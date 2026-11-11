use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// Runs `echo-sync <card> --yes` without a console window and reports how it went.
pub fn run(exe: &Path, card: &Path) -> EchoSyncOutcome {
    let mut cmd = Command::new(exe);
    cmd.arg(card).arg("--yes").stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Err(e) => EchoSyncOutcome::Missing { reason: format!("couldn't start echo-sync: {e}") },
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            match out.status.code() {
                Some(0) => EchoSyncOutcome::Done { log: tail(&stdout, LOG_LINES) },
                Some(NOT_AVAILABLE_EXIT) => EchoSyncOutcome::Missing { reason: not_available_reason(&stderr) },
                code => EchoSyncOutcome::Failed { code, log: tail(&format!("{stdout}{stderr}"), LOG_LINES) },
            }
        }
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
        match run(&fake(dir.path(), 0), &card) {
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
        match run(&fake(dir.path(), 2), dir.path()) {
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
        assert!(matches!(run(&fake(dir.path(), NOT_AVAILABLE_EXIT), dir.path()), EchoSyncOutcome::Missing { .. }));
        assert!(matches!(run(&dir.path().join("no-such-echo-sync"), dir.path()), EchoSyncOutcome::Missing { .. }));
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
        match run(&silent(dir.path(), NOT_AVAILABLE_EXIT), dir.path()) {
            EchoSyncOutcome::Missing { reason } => assert_eq!(reason, "echo-sync isn't available for this platform"),
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
