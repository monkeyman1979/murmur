use serde::Serialize;
use tauri::{AppHandle, Emitter};

const MAIN_WINDOW: &str = "main";
const INDICATOR_WINDOW: &str = "indicator";
const MEETING_CONTROLLER_WINDOW: &str = "meeting-controller";
const REMINDER_WINDOW: &str = "meeting-reminder";

/// Emit private application state only to the routed main window.
pub(crate) fn emit_main<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit_to(MAIN_WINDOW, event, payload);
}

/// Emit indicator-only presentation data. Device names are not required by
/// the main UI, so they stay confined to the transient indicator webview.
pub(crate) fn emit_indicator<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit_to(INDICATOR_WINDOW, event, payload);
}

/// Calendar prompt content is confined to its dedicated, least-privilege window.
pub(crate) fn emit_reminder<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit_to(REMINDER_WINDOW, event, payload);
}

/// Dictation level/state is consumed by both the indicator and the main
/// window's onboarding shortcut tester, but never by the meeting controller.
pub(crate) fn emit_dictation_ui<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit_to(MAIN_WINDOW, event, payload.clone());
    let _ = app.emit_to(INDICATOR_WINDOW, event, payload);
}

/// Live meeting state drives the routed meeting library and the floating
/// controller. It is never delivered to the dictation indicator.
pub(crate) fn emit_meeting_ui<S: Serialize + Clone>(app: &AppHandle, event: &str, payload: S) {
    let _ = app.emit_to(MAIN_WINDOW, event, payload.clone());
    let _ = app.emit_to(MEETING_CONTROLLER_WINDOW, event, payload);
}
