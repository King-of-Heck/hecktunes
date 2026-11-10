use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What we could read from a file the user dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceTrack {
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_no: Option<u32>,
    pub disc_no: Option<u32>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub duration_ms: u64,
}

/// The full tag set we write into a library copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track_no: u32,
    pub track_total: u32,
    pub disc_no: u32,
    pub disc_total: u32,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub mb_release_id: Option<String>,
    pub mb_recording_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateTrack {
    pub disc: u32,
    pub number: u32,
    pub title: String,
    pub artist: String,
    pub length_ms: Option<u64>,
    pub recording_id: String,
}

/// A MusicBrainz release with its full track list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseCandidate {
    pub mbid: String,
    pub title: String,
    pub artist: String,
    pub date: Option<String>,
    pub country: Option<String>,
    pub format: Option<String>,
    pub genre: Option<String>,
    pub disc_count: u32,
    pub tracks: Vec<CandidateTrack>,
}

impl ReleaseCandidate {
    pub fn year(&self) -> Option<String> {
        self.date.as_deref().and_then(|d| d.get(0..4)).map(str::to_string)
    }
}
