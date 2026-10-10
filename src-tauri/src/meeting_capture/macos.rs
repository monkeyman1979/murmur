use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use objc2_foundation::NSProcessInfo;
use parking_lot::Mutex;

use crate::permissions::{self, PermissionState};

use super::clock::CaptureClock;
use super::recorder::{DiskBackedRecorder, RecorderStopResult, TrackInput};
use super::source::{start_microphone_source, CaptureSources, SharedCaptureState, TrackSink};
use super::tap::start_system_audio_source;
use super::timeline::TimelineOriginCell;
use super::writer::{AudioChunk, TrackStats, WriterThread};
use super::AudioCaptureLease;
use super::RecorderStatus;
use super::{Mt0Error, Mt0ErrorCode, Mt0Report, Mt0TrackReport, QUEUE_CAPACITY};

/// Core Audio process taps are macOS 14.2+. The bundle's own
/// `minimumSystemVersion` is 14.4 (see the plan's A6), so this is belt and
/// braces for anyone who forces an older host.
pub(crate) fn is_supported_macos() -> bool {
    let version = NSProcessInfo::processInfo().operatingSystemVersion();
    version.majorVersion > 14 || (version.majorVersion == 14 && version.minorVersion >= 4)
}

#[derive(Debug, Clone)]
pub(super) enum AudioDestination {
    #[cfg(feature = "meeting-capture-debug")]
    Mt0(mpsc::SyncSender<AudioChunk>),
    Recorder(TrackInput),
}

impl AudioDestination {
    pub(super) fn is_recorder(&self) -> bool {
        matches!(self, Self::Recorder(_))
    }

    pub(super) fn try_send(&self, chunk: AudioChunk) -> Result<(), Mt0Error> {
        match self {
            #[cfg(feature = "meeting-capture-debug")]
            Self::Mt0(tx) => tx.try_send(chunk).map_err(|error| match error {
                mpsc::TrySendError::Full(_) => Mt0Error::new(
                    Mt0ErrorCode::WriterQueueOverflow,
                    "bounded MT0 WAV queue filled; capture stopped to avoid dropping audio",
                ),
                mpsc::TrySendError::Disconnected(_) => Mt0Error::new(
                    Mt0ErrorCode::WavWriteFailed,
                    "MT0 WAV writer stopped accepting audio",
                ),
            }),
            Self::Recorder(input) => input.try_send(chunk).map_err(|error| {
                let code = match error.code {
                    crate::captures::CaptureErrorCode::WriterQueueOverflow => {
                        Mt0ErrorCode::WriterQueueOverflow
                    }
                    crate::captures::CaptureErrorCode::ChunkInvalid => {
                        Mt0ErrorCode::UnsupportedAudioFormat
                    }
                    _ => Mt0ErrorCode::WavWriteFailed,
                };
                Mt0Error::new(code, error.message)
            }),
        }
    }
}

fn capture_error_from_mt0(error: Mt0Error) -> crate::captures::CaptureError {
    use crate::captures::CaptureErrorCode;
    let code = match error.code {
        Mt0ErrorCode::UnsupportedMacos | Mt0ErrorCode::UnsupportedPlatform => {
            CaptureErrorCode::UnsupportedMacos
        }
        Mt0ErrorCode::MicrophonePermissionDenied => CaptureErrorCode::MicrophonePermissionDenied,
        Mt0ErrorCode::SystemAudioPermissionDenied => CaptureErrorCode::SystemAudioPermissionDenied,
        Mt0ErrorCode::SourceUnavailable => CaptureErrorCode::SourceUnavailable,
        Mt0ErrorCode::SourceEnded => CaptureErrorCode::SourceEnded,
        Mt0ErrorCode::CaptureBusy => CaptureErrorCode::CaptureBusy,
        Mt0ErrorCode::MicrophoneStartFailed => CaptureErrorCode::MicrophoneStartFailed,
        Mt0ErrorCode::WriterQueueOverflow => CaptureErrorCode::WriterQueueOverflow,
        Mt0ErrorCode::WavCreateFailed | Mt0ErrorCode::WavWriteFailed => {
            CaptureErrorCode::ChunkWriteFailed
        }
        Mt0ErrorCode::AudioFormatChanged => CaptureErrorCode::AudioFormatChanged,
        Mt0ErrorCode::MissingAudioTrack
        | Mt0ErrorCode::UnsupportedAudioFormat
        | Mt0ErrorCode::CallbackBufferTooLarge => CaptureErrorCode::ChunkInvalid,
        Mt0ErrorCode::TeardownFailed => CaptureErrorCode::CaptureInterrupted,
        _ => CaptureErrorCode::SystemAudioStartFailed,
    };
    crate::captures::CaptureError::new(code, error.message)
}

fn mt0_from_capture_error(error: crate::captures::CaptureError) -> Mt0Error {
    let code = match error.code {
        crate::captures::CaptureErrorCode::WriterQueueOverflow => Mt0ErrorCode::WriterQueueOverflow,
        crate::captures::CaptureErrorCode::AudioFormatChanged => Mt0ErrorCode::AudioFormatChanged,
        crate::captures::CaptureErrorCode::ChunkInvalid => Mt0ErrorCode::UnsupportedAudioFormat,
        _ => Mt0ErrorCode::WavWriteFailed,
    };
    Mt0Error::new(code, error.message)
}

pub(super) struct Controller {
    tx: mpsc::Sender<Command>,
}
pub(super) enum RecorderNotice {
    Failure(crate::captures::CaptureError),
    AutomaticStop(Result<RecorderStopResult, crate::captures::CaptureError>),
}

enum Command {
    #[cfg(feature = "meeting-capture-debug")]
    Start(
        PathBuf,
        AudioCaptureLease,
        mpsc::Sender<Result<Mt0Report, Mt0Error>>,
    ),
    StartRecorder(
        DiskBackedRecorder,
        AudioCaptureLease,
        mpsc::Sender<RecorderNotice>,
        Option<String>,
        mpsc::Sender<Result<i64, crate::captures::CaptureError>>,
    ),
    PauseRecorder(i64, mpsc::Sender<Result<(), crate::captures::CaptureError>>),
    ResumeRecorder(i64, mpsc::Sender<Result<(), crate::captures::CaptureError>>),
    CancelRecorder(
        i64,
        mpsc::Sender<Result<RecorderStopResult, crate::captures::CaptureError>>,
    ),
    CheckpointPaused(
        i64,
        i64,
        mpsc::Sender<Result<(), crate::captures::CaptureError>>,
    ),
    StatusRecorder(
        i64,
        mpsc::Sender<Result<RecorderStatus, crate::captures::CaptureError>>,
    ),
    #[cfg(feature = "meeting-capture-debug")]
    Stop(mpsc::Sender<Result<Mt0Report, Mt0Error>>),
    StopRecorder(
        i64,
        mpsc::Sender<Result<RecorderStopResult, crate::captures::CaptureError>>,
    ),
}

impl Controller {
    pub(super) fn spawn(report: Arc<Mutex<Mt0Report>>) -> Self {
        let (tx, rx) = mpsc::channel();
        let report_for_thread = Arc::clone(&report);
        if let Err(error) = thread::Builder::new()
            .name("murmur-mt0-controller".into())
            .spawn(move || controller_loop(rx, report_for_thread))
        {
            let failure = Mt0Error::new(
                Mt0ErrorCode::Internal,
                format!("could not spawn MT0 controller: {error}"),
            );
            let mut current = report.lock();
            current.state = "failed".into();
            current.terminal_error = Some(failure);
        }
        Self { tx }
    }
    #[cfg(feature = "meeting-capture-debug")]
    pub(super) fn start(
        &self,
        output: PathBuf,
        lease: AudioCaptureLease,
    ) -> Result<Mt0Report, Mt0Error> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::Start(output, lease, tx))
            .map_err(|_| Mt0Error::new(Mt0ErrorCode::Internal, "MT0 controller unavailable"))?;
        rx.recv().map_err(|_| {
            Mt0Error::new(Mt0ErrorCode::Internal, "MT0 controller closed during start")
        })?
    }
    #[cfg(feature = "meeting-capture-debug")]
    pub(super) fn stop(&self) -> Result<Mt0Report, Mt0Error> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::Stop(tx))
            .map_err(|_| Mt0Error::new(Mt0ErrorCode::Internal, "MT0 controller unavailable"))?;
        rx.recv().map_err(|_| {
            Mt0Error::new(Mt0ErrorCode::Internal, "MT0 controller closed during stop")
        })?
    }

    pub(super) fn start_recorder(
        &self,
        recorder: DiskBackedRecorder,
        lease: AudioCaptureLease,
        fatal: mpsc::Sender<RecorderNotice>,
        preferred_output: Option<String>,
    ) -> Result<i64, crate::captures::CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::StartRecorder(
                recorder,
                lease,
                fatal,
                preferred_output,
                tx,
            ))
            .map_err(|_| {
                crate::captures::CaptureError::new(
                    crate::captures::CaptureErrorCode::SystemAudioStartFailed,
                    "meeting native capture controller is unavailable",
                )
            })?;
        rx.recv().map_err(|_| {
            crate::captures::CaptureError::new(
                crate::captures::CaptureErrorCode::SystemAudioStartFailed,
                "meeting native capture controller closed during start",
            )
        })?
    }

    pub(super) fn pause_recorder(
        &self,
        capture_id: i64,
    ) -> Result<(), crate::captures::CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::PauseRecorder(capture_id, tx))
            .map_err(|_| {
                crate::captures::CaptureError::new(
                    crate::captures::CaptureErrorCode::CaptureInterrupted,
                    "meeting native controller unavailable",
                )
            })?;
        rx.recv().map_err(|_| {
            crate::captures::CaptureError::new(
                crate::captures::CaptureErrorCode::CaptureInterrupted,
                "meeting pause controller closed",
            )
        })?
    }

    pub(super) fn resume_recorder(
        &self,
        capture_id: i64,
    ) -> Result<(), crate::captures::CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::ResumeRecorder(capture_id, tx))
            .map_err(|_| {
                crate::captures::CaptureError::new(
                    crate::captures::CaptureErrorCode::CaptureInterrupted,
                    "meeting native controller unavailable",
                )
            })?;
        rx.recv().map_err(|_| {
            crate::captures::CaptureError::new(
                crate::captures::CaptureErrorCode::CaptureInterrupted,
                "meeting resume controller closed",
            )
        })?
    }

    pub(super) fn cancel_recorder(
        &self,
        capture_id: i64,
    ) -> Result<RecorderStopResult, crate::captures::CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::CancelRecorder(capture_id, tx))
            .map_err(|_| {
                crate::captures::CaptureError::new(
                    crate::captures::CaptureErrorCode::CaptureInterrupted,
                    "meeting native controller unavailable",
                )
            })?;
        rx.recv().map_err(|_| {
            crate::captures::CaptureError::new(
                crate::captures::CaptureErrorCode::CaptureInterrupted,
                "meeting cancel controller closed",
            )
        })?
    }

    pub(super) fn checkpoint_paused(
        &self,
        capture_id: i64,
        paused_duration_ms: i64,
    ) -> Result<(), crate::captures::CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::CheckpointPaused(
                capture_id,
                paused_duration_ms,
                tx,
            ))
            .map_err(|_| {
                crate::captures::CaptureError::new(
                    crate::captures::CaptureErrorCode::CaptureInterrupted,
                    "meeting native controller unavailable",
                )
            })?;
        rx.recv().map_err(|_| {
            crate::captures::CaptureError::new(
                crate::captures::CaptureErrorCode::CaptureInterrupted,
                "meeting timing controller closed",
            )
        })?
    }

    pub(super) fn status_recorder(
        &self,
        capture_id: i64,
    ) -> Result<RecorderStatus, crate::captures::CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::StatusRecorder(capture_id, tx))
            .map_err(|_| {
                crate::captures::CaptureError::new(
                    crate::captures::CaptureErrorCode::CaptureInterrupted,
                    "meeting native controller unavailable",
                )
            })?;
        rx.recv().map_err(|_| {
            crate::captures::CaptureError::new(
                crate::captures::CaptureErrorCode::CaptureInterrupted,
                "meeting status controller closed",
            )
        })?
    }

    pub(super) fn stop_recorder(
        &self,
        capture_id: i64,
    ) -> Result<RecorderStopResult, crate::captures::CaptureError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::StopRecorder(capture_id, tx))
            .map_err(|_| {
                crate::captures::CaptureError::new(
                    crate::captures::CaptureErrorCode::CaptureInterrupted,
                    "meeting native capture controller is unavailable",
                )
            })?;
        rx.recv().map_err(|_| {
            crate::captures::CaptureError::new(
                crate::captures::CaptureErrorCode::CaptureInterrupted,
                "meeting native capture controller closed during stop",
            )
        })?
    }
}

impl super::RecorderController for Controller {
    fn pause_recorder(&self, capture_id: i64) -> Result<(), crate::captures::CaptureError> {
        Controller::pause_recorder(self, capture_id)
    }

    fn resume_recorder(&self, capture_id: i64) -> Result<(), crate::captures::CaptureError> {
        Controller::resume_recorder(self, capture_id)
    }

    fn status_recorder(
        &self,
        capture_id: i64,
    ) -> Result<super::RecorderStatus, crate::captures::CaptureError> {
        Controller::status_recorder(self, capture_id)
    }

    fn checkpoint_paused(
        &self,
        capture_id: i64,
        paused_duration_ms: i64,
    ) -> Result<(), crate::captures::CaptureError> {
        Controller::checkpoint_paused(self, capture_id, paused_duration_ms)
    }

    fn stop_recorder(
        &self,
        capture_id: i64,
    ) -> Result<RecorderStopResult, crate::captures::CaptureError> {
        Controller::stop_recorder(self, capture_id)
    }

    fn cancel_recorder(
        &self,
        capture_id: i64,
    ) -> Result<RecorderStopResult, crate::captures::CaptureError> {
        Controller::cancel_recorder(self, capture_id)
    }
}

struct ActiveCapture {
    sources: CaptureSources,
    system_writer: Option<WriterThread>,
    microphone_writer: Option<WriterThread>,
    recorder: Option<DiskBackedRecorder>,
    capture_lease: Option<AudioCaptureLease>,
    recorder_fatal: Option<mpsc::Sender<RecorderNotice>>,
    system_stats: Arc<TrackStats>,
    microphone_stats: Arc<TrackStats>,
    fatal: Arc<Mutex<Option<Mt0Error>>>,
    clock: Arc<Mutex<CaptureClock>>,
    paused: Arc<AtomicBool>,
    hard_limit_reached: Arc<AtomicBool>,
    run_directory: PathBuf,
    system_path: PathBuf,
    microphone_path: PathBuf,
    started: Instant,
    started_at: u128,
    last_format_check: Instant,
}

enum CaptureTarget {
    #[cfg(feature = "meeting-capture-debug")]
    Mt0 {
        output: PathBuf,
        lease: AudioCaptureLease,
    },
    Recorder {
        recorder: DiskBackedRecorder,
        lease: AudioCaptureLease,
        fatal: mpsc::Sender<RecorderNotice>,
        preferred_output: Option<String>,
    },
}

const RECORDER_STARTUP_READINESS_TIMEOUT: Duration = Duration::from_secs(2);
const RECORDER_STARTUP_READINESS_POLL: Duration = Duration::from_millis(50);

/// Do not acknowledge a meeting as recording until the required microphone
/// has delivered enough callbacks to prove its media clock. System audio is
/// opportunistic after successful construction and may remain silent.
fn wait_for_recorder_readiness(capture: &ActiveCapture) -> Result<(), Mt0Error> {
    let deadline = Instant::now() + RECORDER_STARTUP_READINESS_TIMEOUT;
    loop {
        if let Some(error) = capture
            .recorder
            .as_ref()
            .and_then(DiskBackedRecorder::terminal_error)
        {
            return Err(mt0_from_capture_error(error));
        }
        if let Some(error) = capture.fatal.lock().clone() {
            return Err(error);
        }
        capture.sources.verify_format_invariant()?;
        let microphone = capture
            .sources
            .health_snapshot()
            .into_iter()
            .find(|health| health.track == "microphone")
            .expect("capture sources always include microphone health");
        if microphone.transcribable {
            return Ok(());
        }
        if microphone.format_change_count > 0 {
            return Err(Mt0Error::new(
                Mt0ErrorCode::AudioFormatChanged,
                "microphone format changed during recording startup",
            ));
        }
        if Instant::now() >= deadline {
            return Err(Mt0Error::new(
                Mt0ErrorCode::MicrophoneStartFailed,
                "microphone did not begin delivering valid audio",
            ));
        }
        thread::sleep(RECORDER_STARTUP_READINESS_POLL);
    }
}

fn controller_loop(rx: mpsc::Receiver<Command>, report: Arc<Mutex<Mt0Report>>) {
    let mut active: Option<ActiveCapture> = None;
    let mut completed_recorder: Option<(
        i64,
        Result<RecorderStopResult, crate::captures::CaptureError>,
    )> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            #[cfg(feature = "meeting-capture-debug")]
            Ok(Command::Start(path, lease, reply)) => {
                if active.is_some() {
                    let _ = reply.send(Err(Mt0Error::new(
                        Mt0ErrorCode::CaptureBusy,
                        "an audio capture is already active",
                    )));
                    continue;
                }
                *report.lock() = Mt0Report {
                    state: "starting".into(),
                    ..Mt0Report::default()
                };
                match start_capture(CaptureTarget::Mt0 {
                    output: path,
                    lease,
                }) {
                    Ok(capture) => {
                        sync_report(&report, &capture, "recording", None);
                        let response = report.lock().clone();
                        active = Some(capture);
                        let _ = reply.send(Ok(response));
                    }
                    Err(error) => {
                        *report.lock() = Mt0Report {
                            state: "failed".into(),
                            terminal_error: Some(error.clone()),
                            ..Mt0Report::default()
                        };
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Ok(Command::StartRecorder(recorder, lease, fatal, preferred_output, reply)) => {
                if active.is_some() {
                    let _ = reply.send(Err(crate::captures::CaptureError::new(
                        crate::captures::CaptureErrorCode::CaptureBusy,
                        "another audio capture is already active",
                    )));
                    continue;
                }
                let capture_id = recorder.capture_id();
                match start_capture(CaptureTarget::Recorder {
                    recorder,
                    lease,
                    fatal,
                    preferred_output,
                }) {
                    Ok(capture) => match wait_for_recorder_readiness(&capture) {
                        Ok(()) => {
                            active = Some(capture);
                            completed_recorder = None;
                            if reply.send(Ok(capture_id)).is_err() {
                                // The caller disappeared before accepting
                                // ownership. Tear down instead of leaving an
                                // invisible recorder running.
                                let error = Mt0Error::new(
                                    Mt0ErrorCode::TeardownFailed,
                                    "meeting startup acknowledgement was abandoned",
                                );
                                if let Some(capture) = active.take() {
                                    let (_, _, result) = stop_capture(capture, Some(error));
                                    if let Some(result) = result {
                                        completed_recorder = Some((capture_id, result));
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            let response = capture_error_from_mt0(error.clone());
                            let (_, _, result) = stop_capture(capture, Some(error));
                            if let Some(result) = result {
                                completed_recorder = Some((capture_id, result));
                            }
                            let _ = reply.send(Err(response));
                        }
                    },
                    Err(error) => {
                        let _ = reply.send(Err(capture_error_from_mt0(error)));
                    }
                }
            }
            Ok(Command::PauseRecorder(capture_id, reply)) => {
                let result = active
                    .as_mut()
                    .filter(|capture| {
                        capture
                            .recorder
                            .as_ref()
                            .is_some_and(|recorder| recorder.capture_id() == capture_id)
                    })
                    .ok_or_else(|| {
                        crate::captures::CaptureError::new(
                            crate::captures::CaptureErrorCode::CaptureNotFound,
                            "meeting recorder is not active",
                        )
                    })
                    .and_then(pause_capture);
                let _ = reply.send(result);
            }
            Ok(Command::ResumeRecorder(capture_id, reply)) => {
                let result = active
                    .as_mut()
                    .filter(|capture| {
                        capture
                            .recorder
                            .as_ref()
                            .is_some_and(|recorder| recorder.capture_id() == capture_id)
                    })
                    .ok_or_else(|| {
                        crate::captures::CaptureError::new(
                            crate::captures::CaptureErrorCode::CaptureNotFound,
                            "meeting recorder is not active",
                        )
                    })
                    .and_then(resume_capture);
                let _ = reply.send(result);
            }
            Ok(Command::CancelRecorder(capture_id, reply)) => {
                let result = if let Some(capture) = active.take() {
                    if capture
                        .recorder
                        .as_ref()
                        .is_some_and(|recorder| recorder.capture_id() == capture_id)
                    {
                        let prior_error = capture
                            .recorder
                            .as_ref()
                            .and_then(DiskBackedRecorder::terminal_error)
                            .map(mt0_from_capture_error)
                            .or_else(|| capture.fatal.lock().clone());
                        let (summary, _, result) = stop_capture_mode(capture, prior_error, false);
                        *report.lock() = summary;
                        result.expect("cancel recorder result")
                    } else {
                        active = Some(capture);
                        Err(crate::captures::CaptureError::new(
                            crate::captures::CaptureErrorCode::CaptureNotFound,
                            "active meeting capture ID does not match",
                        ))
                    }
                } else {
                    Err(crate::captures::CaptureError::new(
                        crate::captures::CaptureErrorCode::CaptureNotFound,
                        "meeting recorder is not active",
                    ))
                };
                let _ = reply.send(result);
            }
            Ok(Command::CheckpointPaused(capture_id, paused_duration_ms, reply)) => {
                let result = active
                    .as_ref()
                    .filter(|capture| {
                        capture
                            .recorder
                            .as_ref()
                            .is_some_and(|recorder| recorder.capture_id() == capture_id)
                    })
                    .and_then(|capture| capture.recorder.as_ref())
                    .ok_or_else(|| {
                        crate::captures::CaptureError::new(
                            crate::captures::CaptureErrorCode::CaptureNotFound,
                            "meeting recorder is not active",
                        )
                    })
                    .and_then(|recorder| recorder.checkpoint_paused_duration(paused_duration_ms));
                let _ = reply.send(result);
            }
            Ok(Command::StatusRecorder(capture_id, reply)) => {
                let result = active
                    .as_ref()
                    .filter(|capture| {
                        capture
                            .recorder
                            .as_ref()
                            .is_some_and(|recorder| recorder.capture_id() == capture_id)
                    })
                    .map(|capture| {
                        let clock = capture.clock.lock();
                        Ok(super::RecorderStatus {
                            active_duration_ms: clock.active_duration_ms(),
                            paused_duration_ms: clock
                                .paused_duration_ms_at(crate::history::now_millis()),
                            paused: clock.is_paused(),
                        })
                    })
                    .unwrap_or_else(|| {
                        Err(crate::captures::CaptureError::new(
                            crate::captures::CaptureErrorCode::CaptureNotFound,
                            "meeting recorder is not active",
                        ))
                    });
                let _ = reply.send(result);
            }
            #[cfg(feature = "meeting-capture-debug")]
            Ok(Command::Stop(reply)) => {
                if active
                    .as_ref()
                    .is_some_and(|capture| capture.recorder.is_some())
                {
                    let _ = reply.send(Err(Mt0Error::new(
                        Mt0ErrorCode::CaptureBusy,
                        "a meeting recorder is active",
                    )));
                } else if let Some(capture) = active.take() {
                    let prior_error = capture
                        .fatal
                        .lock()
                        .clone()
                        .or_else(|| {
                            capture
                                .system_writer
                                .as_ref()
                                .and_then(|writer| writer.error.lock().clone())
                        })
                        .or_else(|| {
                            capture
                                .microphone_writer
                                .as_ref()
                                .and_then(|writer| writer.error.lock().clone())
                        });
                    let (summary, error, _) = stop_capture(capture, prior_error);
                    *report.lock() = summary.clone();
                    let _ = reply.send(error.map_or(Ok(summary), Err));
                } else {
                    let last = report.lock().clone();
                    if matches!(last.state.as_str(), "stopped" | "failed") {
                        let _ = reply.send(Ok(last));
                    } else {
                        let _ = reply.send(Err(Mt0Error::new(
                            Mt0ErrorCode::NotActive,
                            "no MT0 capture is active",
                        )));
                    }
                }
            }
            Ok(Command::StopRecorder(capture_id, reply)) => {
                crate::murmur_diag!(
                    "[murmur][sck-diag] StopRecorder command received for capture_id={capture_id}"
                );
                if let Some((completed_id, result)) = completed_recorder.take() {
                    if completed_id == capture_id {
                        let _ = reply.send(result);
                    } else {
                        completed_recorder = Some((completed_id, result));
                        let _ = reply.send(Err(crate::captures::CaptureError::new(
                            crate::captures::CaptureErrorCode::CaptureNotFound,
                            "active meeting capture ID does not match",
                        )));
                    }
                } else if active
                    .as_ref()
                    .and_then(|capture| capture.recorder.as_ref())
                    .is_some_and(|recorder| recorder.capture_id() == capture_id)
                {
                    let capture = active.take().expect("active recorder checked above");
                    // A callback may have latched a fatal source/queue error
                    // just before this explicit stop command won the controller
                    // race. Preserve it so incomplete audio cannot look clean.
                    let prior_error = capture
                        .recorder
                        .as_ref()
                        .and_then(DiskBackedRecorder::terminal_error)
                        .map(mt0_from_capture_error)
                        .or_else(|| capture.fatal.lock().clone());
                    let (summary, _, recorder_result) = stop_capture(capture, prior_error);
                    *report.lock() = summary;
                    let _ = reply.send(recorder_result.expect("durable recorder result"));
                } else {
                    let _ = reply.send(Err(crate::captures::CaptureError::new(
                        crate::captures::CaptureErrorCode::CaptureNotFound,
                        "meeting recorder is not active",
                    )));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(capture) = active.take() {
                    let _ = stop_capture(
                        capture,
                        Some(Mt0Error::new(
                            Mt0ErrorCode::TeardownFailed,
                            "native capture controller shut down",
                        )),
                    );
                }
                break;
            }
        }
        let hard_limit = active.as_ref().is_some_and(|capture| {
            capture.recorder.is_some() && capture.hard_limit_reached.load(Ordering::Acquire)
        });
        if hard_limit {
            crate::murmur_diag!("[murmur][sck-diag] hard_limit_reached branch triggered stop");
            if let Some(mut capture) = active.take() {
                let capture_id = capture
                    .recorder
                    .as_ref()
                    .map(DiskBackedRecorder::capture_id);
                let notify = capture.recorder_fatal.take();
                let (summary, _, result) = stop_capture(capture, None);
                *report.lock() = summary;
                if let (Some(capture_id), Some(result)) = (capture_id, result) {
                    if let Some(notify) = notify {
                        let _ = notify.send(RecorderNotice::AutomaticStop(result.clone()));
                    }
                    completed_recorder = Some((capture_id, result));
                }
            }
        }
        if let Some(capture) = active.as_mut() {
            if capture.last_format_check.elapsed() >= Duration::from_secs(1) {
                capture.last_format_check = Instant::now();
                if let Err(error) = capture.sources.verify_format_invariant() {
                    let mut fatal = capture.fatal.lock();
                    if fatal.is_none() {
                        *fatal = Some(error);
                    }
                }
            }
        }
        if let Some(capture) = active.as_ref() {
            if capture.recorder.is_none() {
                sync_report(&report, capture, "recording", None);
            }
            pad_opportunistic_tracks(capture);
            // Prefer the recorder's exact typed failure. Callback-side
            // compatibility mapping is intentionally lossy for legacy MT0.
            let fatal = capture
                .recorder
                .as_ref()
                .and_then(DiskBackedRecorder::terminal_error)
                .map(mt0_from_capture_error)
                .or_else(|| capture.fatal.lock().clone())
                .or_else(|| {
                    capture
                        .system_writer
                        .as_ref()
                        .and_then(|writer| writer.error.lock().clone())
                })
                .or_else(|| {
                    capture
                        .microphone_writer
                        .as_ref()
                        .and_then(|writer| writer.error.lock().clone())
                });
            if let Some(ref error) = fatal {
                crate::murmur_diag!(
                    "[murmur][sck-diag] poll-loop fatal branch triggered: {error:?}"
                );
            }
            if let Some(error) = fatal {
                if let Some(mut capture) = active.take() {
                    let capture_id = capture
                        .recorder
                        .as_ref()
                        .map(DiskBackedRecorder::capture_id);
                    let fatal_notify = capture.recorder_fatal.take();
                    let fallback = capture_error_from_mt0(error.clone());
                    let (summary, _, recorder_result) = stop_capture(capture, Some(error));
                    *report.lock() = summary;
                    if let (Some(capture_id), Some(result)) = (capture_id, recorder_result) {
                        let notification = result.as_ref().err().cloned().unwrap_or(fallback);
                        if let Some(notify) = fatal_notify {
                            let _ = notify.send(RecorderNotice::Failure(notification));
                        }
                        completed_recorder = Some((capture_id, result));
                    }
                }
            }
        }
    }
}

/// How far behind the media timeline the padding is allowed to reach.
///
/// Padding and audio share one queue, so a pad can never overtake audio that
/// was already enqueued. What it *could* overtake is audio still sitting in a
/// device callback that has not run yet, and the result would be a packet
/// arriving inside ground the writer has already covered.
///
/// It must also stay **larger than [`MAX_ANCHOR_SKEW_NS`]**, and that is the
/// part with a live failure behind it. A source whose media time falls behind
/// real time has two mechanisms pointed at it — the anchor watchdog drags it
/// forward, and padding covers the ground it has not claimed — and if they
/// trigger at the same distance it is a coin flip which one gets there first.
/// When both sat at one second, a system source running at a third of real
/// time (the sample-rate bug found 2026-08-03) had its real audio padded over
/// before the watchdog could re-anchor it, and the capture died on an overlap
/// the writer was right to reject. Ordering them means the watchdog always
/// wins, and padding only ever covers ground nothing is coming back for.
const PAD_LAG_NS: i64 = 3_000_000_000;
const _: () = assert!(
    PAD_LAG_NS > super::timeline::MAX_ANCHOR_SKEW_NS,
    "padding must trail the anchor watchdog, or the two race to correct the same lag"
);

/// Keep an opportunistic source's track level with the capture timeline.
///
/// The system-audio tap only delivers while some other process is playing
/// (debug-log Bug 9), so on a quiet Mac its track would sit at frame zero while
/// the microphone's ran on. Left alone the two drift past `MAX_PENDING_ORDINALS`
/// and the capture fails; filled in one burst when audio finally arrives, the
/// writer stalls long enough to overflow its own queue. Padding on the
/// controller's own tick makes each fill a tick's worth of silence instead.
///
/// The always-on source is normally already past the target, so its pad costs a
/// comparison and nothing else.
fn pad_opportunistic_tracks(capture: &ActiveCapture) {
    let Some(recorder) = capture.recorder.as_ref() else {
        return;
    };
    if capture.paused.load(Ordering::Acquire) {
        return;
    }
    let target_ns = capture.clock.lock().active_end_ns() - PAD_LAG_NS;
    if target_ns > 0 {
        recorder.pad_to(target_ns);
    }
}

/// Pausing genuinely stops device IO rather than gating the callback, so the
/// macOS microphone indicator goes out while a meeting is paused. The devices
/// themselves stay alive, which is what makes resume cheap.
fn pause_capture(capture: &mut ActiveCapture) -> Result<(), crate::captures::CaptureError> {
    capture.paused.store(true, Ordering::Release);
    if let Err(error) = capture.sources.pause() {
        capture.paused.store(false, Ordering::Release);
        return Err(capture_error_from_mt0(error));
    }
    let recorder = capture.recorder.as_ref().ok_or_else(|| {
        crate::captures::CaptureError::new(
            crate::captures::CaptureErrorCode::CaptureNotFound,
            "MT0 capture cannot be paused through the meeting recorder path",
        )
    })?;
    recorder.pause()?;
    capture.clock.lock().pause_at(crate::history::now_millis());
    Ok(())
}

fn resume_capture(capture: &mut ActiveCapture) -> Result<(), crate::captures::CaptureError> {
    if !capture.clock.lock().is_paused() {
        return Ok(());
    }

    // Where the pause froze the media timeline. Every source is re-armed at
    // this one point, which is what puts them back on a shared timeline — and
    // it comes from the clock rather than from any device, so no source's
    // silence during the pause can shift it. See Bug 8 for what reading a
    // device clock here costs.
    let resume_at_ns = capture.clock.lock().active_end_ns();

    // The `paused` gate stays closed across the whole transaction: the IOProcs
    // drop everything while it is set, so no sample can be mapped against a
    // stale anchor between restarting IO and rebasing the clock. A failure here
    // leaves the capture exactly as paused as it was.
    capture
        .sources
        .resume(resume_at_ns)
        .map_err(capture_error_from_mt0)?;
    capture
        .clock
        .lock()
        .resume_at(crate::history::now_millis(), resume_at_ns);
    capture.paused.store(false, Ordering::Release);
    Ok(())
}

fn start_capture(target: CaptureTarget) -> Result<ActiveCapture, Mt0Error> {
    if !is_supported_macos() {
        return Err(Mt0Error::new(
            Mt0ErrorCode::UnsupportedMacos,
            "meeting capture requires macOS 14.4 or newer",
        ));
    }
    // No screen-recording preflight: the tap path never asks for it. The
    // audio-capture grant is requested by CoreAudio itself on the first tap
    // creation, and surfaces here as a `'nope'` OSStatus mapped to
    // SystemAudioPermissionDenied.
    if permissions::current().microphone != PermissionState::Granted {
        return Err(Mt0Error::new(
            Mt0ErrorCode::MicrophonePermissionDenied,
            "Microphone permission is required before meeting capture",
        ));
    }

    let preferred_output = match &target {
        #[cfg(feature = "meeting-capture-debug")]
        CaptureTarget::Mt0 { .. } => None,
        CaptureTarget::Recorder {
            preferred_output, ..
        } => preferred_output.clone(),
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Mt0Error::new(Mt0ErrorCode::Internal, format!("system clock: {e}")))?;
    let (
        run_directory,
        system_path,
        microphone_path,
        system_writer,
        microphone_writer,
        recorder,
        capture_lease,
        recorder_fatal,
        system_destination,
        microphone_destination,
        system_stats,
        microphone_stats,
    ) = match target {
        #[cfg(feature = "meeting-capture-debug")]
        CaptureTarget::Mt0 { output, lease } => {
            let run_directory = output.join(format!("murmur-mt0-{}", now.as_millis()));
            std::fs::create_dir_all(&run_directory).map_err(|e| {
                Mt0Error::new(
                    Mt0ErrorCode::WavCreateFailed,
                    format!("create {}: {e}", run_directory.display()),
                )
            })?;
            #[cfg(unix)]
            std::fs::set_permissions(
                &run_directory,
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )
            .map_err(|e| {
                Mt0Error::new(
                    Mt0ErrorCode::WavCreateFailed,
                    format!("secure {}: {e}", run_directory.display()),
                )
            })?;
            let system_path = run_directory.join("system.wav");
            let microphone_path = run_directory.join("microphone.wav");
            let system_writer = WriterThread::spawn(system_path.clone(), QUEUE_CAPACITY)?;
            let microphone_writer = WriterThread::spawn(microphone_path.clone(), QUEUE_CAPACITY)?;
            let system_stats = Arc::clone(&system_writer.stats);
            let microphone_stats = Arc::clone(&microphone_writer.stats);
            let system_destination = AudioDestination::Mt0(system_writer.sender().clone());
            let microphone_destination = AudioDestination::Mt0(microphone_writer.sender().clone());
            (
                run_directory,
                system_path,
                microphone_path,
                Some(system_writer),
                Some(microphone_writer),
                None,
                Some(lease),
                None,
                system_destination,
                microphone_destination,
                system_stats,
                microphone_stats,
            )
        }
        CaptureTarget::Recorder {
            recorder,
            lease,
            fatal,
            preferred_output: _,
        } => {
            let run_directory = recorder.staging_directory().to_path_buf();
            let system_path = run_directory.join("system-00000.wav");
            let microphone_path = run_directory.join("mic-00000.wav");
            let system_destination = AudioDestination::Recorder(recorder.system_input());
            let microphone_destination = AudioDestination::Recorder(recorder.microphone_input());
            (
                run_directory,
                system_path,
                microphone_path,
                None,
                None,
                Some(recorder),
                Some(lease),
                Some(fatal),
                system_destination,
                microphone_destination,
                Arc::new(TrackStats::default()),
                Arc::new(TrackStats::default()),
            )
        }
    };

    let shared = SharedCaptureState {
        fatal: Arc::new(Mutex::new(None)),
        clock: Arc::new(Mutex::new(CaptureClock::new())),
        paused: Arc::new(AtomicBool::new(false)),
        hard_limit_reached: Arc::new(AtomicBool::new(false)),
        origin: Arc::new(TimelineOriginCell::new()),
    };
    let fatal = Arc::clone(&shared.fatal);
    let clock = Arc::clone(&shared.clock);
    let paused = Arc::clone(&shared.paused);
    let hard_limit_reached = Arc::clone(&shared.hard_limit_reached);

    // Presentation timestamps are emitted relative to the timeline origin the
    // first source to fire claims, so the clock's source epoch is zero.
    clock.lock().start(0);

    // Microphone first, deliberately. It is the source that always delivers, so
    // starting it first means it — not the tap, which may be silent for minutes
    // — claims the timeline origin, and the capture begins when the user
    // pressed start rather than when the meeting happened to make a sound.
    let microphone = start_microphone_source(
        TrackSink {
            destination: microphone_destination,
            stats: Arc::clone(&microphone_stats),
        },
        shared.clone(),
    )?;
    let system = start_system_audio_source(
        TrackSink {
            destination: system_destination,
            stats: Arc::clone(&system_stats),
        },
        shared.clone(),
        preferred_output.as_deref(),
    )?;
    crate::murmur_diag!(
        "[murmur][tap-diag] capture started, mic={:?} system={:?}",
        microphone.format(),
        system.format()
    );

    // Declare both formats up front. An opportunistic source may deliver
    // nothing at all, and the writer still has to be able to produce a silent
    // track for it — which it cannot do without knowing the format.
    if let Some(recorder) = recorder.as_ref() {
        recorder
            .microphone_input()
            .declare_format(microphone.format())
            .map_err(mt0_from_capture_error)?;
        recorder
            .system_input()
            .declare_format(system.format())
            .map_err(mt0_from_capture_error)?;
    }

    Ok(ActiveCapture {
        sources: CaptureSources { microphone, system },
        system_writer,
        microphone_writer,
        recorder,
        capture_lease,
        recorder_fatal,
        system_stats,
        microphone_stats,
        fatal,
        clock,
        paused,
        hard_limit_reached,
        run_directory,
        system_path,
        microphone_path,
        started: Instant::now(),
        started_at: now.as_millis(),
        last_format_check: Instant::now(),
    })
}

fn note_missing_microphone(error: &mut Option<Mt0Error>, finalize_recorder: bool, frames: u64) {
    if finalize_recorder && frames == 0 {
        error.get_or_insert_with(|| {
            Mt0Error::new(
                Mt0ErrorCode::MissingAudioTrack,
                "audio capture produced no valid microphone track",
            )
        });
    }
}

fn stop_capture(
    capture: ActiveCapture,
    prior_error: Option<Mt0Error>,
) -> (
    Mt0Report,
    Option<Mt0Error>,
    Option<Result<RecorderStopResult, crate::captures::CaptureError>>,
) {
    stop_capture_mode(capture, prior_error, true)
}

fn stop_capture_mode(
    capture: ActiveCapture,
    prior_error: Option<Mt0Error>,
    finalize_recorder: bool,
) -> (
    Mt0Report,
    Option<Mt0Error>,
    Option<Result<RecorderStopResult, crate::captures::CaptureError>>,
) {
    let ActiveCapture {
        mut sources,
        system_writer,
        microphone_writer,
        mut recorder,
        capture_lease: _capture_lease,
        recorder_fatal: _recorder_fatal,
        system_stats,
        microphone_stats,
        fatal,
        clock,
        paused: _paused,
        hard_limit_reached: _hard_limit_reached,
        run_directory,
        system_path,
        microphone_path,
        started,
        started_at,
        last_format_check: _,
    } = capture;
    let mut error = prior_error;
    // Re-read the route immediately before teardown as well as on the
    // controller tick. This closes the window where a user switches devices
    // and presses Stop before the next periodic check.
    if let Err(format_error) = sources.verify_format_invariant() {
        error.get_or_insert(format_error);
    }
    // Destroying each IOProc is what proves no callback is still running: it is
    // synchronous and CoreAudio will not return while one is in flight. That
    // replaces ScreenCaptureKit's stop-then-drain-both-queues dance, and it is
    // the same guarantee — no callback can touch a sender or writer after this.
    if let Err(teardown_error) = sources.stop() {
        error.get_or_insert(teardown_error);
    }
    // The explicit stop command can race a final callback. Re-read the fatal
    // slot only after both callback queues are drained so overflow/source loss
    // in that window cannot be reported as a clean recording.
    if let Some(callback_error) = fatal.lock().clone() {
        error.get_or_insert(callback_error);
    }

    // Source health deliberately outlives IOProc state, so this final snapshot
    // cannot race a callback and includes every delivered frame or mismatch.
    let mut source_health = sources.health_snapshot();
    if error.as_ref().is_some_and(|error| {
        matches!(
            error.code,
            Mt0ErrorCode::AudioFormatChanged
                | Mt0ErrorCode::UnsupportedAudioFormat
                | Mt0ErrorCode::CallbackBufferTooLarge
        )
    }) {
        for health in &mut source_health {
            health.transcribable = false;
            health.format_change_count = health.format_change_count.saturating_add(1);
            health
                .diagnosis
                .get_or_insert_with(|| "callback_format_changed".into());
        }
    }
    if let Some(invalid) = source_health.iter().find(|health| !health.transcribable) {
        error.get_or_insert_with(|| {
            Mt0Error::new(
                Mt0ErrorCode::AudioFormatChanged,
                format!(
                    "{} audio is not transcribable: {}",
                    invalid.track,
                    invalid
                        .diagnosis
                        .as_deref()
                        .unwrap_or("format integrity failed")
                ),
            )
        });
    }
    if let Some(recorder) = recorder.as_mut() {
        if let Err(health_error) = recorder.checkpoint_source_health(source_health) {
            error.get_or_insert_with(|| mt0_from_capture_error(health_error));
        }
    }
    drop(sources);

    // Every source has stopped, so the media timeline is final. Level both
    // tracks up to it before the writers finish: an opportunistic source that
    // fell quiet before the end would otherwise leave its last chunk short of
    // the other track's, and the two would not pair. Unlike the tick's padding
    // this one may not be dropped, so it waits for room in the queue.
    if finalize_recorder {
        let capture_end_ns = clock.lock().active_end_ns();
        if let Some(recorder) = recorder.as_ref() {
            if let Err(pad_error) = recorder.pad_to_final(capture_end_ns) {
                error.get_or_insert_with(|| mt0_from_capture_error(pad_error));
            }
        }
    }

    if let Some(system_writer) = system_writer {
        if let Err(writer_error) = system_writer.finish() {
            error.get_or_insert(writer_error);
        }
    }
    if let Some(microphone_writer) = microphone_writer {
        if let Err(writer_error) = microphone_writer.finish() {
            error.get_or_insert(writer_error);
        }
    }
    let paused_duration_ms = clock
        .lock()
        .paused_duration_ms_at(crate::history::now_millis());
    if let Some(recorder) = recorder.as_ref() {
        if let Err(checkpoint_error) = recorder.checkpoint_paused_duration(paused_duration_ms) {
            error.get_or_insert_with(|| mt0_from_capture_error(checkpoint_error));
        }
    }
    // A meeting where nothing ever played through the speakers is a **valid
    // capture with a silent system track**, not a failure. That is the honest
    // description of what happened, and after Phase A2 it is also the common
    // one: the tap only delivers while some other process is playing (Bug 9),
    // so the recorder pads the system track rather than the source filling it.
    //
    // The microphone is different. It is the always-on source — it delivers
    // silence rather than nothing — so a microphone track with no frames means
    // the device failed, and that is worth failing the capture over.
    note_missing_microphone(
        &mut error,
        finalize_recorder,
        microphone_stats.frames.load(Ordering::Relaxed),
    );

    // Assemble the durable handoff only after every native validity check has
    // contributed to `error`. Previously the missing-microphone check happened
    // below this block, so a recorder could return `Ok` and begin transcription
    // even while the native report said the capture had failed.
    let recorder_result = recorder.map(|recorder| {
        let result = if finalize_recorder {
            recorder.stop()
        } else {
            recorder.interrupt()
        };
        match (result, error.as_ref()) {
            (Err(recorder_error), _) => Err(recorder_error),
            (Ok(_), Some(native_error)) => Err(capture_error_from_mt0(native_error.clone())),
            (Ok(mut result), None) => {
                result.paused_duration_ms = paused_duration_ms;
                Ok(result)
            }
        }
    });

    if system_stats.frames.load(Ordering::Relaxed) == 0 {
        crate::murmur_diag!(
            "[murmur][tap-diag] system audio source produced no frames; \
             recorded as a silent track"
        );
    }
    let report = Mt0Report {
        state: if error.is_some() {
            "failed".into()
        } else {
            "stopped".into()
        },
        run_directory: Some(run_directory),
        registered_outputs: super::REGISTERED_OUTPUTS
            .iter()
            .map(|s| (*s).into())
            .collect(),
        queue_capacity: QUEUE_CAPACITY,
        started_at_unix_ms: Some(started_at),
        elapsed_ms: started.elapsed().as_millis() as u64,
        system_audio: track_report(system_path, &system_stats),
        microphone: track_report(microphone_path, &microphone_stats),
        terminal_error: error.clone(),
    };
    (report, error, recorder_result)
}

fn sync_report(
    report: &Mutex<Mt0Report>,
    capture: &ActiveCapture,
    state: &str,
    terminal_error: Option<Mt0Error>,
) {
    *report.lock() = Mt0Report {
        state: state.into(),
        run_directory: Some(capture.run_directory.clone()),
        registered_outputs: super::REGISTERED_OUTPUTS
            .iter()
            .map(|s| (*s).into())
            .collect(),
        queue_capacity: QUEUE_CAPACITY,
        started_at_unix_ms: Some(capture.started_at),
        elapsed_ms: capture.started.elapsed().as_millis() as u64,
        system_audio: track_report(capture.system_path.clone(), &capture.system_stats),
        microphone: track_report(capture.microphone_path.clone(), &capture.microphone_stats),
        terminal_error,
    };
}

fn track_report(path: PathBuf, stats: &TrackStats) -> Mt0TrackReport {
    let first_pts_ns = stats.first_pts_ns.load(Ordering::Relaxed);
    let last_pts_ns = stats.last_pts_ns.load(Ordering::Relaxed);
    Mt0TrackReport {
        path: Some(path),
        frames: stats.frames.load(Ordering::Relaxed),
        input_bytes: stats.input_bytes.load(Ordering::Relaxed),
        queue_high_water: stats.high_water.load(Ordering::Relaxed),
        first_pts_ns: (first_pts_ns != i64::MIN).then_some(first_pts_ns),
        last_pts_ns: (last_pts_ns != i64::MIN).then_some(last_pts_ns),
        inserted_silence_frames: stats.inserted_silence_frames.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalized_capture_requires_real_microphone_frames_before_handoff() {
        let mut error = None;
        note_missing_microphone(&mut error, true, 0);
        assert_eq!(error.unwrap().code, Mt0ErrorCode::MissingAudioTrack);

        let mut error = None;
        note_missing_microphone(&mut error, true, 1);
        assert!(error.is_none());

        let mut error = None;
        note_missing_microphone(&mut error, false, 0);
        assert!(
            error.is_none(),
            "cancel/interruption preserves recovery audio"
        );
    }

    #[test]
    fn missing_microphone_does_not_hide_an_earlier_native_failure() {
        let mut error = Some(Mt0Error::new(
            Mt0ErrorCode::WriterQueueOverflow,
            "earlier failure",
        ));
        note_missing_microphone(&mut error, true, 0);
        assert_eq!(error.unwrap().code, Mt0ErrorCode::WriterQueueOverflow);
    }
}
