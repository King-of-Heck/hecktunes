//! One-track preview player. A dedicated thread owns the audio device; everything else talks
//! to it through `PlayerService` and hears back through the `notify` callback.
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often position updates are sent while playing.
pub const TICK: Duration = Duration::from_millis(250);

/// The audio device as the player thread sees it. Implemented by `RodioAudio` and by a test fake.
pub trait Audio {
    /// Replaces whatever is playing with `path` and starts it. Returns the duration if the decoder knows it.
    fn load(&mut self, path: &Path) -> Result<Option<u64>>;
    fn pause(&mut self);
    fn resume(&mut self);
    fn seek(&mut self, ms: u64) -> Result<()>;
    fn stop(&mut self);
    fn position_ms(&self) -> u64;
    fn finished(&self) -> bool;
}

pub struct RodioAudio {
    sink: rodio::MixerDeviceSink,
    player: rodio::Player,
}

impl RodioAudio {
    pub fn open() -> Result<Self> {
        let mut sink = rodio::DeviceSinkBuilder::open_default_sink().map_err(|e| anyhow!("{e}"))?;
        sink.log_on_drop(false);
        let player = rodio::Player::connect_new(sink.mixer());
        Ok(Self { sink, player })
    }
}

impl Audio for RodioAudio {
    fn load(&mut self, path: &Path) -> Result<Option<u64>> {
        use rodio::Source;
        let file = std::fs::File::open(path)?;
        let source = rodio::Decoder::try_from(file).map_err(|e| anyhow!("{e}"))?;
        let duration = source.total_duration().map(|d| d.as_millis() as u64);
        // Drop the old player and start a fresh one rather than `clear()`, which blocks until the
        // audio thread answers and would hang this thread on a wedged or unplugged device.
        self.player = rodio::Player::connect_new(self.sink.mixer());
        self.player.append(source);
        self.player.play();
        Ok(duration)
    }
    fn pause(&mut self) {
        self.player.pause();
    }
    fn resume(&mut self) {
        self.player.play();
    }
    fn seek(&mut self, ms: u64) -> Result<()> {
        // Note: `try_seek` can still block until the audio thread answers; no way around that here.
        self.player.try_seek(Duration::from_millis(ms)).map_err(|e| anyhow!("{e}"))
    }
    fn stop(&mut self) {
        // Dropping a `Player` sets its `stopped` flag, which silences and removes its sources.
        self.player = rodio::Player::connect_new(self.sink.mixer());
    }
    fn position_ms(&self) -> u64 {
        self.player.get_pos().as_millis() as u64
    }
    fn finished(&self) -> bool {
        self.player.empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayState {
    Idle,
    Playing,
    Paused,
    Ended,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayerStatus {
    pub state: PlayState,
    pub track_id: Option<i64>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub error: Option<String>,
}

impl PlayerStatus {
    fn idle() -> Self {
        Self { state: PlayState::Idle, track_id: None, position_ms: 0, duration_ms: 0, error: None }
    }
}

enum Cmd {
    Play { track_id: i64, path: PathBuf, duration_ms: u64 },
    Pause,
    Resume,
    Seek(u64),
    /// `reply` hears back once the stop has been handled.
    Stop { reply: Option<Sender<()>> },
}

pub struct PlayerService {
    tx: Sender<Cmd>,
    /// The track whose file the player holds (any state but idle). Its own small lock, never the
    /// Library's, so commands can check it without blocking on the player.
    current: Arc<Mutex<Option<i64>>>,
}

/// The track a status says the player holds.
fn held_track(status: &PlayerStatus) -> Option<i64> {
    if status.state == PlayState::Idle { None } else { status.track_id }
}

impl PlayerService {
    pub fn start(
        open: impl Fn() -> Result<Box<dyn Audio>> + Send + 'static,
        notify: impl Fn(PlayerStatus) + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel::<Cmd>();
        let current = Arc::new(Mutex::new(None));
        let held = current.clone();
        let set_held = move |s: &PlayerStatus| *held.lock().unwrap_or_else(|p| p.into_inner()) = held_track(s);
        std::thread::Builder::new()
            .name("hecktunes-player".into())
            .spawn(move || {
                let mut audio: Option<Box<dyn Audio>> = None;
                let mut status = PlayerStatus::idle();
                loop {
                    match rx.recv_timeout(TICK) {
                        Ok(mut cmd) => {
                            let track_id = match &cmd {
                                Cmd::Play { track_id, .. } => Some(*track_id),
                                _ => status.track_id,
                            };
                            let reply = match &mut cmd {
                                Cmd::Stop { reply } => reply.take(),
                                _ => None,
                            };
                            let done = catch_unwind(AssertUnwindSafe(|| handle(cmd, &open, &mut audio, &mut status)));
                            if done.is_err() {
                                audio = None;
                                status = panic_status(track_id);
                            }
                            set_held(&status);
                            if let Some(reply) = reply {
                                let _ = reply.send(());
                            }
                            notify(status.clone());
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if status.state != PlayState::Playing {
                                continue;
                            }
                            let Some(a) = audio.as_ref() else { continue };
                            match catch_unwind(AssertUnwindSafe(|| if a.finished() { None } else { Some(a.position_ms()) })) {
                                Ok(None) => {
                                    status.state = PlayState::Ended;
                                    status.position_ms = status.duration_ms;
                                    notify(status.clone());
                                }
                                Ok(Some(pos)) => {
                                    if pos != status.position_ms {
                                        status.position_ms = pos;
                                        notify(status.clone());
                                    }
                                }
                                Err(_) => {
                                    audio = None;
                                    status = panic_status(status.track_id);
                                    set_held(&status);
                                    notify(status.clone());
                                }
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("couldn't start the player thread");
        Self { tx, current }
    }

    /// The track whose file the player holds right now, if any.
    pub fn current_track(&self) -> Option<i64> {
        *self.current.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Stops playback and waits (up to `timeout`) until the player has let go of the file.
    /// Returns false if the player didn't answer in time.
    pub fn stop_and_wait(&self, timeout: Duration) -> bool {
        let (reply, done) = channel();
        if self.tx.send(Cmd::Stop { reply: Some(reply) }).is_err() {
            return true; // no player thread, so nothing holds a file
        }
        done.recv_timeout(timeout).is_ok()
    }

    pub fn play(&self, track_id: i64, path: PathBuf, duration_ms: u64) {
        let _ = self.tx.send(Cmd::Play { track_id, path, duration_ms });
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }
    pub fn resume(&self) {
        let _ = self.tx.send(Cmd::Resume);
    }
    pub fn seek(&self, ms: u64) {
        let _ = self.tx.send(Cmd::Seek(ms));
    }
    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop { reply: None });
    }
}

/// What the UI sees when the audio code panicked; the device is reopened on the next play.
fn panic_status(track_id: Option<i64>) -> PlayerStatus {
    PlayerStatus {
        state: PlayState::Error,
        track_id,
        error: Some("Playback stopped unexpectedly. Try playing the track again.".into()),
        ..PlayerStatus::idle()
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn handle(
    cmd: Cmd,
    open: &dyn Fn() -> Result<Box<dyn Audio>>,
    audio: &mut Option<Box<dyn Audio>>,
    status: &mut PlayerStatus,
) {
    match cmd {
        Cmd::Play { track_id, path, duration_ms } => {
            if audio.is_none() {
                match open() {
                    Ok(a) => *audio = Some(a),
                    Err(e) => {
                        *status = PlayerStatus {
                            error: Some(format!("No sound output is available ({e}).")),
                            state: PlayState::Error,
                            track_id: Some(track_id),
                            ..PlayerStatus::idle()
                        };
                        return;
                    }
                }
            }
            let a = audio.as_mut().expect("opened above");
            match a.load(&path) {
                Ok(decoded) => {
                    *status = PlayerStatus {
                        state: PlayState::Playing,
                        track_id: Some(track_id),
                        position_ms: 0,
                        duration_ms: decoded.unwrap_or(duration_ms),
                        error: None,
                    };
                }
                Err(e) => {
                    a.stop();
                    *status = PlayerStatus {
                        state: PlayState::Error,
                        track_id: Some(track_id),
                        error: Some(format!("Can't play {} ({e}).", file_name(&path))),
                        ..PlayerStatus::idle()
                    };
                }
            }
        }
        Cmd::Pause => {
            if let (Some(a), PlayState::Playing) = (audio.as_mut(), status.state) {
                a.pause();
                status.position_ms = a.position_ms();
                status.state = PlayState::Paused;
            }
        }
        Cmd::Resume => {
            if let (Some(a), PlayState::Paused) = (audio.as_mut(), status.state) {
                a.resume();
                status.state = PlayState::Playing;
            }
        }
        Cmd::Seek(ms) => {
            if let (Some(a), PlayState::Playing | PlayState::Paused) = (audio.as_mut(), status.state) {
                let ms = if status.duration_ms > 0 { ms.min(status.duration_ms) } else { ms };
                if a.seek(ms).is_ok() {
                    status.position_ms = ms;
                }
            }
        }
        Cmd::Stop { .. } => {
            if let Some(a) = audio.as_mut() {
                a.stop();
            }
            *status = PlayerStatus::idle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};
    use std::sync::{Arc, Mutex};

    /// What the fake device has been told, shared with the test.
    #[derive(Default)]
    struct Shared {
        loaded: Vec<PathBuf>,
        paused: bool,
        position: u64,
        finished: bool,
        fail_load: bool,
        /// True when `stop` was called after the last successful load.
        stopped_since_load: bool,
        /// pause/resume/seek calls received.
        control_calls: u32,
        /// How long `stop` takes (a slow or wedged device).
        stop_delay_ms: u64,
    }

    struct FakeAudio(Arc<Mutex<Shared>>);

    impl Audio for FakeAudio {
        fn load(&mut self, path: &Path) -> anyhow::Result<Option<u64>> {
            if path.to_string_lossy().contains("corrupt") {
                panic!("decoder blew up on a corrupt file");
            }
            let mut s = self.0.lock().unwrap();
            if s.fail_load {
                anyhow::bail!("unsupported format");
            }
            s.loaded.push(path.to_path_buf());
            s.paused = false;
            s.position = 0;
            s.finished = false;
            s.stopped_since_load = false;
            Ok(Some(90_000))
        }
        fn pause(&mut self) {
            let mut s = self.0.lock().unwrap();
            s.paused = true;
            s.control_calls += 1;
        }
        fn resume(&mut self) {
            let mut s = self.0.lock().unwrap();
            s.paused = false;
            s.control_calls += 1;
        }
        fn seek(&mut self, ms: u64) -> anyhow::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.position = ms;
            s.control_calls += 1;
            Ok(())
        }
        fn stop(&mut self) {
            let delay = self.0.lock().unwrap().stop_delay_ms;
            std::thread::sleep(std::time::Duration::from_millis(delay));
            let mut s = self.0.lock().unwrap();
            s.position = 0;
            s.finished = false;
            s.stopped_since_load = true;
        }
        fn position_ms(&self) -> u64 {
            self.0.lock().unwrap().position
        }
        fn finished(&self) -> bool {
            self.0.lock().unwrap().finished
        }
    }

    fn service(shared: Arc<Mutex<Shared>>, open_fails: Arc<Mutex<bool>>) -> (PlayerService, Receiver<PlayerStatus>) {
        let (tx, rx) = channel();
        let svc = PlayerService::start(
            move || {
                if *open_fails.lock().unwrap() {
                    anyhow::bail!("no output device")
                }
                Ok(Box::new(FakeAudio(shared.clone())) as Box<dyn Audio>)
            },
            move |s| {
                let _ = tx.send(s);
            },
        );
        (svc, rx)
    }

    /// Next status matching `want` (fails after 5 s).
    fn wait_for(rx: &Receiver<PlayerStatus>, want: impl Fn(&PlayerStatus) -> bool) -> PlayerStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let s = rx.recv_timeout(left).expect("player status");
            if want(&s) {
                return s;
            }
        }
    }

    #[test]
    fn play_pause_resume_seek_and_stop() {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        svc.play(7, PathBuf::from("a.flac"), 80_000);
        let s = wait_for(&rx, |s| s.state == PlayState::Playing);
        assert_eq!((s.track_id, s.duration_ms), (Some(7), 90_000)); // decoder's duration wins over the DB's
        assert_eq!(shared.lock().unwrap().loaded, vec![PathBuf::from("a.flac")]);

        svc.pause();
        wait_for(&rx, |s| s.state == PlayState::Paused);
        assert!(shared.lock().unwrap().paused);
        svc.resume();
        wait_for(&rx, |s| s.state == PlayState::Playing);

        svc.seek(30_000);
        let s = wait_for(&rx, |s| s.position_ms == 30_000);
        assert_eq!(s.state, PlayState::Playing);

        svc.stop();
        let s = wait_for(&rx, |s| s.state == PlayState::Idle);
        assert_eq!(s.track_id, None);
    }

    #[test]
    fn reports_position_while_playing_and_ended_once_at_the_end() {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        svc.play(1, PathBuf::from("a.flac"), 0);
        wait_for(&rx, |s| s.state == PlayState::Playing);
        shared.lock().unwrap().position = 1234;
        wait_for(&rx, |s| s.position_ms == 1234);
        shared.lock().unwrap().finished = true;
        let s = wait_for(&rx, |s| s.state == PlayState::Ended);
        assert_eq!(s.track_id, Some(1));
        // no repeats after the end: drain for ~4 ticks, none may be Ended
        let until = std::time::Instant::now() + TICK * 4;
        loop {
            let left = until.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(s) => assert_ne!(s.state, PlayState::Ended, "repeated Ended"),
                Err(_) => break,
            }
        }
    }

    #[test]
    fn a_decoder_panic_is_reported_and_the_player_keeps_working() {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        svc.play(1, PathBuf::from("ok.flac"), 0);
        wait_for(&rx, |s| s.state == PlayState::Playing);
        svc.play(5, PathBuf::from("corrupt.flac"), 0);
        let s = wait_for(&rx, |s| s.state == PlayState::Error);
        assert_eq!(s.track_id, Some(5));
        assert!(s.error.as_deref().unwrap().contains("Playback stopped unexpectedly"), "{s:?}");
        svc.play(2, PathBuf::from("b.flac"), 0);
        wait_for(&rx, |s| s.state == PlayState::Playing && s.track_id == Some(2));
    }

    #[test]
    fn a_failed_load_stops_the_previous_track() {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        svc.play(1, PathBuf::from("a.flac"), 0);
        wait_for(&rx, |s| s.state == PlayState::Playing);
        assert!(!shared.lock().unwrap().stopped_since_load);
        shared.lock().unwrap().fail_load = true;
        svc.play(2, PathBuf::from("b.opus"), 0);
        wait_for(&rx, |s| s.state == PlayState::Error);
        assert!(shared.lock().unwrap().stopped_since_load);
    }

    #[test]
    fn pause_resume_and_seek_do_nothing_after_an_error_or_the_end() {
        // Error state
        let shared = Arc::new(Mutex::new(Shared { fail_load: true, ..Default::default() }));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        svc.play(1, PathBuf::from("a.opus"), 0);
        let err = wait_for(&rx, |s| s.state == PlayState::Error);
        svc.pause();
        svc.resume();
        svc.seek(5_000);
        for _ in 0..3 {
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), err);
        }
        assert_eq!(shared.lock().unwrap().control_calls, 0);

        // Ended state
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        svc.play(1, PathBuf::from("a.flac"), 0);
        wait_for(&rx, |s| s.state == PlayState::Playing);
        shared.lock().unwrap().finished = true;
        let ended = wait_for(&rx, |s| s.state == PlayState::Ended);
        svc.pause();
        svc.resume();
        svc.seek(5_000);
        for _ in 0..3 {
            assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), ended);
        }
        assert_eq!(shared.lock().unwrap().control_calls, 0);
    }

    #[test]
    fn unplayable_files_and_missing_devices_report_errors_and_recover() {
        let shared = Arc::new(Mutex::new(Shared { fail_load: true, ..Default::default() }));
        let open_fails = Arc::new(Mutex::new(true));
        let (svc, rx) = service(shared.clone(), open_fails.clone());

        svc.play(1, PathBuf::from("a.opus"), 1000);
        let s = wait_for(&rx, |s| s.state == PlayState::Error);
        assert!(s.error.as_deref().unwrap().contains("sound output"), "{s:?}");
        assert_eq!(s.track_id, Some(1));

        *open_fails.lock().unwrap() = false;
        svc.play(1, PathBuf::from("a.opus"), 1000);
        let s = wait_for(&rx, |s| s.state == PlayState::Error);
        assert!(s.error.as_deref().unwrap().contains("a.opus"), "{s:?}");

        shared.lock().unwrap().fail_load = false;
        svc.play(2, PathBuf::from("b.flac"), 1000);
        wait_for(&rx, |s| s.state == PlayState::Playing && s.track_id == Some(2));
    }

    #[test]
    fn current_track_follows_what_the_player_holds() {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        assert_eq!(svc.current_track(), None);
        svc.play(7, PathBuf::from("a.flac"), 0);
        wait_for(&rx, |s| s.state == PlayState::Playing);
        assert_eq!(svc.current_track(), Some(7));
        svc.pause();
        wait_for(&rx, |s| s.state == PlayState::Paused);
        assert_eq!(svc.current_track(), Some(7));
        svc.stop();
        wait_for(&rx, |s| s.state == PlayState::Idle);
        assert_eq!(svc.current_track(), None);
    }

    #[test]
    fn stop_and_wait_returns_once_the_player_has_let_go_of_the_file() {
        let shared = Arc::new(Mutex::new(Shared { stop_delay_ms: 200, ..Default::default() }));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        svc.play(7, PathBuf::from("a.flac"), 0);
        wait_for(&rx, |s| s.state == PlayState::Playing);
        assert!(svc.stop_and_wait(std::time::Duration::from_secs(5)));
        assert!(shared.lock().unwrap().stopped_since_load, "the device was stopped before stop_and_wait returned");
        assert_eq!(svc.current_track(), None);
    }

    #[test]
    fn stop_and_wait_gives_up_after_the_timeout() {
        let shared = Arc::new(Mutex::new(Shared { stop_delay_ms: 1_000, ..Default::default() }));
        let (svc, rx) = service(shared.clone(), Arc::new(Mutex::new(false)));
        svc.play(7, PathBuf::from("a.flac"), 0);
        wait_for(&rx, |s| s.state == PlayState::Playing);
        let t = std::time::Instant::now();
        assert!(!svc.stop_and_wait(std::time::Duration::from_millis(50)));
        assert!(t.elapsed() < std::time::Duration::from_millis(900));
    }

    #[test]
    fn status_serializes_with_snake_case_state() {
        let v = serde_json::to_value(PlayerStatus { state: PlayState::Ended, track_id: Some(3), position_ms: 5, duration_ms: 9, error: None }).unwrap();
        assert_eq!(v["state"], "ended");
        assert_eq!(v["track_id"], 3);
    }
}
