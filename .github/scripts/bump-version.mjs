#!/usr/bin/env node
/**
 * Version alignment helper.
 *
 * Cargo.toml is the single source of truth for the app version. This script
 * syncs `package.json` and `src-tauri/tauri.conf.json` to match it, and can
 * verify a git tag matches (used as the first CI step of release.yml).
 *
 * Usage:
 *   node .github/scripts/bump-version.mjs            # sync package.json + tauri.conf.json
 *   node .github/scripts/bump-version.mjs --check v0.1.1   # assert all sources == 0.1.1
 */
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = dirname(dirname(dirname(fileURLToPath(import.meta.url))));
const cargoToml = readFileSync(join(root, "src-tauri/Cargo.toml"), "utf8");
const m = cargoToml.match(/^version\s*=\s*"([^"]+)"/m);
if (!m) {
  console.error("bump-version: could not read version from Cargo.toml");
  process.exit(1);
}
const version = m[1];

const checkMode = process.argv.includes("--check");
const expected = checkMode
  ? process.argv[process.argv.indexOf("--check") + 1]?.replace(/^v/, "")
  : null;

function syncJson(rel, mutate) {
  const path = join(root, rel);
  const obj = JSON.parse(readFileSync(path, "utf8"));
  mutate(obj);
  writeFileSync(path, JSON.stringify(obj, null, 2) + "\n", "utf8");
}

function jsonVersion(rel) {
  return JSON.parse(readFileSync(join(root, rel), "utf8")).version;
}

if (checkMode) {
  const pkg = jsonVersion("package.json");
  const conf = jsonVersion("src-tauri/tauri.conf.json");
  const problems = [];
  if (version !== expected) problems.push(`Cargo.toml ${version} != tag ${expected}`);
  if (pkg !== expected) problems.push(`package.json ${pkg} != tag ${expected}`);
  if (conf !== expected) problems.push(`tauri.conf.json ${conf} != tag ${expected}`);
  if (problems.length) {
    console.error("bump-version: version mismatch:");
    for (const p of problems) console.error("  " + p);
    process.exit(1);
  }
  console.log(`bump-version: all sources consistent at ${version}`);
  process.exit(0);
}

// Sync mode: write the Cargo.toml version into the two build-manifest files.
syncJson("package.json", (o) => {
  o.version = version;
});
syncJson("src-tauri/tauri.conf.json", (o) => {
  o.version = version;
});
console.log(`bump-version: synced package.json + tauri.conf.json to ${version}`);
