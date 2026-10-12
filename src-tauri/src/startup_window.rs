//! Distinguish automatic login startup from a deliberate cold launch.
//! Never infer launch source from whether Launch at login is enabled: the
//! user can still open Murmur manually with that preference switched on.

use std::ffi::{OsStr, OsString};

pub const AUTOSTART_ARGUMENT: &str = "--murmur-autostart";

pub struct StartupLaunch {
    launched_at_login: bool,
}

impl StartupLaunch {
    /// Called in Tauri setup on the AppKit thread, while the initial native
    /// open-application Apple event (when present) is still being handled.
    /// Cache only a boolean; currentAppleEvent must not be consulted later
    /// from an IPC worker or retained past its handler.
    pub fn detect() -> Self {
        let from_agent = has_autostart_argument(std::env::args_os().skip(1));
        #[cfg(target_os = "macos")]
        let from_native_login_item = native_login_launch();
        #[cfg(not(target_os = "macos"))]
        let from_native_login_item = false;
        Self {
            launched_at_login: from_agent || from_native_login_item,
        }
    }
}

fn has_autostart_argument(mut arguments: impl Iterator<Item = OsString>) -> bool {
    arguments.any(|argument| argument == OsStr::new(AUTOSTART_ARGUMENT))
}

#[tauri::command]
pub fn startup_launch_at_login(state: tauri::State<'_, StartupLaunch>) -> bool {
    state.launched_at_login
}

#[cfg(target_os = "macos")]
fn native_login_launch() -> bool {
    use objc2_foundation::NSAppleEventManager;

    NSAppleEventManager::sharedAppleEventManager()
        .currentAppleEvent()
        .is_some_and(|event| is_native_login_event(&event))
}

#[cfg(target_os = "macos")]
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
#[cfg(target_os = "macos")]
const OPEN_APPLICATION: u32 = u32::from_be_bytes(*b"oapp");
#[cfg(target_os = "macos")]
const PROPERTY_DATA: u32 = u32::from_be_bytes(*b"prdt");
#[cfg(target_os = "macos")]
const LAUNCHED_AS_LOGIN_ITEM: u32 = u32::from_be_bytes(*b"lgit");

#[cfg(target_os = "macos")]
fn is_native_login_event(event: &objc2_foundation::NSAppleEventDescriptor) -> bool {
    // Public SDK AERegistry.h: keyAELaunchedAsLogInItem in the initial
    // kAEOpenApplication event identifies login-item startup. Support the
    // native property-data enum and the direct keyword representation.
    event.eventClass() == CORE_EVENT_CLASS
        && event.eventID() == OPEN_APPLICATION
        && (event
            .paramDescriptorForKeyword(PROPERTY_DATA)
            .is_some_and(|property| property.enumCodeValue() == LAUNCHED_AS_LOGIN_ITEM)
            || event
                .paramDescriptorForKeyword(LAUNCHED_AS_LOGIN_ITEM)
                .is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_autostart_marker_identifies_login_launch() {
        assert!(has_autostart_argument(
            [OsString::from(AUTOSTART_ARGUMENT)].into_iter()
        ));
    }

    #[test]
    fn marker_can_follow_other_arguments() {
        assert!(has_autostart_argument(
            [
                OsString::from("--other"),
                OsString::from(AUTOSTART_ARGUMENT)
            ]
            .into_iter()
        ));
    }

    #[test]
    fn ordinary_cold_launch_is_not_login_startup() {
        assert!(!has_autostart_argument(std::iter::empty()));
        assert!(!has_autostart_argument(
            [OsString::from("--murmur-autostart=false")].into_iter()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_do_not_panic_or_match() {
        use std::os::unix::ffi::OsStringExt;
        assert!(!has_autostart_argument(
            [OsString::from_vec(vec![0xff])].into_iter()
        ));
    }

    #[cfg(target_os = "macos")]
    mod native {
        use super::*;
        use objc2::rc::Retained;
        use objc2_foundation::NSAppleEventDescriptor;

        // Construct descriptors only: never dispatch an event, register a
        // Login Item, launch Murmur, or request a macOS privacy permission.
        fn event(event_id: u32) -> Retained<NSAppleEventDescriptor> {
            NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
                CORE_EVENT_CLASS, event_id, None, -1, 0,
            )
        }

        #[test]
        fn ordinary_open_application_is_not_login_startup() {
            assert!(!is_native_login_event(&event(OPEN_APPLICATION)));
        }

        #[test]
        fn login_property_enum_is_recognized() {
            let event = event(OPEN_APPLICATION);
            let property = NSAppleEventDescriptor::descriptorWithEnumCode(LAUNCHED_AS_LOGIN_ITEM);
            event.setParamDescriptor_forKeyword(&property, PROPERTY_DATA);
            assert!(is_native_login_event(&event));
        }

        #[test]
        fn direct_login_keyword_is_recognized() {
            let event = event(OPEN_APPLICATION);
            let property = NSAppleEventDescriptor::descriptorWithBoolean(1);
            event.setParamDescriptor_forKeyword(&property, LAUNCHED_AS_LOGIN_ITEM);
            assert!(is_native_login_event(&event));
        }

        #[test]
        fn unrelated_property_is_not_login_startup() {
            let event = event(OPEN_APPLICATION);
            let property =
                NSAppleEventDescriptor::descriptorWithEnumCode(u32::from_be_bytes(*b"test"));
            event.setParamDescriptor_forKeyword(&property, PROPERTY_DATA);
            assert!(!is_native_login_event(&event));
        }

        #[test]
        fn reopen_remains_an_explicit_request_not_startup() {
            let event = event(u32::from_be_bytes(*b"rapp"));
            let property = NSAppleEventDescriptor::descriptorWithEnumCode(LAUNCHED_AS_LOGIN_ITEM);
            event.setParamDescriptor_forKeyword(&property, PROPERTY_DATA);
            assert!(!is_native_login_event(&event));
        }
    }
}
