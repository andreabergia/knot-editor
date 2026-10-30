//! Native editor command declarations.
use super::*;
use crate::app::commands::declarations::{CommandContext, command};
use crate::app::commands::dispatch::ProductCommandTarget;
use crate::host::protocol::{Command, CommandArgumentValue, CommandOutcome};

fn claim_editor(command: &Command, target: &ProductCommandTarget, cx: &App) -> bool {
    if command.name.as_ref() != COPY_COMMAND || command.arguments != CommandArgumentValue::Null {
        return true;
    }
    matches!(target.command_view(cx), Some(crate::app::workbench::CommandView::Editor(editor)) if editor.read(cx).has_selection())
}

#[command("editor.move-left", title = "Move left", palette = false, id = MOVE_LEFT_COMMAND, participant = Editor)]
#[command("editor.move-right", title = "Move right", palette = false, id = MOVE_RIGHT_COMMAND, participant = Editor)]
#[command("editor.move-up", title = "Move up", palette = false, id = MOVE_UP_COMMAND, participant = Editor)]
#[command("editor.move-down", title = "Move down", palette = false, id = MOVE_DOWN_COMMAND, participant = Editor)]
#[command("editor.move-word-left", title = "Move to previous word", palette = false, id = MOVE_WORD_LEFT_COMMAND, participant = Editor)]
#[command("editor.move-word-right", title = "Move to next word", palette = false, id = MOVE_WORD_RIGHT_COMMAND, participant = Editor)]
#[command("editor.move-line-start", title = "Move to line start", palette = false, id = MOVE_LINE_START_COMMAND, participant = Editor)]
#[command("editor.move-line-end", title = "Move to line end", palette = false, id = MOVE_LINE_END_COMMAND, participant = Editor)]
#[command("editor.move-page-up", title = "Move up one page", palette = false, id = MOVE_PAGE_UP_COMMAND, participant = Editor)]
#[command("editor.move-page-down", title = "Move down one page", palette = false, id = MOVE_PAGE_DOWN_COMMAND, participant = Editor)]
#[command("editor.move-document-start", title = "Move to document start", palette = false, id = MOVE_DOCUMENT_START_COMMAND, participant = Editor)]
#[command("editor.move-document-end", title = "Move to document end", palette = false, id = MOVE_DOCUMENT_END_COMMAND, participant = Editor)]
#[command("editor.select-left", title = "Select left", palette = false, id = SELECT_LEFT_COMMAND, participant = Editor)]
#[command("editor.select-right", title = "Select right", palette = false, id = SELECT_RIGHT_COMMAND, participant = Editor)]
#[command("editor.select-up", title = "Select up", palette = false, id = SELECT_UP_COMMAND, participant = Editor)]
#[command("editor.select-down", title = "Select down", palette = false, id = SELECT_DOWN_COMMAND, participant = Editor)]
#[command("editor.select-word-left", title = "Select to previous word", palette = false, id = SELECT_WORD_LEFT_COMMAND, participant = Editor)]
#[command("editor.select-word-right", title = "Select to next word", palette = false, id = SELECT_WORD_RIGHT_COMMAND, participant = Editor)]
#[command("editor.select-line-start", title = "Select to line start", palette = false, id = SELECT_LINE_START_COMMAND, participant = Editor)]
#[command("editor.select-line-end", title = "Select to line end", palette = false, id = SELECT_LINE_END_COMMAND, participant = Editor)]
#[command("editor.select-page-up", title = "Select up one page", palette = false, id = SELECT_PAGE_UP_COMMAND, participant = Editor)]
#[command("editor.select-page-down", title = "Select down one page", palette = false, id = SELECT_PAGE_DOWN_COMMAND, participant = Editor)]
#[command("editor.select-document-start", title = "Select to document start", palette = false, id = SELECT_DOCUMENT_START_COMMAND, participant = Editor)]
#[command("editor.select-document-end", title = "Select to document end", palette = false, id = SELECT_DOCUMENT_END_COMMAND, participant = Editor)]
#[command("editor.insert-newline", title = "Insert Newline", palette = false, id = INSERT_NEWLINE_COMMAND, participant = Editor)]
#[command("editor.insert-tab", title = "Insert Tab", palette = false, id = INSERT_TAB_COMMAND, participant = Editor)]
#[command("editor.delete-backward", title = "Delete Backward", palette = false, id = DELETE_BACKWARD_COMMAND, participant = Editor)]
#[command("editor.delete-forward", title = "Delete Forward", palette = false, id = DELETE_FORWARD_COMMAND, participant = Editor)]
#[command("editor.select-all", title = "Select All", description = "Select the whole document.", id = SELECT_ALL_COMMAND, participant = Editor)]
#[command("editor.undo", title = "Undo", description = "Undo the last edit.", id = UNDO_COMMAND, participant = Editor)]
#[command("editor.redo", title = "Redo", description = "Redo the last undone edit.", id = REDO_COMMAND, participant = Editor)]
#[command("editor.cut", title = "Cut", description = "Cut the selection to the clipboard.", id = CUT_COMMAND, participant = Editor)]
#[command("copy", title = "Copy", description = "Copy the selection to the clipboard.", id = COPY_COMMAND, participant = Editor, claim = claim_editor)]
#[command("editor.paste", title = "Paste", description = "Paste from the clipboard.", id = PASTE_COMMAND, participant = Editor)]
pub(crate) async fn editing(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    let name = context.name;
    context
        .foreground(move |target, window, cx| {
            let Some(crate::app::workbench::CommandView::Editor(editor)) = target.command_view(cx)
            else {
                return Err(CommandOutcome::InvalidTarget);
            };
            if editor.update(cx, |editor, cx| {
                editor.execute_editing_command(name, window, cx)
            }) {
                Ok(())
            } else {
                Err(CommandOutcome::Unavailable)
            }
        })
        .await
}
