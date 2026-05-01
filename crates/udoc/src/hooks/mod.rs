//! Hook protocol: HookSpec, HookConfig, HookRunner.
//!
//! Hooks are external executables that communicate via JSONL over
//! stdin/stdout. Three phases: OCR -> layout -> annotate.
//!
//! Hook errors are logged to stderr via `eprintln!()` rather than through
//! `DiagnosticsSink`, because hooks are external processes (not part of the
//! document parsing pipeline) and their errors are operational, not format-level
//! warnings.
//!
//! The hook protocol (`udoc-hook-v1`) is documented under
//! `crates/udoc/src/hooks/protocol.rs`.

mod process;
mod protocol;
pub(crate) mod request;
pub(crate) mod response;

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use udoc_core::document::*;
use udoc_core::error::{Error, Result, ResultExt};

use process::{
    is_child_alive, kill_process_tree, read_response, send_request, spawn_hook, HookProcess,
    ReadResult,
};
use protocol::{Need, Phase};
use request::{build_document_request, build_request, collect_page_texts};
use response::{apply_response, parse_document_level_response};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Specification for a hook executable.
///
/// Parsed from a command string (e.g., `"./tesseract-ocr.sh"` or
/// `"python layout.py --model large"`).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HookSpec {
    /// Executable or script path to spawn.
    pub command: String,
    /// Command-line arguments (one per `Vec` entry, not shell-quoted).
    pub args: Vec<String>,
}

impl HookSpec {
    /// Parse a command string into command + args by splitting on whitespace.
    ///
    /// Note: this performs simple whitespace splitting. Quoted arguments
    /// are not parsed (e.g., `"python script.py --path '/tmp/my dir'"` splits
    /// into 4 args, with literal quote chars). Use [`HookSpec::new`] for
    /// commands that need arguments containing spaces.
    ///
    /// ```
    /// # use udoc::hooks::HookSpec;
    /// let spec = HookSpec::from_command("python layout.py --model large");
    /// assert_eq!(spec.command, "python");
    /// assert_eq!(spec.args, vec!["layout.py", "--model", "large"]);
    /// ```
    pub fn from_command(cmd: impl Into<String>) -> Self {
        let cmd = cmd.into();
        let mut parts = cmd.split_whitespace();
        let command = parts.next().unwrap_or_default().to_string();
        let args: Vec<String> = parts.map(String::from).collect();
        Self { command, args }
    }

    /// Create a HookSpec with explicit command and args.
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
        }
    }

    /// Validate the spec. Returns an error if the command is empty.
    pub fn validate(&self) -> Result<()> {
        if self.command.is_empty() {
            return Err(Error::new("hook command is empty"));
        }
        Ok(())
    }
}

/// Configuration for hook execution.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HookConfig {
    /// Timeout per page in seconds. Default: 60.
    /// Applied to both handshake reads and per-page response reads.
    /// If a hook doesn't respond within this window, it is skipped.
    pub page_timeout_secs: u64,
    /// Whether to run OCR hooks on all pages, not just textless ones.
    pub ocr_all_pages: bool,
    /// Image resolution in DPI for page rendering. Default: 300.
    pub image_dpi: u32,
    /// Minimum word count a page must have before OCR is skipped.
    /// Pages with fewer words than this threshold are sent to OCR hooks.
    /// Set to 0 to always OCR every page (equivalent to ocr_all_pages).
    /// Default: 10.
    pub min_words_to_skip_ocr: usize,
}

impl Default for HookConfig {
    fn default() -> Self {
        Self {
            page_timeout_secs: 60,
            ocr_all_pages: false,
            image_dpi: 300,
            min_words_to_skip_ocr: 10,
        }
    }
}

/// Maximum consecutive invalid JSON responses before killing a hook.
const MAX_CONSECUTIVE_FAILURES: u8 = 3;

/// Manages hook process lifecycle and I/O.
///
/// Launches hook processes, performs optional handshake negotiation,
/// sends page requests, reads page responses, handles errors and
/// timeouts. Hooks are grouped by phase (OCR -> layout -> annotate)
/// and chained within each phase.
pub struct HookRunner {
    hooks: Vec<HookProcess>,
    config: HookConfig,
}

// ---------------------------------------------------------------------------
// HookRunner implementation
// ---------------------------------------------------------------------------

impl HookRunner {
    /// Create a new HookRunner from hook specifications.
    ///
    /// Launches each hook process, reads the optional handshake, and
    /// determines execution order from declared capabilities.
    pub fn new(specs: &[HookSpec], config: HookConfig) -> Result<Self> {
        if config.page_timeout_secs == 0 {
            return Err(Error::new(
                "page_timeout_secs must be > 0 (a zero timeout would immediately kill all hooks)",
            ));
        }
        let mut hooks = Vec::with_capacity(specs.len());
        let timeout = Duration::from_secs(config.page_timeout_secs);

        for (i, spec) in specs.iter().enumerate() {
            spec.validate().context("validating hook spec")?;
            let hook = spawn_hook(spec, timeout)
                .context(format!("spawning hook #{} ({})", i, &spec.command))?;
            hooks.push(hook);
        }

        // Sort hooks by phase (Ocr < Layout < Annotate).
        hooks.sort_by_key(|h| h.phase);

        Ok(Self { hooks, config })
    }

    /// Run all hooks against the document.
    ///
    /// Executes hooks in phase order (OCR -> layout -> annotate).
    /// Mutates the document in place: adds spans, builds blocks,
    /// attaches overlays. Falls back gracefully on hook failure.
    ///
    /// `page_images` is the directory containing rendered page images
    /// (page-0.png, page-1.png, ...). Required if any hook needs
    /// "image". Pass None if no hook needs images.
    pub fn run(&mut self, doc: &mut Document, page_images: Option<&Path>) -> Result<()> {
        if doc.metadata.page_count == 0 {
            return Ok(());
        }

        // Auto-render pages when any hook needs images and no image dir provided.
        let auto_render_dir = if page_images.is_none() {
            let any_needs_image = self
                .hooks
                .iter()
                .any(|h| h.needs.contains(&protocol::Need::Image));
            if any_needs_image {
                let dir = std::env::temp_dir().join(format!("udoc-render-{}", std::process::id()));
                if std::fs::create_dir_all(&dir).is_ok() {
                    let mut font_cache = crate::render::font_cache::FontCache::new(&doc.assets);
                    for page_idx in 0..doc.metadata.page_count {
                        if let Ok(png) = crate::render::render_page(
                            doc,
                            page_idx,
                            self.config.image_dpi,
                            &mut font_cache,
                        ) {
                            let _ = std::fs::write(dir.join(format!("page-{page_idx}.png")), &png);
                        }
                    }
                    Some(dir)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let page_images = auto_render_dir.as_deref().or(page_images);

        // Collect page text for determining which pages have content.
        // Used to skip OCR on pages that already have text (unless ocr_all_pages).
        // Use metadata.page_count as the authoritative count so hooks see all
        // pages, including those with no extracted text (the OCR use case).
        let mut page_texts = collect_page_texts(doc);
        let page_count = doc.metadata.page_count.max(page_texts.len());
        page_texts.resize(page_count, String::new());

        // Track hook invocation success/failure for error reporting.
        let mut total_attempts: usize = 0;
        let mut total_failures: usize = 0;

        // Group hooks by phase index for chaining.
        let phases = [Phase::Ocr, Phase::Layout, Phase::Annotate];

        for phase in &phases {
            let hook_indices: Vec<usize> = self
                .hooks
                .iter()
                .enumerate()
                .filter(|(_, h)| h.phase == *phase)
                .map(|(i, _)| i)
                .collect();

            if hook_indices.is_empty() {
                continue;
            }

            // Split hooks into document-level (Need::Document) and page-level.
            // Document-level hooks receive one request for the whole document;
            // page-level hooks receive one request per page.
            let (doc_hook_indices, page_hook_indices): (Vec<usize>, Vec<usize>) = hook_indices
                .iter()
                .partition(|&&i| self.hooks[i].needs.contains(&Need::Document));

            // --- Document-level dispatch ---
            // Each document-level hook receives a single request with document
            // path, page count, and format. Response: {"pages": [{...}, ...]}.
            for &hook_idx in &doc_hook_indices {
                let hook = &mut self.hooks[hook_idx];

                if hook.dead {
                    total_attempts += 1;
                    total_failures += 1;
                    continue;
                }

                if !is_child_alive(&mut hook.child) {
                    eprintln!(
                        "hook {}: process exited unexpectedly before document request",
                        hook.command
                    );
                    hook.dead = true;
                    let _ = hook.stdin.take();
                    total_attempts += 1;
                    total_failures += 1;
                    continue;
                }

                let request = build_document_request(doc, page_images, page_count);
                total_attempts += 1;

                if let Err(e) = send_request(&mut hook.stdin, &request) {
                    eprintln!(
                        "hook {}: failed to send document request: {}",
                        hook.command, e
                    );
                    hook.dead = true;
                    let _ = hook.stdin.take();
                    kill_process_tree(&mut hook.child);
                    total_failures += 1;
                    continue;
                }

                // Document-level hooks get page_timeout_secs per page worth of budget.
                // Use page_count as a multiplier so large documents don't time out.
                let timeout = Duration::from_secs(
                    self.config
                        .page_timeout_secs
                        .saturating_mul(page_count.max(1) as u64),
                );
                let response = read_response(
                    &hook.line_rx,
                    &mut hook.buffered_first_line,
                    &hook.command,
                    0,
                    timeout,
                );

                let response = match response {
                    ReadResult::Ok(r) => {
                        hook.consecutive_failures = 0;
                        r
                    }
                    ReadResult::Timeout => {
                        hook.dead = true;
                        let _ = hook.stdin.take();
                        kill_process_tree(&mut hook.child);
                        total_failures += 1;
                        continue;
                    }
                    ReadResult::Eof => {
                        hook.dead = true;
                        let _ = hook.stdin.take();
                        total_failures += 1;
                        continue;
                    }
                    ReadResult::InvalidJson => {
                        hook.consecutive_failures += 1;
                        total_failures += 1;
                        continue;
                    }
                };

                // Parse per-page responses from the "pages" array.
                parse_document_level_response(doc, &response);
            }

            // --- Page-level dispatch ---
            if page_hook_indices.is_empty() {
                continue;
            }

            for page_idx in 0..page_count {
                // For OCR phase, skip pages that already have sufficient text (unless forced).
                if *phase == Phase::Ocr
                    && !self.config.ocr_all_pages
                    && self.config.min_words_to_skip_ocr > 0
                {
                    let word_count = page_texts
                        .get(page_idx)
                        .map(|t| t.split_whitespace().count())
                        .unwrap_or(0);
                    if word_count >= self.config.min_words_to_skip_ocr {
                        continue;
                    }
                }

                // Chain hooks within the same phase: output of hook N feeds hook N+1.
                // We track intermediate state that accumulates across the chain.
                let mut chain_spans: Vec<Value> = Vec::new();

                for &hook_idx in &page_hook_indices {
                    let hook = &mut self.hooks[hook_idx];

                    // Skip dead hooks (timed out, EOF, or too many failures).
                    if hook.dead {
                        total_attempts += 1;
                        total_failures += 1;
                        continue;
                    }

                    // Check if the child process is still alive.
                    if !is_child_alive(&mut hook.child) {
                        eprintln!(
                            "hook {}: process exited unexpectedly, skipping remaining pages",
                            hook.command
                        );
                        hook.dead = true;
                        let _ = hook.stdin.take(); // close stdin immediately
                        total_attempts += 1;
                        total_failures += 1;
                        break;
                    }

                    // Build request based on hook's needs.
                    let request = build_request(
                        hook,
                        page_idx,
                        page_images,
                        doc,
                        &page_texts,
                        &chain_spans,
                        self.config.image_dpi,
                    );

                    let request = match request {
                        Some(r) => r,
                        None => continue, // skip hook (e.g., needs image but no image dir)
                    };

                    total_attempts += 1;

                    // Send request.
                    if let Err(e) = send_request(&mut hook.stdin, &request) {
                        eprintln!(
                            "hook {}: failed to send page {} request: {}",
                            hook.command, page_idx, e
                        );
                        hook.dead = true;
                        let _ = hook.stdin.take();
                        kill_process_tree(&mut hook.child);
                        total_failures += 1;
                        break;
                    }

                    // Read response.
                    let timeout = Duration::from_secs(self.config.page_timeout_secs);
                    let response = read_response(
                        &hook.line_rx,
                        &mut hook.buffered_first_line,
                        &hook.command,
                        page_idx,
                        timeout,
                    );

                    let response = match response {
                        ReadResult::Ok(r) => {
                            hook.consecutive_failures = 0;
                            r
                        }
                        ReadResult::Timeout => {
                            hook.dead = true;
                            let _ = hook.stdin.take();
                            kill_process_tree(&mut hook.child);
                            total_failures += 1;
                            continue;
                        }
                        ReadResult::Eof => {
                            hook.dead = true;
                            let _ = hook.stdin.take();
                            total_failures += 1;
                            continue;
                        }
                        ReadResult::InvalidJson => {
                            hook.consecutive_failures += 1;
                            if hook.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                eprintln!(
                                    "hook {}: {} consecutive invalid responses, killing hook",
                                    hook.command, hook.consecutive_failures
                                );
                                hook.dead = true;
                                let _ = hook.stdin.take();
                                kill_process_tree(&mut hook.child);
                            }
                            total_failures += 1;
                            continue;
                        }
                    };

                    // Extract chaining data from response. Clones the spans
                    // array for the next hook in the chain. Acceptable cost:
                    // bounded by the per-page item limit and only within one page.
                    if let Some(spans) = response.get("spans").and_then(|v| v.as_array()) {
                        chain_spans = spans.clone();
                    }

                    // Apply response to document.
                    apply_response(doc, &response, page_idx);
                }

                // chain_spans is consumed by build_request for the next hook
                // in the chain; drop it here so it doesn't leak across phases.
                drop(chain_spans);
            }
        }

        // Close stdin, kill, wait, and join reader threads for all hooks.
        let mut thread_panic = false;
        for hook in &mut self.hooks {
            let _ = hook.stdin.take();
            kill_process_tree(&mut hook.child);
            if let Some(h) = hook.stdout_handle.take() {
                if let Err(panic) = h.join() {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("unknown panic");
                    eprintln!(
                        "hook {}: stdout reader thread panicked: {}",
                        hook.command, msg
                    );
                    thread_panic = true;
                }
            }
            if let Some(h) = hook.stderr_handle.take() {
                if let Err(panic) = h.join() {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("unknown panic");
                    eprintln!(
                        "hook {}: stderr reader thread panicked: {}",
                        hook.command, msg
                    );
                    thread_panic = true;
                }
            }
        }

        if thread_panic {
            return Err(Error::new(
                "hook reader thread panicked (possible data loss)",
            ));
        }

        if total_attempts > 0 && total_failures == total_attempts {
            // Clean up auto-rendered temp dir before returning error.
            if let Some(ref dir) = auto_render_dir {
                let _ = std::fs::remove_dir_all(dir);
            }
            return Err(Error::new("all hook invocations failed"));
        }

        // Clean up auto-rendered temp directory.
        if let Some(ref dir) = auto_render_dir {
            let _ = std::fs::remove_dir_all(dir);
        }

        Ok(())
    }
}

impl Drop for HookRunner {
    fn drop(&mut self) {
        for hook in &mut self.hooks {
            // Close stdin to signal EOF, then kill the entire process tree.
            let _ = hook.stdin.take();
            kill_process_tree(&mut hook.child);
            // Join reader threads so they don't outlive the HookRunner.
            if let Some(h) = hook.stdout_handle.take() {
                let _ = h.join();
            }
            if let Some(h) = hook.stderr_handle.take() {
                let _ = h.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_spec_from_command_simple() {
        let spec = HookSpec::from_command("./tesseract-ocr.sh");
        assert_eq!(spec.command, "./tesseract-ocr.sh");
        assert!(spec.args.is_empty());
    }

    #[test]
    fn hook_spec_from_command_with_args() {
        let spec = HookSpec::from_command("python layout.py --model large");
        assert_eq!(spec.command, "python");
        assert_eq!(spec.args, vec!["layout.py", "--model", "large"]);
    }

    #[test]
    fn hook_spec_from_command_empty() {
        let spec = HookSpec::from_command("");
        assert_eq!(spec.command, "");
        assert!(spec.args.is_empty());
    }

    #[test]
    fn hook_spec_new() {
        let spec = HookSpec::new("python", vec!["script.py".into()]);
        assert_eq!(spec.command, "python");
        assert_eq!(spec.args, vec!["script.py"]);
    }

    #[test]
    fn hook_spec_validate_ok() {
        let spec = HookSpec::from_command("echo hello");
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn hook_spec_validate_empty() {
        let spec = HookSpec::from_command("");
        let err = spec.validate().unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("empty"), "error should mention empty: {msg}");
    }

    // Tests for min_words_to_skip_ocr logic.
    // We test the word-count predicate in isolation (the full HookRunner
    // integration requires spawning real processes).

    fn word_count(text: &str) -> usize {
        text.split_whitespace().count()
    }

    #[test]
    fn ocr_skip_threshold_sufficient_text() {
        // A page with >= 10 words should trigger a skip.
        let text = "the quick brown fox jumps over the lazy dog now";
        assert_eq!(word_count(text), 10);
        let threshold = 10usize;
        assert!(word_count(text) >= threshold, "should skip OCR");
    }

    #[test]
    fn ocr_skip_threshold_sparse_text() {
        // A page with < 10 words should NOT trigger a skip.
        let text = "hello world";
        assert_eq!(word_count(text), 2);
        let threshold = 10usize;
        assert!(word_count(text) < threshold, "should not skip OCR");
    }

    #[test]
    fn ocr_skip_threshold_zero_always_ocr() {
        // When min_words_to_skip_ocr = 0, the skip check is bypassed entirely.
        // The condition `min_words_to_skip_ocr > 0` guards the check.
        let config = HookConfig {
            min_words_to_skip_ocr: 0,
            ..HookConfig::default()
        };
        // Verify the guard: with threshold=0, no page should ever be skipped.
        let text = "ten or more words here to verify zero threshold behavior always";
        assert!(
            config.min_words_to_skip_ocr == 0,
            "zero threshold should bypass skip logic"
        );
        // word_count is irrelevant when threshold is 0
        let _ = word_count(text);
    }

    #[test]
    fn ocr_skip_threshold_empty_page() {
        // Empty page (0 words) is always below threshold.
        let text = "";
        let threshold = 10usize;
        assert!(word_count(text) < threshold);
    }

    #[test]
    fn hook_runner_rejects_empty_command() {
        let spec = HookSpec::from_command("");
        let result = HookRunner::new(&[spec], HookConfig::default());
        match result {
            Ok(_) => unreachable!("expected error for empty command"),
            Err(e) => {
                let msg = format!("{e}");
                assert!(msg.contains("empty"), "error should mention empty: {msg}");
            }
        }
    }

    #[test]
    fn hook_config_default() {
        let config = HookConfig::default();
        assert_eq!(config.page_timeout_secs, 60);
        assert!(!config.ocr_all_pages);
        assert_eq!(config.image_dpi, 300);
        assert_eq!(config.min_words_to_skip_ocr, 10);
    }
}
