//! User-visible error notifications via `tauri-plugin-notification`.
//!
//! SPEC §2.1 bullet 3: reserved for "real errors the user can't otherwise
//! see" — specifically transcription failures and paste rejections that
//! *aren't* the Accessibility-not-granted case (that one
//! has its own amber banner on Home + a dedicated Settings → Permissions
//! row).
//!
//! Normal-release captures that produce no audio or no usable speech receive
//! actionable device-aware feedback. Esc-cancel remains intentionally silent.
//!
//! Every call is best-effort: if the user hasn't granted Notifications
//! yet (or the permission has been revoked) the send is swallowed so a
//! notification failure never cascades into the dictation pipeline.

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

/// Post a transient notification. macOS shows it in the top-right and
/// auto-dismisses after ~4–5 s by default. First ever call triggers the
/// OS "Allow Notifications" grant — after that it's silent plumbing.
pub fn error(app: &AppHandle, title: &str, body: &str) {
    post(app, title, body);
}

/// Post a non-error notification — success, completion, "ready to use"
/// type messages. Visually identical to `error` on macOS (the OS doesn't
/// style notifications by severity), but split out so call sites read
/// at their intent level rather than all piling into `error()`.
pub fn info(app: &AppHandle, title: &str, body: &str) {
    post(app, title, body);
}

fn post(app: &AppHandle, title: &str, body: &str) {
    // Notification bodies may contain a user-named microphone. Keep rendered
    // body text out of diagnostics; never add transcript/audio content here.
    crate::murmur_dev_log!("[murmur] notify: {title}");
    let (title, body) =
        crate::native_notifications::localized(crate::native_language::current(), title, body);
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        // Don't propagate — nothing calls this on a path that can react
        // usefully to a notification send failure. Log and move on.
        crate::murmur_dev_log!("[murmur] notification send failed: {e}");
    }
}
