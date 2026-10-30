//! Commands recently confirmed in the command palette, most recent first,
//! kept in the application state database so their ranking survives
//! restarts.

use std::time::Duration;

use gpui::Global;
use serde::{Deserialize, Serialize};

use super::record_store::{RecordStore, StateDatabase};

const COMMAND_HISTORY_VERSION: u32 = 1;
const WRITE_DEBOUNCE: Duration = Duration::from_millis(300);
pub(crate) const COMMAND_HISTORY_LIMIT: usize = 50;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
struct CommandHistoryRecord {
    commands: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct CommandHistory {
    record: RecordStore<CommandHistoryRecord>,
}

impl CommandHistory {
    pub(crate) fn open(database: &StateDatabase) -> Self {
        Self {
            record: database.record("command_history", COMMAND_HISTORY_VERSION, WRITE_DEBOUNCE),
        }
    }

    /// Command names, most recent first.
    pub(crate) fn recent(&self) -> Vec<String> {
        self.record.current().unwrap_or_default().commands
    }

    /// Move a confirmed command to the front, dropping names that are no
    /// longer registered and keeping at most [`COMMAND_HISTORY_LIMIT`].
    pub(crate) fn record(&self, name: &str, registered: impl Fn(&str) -> bool) {
        let commands = std::iter::once(name.to_owned())
            .chain(
                self.recent()
                    .into_iter()
                    .filter(|recent| recent != name && registered(recent)),
            )
            .take(COMMAND_HISTORY_LIMIT)
            .collect();
        self.record.publish(CommandHistoryRecord { commands });
    }
}

pub(crate) struct ApplicationCommandHistory(pub(crate) CommandHistory);

impl Global for ApplicationCommandHistory {}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(path: &std::path::Path, cx: &gpui::TestAppContext) -> (StateDatabase, CommandHistory) {
        let database = StateDatabase::open(path.to_owned(), cx.background_executor.clone());
        let history = CommandHistory::open(&database);
        (database, history)
    }

    #[gpui::test]
    async fn records_most_recent_first_and_survives_reopen(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.sqlite3");
        let (database, history) = open(&path, cx);
        assert!(history.recent().is_empty());
        history.record("file.save", |_| true);
        history.record("file.open", |_| true);
        history.record("file.save", |_| true);
        assert_eq!(history.recent(), ["file.save", "file.open"]);
        database.flush().await.unwrap();
        drop((database, history));

        let (_, reopened) = open(&path, cx);
        assert_eq!(reopened.recent(), ["file.save", "file.open"]);
    }

    #[gpui::test]
    fn prunes_unregistered_names_and_keeps_the_limit(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (_database, history) = open(&dir.path().join("state.sqlite3"), cx);
        for index in 0..COMMAND_HISTORY_LIMIT + 5 {
            history.record(&format!("command.{index}"), |_| true);
        }
        let recent = history.recent();
        assert_eq!(recent.len(), COMMAND_HISTORY_LIMIT);
        assert_eq!(recent[0], format!("command.{}", COMMAND_HISTORY_LIMIT + 4));

        history.record("command.new", |name| name != "command.54");
        let recent = history.recent();
        assert_eq!(recent[0], "command.new");
        assert!(!recent.contains(&"command.54".to_owned()));
        assert_eq!(recent.len(), COMMAND_HISTORY_LIMIT);
    }
}
