//! Editing, clipboard, history, and caret-movement commands.

use super::*;

impl EditorView {
    pub(crate) fn execute_editing_command(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if command == commands::UNDO_COMMAND || command == commands::REDO_COMMAND {
            self.break_history_group(cx);
            self.marked_range_utf16 = None;
            self.preferred_x = None;
            let changed = self.model.update(cx, |model, cx| {
                let changed = if command == commands::UNDO_COMMAND {
                    model.undo()
                } else {
                    model.redo()
                }
                .unwrap_or(false);
                if changed {
                    cx.notify();
                }
                changed
            });
            if changed {
                let model = self.model.read(cx);
                self.text_changed(model);
                self.refresh(model, model.resolve_view_position(self.position_range));
                if let Some(restore) = model.history_view_restore() {
                    self.apply_history_view_restore(model, restore.generation, &restore.state);
                }
                self.sync_view_position(cx);
                cx.notify();
            }
            return true;
        }
        if matches!(command, commands::COPY_COMMAND | commands::CUT_COMMAND) {
            if command == commands::COPY_COMMAND && !self.has_selection() {
                return false;
            }
            self.break_history_group(cx);
            self.marked_range_utf16 = None;
            if command == commands::CUT_COMMAND && !self.model.read(cx).is_editable() {
                return true;
            }
            if let Some(range) = self.selected_byte_range(cx) {
                let text = self.model.read(cx).read(range);
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                if command == commands::CUT_COMMAND {
                    self.replace_text_with_grouping(None, "", None, window, cx);
                }
            }
            return true;
        }
        if command == commands::PASTE_COMMAND {
            self.break_history_group(cx);
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.replace_text_with_grouping(None, &text, None, window, cx);
            }
            return true;
        }
        if command == commands::SELECT_ALL_COMMAND {
            self.break_history_group(cx);
            self.marked_range_utf16 = None;
            self.anchor_line = 0;
            self.anchor_col = 0;
            self.cursor_line = self.line_count - 1;
            self.cursor_col = line_end(self.model.read(cx), self.cursor_line);
            self.has_selection = self.cursor_line != 0 || self.cursor_col != 0;
            self.preferred_x = None;
            self.finish_position_change(cx);
            return true;
        }
        if matches!(
            command,
            commands::INSERT_NEWLINE_COMMAND | commands::INSERT_TAB_COMMAND
        ) {
            self.break_history_group(cx);
            self.replace_text_with_grouping(
                None,
                if command.ends_with("newline") {
                    "\n"
                } else {
                    "\t"
                },
                None,
                window,
                cx,
            );
            return true;
        }
        if matches!(
            command,
            commands::DELETE_BACKWARD_COMMAND | commands::DELETE_FORWARD_COMMAND
        ) {
            if self.has_selection || self.marked_range_utf16.is_some() {
                self.replace_text_with_grouping(None, "", None, window, cx);
            } else {
                let backward = command.ends_with("backward");
                let range = self.grapheme_deletion_range(self.model.read(cx), backward);
                if !range.is_empty() {
                    self.replace_text_with_grouping(
                        Some(range),
                        "",
                        Some(if backward {
                            EditGroupKind::DeleteBackward
                        } else {
                            EditGroupKind::DeleteForward
                        }),
                        window,
                        cx,
                    );
                }
            }
            return true;
        }
        if command == commands::DELETE_TO_LINE_END_COMMAND {
            if self.has_selection || self.marked_range_utf16.is_some() {
                self.replace_text_with_grouping(None, "", None, window, cx);
                return true;
            }
            let model = self.model.read(cx);
            let caret = to_flat_byte(model, self.cursor_line, self.cursor_col);
            let end = to_flat_byte(model, self.cursor_line, line_end(model, self.cursor_line));
            let range = if caret < end {
                caret..end
            } else {
                self.grapheme_deletion_range(model, false)
            };
            if !range.is_empty() {
                self.break_history_group(cx);
                self.replace_text_with_grouping(Some(range), "", None, window, cx);
            }
            return true;
        }
        let (movement, extend) =
            if let Some(movement) = command.strip_prefix(commands::EDITOR_MOVE_PREFIX) {
                (movement, false)
            } else if let Some(movement) = command.strip_prefix(commands::EDITOR_SELECT_PREFIX) {
                (movement, true)
            } else {
                return false;
            };
        self.break_history_group(cx);
        self.marked_range_utf16 = None;
        let model = self.model.read(cx);
        let last = self.line_count - 1;
        let forward = matches!(
            movement,
            "right" | "down" | "word-right" | "line-end" | "page-down" | "document-end"
        );
        let vertical = matches!(movement, "up" | "down" | "page-up" | "page-down");
        let target = match movement {
            "left" if self.cursor_col > 0 => (
                self.cursor_line,
                prev_boundary(model, self.cursor_line, self.cursor_col),
            ),
            "left" if self.cursor_line > 0 => {
                (self.cursor_line - 1, line_end(model, self.cursor_line - 1))
            }
            "left" => (0, 0),
            "right" if self.cursor_col < line_end(model, self.cursor_line) => (
                self.cursor_line,
                next_boundary(model, self.cursor_line, self.cursor_col),
            ),
            "right" if self.cursor_line < last => (self.cursor_line + 1, 0),
            "right" => (last, line_end(model, last)),
            "line-start" => (self.cursor_line, 0),
            "line-end" => (self.cursor_line, line_end(model, self.cursor_line)),
            "document-start" => (0, 0),
            "document-end" => (last, line_end(model, last)),
            "word-left" | "word-right" => {
                let caret = to_flat_byte(model, self.cursor_line, self.cursor_col);
                flat_byte_to_position(
                    model,
                    word_boundary(model, self.cursor_line, caret, forward),
                )
            }
            "up" | "down" | "page-up" | "page-down" => {
                let rows = if movement.starts_with("page-") {
                    (self.viewport_h / LINE_HEIGHT).floor().max(1.) as usize
                } else {
                    1
                };
                let line = if forward {
                    self.cursor_line.saturating_add(rows).min(last)
                } else {
                    self.cursor_line.saturating_sub(rows)
                };
                let x = self
                    .preferred_x
                    .unwrap_or_else(|| self.caret_x(model, window, cx));
                self.preferred_x = Some(x);
                (line, self.column_for_x(model, line, x, window, cx))
            }
            _ => return false,
        };
        if extend {
            if !self.has_selection {
                self.anchor_line = self.cursor_line;
                self.anchor_col = self.cursor_col;
            }
            (self.cursor_line, self.cursor_col) = target;
            self.has_selection = target != (self.anchor_line, self.anchor_col);
        } else if self.has_selection && matches!(movement, "left" | "right" | "up" | "down") {
            let (start, end) = self.selection_range().unwrap();
            (self.cursor_line, self.cursor_col) = if forward { end } else { start };
            self.has_selection = false;
            self.preferred_x = None;
        } else {
            (self.cursor_line, self.cursor_col) = target;
            self.has_selection = false;
        }
        if !vertical {
            self.preferred_x = None;
        }
        self.finish_position_change(cx);
        true
    }

    /// The byte range one grapheme before or after the caret. At a line edge
    /// it covers the whole line terminator, `\n` or `\r\n`.
    pub(super) fn grapheme_deletion_range(
        &self,
        model: &BufferModel,
        backward: bool,
    ) -> Range<usize> {
        let (line, col) = (self.cursor_line, self.cursor_col);
        let caret = to_flat_byte(model, line, col);
        if backward {
            if col > 0 {
                to_flat_byte(model, line, prev_boundary(model, line, col))..caret
            } else if line > 0 {
                to_flat_byte(model, line - 1, line_end(model, line - 1))..caret
            } else {
                caret..caret
            }
        } else if col < line_end(model, line) {
            caret..to_flat_byte(model, line, next_boundary(model, line, col))
        } else if line + 1 < self.line_count {
            caret..model.line_range(line + 1).start
        } else {
            caret..caret
        }
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }
}
