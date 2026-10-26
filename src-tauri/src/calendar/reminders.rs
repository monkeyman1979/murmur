//! Opt-in reminder scheduling. Timers may offer a prompt, never start audio.
//! A main-window acknowledgment is required after the opaque occurrence key
//! has been saved locally. The reminder webview has no recorder authority.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use super::{CalendarMeeting, CalendarPermission, CalendarSnapshot, MeetingProvider};

pub const WINDOW: &str = "meeting-reminder";
const REFRESH_MS: u64 = 15_000;
const GRACE_MS: i64 = 120_000;
const DAY_MS: i64 = 86_400_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandledReminder {
    pub key: String,
    pub expires_ms: i64,
}

#[derive(Clone, Serialize)]
pub struct DueReminder {
    pub token: String,
    pub key: String,
    pub expires_ms: i64,
}

#[derive(Clone, Serialize)]
pub struct ReminderView {
    pub token: String,
    pub title: String,
    pub start_ms: i64,
    pub provider: MeetingProvider,
}

struct Pending {
    meeting: CalendarMeeting,
    due: DueReminder,
    acknowledged: bool,
}

#[derive(Default)]
struct Engine {
    enabled: bool,
    account_ids: Vec<String>,
    lead_ms: i64,
    handled: BTreeMap<String, i64>,
    pending: Option<Pending>,
    revision: u64,
    armed_at: i64,
}

#[derive(Default)]
pub struct CalendarReminderState {
    engine: Mutex<Engine>,
    changed: tokio::sync::Notify,
}

fn occurrence_key(meeting: &CalendarMeeting) -> String {
    format!("{:x}", Sha256::digest(meeting.id.as_bytes()))
}

fn in_window(meeting: &CalendarMeeting, now: i64, armed_at: i64, lead_ms: i64) -> bool {
    meeting.start_ms >= armed_at
        && now >= meeting.start_ms.saturating_sub(lead_ms)
        && now
            < meeting
                .end_ms
                .min(meeting.start_ms.saturating_add(GRACE_MS))
}

impl Engine {
    /// Refresh sync/cancellations regularly, but wake at a known meeting's
    /// exact reminder deadline instead of rounding to the next polling tick.
    fn next_delay(&self, snapshot: &CalendarSnapshot, now: i64) -> Duration {
        let next = snapshot
            .meetings
            .iter()
            .filter(|meeting| {
                meeting.start_ms >= self.armed_at
                    && self.account_ids.contains(&meeting.account_id)
                    && !self.handled.contains_key(&occurrence_key(meeting))
            })
            .map(|meeting| {
                meeting
                    .start_ms
                    .saturating_sub(self.lead_ms)
                    .saturating_sub(now)
            })
            .filter(|delay| *delay > 0)
            .min()
            .unwrap_or(REFRESH_MS as i64);
        Duration::from_millis((next as u64).min(REFRESH_MS))
    }

    fn view(&self) -> Option<ReminderView> {
        self.pending
            .as_ref()
            .filter(|p| p.acknowledged)
            .map(|p| ReminderView {
                token: p.due.token.clone(),
                title: p.meeting.title.clone(),
                start_ms: p.meeting.start_ms,
                provider: p.meeting.provider,
            })
    }

    fn update(
        &mut self,
        snapshot: CalendarSnapshot,
        now: i64,
        recording: bool,
    ) -> Option<DueReminder> {
        self.handled.retain(|_, expires| *expires > now);
        if !self.enabled || recording {
            self.pending = None;
            return None;
        }
        if let Some(pending) = self.pending.as_mut() {
            let updated = snapshot.meetings.iter().find(|meeting| {
                occurrence_key(meeting) == pending.due.key
                    && self.account_ids.contains(&meeting.account_id)
                    && in_window(meeting, now, self.armed_at, self.lead_ms)
            });
            if let Some(meeting) = updated {
                pending.meeting = meeting.clone();
                return (!pending.acknowledged).then(|| pending.due.clone());
            }
            self.pending = None;
        }
        let meeting = snapshot
            .meetings
            .into_iter()
            .filter(|meeting| {
                self.account_ids.contains(&meeting.account_id)
                    && in_window(meeting, now, self.armed_at, self.lead_ms)
                    && !self.handled.contains_key(&occurrence_key(meeting))
            })
            .min_by_key(|meeting| meeting.start_ms)?;
        let mut random = [0u8; 16];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut random);
        let due = DueReminder {
            token: random.iter().map(|byte| format!("{byte:02x}")).collect(),
            key: occurrence_key(&meeting),
            expires_ms: meeting.start_ms.saturating_add(DAY_MS),
        };
        self.pending = Some(Pending {
            meeting,
            due: due.clone(),
            acknowledged: false,
        });
        Some(due)
    }
}

fn recording(app: &AppHandle) -> bool {
    app.state::<Arc<crate::meeting_capture::AudioCaptureArbiter>>()
        .meeting_capture_active()
}

fn publish(app: &AppHandle, view: Option<ReminderView>) {
    crate::window_events::emit_reminder(app, "murmur://calendar-reminder", view.clone());
    if view.is_none() {
        if let Some(window) = app.get_webview_window(WINDOW) {
            let _ = window.hide();
        }
    }
}

#[tauri::command]
pub fn configure_calendar_reminders(
    app: AppHandle,
    state: tauri::State<'_, Arc<CalendarReminderState>>,
    enabled: bool,
    account_ids: Vec<String>,
    handled: Vec<HandledReminder>,
    lead_seconds: u32,
) -> Result<(), String> {
    let now = crate::history::now_millis();
    if ![0, 15, 30, 60].contains(&lead_seconds)
        || account_ids.len() > 2048
        || account_ids.iter().any(|id| id.len() > 4096)
        || handled.len() > 1024
        || handled.iter().any(|item| {
            item.key.len() != 64
                || !item.key.bytes().all(|b| b.is_ascii_hexdigit())
                || item.expires_ms > now.saturating_add(2 * DAY_MS)
        })
    {
        return Err("Invalid calendar reminder preferences.".into());
    }
    if enabled && super::calendar_permission() != CalendarPermission::Granted {
        return Err("Calendar access is required for reminders.".into());
    }
    let mut engine = state.engine.lock();
    let lead_ms = i64::from(lead_seconds) * 1000;
    if engine.enabled != enabled || engine.account_ids != account_ids || engine.lead_ms != lead_ms {
        engine.revision += 1;
        engine.pending = None;
        if enabled && !engine.enabled {
            engine.armed_at = now;
        }
        engine.enabled = enabled;
        engine.account_ids = account_ids;
        engine.lead_ms = lead_ms;
        publish(&app, None);
    }
    // Updates may race a durable acknowledgment. Never replace the in-memory
    // set with an older frontend snapshot and accidentally re-offer a meeting.
    for item in handled.into_iter().filter(|item| item.expires_ms > now) {
        engine.handled.insert(item.key, item.expires_ms);
    }
    engine.handled.retain(|_, expires| *expires > now);
    drop(engine);
    state.changed.notify_one();
    Ok(())
}

#[tauri::command]
pub fn present_calendar_reminder(
    app: AppHandle,
    state: tauri::State<'_, Arc<CalendarReminderState>>,
    token: String,
) -> Result<(), String> {
    let mut engine = state.engine.lock();
    let now = crate::history::now_millis();
    let pending = engine
        .pending
        .as_ref()
        .ok_or("Reminder is no longer available.")?;
    if !engine.enabled
        || recording(&app)
        || super::calendar_permission() != CalendarPermission::Granted
        || pending.due.token != token
        || !in_window(&pending.meeting, now, engine.armed_at, engine.lead_ms)
        || !engine.account_ids.contains(&pending.meeting.account_id)
    {
        return Err("Reminder is no longer available.".into());
    }
    let key = pending.due.key.clone();
    let expires = pending.due.expires_ms;
    engine.handled.insert(key, expires);
    engine.pending.as_mut().unwrap().acknowledged = true;
    let view = engine.view();
    let window = app
        .get_webview_window(WINDOW)
        .ok_or("Reminder window is unavailable.")?;
    publish(&app, view);
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    {
        // Use the recording controller's native coordinate space and target
        // display, avoiding mixed-scale primary-monitor conversion errors.
        crate::meeting_capture::place_meeting_window(&app, &window);
        crate::meeting_capture::show_meeting_window_without_focus(&window)
            .map_err(|_| "Reminder could not be shown.".to_string())
    }
    #[cfg(not(all(target_os = "macos", feature = "meeting-capture")))]
    window
        .show()
        .map_err(|_| "Reminder could not be shown.".to_string())
}

#[tauri::command]
pub fn calendar_reminder_current(
    state: tauri::State<'_, Arc<CalendarReminderState>>,
) -> Option<ReminderView> {
    state.engine.lock().view()
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderAction {
    StartRecording,
    Dismiss,
}

#[tauri::command]
pub fn calendar_reminder_action(
    app: AppHandle,
    state: tauri::State<'_, Arc<CalendarReminderState>>,
    token: String,
    action: ReminderAction,
) -> Result<(), String> {
    let mut engine = state.engine.lock();
    let pending = engine.pending.as_ref().ok_or("Reminder has expired.")?;
    if pending.due.token != token || !pending.acknowledged {
        return Err("Reminder has expired.".into());
    }
    let can_start = engine.enabled
        && !recording(&app)
        && super::calendar_permission() == CalendarPermission::Granted
        && engine.account_ids.contains(&pending.meeting.account_id)
        && in_window(
            &pending.meeting,
            crate::history::now_millis(),
            engine.armed_at,
            engine.lead_ms,
        );
    engine.pending = None;
    publish(&app, None);
    drop(engine);
    if matches!(action, ReminderAction::StartRecording) {
        if !can_start {
            return Err("Meeting recording is not available for this reminder.".into());
        }
        // The main window alone owns recording admission, onboarding consent,
        // microphone/model/disk checks and the existing missing-prerequisite UI.
        crate::window_events::emit_main(&app, "murmur://meeting-start-requested", ());
    }
    state.changed.notify_one();
    Ok(())
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Arc<CalendarReminderState>>().inner().clone();
        loop {
            let settings = {
                let engine = state.engine.lock();
                (engine.enabled && !engine.account_ids.is_empty())
                    .then(|| (engine.revision, engine.account_ids.clone()))
            };
            let Some((revision, accounts)) = settings else {
                state.changed.notified().await;
                continue;
            };
            #[cfg(target_os = "macos")]
            let result = tauri::async_runtime::spawn_blocking(move || {
                super::native::read_snapshot(&accounts)
            })
            .await;
            #[cfg(not(target_os = "macos"))]
            let result: Result<Result<CalendarSnapshot, String>, ()> = {
                let _ = accounts;
                Err(())
            };
            let mut delay = Duration::from_millis(REFRESH_MS);
            {
                let mut engine = state.engine.lock();
                if engine.revision == revision {
                    match result {
                        Ok(Ok(snapshot)) => {
                            let now = crate::history::now_millis();
                            delay = engine.next_delay(&snapshot, now);
                            let due = engine.update(snapshot, now, recording(&app));
                            publish(&app, engine.view());
                            if let Some(due) = due {
                                crate::window_events::emit_main(
                                    &app,
                                    "murmur://calendar-reminder-due",
                                    due,
                                );
                            }
                        }
                        _ => {
                            engine.pending = None;
                            publish(&app, None);
                            crate::window_events::emit_main(
                                &app,
                                "murmur://calendar-reminder-error",
                                (),
                            );
                        }
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                _ = state.changed.notified() => {},
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn meeting(id: &str, start: i64) -> CalendarMeeting {
        CalendarMeeting {
            id: id.into(),
            account_id: "account".into(),
            calendar_id: "calendar".into(),
            title: "Private meeting".into(),
            start_ms: start,
            end_ms: start + 600_000,
            provider: MeetingProvider::GoogleMeet,
        }
    }
    fn engine() -> Engine {
        Engine {
            enabled: true,
            account_ids: vec!["account".into()],
            armed_at: 1,
            lead_ms: 60_000,
            ..Default::default()
        }
    }
    fn snapshot(meetings: Vec<CalendarMeeting>) -> CalendarSnapshot {
        CalendarSnapshot {
            accounts: vec![],
            meetings,
        }
    }
    #[test]
    fn offers_once_in_the_lead_window_and_uses_no_title_in_the_key() {
        let mut e = engine();
        let m = meeting("occurrence", 120_000);
        assert!(e.update(snapshot(vec![m.clone()]), 59_999, false).is_none());
        let due = e.update(snapshot(vec![m.clone()]), 60_000, false).unwrap();
        assert_eq!(due.key.len(), 64);
        assert!(!due.key.contains("Private"));
        assert!(
            e.view().is_none(),
            "durable main-window acknowledgment is required"
        );
        e.handled.insert(due.key.clone(), due.expires_ms);
        e.pending = None;
        assert!(e.update(snapshot(vec![m]), 120_000, false).is_none());
    }
    #[test]
    fn each_timing_offers_at_the_deadline_not_at_meeting_start() {
        for seconds in [0, 15, 30, 60] {
            let mut e = engine();
            e.lead_ms = seconds * 1000;
            let m = meeting("one", 120_000);
            let due_at = m.start_ms - e.lead_ms;
            assert!(e
                .update(snapshot(vec![m.clone()]), due_at - 1, false)
                .is_none());
            assert!(e.update(snapshot(vec![m]), due_at, false).is_some());
        }
    }

    #[test]
    fn polling_wakes_at_the_next_deadline_even_between_refresh_ticks() {
        let mut e = engine();
        e.lead_ms = 15_000;
        let m = meeting("one", 120_000);
        let s = snapshot(vec![m.clone()]);
        assert_eq!(e.next_delay(&s, 90_000), Duration::from_secs(15));
        assert_eq!(e.next_delay(&s, 100_000), Duration::from_secs(5));
        assert_eq!(e.next_delay(&s, 104_999), Duration::from_millis(1));
        assert!(e.update(s, 105_000, false).is_some());
        e.handled.insert(occurrence_key(&m), 200_000);
        assert_eq!(
            e.next_delay(&snapshot(vec![m]), 100_000),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn deadline_recomputes_for_rescheduling_and_timing_changes() {
        let mut e = engine();
        e.lead_ms = 15_000;
        assert_eq!(
            e.next_delay(&snapshot(vec![meeting("one", 120_000)]), 100_000),
            Duration::from_secs(5)
        );
        assert_eq!(
            e.next_delay(&snapshot(vec![meeting("one", 180_000)]), 100_000),
            Duration::from_secs(15)
        );
        e.lead_ms = 30_000;
        assert_eq!(
            e.next_delay(&snapshot(vec![meeting("one", 120_000)]), 85_000),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn disabled_recording_unselected_and_past_meetings_never_offer() {
        let m = meeting("one", 120_000);
        let mut e = engine();
        e.enabled = false;
        assert!(e.update(snapshot(vec![m.clone()]), 60_000, false).is_none());
        e.enabled = true;
        assert!(e.update(snapshot(vec![m.clone()]), 60_000, true).is_none());
        e.account_ids.clear();
        assert!(e.update(snapshot(vec![m.clone()]), 60_000, false).is_none());
        e.account_ids.push("account".into());
        assert!(e.update(snapshot(vec![m]), 240_000, false).is_none());
    }
    #[test]
    fn cancellation_rescheduling_and_wake_expire_stale_tokens() {
        let m = meeting("one", 120_000);
        let mut e = engine();
        let due = e.update(snapshot(vec![m.clone()]), 60_000, false).unwrap();
        assert_eq!(
            e.update(snapshot(vec![m.clone()]), 70_000, false)
                .unwrap()
                .token,
            due.token
        );
        e.update(snapshot(vec![]), 80_000, false);
        assert!(e.pending.is_none());
        e.update(snapshot(vec![m.clone()]), 90_000, false);
        e.update(snapshot(vec![m]), 1_000_000, false);
        assert!(e.pending.is_none());
    }
    #[test]
    fn enabling_mid_meeting_does_not_offer_old_meetings_and_occurrences_are_distinct() {
        let mut e = engine();
        e.armed_at = 130_000;
        assert!(e
            .update(
                snapshot(vec![meeting("yesterday", 120_000)]),
                140_000,
                false
            )
            .is_none());
        assert_ne!(
            occurrence_key(&meeting("series|1", 120_000)),
            occurrence_key(&meeting("series|2", 220_000))
        );
    }
}
