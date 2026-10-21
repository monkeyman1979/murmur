//! Audio cues — short chimes at pipeline transitions so the user has
//! a non-visual signal for what happened.
//!
//! Three cue slots wired in `lib.rs::wire_hotkey` + `transcribe_and_emit`:
//!
//! - **Start** (`Pop`) — capture has begun delivering microphone frames.
//!   Keep the original sound while testing whether cue timing fixes
//!   Bluetooth playback; selecting a different cue is a separate experiment.
//! - **Stop** (`Bottle`) — transcription and corrections completed;
//!   insertion is about to begin. This does not claim paste acceptance.
//! - **Cancel** (`Basso`) — capture/transcription or insertion failed.
//!   Fires on Esc, empty capture, empty transcript, Whisper error, and
//!   paste error, including after Bottle. macOS's `Basso` is its built-in
//!   "error" sound, so the low-thud association is already learned.
//!
//! All three are macOS system sounds from `/System/Library/Sounds/`,
//! picked for low collision risk with common notification sounds
//! (`Glass` used by Mail, `Funk`/`Sosumi` for alerts, `Ping`/`Tink` as
//! default alert options). Zero bundle cost — no asset pipeline.
//!
//! Playback goes through `NSSound::soundNamed(name).play()`. We let the
//! returned `Retained<NSSound>` drop after `play()`; Cocoa keeps enough
//! internal state for short system sounds to finish, and avoiding an
//! intentional per-cue leak matters for a menu-bar app that can stay open
//! for days.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::audio::CaptureReadiness;

/// Allow the output route a short settling window after the first microphone
/// callback. Never hold up capture or the hotkey worker to play a cue.
const START_CUE_SETTLE: Duration = Duration::from_millis(150);
const START_CUE_MAX_WAIT: Duration = Duration::from_secs(2);
const START_CUE_POLL: Duration = Duration::from_millis(20);

/// macOS built-in sound name for the "dictation started" cue.
const SOUND_START: &str = "Pop";
/// macOS built-in sound name for the transcription-complete cue.
const SOUND_STOP: &str = "Bottle";
/// macOS built-in sound name for the "dictation didn't reach the app" cue.
const SOUND_CANCEL: &str = "Basso";

/// Reactive mirror of the user's Settings → Sound cues
/// toggle. Frontend (`useSoundCues`) is the source of truth; this
/// atomic is what the hotkey pipeline reads without paying an IPC
/// round-trip on every press.
pub struct SoundCuesState {
    enabled: AtomicBool,
}

impl SoundCuesState {
    /// Defaults to `false`; the frontend composable flips it to the
    /// persisted `localStorage` value on mount (immediate watcher), so
    /// the atomic reflects the user's last setting before the first
    /// hotkey press.
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
        }
    }

    pub fn set(&self, value: bool) {
        self.enabled.store(value, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

impl Default for SoundCuesState {
    fn default() -> Self {
        Self::new()
    }
}

/// Mirror the frontend toggle into Rust. Called from the
/// `useSoundCues` composable on mount + every change.
#[tauri::command]
pub fn set_sound_cues(state: tauri::State<'_, Arc<SoundCuesState>>, enabled: bool) {
    state.set(enabled);
    crate::murmur_dev_log!("[murmur] sound cues: {enabled}");
}

/// Play start only after audio has actually arrived and the route has had a
/// brief chance to settle. This runs independently of the capture thread; a
/// quick release, cancellation, stream error, or silent/dead route suppresses
/// a misleading *late* start cue. Capture itself is never delayed.
pub async fn play_start_when_ready(state: Arc<SoundCuesState>, ready: CaptureReadiness) {
    if wait_for_start_cue(&state, &ready).await {
        // Playback is asynchronous; capture may end immediately after the
        // readiness check, but the cue was scheduled while it was active.
        play_system_sound(SOUND_START);
    }
}

async fn wait_for_start_cue(state: &SoundCuesState, ready: &CaptureReadiness) -> bool {
    let deadline = tokio::time::Instant::now() + START_CUE_MAX_WAIT;
    loop {
        if !ready.is_active() || !state.is_enabled() {
            return false;
        }
        if ready
            .since_first_callback()
            .is_some_and(|elapsed| elapsed >= START_CUE_SETTLE)
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            crate::murmur_dev_log!("[murmur] start cue skipped: no ready microphone frames");
            return false;
        }
        tokio::time::sleep(START_CUE_POLL).await;
    }
}

/// Play the transcription-complete cue before insertion begins. No-op when
/// the toggle is OFF. A later insertion failure may also play `play_cancel`,
/// so both completion and failure cues can sound for one dictation.
pub fn play_stop(state: &SoundCuesState) {
    if state.is_enabled() {
        play_system_sound(SOUND_STOP);
    }
}

/// Play the "something didn't go through" cue. No-op when the toggle
/// is OFF. Covers Esc/cancel, empty capture, empty transcript, Whisper
/// error, and paste error (non-Accessibility-denied — that path has
/// its own in-app surfaces and a chime would be double-signaling).
pub fn play_cancel(state: &SoundCuesState) {
    if state.is_enabled() {
        play_system_sound(SOUND_CANCEL);
    }
}

/// Fire a one-shot macOS system sound by name. Log-and-swallow on
/// lookup failure — a missing sound must never block or crash the
/// dictation pipeline. Mac-only (the project is Apple-Silicon-only
/// for v0.1 per SPEC §3; the `#[cfg(not(target_os = "macos"))]`
/// branch exists so `cargo check` on a hypothetical Linux port
/// doesn't fail on the call site).
fn play_system_sound(name: &str) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSSound;
        use objc2_foundation::NSString;

        objc2::rc::autoreleasepool(|_| {
            let ns_name = NSString::from_str(name);
            // `NSSound::soundNamed` + `.play()` are safe bindings in the
            // objc2-app-kit 0.3 generated API (no main-thread-only attrs,
            // no other preconditions). Older code `mem::forget`ed the
            // Retained<NSSound> to keep playback alive, but that leaked one
            // object per cue for the lifetime of the process. Let it drop;
            // AppKit's named-sound cache owns the actual system sound.
            match NSSound::soundNamed(&ns_name) {
                Some(sound) => {
                    if !sound.play() {
                        crate::murmur_dev_log!("[murmur] system sound playback failed: {name}");
                    }
                }
                None => crate::murmur_dev_log!("[murmur] system sound not found: {name}"),
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Keep the parameter live under the cfg-gate so Linux/Windows
        // cross-compiles don't flag it as unused. Non-macOS targets
        // silently skip — audio cues are macOS-only for v0.1.
        let _ = name;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_waits_for_frames_and_settling_without_blocking_capture() {
        let state = SoundCuesState::new();
        state.set(true);
        let ready = CaptureReadiness::new();
        ready.note_callback(0, 0.0);
        assert!(ready.since_first_callback().is_none());
        let callback = ready.clone();
        let producer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            callback.note_callback(128, 0.0);
        });
        let start = std::time::Instant::now();
        assert!(wait_for_start_cue(&state, &ready).await);
        assert!(start.elapsed() >= START_CUE_SETTLE);
        producer.await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_capture_never_plays_late_start_cue() {
        let state = SoundCuesState::new();
        state.set(true);
        let ready = CaptureReadiness::new();
        ready.close();
        assert!(!wait_for_start_cue(&state, &ready).await);
    }

    #[tokio::test]
    async fn disabled_cues_never_wait_or_play() {
        let state = SoundCuesState::new();
        let ready = CaptureReadiness::new();
        ready.note_callback(128, 0.0);
        assert!(!wait_for_start_cue(&state, &ready).await);
    }
}
