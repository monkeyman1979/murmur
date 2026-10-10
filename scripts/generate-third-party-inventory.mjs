#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const inventoryPath = resolve(root, "third-party/dependency-inventory.json");
const noticesPath = resolve(root, "src-tauri/resources/THIRD-PARTY-NOTICES.txt");
const checkOnly = process.argv.includes("--check");
// The first release candidate targets Apple Silicon macOS only (see docs/BUILDING.md).
// Keep this list aligned with the release workflow and public requirements.
const macTargets = ["aarch64-apple-darwin"];

function command(program, args) {
  return execFileSync(program, args, {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
  });
}

function repositoryUrl(value) {
  if (typeof value === "string") return value;
  if (value && typeof value.url === "string") return value.url;
  return null;
}

function frontendInventory() {
  const roots = JSON.parse(command("pnpm", ["list", "--prod", "--json", "--depth", "Infinity"]));
  const packages = new Map();
  const missingManifests = new Set();

  function visit(dependencies) {
    for (const [dependencyName, dependency] of Object.entries(dependencies ?? {})) {
      const key = `${dependencyName}@${dependency.version ?? "unknown"}`;
      if (packages.has(key)) continue;

      const manifestPath = dependency.path ? resolve(dependency.path, "package.json") : null;
      if (manifestPath && existsSync(manifestPath)) {
        const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
        packages.set(key, {
          name: dependencyName,
          version: dependency.version ?? manifest.version ?? "unknown",
          license: manifest.license ?? manifest.licenses ?? null,
          homepage: manifest.homepage ?? null,
          repository: repositoryUrl(manifest.repository),
          packageDirectory: dirname(manifestPath),
        });
      } else {
        missingManifests.add(key);
        packages.set(key, {
          name: dependencyName,
          version: dependency.version ?? "unknown",
          license: null,
          homepage: null,
          repository: null,
          packageDirectory: null,
        });
      }
      visit(dependency.dependencies);
    }
  }

  for (const project of roots) visit(project.dependencies);

  return {
    packages: [...packages.values()].sort((a, b) =>
      a.name.localeCompare(b.name) || a.version.localeCompare(b.version),
    ),
    missingManifests: [...missingManifests].sort(),
  };
}

function cargoMetadata(target) {
  return JSON.parse(command("cargo", [
    "metadata",
    "--manifest-path",
    "src-tauri/Cargo.toml",
    "--locked",
    "--format-version",
    "1",
    "--filter-platform",
    target,
  ]));
}

function shippingCargoPackageIds(metadata) {
  const rootPackage = metadata.packages.find((pkg) =>
    pkg.name === "murmur" && pkg.manifest_path.endsWith("/src-tauri/Cargo.toml"),
  );
  if (!rootPackage) throw new Error("Could not find the Murmur package in Cargo metadata.");

  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const included = new Set();
  const pending = [rootPackage.id];

  while (pending.length > 0) {
    const id = pending.pop();
    if (!id || included.has(id)) continue;
    included.add(id);
    const node = nodes.get(id);
    for (const dependency of node?.deps ?? []) {
      // Dev-only dependencies do not contribute to the customer application.
      // Normal and build dependencies are retained: build scripts can compile
      // code or assets into the distributed binary.
      if (dependency.dep_kinds.some((kind) => kind.kind !== "dev")) {
        pending.push(dependency.pkg);
      }
    }
  }

  included.delete(rootPackage.id);
  return included;
}

function rustInventory() {
  const packages = new Map();

  for (const target of macTargets) {
    const metadata = cargoMetadata(target);
    const included = shippingCargoPackageIds(metadata);
    for (const pkg of metadata.packages) {
      if (!included.has(pkg.id)) continue;
      const key = `${pkg.name}@${pkg.version}|${pkg.source ?? pkg.manifest_path}`;
      const existing = packages.get(key);
      if (existing) {
        existing.targets.push(target);
        continue;
      }
      packages.set(key, {
        name: pkg.name,
        version: pkg.version,
        license: pkg.license ?? null,
        licenseFile: pkg.license_file ?? null,
        repository: pkg.repository ?? null,
        source: pkg.source ?? null,
        packageDirectory: dirname(pkg.manifest_path),
        targets: [target],
      });
    }
  }

  return [...packages.values()].sort((a, b) =>
    a.name.localeCompare(b.name) || a.version.localeCompare(b.version),
  );
}

function publicPackage(entry) {
  const { packageDirectory: _packageDirectory, ...publicEntry } = entry;
  return publicEntry;
}

function candidateLicenseFiles(entry) {
  if (!entry.packageDirectory || !existsSync(entry.packageDirectory)) return [];

  const candidates = new Set();
  if (entry.licenseFile) {
    const declared = resolve(entry.packageDirectory, entry.licenseFile);
    if (existsSync(declared)) candidates.add(declared);
  }

  for (const name of readdirSync(entry.packageDirectory)) {
    if (!/^(licen[cs]e|copying|copyright|notice)([._-].*)?$/i.test(name)) continue;
    const path = resolve(entry.packageDirectory, name);
    if (statSync(path).isFile()) candidates.add(path);
  }

  return [...candidates].sort((a, b) => basename(a).localeCompare(basename(b)));
}

function readLicenseMaterials(entry) {
  const materials = [];
  for (const path of candidateLicenseFiles(entry)) {
    const content = readFileSync(path, "utf8")
      .replace(/\r\n?/g, "\n")
      .split("\n")
      .map((line) => line.trimEnd())
      .join("\n")
      .trim();
    if (!content || content.includes("\u0000")) continue;
    materials.push({ name: basename(path), content });
  }
  return materials;
}

function renderNoticeSection(title, entries) {
  const groups = new Map();
  const unresolved = [];

  for (const entry of entries) {
    const materials = readLicenseMaterials(entry);
    const component = `${entry.name} ${entry.version} — ${entry.license ?? "license metadata unavailable"}`;
    if (materials.length === 0) {
      unresolved.push(component);
      continue;
    }
    const body = materials.map(({ name, content }) => `--- ${name} ---\n${content}`).join("\n\n");
    const hash = createHash("sha256").update(body).digest("hex");
    const group = groups.get(hash) ?? { body, components: [] };
    group.components.push(component);
    groups.set(hash, group);
  }

  const lines = [title, "=".repeat(title.length), ""];
  for (const group of [...groups.values()].sort((a, b) =>
    a.components[0].localeCompare(b.components[0]),
  )) {
    lines.push("Components:");
    for (const component of group.components.sort()) lines.push(`- ${component}`);
    lines.push("", group.body, "", "-".repeat(78), "");
  }

  if (unresolved.length > 0) {
    lines.push("Components without a discoverable packaged license file", "-------------------------------------------------------");
    lines.push("These entries retain their package license expression above but require manual", "release review before these notices can be treated as final.", "");
    for (const component of unresolved.sort()) lines.push(`- ${component}`);
    lines.push("");
  }

  return { text: lines.join("\n"), unresolvedCount: unresolved.length };
}

function renderNotices(frontend, rust) {
  const installedFrontend = frontend.packages.filter((entry) => entry.packageDirectory);
  const frontendSection = renderNoticeSection("JavaScript and bundled frontend components", installedFrontend);
  const rustSection = renderNoticeSection("Rust and native components", rust);

  const header = `Murmur Third-Party Notices
==========================

This file is generated from the production pnpm dependency tree and the normal/build
Cargo dependency graphs for ${macTargets.join(" and ")}.
The complete text is distributed offline as a separate file in the macOS app bundle
(Contents/Resources/resources/THIRD-PARTY-NOTICES.txt) and in the source tree.
Settings > About > Acknowledgements gives a short summary, not the full text.

Third-party components remain governed by their own licenses. No third-party license
relicenses Murmur-owned work or grants rights to third-party fonts, model weights,
or copied UI under Murmur's MIT license.

Release-review status
---------------------
This generated file is a reproducible baseline, not legal approval. Before public
release, independently verify the downloaded Whisper model terms, bundled font
provenance and reserved names, copied/generated UI source, SQLite public-domain
provenance, exact release features, and every entry reported below without a
packaged license file. Optional packages absent on the generating host are retained
in third-party/dependency-inventory.json for review but are not represented
as shipped components here.

Generated from:
- package.json and pnpm-lock.yaml
- src-tauri/Cargo.toml and src-tauri/Cargo.lock
- installed package/crate license and notice files

Summary:
- ${installedFrontend.length} installed frontend dependency entries
- ${rust.length} macOS normal/build Cargo dependency entries
- ${frontendSection.unresolvedCount} frontend ${frontendSection.unresolvedCount === 1 ? "entry" : "entries"} without a packaged license file
- ${rustSection.unresolvedCount} Rust ${rustSection.unresolvedCount === 1 ? "entry" : "entries"} without a packaged license file

Downloaded model
----------------
Murmur downloads the configured Whisper model only after explicit user action. The
model is not embedded in the application bundle. The registry pins ggml-small.bin
(487601967 bytes, SHA-256 1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b)
with a fallback at ggerganov/whisper.cpp revision 5359861c739e955e79d9a303bcbc70fb988958b1.
The pinned repository card calls these converted OpenAI Whisper weights MIT;
OpenAI's Whisper README says its model weights are MIT. See the OpenAI notice below.
The exact conversion chain and final download behavior still require release review.

Embedded meeting microphone detector
-------------------------------------
Silero VAD v6.0 (MIT), upstream commit fba061dc5559f696e62171e9a0741782b0fdc23c,
ONNX 2327524 bytes, SHA-256 597d30b3ec076608d059477bb14cfeffdf951bf5cae370d38f65d33bbfe82004.
The detector is embedded for offline post-meeting microphone transcription only.
ONNX Runtime 1.28.0 CPU is statically linked by ort 2.0.0-rc.13; its macOS
prebuilt archive is pinned by ort-sys dist.tsv SHA-256
6934874e2e953576d9c1db47ff1af39c62c4f4220dbe6f988e131f72879674c7.
Review native artifact provenance and transitive notices before release.

SQLite
------
Murmur builds SQLite through rusqlite/libsqlite3-sys. SQLite upstream describes the
SQLite source as public domain; the crate wrapper notices are included below. Exact
bundled-source provenance remains a release-review item.

`;

  // These copies do not occur as installed production packages: shadcn-vue
  // generates source into src/components/ui, and whisper.cpp is nested inside
  // the path-patched whisper-rs-sys crate. Keep their notices in the app too.
  const shadcnManifest = JSON.parse(readFileSync(resolve(root, "node_modules/shadcn-vue/package.json"), "utf8"));
  const shadcnLicense = readFileSync(resolve(root, "node_modules/shadcn-vue/LICENSE"), "utf8").trimEnd();
  const whisperCppLicense = readFileSync(resolve(root, "src-tauri/vendor/whisper-rs-sys/whisper.cpp/LICENSE"), "utf8").trimEnd();
  const sileroLicense = readFileSync(resolve(root, "src-tauri/resources/SILERO-LICENSE.txt"), "utf8").trimEnd();
  const ortLicense = readFileSync(resolve(root, "src-tauri/resources/ONNXRUNTIME-LICENSE.txt"), "utf8").trimEnd();
  const ortNotices = readFileSync(resolve(root, "src-tauri/resources/ONNXRUNTIME-THIRD-PARTY-NOTICES.txt"), "utf8").trimEnd();
  const copiedNotices = `Additional source and model notices
===================================

Generated/copied src/components/ui/ components: shadcn-vue ${shadcnManifest.version}
(source: https://github.com/unovue/shadcn-vue; locally adapted components).

--- shadcn-vue LICENSE ---
${shadcnLicense}

Vendored whisper.cpp inside whisper-rs-sys 0.11.1 (local Metal patch documented
in src-tauri/vendor/whisper-rs-sys/PATCH.md):

--- vendored whisper.cpp LICENSE ---
${whisperCppLicense}

Downloaded OpenAI Whisper model weights converted by ggerganov/whisper.cpp:

--- OpenAI Whisper LICENSE ---
MIT License

Copyright (c) 2022 OpenAI

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

Embedded Silero VAD v6.0 model:

--- Silero LICENSE ---
${sileroLicense}

Statically linked ONNX Runtime 1.28.0 native library:

--- ONNX Runtime LICENSE ---
${ortLicense}

--- ONNX Runtime ThirdPartyNotices.txt ---
${ortNotices}
`;

  return `${header}${copiedNotices}\n${frontendSection.text.trimEnd()}\n\n${rustSection.text.trimEnd()}\n`;
}

const vadModel = readFileSync(resolve(root, "src-tauri/resources/silero-v6.onnx"));
if (vadModel.length !== 2327524 || createHash("sha256").update(vadModel).digest("hex") !== "597d30b3ec076608d059477bb14cfeffdf951bf5cae370d38f65d33bbfe82004") {
  throw new Error("Embedded meeting VAD model does not match pinned upstream bytes.");
}

const frontend = frontendInventory();
const rust = rustInventory();
const inventory = {
  schemaVersion: 2,
  generatedFrom: ["package.json", "pnpm-lock.yaml", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock"],
  targets: macTargets,
  caveats: [
    "The frontend graph is the pnpm production-install view and can retain platform-optional or build-time packages.",
    "The Rust graph follows normal and build dependencies for the configured Apple Silicon macOS target and excludes dev-only dependencies.",
    "Downloaded Whisper and embedded Silero model weights, statically linked ONNX Runtime, bundled SQLite source, fonts, copied/generated UI code, brand assets, and system-framework exclusions still require release review.",
    "A package without an installed manifest remains in missingManifests but is excluded from the bundled notice until target-specific release review establishes that it ships.",
  ],
  frontend: {
    packageCount: frontend.packages.length,
    installedPackageCount: frontend.packages.filter((entry) => entry.packageDirectory).length,
    missingManifestCount: frontend.missingManifests.length,
    missingManifests: frontend.missingManifests,
    packages: frontend.packages.map(publicPackage),
  },
  rust: {
    packageCount: rust.length,
    packages: rust.map(publicPackage),
  },
  embeddedModel: {
    name: "Silero VAD", version: "v6.0", license: "MIT",
    upstreamCommit: "fba061dc5559f696e62171e9a0741782b0fdc23c",
    bytes: 2327524,
    sha256: "597d30b3ec076608d059477bb14cfeffdf951bf5cae370d38f65d33bbfe82004",
  },
  nativeRuntime: {
    name: "ONNX Runtime", version: "1.28.0", wrapper: "ort 2.0.0-rc.13",
    linkage: "static", macosArchiveSha256: "6934874e2e953576d9c1db47ff1af39c62c4f4220dbe6f988e131f72879674c7",
  },
};

const outputs = [
  [inventoryPath, `${JSON.stringify(inventory, null, 2)}\n`],
  [noticesPath, renderNotices(frontend, rust)],
];

if (checkOnly) {
  const stale = outputs.filter(([path, rendered]) =>
    !existsSync(path) || readFileSync(path, "utf8") !== rendered,
  );
  if (stale.length > 0) {
    console.error(`Third-party legal artifacts are missing or stale:\n${stale.map(([path]) => `- ${path}`).join("\n")}\nRun: pnpm legal:inventory`);
    process.exit(1);
  }
  console.log(`Third-party legal artifacts are current (${inventory.frontend.installedPackageCount} installed frontend, ${inventory.rust.packageCount} Rust entries).`);
} else {
  for (const [path, rendered] of outputs) {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, rendered);
  }
  console.log(`Wrote ${inventoryPath} and ${noticesPath} (${inventory.frontend.installedPackageCount} installed frontend, ${inventory.rust.packageCount} Rust entries).`);
}
