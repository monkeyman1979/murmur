//! Model registry + first-run download.
//!
//! Writes to `app_data_dir/models/<filename>` with streaming progress events.
//! Downloads start only from an explicit user action. Once the required model
//! is installed, dictation and meeting transcription run offline.
//!
//! The registry (see `ModelRegistry`) is the single source of truth for
//! every downloadable model. The Tauri commands below default to the M1
//! STT model so existing call sites keep working; callers that need a
//! specific entry go through `model_info_by_id` / `download_by_id` /
//! `reveal_by_id`.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::StreamExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::captures::processing::{HeavyProcessingGate, MeetingProcessingSettingsState};
use crate::stt::{whisper::WhisperEngine, SttEngine};

/// Serializes every operation that can establish, consume, replace, or remove
/// app-managed model bytes. This is process-local; same-account processes are
/// still inside the trusted macOS account boundary documented in TM-09.
static MODEL_LIFECYCLE_MUTEX: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

const DOWNLOAD_REDIRECT_LIMIT: usize = 5;
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Bounds approved-host trickle responses without imposing a short timeout on
/// a legitimate ~488 MB transfer over a slow connection.
const DOWNLOAD_TOTAL_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const MODEL_ORIGIN_HOST: &str = "huggingface.co";
const MODEL_CDN_HOST: &str = "us.aws.cdn.hf.co";

/// Swappable STT engine slot. Built in `lib.rs::setup()` and managed as
/// Tauri state so `set_active_stt` can replace the inner `Arc<dyn SttEngine>`
/// at runtime; the hotkey pipeline reads the current engine on every
/// Release so model swaps take effect immediately — no restart.
#[derive(Clone)]
pub struct ActiveSttEngine {
    pub model_id: String,
    pub engine: Arc<dyn SttEngine>,
}

pub type SttEngineSlot = Arc<RwLock<ActiveSttEngine>>;

/// Stable id for the v0.1 STT model. Multilingual (Whisper `small`,
/// 465 MB) is the only STT model Murmur ships — testing showed it
/// produces noticeably better punctuation + casing than `tiny.en`
/// for English while still running comfortably under 1 s on any
/// Apple-Silicon target, and it covers 99 languages out of the box.
/// One STT means one onboarding card, one Settings row, and zero
/// "which model do I want?" friction.
pub const DEFAULT_STT_MODEL_ID: &str = "whisper-small-multilingual";
/// One downloadable model. Everything the downloader and Settings pane
/// need to describe a model lives here; adding a new model is a matter
/// of adding a `ModelEntry` row rather than editing functions.
#[derive(Debug, Clone, Copy)]
pub struct ModelEntry {
    /// Stable string key. Used by Tauri commands and SPEC references.
    pub id: &'static str,
    /// Human-readable label for Settings UI.
    pub display_name: &'static str,
    /// Hugging Face `<org>/<repo>` path. Combined with `filename` below
    /// to form the canonical download URL.
    pub hf_repo_path: &'static str,
    /// Filename on disk and on the HF `resolve/main/` path.
    pub filename: &'static str,
    /// Approximate display size in bytes.
    pub approx_bytes: u64,
    /// Exact byte count of the pinned artifact. Size is enforced before and
    /// during transfer and again before activation; SHA-256 remains the
    /// cryptographic trust anchor.
    pub expected_bytes: u64,
    /// Known-good SHA-256 (lowercase hex) for post-download verification.
    /// Required for every shipping entry — the pin is the trust anchor
    /// against CDN corruption, supply-chain compromise of the host repo,
    /// and the mirror-cascade path below (no mirror is trusted more than
    /// the primary; the hash is what makes either safe to use).
    ///
    /// `None` is permitted only for non-downloadable development fixtures.
    /// The runtime downloader fails closed without a pin, and registry tests
    /// reject any shipping entry that omits one.
    pub sha256: Option<&'static str>,
    /// Ordered fallback URLs tried after the primary URL fails — HTTP
    /// error, connection reset, timeout, or a SHA-256 mismatch against
    /// the `sha256` pin above. Empty by default. Mirrors must serve
    /// byte-identical content (same SHA) for cascade to succeed: a
    /// re-quant or re-packaged alternative with different bytes will
    /// fail verification and fall through to the next mirror, then
    /// surface a clean "all sources failed" error to the user.
    ///
    /// Adding a mirror is a defensive move, not a quality signal — it
    /// handles primary-host outages, transient regional CDN failures,
    /// and (for self-hosted mirrors in the future) rate limits on
    /// shared CDNs.
    pub mirrors: &'static [&'static str],
}

/// Wire-format of `ModelEntry` for Tauri commands. `ModelEntry` uses
/// `&'static str` so serde serializes cleanly; this owned `String` form
/// is what the frontend receives via `list_models`.
#[derive(Debug, Clone, Serialize)]
pub struct ModelEntryDto {
    pub id: String,
    pub display_name: String,
    pub hf_repo_path: String,
    pub filename: String,
    pub approx_bytes: u64,
    pub sha256: Option<String>,
}

impl From<&ModelEntry> for ModelEntryDto {
    fn from(e: &ModelEntry) -> Self {
        Self {
            id: e.id.to_string(),
            display_name: e.display_name.to_string(),
            hf_repo_path: e.hf_repo_path.to_string(),
            filename: e.filename.to_string(),
            approx_bytes: e.approx_bytes,
            sha256: e.sha256.map(|s| s.to_string()),
        }
    }
}

/// Per-entry bundle returned by `list_models` — one call hydrates the
/// Settings speech-model card without an N-per-row `model_info` round-trip.
#[derive(Debug, Clone, Serialize)]
pub struct ModelListItem {
    pub entry: ModelEntryDto,
    pub info: ModelInfo,
}

impl ModelEntry {
    /// Canonical download URL — the HuggingFace `resolve/main/` path so
    /// LFS-backed files stream through a redirect to the CDN.
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}",
            self.hf_repo_path, self.filename
        )
    }

    /// Full cascade of URLs to try when downloading this entry:
    /// primary first, then each mirror in declared order. Consumed by
    /// the downloader which attempts each in sequence, moving to the
    /// next on any failure (HTTP error, connection reset, SHA mismatch).
    pub fn download_urls(&self) -> Vec<String> {
        let mut urls = Vec::with_capacity(1 + self.mirrors.len());
        urls.push(self.download_url());
        urls.extend(self.mirrors.iter().map(|s| s.to_string()));
        urls
    }
}

/// Static registry of speech-to-text models Murmur knows how to download.
/// New STT entries can be added without changing downloader or Settings code.
pub struct ModelRegistry;

impl ModelRegistry {
    pub const ENTRIES: &'static [ModelEntry] = &[
        ModelEntry {
            id: "whisper-small-multilingual",
            display_name: "Multilingual",
            hf_repo_path: "ggerganov/whisper.cpp",
            filename: "ggml-small.bin",
            // Exact bytes + SHA-256 as served by the ggerganov repo
            // on 2026-04-23. Covers 99 languages via Whisper's own
            // language-detection head; dictation always auto-detects,
            // and Settings lists model languages for reference only.
            // Size chosen as the accuracy inflection point for
            // ES / FR / DE — `tiny` multilingual doubles WER for non-
            // English, `medium` / `large-v3-turbo` trade a lot of
            // latency for marginal accuracy gains.
            approx_bytes: 487_601_967,
            expected_bytes: 487_601_967,
            sha256: Some(
                "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
            ),
            // Revision-pinned mirror on the same host. Defends against
            // the file being replaced in the repo (force-push, silent
            // re-upload, account-level edit) — the pinned commit serves
            // the exact bytes our SHA-256 expects, forever. Does *not*
            // defend against HF-level outages or repo deletion; we'll
            // layer a host-diverse mirror (our own CDN) on top later.
            mirrors: &[
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small.bin",
            ],
        },
    ];

    pub fn get(id: &str) -> Option<&'static ModelEntry> {
        Self::ENTRIES.iter().find(|e| e.id == id)
    }
}

/// Reported back to the frontend so the UI can decide whether to show the
/// download prompt, skip it (env override), or jump straight to the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Absolute path where the app expects to find the model.
    pub path: String,
    /// True when the file exists on disk.
    pub exists: bool,
    /// Bytes on disk if `exists`, else 0.
    pub size: u64,
    /// Unix millis of the model file's last-modified time, or `None`
    /// when the file doesn't exist or its mtime can't be read.
    pub modified: Option<u64>,
    /// True when `$MURMUR_WHISPER_MODEL_PATH` is overriding the default path —
    /// used by the UI to suppress the first-run download prompt for devs.
    pub env_override: bool,
    /// Canonical download URL for the model.
    pub download_url: String,
    /// Approximate download size in bytes (for showing "~75 MB" in the UI
    /// before a real Content-Length is known).
    pub approx_download_bytes: u64,
}

/// Progress payload emitted during a download on the `murmur://model-download`
/// event. In-flight transfers report the registry's exact expected byte size;
/// the terminal event uses `None`. The `model_id` field lets frontend listeners route progress to the right
/// UI slot when multiple speech-to-text models can be downloaded.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done: bool,
}

/// Progress payload emitted during a manual verify pass on the
/// `murmur://model-verify` event. `total` is known upfront (file
/// metadata `len()`) so callers can render a determinate progress bar
/// from the first event. `done=true` fires once after the final hash
/// chunk; callers should clear any in-flight progress UI on that
/// signal. Parallels `DownloadProgress` intentionally so the frontend
/// listener shape is symmetric.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyProgress {
    pub model_id: String,
    pub hashed: u64,
    pub total: u64,
    pub done: bool,
}

/// Explicit, developer-only model override. Override bytes are read-only and
/// intentionally unverified because local model experimentation need not match
/// the shipping registry. No download or delete operation may target it.
pub fn dev_model_override(entry: &ModelEntry) -> Option<PathBuf> {
    if entry.id != DEFAULT_STT_MODEL_ID {
        return None;
    }
    std::env::var(crate::MODEL_PATH_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Path where a registry entry is read. App-data lookup fails closed instead
/// of falling back to the process working directory.
pub fn expected_model_path(app: &AppHandle, entry: &ModelEntry) -> Result<PathBuf, String> {
    if let Some(path) = dev_model_override(entry) {
        return Ok(path);
    }
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("models").join(entry.filename))
        .map_err(|_| "application data directory unavailable".to_string())
}

fn managed_model_path(app: &AppHandle, entry: &ModelEntry) -> Result<PathBuf, String> {
    if dev_model_override(entry).is_some() {
        return Err("developer model overrides are read-only".to_string());
    }
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("models").join(entry.filename))
        .map_err(|_| "application data directory unavailable".to_string())
}

/// Build a `ModelInfo` for an arbitrary registry entry.
pub fn model_info_for(app: &AppHandle, entry: &ModelEntry) -> Result<ModelInfo, String> {
    let path = expected_model_path(app, entry)?;
    let env_override = dev_model_override(entry).is_some();
    let metadata = if env_override {
        // Developer overrides are explicitly outside app-managed storage and
        // retain ordinary read-only path semantics, including local symlinks.
        std::fs::metadata(&path)
    } else {
        std::fs::symlink_metadata(&path)
    };
    let (exists, size, modified) = match metadata {
        Ok(m) => {
            let mtime = m.modified().ok().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as u64)
            });
            (m.is_file(), m.len(), mtime)
        }
        Err(_) => (false, 0, None),
    };
    Ok(ModelInfo {
        path: path.to_string_lossy().into_owned(),
        exists,
        size,
        modified,
        env_override,
        download_url: entry.download_url(),
        approx_download_bytes: entry.approx_bytes,
    })
}

/// Build the `SttEngine` implementation for a given registry entry. A
/// single Whisper engine type covers every STT model we ship today;
/// when a second STT engine earns a shipping slot this grows to a
/// real dispatch on the entry id.
pub fn build_stt_engine_for(
    entry: &ModelEntry,
    path: PathBuf,
    app_managed: bool,
) -> Arc<dyn SttEngine> {
    let integrity = app_managed.then(|| {
        (
            entry.expected_bytes,
            entry.sha256.expect("shipping model missing SHA-256 pin"),
        )
    });
    Arc::new(WhisperEngine::new(path, integrity))
}

const UNKNOWN_MODEL_PUBLIC_ERROR: &str = "That speech model is not available.";
const MODEL_INFO_PUBLIC_ERROR: &str = "Murmur could not read the speech model status.";
const MODEL_REVEAL_PUBLIC_ERROR: &str = "Murmur could not show the model in Finder.";
const MODEL_VERIFY_PUBLIC_ERROR: &str = "Murmur could not verify the speech model.";
const MODEL_DOWNLOAD_PUBLIC_ERROR: &str =
    "The model download failed. Check your connection and available storage, then retry.";
const MODEL_ACTIVATE_PUBLIC_ERROR: &str = "Murmur could not activate the speech model.";
const MODEL_DELETE_PUBLIC_ERROR: &str = "Murmur could not delete the speech model.";

fn public_lookup(id: &str) -> std::result::Result<&'static ModelEntry, String> {
    ModelRegistry::get(id).ok_or_else(|| UNKNOWN_MODEL_PUBLIC_ERROR.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadUrlKind {
    Configured,
    Redirect,
}

/// Pure URL boundary used by both configured-source validation and reqwest's
/// redirect callback. Hugging Face currently signs its CDN redirect using a
/// query string, so that one exact redirect host is the sole query exception.
fn validate_download_url(url: &reqwest::Url, kind: DownloadUrlKind) -> Result<(), &'static str> {
    if url.scheme() != "https" {
        return Err("download URL must use HTTPS");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("download URL must not contain credentials");
    }
    if url.port().is_some() {
        return Err("download URL must use the default port");
    }
    if url.fragment().is_some() {
        return Err("download URL must not contain a fragment");
    }
    let host = url.host_str().ok_or("download URL must have a host")?;
    if host != MODEL_ORIGIN_HOST && host != MODEL_CDN_HOST {
        return Err("download URL host is not approved");
    }
    if url.query().is_some() && !(kind == DownloadUrlKind::Redirect && host == MODEL_CDN_HOST) {
        return Err("download URL query is not approved");
    }
    Ok(())
}

fn validate_redirect_hop(
    previous: &reqwest::Url,
    next: &reqwest::Url,
    previous_count: usize,
) -> Result<(), &'static str> {
    if previous_count > DOWNLOAD_REDIRECT_LIMIT {
        return Err("too many model download redirects");
    }
    validate_download_url(next, DownloadUrlKind::Redirect)?;
    let from = previous.host_str().ok_or("redirect source has no host")?;
    let to = next.host_str().ok_or("redirect destination has no host")?;
    if from == to || (from == MODEL_ORIGIN_HOST && to == MODEL_CDN_HOST) {
        Ok(())
    } else {
        Err("model download redirect edge is not approved")
    }
}

fn parse_content_range(value: &str) -> Result<(u64, u64, u64), &'static str> {
    let value = value
        .strip_prefix("bytes ")
        .ok_or("Content-Range unit is invalid")?;
    let (range, total) = value.split_once('/').ok_or("Content-Range is malformed")?;
    let (start, end) = range
        .split_once('-')
        .ok_or("Content-Range bounds are malformed")?;
    let start = start
        .parse()
        .map_err(|_| "Content-Range start is invalid")?;
    let end = end.parse().map_err(|_| "Content-Range end is invalid")?;
    let total = total
        .parse()
        .map_err(|_| "Content-Range total is invalid")?;
    if start > end || end >= total {
        return Err("Content-Range bounds are inconsistent");
    }
    Ok((start, end, total))
}

fn validate_response_layout(
    status: reqwest::StatusCode,
    resume_from: u64,
    expected_bytes: u64,
    content_length: Option<u64>,
    content_range: Option<&str>,
) -> Result<bool, &'static str> {
    use reqwest::StatusCode;
    if status != StatusCode::OK && status != StatusCode::PARTIAL_CONTENT {
        return Err("model server returned an invalid status");
    }
    if status == StatusCode::PARTIAL_CONTENT {
        if resume_from == 0 {
            return Err("unexpected partial model response");
        }
        let (start, end, total) =
            parse_content_range(content_range.ok_or("model Content-Range is missing")?)?;
        let expected_remaining = expected_bytes
            .checked_sub(resume_from)
            .ok_or("model resume offset is oversized")?;
        let expected_end = expected_bytes
            .checked_sub(1)
            .ok_or("model registry size is invalid")?;
        if start != resume_from
            || end != expected_end
            || total != expected_bytes
            || content_length.is_some_and(|length| length != expected_remaining)
        {
            return Err("model Content-Range does not match the registry");
        }
        Ok(false)
    } else {
        let content_length = content_length.ok_or("model response Content-Length is missing")?;
        if content_range.is_some() {
            return Err("unexpected Content-Range on full model response");
        }
        if content_length != expected_bytes {
            return Err("model Content-Length does not match the registry");
        }
        Ok(resume_from > 0)
    }
}

fn checked_download_total(
    downloaded: u64,
    chunk_bytes: usize,
    expected_bytes: u64,
) -> Result<u64, &'static str> {
    let chunk_bytes = u64::try_from(chunk_bytes).map_err(|_| "model streamed size overflow")?;
    let total = downloaded
        .checked_add(chunk_bytes)
        .ok_or("model streamed size overflow")?;
    if total > expected_bytes {
        return Err("model response exceeded the registry size");
    }
    Ok(total)
}

fn invalid_response_failure(status: reqwest::StatusCode, message: &'static str) -> DownloadFailure {
    use reqwest::StatusCode;
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        DownloadFailure::fatal("model range was not satisfiable")
    } else if status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        )
    {
        DownloadFailure::transient(message)
    } else {
        DownloadFailure::fatal(message)
    }
}

fn validate_and_parse_response_headers(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> std::result::Result<(Option<u64>, Option<&str>), DownloadFailure> {
    use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE};
    use reqwest::StatusCode;

    // Classify non-body statuses before parsing representation headers. A
    // malformed header on a transient 408/429/5xx response must not convert
    // that response into a fatal failure that deletes a valid partial.
    if status != StatusCode::OK && status != StatusCode::PARTIAL_CONTENT {
        return Err(invalid_response_failure(
            status,
            "model server returned unexpected response",
        ));
    }

    let content_length = match headers.get(CONTENT_LENGTH) {
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| DownloadFailure::fatal("model Content-Length is invalid"))?,
        ),
        None => None,
    };
    let content_range = match headers.get(CONTENT_RANGE) {
        Some(value) => Some(
            value
                .to_str()
                .map_err(|_| DownloadFailure::fatal("model Content-Range is invalid"))?,
        ),
        None => None,
    };
    Ok((content_length, content_range))
}

fn model_http_client() -> Result<reqwest::Client, DownloadFailure> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let previous = attempt.previous();
        let Some(from) = previous.last() else {
            return attempt.error("redirect source missing");
        };
        match validate_redirect_hop(from, attempt.url(), previous.len()) {
            Ok(()) => attempt.follow(),
            Err(reason) => attempt.error(reason),
        }
    });
    reqwest::Client::builder()
        .use_rustls_tls()
        .https_only(true)
        .referer(false)
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .read_timeout(DOWNLOAD_READ_TIMEOUT)
        .timeout(DOWNLOAD_TOTAL_TIMEOUT)
        .redirect(policy)
        .build()
        .map_err(|_| DownloadFailure::transient("HTTP client initialization failed"))
}

fn ensure_private_models_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other("models directory is not a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
fn open_model_no_follow(path: &Path, write: bool, create: bool) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    // NONBLOCK prevents a same-account FIFO replacement from wedging the
    // lifecycle lock before descriptor metadata can reject the special file.
    // It has no effect on ordinary regular-file reads or writes.
    let mut flags = OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    flags |= if write { OFlags::RDWR } else { OFlags::RDONLY };
    if create {
        flags |= OFlags::CREATE;
    }
    let descriptor =
        rustix::fs::open(path, flags, Mode::from_raw_mode(0o600)).map_err(io::Error::from)?;
    Ok(descriptor.into())
}

#[cfg(not(unix))]
fn open_model_no_follow(path: &Path, write: bool, create: bool) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(write).create(create);
    options.open(path)
}

fn validate_model_descriptor(file: &File, expected_max: u64) -> io::Result<u64> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > expected_max {
        return Err(io::Error::other("model file is not a bounded regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(io::Error::other("model file must have one link"));
        }
    }
    Ok(metadata.len())
}

fn harden_model_file_permissions(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn descriptor_matches_path(file: &File, path: &Path) -> io::Result<bool> {
    let path_metadata = std::fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Ok(false);
    }
    let open_metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(open_metadata.dev() == path_metadata.dev()
            && open_metadata.ino() == path_metadata.ino()
            && open_metadata.nlink() == 1)
    }
    #[cfg(not(unix))]
    Ok(open_metadata.len() == path_metadata.len())
}

fn hash_descriptor_sha256(
    file: &mut File,
    mut on_progress: impl FnMut(u64),
) -> io::Result<(sha2::Sha256, u64)> {
    use sha2::{Digest, Sha256};
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("model size overflow"))?;
        on_progress(total);
    }
    Ok((hasher, total))
}

/// Descriptor-backed exact-size and SHA check used immediately before every
/// app-managed Whisper parse/reload. On Unix the returned descriptor can be
/// exposed through `/dev/fd`, preventing a pathname replacement from changing
/// what Whisper parses after verification.
pub(crate) fn open_verified_model(
    path: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
) -> Result<File, String> {
    use sha2::Digest;
    let parent = path.parent().ok_or("model parent missing")?;
    ensure_private_models_dir(parent).map_err(|_| "model directory invalid")?;
    let mut file = open_model_no_follow(path, false, false).map_err(|_| "model open failed")?;
    let size = validate_model_descriptor(&file, expected_bytes).map_err(|_| "model invalid")?;
    harden_model_file_permissions(&file).map_err(|_| "model permissions invalid")?;
    if size != expected_bytes {
        return Err("model size mismatch".to_string());
    }
    let (hasher, bytes) =
        hash_descriptor_sha256(&mut file, |_| {}).map_err(|_| "model read failed")?;
    if bytes != expected_bytes
        || !hex_encode(&hasher.finalize()).eq_ignore_ascii_case(expected_sha256)
    {
        return Err("model integrity mismatch".to_string());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "model seek failed")?;
    Ok(file)
}

#[tauri::command]
pub fn model_info(app: AppHandle, id: String) -> std::result::Result<ModelInfo, String> {
    let entry = public_lookup(&id)?;
    model_info_for(&app, entry).map_err(|_| MODEL_INFO_PUBLIC_ERROR.to_string())
}

/// Open Finder focused on the model file. Used by the "Show in Finder"
/// button on the Settings speech-model card. `open -R` reveals the file
/// regardless of whether it currently exists — if the path is missing
/// Finder opens the parent directory.
pub fn reveal_in_finder(app: &AppHandle, entry: &ModelEntry) -> Result<(), String> {
    let path = expected_model_path(app, entry)?;
    std::process::Command::new("open")
        .arg("-R")
        .arg(&path)
        .spawn()
        .map_err(|_| "Finder reveal command failed".to_string())?;
    Ok(())
}

#[tauri::command]
pub fn reveal_model_in_finder(app: AppHandle, id: String) -> Result<(), String> {
    let entry = public_lookup(&id)?;
    reveal_in_finder(&app, entry).map_err(|_| MODEL_REVEAL_PUBLIC_ERROR.to_string())
}

/// Outcome of a manual verification pass against the on-disk bytes.
/// `ok` is `true` when `actual == expected` (case-insensitive); the
/// full hashes are returned either way so the UI can show them in a
/// "looks wrong — re-download?" flow without re-hashing.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub expected: String,
    pub actual: String,
    pub bytes_hashed: u64,
}

/// Streaming SHA-256 of an on-disk file. Separate from the
/// download-time hash in `stream_to_path` so the verify-after-the-fact
/// flow can re-check an installed model without re-fetching it.
///
/// Reads the retained no-follow descriptor in 64 KiB chunks and feeds them
/// into `Sha256::update`. With `sha2`'s `asm` feature enabled, Apple
/// Silicon hits hardware SHA-256 extensions — a 2.5 GB model
/// typically hashes in 2-5 seconds in a release build. Caller must
/// run this on a blocking thread (see `verify_model`).
///
/// `on_progress` is called after each chunk with the running
/// byte-count; caller owns any throttling (emitting an event every
/// byte would flood the bridge). A no-op closure is fine for tests.
fn hash_file_sha256(
    path: &Path,
    on_progress: impl FnMut(u64),
) -> std::result::Result<(String, u64), String> {
    use sha2::Digest;
    let mut file = open_model_no_follow(path, false, false).map_err(|_| "model open failed")?;
    validate_model_descriptor(&file, u64::MAX).map_err(|_| "model descriptor invalid")?;
    let (hasher, total) =
        hash_descriptor_sha256(&mut file, on_progress).map_err(|_| "model read failed")?;
    Ok((hex_encode(&hasher.finalize()), total))
}

/// Verify arbitrary local bytes against a registry entry's shipping pin.
///
/// Standalone developer benchmarks reuse this rather than carrying a second
/// SHA-256 implementation. Callers handling private paths must map the returned
/// I/O error to a content-safe message instead of printing it directly.
pub fn model_file_matches_registry_pin(
    path: &std::path::Path,
    entry: &ModelEntry,
) -> std::result::Result<bool, String> {
    let expected = entry
        .sha256
        .ok_or_else(|| format!("entry `{}` has no sha256 pin", entry.id))?;
    let (actual, size) = hash_file_sha256(path, |_| {})?;
    Ok(size == entry.expected_bytes && actual.eq_ignore_ascii_case(expected))
}

/// Re-hash an installed model against its registry SHA-256 pin. The
/// download path already verifies on the way in; this command covers
/// the "I've had this file for a while, is it still intact?" case —
/// silent bit-rot or accidental edits would otherwise degrade
/// transcription quality with no user-visible signal. Re-downloading
/// resolves any mismatch.
///
/// Runs on a blocking thread since `sha2` is CPU-bound and a 2.5 GB
/// file would park a tokio worker for seconds.
#[tauri::command]
pub async fn verify_model(app: AppHandle, id: String) -> std::result::Result<VerifyReport, String> {
    let entry = public_lookup(&id)?;
    if dev_model_override(entry).is_some() {
        return Err(MODEL_VERIFY_PUBLIC_ERROR.to_string());
    }
    let _lifecycle_guard = MODEL_LIFECYCLE_MUTEX
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let expected = entry
        .sha256
        .ok_or_else(|| MODEL_VERIFY_PUBLIC_ERROR.to_string())?
        .to_string();
    let path =
        managed_model_path(&app, entry).map_err(|_| MODEL_VERIFY_PUBLIC_ERROR.to_string())?;
    ensure_private_models_dir(path.parent().ok_or(MODEL_VERIFY_PUBLIC_ERROR)?)
        .map_err(|_| MODEL_VERIFY_PUBLIC_ERROR.to_string())?;
    let file = open_model_no_follow(&path, false, false)
        .map_err(|_| MODEL_VERIFY_PUBLIC_ERROR.to_string())?;
    let total_bytes = validate_model_descriptor(&file, entry.expected_bytes)
        .map_err(|_| MODEL_VERIFY_PUBLIC_ERROR.to_string())?;
    harden_model_file_permissions(&file).map_err(|_| MODEL_VERIFY_PUBLIC_ERROR.to_string())?;
    if total_bytes != entry.expected_bytes {
        return Err(MODEL_VERIFY_PUBLIC_ERROR.to_string());
    }
    let display_name = entry.display_name.to_string();

    // Emit a zeroed event before the hash starts so the row renders a
    // determinate progress bar from t=0 rather than waiting for the
    // first 8 MB worth of reads before any progress is visible.
    crate::window_events::emit_main(
        &app,
        "murmur://model-verify",
        VerifyProgress {
            model_id: id.clone(),
            hashed: 0,
            total: total_bytes,
            done: false,
        },
    );

    let app_for_progress = app.clone();
    let id_for_progress = id.clone();
    let (actual, bytes_hashed) = tokio::task::spawn_blocking(move || {
        use sha2::Digest;
        // Event every 8 MiB. Tight enough to feel smooth on the progress
        // bar, loose enough to stay out of the IPC fast path.
        const EMIT_EVERY_BYTES: u64 = 8 * 1024 * 1024;
        let mut last_emit: u64 = 0;
        let mut file = file;
        let (hasher, bytes) = hash_descriptor_sha256(&mut file, |hashed| {
            if hashed - last_emit >= EMIT_EVERY_BYTES {
                last_emit = hashed;
                crate::window_events::emit_main(
                    &app_for_progress,
                    "murmur://model-verify",
                    VerifyProgress {
                        model_id: id_for_progress.clone(),
                        hashed,
                        total: total_bytes,
                        done: false,
                    },
                );
            }
        })
        .map_err(|_| "model read failed".to_string())?;
        Ok::<_, String>((hex_encode(&hasher.finalize()), bytes))
    })
    .await
    .map_err(|_| MODEL_VERIFY_PUBLIC_ERROR.to_string())?
    .map_err(|_| MODEL_VERIFY_PUBLIC_ERROR.to_string())?;

    // Final progress event — listeners clear their per-id progress ref
    // on `done=true` so the row cleanly releases the "Verifying…"
    // subtitle slot before the inline ✓ (or red mismatch) takes over.
    crate::window_events::emit_main(
        &app,
        "murmur://model-verify",
        VerifyProgress {
            model_id: id.clone(),
            hashed: bytes_hashed,
            total: total_bytes,
            done: true,
        },
    );

    let ok = actual.eq_ignore_ascii_case(&expected);

    // Success feedback is an inline "Verified ✓" subtitle on the row
    // — native notifications were getting swallowed by Focus mode,
    // Notification Center silence, and generally missed mid-task.
    // Failures still fire a native notification since the user needs
    // to be pulled back to the app to re-download; a silent-fail row
    // could go unnoticed until transcription quality degrades.
    if !ok {
        crate::notify::error(
            &app,
            &format!("{display_name} failed verification"),
            "Bytes no longer match the registry pin. Re-download to restore.",
        );
    }

    Ok(VerifyReport {
        ok,
        expected,
        actual,
        bytes_hashed,
    })
}

#[tauri::command]
pub async fn download_model(app: AppHandle, id: String) -> Result<(), String> {
    let entry = public_lookup(&id)?;
    let target =
        managed_model_path(&app, entry).map_err(|_| MODEL_DOWNLOAD_PUBLIC_ERROR.to_string())?;
    let target_existed_before_wait = target.exists();

    // Only one model lifecycle operation may inspect or mutate model bytes. A
    // duplicate request for the same missing model used to start a second
    // stream against the same temp path, making progress events race
    // backward and ending with the loser failing to rename a `.part` file
    // the winner had already moved. If another request finished this
    // missing target while we were queued, treat this call as successful.
    let _lifecycle_guard = MODEL_LIFECYCLE_MUTEX
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    if !target_existed_before_wait && target.exists() {
        let pin = entry
            .sha256
            .ok_or_else(|| MODEL_DOWNLOAD_PUBLIC_ERROR.to_string())?;
        if open_verified_model(&target, entry.expected_bytes, pin).is_ok() {
            return Ok(());
        }
    }

    download_file(&app, entry, &target)
        .await
        .map_err(|_| MODEL_DOWNLOAD_PUBLIC_ERROR.to_string())?;
    Ok(())
}

/// Outcome of a failed `stream_to_path` attempt. The caller's cascade
/// logic branches on `is_fatal` to decide what to do with the
/// partial `.part` file:
///
/// - **Transient** (`is_fatal: false`) — network or HTTP request failure
///   (including a rejected redirect), write I/O error, HTTP 408/429/5xx,
///   etc. The bytes already on disk are presumed valid (the hasher matched
///   them byte-for-byte while they streamed in). Keep the `.part` so the
///   next cascade attempt resumes from the partial offset via `Range:`
///   rather than re-fetching from zero.
/// - **Fatal** (`is_fatal: true`) — configured-URL, response-header,
///   size/SHA, or local file-identity validation failed. The partial is not safe to resume;
///   wipe it so a later approved source starts from zero.
#[derive(Debug)]
struct DownloadFailure {
    message: String,
    is_fatal: bool,
}

impl DownloadFailure {
    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            is_fatal: false,
        }
    }

    fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            is_fatal: true,
        }
    }
}

/// Rebuild a running SHA-256 state from the bytes already on disk.
/// Used by the resume path in `stream_to_path` — `sha2` doesn't
/// serialize its internal state, so the only way to continue the
/// running hash after a crash / network drop is to re-read the
/// existing `.part` and feed it to a fresh `Sha256::update` loop.
///
/// Runs from `spawn_blocking`; a large model file can take time to rehash
/// or two on Apple Silicon (same path as `verify_model`, benefits
/// from the same `sha2` `asm` feature). Shares no code with
/// `hash_file_sha256` because that helper finalizes the digest;
/// we need the `Sha256` instance still open so we can keep feeding
/// the incoming network bytes into it.
fn rebuild_hasher_from_file(file: &mut File) -> std::result::Result<(sha2::Sha256, u64), String> {
    hash_descriptor_sha256(file, |_| {}).map_err(|_| "partial rehash failed".to_string())
}

/// Streaming download to a `.part` sibling with atomic rename on
/// success. Resumable across retries: a surviving `.part` from a
/// prior interrupted attempt is rehashed and the next request is
/// sent with `Range: bytes=<size>-` to continue from the partial
/// offset.
///
/// Cascades through the registry entry's full URL list — primary
/// first, then each mirror. Transient failures (network drops,
/// HTTP 5xx) leave the `.part` in place so the next URL resumes.
/// Fatal failures (hash mismatch, 416 Range Not Satisfiable) wipe
/// it first so the retry starts fresh. If every URL fails, surfaces
/// the last error.
async fn download_file(
    app: &AppHandle,
    entry: &ModelEntry,
    target: &std::path::Path,
) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| "model target has no parent".to_string())?;
    ensure_private_models_dir(parent).map_err(|_| "models directory unavailable".to_string())?;
    let tmp = target.with_extension("bin.part");
    let urls = entry.download_urls();
    let total_urls = urls.len();
    let mut last_err: Option<String> = None;
    for (idx, url) in urls.iter().enumerate() {
        match stream_to_path(app, entry.id, url, &tmp, entry.expected_bytes, entry.sha256).await {
            Ok(()) => {
                match std::fs::symlink_metadata(target) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                        return Err("model destination is not a regular file".to_string());
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => return Err("model destination inspection failed".to_string()),
                }
                std::fs::rename(&tmp, target).map_err(|_| "model promotion failed".to_string())?;
                #[cfg(unix)]
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|_| "model directory synchronization failed".to_string())?;
                emit_done(app, entry.id);
                // Transient macOS notification so the user sees a
                // "ready to use" confirmation once the progress bar
                // disappears. Silent beyond the log on every failure
                // path of `notify::info`; a dropped notification
                // never breaks the download.
                crate::notify::info(
                    app,
                    &format!("{} installed", entry.display_name),
                    "Ready to use — select it in Settings → Speech model.",
                );
                return Ok(());
            }
            Err(failure) => {
                crate::murmur_dev_log!(
                    "[murmur] model download source {}/{} failed for {}: {} (fatal={})",
                    idx + 1,
                    total_urls,
                    entry.id,
                    failure.message,
                    failure.is_fatal,
                );
                if failure.is_fatal {
                    // Policy/integrity validation says these bytes are not
                    // safe to resume; wipe so the next URL starts fresh.
                    let _ = tokio::fs::remove_file(&tmp).await;
                }
                // Transient failures intentionally leave the `.part`
                // in place; the next cascade attempt will pick it up
                // and resume via `Range:`.
                last_err = Some(failure.message);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "no download URLs configured".to_string()))
}

/// Shared streaming-download body — writes bytes to `target_path`,
/// emits periodic `murmur://model-download` progress events, and
/// computes a streaming SHA-256 as bytes arrive so the post-download
/// verification step can hash-compare against `expected_sha256`
/// without a second pass over the file. Does not rename; the caller
/// owns post-download promotion (atomic rename).
///
/// Resume: if `target_path` already exists from a prior interrupted
/// attempt, rehashes the existing bytes, retains the read/write descriptor
/// positioned at its end, and sends `Range: bytes=<size>-` so the server
/// continues the transfer from the partial offset. Three response-code paths:
/// - `206 Partial Content`: append stream to existing `.part`.
/// - `200 OK` after we sent a `Range:` header: server ignored the
///   range. Truncate our `.part`, reset the hasher, download fresh.
/// - `416 Range Not Satisfiable`: `.part` is past the remote file's
///   end. Fatal failure — caller wipes the `.part` and retries.
///
/// SHA behavior:
/// - `Some(pin)` and bytes verify: returns `Ok(())`. Caller proceeds.
/// - `Some(pin)` and bytes don't verify: fatal failure. Caller wipes
///   the `.part` and moves to the next URL in the cascade.
/// - `None` (pin absent): fatal registry failure. Developer overrides are
///   read-only and never enter the downloader.
async fn stream_to_path(
    app: &AppHandle,
    model_id: &str,
    url: &str,
    target_path: &Path,
    expected_bytes: u64,
    expected_sha256: Option<&str>,
) -> std::result::Result<(), DownloadFailure> {
    use reqwest::header::RANGE;
    use sha2::{Digest, Sha256};

    let configured_url = reqwest::Url::parse(url)
        .map_err(|_| DownloadFailure::fatal("configured model URL is invalid"))?;
    validate_download_url(&configured_url, DownloadUrlKind::Configured)
        .map_err(DownloadFailure::fatal)?;

    // Open the deterministic partial once with no-follow semantics and keep
    // this descriptor through rehash, append/truncate, sync, and the final
    // descriptor/path identity check.
    let file = open_model_no_follow(target_path, true, true)
        .map_err(|_| DownloadFailure::fatal("model partial could not be opened safely"))?;
    let partial_size = validate_model_descriptor(&file, expected_bytes)
        .map_err(|_| DownloadFailure::fatal("model partial is invalid or oversized"))?;
    harden_model_file_permissions(&file)
        .map_err(|_| DownloadFailure::fatal("model partial permissions could not be hardened"))?;
    let (file, mut hasher, mut downloaded) = tokio::task::spawn_blocking(move || {
        let mut file = file;
        let (hasher, size) = rebuild_hasher_from_file(&mut file)?;
        file.seek(SeekFrom::End(0))
            .map_err(|_| "partial seek failed".to_string())?;
        Ok::<_, String>((file, hasher, size))
    })
    .await
    .map_err(|_| DownloadFailure::transient("partial rehash task failed"))?
    .map_err(DownloadFailure::fatal)?;
    if downloaded != partial_size {
        return Err(DownloadFailure::fatal(
            "model partial changed while rehashing",
        ));
    }
    let resume_from = downloaded;

    // A complete surviving partial needs no network request, but still passes
    // both exact-size and SHA gates below before promotion.
    let client = model_http_client()?;
    let mut req = client.get(configured_url);
    if resume_from > 0 {
        req = req.header(RANGE, format!("bytes={resume_from}-"));
    }
    let resp = if resume_from == expected_bytes {
        None
    } else {
        Some(
            req.send()
                .await
                .map_err(|_| DownloadFailure::transient("model HTTP request failed"))?,
        )
    };

    let mut file = tokio::fs::File::from_std(file);
    let Some(resp) = resp else {
        file.sync_all()
            .await
            .map_err(|_| DownloadFailure::transient("model partial sync failed"))?;
        let file = file.into_std().await;
        if !descriptor_matches_path(&file, target_path)
            .map_err(|_| DownloadFailure::fatal("model partial identity check failed"))?
        {
            return Err(DownloadFailure::fatal("model partial path was replaced"));
        }
        let actual_sha = hex_encode(&hasher.finalize());
        return match expected_sha256 {
            Some(expected) if actual_sha.eq_ignore_ascii_case(expected) => Ok(()),
            _ => Err(DownloadFailure::fatal("model SHA-256 mismatch")),
        };
    };

    let status = resp.status();
    let (content_length, content_range) =
        validate_and_parse_response_headers(status, resp.headers())?;
    let restart_from_zero = validate_response_layout(
        status,
        resume_from,
        expected_bytes,
        content_length,
        content_range,
    )
    .map_err(|message| invalid_response_failure(status, message))?;
    if restart_from_zero {
        file.set_len(0)
            .await
            .map_err(|_| DownloadFailure::fatal("model partial truncate failed"))?;
        file.seek(SeekFrom::Start(0))
            .await
            .map_err(|_| DownloadFailure::fatal("model partial seek failed"))?;
        hasher = Sha256::new();
        downloaded = 0;
    }

    let total = Some(expected_bytes);

    // Seed the frontend progress bar with the resume offset so it
    // doesn't snap back to 0 at the start of each cascade attempt.
    crate::window_events::emit_main(
        app,
        "murmur://model-download",
        DownloadProgress {
            model_id: model_id.to_string(),
            downloaded,
            total,
            done: false,
        },
    );

    let mut last_emit: u64 = downloaded;
    const EMIT_EVERY_BYTES: u64 = 256 * 1024;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => {
                let _ = file.sync_data().await;
                return Err(DownloadFailure::transient("model response stream failed"));
            }
        };
        let next_total = checked_download_total(downloaded, chunk.len(), expected_bytes)
            .map_err(DownloadFailure::fatal)?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|_| DownloadFailure::transient("model partial write failed"))?;
        downloaded = next_total;
        if downloaded - last_emit >= EMIT_EVERY_BYTES {
            last_emit = downloaded;
            crate::window_events::emit_main(
                app,
                "murmur://model-download",
                DownloadProgress {
                    model_id: model_id.to_string(),
                    downloaded,
                    total,
                    done: false,
                },
            );
        }
    }
    if downloaded != expected_bytes {
        let _ = file.sync_data().await;
        return Err(DownloadFailure::transient(
            "model response ended before the registry size",
        ));
    }
    file.flush()
        .await
        .map_err(|_| DownloadFailure::transient("model partial flush failed"))?;
    file.sync_all()
        .await
        .map_err(|_| DownloadFailure::transient("model partial sync failed"))?;
    let file = file.into_std().await;
    if validate_model_descriptor(&file, expected_bytes)
        .map_err(|_| DownloadFailure::fatal("model partial descriptor changed"))?
        != expected_bytes
        || !descriptor_matches_path(&file, target_path)
            .map_err(|_| DownloadFailure::fatal("model partial identity check failed"))?
    {
        return Err(DownloadFailure::fatal("model partial path was replaced"));
    }

    let actual_sha = hex_encode(&hasher.finalize());
    match expected_sha256 {
        Some(expected) if actual_sha.eq_ignore_ascii_case(expected) => Ok(()),
        Some(_) => Err(DownloadFailure::fatal("model SHA-256 mismatch")),
        None => Err(DownloadFailure::fatal("model registry pin is missing")),
    }
}

/// Lowercase hex encoding for a 32-byte SHA-256 digest. Small enough
/// to inline; avoids pulling `hex` as a direct dep.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn emit_done(app: &AppHandle, model_id: &str) {
    crate::window_events::emit_main(
        app,
        "murmur://model-download",
        DownloadProgress {
            model_id: model_id.to_string(),
            downloaded: 0,
            total: None,
            done: true,
        },
    );
}

/// Enumerate every registry entry with its current on-disk status.
/// Single round-trip replacement for N `model_info(id)` calls; used by
/// the Settings speech-model card to render every speech-to-text entry.
#[tauri::command]
pub fn list_models(app: AppHandle) -> Result<Vec<ModelListItem>, String> {
    ModelRegistry::ENTRIES
        .iter()
        .map(|entry| {
            Ok(ModelListItem {
                entry: ModelEntryDto::from(entry),
                info: model_info_for(&app, entry)
                    .map_err(|_| MODEL_INFO_PUBLIC_ERROR.to_string())?,
            })
        })
        .collect()
}

/// Shared active-model state. Frontend is the source of truth
/// (`localStorage["murmur:active-stt"]`); this mirror exists so the hotkey
/// pipeline can read the active STT synchronously without the webview.
pub struct ActiveModelsState {
    /// Always `Some` — STT is mandatory. Defaults to
    /// `DEFAULT_STT_MODEL_ID` until the frontend composable boots and
    /// calls `set_active_stt` with the persisted value.
    pub active_stt: RwLock<String>,
}

impl ActiveModelsState {
    pub fn new() -> Self {
        Self {
            active_stt: RwLock::new(DEFAULT_STT_MODEL_ID.to_string()),
        }
    }

    pub fn active_stt_id(&self) -> String {
        self.active_stt.read().clone()
    }
}

impl Default for ActiveModelsState {
    fn default() -> Self {
        Self::new()
    }
}

fn model_selection_requires_swap(current_model_id: &str, requested_model_id: &str) -> bool {
    current_model_id != requested_model_id
}

/// Set the active STT model, validating that the entry is in the
/// registry, the file is on disk, and swapping the live engine slot so
/// the next hotkey transcription runs through the new model. Errors are
/// surfaced to the frontend so the UI can keep the radio selection in
/// sync with what actually succeeded.
#[tauri::command]
pub async fn set_active_stt(
    app: AppHandle,
    active: tauri::State<'_, Arc<ActiveModelsState>>,
    slot: tauri::State<'_, SttEngineSlot>,
    gate: tauri::State<'_, HeavyProcessingGate>,
    meeting_settings: tauri::State<'_, Arc<MeetingProcessingSettingsState>>,
    model_id: String,
) -> Result<(), String> {
    let entry = public_lookup(&model_id)?;
    let _lifecycle_guard = MODEL_LIFECYCLE_MUTEX
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let dev_override = dev_model_override(entry).is_some();
    let path =
        expected_model_path(&app, entry).map_err(|_| MODEL_ACTIVATE_PUBLIC_ERROR.to_string())?;
    if !path.exists() {
        return Err("The speech model is missing. Download it before activating.".to_string());
    }

    // Root frontend hydration re-sends the boot-selected id on every launch.
    // The initial slot already points at that model, so synchronize the state
    // mirrors without hashing/parsing and caching ~488 MB at boot. Integrity
    // remains mandatory immediately before the first lazy load. Actual model
    // changes continue through full pre-swap validation below.
    if !model_selection_requires_swap(&slot.read().model_id, &model_id) {
        *active.active_stt.write() = model_id.clone();
        meeting_settings.set_model(model_id);
        return Ok(());
    }

    let integrity = (!dev_override).then(|| {
        (
            entry.expected_bytes,
            entry.sha256.expect("shipping model missing SHA-256 pin"),
        )
    });
    let new_engine = Arc::new(WhisperEngine::new(path, integrity));
    let _permit = Arc::clone(gate.inner())
        .acquire_owned()
        .await
        .map_err(|_| MODEL_ACTIVATE_PUBLIC_ERROR.to_string())?;
    let engine_to_validate = Arc::clone(&new_engine);
    tokio::task::spawn_blocking(move || engine_to_validate.validate_for_activation())
        .await
        .map_err(|_| MODEL_ACTIVATE_PUBLIC_ERROR.to_string())?
        .map_err(|_| MODEL_ACTIVATE_PUBLIC_ERROR.to_string())?;

    // Validation/loading succeeded before touching old state. A failed
    // activation therefore leaves the current engine loaded and selected.
    let old_engine = slot.read().engine.clone();
    old_engine.unload().await;
    {
        let mut guard = slot.write();
        *guard = ActiveSttEngine {
            model_id: model_id.clone(),
            engine: new_engine,
        };
    }
    *active.active_stt.write() = model_id.clone();
    meeting_settings.set_model(model_id.clone());
    crate::murmur_dev_log!("[murmur] active STT swapped to {model_id}");
    Ok(())
}

/// Remove a speech-to-text model file from disk. Guard rails refuse to
/// delete the active model or the last downloaded model.
#[tauri::command]
pub async fn delete_model(
    app: AppHandle,
    active: tauri::State<'_, Arc<ActiveModelsState>>,
    model_id: String,
) -> Result<(), String> {
    let entry = public_lookup(&model_id)?;
    let path =
        managed_model_path(&app, entry).map_err(|_| MODEL_DELETE_PUBLIC_ERROR.to_string())?;
    let _lifecycle_guard = MODEL_LIFECYCLE_MUTEX
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    ensure_private_models_dir(path.parent().ok_or(MODEL_DELETE_PUBLIC_ERROR)?)
        .map_err(|_| MODEL_DELETE_PUBLIC_ERROR.to_string())?;

    if active.active_stt_id() == model_id {
        return Err("cannot delete the active speech-to-text model".into());
    }
    let downloaded_stt_count = ModelRegistry::ENTRIES
        .iter()
        .filter_map(|candidate| managed_model_path(&app, candidate).ok())
        .filter(|candidate| candidate.exists())
        .count();
    if downloaded_stt_count <= 1 {
        return Err("cannot delete the last downloaded speech-to-text model".into());
    }

    let file = match open_model_no_follow(&path, false, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(MODEL_DELETE_PUBLIC_ERROR.to_string()),
    };
    validate_model_descriptor(&file, u64::MAX)
        .map_err(|_| MODEL_DELETE_PUBLIC_ERROR.to_string())?;
    if !descriptor_matches_path(&file, &path).map_err(|_| MODEL_DELETE_PUBLIC_ERROR.to_string())? {
        return Err(MODEL_DELETE_PUBLIC_ERROR.to_string());
    }
    std::fs::remove_file(&path).map_err(|_| MODEL_DELETE_PUBLIC_ERROR.to_string())?;

    crate::murmur_dev_log!("[murmur] deleted model: {model_id}");
    Ok(())
}

/// Best-effort one-time cleanup of the legacy Parakeet model directory.
/// The Parakeet engine was retired in PR `refactor/drop-parakeet-engine`;
/// users who downloaded it on an earlier build still have the extracted
/// tarball sitting in their app-data directory. Nothing on disk points at
/// it anymore, so it's pure orphan storage. Called once per app boot from
/// `lib.rs::setup`; silent on every failure mode (missing, permission
/// denied, already cleaned).
pub fn cleanup_legacy_parakeet_dir(app: &AppHandle) {
    let Some(data_dir) = app.path().app_data_dir().ok() else {
        return;
    };
    let legacy = data_dir.join("models").join("parakeet-tdt-0.6b-v3-int8");
    if legacy.exists() {
        match std::fs::remove_dir_all(&legacy) {
            Ok(()) => crate::murmur_dev_log!("[murmur] removed legacy Parakeet directory"),
            Err(_) => {
                crate::murmur_dev_log!("[murmur] could not remove legacy Parakeet directory")
            }
        }
    }
    // Also sweep any leftover `.tar.gz` / `.tar.gz.part` / `.extracting`
    // siblings from the old two-phase tarball download.
    for suffix in ["tar.gz", "tar.gz.part", "extracting"] {
        let crumb = data_dir
            .join("models")
            .join(format!("parakeet-tdt-0.6b-v3-int8.{suffix}"));
        if crumb.exists() {
            let _ = if crumb.is_dir() {
                std::fs::remove_dir_all(&crumb)
            } else {
                std::fs::remove_file(&crumb)
            };
        }
    }
}

/// Best-effort cleanup for the exact app-managed model files retired with the
/// local-pipeline simplification. Remove this migration after two stable
/// releases. Never broaden these names or scan arbitrary GGUF files.
pub fn cleanup_retired_model_files(app: &AppHandle) {
    let Some(data_dir) = app.path().app_data_dir().ok() else {
        return;
    };
    let models_dir = data_dir.join("models");
    for filename in [
        "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf.part",
        "Qwen_Qwen3-1.7B-Q4_K_M.gguf",
        "Qwen_Qwen3-1.7B-Q4_K_M.gguf.part",
    ] {
        let path = models_dir.join(filename);
        if !path.exists() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => crate::murmur_dev_log!("[murmur] removed retired model cache: {filename}"),
            Err(_) => {
                crate::murmur_dev_log!("[murmur] could not remove retired model cache: {filename}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn public_model_errors_are_fixed_and_content_free() {
        let messages = [
            UNKNOWN_MODEL_PUBLIC_ERROR,
            MODEL_INFO_PUBLIC_ERROR,
            MODEL_REVEAL_PUBLIC_ERROR,
            MODEL_VERIFY_PUBLIC_ERROR,
            MODEL_DOWNLOAD_PUBLIC_ERROR,
            MODEL_ACTIVATE_PUBLIC_ERROR,
            MODEL_DELETE_PUBLIC_ERROR,
        ];
        for message in messages {
            assert!(!message.contains("/Users/"));
            assert!(!message.contains("https://"));
            assert!(!message.contains("token="));
            assert!(!message.contains("sha256"));
        }
        assert_eq!(
            public_lookup("private-model-id").unwrap_err(),
            UNKNOWN_MODEL_PUBLIC_ERROR
        );
    }

    #[test]
    fn hex_encode_empty_input() {
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn hex_encode_matches_canonical_sha256() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        // (RFC 6234 test vector). Round-trips Sha256 → hex_encode →
        // known-good digest, which is what the download path does.
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            hex_encode(&h.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }

    #[test]
    fn hex_encode_is_lowercase_fixed_width() {
        // Every byte must produce exactly two hex chars; no 0x prefix,
        // no uppercase. Matches what we compare against the SHA pins
        // in the registry (which are stored lowercase).
        let bytes: [u8; 4] = [0x00, 0x0f, 0xff, 0xa5];
        assert_eq!(hex_encode(&bytes), "000fffa5");
    }

    fn test_entry(mirrors: &'static [&'static str]) -> ModelEntry {
        ModelEntry {
            id: "test-model",
            display_name: "Test",
            hf_repo_path: "example/repo",
            filename: "weights.bin",
            approx_bytes: 0,
            expected_bytes: 0,
            sha256: None,
            mirrors,
        }
    }

    #[test]
    fn download_urls_primary_only_when_no_mirrors() {
        let entry = test_entry(&[]);
        let urls = entry.download_urls();
        assert_eq!(urls.len(), 1);
        assert_eq!(
            urls[0],
            "https://huggingface.co/example/repo/resolve/main/weights.bin"
        );
    }

    #[test]
    fn download_urls_cascade_in_declared_order() {
        // Primary is always first; mirrors follow in declared order.
        // This is the contract the cascade logic in `download_file`
        // relies on.
        let entry = test_entry(&[
            "https://mirror-a.example/weights.bin",
            "https://mirror-b.example/weights.bin",
        ]);
        let urls = entry.download_urls();
        assert_eq!(urls.len(), 3);
        assert!(urls[0].contains("huggingface.co"));
        assert_eq!(urls[1], "https://mirror-a.example/weights.bin");
        assert_eq!(urls[2], "https://mirror-b.example/weights.bin");
    }

    #[test]
    fn registry_entry_ids_are_unique() {
        // Pins the bug from PR #193: an early convention used
        // `id: DEFAULT_STT_MODEL_ID` on the entry that was the
        // current default, with the constant pointing at that entry's
        // string id. When the constant flipped to a different entry's
        // string id, the original entry silently inherited the new
        // default's id — and now both rows compared equal to
        // `activeSttId`, lighting both radios in the picker. The fix
        // was to use literal string ids on every entry; this test
        // catches a recurrence regardless of which convention a
        // future contributor reaches for.
        let mut seen: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
        for entry in ModelRegistry::ENTRIES {
            assert!(
                seen.insert(entry.id),
                "duplicate registry entry id `{}` — every entry must have a unique id",
                entry.id,
            );
        }
    }

    #[test]
    fn every_shipping_registry_entry_has_a_sha_pin() {
        // Guard-rail against an unpinned entry sneaking into the
        // registry. Every entry in ENTRIES is something a user can
        // download via Settings; without a SHA pin, CDN
        // corruption or a supply-chain compromise of the host repo
        // would land silently on disk.
        //
        // If you're adding a new entry and this fails, do one of:
        //   1. Compute and pin the SHA (preferred).
        //   2. If the entry is still in dev and the bytes are churning,
        //      move it behind a `#[cfg(test)]` or a feature flag until
        //      it stabilizes — don't ship a None-sha in ENTRIES.
        for entry in ModelRegistry::ENTRIES {
            assert!(
                entry.sha256.is_some(),
                "registry entry `{}` is missing a sha256 pin — see the comment on ModelEntry.sha256",
                entry.id,
            );
            // Pins must be 64 lowercase hex chars (a full SHA-256).
            let pin = entry.sha256.unwrap();
            assert_eq!(
                pin.len(),
                64,
                "entry `{}` has sha256 pin of {} chars; expected 64",
                entry.id,
                pin.len(),
            );
            assert!(
                pin.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "entry `{}` sha256 must be lowercase hex",
                entry.id,
            );
        }
    }

    #[test]
    fn hash_file_sha256_matches_canonical_vector() {
        // Writes the RFC 6234 "abc" test vector to a temp file, runs the
        // on-disk streaming hash, and verifies the digest + byte count.
        // Keeps the file path unique per test run so parallel cargo test
        // invocations don't collide.
        use std::io::Write;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "murmur-hash-test-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"abc").unwrap();
        }
        // Track the final progress callback value so we also verify
        // the callback actually fired with the running byte count.
        let mut last_progress: u64 = 0;
        let (digest, bytes) = hash_file_sha256(&path, |hashed| last_progress = hashed).unwrap();
        let mut entry = test_entry(&[]);
        entry.expected_bytes = 3;
        entry.sha256 = Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(model_file_matches_registry_pin(&path, &entry).unwrap());
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        assert_eq!(bytes, 3);
        assert_eq!(last_progress, 3);
    }

    #[test]
    fn hash_file_sha256_reports_missing_file_cleanly() {
        // An absent path returns a clear `open …` error rather than
        // panicking — caller (`verify_model`) short-circuits before
        // this, but the helper is public within the module and should
        // not trap. A no-op progress callback mirrors what a test
        // harness would use when it doesn't care about intermediate
        // reports.
        let path = std::env::temp_dir().join("murmur-hash-test-definitely-missing.bin");
        let _ = std::fs::remove_file(&path);
        let err = hash_file_sha256(&path, |_| {}).unwrap_err();
        assert_eq!(err, "model open failed");
    }

    #[test]
    fn boot_selected_model_hydrates_without_an_eager_swap() {
        assert!(!model_selection_requires_swap(
            DEFAULT_STT_MODEL_ID,
            DEFAULT_STT_MODEL_ID
        ));
        assert!(model_selection_requires_swap(
            DEFAULT_STT_MODEL_ID,
            "different-reviewed-model"
        ));
    }

    #[test]
    fn download_failure_transient_vs_fatal_flag_round_trips() {
        // Guard-rail against a fat-finger swap between the two
        // constructors. Cascade logic in `download_file` branches on
        // `is_fatal` to decide whether to wipe the .part; flipping
        // the flag would either retry on corrupted bytes (fatal
        // misread as transient) or re-download every 2.5 GB file on
        // every network hiccup (transient misread as fatal).
        let t = DownloadFailure::transient("net dropped");
        assert!(!t.is_fatal);
        assert_eq!(t.message, "net dropped");
        let f = DownloadFailure::fatal("hash mismatch");
        assert!(f.is_fatal);
        assert_eq!(f.message, "hash mismatch");
    }

    #[test]
    fn rebuild_hasher_resumes_streaming_hash_correctly() {
        // The whole resume story hinges on this: write "abc" to a
        // file, rebuild the hasher from that file, feed "def" into
        // the rebuilt hasher, and confirm the final digest matches
        // SHA-256("abcdef"). If this ever breaks, resumed downloads
        // would all fail verification even when the bytes on disk
        // are correct.
        use sha2::Digest;
        use std::io::Write;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "murmur-resume-hasher-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"abc").unwrap();
        }
        let mut file = open_model_no_follow(&path, true, false).unwrap();
        let (mut hasher, bytes) = rebuild_hasher_from_file(&mut file).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(bytes, 3);
        hasher.update(b"def");
        let digest = hex_encode(&hasher.finalize());
        // SHA-256("abcdef")
        assert_eq!(
            digest,
            "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721",
        );
    }

    #[test]
    fn rebuild_hasher_on_empty_file_returns_initial_state() {
        // An empty `.part` (created then never written to) should
        // produce a fresh hasher + zero byte count, equivalent to
        // starting from scratch. Covers the edge case of a crashed
        // create-but-no-write scenario.
        use sha2::Digest;
        let path = std::env::temp_dir().join(format!(
            "murmur-resume-hasher-empty-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let _ = std::fs::File::create(&path).unwrap();
        let mut file = open_model_no_follow(&path, true, false).unwrap();
        let (hasher, bytes) = rebuild_hasher_from_file(&mut file).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(bytes, 0);
        let digest = hex_encode(&hasher.finalize());
        // SHA-256("")
        assert_eq!(
            digest,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
    }

    #[test]
    fn download_url_policy_rejects_unsafe_configured_urls() {
        let rejected = [
            "http://huggingface.co/model.bin",
            "https://user@huggingface.co/model.bin",
            "https://huggingface.co:444/model.bin",
            "https://huggingface.co/model.bin?key=value",
            "https://huggingface.co/model.bin#fragment",
            "https://unapproved.example/model.bin",
        ];
        for raw in rejected {
            let url = reqwest::Url::parse(raw).unwrap();
            assert!(validate_download_url(&url, DownloadUrlKind::Configured).is_err());
        }
        for entry in ModelRegistry::ENTRIES {
            for raw in entry.download_urls() {
                let url = reqwest::Url::parse(&raw).unwrap();
                assert_eq!(
                    validate_download_url(&url, DownloadUrlKind::Configured),
                    Ok(())
                );
            }
        }
    }

    #[test]
    fn redirect_policy_allows_only_reviewed_hugging_face_edge_and_cdn_query() {
        let origin = reqwest::Url::parse("https://huggingface.co/model.bin").unwrap();
        let cdn = reqwest::Url::parse("https://us.aws.cdn.hf.co/model.bin?key=value").unwrap();
        assert_eq!(validate_redirect_hop(&origin, &cdn, 1), Ok(()));
        assert!(validate_download_url(&cdn, DownloadUrlKind::Configured).is_err());

        let downgrade = reqwest::Url::parse("http://us.aws.cdn.hf.co/model.bin").unwrap();
        let port = reqwest::Url::parse("https://us.aws.cdn.hf.co:444/model.bin").unwrap();
        let credentials = reqwest::Url::parse("https://user@us.aws.cdn.hf.co/model.bin").unwrap();
        let fragment = reqwest::Url::parse("https://us.aws.cdn.hf.co/model.bin#fragment").unwrap();
        let unapproved = reqwest::Url::parse("https://unapproved.example/model.bin").unwrap();
        for next in [downgrade, port, credentials, fragment, unapproved] {
            assert!(validate_redirect_hop(&origin, &next, 1).is_err());
        }
        assert!(validate_redirect_hop(&cdn, &origin, 2).is_err());
        assert!(validate_redirect_hop(&origin, &cdn, DOWNLOAD_REDIRECT_LIMIT + 1).is_err());
    }

    #[test]
    fn content_range_parser_is_strict() {
        assert_eq!(parse_content_range("bytes 3-9/10"), Ok((3, 9, 10)));
        for invalid in [
            "3-9/10",
            "bytes 3-10/10",
            "bytes 9-3/10",
            "bytes */10",
            "bytes 3-9/*",
            "bytes 3-9/10/11",
        ] {
            assert!(parse_content_range(invalid).is_err());
        }
    }

    #[test]
    fn response_layout_requires_exact_full_or_resumed_size() {
        use reqwest::StatusCode;
        assert!(DOWNLOAD_TOTAL_TIMEOUT > DOWNLOAD_READ_TIMEOUT);
        assert!(DOWNLOAD_READ_TIMEOUT > DOWNLOAD_CONNECT_TIMEOUT);
        assert_eq!(
            validate_response_layout(StatusCode::OK, 0, 10, Some(10), None),
            Ok(false)
        );
        assert_eq!(
            validate_response_layout(StatusCode::OK, 3, 10, Some(10), None),
            Ok(true)
        );
        assert_eq!(
            validate_response_layout(
                StatusCode::PARTIAL_CONTENT,
                3,
                10,
                None,
                Some("bytes 3-9/10")
            ),
            Ok(false)
        );
        assert_eq!(
            validate_response_layout(
                StatusCode::PARTIAL_CONTENT,
                3,
                10,
                Some(7),
                Some("bytes 3-9/10")
            ),
            Ok(false)
        );
        for result in [
            validate_response_layout(StatusCode::CREATED, 0, 10, Some(10), None),
            validate_response_layout(StatusCode::OK, 0, 10, Some(9), None),
            validate_response_layout(StatusCode::OK, 0, 10, Some(10), Some("bytes 0-9/10")),
            validate_response_layout(
                StatusCode::PARTIAL_CONTENT,
                0,
                10,
                Some(10),
                Some("bytes 0-9/10"),
            ),
            validate_response_layout(
                StatusCode::PARTIAL_CONTENT,
                3,
                10,
                Some(7),
                Some("bytes 2-9/10"),
            ),
            validate_response_layout(
                StatusCode::PARTIAL_CONTENT,
                3,
                10,
                Some(7),
                Some("bytes 3-8/10"),
            ),
            validate_response_layout(
                StatusCode::PARTIAL_CONTENT,
                3,
                10,
                Some(7),
                Some("bytes 3-9/11"),
            ),
            validate_response_layout(
                StatusCode::PARTIAL_CONTENT,
                3,
                10,
                Some(6),
                Some("bytes 3-9/10"),
            ),
        ] {
            assert!(result.is_err());
        }
    }

    #[test]
    fn streaming_total_is_checked_and_capped() {
        assert_eq!(checked_download_total(3, 7, 10), Ok(10));
        assert!(checked_download_total(3, 8, 10).is_err());
        assert!(checked_download_total(u64::MAX, 1, u64::MAX).is_err());
    }

    #[test]
    fn transient_http_statuses_preserve_bounded_resume_data_even_with_bad_headers() {
        use reqwest::header::{HeaderMap, HeaderValue, CONTENT_LENGTH};
        use reqwest::StatusCode;

        let mut malformed_headers = HeaderMap::new();
        malformed_headers.insert(CONTENT_LENGTH, HeaderValue::from_static("not-a-number"));
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let failure = validate_and_parse_response_headers(status, &malformed_headers)
                .expect_err("transient status must be rejected before header parsing");
            assert!(!failure.is_fatal);
        }
        for status in [StatusCode::RANGE_NOT_SATISFIABLE, StatusCode::UNAUTHORIZED] {
            let failure = validate_and_parse_response_headers(status, &malformed_headers)
                .expect_err("fatal status must be rejected before header parsing");
            assert!(failure.is_fatal);
        }
        let malformed_success =
            validate_and_parse_response_headers(StatusCode::OK, &malformed_headers)
                .expect_err("malformed success metadata must remain fatal");
        assert!(malformed_success.is_fatal);
    }

    #[test]
    fn every_shipping_registry_entry_has_an_exact_size() {
        for entry in ModelRegistry::ENTRIES {
            assert!(entry.expected_bytes > 0);
            assert_eq!(entry.expected_bytes, entry.approx_bytes);
        }
    }

    #[cfg(unix)]
    #[test]
    fn partial_open_applies_private_mode_and_rejects_symlinks_and_hardlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = std::env::temp_dir().join(format!(
            "murmur-model-boundary-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        ensure_private_models_dir(&root).unwrap();
        assert_eq!(
            std::fs::symlink_metadata(&root)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let partial = root.join("model.bin.part");
        let file = open_model_no_follow(&partial, true, true).unwrap();
        assert_eq!(validate_model_descriptor(&file, 10).unwrap(), 0);
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        drop(file);
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o666)).unwrap();
        let file = open_model_no_follow(&partial, true, false).unwrap();
        harden_model_file_permissions(&file).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        drop(file);

        let link = root.join("symlink.bin.part");
        symlink(&partial, &link).unwrap();
        assert!(open_model_no_follow(&link, true, true).is_err());

        let fifo = root.join("fifo.bin.part");
        use std::os::unix::ffi::OsStrExt;
        let fifo_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_path` is a live NUL-terminated path and the mode has no
        // unsupported bits. The return value is checked before use.
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        let fifo_for_open = fifo.clone();
        let fifo_thread = std::thread::spawn(move || {
            let result = open_model_no_follow(&fifo_for_open, false, false)
                .and_then(|file| validate_model_descriptor(&file, 10).map(|_| ()));
            let _ = sender.send(result);
        });
        let fifo_result = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("FIFO validation must not block waiting for a writer");
        assert!(fifo_result.is_err());
        fifo_thread.join().unwrap();

        let real_directory = root.join("real-directory");
        std::fs::create_dir(&real_directory).unwrap();
        let directory_link = root.join("directory-link");
        symlink(&real_directory, &directory_link).unwrap();
        assert!(ensure_private_models_dir(&directory_link).is_err());

        let oversized = root.join("oversized.bin.part");
        std::fs::write(&oversized, [0_u8; 11]).unwrap();
        let oversized_file = open_model_no_follow(&oversized, true, false).unwrap();
        assert!(validate_model_descriptor(&oversized_file, 10).is_err());
        drop(oversized_file);

        let hardlink = root.join("hardlink.bin.part");
        std::fs::hard_link(&partial, &hardlink).unwrap();
        let file = open_model_no_follow(&partial, true, false).unwrap();
        assert!(validate_model_descriptor(&file, 10).is_err());
        drop(file);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn promotion_identity_check_detects_path_replacement() {
        let root = std::env::temp_dir().join(format!(
            "murmur-model-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("model.bin.part");
        std::fs::write(&path, b"original").unwrap();
        let file = open_model_no_follow(&path, false, false).unwrap();
        assert!(descriptor_matches_path(&file, &path).unwrap());
        std::fs::rename(&path, root.join("moved.bin.part")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        assert!(!descriptor_matches_path(&file, &path).unwrap());
        drop(file);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_size_is_required_by_registry_pin_check() {
        let path = std::env::temp_dir().join(format!(
            "murmur-model-size-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"abc").unwrap();
        let mut entry = test_entry(&[]);
        entry.expected_bytes = 4;
        entry.sha256 = Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(!model_file_matches_registry_pin(&path, &entry).unwrap());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn every_registry_mirror_is_well_formed() {
        // Guard-rail against a fat-finger in the mirrors array: each
        // URL must be https and must end with the same filename as the
        // primary entry. The cascade relies on every URL in the list
        // serving the same bytes; a mirror pointing at a different
        // filename is almost certainly a copy-paste bug.
        for entry in ModelRegistry::ENTRIES {
            for url in entry.mirrors {
                assert!(
                    url.starts_with("https://"),
                    "entry `{}` mirror `{}` must be https",
                    entry.id,
                    url,
                );
                assert!(
                    url.ends_with(entry.filename),
                    "entry `{}` mirror `{}` must end with filename `{}`",
                    entry.id,
                    url,
                    entry.filename,
                );
            }
        }
    }
}
