//! Foreground dispatch for extension buffer requests.

use std::collections::HashSet;

use gpui::{App, Entity};

use crate::host::protocol::{
    BufferChange, BufferHandle, BufferSubscriptionId, ExtensionId, ExtensionLifecycleId,
    RequestContext, TextSnapshot,
};

use crate::app::extensions::host::handlers::AccessError;
use crate::host::declarations::HostError;

use crate::app::model::{
    BufferAccessError, BufferClosed, BufferModel, BufferRegistry, BufferSubscriptionRegistry,
    ContributionError, ContributionSource,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, HostError)]
pub(crate) enum BufferError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "BufferClosed", message = "BufferClosed")]
    BufferClosed,
    #[error(code = "BufferReadOnly", message = "BufferReadOnly")]
    BufferReadOnly,
    #[error(code = "InvalidRange", message = "InvalidRange")]
    InvalidRange,
    #[error(code = "InvalidEditBatch", message = "InvalidEditBatch")]
    InvalidEditBatch,
    #[error(code = "RevisionConflict", message = "RevisionConflict")]
    RevisionConflict,
    #[error(code = "ContributionSetNotFound", message = "ContributionSetNotFound")]
    ContributionSetNotFound,
}

impl From<AccessError> for BufferError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

use crate::app::extensions::host::handlers::{
    BufferArguments, ContributionArguments, EditArguments, SnapshotArguments, SubscriptionArguments,
};

/// One committed change routed to a lifecycle-owned JavaScript subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BufferChangeDispatch {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub subscription: BufferSubscriptionId,
    pub change: BufferChange,
}

/// Foreground-owned buffer state exposed through the extension protocol.
///
/// Runtime scheduling and response transport remain outside this component.
/// The product host serves typed capabilities and enqueues each
/// [`BufferChangeDispatch`] as extension root work.
pub(crate) struct ExtensionBufferBridge {
    buffers: BufferRegistry,
    subscriptions: BufferSubscriptionRegistry,
    lifecycles: HashSet<(ExtensionId, ExtensionLifecycleId)>,
}

impl ExtensionBufferBridge {
    pub(crate) fn new() -> Self {
        Self {
            buffers: BufferRegistry::new(),
            subscriptions: BufferSubscriptionRegistry::new(),
            lifecycles: HashSet::new(),
        }
    }

    pub(crate) fn admit_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        self.lifecycles.insert((extension, lifecycle));
    }

    pub(crate) fn open_buffer(&mut self, model: &Entity<BufferModel>) -> BufferHandle {
        self.buffers.open(model)
    }

    pub(crate) fn close_buffer(
        &mut self,
        buffer: BufferHandle,
        cx: &mut App,
    ) -> Result<(), BufferClosed> {
        self.buffers.close(buffer, cx)?;
        self.subscriptions.remove_buffer(buffer);
        Ok(())
    }

    /// Remove every foreground resource owned by one extension lifetime.
    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        cx: &mut App,
    ) {
        self.lifecycles.remove(&(extension, lifecycle));
        self.subscriptions.remove_lifecycle(extension, lifecycle);
        self.buffers
            .remove_contribution_lifecycle(extension, lifecycle, cx);
    }

    fn validate(
        &self,
        request: &RequestContext,
        is_cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), BufferError> {
        if !self
            .lifecycles
            .contains(&(request.extension, request.lifecycle))
            || is_cancelled()
        {
            Err(AccessError::Cancelled.into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn snapshot(
        &mut self,
        request: &RequestContext,
        arguments: SnapshotArguments,
        mut cancelled: impl FnMut() -> bool,
        cx: &mut App,
    ) -> Result<TextSnapshot, BufferError> {
        self.validate(request, &mut cancelled)?;
        self.buffers
            .resolve(arguments.buffer, cx)
            .map_err(|_| BufferError::BufferClosed)?
            .read_with(cx, |model, _| model.snapshot(arguments.range))
            .map_err(map_buffer_error)
    }

    pub(crate) fn apply_edits(
        &mut self,
        request: &RequestContext,
        arguments: EditArguments,
        mut cancelled: impl FnMut() -> bool,
        cx: &mut App,
    ) -> Result<u64, BufferError> {
        self.validate(request, &mut cancelled)?;
        let model = self
            .buffers
            .resolve(arguments.buffer, cx)
            .map_err(|_| BufferError::BufferClosed)?;
        model.update(cx, |model, cx| {
            if cancelled() {
                return Err(AccessError::Cancelled.into());
            }
            if model
                .apply_edits(&arguments.edits, arguments.if_revision)
                .map_err(map_buffer_error)?
            {
                cx.notify();
            }
            Ok(model.revision())
        })
    }

    pub(crate) fn subscribe_buffer_changes(
        &mut self,
        request: &RequestContext,
        arguments: BufferArguments,
        mut cancelled: impl FnMut() -> bool,
        cx: &mut App,
    ) -> Result<BufferSubscriptionId, BufferError> {
        self.validate(request, &mut cancelled)?;
        self.buffers
            .resolve(arguments.buffer, cx)
            .map_err(|_| BufferError::BufferClosed)?;
        Ok(self
            .subscriptions
            .subscribe(arguments.buffer, request.extension, request.lifecycle))
    }

    pub(crate) fn unsubscribe_buffer_changes(
        &mut self,
        request: &RequestContext,
        arguments: SubscriptionArguments,
        mut cancelled: impl FnMut() -> bool,
        _: &mut App,
    ) -> Result<(), BufferError> {
        self.validate(request, &mut cancelled)?;
        if self.subscriptions.unsubscribe(
            arguments.subscription,
            request.extension,
            request.lifecycle,
        ) {
            Ok(())
        } else {
            Err(AccessError::OwnershipDisposed.into())
        }
    }

    pub(crate) fn replace_editor_contributions(
        &mut self,
        request: &RequestContext,
        arguments: ContributionArguments,
        mut cancelled: impl FnMut() -> bool,
        cx: &mut App,
    ) -> Result<(), BufferError> {
        self.validate(request, &mut cancelled)?;
        let model = self
            .buffers
            .resolve(arguments.buffer, cx)
            .map_err(|_| BufferError::BufferClosed)?;
        model.update(cx, |model, cx| {
            if cancelled() {
                return Err(AccessError::Cancelled.into());
            }
            model
                .replace_contributions(
                    ContributionSource {
                        extension: request.extension,
                        lifecycle: request.lifecycle,
                    },
                    &arguments.contributions,
                    arguments.if_revision,
                )
                .map_err(map_contribution_error)?;
            cx.notify();
            Ok(())
        })
    }

    pub(crate) fn dispose_editor_contributions(
        &mut self,
        request: &RequestContext,
        arguments: BufferArguments,
        mut cancelled: impl FnMut() -> bool,
        cx: &mut App,
    ) -> Result<(), BufferError> {
        self.validate(request, &mut cancelled)?;
        let model = self
            .buffers
            .resolve(arguments.buffer, cx)
            .map_err(|_| BufferError::BufferClosed)?;
        model.update(cx, |model, cx| {
            if cancelled() {
                return Err(AccessError::Cancelled.into());
            }
            match model.dispose_contributions(ContributionSource {
                extension: request.extension,
                lifecycle: request.lifecycle,
            }) {
                Ok(()) => cx.notify(),
                Err(ContributionError::NotFound) => {}
                Err(error) => return Err(map_contribution_error(error)),
            }
            Ok(())
        })
    }

    /// Drain model commits and fan each change out to its live subscriptions.
    pub(crate) fn drain_model_changes(
        &mut self,
        model: &Entity<BufferModel>,
        cx: &mut App,
    ) -> Vec<BufferChangeDispatch> {
        let Some(buffer) = self.buffers.handle_for(model) else {
            return Vec::new();
        };
        if !model.read(cx).is_open() {
            let _ = self.close_buffer(buffer, cx);
            return Vec::new();
        }
        let mut dispatches = Vec::new();
        while let Some(committed) = model.update(cx, |model, _| model.take_pending_change()) {
            let change = BufferChange {
                buffer,
                before_revision: committed.before_revision,
                revision: committed.revision,
                edits: committed.edits,
            };
            dispatches.extend(self.subscriptions.for_buffer(buffer).map(|subscription| {
                BufferChangeDispatch {
                    extension: subscription.extension,
                    lifecycle: subscription.lifecycle,
                    subscription: subscription.id,
                    change: change.clone(),
                }
            }));
        }
        dispatches
    }
}

impl Default for ExtensionBufferBridge {
    fn default() -> Self {
        Self::new()
    }
}

fn map_buffer_error(error: BufferAccessError) -> BufferError {
    match error {
        BufferAccessError::Closed => BufferError::BufferClosed,
        BufferAccessError::ReadOnly => BufferError::BufferReadOnly,
        BufferAccessError::InvalidRange => BufferError::InvalidRange,
        BufferAccessError::InvalidEditBatch => BufferError::InvalidEditBatch,
        BufferAccessError::RevisionConflict => BufferError::RevisionConflict,
    }
}

fn map_contribution_error(error: ContributionError) -> BufferError {
    match error {
        ContributionError::Closed => BufferError::BufferClosed,
        ContributionError::InvalidRange => BufferError::InvalidRange,
        ContributionError::RevisionConflict => BufferError::RevisionConflict,
        ContributionError::NotFound => BufferError::ContributionSetNotFound,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{AppContext, TestAppContext};

    use crate::host::protocol::{ByteRange, EditorContribution, RequestId, TextEdit};

    use super::*;

    fn request(extension: ExtensionId, lifecycle: ExtensionLifecycleId, id: u64) -> RequestContext {
        RequestContext {
            extension,
            lifecycle,
            id: RequestId::new(id),
            invocation: None,
        }
    }

    #[gpui::test]
    fn snapshots_edits_and_late_cancellation_stay_revisioned(cx: &mut TestAppContext) {
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);
        let model = cx.new(|_| BufferModel::from_text("aé中z"));
        cx.update(|cx| {
            let mut bridge = ExtensionBufferBridge::new();
            bridge.admit_lifecycle(extension, lifecycle);
            let buffer = bridge.open_buffer(&model);

            let snapshot = bridge.snapshot(
                &request(extension, lifecycle, 1),
                SnapshotArguments {
                    buffer,
                    range: Some(ByteRange {
                        start_byte_offset: 1,
                        end_byte_offset: 6,
                    }),
                },
                || false,
                cx,
            );
            let snapshot = snapshot.unwrap();
            assert_eq!(snapshot.text.to_utf8(), "é中");
            assert_eq!(snapshot.revision, 0);

            let checks = Cell::new(0);
            let cancelled = bridge.apply_edits(
                &request(extension, lifecycle, 2),
                EditArguments {
                    buffer,
                    edits: vec![TextEdit {
                        range: ByteRange {
                            start_byte_offset: 0,
                            end_byte_offset: 0,
                        },
                        text: "late".into(),
                    }],
                    if_revision: 0,
                },
                || {
                    let check = checks.get();
                    checks.set(check + 1);
                    check == 1
                },
                cx,
            );
            assert_eq!(cancelled, Err(BufferError::Access(AccessError::Cancelled)));
            assert_eq!(checks.get(), 2);
            assert_eq!(model.read_with(cx, |model, _| model.text()), "aé中z");

            let applied = bridge.apply_edits(
                &request(extension, lifecycle, 3),
                EditArguments {
                    buffer,
                    edits: vec![TextEdit {
                        range: ByteRange {
                            start_byte_offset: 1,
                            end_byte_offset: 3,
                        },
                        text: "E".into(),
                    }],
                    if_revision: 0,
                },
                || false,
                cx,
            );
            assert_eq!(applied, Ok(1));
            let stale = bridge.apply_edits(
                &request(extension, lifecycle, 4),
                EditArguments {
                    buffer,
                    edits: Vec::new(),
                    if_revision: 0,
                },
                || false,
                cx,
            );
            assert_eq!(stale, Err(BufferError::RevisionConflict));
            assert_eq!(model.read(cx).text(), "aE中z");
            bridge.remove_lifecycle(extension, lifecycle, cx);
            assert_eq!(model.read(cx).text(), "aE中z");
            let cancelled = bridge.snapshot(
                &request(extension, lifecycle, 5),
                SnapshotArguments {
                    buffer,
                    range: None,
                },
                || false,
                cx,
            );
            assert_eq!(cancelled, Err(BufferError::Access(AccessError::Cancelled)));
        });
    }

    #[gpui::test]
    fn buffer_policy_and_stale_subscription_failures_are_distinct(cx: &mut TestAppContext) {
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);
        let model = cx.new(|_| BufferModel::from_read_only_text("generated"));
        cx.update(|cx| {
            let mut bridge = ExtensionBufferBridge::new();
            bridge.admit_lifecycle(extension, lifecycle);
            let buffer = bridge.open_buffer(&model);
            let result = bridge.apply_edits(
                &request(extension, lifecycle, 1),
                EditArguments {
                    buffer,
                    edits: vec![TextEdit {
                        range: ByteRange {
                            start_byte_offset: 0,
                            end_byte_offset: 0,
                        },
                        text: "edit".into(),
                    }],
                    if_revision: 0,
                },
                || false,
                cx,
            );
            assert_eq!(result, Err(BufferError::BufferReadOnly));
            let result = bridge.unsubscribe_buffer_changes(
                &request(extension, lifecycle, 2),
                SubscriptionArguments {
                    subscription: BufferSubscriptionId::new(999),
                },
                || false,
                cx,
            );
            assert_eq!(
                result,
                Err(BufferError::Access(AccessError::OwnershipDisposed))
            );
            assert_eq!(model.read(cx).text(), "generated");
        });
    }

    #[gpui::test]
    fn document_removal_invalidates_retained_models_and_releases_buffer_ownership(
        cx: &mut TestAppContext,
    ) {
        use crate::app::documents::DocumentCollection;
        let extension = ExtensionId::new(11);
        let lifecycle = ExtensionLifecycleId::new(5);
        let model = cx.new(|_| BufferModel::from_text("owned"));
        cx.update(|cx| {
            let mut documents = DocumentCollection::new();
            let document = documents.create_untitled("owned", model.clone(), cx);
            let mut bridge = ExtensionBufferBridge::new();
            bridge.admit_lifecycle(extension, lifecycle);
            let buffer = bridge.open_buffer(&model);
            assert!(
                bridge
                    .subscribe_buffer_changes(
                        &request(extension, lifecycle, 1),
                        BufferArguments { buffer },
                        || false,
                        cx
                    )
                    .is_ok()
            );
            assert!(
                bridge
                    .replace_editor_contributions(
                        &request(extension, lifecycle, 2),
                        ContributionArguments {
                            buffer,
                            contributions: vec![EditorContribution {
                                range: ByteRange {
                                    start_byte_offset: 0,
                                    end_byte_offset: 1
                                },
                                decoration: Some("info".to_owned()),
                                gutter: None,
                                command: None,
                            }],
                            if_revision: 0,
                        },
                        || false,
                        cx
                    )
                    .is_ok()
            );
            assert_eq!(bridge.subscriptions.for_buffer(buffer).count(), 1);
            model.update(cx, |model, _| model.replace(0..0, "pending ").unwrap());
            assert!(documents.remove(document, cx));
            assert!(!model.read(cx).is_open());
            assert!(model.read(cx).resolved_contributions().is_empty());
            // Access fails immediately, before the model observer has drained cleanup.
            assert_eq!(
                bridge.snapshot(
                    &request(extension, lifecycle, 3),
                    SnapshotArguments {
                        buffer,
                        range: None
                    },
                    || false,
                    cx
                ),
                Err(BufferError::BufferClosed)
            );
            assert_eq!(
                bridge.subscribe_buffer_changes(
                    &request(extension, lifecycle, 3),
                    BufferArguments { buffer },
                    || false,
                    cx
                ),
                Err(BufferError::BufferClosed)
            );
            assert!(bridge.drain_model_changes(&model, cx).is_empty());
            assert_eq!(bridge.subscriptions.for_buffer(buffer).count(), 0);
            assert!(bridge.buffers.handle_for(&model).is_none());
            assert!(
                model
                    .update(cx, |model, _| model.take_pending_change())
                    .is_none()
            );
        });
    }

    #[gpui::test]
    fn subscriptions_contributions_and_lifecycle_cleanup_share_one_owner(cx: &mut TestAppContext) {
        let extension = ExtensionId::new(11);
        let lifecycle = ExtensionLifecycleId::new(5);
        let model = cx.new(|_| BufferModel::from_text("abcdef"));
        cx.update(|cx| {
            let mut bridge = ExtensionBufferBridge::new();
            bridge.admit_lifecycle(extension, lifecycle);
            let buffer = bridge.open_buffer(&model);

            let subscribed = bridge.subscribe_buffer_changes(
                &request(extension, lifecycle, 1),
                BufferArguments { buffer },
                || false,
                cx,
            );
            let subscription = subscribed.unwrap();

            let replaced = bridge.replace_editor_contributions(
                &request(extension, lifecycle, 2),
                ContributionArguments {
                    buffer,
                    contributions: vec![EditorContribution {
                        range: ByteRange {
                            start_byte_offset: 1,
                            end_byte_offset: 3,
                        },
                        decoration: Some("warning".to_owned()),
                        gutter: Some("info".to_owned()),
                        command: Some("fixture.action".into()),
                    }],
                    if_revision: 0,
                },
                || false,
                cx,
            );
            assert_eq!(replaced, Ok(()));
            assert_eq!(
                model.read_with(cx, |model, _| model.resolved_contributions().len()),
                1
            );

            model.update(cx, |model, _| {
                model.replace(0..0, "x").unwrap();
            });
            let dispatches = bridge.drain_model_changes(&model, cx);
            assert_eq!(dispatches.len(), 1);
            assert_eq!(dispatches[0].subscription, subscription);
            assert_eq!(dispatches[0].change.before_revision, 0);
            assert_eq!(dispatches[0].change.revision, 1);

            let unsubscribed = bridge.unsubscribe_buffer_changes(
                &request(extension, lifecycle, 3),
                SubscriptionArguments { subscription },
                || false,
                cx,
            );
            assert_eq!(unsubscribed, Ok(()));
            model.update(cx, |model, _| {
                model.replace(0..0, "y").unwrap();
            });
            assert!(bridge.drain_model_changes(&model, cx).is_empty());

            let resubscribed = bridge.subscribe_buffer_changes(
                &request(extension, lifecycle, 4),
                BufferArguments { buffer },
                || false,
                cx,
            );
            let replacement_subscription = resubscribed.unwrap();
            assert_ne!(replacement_subscription, subscription);

            let replacement = EditorContribution {
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 1,
                },
                decoration: Some("error".to_owned()),
                gutter: None,
                command: None,
            };
            let replaced = bridge.replace_editor_contributions(
                &request(extension, lifecycle, 5),
                ContributionArguments {
                    buffer,
                    contributions: vec![replacement.clone()],
                    if_revision: 2,
                },
                || false,
                cx,
            );
            assert_eq!(replaced, Ok(()));
            assert_eq!(
                model.read_with(cx, |model, _| model.resolved_contributions()[0]
                    .decoration
                    .clone()),
                Some("error".to_owned())
            );

            for id in 6..=7 {
                let disposed = bridge.dispose_editor_contributions(
                    &request(extension, lifecycle, id),
                    BufferArguments { buffer },
                    || false,
                    cx,
                );
                assert_eq!(disposed, Ok(()));
            }
            assert!(model.read_with(cx, |model, _| model.resolved_contributions().is_empty()));

            let _ = bridge.replace_editor_contributions(
                &request(extension, lifecycle, 8),
                ContributionArguments {
                    buffer,
                    contributions: vec![replacement],
                    if_revision: 2,
                },
                || false,
                cx,
            );
            bridge.remove_lifecycle(extension, lifecycle, cx);
            assert!(model.read_with(cx, |model, _| model.resolved_contributions().is_empty()));
            model.update(cx, |model, _| {
                model.replace(0..0, "z").unwrap();
            });
            assert!(bridge.drain_model_changes(&model, cx).is_empty());

            let stale = bridge.snapshot(
                &request(extension, lifecycle, 9),
                SnapshotArguments {
                    buffer,
                    range: None,
                },
                || false,
                cx,
            );
            assert_eq!(stale, Err(BufferError::Access(AccessError::Cancelled)));
        });
    }
}
