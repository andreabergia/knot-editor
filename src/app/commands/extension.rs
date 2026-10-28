//! Foreground command routing for extension lifecycles.
//!
//! This module shares command definitions and owns invocation trees without owning a
//! runtime pool. Callers execute the emitted events and report their outcomes
//! back, which keeps product and V8 integration outside the command model.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::host::protocol::{
    BufferHandle, Command, CommandInvocation, CommandInvocationId, CommandInvokeDispatch,
    CommandName, CommandOutcome, CommandRegistrationId, ExtensionId, ExtensionLifecycleId,
    RequestContext,
};

#[cfg(test)]
use super::catalog::CommandCatalog;
#[cfg(test)]
use super::catalog::CommandDefinition;
use crate::app::extensions::host::handlers::AccessError;
use crate::host::declarations::HostError;

use crate::app::{
    CommandCompletion, CommandExecution,
    commands::catalog::{CommandCatalogError, CommandTarget, SharedCommandCatalog},
};

type Lifecycle = (ExtensionId, ExtensionLifecycleId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvocationTarget {
    Window,
    Extension(CommandTarget),
}

#[derive(Clone, Copy)]
pub(crate) enum RoutedCommandTarget {
    Window,
    Extension(CommandTarget),
}

struct PendingCommandReply {
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
    reply: CommandReply,
}

pub(crate) type CommandReply =
    tokio::sync::oneshot::Sender<Result<CommandInvokeDispatch, AccessError>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, HostError)]
pub(crate) enum CommandError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "CommandNameInUse", message = "CommandNameInUse")]
    CommandNameInUse,
    #[error(code = "CommandNotFound", message = "CommandNotFound")]
    CommandNotFound,
    #[error(code = "ViewNotFound", message = "ViewNotFound")]
    ViewNotFound,
}
impl From<AccessError> for CommandError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

struct InvocationNode {
    command: Command,
    target: InvocationTarget,
    buffer: Option<BufferHandle>,
    parent: Option<CommandInvocationId>,
    child: Option<CommandInvocationId>,
    completion: CommandCompletion,
    command_reply: Option<PendingCommandReply>,
    handler_outcome: Option<CommandOutcome>,
    started: bool,
    cancelled: bool,
}

/// Work emitted to the product-owned extension pool.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ExtensionCommandEvent {
    DispatchExtension {
        root: CommandInvocationId,
        invocation: CommandInvocation,
        buffer: Option<BufferHandle>,
    },
    CancelExtension {
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        invocation: CommandInvocationId,
    },
}

/// Foreground-authoritative command catalog and serial invocation tree.
pub(crate) struct ExtensionCommandBridge {
    catalog: SharedCommandCatalog,
    lifecycles: HashSet<Lifecycle>,
    next_invocation: u64,
    invocations: HashMap<CommandInvocationId, InvocationNode>,
    roots: VecDeque<CommandInvocationId>,
    active_root: Option<CommandInvocationId>,
    events: VecDeque<ExtensionCommandEvent>,
}

impl ExtensionCommandBridge {
    pub(crate) fn catalog(&self) -> SharedCommandCatalog {
        self.catalog.clone()
    }
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_catalog(std::rc::Rc::new(std::cell::RefCell::new(
            CommandCatalog::new(),
        )))
    }

    pub(crate) fn with_catalog(catalog: SharedCommandCatalog) -> Self {
        Self {
            catalog,
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

    #[cfg(test)]
    pub(crate) fn definitions(&self) -> impl Iterator<Item = CommandDefinition> {
        self.catalog
            .borrow()
            .definitions()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub(crate) fn enqueue_extension_root(
        &mut self,
        command: Command,
        handler: CommandTarget,
        buffer: Option<BufferHandle>,
    ) -> CommandExecution {
        self.enqueue_extension_root_for(command, buffer, handler)
    }

    fn validate_request(&self, request: &RequestContext) -> Result<(), AccessError> {
        if self
            .lifecycles
            .contains(&(request.extension, request.lifecycle))
        {
            Ok(())
        } else {
            Err(AccessError::Cancelled)
        }
    }

    pub(crate) fn register_command(
        &mut self,
        request: &RequestContext,
        name: CommandName,
        title: String,
    ) -> Result<CommandRegistrationId, CommandError> {
        self.validate_request(request)?;
        self.catalog
            .borrow_mut()
            .register_extension(name, title, request.extension, request.lifecycle)
            .map_err(map_catalog_error)
    }

    pub(crate) fn register_view_command(
        &mut self,
        request: &RequestContext,
        view_kind: String,
        name: CommandName,
    ) -> Result<CommandRegistrationId, CommandError> {
        self.validate_request(request)?;
        self.catalog
            .borrow_mut()
            .register_view_handler(view_kind, name, request.extension, request.lifecycle)
            .map_err(map_catalog_error)
    }

    pub(crate) fn unregister_command(
        &mut self,
        request: &RequestContext,
        registration: CommandRegistrationId,
    ) -> Result<(), CommandError> {
        self.validate_request(request)?;
        self.catalog
            .borrow_mut()
            .unregister(registration, request.extension, request.lifecycle)
            .map_err(map_catalog_error)
    }

    pub(crate) fn complete_inline(
        &mut self,
        request: &RequestContext,
        invocation: CommandInvocationId,
        outcome: CommandOutcome,
    ) -> Result<(), AccessError> {
        self.validate_request(request)?;
        let identity = (request.extension, request.lifecycle);
        let authorized = request.invocation == Some(invocation) && self.invocations.get(&invocation).is_some_and(|node| {
            matches!(node.target, InvocationTarget::Extension(target) if (target.extension, target.lifecycle) == identity)
        });
        if !authorized {
            return Err(AccessError::Cancelled);
        }
        self.complete(invocation, outcome);
        Ok(())
    }

    pub(crate) fn handle_routed_child(
        &mut self,
        request: &RequestContext,
        command: Command,
        target: RoutedCommandTarget,
        reply: &mut Option<CommandReply>,
    ) -> (
        Option<Result<CommandInvokeDispatch, AccessError>>,
        Option<CommandInvocationId>,
    ) {
        if let Err(error) = self.validate_request(request) {
            return (Some(Err(error)), None);
        }
        let Some(parent) = request.invocation else {
            return (Some(Err(AccessError::ContextUnavailable)), None);
        };
        let (target, already_started) = match target {
            RoutedCommandTarget::Window => (InvocationTarget::Window, true),
            RoutedCommandTarget::Extension(handler) => {
                (InvocationTarget::Extension(handler), false)
            }
        };
        match self.enqueue_child_with_target(
            parent,
            command,
            (request.extension, request.lifecycle),
            reply,
            target,
            already_started,
        ) {
            Ok((Some(dispatch), id)) => (Some(Ok(dispatch)), Some(id)),
            Ok((None, id)) => (None, Some(id)),
            Err(outcome) => (Some(Ok(CommandInvokeDispatch::Outcome { outcome })), None),
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

    #[cfg(test)]
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
        self.catalog
            .borrow_mut()
            .remove_lifecycle(extension, lifecycle);
        let affected = self
            .invocations
            .iter()
            .filter_map(|(id, node)| {
                (matches!(
                    node.target,
                    InvocationTarget::Extension(target)
                        if (target.extension, target.lifecycle) == identity
                ) || node
                    .command_reply
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

    pub(crate) fn contains_invocation(&self, invocation: CommandInvocationId) -> bool {
        self.invocations.contains_key(&invocation)
    }

    pub(crate) fn root_of(&self, invocation: CommandInvocationId) -> Option<CommandInvocationId> {
        self.invocations
            .contains_key(&invocation)
            .then(|| self.root_for(invocation))
    }

    pub(crate) fn admit_child(
        &self,
        parent: CommandInvocationId,
        caller: (ExtensionId, ExtensionLifecycleId),
    ) -> Result<(), CommandOutcome> {
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
        Ok(())
    }

    pub(crate) fn captured_buffer(&self, invocation: CommandInvocationId) -> Option<BufferHandle> {
        self.invocations
            .get(&invocation)
            .and_then(|node| node.buffer)
    }

    pub(crate) fn authorize_context(
        &self,
        request: &RequestContext,
        explicit: Option<CommandInvocationId>,
    ) -> Result<CommandInvocationId, AccessError> {
        let invocation = request.invocation.ok_or(if explicit.is_some() {
            AccessError::InvalidTarget
        } else {
            AccessError::ContextUnavailable
        })?;
        let node = self
            .invocations
            .get(&invocation)
            .ok_or(AccessError::InvalidTarget)?;
        if !matches!(node.target, InvocationTarget::Extension(handler)
            if (handler.extension, handler.lifecycle) == (request.extension, request.lifecycle))
        {
            return Err(AccessError::InvalidTarget);
        }
        if node.cancelled {
            return Err(AccessError::Cancelled);
        }
        if node.handler_outcome.is_some() {
            return Err(AccessError::InvalidTarget);
        }
        let root = self.root_for(invocation);
        if explicit.is_some_and(|authority| authority != root) {
            return Err(AccessError::InvalidTarget);
        }
        Ok(root)
    }

    pub(crate) fn is_cancelled(&self, invocation: Option<CommandInvocationId>) -> bool {
        invocation.is_some_and(|invocation| {
            self.invocations
                .get(&invocation)
                .is_none_or(|node| node.cancelled)
        })
    }

    fn enqueue_extension_root_for(
        &mut self,
        command: Command,
        buffer: Option<BufferHandle>,
        handler: CommandTarget,
    ) -> CommandExecution {
        let id = self.allocate_invocation();
        let (completion, receiver) = CommandCompletion::new();
        let target = InvocationTarget::Extension(handler);
        self.invocations.insert(
            id,
            InvocationNode {
                command,
                target,
                buffer,
                parent: None,
                child: None,
                completion,
                command_reply: None,
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

    fn enqueue_child_with_target(
        &mut self,
        parent: CommandInvocationId,
        command: Command,
        caller: Lifecycle,
        reply: &mut Option<CommandReply>,
        target: InvocationTarget,
        already_started: bool,
    ) -> Result<(Option<CommandInvokeDispatch>, CommandInvocationId), CommandOutcome> {
        self.admit_child(parent, caller)?;
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
        self.invocations.insert(
            id,
            InvocationNode {
                command,
                target,
                buffer,
                parent: Some(parent),
                child: None,
                completion,
                command_reply: if same_runtime {
                    None
                } else {
                    reply.take().map(|reply| PendingCommandReply {
                        extension: caller.0,
                        lifecycle: caller.1,
                        reply,
                    })
                },
                handler_outcome: None,
                started: same_runtime || already_started,
                cancelled: false,
            },
        );
        self.invocations.get_mut(&parent).unwrap().child = Some(id);
        if same_runtime {
            let InvocationTarget::Extension(target) = target else {
                unreachable!();
            };
            Ok((
                Some(CommandInvokeDispatch::Inline {
                    invocation: id,
                    registration: target.registration,
                }),
                id,
            ))
        } else {
            if !already_started {
                self.start_invocation(id);
            }
            Ok((None, id))
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
        let root = self.root_for(invocation);
        let Some(node) = self.invocations.get_mut(&invocation) else {
            return;
        };
        if node.started {
            return;
        }
        node.started = true;
        match node.target {
            InvocationTarget::Window => {}
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
                        root,
                        invocation: CommandInvocation {
                            id: invocation,
                            registration: target.registration,
                            extension: target.extension,
                            lifecycle: target.lifecycle,
                            arguments: node.command.arguments.clone(),
                            capture: None,
                        },
                        buffer: node.buffer,
                    });
            }
        }
    }

    fn root_for(&self, invocation: CommandInvocationId) -> CommandInvocationId {
        let mut current = invocation;
        while let Some(parent) = self.invocations.get(&current).and_then(|node| node.parent) {
            current = parent;
        }
        current
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
        if node.started
            && let InvocationTarget::Extension(target) = node.target
        {
            self.events
                .push_back(ExtensionCommandEvent::CancelExtension {
                    extension: target.extension,
                    lifecycle: target.lifecycle,
                    invocation,
                });
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
        if let Some(reply) = node.command_reply {
            let _ = reply.reply.send(Ok(CommandInvokeDispatch::Outcome {
                outcome: outcome.clone(),
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

#[cfg(test)]
impl Default for ExtensionCommandBridge {
    fn default() -> Self {
        Self::new()
    }
}

fn map_catalog_error(error: CommandCatalogError) -> CommandError {
    match error {
        CommandCatalogError::NameInUse => CommandError::CommandNameInUse,
        CommandCatalogError::NotFound => CommandError::CommandNotFound,
        CommandCatalogError::ViewNotFound => CommandError::ViewNotFound,
    }
}

#[cfg(test)]
mod tests;
