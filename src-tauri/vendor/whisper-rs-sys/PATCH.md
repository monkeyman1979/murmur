# Murmur vendor patch

This directory is based on the [crates.io `whisper-rs-sys 0.11.1` archive](https://static.crates.io/crates/whisper-rs-sys/whisper-rs-sys-0.11.1.crate) (archive SHA-256 `76bab42b2c319e3a1e0280137c59368072348d3277873c7588b6466a127dca58`). Its Rust bindings are labeled Unlicense; the nested `whisper.cpp/` carries its own MIT `LICENSE` (Copyright (c) 2023-2024 The ggml authors). Both must be attributed separately in app notices.

Compared with that archive, the only substantive code change is a macOS Metal ownership leak fix in:

- `whisper.cpp/ggml/src/ggml-metal.m`

The bundled ggml code creates the embedded Metal shader source with `[[NSString alloc] initWithBytes:…]` and the compile options with `[MTLCompileOptions new]`. Both are owned objects, but upstream 0.11.1 did not release them. Murmur unloads and reloads Whisper contexts, so every load leaked about 384 KiB of shader source plus a compile-options object.

The local change releases `options` after `newLibraryWithSource` and releases the embedded `src` on both success and error paths. No inference behavior is changed. `PATCH.md` is new; five other nested files differ only in trailing blank lines; the archive's `.cargo_vcs_info.json` is not vendored. The patched `ggml-metal.m` SHA-256 is `a0b36119a8fea3f8d5c8a1d7c5b93c33d95d76dc650cd1bc44b31238ca7587e5` (archive original `4597402fe90620897d57f7354b31a28f20577264091f0093db7767ae762b1c49`).

Remove this path override when the selected `whisper-rs`/`whisper-rs-sys` release includes equivalent releases. Re-check with a repeated load/unload run and macOS `leaks`; a release should not produce one 393,216-byte `CFString` and one `MTLCompileOptionsInternal` per context load.
