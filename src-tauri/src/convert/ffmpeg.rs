//! Running the bundled ffmpeg to turn one FLAC file into an AAC (.m4a) file.

use anyhow::{anyhow, bail, Result};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The ffmpeg release bundled with HeckTunes (see scripts/build-ffmpeg.sh).
pub const FFMPEG_VERSION: &str = "8.1.3";
/// Where that release's source code is published.
pub const FFMPEG_SOURCE: &str = "https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz";

/// One track taking longer than this is stopped (a stuck ffmpeg, not a long track: encoding runs
/// at many times real time).
const TRACK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const POLL: Duration = Duration::from_millis(50);

/// Tauri places external binaries next to the app executable (and in target/<profile> in dev).
pub fn sidecar_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX));
    p.is_file().then_some(p)
}

/// The sample rate to resample to, if any. AAC players handle 44.1 and 48 kHz everywhere; higher
/// rates go to whichever of the two divides them (88.2/176.4 kHz to 44.1 kHz, the rest to 48 kHz).
pub fn target_rate(source_rate: u32) -> Option<u32> {
    if source_rate <= 48_000 {
        None
    } else if source_rate % 44_100 == 0 {
        Some(44_100)
    } else {
        Some(48_000)
    }
}

/// ffmpeg's arguments: first audio stream only, no tags or pictures copied (HeckTunes writes its
/// own afterwards), AAC-LC at `kbps`, in an .m4a (ipod) container.
pub fn args(source: &Path, dest: &Path, kbps: u32, rate: Option<u32>) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"].iter().map(OsString::from).collect();
    a.push(source.as_os_str().to_owned());
    for s in ["-map", "0:a:0", "-map_metadata", "-1", "-c:a", "aac", "-b:a"] {
        a.push(s.into());
    }
    a.push(format!("{kbps}k").into());
    if let Some(r) = rate {
        a.push("-ar".into());
        a.push(r.to_string().into());
    }
    for s in ["-f", "ipod"] {
        a.push(s.into());
    }
    a.push(dest.as_os_str().to_owned());
    a
}

/// Runs ffmpeg to completion without a console window. Stops it (and fails) when `cancel` is set.
pub fn run(exe: &Path, args: &[OsString], cancel: &AtomicBool) -> Result<()> {
    let mut cmd = Command::new(exe);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| anyhow!("couldn't start ffmpeg: {e}"))?;
    // Read stderr on its own thread so a chatty ffmpeg can't block on a full pipe.
    let stderr = child.stderr.take().map(|mut p| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = p.read_to_string(&mut s);
            s
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if cancel.load(Ordering::SeqCst) => break Err("cancelled"),
            Ok(None) if started.elapsed() >= TRACK_TIMEOUT => break Err("ffmpeg took too long and was stopped"),
            Ok(None) => std::thread::sleep(POLL),
            Err(_) => break Err("lost track of ffmpeg"),
        }
    };
    let status = match status {
        Ok(s) => s,
        Err(why) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{why}");
        }
    };
    let err = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    match status.code() {
        Some(code) if last.is_empty() => bail!("ffmpeg failed (exit code {code})"),
        Some(code) => bail!("ffmpeg failed (exit code {code}): {last}"),
        None => bail!("ffmpeg stopped unexpectedly"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_rates_above_48k_are_resampled_to_a_matching_family() {
        assert_eq!(target_rate(44_100), None);
        assert_eq!(target_rate(48_000), None);
        assert_eq!(target_rate(32_000), None);
        assert_eq!(target_rate(88_200), Some(44_100));
        assert_eq!(target_rate(176_400), Some(44_100));
        assert_eq!(target_rate(96_000), Some(48_000));
        assert_eq!(target_rate(192_000), Some(48_000));
    }

    #[test]
    fn arguments_encode_the_first_audio_stream_at_the_chosen_bitrate_without_copying_tags() {
        let a: Vec<String> = args(Path::new("in.flac"), Path::new("out.m4a"), 256, Some(48_000))
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            a.join(" "),
            "-nostdin -hide_banner -loglevel error -y -i in.flac -map 0:a:0 -map_metadata -1 -c:a aac -b:a 256k -ar 48000 -f ipod out.m4a"
        );
        let plain = args(Path::new("in.flac"), Path::new("out.m4a"), 192, None);
        assert!(plain.iter().any(|s| s == "192k"));
        assert!(!plain.iter().any(|s| s == "-ar"));
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let e = run(Path::new("no-such-ffmpeg-here"), &[], &AtomicBool::new(false)).unwrap_err();
        assert!(e.to_string().contains("couldn't start ffmpeg"), "{e}");
    }
}
