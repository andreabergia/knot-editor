//! Foreground-owned editor document state.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    sync::Arc,
};

use gpui::{AppContext, Entity, WeakEntity};

use crate::{
    core::{
        anchored_range::{AnchoredRangeId, AnchoredRangeStore},
        buffer::TextBuffer,
    },
    host::protocol::{
        BufferHandle, BufferSubscriptionId, ByteRange, CommandName, CommandRegistrationId,
        DecorationToken, EditorContribution, ExtensionId, ExtensionLifecycleId, GutterToken,
        SnapshotText,
    },
};

/// The producer whose complete contribution set is stored in this buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ContributionSource {
    BuiltIn,
    Extension {
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    },
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
pub struct BufferModel {
    buffer: TextBuffer,
    access_policy: BufferAccessPolicy,
    anchored_ranges: AnchoredRangeStore,
    contributions: ContributionRegistry,
    revision: u64,
    open: bool,
    pending_changes: VecDeque<CommittedBufferChange>,
    snapshot_cache: RefCell<Option<CachedSnapshot>>,
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
    pub fn from_text(text: impl Into<Box<str>>) -> Self {
        Self::with_access_policy(text, BufferAccessPolicy::Editable)
    }

    pub(crate) fn from_read_only_text(text: impl Into<Box<str>>) -> Self {
        Self::with_access_policy(text, BufferAccessPolicy::ReadOnly)
    }

    fn with_access_policy(text: impl Into<Box<str>>, access_policy: BufferAccessPolicy) -> Self {
        Self {
            buffer: TextBuffer::from_text(text),
            access_policy,
            anchored_ranges: AnchoredRangeStore::new(),
            contributions: ContributionRegistry::new(),
            revision: 0,
            open: true,
            pending_changes: VecDeque::new(),
            snapshot_cache: RefCell::new(None),
        }
    }

    pub fn text(&self) -> String {
        self.buffer.read_range(0..self.buffer.len())
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_open(&self) -> bool {
        self.open
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
                    == ContributionSource::Extension {
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

    fn stabilize_anchored_ranges(&mut self) {
        let consumed = self.anchored_ranges.stabilize(&self.buffer).consumed;
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

    pub(crate) fn has_contribution_action(
        &self,
        source: ContributionSource,
        command: &str,
    ) -> bool {
        self.contributions.sets.get(&source).is_some_and(|set| {
            set.anchored_ranges.iter().any(|id| {
                self.contributions
                    .metadata
                    .get(id)
                    .and_then(|metadata| metadata.command.as_deref())
                    == Some(command)
            })
        })
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
                text: SnapshotText::Utf16(Arc::clone(&cached.text)),
                range,
                revision: self.revision,
            });
        }

        let text = match SnapshotText::from_utf8(&self.read_checked(range)?) {
            SnapshotText::Utf16(text) => text,
            SnapshotText::Utf8(_) => unreachable!("UTF-16 snapshot constructor returned UTF-8"),
        };
        *self.snapshot_cache.borrow_mut() = Some(CachedSnapshot {
            revision: self.revision,
            range,
            text: Arc::clone(&text),
        });
        Ok(crate::host::protocol::TextSnapshot {
            text: SnapshotText::Utf16(text),
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
            if let Some(previous) = &previous {
                if range.start < previous.start || range.start < previous.end {
                    return Err(BufferAccessError::InvalidEditBatch);
                }
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
        for (range, text) in validated.into_iter().rev() {
            self.buffer.replace(range, text);
        }
        self.stabilize_anchored_ranges();
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
        assert!(self.open, "cannot edit a closed buffer");
        if self.access_policy == BufferAccessPolicy::ReadOnly {
            return Err(BufferAccessError::ReadOnly);
        }
        if range.is_empty() && text.is_empty() {
            return Ok(false);
        }
        let edit = crate::host::protocol::TextEdit {
            range: ByteRange {
                start_byte_offset: range.start,
                end_byte_offset: range.end,
            },
            text: text.into(),
        };
        self.buffer.replace(range, text);
        self.stabilize_anchored_ranges();
        let before_revision = self.revision;
        self.advance_revision();
        self.pending_changes.push_back(CommittedBufferChange {
            before_revision,
            revision: self.revision,
            edits: vec![edit],
        });
        Ok(true)
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
    }

    fn close(&mut self) {
        self.anchored_ranges = AnchoredRangeStore::new();
        self.contributions = ContributionRegistry::new();
        self.open = false;
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
    active: Option<BufferHandle>,
}

impl BufferRegistry {
    pub fn new() -> Self {
        Self {
            next_handle: 1,
            buffers: HashMap::new(),
            active: None,
        }
    }

    pub fn open(&mut self, model: &Entity<BufferModel>) -> BufferHandle {
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

    pub fn set_active(&mut self, handle: Option<BufferHandle>) {
        self.active = handle;
    }

    pub fn active_handle(&self) -> Option<BufferHandle> {
        self.active
    }

    pub fn resolve(&self, handle: BufferHandle) -> Result<Entity<BufferModel>, BufferClosed> {
        self.buffers
            .get(&handle)
            .and_then(WeakEntity::upgrade)
            .ok_or(BufferClosed)
    }

    pub fn close<C: AppContext>(
        &mut self,
        handle: BufferHandle,
        cx: &mut C,
    ) -> Result<(), BufferClosed> {
        let model = self.resolve(handle)?;
        let _ = model.update(cx, |model, cx| {
            model.close();
            cx.notify();
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
        if self.active == Some(handle) {
            self.active = None;
        }
    }
}

impl Default for BufferRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The application owner of a discoverable command definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOwner {
    Native,
    Extension {
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    },
}

/// Foreground-owned metadata for one discoverable command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandDefinition {
    pub name: CommandName,
    pub title: String,
    pub owner: CommandOwner,
}

/// Foreground-authoritative command definitions and their ownership.
pub struct CommandCatalog {
    next_registration: u64,
    by_name: HashMap<CommandName, CommandCatalogEntry>,
    by_id: HashMap<CommandRegistrationId, CommandName>,
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

    #[allow(
        dead_code,
        reason = "buffer closing is not exposed by the prototype shell yet"
    )]
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandCatalogEntry {
    definition: CommandDefinition,
    extension_registration: Option<CommandRegistrationId>,
}

/// The extension lifetime authorized to receive a named command invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandTarget {
    pub registration: CommandRegistrationId,
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandCatalogError {
    NameInUse,
    NotFound,
}

impl CommandCatalog {
    pub fn new() -> Self {
        Self {
            next_registration: 1,
            by_name: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    pub(crate) fn register_extension(
        &mut self,
        name: CommandName,
        title: String,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<CommandRegistrationId, CommandCatalogError> {
        if self.by_name.contains_key(&name) {
            return Err(CommandCatalogError::NameInUse);
        }
        let id = CommandRegistrationId::new(self.next_registration);
        self.next_registration = self
            .next_registration
            .checked_add(1)
            .expect("command registration space exhausted");
        self.by_name.insert(
            name.clone(),
            CommandCatalogEntry {
                definition: CommandDefinition {
                    name: name.clone(),
                    title,
                    owner: CommandOwner::Extension {
                        extension,
                        lifecycle,
                    },
                },
                extension_registration: Some(id),
            },
        );
        self.by_id.insert(id, name);
        Ok(id)
    }

    #[allow(
        dead_code,
        reason = "native definitions are registered when native command routing is introduced"
    )]
    pub(crate) fn register_native(
        &mut self,
        name: CommandName,
        title: String,
    ) -> Result<(), CommandCatalogError> {
        if self.by_name.contains_key(&name) {
            return Err(CommandCatalogError::NameInUse);
        }
        self.by_name.insert(
            name.clone(),
            CommandCatalogEntry {
                definition: CommandDefinition {
                    name,
                    title,
                    owner: CommandOwner::Native,
                },
                extension_registration: None,
            },
        );
        Ok(())
    }

    pub fn definitions(&self) -> impl Iterator<Item = &CommandDefinition> {
        self.by_name.values().map(|entry| &entry.definition)
    }

    pub(crate) fn resolve_extension(
        &self,
        name: &str,
    ) -> Result<CommandTarget, CommandCatalogError> {
        let entry = self
            .by_name
            .get(name)
            .ok_or(CommandCatalogError::NotFound)?;
        let CommandOwner::Extension {
            extension,
            lifecycle,
        } = entry.definition.owner
        else {
            return Err(CommandCatalogError::NotFound);
        };
        Ok(CommandTarget {
            registration: entry
                .extension_registration
                .expect("extension definitions have registrations"),
            extension,
            lifecycle,
        })
    }

    pub(crate) fn unregister(
        &mut self,
        id: CommandRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<(), CommandCatalogError> {
        let name = self.by_id.get(&id).ok_or(CommandCatalogError::NotFound)?;
        let entry = self.by_name.get(name).expect("command indexes agree");
        if entry.definition.owner
            != (CommandOwner::Extension {
                extension,
                lifecycle,
            })
        {
            return Err(CommandCatalogError::NotFound);
        }
        let name = self.by_id.remove(&id).expect("command exists");
        self.by_name.remove(&name);
        Ok(())
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        let registrations: Vec<_> = self
            .by_name
            .iter()
            .filter_map(|(name, entry)| {
                if entry.definition.owner
                    != (CommandOwner::Extension {
                        extension,
                        lifecycle,
                    })
                {
                    return None;
                }
                Some((
                    entry
                        .extension_registration
                        .expect("extension definitions have registrations"),
                    name.clone(),
                ))
            })
            .collect();
        for (id, name) in registrations {
            self.by_id.remove(&id);
            self.by_name.remove(&name);
        }
    }
}

impl Default for CommandCatalog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::host::protocol::TextEdit;

    use super::*;

    #[test]
    fn handles_are_monotonic_and_invalidated_buffers_stay_invalid() {
        let mut registry = BufferRegistry::new();
        let first_handle = registry.register(WeakEntity::new_invalid());
        let second_handle = registry.register(WeakEntity::new_invalid());
        assert_ne!(first_handle, second_handle);

        registry.set_active(Some(first_handle));
        registry.invalidate(first_handle);
        assert_eq!(registry.active_handle(), None);
        assert!(registry.resolve(first_handle).is_err());

        let third_handle = registry.register(WeakEntity::new_invalid());
        assert_ne!(third_handle, first_handle);
        assert_ne!(third_handle, second_handle);
    }

    #[test]
    fn replacement_is_one_public_commit() {
        let mut model = BufferModel::from_text("abc");
        assert!(model.replace(1..2, "XYZ").unwrap());
        assert_eq!(model.text(), "aXYZc");
        assert_eq!(model.revision(), 1);
        assert_eq!(model.edit_seq(), 2);

        assert!(!model.replace(0..0, "").unwrap());
        assert_eq!(model.revision(), 1);
    }

    #[test]
    fn read_only_models_reject_local_and_extension_text_edits() {
        let mut model = BufferModel::from_read_only_text("generated text");
        let extension_edit = TextEdit {
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 9,
            },
            text: "changed".into(),
        };

        assert_eq!(model.access_policy, BufferAccessPolicy::ReadOnly);
        assert_eq!(
            model.replace(0..9, "changed"),
            Err(BufferAccessError::ReadOnly)
        );
        assert_eq!(
            model.apply_edits(&[extension_edit], 0),
            Err(BufferAccessError::ReadOnly)
        );
        assert_eq!(model.text(), "generated text");
        assert_eq!(model.revision(), 0);
        assert_eq!(model.take_pending_change(), None);
    }

    #[test]
    fn read_only_models_keep_non_text_operations_available() {
        let mut model = BufferModel::from_read_only_text("generated text");
        let position = model.add_view_position(0..9);

        model
            .replace_contributions(
                ContributionSource::BuiltIn,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 9,
                    },
                    decoration: Some(DecorationToken::Info),
                    gutter: None,
                    command: None,
                }],
                0,
            )
            .unwrap();

        assert_eq!(
            model.snapshot(None).unwrap().text.to_utf8(),
            "generated text"
        );
        assert_eq!(model.resolve_view_position(position), Some(0..9));
        assert_eq!(model.resolved_contributions().len(), 1);
        model.close();
        assert!(!model.is_open());
    }

    #[test]
    fn contribution_sets_stabilize_and_replace_atomically() {
        let mut model = BufferModel::from_text("abc def");
        let source = ContributionSource::Extension {
            extension: ExtensionId::new(7),
            lifecycle: ExtensionLifecycleId::new(3),
        };
        model
            .replace_contributions(
                source,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 4,
                        end_byte_offset: 7,
                    },
                    decoration: Some(DecorationToken::Warning),
                    gutter: None,
                    command: None,
                }],
                0,
            )
            .unwrap();

        assert!(model.replace(0..0, "++").unwrap());
        assert_eq!(
            model.resolved_contributions(),
            [ResolvedEditorContribution {
                range: ByteRange {
                    start_byte_offset: 6,
                    end_byte_offset: 9,
                },
                source,
                decoration: Some(DecorationToken::Warning),
                gutter: None,
                command: None,
            }]
        );

        assert_eq!(
            model.replace_contributions(source, &[], 0),
            Err(ContributionError::RevisionConflict)
        );
        assert_eq!(model.resolved_contributions().len(), 1);
        assert_eq!(
            model.replace_contributions(
                source,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 7,
                        end_byte_offset: 99,
                    },
                    decoration: Some(DecorationToken::Error),
                    gutter: None,
                    command: None,
                }],
                model.revision(),
            ),
            Err(ContributionError::InvalidRange)
        );
        assert_eq!(model.resolved_contributions().len(), 1);

        model
            .replace_contributions(source, &[], model.revision())
            .unwrap();
        assert!(model.resolved_contributions().is_empty());
    }

    #[test]
    fn replacing_one_contribution_source_preserves_other_sources() {
        let mut model = BufferModel::from_text("abcdef");
        let extension_source = ContributionSource::Extension {
            extension: ExtensionId::new(7),
            lifecycle: ExtensionLifecycleId::new(3),
        };
        for (source, range, decoration) in [
            (ContributionSource::BuiltIn, 0..2, DecorationToken::Info),
            (extension_source, 2..4, DecorationToken::Warning),
        ] {
            model
                .replace_contributions(
                    source,
                    &[EditorContribution {
                        range: ByteRange {
                            start_byte_offset: range.start,
                            end_byte_offset: range.end,
                        },
                        decoration: Some(decoration),
                        gutter: None,
                        command: None,
                    }],
                    model.revision(),
                )
                .unwrap();
        }

        model
            .replace_contributions(
                extension_source,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 4,
                        end_byte_offset: 6,
                    },
                    decoration: Some(DecorationToken::Error),
                    gutter: None,
                    command: None,
                }],
                model.revision(),
            )
            .unwrap();

        assert_eq!(model.contributions.sets.len(), 2);
        assert_eq!(model.contributions.metadata.len(), 2);
        assert_eq!(model.anchored_ranges.len(), 2);
        assert_eq!(
            model.resolved_contributions(),
            [
                ResolvedEditorContribution {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 2,
                    },
                    source: ContributionSource::BuiltIn,
                    decoration: Some(DecorationToken::Info),
                    gutter: None,
                    command: None,
                },
                ResolvedEditorContribution {
                    range: ByteRange {
                        start_byte_offset: 4,
                        end_byte_offset: 6,
                    },
                    source: extension_source,
                    decoration: Some(DecorationToken::Error),
                    gutter: None,
                    command: None,
                },
            ]
        );
    }

    #[test]
    fn empty_buffer_contributions_are_rejected_recoverably() {
        let mut model = BufferModel::from_text("");
        let source = ContributionSource::BuiltIn;

        assert_eq!(
            model.replace_contributions(
                source,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 0,
                    },
                    decoration: Some(DecorationToken::Info),
                    gutter: None,
                    command: None,
                }],
                0,
            ),
            Err(ContributionError::InvalidRange)
        );
        assert!(model.contributions.sets.is_empty());
        assert!(model.contributions.metadata.is_empty());
        assert!(model.anchored_ranges.is_empty());
    }

    #[test]
    fn contribution_ownership_rejects_stale_lifecycles_and_cleans_up() {
        let mut model = BufferModel::from_text("abc");
        let extension = ExtensionId::new(7);
        let source = ContributionSource::Extension {
            extension,
            lifecycle: ExtensionLifecycleId::new(3),
        };
        let stale_source = ContributionSource::Extension {
            extension,
            lifecycle: ExtensionLifecycleId::new(2),
        };
        model
            .replace_contributions(
                source,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 3,
                    },
                    decoration: Some(DecorationToken::Error),
                    gutter: None,
                    command: None,
                }],
                0,
            )
            .unwrap();

        assert_eq!(
            model.dispose_contributions(stale_source),
            Err(ContributionError::NotFound)
        );
        assert_eq!(model.resolved_contributions().len(), 1);
        assert!(!model.remove_contribution_lifecycle(extension, ExtensionLifecycleId::new(2)));
        assert!(model.remove_contribution_lifecycle(extension, ExtensionLifecycleId::new(3)));
        assert!(model.resolved_contributions().is_empty());
    }

    #[test]
    fn stabilization_prunes_consumed_contribution_metadata() {
        let mut model = BufferModel::from_text("abc def");
        let source = ContributionSource::BuiltIn;
        model
            .replace_contributions(
                source,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 5,
                        end_byte_offset: 6,
                    },
                    decoration: Some(DecorationToken::Warning),
                    gutter: None,
                    command: None,
                }],
                0,
            )
            .unwrap();

        assert!(model.replace(4..7, "").unwrap());

        assert!(model.resolved_contributions().is_empty());
        assert!(model.contributions.metadata.is_empty());
        assert!(model.contributions.sets[&source].anchored_ranges.is_empty());
        assert!(model.anchored_ranges.is_empty());
    }

    #[test]
    fn decoration_precedence_is_application_owned_and_deterministic() {
        let mut model = BufferModel::from_text("abc");
        for (source, decoration) in [
            (ContributionSource::BuiltIn, DecorationToken::Warning),
            (
                ContributionSource::Extension {
                    extension: ExtensionId::new(7),
                    lifecycle: ExtensionLifecycleId::new(3),
                },
                DecorationToken::Info,
            ),
        ] {
            model
                .replace_contributions(
                    source,
                    &[EditorContribution {
                        range: ByteRange {
                            start_byte_offset: 0,
                            end_byte_offset: 3,
                        },
                        decoration: Some(decoration),
                        gutter: None,
                        command: None,
                    }],
                    0,
                )
                .unwrap();
        }

        assert_eq!(
            model
                .resolved_contributions()
                .into_iter()
                .filter_map(|contribution| contribution.decoration)
                .collect::<Vec<_>>(),
            [DecorationToken::Info, DecorationToken::Warning]
        );
    }

    #[test]
    fn checked_reads_reject_invalid_utf8_ranges_without_touching_the_buffer() {
        let model = BufferModel::from_text("aé中");

        assert_eq!(
            model.read_checked(ByteRange {
                start_byte_offset: 1,
                end_byte_offset: 3,
            }),
            Ok("é".into())
        );
        assert_eq!(
            model.read_checked(ByteRange {
                start_byte_offset: 2,
                end_byte_offset: 3,
            }),
            Err(BufferAccessError::InvalidRange)
        );
        assert_eq!(
            model.read_checked(ByteRange {
                start_byte_offset: 4,
                end_byte_offset: 3,
            }),
            Err(BufferAccessError::InvalidRange)
        );
        assert_eq!(model.text(), "aé中");
        assert_eq!(model.revision(), 0);
    }

    #[test]
    fn checked_reads_reject_closed_buffers() {
        let mut model = BufferModel::from_text("abc");
        model.close();

        assert_eq!(
            model.read_checked(ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 3,
            }),
            Err(BufferAccessError::Closed)
        );
    }

    #[test]
    fn snapshots_and_atomic_edits_use_public_revisions() {
        let mut model = BufferModel::from_text("aé中z");
        let snapshot = model
            .snapshot(Some(ByteRange {
                start_byte_offset: 1,
                end_byte_offset: 6,
            }))
            .unwrap();
        assert_eq!(snapshot.text.to_utf8(), "é中");
        assert_eq!(snapshot.revision, 0);

        assert!(
            model
                .apply_edits(
                    &[
                        TextEdit {
                            range: ByteRange {
                                start_byte_offset: 0,
                                end_byte_offset: 1,
                            },
                            text: "A".into(),
                        },
                        TextEdit {
                            range: ByteRange {
                                start_byte_offset: 6,
                                end_byte_offset: 7,
                            },
                            text: "Z".into(),
                        },
                    ],
                    0,
                )
                .unwrap()
        );
        assert_eq!(model.text(), "Aé中Z");
        assert_eq!(model.revision(), 1);
    }

    #[test]
    fn edits_reject_invalid_batches_without_partial_mutation() {
        let mut model = BufferModel::from_text("abcdef");
        let overlapping = [
            TextEdit {
                range: ByteRange {
                    start_byte_offset: 1,
                    end_byte_offset: 3,
                },
                text: "X".into(),
            },
            TextEdit {
                range: ByteRange {
                    start_byte_offset: 2,
                    end_byte_offset: 4,
                },
                text: "Y".into(),
            },
        ];
        assert_eq!(
            model.apply_edits(&overlapping, 0),
            Err(BufferAccessError::InvalidEditBatch)
        );
        assert_eq!(model.text(), "abcdef");
        assert_eq!(model.revision(), 0);

        assert_eq!(
            model.apply_edits(&[], 1),
            Err(BufferAccessError::RevisionConflict)
        );
    }

    #[test]
    fn semantically_empty_batches_do_not_commit() {
        let mut model = BufferModel::from_text("abcdef");
        let edits = [TextEdit {
            range: ByteRange {
                start_byte_offset: 1,
                end_byte_offset: 3,
            },
            text: "bc".into(),
        }];
        assert!(!model.apply_edits(&edits, 0).unwrap());
        assert!(!model.apply_edits(&[], 0).unwrap());
        assert_eq!(model.text(), "abcdef");
        assert_eq!(model.revision(), 0);
    }

    #[test]
    fn snapshots_preserve_unicode_and_reject_split_scalar_boundaries() {
        let text = "ASCII 中 العربية e\u{301} 👩‍💻";
        let model = BufferModel::from_text(text);
        let full = model.snapshot(None).unwrap();
        assert_eq!(full.text.to_utf8(), text);

        let emoji_start = text.find('👩').unwrap();
        let emoji_end = text.len();
        assert_eq!(
            model
                .snapshot(Some(ByteRange {
                    start_byte_offset: emoji_start,
                    end_byte_offset: emoji_end,
                }))
                .unwrap()
                .text
                .to_utf8(),
            "👩‍💻"
        );
        assert_eq!(
            model.snapshot(Some(ByteRange {
                start_byte_offset: emoji_start + 1,
                end_byte_offset: emoji_end,
            })),
            Err(BufferAccessError::InvalidRange)
        );
    }

    #[test]
    fn snapshot_cache_reuses_storage_and_invalidates_on_edit() {
        let mut model = BufferModel::from_text("abc");
        let first = model.snapshot(None).unwrap();
        let second = model.snapshot(None).unwrap();
        let (SnapshotText::Utf16(first), SnapshotText::Utf16(second)) = (first.text, second.text)
        else {
            panic!("buffer snapshots must use UTF-16 cache storage");
        };
        assert!(Arc::ptr_eq(&first, &second));

        assert!(model.replace(1..2, "B").unwrap());
        let third = model.snapshot(None).unwrap();
        let SnapshotText::Utf16(third) = third.text else {
            panic!("buffer snapshots must use UTF-16 cache storage");
        };
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(String::from_utf16(&third).unwrap(), "aBc");
    }

    #[test]
    fn command_names_belong_to_one_extension_lifetime() {
        let mut catalog = CommandCatalog::new();
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);
        let registration = catalog
            .register_extension(
                "knot.fixture.edit".into(),
                "Edit fixture".into(),
                extension,
                lifecycle,
            )
            .unwrap();

        assert_eq!(
            catalog.definitions().cloned().collect::<Vec<_>>(),
            vec![CommandDefinition {
                name: "knot.fixture.edit".into(),
                title: "Edit fixture".into(),
                owner: CommandOwner::Extension {
                    extension,
                    lifecycle,
                },
            }]
        );
        assert_eq!(
            catalog.resolve_extension("knot.fixture.edit").unwrap(),
            CommandTarget {
                registration,
                extension,
                lifecycle,
            }
        );
        assert_eq!(
            catalog.register_extension(
                "knot.fixture.edit".into(),
                "Other edit".into(),
                ExtensionId::new(8),
                ExtensionLifecycleId::new(4),
            ),
            Err(CommandCatalogError::NameInUse)
        );
        assert_eq!(
            catalog.unregister(registration, extension, ExtensionLifecycleId::new(4)),
            Err(CommandCatalogError::NotFound)
        );
        catalog
            .unregister(registration, extension, lifecycle)
            .unwrap();
        assert_eq!(
            catalog.resolve_extension("knot.fixture.edit"),
            Err(CommandCatalogError::NotFound)
        );

        let replacement = catalog
            .register_extension(
                "knot.fixture.edit".into(),
                "Edit fixture".into(),
                extension,
                lifecycle,
            )
            .unwrap();
        catalog.remove_lifecycle(extension, lifecycle);
        assert_eq!(
            catalog.unregister(replacement, extension, lifecycle),
            Err(CommandCatalogError::NotFound)
        );
    }

    #[test]
    fn native_command_names_remain_reserved_across_extension_cleanup() {
        let mut catalog = CommandCatalog::new();
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);

        catalog
            .register_native("editor.copy".into(), "Copy".into())
            .unwrap();
        assert_eq!(
            catalog.register_extension(
                "editor.copy".into(),
                "Replacement copy".into(),
                extension,
                lifecycle,
            ),
            Err(CommandCatalogError::NameInUse)
        );

        catalog
            .register_extension(
                "example.transform".into(),
                "Transform Selection".into(),
                extension,
                lifecycle,
            )
            .unwrap();
        catalog.remove_lifecycle(extension, lifecycle);

        assert_eq!(
            catalog.definitions().cloned().collect::<Vec<_>>(),
            vec![CommandDefinition {
                name: "editor.copy".into(),
                title: "Copy".into(),
                owner: CommandOwner::Native,
            }]
        );
        assert_eq!(
            catalog.register_extension(
                "editor.copy".into(),
                "Replacement copy".into(),
                extension,
                ExtensionLifecycleId::new(4),
            ),
            Err(CommandCatalogError::NameInUse)
        );
    }

    #[test]
    fn command_definitions_describe_native_and_extension_ownership() {
        let native = CommandDefinition {
            name: "editor.copy".into(),
            title: "Copy".into(),
            owner: CommandOwner::Native,
        };
        let extension = CommandDefinition {
            name: "example.transform".into(),
            title: "Transform Selection".into(),
            owner: CommandOwner::Extension {
                extension: ExtensionId::new(7),
                lifecycle: ExtensionLifecycleId::new(3),
            },
        };

        assert_eq!(native.name.as_ref(), "editor.copy");
        assert_eq!(native.title, "Copy");
        assert_eq!(native.owner, CommandOwner::Native);
        assert_eq!(extension.name.as_ref(), "example.transform");
        assert_eq!(
            extension.owner,
            CommandOwner::Extension {
                extension: ExtensionId::new(7),
                lifecycle: ExtensionLifecycleId::new(3),
            }
        );
    }

    #[test]
    fn buffer_subscriptions_are_scoped_by_buffer_and_lifecycle() {
        let mut registry = BufferSubscriptionRegistry::new();
        let first_buffer = BufferHandle::new(1);
        let second_buffer = BufferHandle::new(2);
        let extension = ExtensionId::new(7);
        let first_lifecycle = ExtensionLifecycleId::new(3);
        let second_lifecycle = ExtensionLifecycleId::new(4);
        let first = registry.subscribe(first_buffer, extension, first_lifecycle);
        let second = registry.subscribe(first_buffer, extension, second_lifecycle);
        registry.subscribe(second_buffer, ExtensionId::new(8), first_lifecycle);

        assert!(!registry.unsubscribe(first, extension, second_lifecycle));
        assert_eq!(registry.for_buffer(first_buffer).count(), 2);

        registry.remove_lifecycle(extension, first_lifecycle);
        assert_eq!(
            registry.for_buffer(first_buffer).collect::<Vec<_>>(),
            [BufferSubscription {
                id: second,
                buffer: first_buffer,
                extension,
                lifecycle: second_lifecycle,
            }]
        );
        assert_eq!(registry.for_buffer(second_buffer).count(), 1);

        registry.remove_buffer(second_buffer);
        assert_eq!(registry.for_buffer(second_buffer).count(), 0);
    }
}
