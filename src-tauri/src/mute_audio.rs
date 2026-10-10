//! Mute system audio during dictation — same posture as Willow Voice's
//! "Mute Audio While Dictating" toggle.
//!
//! Saves the current macOS output volume on recording start, drops it
//! to 0, and restores on any exit path (successful paste, Esc,
//! empty-capture, Whisper error, paste error). Covers every audio
//! source including games, Discord notifications, and browser media
//! — it's the master output slider, not per-app.
//!
//! **Chimes keep ringing** because macOS routes `NSSound::soundNamed`
//! through the separate *Alert volume* channel (System Settings →
//! Sound → Sound Effects → Alert volume), which isn't touched by
//! `output volume` / the menu-bar slider. Verified live; if that ever
//! changes in a future macOS release, the fallback is wrapping each
//! chime with a save-unmute-play-remute cycle at the `sound::play_*`
//! call sites.
//!
//! Volume IO goes through `osascript` for v0.1 speed-to-ship — one
//! subprocess per transition, ~100 ms each, imperceptible against
//! the full dictation flow. Migrating to Core Audio HAL (instant, no
//! subprocess) is a post-launch follow-up if latency becomes an issue.
//!
//! **Crash-safe restore:** a save writes the prior volume to an
//! on-disk sentinel (`<app_data>/murmur-muted-volume`). Restore
//! deletes the sentinel. If the app dies mid-dictation (process
//! killed, force-quit, crash), `recover_on_startup` called in
//! `lib.rs::setup` finds the sentinel at launch and restores the
//! saved volume — otherwise the user would find their system stuck
//! muted.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{AppHandle, Manager};

/// Filename of the on-disk sentinel we write before muting + delete
/// after restoring. If present at app launch, it means a prior
/// session muted the system output and didn't get the chance to
/// restore — `recover_on_startup` handles that case.
const SENTINEL_FILENAME: &str = "murmur-muted-volume";

pub struct MuteWhileDictatingState {
    enabled: AtomicBool,
    /// The volume level we saved right before muting. `None` between
    /// dictations or when the toggle is off. Wrapped in `Mutex<Option>`
    /// because the lifecycle is mute → save → restore → clear; this
    /// keeps the state in exactly one place without having to thread
    /// it through `transcribe_and_emit`'s argument list.
    saved_volume: Mutex<Option<i32>>,
}

impl MuteWhileDictatingState {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            saved_volume: Mutex::new(None),
        }
    }

    pub fn set(&self, value: bool) {
        self.enabled.store(value, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Called on hotkey press. If the toggle is off, no-op. Otherwise
    /// save the current output volume (to memory + disk sentinel) and
    /// drop output to 0.
    pub fn mute_if_enabled(&self, app: &AppHandle) {
        if !self.is_enabled() {
            return;
        }
        let current = read_output_volume();
        *self.saved_volume.lock() = Some(current);
        write_sentinel(app, current);
        set_output_volume(0);
    }

    /// Called on every dictation-end path: successful paste,
    /// cancelled, empty capture, Whisper error, paste error. No-op
    /// when there's nothing saved (toggle was off, or already
    /// restored). Clears both the in-memory saved value and the
    /// on-disk sentinel.
    pub fn restore(&self, app: &AppHandle) {
        let saved = self.saved_volume.lock().take();
        if let Some(v) = saved {
            set_output_volume(v);
        }
        delete_sentinel(app);
    }
}

impl Default for MuteWhileDictatingState {
    fn default() -> Self {
        Self::new()
    }
}

/// Mirror the frontend toggle into Rust. Called from
/// `useMuteWhileDictating` on mount + every change.
#[tauri::command]
pub fn set_mute_while_dictating(
    state: tauri::State<'_, Arc<MuteWhileDictatingState>>,
    enabled: bool,
) {
    state.set(enabled);
    crate::murmur_dev_log!("[murmur] mute while dictating: {enabled}");
}

/// Called once from `lib.rs::setup` at app launch. If a prior session
/// muted the system output and didn't get to restore (crash,
/// force-quit mid-dictation), the sentinel file will still exist —
/// read it, restore the volume, delete the sentinel. Otherwise this
/// is a no-op.
pub fn recover_on_startup(app: &AppHandle) {
    let Some(path) = sentinel_path(app) else {
        return;
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(volume) = contents.trim().parse::<i32>() else {
        crate::murmur_dev_log!("[murmur] mute-audio sentinel couldn't be parsed; deleting");
        let _ = std::fs::remove_file(&path);
        return;
    };
    crate::murmur_dev_log!(
        "[murmur] mute-audio: recovering output volume to {volume} from stale sentinel (prior session did not restore)"
    );
    set_output_volume(volume);
    let _ = std::fs::remove_file(&path);
}

fn sentinel_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|d| d.join(SENTINEL_FILENAME))
}

fn write_sentinel(app: &AppHandle, volume: i32) {
    let Some(path) = sentinel_path(app) else {
        return;
    };
    // Best-effort: if the app-data dir doesn't exist or we can't
    // write, skip the sentinel. The in-memory saved volume still
    // lets a normal-exit restore succeed; the sentinel only matters
    // for the crash-recovery case, which is rare enough that a
    // silently-missing one isn't worth escalating.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, volume.to_string());
}

fn delete_sentinel(app: &AppHandle) {
    if let Some(path) = sentinel_path(app) {
        let _ = std::fs::remove_file(&path);
    }
}

/// Read the current macOS output volume (0–100) via `osascript`. On
/// any error (subprocess failure, unexpected output, parse error)
/// returns a safe default of 50, so a failed read can't wedge the
/// app with a stuck-muted volume.
fn read_output_volume() -> i32 {
    let output = Command::new("osascript")
        .arg("-e")
        .arg("output volume of (get volume settings)")
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<i32>()
            .unwrap_or(50),
        _ => {
            crate::murmur_dev_log!(
                "[murmur] mute-audio: failed to read output volume; defaulting to 50"
            );
            50
        }
    }
}

fn set_output_volume(volume: i32) {
    let clamped = volume.clamp(0, 100);
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(format!("set volume output volume {clamped}"))
        .output();
}
