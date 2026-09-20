//! Foreground dispatch for extension buffer requests.

use std::collections::HashSet;

use gpui::{App, Entity};

use crate::host::protocol::{
    BufferChange, BufferHandle, BufferSubscriptionId, ExtensionId, ExtensionLifecycleId,
    HostOperation, HostRequest, HostRequestError, HostResponse, HostResponseValue,
};

use super::model::{
    BufferAccessError, BufferClosed, BufferModel, BufferRegistry, BufferSubscriptionRegistry,
    ContributionError, ContributionSource,
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
/// The product host feeds requests into [`Self::dispatch`] and enqueues each
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

    pub(crate) fn set_active_buffer(&mut self, buffer: Option<BufferHandle>) {
        self.buffers.set_active(buffer);
    }

    #[allow(
        dead_code,
        reason = "document closure notifications do not yet expose a product buffer-removal event"
    )]
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

    /// Handle one typed request on the foreground thread.
    ///
    /// `is_cancelled` is evaluated when the request arrives and again inside
    /// the foreground model update immediately before any mutation. This lets
    /// the command owner reject cancellation that races request delivery.
    pub(crate) fn dispatch<F>(
        &mut self,
        request: HostRequest,
        mut is_cancelled: F,
        cx: &mut App,
    ) -> HostResponse
    where
        F: FnMut() -> bool,
    {
        let identity = (request.extension, request.lifecycle);
        let result = if !self.lifecycles.contains(&identity) || is_cancelled() {
            Err(HostRequestError::Cancelled)
        } else {
            self.dispatch_live(&request, &mut is_cancelled, cx)
        };
        HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result,
        }
    }

    fn dispatch_live<F>(
        &mut self,
        request: &HostRequest,
        is_cancelled: &mut F,
        cx: &mut App,
    ) -> Result<HostResponseValue, HostRequestError>
    where
        F: FnMut() -> bool,
    {
        let identity = (request.extension, request.lifecycle);
        match &request.operation {
            HostOperation::ActiveBuffer => Ok(HostResponseValue::ActiveBuffer(
                self.buffers.active_handle(),
            )),
            HostOperation::Snapshot { buffer, range } => self
                .buffers
                .resolve(*buffer)
                .map_err(|_| HostRequestError::BufferClosed)?
                .read_with(cx, |model, _| model.snapshot(*range))
                .map(HostResponseValue::Snapshot)
                .map_err(map_buffer_error),
            HostOperation::ApplyEdits {
                buffer,
                edits,
                if_revision,
            } => {
                let model = self
                    .buffers
                    .resolve(*buffer)
                    .map_err(|_| HostRequestError::BufferClosed)?;
                if !self.lifecycles.contains(&identity) {
                    return Err(HostRequestError::Cancelled);
                }
                model.update(cx, |model, cx| {
                    if is_cancelled() {
                        return Err(HostRequestError::Cancelled);
                    }
                    if model
                        .apply_edits(edits, *if_revision)
                        .map_err(map_buffer_error)?
                    {
                        cx.notify();
                    }
                    Ok(HostResponseValue::AppliedEdits {
                        revision: model.revision(),
                    })
                })
            }
            HostOperation::SubscribeBufferChanges { buffer } => {
                self.buffers
                    .resolve(*buffer)
                    .map_err(|_| HostRequestError::BufferClosed)?;
                Ok(HostResponseValue::BufferChangesSubscribed {
                    subscription: self.subscriptions.subscribe(
                        *buffer,
                        request.extension,
                        request.lifecycle,
                    ),
                })
            }
            HostOperation::UnsubscribeBufferChanges { subscription } => {
                if self.subscriptions.unsubscribe(
                    *subscription,
                    request.extension,
                    request.lifecycle,
                ) {
                    Ok(HostResponseValue::BufferChangesUnsubscribed {
                        subscription: *subscription,
                    })
                } else {
                    Err(HostRequestError::BufferClosed)
                }
            }
            HostOperation::ReplaceEditorContributions {
                buffer,
                contributions,
                if_revision,
            } => {
                let model = self
                    .buffers
                    .resolve(*buffer)
                    .map_err(|_| HostRequestError::BufferClosed)?;
                if !self.lifecycles.contains(&identity) {
                    return Err(HostRequestError::Cancelled);
                }
                model.update(cx, |model, cx| {
                    if is_cancelled() {
                        return Err(HostRequestError::Cancelled);
                    }
                    model
                        .replace_contributions(
                            ContributionSource::Extension {
                                extension: request.extension,
                                lifecycle: request.lifecycle,
                            },
                            contributions,
                            *if_revision,
                        )
                        .map_err(map_contribution_error)?;
                    cx.notify();
                    Ok(HostResponseValue::EditorContributionsReplaced)
                })
            }
            HostOperation::DisposeEditorContributions { buffer } => {
                let model = self
                    .buffers
                    .resolve(*buffer)
                    .map_err(|_| HostRequestError::BufferClosed)?;
                if !self.lifecycles.contains(&identity) {
                    return Err(HostRequestError::Cancelled);
                }
                model.update(cx, |model, cx| {
                    if is_cancelled() {
                        return Err(HostRequestError::Cancelled);
                    }
                    match model.dispose_contributions(ContributionSource::Extension {
                        extension: request.extension,
                        lifecycle: request.lifecycle,
                    }) {
                        Ok(()) => cx.notify(),
                        Err(ContributionError::NotFound) => {}
                        Err(error) => return Err(map_contribution_error(error)),
                    }
                    Ok(HostResponseValue::EditorContributionsDisposed)
                })
            }
            _ => Err(HostRequestError::UnsupportedOperation),
        }
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

fn map_buffer_error(error: BufferAccessError) -> HostRequestError {
    match error {
        BufferAccessError::Closed => HostRequestError::BufferClosed,
        BufferAccessError::ReadOnly => HostRequestError::UnsupportedOperation,
        BufferAccessError::InvalidRange => HostRequestError::InvalidRange,
        BufferAccessError::InvalidEditBatch => HostRequestError::InvalidEditBatch,
        BufferAccessError::RevisionConflict => HostRequestError::RevisionConflict,
    }
}

fn map_contribution_error(error: ContributionError) -> HostRequestError {
    match error {
        ContributionError::Closed => HostRequestError::BufferClosed,
        ContributionError::InvalidRange => HostRequestError::InvalidRange,
        ContributionError::RevisionConflict => HostRequestError::RevisionConflict,
        ContributionError::NotFound => HostRequestError::ContributionSetNotFound,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{AppContext, TestAppContext};

    use crate::host::protocol::{
        ByteRange, DecorationToken, EditorContribution, GutterToken, RequestId, TextEdit,
    };

    use super::*;

    fn request(
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        id: u64,
        operation: HostOperation,
    ) -> HostRequest {
        HostRequest {
            extension,
            lifecycle,
            id: RequestId::new(id),
            invocation: None,
            operation,
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
            bridge.set_active_buffer(Some(buffer));

            let snapshot = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    1,
                    HostOperation::Snapshot {
                        buffer,
                        range: Some(ByteRange {
                            start_byte_offset: 1,
                            end_byte_offset: 6,
                        }),
                    },
                ),
                || false,
                cx,
            );
            let HostResponseValue::Snapshot(snapshot) = snapshot.result.unwrap() else {
                panic!("expected snapshot response");
            };
            assert_eq!(snapshot.text.to_utf8(), "é中");
            assert_eq!(snapshot.revision, 0);

            let checks = Cell::new(0);
            let cancelled = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    2,
                    HostOperation::ApplyEdits {
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
                ),
                || {
                    let check = checks.get();
                    checks.set(check + 1);
                    check == 1
                },
                cx,
            );
            assert_eq!(cancelled.result, Err(HostRequestError::Cancelled));
            assert_eq!(checks.get(), 2);
            assert_eq!(model.read_with(cx, |model, _| model.text()), "aé中z");

            let applied = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    3,
                    HostOperation::ApplyEdits {
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
                ),
                || false,
                cx,
            );
            assert_eq!(
                applied.result,
                Ok(HostResponseValue::AppliedEdits { revision: 1 })
            );
            let stale = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    4,
                    HostOperation::ApplyEdits {
                        buffer,
                        edits: Vec::new(),
                        if_revision: 0,
                    },
                ),
                || false,
                cx,
            );
            assert_eq!(stale.result, Err(HostRequestError::RevisionConflict));
            assert_eq!(model.read(cx).text(), "aE中z");
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

            let subscribed = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    1,
                    HostOperation::SubscribeBufferChanges { buffer },
                ),
                || false,
                cx,
            );
            let HostResponseValue::BufferChangesSubscribed { subscription } =
                subscribed.result.unwrap()
            else {
                panic!("expected subscription response");
            };

            let replaced = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    2,
                    HostOperation::ReplaceEditorContributions {
                        buffer,
                        contributions: vec![EditorContribution {
                            range: ByteRange {
                                start_byte_offset: 1,
                                end_byte_offset: 3,
                            },
                            decoration: Some(DecorationToken::Warning),
                            gutter: Some(GutterToken::Info),
                            command: Some("fixture.action".into()),
                        }],
                        if_revision: 0,
                    },
                ),
                || false,
                cx,
            );
            assert_eq!(
                replaced.result,
                Ok(HostResponseValue::EditorContributionsReplaced)
            );
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

            let unsubscribed = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    3,
                    HostOperation::UnsubscribeBufferChanges { subscription },
                ),
                || false,
                cx,
            );
            assert_eq!(
                unsubscribed.result,
                Ok(HostResponseValue::BufferChangesUnsubscribed { subscription })
            );
            model.update(cx, |model, _| {
                model.replace(0..0, "y").unwrap();
            });
            assert!(bridge.drain_model_changes(&model, cx).is_empty());

            let resubscribed = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    4,
                    HostOperation::SubscribeBufferChanges { buffer },
                ),
                || false,
                cx,
            );
            let HostResponseValue::BufferChangesSubscribed {
                subscription: replacement_subscription,
            } = resubscribed.result.unwrap()
            else {
                panic!("expected replacement subscription response");
            };
            assert_ne!(replacement_subscription, subscription);

            let replacement = EditorContribution {
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 1,
                },
                decoration: Some(DecorationToken::Error),
                gutter: None,
                command: None,
            };
            let replaced = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    5,
                    HostOperation::ReplaceEditorContributions {
                        buffer,
                        contributions: vec![replacement.clone()],
                        if_revision: 2,
                    },
                ),
                || false,
                cx,
            );
            assert_eq!(
                replaced.result,
                Ok(HostResponseValue::EditorContributionsReplaced)
            );
            assert_eq!(
                model.read_with(cx, |model, _| model.resolved_contributions()[0].decoration),
                Some(DecorationToken::Error)
            );

            for id in 6..=7 {
                let disposed = bridge.dispatch(
                    request(
                        extension,
                        lifecycle,
                        id,
                        HostOperation::DisposeEditorContributions { buffer },
                    ),
                    || false,
                    cx,
                );
                assert_eq!(
                    disposed.result,
                    Ok(HostResponseValue::EditorContributionsDisposed)
                );
            }
            assert!(model.read_with(cx, |model, _| model.resolved_contributions().is_empty()));

            bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    8,
                    HostOperation::ReplaceEditorContributions {
                        buffer,
                        contributions: vec![replacement],
                        if_revision: 2,
                    },
                ),
                || false,
                cx,
            );
            bridge.remove_lifecycle(extension, lifecycle, cx);
            assert!(model.read_with(cx, |model, _| model.resolved_contributions().is_empty()));
            model.update(cx, |model, _| {
                model.replace(0..0, "z").unwrap();
            });
            assert!(bridge.drain_model_changes(&model, cx).is_empty());

            let stale = bridge.dispatch(
                request(extension, lifecycle, 9, HostOperation::ActiveBuffer),
                || false,
                cx,
            );
            assert_eq!(stale.result, Err(HostRequestError::Cancelled));
        });
    }
}
