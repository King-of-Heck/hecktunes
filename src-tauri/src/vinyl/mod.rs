//! Vinyl recording import: split a full-side (or full-album) WAV into tracks.
pub mod wav;
pub mod envelope;
pub mod gaps;
pub mod align;
pub mod encode;
pub mod identify;

use crate::device::detect::available_space;
use crate::import::scan::AlbumGroup;
use crate::model::{ReleaseCandidate, SourceTrack};
use align::{fill_unknown, flip_gap, side_a_count, split_sides, CutKind};
use anyhow::{bail, Result};
use envelope::frame_to_ms;
use gaps::Analysis;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::{Path, PathBuf};
use wav::WavInfo;

/// 5 s: in a one-file recording, the longest gap counts as the side flip only if at least this long.
pub const FLIP_MIN_FRAMES: usize = 100;
/// A release fits when its length is within this fraction of the recording's music.
pub const FIT_TOLERANCE: f64 = 0.05;
/// FLAC is assumed to need at most this fraction of the WAV's size.
const FLAC_SIZE_RATIO: f64 = 0.7;

pub struct FileAnalysis {
    pub info: WavInfo,
    pub db: Vec<f32>,
    pub analysis: Analysis,
}

pub struct Recording {
    pub files: Vec<FileAnalysis>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanFile {
    pub path: PathBuf,
    pub name: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTrack {
    /// Index into `VinylPlan::files`.
    pub file: usize,
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
    /// How this track's start was placed.
    pub cut: CutKind,
    pub disc: u32,
    pub number: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VinylPlan {
    pub files: Vec<PlanFile>,
    pub release: Option<ReleaseCandidate>,
    pub candidates: Vec<ReleaseCandidate>,
    pub tracks: Vec<PlanTrack>,
    /// Shown on the review screen, e.g. the release's length doesn't fit.
    pub warning: Option<String>,
    /// Why there is no match (lookup failure or nothing found).
    pub note: Option<String>,
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Probes, orders and analyses one or two WAVs.
pub fn load(paths: &[PathBuf]) -> Result<Recording> {
    match paths.len() {
        1 | 2 => {}
        0 => bail!("Choose a WAV recording."),
        n => bail!("Choose one or two WAV files (one per side), not {n}."),
    }
    let mut paths = paths.to_vec();
    paths.sort_by_key(|p| file_name(p).to_lowercase());
    let infos = paths.iter().map(|p| wav::probe(p)).collect::<Result<Vec<_>>>()?;
    if let [a, b] = infos.as_slice() {
        if !a.same_format(b) {
            bail!("The two sides don't match ({} vs {}).", a.describe(), b.describe());
        }
    }
    let files = infos
        .into_iter()
        .map(|info| {
            let db = envelope::envelope(&info)?;
            let analysis = gaps::analyse(&db);
            Ok(FileAnalysis { info, db, analysis })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Recording { files })
}

impl Recording {
    /// Music length in ms: run-in and run-out excluded, and in a one-file recording the side flip.
    pub fn music_ms(&self) -> u64 {
        let span: usize = self.files.iter().map(|f| f.analysis.music_end - f.analysis.music_start).sum();
        let flip = match self.files.as_slice() {
            [f] => f.analysis.gaps.iter().map(|g| g.len()).max().filter(|&l| l >= FLIP_MIN_FRAMES).unwrap_or(0),
            _ => 0,
        };
        frame_to_ms(span.saturating_sub(flip))
    }

    /// The pieces between gaps, as (file, start frame, end frame), in order.
    pub fn pieces(&self) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for (fi, f) in self.files.iter().enumerate() {
            let a = &f.analysis;
            let mut starts = vec![a.first_start()];
            starts.extend(a.gaps.iter().map(|g| g.cut()));
            let last = a.last_end(f.db.len());
            for (i, &s) in starts.iter().enumerate() {
                let e = starts.get(i + 1).copied().unwrap_or(last);
                if e > s {
                    out.push((fi, s, e));
                }
            }
        }
        out
    }
}

/// Total length of the release, if every track's length is known.
pub fn release_ms(r: &ReleaseCandidate) -> Option<u64> {
    r.tracks.iter().map(|t| t.length_ms).sum()
}

fn mmss(ms: u64) -> String {
    let s = (ms + 500) / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

fn fit_warning(music_ms: u64, r: &ReleaseCandidate) -> Option<String> {
    let total = release_ms(r)?;
    let off = total.abs_diff(music_ms) as f64 / total.max(1) as f64;
    (off > FIT_TOLERANCE).then(|| {
        format!("This recording has {} of music but the release runs {}. It may be a different edition.", mmss(music_ms), mmss(total))
    })
}

/// The review screen's warning: `message` (something that just went wrong, e.g. a failed search or
/// a refused import) followed by the release's length warning, which stays until the release changes.
pub fn warning(rec: &Recording, release: Option<&ReleaseCandidate>, message: Option<String>) -> Option<String> {
    let fit = release.filter(|r| !r.tracks.is_empty()).and_then(|r| fit_warning(rec.music_ms(), r));
    match (message, fit) {
        (Some(m), Some(f)) => Some(format!("{m}\n{f}")),
        (m, f) => m.or(f),
    }
}

pub fn plan(rec: &Recording, release: Option<&ReleaseCandidate>, candidates: Vec<ReleaseCandidate>, note: Option<String>) -> VinylPlan {
    let files = rec
        .files
        .iter()
        .map(|f| PlanFile { path: f.info.path.clone(), name: file_name(&f.info.path), duration_ms: f.info.duration_ms() })
        .collect();
    let mut tracks = match release {
        Some(r) if !r.tracks.is_empty() => release_tracks(rec, r),
        _ => gap_tracks(rec),
    };
    let warning = warning(rec, release, None);
    // Envelope frames round the file's length up to 50 ms: keep every track inside its file.
    for t in &mut tracks {
        let dur = rec.files[t.file].info.duration_ms();
        t.end_ms = t.end_ms.min(dur);
        t.start_ms = t.start_ms.min(t.end_ms.saturating_sub(1));
    }
    VinylPlan { files, release: release.cloned(), candidates, tracks, warning, note }
}

fn gap_tracks(rec: &Recording) -> Vec<PlanTrack> {
    let mut prev_file = usize::MAX;
    rec.pieces()
        .into_iter()
        .enumerate()
        .map(|(i, (file, s, e))| {
            let cut = if file != prev_file { CutKind::Start } else { CutKind::Gap };
            prev_file = file;
            PlanTrack { file, start_ms: frame_to_ms(s), end_ms: frame_to_ms(e), title: format!("Track {}", i + 1), cut, disc: 1, number: i as u32 + 1 }
        })
        .collect()
}

/// A stretch of one file that holds a run of the release's tracks.
struct Stretch {
    file: usize,
    /// Music frames used for scaling the track lengths.
    music: (usize, usize),
    /// Where the stretch's first track starts and last track ends.
    first_start: usize,
    last_end: usize,
    tracks: Range<usize>,
}

fn stretches(rec: &Recording, r: &ReleaseCandidate) -> Vec<Stretch> {
    let n = r.tracks.len();
    let whole = |fi: usize, tracks: Range<usize>| {
        let f = &rec.files[fi];
        let a = &f.analysis;
        Stretch { file: fi, music: (a.music_start, a.music_end), first_start: a.first_start(), last_end: a.last_end(f.db.len()), tracks }
    };
    match rec.files.as_slice() {
        [f] => {
            let a = &f.analysis;
            let flip = side_a_count(&r.tracks).and_then(|k| {
                let lens = fill_unknown(&r.tracks.iter().map(|t| t.length_ms).collect::<Vec<_>>());
                flip_gap(&a.gaps, a.music_start, lens[..k].iter().sum()).map(|g| (k, g))
            });
            match flip {
                Some((k, g)) => vec![
                    Stretch {
                        file: 0,
                        music: (a.music_start, g.start),
                        first_start: a.first_start(),
                        last_end: (g.start + gaps::RUN_OUT_TAIL_FRAMES).min(g.cut()),
                        tracks: 0..k,
                    },
                    Stretch { file: 0, music: (g.end, a.music_end), first_start: g.cut(), last_end: a.last_end(f.db.len()), tracks: k..n },
                ],
                None => vec![whole(0, 0..n)],
            }
        }
        _ => {
            let ms = |fi: usize| frame_to_ms(rec.files[fi].analysis.music_end - rec.files[fi].analysis.music_start);
            let k = split_sides(&r.tracks, [ms(0), ms(1)]);
            vec![whole(0, 0..k), whole(1, k..n)]
        }
    }
}

fn release_tracks(rec: &Recording, r: &ReleaseCandidate) -> Vec<PlanTrack> {
    let lens: Vec<Option<u64>> = r.tracks.iter().map(|t| t.length_ms).collect();
    let mut out = Vec::new();
    for s in stretches(rec, r) {
        let f = &rec.files[s.file];
        let cuts = align::align(&f.db, &f.analysis.gaps, s.music.0, s.music.1, &lens[s.tracks.clone()]);
        for (i, c) in cuts.iter().enumerate() {
            let rt = &r.tracks[s.tracks.start + i];
            let start = if i == 0 { s.first_start } else { c.frame };
            let end = cuts.get(i + 1).map(|n| n.frame).unwrap_or(s.last_end);
            out.push(PlanTrack {
                file: s.file,
                start_ms: frame_to_ms(start),
                end_ms: frame_to_ms(end.max(start + 1)),
                title: rt.title.clone(),
                cut: c.kind,
                disc: rt.disc,
                number: rt.number,
            });
        }
    }
    out
}

/// Refuses a cut list that can't be imported as given.
pub fn check_tracks(rec: &Recording, tracks: &[PlanTrack], release: Option<&ReleaseCandidate>) -> Result<()> {
    if tracks.is_empty() {
        bail!("There are no tracks to import.");
    }
    for (i, t) in tracks.iter().enumerate() {
        let Some(f) = rec.files.get(t.file) else { bail!("Track {} refers to a missing file.", i + 1) };
        if t.start_ms >= t.end_ms || t.end_ms > f.info.duration_ms() {
            bail!("Track {} has an impossible start or end.", i + 1);
        }
    }
    if let Some(r) = release {
        if tracks.len() != r.tracks.len() {
            bail!("{} pieces, but this release has {} tracks.", tracks.len(), r.tracks.len());
        }
    }
    Ok(())
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// Why the FLACs of `tracks` won't fit on the drive holding `dir`, if they won't. Writes nothing
/// (the free space is measured at the nearest existing folder).
pub fn space_problem(rec: &Recording, tracks: &[PlanTrack], dir: &Path) -> Option<String> {
    let need: u64 = tracks
        .iter()
        .map(|t| (t.end_ms.saturating_sub(t.start_ms) * rec.files[t.file].info.byte_rate() / 1000) as f64 * FLAC_SIZE_RATIO)
        .sum::<f64>() as u64;
    let existing = dir.ancestors().find(|d| d.exists())?;
    let free = available_space(existing)?;
    (free < need).then(|| format!("Not enough free space: the tracks need about {} but only {} is free.", gb(need), gb(free)))
}

/// Encodes each track into `dir` (`01.flac`, `02.flac`...) and returns them as an album to import.
pub fn write_tracks(rec: &Recording, tracks: &[PlanTrack], dir: &Path) -> Result<AlbumGroup> {
    if let Some(problem) = space_problem(rec, tracks, dir) {
        bail!(problem);
    }
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::with_capacity(tracks.len());
    for (i, t) in tracks.iter().enumerate() {
        let info = &rec.files[t.file].info;
        let path = dir.join(format!("{:02}.flac", i + 1));
        encode::encode_range(info, info.ms_to_frame(t.start_ms), info.ms_to_frame(t.end_ms), &path)?;
        out.push(SourceTrack {
            path,
            title: Some(t.title.clone()),
            artist: None,
            album: None,
            album_artist: None,
            track_no: Some(t.number),
            disc_no: Some(t.disc),
            year: None,
            genre: None,
            duration_ms: t.end_ms - t.start_ms,
        });
    }
    let source_dir = rec.files[0].info.path.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(AlbumGroup { tracks: out, source_dir })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{candidate, hiss, tone, write_pattern_wav, Part};

    const THREE: [Part; 7] = [hiss(3.0), tone(30.0, 0.2), hiss(2.0), tone(40.0, 0.2), hiss(2.5), tone(20.0, 0.2), hiss(4.0)];

    fn wav(dir: &Path, name: &str, rate: u32, parts: &[Part]) -> PathBuf {
        let p = dir.join(name);
        write_pattern_wav(&p, rate, 1, 16, parts);
        p
    }

    fn near(a: u64, b: u64) -> bool {
        a.abs_diff(b) <= 100
    }

    fn spans(p: &VinylPlan) -> Vec<(usize, u64, u64)> {
        p.tracks.iter().map(|t| (t.file, t.start_ms, t.end_ms)).collect()
    }

    #[test]
    fn load_takes_one_or_two_matching_wavs_in_name_order() {
        let dir = tempfile::tempdir().unwrap();
        let b = wav(dir.path(), "Side B.wav", 8000, &[tone(1.0, 0.2)]);
        let a = wav(dir.path(), "side a.wav", 8000, &[tone(1.0, 0.2)]);
        let rec = load(&[b.clone(), a.clone()]).unwrap();
        assert_eq!(rec.files[0].info.path, a);
        let c = wav(dir.path(), "c.wav", 16000, &[tone(1.0, 0.2)]);
        let e = format!("{:#}", load(&[a.clone(), c]).err().unwrap());
        assert!(e.contains("don't match"), "{e}");
        assert!(load(&[a.clone(), a.clone(), a]).is_err());
    }

    #[test]
    fn without_a_release_tracks_are_cut_at_the_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let rec = load(&[wav(dir.path(), "side.wav", 8000, &THREE)]).unwrap();
        let p = plan(&rec, None, vec![], Some("No match found online".into()));
        let s = spans(&p);
        assert_eq!(s.len(), 3);
        let want = [(2500, 34_700), (34_700, 77_200), (77_200, 98_500)];
        for (got, want) in s.iter().zip(want) {
            assert!(near(got.1, want.0) && near(got.2, want.1), "{s:?}");
        }
        assert_eq!(p.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Track 1", "Track 2", "Track 3"]);
        assert_eq!(p.note.as_deref(), Some("No match found online"));
    }

    #[test]
    fn a_release_names_the_tracks_and_places_the_cuts() {
        let dir = tempfile::tempdir().unwrap();
        let rec = load(&[wav(dir.path(), "side.wav", 8000, &THREE)]).unwrap();
        let r = candidate("m1", "Album", "Band", &[("One", 30_000), ("Two", 40_000), ("Three", 20_000)]);
        let p = plan(&rec, Some(&r), vec![r.clone()], None);
        assert_eq!(p.tracks.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["One", "Two", "Three"]);
        assert_eq!(p.tracks.iter().map(|t| t.cut).collect::<Vec<_>>(), [CutKind::Start, CutKind::Gap, CutKind::Gap]);
        assert!(near(p.tracks[1].start_ms, 34_700));
        assert_eq!(p.warning, None);
    }

    #[test]
    fn a_release_of_the_wrong_length_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let rec = load(&[wav(dir.path(), "side.wav", 8000, &THREE)]).unwrap();
        let r = candidate("m1", "Album", "Band", &[("One", 60_000), ("Two", 60_000), ("Three", 60_000)]);
        let w = plan(&rec, Some(&r), vec![], None).warning.unwrap();
        assert!(w.contains("different edition"), "{w}");
    }

    #[test]
    fn two_sides_split_by_side_letter() {
        let dir = tempfile::tempdir().unwrap();
        let a = wav(dir.path(), "a.wav", 8000, &[hiss(3.0), tone(30.0, 0.2), hiss(2.0), tone(40.0, 0.2), hiss(3.0)]);
        let b = wav(dir.path(), "b.wav", 8000, &[hiss(3.0), tone(20.0, 0.2), hiss(3.0)]);
        let rec = load(&[a, b]).unwrap();
        let mut r = candidate("m1", "Album", "Band", &[("One", 30_000), ("Two", 40_000), ("Three", 20_000)]);
        for (t, s) in r.tracks.iter_mut().zip(["A", "A", "B"]) {
            t.side = Some(s.into());
        }
        let p = plan(&rec, Some(&r), vec![], None);
        assert_eq!(p.tracks.iter().map(|t| t.file).collect::<Vec<_>>(), [0, 0, 1]);
        assert_eq!(p.tracks[2].cut, CutKind::Start);
    }

    #[test]
    fn one_file_with_both_sides_splits_at_the_flip() {
        let dir = tempfile::tempdir().unwrap();
        let parts = [hiss(3.0), tone(30.0, 0.2), hiss(2.0), tone(40.0, 0.2), hiss(20.0), tone(30.0, 0.2), hiss(2.0), tone(25.0, 0.2), hiss(4.0)];
        let rec = load(&[wav(dir.path(), "lp.wav", 8000, &parts)]).unwrap();
        let mut r = candidate("m1", "LP", "Band", &[("A1", 30_000), ("A2", 40_000), ("B1", 30_000), ("B2", 25_000)]);
        for (t, s) in r.tracks.iter_mut().zip(["A", "A", "B", "B"]) {
            t.side = Some(s.into());
        }
        let p = plan(&rec, Some(&r), vec![], None);
        assert_eq!(p.tracks.len(), 4);
        assert!(near(p.tracks[1].end_ms, 76_000), "{:?}", spans(&p));
        assert!(near(p.tracks[2].start_ms, 94_700), "{:?}", spans(&p));
        assert_eq!(p.warning, None);
    }

    #[test]
    fn music_to_the_very_end_still_passes_the_check() {
        // 44.1 kHz with no run-out and a length that isn't a whole number of 50 ms frames: the last
        // envelope frame is short, so frame times overshoot the file.
        let dir = tempfile::tempdir().unwrap();
        let parts = [hiss(3.0), tone(30.0, 0.2), hiss(2.0), tone(40.0, 0.2), hiss(2.5), tone(20.03, 0.2)];
        let rec = load(&[wav(dir.path(), "side.wav", 44100, &parts)]).unwrap();
        let dur = rec.files[0].info.duration_ms();
        assert_ne!(dur % 50, 0);
        let p = plan(&rec, None, vec![], None);
        assert_eq!(p.tracks.last().unwrap().end_ms, dur);
        check_tracks(&rec, &p.tracks, None).unwrap();
        let r = candidate("m1", "Album", "Band", &[("One", 30_000), ("Two", 40_000), ("Three", 20_000)]);
        let p = plan(&rec, Some(&r), vec![], None);
        assert_eq!(p.tracks.last().unwrap().end_ms, dur);
        check_tracks(&rec, &p.tracks, Some(&r)).unwrap();
    }

    #[test]
    fn check_tracks_wants_one_piece_per_release_track() {
        let dir = tempfile::tempdir().unwrap();
        let rec = load(&[wav(dir.path(), "side.wav", 8000, &THREE)]).unwrap();
        let r = candidate("m1", "Album", "Band", &[("One", 30_000), ("Two", 40_000), ("Three", 20_000)]);
        let p = plan(&rec, Some(&r), vec![], None);
        assert!(check_tracks(&rec, &p.tracks, Some(&r)).is_ok());
        let e = check_tracks(&rec, &p.tracks[..2], Some(&r)).unwrap_err().to_string();
        assert!(e.contains("2 pieces") && e.contains("3 tracks"), "{e}");
        assert!(check_tracks(&rec, &p.tracks[..2], None).is_ok());
    }

    #[test]
    fn space_problem_writes_nothing_and_accepts_a_small_job() {
        let dir = tempfile::tempdir().unwrap();
        let rec = load(&[wav(dir.path(), "side.wav", 8000, &THREE)]).unwrap();
        let p = plan(&rec, None, vec![], None);
        let out = dir.path().join("tmp").join("job-1");
        assert_eq!(space_problem(&rec, &p.tracks, &out), None);
        assert!(!out.exists());
    }

    #[test]
    fn write_tracks_makes_one_flac_per_track() {
        let dir = tempfile::tempdir().unwrap();
        let rec = load(&[wav(dir.path(), "side.wav", 8000, &THREE)]).unwrap();
        let p = plan(&rec, None, vec![], None);
        let group = write_tracks(&rec, &p.tracks, &dir.path().join("out")).unwrap();
        assert_eq!(group.tracks.len(), 3);
        assert!(group.tracks.iter().all(|t| t.path.extension().unwrap() == "flac" && t.path.exists()));
        assert_eq!(group.tracks[1].title.as_deref(), Some("Track 2"));
        assert_eq!(group.tracks[1].track_no, Some(2));
    }
}
