//! Native window hot exit: every product window's roots and layout in one
//! application-owned record beside the tool-only snapshot.
//!
//! Recording starts once product startup has restored and opened its windows,
//! so diagnostic windows and a launch whose config failed never write.
//! Settled window changes and window closure re-record every product window
//! through the store's debounce. Quit captures the windows before closing them
//! and stops recording, so the closing windows are kept. Startup marks the
//! record consumed durably before restoring it, so a crash during restoration
//! loses only the arrangement and the next launch opens the default window.

use std::{cell::Cell, path::PathBuf, time::Duration};

use gpui::{App, AppContext as _, BackgroundExecutor, Global, Subscription, Task};
use serde::{Deserialize, Serialize};

use super::{product::ProductShell, record_store::RecordStore, window_layout::WindowLayout};

pub(crate) const WINDOW_SESSION_VERSION: u32 = 1;

const WRITE_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct WindowSession {
    /// Set durably before a launch restores the record.
    pub(crate) consumed: bool,
    /// Index of the active window.
    pub(crate) active: Option<usize>,
    pub(crate) windows: Vec<RecordedWindow>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct RecordedWindow {
    pub(crate) roots: Vec<String>,
    pub(crate) layout: WindowLayout,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Recording {
    /// Before startup finishes opening windows.
    Waiting,
    Live,
    /// After quit captured the closing windows.
    Stopped,
}

pub(crate) struct ApplicationWindowSessions {
    store: RecordStore<WindowSession>,
    recording: Cell<Recording>,
    _closed: Subscription,
}

impl Global for ApplicationWindowSessions {}

pub(crate) fn user_path() -> Result<PathBuf, String> {
    directories::ProjectDirs::from("", "", "Knot")
        .map(|dirs| dirs.data_local_dir().join("window-session.sqlite3"))
        .ok_or_else(|| "cannot determine the user application-data directory".into())
}

/// Load the record and observe window closure.
pub(crate) fn install(path: PathBuf, executor: BackgroundExecutor, cx: &mut App) {
    let store = RecordStore::open(
        path,
        "window_session",
        WINDOW_SESSION_VERSION,
        WRITE_DEBOUNCE,
        executor,
    );
    let closed = cx.on_window_closed(|cx| cx.defer(record));
    cx.set_global(ApplicationWindowSessions {
        store,
        recording: Cell::new(Recording::Waiting),
        _closed: closed,
    });
}

/// Mark an unconsumed record consumed and resolve with it once that mark is
/// durable. Resolves `None` without a record, or when the mark cannot be
/// written, so a record that keeps crashing startup is never retried.
pub(crate) fn take_for_restore(cx: &App) -> Task<Option<WindowSession>> {
    let Some(sessions) = cx.try_global::<ApplicationWindowSessions>() else {
        return Task::ready(None);
    };
    let Some(record) = sessions
        .store
        .current()
        .filter(|record| !record.consumed && !record.windows.is_empty())
    else {
        return Task::ready(None);
    };
    sessions.store.publish(WindowSession {
        consumed: true,
        ..record.clone()
    });
    let flush = sessions.store.flush();
    cx.background_executor()
        .spawn(async move { flush.await.ok().map(|()| record) })
}

/// Record every product window from now on, starting with the current ones.
pub(crate) fn start_recording(cx: &mut App) {
    if let Some(sessions) = cx.try_global::<ApplicationWindowSessions>() {
        sessions.recording.set(Recording::Live);
    }
    record(cx);
}

/// Record the windows quit is about to close and stop recording.
pub(crate) fn record_for_quit(cx: &mut App) -> Option<Task<Result<(), String>>> {
    record(cx);
    let sessions = cx.try_global::<ApplicationWindowSessions>()?;
    sessions.recording.set(Recording::Stopped);
    Some(sessions.store.flush())
}

/// Re-record every product window's roots and layout.
pub(crate) fn record(cx: &mut App) {
    if cx
        .try_global::<ApplicationWindowSessions>()
        .is_none_or(|sessions| sessions.recording.get() != Recording::Live)
    {
        return;
    }
    let active = cx.active_window();
    let mut captured = Vec::new();
    for handle in cx.windows() {
        if handle.downcast::<ProductShell>().is_none() {
            continue;
        }
        let window = cx.update_window(handle, |root, window, cx| {
            let shell = root.downcast::<ProductShell>().unwrap();
            let shell = shell.read(cx);
            let recorded = RecordedWindow {
                roots: shell.root_uris().iter().map(ToString::to_string).collect(),
                layout: shell.capture_layout(window, cx),
            };
            (shell.creation_order(), active == Some(handle), recorded)
        });
        captured.extend(window);
    }
    captured.sort_by_key(|(order, _, _)| *order);
    let active_index = captured.iter().position(|(_, active, _)| *active);
    let windows = captured
        .into_iter()
        .map(|(_, _, window)| window)
        .collect::<Vec<_>>();
    cx.global::<ApplicationWindowSessions>()
        .store
        .publish(WindowSession {
            consumed: false,
            active: active_index,
            windows,
        });
}

#[cfg(test)]
impl ApplicationWindowSessions {
    pub(crate) fn publish_for_test(&self, record: WindowSession) {
        self.store.publish(record);
    }
}

/// Stop recording as a crash would, without capturing the windows.
#[cfg(test)]
pub(crate) fn stop_for_test(cx: &App) {
    if let Some(sessions) = cx.try_global::<ApplicationWindowSessions>() {
        sessions.recording.set(Recording::Stopped);
    }
}

#[cfg(test)]
pub(crate) fn current(cx: &App) -> Option<WindowSession> {
    cx.try_global::<ApplicationWindowSessions>()
        .and_then(|sessions| sessions.store.current())
}

#[cfg(test)]
pub(crate) fn flush(cx: &App) -> Task<Result<(), String>> {
    cx.global::<ApplicationWindowSessions>().store.flush()
}
