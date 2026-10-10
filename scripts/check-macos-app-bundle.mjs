import { execFileSync } from "node:child_process";
import { lstat, mkdtemp, readFile, readdir, readlink, rmdir } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const defaultAppPath = "src-tauri/target/release/bundle/macos/Murmur.app";
const defaultDmgDirectory = "src-tauri/target/release/bundle/dmg";
const forbiddenNames = new Set(["issue-license", "issue_license"]);
const expectedEntitlements = new Set([
  "com.apple.security.automation.apple-events",
  "com.apple.security.device.audio-input",
]);

function serializedPlistKeys(plist) {
  const withoutComments = plist.replace(/<!--[\s\S]*?-->/g, "");
  return [...withoutComments.matchAll(/<key>\s*([^<]+?)\s*<\/key>/g)]
    .map((match) => match[1]);
}

export function validateProductionEntitlements(entitlements, serializedPlist = "") {
  if (!entitlements || Array.isArray(entitlements) || typeof entitlements !== "object") {
    throw new Error("production entitlements must be a property-list dictionary");
  }

  const entries = Object.entries(entitlements);
  const actual = new Set(entries.map(([key]) => key));
  const missing = [...expectedEntitlements].filter((key) => !actual.has(key));
  const unexpected = [...actual].filter((key) => !expectedEntitlements.has(key));
  const invalid = entries
    .filter(([, enabled]) => enabled !== true)
    .map(([key]) => key);
  const serializedKeys = serializedPlist ? serializedPlistKeys(serializedPlist) : [];
  const duplicate = serializedKeys.find(
    (key, index) => serializedKeys.indexOf(key) !== index,
  );

  if (duplicate) {
    throw new Error(`production entitlements contain a duplicate key: ${duplicate}`);
  }
  if (serializedPlist && serializedKeys.length !== entries.length) {
    throw new Error("serialized production entitlements do not match the parsed dictionary");
  }
  if (missing.length || unexpected.length || invalid.length) {
    throw new Error([
      missing.length ? `missing: ${missing.join(", ")}` : "",
      unexpected.length ? `unexpected: ${unexpected.join(", ")}` : "",
      invalid.length ? `not enabled Boolean values: ${invalid.join(", ")}` : "",
    ].filter(Boolean).join("; "));
  }
}

export function validateAppInfo(info) {
  if (info?.CFBundleIdentifier !== "com.monkeyman.murmur") {
    throw new Error("production app bundle identifier must remain com.monkeyman.murmur");
  }
  if (info.LSUIElement !== true) {
    throw new Error("production app must be menu-bar-only (LSUIElement true)");
  }
}

const propertyList = {
  parse(serialized) {
    const json = execFileSync("/usr/bin/plutil", ["-convert", "json", "-o", "-", "--", "-"], {
      input: serialized,
      encoding: "utf8",
      stdio: ["pipe", "pipe", "ignore"],
    });
    return JSON.parse(json);
  },
};

const codesign = {
  verify(pathname) {
    execFileSync("/usr/bin/codesign", ["--verify", "--deep", "--strict", pathname], {
      stdio: "ignore",
    });
  },
  architectures(executablePath) {
    const output = execFileSync("/usr/bin/lipo", ["-archs", executablePath], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    const architectures = output.trim().split(/\s+/).filter(Boolean);
    if (
      architectures.length === 0
      || new Set(architectures).size !== architectures.length
      || architectures.some((architecture) => !/^[A-Za-z0-9_]+$/.test(architecture))
    ) {
      throw new Error("unable to determine a unique set of production architectures");
    }
    return architectures;
  },
  entitlements(executablePath, architecture) {
    return execFileSync(
      "/usr/bin/codesign",
      ["-d", "--architecture", architecture, "--entitlements", "-", "--xml", executablePath],
      {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      },
    );
  },
};

async function rejectIssuer(pathname) {
  const name = path.basename(pathname);
  if (forbiddenNames.has(name)) {
    throw new Error("operator license issuer found in production app bundle");
  }

  const metadata = await lstat(pathname);
  if (metadata.isSymbolicLink()) {
    const targetParts = path.normalize(await readlink(pathname)).split(path.sep);
    if (targetParts.some((part) => forbiddenNames.has(part))) {
      throw new Error("production app symlink targets the operator license issuer");
    }
    return;
  }
  if (!metadata.isDirectory()) return;

  for (const child of await readdir(pathname)) {
    await rejectIssuer(path.join(pathname, child));
  }
}

async function requireRealDirectory(pathname, description) {
  const metadata = await lstat(pathname);
  if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
    throw new Error(`${description} must be a real directory, not a symlink`);
  }
}

export async function checkApp(
  appPath,
  {
    verifySignatureEntitlements = false,
    codeSignature = codesign,
    plist = propertyList,
  } = {},
) {
  const contentsDirectory = path.join(appPath, "Contents");
  const executableDirectory = path.join(contentsDirectory, "MacOS");
  await requireRealDirectory(appPath, "production app");
  await requireRealDirectory(contentsDirectory, "production app Contents");
  await requireRealDirectory(executableDirectory, "production app executable directory");

  const entries = (await readdir(executableDirectory)).sort();
  const expected = ["murmur"];
  if (JSON.stringify(entries) !== JSON.stringify(expected)) {
    throw new Error(
      `unexpected production app executables: expected ${expected.join(", ")}; found ${entries.join(", ") || "none"}`,
    );
  }

  const executable = await lstat(path.join(executableDirectory, "murmur"));
  if (executable.isSymbolicLink() || !executable.isFile() || (executable.mode & 0o111) === 0) {
    throw new Error("production app main executable is missing, not executable, or a symlink");
  }

  await rejectIssuer(appPath);

  const noticesPath = path.join(
    contentsDirectory,
    "Resources",
    "resources",
    "THIRD-PARTY-NOTICES.txt",
  );
  const notices = await lstat(noticesPath);
  if (notices.isSymbolicLink() || !notices.isFile() || notices.size < 1_000) {
    throw new Error("production app is missing its bundled third-party notices");
  }
  const noticeHeader = (await readFile(noticesPath, "utf8")).slice(0, 256);
  if (!noticeHeader.startsWith("Murmur Third-Party Notices")) {
    throw new Error("production app third-party notices have unexpected content");
  }
  const noticesInBundle = await readFile(noticesPath);
  const sourceNotices = await readFile(path.resolve("src-tauri/resources/THIRD-PARTY-NOTICES.txt"));
  if (!noticesInBundle.equals(sourceNotices)) {
    throw new Error("production app third-party notices do not match source notices");
  }

  const licensePath = path.join(contentsDirectory, "Resources", "LICENSE");
  const license = await lstat(licensePath);
  if (license.isSymbolicLink() || !license.isFile()) {
    throw new Error("production app is missing its bundled Murmur MIT license");
  }
  if ((await readFile(licensePath, "utf8")) !== (await readFile(path.resolve("LICENSE"), "utf8"))) {
    throw new Error("production app Murmur license does not match source LICENSE");
  }

  // Real bundle audits run on macOS. Tests on other hosts inject a parser for
  // synthetic Info.plist fixtures without weakening the production plist read.
  validateAppInfo(plist.parse(await readFile(path.join(contentsDirectory, "Info.plist"))));

  if (verifySignatureEntitlements) {
    codeSignature.verify(appPath);
    for (const architecture of codeSignature.architectures(
      path.join(executableDirectory, "murmur"),
    )) {
      const serialized = codeSignature.entitlements(
        path.join(executableDirectory, "murmur"),
        architecture,
      );
      validateProductionEntitlements(plist.parse(serialized), serialized);
    }
  }
}

async function onlyDmg(inputPath) {
  const input = await lstat(inputPath);
  if (input.isSymbolicLink()) {
    throw new Error("DMG audit input must not be a symlink");
  }
  if (input.isFile()) return inputPath;
  if (!input.isDirectory()) {
    throw new Error("DMG audit input must be a file or directory");
  }

  const images = (await readdir(inputPath))
    .filter((name) => name.toLowerCase().endsWith(".dmg"));
  if (images.length !== 1) {
    throw new Error(`expected exactly one DMG to audit; found ${images.length}`);
  }
  const dmgPath = path.join(inputPath, images[0]);
  const image = await lstat(dmgPath);
  if (image.isSymbolicLink() || !image.isFile()) {
    throw new Error("DMG audit input must be a real file, not a symlink");
  }
  return dmgPath;
}

const hdiutil = {
  attach(dmgPath, mountPoint) {
    execFileSync("/usr/bin/hdiutil", [
      "attach", "-readonly", "-nobrowse", "-mountpoint", mountPoint, dmgPath,
    ], { stdio: "ignore" });
  },
  detach(mountPoint, force = false) {
    execFileSync("/usr/bin/hdiutil", ["detach", ...(force ? ["-force"] : []), mountPoint], {
      stdio: "ignore",
    });
  },
};

function combinedError(primaryError, cleanupErrors) {
  const errors = primaryError ? [primaryError, ...cleanupErrors] : cleanupErrors;
  if (errors.length === 1) return errors[0];
  return new AggregateError(errors, "DMG audit and cleanup failed");
}

export async function checkDmg(
  inputPath,
  {
    platform = process.platform,
    diskImage = hdiutil,
    verifySignatureEntitlements = false,
    codeSignature = codesign,
    plist = propertyList,
  } = {},
) {
  if (platform !== "darwin") {
    throw new Error("DMG inspection requires macOS");
  }
  const dmgPath = await onlyDmg(inputPath);
  if (verifySignatureEntitlements) {
    codeSignature.verify(dmgPath);
  }
  const mountPoint = await mkdtemp(path.join(os.tmpdir(), "murmur-dmg-audit-"));
  let mounted = false;
  let primaryError;
  try {
    diskImage.attach(dmgPath, mountPoint);
    mounted = true;
    const apps = (await readdir(mountPoint)).filter((name) => name.endsWith(".app"));
    if (apps.length !== 1) {
      throw new Error(`expected exactly one app in DMG; found ${apps.length}`);
    }
    const appPath = path.join(mountPoint, apps[0]);
    await requireRealDirectory(appPath, "DMG app entry");
    await checkApp(appPath, {
      verifySignatureEntitlements,
      codeSignature,
      plist,
    });
    await rejectIssuer(mountPoint);
  } catch (error) {
    primaryError = error;
  }

  const cleanupErrors = [];
  if (mounted) {
    try {
      diskImage.detach(mountPoint);
    } catch (initialDetachError) {
      try {
        diskImage.detach(mountPoint, true);
      } catch (forcedDetachError) {
        cleanupErrors.push(new AggregateError(
          [initialDetachError, forcedDetachError],
          "unable to detach audited DMG",
        ));
      }
    }
  }
  try {
    await rmdir(mountPoint);
  } catch (error) {
    cleanupErrors.push(error);
  }

  if (primaryError || cleanupErrors.length > 0) {
    throw combinedError(primaryError, cleanupErrors);
  }
}

export function parseCliArguments(args) {
  if (args.length === 0) {
    return { kind: "app", inputPath: defaultAppPath, verifySignatureEntitlements: false };
  }
  if (args.length === 1 && !args[0].startsWith("--")) {
    return { kind: "app", inputPath: args[0], verifySignatureEntitlements: false };
  }

  const modes = new Map([
    ["--dmg", { kind: "dmg", defaultPath: defaultDmgDirectory, verify: false }],
    ["--verify-app-entitlements", { kind: "app", defaultPath: defaultAppPath, verify: true }],
    ["--verify-dmg-entitlements", { kind: "dmg", defaultPath: defaultDmgDirectory, verify: true }],
  ]);
  const selected = modes.get(args[0]);
  if (!selected) {
    throw new Error(`unknown macOS bundle audit option: ${args[0]}`);
  }
  if (args.length > 2) {
    throw new Error(`${args[0]} accepts at most one path`);
  }
  if (args[1]?.startsWith("--")) {
    throw new Error(`${args[0]} received an option where a path was expected`);
  }
  return {
    kind: selected.kind,
    inputPath: args[1] ?? selected.defaultPath,
    verifySignatureEntitlements: selected.verify,
  };
}

const invokedPath = process.argv[1] ? pathToFileURL(path.resolve(process.argv[1])).href : "";
if (import.meta.url === invokedPath) {
  const selection = parseCliArguments(process.argv.slice(2));
  if (selection.kind === "dmg") {
    await checkDmg(selection.inputPath, {
      verifySignatureEntitlements: selection.verifySignatureEntitlements,
    });
    console.log(selection.verifySignatureEntitlements
      ? "DMG and contained app signature integrity passed; every app architecture has the exact approved entitlements."
      : "Production DMG contains only the Murmur executable and no license issuer.");
  } else {
    await checkApp(selection.inputPath, {
      verifySignatureEntitlements: selection.verifySignatureEntitlements,
    });
    console.log(selection.verifySignatureEntitlements
      ? "App signature integrity passed; every architecture has the exact approved entitlements."
      : "Production app bundle contains only the Murmur executable and no license issuer.");
  }
}
