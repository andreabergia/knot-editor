//! The command palette: the catalog's commands presented in the shared
//! [`Picker`] and confirmed against the target captured when it opened.

use gpui::{App, AppContext as _, Entity};

use crate::host::protocol::{Command, CommandArgumentValue};

use super::{
    PALETTE_KEY_CONTEXT,
    commands::catalog::CommandDefinition,
    picker::{Picker, PickerEntry},
};

pub(crate) struct CommandPalette<Origin> {
    picker: Entity<Picker>,
    commands: Vec<Command>,
    origin: Origin,
}

impl<Origin: Clone> CommandPalette<Origin> {
    pub(crate) fn new(
        definitions: impl IntoIterator<Item = CommandDefinition>,
        origin: Origin,
        cx: &mut App,
    ) -> Self {
        let mut definitions = definitions.into_iter().collect::<Vec<_>>();
        definitions.sort_by(|left, right| left.title.cmp(&right.title));
        let entries = definitions
            .iter()
            .map(|definition| PickerEntry {
                description: Some(definition.name.to_string()),
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
    use gpui::TestAppContext;

    use super::CommandPalette;
    use crate::app::commands::catalog::{CommandDefinition, CommandOwner};

    fn definition(name: &str, title: &str) -> CommandDefinition {
        CommandDefinition {
            name: name.into(),
            title: title.into(),
            owner: CommandOwner::Native,
        }
    }

    #[gpui::test]
    fn lists_by_title_and_matches_the_command_name(cx: &mut TestAppContext) {
        let palette = cx.update(|cx| {
            CommandPalette::new(
                [
                    definition("workspace.close", "Close window"),
                    definition("copy", "Copy selection"),
                ],
                (),
                cx,
            )
        });
        let picker = palette.picker().clone();
        let window = cx.add_window(|_, _| gpui::Empty);
        window
            .update(cx, |_, _, cx| {
                picker.update(cx, |picker, cx| {
                    assert_eq!(picker.visible_entries(), [0, 1]);
                    picker.set_query("workspace.cl", cx);
                    assert_eq!(picker.visible_entries(), [0]);
                    picker.set_query("selection", cx);
                    let entry = picker.selected_entry().unwrap();
                    assert_eq!(palette.command(entry).name.as_ref(), "copy");
                });
            })
            .unwrap();
    }
}
