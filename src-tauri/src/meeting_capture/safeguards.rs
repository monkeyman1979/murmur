//! Disk-space and output-volume safeguards for Meeting Capture.

use std::path::Path;
use std::sync::Arc;

use rustix::fs::statvfs;

use crate::captures::{CaptureError, CaptureErrorCode};

pub const GIB: u64 = 1024 * 1024 * 1024;
pub const MIB: u64 = 1024 * 1024;
pub const PREFLIGHT_MINIMUM_BYTES: u64 = 5 * GIB;
pub const EMERGENCY_THRESHOLD_BYTES: u64 = 2 * GIB;
pub const MINIMUM_RESERVE_BYTES: u64 = 256 * MIB;
pub const RESERVE_LOOKAHEAD_SECONDS: u64 = 70;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskSpaceError {
    QueryFailed,
    InvalidVolume,
}

pub trait DiskSpaceProvider: Send + Sync {
    fn available_bytes(&self, path: &Path) -> Result<u64, DiskSpaceError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StatvfsDiskSpaceProvider;

impl DiskSpaceProvider for StatvfsDiskSpaceProvider {
    fn available_bytes(&self, path: &Path) -> Result<u64, DiskSpaceError> {
        let stats = statvfs(path).map_err(|_| DiskSpaceError::QueryFailed)?;
        stats
            .f_bavail
            .checked_mul(stats.f_frsize)
            .ok_or(DiskSpaceError::InvalidVolume)
    }
}

#[derive(Clone)]
pub struct DiskSafeguards {
    provider: Arc<dyn DiskSpaceProvider>,
}

impl Default for DiskSafeguards {
    fn default() -> Self {
        Self::new(Arc::new(StatvfsDiskSpaceProvider))
    }
}

impl DiskSafeguards {
    pub fn new(provider: Arc<dyn DiskSpaceProvider>) -> Self {
        Self { provider }
    }

    pub fn available_bytes(&self, path: &Path) -> Result<u64, CaptureError> {
        self.provider.available_bytes(path).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::InsufficientDiskSpace,
                "available disk space could not be checked",
            )
        })
    }

    pub fn preflight(&self, staging: &Path, output: &Path) -> Result<(), CaptureError> {
        let staging_free = self.available_bytes(staging)?;
        if staging_free < PREFLIGHT_MINIMUM_BYTES {
            return Err(CaptureError::new(
                CaptureErrorCode::InsufficientDiskSpace,
                "meeting staging volume has less than 5 GiB available",
            ));
        }
        // Query the output volume independently, even when both paths usually
        // live on the same disk. This catches a selected/unmounted volume and
        // keeps the output writability check separate from staging capacity.
        let output_free = self.available_bytes(output)?;
        if output_free == 0 {
            return Err(CaptureError::new(
                CaptureErrorCode::OutputDirectoryUnwritable,
                "meeting output volume has no available space",
            ));
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn should_warn(available_bytes: u64) -> bool {
        available_bytes < PREFLIGHT_MINIMUM_BYTES
    }

    /// Return true before the disk reaches the emergency threshold. The
    /// reserve includes measured near-term growth and a bounded queue budget.
    pub fn should_emergency_stop(
        available_bytes: u64,
        measured_growth_per_second: u64,
        queued_bytes: u64,
    ) -> bool {
        let projected = measured_growth_per_second.saturating_mul(RESERVE_LOOKAHEAD_SECONDS);
        let reserve = MINIMUM_RESERVE_BYTES.max(projected.saturating_add(queued_bytes));
        available_bytes <= EMERGENCY_THRESHOLD_BYTES.saturating_add(reserve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct FakeProvider {
        values: Mutex<HashMap<String, Result<u64, DiskSpaceError>>>,
    }

    impl FakeProvider {
        fn with(values: [(&str, Result<u64, DiskSpaceError>); 2]) -> Arc<Self> {
            Arc::new(Self {
                values: Mutex::new(
                    values
                        .into_iter()
                        .map(|(path, value)| (path.into(), value))
                        .collect(),
                ),
            })
        }
    }

    impl DiskSpaceProvider for FakeProvider {
        fn available_bytes(&self, path: &Path) -> Result<u64, DiskSpaceError> {
            self.values
                .lock()
                .unwrap()
                .get(path.to_str().unwrap())
                .cloned()
                .unwrap_or(Err(DiskSpaceError::QueryFailed))
        }
    }

    #[test]
    fn preflight_checks_staging_and_output_volumes_separately() {
        let provider = FakeProvider::with([
            ("/staging", Ok(PREFLIGHT_MINIMUM_BYTES)),
            ("/output", Ok(1)),
        ]);
        assert!(DiskSafeguards::new(provider)
            .preflight(Path::new("/staging"), Path::new("/output"))
            .is_ok());
    }

    #[test]
    fn equality_at_five_gib_is_allowed_and_below_is_rejected() {
        let provider = FakeProvider::with([
            ("/staging", Ok(PREFLIGHT_MINIMUM_BYTES - 1)),
            ("/output", Ok(PREFLIGHT_MINIMUM_BYTES)),
        ]);
        assert_eq!(
            DiskSafeguards::new(provider)
                .preflight(Path::new("/staging"), Path::new("/output"))
                .unwrap_err()
                .code,
            CaptureErrorCode::InsufficientDiskSpace
        );
    }

    #[test]
    fn projected_reserve_is_conservative_and_has_a_floor() {
        assert!(!DiskSafeguards::should_emergency_stop(
            EMERGENCY_THRESHOLD_BYTES + MINIMUM_RESERVE_BYTES + 1,
            0,
            0
        ));
        assert!(DiskSafeguards::should_emergency_stop(
            EMERGENCY_THRESHOLD_BYTES + MINIMUM_RESERVE_BYTES,
            0,
            0
        ));
        let growth = 10 * MIB;
        assert!(DiskSafeguards::should_emergency_stop(
            EMERGENCY_THRESHOLD_BYTES + growth * RESERVE_LOOKAHEAD_SECONDS,
            growth,
            0
        ));
    }

    #[test]
    fn provider_failure_is_typed() {
        let provider = FakeProvider::with([
            ("/staging", Err(DiskSpaceError::QueryFailed)),
            ("/output", Ok(PREFLIGHT_MINIMUM_BYTES)),
        ]);
        assert_eq!(
            DiskSafeguards::new(provider)
                .available_bytes(Path::new("/staging"))
                .unwrap_err()
                .code,
            CaptureErrorCode::InsufficientDiskSpace
        );
    }
}
