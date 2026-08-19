use gpui::{prelude::*, *};

use crate::host::protocol::CommandName;

use super::model::CommandDefinition;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommandPaletteEvent {
    Confirmed(CommandName),
    Dismissed,
}

pub(crate) struct CommandPalette {
    definitions: Vec<CommandDefinition>,
    query: String,
    selected: usize,
    focus: FocusHandle,
}

impl CommandPalette {
    pub(crate) fn new(
        definitions: impl IntoIterator<Item = CommandDefinition>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut definitions = definitions.into_iter().collect::<Vec<_>>();
        definitions.sort_by(|left, right| left.name.cmp(&right.name));
        Self {
            definitions,
            query: String::new(),
            selected: 0,
            focus: cx.focus_handle(),
        }
    }

    pub(crate) fn visible_definitions(&self) -> Vec<&CommandDefinition> {
        let query = self.query.to_lowercase();
        self.definitions
            .iter()
            .filter(|definition| {
                query.is_empty()
                    || definition.name.as_ref().to_lowercase().contains(&query)
                    || definition.title.to_lowercase().contains(&query)
            })
            .collect()
    }

    fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.query = query;
        self.selected = 0;
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.visible_definitions().len();
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
        if let Some(definition) = self.visible_definitions().get(self.selected) {
            cx.emit(CommandPaletteEvent::Confirmed(definition.name.clone()));
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
            "backspace" => {
                let mut query = self.query.clone();
                query.pop();
                self.set_query(query, cx);
            }
            _ if !event.keystroke.modifiers.platform && !event.keystroke.modifiers.control => {
                if let Some(text) = event.keystroke.key_char.as_deref() {
                    self.set_query(format!("{}{text}", self.query), cx);
                } else {
                    return;
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let selected = self.selected;
        let rows = self
            .visible_definitions()
            .into_iter()
            .enumerate()
            .map(|(index, definition)| {
                let entity = entity.clone();
                let name = definition.name.clone();
                div()
                    .id(("command-palette-entry", index))
                    .px_3()
                    .py_1()
                    .flex()
                    .flex_col()
                    .when(index == selected, |row| row.bg(rgb(0x2a4a7a)))
                    .hover(|row| row.bg(rgb(0x333333)))
                    .child(definition.title.clone())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x999999))
                            .child(definition.name.to_string()),
                    )
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |_palette, cx| {
                            cx.emit(CommandPaletteEvent::Confirmed(name.clone()));
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
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(0x444444))
                    .text_color(if self.query.is_empty() {
                        rgb(0x888888)
                    } else {
                        rgb(0xffffff)
                    })
                    .child(if self.query.is_empty() {
                        "Type to filter commands".to_owned()
                    } else {
                        self.query.clone()
                    }),
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

    use gpui::{AppContext, TestAppContext};

    use super::{CommandPalette, CommandPaletteEvent};
    use crate::app::model::{CommandDefinition, CommandOwner};

    #[gpui::test]
    fn filters_by_name_and_title_and_confirms_the_selection(cx: &mut TestAppContext) {
        let palette = cx.new(|cx| {
            CommandPalette::new(
                [
                    CommandDefinition {
                        name: "editor.copy".into(),
                        title: "Copy selection".into(),
                        owner: CommandOwner::Native,
                    },
                    CommandDefinition {
                        name: "workspace.close".into(),
                        title: "Close window".into(),
                        owner: CommandOwner::Native,
                    },
                ],
                cx,
            )
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&palette, move |_, event, _| {
                observed.borrow_mut().push(event.clone());
            })
        });

        palette.update(cx, |palette, cx| {
            palette.set_query("selection".into(), cx);
            assert_eq!(
                palette
                    .visible_definitions()
                    .iter()
                    .map(|definition| definition.name.as_ref())
                    .collect::<Vec<_>>(),
                ["editor.copy"]
            );
            palette.confirm(cx);
        });

        assert_eq!(
            events.borrow().as_slice(),
            [CommandPaletteEvent::Confirmed("editor.copy".into())]
        );
    }

    #[gpui::test]
    fn selection_is_clamped_and_dismissal_is_emitted(cx: &mut TestAppContext) {
        let palette = cx.new(|cx| {
            CommandPalette::new(
                [CommandDefinition {
                    name: "editor.copy".into(),
                    title: "Copy selection".into(),
                    owner: CommandOwner::Native,
                }],
                cx,
            )
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|cx| {
            cx.subscribe(&palette, move |_, event, _| {
                observed.borrow_mut().push(event.clone());
            })
        });

        palette.update(cx, |palette, cx| {
            palette.move_selection(10, cx);
            assert_eq!(palette.selected, 0);
            palette.dismiss(cx);
        });

        assert_eq!(events.borrow().as_slice(), [CommandPaletteEvent::Dismissed]);
    }
}
