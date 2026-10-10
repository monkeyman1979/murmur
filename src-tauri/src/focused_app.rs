//! Detect which application is frontmost so transcripts can be attributed and
//! insertion can remain bound to the same native process instance.
//!
//! On macOS we ask `NSWorkspace.sharedWorkspace.frontmostApplication`, which
//! Apple documents as thread-safe. Native process identity stays separate from
//! [`FocusedApp`] and is never serialized to a renderer.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusedApp {
    pub name: Option<String>,
    pub bundle_id: Option<String>,
}

/// Native-only identity for one application process instance. PID alone is not
/// sufficient because macOS may reuse it after a process exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pid: i32,
    launch_time_bits: u64,
}

impl ProcessIdentity {
    #[cfg(test)]
    pub(crate) fn synthetic(pid: i32, launch_time_bits: u64) -> Self {
        Self {
            pid,
            launch_time_bits,
        }
    }
}

/// Metadata and native identity from one frontmost-application lookup.
pub(crate) struct FocusedAppTarget {
    pub(crate) app: FocusedApp,
    pub(crate) identity: ProcessIdentity,
}

/// The application currently in front, including its native-only insertion
/// target. Fails closed when PID or launch date is unavailable.
pub(crate) fn current_target() -> Option<FocusedAppTarget> {
    #[cfg(target_os = "macos")]
    {
        mac::current_target()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Return the current frontmost process-instance identity for insertion
/// rechecks. This value never crosses the Tauri serialization boundary.
#[cfg(target_os = "macos")]
pub(crate) fn current_identity() -> Option<ProcessIdentity> {
    #[cfg(target_os = "macos")]
    {
        mac::current_identity()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{FocusedApp, FocusedAppTarget, ProcessIdentity};
    use objc2_app_kit::{NSRunningApplication, NSWorkspace};

    fn process_identity(app: &NSRunningApplication) -> Option<ProcessIdentity> {
        let pid = app.processIdentifier();
        if pid <= 0 {
            return None;
        }
        let launch_time = app.launchDate()?.timeIntervalSinceReferenceDate();
        if !launch_time.is_finite() {
            return None;
        }
        Some(ProcessIdentity {
            pid,
            launch_time_bits: launch_time.to_bits(),
        })
    }

    pub(super) fn current_target() -> Option<FocusedAppTarget> {
        objc2::rc::autoreleasepool(|_| {
            let workspace = NSWorkspace::sharedWorkspace();
            let app = workspace.frontmostApplication()?;
            let identity = process_identity(&app)?;
            let name = app.localizedName().map(|s| s.to_string());
            let bundle_id = app.bundleIdentifier().map(|s| s.to_string());
            Some(FocusedAppTarget {
                app: FocusedApp { name, bundle_id },
                identity,
            })
        })
    }

    pub(super) fn current_identity() -> Option<ProcessIdentity> {
        objc2::rc::autoreleasepool(|_| {
            let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
            process_identity(&app)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_identity_distinguishes_pid_reuse() {
        let first = ProcessIdentity::synthetic(42, 100);
        let relaunched = ProcessIdentity::synthetic(42, 101);
        assert_ne!(first, relaunched);
    }

    #[test]
    fn renderer_metadata_does_not_gain_process_identity_fields() {
        let metadata = FocusedApp {
            name: Some("Example".into()),
            bundle_id: Some("example.invalid".into()),
        };
        let serialized = serde_json::to_value(metadata).unwrap();
        assert_eq!(
            serialized.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["bundle_id", "name"]
        );
    }
}
