//! Quiet gaps between songs, judged against the record's own noise floor.

/// The noise floor is this percentile of all frame levels (the run-in, gaps and run-out).
pub const FLOOR_PERCENTILE: f64 = 0.01;
/// Frames quieter than floor + this count as quiet.
pub const THRESHOLD_MARGIN_DB: f32 = 6.0;
/// 1.0 s: the shortest quiet run that counts as a gap.
pub const MIN_GAP_FRAMES: usize = 20;
/// 0.3 s: loud blips this short inside a quiet stretch are clicks, not music.
pub const MAX_BLIP_FRAMES: usize = 6;
/// 2.0 s: the shortest loud run that counts as music.
pub const MIN_MUSIC_FRAMES: usize = 40;
/// 0.3 s: a track placed in a gap starts this long before the music resumes.
pub const CUT_LEAD_FRAMES: usize = 6;
/// 0.5 s of run-in kept before the first track.
pub const RUN_IN_LEAD_FRAMES: usize = 10;
/// 1.0 s of run-out kept after the last track.
pub const RUN_OUT_TAIL_FRAMES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub start: usize,
    pub end: usize,
}

impl Gap {
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Where the next track starts: shortly before the music resumes.
    pub fn cut(&self) -> usize {
        self.end.saturating_sub(CUT_LEAD_FRAMES).max(self.start)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub threshold: f32,
    /// First frame of music (after the run-in).
    pub music_start: usize,
    /// One past the last frame of music (before the run-out).
    pub music_end: usize,
    /// Gaps strictly inside the music, in order.
    pub gaps: Vec<Gap>,
}

impl Analysis {
    /// Where the first track starts.
    pub fn first_start(&self) -> usize {
        self.music_start.saturating_sub(RUN_IN_LEAD_FRAMES)
    }

    /// Where the last track ends.
    pub fn last_end(&self, total_frames: usize) -> usize {
        (self.music_end + RUN_OUT_TAIL_FRAMES).min(total_frames)
    }
}

/// Maximal runs `[start, end)` where `v[i] == want`.
fn runs(v: &[bool], want: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if v[i] == want {
            let s = i;
            while i < v.len() && v[i] == want {
                i += 1;
            }
            out.push((s, i));
        } else {
            i += 1;
        }
    }
    out
}

/// Frames at or below this are digital silence (exact zeros), not surface noise.
pub const DIGITAL_SILENCE_DB: f32 = -100.0;

fn percentile(db: &[f32], p: f64) -> f32 {
    let mut s = db.to_vec();
    s.sort_by(f32::total_cmp);
    s[((s.len() - 1) as f64 * p).round() as usize]
}

/// The record's noise floor. Digital silence (e.g. zeros before the needle drops) is left out, or a
/// few percent of it would put the floor at -120 dB and every bit of hiss would count as music.
fn noise_floor(db: &[f32]) -> f32 {
    let audible: Vec<f32> = db.iter().copied().filter(|&d| d > DIGITAL_SILENCE_DB).collect();
    percentile(if audible.is_empty() { db } else { &audible }, FLOOR_PERCENTILE)
}

pub fn analyse(db: &[f32]) -> Analysis {
    if db.is_empty() {
        return Analysis { threshold: 0.0, music_start: 0, music_end: 0, gaps: vec![] };
    }
    let threshold = noise_floor(db) + THRESHOLD_MARGIN_DB;
    let mut quiet: Vec<bool> = db.iter().map(|&d| d < threshold).collect();
    // Clicks and pops: short loud blips with quiet on both sides count as quiet.
    for (s, e) in runs(&quiet, false) {
        if e - s <= MAX_BLIP_FRAMES && s > 0 && e < quiet.len() {
            quiet[s..e].fill(true);
        }
    }
    let music: Vec<(usize, usize)> = runs(&quiet, false).into_iter().filter(|(s, e)| e - s >= MIN_MUSIC_FRAMES).collect();
    let (Some(first), Some(last)) = (music.first(), music.last()) else {
        return Analysis { threshold, music_start: 0, music_end: db.len(), gaps: vec![] };
    };
    let (music_start, music_end) = (first.0, last.1);
    let gaps = runs(&quiet, true)
        .into_iter()
        .filter(|&(s, e)| s >= music_start && e <= music_end && e - s >= MIN_GAP_FRAMES)
        .map(|(start, end)| Gap { start, end })
        .collect();
    Analysis { threshold, music_start, music_end, gaps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{db_frames, HISS_DB as H};

    #[test]
    fn finds_the_gaps_between_songs() {
        let db = db_frames(&[(3.0, H), (30.0, -15.0), (2.0, H), (40.0, -15.0), (2.5, H), (20.0, -15.0), (4.0, H)]);
        let a = analyse(&db);
        assert_eq!((a.music_start, a.music_end), (60, 1950));
        assert_eq!(a.gaps, vec![Gap { start: 660, end: 700 }, Gap { start: 1500, end: 1550 }]);
        assert_eq!(a.gaps[0].cut(), 694);
        assert_eq!(a.first_start(), 50);
        assert_eq!(a.last_end(db.len()), 1970);
    }

    #[test]
    fn a_click_does_not_split_a_gap() {
        let db = db_frames(&[(3.0, H), (30.0, -15.0), (1.0, H), (0.1, -10.0), (1.0, H), (30.0, -15.0), (3.0, H)]);
        assert_eq!(analyse(&db).gaps, vec![Gap { start: 660, end: 702 }]);
    }

    #[test]
    fn a_pause_under_a_second_is_not_a_gap() {
        let db = db_frames(&[(3.0, H), (30.0, -15.0), (0.8, H), (30.0, -15.0), (3.0, H)]);
        assert!(analyse(&db).gaps.is_empty());
    }

    #[test]
    fn a_quiet_passage_above_the_hiss_is_not_a_gap() {
        let db = db_frames(&[(3.0, H), (30.0, -15.0), (3.0, -40.0), (30.0, -15.0), (3.0, H)]);
        let a = analyse(&db);
        assert!(a.gaps.is_empty());
        assert_eq!((a.music_start, a.music_end), (60, 1320));
    }

    #[test]
    fn a_needle_drop_thump_is_not_where_the_music_starts() {
        let db = db_frames(&[(5.0, H), (0.5, -20.0), (3.0, H), (30.0, -15.0), (3.0, H)]);
        let a = analyse(&db);
        assert_eq!(a.music_start, 170);
        assert!(a.gaps.is_empty());
    }

    #[test]
    fn digital_silence_does_not_drag_the_floor_down() {
        // A rip that starts with 5% digital silence (exact zeros, -120 dB) before the needle drops.
        let db = db_frames(&[(5.5, -120.0), (3.0, H), (30.0, -15.0), (2.0, H), (40.0, -15.0), (2.5, H), (20.0, -15.0), (4.0, H)]);
        let a = analyse(&db);
        assert_eq!(a.gaps, vec![Gap { start: 770, end: 810 }, Gap { start: 1610, end: 1660 }]);
        assert_eq!((a.music_start, a.music_end), (170, 2060));
    }

    #[test]
    fn all_digital_silence_has_no_gaps() {
        let a = analyse(&db_frames(&[(10.0, -120.0)]));
        assert!(a.gaps.is_empty());
        assert_eq!((a.music_start, a.music_end), (0, 200));
    }

    #[test]
    fn all_hiss_has_no_gaps() {
        let db = db_frames(&[(10.0, H)]);
        let a = analyse(&db);
        assert!(a.gaps.is_empty());
        assert_eq!((a.music_start, a.music_end), (0, 200));
    }
}
