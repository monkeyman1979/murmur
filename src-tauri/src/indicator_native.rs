//! Native helpers for Murmur's transparent floating windows on macOS.
//!
//! Tauri's `transparent: true` only fully works on macOS when the
//! `macOSPrivateApi: true` app-level config is also set — otherwise
//! the WKWebView inside the NSWindow stays opaque and any CSS
//! transparency we apply ends up compositing onto a white backing.
//!
//! This module also positions the dictation indicator through AppKit. Keeping
//! target-screen selection and placement in native logical coordinates avoids
//! Tao's mixed-scale cursor/monitor conversions on multi-display setups.

/// Label of the dictation indicator pill.
pub const INDICATOR: &str = "indicator";
/// Label of the floating meeting-recording control.
pub const MEETING_CONTROLLER: &str = "meeting-controller";

#[cfg(target_os = "macos")]
pub fn force_transparent(app: &tauri::AppHandle, label: &str) {
    mac::force_transparent(app, label);
}

/// Move the dictation indicator to the bottom-center of the display containing
/// the focused control in the frontmost application.
///
/// Returns `true` when native placement was scheduled. Callers retain a
/// framework-level primary-display fallback for unsupported platforms or when
/// a native window handle is unavailable.
#[cfg(target_os = "macos")]
pub fn position_on_focused_screen(app: &tauri::AppHandle) -> bool {
    mac::position_on_focused_screen(app)
}

#[cfg(not(target_os = "macos"))]
pub fn force_transparent(_app: &tauri::AppHandle, _label: &str) {}

#[cfg(not(target_os = "macos"))]
pub fn position_on_focused_screen(_app: &tauri::AppHandle) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod mac {
    use std::{ffi::c_void, ptr};

    use core_foundation::{base::TCFType, string::CFString};
    use objc2::{class, msg_send, runtime::AnyObject, MainThreadMarker};
    use objc2_app_kit::{NSColor, NSScreen, NSWindow, NSWorkspace};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use tauri::{AppHandle, Manager};

    use super::INDICATOR;

    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    type AXUIElementRef = *const c_void;

    const AX_ERROR_SUCCESS: i32 = 0;
    const AX_VALUE_CGPOINT_TYPE: u32 = 1;
    const AX_VALUE_CGSIZE_TYPE: u32 = 2;
    const BOTTOM_MARGIN: f64 = 20.0;

    // The AX attribute names are CFSTR macros in Apple's headers, not exported
    // framework symbols, so construct their CFString values locally.
    const AX_FOCUSED_UI_ELEMENT: &str = "AXFocusedUIElement";
    const AX_FOCUSED_WINDOW: &str = "AXFocusedWindow";
    const AX_POSITION: &str = "AXPosition";
    const AX_SIZE: &str = "AXSize";

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32;
        fn AXValueGetValue(value: CFTypeRef, value_type: u32, value_ptr: *mut c_void) -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: CFTypeRef);
    }

    /// Center of the focused UI element, or its focused window when the
    /// element does not expose geometry. Accessibility permission is already a
    /// Murmur prerequisite for pasting, so this adds no new permission prompt.
    fn focused_target_center() -> Option<NSPoint> {
        let pid = objc2::rc::autoreleasepool(|_| {
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|application| application.processIdentifier())
        })?;

        unsafe {
            let application = AXUIElementCreateApplication(pid);
            if application.is_null() {
                return None;
            }

            let focused_ui_attribute = CFString::new(AX_FOCUSED_UI_ELEMENT);
            let focused_window_attribute = CFString::new(AX_FOCUSED_WINDOW);
            let center = copy_attribute(
                application,
                focused_ui_attribute.as_concrete_TypeRef() as CFStringRef,
            )
            .and_then(|element| {
                let center = element_center(element as AXUIElementRef);
                CFRelease(element);
                center
            })
            .or_else(|| {
                copy_attribute(
                    application,
                    focused_window_attribute.as_concrete_TypeRef() as CFStringRef,
                )
                .and_then(|window| {
                    let center = element_center(window as AXUIElementRef);
                    CFRelease(window);
                    center
                })
            });
            CFRelease(application);
            center
        }
    }

    unsafe fn copy_attribute(element: AXUIElementRef, attribute: CFStringRef) -> Option<CFTypeRef> {
        let mut value = ptr::null();
        if AXUIElementCopyAttributeValue(element, attribute, &mut value) == AX_ERROR_SUCCESS
            && !value.is_null()
        {
            Some(value)
        } else {
            None
        }
    }

    unsafe fn element_center(element: AXUIElementRef) -> Option<NSPoint> {
        let position_attribute = CFString::new(AX_POSITION);
        let size_attribute = CFString::new(AX_SIZE);
        let position_value = copy_attribute(
            element,
            position_attribute.as_concrete_TypeRef() as CFStringRef,
        )?;
        let size_value =
            copy_attribute(element, size_attribute.as_concrete_TypeRef() as CFStringRef);

        let mut position = NSPoint::new(0.0, 0.0);
        let has_position = AXValueGetValue(
            position_value,
            AX_VALUE_CGPOINT_TYPE,
            &mut position as *mut NSPoint as *mut c_void,
        );
        CFRelease(position_value);
        if !has_position {
            if let Some(size_value) = size_value {
                CFRelease(size_value);
            }
            return None;
        }

        let mut size = NSSize::new(0.0, 0.0);
        let has_size = size_value.is_some_and(|value| {
            let result = AXValueGetValue(
                value,
                AX_VALUE_CGSIZE_TYPE,
                &mut size as *mut NSSize as *mut c_void,
            );
            CFRelease(value);
            result
        });

        Some(if has_size {
            NSPoint::new(
                position.x + size.width / 2.0,
                position.y + size.height / 2.0,
            )
        } else {
            position
        })
    }

    pub fn position_on_focused_screen(app: &AppHandle) -> bool {
        // Query the external app before scheduling AppKit work. The global
        // hotkey does not activate Murmur, so this is still the app/control the
        // user was typing into when dictation began.
        let focused_center = focused_target_center();
        let Some(window) = app.get_webview_window(INDICATOR) else {
            crate::murmur_dev_log!(
                "[murmur][indicator_native] indicator window not found for placement"
            );
            return false;
        };
        let ns_window = match window.ns_window() {
            Ok(window) => window as usize,
            Err(error) => {
                crate::murmur_dev_log!(
                    "[murmur][indicator_native] indicator ns_window() failed: {error}"
                );
                return false;
            }
        };

        let scheduled = window.run_on_main_thread(move || unsafe {
            let mtm = MainThreadMarker::new_unchecked();
            let screens = NSScreen::screens(mtm);
            let main_screen = NSScreen::mainScreen(mtm);

            // AX geometry uses the Quartz top-left coordinate system. Convert
            // its focused center to AppKit's bottom-left system before testing
            // it against NSScreen frames. If AX geometry is unavailable, use
            // the pointer display, then the primary display.
            let focused_appkit_point = focused_center.and_then(|point| {
                let main = main_screen.as_ref()?;
                let frame = NSScreen::frame(main);
                Some(NSPoint::new(
                    point.x,
                    frame.origin.y + frame.size.height - point.y,
                ))
            });
            let pointer: NSPoint = msg_send![class!(NSEvent), mouseLocation];
            let target_screen = screens
                .iter()
                .find(|screen| {
                    focused_appkit_point
                        .is_some_and(|point| point_in_rect(point, NSScreen::frame(screen)))
                })
                .or_else(|| {
                    screens
                        .iter()
                        .find(|screen| point_in_rect(pointer, NSScreen::frame(screen)))
                })
                .or(main_screen);
            let Some(screen) = target_screen else {
                crate::murmur_dev_log!("[murmur][indicator_native] AppKit found no screen for placement");
                return;
            };

            let indicator = &*(ns_window as *mut NSWindow);
            let work_area = NSScreen::visibleFrame(&screen);
            let frame = NSWindow::frame(indicator);
            let x = work_area.origin.x + (work_area.size.width - frame.size.width) / 2.0;
            let y = work_area.origin.y + BOTTOM_MARGIN;
            indicator.setFrameOrigin(NSPoint::new(x, y));
            crate::murmur_dev_log!(
                "[murmur][indicator_native] positioned on focused screen: visibleFrame=({:.1},{:.1},{:.1},{:.1}) target=({x:.1},{y:.1})",
                work_area.origin.x,
                work_area.origin.y,
                work_area.size.width,
                work_area.size.height,
            );
        });

        match scheduled {
            Ok(()) => true,
            Err(error) => {
                crate::murmur_dev_log!(
                    "[murmur][indicator_native] scheduling focused-screen placement failed: {error}"
                );
                false
            }
        }
    }

    fn point_in_rect(point: NSPoint, rect: NSRect) -> bool {
        point.x >= rect.origin.x
            && point.x < rect.origin.x + rect.size.width
            && point.y >= rect.origin.y
            && point.y < rect.origin.y + rect.size.height
    }

    pub fn force_transparent(app: &AppHandle, label: &str) {
        let Some(window) = app.get_webview_window(label) else {
            crate::murmur_dev_log!("[murmur][indicator_native] window not found: {label}");
            return;
        };

        let ns_window_ptr = match window.ns_window() {
            Ok(ptr) => ptr,
            Err(e) => {
                crate::murmur_dev_log!(
                    "[murmur][indicator_native] ns_window() failed for {label}: {e}"
                );
                return;
            }
        };

        // SAFETY: `ns_window()` returns a valid `NSWindow *` while the
        // Tauri window is alive. These setters are idempotent.
        unsafe {
            let ns_window = &*(ns_window_ptr as *mut AnyObject as *mut NSWindow);
            ns_window.setOpaque(false);
            ns_window.setBackgroundColor(Some(&NSColor::clearColor()));
        }
        crate::murmur_dev_log!(
            "[murmur][indicator_native] {label} NSWindow forced: isOpaque=NO, backgroundColor=clear"
        );
    }
}
