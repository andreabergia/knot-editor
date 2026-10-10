//! Foreground coordination of exact tab and destination identities.

use super::*;
use crate::app::workbench::{TabMoveCapture, TabMoveDestination};
use crate::host::protocol::CommandOutcome;

#[derive(Clone)]
pub(super) struct TabRelocationSource {
    pub(super) workbench: WeakEntity<Workbench>,
    pub(super) tab: TabMoveCapture,
}

#[derive(Clone)]
pub(super) struct TabRelocationDestination {
    pub(super) window: AnyWindowHandle,
    pub(super) workbench: WeakEntity<Workbench>,
    pub(super) tab: TabMoveDestination,
}

impl ProductShell {
    pub(super) fn capture_tab_relocation(
        &self,
        pane: PaneId,
        tab: TabId,
        cx: &App,
    ) -> Option<TabRelocationSource> {
        Some(TabRelocationSource {
            workbench: self.workbench.downgrade(),
            tab: self.workbench.read(cx).capture_tab_move(pane, tab)?,
        })
    }

    fn relocation_is_live(&self, source: &TabRelocationSource, cx: &App) -> bool {
        if source.workbench.upgrade() != Some(self.workbench.clone())
            || !self.workbench.read(cx).contains_tab_move(source.tab)
        {
            return false;
        }
        match source.tab.tab.surface {
            TabSurfaceId::Document(document) => {
                Self::documents(cx).read(cx).get(document).is_some()
            }
            TabSurfaceId::Terminal(id) => {
                let session = cx
                    .global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .get(&id)
                    .cloned();
                let view = self
                    .workbench
                    .read(cx)
                    .pane(source.tab.tab.pane_id)
                    .and_then(|pane| {
                        pane.tabs()
                            .iter()
                            .find(|tab| tab.id() == source.tab.tab.tab_id)
                    })
                    .and_then(|tab| tab.terminal_view());
                session
                    .zip(view)
                    .is_some_and(|(session, view)| view.read(cx).is_attached_to(&session))
            }
        }
    }

    pub(super) fn relocate_tab_to(
        &mut self,
        source: &TabRelocationSource,
        destination: &TabRelocationDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CommandOutcome {
        if window.root::<ProductShell>().flatten() != Some(cx.entity())
            || !self.relocation_is_live(source, cx)
        {
            return CommandOutcome::InvalidTarget;
        }
        if destination.window == window.window_handle() {
            if destination.workbench.upgrade() != Some(self.workbench.clone()) {
                return CommandOutcome::InvalidTarget;
            }
            let moved = self.workbench.update(cx, |workbench, cx| {
                let transition = workbench.relocate_tab(source.tab, destination.tab);
                if transition.is_some() {
                    cx.notify();
                }
                transition
            });
            if moved.is_none() {
                return CommandOutcome::InvalidTarget;
            }
            self.focus_active_surface(window, cx);
            cx.notify();
            return CommandOutcome::Completed;
        }
        if destination.workbench.upgrade() == Some(self.workbench.clone()) {
            return CommandOutcome::InvalidTarget;
        }
        let result = cx.update_window(destination.window, |_, destination_window, cx| {
            let Some(shell) = destination_window.root::<ProductShell>().flatten() else {
                return CommandOutcome::InvalidTarget;
            };
            shell.update(cx, |shell, cx| {
                if destination.workbench.upgrade() != Some(shell.workbench.clone())
                    || !self.relocation_is_live(source, cx)
                {
                    return CommandOutcome::InvalidTarget;
                }
                let source_workbench = self.workbench.clone();
                let destination_workbench = shell.workbench.clone();
                let target = destination.tab;
                let session = match source.tab.tab.surface {
                    TabSurfaceId::Terminal(id) => cx
                        .global::<ApplicationTerminalSessions>()
                        .0
                        .borrow()
                        .sessions
                        .get(&id)
                        .cloned(),
                    TabSurfaceId::Document(_) => None,
                };
                let moved = source_workbench.update(cx, |workbench, cx| {
                    let moved = destination_workbench.update(cx, |destination, cx| {
                        let moved =
                            workbench.transfer_tab_to(destination, source.tab, target, |view| {
                                view.update(cx, |view, cx| view.detach(cx));
                                cx.new(|cx| {
                                    TerminalView::new(
                                        session.expect("validated terminal registry entry"),
                                        cx,
                                    )
                                })
                            });
                        if moved.is_some() {
                            cx.notify();
                        }
                        moved
                    });
                    if moved.is_some() {
                        cx.notify();
                    }
                    moved
                });
                let Some(moved) = moved else {
                    return CommandOutcome::InvalidTarget;
                };
                if moved.source_is_empty {
                    self.replace_empty_workbench(cx);
                }
                if matches!(destination.tab, TabMoveDestination::Empty) {
                    cx.global::<ApplicationWorkbenches>()
                        .register(&destination_workbench);
                }
                shell.focus_active_surface(destination_window, cx);
                if !matches!(destination.tab, TabMoveDestination::Empty) {
                    destination_window.activate_window();
                }
                shell.status = "tab moved here".into();
                cx.notify();
                CommandOutcome::Completed
            })
        });
        if !matches!(result, Ok(CommandOutcome::Completed)) {
            return CommandOutcome::InvalidTarget;
        }
        self.focus_active_surface(window, cx);
        cx.notify();
        CommandOutcome::Completed
    }

    pub(super) fn relocate_tab_to_new_window_with(
        &mut self,
        source: &TabRelocationSource,
        window: &mut Window,
        cx: &mut Context<Self>,
        create_destination: impl FnOnce(&mut App) -> anyhow::Result<WindowHandle<ProductShell>>,
    ) -> CommandOutcome {
        if !self.relocation_is_live(source, cx) {
            return CommandOutcome::InvalidTarget;
        }
        let existing = cx.windows();
        let Ok(destination) = create_destination(cx) else {
            self.status = "could not create destination window".into();
            cx.notify();
            return CommandOutcome::HandlerFailure {
                message: "could not create destination window".into(),
            };
        };
        let destination_window = destination.into();
        if existing.contains(&destination_window) {
            return CommandOutcome::InvalidTarget;
        }
        let workbench = destination
            .read(cx)
            .ok()
            .map(|shell| shell.workbench.downgrade());
        let outcome = if let Some(workbench) = workbench {
            self.relocate_tab_to(
                source,
                &TabRelocationDestination {
                    window: destination_window,
                    workbench,
                    tab: TabMoveDestination::Empty,
                },
                window,
                cx,
            )
        } else {
            CommandOutcome::InvalidTarget
        };
        if outcome != CommandOutcome::Completed {
            let _ = cx.update_window(destination_window, |_, window, _| window.remove_window());
        } else {
            #[cfg(target_os = "macos")]
            let _ = cx.update_window(destination_window, |_, window, cx| {
                window.draw(cx).clear();
                window.activate_window();
            });
        }
        outcome
    }
}

#[cfg(test)]
mod tests;
