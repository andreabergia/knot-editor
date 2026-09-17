//! Foreground command routing for extension lifecycles.
//!
//! This module owns command definitions and invocation trees without owning a
//! runtime pool. Callers execute the emitted events and report their outcomes
//! back, which keeps product and V8 integration outside the command model.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::host::protocol::{
    BufferHandle, Command, CommandInvocation, CommandInvocationId, CommandInvokeDispatch,
    CommandOutcome, ExtensionId, ExtensionLifecycleId, HostOperation, HostRequest,
    HostRequestError, HostResponse, HostResponseValue, RequestId,
};

use super::{
    CommandCompletion, CommandExecution,
    model::{
        CommandCatalog, CommandCatalogError, CommandDefinition, CommandTarget, CommandTargetKind,
    },
};

type Lifecycle = (ExtensionId, ExtensionLifecycleId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvocationTarget {
    Native,
    Extension(CommandTarget),
    Unavailable,
}

struct PendingHostResponse {
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
    request: RequestId,
}

struct InvocationNode {
    command: Command,
    target: InvocationTarget,
    buffer: Option<BufferHandle>,
    parent: Option<CommandInvocationId>,
    child: Option<CommandInvocationId>,
    completion: CommandCompletion,
    host_response: Option<PendingHostResponse>,
    handler_outcome: Option<CommandOutcome>,
    started: bool,
    cancelled: bool,
}

/// Work for the eventual product/runtime integration to execute.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ExtensionCommandEvent {
    DispatchExtension {
        invocation: CommandInvocation,
        buffer: Option<BufferHandle>,
    },
    DispatchNative {
        invocation: CommandInvocationId,
        command: Command,
        buffer: Option<BufferHandle>,
    },
    CancelExtension {
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        invocation: CommandInvocationId,
    },
    HostResponse(HostResponse),
}

/// Foreground-authoritative command catalog and serial invocation tree.
pub(crate) struct ExtensionCommandBridge {
    catalog: CommandCatalog,
    lifecycles: HashSet<Lifecycle>,
    next_invocation: u64,
    invocations: HashMap<CommandInvocationId, InvocationNode>,
    roots: VecDeque<CommandInvocationId>,
    active_root: Option<CommandInvocationId>,
    events: VecDeque<ExtensionCommandEvent>,
}

impl ExtensionCommandBridge {
    pub(crate) fn new() -> Self {
        Self {
            catalog: CommandCatalog::new(),
            lifecycles: HashSet::new(),
            next_invocation: 1,
            invocations: HashMap::new(),
            roots: VecDeque::new(),
            active_root: None,
            events: VecDeque::new(),
        }
    }

    pub(crate) fn admit_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        self.lifecycles.insert((extension, lifecycle));
    }

    pub(crate) fn register_native(
        &mut self,
        name: impl Into<crate::host::protocol::CommandName>,
        title: impl Into<String>,
    ) -> Result<(), CommandCatalogError> {
        self.catalog.register_native(name.into(), title.into())
    }

    pub(crate) fn definitions(&self) -> impl Iterator<Item = &CommandDefinition> {
        self.catalog.definitions()
    }

    pub(crate) fn enqueue_root(
        &mut self,
        command: Command,
        buffer: Option<BufferHandle>,
    ) -> CommandExecution {
        self.enqueue_root_for(command, buffer, None, None)
    }

    /// Handles command protocol operations. A `None` result means the invoke
    /// response is deferred until its routed invocation settles.
    pub(crate) fn handle_host_request(
        &mut self,
        request: HostRequest,
        captured_buffer: Option<BufferHandle>,
    ) -> Option<HostResponse> {
        let identity = (request.extension, request.lifecycle);
        if !self.lifecycles.contains(&identity) {
            return Some(response(&request, Err(HostRequestError::Cancelled)));
        }

        match request.operation.clone() {
            HostOperation::RegisterCommand { name, title } => Some(response(
                &request,
                self.catalog
                    .register_extension(name, title, request.extension, request.lifecycle)
                    .map(|registration| HostResponseValue::CommandRegistered { registration })
                    .map_err(map_catalog_error),
            )),
            HostOperation::UnregisterCommand { registration } => Some(response(
                &request,
                self.catalog
                    .unregister(registration, request.extension, request.lifecycle)
                    .map(|()| HostResponseValue::CommandUnregistered { registration })
                    .map_err(map_catalog_error),
            )),
            HostOperation::InvokeCommand { command } => {
                if let Some(parent) = request.invocation {
                    match self.enqueue_child(parent, command, identity, Some(&request)) {
                        Ok(Some(dispatch)) => Some(response(
                            &request,
                            Ok(HostResponseValue::CommandInvoked { dispatch }),
                        )),
                        Ok(None) => None,
                        Err(outcome) => Some(command_outcome_response(&request, outcome)),
                    }
                } else {
                    self.enqueue_root_for(
                        command,
                        captured_buffer,
                        Some(identity),
                        Some(PendingHostResponse {
                            extension: request.extension,
                            lifecycle: request.lifecycle,
                            request: request.id,
                        }),
                    );
                    None
                }
            }
            HostOperation::CompleteInlineCommand {
                invocation,
                outcome,
            } => {
                let authorized = request.invocation == Some(invocation)
                    && self.invocations.get(&invocation).is_some_and(|node| {
                        matches!(
                            node.target,
                            InvocationTarget::Extension(target)
                                if (target.extension, target.lifecycle) == identity
                        )
                    });
                if !authorized {
                    return Some(response(&request, Err(HostRequestError::Cancelled)));
                }
                self.complete(invocation, outcome);
                Some(response(
                    &request,
                    Ok(HostResponseValue::InlineCommandCompleted { invocation }),
                ))
            }
            _ => Some(response(
                &request,
                Err(HostRequestError::UnsupportedOperation),
            )),
        }
    }

    pub(crate) fn complete(&mut self, invocation: CommandInvocationId, outcome: CommandOutcome) {
        let Some(node) = self.invocations.get_mut(&invocation) else {
            return;
        };
        if node.handler_outcome.is_some() {
            return;
        }
        node.handler_outcome = Some(if node.cancelled {
            CommandOutcome::Cancelled
        } else {
            outcome
        });
        if let Some(child) = node.child {
            self.cancel_subtree(child);
        } else {
            self.settle(invocation);
        }
    }

    pub(crate) fn cancel(&mut self, invocation: CommandInvocationId) {
        self.cancel_subtree(invocation);
        if self
            .invocations
            .get(&invocation)
            .is_some_and(|node| !node.started)
        {
            self.complete(invocation, CommandOutcome::Cancelled);
        }
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        let identity = (extension, lifecycle);
        self.lifecycles.remove(&identity);
        self.catalog.remove_lifecycle(extension, lifecycle);
        let affected = self
            .invocations
            .iter()
            .filter_map(|(id, node)| {
                (matches!(
                    node.target,
                    InvocationTarget::Extension(target)
                        if (target.extension, target.lifecycle) == identity
                ) || node
                    .host_response
                    .as_ref()
                    .is_some_and(|pending| (pending.extension, pending.lifecycle) == identity))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        for invocation in &affected {
            self.cancel_subtree(*invocation);
        }
        for invocation in affected {
            self.complete(invocation, CommandOutcome::Cancelled);
        }
    }

    pub(crate) fn drain_events(&mut self) -> impl Iterator<Item = ExtensionCommandEvent> + '_ {
        self.events.drain(..)
    }

    fn enqueue_root_for(
        &mut self,
        command: Command,
        buffer: Option<BufferHandle>,
        caller: Option<Lifecycle>,
        host_response: Option<PendingHostResponse>,
    ) -> CommandExecution {
        let id = self.allocate_invocation();
        let (completion, receiver) = CommandCompletion::new();
        let target = match self.catalog.resolve(command.name.as_ref()) {
            Ok(CommandTargetKind::Native) => InvocationTarget::Native,
            Ok(CommandTargetKind::Extension(target))
                if caller == Some((target.extension, target.lifecycle)) =>
            {
                InvocationTarget::Unavailable
            }
            Ok(CommandTargetKind::Extension(target)) => InvocationTarget::Extension(target),
            Err(_) => InvocationTarget::Unavailable,
        };
        self.invocations.insert(
            id,
            InvocationNode {
                command,
                target,
                buffer,
                parent: None,
                child: None,
                completion,
                host_response,
                handler_outcome: None,
                started: false,
                cancelled: false,
            },
        );
        self.roots.push_back(id);
        self.start_next_root();
        CommandExecution {
            id,
            completion: receiver,
        }
    }

    fn enqueue_child(
        &mut self,
        parent: CommandInvocationId,
        command: Command,
        caller: Lifecycle,
        request: Option<&HostRequest>,
    ) -> Result<Option<CommandInvokeDispatch>, CommandOutcome> {
        let Some(parent_node) = self.invocations.get(&parent) else {
            return Err(CommandOutcome::Unavailable);
        };
        if parent_node.cancelled {
            return Err(CommandOutcome::Cancelled);
        }
        if parent_node.handler_outcome.is_some()
            || parent_node.child.is_some()
            || !matches!(
                parent_node.target,
                InvocationTarget::Extension(target)
                    if (target.extension, target.lifecycle) == caller
            )
        {
            return Err(CommandOutcome::Unavailable);
        }
        let target = match self.catalog.resolve(command.name.as_ref()) {
            Ok(CommandTargetKind::Native) => InvocationTarget::Native,
            Ok(CommandTargetKind::Extension(target)) => InvocationTarget::Extension(target),
            Err(_) => return Err(CommandOutcome::Unavailable),
        };
        let same_runtime = matches!(
            target,
            InvocationTarget::Extension(target)
                if (target.extension, target.lifecycle) == caller
        );
        let mut ancestor = Some(parent);
        while let Some(id) = ancestor {
            let node = self
                .invocations
                .get(&id)
                .expect("active ancestry is complete");
            if let (InvocationTarget::Extension(ancestor), InvocationTarget::Extension(child)) =
                (node.target, target)
            {
                let same_owner =
                    (ancestor.extension, ancestor.lifecycle) == (child.extension, child.lifecycle);
                if same_owner && (!same_runtime || ancestor.registration == child.registration) {
                    return Err(CommandOutcome::Unavailable);
                }
            }
            ancestor = node.parent;
        }

        let id = self.allocate_invocation();
        let (completion, _receiver) = CommandCompletion::new();
        let buffer = self.invocations[&parent].buffer;
        let host_response = request.map(|request| PendingHostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            request: request.id,
        });
        self.invocations.insert(
            id,
            InvocationNode {
                command,
                target,
                buffer,
                parent: Some(parent),
                child: None,
                completion,
                host_response: if same_runtime { None } else { host_response },
                handler_outcome: None,
                started: same_runtime,
                cancelled: false,
            },
        );
        self.invocations.get_mut(&parent).unwrap().child = Some(id);
        if same_runtime {
            let InvocationTarget::Extension(target) = target else {
                unreachable!();
            };
            Ok(Some(CommandInvokeDispatch::Inline {
                invocation: id,
                registration: target.registration,
            }))
        } else {
            self.start_invocation(id);
            Ok(None)
        }
    }

    fn start_next_root(&mut self) {
        if self.active_root.is_some() {
            return;
        }
        if let Some(root) = self.roots.pop_front() {
            self.active_root = Some(root);
            self.start_invocation(root);
        }
    }

    fn start_invocation(&mut self, invocation: CommandInvocationId) {
        let Some(node) = self.invocations.get_mut(&invocation) else {
            return;
        };
        if node.started {
            return;
        }
        node.started = true;
        match node.target {
            InvocationTarget::Unavailable => {
                self.complete(invocation, CommandOutcome::Unavailable);
            }
            InvocationTarget::Native => {
                self.events
                    .push_back(ExtensionCommandEvent::DispatchNative {
                        invocation,
                        command: node.command.clone(),
                        buffer: node.buffer,
                    });
            }
            InvocationTarget::Extension(target) => {
                if !self
                    .lifecycles
                    .contains(&(target.extension, target.lifecycle))
                {
                    self.complete(invocation, CommandOutcome::Unavailable);
                    return;
                }
                self.events
                    .push_back(ExtensionCommandEvent::DispatchExtension {
                        invocation: CommandInvocation {
                            id: invocation,
                            registration: target.registration,
                            extension: target.extension,
                            lifecycle: target.lifecycle,
                            arguments: node.command.arguments.clone(),
                        },
                        buffer: node.buffer,
                    });
            }
        }
    }

    fn cancel_subtree(&mut self, invocation: CommandInvocationId) {
        let Some(node) = self.invocations.get_mut(&invocation) else {
            return;
        };
        if node.cancelled {
            return;
        }
        node.cancelled = true;
        let child = node.child;
        if node.started {
            if let InvocationTarget::Extension(target) = node.target {
                self.events
                    .push_back(ExtensionCommandEvent::CancelExtension {
                        extension: target.extension,
                        lifecycle: target.lifecycle,
                        invocation,
                    });
            }
        }
        if let Some(child) = child {
            self.cancel_subtree(child);
        }
    }

    fn settle(&mut self, invocation: CommandInvocationId) {
        let Some(node) = self.invocations.remove(&invocation) else {
            return;
        };
        let outcome = node.handler_outcome.unwrap_or(if node.cancelled {
            CommandOutcome::Cancelled
        } else {
            CommandOutcome::Unavailable
        });
        node.completion.complete(outcome.clone());
        if let Some(pending) = node.host_response {
            self.events
                .push_back(ExtensionCommandEvent::HostResponse(HostResponse {
                    extension: pending.extension,
                    lifecycle: pending.lifecycle,
                    id: pending.request,
                    result: Ok(HostResponseValue::CommandInvoked {
                        dispatch: CommandInvokeDispatch::Outcome {
                            outcome: outcome.clone(),
                        },
                    }),
                }));
        }
        if let Some(parent) = node.parent {
            let parent_ready = self.invocations.get_mut(&parent).is_some_and(|parent| {
                if parent.child == Some(invocation) {
                    parent.child = None;
                }
                parent.child.is_none() && parent.handler_outcome.is_some()
            });
            if parent_ready {
                self.settle(parent);
            }
        } else if self.active_root == Some(invocation) {
            self.active_root = None;
            self.start_next_root();
        } else {
            self.roots.retain(|root| *root != invocation);
        }
    }

    fn allocate_invocation(&mut self) -> CommandInvocationId {
        let id = CommandInvocationId::new(self.next_invocation);
        self.next_invocation = self
            .next_invocation
            .checked_add(1)
            .expect("command invocation space exhausted");
        id
    }
}

impl Default for ExtensionCommandBridge {
    fn default() -> Self {
        Self::new()
    }
}

fn response(
    request: &HostRequest,
    result: Result<HostResponseValue, HostRequestError>,
) -> HostResponse {
    HostResponse {
        extension: request.extension,
        lifecycle: request.lifecycle,
        id: request.id,
        result,
    }
}

fn command_outcome_response(request: &HostRequest, outcome: CommandOutcome) -> HostResponse {
    response(
        request,
        Ok(HostResponseValue::CommandInvoked {
            dispatch: CommandInvokeDispatch::Outcome { outcome },
        }),
    )
}

fn map_catalog_error(error: CommandCatalogError) -> HostRequestError {
    match error {
        CommandCatalogError::NameInUse => HostRequestError::CommandNameInUse,
        CommandCatalogError::NotFound => HostRequestError::CommandNotFound,
    }
}

#[cfg(test)]
mod tests {
    use crate::host::protocol::{
        CommandArgumentValue, CommandName, CommandRegistrationId, ExtensionLifecycleId,
    };

    use super::*;

    fn key(extension: u64) -> Lifecycle {
        (
            ExtensionId::new(extension),
            ExtensionLifecycleId::new(extension * 10),
        )
    }

    fn command(name: &str) -> Command {
        Command {
            name: CommandName::from(name),
            arguments: CommandArgumentValue::String("argument".into()),
        }
    }

    fn request(
        identity: Lifecycle,
        id: u64,
        invocation: Option<CommandInvocationId>,
        operation: HostOperation,
    ) -> HostRequest {
        HostRequest {
            extension: identity.0,
            lifecycle: identity.1,
            id: RequestId::new(id),
            invocation,
            operation,
        }
    }

    fn register(
        bridge: &mut ExtensionCommandBridge,
        identity: Lifecycle,
        name: &str,
        id: u64,
    ) -> CommandRegistrationId {
        let response = bridge
            .handle_host_request(
                request(
                    identity,
                    id,
                    None,
                    HostOperation::RegisterCommand {
                        name: name.into(),
                        title: name.into(),
                    },
                ),
                None,
            )
            .unwrap();
        let Ok(HostResponseValue::CommandRegistered { registration }) = response.result else {
            panic!("command registration failed")
        };
        registration
    }

    #[test]
    fn registration_is_lifecycle_scoped_and_disposable() {
        let mut bridge = ExtensionCommandBridge::new();
        let owner = key(1);
        let other = key(2);
        bridge.admit_lifecycle(owner.0, owner.1);
        bridge.admit_lifecycle(other.0, other.1);
        let registration = register(&mut bridge, owner, "fixture.command", 1);
        assert!(matches!(
            bridge
                .handle_host_request(
                    request(
                        other,
                        2,
                        None,
                        HostOperation::RegisterCommand {
                            name: "fixture.command".into(),
                            title: "duplicate".into(),
                        },
                    ),
                    None,
                )
                .unwrap()
                .result,
            Err(HostRequestError::CommandNameInUse)
        ));
        assert!(matches!(
            bridge
                .handle_host_request(
                    request(
                        other,
                        3,
                        None,
                        HostOperation::UnregisterCommand { registration },
                    ),
                    None,
                )
                .unwrap()
                .result,
            Err(HostRequestError::CommandNotFound)
        ));
        bridge.remove_lifecycle(owner.0, owner.1);
        register(&mut bridge, other, "fixture.command", 4);
    }

    #[test]
    fn roots_are_fifo_and_keep_arguments_and_captured_buffer() {
        let mut bridge = ExtensionCommandBridge::new();
        let owner = key(1);
        bridge.admit_lifecycle(owner.0, owner.1);
        let registration = register(&mut bridge, owner, "fixture.command", 1);
        let buffer = Some(BufferHandle::new(9));
        let mut first = bridge.enqueue_root(command("fixture.command"), buffer);
        let mut second = bridge.enqueue_root(command("fixture.command"), None);
        assert_eq!(first.id, CommandInvocationId::new(1));
        assert_eq!(second.id, CommandInvocationId::new(2));
        assert_eq!(
            bridge.drain_events().collect::<Vec<_>>(),
            vec![ExtensionCommandEvent::DispatchExtension {
                invocation: CommandInvocation {
                    id: first.id,
                    registration,
                    extension: owner.0,
                    lifecycle: owner.1,
                    arguments: CommandArgumentValue::String("argument".into()),
                },
                buffer,
            }]
        );
        bridge.complete(first.id, CommandOutcome::Completed);
        assert!(matches!(
            bridge.drain_events().next(),
            Some(ExtensionCommandEvent::DispatchExtension { invocation, buffer: None })
                if invocation.id == second.id
        ));
        bridge.complete(second.id, CommandOutcome::Completed);
        assert_eq!(
            first.completion.try_recv().unwrap(),
            CommandOutcome::Completed
        );
        assert_eq!(
            second.completion.try_recv().unwrap(),
            CommandOutcome::Completed
        );
    }

    #[test]
    fn same_runtime_children_inline_and_cycles_or_second_children_are_rejected() {
        let mut bridge = ExtensionCommandBridge::new();
        let owner = key(1);
        bridge.admit_lifecycle(owner.0, owner.1);
        let outer = register(&mut bridge, owner, "fixture.outer", 1);
        let inner = register(&mut bridge, owner, "fixture.inner", 2);
        let root = bridge.enqueue_root(command("fixture.outer"), Some(BufferHandle::new(4)));
        bridge.drain_events().for_each(drop);

        let nested = request(
            owner,
            3,
            Some(root.id),
            HostOperation::InvokeCommand {
                command: command("fixture.inner"),
            },
        );
        let response = bridge.handle_host_request(nested, None).unwrap();
        let Ok(HostResponseValue::CommandInvoked {
            dispatch:
                CommandInvokeDispatch::Inline {
                    invocation: child,
                    registration,
                },
        }) = response.result
        else {
            panic!("same-runtime child was not inlined")
        };
        assert_eq!(registration, inner);

        let second = bridge
            .handle_host_request(
                request(
                    owner,
                    4,
                    Some(root.id),
                    HostOperation::InvokeCommand {
                        command: command("fixture.inner"),
                    },
                ),
                None,
            )
            .unwrap();
        assert!(matches!(
            second.result,
            Ok(HostResponseValue::CommandInvoked {
                dispatch: CommandInvokeDispatch::Outcome {
                    outcome: CommandOutcome::Unavailable
                }
            })
        ));
        let recursive = bridge
            .handle_host_request(
                request(
                    owner,
                    5,
                    Some(child),
                    HostOperation::InvokeCommand {
                        command: command("fixture.outer"),
                    },
                ),
                None,
            )
            .unwrap();
        assert!(matches!(
            recursive.result,
            Ok(HostResponseValue::CommandInvoked {
                dispatch: CommandInvokeDispatch::Outcome {
                    outcome: CommandOutcome::Unavailable
                }
            })
        ));
        assert_ne!(outer, inner);
    }

    #[test]
    fn cross_extension_child_defers_response_and_preserves_parent_buffer() {
        let mut bridge = ExtensionCommandBridge::new();
        let parent_owner = key(1);
        let child_owner = key(2);
        bridge.admit_lifecycle(parent_owner.0, parent_owner.1);
        bridge.admit_lifecycle(child_owner.0, child_owner.1);
        register(&mut bridge, parent_owner, "fixture.parent", 1);
        let child_registration = register(&mut bridge, child_owner, "fixture.child", 2);
        let buffer = Some(BufferHandle::new(7));
        let root = bridge.enqueue_root(command("fixture.parent"), buffer);
        bridge.drain_events().for_each(drop);
        assert!(
            bridge
                .handle_host_request(
                    request(
                        parent_owner,
                        3,
                        Some(root.id),
                        HostOperation::InvokeCommand {
                            command: command("fixture.child"),
                        },
                    ),
                    None,
                )
                .is_none()
        );
        let child = match bridge.drain_events().next().unwrap() {
            ExtensionCommandEvent::DispatchExtension {
                invocation,
                buffer: captured,
            } => {
                assert_eq!(invocation.registration, child_registration);
                assert_eq!(captured, buffer);
                invocation.id
            }
            event => panic!("unexpected event: {event:?}"),
        };
        bridge.complete(child, CommandOutcome::Completed);
        assert!(matches!(
            bridge.drain_events().next(),
            Some(ExtensionCommandEvent::HostResponse(HostResponse {
                id,
                result: Ok(HostResponseValue::CommandInvoked {
                    dispatch: CommandInvokeDispatch::Outcome {
                        outcome: CommandOutcome::Completed
                    }
                }),
                ..
            })) if id == RequestId::new(3)
        ));
    }

    #[test]
    fn cross_extension_ancestry_cycle_is_rejected() {
        let mut bridge = ExtensionCommandBridge::new();
        let first = key(1);
        let second = key(2);
        bridge.admit_lifecycle(first.0, first.1);
        bridge.admit_lifecycle(second.0, second.1);
        register(&mut bridge, first, "fixture.first", 1);
        register(&mut bridge, second, "fixture.second", 2);
        let root = bridge.enqueue_root(command("fixture.first"), None);
        bridge.drain_events().for_each(drop);
        assert!(
            bridge
                .handle_host_request(
                    request(
                        first,
                        3,
                        Some(root.id),
                        HostOperation::InvokeCommand {
                            command: command("fixture.second"),
                        },
                    ),
                    None,
                )
                .is_none()
        );
        let child = match bridge.drain_events().next().unwrap() {
            ExtensionCommandEvent::DispatchExtension { invocation, .. } => invocation.id,
            event => panic!("unexpected event: {event:?}"),
        };
        let cycle = bridge
            .handle_host_request(
                request(
                    second,
                    4,
                    Some(child),
                    HostOperation::InvokeCommand {
                        command: command("fixture.first"),
                    },
                ),
                None,
            )
            .unwrap();
        assert!(matches!(
            cycle.result,
            Ok(HostResponseValue::CommandInvoked {
                dispatch: CommandInvokeDispatch::Outcome {
                    outcome: CommandOutcome::Unavailable
                }
            })
        ));
    }

    #[test]
    fn cancellation_and_lifecycle_cleanup_settle_once_and_release_the_queue() {
        let mut bridge = ExtensionCommandBridge::new();
        let owner = key(1);
        bridge.admit_lifecycle(owner.0, owner.1);
        register(&mut bridge, owner, "fixture.command", 1);
        let mut first = bridge.enqueue_root(command("fixture.command"), None);
        let mut second = bridge.enqueue_root(command("fixture.command"), None);
        bridge.drain_events().for_each(drop);
        bridge.cancel(first.id);
        assert!(matches!(
            bridge.drain_events().next(),
            Some(ExtensionCommandEvent::CancelExtension { invocation, .. })
                if invocation == first.id
        ));
        bridge.complete(first.id, CommandOutcome::Completed);
        bridge.complete(
            first.id,
            CommandOutcome::HandlerFailure {
                message: "late".into(),
            },
        );
        assert!(matches!(
            bridge.drain_events().next(),
            Some(ExtensionCommandEvent::DispatchExtension { invocation, .. })
                if invocation.id == second.id
        ));
        assert_eq!(
            first.completion.try_recv().unwrap(),
            CommandOutcome::Cancelled
        );

        bridge.remove_lifecycle(owner.0, owner.1);
        assert_eq!(
            second.completion.try_recv().unwrap(),
            CommandOutcome::Cancelled
        );
    }

    #[test]
    fn native_and_unsupported_requests_are_explicit() {
        let mut bridge = ExtensionCommandBridge::new();
        let caller = key(1);
        bridge.admit_lifecycle(caller.0, caller.1);
        bridge.register_native("editor.copy", "Copy").unwrap();
        let response = bridge.handle_host_request(
            request(
                caller,
                1,
                None,
                HostOperation::InvokeCommand {
                    command: command("editor.copy"),
                },
            ),
            Some(BufferHandle::new(3)),
        );
        assert!(response.is_none());
        let invocation = match bridge.drain_events().next() {
            Some(ExtensionCommandEvent::DispatchNative {
                invocation,
                command,
                ..
            }) if command.name.as_ref() == "editor.copy" => invocation,
            event => panic!("unexpected event: {event:?}"),
        };
        bridge.complete(invocation, CommandOutcome::Completed);
        assert!(matches!(
            bridge.drain_events().next(),
            Some(ExtensionCommandEvent::HostResponse(HostResponse {
                id,
                result: Ok(HostResponseValue::CommandInvoked {
                    dispatch: CommandInvokeDispatch::Outcome {
                        outcome: CommandOutcome::Completed
                    }
                }),
                ..
            })) if id == RequestId::new(1)
        ));
        let unsupported = bridge
            .handle_host_request(request(caller, 2, None, HostOperation::ActiveBuffer), None)
            .unwrap();
        assert_eq!(
            unsupported.result,
            Err(HostRequestError::UnsupportedOperation)
        );
    }
}
