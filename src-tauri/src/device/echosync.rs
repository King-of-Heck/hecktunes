use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// echo-sync is stopped if it runs longer than this.
pub const ECHO_SYNC_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How often a running echo-sync is checked for exit, timeout and cancel.
const POLL: Duration = Duration::from_millis(100);
/// After echo-sync exits, how long to wait for the last of its output.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// The echo-sync release bundled with HeckTunes (see scripts/fetch-echo-sync.mjs).
pub const ECHO_SYNC_VERSION: &str = "0.11.1";
/// Exit code of the stand-in shipped where EchoPod has no build for the platform.
pub const NOT_AVAILABLE_EXIT: i32 = 3;
const LOG_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EchoSyncOutcome {
    Done { log: String },
    Failed { code: Option<i32>, log: String },
    Missing { reason: String },
}

/// Tauri places external binaries next to the app executable (and in target/<profile> in dev).
pub fn sidecar_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join(format!("echo-sync{}", std::env::consts::EXE_SUFFIX));
    p.is_file().then_some(p)
}

/// Runs `echo-sync <card> --yes` without a console window, stopping it after
/// `ECHO_SYNC_TIMEOUT` or when `cancel` is set, and reports how it went.
pub fn run(exe: &Path, card: &Path, cancel: &AtomicBool) -> EchoSyncOutcome {
    run_with(exe, card, ECHO_SYNC_TIMEOUT, cancel)
}

/// `run` with the time limit as a parameter (tests use a short one).
pub fn run_with(exe: &Path, card: &Path, timeout: Duration, cancel: &AtomicBool) -> EchoSyncOutcome {
    let mut cmd = Command::new(exe);
    cmd.arg(card).arg("--yes").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Err(e) => return EchoSyncOutcome::Missing { reason: format!("couldn't start echo-sync: {e}") },
        Ok(c) => c,
    };
    // Drain both pipes on their own threads so a chatty echo-sync can't block on a full pipe.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let stopped = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if cancel.load(Ordering::SeqCst) => break Err(Stop::Cancelled),
            Ok(None) if started.elapsed() >= timeout => break Err(Stop::TimedOut),
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => break Err(Stop::Lost(e)),
        }
    };
    let status = match stopped {
        Ok(status) => status,
        Err(why) => {
            let _ = child.kill();
            let _ = child.wait();
            let out = format!("{}{}", stdout.collected(), stderr.collected());
            let what = match why {
                Stop::TimedOut => format!("timed out after {} seconds; echo-sync was stopped", timeout.as_secs_f32()),
                Stop::Cancelled => "cancelled; echo-sync was stopped".to_string(),
                Stop::Lost(e) => format!("couldn't check on echo-sync ({e}); it was stopped"),
            };
            let log = tail(&out, LOG_LINES);
            return EchoSyncOutcome::Failed { code: None, log: if log.is_empty() { what } else { format!("{what}\n{log}") } };
        }
    };
    let (stdout, stderr) = (stdout.finish(), stderr.finish());
    match status.code() {
        Some(0) => EchoSyncOutcome::Done { log: tail(&stdout, LOG_LINES) },
        Some(NOT_AVAILABLE_EXIT) => EchoSyncOutcome::Missing { reason: not_available_reason(&stderr) },
        code => EchoSyncOutcome::Failed { code, log: tail(&format!("{stdout}{stderr}"), LOG_LINES) },
    }
}

enum Stop {
    Cancelled,
    TimedOut,
    Lost(std::io::Error),
}

/// Collects a child's pipe on a background thread.
struct Drain {
    buf: Arc<Mutex<Vec<u8>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> Drain {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let thread = pipe.map(|mut p| {
        let buf = buf.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = p.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buf.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&chunk[..n]);
            }
        })
    });
    Drain { buf, thread }
}

impl Drain {
    /// What has arrived so far, without waiting (a killed child's own children may keep the pipe
    /// open; the reader thread then ends on its own when they do).
    fn collected(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
    }

    /// Waits briefly for the pipe to close after the child exited, then returns the output.
    fn finish(mut self) -> String {
        if let Some(t) = self.thread.take() {
            let deadline = Instant::now() + DRAIN_GRACE;
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
        self.collected()
    }
}

fn not_available_reason(stderr: &str) -> String {
    match stderr.trim() {
        "" => "echo-sync isn't available for this platform".to_string(),
        s => s.to_string(),
    }
}

/// The last `n` non-empty lines.
pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    /// A fake echo-sync that prints its arguments and exits with `code`.
    fn fake(dir: &Path, code: i32) -> PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join(format!("fake{code}.cmd"));
            std::fs::write(&p, format!("@echo off\r\necho args %1 %2\r\necho oops 1>&2\r\nexit /b {code}\r\n")).unwrap();
            p
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join(format!("fake{code}.sh"));
            std::fs::write(&p, format!("#!/bin/sh\necho \"args $1 $2\"\necho oops >&2\nexit {code}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    #[test]
    fn success_passes_the_card_and_yes_and_keeps_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let card = dir.path().join("card");
        match run(&fake(dir.path(), 0), &card, &AtomicBool::new(false)) {
            EchoSyncOutcome::Done { log } => {
                assert!(log.contains("args"), "{log}");
                assert!(log.contains("--yes"), "{log}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_failing_run_reports_its_code_and_output() {
        let dir = tempfile::tempdir().unwrap();
        match run(&fake(dir.path(), 2), dir.path(), &AtomicBool::new(false)) {
            EchoSyncOutcome::Failed { code, log } => {
                assert_eq!(code, Some(2));
                assert!(log.contains("oops"), "{log}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_platform_stand_in_and_a_missing_program_count_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(run(&fake(dir.path(), NOT_AVAILABLE_EXIT), dir.path(), &AtomicBool::new(false)), EchoSyncOutcome::Missing { .. }));
        assert!(matches!(run(&dir.path().join("no-such-echo-sync"), dir.path(), &AtomicBool::new(false)), EchoSyncOutcome::Missing { .. }));
    }

    /// A fake that exits with `code` without printing anything.
    fn silent(dir: &Path, code: i32) -> PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join(format!("silent{code}.cmd"));
            std::fs::write(&p, format!("@echo off
exit /b {code}
")).unwrap();
            p
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join(format!("silent{code}.sh"));
            std::fs::write(&p, format!("#!/bin/sh
exit {code}
")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    #[test]
    fn a_silent_platform_stand_in_still_gets_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        match run(&silent(dir.path(), NOT_AVAILABLE_EXIT), dir.path(), &AtomicBool::new(false)) {
            EchoSyncOutcome::Missing { reason } => assert_eq!(reason, "echo-sync isn't available for this platform"),
            other => panic!("{other:?}"),
        }
    }

    /// A fake echo-sync that hangs for about 12 seconds.
    fn sleeper(dir: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            let p = dir.join("sleeper.cmd");
            std::fs::write(&p, "@echo off
echo started
ping -n 12 127.0.0.1 >nul
exit /b 0
").unwrap();
            p
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join("sleeper.sh");
            std::fs::write(&p, "#!/bin/sh
echo started
sleep 30
exit 0
").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
    }

    #[test]
    fn a_run_that_takes_too_long_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let t = std::time::Instant::now();
        let out = run_with(&sleeper(dir.path()), dir.path(), Duration::from_millis(1500), &AtomicBool::new(false));
        assert!(t.elapsed() < Duration::from_secs(10), "took {:?}", t.elapsed());
        match out {
            EchoSyncOutcome::Failed { code: None, log } => assert!(log.starts_with("timed out"), "{log}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cancelling_stops_a_running_echo_sync() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let flip = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            c2.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let t = std::time::Instant::now();
        let out = run_with(&sleeper(dir.path()), dir.path(), Duration::from_secs(60), &cancel);
        flip.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(10), "took {:?}", t.elapsed());
        match out {
            EchoSyncOutcome::Failed { code: None, log } => assert!(log.starts_with("cancelled"), "{log}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\nc\n", 2), "b\nc");
        assert_eq!(tail("", 5), "");
    }

    #[test]
    fn outcome_serializes_with_a_status_tag() {
        let v = serde_json::to_value(EchoSyncOutcome::Failed { code: Some(2), log: "x".into() }).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(v["code"], 2);
    }
}
