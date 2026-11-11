pub mod editing;
pub mod hash;
pub mod paths;
pub mod playlists;
pub mod selection;

pub use playlists::{PlaylistEntry, PlaylistRow, SongRow};
pub use selection::{Selection, SelectionKind};

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE albums (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    album_artist TEXT NOT NULL,
    year TEXT,
    genre TEXT,
    mb_release_id TEXT,
    folder_path TEXT NOT NULL,
    cover_path TEXT,
    needs_metadata INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER NOT NULL
);
CREATE TABLE tracks (
    id INTEGER PRIMARY KEY,
    album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    disc INTEGER NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    format TEXT NOT NULL,
    file_path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    mb_recording_id TEXT
);
CREATE INDEX tracks_album ON tracks(album_id);
CREATE INDEX albums_release ON albums(mb_release_id);
"#,
    r#"
ALTER TABLE tracks ADD COLUMN content_hash TEXT;
CREATE TABLE playlists (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created_at INTEGER NOT NULL
);
CREATE TABLE playlist_items (
    id INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    position INTEGER NOT NULL
);
CREATE INDEX playlist_items_playlist ON playlist_items(playlist_id, position);
CREATE INDEX playlist_items_track ON playlist_items(track_id);
CREATE TABLE device_selection (
    kind TEXT NOT NULL CHECK (kind IN ('album', 'artist', 'playlist')),
    ref TEXT NOT NULL,
    PRIMARY KEY (kind, ref)
);
"#,
];

#[derive(Debug, Clone)]
pub struct NewAlbum {
    pub title: String,
    pub album_artist: String,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub mb_release_id: Option<String>,
    pub folder: PathBuf,
    pub cover: Option<PathBuf>,
    pub needs_metadata: bool,
}

#[derive(Debug, Clone)]
pub struct NewTrack {
    pub album_id: i64,
    pub disc: u32,
    pub number: u32,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    pub format: String,
    pub file: PathBuf,
    pub size_bytes: u64,
    pub mb_recording_id: Option<String>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlbumRow {
    pub id: i64,
    pub title: String,
    pub album_artist: String,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub mb_release_id: Option<String>,
    pub folder_path: PathBuf,
    pub cover_path: Option<PathBuf>,
    pub needs_metadata: bool,
    pub track_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackRow {
    pub id: i64,
    pub album_id: i64,
    pub disc: u32,
    pub number: u32,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    pub format: String,
    pub file_path: PathBuf,
    pub size_bytes: u64,
    pub mb_recording_id: Option<String>,
    pub content_hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlbumEdit {
    pub title: String,
    pub album_artist: String,
    pub year: Option<String>,
    pub genre: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackEdit {
    pub title: String,
    pub artist: String,
    pub disc: u32,
    pub number: u32,
}

pub struct Library {
    conn: Connection,
    root: PathBuf,
}

/// Relative path -> stored text with '/' separators.
fn rel_text(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn abs_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |acc, part| acc.join(part))
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

const ALBUM_COLUMNS: &str = "a.id, a.title, a.album_artist, a.year, a.genre, a.mb_release_id, a.folder_path, a.cover_path, a.needs_metadata, (SELECT COUNT(*) FROM tracks t WHERE t.album_id = a.id)";
const TRACK_COLUMNS: &str = "id, album_id, disc, number, title, artist, duration_ms, format, file_path, size_bytes, mb_recording_id, content_hash";

fn album_from_row(root: &Path, r: &Row) -> rusqlite::Result<AlbumRow> {
    Ok(AlbumRow {
        id: r.get(0)?,
        title: r.get(1)?,
        album_artist: r.get(2)?,
        year: r.get(3)?,
        genre: r.get(4)?,
        mb_release_id: r.get(5)?,
        folder_path: abs_path(root, &r.get::<_, String>(6)?),
        cover_path: r.get::<_, Option<String>>(7)?.map(|c| abs_path(root, &c)),
        needs_metadata: r.get(8)?,
        track_count: r.get::<_, i64>(9)? as u32,
    })
}

fn track_from_row(root: &Path, r: &Row) -> rusqlite::Result<TrackRow> {
    Ok(TrackRow {
        id: r.get(0)?,
        album_id: r.get(1)?,
        disc: r.get::<_, i64>(2)? as u32,
        number: r.get::<_, i64>(3)? as u32,
        title: r.get(4)?,
        artist: r.get(5)?,
        duration_ms: r.get::<_, i64>(6)? as u64,
        format: r.get(7)?,
        file_path: abs_path(root, &r.get::<_, String>(8)?),
        size_bytes: r.get::<_, i64>(9)? as u64,
        mb_recording_id: r.get(10)?,
        content_hash: r.get(11)?,
    })
}

fn run_migrations(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in migrations.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

fn insert_album_row(conn: &Connection, a: &NewAlbum) -> Result<i64> {
    conn.execute(
        "INSERT INTO albums (title, album_artist, year, genre, mb_release_id, folder_path, cover_path, needs_metadata, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            a.title,
            a.album_artist,
            a.year,
            a.genre,
            a.mb_release_id,
            rel_text(&a.folder),
            a.cover.as_deref().map(rel_text),
            a.needs_metadata,
            now_secs()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn insert_track_row(conn: &Connection, t: &NewTrack) -> Result<i64> {
    conn.execute(
        "INSERT INTO tracks (album_id, disc, number, title, artist, duration_ms, format, file_path, size_bytes, mb_recording_id, content_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            t.album_id,
            t.disc as i64,
            t.number as i64,
            t.title,
            t.artist,
            t.duration_ms as i64,
            t.format,
            rel_text(&t.file),
            t.size_bytes as i64,
            t.mb_recording_id,
            t.content_hash
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

impl Library {
    pub fn open(db_path: &Path, root: PathBuf) -> Result<Self> {
        Self::init(Connection::open(db_path)?, root)
    }

    pub fn open_in_memory(root: PathBuf) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, root)
    }

    fn init(mut conn: Connection, root: PathBuf) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        run_migrations(&mut conn, MIGRATIONS)?;
        Ok(Self { conn, root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn insert_album(&mut self, a: &NewAlbum) -> Result<i64> {
        insert_album_row(&self.conn, a)
    }

    pub fn insert_track(&mut self, t: &NewTrack) -> Result<i64> {
        insert_track_row(&self.conn, t)
    }

    /// Inserts an album and its tracks as one unit: either all rows land or none do.
    /// Each track's `album_id` is ignored and set to the new album's id.
    pub fn insert_album_with_tracks(&mut self, a: &NewAlbum, tracks: &[NewTrack]) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let id = insert_album_row(&tx, a)?;
        for t in tracks {
            insert_track_row(&tx, &NewTrack { album_id: id, ..t.clone() })?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn album(&self, id: i64) -> Result<Option<AlbumRow>> {
        let root = self.root.clone();
        Ok(self
            .conn
            .query_row(&format!("SELECT {ALBUM_COLUMNS} FROM albums a WHERE a.id = ?1"), [id], |r| album_from_row(&root, r))
            .optional()?)
    }

    pub fn list_albums(&self) -> Result<Vec<AlbumRow>> {
        let root = self.root.clone();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {ALBUM_COLUMNS} FROM albums a ORDER BY a.album_artist COLLATE NOCASE, a.year, a.title COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map([], |r| album_from_row(&root, r))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn album_tracks(&self, album_id: i64) -> Result<Vec<TrackRow>> {
        let root = self.root.clone();
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE album_id = ?1 ORDER BY disc, number"))?;
        let rows = stmt.query_map([album_id], |r| track_from_row(&root, r))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn track(&self, id: i64) -> Result<Option<TrackRow>> {
        let root = self.root.clone();
        Ok(self
            .conn
            .query_row(&format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE id = ?1"), [id], |r| track_from_row(&root, r))
            .optional()?)
    }

    pub fn find_album_by_release(&self, mbid: &str) -> Result<Option<i64>> {
        self.find_other_album_by_release(mbid, None)
    }

    /// Lowest-id album holding `mbid`, ignoring `exclude` (the album being re-identified).
    pub fn find_other_album_by_release(&self, mbid: &str, exclude: Option<i64>) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM albums WHERE mb_release_id = ?1 AND id IS NOT ?2 ORDER BY id LIMIT 1",
                params![mbid, exclude],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Removes the album. With `remove_files`, its folder goes first (only when strictly inside the
    /// root and not shared with another album); if that fails the row is kept, so nothing on disk is
    /// left without a library entry.
    pub fn delete_album(&mut self, id: i64, remove_files: bool) -> Result<()> {
        let Some(a) = self.album(id)? else { return Ok(()) };
        if remove_files {
            let rel = a.folder_path.strip_prefix(&self.root).map(rel_text).unwrap_or_default();
            let inside_root = !rel.is_empty()
                && a.folder_path.starts_with(&self.root)
                && !a.folder_path.components().any(|c| matches!(c, std::path::Component::ParentDir));
            let shared: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM albums WHERE folder_path = ?1 AND id != ?2",
                params![rel, id],
                |r| r.get(0),
            )?;
            if inside_root && shared == 0 {
                match fs::remove_dir_all(&a.folder_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(anyhow!("couldn't remove {}: {e}", a.folder_path.display())),
                }
                if let Some(parent) = a.folder_path.parent().filter(|p| *p != self.root) {
                    let _ = fs::remove_dir(parent); // only succeeds when the artist folder is now empty
                }
            }
        }
        self.conn.execute("DELETE FROM device_selection WHERE kind = 'album' AND ref = ?1", [id.to_string()])?;
        self.conn.execute("DELETE FROM albums WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Records a track file's new size and BLAKE3 hash after its bytes changed (tag rewrite).
    pub fn set_track_file_state(&mut self, track_id: i64, size_bytes: u64, hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET size_bytes = ?1, content_hash = ?2 WHERE id = ?3",
            params![size_bytes as i64, hash, track_id],
        )?;
        Ok(())
    }

    pub fn move_album_folder(&mut self, id: i64, new_rel: &Path) -> Result<()> {
        let album = self.album(id)?.ok_or_else(|| anyhow!("album {id} not found"))?;
        let tracks = self.album_tracks(id)?;
        let new_abs = self.root.join(new_rel);
        if new_abs.exists() {
            return Err(anyhow!("destination {} already exists", new_abs.display()));
        }
        if let Some(parent) = new_abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&album.folder_path, &new_abs)?;
        let new_rel_s = rel_text(new_rel);
        let new_cover = album
            .cover_path
            .as_ref()
            .and_then(|c| c.file_name())
            .map(|n| format!("{new_rel_s}/{}", n.to_string_lossy()));
        let result = (|| -> Result<()> {
            let tx = self.conn.transaction()?;
            tx.execute(
                "UPDATE albums SET folder_path = ?1, cover_path = ?2 WHERE id = ?3",
                params![new_rel_s, new_cover, id],
            )?;
            for t in &tracks {
                let name = t.file_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                tx.execute("UPDATE tracks SET file_path = ?1 WHERE id = ?2", params![format!("{new_rel_s}/{name}"), t.id])?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(e) = result {
            if let Err(re) = fs::rename(&new_abs, &album.folder_path) {
                return Err(anyhow!("{e}; additionally failed to restore folder: {re}"));
            }
            return Err(e);
        }
        Ok(())
    }

    pub fn update_album_fields(&mut self, id: i64, e: &AlbumEdit) -> Result<()> {
        self.conn.execute(
            "UPDATE albums SET title = ?1, album_artist = ?2, year = ?3, genre = ?4, needs_metadata = 0 WHERE id = ?5",
            params![e.title, e.album_artist, e.year, e.genre, id],
        )?;
        Ok(())
    }

    pub fn update_track_fields(&mut self, id: i64, e: &TrackEdit) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET title = ?1, artist = ?2, disc = ?3, number = ?4 WHERE id = ?5",
            params![e.title, e.artist, e.disc as i64, e.number as i64, id],
        )?;
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn album(title: &str, artist: &str, mbid: Option<&str>) -> NewAlbum {
        NewAlbum {
            title: title.into(),
            album_artist: artist.into(),
            year: Some("1997".into()),
            genre: None,
            mb_release_id: mbid.map(String::from),
            folder: PathBuf::from(artist).join(title),
            cover: Some(PathBuf::from(artist).join(title).join("cover.jpg")),
            needs_metadata: false,
        }
    }

    fn track(album_id: i64, n: u32, rel_dir: &str) -> NewTrack {
        NewTrack {
            album_id,
            disc: 1,
            number: n,
            title: format!("T{n}"),
            artist: "A".into(),
            duration_ms: 1000 * n as u64,
            format: "flac".into(),
            file: PathBuf::from(rel_dir).join(format!("0{n} T{n}.flac")),
            size_bytes: 10,
            mb_recording_id: None,
            content_hash: None,
        }
    }

    #[test]
    fn insert_and_list_albums_sorted_with_absolute_paths() {
        let root = PathBuf::from("/lib");
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let b = lib.insert_album(&album("Zed", "beta", None)).unwrap();
        let a = lib.insert_album(&album("Alpha", "Alpha Band", Some("mb-1"))).unwrap();
        lib.insert_track(&track(a, 1, "Alpha Band/Alpha")).unwrap();
        lib.insert_track(&track(a, 2, "Alpha Band/Alpha")).unwrap();

        let rows = lib.list_albums().unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(rows[0].track_count, 2);
        assert_eq!(rows[0].folder_path, root.join("Alpha Band").join("Alpha"));
        assert_eq!(rows[0].cover_path, Some(root.join("Alpha Band").join("Alpha").join("cover.jpg")));
    }

    #[test]
    fn album_tracks_ordered_by_disc_then_number() {
        let mut lib = Library::open_in_memory(PathBuf::from("/lib")).unwrap();
        let a = lib.insert_album(&album("X", "Y", None)).unwrap();
        lib.insert_track(&track(a, 2, "Y/X")).unwrap();
        lib.insert_track(&track(a, 1, "Y/X")).unwrap();
        let t = lib.album_tracks(a).unwrap();
        assert_eq!(t.iter().map(|t| t.number).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(t[0].duration_ms, 1000);
        assert_eq!(t[0].file_path, PathBuf::from("/lib").join("Y").join("X").join("01 T1.flac"));
    }

    #[test]
    fn find_by_release_and_delete_cascades() {
        let mut lib = Library::open_in_memory(PathBuf::from("/lib")).unwrap();
        let a = lib.insert_album(&album("X", "Y", Some("mb-9"))).unwrap();
        lib.insert_track(&track(a, 1, "Y/X")).unwrap();
        assert_eq!(lib.find_album_by_release("mb-9").unwrap(), Some(a));
        lib.delete_album(a, false).unwrap();
        assert_eq!(lib.find_album_by_release("mb-9").unwrap(), None);
        assert!(lib.album_tracks(a).unwrap().is_empty());
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lib.db");
        {
            let mut lib = Library::open(&db, dir.path().to_path_buf()).unwrap();
            lib.insert_album(&album("X", "Y", None)).unwrap();
        }
        let lib = Library::open(&db, dir.path().to_path_buf()).unwrap();
        assert_eq!(lib.list_albums().unwrap().len(), 1);
    }

    #[test]
    fn move_album_folder_renames_and_rewrites_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("Y/X [2]")).unwrap();
        fs::write(root.join("Y/X [2]/01 T1.flac"), b"x").unwrap();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let mut a = album("X", "Y", None);
        a.folder = PathBuf::from("Y/X [2]");
        a.cover = Some(PathBuf::from("Y/X [2]/cover.jpg"));
        let id = lib.insert_album(&a).unwrap();
        lib.insert_track(&track(id, 1, "Y/X [2]")).unwrap();

        lib.move_album_folder(id, Path::new("Y/X")).unwrap();

        assert!(root.join("Y/X/01 T1.flac").exists());
        let row = lib.album(id).unwrap().unwrap();
        assert_eq!(row.folder_path, root.join("Y").join("X"));
        assert_eq!(row.cover_path, Some(root.join("Y").join("X").join("cover.jpg")));
        assert_eq!(lib.album_tracks(id).unwrap()[0].file_path, root.join("Y").join("X").join("01 T1.flac"));
    }

    #[test]
    fn delete_album_can_remove_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("Y/X")).unwrap();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let id = lib.insert_album(&album("X", "Y", None)).unwrap();
        lib.delete_album(id, true).unwrap();
        assert!(!root.join("Y/X").exists());
    }

    #[test]
    fn updates_album_and_track_fields() {
        let mut lib = Library::open_in_memory(PathBuf::from("/lib")).unwrap();
        let a = lib.insert_album(&album("X", "Y", None)).unwrap();
        let t = lib.insert_track(&track(a, 1, "Y/X")).unwrap();
        lib.update_album_fields(a, &AlbumEdit { title: "New".into(), album_artist: "Z".into(), year: None, genre: Some("rock".into()) }).unwrap();
        lib.update_track_fields(t, &TrackEdit { title: "Song".into(), artist: "Q".into(), disc: 1, number: 5 }).unwrap();
        let row = lib.album(a).unwrap().unwrap();
        assert_eq!((row.title.as_str(), row.album_artist.as_str(), row.year.clone(), row.genre.clone()), ("New", "Z", None, Some("rock".into())));
        let tr = &lib.album_tracks(a).unwrap()[0];
        assert_eq!((tr.title.as_str(), tr.artist.as_str(), tr.number), ("Song", "Q", 5));
    }

    #[test]
    fn delete_album_never_removes_library_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("keep.txt"), b"x").unwrap();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let mut a = album("X", "Y", None);
        a.folder = PathBuf::new();
        a.cover = None;
        let id = lib.insert_album(&a).unwrap();
        lib.delete_album(id, true).unwrap();
        assert!(root.join("keep.txt").exists());
        assert!(lib.album(id).unwrap().is_none());
    }

    #[test]
    fn delete_album_never_removes_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("lib");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"x").unwrap();
        let mut lib = Library::open_in_memory(root).unwrap();
        let mut a = album("X", "Y", None);
        a.folder = PathBuf::from("..").join("outside");
        a.cover = None;
        let id = lib.insert_album(&a).unwrap();
        lib.delete_album(id, true).unwrap();
        assert!(outside.join("keep.txt").exists());
    }

    #[test]
    fn delete_album_keeps_folder_shared_with_another_album() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("Y/X")).unwrap();
        fs::write(root.join("Y/X/01.flac"), b"x").unwrap();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let a = lib.insert_album(&album("X", "Y", None)).unwrap();
        let b = lib.insert_album(&album("X", "Y", None)).unwrap();
        lib.delete_album(a, true).unwrap();
        assert!(root.join("Y/X/01.flac").exists());
        assert!(lib.album(b).unwrap().is_some());
    }

    #[test]
    fn move_album_folder_refuses_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("Y/Old")).unwrap();
        fs::write(root.join("Y/Old/01 T1.flac"), b"x").unwrap();
        fs::create_dir_all(root.join("Y/New")).unwrap();
        fs::write(root.join("Y/New/other.flac"), b"o").unwrap();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let mut a = album("X", "Y", None);
        a.folder = PathBuf::from("Y/Old");
        a.cover = None;
        let id = lib.insert_album(&a).unwrap();
        lib.insert_track(&track(id, 1, "Y/Old")).unwrap();
        assert!(lib.move_album_folder(id, Path::new("Y/New")).is_err());
        assert!(root.join("Y/Old/01 T1.flac").exists());
        assert!(root.join("Y/New/other.flac").exists());
        assert_eq!(lib.album(id).unwrap().unwrap().folder_path, root.join("Y").join("Old"));
    }

    #[test]
    fn move_album_folder_rolls_back_rename_when_db_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("Y/Old")).unwrap();
        fs::write(root.join("Y/Old/01 T1.flac"), b"x").unwrap();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let mut a = album("X", "Y", None);
        a.folder = PathBuf::from("Y/Old");
        a.cover = None;
        let id = lib.insert_album(&a).unwrap();
        lib.insert_track(&track(id, 1, "Y/Old")).unwrap();
        lib.conn
            .execute_batch("CREATE TRIGGER boom BEFORE UPDATE ON tracks BEGIN SELECT RAISE(ABORT, 'boom'); END;")
            .unwrap();
        assert!(lib.move_album_folder(id, Path::new("Y/New")).is_err());
        assert!(root.join("Y/Old/01 T1.flac").exists());
        assert!(!root.join("Y/New").exists());
        assert_eq!(lib.album(id).unwrap().unwrap().folder_path, root.join("Y").join("Old"));
    }

    #[test]
    fn move_album_folder_keeps_cover_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join("Y/Old")).unwrap();
        fs::write(root.join("Y/Old/folder.png"), b"x").unwrap();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let mut a = album("X", "Y", None);
        a.folder = PathBuf::from("Y/Old");
        a.cover = Some(PathBuf::from("Y/Old/folder.png"));
        let id = lib.insert_album(&a).unwrap();
        lib.move_album_folder(id, Path::new("Y/New")).unwrap();
        assert_eq!(lib.album(id).unwrap().unwrap().cover_path, Some(root.join("Y").join("New").join("folder.png")));
    }

    #[test]
    fn failed_migration_rolls_back_sql_and_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        let migs: &[&str] = &["CREATE TABLE ok_t (a INTEGER);", "CREATE TABLE half_t (a INTEGER); THIS IS NOT SQL;"];
        assert!(run_migrations(&mut conn, migs).is_err());
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'half_t'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn insert_album_with_tracks_is_all_or_nothing() {
        let mut lib = Library::open_in_memory(PathBuf::from("/lib")).unwrap();
        let ok = lib.insert_album_with_tracks(&album("X", "Y", None), &[track(0, 1, "Y/X"), track(0, 2, "Y/X")]).unwrap();
        assert_eq!(lib.album(ok).unwrap().unwrap().track_count, 2);
        assert!(lib.album_tracks(ok).unwrap().iter().all(|t| t.album_id == ok));

        lib.conn
            .execute_batch("CREATE TRIGGER boom BEFORE INSERT ON tracks WHEN NEW.number = 9 BEGIN SELECT RAISE(ABORT, 'boom'); END;")
            .unwrap();
        let err = lib.insert_album_with_tracks(&album("Z", "Y", None), &[track(0, 1, "Y/Z"), track(0, 9, "Y/Z")]);
        assert!(err.is_err());
        let albums = lib.list_albums().unwrap();
        assert_eq!(albums.len(), 1, "no partial album left behind: {albums:?}");
        assert_eq!(albums[0].id, ok);
        let orphans: i64 = lib.conn.query_row("SELECT COUNT(*) FROM tracks WHERE album_id <> ?1", [ok], |r| r.get(0)).unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn upgrades_a_version_1_database_in_place() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run_migrations(&mut conn, &MIGRATIONS[..1]).unwrap();
        conn.execute("INSERT INTO albums (title, album_artist, folder_path, added_at) VALUES ('A', 'B', 'B/A', 0)", []).unwrap();
        conn.execute(
            "INSERT INTO tracks (album_id, disc, number, title, artist, duration_ms, format, file_path, size_bytes)
             VALUES (1, 1, 1, 'T', 'B', 1000, 'flac', 'B/A/01 T.flac', 10)",
            [],
        )
        .unwrap();
        let lib = Library::init(conn, PathBuf::from("/lib")).unwrap();
        let v: i64 = lib.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        assert_eq!(lib.album_tracks(1).unwrap()[0].content_hash, None);
        for table in ["playlists", "playlist_items", "device_selection"] {
            let n: i64 = lib
                .conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [table], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1, "{table}");
        }
    }

    #[test]
    fn stores_and_updates_track_hash_and_size() {
        let mut lib = Library::open_in_memory(PathBuf::from("/lib")).unwrap();
        let a = lib.insert_album(&album("A", "B", None)).unwrap();
        let t = lib.insert_track(&NewTrack { content_hash: Some("h1".into()), ..track(a, 1, "B/A") }).unwrap();
        assert_eq!(lib.track(t).unwrap().unwrap().content_hash.as_deref(), Some("h1"));
        lib.set_track_file_state(t, 99, "h2").unwrap();
        let row = lib.track(t).unwrap().unwrap();
        assert_eq!((row.size_bytes, row.content_hash.as_deref()), (99, Some("h2")));
    }

    #[test]
    fn find_other_album_by_release_skips_the_excluded_album() {
        let mut lib = Library::open_in_memory(PathBuf::from("/lib")).unwrap();
        let x = lib.insert_album(&album("X", "B", Some("mb"))).unwrap();
        let y = lib.insert_album(&album("Y", "B", Some("mb"))).unwrap();
        assert_eq!(lib.find_other_album_by_release("mb", Some(x)).unwrap(), Some(y));
        assert_eq!(lib.find_other_album_by_release("mb", Some(y)).unwrap(), Some(x));
        assert_eq!(lib.find_album_by_release("mb").unwrap(), Some(x));
        assert_eq!(lib.find_other_album_by_release("none", None).unwrap(), None);
    }

    #[test]
    fn failed_file_removal_keeps_the_album_row() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let a = lib.insert_album(&album("Alb", "Band", None)).unwrap();
        let folder = root.join("Band").join("Alb");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("01 T1.flac"), b"x").unwrap();
        let guard = crate::test_support::lock_folder(&folder);
        assert!(lib.delete_album(a, true).is_err());
        drop(guard);
        assert!(lib.album(a).unwrap().is_some(), "the row must survive a failed removal");
    }

    #[test]
    fn removing_an_album_also_removes_its_now_empty_artist_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let mut lib = Library::open_in_memory(root.clone()).unwrap();
        let a = lib.insert_album(&album("Alb", "Band", None)).unwrap();
        let folder = root.join("Band").join("Alb");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("01 T1.flac"), b"x").unwrap();
        lib.delete_album(a, true).unwrap();
        assert!(!root.join("Band").exists());
        assert!(root.exists());
    }

    #[test]
    fn deleting_an_album_clears_its_echo_mini_tick() {
        let mut lib = Library::open_in_memory(PathBuf::from("/lib")).unwrap();
        let a = lib.insert_album(&album("A", "B", None)).unwrap();
        lib.conn.execute("INSERT INTO device_selection (kind, ref) VALUES ('album', ?1)", [a.to_string()]).unwrap();
        lib.delete_album(a, false).unwrap();
        let n: i64 = lib.conn.query_row("SELECT COUNT(*) FROM device_selection", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
