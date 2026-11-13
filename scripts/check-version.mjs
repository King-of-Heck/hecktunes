// Fails unless a release tag (e.g. v0.2.0) matches the version in package.json, tauri.conf.json
// and Cargo.toml, and that version is one the Windows .msi build accepts.
// Usage: node scripts/check-version.mjs v0.2.0
import { readFileSync } from 'node:fs';

const tag = process.argv[2];
if (!tag) {
  console.error('usage: node scripts/check-version.mjs <tag>');
  process.exit(2);
}

const read = (p) => readFileSync(new URL(`../${p}`, import.meta.url), 'utf8');
const cargoPackage = read('src-tauri/Cargo.toml').split(/^\[/m).find((s) => s.startsWith('package]')) ?? '';
const versions = {
  'package.json': JSON.parse(read('package.json')).version,
  'src-tauri/tauri.conf.json': JSON.parse(read('src-tauri/tauri.conf.json')).version,
  'src-tauri/Cargo.toml': cargoPackage.match(/^version\s*=\s*"([^"]+)"/m)?.[1],
};

const errors = [];
const expected = tag.replace(/^v/, '');
if (!tag.startsWith('v')) errors.push(`tag "${tag}" must start with "v"`);

for (const [file, v] of Object.entries(versions)) {
  if (v !== expected) errors.push(`${file} has version ${v ?? '(none)'}, but the tag is ${tag}`);
}

// Semver, restricted to what WiX (.msi) accepts: major/minor <= 255, patch <= 65535,
// and an optional pre-release that is a single number <= 65535 (e.g. 0.2.0-1, not 0.2.0-beta).
const m = expected.match(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(0|[1-9]\d*))?$/);
if (!m) {
  errors.push(`${expected} is not X.Y.Z or X.Y.Z-N (the .msi build rejects other pre-release labels)`);
} else {
  const [, major, minor, patch, pre] = m.map(Number);
  if (major > 255 || minor > 255) errors.push(`${expected}: major and minor must be <= 255 for the .msi build`);
  if (patch > 65535 || (m[4] !== undefined && pre > 65535)) errors.push(`${expected}: patch and pre-release must be <= 65535 for the .msi build`);
}

if (errors.length) {
  for (const e of errors) console.error(`::error::${e}`);
  process.exit(1);
}
console.log(`Version ${expected} matches ${tag} in all three files.`);
