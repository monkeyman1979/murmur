# Build from source (macOS candidate)

This is a development build, not an installed-app release or a signed/notarized download. The provisional v0.1.0 target is Apple Silicon macOS 14.4+. Linux is a later implementation and QA milestone; passing portable Rust checks on Linux does not make Linux supported.

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

The notice generator shells out to `pnpm`, so expose the Corepack pnpm shim on `PATH` if your Corepack installation does not do so automatically. The macOS build checker verifies the separately bundled MIT LICENSE and third-party notices. Do not distribute an unsigned local app/DMG as an official installer. No existing-user migration, permission continuity, or Gatekeeper behavior has been validated on an Internet-downloaded artifact.

At first run, grant the macOS permissions requested for the features you use. The speech model download requires a network connection and disk space (~488 MB for the current model); transcription runs offline afterward. To build a release, independently review third-party rights and the exact artifact, sign/notarize/staple with owner-controlled credentials, verify on clean Macs and test upgrades. A source-only first release avoids implying that those binary gates have passed. Do not enable or depend on automatic updates until the endpoint, signature and upgrade path have been validated for the final repository.
