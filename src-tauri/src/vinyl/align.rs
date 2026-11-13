//! Placing track starts: the release's track lengths say roughly where, detected gaps say exactly.

use super::envelope::FRAME_MS;
use super::gaps::Gap;
use crate::model::CandidateTrack;
use serde::{Deserialize, Serialize};

/// ±8 s: how far an expected cut may be from a gap and still take it.
pub const MATCH_TOLERANCE_FRAMES: usize = 160;
/// ±5 s: where to look for the quietest moment when no gap is near.
pub const SEARCH_FRAMES: usize = 100;
/// 1 s window used to judge "quietest".
pub const QUIET_WINDOW_FRAMES: usize = 20;
/// The flip gap must be within this fraction of side A's expected length of where side A should end.
pub const FLIP_TOLERANCE: f64 = 0.10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutKind {
    /// The first track of a file or side.
    Start,
    /// Placed in a detected gap.
    Gap,
    /// No gap near the expected place: put at the quietest moment. Worth a listen.
    Estimated,
    /// Added or moved by the user on the review screen.
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    pub frame: usize,
    pub kind: CutKind,
}

/// Track lengths in ms, unknown ones replaced by the median of the known (all equal if none known).
pub fn fill_unknown(lengths_ms: &[Option<u64>]) -> Vec<f64> {
    let mut known: Vec<u64> = lengths_ms.iter().flatten().copied().collect();
    known.sort_unstable();
    let median = known.get(known.len() / 2).map(|&m| m as f64).unwrap_or(1.0);
    lengths_ms.iter().map(|l| l.map(|v| v as f64).unwrap_or(median)).collect()
}

/// Starts of `lengths_ms.len()` tracks laid over the music in frames `[start, end)`.
pub fn align(db: &[f32], gaps: &[Gap], start: usize, end: usize, lengths_ms: &[Option<u64>]) -> Vec<Cut> {
    let n = lengths_ms.len();
    if n == 0 {
        return vec![];
    }
    let mut cuts = vec![Cut { frame: start, kind: CutKind::Start }];
    if n == 1 || end <= start {
        return cuts;
    }
    let lens = fill_unknown(lengths_ms);
    let total: f64 = lens.iter().sum();
    let span = (end - start) as f64;
    // Scaling to the music's own length absorbs turntable speed and gaps not in the track lengths.
    let expected: Vec<usize> = (1..n).map(|k| start + (lens[..k].iter().sum::<f64>() / total * span).round() as usize).collect();
    let gap_cuts: Vec<usize> = gaps.iter().filter(|g| g.start >= start && g.end <= end).map(Gap::cut).collect();
    let assigned = match_cuts(&expected, &gap_cuts);
    for (k, &e) in expected.iter().enumerate() {
        let prev = cuts.last().expect("starts with one").frame;
        let cut = match assigned[k] {
            Some(j) => Cut { frame: gap_cuts[j].max(prev + 1), kind: CutKind::Gap },
            None => Cut { frame: quietest(db, e, prev + 1, end.saturating_sub(1)), kind: CutKind::Estimated },
        };
        cuts.push(cut);
    }
    cuts
}

/// For each expected cut, the index of the gap cut it takes (in order, each gap at most once,
/// within ±MATCH_TOLERANCE_FRAMES), minimising the total distance.
fn match_cuts(expected: &[usize], gaps: &[usize]) -> Vec<Option<usize>> {
    const INF: usize = usize::MAX / 2;
    let (m, g) = (expected.len(), gaps.len());
    let penalty = MATCH_TOLERANCE_FRAMES + 1;
    let mut dp = vec![vec![INF; g + 1]; m + 1];
    dp[0].fill(0);
    for i in 0..=m {
        for j in 0..=g {
            let here = dp[i][j];
            if here == INF {
                continue;
            }
            if j < g && here < dp[i][j + 1] {
                dp[i][j + 1] = here; // gap j left unused
            }
            if i < m {
                if here + penalty < dp[i + 1][j] {
                    dp[i + 1][j] = here + penalty; // expected cut i takes no gap
                }
                if j < g {
                    let d = expected[i].abs_diff(gaps[j]);
                    if d <= MATCH_TOLERANCE_FRAMES && here + d < dp[i + 1][j + 1] {
                        dp[i + 1][j + 1] = here + d;
                    }
                }
            }
        }
    }
    let mut out = vec![None; m];
    let (mut i, mut j) = (m, g);
    while i > 0 {
        if j > 0 && dp[i][j] == dp[i][j - 1] {
            j -= 1;
        } else if dp[i][j] == dp[i - 1][j] + penalty {
            i -= 1;
        } else {
            out[i - 1] = Some(j - 1);
            i -= 1;
            j -= 1;
        }
    }
    out
}

fn window_mean(db: &[f32], centre: usize) -> f32 {
    let half = QUIET_WINDOW_FRAMES / 2;
    let (s, e) = (centre.saturating_sub(half), (centre + half).min(db.len()));
    if s >= e {
        return 0.0;
    }
    db[s..e].iter().sum::<f32>() / (e - s) as f32
}

/// The centre of the quietest 1 s window within ±SEARCH_FRAMES of `around`, kept within `[lo, hi]`.
fn quietest(db: &[f32], around: usize, lo: usize, hi: usize) -> usize {
    let a = around.saturating_sub(SEARCH_FRAMES).max(lo);
    let b = (around + SEARCH_FRAMES).min(hi).min(db.len().saturating_sub(1));
    if a > b {
        return around.clamp(lo, hi.max(lo));
    }
    (a..=b).min_by(|&x, &y| window_mean(db, x).total_cmp(&window_mean(db, y))).unwrap_or(around)
}

/// How many tracks are on side A, when every track has a side letter and there are at least two sides.
pub fn side_a_count(tracks: &[CandidateTrack]) -> Option<usize> {
    let first = tracks.first()?.side.clone()?;
    if !tracks.iter().all(|t| t.side.is_some()) {
        return None;
    }
    let k = tracks.iter().take_while(|t| t.side.as_deref() == Some(first.as_str())).count();
    (k < tracks.len()).then_some(k)
}

/// How many of `tracks` belong on the first of two files.
pub fn split_sides(tracks: &[CandidateTrack], music_ms: [u64; 2]) -> usize {
    if let Some(k) = side_a_count(tracks) {
        return k;
    }
    let lens = fill_unknown(&tracks.iter().map(|t| t.length_ms).collect::<Vec<_>>());
    let misfit = |k: usize| (lens[..k].iter().sum::<f64>() - music_ms[0] as f64).abs() + (lens[k..].iter().sum::<f64>() - music_ms[1] as f64).abs();
    (1..tracks.len()).min_by(|&a, &b| misfit(a).total_cmp(&misfit(b))).unwrap_or(tracks.len())
}

/// One file holding both sides: the longest gap within FLIP_TOLERANCE of where side A should end
/// (`side_a_ms` of music after `start`).
pub fn flip_gap(gaps: &[Gap], start: usize, side_a_ms: f64) -> Option<Gap> {
    let frames = side_a_ms / FRAME_MS as f64;
    let target = start as f64 + frames;
    gaps.iter().copied().filter(|g| (g.start as f64 - target).abs() <= frames * FLIP_TOLERANCE).max_by_key(|g| g.len())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{candidate, db_frames, HISS_DB as H};
    use crate::vinyl::gaps::analyse;

    fn cuts(parts: &[(f32, f32)], lengths: &[Option<u64>]) -> Vec<Cut> {
        let db = db_frames(parts);
        let a = analyse(&db);
        align(&db, &a.gaps, a.music_start, a.music_end, lengths)
    }

    const THREE_SONGS: [(f32, f32); 7] = [(3.0, H), (30.0, -15.0), (2.0, H), (40.0, -15.0), (2.5, H), (20.0, -15.0), (4.0, H)];
    use CutKind::*;

    #[test]
    fn cuts_land_in_the_gaps() {
        let got = cuts(&THREE_SONGS, &[Some(30_000), Some(40_000), Some(20_000)]);
        assert_eq!(got, vec![Cut { frame: 60, kind: Start }, Cut { frame: 694, kind: Gap }, Cut { frame: 1544, kind: Gap }]);
    }

    #[test]
    fn a_fast_turntable_changes_nothing() {
        let got = cuts(&THREE_SONGS, &[Some(30_300), Some(40_400), Some(20_200)]);
        assert_eq!(got.iter().map(|c| c.frame).collect::<Vec<_>>(), vec![60, 694, 1544]);
    }

    #[test]
    fn songs_that_run_together_get_an_estimated_cut_at_the_quietest_moment() {
        let got = cuts(
            &[(3.0, H), (30.0, -15.0), (0.5, -30.0), (40.0, -15.0), (2.5, H), (20.0, -15.0), (4.0, H)],
            &[Some(30_000), Some(40_000), Some(20_000)],
        );
        assert_eq!(got[1].kind, Estimated);
        assert!((655..=675).contains(&got[1].frame), "{:?}", got[1]);
        assert_eq!(got[2], Cut { frame: 1514, kind: Gap });
    }

    #[test]
    fn a_pause_inside_a_song_is_ignored() {
        let got = cuts(&[(3.0, H), (12.0, -15.0), (1.5, H), (18.0, -15.0), (2.0, H), (40.0, -15.0), (3.0, H)], &[Some(31_500), Some(40_000)]);
        assert_eq!(got, vec![Cut { frame: 60, kind: Start }, Cut { frame: 724, kind: Gap }]);
    }

    #[test]
    fn unknown_lengths_use_the_median() {
        let got = cuts(&THREE_SONGS, &[Some(30_000), None, Some(20_000)]);
        assert_eq!(got.iter().map(|c| (c.frame, c.kind)).collect::<Vec<_>>(), vec![(60, Start), (694, Gap), (1544, Gap)]);
    }

    #[test]
    fn matching_prefers_the_nearest_gaps_in_order() {
        assert_eq!(match_cuts(&[100, 500], &[90, 300, 520]), vec![Some(0), Some(2)]);
        assert_eq!(match_cuts(&[100, 500], &[900]), vec![None, None]);
    }

    fn sided(sides: &[&str], ms: u64) -> Vec<crate::model::CandidateTrack> {
        let names: Vec<(String, u64)> = sides.iter().enumerate().map(|(i, _)| (format!("T{i}"), ms)).collect();
        let refs: Vec<(&str, u64)> = names.iter().map(|(n, l)| (n.as_str(), *l)).collect();
        let mut tracks = candidate("m", "A", "B", &refs).tracks;
        for (t, s) in tracks.iter_mut().zip(sides) {
            t.side = (!s.is_empty()).then(|| s.to_string());
        }
        tracks
    }

    #[test]
    fn sides_split_by_letter_or_by_best_fit() {
        assert_eq!(split_sides(&sided(&["A", "A", "B", "B", "B"], 100_000), [0, 0]), 2);
        assert_eq!(side_a_count(&sided(&["", "", ""], 1)), None);
        assert_eq!(split_sides(&sided(&["", "", "", "", ""], 100_000), [205_000, 300_000]), 2);
    }

    #[test]
    fn the_flip_gap_is_the_longest_gap_near_the_end_of_side_a() {
        let gaps = [crate::vinyl::gaps::Gap { start: 660, end: 700 }, crate::vinyl::gaps::Gap { start: 1500, end: 1900 }];
        assert_eq!(flip_gap(&gaps, 60, 72_000.0), Some(gaps[1]));
        assert_eq!(flip_gap(&gaps, 60, 20_000.0), None);
    }
}
