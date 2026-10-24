//! Product commit-boundary regressions for application metadata observation.

use super::*;
use crate::app::{
    entry::parse_launch_arguments,
    extension_host::{self, ProductExtensionHost},
};
use crate::host::protocol::{ApplicationChange, ApplicationEvent, DocumentInfo, DocumentKind};

fn take_events(
    host: &Entity<ProductExtensionHost>,
    cx: &mut TestAppContext,
) -> Vec<ApplicationEvent> {
    cx.run_until_parked();
    host.update(cx, |host, _| host.take_test_application_events())
}

fn changed_documents(events: &[ApplicationEvent]) -> Vec<&DocumentInfo> {
    events
        .iter()
        .filter_map(|event| match &event.change {
            ApplicationChange::DocumentOpened { document }
            | ApplicationChange::DocumentChanged { document } => Some(document),
            _ => None,
        })
        .collect()
}

fn start_open(
    shell: &Entity<ProductShell>,
    request: OpenRequest,
    replace: bool,
    cx: &mut TestAppContext,
) -> tokio::sync::oneshot::Receiver<CommandOutcome> {
    let target = cx.read(|cx| current_open_target(shell, cx));
    let (completion, receiver) = super::super::CommandCompletion::new();
    shell.update(cx, |shell, cx| {
        shell.start_open_request(request, target, replace, Some(completion), cx)
    });
    receiver
}

fn wait_open(
    mut receiver: tokio::sync::oneshot::Receiver<CommandOutcome>,
    cx: &mut TestAppContext,
) -> CommandOutcome {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        match receiver.try_recv() {
            Ok(outcome) => return outcome,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "native Open did not finish"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("native Open completion closed: {error}"),
        }
    }
}

#[gpui::test]
fn open_entry_points_share_document_identity_and_publish_only_committed_resources(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.txt");
    std::fs::write(&path, "native file").unwrap();
    let cli = parse_launch_arguments(
        ["knot".to_owned(), "notes.txt".to_owned()],
        directory.path(),
    )
    .unwrap()
    .expect("CLI target missing");
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let placeholder_model =
        cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let old_id = cx.read(|cx| {
        documents
            .read(cx)
            .get(placeholder)
            .unwrap()
            .identity()
            .to_string()
    });
    let (shell, window) = product_window(placeholder, placeholder_model.clone(), cx);
    take_events(&host, cx);
    assert_eq!(
        wait_open(start_open(&shell, cli.clone(), true, cx), cx),
        CommandOutcome::Completed
    );
    let opened = take_events(&host, cx);
    let file = changed_documents(&opened)
        .into_iter()
        .find(|document| document.kind == DocumentKind::Persisted)
        .unwrap();
    let id = file.id.clone();
    assert!(!file.dirty);
    assert_eq!(file.title, "notes.txt");
    assert!(opened.iter().any(|event| matches!(&event.change, ApplicationChange::DocumentClosed { document } if document == &old_id)));
    assert!(!cx.read(|cx| placeholder_model.read(cx).is_open()));

    let platform =
        OpenRequest::from_url(url::Url::from_file_path(&path).unwrap().as_str()).unwrap();
    assert_eq!(
        wait_open(start_open(&shell, platform, false, cx), cx),
        CommandOutcome::Completed
    );
    assert!(changed_documents(&take_events(&host, cx)).is_empty());
    let second = cx.update(|cx| create_untitled_document(&documents, cx));
    let second_model = cx.read(|cx| documents.read(cx).get(second).unwrap().model().clone());
    let (second_shell, second_window) = product_window(second, second_model, cx);
    take_events(&host, cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(path),
    ))));
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&second_shell, second_window, super::super::OPEN_COMMAND, cx),
            cx
        ),
        CommandOutcome::Completed
    );
    let presented = take_events(&host, cx);
    assert!(changed_documents(&presented).is_empty());
    assert!(presented.iter().any(|event| matches!(&event.change, ApplicationChange::PresentationsChanged { window } if window.panes.iter().flat_map(|pane| &pane.tabs).any(|tab| tab.document.as_ref() == Some(&id)))));

    let missing =
        OpenRequest::from_path(&directory.path().join("missing.txt"), directory.path()).unwrap();
    assert_eq!(
        wait_open(start_open(&shell, missing, false, cx), cx),
        CommandOutcome::Completed
    );
    let missing_events = take_events(&host, cx);
    assert!(
        changed_documents(&missing_events)
            .iter()
            .any(|document| document.kind == DocumentKind::Destination && document.dirty)
    );
    assert_eq!(
        wait_open(start_open(&shell, cli, false, cx), cx),
        CommandOutcome::Completed
    );
    let activated = take_events(&host, cx);
    assert!(changed_documents(&activated).is_empty());
    assert!(activated.iter().any(|event| matches!(&event.change, ApplicationChange::PresentationsChanged { window } if window.panes.iter().any(|pane| pane.tabs.iter().any(|tab| tab.document.as_ref() == Some(&id) && pane.selected == tab.target && window.current_target.as_ref() == Some(&tab.target))))));
    let invalid_path = directory.path().join("invalid.txt");
    std::fs::write(&invalid_path, [0xff]).unwrap();
    for request in [
        OpenRequest::from_path(&invalid_path, directory.path()).unwrap(),
        OpenRequest::from_uri_for_product(ResourceUri::parse("unknown://provider/file").unwrap()),
    ] {
        assert!(matches!(
            wait_open(start_open(&shell, request, false, cx), cx),
            CommandOutcome::HandlerFailure { .. }
        ));
        assert!(changed_documents(&take_events(&host, cx)).is_empty());
    }
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Cancelled,
    ))));
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, super::super::OPEN_COMMAND, cx),
            cx
        ),
        CommandOutcome::Cancelled
    );
    assert!(changed_documents(&take_events(&host, cx)).is_empty());
}

#[gpui::test]
async fn stale_open_does_not_publish_or_leak_a_document(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let (_, provider) = memory_filesystems_with_provider();
    let (release, gate) = tokio::sync::oneshot::channel();
    let gated = Arc::new(GatedFileSystemProvider {
        inner: provider,
        read_release: std::sync::Mutex::new(Some(gate)),
        replace_release: std::sync::Mutex::new(None),
    });
    let mut filesystems = FileSystemProviderRegistry::new();
    filesystems.register("mem", gated.clone()).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    take_events(&host, cx);
    let pending = start_open(
        &shell,
        OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/notes.txt").unwrap()),
        true,
        cx,
    );
    wait_for_api_condition(cx, |_| gated.read_release.lock().unwrap().is_none());
    assert_eq!(
        start_open(
            &shell,
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/new.txt").unwrap()),
            true,
            cx
        )
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    take_events(&host, cx);
    release.send(()).unwrap();
    assert_eq!(pending.await.unwrap(), CommandOutcome::InvalidTarget);
    assert!(changed_documents(&take_events(&host, cx)).is_empty());
    cx.read(|cx| {
        assert!(
            documents
                .read(cx)
                .document_for_resource(&ResourceUri::parse("mem://product/notes.txt").unwrap())
                .is_none()
        )
    });
}

#[gpui::test]
async fn save_as_and_racing_edits_publish_current_metadata_without_changing_identity(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let (_, provider) = memory_filesystems_with_provider();
    let (release, gate) = tokio::sync::oneshot::channel();
    let gated = Arc::new(GatedFileSystemProvider {
        inner: provider.clone(),
        read_release: std::sync::Mutex::new(None),
        replace_release: std::sync::Mutex::new(Some(gate)),
    });
    let mut filesystems = FileSystemProviderRegistry::new();
    filesystems.register("mem", gated.clone()).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(uri.clone()),
    ))));
    let model = cx.new(|_| BufferModel::from_text("initial"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_untitled("Untitled", model.clone(), cx)
    });
    let (shell, window) = product_window(document, model.clone(), cx);
    let opened = take_events(&host, cx);
    let initial = changed_documents(&opened)
        .into_iter()
        .find(|document| document.title == "Untitled")
        .unwrap()
        .clone();
    model.update(cx, |model, cx| {
        model.replace(0..7, "captured").unwrap();
        cx.notify();
    });
    take_events(&host, cx);
    let save_as = dispatch_product_command(&shell, window, super::super::SAVE_AS_COMMAND, cx);
    wait_for_api_condition(cx, |_| gated.replace_release.lock().unwrap().is_none());
    assert!(changed_documents(&take_events(&host, cx)).is_empty());
    model.update(cx, |model, cx| {
        model.replace(0..8, "newer").unwrap();
        cx.notify();
    });
    let raced = take_events(&host, cx);
    assert!(
        changed_documents(&raced)
            .iter()
            .any(|document| document.id == initial.id
                && document.kind == DocumentKind::Untitled
                && document.revision == 2
                && document.dirty)
    );
    release.send(()).unwrap();
    assert_eq!(save_as.completion.await.unwrap(), CommandOutcome::Completed);
    assert_eq!(provider.read(uri.clone()).await.unwrap().bytes, b"captured");
    let committed = take_events(&host, cx);
    let saved = changed_documents(&committed)
        .into_iter()
        .find(|document| document.id == initial.id)
        .unwrap();
    assert_eq!(saved.buffer, initial.buffer);
    assert_eq!(saved.title, "notes.txt");
    assert_eq!(
        saved.resource_uri.as_deref(),
        Some("mem://product/notes.txt")
    );
    assert_eq!(saved.kind, DocumentKind::Persisted);
    assert_eq!(saved.revision, 2);
    assert!(saved.dirty);
    assert!(!committed.iter().any(|event| matches!(
        event.change,
        ApplicationChange::DocumentOpened { .. } | ApplicationChange::DocumentClosed { .. }
    )));
    assert_eq!(
        dispatch_product_command(&shell, window, SAVE_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let clean = take_events(&host, cx);
    assert!(
        changed_documents(&clean)
            .iter()
            .any(|document| document.id == initial.id && document.revision == 2 && !document.dirty)
    );
    assert_eq!(provider.read(uri.clone()).await.unwrap().bytes, b"newer");
    for (dialog, expected_cancelled) in [
        (SaveDialogOutcome::Cancelled, true),
        (
            SaveDialogOutcome::Selected(
                ResourceUri::parse("mem://product/missing-parent/file.txt").unwrap(),
            ),
            false,
        ),
    ] {
        cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(dialog))));
        let outcome = dispatch_product_command(&shell, window, super::super::SAVE_AS_COMMAND, cx)
            .completion
            .await
            .unwrap();
        if expected_cancelled {
            assert_eq!(outcome, CommandOutcome::Cancelled);
        } else {
            assert!(matches!(outcome, CommandOutcome::HandlerFailure { .. }));
        }
        assert!(changed_documents(&take_events(&host, cx)).is_empty());
    }
}

#[gpui::test]
async fn completed_save_remains_observable_when_a_later_close_prompt_is_cancelled(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let file = provider.read(uri.clone()).await.unwrap();
    let first_model = cx.new(|_| BufferModel::from_text("loaded text"));
    let first = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            first_model.clone(),
            uri.clone(),
            0,
            file.version,
            cx,
        )
    });
    let (shell, window) = product_window(first, first_model.clone(), cx);
    let second_model = cx.new(|_| BufferModel::from_text(""));
    let second = documents.update(cx, |documents, cx| {
        documents.create_untitled("Second", second_model.clone(), cx)
    });
    shell.update(cx, |shell, cx| {
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, cx| {
            workbench
                .open_tab_for_document(pane, second, second_model.clone(), cx)
                .unwrap()
        });
        cx.notify();
    });
    for model in [&first_model, &second_model] {
        model.update(cx, |model, cx| {
            model.replace(0..0, "changed ").unwrap();
            cx.notify();
        });
    }
    let first_identity = cx.read(|cx| {
        documents
            .read(cx)
            .get(first)
            .unwrap()
            .identity()
            .to_string()
    });
    take_events(&host, cx);
    let close = dispatch_product_command(&shell, window, super::super::CLOSE_WINDOW_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Save");
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    assert_eq!(close.completion.await.unwrap(), CommandOutcome::Cancelled);
    let events = take_events(&host, cx);
    assert!(
        changed_documents(&events)
            .iter()
            .any(|document| document.id == first_identity && !document.dirty)
    );
    assert!(!events.iter().any(|event| matches!(
        event.change,
        ApplicationChange::DocumentClosed { .. } | ApplicationChange::WindowClosed { .. }
    )));
    cx.read(|cx| {
        assert!(!documents.read(cx).get(first).unwrap().is_dirty(cx));
        assert!(documents.read(cx).get(second).unwrap().is_dirty(cx));
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(first), 1);
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(second), 1);
    });
}

#[gpui::test]
async fn replaced_or_closed_open_presentations_cannot_accept_in_flight_results(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    for close_window in [false, true] {
        let (_, provider) = memory_filesystems_with_provider();
        let (release, gate) = tokio::sync::oneshot::channel();
        let gated = Arc::new(GatedFileSystemProvider {
            inner: provider,
            read_release: std::sync::Mutex::new(Some(gate)),
            replace_release: std::sync::Mutex::new(None),
        });
        let mut filesystems = FileSystemProviderRegistry::new();
        filesystems.register("mem", gated.clone()).unwrap();
        cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
        let document = cx.update(|cx| create_untitled_document(&documents, cx));
        let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
        let (shell, window) = product_window(document, model.clone(), cx);
        let pending = start_open(
            &shell,
            OpenRequest::from_uri_for_product(
                ResourceUri::parse("mem://product/notes.txt").unwrap(),
            ),
            true,
            cx,
        );
        wait_for_api_condition(cx, |_| gated.read_release.lock().unwrap().is_none());
        if close_window {
            assert_eq!(
                dispatch_product_command(&shell, window, super::super::CLOSE_WINDOW_COMMAND, cx)
                    .completion
                    .await
                    .unwrap(),
                CommandOutcome::Completed
            );
        } else {
            shell.update(cx, |shell, cx| {
                let pane = shell.workbench.read(cx).focused_pane().unwrap();
                let (pane, tab) = (pane.id(), pane.active_tab_id());
                shell.workbench.update(cx, |workbench, cx| {
                    assert!(workbench.replace_tab_document(pane, tab, document, model.clone(), cx))
                });
                cx.notify();
            });
        }
        take_events(&host, cx);
        release.send(()).unwrap();
        assert_eq!(pending.await.unwrap(), CommandOutcome::InvalidTarget);
        assert!(changed_documents(&take_events(&host, cx)).is_empty());
        cx.read(|cx| {
            assert!(
                documents
                    .read(cx)
                    .document_for_resource(&ResourceUri::parse("mem://product/notes.txt").unwrap())
                    .is_none()
            )
        });
    }
}

#[gpui::test]
async fn reordering_retains_exact_targets_while_replacement_and_transfer_change_them(
    cx: &mut TestAppContext,
) {
    use crate::app::workbench::{TabMoveDestination, TabMovePlacement};
    use crate::host::protocol::WindowInfo;
    fn window_state(events: &[ApplicationEvent], handle: gpui::AnyWindowHandle) -> &WindowInfo {
        events
            .iter()
            .rev()
            .find_map(|event| match &event.change {
                ApplicationChange::WindowOpened { window }
                | ApplicationChange::PresentationsChanged { window }
                    if window.target.window == handle.window_id().as_u64().to_string() =>
                {
                    Some(window)
                }
                _ => None,
            })
            .unwrap()
    }
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    assert_eq!(
        dispatch_product_command(&shell, window, super::super::NEW_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let initial_events = take_events(&host, cx);
    let initial = window_state(&initial_events, window);
    let original = initial.panes[0].tabs[0].clone();
    let (source, destination) = cx.read(|cx| {
        let shell = shell.read(cx);
        let workbench = shell.workbench.read(cx);
        let pane = workbench.focused_pane().unwrap();
        (
            shell
                .capture_tab_relocation(pane.id(), pane.tabs()[0].id(), cx)
                .unwrap(),
            super::super::relocate::TabRelocationDestination {
                window,
                workbench: shell.workbench.downgrade(),
                tab: TabMoveDestination::At {
                    anchor: workbench
                        .capture_tab_move(pane.id(), pane.tabs()[1].id())
                        .unwrap(),
                    placement: TabMovePlacement::End,
                },
            },
        )
    });
    assert_eq!(
        cx.update_window(window, |_, window, cx| shell.update(cx, |shell, cx| shell
            .relocate_tab_to(&source, &destination, window, cx)))
            .unwrap(),
        CommandOutcome::Completed
    );
    let reordered_events = take_events(&host, cx);
    let reordered = window_state(&reordered_events, window);
    assert_eq!(reordered.panes[0].tabs[1].target, original.target);
    assert_eq!(reordered.panes[0].tabs[1].buffer, original.buffer);
    assert!(changed_documents(&reordered_events).is_empty());
    let before_replacement = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        assert!(before_replacement.matches_presentation(&shell.workbench, cx));
        shell.workbench.update(cx, |workbench, cx| {
            assert!(workbench.replace_tab_document(
                before_replacement.pane,
                before_replacement.tab,
                document,
                model.clone(),
                cx
            ))
        });
        cx.notify();
    });
    let replaced_events = take_events(&host, cx);
    let replacement = window_state(&replaced_events, window).panes[0].tabs[1].clone();
    assert_ne!(replacement.target, original.target);
    assert_eq!(replacement.document, original.document);
    assert_eq!(replacement.buffer, original.buffer);
    assert!(!cx.read(|cx| before_replacement.matches_presentation(&shell.read(cx).workbench, cx)));
    assert!(changed_documents(&replaced_events).is_empty());
    assert_eq!(
        dispatch_product_command(&shell, window, super::super::SPLIT_HORIZONTAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let old_target = cx.read(|cx| {
        let workbench = &shell.read(cx).workbench;
        let first_pane = &workbench.read(cx).panes()[0];
        let tab = first_pane
            .tabs()
            .iter()
            .find(|tab| tab.document_id() == Some(document))
            .unwrap();
        OpenTarget {
            window,
            workbench: workbench.downgrade(),
            pane: first_pane.id(),
            tab: tab.id(),
            surface: tab.surface_id(),
            presentation: tab.editor().map(Entity::entity_id),
        }
    });
    let (source, destination) = cx.read(|cx| {
        let shell = shell.read(cx);
        let workbench = shell.workbench.read(cx);
        let target_pane = &workbench.panes()[1];
        (
            shell
                .capture_tab_relocation(old_target.pane, old_target.tab, cx)
                .unwrap(),
            super::super::relocate::TabRelocationDestination {
                window,
                workbench: shell.workbench.downgrade(),
                tab: TabMoveDestination::At {
                    anchor: workbench
                        .capture_tab_move(target_pane.id(), target_pane.active_tab_id())
                        .unwrap(),
                    placement: TabMovePlacement::End,
                },
            },
        )
    });
    take_events(&host, cx);
    assert_eq!(
        cx.update_window(window, |_, window, cx| shell.update(cx, |shell, cx| shell
            .relocate_tab_to(&source, &destination, window, cx)))
            .unwrap(),
        CommandOutcome::Completed
    );
    let transferred_events = take_events(&host, cx);
    let moved = window_state(&transferred_events, window)
        .panes
        .iter()
        .flat_map(|pane| &pane.tabs)
        .find(|tab| tab.target.presentation == replacement.target.presentation)
        .unwrap();
    assert_eq!(moved.buffer, original.buffer);
    assert_ne!(moved.target.pane, replacement.target.pane);
    assert!(!cx.read(|cx| old_target.matches_presentation(&shell.read(cx).workbench, cx)));
    assert!(changed_documents(&transferred_events).is_empty());
}

#[gpui::test]
fn empty_workbench_and_clean_untitled_documents_keep_their_actual_metadata(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let handle = cx.update(open_tab_transfer_window).unwrap();
    let window = handle.into();
    let opened = take_events(&host, cx);
    assert!(opened.iter().any(|event| matches!(&event.change, ApplicationChange::WindowOpened { window } if window.panes.is_empty() && window.current_target.is_none())));
    assert!(changed_documents(&opened).is_empty());
    let shell = cx
        .update_window(window, |_, window, _| {
            window.root::<ProductShell>().flatten().unwrap()
        })
        .unwrap();
    let model = cx.new(|_| BufferModel::from_text("clean"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_untitled("Untitled", model.clone(), cx)
    });
    shell.update(cx, |shell, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(document, model.clone(), cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window);
        shell.workbench = workbench;
        cx.notify();
    });
    let filled = take_events(&host, cx);
    let info = changed_documents(&filled)
        .into_iter()
        .find(|document| document.kind == DocumentKind::Untitled)
        .unwrap();
    assert!(!info.dirty);
    assert!(info.resource_uri.is_none());
    let old_target = filled
        .iter()
        .rev()
        .find_map(|event| match &event.change {
            ApplicationChange::PresentationsChanged { window } => {
                Some(window.panes[0].tabs[0].target.clone())
            }
            _ => None,
        })
        .unwrap();
    let retained_workbench = cx.read(|cx| shell.read(cx).workbench.clone());
    let old_command_target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let close = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(wait_for_command(close, cx), CommandOutcome::Completed);
    let closed = take_events(&host, cx);
    assert!(closed.iter().any(|event| matches!(&event.change, ApplicationChange::DocumentClosed { document } if document == &info.id)));
    assert!(
        changed_documents(&closed)
            .iter()
            .any(|document| document.kind == DocumentKind::Untitled)
    );
    let new_target = closed
        .iter()
        .rev()
        .find_map(|event| match &event.change {
            ApplicationChange::PresentationsChanged { window } => {
                Some(window.panes[0].tabs[0].target.clone())
            }
            _ => None,
        })
        .unwrap();
    cx.read(|cx| {
        assert_eq!(retained_workbench.read(cx).view_count(document), 0);
        assert_eq!(
            old_command_target.validate_tab(cx),
            Err(CommandOutcome::InvalidTarget)
        );
    });
    assert_ne!(old_target.pane, new_target.pane);
    assert_eq!(old_target.pane.window, new_target.pane.window);
    assert!(!cx.read(|cx| model.read(cx).is_open()));
}

#[gpui::test]
fn native_focus_loss_and_deactivation_publish_null_targets_without_repaint_events(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, visual) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });
    visual.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.observe_extension_host(window, cx))
    });
    visual.refresh().unwrap();
    visual.update(|window, cx| {
        window.activate_window();
        shell.read(cx).focus_active_editor(window, cx);
    });
    let focused = take_events(&host, visual);
    assert!(focused.iter().any(|event| matches!(
        &event.change,
        ApplicationChange::ActiveTargetChanged {
            active_window: Some(_),
            active_target: Some(_)
        }
    )));
    visual.refresh().unwrap();
    assert!(take_events(&host, visual).is_empty());
    visual.update(|window, _| window.blur());
    let blurred = take_events(&host, visual);
    assert!(blurred.iter().any(|event| matches!(
        &event.change,
        ApplicationChange::ActiveTargetChanged {
            active_window: Some(_),
            active_target: None
        }
    )));
    assert!(changed_documents(&blurred).is_empty());
    visual.deactivate_window();
    let deactivated = take_events(&host, visual);
    assert!(deactivated.iter().any(|event| matches!(
        &event.change,
        ApplicationChange::ActiveTargetChanged {
            active_window: None,
            active_target: None
        }
    )));
}

#[gpui::test]
fn application_metadata_and_buffer_listeners_share_each_edit_once(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;
    let documents = install_globals(cx);
    let host = cx.update(extension_host::install);
    let key = ExtensionKey::new(ExtensionId::new(709), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, r#"
import { commands, workbench } from "knot";
const edits = [], metadata = [];
await workbench.observe(async event => {
  if (event.type === "documentOpened") await event.document.buffer.onDidChange(change => { edits.push([change.beforeRevision, change.revision]); });
  if (event.type === "documentChanged") metadata.push([event.document.revision, event.document.dirty]);
});
await commands.register("observation.barrier", () => {});
await commands.register("observation.stream", () => {
  if (JSON.stringify(edits) !== "[[0,1],[1,2]]") throw new Error(`buffer stream ${JSON.stringify(edits)}`);
  if (JSON.stringify(metadata) !== "[[1,true],[2,false]]") throw new Error(`metadata stream ${JSON.stringify(metadata)}`);
});
"#).unwrap();
        cx.spawn(async move |_, _| { execution.await.unwrap(); }).detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("observation.stream")
            .is_ok()
    });
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "observation.barrier", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    model.update(cx, |model, cx| {
        model.replace(0..0, "edit").unwrap();
        cx.notify();
    });
    model.update(cx, |model, cx| {
        model.undo().unwrap();
        cx.notify();
    });
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, "observation.stream", cx),
            cx
        ),
        CommandOutcome::Completed
    );
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
}
