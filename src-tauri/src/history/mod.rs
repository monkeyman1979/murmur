//! Local SQLite-backed transcript history (SPEC §2.8).
//!
//! The store is the durable record of every dictation that actually
//! made it through the pipeline. Survives app restart (unlike the
//! in-memory session list that powered `/home` before this landed),
//! feeds the stats dashboard (§2.9), and — once the UI catches up —
//! will be the source for one-click "remember this correction" from
//! a past entry (§2.6).
//!
//! SQLite was chosen over flat-file JSONL / KV stores because §2.8
//! specifically asks for search, filter-by-app, and date ranges, and
//! §2.9 asks for aggregations across the corpus. Those are trivial
//! in SQL + FTS5 and expensive to hand-roll elsewhere.
//!
//! Durability tuning: WAL journaling + `synchronous=NORMAL`. Writes
//! are fast enough to sit on the hot dictation path without stalling
//! paste, and we lose at most the last few transactions on a hard
//! crash — acceptable for a dictation log.
//!
//! Timestamps are stored as epoch milliseconds (INTEGER). Sort
//! correctly, round-trip cleanly with the frontend's existing
//! `RecordedTranscript.created_at: number` shape, and avoid adding a
//! `chrono` dep.

use std::ffi::OsString;
use std::fs::File;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Schema version persisted via `PRAGMA user_version`. Bump whenever
/// we add or modify tables; the `migrate` function applies each step
/// idempotently on open.
const SCHEMA_VERSION: u32 = 5;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite storage operation failed")]
    Sqlite(#[source] rusqlite::Error),
    #[error("durable storage boundary validation failed")]
    Storage(#[source] io::Error),
    #[error("store poisoned")]
    Poisoned,
    #[error("existing database schema is not recognized as Murmur storage")]
    UnrecognizedSchema,
    #[error("database schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Storage(error)
    }
}

/// Wire-format record. Mirrors the frontend's `TranscriptRecord`
/// after camelCase conversion. `id` is `None` on insert (SQLite
/// assigns) and always `Some` on read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub created_at: i64,
    pub text_raw: String,
    pub text_final: String,
    pub duration_ms: u32,
    pub word_count: u32,
    pub app_name: Option<String>,
    pub app_bundle: Option<String>,
}

impl TranscriptRecord {
    /// Convenience for the hotkey pipeline: build a record with
    /// `created_at` = now and `word_count` derived from `text_final`.
    pub fn new_now(
        text_raw: String,
        text_final: String,
        duration_ms: u32,
        app_name: Option<String>,
        app_bundle: Option<String>,
    ) -> Self {
        let word_count = count_words(&text_final);
        Self {
            id: None,
            created_at: now_millis(),
            text_raw,
            text_final,
            duration_ms,
            word_count,
            app_name,
            app_bundle,
        }
    }
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn count_words(text: &str) -> u32 {
    text.split_whitespace().count() as u32
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoreHealth {
    Durable,
    Ephemeral { reason: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn unsafe_storage(message: &'static str) -> StoreError {
    StoreError::Storage(io::Error::other(message))
}

fn ensure_private_database_parent(path: &Path) -> Result<PathBuf, StoreError> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(unsafe_storage("database path is not a safe absolute path"));
    }
    let parent = path
        .parent()
        .filter(|parent| parent.parent().is_some())
        .ok_or_else(|| unsafe_storage("database path has no private parent"))?;

    let mut current = PathBuf::new();
    for component in parent.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir | Component::ParentDir => {
                return Err(unsafe_storage(
                    "database directory contains relative components",
                ));
            }
        }
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_private_directory(&current)?;
                std::fs::symlink_metadata(&current)?
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(unsafe_storage("database directory has an unsafe component"));
        }
    }

    let directory = open_private_directory(parent, true)?;
    if !directory.metadata()?.is_dir() {
        return Err(unsafe_storage("database parent is not a directory"));
    }
    Ok(parent.to_path_buf())
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                Err(io::Error::other(
                    "database directory creation raced with an unsafe path",
                ))
            } else {
                Ok(())
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn open_private_directory(path: &Path, harden: bool) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let file: File = descriptor.into();
    if harden {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o777 != 0o700 {
            file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_private_directory(path: &Path, _harden: bool) -> io::Result<File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other("database parent is not a real directory"));
    }
    File::open(path)
}

fn validate_database_descriptor(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("database artifact is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(io::Error::other("database artifact must have one link"));
        }
    }
    Ok(())
}

fn harden_database_descriptor(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open_database_artifact(path: &Path, create_new: bool) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let mut flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    if create_new {
        flags |= OFlags::CREATE | OFlags::EXCL;
    }
    let descriptor =
        rustix::fs::open(path, flags, Mode::from_raw_mode(0o600)).map_err(io::Error::from)?;
    Ok(descriptor.into())
}

#[cfg(not(unix))]
fn open_database_artifact(path: &Path, create_new: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(create_new);
    options.open(path)
}

fn prepare_database_descriptor(file: File) -> io::Result<File> {
    // Validate link count before chmod: a hard-linked sentinel must remain
    // completely untouched when the storage boundary refuses it.
    validate_database_descriptor(&file)?;
    harden_database_descriptor(&file)?;
    Ok(file)
}

/// Open and validate an existing artifact without changing it. Identity checks
/// must use this path so a raced replacement is refused before any chmod.
fn open_existing_validated_artifact(path: &Path) -> Result<Option<File>, StoreError> {
    match open_database_artifact(path, false) {
        Ok(file) => {
            validate_database_descriptor(&file)?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

struct OpenedDatabase {
    file: File,
    initialize: bool,
}

fn open_or_create_private_database(path: &Path) -> Result<OpenedDatabase, StoreError> {
    for _ in 0..3 {
        if let Some(file) = open_existing_validated_artifact(path)? {
            let initialize = file.metadata()?.len() == 0;
            return Ok(OpenedDatabase { file, initialize });
        }
        match open_database_artifact(path, true) {
            Ok(file) => {
                let file = prepare_database_descriptor(file)?;
                return Ok(OpenedDatabase {
                    file,
                    initialize: true,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(unsafe_storage("database creation raced repeatedly"))
}

fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(FileIdentity {})
    }
}

fn descriptor_matches_path(file: &File, path: &Path) -> io::Result<bool> {
    let Some(path_file) = open_existing_validated_artifact(path).map_err(|error| match error {
        StoreError::Storage(error) => error,
        _ => io::Error::other("database artifact validation failed"),
    })?
    else {
        return Ok(false);
    };
    Ok(file_identity(file)? == file_identity(&path_file)?)
}

fn database_artifact_paths(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name: OsString = path.as_os_str().to_os_string();
        name.push(suffix);
        paths.push(PathBuf::from(name));
    }
    paths
}

#[derive(Debug)]
struct DurableStorageBoundary {
    parent_path: PathBuf,
    parent_identity: FileIdentity,
    artifacts: Vec<(PathBuf, Option<FileIdentity>)>,
}

impl DurableStorageBoundary {
    fn verify(&self) -> Result<(), StoreError> {
        // Runtime verification is intentionally read-only. If a path was
        // replaced, refuse it without first changing the replacement's mode.
        let parent = open_private_directory(&self.parent_path, false)?;
        if file_identity(&parent)? != self.parent_identity {
            return Err(unsafe_storage("database directory identity changed"));
        }
        for (path, expected) in &self.artifacts {
            let actual = open_existing_validated_artifact(path)?
                .as_ref()
                .map(file_identity)
                .transpose()?;
            if &actual != expected {
                return Err(unsafe_storage("database artifact identity changed"));
            }
        }
        Ok(())
    }
}

pub struct TranscriptStore {
    conn: Mutex<Connection>,
    health: StoreHealth,
    durable_boundary: Option<DurableStorageBoundary>,
}

impl TranscriptStore {
    /// Open or create a durable store at `path`. The dedicated parent is
    /// created privately when absent. Existing database, WAL, SHM, and journal
    /// paths must be nonsymlink regular files with one link on Unix.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if path == Path::new(":memory:") {
            return Self::open_ephemeral();
        }
        let parent_path = ensure_private_database_parent(path)?;
        let parent = open_private_directory(&parent_path, true)?;
        let parent_identity = file_identity(&parent)?;
        let OpenedDatabase {
            file: database,
            initialize,
        } = open_or_create_private_database(path)?;
        let database_identity = file_identity(&database)?;
        let artifact_paths = database_artifact_paths(path);

        // Refuse suspicious pre-existing sidecars before SQLite can inspect,
        // recover, truncate, or otherwise act on them. Keep the descriptors so
        // an accepted existing database can be hardened only after its schema
        // is recognized, without reopening a raced path.
        let mut preflight_sidecars = Vec::with_capacity(artifact_paths.len() - 1);
        for artifact in artifact_paths.iter().skip(1) {
            preflight_sidecars.push(open_existing_validated_artifact(artifact)?);
        }
        if initialize && preflight_sidecars.iter().any(Option::is_some) {
            return Err(unsafe_storage(
                "uninitialized database has pre-existing SQLite sidecars",
            ));
        }
        if !descriptor_matches_path(&database, path)? {
            return Err(unsafe_storage("database path changed before SQLite open"));
        }

        // A nonempty existing SQLite file is not automatically Murmur-owned.
        // Recognize a supported Murmur schema through a read-only connection
        // before chmod, migration, recovery, or WAL initialization.
        if !initialize {
            let read_flags = OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
            let read_conn = Connection::open_with_flags(path, read_flags)?;
            if !descriptor_matches_path(&database, path)? {
                return Err(unsafe_storage(
                    "database path changed during schema recognition",
                ));
            }
            validate_existing_murmur_schema(&read_conn)?;
        }

        // Harden only descriptors that have been adopted. A later identity
        // mismatch is checked through nonmutating opens and refused untouched.
        harden_database_descriptor(&database)?;
        for sidecar in preflight_sidecars.iter().flatten() {
            harden_database_descriptor(sidecar)?;
        }
        if !descriptor_matches_path(&database, path)? {
            return Err(unsafe_storage("database path changed before writable open"));
        }
        for (artifact, expected) in artifact_paths
            .iter()
            .skip(1)
            .zip(preflight_sidecars.iter_mut())
        {
            let actual = open_existing_validated_artifact(artifact)?;
            let expected_identity = expected.as_ref().map(file_identity).transpose()?;
            let actual_identity = actual.as_ref().map(file_identity).transpose()?;
            if expected_identity.is_some() && actual_identity != expected_identity {
                return Err(unsafe_storage(
                    "database sidecar changed before writable open",
                ));
            }
            if let Some(file) = &actual {
                harden_database_descriptor(file)?;
            }
            *expected = actual;
        }

        // The file was already created/opened through the restrictive
        // descriptor path above. Omit SQLite's CREATE and URI flags so it
        // cannot create a raced replacement or reinterpret path punctuation.
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE;
        let mut conn = Connection::open_with_flags(path, flags)?;
        if !descriptor_matches_path(&database, path)? {
            return Err(unsafe_storage("database path changed during SQLite open"));
        }

        // Recheck recognized structure on the writable handle before allowing
        // migration. Future, malformed, unrelated, and partial schemas are
        // preserved rather than initialized or "repaired" for startup.
        if !initialize {
            validate_existing_murmur_schema(&conn)?;
        }
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrate(&mut conn)?;
        let journal_mode: String =
            conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(unsafe_storage("SQLite did not enter WAL mode"));
        }
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             BEGIN IMMEDIATE;
             COMMIT;",
        )?;

        // SQLite's Unix VFS derives WAL permissions from the main database,
        // but verify every path before chmod, then harden accepted descriptors
        // and retain their identities for later replacement checks.
        let preflight_identities = std::iter::once(Some(database_identity))
            .chain(
                preflight_sidecars
                    .iter()
                    .map(|file| file.as_ref().map(file_identity).transpose())
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .collect::<Vec<_>>();
        let mut artifacts = Vec::with_capacity(artifact_paths.len());
        for (index, artifact) in artifact_paths.into_iter().enumerate() {
            let opened = open_existing_validated_artifact(&artifact)?;
            let identity = opened.as_ref().map(file_identity).transpose()?;
            if index == 0 && identity != Some(database_identity) {
                return Err(unsafe_storage(
                    "database path was replaced during initialization",
                ));
            }
            if index > 0
                && preflight_identities[index].is_some()
                && identity != preflight_identities[index]
            {
                return Err(unsafe_storage(
                    "database sidecar was replaced during initialization",
                ));
            }
            if let Some(file) = &opened {
                harden_database_descriptor(file)?;
            }
            artifacts.push((artifact, identity));
        }
        if !descriptor_matches_path(&database, path)? {
            return Err(unsafe_storage(
                "database path was replaced during initialization",
            ));
        }

        let boundary = DurableStorageBoundary {
            parent_path,
            parent_identity,
            artifacts,
        };
        boundary.verify()?;
        Ok(Self {
            conn: Mutex::new(conn),
            health: StoreHealth::Durable,
            durable_boundary: Some(boundary),
        })
    }

    /// Open the explicit best-effort in-memory fallback used when the durable
    /// history database is unavailable. Dictation may continue, but meeting
    /// capture creation is rejected by the capture domain.
    pub fn open_ephemeral() -> Result<Self, StoreError> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            health: StoreHealth::Ephemeral {
                reason: "durable_storage_unavailable".into(),
            },
            durable_boundary: None,
        })
    }

    pub fn health(&self) -> StoreHealth {
        if let Some(boundary) = &self.durable_boundary {
            if boundary.verify().is_err() {
                return StoreHealth::Unavailable {
                    reason: "durable_storage_boundary_unavailable".into(),
                };
            }
        }
        self.health.clone()
    }

    pub fn is_durable(&self) -> bool {
        matches!(self.health(), StoreHealth::Durable)
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        let guard = self.conn.lock().map_err(|_| StoreError::Poisoned)?;
        if let Some(boundary) = &self.durable_boundary {
            boundary.verify()?;
        }
        Ok(guard)
    }

    #[cfg(test)]
    pub(crate) fn connection_guard_available(&self) -> bool {
        self.conn.try_lock().is_ok()
    }

    pub fn insert(&self, record: &TranscriptRecord) -> Result<i64, StoreError> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO transcripts
               (created_at, text_raw, text_final, duration_ms,
                word_count, app_name, app_bundle)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.created_at,
                record.text_raw,
                record.text_final,
                record.duration_ms,
                record.word_count,
                record.app_name,
                record.app_bundle,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Newest-first page.
    pub fn list(&self, limit: i64, offset: i64) -> Result<Vec<TranscriptRecord>, StoreError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, created_at, text_raw, text_final, duration_ms,
                    word_count, app_name, app_bundle
             FROM transcripts
             ORDER BY created_at DESC, id DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit, offset], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The latest retained, non-empty dictated text for an explicit menu-bar copy.
    /// Do not fetch raw text, audio, or application metadata for this action.
    pub fn latest_dictation_text(&self) -> Result<Option<String>, StoreError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT text_final FROM transcripts
             WHERE TRIM(text_final, char(9) || char(10) || char(13) || ' ') <> ''
             ORDER BY created_at DESC, id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Full-text search via FTS5. Empty query falls back to
    /// newest-first list, so a UI with a search box doesn't have to
    /// branch on empty input.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<TranscriptRecord>, StoreError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.list(limit, 0);
        }
        let conn = self.connection()?;
        // FTS5 MATCH syntax — wrap in quotes + append `*` to each
        // token so partial words match as prefixes. Prevents the user
        // from having to type the full word before hits appear.
        let match_expr = fts_prefix_expr(trimmed);
        let mut stmt = conn.prepare(
            "SELECT t.id, t.created_at, t.text_raw, t.text_final,
                    t.duration_ms, t.word_count, t.app_name, t.app_bundle
             FROM transcripts t
             JOIN transcripts_fts f ON f.rowid = t.id
             WHERE transcripts_fts MATCH ?1
             ORDER BY t.created_at DESC, t.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![match_expr, limit], row_to_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete(&self, id: i64) -> Result<bool, StoreError> {
        let conn = self.connection()?;
        let changed = conn.execute("DELETE FROM transcripts WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn clear_all(&self) -> Result<u64, StoreError> {
        let conn = self.connection()?;
        let changed = conn.execute("DELETE FROM transcripts", [])?;
        Ok(changed as u64)
    }

    /// Remove every record older than `cutoff_millis`. Returns the
    /// deleted count so the caller can log how much the retention
    /// policy trimmed.
    pub fn purge_older_than(&self, cutoff_millis: i64) -> Result<u64, StoreError> {
        let conn = self.connection()?;
        let changed = conn.execute(
            "DELETE FROM transcripts WHERE created_at < ?1",
            params![cutoff_millis],
        )?;
        Ok(changed as u64)
    }

    pub fn count(&self) -> Result<i64, StoreError> {
        let conn = self.connection()?;
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM transcripts", [], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        Ok(n)
    }

    /// Single-query aggregate over the whole corpus. Drives the Home
    /// stat cards (SPEC §2.9) — total dictated words, total time
    /// spent dictating, total entry count. `COALESCE(..., 0)` keeps
    /// the result shape stable when the store is empty (SUM on zero
    /// rows returns NULL, which would blow up the i64 cast).
    pub fn stats(&self) -> Result<HistoryStats, StoreError> {
        let conn = self.connection()?;
        let (total_entries, total_words, total_duration_ms): (i64, i64, i64) = conn.query_row(
            "SELECT COUNT(*),
                        COALESCE(SUM(word_count), 0),
                        COALESCE(SUM(duration_ms), 0)
                 FROM transcripts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(HistoryStats {
            total_entries,
            total_words,
            total_duration_ms,
        })
    }
}

/// Full-corpus aggregate used to drive the Home stat cards (§2.9).
/// Values are totals over the entire `transcripts` table, so they
/// survive app restart and reflect whatever retention has kept.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryStats {
    pub total_entries: i64,
    pub total_words: i64,
    pub total_duration_ms: i64,
}

/// Each whitespace-separated token → `"token"*` so FTS5 treats it as
/// a prefix search. `"` inside tokens is doubled per FTS5 quoting
/// rules so a user typing `a"b` doesn't blow up the query. AND-joined
/// so all tokens must appear (matches the user's mental model of
/// narrowing results).
fn fts_prefix_expr(query: &str) -> String {
    query
        .split_whitespace()
        .map(|tok| format!("\"{}\"*", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranscriptRecord> {
    Ok(TranscriptRecord {
        id: Some(row.get(0)?),
        created_at: row.get(1)?,
        text_raw: row.get(2)?,
        text_final: row.get(3)?,
        duration_ms: row.get::<_, i64>(4)? as u32,
        word_count: row.get::<_, i64>(5)? as u32,
        app_name: row.get(6)?,
        app_bundle: row.get(7)?,
    })
}

fn schema_has_object(conn: &Connection, object_type: &str, name: &str) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema WHERE type = ?1 AND name = ?2
         )",
        params![object_type, name],
        |row| row.get(0),
    )?)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2
         )",
        params![table, column],
        |row| row.get(0),
    )?)
}

/// Recognize only schemas Murmur itself could have created at a supported
/// version. This runs before any migration or WAL pragma for a nonempty file.
fn validate_existing_murmur_schema(conn: &Connection) -> Result<(), StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    if current == 0 {
        return Err(StoreError::UnrecognizedSchema);
    }

    let quick_check: String = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if !quick_check.eq_ignore_ascii_case("ok") {
        return Err(StoreError::UnrecognizedSchema);
    }

    let mut required_objects = vec![
        ("table", "transcripts"),
        ("table", "transcripts_fts"),
        ("index", "idx_transcripts_created_at"),
        ("trigger", "transcripts_ai"),
        ("trigger", "transcripts_ad"),
        ("trigger", "transcripts_au"),
    ];
    if current >= 2 {
        required_objects.extend([
            ("table", "captures"),
            ("table", "capture_chunks"),
            ("table", "capture_segments"),
            ("table", "captures_fts"),
            ("index", "idx_captures_created_at"),
            ("index", "idx_captures_status"),
            ("index", "idx_capture_segments_timeline"),
            ("trigger", "captures_ai"),
            ("trigger", "captures_ad"),
            ("trigger", "captures_au"),
        ]);
    }
    if current >= 3 {
        required_objects.push(("table", "capture_processing_profiles"));
    }
    if current >= 4 {
        required_objects.push(("index", "idx_captures_audio_expiry"));
    }
    for (kind, name) in required_objects {
        if !schema_has_object(conn, kind, name)? {
            return Err(StoreError::UnrecognizedSchema);
        }
    }

    let mut required_columns = vec![(
        "transcripts",
        &[
            "id",
            "created_at",
            "text_raw",
            "text_final",
            "duration_ms",
            "word_count",
            "app_name",
            "app_bundle",
        ][..],
    )];
    if current >= 2 {
        required_columns.extend([
            (
                "captures",
                &[
                    "id",
                    "kind",
                    "status",
                    "title",
                    "created_at",
                    "updated_at",
                    "started_at",
                    "ended_at",
                    "active_duration_ms",
                    "paused_duration_ms",
                    "source_kind",
                    "source_name",
                    "source_bundle",
                    "language",
                    "word_count",
                    "transcript_text",
                    "markdown_path",
                    "staging_path",
                    "error_code",
                    "error_message",
                    "completed_chunk_count",
                ][..],
            ),
            (
                "capture_chunks",
                &[
                    "id",
                    "capture_id",
                    "ordinal",
                    "track",
                    "start_ms",
                    "end_ms",
                    "path",
                    "byte_length",
                    "status",
                    "error_code",
                ][..],
            ),
            (
                "capture_segments",
                &[
                    "id",
                    "capture_id",
                    "chunk_id",
                    "track",
                    "ordinal",
                    "start_ms",
                    "end_ms",
                    "text_raw",
                    "text_final",
                    "language",
                    "confidence",
                ][..],
            ),
        ]);
    }
    if current >= 3 {
        required_columns.push((
            "capture_processing_profiles",
            &[
                "capture_id",
                "stt_model_id",
                "language",
                "vocabulary_prompt",
                "corrections_json",
                "created_at",
            ][..],
        ));
    }
    if current >= 4 {
        required_columns.push((
            "captures",
            &["audio_retention", "audio_delete_at", "audio_bytes"][..],
        ));
    }
    if current >= 5 {
        required_columns.push(("captures", &["transcription_paused"][..]));
    }
    for (table, columns) in required_columns {
        for column in columns {
            if !table_has_column(conn, table, column)? {
                return Err(StoreError::UnrecognizedSchema);
            }
        }
    }
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let current: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    if current == SCHEMA_VERSION {
        return Ok(());
    }

    // Every schema step and the user_version bump commit together. A failed
    // v2 statement therefore leaves a v1 database fully usable and retryable.
    let tx = conn.transaction()?;
    if current < 1 {
        apply_v1(&tx)?;
    }
    if current < 2 {
        apply_v2(&tx)?;
    }
    if current < 3 {
        apply_v3(&tx)?;
    }
    if current < 4 {
        apply_v4(&tx)?;
    }
    if current < 5 {
        apply_v5(&tx)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

fn apply_v1(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS transcripts (
             id           INTEGER PRIMARY KEY AUTOINCREMENT,
             created_at   INTEGER NOT NULL,
             text_raw     TEXT NOT NULL,
             text_final   TEXT NOT NULL,
             duration_ms  INTEGER NOT NULL,
             word_count   INTEGER NOT NULL,
             app_name     TEXT,
             app_bundle   TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_transcripts_created_at
             ON transcripts(created_at DESC);
         CREATE VIRTUAL TABLE IF NOT EXISTS transcripts_fts USING fts5(
             text_final,
             content='transcripts',
             content_rowid='id'
         );
         CREATE TRIGGER IF NOT EXISTS transcripts_ai
             AFTER INSERT ON transcripts BEGIN
             INSERT INTO transcripts_fts(rowid, text_final)
                 VALUES (new.id, new.text_final);
         END;
         CREATE TRIGGER IF NOT EXISTS transcripts_ad
             AFTER DELETE ON transcripts BEGIN
             INSERT INTO transcripts_fts(transcripts_fts, rowid, text_final)
                 VALUES('delete', old.id, old.text_final);
         END;
         CREATE TRIGGER IF NOT EXISTS transcripts_au
             AFTER UPDATE ON transcripts BEGIN
             INSERT INTO transcripts_fts(transcripts_fts, rowid, text_final)
                 VALUES('delete', old.id, old.text_final);
             INSERT INTO transcripts_fts(rowid, text_final)
                 VALUES (new.id, new.text_final);
         END;",
    )?;
    Ok(())
}

fn apply_v3(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE capture_processing_profiles (
             capture_id        INTEGER PRIMARY KEY
                                REFERENCES captures(id) ON DELETE CASCADE,
             stt_model_id      TEXT NOT NULL,
             language          TEXT,
             vocabulary_prompt TEXT,
             corrections_json  TEXT NOT NULL,
             created_at        INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

fn apply_v5(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "ALTER TABLE captures ADD COLUMN transcription_paused INTEGER NOT NULL DEFAULT 0
            CHECK (transcription_paused IN (0, 1));",
    )?;
    Ok(())
}

fn apply_v4(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "ALTER TABLE captures ADD COLUMN audio_retention TEXT NOT NULL DEFAULT 'failure_preserved'
             CHECK (audio_retention IN ('failure_preserved', 'delete_immediately', 'after_24_hours', 'after_7_days', 'keep'));
         ALTER TABLE captures ADD COLUMN audio_delete_at INTEGER;
         ALTER TABLE captures ADD COLUMN audio_bytes INTEGER NOT NULL DEFAULT 0
             CHECK (audio_bytes >= 0);
         UPDATE captures
            SET audio_retention = 'after_24_hours',
                audio_delete_at = updated_at + 86400000
          WHERE status = 'completed' AND staging_path IS NOT NULL;
         CREATE INDEX idx_captures_audio_expiry
             ON captures(audio_delete_at)
             WHERE staging_path IS NOT NULL AND audio_delete_at IS NOT NULL;",
    )?;
    Ok(())
}

fn apply_v2(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE captures (
             id                    INTEGER PRIMARY KEY AUTOINCREMENT,
             kind                  TEXT NOT NULL CHECK (kind IN ('meeting')),
             status                TEXT NOT NULL CHECK (status IN (
                 'draft', 'selecting_source', 'starting', 'recording',
                 'pausing', 'paused', 'resuming', 'stopping',
                 'transcribing', 'finalizing', 'completed', 'interrupted',
                 'capture_failed', 'transcription_failed',
                 'finalization_failed', 'deleting', 'cleanup_failed',
                 'cancelled'
             )),
             title                 TEXT NOT NULL,
             created_at            INTEGER NOT NULL,
             updated_at            INTEGER NOT NULL,
             started_at            INTEGER,
             ended_at              INTEGER,
             active_duration_ms    INTEGER NOT NULL DEFAULT 0
                                       CHECK (active_duration_ms >= 0),
             paused_duration_ms    INTEGER NOT NULL DEFAULT 0
                                       CHECK (paused_duration_ms >= 0),
             source_kind           TEXT,
             source_name           TEXT,
             source_bundle         TEXT,
             language              TEXT,
             word_count            INTEGER NOT NULL DEFAULT 0
                                       CHECK (word_count >= 0),
             transcript_text       TEXT,
             markdown_path         TEXT,
             staging_path          TEXT,
             error_code            TEXT,
             error_message         TEXT,
             completed_chunk_count INTEGER NOT NULL DEFAULT 0
                                       CHECK (completed_chunk_count >= 0)
         );
         CREATE INDEX idx_captures_created_at
             ON captures(created_at DESC);
         CREATE INDEX idx_captures_status
             ON captures(status);

         CREATE TABLE capture_chunks (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             capture_id  INTEGER NOT NULL
                             REFERENCES captures(id) ON DELETE CASCADE,
             ordinal     INTEGER NOT NULL CHECK (ordinal >= 0),
             track       TEXT NOT NULL
                             CHECK (track IN ('microphone', 'system')),
             start_ms    INTEGER NOT NULL CHECK (start_ms >= 0),
             end_ms      INTEGER NOT NULL CHECK (end_ms >= start_ms),
             path        TEXT NOT NULL,
             byte_length INTEGER NOT NULL DEFAULT 0 CHECK (byte_length >= 0),
             status      TEXT NOT NULL
                             CHECK (status IN (
                                 'open', 'ready', 'transcribing',
                                 'transcribed', 'invalid', 'deleted'
                             )),
             error_code  TEXT,
             UNIQUE(capture_id, ordinal, track)
         );

         CREATE TABLE capture_segments (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             capture_id  INTEGER NOT NULL
                             REFERENCES captures(id) ON DELETE CASCADE,
             chunk_id    INTEGER
                             REFERENCES capture_chunks(id) ON DELETE SET NULL,
             track       TEXT NOT NULL
                             CHECK (track IN ('microphone', 'system')),
             ordinal     INTEGER NOT NULL CHECK (ordinal >= 0),
             start_ms    INTEGER NOT NULL CHECK (start_ms >= 0),
             end_ms      INTEGER NOT NULL CHECK (end_ms >= start_ms),
             text_raw    TEXT NOT NULL,
             text_final  TEXT NOT NULL,
             language    TEXT,
             confidence  REAL,
             UNIQUE(capture_id, track, ordinal)
         );
         CREATE INDEX idx_capture_segments_timeline
             ON capture_segments(capture_id, start_ms, end_ms, id);

         CREATE VIRTUAL TABLE captures_fts USING fts5(
             title,
             transcript_text,
             content='captures',
             content_rowid='id'
         );
         CREATE TRIGGER captures_ai
             AFTER INSERT ON captures BEGIN
             INSERT INTO captures_fts(rowid, title, transcript_text)
                 VALUES (new.id, new.title, new.transcript_text);
         END;
         CREATE TRIGGER captures_ad
             AFTER DELETE ON captures BEGIN
             INSERT INTO captures_fts(
                 captures_fts, rowid, title, transcript_text
             ) VALUES (
                 'delete', old.id, old.title, old.transcript_text
             );
         END;
         CREATE TRIGGER captures_au
             AFTER UPDATE ON captures BEGIN
             INSERT INTO captures_fts(
                 captures_fts, rowid, title, transcript_text
             ) VALUES (
                 'delete', old.id, old.title, old.transcript_text
             );
             INSERT INTO captures_fts(rowid, title, transcript_text)
                 VALUES (new.id, new.title, new.transcript_text);
         END;
         INSERT INTO captures_fts(captures_fts) VALUES('rebuild');",
    )?;
    Ok(())
}

/// User-configurable retention window. 0 = keep forever (default),
/// otherwise the age (in days) past which records are purged on
/// startup and after every successful dictation insert. Frontend is
/// the source of truth; `set_history_retention` mirrors
/// `localStorage["murmur:history-retention"]` into this atomic.
#[derive(Debug, Default)]
pub struct HistoryRetentionState(AtomicU32);

impl HistoryRetentionState {
    pub fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    pub fn set_days(&self, days: u32) {
        self.0.store(days, Ordering::Relaxed);
    }

    pub fn days(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    /// Returns the cutoff timestamp (epoch ms) below which records
    /// should be purged, or `None` when retention is "forever" (the
    /// caller should skip the purge entirely in that case).
    pub fn cutoff_millis(&self) -> Option<i64> {
        let days = self.days();
        if days == 0 {
            return None;
        }
        let window_ms = (days as i64) * 24 * 60 * 60 * 1000;
        Some(now_millis().saturating_sub(window_ms))
    }
}

/// Apply the configured retention window. No-op if retention is
/// "forever". Logs the purged count but does not surface errors —
/// a failed purge should never block dictation.
pub fn purge_per_retention(store: &TranscriptStore, retention: &HistoryRetentionState) {
    let Some(cutoff) = retention.cutoff_millis() else {
        return;
    };
    match store.purge_older_than(cutoff) {
        Ok(0) => {}
        Ok(n) => crate::murmur_dev_log!(
            "[murmur] history: purged {n} record(s) older than {} day(s)",
            retention.days()
        ),
        Err(e) => crate::murmur_dev_log!("[murmur] history: purge failed: {e}"),
    }
}

// ---------------- Tauri commands ----------------

type StoreState<'a> = tauri::State<'a, std::sync::Arc<TranscriptStore>>;
type RetentionStateRef<'a> = tauri::State<'a, std::sync::Arc<HistoryRetentionState>>;

const HISTORY_READ_PUBLIC_ERROR: &str = "Murmur could not read transcript history.";
const HISTORY_WRITE_PUBLIC_ERROR: &str = "Murmur could not update transcript history.";

#[tauri::command]
pub async fn history_list(
    store: StoreState<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<TranscriptRecord>, String> {
    let store = std::sync::Arc::clone(&store);
    tauri::async_runtime::spawn_blocking(move || store.list(limit.max(0), offset.max(0)))
        .await
        .map_err(|_| HISTORY_READ_PUBLIC_ERROR.to_string())?
        .map_err(|_| HISTORY_READ_PUBLIC_ERROR.to_string())
}

#[tauri::command]
pub async fn history_search(
    store: StoreState<'_>,
    query: String,
    limit: i64,
) -> Result<Vec<TranscriptRecord>, String> {
    let store = std::sync::Arc::clone(&store);
    tauri::async_runtime::spawn_blocking(move || store.search(&query, limit.max(0)))
        .await
        .map_err(|_| HISTORY_READ_PUBLIC_ERROR.to_string())?
        .map_err(|_| HISTORY_READ_PUBLIC_ERROR.to_string())
}

#[tauri::command]
pub async fn history_delete(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    id: i64,
) -> Result<bool, String> {
    let store = std::sync::Arc::clone(&store);
    let removed = tauri::async_runtime::spawn_blocking({
        let store = std::sync::Arc::clone(&store);
        move || store.delete(id)
    })
    .await
    .map_err(|_| HISTORY_WRITE_PUBLIC_ERROR.to_string())?
    .map_err(|_| HISTORY_WRITE_PUBLIC_ERROR.to_string())?;
    if removed {
        crate::tray::refresh_dictation_copy_availability(&app, &store);
    }
    Ok(removed)
}

#[tauri::command]
pub async fn history_clear_all(
    app: tauri::AppHandle,
    store: StoreState<'_>,
) -> Result<u64, String> {
    let store = std::sync::Arc::clone(&store);
    let removed = tauri::async_runtime::spawn_blocking({
        let store = std::sync::Arc::clone(&store);
        move || store.clear_all()
    })
    .await
    .map_err(|_| HISTORY_WRITE_PUBLIC_ERROR.to_string())?
    .map_err(|_| HISTORY_WRITE_PUBLIC_ERROR.to_string())?;
    crate::tray::refresh_dictation_copy_availability(&app, &store);
    Ok(removed)
}

#[tauri::command]
pub async fn history_count(store: StoreState<'_>) -> Result<i64, String> {
    let store = std::sync::Arc::clone(&store);
    tauri::async_runtime::spawn_blocking(move || store.count())
        .await
        .map_err(|_| HISTORY_READ_PUBLIC_ERROR.to_string())?
        .map_err(|_| HISTORY_READ_PUBLIC_ERROR.to_string())
}

#[tauri::command]
pub async fn history_stats(store: StoreState<'_>) -> Result<HistoryStats, String> {
    let store = std::sync::Arc::clone(&store);
    tauri::async_runtime::spawn_blocking(move || store.stats())
        .await
        .map_err(|_| HISTORY_READ_PUBLIC_ERROR.to_string())?
        .map_err(|_| HISTORY_READ_PUBLIC_ERROR.to_string())
}

#[tauri::command]
pub fn set_history_retention(
    app: tauri::AppHandle,
    store: StoreState<'_>,
    retention: RetentionStateRef<'_>,
    days: u32,
) {
    retention.set_days(days);
    crate::murmur_dev_log!("[murmur] history retention: {days} day(s) (0 = forever)");
    // Apply the new window immediately — if the user drops from
    // "forever" to "7 days" we want stale rows gone right away, not
    // at the next dictation.
    purge_per_retention(&store, &retention);
    crate::tray::refresh_dictation_copy_availability(&app, &store);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "murmur-history-{label}-{}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn in_memory_store() -> TranscriptStore {
        TranscriptStore::open_ephemeral().unwrap()
    }

    fn sample(text: &str, ts: i64) -> TranscriptRecord {
        TranscriptRecord {
            id: None,
            created_at: ts,
            text_raw: text.to_string(),
            text_final: text.to_string(),
            duration_ms: 1234,
            word_count: count_words(text),
            app_name: Some("TestApp".into()),
            app_bundle: Some("com.test.app".into()),
        }
    }

    #[test]
    fn insert_and_round_trip() {
        let s = in_memory_store();
        let id = s.insert(&sample("hello world", 100)).unwrap();
        assert!(id > 0);

        let rows = s.list(10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text_final, "hello world");
        assert_eq!(rows[0].word_count, 2);
        assert_eq!(rows[0].id, Some(id));
    }

    #[test]
    fn list_is_newest_first() {
        let s = in_memory_store();
        s.insert(&sample("oldest", 100)).unwrap();
        s.insert(&sample("middle", 200)).unwrap();
        s.insert(&sample("newest", 300)).unwrap();

        let rows = s.list(10, 0).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].text_final, "newest");
        assert_eq!(rows[1].text_final, "middle");
        assert_eq!(rows[2].text_final, "oldest");
    }

    #[test]
    fn latest_dictation_copy_uses_retained_final_text_and_follows_deletion() {
        let s = in_memory_store();
        assert_eq!(s.latest_dictation_text().unwrap(), None);
        let first = s.insert(&sample("original", 100)).unwrap();
        let mut corrected = sample("raw speech", 200);
        corrected.text_final = "corrected speech".into();
        let second = s.insert(&corrected).unwrap();
        assert_eq!(
            s.latest_dictation_text().unwrap().as_deref(),
            Some("corrected speech")
        );
        // Empty corrected rows are not copyable and must not hide earlier text.
        let mut empty = sample("removed by correction", 300);
        empty.text_final.clear();
        s.insert(&empty).unwrap();
        let mut whitespace = sample("blank correction", 400);
        whitespace.text_final = " \n\t".into();
        s.insert(&whitespace).unwrap();
        assert_eq!(
            s.latest_dictation_text().unwrap().as_deref(),
            Some("corrected speech")
        );
        s.delete(second).unwrap();
        assert_eq!(
            s.latest_dictation_text().unwrap().as_deref(),
            Some("original")
        );
        s.delete(first).unwrap();
        assert_eq!(s.latest_dictation_text().unwrap(), None);
        s.clear_all().unwrap();
        assert_eq!(s.latest_dictation_text().unwrap(), None);
    }

    #[test]
    fn list_paginates() {
        let s = in_memory_store();
        for i in 0..5 {
            s.insert(&sample(&format!("entry {i}"), (i * 10) as i64))
                .unwrap();
        }
        let page1 = s.list(2, 0).unwrap();
        let page2 = s.list(2, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 2);
        assert_ne!(page1[0].id, page2[0].id);
    }

    #[test]
    fn fts_search_finds_matches() {
        let s = in_memory_store();
        s.insert(&sample("the quick brown fox", 100)).unwrap();
        s.insert(&sample("lazy dog in the yard", 200)).unwrap();
        s.insert(&sample("fox hunting season", 300)).unwrap();

        let hits = s.search("fox", 10).unwrap();
        assert_eq!(hits.len(), 2);
        // Newest-first ordering preserved across FTS matches.
        assert_eq!(hits[0].text_final, "fox hunting season");
    }

    #[test]
    fn fts_search_supports_prefix() {
        let s = in_memory_store();
        s.insert(&sample("transcription quality", 100)).unwrap();
        let hits = s.search("transcr", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn empty_search_returns_recent_list() {
        let s = in_memory_store();
        s.insert(&sample("first", 100)).unwrap();
        s.insert(&sample("second", 200)).unwrap();
        let hits = s.search("   ", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text_final, "second");
    }

    #[test]
    fn delete_removes_one_row() {
        let s = in_memory_store();
        let id = s.insert(&sample("byebye", 100)).unwrap();
        assert!(s.delete(id).unwrap());
        assert!(!s.delete(id).unwrap());
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn delete_keeps_fts_in_sync() {
        let s = in_memory_store();
        let id = s.insert(&sample("ghost", 100)).unwrap();
        s.insert(&sample("living", 200)).unwrap();
        assert!(s.delete(id).unwrap());
        let hits = s.search("ghost", 10).unwrap();
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn clear_all_empties_store() {
        let s = in_memory_store();
        s.insert(&sample("a", 100)).unwrap();
        s.insert(&sample("b", 200)).unwrap();
        let removed = s.clear_all().unwrap();
        assert_eq!(removed, 2);
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn purge_older_than_trims_only_below_cutoff() {
        let s = in_memory_store();
        s.insert(&sample("stale", 100)).unwrap();
        s.insert(&sample("fresh", 500)).unwrap();
        let removed = s.purge_older_than(300).unwrap();
        assert_eq!(removed, 1);
        let rows = s.list(10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text_final, "fresh");
    }

    #[test]
    fn stats_zero_on_empty_store() {
        let s = in_memory_store();
        let stats = s.stats().unwrap();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.total_words, 0);
        assert_eq!(stats.total_duration_ms, 0);
    }

    #[test]
    fn stats_sums_across_corpus() {
        let s = in_memory_store();
        s.insert(&TranscriptRecord {
            id: None,
            created_at: 100,
            text_raw: "a b c".into(),
            text_final: "a b c".into(),
            duration_ms: 1000,
            word_count: 3,
            app_name: None,
            app_bundle: None,
        })
        .unwrap();
        s.insert(&TranscriptRecord {
            id: None,
            created_at: 200,
            text_raw: "d e f g".into(),
            text_final: "d e f g".into(),
            duration_ms: 2500,
            word_count: 4,
            app_name: None,
            app_bundle: None,
        })
        .unwrap();
        let stats = s.stats().unwrap();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.total_words, 7);
        assert_eq!(stats.total_duration_ms, 3500);
    }

    #[test]
    fn count_matches_inserts() {
        let s = in_memory_store();
        assert_eq!(s.count().unwrap(), 0);
        for i in 0..4 {
            s.insert(&sample("x", i as i64)).unwrap();
        }
        assert_eq!(s.count().unwrap(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn new_database_directory_file_and_wal_sidecars_are_private() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = TestDirectory::new("new-private-store");
        let app_data = root.join("nested/app-data");
        let path = app_data.join("murmur.db");
        let store = TranscriptStore::open(&path).unwrap();
        store.insert(&sample("private", 1)).unwrap();

        assert_eq!(store.health(), StoreHealth::Durable);
        assert_eq!(
            std::fs::metadata(&app_data).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for artifact in database_artifact_paths(&path).into_iter().take(3) {
            let metadata = std::fs::symlink_metadata(&artifact).unwrap();
            assert!(metadata.is_file());
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_database_is_hardened_without_losing_rows() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDirectory::new("existing-private-store");
        let path = root.join("murmur.db");
        {
            let store = TranscriptStore::open(&path).unwrap();
            store.insert(&sample("preserved row", 10)).unwrap();
        }
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let reopened = TranscriptStore::open(&path).unwrap();
        assert_eq!(reopened.list(10, 0).unwrap()[0].text_final, "preserved row");
        assert_eq!(
            std::fs::metadata(&root.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_symlink_is_refused_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new("database-symlink");
        let sentinel = root.join("sentinel");
        let original = b"do not open as SQLite";
        std::fs::write(&sentinel, original).unwrap();
        let path = root.join("murmur.db");
        symlink(&sentinel, &path).unwrap();

        assert!(TranscriptStore::open(&path).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), original);
        assert!(std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn database_socket_is_refused_without_blocking_or_replacing_it() {
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::net::UnixListener;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let short_root = std::fs::canonicalize("/tmp")
            .unwrap()
            .join(format!("murmur-socket-{}-{unique}", std::process::id()));
        std::fs::create_dir(&short_root).unwrap();
        let root = TestDirectory(short_root);
        let path = root.join("murmur.db");
        let _listener = UnixListener::bind(&path).unwrap();

        assert!(TranscriptStore::open(&path).is_err());
        assert!(std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_socket());
    }

    #[cfg(unix)]
    #[test]
    fn database_hard_link_is_refused_without_touching_its_target() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDirectory::new("database-hard-link");
        let sentinel = root.join("sentinel");
        let original = b"do not modify";
        std::fs::write(&sentinel, original).unwrap();
        std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o640)).unwrap();
        let path = root.join("murmur.db");
        std::fs::hard_link(&sentinel, &path).unwrap();

        assert!(TranscriptStore::open(&path).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_sqlite_sidecar_paths_are_refused_without_touching_targets() {
        use std::os::unix::fs::symlink;

        for suffix in ["-wal", "-shm", "-journal"] {
            let root = TestDirectory::new(&format!("unsafe-sidecar{suffix}"));
            let sentinel = root.join("sentinel");
            let original = b"sidecar sentinel";
            std::fs::write(&sentinel, original).unwrap();
            let path = root.join("murmur.db");
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            symlink(&sentinel, &sidecar).unwrap();

            assert!(TranscriptStore::open(&path).is_err());
            assert_eq!(std::fs::read(&sentinel).unwrap(), original);
            assert!(std::fs::symlink_metadata(sidecar)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_sqlite_sidecars_are_refused_without_chmod_or_recovery() {
        use std::os::unix::fs::PermissionsExt;

        for suffix in ["-wal", "-shm", "-journal"] {
            let root = TestDirectory::new(&format!("hard-linked-sidecar{suffix}"));
            let sentinel = root.join("sentinel");
            let original = b"sidecar hard-link sentinel";
            std::fs::write(&sentinel, original).unwrap();
            std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o640)).unwrap();
            let path = root.join("murmur.db");
            let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
            std::fs::hard_link(&sentinel, &sidecar).unwrap();

            assert!(TranscriptStore::open(&path).is_err());
            assert_eq!(std::fs::read(&sentinel).unwrap(), original);
            assert_eq!(
                std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_app_data_directory_is_refused_without_creating_database() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new("symlinked-app-data");
        let target = root.join("target");
        std::fs::create_dir(&target).unwrap();
        let app_data = root.join("app-data");
        symlink(&target, &app_data).unwrap();
        let path = app_data.join("murmur.db");

        assert!(TranscriptStore::open(&path).is_err());
        assert!(!target.join("murmur.db").exists());
    }

    #[cfg(unix)]
    #[test]
    fn replacement_after_open_is_detected_without_mutating_new_path() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDirectory::new("database-replacement");
        let path = root.join("murmur.db");
        let detached = root.join("detached.db");
        let store = TranscriptStore::open(&path).unwrap();
        store.insert(&sample("original", 1)).unwrap();
        std::fs::rename(&path, &detached).unwrap();
        let replacement = b"replacement must stay untouched";
        std::fs::write(&path, replacement).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(matches!(
            store.insert(&sample("blocked", 2)),
            Err(StoreError::Storage(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), replacement);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            store.health(),
            StoreHealth::Unavailable {
                reason: "durable_storage_boundary_unavailable".into()
            }
        );

        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&detached, &path).unwrap();
        drop(store);
    }

    #[cfg(unix)]
    #[test]
    fn wal_replacement_is_detected_without_mutating_new_path() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDirectory::new("wal-replacement");
        let path = root.join("murmur.db");
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let detached = root.join("detached.wal");
        let store = TranscriptStore::open(&path).unwrap();
        store.insert(&sample("original", 1)).unwrap();
        std::fs::rename(&wal, &detached).unwrap();
        let replacement = b"replacement WAL must stay untouched";
        std::fs::write(&wal, replacement).unwrap();
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(matches!(store.count(), Err(StoreError::Storage(_))));
        assert_eq!(std::fs::read(&wal).unwrap(), replacement);
        assert_eq!(
            std::fs::metadata(&wal).unwrap().permissions().mode() & 0o777,
            0o644
        );

        std::fs::remove_file(&wal).unwrap();
        std::fs::rename(&detached, &wal).unwrap();
        drop(store);
    }

    #[cfg(unix)]
    #[test]
    fn parent_replacement_is_detected_without_mutating_new_directory() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDirectory::new("parent-replacement");
        let app_data = root.join("app-data");
        let path = app_data.join("murmur.db");
        let detached = root.join("detached-app-data");
        let store = TranscriptStore::open(&path).unwrap();
        store.insert(&sample("original", 1)).unwrap();
        std::fs::rename(&app_data, &detached).unwrap();
        std::fs::create_dir(&app_data).unwrap();
        std::fs::set_permissions(&app_data, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(matches!(store.count(), Err(StoreError::Storage(_))));
        assert_eq!(
            std::fs::metadata(&app_data).unwrap().permissions().mode() & 0o777,
            0o755
        );
        drop(store);
    }

    #[test]
    fn store_health_and_errors_do_not_serialize_sensitive_paths() {
        let durable_health = serde_json::to_string(&StoreHealth::Durable).unwrap();
        assert_eq!(durable_health, r#"{"kind":"durable"}"#);
        assert!(!durable_health.contains("path"));

        let ephemeral = TranscriptStore::open_ephemeral().unwrap();
        let health = serde_json::to_string(&ephemeral.health()).unwrap();
        assert_eq!(
            health,
            r#"{"kind":"ephemeral","reason":"durable_storage_unavailable"}"#
        );
        assert!(!health.contains("/Users/Private"));

        let unavailable = serde_json::to_string(&StoreHealth::Unavailable {
            reason: "durable_storage_boundary_unavailable".into(),
        })
        .unwrap();
        assert_eq!(
            unavailable,
            r#"{"kind":"unavailable","reason":"durable_storage_boundary_unavailable"}"#
        );
        assert!(!unavailable.contains("/Users/Private"));

        let error = StoreError::Storage(io::Error::other("/Users/Private/murmur.db"));
        assert_eq!(
            error.to_string(),
            "durable storage boundary validation failed"
        );
        assert!(!error.to_string().contains("/Users/Private"));
    }

    #[test]
    fn relative_database_path_is_refused() {
        assert!(matches!(
            TranscriptStore::open(Path::new("relative/murmur.db")),
            Err(StoreError::Storage(_))
        ));
    }

    #[test]
    fn unsupported_existing_database_is_preserved_without_wal_initialization() {
        let root = TestDirectory::new("future-database");
        let path = root.join("murmur.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }
        let original = std::fs::read(&path).unwrap();

        assert!(matches!(
            TranscriptStore::open(&path),
            Err(StoreError::UnsupportedSchema { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", path.display())).exists());
    }

    #[test]
    fn malformed_existing_database_is_preserved() {
        let root = TestDirectory::new("malformed-database");
        let path = root.join("murmur.db");
        let original = b"not a SQLite database; preserve for inspection";
        std::fs::write(&path, original).unwrap();

        assert!(matches!(
            TranscriptStore::open(&path),
            Err(StoreError::Sqlite(_))
        ));
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn valid_unrelated_database_is_refused_without_changes_or_sidecars() {
        let root = TestDirectory::new("unrelated-database");
        let path = root.join("murmur.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("CREATE TABLE unrelated (secret TEXT)", [])
                .unwrap();
            conn.execute("INSERT INTO unrelated VALUES ('preserve me')", [])
                .unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let original = std::fs::read(&path).unwrap();

        assert!(matches!(
            TranscriptStore::open(&path),
            Err(StoreError::UnrecognizedSchema)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
        assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", path.display())).exists());
    }

    #[test]
    fn claimed_current_but_partial_schema_is_refused_without_changes_or_sidecars() {
        let root = TestDirectory::new("partial-current-database");
        let path = root.join("murmur.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("CREATE TABLE transcripts (id INTEGER PRIMARY KEY)", [])
                .unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                .unwrap();
        }
        let original = std::fs::read(&path).unwrap();

        assert!(matches!(
            TranscriptStore::open(&path),
            Err(StoreError::UnrecognizedSchema)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());
        assert!(!PathBuf::from(format!("{}-shm", path.display())).exists());
    }

    #[cfg(not(unix))]
    #[test]
    fn durable_writes_do_not_invalidate_non_unix_boundary_identity() {
        let root = TestDirectory::new("non-unix-durable-sequence");
        let path = root.join("murmur.db");
        let store = TranscriptStore::open(&path).unwrap();
        store.insert(&sample("first", 1)).unwrap();
        store.insert(&sample("second", 2)).unwrap();
        assert_eq!(store.count().unwrap(), 2);
        assert_eq!(store.list(10, 0).unwrap().len(), 2);
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn v1_fixture_migrates_without_losing_dictation_or_fts() {
        let root = TestDirectory::new("v1-migration");
        let path = root.join("murmur.db");
        {
            let conn = Connection::open(&path).unwrap();
            apply_v1(&conn).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO transcripts
                    (created_at, text_raw, text_final, duration_ms,
                     word_count, app_name, app_bundle)
                 VALUES (100, 'fixture raw', 'durable fixture phrase',
                         1234, 3, 'Fixture', 'com.test.fixture')",
                [],
            )
            .unwrap();
        }

        let store = TranscriptStore::open(&path).unwrap();
        let rows = store.list(10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text_final, "durable fixture phrase");
        assert_eq!(store.search("fixture", 10).unwrap().len(), 1);
    }

    #[test]
    fn failed_v2_migration_rolls_back_schema_and_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_v1(&conn).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO transcripts
                (created_at, text_raw, text_final, duration_ms,
                 word_count, app_name, app_bundle)
             VALUES (1, 'raw', 'preserved', 1, 1, NULL, NULL)",
            [],
        )
        .unwrap();
        // Fail late in apply_v2, after the three base tables and indexes have
        // been created inside the migration transaction.
        conn.execute("CREATE TABLE captures_fts (broken INTEGER)", [])
            .unwrap();

        assert!(migrate(&mut conn).is_err());
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let capture_table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN ('captures', 'capture_chunks', 'capture_segments')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let transcript_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM transcripts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        assert_eq!(capture_table_count, 0);
        assert_eq!(transcript_count, 1);
    }

    #[test]
    fn v2_migrates_to_v3_without_fabricating_profiles() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_v1(&conn).unwrap();
        apply_v2(&conn).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "INSERT INTO captures (kind, status, title, created_at, updated_at)
             VALUES ('meeting', 'transcribing', 'legacy', 1, 1)",
            [],
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let profiles: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM capture_processing_profiles",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let captures: i64 = conn
            .query_row("SELECT COUNT(*) FROM captures", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(profiles, 0);
        assert_eq!(captures, 1);
    }

    #[test]
    fn v3_migrates_to_v4_with_failure_safe_audio_defaults() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_v1(&conn).unwrap();
        apply_v2(&conn).unwrap();
        apply_v3(&conn).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute(
            "INSERT INTO captures (kind, status, title, created_at, updated_at, staging_path)
             VALUES ('meeting', 'interrupted', 'legacy', 1, 1, '/private/legacy')",
            [],
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let values: (String, Option<i64>, i64) = conn
            .query_row(
                "SELECT audio_retention, audio_delete_at, audio_bytes FROM captures",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(values, ("failure_preserved".into(), None, 0));
    }

    #[test]
    fn v4_migrates_to_v5_without_pausing_existing_meetings() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_v1(&conn).unwrap();
        apply_v2(&conn).unwrap();
        apply_v3(&conn).unwrap();
        apply_v4(&conn).unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn.execute(
            "INSERT INTO captures (kind, status, title, created_at, updated_at)
             VALUES ('meeting', 'transcribing', 'legacy meeting', 1, 1)",
            [],
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        let (version, paused, title): (u32, i64, String) = conn.query_row(
            "SELECT (SELECT user_version FROM pragma_user_version), transcription_paused, title FROM captures",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(
            (version, paused, title),
            (SCHEMA_VERSION, 0, "legacy meeting".into())
        );
    }

    #[test]
    fn failed_v3_migration_rolls_back_schema_and_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_v1(&conn).unwrap();
        apply_v2(&conn).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute(
            "CREATE TABLE capture_processing_profiles (broken INTEGER)",
            [],
        )
        .unwrap();
        assert!(migrate(&mut conn).is_err());
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
    }

    #[test]
    fn newer_schema_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            migrate(&mut conn),
            Err(StoreError::UnsupportedSchema { .. })
        ));
    }

    #[test]
    fn fts_expression_escapes_quotes() {
        assert_eq!(fts_prefix_expr("foo"), "\"foo\"*");
        assert_eq!(fts_prefix_expr("foo bar"), "\"foo\"* AND \"bar\"*");
        assert_eq!(fts_prefix_expr("a\"b"), "\"a\"\"b\"*");
    }
}
