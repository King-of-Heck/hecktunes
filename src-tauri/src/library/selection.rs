use super::Library;
use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionKind {
    Album,
    Artist,
    Playlist,
}

impl SelectionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

/// What is ticked for the HeckPod: albums and playlists by id, artists by album-artist name.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Selection {
    pub albums: Vec<i64>,
    pub artists: Vec<String>,
    pub playlists: Vec<i64>,
}

impl Library {
    pub fn selection(&self) -> Result<Selection> {
        let mut s = Selection::default();
        let mut stmt = self.conn.prepare("SELECT kind, ref FROM device_selection ORDER BY kind, ref")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (kind, reference) = row?;
            match kind.as_str() {
                "album" => s.albums.extend(reference.parse::<i64>().ok()),
                "playlist" => s.playlists.extend(reference.parse::<i64>().ok()),
                _ => s.artists.push(reference),
            }
        }
        s.albums.sort_unstable();
        s.playlists.sort_unstable();
        Ok(s)
    }

    pub fn set_selected(&mut self, kind: SelectionKind, reference: &str, on: bool) -> Result<()> {
        if on {
            self.conn.execute(
                "INSERT OR IGNORE INTO device_selection (kind, ref) VALUES (?1, ?2)",
                params![kind.as_str(), reference],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM device_selection WHERE kind = ?1 AND ref = ?2",
                params![kind.as_str(), reference],
            )?;
        }
        Ok(())
    }

    /// Before album `from` is replaced by `to` (Fix metadata / Replace), moves its playlist lines and
    /// its HeckPod tick to the new album. Tracks pair up by (disc, number), else by position.
    pub fn transfer_album_refs(&mut self, from: i64, to: i64) -> Result<()> {
        let old = self.album_tracks(from)?;
        let new = self.album_tracks(to)?;
        let tx = self.conn.transaction()?;
        for (i, o) in old.iter().enumerate() {
            let target = new.iter().find(|n| n.disc == o.disc && n.number == o.number).or_else(|| new.get(i));
            if let Some(n) = target {
                tx.execute("UPDATE playlist_items SET track_id = ?1 WHERE track_id = ?2", params![n.id, o.id])?;
            }
        }
        tx.execute(
            "UPDATE OR IGNORE device_selection SET ref = ?1 WHERE kind = 'album' AND ref = ?2",
            params![to.to_string(), from.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::add_memory_album;
    use std::path::PathBuf;

    fn lib() -> Library {
        Library::open_in_memory(PathBuf::from("/lib")).unwrap()
    }

    #[test]
    fn ticks_round_trip_and_are_idempotent() {
        let mut l = lib();
        l.set_selected(SelectionKind::Album, "12", true).unwrap();
        l.set_selected(SelectionKind::Album, "12", true).unwrap();
        l.set_selected(SelectionKind::Artist, "Band", true).unwrap();
        l.set_selected(SelectionKind::Playlist, "3", true).unwrap();
        assert_eq!(l.selection().unwrap(), Selection { albums: vec![12], artists: vec!["Band".into()], playlists: vec![3] });
        l.set_selected(SelectionKind::Album, "12", false).unwrap();
        l.set_selected(SelectionKind::Album, "12", false).unwrap();
        assert!(l.selection().unwrap().albums.is_empty());
    }

    #[test]
    fn selection_kind_uses_snake_case_names() {
        assert_eq!(serde_json::from_str::<SelectionKind>("\"playlist\"").unwrap(), SelectionKind::Playlist);
        assert_eq!(serde_json::to_string(&SelectionKind::Album).unwrap(), "\"album\"");
    }

    #[test]
    fn transfer_moves_playlist_entries_and_tick_to_the_new_album() {
        let mut l = lib();
        let (old, t) = add_memory_album(&mut l, "Alb", "Band", 2);
        let (new, u) = add_memory_album(&mut l, "Alb (fixed)", "Band", 2);
        let p = l.create_playlist("Mix").unwrap();
        l.add_to_playlist(p, &[t[1], t[0]]).unwrap();
        l.set_selected(SelectionKind::Album, &old.to_string(), true).unwrap();
        l.transfer_album_refs(old, new).unwrap();
        l.delete_album(old, false).unwrap();
        let ids: Vec<i64> = l.playlist_entries(p).unwrap().iter().map(|e| e.song.track.id).collect();
        assert_eq!(ids, vec![u[1], u[0]]);
        assert_eq!(l.selection().unwrap().albums, vec![new]);
    }
}
