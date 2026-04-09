#!/usr/bin/env node

import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");
const manifestPath = path.join(repoRoot, "manifest.json");
const siteReleasePath = path.join(repoRoot, "site", "lib", "release.ts");
const checkOnly = process.argv.includes("--check");

const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const version = manifest.version;

if (!version || typeof version !== "string") {
  throw new Error(`manifest.json is missing a valid string version: ${version}`);
}

const nextContent = `// Generated from manifest.json by scripts/sync_site_release_version.mjs.
// Do not edit by hand.

export const MINUTES_RELEASE_VERSION = "${version}";
export const MINUTES_RELEASE_TAG = \`v\${MINUTES_RELEASE_VERSION}\`;

export const APPLE_SILICON_DMG =
  \`https://github.com/silverstein/minutes/releases/download/\${MINUTES_RELEASE_TAG}/Minutes_\${MINUTES_RELEASE_VERSION}_aarch64.dmg\`;

export const WINDOWS_SETUP_EXE =
  \`https://github.com/silverstein/minutes/releases/download/\${MINUTES_RELEASE_TAG}/minutes-desktop-windows-x64-setup.exe\`;
`;

let currentContent = "";
try {
  currentContent = await readFile(siteReleasePath, "utf8");
} catch (error) {
  if (checkOnly) {
    throw new Error(`Missing ${siteReleasePath}. Run node scripts/sync_site_release_version.mjs`);
  }
}

if (currentContent === nextContent) {
  console.log(`site release constants already match manifest version ${version}`);
  process.exit(0);
}

if (checkOnly) {
  console.error(
    [
      "site release constants are out of sync with manifest.json",
      `manifest version: ${version}`,
      `target file: ${path.relative(repoRoot, siteReleasePath)}`,
      "run: node scripts/sync_site_release_version.mjs",
    ].join("\n"),
  );
  process.exit(1);
}

await writeFile(siteReleasePath, nextContent, "utf8");
console.log(`updated ${path.relative(repoRoot, siteReleasePath)} to version ${version}`);
