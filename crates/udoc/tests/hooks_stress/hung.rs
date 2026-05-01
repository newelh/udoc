//! Hung-process stress tests.
//!
//! A hook that traps SIGTERM and refuses to exit must still be killed by
//! the host process. Verifies SIGKILL escalation, no zombies, no FD leaks,
//! and grandchild reaping.

use std::time::{Duration, Instant};

use udoc::hooks::{HookConfig, HookRunner, HookSpec};

use super::helpers::{small_pdf, write_script, NOOP_HANDSHAKE};

// ---------------------------------------------------------------------------
// 1. SIGTERM-trap hook is killed within the timeout window
// ---------------------------------------------------------------------------
#[test]
fn sigterm_trap_killed_by_sigkill() {
    let body = format!(
        "trap '' TERM\necho '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 60\ndone"
    );
    let script = write_script("sigterm-trap", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");

    let start = Instant::now();
    let _ = runner.run(&mut doc, None);
    let elapsed = start.elapsed();

    // SIGKILL cannot be trapped. Even with the SIGTERM trap, the runner
    // sends SIGKILL via `libc::kill(-pid, SIGKILL)` to the process group.
    // Total time should be ~page_timeout, not 60 s.
    assert!(
        elapsed.as_secs() < 10,
        "SIGTERM-trap hook should be killed via SIGKILL escalation, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. No zombie children left behind from OUR specific hook
//
// Cargo runs tests within a binary in parallel by default. To distinguish
// our hook's processes from sibling tests' hook processes, we look up each
// candidate child's /proc/<pid>/cmdline and match on the unique script path
// we wrote (which contains a unique label).
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
#[test]
fn no_zombie_after_hung_hook() {
    fn process_state(pid: &str) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close = stat.rfind(')')?;
        let after = &stat[close + 1..];
        after
            .split_whitespace()
            .next()
            .and_then(|s| s.chars().next())
    }

    /// Find any child or descendant of our_pid whose cmdline contains
    /// `marker`.
    fn find_descendants_by_cmdline(marker: &str) -> Vec<String> {
        let our_pid = std::process::id().to_string();
        let mut hits = Vec::new();
        let entries = match std::fs::read_dir("/proc") {
            Ok(e) => e,
            Err(_) => return hits,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let pid = match name.to_str() {
                Some(p) if p.chars().all(|c| c.is_ascii_digit()) => p.to_string(),
                _ => continue,
            };
            // cmdline check
            let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(b) => String::from_utf8_lossy(&b).to_string(),
                Err(_) => continue,
            };
            if !cmdline.contains(marker) {
                continue;
            }
            // ppid check: walk up via stat field 4 to ensure ancestor is us.
            let mut cur = pid.clone();
            let mut is_descendant = false;
            for _ in 0..16 {
                let stat = match std::fs::read_to_string(format!("/proc/{cur}/stat")) {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let close = match stat.rfind(')') {
                    Some(c) => c,
                    None => break,
                };
                let after = &stat[close + 1..];
                let mut fields = after.split_whitespace();
                let _state = fields.next();
                let ppid = match fields.next() {
                    Some(p) => p.to_string(),
                    None => break,
                };
                if ppid == our_pid {
                    is_descendant = true;
                    break;
                }
                if ppid == "1" || ppid == "0" {
                    break;
                }
                cur = ppid;
            }
            if is_descendant {
                hits.push(pid);
            }
        }
        hits
    }

    let label = format!("nozombie-{}-{}", std::process::id(), unique_id());
    let body = format!(
        "trap '' TERM\necho '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 60\ndone"
    );
    let script = write_script(&label, &body);
    let marker = script.path.to_string_lossy().to_string();
    let spec = HookSpec::from_command(&marker);

    let mut config = HookConfig::default();
    config.page_timeout_secs = 1;

    {
        let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
        let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
        let _ = runner.run(&mut doc, None);
        // runner dropped here; Drop impl kills + reaps every child
    }

    // Give the kernel a moment to reap.
    std::thread::sleep(Duration::from_millis(300));

    let our_descendants = find_descendants_by_cmdline(&marker);
    let zombies: Vec<String> = our_descendants
        .iter()
        .filter(|pid| matches!(process_state(pid), Some('Z')))
        .cloned()
        .collect();

    assert!(
        zombies.is_empty(),
        "found {} zombies traceable to our hook ({}): {:?}",
        zombies.len(),
        marker,
        zombies
    );
}

// ---------------------------------------------------------------------------
// 3. No descendant processes left running (grandchild reaping)
//
// Same identification approach: find descendants whose cmdline contains the
// unique script path.
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
#[test]
fn no_descendant_processes_after_hook_with_grandchild() {
    fn descendants_with_marker(marker: &str) -> Vec<String> {
        let our_pid = std::process::id().to_string();
        let mut hits = Vec::new();
        let entries = match std::fs::read_dir("/proc") {
            Ok(e) => e,
            Err(_) => return hits,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let pid = match name.to_str() {
                Some(p) if p.chars().all(|c| c.is_ascii_digit()) => p.to_string(),
                _ => continue,
            };
            let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(b) => String::from_utf8_lossy(&b).to_string(),
                Err(_) => continue,
            };
            if !cmdline.contains(marker) {
                continue;
            }
            // Also accept grandchildren: walk up ppid chain looking for our pid.
            let mut cur = pid.clone();
            let mut is_descendant = false;
            for _ in 0..16 {
                let stat = match std::fs::read_to_string(format!("/proc/{cur}/stat")) {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let close = match stat.rfind(')') {
                    Some(c) => c,
                    None => break,
                };
                let after = &stat[close + 1..];
                let mut fields = after.split_whitespace();
                let _state = fields.next();
                let ppid = match fields.next() {
                    Some(p) => p.to_string(),
                    None => break,
                };
                if ppid == our_pid {
                    is_descendant = true;
                    break;
                }
                if ppid == "1" || ppid == "0" {
                    break;
                }
                cur = ppid;
            }
            if is_descendant {
                hits.push(pid);
            }
        }
        hits
    }

    let label = format!("grandchild-{}-{}", std::process::id(), unique_id());
    // The grandchild inherits the parent's cmdline view but executes `sleep`
    // -- so the cmdline marker is the LABEL embedded in the script path,
    // which we put into an exported env var so children include it via
    // `argv[0]`. Simpler: use bash -c 'sleep 120 # MARKER' so the cmdline
    // contains the marker literally.
    let body = format!(
        "trap '' TERM\necho '{NOOP_HANDSHAKE}'\nbash -c 'exec -a \"sleep-{label}\" sleep 120' &\nwhile IFS= read -r line; do\n    sleep 60\ndone"
    );
    let script = write_script(&label, &body);
    // Match on the script path (parent) OR on the renamed grandchild argv[0].
    let marker = label.clone();
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 1;

    {
        let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
        let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");
        let _ = runner.run(&mut doc, None);
    }

    std::thread::sleep(Duration::from_millis(500));

    let alive = descendants_with_marker(&marker);
    assert!(
        alive.is_empty(),
        "found descendants from our hook ({marker}) still alive after cleanup: {alive:?}"
    );
}

#[cfg(target_os = "linux")]
fn unique_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    C.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// 4. Drop without explicit cleanup also kills the hook
// ---------------------------------------------------------------------------
#[test]
fn runner_drop_kills_hung_hook() {
    let body = format!(
        "trap '' TERM\necho '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 120\ndone"
    );
    let script = write_script("drop-kills", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 60; // intentionally large -- we'll never wait

    let start = Instant::now();
    {
        let runner = HookRunner::new(&[spec], config).expect("spawn hook");
        // Drop runner immediately without calling run().
        drop(runner);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 5,
        "Drop should kill the hook process without waiting, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. FD count stable after repeated invocations
//
// Operational target: 200 invocations with no FD leak. Test runs 20 to keep
// CI time low; the loop is identical so 200 is documented in the report
// and can be enabled by setting `UDOC_HOOKS_FD_ITERS`.
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
#[test]
fn fd_count_stable_across_invocations() {
    let our_pid = std::process::id();
    let fd_dir = format!("/proc/{our_pid}/fd");

    fn count_fds(dir: &str) -> usize {
        std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
    }

    let body = format!(
        "trap '' TERM\necho '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 60\ndone"
    );
    let script = write_script("fd-stable", &body);

    let iters: usize = std::env::var("UDOC_HOOKS_FD_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let mut config = HookConfig::default();
    config.page_timeout_secs = 1;

    // Warm up once so the FD count includes any one-time allocations.
    {
        let spec = HookSpec::from_command(script.path.to_str().unwrap());
        if let Ok(mut runner) = HookRunner::new(&[spec], config.clone()) {
            let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
            let _ = runner.run(&mut doc, None);
        }
    }
    std::thread::sleep(Duration::from_millis(100));
    let baseline = count_fds(&fd_dir);

    for _ in 0..iters {
        let spec = HookSpec::from_command(script.path.to_str().unwrap());
        if let Ok(mut runner) = HookRunner::new(&[spec], config.clone()) {
            let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
            let _ = runner.run(&mut doc, None);
        }
    }
    std::thread::sleep(Duration::from_millis(200));
    let after = count_fds(&fd_dir);

    // Allow some slack (10) for transient sockets / pipes that the test
    // harness or runtime may keep around. A real leak would scale with iters.
    assert!(
        after <= baseline + 10,
        "FD count drifted: baseline={baseline} after_{iters}_iters={after} (delta={})",
        after as i64 - baseline as i64
    );
}

// ---------------------------------------------------------------------------
// 6. SIGKILL escalation is observed within the timeout window even when the
// hook also ignores SIGINT and SIGHUP
// ---------------------------------------------------------------------------
#[test]
fn ignore_all_catchable_signals_killed_anyway() {
    let body = format!(
        "trap '' TERM INT HUP QUIT USR1 USR2\necho '{NOOP_HANDSHAKE}'\nwhile IFS= read -r line; do\n    sleep 120\ndone"
    );
    let script = write_script("trap-everything", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");

    let start = Instant::now();
    let _ = runner.run(&mut doc, None);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 10,
        "SIGKILL escalation broken when hook traps every catchable signal; elapsed={elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 7. Hook that closes stdout but stays alive (no responses, but not EOF on
// the channel until process exit)
// ---------------------------------------------------------------------------
#[test]
fn hook_closes_stdout_then_hangs() {
    let body =
        format!("echo '{NOOP_HANDSHAKE}'\nexec 1>&-\n# stdout closed; sleep forever\nsleep 120");
    let script = write_script("close-stdout", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");

    let start = Instant::now();
    let _ = runner.run(&mut doc, None);
    let elapsed = start.elapsed();

    // EOF on the channel is detected; runner should not wait for the full
    // sleep window.
    assert!(
        elapsed.as_secs() < 10,
        "EOF detection broken when hook closes stdout; elapsed={elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// 8. Hook that takes forever to write its first byte after handshake
// ---------------------------------------------------------------------------
#[test]
fn hook_silent_after_handshake() {
    let body = format!(
        "echo '{NOOP_HANDSHAKE}'\n# Read stdin but never write a response.\nwhile IFS= read -r line; do\n    sleep 30\ndone"
    );
    let script = write_script("silent-after-handshake", &body);
    let spec = HookSpec::from_command(script.path.to_str().unwrap());

    let mut config = HookConfig::default();
    config.page_timeout_secs = 2;

    let mut doc = udoc::extract(small_pdf()).expect("extract pdf");
    let mut runner = HookRunner::new(&[spec], config).expect("spawn hook");

    let start = Instant::now();
    let _ = runner.run(&mut doc, None);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 10,
        "silent-after-handshake hook not killed promptly; elapsed={elapsed:?}"
    );
}
