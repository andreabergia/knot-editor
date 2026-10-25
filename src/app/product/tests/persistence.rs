//! Open, Save, Save As, and external conflicts through product commands.
use super::*;

#[gpui::test]
async fn cancelling_the_native_open_selection_settles_the_command(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Cancelled,
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: super::super::OPEN_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    cx.read(|cx| assert_eq!(documents.read(cx).documents().count(), 1));
}

#[gpui::test]
async fn save_creates_a_destination_before_binding_it_as_persisted(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/created.txt").unwrap();
    let model = cx.new(|_| BufferModel::from_text("created text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_destination("created.txt", model.clone(), uri.clone(), cx)
    });
    let (shell, window_handle) = product_window(document, model, cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"created text"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.resource_uri(), Some(&uri));
        assert!(matches!(document.state(), DocumentState::Persisted { .. }));
        assert!(!document.is_dirty(cx));
    });
}

#[cfg(unix)]
#[gpui::test]
fn opening_and_saving_through_a_symlink_keeps_the_link_and_updates_its_target(
    cx: &mut TestAppContext,
) {
    let links = tempfile::tempdir().unwrap();
    let targets = tempfile::tempdir().unwrap();
    let target = targets.path().join("dotfile");
    let link = links.path().join(".dotfile");
    std::fs::write(&target, "linked text").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let documents = install_globals(cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(link.clone()),
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let execution = dispatch_product_command(&shell, window, super::super::OPEN_COMMAND, cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    let opened = cx.read(|cx| {
        let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
        pane.active_tab().editor().unwrap().read(cx).model().clone()
    });
    assert_eq!(cx.read(|cx| opened.read(cx).text()), "linked text");
    opened.update(cx, |model, cx| {
        let len = model.text().len();
        model.replace(0..len, "saved through the link").unwrap();
        cx.notify();
    });

    let execution = dispatch_product_command(&shell, window, SAVE_COMMAND, cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "saved through the link"
    );
    cx.read(|cx| {
        let document = documents
            .read(cx)
            .documents()
            .find(|document| document.model() == &opened)
            .unwrap();
        assert!(matches!(document.state(), DocumentState::Persisted { .. }));
        assert!(!document.is_dirty(cx));
    });
}

#[gpui::test]
async fn edit_racing_save_keeps_the_newer_revision_dirty(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| {
        let len = model.text().len();
        model.replace(0..len, "captured text").unwrap();
    });
    let captured_revision = model.read_with(cx, |model, _| model.revision());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let (completion, receiver) = super::super::CommandCompletion::new();
    cx.update_window(window_handle, |_, window, cx| {
        let target = shell.update(cx, |shell, cx| {
            shell.focus_active_editor(window, cx);
            shell.capture_command_target(window, cx).unwrap()
        });
        shell.update(cx, |shell, cx| {
            shell.start_save_command(false, target, completion, cx)
        });
        model.update(cx, |model, _| {
            let len = model.text().len();
            model.replace(0..len, "newer text").unwrap();
        });
    })
    .unwrap();

    assert_eq!(receiver.await.unwrap(), CommandOutcome::Completed);
    assert_eq!(provider.read(uri).await.unwrap().bytes, b"captured text");
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(
            document.state().persisted_revision(),
            Some(captured_revision)
        );
        assert!(document.is_dirty(cx));
    });
}

#[gpui::test]
async fn save_completion_rejects_a_closed_captured_tab(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| model.replace(0..0, "changed ").unwrap());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let (completion, receiver) = super::super::CommandCompletion::new();
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            assert!(shell.split_pane(
                shell.workbench.read(cx).focused_pane_id().unwrap(),
                SplitDirection::Horizontal,
                window,
                cx,
            ));
        });
        let target = shell.update(cx, |shell, cx| {
            shell.capture_command_target(window, cx).unwrap()
        });
        shell.update(cx, |shell, cx| {
            shell.start_save_command(false, target.clone(), completion, cx);
            assert_eq!(
                shell.close_tab(
                    target.pane,
                    target.tab,
                    super::super::target_document(&target).unwrap(),
                    window,
                    cx
                ),
                CommandOutcome::Completed
            );
        });
    })
    .unwrap();

    assert_eq!(receiver.await.unwrap(), CommandOutcome::InvalidTarget);
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.state().persisted_revision(), Some(0));
        assert!(document.is_dirty(cx));
    });
}

#[gpui::test]
async fn save_as_overwrites_the_selected_file_and_retargets_only_after_success(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(uri.clone()),
    ))));
    let model = cx.new(|_| BufferModel::from_text("replacement"));
    let document = cx.update(|cx| {
        documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model.clone(), cx)
        })
    });
    let (shell, window_handle) = product_window(document, model, cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_AS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"replacement"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.resource_uri(), Some(&uri));
        assert_eq!(document.title(), "notes.txt");
    });
}

#[gpui::test]
async fn save_as_cancellation_preserves_untitled_identity(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Cancelled,
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_AS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    cx.read(|cx| {
        assert!(matches!(
            documents.read(cx).get(document).unwrap().state(),
            DocumentState::Untitled { .. }
        ));
    });
}

#[gpui::test]
async fn save_as_failure_preserves_text_and_document_identity(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
    ))));
    let model = cx.new(|_| BufferModel::from_text("unsaved text"));
    let document = cx.update(|cx| {
        documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model.clone(), cx)
        })
    });
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_AS_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });

    assert!(matches!(
        execution.completion.await.unwrap(),
        CommandOutcome::HandlerFailure { .. }
    ));
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert!(matches!(document.state(), DocumentState::Untitled { .. }));
        assert_eq!(document.model(), &model);
        assert_eq!(model.read(cx).text(), "unsaved text");
    });
}

#[gpui::test]
async fn external_save_conflict_offers_reload_and_installs_the_external_version(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
    provider.seed_file(uri.clone(), b"external text").unwrap();
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Reload");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "external text");
        assert!(!documents.read(cx).get(document).unwrap().is_dirty(cx));
    });
}

#[gpui::test]
async fn external_conflict_reload_rejects_an_edit_racing_its_read(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let root = ResourceUri::parse("mem://product/").unwrap();
    let provider = Arc::new(MemoryFileSystemProvider::new(root).unwrap());
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    provider.seed_file(uri.clone(), b"loaded text").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    provider.seed_file(uri.clone(), b"external text").unwrap();
    let (read_release, gated_read) = tokio::sync::oneshot::channel();
    let gated_provider = Arc::new(GatedFileSystemProvider {
        inner: provider.clone(),
        read_release: Mutex::new(Some(gated_read)),
        replace_release: Mutex::new(None),
    });
    let mut filesystems = FileSystemProviderRegistry::new();
    filesystems.register("mem", gated_provider).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));

    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let execution = dispatch_product_command(&shell, window_handle, SAVE_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Reload");
    cx.run_until_parked();

    model.update(cx, |model, _| model.replace(0..0, "newer ").unwrap());
    read_release.send(()).unwrap();

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"external text"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(model.read(cx).text(), "newer local loaded text");
        assert_eq!(document.resource_uri(), Some(&uri));
        assert_eq!(document.state().persisted_revision(), Some(0));
        assert!(document.is_dirty(cx));
    });
}

#[gpui::test]
async fn cancelling_an_external_save_conflict_preserves_the_dirty_document(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
    provider.seed_file(uri.clone(), b"external text").unwrap();
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Cancelled
    );
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"external text"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(model.read(cx).text(), "local loaded text");
        assert_eq!(document.resource_uri(), Some(&uri));
        assert!(document.is_dirty(cx));
    });
}

#[gpui::test]
async fn save_as_after_an_external_conflict_retargets_only_after_saving(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let original_uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let alternate_uri = ResourceUri::parse("mem://product/recovered.txt").unwrap();
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(alternate_uri.clone()),
    ))));
    let opened = provider.read(original_uri.clone()).await.unwrap();
    let model = cx.new(|_| BufferModel::from_text("loaded text"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            model.clone(),
            original_uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    model.update(cx, |model, _| model.replace(0..0, "local ").unwrap());
    provider
        .seed_file(original_uri.clone(), b"external text")
        .unwrap();
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_editor(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: SAVE_COMMAND.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target,
                cx,
            )
        })
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.resource_uri(), Some(&original_uri));
        assert!(document.is_dirty(cx));
    });
    cx.simulate_prompt_answer("Save As…");

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert_eq!(
        provider.read(original_uri).await.unwrap().bytes,
        b"external text"
    );
    assert_eq!(
        provider.read(alternate_uri.clone()).await.unwrap().bytes,
        b"local loaded text"
    );
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_eq!(document.resource_uri(), Some(&alternate_uri));
        assert_eq!(document.title(), "recovered.txt");
        assert!(!document.is_dirty(cx));
    });
}

#[gpui::test]
async fn file_and_missing_file_open_replace_the_launch_placeholder(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);

    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(
                ResourceUri::parse("mem://product/notes.txt").unwrap(),
            ),
            target,
            true,
            None,
            cx,
        );
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let documents = documents.read(cx);
        assert_eq!(documents.documents().count(), 1);
        let document = documents.documents().next().unwrap();
        assert_eq!(document.model().read(cx).text(), "loaded text");
        assert!(matches!(document.state(), DocumentState::Persisted { .. }));
    });

    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/new.txt").unwrap()),
            target,
            false,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let documents = documents.read(cx);
        let id = documents
            .document_for_resource(&ResourceUri::parse("mem://product/new.txt").unwrap())
            .unwrap();
        assert!(matches!(
            documents.get(id).unwrap().state(),
            DocumentState::Destination { .. }
        ));
    });
}

#[gpui::test]
async fn open_deduplicates_resources_and_rejects_stale_completions(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    let target = cx.read(|cx| current_open_target(&shell, cx));

    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(
                ResourceUri::parse("mem://product/notes.txt").unwrap(),
            ),
            target.clone(),
            true,
            None,
            cx,
        );
        shell.start_open_request(
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/new.txt").unwrap()),
            target,
            true,
            None,
            cx,
        );
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let documents = documents.read(cx);
        assert_eq!(documents.documents().count(), 1);
        assert!(
            documents
                .document_for_resource(&ResourceUri::parse("mem://product/notes.txt").unwrap())
                .is_none()
        );
        assert!(
            documents
                .document_for_resource(&ResourceUri::parse("mem://product/new.txt").unwrap())
                .is_some()
        );
    });

    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/new.txt").unwrap()),
            target,
            false,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(documents.read(cx).documents().count(), 1);
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
    });
}

#[gpui::test]
async fn folder_open_installs_a_workspace_and_failures_preserve_the_document(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    let target = cx.read(|cx| current_open_target(&shell, cx));

    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(
                ResourceUri::parse("mem://product/invalid.txt").unwrap(),
            ),
            target,
            true,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(documents.read(cx).documents().count(), 1);
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .document_id()
                .unwrap(),
            placeholder
        );
        assert!(shell.read(cx).status.contains("not valid UTF-8"));
    });

    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_request(
            OpenRequest::from_uri_for_product(ResourceUri::parse("mem://product/src").unwrap()),
            target,
            false,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let shell = shell.read(cx);
        assert_eq!(
            shell.workspace.snapshot().first_root().unwrap().to_string(),
            "mem://product/src"
        );
        assert!(shell.workspace_tree.is_some());
        assert_eq!(documents.read(cx).documents().count(), 1);
    });
}

#[gpui::test]
fn open_from_terminal_requests_picker_and_preserves_terminal(cx: &mut TestAppContext) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "opened from terminal").unwrap();
    let documents = install_globals(cx);
    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(file.path().to_path_buf()),
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let terminal_tab = shell.update(cx, |shell, cx| {
        let (session, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, session, view).unwrap()
        })
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.focus_active_surface(window, cx));
    })
    .unwrap();
    let execution = dispatch_product_command(&shell, window, super::super::OPEN_COMMAND, cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
        assert!(
            pane.tabs()
                .iter()
                .any(|tab| tab.id() == terminal_tab && tab.terminal_view().is_some())
        );
        assert_eq!(
            pane.active_tab()
                .editor()
                .unwrap()
                .read(cx)
                .model()
                .read(cx)
                .text(),
            "opened from terminal"
        );
    });
}
