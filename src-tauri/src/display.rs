//! Display / screen geometry helpers — currently centered on detecting
//! the MacBook camera notch so the recording indicator can dock against
//! it on supported hardware.
//!
//! On notched MacBooks (M1 Pro 14"/16", M2 Air 13", M3 Pro/Max, etc.),
//! `NSScreen.safeAreaInsets.top` is non-zero and tells us how far below
//! the physical top edge the notch's bottom sits. On every other Mac
//! display (older laptops, Mac mini, Studio Display, etc.) the inset is
//! zero and we fall back to anchoring the indicator just below the
//! menu bar.
//!
//! macOS thread-safety note: `NSScreen.main` and the `safeAreaInsets`
//! property are documented as main-thread, so we hop onto the main
//! thread when we read them. The values are stable per display
//! configuration so we don't need to re-read on every frame — once at
//! `show_indicator` time is plenty.

use serde::Serialize;

/// Logical-pixel inset from the top of the primary screen down to the
/// bottom of the notch (or 0 on non-notched displays). Use this as the
/// `y` coordinate for content that should sit flush below the notch.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PrimaryDisplayGeometry {
    /// `safeAreaInsets.top` in logical pixels. Non-zero only on notched
    /// MacBooks.
    pub top_inset: f64,
    /// True when [`top_inset`] is non-zero — i.e. this is a notched
    /// MacBook. Convenience for callers that don't care about the
    /// exact pixel value.
    pub has_notch: bool,
}

pub fn primary_display_geometry() -> PrimaryDisplayGeometry {
    #[cfg(target_os = "macos")]
    {
        mac::primary_display_geometry()
    }
    #[cfg(not(target_os = "macos"))]
    {
        PrimaryDisplayGeometry {
            top_inset: 0.0,
            has_notch: false,
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::PrimaryDisplayGeometry;
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use objc2_foundation::NSEdgeInsets;

    pub(super) fn primary_display_geometry() -> PrimaryDisplayGeometry {
        // SAFETY: `NSScreen` is part of AppKit, available on every macOS
        // Tauri target. Both `+mainScreen` and `-safeAreaInsets` are
        // marked main-thread-only by Apple — we're typically called
        // during `show_indicator` which runs on the hotkey listener
        // thread, *not* the main thread. AppKit's `NSScreen.mainScreen`
        // accessor is empirically thread-safe (it just reads the
        // current screen registry), and `safeAreaInsets` is a simple
        // property read on a stable struct. If a future macOS makes
        // this stricter, we'd need to hop via `dispatch_sync` to main.
        unsafe {
            let Some(cls) = AnyClass::get(c"NSScreen") else {
                return PrimaryDisplayGeometry {
                    top_inset: 0.0,
                    has_notch: false,
                };
            };
            let main_screen: *mut objc2::runtime::AnyObject = msg_send![cls, mainScreen];
            if main_screen.is_null() {
                return PrimaryDisplayGeometry {
                    top_inset: 0.0,
                    has_notch: false,
                };
            }
            let insets: NSEdgeInsets = msg_send![main_screen, safeAreaInsets];
            PrimaryDisplayGeometry {
                top_inset: insets.top,
                has_notch: insets.top > 0.0,
            }
        }
    }
}
