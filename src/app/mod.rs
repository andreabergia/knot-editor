//! Knot's native application shell.

use std::sync::{Arc, Mutex};

#[cfg(test)]
use crate::host::protocol::BufferHandle;
use crate::host::protocol::CommandOutcome;

use gpui::*;

mod application_observation;
mod bundled;
mod command_declarations;
mod command_palette;
mod completion;
mod documents;
mod editor;
mod entry;
mod extension_buffers;
mod extension_commands;
mod extension_host;
#[cfg(test)]
pub(crate) use extension_host::handlers as declared_host;
mod extension_load;
pub(crate) mod extension_package;
mod extension_semantics;
mod extension_storage;
mod filesystem;
mod history;
mod keymaps;
pub mod model;
mod open;
mod personal_config;
mod personal_config_error;
mod product;
mod product_commands;
mod record_store;
mod recovery;
mod resource;
mod terminal;
mod text_input;
mod text_view;
pub(crate) mod tool_layout;
mod transient;
mod tree_view;
mod typography;
mod window_layout;
mod window_sessions;
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

const EDITOR_KEY_CONTEXT: &str = "editor";
const TREE_KEY_CONTEXT: &str = "tree";
const TERMINAL_KEY_CONTEXT: &str = "terminal";
const PALETTE_KEY_CONTEXT: &str = "palette";
const TEXT_INPUT_KEY_CONTEXT: &str = "text_input";
const TRANSIENT_KEY_CONTEXT: &str = "transient";

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

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "product callers discard it; tests await the outcome"
    )
)]
struct CommandExecution {
    id: crate::host::protocol::CommandInvocationId,
    completion: tokio::sync::oneshot::Receiver<CommandOutcome>,
}

pub fn run() {
    let request = entry::parse_launch_arguments(
        std::env::args(),
        &std::env::current_dir().expect("Knot requires a current working directory"),
    )
    .unwrap_or_else(|error| {
        eprintln!("[knot] {error}");
        std::process::exit(2);
    });
    product::run(request);
}
