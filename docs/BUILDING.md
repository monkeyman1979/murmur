# Build from source (macOS)

For the official signed/notarized installer, use the download link in [README.md](../README.md). Commands here create development builds; they do not automatically establish trusted release artifacts. Murmur v0.1.0 requires Apple Silicon macOS 14.4+. Installed release checks were performed on macOS 27.0.1; older versions in that range remain unverified. Intel Macs, Windows and Linux are not supported; portable Rust checks on Linux do not establish app support.

Requirements: full Xcode with command-line tools, Rust 1.95.0 (`rust-toolchain.toml`), Node 22 or 24 (Node 24 used for the full local unit suite), Corepack and pnpm 10.32.0 (`packageManager` in `package.json`). Install the target/toolchain and native dependencies required by Tauri 2 and whisper.cpp/Metal. On macOS run:

```sh
corepack pnpm install --frozen-lockfile
corepack pnpm legal:inventory:check
corepack pnpm test:unit
corepack pnpm test:release-metadata
corepack pnpm check:release-metadata
corepack pnpm build
corepack pnpm tauri dev
# Optional unsigned local app for development only:
corepack pnpm build:app-local
```

The notice generator shells out to `pnpm`, so expose the Corepack pnpm shim on `PATH` if your Corepack installation does not do so automatically. The macOS build checker verifies the separately bundled MIT LICENSE and third-party notices. Do not distribute an unsigned local app/DMG as an official installer. The release installer passes strict signature, exact-entitlement, notarization/staple and Gatekeeper checks. Fresh onboarding and a retained-data update between release candidates were tested on the stated Mac; this does not validate every legacy-data migration, failure/recovery path or supported macOS version.

At first run, grant the macOS permissions requested for the features you use. The speech model download requires a network connection and disk space (~488 MB for the current model); transcription runs offline afterward. To build a release, independently review third-party rights and the exact artifact, sign/notarize/staple with owner-controlled credentials, verify on clean Macs and test upgrades. Do not assume a new locally built artifact is covered by the QA of the published binary. Do not enable or depend on automatic updates until the endpoint, signature and upgrade path have been validated for the final repository.
