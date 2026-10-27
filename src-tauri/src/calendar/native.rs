use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::autoreleasepool;
use objc2::runtime::Bool;
use objc2_event_kit::{
    EKAuthorizationStatus, EKEntityType, EKEventStatus, EKEventStore, EKParticipantStatus,
    EKSourceType,
};
use objc2_foundation::{NSArray, NSDate, NSError};

use super::{
    calendar_accounts, eligible, meeting_provider, occurrence_id, selected_calendar_ids,
    CalendarInfo, CalendarMeeting, CalendarPermission, CalendarSnapshot,
};

pub(super) fn permission() -> CalendarPermission {
    // All supported Macs are macOS 14.4+, where full/write-only access exists.
    match unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) } {
        EKAuthorizationStatus::NotDetermined => CalendarPermission::NotDetermined,
        EKAuthorizationStatus::FullAccess => CalendarPermission::Granted,
        EKAuthorizationStatus::Restricted => CalendarPermission::Restricted,
        EKAuthorizationStatus::WriteOnly => CalendarPermission::WriteOnly,
        _ => CalendarPermission::Denied,
    }
}

static REQUEST_PENDING: AtomicBool = AtomicBool::new(false);
struct PermissionRequest;
impl Drop for PermissionRequest {
    fn drop(&mut self) {
        REQUEST_PENDING.store(false, Ordering::Release);
    }
}

pub(super) async fn request_access() -> Result<CalendarPermission, String> {
    if REQUEST_PENDING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("A calendar permission request is already pending.".to_string());
    }
    let guard = PermissionRequest;
    // No ObjC object crosses a task/thread boundary. A worker retains its store
    // until the asynchronous OS completion, keeping the main event loop free.
    tauri::async_runtime::spawn_blocking(move || {
        autoreleasepool(|_| {
            let _guard = guard;
            let current = permission();
            if !matches!(
                current,
                CalendarPermission::NotDetermined | CalendarPermission::WriteOnly
            ) {
                return Ok(current);
            }
            let store = unsafe { EKEventStore::new() };
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            let completion = RcBlock::new(move |_granted: Bool, error: *mut NSError| {
                let _ = send.send(error.is_null());
            });
            unsafe { store.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&completion)) };
            let success = receive
                .recv_timeout(Duration::from_secs(120))
                .map_err(|_| {
                    "Calendar permission is still awaiting a response. Check System Settings."
                        .to_string()
                })?;
            if !success {
                return Err("macOS could not complete the calendar permission request.".to_string());
            }
            let actual = permission();
            if actual == CalendarPermission::NotDetermined {
                return Err("macOS did not complete calendar authorization. Check the app signing configuration.".to_string());
            }
            Ok(actual)
        })
    })
    .await
    .map_err(|_| "Calendar permission request failed.".to_string())?
}

pub(super) fn read_snapshot(account_ids: &[String]) -> Result<CalendarSnapshot, String> {
    autoreleasepool(|_| unsafe {
        if permission() != CalendarPermission::Granted {
            return Err("Calendar access has not been granted.".to_string());
        }
        // Connection reads are explicit, so create a fresh store here. This
        // also avoids retaining an empty pre-grant store. EventKit objects stay
        // on this worker and only owned Rust values leave the autorelease pool.
        let store = EKEventStore::new();
        let calendars = store.calendarsForEntityType(EKEntityType::Event);
        let mut snapshot = CalendarSnapshot::default();
        let mut calendar_info = Vec::new();
        for calendar in calendars.iter() {
            let source = calendar.source();
            calendar_info.push(CalendarInfo {
                id: calendar.calendarIdentifier().to_string(),
                title: calendar.title().to_string(),
                account_id: source
                    .as_ref()
                    .map(|s| s.sourceIdentifier().to_string())
                    .unwrap_or_default(),
                account_name: source
                    .as_ref()
                    .map(|s| s.title().to_string())
                    .unwrap_or_default(),
                account_source: source.as_ref().is_some_and(|s| {
                    matches!(
                        s.sourceType(),
                        EKSourceType::CalDAV | EKSourceType::Exchange | EKSourceType::MobileMe
                    )
                }),
            });
        }
        snapshot.accounts = calendar_accounts(&calendar_info);
        let selected_ids = selected_calendar_ids(&calendar_info, account_ids);
        let selected = calendars
            .iter()
            .filter(|calendar| {
                selected_ids.contains(calendar.calendarIdentifier().to_string().as_str())
            })
            .collect::<Vec<_>>();
        if selected.is_empty() {
            // Never pass nil/an empty filter to EventKit: return the calendar
            // directory without reading any appointments until the user opts in.
            if permission() != CalendarPermission::Granted {
                return Err("Calendar access has been revoked.".to_string());
            }
            return Ok(snapshot);
        }
        let selected = NSArray::from_retained_slice(&selected);
        let now_seconds = NSDate::date().timeIntervalSince1970();
        let now_ms = (now_seconds * 1000.0) as i64;
        // An overlapping event is returned too, so an ongoing call remains
        // visible. EventKit expands recurrence, including edited occurrences.
        let start = NSDate::dateWithTimeIntervalSince1970(now_seconds);
        let end = NSDate::dateWithTimeIntervalSince1970(now_seconds + 7.0 * 24.0 * 3600.0);
        let predicate =
            store.predicateForEventsWithStartDate_endDate_calendars(&start, &end, Some(&selected));
        let events = store.eventsMatchingPredicate(&predicate);
        for event in events.iter() {
            let start_ms = (event.startDate().timeIntervalSince1970() * 1000.0) as i64;
            let end_ms = (event.endDate().timeIntervalSince1970() * 1000.0) as i64;
            let declined = event.attendees().is_some_and(|attendees| {
                attendees.iter().any(|a| {
                    a.isCurrentUser() && a.participantStatus() == EKParticipantStatus::Declined
                })
            });
            if !eligible(
                start_ms,
                end_ms,
                now_ms,
                event.isAllDay(),
                event.status() == EKEventStatus::Canceled,
                declined,
            ) {
                continue;
            }
            let Some(calendar) = event.calendar() else {
                continue;
            };
            let calendar_id = calendar.calendarIdentifier().to_string();
            if !selected_ids.contains(calendar_id.as_str()) {
                continue;
            }
            let Some(info) = calendar_info.iter().find(|info| info.id == calendar_id) else {
                continue;
            };
            let account_id = info.account_id.clone();
            let fields = [
                event
                    .URL()
                    .and_then(|u| u.absoluteString())
                    .map(|s| s.to_string()),
                event.location().map(|s| s.to_string()),
                event.notes().map(|s| s.to_string()),
            ];
            let Some(provider) = meeting_provider(&fields) else {
                continue;
            };
            let event_id = event
                .eventIdentifier()
                .unwrap_or_else(|| event.calendarItemIdentifier())
                .to_string();
            snapshot.meetings.push(CalendarMeeting {
                id: occurrence_id(&calendar_id, &event_id, start_ms),
                account_id,
                calendar_id,
                title: event.title().to_string(),
                start_ms,
                end_ms,
                provider,
            });
        }
        // Permission may have been revoked while EventKit was answering. Never
        // return the earlier private snapshot once the revocation is observed.
        if permission() != CalendarPermission::Granted {
            return Err("Calendar access has been revoked.".to_string());
        }
        snapshot
            .meetings
            .sort_by(|a, b| (a.start_ms, &a.id).cmp(&(b.start_ms, &b.id)));
        snapshot.meetings.dedup_by(|a, b| a.id == b.id);
        Ok(snapshot)
    })
}
