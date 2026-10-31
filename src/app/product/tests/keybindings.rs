//! Keymap precedence and script keybindings across config, extensions, and native defaults.
use super::*;

#[gpui::test]
fn script_keybindings_follow_config_phases_and_runtime_replacement(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; knot.keymaps.bind('cmd-n', 'file.open');",
    )
    .unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; knot.keymaps.bind('cmd-n', 'file.save-as'); knot.keymaps.bind('cmd-x', null); knot.keymaps.bind('cmd-j', { name: 'test.args', arguments: { marker: 42 } });",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/good",
        &[],
        &[(
            "main.js",
            r#"
                import * as knot from 'knot';
                knot.keymaps.bind('cmd-n', 'file.save');
                knot.keymaps.bind('cmd-r', 'file.open');
                await knot.commands.register('test.remap', () => {
                    knot.keymaps.bind('cmd-r', 'file.new');
                    knot.keymaps.unbind('cmd-n');
                });
                await knot.commands.register('test.unbind', () => knot.keymaps.bind('cmd-r', null, { view: 'editor' }));
                await knot.commands.register('test.restore', () => knot.keymaps.unbind('cmd-r', { view: 'editor' }));
                await knot.commands.register('test.remove', () => knot.keymaps.unbind('cmd-r'));
                await knot.commands.register('test.args', ({ arguments: args }) => {
                    if (args.marker !== 42) throw new Error('wrong binding arguments');
                });
                await knot.commands.register('test.invalid', () => {
                    knot.keymaps.bind('cmd-r', 'file.save');
                    knot.keymaps.bind('bad-modifier-key', 'file.new');
                });
            "#,
        )],
    );
    install_extension(
        extensions.path(),
        "@example/bad",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; knot.keymaps.bind('cmd-l', 'file.new'); await knot.workbench.writeClipboardText('startup'); throw new Error('failed after binding');",
        )],
    );
    install_globals(cx);
    cx.update(super::super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    cx.read(|cx| {
        assert_eq!(keymap_command(cx, "cmd-n", Some("editor")).as_deref(), Some("file.save-as"));
        assert_eq!(keymap_command(cx, "cmd-r", Some("editor")).as_deref(), Some("file.open"));
        assert_eq!(keymap_command(cx, "cmd-x", Some("editor")).as_deref(), Some("<unbound>"));
        assert_eq!(keymap_command(cx, "cmd-l", Some("editor")), None);
        assert!(matches!(
            cx.global::<ApplicationKeymaps>().0.resolve(&gpui::Keystroke::parse("cmd-j").unwrap().unparse(), Some("editor")),
            Some(BindingResolution::Command(Command { arguments: CommandArgumentValue::Object(arguments), .. }))
                if arguments.get("marker") == Some(&CommandArgumentValue::Number(42.0))
        ));
    });
    let report = cx
        .read(super::super::super::extensions::host::startup_report)
        .unwrap();
    assert!(report.report.entries.iter().any(|entry| {
        entry.name.as_deref() == Some("@example/bad")
            && matches!(&entry.result, super::super::super::extensions::load::LoadResult::Failed(cause) if cause.contains("failed after binding"))
    }));
    assert!(cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .any(|definition| definition.name.as_ref() == "test.args")
    }));
    let documents = cx.read(|cx| cx.global::<ApplicationDocuments>().0.clone());
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let command = cx.read(|cx| {
        let Some(BindingResolution::Command(command)) =
            cx.global::<ApplicationKeymaps>().0.resolve(
                &gpui::Keystroke::parse("cmd-j").unwrap().unparse(),
                Some("editor"),
            )
        else {
            panic!("expected command binding");
        };
        command
    });
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(command, target, cx)
        })
    });
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    let execution = dispatch_product_command(&shell, window, "test.remap", cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("file.new")
        );
        assert_eq!(
            keymap_command(cx, "cmd-n", Some("editor")).as_deref(),
            Some("file.save-as")
        );
    });
    let invalid = wait_for_command(
        dispatch_product_command(&shell, window, "test.invalid", cx),
        cx,
    );
    assert!(
        matches!(invalid, CommandOutcome::HandlerFailure { message } if message.contains("keybinding operation 2"))
    );
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("file.new")
        )
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "test.unbind", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("<unbound>")
        );
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("terminal")).as_deref(),
            Some("file.new")
        );
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "test.restore", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("file.new")
        )
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "test.remove", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    cx.read(|cx| assert_eq!(keymap_command(cx, "cmd-r", Some("editor")), None));
}

#[gpui::test]
fn config_remaps_product_keys_without_changing_menu_or_palette_dispatch(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; knot.keymaps.bind('cmd-n', null); knot.keymaps.bind('cmd-g', 'file.new');",
    )
    .unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; knot.keymaps.bind('cmd-r', 'test.mark');",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/mark",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; await knot.commands.register('test.mark', () => knot.keymaps.bind('cmd-y', 'file.new'));",
        )],
    );
    install_globals(cx);
    cx.update(super::super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    let window = cx.windows()[0];
    let shell = cx
        .update_window(window, |_, window, _| {
            window.root::<ProductShell>().flatten()
        })
        .unwrap()
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.refresh().unwrap();

    cx.simulate_keystrokes(window, "cmd-n");
    cx.run_until_parked();
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
            1
        )
    });
    cx.simulate_keystrokes(window, "cmd-g");
    cx.run_until_parked();
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
        )
    });

    cx.simulate_keystrokes(window, "cmd-r");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| keymap_command(cx, "cmd-y", Some("editor")).as_deref() == Some("file.new"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "config binding did not invoke the extension command"
        );
        std::thread::yield_now();
    }

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
            3
        )
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.open_command_palette(window, cx));
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "f i l e . n e w enter");
    cx.run_until_parked();
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
            4
        )
    });
}

#[gpui::test]
fn config_global_copy_binding_routes_to_editor_and_extension_view(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;

    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; knot.keymaps.bind('cmd-j', 'copy');",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/outline-copy",
        &[],
        &[(
            "main.js",
            r#"
                import * as knot from 'knot';
                await knot.workbench.registerView({
                    id: 'example.outline',
                    title: 'Outline',
                    content: { kind: 'tree', provider: {
                        getChildren(parentId) {
                            return parentId === null
                                ? [{ id: 'first', label: 'Tree item', collapsibleState: 'none' }]
                                : [];
                        },
                    } },
                });
                await knot.commands.registerForView('example.outline', 'copy', async () => {
                    const text = await knot.workbench.selectedText();
                    await knot.workbench.writeClipboardText(text);
                });
            "#,
        )],
    );
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-j", Some("editor")).as_deref(),
            Some("copy")
        );
        assert_eq!(
            keymap_command(cx, "cmd-j", Some("example.outline")).as_deref(),
            Some("copy")
        );
    });
    let window = cx.windows()[0];
    let shell = cx
        .update_window(window, |_, window, _| {
            window.root::<ProductShell>().flatten()
        })
        .unwrap()
        .unwrap();
    let tree = wait_for_contributed_view(&shell, "example.outline", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| tree.read(cx).root_labels() == ["Tree item"]) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "outline did not load");
        std::thread::yield_now();
    }

    cx.update_window(window, |_, window, cx| {
        let editor = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "Editor selection", window, cx);
            editor.execute_editing_command("editor.select-all", window, cx);
        });
        editor.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "cmd-j");
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("Editor selection")
        );
    });

    cx.update_window(window, |_, window, cx| {
        tree.update(cx, |tree, cx| assert!(tree.select_item("first", cx)));
        tree.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("untouched".into())));
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "cmd-j");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref()
                == Some("Tree item")
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "global copy binding did not route to the outline"
        );
        std::thread::yield_now();
    }
    cx.read(|cx| assert_eq!(documents.read(cx).documents().count(), 1));
}

#[gpui::test]
fn invalid_config_keybinding_batch_rolls_back_before_product_readiness(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; knot.keymaps.bind('cmd-r', 'file.open'); knot.keymaps.bind('bad-modifier-key', 'file.new');",
    )
    .unwrap();
    install_globals(cx);
    cx.update(super::super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    cx.read(|cx| {
        assert_eq!(keymap_command(cx, "cmd-r", Some("editor")), None);
        assert_eq!(host.read(cx).lifecycle_count(), 0);
        assert!(cx.global::<ApplicationWorkbenches>().0.borrow().is_empty());
    });
    assert_eq!(cx.windows().len(), 1);
}

#[gpui::test]
fn invalid_extension_keybinding_batch_does_not_block_other_extensions(cx: &mut TestAppContext) {
    let extensions = tempfile::tempdir().unwrap();
    install_extension(
        extensions.path(),
        "@example/good",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; knot.keymaps.bind('cmd-r', 'file.new');",
        )],
    );
    install_extension(
        extensions.path(),
        "@example/bad",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; knot.keymaps.bind('cmd-l', 'file.new'); knot.keymaps.bind('bad-modifier-key', 'file.open');",
        )],
    );
    install_globals(cx);
    cx.update(super::super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(None, Ok(extensions.path().to_path_buf()), cx)
    });
    assert!(wait_for_product_startup(cx));
    let report = cx
        .read(super::super::super::extensions::host::startup_report)
        .unwrap();
    assert!(report.report.entries.iter().any(|entry| {
        entry.name.as_deref() == Some("@example/good")
            && entry.result == super::super::super::extensions::load::LoadResult::Loaded
    }));
    assert!(report.report.entries.iter().any(|entry| {
        entry.name.as_deref() == Some("@example/bad")
            && matches!(&entry.result, super::super::super::extensions::load::LoadResult::Failed(cause) if cause.contains("keybinding operation 2"))
    }));
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", Some("editor")).as_deref(),
            Some("file.new")
        );
        assert_eq!(keymap_command(cx, "cmd-l", Some("editor")), None);
    });
}

#[gpui::test]
fn unloading_a_script_lifecycle_removes_its_keybindings(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let host = cx.update(super::super::super::extensions::host::install);
    let key = ExtensionKey::new(ExtensionId::new(91), ExtensionLifecycleId::new(1));
    let loaded = Arc::new(Mutex::new(false));
    let done = loaded.clone();
    host.update(cx, |host, cx| {
        let execution = host
            .load_test_source(
                key,
                "import * as knot from 'knot'; knot.keymaps.bind('cmd-r', 'file.open');",
            )
            .unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
            *done.lock().unwrap() = true;
        })
        .detach();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !*loaded.lock().unwrap() {
        cx.run_until_parked();
        assert!(
            std::time::Instant::now() < deadline,
            "extension keybinding did not load"
        );
        std::thread::yield_now();
    }
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", None).as_deref(),
            Some("file.open")
        )
    });
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    cx.read(|cx| assert_eq!(keymap_command(cx, "cmd-r", None), None));
}

#[gpui::test]
async fn keybinding_adapter_enters_the_product_dispatcher(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();

    cx.simulate_keystrokes(window_handle, "secondary-n");
    cx.run_until_parked();

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
            Some(&CommandOutcome::Completed)
        );
    });
}

#[gpui::test]
fn personal_global_binding_overrides_editor_default_in_every_window(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let mut windows = Vec::new();
    for _ in 0..2 {
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        cx.update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx)
        })
        .unwrap();
        windows.push((window, model));
    }
    cx.update(|cx| {
        let changed = cx
            .global_mut::<ApplicationKeymaps>()
            .0
            .set(
                BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                "Enter",
                None,
                Some(Command {
                    name: super::super::super::commands::dispatch::INSERT_TAB_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                }),
            )
            .unwrap();
        assert!(changed);
        super::super::super::commands::keymaps::rebuild(cx);
    });
    cx.refresh().unwrap();
    for (window, _) in &windows {
        cx.simulate_keystrokes(*window, "enter");
    }
    cx.run_until_parked();
    cx.read(|cx| {
        for (_, model) in &windows {
            assert_eq!(model.read(cx).text(), "\t");
        }
    });
}

#[gpui::test]
fn unbinding_an_unmapped_letter_preserves_editor_input(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.update(|cx| {
        let owner = BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1));
        assert!(
            cx.global_mut::<ApplicationKeymaps>()
                .0
                .set(owner, "a", Some("editor"), None)
                .unwrap()
        );
        super::super::super::commands::keymaps::rebuild(cx);
    });
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "a");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "a"));
}

#[gpui::test]
async fn editor_unbind_leaves_global_binding_available_in_terminal(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    // A named key, because Windows and Linux terminals leave ctrl with a
    // printable character to the shell.
    cx.update(|cx| {
        let registry = &mut cx.global_mut::<ApplicationKeymaps>().0;
        let owner = BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1));
        registry
            .set(
                owner,
                "f9",
                None,
                Some(Command {
                    name: NEW_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                }),
            )
            .unwrap();
        assert!(registry.set(owner, "f9", Some("editor"), None).unwrap());
        super::super::super::commands::keymaps::rebuild(cx);
    });
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "f9");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 1));

    let terminal = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
    assert_eq!(
        terminal.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "f9");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 3));
}

#[gpui::test]
fn sequence_prefix_dispatches_the_product_command(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "secondary-k right");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).panes().len(), 2));
}

#[gpui::test]
fn unavailable_high_priority_binding_does_not_invoke_native_default(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.update(|cx| {
        assert!(
            cx.global_mut::<ApplicationKeymaps>()
                .0
                .set(
                    BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                    "secondary-n",
                    None,
                    Some(Command {
                        name: "missing.command".into(),
                        arguments: CommandArgumentValue::Null,
                    }),
                )
                .unwrap()
        );
        super::super::super::commands::keymaps::rebuild(cx);
    });
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "secondary-n");
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(shell.read(cx).workbench.read(cx).panes()[0].tabs().len(), 1);
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Unavailable),
        );
    });
}

#[cfg(windows)]
#[gpui::test]
fn windows_native_ctrl_shift_p_opens_command_palette(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "ctrl-shift-p");
    cx.run_until_parked();
    cx.read(|cx| assert!(shell.read(cx).command_palette.is_some()));
}

#[gpui::test]
fn personal_config_ctrl_shift_p_opens_command_palette(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; knot.keymaps.bind('ctrl-shift-p', 'workbench.show-command-palette');",
    )
    .unwrap();
    install_globals(cx);
    cx.update(super::super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    let window = cx.windows()[0];
    let shell = cx
        .update_window(window, |_, window, _| {
            window.root::<ProductShell>().flatten()
        })
        .unwrap()
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "ctrl-shift-p");
    cx.run_until_parked();
    cx.read(|cx| assert!(shell.read(cx).command_palette.is_some()));
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn macos_emacs_keys_edit_through_the_knot_preset(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "hello world").unwrap());
    let (shell, window) = product_window(document, model.clone(), cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "ctrl-e ctrl-b ctrl-b ctrl-h ctrl-a ctrl-f ctrl-k");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "h"));
}

#[gpui::test]
async fn linux_preset_leaves_plain_control_keys_to_the_terminal(cx: &mut TestAppContext) {
    use super::super::super::commands::keymaps::{BindingRegistry, Platform, knot_preset};
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update(|cx| {
        let mut registry = BindingRegistry::new();
        registry.replace_preset(&knot_preset(Platform::Linux));
        cx.global_mut::<ApplicationKeymaps>().0 = registry;
        super::super::super::commands::keymaps::rebuild(cx);
    });
    let terminal = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
    assert_eq!(
        terminal.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.refresh().unwrap();
    let tabs = |cx: &mut TestAppContext| {
        cx.read(|cx| shell.read(cx).workbench.read(cx).panes()[0].tabs().len())
    };
    assert_eq!(tabs(cx), 2);
    cx.simulate_keystrokes(window, "ctrl-w");
    cx.run_until_parked();
    assert_eq!(tabs(cx), 2);

    cx.simulate_keystrokes(window, "ctrl-shift-p");
    cx.run_until_parked();
    cx.read(|cx| assert!(shell.read(cx).command_palette.is_some()));
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn selected_contributed_preset_replaces_knot_beneath_extension_bindings(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        r#"import * as knot from 'knot';
        await knot.keymaps.registerPreset({ name: 'test', title: 'Test', bindings: [
            { key: 'cmd-r', command: 'file.new' },
            { key: 'cmd-j', command: 'file.open' },
        ] });
        knot.keymaps.bind('cmd-j', 'file.save-as');
        await knot.settings.set('keymap.preset', 'test');
        await knot.commands.register('test.knot', () => knot.settings.set('keymap.preset', 'knot'));"#,
    )
    .unwrap();
    install_globals(cx);
    cx.update(super::super::bind_product_keys);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extensions::host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-r", None).as_deref(),
            Some("file.new")
        );
        assert_eq!(keymap_command(cx, "cmd-s", None), None);
        assert_eq!(
            keymap_command(cx, "cmd-j", None).as_deref(),
            Some("file.save-as")
        );
    });
    let window = cx.windows()[0];
    let shell = cx
        .update_window(window, |_, window, _| {
            window.root::<ProductShell>().flatten()
        })
        .unwrap()
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx);
    })
    .unwrap();
    let tabs = |cx: &mut TestAppContext| {
        cx.read(|cx| shell.read(cx).workbench.read(cx).panes()[0].tabs().len())
    };
    let before = tabs(cx);
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "cmd-r");
    cx.run_until_parked();
    assert_eq!(tabs(cx), before + 1);

    drop(dispatch_product_command(&shell, window, "test.knot", cx));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| keymap_command(cx, "cmd-s", None).is_some()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "selecting knot did not restore its bindings"
        );
        std::thread::yield_now();
    }
    cx.read(|cx| {
        assert_eq!(keymap_command(cx, "cmd-r", None), None);
        assert_eq!(
            keymap_command(cx, "cmd-s", None).as_deref(),
            Some("file.save")
        );
        assert_eq!(
            keymap_command(cx, "cmd-j", None).as_deref(),
            Some("file.save-as")
        );
    });
}

#[gpui::test]
fn show_keybindings_lists_owners_and_overrides_and_runs_the_accepted_command(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    let save = gpui::Keystroke::parse("secondary-s").unwrap().unparse();
    cx.update(|cx| {
        cx.global_mut::<ApplicationKeymaps>()
            .0
            .set(
                BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1)),
                &save,
                None,
                Some(Command {
                    name: NEW_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                }),
            )
            .unwrap();
    });
    let execution = dispatch_product_command(
        &shell,
        window,
        super::super::commands::SHOW_KEYBINDINGS_COMMAND,
        cx,
    );
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.run_until_parked();
    cx.read(|cx| {
        let picker = match shell.read(cx).transient.as_ref().unwrap().surface.as_ref() {
            Some(crate::app::transient::TransientSurface::Picker(picker)) => picker.read(cx),
            _ => panic!("the transient is not a picker"),
        };
        let rows = picker
            .visible_entries()
            .into_iter()
            .map(|entry| picker.entry(entry).clone())
            .collect::<Vec<_>>();
        let row = |label: &str, owner: &str| {
            rows.iter()
                .find(|row| {
                    row.label == label
                        && row.description.as_deref() == Some(&format!("{save} · {owner}"))
                })
                .unwrap_or_else(|| panic!("no {label} row from {owner}"))
        };
        let overriding = row("New Document", "extension");
        assert_eq!(overriding.detail, None);
        assert_eq!(
            overriding.hint,
            Some(crate::app::commands::keymaps::display_key(&save))
        );
        assert_eq!(
            row("Save", "Knot preset").detail.as_deref(),
            Some("Overridden by extension")
        );
        assert!(rows.iter().any(|row| {
            row.label == "Move to line start"
                && row
                    .description
                    .as_deref()
                    .is_some_and(|description| description.ends_with(" · in editor"))
        }));
    });
    let tabs = |cx: &mut TestAppContext| {
        cx.read(|cx| shell.read(cx).workbench.read(cx).panes()[0].tabs().len())
    };
    let before = tabs(cx);
    cx.simulate_input(window, "New Document");
    cx.simulate_keystrokes(window, "enter");
    cx.run_until_parked();
    assert_eq!(tabs(cx), before + 1);
}
