import { execFileSync } from "node:child_process";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { afterEach, describe, expect, it } from "vitest";

interface DiskImage {
  attach: (dmgPath: string, mountPoint: string) => void;
  detach: (mountPoint: string, force?: boolean) => void;
}

interface CodeSignature {
  verify: (pathname: string) => void;
  architectures: (executablePath: string) => string[];
  entitlements: (executablePath: string, architecture: string) => string;
}

interface PropertyList {
  parse: (serialized: string) => Record<string, unknown>;
}

interface AuditModule {
  validateProductionEntitlements: (
    entitlements: Record<string, unknown>,
    serializedPlist?: string,
  ) => void;
  parseCliArguments: (args: string[]) => {
    kind: "app" | "dmg";
    inputPath: string;
    verifySignatureEntitlements: boolean;
  };
  checkApp: (
    appPath: string,
    options?: {
      verifySignatureEntitlements?: boolean;
      codeSignature?: CodeSignature;
      plist?: PropertyList;
    },
  ) => Promise<void>;
  checkDmg: (
    inputPath: string,
    options: {
      platform: string;
      diskImage: DiskImage;
      verifySignatureEntitlements?: boolean;
      codeSignature?: CodeSignature;
      plist?: PropertyList;
    },
  ) => Promise<void>;
}

const exactEntitlementValues = {
  "com.apple.security.device.audio-input": true,
  "com.apple.security.automation.apple-events": true,
  "com.apple.security.personal-information.calendars": true,
};
const exactEntitlements = `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>com.apple.security.device.audio-input</key><true/>
<key>com.apple.security.automation.apple-events</key><true/>
<key>com.apple.security.personal-information.calendars</key><true/>
</dict></plist>`;

function fakeCodeSignature(
  architectures = ["arm64"],
  entitlements: Record<string, string> = { arm64: exactEntitlements },
  verified: string[] = [],
): CodeSignature {
  return {
    verify: (pathname) => { verified.push(pathname); },
    architectures: () => architectures,
    entitlements: (_executablePath, architecture) => entitlements[architecture] ?? "",
  };
}

function fakePlist(values: Record<string, unknown> = exactEntitlementValues): PropertyList {
  return {
    parse: (serialized) => {
      // Test-only parser for the fixed synthetic fixture; production uses
      // macOS plutil to interpret the actual bundled property list.
      const text = String(serialized);
      if (text.includes("<key>CFBundleIdentifier</key>")) {
        return {
          CFBundleIdentifier: text.includes("com.monkeyman.murmur")
            ? "com.monkeyman.murmur" : "com.example.murmur",
          LSUIElement: text.includes("<key>LSUIElement</key><true/>"),
        };
      }
      return values;
    },
  };
}

const roots: string[] = [];

function makeBundle(app: string): string {
  const executables = path.join(app, "Contents", "MacOS");
  mkdirSync(executables, { recursive: true });
  writeFileSync(path.join(app, "Contents", "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.monkeyman.murmur</string>
<key>LSUIElement</key><true/>
</dict></plist>`);
  const executable = path.join(executables, "murmur");
  writeFileSync(executable, "synthetic executable");
  chmodSync(executable, 0o755);
  const resources = path.join(app, "Contents", "Resources", "resources");
  mkdirSync(resources, { recursive: true });
  writeFileSync(path.join(resources, "THIRD-PARTY-NOTICES.txt"),
    readFileSync("src-tauri/resources/THIRD-PARTY-NOTICES.txt"));
  writeFileSync(path.join(app, "Contents", "Resources", "LICENSE"), readFileSync("LICENSE"));
  return app;
}

function fakeApp(): string {
  const root = mkdtempSync(path.join(tmpdir(), "murmur-bundle-audit-"));
  roots.push(root);
  return makeBundle(path.join(root, "Murmur.app"));
}

function fakeDmgDirectory(count = 1): string {
  const root = mkdtempSync(path.join(tmpdir(), "murmur-dmg-audit-input-"));
  roots.push(root);
  for (let index = 0; index < count; index += 1) {
    writeFileSync(path.join(root, `Murmur-${index}.dmg`), "synthetic disk image");
  }
  return root;
}

function clearMount(mountPoint: string): void {
  for (const child of readdirSync(mountPoint)) {
    rmSync(path.join(mountPoint, child), { recursive: true, force: true });
  }
}

function audit(app: string): string {
  return execFileSync(process.execPath, ["scripts/check-macos-app-bundle.mjs", app], {
    cwd: process.cwd(),
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

async function auditModule(): Promise<AuditModule> {
  const script = pathToFileURL(path.resolve("scripts/check-macos-app-bundle.mjs")).href;
  return import(/* @vite-ignore */ script) as Promise<AuditModule>;
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("macOS production app bundle audit", () => {
  it("binds the exact production entitlement policy in the base config", async () => {
    const { validateProductionEntitlements } = await auditModule();
    const config = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
    expect(config.bundle.macOS).toMatchObject({
      hardenedRuntime: true,
      entitlements: "./entitlements.plist",
    });

    for (const overlayPath of [
      "src-tauri/tauri.debug.conf.json",
      "src-tauri/tauri.meeting-local.conf.json",
    ]) {
      const overlay = JSON.parse(readFileSync(overlayPath, "utf8"));
      expect(overlay.bundle.macOS).not.toHaveProperty("hardenedRuntime");
      expect(overlay.bundle.macOS).not.toHaveProperty("entitlements");
    }

    expect(() => validateProductionEntitlements(
      exactEntitlementValues,
      readFileSync("src-tauri/entitlements.plist", "utf8"),
    )).not.toThrow();
    const info = readFileSync("src-tauri/Info.plist", "utf8");
    expect(info).toContain("<key>LSUIElement</key>");
    expect(info).toContain("<true/>");
    expect(info).toContain("<key>NSAppleEventsUsageDescription</key>");
    expect(info).toContain("opens the Privacy &amp; Security pane when you ask");
    expect(info).toContain("Mute system audio while dictating");
    expect(info).toContain("captures your Mac’s system audio");
    expect(info).toContain("configured dictation shortcut (Right ⌘ by default)");
    expect(info).not.toContain("<key>NSScreenCaptureUsageDescription</key>");
  });

  it.skipIf(process.platform !== "darwin")("accepts a bundle with only the main executable and bundled licenses", () => {
    expect(audit(fakeApp())).toContain("no license issuer");
  });

  it("rejects bundles that can disappear from the menu bar or change identity", async () => {
    const { checkApp } = await auditModule();
    const app = fakeApp();
    const infoPath = path.join(app, "Contents", "Info.plist");
    const validInfo = readFileSync(infoPath, "utf8");
    writeFileSync(infoPath, validInfo.replace("<key>LSUIElement</key><true/>", "<key>LSUIElement</key><false/>"));
    await expect(checkApp(app, { plist: fakePlist() })).rejects.toThrow("LSUIElement");
    writeFileSync(infoPath, validInfo.replace("com.monkeyman.murmur", "com.example.murmur"));
    await expect(checkApp(app, { plist: fakePlist() })).rejects.toThrow("bundle identifier");
  });

  it("rejects a bundle without either required license file", () => {
    const app = fakeApp();
    const notices = path.join(app, "Contents", "Resources", "resources", "THIRD-PARTY-NOTICES.txt");
    rmSync(notices);
    expect(() => audit(app)).toThrow();
    writeFileSync(notices, readFileSync("src-tauri/resources/THIRD-PARTY-NOTICES.txt"));
    rmSync(path.join(app, "Contents", "Resources", "LICENSE"));
    expect(() => audit(app)).toThrow();
  });

  it("verifies every architecture has exactly the approved entitlements when requested", async () => {
    const { checkApp } = await auditModule();
    const app = fakeApp();
    const verified: string[] = [];
    const architectures: string[] = [];
    const codeSignature = fakeCodeSignature(
      ["arm64", "x86_64"],
      { arm64: exactEntitlements, x86_64: exactEntitlements },
      verified,
    );
    const wrapped: CodeSignature = {
      ...codeSignature,
      entitlements: (executablePath, architecture) => {
        architectures.push(architecture);
        return codeSignature.entitlements(executablePath, architecture);
      },
    };

    await expect(checkApp(app, {
      verifySignatureEntitlements: true,
      codeSignature: wrapped,
      plist: fakePlist(),
    })).resolves.toBeUndefined();
    expect(verified).toEqual([app]);
    expect(architectures).toEqual(["arm64", "x86_64"]);
  });

  it.each([
    ["missing entitlement", {
      "com.apple.security.automation.apple-events": true,
    }],
    ["missing calendar entitlement", {
      "com.apple.security.automation.apple-events": true,
      "com.apple.security.device.audio-input": true,
    }],
    ["unexpected entitlement", {
      ...exactEntitlementValues,
      "com.apple.security.network.client": true,
    }],
    ["disabled entitlement", {
      ...exactEntitlementValues,
      "com.apple.security.device.audio-input": false,
    }],
    ["non-Boolean entitlement", {
      ...exactEntitlementValues,
      "com.apple.security.device.audio-input": "true",
    }],
  ])("rejects signed bundles with a %s", async (_label, entitlements) => {
    const { checkApp } = await auditModule();
    await expect(checkApp(fakeApp(), {
      verifySignatureEntitlements: true,
      codeSignature: fakeCodeSignature(),
      plist: fakePlist(entitlements),
    })).rejects.toThrow();
  });

  it("rejects comments masquerading as active entitlements", async () => {
    const { validateProductionEntitlements } = await auditModule();
    const commented = `<plist><dict><!-- ${exactEntitlements} --></dict></plist>`;
    expect(() => validateProductionEntitlements({}, commented)).toThrow();
  });

  it("rejects duplicate serialized entitlement keys", async () => {
    const { validateProductionEntitlements } = await auditModule();
    const duplicate = exactEntitlements.replace(
      "</dict>",
      "<key>com.apple.security.device.audio-input</key><true/></dict>",
    );
    expect(() => validateProductionEntitlements(exactEntitlementValues, duplicate)).toThrow();
  });

  it("rejects malformed property lists before approving signed entitlements", async () => {
    const { checkApp } = await auditModule();
    await expect(checkApp(fakeApp(), {
      verifySignatureEntitlements: true,
      codeSignature: fakeCodeSignature(),
      plist: { parse: () => { throw new Error("malformed plist"); } },
    })).rejects.toThrow("malformed plist");
  });

  it("rejects a universal binary when one architecture has extra entitlements", async () => {
    const { checkApp } = await auditModule();
    const unexpected = exactEntitlements.replace(
      "</dict>",
      "<key>com.apple.security.network.client</key><true/></dict>",
    );
    const plist: PropertyList = {
      parse: (serialized) => serialized === unexpected
        ? { ...exactEntitlementValues, "com.apple.security.network.client": true }
        : fakePlist().parse(serialized),
    };
    await expect(checkApp(fakeApp(), {
      verifySignatureEntitlements: true,
      codeSignature: fakeCodeSignature(
        ["arm64", "x86_64"],
        { arm64: exactEntitlements, x86_64: unexpected },
      ),
      plist,
    })).rejects.toThrow("unexpected");
  });

  it("parses only the documented CLI modes and one optional path", async () => {
    const { parseCliArguments } = await auditModule();
    expect(parseCliArguments([])).toMatchObject({
      kind: "app",
      verifySignatureEntitlements: false,
    });
    expect(parseCliArguments(["custom.app"])).toEqual({
      kind: "app",
      inputPath: "custom.app",
      verifySignatureEntitlements: false,
    });
    expect(parseCliArguments(["--verify-dmg-entitlements", "custom.dmg"])).toEqual({
      kind: "dmg",
      inputPath: "custom.dmg",
      verifySignatureEntitlements: true,
    });
    expect(() => parseCliArguments(["--unknown"])).toThrow("unknown");
    expect(() => parseCliArguments(["--dmg", "one.dmg", "two.dmg"])).toThrow("at most one");
    expect(() => parseCliArguments(["--dmg", "--verify-app-entitlements"]))
      .toThrow("where a path was expected");
  });

  it("rejects a nested issuer resource outside Contents/MacOS", () => {
    const app = fakeApp();
    const resource = path.join(app, "Contents", "Resources", "helpers", "issue-license");
    mkdirSync(path.dirname(resource), { recursive: true });
    writeFileSync(resource, "must not ship");
    expect(() => audit(app)).toThrow();
  });

  it("rejects a symlink that targets the issuer", () => {
    const app = fakeApp();
    const resources = path.join(app, "Contents", "Resources");
    mkdirSync(resources, { recursive: true });
    symlinkSync("../private/issue_license", path.join(resources, "helper"));
    expect(() => audit(app)).toThrow();
  });

  it("rejects a symlinked app entry in a DMG and still detaches it", async () => {
    const { checkDmg } = await auditModule();
    let detached = false;
    const diskImage: DiskImage = {
      attach: (_dmgPath, mountPoint) => {
        makeBundle(path.join(mountPoint, "payload"));
        symlinkSync("payload", path.join(mountPoint, "Murmur.app"));
      },
      detach: (mountPoint) => {
        detached = true;
        clearMount(mountPoint);
      },
    };

    await expect(checkDmg(fakeDmgDirectory(), { platform: "darwin", diskImage }))
      .rejects.toThrow("real directory");
    expect(detached).toBe(true);
  });

  it("detaches the DMG when app auditing fails", async () => {
    const { checkDmg } = await auditModule();
    let detached = false;
    const diskImage: DiskImage = {
      attach: (_dmgPath, mountPoint) => {
        mkdirSync(path.join(mountPoint, "Murmur.app"));
      },
      detach: (mountPoint) => {
        detached = true;
        clearMount(mountPoint);
      },
    };

    await expect(checkDmg(fakeDmgDirectory(), { platform: "darwin", diskImage }))
      .rejects.toThrow();
    expect(detached).toBe(true);
  });

  it("verifies both the DMG and every contained-app architecture when requested", async () => {
    const { checkDmg } = await auditModule();
    const verified: string[] = [];
    const diskImage: DiskImage = {
      attach: (_dmgPath, mountPoint) => {
        makeBundle(path.join(mountPoint, "Murmur.app"));
      },
      detach: (mountPoint) => clearMount(mountPoint),
    };

    await expect(checkDmg(fakeDmgDirectory(), {
      platform: "darwin",
      diskImage,
      verifySignatureEntitlements: true,
      codeSignature: fakeCodeSignature(["arm64", "x86_64"], {
        arm64: exactEntitlements,
        x86_64: exactEntitlements,
      }, verified),
      plist: fakePlist(),
    })).resolves.toBeUndefined();
    expect(verified).toHaveLength(2);
    expect(verified[0]).toMatch(/\.dmg$/);
    expect(verified[1]).toMatch(/Murmur\.app$/);
  });

  it("force-detaches after an ordinary detach failure", async () => {
    const { checkDmg } = await auditModule();
    const detachModes: boolean[] = [];
    const diskImage: DiskImage = {
      attach: (_dmgPath, mountPoint) => {
        makeBundle(path.join(mountPoint, "Murmur.app"));
      },
      detach: (mountPoint, force = false) => {
        detachModes.push(force);
        if (!force) throw new Error("synthetic detach failure");
        clearMount(mountPoint);
      },
    };

    await expect(checkDmg(fakeDmgDirectory(), { platform: "darwin", diskImage, plist: fakePlist() }))
      .resolves.toBeUndefined();
    expect(detachModes).toEqual([false, true]);
  });

  it("rejects ambiguous DMG output before attempting attachment", async () => {
    const { checkDmg } = await auditModule();
    let attached = false;
    const diskImage: DiskImage = {
      attach: () => { attached = true; },
      detach: () => undefined,
    };

    await expect(checkDmg(fakeDmgDirectory(2), { platform: "darwin", diskImage }))
      .rejects.toThrow("exactly one DMG");
    expect(attached).toBe(false);
  });
});
