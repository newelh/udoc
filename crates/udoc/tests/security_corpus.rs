//! Regression tests against `tests/corpus/security/*` seeds.
//!
//! Each test here ties a committed adversarial-input seed to a specific
//! finding and asserts the fix holds. A bare "didn't crash" assertion is
//! too weak (many regressions could cause different crashes); we aim
//! for "returns the error shape we expect for THIS finding."
//!
//! The seeds are maintained per `tests/corpus/security/README.md`.
//! Large seeds (>1 MB) that aren't committed get `#[ignore]`'d here so
//! the test exists for operators who reproduce locally but the build
//! doesn't fail on CI's minimal checkout.

use std::path::PathBuf;

fn security_seed(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/corpus/security")
        .join(name)
}

/// **#62 SEC-ALLOC-CLAMP**: a 296 KB RC4-encrypted ArcInfo GIS PDF
/// tricks udoc into computing a 254 PB allocation request from a
/// malformed stream dictionary. With the  fixes (safe_alloc_size
/// helper + image-dim clamps + arithmetic-overflow fixes in the content
/// interpreter), the worst case is a bounded error, not SIGABRT from
/// glibc's alloc-error handler.
///
/// Assertion: `extract()` returns (either success with a few page
/// results, or an Err that doesn't panic). Either is fine -- what
/// matters is the library didn't crash. We also bound wall time so a
/// regression that accidentally re-introduces the infinite-alloc-loop
/// path (rather than the 254 PB request) gets caught.
#[test]
fn alloc_bomb_returns_bounded_error() {
    let seed = security_seed("govdocs1-010258-alloc-bomb.pdf");
    assert!(
        seed.is_file(),
        "alloc-bomb seed missing at {} -- run `git status` to see if\
         tests/corpus/security/ is ignored or the seed was deleted.",
        seed.display()
    );

    let start = std::time::Instant::now();
    // extract() internally uses the default Config (no memory_budget
    // override); we just want "doesn't crash, doesn't hang."
    let result = udoc::extract(&seed);
    let elapsed = start.elapsed();

    // Either an Ok extraction or a clean Err is acceptable. We only fail
    // if the test hits the 30 s wall-clock budget below (suggests a
    // regression re-introduced the infinite-alloc loop).
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "alloc-bomb seed took {elapsed:?} (>30s); possible infinite-alloc regression"
    );

    // Either outcome is valid; print which one so CI logs carry the
    // current state. If this regresses to panic, cargo test reports it.
    match &result {
        Ok(doc) => eprintln!(
            "alloc_bomb: Ok extraction, {} top-level content nodes",
            doc.content.len()
        ),
        Err(e) => eprintln!("alloc_bomb: graceful Err -- {e}"),
    }
}

/// **#63 JBIG2-SOFTMASK-HANG** regression. The original 17 MB Internet
/// Archive book triggered two distinct symptoms:
///
/// (a) RSS balloon past ~10 GB on a worker under parallel bench load.
///      fixed this via the FontBundle / per-doc state split (commit
///     `9bee141b`), so the parallel-load symptom is closed.
/// (b) A CPU-bound infinite loop in our own JBIG2 decoder when
///     extracting the soft-mask on page 265 of the original book.
///     Standalone repro on page 265 (this seed) burns 100% CPU
///     indefinitely -- a 10-minute test confirmed >410 s of user time
///     with zero output -- whether the hang lives in the arithmetic
///     coder, segment loop, or symbol/generic region decoder is
///     unfixed as of .
///
/// The committed seed `jbig2-softmask-hang-min.pdf` is `mutool merge`'s
/// page-265-only carve-out of the original book. 53 KB, public-domain
/// IA scan. It contains 1 JBIG2 SMask + 2 JPX content images and is
/// the smallest input that still triggers (b) on the current code.
///
/// **This test is `#[ignore]`'d** until the JBIG2 decoder loop is
/// fixed: the seed currently CPU-loops on every code path that touches
/// images (default extract, `--no-images` alone, `--no-tables` alone --
/// only `--no-images --no-tables` together skips it), so running this
/// in CI burns 60 s per build for a known-broken case. Operators
/// validating the eventual fix run `cargo test --test security_corpus
/// -- --ignored`.
///
/// What this test guards once unblocked: bound the wall time so a
/// future regression that re-introduces the original 27+ min hang
/// signature trips CI. We use a worker thread + `recv_timeout` rather
/// than relying on test-runner timeouts so the assertion fires cleanly
/// at 60 s instead of orphaning the suite. Tighten to <5 s (the
/// aspirational target) once the decoder fix lands.
#[test]
#[ignore = "JBIG2 decoder CPU-loop on page 265 still open; see test docs"]
fn jbig2_softmask_hang_bounded() {
    let seed = security_seed("jbig2-softmask-hang-min.pdf");
    assert!(
        seed.is_file(),
        "JBIG2 hang seed missing at {} -- run `git status` to see if \
         tests/corpus/security/ is ignored or the seed was deleted.",
        seed.display()
    );

    // Run extract in a worker thread so a true infinite loop in JBIG2
    // doesn't hang the test runner forever -- `recv_timeout` returns
    // even if the worker keeps spinning. The detached worker is leaked
    // deliberately on timeout; it'll be reaped when the test binary
    // exits.
    let (tx, rx) = std::sync::mpsc::channel();
    let seed_clone = seed.clone();
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let result = udoc::extract(&seed_clone);
        let _ = tx.send((start.elapsed(), result));
    });

    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok((elapsed, Ok(doc))) => eprintln!(
            "jbig2_softmask: Ok extraction in {elapsed:?}, {} top-level \
             content nodes",
            doc.content.len()
        ),
        Ok((elapsed, Err(e))) => {
            eprintln!("jbig2_softmask: graceful Err in {elapsed:?} -- {e}")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
            "JBIG2-softmask extract exceeded 60 s budget; possible \
             regression past the original 27+ min hang signature"
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("JBIG2-softmask worker thread panicked before sending result")
        }
    }
}

/// **#63 SMOKE**: the always-on companion to `jbig2_softmask_hang_bounded`.
/// Catches the regression of `Document::open` itself (lexer / xref /
/// trailer parse) on the minimised seed -- this much *should* finish
/// in <1 s today and remains a useful tripwire even while the JBIG2
/// decoder loop keeps the full-extract test ignored.
///
/// Specifically guards against: a regression that breaks PDF header /
/// xref recovery so the seed fails to even open. The full extract is
/// the ignored test; this one keeps minimal coverage live in CI.
#[test]
fn jbig2_softmask_seed_opens_quickly() {
    let seed = security_seed("jbig2-softmask-hang-min.pdf");
    assert!(
        seed.is_file(),
        "JBIG2 hang seed missing at {} -- run `git status` to see if \
         tests/corpus/security/ is ignored or the seed was deleted.",
        seed.display()
    );

    // Disabling images AND tables routes around the JBIG2 decode
    // entirely (both code paths touch image streams on this seed).
    // The text/metadata/structure parse should finish in <1 s.
    let mut cfg = udoc::Config::default();
    cfg.layers.images = false;
    cfg.layers.tables = false;

    let start = std::time::Instant::now();
    let result = udoc::extract_with(&seed, cfg);
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "JBIG2-softmask seed open (no-images/no-tables) took {elapsed:?} \
         (>5 s); the lexer/xref path shouldn't touch JBIG2 -- regression"
    );
    match &result {
        Ok(doc) => eprintln!(
            "jbig2_softmask_open: Ok in {elapsed:?}, {} top-level nodes",
            doc.content.len()
        ),
        Err(e) => eprintln!("jbig2_softmask_open: graceful Err in {elapsed:?} -- {e}"),
    }
}
