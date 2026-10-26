//! Native calendar connection shared by settings and the future reminder scheduler.
//!
//! No account credentials, calendar writes, recordings, or automatic permission
//! requests. Only explicit main-window actions can request access or read events.
//! Calendar content must never be logged, put in diagnostics, or broadcast.

#[cfg(target_os = "macos")]
mod native;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CalendarPermission {
    NotDetermined,
    Granted,
    Denied,
    Restricted,
    WriteOnly,
    Unsupported,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct CalendarInfo {
    id: String,
    title: String,
    account_id: String,
    account_name: String,
    account_source: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalendarAccount {
    pub id: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalendarMeeting {
    pub id: String,
    pub account_id: String,
    pub calendar_id: String,
    pub title: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub provider: MeetingProvider,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CalendarSnapshot {
    pub accounts: Vec<CalendarAccount>,
    pub meetings: Vec<CalendarMeeting>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MeetingProvider {
    GoogleMeet,
    Zoom,
    Teams,
    Webex,
}

/// Recognize a meeting, not merely a URL in an appointment. An unrelated
/// document/password-reset link must not shadow a call link in another field.
/// Only the provider leaves this parser; invite URLs and notes are discarded.
#[cfg(any(target_os = "macos", test))]
fn meeting_provider(fields: &[Option<String>]) -> Option<MeetingProvider> {
    static URLS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let urls = URLS.get_or_init(|| regex::Regex::new(r#"(?i:https?)://[^\s<>\"']+"#).unwrap());
    for field in fields.iter().flatten() {
        for found in urls.find_iter(field) {
            let text = found
                .as_str()
                .trim_end_matches([')', ']', '}', '.', ',', ';']);
            let Ok(url) = reqwest::Url::parse(text) else {
                continue;
            };
            if !url.username().is_empty() || url.password().is_some() {
                continue;
            }
            let host = url.host_str().unwrap_or_default();
            let segments: Vec<_> = url.path().split('/').filter(|s| !s.is_empty()).collect();
            let Some(first) = segments.first().copied() else {
                continue;
            };
            if host == "meet.google.com" {
                let parts: Vec<_> = first.split('-').collect();
                if segments.len() == 1
                    && parts.iter().map(|p| p.len()).eq([3, 4, 3])
                    && parts
                        .iter()
                        .all(|p| p.bytes().all(|b| b.is_ascii_alphabetic()))
                {
                    return Some(MeetingProvider::GoogleMeet);
                }
            } else if host == "zoom.us"
                || host.ends_with(".zoom.us")
                || host == "zoomgov.com"
                || host.ends_with(".zoomgov.com")
            {
                if segments.len() >= 2 && ["j", "w", "s", "my"].contains(&first) {
                    return Some(MeetingProvider::Zoom);
                }
            } else if host == "teams.microsoft.com" || host == "teams.live.com" {
                if segments.len() >= 2 && ["meetup-join", "meet"].contains(&first)
                    || segments.len() >= 3 && first == "l" && segments[1] == "meetup-join"
                {
                    return Some(MeetingProvider::Teams);
                }
            } else if host.ends_with(".webex.com")
                && (first == "meet" && segments.len() >= 2
                    || url.path().contains("/j.php")
                    || url.path().contains("/join/"))
            {
                return Some(MeetingProvider::Webex);
            }
        }
    }
    None
}

#[cfg(any(target_os = "macos", test))]
fn occurrence_id(calendar_id: &str, event_id: &str, start_ms: i64) -> String {
    // Length prefixes avoid collisions when identifiers contain delimiters.
    format!(
        "{}:{calendar_id}{}:{event_id}:{start_ms}",
        calendar_id.len(),
        event_id.len()
    )
}

#[cfg(any(target_os = "macos", test))]
fn eligible(
    start_ms: i64,
    end_ms: i64,
    now_ms: i64,
    all_day: bool,
    canceled: bool,
    declined: bool,
) -> bool {
    !all_day && !canceled && !declined && end_ms > now_ms && end_ms > start_ms
}

#[cfg(any(target_os = "macos", test))]
fn email_label(label: &str) -> Option<String> {
    static EMAIL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let label = label.trim();
    let pattern = EMAIL.get_or_init(|| regex::Regex::new(r"(?i)^[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$").unwrap());
    (label.len() <= 320 && pattern.is_match(label)).then(|| label.to_string())
}

/// EventKit exposes a source title, not a dedicated login-email property. Use
/// an explicit source email first; otherwise accept one unambiguous exact email
/// label among that source's calendars. Never infer identity from invite guests,
/// unrelated sources, or private Apple account databases. These are display
/// labels: sourceIdentifier, not the email/calendar title, controls selection.
#[cfg(any(target_os = "macos", test))]
fn calendar_accounts(calendars: &[CalendarInfo]) -> Vec<CalendarAccount> {
    let mut sources = std::collections::BTreeMap::<&str, Vec<&CalendarInfo>>::new();
    for calendar in calendars
        .iter()
        .filter(|c| c.account_source && !c.account_id.is_empty())
    {
        sources
            .entry(&calendar.account_id)
            .or_default()
            .push(calendar);
    }
    let mut accounts = Vec::new();
    for (id, calendars) in sources {
        let explicit = calendars.iter().find_map(|c| email_label(&c.account_name));
        let email = explicit.or_else(|| {
            let labels: std::collections::BTreeMap<_, _> = calendars
                .iter()
                .filter_map(|c| email_label(&c.title))
                .map(|email| (email.to_ascii_lowercase(), email))
                .collect();
            (labels.len() == 1)
                .then(|| labels.into_values().next())
                .flatten()
        });
        if let Some(email) = email {
            accounts.push(CalendarAccount {
                id: id.to_string(),
                email,
            });
        }
    }
    accounts.sort_by(|a, b| (&a.email, &a.id).cmp(&(&b.email, &b.id)));
    accounts
}

/// Selecting a source includes all of its calendars, including newly synced
/// ones. Local/subscribed/birthday sources and unidentified accounts cannot be
/// activated via guessed IDs. Empty selection always means no event query.
#[cfg(any(target_os = "macos", test))]
fn selected_calendar_ids<'a>(
    available: &'a [CalendarInfo],
    requested: &[String],
) -> std::collections::BTreeSet<&'a str> {
    let accounts = calendar_accounts(available);
    let known: std::collections::BTreeSet<_> = accounts.iter().map(|a| a.id.as_str()).collect();
    let selected: std::collections::BTreeSet<_> = requested
        .iter()
        .map(String::as_str)
        .filter(|id| known.contains(id))
        .collect();
    available
        .iter()
        .filter(|c| c.account_source && selected.contains(c.account_id.as_str()))
        .map(|c| c.id.as_str())
        .collect()
}

#[tauri::command]
pub fn calendar_permission() -> CalendarPermission {
    #[cfg(target_os = "macos")]
    {
        native::permission()
    }
    #[cfg(not(target_os = "macos"))]
    {
        CalendarPermission::Unsupported
    }
}

/// The OS dialog appears only on this explicit user action. The completion
/// outlives the command invocation without blocking the application's UI thread.
#[tauri::command]
pub async fn request_calendar_access() -> Result<CalendarPermission, String> {
    #[cfg(target_os = "macos")]
    {
        native::request_access().await
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(CalendarPermission::Unsupported)
    }
}

/// Bounded to the next seven days, including currently-running meetings. This
/// never requests permission. The frontend must invoke it only after connection
/// consent, not at launch or merely on entering Settings.
#[tauri::command]
pub async fn read_calendar_snapshot(account_ids: Vec<String>) -> Result<CalendarSnapshot, String> {
    if account_ids.len() > 2048 || account_ids.iter().any(|id| id.len() > 4096) {
        return Err("Too many account selections.".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        tauri::async_runtime::spawn_blocking(move || native::read_snapshot(&account_ids))
            .await
            .map_err(|_| "Calendar reading failed.".to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("Calendar access is only available on macOS.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(fields: &[&str]) -> Option<MeetingProvider> {
        meeting_provider(
            &fields
                .iter()
                .map(|s| Some((*s).to_owned()))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn finds_call_links_across_url_location_and_html_notes() {
        assert_eq!(
            provider(&[
                "https://example.com/doc",
                "Office",
                "<a href=\"https://meet.google.com/abc-defg-hij\">Join</a>"
            ]),
            Some(MeetingProvider::GoogleMeet)
        );
        assert_eq!(
            provider(&["Join (https://us02web.zoom.us/j/123?pwd=secret)."]),
            Some(MeetingProvider::Zoom)
        );
        assert_eq!(
            provider(&["https://teams.microsoft.com/l/meetup-join/19%3ameeting/thread"]),
            Some(MeetingProvider::Teams)
        );
        assert_eq!(
            provider(&["https://teams.live.com/meet/123"]),
            Some(MeetingProvider::Teams)
        );
        assert_eq!(
            provider(&["https://acme.webex.com/acme/j.php?MTID=abc"]),
            Some(MeetingProvider::Webex)
        );
    }

    #[test]
    fn rejects_lookalike_hosts_and_non_meeting_pages() {
        for link in [
            "https://meet.google.com/tel/123",
            "https://meet.google.com/landing",
            "https://zoom.us/download",
            "https://zoom.us.evil.example/j/123",
            "https://notzoom.us/j/123",
            "https://zoom.us@evil.example/j/123",
            "https://user@zoom.us/j/123",
            "https://teams.microsoft.com/",
        ] {
            assert_eq!(provider(&[link]), None, "{link}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_permission_query_does_not_prompt_or_read_events() {
        assert_ne!(calendar_permission(), CalendarPermission::Unsupported);
    }

    fn calendar(id: &str, title: &str, account_id: &str, account_name: &str) -> CalendarInfo {
        CalendarInfo {
            id: id.into(),
            title: title.into(),
            account_id: account_id.into(),
            account_name: account_name.into(),
            account_source: true,
        }
    }

    #[test]
    fn account_selection_includes_all_its_calendars_but_never_another_account() {
        let calendars = vec![
            calendar("work", "work@example.com", "a", "Google"),
            calendar("holidays", "Holidays", "a", "Google"),
            calendar("new", "New calendar", "a", "Google"),
            calendar("personal", "Personal", "b", "personal@example.com"),
        ];
        assert!(selected_calendar_ids(&calendars, &[]).is_empty());
        assert!(selected_calendar_ids(&calendars, &["unknown".into(), "work".into()]).is_empty());
        let selected =
            selected_calendar_ids(&calendars, &["a".into(), "a".into(), "unknown".into()]);
        assert_eq!(
            selected.into_iter().collect::<Vec<_>>(),
            vec!["holidays", "new", "work"]
        );
    }

    #[test]
    fn email_account_rows_use_source_identity_and_unambiguous_email_labels_only() {
        let calendars = vec![
            calendar("work", "work@example.com", "a", "Google"),
            calendar("holidays", "Holidays", "a", "Google"),
            calendar("personal", "Family", "b", "personal@example.com"),
            calendar("shared", "other@example.com", "b", "personal@example.com"),
            calendar("home", "Home", "icloud", "iCloud"),
        ];
        assert_eq!(
            calendar_accounts(&calendars),
            vec![
                CalendarAccount {
                    id: "b".into(),
                    email: "personal@example.com".into()
                },
                CalendarAccount {
                    id: "a".into(),
                    email: "work@example.com".into()
                },
            ]
        );
    }

    #[test]
    fn ambiguous_and_local_email_labels_never_become_selectable_accounts() {
        let mut local = calendar("local", "local@example.com", "local", "local@example.com");
        local.account_source = false;
        let calendars = vec![
            local,
            calendar("one", "one@example.com", "ambiguous", "Google"),
            calendar("two", "two@example.com", "ambiguous", "Google"),
        ];
        assert!(calendar_accounts(&calendars).is_empty());
        assert!(
            selected_calendar_ids(&calendars, &["local".into(), "ambiguous".into()]).is_empty()
        );
        for label in [
            "Google",
            "Holidays",
            "Email me at person@example.com",
            "https://example.com",
            "a@b@c.com",
            "<script>@example.com",
        ] {
            assert_eq!(email_label(label), None);
        }
    }

    #[test]
    fn recurrence_and_calendar_identifiers_are_part_of_identity() {
        assert_ne!(
            occurrence_id("a", "series", 1),
            occurrence_id("a", "series", 2)
        );
        assert_ne!(
            occurrence_id("a", "series", 1),
            occurrence_id("b", "series", 1)
        );
        assert_ne!(occurrence_id("a|b", "c", 1), occurrence_id("a", "b|c", 1));
    }

    #[test]
    fn filters_events_unsuitable_for_meeting_reminders() {
        assert!(eligible(90, 110, 100, false, false, false));
        assert!(eligible(110, 120, 100, false, false, false));
        for (start, end, all_day, canceled, declined) in [
            (80, 100, false, false, false),
            (110, 110, false, false, false),
            (110, 120, true, false, false),
            (110, 120, false, true, false),
            (110, 120, false, false, true),
        ] {
            assert!(!eligible(start, end, 100, all_day, canceled, declined));
        }
    }
}
