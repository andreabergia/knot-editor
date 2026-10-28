use crate::host::protocol::{
    CommandArgumentValue, CommandName, CommandRegistrationId, ExtensionLifecycleId,
};

use super::*;
use crate::app::declared_host as declared;
use crate::host::declarations::{DeclaredRequest, Handler};
use crate::host::protocol::{HostRequest, RequestId};
fn operation<H: Handler>(args: H::Arguments) -> DeclaredRequest {
    DeclaredRequest::new::<H>(args)
}

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
    operation: crate::host::declarations::DeclaredRequest,
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
    bridge
        .register_command(
            &RequestContext {
                extension: identity.0,
                lifecycle: identity.1,
                id: RequestId::new(id),
                invocation: None,
            },
            name.into(),
            name.into(),
        )
        .unwrap()
}

fn enqueue_registered_root(
    bridge: &mut ExtensionCommandBridge,
    command: Command,
    buffer: Option<BufferHandle>,
) -> CommandExecution {
    let target = bridge
        .catalog
        .borrow()
        .resolve_extension(command.name.as_ref())
        .unwrap();
    bridge.enqueue_extension_root(command, target, buffer)
}

type RoutedTestResult = (
    Option<Result<CommandInvokeDispatch, AccessError>>,
    Option<CommandInvocationId>,
    tokio::sync::oneshot::Receiver<Result<CommandInvokeDispatch, AccessError>>,
);

fn routed(bridge: &mut ExtensionCommandBridge, request: &HostRequest) -> RoutedTestResult {
    let command = request
        .operation
        .arguments::<declared::invoke_command>()
        .unwrap()
        .command
        .clone();
    let target = bridge
        .catalog
        .borrow()
        .resolve_extension(command.name.as_ref())
        .unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    let mut reply = Some(send);
    let (response, child) = bridge.handle_routed_child(
        &request.context(),
        command,
        RoutedCommandTarget::Extension(target),
        &mut reply,
    );
    (response, child, receive)
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
        bridge.register_command(
            &RequestContext {
                extension: other.0,
                lifecycle: other.1,
                id: RequestId::new(2),
                invocation: None
            },
            "fixture.command".into(),
            "duplicate".into()
        ),
        Err(CommandError::CommandNameInUse)
    ));
    assert!(matches!(
        bridge.unregister_command(
            &RequestContext {
                extension: other.0,
                lifecycle: other.1,
                id: RequestId::new(3),
                invocation: None
            },
            registration
        ),
        Err(CommandError::CommandNotFound)
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
    let mut first = enqueue_registered_root(&mut bridge, command("fixture.command"), buffer);
    let mut second = enqueue_registered_root(&mut bridge, command("fixture.command"), None);
    assert_eq!(first.id, CommandInvocationId::new(1));
    assert_eq!(second.id, CommandInvocationId::new(2));
    assert_eq!(
        bridge.drain_events().collect::<Vec<_>>(),
        vec![ExtensionCommandEvent::DispatchExtension {
            root: first.id,
            invocation: CommandInvocation {
                id: first.id,
                registration,
                extension: owner.0,
                lifecycle: owner.1,
                arguments: CommandArgumentValue::String("argument".into()),
                capture: None,
            },
            buffer,
        }]
    );
    bridge.complete(first.id, CommandOutcome::Completed);
    assert!(matches!(
        bridge.drain_events().next(),
        Some(ExtensionCommandEvent::DispatchExtension {
            invocation,
            buffer: None,
            ..
        })
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
fn routed_children_compose_and_reject_cycles() {
    let mut bridge = ExtensionCommandBridge::new();
    let first = key(1);
    let second = key(2);
    bridge.admit_lifecycle(first.0, first.1);
    bridge.admit_lifecycle(second.0, second.1);
    register(&mut bridge, first, "fixture.outer", 1);
    register(&mut bridge, first, "fixture.inner", 2);
    register(&mut bridge, second, "fixture.other", 3);
    let root = enqueue_registered_root(
        &mut bridge,
        command("fixture.outer"),
        Some(BufferHandle::new(4)),
    );
    bridge.drain_events().for_each(drop);

    let inline = request(
        first,
        4,
        Some(root.id),
        operation::<declared::invoke_command>(declared::InvokeArguments {
            command: command("fixture.inner"),
        }),
    );
    let (response, child, _pending_reply) = routed(&mut bridge, &inline);
    let child = child.unwrap();
    assert!(
        matches!(response.unwrap(), Ok(CommandInvokeDispatch::Inline { invocation, .. }) if invocation == child)
    );
    let duplicate = request(
        first,
        5,
        Some(root.id),
        operation::<declared::invoke_command>(declared::InvokeArguments {
            command: command("fixture.inner"),
        }),
    );
    assert!(matches!(
        routed(&mut bridge, &duplicate).0.unwrap(),
        Ok(CommandInvokeDispatch::Outcome {
            outcome: CommandOutcome::Unavailable
        })
    ));
    let recursive = request(
        first,
        6,
        Some(child),
        operation::<declared::invoke_command>(declared::InvokeArguments {
            command: command("fixture.outer"),
        }),
    );
    assert!(matches!(
        routed(&mut bridge, &recursive).0.unwrap(),
        Ok(CommandInvokeDispatch::Outcome {
            outcome: CommandOutcome::Unavailable
        })
    ));
    bridge.complete(child, CommandOutcome::Completed);
    bridge.complete(root.id, CommandOutcome::Completed);

    let root = enqueue_registered_root(
        &mut bridge,
        command("fixture.outer"),
        Some(BufferHandle::new(7)),
    );
    bridge.drain_events().for_each(drop);
    let cross = request(
        first,
        7,
        Some(root.id),
        operation::<declared::invoke_command>(declared::InvokeArguments {
            command: command("fixture.other"),
        }),
    );
    let (response, child, mut pending_reply) = routed(&mut bridge, &cross);
    assert!(response.is_none());
    let child = child.unwrap();
    assert!(
        matches!(bridge.drain_events().next(), Some(ExtensionCommandEvent::DispatchExtension {
        invocation, buffer: Some(buffer), ..
    }) if invocation.id == child && buffer == BufferHandle::new(7))
    );
    let cycle = request(
        second,
        8,
        Some(child),
        operation::<declared::invoke_command>(declared::InvokeArguments {
            command: command("fixture.outer"),
        }),
    );
    assert!(matches!(
        routed(&mut bridge, &cycle).0.unwrap(),
        Ok(CommandInvokeDispatch::Outcome {
            outcome: CommandOutcome::Unavailable
        })
    ));
    bridge.complete(child, CommandOutcome::Completed);
    assert_eq!(
        pending_reply.try_recv().unwrap(),
        Ok(CommandInvokeDispatch::Outcome {
            outcome: CommandOutcome::Completed
        })
    );
}

#[test]
fn window_child_settles_after_routed_dispatch() {
    let mut bridge = ExtensionCommandBridge::new();
    let owner = key(1);
    bridge.admit_lifecycle(owner.0, owner.1);
    register(&mut bridge, owner, "fixture.parent", 1);
    let root = enqueue_registered_root(&mut bridge, command("fixture.parent"), None);
    bridge.drain_events().for_each(drop);
    let nested = request(
        owner,
        2,
        Some(root.id),
        operation::<declared::invoke_command>(declared::InvokeArguments {
            command: command("file.new"),
        }),
    );
    let (send, mut pending_reply) = tokio::sync::oneshot::channel();
    let command = nested
        .operation
        .arguments::<declared::invoke_command>()
        .unwrap()
        .command
        .clone();
    let (response, child) = bridge.handle_routed_child(
        &nested.context(),
        command,
        RoutedCommandTarget::Window,
        &mut Some(send),
    );
    assert!(response.is_none());
    let child = child.unwrap();
    assert!(bridge.drain_events().next().is_none());
    bridge.complete(child, CommandOutcome::Completed);
    assert_eq!(
        pending_reply.try_recv().unwrap(),
        Ok(CommandInvokeDispatch::Outcome {
            outcome: CommandOutcome::Completed
        })
    );
}

#[test]
fn cancellation_and_lifecycle_cleanup_settle_once_and_release_the_queue() {
    let mut bridge = ExtensionCommandBridge::new();
    let owner = key(1);
    bridge.admit_lifecycle(owner.0, owner.1);
    register(&mut bridge, owner, "fixture.command", 1);
    let mut first = enqueue_registered_root(&mut bridge, command("fixture.command"), None);
    let mut second = enqueue_registered_root(&mut bridge, command("fixture.command"), None);
    bridge.drain_events().for_each(drop);
    bridge.cancel(first.id);
    assert!(
        matches!(bridge.drain_events().next(), Some(ExtensionCommandEvent::CancelExtension {
        invocation, ..
    }) if invocation == first.id)
    );
    bridge.complete(first.id, CommandOutcome::Completed);
    bridge.complete(
        first.id,
        CommandOutcome::HandlerFailure {
            message: "late".into(),
        },
    );
    assert!(
        matches!(bridge.drain_events().next(), Some(ExtensionCommandEvent::DispatchExtension {
        invocation, ..
    }) if invocation.id == second.id)
    );
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
fn context_authority_is_owned_by_the_live_invocation_tree() {
    let mut bridge = ExtensionCommandBridge::new();
    let first = key(1);
    let second = key(2);
    bridge.admit_lifecycle(first.0, first.1);
    bridge.admit_lifecycle(second.0, second.1);
    register(&mut bridge, first, "fixture.outer", 1);
    register(&mut bridge, first, "fixture.inner", 2);
    register(&mut bridge, second, "fixture.other", 3);
    let root = enqueue_registered_root(
        &mut bridge,
        command("fixture.outer"),
        Some(BufferHandle::new(4)),
    );
    let contextual = request(
        first,
        4,
        Some(root.id),
        operation::<declared::active_buffer>(declared::ContextArguments { target: None }),
    );
    assert_eq!(
        bridge.authorize_context(&contextual.context(), None),
        Ok(root.id)
    );
    assert_eq!(
        bridge.authorize_context(&contextual.context(), Some(root.id)),
        Ok(root.id)
    );
    assert_eq!(
        bridge.authorize_context(&contextual.context(), Some(CommandInvocationId::new(999))),
        Err(AccessError::InvalidTarget)
    );
    let foreign = request(
        second,
        5,
        Some(root.id),
        operation::<declared::active_buffer>(declared::ContextArguments { target: None }),
    );
    assert_eq!(
        bridge.authorize_context(&foreign.context(), None),
        Err(AccessError::InvalidTarget)
    );
    let unrelated = request(
        first,
        6,
        None,
        operation::<declared::active_buffer>(declared::ContextArguments { target: None }),
    );
    assert_eq!(
        bridge.authorize_context(&unrelated.context(), None),
        Err(AccessError::ContextUnavailable)
    );
    assert_eq!(
        bridge.authorize_context(&unrelated.context(), Some(root.id)),
        Err(AccessError::InvalidTarget)
    );
    for (owner, name) in [(first, "fixture.inner"), (second, "fixture.other")] {
        let nested = request(
            first,
            7,
            Some(root.id),
            operation::<declared::invoke_command>(declared::InvokeArguments {
                command: command(name),
            }),
        );
        let (_, child, _pending_reply) = routed(&mut bridge, &nested);
        let child = child.unwrap();
        let contextual = request(
            owner,
            8,
            Some(child),
            operation::<declared::active_buffer>(declared::ContextArguments {
                target: Some(root.id),
            }),
        );
        assert_eq!(
            bridge.authorize_context(&contextual.context(), Some(root.id)),
            Ok(root.id)
        );
        assert_eq!(bridge.captured_buffer(child), Some(BufferHandle::new(4)));
        bridge.complete(child, CommandOutcome::Completed);
    }
    bridge.cancel(root.id);
    assert_eq!(
        bridge.authorize_context(&contextual.context(), None),
        Err(AccessError::Cancelled)
    );
    bridge.complete(root.id, CommandOutcome::Cancelled);
    assert_eq!(
        bridge.authorize_context(&contextual.context(), Some(root.id)),
        Err(AccessError::InvalidTarget)
    );
}

#[test]
fn direct_invocation_requires_command_context() {
    let mut bridge = ExtensionCommandBridge::new();
    let caller = key(1);
    bridge.admit_lifecycle(caller.0, caller.1);
    let request = RequestContext {
        extension: caller.0,
        lifecycle: caller.1,
        id: RequestId::new(1),
        invocation: None,
    };
    let result = bridge.handle_routed_child(
        &request,
        command("file.new"),
        RoutedCommandTarget::Window,
        &mut None,
    );
    assert_eq!(result.0.unwrap(), Err(AccessError::ContextUnavailable));
}
