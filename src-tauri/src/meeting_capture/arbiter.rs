use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const IDLE: u8 = 0;
const DICTATION: u8 = 1;
const MEETING: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioCaptureOwner {
    Dictation,
    #[cfg_attr(
        not(any(test, all(target_os = "macos", feature = "meeting-capture"))),
        allow(dead_code)
    )]
    Meeting,
}

impl AudioCaptureOwner {
    fn value(self) -> u8 {
        match self {
            Self::Dictation => DICTATION,
            Self::Meeting => MEETING,
        }
    }
}

#[derive(Debug, Default)]
pub struct AudioCaptureArbiter {
    owner: AtomicU8,
}

impl AudioCaptureArbiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        owner: AudioCaptureOwner,
    ) -> Result<AudioCaptureLease, ()> {
        self.owner
            .compare_exchange(IDLE, owner.value(), Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ())?;
        Ok(AudioCaptureLease {
            arbiter: Arc::clone(self),
            owner,
            released: false,
        })
    }

    pub(crate) fn meeting_capture_active(&self) -> bool {
        self.owner.load(Ordering::Acquire) == MEETING
    }

    #[cfg(test)]
    pub(crate) fn owner(&self) -> Option<AudioCaptureOwner> {
        match self.owner.load(Ordering::Acquire) {
            DICTATION => Some(AudioCaptureOwner::Dictation),
            MEETING => Some(AudioCaptureOwner::Meeting),
            _ => None,
        }
    }
}

pub(crate) struct AudioCaptureLease {
    arbiter: Arc<AudioCaptureArbiter>,
    owner: AudioCaptureOwner,
    released: bool,
}

impl AudioCaptureLease {
    fn release_inner(&mut self) {
        if !self.released {
            let _ = self.arbiter.owner.compare_exchange(
                self.owner.value(),
                IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            self.released = true;
        }
    }
}

impl Drop for AudioCaptureLease {
    fn drop(&mut self) {
        self.release_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_are_mutually_exclusive_and_drop_releases() {
        let arbiter = Arc::new(AudioCaptureArbiter::new());
        let lease = arbiter.reserve(AudioCaptureOwner::Meeting).unwrap();
        assert_eq!(arbiter.owner(), Some(AudioCaptureOwner::Meeting));
        assert!(arbiter.reserve(AudioCaptureOwner::Dictation).is_err());
        drop(lease);
        assert!(arbiter.reserve(AudioCaptureOwner::Dictation).is_ok());
    }
}
