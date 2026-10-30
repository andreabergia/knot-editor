//! Native Show Keybindings: list every key binding in the window's transient
//! picker, with the owner that made it and what it overrides, and run the
//! accepted binding's command.
//!
//! Each row is one keymap slot, so a key bound by the preset and again by an
//! extension appears twice: the winner, then the slot it overrides.

use super::*;
use crate::app::commands::keymaps::{
    ApplicationKeymapPresets, BindingOwner, ListedBinding, display_key,
};
use crate::host::protocol::PickerItem;

pub(super) const SHOW_KEYBINDINGS_TITLE: &str = "Show Keybindings";

impl ProductShell {
    /// List the keymap and dispatch the accepted row's command to `target`.
    pub(crate) fn start_show_keybindings(
        &mut self,
        target: ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let listing = cx
            .try_global::<ApplicationKeymaps>()
            .map(|keymaps| keymaps.0.listing())
            .unwrap_or_default();
        let titles = Self::command_dispatcher(cx)
            .read(cx)
            .definitions()
            .map(|definition| (definition.name.to_string(), definition.title))
            .collect::<HashMap<_, _>>();
        let items = listing
            .iter()
            .enumerate()
            .map(|(index, binding)| keybinding_item(index, binding, &titles, cx))
            .collect::<Vec<_>>();
        let Ok(receiver) =
            self.begin_owned_picker(None, SHOW_KEYBINDINGS_TITLE.into(), items, None, window, cx)
        else {
            cx.notify();
            return;
        };
        let task = cx.spawn(async move |this, cx| {
            let Ok(TransientOutcome::Picker(id)) = receiver.await else {
                return;
            };
            let Some(command) = id
                .parse::<usize>()
                .ok()
                .and_then(|index| listing.get(index))
                .and_then(|binding| binding.command.clone())
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.dispatch_command(command, target, cx));
        });
        self.tasks.push(task);
    }
}

fn keybinding_item(
    index: usize,
    binding: &ListedBinding,
    titles: &HashMap<String, String>,
    cx: &App,
) -> PickerItem {
    let label = match &binding.command {
        Some(command) => titles
            .get(command.name.as_ref())
            .cloned()
            .unwrap_or_else(|| command.name.to_string()),
        None => "Unbound".into(),
    };
    let mut description = format!("{} · {}", binding.key, owner_label(binding.owner, cx));
    if let Some(view) = &binding.view {
        description.push_str(&format!(" · in {view}"));
    }
    PickerItem {
        id: index.to_string(),
        label,
        description: Some(description),
        detail: binding
            .shadowed_by
            .map(|owner| format!("Overridden by {}", owner_label(owner, cx))),
        hint: Some(display_key(&binding.key)),
    }
}

fn owner_label(owner: BindingOwner, cx: &App) -> String {
    match owner {
        BindingOwner::Preset => {
            let title = cx
                .try_global::<ApplicationKeymapPresets>()
                .map(|presets| presets.0.applied_title())
                .unwrap_or_else(|| "Knot".into());
            format!("{title} preset")
        }
        BindingOwner::Extension(extension, lifecycle) => {
            crate::app::extensions::host::lifecycle_label(extension, lifecycle, cx)
                .unwrap_or_else(|| "extension".into())
        }
    }
}
