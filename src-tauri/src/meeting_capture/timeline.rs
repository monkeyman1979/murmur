//! Placing independent capture sources on one media timeline.
//!
//! Phase A2. A5's design put the microphone and the system-audio tap on a
//! single aggregate device and a single IOProc, which bought "one timeline by
//! construction" and cost Bug 9: the tap gates that IOProc, so a quiet Mac
//! recorded nothing at all, microphone included.
//!
//! Splitting them into independent sources means two device clocks. The naive
//! answer is cross-source rebasing, which A5 was right to avoid. It is also
//! unnecessary, because the two sources already share a clock:
//! `AudioTimeStamp.mHostTime` is `mach_absolute_time` (measured in A4), and it
//! is a system-wide monotonic clock rather than a per-device one — every
//! CoreAudio device reports the same domain.
//!
//! So each source **anchors once**, on its first callback, and derives every
//! later timestamp in the frame domain:
//!
//! ```text
//! pts_ns = anchor_offset_ns + (frames_since_epoch × 1e9 / sample_rate)
//! ```
//!
//! Why this shape rather than converting host time per callback:
//!
//! - **Exact.** Frame arithmetic, derived from the absolute frame offset every
//!   time and never accumulated. A4's rule holds and the floor-division seam
//!   behind Bugs 1 and 5 stays closed.
//! - **Jitter-free.** Host time is read once per source. Callback scheduling
//!   jitter — which is real, and is *not* present in `mSampleTime` — can never
//!   reach frame placement.
//! - **Late sources land correctly by construction.** A tap whose first
//!   callback arrives thirty seconds into the capture anchors thirty seconds
//!   in. That is precisely the case Bug 9 broke.
//! - **It scales.** A third source is a third anchor. Nothing else changes.
//!
//! This module is deliberately free of CoreAudio: it is arithmetic over
//! `(host_time_ns, sample_time_frames, sample_rate)`, so it can be tested
//! exhaustively without a device.

use std::sync::atomic::{AtomicI64, Ordering};

/// The capture-wide origin, taken from whichever source produces frames first.
///
/// Held once per capture and shared by every source. `origin_host_ns` is a
/// `mach_absolute_time`-domain value already converted to nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineOrigin {
    origin_host_ns: i64,
}

impl TimelineOrigin {
    pub fn new(origin_host_ns: i64) -> Self {
        Self { origin_host_ns }
    }

    pub fn host_ns(self) -> i64 {
        self.origin_host_ns
    }
}

/// One source's fixed relationship between its own device clock and the
/// capture timeline, latched on its first callback and never recomputed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceAnchor {
    /// The source's `mSampleTime` at the anchoring callback.
    epoch_frames: i64,
    /// Where that callback sits on the capture timeline. Zero for whichever
    /// source anchored the origin; positive for any source that started later.
    offset_ns: i64,
    sample_rate: u32,
}

impl SourceAnchor {
    /// Rebuild an anchor from the parts a source keeps in atomics.
    pub fn from_parts(epoch_frames: i64, offset_ns: i64, sample_rate: u32) -> Self {
        Self {
            epoch_frames,
            offset_ns: offset_ns.max(0),
            sample_rate,
        }
    }

    /// Anchor a source against the capture origin.
    ///
    /// `host_ns` is this callback's `mHostTime` in nanoseconds; the difference
    /// from the origin is how far into the capture this source's first frame
    /// belongs. A source that somehow reports a host time *before* the origin
    /// is clamped to zero rather than mapped into negative time — that would
    /// mean the origin was mis-set, and silently producing negative pts would
    /// corrupt every downstream range.
    pub fn new(origin: TimelineOrigin, host_ns: i64, epoch_frames: i64, sample_rate: u32) -> Self {
        Self {
            epoch_frames,
            offset_ns: host_ns.saturating_sub(origin.host_ns()).max(0),
            sample_rate,
        }
    }

    pub fn offset_ns(self) -> i64 {
        self.offset_ns
    }

    /// Capture-timeline pts for a callback carrying `sample_time` frames.
    ///
    /// Derived from the *absolute* frame offset, never accumulated. A sample
    /// time before the epoch clamps to the anchor rather than running
    /// backwards: `mSampleTime` is monotonic in practice (A4 measured zero
    /// regressions over 748 cycles), but a device reconfiguration can restart
    /// it, and time running backwards is worse than time standing still.
    pub fn pts_ns(self, sample_time: i64) -> i64 {
        let elapsed = sample_time.saturating_sub(self.epoch_frames).max(0);
        self.offset_ns
            .saturating_add(frames_to_ns(elapsed, self.sample_rate))
    }

    /// Anchor a source that is resuming, continuing the timeline from
    /// `resume_at_ns` instead of from the capture origin.
    ///
    /// The source's device clock is not consulted — Bug 8 is exactly what
    /// happens when it is, because `AudioDeviceStart` returns before the HAL
    /// has the device running. The first post-resume callback's sample time
    /// becomes the new epoch instead, so that frame lands on `resume_at_ns`
    /// exactly rather than approximately.
    pub fn resumed(resume_at_ns: i64, epoch_frames: i64, sample_rate: u32) -> Self {
        Self {
            epoch_frames,
            offset_ns: resume_at_ns.max(0),
            sample_rate,
        }
    }
}

/// Sentinel for "not yet set". `mach_absolute_time` is unsigned and its
/// nanosecond conversion is comfortably positive, so `i64::MIN` can never be a
/// real value.
const UNSET: i64 = i64::MIN;

/// The capture origin, claimed by whichever source calls first.
///
/// Shared by every source for the life of one capture. It is deliberately not
/// taken when the capture *starts*: a source that is asked to start and then
/// never delivers a callback — which is exactly what the tap does on a quiet
/// Mac — must not define where zero is.
#[derive(Debug)]
pub struct TimelineOriginCell {
    host_ns: AtomicI64,
}

impl Default for TimelineOriginCell {
    fn default() -> Self {
        Self::new()
    }
}

impl TimelineOriginCell {
    pub fn new() -> Self {
        Self {
            host_ns: AtomicI64::new(UNSET),
        }
    }

    /// Claim the origin for `host_ns`, or return the one already claimed.
    ///
    /// Two sources really can race here — they run on different device IO
    /// threads — so the winner is decided by a compare-exchange rather than by
    /// a check-then-set.
    pub fn resolve(&self, host_ns: i64) -> TimelineOrigin {
        match self
            .host_ns
            .compare_exchange(UNSET, host_ns, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => TimelineOrigin::new(host_ns),
            Err(existing) => TimelineOrigin::new(existing),
        }
    }
}

/// The lock-free half of `SourceAnchor` — what a source's IOProc actually
/// touches, and the only timeline state that lives outside this module.
///
/// One source is written by exactly one thread: CoreAudio serialises a device's
/// IOProc against itself. The controller thread also writes, but only through
/// [`arm_resume`](Self::arm_resume), and only while the capture's shared
/// `paused` gate is closed — the IOProc returns early under that gate, so the
/// two writers never overlap.
#[derive(Debug)]
pub struct AtomicSourceAnchor {
    /// `UNSET` until the next callback anchors, which is both the initial state
    /// and what a resume restores.
    epoch_frames: AtomicI64,
    offset_ns: AtomicI64,
    /// Where the next anchor must land. `UNSET` means "wherever this source's
    /// host time falls relative to the capture origin", i.e. a first start;
    /// anything else is a resume continuing the timeline at that point.
    resume_at_ns: AtomicI64,
    /// `mHostTime` at the anchoring callback, in nanoseconds. Only the
    /// staleness watchdog reads it; frame placement never does.
    anchor_host_ns: AtomicI64,
    /// Core Audio may restart a Bluetooth device's sample clock while its
    /// call-mode route settles. Remember the last callback and mapped endpoint
    /// so that discontinuity can be re-anchored forward without overlapping
    /// audio the writer has already accepted.
    last_sample_time: AtomicI64,
    last_mapped_end_ns: AtomicI64,
    sample_rate: u32,
}

/// How far a source's frame domain may fall behind its own host domain before
/// its anchor is treated as stale.
///
/// This is a watchdog for one specific hazard, and it is deliberately one-sided.
/// An **opportunistic source stops being called entirely** while it has nothing
/// to deliver — that is Bug 9 — and whether the device keeps counting sample
/// time through that gap is up to the device, not to us. If it does not, the
/// source comes back believing no time has passed, and every frame after that
/// is placed in ground the recorder has already covered, which reads downstream
/// as timestamps moving backwards.
///
/// A source that comes back after a gap is really a late-starting source again,
/// so it re-anchors like one. A second is enormous next to the ~10.6 ms cycle
/// and the microseconds-per-hour of genuine crystal drift, so this cannot fire
/// during ordinary running.
pub const MAX_ANCHOR_SKEW_NS: i64 = 1_000_000_000;

impl AtomicSourceAnchor {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            epoch_frames: AtomicI64::new(UNSET),
            offset_ns: AtomicI64::new(0),
            resume_at_ns: AtomicI64::new(UNSET),
            anchor_host_ns: AtomicI64::new(0),
            last_sample_time: AtomicI64::new(UNSET),
            last_mapped_end_ns: AtomicI64::new(UNSET),
            sample_rate,
        }
    }

    /// Test and arithmetic callers that do not own a concrete audio buffer can
    /// map its starting frame without advancing the remembered endpoint.
    #[cfg(test)]
    pub fn map(&self, origin: &TimelineOriginCell, host_ns: i64, sample_time: i64) -> i64 {
        self.map_frames(origin, host_ns, sample_time, 0)
    }

    /// Map one callback onto the capture timeline, anchoring if this is the
    /// first callback since the capture started or resumed.
    ///
    /// `host_ns` is the callback's `mHostTime` in nanoseconds. It is what
    /// *places* this source relative to the others, and ordinarily it does that
    /// once per anchor so callback scheduling jitter cannot reach frame
    /// placement. It is consulted again only when the frame clock stalls,
    /// falls behind, or restarts.
    pub fn map_frames(
        &self,
        origin: &TimelineOriginCell,
        host_ns: i64,
        sample_time: i64,
        frames: usize,
    ) -> i64 {
        if self.epoch_frames.load(Ordering::Acquire) == UNSET {
            let anchor = match self.resume_at_ns.load(Ordering::Acquire) {
                UNSET => SourceAnchor::new(
                    origin.resolve(host_ns),
                    host_ns,
                    sample_time,
                    self.sample_rate,
                ),
                resume_at_ns => SourceAnchor::resumed(resume_at_ns, sample_time, self.sample_rate),
            };
            self.store_anchor(anchor, host_ns, sample_time);
            return self.finish_mapping(anchor.pts_ns(sample_time), sample_time, frames);
        }

        let anchor = SourceAnchor::from_parts(
            self.epoch_frames.load(Ordering::Acquire),
            self.offset_ns.load(Ordering::Acquire),
            self.sample_rate,
        );
        let pts_ns = anchor.pts_ns(sample_time);

        // Where host time says this callback belongs, measured from this
        // source's own anchor rather than from the capture origin — so a pause,
        // which re-anchors, cannot make the paused wall time look like skew.
        let elapsed_host_ns = host_ns.saturating_sub(self.anchor_host_ns.load(Ordering::Acquire));
        let expected_ns = anchor.offset_ns().saturating_add(elapsed_host_ns.max(0));
        let previous_sample = self.last_sample_time.load(Ordering::Acquire);
        let sample_clock_restarted = previous_sample != UNSET && sample_time <= previous_sample;
        if !sample_clock_restarted && expected_ns.saturating_sub(pts_ns) <= MAX_ANCHOR_SKEW_NS {
            return self.finish_mapping(pts_ns, sample_time, frames);
        }

        // Stale or restarted. Re-anchor forward, never backward. Bluetooth
        // call-mode setup can restart mSampleTime after several valid startup
        // callbacks; host time places the new epoch, while the prior mapped end
        // prevents scheduling jitter from overlapping frames already written.
        let previous_end = self.last_mapped_end_ns.load(Ordering::Acquire).max(0);
        let restart_at_ns = expected_ns.max(previous_end);
        let renewed = SourceAnchor::resumed(restart_at_ns, sample_time, self.sample_rate);
        self.store_anchor(renewed, host_ns, sample_time);
        self.finish_mapping(renewed.pts_ns(sample_time), sample_time, frames)
    }

    fn finish_mapping(&self, pts_ns: i64, sample_time: i64, frames: usize) -> i64 {
        let frame_count = i64::try_from(frames).unwrap_or(i64::MAX);
        let end_ns = pts_ns.saturating_add(frames_to_ns(frame_count, self.sample_rate));
        self.last_mapped_end_ns.store(end_ns, Ordering::Release);
        self.last_sample_time.store(sample_time, Ordering::Release);
        pts_ns
    }

    fn store_anchor(&self, anchor: SourceAnchor, host_ns: i64, sample_time: i64) {
        self.anchor_host_ns.store(host_ns, Ordering::Release);
        // Order matters: the offset has to be visible before the epoch that
        // publishes it, because `epoch_frames` is what gates the anchor branch.
        self.offset_ns.store(anchor.offset_ns(), Ordering::Release);
        self.epoch_frames.store(sample_time, Ordering::Release);
    }

    /// Re-arm so the next callback anchors at `resume_at_ns`.
    ///
    /// The device clock is not read — see [`SourceAnchor::resumed`] and Bug 8.
    /// Every source in a capture is armed with the same value, which is what
    /// keeps them on one timeline across a pause.
    pub fn arm_resume(&self, resume_at_ns: i64) {
        self.resume_at_ns
            .store(resume_at_ns.max(0), Ordering::Release);
        self.last_sample_time.store(UNSET, Ordering::Release);
        self.last_mapped_end_ns
            .store(resume_at_ns.max(0), Ordering::Release);
        self.epoch_frames.store(UNSET, Ordering::Release);
    }
}

/// Exact frames → nanoseconds. `i128` so a four-hour capture at 48 kHz cannot
/// overflow the intermediate multiply.
pub fn frames_to_ns(frames: i64, sample_rate: u32) -> i64 {
    if sample_rate == 0 || frames <= 0 {
        return 0;
    }
    ((i128::from(frames) * 1_000_000_000i128) / i128::from(sample_rate)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// A4: delivery is a flat 512 frames per IO cycle.
    const CYCLE: i64 = 512;

    fn origin() -> TimelineOrigin {
        // Arbitrary large value — mach_absolute_time is not zero-based, and
        // anything that assumes it is should fail here.
        TimelineOrigin::new(9_876_543_210_000)
    }

    #[test]
    fn the_anchoring_source_starts_at_zero() {
        let anchor = SourceAnchor::new(origin(), origin().host_ns(), 1_000, RATE);
        assert_eq!(anchor.offset_ns(), 0);
        assert_eq!(anchor.pts_ns(1_000), 0);
    }

    /// The Bug 9 case, and the reason this module exists. A source whose first
    /// callback arrives well into the capture must land there, not at zero.
    #[test]
    fn a_source_that_starts_late_lands_where_it_started() {
        let origin = origin();
        let thirty_seconds = 30 * 1_000_000_000i64;
        // Its device clock is unrelated to the first source's — arbitrary epoch.
        let anchor = SourceAnchor::new(origin, origin.host_ns() + thirty_seconds, 777_777, RATE);

        assert_eq!(anchor.offset_ns(), thirty_seconds);
        assert_eq!(anchor.pts_ns(777_777), thirty_seconds);
        // One second of audio later, it is at 31 s — not 1 s.
        assert_eq!(
            anchor.pts_ns(777_777 + 48_000),
            thirty_seconds + 1_000_000_000
        );
    }

    /// Two sources on unrelated device clocks must agree about "now" as long as
    /// they anchored against the same origin.
    #[test]
    fn independent_sources_agree_on_the_same_instant() {
        let origin = origin();
        let mic = SourceAnchor::new(origin, origin.host_ns(), 5_000, RATE);
        // System audio joins 2.5 s later, with a completely different epoch.
        let late_ns = 2_500_000_000i64;
        let system = SourceAnchor::new(origin, origin.host_ns() + late_ns, -900_000, RATE);

        // Ten seconds after the origin, both must report ten seconds.
        let ten = 10 * 1_000_000_000i64;
        assert_eq!(mic.pts_ns(5_000 + 480_000), ten);
        // The system source has been running 7.5 s by then.
        assert_eq!(system.pts_ns(-900_000 + 360_000), ten);
    }

    /// A4's rule: derive from the absolute frame offset, never accumulate.
    /// 512 frames at 48 kHz is 10,666.67 ns, so per-cycle conversion loses two
    /// thirds of a nanosecond every cycle — the seam behind Bugs 1 and 5.
    #[test]
    fn pts_does_not_drift_over_a_long_capture() {
        let anchor = SourceAnchor::new(origin(), origin().host_ns(), 0, RATE);
        // Four hours, the hard limit.
        let cycles = 4 * 60 * 60 * RATE as i64 / CYCLE;
        let last = anchor.pts_ns(cycles * CYCLE);
        assert_eq!(last, 4 * 60 * 60 * 1_000_000_000i64);

        let accumulated = cycles * frames_to_ns(CYCLE, RATE);
        assert!(
            last > accumulated,
            "accumulating per cycle should lose time: {accumulated} vs {last}"
        );
        // Exactly 0.9 ms lost over four hours: 1,350,000 cycles each losing the
        // 0.667 ns that flooring 10,666,666.67 discards. Pinned as a number
        // rather than a direction so that if the cycle size or rate changes,
        // this fails loudly instead of quietly asserting something weaker.
        assert_eq!(last - accumulated, 900_000);
    }

    #[test]
    fn sample_time_before_the_epoch_stands_still_rather_than_running_backwards() {
        let anchor = SourceAnchor::new(origin(), origin().host_ns() + 1_000, 5_000, RATE);
        assert_eq!(anchor.pts_ns(4_000), anchor.offset_ns());
        assert_eq!(anchor.pts_ns(5_000), anchor.offset_ns());
    }

    /// A source reporting a host time before the origin means the origin was
    /// mis-set. Clamp rather than emit negative pts, which would corrupt every
    /// downstream chunk range.
    #[test]
    fn a_host_time_before_the_origin_clamps_to_zero() {
        let origin = origin();
        let anchor = SourceAnchor::new(origin, origin.host_ns() - 5_000_000_000, 0, RATE);
        assert_eq!(anchor.offset_ns(), 0);
        assert_eq!(anchor.pts_ns(0), 0);
    }

    /// Resume continues the timeline without consulting the device clock —
    /// Bug 8 is what happens when it is consulted.
    #[test]
    fn a_resumed_anchor_continues_the_timeline_from_the_given_point() {
        let anchor = SourceAnchor::new(origin(), origin().host_ns(), 0, RATE);
        let paused_at = anchor.pts_ns(480_000); // 10 s in
        assert_eq!(paused_at, 10_000_000_000);

        // The device's sample time after the pause is arbitrary — it may have
        // kept running, frozen, or restarted.
        for next_epoch in [480_000i64, 1_200_000, 0, -4_096, i64::MAX / 4] {
            let resumed = SourceAnchor::resumed(paused_at, next_epoch, RATE);
            assert_eq!(resumed.pts_ns(next_epoch), paused_at);
            assert_eq!(
                resumed.pts_ns(next_epoch.saturating_add(48_000)),
                paused_at + 1_000_000_000
            );
        }
    }

    /// The origin belongs to whichever source *fires* first, not to whichever
    /// was started first. On a quiet Mac the tap is started and then says
    /// nothing for minutes; if it had claimed the origin at start, the
    /// microphone's audio would have been placed before zero.
    #[test]
    fn the_origin_is_claimed_once_by_the_first_caller() {
        let cell = TimelineOriginCell::new();
        let first = cell.resolve(9_876_543_210_000);
        assert_eq!(first.host_ns(), 9_876_543_210_000);
        // A later source gets the established origin, not its own host time.
        let second = cell.resolve(9_876_543_210_000 + 30 * 1_000_000_000);
        assert_eq!(second.host_ns(), first.host_ns());
    }

    /// The whole Phase A2 shape, exercised through the atomics the IOProcs
    /// actually touch: the microphone fires from the start, the tap stays
    /// silent for thirty seconds, and both land on one timeline.
    #[test]
    fn an_always_on_source_and_a_late_one_share_one_timeline() {
        let origin = TimelineOriginCell::new();
        let mic = AtomicSourceAnchor::new(RATE);
        let system = AtomicSourceAnchor::new(RATE);

        let start_host = 9_876_543_210_000i64;
        // The microphone anchors the capture at its first callback.
        assert_eq!(mic.map(&origin, start_host, 1_000), 0);

        // Ten seconds of microphone-only capture.
        let ten = 10 * 1_000_000_000i64;
        assert_eq!(mic.map(&origin, start_host + ten, 1_000 + 480_000), ten);

        // The tap finally fires thirty seconds in, on an unrelated device clock.
        let thirty = 30 * 1_000_000_000i64;
        assert_eq!(system.map(&origin, start_host + thirty, -777_777), thirty);
        // ...and one second later both sources agree it is 31 s.
        assert_eq!(
            system.map(&origin, start_host + thirty, -777_777 + 48_000),
            thirty + 1_000_000_000
        );
        assert_eq!(
            mic.map(&origin, start_host + thirty, 1_000 + 48_000 * 31),
            thirty + 1_000_000_000
        );
    }

    /// Host time is an anchor, not a per-callback conversion. Jitter in when a
    /// callback is *scheduled* must not move where its frames land.
    #[test]
    fn callback_jitter_after_anchoring_cannot_move_frame_placement() {
        let origin = TimelineOriginCell::new();
        let source = AtomicSourceAnchor::new(RATE);
        let start_host = 1_000_000_000_000i64;
        source.map(&origin, start_host, 0);

        // Jittery host times on later callbacks; frames decide the pts.
        for (cycle, jitter) in [0i64, 5_000_000, -5_000_000, 50_000_000]
            .into_iter()
            .enumerate()
        {
            let frames = (cycle as i64 + 1) * CYCLE;
            let host = start_host + frames_to_ns(frames, RATE) + jitter;
            assert_eq!(
                source.map(&origin, host, frames),
                frames_to_ns(frames, RATE)
            );
        }
    }

    /// Pause/resume is a capture-level operation: every source is armed with
    /// the same point, so they come back on the same timeline even though only
    /// one of them may actually resume delivering.
    #[test]
    fn arming_resume_puts_every_source_back_on_the_same_point() {
        let origin = TimelineOriginCell::new();
        let mic = AtomicSourceAnchor::new(RATE);
        let system = AtomicSourceAnchor::new(RATE);
        let start_host = 500_000_000_000i64;
        mic.map(&origin, start_host, 0);
        system.map(&origin, start_host, 10_000);

        let paused_at = mic.map(&origin, start_host, 480_000); // 10 s in
        assert_eq!(paused_at, 10_000_000_000);

        mic.arm_resume(paused_at);
        system.arm_resume(paused_at);

        // Both devices hand back arbitrary sample times after the restart, and
        // the host time is now well past the origin — neither may shift the
        // resumed pts, which has to be exactly where the pause left it.
        assert_eq!(mic.map(&origin, start_host + 60_000_000_000, 33), paused_at);
        assert_eq!(
            system.map(&origin, start_host + 60_000_000_000, -9_999),
            paused_at
        );
        assert_eq!(
            mic.map(&origin, start_host + 60_000_000_000, 33 + 48_000),
            paused_at + 1_000_000_000
        );
    }

    /// The hazard the watchdog exists for. An opportunistic source is not
    /// called at all while it has nothing to deliver, and if its device does
    /// not count sample time through that gap it comes back believing no time
    /// has passed. Placing those frames where the frame domain says would put
    /// them behind everything already recorded.
    #[test]
    fn a_source_whose_sample_clock_froze_while_gated_re_anchors_forward() {
        let origin = TimelineOriginCell::new();
        let source = AtomicSourceAnchor::new(RATE);
        let start_host = 300_000_000_000i64;
        source.map(&origin, start_host, 0);

        // Two seconds of real audio, then the source goes quiet.
        let two_seconds = 2 * 1_000_000_000i64;
        assert_eq!(
            source.map(&origin, start_host + two_seconds, 96_000),
            two_seconds
        );

        // Thirty seconds later it fires again — with a sample time that barely
        // moved, because the device stopped counting.
        let thirty = 30 * 1_000_000_000i64;
        let resumed = source.map(&origin, start_host + thirty, 96_512);
        assert_eq!(
            resumed, thirty,
            "a frozen sample clock must not place audio back in the past"
        );
        // ...and it runs normally from there, on its refreshed anchor.
        assert_eq!(
            source.map(&origin, start_host + thirty, 96_512 + 48_000),
            thirty + 1_000_000_000
        );
    }

    #[test]
    fn a_bluetooth_sample_clock_restart_reanchors_after_the_previous_buffer() {
        const BLUETOOTH_RATE: u32 = 16_000;
        const CALLBACK_FRAMES: usize = 320;
        const CALLBACK_NS: i64 = 20_000_000;

        let origin = TimelineOriginCell::new();
        let source = AtomicSourceAnchor::new(BLUETOOTH_RATE);
        let start_host = 400_000_000_000i64;

        assert_eq!(
            source.map_frames(&origin, start_host, 10_000, CALLBACK_FRAMES),
            0
        );
        assert_eq!(
            source.map_frames(&origin, start_host + CALLBACK_NS, 10_320, CALLBACK_FRAMES,),
            CALLBACK_NS
        );

        // Opening the system-audio side of a headset route can restart the
        // microphone's device frame clock after several valid callbacks. A
        // little host-time jitter must not place the restarted packet over the
        // two packets the writer already accepted.
        assert_eq!(
            source.map_frames(
                &origin,
                start_host + 2 * CALLBACK_NS - 100_000,
                0,
                CALLBACK_FRAMES,
            ),
            2 * CALLBACK_NS
        );
        assert_eq!(
            source.map_frames(&origin, start_host + 3 * CALLBACK_NS, 320, CALLBACK_FRAMES,),
            3 * CALLBACK_NS
        );
    }

    /// The watchdog must not confuse a pause with a stalled device. Paused wall
    /// time is deliberately *not* on the media timeline, so a long pause makes
    /// host time and frame time disagree by exactly the pause — which is
    /// correct, not stale.
    #[test]
    fn a_long_pause_does_not_trip_the_staleness_watchdog() {
        let origin = TimelineOriginCell::new();
        let source = AtomicSourceAnchor::new(RATE);
        let start_host = 800_000_000_000i64;
        source.map(&origin, start_host, 0);
        let paused_at = source.map(&origin, start_host + 1_000_000_000, 48_000);
        assert_eq!(paused_at, 1_000_000_000);

        // Ten minutes of pause, then resume.
        let ten_minutes = 600 * 1_000_000_000i64;
        source.arm_resume(paused_at);
        assert_eq!(
            source.map(&origin, start_host + ten_minutes, 48_000),
            paused_at
        );
        assert_eq!(
            source.map(&origin, start_host + ten_minutes + 1_000_000_000, 96_000),
            paused_at + 1_000_000_000
        );
    }

    /// Real crystal drift is microseconds per hour, and callback scheduling
    /// wanders by milliseconds. Neither may re-anchor anything — the whole
    /// point of anchoring once is that frames, not host time, place the audio.
    #[test]
    fn ordinary_drift_and_scheduling_never_re_anchor() {
        let origin = TimelineOriginCell::new();
        let source = AtomicSourceAnchor::new(RATE);
        let start_host = 100_000_000_000i64;
        source.map(&origin, start_host, 0);

        // An hour of audio whose host time creeps 100 ms ahead of frame time.
        for minute in 1..=60i64 {
            let frames = minute * 60 * i64::from(RATE);
            let expected = frames_to_ns(frames, RATE);
            let creep = minute * 100_000_000 / 60;
            assert_eq!(
                source.map(&origin, start_host + expected + creep, frames),
                expected,
                "minute {minute} should still be placed by its frames"
            );
        }
    }

    /// Pause/resume must not rewind the media timeline, however many times it
    /// happens and however the devices' own sample times behave across each
    /// gap. Carried over from the single-IOProc design this replaced.
    #[test]
    fn repeated_pause_resume_cycles_never_move_the_timeline_backwards() {
        let origin = TimelineOriginCell::new();
        let source = AtomicSourceAnchor::new(RATE);
        let start_host = 700_000_000_000i64;
        let mut device_frames = 1_000i64;
        source.map(&origin, start_host, device_frames);
        let mut previous = 0i64;

        for cycle in 0..5 {
            // A second of audio, then a pause.
            device_frames += 48_000;
            let pts = source.map(&origin, start_host, device_frames);
            assert!(
                pts > previous,
                "cycle {cycle} went backwards: {pts} <= {previous}"
            );
            previous = pts;

            source.arm_resume(pts);
            // The device's sample time jumps arbitrarily across the pause.
            device_frames = device_frames.saturating_add(1_234_567);
            assert_eq!(source.map(&origin, start_host, device_frames), previous);
        }

        assert_eq!(previous, 5_000_000_000, "five one-second segments");
    }

    /// A source that never fires never anchors, so it never claims the origin
    /// and never contributes a pts. That is Bug 9's tap, and it needs no
    /// special case anywhere.
    #[test]
    fn a_source_that_never_fires_leaves_the_timeline_alone() {
        let origin = TimelineOriginCell::new();
        let mic = AtomicSourceAnchor::new(RATE);
        let silent = AtomicSourceAnchor::new(RATE);

        assert_eq!(mic.map(&origin, 42_000_000_000, 0), 0);
        // The silent source never called `map`, so it never claimed the origin:
        // a source that fires an hour later still measures from the microphone.
        assert_eq!(origin.resolve(99_000_000_000).host_ns(), 42_000_000_000);
        let _ = silent;
    }

    #[test]
    fn frames_to_ns_is_exact_and_saturating() {
        assert_eq!(frames_to_ns(48_000, RATE), 1_000_000_000);
        assert_eq!(frames_to_ns(0, RATE), 0);
        assert_eq!(frames_to_ns(-5, RATE), 0);
        assert_eq!(frames_to_ns(100, 0), 0);
        // Four hours at 48 kHz. The intermediate multiply is ~6.9e17, which
        // still fits i64 — but only just, and the i128 widening is what stops
        // a longer capture or a higher sample rate from wrapping silently.
        assert_eq!(frames_to_ns(691_200_000, RATE), 14_400_000_000_000);
    }
}
