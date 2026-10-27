//! Launch gating, personal config phases, and installed-extension startup.
use super::*;

#[test]
fn launch_gate_keeps_requests_ordered_and_discards_them_on_failure() {
    let first =
        OpenRequest::from_uri_for_product(ResourceUri::parse("file:///queued-first.txt").unwrap());
    let second =
        OpenRequest::from_uri_for_product(ResourceUri::parse("file:///queued-second.txt").unwrap());
    let mut gate = LaunchGate::Pending(Vec::new());
    assert!(gate.request(Some(first.clone())).is_none());
    assert!(gate.request(Some(second.clone())).is_none());
    assert_eq!(gate.complete(), vec![Some(first), Some(second.clone())]);
    assert_eq!(gate.request(None), Some(None));

    let mut failed = LaunchGate::Pending(Vec::new());
    failed.request(None);
    failed.fail();
    assert!(failed.complete().is_empty());
    assert!(failed.request(Some(second)).is_none());

    let mut empty = LaunchGate::Pending(Vec::new());
    assert!(empty.complete().is_empty());
}

#[gpui::test]
fn personal_config_gates_product_windows_and_keeps_extension_failures_independent(
    cx: &mut TestAppContext,
) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.pre', () => {});",
    )
    .unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.post', () => {});",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/good",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; const { commands } = knot; await commands.register('extension.good', () => {});",
        )],
    );
    install_extension(
        extensions.path(),
        "@example/bad",
        &[],
        &[("main.js", "throw new Error('package failed');")],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extension_host::install);
    cx.update(|cx| super::super::dispatch_launch_request(None, cx));
    assert!(cx.windows().is_empty());
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    let report = cx
        .read(super::super::super::extension_host::startup_report)
        .unwrap();
    assert!(
        report
            .report
            .entries
            .iter()
            .any(|entry| entry.name.as_deref() == Some("@example/good")
                && entry.result == super::super::super::extension_load::LoadResult::Loaded)
    );
    assert!(report.report.entries.iter().any(|entry| entry.name.as_deref() == Some("@example/bad") && matches!(&entry.result, super::super::super::extension_load::LoadResult::Failed(cause) if cause.contains("package failed"))));
    let names = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    for name in ["config.pre", "extension.good", "config.post"] {
        assert!(names.contains(&name.to_owned()), "missing {name}");
    }
    cx.update(|cx| super::super::dispatch_launch_request(None, cx));
    assert_eq!(cx.windows().len(), 2);
}

#[gpui::test]
fn queued_file_open_runs_only_after_configured_startup(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    let file = config.path().join("queued.txt");
    std::fs::write(&file, "queued contents").unwrap();
    let request = OpenRequest::from_path(&file, config.path()).unwrap();
    let uri = request.uri().clone();
    let documents = install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extension_host::install);
    cx.update(|cx| super::super::dispatch_launch_request(Some(request), cx));
    assert!(cx.windows().is_empty());
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        let text = cx.read(|cx| {
            let documents = documents.read(cx);
            documents
                .document_for_resource(&uri)
                .and_then(|id| documents.get(id))
                .map(|document| document.model().read(cx).text().to_owned())
        });
        if let Some(text) = text {
            assert_eq!(text, "queued contents");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "queued file did not open"
        );
        std::thread::yield_now();
    }
}

#[gpui::test]
fn pre_init_failure_skips_extensions_and_opens_only_the_error_window(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("pre-init.js"), "import * as knot from 'knot'; const { commands } = knot; await commands.register('config.before-failure', () => {}); throw new Error('pre failed');").unwrap();
    install_extension(
        extensions.path(),
        "@example/skipped",
        &[],
        &[("main.js", "throw new Error('extension ran');")],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extension_host::install);
    let queued = OpenRequest::from_uri_for_product(
        ResourceUri::parse("file:///never-open-on-failure.txt").unwrap(),
    );
    cx.update(|cx| super::super::dispatch_launch_request(Some(queued.clone()), cx));
    assert!(cx.windows().is_empty());
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    assert!(
        cx.read(super::super::super::extension_host::startup_report)
            .is_none()
    );
    assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
    assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
    let names = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    assert!(!names.contains(&"config.before-failure".to_owned()));
    cx.update(|cx| super::super::dispatch_launch_request(Some(queued), cx));
    assert_eq!(cx.windows().len(), 1);
}

#[gpui::test]
fn invalid_config_import_prevents_extension_discovery(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("pre-init.js"), "import './missing.js';").unwrap();
    install_extension(
        extensions.path(),
        "@example/skipped",
        &[],
        &[("main.js", "throw new Error('extension ran');")],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    assert!(
        cx.read(super::super::super::extension_host::startup_report)
            .is_none()
    );
    assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
}

#[gpui::test]
fn post_init_failure_unloads_config_and_installed_extensions(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "import * as knot from 'knot'; const { commands, editor } = knot; await commands.register('config.before', () => {}); await editor.registerCompletionProvider('config-provider', { provideCompletions() { return []; } });",
    )
    .unwrap();
    std::fs::write(
        config.path().join("post-init.js"),
        "throw new Error('post failed');",
    )
    .unwrap();
    install_extension(
        extensions.path(),
        "@example/loaded",
        &[],
        &[(
            "main.js",
            "import * as knot from 'knot'; const { commands } = knot; await commands.register('extension.before', () => {});",
        )],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
    let report = cx
        .read(super::super::super::extension_host::startup_report)
        .unwrap();
    assert_eq!(
        report.report.entries[0].result,
        super::super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
    let names = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    assert!(!names.contains(&"config.before".to_owned()));
    assert!(!names.contains(&"extension.before".to_owned()));
    assert_eq!(
        cx.read(super::super::super::extension_host::completion_provider_count),
        0
    );
}

#[gpui::test]
fn post_init_validation_failure_waits_for_installed_extensions(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("pre-init.js"),
        "globalThis.preRan = true;",
    )
    .unwrap();
    std::fs::write(config.path().join("post-init.js"), "import './missing.js';").unwrap();
    install_extension(
        extensions.path(),
        "@example/loaded",
        &[],
        &[("main.js", "export {};")],
    );
    install_globals(cx);
    install_launch_gate(cx);
    let host = cx.update(super::super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(!wait_for_product_startup(cx));
    let report = cx
        .read(super::super::super::extension_host::startup_report)
        .unwrap();
    assert_eq!(
        report.report.entries[0].result,
        super::super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(cx.read(|cx| host.read(cx).lifecycle_count()), 0);
    assert!(cx.read(|cx| cx.global::<ApplicationWorkbenches>().0.borrow().is_empty()));
}

#[gpui::test]
fn installed_api_version_rejection_prevents_evaluation_and_preserves_dependency_diagnostics(
    cx: &mut TestAppContext,
) {
    let root = tempfile::tempdir().unwrap();
    const PROBE: &str = "import {commands, workbench} from 'knot'; await workbench.writeClipboardText('rejected entry evaluated'); await commands.register('rejected-entry', () => {});";
    for (suffix, version) in [
        ("missing", None),
        ("null", Some(serde_json::Value::Null)),
        ("string", Some(serde_json::json!("1"))),
        ("boolean", Some(serde_json::json!(true))),
        ("zero", Some(serde_json::json!(0))),
        ("fraction", Some(serde_json::json!(1.5))),
        ("unsupported", Some(serde_json::json!(2))),
    ] {
        let name = format!("@example/api-{suffix}");
        install_extension(root.path(), &name, &[], &[("main.js", PROBE)]);
        let manifest_path = root.path().join(&name).join("knot.jsonc");
        let mut manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        match version {
            Some(version) => manifest["apiVersion"] = version,
            None => {
                manifest.as_object_mut().unwrap().remove("apiVersion");
            }
        }
        std::fs::write(manifest_path, manifest.to_string()).unwrap();
        install_extension(
            root.path(),
            &format!("@example/dependent-{suffix}"),
            &[&name],
            &[("main.js", PROBE)],
        );
    }
    install_extension(
        root.path(),
        "@example/duplicate",
        &[],
        &[("main.js", PROBE)],
    );
    install_extension(
        root.path(),
        "@example/duplicate-peer",
        &[],
        &[("main.js", PROBE)],
    );
    let duplicate_path = root.path().join("@example/duplicate-peer/knot.jsonc");
    std::fs::write(
        duplicate_path,
        r#"{"name":"@example/duplicate","apiVersion":2,"version":"1.0.0","main":"main.js"}"#,
    )
    .unwrap();
    install_extension(
        root.path(),
        "@example/independent",
        &[],
        &[(
            "main.js",
            "import {commands} from 'knot'; await commands.register('admission.independent', () => {});",
        )],
    );
    let documents = install_globals(cx);
    let host = cx.update(super::super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (_shell, _window) = product_window(document, model, cx);
    cx.update(|cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("admission sentinel".into()))
    });
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    assert_eq!(report.report.entries.len(), 17);
    for suffix in [
        "missing",
        "null",
        "string",
        "boolean",
        "zero",
        "fraction",
        "unsupported",
    ] {
        let rejected = format!("@example/api-{suffix}");
        let dependent = format!("@example/dependent-{suffix}");
        for name in [&rejected, &dependent] {
            let entry = report
                .report
                .entries
                .iter()
                .find(|entry| entry.name.as_ref() == Some(name))
                .unwrap();
            assert!(
                matches!(&entry.result, super::super::super::extension_load::LoadResult::Failed(cause) if cause.contains("apiVersion") && cause.contains("expected") && cause.contains("declared")),
                "{entry:?}"
            );
        }
    }
    let duplicates = report
        .report
        .entries
        .iter()
        .filter(|entry| entry.name.as_deref() == Some("@example/duplicate"))
        .collect::<Vec<_>>();
    assert_eq!(duplicates.len(), 2);
    assert!(duplicates.iter().all(|entry| matches!(&entry.result, super::super::super::extension_load::LoadResult::Failed(cause) if cause.contains("duplicate package ID"))));
    assert!(duplicates.iter().any(|entry| matches!(&entry.result, super::super::super::extension_load::LoadResult::Failed(cause) if cause.contains("unsupported apiVersion"))));
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "admission sentinel"
        );
        assert!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("rejected-entry")
                .is_err()
        );
        assert!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("admission.independent")
                .is_ok()
        );
        assert_eq!(host.read(cx).lifecycle_count(), 1);
    });
}

#[gpui::test]
fn checked_authoring_examples_run_as_an_installed_version_one_package(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/guide",
        &[],
        &[
            (
                "main.js",
                "import {deactivateFeature} from './activation.js'; import './errors.js'; import {commands} from 'knot'; await commands.register('guide.test-deactivate', deactivateFeature);",
            ),
            (
                "activation.js",
                include_str!("../../../../scripts/extensions/examples/activation.js"),
            ),
            (
                "errors.js",
                include_str!("../../../../scripts/extensions/examples/errors.js"),
            ),
        ],
    );
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let original_binding = cx.read(|cx| keymap_command(cx, "cmd-g", Some("editor")));
    let host = cx.update(super::super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "text").unwrap());
    let (shell, window) = product_window(document, model.clone(), cx);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    assert_eq!(
        report.report.entries[0].result,
        super::super::super::extension_load::LoadResult::Loaded
    );
    cx.read(|cx| {
        assert_eq!(
            keymap_command(cx, "cmd-g", Some("editor")).as_deref(),
            Some("guide.compose")
        )
    });
    let annotate = dispatch_product_command(&shell, window, "guide.compose", cx);
    assert_eq!(wait_for_command(annotate, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(model.read(cx).resolved_contributions().len(), 1));
    let invalid = dispatch_product_command(&shell, window, "guide.insert", cx);
    assert!(matches!(
        wait_for_command(invalid, cx),
        CommandOutcome::InvalidArgument { .. }
    ));
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let insertion = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: "guide.insert".into(),
                arguments: CommandArgumentValue::String("!".into()),
            },
            target,
            cx,
        )
    });
    assert_eq!(wait_for_command(insertion, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(model.read(cx).text(), "!text"));
    let native_child = dispatch_product_command(&shell, window, "guide.select-all", cx);
    assert_eq!(
        wait_for_command(native_child, cx),
        CommandOutcome::Completed
    );
    let native_copy = dispatch_product_command(&shell, window, super::super::COPY_COMMAND, cx);
    assert_eq!(wait_for_command(native_copy, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "!text"));
    let tree = wait_for_contributed_view(&shell, "guide.outline", cx);
    wait_for_api_condition(cx, |cx| tree.read(cx).root_labels() == ["Guide"]);
    let copy_editor = cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .and_then(|pane| pane.active_tab().editor().cloned())
            .expect("document editor is focused before the tree")
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve_view(tree.read(cx).instance_id(), "copy")
            .is_some()
    });
    cx.update_window(window, |_, window, cx| {
        tree.update(cx, |tree, cx| assert!(tree.select_item("root", cx)));
        tree.focus_handle(cx).focus(window);
    })
    .unwrap();
    let copy = dispatch_tree_copy(&shell, window, cx);
    assert_eq!(wait_for_command(copy, cx), CommandOutcome::Completed);
    cx.read(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "Guide"));
    wait_for_api_condition(cx, |cx| {
        super::super::super::extension_host::completion_provider_count(cx) == 1
    });
    cx.update_window(window, |_, window, cx| {
        copy_editor.read(cx).focus_handle(cx).focus(window);
        shell.update(cx, |shell, cx| shell.sync_focused_pane(window, cx));
    })
    .unwrap();
    let completion =
        dispatch_product_command(&shell, window, super::super::SHOW_COMPLETIONS_COMMAND, cx);
    assert_eq!(wait_for_command(completion, cx), CommandOutcome::Completed);
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
    wait_for_api_condition(cx, |cx| {
        editor
            .read(cx)
            .completion_state()
            .is_some_and(|(items, pending, failures, _)| {
                items == 1 && pending == 0 && failures == 0
            })
    });
    let cleanup = dispatch_product_command(&shell, window, "guide.test-deactivate", cx);
    assert_eq!(wait_for_command(cleanup, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        assert!(!tree.read(cx).lifecycle_state().0);
        assert!(model.read(cx).resolved_contributions().is_empty());
        assert_eq!(
            keymap_command(cx, "cmd-g", Some("editor")),
            original_binding
        );
        assert_eq!(
            super::super::super::extension_host::completion_provider_count(cx),
            0
        );
        assert!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .catalog()
                .borrow()
                .resolve("guide.compose")
                .is_err()
        );
    });
}

#[gpui::test]
fn on_disk_extensions_load_in_order_invoke_dependencies_and_isolate_failures(
    cx: &mut TestAppContext,
) {
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/base",
        &[],
        &[
            (
                "dist/main.js",
                r#"
            import * as knot from 'knot'; const { commands, editor, workbench } = knot;
            import { prefix } from './words.js';
            await commands.register('example.base', async ({ buffer }) => {
                const snapshot = await buffer.snapshot();
                await buffer.applyEdits([{
                    range: { startByteOffset: 0, endByteOffset: 0 },
                    text: prefix,
                }], { ifRevision: snapshot.revision });
            });
            await editor.registerCompletionProvider('example-provider', {
                provideCompletions() { return [{ label: 'Installed', insertText: 'Installed' }]; }
            });
        "#,
            ),
            ("dist/words.js", "export const prefix = 'base ';"),
        ],
    );
    install_extension(
        root.path(),
        "@example/dependent",
        &["@example/base"],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands } = knot;
            await commands.register('example.dependent', async () => {
                await commands.invoke('example.base', null);
            });
        "#,
        )],
    );
    install_extension(
        root.path(),
        "@example/broken",
        &[],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands, editor, workbench } = knot;
            await commands.register('example.shared', async () => {});
            await editor.registerCompletionProvider('broken-provider', {
                provideCompletions() { return []; }
            });
            await workbench.registerView({
                id: 'example.broken.outline',
                title: 'Broken outline',
                content: { kind: 'tree', provider: { getChildren() { return []; } } },
            });
            await commands.registerForView('example.broken.outline', 'copy', async () => {});
            throw new Error('broken entry');
        "#,
        )],
    );
    install_extension(
        root.path(),
        "@example/blocked",
        &["@example/broken"],
        &[("main.js", "throw new Error('must not run');")],
    );
    install_extension(
        root.path(),
        "@example/independent",
        &[],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands } = knot;
            await commands.register('example.shared', async () => {});
        "#,
        )],
    );
    let invalid = root.path().join("@example/invalid");
    std::fs::create_dir_all(&invalid).unwrap();
    std::fs::write(invalid.join("knot.jsonc"), "{ invalid").unwrap();

    let documents = install_globals(cx);
    let host = cx.update(super::super::super::extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    cx.read(|cx| assert!(shell.read(cx).contributed_views.is_empty()));
    let result = |name| {
        report
            .report
            .entries
            .iter()
            .find(|entry| entry.name.as_deref() == Some(name))
            .unwrap()
            .result
            .clone()
    };
    assert_eq!(
        result("@example/base"),
        super::super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(
        result("@example/dependent"),
        super::super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(
        result("@example/independent"),
        super::super::super::extension_load::LoadResult::Loaded
    );
    assert!(
        matches!(result("@example/broken"), super::super::super::extension_load::LoadResult::Failed(cause) if cause.contains("broken entry"))
    );
    assert!(
        matches!(result("@example/blocked"), super::super::super::extension_load::LoadResult::Failed(cause) if cause.contains("@example/broken"))
    );
    assert!(report.report.entries.iter().any(|entry| entry.directory == invalid && matches!(&entry.result, super::super::super::extension_load::LoadResult::Failed(cause) if cause.contains("invalid knot.jsonc"))));
    let definitions = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    assert!(definitions.contains(&"example.base".to_owned()));
    assert!(definitions.contains(&"example.dependent".to_owned()));
    assert!(definitions.contains(&"example.shared".to_owned()));
    assert_eq!(
        definitions
            .iter()
            .filter(|name| name.as_str() == "example.shared")
            .count(),
        1
    );
    cx.read(|cx| assert!(shell.read(cx).contributed_views.is_empty()));
    let mut execution = dispatch_product_command(&shell, window, "example.dependent", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        match execution.completion.try_recv() {
            Ok(outcome) => {
                assert_eq!(outcome, CommandOutcome::Completed);
                break;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "dependent command did not finish"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("dependent command completion closed: {error}"),
        }
    }
    assert_eq!(cx.read(|cx| model.read(cx).text()), "base ");
    assert_eq!(
        cx.read(super::super::super::extension_host::completion_provider_count),
        1
    );

    let mut report_command =
        dispatch_product_command(&shell, window, SHOW_EXTENSION_REPORT_COMMAND, cx);
    cx.run_until_parked();
    assert_eq!(
        report_command.completion.try_recv().unwrap(),
        CommandOutcome::Completed
    );
    assert!(cx.read(|cx| shell.read(cx).extension_report_open));
}

#[gpui::test]
fn extension_startup_shows_known_outcomes_while_a_later_entry_is_pending(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/a-ready",
        &[],
        &[("main.js", "export {};")],
    );
    install_extension(
        root.path(),
        "@example/z-pending",
        &[],
        &[("main.js", "await new Promise(() => {});")],
    );
    let invalid = root.path().join("@example/invalid");
    std::fs::create_dir_all(&invalid).unwrap();
    std::fs::write(invalid.join("knot.jsonc"), "{ invalid").unwrap();

    install_globals(cx);
    let host = cx.update(super::super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        let report = cx
            .read(super::super::super::extension_host::startup_report)
            .unwrap();
        if report.report.entries.len() == 2 {
            assert_eq!(
                report.state,
                super::super::super::extension_host::StartupState::Loading
            );
            assert_eq!(report.report.entries[0].directory, invalid);
            assert!(matches!(
                &report.report.entries[0].result,
                super::super::super::extension_load::LoadResult::Failed(cause)
                    if cause.contains("invalid knot.jsonc")
            ));
            assert_eq!(
                report.report.entries[1].name.as_deref(),
                Some("@example/a-ready")
            );
            assert_eq!(
                report.report.entries[1].result,
                super::super::super::extension_load::LoadResult::Loaded
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "startup outcomes were not published"
        );
        std::thread::yield_now();
    }
}

#[gpui::test]
fn extension_startup_rescans_changed_directories_and_cleans_old_registrations(
    cx: &mut TestAppContext,
) {
    let root = tempfile::tempdir().unwrap();
    install_extension(
        root.path(),
        "@example/old",
        &[],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands, editor } = knot;
            await commands.register('example.old', async () => {});
            await editor.registerCompletionProvider('old-provider', {
                provideCompletions() { return []; }
            });
        "#,
        )],
    );
    install_globals(cx);
    let host = cx.update(super::super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let first = wait_for_extension_report(&host, cx);
    assert_eq!(first.report.entries.len(), 1);
    assert_eq!(
        first.report.entries[0].name.as_deref(),
        Some("@example/old")
    );
    assert_eq!(
        cx.read(super::super::super::extension_host::completion_provider_count),
        1
    );

    std::fs::remove_dir_all(root.path().join("@example/old")).unwrap();
    install_extension(
        root.path(),
        "@example/new",
        &[],
        &[(
            "main.js",
            r#"
            import * as knot from 'knot'; const { commands } = knot;
            await commands.register('example.new', async () => {});
        "#,
        )],
    );
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.path().to_path_buf(), cx)
    });
    let second = wait_for_extension_report(&host, cx);
    assert_eq!(second.report.entries.len(), 1);
    assert_eq!(
        second.report.entries[0].name.as_deref(),
        Some("@example/new")
    );
    assert_eq!(
        second.report.entries[0].result,
        super::super::super::extension_load::LoadResult::Loaded
    );
    assert_eq!(
        cx.read(super::super::super::extension_host::completion_provider_count),
        0
    );
    let definitions = cx.read(|cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .definitions()
            .map(|definition| definition.name.to_string())
            .collect::<Vec<_>>()
    });
    assert!(definitions.contains(&"example.new".to_owned()));
    assert!(!definitions.contains(&"example.old".to_owned()));
}

#[gpui::test]
fn checked_in_extension_example_runs_through_the_product_command_bridge(cx: &mut TestAppContext) {
    let storage = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/extensions");
    let documents = install_globals(cx);
    let host = cx.update(super::super::super::extension_host::install);
    host.update(cx, |host, cx| {
        host.set_test_storage_root(storage.path().to_owned(), cx)
    });
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    host.update(cx, |host, cx| {
        host.start_installed_extensions(root.clone(), cx)
    });
    let report = wait_for_extension_report(&host, cx);
    assert_eq!(report.report.entries.len(), 5);
    assert!(
        report
            .report
            .entries
            .iter()
            .all(|entry| entry.result == super::super::super::extension_load::LoadResult::Loaded)
    );
    let mut execution = dispatch_product_command(&shell, window, "example.greet", cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        match execution.completion.try_recv() {
            Ok(outcome) => {
                assert_eq!(outcome, CommandOutcome::Completed);
                break;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "example command did not finish"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("example command completion closed: {error}"),
        }
    }
    assert_eq!(cx.read(|cx| model.read(cx).text()), "Hello ");
    for activations in 1..=2 {
        if activations == 2 {
            host.update(cx, |host, cx| {
                host.start_installed_extensions(root.clone(), cx)
            });
            let report = wait_for_extension_report(&host, cx);
            assert!(report.report.entries.iter().all(
                |entry| entry.result == super::super::super::extension_load::LoadResult::Loaded
            ));
        }
        let execution = dispatch_product_command(&shell, window, "example.storage-report", cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        let report: serde_json::Value = cx.read(|cx| {
            serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap()
        });
        assert_eq!(report["activations"]["found"], true);
        assert_eq!(
            report["activations"]["value"].as_f64(),
            Some(f64::from(activations))
        );
        assert_eq!(
            report["example"]["value"]["optional"],
            serde_json::Value::Null
        );
        assert_eq!(report["missing"], serde_json::json!({"found": false}));
        assert_eq!(
            report["keys"],
            serde_json::json!(["activations", "example"])
        );
    }
}
