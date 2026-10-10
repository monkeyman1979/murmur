//! Text insertion into the currently focused application.
//!
//! Strategy A temporarily replaces the macOS pasteboard with the transcript,
//! posts `⌘V`, then attempts to restore a bounded, detached snapshot of the prior
//! items and representations. The transaction is serialized process-wide and
//! guarded by the pasteboard generation and native process-instance identity.
//!
//! Requires Accessibility permission on macOS. `CGEventPost` has no delivery
//! result, so a successfully constructed and posted event is not proof that the
//! destination accepted the paste. The final focus check and generation guard
//! narrow races but cannot make focus, event delivery, and pasteboard mutation
//! one atomic OS operation.

#[cfg(any(target_os = "macos", test))]
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::focused_app::ProcessIdentity;

/// How long to wait after posting `⌘V` before attempting clipboard restoration.
const CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(200);

// These are raw snapshot-payload bounds, not peak-memory bounds. Snapshot byte
// vectors, restoration NSData/object graphs, and pasteboard-server/provider
// copies can coexist and amplify peak memory. A provider may also allocate or
// block inside AppKit while fulfilling `dataForType:` before NSData exposes its
// length, so these checks cannot bound provider resource use.
#[cfg(any(target_os = "macos", test))]
const MAX_PASTEBOARD_ITEMS: usize = 64;
#[cfg(any(target_os = "macos", test))]
const MAX_TYPES_PER_ITEM: usize = 64;
#[cfg(any(target_os = "macos", test))]
const MAX_TYPE_IDENTIFIER_BYTES: usize = 1_024;
#[cfg(any(target_os = "macos", test))]
const MAX_REPRESENTATION_BYTES: usize = 128 * 1024 * 1024;
#[cfg(any(target_os = "macos", test))]
const MAX_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024;

static INSERTION_TRANSACTION: Mutex<()> = Mutex::new(());

#[cfg(any(target_os = "macos", test))]
type Generation = i64;

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PasteboardSnapshot {
    items: Vec<PasteboardItem>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct PasteboardItem {
    representations: Vec<PasteboardRepresentation>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct PasteboardRepresentation {
    data_type: String,
    data: Vec<u8>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
struct SnapshotBudget {
    bytes: usize,
}

#[cfg(any(target_os = "macos", test))]
impl SnapshotBudget {
    fn check_item_count(count: usize) -> Result<(), InsertError> {
        check_limit(count, MAX_PASTEBOARD_ITEMS)
    }

    fn check_type_count(count: usize) -> Result<(), InsertError> {
        check_limit(count, MAX_TYPES_PER_ITEM)
    }

    fn check_type_identifier(bytes: usize) -> Result<(), InsertError> {
        check_limit(bytes, MAX_TYPE_IDENTIFIER_BYTES)
    }

    fn reserve_representation(&mut self, bytes: usize) -> Result<(), InsertError> {
        check_limit(bytes, MAX_REPRESENTATION_BYTES)?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= MAX_SNAPSHOT_BYTES)
            .ok_or(InsertError::ClipboardSnapshotTooLarge)?;
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
fn check_limit(value: usize, maximum: usize) -> Result<(), InsertError> {
    if value <= maximum {
        Ok(())
    } else {
        Err(InsertError::ClipboardSnapshotTooLarge)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertError {
    NotTrusted,
    TransactionUnavailable,
    TargetUnavailable,
    TargetChanged,
    ClipboardSnapshotFailed,
    ClipboardSnapshotTooLarge,
    ClipboardChangedDuringSnapshot,
    ClipboardOwnershipLostBeforePaste,
    ClipboardWriteFailed,
    SyntheticPasteFailed,
    ClipboardRestoreFailed,
    PrimaryAndRestoreFailed,
}

impl InsertError {
    /// Whether the synthetic paste request had already been posted before the
    /// failure. This does not claim the destination accepted the event.
    pub fn paste_was_requested(&self) -> bool {
        matches!(self, Self::ClipboardRestoreFailed)
    }

    /// Whether restoration of Murmur's owned clipboard generation failed,
    /// independently of whether the synthetic paste request was posted.
    pub fn restore_failed(&self) -> bool {
        matches!(
            self,
            Self::ClipboardRestoreFailed | Self::PrimaryAndRestoreFailed
        )
    }
}

impl std::fmt::Display for InsertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::NotTrusted => {
                "Accessibility permission not granted; cannot synthesize keystrokes"
            }
            Self::TransactionUnavailable => "clipboard insertion transaction is unavailable",
            Self::TargetUnavailable => "paste target is unavailable",
            Self::TargetChanged => "paste target changed before insertion",
            Self::ClipboardSnapshotFailed => "clipboard snapshot could not be completed",
            Self::ClipboardSnapshotTooLarge => "clipboard is too large or complex to preserve",
            Self::ClipboardChangedDuringSnapshot => {
                "clipboard changed before insertion could begin"
            }
            Self::ClipboardOwnershipLostBeforePaste => "clipboard changed before the paste request",
            Self::ClipboardWriteFailed => "clipboard could not be prepared for insertion",
            Self::SyntheticPasteFailed => "synthetic paste event could not be constructed",
            Self::ClipboardRestoreFailed => {
                "paste was requested but the previous clipboard could not be restored"
            }
            Self::PrimaryAndRestoreFailed => {
                "insertion failed and the previous clipboard could not be restored"
            }
        };
        f.write_str(message)
    }
}

impl std::error::Error for InsertError {}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MutationFailure {
    /// Present only when the adapter proved the generation came from Murmur's
    /// own clear/write sequence. Unrelated later generations are never adopted.
    owned_generation: Option<Generation>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConditionalMutation<T> {
    Applied(T),
    OwnershipLost,
}

#[cfg(any(target_os = "macos", test))]
trait Pasteboard {
    fn snapshot(&mut self) -> Result<(Generation, Arc<PasteboardSnapshot>), InsertError>;
    fn generation(&mut self) -> Result<Generation, InsertError>;

    /// Validate ownership and mutate within one platform callback.
    fn replace_with_text_if_generation(
        &mut self,
        expected: Generation,
        text: &str,
    ) -> Result<ConditionalMutation<Generation>, MutationFailure>;

    /// Validate ownership and restore within one platform callback.
    fn restore_if_generation(
        &mut self,
        expected: Generation,
        snapshot: Arc<PasteboardSnapshot>,
    ) -> Result<ConditionalMutation<Generation>, ()>;
}

#[cfg(any(target_os = "macos", test))]
trait FocusGuard {
    fn is_target_frontmost(&mut self, target: ProcessIdentity) -> bool;
}

#[cfg(any(target_os = "macos", test))]
trait PasteEventPoster {
    fn post_cmd_v(&mut self) -> Result<(), InsertError>;
}

#[cfg(any(target_os = "macos", test))]
trait RestoreDelay {
    fn wait(&mut self);
}

#[cfg(any(target_os = "macos", test))]
fn restore_after_primary_failure<P: Pasteboard>(
    pasteboard: &mut P,
    snapshot: Arc<PasteboardSnapshot>,
    owned_generation: Generation,
    primary: InsertError,
) -> Result<(), InsertError> {
    match pasteboard.restore_if_generation(owned_generation, snapshot) {
        Ok(ConditionalMutation::Applied(_)) | Ok(ConditionalMutation::OwnershipLost) => {
            Err(primary)
        }
        Err(()) => Err(InsertError::PrimaryAndRestoreFailed),
    }
}

#[cfg(any(target_os = "macos", test))]
fn coordinate_insertion<P, F, E, D>(
    text: &str,
    trusted: bool,
    target: Option<ProcessIdentity>,
    pasteboard: &mut P,
    focus: &mut F,
    poster: &mut E,
    delay: &mut D,
) -> Result<(), InsertError>
where
    P: Pasteboard,
    F: FocusGuard,
    E: PasteEventPoster,
    D: RestoreDelay,
{
    if text.is_empty() {
        return Ok(());
    }
    if !trusted {
        return Err(InsertError::NotTrusted);
    }
    let target = target.ok_or(InsertError::TargetUnavailable)?;

    let (snapshot_generation, snapshot) = pasteboard.snapshot()?;

    // The process instance was captured with renderer-safe metadata before
    // history/event publication. Recheck it immediately before mutation.
    if !focus.is_target_frontmost(target) {
        return Err(InsertError::TargetChanged);
    }

    let transcript_generation =
        match pasteboard.replace_with_text_if_generation(snapshot_generation, text) {
            Ok(ConditionalMutation::Applied(generation)) => generation,
            Ok(ConditionalMutation::OwnershipLost) => {
                return Err(InsertError::ClipboardChangedDuringSnapshot)
            }
            Err(failure) => {
                return match failure.owned_generation {
                    Some(generation) => restore_after_primary_failure(
                        pasteboard,
                        Arc::clone(&snapshot),
                        generation,
                        InsertError::ClipboardWriteFailed,
                    ),
                    None => Err(InsertError::ClipboardWriteFailed),
                };
            }
        };

    // Main-thread pasteboard work can block. Complete it before the final
    // process-identity check so no potentially blocking clipboard operation
    // widens the interval between focus validation and event posting.
    match pasteboard.generation() {
        Ok(generation) if generation == transcript_generation => {}
        Ok(_) => return Err(InsertError::ClipboardOwnershipLostBeforePaste),
        Err(primary) => {
            return restore_after_primary_failure(
                pasteboard,
                Arc::clone(&snapshot),
                transcript_generation,
                primary,
            )
        }
    }

    if !focus.is_target_frontmost(target) {
        return restore_after_primary_failure(
            pasteboard,
            Arc::clone(&snapshot),
            transcript_generation,
            InsertError::TargetChanged,
        );
    }

    // Post immediately after the final process-identity validation. The OS
    // still cannot make focus validation and event delivery atomic.
    if let Err(primary) = poster.post_cmd_v() {
        return restore_after_primary_failure(
            pasteboard,
            Arc::clone(&snapshot),
            transcript_generation,
            primary,
        );
    }

    delay.wait();

    match pasteboard.restore_if_generation(transcript_generation, snapshot) {
        Ok(ConditionalMutation::Applied(_)) | Ok(ConditionalMutation::OwnershipLost) => Ok(()),
        Err(()) => Err(InsertError::ClipboardRestoreFailed),
    }
}

/// Returns true if the process is a trusted Accessibility client and can
/// post synthetic keystrokes. On non-macOS, returns true unconditionally
/// (no insertion path is wired up off-platform yet anyway).
pub fn is_trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::is_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Check trust and, if not trusted, show the macOS "Grant Accessibility"
/// prompt that deep-links into System Settings. Safe to call on boot —
/// subsequent calls while already trusted return true without prompting.
pub fn prompt_for_trust() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::prompt_for_trust()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Tauri command — re-invoke the Accessibility prompt on demand.
#[tauri::command]
pub fn request_accessibility_prompt() -> bool {
    prompt_for_trust()
}

/// Request insertion into the process instance that was frontmost when
/// transcript attribution was captured. The identity remains native-only.
pub(crate) fn insert(
    app: &tauri::AppHandle,
    text: &str,
    target: Option<ProcessIdentity>,
) -> Result<(), InsertError> {
    if text.is_empty() {
        return Ok(());
    }
    if !is_trusted() {
        return Err(InsertError::NotTrusted);
    }

    let _transaction = INSERTION_TRANSACTION
        .lock()
        .map_err(|_| InsertError::TransactionUnavailable)?;

    #[cfg(target_os = "macos")]
    {
        objc2::rc::autoreleasepool(|_| {
            let mut pasteboard = mac::MainThreadPasteboard::new(app.clone());
            let mut focus = mac::MacFocusGuard;
            let mut poster = mac::MacPasteEventPoster;
            let mut delay = ThreadDelay;
            coordinate_insertion(
                text,
                true,
                target,
                &mut pasteboard,
                &mut focus,
                &mut poster,
                &mut delay,
            )
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, target);
        insert_non_macos(text)
    }
}

#[cfg(target_os = "macos")]
struct ThreadDelay;

#[cfg(target_os = "macos")]
impl RestoreDelay for ThreadDelay {
    fn wait(&mut self) {
        thread::sleep(CLIPBOARD_RESTORE_DELAY);
    }
}

#[cfg(not(target_os = "macos"))]
fn insert_non_macos(text: &str) -> Result<(), InsertError> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|_| InsertError::ClipboardSnapshotFailed)?;
    let previous = clipboard.get_text().ok();
    clipboard
        .set_text(text.to_owned())
        .map_err(|_| InsertError::ClipboardWriteFailed)?;
    thread::sleep(CLIPBOARD_RESTORE_DELAY);
    if clipboard.get_text().ok().as_deref() == Some(text) {
        if let Some(previous) = previous {
            clipboard
                .set_text(previous)
                .map_err(|_| InsertError::ClipboardRestoreFailed)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod mac {
    use std::os::raw::c_void;
    use std::ptr;

    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting,
    };
    use objc2_foundation::{NSArray, NSData, NSString, NSUTF8StringEncoding};

    use super::{
        Arc, ConditionalMutation, FocusGuard, Generation, InsertError, MutationFailure,
        PasteEventPoster, Pasteboard, PasteboardItem, PasteboardRepresentation, PasteboardSnapshot,
        ProcessIdentity, SnapshotBudget,
    };

    const MAX_TYPE_DISCOVERY_PASSES: usize = 16;

    // `v` key on ANSI-layout Mac keyboards.
    const KEYCODE_V: u16 = 9;
    // kCGEventFlagMaskCommand.
    const CMD_FLAG: u64 = 0x0010_0000;
    // CGEventTapLocation::HID.
    const KCG_HID_EVENT_TAP: u32 = 0;
    // kCGEventSourceStateHIDSystemState.
    const KCG_EVENT_SOURCE_STATE_HID_SYSTEM: u32 = 1;

    #[allow(non_camel_case_types)]
    type CFTypeRef = *const c_void;
    #[allow(non_camel_case_types)]
    type CFDictionaryRef = *const c_void;
    #[allow(non_camel_case_types)]
    type CGEventSourceRef = *mut c_void;
    #[allow(non_camel_case_types)]
    type CGEventRef = *mut c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        static kAXTrustedCheckOptionPrompt: CFStringRef;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceCreate(state_id: u32) -> CGEventSourceRef;
        fn CGEventCreateKeyboardEvent(
            source: CGEventSourceRef,
            keycode: u16,
            keydown: bool,
        ) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CGEventPost(tap: u32, event: CGEventRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(x: CFTypeRef);
    }

    struct RawMacPasteboard {
        pasteboard: Retained<NSPasteboard>,
    }

    impl RawMacPasteboard {
        fn general() -> Self {
            Self {
                pasteboard: NSPasteboard::generalPasteboard(),
            }
        }

        #[cfg(test)]
        #[allow(dead_code)]
        fn unique() -> Self {
            Self {
                pasteboard: NSPasteboard::pasteboardWithUniqueName(),
            }
        }

        fn build_objects(
            snapshot: &PasteboardSnapshot,
        ) -> Result<Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>>, ()> {
            let mut objects = Vec::with_capacity(snapshot.items.len());
            for item in &snapshot.items {
                let pasteboard_item = NSPasteboardItem::new();
                for representation in &item.representations {
                    let data_type = NSString::from_str(&representation.data_type);
                    let data = NSData::with_bytes(&representation.data);
                    if !pasteboard_item.setData_forType(&data, &data_type) {
                        return Err(());
                    }
                }
                objects.push(ProtocolObject::from_retained(pasteboard_item));
            }
            Ok(NSArray::from_retained_slice(&objects))
        }

        fn replace_objects_if_generation(
            &self,
            expected: Generation,
            objects: &NSArray<ProtocolObject<dyn NSPasteboardWriting>>,
        ) -> Result<ConditionalMutation<Generation>, MutationFailure> {
            if self.pasteboard.changeCount() as Generation != expected {
                return Ok(ConditionalMutation::OwnershipLost);
            }

            let Some(expected_clear) = expected.checked_add(1) else {
                return Err(MutationFailure {
                    owned_generation: None,
                });
            };
            let clear_generation = self.pasteboard.clearContents() as Generation;
            if clear_generation != expected_clear
                || self.pasteboard.changeCount() as Generation != clear_generation
            {
                return Err(MutationFailure {
                    owned_generation: None,
                });
            }
            if objects.is_empty() {
                return Ok(ConditionalMutation::Applied(clear_generation));
            }

            if !self.pasteboard.writeObjects(objects) {
                let current = self.pasteboard.changeCount() as Generation;
                return Err(MutationFailure {
                    owned_generation: (current == clear_generation).then_some(clear_generation),
                });
            }

            // `clearContents` establishes the new owner/generation;
            // `writeObjects` populates that declaration without advancing it.
            // Any later count therefore belongs to another writer.
            let write_generation = self.pasteboard.changeCount() as Generation;
            if write_generation != clear_generation {
                return Err(MutationFailure {
                    owned_generation: None,
                });
            }
            Ok(ConditionalMutation::Applied(write_generation))
        }

        fn bounded_type_names(types: &NSArray<NSString>) -> Result<Vec<String>, InsertError> {
            SnapshotBudget::check_type_count(types.len())?;
            let mut names = Vec::with_capacity(types.len());
            for data_type in types.iter() {
                SnapshotBudget::check_type_identifier(
                    data_type.lengthOfBytesUsingEncoding(NSUTF8StringEncoding),
                )?;
                names.push(data_type.to_string());
            }
            Ok(names)
        }

        fn snapshot_item(
            pasteboard_item: &NSPasteboardItem,
            budget: &mut SnapshotBudget,
        ) -> Result<PasteboardItem, InsertError> {
            let mut materialized = std::collections::HashMap::<String, Vec<u8>>::new();
            let mut types = pasteboard_item.types();
            if types.is_empty() {
                return Err(InsertError::ClipboardSnapshotFailed);
            }

            // Fulfilling a promised representation can make AppKit expose
            // another converted type without advancing changeCount. Repeat
            // until the bounded ordered type list itself is stable.
            for _ in 0..MAX_TYPE_DISCOVERY_PASSES {
                let type_names = Self::bounded_type_names(&types)?;
                for (data_type, name) in types.iter().zip(&type_names) {
                    if materialized.contains_key(name) {
                        continue;
                    }
                    // `dataForType:` may fulfill a provider promise before a
                    // length is visible. Once NSData exists, reject its length
                    // before copying any representation bytes into Rust.
                    let data = pasteboard_item
                        .dataForType(&data_type)
                        .ok_or(InsertError::ClipboardSnapshotFailed)?;
                    budget.reserve_representation(data.len())?;
                    materialized.insert(name.clone(), data.to_vec());
                }

                let refreshed_types = pasteboard_item.types();
                let refreshed_names = Self::bounded_type_names(&refreshed_types)?;
                if refreshed_names == type_names {
                    let mut representations = Vec::with_capacity(refreshed_names.len());
                    for name in refreshed_names {
                        let data = materialized
                            .remove(&name)
                            .ok_or(InsertError::ClipboardSnapshotFailed)?;
                        representations.push(PasteboardRepresentation {
                            data_type: name,
                            data,
                        });
                    }
                    return Ok(PasteboardItem { representations });
                }
                types = refreshed_types;
            }

            Err(InsertError::ClipboardSnapshotFailed)
        }

        #[cfg(test)]
        #[allow(dead_code)]
        fn seed_snapshot(&mut self, snapshot: &PasteboardSnapshot) -> Result<Generation, ()> {
            let expected = self.pasteboard.changeCount() as Generation;
            let objects = Self::build_objects(snapshot)?;
            match self.replace_objects_if_generation(expected, &objects) {
                Ok(ConditionalMutation::Applied(generation)) => Ok(generation),
                Ok(ConditionalMutation::OwnershipLost) | Err(_) => Err(()),
            }
        }
    }

    impl Pasteboard for RawMacPasteboard {
        fn snapshot(&mut self) -> Result<(Generation, Arc<PasteboardSnapshot>), InsertError> {
            let before = self.pasteboard.changeCount() as Generation;
            let pasteboard_items = self.pasteboard.pasteboardItems();

            let Some(pasteboard_items) = pasteboard_items else {
                let has_types = self
                    .pasteboard
                    .types()
                    .is_some_and(|types| !types.is_empty());
                let after = self.pasteboard.changeCount() as Generation;
                if after != before {
                    return Err(InsertError::ClipboardChangedDuringSnapshot);
                }
                if has_types {
                    return Err(InsertError::ClipboardSnapshotFailed);
                }
                return Ok((before, Arc::new(PasteboardSnapshot::default())));
            };

            SnapshotBudget::check_item_count(pasteboard_items.len())?;
            let mut budget = SnapshotBudget::default();
            let mut snapshot = PasteboardSnapshot {
                items: Vec::with_capacity(pasteboard_items.len()),
            };
            for pasteboard_item in pasteboard_items.iter() {
                snapshot
                    .items
                    .push(Self::snapshot_item(&pasteboard_item, &mut budget)?);
            }

            let after = self.pasteboard.changeCount() as Generation;
            if after != before {
                return Err(InsertError::ClipboardChangedDuringSnapshot);
            }
            Ok((before, Arc::new(snapshot)))
        }

        fn generation(&mut self) -> Result<Generation, InsertError> {
            Ok(self.pasteboard.changeCount() as Generation)
        }

        fn replace_with_text_if_generation(
            &mut self,
            expected: Generation,
            text: &str,
        ) -> Result<ConditionalMutation<Generation>, MutationFailure> {
            let text = NSString::from_str(text);
            let item = NSPasteboardItem::new();
            if !item.setString_forType(&text, unsafe { NSPasteboardTypeString }) {
                return Err(MutationFailure {
                    owned_generation: None,
                });
            }
            let objects = NSArray::from_retained_slice(&[ProtocolObject::from_retained(item)]);
            self.replace_objects_if_generation(expected, &objects)
        }

        fn restore_if_generation(
            &mut self,
            expected: Generation,
            snapshot: Arc<PasteboardSnapshot>,
        ) -> Result<ConditionalMutation<Generation>, ()> {
            // Avoid rebuilding a potentially large restoration graph after a
            // newer owner is already visible. `replace_objects_if_generation`
            // checks again immediately before clear/write because construction
            // itself gives another process time to take ownership.
            if self.pasteboard.changeCount() as Generation != expected {
                return Ok(ConditionalMutation::OwnershipLost);
            }
            let objects = Self::build_objects(&snapshot)?;
            self.replace_objects_if_generation(expected, &objects)
                .map_err(|_| ())
        }
    }

    /// Routes all general-pasteboard access through AppKit's main thread.
    /// Promised pasteboard providers may synchronously fulfill data reads and
    /// AppKit warns that requesting those promises from a worker can fail.
    pub(super) struct MainThreadPasteboard {
        app: tauri::AppHandle,
    }

    impl MainThreadPasteboard {
        pub(super) fn new(app: tauri::AppHandle) -> Self {
            Self { app }
        }

        fn run<R, O>(&self, operation: O) -> Result<R, ()>
        where
            R: Send + 'static,
            O: FnOnce() -> R + Send + 'static,
        {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            self.app
                .run_on_main_thread(move || {
                    let result = objc2::rc::autoreleasepool(|_| operation());
                    let _ = sender.send(result);
                })
                .map_err(|_| ())?;
            receiver.recv().map_err(|_| ())
        }
    }

    impl Pasteboard for MainThreadPasteboard {
        fn snapshot(&mut self) -> Result<(Generation, Arc<PasteboardSnapshot>), InsertError> {
            self.run(|| RawMacPasteboard::general().snapshot())
                .map_err(|_| InsertError::ClipboardSnapshotFailed)?
        }

        fn generation(&mut self) -> Result<Generation, InsertError> {
            self.run(|| RawMacPasteboard::general().generation())
                .map_err(|_| InsertError::ClipboardSnapshotFailed)?
        }

        fn replace_with_text_if_generation(
            &mut self,
            expected: Generation,
            text: &str,
        ) -> Result<ConditionalMutation<Generation>, MutationFailure> {
            let text = text.to_owned();
            self.run(move || {
                RawMacPasteboard::general().replace_with_text_if_generation(expected, &text)
            })
            .unwrap_or(Err(MutationFailure {
                owned_generation: None,
            }))
        }

        fn restore_if_generation(
            &mut self,
            expected: Generation,
            snapshot: Arc<PasteboardSnapshot>,
        ) -> Result<ConditionalMutation<Generation>, ()> {
            self.run(move || RawMacPasteboard::general().restore_if_generation(expected, snapshot))?
        }
    }

    pub(super) struct MacFocusGuard;

    impl FocusGuard for MacFocusGuard {
        fn is_target_frontmost(&mut self, target: ProcessIdentity) -> bool {
            crate::focused_app::current_identity() == Some(target)
        }
    }

    pub(super) struct MacPasteEventPoster;

    impl PasteEventPoster for MacPasteEventPoster {
        fn post_cmd_v(&mut self) -> Result<(), InsertError> {
            post_cmd_v()
        }
    }

    pub(super) fn is_trusted() -> bool {
        unsafe { AXIsProcessTrustedWithOptions(ptr::null()) }
    }

    pub(super) fn prompt_for_trust() -> bool {
        let prompt_key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
        let dict = CFDictionary::from_CFType_pairs(&[(
            prompt_key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);
        unsafe { AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef() as *const c_void) }
    }

    fn post_cmd_v() -> Result<(), InsertError> {
        unsafe {
            let source = CGEventSourceCreate(KCG_EVENT_SOURCE_STATE_HID_SYSTEM);
            if source.is_null() {
                return Err(InsertError::SyntheticPasteFailed);
            }

            let v_down = CGEventCreateKeyboardEvent(source, KEYCODE_V, true);
            let v_up = CGEventCreateKeyboardEvent(source, KEYCODE_V, false);
            if v_down.is_null() || v_up.is_null() {
                if !v_down.is_null() {
                    CFRelease(v_down as CFTypeRef);
                }
                if !v_up.is_null() {
                    CFRelease(v_up as CFTypeRef);
                }
                CFRelease(source as CFTypeRef);
                return Err(InsertError::SyntheticPasteFailed);
            }

            CGEventSetFlags(v_down, CMD_FLAG);
            CGEventSetFlags(v_up, CMD_FLAG);
            CGEventPost(KCG_HID_EVENT_TAP, v_down);
            CGEventPost(KCG_HID_EVENT_TAP, v_up);

            CFRelease(v_down as CFTypeRef);
            CFRelease(v_up as CFTypeRef);
            CFRelease(source as CFTypeRef);
        }
        Ok(())
    }

    /// Native adapter checks invoked by the harness-free Cargo test target.
    /// Its `main` entry point runs on the process main thread, uses only unique
    /// named pasteboards, and substitutes fake focus/event adapters.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn run_named_pasteboard_adapter_checks() {
        use objc2_foundation::NSThread;

        struct FakeFocus;
        impl FocusGuard for FakeFocus {
            fn is_target_frontmost(&mut self, _target: ProcessIdentity) -> bool {
                true
            }
        }

        struct FakePoster;
        impl PasteEventPoster for FakePoster {
            fn post_cmd_v(&mut self) -> Result<(), InsertError> {
                Ok(())
            }
        }

        struct FakeDelay;
        impl super::RestoreDelay for FakeDelay {
            fn wait(&mut self) {}
        }

        fn rich_snapshot() -> PasteboardSnapshot {
            PasteboardSnapshot {
                items: vec![
                    PasteboardItem {
                        representations: vec![
                            PasteboardRepresentation {
                                data_type: "public.utf8-plain-text".into(),
                                data: b"rich text fallback".to_vec(),
                            },
                            PasteboardRepresentation {
                                data_type: "public.rtf".into(),
                                data: b"{\\rtf1 exact bytes}".to_vec(),
                            },
                        ],
                    },
                    PasteboardItem {
                        representations: vec![PasteboardRepresentation {
                            data_type: "public.png".into(),
                            data: vec![0x89, b'P', b'N', b'G', 0, 1, 2, 3],
                        }],
                    },
                    PasteboardItem {
                        representations: vec![PasteboardRepresentation {
                            data_type: "public.file-url".into(),
                            data: b"file:///example-placeholder".to_vec(),
                        }],
                    },
                ],
            }
        }

        assert!(NSThread::isMainThread_class());
        objc2::rc::autoreleasepool(|_| {
            let target = ProcessIdentity::synthetic(42, 100);

            let seeded = rich_snapshot();
            let mut pasteboard = RawMacPasteboard::unique();
            pasteboard.seed_snapshot(&seeded).unwrap();
            let (_, expected) = pasteboard.snapshot().unwrap();
            assert_eq!(expected.items.len(), seeded.items.len());
            for (expected_item, seeded_item) in expected.items.iter().zip(&seeded.items) {
                for representation in &seeded_item.representations {
                    assert!(expected_item.representations.contains(representation));
                }
            }
            super::coordinate_insertion(
                "replacement",
                true,
                Some(target),
                &mut pasteboard,
                &mut FakeFocus,
                &mut FakePoster,
                &mut FakeDelay,
            )
            .unwrap();
            let (_, actual) = pasteboard.snapshot().unwrap();
            assert_eq!(actual, expected);

            let mut empty = RawMacPasteboard::unique();
            empty.seed_snapshot(&PasteboardSnapshot::default()).unwrap();
            super::coordinate_insertion(
                "replacement",
                true,
                Some(target),
                &mut empty,
                &mut FakeFocus,
                &mut FakePoster,
                &mut FakeDelay,
            )
            .unwrap();
            let (_, actual) = empty.snapshot().unwrap();
            assert_eq!(*actual, PasteboardSnapshot::default());

            let mut stale_write = RawMacPasteboard::unique();
            stale_write.seed_snapshot(&seeded).unwrap();
            let (generation, expected) = stale_write.snapshot().unwrap();
            assert_eq!(
                stale_write
                    .replace_with_text_if_generation(generation - 1, "replacement")
                    .unwrap(),
                ConditionalMutation::OwnershipLost
            );
            let (after_generation, actual) = stale_write.snapshot().unwrap();
            assert_eq!(after_generation, generation);
            assert_eq!(actual, expected);

            let mut stale_restore = RawMacPasteboard::unique();
            stale_restore.seed_snapshot(&seeded).unwrap();
            let (snapshot_generation, original) = stale_restore.snapshot().unwrap();
            let transcript_generation = match stale_restore
                .replace_with_text_if_generation(snapshot_generation, "replacement")
                .unwrap()
            {
                ConditionalMutation::Applied(generation) => generation,
                ConditionalMutation::OwnershipLost => panic!("test pasteboard lost ownership"),
            };
            let newer_generation = stale_restore
                .seed_snapshot(&PasteboardSnapshot::default())
                .unwrap();
            assert_eq!(
                stale_restore
                    .restore_if_generation(transcript_generation, original)
                    .unwrap(),
                ConditionalMutation::OwnershipLost
            );
            let (actual_generation, actual) = stale_restore.snapshot().unwrap();
            assert_eq!(actual_generation, newer_generation);
            assert_eq!(*actual, PasteboardSnapshot::default());
        });
    }
}

/// Entry point for the harness-free macOS native pasteboard test executable.
/// Keeping the executable's `main` thread avoids libtest worker-thread access
/// to AppKit while still exercising the production adapter implementation.
#[cfg(all(test, target_os = "macos"))]
#[allow(dead_code)]
pub(crate) fn run_macos_named_pasteboard_adapter_checks() {
    mac::run_named_pasteboard_adapter_checks();
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    type OperationLog = Rc<RefCell<Vec<&'static str>>>;

    fn original_snapshot() -> PasteboardSnapshot {
        PasteboardSnapshot {
            items: vec![
                PasteboardItem {
                    representations: vec![
                        PasteboardRepresentation {
                            data_type: "public.text".into(),
                            data: b"plain".to_vec(),
                        },
                        PasteboardRepresentation {
                            data_type: "public.rtf".into(),
                            data: vec![0, 1, 2, 255],
                        },
                    ],
                },
                PasteboardItem {
                    representations: vec![PasteboardRepresentation {
                        data_type: "public.png".into(),
                        data: vec![9, 8, 7],
                    }],
                },
            ],
        }
    }

    struct FakePasteboard {
        snapshot: Result<(Generation, Arc<PasteboardSnapshot>), InsertError>,
        generation_results: VecDeque<Result<Generation, InsertError>>,
        text_result: Result<ConditionalMutation<Generation>, MutationFailure>,
        restore_results: VecDeque<Result<ConditionalMutation<Generation>, ()>>,
        snapshot_calls: usize,
        conditional_write_calls: usize,
        text_mutations: usize,
        restore_calls: usize,
        restored: Option<Arc<PasteboardSnapshot>>,
        operation_log: Option<OperationLog>,
    }

    impl FakePasteboard {
        fn successful(snapshot: PasteboardSnapshot) -> Self {
            Self {
                snapshot: Ok((1, Arc::new(snapshot))),
                generation_results: VecDeque::from([Ok(2)]),
                text_result: Ok(ConditionalMutation::Applied(2)),
                restore_results: VecDeque::from([Ok(ConditionalMutation::Applied(3))]),
                snapshot_calls: 0,
                conditional_write_calls: 0,
                text_mutations: 0,
                restore_calls: 0,
                restored: None,
                operation_log: None,
            }
        }
    }

    impl Pasteboard for FakePasteboard {
        fn snapshot(&mut self) -> Result<(Generation, Arc<PasteboardSnapshot>), InsertError> {
            self.snapshot_calls += 1;
            self.snapshot.clone()
        }

        fn generation(&mut self) -> Result<Generation, InsertError> {
            if let Some(log) = &self.operation_log {
                log.borrow_mut().push("generation");
            }
            self.generation_results
                .pop_front()
                .unwrap_or(Err(InsertError::ClipboardRestoreFailed))
        }

        fn replace_with_text_if_generation(
            &mut self,
            _expected: Generation,
            _text: &str,
        ) -> Result<ConditionalMutation<Generation>, MutationFailure> {
            self.conditional_write_calls += 1;
            if matches!(self.text_result, Ok(ConditionalMutation::Applied(_))) {
                self.text_mutations += 1;
            }
            self.text_result
        }

        fn restore_if_generation(
            &mut self,
            _expected: Generation,
            snapshot: Arc<PasteboardSnapshot>,
        ) -> Result<ConditionalMutation<Generation>, ()> {
            if let Some(log) = &self.operation_log {
                log.borrow_mut().push("restore");
            }
            self.restore_calls += 1;
            let result = self.restore_results.pop_front().unwrap_or(Err(()));
            if matches!(result, Ok(ConditionalMutation::Applied(_))) {
                self.restored = Some(snapshot);
            }
            result
        }
    }

    pub(super) struct FakeFocus {
        results: VecDeque<bool>,
        operation_log: Option<OperationLog>,
    }

    impl FakeFocus {
        pub(super) fn always(value: bool) -> Self {
            Self {
                results: VecDeque::from([value, value]),
                operation_log: None,
            }
        }

        fn sequence(values: impl IntoIterator<Item = bool>) -> Self {
            Self {
                results: values.into_iter().collect(),
                operation_log: None,
            }
        }
    }

    impl FocusGuard for FakeFocus {
        fn is_target_frontmost(&mut self, _target: ProcessIdentity) -> bool {
            if let Some(log) = &self.operation_log {
                log.borrow_mut().push("focus");
            }
            self.results.pop_front().unwrap_or(false)
        }
    }

    pub(super) struct FakePoster {
        result: Result<(), InsertError>,
        calls: usize,
        operation_log: Option<OperationLog>,
    }

    impl FakePoster {
        pub(super) fn success() -> Self {
            Self {
                result: Ok(()),
                calls: 0,
                operation_log: None,
            }
        }

        fn failure() -> Self {
            Self {
                result: Err(InsertError::SyntheticPasteFailed),
                calls: 0,
                operation_log: None,
            }
        }
    }

    impl PasteEventPoster for FakePoster {
        fn post_cmd_v(&mut self) -> Result<(), InsertError> {
            if let Some(log) = &self.operation_log {
                log.borrow_mut().push("post");
            }
            self.calls += 1;
            self.result
        }
    }

    pub(super) struct FakeDelay;

    impl RestoreDelay for FakeDelay {
        fn wait(&mut self) {}
    }

    fn run(
        text: &str,
        trusted: bool,
        pasteboard: &mut FakePasteboard,
        focus: &mut FakeFocus,
        poster: &mut FakePoster,
    ) -> Result<(), InsertError> {
        coordinate_insertion(
            text,
            trusted,
            Some(ProcessIdentity::synthetic(42, 100)),
            pasteboard,
            focus,
            poster,
            &mut FakeDelay,
        )
    }

    #[test]
    fn empty_input_is_a_no_op() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        let mut focus = FakeFocus::always(false);
        let mut poster = FakePoster::failure();
        assert_eq!(
            run("", false, &mut pasteboard, &mut focus, &mut poster),
            Ok(())
        );
        assert_eq!(pasteboard.snapshot_calls, 0);
        assert_eq!(pasteboard.conditional_write_calls, 0);
        assert_eq!(poster.calls, 0);
    }

    #[test]
    fn untrusted_refuses_without_clipboard_access() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", false, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::NotTrusted)
        );
        assert_eq!(pasteboard.snapshot_calls, 0);
        assert_eq!(pasteboard.conditional_write_calls, 0);
    }

    #[test]
    fn snapshot_failure_refuses_before_overwrite() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.snapshot = Err(InsertError::ClipboardSnapshotFailed);
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::ClipboardSnapshotFailed)
        );
        assert_eq!(pasteboard.conditional_write_calls, 0);
    }

    #[test]
    fn conditional_write_generation_race_refuses_before_mutation() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.text_result = Ok(ConditionalMutation::OwnershipLost);
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::ClipboardChangedDuringSnapshot)
        );
        assert_eq!(pasteboard.conditional_write_calls, 1);
        assert_eq!(pasteboard.text_mutations, 0);
        assert_eq!(pasteboard.restore_calls, 0);
        assert_eq!(poster.calls, 0);
    }

    #[test]
    fn restores_multi_item_multi_type_snapshot_exactly() {
        let expected = original_snapshot();
        let mut pasteboard = FakePasteboard::successful(expected.clone());
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Ok(())
        );
        assert_eq!(pasteboard.restored, Some(Arc::new(expected)));
    }

    #[test]
    fn restores_an_originally_empty_snapshot_to_empty() {
        let mut pasteboard = FakePasteboard::successful(PasteboardSnapshot::default());
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Ok(())
        );
        assert_eq!(
            pasteboard.restored,
            Some(Arc::new(PasteboardSnapshot::default()))
        );
    }

    #[test]
    fn same_text_external_generation_change_after_paste_is_not_overwritten() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.restore_results = VecDeque::from([Ok(ConditionalMutation::OwnershipLost)]);
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("same text", true, &mut pasteboard, &mut focus, &mut poster),
            Ok(())
        );
        assert_eq!(poster.calls, 1);
        assert_eq!(pasteboard.restore_calls, 1);
        assert_eq!(pasteboard.restored, None);
    }

    #[test]
    fn target_change_before_write_refuses_without_mutation() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        let mut focus = FakeFocus::sequence([false]);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::TargetChanged)
        );
        assert_eq!(pasteboard.conditional_write_calls, 0);
        assert_eq!(pasteboard.restore_calls, 0);
        assert_eq!(poster.calls, 0);
    }

    #[test]
    fn target_change_after_write_restores_before_return() {
        let expected = original_snapshot();
        let mut pasteboard = FakePasteboard::successful(expected.clone());
        let mut focus = FakeFocus::sequence([true, false]);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::TargetChanged)
        );
        assert_eq!(pasteboard.restored, Some(Arc::new(expected)));
        assert_eq!(poster.calls, 0);
    }

    #[test]
    fn generation_change_before_final_focus_check_does_not_post_or_restore() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.generation_results = VecDeque::from([Ok(9)]);
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::ClipboardOwnershipLostBeforePaste)
        );
        assert_eq!(poster.calls, 0);
        assert_eq!(pasteboard.restore_calls, 0);
    }

    #[test]
    fn generation_query_failure_rolls_back_owned_generation() {
        let expected = original_snapshot();
        let mut pasteboard = FakePasteboard::successful(expected.clone());
        pasteboard.generation_results = VecDeque::from([Err(InsertError::ClipboardSnapshotFailed)]);
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::ClipboardSnapshotFailed)
        );
        assert_eq!(pasteboard.restore_calls, 1);
        assert_eq!(pasteboard.restored, Some(Arc::new(expected)));
        assert_eq!(poster.calls, 0);
    }

    #[test]
    fn generation_query_and_rollback_failure_are_combined() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.generation_results = VecDeque::from([Err(InsertError::ClipboardSnapshotFailed)]);
        pasteboard.restore_results = VecDeque::from([Err(())]);
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::PrimaryAndRestoreFailed)
        );
        assert_eq!(pasteboard.restore_calls, 1);
        assert_eq!(poster.calls, 0);
    }

    #[test]
    fn final_guard_orders_generation_before_focus_and_posts_immediately_after_focus() {
        let operations = Rc::new(RefCell::new(Vec::new()));
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.operation_log = Some(Rc::clone(&operations));
        let mut focus = FakeFocus::always(true);
        focus.operation_log = Some(Rc::clone(&operations));
        let mut poster = FakePoster::success();
        poster.operation_log = Some(Rc::clone(&operations));

        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Ok(())
        );
        assert_eq!(
            *operations.borrow(),
            ["focus", "generation", "focus", "post", "restore"]
        );
    }

    #[test]
    fn failed_text_write_rolls_back_only_its_owned_generation() {
        let expected = original_snapshot();
        let mut pasteboard = FakePasteboard::successful(expected.clone());
        pasteboard.text_result = Err(MutationFailure {
            owned_generation: Some(2),
        });
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::ClipboardWriteFailed)
        );
        assert_eq!(pasteboard.restored, Some(Arc::new(expected)));
        assert_eq!(poster.calls, 0);
    }

    #[test]
    fn failed_text_write_never_adopts_unknown_generation() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.text_result = Err(MutationFailure {
            owned_generation: None,
        });
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::ClipboardWriteFailed)
        );
        assert_eq!(pasteboard.restore_calls, 0);
        assert_eq!(poster.calls, 0);
    }

    #[test]
    fn synthesis_failure_rolls_back_owned_clipboard() {
        let expected = original_snapshot();
        let mut pasteboard = FakePasteboard::successful(expected.clone());
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::failure();
        assert_eq!(
            run("text", true, &mut pasteboard, &mut focus, &mut poster),
            Err(InsertError::SyntheticPasteFailed)
        );
        assert_eq!(pasteboard.restored, Some(Arc::new(expected)));
    }

    #[test]
    fn post_paste_restore_failure_is_propagated() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.restore_results = VecDeque::from([Err(())]);
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::success();
        let error = run("text", true, &mut pasteboard, &mut focus, &mut poster).unwrap_err();
        assert_eq!(error, InsertError::ClipboardRestoreFailed);
        assert!(error.paste_was_requested());
        assert!(error.restore_failed());
    }

    #[test]
    fn primary_and_restore_failure_is_combined() {
        let mut pasteboard = FakePasteboard::successful(original_snapshot());
        pasteboard.restore_results = VecDeque::from([Err(())]);
        let mut focus = FakeFocus::always(true);
        let mut poster = FakePoster::failure();
        let error = run("text", true, &mut pasteboard, &mut focus, &mut poster).unwrap_err();
        assert_eq!(error, InsertError::PrimaryAndRestoreFailed);
        assert!(!error.paste_was_requested());
        assert!(error.restore_failed());
    }

    #[test]
    fn snapshot_count_and_identifier_limits_accept_boundary_and_reject_excess() {
        assert_eq!(
            SnapshotBudget::check_item_count(MAX_PASTEBOARD_ITEMS),
            Ok(())
        );
        assert_eq!(
            SnapshotBudget::check_item_count(MAX_PASTEBOARD_ITEMS + 1),
            Err(InsertError::ClipboardSnapshotTooLarge)
        );
        assert_eq!(SnapshotBudget::check_type_count(MAX_TYPES_PER_ITEM), Ok(()));
        assert_eq!(
            SnapshotBudget::check_type_count(MAX_TYPES_PER_ITEM + 1),
            Err(InsertError::ClipboardSnapshotTooLarge)
        );
        assert_eq!(
            SnapshotBudget::check_type_identifier(MAX_TYPE_IDENTIFIER_BYTES),
            Ok(())
        );
        assert_eq!(
            SnapshotBudget::check_type_identifier(MAX_TYPE_IDENTIFIER_BYTES + 1),
            Err(InsertError::ClipboardSnapshotTooLarge)
        );
    }

    #[test]
    fn representation_and_aggregate_limits_are_checked_before_copy() {
        let mut budget = SnapshotBudget::default();
        assert_eq!(
            budget.reserve_representation(MAX_REPRESENTATION_BYTES),
            Ok(())
        );
        assert_eq!(
            budget.reserve_representation(MAX_REPRESENTATION_BYTES),
            Ok(())
        );
        assert_eq!(
            budget.reserve_representation(1),
            Err(InsertError::ClipboardSnapshotTooLarge)
        );

        let mut oversized = SnapshotBudget::default();
        assert_eq!(
            oversized.reserve_representation(MAX_REPRESENTATION_BYTES + 1),
            Err(InsertError::ClipboardSnapshotTooLarge)
        );

        let mut overflow = SnapshotBudget { bytes: usize::MAX };
        assert_eq!(
            overflow.reserve_representation(1),
            Err(InsertError::ClipboardSnapshotTooLarge)
        );
    }

    #[test]
    fn error_display_and_debug_are_content_safe() {
        let secret_canary = "private-canary-content";
        let errors = [
            InsertError::NotTrusted,
            InsertError::TransactionUnavailable,
            InsertError::TargetUnavailable,
            InsertError::TargetChanged,
            InsertError::ClipboardSnapshotFailed,
            InsertError::ClipboardSnapshotTooLarge,
            InsertError::ClipboardChangedDuringSnapshot,
            InsertError::ClipboardOwnershipLostBeforePaste,
            InsertError::ClipboardWriteFailed,
            InsertError::SyntheticPasteFailed,
            InsertError::ClipboardRestoreFailed,
            InsertError::PrimaryAndRestoreFailed,
        ];
        for error in errors {
            assert!(!error.to_string().contains(secret_canary));
            assert!(!format!("{error:?}").contains(secret_canary));
        }
    }
}
