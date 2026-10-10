//! Pure, unit-local speech evidence and decode coordinate policy. No native runtime here.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

mod native;
pub(crate) use native::scan;

use super::{TimedSegment, TimedTranscript};
use crate::error::{Error, Result};

/// A failing detector/worker factory is never evaluated by dictation or the
/// system track. The caller uses this before any new WAV or native work.
#[inline]
pub(crate) fn only_when_enabled<T>(
    mode: super::SpeechGateMode,
    factory: impl FnOnce() -> T,
) -> Option<T> {
    match mode {
        super::SpeechGateMode::Off => None,
        super::SpeechGateMode::MeetingMicrophone => Some(factory()),
    }
}

pub(crate) const RATE: usize = 16_000;
pub(crate) const FRAME: usize = 512;
const MAX_SAMPLES: usize = 60 * RATE;
const CONTEXT_SAMPLES: usize = 300 * RATE / 1000;
const MIN_REAL_REGION_SAMPLES: usize = 1700 * RATE / 1000;
pub(crate) const MIN_DECODE_SAMPLES: usize = 1100 * RATE / 1000;
const END_GAP_FRAMES: usize = 10; // 320 ms, rounded up from 300 ms.
const WINDOW_MS: u32 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Region {
    pub start: usize,
    pub end: usize, // exclusive, real audio only
}

pub(crate) fn check_cancelled(signal: Option<&Arc<AtomicBool>>) -> Result<()> {
    if signal.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        Err(Error::Backend("meeting speech gate cancelled".into()))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct RegionPolicy {
    interior_min_positive_frames: usize,
}

impl Default for RegionPolicy {
    fn default() -> Self {
        Self {
            interior_min_positive_frames: 5,
        }
    }
}

/// Scores are one per 512-sample frame; the last frame may have fewer real samples.
/// Pad only qualified cores. Neither hangover nor padding contributes to the evidence count.
/// Production uses the frozen configuration 3, not the historical five-frame policy.
pub(crate) fn regions(scores: &[f32], real_samples: usize) -> Result<Vec<Region>> {
    let cores = qualified_cores(
        scores,
        real_samples,
        RegionPolicy {
            interior_min_positive_frames: 6,
        },
    )?;
    pad_and_bound_cores(cores, real_samples, MIN_REAL_REGION_SAMPLES)
}

#[cfg(test)]
fn historical_regions(scores: &[f32], real_samples: usize) -> Result<Vec<Region>> {
    regions_with_policy(scores, real_samples, RegionPolicy::default())
}

#[cfg(test)]
fn regions_with_policy(
    scores: &[f32],
    real_samples: usize,
    policy: RegionPolicy,
) -> Result<Vec<Region>> {
    pad_and_bound_cores(
        qualified_cores(scores, real_samples, policy)?,
        real_samples,
        0,
    )
}

fn qualified_cores(
    scores: &[f32],
    real_samples: usize,
    policy: RegionPolicy,
) -> Result<Vec<Region>> {
    if scores.len() != real_samples.div_ceil(FRAME)
        || scores
            .iter()
            .any(|s| !s.is_finite() || !(0.0..=1.0).contains(s))
    {
        return Err(Error::Backend("invalid meeting speech evidence".into()));
    }
    let mut cores = Vec::new();
    let mut active: Option<(usize, usize, usize)> = None; // start frame, last evidence frame, positive frames
    for (i, &score) in scores.iter().enumerate() {
        if let Some((_, last, positive)) = active.as_mut() {
            if score >= 0.5 {
                *positive += 1;
            }
            if score >= 0.35 {
                *last = i;
            }
            if i.saturating_sub(*last) >= END_GAP_FRAMES {
                let (start, last, positive) = active.take().expect("active candidate");
                if positive >= policy.interior_min_positive_frames || start == 0 {
                    cores.push(Region {
                        start: start * FRAME,
                        end: ((last + 1) * FRAME).min(real_samples),
                    });
                }
            }
        } else if score >= 0.5 {
            active = Some((i, i, 1));
        }
    }
    if let Some((start, last, positive)) = active {
        // An onset-positive candidate at a real processing-unit edge may be clipped speech.
        if positive >= policy.interior_min_positive_frames || start == 0 || last + 1 == scores.len()
        {
            cores.push(Region {
                start: start * FRAME,
                end: ((last + 1) * FRAME).min(real_samples),
            });
        }
    }
    Ok(cores)
}

// Frozen test comparison: historical five-frame selection versus the production
// configuration-3 selection. The diagnostic still reports qualified cores.
#[cfg(test)]
pub(super) fn diagnostic_selection(
    scores: &[f32],
    real_samples: usize,
    final_candidate: bool,
) -> Result<(Vec<Region>, Vec<Region>)> {
    let policy = RegionPolicy {
        interior_min_positive_frames: if final_candidate { 6 } else { 5 },
    };
    let cores = qualified_cores(scores, real_samples, policy)?;
    let regions = pad_and_bound_cores(
        cores.clone(),
        real_samples,
        if final_candidate {
            MIN_REAL_REGION_SAMPLES
        } else {
            0
        },
    )?;
    Ok((cores, regions))
}

fn pad_and_bound_cores(
    cores: Vec<Region>,
    real_samples: usize,
    minimum_context_samples: usize,
) -> Result<Vec<Region>> {
    let mut padded: Vec<Region> = Vec::new();
    for core in cores {
        let mut next = Region {
            start: core.start.saturating_sub(CONTEXT_SAMPLES),
            end: core.end.saturating_add(CONTEXT_SAMPLES).min(real_samples),
        };
        // Add available real context symmetrically; redistribute at a unit edge.
        // This cannot qualify rejected evidence or extend beyond the original unit.
        let needed = minimum_context_samples
            .min(real_samples)
            .saturating_sub(next.end - next.start);
        let left = (needed / 2).min(next.start);
        next.start -= left;
        let right = (needed - left).min(real_samples - next.end);
        next.end += right;
        next.start = next.start.saturating_sub(needed - left - right);
        if let Some(prev) = padded.last_mut() {
            if next.start <= prev.end {
                prev.end = prev.end.max(next.end);
                continue;
            }
        }
        padded.push(next);
    }
    // Deterministic full-coverage partition. Quality across a forced continuous-speech
    // split is a separate release gate: never silently omit the tail.
    let mut bounded = Vec::new();
    for region in padded {
        let mut start = region.start;
        while start < region.end {
            let end = start.saturating_add(MAX_SAMPLES).min(region.end);
            bounded.push(Region { start, end });
            start = end;
        }
    }
    Ok(bounded)
}

fn millis(samples: usize) -> Result<u32> {
    let value = (samples as u64)
        .checked_mul(1000)
        .ok_or_else(|| Error::Backend("speech gate timestamp overflow".into()))?
        / RATE as u64;
    u32::try_from(value)
        .map_err(|_| Error::Backend("speech gate duration exceeds supported bound".into()))
}

/// The supplied decoder is only invoked for retained regions; its implementation
/// must create an isolated native state per call. No partial result escapes on error.
pub(crate) fn decode_regions(
    pcm: &[f32],
    regions: &[Region],
    opts: &super::SttOptions,
    mut decode: impl FnMut(Vec<f32>) -> Result<TimedTranscript>,
) -> Result<TimedTranscript> {
    let duration_ms = millis(pcm.len())?;
    let mut all = Vec::new();
    let mut languages = Vec::new();
    let mut last_start = 0;
    let mut last_end = 0;
    for &region in regions {
        check_cancelled(opts.cancellation.as_ref())?;
        if region.start < last_end || region.start >= region.end || region.end > pcm.len() {
            return Err(Error::Backend("invalid speech decode region".into()));
        }
        last_end = region.end;
        let mut input = pcm[region.start..region.end].to_vec();
        input.resize(input.len().max(MIN_DECODE_SAMPLES), 0.0);
        let decoded = decode(input)?;
        check_cancelled(opts.cancellation.as_ref())?;
        let real_ms = millis(region.end - region.start)?;
        let offset = millis(region.start)?;
        let mut contributed = false;
        for segment in decoded.segments {
            if segment.end_ms < segment.start_ms
                || segment.end_ms > real_ms.saturating_add(WINDOW_MS)
            {
                return Err(Error::Backend(
                    "invalid region-local Whisper timestamp".into(),
                ));
            }
            if segment.start_ms >= real_ms {
                continue;
            } // artificial padding or native-window tail
            let start_ms = offset
                .checked_add(segment.start_ms)
                .ok_or_else(|| Error::Backend("speech gate timestamp overflow".into()))?;
            let end_ms = offset
                .checked_add(segment.end_ms.min(real_ms))
                .ok_or_else(|| Error::Backend("speech gate timestamp overflow".into()))?;
            if start_ms < last_start {
                return Err(Error::Backend("speech gate timestamp regresses".into()));
            }
            last_start = start_ms;
            if !segment.text.trim().is_empty() {
                contributed = true;
                all.push(TimedSegment {
                    start_ms,
                    end_ms,
                    ..segment
                });
            }
        }
        if contributed {
            languages.push(decoded.language);
        }
    }
    let language = if languages.is_empty() {
        None
    } else if opts.language.is_some() {
        // A persisted explicit hint remains authoritative for the meeting profile.
        opts.language.clone()
    } else if languages.iter().all(|l| l.is_some() && l == &languages[0]) {
        languages[0].clone()
    } else {
        None
    };
    let text = all
        .iter()
        .map(|s| s.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(TimedTranscript {
        text,
        language,
        duration_ms,
        segments: all,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::SttOptions;
    #[test]
    fn off_policy_never_constructs_a_failing_detector_worker() {
        let default = SttOptions::default(); // dictation and generic timed STT
        assert!(only_when_enabled(default.speech_gate, || -> Result<()> {
            panic!("VAD factory invoked by dictation")
        })
        .is_none());
        assert!(
            only_when_enabled(super::super::SpeechGateMode::Off, || -> Result<()> {
                Err(Error::Backend("VAD unavailable".into()))
            })
            .is_none()
        );
        assert!(
            only_when_enabled(super::super::SpeechGateMode::MeetingMicrophone, || 42).is_some()
        );
    }

    fn evidence(n: usize, start: usize, count: usize) -> Vec<f32> {
        let mut v = vec![0.; n];
        v[start..start + count].fill(0.8);
        v
    }
    #[test]
    #[ignore = "requires an approved external public-speech manifest"]
    fn public_short_speech_policy_evidence_probe() {
        let manifest_path = std::env::var_os("MURMUR_APPROVED_PUBLIC_SPEECH_MANIFEST")
            .expect("set the external manifest for this ignored test");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
        let sources = manifest["audio_sources"].as_array().unwrap();
        assert!(!sources.is_empty() && sources.len() <= 24);
        let mut retained_5 = 0;
        let mut retained_6 = 0;
        let mut positive_under_6 = 0;
        for source in sources {
            let path = std::path::Path::new(source["wav_path"].as_str().unwrap());
            assert!(path.is_absolute() && path.exists());
            let pcm = crate::stt::whisper::read_mono_16k_from_wav(path).unwrap();
            let scores = scan(&pcm, &SttOptions::default()).unwrap();
            let positives = scores.iter().filter(|score| **score >= 0.5).count();
            positive_under_6 += usize::from(positives < 6);
            retained_5 += usize::from(
                !regions_with_policy(
                    &scores,
                    pcm.len(),
                    RegionPolicy {
                        interior_min_positive_frames: 5,
                    },
                )
                .unwrap()
                .is_empty(),
            );
            retained_6 += usize::from(
                !regions_with_policy(
                    &scores,
                    pcm.len(),
                    RegionPolicy {
                        interior_min_positive_frames: 6,
                    },
                )
                .unwrap()
                .is_empty(),
            );
        }
        println!("public short speech evidence: clips={} positive_under_6={} retained_5={} retained_6={}", sources.len(), positive_under_6, retained_5, retained_6);
    }

    #[test]
    fn final_candidate_preserves_evidence_and_extends_only_qualified_context() {
        let five = evidence(200, 50, 5);
        let (_, original) = diagnostic_selection(&five, 200 * FRAME, false).unwrap();
        assert_eq!(original, historical_regions(&five, 200 * FRAME).unwrap());
        assert_eq!(
            diagnostic_selection(&five, 200 * FRAME, true).unwrap().1,
            regions(&five, 200 * FRAME).unwrap()
        );
        assert!(diagnostic_selection(&five, 200 * FRAME, true)
            .unwrap()
            .1
            .is_empty());
        let six = evidence(200, 50, 6);
        let (old_cores, _) = diagnostic_selection(&six, 200 * FRAME, false).unwrap();
        let (new_cores, expanded) = diagnostic_selection(&six, 200 * FRAME, true).unwrap();
        assert_eq!(old_cores, new_cores);
        assert_eq!(expanded, regions(&six, 200 * FRAME).unwrap());
        assert_eq!(expanded[0].end - expanded[0].start, 27_200);
        assert!(expanded[0].start <= new_cores[0].start);
        assert!(expanded[0].end >= new_cores[0].end);
        let short = evidence(54, 9, 7);
        let (_, full) = diagnostic_selection(&short, 27_200, true).unwrap();
        assert_eq!(
            full,
            vec![Region {
                start: 0,
                end: 27_200
            }]
        );
        let edge = evidence(2, 0, 1);
        assert_eq!(
            diagnostic_selection(&edge, 700, true).unwrap().1,
            vec![Region { start: 0, end: 700 }]
        );
        let mut nearby = evidence(200, 50, 6);
        nearby[80..86].fill(0.8);
        let (_, merged) = diagnostic_selection(&nearby, 200 * FRAME, true).unwrap();
        assert_eq!(merged.len(), 1);
        let long = vec![0.8; 16_000 * 121 / FRAME];
        let (_, split) = diagnostic_selection(&long, long.len() * FRAME, true).unwrap();
        assert!(split.windows(2).all(|pair| pair[0].end == pair[1].start));
        assert!(split.iter().all(|r| r.end - r.start <= MAX_SAMPLES));
        assert_eq!(split.last().unwrap().end, long.len() * FRAME);
    }

    #[test]
    fn noise_and_four_frame_blip_do_not_unlock_a_unit() {
        assert!(regions(&vec![0.; 9375], 300 * RATE).unwrap().is_empty());
        assert!(regions(&evidence(9375, 200, 4), 300 * RATE)
            .unwrap()
            .is_empty());
        assert!(regions(&evidence(9375, 200, 1), 300 * RATE)
            .unwrap()
            .is_empty());
        // The production candidate rejects the historical five-frame trigger.
        assert!(regions(&evidence(9375, 200, 5), 300 * RATE)
            .unwrap()
            .is_empty());
        assert!(regions_with_policy(
            &evidence(9375, 200, 5),
            300 * RATE,
            RegionPolicy {
                interior_min_positive_frames: 6
            }
        )
        .unwrap()
        .is_empty());
        assert!(regions(&[f32::NAN], FRAME).is_err());
        assert!(regions(&[], FRAME).is_err());
    }
    #[test]
    fn production_selection_rejects_five_frame_noise_and_keeps_real_short_context() {
        let pcm = vec![0.; 1700 * RATE / 1000];
        let five = evidence(pcm.len().div_ceil(FRAME), 12, 5);
        let rejected = regions(&five, pcm.len()).unwrap();
        assert!(rejected.is_empty());
        assert!(
            decode_regions(&pcm, &rejected, &SttOptions::default(), |_| {
                panic!("five-frame interior noise must not reach Whisper")
            })
            .unwrap()
            .segments
            .is_empty()
        );

        let six = evidence(pcm.len().div_ceil(FRAME), 12, 6);
        let accepted = regions(&six, pcm.len()).unwrap();
        assert_eq!(
            accepted,
            vec![Region {
                start: 0,
                end: pcm.len()
            }]
        );
        assert_eq!(
            accepted,
            diagnostic_selection(&six, pcm.len(), true).unwrap().1
        );
        let mut calls = 0;
        decode_regions(&pcm, &accepted, &SttOptions::default(), |input| {
            calls += 1;
            assert_eq!(input.len(), pcm.len()); // real context, not artificial zeros
            Ok(TimedTranscript {
                text: String::new(),
                language: None,
                duration_ms: 1700,
                segments: vec![],
            })
        })
        .unwrap();
        assert_eq!(calls, 1);
    }

    // Characterization of the failed historical quality gate, not acoustic acceptance.
    // These use fabricated scores/PCM: real silence and speech still need replay.
    #[test]
    fn five_frame_false_acceptance_can_produce_in_bounds_text() {
        let pcm = vec![0.; 300 * RATE];
        let mut scores = vec![0.; pcm.len() / FRAME];
        scores[146..151].fill(0.6); // Same count/location as the recorded aggregate.
        let selected = historical_regions(&scores, pcm.len()).unwrap();
        assert_eq!(
            selected,
            vec![Region {
                start: 146 * FRAME - CONTEXT_SAMPLES,
                end: 151 * FRAME + CONTEXT_SAMPLES,
            }]
        );
        let mut calls = 0;
        let result = decode_regions(&pcm, &selected, &SttOptions::default(), |input| {
            calls += 1;
            assert_eq!(input.len(), MIN_DECODE_SAMPLES);
            Ok(TimedTranscript {
                text: "fictional output".into(),
                language: Some("en".into()),
                duration_ms: 1100,
                segments: vec![TimedSegment {
                    start_ms: 0,
                    end_ms: 500,
                    text: "fictional output".into(),
                    confidence: None,
                }],
            })
        })
        .unwrap();
        assert_eq!(calls, 1);
        // Mapping is not a second acoustic classifier or text censor.
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].start_ms, 4372);
        assert_eq!(result.duration_ms, 300_000);
    }

    #[test]
    fn historical_six_frame_minimum_alone_does_not_restore_context() {
        let six = RegionPolicy {
            interior_min_positive_frames: 6,
        };
        for count in [6, 7, 12] {
            let scores = evidence(100, 25, count);
            assert_eq!(
                historical_regions(&scores, 100 * FRAME).unwrap(),
                regions_with_policy(&scores, 100 * FRAME, six).unwrap()
            );
        }
        // Conversely, a five-positive-frame genuine response would be lost too.
        // Fabricated evidence cannot tell that response apart from a noise blip.
        let scores = evidence(100, 25, 5);
        assert_eq!(historical_regions(&scores, 100 * FRAME).unwrap().len(), 1);
        assert!(regions_with_policy(&scores, 100 * FRAME, six)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn historical_short_region_does_not_preserve_the_padded_baseline_input() {
        // Simulate a one-second source plus the benchmark's 700-ms trailing zeros.
        // Marker samples test slicing only, not speech recall or VAD inference.
        let mut pcm = vec![0.; 1700 * RATE / 1000];
        pcm[12 * FRAME..18 * FRAME].fill(0.1);
        let scores = evidence(pcm.len().div_ceil(FRAME), 12, 6);
        let selected = historical_regions(&scores, pcm.len()).unwrap();
        assert_eq!(selected.len(), 1);
        let real = &pcm[selected[0].start..selected[0].end];
        assert_eq!(real.len(), 792 * RATE / 1000);
        let result = decode_regions(&pcm, &selected, &SttOptions::default(), |input| {
            assert_eq!(&input[..real.len()], real);
            assert!(input[real.len()..].iter().all(|sample| *sample == 0.));
            assert_eq!(input.len(), 1100 * RATE / 1000);
            assert_ne!(input.len(), pcm.len());
            Ok(TimedTranscript {
                text: "yes".into(),
                language: Some("en".into()),
                duration_ms: 1100,
                segments: vec![TimedSegment {
                    start_ms: 300,
                    end_ms: 492,
                    text: "yes".into(),
                    confidence: None,
                }],
            })
        })
        .unwrap();
        assert_eq!(result.text, "yes");
        assert_eq!(result.segments[0].start_ms, 384);
        assert_eq!(result.segments[0].end_ms, 576);
        assert_eq!(result.duration_ms, 1700);
    }

    #[test]
    fn qualified_short_edge_and_partial_final_frame() {
        let r = historical_regions(&evidence(100, 25, 5), 100 * FRAME).unwrap();
        assert_eq!(
            r,
            vec![Region {
                start: 25 * FRAME - CONTEXT_SAMPLES,
                end: 30 * FRAME + CONTEXT_SAMPLES
            }]
        );
        assert!(!regions(&evidence(100, 0, 1), 100 * FRAME)
            .unwrap()
            .is_empty());
        assert!(!regions(&evidence(100, 99, 1), 99 * FRAME + 1)
            .unwrap()
            .is_empty());
        assert_eq!(
            regions(&[0.9], 1).unwrap(),
            vec![Region { start: 0, end: 1 }]
        );
    }
    #[test]
    fn long_speech_is_fully_covered_and_nearby_regions_coalesce() {
        let scores = evidence(5000, 10, 4900);
        let a = regions(&scores, 5000 * FRAME).unwrap();
        assert!(a.len() > 2);
        assert!(a.iter().all(|r| r.end - r.start <= MAX_SAMPLES));
        assert!(a.windows(2).all(|pair| pair[0].end == pair[1].start));
        let mut scores = vec![0.; 150];
        scores[30..36].fill(0.9);
        scores[40..46].fill(0.9);
        assert_eq!(regions(&scores, 150 * FRAME).unwrap().len(), 1);
        assert_eq!(
            regions(&scores, 150 * FRAME).unwrap(),
            regions(&scores, 150 * FRAME).unwrap()
        );
    }
    #[test]
    fn mapping_and_short_padding_keep_original_timeline() {
        let pcm = vec![0.; 300 * RATE];
        let opts = SttOptions::default();
        let mut calls = 0;
        let result = decode_regions(
            &pcm,
            &[Region {
                start: 130 * RATE,
                end: 130 * RATE + 1000,
            }],
            &opts,
            |audio| {
                calls += 1;
                assert_eq!(audio.len(), MIN_DECODE_SAMPLES);
                Ok(TimedTranscript {
                    text: "ignored".into(),
                    language: Some("en".into()),
                    duration_ms: 1100,
                    segments: vec![
                        TimedSegment {
                            start_ms: 40,
                            end_ms: 120,
                            text: " yes".into(),
                            confidence: None,
                        },
                        TimedSegment {
                            start_ms: 100,
                            end_ms: 200,
                            text: "fake tail".into(),
                            confidence: None,
                        },
                    ],
                })
            },
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result.duration_ms, 300_000);
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].start_ms, 130_040);
        assert_eq!(result.segments[0].end_ms, 130_062);
        assert_eq!(result.text, "yes");
        assert_eq!(result.language.as_deref(), Some("en"));
    }
    #[test]
    fn separated_regions_and_mixed_languages_keep_original_positions() {
        let pcm = vec![0.; 300 * RATE];
        let mut n = 0;
        let result = decode_regions(
            &pcm,
            &[
                Region {
                    start: 5 * RATE,
                    end: 8 * RATE,
                },
                Region {
                    start: 250 * RATE,
                    end: 253 * RATE,
                },
            ],
            &SttOptions::default(),
            |_| {
                n += 1;
                Ok(TimedTranscript {
                    text: "ignored".into(),
                    language: Some(if n == 1 { "en" } else { "es" }.into()),
                    duration_ms: 3000,
                    segments: vec![TimedSegment {
                        start_ms: 400,
                        end_ms: 1200,
                        text: if n == 1 { " yes" } else { " sí" }.into(),
                        confidence: None,
                    }],
                })
            },
        )
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(result.segments[0].start_ms, 5400);
        assert_eq!(result.segments[1].start_ms, 250400);
        assert_eq!(result.segments[1].end_ms, 251200);
        assert_eq!(result.text, "yes sí");
        assert_eq!(result.language, None);
        // The coordinator adds the durable unit's 300,000 ms meeting offset:
        assert_eq!(300_000 + result.segments[1].start_ms, 550_400);
    }

    #[test]
    fn empty_failure_and_cancellation_never_emit_partial_success() {
        let pcm = vec![0.; RATE];
        let mut opts = SttOptions::default();
        assert_eq!(
            decode_regions(&pcm, &[], &opts, |_| panic!("no decoder"))
                .unwrap()
                .segments
                .len(),
            0
        );
        let regions = [
            Region { start: 0, end: 300 },
            Region {
                start: 500,
                end: 800,
            },
        ];
        let mut count = 0;
        assert!(decode_regions(&pcm, &regions, &opts, |_| {
            count += 1;
            if count == 2 {
                Err(Error::Backend("second failed".into()))
            } else {
                Ok(TimedTranscript {
                    text: "ok".into(),
                    language: None,
                    duration_ms: 1100,
                    segments: vec![],
                })
            }
        })
        .is_err());
        opts.cancellation = Some(Arc::new(AtomicBool::new(true)));
        assert!(decode_regions(&pcm, &regions, &opts, |_| panic!("cancelled")).is_err());
    }
}
