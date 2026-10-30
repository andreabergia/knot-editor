//! The product command catalog, captured targets, palette, and dispatch.
use super::*;

#[gpui::test]
fn product_catalog_registers_every_slice_command(cx: &mut TestAppContext) {
    install_globals(cx);
    cx.read(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.read(cx);
        let mut actual = dispatcher
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>();
        let mut expected = super::super::super::commands::dispatch::product_command_names()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    });
}

#[gpui::test]
fn pooled_fixture_commands_and_completions_use_the_product_target(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    let host = cx.update(|cx| {
        let host = super::super::super::extensions::host::install(cx);
        host.update(cx, |host, cx| host.start_test_fixture("product", cx));
        host
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        let registered = cx.read(|cx| {
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .definitions()
                .any(|definition| definition.name.as_ref() == "knot.fixture.primary")
        });
        if registered {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture command did not enter the product catalog"
        );
        std::thread::yield_now();
    }

    let mut execution = dispatch_product_command(&shell, window, "knot.fixture.primary", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let outcome = loop {
        cx.run_until_parked();
        match execution.completion.try_recv() {
            Ok(outcome) => break outcome,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            Err(error) => panic!("fixture command completion closed: {error}"),
        }
    };
    assert_eq!(outcome, CommandOutcome::Completed);
    assert_eq!(cx.read(|cx| model.read(cx).text()), "fixture ");

    let mut completion = dispatch_product_command(
        &shell,
        window,
        super::super::super::commands::dispatch::SHOW_COMPLETIONS_COMMAND,
        cx,
    );
    cx.run_until_parked();
    assert_eq!(
        completion.completion.try_recv().unwrap(),
        CommandOutcome::Completed
    );

    let editor = cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone()
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            editor
                .read(cx)
                .completion_state()
                .is_some_and(|(items, pending, failures, _)| {
                    items == 2 && pending == 0 && failures == 0
                })
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture completion items did not reach the product editor"
        );
        std::thread::yield_now();
    }

    drop(host);
    cx.update(|cx| {
        drop(cx.remove_global::<super::super::super::extensions::host::ApplicationExtensionHost>())
    });
    cx.run_until_parked();
}

#[gpui::test]
async fn dispatch_completion_preserves_the_captured_pane(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);

    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.refresh().unwrap();

    let (first_pane, second_pane, first_editor, second_editor) = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let first = &workbench.panes()[0];
        let second = &workbench.panes()[1];
        (
            first.id(),
            second.id(),
            first.active_tab().editor().unwrap().clone(),
            second.active_tab().editor().unwrap().clone(),
        )
    });
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            first_editor.focus_handle(cx).focus(window);
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: NEW_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });
    cx.update_window(window_handle, |_, window, cx| {
        second_editor.focus_handle(cx).focus(window)
    })
    .unwrap();

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
        assert_eq!(workbench.pane(second_pane).unwrap().tabs().len(), 1);
    });
}

#[gpui::test]
async fn destroyed_product_targets_are_not_retargeted(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    cx.refresh().unwrap();

    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.close_active_tab(window, cx));
    })
    .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SPLIT_HORIZONTAL_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes().len(), 1));
}

#[gpui::test]
fn menu_action_reaches_the_captured_window_in_its_dispatch_turn(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.refresh().unwrap();

    cx.dispatch_action(window, ProductCommandSource::new(NEW_COMMAND));

    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            2
        );
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Completed),
        );
    });
}

#[gpui::test]
async fn palette_discovers_commands_and_keeps_its_opening_target(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();
    cx.refresh().unwrap();

    let (first_pane, second_pane, first_editor) = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let first = &workbench.panes()[0];
        (
            first.id(),
            workbench.panes()[1].id(),
            first.active_tab().editor().unwrap().clone(),
        )
    });
    cx.update_window(window_handle, |_, window, cx| {
        first_editor.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "ctrl-shift-p"
        } else {
            "cmd-shift-p"
        },
    );
    cx.run_until_parked();
    cx.update_window(window_handle, |_, _, cx| {
        assert!(shell.read(cx).command_palette.is_some());
        let workbench = shell.read(cx).workbench.clone();
        workbench.update(cx, |workbench, _| {
            workbench.focus_pane(second_pane);
        });
    })
    .unwrap();
    cx.refresh().unwrap();

    cx.simulate_keystrokes(window_handle, "f i l e . n e w enter");

    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
        assert_eq!(workbench.pane(second_pane).unwrap().tabs().len(), 1);
    });
}

#[gpui::test]
async fn native_participants_reject_arguments_before_mutation(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("ignored".into())));
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    for name in [
        super::super::COPY_COMMAND,
        super::super::PASTE_COMMAND,
        NEW_COMMAND,
        super::super::NEW_WINDOW_COMMAND,
        SHOW_EXTENSION_REPORT_COMMAND,
    ] {
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: name.into(),
                    arguments: CommandArgumentValue::String("unexpected".into()),
                },
                target.clone(),
                cx,
            )
        });
        assert!(
            matches!(
                execution.completion.await.unwrap(),
                CommandOutcome::InvalidArgument { .. }
            ),
            "{name}"
        );
    }
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "");
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .tabs()
                .len(),
            1
        );
        assert_eq!(cx.windows().len(), 1);
    });
}

#[gpui::test]
fn palette_shows_titles_descriptions_and_bindings_and_hides_opted_out_commands(
    cx: &mut TestAppContext,
) {
    use crate::host::lifecycle::ExtensionKey;
    use crate::host::protocol::{ExtensionId, ExtensionLifecycleId};

    const SOURCE: &str = r#"
import { commands, keybinding } from "knot";
await commands.register("fixture.described", async () => {}, {
  title: "Run Fixture",
  description: "Runs the fixture.",
});
await commands.register("fixture.hidden", async () => {}, { palette: false });
await commands.register("fixture.plain", async () => {});
keybinding("ctrl-alt-f", "fixture.described");
"#;
    let documents = install_globals(cx);
    cx.update(crate::app::commands::keymaps::install);
    let host = cx.update(crate::app::extensions::host::install);
    let key = ExtensionKey::new(ExtensionId::new(712), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("fixture.plain")
            .is_ok()
    });
    cx.run_until_parked();
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_active_surface(window, cx);
            shell.open_command_palette(window, cx);
        });
    })
    .unwrap();
    cx.update(|cx| {
        let picker = shell
            .read(cx)
            .command_palette
            .as_ref()
            .unwrap()
            .picker()
            .clone();
        let picker = picker.read(cx);
        let rows = picker
            .visible_entries()
            .into_iter()
            .map(|entry| picker.entry(entry).clone())
            .collect::<Vec<_>>();
        let described = rows.iter().find(|row| row.label == "Run Fixture").unwrap();
        assert_eq!(described.description.as_deref(), Some("Runs the fixture."));
        assert_eq!(
            described.hint,
            Some(crate::app::commands::keymaps::display_key("ctrl-alt-f"))
        );
        assert!(rows.iter().any(|row| row.label == "fixture.plain"));
        assert!(!rows.iter().any(|row| row.label == "fixture.hidden"));
        assert!(!rows.iter().any(|row| row.label == "Move left"));
        let save = rows.iter().find(|row| row.label == "Save").unwrap();
        assert!(save.hint.is_some());
    });
}

#[gpui::test]
fn confirming_a_palette_command_ranks_it_first_next_time(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let state_dir = tempfile::tempdir().unwrap();
    let database = crate::app::record_store::StateDatabase::open(
        state_dir.path().join("state.sqlite3"),
        cx.background_executor.clone(),
    );
    cx.update(|cx| {
        cx.set_global(crate::app::command_history::ApplicationCommandHistory(
            crate::app::command_history::CommandHistory::open(&database),
        ))
    });
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let first_command = |cx: &mut TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.open_command_palette(window, cx);
                let palette = shell.command_palette.as_ref().unwrap();
                let entry = palette.picker().read(cx).selected_entry().unwrap();
                palette.command(entry).name.to_string()
            })
        })
        .unwrap()
    };
    assert_ne!(first_command(cx), "workbench.split-horizontal");
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "s p l i t r i g h t enter");
    cx.run_until_parked();
    cx.read(|cx| assert!(shell.read(cx).command_palette.is_none()));
    assert_eq!(first_command(cx), "workbench.split-horizontal");
    cx.read(|cx| {
        assert_eq!(
            cx.global::<crate::app::command_history::ApplicationCommandHistory>()
                .0
                .recent(),
            ["workbench.split-horizontal"]
        );
    });
}
