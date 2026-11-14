//! A small diagnostic log (`<app data>/hecktunes.log`): start-ups, library loads, panics and
//! problems the window reports. It exists to explain rare failures that can't be reproduced, such
//! as the library showing empty on start-up. It never holds personal data beyond file paths.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// The log is restarted when it grows past this, so it never takes real space.
const MAX_BYTES: u64 = 512 * 1024;

static LOG: OnceLock<Mutex<PathBuf>> = OnceLock::new();

/// Starts logging to `<data_dir>/hecktunes.log` and records panics there too.
pub fn init(data_dir: &Path) {
    let path = data_dir.join("hecktunes.log");
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = LOG.set(Mutex::new(path));
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        log(&format!("PANIC in thread '{thread}': {info}"));
        default_hook(info);
    }));
}

/// Appends one timestamped line (seconds since the epoch, with milliseconds). Never fails.
pub fn log(line: &str) {
    let Some(lock) = LOG.get() else { return };
    let path = lock.lock().unwrap_or_else(|p| p.into_inner());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&*path) {
        let _ = writeln!(f, "{}.{:03} [{}] {line}", now.as_secs(), now.subsec_millis(), std::process::id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_panics_are_written() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path());
        log("hello");
        let _ = std::thread::Builder::new().name("boom".into()).spawn(|| panic!("on purpose")).unwrap().join();
        let text = std::fs::read_to_string(dir.path().join("hecktunes.log")).unwrap();
        assert!(text.contains("hello"), "{text}");
        assert!(text.contains("PANIC in thread 'boom'") && text.contains("on purpose"), "{text}");
    }
}
