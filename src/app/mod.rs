//! Knot's native application shell.

use std::sync::{Arc, Mutex};

use gpui::*;

#[cfg(test)]
use crate::host::protocol::BufferHandle;
use crate::host::protocol::CommandOutcome;

mod command_palette;
mod completion;
mod documents;
mod editor;
mod entry;
mod filesystem;
mod history;
pub mod model;
mod open;
mod product;
mod product_commands;
mod resource;
mod search_results;
mod terminal_view;
mod tree_view;
mod workbench;
mod workspace;
mod workspace_tree;

use command_palette::{CommandPalette, CommandPaletteEntry, CommandPaletteEvent};

actions!(
    knot,
    [
        CompletionPrevious,
        CompletionNext,
        CompletionAccept,
        CompletionDismiss
    ]
);

const DEFAULT_FIXTURE_NAME: &str = "rust_sample";
const EDITOR_KEY_CONTEXT: &str = "editor";
const TREE_KEY_CONTEXT: &str = "tree";
const TERMINAL_KEY_CONTEXT: &str = "terminal";
const PALETTE_KEY_CONTEXT: &str = "palette";

#[cfg(test)]
#[derive(Clone, Eq, PartialEq)]
struct CommandOrigin {
    window: AnyWindowHandle,
    focus: WeakFocusHandle,
    buffer: Option<BufferHandle>,
}

struct CommandCompletionState {
    sender: Option<tokio::sync::oneshot::Sender<CommandOutcome>>,
}

#[derive(Clone)]
struct CommandCompletion {
    state: Arc<Mutex<CommandCompletionState>>,
}

impl PartialEq for CommandCompletion {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl CommandCompletion {
    fn new() -> (Self, tokio::sync::oneshot::Receiver<CommandOutcome>) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        (
            Self {
                state: Arc::new(Mutex::new(CommandCompletionState {
                    sender: Some(sender),
                })),
            },
            receiver,
        )
    }

    fn complete(&self, outcome: CommandOutcome) {
        let sender = self
            .state
            .lock()
            .expect("command completion lock poisoned")
            .sender
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(outcome);
        }
    }
}

#[allow(
    dead_code,
    reason = "command sources may observe the identity and await the structured result"
)]
struct CommandExecution {
    id: crate::host::protocol::CommandInvocationId,
    completion: tokio::sync::oneshot::Receiver<CommandOutcome>,
}

pub fn run() {
    let launch = entry::LaunchConfiguration::parse(
        std::env::args(),
        &std::env::current_dir().expect("Knot requires a current working directory"),
    )
    .unwrap_or_else(|error| {
        eprintln!("[knot] {error}");
        std::process::exit(2);
    });
    match launch {
        entry::LaunchConfiguration::Product(request) => product::run(request),
        entry::LaunchConfiguration::Fixture(fixture) => {
            eprintln!(
                "[knot] fixture {fixture:?} is unavailable while the extension host is rebuilt"
            );
            product::run(None);
        }
    }
}
