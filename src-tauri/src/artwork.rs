use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use std::fs;
use std::path::Path;

pub const MAX_EDGE: u32 = 600;
const COVER_STEMS: [&str; 4] = ["cover", "folder", "front", "albumart"];
const COVER_EXTS: [&str; 3] = ["jpg", "jpeg", "png"];

pub fn prepare_cover(bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).context("cover is not a readable image")?;
    let img = if img.width() > MAX_EDGE || img.height() > MAX_EDGE {
        img.resize(MAX_EDGE, MAX_EDGE, FilterType::Lanczos3)
    } else {
        img
    };
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 90).encode_image(&img.to_rgb8())?;
    Ok(out)
}

pub fn find_folder_cover(dir: &Path) -> Option<Vec<u8>> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase());
        let ext = path.extension().map(|s| s.to_string_lossy().to_ascii_lowercase());
        if let (Some(stem), Some(ext)) = (stem, ext) {
            if COVER_STEMS.contains(&stem.as_str()) && COVER_EXTS.contains(&ext.as_str()) {
                return fs::read(&path).ok();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::png_bytes;

    fn dims(jpeg: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(jpeg).unwrap();
        (img.width(), img.height())
    }

    #[test]
    fn downscales_large_cover_to_600_jpeg() {
        let out = prepare_cover(&png_bytes(1200, 800)).unwrap();
        assert_eq!(&out[0..2], &[0xFF, 0xD8]);
        assert_eq!(dims(&out), (600, 400));
    }

    #[test]
    fn keeps_small_cover_size() {
        let out = prepare_cover(&png_bytes(300, 200)).unwrap();
        assert_eq!(dims(&out), (300, 200));
    }

    #[test]
    fn rejects_garbage() {
        assert!(prepare_cover(b"definitely not an image").is_err());
    }

    #[test]
    fn finds_folder_cover_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        assert_eq!(find_folder_cover(dir.path()), None);
        std::fs::write(dir.path().join("Folder.JPG"), b"jpegbytes").unwrap();
        assert_eq!(find_folder_cover(dir.path()), Some(b"jpegbytes".to_vec()));
    }
}
