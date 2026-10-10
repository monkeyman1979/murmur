use std::path::{Path, PathBuf};

#[path = "../build_policy.rs"]
mod build_policy;

fn manifest_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("read Rust source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

fn without_xml_comments(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut remaining = source;
    while let Some(start) = remaining.find("<!--") {
        output.push_str(&remaining[..start]);
        let after_start = &remaining[start + 4..];
        let end = after_start.find("-->").expect("well-formed plist comment");
        remaining = &after_start[end + 3..];
    }
    output.push_str(remaining);
    output
}

#[test]
fn gui_diagnostics_use_the_release_suppressed_facade() {
    let source_root = manifest_path("src");
    let mut sources = Vec::new();
    rust_sources(&source_root, &mut sources);

    for path in sources {
        let relative = path
            .strip_prefix(&source_root)
            .expect("relative source path");
        if relative == Path::new("diagnostics.rs") || relative == Path::new("lib.rs") {
            continue;
        }
        if relative.starts_with("bin") {
            // Explicit developer-only binaries are not linked into the GUI.
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read Rust source");
        assert!(
            !source.contains("eprintln!"),
            "{} bypasses the production-suppressed diagnostic facade",
            path.display()
        );
        assert!(
            !source.contains("dbg!("),
            "{} contains an unrestricted debug sink",
            path.display()
        );
    }

    let lib = std::fs::read_to_string(manifest_path("src/lib.rs")).expect("read lib.rs");
    assert_eq!(lib.matches("std::eprintln!").count(), 1);
    assert!(lib.contains("[murmur] application terminated unexpectedly"));
    assert!(!lib.contains("dbg!("));

    let facade = std::fs::read_to_string(manifest_path("src/diagnostics.rs"))
        .expect("read diagnostics facade");
    assert!(facade.contains("#[cfg(debug_assertions)]"));
    assert!(facade.contains("#[cfg(not(debug_assertions))]"));
    assert!(facade.contains("if false"));
}

#[test]
fn capture_does_not_load_or_mutate_legacy_license_state() {
    let lib = std::fs::read_to_string(manifest_path("src/lib.rs")).expect("read app runtime");
    let meeting = std::fs::read_to_string(manifest_path("src/meeting_capture/mod.rs"))
        .expect("read meeting capture runtime");
    let manifest =
        std::fs::read_to_string(manifest_path("Cargo.toml")).expect("read Tauri Cargo manifest");
    assert!(!manifest_path("src/trial.rs").exists());
    assert!(!manifest_path("src/license.rs").exists());
    assert!(!manifest_path("../tools/issue-license/Cargo.toml").exists());
    assert!(!manifest_path("../crates/license-token/Cargo.toml").exists());
    for source in [&lib, &meeting] {
        assert!(!source.contains("require_capture_activation"));
        assert!(!source.contains("load_or_init(&app_handle)"));
        assert!(!source.contains("record_dictation_words"));
    }
    assert!(!manifest.contains("murmur-license-token"));
    assert!(!manifest.contains("ed25519-dalek"));
    // Existing trial.json bytes remain in app data, but no startup or capture
    // path opens the file. The installed-app migration still needs manual QA.
}

#[test]
fn tauri_javascript_toolchain_is_project_scoped_and_minor_aligned() {
    let repository = manifest_path("..");
    let package =
        std::fs::read_to_string(repository.join("package.json")).expect("read package.json");
    let workspace = std::fs::read_to_string(repository.join("pnpm-workspace.yaml"))
        .expect("read project workspace boundary");
    let lockfile =
        std::fs::read_to_string(repository.join("pnpm-lock.yaml")).expect("read pnpm lockfile");

    let packages: serde_json::Value = serde_json::from_str(&package).expect("parse package.json");
    let rust_manifest =
        std::fs::read_to_string(manifest_path("Cargo.toml")).expect("read Rust manifest");
    assert_eq!(packages["dependencies"]["@tauri-apps/api"], "2.11.1");
    assert_eq!(packages["pnpm"]["overrides"]["@tauri-apps/api"], "2.11.1");
    assert_eq!(packages["devDependencies"]["@tauri-apps/cli"], "2.11.1");
    assert!(rust_manifest.contains("tauri = { version = \"=2.11.1\""));
    assert!(workspace.contains("  - \".\""));
    assert!(lockfile.contains("'@tauri-apps/api@2.11.1':"));
    assert!(!lockfile.contains("'@tauri-apps/api@2.10.1':"));
}

#[test]
fn production_macos_privileges_are_explicit_and_allowlisted() {
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_path("tauri.conf.json"))
            .expect("read production Tauri config"),
    )
    .expect("parse production Tauri config");
    let macos = config
        .pointer("/bundle/macOS")
        .and_then(serde_json::Value::as_object)
        .expect("production macOS bundle config");
    assert_eq!(
        macos
            .get("hardenedRuntime")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        macos
            .get("entitlements")
            .and_then(serde_json::Value::as_str),
        Some("./entitlements.plist")
    );

    for overlay in ["tauri.debug.conf.json", "tauri.meeting-local.conf.json"] {
        let overlay: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(manifest_path(overlay)).expect("read Tauri config overlay"),
        )
        .expect("parse Tauri config overlay");
        let macos = overlay
            .pointer("/bundle/macOS")
            .and_then(serde_json::Value::as_object)
            .expect("overlay macOS bundle config");
        assert!(!macos.contains_key("hardenedRuntime"));
        assert!(!macos.contains_key("entitlements"));
    }

    let entitlements =
        std::fs::read_to_string(manifest_path("entitlements.plist")).expect("read entitlements");
    let parsed = plist::Value::from_reader_xml(entitlements.as_bytes())
        .expect("parse production entitlement plist");
    let dictionary = parsed
        .as_dictionary()
        .expect("production entitlements must be a dictionary");
    let mut keys = dictionary.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    let expected = vec![
        "com.apple.security.automation.apple-events",
        "com.apple.security.device.audio-input",
    ];
    assert_eq!(keys, expected);
    for key in &expected {
        assert_eq!(dictionary.get(key), Some(&plist::Value::Boolean(true)));
    }

    let active_xml = without_xml_comments(&entitlements);
    let serialized_keys = active_xml
        .split("<key>")
        .skip(1)
        .map(|entry| entry.split("</key>").next().expect("entitlement key"))
        .collect::<Vec<_>>();
    assert_eq!(serialized_keys.len(), dictionary.len());
    let mut unique_keys = serialized_keys.clone();
    unique_keys.sort_unstable();
    unique_keys.dedup();
    assert_eq!(unique_keys.len(), serialized_keys.len());

    let info = std::fs::read_to_string(manifest_path("Info.plist")).expect("read Info.plist");
    assert!(info.contains("<key>NSAppleEventsUsageDescription</key>"));
    assert!(info.contains("opens the Privacy &amp; Security pane when you ask"));
    assert!(info.contains("Mute system audio while dictating"));
    assert!(info.contains("captures your Mac’s system audio"));
    assert!(info.contains("configured dictation shortcut (Right ⌘ by default)"));
    assert!(!info.contains("<key>NSScreenCaptureUsageDescription</key>"));

    let mute_audio =
        std::fs::read_to_string(manifest_path("src/mute_audio.rs")).expect("read mute audio");
    assert!(mute_audio.contains("Command::new(\"osascript\")"));
    let permissions =
        std::fs::read_to_string(manifest_path("src/permissions.rs")).expect("read permissions");
    assert!(permissions.contains("Command::new(\"open\")"));
    assert_eq!(
        config
            .pointer("/app/macOSPrivateApi")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(config
        .pointer("/app/windows")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|windows| windows.iter().any(|window| {
            window
                .get("transparent")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        })));
}

#[test]
fn local_signed_build_does_not_require_production_updater_secrets() {
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_path("tauri.meeting-local.conf.json"))
            .expect("read local signed Tauri config"),
    )
    .expect("parse local signed Tauri config");
    assert_eq!(
        config
            .pointer("/bundle/createUpdaterArtifacts")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        config
            .pointer("/bundle/macOS/signingIdentity")
            .and_then(serde_json::Value::as_str),
        Some("Murmur Dev")
    );
}

#[test]
fn source_only_build_pauses_public_updates() {
    let repository = manifest_path("..");
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_path("tauri.conf.json"))
            .expect("read production Tauri config"),
    )
    .expect("parse production Tauri config");
    assert_eq!(
        config
            .pointer("/bundle/createUpdaterArtifacts")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        config
            .pointer("/plugins/updater/endpoints/0")
            .and_then(serde_json::Value::as_str),
        Some("https://releases.seedtoshelves.com/murmur/stable/latest.json")
    );
    assert!(config
        .pointer("/plugins/updater/pubkey")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|key| key.len() > 80));

    let capability: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_path("capabilities/main.json"))
            .expect("read main capability"),
    )
    .expect("parse main capability");
    let permissions = capability["permissions"]
        .as_array()
        .expect("main permissions");
    for permission in [
        "updater:allow-check",
        "updater:allow-download",
        "updater:allow-install",
        "process:allow-restart",
    ] {
        assert!(permissions.contains(&serde_json::json!(permission)));
    }
    for forbidden in [
        "updater:default",
        "updater:allow-download-and-install",
        "process:default",
        "process:allow-exit",
    ] {
        assert!(!permissions.contains(&serde_json::json!(forbidden)));
    }

    let package =
        std::fs::read_to_string(repository.join("package.json")).expect("read package.json");
    let manifest = std::fs::read_to_string(manifest_path("Cargo.toml")).expect("read Cargo.toml");
    assert!(package.contains("\"@tauri-apps/plugin-updater\": \"2.10.0\""));
    assert!(package.contains("\"@tauri-apps/plugin-process\": \"2.3.1\""));
    assert!(manifest.contains("tauri-plugin-updater = \"=2.10.0\""));
    assert!(manifest.contains("tauri-plugin-process = \"=2.3.1\""));

    let updater = std::fs::read_to_string(repository.join("src/composables/useUpdater.ts"))
        .expect("read updater composable");
    assert!(updater.contains("UPDATES_ENABLED = import.meta.env.MODE === \"test\""));
    assert!(updater.contains("if (!UPDATES_ENABLED) return;"));
    let settings = std::fs::read_to_string(repository.join("src/views/settings/General.vue"))
        .expect("read General settings");
    assert!(settings.contains("v-if=\"updater.enabled\""));
}

#[test]
fn production_frontend_build_has_a_post_bundle_console_gate() {
    let repository = manifest_path("..");
    let vite =
        std::fs::read_to_string(repository.join("vite.config.ts")).expect("read Vite config");
    let package =
        std::fs::read_to_string(repository.join("package.json")).expect("read package.json");
    let checker = std::fs::read_to_string(repository.join("scripts/check-production-bundle.mjs"))
        .expect("read production bundle checker");

    assert!(vite.contains("mode === \"production\" ? [\"console\", \"debugger\"]"));
    assert!(package.contains("node scripts/check-production-bundle.mjs"));
    assert!(package.contains("node scripts/check-macos-app-bundle.mjs"));
    assert!(package.contains(
        "\"check:macos-signature-entitlements\": \"node scripts/check-macos-app-bundle.mjs --verify-app-entitlements\""
    ));
    assert!(package.contains(
        "tauri build --bundles dmg --no-sign && node scripts/check-macos-app-bundle.mjs --dmg"
    ));
    assert!(checker.contains("console\\s*\\."));
    assert!(checker.contains("debugger"));
}
