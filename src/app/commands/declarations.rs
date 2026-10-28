//! Automatically linked native commands and owned foreground command access.
//!
//! Annotate a safe async function taking owned `CommandContext` and arguments and
//! returning `Result<Success, Error>`. The `command` attribute requires a wire
//! name, `title`, generated name constant `id`, and routing `participant`:
//! `#[command("file.open", title = "Open…", id = OPEN_COMMAND, participant = Workbench)]`.
//! Repeated attributes share an implementation; `context.name` identifies its
//! declaration. An optional `claim = path` predicate returns false to decline
//! synchronously, preserving routing before any handler work starts.
//!
//! Arguments implement `CommandArguments`; success and error types implement
//! `CommandSuccess` and `CommandError`. Unit arguments reject non-null input;
//! unit success means Completed. Existing native flows can return CommandOutcome.
//! Use foreground/shell closures for model access and pending for an owned native
//! completion channel. Await these methods; closure captures and results must be
//! owned. Each foreground entry revalidates the captured presentation.

use std::collections::{BTreeMap, VecDeque};
use std::sync::OnceLock;
use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context as TaskContext, Poll, Waker},
};

use crate::app::commands::dispatch::{
    ApplicationProductCommands, CommandClaim, ProductCommandTarget,
};
use crate::app::product::ProductShell;
use crate::host::declarations::{AsyncHandler, HandlerFuture};
use crate::host::protocol::{Command, CommandArgumentValue, CommandOutcome};
use gpui::{App, AppContext, Context, Window};

/// Declares name/title, the routing participant, and a generated name constant.
/// Repeated attributes share a typed async implementation. Optional claim
/// predicates run during routing, before a native future is started.
pub(crate) use knot_handler_macros::command;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Participant {
    Editor,
    Terminal,
    WorkspaceTree,
    Workbench,
    Application,
}

pub(crate) trait CommandArguments: Sized + 'static {
    fn decode(value: &CommandArgumentValue) -> Result<Self, CommandOutcome>;
}

impl CommandArguments for () {
    fn decode(value: &CommandArgumentValue) -> Result<Self, CommandOutcome> {
        if *value == CommandArgumentValue::Null {
            Ok(())
        } else {
            Err(CommandOutcome::InvalidArgument {
                message: "product commands do not accept arguments".into(),
            })
        }
    }
}

pub(crate) trait CommandSuccess {
    fn into_outcome(self) -> CommandOutcome;
}
impl CommandSuccess for () {
    fn into_outcome(self) -> CommandOutcome {
        CommandOutcome::Completed
    }
}
impl CommandSuccess for CommandOutcome {
    fn into_outcome(self) -> CommandOutcome {
        self
    }
}
pub(crate) trait CommandError {
    fn into_outcome(self) -> CommandOutcome;
}
impl CommandError for CommandOutcome {
    fn into_outcome(self) -> CommandOutcome {
        self
    }
}

type ForegroundWork = Box<dyn FnOnce(&mut App)>;
type ForegroundQueue = Rc<RefCell<VecDeque<ForegroundWork>>>;

/// No foreground borrow is retained by a command future, including across native dialogs.
#[derive(Clone)]
pub(crate) struct CommandContext {
    pub(crate) name: &'static str,
    target: ProductCommandTarget,
    foreground: ForegroundQueue,
}

impl CommandContext {
    async fn application<R: 'static>(
        &self,
        access: impl FnOnce(&mut App) -> R + 'static,
    ) -> Result<R, CommandOutcome> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.foreground.borrow_mut().push_back(Box::new(move |cx| {
            let _ = sender.send(access(cx));
        }));
        receiver.await.map_err(|_| CommandOutcome::Cancelled)
    }

    pub(crate) async fn foreground<R: 'static>(
        &self,
        access: impl FnOnce(&ProductCommandTarget, &mut Window, &mut App) -> Result<R, CommandOutcome>
        + 'static,
    ) -> Result<R, CommandOutcome> {
        let target = self.target.clone();
        self.application(move |cx| {
            cx.update_window(target.window, |_, window, cx| {
                let shell = target
                    .shell
                    .upgrade()
                    .ok_or(CommandOutcome::InvalidTarget)?;
                if window.root::<ProductShell>().flatten().as_ref() != Some(&shell) {
                    return Err(CommandOutcome::InvalidTarget);
                }
                target.validate_tab(cx)?;
                target.validate_view(cx)?;
                access(&target, window, cx)
            })
            .unwrap_or(Err(CommandOutcome::InvalidTarget))
        })
        .await?
    }

    pub(crate) async fn shell<R: 'static>(
        &self,
        access: impl FnOnce(
            &mut ProductShell,
            &ProductCommandTarget,
            &mut Window,
            &mut Context<ProductShell>,
        ) -> Result<R, CommandOutcome>
        + 'static,
    ) -> Result<R, CommandOutcome> {
        self.foreground(move |target, window, cx| {
            let shell = target
                .shell
                .upgrade()
                .ok_or(CommandOutcome::InvalidTarget)?;
            shell.update(cx, |shell, cx| access(shell, target, window, cx))
        })
        .await
    }

    pub(crate) async fn pending(
        &self,
        start: impl FnOnce(
            &ProductCommandTarget,
            crate::app::CommandCompletion,
            &mut App,
        ) -> Result<(), CommandOutcome>
        + 'static,
    ) -> Result<CommandOutcome, CommandOutcome> {
        let (completion, receiver) = crate::app::CommandCompletion::new();
        let target = self.target.clone();
        self.application(move |cx| start(&target, completion, cx))
            .await??;
        Ok(receiver.await.unwrap_or(CommandOutcome::Cancelled))
    }
}

/// Drive foreground requests before returning to input dispatch. Only genuinely
/// suspended work moves to the executor; the future never receives a model borrow.
fn poll_command<T>(
    future: Pin<&mut dyn Future<Output = T>>,
    queue: &ForegroundQueue,
    task: &mut TaskContext<'_>,
    cx: &mut App,
) -> Poll<T> {
    let mut future = future;
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(task) {
            return Poll::Ready(result);
        }
        let mut worked = false;
        loop {
            let work = queue.borrow_mut().pop_front();
            let Some(work) = work else {
                break;
            };
            worked = true;
            work(cx);
        }
        if !worked {
            return Poll::Pending;
        }
    }
}

fn complete(completion: &crate::app::CommandCompletion, outcome: CommandOutcome, cx: &mut App) {
    completion.complete(outcome.clone());
    if let Some(commands) = cx.try_global::<ApplicationProductCommands>() {
        let commands = commands.0.clone();
        commands.update(cx, |dispatcher, cx| dispatcher.record_outcome(outcome, cx));
    }
}

pub(crate) fn always_claim(_: &Command, _: &ProductCommandTarget, _: &App) -> bool {
    true
}
type Claim = fn(&Command, &ProductCommandTarget, &App) -> bool;
type Invoke = fn(
    CommandArgumentValue,
    CommandContext,
) -> Result<HandlerFuture<CommandOutcome>, CommandOutcome>;

pub(crate) struct CommandDescriptor {
    pub(crate) name: &'static str,
    pub(crate) title: &'static str,
    pub(crate) participant: Participant,
    claim: Claim,
    invoke: Invoke,
}
inventory::collect!(CommandDescriptor);

impl CommandDescriptor {
    pub(crate) const fn new<H>(title: &'static str, participant: Participant, claim: Claim) -> Self
    where
        H: AsyncHandler<
                Context = CommandContext,
                Arguments: CommandArguments,
                Success: CommandSuccess,
                Error: CommandError,
            >,
    {
        Self {
            name: H::NAME,
            title,
            participant,
            claim,
            invoke: |arguments, context| {
                let arguments = H::Arguments::decode(&arguments)?;
                Ok(Box::pin(async move {
                    match H::call(context, arguments).await {
                        Ok(success) => success.into_outcome(),
                        Err(error) => error.into_outcome(),
                    }
                }))
            },
        }
    }

    fn dispatch_with_completion(
        &'static self,
        command: &Command,
        target: &ProductCommandTarget,
        completion: crate::app::CommandCompletion,
        cx: &App,
    ) -> CommandClaim {
        if !(self.claim)(command, target, cx) {
            return CommandClaim::Declined;
        }
        let foreground = ForegroundQueue::default();
        let context = CommandContext {
            name: self.name,
            target: target.clone(),
            foreground: foreground.clone(),
        };
        let mut future = match (self.invoke)(command.arguments.clone(), context) {
            Ok(future) => future,
            Err(error) => return CommandClaim::Finished(error),
        };
        CommandClaim::Pending(Box::new(move |cx| {
            let mut task = TaskContext::from_waker(Waker::noop());
            if let Poll::Ready(outcome) = poll_command(future.as_mut(), &foreground, &mut task, cx)
            {
                complete(&completion, outcome, cx);
                return Ok(());
            }
            cx.spawn(async move |cx| {
                let outcome = std::future::poll_fn(|task| {
                    cx.update(|cx| poll_command(future.as_mut(), &foreground, task, cx))
                        .unwrap_or(Poll::Ready(CommandOutcome::Cancelled))
                })
                .await;
                let _ = cx.update(|cx| complete(&completion, outcome, cx));
            })
            .detach();
            Ok(())
        }))
    }
}

pub(crate) struct CommandRegistry {
    entries: BTreeMap<(&'static str, Participant), &'static CommandDescriptor>,
    definitions: BTreeMap<&'static str, &'static str>,
}

impl CommandRegistry {
    fn assemble(
        descriptors: impl IntoIterator<Item = &'static CommandDescriptor>,
    ) -> Result<Self, String> {
        let mut descriptors = descriptors.into_iter().collect::<Vec<_>>();
        descriptors.sort_by_key(|entry| (entry.name, entry.participant, entry.title));
        let mut registry = Self {
            entries: BTreeMap::new(),
            definitions: BTreeMap::new(),
        };
        for descriptor in descriptors {
            if descriptor.name.is_empty() || descriptor.title.is_empty() {
                return Err("native commands require nonempty names and titles".into());
            }
            if registry
                .entries
                .insert((descriptor.name, descriptor.participant), descriptor)
                .is_some()
            {
                return Err(format!(
                    "duplicate native command {:?} at {:?}",
                    descriptor.name, descriptor.participant
                ));
            }
            if let Some(title) = registry
                .definitions
                .insert(descriptor.name, descriptor.title)
                && title != descriptor.title
            {
                return Err(format!(
                    "conflicting titles for native command {:?}: {title:?} and {:?}",
                    descriptor.name, descriptor.title
                ));
            }
        }
        Ok(registry)
    }

    pub(crate) fn definitions(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.definitions.iter().map(|(name, title)| (*name, *title))
    }

    pub(crate) fn dispatch(
        &self,
        participant: Participant,
        command: &Command,
        target: &ProductCommandTarget,
        completion: crate::app::CommandCompletion,
        cx: &App,
    ) -> CommandClaim {
        let Some(descriptor) = self.entries.get(&(command.name.as_ref(), participant)) else {
            return CommandClaim::Declined;
        };
        descriptor.dispatch_with_completion(command, target, completion, cx)
    }
}

pub(crate) fn registry() -> &'static CommandRegistry {
    static REGISTRY: OnceLock<CommandRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        CommandRegistry::assemble(inventory::iter::<CommandDescriptor>)
            .unwrap_or_else(|message| panic!("{message}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    struct Fixture;
    impl AsyncHandler for Fixture {
        type Context = CommandContext;
        type Arguments = ();
        type Success = ();
        type Error = CommandOutcome;
        const NAME: &'static str = "fixture.command";
        fn call(_: CommandContext, (): ()) -> HandlerFuture<Result<(), CommandOutcome>> {
            Box::pin(async { Ok(()) })
        }
    }
    static EDITOR: CommandDescriptor =
        CommandDescriptor::new::<Fixture>("Fixture", Participant::Editor, always_claim);
    static WORKBENCH: CommandDescriptor =
        CommandDescriptor::new::<Fixture>("Fixture", Participant::Workbench, always_claim);
    static CONFLICT: CommandDescriptor =
        CommandDescriptor::new::<Fixture>("Other title", Participant::Application, always_claim);

    #[test]
    fn discovery_combines_participants_and_rejects_conflicts_deterministically() {
        for descriptors in [[&EDITOR, &WORKBENCH], [&WORKBENCH, &EDITOR]] {
            let registry = CommandRegistry::assemble(descriptors).unwrap();
            assert_eq!(
                registry.definitions().collect::<Vec<_>>(),
                [("fixture.command", "Fixture")]
            );
            assert_eq!(registry.entries.len(), 2);
        }
        assert_eq!(
            CommandRegistry::assemble([&EDITOR, &EDITOR]).err().unwrap(),
            "duplicate native command \"fixture.command\" at Editor"
        );
        let conflict = CommandRegistry::assemble([&EDITOR, &CONFLICT])
            .err()
            .unwrap();
        assert_eq!(
            conflict,
            CommandRegistry::assemble([&CONFLICT, &EDITOR])
                .err()
                .unwrap()
        );
        assert_eq!(
            conflict,
            "conflicting titles for native command \"fixture.command\": \"Fixture\" and \"Other title\""
        );
    }

    #[test]
    fn linked_declarations_share_copy_and_paste_metadata_without_duplicate_definitions() {
        let registry = registry();
        for name in [
            crate::app::commands::dispatch::COPY_COMMAND,
            crate::app::commands::dispatch::PASTE_COMMAND,
        ] {
            assert!(registry.entries.contains_key(&(name, Participant::Editor)));
            assert!(
                registry
                    .entries
                    .contains_key(&(name, Participant::Terminal))
            );
            assert_eq!(
                registry
                    .definitions()
                    .filter(|(definition, _)| *definition == name)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn argument_and_outcome_adapters_preserve_structured_failures() {
        assert_eq!(
            <() as CommandArguments>::decode(&CommandArgumentValue::Null),
            Ok(())
        );
        assert_eq!(
            <() as CommandArguments>::decode(&CommandArgumentValue::Boolean(true)),
            Err(CommandOutcome::InvalidArgument {
                message: "product commands do not accept arguments".into()
            })
        );
        assert_eq!(CommandSuccess::into_outcome(()), CommandOutcome::Completed);
        struct Failure;
        impl CommandError for Failure {
            fn into_outcome(self) -> CommandOutcome {
                CommandOutcome::HandlerFailure {
                    message: "typed failure".into(),
                }
            }
        }
        assert_eq!(
            Failure.into_outcome(),
            CommandOutcome::HandlerFailure {
                message: "typed failure".into()
            }
        );
        assert_eq!(
            CommandSuccess::into_outcome(CommandOutcome::Cancelled),
            CommandOutcome::Cancelled
        );
    }

    #[gpui::test]
    fn foreground_work_finishes_in_turn_and_resumes_after_real_suspension(cx: &mut TestAppContext) {
        use std::cell::Cell;
        let queue = ForegroundQueue::default();
        let performed = Rc::new(Cell::new(0));
        let (release, suspended) = tokio::sync::oneshot::channel::<()>();
        let future_queue = queue.clone();
        let result = performed.clone();
        let mut future: HandlerFuture<usize> = Box::pin(async move {
            let (reply, receive) = tokio::sync::oneshot::channel();
            let result_before = result.clone();
            future_queue.borrow_mut().push_back(Box::new(move |_| {
                result_before.set(1);
                let _ = reply.send(());
            }));
            receive.await.unwrap();
            suspended.await.unwrap();
            let (reply, receive) = tokio::sync::oneshot::channel();
            future_queue.borrow_mut().push_back(Box::new(move |_| {
                result.set(2);
                let _ = reply.send(42);
            }));
            receive.await.unwrap()
        });
        cx.update(|cx| {
            let mut task = TaskContext::from_waker(Waker::noop());
            assert!(poll_command(future.as_mut(), &queue, &mut task, cx).is_pending());
            assert_eq!(performed.get(), 1);
            release.send(()).unwrap();
            assert_eq!(
                poll_command(future.as_mut(), &queue, &mut task, cx),
                Poll::Ready(42)
            );
            assert_eq!(performed.get(), 2);
        });
    }
}
