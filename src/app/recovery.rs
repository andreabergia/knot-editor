//! Durable unsaved-document recovery: application-owned protection for dirty documents.
//!
//! The store is one application-owned `recovery.sqlite3` beside the tool-layout
//! database. It holds a versioned `snapshots` table with two retention roles:
//! `automatic` rows are the replaceable protection for a live dirty document
//! (one row per document UUID), and `retained` rows are immutable checkpoints
//! owned by a suspended session or another extension owner.
//!
//! Captures copy text on the foreground when a document model notifies and
//! commit off it. Each document coalesces through a trailing-edge debounce and
//! fires at least every five seconds while typing continues. Every fired
//! capture is written and committed with `FULL` synchronization before it is
//! considered accepted, so the advertised crash-loss window is the cadence
//! itself. The quit path drains a capture still inside its debounce window.
//!
//! Restore rebuilds a document from a snapshot by loading the current resource
//! through its provider as revision 0 and applying the recovered text as one
//! edit with an empty undo history, so dirty state derives from the ordinary
//! content comparison. Restore never writes a resource and never replaces a
//! live document: when the identity or URI is already owned, the recovered text
//! opens as a separate untitled document. Startup silently restores every
//! automatic snapshot that no live document claims; everything else stays in
//! the store for the native listing to restore or discard explicitly.
//!
//! A missing, corrupt, or incompatible database is preserved and reported; it
//! never blocks editing and is never replaced.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, ensure};
use gpui::{
    App, AppContext as _, AsyncApp, BackgroundExecutor, Entity, Global, Subscription, Task,
};
use rusqlite::{Connection, OpenFlags, params, types::ToSql};

use super::{
    documents::{ApplicationDocuments, Document, DocumentId, DocumentState},
    filesystem::{FileSystemProviderRegistry, ResourceKind, ResourceVersion},
    model::BufferModel,
    open::{OpenResource, OpenResourceError, load_resource_kind},
    resource::ResourceUri,
};

pub(crate) const RECOVERY_VERSION: u32 = 1;
const APPLICATION_ID: i64 = 0x4b4e5254;

const SCHEMA: &str = "CREATE TABLE snapshots(\
kind TEXT NOT NULL CHECK(kind IN ('automatic','retained')),\
capture_key TEXT NOT NULL,\
owner TEXT,\
document_id TEXT NOT NULL,\
title TEXT NOT NULL,\
resource_uri TEXT,\
state TEXT NOT NULL CHECK(state IN ('untitled','destination','persisted')),\
provider_version BLOB,\
generation INTEGER NOT NULL,\
captured_at INTEGER NOT NULL,\
text TEXT NOT NULL,\
PRIMARY KEY(kind, capture_key)) STRICT";

/// Trailing-edge settle delay: a capture fires this long after edits stop.
const SETTLE: Duration = Duration::from_millis(1000);
/// Upper bound on a continuous typing burst without a durable capture.
const MAX_INTERVAL: Duration = Duration::from_millis(5000);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SnapshotKind {
    Automatic,
    Retained,
}

impl SnapshotKind {
    fn as_column(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Retained => "retained",
        }
    }

    fn from_column(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "retained" => Some(Self::Retained),
            _ => None,
        }
    }
}

/// The persistence relationship captured for a document.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SnapshotState {
    Untitled,
    Destination,
    Persisted,
}

impl SnapshotState {
    fn as_column(self) -> &'static str {
        match self {
            Self::Untitled => "untitled",
            Self::Destination => "destination",
            Self::Persisted => "persisted",
        }
    }

    fn from_column(value: &str) -> Option<Self> {
        match value {
            "untitled" => Some(Self::Untitled),
            "destination" => Some(Self::Destination),
            "persisted" => Some(Self::Persisted),
            _ => None,
        }
    }
}

/// Foreground-copied snapshot content ready to be written.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SnapshotDraft {
    pub(crate) document_id: uuid::Uuid,
    pub(crate) title: String,
    pub(crate) resource_uri: Option<String>,
    pub(crate) state: SnapshotState,
    pub(crate) provider_version: Option<Vec<u8>>,
    pub(crate) text: String,
    pub(crate) captured_at_ms: i64,
}

impl SnapshotDraft {
    /// Content identity used to skip identical captures. The capture timestamp
    /// is deliberately excluded: it records when a snapshot was taken, not what
    /// it contains.
    fn digest(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.document_id.hash(&mut hasher);
        self.title.hash(&mut hasher);
        self.resource_uri.hash(&mut hasher);
        self.state.hash(&mut hasher);
        self.provider_version.hash(&mut hasher);
        self.text.hash(&mut hasher);
        hasher.finish()
    }
}

/// One durable snapshot row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredSnapshot {
    pub(crate) kind: SnapshotKind,
    pub(crate) capture_key: String,
    pub(crate) owner: Option<String>,
    pub(crate) document_id: uuid::Uuid,
    pub(crate) title: String,
    pub(crate) resource_uri: Option<String>,
    pub(crate) state: SnapshotState,
    pub(crate) provider_version: Option<Vec<u8>>,
    pub(crate) generation: u64,
    pub(crate) captured_at_ms: i64,
    pub(crate) text: String,
}

#[derive(Clone, Debug)]
enum Intent {
    Write(SnapshotDraft),
    Delete(uuid::Uuid),
}

impl Intent {
    fn matches(&self, other: &Intent) -> bool {
        match (self, other) {
            (Intent::Write(left), Intent::Write(right)) => left.digest() == right.digest(),
            (Intent::Delete(left), Intent::Delete(right)) => left == right,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Applied {
    Written(u64),
    Deleted,
}

fn applied_for(intent: &Intent) -> Applied {
    match intent {
        Intent::Write(draft) => Applied::Written(draft.digest()),
        Intent::Delete(_) => Applied::Deleted,
    }
}

struct Pending {
    intent: Intent,
    token: u64,
    burst_start: Instant,
    due: Instant,
}

/// A lazily opened, serialized connection to the recovery database.
struct Database {
    path: PathBuf,
    connection: Mutex<Option<Connection>>,
    next_generation: AtomicU64,
}

impl Database {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            connection: Mutex::new(None),
            next_generation: AtomicU64::new(0),
        }
    }

    fn run<T>(&self, work: impl FnOnce(&mut Connection) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let mut connection = self.connection.lock().unwrap();
        if connection.is_none() {
            *connection = Some(self.open()?);
        }
        work(connection.as_mut().unwrap())
    }

    fn allocate_generation(&self) -> u64 {
        self.next_generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn open(&self) -> anyhow::Result<Connection> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open recovery database {}", self.path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let transaction = connection.transaction()?;
        let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let application: i64 =
            transaction.query_row("PRAGMA application_id", [], |row| row.get(0))?;
        let objects: Vec<(String, String)> = transaction
            .prepare(
                "SELECT name, sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name",
            )?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        if version == 0 && application == 0 && objects.is_empty() {
            transaction.execute_batch(SCHEMA)?;
            transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
            transaction.pragma_update(None, "user_version", RECOVERY_VERSION)?;
        } else {
            ensure!(
                version == RECOVERY_VERSION as i64
                    && application == APPLICATION_ID
                    && objects == [("snapshots".to_owned(), SCHEMA.to_owned())],
                "unsupported recovery schema (application {application}, version {version})"
            );
        }
        let maximum: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(generation), 0) FROM snapshots",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        // Ordinary rollback journaling and FULL synchronization make success a durable commit.
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        self.next_generation
            .fetch_max(maximum as u64, Ordering::Relaxed);
        Ok(connection)
    }
}

struct StoreState {
    last_error: Option<String>,
    pending: HashMap<uuid::Uuid, Pending>,
    applied: HashMap<uuid::Uuid, Applied>,
    next_token: u64,
}

struct Inner {
    database: Database,
    executor: BackgroundExecutor,
    settle: Duration,
    max_interval: Duration,
    state: Mutex<StoreState>,
}

/// Application-owned recovery persistence boundary.
#[derive(Clone)]
pub(crate) struct RecoveryStore {
    inner: Arc<Inner>,
}

impl RecoveryStore {
    pub(crate) fn user_path() -> Result<PathBuf, String> {
        directories::ProjectDirs::from("", "", "Knot")
            .map(|dirs| dirs.data_local_dir().join("recovery.sqlite3"))
            .ok_or_else(|| "cannot determine the user application-data directory".into())
    }

    pub(crate) fn open(path: PathBuf, executor: BackgroundExecutor) -> Self {
        Self::open_with_cadence(path, executor, SETTLE, MAX_INTERVAL)
    }

    fn open_with_cadence(
        path: PathBuf,
        executor: BackgroundExecutor,
        settle: Duration,
        max_interval: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                database: Database::new(path),
                executor,
                settle,
                max_interval,
                state: Mutex::new(StoreState {
                    last_error: None,
                    pending: HashMap::new(),
                    applied: HashMap::new(),
                    next_token: 0,
                }),
            }),
        }
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "write failures stay observable in diagnostics")
    )]
    pub(crate) fn last_error(&self) -> Option<String> {
        self.inner.state.lock().unwrap().last_error.clone()
    }

    /// Replace the automatic protection for a dirty document, coalescing through
    /// the debounce. Identical captures are skipped.
    pub(crate) fn submit_automatic(&self, draft: SnapshotDraft) {
        self.schedule(draft.document_id, Intent::Write(draft));
    }

    /// Retire the automatic protection for a document that is no longer dirty.
    pub(crate) fn retire_automatic(&self, document_id: uuid::Uuid) {
        self.schedule(document_id, Intent::Delete(document_id));
    }

    /// Retain an immutable checkpoint under an opaque owner key.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "retention is exposed through the checkpoint 7c API"
        )
    )]
    pub(crate) fn retain(
        &self,
        owner: String,
        capture_key: String,
        draft: SnapshotDraft,
    ) -> Task<Result<bool, String>> {
        let this = self.clone();
        self.inner.executor.spawn(async move {
            this.inner
                .database
                .run(|connection| {
                    let generation = this.inner.database.allocate_generation();
                    write_retained(connection, &owner, &capture_key, &draft, generation)
                })
                .map_err(display_error)
        })
    }

    /// Release a retained checkpoint. Automatic rows are never affected.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "retention is exposed through the checkpoint 7c API"
        )
    )]
    pub(crate) fn release(&self, owner: String, capture_key: String) -> Task<Result<bool, String>> {
        let this = self.clone();
        self.inner.executor.spawn(async move {
            this.inner
                .database
                .run(|connection| {
                    let removed = connection.execute(
                        "DELETE FROM snapshots WHERE kind = 'retained' AND owner = ?1 AND capture_key = ?2",
                        params![owner, capture_key],
                    )?;
                    Ok(removed > 0)
                })
                .map_err(display_error)
        })
    }

    /// Drain a capture still inside its debounce window. The quit path awaits it.
    pub(crate) fn flush(&self) -> Task<Result<(), String>> {
        let this = self.clone();
        self.inner.executor.spawn(async move {
            let ids: Vec<uuid::Uuid> = {
                let state = this.inner.state.lock().unwrap();
                state.pending.keys().copied().collect()
            };
            let mut outcome = Ok(());
            for id in ids {
                let next = {
                    let state = this.inner.state.lock().unwrap();
                    state
                        .pending
                        .get(&id)
                        .map(|pending| (pending.intent.clone(), pending.token))
                };
                let Some((intent, token)) = next else {
                    continue;
                };
                match this
                    .inner
                    .database
                    .run(|connection| apply(connection, &this.inner.database, &intent))
                {
                    Ok(()) => {
                        let mut state = this.inner.state.lock().unwrap();
                        if state.pending.get(&id).map(|pending| pending.token) == Some(token) {
                            state.pending.remove(&id);
                        }
                        state.applied.insert(id, applied_for(&intent));
                        state.last_error = None;
                    }
                    Err(error) => {
                        let message = display_error(error);
                        let mut state = this.inner.state.lock().unwrap();
                        state.last_error = Some(message.clone());
                        outcome = Err(message);
                        // Leave the failed intent for a later capture or flush;
                        // a damaged database must not block the quit path.
                        if let Some(pending) = state.pending.get_mut(&id) {
                            pending.due = Instant::now();
                        }
                    }
                }
            }
            outcome
        })
    }

    #[cfg(test)]
    pub(crate) fn automatic(
        &self,
        document_id: uuid::Uuid,
    ) -> Task<Result<Option<StoredSnapshot>, String>> {
        self.snapshot(SnapshotKind::Automatic, document_id.to_string())
    }

    /// Read one snapshot by its retention role and capture key.
    pub(crate) fn snapshot(
        &self,
        kind: SnapshotKind,
        capture_key: String,
    ) -> Task<Result<Option<StoredSnapshot>, String>> {
        let this = self.clone();
        self.inner.executor.spawn(async move {
            this.inner
                .database
                .run(|connection| {
                    read_rows(
                        connection,
                        "SELECT kind, capture_key, owner, document_id, title, resource_uri, state, \
                         provider_version, generation, captured_at, text FROM snapshots \
                         WHERE kind = ?1 AND capture_key = ?2",
                        &[&kind.as_column(), &capture_key],
                    )
                    .map(|mut rows| rows.pop())
                })
                .map_err(display_error)
        })
    }

    /// Every stored snapshot, newest capture first.
    pub(crate) fn snapshots(&self) -> Task<Result<Vec<StoredSnapshot>, String>> {
        let this = self.clone();
        self.inner.executor.spawn(async move {
            this.inner
                .database
                .run(|connection| {
                    read_rows(
                        connection,
                        "SELECT kind, capture_key, owner, document_id, title, resource_uri, state, \
                         provider_version, generation, captured_at, text FROM snapshots \
                         ORDER BY captured_at DESC, generation DESC",
                        &[],
                    )
                })
                .map_err(display_error)
        })
    }

    /// Explicitly discard one snapshot of either role. Resolves `false` when the
    /// snapshot no longer exists.
    pub(crate) fn discard(
        &self,
        kind: SnapshotKind,
        capture_key: String,
    ) -> Task<Result<bool, String>> {
        let this = self.clone();
        self.inner.executor.spawn(async move {
            let removed = this
                .inner
                .database
                .run(|connection| {
                    Ok(connection.execute(
                        "DELETE FROM snapshots WHERE kind = ?1 AND capture_key = ?2",
                        params![kind.as_column(), capture_key],
                    )? > 0)
                })
                .map_err(display_error)?;
            if kind == SnapshotKind::Automatic
                && let Ok(document_id) = uuid::Uuid::parse_str(&capture_key)
            {
                this.inner
                    .state
                    .lock()
                    .unwrap()
                    .applied
                    .remove(&document_id);
            }
            Ok(removed)
        })
    }

    /// Move automatic protection to another document identity in one durable
    /// transaction, so the text is never unprotected between the two rows. A
    /// failure keeps the original row.
    fn transfer_automatic(
        &self,
        from: uuid::Uuid,
        draft: SnapshotDraft,
    ) -> Task<Result<(), String>> {
        let this = self.clone();
        self.inner.executor.spawn(async move {
            let result = this
                .inner
                .database
                .run(|connection| {
                    let generation = this.inner.database.allocate_generation();
                    let transaction = connection.transaction()?;
                    write_automatic(&transaction, &draft, generation)?;
                    transaction.execute(
                        "DELETE FROM snapshots WHERE kind = 'automatic' AND capture_key = ?1",
                        params![from.to_string()],
                    )?;
                    transaction.commit()?;
                    Ok(())
                })
                .map_err(display_error);
            let mut state = this.inner.state.lock().unwrap();
            match &result {
                Ok(()) => {
                    state.applied.remove(&from);
                    state
                        .applied
                        .insert(draft.document_id, Applied::Written(draft.digest()));
                }
                Err(message) => state.last_error = Some(message.clone()),
            }
            result
        })
    }

    fn schedule(&self, document_id: uuid::Uuid, intent: Intent) {
        let now = Instant::now();
        let mut state = self.inner.state.lock().unwrap();
        if let Some(pending) = state.pending.get(&document_id) {
            if pending.intent.matches(&intent) {
                return;
            }
        } else if state.applied.get(&document_id) == Some(&applied_for(&intent)) {
            return;
        }
        let burst_start = state
            .pending
            .get(&document_id)
            .map(|pending| pending.burst_start)
            .unwrap_or(now);
        let due = match intent {
            // Saving or discarding a document is a discrete event, not typing.
            Intent::Delete(_) => now,
            Intent::Write(_) => {
                next_due(now, burst_start, self.inner.settle, self.inner.max_interval)
            }
        };
        state.next_token = state.next_token.wrapping_add(1);
        let token = state.next_token;
        state.pending.insert(
            document_id,
            Pending {
                intent,
                token,
                burst_start,
                due,
            },
        );
        drop(state);
        let delay = due.saturating_duration_since(now);
        let this = self.clone();
        self.inner
            .executor
            .spawn(async move {
                this.inner.executor.timer(delay).await;
                this.write_if_current(document_id, token);
            })
            .detach();
    }

    fn write_if_current(&self, document_id: uuid::Uuid, token: u64) {
        let intent = {
            let mut state = self.inner.state.lock().unwrap();
            match state.pending.get(&document_id) {
                Some(pending) if pending.token == token => state
                    .pending
                    .remove(&document_id)
                    .map(|pending| pending.intent),
                _ => None,
            }
        };
        let Some(intent) = intent else {
            return;
        };
        match self
            .inner
            .database
            .run(|connection| apply(connection, &self.inner.database, &intent))
        {
            Ok(()) => {
                let mut state = self.inner.state.lock().unwrap();
                state.applied.insert(document_id, applied_for(&intent));
                state.last_error = None;
            }
            Err(error) => {
                let mut state = self.inner.state.lock().unwrap();
                state.last_error = Some(display_error(error));
                // Keep the intent pending so a later capture or the quit flush retries.
                if let Some(pending) = state.pending.get_mut(&document_id) {
                    pending.due = Instant::now();
                    pending.token = token;
                } else {
                    state.pending.insert(
                        document_id,
                        Pending {
                            intent,
                            token,
                            burst_start: Instant::now(),
                            due: Instant::now(),
                        },
                    );
                }
            }
        }
    }
}

/// Earliest time a coalesced capture may fire: the settle delay after the latest
/// change, but never later than the maximum interval since the burst began.
fn next_due(
    now: Instant,
    burst_start: Instant,
    settle: Duration,
    max_interval: Duration,
) -> Instant {
    (now + settle).min(burst_start + max_interval)
}

fn apply(connection: &Connection, database: &Database, intent: &Intent) -> anyhow::Result<()> {
    match intent {
        Intent::Write(draft) => {
            let generation = database.allocate_generation();
            write_automatic(connection, draft, generation)
        }
        Intent::Delete(document_id) => {
            connection.execute(
                "DELETE FROM snapshots WHERE kind = 'automatic' AND capture_key = ?1",
                params![document_id.to_string()],
            )?;
            Ok(())
        }
    }
}

fn write_automatic(
    connection: &Connection,
    draft: &SnapshotDraft,
    generation: u64,
) -> anyhow::Result<()> {
    let key = draft.document_id.to_string();
    connection.execute(
        "INSERT INTO snapshots(kind, capture_key, owner, document_id, title, resource_uri, state, \
         provider_version, generation, captured_at, text) \
         VALUES('automatic', ?1, NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(kind, capture_key) DO UPDATE SET \
         owner = NULL, document_id = excluded.document_id, title = excluded.title, \
         resource_uri = excluded.resource_uri, state = excluded.state, \
         provider_version = excluded.provider_version, generation = excluded.generation, \
         captured_at = excluded.captured_at, text = excluded.text",
        params![
            key,
            draft.title,
            draft.resource_uri,
            draft.state.as_column(),
            draft.provider_version,
            generation as i64,
            draft.captured_at_ms,
            draft.text,
        ],
    )?;
    Ok(())
}

fn write_retained(
    connection: &Connection,
    owner: &str,
    capture_key: &str,
    draft: &SnapshotDraft,
    generation: u64,
) -> anyhow::Result<bool> {
    // Retained rows are immutable: an existing key is never overwritten.
    let inserted = connection.execute(
        "INSERT OR IGNORE INTO snapshots(kind, capture_key, owner, document_id, title, \
         resource_uri, state, provider_version, generation, captured_at, text) \
         VALUES('retained', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            capture_key,
            owner,
            draft.document_id.to_string(),
            draft.title,
            draft.resource_uri,
            draft.state.as_column(),
            draft.provider_version,
            generation as i64,
            draft.captured_at_ms,
            draft.text,
        ],
    )?;
    Ok(inserted > 0)
}

fn read_rows(
    connection: &Connection,
    sql: &str,
    parameters: &[&dyn ToSql],
) -> anyhow::Result<Vec<StoredSnapshot>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map(parameters, |row| {
            let kind = row.get::<_, String>(0)?;
            let state = row.get::<_, String>(6)?;
            let document_id = row.get::<_, String>(3)?;
            Ok((
                kind,
                state,
                document_id,
                (
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                    row.get::<_, i64>(8)? as u64,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                ),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(kind, state, document_id, rest)| {
            let document_id = uuid::Uuid::parse_str(&document_id)
                .map_err(|error| anyhow::anyhow!("invalid document identity: {error}"))?;
            let kind = SnapshotKind::from_column(&kind)
                .ok_or_else(|| anyhow::anyhow!("invalid snapshot kind: {kind}"))?;
            let state = SnapshotState::from_column(&state)
                .ok_or_else(|| anyhow::anyhow!("invalid snapshot state: {state}"))?;
            Ok(StoredSnapshot {
                kind,
                capture_key: rest.0,
                owner: rest.1,
                document_id,
                title: rest.2,
                resource_uri: rest.3,
                state,
                provider_version: rest.4,
                generation: rest.5,
                captured_at_ms: rest.6,
                text: rest.7,
            })
        })
        .collect()
}

fn display_error(error: anyhow::Error) -> String {
    format!("recovery: {error:#}")
}

/// Attach recovery observation to a newly created document.
pub(crate) fn observe_document(model: &Entity<BufferModel>, identity: uuid::Uuid, cx: &mut App) {
    let Some(service) = cx
        .try_global::<ApplicationRecovery>()
        .map(|global| global.0.clone())
    else {
        return;
    };
    service.observe(model, identity, cx);
}

/// Publish a document's current dirty state or retire its automatic record.
pub(crate) fn publish(document: &Document, cx: &mut App) {
    let Some(service) = cx
        .try_global::<ApplicationRecovery>()
        .map(|global| global.0.clone())
    else {
        return;
    };
    match gather(document, cx) {
        Some(draft) => service.store.submit_automatic(draft),
        None => service.store.retire_automatic(document.identity()),
    }
}

/// Retire the automatic record for an explicitly discarded document that stays open.
pub(crate) fn discard(document_id: uuid::Uuid, cx: &mut App) {
    let Some(service) = cx
        .try_global::<ApplicationRecovery>()
        .map(|global| global.0.clone())
    else {
        return;
    };
    service.store.retire_automatic(document_id);
}

/// Drop observation and retire the automatic record when a document is removed.
pub(crate) fn forget_document(identity: uuid::Uuid, cx: &mut App) {
    let Some(service) = cx
        .try_global::<ApplicationRecovery>()
        .map(|global| global.0.clone())
    else {
        return;
    };
    service.subscriptions.borrow_mut().remove(&identity);
    service.store.retire_automatic(identity);
}

/// How a restored document relates to its resource at restore time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RestoredResource {
    /// No resource: an untitled document.
    Untitled,
    /// The resource is as it was captured, or the destination is still absent.
    Unchanged,
    /// The persisted resource no longer exists; the document targets it as a
    /// destination that a save creates.
    Missing,
    /// The resource changed or appeared after capture. The recovered text sits
    /// dirty on top of the current content and the capture-time version is kept,
    /// so a later save reaches the ordinary conflict handling.
    Changed,
}

/// Committed result of one restore.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RestoreOutcome {
    Restored {
        document: DocumentId,
        resource: RestoredResource,
    },
    /// The snapshot is the automatic protection of a document that is open.
    AlreadyOpen { document: DocumentId },
    /// A live document already owns the identity or URI. It is left untouched and
    /// the recovered text opens as a separate untitled document.
    Preserved {
        owner: DocumentId,
        document: DocumentId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RestoreError {
    /// The snapshot was discarded, released, or restored elsewhere.
    Unavailable,
    /// The resource baseline could not be read; the snapshot is kept.
    Resource(OpenResourceError),
    Store(String),
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => {
                formatter.write_str("the recovered document is no longer available")
            }
            Self::Resource(error) => error.fmt(formatter),
            Self::Store(message) => formatter.write_str(message),
        }
    }
}

fn service(cx: &App) -> Option<RecoveryService> {
    cx.try_global::<ApplicationRecovery>()
        .map(|global| global.0.clone())
}

/// Snapshots the user can restore or discard: every retained capture and every
/// automatic snapshot whose document is not open, newest first.
pub(crate) fn recoverable(cx: &mut App) -> Task<Result<Vec<StoredSnapshot>, String>> {
    let Some(service) = service(cx) else {
        return Task::ready(Ok(Vec::new()));
    };
    let listing = service.store.snapshots();
    cx.spawn(async move |cx| {
        let snapshots = listing.await?;
        cx.update(|cx| {
            let documents = cx.global::<ApplicationDocuments>().0.read(cx);
            snapshots
                .into_iter()
                .filter(|snapshot| {
                    snapshot.kind == SnapshotKind::Retained
                        || documents
                            .document_by_identity(snapshot.document_id)
                            .is_none()
                })
                .collect()
        })
        .map_err(display_error)
    })
}

/// Restore one stored snapshot as an ordinary dirty document. Nothing is written
/// to its resource.
pub(crate) fn restore(
    kind: SnapshotKind,
    capture_key: String,
    providers: Arc<FileSystemProviderRegistry>,
    cx: &mut App,
) -> Task<Result<RestoreOutcome, RestoreError>> {
    let Some(service) = service(cx) else {
        return Task::ready(Err(RestoreError::Store("recovery is unavailable".into())));
    };
    let read = service.store.snapshot(kind, capture_key);
    cx.spawn(async move |cx| {
        let snapshot = read
            .await
            .map_err(RestoreError::Store)?
            .ok_or(RestoreError::Unavailable)?;
        restore_snapshot(&service, snapshot, providers, cx).await
    })
}

/// Explicitly discard one stored snapshot. Resolves `false` when it no longer exists.
pub(crate) fn discard_snapshot(
    kind: SnapshotKind,
    capture_key: String,
    cx: &App,
) -> Task<Result<bool, String>> {
    match service(cx) {
        Some(service) => service.store.discard(kind, capture_key),
        None => Task::ready(Err("recovery is unavailable".into())),
    }
}

/// Documents restored silently at startup and the snapshots left for the listing.
#[derive(Debug, Default)]
pub(crate) struct StartupRecovery {
    pub(crate) restored: Vec<DocumentId>,
    pub(crate) remaining: usize,
    pub(crate) error: Option<String>,
}

/// Restore every automatic snapshot no live document claims, newest first.
///
/// When several snapshots target one URI only the newest is restored; older
/// ones, retained captures, and snapshots whose resource cannot be read stay in
/// the store and are counted as remaining.
pub(crate) fn restore_at_startup(
    providers: Arc<FileSystemProviderRegistry>,
    cx: &mut App,
) -> Task<StartupRecovery> {
    let Some(service) = service(cx) else {
        return Task::ready(StartupRecovery::default());
    };
    let listing = service.store.snapshots();
    cx.spawn(async move |cx| {
        let mut recovery = StartupRecovery::default();
        let snapshots = match listing.await {
            Ok(snapshots) => snapshots,
            Err(error) => {
                recovery.error = Some(error);
                return recovery;
            }
        };
        let mut claimed = HashSet::new();
        for snapshot in snapshots {
            if snapshot.kind == SnapshotKind::Retained
                || snapshot
                    .resource_uri
                    .as_ref()
                    .is_some_and(|uri| !claimed.insert(uri.clone()))
            {
                recovery.remaining += 1;
                continue;
            }
            match restore_snapshot(&service, snapshot, providers.clone(), cx).await {
                Ok(RestoreOutcome::Restored { document, .. }) => recovery.restored.push(document),
                Ok(RestoreOutcome::AlreadyOpen { .. }) => {}
                Ok(RestoreOutcome::Preserved { document, .. }) => {
                    recovery.restored.push(document);
                }
                Err(error) => {
                    eprintln!("[knot] recovery: {error}");
                    recovery.remaining += 1;
                }
            }
        }
        recovery
    })
}

async fn restore_snapshot(
    service: &RecoveryService,
    snapshot: StoredSnapshot,
    providers: Arc<FileSystemProviderRegistry>,
    cx: &mut AsyncApp,
) -> Result<RestoreOutcome, RestoreError> {
    let loaded = match (&snapshot.resource_uri, snapshot.state) {
        (_, SnapshotState::Untitled) => None,
        (Some(uri), _) => {
            let uri = ResourceUri::parse(uri)
                .map_err(|error| RestoreError::Store(format!("invalid recovered URI: {error}")))?;
            Some(
                load_resource_kind(providers, uri, Some(ResourceKind::File))
                    .await
                    .map_err(RestoreError::Resource)?,
            )
        }
        (None, _) => {
            return Err(RestoreError::Store(
                "recovered document has no resource".into(),
            ));
        }
    };
    let service = service.clone();
    cx.update(|cx| commit_restore(&service, snapshot, loaded, cx))
        .map_err(|error| RestoreError::Store(display_error(error)))?
}

fn commit_restore(
    service: &RecoveryService,
    snapshot: StoredSnapshot,
    loaded: Option<OpenResource>,
    cx: &mut App,
) -> Result<RestoreOutcome, RestoreError> {
    let documents = cx.global::<ApplicationDocuments>().0.clone();
    let (baseline, state, resource) = match (snapshot.state, loaded) {
        (SnapshotState::Untitled, _) => (
            String::new(),
            DocumentState::Untitled { clean_revision: 0 },
            RestoredResource::Untitled,
        ),
        (state, Some(OpenResource::Missing(uri))) => (
            String::new(),
            DocumentState::Destination { uri },
            if state == SnapshotState::Persisted {
                RestoredResource::Missing
            } else {
                RestoredResource::Unchanged
            },
        ),
        (SnapshotState::Destination, Some(OpenResource::File { uri, text, .. })) => (
            text,
            DocumentState::Destination { uri },
            RestoredResource::Changed,
        ),
        (SnapshotState::Persisted, Some(OpenResource::File { uri, text, version })) => {
            let captured =
                ResourceVersion::new(snapshot.provider_version.clone().unwrap_or_default());
            let resource = if version == captured {
                RestoredResource::Unchanged
            } else {
                RestoredResource::Changed
            };
            (
                text,
                DocumentState::Persisted {
                    uri,
                    persisted_revision: 0,
                    version: captured,
                },
                resource,
            )
        }
        (_, Some(OpenResource::Directory(_))) | (_, None) => {
            unreachable!("file loads reject directories and only untitled snapshots skip loading")
        }
    };
    let (by_identity, by_resource) = {
        let documents = documents.read(cx);
        (
            documents
                .document_by_identity(snapshot.document_id)
                .map(Document::id),
            state
                .resource_uri()
                .and_then(|uri| documents.document_for_resource(uri)),
        )
    };
    if snapshot.kind == SnapshotKind::Automatic
        && let Some(document) = by_identity
    {
        return Ok(RestoreOutcome::AlreadyOpen { document });
    }
    if let Some(owner) = by_resource.or(by_identity) {
        let model = cx.new(|_| BufferModel::from_recovered_text("", &snapshot.text));
        let document = documents.update(cx, |documents, cx| {
            documents
                .create_recovered(
                    format!("{} (Recovered)", snapshot.title),
                    model,
                    DocumentState::Untitled { clean_revision: 0 },
                    None,
                    cx,
                )
                .expect("a fresh untitled identity is never live")
        });
        let draft = gather(documents.read(cx).get(document).unwrap(), cx);
        match (snapshot.kind, draft) {
            // The separate copy now carries the automatic protection.
            (SnapshotKind::Automatic, Some(draft)) => service
                .store
                .transfer_automatic(snapshot.document_id, draft)
                .detach(),
            _ => documents.update(cx, |documents, cx| {
                publish(documents.get(document).unwrap(), cx);
            }),
        }
        return Ok(RestoreOutcome::Preserved { owner, document });
    }
    // Retained captures restore under a fresh identity: the automatic role may
    // still protect newer text for the original one.
    let identity = (snapshot.kind == SnapshotKind::Automatic).then_some(snapshot.document_id);
    let model = cx.new(|_| BufferModel::from_recovered_text(baseline, &snapshot.text));
    let document = documents.update(cx, |documents, cx| {
        let document = documents
            .create_recovered(snapshot.title.clone(), model, state, identity, cx)
            .expect("ownership was checked in this turn");
        publish(documents.get(document).unwrap(), cx);
        document
    });
    Ok(RestoreOutcome::Restored { document, resource })
}

fn gather(document: &Document, cx: &App) -> Option<SnapshotDraft> {
    if matches!(document.state(), DocumentState::Generated) || !document.is_dirty(cx) {
        return None;
    }
    let (state, provider_version) = match document.state() {
        DocumentState::Untitled { .. } => (SnapshotState::Untitled, None),
        DocumentState::Destination { .. } => (SnapshotState::Destination, None),
        DocumentState::Persisted { version, .. } => {
            (SnapshotState::Persisted, Some(version.as_bytes().to_vec()))
        }
        DocumentState::Generated => return None,
    };
    Some(SnapshotDraft {
        document_id: document.identity(),
        title: document.title().to_string(),
        resource_uri: document
            .resource_uri()
            .map(|uri| uri.as_url().as_str().to_owned()),
        state,
        provider_version,
        text: document.model().read(cx).text(),
        captured_at_ms: now_ms(),
    })
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Foreground recovery service: the store plus lifecycle-scoped model observers.
#[derive(Clone)]
pub(crate) struct RecoveryService {
    store: RecoveryStore,
    subscriptions: Rc<RefCell<HashMap<uuid::Uuid, Subscription>>>,
}

impl RecoveryService {
    pub(crate) fn new(store: RecoveryStore) -> Self {
        Self {
            store,
            subscriptions: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    pub(crate) fn store(&self) -> &RecoveryStore {
        &self.store
    }

    fn observe(&self, model: &Entity<BufferModel>, identity: uuid::Uuid, cx: &mut App) {
        let service = self.clone();
        let subscription = cx.observe(model, move |_model, cx| {
            service.capture_registered(identity, cx);
        });
        self.subscriptions
            .borrow_mut()
            .insert(identity, subscription);
    }

    fn capture_registered(&self, identity: uuid::Uuid, cx: &mut App) {
        let update = {
            let Some(documents) = cx
                .try_global::<ApplicationDocuments>()
                .map(|global| global.0.clone())
            else {
                return;
            };
            let documents = documents.read(cx);
            documents
                .document_by_identity(identity)
                .map(|document| gather(document, cx))
        };
        match update {
            Some(Some(draft)) => self.store.submit_automatic(draft),
            Some(None) => self.store.retire_automatic(identity),
            // A removed document is handled by `forget_document`.
            None => {}
        }
    }
}

pub(crate) struct ApplicationRecovery(pub(crate) RecoveryService);

impl Global for ApplicationRecovery {}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store(path: &std::path::Path, cx: &gpui::TestAppContext) -> RecoveryStore {
        RecoveryStore::open(path.to_owned(), cx.background_executor.clone())
    }

    fn draft(document_id: uuid::Uuid, text: &str) -> SnapshotDraft {
        SnapshotDraft {
            document_id,
            title: "Untitled".into(),
            resource_uri: None,
            state: SnapshotState::Untitled,
            provider_version: None,
            text: text.into(),
            captured_at_ms: now_ms(),
        }
    }

    #[gpui::test]
    async fn automatic_captures_survive_database_reopen(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.sqlite3");
        let id = uuid::Uuid::new_v4();
        let store = open_store(&path, cx);
        assert!(!path.exists());
        store.submit_automatic(draft(id, "unsaved"));
        store.flush().await.unwrap();
        assert!(path.exists());
        drop(store);
        let reopened = open_store(&path, cx);
        let stored = reopened.automatic(id).await.unwrap().unwrap();
        assert_eq!(stored.text, "unsaved");
        assert_eq!(stored.kind, SnapshotKind::Automatic);
        assert_eq!(stored.state, SnapshotState::Untitled);
        assert_eq!(reopened.last_error(), None);
    }

    #[gpui::test]
    async fn coalesced_captures_persist_only_the_latest(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.sqlite3");
        let id = uuid::Uuid::new_v4();
        let store = open_store(&path, cx);
        store.submit_automatic(draft(id, "first"));
        store.submit_automatic(draft(id, "second"));
        store.flush().await.unwrap();
        assert_eq!(
            open_store(&path, cx)
                .automatic(id)
                .await
                .unwrap()
                .unwrap()
                .text,
            "second"
        );
    }

    #[gpui::test]
    async fn identical_captures_skip_writes(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.sqlite3");
        let id = uuid::Uuid::new_v4();
        let store = open_store(&path, cx);
        store.submit_automatic(draft(id, "same"));
        store.flush().await.unwrap();
        std::fs::remove_file(&path).unwrap();
        store.submit_automatic(draft(id, "same"));
        store.flush().await.unwrap();
        assert!(!path.exists());
    }

    #[gpui::test]
    async fn retiring_a_document_removes_its_automatic_row(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.sqlite3");
        let id = uuid::Uuid::new_v4();
        let store = open_store(&path, cx);
        store.submit_automatic(draft(id, "discarded"));
        store.flush().await.unwrap();
        assert!(open_store(&path, cx).automatic(id).await.unwrap().is_some());
        store.retire_automatic(id);
        store.flush().await.unwrap();
        assert!(open_store(&path, cx).automatic(id).await.unwrap().is_none());
    }

    #[gpui::test]
    async fn retained_rows_are_immutable_and_survive_automatic_captures(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.sqlite3");
        let id = uuid::Uuid::new_v4();
        let store = open_store(&path, cx);
        assert!(
            store
                .retain("owner".into(), "retained-1".into(), draft(id, "checkpoint"))
                .await
                .unwrap()
        );
        // Reusing the same opaque key never overwrites the immutable capture.
        assert!(
            !store
                .retain("owner".into(), "retained-1".into(), draft(id, "newer"))
                .await
                .unwrap()
        );
        // A later automatic capture is a separate role and does not replace it.
        store.submit_automatic(draft(id, "automatic"));
        store.flush().await.unwrap();
        let snapshots = open_store(&path, cx).snapshots().await.unwrap();
        let retained: Vec<_> = snapshots
            .iter()
            .filter(|snapshot| snapshot.kind == SnapshotKind::Retained)
            .collect();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].text, "checkpoint");
        let automatic = snapshots
            .iter()
            .find(|snapshot| snapshot.kind == SnapshotKind::Automatic)
            .unwrap();
        assert_eq!(automatic.text, "automatic");
        assert!(
            store
                .release("owner".into(), "retained-1".into())
                .await
                .unwrap()
        );
        assert!(
            open_store(&path, cx)
                .snapshots()
                .await
                .unwrap()
                .iter()
                .all(|snapshot| snapshot.kind != SnapshotKind::Retained)
        );
        // Releasing does not touch the automatic protection.
        assert_eq!(
            open_store(&path, cx)
                .automatic(id)
                .await
                .unwrap()
                .unwrap()
                .text,
            "automatic"
        );
    }

    #[gpui::test]
    async fn discard_removes_either_role_and_transfer_moves_protection_atomically(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.sqlite3");
        let orphan = uuid::Uuid::new_v4();
        let store = open_store(&path, cx);
        store.submit_automatic(draft(orphan, "orphan"));
        store.flush().await.unwrap();
        assert!(
            store
                .retain("owner".into(), "kept".into(), draft(orphan, "kept"))
                .await
                .unwrap()
        );

        let copy = draft(uuid::Uuid::new_v4(), "orphan");
        store
            .transfer_automatic(orphan, copy.clone())
            .await
            .unwrap();
        let reopened = open_store(&path, cx);
        assert!(reopened.automatic(orphan).await.unwrap().is_none());
        assert_eq!(
            reopened
                .automatic(copy.document_id)
                .await
                .unwrap()
                .unwrap()
                .text,
            "orphan"
        );
        // The retained capture of the same document is a separate role.
        assert!(
            reopened
                .snapshot(SnapshotKind::Retained, "kept".into())
                .await
                .unwrap()
                .is_some()
        );

        assert!(
            store
                .discard(SnapshotKind::Retained, "kept".into())
                .await
                .unwrap()
        );
        assert!(
            store
                .discard(SnapshotKind::Automatic, copy.document_id.to_string())
                .await
                .unwrap()
        );
        assert!(
            !store
                .discard(SnapshotKind::Automatic, copy.document_id.to_string())
                .await
                .unwrap()
        );
        assert!(open_store(&path, cx).snapshots().await.unwrap().is_empty());
    }

    #[gpui::test]
    async fn incompatible_or_corrupt_databases_are_preserved(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let future = dir.path().join("future.sqlite3");
        let connection = Connection::open(&future).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 999).unwrap();
        drop(connection);
        let store = open_store(&future, cx);
        store.submit_automatic(draft(uuid::Uuid::new_v4(), "kept"));
        assert!(store.flush().await.is_err());
        assert!(store.last_error().is_some());
        // The incompatible database is preserved, not replaced.
        let connection = Connection::open(&future).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 999);
        drop(connection);

        let corrupt = dir.path().join("corrupt.sqlite3");
        std::fs::write(&corrupt, "not a database").unwrap();
        let store = open_store(&corrupt, cx);
        store.submit_automatic(draft(uuid::Uuid::new_v4(), "kept"));
        assert!(store.flush().await.is_err());
        assert_eq!(std::fs::read(&corrupt).unwrap(), b"not a database");
    }

    #[gpui::test]
    async fn interrupted_capture_leaves_the_previous_row_durable(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.sqlite3");
        let id = uuid::Uuid::new_v4();
        let store = open_store(&path, cx);
        store.submit_automatic(draft(id, "durable"));
        store.flush().await.unwrap();
        // A newer capture is still inside its debounce window when the process
        // disappears; the durable row is the previously committed one.
        store.submit_automatic(draft(id, "in flight"));
        drop(store);
        assert_eq!(
            open_store(&path, cx)
                .automatic(id)
                .await
                .unwrap()
                .unwrap()
                .text,
            "durable"
        );
        // The next capture commits the new text.
        let reopened = open_store(&path, cx);
        reopened.submit_automatic(draft(id, "in flight"));
        reopened.flush().await.unwrap();
        assert_eq!(
            open_store(&path, cx)
                .automatic(id)
                .await
                .unwrap()
                .unwrap()
                .text,
            "in flight"
        );
    }

    #[gpui::test]
    async fn write_failure_keeps_the_capture_and_reports(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("recovery.sqlite3");
        let store = open_store(&path, cx);
        store.submit_automatic(draft(uuid::Uuid::new_v4(), "unsaved"));
        assert!(store.flush().await.is_err());
        assert!(store.last_error().is_some());
        // Nothing durable was written and the pending capture is retained for a
        // later retry; editing is never blocked by the failure.
        assert!(!path.exists());
    }

    #[test]
    fn coalescing_never_exceeds_the_maximum_interval() {
        let start = Instant::now();
        let settle = Duration::from_millis(1000);
        let max_interval = Duration::from_millis(5000);
        // A change well after the burst began is capped by the interval.
        let late = start + Duration::from_millis(4500);
        assert_eq!(
            next_due(late, start, settle, max_interval),
            start + max_interval
        );
        // A settled edit uses the trailing-edge delay.
        let quiet = start + Duration::from_millis(2000);
        assert_eq!(next_due(quiet, quiet, settle, max_interval), quiet + settle);
    }
}
