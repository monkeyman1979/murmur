//! Deterministic, portable Markdown output for durable captures (MT2).
//!
//! SQLite snapshots and commits are performed by the parent capture domain.
//! Every directory check, render, file write, sync, rename, clipboard/open
//! preparation, and settings-file operation in this module runs after the
//! SQLite connection guard has been dropped.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Local, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::{CaptureError, CaptureErrorCode, CaptureRecord, CaptureState, CaptureTrack};

const OUTPUT_SETTINGS_FILENAME: &str = "capture-output-directory.json";
const DEFAULT_TITLE: &str = "Untitled Meeting";
const MAX_FILENAME_BYTES: usize = 180;
const PARAGRAPH_MAX_GAP_MS: i64 = 2_000;
const PARAGRAPH_MAX_DURATION_MS: i64 = 30_000;
const PARAGRAPH_MAX_SEGMENTS: usize = 6;
const PARAGRAPH_MAX_WORDS: usize = 100;
const SYNC_DISCLOSURE: &str = "Files in Documents or another selected directory may be synchronized by iCloud Drive or third-party software depending on your Mac settings. Murmur itself does not upload them.";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static OUTPUT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureOutputDirectory {
    pub path: PathBuf,
    pub is_default: bool,
    pub sync_disclosure: String,
}

#[derive(Debug, Clone)]
pub(super) struct MarkdownSegment {
    pub id: i64,
    pub track: CaptureTrack,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text_final: String,
}

#[derive(Debug, Clone)]
pub(super) struct MarkdownSnapshot {
    pub capture: CaptureRecord,
    pub segments: Vec<MarkdownSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TranscriptParagraph {
    track: CaptureTrack,
    start_ms: i64,
    end_ms: i64,
    segment_count: usize,
    word_count: usize,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderedMarkdown {
    pub markdown: String,
    pub transcript_text: String,
    pub word_count: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteMode {
    CreateNew,
    ReplaceAuthorized,
}

#[derive(Debug, Serialize, Deserialize)]
struct OutputSettings {
    path: PathBuf,
}

pub(super) fn with_output_lock<T>(
    operation: impl FnOnce() -> Result<T, CaptureError>,
) -> Result<T, CaptureError> {
    let _guard = OUTPUT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "capture output lock is unavailable",
            )
        })?;
    operation()
}

pub(super) fn default_output_directory(
    documents_directory: &Path,
) -> Result<PathBuf, CaptureError> {
    validate_absolute_path(documents_directory)?;
    Ok(documents_directory.join("Murmur").join("Meetings"))
}

pub(super) fn get_output_directory(
    documents_directory: &Path,
    app_data_directory: &Path,
) -> Result<CaptureOutputDirectory, CaptureError> {
    let default = default_output_directory(documents_directory)?;
    let settings_path = output_settings_path(app_data_directory)?;
    let (selected, is_default) = match read_output_settings(&settings_path)? {
        Some(settings) => (settings.path, false),
        None => (default, true),
    };
    let path = validate_output_directory(&selected)?;
    Ok(CaptureOutputDirectory {
        path,
        is_default,
        sync_disclosure: SYNC_DISCLOSURE.into(),
    })
}

pub(super) fn set_output_directory(
    selected: &Path,
    documents_directory: &Path,
    app_data_directory: &Path,
) -> Result<CaptureOutputDirectory, CaptureError> {
    let default = default_output_directory(documents_directory)?;
    let selected_is_default = selected == default;
    let path = validate_output_directory(selected)?;
    let is_default = selected_is_default;
    let settings_path = output_settings_path(app_data_directory)?;
    if is_default {
        match fs::remove_file(&settings_path) {
            Ok(()) => sync_parent_directory(&settings_path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(output_directory_error(
                    "the capture output setting could not be cleared",
                ))
            }
        }
    } else {
        write_output_settings(&settings_path, &OutputSettings { path: path.clone() })?;
    }
    Ok(CaptureOutputDirectory {
        path,
        is_default,
        sync_disclosure: SYNC_DISCLOSURE.into(),
    })
}

fn output_settings_path(app_data_directory: &Path) -> Result<PathBuf, CaptureError> {
    validate_absolute_path(app_data_directory)?;
    fs::create_dir_all(app_data_directory).map_err(|_| {
        output_directory_error("the application data directory could not be created")
    })?;
    let canonical = fs::canonicalize(app_data_directory).map_err(|_| {
        output_directory_error("the application data directory could not be resolved")
    })?;
    Ok(canonical.join(OUTPUT_SETTINGS_FILENAME))
}

fn read_output_settings(path: &Path) -> Result<Option<OutputSettings>, CaptureError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(output_directory_error(
                "the capture output setting could not be inspected",
            ))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err(output_directory_error(
            "the capture output setting is not a safe regular file",
        ));
    }
    let bytes = fs::read(path)
        .map_err(|_| output_directory_error("the capture output setting could not be read"))?;
    let settings = serde_json::from_slice(&bytes)
        .map_err(|_| output_directory_error("the capture output setting is invalid"))?;
    Ok(Some(settings))
}

fn write_output_settings(path: &Path, settings: &OutputSettings) -> Result<(), CaptureError> {
    let bytes = serde_json::to_vec(settings)
        .map_err(|_| output_directory_error("the capture output setting could not be encoded"))?;
    let parent = path.parent().ok_or_else(|| {
        output_directory_error("the capture output setting has no parent directory")
    })?;
    let (temporary, mut file) = create_unique_restrictive_file(
        parent,
        ".capture-output-directory",
        ".json.tmp",
    )
    .map_err(|_| {
        output_directory_error("the capture output setting temporary file could not be created")
    })?;
    let result = (|| {
        file.write_all(&bytes).map_err(|_| {
            output_directory_error("the capture output setting could not be written")
        })?;
        file.sync_all().map_err(|_| {
            output_directory_error("the capture output setting could not be synchronized")
        })?;
        drop(file);
        fs::rename(&temporary, path).map_err(|_| {
            output_directory_error("the capture output setting could not be finalized")
        })?;
        sync_parent_directory(path);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn validate_output_directory(path: &Path) -> Result<PathBuf, CaptureError> {
    validate_absolute_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(output_directory_error(
                "the capture output directory cannot be a symbolic link",
            ))
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(output_directory_error(
                "the capture output path is not a directory",
            ))
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_directories_restrictive(path)?;
        }
        Err(_) => {
            return Err(output_directory_error(
                "the capture output directory could not be inspected",
            ))
        }
    }

    let canonical = fs::canonicalize(path).map_err(|_| {
        output_directory_error("the capture output directory could not be resolved")
    })?;
    if !canonical.is_absolute() || !canonical.is_dir() {
        return Err(output_directory_error(
            "the capture output directory is not a safe absolute directory",
        ));
    }
    // Store and subsequently use the canonical path. This prevents a later
    // relative-path interpretation and makes symlink replacement detectable.
    verify_writable(&canonical)?;
    Ok(canonical)
}

fn validate_absolute_path(path: &Path) -> Result<(), CaptureError> {
    if path.as_os_str().is_empty() || !path.is_absolute() || path.to_str().is_none() {
        return Err(output_directory_error(
            "the capture output directory must be an absolute path",
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(output_directory_error(
            "the capture output directory cannot contain relative components",
        ));
    }
    Ok(())
}

fn create_directories_restrictive(path: &Path) -> Result<(), CaptureError> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(output_directory_error(
                    "the capture output directory has an unsafe parent",
                ))
            }
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                current = current.parent().ok_or_else(|| {
                    output_directory_error("the capture output directory has no existing parent")
                })?;
            }
            Err(_) => {
                return Err(output_directory_error(
                    "the capture output directory parent could not be inspected",
                ))
            }
        }
    }
    fs::create_dir_all(path)
        .map_err(|_| output_directory_error("the capture output directory could not be created"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for directory in missing.iter().rev() {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|_| {
                output_directory_error(
                    "the capture output directory permissions could not be restricted",
                )
            })?;
        }
    }
    Ok(())
}

fn verify_writable(directory: &Path) -> Result<(), CaptureError> {
    let (probe, file) = create_unique_restrictive_file(directory, ".murmur-write-test", ".tmp")
        .map_err(|_| output_directory_error("the capture output directory is not writable"))?;
    let result = (|| {
        file.sync_all().map_err(|_| {
            output_directory_error("the capture output directory cannot synchronize files")
        })?;
        drop(file);
        fs::remove_file(&probe).map_err(|_| {
            output_directory_error("the capture output directory cannot remove temporary files")
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&probe);
    }
    result
}

/// Coalesce ordered durable segments only for readable output. Track equality means
/// capture-source provenance, not speaker identity; the SQLite rows remain untouched.
///
/// Native Whisper spans are not reliable evidence of simultaneous speech on sparse tracks,
/// so presentation deliberately stays flat until source activity can be quality-gated.
fn presentation_paragraphs(segments: &[MarkdownSegment]) -> Vec<TranscriptParagraph> {
    let mut paragraphs: Vec<TranscriptParagraph> = Vec::with_capacity(segments.len());
    for segment in segments {
        let text = single_line_text(&segment.text_final);
        let word_count = text.split_whitespace().count();
        let should_merge = paragraphs.last().is_some_and(|paragraph| {
            paragraph.track == segment.track
                && segment.start_ms >= paragraph.end_ms
                && segment.start_ms - paragraph.end_ms <= PARAGRAPH_MAX_GAP_MS
                && segment.end_ms - paragraph.start_ms <= PARAGRAPH_MAX_DURATION_MS
                && paragraph.segment_count < PARAGRAPH_MAX_SEGMENTS
                && paragraph.word_count.saturating_add(word_count) <= PARAGRAPH_MAX_WORDS
        });

        if should_merge {
            let paragraph = paragraphs.last_mut().expect("paragraph exists");
            paragraph.text.push(' ');
            paragraph.text.push_str(&text);
            paragraph.end_ms = segment.end_ms;
            paragraph.segment_count += 1;
            paragraph.word_count += word_count;
        } else {
            paragraphs.push(TranscriptParagraph {
                track: segment.track,
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                segment_count: 1,
                word_count,
                text,
            });
        }
    }
    paragraphs
}

pub(super) fn render(snapshot: &MarkdownSnapshot) -> Result<RenderedMarkdown, CaptureError> {
    if snapshot.capture.id <= 0 || snapshot.segments.is_empty() {
        return Err(CaptureError::new(
            CaptureErrorCode::TranscriptUnavailable,
            "capture has no final transcript segments to render",
        ));
    }
    if snapshot.segments.iter().any(|segment| {
        segment.id <= 0
            || segment.start_ms < 0
            || segment.end_ms < segment.start_ms
            || segment.text_final.trim().is_empty()
    }) {
        return Err(CaptureError::new(
            CaptureErrorCode::TranscriptUnavailable,
            "capture has invalid final transcript segments",
        ));
    }

    let title = normalized_title(&snapshot.capture.title);
    let word_count = snapshot
        .segments
        .iter()
        .map(|segment| segment.text_final.split_whitespace().count() as i64)
        .sum();

    let mut markdown = String::new();
    markdown.push_str("---\n");
    push_yaml_number(&mut markdown, "id", snapshot.capture.id);
    push_yaml_literal(&mut markdown, "type", "meeting_transcript");
    push_yaml_quoted(&mut markdown, "title", &title);
    push_yaml_timestamp(&mut markdown, "created", snapshot.capture.created_at)?;
    if let Some(started) = snapshot.capture.started_at {
        push_yaml_timestamp(&mut markdown, "started", started)?;
    }
    if let Some(ended) = snapshot.capture.ended_at {
        push_yaml_timestamp(&mut markdown, "ended", ended)?;
    }
    push_yaml_quoted(
        &mut markdown,
        "duration",
        &format_duration(snapshot.capture.active_duration_ms),
    );
    push_yaml_quoted(
        &mut markdown,
        "paused_duration",
        &format_duration(snapshot.capture.paused_duration_ms),
    );
    if let Some(source_name) = nonempty(snapshot.capture.source_name.as_deref()) {
        push_yaml_quoted(&mut markdown, "source_app", source_name);
    }
    if let Some(source_bundle) = nonempty(snapshot.capture.source_bundle.as_deref()) {
        push_yaml_quoted(&mut markdown, "source_bundle", source_bundle);
    }
    if let Some(language) = nonempty(snapshot.capture.language.as_deref()) {
        push_yaml_quoted(&mut markdown, "language", language);
    }
    push_yaml_number(&mut markdown, "word_count", word_count);
    push_yaml_literal(&mut markdown, "speaker_labels", "track_provenance");
    markdown.push_str("---\n\n# ");
    markdown.push_str(&escape_markdown_inline(&title));
    markdown.push_str("\n\n## Transcript\n\n");

    let mut transcript_text = String::new();
    for (index, paragraph) in presentation_paragraphs(&snapshot.segments)
        .iter()
        .enumerate()
    {
        let timestamp = format_duration(paragraph.start_ms);
        let label = track_label(paragraph.track);
        if index > 0 {
            markdown.push('\n');
            transcript_text.push_str("\n\n");
        }
        markdown.push_str("**[");
        markdown.push_str(&timestamp);
        markdown.push_str("] ");
        markdown.push_str(label);
        markdown.push_str(":** ");
        markdown.push_str(&escape_markdown_inline(&paragraph.text));
        markdown.push('\n');

        transcript_text.push('[');
        transcript_text.push_str(&timestamp);
        transcript_text.push_str("] ");
        transcript_text.push_str(label);
        transcript_text.push_str(": ");
        transcript_text.push_str(&paragraph.text);
    }
    markdown.push('\n');

    Ok(RenderedMarkdown {
        markdown,
        transcript_text,
        word_count,
    })
}

pub(super) fn rewrite_title(contents: &[u8], title: &str) -> Result<Vec<u8>, CaptureError> {
    let markdown = std::str::from_utf8(contents).map_err(|_| {
        CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the completed transcript is not valid UTF-8",
        )
    })?;
    if !markdown.starts_with("---\n") {
        return Err(CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the completed transcript has no recognizable frontmatter",
        ));
    }
    let frontmatter_end = markdown[4..]
        .find("\n---\n")
        .map(|offset| offset + 4)
        .ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the completed transcript has no closing frontmatter delimiter",
            )
        })?;
    let title_start = markdown[..frontmatter_end]
        .find("\ntitle: ")
        .map(|offset| offset + 1)
        .ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the completed transcript has no title metadata",
            )
        })?;
    let title_end = markdown[title_start..]
        .find('\n')
        .map(|offset| title_start + offset)
        .ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the completed transcript title metadata is malformed",
            )
        })?;
    let body_start = frontmatter_end + "\n---\n".len();
    if !markdown[body_start..].starts_with("\n# ") {
        return Err(CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the completed transcript has no recognizable title heading",
        ));
    }
    let heading_start = body_start + "\n# ".len();
    let heading_end = markdown[heading_start..]
        .find('\n')
        .map(|offset| heading_start + offset)
        .ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the completed transcript title heading is malformed",
            )
        })?;

    let title = normalized_title(title);
    let mut rewritten = String::with_capacity(markdown.len() + title.len());
    rewritten.push_str(&markdown[..title_start]);
    rewritten.push_str("title: \"");
    rewritten.push_str(&yaml_double_quoted(&title));
    rewritten.push('"');
    rewritten.push_str(&markdown[title_end..heading_start]);
    rewritten.push_str(&escape_markdown_inline(&title));
    rewritten.push_str(&markdown[heading_end..]);
    Ok(rewritten.into_bytes())
}

fn normalized_title(value: &str) -> String {
    let title = single_line_text(value);
    if title.trim_matches([' ', '.']).is_empty() {
        DEFAULT_TITLE.to_string()
    } else {
        title
    }
}

fn push_yaml_number(output: &mut String, key: &str, value: i64) {
    output.push_str(key);
    output.push_str(": ");
    output.push_str(&value.to_string());
    output.push('\n');
}

fn push_yaml_literal(output: &mut String, key: &str, value: &str) {
    output.push_str(key);
    output.push_str(": ");
    output.push_str(value);
    output.push('\n');
}

fn push_yaml_quoted(output: &mut String, key: &str, value: &str) {
    output.push_str(key);
    output.push_str(": \"");
    output.push_str(&yaml_double_quoted(value));
    output.push_str("\"\n");
}

fn push_yaml_timestamp(
    output: &mut String,
    key: &str,
    epoch_millis: i64,
) -> Result<(), CaptureError> {
    let timestamp = utc_timestamp(epoch_millis)?;
    push_yaml_quoted(
        output,
        key,
        &timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    Ok(())
}

fn yaml_double_quoted(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{:04X}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn escape_markdown_inline(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn single_line_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_control() || character.is_whitespace() {
            pending_space = !output.is_empty();
        } else {
            if pending_space {
                output.push(' ');
                pending_space = false;
            }
            output.push(character);
        }
    }
    output.trim().to_string()
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn track_label(track: CaptureTrack) -> &'static str {
    match track {
        CaptureTrack::Microphone => "You",
        CaptureTrack::System => "Meeting audio",
    }
}

fn format_duration(milliseconds: i64) -> String {
    let total_seconds = milliseconds.max(0) / 1000;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

pub(super) fn filename_for_capture(
    capture: &CaptureRecord,
    collision: u32,
) -> Result<String, CaptureError> {
    let generic_timestamp = generic_meeting_timestamp(&capture.title);
    let timestamp = match generic_timestamp.as_ref() {
        Some(timestamp) => timestamp.clone(),
        None => utc_timestamp(capture.created_at)?
            .with_timezone(&Local)
            .format("%Y-%m-%d %H%M")
            .to_string(),
    };
    let suffix = if collision <= 1 {
        ".md".to_string()
    } else {
        format!(" ({collision}).md")
    };
    let separator = " - ";
    let title_budget =
        MAX_FILENAME_BYTES.saturating_sub(separator.len() + timestamp.len() + suffix.len());
    let sanitized_title = generic_timestamp
        .map(|_| "Meeting".to_string())
        .unwrap_or_else(|| sanitize_filename_title(&capture.title));
    let title = truncate_utf8(&sanitized_title, title_budget);
    let title = title.trim_end_matches([' ', '.']);
    let title = if title.is_empty() {
        DEFAULT_TITLE
    } else {
        title
    };
    Ok(format!("{title}{separator}{timestamp}{suffix}"))
}

fn generic_meeting_timestamp(title: &str) -> Option<String> {
    let value = title.strip_prefix("Meeting ")?;
    let timestamp = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M").ok()?;
    // Parsing must consume the exact shipping auto-title shape rather than a
    // descriptive title that merely starts with "Meeting".
    (timestamp.format("%Y-%m-%d %H:%M").to_string() == value)
        .then(|| timestamp.format("%Y-%m-%d %H%M").to_string())
}

fn sanitize_filename_title(title: &str) -> String {
    let mut sanitized = String::with_capacity(title.len());
    let mut pending_space = false;
    for character in title.chars() {
        let unsafe_character = matches!(character, '/' | ':' | '\\' | '\0')
            || character.is_control()
            || is_unsafe_format_character(character);
        if unsafe_character || character.is_whitespace() {
            pending_space = !sanitized.is_empty();
        } else {
            if pending_space {
                sanitized.push(' ');
                pending_space = false;
            }
            sanitized.push(character);
        }
    }
    let sanitized = sanitized.trim().trim_end_matches([' ', '.']);
    let unsafe_name =
        sanitized.is_empty() || matches!(sanitized, "." | "..") || is_reserved_name(sanitized);
    if unsafe_name {
        DEFAULT_TITLE.into()
    } else {
        sanitized.into()
    }
}

fn is_unsafe_format_character(character: char) -> bool {
    matches!(
        character as u32,
        0x061C
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0xFEFF
    )
}

fn utc_timestamp(epoch_millis: i64) -> Result<DateTime<Utc>, CaptureError> {
    if epoch_millis < 0 {
        return Err(CaptureError::new(
            CaptureErrorCode::TranscriptUnavailable,
            "capture contains an invalid timestamp",
        ));
    }
    DateTime::<Utc>::from_timestamp_millis(epoch_millis).ok_or_else(|| {
        CaptureError::new(
            CaptureErrorCode::TranscriptUnavailable,
            "capture contains an invalid timestamp",
        )
    })
}

fn is_reserved_name(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(super) fn atomic_write(
    final_path: &Path,
    contents: &[u8],
    mode: WriteMode,
) -> Result<(), CaptureError> {
    validate_final_markdown_path(final_path)?;
    let parent = final_path.parent().ok_or_else(|| {
        CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the transcript path has no parent directory",
        )
    })?;
    let (temporary, mut file) =
        create_unique_restrictive_file(parent, ".murmur-transcript", ".md.tmp").map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the transcript temporary file could not be created",
            )
        })?;
    let result = (|| {
        file.write_all(contents).map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the transcript temporary file could not be written",
            )
        })?;
        file.flush().map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the transcript temporary file could not be flushed",
            )
        })?;
        file.sync_all().map_err(|_| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the transcript temporary file could not be synchronized",
            )
        })?;
        drop(file);

        match mode {
            WriteMode::CreateNew => rename_no_replace(&temporary, final_path),
            WriteMode::ReplaceAuthorized => fs::rename(&temporary, final_path),
        }
        .map_err(|error| {
            let code = if error.kind() == io::ErrorKind::AlreadyExists {
                CaptureErrorCode::OutputFileExists
            } else {
                CaptureErrorCode::MarkdownWriteFailed
            };
            CaptureError::new(
                code,
                if code == CaptureErrorCode::OutputFileExists {
                    "the transcript file already exists; explicit overwrite authorization is required"
                } else {
                    "the transcript temporary file could not be atomically finalized"
                },
            )
        })?;
        sync_parent_directory(final_path);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn validate_completed_file(path: &Path) -> Result<(), CaptureError> {
    validate_final_markdown_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        let code = if error.kind() == io::ErrorKind::NotFound {
            CaptureErrorCode::TranscriptFileMissing
        } else {
            CaptureErrorCode::MarkdownWriteFailed
        };
        CaptureError::new(code, "the completed transcript file is unavailable")
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the completed transcript path is not a safe regular file",
        ));
    }
    Ok(())
}

pub(super) fn validate_final_markdown_path(path: &Path) -> Result<(), CaptureError> {
    validate_absolute_path(path)?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the transcript filename is not valid UTF-8",
            )
        })?;
    if filename.starts_with('.') || !filename.ends_with(".md") || filename.ends_with(".tmp") {
        return Err(CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the transcript path is not a completed Markdown filename",
        ));
    }
    Ok(())
}

fn unique_temporary_path(directory: &Path, prefix: &str, suffix: &str) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    directory.join(format!("{prefix}-{}-{counter}{suffix}", std::process::id()))
}

fn create_unique_restrictive_file(
    directory: &Path,
    prefix: &str,
    suffix: &str,
) -> io::Result<(PathBuf, File)> {
    for _ in 0..10_000 {
        let path = unique_temporary_path(directory, prefix, suffix);
        match open_restrictive_new(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many temporary file collisions",
    ))
}

#[cfg(unix)]
fn open_restrictive_new(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_restrictive_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use rustix::fs::{renameat_with, RenameFlags, CWD};
    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(io::Error::from)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    // The application feature is macOS-only. This fallback retains atomic
    // no-clobber publication for portability tests on other hosts: linking a
    // fully-synced sibling creates the destination in one operation.
    fs::hard_link(source, destination)?;
    fs::remove_file(source)
}

fn sync_parent_directory(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
    }
}

fn output_directory_error(message: &'static str) -> CaptureError {
    CaptureError::new(CaptureErrorCode::OutputDirectoryUnwritable, message)
}

pub(super) fn ensure_regeneration_target(
    path: &Path,
    overwrite: bool,
) -> Result<WriteMode, CaptureError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(WriteMode::CreateNew),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(CaptureError::new(
                CaptureErrorCode::MarkdownWriteFailed,
                "the stored transcript path is not a safe regular file",
            ))
        }
        Ok(_) if overwrite => Ok(WriteMode::ReplaceAuthorized),
        Ok(_) => Err(CaptureError::new(
            CaptureErrorCode::OutputFileExists,
            "the transcript file already exists; explicit overwrite authorization is required",
        )),
        Err(_) => Err(CaptureError::new(
            CaptureErrorCode::MarkdownWriteFailed,
            "the stored transcript path could not be inspected",
        )),
    }
}

pub(super) fn capture_requires_finalize(snapshot: &MarkdownSnapshot) -> Result<(), CaptureError> {
    if snapshot.capture.status != CaptureState::Finalizing {
        return Err(CaptureError::new(
            CaptureErrorCode::InvalidStateTransition,
            "capture must be finalizing before Markdown can be generated",
        ));
    }
    Ok(())
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
        let root = std::env::temp_dir().join(format!(
            "murmur-mt2-markdown-{name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn capture(title: &str) -> CaptureRecord {
        CaptureRecord {
            id: 42,
            kind: "meeting".into(),
            status: CaptureState::Finalizing,
            title: title.into(),
            created_at: 1_721_212_645_123,
            updated_at: 1_721_212_645_123,
            started_at: Some(1_721_212_650_123),
            ended_at: Some(1_721_216_508_123),
            active_duration_ms: 3_858_000,
            paused_duration_ms: 2_000,
            source_kind: Some("application".into()),
            source_name: Some("Chrome \"Beta\"\nUnsafe".into()),
            source_bundle: Some("com.google.Chrome".into()),
            language: Some("en".into()),
            word_count: 0,
            transcript_text: None,
            markdown_path: None,
            staging_path: None,
            error_code: None,
            error_message: None,
            completed_chunk_count: 2,
            audio_retention: crate::captures::MeetingAudioRetention::FailurePreserved,
            audio_delete_at: None,
            audio_bytes: 0,
        }
    }

    fn segment(
        id: i64,
        track: CaptureTrack,
        start_ms: i64,
        end_ms: i64,
        text: &str,
    ) -> MarkdownSegment {
        MarkdownSegment {
            id,
            track,
            start_ms,
            end_ms,
            text_final: text.into(),
        }
    }

    fn snapshot(title: &str) -> MarkdownSnapshot {
        MarkdownSnapshot {
            capture: capture(title),
            segments: vec![
                segment(
                    1,
                    CaptureTrack::Microphone,
                    4_000,
                    6_000,
                    "Let's *start*\nnow.",
                ),
                segment(2, CaptureTrack::System, 11_000, 15_000, "The [first] item."),
            ],
        }
    }

    #[test]
    fn rendering_is_deterministic_utf8_and_yaml_safe() {
        let first = render(&snapshot("Project: \"Launch\"\n#1")).unwrap();
        let second = render(&snapshot("Project: \"Launch\"\n#1")).unwrap();
        assert_eq!(first, second);
        assert!(std::str::from_utf8(first.markdown.as_bytes()).is_ok());
        assert!(first
            .markdown
            .contains("title: \"Project: \\\"Launch\\\" #1\""));
        assert!(first
            .markdown
            .contains("source_app: \"Chrome \\\"Beta\\\"\\nUnsafe\""));
        assert!(first
            .markdown
            .contains("**[00:00:04] You:** Let's \\*start\\* now."));
        assert!(first
            .markdown
            .contains("**[00:00:11] Meeting audio:** The \\[first\\] item."));
        assert!(!first.markdown.contains("speaker:"));
        assert_eq!(first.word_count, 6);
    }

    #[test]
    fn adjacent_same_source_segments_render_as_one_presentation_paragraph() {
        let rendered = render(&MarkdownSnapshot {
            capture: capture("Paragraphs"),
            segments: vec![
                segment(1, CaptureTrack::Microphone, 1_000, 2_000, "First thought."),
                segment(
                    2,
                    CaptureTrack::Microphone,
                    2_500,
                    4_000,
                    "Second *thought*.\nStill here.",
                ),
                segment(3, CaptureTrack::System, 4_500, 5_500, "Remote response."),
            ],
        })
        .unwrap();

        assert_eq!(
            rendered.transcript_text,
            "[00:00:01] You: First thought. Second *thought*. Still here.\n\n[00:00:04] Meeting audio: Remote response."
        );
        assert!(rendered
            .markdown
            .contains("**[00:00:01] You:** First thought. Second \\*thought\\*. Still here."));
        assert!(!rendered.transcript_text.contains("[00:00:02]"));
        assert_eq!(rendered.word_count, 8);
    }

    #[test]
    fn contained_cross_track_segments_render_flat_until_activity_timing_is_quality_gated() {
        let rendered = render(&MarkdownSnapshot {
            capture: capture("Flat overlap"),
            segments: vec![
                segment(
                    1,
                    CaptureTrack::System,
                    0,
                    10_000,
                    "Alpha beta gamma delta.",
                ),
                segment(2, CaptureTrack::Microphone, 3_000, 4_000, "Epsilon."),
            ],
        })
        .unwrap();

        assert_eq!(
            rendered.transcript_text,
            "[00:00:00] Meeting audio: Alpha beta gamma delta.\n\n[00:00:03] You: Epsilon."
        );
        assert!(!rendered.markdown.contains("> **"));
        assert!(!rendered.markdown.contains("(overlapping)"));
        assert_eq!(rendered.word_count, 5);
    }

    #[test]
    fn long_sparse_system_span_cannot_invert_microphone_presentation() {
        let rendered = render(&MarkdownSnapshot {
            capture: capture("Sparse system"),
            segments: vec![
                segment(1, CaptureTrack::System, 0, 30_000, "Alpha."),
                segment(
                    2,
                    CaptureTrack::Microphone,
                    5_000,
                    10_000,
                    "Bravo charlie delta.",
                ),
                segment(3, CaptureTrack::Microphone, 10_200, 11_000, "Echo."),
            ],
        })
        .unwrap();

        assert_eq!(
            rendered.transcript_text,
            "[00:00:00] Meeting audio: Alpha.\n\n[00:00:05] You: Bravo charlie delta. Echo."
        );
        assert!(!rendered.markdown.contains("> **"));
        assert!(!rendered.markdown.contains("(overlapping)"));
        assert_eq!(rendered.word_count, 5);
    }

    #[test]
    fn long_sparse_microphone_span_cannot_invert_system_presentation() {
        let rendered = render(&MarkdownSnapshot {
            capture: capture("Sparse microphone"),
            segments: vec![
                segment(1, CaptureTrack::Microphone, 0, 30_000, "Alpha."),
                segment(
                    2,
                    CaptureTrack::System,
                    5_000,
                    10_000,
                    "Bravo charlie delta.",
                ),
                segment(3, CaptureTrack::System, 10_200, 11_000, "Echo."),
            ],
        })
        .unwrap();

        assert_eq!(
            rendered.transcript_text,
            "[00:00:00] You: Alpha.\n\n[00:00:05] Meeting audio: Bravo charlie delta. Echo."
        );
        assert!(!rendered.markdown.contains("> **"));
        assert!(!rendered.markdown.contains("(overlapping)"));
        assert_eq!(rendered.word_count, 5);
    }

    #[test]
    fn overlapping_run_and_takeover_keep_canonical_order_and_flat_grouping() {
        let paragraphs = presentation_paragraphs(&[
            segment(1, CaptureTrack::Microphone, 0, 14_000, "Alpha."),
            segment(2, CaptureTrack::System, 2_000, 6_000, "Bravo charlie."),
            segment(3, CaptureTrack::System, 6_200, 10_000, "Delta echo."),
            segment(4, CaptureTrack::System, 14_100, 15_000, "Foxtrot."),
        ]);

        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[0].text, "Alpha.");
        assert_eq!(paragraphs[1].text, "Bravo charlie. Delta echo.");
        assert_eq!(paragraphs[1].segment_count, 2);
        assert_eq!(paragraphs[2].text, "Foxtrot.");
    }

    #[test]
    fn flat_fail_closed_sequence_can_merge_former_overlap_and_takeover_segments() {
        let paragraphs = presentation_paragraphs(&[
            segment(1, CaptureTrack::Microphone, 0, 14_000, "Alpha."),
            segment(2, CaptureTrack::System, 2_000, 6_000, "Bravo."),
            segment(3, CaptureTrack::System, 6_200, 15_000, "Charlie."),
        ]);

        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].text, "Alpha.");
        assert_eq!(paragraphs[1].text, "Bravo. Charlie.");
        assert_eq!(paragraphs[1].start_ms, 2_000);
        assert_eq!(paragraphs[1].end_ms, 15_000);
    }

    #[test]
    fn overlap_track_change_and_long_gap_keep_segments_separate() {
        let paragraphs = presentation_paragraphs(&[
            segment(1, CaptureTrack::Microphone, 0, 2_000, "One."),
            segment(2, CaptureTrack::Microphone, 1_500, 2_500, "Overlap."),
            segment(3, CaptureTrack::System, 2_500, 3_000, "Other source."),
            segment(4, CaptureTrack::System, 5_001, 6_000, "After gap."),
        ]);

        assert_eq!(paragraphs.len(), 4);
        assert_eq!(paragraphs[0].text, "One.");
        assert_eq!(paragraphs[1].text, "Overlap.");
        assert_eq!(paragraphs[2].text, "Other source.");
        assert_eq!(paragraphs[3].text, "After gap.");
    }

    #[test]
    fn exact_gap_and_duration_boundaries_merge_but_excess_does_not() {
        let paragraphs = presentation_paragraphs(&[
            segment(1, CaptureTrack::System, 0, 14_000, "One."),
            segment(2, CaptureTrack::System, 16_000, 30_000, "Two."),
            segment(3, CaptureTrack::System, 31_000, 32_000, "Three."),
        ]);

        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].text, "One. Two.");
        assert_eq!(paragraphs[0].start_ms, 0);
        assert_eq!(paragraphs[0].end_ms, 30_000);
        assert_eq!(paragraphs[1].text, "Three.");
    }

    #[test]
    fn segment_and_word_caps_bound_long_same_source_runs() {
        let six_segments: Vec<_> = (0..7)
            .map(|index| {
                segment(
                    index + 1,
                    CaptureTrack::System,
                    index * 1_000,
                    index * 1_000 + 500,
                    "word",
                )
            })
            .collect();
        let paragraphs = presentation_paragraphs(&six_segments);
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].segment_count, PARAGRAPH_MAX_SEGMENTS);
        assert_eq!(paragraphs[1].segment_count, 1);

        let ninety_nine_words = std::iter::repeat_n("word", 99)
            .collect::<Vec<_>>()
            .join(" ");
        let paragraphs = presentation_paragraphs(&[
            segment(1, CaptureTrack::Microphone, 0, 500, &ninety_nine_words),
            segment(2, CaptureTrack::Microphone, 500, 1_000, "hundred"),
            segment(3, CaptureTrack::Microphone, 1_000, 1_500, "overflow"),
        ]);
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0].word_count, PARAGRAPH_MAX_WORDS);
        assert_eq!(paragraphs[1].text, "overflow");
    }

    #[test]
    fn filename_puts_title_first_and_uses_the_original_local_timestamp_once() {
        let generic = capture("Meeting 2026-09-05 21:29");
        assert_eq!(
            filename_for_capture(&generic, 1).unwrap(),
            "Meeting - 2026-09-05 2129.md"
        );

        let custom = capture("New Title Name");
        let local_timestamp = utc_timestamp(custom.created_at)
            .unwrap()
            .with_timezone(&Local)
            .format("%Y-%m-%d %H%M");
        assert_eq!(
            filename_for_capture(&custom, 1).unwrap(),
            format!("New Title Name - {local_timestamp}.md")
        );
        assert_eq!(
            filename_for_capture(&custom, 2).unwrap(),
            format!("New Title Name - {local_timestamp} (2).md")
        );
    }

    #[test]
    fn filename_sanitizes_unsafe_names_and_caps_utf8_bytes() {
        let unsafe_capture = capture(" /:\0\n... ");
        let filename = filename_for_capture(&unsafe_capture, 1).unwrap();
        assert!(filename.starts_with("Untitled Meeting - "));
        assert!(filename.ends_with(".md"));
        assert!(!filename.contains('/'));
        assert!(!filename.contains(':'));
        assert!(filename.len() <= MAX_FILENAME_BYTES);

        let long = capture(&"é".repeat(300));
        let filename = filename_for_capture(&long, 9).unwrap();
        assert!(filename.len() <= MAX_FILENAME_BYTES);
        assert!(filename.ends_with(" (9).md"));
        assert!(filename.is_char_boundary(filename.len()));

        assert!(filename_for_capture(&capture("NUL"), 1)
            .unwrap()
            .starts_with("Untitled Meeting - "));
        assert!(filename_for_capture(&capture("\u{202E}"), 1)
            .unwrap()
            .starts_with("Untitled Meeting - "));
    }

    #[test]
    fn collisions_are_numbered_without_overwrite() {
        let root = root("collisions");
        let capture = capture("Planning");
        let first = root.join(filename_for_capture(&capture, 1).unwrap());
        fs::write(&first, "unrelated").unwrap();
        let second = root.join(filename_for_capture(&capture, 2).unwrap());
        assert!(second
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(" (2).md"));
        atomic_write(&second, b"complete", WriteMode::CreateNew).unwrap();
        assert_eq!(fs::read_to_string(first).unwrap(), "unrelated");
        assert_eq!(fs::read_to_string(second).unwrap(), "complete");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_write_refuses_existing_destination_and_cleans_temporary() {
        let root = root("refuse");
        let target = root.join("transcript.md");
        fs::write(&target, "external edit").unwrap();
        let error = atomic_write(&target, b"replacement", WriteMode::CreateNew).unwrap_err();
        assert_eq!(error.code, CaptureErrorCode::OutputFileExists);
        assert_eq!(fs::read_to_string(&target).unwrap(), "external edit");
        assert!(fs::read_dir(&root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_temporary_file_is_never_reused_as_completed_output() {
        let root = root("stale-temp");
        let stale = unique_temporary_path(&root, ".murmur-transcript", ".md.tmp");
        fs::write(&stale, "partial").unwrap();
        let (fresh, mut file) =
            create_unique_restrictive_file(&root, ".murmur-transcript", ".md.tmp").unwrap();
        file.write_all(b"complete").unwrap();
        file.sync_all().unwrap();
        drop(file);
        assert_ne!(fresh, stale);
        assert_eq!(fs::read_to_string(stale).unwrap(), "partial");
        assert!(fs::read_dir(&root).unwrap().all(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn regeneration_requires_authorization_to_replace() {
        let root = root("regenerate");
        let target = root.join("transcript.md");
        assert_eq!(
            ensure_regeneration_target(&target, false).unwrap(),
            WriteMode::CreateNew
        );
        fs::write(&target, "edited").unwrap();
        assert_eq!(
            ensure_regeneration_target(&target, false).unwrap_err().code,
            CaptureErrorCode::OutputFileExists
        );
        let mode = ensure_regeneration_target(&target, true).unwrap();
        atomic_write(&target, b"regenerated", mode).unwrap();
        assert_eq!(fs::read_to_string(target).unwrap(), "regenerated");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn output_directory_setting_is_absolute_persisted_and_disclosed() {
        let root = root("settings");
        let documents = root.join("Documents");
        let app_data = root.join("AppData");
        fs::create_dir_all(&documents).unwrap();
        fs::create_dir_all(&app_data).unwrap();
        let default = get_output_directory(&documents, &app_data).unwrap();
        assert!(default.is_default);
        assert!(default.path.ends_with("Documents/Murmur/Meetings"));
        assert!(default.sync_disclosure.contains("iCloud"));
        assert!(default.sync_disclosure.contains("third-party"));

        let custom = root.join("Custom");
        let selected = set_output_directory(&custom, &documents, &app_data).unwrap();
        assert!(!selected.is_default);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&custom).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert_eq!(
            get_output_directory(&documents, &app_data).unwrap().path,
            selected.path
        );
        assert!(set_output_directory(Path::new("relative"), &documents, &app_data).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn output_and_destination_symlinks_are_rejected_without_following() {
        use std::os::unix::fs::symlink;

        let root = root("symlink-output");
        let real_directory = root.join("real");
        fs::create_dir(&real_directory).unwrap();
        let linked_directory = root.join("linked");
        symlink(&real_directory, &linked_directory).unwrap();
        assert_eq!(
            validate_output_directory(&linked_directory)
                .unwrap_err()
                .code,
            CaptureErrorCode::OutputDirectoryUnwritable
        );

        let outside = root.join("outside.md");
        fs::write(&outside, "unrelated").unwrap();
        let destination = real_directory.join("transcript.md");
        symlink(&outside, &destination).unwrap();
        assert_eq!(
            atomic_write(&destination, b"replacement", WriteMode::CreateNew)
                .unwrap_err()
                .code,
            CaptureErrorCode::OutputFileExists
        );
        assert!(ensure_regeneration_target(&destination, true).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "unrelated");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn generated_file_permissions_are_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let root = root("permissions");
        let target = root.join("transcript.md");
        atomic_write(&target, b"private", WriteMode::CreateNew).unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0);
        assert_eq!(mode & 0o600, 0o600);
        fs::remove_dir_all(root).unwrap();
    }
}
