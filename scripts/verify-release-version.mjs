#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const rootDirectory = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const stableVersionPattern = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

function fail(message) {
  throw new Error(message);
}

function parseJsonVersion(contents, filePath) {
  let parsed;

  try {
    parsed = JSON.parse(contents);
  } catch (error) {
    fail(`Could not parse ${filePath}: ${error.message}`);
  }

  if (typeof parsed.version !== "string" || parsed.version.length === 0) {
    fail(`${filePath} must contain a non-empty string \"version\" field.`);
  }

  return parsed.version;
}

function parseCargoPackageVersion(contents, filePath) {
  let inPackageSection = false;

  for (const line of contents.split(/\r?\n/)) {
    const section = line.match(/^\s*\[([^\]]+)\]\s*(?:#.*)?$/);

    if (section) {
      inPackageSection = section[1] === "package";
      continue;
    }

    if (!inPackageSection) {
      continue;
    }

    const version = line.match(/^\s*version\s*=\s*"([^"]+)"\s*(?:#.*)?$/);

    if (version) {
      return version[1];
    }
  }

  fail(`${filePath} must define version = \"…\" in its [package] section.`);
}

async function readVersion(relativePath, parser) {
  const absolutePath = path.join(rootDirectory, relativePath);
  let contents;

  try {
    contents = await readFile(absolutePath, "utf8");
  } catch (error) {
    fail(`Could not read ${relativePath}: ${error.message}`);
  }

  return parser(contents, relativePath);
}

async function main() {
  const [tag, ...extraArguments] = process.argv.slice(2);

  if (extraArguments.length > 0 || !tag) {
    fail("Usage: node scripts/verify-release-version.mjs vX.Y.Z");
  }

  const tagMatch = stableVersionPattern.exec(tag);

  if (!tagMatch) {
    fail(
      `Invalid release tag \"${tag}\". Use a stable semantic version in the exact form vX.Y.Z (for example, v0.1.0).`,
    );
  }

  const expectedVersion = tag.slice(1);
  const versions = await Promise.all([
    readVersion("package.json", parseJsonVersion),
    readVersion("src-tauri/tauri.conf.json", parseJsonVersion),
    readVersion("src-tauri/Cargo.toml", parseCargoPackageVersion),
  ]);
  const [packageVersion, tauriVersion, cargoVersion] = versions;
  const mismatches = [
    ["package.json", packageVersion],
    ["src-tauri/tauri.conf.json", tauriVersion],
    ["src-tauri/Cargo.toml [package]", cargoVersion],
  ].filter(([, version]) => version !== expectedVersion);

  if (mismatches.length > 0) {
    const details = mismatches
      .map(([source, version]) => `${source} has \"${version}\"`)
      .join("; ");
    fail(
      `Version mismatch for ${tag}: expected \"${expectedVersion}\", but ${details}.`,
    );
  }

  console.log(
    `Release version check passed: ${tag} matches package.json, src-tauri/tauri.conf.json, and src-tauri/Cargo.toml.`,
  );
}

main().catch((error) => {
  console.error(`Release version check failed: ${error.message}`);
  process.exitCode = 1;
});
