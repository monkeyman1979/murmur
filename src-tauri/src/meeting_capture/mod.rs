//! Production Meeting Transcription capture and disk-backed recorder.
//!
//! Shipping macOS builds enable `meeting-capture` by default. Core Audio
//! process-tap capture remains separate from short dictation, while explicit
//! `meeting-capture-debug` builds add native diagnostics and raw MT0 commands.

mod arbiter;
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
mod clock;
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
mod safeguards;

/// Phase A2: places independent capture sources on one media timeline. Pure
/// arithmetic — no CoreAudio — so it builds and tests everywhere.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
mod timeline;

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
mod recorder;

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
mod writer;

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
mod macos;

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
mod source;

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
mod tap;

pub use arbiter::AudioCaptureArbiter;
pub(crate) use arbiter::{AudioCaptureLease, AudioCaptureOwner};

// Debug-only native capture diagnostics. They write content-free lifecycle,
// timing, and device metadata to a fixed local path so audio-route failures can
// be investigated without exposing transcript or audio content. The release
// profile guard in build.rs prevents this path (and Tauri devtools) from
// entering a production bundle.
#[cfg(all(target_os = "macos", feature = "meeting-capture-debug"))]
pub(crate) fn diag_log(msg: &str) {
    use std::io::Write;
    let line = format!(
        "{} {msg}\n",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/murmur-native-diag.log")
    {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(all(target_os = "macos", feature = "meeting-capture-debug"))]
#[macro_export]
macro_rules! murmur_diag {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        $crate::murmur_dev_log!("{msg}");
        $crate::meeting_capture::diag_log(&msg);
    }};
}

#[cfg(not(all(target_os = "macos", feature = "meeting-capture-debug")))]
#[macro_export]
macro_rules! murmur_diag {
    ($($arg:tt)*) => {{
        let _ = format_args!($($arg)*);
    }};
}

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
use {
    objc2::MainThreadMarker,
    objc2_app_kit::{NSScreen, NSWindow},
    objc2_foundation::NSPoint,
    tauri::Manager,
};

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
use crate::captures::CaptureState;
#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
use crate::captures::NewCaptureChunk;
use crate::captures::{CaptureError, CaptureErrorCode, CaptureRecord};
use crate::history::TranscriptStore;
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
use recorder::RecorderStopResult;
use serde::{Deserialize, Serialize};

pub(crate) const QUEUE_CAPACITY: usize = 32;
#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
pub(crate) const MAX_CALLBACK_BYTES: usize = 1024 * 1024;
pub(crate) const REGISTERED_OUTPUTS: [&str; 2] = ["system_audio", "microphone"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mt0ErrorCode {
    FeatureDisabled,
    UnsupportedPlatform,
    UnsupportedMacos,
    MicrophonePermissionDenied,
    /// The audio-capture TCC grant ("System Audio Recording Only") is missing
    /// or was denied. CoreAudio reports this as OSStatus `'nope'`.
    SystemAudioPermissionDenied,
    SourceUnavailable,
    SourceEnded,
    CaptureBusy,
    SystemAudioStartFailed,
    MicrophoneStartFailed,
    StreamStartFailed,
    WriterQueueOverflow,
    CallbackBufferTooLarge,
    UnsupportedAudioFormat,
    AudioFormatChanged,
    MissingAudioTrack,
    WavCreateFailed,
    WavWriteFailed,
    TeardownFailed,
    NotActive,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mt0Error {
    pub code: Mt0ErrorCode,
    pub message: String,
}

impl Mt0Error {
    pub(crate) fn new(code: Mt0ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Mt0Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Mt0Error {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mt0TrackReport {
    pub path: Option<PathBuf>,
    pub frames: u64,
    pub input_bytes: u64,
    pub queue_high_water: usize,
    pub first_pts_ns: Option<i64>,
    pub last_pts_ns: Option<i64>,
    pub inserted_silence_frames: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mt0Report {
    pub state: String,
    pub run_directory: Option<PathBuf>,
    pub registered_outputs: Vec<String>,
    pub queue_capacity: usize,
    pub started_at_unix_ms: Option<u128>,
    pub elapsed_ms: u64,
    pub system_audio: Mt0TrackReport,
    pub microphone: Mt0TrackReport,
    pub terminal_error: Option<Mt0Error>,
}

impl Default for Mt0Report {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            run_directory: None,
            registered_outputs: REGISTERED_OUTPUTS.iter().map(|s| (*s).into()).collect(),
            queue_capacity: QUEUE_CAPACITY,
            started_at_unix_ms: None,
            elapsed_ms: 0,
            system_audio: Mt0TrackReport::default(),
            microphone: Mt0TrackReport::default(),
            terminal_error: None,
        }
    }
}

#[derive(Clone)]
pub struct Mt0State {
    report: Arc<Mutex<Mt0Report>>,
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    controller: Arc<macos::Controller>,
}

impl Default for Mt0State {
    fn default() -> Self {
        Self::new()
    }
}

impl Mt0State {
    pub fn new() -> Self {
        let report = Arc::new(Mutex::new(Mt0Report::default()));
        Self {
            #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
            controller: Arc::new(macos::Controller::spawn(Arc::clone(&report))),
            report,
        }
    }

    #[cfg(not(all(target_os = "macos", feature = "meeting-capture-debug")))]
    fn unavailable_error() -> Mt0Error {
        #[cfg(not(target_os = "macos"))]
        return Mt0Error::new(
            Mt0ErrorCode::UnsupportedPlatform,
            "MT0 Core Audio capture diagnostics are available only on macOS",
        );
        #[cfg(all(target_os = "macos", not(feature = "meeting-capture-debug")))]
        return Mt0Error::new(
            Mt0ErrorCode::FeatureDisabled,
            "rebuild with --features meeting-capture-debug to use MT0 diagnostics",
        );
        #[allow(unreachable_code)]
        Mt0Error::new(Mt0ErrorCode::Internal, "MT0 unavailable")
    }
}

#[tauri::command]
pub async fn mt0_start(
    _app: tauri::AppHandle,
    _state: tauri::State<'_, Mt0State>,
    _arbiter: tauri::State<'_, Arc<AudioCaptureArbiter>>,
    output_directory: PathBuf,
) -> Result<Mt0Report, Mt0Error> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture-debug"))]
    {
        let lease = Arc::clone(_arbiter.inner())
            .reserve(AudioCaptureOwner::Meeting)
            .map_err(|_| {
                Mt0Error::new(Mt0ErrorCode::CaptureBusy, "another audio capture is active")
            })?;
        let controller = Arc::clone(&_state.controller);
        tokio::task::spawn_blocking(move || controller.start(output_directory, lease))
            .await
            .map_err(|e| Mt0Error::new(Mt0ErrorCode::Internal, format!("MT0 start task: {e}")))?
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture-debug")))]
    {
        let _ = output_directory;
        Err(Mt0State::unavailable_error())
    }
}

#[tauri::command]
pub async fn mt0_stop(_state: tauri::State<'_, Mt0State>) -> Result<Mt0Report, Mt0Error> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture-debug"))]
    {
        let controller = Arc::clone(&_state.controller);
        tokio::task::spawn_blocking(move || controller.stop())
            .await
            .map_err(|e| Mt0Error::new(Mt0ErrorCode::Internal, format!("MT0 stop task: {e}")))?
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture-debug")))]
    Err(Mt0State::unavailable_error())
}

#[tauri::command]
pub fn mt0_status(state: tauri::State<'_, Mt0State>) -> Mt0Report {
    state.report.lock().clone()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRecorderEvent {
    pub capture_id: i64,
    pub status: String,
    pub error_code: Option<CaptureErrorCode>,
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
fn hide_meeting_controller(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("meeting-controller") {
        let _ = window.hide();
    }
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
struct MeetingControllerVisibilityGuard {
    app: tauri::AppHandle,
    keep_visible: bool,
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
impl MeetingControllerVisibilityGuard {
    fn new(app: &tauri::AppHandle) -> Self {
        Self {
            app: app.clone(),
            keep_visible: false,
        }
    }

    fn keep_visible(&mut self) {
        self.keep_visible = true;
    }
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
impl Drop for MeetingControllerVisibilityGuard {
    fn drop(&mut self) {
        if !self.keep_visible {
            hide_meeting_controller(&self.app);
        }
    }
}

/// Park the floating controller in the top-right of the display containing
/// Murmur's main window, just below the menu bar.
///
/// This deliberately uses AppKit's `NSWindow.screen` and `NSScreen.visibleFrame`
/// instead of Tauri's cursor/monitor coordinate path. Tao currently scales the
/// global macOS cursor using the primary display's factor, which can select the
/// wrong display on mixed-scale setups. It can also convert a physical target
/// through the controller's previous display scale while moving the hidden
/// controller, producing the seemingly random position seen on an ultrawide.
///
/// AppKit supplies the main window's screen and usable frame in one consistent
/// logical coordinate space. The only fixed value is the desired 8-point inset;
/// the display origin, size, menu bar, Dock, and controller size are all native.
///
/// Only ever called for a controller that is not currently visible — see the
/// caller for why.
#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
fn place_meeting_controller(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    const MARGIN: f64 = 8.0;

    let Some(main_window) = app.get_webview_window("main") else {
        crate::murmur_diag!("[murmur][mc-diag] main window unavailable, leaving default position");
        return;
    };
    let Ok(main_ns_window) = main_window.ns_window() else {
        crate::murmur_diag!("[murmur][mc-diag] main ns_window() failed, leaving default position");
        return;
    };
    let Ok(controller_ns_window) = window.ns_window() else {
        crate::murmur_diag!(
            "[murmur][mc-diag] controller ns_window() failed, leaving default position"
        );
        return;
    };

    // Raw AppKit handles are valid for the lifetime of their Tauri windows.
    // Pass their addresses as integers because the main-thread closure must be
    // `Send`; they are cast back and dereferenced only on the AppKit thread.
    let main_ns_window = main_ns_window as usize;
    let controller_ns_window = controller_ns_window as usize;
    if let Err(error) = window.run_on_main_thread(move || unsafe {
        let main_window = &*(main_ns_window as *mut NSWindow);
        let controller = &*(controller_ns_window as *mut NSWindow);
        let mtm = MainThreadMarker::new_unchecked();
        let screen = main_window.screen().or_else(|| NSScreen::mainScreen(mtm));
        let Some(screen) = screen else {
            crate::murmur_diag!(
                "[murmur][mc-diag] AppKit found no target screen, leaving default position"
            );
            return;
        };

        let work_area = NSScreen::visibleFrame(&screen);
        let controller_frame = NSWindow::frame(controller);
        let x = work_area.origin.x + work_area.size.width - controller_frame.size.width - MARGIN;
        // `setFrameTopLeftPoint` expects the top edge in AppKit's bottom-left
        // coordinate system, so visibleFrame.maxY is exactly the desired base.
        let y = work_area.origin.y + work_area.size.height - MARGIN;

        crate::murmur_diag!(
            "[murmur][mc-diag] AppKit visible_frame=({:.1},{:.1},{:.1},{:.1}) \
             window=({:.1},{:.1}) -> top_left=({x:.1},{y:.1})",
            work_area.origin.x,
            work_area.origin.y,
            work_area.size.width,
            work_area.size.height,
            controller_frame.size.width,
            controller_frame.size.height,
        );
        controller.setFrameTopLeftPoint(NSPoint::new(x, y));
    }) {
        crate::murmur_diag!(
            "[murmur][mc-diag] scheduling AppKit controller placement failed: {error:?}"
        );
    }
}

/// Reveal the non-activating controller without making it the key window.
/// Tauri's `window.show()` calls `makeKeyAndOrderFront` on macOS, which can
/// steal focus from the main window (and make its next button click only
/// reactivate it). AppKit's `orderFront` preserves the current key window.
#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
fn show_meeting_controller_without_focus(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    let ns_window = window.ns_window()? as usize;
    // The native handle belongs to this app-managed window. Like placement,
    // dereference it only on AppKit's main thread while the window is alive.
    window.run_on_main_thread(move || unsafe {
        let controller = &*(ns_window as *mut NSWindow);
        controller.orderFront(None);
    })
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
fn emit_meeting_state(
    app: &tauri::AppHandle,
    capture_id: i64,
    status: CaptureState,
    error_code: Option<CaptureErrorCode>,
) {
    // TEMPORARY diagnostic logging (2026-08-01, meeting-transcription-unblock-plan
    // Phase 3): the controller window has never been observed to appear on a real
    // device despite this code path looking correct on paper, and every call here
    // previously swallowed its Result with `let _ =`. Remove once the root cause
    // is found and fixed — content-free (window ids/coordinates/booleans only).
    if let Some(window) = app.get_webview_window("meeting-controller") {
        crate::murmur_diag!(
            "[murmur][mc-diag] found meeting-controller window handle, status={status:?}"
        );
        if matches!(
            status,
            CaptureState::Recording
                | CaptureState::Pausing
                | CaptureState::Paused
                | CaptureState::Resuming
        ) {
            // State changes arrive for Pause and Resume too. Re-showing an
            // already-visible controller makes it key on macOS, defocusing the
            // main window and swallowing its next button click. Keep its
            // dragged position and current key window intact.
            let already_visible = window.is_visible().unwrap_or(false);
            if already_visible {
                crate::murmur_diag!("[murmur][mc-diag] already visible, leaving window alone");
            } else {
                place_meeting_controller(app, &window);
                match show_meeting_controller_without_focus(&window) {
                    Ok(()) => crate::murmur_diag!("[murmur][mc-diag] orderFront() ok"),
                    Err(e) => crate::murmur_diag!("[murmur][mc-diag] orderFront() FAILED: {e:?}"),
                }
            }
            match window.set_always_on_top(true) {
                Ok(()) => crate::murmur_diag!("[murmur][mc-diag] set_always_on_top ok"),
                Err(e) => crate::murmur_diag!("[murmur][mc-diag] set_always_on_top FAILED: {e:?}"),
            }
            match window.is_visible() {
                Ok(v) => crate::murmur_diag!("[murmur][mc-diag] is_visible() after show = {v}"),
                Err(e) => crate::murmur_diag!("[murmur][mc-diag] is_visible() FAILED: {e:?}"),
            }
        } else {
            crate::murmur_diag!("[murmur][mc-diag] status not in show-list, hiding");
            hide_meeting_controller(app);
        }
    } else {
        crate::murmur_diag!(
            "[murmur][mc-diag] get_webview_window(\"meeting-controller\") returned None"
        );
    }
    crate::window_events::emit_meeting_ui(
        app,
        "murmur://meeting-capture-state",
        MeetingRecorderEvent {
            capture_id,
            status: status.as_str().into(),
            error_code,
        },
    );
}

#[derive(Default)]
pub struct MeetingRecorderState {
    #[allow(dead_code)]
    lifecycle: Arc<tokio::sync::Mutex<MeetingLifecycle>>,
}

#[derive(Default)]
struct MeetingLifecycle {
    #[allow(dead_code)]
    active_capture_id: Option<i64>,
    #[allow(dead_code)]
    update_activity: Option<crate::update_install::UpdateActivityGuard>,
    recording_priority: Option<crate::captures::processing::RecordingPriorityGuard>,
}

impl MeetingLifecycle {
    #[cfg_attr(
        not(all(target_os = "macos", feature = "meeting-capture")),
        allow(dead_code)
    )]
    fn clear_active(&mut self) {
        self.active_capture_id = None;
        self.update_activity = None;
        self.recording_priority = None;
    }

    #[cfg_attr(
        not(all(target_os = "macos", feature = "meeting-capture")),
        allow(dead_code)
    )]
    fn transcription_handoff(&self) -> Option<crate::update_install::UpdateActivityGuard> {
        self.update_activity.clone()
    }
}

impl MeetingRecorderState {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecorderStatus {
    pub active_duration_ms: i64,
    pub paused_duration_ms: i64,
    pub paused: bool,
}

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
#[allow(dead_code)]
pub(crate) trait RecorderController: Send + Sync {
    fn pause_recorder(&self, capture_id: i64) -> Result<(), CaptureError>;
    fn resume_recorder(&self, capture_id: i64) -> Result<(), CaptureError>;
    fn status_recorder(&self, capture_id: i64) -> Result<RecorderStatus, CaptureError>;
    fn checkpoint_paused(
        &self,
        capture_id: i64,
        paused_duration_ms: i64,
    ) -> Result<(), CaptureError>;
    fn stop_recorder(&self, capture_id: i64) -> Result<RecorderStopResult, CaptureError>;
    fn cancel_recorder(&self, capture_id: i64) -> Result<RecorderStopResult, CaptureError>;
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
struct StoreCheckpoint(Arc<TranscriptStore>);

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
impl recorder::ChunkCheckpoint for StoreCheckpoint {
    fn commit_pair(
        &self,
        microphone: &NewCaptureChunk,
        system: &NewCaptureChunk,
        active_duration_ms: i64,
    ) -> Result<(), CaptureError> {
        self.0
            .commit_ready_chunk_pair(microphone, system, active_duration_ms)
    }
}

#[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
fn disabled_error() -> CaptureError {
    CaptureError::new(
        CaptureErrorCode::FeatureDisabled,
        "meeting capture is not included in this build",
    )
}

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
async fn persist_capture_failure(
    store: Arc<TranscriptStore>,
    capture_id: i64,
    error: &CaptureError,
) -> Result<CaptureRecord, CaptureError> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    crate::murmur_diag!(
        "[murmur][capture-failure] capture_id={capture_id} code={} message={}",
        error.code.as_str(),
        error.message
    );
    let failure = error.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let current = store.get_capture(capture_id)?;
        if current.status == CaptureState::CaptureFailed {
            return Ok(current);
        }
        store.transition_capture(
            capture_id,
            CaptureState::CaptureFailed,
            Some(failure.code),
            Some(failure.message),
        )
    })
    .await
    .map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::PersistenceUnavailable,
            "capture failure transition task failed",
        )
    })?
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
fn enqueue_transcription(
    app: &tauri::AppHandle,
    capture_id: i64,
    update_activity: Option<crate::update_install::UpdateActivityGuard>,
) {
    if let Some(coordinator) =
        app.try_state::<Arc<crate::captures::processing::MeetingTranscriptionCoordinator>>()
    {
        if let Some(update_activity) = update_activity {
            coordinator.enqueue_with_activity(capture_id, update_activity);
        } else {
            coordinator.enqueue(capture_id);
        }
    }
}

/// Native ownership is released before this function clears the lifecycle
/// owner. This is the only command-level failure/stop handoff: it makes a
/// failed SQLite transition unable to strand live Core Audio capture sources,
/// writer threads, or the arbiter lease.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
async fn terminalize_capture(
    store: Arc<TranscriptStore>,
    lifecycle: &mut MeetingLifecycle,
    controller: Arc<dyn RecorderController>,
    capture_id: i64,
    failure: Option<CaptureError>,
) -> Result<CaptureRecord, CaptureError> {
    let controller_for_stop = Arc::clone(&controller);
    let native_result = match tauri::async_runtime::spawn_blocking(move || {
        controller_for_stop.stop_recorder(capture_id)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => {
            // A blocking-task join failure is itself a teardown failure. Make
            // one direct compensating controller call before releasing the
            // lifecycle owner rather than leaving a live native owner behind.
            let fallback = CaptureError::new(
                CaptureErrorCode::CaptureInterrupted,
                "meeting native terminalization task failed",
            );
            let native_error = controller.stop_recorder(capture_id).err();
            let failure = failure.or(native_error).unwrap_or(fallback);
            let persisted = persist_capture_failure(Arc::clone(&store), capture_id, &failure).await;
            lifecycle.clear_active();
            return Err(persisted.err().unwrap_or(failure));
        }
    };

    let native_failure = native_result.as_ref().err().cloned();
    let failure = failure.or(native_failure);
    if let Some(failure) = failure {
        let persisted = persist_capture_failure(Arc::clone(&store), capture_id, &failure).await;
        // `stop_recorder` has synchronously drained native callbacks and
        // writers (or reported that native teardown failed) before ownership
        // is released. Recovery files are intentionally not touched here.
        lifecycle.clear_active();
        return match persisted {
            Ok(_capture) => Err(failure),
            Err(persistence_error) => Err(persistence_error),
        };
    }

    let result = native_result.expect("native failure handled above");
    let persisted = (|| {
        store.checkpoint_timing(
            capture_id,
            result.active_duration_ms,
            result.paused_duration_ms,
        )?;
        let current = store.get_capture(capture_id)?;
        let current = match current.status {
            CaptureState::Recording | CaptureState::Paused | CaptureState::Stopping => {
                store.transition_capture(capture_id, CaptureState::Stopping, None, None)?
            }
            CaptureState::Transcribing => current,
            _ => {
                return Err(CaptureError::new(
                    CaptureErrorCode::InvalidStateTransition,
                    "capture cannot be handed off after native terminalization",
                ))
            }
        };
        if current.status == CaptureState::Stopping {
            store.transition_capture(capture_id, CaptureState::Transcribing, None, None)
        } else {
            Ok(current)
        }
    })();

    match persisted {
        Ok(capture) => {
            lifecycle.clear_active();
            Ok(capture)
        }
        Err(error) => {
            // The native side is already terminal. Best effort failure
            // persistence keeps the staging directory discoverable if the
            // original handoff failed, and ownership is cleared only after
            // that reconciliation attempt completes.
            let failure_persisted =
                persist_capture_failure(Arc::clone(&store), capture_id, &error).await;
            lifecycle.clear_active();
            match failure_persisted {
                Ok(_) => Err(error),
                Err(persistence_error) => Err(persistence_error),
            }
        }
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn meeting_recorder_start(
    app: tauri::AppHandle,
    store: tauri::State<'_, Arc<TranscriptStore>>,
    arbiter: tauri::State<'_, Arc<AudioCaptureArbiter>>,
    meeting: tauri::State<'_, MeetingRecorderState>,
    native: tauri::State<'_, Mt0State>,
    settings: tauri::State<'_, Arc<crate::captures::processing::MeetingProcessingSettingsState>>,
    heavy_gate: tauri::State<'_, crate::captures::processing::HeavyProcessingGate>,
    coordinator: tauri::State<
        '_,
        Arc<crate::captures::processing::MeetingTranscriptionCoordinator>,
    >,
    output_device: tauri::State<'_, crate::audio_devices::OutputDeviceState>,
    update_gate: tauri::State<'_, Arc<crate::update_install::UpdateInstallGate>>,
    title: String,
) -> Result<CaptureRecord, CaptureError> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    {
        use tauri::Manager;

        let update_activity = Arc::clone(update_gate.inner())
            .begin_activity()
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::CaptureBusy,
                    "an update installation is starting",
                )
            })?;

        // One lifecycle owner serializes start/stop/pause/resume/cancel and
        // fatal notices. Native teardown remains on its own controller thread,
        // but no command can concurrently claim the same capture.
        let mut lifecycle = meeting.lifecycle.lock().await;
        if lifecycle.active_capture_id.is_some() {
            return Err(CaptureError::new(
                CaptureErrorCode::CaptureBusy,
                "another meeting capture is already active",
            ));
        }
        let mut controller_visibility = MeetingControllerVisibilityGuard::new(&app);
        // Inference must yield before the profile snapshot can acquire the
        // shared model gate. Hold this reservation until native teardown.
        let recording_priority = coordinator.prioritize_recording().await?;
        let profile_permit = Arc::clone(heavy_gate.inner())
            .acquire_owned()
            .await
            .map_err(|_| CaptureError::persistence())?;
        let profile = crate::captures::ProcessingProfileRecord::from(settings.snapshot()?);
        drop(profile_permit);
        let staging_root = app
            .path()
            .app_data_dir()
            .map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::PersistenceUnavailable,
                    "meeting staging root is unavailable",
                )
            })?
            .join("captures")
            .join("staging");
        std::fs::create_dir_all(&staging_root).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::InsufficientDiskSpace,
                "meeting staging volume is unavailable",
            )
        })?;
        let output_directory = crate::captures::configured_output_directory(&app)?;
        safeguards::DiskSafeguards::default().preflight(&staging_root, &output_directory)?;
        // Check durable-store health before reserving the process audio owner.
        // The reservation then closes the race with dictation before a row is
        // created, avoiding abandoned draft rows on `capture_busy`.
        let store_handle = Arc::clone(store.inner());
        if !store_handle.is_durable() {
            return Err(CaptureError::new(
                CaptureErrorCode::PersistenceUnavailable,
                "durable meeting-capture persistence is unavailable",
            ));
        }
        let lease = match Arc::clone(arbiter.inner()).reserve(AudioCaptureOwner::Meeting) {
            Ok(lease) => lease,
            Err(()) => {
                return Err(CaptureError::new(
                    CaptureErrorCode::CaptureBusy,
                    "another audio capture is active",
                ))
            }
        };
        // Capture creation produces the only ID used by native state, events,
        // manifests, and results.
        let store_for_create = Arc::clone(&store_handle);
        let title_for_create = title.clone();
        let root_for_create = staging_root.clone();
        let profile_for_create = profile.clone();
        let capture = tauri::async_runtime::spawn_blocking(move || {
            store_for_create.create_capture_with_processing_profile(
                &title_for_create,
                &root_for_create,
                &profile_for_create,
            )
        })
        .await
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::PersistenceUnavailable,
                "meeting capture creation task failed",
            )
        })??;
        let capture_id = capture.id;
        let store_for_state = Arc::clone(&store_handle);
        tauri::async_runtime::spawn_blocking(move || {
            store_for_state.transition_capture(
                capture_id,
                CaptureState::SelectingSource,
                None,
                None,
            )?;
            store_for_state.transition_capture(capture_id, CaptureState::Starting, None, None)
        })
        .await
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::PersistenceUnavailable,
                "meeting start transition task failed",
            )
        })??;

        let staging = capture.staging_path.clone().ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::PersistenceUnavailable,
                "meeting capture has no durable staging path",
            )
        })?;
        let checkpoint: Arc<dyn recorder::ChunkCheckpoint> =
            Arc::new(StoreCheckpoint(Arc::clone(&store_handle)));
        // Claim lifecycle ownership before native startup can produce a
        // callback or terminal notice. Native teardown is reconciled before
        // this owner is ever cleared.
        lifecycle.active_capture_id = Some(capture_id);
        lifecycle.update_activity = Some(update_activity);
        lifecycle.recording_priority = Some(recording_priority);
        // Directory creation, initial manifest fsync, source selection, and
        // native startup all occur after the SQLite methods returned.
        let disk_recorder = match recorder::DiskBackedRecorder::start(
            recorder::RecorderConfig::production(capture_id, capture.created_at, staging.clone()),
            checkpoint,
        ) {
            Ok(recorder) => recorder,
            Err(error) => {
                let _ =
                    persist_capture_failure(Arc::clone(&store_handle), capture_id, &error).await;
                lifecycle.clear_active();
                return Err(error);
            }
        };
        let (fatal_tx, fatal_rx) = std::sync::mpsc::channel();
        let controller = Arc::clone(&native.controller);
        let native_start_controller = Arc::clone(&controller);
        let preferred_output = output_device.preferred();
        let native_start = tauri::async_runtime::spawn_blocking(move || {
            native_start_controller.start_recorder(disk_recorder, lease, fatal_tx, preferred_output)
        })
        .await
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::SystemAudioStartFailed,
                "meeting native start task failed",
            )
        });
        if let Err(error) = native_start.and_then(|result| result.map(|_| ())) {
            let controller_for_terminalization: Arc<dyn RecorderController> = controller;
            let _ = terminalize_capture(
                Arc::clone(&store_handle),
                &mut lifecycle,
                controller_for_terminalization,
                capture_id,
                Some(error.clone()),
            )
            .await;
            emit_meeting_state(
                &app,
                capture_id,
                CaptureState::CaptureFailed,
                Some(error.code),
            );
            return Err(error);
        }
        let lifecycle_for_failure = Arc::clone(&meeting.lifecycle);
        let app_for_failure = app.clone();
        let store_for_failure = Arc::clone(&store_handle);
        let controller_for_failure: Arc<dyn RecorderController> = controller;
        tauri::async_runtime::spawn(async move {
            let received = tauri::async_runtime::spawn_blocking(move || fatal_rx.recv()).await;
            let Ok(Ok(notice)) = received else {
                return;
            };
            let mut lifecycle = lifecycle_for_failure.lock().await;
            if lifecycle.active_capture_id != Some(capture_id) {
                return;
            }
            let failure = match notice {
                macos::RecorderNotice::Failure(error) => Some(error),
                macos::RecorderNotice::AutomaticStop(result) => result.err(),
            };
            let update_activity = lifecycle.transcription_handoff();
            let outcome = terminalize_capture(
                Arc::clone(&store_for_failure),
                &mut lifecycle,
                Arc::clone(&controller_for_failure),
                capture_id,
                failure,
            )
            .await;
            let should_enqueue = outcome.is_ok();
            drop(lifecycle);
            if should_enqueue {
                enqueue_transcription(&app_for_failure, capture_id, update_activity);
            }
            let (status, error_code) = match outcome {
                Ok(_) => (CaptureState::Transcribing, None),
                Err(error) => (CaptureState::CaptureFailed, Some(error.code)),
            };
            emit_meeting_state(&app_for_failure, capture_id, status, error_code);
        });

        let store_for_recording = Arc::clone(&store_handle);
        let recording_result = tauri::async_runtime::spawn_blocking(move || {
            store_for_recording.transition_capture(capture_id, CaptureState::Recording, None, None)
        })
        .await
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::PersistenceUnavailable,
                "meeting recording transition task failed",
            )
        })?;
        let recording = match recording_result {
            Ok(recording) => recording,
            Err(error) => {
                let controller_for_terminalization: Arc<dyn RecorderController> =
                    native.controller.clone();
                let failure = terminalize_capture(
                    Arc::clone(&store_handle),
                    &mut lifecycle,
                    controller_for_terminalization,
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        emit_meeting_state(&app, capture_id, CaptureState::Recording, None);
        spawn_duration_reporter(
            app.clone(),
            Arc::clone(&store_handle),
            Arc::clone(&meeting.lifecycle),
            Arc::clone(&native.controller),
            capture_id,
            staging,
            output_directory,
        );
        controller_visibility.keep_visible();
        Ok(recording)
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
    {
        let _ = (
            app,
            store,
            arbiter,
            meeting,
            native,
            settings,
            heavy_gate,
            coordinator,
            output_device,
            update_gate,
            title,
        );
        Err(disabled_error())
    }
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
fn finish_automatic_stop(
    app: &tauri::AppHandle,
    store: &Arc<TranscriptStore>,
    lifecycle: &Arc<tokio::sync::Mutex<MeetingLifecycle>>,
    controller: &Arc<macos::Controller>,
    capture_id: i64,
    failure: Option<CaptureError>,
) {
    tauri::async_runtime::block_on(async {
        let mut lifecycle = lifecycle.lock().await;
        if lifecycle.active_capture_id != Some(capture_id) {
            return;
        }
        let controller: Arc<dyn RecorderController> = controller.clone();
        let update_activity = lifecycle.transcription_handoff();
        let outcome = terminalize_capture(
            Arc::clone(store),
            &mut lifecycle,
            controller,
            capture_id,
            failure,
        )
        .await;
        let should_enqueue = outcome.is_ok();
        drop(lifecycle);
        if should_enqueue {
            enqueue_transcription(app, capture_id, update_activity);
        }
        let (status, error_code) = match outcome {
            Ok(_) => (CaptureState::Transcribing, None),
            Err(error) => (CaptureState::CaptureFailed, Some(error.code)),
        };
        emit_meeting_state(app, capture_id, status, error_code);
    });
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
fn spawn_duration_reporter(
    app: tauri::AppHandle,
    store: Arc<TranscriptStore>,
    lifecycle: Arc<tokio::sync::Mutex<MeetingLifecycle>>,
    controller: Arc<macos::Controller>,
    capture_id: i64,
    staging: PathBuf,
    output: PathBuf,
) {
    std::thread::Builder::new()
        .name("murmur-meeting-duration".into())
        .spawn(move || {
            let mut warned_two_hours = false;
            let mut warned_three_hours = false;
            let mut disk_warning_emitted = false;
            let mut last_disk_available: Option<u64> = None;
            let mut ticks = 0_u64;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let active = tauri::async_runtime::block_on(async {
                    lifecycle.lock().await.active_capture_id
                });
                if active != Some(capture_id) {
                    break;
                }
                let Ok(status) = controller.status_recorder(capture_id) else {
                    break;
                };
                ticks = ticks.saturating_add(1);
                if ticks.is_multiple_of(60) {
                    let safeguards = safeguards::DiskSafeguards::default();
                    let staging_free = safeguards.available_bytes(&staging);
                    let output_free = safeguards.available_bytes(&output);
                    let available = match (staging_free, output_free) {
                        (Ok(staging_free), Ok(output_free)) => staging_free.min(output_free),
                        _ => {
                            crate::window_events::emit_meeting_ui(
                                &app,
                                "murmur://meeting-capture-warning",
                                MeetingWarningEvent {
                                    capture_id,
                                    warning: "disk_query_failed".into(),
                                    active_duration_ms: status.active_duration_ms,
                                },
                            );
                            finish_automatic_stop(
                                &app,
                                &store,
                                &lifecycle,
                                &controller,
                                capture_id,
                                Some(CaptureError::new(
                                    CaptureErrorCode::InsufficientDiskSpace,
                                    "available disk space could not be checked",
                                )),
                            );
                            break;
                        }
                    };
                    let measured_growth_per_second = last_disk_available
                        .map(|previous| previous.saturating_sub(available) / 60)
                        .unwrap_or(0);
                    last_disk_available = Some(available);
                    if safeguards::DiskSafeguards::should_warn(available) && !disk_warning_emitted {
                        disk_warning_emitted = true;
                        crate::window_events::emit_meeting_ui(
                            &app,
                            "murmur://meeting-capture-warning",
                            MeetingWarningEvent {
                                capture_id,
                                warning: "low_disk_space".into(),
                                active_duration_ms: status.active_duration_ms,
                            },
                        );
                    }
                    if safeguards::DiskSafeguards::should_emergency_stop(
                        available,
                        measured_growth_per_second,
                        64 * 1024 * 1024,
                    ) {
                        finish_automatic_stop(
                            &app,
                            &store,
                            &lifecycle,
                            &controller,
                            capture_id,
                            None,
                        );
                        break;
                    }
                }
                crate::window_events::emit_meeting_ui(
                    &app,
                    "murmur://meeting-capture-duration",
                    MeetingDurationEvent {
                        capture_id,
                        active_duration_ms: status.active_duration_ms,
                        paused_duration_ms: status.paused_duration_ms,
                    },
                );
                if !warned_two_hours && status.active_duration_ms >= clock::WARNING_TWO_HOURS_MS {
                    warned_two_hours = true;
                    crate::window_events::emit_meeting_ui(
                        &app,
                        "murmur://meeting-capture-warning",
                        MeetingWarningEvent {
                            capture_id,
                            warning: "two_hours".into(),
                            active_duration_ms: status.active_duration_ms,
                        },
                    );
                }
                if !warned_three_hours
                    && status.active_duration_ms >= clock::WARNING_THREE_HOURS_FORTY_FIVE_MS
                {
                    warned_three_hours = true;
                    crate::window_events::emit_meeting_ui(
                        &app,
                        "murmur://meeting-capture-warning",
                        MeetingWarningEvent {
                            capture_id,
                            warning: "three_hours_forty_five".into(),
                            active_duration_ms: status.active_duration_ms,
                        },
                    );
                }
            }
        })
        .ok();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum CancelResult {
    ConfirmationRequired { capture: CaptureRecord },
    Cancelled { capture_id: i64 },
    CleanupFailed { capture: CaptureRecord },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingDurationEvent {
    pub capture_id: i64,
    pub active_duration_ms: i64,
    pub paused_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingWarningEvent {
    pub capture_id: i64,
    pub warning: String,
    pub active_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingRecorderStatus {
    pub capture: Option<CaptureRecord>,
    pub active_duration_ms: i64,
    pub paused_duration_ms: i64,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MeetingCaptureCapabilities {
    pub feature_enabled: bool,
    pub supported: bool,
    pub minimum_macos_version: String,
    pub microphone: crate::permissions::PermissionState,
    pub persistence_durable: bool,
}

#[tauri::command]
pub fn meeting_capture_capabilities(
    store: tauri::State<'_, Arc<TranscriptStore>>,
) -> MeetingCaptureCapabilities {
    MeetingCaptureCapabilities {
        feature_enabled: cfg!(all(target_os = "macos", feature = "meeting-capture")),
        #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
        supported: macos::is_supported_macos(),
        #[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
        supported: false,
        // Core Audio process taps. See the plan's A6 for why this went *down*
        // from the 15.0 ScreenCaptureKit's captureMicrophone required.
        minimum_macos_version: "14.4".into(),
        microphone: crate::permissions::current().microphone,
        persistence_durable: store.is_durable(),
    }
}

#[tauri::command]
pub async fn meeting_recorder_pause(
    app: tauri::AppHandle,
    store: tauri::State<'_, Arc<TranscriptStore>>,
    meeting: tauri::State<'_, MeetingRecorderState>,
    native: tauri::State<'_, Mt0State>,
    capture_id: i64,
) -> Result<CaptureRecord, CaptureError> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    {
        let mut lifecycle = meeting.lifecycle.lock().await;
        if lifecycle.active_capture_id != Some(capture_id) {
            return Err(CaptureError::new(
                CaptureErrorCode::CaptureNotFound,
                "meeting recorder is not active",
            ));
        }
        let mut controller_visibility = MeetingControllerVisibilityGuard::new(&app);
        let current = match store.get_capture(capture_id) {
            Ok(current) => current,
            Err(error) => {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    native.controller.clone(),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        if current.status != CaptureState::Recording {
            controller_visibility.keep_visible();
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "meeting capture is not recording",
            ));
        }
        let controller: Arc<dyn RecorderController> = native.controller.clone();
        if let Err(error) = store.transition_capture(capture_id, CaptureState::Pausing, None, None)
        {
            let _ = terminalize_capture(
                Arc::clone(store.inner()),
                &mut lifecycle,
                Arc::clone(&controller),
                capture_id,
                Some(error.clone()),
            )
            .await;
            return Err(error);
        }
        let pause_controller = Arc::clone(&controller);
        let result = match tauri::async_runtime::spawn_blocking(move || {
            pause_controller.pause_recorder(capture_id)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let error = CaptureError::new(
                    CaptureErrorCode::CaptureInterrupted,
                    "meeting pause task failed",
                );
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    Arc::clone(&controller),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        if let Err(error) = result {
            let failure = terminalize_capture(
                Arc::clone(store.inner()),
                &mut lifecycle,
                Arc::clone(&controller),
                capture_id,
                Some(error.clone()),
            )
            .await
            .err()
            .unwrap_or(error);
            return Err(failure);
        }
        let status = match controller.status_recorder(capture_id) {
            Ok(status) => status,
            Err(error) => {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    Arc::clone(&controller),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        if let Err(error) = controller
            .checkpoint_paused(capture_id, status.paused_duration_ms)
            .and_then(|_| {
                store
                    .checkpoint_timing(
                        capture_id,
                        status.active_duration_ms,
                        status.paused_duration_ms,
                    )
                    .map(|_| ())
            })
        {
            let failure = terminalize_capture(
                Arc::clone(store.inner()),
                &mut lifecycle,
                Arc::clone(&controller),
                capture_id,
                Some(error.clone()),
            )
            .await
            .err()
            .unwrap_or(error);
            return Err(failure);
        }
        let paused = match store.transition_capture(capture_id, CaptureState::Paused, None, None) {
            Ok(paused) => paused,
            Err(error) => {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    controller,
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        emit_meeting_state(&app, capture_id, CaptureState::Paused, None);
        controller_visibility.keep_visible();
        Ok(paused)
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
    {
        let _ = (app, store, meeting, native, capture_id);
        Err(disabled_error())
    }
}

#[tauri::command]
pub async fn meeting_recorder_resume(
    app: tauri::AppHandle,
    store: tauri::State<'_, Arc<TranscriptStore>>,
    meeting: tauri::State<'_, MeetingRecorderState>,
    native: tauri::State<'_, Mt0State>,
    capture_id: i64,
) -> Result<CaptureRecord, CaptureError> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    {
        let mut lifecycle = meeting.lifecycle.lock().await;
        if lifecycle.active_capture_id != Some(capture_id) {
            return Err(CaptureError::new(
                CaptureErrorCode::CaptureNotFound,
                "meeting recorder is not active",
            ));
        }
        let mut controller_visibility = MeetingControllerVisibilityGuard::new(&app);
        let current = match store.get_capture(capture_id) {
            Ok(current) => current,
            Err(error) => {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    native.controller.clone(),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        if current.status != CaptureState::Paused {
            controller_visibility.keep_visible();
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidStateTransition,
                "meeting capture is not paused",
            ));
        }
        if let Some(staging) = current.staging_path.as_ref() {
            let output = match crate::captures::configured_output_directory(&app) {
                Ok(output) => output,
                Err(error) => {
                    let failure = terminalize_capture(
                        Arc::clone(store.inner()),
                        &mut lifecycle,
                        native.controller.clone(),
                        capture_id,
                        Some(error.clone()),
                    )
                    .await
                    .err()
                    .unwrap_or(error);
                    return Err(failure);
                }
            };
            if let Err(error) = safeguards::DiskSafeguards::default().preflight(staging, &output) {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    native.controller.clone(),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        }
        let controller: Arc<dyn RecorderController> = native.controller.clone();
        if let Err(error) = store.transition_capture(capture_id, CaptureState::Resuming, None, None)
        {
            let _ = terminalize_capture(
                Arc::clone(store.inner()),
                &mut lifecycle,
                Arc::clone(&controller),
                capture_id,
                Some(error.clone()),
            )
            .await;
            return Err(error);
        }
        let resume_controller = Arc::clone(&controller);
        let result = match tauri::async_runtime::spawn_blocking(move || {
            resume_controller.resume_recorder(capture_id)
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let error = CaptureError::new(
                    CaptureErrorCode::CaptureInterrupted,
                    "meeting resume task failed",
                );
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    Arc::clone(&controller),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        if let Err(error) = result {
            let failure = terminalize_capture(
                Arc::clone(store.inner()),
                &mut lifecycle,
                Arc::clone(&controller),
                capture_id,
                Some(error.clone()),
            )
            .await
            .err()
            .unwrap_or(error);
            return Err(failure);
        }
        let status = match controller.status_recorder(capture_id) {
            Ok(status) => status,
            Err(error) => {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    Arc::clone(&controller),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        if let Err(error) = controller
            .checkpoint_paused(capture_id, status.paused_duration_ms)
            .and_then(|_| {
                store
                    .checkpoint_timing(
                        capture_id,
                        status.active_duration_ms,
                        status.paused_duration_ms,
                    )
                    .map(|_| ())
            })
        {
            let failure = terminalize_capture(
                Arc::clone(store.inner()),
                &mut lifecycle,
                Arc::clone(&controller),
                capture_id,
                Some(error.clone()),
            )
            .await
            .err()
            .unwrap_or(error);
            return Err(failure);
        }
        let recording =
            match store.transition_capture(capture_id, CaptureState::Recording, None, None) {
                Ok(recording) => recording,
                Err(error) => {
                    let failure = terminalize_capture(
                        Arc::clone(store.inner()),
                        &mut lifecycle,
                        controller,
                        capture_id,
                        Some(error.clone()),
                    )
                    .await
                    .err()
                    .unwrap_or(error);
                    return Err(failure);
                }
            };
        emit_meeting_state(&app, capture_id, CaptureState::Recording, None);
        controller_visibility.keep_visible();
        Ok(recording)
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
    {
        let _ = (app, store, meeting, native, capture_id);
        Err(disabled_error())
    }
}

#[tauri::command]
pub async fn meeting_recorder_status(
    store: tauri::State<'_, Arc<TranscriptStore>>,
    meeting: tauri::State<'_, MeetingRecorderState>,
    native: tauri::State<'_, Mt0State>,
) -> Result<MeetingRecorderStatus, CaptureError> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    {
        let lifecycle = meeting.lifecycle.lock().await;
        let Some(capture_id) = lifecycle.active_capture_id else {
            return Ok(MeetingRecorderStatus {
                capture: None,
                active_duration_ms: 0,
                paused_duration_ms: 0,
                paused: false,
            });
        };
        let capture = store.get_capture(capture_id)?;
        let status = native
            .controller
            .status_recorder(capture_id)
            .unwrap_or(RecorderStatus {
                active_duration_ms: capture.active_duration_ms,
                paused_duration_ms: capture.paused_duration_ms,
                paused: capture.status == CaptureState::Paused,
            });
        Ok(MeetingRecorderStatus {
            capture: Some(capture),
            active_duration_ms: status.active_duration_ms,
            paused_duration_ms: status.paused_duration_ms,
            paused: status.paused,
        })
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
    {
        let _ = (store, meeting, native);
        Err(disabled_error())
    }
}

#[tauri::command]
pub async fn meeting_recorder_cancel(
    app: tauri::AppHandle,
    store: tauri::State<'_, Arc<TranscriptStore>>,
    meeting: tauri::State<'_, MeetingRecorderState>,
    native: tauri::State<'_, Mt0State>,
    capture_id: i64,
    confirmed: bool,
) -> Result<CancelResult, CaptureError> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    {
        let mut lifecycle = meeting.lifecycle.lock().await;
        if lifecycle.active_capture_id != Some(capture_id) {
            return Err(CaptureError::new(
                CaptureErrorCode::CaptureNotFound,
                "meeting recorder is not active",
            ));
        }
        let mut controller_visibility = MeetingControllerVisibilityGuard::new(&app);
        let current = match store.get_capture(capture_id) {
            Ok(current) => current,
            Err(error) => {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    native.controller.clone(),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
        };
        let controller: Arc<dyn RecorderController> = native.controller.clone();
        let runtime_duration = if current.status == CaptureState::Deleting {
            0
        } else {
            match controller.status_recorder(capture_id) {
                Ok(status) => status.active_duration_ms,
                Err(error) => {
                    let failure = terminalize_capture(
                        Arc::clone(store.inner()),
                        &mut lifecycle,
                        Arc::clone(&controller),
                        capture_id,
                        Some(error.clone()),
                    )
                    .await
                    .err()
                    .unwrap_or(error);
                    return Err(failure);
                }
            }
        };
        let needs_confirmation = current.active_duration_ms.max(runtime_duration) >= 10_000
            || current.completed_chunk_count > 0;
        if needs_confirmation && !confirmed {
            controller_visibility.keep_visible();
            return Ok(CancelResult::ConfirmationRequired { capture: current });
        }
        if current.status != CaptureState::Deleting {
            let timing = match controller.status_recorder(capture_id) {
                Ok(timing) => timing,
                Err(error) => {
                    let failure = terminalize_capture(
                        Arc::clone(store.inner()),
                        &mut lifecycle,
                        Arc::clone(&controller),
                        capture_id,
                        Some(error.clone()),
                    )
                    .await
                    .err()
                    .unwrap_or(error);
                    return Err(failure);
                }
            };
            if let Err(error) = controller
                .checkpoint_paused(capture_id, timing.paused_duration_ms)
                .and_then(|_| {
                    store
                        .checkpoint_timing(
                            capture_id,
                            timing.active_duration_ms,
                            timing.paused_duration_ms,
                        )
                        .map(|_| ())
                })
            {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    Arc::clone(&controller),
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
            let cancel_controller = Arc::clone(&controller);
            let stopped = match tauri::async_runtime::spawn_blocking(move || {
                cancel_controller.cancel_recorder(capture_id)
            })
            .await
            {
                Ok(stopped) => stopped,
                Err(_) => {
                    let error = CaptureError::new(
                        CaptureErrorCode::CaptureInterrupted,
                        "meeting cancel teardown task failed",
                    );
                    let failure = terminalize_capture(
                        Arc::clone(store.inner()),
                        &mut lifecycle,
                        Arc::clone(&controller),
                        capture_id,
                        Some(error.clone()),
                    )
                    .await
                    .err()
                    .unwrap_or(error);
                    return Err(failure);
                }
            };
            if let Err(error) = stopped {
                let failure = terminalize_capture(
                    Arc::clone(store.inner()),
                    &mut lifecycle,
                    controller,
                    capture_id,
                    Some(error.clone()),
                )
                .await
                .err()
                .unwrap_or(error);
                return Err(failure);
            }
            if let Err(error) =
                store.transition_capture(capture_id, CaptureState::Deleting, None, None)
            {
                // Native cancellation already drained writers. Reconcile the row
                // as a recoverable failure before releasing lifecycle ownership.
                let failure =
                    persist_capture_failure(Arc::clone(store.inner()), capture_id, &error).await;
                lifecycle.clear_active();
                return Err(failure.err().unwrap_or(error));
            }
        }
        let deleting = match store.get_capture(capture_id) {
            Ok(deleting) => deleting,
            Err(error) => {
                // Keep runtime ownership until the deleting row can be
                // reconciled; cleanup retry must not be stranded as `deleting`.
                return Err(error);
            }
        };
        if let Err(error) = crate::captures::remove_validated_staging(&app, &deleting) {
            let failed = match store.transition_capture(
                capture_id,
                CaptureState::CleanupFailed,
                Some(CaptureErrorCode::CleanupFailed),
                Some(error.message.clone()),
            ) {
                Ok(failed) => failed,
                Err(persistence_error) => {
                    lifecycle.clear_active();
                    return Err(persistence_error);
                }
            };
            lifecycle.clear_active();
            emit_meeting_state(
                &app,
                capture_id,
                CaptureState::CleanupFailed,
                Some(CaptureErrorCode::CleanupFailed),
            );
            return Ok(CancelResult::CleanupFailed { capture: failed });
        }
        if let Err(error) =
            store.transition_capture(capture_id, CaptureState::Cancelled, None, None)
        {
            match store.transition_capture(
                capture_id,
                CaptureState::CleanupFailed,
                Some(CaptureErrorCode::CleanupFailed),
                Some(error.message.clone()),
            ) {
                Ok(_) => {
                    lifecycle.clear_active();
                }
                Err(_) => {
                    // Retain the lifecycle owner if even reconciliation is
                    // unavailable. A later command/restart can recover it.
                    return Err(error);
                }
            }
            return Err(error);
        }
        if let Err(error) = store.delete_capture_metadata(capture_id) {
            lifecycle.clear_active();
            return Err(error);
        }
        lifecycle.clear_active();
        emit_meeting_state(&app, capture_id, CaptureState::Cancelled, None);
        Ok(CancelResult::Cancelled { capture_id })
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
    {
        let _ = (app, store, meeting, native, capture_id, confirmed);
        Err(disabled_error())
    }
}

#[tauri::command]
pub async fn meeting_recorder_stop(
    app: tauri::AppHandle,
    store: tauri::State<'_, Arc<TranscriptStore>>,
    meeting: tauri::State<'_, MeetingRecorderState>,
    native: tauri::State<'_, Mt0State>,
    capture_id: i64,
) -> Result<CaptureRecord, CaptureError> {
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    {
        let mut lifecycle = meeting.lifecycle.lock().await;
        match lifecycle.active_capture_id {
            Some(active_id) if active_id == capture_id => {}
            Some(_) | None => {
                return Err(CaptureError::new(
                    CaptureErrorCode::CaptureNotFound,
                    "meeting recorder is not active",
                ))
            }
        }
        let failure = store.get_capture(capture_id).ok().and_then(|capture| {
            (capture.status == CaptureState::CaptureFailed).then(|| {
                CaptureError::new(
                    CaptureErrorCode::CaptureInterrupted,
                    "meeting capture already failed",
                )
            })
        });
        let controller: Arc<dyn RecorderController> = native.controller.clone();
        let update_activity = lifecycle.transcription_handoff();
        let outcome = terminalize_capture(
            Arc::clone(store.inner()),
            &mut lifecycle,
            controller,
            capture_id,
            failure,
        )
        .await;
        let should_enqueue = outcome.is_ok();
        drop(lifecycle);
        if should_enqueue {
            enqueue_transcription(&app, capture_id, update_activity);
        }
        let (status, error_code) = match &outcome {
            Ok(_) => (CaptureState::Transcribing, None),
            Err(error) => (CaptureState::CaptureFailed, Some(error.code)),
        };
        emit_meeting_state(&app, capture_id, status, error_code);
        outcome
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
    {
        let _ = (app, store, meeting, native, capture_id);
        Err(disabled_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FakeController {
        stop_calls: AtomicUsize,
        stop_result: StdMutex<Result<RecorderStopResult, CaptureError>>,
    }

    impl FakeController {
        fn successful(capture_id: i64) -> Arc<Self> {
            Arc::new(Self {
                stop_calls: AtomicUsize::new(0),
                stop_result: StdMutex::new(Ok(RecorderStopResult {
                    capture_id,
                    active_duration_ms: 1_000,
                    paused_duration_ms: 250,
                    completed_chunk_pairs: 1,
                    staging_directory: PathBuf::from("/tmp/meeting-recovery"),
                })),
            })
        }

        fn failing(error: CaptureError) -> Arc<Self> {
            Arc::new(Self {
                stop_calls: AtomicUsize::new(0),
                stop_result: StdMutex::new(Err(error)),
            })
        }
    }

    impl RecorderController for FakeController {
        fn pause_recorder(&self, _capture_id: i64) -> Result<(), CaptureError> {
            Ok(())
        }

        fn resume_recorder(&self, _capture_id: i64) -> Result<(), CaptureError> {
            Ok(())
        }

        fn status_recorder(&self, _capture_id: i64) -> Result<RecorderStatus, CaptureError> {
            Ok(RecorderStatus {
                active_duration_ms: 1_000,
                paused_duration_ms: 250,
                paused: false,
            })
        }

        fn checkpoint_paused(
            &self,
            _capture_id: i64,
            _paused_duration_ms: i64,
        ) -> Result<(), CaptureError> {
            Ok(())
        }

        fn stop_recorder(&self, _capture_id: i64) -> Result<RecorderStopResult, CaptureError> {
            self.stop_calls.fetch_add(1, Ordering::SeqCst);
            self.stop_result.lock().unwrap().clone()
        }

        fn cancel_recorder(&self, capture_id: i64) -> Result<RecorderStopResult, CaptureError> {
            self.stop_recorder(capture_id)
        }
    }

    fn durable_capture(status: CaptureState) -> (Arc<TranscriptStore>, i64) {
        static TEST_NONCE: AtomicU64 = AtomicU64::new(0);
        let wall_nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let thread_nonce = TEST_NONCE.fetch_add(1, Ordering::Relaxed);
        let nonce = format!("{wall_nonce}-{thread_nonce}");
        let root = std::env::temp_dir().join(format!("murmur-meeting-command-{nonce}"));
        std::fs::create_dir(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let db = root.join("murmur.db");
        let store = Arc::new(TranscriptStore::open(&db).unwrap());
        let staging_root = root.join("staging");
        let capture = store
            .create_capture("command regression", &staging_root)
            .unwrap();
        store
            .transition_capture(capture.id, CaptureState::SelectingSource, None, None)
            .unwrap();
        store
            .transition_capture(capture.id, CaptureState::Starting, None, None)
            .unwrap();
        if status == CaptureState::Recording {
            store
                .transition_capture(capture.id, CaptureState::Recording, None, None)
                .unwrap();
        }
        (store, capture.id)
    }

    #[test]
    fn terminalization_stops_native_before_clearing_owner_and_hands_off() {
        let (store, capture_id) = durable_capture(CaptureState::Recording);
        let controller = FakeController::successful(capture_id);
        let update_gate = Arc::new(crate::update_install::UpdateInstallGate::default());
        let update_activity = update_gate.begin_activity().expect("meeting admitted");
        let mut lifecycle = MeetingLifecycle {
            active_capture_id: Some(capture_id),
            update_activity: Some(update_activity),
            ..Default::default()
        };
        let transcription_handoff = lifecycle
            .transcription_handoff()
            .expect("recording activity can transfer to transcription");
        let result = tauri::async_runtime::block_on(terminalize_capture(
            Arc::clone(&store),
            &mut lifecycle,
            controller.clone(),
            capture_id,
            None,
        ))
        .unwrap();
        assert_eq!(result.status, CaptureState::Transcribing);
        assert_eq!(controller.stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.active_capture_id, None);
        assert!(!update_gate.try_claim_install());
        drop(transcription_handoff);
        assert!(update_gate.try_claim_install());
    }

    #[test]
    fn disk_failure_is_persisted_only_after_native_teardown() {
        let (store, capture_id) = durable_capture(CaptureState::Recording);
        let controller = FakeController::failing(CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "injected writer failure",
        ));
        let mut lifecycle = MeetingLifecycle {
            active_capture_id: Some(capture_id),
            update_activity: None,
            ..Default::default()
        };
        let error = tauri::async_runtime::block_on(terminalize_capture(
            Arc::clone(&store),
            &mut lifecycle,
            controller.clone(),
            capture_id,
            None,
        ))
        .unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::ChunkWriteFailed);
        assert_eq!(controller.stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.get_capture(capture_id).unwrap().status,
            CaptureState::CaptureFailed
        );
        assert_eq!(lifecycle.active_capture_id, None);
    }

    #[test]
    fn handoff_persistence_failure_reconciles_to_recoverable_failure() {
        let (store, capture_id) = durable_capture(CaptureState::Starting);
        let controller = FakeController::successful(capture_id);
        let mut lifecycle = MeetingLifecycle {
            active_capture_id: Some(capture_id),
            update_activity: None,
            ..Default::default()
        };
        let error = tauri::async_runtime::block_on(terminalize_capture(
            Arc::clone(&store),
            &mut lifecycle,
            controller.clone(),
            capture_id,
            None,
        ))
        .unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::InvalidStateTransition);
        assert_eq!(controller.stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.get_capture(capture_id).unwrap().status,
            CaptureState::CaptureFailed
        );
        assert_eq!(lifecycle.active_capture_id, None);
    }

    #[test]
    fn explicit_failure_is_terminalized_once_like_an_automatic_notice() {
        let (store, capture_id) = durable_capture(CaptureState::Recording);
        let controller = FakeController::successful(capture_id);
        let mut lifecycle = MeetingLifecycle {
            active_capture_id: Some(capture_id),
            update_activity: None,
            ..Default::default()
        };
        let error = CaptureError::new(CaptureErrorCode::InsufficientDiskSpace, "disk guard");
        let result = tauri::async_runtime::block_on(terminalize_capture(
            Arc::clone(&store),
            &mut lifecycle,
            controller.clone(),
            capture_id,
            Some(error.clone()),
        ))
        .unwrap_err();
        assert_eq!(result.code, error.code);
        assert_eq!(controller.stop_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.get_capture(capture_id).unwrap().status,
            CaptureState::CaptureFailed
        );
        assert_eq!(lifecycle.active_capture_id, None);
    }

    #[test]
    fn registration_allowlist_is_audio_only() {
        assert_eq!(REGISTERED_OUTPUTS, ["system_audio", "microphone"]);
        assert!(!REGISTERED_OUTPUTS
            .iter()
            .any(|kind| kind.contains("screen") || kind.contains("video")));
    }

    #[test]
    fn typed_error_serializes_with_stable_code() {
        let error = Mt0Error::new(Mt0ErrorCode::WriterQueueOverflow, "bounded queue full");
        let json = serde_json::to_value(error).unwrap();
        assert_eq!(json["code"], "writer_queue_overflow");
    }

    #[test]
    fn meeting_state_event_carries_real_positive_capture_id() {
        let event = MeetingRecorderEvent {
            capture_id: 42,
            status: "recording".into(),
            error_code: None,
        };
        let json = serde_json::to_value(event).unwrap();
        assert_eq!(json["capture_id"], 42);
        assert!(json["capture_id"].as_i64().unwrap() > 0);
        assert!(json["error_code"].is_null());

        let failed = MeetingRecorderEvent {
            capture_id: 42,
            status: "capture_failed".into(),
            error_code: Some(CaptureErrorCode::WriterQueueOverflow),
        };
        let failed_json = serde_json::to_value(failed).unwrap();
        assert_eq!(failed_json["capture_id"], 42);
        assert_eq!(failed_json["error_code"], "writer_queue_overflow");
    }

    #[test]
    fn cancel_result_keeps_confirmation_and_terminal_outcomes_distinct() {
        let (store, capture_id) = durable_capture(CaptureState::Recording);
        let capture = store.get_capture(capture_id).unwrap();
        let confirmation =
            serde_json::to_value(CancelResult::ConfirmationRequired { capture }).unwrap();
        assert_eq!(confirmation["status"], "confirmation_required");
        assert_eq!(confirmation["capture"]["id"], capture_id);

        let cancelled = serde_json::to_value(CancelResult::Cancelled { capture_id }).unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(cancelled["capture_id"], capture_id);
    }

    #[test]
    fn source_end_is_a_typed_recoverable_capture_error() {
        let error = CaptureError::new(CaptureErrorCode::SourceEnded, "source ended");
        let json = serde_json::to_value(error).unwrap();
        assert_eq!(json["code"], "source_ended");
    }
}
