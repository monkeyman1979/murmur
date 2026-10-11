//! Global hotkey listener — Phase 2 with unified gesture activation.
//!
//! Direct `CGEventTap` via raw FFI. We previously used `rdev`, but its
//! per-event keycode-to-string translation calls `TSMGetInputSourceProperty`,
//! which on macOS 26+ hard-asserts it is running on the main dispatch queue
//! and traps the process (SIGTRAP) when called from the event-tap thread.
//! We only need keycodes, not typed characters, so we skip the translation
//! layer entirely.
//!
//! Activation is gesture-driven: the same binding supports hold-to-talk AND
//! double-tap-toggle, and an activation state machine decides which based
//! on how the user presses the key:
//!
//! - **Hold past `HOLD_THRESHOLD`** → hold-to-talk. Recording starts when
//!   the threshold is crossed; key-up fires Released.
//! - **Two taps within `DOUBLE_TAP_WINDOW`** → toggle-lock. Pressed fires
//!   on the second tap; the next tap fires Released.
//! - **Single short tap** → filtered out (accidental taps don't open the mic).
//!
//! The state machine runs on a dedicated worker thread so it can wait on
//! deadlines without blocking the CoreGraphics event tap. Pure state
//! transitions live in `step_activation` / `step_timeout` for
//! unit-testing without a live event loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// User-configurable binding. The event tap dispatches against this enum;
/// the `kind` tag on the wire format matches the TypeScript composable's
/// `HotkeyBinding` discriminant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HotkeyBinding {
    /// A bare modifier key (e.g. Right ⌘, Left ⌥). Detected by watching
    /// `FLAGS_CHANGED` for the specific side-aware device mask bit.
    ModifierOnly { device_mask: u64 },
    /// A non-modifier keycode that fires when pressed with the required
    /// modifier-flag set (e.g. Ctrl+Shift+Space). Detected on
    /// `KEY_DOWN`; released on `KEY_UP` of the same keycode or on a
    /// `FLAGS_CHANGED` that drops one of the required flags.
    KeyCombo { keycode: i64, required_flags: u64 },
}

impl Default for HotkeyBinding {
    /// Right ⌘ — matches pre-v2 default, no regression for existing users.
    fn default() -> Self {
        Self::ModifierOnly {
            device_mask: NX_DEVICERCMDKEYMASK,
        }
    }
}

/// Side-specific bits from IOKit's `ev_keymap.h`. These ride along in
/// `CGEventFlags` and tell us *which side* of a modifier is down — plain
/// `kCGEventFlagMaskCommand` alone can't. Exported so the frontend and
/// tests share a single source of truth.
pub const NX_DEVICELCTLKEYMASK: u64 = 0x0000_0001;
pub const NX_DEVICELSHIFTKEYMASK: u64 = 0x0000_0002;
pub const NX_DEVICERSHIFTKEYMASK: u64 = 0x0000_0004;
pub const NX_DEVICELCMDKEYMASK: u64 = 0x0000_0008;
pub const NX_DEVICERCMDKEYMASK: u64 = 0x0000_0010;
pub const NX_DEVICELALTKEYMASK: u64 = 0x0000_0020;
pub const NX_DEVICERALTKEYMASK: u64 = 0x0000_0040;
pub const NX_DEVICERCTLKEYMASK: u64 = 0x0000_2000;

/// Rust-side state that the event tap reads on every callback and the
/// Tauri commands write through. Held as an `Arc<HotkeyState>` and
/// `.manage()`'d on the `App` so the command handlers can clone in.
pub struct HotkeyState {
    pub binding: RwLock<HotkeyBinding>,
}

impl Default for HotkeyState {
    fn default() -> Self {
        Self {
            binding: RwLock::new(HotkeyBinding::default()),
        }
    }
}

/// Snapshot shape returned from `get_hotkey_config` — used by the
/// frontend to hydrate from a fresh install or to sanity-check a
/// potentially-corrupt localStorage entry.
#[derive(Debug, Clone, Serialize)]
pub struct HotkeyConfig {
    pub binding: HotkeyBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationMode {
    Hold,
    Toggle,
}

impl ActivationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hold => "hold",
            Self::Toggle => "toggle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    BindingState(bool),
    Pressed(ActivationMode),
    Released,
    /// User pressed Esc while a capture was live — discard it. In
    /// hold-to-talk this fires while the key is still held; in
    /// toggle-lock it fires while the lock is active. In both cases the
    /// transcription pipeline should skip the current capture.
    Cancelled,
}

#[tauri::command]
pub fn set_hotkey_binding(state: tauri::State<'_, Arc<HotkeyState>>, binding: HotkeyBinding) {
    *state.binding.write() = binding.clone();
    crate::murmur_dev_log!("[murmur] hotkey binding set to {:?}", binding);
}

#[tauri::command]
pub fn get_hotkey_config(state: tauri::State<'_, Arc<HotkeyState>>) -> HotkeyConfig {
    HotkeyConfig {
        binding: state.binding.read().clone(),
    }
}

// Carbon virtual keycode for Escape. Exported so the tap module and the
// tests share it; frontend mirrors this in `src/lib/hotkey.ts`.
pub const KEYCODE_ESCAPE: i64 = 53;

/// How long the binding must be held after key-down before hold-to-talk
/// commits. Also the filter that suppresses accidental single taps from
/// opening the mic.
pub const HOLD_THRESHOLD: Duration = Duration::from_millis(250);

/// Max gap between the first tap's release and the second tap's press
/// for a double-tap to register as toggle-lock. Matches macOS-style
/// double-click thresholds.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(350);

/// Signals the event tap sends to the activation worker. The worker
/// owns the gesture state machine so the tap callback stays lightweight
/// and doesn't need to wait on deadlines.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivationSignal {
    KeyDown,
    KeyUp,
    EscCancel,
    /// The user changed their binding while a recording was in
    /// progress — flush the state machine so we don't leak a
    /// "recording" that no key can stop.
    Rebind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ActivationState {
    Idle,
    /// Key is down; not yet known whether this will be a hold or a tap.
    /// `had_prior_tap` means a tap preceded this press within
    /// `DOUBLE_TAP_WINDOW` — if key-up comes before `HOLD_THRESHOLD`,
    /// this resolves as the second tap of a double-tap.
    Pending {
        down_at: Instant,
        had_prior_tap: bool,
    },
    /// Hold-to-talk committed; recording until key-up.
    HoldActive,
    /// A short tap completed. Waiting on `DOUBLE_TAP_WINDOW` for a
    /// potential second tap before dropping this one.
    AwaitingSecondTap {
        last_tap_at: Instant,
    },
    /// Double-tap lock engaged; recording until the next key-down.
    ToggleLocked,
}

/// Deadline (if any) the worker should wait on before forcing a
/// timeout-driven transition.
pub(crate) fn next_deadline(state: &ActivationState) -> Option<Instant> {
    match *state {
        ActivationState::Pending { down_at, .. } => Some(down_at + HOLD_THRESHOLD),
        ActivationState::AwaitingSecondTap { last_tap_at } => Some(last_tap_at + DOUBLE_TAP_WINDOW),
        _ => None,
    }
}

/// Pure state transition for an incoming signal. Separated so tests can
/// drive it without threads. Fires events through `on_event` as a
/// side-effect before returning the new state.
pub(crate) fn step_activation(
    state: ActivationState,
    signal: ActivationSignal,
    now: Instant,
    on_event: &mut impl FnMut(HotkeyEvent),
) -> ActivationState {
    use ActivationSignal as S;
    use ActivationState as St;
    match (state, signal) {
        // Fresh press — start watching for hold vs tap.
        (St::Idle, S::KeyDown) => St::Pending {
            down_at: now,
            had_prior_tap: false,
        },
        // Press on the heels of a recent short tap — could resolve as
        // the second tap of a double-tap (quick release) or as a hold
        // (release after `HOLD_THRESHOLD`).
        (St::AwaitingSecondTap { .. }, S::KeyDown) => St::Pending {
            down_at: now,
            had_prior_tap: true,
        },
        // Tap while toggle-lock is engaged — stop recording.
        (St::ToggleLocked, S::KeyDown) => {
            on_event(HotkeyEvent::Released);
            St::Idle
        }
        // Key-repeat or stray KeyDown while already holding / pending —
        // ignore so we don't fire duplicate Presseds.
        (St::Pending { .. } | St::HoldActive, S::KeyDown) => state,

        // Release while holding — fire Released, go idle.
        (St::HoldActive, S::KeyUp) => {
            on_event(HotkeyEvent::Released);
            St::Idle
        }
        // Release before hold threshold on a first press — candidate tap;
        // wait to see if a second tap follows.
        (
            St::Pending {
                had_prior_tap: false,
                ..
            },
            S::KeyUp,
        ) => St::AwaitingSecondTap { last_tap_at: now },
        // Release before hold threshold on a second press — that's a
        // full double-tap. Fire Pressed and engage toggle-lock.
        (
            St::Pending {
                had_prior_tap: true,
                ..
            },
            S::KeyUp,
        ) => {
            on_event(HotkeyEvent::Pressed(ActivationMode::Toggle));
            St::ToggleLocked
        }
        // KeyUp in states where we haven't registered a live press.
        (St::Idle | St::AwaitingSecondTap { .. } | St::ToggleLocked, S::KeyUp) => state,

        // Esc while a capture is live — cancel it.
        (St::HoldActive | St::ToggleLocked, S::EscCancel) => {
            on_event(HotkeyEvent::Cancelled);
            St::Idle
        }
        // Esc otherwise — reset, nothing to cancel.
        (_, S::EscCancel) => St::Idle,

        // Rebind while recording — cancel the orphaned capture.
        (St::HoldActive | St::ToggleLocked, S::Rebind) => {
            on_event(HotkeyEvent::Cancelled);
            St::Idle
        }
        // Rebind otherwise — drop any pending state.
        (_, S::Rebind) => St::Idle,
    }
}

/// Pure timeout transition — invoked when the worker's recv times out
/// at the current state's deadline.
pub(crate) fn step_timeout(
    state: ActivationState,
    on_event: &mut impl FnMut(HotkeyEvent),
) -> ActivationState {
    match state {
        // Held past threshold — commit to hold-to-talk.
        ActivationState::Pending { .. } => {
            on_event(HotkeyEvent::Pressed(ActivationMode::Hold));
            ActivationState::HoldActive
        }
        // Single tap wasn't followed — drop.
        ActivationState::AwaitingSecondTap { .. } => ActivationState::Idle,
        _ => state,
    }
}

/// Content-free physical feedback; unrelated keys never reach this layer.
fn binding_feedback(pressed: &mut bool, signal: ActivationSignal) -> Option<HotkeyEvent> {
    let next = matches!(signal, ActivationSignal::KeyDown);
    if next == *pressed {
        return None;
    }
    *pressed = next;
    Some(HotkeyEvent::BindingState(next))
}

/// Dedicated worker that owns the activation state machine. Runs until
/// the event-tap side drops its `Sender` (i.e. process exit).
fn activation_loop(rx: mpsc::Receiver<ActivationSignal>, mut on_event: impl FnMut(HotkeyEvent)) {
    let mut state = ActivationState::Idle;
    let mut pressed = false;
    loop {
        let recv_result = match next_deadline(&state) {
            Some(deadline) => {
                let now = Instant::now();
                if deadline <= now {
                    Err(mpsc::RecvTimeoutError::Timeout)
                } else {
                    rx.recv_timeout(deadline - now)
                }
            }
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };

        match recv_result {
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(event) = binding_feedback(&mut pressed, ActivationSignal::Rebind) {
                    on_event(event);
                }
                let _ = step_activation(
                    state,
                    ActivationSignal::Rebind,
                    Instant::now(),
                    &mut on_event,
                );
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                state = step_timeout(state, &mut on_event);
            }
            Ok(sig) => {
                if let Some(event) = binding_feedback(&mut pressed, sig) {
                    on_event(event);
                }
                state = step_activation(state, sig, Instant::now(), &mut on_event);
            }
        }
    }
}

/// Spawn the event-tap and activation threads. The closure is invoked
/// on every gesture-decided press/release and on Esc while a capture is
/// live. The current binding is read from `state` on every tap callback
/// so UI updates take effect without recreating the tap.
pub fn spawn<F>(state: Arc<HotkeyState>, on_event: F)
where
    F: FnMut(HotkeyEvent) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<ActivationSignal>();

    thread::Builder::new()
        .name("murmur-hotkey-activation".into())
        .spawn(move || activation_loop(rx, on_event))
        .expect("spawn activation thread");

    thread::Builder::new()
        .name("murmur-hotkey".into())
        .spawn(move || {
            #[cfg(target_os = "macos")]
            mac::run(state, tx);
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (state, tx);
                crate::murmur_dev_log!("[murmur] hotkey listener is macOS-only for now");
            }
        })
        .expect("spawn hotkey thread");
}

/// Whether the permanent keyboard tap is installed and enabled. Updated on
/// the owning run-loop thread, including disable notifications and a periodic
/// native enabled-state check. This supplies live readiness when the macOS 27
/// preflight still reports its earlier, cached answer.
static LISTENER_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn listener_active() -> bool {
    LISTENER_ACTIVE.load(Ordering::Relaxed)
}

/// Whether to attempt the permanent tap. On macOS 27, TCC resolves event
/// listening through its composition chain (ListenEvent → PostEvent →
/// Accessibility), so an Accessibility grant alone authorizes a listen-only
/// keyboard tap. Never attempt with neither grant: an unpermissioned attempt
/// can open System Settings before onboarding is visible.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn should_attempt_tap(accessibility_trusted: bool, listen_preflight: bool) -> bool {
    accessibility_trusted || listen_preflight
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{ActivationSignal, HotkeyBinding, HotkeyState, KEYCODE_ESCAPE};
    use std::os::raw::c_void;
    use std::ptr;
    use std::sync::mpsc;
    use std::sync::Arc;

    // CGEventType values we care about.
    const KCG_EVENT_KEY_DOWN: u32 = 10;
    const KCG_EVENT_KEY_UP: u32 = 11;
    const KCG_EVENT_FLAGS_CHANGED: u32 = 12;
    const KCG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    const KCG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

    // CGEventTap placement / options. The Xcode 27 CGEvent.h specifies that
    // kCGHIDEventTap (0) requires root; a global keyboard listener running
    // as the signed app's normal user must use kCGSessionEventTap (1).
    const KCG_SESSION_EVENT_TAP: u32 = 1;
    const KCG_HEAD_INSERT_EVENT_TAP: u32 = 0;
    const KCG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
    const LISTEN_EVENT_MASK: u64 = (1u64 << KCG_EVENT_KEY_DOWN)
        | (1u64 << KCG_EVENT_KEY_UP)
        | (1u64 << KCG_EVENT_FLAGS_CHANGED);
    const TAP_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

    // Keyboard event field id for the virtual keycode.
    const KCG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

    #[allow(non_camel_case_types)]
    type CFTypeRef = *const c_void;
    #[allow(non_camel_case_types)]
    type CFAllocatorRef = *const c_void;
    #[allow(non_camel_case_types)]
    type CFMachPortRef = *mut c_void;
    #[allow(non_camel_case_types)]
    type CFRunLoopRef = *mut c_void;
    #[allow(non_camel_case_types)]
    type CFRunLoopSourceRef = *mut c_void;
    #[allow(non_camel_case_types)]
    type CFStringRef = *const c_void;
    #[allow(non_camel_case_types)]
    type CFIndex = isize;
    #[allow(non_camel_case_types)]
    type CGEventTapProxy = *mut c_void;
    #[allow(non_camel_case_types)]
    type CGEventRef = *mut c_void;

    type CGEventTapCallBack = unsafe extern "C" fn(
        proxy: CGEventTapProxy,
        etype: u32,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopCommonModes: CFStringRef;
        static kCFRunLoopDefaultMode: CFStringRef;

        fn CFMachPortCreateRunLoopSource(
            alloc: CFAllocatorRef,
            port: CFMachPortRef,
            order: CFIndex,
        ) -> CFRunLoopSourceRef;

        fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        fn CFRunLoopAddSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: CFStringRef);
        fn CFRunLoopRemoveSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: CFStringRef);
        fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: u8) -> i32;
        fn CFRelease(x: CFTypeRef);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: CGEventTapCallBack,
            user_info: *mut c_void,
        ) -> CFMachPortRef;

        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
        fn CGEventTapIsEnabled(tap: CFMachPortRef) -> bool;
        fn CGEventGetFlags(event: CGEventRef) -> u64;
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    }

    /// Mutable state owned by the tap thread. Everything here is
    /// derived from the current event stream — the activation state
    /// machine lives in the worker thread and is reached via
    /// `signal_tx`.
    struct Ctx {
        signal_tx: mpsc::Sender<ActivationSignal>,
        tap_port: CFMachPortRef,
        state: Arc<HotkeyState>,
        /// Last binding we observed. When it changes we flush via a
        /// `Rebind` signal so a stuck-in-the-middle rebind doesn't
        /// leave the worker in `HoldActive` for a key nobody's
        /// holding.
        last_binding: HotkeyBinding,
        /// Physical state of the configured binding. Drives edge
        /// detection in `detect_edge` and gets cleared on rebind.
        binding_down: bool,
    }

    pub(super) fn run(state: Arc<HotkeyState>, tx: mpsc::Sender<ActivationSignal>) {
        // Do not create a tap without a grant: an unpermissioned attempt can
        // open System Settings before onboarding is visible. Accessibility is
        // polled live, so the tap starts as soon as the user grants it, with
        // no relaunch.
        if !hotkey_permissions_ready() {
            crate::murmur_dev_log!("[murmur] hotkey: waiting for Accessibility");
            while !hotkey_permissions_ready() {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            crate::murmur_dev_log!("[murmur] hotkey: permission granted — creating tap");
        }

        let initial_binding = state.binding.read().clone();
        let ctx = Box::new(Ctx {
            signal_tx: tx,
            tap_port: ptr::null_mut(),
            state,
            last_binding: initial_binding,
            binding_down: false,
        });
        // Hand raw ownership to the event tap for the process lifetime.
        let ctx_ptr = Box::into_raw(ctx);

        // Listen for key down, key up, and flags changed. KEY_UP is
        // needed to detect chord releases for `KeyCombo` bindings;
        // `ModifierOnly` drives off FLAGS_CHANGED. A null tap is retried
        // rather than ending the thread, so a grant that lands a moment
        // after the check (or a revoke and re-grant) doesn't leave the
        // shortcut dead until Murmur restarts.
        let mut logged_null = false;
        let tap_port = loop {
            let port = unsafe {
                CGEventTapCreate(
                    KCG_SESSION_EVENT_TAP,
                    KCG_HEAD_INSERT_EVENT_TAP,
                    KCG_EVENT_TAP_OPTION_LISTEN_ONLY,
                    LISTEN_EVENT_MASK,
                    tap_callback,
                    ctx_ptr as *mut c_void,
                )
            };
            if !port.is_null() {
                break port;
            }
            if !logged_null {
                crate::murmur_dev_log!("[murmur] CGEventTapCreate returned null — retrying");
                logged_null = true;
            }
            std::thread::sleep(TAP_RETRY_INTERVAL);
            while !hotkey_permissions_ready() {
                std::thread::sleep(TAP_RETRY_INTERVAL);
            }
        };

        // Stash the port so the callback can re-enable after a tap-disable.
        unsafe { (*ctx_ptr).tap_port = tap_port };

        unsafe {
            let source = CFMachPortCreateRunLoopSource(ptr::null(), tap_port, 0);
            if source.is_null() {
                crate::murmur_dev_log!("[murmur] CFMachPortCreateRunLoopSource returned null");
                CFRelease(tap_port as CFTypeRef);
                let _ = Box::from_raw(ctx_ptr);
                return;
            }
            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap_port, true);
            super::LISTENER_ACTIVE.store(CGEventTapIsEnabled(tap_port), super::Ordering::Relaxed);

            loop {
                // Process callbacks normally, but wake once per second to
                // detect a disabled tap even when no keys are arriving.
                let result = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, 0);
                if result == 1 || result == 2 {
                    // kCFRunLoopRunFinished / kCFRunLoopRunStopped.
                    break;
                }
                if !CGEventTapIsEnabled(tap_port) && hotkey_permissions_ready() {
                    CGEventTapEnable(tap_port, true);
                }
                let enabled = CGEventTapIsEnabled(tap_port);
                if super::LISTENER_ACTIVE.swap(enabled, super::Ordering::Relaxed) && !enabled {
                    let _ = (*ctx_ptr).signal_tx.send(ActivationSignal::Rebind);
                }
            }

            super::LISTENER_ACTIVE.store(false, super::Ordering::Relaxed);
            let _ = (*ctx_ptr).signal_tx.send(ActivationSignal::Rebind);
            CFRunLoopRemoveSource(run_loop, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap_port, false);
            CFRelease(source as CFTypeRef);
            CFRelease(tap_port as CFTypeRef);
            let _ = Box::from_raw(ctx_ptr);
        }
    }

    fn hotkey_permissions_ready() -> bool {
        super::should_attempt_tap(
            crate::insertion::is_trusted(),
            crate::permissions::listen_event_preflight(),
        )
    }

    /// What edge (if any) the current event represents for the
    /// configured binding.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BindingEdge {
        None,
        Press,
        Release,
    }

    fn detect_edge(
        binding: &HotkeyBinding,
        etype: u32,
        keycode: i64,
        flags: u64,
        binding_down: bool,
    ) -> BindingEdge {
        match binding {
            HotkeyBinding::ModifierOnly { device_mask } => {
                if etype != KCG_EVENT_FLAGS_CHANGED {
                    return BindingEdge::None;
                }
                let down = (flags & device_mask) != 0;
                match (binding_down, down) {
                    (false, true) => BindingEdge::Press,
                    (true, false) => BindingEdge::Release,
                    _ => BindingEdge::None,
                }
            }
            HotkeyBinding::KeyCombo {
                keycode: target,
                required_flags,
            } => match etype {
                KCG_EVENT_KEY_DOWN
                    if keycode == *target
                        && (flags & required_flags) == *required_flags
                        && !binding_down =>
                {
                    BindingEdge::Press
                }
                KCG_EVENT_KEY_UP if keycode == *target && binding_down => BindingEdge::Release,
                KCG_EVENT_FLAGS_CHANGED
                    if binding_down && (flags & required_flags) != *required_flags =>
                {
                    BindingEdge::Release
                }
                _ => BindingEdge::None,
            },
        }
    }

    unsafe extern "C" fn tap_callback(
        _proxy: CGEventTapProxy,
        etype: u32,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef {
        let ctx = &mut *(user_info as *mut Ctx);

        if etype == KCG_EVENT_TAP_DISABLED_BY_TIMEOUT
            || etype == KCG_EVENT_TAP_DISABLED_BY_USER_INPUT
        {
            crate::murmur_dev_log!("[murmur] event tap disabled (code {etype:#x}), re-enabling");
            super::LISTENER_ACTIVE.store(false, super::Ordering::Relaxed);
            ctx.binding_down = false;
            let _ = ctx.signal_tx.send(ActivationSignal::Rebind);
            if hotkey_permissions_ready() {
                CGEventTapEnable(ctx.tap_port, true);
            }
            super::LISTENER_ACTIVE
                .store(CGEventTapIsEnabled(ctx.tap_port), super::Ordering::Relaxed);
            return event;
        }

        let binding = ctx.state.binding.read().clone();

        // Rebind flush: if the user changed the binding while the
        // worker was in HoldActive / ToggleLocked, we need to tell it
        // to cancel the orphaned capture. Also clear our own
        // `binding_down` so edge detection against the new binding
        // starts fresh.
        if binding != ctx.last_binding {
            ctx.last_binding = binding.clone();
            ctx.binding_down = false;
            let _ = ctx.signal_tx.send(ActivationSignal::Rebind);
        }

        let keycode = CGEventGetIntegerValueField(event, KCG_KEYBOARD_EVENT_KEYCODE);
        let flags = CGEventGetFlags(event);

        let edge = detect_edge(&binding, etype, keycode, flags, ctx.binding_down);

        match edge {
            BindingEdge::Press => {
                ctx.binding_down = true;
                let _ = ctx.signal_tx.send(ActivationSignal::KeyDown);
            }
            BindingEdge::Release => {
                ctx.binding_down = false;
                let _ = ctx.signal_tx.send(ActivationSignal::KeyUp);
            }
            BindingEdge::None => {}
        }

        // Esc-to-cancel — runs independently of the binding edge so a
        // user with a non-Esc binding can abort a live capture by
        // tapping Esc. The worker only fires Cancelled when we're
        // actually recording, so tapping Esc in Idle is a no-op.
        if etype == KCG_EVENT_KEY_DOWN && keycode == KEYCODE_ESCAPE && edge == BindingEdge::None {
            let _ = ctx.signal_tx.send(ActivationSignal::EscCancel);
        }

        event
    }

    #[cfg(test)]
    mod edge_tests {
        use super::*;

        #[test]
        fn session_listen_only_tap_matches_sdk_and_hotkey_mask() {
            // The installed macOS SDK declares session location 1 and
            // listen-only option 1; location 0 (HID) requires root.
            assert_eq!(KCG_SESSION_EVENT_TAP, 1);
            assert_eq!(KCG_EVENT_TAP_OPTION_LISTEN_ONLY, 1);
            assert_eq!(
                LISTEN_EVENT_MASK,
                (1u64 << KCG_EVENT_KEY_DOWN)
                    | (1u64 << KCG_EVENT_KEY_UP)
                    | (1u64 << KCG_EVENT_FLAGS_CHANGED)
            );
            let _: CGEventTapCallBack = tap_callback;
            let _: unsafe extern "C" fn(CFMachPortRef) -> bool = CGEventTapIsEnabled;
            // CoreFoundation Boolean is UInt8, not an ObjC BOOL.
            let _: unsafe extern "C" fn(CFStringRef, f64, u8) -> i32 = CFRunLoopRunInMode;
        }

        #[test]
        fn modifier_only_down_up_and_unrelated_keys() {
            let binding = HotkeyBinding::default();
            assert_eq!(
                detect_edge(
                    &binding,
                    KCG_EVENT_FLAGS_CHANGED,
                    54,
                    super::super::NX_DEVICERCMDKEYMASK,
                    false
                ),
                BindingEdge::Press
            );
            assert_eq!(
                detect_edge(&binding, KCG_EVENT_FLAGS_CHANGED, 54, 0, true),
                BindingEdge::Release
            );
            assert_eq!(
                detect_edge(&binding, KCG_EVENT_KEY_DOWN, 0, 0, false),
                BindingEdge::None
            );
        }
        #[test]
        fn combo_down_up_modifier_release_and_repeat() {
            let binding = HotkeyBinding::KeyCombo {
                keycode: 49,
                required_flags: 0x60000,
            };
            assert_eq!(
                detect_edge(&binding, KCG_EVENT_KEY_DOWN, 49, 0x60000, false),
                BindingEdge::Press
            );
            assert_eq!(
                detect_edge(&binding, KCG_EVENT_KEY_DOWN, 49, 0x60000, true),
                BindingEdge::None
            );
            assert_eq!(
                detect_edge(&binding, KCG_EVENT_KEY_UP, 49, 0x60000, true),
                BindingEdge::Release
            );
            assert_eq!(
                detect_edge(&binding, KCG_EVENT_FLAGS_CHANGED, 0, 0x20000, true),
                BindingEdge::Release
            );
            assert_eq!(
                detect_edge(&binding, KCG_EVENT_KEY_DOWN, 1, 0x60000, false),
                BindingEdge::None
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn configured_binding_feedback_releases_on_up_cancel_rebind() {
        for release in [
            ActivationSignal::KeyUp,
            ActivationSignal::EscCancel,
            ActivationSignal::Rebind,
        ] {
            let mut pressed = false;
            assert_eq!(
                binding_feedback(&mut pressed, ActivationSignal::KeyDown),
                Some(HotkeyEvent::BindingState(true))
            );
            assert_eq!(
                binding_feedback(&mut pressed, ActivationSignal::KeyDown),
                None
            );
            assert_eq!(
                binding_feedback(&mut pressed, release),
                Some(HotkeyEvent::BindingState(false))
            );
            assert_eq!(binding_feedback(&mut pressed, release), None);
        }
    }

    // Helper: runs one step and collects any events fired.
    fn step(
        state: ActivationState,
        signal: ActivationSignal,
        now: Instant,
    ) -> (ActivationState, Vec<HotkeyEvent>) {
        let mut fired = Vec::new();
        let next = step_activation(state, signal, now, &mut |e| fired.push(e));
        (next, fired)
    }

    fn timeout(state: ActivationState) -> (ActivationState, Vec<HotkeyEvent>) {
        let mut fired = Vec::new();
        let next = step_timeout(state, &mut |e| fired.push(e));
        (next, fired)
    }

    #[test]
    fn tap_attempt_needs_accessibility_or_listen_access() {
        // Accessibility alone authorizes a listen-only tap on macOS 27
        // (TCC composes ListenEvent → PostEvent → Accessibility).
        assert!(should_attempt_tap(true, false));
        assert!(should_attempt_tap(false, true));
        assert!(should_attempt_tap(true, true));
        // Never attempt unpermissioned: it can open Settings unprompted.
        assert!(!should_attempt_tap(false, false));
        assert!(!listener_active());
    }

    #[test]
    fn default_binding_is_right_command() {
        assert_eq!(
            HotkeyBinding::default(),
            HotkeyBinding::ModifierOnly {
                device_mask: NX_DEVICERCMDKEYMASK
            }
        );
    }

    #[test]
    fn binding_serde_round_trip_modifier_only() {
        let b = HotkeyBinding::ModifierOnly {
            device_mask: NX_DEVICERCMDKEYMASK,
        };
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("\"kind\":\"modifier_only\""));
        let back: HotkeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn binding_serde_round_trip_key_combo() {
        let b = HotkeyBinding::KeyCombo {
            keycode: 49,
            required_flags: 0x20000 | 0x40000, // shift + control
        };
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("\"kind\":\"key_combo\""));
        let back: HotkeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn hotkey_state_default_matches_phase_one() {
        let s = HotkeyState::default();
        assert_eq!(*s.binding.read(), HotkeyBinding::default());
    }

    #[test]
    fn idle_keydown_goes_pending() {
        let t0 = Instant::now();
        let (state, fired) = step(ActivationState::Idle, ActivationSignal::KeyDown, t0);
        assert!(matches!(
            state,
            ActivationState::Pending {
                had_prior_tap: false,
                ..
            }
        ));
        assert!(fired.is_empty());
    }

    #[test]
    fn hold_past_threshold_fires_pressed() {
        let t0 = Instant::now();
        let (state, _) = step(ActivationState::Idle, ActivationSignal::KeyDown, t0);
        let (state, fired) = timeout(state);
        assert_eq!(state, ActivationState::HoldActive);
        assert_eq!(fired, vec![HotkeyEvent::Pressed(ActivationMode::Hold)]);
    }

    #[test]
    fn hold_release_fires_released() {
        let t0 = Instant::now();
        let (state, fired) = step(
            ActivationState::HoldActive,
            ActivationSignal::KeyUp,
            t0 + Duration::from_millis(500),
        );
        assert_eq!(state, ActivationState::Idle);
        assert_eq!(fired, vec![HotkeyEvent::Released]);
    }

    #[test]
    fn short_tap_does_not_fire() {
        let t0 = Instant::now();
        let (state, _) = step(ActivationState::Idle, ActivationSignal::KeyDown, t0);
        let (state, fired) = step(
            state,
            ActivationSignal::KeyUp,
            t0 + Duration::from_millis(50),
        );
        assert!(matches!(state, ActivationState::AwaitingSecondTap { .. }));
        assert!(fired.is_empty());

        // No second tap — deadline fires, we drop.
        let (state, fired) = timeout(state);
        assert_eq!(state, ActivationState::Idle);
        assert!(fired.is_empty());
    }

    #[test]
    fn double_tap_enters_toggle_locked_with_pressed() {
        let t0 = Instant::now();
        let (state, _) = step(ActivationState::Idle, ActivationSignal::KeyDown, t0);
        let (state, _) = step(
            state,
            ActivationSignal::KeyUp,
            t0 + Duration::from_millis(50),
        );
        let (state, _) = step(
            state,
            ActivationSignal::KeyDown,
            t0 + Duration::from_millis(150),
        );
        assert!(matches!(
            state,
            ActivationState::Pending {
                had_prior_tap: true,
                ..
            }
        ));
        let (state, fired) = step(
            state,
            ActivationSignal::KeyUp,
            t0 + Duration::from_millis(200),
        );
        assert_eq!(state, ActivationState::ToggleLocked);
        assert_eq!(fired, vec![HotkeyEvent::Pressed(ActivationMode::Toggle)]);
    }

    #[test]
    fn toggle_locked_tap_stops_recording() {
        let (state, fired) = step(
            ActivationState::ToggleLocked,
            ActivationSignal::KeyDown,
            Instant::now(),
        );
        assert_eq!(state, ActivationState::Idle);
        assert_eq!(fired, vec![HotkeyEvent::Released]);
    }

    #[test]
    fn hold_after_single_tap_commits_to_hold() {
        // User taps, then re-presses and holds past threshold — should
        // resolve as hold-to-talk, not toggle-lock.
        let t0 = Instant::now();
        let (state, _) = step(ActivationState::Idle, ActivationSignal::KeyDown, t0);
        let (state, _) = step(
            state,
            ActivationSignal::KeyUp,
            t0 + Duration::from_millis(50),
        );
        let (state, _) = step(
            state,
            ActivationSignal::KeyDown,
            t0 + Duration::from_millis(150),
        );
        let (state, fired) = timeout(state);
        assert_eq!(state, ActivationState::HoldActive);
        assert_eq!(fired, vec![HotkeyEvent::Pressed(ActivationMode::Hold)]);
    }

    #[test]
    fn esc_cancels_hold_active() {
        let (state, fired) = step(
            ActivationState::HoldActive,
            ActivationSignal::EscCancel,
            Instant::now(),
        );
        assert_eq!(state, ActivationState::Idle);
        assert_eq!(fired, vec![HotkeyEvent::Cancelled]);
    }

    #[test]
    fn esc_cancels_toggle_locked() {
        let (state, fired) = step(
            ActivationState::ToggleLocked,
            ActivationSignal::EscCancel,
            Instant::now(),
        );
        assert_eq!(state, ActivationState::Idle);
        assert_eq!(fired, vec![HotkeyEvent::Cancelled]);
    }

    #[test]
    fn esc_in_idle_no_fire() {
        let (state, fired) = step(
            ActivationState::Idle,
            ActivationSignal::EscCancel,
            Instant::now(),
        );
        assert_eq!(state, ActivationState::Idle);
        assert!(fired.is_empty());
    }

    #[test]
    fn rebind_cancels_orphaned_capture() {
        let (state, fired) = step(
            ActivationState::HoldActive,
            ActivationSignal::Rebind,
            Instant::now(),
        );
        assert_eq!(state, ActivationState::Idle);
        assert_eq!(fired, vec![HotkeyEvent::Cancelled]);
    }

    #[test]
    fn rebind_in_idle_is_noop() {
        let (state, fired) = step(
            ActivationState::Idle,
            ActivationSignal::Rebind,
            Instant::now(),
        );
        assert_eq!(state, ActivationState::Idle);
        assert!(fired.is_empty());
    }

    #[test]
    fn key_repeat_in_hold_active_ignored() {
        // Second KeyDown while HoldActive (key-repeat) shouldn't fire
        // a duplicate Pressed.
        let (state, fired) = step(
            ActivationState::HoldActive,
            ActivationSignal::KeyDown,
            Instant::now(),
        );
        assert_eq!(state, ActivationState::HoldActive);
        assert!(fired.is_empty());
    }

    #[test]
    fn next_deadline_pending_is_hold_threshold() {
        let t0 = Instant::now();
        let state = ActivationState::Pending {
            down_at: t0,
            had_prior_tap: false,
        };
        assert_eq!(next_deadline(&state), Some(t0 + HOLD_THRESHOLD));
    }

    #[test]
    fn next_deadline_awaiting_second_tap_is_double_tap_window() {
        let t0 = Instant::now();
        let state = ActivationState::AwaitingSecondTap { last_tap_at: t0 };
        assert_eq!(next_deadline(&state), Some(t0 + DOUBLE_TAP_WINDOW));
    }

    #[test]
    fn next_deadline_idle_is_none() {
        assert!(next_deadline(&ActivationState::Idle).is_none());
    }

    #[test]
    fn next_deadline_hold_active_is_none() {
        assert!(next_deadline(&ActivationState::HoldActive).is_none());
    }

    #[test]
    fn next_deadline_toggle_locked_is_none() {
        assert!(next_deadline(&ActivationState::ToggleLocked).is_none());
    }
}
