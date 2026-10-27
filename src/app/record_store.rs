//! One versioned JSON record in an application-owned SQLite file.
//!
//! The cached value updates synchronously on publish so readers see it
//! immediately; SQLite writes follow off the foreground with a trailing-edge
//! debounce. [`RecordStore::flush`] forces a pending write out, which the quit
//! path awaits before exiting. A failed write keeps the cache, records the
//! error, and leaves the value pending for a later retry. Missing, corrupt, or
//! versioned-away records read as absent without creating the database.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{BackgroundExecutor, Task};
use rusqlite::{Connection, params};
use serde::{Serialize, de::DeserializeOwned};

const APPLICATION_ID: i64 = 0x4b4e4f54;

struct State<T> {
    current: Option<T>,
    pending: Option<T>,
    generation: u64,
    last_error: Option<String>,
}

struct Inner<T> {
    path: PathBuf,
    table: &'static str,
    version: u32,
    debounce: Duration,
    executor: BackgroundExecutor,
    state: Mutex<State<T>>,
}

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

fn schema(table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table}(state INTEGER PRIMARY KEY CHECK(state = 0), version INTEGER NOT NULL, payload TEXT NOT NULL CHECK(json_valid(payload))) STRICT"
    )
}

impl<T> RecordStore<T>
where
    T: Clone + PartialEq + Serialize + DeserializeOwned + Send + 'static,
{
    /// Load the record synchronously.
    pub(crate) fn open(
        path: PathBuf,
        table: &'static str,
        version: u32,
        debounce: Duration,
        executor: BackgroundExecutor,
    ) -> Self {
        let current = Self::read(&path, table, version);
        Self {
            inner: Arc::new(Inner {
                path,
                table,
                version,
                debounce,
                executor,
                state: Mutex::new(State {
                    current,
                    pending: None,
                    generation: 0,
                    last_error: None,
                }),
            }),
        }
    }

    fn read(path: &Path, table: &str, version: u32) -> Option<T> {
        // Never create the database on read: a fresh install has no record.
        if !path.is_file() {
            return None;
        }
        let connection = Connection::open(path).ok()?;
        let (stored, payload): (u32, String) = connection
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
        let this = self.clone();
        self.inner
            .executor
            .spawn(async move {
                this.inner.executor.timer(this.inner.debounce).await;
                this.write_if_current(generation);
            })
            .detach();
    }

    /// Write the pending record now, off the foreground.
    pub(crate) fn flush(&self) -> Task<Result<(), String>> {
        let pending = self.inner.state.lock().unwrap().pending.take();
        let this = self.clone();
        self.inner.executor.spawn(async move {
            match pending {
                Some(value) => this.commit(value),
                None => Ok(()),
            }
        })
    }

    fn write_if_current(&self, generation: u64) {
        let pending = {
            let mut state = self.inner.state.lock().unwrap();
            if state.generation != generation {
                return;
            }
            state.pending.take()
        };
        // Only one of the debounce timer and flush can take the pending write;
        // both persist the same value, so whichever runs first wins.
        if let Some(value) = pending {
            let _ = self.commit(value);
        }
    }

    fn commit(&self, value: T) -> Result<(), String> {
        match self.write(&value) {
            Ok(()) => {
                self.inner.state.lock().unwrap().last_error = None;
                Ok(())
            }
            Err(error) => {
                let mut state = self.inner.state.lock().unwrap();
                state.last_error = Some(error.clone());
                // A newer publish keeps priority over the failed value.
                if state.pending.is_none() {
                    state.pending = Some(value);
                }
                Err(error)
            }
        }
    }

    fn write(&self, value: &T) -> Result<(), String> {
        let path = &self.inner.path;
        let failed = |error: &dyn std::fmt::Display| {
            format!("{} {}: {error}", self.inner.table, path.display())
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| failed(&error))?;
        }
        let connection = Connection::open(path).map_err(|error| failed(&error))?;
        connection
            .execute_batch(&format!(
                "PRAGMA application_id = {APPLICATION_ID}; {}",
                schema(self.inner.table)
            ))
            .map_err(|error| failed(&error))?;
        let payload = serde_json::to_string(value).expect("records serialize");
        connection
            .execute(
                &format!(
                    "INSERT INTO {}(state, version, payload) VALUES (0, ?1, ?2) \
                     ON CONFLICT(state) DO UPDATE SET version = excluded.version, payload = excluded.payload",
                    self.inner.table
                ),
                params![self.inner.version, payload],
            )
            .map_err(|error| failed(&error))?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn test_schema(table: &str) -> String {
    schema(table)
}
