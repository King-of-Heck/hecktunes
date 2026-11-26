use super::{now_secs, track_from_row, Library, TrackRow};
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, ErrorCode, OptionalExtension};
use serde::Serialize;
use std::path::Path;

/// A track with the album fields the Songs and playlist views show.
#[derive(Debug, Clone, Serialize)]
pub struct SongRow {
    #[serde(flatten)]
    pub track: TrackRow,
    pub album_title: String,
    pub album_artist: String,
    pub year: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistRow {
    pub id: i64,
    pub name: String,
    pub track_count: u32,
    pub duration_ms: u64,
}

/// One line of a playlist. `item_id` identifies the line (a track may appear more than once).
#[derive(Debug, Clone, Serialize)]
pub struct PlaylistEntry {
    pub item_id: i64,
    #[serde(flatten)]
    pub song: SongRow,
}

/// Same column order as `TRACK_COLUMNS` (indices 0..=14), then album fields 15..=17.
const SONG_COLUMNS: &str = "t.id, t.album_id, t.disc, t.number, t.title, t.artist, t.duration_ms, t.format, t.file_path, t.size_bytes, t.mb_recording_id, t.content_hash, t.aac_path, t.aac_size, t.aac_hash, a.title, a.album_artist, a.year";

fn song_from_row(root: &Path, r: &rusqlite::Row) -> rusqlite::Result<SongRow> {
    Ok(SongRow { track: track_from_row(root, r)?, album_title: r.get(15)?, album_artist: r.get(16)?, year: r.get(17)? })
}

fn clean_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        bail!("A playlist needs a name");
    }
    Ok(n.to_string())
}

fn name_error(e: rusqlite::Error, name: &str) -> anyhow::Error {
    match &e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == ErrorCode::ConstraintViolation => {
            anyhow!("A playlist called \"{name}\" already exists")
        }
        _ => e.into(),
    }
}

impl Library {
    pub fn list_songs(&self) -> Result<Vec<SongRow>> {
        let root = self.root.clone();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SONG_COLUMNS} FROM tracks t JOIN albums a ON a.id = t.album_id
             ORDER BY a.album_artist COLLATE NOCASE, a.year, a.title COLLATE NOCASE, t.disc, t.number"
        ))?;
        let rows = stmt.query_map([], |r| song_from_row(&root, r))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_playlist(&mut self, name: &str) -> Result<i64> {
        let name = clean_name(name)?;
        self.conn
            .execute("INSERT INTO playlists (name, created_at) VALUES (?1, ?2)", params![name, now_secs()])
            .map_err(|e| name_error(e, &name))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_playlist(&mut self, id: i64, name: &str) -> Result<()> {
        let name = clean_name(name)?;
        let changed = self
            .conn
            .execute("UPDATE playlists SET name = ?1 WHERE id = ?2", params![name, id])
            .map_err(|e| name_error(e, &name))?;
        if changed == 0 {
            bail!("playlist {id} not found");
        }
        Ok(())
    }

    /// Deletes the playlist (its songs stay in the library) and its Echo Mini tick.
    pub fn delete_playlist(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM device_selection WHERE kind = 'playlist' AND ref = ?1", [id.to_string()])?;
        tx.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_playlists(&self) -> Result<Vec<PlaylistRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, COUNT(t.id), COALESCE(SUM(t.duration_ms), 0)
             FROM playlists p
             LEFT JOIN playlist_items i ON i.playlist_id = p.id
             LEFT JOIN tracks t ON t.id = i.track_id
             GROUP BY p.id ORDER BY p.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PlaylistRow {
                id: r.get(0)?,
                name: r.get(1)?,
                track_count: r.get::<_, i64>(2)? as u32,
                duration_ms: r.get::<_, i64>(3)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn playlist_entries(&self, id: i64) -> Result<Vec<PlaylistEntry>> {
        let root = self.root.clone();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SONG_COLUMNS}, i.id FROM playlist_items i
             JOIN tracks t ON t.id = i.track_id JOIN albums a ON a.id = t.album_id
             WHERE i.playlist_id = ?1 ORDER BY i.position, i.id"
        ))?;
        let rows = stmt.query_map([id], |r| Ok(PlaylistEntry { item_id: r.get(18)?, song: song_from_row(&root, r)? }))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Appends tracks in the given order. All or nothing.
    pub fn add_to_playlist(&mut self, id: i64, track_ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        let exists: Option<i64> = tx.query_row("SELECT id FROM playlists WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        if exists.is_none() {
            bail!("playlist {id} not found");
        }
        let start: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        for (offset, track) in track_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO playlist_items (playlist_id, track_id, position) VALUES (?1, ?2, ?3)",
                params![id, track, start + offset as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_playlist_items(&mut self, id: i64, item_ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for item in item_ids {
            tx.execute("DELETE FROM playlist_items WHERE id = ?1 AND playlist_id = ?2", params![item, id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Moves one line to `to_index` (0-based, clamped to the end) and renumbers the playlist 0..n.
    pub fn move_playlist_item(&mut self, id: i64, item_id: i64, to_index: usize) -> Result<()> {
        let mut order: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, id")?;
            let rows = stmt.query_map([id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let from = order
            .iter()
            .position(|&i| i == item_id)
            .ok_or_else(|| anyhow!("item {item_id} is not in playlist {id}"))?;
        let item = order.remove(from);
        order.insert(to_index.min(order.len()), item);
        let tx = self.conn.transaction()?;
        for (pos, item) in order.iter().enumerate() {
            tx.execute("UPDATE playlist_items SET position = ?1 WHERE id = ?2", params![pos as i64, item])?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::library::{Library, SelectionKind};
    use crate::test_support::add_memory_album;
    use std::path::PathBuf;

    fn lib() -> Library {
        Library::open_in_memory(PathBuf::from("/lib")).unwrap()
    }

    #[test]
    fn create_rename_and_list_playlists() {
        let mut l = lib();
        let road = l.create_playlist("  Road Trip ").unwrap();
        let chill = l.create_playlist("Chill").unwrap();
        let listed: Vec<(i64, String)> = l.list_playlists().unwrap().into_iter().map(|p| (p.id, p.name)).collect();
        assert_eq!(listed, vec![(chill, "Chill".to_string()), (road, "Road Trip".to_string())]);
        l.rename_playlist(chill, "Zzz").unwrap();
        assert_eq!(l.list_playlists().unwrap()[1].name, "Zzz");
    }

    #[test]
    fn names_must_be_unique_ignoring_case_and_not_blank() {
        let mut l = lib();
        let mix = l.create_playlist("Mix").unwrap();
        assert!(l.create_playlist("mix").unwrap_err().to_string().contains("already exists"));
        let other = l.create_playlist("Other").unwrap();
        assert!(l.rename_playlist(other, "MIX").unwrap_err().to_string().contains("already exists"));
        assert!(l.create_playlist("   ").unwrap_err().to_string().contains("needs a name"));
        l.rename_playlist(mix, "Mix").unwrap();
        assert!(l.rename_playlist(9999, "Gone").is_err());
    }

    #[test]
    fn entries_keep_order_allow_repeats_and_can_be_moved_and_removed() {
        let mut l = lib();
        let (_, t) = add_memory_album(&mut l, "Alb", "Band", 3);
        let p = l.create_playlist("Mix").unwrap();
        l.add_to_playlist(p, &[t[2], t[0]]).unwrap();
        l.add_to_playlist(p, &[t[2]]).unwrap();
        let ids = |l: &Library| l.playlist_entries(p).unwrap().iter().map(|e| e.song.track.id).collect::<Vec<_>>();
        assert_eq!(ids(&l), vec![t[2], t[0], t[2]]);

        let first = l.playlist_entries(p).unwrap()[0].item_id;
        l.move_playlist_item(p, first, 99).unwrap();
        assert_eq!(ids(&l), vec![t[0], t[2], t[2]]);
        let last = l.playlist_entries(p).unwrap()[2].item_id;
        l.move_playlist_item(p, last, 0).unwrap();
        assert_eq!(ids(&l), vec![t[2], t[0], t[2]]);

        let mid = l.playlist_entries(p).unwrap()[1].item_id;
        l.remove_playlist_items(p, &[mid]).unwrap();
        assert_eq!(ids(&l), vec![t[2], t[2]]);
        let row = &l.list_playlists().unwrap()[0];
        assert_eq!((row.track_count, row.duration_ms), (2, 6000));
    }

    #[test]
    fn adding_to_a_missing_playlist_or_track_changes_nothing() {
        let mut l = lib();
        let (_, t) = add_memory_album(&mut l, "Alb", "Band", 1);
        assert!(l.add_to_playlist(4242, &[t[0]]).is_err());
        let p = l.create_playlist("Mix").unwrap();
        assert!(l.add_to_playlist(p, &[t[0], 9999]).is_err());
        assert!(l.playlist_entries(p).unwrap().is_empty());
    }

    #[test]
    fn removing_an_album_drops_its_entries_and_deleting_a_playlist_clears_its_tick() {
        let mut l = lib();
        let (a, t) = add_memory_album(&mut l, "Alb", "Band", 2);
        let (_, u) = add_memory_album(&mut l, "Other", "Solo", 1);
        let p = l.create_playlist("Mix").unwrap();
        l.add_to_playlist(p, &[t[0], u[0], t[1]]).unwrap();
        l.delete_album(a, false).unwrap();
        assert_eq!(l.playlist_entries(p).unwrap().len(), 1);
        l.set_selected(SelectionKind::Playlist, &p.to_string(), true).unwrap();
        l.delete_playlist(p).unwrap();
        assert!(l.list_playlists().unwrap().is_empty());
        assert!(l.selection().unwrap().playlists.is_empty());
    }

    #[test]
    fn songs_list_every_track_with_album_fields_flat_in_json() {
        let mut l = lib();
        add_memory_album(&mut l, "Zed", "beta", 1);
        add_memory_album(&mut l, "Alb", "Alpha", 2);
        let songs = l.list_songs().unwrap();
        let order: Vec<(&str, u32)> = songs.iter().map(|s| (s.album_artist.as_str(), s.track.number)).collect();
        assert_eq!(order, vec![("Alpha", 1), ("Alpha", 2), ("beta", 1)]);
        let json = serde_json::to_value(&songs[0]).unwrap();
        assert_eq!(json["album_title"], "Alb");
        assert_eq!(json["title"], "T1");
        assert!(json.get("track").is_none());
    }
}
