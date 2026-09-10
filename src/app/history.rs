//! Session-local linear edit history for one buffer model.

use std::{ops::Range, time::{Duration, Instant}};

use crate::core::{buffer::TextBuffer, transaction::EditTransaction};

const GROUPING_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditGroupKind {
    Typing,
    DeleteBackward,
    DeleteForward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EditGrouping {
    pub context: u64,
    pub kind: EditGroupKind,
}

struct HistoryEntry {
    transaction: EditTransaction,
    group: Option<GroupState>,
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
            return;
        }

        self.break_group();
        let mut transaction = EditTransaction::new();
        transaction.replace(buffer, range.clone(), text);
        self.undo.push(HistoryEntry {
            transaction,
            group: grouping.map(|grouping| GroupState::new(grouping, &range, text, now)),
        });
    }

    pub(crate) fn apply_batch<'a>(
        &mut self,
        buffer: &mut TextBuffer,
        edits: impl DoubleEndedIterator<Item = (Range<usize>, &'a str)>,
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
        });
    }

    pub(crate) fn break_group(&mut self) {
        if let Some(entry) = self.undo.last_mut() {
            entry.group = None;
        }
    }

    pub(crate) fn undo(&mut self, buffer: &mut TextBuffer) -> bool {
        let Some(mut entry) = self.undo.pop() else {
            return false;
        };
        entry.group = None;
        entry.transaction.prepare_for_history_replay(buffer);
        entry.transaction.undo(buffer);
        self.redo.push(entry);
        self.break_group();
        true
    }

    pub(crate) fn redo(&mut self, buffer: &mut TextBuffer) -> bool {
        let Some(mut entry) = self.redo.pop() else {
            return false;
        };
        entry.transaction.prepare_for_history_replay(buffer);
        entry.transaction.redo(buffer);
        entry.group = None;
        self.undo.push(entry);
        self.break_group();
        true
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
        if self.grouping != grouping
            || now.saturating_duration_since(self.last_edit_at) > GROUPING_TIMEOUT
        {
            return false;
        }
        match grouping.kind {
            EditGroupKind::Typing => {
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
            EditGroupKind::Typing => self.end = range.start + text.len(),
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
            );
        }
        assert!(history.undo(&mut buffer));
        assert_eq!(buffer.read_range(0..buffer.len()), "");
        assert!(history.redo(&mut buffer));
        assert_eq!(buffer.read_range(0..buffer.len()), "aβc");

        for range in [3..4, 1..3, 0..1] {
            history.apply_replace(
                &mut buffer,
                range,
                "",
                Some(grouping(1, EditGroupKind::DeleteBackward)),
                now,
            );
        }
        assert!(history.undo(&mut buffer));
        assert_eq!(buffer.read_range(0..buffer.len()), "aβc");

        for range in [0..1, 0..2, 0..1] {
            history.apply_replace(
                &mut buffer,
                range,
                "",
                Some(grouping(1, EditGroupKind::DeleteForward)),
                now,
            );
        }
        assert!(history.undo(&mut buffer));
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
        );
        history.break_group();
        history.apply_replace(
            &mut buffer,
            1..1,
            "b",
            Some(grouping(1, EditGroupKind::Typing)),
            now,
        );
        assert!(history.undo(&mut buffer));
        assert_eq!(buffer.read_range(0..buffer.len()), "a");
        history.apply_replace(
            &mut buffer,
            1..1,
            "c",
            Some(grouping(1, EditGroupKind::Typing)),
            now,
        );
        assert!(!history.redo(&mut buffer));
        assert!(history.undo(&mut buffer));
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
        );
        history.apply_replace(
            &mut buffer,
            1..1,
            "b",
            Some(grouping(2, EditGroupKind::Typing)),
            now,
        );
        history.apply_replace(
            &mut buffer,
            2..2,
            "c",
            Some(grouping(2, EditGroupKind::Typing)),
            now + GROUPING_TIMEOUT + Duration::from_millis(1),
        );
        assert!(history.undo(&mut buffer));
        assert_eq!(buffer.read_range(0..buffer.len()), "ab");
        assert!(history.undo(&mut buffer));
        assert_eq!(buffer.read_range(0..buffer.len()), "a");
        assert!(history.undo(&mut buffer));
        assert_eq!(buffer.read_range(0..buffer.len()), "");
    }
}
