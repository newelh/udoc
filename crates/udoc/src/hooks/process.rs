//! Hook process lifecycle: spawning, I/O, cleanup.
//!
//! Manages child process creation, stdin/stdout plumbing, stderr draining,
//! line reading with size limits, and process tree cleanup.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use udoc_core::error::{Error, Result};

use super::protocol::{
    parse_handshake, phase_from_capabilities, Capability, HandshakeOutcome, Need, Phase, Provide,
    HOOK_PROTOCOL_ID,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum line size (1 MB) accepted from a hook's stdout.
/// Lines exceeding this are truncated to prevent OOM from misbehaving hooks.
/// Legitimate hook responses are well under this (full-page OCR spans ~50KB).
pub(crate) const MAX_HOOK_LINE_SIZE: usize = 1024 * 1024;

/// Maximum total bytes forwarded from a hook's stderr (1 MB).
/// After this limit, remaining stderr is silently drained. Prevents a
/// malicious hook from flooding the host's stderr indefinitely.
const MAX_STDERR_BYTES: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// HookProcess type
// ---------------------------------------------------------------------------

#[allow(dead_code)] // capabilities and provides fields stored for future v2 hook filtering
pub(crate) struct HookProcess {
    pub(crate) child: Child,
    /// Wrapped in Option so we can close stdin by calling take().
    pub(crate) stdin: Option<BufWriter<ChildStdin>>,
    /// Lines from stdout, read by a background thread. Supports timeout
    /// via `recv_timeout` so a hanging hook won't block forever.
    pub(crate) line_rx: mpsc::Receiver<std::io::Result<String>>,
    pub(crate) command: String,
    pub(crate) capabilities: Vec<Capability>,
    pub(crate) needs: Vec<Need>,
    pub(crate) provides: Vec<Provide>,
    pub(crate) phase: Phase,
    /// If no handshake was detected, the first stdout line may be page 0's
    /// response. Buffer it here so we don't lose it.
    pub(crate) buffered_first_line: Option<String>,
    /// Hook is dead and should not receive further requests. Set on
    /// timeout, EOF, or after MAX_CONSECUTIVE_FAILURES invalid JSON
    /// responses to avoid protocol desynchronization.
    pub(crate) dead: bool,
    /// Consecutive invalid JSON response count. Reset on success.
    pub(crate) consecutive_failures: u8,
    /// Background stdout reader thread. Joined on cleanup.
    pub(crate) stdout_handle: Option<thread::JoinHandle<()>>,
    /// Background stderr reader thread. Joined on cleanup.
    pub(crate) stderr_handle: Option<thread::JoinHandle<()>>,
}

// ---------------------------------------------------------------------------
// LineError
// ---------------------------------------------------------------------------

/// Classified error from recv_line.
pub(crate) enum LineError {
    Timeout(String),
    Disconnected(String),
    ReadError(String),
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

/// Read one line from a BufRead, capping in-memory accumulation at `limit`
/// bytes. Bytes beyond the limit are drained (read and discarded) until the
/// next newline or EOF, preventing OOM from a malicious/misbehaving hook that
/// sends a multi-GB line without newlines.
///
/// Returns `Ok(0)` on EOF (like `BufRead::read_line`), `Ok(bytes_consumed)`
/// on success. The actual content in `buf` is at most `limit` bytes.
pub(crate) fn read_line_bounded(
    reader: &mut impl BufRead,
    buf: &mut String,
    limit: usize,
) -> std::io::Result<usize> {
    let mut total = 0;
    let mut truncated = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total); // EOF
        }

        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let chunk = pos + 1; // include the newline
            if !truncated {
                let usable = chunk.min(limit.saturating_sub(buf.len()));
                // from_utf8_lossy handles any non-UTF8 from hooks gracefully
                buf.push_str(&String::from_utf8_lossy(&available[..usable]));
            }
            total += chunk;
            reader.consume(chunk);
            return Ok(total);
        }

        let len = available.len();
        if !truncated && buf.len() + len <= limit {
            buf.push_str(&String::from_utf8_lossy(available));
        } else if !truncated {
            // Partially fill up to the limit, then start discarding
            let usable = limit.saturating_sub(buf.len());
            if usable > 0 {
                buf.push_str(&String::from_utf8_lossy(&available[..usable]));
            }
            truncated = true;
        }
        total += len;
        reader.consume(len);
    }
}

/// Spawn a background thread that reads lines from a child's stdout and
/// sends them over a channel. This decouples I/O from the main thread
/// so we can apply timeouts via `recv_timeout`.
pub(crate) fn spawn_line_reader(
    stdout: ChildStdout,
) -> (
    mpsc::Receiver<std::io::Result<String>>,
    thread::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::sync_channel(16);
    let handle = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = String::new();
        loop {
            buf.clear();
            match read_line_bounded(&mut reader, &mut buf, MAX_HOOK_LINE_SIZE) {
                Ok(0) => break, // EOF
                Ok(bytes_consumed) => {
                    if bytes_consumed > MAX_HOOK_LINE_SIZE {
                        eprintln!(
                            "hook output line too large ({} bytes, max {}), truncated",
                            bytes_consumed, MAX_HOOK_LINE_SIZE
                        );
                    }
                    let line = buf
                        .trim_end_matches('\n')
                        .trim_end_matches('\r')
                        .to_string();
                    if tx.send(Ok(line)).is_err() {
                        break; // receiver dropped
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });
    (rx, handle)
}

/// Read one line from the channel with a timeout.
/// Returns Ok(line) on success, Err(LineError) with classified failure.
pub(crate) fn recv_line(
    rx: &mpsc::Receiver<std::io::Result<String>>,
    timeout: Duration,
    command: &str,
    context: &str,
) -> std::result::Result<String, LineError> {
    match rx.recv_timeout(timeout) {
        Ok(Ok(line)) => Ok(line),
        Ok(Err(e)) => Err(LineError::ReadError(format!(
            "hook {}: read error {}: {}",
            command, context, e
        ))),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(LineError::Timeout(format!(
            "hook {}: timed out {}",
            command, context
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(LineError::Disconnected(format!(
            "hook {}: stdout closed {}",
            command, context
        ))),
    }
}

/// Kill a child process and all its descendants, then reap.
/// On Unix, kills the entire process group to prevent orphaned
/// children from shell scripts.
#[allow(unsafe_code)]
pub(crate) fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        // SAFETY: libc::kill sends a signal to a process group. The negative
        // PID targets the group created by process_group(0) in spawn_hook.
        // SIGKILL cannot be caught, so this cleanly terminates the tree.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

/// Spawn a hook process, read optional handshake, determine capabilities.
pub(crate) fn spawn_hook(spec: &super::HookSpec, timeout: Duration) -> Result<HookProcess> {
    let mut cmd = Command::new(&spec.command);
    cmd.args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // On Unix, create a new process group so we can kill the entire tree.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::with_source(format!("spawning hook '{}'", spec.command), e))?;

    let child_stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(format!(
                "hook '{}': failed to open stdin",
                spec.command
            )));
        }
    };
    let child_stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(format!(
                "hook '{}': failed to open stdout",
                spec.command
            )));
        }
    };

    // Spawn a thread to drain stderr and forward as warnings.
    // Total forwarded bytes are capped at MAX_STDERR_BYTES to prevent
    // a malicious hook from flooding the host's stderr.
    let stderr_handle = if let Some(stderr) = child.stderr.take() {
        let cmd = spec.command.clone();
        Some(thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buf = String::new();
            let mut total_bytes: usize = 0;
            let mut capped = false;
            let mut suppressed_bytes: usize = 0;
            loop {
                buf.clear();
                match read_line_bounded(&mut reader, &mut buf, MAX_HOOK_LINE_SIZE) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        total_bytes = total_bytes.saturating_add(n);
                        if capped {
                            suppressed_bytes = suppressed_bytes.saturating_add(n);
                            continue; // drain but don't forward
                        }
                        if total_bytes > MAX_STDERR_BYTES {
                            eprintln!(
                                "hook {} (stderr): output exceeded {} bytes, suppressing further output",
                                cmd, MAX_STDERR_BYTES
                            );
                            capped = true;
                            suppressed_bytes = suppressed_bytes.saturating_add(n);
                            continue;
                        }
                        let line = buf.trim_end_matches('\n').trim_end_matches('\r');
                        if !line.is_empty() {
                            eprintln!("hook {} (stderr): {}", cmd, line);
                        }
                    }
                    Err(_) => break,
                }
            }
            if suppressed_bytes > 0 {
                eprintln!(
                    "hook {} (stderr): suppressed {} additional bytes",
                    cmd, suppressed_bytes
                );
            }
        }))
    } else {
        None
    };

    let (line_rx, stdout_handle) = spawn_line_reader(child_stdout);
    let stdin = Some(BufWriter::new(child_stdin));

    // Try to read the first line for handshake detection (with timeout).
    let (capabilities, needs, provides, phase, buffered_first_line, start_dead) = match recv_line(
        &line_rx,
        timeout,
        &spec.command,
        "reading handshake",
    ) {
        Err(err) => {
            let is_timeout = matches!(&err, LineError::Timeout(_));
            let msg = match &err {
                LineError::Timeout(m) | LineError::Disconnected(m) | LineError::ReadError(m) => m,
            };
            if is_timeout {
                // Hook is hung during handshake. Kill immediately to avoid
                // leaving a process that will never respond.
                eprintln!("{}, killing unresponsive hook process", msg);
                kill_process_tree(&mut child);
            } else {
                eprintln!(
                    "{}, treating as OCR hook (needs=[image], provides=[spans])",
                    msg
                );
            }
            (
                vec![Capability::Ocr],
                vec![Need::Image],
                vec![Provide::Spans],
                Phase::Ocr,
                None,
                is_timeout, // mark dead if timed out
            )
        }
        Ok(first_line) => {
            let trimmed = first_line.trim();
            match parse_handshake(trimmed) {
                HandshakeOutcome::Valid {
                    capabilities,
                    needs,
                    provides,
                } => {
                    let ph = phase_from_capabilities(&capabilities);
                    (capabilities, needs, provides, ph, None, false)
                }
                HandshakeOutcome::WrongProtocol { observed } => {
                    // A handshake with a wrong protocol id is a clear
                    // configuration error; surface both expected and
                    // observed and mark the hook dead so a stale gist
                    // does not silently degrade to a no-op.
                    eprintln!(
                        "hook {}: protocol mismatch: expected '{}', got '{}'. \
                         Update your hook script to emit \"protocol\": \"{}\". \
                         Marking hook dead.",
                        spec.command, HOOK_PROTOCOL_ID, observed, HOOK_PROTOCOL_ID
                    );
                    (
                        vec![Capability::Ocr],
                        vec![Need::Image],
                        vec![Provide::Spans],
                        Phase::Ocr,
                        None, // don't buffer the rejected handshake
                        true, // mark dead
                    )
                }
                HandshakeOutcome::NotHandshake => {
                    // Not a handshake. Check if it's valid JSON at all.
                    if serde_json::from_str::<Value>(trimmed).is_ok() {
                        eprintln!(
                            "hook {}: no handshake, treating as OCR hook (needs=[image], provides=[spans])",
                            spec.command
                        );
                        (
                            vec![Capability::Ocr],
                            vec![Need::Image],
                            vec![Provide::Spans],
                            Phase::Ocr,
                            Some(first_line.trim().to_string()),
                            false,
                        )
                    } else {
                        // Not valid JSON. Hook is producing garbage -- mark dead.
                        eprintln!(
                            "hook {}: first output line is not valid JSON, marking hook as dead",
                            spec.command
                        );
                        (
                            vec![Capability::Ocr],
                            vec![Need::Image],
                            vec![Provide::Spans],
                            Phase::Ocr,
                            None, // don't buffer garbage
                            true,
                        )
                    }
                }
            }
        }
    };

    Ok(HookProcess {
        child,
        stdin,
        line_rx,
        command: spec.command.clone(),
        capabilities,
        needs,
        provides,
        phase,
        buffered_first_line,
        dead: start_dead,
        consecutive_failures: 0,
        stdout_handle: Some(stdout_handle),
        stderr_handle,
    })
}

/// Check if a child process is still alive (non-blocking).
pub(crate) fn is_child_alive(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(None) => true,     // Still running
        Ok(Some(_)) => false, // Exited
        Err(_) => false,      // Error checking, assume dead
    }
}

// ---------------------------------------------------------------------------
// ReadResult and response reading
// ---------------------------------------------------------------------------

/// Classified result from reading a hook response line.
pub(crate) enum ReadResult {
    /// Successfully parsed a JSON response.
    Ok(Value),
    /// Timed out waiting for a response. Hook is likely stuck.
    Timeout,
    /// EOF or disconnected channel. Hook process has exited.
    Eof,
    /// The hook returned a line that is not valid JSON. The hook may
    /// still be functional for subsequent pages.
    InvalidJson,
}

/// Read a JSON response line from a hook's stdout channel.
/// Uses the buffered first line if available (no-handshake case).
/// Returns a classified ReadResult distinguishing timeout, EOF, invalid
/// JSON, and success, so the caller can apply appropriate recovery.
pub(crate) fn read_response(
    line_rx: &mpsc::Receiver<std::io::Result<String>>,
    buffered_first_line: &mut Option<String>,
    command: &str,
    page_idx: usize,
    timeout: Duration,
) -> ReadResult {
    let line = if let Some(buffered) = buffered_first_line.take() {
        buffered
    } else {
        let context = format!("reading response for page {}", page_idx);
        match recv_line(line_rx, timeout, command, &context) {
            Ok(l) => l.trim().to_string(),
            Err(LineError::Timeout(msg)) => {
                eprintln!("{}", msg);
                return ReadResult::Timeout;
            }
            Err(LineError::Disconnected(msg) | LineError::ReadError(msg)) => {
                eprintln!("{}", msg);
                return ReadResult::Eof;
            }
        }
    };

    match serde_json::from_str::<Value>(&line) {
        Ok(v) => ReadResult::Ok(v),
        Err(e) => {
            eprintln!(
                "hook {}: invalid JSON for page {}: {}",
                command, page_idx, e
            );
            ReadResult::InvalidJson
        }
    }
}

/// Send a JSON request line to a hook's stdin.
pub(crate) fn send_request(
    stdin: &mut Option<BufWriter<ChildStdin>>,
    request: &Value,
) -> std::io::Result<()> {
    let writer = stdin
        .as_mut()
        .ok_or_else(|| std::io::Error::other("hook stdin already closed"))?;
    let line = serde_json::to_string(request)?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_line_bounded_normal_line() {
        let data = b"hello world\n";
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut buf = String::new();
        let n = read_line_bounded(&mut reader, &mut buf, 1024).unwrap();
        assert_eq!(n, 12);
        assert_eq!(buf, "hello world\n");
    }

    #[test]
    fn read_line_bounded_truncates_long_line() {
        // Line exceeds limit: content should be capped, rest drained
        let data = b"abcdefghij\n"; // 11 bytes total
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut buf = String::new();
        let n = read_line_bounded(&mut reader, &mut buf, 5).unwrap();
        assert_eq!(n, 11); // all bytes consumed (drained to newline)
        assert_eq!(buf.len(), 5); // but only 5 bytes in buffer
        assert_eq!(buf, "abcde");
    }

    #[test]
    fn read_line_bounded_eof_without_newline() {
        let data = b"no newline here";
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut buf = String::new();
        let n = read_line_bounded(&mut reader, &mut buf, 1024).unwrap();
        assert_eq!(n, 15);
        assert_eq!(buf, "no newline here");
    }

    #[test]
    fn read_line_bounded_eof_returns_zero() {
        let data = b"";
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut buf = String::new();
        let n = read_line_bounded(&mut reader, &mut buf, 1024).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn read_line_bounded_multiple_lines() {
        let data = b"line1\nline2\n";
        let mut reader = std::io::BufReader::new(&data[..]);

        let mut buf = String::new();
        let n = read_line_bounded(&mut reader, &mut buf, 1024).unwrap();
        assert_eq!(n, 6);
        assert!(buf.starts_with("line1"));

        buf.clear();
        let n = read_line_bounded(&mut reader, &mut buf, 1024).unwrap();
        assert_eq!(n, 6);
        assert!(buf.starts_with("line2"));
    }
}
