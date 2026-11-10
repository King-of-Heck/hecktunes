use crate::model::{CandidateTrack, ReleaseCandidate};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

pub const USER_AGENT: &str = concat!("HeckTunes/", env!("CARGO_PKG_VERSION"), " ( hecktunes-app )");
const BASE: &str = "https://musicbrainz.org/ws/2";

/// Shared HTTP agent config: 30 s overall timeout so a stuck request can't hang the import worker.
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into()
}

pub struct RateLimiter {
    min_gap: Duration,
    last: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(min_gap: Duration) -> Self {
        Self { min_gap, last: Mutex::new(None) }
    }

    pub fn wait(&self) {
        let mut last = self.last.lock().unwrap();
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < self.min_gap {
                thread::sleep(self.min_gap - elapsed);
            }
        }
        *last = Some(Instant::now());
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub mbid: String,
    pub score: u32,
    pub track_count: u32,
}

pub fn lucene_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "+-&|!(){}[]^\"~*?:\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn join_credit(v: &Value) -> String {
    v.as_array()
        .map(|parts| {
            parts
                .iter()
                .map(|p| format!("{}{}", p["name"].as_str().unwrap_or(""), p["joinphrase"].as_str().unwrap_or("")))
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().map(str::to_string).filter(|s| !s.is_empty())
}

pub fn parse_search(v: &Value) -> Vec<SearchHit> {
    v["releases"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| {
                    Some(SearchHit {
                        mbid: r["id"].as_str()?.to_string(),
                        score: r["score"].as_u64().unwrap_or(0) as u32,
                        track_count: r["track-count"].as_u64().unwrap_or(0) as u32,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn top_genre<'a>(genres: impl Iterator<Item = &'a Value>) -> Option<String> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for g in genres {
        if let Some(name) = g["name"].as_str() {
            *counts.entry(name.to_string()).or_default() += g["count"].as_u64().unwrap_or(1);
        }
    }
    counts.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|(n, _)| n)
}

pub fn parse_release(v: &Value) -> Result<ReleaseCandidate> {
    let mbid = str_field(v, "id").ok_or_else(|| anyhow!("release without id"))?;
    let artist = join_credit(&v["artist-credit"]);
    let media = v["media"].as_array().cloned().unwrap_or_default();
    let mut tracks = Vec::new();
    for (mi, medium) in media.iter().enumerate() {
        let disc = medium["position"].as_u64().map(|p| p as u32).unwrap_or(mi as u32 + 1);
        for (ti, t) in medium["tracks"].as_array().into_iter().flatten().enumerate() {
            let credit = join_credit(&t["artist-credit"]);
            tracks.push(CandidateTrack {
                disc,
                number: t["position"].as_u64().map(|p| p as u32).unwrap_or(ti as u32 + 1),
                title: str_field(t, "title").unwrap_or_default(),
                artist: if credit.is_empty() { artist.clone() } else { credit },
                length_ms: t["length"].as_u64().or_else(|| t["recording"]["length"].as_u64()),
                recording_id: t["recording"]["id"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    let release_genres = v["genres"].as_array().cloned().unwrap_or_default();
    let genre = top_genre(release_genres.iter()).or_else(|| {
        let all: Vec<Value> = media
            .iter()
            .flat_map(|m| m["tracks"].as_array().cloned().unwrap_or_default())
            .flat_map(|t| t["recording"]["genres"].as_array().cloned().unwrap_or_default())
            .collect();
        top_genre(all.iter())
    });
    Ok(ReleaseCandidate {
        mbid,
        title: str_field(v, "title").unwrap_or_default(),
        artist,
        date: str_field(v, "date"),
        country: str_field(v, "country"),
        format: media.first().and_then(|m| str_field(m, "format")),
        genre,
        disc_count: media.len().max(1) as u32,
        tracks,
    })
}

pub struct MusicBrainz {
    agent: ureq::Agent,
    limiter: RateLimiter,
}

impl Default for MusicBrainz {
    fn default() -> Self {
        Self::new()
    }
}

impl MusicBrainz {
    pub fn new() -> Self {
        Self { agent: http_agent(), limiter: RateLimiter::new(Duration::from_millis(1100)) }
    }

    fn get_json(&self, url: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut attempt = 0;
        loop {
            self.limiter.wait();
            let mut req = self.agent.get(url).header("User-Agent", USER_AGENT).header("Accept", "application/json");
            for (k, v) in query {
                req = req.query(*k, *v);
            }
            match req.call() {
                Ok(mut resp) => return Ok(resp.body_mut().read_json::<Value>()?),
                Err(ureq::Error::StatusCode(503)) if attempt < 3 => {
                    attempt += 1;
                    thread::sleep(Duration::from_secs(2 * attempt));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub fn search_releases(&self, album: &str, artist: &str) -> Result<Vec<SearchHit>> {
        let q = format!("release:\"{}\" AND artist:\"{}\"", lucene_escape(album), lucene_escape(artist));
        self.search_text(&q)
    }

    pub fn search_text(&self, query: &str) -> Result<Vec<SearchHit>> {
        let v = self.get_json(&format!("{BASE}/release/"), &[("query", query), ("limit", "15"), ("fmt", "json")])?;
        Ok(parse_search(&v))
    }

    pub fn release(&self, mbid: &str) -> Result<ReleaseCandidate> {
        let v = self.get_json(
            &format!("{BASE}/release/{mbid}"),
            &[("inc", "recordings artist-credits genres"), ("fmt", "json")],
        )?;
        parse_release(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn parses_search_hits() {
        let hits = parse_search(&fixture(include_str!("../../tests/fixtures/mb_search.json")));
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|h| h.mbid.len() == 36 && h.track_count > 0));
    }

    #[test]
    fn parses_release_with_tracks() {
        let r = parse_release(&fixture(include_str!("../../tests/fixtures/mb_release.json"))).unwrap();
        assert_eq!(r.mbid, "0b6b4ba0-d36f-47bd-b4ea-6a5b91842d29");
        assert_eq!(r.title, "OK Computer");
        assert_eq!(r.artist, "Radiohead");
        assert_eq!(r.disc_count, 1);
        assert_eq!(r.tracks.len(), 12);
        assert_eq!(r.tracks[0].title, "Airbag");
        assert_eq!((r.tracks[0].disc, r.tracks[0].number), (1, 1));
        assert!(r.tracks[0].length_ms.is_some());
        assert_eq!(r.year().as_deref(), Some("1997"));
    }

    #[test]
    fn genre_falls_back_to_recording_genres() {
        let v = json!({
            "id": "r1", "title": "T", "artist-credit": [{"name": "A", "joinphrase": ""}],
            "media": [{"position": 1, "format": "CD", "tracks": [
                {"position": 1, "title": "x", "length": 1000, "recording": {"id": "rec", "genres": [{"name": "jazz", "count": 3}, {"name": "pop", "count": 1}]}}
            ]}]
        });
        let r = parse_release(&v).unwrap();
        assert_eq!(r.genre.as_deref(), Some("jazz"));
        assert_eq!(r.tracks[0].artist, "A");
    }

    #[test]
    fn joins_artist_credits() {
        let v = json!([{"name": "Simon", "joinphrase": " & "}, {"name": "Garfunkel", "joinphrase": ""}]);
        assert_eq!(join_credit(&v), "Simon & Garfunkel");
    }

    #[test]
    fn escapes_lucene_specials() {
        assert_eq!(lucene_escape(r#"AC/DC: "Live" (1992)"#), r#"AC\/DC\: \"Live\" \(1992\)"#);
    }

    #[test]
    fn rate_limiter_spaces_calls() {
        let rl = RateLimiter::new(std::time::Duration::from_millis(50));
        let start = std::time::Instant::now();
        rl.wait();
        rl.wait();
        rl.wait();
        assert!(start.elapsed() >= std::time::Duration::from_millis(100));
    }
}
