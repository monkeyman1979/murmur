#!/usr/bin/env node
// Package a notarized, stapled Murmur.app as a signed in-app update.
//
//   corepack pnpm release:update-package --app /path/to/Murmur.app [--out dir] [--key path]
//
// Run this on the same app that goes into the release DMG, after signing,
// notarization and stapling. Tauri's createUpdaterArtifacts stays off because
// it would archive the unsigned build before notarization. The script writes
// Murmur_<version>_aarch64.app.tar.gz, its .sig and latest.json, and verifies
// the signature against the public key the shipped app trusts.

import { createHash, createPublicKey, verify } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseCargoPackageVersion, validateReleaseMetadata } from "./release-metadata.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REPOSITORY = "monkeyman1979/murmur";
const PLATFORM = "darwin-aarch64";
const KEYCHAIN_SERVICE = "murmur-updater-key";
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

export function updateArchiveName(version) {
  return `Murmur_${version}_aarch64.app.tar.gz`;
}

export function releaseAssetUrl(version, name) {
  return `https://github.com/${REPOSITORY}/releases/download/v${version}/${name}`;
}

/** The body of `## [version]` in CHANGELOG.md, up to the next release heading. */
export function changelogNotes(changelog, version) {
  const lines = changelog.split(/\r?\n/);
  const start = lines.findIndex((line) => line.startsWith(`## [${version}]`));
  if (start === -1) return null;
  const end = lines.findIndex((line, index) => index > start && /^## \[/.test(line));
  const body = lines.slice(start + 1, end === -1 ? undefined : end).join("\n").trim();
  return body || null;
}

// Minisign keys and signatures as Tauri stores them: base64 of a text file
// whose second line is base64 of [algorithm(2) | key id(8) | payload].
function minisignLine(encoded, label) {
  const text = Buffer.from(encoded.trim(), "base64").toString("utf8");
  const line = text.split(/\r?\n/)[1];
  if (!line) throw new Error(`${label} is not in minisign format.`);
  return Buffer.from(line, "base64");
}

export function verifyUpdateSignature(file, signature, publicKey) {
  const key = minisignLine(publicKey, "Updater public key");
  const sig = minisignLine(signature, "Update signature");
  if (key.length !== 42 || sig.length !== 74) throw new Error("Unexpected minisign key or signature length.");
  if (!key.subarray(2, 10).equals(sig.subarray(2, 10))) {
    throw new Error("Update was signed with a different key than the app trusts.");
  }
  const algorithm = sig.subarray(0, 2).toString("ascii");
  // "ED" signs a BLAKE2b-512 digest of the file; legacy "Ed" signs the file itself.
  const message = algorithm === "ED" ? createHash("blake2b512").update(file).digest()
    : algorithm === "Ed" ? file
    : null;
  if (!message) throw new Error(`Unsupported minisign algorithm ${algorithm}.`);
  const spki = createPublicKey({ key: Buffer.concat([ED25519_SPKI_PREFIX, key.subarray(10)]), format: "der", type: "spki" });
  if (!verify(null, message, spki, sig.subarray(10))) throw new Error("Update signature does not verify.");
}

export function updateManifest({ version, notes, signature, url, publishedAt }) {
  return {
    version,
    notes,
    pub_date: publishedAt.toISOString(),
    platforms: { [PLATFORM]: { signature: signature.trim(), url } },
  };
}

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!["--app", "--out", "--key"].includes(flag) || !value) {
      throw new Error("Usage: package-update --app <Murmur.app> [--out <dir>] [--key <private key path>]");
    }
    options[flag.slice(2)] = resolve(value);
  }
  if (!options.app) throw new Error("Pass --app with the notarized, stapled Murmur.app.");
  return options;
}

/**
 * The updater key password lives in the login keychain, so signing needs no
 * typed password; macOS may ask the owner to allow access.
 */
function keychainPassword() {
  try {
    return execFileSync("/usr/bin/security", ["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).replace(/\n$/, "");
  } catch {
    throw new Error(`No "${KEYCHAIN_SERVICE}" keychain item. Set TAURI_SIGNING_PRIVATE_KEY_PASSWORD or add the password to the keychain.`);
  }
}

function plistValue(app, key) {
  return execFileSync("/usr/bin/plutil", ["-extract", key, "raw", "-o", "-", join(app, "Contents", "Info.plist")], { encoding: "utf8" }).trim();
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const packageJson = JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8"));
  const cargoToml = readFileSync(join(ROOT, "src-tauri", "Cargo.toml"), "utf8");
  const tauriConfig = JSON.parse(readFileSync(join(ROOT, "src-tauri", "tauri.conf.json"), "utf8"));
  const { errors } = validateReleaseMetadata({ packageJson, cargoToml, tauriConfig, release: true });
  if (errors.length) throw new Error(errors.join("\n"));
  const version = parseCargoPackageVersion(cargoToml);

  const app = options.app;
  if (basename(app) !== `${tauriConfig.productName}.app`) throw new Error(`Expected ${tauriConfig.productName}.app, got ${basename(app)}.`);
  if (plistValue(app, "CFBundleShortVersionString") !== version) throw new Error(`${basename(app)} is not version ${version}.`);
  if (plistValue(app, "CFBundleIdentifier") !== tauriConfig.identifier) throw new Error(`${basename(app)} has the wrong bundle identifier.`);
  execFileSync("/usr/bin/codesign", ["--verify", "--deep", "--strict", app], { stdio: "inherit" });
  execFileSync("/usr/bin/xcrun", ["stapler", "validate", app], { stdio: "inherit" });

  const notes = changelogNotes(readFileSync(join(ROOT, "CHANGELOG.md"), "utf8"), version);
  if (!notes) throw new Error(`CHANGELOG.md has no notes for ${version}.`);

  const out = options.out ?? join(ROOT, "src-tauri", "target", "release", "update", version);
  mkdirSync(out, { recursive: true });
  const archive = join(out, updateArchiveName(version));
  execFileSync("/usr/bin/tar", ["-czf", archive, "-C", dirname(app), basename(app)], {
    env: { ...process.env, COPYFILE_DISABLE: "1" },
    stdio: "inherit",
  });

  const key = options.key ?? process.env.TAURI_SIGNING_PRIVATE_KEY_PATH ?? join(homedir(), ".tauri", "murmur-updater.key");
  if (!existsSync(key)) throw new Error(`Updater private key not found at ${key}.`);
  // An inline key in the environment would compete with the key file.
  const env = { ...process.env };
  delete env.TAURI_SIGNING_PRIVATE_KEY;
  if (env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD === undefined) {
    env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = keychainPassword();
  }
  const signed = spawnSync("corepack", ["pnpm", "--silent", "tauri", "signer", "sign", "--private-key-path", key, archive], {
    cwd: ROOT,
    stdio: "inherit",
    env,
  });
  if (signed.status !== 0) throw new Error("Signing the update archive failed.");

  const signature = readFileSync(`${archive}.sig`, "utf8");
  verifyUpdateSignature(readFileSync(archive), signature, tauriConfig.plugins.updater.pubkey);

  const manifest = updateManifest({
    version,
    notes,
    signature,
    url: releaseAssetUrl(version, updateArchiveName(version)),
    publishedAt: new Date(),
  });
  writeFileSync(join(out, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`);

  console.log(`\nSigned update for ${version} written to ${out}`);
  console.log(`Upload with:\n  gh release upload v${version} "${archive}" "${join(out, "latest.json")}"`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  }
}
