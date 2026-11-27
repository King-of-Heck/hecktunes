use super::*;
use crate::library::SyncAs;
use crate::test_support::{add_album, png_bytes, tone, write_pattern_wav};
use std::time::Duration;

/// The ffmpeg sidecar for this platform built by scripts/build-ffmpeg.sh, if it is there.
/// Tests that need it print a note and pass without it.
pub(crate) fn test_ffmpeg() -> Option<PathBuf> {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "linux"
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("binaries");
    let found = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        name.starts_with("ffmpeg-") && name.contains(std::env::consts::ARCH) && name.contains(os)
    });
    if found.is_none() {
        eprintln!("note: no ffmpeg sidecar in src-tauri/binaries; run scripts/build-ffmpeg.sh to test conversion");
    }
    found
}

/// Imports a FLAC album (tagged "Song 1".."Song n", 1.5 s each at `rate`/`bits`, stereo) into `lib`.
pub(crate) fn add_flac_album(lib: &mut Library, parent: &Path, album: &str, artist: &str, n: u32, rate: u32, bits: u16, cover: bool) -> i64 {
    let src = parent.join(format!("src {artist} - {album}"));
    std::fs::create_dir_all(&src).unwrap();
    for i in 1..=n {
        let wav = parent.join(format!("{album}-{i}.wav"));
        write_pattern_wav(&wav, rate, 2, bits, &[tone(1.5, 0.5)]);
        let info = crate::vinyl::source::probe(&wav).unwrap();
        let flac = src.join(format!("{i}.flac"));
        crate::vinyl::encode::encode_range(&info, 0, info.frames, &flac).unwrap();
        write_tags(
            &flac,
            &TrackTags {
                title: format!("Song {i}"),
                artist: artist.into(),
                album: album.into(),
                album_artist: artist.into(),
                track_no: i,
                track_total: n,
                disc_no: 1,
                disc_total: 1,
                year: Some("2001".into()),
                genre: None,
                mb_release_id: None,
                mb_recording_id: None,
            },
            None,
        )
        .unwrap();
    }
    let group = crate::import::scan::scan_paths(&[src]).groups.remove(0);
    crate::import::apply::apply(lib, &group, None, cover.then(|| png_bytes(64, 64)), false).unwrap()
}

fn no_progress(_: usize, _: usize) {}

#[test]
fn converting_an_album_makes_tagged_256k_copies_and_switches_it_to_aac() {
    let Some(exe) = test_ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
    let a = add_flac_album(&mut l, dir.path(), "Hi Res", "Band", 2, 96_000, 24, true);
    let lib = Mutex::new(l);
    let steps = Mutex::new(Vec::new());
    let out = convert_album(&lib, &exe, a, &ConvertOptions::default(), 2, &AtomicBool::new(false), &|d, t| steps.lock().unwrap().push((d, t))).unwrap();
    assert_eq!(out, AlbumOutcome { converted: 2, failed: vec![], cancelled: false });
    assert_eq!(steps.into_inner().unwrap(), vec![(0, 2), (1, 2), (2, 2)]);

    let l = lib.lock().unwrap();
    let row = l.album(a).unwrap().unwrap();
    assert_eq!((row.sync_as, row.aac_count), (SyncAs::Aac, 2));
    for t in l.album_tracks(a).unwrap() {
        let c = t.aac.clone().expect("track has an AAC copy");
        assert_eq!(c.path, t.file_path.with_extension("m4a"));
        assert_eq!((c.size, c.hash.clone()), (std::fs::metadata(&c.path).unwrap().len(), file_hash(&c.path).unwrap()));
        assert!(!t.file_path.with_extension("converting.m4a").exists());
        assert!(t.file_path.exists(), "the FLAC stays");
        let tf = lofty::read_from_path(&c.path).unwrap();
        assert_eq!(tf.file_type(), lofty::file::FileType::Mp4);
        assert_eq!(tf.properties().sample_rate(), Some(48_000));
        assert_eq!(tf.properties().channels(), Some(2));
        let kbps = tf.properties().audio_bitrate().unwrap_or(0);
        assert!((200..=290).contains(&kbps), "about 256 kbps, got {kbps}");
        let tag = tf.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some(t.title.as_str()));
        assert_eq!(tag.album().as_deref(), Some("Hi Res"));
        assert_eq!(tag.track(), Some(t.number));
        assert_eq!(tag.pictures().len(), 1, "the album cover is embedded");
    }
}

#[test]
fn the_chosen_bitrate_is_used_and_the_album_can_stay_on_flac() {
    let Some(exe) = test_ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
    let a = add_flac_album(&mut l, dir.path(), "Small", "Band", 1, 44_100, 16, false);
    let lib = Mutex::new(l);
    let opts = ConvertOptions { bitrate_kbps: 128, switch_to_aac: false };
    let out = convert_album(&lib, &exe, a, &opts, 1, &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(out.converted, 1);
    let l = lib.lock().unwrap();
    let row = l.album(a).unwrap().unwrap();
    assert_eq!((row.sync_as, row.aac_count), (SyncAs::Flac, 1), "converted, but still syncing FLAC");
    let copy = l.album_tracks(a).unwrap()[0].aac.clone().unwrap();
    let kbps = lofty::read_from_path(&copy.path).unwrap().properties().audio_bitrate().unwrap_or(0);
    assert!((100..=160).contains(&kbps), "about 128 kbps, got {kbps}");
}

#[test]
fn bitrates_outside_the_offered_list_are_refused() {
    assert!(ConvertOptions::default().validate().is_ok());
    for kbps in BITRATES {
        assert!(ConvertOptions { bitrate_kbps: kbps, switch_to_aac: true }.validate().is_ok());
    }
    let e = ConvertOptions { bitrate_kbps: 300, switch_to_aac: true }.validate().unwrap_err();
    assert!(e.to_string().contains("300 kbps"), "{e}");
    let svc = ConvertService::start(Arc::new(Mutex::new(Library::open_in_memory(PathBuf::from("lib")).unwrap())), Some(PathBuf::from("ffmpeg")), 1, |_| {});
    assert!(svc.enqueue(1, ConvertOptions { bitrate_kbps: 64, switch_to_aac: true }).is_err());
    assert!(!svc.is_busy(), "a refused album isn't queued");
}

#[test]
fn converting_again_only_does_what_is_missing_and_removing_copies_resets_the_album() {
    let Some(exe) = test_ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
    let a = add_flac_album(&mut l, dir.path(), "Alb", "Band", 2, 44_100, 16, false);
    let lib = Mutex::new(l);
    convert_album(&lib, &exe, a, &ConvertOptions::default(), 1, &AtomicBool::new(false), &no_progress).unwrap();
    let first = lib.lock().unwrap().album_tracks(a).unwrap()[0].aac.clone().unwrap();
    std::fs::remove_file(&first.path).unwrap();
    let again = convert_album(&lib, &exe, a, &ConvertOptions::default(), 1, &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(again.converted, 1, "only the track whose copy went missing");
    assert!(first.path.exists());

    let mut l = lib.lock().unwrap();
    assert_eq!(remove_copies(&mut l, a).unwrap(), 2);
    let row = l.album(a).unwrap().unwrap();
    assert_eq!((row.sync_as, row.aac_count), (SyncAs::Flac, 0));
    assert!(!first.path.exists());
    assert!(l.album_tracks(a).unwrap().iter().all(|t| t.file_path.exists()));
}

#[test]
fn a_failed_encode_leaves_no_file_and_keeps_the_album_on_flac() {
    let dir = tempfile::tempdir().unwrap();
    let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
    let a = add_flac_album(&mut l, dir.path(), "Alb", "Band", 1, 44_100, 16, false);
    let flac = l.album_tracks(a).unwrap()[0].file_path.clone();
    // Leftover from a crash: it is replaced or removed, never kept.
    std::fs::write(flac.with_extension("converting.m4a"), b"junk").unwrap();
    let lib = Mutex::new(l);
    let out = convert_album(&lib, Path::new("no-such-ffmpeg"), a, &ConvertOptions::default(), 2, &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(out.converted, 0);
    assert_eq!(out.failed.len(), 1);
    assert_eq!(out.failed[0].title, "Song 1");
    assert!(out.failed[0].error.contains("couldn't start ffmpeg"), "{:?}", out.failed);
    assert!(!flac.with_extension("converting.m4a").exists());
    assert!(!flac.with_extension("m4a").exists());
    let row = lib.lock().unwrap().album(a).unwrap().unwrap();
    assert_eq!((row.sync_as, row.aac_count), (SyncAs::Flac, 0));
}

#[test]
fn only_flac_tracks_are_converted() {
    let dir = tempfile::tempdir().unwrap();
    let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
    let a = add_album(&mut l, dir.path(), "Wavs", "Band", 2, false);
    assert!(l.album_tracks(a).unwrap().iter().all(|t| !convertible(t)));
    let lib = Mutex::new(l);
    let out = convert_album(&lib, Path::new("no-such-ffmpeg"), a, &ConvertOptions::default(), 2, &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(out, AlbumOutcome::default());
    assert_eq!(lib.lock().unwrap().album(a).unwrap().unwrap().sync_as, SyncAs::Flac);
}

#[test]
fn a_cancelled_run_converts_nothing_more_and_does_not_switch_the_album() {
    let dir = tempfile::tempdir().unwrap();
    let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
    let a = add_flac_album(&mut l, dir.path(), "Alb", "Band", 2, 44_100, 16, false);
    let lib = Mutex::new(l);
    let out = convert_album(&lib, Path::new("no-such-ffmpeg"), a, &ConvertOptions::default(), 1, &AtomicBool::new(true), &no_progress).unwrap();
    assert_eq!(out, AlbumOutcome { converted: 0, failed: vec![], cancelled: true });
}

fn wait_for(rx: &mpsc::Receiver<ConvertEvent>, done: impl Fn(&ConvertState) -> bool) -> Vec<ConvertEvent> {
    let mut seen = Vec::new();
    loop {
        let e = rx.recv_timeout(Duration::from_secs(60)).expect("conversion event");
        let stop = done(&e.state);
        seen.push(e);
        if stop {
            return seen;
        }
    }
}

#[test]
fn the_service_converts_queued_albums_in_turn_and_reports_each() {
    let Some(exe) = test_ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
    let a = add_flac_album(&mut l, dir.path(), "One", "Band", 1, 44_100, 16, false);
    let b = add_flac_album(&mut l, dir.path(), "Two", "Band", 1, 44_100, 16, false);
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let svc = ConvertService::start(Arc::new(Mutex::new(l)), Some(exe), 2, move |e| {
        let _ = tx.lock().unwrap().send(e);
    });
    assert_eq!(svc.unavailable_reason(), None);
    svc.enqueue(a, ConvertOptions::default()).unwrap();
    svc.enqueue(b, ConvertOptions::default()).unwrap();
    assert!(svc.is_busy());
    assert!(svc.enqueue(b, ConvertOptions::default()).is_err(), "an album already queued is refused");
    let events = wait_for(&rx, |s| matches!(s, ConvertState::Finished { .. }));
    let events = [events, wait_for(&rx, |s| matches!(s, ConvertState::Finished { .. }))].concat();
    let finished: Vec<i64> = events.iter().filter(|e| matches!(e.state, ConvertState::Finished { converted: 1, .. })).map(|e| e.album_id).collect();
    assert_eq!(finished, vec![a, b]);
    assert!(events.contains(&ConvertEvent { album_id: b, state: ConvertState::Queued }));
    assert!(events.contains(&ConvertEvent { album_id: a, state: ConvertState::Converting { done: 1, total: 1 } }));
    assert!(!svc.is_busy());
}

#[test]
fn without_ffmpeg_the_service_refuses_and_says_why() {
    let lib = Arc::new(Mutex::new(Library::open_in_memory(PathBuf::from("/lib")).unwrap()));
    let svc = ConvertService::start(lib, None, 1, |_| {});
    assert!(svc.unavailable_reason().unwrap().contains("ffmpeg"));
    assert!(svc.enqueue(1, ConvertOptions::default()).unwrap_err().to_string().contains("ffmpeg"));
    assert!(!svc.is_busy());
}

#[test]
fn events_serialize_flat_with_a_state_tag() {
    let e = ConvertEvent { album_id: 7, state: ConvertState::Converting { done: 1, total: 3 } };
    assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"album_id":7,"state":"converting","done":1,"total":3}"#);
}

#[test]
fn non_ascii_album_and_artist_names_convert() {
    let Some(exe) = test_ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut l = Library::open_in_memory(dir.path().join("lib")).unwrap();
    let a = add_flac_album(&mut l, dir.path(), "Jóga – 日本", "Björk", 1, 44_100, 16, false);
    let lib = Mutex::new(l);
    let out = convert_album(&lib, &exe, a, &ConvertOptions::default(), 1, &AtomicBool::new(false), &no_progress).unwrap();
    assert_eq!(out, AlbumOutcome { converted: 1, failed: vec![], cancelled: false });
    let copy = lib.lock().unwrap().album_tracks(a).unwrap()[0].aac.clone().unwrap();
    assert!(copy.path.to_string_lossy().contains("日本"));
    assert!(copy.path.is_file());
}
