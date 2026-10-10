//! Speech-to-text engine abstraction.
//!
//! Concrete impls live in sibling modules and are selected at runtime
//! via the active model id recorded in `ActiveModelsState`.

mod meeting_speech_gate;
pub mod whisper;

use std::path::Path;
use std::sync::{atomic::AtomicBool, Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::{Error, Result};
use crate::focused_app::FocusedApp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
    pub duration_ms: u32,
    pub confidence: Option<f32>,
    /// Which app had focus at the moment we went to paste. Populated by
    /// the pipeline after the engine returns, not by the engine itself —
    /// engines have no business knowing about NSWorkspace. Useful for
    /// M4 history attribution and future per-app insertion strategies.
    #[serde(default)]
    pub app: Option<FocusedApp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimedTranscript {
    pub text: String,
    pub language: Option<String>,
    pub duration_ms: u32,
    pub segments: Vec<TimedSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimedSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpeechGateMode {
    #[default]
    Off,
    MeetingMicrophone,
}

#[derive(Debug, Clone, Default)]
pub struct SttOptions {
    pub language: Option<String>,
    /// Words/names to bias the model toward. Surfaced as Vocabulary in settings (§2.5).
    pub initial_prompt: Option<String>,
    pub translate: bool,
    /// Only the post-meeting microphone track opts in. Dictation and generic STT remain off.
    pub speech_gate: SpeechGateMode,
    /// Cooperative cancellation for long-running inference. Engines that do
    /// not support it may ignore the signal; callers must still await the
    /// engine future before releasing any shared processing permit.
    pub cancellation: Option<Arc<AtomicBool>>,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A partial hypothesis. If `stable == false`, it may be revised in a later event.
    Partial {
        text: String,
        stable: bool,
    },
    Final(Transcript),
}

#[async_trait]
pub trait SttEngine: Send + Sync {
    fn id(&self) -> &'static str;
    async fn transcribe(&self, wav_path: &Path, opts: &SttOptions) -> Result<Transcript>;

    /// Provide a compatibility result for engines that only produce aggregate
    /// transcripts. The aggregate text stays untouched because dictation relies
    /// on its existing spacing semantics; only the synthetic segment is trimmed.
    /// Focused-app attribution remains owned by the dictation pipeline, so it is
    /// intentionally not part of the timed engine result.
    async fn transcribe_timed(
        &self,
        wav_path: &Path,
        opts: &SttOptions,
    ) -> Result<TimedTranscript> {
        if opts.speech_gate != SpeechGateMode::Off {
            return Err(Error::Backend(
                "speech gate unsupported by STT engine".into(),
            ));
        }
        let transcript = self.transcribe(wav_path, opts).await?;
        let segments = if transcript.text.trim().is_empty() {
            Vec::new()
        } else {
            vec![TimedSegment {
                start_ms: 0,
                end_ms: transcript.duration_ms,
                text: transcript.text.trim().to_string(),
                confidence: transcript.confidence,
            }]
        };
        Ok(TimedTranscript {
            text: transcript.text,
            language: transcript.language,
            duration_ms: transcript.duration_ms,
            segments,
        })
    }

    async fn transcribe_stream(
        &self,
        _audio_rx: mpsc::Receiver<Vec<f32>>,
        _opts: &SttOptions,
    ) -> Result<mpsc::Receiver<StreamEvent>> {
        Err(Error::StreamingUnsupported)
    }

    /// Best-effort "warm this engine up" hook. Default is a no-op so
    /// engines that don't need warming (future cloud STT, etc.) don't
    /// have to implement anything. Concrete impls should load the
    /// model + exercise the full inference path on a tiny silent
    /// sample so the first real `transcribe()` after app boot doesn't
    /// pay the cold-load tax. Must never panic,
    /// must never block app startup, must never surface errors to
    /// the user; at worst, the next real `transcribe()` re-pays the
    /// cost we tried to amortize.
    async fn warmup(&self) {
        // Default no-op.
    }

    /// Drop any cached model weights so resident memory is reclaimed.
    /// Called by the idle-unload watcher in `lib.rs` after the user
    /// has been inactive for a configurable window. Default is a
    /// no-op for engines whose memory footprint doesn't grow with
    /// load (cloud STT, etc.). Concrete impls should make this
    /// idempotent — calling on an already-unloaded engine must be
    /// a cheap no-op. The next `transcribe()` call after an unload
    /// pays the cold-load cost again; the eager warmup wired to
    /// hotkey-press hides that cost in the recording window.
    async fn unload(&self) {
        // Default no-op.
    }
}

#[cfg(test)]
mod tests {
    use super::{SpeechGateMode, SttEngine, SttOptions, TimedSegment, Transcript};
    use crate::error::Result;
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct CountingEngine {
        calls: Arc<AtomicUsize>,
        transcript: Transcript,
    }

    #[async_trait]
    impl SttEngine for CountingEngine {
        fn id(&self) -> &'static str {
            "counting"
        }

        async fn transcribe(&self, _wav_path: &Path, _opts: &SttOptions) -> Result<Transcript> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.transcript.clone())
        }
    }

    fn engine_with(transcript: Transcript) -> (CountingEngine, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            CountingEngine {
                calls: Arc::clone(&calls),
                transcript,
            },
            calls,
        )
    }

    #[tokio::test]
    async fn gate_is_default_off_and_unsupported_engine_never_falls_back() {
        assert_eq!(SttOptions::default().speech_gate, SpeechGateMode::Off);
        let (engine, calls) = engine_with(Transcript {
            text: "should not transcribe".into(),
            language: None,
            duration_ms: 100,
            confidence: None,
            app: None,
        });
        let options = SttOptions {
            speech_gate: SpeechGateMode::MeetingMicrophone,
            ..SttOptions::default()
        };
        assert!(engine
            .transcribe_timed(Path::new("unused.wav"), &options)
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn timed_fallback_preserves_aggregate_and_creates_one_segment() {
        let (engine, calls) = engine_with(Transcript {
            text: "  hello world  ".to_string(),
            language: Some("en".to_string()),
            duration_ms: 1_234,
            confidence: Some(0.75),
            app: None,
        });

        let timed = engine
            .transcribe_timed(Path::new("unused.wav"), &SttOptions::default())
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(timed.text, "  hello world  ");
        assert_eq!(timed.language.as_deref(), Some("en"));
        assert_eq!(timed.duration_ms, 1_234);
        assert_eq!(
            timed.segments,
            vec![TimedSegment {
                start_ms: 0,
                end_ms: 1_234,
                text: "hello world".to_string(),
                confidence: Some(0.75),
            }]
        );
    }

    #[tokio::test]
    async fn timed_fallback_omits_empty_and_whitespace_transcripts() {
        for text in ["", "   ", "\n\t"] {
            let (engine, calls) = engine_with(Transcript {
                text: text.to_string(),
                language: None,
                duration_ms: 100,
                confidence: None,
                app: None,
            });

            let timed = engine
                .transcribe_timed(Path::new("unused.wav"), &SttOptions::default())
                .await
                .unwrap();

            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(timed.text, text);
            assert!(timed.segments.is_empty());
        }
    }
}
