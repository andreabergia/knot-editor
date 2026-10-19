use gpui::{prelude::*, *};

use crate::host::protocol::{Command, CommandArgumentValue};

use super::{
    PALETTE_KEY_CONTEXT,
    model::CommandDefinition,
    text_input::{TextInput, TextInputEvent},
};

#[derive(Clone, PartialEq)]
pub(crate) enum CommandPaletteEvent<Origin> {
    Confirmed { command: Command, origin: Origin },
    Dismissed,
}

#[derive(Clone)]
pub(crate) struct CommandPaletteEntry {
    definition: CommandDefinition,
    arguments: CommandArgumentValue,
}

impl CommandPaletteEntry {
    pub(crate) fn new(definition: CommandDefinition, arguments: CommandArgumentValue) -> Self {
        Self {
            definition,
            arguments,
        }
    }

    fn command(&self) -> Command {
        Command {
            name: self.definition.name.clone(),
            arguments: self.arguments.clone(),
        }
    }
}

pub(crate) struct CommandPalette<Origin> {
    entries: Vec<CommandPaletteEntry>,
    query: String,
    selected: usize,
    input: Entity<TextInput>,
    _input_subscription: Subscription,
    origin: Origin,
}

impl<Origin> CommandPalette<Origin>
where
    Origin: Clone + PartialEq + 'static,
{
    pub(crate) fn new(
        entries: impl IntoIterator<Item = CommandPaletteEntry>,
        origin: Origin,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut entries = entries.into_iter().collect::<Vec<_>>();
        entries.sort_by(|left, right| left.definition.name.cmp(&right.definition.name));
        let input = cx.new(|cx| TextInput::new("", cx));
        let subscription = cx.subscribe(
            &input,
            |this: &mut Self, _input, _event: &TextInputEvent, cx| {
                this.query = this.input.read(cx).text().to_string();
                this.selected = 0;
                cx.notify();
            },
        );
        Self {
            entries,
            query: String::new(),
            selected: 0,
            input,
            _input_subscription: subscription,
            origin,
        }
    }

    pub(crate) fn origin(&self) -> Origin {
        self.origin.clone()
    }

    fn visible_entries(&self) -> Vec<&CommandPaletteEntry> {
        let query = self.query.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| {
                query.is_empty()
                    || entry
                        .definition
                        .name
                        .as_ref()
                        .to_lowercase()
                        .contains(&query)
                    || entry.definition.title.to_lowercase().contains(&query)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn visible_definitions(&self) -> Vec<&CommandDefinition> {
        self.visible_entries()
            .into_iter()
            .map(|entry| &entry.definition)
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn selected_definition(&self) -> Option<&CommandDefinition> {
        self.visible_entries()
            .get(self.selected)
            .map(|entry| &entry.definition)
    }

    #[cfg(test)]
    pub(crate) fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(query, cx));
        self.query = self.input.read(cx).text().to_string();
        self.selected = 0;
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.visible_entries().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.visible_entries().get(self.selected) {
            cx.emit(CommandPaletteEvent::Confirmed {
                command: entry.command(),
                origin: self.origin(),
            });
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(CommandPaletteEvent::Dismissed);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.to_lowercase().as_str() {
            "escape" => self.dismiss(cx),
            "enter" => self.confirm(cx),
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "pageup" => self.move_selection(-10, cx),
            "pagedown" => self.move_selection(10, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl<Origin> EventEmitter<CommandPaletteEvent<Origin>> for CommandPalette<Origin> where
    Origin: Clone + PartialEq + 'static
{
}

impl<Origin> Focusable for CommandPalette<Origin>
where
    Origin: Clone + PartialEq + 'static,
{
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle()
    }
}

impl<Origin> Render for CommandPalette<Origin>
where
    Origin: Clone + PartialEq + 'static,
{
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let selected = self.selected;
        let rows = self
            .visible_entries()
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let entity = entity.clone();
                let command = entry.command();
                let arguments = (entry.arguments != CommandArgumentValue::Null)
                    .then(|| serde_json::to_string(&entry.arguments).expect("arguments serialize"));
                div()
                    .id(("command-palette-entry", index))
                    .px_3()
                    .py_1()
                    .flex()
                    .flex_col()
                    .when(index == selected, |row| row.bg(rgb(0x2a4a7a)))
                    .hover(|row| row.bg(rgb(0x333333)))
                    .child(entry.definition.title.clone())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x999999))
                            .child(entry.definition.name.to_string()),
                    )
                    .children(arguments.map(|arguments| {
                        div()
                            .text_xs()
                            .text_color(rgb(0xc586c0))
                            .child(format!("Arguments: {arguments}"))
                    }))
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |palette, cx| {
                            cx.emit(CommandPaletteEvent::Confirmed {
                                command: command.clone(),
                                origin: palette.origin(),
                            });
                        });
                    })
            })
            .collect::<Vec<_>>();

        div()
            .id("command-palette")
            .w(px(560.))
            .max_h(px(420.))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x555555))
            .bg(rgb(0x252526))
            .shadow_lg()
            .key_context(PALETTE_KEY_CONTEXT)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(0x444444))
                    .child(self.input.clone()),
            )
            .child(
                div()
                    .id("command-palette-results")
                    .overflow_scroll()
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::TestAppContext;

    use super::{CommandPalette, CommandPaletteEntry, CommandPaletteEvent};
    use crate::app::CommandOrigin;
    use crate::app::model::{CommandDefinition, CommandOwner};
    use crate::host::protocol::{Command, CommandArgumentValue};

    #[gpui::test]
    fn filters_by_name_and_title_and_confirms_the_selection(cx: &mut TestAppContext) {
        let (palette, cx) = cx.add_window_view(|window, cx| {
            let origin_focus = cx.focus_handle();
            CommandPalette::new(
                [
                    CommandPaletteEntry::new(
                        CommandDefinition {
                            name: "copy".into(),
                            title: "Copy selection".into(),
                            owner: CommandOwner::Native,
                        },
                        CommandArgumentValue::String("fixture".into()),
                    ),
                    CommandPaletteEntry::new(
                        CommandDefinition {
                            name: "workspace.close".into(),
                            title: "Close window".into(),
                            owner: CommandOwner::Native,
                        },
                        CommandArgumentValue::Null,
                    ),
                ],
                CommandOrigin {
                    window: window.window_handle(),
                    focus: origin_focus.downgrade(),
                    buffer: None,
                },
                cx,
            )
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&palette, move |_, event, _| {
                observed.borrow_mut().push(event.clone());
            })
        });

        let expected_origin = palette.read_with(cx, |palette, _| palette.origin());
        palette.update(cx, |palette, cx| {
            palette.set_query("selection".into(), cx);
            assert_eq!(
                palette
                    .visible_definitions()
                    .iter()
                    .map(|definition| definition.name.as_ref())
                    .collect::<Vec<_>>(),
                ["copy"]
            );
            palette.confirm(cx);
        });

        assert!(
            events.borrow().as_slice()
                == [CommandPaletteEvent::Confirmed {
                    command: Command {
                        name: "copy".into(),
                        arguments: CommandArgumentValue::String("fixture".into()),
                    },
                    origin: expected_origin,
                }]
                .as_slice()
        );
    }

    #[gpui::test]
    fn selection_is_clamped_and_dismissal_is_emitted(cx: &mut TestAppContext) {
        let (palette, cx) = cx.add_window_view(|window, cx| {
            let origin_focus = cx.focus_handle();
            CommandPalette::new(
                [CommandPaletteEntry::new(
                    CommandDefinition {
                        name: "copy".into(),
                        title: "Copy selection".into(),
                        owner: CommandOwner::Native,
                    },
                    CommandArgumentValue::Null,
                )],
                CommandOrigin {
                    window: window.window_handle(),
                    focus: origin_focus.downgrade(),
                    buffer: None,
                },
                cx,
            )
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&palette, move |_, event, _| {
                observed.borrow_mut().push(event.clone());
            })
        });

        palette.update(cx, |palette, cx| {
            palette.move_selection(10, cx);
            assert_eq!(palette.selected, 0);
            palette.dismiss(cx);
        });

        assert!(events.borrow().as_slice() == [CommandPaletteEvent::Dismissed].as_slice());
    }
}
