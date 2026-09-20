//! Knot's native application shell.

use std::sync::{Arc, Mutex};

use gpui::*;

#[cfg(test)]
use crate::host::protocol::BufferHandle;
use crate::host::protocol::CommandOutcome;

#[allow(
    dead_code,
    reason = "retained application boundary outside the current product surface"
)]
mod command_palette;
mod completion;
#[allow(
    dead_code,
    reason = "retained application boundary outside the current product surface"
)]
mod documents;
#[allow(
    dead_code,
    reason = "retained application boundary outside the current product surface"
)]
mod editor;
mod entry;
mod extension_buffers;
mod extension_commands;
mod extension_host;
mod extension_semantics;
#[allow(
    dead_code,
    reason = "retained application boundary outside the current product surface"
)]
mod filesystem;
mod history;
#[allow(
    dead_code,
    reason = "retained application boundary outside the current product surface"
)]
pub mod model;
mod open;
mod product;
mod product_commands;
#[allow(
    dead_code,
    reason = "retained application boundary during the pooled extension-host rebuild"
)]
mod resource;
#[allow(
    dead_code,
    reason = "retained native generated-text surface outside the current product shell"
)]
mod search_results;
#[allow(
    dead_code,
    reason = "retained native terminal surface outside the current product shell"
)]
mod terminal_view;
mod tree_view;
mod workbench;
#[allow(
    dead_code,
    reason = "retained application boundary outside the current product surface"
)]
mod workspace;
#[allow(
    dead_code,
    reason = "retained application boundary outside the current product surface"
)]
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
#[allow(dead_code, reason = "retained with the dormant extension tree view")]
const TREE_KEY_CONTEXT: &str = "tree";
#[allow(dead_code, reason = "retained with the dormant fixture terminal")]
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
        entry::LaunchConfiguration::Product(request) => product::run(request, None),
        entry::LaunchConfiguration::Fixture(fixture) => product::run(None, Some(fixture)),
    }
}
