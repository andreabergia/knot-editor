//! The command palette: the catalog's commands presented in the shared
//! [`Picker`] and confirmed against the target captured when it opened.
//!
//! Each row shows a command's title, description, and the binding shown for
//! the view the palette opened from. The command name is matched but not
//! displayed, and commands that opt out of the palette are left out.

use std::collections::HashMap;

use gpui::{App, AppContext as _, Entity};

use crate::host::protocol::{Command, CommandArgumentValue, CommandName};

use super::{
    PALETTE_KEY_CONTEXT,
    commands::{catalog::CommandDefinition, keymaps::display_key},
    picker::{Picker, PickerEntry},
};

pub(crate) struct CommandPalette<Origin> {
    picker: Entity<Picker>,
    commands: Vec<Command>,
    origin: Origin,
}

impl<Origin: Clone> CommandPalette<Origin> {
    /// `bindings` maps command names to the canonical key sequence to show.
    pub(crate) fn new(
        definitions: impl IntoIterator<Item = CommandDefinition>,
        bindings: &HashMap<CommandName, String>,
        origin: Origin,
        cx: &mut App,
    ) -> Self {
        let mut definitions = definitions
            .into_iter()
            .filter(|definition| definition.palette)
            .collect::<Vec<_>>();
        definitions.sort_by(|left, right| left.title.cmp(&right.title));
        let entries = definitions
            .iter()
            .map(|definition| PickerEntry {
                description: definition.description.clone(),
                hint: bindings.get(&definition.name).map(|key| display_key(key)),
                match_text: Some(definition.name.to_string()),
                ..PickerEntry::new(definition.title.clone())
            })
            .collect();
        let commands = definitions
            .into_iter()
            .map(|definition| Command {
                name: definition.name,
                arguments: CommandArgumentValue::Null,
            })
            .collect();
        let picker = cx.new(|cx| Picker::new(None, entries, PALETTE_KEY_CONTEXT, cx));
        Self {
            picker,
            commands,
            origin,
        }
    }

    pub(crate) fn picker(&self) -> &Entity<Picker> {
        &self.picker
    }

    pub(crate) fn origin(&self) -> Origin {
        self.origin.clone()
    }

    /// The command of a confirmed picker entry.
    pub(crate) fn command(&self, entry: usize) -> Command {
        self.commands[entry].clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gpui::TestAppContext;

    use super::CommandPalette;
    use crate::app::commands::catalog::{CommandDefinition, CommandOwner};

    fn definition(name: &str, title: &str) -> CommandDefinition {
        CommandDefinition {
            name: name.into(),
            title: title.into(),
            description: None,
            palette: true,
            owner: CommandOwner::Native,
        }
    }

    #[gpui::test]
    fn lists_visible_commands_by_title_with_descriptions_and_bindings(cx: &mut TestAppContext) {
        let hidden = CommandDefinition {
            palette: false,
            ..definition("editor.move-left", "Move Left")
        };
        let described = CommandDefinition {
            description: Some("Close the focused window.".into()),
            ..definition("workspace.close", "Close Window")
        };
        let bindings = HashMap::from([("copy".into(), "cmd-c".to_owned())]);
        let palette = cx.update(|cx| {
            CommandPalette::new(
                [hidden, described, definition("copy", "Copy Selection")],
                &bindings,
                (),
                cx,
            )
        });
        let picker = palette.picker().clone();
        picker.update(cx, |picker, cx| {
            let rows = picker
                .visible_entries()
                .into_iter()
                .map(|entry| {
                    let row = picker.entry(entry);
                    (row.label.clone(), row.description.clone(), row.hint.clone())
                })
                .collect::<Vec<_>>();
            let copy_hint = super::display_key("cmd-c");
            assert_eq!(
                rows,
                [
                    (
                        "Close Window".into(),
                        Some("Close the focused window.".into()),
                        None
                    ),
                    ("Copy Selection".into(), None, Some(copy_hint)),
                ]
            );
            picker.set_query("move", cx);
            assert!(picker.visible_entries().is_empty());
            picker.set_query("workspace.cl", cx);
            assert_eq!(picker.visible_entries(), [0]);
            picker.set_query("selection", cx);
            let entry = picker.selected_entry().unwrap();
            assert_eq!(palette.command(entry).name.as_ref(), "copy");
        });
    }
}
