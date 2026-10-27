use std::collections::BTreeSet;
use std::path::PathBuf;

mod command_policy {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/app_commands.rs"));
}

fn manifest_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn capability(name: &str) -> serde_json::Value {
    let path = manifest_path(&format!("capabilities/{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("read capability"))
        .expect("parse capability")
}

fn string_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn command_permission(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

fn app_command_permissions(capability: &serde_json::Value) -> BTreeSet<String> {
    let known = command_policy::APP_COMMANDS
        .iter()
        .map(|command| command_permission(command))
        .collect::<BTreeSet<_>>();
    capability["permissions"]
        .as_array()
        .expect("permissions array")
        .iter()
        .filter_map(|permission| permission.as_str().map(str::to_owned))
        .filter(|permission| known.contains(permission))
        .collect()
}

#[test]
fn app_manifest_commands_match_registered_handlers() {
    let source = std::fs::read_to_string(manifest_path("src/lib.rs")).expect("read lib.rs");
    let block = source
        .split_once(".invoke_handler(tauri::generate_handler![")
        .expect("generate_handler start")
        .1
        .split_once("])")
        .expect("generate_handler end")
        .0;
    let registered = block
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            (!entry.is_empty()).then(|| entry.rsplit("::").next().unwrap().to_owned())
        })
        .collect::<BTreeSet<_>>();
    let declared = string_set(command_policy::APP_COMMANDS);

    assert_eq!(
        declared, registered,
        "Tauri ACL manifest and invoke handler diverged"
    );
}

#[test]
fn each_window_has_one_exact_capability() {
    let capability_directory = manifest_path("capabilities");
    let files = std::fs::read_dir(&capability_directory)
        .expect("read capabilities")
        .map(|entry| entry.expect("capability entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        files,
        string_set(&[
            "indicator.json",
            "main.json",
            "meeting-controller.json",
            "meeting-reminder.json"
        ]),
        "an extra capability could add authority to a matching window"
    );

    for label in [
        "main",
        "indicator",
        "meeting-controller",
        "meeting-reminder",
    ] {
        let value = capability(label);
        assert_eq!(value["identifier"], label);
        assert_eq!(value["windows"], serde_json::json!([label]));
    }
}

#[test]
fn hidden_main_window_keeps_durable_reminder_acknowledgments_running() {
    // Native timers cannot show an offer until this webview persists its
    // occurrence key. WKWebView's default hidden-view suspension delays that.
    for file in ["tauri.conf.json", "tauri.debug.conf.json"] {
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(manifest_path(file)).unwrap()).unwrap();
        let main = config["app"]["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|window| window["label"] == "main")
            .unwrap();
        assert_eq!(main["backgroundThrottling"], "disabled", "{file}");
    }
}

#[test]
fn main_window_grants_only_its_audited_command_surface() {
    let actual = app_command_permissions(&capability("main"));
    let expected = command_policy::MAIN_WINDOW_COMMANDS
        .iter()
        .map(|command| command_permission(command))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn only_main_can_open_the_exact_project_urls() {
    let expected = serde_json::json!({
        "identifier": "opener:allow-open-url",
        "allow": [
            { "url": "https://github.com/monkeyman1979/murmur/blob/main/docs/PRIVACY.md" },
            { "url": "https://github.com/monkeyman1979/murmur/issues/new?template=bug_report.yml" }
        ]
    });
    for label in [
        "main",
        "indicator",
        "meeting-controller",
        "meeting-reminder",
    ] {
        let capability = capability(label);
        let opener_permissions = capability["permissions"]
            .as_array()
            .expect("permissions array")
            .iter()
            .filter(|permission| {
                permission
                    .as_str()
                    .is_some_and(|name| name.starts_with("opener:"))
                    || permission["identifier"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("opener:"))
            })
            .collect::<Vec<_>>();
        if label == "main" {
            assert_eq!(opener_permissions, vec![&expected]);
        } else {
            assert!(
                opener_permissions.is_empty(),
                "{label} must not open external URLs"
            );
        }
    }
}

#[test]
fn indicator_has_no_application_command_authority() {
    let value = capability("indicator");
    assert!(app_command_permissions(&value).is_empty());
    assert_eq!(
        value["permissions"],
        serde_json::json!(["core:event:allow-listen", "core:event:allow-unlisten"])
    );
}

#[test]
fn meeting_controller_grants_only_existing_recording_controls() {
    let value = capability("meeting-controller");
    let actual = app_command_permissions(&value);
    let expected = command_policy::MEETING_CONTROLLER_COMMANDS
        .iter()
        .map(|command| command_permission(command))
        .collect();
    assert_eq!(actual, expected);

    let permissions = value["permissions"].as_array().expect("permissions array");
    assert!(permissions.contains(&serde_json::json!("core:event:allow-listen")));
    assert!(permissions.contains(&serde_json::json!("core:event:allow-unlisten")));
    assert!(permissions.contains(&serde_json::json!("core:window:allow-start-dragging")));
    assert!(!permissions.contains(&serde_json::json!("core:event:allow-emit")));
    assert!(!permissions.contains(&serde_json::json!("core:event:allow-emit-to")));

    for forbidden in [
        "capture_read_transcript",
        "capture_delete_with_cleanup",
        "capture_save_audio_copy",
        "activate_license",
        "deactivate_license",
        "download_model",
        "delete_model",
        "set_active_stt",
        "set_capture_output_directory",
        "meeting_recorder_start",
        "capture_recovery_discover",
    ] {
        assert!(
            !actual.contains(&command_permission(forbidden)),
            "unexpected authority: {forbidden}"
        );
    }
}

#[test]
fn reminder_can_only_read_and_respond_to_its_offer() {
    let value = capability("meeting-reminder");
    let actual = app_command_permissions(&value);
    let expected = command_policy::REMINDER_WINDOW_COMMANDS
        .iter()
        .map(|command| command_permission(command))
        .collect();
    assert_eq!(actual, expected);
    assert!(!actual.contains(&command_permission("meeting_recorder_start")));
    assert!(!actual.contains(&command_permission("configure_calendar_reminders")));
    assert!(!actual.contains(&command_permission("read_calendar_snapshot")));
}

fn rust_sources(directory: &std::path::Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("read Rust source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

#[test]
fn rust_events_are_never_broadcast_to_every_webview() {
    let source_root = manifest_path("src");
    let mut sources = Vec::new();
    rust_sources(&source_root, &mut sources);

    for path in sources {
        let source = std::fs::read_to_string(&path).expect("read Rust source");
        assert!(
            !source.contains(".emit("),
            "{} broadcasts an event instead of using a window-scoped helper",
            path.display()
        );
    }
}
