#!/usr/bin/env node
/**
 * Assembles the updater `latest.json` for a GitHub Releases release
 * (docs/version-update-design.md §8).
 *
 * Signatures are produced at BUILD time by `tauri build`
 * (`bundle.createUpdaterArtifacts: true`) as `.sig` files next to each update
 * artifact. This script runs AFTER every platform job has uploaded its bundles:
 * it reads each platform's `.sig` content and writes the static manifest the
 * app polls at
 * `https://github.com/hachi7574/CaPilot-IDE/releases/latest/download/latest.json`.
 *
 * Env: TAG (e.g. "v0.1.1"), GH_TOKEN (auto-provided on the runner).
 */
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, rmSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const tag = process.env.TAG;
if (!tag) {
  console.error("make-manifest: TAG env is required (e.g. v0.1.1)");
  process.exit(1);
}
const version = tag.replace(/^v/, "");

// ── Release metadata ────────────────────────────────────────────
let info;
try {
  info = JSON.parse(
    execFileSync(
      "gh",
      ["release", "view", tag, "--json", "assets,publishedAt,body"],
      { encoding: "utf8" }
    )
  );
} catch (e) {
  console.error(`make-manifest: gh release view failed: ${e.message}`);
  process.exit(1);
}
const assetUrl = new Map(info.assets.map((a) => [a.name, a.browser_download_url]));

// Notes: the GitHub release body, falling back to the tag's commit body/subject
// (handy when the release was created by the workflow with no body).
let notes = (info.body || "").trim();
if (!notes) {
  try {
    notes = execFileSync("git", ["log", "-1", "--format=%b", tag], {
      encoding: "utf8",
    }).trim();
  } catch {
    notes = "";
  }
}

// ── Resolve per-platform update artifacts ───────────────────────
// Each platform maps to the exact file the updater client downloads. The
// signature field must be the content of the adjacent `.sig` file.
function firstAsset(...patterns) {
  for (const name of assetUrl.keys()) {
    if (patterns.some((re) => re.test(name))) return name;
  }
  return null;
}

const candidates = {
  "linux-x86_64": [/\.AppImage\.tar\.gz$/, /\.AppImage$/],
  "windows-x86_64": [/-setup\.exe$/, /\.msi$/],
  // Tauri names single-arch macOS updater artifacts without an arch suffix
  // (`CaPilot.app.tar.gz`), so prefer an explicit aarch64 suffix when present,
  // then fall back to the bare name.
  "darwin-aarch64": [/aarch64\.app\.tar\.gz$/, /\.app\.tar\.gz$/],
  "darwin-x86_64": [/x86_64\.app\.tar\.gz$/],
  "linux-aarch64": [/aarch64\.AppImage\.tar\.gz$/, /aarch64\.AppImage$/],
};

const platforms = {};
const sigNames = new Set();
for (const [target, patterns] of Object.entries(candidates)) {
  const artifact = firstAsset(...patterns);
  if (!artifact) continue;
  const sigName = artifact + ".sig";
  if (!assetUrl.has(sigName)) {
    console.warn(`make-manifest: missing ${sigName} for ${target} — skipping`);
    continue;
  }
  sigNames.add(sigName);
  platforms[target] = {
    url: assetUrl.get(artifact),
    signature: "", // filled after downloading the .sig contents
  };
}

if (Object.keys(platforms).length === 0) {
  console.error("make-manifest: no updater artifacts found in the release");
  process.exit(1);
}

// ── Read the build-time signatures ──────────────────────────────
// Download every needed `.sig` once, then attach each platform's artifact to
// its sig content (sig filename = artifact filename + ".sig").
const sigDir = mkdtempSync(join(tmpdir(), "capilot-sigs-"));
try {
  for (const name of sigNames) {
    execFileSync("gh", ["release", "download", tag, "--pattern", name, "--dir", sigDir, "--clobber"]);
  }
  const sigContents = new Map();
  for (const [target, p] of Object.entries(platforms)) {
    const artifactName = p.url.split("/").pop();
    sigContents.set(
      artifactName,
      readFileSync(join(sigDir, artifactName + ".sig"), "utf8").trim()
    );
  }
  for (const p of Object.values(platforms)) {
    const artifactName = p.url.split("/").pop();
    p.signature = sigContents.get(artifactName) ?? "";
  }
} finally {
  rmSync(sigDir, { recursive: true, force: true });
}

// ── Write the manifest ──────────────────────────────────────────
const manifest = {
  version,
  notes,
  pub_date: info.publishedAt,
  platforms,
};
writeFileSync("latest.json", JSON.stringify(manifest, null, 2) + "\n", "utf8");
console.log(`make-manifest: wrote latest.json for ${version} (${Object.keys(platforms).join(", ")})`);
