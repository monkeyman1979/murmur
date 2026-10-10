//! Native admission gate for updater installation.
//!
//! The frontend disables Restart while work is visible, but a webview snapshot
//! cannot close the race between its final check and the updater plugin's
//! install command. This gate does: user work holds a cheap reference-counted
//! activity lease, and installation can claim exclusive admission only while
//! no lease exists. Once claimed, new work is rejected until the process exits
//! or code explicitly releases the claim after an error that occurred before
//! application-bundle replacement.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;

#[derive(Default)]
struct GateState {
    active_work: u64,
    install_claimed: bool,
}

#[derive(Default)]
pub struct UpdateInstallGate {
    state: Mutex<GateState>,
}

struct ActivityToken {
    gate: Weak<UpdateInstallGate>,
}

impl Drop for ActivityToken {
    fn drop(&mut self) {
        let Some(gate) = self.gate.upgrade() else {
            return;
        };
        let mut state = gate.state.lock();
        if state.active_work == 0 {
            debug_assert!(false, "update activity count underflow");
            return;
        }
        state.active_work -= 1;
    }
}

/// Cloneable ownership for one logical unit of work. Clones keep the same
/// single counter entry alive, which allows meeting recording to hand its lease
/// to transcription without creating an updater-admission gap.
#[derive(Clone)]
pub struct UpdateActivityGuard {
    _token: Arc<ActivityToken>,
}

impl UpdateInstallGate {
    /// Admit new recording, processing, or capture mutation work unless an
    /// update installation has already claimed exclusive admission.
    pub fn begin_activity(self: &Arc<Self>) -> Option<UpdateActivityGuard> {
        let mut state = self.state.lock();
        if state.install_claimed {
            return None;
        }
        state.active_work = state
            .active_work
            .checked_add(1)
            .expect("update activity count overflow");
        Some(UpdateActivityGuard {
            _token: Arc::new(ActivityToken {
                gate: Arc::downgrade(self),
            }),
        })
    }

    /// Atomically claim installation only when no admitted work is alive.
    pub fn try_claim_install(&self) -> bool {
        let mut state = self.state.lock();
        if state.install_claimed || state.active_work != 0 {
            return false;
        }
        state.install_claimed = true;
        true
    }

    /// Release a failed installation attempt so normal work can resume.
    pub fn release_install_claim(&self) {
        self.state.lock().install_claimed = false;
    }

    #[cfg(test)]
    fn snapshot(&self) -> (u64, bool) {
        let state = self.state.lock();
        (state.active_work, state.install_claimed)
    }
}

#[tauri::command]
pub fn claim_update_install(gate: tauri::State<'_, Arc<UpdateInstallGate>>) -> bool {
    gate.try_claim_install()
}

#[tauri::command]
pub fn release_update_install(gate: tauri::State<'_, Arc<UpdateInstallGate>>) {
    gate.release_install_claim();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_claim_and_activity_admission_are_mutually_exclusive() {
        let gate = Arc::new(UpdateInstallGate::default());
        let activity = gate.begin_activity().expect("initial work is admitted");
        assert!(!gate.try_claim_install());
        assert_eq!(gate.snapshot(), (1, false));

        drop(activity);
        assert!(gate.try_claim_install());
        assert!(gate.begin_activity().is_none());
        assert_eq!(gate.snapshot(), (0, true));

        gate.release_install_claim();
        assert!(gate.begin_activity().is_some());
    }

    #[test]
    fn cloned_handoff_is_one_bounded_activity() {
        let gate = Arc::new(UpdateInstallGate::default());
        let recording = gate.begin_activity().expect("recording admitted");
        let transcription = recording.clone();
        assert_eq!(gate.snapshot(), (1, false));

        drop(recording);
        assert_eq!(gate.snapshot(), (1, false));
        assert!(!gate.try_claim_install());

        drop(transcription);
        assert_eq!(gate.snapshot(), (0, false));
        assert!(gate.try_claim_install());
    }

    #[test]
    fn completed_work_does_not_accumulate_activity_state() {
        let gate = Arc::new(UpdateInstallGate::default());
        for _ in 0..10_000 {
            let activity = gate.begin_activity().expect("work is admitted");
            drop(activity);
        }
        assert_eq!(gate.snapshot(), (0, false));
        assert!(gate.try_claim_install());
    }
}
