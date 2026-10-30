//! The application state database: one SQLite file holding versioned JSON
//! records, one table each.
//!
//! [`StateDatabase`] owns a single connection, so writes from every record
//! are serialized and never contend for the file. A [`RecordStore`] is one
//! record's handle: its cached value updates synchronously on publish so
//! readers see it immediately, and SQLite writes follow off the foreground
//! with a trailing-edge debounce. [`StateDatabase::flush`] forces every
//! pending write out, which the quit path awaits before exiting. A failed
//! write keeps the cache, records the error, and leaves the value pending for
//! a later retry. Missing, corrupt, or versioned-away records read as absent
//! without creating the database.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use gpui::{BackgroundExecutor, Global, Task};
use rusqlite::{Connection, params};
use serde::{Serialize, de::DeserializeOwned};

const APPLICATION_ID: i64 = 0x4b4e4f54;

fn schema(table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table}(state INTEGER PRIMARY KEY CHECK(state = 0), version INTEGER NOT NULL, payload TEXT NOT NULL CHECK(json_valid(payload))) STRICT"
    )
}

/// A record whose pending value can be written by a database-wide flush.
trait PendingRecord: Send + Sync {
    fn write_pending(&self) -> Result<(), String>;
}

struct Database {
    path: PathBuf,
    executor: BackgroundExecutor,
    /// Opened on the first read of an existing file or the first write.
    connection: Mutex<Option<Connection>>,
    records: Mutex<Vec<Weak<dyn PendingRecord>>>,
}

/// Shared handle to the application state database.
#[derive(Clone)]
pub(crate) struct StateDatabase {
    inner: Arc<Database>,
}

impl StateDatabase {
    pub(crate) fn user_path() -> Result<PathBuf, String> {
        directories::ProjectDirs::from("", "", "Knot")
            .map(|dirs| dirs.data_local_dir().join("state.sqlite3"))
            .ok_or_else(|| "cannot determine the user application-data directory".into())
    }

    pub(crate) fn open(path: PathBuf, executor: BackgroundExecutor) -> Self {
        Self {
            inner: Arc::new(Database {
                path,
                executor,
                connection: Mutex::new(None),
                records: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Load one record synchronously and return its handle.
    pub(crate) fn record<T>(
        &self,
        table: &'static str,
        version: u32,
        debounce: Duration,
    ) -> RecordStore<T>
    where
        T: Clone + PartialEq + Serialize + DeserializeOwned + Send + 'static,
    {
        let current = self.read(table, version);
        let inner = Arc::new(Inner {
            database: self.clone(),
            table,
            version,
            debounce,
            state: Mutex::new(State {
                current,
                pending: None,
                generation: 0,
                last_error: None,
            }),
        });
        let pending: Arc<dyn PendingRecord> = inner.clone();
        let mut records = self.inner.records.lock().unwrap();
        records.retain(|record| record.strong_count() > 0);
        records.push(Arc::downgrade(&pending));
        RecordStore { inner }
    }

    /// Write every record's pending value now, off the foreground. Every
    /// record is attempted; the first failure is reported.
    pub(crate) fn flush(&self) -> Task<Result<(), String>> {
        let records = self
            .inner
            .records
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        self.inner.executor.spawn(async move {
            records
                .iter()
                .map(|record| record.write_pending())
                .fold(Ok(()), Result::and)
        })
    }

    fn read<T: DeserializeOwned>(&self, table: &str, version: u32) -> Option<T> {
        let mut connection = self.inner.connection.lock().unwrap();
        if connection.is_none() {
            // Never create the database on read: a fresh install has no record.
            if !self.inner.path.is_file() {
                return None;
            }
            *connection = Some(Connection::open(&self.inner.path).ok()?);
        }
        let (stored, payload): (u32, String) = connection
            .as_ref()?
            .query_row(
                &format!("SELECT version, payload FROM {table} WHERE state = 0"),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()?;
        (stored == version)
            .then(|| serde_json::from_str(&payload).ok())
            .flatten()
    }

    fn write(&self, table: &str, version: u32, payload: &str) -> Result<(), String> {
        let path = &self.inner.path;
        let failed = |error: &dyn std::fmt::Display| format!("{table} {}: {error}", path.display());
        let mut connection = self.inner.connection.lock().unwrap();
        if connection.is_none() {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|error| failed(&error))?;
            }
            *connection = Some(Connection::open(path).map_err(|error| failed(&error))?);
        }
        let connection = connection.as_ref().expect("connection opened above");
        connection
            .execute_batch(&format!(
                "PRAGMA application_id = {APPLICATION_ID}; {}",
                schema(table)
            ))
            .map_err(|error| failed(&error))?;
        connection
            .execute(
                &format!(
                    "INSERT INTO {table}(state, version, payload) VALUES (0, ?1, ?2) \
                     ON CONFLICT(state) DO UPDATE SET version = excluded.version, payload = excluded.payload"
                ),
                params![version, payload],
            )
            .map_err(|error| failed(&error))?;
        Ok(())
    }
}

/// The application's state database, shared by every UI-state record.
pub(crate) struct ApplicationState(pub(crate) StateDatabase);

impl Global for ApplicationState {}

struct State<T> {
    current: Option<T>,
    pending: Option<T>,
    generation: u64,
    last_error: Option<String>,
}

struct Inner<T> {
    database: StateDatabase,
    table: &'static str,
    version: u32,
    debounce: Duration,
    state: Mutex<State<T>>,
}

impl<T> Inner<T>
where
    T: Clone + Serialize + Send,
{
    fn commit(&self, value: T) -> Result<(), String> {
        let payload = serde_json::to_string(&value).expect("records serialize");
        match self.database.write(self.table, self.version, &payload) {
            Ok(()) => {
                self.state.lock().unwrap().last_error = None;
                Ok(())
            }
            Err(error) => {
                let mut state = self.state.lock().unwrap();
                state.last_error = Some(error.clone());
                // A newer publish keeps priority over the failed value.
                if state.pending.is_none() {
                    state.pending = Some(value);
                }
                Err(error)
            }
        }
    }
}

impl<T> PendingRecord for Inner<T>
where
    T: Clone + Serialize + Send,
{
    fn write_pending(&self) -> Result<(), String> {
        let pending = self.state.lock().unwrap().pending.take();
        match pending {
            Some(value) => self.commit(value),
            None => Ok(()),
        }
    }
}

/// One record's handle in the [`StateDatabase`].
pub(crate) struct RecordStore<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Clone for RecordStore<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> RecordStore<T>
where
    T: Clone + PartialEq + Serialize + DeserializeOwned + Send + 'static,
{
    pub(crate) fn current(&self) -> Option<T> {
        self.inner.state.lock().unwrap().current.clone()
    }

    #[cfg(test)]
    pub(crate) fn last_error(&self) -> Option<String> {
        self.inner.state.lock().unwrap().last_error.clone()
    }

    /// Replace the record and schedule a coalesced write. Publishing an equal
    /// value is a no-op.
    pub(crate) fn publish(&self, value: T) {
        let generation = {
            let mut state = self.inner.state.lock().unwrap();
            if state.current.as_ref() == Some(&value) {
                return;
            }
            state.current = Some(value.clone());
            state.pending = Some(value);
            state.generation += 1;
            state.generation
        };
        let inner = self.inner.clone();
        let executor = self.inner.database.inner.executor.clone();
        executor
            .clone()
            .spawn(async move {
                executor.timer(inner.debounce).await;
                // Only one of the debounce timer and a flush can take the
                // pending write; both persist the same value, so whichever
                // runs first wins.
                if inner.state.lock().unwrap().generation == generation {
                    let _ = inner.write_pending();
                }
            })
            .detach();
    }

    /// Write this record's pending value now, off the foreground.
    pub(crate) fn flush(&self) -> Task<Result<(), String>> {
        let inner = self.inner.clone();
        self.inner
            .database
            .inner
            .executor
            .spawn(async move { inner.write_pending() })
    }
}

#[cfg(test)]
pub(crate) fn test_schema(table: &str) -> String {
    schema(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
    struct Value(u32);

    const DEBOUNCE: Duration = Duration::from_millis(300);

    #[gpui::test]
    async fn records_share_one_file_and_flush_together(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        let database = StateDatabase::open(path.clone(), cx.background_executor.clone());
        let first = database.record::<Value>("first", 1, DEBOUNCE);
        let second = database.record::<Value>("second", 1, DEBOUNCE);
        first.publish(Value(1));
        second.publish(Value(2));
        // Both debounced writes race the flush on one connection.
        cx.background_executor.advance_clock(DEBOUNCE);
        database.flush().await.unwrap();
        cx.run_until_parked();
        assert_eq!(first.last_error(), None);
        assert_eq!(second.last_error(), None);

        let reopened = StateDatabase::open(path, cx.background_executor.clone());
        assert_eq!(
            reopened.record::<Value>("first", 1, DEBOUNCE).current(),
            Some(Value(1))
        );
        assert_eq!(
            reopened.record::<Value>("second", 1, DEBOUNCE).current(),
            Some(Value(2))
        );
    }

    #[gpui::test]
    async fn a_damaged_record_leaves_the_others_readable(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        let database = StateDatabase::open(path.clone(), cx.background_executor.clone());
        database
            .record::<Value>("good", 1, DEBOUNCE)
            .publish(Value(7));
        database
            .record::<Value>("bad", 1, DEBOUNCE)
            .publish(Value(8));
        database.flush().await.unwrap();
        drop(database);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute("UPDATE bad SET payload = '\"not a number\"'", [])
            .unwrap();
        drop(connection);

        let reopened = StateDatabase::open(path, cx.background_executor.clone());
        assert_eq!(reopened.record::<Value>("bad", 1, DEBOUNCE).current(), None);
        assert_eq!(
            reopened.record::<Value>("good", 1, DEBOUNCE).current(),
            Some(Value(7))
        );
    }

    #[gpui::test]
    async fn flush_skips_dropped_records(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        let database = StateDatabase::open(path.clone(), cx.background_executor.clone());
        database.record::<Value>("dropped", 1, DEBOUNCE);
        let kept = database.record::<Value>("kept", 1, DEBOUNCE);
        kept.publish(Value(3));
        database.flush().await.unwrap();
        assert!(path.exists());
    }
}
