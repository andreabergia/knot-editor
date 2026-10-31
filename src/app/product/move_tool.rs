//! Native Move Tool to Dock…: choose where the focused tool goes from the
//! window's transient picker.

use super::*;
use crate::host::protocol::{CommandOutcome, PickerItem};

pub(super) const MOVE_TOOL_TITLE: &str = "Move Tool to Dock";

const DESTINATIONS: [(&str, ToolPlacement); 4] = [
    ("Left", ToolPlacement::Dock(DockLocation::Left)),
    ("Right", ToolPlacement::Dock(DockLocation::Right)),
    ("Bottom", ToolPlacement::Dock(DockLocation::Bottom)),
    ("Document Area", ToolPlacement::DocumentGroup),
];

impl ProductShell {
    /// Offer the docks and the document area for the captured tool and place
    /// it at the choice, publishing the tool-only default as any explicit
    /// move does.
    pub(crate) fn start_move_tool(
        &mut self,
        target: &ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        let TabSurfaceId::Tool(id) = target.surface else {
            return Err(CommandOutcome::Unavailable);
        };
        let items = DESTINATIONS
            .iter()
            .enumerate()
            .map(|(index, (label, _))| PickerItem {
                detail: None,
                hint: None,
                id: index.to_string(),
                label: (*label).into(),
                description: None,
            })
            .collect();
        let Ok(receiver) =
            self.begin_owned_picker(None, MOVE_TOOL_TITLE.into(), items, None, window, cx)
        else {
            cx.notify();
            return Ok(());
        };
        let handle = window.window_handle();
        let task = cx.spawn(async move |this, cx| {
            let Ok(TransientOutcome::Picker(choice)) = receiver.await else {
                return;
            };
            let Some((_, placement)) = choice
                .parse::<usize>()
                .ok()
                .and_then(|index| DESTINATIONS.get(index))
            else {
                return;
            };
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |shell, cx| shell.place_tool(id, *placement, window, cx))
            });
        });
        self.tasks.push(task);
        Ok(())
    }
}
