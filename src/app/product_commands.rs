//! Application-owned command discovery and dispatch for product windows.

use gpui::*;
use std::time::Duration;

#[cfg(test)]
use crate::host::ExtensionRuntimeControl;
use crate::host::protocol::{
    Command, CommandArgumentValue, CommandInvokeDispatch, CommandOutcome, ExtensionId,
    HostOperation, HostRequestError, HostResponse, HostResponseValue,
};
use crate::host::{ExtensionRuntimeThread, V8Host};

use super::model::{CommandCatalog, CommandDefinition, CommandTargetKind};
use super::product::ProductShell;
use super::{CommandCompletion, CommandExecution};

pub(crate) const NEW_COMMAND: &str = "file.new";
pub(crate) const OPEN_COMMAND: &str = "file.open";
pub(crate) const SAVE_COMMAND: &str = "file.save";
pub(crate) const SAVE_AS_COMMAND: &str = "file.save-as";
pub(crate) const CLOSE_TAB_COMMAND: &str = "workbench.close-tab";
pub(crate) const CLOSE_WINDOW_COMMAND: &str = "window.close";
pub(crate) const NEW_WINDOW_COMMAND: &str = "window.new";
pub(crate) const QUIT_COMMAND: &str = "application.quit";
pub(crate) const SPLIT_HORIZONTAL_COMMAND: &str = "workbench.split-horizontal";
pub(crate) const SPLIT_VERTICAL_COMMAND: &str = "workbench.split-vertical";
pub(crate) const UNDO_COMMAND: &str = "editor.undo";
pub(crate) const REDO_COMMAND: &str = "editor.redo";
pub(crate) const CUT_COMMAND: &str = "editor.cut";
pub(crate) const COPY_COMMAND: &str = "editor.copy";
pub(crate) const PASTE_COMMAND: &str = "editor.paste";
pub(crate) const SELECT_ALL_COMMAND: &str = "editor.select-all";
pub(crate) const FIND_COMMAND: &str = "editor.find";
pub(crate) const FIND_NEXT_COMMAND: &str = "editor.find-next";
pub(crate) const FIND_PREVIOUS_COMMAND: &str = "editor.find-previous";

const PRODUCT_COMMANDS: [(&str, &str); 19] = [
    (NEW_COMMAND, "New Document"),
    (OPEN_COMMAND, "Open…"),
    (SAVE_COMMAND, "Save"),
    (SAVE_AS_COMMAND, "Save As…"),
    (CLOSE_TAB_COMMAND, "Close Tab"),
    (CLOSE_WINDOW_COMMAND, "Close Window"),
    (NEW_WINDOW_COMMAND, "New Window"),
    (QUIT_COMMAND, "Quit Knot"),
    (SPLIT_HORIZONTAL_COMMAND, "Split Right"),
    (SPLIT_VERTICAL_COMMAND, "Split Down"),
    (UNDO_COMMAND, "Undo"),
    (REDO_COMMAND, "Redo"),
    (CUT_COMMAND, "Cut"),
    (COPY_COMMAND, "Copy"),
    (PASTE_COMMAND, "Paste"),
    (SELECT_ALL_COMMAND, "Select All"),
    (FIND_COMMAND, "Find"),
    (FIND_NEXT_COMMAND, "Find Next"),
    (FIND_PREVIOUS_COMMAND, "Find Previous"),
];

#[derive(Clone, PartialEq, Action)]
#[action(namespace = knot, no_json)]
pub(crate) struct ProductCommandSource {
    pub(crate) name: SharedString,
}

impl ProductCommandSource {
    pub(crate) fn new(name: &'static str) -> Self {
        Self { name: name.into() }
    }

    pub(crate) fn command(&self) -> Command {
        Command {
            name: self.name.as_ref().into(),
            arguments: CommandArgumentValue::Null,
        }
    }
}

#[derive(Clone, PartialEq, Action)]
#[action(namespace = knot, no_json)]
pub(crate) struct ShowProductCommandPalette;

#[derive(Clone, PartialEq)]
pub(crate) struct ProductCommandTarget {
    pub(crate) window: AnyWindowHandle,
    pub(crate) shell: WeakEntity<ProductShell>,
    pub(crate) workbench: WeakEntity<super::workbench::Workbench>,
    pub(crate) pane: super::workbench::PaneId,
    pub(crate) tab: super::workbench::TabId,
    pub(crate) document: super::documents::DocumentId,
    pub(crate) focus: WeakFocusHandle,
}

pub(crate) struct ApplicationProductCommands(pub(crate) Entity<ProductCommandDispatcher>);

impl Global for ApplicationProductCommands {}

pub(crate) struct ProductCommandDispatcher {
    catalog: CommandCatalog,
    next_invocation: u64,
    last_outcome: Option<CommandOutcome>,
    #[cfg(test)]
    runtime_control: ExtensionRuntimeControl,
    _runtime_thread: ExtensionRuntimeThread,
    _runtime_bridge: Task<()>,
    _runtime_wake: Task<()>,
}

impl ProductCommandDispatcher {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let mut catalog = CommandCatalog::new();
        for (name, title) in PRODUCT_COMMANDS {
            catalog
                .register_native(name.into(), title.into())
                .expect("product command names are unique");
        }

        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(1))
            .into_parts();
        #[cfg(test)]
        let runtime_control = runtime.control.clone();
        let response_control = runtime.control.clone();
        let mut requests = runtime.requests;
        let runtime_bridge = cx.spawn(async move |this, cx| {
            while let Some(request) = requests.receive().await {
                let extension = request.extension;
                let lifecycle = request.lifecycle;
                let id = request.id;
                let result = match request.operation {
                    HostOperation::InvokeCommand { command } if request.invocation.is_none() => {
                        let execution = cx
                            .update(|cx| {
                                let dispatcher = this.upgrade()?;
                                let target = capture_active_product_target(cx)?;
                                Some(dispatcher.update(cx, |dispatcher, cx| {
                                    dispatcher.dispatch(command, target, cx)
                                }))
                            })
                            .ok()
                            .flatten();
                        let outcome = match execution {
                            Some(execution) => execution
                                .completion
                                .await
                                .unwrap_or(CommandOutcome::Cancelled),
                            None => CommandOutcome::InvalidTarget,
                        };
                        Ok(HostResponseValue::CommandInvoked {
                            dispatch: CommandInvokeDispatch::Outcome { outcome },
                        })
                    }
                    HostOperation::InvokeCommand { .. } => Ok(HostResponseValue::CommandInvoked {
                        dispatch: CommandInvokeDispatch::Outcome {
                            outcome: CommandOutcome::Unavailable,
                        },
                    }),
                    _ => Err(HostRequestError::UnsupportedOperation),
                };
                if response_control
                    .respond(HostResponse {
                        extension,
                        lifecycle,
                        id,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let runtime_wake = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if this.upgrade().is_none() {
                    break;
                }
            }
        });

        Self {
            catalog,
            next_invocation: 1,
            last_outcome: None,
            #[cfg(test)]
            runtime_control,
            _runtime_thread: runtime.thread,
            _runtime_bridge: runtime_bridge,
            _runtime_wake: runtime_wake,
        }
    }

    pub(crate) fn definitions(&self) -> impl Iterator<Item = &CommandDefinition> {
        self.catalog.definitions()
    }

    pub(crate) fn dispatch(
        &mut self,
        command: Command,
        target: ProductCommandTarget,
        cx: &mut Context<Self>,
    ) -> CommandExecution {
        let id = crate::host::protocol::CommandInvocationId::new(self.next_invocation);
        self.next_invocation = self
            .next_invocation
            .checked_add(1)
            .expect("product command invocation space exhausted");
        let (completion, receiver) = CommandCompletion::new();
        let execution = CommandExecution {
            id,
            completion: receiver,
        };

        let admitted = self.catalog.resolve(command.name.as_ref()) == Ok(CommandTargetKind::Native);
        let dispatcher = cx.entity();
        cx.defer(move |cx| {
            let outcome = if !admitted {
                CommandOutcome::Unavailable
            } else if command.arguments != CommandArgumentValue::Null {
                CommandOutcome::InvalidArgument {
                    message: "product commands do not accept arguments".into(),
                }
            } else {
                dispatch_to_captured_target(&command, &target, cx)
            };
            completion.complete(outcome.clone());
            let _ = dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.last_outcome = Some(outcome);
                cx.notify();
            });
        });
        execution
    }

    #[cfg(test)]
    pub(crate) fn last_outcome(&self) -> Option<&CommandOutcome> {
        self.last_outcome.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn runtime_control(&self) -> ExtensionRuntimeControl {
        self.runtime_control.clone()
    }
}

fn dispatch_to_captured_target(
    command: &Command,
    target: &ProductCommandTarget,
    cx: &mut App,
) -> CommandOutcome {
    if target.shell.upgrade().is_none()
        || target.workbench.upgrade().is_none()
        || target.focus.upgrade().is_none()
        || !cx.windows().contains(&target.window)
    {
        return CommandOutcome::InvalidTarget;
    }
    let shell = target.shell.clone();
    let target = target.clone();
    cx.update_window(target.window, move |_, window, cx| {
        let Some(shell) = shell.upgrade() else {
            return CommandOutcome::InvalidTarget;
        };
        if window.root::<ProductShell>().flatten().as_ref() != Some(&shell) {
            return CommandOutcome::InvalidTarget;
        }
        shell.update(cx, |shell, cx| {
            shell.execute_product_command(command.name.as_ref(), &target, window, cx)
        })
    })
    .unwrap_or(CommandOutcome::InvalidTarget)
}

fn capture_active_product_target(cx: &mut App) -> Option<ProductCommandTarget> {
    let windows = cx.window_stack().unwrap_or_else(|| cx.windows());
    for window_handle in windows {
        let target = cx
            .update_window(window_handle, |_, window, cx| {
                let shell = window.root::<ProductShell>().flatten()?;
                shell.update(cx, |shell, cx| shell.capture_command_target(window, cx))
            })
            .ok()
            .flatten();
        if target.is_some() {
            return target;
        }
    }
    None
}

#[cfg(test)]
pub(crate) fn product_command_names() -> impl Iterator<Item = &'static str> {
    PRODUCT_COMMANDS.into_iter().map(|(name, _)| name)
}
