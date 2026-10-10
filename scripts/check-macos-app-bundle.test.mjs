import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { checkApp, validateAppInfo } from "./check-macos-app-bundle.mjs";

test("app bundle contains the exact source MIT license alongside third-party notices", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "murmur-license-bundle-"));
  const app = path.join(directory, "Murmur.app");
  const contents = path.join(app, "Contents");
  const resources = path.join(contents, "Resources");
  const license = path.join(resources, "LICENSE");
  try {
    await mkdir(path.join(contents, "MacOS"), { recursive: true });
    await mkdir(path.join(resources, "resources"), { recursive: true });
    await writeFile(path.join(contents, "MacOS", "murmur"), "binary", { mode: 0o755 });
    await writeFile(path.join(contents, "Info.plist"), "fixture");
    const noticesPath = path.join(resources, "resources", "THIRD-PARTY-NOTICES.txt");
    const sourceNotices = await readFile(new URL("../src-tauri/resources/THIRD-PARTY-NOTICES.txt", import.meta.url));
    await writeFile(noticesPath, sourceNotices);
    const sourceLicense = await readFile(new URL("../LICENSE", import.meta.url));
    const options = { plist: { parse: () => ({ CFBundleIdentifier: "com.monkeyman.murmur", LSUIElement: true }) } };
    await assert.rejects(checkApp(app, options), /ENOENT/);
    await writeFile(license, "wrong license");
    await assert.rejects(checkApp(app, options), /does not match source LICENSE/);
    await writeFile(license, sourceLicense);
    await writeFile(noticesPath, "Murmur Third-Party Notices\n" + "bad notice\n".repeat(200));
    await assert.rejects(checkApp(app, options), /third-party notices do not match source notices/);
    await writeFile(noticesPath, sourceNotices);
    await assert.doesNotReject(checkApp(app, options));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("menu-bar-only bundle keeps the existing app identity", () => {
  assert.doesNotThrow(() => validateAppInfo({
    CFBundleIdentifier: "com.monkeyman.murmur",
    LSUIElement: true,
  }));
  assert.throws(() => validateAppInfo({
    CFBundleIdentifier: "com.example.murmur",
    LSUIElement: true,
  }), /bundle identifier/);
  assert.throws(() => validateAppInfo({
    CFBundleIdentifier: "com.monkeyman.murmur",
    LSUIElement: false,
  }), /LSUIElement/);
  assert.throws(() => validateAppInfo({
    CFBundleIdentifier: "com.monkeyman.murmur",
  }), /LSUIElement/);
});
