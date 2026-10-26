#[macro_use]
mod diagnostics;

pub mod audio;
pub mod audio_devices;
pub mod calendar;
pub mod captures;
pub mod corrections;
pub mod display;
pub mod error;
pub mod focused_app;
pub mod history;
pub mod hotkey;
pub mod indicator_native;
pub mod insertion;
mod login_item;
pub mod meeting_capture;
pub mod memory;
pub mod model_download;
pub mod mute_audio;
pub mod native_language;
pub mod native_notifications;
pub mod notify;
pub mod permissions;
pub mod sound;
mod startup_window;
pub mod stt;
pub mod support_diagnostics;
pub mod tray;
pub mod update_install;
pub mod vocabulary;
mod window_events;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tauri::{AppHandle, Manager};

use crate::audio::{AudioHandle, AudioStartError, CaptureSignal, RecordedAudio};
use crate::captures::processing::{
    HeavyProcessingGate, MeetingProcessingSettingsState, MeetingTranscriptionCoordinator,
};
use crate::corrections::CorrectionsState;
use crate::error::Error;
use crate::history::{HistoryRetentionState, TranscriptRecord, TranscriptStore};
use crate::hotkey::{HotkeyEvent, HotkeyState};
use crate::model_download::{ActiveModelsState, ActiveSttEngine, SttEngineSlot};
use crate::mute_audio::MuteWhileDictatingState;
use crate::sound::SoundCuesState;
use crate::stt::{SttEngine, SttOptions};
use crate::vocabulary::VocabularyState;

/// Environment override so M1 can be wired/tested before the download UX exists.
pub const MODEL_PATH_ENV: &str = "MURMUR_WHISPER_MODEL_PATH";

/// How long the user must be inactive before the idle watcher unloads
/// the cached Whisper context. 5 minutes — long enough that bursty
/// dictation sessions (a flurry of holds, then nothing for a few
/// minutes) don't trigger reload churn, short enough that walking
/// away from the desk reclaims the ~165–500 MB of weight memory
/// within a reasonable window.
const STT_IDLE_THRESHOLD: Duration = Duration::from_secs(300);

/// How often the idle watcher wakes up to check the elapsed time.
/// 1 minute is plenty — the actual unload moment can drift by up
/// to one interval past the threshold without anyone noticing.
const STT_IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// Tracks the last hotkey-press timestamp so the idle-unload watcher
/// can decide whether to reclaim the Whisper context. Updated only on
/// `HotkeyEvent::Pressed` — release/transcribe-end aren't necessary
/// because every dictation starts with a press, and a multi-minute
/// hold is unusual enough to not bother covering separately.
pub struct LastActivity {
    at: RwLock<Instant>,
}

#[derive(Default)]
pub struct OnboardingDemoState {
    active: AtomicBool,
}

/// Admission belongs to one hotkey gesture. A rejected start must not let a
/// later release/cancel stop another capture or change its indicator.
#[derive(Default)]
struct DictationAdmission {
    admitted: bool,
}

impl DictationAdmission {
    fn press(&mut self) {
        self.admitted = true;
    }

    fn reject(&mut self) {
        self.admitted = false;
    }

    fn finish(&mut self) -> bool {
        std::mem::take(&mut self.admitted)
    }
}

/// Monotonic ownership for the shared floating indicator. Transcription and
/// transient-result timers outlive a hotkey gesture; checking this generation
/// prevents an older result from hiding or overwriting a newer recording.
#[derive(Default)]
struct IndicatorSession {
    current: AtomicU64,
}

impl IndicatorSession {
    fn begin(&self) -> u64 {
        self.current.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn is_current(&self, session_id: u64) -> bool {
        self.current.load(Ordering::Relaxed) == session_id
    }
}

/// Counts queued/running dictation pipelines so the main window can prevent an
/// updater restart throughout transcription and insertion, not only recording.
/// A count avoids an older pipeline publishing `false` while a newer one waits
/// on the shared processing gate.
struct DictationProcessingGuard {
    app: AppHandle,
    active: Arc<AtomicU64>,
}

impl DictationProcessingGuard {
    fn begin(app: &AppHandle, active: Arc<AtomicU64>) -> Self {
        if active.fetch_add(1, Ordering::AcqRel) == 0 {
            window_events::emit_main(app, "murmur://dictation-processing", true);
        }
        Self {
            app: app.clone(),
            active,
        }
    }
}

impl Drop for DictationProcessingGuard {
    fn drop(&mut self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            window_events::emit_main(&self.app, "murmur://dictation-processing", false);
        }
    }
}

#[derive(Clone, serde::Serialize)]
struct BindingStatePayload {
    pressed: bool,
}

#[derive(Clone, serde::Serialize)]
struct ActivationPayload {
    attempt_id: u64,
    mode: &'static str,
}

#[derive(Clone, serde::Serialize)]
struct AttemptOutcome {
    attempt_id: u64,
    outcome: &'static str,
}

#[derive(Clone, serde::Serialize)]
struct CorrelatedTranscript<'a> {
    attempt_id: u64,
    #[serde(flatten)]
    transcript: &'a crate::stt::Transcript,
}

fn emit_attempt_outcome(app: &AppHandle, attempt_id: u64, outcome: &'static str) {
    window_events::emit_main(
        app,
        "murmur://dictation-attempt-ended",
        AttemptOutcome {
            attempt_id,
            outcome,
        },
    );
}

const DICTATION_RESULT_PILL_DURATION: Duration = Duration::from_millis(3500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoResultKind {
    NoAudio,
    NoSpeech,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NoResultFeedback {
    indicator_state: &'static str,
    notification_title: &'static str,
    notification_body: String,
}

fn no_result_feedback(
    kind: NoResultKind,
    _effective_device_name: Option<&str>,
) -> NoResultFeedback {
    let (indicator_state, notification_title) = match kind {
        NoResultKind::NoAudio => ("no-audio", "No audio detected"),
        NoResultKind::NoSpeech => ("no-speech", "No speech recognized"),
    };
    // Notification Center may show this text on the lock screen. Keep the
    // selected device label inside Murmur's visible indicator instead.
    let notification_body = "Check that your intended audio input is selected.".to_string();

    NoResultFeedback {
        indicator_state,
        notification_title,
        notification_body,
    }
}

impl OnboardingDemoState {
    fn set(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    fn snapshot(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
}

#[tauri::command]
fn set_onboarding_demo_active(state: tauri::State<'_, Arc<OnboardingDemoState>>, active: bool) {
    state.set(active);
}

impl LastActivity {
    pub fn new() -> Self {
        Self {
            at: RwLock::new(Instant::now()),
        }
    }

    pub fn touch(&self) {
        *self.at.write() = Instant::now();
    }

    pub fn elapsed(&self) -> Duration {
        Instant::now().duration_since(*self.at.read())
    }
}

impl Default for LastActivity {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the Whisper model path via the model registry. Prefers
/// `$MURMUR_WHISPER_MODEL_PATH`, then the app-data `models/` dir. The
/// returned path may not exist yet, but app-data resolution itself fails
/// closed during setup rather than falling back to the working directory.
fn resolve_model_path(app: &AppHandle) -> std::io::Result<PathBuf> {
    let entry = model_download::ModelRegistry::get(model_download::DEFAULT_STT_MODEL_ID)
        .expect("default STT model missing from registry");
    model_download::expected_model_path(app, entry).map_err(std::io::Error::other)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(not(debug_assertions))]
    std::panic::set_hook(Box::new(|_| {
        std::eprintln!("[murmur] application terminated unexpectedly");
    }));

    let heavy_processing_gate: HeavyProcessingGate = Arc::new(tokio::sync::Semaphore::new(1));
    let meeting_settings = Arc::new(MeetingProcessingSettingsState::new());
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        // Native OS notifications — fires on the first real error so macOS
        // shows its one-time "Allow Notifications" prompt at that point
        // rather than at every launch. SPEC §2.1 bullet 3.
        .plugin(tauri_plugin_notification::init())
        // Close-to-menu-bar: closing the main window hides it without
        // stopping the hotkey listener. The permanent status item offers
        // Open Murmur and Quit Murmur; only the main window is hidden here.
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            model_download::model_info,
            model_download::list_models,
            model_download::download_model,
            model_download::reveal_model_in_finder,
            model_download::verify_model,
            model_download::set_active_stt,
            model_download::delete_model,
            meeting_capture::mt0_start,
            meeting_capture::mt0_stop,
            meeting_capture::mt0_status,
            meeting_capture::meeting_recorder_start,
            meeting_capture::meeting_capture_capabilities,
            meeting_capture::meeting_recorder_pause,
            meeting_capture::meeting_recorder_resume,
            meeting_capture::meeting_recorder_status,
            meeting_capture::meeting_recorder_cancel,
            meeting_capture::meeting_recorder_stop,
            captures::capture_store_health,
            captures::capture_create,
            captures::capture_retry_transcription,
            captures::capture_retry_audio_cleanup,
            captures::capture_summary_list,
            captures::capture_stats,
            captures::capture_summary_search,
            captures::capture_summary_get,
            captures::capture_detail_get,
            captures::capture_ui_rename,
            captures::capture_ui_retry_transcription,
            captures::capture_ui_pause_transcription,
            captures::capture_ui_resume_transcription,
            captures::capture_ui_cancel_transcription,
            captures::capture_ui_retry_audio_cleanup,
            captures::set_meeting_audio_retention,
            captures::set_meeting_transcript_retention,
            captures::capture_clear_all_meetings,
            captures::capture_delete_audio,
            captures::capture_save_audio_copy,
            captures::capture_ui_retry_finalization,
            captures::capture_ui_regenerate_markdown,
            captures::capture_delete_with_cleanup,
            captures::capture_list,
            captures::capture_get,
            captures::capture_search,
            captures::capture_update,
            captures::capture_rename,
            captures::capture_transition,
            captures::capture_delete,
            captures::capture_retry_cleanup,
            captures::capture_recovery_discover,
            captures::get_capture_output_directory,
            captures::set_capture_output_directory,
            captures::capture_finalize_markdown,
            captures::capture_regenerate_markdown,
            captures::capture_read_transcript,
            captures::capture_read_ai_handoff_context,
            captures::capture_get_transcript_path,
            captures::capture_read_transcript_file,
            captures::capture_open,
            captures::capture_reveal,
            mute_audio::set_mute_while_dictating,
            sound::set_sound_cues,
            vocabulary::set_vocabulary,
            corrections::set_corrections,
            history::history_list,
            history::history_search,
            history::history_delete,
            history::history_clear_all,
            history::history_count,
            history::history_stats,
            history::set_history_retention,
            insertion::request_accessibility_prompt,
            audio::start_mic_monitor,
            audio::stop_mic_monitor,
            audio_devices::list_input_devices,
            audio_devices::set_input_device,
            audio_devices::list_output_devices,
            audio_devices::set_output_device,
            calendar::calendar_permission,
            calendar::request_calendar_access,
            calendar::read_calendar_snapshot,
            permissions::get_permissions,
            permissions::open_privacy_pane,
            permissions::request_input_monitoring_prompt,
            permissions::request_microphone_prompt,
            permissions::restart_murmur,
            set_onboarding_demo_active,
            tray::set_tray_meeting_state,
            tray::set_app_language,
            tray::show_main_window,
            startup_window::startup_launch_at_login,
            login_item::launch_at_login_enabled,
            login_item::set_launch_at_login,
            tray::tray_read_latest_ai_handoff_context,
            tray::tray_read_latest_transcript,
            tray::tray_copy_text,
            hotkey::set_hotkey_binding,
            hotkey::get_hotkey_config,
            support_diagnostics::support_diagnostics,
            update_install::claim_update_install,
            update_install::release_update_install,
        ])
        .manage(tray::TrayState::default())
        .manage(audio_devices::OutputDeviceState::default())
        .manage(meeting_capture::Mt0State::new())
        .manage(meeting_capture::MeetingRecorderState::new())
        .manage(Arc::new(meeting_capture::AudioCaptureArbiter::new()))
        .manage(Arc::clone(&heavy_processing_gate))
        .manage(Arc::clone(&meeting_settings))
        .manage(Arc::new(update_install::UpdateInstallGate::default()))
        .setup(|app| {
            app.manage(startup_window::StartupLaunch::detect());
            // Launch at login is opt-in from Settings and uses a main-app
            // login item; carry a v0.1.0 LaunchAgent opt-in across once.
            login_item::migrate_legacy_agent();
            // Tauri defaults to a regular macOS activation policy even when
            // the bundle declares LSUIElement. Apply accessory at runtime too
            // so LaunchServices and the running app both stay out of the Dock.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let app_handle = app.handle().clone();
            let model_path = resolve_model_path(&app_handle)?;
            crate::murmur_dev_log!("[murmur] whisper model path resolved");

            // Best-effort storage cleanup from prior sessions/builds.
            // Parakeet files were retired; temp WAVs are retained only
            // as a short crash-resilience window and should not grow
            // forever across a month of dictation.
            model_download::cleanup_legacy_parakeet_dir(&app_handle);
            model_download::cleanup_retired_model_files(&app_handle);
            audio::cleanup_stale_temp_wavs();

            // Swappable STT engine. The initial build uses the
            // default STT entry; `build_stt_engine_for` returns the
            // right engine type for the entry. `set_active_stt`
            // replaces the inner `Arc<dyn SttEngine>` at runtime so
            // model swaps take effect without a restart. The hotkey
            // pipeline reads the slot on every Release (see
            // `wire_hotkey`).
            let default_stt_entry =
                model_download::ModelRegistry::get(model_download::DEFAULT_STT_MODEL_ID)
                    .expect("default STT model missing from registry");
            let app_managed = model_download::dev_model_override(default_stt_entry).is_none();
            let engine: Arc<dyn SttEngine> = model_download::build_stt_engine_for(
                default_stt_entry,
                model_path,
                app_managed,
            );
            let engine_slot: SttEngineSlot = Arc::new(RwLock::new(ActiveSttEngine {
                model_id: model_download::DEFAULT_STT_MODEL_ID.to_string(),
                engine,
            }));
            app.manage(Arc::clone(&engine_slot));

            // Do NOT pre-warm STT at boot. A launch-at-login helper that the
            // user never dictates into must stay at the small Tauri/WebKit
            // baseline; loading Whisper on startup dirties hundreds of MB (and
            // on macOS those pages can remain in the process footprint even
            // after the cache owner is dropped). `wire_hotkey` still starts an
            // eager warmup on actual hotkey press, in parallel with recording,
            // so real dictation keeps the warm-path behavior.

            // Idle-unload watcher. After `STT_IDLE_THRESHOLD` of no
            // hotkey activity, drop the cached `WhisperContext` so
            // the model's resident memory (~100 MB tiny.en, ~500 MB
            // multilingual) is reclaimed. The eager warmup wired to
            // hotkey-press in `wire_hotkey` reloads the model in the
            // background while audio is being captured, hiding the
            // cold-load cost from the user on the next dictation
            // after a long idle. Activity is tracked via
            // `LastActivity`, updated on every `HotkeyEvent::Pressed`.
            //
            // Why a background loop rather than a timer-on-press:
            // pressing the hotkey is the only event we care about,
            // but the loop also has to keep running while the user
            // is away (no presses) so it can fire the unload. Cheap
            // — a `sleep(60s)` + lock read.
            let last_activity = Arc::new(LastActivity::new());
            app.manage(Arc::clone(&last_activity));

            let engine_slot_for_idle = Arc::clone(&engine_slot);
            let last_activity_for_idle = Arc::clone(&last_activity);
            let gate_for_idle = Arc::clone(&heavy_processing_gate);
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(STT_IDLE_CHECK_INTERVAL).await;
                    let elapsed = last_activity_for_idle.elapsed();
                    if elapsed >= STT_IDLE_THRESHOLD {
                        let Ok(_permit) = gate_for_idle.clone().acquire_owned().await else { continue };
                        let engine = engine_slot_for_idle.read().engine.clone();
                        engine.unload().await;
                    }
                }
            });

            // Frontend is the source of truth for the active STT id
            // (`localStorage["murmur:active-stt"]`). This mirror reflects the
            // last value `set_active_stt` pushed and starts at the default so
            // guard rails work before the composable boots.
            let active_models = Arc::new(ActiveModelsState::new());
            app.manage(Arc::clone(&active_models));

            // Legacy `<app_data>/trial.json` is intentionally left untouched.
            // Capture no longer reads, creates, or validates purchase state.
            let onboarding_demo = Arc::new(OnboardingDemoState::default());
            app.manage(Arc::clone(&onboarding_demo));

            // Sound cues — frontend source of truth. `useSoundCues` pushes
            // the persisted state into this atomic on mount +
            // on every change via `set_sound_cues`. Default OFF until
            // the composable flips it; no risk of spurious chimes at
            // launch from a racy atomic.
            let sound_cues = Arc::new(SoundCuesState::new());
            app.manage(Arc::clone(&sound_cues));

            // Mute-while-dictating — same frontend-source-of-truth
            // pattern as sound cues. `useMuteWhileDictating` pushes
            // the persisted state via `set_mute_while_dictating`.
            // Also runs a one-shot recovery here: if a prior session
            // muted the output and didn't restore (crashed / force-
            // quit mid-dictation), a sentinel file in app-data holds
            // the saved volume; `recover_on_startup` reads it,
            // restores, and deletes. Keeps the user from finding
            // their system stuck muted after a crash.
            let mute_state = Arc::new(MuteWhileDictatingState::new());
            app.manage(Arc::clone(&mute_state));
            mute_audio::recover_on_startup(&app_handle);

            // User vocabulary — frontend source of truth
            // (`localStorage["murmur:vocabulary"]`), pushed here via
            // `set_vocabulary` on mount + every change. Pipeline reads
            // `initial_prompt()` at transcription time to bias Whisper.
            let vocab = Arc::new(VocabularyState::new());
            app.manage(Arc::clone(&vocab));

            // Corrections — same frontend-source-of-truth pattern as
            // vocabulary (`localStorage["murmur:corrections"]`, pushed
            // here via `set_corrections`). Applied to the Whisper
            // output before paste; see `transcribe_and_emit`.
            let corrections = Arc::new(CorrectionsState::new());
            app.manage(Arc::clone(&corrections));

            // SQLite transcript/capture store (SPEC §2.8). Lives at
            // `<app_data_dir>/murmur.db`. The store creates and validates its
            // own private directory/file boundary, including WAL/SHM, before
            // migrations or capture work. A suspicious or unavailable durable
            // path is never deleted or repaired to make startup succeed.
            // Dictation retains the explicit in-memory fallback; meeting
            // capture continues to reject nondurable persistence.
            let store = Arc::new(match app_handle.path().app_data_dir() {
                Ok(app_data_dir) => {
                    let db_path = app_data_dir.join("murmur.db");
                    crate::murmur_dev_log!("[murmur] history database path resolved");
                    match TranscriptStore::open(&db_path) {
                        Ok(store) => store,
                        Err(error) => {
                            crate::murmur_dev_log!(
                                "[murmur] WARN: durable history storage unavailable ({error}); using ephemeral storage"
                            );
                            TranscriptStore::open_ephemeral()
                                .expect("in-memory SQLite open must succeed")
                        }
                    }
                }
                Err(error) => {
                    crate::murmur_dev_log!(
                        "[murmur] WARN: app-data directory unavailable ({error}); using ephemeral storage"
                    );
                    TranscriptStore::open_ephemeral()
                        .expect("in-memory SQLite open must succeed")
                }
            });
            app.manage(Arc::clone(&store));
            let meeting_audio_retention = Arc::new(captures::MeetingAudioRetentionState::new());
            app.manage(Arc::clone(&meeting_audio_retention));
            let meeting_transcript_retention =
                Arc::new(captures::MeetingTranscriptRetentionState::new());
            app.manage(Arc::clone(&meeting_transcript_retention));
            let update_install_gate = Arc::clone(
                app.state::<Arc<update_install::UpdateInstallGate>>()
                    .inner(),
            );
            if let Ok(staging_root) = app_handle.path().app_data_dir().map(|path| {
                path.join("captures").join("staging")
            }) {
                let _ = store.reconcile_completed_staging_paths(&staging_root);
            }
            let _ = captures::cleanup_expired_capture_audio(
                &app_handle,
                &store,
                history::now_millis(),
            );
            let cleanup_app = app_handle.clone();
            let cleanup_store = Arc::clone(&store);
            let cleanup_meeting_transcript_retention =
                Arc::clone(&meeting_transcript_retention);
            let cleanup_update_install_gate = Arc::clone(&update_install_gate);
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(15 * 60)).await;
                    let Some(update_activity) = cleanup_update_install_gate.begin_activity() else {
                        continue;
                    };
                    let app = cleanup_app.clone();
                    let store = Arc::clone(&cleanup_store);
                    let transcript_retention =
                        Arc::clone(&cleanup_meeting_transcript_retention);
                    let _ = tauri::async_runtime::spawn_blocking(move || {
                        let _update_activity = update_activity;
                        let _ = captures::cleanup_expired_capture_audio(
                            &app,
                            &store,
                            history::now_millis(),
                        );
                        captures::cleanup_expired_meeting_transcripts(
                            &app,
                            &store,
                            &transcript_retention,
                        )
                    })
                    .await;
                }
            });
            let coordinator = MeetingTranscriptionCoordinator::new(
                app_handle.clone(),
                Arc::clone(&store),
                Arc::clone(&engine_slot),
                Arc::clone(&heavy_processing_gate),
                Arc::clone(&meeting_audio_retention),
                Arc::clone(&update_install_gate),
            );
            app.manage(Arc::clone(&coordinator));
            if let Ok(ids) = store.transcribing_capture_ids() {
                for capture_id in ids {
                    coordinator.enqueue(capture_id);
                }
            }

            // Retention policy — frontend source of truth
            // (`localStorage["murmur:history-retention"]`), pushed via
            // `set_history_retention`. 0 = forever (default). Initial
            // purge on boot catches the case where the user changed
            // the setting in a previous session but no dictation has
            // happened yet to trigger the post-insert purge.
            let retention = Arc::new(HistoryRetentionState::new());
            app.manage(Arc::clone(&retention));
            history::purge_per_retention(&store, &retention);

            let capture_arbiter = Arc::clone(
                app.state::<Arc<meeting_capture::AudioCaptureArbiter>>()
                    .inner(),
            );
            let audio = audio::spawn(app_handle.clone(), capture_arbiter);
            // Make the audio handle available to Tauri commands
            // (`audio_devices::set_input_device` needs it to update the
            // preferred-device mutex from outside the audio thread).
            app.manage(audio.clone());
            audio_devices::spawn_watcher(app_handle.clone(), audio.clone());

            // Hotkey binding — frontend source of truth
            // (`localStorage["murmur:hotkey-binding"]`), pushed here via
            // `set_hotkey_binding` on mount and every change. Default
            // is Right ⌘ so first-run has zero regression while the
            // composable is booting. Activation mode (hold vs.
            // double-tap-toggle) is no longer a setting — both
            // gestures work on the same binding, routed by the
            // gesture-driven state machine in `hotkey.rs`.
            let hotkey_state = Arc::new(HotkeyState::default());
            app.manage(Arc::clone(&hotkey_state));

            // Log the current Accessibility state but do NOT trigger
            // the TCC prompt here. The onboarding wizard's Permissions
            // step drives all user-visible system prompts now (via the
            // Grant button on each PermissionRow) so the native dialogs
            // don't stack on top of the onboarding modal before the
            // user has even seen it. See the `permissions::*_prompt`
            // Tauri commands + PermissionRow.vue.
            crate::murmur_dev_log!(
                "[murmur] accessibility trusted at boot: {}",
                insertion::is_trusted()
            );

            // Permission watcher — emits `murmur://permissions` (full
            // snapshot) every 2 s on change, so the Permissions page
            // and home banner react to grants / revokes without a
            // relaunch.
            permissions::spawn_watcher(app_handle.clone());

            // Force the borderless windows' NSWindows to be fully transparent
            // at the AppKit layer. Tauri's `transparent: true` should do this,
            // but on macOS 26 it isn't fully taking — see indicator_native.rs
            // for the full story. Both of these draw a rounded pill, which
            // shows the window's square corners if the backing stays opaque.
            indicator_native::force_transparent(&app_handle, indicator_native::INDICATOR);

            bootstrap_indicator_first_paint(&app_handle);
            #[cfg(feature = "meeting-capture")]
            {
                indicator_native::force_transparent(
                    &app_handle,
                    indicator_native::MEETING_CONTROLLER,
                );
                bootstrap_meeting_controller_first_paint(&app_handle);
            }

            // Install the only persistent entry point during native setup.
            // Never depend on webview localStorage (including
            // old "hide menu bar icon" preferences) to make it appear.
            tray::install(&app_handle, true).map_err(std::io::Error::other)?;

            wire_hotkey(
                app_handle,
                engine_slot,
                sound_cues,
                mute_state,
                vocab,
                corrections,
                store,
                retention,
                onboarding_demo,
                audio,
                hotkey_state,
                last_activity,
                heavy_processing_gate,
                update_install_gate,
            );
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        // Finder / Dock reopening an already-running app is an explicit
        // request to show its window, even if AppKit considers it visible but
        // minimized or not focused. This explicit request must show it even
        // when this process originally started quietly at login.
        .run(|app_handle, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Err(error) = tray::show_main_window(app_handle.clone()) {
                    crate::murmur_dev_log!("[murmur] reopen main window failed: {error}");
                }
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app_handle, event);
        });
}

#[allow(clippy::too_many_arguments)]
fn wire_hotkey(
    app: AppHandle,
    engine_slot: SttEngineSlot,
    sound_cues: Arc<SoundCuesState>,
    mute_state: Arc<MuteWhileDictatingState>,
    vocab: Arc<VocabularyState>,
    corrections: Arc<CorrectionsState>,
    store: Arc<TranscriptStore>,
    retention: Arc<HistoryRetentionState>,
    onboarding_demo: Arc<OnboardingDemoState>,
    audio: AudioHandle,
    hotkey_state: Arc<HotkeyState>,
    last_activity: Arc<LastActivity>,
    heavy_processing_gate: HeavyProcessingGate,
    update_install_gate: Arc<update_install::UpdateInstallGate>,
) {
    let runtime = tauri::async_runtime::handle();
    let indicator_session = Arc::new(IndicatorSession::default());
    let dictation_processing = Arc::new(AtomicU64::new(0));
    let mut current_indicator_session_id = 0_u64;
    let mut current_demo = false;
    let mut current_update_activity: Option<update_install::UpdateActivityGuard> = None;
    let mut admission = DictationAdmission::default();

    hotkey::spawn(hotkey_state, move |event| match event {
        HotkeyEvent::BindingState(pressed) => {
            window_events::emit_main(
                &app,
                "murmur://hotkey-binding-state",
                BindingStatePayload { pressed },
            );
        }
        HotkeyEvent::Pressed(mode) => {
            // Native update admission closes the race between the webview's
            // visible-state check and updater installation. The lease remains
            // alive through recording, transcription, and insertion.
            let Some(update_activity) = update_install_gate.begin_activity() else {
                admission.reject();
                crate::murmur_dev_log!("[murmur] dictation ignored: update installation starting");
                return;
            };

            // Reject a meeting-owned hotkey before dictation UI/audio effects
            // or any dictation UI/audio effect. `start` repeats this check while
            // reserving the lease so a meeting that wins the race is also inert.
            if audio.meeting_capture_active() {
                admission.reject();
                crate::murmur_dev_log!("[murmur] dictation ignored: meeting capture active");
                return;
            }

            // A pipeline can finish after the step exits or another capture
            // starts. Snapshot its onboarding insertion/privacy policy.
            current_demo = onboarding_demo.snapshot();
            admission.press();
            // Reserve and start dictation audio before publishing any recording
            // UI. A meeting owns the shared capture lease, so its hotkey
            // rejection must remain invisible and must not touch meeting state.
            let capture_ready = match audio.start() {
                Ok(ready) => ready,
                Err(error) => {
                    // No release/cancel event from this rejected gesture may call
                    // into dictation audio or alter an existing indicator session.
                    admission.reject();
                    if error == AudioStartError::CaptureBusy {
                        crate::murmur_dev_log!(
                            "[murmur] dictation ignored: meeting capture active"
                        );
                        return;
                    }

                    current_indicator_session_id = indicator_session.begin();
                    crate::murmur_dev_log!("[murmur] audio start failed: {error}");
                    emit_attempt_outcome(&app, current_indicator_session_id, "pipeline-error");
                    window_events::emit_main(
                        &app,
                        "murmur://hotkey-binding-state",
                        BindingStatePayload { pressed: false },
                    );
                    emit_indicator_state_for_session(
                        &app,
                        &indicator_session,
                        current_indicator_session_id,
                        "microphone-error",
                    );
                    show_indicator(&app);
                    notify::error(
                        &app,
                        "Microphone unavailable",
                        "Murmur couldn't start recording. Check your input device, clamshell/display setup, and Microphone permission.",
                    );
                    sound::play_cancel(&sound_cues);
                    schedule_indicator_hide(
                        app.clone(),
                        Arc::clone(&indicator_session),
                        current_indicator_session_id,
                        DICTATION_RESULT_PILL_DURATION,
                    );
                    return;
                }
            };

            current_update_activity = Some(update_activity);
            current_indicator_session_id = indicator_session.begin();
            // Activity touch — refreshes the idle-unload timer so
            // the watcher doesn't reclaim the model mid-session.
            last_activity.touch();
            window_events::emit_main(
                &app,
                "murmur://activation-mode",
                ActivationPayload {
                    attempt_id: current_indicator_session_id,
                    mode: mode.as_str(),
                },
            );
            window_events::emit_main(&app, "murmur://recording", true);
            emit_indicator_state_for_session(
                &app,
                &indicator_session,
                current_indicator_session_id,
                "recording",
            );
            show_indicator(&app);
            // Bluetooth outputs can still be switching into microphone mode
            // when `Stream::play()` returns. Schedule the start cue after the
            // first input callback, without delaying or blocking capture.
            let sound_cues_for_start = Arc::clone(&sound_cues);
            runtime.spawn(async move {
                sound::play_start_when_ready(sound_cues_for_start, capture_ready).await;
            });
            // Mute system output if opted in. NSSound uses the separate alert
            // volume (see mute_audio.rs); the cue may now play after muting.
            // Restore at every exit path; a sentinel covers crashes.
            mute_state.mute_if_enabled(&app);
            // Eager warmup. Idempotent on the WhisperEngine cache:
            // a no-op when the model is already loaded, kicks the
            // load in parallel with audio capture when the idle
            // watcher has unloaded. Hides the cold-load latency
            // inside the user's recording window so the first press
            // after a long idle doesn't see a longer "Transcribing…"
            // pill — the model is already (or almost) ready by the
            // time release fires `transcribe()`.
            let engine_slot_for_warmup = Arc::clone(&engine_slot);
            let gate_for_warmup = Arc::clone(&heavy_processing_gate);
            let update_activity_for_warmup = current_update_activity
                .as_ref()
                .expect("successful dictation start owns update activity")
                .clone();
            runtime.spawn(async move {
                let _update_activity = update_activity_for_warmup;
                let Ok(_permit) = gate_for_warmup.acquire_owned().await else {
                    return;
                };
                let engine_for_warmup = engine_slot_for_warmup.read().engine.clone();
                engine_for_warmup.warmup().await;
            });
        }
        HotkeyEvent::Released => {
            // A rejected press owns no audio or indicator. In particular, don't
            // disturb a previously accepted attempt still transcribing.
            if !admission.finish() {
                return;
            }
            let Some(update_activity) = current_update_activity.take() else {
                crate::murmur_dev_log!("[murmur] accepted dictation lost update activity");
                return;
            };
            window_events::emit_main(&app, "murmur://recording", false);
            // Keep the indicator visible through transcription —
            // `transcribe_and_emit` drives it to "idle"
            // (which hides the window) when the paste lands, or on any
            // error path. Swap the pill content to "Transcribing…"
            // immediately so the user sees the hand-off from recording
            // to processing.
            emit_indicator_state_for_session(
                &app,
                &indicator_session,
                current_indicator_session_id,
                "transcribing",
            );
            let Some(recorded) = audio.stop() else {
                crate::murmur_dev_log!("[murmur] stop returned no audio (was recording active?)");
                hide_indicator_for_session(&app, &indicator_session, current_indicator_session_id);
                emit_attempt_outcome(&app, current_indicator_session_id, "no-audio");
                sound::play_cancel(&sound_cues);
                // No samples → nothing to transcribe; restore system
                // volume now so other apps aren't stuck silent.
                mute_state.restore(&app);
                return;
            };
            // Recording capture is done — restore system volume now
            // so the user can hear other apps (YouTube, music,
            // video calls) during transcription. Previously the restore fired inside
            // `transcribe_and_emit` after the full pipeline, which
            // left other audio silent for seconds on every hold.
            // Later restores downstream are no-ops once `saved_volume`
            // has been taken — kept as belt-and-suspenders for error
            // paths but they won't re-restore.
            mute_state.restore(&app);

            // Assess only after a normal release. Near-digital silence gets
            // actionable feedback immediately and never writes a WAV, enters
            // Whisper, emits a transcript, or touches history/clipboard.
            if matches!(
                recorded.signal(),
                CaptureSignal::NoFrames | CaptureSignal::NearDigitalSilence
            ) {
                crate::murmur_dev_log!(
                    "[murmur] capture contained no meaningful microphone signal"
                );
                show_no_result_feedback(
                    &app,
                    &indicator_session,
                    current_indicator_session_id,
                    NoResultKind::NoAudio,
                    recorded.effective_device_name.as_deref(),
                );
                sound::play_cancel(&sound_cues);
                return;
            }

            let app = app.clone();
            // Snapshot the STT engine only after the shared permit is acquired.
            // This ordering prevents a warmup or model swap from racing dictation.
            let engine_slot_for_pipeline = Arc::clone(&engine_slot);
            let gate_for_pipeline = Arc::clone(&heavy_processing_gate);
            let sound_cues_pipeline = Arc::clone(&sound_cues);
            let mute_pipeline = Arc::clone(&mute_state);
            let vocab = Arc::clone(&vocab);
            let corrections = Arc::clone(&corrections);
            let store = Arc::clone(&store);
            let retention = Arc::clone(&retention);
            let onboarding_demo_for_pipeline = current_demo;
            let indicator_session_for_pipeline = Arc::clone(&indicator_session);
            let indicator_session_id = current_indicator_session_id;
            let processing_guard =
                DictationProcessingGuard::begin(&app, Arc::clone(&dictation_processing));
            runtime.spawn(async move {
                let _update_activity = update_activity;
                let _processing_guard = processing_guard;
                let Ok(permit) = gate_for_pipeline.acquire_owned().await else {
                    emit_attempt_outcome(&app, indicator_session_id, "pipeline-error");
                    return;
                };
                let engine = engine_slot_for_pipeline.read().engine.clone();
                if let Err(e) = transcribe_and_emit(
                    app.clone(),
                    engine,
                    Arc::clone(&sound_cues_pipeline),
                    Arc::clone(&mute_pipeline),
                    vocab,
                    corrections,
                    store,
                    retention,
                    onboarding_demo_for_pipeline,
                    recorded,
                    permit,
                    Arc::clone(&indicator_session_for_pipeline),
                    indicator_session_id,
                )
                .await
                {
                    crate::murmur_dev_log!("[murmur] transcription pipeline error: {e}");
                    emit_attempt_outcome(&app, indicator_session_id, "pipeline-error");
                    // Reset the indicator so a pipeline error cannot leave the
                    // transcribing pill visible indefinitely. A newer recording
                    // keeps ownership if it started while this pipeline ran.
                    hide_indicator_for_session(
                        &app,
                        &indicator_session_for_pipeline,
                        indicator_session_id,
                    );
                    // Pipeline error = dictation didn't reach the target
                    // app. Match the cancel audio so the user has a
                    // non-visual signal that something went wrong.
                    sound::play_cancel(&sound_cues_pipeline);
                    // Restore output volume — the pipeline is dead,
                    // no reason to keep other apps muted.
                    mute_pipeline.restore(&app);
                }
            });
        }
        HotkeyEvent::Cancelled => {
            if !admission.finish() {
                return;
            }
            let update_activity = current_update_activity.take();
            audio.cancel();

            // Press-time warmup may have loaded Whisper even though Escape
            // bypasses transcription, so the normal post-STT unload path will
            // never run. Wait behind any in-flight warmup, then release only
            // if this cancelled attempt still owns the current session. A
            // newer recording either keeps its warm context or performs its
            // own post-transcription unload.
            let engine_slot_for_cancel = Arc::clone(&engine_slot);
            let gate_for_cancel = Arc::clone(&heavy_processing_gate);
            let indicator_session_for_cancel = Arc::clone(&indicator_session);
            let cancelled_session_id = current_indicator_session_id;
            runtime.spawn(async move {
                let _update_activity = update_activity;
                let Ok(_permit) = gate_for_cancel.acquire_owned().await else {
                    return;
                };
                if !indicator_session_for_cancel.is_current(cancelled_session_id) {
                    return;
                }
                let engine = engine_slot_for_cancel.read().engine.clone();
                engine.unload().await;
            });

            emit_attempt_outcome(&app, current_indicator_session_id, "cancelled");
            window_events::emit_main(
                &app,
                "murmur://hotkey-binding-state",
                BindingStatePayload { pressed: false },
            );
            window_events::emit_main(&app, "murmur://recording", false);
            window_events::emit_main(&app, "murmur://cancelled", ());
            // Keep the pill visible with a "Cancelled" label long enough
            // for the user to glance at and register it, especially when
            // they've already switched windows. Bumped from 1.5 s to 2 s
            // after live feedback — 1.5 felt like it flashed and vanished
            // before the user could process both the pill + the cancel
            // chime. 2 s lands just past typical reaction+registration
            // time without overstaying.
            emit_indicator_state_for_session(
                &app,
                &indicator_session,
                current_indicator_session_id,
                "cancelled",
            );
            sound::play_cancel(&sound_cues);
            // Restore output volume *immediately*, not paired with the
            // 2 s pill fade — there's no reason to keep YouTube muted
            // while the Cancelled pill is still visible.
            mute_state.restore(&app);
            schedule_indicator_hide(
                app.clone(),
                Arc::clone(&indicator_session),
                current_indicator_session_id,
                Duration::from_millis(2000),
            );
            crate::murmur_dev_log!("[murmur] capture cancelled by user (Esc)");
        }
    });
}

/// Publish the indicator-pill state to the frontend. The payload is a
/// string enum — `"recording" | "transcribing" | "cancelled" |
/// "microphone-error" | "model-missing" | "recording-limit" |
/// "no-audio" | "no-speech" | "idle"` — consumed by
/// `src/components/Indicator.vue`
/// via `@tauri-apps/api/event::listen("murmur://indicator-state")`.
/// Window show/hide stays controlled from Rust (`show_indicator` /
/// `hide_indicator`); this event only drives pill *content*, so a
/// stale state never leaves a ghost window visible.
fn emit_indicator_state(app: &AppHandle, state: &str) {
    window_events::emit_dictation_ui(app, "murmur://indicator-state", state);
}

fn emit_indicator_state_for_session(
    app: &AppHandle,
    sessions: &IndicatorSession,
    session_id: u64,
    state: &str,
) {
    if sessions.is_current(session_id) {
        emit_indicator_state(app, state);
    }
}

fn hide_indicator_for_session(app: &AppHandle, sessions: &IndicatorSession, session_id: u64) {
    if sessions.is_current(session_id) {
        emit_indicator_state(app, "idle");
        hide_indicator(app);
    }
}

fn schedule_indicator_hide(
    app: AppHandle,
    sessions: Arc<IndicatorSession>,
    session_id: u64,
    duration: Duration,
) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(duration).await;
        hide_indicator_for_session(&app, &sessions, session_id);
    });
}

fn show_no_result_feedback(
    app: &AppHandle,
    sessions: &Arc<IndicatorSession>,
    session_id: u64,
    kind: NoResultKind,
    effective_device_name: Option<&str>,
) {
    let feedback = no_result_feedback(kind, effective_device_name);
    emit_attempt_outcome(app, session_id, feedback.indicator_state);

    // Only the current session may change the shared pill. This keeps an older
    // Whisper completion from overwriting a newer recording. Every completed
    // no-result dictation still gets exactly one native notification.
    if sessions.is_current(session_id) {
        window_events::emit_indicator(
            app,
            "murmur://indicator-device",
            effective_device_name.unwrap_or_default(),
        );
        emit_indicator_state(app, feedback.indicator_state);
        show_indicator(app);
        schedule_indicator_hide(
            app.clone(),
            Arc::clone(sessions),
            session_id,
            DICTATION_RESULT_PILL_DURATION,
        );
    }
    notify::info(
        app,
        feedback.notification_title,
        &feedback.notification_body,
    );
}

/// Position and reveal the floating recording indicator.
///
/// On macOS, native placement selects the display containing the focused UI
/// control in the frontmost application, falling back to the focused window,
/// pointer display, then primary display. AppKit performs the final placement
/// in one logical coordinate space so mixed display scale factors cannot skew
/// the result. Other platforms and native-handle failures retain a primary
/// monitor fallback.
fn show_indicator(app: &AppHandle) {
    let Some(window) = app.get_webview_window("indicator") else {
        crate::murmur_dev_log!("[murmur] indicator window not found");
        return;
    };

    if !indicator_native::position_on_focused_screen(app) {
        if let Ok(Some(monitor)) = window.primary_monitor() {
            let scale = monitor.scale_factor();
            let work_area = monitor.work_area();
            let position = work_area.position.to_logical::<f64>(scale);
            let size = work_area.size.to_logical::<f64>(scale);
            let win_w_pt = 360.0;
            let win_h_pt = 40.0;
            let bottom_margin_pt = 20.0;
            let x = position.x + (size.width - win_w_pt) / 2.0;
            let y = position.y + size.height - win_h_pt - bottom_margin_pt;
            let _ = window.set_position(tauri::LogicalPosition::new(x, y));
        }
    }

    let _ = window.show();
    // Re-assert always-on-top in case macOS demoted it after a Space switch.
    let _ = window.set_always_on_top(true);
    // Re-apply native transparency defensively — cheap, idempotent,
    // covers any timing race where `setup()` ran before the WKWebView
    // subtree was fully constructed.
    indicator_native::force_transparent(app, indicator_native::INDICATOR);
}

fn hide_indicator(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("indicator") {
        let _ = window.hide();
    }
}

#[cfg(feature = "meeting-capture")]
fn bootstrap_meeting_controller_first_paint(app: &AppHandle) {
    let Some(window) = app.get_webview_window("meeting-controller") else {
        crate::murmur_dev_log!("[murmur] bootstrap: meeting controller window not found");
        return;
    };
    let _ = window.set_position(tauri::PhysicalPosition::new(-10_000, -10_000));
    let _ = window.show();
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let _ = window.hide();
    });
}

/// Force the indicator WebView to first-paint at startup so the user's
/// first Right ⌘ press actually shows the pill. Tauri lazily initializes
/// the WebView inside a window declared `visible: false`, so without this
/// bootstrap the first `show()` call puts a blank surface on screen and
/// only the second hold renders correctly.
///
/// We show the window at `(-10_000, -10_000)` — comfortably off every
/// monitor on every current display — then hide it after ~300 ms on the
/// async runtime so the caller isn't blocked. The user never sees the
/// bootstrap paint. `show_indicator` will re-position on the real first
/// press.
fn bootstrap_indicator_first_paint(app: &AppHandle) {
    let Some(window) = app.get_webview_window("indicator") else {
        crate::murmur_dev_log!("[murmur] bootstrap: indicator window not found");
        return;
    };
    let _ = window.set_position(tauri::PhysicalPosition::new(-10_000, -10_000));
    let _ = window.show();
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let _ = window.hide();
    });
}

fn should_delete_dictation_wav(result: &Result<(), insertion::InsertError>) -> bool {
    result.is_ok()
}

fn insertion_failure_notification(error: insertion::InsertError) -> (&'static str, &'static str) {
    if error.restore_failed() {
        if error.paste_was_requested() {
            (
                "Clipboard restore failed",
                "Murmur sent the paste command but couldn't restore the previous clipboard.",
            )
        } else {
            (
                "Clipboard restore failed",
                "Murmur couldn't complete the insertion and couldn't restore the previous clipboard.",
            )
        }
    } else {
        (
            "Paste failed",
            "Murmur transcribed your dictation but couldn't complete the paste safely.",
        )
    }
}

fn dictation_stt_options(initial_prompt: Option<String>) -> SttOptions {
    SttOptions {
        initial_prompt,
        language: None,
        ..SttOptions::default()
    }
}

#[allow(clippy::too_many_arguments)]
async fn transcribe_and_emit(
    app: AppHandle,
    engine: Arc<dyn SttEngine>,
    sound_cues: Arc<SoundCuesState>,
    mute_state: Arc<MuteWhileDictatingState>,
    vocab: Arc<VocabularyState>,
    corrections: Arc<CorrectionsState>,
    store: Arc<TranscriptStore>,
    retention: Arc<HistoryRetentionState>,
    onboarding_demo: bool,
    audio: RecordedAudio,
    _heavy_permit: tokio::sync::OwnedSemaphorePermit,
    indicator_session: Arc<IndicatorSession>,
    indicator_session_id: u64,
) -> crate::error::Result<()> {
    let temp_wav = audio::write_temp_wav(&audio)?;

    // Build the Whisper options for this capture. Vocabulary (§2.5) gets
    // packed into `initial_prompt` as a glossary hint so proper nouns /
    // jargon / acronyms the user cares about transcribe correctly on
    // the first pass. Empty vocabulary → None → Whisper runs without a
    // prompt, same as before.
    //
    // Dictation always uses multilingual auto-detect, independent of App
    // language and of any old localStorage hint. `None` must be passed to
    // Whisper's set_language; skipping that call defaults to English.
    let stt_opts = dictation_stt_options(vocab.initial_prompt());
    let transcribe_result = engine
        .transcribe(temp_wav.transcription_path(), &stt_opts)
        .await;

    // The multilingual Whisper model's decode path can leave multiple GB
    // resident until the cached context is dropped. Keeping it warm for a
    // five-minute idle window made active dictation sessions trip macOS's
    // heavy-memory warning even though history/text storage was tiny. Unload
    // immediately after every attempted STT pass (success or backend error);
    // the next hotkey press still kicks an eager warmup in parallel with
    // recording, so normal holds hide most of the cold-load cost while idle
    // RAM returns to baseline promptly. ModelNotFound never loaded a context,
    // so there is nothing to reclaim on that path.
    if !matches!(transcribe_result, Err(Error::ModelNotFound(_))) {
        engine.unload().await;
    }

    let mut transcript = match transcribe_result {
        Ok(transcript) => transcript,
        Err(Error::ModelNotFound(_)) => {
            crate::murmur_dev_log!("[murmur] speech model missing; cannot transcribe");
            emit_attempt_outcome(&app, indicator_session_id, "model-missing");
            let _ = temp_wav.try_remove_if_same();
            emit_indicator_state_for_session(
                &app,
                &indicator_session,
                indicator_session_id,
                "model-missing",
            );
            notify::error(
                &app,
                "Speech model missing",
                "Murmur couldn't find its local Whisper model. Open Settings → Speech model and download the speech-to-text model.",
            );
            sound::play_cancel(&sound_cues);
            mute_state.restore(&app);
            tokio::time::sleep(DICTATION_RESULT_PILL_DURATION).await;
            hide_indicator_for_session(&app, &indicator_session, indicator_session_id);
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    // Leave the WAV in place until the insertion outcome is known. Files
    // retained after a failed or interrupted transaction are eligible for the
    // existing launch-time janitor once they are at least 24 hours old.
    // Transcript content is private. Diagnostics may record aggregate shape,
    // but never the words themselves.
    crate::murmur_dev_log!(
        "[murmur] transcript ready: audio_ms={} chars={} words={}",
        transcript.duration_ms,
        transcript.text.chars().count(),
        transcript.text.split_whitespace().count()
    );

    // Meaningful audio can still yield no usable speech after Whisper's
    // annotation filtering. Keep this distinct from near-digital silence so
    // the feedback does not incorrectly blame the microphone route.
    if transcript.text.trim().is_empty() {
        crate::murmur_dev_log!("[murmur] transcript empty after filtering; no speech recognized");
        // Remove private temporary audio before the visible result dwell.
        let _ = temp_wav.try_remove_if_same();
        show_no_result_feedback(
            &app,
            &indicator_session,
            indicator_session_id,
            NoResultKind::NoSpeech,
            audio.effective_device_name.as_deref(),
        );
        sound::play_cancel(&sound_cues);
        mute_state.restore(&app);
        return Ok(());
    }

    // Corrections — deterministic find/replace after STT (SPEC §2.6).
    // Empty ruleset → no-op.
    // Snapshot the raw text first so the history store can keep both
    // the pre-correction and post-correction versions — useful for
    // later auditing and the "remember this correction" flow (§2.6).
    let text_raw = transcript.text.clone();
    let corrected = corrections.apply(&transcript.text);
    if corrected != transcript.text {
        crate::murmur_dev_log!(
            "[murmur] corrections applied: input_chars={} output_chars={}",
            transcript.text.chars().count(),
            corrected.chars().count()
        );
        transcript.text = corrected;
    }

    // Attribute the transcript to whichever app is frontmost right now and
    // retain its native process-instance identity as the insertion target.
    // That identity stays native-only; only name/bundle metadata is serialized.
    // Insertion rechecks it before clipboard mutation and event posting.
    let insertion_target = focused_app::current_target();
    transcript.app = insertion_target.as_ref().map(|target| target.app.clone());
    if let Some(app_info) = &transcript.app {
        crate::murmur_dev_log!(
            "[murmur] attributing to app: {:?} ({})",
            app_info.name.as_deref().unwrap_or("?"),
            app_info.bundle_id.as_deref().unwrap_or("?")
        );
    }

    if onboarding_demo {
        crate::murmur_dev_log!("[murmur] onboarding demo transcript skipped for history");
    } else {
        // Persist to the SQLite history store (SPEC §2.8). Failure here
        // shouldn't break dictation — the paste still happens, the user
        // just loses the history entry. Log and move on.
        let app_name = transcript.app.as_ref().and_then(|a| a.name.clone());
        let app_bundle = transcript.app.as_ref().and_then(|a| a.bundle_id.clone());
        let record = TranscriptRecord::new_now(
            text_raw,
            transcript.text.clone(),
            transcript.duration_ms,
            app_name,
            app_bundle,
        );
        match store.insert(&record) {
            Ok(_id) => {
                history::purge_per_retention(&store, &retention);
                tray::refresh_dictation_copy_availability(&app, &store);
            }
            Err(e) => crate::murmur_dev_log!("[murmur] history insert failed: {e}"),
        }
    }

    window_events::emit_main(
        &app,
        "murmur://transcript",
        CorrelatedTranscript {
            attempt_id: indicator_session_id,
            transcript: &transcript,
        },
    );

    if onboarding_demo {
        // In the onboarding shortcut tester, the Vue demo textarea is the
        // insertion target. The frontend mirrors the transcript event into
        // that field, so running the normal clipboard + ⌘V insertion here
        // would append the same text a second time while the textarea has
        // focus.
        hide_indicator_for_session(&app, &indicator_session, indicator_session_id);
        sound::play_stop(&sound_cues);
        mute_state.restore(&app);
        let _ = temp_wav.try_remove_if_same();
        return Ok(());
    }

    // Pipeline is done (STT + deterministic corrections) —
    // hide the indicator just before the paste fires. The paste itself
    // is a few-ms synthetic ⌘V + clipboard restore; waiting for it to
    // complete before hiding would leave the pill lingering after the
    // user already sees the text appear in their focused app.
    hide_indicator_for_session(&app, &indicator_session, indicator_session_id);
    // Transcription complete → "Bottle" chime. It fires before insertion,
    // so it does not claim the destination accepted a paste. A later
    // insertion error also plays Basso, producing truthful completion-then-
    // failure cues for the same dictation.
    sound::play_stop(&sound_cues);
    // Restore output volume — dictation is complete, other apps
    // come back to normal. Done before the paste spawn so there's
    // no "I hear the paste happening but my music is still muted"
    // window.
    mute_state.restore(&app);

    // Paste only if the app captured at attribution time remains frontmost.
    // Runs off the async executor because it briefly blocks while preserving
    // and restoring the prior pasteboard.
    let text = transcript.text.clone();
    let insertion_target_identity = insertion_target.map(|target| target.identity);
    let app_for_insert = app.clone();
    let sound_cues_for_insert = Arc::clone(&sound_cues);
    tauri::async_runtime::spawn_blocking(move || {
        let insertion_result = insertion::insert(&app_for_insert, &text, insertion_target_identity);
        if should_delete_dictation_wav(&insertion_result) {
            // `Ok(())` means the insertion request was posted and the
            // clipboard transaction completed or ceded ownership without a
            // detected error; CGEventPost cannot prove destination acceptance.
            let _ = temp_wav.try_remove_if_same();
        }
        match insertion_result {
            Ok(()) => {}
            Err(insertion::InsertError::NotTrusted) => {
                // Accessibility has its own in-app surface (amber banner +
                // Settings → Permissions row) — firing a notification OR a
                // cancel chime too would be noise on top of a visible,
                // permanent UI state. Stays silent on both surfaces.
                // Push a fresh snapshot so the banner updates immediately
                // instead of waiting up to 2 s for the watcher's next tick.
                crate::murmur_dev_log!(
                    "[murmur] skipping paste: Accessibility permission not granted"
                );
                window_events::emit_main(
                    &app_for_insert,
                    "murmur://permissions",
                    permissions::current(),
                );
            }
            Err(e) => {
                // Keep the diagnostic content-free. A post-event restoration
                // failure needs different copy: CGEventPost cannot prove the
                // target accepted the paste, but saying it never happened
                // would also be false.
                crate::murmur_dev_log!("[murmur] insertion error: {e}");
                let (title, body) = insertion_failure_notification(e);
                notify::error(&app_for_insert, title, body);
                // Any insertion or restoration failure retains the temp WAV
                // for cleanup by the startup janitor and uses the failure cue.
                sound::play_cancel(&sound_cues_for_insert);
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod dictation_feedback_tests {
    use super::*;

    #[test]
    fn free_dictation_admission_still_owns_only_one_gesture() {
        let mut admission = DictationAdmission::default();
        assert!(!admission.finish());
        admission.press(); // no license, trial record, or token is consulted
        assert!(admission.finish());
        assert!(!admission.finish());
        admission.press();
        admission.reject(); // busy meeting, failed mic, or updater installation
        assert!(!admission.finish());
        admission.press();
        assert!(admission.finish());
    }

    #[test]
    fn dictation_always_auto_detects_and_snapshots_demo_policy() {
        let demo = OnboardingDemoState::default();
        demo.set(true);
        let first = demo.snapshot();
        demo.set(false);
        let second = demo.snapshot();
        assert!(first);
        assert!(!second);
        let options = dictation_stt_options(Some("Glossary: Murmur.".into()));
        assert_eq!(options.language, None);
        assert_eq!(options.initial_prompt.as_deref(), Some("Glossary: Murmur."));
        assert_eq!(options.speech_gate, crate::stt::SpeechGateMode::Off);
    }

    #[test]
    fn onboarding_event_payloads_are_correlated_and_content_free_for_keys() {
        let sessions = IndicatorSession::default();
        let first = sessions.begin();
        let second = sessions.begin();
        let activation = serde_json::to_value(ActivationPayload {
            attempt_id: first,
            mode: "hold",
        })
        .unwrap();
        let outcome = serde_json::to_value(AttemptOutcome {
            attempt_id: first,
            outcome: "cancelled",
        })
        .unwrap();
        assert_eq!(activation["attempt_id"], outcome["attempt_id"]);
        assert_ne!(first, second);
        let key = serde_json::to_value(BindingStatePayload { pressed: true }).unwrap();
        assert_eq!(key, serde_json::json!({ "pressed": true }));
        let transcript = crate::stt::Transcript {
            text: "test".into(),
            language: None,
            duration_ms: 10,
            confidence: None,
            app: None,
        };
        let payload = serde_json::to_value(CorrelatedTranscript {
            attempt_id: first,
            transcript: &transcript,
        })
        .unwrap();
        assert_eq!(payload["attempt_id"], first);
        assert_eq!(payload["text"], "test");
        assert_eq!(payload["duration_ms"], 10);
    }

    #[test]
    fn no_result_notification_distinguishes_outcome_without_naming_the_input() {
        let expected = "Check that your intended audio input is selected.";
        let no_audio = no_result_feedback(NoResultKind::NoAudio, Some("Private Device Name"));
        assert_eq!(no_audio.indicator_state, "no-audio");
        assert_eq!(no_audio.notification_title, "No audio detected");
        assert_eq!(no_audio.notification_body, expected);
        assert!(!no_audio.notification_body.contains("Private Device Name"));

        let no_speech = no_result_feedback(NoResultKind::NoSpeech, Some("Private Device Name"));
        assert_eq!(no_speech.indicator_state, "no-speech");
        assert_eq!(no_speech.notification_title, "No speech recognized");
        assert_eq!(no_speech.notification_body, expected);
        assert!(!no_speech.notification_body.contains("Private Device Name"));
    }

    #[test]
    fn dictation_wav_cleanup_requires_an_error_free_insertion_transaction() {
        assert!(should_delete_dictation_wav(&Ok(())));
        for error in [
            insertion::InsertError::NotTrusted,
            insertion::InsertError::TransactionUnavailable,
            insertion::InsertError::TargetUnavailable,
            insertion::InsertError::TargetChanged,
            insertion::InsertError::ClipboardSnapshotFailed,
            insertion::InsertError::ClipboardSnapshotTooLarge,
            insertion::InsertError::ClipboardChangedDuringSnapshot,
            insertion::InsertError::ClipboardOwnershipLostBeforePaste,
            insertion::InsertError::ClipboardWriteFailed,
            insertion::InsertError::SyntheticPasteFailed,
            insertion::InsertError::ClipboardRestoreFailed,
            insertion::InsertError::PrimaryAndRestoreFailed,
        ] {
            assert!(!should_delete_dictation_wav(&Err(error)));
        }
    }

    #[test]
    fn restore_failure_notifications_distinguish_post_event_from_primary_failure() {
        assert_eq!(
            insertion_failure_notification(insertion::InsertError::ClipboardRestoreFailed),
            (
                "Clipboard restore failed",
                "Murmur sent the paste command but couldn't restore the previous clipboard."
            )
        );
        assert_eq!(
            insertion_failure_notification(insertion::InsertError::PrimaryAndRestoreFailed),
            (
                "Clipboard restore failed",
                "Murmur couldn't complete the insertion and couldn't restore the previous clipboard."
            )
        );
        assert_eq!(
            insertion_failure_notification(insertion::InsertError::SyntheticPasteFailed).0,
            "Paste failed"
        );
    }

    #[test]
    fn newer_indicator_session_invalidates_older_work() {
        let sessions = IndicatorSession::default();
        let first = sessions.begin();
        assert!(sessions.is_current(first));
        let second = sessions.begin();
        assert!(!sessions.is_current(first));
        assert!(sessions.is_current(second));
    }

    #[test]
    fn result_pill_duration_is_exactly_three_and_a_half_seconds() {
        assert_eq!(DICTATION_RESULT_PILL_DURATION, Duration::from_millis(3500));
    }
}
