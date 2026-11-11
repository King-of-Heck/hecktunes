#!/usr/bin/env node
// Downloads the pinned echo-sync release binary for this build target into src-tauri/binaries/,
// named the way Tauri's externalBin expects (echo-sync-<target-triple>[.exe]), and checks its SHA-256.
// Usage: node scripts/fetch-echo-sync.mjs [--target <triple>] [--soft]
//   --soft  never fail (used by postinstall); print a warning instead.
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const VERSION = 'v0.11.1';
const BASE = `https://github.com/BasilSkyWalk/echopod/releases/download/${VERSION}`;
const ASSETS = {
  'x86_64-pc-windows-msvc': { name: 'echo-sync-windows-x64.exe', sha256: 'f2f1b2ed0c8d5d11f7cd7aaea7e4d0455d6c85ecaf381a17dbb66deebc3268bb' },
  'aarch64-apple-darwin': { name: 'echo-sync-macos-arm64', sha256: '62037dd5c0af6b0fd5a237f021a168fd174091acf25ed26f25d73d3bcf43f50a' },
  'x86_64-unknown-linux-gnu': { name: 'echo-sync-linux-x64', sha256: '15162a8814f4726170767e6f9c7c6cacd0de605bdbe50df1425d94d4c5d4e105' },
};
// HeckTunes reads this exit code as "echo-sync isn't available on this platform".
const NOT_AVAILABLE_EXIT = 3;

const args = process.argv.slice(2);
const soft = args.includes('--soft');
const targetArg = args.includes('--target') ? args[args.indexOf('--target') + 1] : undefined;

function hostTriple() {
  const out = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
  const line = out.split('\n').find((l) => l.startsWith('host:'));
  if (!line) throw new Error('could not read the host target from `rustc -vV`');
  return line.slice('host:'.length).trim();
}

const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

async function main() {
  const triple = targetArg ?? process.env.TAURI_ENV_TARGET_TRIPLE ?? hostTriple();
  const windows = triple.includes('windows');
  const outDir = join(dirname(fileURLToPath(import.meta.url)), '..', 'src-tauri', 'binaries');
  const out = join(outDir, `echo-sync-${triple}${windows ? '.exe' : ''}`);
  mkdirSync(outDir, { recursive: true });

  const asset = ASSETS[triple];
  if (!asset) {
    if (windows) throw new Error(`EchoPod ${VERSION} has no echo-sync build for ${triple}`);
    // No upstream build for this target: ship a stand-in so the app still builds; syncs skip echo-sync.
    writeFileSync(out, `#!/bin/sh\necho "echo-sync ${VERSION} has no build for ${triple}" >&2\nexit ${NOT_AVAILABLE_EXIT}\n`);
    chmodSync(out, 0o755);
    console.warn(`echo-sync: no ${VERSION} build for ${triple}; wrote a stand-in.`);
    return;
  }
  if (existsSync(out) && sha256(readFileSync(out)) === asset.sha256) {
    console.log(`echo-sync ${VERSION} already present for ${triple}`);
    return;
  }
  const res = await fetch(`${BASE}/${asset.name}`);
  if (!res.ok) throw new Error(`download failed: HTTP ${res.status} for ${asset.name}`);
  const buf = Buffer.from(await res.arrayBuffer());
  const got = sha256(buf);
  if (got !== asset.sha256) throw new Error(`checksum mismatch for ${asset.name}: expected ${asset.sha256}, got ${got}`);
  writeFileSync(out, buf);
  if (!windows) chmodSync(out, 0o755);
  console.log(`echo-sync ${VERSION} saved to ${out}`);
}

main().catch((e) => {
  console.error(`echo-sync: ${e.message}`);
  if (soft) console.error('echo-sync: continuing without it; run `npm run sidecar` before building.');
  process.exit(soft ? 0 : 1);
});
