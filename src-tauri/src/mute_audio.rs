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
//!
//! **Bluetooth headsets** take a separate path (see `bluetooth_route.rs`):
//! opening the headset mic switches its playback into call mode, which is
//! audible as a gap plus muffled audio at both ends of a dictation. When the
//! output is a Bluetooth headset, Murmur turns on the system output *mute*
//! before the mic opens and turns it off only after the headset is back in
//! music mode. With the toggle off, output is unmuted once the mic is live,
//! so only the transitions are hidden. Mute is used instead of volume 0 so
//! no volume level (including the headset's call-mode level) is rewritten.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{AppHandle, Manager};

use crate::bluetooth_route::BluetoothOutput;

/// Filename of the on-disk sentinel we write before muting + delete
/// after restoring. If present at app launch, it means a prior
/// session muted the system output and didn't get the chance to
/// restore — `recover_on_startup` handles that case.
const SENTINEL_FILENAME: &str = "murmur-muted-volume";
/// Present while Murmur holds the system output mute on for a Bluetooth
/// dictation. Recovery turns mute back off.
const BLUETOOTH_SENTINEL_FILENAME: &str = "murmur-muted-bluetooth";

pub struct MuteWhileDictatingState {
    enabled: AtomicBool,
    /// The volume level we saved right before muting. `None` between
    /// dictations or when the toggle is off. Wrapped in `Mutex<Option>`
    /// because the lifecycle is mute → save → restore → clear; this
    /// keeps the state in exactly one place without having to thread
    /// it through `transcribe_and_emit`'s argument list.
    saved_volume: Mutex<Option<i32>>,
    bluetooth: Mutex<BluetoothState>,
}

#[derive(Default)]
struct BluetoothState {
    /// Bumped on every dictation start so a stale post-release watch never
    /// unmutes a newer dictation.
    generation: u64,
    session: Option<BluetoothSession>,
}

struct BluetoothSession {
    output: BluetoothOutput,
    /// Output rate once the mic delivered frames — the call-mode rate on
    /// devices that report one.
    call_rate: Option<f64>,
    /// Murmur turned system mute on and owes the unmute. False when the user
    /// had already muted, so their choice is never undone.
    muted_by_us: bool,
    /// The post-release watch owns the unmute.
    returning: bool,
}

impl MuteWhileDictatingState {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            saved_volume: Mutex::new(None),
            bluetooth: Mutex::new(BluetoothState::default()),
        }
    }

    pub fn set(&self, value: bool) {
        self.enabled.store(value, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Called on hotkey press *before* the microphone opens. With a Bluetooth
    /// headset output, mutes now so its switch into call mode is never heard,
    /// regardless of the toggle. Otherwise only settles any mute still held
    /// from a previous Bluetooth dictation. Returns the dictation generation
    /// for [`Self::bluetooth_mic_ready`].
    pub fn begin_dictation(&self, app: &AppHandle, output: Option<BluetoothOutput>) -> u64 {
        let mut bt = self.bluetooth.lock();
        bt.generation += 1;
        // A pending return watch is superseded; carry its mute forward.
        let carried = bt.session.take().is_some_and(|session| session.muted_by_us);
        match output {
            Some(output) => {
                let muted_by_us = carried || mute_system_output(app);
                bt.session = Some(BluetoothSession {
                    output,
                    call_rate: None,
                    muted_by_us,
                    returning: false,
                });
            }
            None if carried => unmute_system_output(app),
            None => {}
        }
        bt.generation
    }

    /// The Bluetooth mic delivered its first frames, so the headset is now in
    /// call mode. With the toggle off, unmute: the user chose to hear other
    /// audio while dictating, and only the transitions needed hiding.
    pub fn bluetooth_mic_ready(&self, app: &AppHandle, generation: u64) {
        let mut bt = self.bluetooth.lock();
        if bt.generation != generation {
            return;
        }
        let Some(session) = bt.session.as_mut().filter(|session| !session.returning) else {
            return;
        };
        session.call_rate = session.output.current_rate();
        crate::murmur_dev_log!(
            "[murmur] bluetooth mic live: music_rate={} call_rate={:?}",
            session.output.music_rate(),
            session.call_rate
        );
        if !session.muted_by_us {
            return;
        }
        if self.is_enabled() {
            // Re-assert in case the profile switch reset the mute.
            set_system_output_muted(true);
        } else {
            unmute_system_output(app);
            session.muted_by_us = false;
        }
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

    /// Called where the current dictation's capture ends: release, Esc, or a
    /// failed start. Restores like [`Self::restore`]; a Bluetooth dictation
    /// additionally stays muted until its headset is back in music mode.
    /// Kept separate from `restore` because pipeline error paths call that
    /// later, possibly while a newer Bluetooth dictation is recording.
    pub fn end_capture(self: &Arc<Self>, app: &AppHandle) {
        self.restore(app);
        self.restore_bluetooth_after_return(app);
    }

    fn restore_bluetooth_after_return(self: &Arc<Self>, app: &AppHandle) {
        let mut bt = self.bluetooth.lock();
        let generation = bt.generation;
        let Some(session) = bt.session.as_mut().filter(|session| !session.returning) else {
            return;
        };
        session.returning = true;
        // Mute (or re-assert it) through the switch back to music mode.
        if session.muted_by_us {
            set_system_output_muted(true);
        } else {
            session.muted_by_us = mute_system_output(app);
        }
        let output = session.output;
        let call_rate = session.call_rate;
        let state = Arc::clone(self);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let (outcome, waited) = output.wait_for_music_mode(call_rate).await;
            crate::murmur_dev_log!(
                "[murmur] bluetooth output restored after {} ms ({outcome:?})",
                waited.as_millis()
            );
            let mut bt = state.bluetooth.lock();
            if bt.generation != generation {
                return;
            }
            if let Some(session) = bt.session.take() {
                if session.muted_by_us {
                    unmute_system_output(&app);
                }
            }
        });
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
    if let Some(path) = bluetooth_sentinel_path(app).filter(|path| path.exists()) {
        crate::murmur_dev_log!(
            "[murmur] mute-audio: turning output mute off from stale bluetooth sentinel"
        );
        set_system_output_muted(false);
        let _ = std::fs::remove_file(&path);
    }
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
    app_data_file(app, SENTINEL_FILENAME)
}

fn bluetooth_sentinel_path(app: &AppHandle) -> Option<PathBuf> {
    app_data_file(app, BLUETOOTH_SENTINEL_FILENAME)
}

fn app_data_file(app: &AppHandle, name: &str) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join(name))
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

/// Turn system output mute on, recording a sentinel when Murmur is the one
/// that changed it. Returns `false` if output was already muted (or the
/// device has no mute control), so the caller never unmutes the user's mute.
fn mute_system_output(app: &AppHandle) -> bool {
    let output = Command::new("osascript")
        .args([
            "-e",
            "set wasMuted to output muted of (get volume settings)",
            "-e",
            "if wasMuted is false then set volume with output muted",
            "-e",
            "return wasMuted",
        ])
        .output();
    let muted_by_us = matches!(
        output,
        Ok(out) if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "false"
    );
    if muted_by_us {
        if let Some(path) = bluetooth_sentinel_path(app) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, "muted");
        }
    }
    muted_by_us
}

fn unmute_system_output(app: &AppHandle) {
    set_system_output_muted(false);
    if let Some(path) = bluetooth_sentinel_path(app) {
        let _ = std::fs::remove_file(&path);
    }
}

fn set_system_output_muted(muted: bool) {
    let verb = if muted { "with" } else { "without" };
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(format!("set volume {verb} output muted"))
        .output();
}
