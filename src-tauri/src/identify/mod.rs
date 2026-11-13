pub mod acoustid;
pub mod fingerprint;
pub mod musicbrainz;
pub mod net;
pub mod score;

use crate::model::{ReleaseCandidate, SourceTrack};
use anyhow::Result;
use score::{decide, score, AlbumEvidence, Decision};

pub const MAX_CANDIDATES: usize = 5;

pub trait MetadataSource: Send + Sync {
    /// Release MBIDs for a tag search, best first.
    fn search(&self, album: &str, artist: &str, track_count: usize) -> Result<Vec<String>>;
    /// Release MBIDs for a free-text search typed by the user.
    fn search_text(&self, query: &str) -> Result<Vec<String>>;
    /// Release MBIDs found by audio fingerprint.
    fn by_fingerprint(&self, track: &SourceTrack) -> Result<Vec<String>>;
    fn release(&self, mbid: &str) -> Result<ReleaseCandidate>;
    fn cover(&self, mbid: &str) -> Result<Option<Vec<u8>>>;
}

#[derive(Debug, Clone)]
pub enum Identification {
    Matched(ReleaseCandidate),
    Ambiguous(Vec<ReleaseCandidate>),
    NotFound,
}

pub fn rank(tracks: &[SourceTrack], candidates: Vec<ReleaseCandidate>) -> Vec<ReleaseCandidate> {
    let ev = AlbumEvidence::from_tracks(tracks);
    let mut scored: Vec<(f64, ReleaseCandidate)> = candidates.into_iter().map(|c| (score(&ev, &c), c)).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().map(|(_, c)| c).collect()
}

/// Looks the album up (tag search, else fingerprints) and returns up to MAX_CANDIDATES releases, best first.
/// Empty when nothing was found; Err when nothing was found because lookups failed.
pub fn rank_candidates(tracks: &[SourceTrack], source: &dyn MetadataSource) -> Result<Vec<ReleaseCandidate>> {
    let ev = AlbumEvidence::from_tracks(tracks);
    let mut last_err: Option<anyhow::Error> = None;
    let mut ids: Vec<String> = Vec::new();
    // Once the music database proves unreachable, later lookups would only wait out their own
    // retries: give up at once so an offline import takes one retry cycle.
    if let (Some(album), Some(artist)) = (&ev.album, &ev.artist) {
        match source.search(album, artist, tracks.len()) {
            Ok(found) => ids = found,
            Err(e) if net::is_network_failure(&e) => return Err(e),
            Err(e) => last_err = Some(e),
        }
    }
    if ids.is_empty() {
        // Rank by how many fingerprinted tracks agree on a release (stable: ties keep first-seen order).
        let mut counts: Vec<(String, usize)> = Vec::new();
        for t in tracks.iter().take(3) {
            let found = match source.by_fingerprint(t) {
                Ok(found) => found,
                Err(e) if net::is_network_failure(&e) => return Err(e),
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };
            let mut seen: Vec<String> = Vec::new();
            for id in found {
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id.clone());
                match counts.iter_mut().find(|(k, _)| *k == id) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((id, 1)),
                }
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        ids = counts.into_iter().map(|(id, _)| id).collect();
    }
    ids.truncate(MAX_CANDIDATES);
    let mut candidates = Vec::new();
    for id in &ids {
        match source.release(id) {
            Ok(r) => candidates.push(r),
            Err(e) if net::is_network_failure(&e) => return Err(e),
            Err(e) => last_err = Some(e),
        }
    }
    if candidates.is_empty() {
        return match last_err {
            Some(e) => Err(e),
            None => Ok(vec![]),
        };
    }
    Ok(rank(tracks, candidates))
}

pub fn identify(tracks: &[SourceTrack], source: &dyn MetadataSource) -> Result<Identification> {
    let ranked = rank_candidates(tracks, source)?;
    if ranked.is_empty() {
        return Ok(Identification::NotFound);
    }
    let ev = AlbumEvidence::from_tracks(tracks);
    let scores: Vec<f64> = ranked.iter().map(|c| score(&ev, c)).collect();
    Ok(match decide(&ranked, &scores) {
        Decision::Auto(i) => Identification::Matched(ranked[i].clone()),
        Decision::Ask => Identification::Ambiguous(ranked),
    })
}

pub struct OnlineSource {
    mb: musicbrainz::MusicBrainz,
    acoustid: Option<acoustid::AcoustId>,
    agent: ureq::Agent,
}

impl OnlineSource {
    pub fn new(acoustid_key: Option<String>) -> Self {
        Self {
            mb: musicbrainz::MusicBrainz::new(),
            acoustid: acoustid_key.filter(|k| !k.is_empty()).map(acoustid::AcoustId::new),
            agent: musicbrainz::http_agent(),
        }
    }
}

impl MetadataSource for OnlineSource {
    fn search(&self, album: &str, artist: &str, track_count: usize) -> Result<Vec<String>> {
        let mut hits = self.mb.search_releases(album, artist)?;
        // Stable sort: releases with the right number of tracks first, MB relevance order otherwise.
        hits.sort_by_key(|h| h.track_count as usize != track_count);
        Ok(hits.into_iter().map(|h| h.mbid).collect())
    }

    fn search_text(&self, query: &str) -> Result<Vec<String>> {
        Ok(self.mb.search_text(query)?.into_iter().map(|h| h.mbid).collect())
    }

    fn by_fingerprint(&self, track: &SourceTrack) -> Result<Vec<String>> {
        let Some(client) = &self.acoustid else { return Ok(vec![]) };
        // A file we can't decode (e.g. Opus) just has no fingerprint matches; it is not a lookup failure.
        let fp = match fingerprint::fingerprint_file(&track.path) {
            Ok(fp) => fp,
            Err(e) => {
                eprintln!("no fingerprint for {}: {}", ascii(&track.path.display().to_string()), ascii(&format!("{e:#}")));
                return Ok(vec![]);
            }
        };
        client.lookup(&fp, (track.duration_ms / 1000) as u32)
    }

    fn release(&self, mbid: &str) -> Result<ReleaseCandidate> {
        self.mb.release(mbid)
    }

    fn cover(&self, mbid: &str) -> Result<Option<Vec<u8>>> {
        cover_with_fallback(
            || self.image(&cover_url(mbid)),
            || self.mb.release_group_id(mbid),
            |rg| self.image(&release_group_cover_url(rg)),
        )
    }
}

impl OnlineSource {
    /// A Cover Art Archive image, or `None` when it has none (404).
    fn image(&self, url: &str) -> Result<Option<Vec<u8>>> {
        match net::with_retry(|| self.agent.get(url).header("User-Agent", net::user_agent()).call(), std::thread::sleep) {
            Ok(mut resp) => Ok(Some(resp.body_mut().read_to_vec()?)),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// The release's own front cover; if it has none (common for less-documented editions), the
/// front cover chosen for its release group, i.e. the same album in any edition.
fn cover_with_fallback(
    release_cover: impl FnOnce() -> Result<Option<Vec<u8>>>,
    release_group_id: impl FnOnce() -> Result<Option<String>>,
    group_cover: impl FnOnce(&str) -> Result<Option<Vec<u8>>>,
) -> Result<Option<Vec<u8>>> {
    if let Some(bytes) = release_cover()? {
        return Ok(Some(bytes));
    }
    match release_group_id()? {
        Some(rg) => group_cover(&rg),
        None => Ok(None),
    }
}

/// Console-safe copy of `s`: non-ASCII characters become '?'.
fn ascii(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii() { c } else { '?' }).collect()
}

/// Cover Art Archive front image. The 1200 px thumbnail (not 500) so `prepare_cover` can produce a
/// full 600 px cover; it downscales anything larger.
fn cover_url(mbid: &str) -> String {
    format!("https://coverartarchive.org/release/{mbid}/front-1200")
}

fn release_group_cover_url(release_group: &str) -> String {
    format!("https://coverartarchive.org/release-group/{release_group}/front-1200")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{candidate, FakeSource};

    fn src(title: Option<&str>, album: Option<&str>, artist: Option<&str>, ms: u64) -> SourceTrack {
        SourceTrack {
            path: "x.wav".into(),
            title: title.map(String::from),
            artist: artist.map(String::from),
            album: album.map(String::from),
            album_artist: None,
            track_no: None,
            disc_no: None,
            year: None,
            genre: None,
            duration_ms: ms,
        }
    }

    const TL: [(&str, u64); 2] = [("One", 100_000), ("Two", 200_000)];

    /// Two tagged tracks and release "a", which matches them exactly.
    fn matching_tracks_and_release() -> ([SourceTrack; 2], ReleaseCandidate) {
        (
            [src(Some("One"), Some("Album"), Some("Band"), 100_000), src(Some("Two"), Some("Album"), Some("Band"), 200_000)],
            candidate("a", "Album", "Band", &TL),
        )
    }

    #[test]
    fn matches_a_clear_winner() {
        let (tracks, release) = matching_tracks_and_release();
        let fake = FakeSource { releases: vec![release], search_ids: vec!["a".into()] };
        match identify(&tracks, &fake).unwrap() {
            Identification::Matched(r) => assert_eq!(r.mbid, "a"),
            other => panic!("expected match, got {other:?}"),
        }
    }

    /// FakeSource whose text search, fingerprint lookups and/or release fetches fail.
    struct Failing {
        inner: FakeSource,
        search_fails: bool,
        fingerprint_ids: Vec<String>,
        fingerprint_fails: bool,
        release_fails: bool,
    }

    impl MetadataSource for Failing {
        fn search(&self, a: &str, b: &str, n: usize) -> Result<Vec<String>> {
            if self.search_fails { anyhow::bail!("search offline") } else { self.inner.search(a, b, n) }
        }
        fn search_text(&self, q: &str) -> Result<Vec<String>> {
            self.inner.search_text(q)
        }
        fn by_fingerprint(&self, _t: &SourceTrack) -> Result<Vec<String>> {
            if self.fingerprint_fails { anyhow::bail!("acoustid offline") } else { Ok(self.fingerprint_ids.clone()) }
        }
        fn release(&self, id: &str) -> Result<ReleaseCandidate> {
            if self.release_fails { anyhow::bail!("release offline") } else { self.inner.release(id) }
        }
        fn cover(&self, m: &str) -> Result<Option<Vec<u8>>> {
            self.inner.cover(m)
        }
    }

    #[test]
    fn a_failing_search_falls_back_to_fingerprints() {
        let (tracks, release_a) = matching_tracks_and_release();
        let src = Failing {
            inner: FakeSource { releases: vec![release_a], search_ids: vec![] },
            search_fails: true,
            fingerprint_ids: vec!["a".into()],
            fingerprint_fails: false,
            release_fails: false,
        };
        assert!(matches!(identify(&tracks, &src).unwrap(), Identification::Matched(_) | Identification::Ambiguous(_)));
    }

    #[test]
    fn nothing_found_because_lookups_failed_is_an_error_not_not_found() {
        let (tracks, release_a) = matching_tracks_and_release();
        let src = Failing {
            inner: FakeSource { releases: vec![release_a], search_ids: vec!["a".into()] },
            search_fails: false,
            fingerprint_ids: vec![],
            fingerprint_fails: false,
            release_fails: true,
        };
        let err = identify(&tracks, &src).unwrap_err();
        assert!(format!("{err:#}").contains("release offline"));
    }

    #[test]
    fn nothing_found_without_errors_is_not_found() {
        let (tracks, _) = matching_tracks_and_release();
        let src = Failing {
            inner: FakeSource { releases: vec![], search_ids: vec![] },
            search_fails: false,
            fingerprint_ids: vec![],
            fingerprint_fails: false,
            release_fails: false,
        };
        assert!(matches!(identify(&tracks, &src).unwrap(), Identification::NotFound));
    }

    #[test]
    fn ambiguous_returns_candidates_best_first() {
        let other = [("Uno", 100_500), ("Dos", 199_000)];
        let fake = FakeSource {
            releases: vec![candidate("b", "Album", "Band", &other), candidate("a", "Album", "Band", &TL)],
            search_ids: vec!["b".into(), "a".into()],
        };
        let tracks = [src(None, Some("Album"), Some("Band"), 100_000), src(None, Some("Album"), Some("Band"), 200_000)];
        match identify(&tracks, &fake).unwrap() {
            Identification::Ambiguous(c) => {
                assert_eq!(c.len(), 2);
                assert_eq!(c[0].mbid, "a", "exact-duration release must rank first");
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    struct PerTrackFp {
        inner: FakeSource,
        by_path: Vec<(String, Vec<String>)>,
    }

    impl MetadataSource for PerTrackFp {
        fn search(&self, a: &str, b: &str, n: usize) -> Result<Vec<String>> {
            self.inner.search(a, b, n)
        }
        fn search_text(&self, q: &str) -> Result<Vec<String>> {
            self.inner.search_text(q)
        }
        fn by_fingerprint(&self, t: &SourceTrack) -> Result<Vec<String>> {
            let p = t.path.to_string_lossy().to_string();
            Ok(self.by_path.iter().find(|(k, _)| *k == p).map(|(_, v)| v.clone()).unwrap_or_default())
        }
        fn release(&self, m: &str) -> Result<ReleaseCandidate> {
            self.inner.release(m)
        }
        fn cover(&self, m: &str) -> Result<Option<Vec<u8>>> {
            self.inner.cover(m)
        }
    }

    #[test]
    fn fingerprint_release_shared_by_all_tracks_beats_one_tracks_many_releases() {
        let tl3 = [("One", 100_000), ("Two", 200_000), ("Three", 150_000)];
        let mut releases = vec![candidate("shared", "Album", "Band", &tl3)];
        let mut noisy: Vec<String> = Vec::new();
        for i in 0..MAX_CANDIDATES {
            let id = format!("noise{i}");
            releases.push(candidate(&id, "Other", "Other", &[("Zzz", 1_000)]));
            noisy.push(id);
        }
        let mut first = noisy.clone();
        first.push("shared".into());
        let fake = PerTrackFp {
            inner: FakeSource { releases, search_ids: vec![] },
            by_path: vec![
                ("t0.wav".into(), first),
                ("t1.wav".into(), vec!["shared".into()]),
                ("t2.wav".into(), vec!["shared".into()]),
            ],
        };
        let mk = |p: &str, t: &str, ms| {
            let mut s = src(Some(t), None, None, ms);
            s.path = p.into();
            s
        };
        let tracks = [mk("t0.wav", "One", 100_000), mk("t1.wav", "Two", 200_000), mk("t2.wav", "Three", 150_000)];
        match identify(&tracks, &fake).unwrap() {
            Identification::Matched(r) => assert_eq!(r.mbid, "shared"),
            Identification::Ambiguous(c) => assert_eq!(c[0].mbid, "shared"),
            Identification::NotFound => panic!("shared release was crowded out"),
        }
    }

    #[test]
    fn nothing_found_without_tags_or_fingerprints() {
        let fake = FakeSource { releases: vec![], search_ids: vec![] };
        let tracks = [src(None, None, None, 100_000)];
        assert!(matches!(identify(&tracks, &fake).unwrap(), Identification::NotFound));
    }

    #[test]
    fn rank_candidates_returns_the_ranked_list_even_for_a_confident_match() {
        let other = [("Uno", 100_500), ("Dos", 199_000)];
        let tracks = [src(None, Some("Album"), Some("Band"), 100_000), src(None, Some("Album"), Some("Band"), 200_000)];
        let fake = FakeSource {
            releases: vec![candidate("b", "Album", "Band", &other), candidate("a", "Album", "Band", &TL)],
            search_ids: vec!["b".into(), "a".into()],
        };
        let ranked = rank_candidates(&tracks, &fake).unwrap();
        assert_eq!(ranked.iter().map(|c| c.mbid.as_str()).collect::<Vec<_>>(), ["a", "b"]);

        let (tracks, release) = matching_tracks_and_release();
        let fake = FakeSource { releases: vec![release], search_ids: vec!["a".into()] };
        assert!(matches!(identify(&tracks, &fake).unwrap(), Identification::Matched(_)));
        let ranked = rank_candidates(&tracks, &fake).unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].mbid, "a");
    }

    #[test]
    fn rank_candidates_is_empty_when_nothing_is_found_and_errs_when_lookups_failed() {
        let (tracks, release_a) = matching_tracks_and_release();
        let none = Failing {
            inner: FakeSource { releases: vec![], search_ids: vec![] },
            search_fails: false,
            fingerprint_ids: vec![],
            fingerprint_fails: false,
            release_fails: false,
        };
        assert!(rank_candidates(&tracks, &none).unwrap().is_empty());
        let failed = Failing {
            inner: FakeSource { releases: vec![release_a], search_ids: vec!["a".into()] },
            search_fails: false,
            fingerprint_ids: vec![],
            fingerprint_fails: false,
            release_fails: true,
        };
        let err = rank_candidates(&tracks, &failed).unwrap_err();
        assert!(format!("{err:#}").contains("release offline"));
    }

    #[test]
    fn rank_sorts_by_score() {
        let tracks = [src(Some("One"), None, None, 100_000), src(Some("Two"), None, None, 200_000)];
        let ranked = rank(&tracks, vec![candidate("bad", "X", "Y", &[("Q", 5_000)]), candidate("good", "X", "Y", &TL)]);
        assert_eq!(ranked[0].mbid, "good");
    }

    #[test]
    #[ignore]
    fn live_musicbrainz_lookup() {
        let src = OnlineSource::new(None);
        let ids = src.search("OK Computer", "Radiohead", 12).unwrap();
        assert!(!ids.is_empty());
        let r = src.release(&ids[0]).unwrap();
        assert_eq!(r.title, "OK Computer");
        assert!(src.cover(&r.mbid).is_ok());
    }

    #[test]
    fn cover_url_asks_for_an_image_at_least_600_px() {
        assert_eq!(cover_url("abc"), "https://coverartarchive.org/release/abc/front-1200");
        assert_eq!(release_group_cover_url("rg"), "https://coverartarchive.org/release-group/rg/front-1200");
    }

    /// Live: Bleed American (2001, XE) has no art of its own on the Cover Art Archive; its
    /// release group does. Run with `cargo test -- --ignored live_cover`.
    #[test]
    #[ignore]
    fn live_cover_falls_back_to_release_group() {
        let bytes = OnlineSource::new(None).cover("2adfeecf-91e2-4821-ac14-d58aef312659").unwrap().expect("a cover");
        assert!(crate::artwork::prepare_cover(&bytes).is_ok());
    }

    #[test]
    fn a_release_without_art_falls_back_to_its_release_groups_cover() {
        let asked = std::cell::RefCell::new(Vec::new());
        let got = cover_with_fallback(
            || Ok(None),
            || Ok(Some("rg-1".to_string())),
            |rg| {
                asked.borrow_mut().push(rg.to_string());
                Ok(Some(vec![1, 2, 3]))
            },
        )
        .unwrap();
        assert_eq!(got, Some(vec![1, 2, 3]));
        assert_eq!(*asked.borrow(), vec!["rg-1".to_string()]);
    }

    #[test]
    fn the_releases_own_cover_wins_and_no_group_lookup_happens() {
        let got = cover_with_fallback(
            || Ok(Some(vec![9])),
            || panic!("must not look up the release group"),
            |_| panic!("must not fetch the group cover"),
        )
        .unwrap();
        assert_eq!(got, Some(vec![9]));
    }

    #[test]
    fn no_art_anywhere_is_none_and_lookup_errors_are_reported() {
        assert_eq!(cover_with_fallback(|| Ok(None), || Ok(None), |_| panic!("no group")).unwrap(), None);
        assert_eq!(cover_with_fallback(|| Ok(None), || Ok(Some("rg".into())), |_| Ok(None)).unwrap(), None);
        assert!(cover_with_fallback(|| Ok(None), || anyhow::bail!("offline"), |_| Ok(None)).is_err());
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use crate::test_support::{candidate, FakeSource};
    use std::sync::Mutex;

    #[derive(Clone, Copy, PartialEq)]
    enum Fail {
        No,
        Net,
        Other,
    }

    fn err(kind: Fail, what: &str) -> anyhow::Error {
        match kind {
            Fail::Net => anyhow::Error::from(ureq::Error::ConnectionFailed).context(format!("{what} lookup")),
            _ => anyhow::anyhow!("{what} broke"),
        }
    }

    /// Fake source that counts calls per lookup kind and fails the chosen ones.
    struct Counting {
        inner: FakeSource,
        search: Fail,
        fingerprint: Fail,
        fingerprint_ids: Vec<String>,
        release: Fail,
        calls: Mutex<[usize; 3]>,
    }

    impl Counting {
        fn new(inner: FakeSource) -> Self {
            Counting { inner, search: Fail::No, fingerprint: Fail::No, fingerprint_ids: vec![], release: Fail::No, calls: Mutex::new([0; 3]) }
        }
        fn calls(&self) -> [usize; 3] {
            *self.calls.lock().unwrap()
        }
    }

    impl MetadataSource for Counting {
        fn search(&self, a: &str, b: &str, n: usize) -> Result<Vec<String>> {
            self.calls.lock().unwrap()[0] += 1;
            if self.search != Fail::No { return Err(err(self.search, "search")) }
            self.inner.search(a, b, n)
        }
        fn search_text(&self, q: &str) -> Result<Vec<String>> {
            self.inner.search_text(q)
        }
        fn by_fingerprint(&self, _t: &SourceTrack) -> Result<Vec<String>> {
            self.calls.lock().unwrap()[1] += 1;
            if self.fingerprint != Fail::No { return Err(err(self.fingerprint, "fingerprint")) }
            Ok(self.fingerprint_ids.clone())
        }
        fn release(&self, id: &str) -> Result<ReleaseCandidate> {
            self.calls.lock().unwrap()[2] += 1;
            if self.release != Fail::No { return Err(err(self.release, "release")) }
            self.inner.release(id)
        }
        fn cover(&self, m: &str) -> Result<Option<Vec<u8>>> {
            self.inner.cover(m)
        }
    }

    fn track(path: &str, tagged: bool) -> SourceTrack {
        SourceTrack {
            path: path.into(),
            title: Some("One".into()),
            artist: tagged.then(|| "Band".into()),
            album: tagged.then(|| "Album".into()),
            album_artist: None,
            track_no: None,
            disc_no: None,
            year: None,
            genre: None,
            duration_ms: 100_000,
        }
    }

    fn three(tagged: bool) -> Vec<SourceTrack> {
        vec![track("a.wav", tagged), track("b.wav", tagged), track("c.wav", tagged)]
    }

    #[test]
    fn a_network_failure_on_search_stops_at_once() {
        let mut s = Counting::new(FakeSource { releases: vec![], search_ids: vec![] });
        s.search = Fail::Net;
        s.fingerprint_ids = vec!["a".into()];
        let e = identify(&three(true), &s).unwrap_err();
        assert!(net::is_network_failure(&e), "{e:#}");
        assert_eq!(s.calls(), [1, 0, 0], "an offline import must not go on to fingerprint and release lookups");
    }

    #[test]
    fn a_network_failure_on_fingerprints_is_tried_once() {
        let mut s = Counting::new(FakeSource { releases: vec![], search_ids: vec![] });
        s.fingerprint = Fail::Net;
        let e = identify(&three(false), &s).unwrap_err();
        assert!(net::is_network_failure(&e), "{e:#}");
        assert_eq!(s.calls(), [0, 1, 0]);
    }

    #[test]
    fn a_network_failure_on_releases_is_tried_once() {
        let tl = [("One", 100_000)];
        let releases = vec![candidate("a", "Album", "Band", &tl), candidate("b", "Album", "Band", &tl)];
        let mut s = Counting::new(FakeSource { releases, search_ids: vec!["a".into(), "b".into()] });
        s.release = Fail::Net;
        let e = identify(&three(true), &s).unwrap_err();
        assert!(net::is_network_failure(&e), "{e:#}");
        assert_eq!(s.calls(), [1, 0, 1]);
    }

    #[test]
    fn other_lookup_failures_still_try_the_rest() {
        let mut s = Counting::new(FakeSource { releases: vec![], search_ids: vec![] });
        s.search = Fail::Other;
        s.fingerprint = Fail::Other;
        let e = identify(&three(true), &s).unwrap_err();
        assert!(!net::is_network_failure(&e), "{e:#}");
        assert_eq!(s.calls(), [1, 3, 0]);
    }

    #[test]
    fn an_undecodable_file_has_no_fingerprint_matches_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.opus");
        std::fs::write(&path, b"definitely not audio").unwrap();
        let src = OnlineSource::new(Some("key".into()));
        let found = src.by_fingerprint(&track(path.to_str().unwrap(), false)).unwrap();
        assert!(found.is_empty());
    }
}
