// Generates the MP3/MP2 test fixtures in src-tauri/tests/fixtures/vinyl/.
// The outputs are committed, so tests do NOT need ffmpeg; run this only to regenerate them:
//   node scripts/make-vinyl-mp3-fixtures.mjs
// Each file is a 21 s "vinyl rip": 1 s quiet pink noise, three 5 s tones (440/550/660 Hz)
// separated by 2 s of noise, then 1 s of noise (music 1-20 s, gaps at 6-8 s and 13-15 s).
// cbr.mp3 = 128 kbps stereo, vbr.mp3 = LAME -q:a 4, mono22k.mp3 = MPEG-2 mono 48 kbps,
// layer2.mp2 = 1 s of MPEG Layer II (must be refused).
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = join(root, "src-tauri", "tests", "fixtures", "vinyl");

function findFfmpeg() {
  if (process.env.FFMPEG) return process.env.FFMPEG;
  const probe = spawnSync("ffmpeg", ["-version"], { stdio: "ignore" });
  if (!probe.error && probe.status === 0) return "ffmpeg";
  const local = process.env.LOCALAPPDATA;
  if (local) {
    const pkgs = join(local, "Microsoft", "WinGet", "Packages");
    if (existsSync(pkgs)) {
      for (const p of readdirSync(pkgs).filter((n) => n.startsWith("Gyan.FFmpeg"))) {
        for (const d of readdirSync(join(pkgs, p))) {
          const exe = join(pkgs, p, d, "bin", "ffmpeg.exe");
          if (existsSync(exe)) return exe;
        }
      }
    }
  }
  throw new Error("ffmpeg not found: set FFMPEG, or put it on PATH");
}

const ffmpeg = findFfmpeg();
mkdirSync(outDir, { recursive: true });

function make(file, rate, layout, codec, extra = []) {
  const noise = (d, seed) => ["-f", "lavfi", "-i", `anoisesrc=d=${d}:a=0.003:c=pink:r=${rate}:seed=${seed}`];
  const tone = (f) => ["-f", "lavfi", "-i", `sine=f=${f}:d=5:r=${rate}`];
  const args = [
    "-hide_banner", "-loglevel", "error", "-y",
    ...noise(1, 1), ...tone(440), ...noise(2, 2), ...tone(550), ...noise(2, 3), ...tone(660), ...noise(1, 4),
    "-filter_complex", `[0][1][2][3][4][5][6]concat=n=7:v=0:a=1,aformat=channel_layouts=${layout}`,
    ...codec, ...extra, join(outDir, file),
  ];
  const r = spawnSync(ffmpeg, args, { stdio: "inherit" });
  if (r.status !== 0) throw new Error(`ffmpeg failed for ${file}`);
  console.log("wrote", file);
}

make("cbr.mp3", 44100, "stereo", ["-c:a", "libmp3lame", "-b:a", "128k"]);
make("vbr.mp3", 44100, "stereo", ["-c:a", "libmp3lame", "-q:a", "4"]);
make("mono22k.mp3", 22050, "mono", ["-c:a", "libmp3lame", "-b:a", "48k"]);
make("layer2.mp2", 44100, "stereo", ["-c:a", "mp2", "-b:a", "128k"], ["-t", "1"]);
