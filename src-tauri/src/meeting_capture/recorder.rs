use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;

use parking_lot::Mutex;

use crate::captures::recovery::{
    atomic_write_manifest, CaptureSourceHealth, Manifest, ManifestChunk,
};
use crate::captures::{
    CaptureChunkState, CaptureError, CaptureErrorCode, CaptureTrack, NewCaptureChunk,
};

use super::writer::{
    AudioChunk, PcmFormat, FLAG_BIG_ENDIAN, FLAG_FLOAT, FLAG_NON_INTERLEAVED, FLAG_PACKED,
    FLAG_SIGNED_INTEGER, FORMAT_LINEAR_PCM,
};

pub(crate) const DEFAULT_QUEUE_CAPACITY: usize = 32;
#[cfg(all(target_os = "macos", feature = "meeting-capture"))]
pub(crate) const DEFAULT_CHUNK_DURATION_MS: u64 = 5 * 60 * 1_000;
pub(crate) const MAX_PACKET_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PACKET_PLANES: usize = 64;
/// Fixed structural bound for chunk pairs. Four hours is enforced by the
/// capture clock; repeated pause boundaries consume ordinals too, so this is
/// deliberately much larger than four hours / five minutes.
pub(crate) const MAX_CHUNK_PAIRS: usize = 1_024;
/// One track may finish slightly ahead, but unmatched metadata must stay fixed.
pub(crate) const MAX_PENDING_ORDINALS: usize = 4;
/// Reject pathological source-clock jumps instead of synthesizing unbounded silence.
const MAX_FORWARD_GAP_MS: u64 = 60_000;
#[cfg(test)]
pub(crate) const MAX_QUEUED_AUDIO_BYTES_PER_TRACK: usize =
    DEFAULT_QUEUE_CAPACITY * MAX_PACKET_BYTES;

pub(crate) trait ChunkCheckpoint: Send + Sync + 'static {
    fn commit_pair(
        &self,
        microphone: &NewCaptureChunk,
        system: &NewCaptureChunk,
        active_duration_ms: i64,
    ) -> Result<(), CaptureError>;
}

#[derive(Debug, Clone)]
pub(crate) struct RecorderConfig {
    pub capture_id: i64,
    pub created_at: i64,
    pub directory: PathBuf,
    pub queue_capacity: usize,
    pub chunk_duration_ms: u64,
    #[cfg(test)]
    fail_open: Option<(CaptureTrack, i64)>,
}

impl RecorderConfig {
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    pub(crate) fn production(capture_id: i64, created_at: i64, directory: PathBuf) -> Self {
        Self {
            capture_id,
            created_at,
            directory,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            chunk_duration_ms: DEFAULT_CHUNK_DURATION_MS,
            #[cfg(test)]
            fail_open: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecorderStopResult {
    pub capture_id: i64,
    pub active_duration_ms: i64,
    pub paused_duration_ms: i64,
    pub completed_chunk_pairs: usize,
    pub staging_directory: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct TrackInput {
    tx: mpsc::SyncSender<WriterMessage>,
}

impl TrackInput {
    /// Callback-safe enqueue: bounded, nonblocking, and content-free on error.
    pub(crate) fn try_send(&self, chunk: AudioChunk) -> Result<(), CaptureError> {
        let bytes = chunk.planes.iter().try_fold(0usize, |sum, plane| {
            sum.checked_add(plane.len()).ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::ChunkInvalid,
                    "audio callback buffer size overflowed",
                )
            })
        })?;
        if bytes > MAX_PACKET_BYTES
            || chunk.planes.len() > MAX_PACKET_PLANES
            || chunk.plane_channels.len() > MAX_PACKET_PLANES
            || usize::from(chunk.format.channels) > MAX_PACKET_PLANES
        {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "audio callback buffer exceeded the recorder bound",
            ));
        }
        self.tx
            .try_send(WriterMessage::Audio(chunk))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => CaptureError::new(
                    CaptureErrorCode::WriterQueueOverflow,
                    "bounded meeting audio queue filled; capture must stop",
                ),
                mpsc::TrySendError::Disconnected(_) => CaptureError::new(
                    CaptureErrorCode::ChunkWriteFailed,
                    "meeting chunk writer stopped accepting audio",
                ),
            })
    }

    /// Tell the writer what this track's PCM format is before any audio
    /// arrives, so it can synthesize silence for a source that starts late or
    /// never starts at all.
    ///
    /// Sent from the controller thread, once, immediately after the source's
    /// device is open. It is a blocking send because it must not be dropped:
    /// without it a silent track has no format to write.
    #[cfg_attr(not(feature = "meeting-capture"), allow(dead_code))]
    pub(crate) fn declare_format(&self, format: PcmFormat) -> Result<(), CaptureError> {
        self.tx.send(WriterMessage::Format(format)).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting chunk writer stopped accepting audio",
            )
        })
    }

    /// Fill this track with silence up to `pts_ns` on the capture timeline if
    /// it is behind, and do nothing if it is not.
    ///
    /// This is what keeps an **opportunistic** source honest. The tap only
    /// fires while something is playing (debug-log Bug 9), so on a quiet Mac
    /// the system track would otherwise sit at frame zero while the microphone
    /// track ran on for minutes. Two tracks that drift apart like that blow
    /// past `MAX_PENDING_ORDINALS` and fail the capture; worse, letting the gap
    /// accumulate and filling it in one burst when audio finally arrives stalls
    /// the writer long enough to overflow its own queue.
    ///
    /// So the controller pads on its regular tick, which bounds each fill to
    /// one tick's worth of silence and keeps the two tracks within a chunk of
    /// each other at all times.
    ///
    /// Nonblocking and *droppable*: this shares the audio queue, so ordering
    /// against real audio is guaranteed, and a full queue means audio is
    /// already backed up — the next tick will pad instead.
    #[cfg_attr(not(feature = "meeting-capture"), allow(dead_code))]
    pub(crate) fn pad_to(&self, pts_ns: i64) {
        let _ = self.tx.try_send(WriterMessage::PadTo(pts_ns));
    }

    /// The last pad of a capture, which decides whether the two tracks end on
    /// matching ordinals — so unlike the tick's padding it waits for room in
    /// the queue rather than giving up.
    #[cfg_attr(not(feature = "meeting-capture"), allow(dead_code))]
    pub(crate) fn pad_to_final(&self, pts_ns: i64) -> Result<(), CaptureError> {
        self.tx.send(WriterMessage::PadTo(pts_ns)).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting chunk writer stopped accepting audio",
            )
        })
    }
}

pub(crate) struct DiskBackedRecorder {
    capture_id: i64,
    directory: PathBuf,
    microphone: TrackInput,
    system: TrackInput,
    writers: Vec<WriterThread>,
    manifest: Arc<Mutex<ManifestState>>,
    terminal_error: Arc<Mutex<Option<CaptureError>>>,
    source_health: Vec<CaptureSourceHealth>,
    finished: bool,
}

impl DiskBackedRecorder {
    pub(crate) fn start(
        config: RecorderConfig,
        checkpoint: Arc<dyn ChunkCheckpoint>,
    ) -> Result<Self, CaptureError> {
        if config.capture_id <= 0 || config.queue_capacity == 0 || config.chunk_duration_ms == 0 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "meeting recorder configuration is invalid",
            ));
        }
        create_secure_directory(&config.directory)?;
        let manifest = Manifest::empty(config.capture_id, config.created_at);
        atomic_write_manifest(&config.directory, &manifest).map_err(manifest_write_error)?;
        let manifest = Arc::new(Mutex::new(ManifestState {
            manifest,
            directory: config.directory.clone(),
            pending: BTreeMap::new(),
            checkpoint: Arc::clone(&checkpoint),
        }));
        let terminal_error = Arc::new(Mutex::new(None));
        let (microphone, microphone_writer) = spawn_writer(
            CaptureTrack::Microphone,
            &config,
            Arc::clone(&manifest),
            Arc::clone(&terminal_error),
        )?;
        let (system, system_writer) = match spawn_writer(
            CaptureTrack::System,
            &config,
            Arc::clone(&manifest),
            Arc::clone(&terminal_error),
        ) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = microphone.tx.send(WriterMessage::Interrupt);
                let _ = microphone_writer.join();
                return Err(error);
            }
        };
        Ok(Self {
            capture_id: config.capture_id,
            directory: config.directory,
            microphone,
            system,
            writers: vec![microphone_writer, system_writer],
            manifest,
            terminal_error,
            source_health: Vec::new(),
            finished: false,
        })
    }

    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    pub(crate) fn capture_id(&self) -> i64 {
        self.capture_id
    }

    pub(crate) fn microphone_input(&self) -> TrackInput {
        self.microphone.clone()
    }

    pub(crate) fn system_input(&self) -> TrackInput {
        self.system.clone()
    }

    /// Fill both tracks with silence up to `pts_ns` on the capture timeline.
    ///
    /// Called on the controller's tick and once more just before `stop`. The
    /// always-on source is normally already past this point, so its pad is a
    /// no-op; the opportunistic one is what this exists for.
    #[cfg_attr(not(feature = "meeting-capture"), allow(dead_code))]
    pub(crate) fn pad_to(&self, pts_ns: i64) {
        self.microphone.pad_to(pts_ns);
        self.system.pad_to(pts_ns);
    }

    /// The final levelling pass, just before `stop`. See
    /// [`TrackInput::pad_to_final`].
    #[cfg_attr(not(feature = "meeting-capture"), allow(dead_code))]
    pub(crate) fn pad_to_final(&self, pts_ns: i64) -> Result<(), CaptureError> {
        self.microphone.pad_to_final(pts_ns)?;
        self.system.pad_to_final(pts_ns)
    }

    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    pub(crate) fn staging_directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn terminal_error(&self) -> Option<CaptureError> {
        self.terminal_error.lock().clone()
    }

    /// Persist the source clock/format verdict before writer teardown. This
    /// makes a rate or route failure durable even if finalization later fails.
    #[cfg(all(target_os = "macos", feature = "meeting-capture"))]
    pub(crate) fn checkpoint_source_health(
        &mut self,
        health: Vec<CaptureSourceHealth>,
    ) -> Result<(), CaptureError> {
        self.source_health = health;
        self.manifest
            .lock()
            .set_source_health(self.source_health.clone())
    }

    /// Ordered pause barrier. Both writer threads finish their current short
    /// chunks before the acknowledgement is returned.
    pub(crate) fn pause(&self) -> Result<(), CaptureError> {
        let (microphone_tx, microphone_rx) = mpsc::channel();
        let (system_tx, system_rx) = mpsc::channel();
        self.microphone
            .tx
            .send(WriterMessage::Pause(microphone_tx))
            .map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::ChunkWriteFailed,
                    "microphone writer stopped during pause",
                )
            })?;
        self.system
            .tx
            .send(WriterMessage::Pause(system_tx))
            .map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::ChunkWriteFailed,
                    "system writer stopped during pause",
                )
            })?;
        microphone_rx.recv().map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "microphone pause barrier failed",
            )
        })??;
        system_rx.recv().map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "system pause barrier failed",
            )
        })??;
        self.terminal_error().map_or(Ok(()), Err)
    }

    #[allow(dead_code)]
    pub(crate) fn checkpoint_paused_duration(
        &self,
        paused_duration_ms: i64,
    ) -> Result<(), CaptureError> {
        if paused_duration_ms < 0 {
            return Err(CaptureError::new(
                CaptureErrorCode::InvalidRequest,
                "paused duration cannot be negative",
            ));
        }
        self.manifest.lock().set_paused_duration(paused_duration_ms)
    }

    pub(crate) fn stop(mut self) -> Result<RecorderStopResult, CaptureError> {
        self.finished = true;
        let _ = self.microphone.tx.send(WriterMessage::Finish);
        let _ = self.system.tx.send(WriterMessage::Finish);
        self.join_writers()?;
        enrich_signal_metrics(&self.directory, &mut self.source_health);
        self.manifest
            .lock()
            .set_source_health(self.source_health.clone())?;
        if let Some(error) = self.terminal_error() {
            return Err(error);
        }
        let state = self.manifest.lock();
        if !state.pending.is_empty() {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "meeting tracks did not finalize matching chunk ordinals",
            ));
        }
        Ok(RecorderStopResult {
            capture_id: self.capture_id,
            active_duration_ms: state.manifest.active_duration_ms,
            paused_duration_ms: state.manifest.paused_duration_ms,
            completed_chunk_pairs: state.manifest.chunks.len(),
            staging_directory: self.directory.clone(),
        })
    }

    /// Stop writer threads without publishing their current `.partial` files.
    /// Cancellation uses this path so recoverable staging is retained until
    /// the caller has persisted `deleting` and explicitly confirmed cleanup.
    pub(crate) fn interrupt(mut self) -> Result<RecorderStopResult, CaptureError> {
        self.finished = true;
        let _ = self.microphone.tx.send(WriterMessage::Interrupt);
        let _ = self.system.tx.send(WriterMessage::Interrupt);
        self.join_writers()?;
        if let Some(error) = self.terminal_error() {
            return Err(error);
        }
        let state = self.manifest.lock();
        Ok(RecorderStopResult {
            capture_id: self.capture_id,
            active_duration_ms: state.manifest.active_duration_ms,
            paused_duration_ms: state.manifest.paused_duration_ms,
            completed_chunk_pairs: state.manifest.chunks.len(),
            staging_directory: self.directory.clone(),
        })
    }

    fn join_writers(&mut self) -> Result<(), CaptureError> {
        let mut first_error = None;
        for writer in self.writers.drain(..) {
            if let Err(error) = writer.join() {
                set_terminal(&self.terminal_error, error.clone());
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for DiskBackedRecorder {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.microphone.tx.send(WriterMessage::Interrupt);
            let _ = self.system.tx.send(WriterMessage::Interrupt);
            let _ = self.join_writers();
        }
    }
}

#[derive(Debug)]
enum WriterMessage {
    Audio(AudioChunk),
    /// This track's PCM format, so silence can be written for a source that
    /// has not delivered (or has never delivered) any audio.
    Format(PcmFormat),
    /// Fill silence up to this point on the capture timeline. See
    /// [`TrackInput::pad_to`].
    PadTo(i64),
    Pause(mpsc::Sender<Result<(), CaptureError>>),
    Finish,
    Interrupt,
}

struct WriterThread {
    join: thread::JoinHandle<()>,
    error: Arc<Mutex<Option<CaptureError>>>,
}

impl WriterThread {
    fn join(self) -> Result<(), CaptureError> {
        self.join.join().map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting chunk writer thread panicked",
            )
        })?;
        match self.error.lock().clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn spawn_writer(
    track: CaptureTrack,
    config: &RecorderConfig,
    manifest: Arc<Mutex<ManifestState>>,
    terminal_error: Arc<Mutex<Option<CaptureError>>>,
) -> Result<(TrackInput, WriterThread), CaptureError> {
    let (tx, rx) = mpsc::sync_channel(config.queue_capacity);
    let error = Arc::new(Mutex::new(None));
    let thread_error = Arc::clone(&error);
    let global_error = Arc::clone(&terminal_error);
    let directory = config.directory.clone();
    let chunk_duration_ms = config.chunk_duration_ms;
    let capture_id = config.capture_id;
    #[cfg(test)]
    let fail_open_ordinal = config
        .fail_open
        .filter(|(failed_track, _)| *failed_track == track)
        .map(|(_, ordinal)| ordinal);
    let join = thread::Builder::new()
        .name(format!("murmur-meeting-{}", track.as_str()))
        .spawn(move || {
            if let Err(failure) = writer_loop(
                capture_id,
                track,
                &directory,
                chunk_duration_ms,
                rx,
                manifest,
                #[cfg(test)]
                fail_open_ordinal,
            ) {
                *thread_error.lock() = Some(failure.clone());
                set_terminal(&global_error, failure);
            }
        })
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting chunk writer thread could not start",
            )
        })?;
    Ok((TrackInput { tx }, WriterThread { join, error }))
}

struct OpenChunk {
    ordinal: i64,
    start_frame: u64,
    frames: u64,
    format: PcmFormat,
    partial_path: PathBuf,
    final_path: PathBuf,
    writer: Option<hound::WavWriter<BufWriter<File>>>,
}

fn writer_loop(
    capture_id: i64,
    track: CaptureTrack,
    directory: &Path,
    chunk_duration_ms: u64,
    rx: mpsc::Receiver<WriterMessage>,
    manifest: Arc<Mutex<ManifestState>>,
    #[cfg(test)] fail_open_ordinal: Option<i64>,
) -> Result<(), CaptureError> {
    let mut open: Option<OpenChunk> = None;
    let mut active_frames = 0u64;
    let mut next_ordinal = 0i64;
    let mut current_chunk_start = 0u64;
    let mut initial_format: Option<PcmFormat> = None;
    while let Ok(message) = rx.recv() {
        match message {
            WriterMessage::Audio(chunk) => {
                validate_format(chunk.format)?;
                if let Some(format) = initial_format {
                    if format != chunk.format {
                        return Err(CaptureError::new(
                            CaptureErrorCode::ChunkInvalid,
                            "meeting audio format changed during recording",
                        ));
                    }
                } else {
                    initial_format = Some(chunk.format);
                }
                let format = chunk.format;
                let chunk_frames = duration_frames(chunk_duration_ms, format.sample_rate)?;
                let target_frame = pts_to_frame(chunk.pts_ns, chunk.origin_ns, format.sample_rate)?;
                let first_source_frame = if target_frame < active_frames {
                    let overlap = active_frames - target_frame;
                    if overlap > 1 || overlap as usize >= chunk.frames {
                        return Err(CaptureError::new(
                            CaptureErrorCode::ChunkInvalid,
                            "meeting audio timestamps moved backwards or overlapped",
                        ));
                    }
                    overlap as usize
                } else {
                    if target_frame > active_frames {
                        let gap_frames = target_frame - active_frames;
                        let maximum_gap = duration_frames(MAX_FORWARD_GAP_MS, format.sample_rate)?;
                        if gap_frames > maximum_gap {
                            return Err(CaptureError::new(
                                CaptureErrorCode::ChunkInvalid,
                                "meeting audio timestamp gap exceeded the writer bound",
                            ));
                        }
                        write_silence_frames(
                            track,
                            directory,
                            format,
                            chunk_frames,
                            gap_frames,
                            &mut active_frames,
                            &mut open,
                            &manifest,
                            &mut next_ordinal,
                            &mut current_chunk_start,
                            #[cfg(test)]
                            fail_open_ordinal,
                        )?;
                    }
                    0
                };
                for frame in first_source_frame..chunk.frames {
                    ensure_open_chunk(
                        track,
                        directory,
                        format,
                        chunk_frames,
                        active_frames,
                        next_ordinal,
                        current_chunk_start,
                        &mut open,
                        #[cfg(test)]
                        fail_open_ordinal,
                    )?;
                    let current = open.as_mut().expect("open chunk initialized");
                    write_frame(current, &chunk, frame)?;
                    current.frames += 1;
                    active_frames += 1;
                    publish_if_full(
                        current.frames,
                        chunk_frames,
                        track,
                        &mut open,
                        &manifest,
                        &mut next_ordinal,
                        &mut current_chunk_start,
                    )?;
                }
            }
            WriterMessage::Format(format) => {
                validate_format(format)?;
                match initial_format {
                    // The declaration races the source's own first packet —
                    // both are sent once the device is open — so either order
                    // is legal, but they have to agree.
                    Some(existing) if existing != format => {
                        return Err(CaptureError::new(
                            CaptureErrorCode::ChunkInvalid,
                            "meeting audio format changed during recording",
                        ))
                    }
                    Some(_) => {}
                    None => initial_format = Some(format),
                }
            }
            WriterMessage::PadTo(pts_ns) => {
                // Before the format is known there is nothing to write silence
                // in, and nothing has been written that could need aligning.
                let Some(format) = initial_format else {
                    continue;
                };
                let target_frame = pts_to_frame(pts_ns.max(0), 0, format.sample_rate)?;
                if target_frame > active_frames {
                    let chunk_frames = duration_frames(chunk_duration_ms, format.sample_rate)?;
                    write_silence_frames(
                        track,
                        directory,
                        format,
                        chunk_frames,
                        target_frame - active_frames,
                        &mut active_frames,
                        &mut open,
                        &manifest,
                        &mut next_ordinal,
                        &mut current_chunk_start,
                        #[cfg(test)]
                        fail_open_ordinal,
                    )?;
                }
            }
            WriterMessage::Pause(reply) => {
                let result = if let Some(open_chunk) = open.take() {
                    let result = finalize_chunk(open_chunk, track)
                        .and_then(|completed| manifest.lock().publish(completed));
                    if result.is_ok() {
                        next_ordinal = next_ordinal.saturating_add(1);
                        current_chunk_start = active_frames;
                    }
                    result
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            WriterMessage::Finish => {
                let Some(open_chunk) = open.take() else {
                    if active_frames == 0 {
                        return Err(CaptureError::new(
                            CaptureErrorCode::ChunkInvalid,
                            "meeting track contained no audio frames",
                        ));
                    }
                    return Ok(());
                };
                let completed = finalize_chunk(open_chunk, track)?;
                manifest.lock().publish(completed)?;
                return Ok(());
            }
            WriterMessage::Interrupt => {
                drop(open.take());
                return Ok(());
            }
        }
    }
    drop(open.take());
    let _ = capture_id;
    Ok(())
}

fn pts_to_frame(pts_ns: i64, origin_ns: i64, sample_rate: u32) -> Result<u64, CaptureError> {
    if pts_ns < origin_ns {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting audio timestamp preceded the capture origin",
        ));
    }
    let delta = u128::try_from(pts_ns - origin_ns).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting audio timestamp is invalid",
        )
    })?;
    let frames = delta
        .checked_mul(u128::from(sample_rate))
        .map(|value| value / 1_000_000_000)
        .ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "meeting audio timestamp overflowed",
            )
        })?;
    u64::try_from(frames).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting audio timestamp exceeded the writer range",
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn write_silence_frames(
    track: CaptureTrack,
    directory: &Path,
    format: PcmFormat,
    chunk_frames: u64,
    frames: u64,
    active_frames: &mut u64,
    open: &mut Option<OpenChunk>,
    manifest: &Arc<Mutex<ManifestState>>,
    next_ordinal: &mut i64,
    current_chunk_start: &mut u64,
    #[cfg(test)] fail_open_ordinal: Option<i64>,
) -> Result<(), CaptureError> {
    let mut remaining = frames;
    while remaining > 0 {
        ensure_open_chunk(
            track,
            directory,
            format,
            chunk_frames,
            *active_frames,
            *next_ordinal,
            *current_chunk_start,
            open,
            #[cfg(test)]
            fail_open_ordinal,
        )?;
        let current = open.as_mut().expect("open chunk initialized");
        // Never write past the current chunk's boundary; `publish_if_full`
        // below closes it and the next pass opens the next ordinal.
        let batch = remaining
            .min(chunk_frames.saturating_sub(current.frames))
            .max(1);
        write_silence_frame_batch(current, format.channels, batch)?;
        current.frames += batch;
        *active_frames += batch;
        remaining -= batch;
        publish_if_full(
            current.frames,
            chunk_frames,
            track,
            open,
            manifest,
            next_ordinal,
            current_chunk_start,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ensure_open_chunk(
    track: CaptureTrack,
    directory: &Path,
    format: PcmFormat,
    _chunk_frames: u64,
    active_frames: u64,
    next_ordinal: i64,
    current_chunk_start: u64,
    open: &mut Option<OpenChunk>,
    #[cfg(test)] fail_open_ordinal: Option<i64>,
) -> Result<(), CaptureError> {
    if open.is_some() {
        return Ok(());
    }
    if next_ordinal < 0 || next_ordinal as usize >= MAX_CHUNK_PAIRS {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting capture exceeded the bounded chunk ordinal range",
        ));
    }
    #[cfg(test)]
    if fail_open_ordinal == Some(next_ordinal) {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "injected meeting writer failure",
        ));
    }
    *open = Some(open_chunk(
        track,
        next_ordinal,
        current_chunk_start.min(active_frames),
        format,
        directory,
    )?);
    Ok(())
}

fn publish_if_full(
    frames: u64,
    chunk_frames: u64,
    track: CaptureTrack,
    open: &mut Option<OpenChunk>,
    manifest: &Arc<Mutex<ManifestState>>,
    next_ordinal: &mut i64,
    current_chunk_start: &mut u64,
) -> Result<(), CaptureError> {
    if frames == chunk_frames {
        let end_frame = open
            .as_ref()
            .map(|chunk| chunk.start_frame.saturating_add(chunk.frames))
            .unwrap_or(*current_chunk_start);
        let completed = finalize_chunk(open.take().expect("full open chunk"), track)?;
        manifest.lock().publish(completed)?;
        *next_ordinal = next_ordinal.saturating_add(1);
        *current_chunk_start = end_frame;
    }
    Ok(())
}

/// Samples per `get_i16_writer` batch. Silence fills are now routine rather
/// than exceptional — an opportunistic source can be quiet for the length of a
/// meeting — so this goes through hound's bulk path, which builds one buffer
/// and issues one write, instead of a `write_sample` call (and a `write_all`)
/// per sample.
const SILENCE_BATCH_SAMPLES: u64 = 8_192;

fn write_silence_frame_batch(
    open: &mut OpenChunk,
    channels: u16,
    frames: u64,
) -> Result<(), CaptureError> {
    let mut remaining = frames.saturating_mul(u64::from(channels.max(1)));
    let writer = open.writer.as_mut().expect("open chunk owns writer");
    while remaining > 0 {
        let batch = remaining.min(SILENCE_BATCH_SAMPLES);
        let mut samples = writer.get_i16_writer(batch as u32);
        for _ in 0..batch {
            samples.write_sample(0_i16);
        }
        samples.flush().map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting alignment silence could not be written",
            )
        })?;
        remaining -= batch;
    }
    Ok(())
}

fn duration_frames(duration_ms: u64, sample_rate: u32) -> Result<u64, CaptureError> {
    duration_ms
        .checked_mul(u64::from(sample_rate))
        .map(|value| (value / 1_000).max(1))
        .ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "meeting chunk duration overflowed",
            )
        })
}

fn open_chunk(
    track: CaptureTrack,
    ordinal: i64,
    start_frame: u64,
    format: PcmFormat,
    directory: &Path,
) -> Result<OpenChunk, CaptureError> {
    let prefix = match track {
        CaptureTrack::Microphone => "mic",
        CaptureTrack::System => "system",
    };
    let basename = format!("{prefix}-{ordinal:05}.wav");
    let final_path = directory.join(&basename);
    let partial_path = directory.join(format!("{basename}.partial"));
    let spec = hound::WavSpec {
        channels: format.channels,
        sample_rate: format.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    if fs::symlink_metadata(&final_path).is_ok() {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "meeting WAV chunk destination already exists",
        ));
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial_path)
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting partial WAV could not be created",
            )
        })?;
    secure_file(&partial_path)?;
    let writer = hound::WavWriter::new(BufWriter::new(file), spec).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "meeting partial WAV header could not be created",
        )
    })?;
    Ok(OpenChunk {
        ordinal,
        start_frame,
        frames: 0,
        format,
        partial_path,
        final_path,
        writer: Some(writer),
    })
}

fn finalize_chunk(
    mut chunk: OpenChunk,
    track: CaptureTrack,
) -> Result<CompletedTrackChunk, CaptureError> {
    if chunk.frames == 0 {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "empty meeting chunk cannot be published",
        ));
    }
    chunk
        .writer
        .take()
        .expect("open chunk owns writer")
        .finalize()
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting WAV header could not be finalized",
            )
        })?;
    File::open(&chunk.partial_path)
        .and_then(|file| file.sync_all())
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting WAV data could not be synchronized",
            )
        })?;
    // A finalized header remains under `.partial` until the matching track is
    // also ready. Pair publication below is the only code allowed to expose a
    // completed-looking `.wav` name.
    let byte_length = fs::metadata(&chunk.partial_path)
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "finalized meeting partial could not be inspected",
            )
        })?
        .len();
    let start_ms = frames_to_ms(chunk.start_frame, chunk.format.sample_rate);
    let end_ms = frames_to_ms(
        chunk.start_frame.saturating_add(chunk.frames),
        chunk.format.sample_rate,
    );
    Ok(CompletedTrackChunk {
        track,
        ordinal: chunk.ordinal,
        start_ms,
        end_ms,
        partial_path: chunk.partial_path,
        final_path: chunk.final_path,
        byte_length: i64::try_from(byte_length).unwrap_or(i64::MAX),
    })
}

fn frames_to_ms(frames: u64, sample_rate: u32) -> i64 {
    let millis = u128::from(frames).saturating_mul(1_000) / u128::from(sample_rate.max(1));
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn write_frame(open: &mut OpenChunk, chunk: &AudioChunk, frame: usize) -> Result<(), CaptureError> {
    let format = chunk.format;
    let bytes_per_sample = (format.bits_per_channel / 8) as usize;
    let planar = format.flags & FLAG_NON_INTERLEAVED != 0;
    let layout_matches = if planar {
        chunk.planes.len() == format.channels as usize
            && chunk.plane_channels.len() == chunk.planes.len()
            && chunk.plane_channels.iter().all(|&count| count == 1)
    } else {
        chunk.planes.len() == 1 && chunk.plane_channels.as_slice() == [u32::from(format.channels)]
    };
    if !layout_matches {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting PCM plane layout is invalid",
        ));
    }
    for channel in 0..format.channels as usize {
        let (plane, offset) = if planar {
            (&chunk.planes[channel], frame * bytes_per_sample)
        } else {
            (
                &chunk.planes[0],
                (frame * format.channels as usize + channel) * bytes_per_sample,
            )
        };
        let bytes = plane
            .get(offset..offset.saturating_add(bytes_per_sample))
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::ChunkInvalid,
                    "meeting PCM buffer is shorter than its declared frame count",
                )
            })?;
        let sample = if format.flags & FLAG_FLOAT != 0 {
            let value = f32::from_le_bytes(bytes.try_into().map_err(|_| {
                CaptureError::new(CaptureErrorCode::ChunkInvalid, "invalid float PCM sample")
            })?)
            .clamp(-1.0, 1.0);
            (value * i16::MAX as f32) as i16
        } else if format.bits_per_channel == 16 {
            i16::from_le_bytes(bytes.try_into().map_err(|_| {
                CaptureError::new(CaptureErrorCode::ChunkInvalid, "invalid 16-bit PCM sample")
            })?)
        } else {
            (i32::from_le_bytes(bytes.try_into().map_err(|_| {
                CaptureError::new(CaptureErrorCode::ChunkInvalid, "invalid 32-bit PCM sample")
            })?) >> 16) as i16
        };
        open.writer
            .as_mut()
            .expect("open chunk owns writer")
            .write_sample(sample)
            .map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::ChunkWriteFailed,
                    "meeting PCM sample could not be written",
                )
            })?;
    }
    Ok(())
}

fn validate_format(format: PcmFormat) -> Result<(), CaptureError> {
    let encoding_ok = (format.flags & FLAG_FLOAT != 0 && format.bits_per_channel == 32)
        || (format.flags & FLAG_SIGNED_INTEGER != 0 && matches!(format.bits_per_channel, 16 | 32));
    let bytes_per_sample = format.bits_per_channel / 8;
    let expected_bytes_per_frame = if format.flags & FLAG_NON_INTERLEAVED != 0 {
        bytes_per_sample
    } else {
        bytes_per_sample.saturating_mul(u32::from(format.channels))
    };
    if format.format_id != FORMAT_LINEAR_PCM
        || format.sample_rate == 0
        || format.channels == 0
        || format.flags & FLAG_BIG_ENDIAN != 0
        || format.flags & FLAG_PACKED == 0
        || format.bytes_per_frame != expected_bytes_per_frame
        || !encoding_ok
    {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting source delivered an unsupported PCM format",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct CompletedTrackChunk {
    track: CaptureTrack,
    ordinal: i64,
    start_ms: i64,
    end_ms: i64,
    partial_path: PathBuf,
    final_path: PathBuf,
    byte_length: i64,
}

#[derive(Default)]
struct PendingPair {
    microphone: Option<CompletedTrackChunk>,
    system: Option<CompletedTrackChunk>,
}

struct ManifestState {
    manifest: Manifest,
    directory: PathBuf,
    pending: BTreeMap<i64, PendingPair>,
    checkpoint: Arc<dyn ChunkCheckpoint>,
}

impl ManifestState {
    #[allow(dead_code)]
    fn set_paused_duration(&mut self, paused_duration_ms: i64) -> Result<(), CaptureError> {
        let mut next = self.manifest.clone();
        next.paused_duration_ms = next.paused_duration_ms.max(paused_duration_ms);
        atomic_write_manifest(&self.directory, &next).map_err(manifest_write_error)?;
        self.manifest = next;
        Ok(())
    }

    fn set_source_health(
        &mut self,
        source_health: Vec<CaptureSourceHealth>,
    ) -> Result<(), CaptureError> {
        let mut next = self.manifest.clone();
        next.health_required = true;
        next.source_health = source_health;
        atomic_write_manifest(&self.directory, &next).map_err(manifest_write_error)?;
        self.manifest = next;
        Ok(())
    }

    fn publish(&mut self, chunk: CompletedTrackChunk) -> Result<(), CaptureError> {
        let ordinal = chunk.ordinal;
        if ordinal < 0 || ordinal as usize >= MAX_CHUNK_PAIRS {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "meeting chunk ordinal exceeded the bounded first-release range",
            ));
        }
        if !self.pending.contains_key(&ordinal) && self.pending.len() >= MAX_PENDING_ORDINALS {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkInvalid,
                "meeting tracks exceeded the bounded chunk skew",
            ));
        }
        let pending = self.pending.entry(ordinal).or_default();
        match chunk.track {
            CaptureTrack::Microphone => pending.microphone = Some(chunk),
            CaptureTrack::System => pending.system = Some(chunk),
        }
        if pending.microphone.is_none() || pending.system.is_none() {
            return Ok(());
        }
        let pair = self.pending.remove(&ordinal).expect("ready pair exists");
        let microphone = pair.microphone.expect("microphone ready");
        let system = pair.system.expect("system ready");
        let start_ms = microphone.start_ms.min(system.start_ms);
        let end_ms = microphone.end_ms.max(system.end_ms);
        let microphone_name = relative_filename(&microphone.final_path, &self.directory)?;
        let system_name = relative_filename(&system.final_path, &self.directory)?;
        publish_pair_files(&self.directory, &microphone, &system)?;

        let manifest_chunk = ManifestChunk {
            ordinal,
            start_ms,
            end_ms,
            microphone: Some(microphone_name),
            system: Some(system_name),
        };
        let mut next = self.manifest.clone();
        next.active_duration_ms = next.active_duration_ms.max(end_ms);
        next.chunks.push(manifest_chunk);
        next.chunks.sort_by_key(|chunk| chunk.ordinal);
        // If this fails after the rename became visible, recovery discovery
        // exposes both finalized but unindexed WAVs. Do not delete or roll back
        // the only durable audio copy after an ambiguous fsync failure.
        atomic_write_manifest(&self.directory, &next).map_err(manifest_write_error)?;
        self.manifest = next.clone();

        let capture_id = next.capture_id;
        let mic_row = NewCaptureChunk {
            capture_id,
            ordinal,
            track: CaptureTrack::Microphone,
            start_ms: microphone.start_ms,
            end_ms: microphone.end_ms,
            path: microphone.final_path,
            byte_length: microphone.byte_length,
            status: CaptureChunkState::Ready,
        };
        let system_row = NewCaptureChunk {
            capture_id,
            ordinal,
            track: CaptureTrack::System,
            start_ms: system.start_ms,
            end_ms: system.end_ms,
            path: system.final_path,
            byte_length: system.byte_length,
            status: CaptureChunkState::Ready,
        };
        self.checkpoint
            .commit_pair(&mic_row, &system_row, next.active_duration_ms)?;
        Ok(())
    }
}

fn publish_pair_files(
    directory: &Path,
    microphone: &CompletedTrackChunk,
    system: &CompletedTrackChunk,
) -> Result<(), CaptureError> {
    for path in [&microphone.final_path, &system.final_path] {
        if fs::symlink_metadata(path).is_ok() {
            return Err(CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting WAV chunk destination already exists",
            ));
        }
    }
    fs::rename(&microphone.partial_path, &microphone.final_path).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "microphone WAV chunk could not be atomically published",
        )
    })?;
    if fs::rename(&system.partial_path, &system.final_path).is_err() {
        // Restore the first track's non-complete name when possible. If this
        // rollback itself fails, recovery still exposes the unindexed WAV.
        let _ = fs::rename(&microphone.final_path, &microphone.partial_path);
        let _ = sync_directory(directory);
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "system WAV chunk could not be atomically published",
        ));
    }
    sync_directory(directory)
}

fn sync_directory(directory: &Path) -> Result<(), CaptureError> {
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting chunk directory could not be synchronized",
            )
        })
}

fn relative_filename(path: &Path, directory: &Path) -> Result<PathBuf, CaptureError> {
    let relative = path.strip_prefix(directory).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting chunk escaped its staging directory",
        )
    })?;
    if relative.components().count() != 1 {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting chunk path is not a safe staging filename",
        ));
    }
    Ok(relative.to_path_buf())
}

fn create_secure_directory(path: &Path) -> Result<(), CaptureError> {
    let parent = path.parent().ok_or_else(|| {
        CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "meeting staging directory has no parent",
        )
    })?;
    fs::create_dir_all(parent).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "meeting staging parent could not be created",
        )
    })?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "meeting staging parent could not be inspected",
        )
    })?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "meeting staging parent is not a safe directory",
        ));
    }
    fs::create_dir(path).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::ChunkWriteFailed,
            "meeting staging directory must be newly created",
        )
    })?;
    let result = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| {
                CaptureError::new(
                    CaptureErrorCode::ChunkWriteFailed,
                    "meeting staging directory permissions could not be secured",
                )
            })?;
        }
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_dir(path);
    }
    result
}

fn secure_file(path: &Path) -> Result<(), CaptureError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::ChunkWriteFailed,
                "meeting audio file permissions could not be secured",
            )
        })?;
    }
    Ok(())
}

/// Scan finalized recovery WAVs after native capture has stopped. Signal
/// levels are deliberately derived here, never on the Core Audio IOProc.
fn enrich_signal_metrics(directory: &Path, health: &mut [CaptureSourceHealth]) {
    for source in health {
        let prefix = if source.track == "microphone" {
            "mic-"
        } else {
            "system-"
        };
        let mut peak = 0.0_f64;
        let mut sum_squares = 0.0_f64;
        let mut samples = 0_u64;
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for path in entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".wav"))
            })
        {
            let Ok(mut reader) = hound::WavReader::open(path) else {
                continue;
            };
            let bits = reader.spec().bits_per_sample;
            let full_scale = if bits >= 32 {
                i32::MAX as f64
            } else {
                ((1_u64 << bits.saturating_sub(1)) - 1).max(1) as f64
            };
            for sample in reader.samples::<i32>().flatten() {
                let normalized = f64::from(sample) / full_scale;
                peak = peak.max(normalized.abs());
                sum_squares += normalized * normalized;
                samples = samples.saturating_add(1);
            }
        }
        source.peak = peak;
        source.rms = if samples == 0 {
            0.0
        } else {
            (sum_squares / samples as f64).sqrt()
        };
    }
}

fn manifest_write_error(_error: std::io::Error) -> CaptureError {
    CaptureError::new(
        CaptureErrorCode::ChunkWriteFailed,
        "meeting recovery manifest could not be atomically published",
    )
}

fn set_terminal(slot: &Mutex<Option<CaptureError>>, error: CaptureError) {
    let mut slot = slot.lock();
    if slot.is_none() {
        *slot = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captures::recovery::{discover, RecoveryCandidateStatus};
    use crate::captures::CaptureState;
    use crate::history::{now_millis, TranscriptStore};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct RecordingCheckpoint {
        pairs: AtomicUsize,
        fail_after: Option<usize>,
    }

    impl RecordingCheckpoint {
        fn successful() -> Arc<Self> {
            Arc::new(Self {
                pairs: AtomicUsize::new(0),
                fail_after: None,
            })
        }
    }

    impl ChunkCheckpoint for RecordingCheckpoint {
        fn commit_pair(
            &self,
            _microphone: &NewCaptureChunk,
            _system: &NewCaptureChunk,
            _active_duration_ms: i64,
        ) -> Result<(), CaptureError> {
            let count = self.pairs.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_after.is_some_and(|limit| count > limit) {
                Err(CaptureError::new(
                    CaptureErrorCode::PersistenceUnavailable,
                    "injected checkpoint failure",
                ))
            } else {
                Ok(())
            }
        }
    }

    fn root(name: &str, id: i64) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "murmur-recorder-{name}-{}-{unique}",
                std::process::id()
            ))
            .join(id.to_string())
    }

    fn format() -> PcmFormat {
        PcmFormat {
            sample_rate: 1_000,
            channels: 1,
            format_id: FORMAT_LINEAR_PCM,
            flags: FLAG_SIGNED_INTEGER | FLAG_PACKED,
            bits_per_channel: 16,
            bytes_per_frame: 2,
        }
    }

    fn packet(frames: usize) -> AudioChunk {
        packet_at(frames, 0)
    }

    fn packet_at(frames: usize, pts_ms: i64) -> AudioChunk {
        AudioChunk {
            format: format(),
            frames,
            planes: vec![(0..frames)
                .flat_map(|index| ((index + 1) as i16).to_le_bytes())
                .collect()],
            plane_channels: vec![1],
            pts_ns: pts_ms * 1_000_000,
            origin_ns: 0,
        }
    }

    fn recorder(name: &str, id: i64, duration_ms: u64) -> (DiskBackedRecorder, PathBuf) {
        let directory = root(name, id);
        let recorder = DiskBackedRecorder::start(
            RecorderConfig {
                capture_id: id,
                created_at: now_millis(),
                directory: directory.clone(),
                queue_capacity: 4,
                chunk_duration_ms: duration_ms,
                fail_open: None,
            },
            RecordingCheckpoint::successful(),
        )
        .unwrap();
        (recorder, directory)
    }

    #[test]
    fn startup_refuses_existing_staging_without_touching_manifest() {
        let directory = root("existing", 41);
        fs::create_dir_all(&directory).unwrap();
        let manifest_path = directory.join("manifest.json");
        fs::write(&manifest_path, b"existing recovery truth").unwrap();
        let result = DiskBackedRecorder::start(
            RecorderConfig {
                capture_id: 41,
                created_at: now_millis(),
                directory: directory.clone(),
                queue_capacity: 4,
                chunk_duration_ms: 10,
                fail_open: None,
            },
            RecordingCheckpoint::successful(),
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(&manifest_path).unwrap(),
            b"existing recovery truth"
        );
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn deterministic_dual_track_rollover_publishes_valid_wavs_and_manifest() {
        let (recorder, directory) = recorder("rollover", 42, 10);
        recorder.microphone_input().try_send(packet(25)).unwrap();
        recorder.system_input().try_send(packet(25)).unwrap();
        let result = recorder.stop().unwrap();
        assert_eq!(result.capture_id, 42);
        assert_eq!(result.completed_chunk_pairs, 3);
        for ordinal in 0..3 {
            for prefix in ["mic", "system"] {
                let path = directory.join(format!("{prefix}-{ordinal:05}.wav"));
                let reader = hound::WavReader::open(&path).unwrap();
                assert_eq!(reader.spec().sample_rate, 1_000);
                assert_eq!(reader.spec().channels, 1);
                assert!(!directory
                    .join(format!("{prefix}-{ordinal:05}.wav.partial"))
                    .exists());
            }
        }
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.capture_id, 42);
        assert_eq!(manifest.chunks.len(), 3);
        assert_eq!(
            manifest.chunks[0].microphone.as_deref(),
            Some(Path::new("mic-00000.wav"))
        );
        assert_eq!(
            discover(directory.parent().unwrap()).unwrap()[0].status,
            RecoveryCandidateStatus::Recoverable
        );
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    /// Phase A2's core outcome: the system-audio source is opportunistic, so a
    /// meeting where nothing ever played is a valid capture with a silent
    /// system track — not `chunk_invalid`. The track has no audio to publish of
    /// its own, so the controller's padding is what produces it.
    #[test]
    fn a_source_that_never_delivers_is_padded_into_a_valid_silent_track() {
        let (recorder, directory) = recorder("silent-source", 90, 10);
        recorder.system_input().declare_format(format()).unwrap();
        // 25 ms of microphone audio, and not one packet of system audio.
        recorder.microphone_input().try_send(packet(25)).unwrap();
        recorder.pad_to_final(25_000_000).unwrap();
        let result = recorder.stop().unwrap();

        assert_eq!(result.completed_chunk_pairs, 3);
        for ordinal in 0..3 {
            let system = hound::WavReader::open(directory.join(format!("system-{ordinal:05}.wav")))
                .unwrap()
                .into_samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(!system.is_empty());
            assert!(
                system.iter().all(|sample| *sample == 0),
                "padded system chunk {ordinal} should be silence"
            );
        }
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.active_duration_ms, 25);
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    /// The Bug 9 order of operations: start the capture, then join the call.
    /// The tap's first packet arrives well after the microphone's, and it has
    /// to land where it happened rather than at zero.
    #[test]
    fn a_late_source_is_padded_up_to_where_it_actually_started() {
        let (recorder, directory) = recorder("late-source", 91, 100);
        recorder.system_input().declare_format(format()).unwrap();
        recorder.microphone_input().try_send(packet(60)).unwrap();
        // The controller pads while the tap is still silent...
        recorder.pad_to(40_000_000);
        // ...and then the tap finally delivers, 40 ms in.
        recorder.system_input().try_send(packet_at(20, 40)).unwrap();
        recorder.pad_to_final(60_000_000).unwrap();
        recorder.stop().unwrap();

        let system = hound::WavReader::open(directory.join("system-00000.wav"))
            .unwrap()
            .into_samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        // 40 ms of silence at 1 kHz mono, then the 20 real frames, then the
        // final pad back up to the microphone's 60 ms.
        assert_eq!(system.len(), 60);
        assert!(system[..40].iter().all(|sample| *sample == 0));
        assert_eq!(&system[40..60], &(1..=20).collect::<Vec<i16>>()[..]);
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    /// Padding is an alignment tool, never a rewrite: a track that is already
    /// past the requested point must be left exactly as it is.
    #[test]
    fn padding_a_track_that_is_already_ahead_writes_nothing() {
        let (recorder, directory) = recorder("pad-noop", 92, 100);
        recorder.microphone_input().try_send(packet(30)).unwrap();
        recorder.system_input().try_send(packet(30)).unwrap();
        recorder.pad_to(10_000_000);
        recorder.pad_to(-5_000_000);
        recorder.stop().unwrap();

        for prefix in ["mic", "system"] {
            let samples = hound::WavReader::open(directory.join(format!("{prefix}-00000.wav")))
                .unwrap()
                .into_samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(samples, (1..=30).collect::<Vec<i16>>());
        }
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn pause_finalizes_short_pair_and_resume_uses_next_ordinal() {
        let (recorder, directory) = recorder("pause-resume", 79, 100);
        recorder.microphone_input().try_send(packet(30)).unwrap();
        recorder.system_input().try_send(packet(30)).unwrap();
        recorder.pause().unwrap();
        recorder
            .microphone_input()
            .try_send(packet_at(10, 30))
            .unwrap();
        recorder.system_input().try_send(packet_at(10, 30)).unwrap();
        recorder.stop().unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.chunks.len(), 2);
        assert_eq!(manifest.chunks[0].ordinal, 0);
        assert_eq!(manifest.chunks[1].ordinal, 1);
        assert_eq!(manifest.chunks[0].end_ms, 30);
        assert_eq!(manifest.chunks[1].start_ms, 30);
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn repeated_pause_boundaries_do_not_reuse_ordinals() {
        let (recorder, directory) = recorder("many-pauses", 80, 100_000);
        for index in 0..50 {
            let pts = index;
            recorder
                .microphone_input()
                .try_send(packet_at(1, pts))
                .unwrap();
            recorder.system_input().try_send(packet_at(1, pts)).unwrap();
            recorder.pause().unwrap();
        }
        recorder.stop().unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        let ordinals: Vec<i64> = manifest.chunks.iter().map(|chunk| chunk.ordinal).collect();
        assert_eq!(ordinals, (0..50).collect::<Vec<_>>());
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn interrupt_keeps_open_partial_and_last_atomic_manifest() {
        let (recorder, directory) = recorder("interrupt", 77, 100);
        recorder.microphone_input().try_send(packet(10)).unwrap();
        recorder.system_input().try_send(packet(10)).unwrap();
        // Give the bounded writers time to create their open files before the
        // ordered interrupt command reaches them.
        thread::sleep(std::time::Duration::from_millis(20));
        recorder.interrupt().unwrap();
        assert!(directory.join("mic-00000.wav.partial").exists());
        assert!(directory.join("system-00000.wav.partial").exists());
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert!(manifest.chunks.is_empty());
        let candidate = &discover(directory.parent().unwrap()).unwrap()[0];
        assert_eq!(candidate.status, RecoveryCandidateStatus::Recoverable);
        assert_eq!(
            candidate.open_partial_files,
            vec![
                PathBuf::from("mic-00000.wav.partial"),
                PathBuf::from("system-00000.wav.partial")
            ]
        );
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn empty_tracks_return_typed_invalid_chunk_and_keep_manifest() {
        let (recorder, directory) = recorder("empty", 78, 100);
        let error = recorder.stop().unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::ChunkInvalid);
        assert!(directory.join("manifest.json").is_file());
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn bounded_sender_overflow_is_explicit_and_memory_bound_is_constant() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let input = TrackInput { tx };
        input.try_send(packet(1)).unwrap();
        let error = input.try_send(packet(1)).unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::WriterQueueOverflow);
        assert_eq!(MAX_QUEUED_AUDIO_BYTES_PER_TRACK, 32 * 1024 * 1024);
    }

    #[test]
    fn checkpoint_failure_keeps_published_audio_and_recovery_manifest() {
        let directory = root("checkpoint-failure", 88);
        let checkpoint = Arc::new(RecordingCheckpoint {
            pairs: AtomicUsize::new(0),
            fail_after: Some(1),
        });
        let recorder = DiskBackedRecorder::start(
            RecorderConfig {
                capture_id: 88,
                created_at: now_millis(),
                directory: directory.clone(),
                queue_capacity: 4,
                chunk_duration_ms: 10,
                fail_open: None,
            },
            checkpoint,
        )
        .unwrap();
        recorder.microphone_input().try_send(packet(20)).unwrap();
        recorder.system_input().try_send(packet(20)).unwrap();
        let error = recorder.stop().unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::PersistenceUnavailable);
        assert!(directory.join("mic-00000.wav").exists());
        assert!(directory.join("system-00000.wav").exists());
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert!(!manifest.chunks.is_empty());
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn second_track_publish_failure_rolls_first_track_back_to_partial() {
        let directory = root("pair-rename-failure", 92);
        fs::create_dir_all(&directory).unwrap();
        let microphone = CompletedTrackChunk {
            track: CaptureTrack::Microphone,
            ordinal: 0,
            start_ms: 0,
            end_ms: 10,
            partial_path: directory.join("mic-00000.wav.partial"),
            final_path: directory.join("mic-00000.wav"),
            byte_length: 44,
        };
        let system = CompletedTrackChunk {
            track: CaptureTrack::System,
            ordinal: 0,
            start_ms: 0,
            end_ms: 10,
            partial_path: directory.join("system-00000.wav.partial"),
            final_path: directory.join("system-00000.wav"),
            byte_length: 44,
        };
        fs::write(&microphone.partial_path, b"valid finalized placeholder").unwrap();
        let error = publish_pair_files(&directory, &microphone, &system).unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::ChunkWriteFailed);
        assert!(microphone.partial_path.exists());
        assert!(!microphone.final_path.exists());
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn writer_failure_retains_complete_pair_and_recoverable_manifest() {
        let directory = root("writer-failure", 89);
        let recorder = DiskBackedRecorder::start(
            RecorderConfig {
                capture_id: 89,
                created_at: now_millis(),
                directory: directory.clone(),
                queue_capacity: 4,
                chunk_duration_ms: 10,
                fail_open: Some((CaptureTrack::Microphone, 1)),
            },
            RecordingCheckpoint::successful(),
        )
        .unwrap();
        recorder.microphone_input().try_send(packet(20)).unwrap();
        recorder.system_input().try_send(packet(20)).unwrap();
        let error = recorder.stop().unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::ChunkWriteFailed);
        assert!(directory.join("mic-00000.wav").is_file());
        assert!(directory.join("system-00000.wav").is_file());
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.chunks.len(), 1);
        let candidate = &discover(directory.parent().unwrap()).unwrap()[0];
        assert_eq!(candidate.status, RecoveryCandidateStatus::Recoverable);
        assert!(!directory.join("mic-00001.wav").exists());
        assert!(!directory.join("system-00001.wav").exists());
        assert!(candidate.unindexed_wav_files.is_empty());
        assert!(candidate
            .open_partial_files
            .contains(&PathBuf::from("system-00001.wav.partial")));
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn presentation_timestamps_align_initial_offset_and_midstream_gap() {
        let (recorder, directory) = recorder("pts-alignment", 90, 100);
        for input in [recorder.microphone_input(), recorder.system_input()] {
            input.try_send(packet_at(5, 10)).unwrap();
            input.try_send(packet_at(5, 20)).unwrap();
        }
        recorder.stop().unwrap();
        let mut reader = hound::WavReader::open(directory.join("mic-00000.wav")).unwrap();
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(&samples[..10], &[0; 10]);
        assert_eq!(&samples[10..15], &[1, 2, 3, 4, 5]);
        assert_eq!(&samples[15..20], &[0; 5]);
        assert_eq!(&samples[20..25], &[1, 2, 3, 4, 5]);
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn one_frame_timestamp_rounding_does_not_accumulate_drift() {
        let (recorder, directory) = recorder("one-frame-overlap", 93, 100);
        for input in [recorder.microphone_input(), recorder.system_input()] {
            input.try_send(packet_at(5, 0)).unwrap();
            input.try_send(packet_at(5, 4)).unwrap();
        }
        recorder.stop().unwrap();
        let reader = hound::WavReader::open(directory.join("mic-00000.wav")).unwrap();
        assert_eq!(reader.duration(), 9);
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn backwards_and_pathological_forward_timestamps_are_typed_invalid_chunks() {
        let (recorder, directory) = recorder("bad-pts", 91, 120_000);
        recorder
            .microphone_input()
            .try_send(packet_at(5, 0))
            .unwrap();
        recorder
            .microphone_input()
            .try_send(packet_at(5, 1))
            .unwrap();
        recorder.system_input().try_send(packet_at(5, 0)).unwrap();
        recorder
            .system_input()
            .try_send(packet_at(5, 61_000))
            .unwrap();
        let error = recorder.stop().unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::ChunkInvalid);
        fs::remove_dir_all(directory.parent().unwrap()).unwrap();
    }

    #[test]
    fn filesystem_publication_precedes_short_sqlite_checkpoint_without_guard_overlap() {
        struct StoreBoundaryCheckpoint {
            store: Arc<TranscriptStore>,
            directory: PathBuf,
        }
        impl ChunkCheckpoint for StoreBoundaryCheckpoint {
            fn commit_pair(
                &self,
                microphone: &NewCaptureChunk,
                system: &NewCaptureChunk,
                active_duration_ms: i64,
            ) -> Result<(), CaptureError> {
                assert!(self.store.connection_guard_available());
                assert!(self.directory.join("manifest.json").is_file());
                assert!(microphone.path.is_file());
                assert!(system.path.is_file());
                self.store
                    .commit_ready_chunk_pair(microphone, system, active_duration_ms)
            }
        }

        let requested_directory = root("sqlite-boundary", 1);
        let staging_root = requested_directory.parent().unwrap().to_path_buf();
        fs::create_dir_all(&staging_root).unwrap();
        let staging_root = fs::canonicalize(staging_root).unwrap();
        let db_path = staging_root.join("murmur.db");
        let store = Arc::new(TranscriptStore::open(&db_path).unwrap());
        let capture = store.create_capture("Boundary", &staging_root).unwrap();
        assert_eq!(capture.id, 1);
        for state in [
            CaptureState::SelectingSource,
            CaptureState::Starting,
            CaptureState::Recording,
        ] {
            store
                .transition_capture(capture.id, state, None, None)
                .unwrap();
        }
        let recorder = DiskBackedRecorder::start(
            RecorderConfig {
                capture_id: capture.id,
                created_at: capture.created_at,
                directory: capture.staging_path.clone().unwrap(),
                queue_capacity: 4,
                chunk_duration_ms: 10,
                fail_open: None,
            },
            Arc::new(StoreBoundaryCheckpoint {
                store: Arc::clone(&store),
                directory: capture.staging_path.unwrap(),
            }),
        )
        .unwrap();
        recorder.microphone_input().try_send(packet(10)).unwrap();
        recorder.system_input().try_send(packet(10)).unwrap();
        recorder.stop().unwrap();
        let conn = store.connection().unwrap();
        let chunk_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM capture_chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(chunk_count, 2);
        drop(conn);
        drop(store);
        fs::remove_dir_all(staging_root).unwrap();
    }

    #[test]
    fn fixed_queue_and_packet_caps_make_memory_duration_independent() {
        assert_eq!(
            MAX_QUEUED_AUDIO_BYTES_PER_TRACK,
            DEFAULT_QUEUE_CAPACITY * MAX_PACKET_BYTES
        );
        assert_eq!(DEFAULT_QUEUE_CAPACITY, 32);
        assert_eq!(MAX_PACKET_BYTES, 1024 * 1024);
        assert_eq!(MAX_PACKET_PLANES, 64);
        assert_eq!(MAX_PENDING_ORDINALS, 4);
        assert_eq!(MAX_CHUNK_PAIRS, 1_024);
        // Capture duration is absent from every allocation bound: extending a
        // meeting changes only sequential file bytes and chunk ordinals.
        assert!(std::mem::size_of::<TrackInput>() < 256);
    }
}
