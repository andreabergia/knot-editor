//! Foreground-owned editor document state.

use std::{collections::HashMap, ops::Range};

use gpui::{AppContext, Entity, WeakEntity};

use crate::{
    core::buffer::TextBuffer,
    host::protocol::{
        BufferHandle, BufferSubscriptionId, ByteRange, CommandRegistrationId, ExtensionId,
        ExtensionLifecycleId,
    },
};

/// The authoritative document owned by gpui's foreground thread.
pub struct BufferModel {
    buffer: TextBuffer,
    revision: u64,
    open: bool,
    pending_change: Option<CommittedBufferChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommittedBufferChange {
    pub before_revision: u64,
    pub revision: u64,
    pub edits: Vec<crate::host::protocol::TextEdit>,
}

impl BufferModel {
    pub fn from_text(text: impl Into<Box<str>>) -> Self {
        Self {
            buffer: TextBuffer::from_text(text),
            revision: 0,
            open: true,
            pending_change: None,
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
        Ok(crate::host::protocol::TextSnapshot {
            text: self.read_checked(range)?,
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
        let before_revision = self.revision;
        self.advance_revision();
        self.pending_change = Some(CommittedBufferChange {
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
    pub fn replace(&mut self, range: Range<usize>, text: &str) -> bool {
        assert!(self.open, "cannot edit a closed buffer");
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
        self.buffer.replace(range, text);
        let before_revision = self.revision;
        self.advance_revision();
        self.pending_change = Some(CommittedBufferChange {
            before_revision,
            revision: self.revision,
            edits: vec![edit],
        });
        true
    }

    pub(crate) fn take_pending_change(&mut self) -> Option<CommittedBufferChange> {
        self.pending_change.take()
    }

    fn advance_revision(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("public buffer revision overflowed");
    }

    fn close(&mut self) {
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

/// Foreground-authoritative command names and their extension ownership.
pub struct CommandRegistry {
    next_registration: u64,
    by_name: HashMap<String, CommandRegistration>,
    by_id: HashMap<CommandRegistrationId, String>,
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
}

impl Default for BufferSubscriptionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandRegistration {
    id: CommandRegistrationId,
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
}

/// The extension lifetime authorized to receive a named command invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandTarget {
    pub registration: CommandRegistrationId,
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandRegistryError {
    NameInUse,
    NotFound,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            next_registration: 1,
            by_name: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        name: String,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<CommandRegistrationId, CommandRegistryError> {
        if self.by_name.contains_key(&name) {
            return Err(CommandRegistryError::NameInUse);
        }
        let id = CommandRegistrationId::new(self.next_registration);
        self.next_registration = self
            .next_registration
            .checked_add(1)
            .expect("command registration space exhausted");
        self.by_name.insert(
            name.clone(),
            CommandRegistration {
                id,
                extension,
                lifecycle,
            },
        );
        self.by_id.insert(id, name);
        Ok(id)
    }

    pub(crate) fn resolve(&self, name: &str) -> Result<CommandTarget, CommandRegistryError> {
        let registration = self
            .by_name
            .get(name)
            .ok_or(CommandRegistryError::NotFound)?;
        Ok(CommandTarget {
            registration: registration.id,
            extension: registration.extension,
            lifecycle: registration.lifecycle,
        })
    }

    pub(crate) fn unregister(
        &mut self,
        id: CommandRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<(), CommandRegistryError> {
        let name = self.by_id.get(&id).ok_or(CommandRegistryError::NotFound)?;
        let registration = self.by_name.get(name).expect("command indexes agree");
        if registration.extension != extension || registration.lifecycle != lifecycle {
            return Err(CommandRegistryError::NotFound);
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
            .filter_map(|(name, registration)| {
                (registration.extension == extension && registration.lifecycle == lifecycle)
                    .then_some((registration.id, name.clone()))
            })
            .collect();
        for (id, name) in registrations {
            self.by_id.remove(&id);
            self.by_name.remove(&name);
        }
    }
}

impl Default for CommandRegistry {
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
        assert!(model.replace(1..2, "XYZ"));
        assert_eq!(model.text(), "aXYZc");
        assert_eq!(model.revision(), 1);
        assert_eq!(model.edit_seq(), 2);

        assert!(!model.replace(0..0, ""));
        assert_eq!(model.revision(), 1);
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
        assert_eq!(snapshot.text, "é中");
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
        assert_eq!(full.text, text);

        let emoji_start = text.find('👩').unwrap();
        let emoji_end = text.len();
        assert_eq!(
            model
                .snapshot(Some(ByteRange {
                    start_byte_offset: emoji_start,
                    end_byte_offset: emoji_end,
                }))
                .unwrap()
                .text,
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
    fn command_names_belong_to_one_extension_lifetime() {
        let mut registry = CommandRegistry::new();
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);
        let registration = registry
            .register("knot.fixture.edit".into(), extension, lifecycle)
            .unwrap();

        assert_eq!(
            registry.resolve("knot.fixture.edit").unwrap(),
            CommandTarget {
                registration,
                extension,
                lifecycle,
            }
        );
        assert_eq!(
            registry.register(
                "knot.fixture.edit".into(),
                ExtensionId::new(8),
                ExtensionLifecycleId::new(4),
            ),
            Err(CommandRegistryError::NameInUse)
        );
        assert_eq!(
            registry.unregister(registration, extension, ExtensionLifecycleId::new(4)),
            Err(CommandRegistryError::NotFound)
        );
        registry
            .unregister(registration, extension, lifecycle)
            .unwrap();
        assert_eq!(
            registry.resolve("knot.fixture.edit"),
            Err(CommandRegistryError::NotFound)
        );

        let replacement = registry
            .register("knot.fixture.edit".into(), extension, lifecycle)
            .unwrap();
        registry.remove_lifecycle(extension, lifecycle);
        assert_eq!(
            registry.unregister(replacement, extension, lifecycle),
            Err(CommandRegistryError::NotFound)
        );
    }
}
