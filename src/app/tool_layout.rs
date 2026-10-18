//! Automatic tool-layout memory: one versioned tool-only snapshot in application-owned SQLite.
//!
//! The snapshot records stable namespaced tool IDs, dock location, grouping
//! and order, sizes, visibility, the selected tool per dock, and hidden tools'
//! remembered locations. Document tabs, document split structure, terminal
//! contents, and empty placeholders are never serialized here.
//!
//! A fresh window reads the last published snapshot; already-open windows keep
//! their live arrangements. Only explicit user moves, resizes, and show/hide
//! changes publish a new default through [`ToolLayoutStore::publish`]. Restore,
//! window activation, and automatic presentation never publish. Writes run off
//! the foreground with trailing-edge debounce so divider dragging coalesces
//! into a single write; [`ToolLayoutStore::flush`] forces a pending write out,
//! which the quit path awaits before exiting.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{BackgroundExecutor, Global, Task};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::workbench::DockLocation;

pub(crate) const TOOL_LAYOUT_VERSION: u32 = 1;
pub(crate) const EXPLORER_TOOL_ID: &str = "knot.explorer";

/// Trailing-edge debounce for automatic writes. Pointer-drag resizes publish
/// on every movement; only the settled arrangement reaches SQLite.
const WRITE_DEBOUNCE: Duration = Duration::from_millis(300);

const APPLICATION_ID: i64 = 0x4b4e4f54;
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS tool_layout(state INTEGER PRIMARY KEY CHECK(state = 0), version INTEGER NOT NULL, payload TEXT NOT NULL CHECK(json_valid(payload))) STRICT";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum StoredLocation {
    Left,
    Right,
    Bottom,
    DocumentGroup,
}

impl StoredLocation {
    pub(crate) fn placement(self) -> super::workbench::ToolPlacement {
        match self {
            Self::Left => super::workbench::ToolPlacement::Dock(DockLocation::Left),
            Self::Right => super::workbench::ToolPlacement::Dock(DockLocation::Right),
            Self::Bottom => super::workbench::ToolPlacement::Dock(DockLocation::Bottom),
            Self::DocumentGroup => super::workbench::ToolPlacement::DocumentGroup,
        }
    }
}

impl From<DockLocation> for StoredLocation {
    fn from(dock: DockLocation) -> Self {
        match dock {
            DockLocation::Left => Self::Left,
            DockLocation::Right => Self::Right,
            DockLocation::Bottom => Self::Bottom,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StoredTool {
    pub(crate) id: String,
    pub(crate) location: StoredLocation,
    pub(crate) index: usize,
    pub(crate) visible: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct ToolLayoutSnapshot {
    pub(crate) version: u32,
    pub(crate) tools: Vec<StoredTool>,
    pub(crate) dock_sizes: [f32; 3],
    pub(crate) dock_selected: [Option<String>; 3],
    /// Whether the user has ever customized the arrangement. Fresh windows
    /// only auto-show stored tools once set, preserving first-run behavior
    /// (no eager explorer before any folder or toggle) on fresh installs.
    #[serde(default)]
    pub(crate) customized: bool,
}

impl ToolLayoutSnapshot {
    pub(crate) fn product_default() -> Self {
        Self {
            version: TOOL_LAYOUT_VERSION,
            tools: vec![StoredTool {
                id: EXPLORER_TOOL_ID.to_owned(),
                location: StoredLocation::Left,
                index: 0,
                visible: true,
            }],
            dock_sizes: [240., 240., 220.],
            dock_selected: [Some(EXPLORER_TOOL_ID.to_owned()), None, None],
            customized: false,
        }
    }

    pub(crate) fn tool(&self, id: &str) -> Option<&StoredTool> {
        self.tools.iter().find(|tool| tool.id == id)
    }

    fn encode(&self) -> String {
        serde_json::to_string(self).expect("tool layout snapshot must serialize")
    }

    fn decode(payload: &str) -> Option<Self> {
        let snapshot: Self = serde_json::from_str(payload).ok()?;
        (snapshot.version == TOOL_LAYOUT_VERSION).then_some(snapshot)
    }
}

fn read_snapshot(path: &Path) -> ToolLayoutSnapshot {
    // Never create the database on read: a fresh install simply uses defaults.
    if !path.is_file() {
        return ToolLayoutSnapshot::product_default();
    }
    let snapshot = (|| {
        let connection = Connection::open(path).ok()?;
        let (version, payload): (u32, String) = connection
            .query_row(
                "SELECT version, payload FROM tool_layout WHERE state = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok()?;
        (version == TOOL_LAYOUT_VERSION)
            .then(|| ToolLayoutSnapshot::decode(&payload))
            .flatten()
    })();
    snapshot.unwrap_or_else(ToolLayoutSnapshot::product_default)
}

fn write_snapshot(path: &Path, snapshot: &ToolLayoutSnapshot) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("tool layout {}: {error}", path.display()))?;
    }
    let connection = Connection::open(path)
        .map_err(|error| format!("tool layout {}: {error}", path.display()))?;
    connection
        .execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID}; {SCHEMA}"
        ))
        .map_err(|error| format!("tool layout {}: {error}", path.display()))?;
    connection
        .execute(
            "INSERT INTO tool_layout(state, version, payload) VALUES (0, ?1, ?2) \
             ON CONFLICT(state) DO UPDATE SET version = excluded.version, payload = excluded.payload",
            params![TOOL_LAYOUT_VERSION, snapshot.encode()],
        )
        .map_err(|error| format!("tool layout {}: {error}", path.display()))?;
    Ok(())
}

struct StoreState {
    current: ToolLayoutSnapshot,
    pending: Option<ToolLayoutSnapshot>,
    generation: u64,
    last_error: Option<String>,
}

struct Inner {
    path: PathBuf,
    executor: BackgroundExecutor,
    state: Mutex<StoreState>,
}

/// Application-owned tool-layout persistence boundary.
///
/// The cached [`ToolLayoutSnapshot`] is updated synchronously on publish so
/// fresh windows immediately inherit user customizations; SQLite writes follow
/// off the foreground with debounce. A failed write keeps the live cache and
/// records the error without disturbing the window.
#[derive(Clone)]
pub(crate) struct ToolLayoutStore {
    inner: Arc<Inner>,
}

impl ToolLayoutStore {
    pub(crate) fn user_path() -> Result<PathBuf, String> {
        directories::ProjectDirs::from("", "", "Knot")
            .map(|dirs| dirs.data_local_dir().join("tool-layout.sqlite3"))
            .ok_or_else(|| "cannot determine the user application-data directory".into())
    }

    /// Load the persisted default synchronously; missing, corrupt, or
    /// versioned-away records fall back to product defaults without I/O noise.
    pub(crate) fn open(path: PathBuf, executor: BackgroundExecutor) -> Self {
        let current = read_snapshot(&path);
        Self {
            inner: Arc::new(Inner {
                path,
                executor,
                state: Mutex::new(StoreState {
                    current,
                    pending: None,
                    generation: 0,
                    last_error: None,
                }),
            }),
        }
    }

    pub(crate) fn current(&self) -> ToolLayoutSnapshot {
        self.inner.state.lock().unwrap().current.clone()
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "write failures stay observable in tests")
    )]
    pub(crate) fn last_error(&self) -> Option<String> {
        self.inner.state.lock().unwrap().last_error.clone()
    }

    /// Record a user-customized arrangement and schedule a coalesced write.
    /// Equal snapshots are no-ops, so divider release without movement and
    /// repeated identical publishes never touch SQLite. Any publish marks the
    /// arrangement customized, including a return to product defaults.
    pub(crate) fn publish(&self, snapshot: ToolLayoutSnapshot) {
        let mut snapshot = snapshot;
        snapshot.customized = true;
        let generation = {
            let mut state = self.inner.state.lock().unwrap();
            if state.current == snapshot {
                return;
            }
            state.current = snapshot.clone();
            state.pending = Some(snapshot);
            state.generation += 1;
            state.generation
        };
        let this = self.clone();
        self.inner
            .executor
            .spawn(async move {
                this.inner.executor.timer(WRITE_DEBOUNCE).await;
                this.write_if_current(generation);
            })
            .detach();
    }

    /// Write the pending snapshot now, off the foreground. Used by tests and
    /// the quit path; on failure the snapshot stays pending for a later retry.
    pub(crate) fn flush(&self) -> Task<Result<(), String>> {
        let pending = self.inner.state.lock().unwrap().pending.take();
        let this = self.clone();
        self.inner.executor.spawn(async move {
            let Some(snapshot) = pending else {
                return Ok(());
            };
            match write_snapshot(&this.inner.path, &snapshot) {
                Ok(()) => {
                    this.inner.state.lock().unwrap().last_error = None;
                    Ok(())
                }
                Err(error) => {
                    let mut state = this.inner.state.lock().unwrap();
                    state.last_error = Some(error.clone());
                    state.pending = Some(snapshot);
                    Err(error)
                }
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
        // both persist the same snapshot, so whichever runs first wins.
        let Some(snapshot) = pending else {
            return;
        };
        match write_snapshot(&self.inner.path, &snapshot) {
            Ok(()) => {
                self.inner.state.lock().unwrap().last_error = None;
            }
            Err(error) => {
                let mut state = self.inner.state.lock().unwrap();
                state.last_error = Some(error);
                state.pending = Some(snapshot);
            }
        }
    }
}

pub(crate) struct ApplicationToolLayout(pub(crate) ToolLayoutStore);

impl Global for ApplicationToolLayout {}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store(path: &Path, cx: &gpui::TestAppContext) -> ToolLayoutStore {
        ToolLayoutStore::open(path.to_owned(), cx.background_executor.clone())
    }

    fn customized(size: f32) -> ToolLayoutSnapshot {
        let mut snapshot = ToolLayoutSnapshot::product_default();
        snapshot.dock_sizes = [size, 200., 180.];
        snapshot.tools[0].location = StoredLocation::Bottom;
        // Publish marks the arrangement customized; stored snapshots compare
        // against that form.
        snapshot.customized = true;
        snapshot
    }

    #[gpui::test]
    async fn snapshots_survive_database_reopen(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool-layout.sqlite3");
        let expected = customized(311.);
        let store = open_store(&path, cx);
        assert_eq!(store.current(), ToolLayoutSnapshot::product_default());
        store.publish(expected.clone());
        // The debounced write has not run yet: the cache is live but SQLite is untouched.
        assert!(!path.exists());
        store.flush().await.unwrap();
        assert!(path.exists());
        drop(store);
        let reopened = open_store(&path, cx);
        assert_eq!(reopened.current(), expected);
        assert_eq!(reopened.last_error(), None);
    }

    #[gpui::test]
    async fn coalesced_publishes_persist_only_the_latest(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("tool-layout.sqlite3");
        let store = open_store(&path, cx);
        store.publish(customized(300.));
        store.publish(customized(320.));
        assert!(!path.exists());
        store.flush().await.unwrap();
        drop(store);
        assert_eq!(open_store(&path, cx).current(), customized(320.));
    }

    #[gpui::test]
    async fn identical_republish_skips_writes(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool-layout.sqlite3");
        let store = open_store(&path, cx);
        assert!(!store.current().customized);
        store.publish(customized(320.));
        store.flush().await.unwrap();
        std::fs::remove_file(&path).unwrap();
        store.publish(customized(320.));
        store.flush().await.unwrap();
        assert!(!path.exists());
    }

    #[gpui::test]
    async fn publish_marks_customized_and_survives_reopen(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool-layout.sqlite3");
        let store = open_store(&path, cx);
        // Even confirming product defaults counts as a customization, so fresh
        // windows later inherit the confirmed arrangement.
        store.publish(ToolLayoutSnapshot::product_default());
        store.flush().await.unwrap();
        drop(store);
        let reopened = open_store(&path, cx);
        let mut expected = ToolLayoutSnapshot::product_default();
        expected.customized = true;
        assert_eq!(reopened.current(), expected);
    }

    #[gpui::test]
    async fn missing_corrupt_and_future_records_fall_back_to_defaults(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            open_store(&dir.path().join("absent.sqlite3"), cx).current(),
            ToolLayoutSnapshot::product_default()
        );
        let garbage = dir.path().join("garbage.sqlite3");
        std::fs::write(&garbage, "not sqlite").unwrap();
        assert_eq!(
            open_store(&garbage, cx).current(),
            ToolLayoutSnapshot::product_default()
        );
        let future = dir.path().join("future.sqlite3");
        let connection = Connection::open(&future).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection
            .execute(
                "INSERT INTO tool_layout(state, version, payload) VALUES (0, 999, '{\"version\":999}')",
                [],
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            open_store(&future, cx).current(),
            ToolLayoutSnapshot::product_default()
        );
    }

    #[gpui::test]
    async fn write_failure_keeps_live_state_and_falls_back_on_reload(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("tool-layout.sqlite3");
        let store = open_store(&path, cx);
        let expected = customized(311.);
        store.publish(expected.clone());
        assert_eq!(store.current(), expected);
        assert!(store.flush().await.is_err());
        assert_eq!(store.current(), expected);
        assert!(store.last_error().is_some());
        // Nothing durable was written, so a fresh open falls back to defaults.
        assert_eq!(
            open_store(&path, cx).current(),
            ToolLayoutSnapshot::product_default()
        );
    }
}
