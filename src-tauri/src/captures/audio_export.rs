use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{renameat_with, RenameFlags, CWD};

use super::{CaptureError, CaptureErrorCode, CaptureState, CaptureTrack, TranscriptStore};

struct TempFiles {
    wav: PathBuf,
    encoded: PathBuf,
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.wav);
        let _ = fs::remove_file(&self.encoded);
    }
}

fn export_error(message: &'static str) -> CaptureError {
    CaptureError::new(CaptureErrorCode::AudioExportFailed, message)
}

fn validate_destination(destination: &Path) -> Result<&Path, CaptureError> {
    if !destination.is_absolute()
        || destination.extension().and_then(|value| value.to_str()) != Some("m4a")
    {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidRequest,
            "audio export destination must be an absolute .m4a path",
        ));
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(CaptureError::new(
            CaptureErrorCode::OutputFileExists,
            "audio export destination already exists",
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| export_error("audio export destination is invalid"))?;
    let metadata = fs::symlink_metadata(parent)
        .map_err(|_| export_error("audio export destination is unavailable"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(export_error(
            "audio export destination is not a trusted directory",
        ));
    }
    Ok(parent)
}

fn read_mono(path: &Path) -> Result<(u32, Vec<i16>), CaptureError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::AudioUnavailable,
            "retained audio file is unavailable",
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CaptureError::new(
            CaptureErrorCode::AudioUnavailable,
            "retained audio path is not a trusted file",
        ));
    }
    let mut reader = hound::WavReader::open(path).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::AudioUnavailable,
            "retained audio file is invalid",
        )
    })?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int
        || spec.bits_per_sample != 16
        || spec.channels == 0
        || spec.sample_rate == 0
    {
        return Err(CaptureError::new(
            CaptureErrorCode::AudioUnavailable,
            "retained audio format cannot be exported safely",
        ));
    }
    let channels = usize::from(spec.channels);
    let samples = reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "retained audio file is invalid",
            )
        })?;
    let mut mono = Vec::with_capacity(samples.len() / channels);
    for frame in samples.chunks_exact(channels) {
        let sum: i64 = frame.iter().map(|sample| i64::from(*sample)).sum();
        mono.push((sum / i64::try_from(channels).unwrap_or(1)) as i16);
    }
    Ok((spec.sample_rate, mono))
}

fn write_mixed_wav(
    chunks: &BTreeMap<i64, (Option<PathBuf>, Option<PathBuf>)>,
    destination: &Path,
) -> Result<(), CaptureError> {
    let mut writer: Option<hound::WavWriter<std::io::BufWriter<fs::File>>> = None;
    let mut output_rate = None;
    for (microphone, system) in chunks.values() {
        let microphone = microphone.as_deref().map(read_mono).transpose()?;
        let system = system.as_deref().map(read_mono).transpose()?;
        let rate = microphone
            .as_ref()
            .map(|(rate, _)| *rate)
            .or_else(|| system.as_ref().map(|(rate, _)| *rate))
            .ok_or_else(|| {
                CaptureError::new(
                    CaptureErrorCode::AudioUnavailable,
                    "capture has no exportable audio",
                )
            })?;
        if microphone.as_ref().is_some_and(|(value, _)| *value != rate)
            || system.as_ref().is_some_and(|(value, _)| *value != rate)
            || output_rate.is_some_and(|value| value != rate)
        {
            return Err(CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "retained tracks use incompatible sample rates",
            ));
        }
        output_rate = Some(rate);
        if writer.is_none() {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(|_| export_error("temporary mixed audio could not be created"))?;
            #[cfg(unix)]
            file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))
                .map_err(|_| export_error("temporary mixed audio could not be secured"))?;
            writer = Some(
                hound::WavWriter::new(
                    BufWriter::new(file),
                    hound::WavSpec {
                        channels: 1,
                        sample_rate: rate,
                        bits_per_sample: 16,
                        sample_format: hound::SampleFormat::Int,
                    },
                )
                .map_err(|_| export_error("temporary mixed audio could not be created"))?,
            );
        }
        let mic = microphone
            .as_ref()
            .map(|(_, samples)| samples.as_slice())
            .unwrap_or(&[]);
        let sys = system
            .as_ref()
            .map(|(_, samples)| samples.as_slice())
            .unwrap_or(&[]);
        let frames = mic.len().max(sys.len());
        for index in 0..frames {
            let mic_sample = mic.get(index).copied();
            let sys_sample = sys.get(index).copied();
            let sample = match (mic_sample, sys_sample) {
                (Some(a), Some(b)) => ((i32::from(a) + i32::from(b)) / 2) as i16,
                (Some(value), None) | (None, Some(value)) => value,
                (None, None) => 0,
            };
            writer
                .as_mut()
                .expect("writer initialized")
                .write_sample(sample)
                .map_err(|_| export_error("temporary mixed audio could not be written"))?;
        }
    }
    writer
        .ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "capture has no exportable audio",
            )
        })?
        .finalize()
        .map_err(|_| export_error("temporary mixed audio could not be finalized"))
}

#[cfg(target_os = "macos")]
fn encode_m4a(input: &Path, output: &Path) -> Result<(), CaptureError> {
    let result = Command::new("/usr/bin/afconvert")
        .args(["-f", "m4af", "-d", "aac ", "-q", "127", "-b", "96000"])
        .arg(input)
        .arg(output)
        .output()
        .map_err(|_| export_error("macOS audio encoder could not be started"))?;
    if !result.status.success() {
        return Err(export_error("macOS could not encode the audio copy"));
    }
    let metadata =
        fs::metadata(output).map_err(|_| export_error("encoded audio copy is missing"))?;
    if metadata.len() == 0 {
        return Err(export_error("encoded audio copy is empty"));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn encode_m4a(_input: &Path, _output: &Path) -> Result<(), CaptureError> {
    Err(export_error("M4A audio export is available on macOS only"))
}

pub(super) fn save_audio_copy(
    store: &TranscriptStore,
    capture_id: i64,
    staging_root: &Path,
    destination: &Path,
) -> Result<(), CaptureError> {
    let parent = validate_destination(destination)?;
    let capture = store.get_capture(capture_id)?;
    let expected_staging = staging_root.join(capture_id.to_string());
    if capture.staging_path.as_deref() != Some(expected_staging.as_path()) {
        return Err(CaptureError::new(
            CaptureErrorCode::AudioUnavailable,
            "capture has no trusted retained audio",
        ));
    }
    if !matches!(
        capture.status,
        CaptureState::Completed
            | CaptureState::Interrupted
            | CaptureState::CaptureFailed
            | CaptureState::TranscriptionFailed
            | CaptureState::FinalizationFailed
    ) {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidStateTransition,
            "capture audio is still in use",
        ));
    }
    let mut chunks: BTreeMap<i64, (Option<PathBuf>, Option<PathBuf>)> = BTreeMap::new();
    for (ordinal, track, path) in store.audio_chunk_paths(capture_id)? {
        if path.parent() != Some(expected_staging.as_path())
            || path.extension().and_then(|value| value.to_str()) != Some("wav")
        {
            return Err(CaptureError::new(
                CaptureErrorCode::AudioUnavailable,
                "retained audio path failed validation",
            ));
        }
        let pair = chunks.entry(ordinal).or_default();
        match track {
            CaptureTrack::Microphone => pair.0 = Some(path),
            CaptureTrack::System => pair.1 = Some(path),
        }
    }
    let nonce = format!(
        "{}-{capture_id}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default(),
    );
    let temp = TempFiles {
        wav: parent.join(format!(".murmur-audio-{nonce}.wav")),
        encoded: parent.join(format!(".murmur-audio-{nonce}.m4a")),
    };
    if temp.wav.exists() || temp.encoded.exists() {
        return Err(export_error("temporary audio export already exists"));
    }
    write_mixed_wav(&chunks, &temp.wav)?;
    encode_m4a(&temp.wav, &temp.encoded)?;
    #[cfg(unix)]
    fs::set_permissions(
        &temp.encoded,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .map_err(|_| export_error("encoded audio copy could not be secured"))?;
    renameat_with(CWD, &temp.encoded, CWD, destination, RenameFlags::NOREPLACE).map_err(
        |error| {
            if destination.exists() {
                CaptureError::new(
                    CaptureErrorCode::OutputFileExists,
                    "audio export destination already exists",
                )
            } else {
                let _ = error;
                export_error("encoded audio copy could not be saved atomically")
            }
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_must_be_absolute_m4a_and_not_exist() {
        assert_eq!(
            validate_destination(Path::new("relative.m4a"))
                .unwrap_err()
                .code,
            CaptureErrorCode::InvalidRequest
        );
        assert_eq!(
            validate_destination(Path::new("/tmp/copy.wav"))
                .unwrap_err()
                .code,
            CaptureErrorCode::InvalidRequest
        );
    }

    #[test]
    fn downmixes_stereo_pcm() {
        let root = std::env::temp_dir().join(format!("murmur-export-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&root);
        let path = root.join("stereo.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        writer.write_sample(1000_i16).unwrap();
        writer.write_sample(-500_i16).unwrap();
        writer.finalize().unwrap();
        let (rate, mono) = read_mono(&path).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(mono, vec![250]);
        let _ = fs::remove_dir_all(root);
    }
}
