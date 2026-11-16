//! One reminder follows attention at reveal, then stays on that display.
//! Only display topology changes are observed; no focus or pointer monitoring.

/// Keep an existing display whenever possible. Otherwise prefer the focused
/// window's display, then the pointer, then the primary (first) display.
fn select_display(
    connected: &[u32],
    pinned: Option<u32>,
    focused: Option<u32>,
    pointer: Option<u32>,
) -> Option<u32> {
    [pinned, focused, pointer]
        .into_iter()
        .flatten()
        .find(|id| connected.contains(id))
        .or_else(|| connected.first().copied())
}

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
pub(super) use mac::{focused_window_center, place, stop};

#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
mod mac {
    use std::{cell::RefCell, ptr::NonNull};

    use block2::RcBlock;
    use objc2::{class, msg_send, rc::Retained, runtime::ProtocolObject, MainThreadMarker};
    use objc2_app_kit::{NSApplicationDidChangeScreenParametersNotification, NSScreen, NSWindow};
    use objc2_foundation::{
        ns_string, NSNotification, NSNotificationCenter, NSNumber, NSObjectProtocol,
        NSOperationQueue, NSPoint, NSRect,
    };
    use tauri::{AppHandle, Manager};

    use super::super::reminders::{reminder_offsets, WINDOW};
    use super::select_display;

    struct Placement {
        token: String,
        display: u32,
        observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    }

    impl Drop for Placement {
        fn drop(&mut self) {
            // The observer and all Placement access belong to AppKit's thread.
            unsafe {
                NSNotificationCenter::defaultCenter().removeObserver((*self.observer).as_ref())
            };
        }
    }

    thread_local! {
        static PLACEMENT: RefCell<Option<Placement>> = const { RefCell::new(None) };
    }

    pub(crate) async fn focused_window_center() -> Option<NSPoint> {
        tauri::async_runtime::spawn_blocking(crate::indicator_native::focused_window_center)
            .await
            .ok()
            .flatten()
    }

    fn display_id(screen: &NSScreen) -> Option<u32> {
        screen
            .deviceDescription()
            .objectForKey(ns_string!("NSScreenNumber"))?
            .downcast_ref::<NSNumber>()
            .map(NSNumber::unsignedIntValue)
    }

    fn contains(point: NSPoint, rect: NSRect) -> bool {
        point.x >= rect.origin.x
            && point.x < rect.origin.x + rect.size.width
            && point.y >= rect.origin.y
            && point.y < rect.origin.y + rect.size.height
    }

    fn target_screen(
        pinned: Option<u32>,
        focused: Option<NSPoint>,
        mtm: MainThreadMarker,
    ) -> Option<Retained<NSScreen>> {
        // Never cache NSScreen objects across a display configuration change.
        let screens = NSScreen::screens(mtm);
        let primary = screens.firstObject()?;
        let ids: Vec<_> = screens
            .iter()
            .filter_map(|screen| display_id(&screen))
            .collect();
        // AX is Quartz top-left; AppKit uses a bottom-left global origin.
        let primary_frame = primary.frame();
        let focused = focused.map(|point| {
            NSPoint::new(
                point.x,
                primary_frame.origin.y + primary_frame.size.height - point.y,
            )
        });
        let pointer: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
        let screen_at = |point| {
            screens
                .iter()
                .find(|screen| contains(point, screen.frame()))
                .and_then(|screen| display_id(&screen))
        };
        let id = select_display(
            &ids,
            pinned,
            focused.and_then(screen_at),
            screen_at(pointer),
        )?;
        screens.iter().find(|screen| display_id(screen) == Some(id))
    }

    fn move_to_corner(window: &NSWindow, screen: &NSScreen) {
        let work = screen.visibleFrame();
        let size = window.frame().size;
        let (x, y) = reminder_offsets(work.size.width, work.size.height, size.width, size.height);
        // Move only; preserve the full reminder size and nonactivating behavior.
        window.setFrameOrigin(NSPoint::new(work.origin.x + x, work.origin.y + y));
    }

    fn clear() {
        // Release the RefCell borrow before deregistering the observer.
        let previous = PLACEMENT.with(|state| state.borrow_mut().take());
        drop(previous);
    }

    pub(crate) fn stop(app: &AppHandle) {
        let _ = app.run_on_main_thread(clear);
    }

    fn pinned_display(token: &str) -> Option<u32> {
        PLACEMENT.with(|state| {
            state
                .borrow()
                .as_ref()
                .filter(|placement| placement.token == token)
                .map(|placement| placement.display)
        })
    }

    fn refresh(app: &AppHandle, token: &str, focused: Option<NSPoint>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(pinned) = pinned_display(token) else {
            return;
        };
        let Some(window) = app.get_webview_window(WINDOW) else {
            return;
        };
        let Ok(handle) = window.ns_window() else {
            return;
        };
        // Called only on AppKit's thread. This is still the same managed window.
        let native = unsafe { &*(handle as *mut NSWindow) };
        if !native.isVisible() {
            return;
        }
        let Some(screen) = target_screen(Some(pinned), focused, mtm) else {
            return;
        };
        move_to_corner(native, &screen);
        if let Some(id) = display_id(&screen) {
            PLACEMENT.with(|state| {
                if let Some(placement) = state.borrow_mut().as_mut() {
                    placement.display = id;
                }
            });
        }
    }

    fn screens_changed(app: &AppHandle, token: &str) {
        let Some(pinned) = pinned_display(token) else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if NSScreen::screens(mtm)
            .iter()
            .any(|screen| display_id(&screen) == Some(pinned))
        {
            // A resolution, Dock or topology change may alter the usable frame,
            // but cannot move the reminder to a different connected display.
            refresh(app, token, None);
            return;
        }
        // Only a disconnected target needs fresh focus detection. Never block
        // AppKit on an AX query, and ignore results for a dismissed/replaced offer.
        let app = app.clone();
        let token = token.to_owned();
        tauri::async_runtime::spawn(async move {
            let focused = focused_window_center().await;
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || refresh(&handle, &token, focused));
        });
    }

    pub(crate) fn place(
        app: &AppHandle,
        window: &tauri::WebviewWindow,
        token: String,
        focused: Option<NSPoint>,
        mtm: MainThreadMarker,
    ) -> Result<(), String> {
        // The marker requires callers to validate and reveal on AppKit's thread.
        // Duplicate delivery of the same offer must not retarget it.
        if pinned_display(&token).is_some() {
            return Ok(());
        }
        clear();
        let handle = window
            .ns_window()
            .map_err(|_| "Reminder window is unavailable.")?;
        let screen = target_screen(None, focused, mtm).ok_or("Reminder display is unavailable.")?;
        let display = display_id(&screen).ok_or("Reminder display is unavailable.")?;
        let native = unsafe { &*(handle as *mut NSWindow) };
        move_to_corner(native, &screen);
        let callback_app = app.clone();
        let callback_token = token.clone();
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            screens_changed(&callback_app, &callback_token);
        });
        let observer = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSApplicationDidChangeScreenParametersNotification),
                None,
                Some(&NSOperationQueue::mainQueue()),
                &block,
            )
        };
        PLACEMENT.with(|state| {
            *state.borrow_mut() = Some(Placement {
                token,
                display,
                observer,
            });
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::select_display;

    #[test]
    fn reveal_prefers_focused_window_then_pointer_then_primary() {
        assert_eq!(select_display(&[1, 2], None, Some(2), Some(1)), Some(2));
        assert_eq!(select_display(&[1, 2], None, None, Some(2)), Some(2));
        assert_eq!(select_display(&[1, 2], None, Some(99), Some(2)), Some(2));
        assert_eq!(select_display(&[1, 2], None, None, None), Some(1));
    }

    #[test]
    fn focus_pointer_and_display_order_changes_cannot_move_a_visible_reminder() {
        assert_eq!(select_display(&[1, 2], Some(2), Some(1), Some(1)), Some(2));
        assert_eq!(
            select_display(&[2, 1, 3], Some(2), Some(3), Some(3)),
            Some(2)
        );
    }

    #[test]
    fn disconnect_recovers_to_an_available_display_then_stays_there() {
        let replacement = select_display(&[1, 3], Some(2), Some(3), Some(1));
        assert_eq!(replacement, Some(3));
        assert_eq!(
            select_display(&[1, 2, 3], replacement, Some(2), Some(2)),
            Some(3)
        );
        assert_eq!(select_display(&[1], Some(2), None, None), Some(1));
    }

    #[test]
    fn a_temporary_empty_display_list_does_not_choose_a_nonexistent_monitor() {
        assert_eq!(select_display(&[], Some(2), Some(2), Some(1)), None);
    }
}
