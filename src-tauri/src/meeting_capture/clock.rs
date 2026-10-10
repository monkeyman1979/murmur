//! Capture-relative media clock for long-form meeting recording.
//!
//! Sample-buffer PTS values are the source of truth. Wall-clock time is used
//! only for accounting the duration spent paused; it never creates media
//! frames or advances the active recording duration.

pub const WARNING_TWO_HOURS_MS: i64 = 7_200_000;
pub const WARNING_THREE_HOURS_FORTY_FIVE_MS: i64 = 13_500_000;
pub const HARD_LIMIT_MS: i64 = 14_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockWarning {
    TwoHours,
    ThreeHoursFortyFive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockMapping {
    /// Capture-relative PTS in nanoseconds. Both tracks receive the same
    /// rebasing function and therefore share one timeline.
    pub mapped_pts_ns: i64,
    /// Number of source frames that are allowed at the hard limit.
    pub frames: usize,
    pub warning: Option<ClockWarning>,
    pub hard_limit_reached: bool,
}

#[derive(Debug, Clone)]
pub struct CaptureClock {
    source_epoch_ns: Option<i64>,
    active_offset_ns: i64,
    active_end_ns: i64,
    paused_since_ms: Option<i64>,
    paused_duration_ms: i64,
    warned_two_hours: bool,
    warned_three_hours_forty_five: bool,
    hard_limit_reached: bool,
}

impl Default for CaptureClock {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureClock {
    pub fn new() -> Self {
        Self {
            source_epoch_ns: None,
            active_offset_ns: 0,
            active_end_ns: 0,
            paused_since_ms: None,
            paused_duration_ms: 0,
            warned_two_hours: false,
            warned_three_hours_forty_five: false,
            hard_limit_reached: false,
        }
    }

    pub fn start(&mut self, source_epoch_ns: i64) {
        self.source_epoch_ns = Some(source_epoch_ns);
        self.active_offset_ns = 0;
    }

    pub fn is_paused(&self) -> bool {
        self.paused_since_ms.is_some()
    }

    pub fn active_duration_ms(&self) -> i64 {
        self.active_end_ns / 1_000_000
    }

    /// How far the media timeline has actually reached, in nanoseconds.
    ///
    /// This is the max over every source, so it is driven by whichever one is
    /// delivering. Phase A2 uses it for two things: it is where a resumed
    /// capture re-anchors every source, and it is the point the recorder levels
    /// an opportunistic track up to with silence.
    pub fn active_end_ns(&self) -> i64 {
        self.active_end_ns
    }

    pub fn paused_duration_ms_at(&self, wall_time_ms: i64) -> i64 {
        self.paused_duration_ms.saturating_add(
            self.paused_since_ms
                .map(|started| wall_time_ms.saturating_sub(started).max(0))
                .unwrap_or(0),
        )
    }

    pub fn pause_at(&mut self, wall_time_ms: i64) {
        if self.paused_since_ms.is_none() {
            self.paused_since_ms = Some(wall_time_ms.max(0));
        }
    }

    /// Rebase newly-created Core Audio sources to the prior active endpoint.
    /// `new_source_epoch_ns` is the shared monotonic capture timeline origin
    /// established before output delivery is enabled.
    pub fn resume_at(&mut self, wall_time_ms: i64, new_source_epoch_ns: i64) {
        if let Some(paused_since) = self.paused_since_ms.take() {
            self.paused_duration_ms = self
                .paused_duration_ms
                .saturating_add(wall_time_ms.saturating_sub(paused_since).max(0));
        }
        self.active_offset_ns = self.active_end_ns;
        self.source_epoch_ns = Some(new_source_epoch_ns);
    }

    /// Map one sample-buffer interval. Samples after the exact hard limit are
    /// clipped, never silently accepted. A paused clock maps no samples.
    pub fn map_sample(
        &mut self,
        sample_pts_ns: i64,
        sample_duration_ns: i64,
        source_frames: usize,
        sample_rate: u32,
    ) -> Option<ClockMapping> {
        if self.is_paused() || self.hard_limit_reached || sample_rate == 0 {
            return None;
        }
        let source_epoch = self.source_epoch_ns?;
        let delta = sample_pts_ns.saturating_sub(source_epoch).max(0);
        let mapped_pts_ns = self.active_offset_ns.saturating_add(delta);
        let mapped_end_ns = mapped_pts_ns.saturating_add(sample_duration_ns.max(0));
        let limit_ns = HARD_LIMIT_MS.saturating_mul(1_000_000);
        // Only a sample that genuinely crosses the hard limit gets clipped.
        // Deriving an allowed frame count unconditionally would truncate every
        // ordinary sample: `sample_duration_ns` is itself a floored conversion
        // of the source frame count, so converting it back to frames rounds
        // down again and lands one frame short. That dropped frame became an
        // inserted silence frame downstream in the writer, quietly zeroing
        // roughly one sample in every packet for the whole recording.
        let frames = if mapped_end_ns > limit_ns {
            let allowed_duration_ns = limit_ns.saturating_sub(mapped_pts_ns).max(0);
            let allowed_frames = ((allowed_duration_ns as u128)
                .saturating_mul(u128::from(sample_rate))
                / 1_000_000_000) as usize;
            source_frames.min(allowed_frames)
        } else {
            source_frames
        };
        if frames == 0 {
            self.active_end_ns = self.active_end_ns.max(mapped_pts_ns.min(limit_ns));
        } else {
            let actual_duration_ns = (u128::from(frames as u64).saturating_mul(1_000_000_000)
                / u128::from(sample_rate)) as i64;
            self.active_end_ns = self.active_end_ns.max(
                mapped_pts_ns
                    .saturating_add(actual_duration_ns)
                    .min(limit_ns),
            );
        }
        let active_ms = self.active_duration_ms();
        let warning = if !self.warned_two_hours && active_ms >= WARNING_TWO_HOURS_MS {
            self.warned_two_hours = true;
            Some(ClockWarning::TwoHours)
        } else if !self.warned_three_hours_forty_five
            && active_ms >= WARNING_THREE_HOURS_FORTY_FIVE_MS
        {
            self.warned_three_hours_forty_five = true;
            Some(ClockWarning::ThreeHoursFortyFive)
        } else {
            None
        };
        // `frames < source_frames` used to also trip this flag, on the theory
        // that any truncation meant the sample crossed the boundary. In
        // practice `frames` and `source_frames` round-trip through
        // nanoseconds with floor division at each step (frames -> duration_ns
        // in the native callback, duration_ns -> allowed_frames here), so
        // `frames` is very frequently 1 short of `source_frames` from
        // rounding alone — on the very first sample of a recording, nowhere
        // near the real 4-hour limit. That falsely latched
        // `self.hard_limit_reached` (a one-way flag) almost immediately on
        // every capture, which is why every real recording stopped itself
        // within milliseconds of starting. `self.active_end_ns >= limit_ns`
        // alone is the correct, exact test — it already accounts for the
        // clamped `frames` via the `self.active_end_ns` update above.
        let hard_limit_reached = self.active_end_ns >= limit_ns;
        if hard_limit_reached {
            self.hard_limit_reached = true;
        }
        Some(ClockMapping {
            mapped_pts_ns,
            frames,
            warning,
            hard_limit_reached,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> CaptureClock {
        let mut clock = CaptureClock::new();
        clock.start(1_000_000_000);
        clock
    }

    #[test]
    fn rebases_resumed_pts_to_prior_active_endpoint() {
        let mut clock = clock();
        let first = clock
            .map_sample(1_000_000_000, 2_000_000_000, 2, 1_000)
            .unwrap();
        assert_eq!(first.mapped_pts_ns, 0);
        assert_eq!(clock.active_duration_ms(), 2);
        clock.pause_at(100);
        clock.resume_at(5_100, 9_000_000_000);
        let resumed = clock
            .map_sample(9_000_000_000, 3_000_000_000, 3, 1_000)
            .unwrap();
        assert_eq!(resumed.mapped_pts_ns, 2_000_000);
        assert_eq!(clock.active_duration_ms(), 5);
        assert_eq!(clock.paused_duration_ms_at(5_100), 5_000);
    }

    /// Phase A2's resume contract. Every source is re-armed at
    /// `active_end_ns()` and the clock is rebased to the same value, so the
    /// first frame back — from *whichever* source delivers it — lands exactly
    /// on the prior endpoint. No device clock is involved, which is what keeps
    /// Bug 8 fixed, and no source's silence during the pause can shift it.
    #[test]
    fn resuming_at_the_active_end_maps_the_next_frame_onto_the_prior_endpoint() {
        // Two seconds of audio at 1 kHz, then a pause.
        let mut clock = clock();
        clock
            .map_sample(1_000_000_000, 2_000_000_000, 2_000, 1_000)
            .unwrap();
        let resume_at_ns = clock.active_end_ns();
        assert_eq!(resume_at_ns, 2_000_000_000);
        assert_eq!(resume_at_ns / 1_000_000, clock.active_duration_ms());

        clock.pause_at(100);
        clock.resume_at(5_100, resume_at_ns);

        // The always-on source comes back first, carrying exactly the pts it
        // was armed with.
        let microphone = clock
            .map_sample(resume_at_ns, 1_000_000_000, 1_000, 1_000)
            .unwrap();
        assert_eq!(microphone.mapped_pts_ns, 2_000_000_000);
        // The opportunistic source only speaks up a second later, and lands a
        // second later — not back at the resume point.
        let system = clock
            .map_sample(resume_at_ns + 1_000_000_000, 1_000_000_000, 1_000, 1_000)
            .unwrap();
        assert_eq!(system.mapped_pts_ns, 3_000_000_000);
        assert_eq!(clock.active_duration_ms(), 4_000);
    }

    #[test]
    fn stopping_while_paused_includes_only_the_pause_interval() {
        let mut clock = clock();
        clock
            .map_sample(1_000_000_000, 1_000_000_000, 1, 1_000)
            .unwrap();
        clock.pause_at(200);
        assert_eq!(clock.paused_duration_ms_at(5_200), 5_000);
        assert_eq!(clock.active_duration_ms(), 1);
    }

    #[test]
    fn pause_excludes_wall_time_and_maps_no_samples() {
        let mut clock = clock();
        clock
            .map_sample(1_000_000_000, 1_000_000_000, 1, 1_000)
            .unwrap();
        clock.pause_at(10);
        assert!(clock
            .map_sample(2_000_000_000, 1_000_000_000, 1, 1_000)
            .is_none());
        assert_eq!(clock.active_duration_ms(), 1);
    }

    #[test]
    fn warnings_and_limit_fire_at_exact_boundaries() {
        let mut clock = clock();
        let two_hours = clock
            .map_sample(
                1_000_000_000,
                WARNING_TWO_HOURS_MS * 1_000_000,
                (WARNING_TWO_HOURS_MS) as usize,
                1_000,
            )
            .unwrap();
        assert_eq!(two_hours.warning, Some(ClockWarning::TwoHours));
        let second = clock
            .map_sample(
                1_000_000_000 + WARNING_THREE_HOURS_FORTY_FIVE_MS * 1_000_000,
                1_000,
                1,
                1_000_000,
            )
            .unwrap();
        assert_eq!(second.warning, Some(ClockWarning::ThreeHoursFortyFive));
        let limit = clock
            .map_sample(
                1_000_000_000 + HARD_LIMIT_MS * 1_000_000,
                1_000_000,
                1_000,
                1_000_000,
            )
            .unwrap();
        assert!(limit.hard_limit_reached);
        assert!(limit.hard_limit_reached);
    }

    #[test]
    fn ordinary_samples_keep_every_source_frame_despite_nanosecond_rounding() {
        // The exact shape a real 48 kHz capture delivers: a 512-frame packet
        // whose duration was floored to nanoseconds by the native callback.
        // Converting that duration back to frames rounds down to 511, so any
        // unconditional clamp silently drops the last frame of every packet.
        let sample_rate = 48_000;
        let source_frames = 512;
        let duration_ns = (source_frames as i64 * 1_000_000_000) / sample_rate as i64;
        assert_eq!(duration_ns, 10_666_666);
        let mut clock = clock();
        for index in 0..100 {
            let mapping = clock
                .map_sample(
                    1_000_000_000 + index * duration_ns,
                    duration_ns,
                    source_frames,
                    sample_rate,
                )
                .unwrap();
            assert_eq!(mapping.frames, source_frames);
            assert!(!mapping.hard_limit_reached);
        }
    }

    #[test]
    fn clipping_never_maps_frames_past_limit() {
        let mut clock = clock();
        let mapping = clock
            .map_sample(
                1_000_000_000 + (HARD_LIMIT_MS - 1) * 1_000_000,
                10_000_000,
                20,
                1_000,
            )
            .unwrap();
        assert_eq!(mapping.frames, 1);
        assert!(mapping.hard_limit_reached);
        assert_eq!(clock.active_duration_ms(), HARD_LIMIT_MS);
    }
}
