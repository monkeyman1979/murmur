//! Launch at login through SMAppService.mainApp.
//!
//! v0.1.0 used a per-user LaunchAgent (~/Library/LaunchAgents/Murmur.plist).
//! macOS lists those under "Allow in the Background" and announces them as
//! software from the signing team's legal name. A main-app login item is
//! listed under "Open at Login" as Murmur itself. `migrate_legacy_agent`
//! carries an existing opt-in across once, then removes the old plist.

use std::path::PathBuf;

/// Program argument the v0.1.0 LaunchAgent passed to Murmur. Only a plist
/// carrying it is treated as ours; another app could use the same label.
const LEGACY_AGENT_MARKER: &str = crate::startup_window::AUTOSTART_ARGUMENT;
const LEGACY_AGENT_FILE: &str = "Murmur.plist";

fn legacy_agent_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(LEGACY_AGENT_FILE),
    )
}

fn is_legacy_agent(contents: &str) -> bool {
    contents.contains(LEGACY_AGENT_MARKER) && contents.contains("<string>Murmur</string>")
}

/// Returns true only when a Murmur-written LaunchAgent plist exists.
fn legacy_agent_present() -> bool {
    legacy_agent_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|contents| is_legacy_agent(&contents))
}

fn remove_legacy_agent() {
    if !legacy_agent_present() {
        return;
    }
    if let Some(path) = legacy_agent_path() {
        if let Err(error) = std::fs::remove_file(path) {
            crate::murmur_dev_log!("[murmur] could not remove legacy launch agent: {error}");
        }
    }
}

/// Run once from setup: a user who opted in under v0.1.0 keeps Launch at
/// login, now as a main-app login item. The old plist is removed only after
/// registration succeeds so a failure never silently turns the feature off.
pub fn migrate_legacy_agent() {
    if !legacy_agent_present() {
        return;
    }
    match native::register() {
        Ok(()) => remove_legacy_agent(),
        Err(error) => crate::murmur_dev_log!("[murmur] could not migrate launch at login: {error}"),
    }
}

#[tauri::command]
pub fn launch_at_login_enabled() -> Result<bool, String> {
    Ok(native::status()? == native::Status::Enabled)
}

/// Returns the verified state after the change. When macOS requires the user
/// to approve Murmur in System Settings, opens Login Items and reports false.
#[tauri::command]
pub fn set_launch_at_login(enabled: bool) -> Result<bool, String> {
    if enabled {
        native::register()?;
        let status = native::status()?;
        if status == native::Status::RequiresApproval {
            native::open_login_items_settings();
        }
        remove_legacy_agent();
        Ok(status == native::Status::Enabled)
    } else {
        native::unregister()?;
        remove_legacy_agent();
        Ok(false)
    }
}

#[cfg(target_os = "macos")]
mod native {
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    // SMErrors.h: registering an already-registered item, or unregistering
    // one that is not registered, is the requested end state.
    const ALREADY_REGISTERED: isize = 12;
    const JOB_NOT_FOUND: isize = 6;

    #[derive(Debug, PartialEq, Eq)]
    pub enum Status {
        Enabled,
        RequiresApproval,
        Disabled,
    }

    fn from_native(status: SMAppServiceStatus) -> Status {
        match status {
            SMAppServiceStatus::Enabled => Status::Enabled,
            SMAppServiceStatus::RequiresApproval => Status::RequiresApproval,
            // NotRegistered, and NotFound for a never-registered main app.
            _ => Status::Disabled,
        }
    }

    pub fn status() -> Result<Status, String> {
        Ok(from_native(unsafe { SMAppService::mainAppService().status() }))
    }

    pub fn register() -> Result<(), String> {
        match unsafe { SMAppService::mainAppService().registerAndReturnError() } {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ALREADY_REGISTERED => Ok(()),
            Err(error) => Err(format!("login item registration failed ({})", error.code())),
        }
    }

    pub fn unregister() -> Result<(), String> {
        let service = unsafe { SMAppService::mainAppService() };
        if from_native(unsafe { service.status() }) == Status::Disabled {
            return Ok(());
        }
        match unsafe { service.unregisterAndReturnError() } {
            Ok(()) => Ok(()),
            Err(error) if error.code() == JOB_NOT_FOUND => Ok(()),
            Err(error) => Err(format!("login item removal failed ({})", error.code())),
        }
    }

    pub fn open_login_items_settings() {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    #[allow(dead_code)]
    #[derive(Debug, PartialEq, Eq)]
    pub enum Status {
        Enabled,
        RequiresApproval,
        Disabled,
    }

    const UNSUPPORTED: &str = "Launch at login is only available on macOS";

    pub fn status() -> Result<Status, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn register() -> Result<(), String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn unregister() -> Result<(), String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn open_login_items_settings() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const V010_AGENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
  <dict>
  <key>Label</key>
  <string>Murmur</string>
  <key>ProgramArguments</key>
  <array><string>/Applications/Murmur.app/Contents/MacOS/murmur</string><string>--murmur-autostart</string></array>
  <key>RunAtLoad</key>
  <true/>
  </dict>
</plist>"#;

    #[test]
    fn recognizes_the_v010_launch_agent() {
        assert!(is_legacy_agent(V010_AGENT));
    }

    #[test]
    fn leaves_an_unrelated_agent_with_the_same_file_name_alone() {
        let other = V010_AGENT.replace("--murmur-autostart", "--other-app");
        assert!(!is_legacy_agent(&other));
        assert!(!is_legacy_agent(""));
    }
}
