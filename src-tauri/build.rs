mod app_commands;
mod build_policy;

fn main() {
    let debug_meeting_tools = std::env::var_os("CARGO_FEATURE_MEETING_CAPTURE_DEBUG").is_some();
    let profile = std::env::var("PROFILE").ok();

    if build_policy::rejects_debug_tooling(debug_meeting_tools, profile.as_deref()) {
        panic!("meeting-capture-debug enables Tauri devtools and is allowed only in debug builds");
    }

    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(app_commands::APP_COMMANDS)),
    )
    .expect("failed to run Tauri build script")
}
