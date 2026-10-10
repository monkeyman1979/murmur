use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const MANIFEST_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCandidateStatus {
    Recoverable,
    MissingManifest,
    InvalidManifest,
    UnknownManifestVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCandidate {
    pub directory: PathBuf,
    pub manifest_path: PathBuf,
    pub capture_id: Option<i64>,
    pub known_capture: bool,
    /// Safe relative filenames for open writer chunks. Recovery never treats
    /// these as complete WAVs until a later validator repairs or quarantines
    /// the provisional header.
    pub open_partial_files: Vec<PathBuf>,
    /// Completed-looking WAVs not referenced by the last atomic manifest.
    /// These can exist only after a crash or ambiguous pair-publication error.
    pub unindexed_wav_files: Vec<PathBuf>,
    pub status: RecoveryCandidateStatus,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub version: u32,
    pub capture_id: i64,
    pub created_at: i64,
    pub active_duration_ms: i64,
    pub paused_duration_ms: i64,
    pub tracks: Vec<String>,
    pub chunks: Vec<ManifestChunk>,
    /// Set once an orderly native stop can persist a source-health verdict.
    /// The default keeps older manifests and crash-open recovery compatible.
    #[serde(default)]
    pub health_required: bool,
    #[serde(default)]
    pub source_health: Vec<CaptureSourceHealth>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CaptureSourceHealth {
    pub track: String,
    pub device_id: u32,
    pub device_name: String,
    pub advertised_sample_rate: u32,
    pub selected_sample_rate: u32,
    pub measured_sample_rate: Option<u32>,
    pub channels: u16,
    pub layout: String,
    pub first_callback_host_ns: Option<i64>,
    pub last_callback_host_ns: Option<i64>,
    pub callback_count: u64,
    pub gap_count: u64,
    pub format_change_count: u64,
    pub peak: f64,
    pub rms: f64,
    pub transcribable: bool,
    pub diagnosis: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManifestChunk {
    pub ordinal: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub microphone: Option<PathBuf>,
    pub system: Option<PathBuf>,
}

impl Manifest {
    #[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
    pub(crate) fn empty(capture_id: i64, created_at: i64) -> Self {
        Self {
            version: MANIFEST_VERSION,
            capture_id,
            created_at,
            active_duration_ms: 0,
            paused_duration_ms: 0,
            tracks: vec!["microphone".into(), "system".into()],
            chunks: Vec::new(),
            health_required: false,
            source_health: Vec::new(),
        }
    }
}

/// Atomically publish safe recovery metadata without ever exposing a
/// truncated completed-looking manifest.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
pub(crate) fn atomic_write_manifest(directory: &Path, manifest: &Manifest) -> io::Result<()> {
    let temporary = directory.join("manifest.json.tmp");
    let final_path = directory.join("manifest.json");
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture manifest exceeds the size limit",
        ));
    }
    match fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(&temporary)?;
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "capture manifest temporary path is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&temporary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    use std::io::Write;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, &final_path)?;
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

pub fn discover(root: &Path) -> io::Result<Vec<RecoveryCandidate>> {
    let mut candidates = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidates),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        // Do not follow staging symlinks during recovery discovery.
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        candidates.push(inspect_directory(entry.path()));
    }
    candidates.sort_by(|left, right| left.directory.cmp(&right.directory));
    Ok(candidates)
}

fn inspect_directory(directory: PathBuf) -> RecoveryCandidate {
    let manifest_path = directory.join("manifest.json");
    let missing = || RecoveryCandidate {
        directory: directory.clone(),
        manifest_path: manifest_path.clone(),
        capture_id: directory_capture_id(&directory),
        known_capture: false,
        open_partial_files: discover_open_partials(&directory),
        unindexed_wav_files: discover_unindexed_wavs(&directory, None),
        status: RecoveryCandidateStatus::MissingManifest,
        message: Some("staging directory has no manifest.json".into()),
    };
    let Ok(metadata) = fs::symlink_metadata(&manifest_path) else {
        return missing();
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return invalid_candidate(
            directory,
            manifest_path,
            None,
            "manifest is not a regular file",
        );
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return invalid_candidate(
            directory,
            manifest_path,
            None,
            "manifest exceeds the recovery size limit",
        );
    }
    let bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return invalid_candidate(directory, manifest_path, None, "manifest could not be read")
        }
    };
    let manifest: Manifest = match serde_json::from_slice(&bytes) {
        Ok(manifest) => manifest,
        Err(_) => {
            return invalid_candidate(directory, manifest_path, None, "manifest JSON is invalid")
        }
    };
    if manifest.version != MANIFEST_VERSION {
        let open_partial_files = discover_open_partials(&directory);
        let unindexed_wav_files = discover_unindexed_wavs(&directory, None);
        return RecoveryCandidate {
            directory,
            manifest_path,
            capture_id: positive_id(manifest.capture_id),
            known_capture: false,
            open_partial_files,
            unindexed_wav_files,
            status: RecoveryCandidateStatus::UnknownManifestVersion,
            message: Some("manifest version is not supported".into()),
        };
    }
    let capture_id = positive_id(manifest.capture_id);
    if capture_id.is_none()
        || directory_capture_id(&directory) != capture_id
        || manifest.created_at < 0
        || manifest.active_duration_ms < 0
        || manifest.paused_duration_ms < 0
        || !valid_tracks(&manifest.tracks)
        || !valid_chunks(&manifest.chunks, &manifest.tracks, &directory)
    {
        return invalid_candidate(
            directory,
            manifest_path,
            capture_id,
            "manifest metadata failed validation",
        );
    }

    let open_partial_files = discover_open_partials(&directory);
    let unindexed_wav_files = discover_unindexed_wavs(&directory, Some(&manifest));
    RecoveryCandidate {
        directory,
        manifest_path,
        capture_id,
        known_capture: false,
        open_partial_files,
        unindexed_wav_files,
        status: RecoveryCandidateStatus::Recoverable,
        message: None,
    }
}

fn valid_tracks(tracks: &[String]) -> bool {
    let unique = tracks.iter().map(String::as_str).collect::<HashSet<_>>();
    !unique.is_empty()
        && unique.len() == tracks.len()
        && unique.len() <= 2
        && unique
            .iter()
            .all(|track| matches!(*track, "microphone" | "system"))
}

fn valid_chunks(chunks: &[ManifestChunk], tracks: &[String], directory: &Path) -> bool {
    let tracks = tracks.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut keys = HashSet::new();
    chunks.iter().all(|chunk| {
        chunk.ordinal >= 0
            && chunk.start_ms >= 0
            && chunk.end_ms >= chunk.start_ms
            && keys.insert(chunk.ordinal)
            // A source may never open or may end before its partner. The
            // manifest's track list describes expected sources, while each
            // chunk may validly contain only the source that produced audio.
            && (chunk.microphone.is_some() || chunk.system.is_some())
            && (chunk.microphone.is_none() || tracks.contains("microphone"))
            && (chunk.system.is_none() || tracks.contains("system"))
            && [chunk.microphone.as_deref(), chunk.system.as_deref()]
                .into_iter()
                .flatten()
                .all(|path| safe_staging_path(directory, path))
    })
}

fn safe_staging_path(directory: &Path, path: &Path) -> bool {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return false;
    }

    // Reject any existing symlink component before later milestones open a
    // discovered chunk. Missing leaf files are allowed because MT1 only
    // discovers metadata; the recorder/recovery validator owns audio checks.
    let mut current = directory.to_path_buf();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return false;
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(_) => return false,
        }
    }
    true
}

pub(crate) fn discover_open_partials(directory: &Path) -> Vec<PathBuf> {
    discover_audio_files(directory, ".wav.partial")
}

pub(crate) fn discover_unindexed_wavs(
    directory: &Path,
    manifest: Option<&Manifest>,
) -> Vec<PathBuf> {
    let indexed = manifest
        .into_iter()
        .flat_map(|manifest| &manifest.chunks)
        .flat_map(|chunk| [chunk.microphone.as_ref(), chunk.system.as_ref()])
        .flatten()
        .cloned()
        .collect::<HashSet<_>>();
    discover_audio_files(directory, ".wav")
        .into_iter()
        .filter(|path| !indexed.contains(path))
        .collect()
}

/// Read-only capability check used by capture summaries. It deliberately
/// mirrors processing/recovery path and WAV checks without repairing partials
/// or changing persisted state while the library is being listed.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
pub(crate) fn has_recoverable_audio(
    capture_id: i64,
    staging: Option<&Path>,
    expected_root: &Path,
    units: &[crate::captures::ProcessingUnit],
) -> bool {
    use crate::captures::{CaptureChunkState, CaptureTrack};

    let Some(staging) = staging else {
        return false;
    };
    if !staging.is_absolute()
        || !expected_root.is_absolute()
        || staging != expected_root.join(capture_id.to_string())
    {
        return false;
    }
    let Ok(root_meta) = fs::symlink_metadata(expected_root) else {
        return false;
    };
    let Ok(staging_meta) = fs::symlink_metadata(staging) else {
        return false;
    };
    if !root_meta.is_dir()
        || root_meta.file_type().is_symlink()
        || !staging_meta.is_dir()
        || staging_meta.file_type().is_symlink()
    {
        return false;
    }
    let Ok(canonical_root) = fs::canonicalize(expected_root) else {
        return false;
    };
    let Ok(canonical_staging) = fs::canonicalize(staging) else {
        return false;
    };
    if canonical_staging != canonical_root.join(capture_id.to_string()) {
        return false;
    }

    let candidate = inspect_directory(staging.to_path_buf());
    if candidate.status != RecoveryCandidateStatus::Recoverable
        || candidate.capture_id != Some(capture_id)
    {
        return false;
    }
    let Ok(manifest) = read_manifest(staging, capture_id) else {
        return false;
    };
    if manifest.health_required {
        let healthy = |track: &str| {
            manifest
                .source_health
                .iter()
                .find(|health| health.track == track)
                .is_some_and(|health| health.transcribable)
        };
        if !healthy("microphone") || !healthy("system") {
            return false;
        }
    }

    let manifest_units = manifest
        .chunks
        .iter()
        .flat_map(|chunk| {
            [
                (CaptureTrack::Microphone, chunk.microphone.as_ref()),
                (CaptureTrack::System, chunk.system.as_ref()),
            ]
            .into_iter()
            .filter_map(move |(track, path)| {
                path.map(|path| (chunk.ordinal, track, path, chunk.start_ms, chunk.end_ms))
            })
        })
        .collect::<Vec<_>>();
    if manifest_units.len() != units.len() {
        return false;
    }

    let mut indexed_has_frames = false;
    for unit in units {
        if !matches!(
            unit.status,
            CaptureChunkState::Ready
                | CaptureChunkState::Transcribing
                | CaptureChunkState::Transcribed
        ) || unit.byte_length <= 0
            || !unit.path.is_absolute()
        {
            return false;
        }
        let Some((_, _, relative, pair_start, pair_end)) = manifest_units
            .iter()
            .find(|(ordinal, track, _, _, _)| *ordinal == unit.ordinal && *track == unit.track)
        else {
            return false;
        };
        let expected_name = match unit.track {
            CaptureTrack::Microphone => format!("mic-{:05}.wav", unit.ordinal),
            CaptureTrack::System => format!("system-{:05}.wav", unit.ordinal),
        };
        if unit.path != staging.join(relative)
            || relative.as_path() != Path::new(&expected_name)
            || relative.components().count() != 1
            || unit.start_ms < *pair_start
            || unit.end_ms > *pair_end
            || unit.end_ms < unit.start_ms
        {
            return false;
        }
        let Ok(metadata) = fs::symlink_metadata(&unit.path) else {
            return false;
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != unit.byte_length as u64
        {
            return false;
        }
        let Ok(canonical) = fs::canonicalize(&unit.path) else {
            return false;
        };
        if canonical.parent() != Some(canonical_staging.as_path()) {
            return false;
        }
        let Ok(reader) = hound::WavReader::open(&unit.path) else {
            return false;
        };
        indexed_has_frames |= reader.duration() > 0;
    }

    let indexed_ordinals = manifest
        .chunks
        .iter()
        .map(|chunk| chunk.ordinal)
        .collect::<HashSet<_>>();

    let mut partial_has_frames = false;
    for relative in &candidate.open_partial_files {
        let Some(frames) = open_partial_frames(&staging.join(relative)) else {
            return false;
        };
        let Some((_, ordinal)) = relative.to_str().and_then(parse_partial_name) else {
            return false;
        };
        // Recovery can safely delete an empty same-ordinal orphan, but it does
        // not merge a nonempty missing counterpart into an existing manifest
        // chunk. Fail closed instead of advertising a retry that must fail.
        if frames > 0 && indexed_ordinals.contains(&ordinal) {
            return false;
        }
        partial_has_frames |= frames > 0;
    }

    let mut unindexed_has_frames = false;
    for relative in &candidate.unindexed_wav_files {
        let path = staging.join(relative);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return false;
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return false;
        }
        let Ok(reader) = hound::WavReader::open(path) else {
            return false;
        };
        let frames = u64::from(reader.duration());
        let Some((_, ordinal)) = relative
            .to_str()
            .and_then(|name| parse_partial_name(&format!("{name}.partial")))
        else {
            return false;
        };
        if frames > 0 && indexed_ordinals.contains(&ordinal) {
            return false;
        }
        unindexed_has_frames |= frames > 0;
    }

    indexed_has_frames || partial_has_frames || unindexed_has_frames
}

#[cfg(not(any(test, all(target_os = "macos", feature = "meeting-capture"))))]
pub(crate) fn has_recoverable_audio(
    _capture_id: i64,
    _staging: Option<&Path>,
    _expected_root: &Path,
    _units: &[crate::captures::ProcessingUnit],
) -> bool {
    false
}

fn discover_audio_files(directory: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_file() || file_type.is_symlink() {
                return None;
            }
            let name = entry.file_name();
            let name = name.to_str()?;
            let ordinal = name
                .strip_prefix("mic-")
                .or_else(|| name.strip_prefix("system-"))?
                .strip_suffix(suffix)?;
            if ordinal.len() != 5 || !ordinal.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some(PathBuf::from(name))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

/// Read and sanity-check a staging manifest for a known capture.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
pub(crate) fn read_manifest(
    staging: &Path,
    capture_id: i64,
) -> Result<Manifest, crate::captures::CaptureError> {
    use crate::captures::{CaptureError, CaptureErrorCode};
    let invalid = || {
        CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "capture staging metadata is invalid",
        )
    };
    let manifest_path = staging.join("manifest.json");
    let metadata = fs::symlink_metadata(&manifest_path).map_err(|_| invalid())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Err(invalid());
    }
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    if manifest.version != MANIFEST_VERSION || manifest.capture_id != capture_id {
        return Err(invalid());
    }
    Ok(manifest)
}

/// Append one repaired chunk pair to a manifest, mirroring how the recorder's
/// `publish` merges the two tracks into a single indexed range.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
pub(crate) fn push_manifest_chunk(
    manifest: &mut Manifest,
    ordinal: i64,
    microphone: Option<(PathBuf, i64, i64)>,
    system: Option<(PathBuf, i64, i64)>,
) {
    let bounds =
        |entry: &Option<(PathBuf, i64, i64)>| entry.as_ref().map(|(_, start, end)| (*start, *end));
    let (start_ms, end_ms) = match (bounds(&microphone), bounds(&system)) {
        (Some((left_start, left_end)), Some((right_start, right_end))) => {
            (left_start.min(right_start), left_end.max(right_end))
        }
        (Some((start, end)), None) | (None, Some((start, end))) => (start, end),
        (None, None) => return,
    };
    manifest.active_duration_ms = manifest.active_duration_ms.max(end_ms);
    manifest.chunks.push(ManifestChunk {
        ordinal,
        start_ms,
        end_ms,
        microphone: microphone.map(|(name, _, _)| name),
        system: system.map(|(name, _, _)| name),
    });
    manifest.chunks.sort_by_key(|chunk| chunk.ordinal);
}

/// What a repaired `.partial` turned out to hold.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepairedPartial {
    pub track: crate::captures::CaptureTrack,
    pub ordinal: i64,
    /// Still the `.partial` path. Publication renames it, never this function.
    pub partial_path: PathBuf,
    pub frames: u64,
    pub sample_rate: u32,
    pub duration_ms: i64,
    pub byte_length: i64,
}

/// Split `mic-00007.wav.partial` into its track and ordinal.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
pub(crate) fn parse_partial_name(name: &str) -> Option<(crate::captures::CaptureTrack, i64)> {
    let stem = name.strip_suffix(".wav.partial")?;
    let (track, digits) = match stem.strip_prefix("mic-") {
        Some(digits) => (crate::captures::CaptureTrack::Microphone, digits),
        None => (
            crate::captures::CaptureTrack::System,
            stem.strip_prefix("system-")?,
        ),
    };
    if digits.len() != 5 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().map(|ordinal| (track, ordinal))
}

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
fn open_partial_frames(path: &Path) -> Option<u64> {
    use std::io::Read;

    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let mut file = fs::File::open(path).ok()?;
    let mut header = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_WAV_HEADER_BYTES)
        .read_to_end(&mut header)
        .ok()?;
    let layout = parse_wav_layout(&header)?;
    let data_bytes = metadata.len().checked_sub(u64::from(layout.data_start))?;
    Some(data_bytes / u64::from(layout.block_align))
}

/// Repair one open `.partial` WAV in place so it becomes a readable file.
///
/// A `.partial` is what the recorder leaves behind when the process dies
/// mid-chunk. `hound` writes the RIFF and data length fields as zero up front
/// and only patches them when the writer is finalized or dropped, so a
/// force-quit leaves a correctly shaped header advertising no audio at all,
/// followed by however many real PCM bytes reached the disk. Up to one
/// `BufWriter` of tail samples is simply gone, and the last frame on disk can
/// be torn.
///
/// Repair therefore trusts the file length rather than the stored fields:
/// truncate to a whole number of frames, then write the two lengths that
/// describe what is actually there. A file holding no complete frame is
/// reported as `frames == 0` and left untouched for the caller to discard.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
pub(crate) fn repair_open_partial(
    path: &Path,
) -> Result<RepairedPartial, crate::captures::CaptureError> {
    use crate::captures::{CaptureError, CaptureErrorCode};
    use std::io::{Read, Seek, SeekFrom, Write};

    let invalid = || {
        CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "meeting partial WAV could not be repaired",
        )
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    let (track, ordinal) = parse_partial_name(name).ok_or_else(invalid)?;

    let metadata = fs::symlink_metadata(path).map_err(|_| invalid())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid());
    }
    let file_length = metadata.len();

    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| invalid())?;
    let mut header = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_WAV_HEADER_BYTES)
        .read_to_end(&mut header)
        .map_err(|_| invalid())?;
    let layout = parse_wav_layout(&header).ok_or_else(invalid)?;

    let data_start = u64::from(layout.data_start);
    if file_length < data_start {
        return Err(invalid());
    }
    let block_align = u64::from(layout.block_align);
    let frames = (file_length - data_start) / block_align;
    let data_length = frames.saturating_mul(block_align);
    let total_length = data_start.saturating_add(data_length);

    // A torn trailing frame is dropped rather than described; nothing else in
    // the pipeline can interpret a fraction of a frame.
    if total_length != file_length {
        file.set_len(total_length).map_err(|_| invalid())?;
    }
    let riff_length = u32::try_from(total_length.saturating_sub(8)).map_err(|_| invalid())?;
    let data_length_field = u32::try_from(data_length).map_err(|_| invalid())?;
    file.seek(SeekFrom::Start(4)).map_err(|_| invalid())?;
    file.write_all(&riff_length.to_le_bytes())
        .map_err(|_| invalid())?;
    file.seek(SeekFrom::Start(u64::from(layout.data_length_offset)))
        .map_err(|_| invalid())?;
    file.write_all(&data_length_field.to_le_bytes())
        .map_err(|_| invalid())?;
    file.sync_all().map_err(|_| invalid())?;

    Ok(RepairedPartial {
        track,
        ordinal,
        partial_path: path.to_path_buf(),
        frames,
        sample_rate: layout.sample_rate,
        duration_ms: frames_to_millis(frames, layout.sample_rate),
        byte_length: i64::try_from(total_length).unwrap_or(i64::MAX),
    })
}

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
pub(crate) fn frames_to_millis(frames: u64, sample_rate: u32) -> i64 {
    let millis = u128::from(frames).saturating_mul(1_000) / u128::from(sample_rate.max(1));
    i64::try_from(millis).unwrap_or(i64::MAX)
}

/// A `fmt `/`data` chunk header is never this far into a WAV we wrote.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
const MAX_WAV_HEADER_BYTES: u64 = 4_096;

#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WavLayout {
    sample_rate: u32,
    block_align: u16,
    /// Byte offset of the `data` chunk's 32-bit length field.
    data_length_offset: u32,
    /// Byte offset of the first PCM byte.
    data_start: u32,
}

/// Walk a RIFF/WAVE chunk list far enough to locate `fmt ` and `data`.
///
/// Deliberately does not trust the `data` chunk's declared length — on the
/// files this exists to repair, that field is exactly what is wrong.
#[cfg(any(test, all(target_os = "macos", feature = "meeting-capture")))]
fn parse_wav_layout(header: &[u8]) -> Option<WavLayout> {
    if header.len() < 12 || &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return None;
    }
    let read_u16 = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(header.get(at..at + 2)?.try_into().ok()?))
    };
    let read_u32 = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(header.get(at..at + 4)?.try_into().ok()?))
    };

    let mut sample_rate = None;
    let mut block_align = None;
    let mut position = 12_usize;
    loop {
        let id = header.get(position..position + 4)?;
        let declared = read_u32(position + 4)?;
        let payload = position.checked_add(8)?;
        if id == b"data" {
            let (sample_rate, block_align) = (sample_rate?, block_align?);
            if sample_rate == 0 || block_align == 0 {
                return None;
            }
            return Some(WavLayout {
                sample_rate,
                block_align,
                data_length_offset: u32::try_from(position + 4).ok()?,
                data_start: u32::try_from(payload).ok()?,
            });
        }
        if id == b"fmt " {
            // wFormatTag, nChannels, nSamplesPerSec, nAvgBytesPerSec,
            // nBlockAlign, wBitsPerSample.
            if declared < 16 {
                return None;
            }
            sample_rate = read_u32(payload + 4);
            block_align = read_u16(payload + 12);
        }
        // RIFF chunks are word-aligned; an odd length carries a pad byte.
        position = payload.checked_add(declared as usize + (declared as usize & 1))?;
    }
}

fn directory_capture_id(directory: &Path) -> Option<i64> {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse().ok())
        .and_then(positive_id)
}

fn positive_id(id: i64) -> Option<i64> {
    (id > 0).then_some(id)
}

pub(crate) fn validate_processing_manifest(
    staging: &Path,
    capture_id: i64,
    expected_root: &Path,
    units: &[crate::captures::ProcessingUnit],
) -> Result<(), crate::captures::CaptureError> {
    use crate::captures::{CaptureError, CaptureErrorCode};
    let invalid = || {
        CaptureError::new(
            CaptureErrorCode::ChunkInvalid,
            "capture staging metadata is invalid",
        )
    };
    if !staging.is_absolute() || !expected_root.is_absolute() {
        return Err(invalid());
    }
    let root_meta = fs::symlink_metadata(expected_root).map_err(|_| invalid())?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err(invalid());
    }
    let root = fs::canonicalize(expected_root).map_err(|_| invalid())?;
    let staging_meta = fs::symlink_metadata(staging).map_err(|_| invalid())?;
    if !staging_meta.is_dir() || staging_meta.file_type().is_symlink() {
        return Err(invalid());
    }
    let canonical_staging = fs::canonicalize(staging).map_err(|_| invalid())?;
    let expected_staging = root.join(capture_id.to_string());
    if canonical_staging != fs::canonicalize(&expected_staging).map_err(|_| invalid())? {
        return Err(invalid());
    }
    let manifest_path = staging.join("manifest.json");
    let manifest_meta = fs::symlink_metadata(&manifest_path).map_err(|_| invalid())?;
    if !manifest_meta.is_file()
        || manifest_meta.file_type().is_symlink()
        || manifest_meta.len() > MAX_MANIFEST_BYTES
    {
        return Err(invalid());
    }
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    if manifest.version != MANIFEST_VERSION || manifest.capture_id != capture_id {
        return Err(invalid());
    }
    if manifest.health_required {
        let microphone = manifest
            .source_health
            .iter()
            .find(|health| health.track == "microphone");
        let system = manifest
            .source_health
            .iter()
            .find(|health| health.track == "system");
        if !microphone.is_some_and(|health| health.transcribable)
            || !system.is_some_and(|health| health.transcribable)
        {
            return Err(CaptureError::new(
                CaptureErrorCode::AudioFormatChanged,
                "capture audio failed media-clock integrity validation",
            ));
        }
    }
    let indexed_names = manifest
        .chunks
        .iter()
        .flat_map(|chunk| [chunk.microphone.as_ref(), chunk.system.as_ref()])
        .flatten()
        .filter_map(|path| path.file_name().map(|name| name.to_os_string()))
        .collect::<HashSet<_>>();
    let has_extra_audio = fs::read_dir(staging)
        .map_err(|_| invalid())?
        .filter_map(Result::ok)
        .any(|entry| {
            let name = entry.file_name();
            let text = name.to_string_lossy();
            (text.ends_with(".wav") || text.ends_with(".wav.partial"))
                && !indexed_names.contains(&name)
        });
    if !discover_open_partials(staging).is_empty()
        || !discover_unindexed_wavs(staging, Some(&manifest)).is_empty()
        || has_extra_audio
    {
        return Err(invalid());
    }
    // A manifest chunk carries the *pair's* merged range (the minimum start
    // and maximum end across both tracks), while each database unit carries
    // its own track's exact range. The two tracks almost never end on the same
    // millisecond in a real capture — they arrive in different frame counts
    // per callback — so the manifest range is compared by containment, not by
    // equality. Identity still has to match exactly.
    let mut manifest_units = HashMap::new();
    for chunk in &manifest.chunks {
        for (track, relative) in [
            ("microphone", chunk.microphone.as_ref()),
            ("system", chunk.system.as_ref()),
        ] {
            if let Some(relative) = relative {
                let identity = (chunk.ordinal, track.to_string(), relative.clone());
                if manifest_units
                    .insert(identity, (chunk.start_ms, chunk.end_ms))
                    .is_some()
                {
                    return Err(invalid());
                }
            }
        }
    }
    let mut seen = HashSet::new();
    for unit in units {
        if !matches!(
            unit.status,
            crate::captures::CaptureChunkState::Ready
                | crate::captures::CaptureChunkState::Transcribing
                | crate::captures::CaptureChunkState::Transcribed
        ) {
            return Err(invalid());
        }
        let path = &unit.path;
        if !path.is_absolute() || path.to_str().is_none() {
            return Err(invalid());
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| invalid())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != unit.byte_length as u64
        {
            return Err(invalid());
        }
        let canonical = fs::canonicalize(path).map_err(|_| invalid())?;
        if !canonical.starts_with(&canonical_staging) {
            return Err(invalid());
        }
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(invalid)?;
        let prefix = match unit.track {
            crate::captures::CaptureTrack::Microphone => "mic-",
            crate::captures::CaptureTrack::System => "system-",
        };
        if filename != format!("{prefix}{:05}.wav", unit.ordinal) {
            return Err(invalid());
        }
        let relative = path.strip_prefix(staging).map_err(|_| invalid())?;
        if relative.components().count() != 1 {
            return Err(invalid());
        }
        let identity = (
            unit.ordinal,
            unit.track.as_str().to_string(),
            relative.to_path_buf(),
        );
        let Some(&(pair_start_ms, pair_end_ms)) = manifest_units.get(&identity) else {
            return Err(invalid());
        };
        if unit.end_ms < unit.start_ms
            || unit.start_ms < pair_start_ms
            || unit.end_ms > pair_end_ms
            || !seen.insert(identity)
        {
            return Err(invalid());
        }
        hound::WavReader::open(path).map_err(|_| invalid())?;
    }
    if seen.len() != manifest_units.len() || manifest_units.len() != units.len() {
        return Err(invalid());
    }
    Ok(())
}

fn invalid_candidate(
    directory: PathBuf,
    manifest_path: PathBuf,
    capture_id: Option<i64>,
    message: &str,
) -> RecoveryCandidate {
    let open_partial_files = discover_open_partials(&directory);
    let unindexed_wav_files = discover_unindexed_wavs(&directory, None);
    RecoveryCandidate {
        directory,
        manifest_path,
        capture_id,
        known_capture: false,
        open_partial_files,
        unindexed_wav_files,
        status: RecoveryCandidateStatus::InvalidManifest,
        message: Some(message.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "murmur-recovery-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn atomic_manifest_update_replaces_complete_version_without_tmp_residue() {
        let root = root("atomic-write");
        let directory = root.join("42");
        fs::create_dir(&directory).unwrap();
        let mut manifest = Manifest::empty(42, 1);
        atomic_write_manifest(&directory, &manifest).unwrap();
        manifest.active_duration_ms = 1_000;
        manifest.chunks.push(ManifestChunk {
            ordinal: 0,
            start_ms: 0,
            end_ms: 1_000,
            microphone: Some("mic-00000.wav".into()),
            system: Some("system-00000.wav".into()),
        });
        atomic_write_manifest(&directory, &manifest).unwrap();
        assert!(!directory.join("manifest.json.tmp").exists());
        let persisted: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(persisted, manifest);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovery_exposes_finalized_wavs_missing_from_atomic_manifest() {
        let root = root("unindexed-wav");
        let directory = root.join("42");
        fs::create_dir(&directory).unwrap();
        atomic_write_manifest(&directory, &Manifest::empty(42, 1)).unwrap();
        fs::write(directory.join("mic-00000.wav"), b"complete-ish").unwrap();
        fs::write(directory.join("system-00000.wav.partial"), b"partial").unwrap();
        let found = discover(&root).unwrap();
        assert_eq!(
            found[0].unindexed_wav_files,
            vec![PathBuf::from("mic-00000.wav")]
        );
        assert_eq!(
            found[0].open_partial_files,
            vec![PathBuf::from("system-00000.wav.partial")]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn valid_manifest_is_recoverable() {
        let root = root("valid");
        let directory = root.join("42");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("manifest.json"),
            br#"{
                "version": 1,
                "capture_id": 42,
                "created_at": 1,
                "active_duration_ms": 1000,
                "paused_duration_ms": 0,
                "tracks": ["microphone", "system"],
                "chunks": [{
                    "ordinal": 0,
                    "start_ms": 0,
                    "end_ms": 1000,
                    "microphone": "mic-00000.wav",
                    "system": "system-00000.wav"
                }]
            }"#,
        )
        .unwrap();
        let found = discover(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].capture_id, Some(42));
        assert_eq!(found[0].status, RecoveryCandidateStatus::Recoverable);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn traversal_and_id_mismatch_are_invalid() {
        let root = root("invalid");
        let directory = root.join("42");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("manifest.json"),
            br#"{
                "version": 1,
                "capture_id": 43,
                "created_at": 1,
                "active_duration_ms": 1000,
                "paused_duration_ms": 0,
                "tracks": ["microphone"],
                "chunks": [{
                    "ordinal": 0,
                    "start_ms": 0,
                    "end_ms": 1000,
                    "microphone": "../escape.wav"
                }]
            }"#,
        )
        .unwrap();
        let found = discover(&root).unwrap();
        assert_eq!(found[0].status, RecoveryCandidateStatus::InvalidManifest);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn track_mismatch_is_invalid() {
        let root = root("track-mismatch");
        let directory = root.join("42");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("manifest.json"),
            br#"{
                "version": 1,
                "capture_id": 42,
                "created_at": 1,
                "active_duration_ms": 1000,
                "paused_duration_ms": 0,
                "tracks": ["microphone"],
                "chunks": [{
                    "ordinal": 0,
                    "start_ms": 0,
                    "end_ms": 1000,
                    "system": "system-00000.wav"
                }]
            }"#,
        )
        .unwrap();
        let found = discover(&root).unwrap();
        assert_eq!(found[0].status, RecoveryCandidateStatus::InvalidManifest);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn nested_symlink_escape_is_invalid() {
        use std::os::unix::fs::symlink;

        let staging_root = root("symlink");
        let outside = root("outside");
        let directory = staging_root.join("42");
        fs::create_dir(&directory).unwrap();
        symlink(&outside, directory.join("audio")).unwrap();
        fs::write(
            directory.join("manifest.json"),
            br#"{
                "version": 1,
                "capture_id": 42,
                "created_at": 1,
                "active_duration_ms": 1000,
                "paused_duration_ms": 0,
                "tracks": ["microphone"],
                "chunks": [{
                    "ordinal": 0,
                    "start_ms": 0,
                    "end_ms": 1000,
                    "microphone": "audio/mic-00000.wav"
                }]
            }"#,
        )
        .unwrap();
        let found = discover(&staging_root).unwrap();
        assert_eq!(found[0].status, RecoveryCandidateStatus::InvalidManifest);
        fs::remove_dir_all(staging_root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn inaccessible_root_is_an_error() {
        let root = root("root-error");
        let not_a_directory = root.join("file");
        fs::write(&not_a_directory, b"not a directory").unwrap();
        assert!(discover(&not_a_directory).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    /// Write a real single-channel WAV and return its byte length.
    fn write_wav(path: &Path, frames: u32) -> i64 {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for index in 0..frames {
            writer.write_sample(index as i16).unwrap();
        }
        writer.finalize().unwrap();
        fs::metadata(path).unwrap().len() as i64
    }

    /// One published dual-track chunk pair, exactly as the recorder leaves it:
    /// per-track ranges in the units, the merged pair range in the manifest.
    fn published_pair(
        directory: &Path,
        capture_id: i64,
        microphone_end_ms: i64,
        system_end_ms: i64,
    ) -> Vec<crate::captures::ProcessingUnit> {
        use crate::captures::{CaptureChunkState, CaptureTrack, ProcessingUnit};

        let microphone_path = directory.join("mic-00000.wav");
        let system_path = directory.join("system-00000.wav");
        let microphone_bytes = write_wav(&microphone_path, 64);
        let system_bytes = write_wav(&system_path, 32);
        let manifest = Manifest {
            version: MANIFEST_VERSION,
            capture_id,
            created_at: 1,
            active_duration_ms: microphone_end_ms.max(system_end_ms),
            paused_duration_ms: 0,
            tracks: vec!["microphone".into(), "system".into()],
            chunks: vec![ManifestChunk {
                ordinal: 0,
                start_ms: 0,
                end_ms: microphone_end_ms.max(system_end_ms),
                microphone: Some("mic-00000.wav".into()),
                system: Some("system-00000.wav".into()),
            }],
            health_required: false,
            source_health: Vec::new(),
        };
        atomic_write_manifest(directory, &manifest).unwrap();
        vec![
            ProcessingUnit {
                id: 1,
                capture_id,
                ordinal: 0,
                track: CaptureTrack::Microphone,
                start_ms: 0,
                end_ms: microphone_end_ms,
                path: microphone_path,
                byte_length: microphone_bytes,
                status: CaptureChunkState::Ready,
            },
            ProcessingUnit {
                id: 2,
                capture_id,
                ordinal: 0,
                track: CaptureTrack::System,
                start_ms: 0,
                end_ms: system_end_ms,
                path: system_path,
                byte_length: system_bytes,
                status: CaptureChunkState::Ready,
            },
        ]
    }

    #[test]
    fn integrity_invalid_health_retains_audio_but_blocks_processing() {
        let root = root("invalid-health");
        let directory = root.join("37");
        fs::create_dir(&directory).unwrap();
        let units = published_pair(&directory, 37, 1_000, 1_000);
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        manifest.health_required = true;
        manifest.source_health = vec![
            CaptureSourceHealth {
                track: "microphone".into(),
                device_id: 1,
                device_name: "test microphone".into(),
                advertised_sample_rate: 48_000,
                selected_sample_rate: 48_000,
                measured_sample_rate: Some(48_000),
                channels: 1,
                layout: "interleaved".into(),
                first_callback_host_ns: Some(0),
                last_callback_host_ns: Some(1_000_000_000),
                callback_count: 100,
                gap_count: 0,
                format_change_count: 0,
                peak: 0.5,
                rms: 0.1,
                transcribable: true,
                diagnosis: None,
            },
            CaptureSourceHealth {
                track: "system".into(),
                device_id: 2,
                device_name: "test output".into(),
                advertised_sample_rate: 48_000,
                selected_sample_rate: 48_000,
                measured_sample_rate: Some(16_000),
                channels: 2,
                layout: "interleaved".into(),
                first_callback_host_ns: Some(0),
                last_callback_host_ns: Some(1_000_000_000),
                callback_count: 100,
                gap_count: 0,
                format_change_count: 1,
                peak: 0.5,
                rms: 0.1,
                transcribable: false,
                diagnosis: Some("sample_rate_or_route_changed".into()),
            },
        ];
        atomic_write_manifest(&directory, &manifest).unwrap();

        let error = validate_processing_manifest(&directory, 37, &root, &units).unwrap_err();
        assert_eq!(
            error.code,
            crate::captures::CaptureErrorCode::AudioFormatChanged
        );
        assert!(directory.join("mic-00000.wav").is_file());
        assert!(directory.join("system-00000.wav").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tracks_ending_on_different_milliseconds_still_validate() {
        // The real-capture case: microphone and system audio arrive in
        // different frame counts per callback, so the two tracks stop on
        // different milliseconds and the manifest records their merged range.
        // Requiring each unit's range to equal that merged range failed every
        // genuine capture.
        let root = root("asymmetric-pair");
        let directory = root.join("16");
        fs::create_dir(&directory).unwrap();
        let units = published_pair(&directory, 16, 20_950, 20_906);
        validate_processing_manifest(&directory, 16, &root, &units).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identical_track_ranges_still_validate() {
        let root = root("symmetric-pair");
        let directory = root.join("16");
        fs::create_dir(&directory).unwrap();
        let units = published_pair(&directory, 16, 20_950, 20_950);
        validate_processing_manifest(&directory, 16, &root, &units).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unit_range_outside_the_manifest_pair_range_is_invalid() {
        let root = root("range-escape");
        let directory = root.join("16");
        fs::create_dir(&directory).unwrap();
        let mut units = published_pair(&directory, 16, 20_950, 20_906);
        // One millisecond past the pair's merged end.
        units[0].end_ms = 20_951;
        let error = validate_processing_manifest(&directory, 16, &root, &units).unwrap_err();
        assert_eq!(error.code, crate::captures::CaptureErrorCode::ChunkInvalid);
        units[0].end_ms = 20_950;
        units[1].start_ms = -1;
        let error = validate_processing_manifest(&directory, 16, &root, &units).unwrap_err();
        assert_eq!(error.code, crate::captures::CaptureErrorCode::ChunkInvalid);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unit_identity_must_match_the_manifest_exactly() {
        let root = root("identity");
        let directory = root.join("16");
        fs::create_dir(&directory).unwrap();
        let mut units = published_pair(&directory, 16, 20_950, 20_906);
        units[1].ordinal = 1;
        let error = validate_processing_manifest(&directory, 16, &root, &units).unwrap_err();
        assert_eq!(error.code, crate::captures::CaptureErrorCode::ChunkInvalid);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn byte_length_and_inverted_range_guards_survive_the_containment_check() {
        let root = root("guards");
        let directory = root.join("16");
        fs::create_dir(&directory).unwrap();
        let mut units = published_pair(&directory, 16, 20_950, 20_906);
        let real_length = units[0].byte_length;
        units[0].byte_length = real_length - 1;
        let error = validate_processing_manifest(&directory, 16, &root, &units).unwrap_err();
        assert_eq!(error.code, crate::captures::CaptureErrorCode::ChunkInvalid);
        units[0].byte_length = real_length;
        units[0].start_ms = 20_000;
        units[0].end_ms = 19_000;
        let error = validate_processing_manifest(&directory, 16, &root, &units).unwrap_err();
        assert_eq!(error.code, crate::captures::CaptureErrorCode::ChunkInvalid);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_leftover_open_partial_still_blocks_processing() {
        let root = root("open-partial");
        let directory = root.join("16");
        fs::create_dir(&directory).unwrap();
        let units = published_pair(&directory, 16, 20_950, 20_906);
        fs::write(directory.join("mic-00001.wav.partial"), b"partial").unwrap();
        let error = validate_processing_manifest(&directory, 16, &root, &units).unwrap_err();
        assert_eq!(error.code, crate::captures::CaptureErrorCode::ChunkInvalid);
        fs::remove_dir_all(root).unwrap();
    }

    /// Reproduce exactly what a killed recorder leaves on disk: the header
    /// `hound` writes up front, with both length fields still zero, followed
    /// by whatever PCM escaped the buffer.
    fn crashed_partial(path: &Path, channels: u16, sample_rate: u32, data: &[u8]) {
        let block_align = channels * 2;
        let mut header = Vec::new();
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&0_u32.to_le_bytes()); // never patched
        header.extend_from_slice(b"WAVE");
        header.extend_from_slice(b"fmt ");
        header.extend_from_slice(&16_u32.to_le_bytes());
        header.extend_from_slice(&1_u16.to_le_bytes()); // WAVE_FORMAT_PCM
        header.extend_from_slice(&channels.to_le_bytes());
        header.extend_from_slice(&sample_rate.to_le_bytes());
        header.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
        header.extend_from_slice(&block_align.to_le_bytes());
        header.extend_from_slice(&16_u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&0_u32.to_le_bytes()); // never patched
        assert_eq!(header.len(), 44);
        header.extend_from_slice(data);
        fs::write(path, &header).unwrap();
    }

    #[test]
    fn repairs_a_crashed_partial_into_a_readable_wav() {
        let root = root("repair-crashed");
        let path = root.join("mic-00001.wav.partial");
        // 480 mono frames of real audio behind a header claiming zero.
        let samples: Vec<u8> = (0..480_i16).flat_map(|value| value.to_le_bytes()).collect();
        crashed_partial(&path, 1, 48_000, &samples);
        assert!(
            hound::WavReader::open(&path).is_err()
                || hound::WavReader::open(&path).unwrap().duration() == 0
        );

        let repaired = repair_open_partial(&path).unwrap();
        assert_eq!(repaired.track, crate::captures::CaptureTrack::Microphone);
        assert_eq!(repaired.ordinal, 1);
        assert_eq!(repaired.frames, 480);
        assert_eq!(repaired.sample_rate, 48_000);
        assert_eq!(repaired.duration_ms, 10);
        assert_eq!(repaired.byte_length, 44 + 960);

        // The whole point: the audio is now reachable.
        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 480);
        let decoded: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(decoded.len(), 480);
        assert_eq!(decoded[0], 0);
        assert_eq!(decoded[479], 479);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_torn_final_frame_is_truncated_rather_than_described() {
        let root = root("repair-torn");
        let path = root.join("system-00002.wav.partial");
        // Stereo: 4 bytes per frame. 10 frames plus a half-written 11th.
        let mut samples: Vec<u8> = (0..20_i16).flat_map(|value| value.to_le_bytes()).collect();
        samples.extend_from_slice(&[0x7f, 0x00]);
        crashed_partial(&path, 2, 48_000, &samples);

        let repaired = repair_open_partial(&path).unwrap();
        assert_eq!(repaired.track, crate::captures::CaptureTrack::System);
        assert_eq!(repaired.ordinal, 2);
        assert_eq!(repaired.frames, 10);
        assert_eq!(fs::metadata(&path).unwrap().len(), 44 + 40);
        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 10);
        assert_eq!(reader.samples::<i16>().count(), 20);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_header_only_partial_reports_no_frames() {
        let root = root("repair-empty");
        let path = root.join("mic-00000.wav.partial");
        crashed_partial(&path, 1, 48_000, &[]);
        let repaired = repair_open_partial(&path).unwrap();
        assert_eq!(repaired.frames, 0);
        assert_eq!(repaired.duration_ms, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_already_finalized_partial_repairs_to_the_same_content() {
        let root = root("repair-idempotent");
        let path = root.join("mic-00003.wav.partial");
        // A cancelled capture drops the writer, and hound's Drop patches the
        // header — so this file is already well-formed. Repair must not
        // corrupt it, and must agree with what it already says.
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for value in 0..100_i16 {
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
        let before = fs::read(&path).unwrap();

        let repaired = repair_open_partial(&path).unwrap();
        assert_eq!(repaired.frames, 100);
        assert_eq!(repaired.sample_rate, 16_000);
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn non_wav_and_misnamed_partials_are_rejected() {
        let root = root("repair-reject");
        let bogus = root.join("mic-00000.wav.partial");
        fs::write(&bogus, b"this is not a RIFF file at all").unwrap();
        assert_eq!(
            repair_open_partial(&bogus).unwrap_err().code,
            crate::captures::CaptureErrorCode::ChunkInvalid
        );

        let misnamed = root.join("notes-00000.wav.partial");
        crashed_partial(&misnamed, 1, 48_000, &[0, 0]);
        assert_eq!(
            repair_open_partial(&misnamed).unwrap_err().code,
            crate::captures::CaptureErrorCode::ChunkInvalid
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_names_map_to_tracks_and_ordinals() {
        use crate::captures::CaptureTrack;
        assert_eq!(
            parse_partial_name("mic-00000.wav.partial"),
            Some((CaptureTrack::Microphone, 0))
        );
        assert_eq!(
            parse_partial_name("system-00042.wav.partial"),
            Some((CaptureTrack::System, 42))
        );
        for rejected in [
            "mic-0000.wav.partial",
            "mic-000000.wav.partial",
            "mic-0000a.wav.partial",
            "mic-00000.wav",
            "other-00000.wav.partial",
            "../mic-00000.wav.partial",
        ] {
            assert_eq!(parse_partial_name(rejected), None, "{rejected}");
        }
    }

    #[test]
    fn manifest_chunks_record_the_merged_pair_range() {
        let mut manifest = Manifest::empty(7, 1);
        push_manifest_chunk(
            &mut manifest,
            1,
            Some((PathBuf::from("mic-00001.wav"), 300_000, 320_950)),
            Some((PathBuf::from("system-00001.wav"), 300_000, 320_906)),
        );
        assert_eq!(manifest.chunks[0].start_ms, 300_000);
        assert_eq!(manifest.chunks[0].end_ms, 320_950);
        assert_eq!(manifest.active_duration_ms, 320_950);

        // A track that never opened its side of the chunk still indexes.
        push_manifest_chunk(
            &mut manifest,
            2,
            Some((PathBuf::from("mic-00002.wav"), 320_950, 330_000)),
            None,
        );
        assert_eq!(manifest.chunks[1].system, None);
        assert_eq!(manifest.chunks[1].end_ms, 330_000);
        assert!(valid_chunks(
            &manifest.chunks,
            &manifest.tracks,
            Path::new("/tmp/murmur-one-sided-manifest")
        ));
    }

    #[test]
    fn unknown_version_is_quarantined_for_future_code() {
        let root = root("unknown");
        let directory = root.join("7");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("manifest.json"),
            br#"{
                "version": 99,
                "capture_id": 7,
                "created_at": 1,
                "active_duration_ms": 0,
                "paused_duration_ms": 0,
                "tracks": [],
                "chunks": []
            }"#,
        )
        .unwrap();
        let found = discover(&root).unwrap();
        assert_eq!(
            found[0].status,
            RecoveryCandidateStatus::UnknownManifestVersion
        );
        fs::remove_dir_all(root).unwrap();
    }
}
