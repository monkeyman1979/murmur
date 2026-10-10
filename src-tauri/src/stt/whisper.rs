//! whisper.cpp via `whisper-rs`.
//!
//! Thread-safety: `WhisperContext` is `Send + Sync` and cheap to share; a new
//! `WhisperState` is created per transcription. The model is lazily loaded on
//! the first `transcribe` call so startup stays fast and a missing model
//! surfaces as a clear runtime error (rather than panicking at boot).

use std::fs::File;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::{atomic::Ordering, Arc};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperError,
};

use super::{
    meeting_speech_gate as speech_gate, SpeechGateMode, SttEngine, SttOptions, TimedSegment,
    TimedTranscript, Transcript,
};
use crate::error::{Error, Result};

#[derive(Clone)]
pub struct WhisperEngine {
    pub model_path: PathBuf,
    /// `None` is reserved for the explicit developer-only environment
    /// override. App-managed bytes always carry the exact registry size/pin.
    registry_integrity: Option<(u64, &'static str)>,
    context: Arc<Mutex<Option<Arc<WhisperContext>>>>,
}

impl WhisperEngine {
    pub fn new(model_path: PathBuf, registry_integrity: Option<(u64, &'static str)>) -> Self {
        Self {
            model_path,
            registry_integrity,
            context: Arc::new(Mutex::new(None)),
        }
    }

    fn transcribe_meeting_microphone(
        &self,
        path: &Path,
        opts: &SttOptions,
    ) -> Result<TimedTranscript> {
        speech_gate::check_cancelled(opts.cancellation.as_ref())?;
        // Check the new path's input bounds before invoking the unchanged shared converter.
        let reader = hound::WavReader::open(path)
            .map_err(|_| Error::Backend("meeting speech gate WAV open failed".into()))?;
        let spec = reader.spec();
        if !(1..=8).contains(&spec.channels)
            || !(8_000..=96_000).contains(&spec.sample_rate)
            || spec.bits_per_sample == 0
            || spec.bits_per_sample > 32
            || (spec.sample_format == hound::SampleFormat::Float && spec.bits_per_sample != 32)
            || reader.duration() == 0
            || u64::from(reader.duration()) > u64::from(spec.sample_rate) * 360
        {
            return Err(Error::Backend(
                "meeting speech gate invalid WAV format or duration".into(),
            ));
        }
        drop(reader);
        let pcm = read_mono_16k_from_wav(path)?;
        if pcm.is_empty()
            || pcm.len() > speech_gate::RATE * 360
            || pcm.iter().any(|s| !s.is_finite())
        {
            return Err(Error::Backend(
                "meeting speech gate invalid canonical PCM".into(),
            ));
        }
        speech_gate::check_cancelled(opts.cancellation.as_ref())?;
        let scores = speech_gate::scan(&pcm, opts)?;
        let regions = speech_gate::regions(&scores, pcm.len())?;
        if regions.is_empty() {
            return speech_gate::decode_regions(&pcm, &[], opts, |_| {
                unreachable!("no Whisper for silence")
            });
        }
        speech_gate::check_cancelled(opts.cancellation.as_ref())?;
        let ctx = self.load_context()?;
        speech_gate::decode_regions(&pcm, &regions, opts, |audio| {
            let duration_ms = u32::try_from((audio.len() as u64) * 1000 / 16_000)
                .map_err(|_| Error::Backend("speech gate decode duration overflow".into()))?;
            let result = transcribe_blocking(Arc::clone(&ctx), audio, opts)?;
            Ok(TimedTranscript {
                text: result.text,
                language: result.language,
                duration_ms,
                segments: result.segments,
            })
        })
    }

    fn is_loaded(&self) -> bool {
        self.context.lock().is_some()
    }

    /// Parse and validate a replacement engine before the caller unloads or
    /// swaps the currently active engine.
    pub fn validate_for_activation(&self) -> Result<()> {
        self.load_context().map(|_| ())
    }

    fn load_context(&self) -> Result<Arc<WhisperContext>> {
        let mut slot = self.context.lock();
        if let Some(ctx) = slot.as_ref() {
            return Ok(Arc::clone(ctx));
        }
        if !self.model_path.exists() {
            return Err(Error::ModelNotFound(self.model_path.clone()));
        }

        // Verify app-managed bytes through a no-follow descriptor immediately
        // before every lazy load/reload. On Unix Whisper receives /dev/fd for
        // that same descriptor, so pathname replacement after verification
        // cannot substitute parser input. The explicit developer override is
        // intentionally unverified and read-only.
        let verified_file: Option<File> = match self.registry_integrity {
            Some((expected_bytes, expected_sha256)) => Some(
                crate::model_download::open_verified_model(
                    &self.model_path,
                    expected_bytes,
                    expected_sha256,
                )
                .map_err(|_| Error::Backend("speech model integrity check failed".to_string()))?,
            ),
            None => None,
        };
        #[cfg(unix)]
        let parser_path = if let Some(file) = verified_file.as_ref() {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()))
        } else {
            self.model_path.clone()
        };
        #[cfg(not(unix))]
        let parser_path = self.model_path.clone();
        let path_str = parser_path
            .to_str()
            .ok_or_else(|| Error::Backend("speech model path is invalid".to_string()))?;
        let mut params = WhisperContextParameters::default();
        if whisper_metal_disabled() {
            params.use_gpu(false);
            crate::murmur_dev_log!(
                "[murmur] whisper Metal disabled by MURMUR_WHISPER_DISABLE_METAL"
            );
        }
        let ctx = WhisperContext::new_with_params(path_str, params)
            .map_err(|_| Error::Backend("speech model load failed".to_string()))?;
        // Keep the verified descriptor alive until native parsing has
        // completed. The context owns parsed/mapped state after this call.
        drop(verified_file);
        let arc = Arc::new(ctx);
        *slot = Some(Arc::clone(&arc));
        Ok(arc)
    }
}

fn whisper_metal_disabled() -> bool {
    std::env::var("MURMUR_WHISPER_DISABLE_METAL")
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

#[async_trait]
impl SttEngine for WhisperEngine {
    fn id(&self) -> &'static str {
        "whisper"
    }

    async fn transcribe(&self, wav_path: &Path, opts: &SttOptions) -> Result<Transcript> {
        let timed = self.transcribe_timed(wav_path, opts).await?;
        Ok(Transcript {
            text: timed.text,
            language: timed.language,
            duration_ms: timed.duration_ms,
            confidence: None,
            app: None,
        })
    }

    async fn transcribe_timed(
        &self,
        wav_path: &Path,
        opts: &SttOptions,
    ) -> Result<TimedTranscript> {
        // Dispatch before loading Whisper or even reading the WAV. Dictation and
        // the meeting system track keep the exact previous ungated path.
        if let Some(worker) = speech_gate::only_when_enabled(opts.speech_gate, || {
            let engine = self.clone();
            let path = wav_path.to_path_buf();
            let opts = opts.clone();
            tokio::task::spawn_blocking(move || engine.transcribe_meeting_microphone(&path, &opts))
        }) {
            return worker
                .await
                .map_err(|e| Error::Backend(format!("join error: {e}")))?;
        }
        let ctx = self.load_context()?;
        let samples = read_mono_16k_from_wav(wav_path)?;
        let duration_ms = ((samples.len() as u64) * 1000 / 16_000) as u32;
        let opts = opts.clone();
        let decoded = tokio::task::spawn_blocking(move || transcribe_blocking(ctx, samples, &opts))
            .await
            .map_err(|e| Error::Backend(format!("join error: {e}")))??;
        Ok(TimedTranscript {
            text: decoded.text,
            language: decoded.language,
            duration_ms,
            segments: decoded.segments,
        })
    }

    async fn unload(&self) {
        // parking_lot::Mutex is sync; we hold it just long enough to
        // take the cached `Arc<WhisperContext>`, then drop the lock
        // before the Arc itself drops. Idempotent: `take()` on an
        // already-`None` slot is a no-op. Any in-flight `transcribe`
        // is holding its own `Arc<WhisperContext>` clone, so unload
        // doesn't break a concurrent call — the cache just clears
        // while the in-flight Arc keeps the context alive until that
        // call finishes.
        let prev = {
            let mut slot = self.context.lock();
            slot.take()
        };
        let was_loaded = prev.is_some();
        // Drop the last cached Arc before asking libmalloc to purge freed
        // pages. Keeping `prev` alive until the end of this method delays the
        // actual `whisper_free()` call and makes Activity Monitor continue to
        // charge Murmur for model pages longer than necessary.
        drop(prev);
        // Ask malloc to purge even if this particular call did not own a
        // cached context. A hotkey warmup or in-flight transcription may have
        // dropped the final Arc just after an earlier unload attempt; the idle
        // backstop should still be able to return those now-free pages.
        crate::memory::release_freed_memory_to_os("STT unload");
        if was_loaded {
            crate::murmur_dev_log!("[murmur] stt model unloaded from RAM");
        }
    }

    async fn warmup(&self) {
        // Hotkey press calls warmup every time so the first capture
        // after an idle unload can reload in parallel with audio
        // capture. When the context is already loaded, keep that path
        // a cheap mutex check instead of running a redundant silent
        // transcription that can contend with the real request.
        if self.is_loaded() {
            return;
        }
        if !self.model_path.exists() {
            crate::murmur_dev_log!("[murmur] stt warmup skipped: model not on disk");
            return;
        }
        crate::murmur_dev_log!("[murmur] stt warmup starting");
        // Only load the model context. Older builds also ran a 1-second
        // silent transcription here to pre-warm the per-state Metal path, but
        // quick user captures could then overlap that silent pass with the
        // real transcription. That doubled peak memory and let the final
        // warmup Arc drop *after* the real STT unload had already asked malloc
        // for pressure relief, causing idle RSS to ratchet upward after each
        // dictation. Loading the context still hides the large model I/O cost
        // during recording without creating a competing WhisperState.
        match self.load_context() {
            Ok(_) => crate::murmur_dev_log!("[murmur] stt warmup complete"),
            Err(_) => crate::murmur_dev_log!("[murmur] stt warmup load failed (non-fatal)"),
        }
    }
}

/// Result of one blocking Whisper decode. It includes the aggregate text,
/// detected language, and the timestamped segments extracted from that same
/// `WhisperState`.
struct WhisperDecode {
    text: String,
    language: Option<String>,
    segments: Vec<TimedSegment>,
}

struct WhisperRawSegment {
    text: String,
    t0: i64,
    t1: i64,
}

#[derive(Debug)]
struct NormalizedWhisperSegments {
    text: String,
    segments: Vec<TimedSegment>,
}

/// Run one Whisper inference and extract both aggregate and timed results.
fn transcribe_blocking(
    ctx: Arc<WhisperContext>,
    samples: Vec<f32>,
    opts: &SttOptions,
) -> Result<WhisperDecode> {
    let mut state = ctx
        .create_state()
        .map_err(|e| Error::Backend(format!("whisper create_state: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(
        std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4),
    );
    // Language hint + auto-detect wiring. Two facts from the whisper
    // stack that aren't obvious from the call signature:
    //   1. `FullParams`'s C-level default is `language = "en"` with
    //      `detect_language = false`. So *skipping* `set_language` on
    //      the None branch (what PR #137 shipped) silently locks
    //      Whisper to English — symptom is Russian / Japanese / any
    //      non-English audio coming back as garbled phonetic English.
    //   2. Calling `set_detect_language(true)` is NOT the way to
    //      enable auto-detect during transcription. whisper.cpp's
    //      `detect_language` flag puts the decoder into a
    //      "detect-only" path that returns immediately after picking
    //      a language, producing an **empty transcript**. PR #139
    //      over-corrected into this trap; symptom was every capture
    //      logging `auto-detected language: en (p = 0.99)` followed
    //      by a zero-length transcript.
    //
    // The correct wiring per whisper-rs's own docstring on
    // `set_language` ("For auto-detection, set this to either 'auto'
    // or None"): just pass the language option through. `None` ->
    // nullptr at the C layer -> whisper.cpp auto-detects in-pass and
    // transcribes in the detected language. `Some("ru")` -> locked
    // to Russian. `detect_language` stays at its default `false` and
    // never gets touched.
    params.set_language(opts.language.as_deref());
    if opts.speech_gate == SpeechGateMode::MeetingMicrophone {
        params.set_no_context(true);
        params.set_n_max_text_ctx(0);
    }
    if opts.speech_gate == SpeechGateMode::Off {
        if let Some(prompt) = opts.initial_prompt.as_deref() {
            params.set_initial_prompt(prompt);
        }
    }
    params.set_translate(opts.translate);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    if let Some(cancellation) = opts.cancellation.clone() {
        let callback: Box<dyn FnMut() -> bool> =
            Box::new(move || cancellation.load(Ordering::Acquire));
        params.set_abort_callback_safe::<_, Box<dyn FnMut() -> bool>>(Some(callback));
    }

    let full_result = state.full(params, &samples);
    if opts
        .cancellation
        .as_ref()
        .is_some_and(|signal| signal.load(Ordering::Acquire))
    {
        return Err(Error::Backend("whisper transcription cancelled".into()));
    }
    full_result.map_err(|e| Error::Backend(format!("whisper full: {e}")))?;

    let num_segments = state
        .full_n_segments()
        .map_err(|e| Error::Backend(format!("whisper n_segments: {e}")))?;
    let mut raw_segments = Vec::with_capacity(num_segments.max(0) as usize);
    for i in 0..num_segments {
        let text = segment_text_with_invalid_utf8_fallback(state.full_get_segment_text(i), || {
            state.full_get_segment_bytes(i)
        })
        .map_err(|e| Error::Backend(format!("whisper segment {i} text: {e}")))?;
        let t0 = state
            .full_get_segment_t0(i)
            .map_err(|e| Error::Backend(format!("whisper segment {i} t0: {e}")))?;
        let t1 = state
            .full_get_segment_t1(i)
            .map_err(|e| Error::Backend(format!("whisper segment {i} t1: {e}")))?;
        raw_segments.push(WhisperRawSegment { text, t0, t1 });
    }
    let normalized = normalize_whisper_segments(&raw_segments)?;

    // Language whisper actually settled on. `full_lang_id_from_state`
    // returns `-1` when the state never had a language decoded (empty
    // audio, decoder short-circuit, etc.); treat that as "unknown"
    // rather than inventing an English default. `get_lang_str` maps
    // the id back to the ISO code (`"en"`, `"ru"`, `"es"`, …); it can
    // return `None` if whisper-rs's internal table doesn't recognize
    // the id, which we forward as-is.
    let detected_language = match state.full_lang_id_from_state() {
        Ok(id) if id >= 0 => whisper_rs::get_lang_str(id).map(|s| s.to_string()),
        _ => None,
    };

    Ok(WhisperDecode {
        text: normalized.text,
        language: detected_language,
        segments: normalized.segments,
    })
}

/// Replace only malformed UTF-8 bytes in model-generated text. Other native
/// segment errors still fail closed; valid strings never take the fallback.
fn segment_text_with_invalid_utf8_fallback(
    strict: std::result::Result<String, WhisperError>,
    bytes: impl FnOnce() -> std::result::Result<Vec<u8>, WhisperError>,
) -> std::result::Result<String, WhisperError> {
    match strict {
        Ok(text) => Ok(text),
        Err(WhisperError::InvalidUtf8 { .. }) => {
            bytes().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        }
        Err(error) => Err(error),
    }
}

fn normalize_whisper_segments(
    raw_segments: &[WhisperRawSegment],
) -> Result<NormalizedWhisperSegments> {
    let mut aggregate = String::new();
    let mut segments = Vec::new();
    let mut previous_retained_start = None;

    for (index, raw) in raw_segments.iter().enumerate() {
        // Validate the native range before applying annotation filtering. An
        // invalid annotation is still a malformed Whisper response.
        let start_ms = ticks_to_milliseconds(raw.t0, index, "t0")?;
        let end_ms = ticks_to_milliseconds(raw.t1, index, "t1")?;
        if end_ms < start_ms {
            return Err(Error::Backend(format!(
                "whisper segment {index} has reversed timestamp range"
            )));
        }

        if is_whisper_annotation(&raw.text) {
            continue;
        }
        if let Some(previous_start) = previous_retained_start {
            if start_ms < previous_start {
                return Err(Error::Backend(format!(
                    "whisper segment {index} start timestamp regresses"
                )));
            }
        }
        previous_retained_start = Some(start_ms);
        aggregate.push_str(&raw.text);
        segments.push(TimedSegment {
            start_ms,
            end_ms,
            text: raw.text.trim().to_string(),
            confidence: None,
        });
    }

    Ok(NormalizedWhisperSegments {
        text: aggregate.trim().to_string(),
        segments,
    })
}

fn ticks_to_milliseconds(ticks: i64, index: usize, field: &str) -> Result<u32> {
    if ticks < 0 {
        return Err(Error::Backend(format!(
            "whisper segment {index} {field} timestamp is negative"
        )));
    }
    let milliseconds = ticks.checked_mul(10).ok_or_else(|| {
        Error::Backend(format!(
            "whisper segment {index} {field} timestamp overflows milliseconds"
        ))
    })?;
    u32::try_from(milliseconds).map_err(|_| {
        Error::Backend(format!(
            "whisper segment {index} {field} timestamp exceeds u32 milliseconds"
        ))
    })
}

/// Whisper emits non-speech annotations like `[BLANK_AUDIO]`, `[Music]`,
/// `(silence)`, `(speaking in French)` as standalone segments. They are an
/// artifact of the model's captioning training data, not a user intent to
/// type literal square brackets, so we drop any segment whose trimmed text
/// is entirely wrapped in `[...]` or `(...)`.
fn is_whisper_annotation(segment: &str) -> bool {
    let t = segment.trim();
    if t.is_empty() {
        return true;
    }
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')'))
}

/// Load a WAV file, downmix to mono, resample to 16 kHz — Whisper's
/// expected input. `pub(crate)` so sibling engines can import it if a
/// second STT engine ever earns a shipping slot.
pub(crate) fn read_mono_16k_from_wav(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| Error::Backend(format!("wav open: {e}")))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let src_rate = spec.sample_rate;

    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Backend(format!("wav decode: {e}")))?
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Backend(format!("wav decode: {e}")))?,
    };

    let mono: Vec<f32> = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    Ok(crate::audio::resample_linear(&mono, src_rate, 16_000))
}

#[cfg(test)]
mod speech_gate_diagnostic;

#[cfg(test)]
mod tests {
    use super::{
        is_whisper_annotation, normalize_whisper_segments, read_mono_16k_from_wav,
        segment_text_with_invalid_utf8_fallback, ticks_to_milliseconds, WhisperEngine,
        WhisperRawSegment,
    };
    use crate::error::Error;
    use crate::stt::{SpeechGateMode, SttEngine, SttOptions};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn malformed_whisper_segment_utf8_keeps_valid_words_and_timing() {
        let mut invalid = b"Before ? after".to_vec();
        invalid[7] = 0xff;
        let strict = std::str::from_utf8(&invalid)
            .map(str::to_owned)
            .map_err(whisper_rs::WhisperError::from);
        let repaired = segment_text_with_invalid_utf8_fallback(strict, || Ok(invalid))
            .expect("replace only invalid model output bytes");
        assert_eq!(repaired, "Before \u{fffd} after");
        let normalized =
            normalize_whisper_segments(&[raw(&repaired, 10, 20), raw(" next", 20, 30)])
                .expect("preserve all timed segments");
        assert_eq!(normalized.text, "Before \u{fffd} after next");
        assert_eq!(normalized.segments.len(), 2);
        assert_eq!(
            (
                normalized.segments[0].start_ms,
                normalized.segments[0].end_ms
            ),
            (100, 200)
        );
        assert_eq!(
            (
                normalized.segments[1].start_ms,
                normalized.segments[1].end_ms
            ),
            (200, 300)
        );
    }

    #[test]
    fn valid_text_skips_fallback_and_other_errors_remain_errors() {
        let valid = segment_text_with_invalid_utf8_fallback(Ok("Caf\u{e9}".to_owned()), || {
            panic!("valid UTF-8 must not fetch native bytes")
        });
        assert_eq!(valid.unwrap(), "Caf\u{e9}");
        let error = segment_text_with_invalid_utf8_fallback(
            Err(whisper_rs::WhisperError::NullPointer),
            || panic!("null pointer must not use the UTF-8 fallback"),
        );
        assert!(matches!(error, Err(whisper_rs::WhisperError::NullPointer)));
        let mut invalid_bytes = b"?".to_vec();
        invalid_bytes[0] = 0xff;
        let invalid = std::str::from_utf8(&invalid_bytes)
            .map(str::to_owned)
            .map_err(whisper_rs::WhisperError::from);
        let error = segment_text_with_invalid_utf8_fallback(invalid, || {
            Err(whisper_rs::WhisperError::NullPointer)
        });
        assert!(matches!(error, Err(whisper_rs::WhisperError::NullPointer)));
    }

    fn canonical_input_fixture(rate: u32, channels: u16) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "murmur-canonical-{rate}-{channels}-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for frame in 0..rate {
            for channel in 0..channels {
                let sample = (((frame + u32::from(channel)) % 200) as i16 - 100) * 100;
                writer.write_sample(sample).unwrap();
            }
        }
        writer.finalize().unwrap();
        path
    }

    fn raw(text: &str, t0: i64, t1: i64) -> WhisperRawSegment {
        WhisperRawSegment {
            text: text.to_string(),
            t0,
            t1,
        }
    }

    /// Operator-supplied, read-only private replay diagnostic; never runs in CI
    /// or ordinary app work. Prints only bounded numeric evidence, not paths/audio/text.
    #[test]
    #[ignore = "requires explicitly approved external microphone WAV"]
    fn approved_silent_unit_native_evidence_diagnostic() {
        let path = std::env::var_os("MURMUR_APPROVED_READONLY_MIC_WAV")
            .expect("set the external WAV path explicitly for this ignored test");
        let pcm = read_mono_16k_from_wav(std::path::Path::new(&path)).unwrap();
        let scores = super::speech_gate::scan(&pcm, &SttOptions::default()).unwrap();
        let positive: Vec<_> = scores
            .iter()
            .enumerate()
            .filter(|(_, s)| **s >= 0.5)
            .map(|(i, s)| (i, (f64::from(*s) * 10_000.0).round() as u32))
            .collect();
        let marginal: Vec<_> = scores
            .iter()
            .enumerate()
            .filter(|(_, s)| **s >= 0.3 && **s < 0.5)
            .map(|(i, s)| (i, (f64::from(*s) * 10_000.0).round() as u32))
            .take(32)
            .collect();
        let regions = super::speech_gate::regions(&scores, pcm.len()).unwrap();
        println!("numeric native evidence: frames={} positive_count={} positive_frame_score_x10000={positive:?} marginal_first_32={marginal:?} region_sample_bounds={regions:?}", scores.len(), positive.len());
    }

    #[tokio::test]
    async fn gated_synthetic_silence_succeeds_without_loading_whisper_or_mutating_wav() {
        let path = canonical_input_fixture(16_000, 1);
        // Use actual zero PCM so the detector has no speech; the model path must
        // remain unused. This also runs the real native adapter off the async thread.
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..16_000 {
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();
        let original = std::fs::read(&path).unwrap();
        let engine = WhisperEngine::new(PathBuf::from("nonexistent-whisper-model"), None);
        let gated = SttOptions {
            speech_gate: SpeechGateMode::MeetingMicrophone,
            ..SttOptions::default()
        };
        let timed = engine.transcribe_timed(&path, &gated).await.unwrap();
        assert_eq!(timed.duration_ms, 1000);
        assert!(timed.text.is_empty() && timed.segments.is_empty() && timed.language.is_none());
        assert!(!engine.is_loaded());
        assert_eq!(original, std::fs::read(&path).unwrap());
        assert!(engine
            .transcribe_timed(&path, &SttOptions::default())
            .await
            .is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn meeting_wavs_from_16k_44k1_and_48k_become_bounded_16k_mono() {
        for (rate, channels) in [(16_000, 1), (44_100, 2), (48_000, 2)] {
            let path = canonical_input_fixture(rate, channels);
            let samples = read_mono_16k_from_wav(&path).unwrap();
            assert_eq!(samples.len(), 16_000, "source rate {rate}");
            assert!(samples.iter().all(|sample| sample.is_finite()));
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn filters_blank_audio_annotation() {
        assert!(is_whisper_annotation("[BLANK_AUDIO]"));
        assert!(is_whisper_annotation(" [BLANK_AUDIO] "));
    }

    #[test]
    fn filters_bracketed_and_parenthesized_annotations() {
        assert!(is_whisper_annotation("[Music]"));
        assert!(is_whisper_annotation("(silence)"));
        assert!(is_whisper_annotation("(speaking in French)"));
    }

    #[test]
    fn empty_or_whitespace_is_annotation() {
        assert!(is_whisper_annotation(""));
        assert!(is_whisper_annotation("   "));
        assert!(is_whisper_annotation("\n"));
    }

    #[test]
    fn real_speech_is_not_annotation() {
        assert!(!is_whisper_annotation("hello world"));
        assert!(!is_whisper_annotation("I said [something] to them"));
        assert!(!is_whisper_annotation("1 + 2 = 3"));
    }

    #[test]
    fn converts_whisper_ticks_to_checked_milliseconds() {
        assert_eq!(ticks_to_milliseconds(0, 0, "t0").unwrap(), 0);
        assert_eq!(ticks_to_milliseconds(123, 0, "t0").unwrap(), 1_230);
        assert_eq!(
            ticks_to_milliseconds(429_496_729, 0, "t0").unwrap(),
            u32::MAX - 5
        );
    }

    #[test]
    fn rejects_negative_and_overflowing_ticks() {
        assert!(matches!(
            ticks_to_milliseconds(-1, 2, "t0"),
            Err(Error::Backend(message)) if message.contains("segment 2 t0")
        ));
        assert!(matches!(
            ticks_to_milliseconds(i64::MAX, 3, "t1"),
            Err(Error::Backend(message)) if message.contains("overflows milliseconds")
        ));
        assert!(matches!(
            ticks_to_milliseconds(429_496_730, 4, "t1"),
            Err(Error::Backend(message)) if message.contains("exceeds u32 milliseconds")
        ));
    }

    #[test]
    fn rejects_reversed_ranges_before_filtering() {
        assert!(matches!(
            normalize_whisper_segments(&[raw("speech", 20, 10)]),
            Err(Error::Backend(message)) if message.contains("reversed timestamp range")
        ));
        let error = normalize_whisper_segments(&[raw("[secret]", -1, 0)]).unwrap_err();
        assert!(!format!("{error:?}").contains("secret"));
    }

    #[test]
    fn accepts_nondecreasing_starts_overlap_and_zero_duration() {
        let normalized = normalize_whisper_segments(&[
            raw(" first", 10, 20),
            raw(" second", 10, 15),
            raw(" third", 15, 15),
        ])
        .unwrap();
        assert_eq!(normalized.segments.len(), 3);
        assert_eq!(normalized.segments[1].start_ms, 100);
        assert_eq!(normalized.segments[1].end_ms, 150);
        assert_eq!(normalized.segments[2].start_ms, 150);
    }

    #[test]
    fn rejects_decreasing_retained_starts() {
        assert!(matches!(
            normalize_whisper_segments(&[raw("first", 10, 20), raw("second", 9, 20)]),
            Err(Error::Backend(message)) if message.contains("start timestamp regresses")
        ));
    }

    #[test]
    fn filters_annotations_trims_segments_and_preserves_aggregate_spacing() {
        let normalized = normalize_whisper_segments(&[
            raw(" hello", 0, 10),
            raw(" [Music] ", 10, 20),
            raw(" world ", 20, 30),
            raw(" I said [something] ", 30, 40),
        ])
        .unwrap();

        assert_eq!(normalized.text, "hello world  I said [something]");
        assert_eq!(normalized.segments.len(), 3);
        assert_eq!(normalized.segments[0].text, "hello");
        assert_eq!(normalized.segments[1].text, "world");
        assert_eq!(normalized.segments[2].text, "I said [something]");
        assert!(normalized
            .segments
            .iter()
            .all(|segment| segment.confidence.is_none()));
    }

    #[test]
    fn all_annotations_produce_empty_text_and_no_segments() {
        let normalized = normalize_whisper_segments(&[
            raw("[Music]", 0, 10),
            raw(" (silence) ", 10, 20),
            raw(" \t", 20, 30),
        ])
        .unwrap();
        assert!(normalized.text.is_empty());
        assert!(normalized.segments.is_empty());
    }

    #[test]
    fn omitted_valid_annotations_do_not_participate_in_start_ordering() {
        let normalized =
            normalize_whisper_segments(&[raw("[Music]", 100, 110), raw(" speech", 0, 10)]).unwrap();
        assert_eq!(normalized.segments.len(), 1);
        assert_eq!(normalized.segments[0].start_ms, 0);
    }
}
