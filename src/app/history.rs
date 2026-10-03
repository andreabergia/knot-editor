//! Session-local linear edit history for one buffer model.

use std::{
    ops::Range,
    time::{Duration, Instant},
};

use crate::core::{buffer::TextBuffer, transaction::EditTransaction};

const GROUPING_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditGroupKind {
    Typing,
    Composition,
    DeleteBackward,
    DeleteForward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EditGrouping {
    pub context: u64,
    pub kind: EditGroupKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryViewState {
    pub context: u64,
    pub anchor: usize,
    pub caret: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryViewChange {
    pub before: HistoryViewState,
    pub after: HistoryViewState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryReplay {
    pub content_state: u64,
    pub view_state: Option<HistoryViewState>,
}

struct HistoryEntry {
    transaction: EditTransaction,
    group: Option<GroupState>,
    before_state: u64,
    after_state: u64,
    view: Option<HistoryViewChange>,
}

#[derive(Clone, Debug)]
struct GroupState {
    grouping: EditGrouping,
    start: usize,
    end: usize,
    last_edit_at: Instant,
}

#[derive(Default)]
pub(crate) struct EditHistory {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

impl EditHistory {
    pub(crate) fn apply_replace(
        &mut self,
        buffer: &mut TextBuffer,
        range: Range<usize>,
        text: &str,
        grouping: Option<EditGrouping>,
        now: Instant,
        before_state: u64,
        after_state: u64,
        view: Option<HistoryViewChange>,
    ) {
        self.redo.clear();
        if let Some(grouping) = grouping
            && self
                .undo
                .last()
                .and_then(|entry| entry.group.as_ref())
                .is_some_and(|group| group.accepts(grouping, &range, text, now))
        {
            let entry = self.undo.last_mut().expect("grouped history entry exists");
            entry.transaction.replace(buffer, range.clone(), text);
            entry.group.as_mut().unwrap().advance(&range, text, now);
            entry.after_state = after_state;
            match (&mut entry.view, view) {
                (Some(entry_view), Some(view)) => entry_view.after = view.after,
                (None, None) => {}
                _ => entry.view = None,
            }
            return;
        }

        self.break_group();
        let mut transaction = EditTransaction::new();
        transaction.replace(buffer, range.clone(), text);
        self.undo.push(HistoryEntry {
            transaction,
            group: grouping.map(|grouping| GroupState::new(grouping, &range, text, now)),
            before_state,
            after_state,
            view,
        });
    }

    pub(crate) fn apply_batch<'a>(
        &mut self,
        buffer: &mut TextBuffer,
        edits: impl DoubleEndedIterator<Item = (Range<usize>, &'a str)>,
        before_state: u64,
        after_state: u64,
    ) {
        self.redo.clear();
        self.break_group();
        let mut transaction = EditTransaction::new();
        for (range, text) in edits.rev() {
            transaction.replace(buffer, range, text);
        }
        self.undo.push(HistoryEntry {
            transaction,
            group: None,
            before_state,
            after_state,
            view: None,
        });
    }

    pub(crate) fn break_group(&mut self) {
        if let Some(entry) = self.undo.last_mut() {
            entry.group = None;
        }
    }

    pub(crate) fn undo(&mut self, buffer: &mut TextBuffer) -> Option<HistoryReplay> {
        let mut entry = self.undo.pop()?;
        entry.group = None;
        entry.transaction.prepare_for_history_replay(buffer);
        entry.transaction.undo(buffer);
        let replay = HistoryReplay {
            content_state: entry.before_state,
            view_state: entry.view.as_ref().map(|view| view.before.clone()),
        };
        self.redo.push(entry);
        self.break_group();
        Some(replay)
    }

    pub(crate) fn redo(&mut self, buffer: &mut TextBuffer) -> Option<HistoryReplay> {
        let mut entry = self.redo.pop()?;
        entry.transaction.prepare_for_history_replay(buffer);
        entry.transaction.redo(buffer);
        let replay = HistoryReplay {
            content_state: entry.after_state,
            view_state: entry.view.as_ref().map(|view| view.after.clone()),
        };
        entry.group = None;
        self.undo.push(entry);
        self.break_group();
        Some(replay)
    }
}

impl GroupState {
    fn new(grouping: EditGrouping, range: &Range<usize>, text: &str, now: Instant) -> Self {
        Self {
            grouping,
            start: range.start,
            end: range.start + text.len(),
            last_edit_at: now,
        }
    }

    fn accepts(
        &self,
        grouping: EditGrouping,
        range: &Range<usize>,
        text: &str,
        now: Instant,
    ) -> bool {
        if self.grouping != grouping {
            return false;
        }
        if grouping.kind != EditGroupKind::Composition
            && now.saturating_duration_since(self.last_edit_at) > GROUPING_TIMEOUT
        {
            return false;
        }
        match grouping.kind {
            EditGroupKind::Typing | EditGroupKind::Composition => {
                !text.is_empty()
                    && ((range.is_empty() && range.start == self.end)
                        || (range.start == self.start && range.end == self.end))
            }
            EditGroupKind::DeleteBackward => {
                text.is_empty() && !range.is_empty() && range.end == self.start
            }
            EditGroupKind::DeleteForward => {
                text.is_empty() && !range.is_empty() && range.start == self.start
            }
        }
    }

    fn advance(&mut self, range: &Range<usize>, text: &str, now: Instant) {
        match self.grouping.kind {
            EditGroupKind::Typing | EditGroupKind::Composition => {
                self.end = range.start + text.len()
            }
            EditGroupKind::DeleteBackward => {
                self.start = range.start;
                self.end = range.start;
            }
            EditGroupKind::DeleteForward => self.end = self.start,
        }
        self.last_edit_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grouping(context: u64, kind: EditGroupKind) -> EditGrouping {
        EditGrouping { context, kind }
    }

    #[test]
    fn groups_typing_and_both_deletion_directions() {
        let now = Instant::now();
        let mut buffer = TextBuffer::from_text("");
        let mut history = EditHistory::default();
        for (at, text) in [(0, "a"), (1, "β"), (3, "c")] {
            history.apply_replace(
                &mut buffer,
                at..at,
                text,
                Some(grouping(1, EditGroupKind::Typing)),
                now,
                0,
                1,
                None,
            );
        }
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "");
        assert!(history.redo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "aβc");

        for range in [3..4, 1..3, 0..1] {
            history.apply_replace(
                &mut buffer,
                range,
                "",
                Some(grouping(1, EditGroupKind::DeleteBackward)),
                now,
                1,
                2,
                None,
            );
        }
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "aβc");

        for range in [0..1, 0..2, 0..1] {
            history.apply_replace(
                &mut buffer,
                range,
                "",
                Some(grouping(1, EditGroupKind::DeleteForward)),
                now,
                1,
                2,
                None,
            );
        }
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "aβc");
    }

    #[test]
    fn boundaries_and_divergent_edits_split_or_clear_history() {
        let now = Instant::now();
        let mut buffer = TextBuffer::from_text("");
        let mut history = EditHistory::default();
        history.apply_replace(
            &mut buffer,
            0..0,
            "a",
            Some(grouping(1, EditGroupKind::Typing)),
            now,
            0,
            1,
            None,
        );
        history.break_group();
        history.apply_replace(
            &mut buffer,
            1..1,
            "b",
            Some(grouping(1, EditGroupKind::Typing)),
            now,
            1,
            2,
            None,
        );
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "a");
        history.apply_replace(
            &mut buffer,
            1..1,
            "c",
            Some(grouping(1, EditGroupKind::Typing)),
            now,
            1,
            3,
            None,
        );
        assert!(history.redo(&mut buffer).is_none());
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "a");
    }

    #[test]
    fn context_and_elapsed_time_break_groups() {
        let now = Instant::now();
        let mut buffer = TextBuffer::from_text("");
        let mut history = EditHistory::default();
        history.apply_replace(
            &mut buffer,
            0..0,
            "a",
            Some(grouping(1, EditGroupKind::Typing)),
            now,
            0,
            1,
            None,
        );
        history.apply_replace(
            &mut buffer,
            1..1,
            "b",
            Some(grouping(2, EditGroupKind::Typing)),
            now,
            1,
            2,
            None,
        );
        history.apply_replace(
            &mut buffer,
            2..2,
            "c",
            Some(grouping(2, EditGroupKind::Typing)),
            now + GROUPING_TIMEOUT + Duration::from_millis(1),
            2,
            3,
            None,
        );
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "ab");
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "a");
        assert!(history.undo(&mut buffer).is_some());
        assert_eq!(buffer.read_range(0..buffer.len()), "");
    }

    #[test]
    fn composition_remains_one_group_across_the_typing_timeout() {
        let now = Instant::now();
        let mut buffer = TextBuffer::from_text("");
        let mut history = EditHistory::default();
        let composition = Some(grouping(1, EditGroupKind::Composition));
        history.apply_replace(&mut buffer, 0..0, "に", composition, now, 0, 1, None);
        history.apply_replace(
            &mut buffer,
            0.."に".len(),
            "日本",
            composition,
            now + GROUPING_TIMEOUT + Duration::from_secs(5),
            1,
            2,
            None,
        );

        assert_eq!(history.undo(&mut buffer).unwrap().content_state, 0);
        assert_eq!(buffer.read_range(0..buffer.len()), "");
    }

    #[test]
    fn grouped_history_retains_the_first_before_and_last_after_view_state() {
        let now = Instant::now();
        let mut buffer = TextBuffer::from_text("abcd");
        let mut history = EditHistory::default();
        let first = HistoryViewChange {
            before: HistoryViewState {
                context: 7,
                anchor: 3,
                caret: 1,
            },
            after: HistoryViewState {
                context: 7,
                anchor: 2,
                caret: 2,
            },
        };
        history.apply_replace(
            &mut buffer,
            1..3,
            "x",
            Some(grouping(7, EditGroupKind::Typing)),
            now,
            0,
            1,
            Some(first.clone()),
        );
        let last = HistoryViewChange {
            before: first.after,
            after: HistoryViewState {
                context: 7,
                anchor: 3,
                caret: 3,
            },
        };
        history.apply_replace(
            &mut buffer,
            2..2,
            "y",
            Some(grouping(7, EditGroupKind::Typing)),
            now,
            1,
            2,
            Some(last.clone()),
        );

        assert_eq!(
            history.undo(&mut buffer).unwrap().view_state,
            Some(first.before)
        );
        assert_eq!(
            history.redo(&mut buffer).unwrap().view_state,
            Some(last.after)
        );
    }
}
