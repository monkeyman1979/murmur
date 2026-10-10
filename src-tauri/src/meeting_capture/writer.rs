#[cfg(any(test, feature = "meeting-capture-debug"))]
use std::fs::File;
#[cfg(any(test, feature = "meeting-capture-debug"))]
use std::io::BufWriter;
#[cfg(any(test, feature = "meeting-capture-debug"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use parking_lot::Mutex;

use super::{Mt0Error, Mt0ErrorCode};

pub(crate) const FORMAT_LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
pub(crate) const FLAG_FLOAT: u32 = 1 << 0;
pub(crate) const FLAG_BIG_ENDIAN: u32 = 1 << 1;
pub(crate) const FLAG_SIGNED_INTEGER: u32 = 1 << 2;
pub(crate) const FLAG_PACKED: u32 = 1 << 3;
pub(crate) const FLAG_NON_INTERLEAVED: u32 = 1 << 5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub format_id: u32,
    pub flags: u32,
    pub bits_per_channel: u32,
    pub bytes_per_frame: u32,
}

#[derive(Debug)]
pub(crate) struct AudioChunk {
    pub format: PcmFormat,
    pub frames: usize,
    pub planes: Vec<Vec<u8>>,
    pub plane_channels: Vec<u32>,
    pub pts_ns: i64,
    pub origin_ns: i64,
}

#[derive(Debug)]
pub(crate) struct TrackStats {
    pub frames: AtomicU64,
    pub input_bytes: AtomicU64,
    pub queued: AtomicUsize,
    pub high_water: AtomicUsize,
    pub first_pts_ns: AtomicI64,
    pub last_pts_ns: AtomicI64,
    pub inserted_silence_frames: AtomicU64,
}

impl Default for TrackStats {
    fn default() -> Self {
        Self {
            frames: AtomicU64::new(0),
            input_bytes: AtomicU64::new(0),
            queued: AtomicUsize::new(0),
            high_water: AtomicUsize::new(0),
            first_pts_ns: AtomicI64::new(i64::MIN),
            last_pts_ns: AtomicI64::new(i64::MIN),
            inserted_silence_frames: AtomicU64::new(0),
        }
    }
}

impl TrackStats {
    pub(crate) fn note_enqueued(&self) -> usize {
        self.queued.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn note_enqueued_success(&self, queued: usize) {
        self.high_water.fetch_max(queued, Ordering::Relaxed);
    }

    pub(crate) fn note_dequeued(&self) {
        self.queued.fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn note_pts(&self, pts_ns: i64) {
        let _ = self.first_pts_ns.compare_exchange(
            i64::MIN,
            pts_ns,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        self.last_pts_ns.store(pts_ns, Ordering::Relaxed);
    }
}

pub(crate) struct WriterThread {
    pub tx: Option<mpsc::SyncSender<AudioChunk>>,
    #[cfg_attr(not(any(test, feature = "meeting-capture-debug")), allow(dead_code))]
    pub stats: Arc<TrackStats>,
    pub error: Arc<Mutex<Option<Mt0Error>>>,
    join: Option<thread::JoinHandle<()>>,
}

impl WriterThread {
    #[cfg(any(test, feature = "meeting-capture-debug"))]
    pub(crate) fn spawn(path: PathBuf, capacity: usize) -> Result<Self, Mt0Error> {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let stats = Arc::new(TrackStats::default());
        let error = Arc::new(Mutex::new(None));
        let stats_thread = Arc::clone(&stats);
        let error_thread = Arc::clone(&error);
        let join = thread::Builder::new()
            .name("murmur-mt0-wav".into())
            .spawn(move || {
                if let Err(e) = writer_loop(path, rx, &stats_thread) {
                    *error_thread.lock() = Some(e);
                }
            })
            .map_err(|e| {
                Mt0Error::new(
                    Mt0ErrorCode::WavCreateFailed,
                    format!("spawn WAV writer: {e}"),
                )
            })?;
        Ok(Self {
            tx: Some(tx),
            stats,
            error,
            join: Some(join),
        })
    }

    #[cfg(any(test, feature = "meeting-capture-debug"))]
    pub(crate) fn sender(&self) -> &mpsc::SyncSender<AudioChunk> {
        self.tx
            .as_ref()
            .expect("writer sender exists until teardown")
    }

    pub(crate) fn finish(mut self) -> Result<(), Mt0Error> {
        self.tx.take();
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| {
                Mt0Error::new(Mt0ErrorCode::TeardownFailed, "WAV writer thread panicked")
            })?;
        }
        match self.error.lock().clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for WriterThread {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(any(test, feature = "meeting-capture-debug"))]
fn validate(format: PcmFormat) -> Result<(), Mt0Error> {
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
        return Err(Mt0Error::new(
            Mt0ErrorCode::UnsupportedAudioFormat,
            format!(
                "unsupported PCM format id={:#x} flags={:#x} bits={} bytes/frame={} rate={} channels={}",
                format.format_id,
                format.flags,
                format.bits_per_channel,
                format.bytes_per_frame,
                format.sample_rate,
                format.channels
            ),
        ));
    }
    Ok(())
}

#[cfg(any(test, feature = "meeting-capture-debug"))]
fn writer_loop(
    path: PathBuf,
    rx: mpsc::Receiver<AudioChunk>,
    stats: &TrackStats,
) -> Result<(), Mt0Error> {
    let mut writer: Option<hound::WavWriter<BufWriter<File>>> = None;
    let mut first_format: Option<PcmFormat> = None;
    let mut output_frames = 0_u64;
    while let Ok(chunk) = rx.recv() {
        stats.note_dequeued();
        validate(chunk.format)?;
        if let Some(initial) = first_format {
            if initial != chunk.format {
                return Err(Mt0Error::new(
                    Mt0ErrorCode::AudioFormatChanged,
                    "audio format changed during MT0 capture",
                ));
            }
        } else {
            let spec = hound::WavSpec {
                channels: chunk.format.channels,
                sample_rate: chunk.format.sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            writer = Some(hound::WavWriter::create(&path, spec).map_err(|e| {
                Mt0Error::new(
                    Mt0ErrorCode::WavCreateFailed,
                    format!("create {}: {e}", path.display()),
                )
            })?);
            #[cfg(unix)]
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
                .map_err(|e| {
                    Mt0Error::new(
                        Mt0ErrorCode::WavCreateFailed,
                        format!("secure {}: {e}", path.display()),
                    )
                })?;
            first_format = Some(chunk.format);
        }
        let input_bytes = chunk.planes.iter().map(Vec::len).sum::<usize>();
        stats.note_pts(chunk.pts_ns);
        let target_frame = pts_to_frame(chunk.pts_ns, chunk.origin_ns, chunk.format.sample_rate);
        if target_frame > output_frames {
            let silence_frames = target_frame - output_frames;
            write_silence(
                writer.as_mut().expect("writer initialized above"),
                silence_frames,
                chunk.format.channels,
            )?;
            stats
                .inserted_silence_frames
                .fetch_add(silence_frames, Ordering::Relaxed);
            output_frames = target_frame;
        }
        write_chunk(writer.as_mut().expect("writer initialized above"), &chunk)?;
        output_frames = output_frames.saturating_add(chunk.frames as u64);
        stats
            .frames
            .fetch_add(chunk.frames as u64, Ordering::Relaxed);
        stats
            .input_bytes
            .fetch_add(input_bytes as u64, Ordering::Relaxed);
    }
    if let Some(writer) = writer {
        writer.finalize().map_err(|e| {
            Mt0Error::new(
                Mt0ErrorCode::WavWriteFailed,
                format!("finalize {}: {e}", path.display()),
            )
        })?;
    }
    Ok(())
}

#[cfg(any(test, feature = "meeting-capture-debug"))]
fn pts_to_frame(pts_ns: i64, origin_ns: i64, sample_rate: u32) -> u64 {
    let delta_ns = pts_ns.saturating_sub(origin_ns).max(0) as u128;
    ((delta_ns.saturating_mul(u128::from(sample_rate))) / 1_000_000_000).min(u128::from(u64::MAX))
        as u64
}

#[cfg(any(test, feature = "meeting-capture-debug"))]
fn write_silence(
    writer: &mut hound::WavWriter<BufWriter<File>>,
    frames: u64,
    channels: u16,
) -> Result<(), Mt0Error> {
    for _ in 0..frames.saturating_mul(u64::from(channels)) {
        writer.write_sample(0_i16).map_err(|e| {
            Mt0Error::new(
                Mt0ErrorCode::WavWriteFailed,
                format!("write WAV alignment silence: {e}"),
            )
        })?;
    }
    Ok(())
}

#[cfg(any(test, feature = "meeting-capture-debug"))]
fn write_chunk(
    writer: &mut hound::WavWriter<BufWriter<File>>,
    chunk: &AudioChunk,
) -> Result<(), Mt0Error> {
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
        return Err(Mt0Error::new(
            Mt0ErrorCode::UnsupportedAudioFormat,
            "PCM plane count does not match channel layout",
        ));
    }
    for frame in 0..chunk.frames {
        for channel in 0..format.channels as usize {
            let (plane, offset) = if planar {
                (&chunk.planes[channel], frame * bytes_per_sample)
            } else {
                (
                    &chunk.planes[0],
                    (frame * format.channels as usize + channel) * bytes_per_sample,
                )
            };
            let end = offset.saturating_add(bytes_per_sample);
            let bytes = plane.get(offset..end).ok_or_else(|| {
                Mt0Error::new(
                    Mt0ErrorCode::WavWriteFailed,
                    "PCM buffer shorter than declared frame count",
                )
            })?;
            let sample = if format.flags & FLAG_FLOAT != 0 {
                let value = f32::from_le_bytes(bytes.try_into().unwrap()).clamp(-1.0, 1.0);
                (value * i16::MAX as f32) as i16
            } else if format.bits_per_channel == 16 {
                i16::from_le_bytes(bytes.try_into().unwrap())
            } else {
                (i32::from_le_bytes(bytes.try_into().unwrap()) >> 16) as i16
            };
            writer.write_sample(sample).map_err(|e| {
                Mt0Error::new(
                    Mt0ErrorCode::WavWriteFailed,
                    format!("write WAV sample: {e}"),
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "murmur-mt0-{name}-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn writes_valid_interleaved_float_wav() {
        let path = temp_path("float");
        let writer = WriterThread::spawn(path.clone(), 2).unwrap();
        let format = PcmFormat {
            sample_rate: 48_000,
            channels: 2,
            format_id: FORMAT_LINEAR_PCM,
            flags: FLAG_FLOAT | FLAG_PACKED,
            bits_per_channel: 32,
            bytes_per_frame: 8,
        };
        let samples = [0.0_f32, 0.5, -0.5, 1.0];
        let bytes = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let queued = writer.stats.note_enqueued();
        writer
            .sender()
            .send(AudioChunk {
                format,
                frames: 2,
                planes: vec![bytes],
                plane_channels: vec![2],
                pts_ns: 1_000_000_000,
                origin_ns: 1_000_000_000,
            })
            .unwrap();
        writer.stats.note_enqueued_success(queued);
        writer.finish().unwrap();
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(reader.spec().sample_rate, 48_000);
        assert_eq!(reader.duration(), 2);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn writes_valid_planar_float_wav() {
        let path = temp_path("planar");
        let writer = WriterThread::spawn(path.clone(), 2).unwrap();
        let format = PcmFormat {
            sample_rate: 48_000,
            channels: 2,
            format_id: FORMAT_LINEAR_PCM,
            flags: FLAG_FLOAT | FLAG_PACKED | FLAG_NON_INTERLEAVED,
            bits_per_channel: 32,
            bytes_per_frame: 4,
        };
        let queued = writer.stats.note_enqueued();
        writer
            .sender()
            .send(AudioChunk {
                format,
                frames: 2,
                planes: vec![
                    [0.25_f32, 0.5]
                        .iter()
                        .flat_map(|sample| sample.to_le_bytes())
                        .collect(),
                    [-0.25_f32, -0.5]
                        .iter()
                        .flat_map(|sample| sample.to_le_bytes())
                        .collect(),
                ],
                plane_channels: vec![1, 1],
                pts_ns: 0,
                origin_ns: 0,
            })
            .unwrap();
        writer.stats.note_enqueued_success(queued);
        writer.finish().unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(samples.len(), 4);
        assert!(samples[0] > 0 && samples[1] < 0);
        assert!(samples[2] > 0 && samples[3] < 0);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn bounded_sender_reports_full_without_blocking() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let format = PcmFormat {
            sample_rate: 16_000,
            channels: 1,
            format_id: FORMAT_LINEAR_PCM,
            flags: FLAG_SIGNED_INTEGER | FLAG_PACKED,
            bits_per_channel: 16,
            bytes_per_frame: 2,
        };
        let chunk = || AudioChunk {
            format,
            frames: 1,
            planes: vec![vec![0, 0]],
            plane_channels: vec![1],
            pts_ns: 0,
            origin_ns: 0,
        };
        tx.try_send(chunk()).unwrap();
        let result = tx.try_send(chunk());
        assert!(matches!(result, Err(mpsc::TrySendError::Full(_))));
    }

    #[test]
    fn rejects_format_change() {
        let path = temp_path("format-change");
        let writer = WriterThread::spawn(path.clone(), 2).unwrap();
        let f1 = PcmFormat {
            sample_rate: 16_000,
            channels: 1,
            format_id: FORMAT_LINEAR_PCM,
            flags: FLAG_SIGNED_INTEGER | FLAG_PACKED,
            bits_per_channel: 16,
            bytes_per_frame: 2,
        };
        let f2 = PcmFormat {
            sample_rate: 48_000,
            ..f1
        };
        for format in [f1, f2] {
            let queued = writer.stats.note_enqueued();
            writer
                .sender()
                .send(AudioChunk {
                    format,
                    frames: 1,
                    planes: vec![vec![0, 0]],
                    plane_channels: vec![1],
                    pts_ns: 0,
                    origin_ns: 0,
                })
                .unwrap();
            writer.stats.note_enqueued_success(queued);
        }
        let error = writer.finish().unwrap_err();
        assert_eq!(error.code, Mt0ErrorCode::AudioFormatChanged);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn presentation_timestamp_inserts_alignment_silence() {
        let path = temp_path("aligned");
        let writer = WriterThread::spawn(path.clone(), 2).unwrap();
        let format = PcmFormat {
            sample_rate: 1_000,
            channels: 1,
            format_id: FORMAT_LINEAR_PCM,
            flags: FLAG_SIGNED_INTEGER | FLAG_PACKED,
            bits_per_channel: 16,
            bytes_per_frame: 2,
        };
        let queued = writer.stats.note_enqueued();
        writer
            .sender()
            .send(AudioChunk {
                format,
                frames: 2,
                planes: vec![vec![1, 0, 2, 0]],
                plane_channels: vec![1],
                pts_ns: 1_010_000_000,
                origin_ns: 1_000_000_000,
            })
            .unwrap();
        writer.stats.note_enqueued_success(queued);
        let stats = Arc::clone(&writer.stats);
        writer.finish().unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 12);
        let samples: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(&samples[..10], &[0; 10]);
        assert_eq!(&samples[10..], &[1, 2]);
        assert_eq!(stats.inserted_silence_frames.load(Ordering::Relaxed), 10);
        std::fs::remove_file(path).unwrap();
    }
}
