//! Finding the album: fingerprint the longest pieces and rank the releases they point to.

use super::envelope::frame_to_ms;
use super::{release_ms, wav, Recording, FIT_TOLERANCE};
use crate::identify::fingerprint::{fingerprint_pcm, MAX_SECONDS};
use crate::identify::net::is_network_failure;
use crate::identify::{MetadataSource, MAX_CANDIDATES};
use crate::model::ReleaseCandidate;
use anyhow::Result;
use std::cmp::Reverse;

/// How many pieces are fingerprinted.
pub const MAX_PIECES: usize = 3;
/// Pieces shorter than this are skipped (intros, false splits).
pub const MIN_PIECE_MS: u64 = 30_000;

fn piece_fingerprint(info: &wav::WavInfo, start_frame: usize, end_frame: usize) -> Result<String> {
    let start = info.ms_to_frame(frame_to_ms(start_frame));
    let end = info.ms_to_frame(frame_to_ms(end_frame)).min(start + MAX_SECONDS * info.sample_rate as u64);
    let shift = info.bits - 16;
    let pcm: Vec<i16> = wav::read_range(info, start, end)?.iter().map(|&s| (s >> shift) as i16).collect();
    fingerprint_pcm(&pcm, info.sample_rate, info.channels as u32)
}

/// Releases the longest pieces point to, best first. Empty when nothing was found; Err when the
/// lookups failed (offline, key rejected).
pub fn identify(rec: &Recording, source: &dyn MetadataSource) -> Result<Vec<ReleaseCandidate>> {
    let mut pieces = rec.pieces();
    pieces.sort_by_key(|&(_, s, e)| Reverse(e - s));
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut last_err = None;
    for (fi, s, e) in pieces.into_iter().filter(|&(_, s, e)| frame_to_ms(e - s) >= MIN_PIECE_MS).take(MAX_PIECES) {
        let fp = match piece_fingerprint(&rec.files[fi].info, s, e) {
            Ok(fp) => fp,
            Err(err) => {
                eprintln!("vinyl: no fingerprint for a piece: {}", format!("{err:#}").replace(|c: char| !c.is_ascii(), "?"));
                continue;
            }
        };
        let found = match source.by_fingerprint_code(&fp, (frame_to_ms(e - s) / 1000) as u32) {
            Ok(found) => found,
            Err(err) if is_network_failure(&err) => return Err(err),
            Err(err) => {
                last_err = Some(err);
                continue;
            }
        };
        let mut seen: Vec<&String> = Vec::new();
        for id in &found {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            match counts.iter_mut().find(|(k, _)| k == id) {
                Some((_, n)) => *n += 1,
                None => counts.push((id.clone(), 1)),
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    let mut found = Vec::new();
    for (id, n) in counts.into_iter().take(MAX_CANDIDATES) {
        match source.release(&id) {
            Ok(r) => found.push((r, n)),
            Err(err) if is_network_failure(&err) => return Err(err),
            Err(err) => last_err = Some(err),
        }
    }
    if found.is_empty() {
        return match last_err {
            Some(err) => Err(err),
            None => Ok(vec![]),
        };
    }
    Ok(rank(found, |r| rec.music_ms(Some(r))))
}

/// Releases that fit the recording's length first, then vinyl editions, then the most fingerprint
/// agreement, then the closest length. `music_ms` is the recording's music length as that release
/// places the side flip.
pub fn rank(found: Vec<(ReleaseCandidate, usize)>, music_ms: impl Fn(&ReleaseCandidate) -> u64) -> Vec<ReleaseCandidate> {
    let mut keyed: Vec<_> = found
        .into_iter()
        .map(|(r, agree)| {
            let music_ms = music_ms(&r);
            let diff = release_ms(&r).map(|t| t.abs_diff(music_ms));
            let fits = diff.is_some_and(|d| d as f64 <= FIT_TOLERANCE * music_ms as f64);
            let vinyl = r.format.as_deref().is_some_and(|f| f.to_lowercase().contains("vinyl"));
            ((Reverse(fits), Reverse(vinyl), Reverse(agree), diff.unwrap_or(u64::MAX)), r)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{candidate, hiss, tone, write_pattern_wav, VinylFake};
    use crate::vinyl::load;

    fn recording(dir: &std::path::Path) -> Recording {
        let p = dir.join("side.wav");
        write_pattern_wav(&p, 8000, 1, 16, &[hiss(3.0), tone(35.0, 0.2), hiss(2.0), tone(40.0, 0.2), hiss(3.0)]);
        load(&[p]).unwrap()
    }

    #[test]
    fn fingerprints_find_the_release() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recording(dir.path());
        let r = candidate("m1", "Album", "Band", &[("One", 35_000), ("Two", 40_000)]);
        let fake = VinylFake { releases: vec![r], fingerprint_ids: vec!["m1".into()], offline: false };
        assert_eq!(identify(&rec, &fake).unwrap().iter().map(|c| c.mbid.as_str()).collect::<Vec<_>>(), ["m1"]);
    }

    #[test]
    fn being_offline_is_an_error_not_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let rec = recording(dir.path());
        let fake = VinylFake { releases: vec![], fingerprint_ids: vec![], offline: true };
        assert!(identify(&rec, &fake).is_err());
    }

    #[test]
    fn ranking_puts_fitting_then_vinyl_then_agreement_first() {
        let fits_cd = candidate("cd", "A", "B", &[("x", 60_000)]);
        let mut fits_lp = candidate("lp", "A", "B", &[("x", 60_000)]);
        fits_lp.format = Some("12\" Vinyl".into());
        let mut long_lp = candidate("long", "A", "B", &[("x", 90_000)]);
        long_lp.format = Some("Vinyl".into());
        let ranked = rank(vec![(long_lp, 3), (fits_cd, 3), (fits_lp, 1)], |_| 60_000);
        assert_eq!(ranked.iter().map(|c| c.mbid.as_str()).collect::<Vec<_>>(), ["lp", "cd", "long"]);
    }
}
