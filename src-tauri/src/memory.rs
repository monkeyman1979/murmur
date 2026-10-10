//! Process-memory helpers.
//!
//! macOS's malloc/Metal stack can keep large freed regions in the process
//! footprint (often compressed) after model caches are dropped. We still have
//! to drop the Rust/Whisper owners first; this module only nudges the allocator
//! to hand already-freed pages back to the OS sooner.

/// Best-effort request to return freed malloc pages to the OS.
///
/// Safe to call after unloading large model caches. No-op on non-macOS and
/// harmless if the platform allocator has nothing releasable.
pub fn release_freed_memory_to_os(reason: &str) {
    #[cfg(target_os = "macos")]
    unsafe {
        use std::os::raw::c_void;

        extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
        }

        // zone=NULL examines all malloc zones; goal=0 asks libmalloc to release
        // whatever it can. This does not free live allocations; it only purges
        // allocator-retained pages after the relevant cache owner has been
        // dropped.
        let released = malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        if released > 0 {
            crate::murmur_dev_log!(
                "[murmur] memory pressure relief after {reason}: released {:.1} MB",
                released as f64 / 1024.0 / 1024.0
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    let _ = reason;
}
