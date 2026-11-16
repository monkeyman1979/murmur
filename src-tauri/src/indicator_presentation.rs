//! Recording-surface geometry only. No audio lifecycle or routing changes.
use std::{collections::HashMap, sync::Mutex};

use serde::Serialize;
#[cfg(target_os = "macos")]
use tauri::Emitter;
use tauri::Manager;

use crate::indicator_position::IndicatorStyle;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Presentation {
    pub style: IndicatorStyle,
    pub expanded: bool,
    pub compact_width: f64,
    pub notch_width: f64,
    pub notch_height: f64,
    pub expanded_width: f64,
    pub expanded_height: f64,
    pub revision: u64,
    pub fresh: bool,
    #[serde(skip_serializing)]
    pub anchor_center_x: f64,
    #[serde(skip_serializing)]
    pub anchor_top_y: f64,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            style: IndicatorStyle::Floating,
            expanded: false,
            compact_width: 360.0,
            notch_width: 0.0,
            notch_height: 0.0,
            expanded_width: 0.0,
            expanded_height: 0.0,
            revision: 0,
            fresh: true,
            anchor_center_x: 0.0,
            anchor_top_y: 0.0,
        }
    }
}

#[derive(Default)]
pub struct PresentationState(Mutex<HashMap<String, Presentation>>);

fn expandable_surface(label: &str) -> bool {
    matches!(label, "indicator" | "meeting-controller" | "meeting-notch")
}

/// Both meeting windows are full controllers: the floating/notch controller on
/// the meeting's display and, when that display has no notch, the laptop notch.
/// Only the dictation laptop mirror (`indicator-notch`) stays status-only.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn meeting_surface(label: &str) -> bool {
    matches!(label, "meeting-controller" | "meeting-notch")
}

fn recording_surface(label: &str) -> bool {
    expandable_surface(label) || matches!(label, "indicator-notch" | "meeting-notch")
}

#[tauri::command]
pub fn get_indicator_presentation(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> Result<Presentation, String> {
    if !recording_surface(window.label()) {
        return Err("Not a recording surface".into());
    }
    let state = app.state::<PresentationState>();
    let surfaces = state.0.lock().map_err(|_| "Presentation unavailable")?;
    Ok(surfaces.get(window.label()).copied().unwrap_or_default())
}

#[tauri::command]
pub async fn set_indicator_expanded(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    expanded: bool,
) -> Result<(), String> {
    // The secondary laptop indicator is status-only, even if invoked outside
    // the frontend. It cannot resize/control another recording surface.
    if !expandable_surface(window.label()) {
        return Err("Not an interactive recording surface".into());
    }
    #[cfg(target_os = "macos")]
    {
        let handle = window.ns_window().map_err(|_| "Window unavailable")? as usize;
        let label = window.label().to_owned();
        let (send, receive) = tokio::sync::oneshot::channel();
        window
            .run_on_main_thread(move || {
                let result = unsafe { mac::resize(&app, &label, handle, expanded) };
                let _ = send.send(result);
            })
            .map_err(|_| "Cannot schedule presentation")?;
        receive.await.map_err(|_| "Presentation interrupted")?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, expanded);
        Ok(())
    }
}

/// The rendering view acknowledges its completed close. Only the invoking
/// interactive surface may shrink, and only the same still-closed revision.
#[tauri::command]
pub async fn finish_indicator_collapse(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    revision: u64,
) -> Result<(), String> {
    if !expandable_surface(window.label()) {
        return Err("Not an interactive recording surface".into());
    }
    #[cfg(target_os = "macos")]
    {
        let handle = window.ns_window().map_err(|_| "Window unavailable")? as usize;
        let label = window.label().to_owned();
        let (send, receive) = tokio::sync::oneshot::channel();
        window
            .run_on_main_thread(move || unsafe {
                mac::finish_collapse(&app, &label, handle, revision);
                let _ = send.send(());
            })
            .map_err(|_| "Cannot schedule collapse".to_owned())?;
        receive.await.map_err(|_| "Collapse interrupted".to_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, revision);
        Ok(())
    }
}

/// Reassert the correct level after Space changes without downgrading a notch
/// window to the ordinary floating level below the menu bar.
pub fn refresh_level(app: &tauri::AppHandle, window: &tauri::WebviewWindow) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let handle = window.ns_window()? as usize;
        let label = window.label().to_owned();
        let app = app.clone();
        window.run_on_main_thread(move || unsafe {
            let style = app
                .state::<PresentationState>()
                .0
                .lock()
                .ok()
                .and_then(|surfaces| surfaces.get(&label).map(|p| p.style))
                .unwrap_or_default();
            mac::configure_level(&*(handle as *mut objc2_app_kit::NSWindow), style);
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        window.set_always_on_top(true)
    }
}

#[cfg(target_os = "macos")]
fn publish(app: &tauri::AppHandle, label: &str, mut presentation: Presentation) -> Presentation {
    if let Ok(mut state) = app.state::<PresentationState>().0.lock() {
        presentation.revision = state.get(label).map_or(1, |p| p.revision.saturating_add(1));
        state.insert(label.to_owned(), presentation);
    }
    let _ = app.emit_to(label, "murmur://indicator-presentation", presentation);
    presentation
}

/// Mouse-only outside-click observation exists only while a panel is expanded.
/// Hide paths explicitly dispose it; no keyboard or global scroll observation.
pub fn stop_monitoring(app: &tauri::AppHandle, label: &str) {
    #[cfg(target_os = "macos")]
    if let Some(window) = app.get_webview_window(label) {
        let label = label.to_owned();
        let handle = window.ns_window().ok().map(|handle| handle as usize);
        let _ = window.run_on_main_thread(move || {
            mac::remove_monitors(&label);
            if let Some(handle) = handle {
                mac::return_borrowed_focus(&label, unsafe { &*(handle as *mut objc2_app_kit::NSWindow) });
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, label);
}

#[cfg(target_os = "macos")]
pub(crate) mod mac {
    use super::*;
    use crate::indicator_position::IndicatorPosition;
    use block2::RcBlock;
    use objc2::{
        define_class, msg_send, rc::Retained, runtime::AnyObject, AllocAnyThread, DefinedClass,
        MainThreadMarker, MainThreadOnly,
    };
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationOptions, NSAutoresizingMaskOptions, NSEvent,
        NSEventMask, NSFloatingWindowLevel, NSRunningApplication, NSScreen, NSStatusWindowLevel,
        NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowCollectionBehavior,
        NSWorkspace,
    };
    use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize};
    use std::{cell::RefCell, ptr::NonNull};

    const WING_WIDTH: f64 = 72.0;
    // Transparent canvas around the visible surface: room for the meeting
    // notch's hover growth (+5pt per side, +3pt below) plus its drop shadow
    // (6pt blur, 2pt down). Keep in sync with RecordingSurface.vue.
    const HOVER_SLACK: f64 = 12.0;
    const EDGE_HEADROOM: f64 = 12.0;

    thread_local! {
        static MONITORS: RefCell<HashMap<String, Vec<Retained<AnyObject>>>> = RefCell::new(HashMap::new());
        static HOVER: RefCell<HashMap<String, HoverTracking>> = RefCell::new(HashMap::new());
        /// The app that was frontmost when a scroll-opened meeting panel took
        /// focus, so collapsing or hiding the panel can give it straight back.
        static BORROWED_FOCUS: RefCell<Option<Retained<NSRunningApplication>>> = const { RefCell::new(None) };
    }

    // Only the meeting notches have a hover cue. The dictation indicator is
    // transient, and its laptop mirror ignores the mouse entirely.
    const HOVER_EVENT: &str = "murmur://indicator-hover";

    pub(crate) struct HoverIvars {
        app: tauri::AppHandle,
        label: String,
    }

    // WKWebView only updates CSS :hover while Murmur is the active app, so the
    // notch never reacted when the pointer arrived from Slack, a browser, etc.
    // An NSTrackingArea with ActiveAlways reports enter/exit regardless of
    // which app is active. It only observes this window's own bounds: no
    // global pointer-movement monitoring.
    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "MurmurNotchHoverTracker"]
        #[ivars = HoverIvars]
        pub(crate) struct HoverTracker;

        unsafe impl NSObjectProtocol for HoverTracker {}

        impl HoverTracker {
            #[unsafe(method(mouseEntered:))]
            fn mouse_entered(&self, _event: &NSEvent) {
                self.report(true);
            }

            #[unsafe(method(mouseExited:))]
            fn mouse_exited(&self, _event: &NSEvent) {
                self.report(false);
            }
        }
    );

    impl HoverTracker {
        fn new(mtm: MainThreadMarker, app: tauri::AppHandle, label: String) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(HoverIvars { app, label });
            unsafe { msg_send![super(this), init] }
        }

        fn report(&self, hovered: bool) {
            let ivars = self.ivars();
            let _ = ivars.app.emit_to(ivars.label.as_str(), HOVER_EVENT, hovered);
        }
    }

    struct HoverTracking {
        view: Retained<NSView>,
        area: Retained<NSTrackingArea>,
        tracker: Retained<HoverTracker>,
        webview: Option<(Retained<NSView>, Retained<NSTrackingArea>)>,
    }

    fn remove_hover_tracking(label: &str) {
        if let Some(old) = HOVER.with(|all| all.borrow_mut().remove(label)) {
            old.view.removeTrackingArea(&old.area);
            if let Some((webview, area)) = old.webview {
                webview.removeTrackingArea(&area);
            }
        }
    }

    /// WKWebView's own pointer-movement tracking is ActiveInKeyWindow, so while
    /// another app is active the expanded panel's buttons never show their
    /// hover styles. Feed the same mouseMoved/enter/exit events to the webview
    /// from an ActiveAlways area over its visible rect. (The cursor still cannot
    /// change from a background app; that needs activation, see below.)
    unsafe fn track_webview_pointer(window: &NSWindow) -> Option<(Retained<NSView>, Retained<NSTrackingArea>)> {
        let webview = window.contentView()?.subviews().firstObject()?;
        let owner: &AnyObject = &webview;
        let area = NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(owner),
            None,
        );
        webview.addTrackingArea(&area);
        Some((webview, area))
    }

    /// Clicking the notch activates Murmur (normal AppKit click activation),
    /// which is what gives the expanded buttons a pointing cursor and focus.
    /// Scrolling open does not, so the panel looked inert. When the meeting
    /// panel opens while another app is frontmost, take focus the same way a
    /// click would and remember who had it.
    unsafe fn borrow_focus(label: &str, window: &NSWindow) {
        if !meeting_surface(label) {
            return; // Never take focus from the app a dictation is typing into.
        }
        let app = NSApplication::sharedApplication(MainThreadMarker::new_unchecked());
        if app.isActive() {
            return;
        }
        let previous = NSWorkspace::sharedWorkspace().frontmostApplication();
        BORROWED_FOCUS.with(|slot| *slot.borrow_mut() = previous);
        window.makeKeyAndOrderFront(None);
        // Key and main on the same window, so activation brings only the
        // notch forward and not Murmur's main window.
        window.makeMainWindow();
        app.activate();
    }

    /// Hand focus back to the app the panel borrowed it from, but only if the
    /// user is still on the notch (not if they moved on to Murmur's window).
    pub(super) fn return_borrowed_focus(label: &str, window: &NSWindow) {
        if !meeting_surface(label) {
            return;
        }
        let Some(previous) = BORROWED_FOCUS.with(|slot| slot.borrow_mut().take()) else {
            return;
        };
        let app = NSApplication::sharedApplication(unsafe { MainThreadMarker::new_unchecked() });
        let on_notch = app
            .keyWindow()
            .is_none_or(|key| std::ptr::eq(Retained::as_ptr(&key), window));
        if !app.isActive() || !on_notch || previous.isTerminated() {
            return;
        }
        app.yieldActivationToApplication(&previous);
        previous.activateWithOptions(NSApplicationActivationOptions::empty());
    }

    /// The tracking area follows the window's visible bounds, which include
    /// the transparent headroom, so the cue starts just before the notch edge.
    unsafe fn install_hover_tracking(app: &tauri::AppHandle, label: &str, window: &NSWindow) {
        remove_hover_tracking(label);
        if !meeting_surface(label) {
            return;
        }
        let Some(view) = window.contentView() else {
            return;
        };
        let tracker = HoverTracker::new(
            MainThreadMarker::new_unchecked(),
            app.clone(),
            label.to_owned(),
        );
        let owner: &AnyObject = &tracker;
        let area = NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(owner),
            None,
        );
        view.addTrackingArea(&area);
        let webview = track_webview_pointer(window);
        HOVER.with(|all| {
            all.borrow_mut().insert(
                label.to_owned(),
                HoverTracking { view, area, tracker, webview },
            );
        });
        sync_hover(label, window);
    }

    /// Enter/exit events are not guaranteed when the window moves or resizes
    /// under a stationary pointer, so re-report the truth after every resize.
    fn sync_hover(label: &str, window: &NSWindow) {
        HOVER.with(|all| {
            if let Some(tracking) = all.borrow().get(label) {
                let point = NSEvent::mouseLocation();
                let frame = window.frame();
                let inside = point.x >= frame.origin.x
                    && point.x <= frame.origin.x + frame.size.width
                    && point.y >= frame.origin.y
                    && point.y <= frame.origin.y + frame.size.height;
                tracking.tracker.report(inside && window.isVisible());
            }
        });
    }

    pub(super) fn remove_monitors(label: &str) {
        let monitors = MONITORS.with(|all| all.borrow_mut().remove(label));
        if let Some(monitors) = monitors {
            for monitor in monitors {
                unsafe {
                    NSEvent::removeMonitor(&monitor);
                }
            }
        }
    }

    fn outside_click_mask() -> NSEventMask {
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown
    }

    fn close_is_current(current: Presentation, revision: u64) -> bool {
        current.style == IndicatorStyle::Notch && !current.expanded && current.revision == revision
    }

    fn inside_surface(point: NSPoint, frame: NSRect) -> bool {
        point.x >= frame.origin.x + EDGE_HEADROOM
            && point.x <= frame.origin.x + frame.size.width - EDGE_HEADROOM
            && point.y >= frame.origin.y + HOVER_SLACK
            && point.y <= frame.origin.y + frame.size.height
    }

    unsafe fn dismiss_if_outside(app: &tauri::AppHandle, label: &str) {
        let Some(window) = app.get_webview_window(label) else {
            remove_monitors(label);
            return;
        };
        let Ok(handle) = window.ns_window() else {
            return;
        };
        let native = &*(handle as *mut NSWindow);
        if !native.isVisible() {
            remove_monitors(label);
            return;
        }
        if !inside_surface(NSEvent::mouseLocation(), native.frame()) {
            let _ = resize(app, label, handle as usize, false);
        }
    }

    unsafe fn install_monitors(app: &tauri::AppHandle, label: &str) {
        remove_monitors(label);
        let mask = outside_click_mask();
        let global_app = app.clone();
        let global_label = label.to_owned();
        let global = RcBlock::new(move |_event: NonNull<NSEvent>| {
            dismiss_if_outside(&global_app, &global_label);
        });
        let local_app = app.clone();
        let local_label = label.to_owned();
        let local = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            dismiss_if_outside(&local_app, &local_label);
            event.as_ptr() // Never swallow or redirect the user's outside click.
        });
        let tokens = [
            NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &global),
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &local),
        ]
        .into_iter()
        .flatten()
        .collect();
        MONITORS.with(|all| {
            all.borrow_mut().insert(label.to_owned(), tokens);
        });
    }

    #[derive(Clone, Copy, Debug)]
    struct NotchGeometry {
        center_x: f64,
        width: f64,
        height: f64,
    }

    // AppKit's auxiliary rectangles are in global logical screen coordinates.
    // Reject incomplete geometry rather than putting content behind a camera.
    fn notch_geometry(
        full: NSRect,
        inset: f64,
        left: NSRect,
        right: NSRect,
    ) -> Option<NotchGeometry> {
        let start = left.origin.x + left.size.width;
        let end = right.origin.x;
        if inset <= 0.0
            || left.size.width <= 0.0
            || right.size.width <= 0.0
            || end <= start
            || start < full.origin.x
            || end > full.origin.x + full.size.width
        {
            return None;
        }
        Some(NotchGeometry {
            center_x: (start + end) / 2.0,
            width: end - start,
            height: inset,
        })
    }

    fn floating_size(label: &str) -> NSSize {
        if label == "meeting-controller" {
            NSSize::new(320.0, 46.0)
        } else {
            NSSize::new(360.0, 40.0)
        }
    }

    fn docked_frame(full: NSRect, camera: NotchGeometry, size: NSSize) -> NSRect {
        NSRect::new(
            NSPoint::new(
                camera.center_x - size.width / 2.0,
                full.origin.y + full.size.height - size.height,
            ),
            size,
        )
    }

    // AppKit rounds borderless frame origins. Mixing odd/even canvas widths
    // and recentering from the rounded old frame accumulates half-point drift.
    fn canvas_width(content_width: f64) -> f64 {
        ((content_width + EDGE_HEADROOM * 2.0) / 2.0).ceil() * 2.0
    }

    fn anchored_frame(center_x: f64, top_y: f64, size: NSSize) -> NSRect {
        NSRect::new(
            NSPoint::new(center_x - size.width / 2.0, top_y - size.height),
            size,
        )
    }

    /// The largest canvas a notch surface ever needs: its expanded panel plus
    /// the transparent headroom around it.
    fn notch_canvas(presentation: &Presentation) -> NSSize {
        NSSize::new(
            canvas_width(presentation.expanded_width),
            presentation.expanded_height + HOVER_SLACK,
        )
    }

    /// Where a fixed-size webview sits inside a (smaller) window so that both
    /// share a horizontal center and a top edge. Whole points keep text crisp.
    fn pinned_webview_frame(bounds: NSSize, canvas: NSSize, flipped: bool) -> NSRect {
        let x = ((bounds.width - canvas.width) / 2.0).round();
        let y = if flipped { 0.0 } else { bounds.height - canvas.height };
        NSRect::new(NSPoint::new(x, y), canvas)
    }

    /// WKWebView repaints asynchronously after its window is resized. For
    /// about one frame the window shows its new size while still drawing the
    /// old page pinned to its new top-left corner. A symmetric notch shrink
    /// (416pt -> ~346pt) moved the whole stale frame ~35pt to the right, which
    /// read as a duplicated timer and leading icon at the end of every collapse.
    ///
    /// In notch mode the webview therefore never changes size: it keeps the
    /// expanded canvas and is re-centered inside the window in the same
    /// main-thread turn as the window frame change. The page never re-lays out
    /// and no stale frame can show. Floating mode restores normal autoresizing.
    unsafe fn set_surface_frame(window: &NSWindow, frame: NSRect, canvas: Option<NSSize>) {
        let parent = window.contentView();
        let webview = parent.as_ref().and_then(|parent| parent.subviews().firstObject());
        if let (Some(_), Some(webview)) = (canvas, &webview) {
            // Detach before the frame change so AppKit never sends the page a
            // transient viewport size.
            webview.setAutoresizingMask(NSAutoresizingMaskOptions::ViewNotSizable);
        }
        window.setFrame_display(frame, true);
        let (Some(parent), Some(webview)) = (parent, webview) else {
            return;
        };
        let bounds = parent.bounds();
        match canvas {
            Some(canvas) => webview.setFrame(pinned_webview_frame(
                bounds.size,
                canvas,
                parent.isFlipped(),
            )),
            None => {
                webview.setAutoresizingMask(
                    NSAutoresizingMaskOptions::ViewWidthSizable
                        | NSAutoresizingMaskOptions::ViewHeightSizable,
                );
                webview.setFrame(bounds);
            }
        }
    }

    pub(crate) fn configure_level(window: &NSWindow, style: IndicatorStyle) {
        window.setLevel(if style == IndicatorStyle::Notch {
            NSStatusWindowLevel + 1
        } else {
            NSFloatingWindowLevel
        });
        let behavior =
            window.collectionBehavior() | NSWindowCollectionBehavior::FullScreenAuxiliary;
        window.setCollectionBehavior(behavior);
    }

    /// Called only on AppKit's main thread, after selecting the target screen.
    pub(crate) unsafe fn place(
        app: &tauri::AppHandle,
        label: &str,
        window: &NSWindow,
        screen: &NSScreen,
        anchor: IndicatorPosition,
        requested: IndicatorStyle,
    ) -> Presentation {
        remove_monitors(label);
        let full = screen.frame();
        let camera = if requested == IndicatorStyle::Notch {
            notch_geometry(
                full,
                screen.safeAreaInsets().top,
                screen.auxiliaryTopLeftArea(),
                screen.auxiliaryTopRightArea(),
            )
        } else {
            None
        };
        let (frame, presentation) = if let Some(camera) = camera {
            let compact_width = (camera.width + WING_WIDTH * 2.0).min(full.size.width);
            let size = NSSize::new(canvas_width(compact_width), camera.height + HOVER_SLACK);
            (
                docked_frame(full, camera, size),
                Presentation {
                    style: IndicatorStyle::Notch,
                    expanded: false,
                    compact_width,
                    notch_width: camera.width,
                    notch_height: camera.height,
                    expanded_width: 400.0_f64.max(compact_width),
                    expanded_height: camera.height
                        + if meeting_surface(label) {
                            144.0
                        } else {
                            64.0
                        },
                    revision: 0,
                    fresh: true,
                    anchor_center_x: camera.center_x,
                    anchor_top_y: full.origin.y + full.size.height,
                },
            )
        } else {
            let work = screen.visibleFrame();
            let size = floating_size(label);
            let (x, y) = anchor.offsets(work.size.width, work.size.height, size.width, size.height);
            (
                NSRect::new(NSPoint::new(work.origin.x + x, work.origin.y + y), size),
                Presentation::default(),
            )
        };
        let canvas = (presentation.style == IndicatorStyle::Notch).then(|| notch_canvas(&presentation));
        set_surface_frame(window, frame, canvas);
        configure_level(window, presentation.style);
        let published = publish(app, label, presentation);
        if published.style == IndicatorStyle::Notch {
            install_hover_tracking(app, label, window);
        } else {
            remove_hover_tracking(label);
        }
        published
    }

    pub(super) unsafe fn finish_collapse(
        app: &tauri::AppHandle,
        label: &str,
        handle: usize,
        revision: u64,
    ) {
        let current = app
            .state::<PresentationState>()
            .0
            .lock()
            .ok()
            .and_then(|all| all.get(label).copied());
        if let Some(current) = current {
            let native = &*(handle as *mut NSWindow);
            if close_is_current(current, revision) && native.isVisible() {
                let size = NSSize::new(
                    canvas_width(current.compact_width),
                    current.notch_height + HOVER_SLACK,
                );
                if native.frame().size != size {
                    set_surface_frame(
                        native,
                        anchored_frame(current.anchor_center_x, current.anchor_top_y, size),
                        Some(notch_canvas(&current)),
                    );
                }
                sync_hover(label, native);
            }
        }
    }

    pub(super) unsafe fn resize(
        app: &tauri::AppHandle,
        label: &str,
        handle: usize,
        expanded: bool,
    ) -> Result<(), String> {
        let mut presentation = {
            let state = app.state::<PresentationState>();
            let surfaces = state.0.lock().map_err(|_| "Presentation unavailable")?;
            surfaces.get(label).copied().unwrap_or_default()
        };
        if presentation.style != IndicatorStyle::Notch || presentation.expanded == expanded {
            return Ok(());
        }
        let window = &*(handle as *mut NSWindow);
        presentation.expanded = expanded;
        presentation.fresh = false;
        if expanded {
            let size = notch_canvas(&presentation);
            set_surface_frame(
                window,
                anchored_frame(
                    presentation.anchor_center_x,
                    presentation.anchor_top_y,
                    size,
                ),
                Some(size),
            );
            publish(app, label, presentation);
            install_monitors(app, label);
            borrow_focus(label, window);
        } else {
            remove_monitors(label);
            return_borrowed_focus(label, window);
            let published = publish(app, label, presentation);
            // Normally the view acknowledges transition completion. Keep only
            // a failsafe for a lost/dead view; never race a 200ms CSS transition
            // against a 220ms timer started before delivery to the renderer.
            let app = app.clone();
            let label = label.to_owned();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                let Some(window) = app.get_webview_window(&label) else {
                    return;
                };
                let Ok(handle) = window.ns_window() else {
                    return;
                };
                let handle = handle as usize;
                let _ = window.run_on_main_thread(move || {
                    finish_collapse(&app, &label, handle, published.revision);
                });
            });
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn actual_macbook_camera_has_two_clear_wings_at_the_physical_top() {
            let full = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1512.0, 982.0));
            let camera = notch_geometry(
                full,
                32.0,
                NSRect::new(NSPoint::new(0.0, 950.0), NSSize::new(663.5, 32.0)),
                NSRect::new(NSPoint::new(848.5, 950.0), NSSize::new(663.5, 32.0)),
            )
            .unwrap();
            assert_eq!(camera.width, 185.0);
            let compact = docked_frame(
                full,
                camera,
                NSSize::new(
                    canvas_width(camera.width + WING_WIDTH * 2.0),
                    camera.height + HOVER_SLACK,
                ),
            );
            let canvas_margin = (compact.size.width - camera.width - WING_WIDTH * 2.0) / 2.0;
            assert_eq!(compact.origin.x + canvas_margin + WING_WIDTH, 663.5);
            assert_eq!(
                compact.origin.x + canvas_margin + WING_WIDTH + camera.width,
                848.5
            );
            assert_eq!(compact.origin.y + compact.size.height, 982.0);
            let expanded = anchored_frame(
                camera.center_x,
                full.origin.y + full.size.height,
                NSSize::new(canvas_width(400.0), 200.0 + HOVER_SLACK),
            );
            assert_eq!(expanded.origin.x + expanded.size.width / 2.0, 756.0);
            assert_eq!(expanded.origin.y + expanded.size.height, 982.0);
            assert_eq!(
                anchored_frame(
                    camera.center_x,
                    full.origin.y + full.size.height,
                    compact.size
                ),
                compact
            );
        }

        #[test]
        fn geometry_uses_global_coordinates_on_offset_displays() {
            let full = NSRect::new(NSPoint::new(-1512.0, 900.0), NSSize::new(1512.0, 982.0));
            let camera = notch_geometry(
                full,
                32.0,
                NSRect::new(NSPoint::new(-1512.0, 1850.0), NSSize::new(663.5, 32.0)),
                NSRect::new(NSPoint::new(-663.5, 1850.0), NSSize::new(663.5, 32.0)),
            )
            .unwrap();
            let frame = docked_frame(full, camera, NSSize::new(canvas_width(329.0), 40.0));
            assert_eq!(frame.origin.x + frame.size.width / 2.0, -756.0);
            assert_eq!(frame.origin.y + frame.size.height, 1882.0);
        }

        #[test]
        fn repeated_open_close_never_accumulates_appkit_rounding_drift() {
            for (center, top) in [(756.0, 982.0), (-756.0, 1882.0)] {
                for _ in 0..128 {
                    for (width, height) in [(400.0, 208.0), (329.0, 40.0)] {
                        let frame =
                            anchored_frame(center, top, NSSize::new(canvas_width(width), height));
                        let rounded = NSRect::new(
                            NSPoint::new(frame.origin.x.floor(), frame.origin.y.floor()),
                            frame.size,
                        );
                        assert_eq!(rounded.origin.x + rounded.size.width / 2.0, center);
                        assert_eq!(rounded.origin.y + rounded.size.height, top);
                    }
                }
            }
        }

        #[test]
        fn pinned_webview_keeps_one_center_and_top_edge_through_collapse() {
            // A collapse must not change the page's viewport: the webview keeps
            // the expanded canvas and only its offset inside the window moves.
            let canvas = NSSize::new(canvas_width(400.0), 176.0 + HOVER_SLACK);
            for window in [canvas, NSSize::new(canvas_width(329.0), 32.0 + HOVER_SLACK)] {
                for flipped in [false, true] {
                    let frame = pinned_webview_frame(window, canvas, flipped);
                    assert_eq!(frame.size, canvas);
                    assert_eq!(frame.origin.x + canvas.width / 2.0, window.width / 2.0);
                    assert_eq!(frame.origin.x, frame.origin.x.round());
                    let top = if flipped { frame.origin.y } else { frame.origin.y + canvas.height };
                    assert_eq!(top, if flipped { 0.0 } else { window.height });
                }
            }
        }

        #[test]
        fn delayed_close_cannot_resize_a_reopened_or_new_surface() {
            let closed = Presentation {
                style: IndicatorStyle::Notch,
                revision: 4,
                expanded: false,
                ..Presentation::default()
            };
            assert!(close_is_current(closed, 4));
            assert!(!close_is_current(closed, 3));
            assert!(!close_is_current(
                Presentation {
                    expanded: true,
                    ..closed
                },
                4
            ));
            assert!(!close_is_current(
                Presentation {
                    style: IndicatorStyle::Floating,
                    ..closed
                },
                4
            ));
        }

        #[test]
        fn outside_clicks_exclude_transparent_animation_headroom() {
            let frame = NSRect::new(NSPoint::new(-800.0, 700.0), NSSize::new(416.0, 208.0));
            assert!(inside_surface(NSPoint::new(-780.0, 800.0), frame));
            assert!(!inside_surface(NSPoint::new(-799.0, 800.0), frame));
            assert!(!inside_surface(NSPoint::new(-780.0, 701.0), frame));
            assert!(!inside_surface(NSPoint::new(2000.0, 800.0), frame));
            let mask = outside_click_mask();
            assert!(mask.contains(
                NSEventMask::LeftMouseDown
                    | NSEventMask::RightMouseDown
                    | NSEventMask::OtherMouseDown
            ));
            assert!(!mask.intersects(
                NSEventMask::KeyDown
                    | NSEventMask::KeyUp
                    | NSEventMask::ScrollWheel
                    | NSEventMask::MouseMoved
            ));
        }

        #[test]
        fn incomplete_or_notchless_geometry_never_creates_a_camera_overlay() {
            let full = NSRect::new(NSPoint::new(1512.0, -360.0), NSSize::new(3440.0, 1440.0));
            let zero = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            assert!(notch_geometry(full, 0.0, zero, zero).is_none());
            assert!(notch_geometry(full, 32.0, zero, zero).is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dictation_mirror_is_read_only_and_only_recording_windows_can_read_presentation() {
        assert!(recording_surface("indicator-notch"));
        assert!(!expandable_surface("indicator-notch"));
        assert!(!meeting_surface("indicator-notch"));
        // The laptop meeting notch must stay usable when the meeting started
        // on an external display.
        for label in ["indicator", "meeting-controller", "meeting-notch"] {
            assert!(expandable_surface(label));
            assert!(recording_surface(label));
        }
        assert!(meeting_surface("meeting-controller") && meeting_surface("meeting-notch"));
        assert!(!meeting_surface("indicator"));
        for label in ["main", "meeting-reminder", "", "other"] {
            assert!(!recording_surface(label));
        }
        assert_eq!(Presentation::default().style, IndicatorStyle::Floating);
    }
}
