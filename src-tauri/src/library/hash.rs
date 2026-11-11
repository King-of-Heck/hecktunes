use anyhow::{Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// BLAKE3 of a file's bytes as lowercase hex. Used to tell whether a card copy is current.
pub fn file_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("couldn't open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).with_context(|| format!("couldn't read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// BLAKE3 of in-memory bytes (generated playlist files).
pub fn bytes_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_known_vector_and_follow_content() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        std::fs::write(&a, b"abc").unwrap();
        assert_eq!(file_hash(&a).unwrap(), "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85");
        assert_eq!(bytes_hash(b"abc"), file_hash(&a).unwrap());
        std::fs::write(&a, b"abd").unwrap();
        assert_ne!(file_hash(&a).unwrap(), bytes_hash(b"abc"));
    }

    #[test]
    fn large_file_hash_equals_in_memory_hash() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.bin");
        let data: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        assert_eq!(file_hash(&p).unwrap(), bytes_hash(&data));
    }

    #[test]
    fn missing_file_is_an_error_naming_the_path() {
        let err = file_hash(Path::new("definitely/not/here.flac")).unwrap_err();
        assert!(format!("{err:#}").contains("here.flac"));
    }
}
