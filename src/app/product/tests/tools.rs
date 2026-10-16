//! Native tools participate in the same presentation and closure boundaries as tabs.
use super::operations::empty_product_window;
use super::*;
use crate::app::workbench::{DockLocation, ToolPlacement};

#[gpui::test]
fn docked_explorer_opens_resources_routes_copy_and_has_nontext_snapshot(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    let (shell, window) = empty_product_window(&documents, cx);
    shell.update(cx, |shell, cx| {
        shell.install_workspace(ResourceUri::parse("mem://product/").unwrap(), cx);
    });
    cx.run_until_parked();
    let tree = cx.read(|cx| shell.read(cx).workspace_tree.clone().unwrap());
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    cx.update_window(window, |_, window, cx| {
        tree.focus_handle(cx).focus(window);
        tree.update(cx, |tree, cx| assert!(tree.activate_for_test(&uri, cx)));
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.normal_pane().unwrap();
        assert!(workbench.pane(pane).unwrap().tabs().iter().any(|tab| {
            tab.document_id()
                .is_some_and(|id| documents.read(cx).get(id).unwrap().resource_uri() == Some(&uri))
        }));
        let id = workbench.tools()[0].id;
        assert_eq!(
            workbench.dock_location(workbench.tool_tab(id).unwrap().0),
            Some(DockLocation::Left)
        );
    });
    let target = cx
        .update_window(window, |_, window, cx| {
            tree.focus_handle(cx).focus(window);
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    cx.read(|cx| {
        assert!(matches!(target.surface, TabSurfaceId::Tool(_)));
        assert!(target.document_id(cx).is_err());
    });
    let copy = cx.update(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .clone()
            .update(cx, |dispatcher, cx| {
                dispatcher.dispatch(
                    Command {
                        name: super::super::COPY_COMMAND.into(),
                        arguments: CommandArgumentValue::Null,
                    },
                    target.clone(),
                    cx,
                )
            })
    });
    assert_eq!(wait_for_command(copy, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            uri.to_string()
        )
    });
    cx.update_window(window, |_, window, cx| {
        let state =
            shell
                .read(cx)
                .application_window(window, &std::collections::HashMap::new(), cx);
        let tool = state
            .panes
            .iter()
            .flat_map(|pane| &pane.tabs)
            .find(|tab| tab.kind == crate::host::protocol::SurfaceKind::Tool)
            .unwrap();
        assert!(tool.buffer.is_none());
        assert!(tool.document.is_none());
        assert_eq!(state.current_target, Some(tool.target.clone()));
    })
    .unwrap();
    let moved = dispatch_product_command(&shell, window, "workbench.explorer-in-editor", cx);
    assert_eq!(wait_for_command(moved, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(target.validate_tab(cx), Err(CommandOutcome::InvalidTarget)));
    let close = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    assert!(!cx.has_pending_prompt());
    let reopen = dispatch_product_command(&shell, window, "workbench.toggle-explorer", cx);
    assert_eq!(wait_for_command(reopen, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(
            workbench
                .tool(id_of(workbench))
                .unwrap()
                .content
                .presentation(),
            tree.entity_id()
        );
        let pane = workbench.tool_tab(id_of(workbench)).unwrap().0;
        assert!(workbench.dock_location(pane).is_none());
        assert!(
            workbench
                .pane(pane)
                .unwrap()
                .tabs()
                .iter()
                .any(|tab| tab.document_id().is_some())
        );
    });
}

fn id_of(workbench: &Workbench) -> crate::app::workbench::ToolFrameId {
    workbench.tools()[0].id
}

#[gpui::test]
fn last_document_closure_leaves_tool_only_window_and_hidden_definitions_survive_replacement(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    let workbench = cx.read(|cx| shell.read(cx).workbench.clone());
    shell.update(cx, |shell, cx| shell.ensure_explorer(cx));
    let document = cx.read(|cx| {
        workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .document_id()
            .unwrap()
    });
    let close = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).is_none());
        assert_eq!(documents.read(cx).documents().count(), 0);
        assert_eq!(&shell.read(cx).workbench, &workbench);
        assert_eq!(workbench.read(cx).tab_snapshots().len(), 1);
        assert!(matches!(
            workbench.read(cx).tab_snapshots()[0].surface,
            TabSurfaceId::Tool(_)
        ));
    });
    cx.update_window(window, |_, window, cx| {
        let target = shell.update(cx, |shell, cx| {
            shell.capture_command_target(window, cx).unwrap()
        });
        assert!(matches!(target.surface, TabSurfaceId::Tool(_)));
        assert_eq!(target.validate_view(cx), Ok(()));
    })
    .unwrap();
    let new = dispatch_product_command(&shell, window, NEW_COMMAND, cx);
    assert_eq!(wait_for_command(new, cx), CommandOutcome::Completed);
    shell.update(cx, |shell, cx| {
        let id = id_of(shell.workbench.read(cx));
        shell.workbench.update(cx, |workbench, _| {
            workbench.hide_tool(id);
        });
    });
    let close = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert_eq!(&shell.read(cx).workbench, &workbench);
        assert_eq!(documents.read(cx).documents().count(), 1);
        assert_eq!(workbench.read(cx).tools().len(), 1);
        assert!(
            workbench
                .read(cx)
                .tool_tab(id_of(workbench.read(cx)))
                .is_none()
        );
    });
}

#[gpui::test]
fn workspace_refresh_does_not_reopen_or_reposition_tool(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    let (shell, _) = empty_product_window(&documents, cx);
    shell.update(cx, |shell, cx| {
        shell.ensure_explorer(cx);
        let id = id_of(shell.workbench.read(cx));
        shell.workbench.update(cx, |workbench, _| {
            workbench.place_tool(id, ToolPlacement::Dock(DockLocation::Bottom));
            workbench.hide_tool(id);
        });
        shell.install_workspace(ResourceUri::parse("mem://product/src").unwrap(), cx);
        assert!(shell.workbench.read(cx).tool_tab(id).is_none());
        shell.workbench.update(cx, |workbench, _| {
            let (pane, _) = workbench.show_tool(id).unwrap();
            assert_eq!(workbench.dock_location(pane), Some(DockLocation::Bottom));
        });
    });
}

#[gpui::test]
async fn tools_do_not_bypass_dirty_document_or_terminal_protection(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    shell.update(cx, |shell, cx| shell.ensure_explorer(cx));
    let close = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    assert_eq!(close.completion.await.unwrap(), CommandOutcome::Cancelled);
    let moved = dispatch_product_command(&shell, window, "workbench.explorer-in-editor", cx);
    assert_eq!(wait_for_command(moved, cx), CommandOutcome::Completed);
    let close_tool = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(wait_for_command(close_tool, cx), CommandOutcome::Completed);
    assert!(!cx.has_pending_prompt());
    cx.read(|cx| assert!(documents.read(cx).get(document).unwrap().is_dirty(cx)));
    let reopen = dispatch_product_command(&shell, window, "workbench.toggle-explorer", cx);
    assert_eq!(wait_for_command(reopen, cx), CommandOutcome::Completed);
    let terminal = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
    assert_eq!(wait_for_command(terminal, cx), CommandOutcome::Completed);
    let session = cx.read(|cx| {
        match shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        {
            TabSurfaceId::Terminal(id) => id,
            _ => panic!("terminal missing"),
        }
    });
    for command in [CLOSE_WINDOW_COMMAND, QUIT_COMMAND] {
        let close = dispatch_product_command(&shell, window, command, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        assert_eq!(close.completion.await.unwrap(), CommandOutcome::Cancelled);
        cx.read(|cx| {
            assert!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions
                    .contains_key(&session)
            );
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
            assert!(
                shell
                    .read(cx)
                    .workbench
                    .read(cx)
                    .tool_tab(id_of(shell.read(cx).workbench.read(cx)))
                    .is_some()
            );
        });
    }
    let close_terminal = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(
        wait_for_command(close_terminal, cx),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
    cx.read(|cx| {
        assert!(
            !cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&session)
        )
    });
}

#[gpui::test]
fn explorer_docks_have_visible_geometry_and_open_files_by_click(cx: &mut TestAppContext) {
    for dock in DockLocation::ALL {
        let documents = install_globals(cx);
        cx.set_global(ApplicationFileSystems(memory_filesystems()));
        let (shell, window) = empty_product_window(&documents, cx);
        shell.update(cx, |shell, cx| {
            shell.install_workspace(ResourceUri::parse("mem://product/").unwrap(), cx);
            let id = shell.explorer_frame(cx).unwrap();
            shell.workbench.update(cx, |workbench, _| {
                assert!(workbench.place_tool(id, ToolPlacement::Dock(dock)));
                assert_eq!(workbench.tool_tab(id).unwrap().0.value(), 3);
            });
            cx.notify();
        });
        cx.run_until_parked();
        let mut visual = VisualTestContext::from_window(window, cx);
        let explorer = visual.debug_bounds("pane-3").unwrap();
        let editor = visual.debug_bounds("pane-1").unwrap();
        match dock {
            DockLocation::Left | DockLocation::Right => {
                assert_eq!(explorer.size.width, px(240.), "{dock:?}: {explorer:?}");
                assert!(explorer.size.height > px(100.));
            }
            DockLocation::Bottom => {
                assert_eq!(explorer.size.height, px(220.), "{explorer:?}");
                assert!(explorer.size.width > px(100.));
            }
        }
        assert!(editor.size.width > px(100.));
        assert!(editor.size.height > px(100.));
        shell.update(&mut visual, |shell, cx| {
            let id = shell.explorer_frame(cx).unwrap();
            shell.workbench.update(cx, |workbench, _| {
                assert!(workbench.hide_tool(id));
            });
            cx.notify();
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("workspace-tree-row-2").is_none());
        shell.update(&mut visual, |shell, cx| {
            let id = shell.explorer_frame(cx).unwrap();
            shell.workbench.update(cx, |workbench, _| {
                workbench.show_tool(id).unwrap();
            });
            cx.notify();
        });
        visual.run_until_parked();
        let reopened = visual.debug_bounds("pane-4").unwrap();
        assert_eq!(reopened.size, explorer.size);
        let file = visual.debug_bounds("workspace-tree-row-2").unwrap();
        assert!(file.size.width > px(100.));
        assert!(file.size.height > px(0.));
        assert!(explorer.contains(&file.center()));
        visual.simulate_click(file.center(), Modifiers::default());
        visual.run_until_parked();
        visual.read(|cx| {
            let state = shell.read(cx);
            let pane = state.workbench.read(cx).focused_pane().unwrap();
            let document = pane.active_tab().document_id().unwrap();
            assert_eq!(
                documents.read(cx).get(document).unwrap().resource_uri(),
                Some(&ResourceUri::parse("mem://product/notes.txt").unwrap())
            );
            assert!(state.workbench.read(cx).dock_location(pane.id()).is_none());
        });
    }
}
