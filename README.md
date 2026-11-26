# HeckTunes

A small desktop app that acts as iTunes for a Snowsky Echo Mini running [EchoPod](https://github.com/BasilSkyWalk/echopod) firmware or its fork [HeckPod](https://github.com/King-of-Heck/heckpod). The app calls the player **HeckPod**.

- Drag albums onto the window: HeckTunes copies them into its library folder and fixes their tags and album art (MusicBrainz, Cover Art Archive, optionally AcoustID). Your original files are never changed.
- Browse by album, artist or song; make playlists; preview tracks, with a volume slider and mute.
- Import vinyl recordings: record a whole side as one WAV or MP3 file, and HeckTunes finds the tracks, identifies the record, and lets you check every cut before importing (**Tools → Choose recording…**).
- Tick albums, artists and playlists for the player and sync them to its microSD card. HeckTunes only ever changes files it put there, and runs echo-sync afterwards so the player doesn't have to rescan.
- Fit more on the card: **Convert to AAC** on a FLAC album makes 256 kbps AAC (.m4a) copies beside the FLAC files, and the album's **Echo Mini gets FLAC | AAC** switch chooses which ones sync. The FLAC files stay, and tag or cover edits reach both.

## Install

Download the installer for your computer from the [Releases](../../releases) page:

- **Windows:** `HeckTunes_x.y.z_x64_en-US.msi`. The build isn't signed, so Windows SmartScreen may warn you: choose **More info → Run anyway**.
- **macOS (Apple silicon or Intel):** the matching `.dmg`. The build isn't signed, so the first time **right-click the app → Open → Open**. On Intel Macs echo-sync isn't available (EchoPod publishes no Intel build), so the player rescans the card after a sync.

## First run

1. Drop an album folder anywhere in the window.
2. If HeckTunes isn't sure which release it is, it asks you to pick.
3. Open **HeckPod** under **Devices** in the sidebar, choose the microSD card (in a card reader, or the player connected over USB), tick what you want, and press **Sync**. Eject the card safely afterwards.

Untagged albums can be recognised by sound if an AcoustID key is set: get a free one at https://acoustid.org/new-application and paste it in **Settings**.

## Develop

Requirements: Node 24, Rust (stable), and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS.

```bash
npm install          # also fetches echo-sync (checksum-verified)
npm run sidecar      # re-run if the echo-sync download failed
bash scripts/build-ffmpeg.sh   # builds the AAC-only ffmpeg sidecar (see below)
npx tauri dev
```

The ffmpeg sidecar is a small LGPL build of a pinned FFmpeg release with only the FLAC decoder, the AAC encoder and the .m4a muxer. On Windows run the script in an MSYS2 **CLANG64** shell (packages `make diffutils curl xz mingw-w64-clang-x86_64-clang`); on macOS the Xcode command-line tools are enough. The conversion tests use it when it's there and skip it otherwise.

Checks: `npm run check`, `npm run build`, `cd src-tauri && cargo test`. Regenerate `src-tauri/licenses/THIRD_PARTY_NOTICES.md` with `npm run notices` after changing dependencies.

Releases: bump the version in `package.json`, `src-tauri/tauri.conf.json` and `src-tauri/Cargo.toml`, then push a tag like `v1.2.3`; GitHub Actions builds the installers into a draft release to check and publish. Optional repository secret: `HECKTUNES_ACOUSTID_KEY`.

## Licences

echo-sync (bundled as a separate program) is GPLv3, and the bundled FFmpeg build is LGPL-2.1-or-later. Their licences and links to their source are in **About**, along with the licences of the open-source components HeckTunes is built from, and each release page carries both source archives. HeckTunes itself is free software under the GNU General Public License, version 3 or (at your option) any later version (GPL-3.0-or-later); see `LICENSE`.
