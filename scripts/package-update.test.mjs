import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, randomBytes, sign } from "node:crypto";
import test from "node:test";
import {
  changelogNotes,
  releaseAssetUrl,
  updateArchiveName,
  updateManifest,
  verifyUpdateSignature,
} from "./package-update.mjs";

// Encode a key or signature the way Tauri stores minisign files.
function minisignFile(comment, payload, extra = "") {
  return Buffer.from(`untrusted comment: ${comment}\n${payload.toString("base64")}\n${extra}`).toString("base64");
}

function testKeys(keyId = randomBytes(8)) {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const raw = Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url");
  return { keyId, privateKey, publicKey: minisignFile("test key", Buffer.concat([Buffer.from("Ed"), keyId, raw])) };
}

function signUpdate(file, { keyId, privateKey }) {
  const digest = createHash("blake2b512").update(file).digest();
  const signature = sign(null, digest, privateKey);
  return minisignFile("test signature", Buffer.concat([Buffer.from("ED"), keyId, signature]), "trusted comment: test\nAAAA\n");
}

test("names the update archive and its release URL by version", () => {
  assert.equal(updateArchiveName("0.1.2"), "Murmur_0.1.2_aarch64.app.tar.gz");
  assert.equal(
    releaseAssetUrl("0.1.2", "Murmur_0.1.2_aarch64.app.tar.gz"),
    "https://github.com/monkeyman1979/murmur/releases/download/v0.1.2/Murmur_0.1.2_aarch64.app.tar.gz",
  );
});

test("takes release notes from the matching changelog section only", () => {
  const changelog = "# Changelog\n\n## [0.1.2] - 2026-10-16\n\n### Changed\n\n- New thing.\n\n## [0.1.1] - 2026-10-14\n\n- Old thing.\n";
  assert.equal(changelogNotes(changelog, "0.1.2"), "### Changed\n\n- New thing.");
  assert.equal(changelogNotes(changelog, "0.1.1"), "- Old thing.");
  assert.equal(changelogNotes(changelog, "0.1.3"), null);
});

test("builds a darwin-aarch64 latest.json entry", () => {
  const manifest = updateManifest({
    version: "0.1.2",
    notes: "- New thing.",
    signature: "c2ln\n",
    url: "https://example.invalid/update.tar.gz",
    publishedAt: new Date("2026-10-16T12:00:00Z"),
  });
  assert.deepEqual(manifest, {
    version: "0.1.2",
    notes: "- New thing.",
    pub_date: "2026-10-16T12:00:00.000Z",
    platforms: { "darwin-aarch64": { signature: "c2ln", url: "https://example.invalid/update.tar.gz" } },
  });
});

test("accepts an update signed by the trusted key", () => {
  const keys = testKeys();
  const file = Buffer.from("update archive");
  assert.doesNotThrow(() => verifyUpdateSignature(file, signUpdate(file, keys), keys.publicKey));
});

test("rejects a modified archive", () => {
  const keys = testKeys();
  const signature = signUpdate(Buffer.from("update archive"), keys);
  assert.throws(() => verifyUpdateSignature(Buffer.from("tampered archive"), signature, keys.publicKey), /does not verify/);
});

test("rejects an archive signed by a different key", () => {
  const trusted = testKeys();
  const other = testKeys();
  const file = Buffer.from("update archive");
  assert.throws(() => verifyUpdateSignature(file, signUpdate(file, other), trusted.publicKey), /different key/);
});
