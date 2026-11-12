#!/usr/bin/env node
// Writes src-tauri/licenses/THIRD_PARTY_NOTICES.md: the Rust crates compiled into HeckTunes
// (normal dependencies only, for one target) and the npm packages whose code ships in the UI.
// Usage: node scripts/third-party-notices.mjs [--target <triple>]
import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const at = args.indexOf('--target');
if (at >= 0 && !args[at + 1]) throw new Error('--target needs a value');

function hostTriple() {
  const line = execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).split('\n').find((l) => l.startsWith('host:'));
  return line.slice('host:'.length).trim();
}
const target = at >= 0 ? args[at + 1] : hostTriple();

/** Licence-ish files at the top of a package folder. */
function licenceTexts(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((f) => /^(licen[cs]e|copying|notice)/i.test(f))
    .sort()
    .map((f) => readFileSync(join(dir, f), 'utf8').replace(/\r\n/g, '\n').trim())
    .filter(Boolean);
}

// ---- Fallback: standard licence texts for packages that ship no licence file ----
// scripts/licence-templates/ holds verbatim copies of the standard texts, taken from crates.io sources:
//   MIT.txt          serde 1.0.229 LICENSE-MIT
//   Apache-2.0.txt   serde 1.0.229 LICENSE-APACHE
//   MPL-2.0.txt      symphonia 0.6.1 LICENSE
//   BSD-3-Clause.txt brotli 8.0.4 LICENSE.BSD-3-Clause (without its copyright lines)
// MIT and BSD texts get a "Copyright (c) <authors>" line (or "the <name> contributors") prepended.
const templateDir = join(root, 'scripts', 'licence-templates');
const templateFiles = { MIT: 'MIT.txt', 'BSD-3-Clause': 'BSD-3-Clause.txt', 'Apache-2.0': 'Apache-2.0.txt', 'MPL-2.0': 'MPL-2.0.txt' };
const needsCopyright = new Set(['MIT', 'BSD-3-Clause']);
function template(id) {
  return readFileSync(join(templateDir, templateFiles[id]), 'utf8').replace(/\r\n/g, '\n').trim();
}
/** Licence ids to include for an SPDX expression: one of "A OR B" (MIT preferred), each of "A AND B" / "A WITH B". */
function pickLicences(expr) {
  const alternatives = expr.replace(/[()]/g, ' ').split(/\s+OR\s+|\//).map((a) => a.trim()).filter(Boolean);
  const usable = alternatives
    .map((a) => a.split(/\s+(?:AND|WITH)\s+/).map((t) => t.trim()))
    .filter((ids) => ids.every((id) => id in templateFiles));
  if (!usable.length) return [];
  return usable.find((ids) => ids.includes('MIT')) ?? usable[0];
}
/** Texts for a package with no licence file of its own. */
function templateTexts(license, holders, name) {
  const who = holders.length ? holders.join(', ') : `the ${name} contributors`;
  return pickLicences(license).map((id) => {
    const text = template(id);
    return needsCopyright.has(id) && !/^\s*copyright/im.test(text) ? `Copyright (c) ${who}\n\n${text}` : text;
  });
}
const stripEmail = (a) => a.replace(/\s*<[^>]*>/g, '').replace(/\s*\([^)]*\)/g, '').trim();
const withFallback = (texts, license, holders, name) => (texts.length ? texts : templateTexts(license, holders, name));

// ---- Rust: walk normal dependencies from the app crate ----
const meta = JSON.parse(
  execFileSync('cargo', ['metadata', '--format-version', '1', '--manifest-path', join(root, 'src-tauri', 'Cargo.toml'), '--filter-platform', target], {
    encoding: 'utf8',
    maxBuffer: 256 * 1024 * 1024,
  }),
);
const pkgs = new Map(meta.packages.map((p) => [p.id, p]));
const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
const rootId = meta.resolve.root;
const seen = new Set();
const stack = [rootId];
while (stack.length) {
  const id = stack.pop();
  if (seen.has(id)) continue;
  seen.add(id);
  for (const dep of nodes.get(id)?.deps ?? []) {
    if (dep.dep_kinds.some((k) => k.kind === null)) stack.push(dep.pkg);
  }
}
seen.delete(rootId);
const crates = [...seen]
  .map((id) => pkgs.get(id))
  .map((p) => ({
    name: p.name,
    version: p.version,
    license: p.license ?? (p.license_file ? `see ${p.license_file}` : 'unknown'),
    source: p.repository ?? p.homepage ?? `https://crates.io/crates/${p.name}`,
    texts: withFallback(licenceTexts(dirname(p.manifest_path)), p.license ?? '', (p.authors ?? []).map(stripEmail).filter(Boolean), p.name),
  }))
  .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));

// ---- npm: production dependencies (transitively) plus Svelte's runtime, which Vite bundles ----
const appPkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
const npmQueue = [...Object.keys(appPkg.dependencies ?? {}), 'svelte'];
const npmSeen = new Map();
while (npmQueue.length) {
  const name = npmQueue.pop();
  if (npmSeen.has(name)) continue;
  const dir = join(root, 'node_modules', ...name.split('/'));
  const pj = join(dir, 'package.json');
  if (!existsSync(pj)) continue;
  const p = JSON.parse(readFileSync(pj, 'utf8'));
  const repo = typeof p.repository === 'string' ? p.repository : p.repository?.url;
  const license = typeof p.license === 'string' ? p.license : p.license?.type ?? 'unknown';
  const people = [p.author, ...(p.contributors ?? [])].filter(Boolean).map((a) => stripEmail(typeof a === 'string' ? a : a.name ?? ''));
  npmSeen.set(name, {
    name,
    version: p.version,
    license,
    source: repo ?? p.homepage ?? `https://www.npmjs.com/package/${name}`,
    texts: withFallback(licenceTexts(dir), license, people.filter(Boolean), name),
  });
  npmQueue.push(...Object.keys(p.dependencies ?? {}));
}
const npm = [...npmSeen.values()].sort((a, b) => a.name.localeCompare(b.name));

// ---- write ----
const all = [...crates.map((c) => ({ ...c, kind: 'crate' })), ...npm.map((n) => ({ ...n, kind: 'npm' }))];
const missing = all.filter((c) => c.texts.length === 0);
if (missing.length) {
  console.error(`error: ${missing.length} of ${all.length} shipped packages have no licence text:`);
  for (const c of missing) console.error(`  ${c.kind} ${c.name} ${c.version} (${c.license})`);
  process.exit(1);
}
const byText = new Map();
for (const c of all) {
  for (const t of c.texts) {
    if (!byText.has(t)) byText.set(t, []);
    byText.get(t).push(`${c.name} ${c.version}`);
  }
}
const row = (c) => `| ${c.name} | ${c.version} | ${c.license} | ${c.source} |`;
const out = [
  '# Third-party notices',
  '',
  `HeckTunes is built from the open-source components below (Rust crates for ${target}, and the JavaScript bundled into its window). Each is used under the licence shown; the licence texts follow.`,
  '',
  'echo-sync, bundled as a separate program, is covered by its own licence (see echo-sync-LICENSE.txt).',
  '',
  '## Rust crates',
  '',
  '| Crate | Version | Licence | Source |',
  '|---|---|---|---|',
  ...crates.map(row),
  '',
  '## JavaScript packages',
  '',
  '| Package | Version | Licence | Source |',
  '|---|---|---|---|',
  ...npm.map(row),
  '',
  '## Licence texts',
  '',
  ...[...byText.entries()].flatMap(([text, users]) => [`### Used by: ${users.join(', ')}`, '', '```', text, '```', '']),
].join('\n');
mkdirSync(join(root, 'src-tauri', 'licenses'), { recursive: true });
writeFileSync(join(root, 'src-tauri', 'licenses', 'THIRD_PARTY_NOTICES.md'), out + '\n');
console.log(`wrote THIRD_PARTY_NOTICES.md: ${crates.length} crates, ${npm.length} npm packages, ${byText.size} licence texts`);
