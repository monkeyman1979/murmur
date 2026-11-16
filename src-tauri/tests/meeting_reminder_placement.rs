const REMINDERS: &str = include_str!("../src/calendar/reminders.rs");
const WINDOW: &str = include_str!("../src/calendar/reminder_window.rs");

#[test]
fn reminders_use_their_own_placement_and_nonactivating_reveal() {
    assert!(
        REMINDERS.contains("super::reminder_window::place(&app, &window, token, focused, mtm)?")
    );
    assert!(REMINDERS.contains("crate::indicator_native::show_without_focus(&window)"));
    assert!(!REMINDERS.contains("crate::meeting_capture::place_meeting_window"));
    assert!(!REMINDERS.contains("crate::meeting_capture::show_meeting_window_without_focus"));
}

#[test]
fn native_reminder_placement_only_moves_the_full_size_prompt_to_top_right() {
    let placement = WINDOW;
    assert!(placement.contains("target_screen(None, focused, mtm)"));
    assert!(placement.contains("crate::indicator_native::focused_window_center"));
    assert!(!placement.contains("get_webview_window(\"main\")"));
    assert!(placement.contains("screen.visibleFrame()"));
    assert!(placement.contains("reminder_offsets("));
    assert!(placement.contains("window.frame().size"));
    assert!(placement.contains("window.setFrameOrigin("));
    // No saved feedback preferences, compact resizing, presentation state,
    // hover tracking or laptop recording-controller mirror side effects.
    for forbidden in [
        "indicator_position::current",
        "indicator_presentation",
        "preferences.style",
        "MEETING_NOTCH",
        "setFrame_display",
        "setFrameSize",
    ] {
        assert!(!placement.contains(forbidden), "unexpected {forbidden}");
    }
}

#[test]
fn display_observer_is_scoped_to_the_live_offer_not_focus_or_pointer_changes() {
    assert!(REMINDERS.contains("super::reminder_window::stop(app)"));
    assert!(WINDOW.contains("NSApplicationDidChangeScreenParametersNotification"));
    assert!(WINDOW.contains("removeObserver("));
    assert!(WINDOW.contains("placement.token == token"));
    assert!(WINDOW.contains("pinned_display(&token).is_some()"));
    assert!(WINDOW.contains("spawn_blocking(crate::indicator_native::focused_window_center)"));
    assert!(!WINDOW.contains("addGlobalMonitorForEvents"));
    assert!(!WINDOW.contains("NSWorkspaceDidActivateApplicationNotification"));
}

#[test]
fn asynchronous_focus_lookup_never_holds_the_engine_lock_while_waiting_for_appkit() {
    let command = REMINDERS
        .split("pub async fn present_calendar_reminder(")
        .nth(1)
        .unwrap()
        .split("fn present_current_reminder(")
        .next()
        .unwrap();
    assert!(command.contains("focused_window_center().await"));
    assert!(command.contains("app.run_on_main_thread("));
    let compact: String = command.split_whitespace().collect();
    assert!(compact.contains("receive.await"));
    assert!(!command.contains("engine.lock()"));
    assert!(!command.contains("ns_window()"));
    assert!(WINDOW.contains("mtm: MainThreadMarker"));
}

#[test]
fn production_and_debug_prompts_keep_room_for_both_actions() {
    for config in [
        include_str!("../tauri.conf.json"),
        include_str!("../tauri.debug.conf.json"),
    ] {
        let config: serde_json::Value = serde_json::from_str(config).unwrap();
        let window = config["app"]["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|window| window["label"] == "meeting-reminder")
            .unwrap();
        let width = window["width"].as_f64().unwrap();
        let height = window["height"].as_f64().unwrap();
        assert_eq!((width, height), (360.0, 208.0));
    }
}
