//! Native menu-bar surface for Murmur.
//!
//! The tray stays intentionally compact: app navigation, one-click meeting
//! recording, the latest dictated text and completed meeting transcript, microphone/output
//! selection, Settings, and Quit. Frontend localStorage remains the persisted
//! source of truth for device selection and the AI handoff prompt, so native
//! choices are mirrored to the always-running main webview through narrow events.

use std::sync::{Arc, Mutex};

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{image::Image, AppHandle, Manager, State};

use crate::audio::AudioHandle;
use crate::audio_devices::OutputDeviceState;
use crate::history::TranscriptStore;
use crate::native_language::{self, Language};
use crate::permissions::{self, PermissionState};

const RECORD_MEETING_ID: &str = "record-meeting";
const PAUSE_RESUME_MEETING_ID: &str = "pause-resume-meeting";
const STOP_MEETING_ID: &str = "stop-meeting";
const COPY_LAST_DICTATION_ID: &str = "copy-last-dictation";
const COPY_LAST_TRANSCRIPT_ID: &str = "copy-last-transcript";
const COPY_LAST_TRANSCRIPT_FOR_AI_ID: &str = "copy-last-transcript-for-ai";
const MICROPHONE_ID_PREFIX: &str = "tray-microphone-";
const OUTPUT_ID_PREFIX: &str = "tray-output-";
const TRAY_ICON_BYTES: &[u8] = include_bytes!("../icons/tray-icon-template@2x.png");

fn tray_icon_image() -> Result<Image<'static>, String> {
    Image::from_bytes(TRAY_ICON_BYTES).map_err(|error| format!("decode tray icon: {error}"))
}

type NativeCheckMenuItem = CheckMenuItem<tauri::Wry>;
type NativeMenu = Menu<tauri::Wry>;
type NativeMenuItem = MenuItem<tauri::Wry>;
type NativePredefinedMenuItem = PredefinedMenuItem<tauri::Wry>;
type NativeSubmenu = Submenu<tauri::Wry>;

#[derive(Clone)]
struct SelectionHandle {
    value: Option<String>,
    item: NativeCheckMenuItem,
}

struct TrayMenuHandles {
    root: NativeMenu,
    open_item: NativeMenuItem,
    settings_item: NativeMenuItem,
    quit_item: NativeMenuItem,
    record_meeting: NativeMenuItem,
    pause_resume_meeting: NativeMenuItem,
    stop_meeting: NativeMenuItem,
    meeting_active: bool,
    meeting_controls_enabled: bool,
    dictation_copy_item: NativeMenuItem,
    copy_items: [NativeMenuItem; 2],
    microphone_menu: NativeSubmenu,
    microphones: Vec<SelectionHandle>,
    output_menu: NativeSubmenu,
    outputs: Vec<SelectionHandle>,
}

#[derive(Default)]
struct TrayInner {
    icon: Option<TrayIcon>,
    menu: Option<TrayMenuHandles>,
    meeting_status: Option<String>,
}

/// Holds the live tray and menu-item handles. Keeping the microphone submenu
/// lets device hot-plug update its children without rebuilding the status item;
/// recreating the entire TrayIcon from CoreAudio's watcher can leave duplicate
/// NSStatusItems while macOS processes the old handle's asynchronous drop.
#[derive(Default)]
pub struct TrayState {
    inner: Mutex<TrayInner>,
}

fn separator(app: &AppHandle) -> Result<NativePredefinedMenuItem, String> {
    PredefinedMenuItem::separator(app).map_err(|error| format!("separator: {error}"))
}

fn populate_microphone_submenu(
    app: &AppHandle,
    submenu: &NativeSubmenu,
    selected: Option<String>,
) -> Result<Vec<SelectionHandle>, String> {
    let automatic = CheckMenuItem::with_id(
        app,
        format!("{MICROPHONE_ID_PREFIX}0"),
        native_language::tray(native_language::current(), "automatic"),
        true,
        selected.is_none(),
        None::<&str>,
    )
    .map_err(|error| format!("automatic microphone item: {error}"))?;
    submenu
        .append(&automatic)
        .map_err(|error| format!("append automatic microphone: {error}"))?;

    let mut handles = vec![SelectionHandle {
        value: None,
        item: automatic,
    }];
    let devices = crate::audio_devices::list_input_devices();
    if devices.is_empty() {
        let label = if permissions::current().microphone == PermissionState::Granted {
            native_language::tray(native_language::current(), "no_microphones")
        } else {
            native_language::tray(native_language::current(), "microphone_access")
        };
        let divider = separator(app)?;
        let unavailable = MenuItem::with_id(
            app,
            "tray-microphone-unavailable",
            label,
            false,
            None::<&str>,
        )
        .map_err(|error| format!("microphone unavailable item: {error}"))?;
        submenu
            .append(&divider)
            .and_then(|_| submenu.append(&unavailable))
            .map_err(|error| format!("append unavailable microphone state: {error}"))?;
        return Ok(handles);
    }

    let divider = separator(app)?;
    submenu
        .append(&divider)
        .map_err(|error| format!("append microphone separator: {error}"))?;
    for (index, device) in devices.into_iter().enumerate() {
        let value = Some(device.name);
        let item = CheckMenuItem::with_id(
            app,
            format!("{MICROPHONE_ID_PREFIX}{}", index + 1),
            value.as_deref().unwrap_or_default(),
            true,
            selected == value,
            None::<&str>,
        )
        .map_err(|error| format!("microphone item: {error}"))?;
        submenu
            .append(&item)
            .map_err(|error| format!("append microphone item: {error}"))?;
        handles.push(SelectionHandle { value, item });
    }
    Ok(handles)
}

fn build_microphone_submenu(
    app: &AppHandle,
    selected: Option<String>,
) -> Result<(NativeSubmenu, Vec<SelectionHandle>), String> {
    let submenu = Submenu::new(
        app,
        native_language::tray(native_language::current(), "microphone"),
        true,
    )
    .map_err(|error| format!("microphone submenu: {error}"))?;
    let handles = populate_microphone_submenu(app, &submenu, selected)?;
    Ok((submenu, handles))
}

fn populate_output_submenu(
    app: &AppHandle,
    submenu: &NativeSubmenu,
    selected: Option<String>,
) -> Result<Vec<SelectionHandle>, String> {
    let automatic = CheckMenuItem::with_id(
        app,
        format!("{OUTPUT_ID_PREFIX}0"),
        native_language::tray(native_language::current(), "automatic"),
        true,
        selected.is_none(),
        None::<&str>,
    )
    .map_err(|error| format!("automatic output item: {error}"))?;
    submenu
        .append(&automatic)
        .map_err(|error| format!("append automatic output: {error}"))?;
    let mut handles = vec![SelectionHandle {
        value: None,
        item: automatic,
    }];

    let devices = crate::audio_devices::list_output_devices();
    if devices.is_empty() {
        let divider = separator(app)?;
        let unavailable = MenuItem::with_id(
            app,
            "tray-output-unavailable",
            native_language::tray(native_language::current(), "no_outputs"),
            false,
            None::<&str>,
        )
        .map_err(|error| format!("output unavailable item: {error}"))?;
        submenu
            .append(&divider)
            .and_then(|_| submenu.append(&unavailable))
            .map_err(|error| format!("append unavailable output state: {error}"))?;
        return Ok(handles);
    }

    submenu
        .append(&separator(app)?)
        .map_err(|error| format!("append output separator: {error}"))?;
    for (index, device) in devices.into_iter().enumerate() {
        let value = Some(device.name);
        let item = CheckMenuItem::with_id(
            app,
            format!("{OUTPUT_ID_PREFIX}{}", index + 1),
            value.as_deref().unwrap_or_default(),
            true,
            selected == value,
            None::<&str>,
        )
        .map_err(|error| format!("output item: {error}"))?;
        submenu
            .append(&item)
            .map_err(|error| format!("append output item: {error}"))?;
        handles.push(SelectionHandle { value, item });
    }
    Ok(handles)
}

fn build_output_submenu(
    app: &AppHandle,
    selected: Option<String>,
) -> Result<(NativeSubmenu, Vec<SelectionHandle>), String> {
    let submenu = Submenu::new(
        app,
        native_language::tray(native_language::current(), "output"),
        true,
    )
    .map_err(|error| format!("output submenu: {error}"))?;
    let handles = populate_output_submenu(app, &submenu, selected)?;
    Ok((submenu, handles))
}

fn meeting_controls_available(requested: bool) -> bool {
    #[cfg(feature = "meeting-capture")]
    {
        requested
    }
    #[cfg(not(feature = "meeting-capture"))]
    {
        let _ = requested;
        false
    }
}

fn meeting_status_is_active(status: Option<&str>) -> bool {
    matches!(
        status,
        Some("starting" | "recording" | "pausing" | "paused" | "resuming" | "stopping")
    )
}

fn meeting_status_accepts_actions(status: Option<&str>) -> bool {
    matches!(status, Some("recording" | "paused"))
}

fn apply_meeting_menu_state(
    handles: &mut TrayMenuHandles,
    status: Option<&str>,
) -> Result<(), String> {
    if !handles.meeting_controls_enabled {
        return Ok(());
    }
    let active = meeting_status_is_active(status);
    if active != handles.meeting_active {
        if active {
            handles
                .root
                .remove(&handles.record_meeting)
                .map_err(|error| format!("remove record meeting item: {error}"))?;
            handles
                .root
                .insert(&handles.pause_resume_meeting, 1)
                .map_err(|error| format!("insert pause meeting item: {error}"))?;
            handles
                .root
                .insert(&handles.stop_meeting, 2)
                .map_err(|error| format!("insert stop meeting item: {error}"))?;
        } else {
            handles
                .root
                .remove(&handles.pause_resume_meeting)
                .map_err(|error| format!("remove pause meeting item: {error}"))?;
            handles
                .root
                .remove(&handles.stop_meeting)
                .map_err(|error| format!("remove stop meeting item: {error}"))?;
            handles
                .root
                .insert(&handles.record_meeting, 1)
                .map_err(|error| format!("insert record meeting item: {error}"))?;
        }
        handles.meeting_active = active;
    }

    handles
        .pause_resume_meeting
        .set_text(native_language::tray(
            native_language::current(),
            if status == Some("paused") {
                "resume"
            } else {
                "pause"
            },
        ))
        .map_err(|error| format!("update pause/resume meeting label: {error}"))?;
    let actions_enabled = meeting_status_accepts_actions(status);
    handles
        .pause_resume_meeting
        .set_enabled(actions_enabled)
        .map_err(|error| format!("update pause/resume meeting state: {error}"))?;
    handles
        .stop_meeting
        .set_enabled(actions_enabled)
        .map_err(|error| format!("update stop meeting state: {error}"))
}

/// Install the permanent menu-bar icon at native startup. Submenus are
/// refreshed in place rather than recreating the status item.
pub fn install(app: &AppHandle, meeting_controls_enabled: bool) -> Result<(), String> {
    let state: State<TrayState> = app.state();
    let meeting_status = {
        let mut inner = state.inner.lock().unwrap();
        inner.menu = None;
        inner.icon = None;
        inner.meeting_status.clone()
    };

    let has_dictation = app
        .state::<Arc<crate::history::TranscriptStore>>()
        .latest_dictation_text()
        .map(|text| text.is_some())
        .unwrap_or(false);
    let has_transcript = app
        .state::<Arc<TranscriptStore>>()
        .latest_completed_ai_handoff_context()
        .map(|context| context.is_some())
        .unwrap_or(false);
    let selected_microphone = app.state::<AudioHandle>().preferred_device();
    let selected_output = app.state::<OutputDeviceState>().preferred();

    let language = native_language::current();
    let open_item = MenuItem::with_id(
        app,
        "open-murmur",
        native_language::tray(language, "open"),
        true,
        None::<&str>,
    )
    .map_err(|error| format!("open menu item: {error}"))?;
    let record_meeting_item = MenuItem::with_id(
        app,
        RECORD_MEETING_ID,
        native_language::tray(language, "record"),
        true,
        None::<&str>,
    )
    .map_err(|error| format!("record meeting item: {error}"))?;
    let pause_resume_meeting_item = MenuItem::with_id(
        app,
        PAUSE_RESUME_MEETING_ID,
        native_language::tray(
            language,
            if meeting_status.as_deref() == Some("paused") {
                "resume"
            } else {
                "pause"
            },
        ),
        meeting_status_accepts_actions(meeting_status.as_deref()),
        None::<&str>,
    )
    .map_err(|error| format!("pause/resume meeting item: {error}"))?;
    let stop_meeting_item = MenuItem::with_id(
        app,
        STOP_MEETING_ID,
        native_language::tray(language, "stop"),
        meeting_status_accepts_actions(meeting_status.as_deref()),
        None::<&str>,
    )
    .map_err(|error| format!("stop meeting item: {error}"))?;
    let copy_dictation_item = MenuItem::with_id(
        app,
        COPY_LAST_DICTATION_ID,
        native_language::tray(language, "copy_dictation"),
        has_dictation,
        None::<&str>,
    )
    .map_err(|error| format!("copy dictation item: {error}"))?;
    let copy_item = MenuItem::with_id(
        app,
        COPY_LAST_TRANSCRIPT_ID,
        native_language::tray(language, "copy_transcript"),
        has_transcript,
        None::<&str>,
    )
    .map_err(|error| format!("copy transcript item: {error}"))?;
    let copy_for_ai_item = MenuItem::with_id(
        app,
        COPY_LAST_TRANSCRIPT_FOR_AI_ID,
        native_language::tray(language, "copy_for_ai"),
        has_transcript,
        None::<&str>,
    )
    .map_err(|error| format!("copy transcript for AI item: {error}"))?;
    let (microphone_menu, microphones) = build_microphone_submenu(app, selected_microphone)?;
    let (output_menu, outputs) = build_output_submenu(app, selected_output)?;
    let settings_item = MenuItem::with_id(
        app,
        "open-settings",
        native_language::tray(language, "settings"),
        true,
        None::<&str>,
    )
    .map_err(|error| format!("settings menu item: {error}"))?;
    let quit_item = MenuItem::with_id(
        app,
        "quit-murmur",
        native_language::tray(language, "quit"),
        true,
        None::<&str>,
    )
    .map_err(|error| format!("quit menu item: {error}"))?;

    let menu = Menu::new(app).map_err(|error| format!("menu: {error}"))?;
    menu.append(&open_item)
        .map_err(|error| format!("append open item: {error}"))?;
    let meeting_active = meeting_status_is_active(meeting_status.as_deref());
    if meeting_controls_available(meeting_controls_enabled) {
        if meeting_active {
            menu.append(&pause_resume_meeting_item)
                .and_then(|_| menu.append(&stop_meeting_item))
                .map_err(|error| format!("append active meeting controls: {error}"))?;
        } else {
            menu.append(&record_meeting_item)
                .map_err(|error| format!("append record meeting item: {error}"))?;
        }
    }
    for item in [
        &separator(app)? as &dyn tauri::menu::IsMenuItem<_>,
        &copy_dictation_item,
        &copy_item,
        &copy_for_ai_item,
        &separator(app)?,
        &microphone_menu,
    ] {
        menu.append(item)
            .map_err(|error| format!("append tray item: {error}"))?;
    }
    if meeting_controls_available(meeting_controls_enabled) {
        menu.append(&output_menu)
            .map_err(|error| format!("append output menu: {error}"))?;
    }
    for item in [
        &separator(app)? as &dyn tauri::menu::IsMenuItem<_>,
        &settings_item,
        &separator(app)?,
        &quit_item,
    ] {
        menu.append(item)
            .map_err(|error| format!("append tray item: {error}"))?;
    }

    let tray_builder = TrayIconBuilder::with_id("murmur-main")
        .tooltip("Murmur")
        .icon(tray_icon_image()?);
    #[cfg(target_os = "macos")]
    let tray_builder = tray_builder.icon_as_template(true);
    let icon = tray_builder
        .menu(&menu)
        .on_menu_event(handle_menu_event)
        .build(app)
        .map_err(|error| format!("tray build: {error}"))?;

    let mut inner = state.inner.lock().unwrap();
    let mut handles = TrayMenuHandles {
        root: menu,
        open_item,
        settings_item,
        quit_item,
        record_meeting: record_meeting_item,
        pause_resume_meeting: pause_resume_meeting_item,
        stop_meeting: stop_meeting_item,
        meeting_active,
        meeting_controls_enabled: meeting_controls_available(meeting_controls_enabled),
        dictation_copy_item: copy_dictation_item,
        copy_items: [copy_item, copy_for_ai_item],
        microphone_menu,
        microphones,
        output_menu,
        outputs,
    };
    // Status can change while the native menu is being constructed. Reconcile
    // against the latest retained value before publishing the handles so an
    // install/toggle race cannot leave stale meeting actions indefinitely.
    apply_meeting_menu_state(&mut handles, inner.meeting_status.as_deref())?;
    inner.menu = Some(handles);
    inner.icon = Some(icon);
    Ok(())
}

fn show_and_focus_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn selected_microphone_value(app: &AppHandle, id: &str) -> Option<Option<String>> {
    let state: State<TrayState> = app.state();
    let inner = state.inner.lock().unwrap();
    let handles = inner.menu.as_ref()?;
    handles
        .microphones
        .iter()
        .find(|choice| choice.item.id().as_ref() == id)
        .map(|choice| choice.value.clone())
}

fn selected_output_value(app: &AppHandle, id: &str) -> Option<Option<String>> {
    let state: State<TrayState> = app.state();
    let inner = state.inner.lock().unwrap();
    let handles = inner.menu.as_ref()?;
    handles
        .outputs
        .iter()
        .find(|choice| choice.item.id().as_ref() == id)
        .map(|choice| choice.value.clone())
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id.as_ref();
    match id {
        "open-murmur" => show_and_focus_main(app),
        RECORD_MEETING_ID => {
            crate::window_events::emit_main(app, "murmur://meeting-start-requested", ());
        }
        PAUSE_RESUME_MEETING_ID => {
            crate::window_events::emit_main(
                app,
                "murmur://tray-meeting-pause-resume-requested",
                (),
            );
        }
        STOP_MEETING_ID => {
            crate::window_events::emit_main(app, "murmur://tray-meeting-stop-requested", ());
        }
        COPY_LAST_DICTATION_ID => copy_last_dictation(app),
        COPY_LAST_TRANSCRIPT_ID => {
            crate::window_events::emit_main(app, "murmur://tray-copy-requested", "transcript");
        }
        COPY_LAST_TRANSCRIPT_FOR_AI_ID => {
            crate::window_events::emit_main(app, "murmur://tray-copy-requested", "for_ai");
        }
        "open-settings" => {
            show_and_focus_main(app);
            crate::window_events::emit_main(app, "murmur://tray-open-settings", ());
        }
        "quit-murmur" => app.exit(0),
        _ if id.starts_with(MICROPHONE_ID_PREFIX) => {
            if let Some(selection) = selected_microphone_value(app, id) {
                app.state::<AudioHandle>()
                    .set_preferred_device(selection.clone());
                sync_microphone_selection(app, selection.clone());
                crate::window_events::emit_main(
                    app,
                    "murmur://tray-microphone-selected",
                    selection,
                );
            }
        }
        _ if id.starts_with(OUTPUT_ID_PREFIX) => {
            if let Some(selection) = selected_output_value(app, id) {
                app.state::<OutputDeviceState>()
                    .set_preferred(selection.clone());
                sync_output_selection(app, selection.clone());
                crate::window_events::emit_main(app, "murmur://tray-output-selected", selection);
            }
        }
        _ => {}
    }
}

fn sync_microphone_selection_items(app: &AppHandle, selection: Option<String>) {
    let items = {
        let state: State<TrayState> = app.state();
        let inner = state.inner.lock().unwrap();
        let Some(menu) = inner.menu.as_ref() else {
            return;
        };
        menu.microphones.clone()
    };
    for choice in items {
        let _ = choice.item.set_checked(choice.value == selection);
    }
}

pub fn sync_microphone_selection(app: &AppHandle, selection: Option<String>) {
    sync_microphone_selection_items(app, selection);
}

pub fn sync_output_selection(app: &AppHandle, selection: Option<String>) {
    let items = {
        let state: State<TrayState> = app.state();
        let inner = state.inner.lock().unwrap();
        let Some(menu) = inner.menu.as_ref() else {
            return;
        };
        menu.outputs.clone()
    };
    for choice in items {
        let _ = choice.item.set_checked(choice.value == selection);
    }
}

/// Keep the menu item in sync with retained dictations without rebuilding the tray icon.
pub fn refresh_dictation_copy_availability(app: &AppHandle, store: &TranscriptStore) {
    let enabled = store
        .latest_dictation_text()
        .map(|text| text.is_some())
        .unwrap_or(false);
    let item = {
        let state: State<TrayState> = app.state();
        let inner = state.inner.lock().unwrap();
        inner
            .menu
            .as_ref()
            .map(|menu| menu.dictation_copy_item.clone())
    };
    if let Some(item) = item {
        let _ = item.set_enabled(enabled);
    }
}

pub fn refresh_copy_availability(app: &AppHandle, store: &TranscriptStore) {
    let enabled = store
        .latest_completed_ai_handoff_context()
        .map(|context| context.is_some())
        .unwrap_or(false);
    let items = {
        let state: State<TrayState> = app.state();
        let inner = state.inner.lock().unwrap();
        inner.menu.as_ref().map(|menu| menu.copy_items.clone())
    };
    if let Some(items) = items {
        for item in items {
            let _ = item.set_enabled(enabled);
        }
    }
}

/// Refresh the microphone submenu in place after a CoreAudio device change.
/// Do not reinstall the TrayIcon here: this runs on the device-watcher thread,
/// and macOS may not remove the old NSStatusItem before Tauri creates the new
/// one, leaving two persistent Murmur icons after a hot-plug event.
pub fn refresh_microphones(app: &AppHandle) {
    let submenu = {
        let state: State<TrayState> = app.state();
        let mut inner = state.inner.lock().unwrap();
        inner.menu.as_mut().map(|menu| {
            // Drop the old wrappers before reusing their stable menu IDs.
            menu.microphones.clear();
            menu.microphone_menu.clone()
        })
    };
    let Some(submenu) = submenu else {
        return;
    };

    loop {
        match submenu.remove_at(0) {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                crate::murmur_dev_log!("[murmur] microphone menu clear failed: {error}");
                return;
            }
        }
    }

    let selected = app.state::<AudioHandle>().preferred_device();
    match populate_microphone_submenu(app, &submenu, selected) {
        Ok(microphones) => {
            let state: State<TrayState> = app.state();
            let mut inner = state.inner.lock().unwrap();
            if let Some(menu) = inner.menu.as_mut() {
                menu.microphones = microphones;
            }
        }
        Err(error) => crate::murmur_dev_log!("[murmur] microphone menu refresh failed: {error}"),
    }
}

/// Refresh the Output submenu after a CoreAudio route/device change.
pub fn refresh_outputs(app: &AppHandle) {
    let submenu = {
        let state: State<TrayState> = app.state();
        let mut inner = state.inner.lock().unwrap();
        inner.menu.as_mut().map(|menu| {
            menu.outputs.clear();
            menu.output_menu.clone()
        })
    };
    let Some(submenu) = submenu else {
        return;
    };
    loop {
        match submenu.remove_at(0) {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                crate::murmur_dev_log!("[murmur] output menu clear failed: {error}");
                return;
            }
        }
    }

    let selected = app.state::<OutputDeviceState>().preferred();
    match populate_output_submenu(app, &submenu, selected) {
        Ok(outputs) => {
            let state: State<TrayState> = app.state();
            let mut inner = state.inner.lock().unwrap();
            if let Some(menu) = inner.menu.as_mut() {
                menu.outputs = outputs;
            }
        }
        Err(error) => crate::murmur_dev_log!("[murmur] output menu refresh failed: {error}"),
    }
}

#[tauri::command]
pub fn show_main_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    // Even if minimized-state inspection fails, still attempt to show/focus.
    if window.is_minimized().unwrap_or(false) {
        let _ = window.unminimize();
    }
    window
        .show()
        .map_err(|error| format!("show main window: {error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("focus main window: {error}"))
}

/// Sync the persisted interface preference after the main webview hydrates.
/// Update existing handles instead of reinstalling the macOS status item.
#[tauri::command]
pub fn set_app_language(app: AppHandle, language: String) -> Result<(), String> {
    let language = Language::parse(&language).ok_or("unsupported app language")?;
    if native_language::current() == language {
        return Ok(());
    }
    native_language::set(language);
    {
        let state: State<TrayState> = app.state();
        let mut inner = state.inner.lock().unwrap();
        let status = inner.meeting_status.clone();
        if let Some(menu) = inner.menu.as_mut() {
            for (item, key) in [
                (&menu.open_item, "open"),
                (&menu.settings_item, "settings"),
                (&menu.quit_item, "quit"),
                (&menu.record_meeting, "record"),
                (&menu.stop_meeting, "stop"),
                (&menu.dictation_copy_item, "copy_dictation"),
                (&menu.copy_items[0], "copy_transcript"),
                (&menu.copy_items[1], "copy_for_ai"),
            ] {
                item.set_text(native_language::tray(language, key))
                    .map_err(|e| e.to_string())?;
            }
            menu.microphone_menu
                .set_text(native_language::tray(language, "microphone"))
                .map_err(|e| e.to_string())?;
            menu.output_menu
                .set_text(native_language::tray(language, "output"))
                .map_err(|e| e.to_string())?;
            apply_meeting_menu_state(menu, status.as_deref())?;
        }
    }
    refresh_microphones(&app);
    refresh_outputs(&app);
    Ok(())
}

#[tauri::command]
pub fn set_tray_meeting_state(app: AppHandle, status: Option<String>) -> Result<(), String> {
    let state: State<TrayState> = app.state();
    let mut inner = state.inner.lock().unwrap();
    inner.meeting_status.clone_from(&status);
    if let Some(menu) = inner.menu.as_mut() {
        apply_meeting_menu_state(menu, status.as_deref())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn tray_read_latest_ai_handoff_context(
    store: State<'_, Arc<TranscriptStore>>,
) -> Result<Option<crate::captures::MeetingAiHandoffContext>, String> {
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || store.latest_completed_ai_handoff_context())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn tray_read_latest_transcript(
    store: State<'_, Arc<TranscriptStore>>,
) -> Result<Option<String>, String> {
    let store = Arc::clone(store.inner());
    tauri::async_runtime::spawn_blocking(move || {
        store
            .latest_completed_ai_handoff_context()
            .map(|context| context.map(|context| context.transcript))
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

/// Explicit copy only: never change focus or synthesize a paste into another app.
fn copy_last_dictation(app: &AppHandle) {
    let result = app
        .state::<Arc<crate::history::TranscriptStore>>()
        .latest_dictation_text()
        .map_err(|_| "Dictation history could not be read.")
        .and_then(|text| text.ok_or("No saved dictation is available."))
        .and_then(|text| tray_copy_text(text).map_err(|_| "Clipboard could not be updated."));
    if let Err(message) = result {
        crate::notify::error(app, "Copy failed", message);
    }
}

#[tauri::command]
pub fn tray_copy_text(text: String) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
    clipboard.set_text(text).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_tray_icon_is_a_retina_template_image() {
        let icon = tray_icon_image().expect("embedded tray icon should decode");
        assert_eq!((icon.width(), icon.height()), (44, 44));

        let mut alpha = icon.rgba().chunks_exact(4).map(|pixel| pixel[3]);
        assert!(alpha.any(|value| value == 0));
        assert!(alpha.any(|value| value > 0));
        for (index, pixel) in icon.rgba().chunks_exact(4).enumerate() {
            if pixel[3] > 0 {
                assert_eq!(&pixel[..3], &[0, 0, 0], "template artwork must be black");
                let (x, y) = (index % 44, index / 44);
                assert!(
                    (4..40).contains(&x) && (4..40).contains(&y),
                    "artwork must retain its 4px safe margin"
                );
            }
        }
    }

    #[test]
    fn meeting_item_requires_frontend_request_and_production_capture_support() {
        assert!(!meeting_controls_available(false));
        #[cfg(feature = "meeting-capture")]
        assert!(meeting_controls_available(true));
        #[cfg(not(feature = "meeting-capture"))]
        assert!(!meeting_controls_available(true));
    }

    #[test]
    fn meeting_status_controls_active_menu_and_actions() {
        assert!(!meeting_status_is_active(None));
        assert!(meeting_status_is_active(Some("recording")));
        assert!(meeting_status_is_active(Some("paused")));
        assert!(meeting_status_is_active(Some("stopping")));
        assert!(!meeting_status_is_active(Some("transcribing")));

        assert!(meeting_status_accepts_actions(Some("recording")));
        assert!(meeting_status_accepts_actions(Some("paused")));
        assert!(!meeting_status_accepts_actions(Some("pausing")));
        assert!(!meeting_status_accepts_actions(None));
    }
}
