//! Live state for the three macOS permissions Murmur depends on, plus
//! the deep-link command that opens System Settings at the right pane.
//!
//! - **Microphone** — `AVCaptureDevice.authorizationStatusForMediaType(AVMediaTypeAudio)`.
//! - **Input Monitoring** — event listening for the hotkey's CoreGraphics
//!   tap: `CGPreflightListenEventAccess()`, or a live tap.
//! - **Accessibility** — reuses `insertion::is_trusted()`.
//!
//! On the macOS 27.0.1 installed-app test, TCC resolved event listening
//! through a composition chain
//! (ListenEvent → PostEvent → Accessibility). An app with no Input
//! Monitoring record inherits its Accessibility decision, so:
//!
//! - granting Accessibility also authorizes the listen-only keyboard tap,
//!   without an Input Monitoring entry; and
//! - once Accessibility is recorded as off (macOS records that the first
//!   time an untrusted process touches the AX API), an Input Monitoring
//!   request is answered from that record — no prompt, no Settings row.
//!
//! Onboarding therefore asks for Accessibility and treats Input Monitoring
//! as a fallback. In that test, `CGPreflightListenEventAccess` retained its
//! earlier answer after a grant, so an installed, enabled tap
//! (`hotkey::listener_active`) also counts.
//!
//! A background task polls all three every 2 s and emits
//! `murmur://permissions` when anything changes, so the Permissions
//! page reacts to grants/revokes without the user having to relaunch.

use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;

use crate::insertion;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionState {
    Granted,
    Denied,
    NotDetermined,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PermissionSnapshot {
    pub microphone: PermissionState,
    pub input_monitoring: PermissionState,
    pub accessibility: PermissionState,
}

pub fn current() -> PermissionSnapshot {
    #[cfg(target_os = "macos")]
    {
        // This function is called by the background permission watcher every
        // two seconds for the lifetime of the app. Keep an explicit
        // autorelease pool around the AppKit/AVFoundation/IOKit calls so any
        // ObjC temporaries created by those frameworks are drained on each
        // tick instead of accumulating on a Tokio worker thread while Murmur
        // is otherwise idle.
        objc2::rc::autoreleasepool(|_| current_unpooled())
    }
    #[cfg(not(target_os = "macos"))]
    {
        current_unpooled()
    }
}

fn current_unpooled() -> PermissionSnapshot {
    PermissionSnapshot {
        microphone: mic_status(),
        input_monitoring: input_monitoring_status(),
        accessibility: accessibility_status(),
    }
}

fn accessibility_status() -> PermissionState {
    if insertion::is_trusted() {
        PermissionState::Granted
    } else {
        // Accessibility has no "not determined" — the process is either
        // in TCC's trusted list or it isn't.
        PermissionState::Denied
    }
}

#[cfg(target_os = "macos")]
fn mic_status() -> PermissionState {
    mac::mic_status()
}
#[cfg(not(target_os = "macos"))]
fn mic_status() -> PermissionState {
    PermissionState::Unknown
}

#[cfg(target_os = "macos")]
pub fn input_monitoring_status() -> PermissionState {
    input_monitoring_state(listen_event_preflight(), crate::hotkey::listener_active())
}
#[cfg(not(target_os = "macos"))]
pub fn input_monitoring_status() -> PermissionState {
    PermissionState::Unknown
}

/// Raw `CGPreflightListenEventAccess()`. The hotkey listener uses this
/// rather than [`input_monitoring_status`], which also counts its own tap.
#[cfg(target_os = "macos")]
pub fn listen_event_preflight() -> bool {
    mac::listen_event_preflight()
}
#[cfg(not(target_os = "macos"))]
pub fn listen_event_preflight() -> bool {
    false
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn input_monitoring_state(preflight: bool, listener_active: bool) -> PermissionState {
    if preflight || listener_active {
        PermissionState::Granted
    } else {
        // CoreGraphics returns only a bool; it cannot distinguish a
        // previous denial from a first-time request.
        PermissionState::Denied
    }
}

#[cfg(target_os = "macos")]
pub fn request_input_monitoring_access() -> PermissionState {
    mac::request_input_monitoring_access()
}
#[cfg(not(target_os = "macos"))]
pub fn request_input_monitoring_access() -> PermissionState {
    PermissionState::Unknown
}

/// Poll [`current`] every 2 s and emit `murmur://permissions` on every
/// change.
pub fn spawn_watcher(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last: Option<PermissionSnapshot> = None;
        loop {
            let snapshot = current();
            if Some(snapshot) != last {
                crate::murmur_dev_log!(
                    "[murmur] permissions: mic={:?} input={:?} accessibility={:?}",
                    snapshot.microphone,
                    snapshot.input_monitoring,
                    snapshot.accessibility
                );
                crate::window_events::emit_main(&app, "murmur://permissions", snapshot);
                last = Some(snapshot);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

/// Tauri command — snapshot of all three permission states right now.
/// Safe to call from the frontend on boot to hydrate before the
/// watcher's first emit lands.
#[tauri::command]
pub fn get_permissions() -> PermissionSnapshot {
    current()
}

/// Trigger the macOS microphone TCC record. On a status of
/// `NotDetermined`, this shows the native "Murmur would like to
/// access the microphone" prompt and adds Murmur to System
/// Settings → Privacy & Security → Microphone. On `Granted` or
/// `Denied` it's a no-op (prompt is suppressed by macOS).
///
/// Called from the UI via `request_microphone_prompt` when the
/// user clicks a Grant button. **Not** called at app startup —
/// we don't want native prompts stacking on top of the onboarding
/// modal before the user has interacted with it.
#[cfg(target_os = "macos")]
pub fn request_mic_access() {
    mac::request_mic_access();
}
#[cfg(not(target_os = "macos"))]
pub fn request_mic_access() {}

/// Tauri command — fired by the Grant button on the Microphone
/// PermissionRow. Returns immediately; the 2 s permissions watcher
/// picks up the resulting state change and the UI reacts to that.
#[tauri::command]
pub fn request_microphone_prompt() {
    request_mic_access();
}

/// Tauri command — ask macOS for event-listening access used by our
/// `CGEventTap`. Onboarding only offers this as a fallback after
/// Accessibility is granted; on macOS 27 the Accessibility grant normally
/// covers listening already (see the module docs).
#[tauri::command]
pub fn request_input_monitoring_prompt() -> PermissionState {
    request_input_monitoring_access()
}

/// Tauri command — quit the app and relaunch. Surfaced to the
/// onboarding Permissions step so users can recover from the macOS
/// TCC quirk where Input Monitoring and Accessibility grants don't
/// apply to the running process until it restarts. After relaunch
/// the persisted onboarding step (localStorage key
/// `murmur:onboarding-step-key`) resumes the user where they left off,
/// with the newly-granted permissions now reflected in the row
/// status.
#[tauri::command]
pub fn restart_murmur(
    app: tauri::AppHandle,
    update_gate: tauri::State<'_, std::sync::Arc<crate::update_install::UpdateInstallGate>>,
) -> Result<(), String> {
    if !update_gate.try_claim_install() {
        return Err("Finish current work before restarting Murmur.".to_string());
    }
    // `restart` does not return. Keep exclusive admission claimed if the
    // platform unexpectedly fails to terminate the process.
    app.restart();
}

/// Tauri command — open System Settings directly at the named Privacy
/// pane. `pane` must be one of `"microphone"`, `"audio-capture"`,
/// `"input-monitoring"`, `"accessibility"`.
fn privacy_pane_url(pane: &str) -> Result<&'static str, String> {
    match pane {
        "microphone" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        }
        // "System Audio Recording Only" — the bucket process taps land in.
        "audio-capture" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_AudioCapture")
        }
        "input-monitoring" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
        }
        "accessibility" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
        _ => Err("That privacy settings pane is not available.".to_string()),
    }
}

#[tauri::command]
pub fn open_privacy_pane(pane: String) -> Result<(), String> {
    let url = privacy_pane_url(&pane)?;
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map_err(|_| "Murmur could not open System Settings.".to_string())?;
    Ok(())
}

#[cfg(target_os = "macos")]
mod mac {
    use super::PermissionState;
    use block2::StackBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, Bool};
    use objc2_foundation::ns_string;

    // Referencing the AVFoundation framework forces it into the link
    // step; without this, `AnyClass::get(c"AVCaptureDevice")` returns
    // None because the class hasn't been loaded into the Obj-C runtime.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    // The hotkey listens with a CoreGraphics CGEventTap, not an IOHIDManager
    // or IOHIDDevice, so use the matching CoreGraphics pair (CGEvent.h).
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
    }

    // AVAuthorizationStatus enum values, from AVCaptureDevice.h.
    const AV_STATUS_NOT_DETERMINED: isize = 0;
    const AV_STATUS_RESTRICTED: isize = 1;
    const AV_STATUS_DENIED: isize = 2;
    const AV_STATUS_AUTHORIZED: isize = 3;

    pub(super) fn mic_status() -> PermissionState {
        unsafe {
            let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
                return PermissionState::Unknown;
            };
            // AVMediaTypeAudio is a CFString constant that equals "soun".
            // Use a static NSString so the 2 s permission poll does not
            // allocate a new ObjC string on every idle tick.
            let media_type = ns_string!("soun");
            let status: isize = msg_send![cls, authorizationStatusForMediaType: media_type];
            match status {
                AV_STATUS_NOT_DETERMINED => PermissionState::NotDetermined,
                AV_STATUS_RESTRICTED | AV_STATUS_DENIED => PermissionState::Denied,
                AV_STATUS_AUTHORIZED => PermissionState::Granted,
                _ => PermissionState::Unknown,
            }
        }
    }

    pub(super) fn listen_event_preflight() -> bool {
        unsafe { CGPreflightListenEventAccess() }
    }

    pub(super) fn request_input_monitoring_access() -> PermissionState {
        if super::input_monitoring_status() != PermissionState::Granted {
            // "Potentially prompting" per CGEvent.h. Its return value is
            // potentially the same cached answer as the preflight, so
            // re-read status, which also checks the running listener.
            unsafe { CGRequestListenEventAccess() };
        }
        super::input_monitoring_status()
    }

    pub(super) fn request_mic_access() {
        unsafe {
            let Some(cls) = AnyClass::get(c"AVCaptureDevice") else {
                crate::murmur_dev_log!(
                    "[murmur] AVCaptureDevice class unavailable — skipping mic TCC request"
                );
                return;
            };
            // AVMediaTypeAudio is the CFString "soun" — matches the
            // constant used in `mic_status()` above.
            let media_type = ns_string!("soun");
            // The completion handler signature on the Obj-C side is
            // `void (^)(BOOL)`. Objective-C BOOL is a signed-char
            // wrapper (`objc2::runtime::Bool`), not Rust's `bool`, so
            // the closure arg must take `Bool` — otherwise `IntoBlock`
            // can't synthesize an ABI-compatible trampoline. We don't
            // act on the result here; the permissions watcher polls
            // every 2 s and the UI reacts to that stream. Retain the
            // block through `copy()` so AVFoundation can safely hold
            // it if the prompt is deferred (user takes time to answer
            // "Allow / Don't Allow").
            let handler = StackBlock::new(|_granted: Bool| {}).copy();
            let _: () = msg_send![
                cls,
                requestAccessForMediaType: media_type,
                completionHandler: &*handler,
            ];
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn listen_access_pair_has_boolean_abi() {
            // Compile-time signatures only; never prompt in tests.
            let _: unsafe extern "C" fn() -> bool = CGPreflightListenEventAccess;
            let _: unsafe extern "C" fn() -> bool = CGRequestListenEventAccess;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_monitoring_counts_preflight_or_live_listener() {
        assert_eq!(
            input_monitoring_state(true, false),
            PermissionState::Granted
        );
        // Preflight is cached from launch; a tap created after an
        // Accessibility grant still proves listening is authorized.
        assert_eq!(
            input_monitoring_state(false, true),
            PermissionState::Granted
        );
        assert_eq!(
            input_monitoring_state(false, false),
            PermissionState::Denied
        );
    }

    #[test]
    fn meeting_audio_capture_pane_is_allowlisted() {
        assert!(privacy_pane_url("audio-capture")
            .unwrap()
            .contains("Privacy_AudioCapture"));
        // The screen-recording pane is deliberately no longer reachable:
        // meeting capture does not use that permission any more.
        assert!(privacy_pane_url("screen-recording").is_err());
        assert!(privacy_pane_url("not-a-pane").is_err());
    }
}
