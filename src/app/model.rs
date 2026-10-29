//! Foreground-owned editor document state.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    sync::Arc,
    time::Instant,
};

use gpui::{AppContext, Context, Entity, Subscription, WeakEntity};

use crate::{
    core::{
        anchored_range::{AnchoredRangeId, AnchoredRangeStore},
        buffer::{BufferEdit, TextBuffer},
    },
    host::protocol::{
        BufferHandle, BufferSubscriptionId, ByteRange, DecorationToken, EditorContribution,
        ExtensionId, ExtensionLifecycleId, GutterToken, SnapshotText,
    },
};

use super::{
    history::{EditGrouping, EditHistory, HistoryViewChange, HistoryViewState},
    syntax::{FOREGROUND_BUDGET, ParsedTree, SyntaxLanguage, SyntaxState},
    theme::Highlight,
};

/// The extension lifecycle whose complete contribution set is stored in this buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ContributionSource {
    pub(crate) extension: ExtensionId,
    pub(crate) lifecycle: ExtensionLifecycleId,
}

#[cfg(test)]
impl ContributionSource {
    pub(crate) fn fixture() -> Self {
        Self {
            extension: ExtensionId::new(1),
            lifecycle: ExtensionLifecycleId::new(1),
        }
    }
}

impl DecorationToken {
    fn precedence(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedEditorContribution {
    pub range: ByteRange,
    pub source: ContributionSource,
    pub decoration: Option<DecorationToken>,
    pub gutter: Option<GutterToken>,
    pub command: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContributionError {
    Closed,
    InvalidRange,
    RevisionConflict,
    NotFound,
}

/// Whether foreground-owned text mutations are permitted for a buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BufferAccessPolicy {
    Editable,
    ReadOnly,
}

struct ContributionSet {
    anchored_ranges: Vec<AnchoredRangeId>,
}

#[derive(Clone)]
struct ContributionMetadata {
    source: ContributionSource,
    decoration: Option<DecorationToken>,
    gutter: Option<GutterToken>,
    command: Option<String>,
}

struct ContributionRegistry {
    sets: HashMap<ContributionSource, ContributionSet>,
    metadata: HashMap<AnchoredRangeId, ContributionMetadata>,
}

impl ContributionRegistry {
    fn new() -> Self {
        Self {
            sets: HashMap::new(),
            metadata: HashMap::new(),
        }
    }
}

/// The authoritative document owned by gpui's foreground thread.
///
/// Commit methods take no gpui context, so a caller that changes the text
/// notifies the model. Views, recovery capture, and background syntax parses
/// all start from that notification.
pub struct BufferModel {
    buffer: TextBuffer,
    access_policy: BufferAccessPolicy,
    anchored_ranges: AnchoredRangeStore,
    contributions: ContributionRegistry,
    revision: u64,
    content_state: u64,
    next_content_state: u64,
    revision_content_states: Vec<u64>,
    open: bool,
    pending_changes: VecDeque<CommittedBufferChange>,
    snapshot_cache: RefCell<Option<CachedSnapshot>>,
    history: EditHistory,
    history_replay_generation: u64,
    history_view_restore: Option<HistoryViewRestore>,
    /// Derived syntax tree, kept current with every commit.
    syntax: Option<SyntaxState>,
    /// Starts a background parse after any notify that leaves the tree
    /// behind the text, so commit paths need no gpui context.
    syntax_driver: Option<Subscription>,
    /// Whole-document reads, so tests can assert that views never make them.
    #[cfg(test)]
    whole_text_reads: std::cell::Cell<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HistoryViewRestore {
    pub generation: u64,
    revision: u64,
    pub state: HistoryViewState,
}

#[derive(Clone)]
struct CachedSnapshot {
    revision: u64,
    range: ByteRange,
    text: Arc<[u16]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommittedBufferChange {
    pub before_revision: u64,
    pub revision: u64,
    pub edits: Vec<crate::host::protocol::TextEdit>,
}

impl BufferModel {
    pub fn from_text(text: impl Into<Arc<str>>) -> Self {
        Self::with_access_policy(text, BufferAccessPolicy::Editable)
    }

    pub(crate) fn from_read_only_text(text: impl Into<Arc<str>>) -> Self {
        Self::with_access_policy(text, BufferAccessPolicy::ReadOnly)
    }

    /// A model whose revision 0 holds `baseline` and whose current content is
    /// `text`, committed as one edit with an empty undo history. Dirty state
    /// then derives from comparing against the baseline revision, so text equal
    /// to its baseline is clean.
    pub(crate) fn from_recovered_text(baseline: impl Into<Arc<str>>, text: &str) -> Self {
        let mut model = Self::from_text(baseline);
        if model.text() != text {
            let len = model.buffer.len();
            model.commit_replacement(0..len, text, None, None);
        }
        model.history = EditHistory::default();
        model.pending_changes.clear();
        model
    }

    fn with_access_policy(text: impl Into<Arc<str>>, access_policy: BufferAccessPolicy) -> Self {
        Self {
            buffer: TextBuffer::from_text(text),
            access_policy,
            anchored_ranges: AnchoredRangeStore::new(),
            contributions: ContributionRegistry::new(),
            revision: 0,
            content_state: 0,
            next_content_state: 1,
            revision_content_states: vec![0],
            open: true,
            pending_changes: VecDeque::new(),
            snapshot_cache: RefCell::new(None),
            history: EditHistory::default(),
            history_replay_generation: 0,
            history_view_restore: None,
            syntax: None,
            syntax_driver: None,
            #[cfg(test)]
            whole_text_reads: Default::default(),
        }
    }

    pub fn text(&self) -> String {
        self.read(0..self.buffer.len())
    }

    /// Text in a byte range on character boundaries.
    pub(crate) fn read(&self, range: Range<usize>) -> String {
        #[cfg(test)]
        if range.start == 0 && range.end == self.buffer.len() && !range.is_empty() {
            self.whole_text_reads.set(self.whole_text_reads.get() + 1);
        }
        self.buffer.read_range(range)
    }

    #[cfg(test)]
    pub(crate) fn whole_text_reads(&self) -> usize {
        self.whole_text_reads.get()
    }

    pub(crate) fn len(&self) -> usize {
        self.buffer.len()
    }

    pub(crate) fn is_char_boundary(&self, offset: usize) -> bool {
        self.buffer.is_char_boundary(offset)
    }

    /// Number of lines; always at least one.
    pub(crate) fn line_count(&self) -> usize {
        self.buffer.line_count()
    }

    /// Byte range of `line`, excluding its `\n` terminator but including any
    /// preceding `\r`.
    pub(crate) fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.buffer.line_start(line);
        let end = if line + 1 < self.buffer.line_count() {
            self.buffer.line_start(line + 1) - 1
        } else {
            self.buffer.len()
        };
        start..end
    }

    /// The terminator inserted newlines should use: `\r\n` when the first
    /// line ends with one, otherwise `\n`.
    pub(crate) fn line_ending(&self) -> &'static str {
        if self.buffer.line_count() < 2 {
            return "\n";
        }
        let end = self.line_range(0).end;
        if end > 0 && self.is_char_boundary(end - 1) && self.read(end - 1..end) == "\r" {
            "\r\n"
        } else {
            "\n"
        }
    }

    pub(crate) fn line_of_offset(&self, offset: usize) -> usize {
        self.buffer.line_of_offset(offset)
    }

    /// Document-wide UTF-16 offset of a byte offset on a character boundary.
    pub(crate) fn utf16_of_offset(&self, offset: usize) -> usize {
        self.buffer.utf16_of_offset(offset)
    }

    /// Byte offset of a document-wide UTF-16 offset. Offsets inside a
    /// surrogate pair round up; offsets past the end clamp.
    pub(crate) fn offset_of_utf16(&self, utf16: usize) -> usize {
        self.buffer.offset_of_utf16(utf16)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn content_matches_revision(&self, revision: u64) -> bool {
        usize::try_from(revision)
            .ok()
            .and_then(|revision| self.revision_content_states.get(revision))
            .is_some_and(|state| *state == self.content_state)
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn is_editable(&self) -> bool {
        self.open && self.access_policy == BufferAccessPolicy::Editable
    }

    /// Atomically replace one source-owned set against the current revision.
    pub(crate) fn replace_contributions(
        &mut self,
        source: ContributionSource,
        contributions: &[EditorContribution],
        if_revision: u64,
    ) -> Result<(), ContributionError> {
        if !self.open {
            return Err(ContributionError::Closed);
        }
        if if_revision != self.revision {
            return Err(ContributionError::RevisionConflict);
        }

        let mut validated = Vec::with_capacity(contributions.len());
        for contribution in contributions {
            let range = self
                .checked_range(contribution.range)
                .map_err(|error| match error {
                    BufferAccessError::Closed => ContributionError::Closed,
                    _ => ContributionError::InvalidRange,
                })?;
            if self.buffer.is_empty() {
                return Err(ContributionError::InvalidRange);
            }
            validated.push((
                range,
                contribution.decoration,
                contribution.gutter,
                contribution.command.clone(),
            ));
        }

        if let Some(previous) = self.contributions.sets.remove(&source) {
            for id in previous.anchored_ranges {
                self.anchored_ranges.remove(id);
                self.contributions.metadata.remove(&id);
            }
        }

        let mut anchored_range_ids = Vec::with_capacity(validated.len());
        for (range, decoration, gutter, command) in validated {
            let id = self
                .anchored_ranges
                .add(&self.buffer, range.start, range.end);
            self.contributions.metadata.insert(
                id,
                ContributionMetadata {
                    source,
                    decoration,
                    gutter,
                    command,
                },
            );
            anchored_range_ids.push(id);
        }
        self.contributions.sets.insert(
            source,
            ContributionSet {
                anchored_ranges: anchored_range_ids,
            },
        );
        Ok(())
    }

    pub(crate) fn dispose_contributions(
        &mut self,
        source: ContributionSource,
    ) -> Result<(), ContributionError> {
        if !self.open {
            return Err(ContributionError::Closed);
        }
        if !self.contributions.sets.contains_key(&source) {
            return Err(ContributionError::NotFound);
        }
        self.remove_contribution_set(source);
        Ok(())
    }

    pub(crate) fn remove_contribution_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        let sources: Vec<_> = self
            .contributions
            .sets
            .keys()
            .copied()
            .filter(|source| {
                *source
                    == ContributionSource {
                        extension,
                        lifecycle,
                    }
            })
            .collect();
        let changed = !sources.is_empty();
        for source in sources {
            self.remove_contribution_set(source);
        }
        changed
    }

    fn remove_contribution_set(&mut self, source: ContributionSource) {
        let Some(set) = self.contributions.sets.remove(&source) else {
            return;
        };
        for id in set.anchored_ranges {
            self.anchored_ranges.remove(id);
            self.contributions.metadata.remove(&id);
        }
    }

    /// Feed a commit's primitive edits to every edit-log observer, then
    /// discard them.
    fn observe_committed_edits(&mut self, seq: usize) {
        if let Some(syntax) = &mut self.syntax {
            syntax.edit(self.buffer.edits_since(seq));
            syntax.parse_in_foreground(&self.buffer.snapshot(), FOREGROUND_BUDGET);
        }
        let consumed = self.anchored_ranges.stabilize(&self.buffer).consumed;
        self.buffer.discard_edits_before(self.buffer.edit_seq());
        if consumed.is_empty() {
            return;
        }

        let consumed = consumed.into_iter().collect::<HashSet<_>>();
        self.contributions
            .metadata
            .retain(|id, _| !consumed.contains(id));
        for set in self.contributions.sets.values_mut() {
            set.anchored_ranges.retain(|id| !consumed.contains(id));
        }
    }

    pub(crate) fn resolved_contributions(&self) -> Vec<ResolvedEditorContribution> {
        let mut resolved = self
            .contributions
            .metadata
            .iter()
            .filter_map(|(&id, metadata)| {
                self.anchored_ranges
                    .resolve(&self.buffer, id)
                    .map(|range| (metadata, id, range))
            })
            .collect::<Vec<_>>();
        resolved.sort_unstable_by_key(|(metadata, id, _)| {
            (
                metadata
                    .decoration
                    .map(DecorationToken::precedence)
                    .unwrap_or(0),
                *id,
            )
        });
        resolved
            .into_iter()
            .map(|(metadata, _, range)| ResolvedEditorContribution {
                range: ByteRange {
                    start_byte_offset: range.start,
                    end_byte_offset: range.end,
                },
                source: metadata.source,
                decoration: metadata.decoration,
                gutter: metadata.gutter,
                command: metadata.command.clone(),
            })
            .collect()
    }

    /// Parse this buffer as `language`, discarding any previous tree, or stop
    /// parsing with `None`.
    // No language registration attaches syntax yet.
    #[allow(dead_code)]
    pub(crate) fn set_syntax(
        &mut self,
        language: Option<Arc<SyntaxLanguage>>,
        cx: &mut Context<Self>,
    ) {
        self.syntax = language.map(|language| {
            let mut syntax = SyntaxState::new(language);
            syntax.parse_in_foreground(&self.buffer.snapshot(), FOREGROUND_BUDGET);
            syntax
        });
        if self.syntax.is_some() && self.syntax_driver.is_none() {
            self.syntax_driver = Some(cx.observe_self(Self::continue_syntax_parse));
        }
        cx.notify();
    }

    fn continue_syntax_parse(&mut self, cx: &mut Context<Self>) {
        let Some(syntax) = self
            .syntax
            .as_mut()
            .filter(|syntax| syntax.needs_background_parse())
        else {
            return;
        };
        let parse = syntax.start_background_parse(self.buffer.snapshot());
        cx.spawn(async move |this, cx| {
            let parsed = cx.background_spawn(async move { parse.run() }).await;
            this.update(cx, |model, cx| model.finish_syntax_parse(parsed, cx))
        })
        .detach();
    }

    fn finish_syntax_parse(&mut self, parsed: ParsedTree, cx: &mut Context<Self>) {
        let Some(syntax) = &mut self.syntax else {
            return;
        };
        if syntax.finish_background_parse(parsed) {
            syntax.parse_in_foreground(&self.buffer.snapshot(), FOREGROUND_BUDGET);
            cx.notify();
        }
    }

    /// Identifies the current syntax highlights; `None` without syntax.
    pub(crate) fn syntax_generation(&self) -> Option<u64> {
        self.syntax.as_ref().map(SyntaxState::generation)
    }

    /// Sorted, non-overlapping syntax highlights within `range`.
    pub(crate) fn syntax_highlights(&self, range: Range<usize>) -> Vec<(Range<usize>, Highlight)> {
        self.syntax.as_ref().map_or_else(Vec::new, |syntax| {
            syntax.highlights(&self.buffer.snapshot(), range)
        })
    }

    pub(crate) fn add_view_position(&mut self, range: Range<usize>) -> Option<AnchoredRangeId> {
        if self.buffer.is_empty() {
            return None;
        }
        Some(
            self.anchored_ranges
                .add_persistent(&self.buffer, range.start, range.end),
        )
    }

    pub(crate) fn replace_view_position(
        &mut self,
        id: Option<AnchoredRangeId>,
        range: Range<usize>,
    ) -> Option<AnchoredRangeId> {
        if self.buffer.is_empty() {
            return id;
        }
        if let Some(id) = id {
            self.anchored_ranges.remove(id);
        }
        self.add_view_position(range)
    }

    pub(crate) fn resolve_view_position(
        &self,
        id: Option<AnchoredRangeId>,
    ) -> Option<Range<usize>> {
        self.anchored_ranges.resolve(&self.buffer, id?)
    }

    pub(crate) fn remove_view_position(&mut self, id: Option<AnchoredRangeId>) {
        if let Some(id) = id {
            self.anchored_ranges.remove(id);
        }
    }

    /// Validate a public UTF-8 byte range before it reaches `TextBuffer`.
    ///
    /// Core buffer operations deliberately assert their preconditions. The
    /// foreground editor host converts externally supplied offsets into this
    /// recoverable result instead.
    pub(crate) fn checked_range(
        &self,
        range: ByteRange,
    ) -> Result<Range<usize>, BufferAccessError> {
        if !self.open {
            return Err(BufferAccessError::Closed);
        }

        let range = range.start_byte_offset..range.end_byte_offset;
        if range.start > range.end
            || range.end > self.buffer.len()
            || !self.buffer.is_char_boundary(range.start)
            || !self.buffer.is_char_boundary(range.end)
        {
            return Err(BufferAccessError::InvalidRange);
        }
        Ok(range)
    }

    /// Read a host-validated UTF-8 byte range without exposing core's
    /// assertion-based API to the extension boundary.
    pub(crate) fn read_checked(&self, range: ByteRange) -> Result<String, BufferAccessError> {
        let range = self.checked_range(range)?;
        Ok(self.buffer.read_range(range))
    }

    pub(crate) fn snapshot(
        &self,
        range: Option<ByteRange>,
    ) -> Result<crate::host::protocol::TextSnapshot, BufferAccessError> {
        let range = range.unwrap_or(ByteRange {
            start_byte_offset: 0,
            end_byte_offset: self.buffer.len(),
        });
        if let Some(cached) = self.snapshot_cache.borrow().as_ref()
            && cached.revision == self.revision
            && cached.range == range
        {
            return Ok(crate::host::protocol::TextSnapshot {
                text: SnapshotText(Arc::clone(&cached.text)),
                range,
                revision: self.revision,
            });
        }

        let text = SnapshotText::from_utf8(&self.read_checked(range)?).0;
        *self.snapshot_cache.borrow_mut() = Some(CachedSnapshot {
            revision: self.revision,
            range,
            text: Arc::clone(&text),
        });
        Ok(crate::host::protocol::TextSnapshot {
            text: SnapshotText(text),
            range,
            revision: self.revision,
        })
    }

    /// Validate and atomically apply an extension edit batch.
    ///
    /// Ranges remain in the pre-commit coordinate space because mutations run
    /// from the final range backwards.
    pub(crate) fn apply_edits(
        &mut self,
        edits: &[crate::host::protocol::TextEdit],
        if_revision: u64,
    ) -> Result<bool, BufferAccessError> {
        if !self.open {
            return Err(BufferAccessError::Closed);
        }
        if self.access_policy == BufferAccessPolicy::ReadOnly {
            return Err(BufferAccessError::ReadOnly);
        }
        if if_revision != self.revision {
            return Err(BufferAccessError::RevisionConflict);
        }

        let mut validated = Vec::with_capacity(edits.len());
        let mut previous: Option<Range<usize>> = None;
        let mut changed = false;
        for edit in edits {
            let range = self.checked_range(edit.range)?;
            if let Some(previous) = &previous
                && (range.start < previous.start || range.start < previous.end)
            {
                return Err(BufferAccessError::InvalidEditBatch);
            }
            if self.buffer.read_range(range.clone()) != edit.text {
                changed = true;
            }
            previous = Some(range.clone());
            validated.push((range, edit.text.as_str()));
        }

        if !changed {
            return Ok(false);
        }
        let before_state = self.content_state;
        let after_state = self.allocate_content_state();
        let seq = self.buffer.edit_seq();
        self.history.apply_batch(
            &mut self.buffer,
            validated.into_iter(),
            before_state,
            after_state,
        );
        self.content_state = after_state;
        self.observe_committed_edits(seq);
        let before_revision = self.revision;
        self.advance_revision();
        self.pending_changes.push_back(CommittedBufferChange {
            before_revision,
            revision: self.revision,
            edits: edits.to_vec(),
        });
        Ok(true)
    }

    /// Apply one editor-visible local replacement as one public commit.
    ///
    /// `TextBuffer::replace` may emit multiple primitive edit-log entries;
    /// that private cursor is deliberately independent of `revision`.
    pub(crate) fn replace(
        &mut self,
        range: Range<usize>,
        text: &str,
    ) -> Result<bool, BufferAccessError> {
        self.replace_recorded(range, text, None, None)
    }

    pub(crate) fn replace_from_view(
        &mut self,
        range: Range<usize>,
        text: &str,
        view: HistoryViewChange,
    ) -> Result<bool, BufferAccessError> {
        self.replace_recorded(range, text, None, Some(view))
    }

    fn replace_recorded(
        &mut self,
        range: Range<usize>,
        text: &str,
        grouping: Option<EditGrouping>,
        view: Option<HistoryViewChange>,
    ) -> Result<bool, BufferAccessError> {
        assert!(self.open, "cannot edit a closed buffer");
        if self.access_policy == BufferAccessPolicy::ReadOnly {
            return Err(BufferAccessError::ReadOnly);
        }
        Ok(self.commit_replacement(range, text, grouping, view))
    }

    /// Replace the complete text of a read-only generated buffer. Generated
    /// content is not an editable document, so this bypasses the access policy
    /// while still committing one public revision.
    pub(crate) fn replace_generated_text(&mut self, text: &str) -> bool {
        assert!(self.open, "cannot edit a closed buffer");
        assert!(
            self.access_policy == BufferAccessPolicy::ReadOnly,
            "generated replacement requires a read-only buffer"
        );
        if self.text() == text {
            return false;
        }
        self.commit_replacement(0..self.buffer.len(), text, None, None)
    }

    fn commit_replacement(
        &mut self,
        range: Range<usize>,
        text: &str,
        grouping: Option<EditGrouping>,
        view: Option<HistoryViewChange>,
    ) -> bool {
        if range.is_empty() && text.is_empty() {
            return false;
        }
        let edit = crate::host::protocol::TextEdit {
            range: ByteRange {
                start_byte_offset: range.start,
                end_byte_offset: range.end,
            },
            text: text.into(),
        };
        let before_state = self.content_state;
        let after_state = self.allocate_content_state();
        let seq = self.buffer.edit_seq();
        self.history.apply_replace(
            &mut self.buffer,
            (range, text),
            grouping,
            Instant::now(),
            (before_state, after_state),
            view,
        );
        self.content_state = after_state;
        self.observe_committed_edits(seq);
        let before_revision = self.revision;
        self.advance_revision();
        self.pending_changes.push_back(CommittedBufferChange {
            before_revision,
            revision: self.revision,
            edits: vec![edit],
        });
        true
    }

    #[cfg(test)]
    pub(crate) fn replace_grouped(
        &mut self,
        range: Range<usize>,
        text: &str,
        grouping: EditGrouping,
    ) -> Result<bool, BufferAccessError> {
        self.replace_recorded(range, text, Some(grouping), None)
    }

    pub(crate) fn replace_grouped_from_view(
        &mut self,
        range: Range<usize>,
        text: &str,
        grouping: EditGrouping,
        view: HistoryViewChange,
    ) -> Result<bool, BufferAccessError> {
        self.replace_recorded(range, text, Some(grouping), Some(view))
    }

    pub(crate) fn break_history_group(&mut self) {
        self.history.break_group();
    }

    pub(crate) fn undo(&mut self) -> Result<bool, BufferAccessError> {
        self.replay_history(true)
    }

    pub(crate) fn redo(&mut self) -> Result<bool, BufferAccessError> {
        self.replay_history(false)
    }

    fn replay_history(&mut self, undo: bool) -> Result<bool, BufferAccessError> {
        assert!(self.open, "cannot edit a closed buffer");
        if self.access_policy == BufferAccessPolicy::ReadOnly {
            return Err(BufferAccessError::ReadOnly);
        }
        let seq = self.buffer.edit_seq();
        let state = if undo {
            self.history.undo(&mut self.buffer)
        } else {
            self.history.redo(&mut self.buffer)
        };
        let Some(state) = state else {
            return Ok(false);
        };
        self.content_state = state.content_state;
        let view_state = state.view_state;
        let edit = self.covering_edit(seq);
        self.observe_committed_edits(seq);
        let before_revision = self.revision;
        self.advance_revision();
        if let Some(view_state) = view_state {
            self.history_replay_generation = self
                .history_replay_generation
                .checked_add(1)
                .expect("history replay generation overflowed");
            self.history_view_restore = Some(HistoryViewRestore {
                generation: self.history_replay_generation,
                revision: self.revision,
                state: view_state,
            });
        }
        self.pending_changes.push_back(CommittedBufferChange {
            before_revision,
            revision: self.revision,
            edits: vec![edit],
        });
        Ok(true)
    }

    pub(crate) fn history_view_restore(&self) -> Option<&HistoryViewRestore> {
        self.history_view_restore
            .as_ref()
            .filter(|restore| restore.revision == self.revision)
    }

    pub(crate) fn take_pending_change(&mut self) -> Option<CommittedBufferChange> {
        self.pending_changes.pop_front()
    }

    fn advance_revision(&mut self) {
        self.snapshot_cache.get_mut().take();
        self.revision = self
            .revision
            .checked_add(1)
            .expect("public buffer revision overflowed");
        self.revision_content_states.push(self.content_state);
    }

    fn allocate_content_state(&mut self) -> u64 {
        let state = self.next_content_state;
        self.next_content_state = self
            .next_content_state
            .checked_add(1)
            .expect("buffer content identity space exhausted");
        state
    }

    pub(crate) fn close(&mut self) {
        self.snapshot_cache.get_mut().take();
        self.anchored_ranges = AnchoredRangeStore::new();
        self.contributions = ContributionRegistry::new();
        self.pending_changes.clear();
        self.syntax = None;
        self.syntax_driver = None;
        self.open = false;
    }

    /// One replacement, in pre-edit coordinates, covering every primitive
    /// applied since `seq`. Reads only the replaced span.
    fn covering_edit(&self, seq: usize) -> crate::host::protocol::TextEdit {
        // `start..end` is the changed span in current coordinates, and
        // `before_len` its length before the first primitive.
        let mut span: Option<(usize, usize, usize)> = None;
        for edit in self.buffer.edits_since(seq) {
            let (at, removed, inserted) = match edit {
                BufferEdit::Insert {
                    at, inserted_len, ..
                } => (*at, 0, *inserted_len),
                BufferEdit::Delete { range, .. } => (range.start, range.len(), 0),
            };
            span = Some(match span {
                None => (at, at + inserted, removed),
                Some((start, end, before_len)) => {
                    let new_start = start.min(at);
                    let new_end = end.max(at + removed);
                    let before_len = before_len + (start - new_start) + (new_end - end);
                    (new_start, new_end + inserted - removed, before_len)
                }
            });
        }
        let (start, end, before_len) = span.expect("history replay applied no primitive");
        crate::host::protocol::TextEdit {
            range: ByteRange {
                start_byte_offset: start,
                end_byte_offset: start + before_len,
            },
            text: self.read(start..end),
        }
    }

    #[cfg(test)]
    fn edit_seq(&self) -> usize {
        self.buffer.edit_seq()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct BufferClosed;

/// Recoverable failures while accessing a foreground-owned buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BufferAccessError {
    Closed,
    ReadOnly,
    InvalidRange,
    InvalidEditBatch,
    RevisionConflict,
}

/// Maps transport-safe buffer handles to foreground-owned gpui entities.
pub struct BufferRegistry {
    next_handle: u64,
    buffers: HashMap<BufferHandle, WeakEntity<BufferModel>>,
}

impl BufferRegistry {
    pub fn new() -> Self {
        Self {
            next_handle: 1,
            buffers: HashMap::new(),
        }
    }

    pub fn open(&mut self, model: &Entity<BufferModel>) -> BufferHandle {
        if let Some(handle) = self.handle_for(model) {
            return handle;
        }
        self.register(model.downgrade())
    }

    fn register(&mut self, model: WeakEntity<BufferModel>) -> BufferHandle {
        let handle = BufferHandle::new(self.next_handle);
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .expect("buffer handle space exhausted");
        self.buffers.insert(handle, model);
        handle
    }

    pub(crate) fn handle_for(&self, model: &Entity<BufferModel>) -> Option<BufferHandle> {
        self.buffers.iter().find_map(|(handle, candidate)| {
            (candidate.upgrade().as_ref() == Some(model)).then_some(*handle)
        })
    }

    pub fn resolve(
        &self,
        handle: BufferHandle,
        cx: &gpui::App,
    ) -> Result<Entity<BufferModel>, BufferClosed> {
        self.buffers
            .get(&handle)
            .and_then(WeakEntity::upgrade)
            .filter(|model| model.read(cx).is_open())
            .ok_or(BufferClosed)
    }

    pub fn close<C: AppContext>(
        &mut self,
        handle: BufferHandle,
        cx: &mut C,
    ) -> Result<(), BufferClosed> {
        let model = self
            .buffers
            .get(&handle)
            .and_then(WeakEntity::upgrade)
            .ok_or(BufferClosed)?;
        let _ = model.update(cx, |model, cx| {
            if model.is_open() {
                model.close();
                cx.notify();
            }
        });
        self.invalidate(handle);
        Ok(())
    }

    pub(crate) fn remove_contribution_lifecycle<C: AppContext>(
        &self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        cx: &mut C,
    ) {
        for model in self.buffers.values().filter_map(WeakEntity::upgrade) {
            let _ = model.update(cx, |model, cx| {
                if model.remove_contribution_lifecycle(extension, lifecycle) {
                    cx.notify();
                }
            });
        }
    }

    fn invalidate(&mut self, handle: BufferHandle) {
        self.buffers.remove(&handle);
    }
}

impl Default for BufferRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Foreground-authoritative buffer-change subscriptions.
pub struct BufferSubscriptionRegistry {
    next_subscription: u64,
    subscriptions: HashMap<BufferSubscriptionId, BufferSubscription>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BufferSubscription {
    pub id: BufferSubscriptionId,
    pub buffer: BufferHandle,
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
}

impl BufferSubscriptionRegistry {
    pub fn new() -> Self {
        Self {
            next_subscription: 1,
            subscriptions: HashMap::new(),
        }
    }

    pub(crate) fn subscribe(
        &mut self,
        buffer: BufferHandle,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> BufferSubscriptionId {
        let id = BufferSubscriptionId::new(self.next_subscription);
        self.next_subscription = self
            .next_subscription
            .checked_add(1)
            .expect("subscription space exhausted");
        self.subscriptions.insert(
            id,
            BufferSubscription {
                id,
                buffer,
                extension,
                lifecycle,
            },
        );
        id
    }

    pub(crate) fn unsubscribe(
        &mut self,
        id: BufferSubscriptionId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        if self.subscriptions.get(&id).is_some_and(|subscription| {
            subscription.extension == extension && subscription.lifecycle == lifecycle
        }) {
            self.subscriptions.remove(&id);
            true
        } else {
            false
        }
    }

    pub(crate) fn for_buffer(
        &self,
        buffer: BufferHandle,
    ) -> impl Iterator<Item = BufferSubscription> + '_ {
        self.subscriptions
            .values()
            .copied()
            .filter(move |subscription| subscription.buffer == buffer)
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        self.subscriptions.retain(|_, subscription| {
            subscription.extension != extension || subscription.lifecycle != lifecycle
        });
    }

    pub(crate) fn remove_buffer(&mut self, buffer: BufferHandle) {
        self.subscriptions
            .retain(|_, subscription| subscription.buffer != buffer);
    }
}

impl Default for BufferSubscriptionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
