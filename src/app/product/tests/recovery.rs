//! Durable unsaved-document recovery through the product document lifecycle.
use super::*;
use crate::app::recovery::{
    ApplicationRecovery, RecoveryService, RecoveryStore, RestoreError, RestoreOutcome,
    RestoredResource, SnapshotDraft, SnapshotKind, SnapshotState, StartupRecovery,
};

fn recovery_store(cx: &TestAppContext) -> crate::app::recovery::RecoveryStore {
    cx.read(|cx| cx.global::<ApplicationRecovery>().0.store().clone())
}

async fn flush(cx: &TestAppContext) {
    recovery_store(cx).flush().await.unwrap();
}

#[gpui::test]
async fn dirty_documents_are_captured_and_persistence_retires_the_record(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let model = cx.new(|_| BufferModel::from_text("initial"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_untitled("Untitled", model.clone(), cx)
    });
    let identity = cx.read(|cx| documents.read(cx).get(document).unwrap().identity());

    // A clean document has no automatic record.
    flush(cx).await;
    assert!(
        recovery_store(cx)
            .automatic(identity)
            .await
            .unwrap()
            .is_none()
    );

    model.update(cx, |model, cx| {
        model.replace(0..7, "dirty").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    flush(cx).await;
    let stored = recovery_store(cx)
        .automatic(identity)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.kind, SnapshotKind::Automatic);
    assert_eq!(stored.state, SnapshotState::Untitled);
    assert_eq!(stored.text, "dirty");

    // Save As commits a new baseline; the automatic record is retired because
    // the document is clean.
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let revision = cx.read(|cx| model.read(cx).revision());
    let capture = documents
        .update(cx, |documents, _| {
            documents.begin_persistence(document, &model)
        })
        .unwrap();
    let committed = documents.update(cx, |documents, cx| {
        documents.finish_persistence(
            document,
            &model,
            &capture,
            ("notes.txt", uri.clone()),
            (ResourceVersion::new([1]), revision),
            cx,
        )
    });
    assert!(committed);
    flush(cx).await;
    assert!(
        recovery_store(cx)
            .automatic(identity)
            .await
            .unwrap()
            .is_none()
    );

    // An edit after the save captures the new destination and version.
    model.update(cx, |model, cx| {
        model.replace(0..5, "again").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    flush(cx).await;
    let stored = recovery_store(cx)
        .automatic(identity)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, SnapshotState::Persisted);
    assert_eq!(
        stored.resource_uri.as_deref(),
        Some("mem://product/notes.txt")
    );
    assert_eq!(stored.provider_version.as_deref(), Some(&[1u8][..]));
    assert_eq!(stored.text, "again");
}

#[gpui::test]
async fn repeated_edits_keep_one_record_per_document(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let model = cx.new(|_| BufferModel::from_text("initial"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_untitled("Untitled", model.clone(), cx)
    });
    let identity = cx.read(|cx| documents.read(cx).get(document).unwrap().identity());
    for text in ["one", "two", "three"] {
        model.update(cx, |model, cx| {
            let len = model.text().len();
            model.replace(0..len, text).unwrap();
            cx.notify();
        });
        cx.run_until_parked();
    }
    flush(cx).await;
    let snapshots = recovery_store(cx).snapshots().await.unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].document_id, identity);
    assert_eq!(snapshots[0].text, "three");
}

fn snapshot_draft(
    title: &str,
    uri: Option<&str>,
    state: SnapshotState,
    version: Option<Vec<u8>>,
    text: &str,
    captured_at_ms: i64,
) -> SnapshotDraft {
    SnapshotDraft {
        document_id: uuid::Uuid::new_v4(),
        title: title.into(),
        resource_uri: uri.map(str::to_owned),
        state,
        provider_version: version,
        text: text.into(),
        captured_at_ms,
    }
}

/// Leave automatic snapshots behind as an interrupted session would.
async fn seed(drafts: &[&SnapshotDraft], cx: &TestAppContext) {
    for draft in drafts {
        recovery_store(cx).submit_automatic((*draft).clone());
    }
    flush(cx).await;
}

async fn startup_recovery(
    filesystems: Arc<FileSystemProviderRegistry>,
    cx: &mut TestAppContext,
) -> StartupRecovery {
    cx.update(|cx| crate::app::recovery::restore_at_startup(filesystems, cx))
        .await
}

async fn restore_automatic(
    document_id: uuid::Uuid,
    filesystems: Arc<FileSystemProviderRegistry>,
    cx: &mut TestAppContext,
) -> Result<RestoreOutcome, RestoreError> {
    cx.update(|cx| {
        crate::app::recovery::restore(
            SnapshotKind::Automatic,
            document_id.to_string(),
            filesystems,
            cx,
        )
    })
    .await
}

fn document_by_identity(
    documents: &Entity<DocumentCollection>,
    identity: uuid::Uuid,
    cx: &TestAppContext,
) -> super::super::DocumentId {
    cx.read(|cx| {
        documents
            .read(cx)
            .document_by_identity(identity)
            .map(|document| document.id())
            .expect("restored document keeps its identity")
    })
}

#[gpui::test]
async fn startup_restores_unsaved_documents_dirty_without_touching_resources(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems.clone()));
    let notes = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let version = provider.read(notes.clone()).await.unwrap().version;
    let untitled = snapshot_draft(
        "Untitled",
        None,
        SnapshotState::Untitled,
        None,
        "draft text",
        3,
    );
    let persisted = snapshot_draft(
        "notes.txt",
        Some("mem://product/notes.txt"),
        SnapshotState::Persisted,
        Some(version.as_bytes().to_vec()),
        "edited text",
        2,
    );
    let destination = snapshot_draft(
        "new.txt",
        Some("mem://product/new.txt"),
        SnapshotState::Destination,
        None,
        "new text",
        1,
    );
    seed(&[&untitled, &persisted, &destination], cx).await;

    let recovery = startup_recovery(filesystems, cx).await;
    assert_eq!(recovery.restored.len(), 3);
    assert_eq!(recovery.remaining, 0);
    assert_eq!(recovery.error, None);
    for (draft, expected_text) in [
        (&untitled, "draft text"),
        (&persisted, "edited text"),
        (&destination, "new text"),
    ] {
        let document = document_by_identity(&documents, draft.document_id, cx);
        let model = cx.read(|cx| {
            let entry = documents.read(cx).get(document).unwrap();
            assert!(entry.is_dirty(cx), "{} is dirty", draft.title);
            assert_eq!(entry.title(), draft.title.as_str());
            assert_eq!(entry.model().read(cx).text(), expected_text);
            entry.model().clone()
        });
        // Undo history starts empty.
        assert!(!model.update(cx, |model, _| model.undo().unwrap()));
    }
    cx.read(|cx| {
        let documents = documents.read(cx);
        let persisted = documents
            .get(document_by_identity_in(documents, persisted.document_id))
            .unwrap();
        assert_eq!(persisted.state().persisted_version(), Some(&version));
        assert!(matches!(
            documents
                .get(document_by_identity_in(documents, untitled.document_id))
                .unwrap()
                .state(),
            DocumentState::Untitled { .. }
        ));
        assert!(matches!(
            documents
                .get(document_by_identity_in(documents, destination.document_id))
                .unwrap()
                .state(),
            DocumentState::Destination { .. }
        ));
        // Restored resources deduplicate with ordinary opening.
        assert!(documents.document_for_resource(&notes).is_some());
    });
    // Nothing was written: the file keeps its content and the destination is absent.
    assert_eq!(
        provider.read(notes.clone()).await.unwrap().bytes,
        b"loaded text"
    );
    assert_eq!(
        provider
            .stat(ResourceUri::parse("mem://product/new.txt").unwrap())
            .await
            .unwrap(),
        ResourceStat::Missing
    );
    // Restored documents keep exactly their automatic protection.
    flush(cx).await;
    let snapshots = recovery_store(cx).snapshots().await.unwrap();
    assert_eq!(snapshots.len(), 3);
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.kind == SnapshotKind::Automatic)
    );
}

fn document_by_identity_in(
    documents: &DocumentCollection,
    identity: uuid::Uuid,
) -> super::super::DocumentId {
    documents.document_by_identity(identity).unwrap().id()
}

#[gpui::test]
async fn changed_and_deleted_resources_restore_dirty_and_save_reaches_the_conflict_path(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems.clone()));
    let notes = ResourceUri::parse("mem://product/notes.txt").unwrap();
    // The capture-time version no longer matches: the file changed externally.
    let changed = snapshot_draft(
        "notes.txt",
        Some("mem://product/notes.txt"),
        SnapshotState::Persisted,
        Some(vec![0xde, 0xad]),
        "recovered text",
        2,
    );
    let deleted = snapshot_draft(
        "gone.txt",
        Some("mem://product/gone.txt"),
        SnapshotState::Persisted,
        Some(vec![1]),
        "orphaned text",
        1,
    );
    seed(&[&changed, &deleted], cx).await;

    let changed_outcome = restore_automatic(changed.document_id, filesystems.clone(), cx)
        .await
        .unwrap();
    let RestoreOutcome::Restored {
        document,
        resource: RestoredResource::Changed,
    } = changed_outcome
    else {
        panic!("unexpected outcome {changed_outcome:?}");
    };
    let deleted_outcome = restore_automatic(deleted.document_id, filesystems, cx)
        .await
        .unwrap();
    let RestoreOutcome::Restored {
        document: deleted_document,
        resource: RestoredResource::Missing,
    } = deleted_outcome
    else {
        panic!("unexpected outcome {deleted_outcome:?}");
    };
    let model = cx.read(|cx| {
        let documents = documents.read(cx);
        let entry = documents.get(document).unwrap();
        assert!(entry.is_dirty(cx));
        assert_eq!(entry.model().read(cx).text(), "recovered text");
        assert_eq!(
            entry.state().persisted_version(),
            Some(&ResourceVersion::new([0xde, 0xad]))
        );
        let deleted = documents.get(deleted_document).unwrap();
        assert!(deleted.is_dirty(cx));
        assert!(matches!(deleted.state(), DocumentState::Destination { .. }));
        entry.model().clone()
    });
    assert_eq!(
        provider.read(notes.clone()).await.unwrap().bytes,
        b"loaded text"
    );

    // Saving compares the capture-time version and offers the conflict prompt.
    let (shell, window) = product_window(document, model.clone(), cx);
    let execution = dispatch_product_command(&shell, window, SAVE_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Cancel");
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Cancelled);
    assert_eq!(
        provider.read(notes.clone()).await.unwrap().bytes,
        b"loaded text"
    );
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "recovered text");
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
    });
}

#[gpui::test]
async fn duplicate_records_for_one_uri_restore_the_newest_and_keep_the_older(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems.clone()));
    let version = provider
        .read(ResourceUri::parse("mem://product/notes.txt").unwrap())
        .await
        .unwrap()
        .version
        .as_bytes()
        .to_vec();
    let older = snapshot_draft(
        "notes.txt",
        Some("mem://product/notes.txt"),
        SnapshotState::Persisted,
        Some(version.clone()),
        "older text",
        1,
    );
    let newer = snapshot_draft(
        "notes.txt",
        Some("mem://product/notes.txt"),
        SnapshotState::Persisted,
        Some(version),
        "newer text",
        2,
    );
    // An unreadable baseline keeps its record rather than dropping it.
    let unreadable = snapshot_draft(
        "invalid.txt",
        Some("mem://product/invalid.txt"),
        SnapshotState::Persisted,
        Some(vec![1]),
        "kept text",
        0,
    );
    seed(&[&older, &newer, &unreadable], cx).await;

    let recovery = startup_recovery(filesystems.clone(), cx).await;
    assert_eq!(recovery.restored.len(), 1);
    assert_eq!(recovery.remaining, 2);
    let owner = document_by_identity(&documents, newer.document_id, cx);
    assert_eq!(
        cx.read(|cx| documents
            .read(cx)
            .get(owner)
            .unwrap()
            .model()
            .read(cx)
            .text()),
        "newer text"
    );
    let listed = cx.update(crate::app::recovery::recoverable).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|snapshot| snapshot.document_id)
            .collect::<Vec<_>>(),
        vec![older.document_id, unreadable.document_id]
    );

    // Restoring the older record onto the live URI preserves both versions.
    let outcome = restore_automatic(older.document_id, filesystems.clone(), cx)
        .await
        .unwrap();
    let RestoreOutcome::Preserved {
        owner: live,
        document: preserved,
    } = outcome
    else {
        panic!("unexpected outcome {outcome:?}");
    };
    assert_eq!(live, owner);
    let preserved_identity = cx.read(|cx| {
        let documents = documents.read(cx);
        assert_eq!(
            documents.get(owner).unwrap().model().read(cx).text(),
            "newer text"
        );
        let preserved = documents.get(preserved).unwrap();
        assert_eq!(preserved.title(), "notes.txt (Recovered)");
        assert!(matches!(preserved.state(), DocumentState::Untitled { .. }));
        assert!(preserved.is_dirty(cx));
        assert_eq!(preserved.model().read(cx).text(), "older text");
        preserved.identity()
    });
    // Protection moved to the separate copy in one transaction.
    flush(cx).await;
    let snapshots = recovery_store(cx).snapshots().await.unwrap();
    assert!(
        snapshots
            .iter()
            .all(|snapshot| snapshot.document_id != older.document_id)
    );
    let moved = snapshots
        .iter()
        .find(|snapshot| snapshot.document_id == preserved_identity)
        .unwrap();
    assert_eq!(moved.text, "older text");
    assert_eq!(moved.state, SnapshotState::Untitled);

    let failed = restore_automatic(unreadable.document_id, filesystems.clone(), cx).await;
    assert!(
        matches!(failed, Err(RestoreError::Resource(_))),
        "{failed:?}"
    );
    assert!(
        recovery_store(cx)
            .automatic(unreadable.document_id)
            .await
            .unwrap()
            .is_some()
    );
    // An automatic snapshot of an open document is its live protection.
    assert_eq!(
        restore_automatic(newer.document_id, filesystems, cx).await,
        Ok(RestoreOutcome::AlreadyOpen { document: owner })
    );
}

#[gpui::test]
async fn retained_captures_restore_under_a_fresh_identity_until_discarded(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems.clone()));
    let draft = snapshot_draft(
        "Session",
        None,
        SnapshotState::Untitled,
        None,
        "retained text",
        1,
    );
    assert!(
        recovery_store(cx)
            .retain("owner".into(), "session-1".into(), draft.clone())
            .await
            .unwrap()
            .is_some()
    );
    // Startup neither restores nor counts retained captures; they belong to
    // their owner and stay in the listing.
    let recovery = startup_recovery(filesystems.clone(), cx).await;
    assert!(recovery.restored.is_empty());
    assert_eq!(recovery.remaining, 0);
    assert_eq!(
        cx.update(crate::app::recovery::recoverable)
            .await
            .unwrap()
            .len(),
        1
    );

    let restore = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            crate::app::recovery::restore(
                SnapshotKind::Retained,
                "session-1".into(),
                filesystems.clone(),
                cx,
            )
        })
    };
    let outcome = restore(cx).await.unwrap();
    let RestoreOutcome::Restored {
        document,
        resource: RestoredResource::Untitled,
    } = outcome
    else {
        panic!("unexpected outcome {outcome:?}");
    };
    cx.read(|cx| {
        let document = documents.read(cx).get(document).unwrap();
        assert_ne!(document.identity(), draft.document_id);
        assert_eq!(document.model().read(cx).text(), "retained text");
        assert!(document.is_dirty(cx));
    });
    // Restoring does not consume the immutable capture; discarding does.
    assert!(
        cx.update(|cx| {
            crate::app::recovery::discard_snapshot(SnapshotKind::Retained, "session-1".into(), cx)
        })
        .await
        .unwrap()
    );
    assert_eq!(restore(cx).await, Err(RestoreError::Unavailable));
}

#[gpui::test]
async fn multiple_views_of_a_restored_document_keep_one_record(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems.clone()));
    let draft = snapshot_draft("Untitled", None, SnapshotState::Untitled, None, "shared", 1);
    seed(&[&draft], cx).await;
    startup_recovery(filesystems, cx).await;
    let document = document_by_identity(&documents, draft.document_id, cx);
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    product_window(document, model.clone(), cx);
    product_window(document, model.clone(), cx);
    model.update(cx, |model, cx| {
        model.replace(0..0, "more ").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    flush(cx).await;
    let snapshots = recovery_store(cx).snapshots().await.unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].document_id, draft.document_id);
    assert_eq!(snapshots[0].text, "more shared");
}

#[gpui::test]
async fn native_listing_restores_and_discards_with_no_extension_host(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let (shell, window) = super::operations::empty_product_window(&documents, cx);
    // A live dirty document's own protection is never listed.
    let live = cx.read(|cx| {
        let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
        pane.active_tab().document_id().unwrap()
    });
    let live_model = cx.read(|cx| documents.read(cx).get(live).unwrap().model().clone());
    live_model.update(cx, |model, cx| {
        model.replace(0..0, "live").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    let restored = snapshot_draft(
        "Restore me",
        None,
        SnapshotState::Untitled,
        None,
        "restored text",
        2,
    );
    let discarded = snapshot_draft(
        "Discard me",
        None,
        SnapshotState::Untitled,
        None,
        "discarded text",
        1,
    );
    seed(&[&restored, &discarded], cx).await;

    let open_listing = |cx: &mut TestAppContext| {
        let execution =
            dispatch_product_command(&shell, window, super::super::commands::RECOVER_COMMAND, cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while cx.read(|cx| shell.read(cx).transient.is_none()) {
            assert!(std::time::Instant::now() < deadline, "listing did not open");
            cx.run_until_parked();
            std::thread::yield_now();
        }
    };

    // The newest snapshot is first; Enter selects it and the prompt restores it.
    open_listing(cx);
    cx.simulate_keystrokes(window, "enter");
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Restore");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while cx.read(|cx| {
        documents
            .read(cx)
            .document_by_identity(restored.document_id)
            .is_none()
    }) {
        assert!(
            std::time::Instant::now() < deadline,
            "restore did not commit"
        );
        cx.run_until_parked();
        std::thread::yield_now();
    }
    let document = document_by_identity(&documents, restored.document_id, cx);
    cx.read(|cx| {
        let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
        assert_eq!(pane.active_tab().document_id(), Some(document));
        assert_eq!(shell.read(cx).status.as_ref(), "recovered Restore me");
    });

    // The remaining snapshot is discarded explicitly.
    open_listing(cx);
    cx.simulate_keystrokes(window, "enter");
    cx.run_until_parked();
    cx.simulate_prompt_answer("Discard");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while recovery_store(cx)
        .automatic(discarded.document_id)
        .await
        .unwrap()
        .is_some()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "discard did not commit"
        );
        cx.run_until_parked();
        std::thread::yield_now();
    }
    assert!(
        cx.update(crate::app::recovery::recoverable)
            .await
            .unwrap()
            .is_empty()
    );
    open_listing_empty(&shell, window, cx);
}

fn open_listing_empty(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    cx: &mut TestAppContext,
) {
    let execution =
        dispatch_product_command(shell, window, super::super::commands::RECOVER_COMMAND, cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    cx.run_until_parked();
    cx.read(|cx| {
        assert!(shell.read(cx).transient.is_none());
        assert_eq!(
            shell.read(cx).status.as_ref(),
            "no unsaved documents to recover"
        );
    });
}

#[gpui::test]
async fn startup_restores_into_the_launch_window_without_any_extension_host(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    install_launch_gate(cx);
    let draft = snapshot_draft(
        "Untitled",
        None,
        SnapshotState::Untitled,
        None,
        "survived",
        1,
    );
    seed(&[&draft], cx).await;
    // No extension host is installed, so no bundled project registry or session
    // metadata exists; native startup still restores the document.
    cx.update(|cx| super::super::finish_product_startup(Ok(()), cx));
    assert!(wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    let document = document_by_identity(&documents, draft.document_id, cx);
    let shell = cx.windows()[0].downcast::<ProductShell>().unwrap();
    let shell = cx.update(|cx| shell.entity(cx).unwrap());
    cx.read(|cx| {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        // The restored document takes the place of the untitled placeholder.
        assert_eq!(pane.tabs().len(), 1);
        assert_eq!(pane.active_tab().document_id(), Some(document));
        assert_eq!(shell.status.as_ref(), "recovered 1 unsaved document");
        assert_eq!(documents.read(cx).documents().count(), 1);
    });
}

#[gpui::test]
async fn unavailable_recovery_store_never_blocks_startup(cx: &mut TestAppContext) {
    install_globals(cx);
    install_launch_gate(cx);
    let dir = tempfile::tempdir().unwrap();
    let corrupt = dir.path().join("recovery.sqlite3");
    std::fs::write(&corrupt, "not a database").unwrap();
    cx.set_global(ApplicationRecovery(RecoveryService::new(
        RecoveryStore::open(corrupt.clone(), cx.background_executor.clone()),
    )));
    cx.update(|cx| super::super::finish_product_startup(Ok(()), cx));
    assert!(wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 1);
    let shell = cx.windows()[0].downcast::<ProductShell>().unwrap();
    let shell = cx.update(|cx| shell.entity(cx).unwrap());
    cx.read(|cx| {
        assert!(
            shell
                .read(cx)
                .status
                .starts_with("recovery unavailable: recovery:")
        );
    });
    assert_eq!(std::fs::read(&corrupt).unwrap(), b"not a database");
}

#[gpui::test]
async fn launch_requests_open_alongside_recovery_when_an_extension_fails(cx: &mut TestAppContext) {
    let config = tempfile::tempdir().unwrap();
    let extensions = tempfile::tempdir().unwrap();
    let file = config.path().join("edited.txt");
    std::fs::write(&file, "on disk").unwrap();
    let request = OpenRequest::from_path(&file, config.path()).unwrap();
    let uri = request.uri().clone();
    let other = OpenRequest::from_path(&config.path().join("other.txt"), config.path()).unwrap();
    install_extension(
        extensions.path(),
        "@example/broken",
        &[],
        &[("main.js", "throw new Error('package failed');")],
    );
    let documents = install_globals(cx);
    install_launch_gate(cx);
    // The local provider runs on its own runtime, outside the test executor.
    let version = pollster::block_on(
        product_filesystems()
            .provider(&uri)
            .unwrap()
            .read(uri.clone()),
    )
    .unwrap()
    .version;
    let edited = snapshot_draft(
        "edited.txt",
        Some(uri.as_url().as_str()),
        SnapshotState::Persisted,
        Some(version.as_bytes().to_vec()),
        "unsaved edits",
        2,
    );
    let older = SnapshotDraft {
        text: "older edits".into(),
        captured_at_ms: 1,
        ..snapshot_draft(
            "edited.txt",
            Some(uri.as_url().as_str()),
            SnapshotState::Persisted,
            Some(version.as_bytes().to_vec()),
            "",
            1,
        )
    };
    seed(&[&edited, &older], cx).await;

    let host = cx.update(super::super::super::extensions::host::install);
    cx.update(|cx| super::super::dispatch_launch_request(Some(request), cx));
    cx.update(|cx| super::super::dispatch_launch_request(Some(other), cx));
    host.update(cx, |host, cx| {
        host.start_product_startup(
            Some(config.path().to_path_buf()),
            Ok(extensions.path().to_path_buf()),
            cx,
        )
    });
    assert!(wait_for_product_startup(cx));
    assert_eq!(cx.windows().len(), 2);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        let opened = cx.read(|cx| {
            documents
                .read(cx)
                .documents()
                .filter(|document| matches!(document.state(), DocumentState::Destination { .. }))
                .count()
        });
        if opened == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the second launch request did not open"
        );
        std::thread::yield_now();
    }
    cx.read(|cx| {
        let documents = documents.read(cx);
        // The launch request for the edited file deduplicates to the restored
        // document instead of loading the disk text.
        let restored = documents
            .get(documents.document_for_resource(&uri).unwrap())
            .unwrap();
        assert_eq!(restored.identity(), edited.document_id);
        assert_eq!(restored.model().read(cx).text(), "unsaved edits");
        assert!(restored.is_dirty(cx));
    });
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "on disk");
    // The recovery pointer replaces the launch open's status once it settles.
    let expected = "1 unsaved document can be recovered with File › Recover Unsaved Documents…";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        let statuses = cx.read(|cx| {
            cx.windows()
                .iter()
                .filter_map(|window| window.downcast::<ProductShell>())
                .filter_map(|window| window.entity(cx).ok())
                .map(|shell| shell.read(cx).status.to_string())
                .collect::<Vec<_>>()
        });
        if statuses.iter().any(|status| status == expected) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{statuses:?}");
        std::thread::yield_now();
    }
}

#[gpui::test]
async fn quit_is_a_hot_exit_that_restores_dirty_documents_on_relaunch(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, provider) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems.clone()));
    let uri = ResourceUri::parse("mem://product/notes.txt").unwrap();
    let opened = provider.read(uri.clone()).await.unwrap();
    let saved_model = cx.new(|_| BufferModel::from_text("loaded text"));
    let saved = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "notes.txt",
            saved_model.clone(),
            uri.clone(),
            0,
            opened.version,
            cx,
        )
    });
    let untitled = cx.update(|cx| create_untitled_document(&documents, cx));
    let untitled_model = cx.read(|cx| documents.read(cx).get(untitled).unwrap().model().clone());
    let (shell, window) = product_window(saved, saved_model.clone(), cx);
    shell.update(cx, |shell, cx| {
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, cx| {
            workbench
                .open_tab_for_document(pane, untitled, untitled_model.clone(), cx)
                .unwrap();
        });
    });
    // Both edits are still inside their debounce window when quit begins.
    for (model, text) in [(&saved_model, "edited "), (&untitled_model, "draft")] {
        model.update(cx, |model, cx| {
            model.replace(0..0, text).unwrap();
            cx.notify();
        });
    }
    let identities = cx.read(|cx| {
        let documents = documents.read(cx);
        [saved, untitled].map(|document| documents.get(document).unwrap().identity())
    });

    let execution = dispatch_product_command(&shell, window, QUIT_COMMAND, cx);
    assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
    assert!(!cx.has_pending_prompt());
    assert_eq!(
        provider.read(uri.clone()).await.unwrap().bytes,
        b"loaded text"
    );

    // Relaunch with an empty document collection over the same store.
    let relaunched = cx.new(|_| DocumentCollection::new());
    cx.set_global(ApplicationDocuments(relaunched.clone()));
    let recovery = startup_recovery(filesystems, cx).await;
    assert_eq!(recovery.restored.len(), 2);
    assert_eq!(recovery.remaining, 0);
    for (identity, text) in identities.into_iter().zip(["edited loaded text", "draft"]) {
        let document = document_by_identity(&relaunched, identity, cx);
        cx.read(|cx| {
            let document = relaunched.read(cx).get(document).unwrap();
            assert!(document.is_dirty(cx));
            assert_eq!(document.model().read(cx).text(), text);
        });
    }
}

const RECOVERY_OWNER: &str = r#"
import { commands, recovery, workbench } from 'knot';
const report = value => workbench.writeClipboardText(JSON.stringify(value));
await commands.register('owner.retain', async ({ arguments: { document } }) => {
  const capture = await recovery.retain(document);
  if (!Object.isFrozen(capture)) throw Error('capture is mutable');
  await report({ capture, listed: (await recovery.list()).map(capture => capture.id) });
});
await commands.register('owner.list', async () => {
  await report((await recovery.list()).map(capture => capture.id));
});
await commands.register('owner.restore', async ({ arguments: { id } }) => {
  const restored = await recovery.restore(id);
  await report({ ...restored, document: restored.document.id,
    presented: restored.presentation.document, presentation: undefined });
});
await commands.register('owner.release', async ({ arguments: { id } }) => {
  await report(await recovery.release(id));
});
"#;

const RECOVERY_OTHER: &str = r#"
import { commands, recovery, workbench } from 'knot';
const code = async operation => {
  try { await operation(); return null; } catch (error) { return error.code ?? String(error); }
};
await commands.register('other.probe', async ({ arguments: { id } }) => {
  await workbench.writeClipboardText(JSON.stringify({
    listed: (await recovery.list()).length,
    released: await recovery.release(id),
    restore: await code(() => recovery.restore(id)),
    retain: await code(() => recovery.retain('not-a-document')),
  }));
});
"#;

fn object(fields: &[(&str, &str)]) -> CommandArgumentValue {
    CommandArgumentValue::Object(
        fields
            .iter()
            .map(|(key, value)| {
                (
                    (*key).to_owned(),
                    CommandArgumentValue::String((*value).into()),
                )
            })
            .collect(),
    )
}

fn run_reporting(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    name: &'static str,
    arguments: CommandArgumentValue,
    cx: &mut TestAppContext,
) -> serde_json::Value {
    let execution = dispatch_product_command_with(shell, window, name, arguments, cx);
    assert_eq!(
        wait_for_command(execution, cx),
        CommandOutcome::Completed,
        "{name}"
    );
    cx.read(|cx| serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap())
}

fn retained_text(id: &str, cx: &mut TestAppContext) -> Option<String> {
    let read = recovery_store(cx).snapshot(SnapshotKind::Retained, id.to_owned());
    let (sender, receiver) = std::sync::mpsc::channel();
    cx.spawn(async move |_| {
        let _ = sender.send(read.await);
    })
    .detach();
    cx.run_until_parked();
    receiver
        .recv()
        .unwrap()
        .unwrap()
        .map(|snapshot| snapshot.text)
}

#[gpui::test]
fn extension_captures_are_owner_isolated_durable_and_survive_failure_and_restart(
    cx: &mut TestAppContext,
) {
    let extensions = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    install_extension(
        extensions.path(),
        "@example/recovery-owner",
        &[],
        &[("main.js", RECOVERY_OWNER)],
    );
    install_extension(
        extensions.path(),
        "@example/recovery-other",
        &[],
        &[("main.js", RECOVERY_OTHER)],
    );
    let documents = install_globals(cx);
    let host = cx.update(crate::app::extensions::host::install);
    host.update(cx, |host, cx| {
        host.set_test_storage_root(storage.path().to_owned(), cx)
    });
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let identity = cx.read(|cx| {
        documents
            .read(cx)
            .get(document)
            .unwrap()
            .identity()
            .to_string()
    });
    let (shell, window) = product_window(document, model.clone(), cx);
    let start = |cx: &mut TestAppContext| {
        host.update(cx, |host, cx| {
            host.start_installed_extensions(extensions.path().to_owned(), cx)
        });
        wait_for_extension_report(&host, cx)
    };
    let report = start(cx);
    assert!(
        report
            .report
            .entries
            .iter()
            .all(|entry| entry.result == crate::app::extensions::load::LoadResult::Loaded)
    );
    let edit = |text: &str, cx: &mut TestAppContext| {
        model.update(cx, |model, cx| {
            let len = model.text().len();
            model.replace(0..len, text).unwrap();
            cx.notify();
        });
    };

    // Each retain is durable when it resolves and allocates a new capture, so
    // captures of different revisions of one document coexist.
    edit("first", cx);
    let first = run_reporting(
        &shell,
        window,
        "owner.retain",
        object(&[("document", &identity)]),
        cx,
    );
    let first_id = first["capture"]["id"].as_str().unwrap().to_owned();
    assert_eq!(first["capture"]["document"], identity);
    assert_eq!(first["capture"]["kind"], "untitled");
    assert_eq!(first["listed"], serde_json::json!([first_id]));
    assert_eq!(retained_text(&first_id, cx).as_deref(), Some("first"));
    edit("second", cx);
    let second = run_reporting(
        &shell,
        window,
        "owner.retain",
        object(&[("document", &identity)]),
        cx,
    );
    let second_id = second["capture"]["id"].as_str().unwrap().to_owned();
    assert_ne!(first_id, second_id);
    assert_eq!(second["listed"], serde_json::json!([second_id, first_id]));
    assert_eq!(retained_text(&second_id, cx).as_deref(), Some("second"));

    // Another owner cannot list, release, or restore the captures, even with
    // the exact ID; a foreign capture is indistinguishable from a missing one.
    let probe = run_reporting(
        &shell,
        window,
        "other.probe",
        object(&[("id", &first_id)]),
        cx,
    );
    assert_eq!(
        probe,
        serde_json::json!({
            "listed": 0, "released": false,
            "restore": "CaptureNotFound", "retain": "DocumentNotFound",
        })
    );
    assert_eq!(retained_text(&first_id, cx).as_deref(), Some("first"));

    // Restoring while the captured document is open preserves both: the older
    // text opens as a separate document and the capture stays retained.
    let restored = run_reporting(
        &shell,
        window,
        "owner.restore",
        object(&[("id", &first_id)]),
        cx,
    );
    assert_eq!(restored["outcome"], "preserved");
    assert_eq!(restored["owner"], identity);
    assert_eq!(restored["presented"], restored["document"]);
    assert_ne!(restored["document"], identity);
    cx.read(|cx| {
        let documents = documents.read(cx);
        let copy = documents
            .documents()
            .find(|document| document.identity().to_string() == restored["document"])
            .unwrap();
        assert_eq!(copy.model().read(cx).text(), "first");
        assert!(copy.is_dirty(cx));
        assert_eq!(model.read(cx).text(), "second");
    });
    assert_eq!(retained_text(&first_id, cx).as_deref(), Some("first"));

    // Release deletes only the owner's capture; a second release is false.
    let released = run_reporting(
        &shell,
        window,
        "owner.release",
        object(&[("id", &first_id)]),
        cx,
    );
    assert_eq!(released, true);
    let again = run_reporting(
        &shell,
        window,
        "owner.release",
        object(&[("id", &first_id)]),
        cx,
    );
    assert_eq!(again, false);
    assert_eq!(retained_text(&first_id, cx), None);
    assert_eq!(retained_text(&second_id, cx).as_deref(), Some("second"));

    // A capture of a document that has since closed restores under a fresh
    // identity and is presented in the command's window.
    let closing = cx.new(|_| BufferModel::from_text("closed text"));
    let closing = documents.update(cx, |documents, cx| {
        documents.create_untitled("Notes", closing, cx)
    });
    let closing_identity = cx.read(|cx| {
        documents
            .read(cx)
            .get(closing)
            .unwrap()
            .identity()
            .to_string()
    });
    let third = run_reporting(
        &shell,
        window,
        "owner.retain",
        object(&[("document", &closing_identity)]),
        cx,
    );
    let third_id = third["capture"]["id"].as_str().unwrap().to_owned();
    documents.update(cx, |documents, cx| documents.remove(closing, cx));
    let restored = run_reporting(
        &shell,
        window,
        "owner.restore",
        object(&[("id", &third_id)]),
        cx,
    );
    assert_eq!(restored["outcome"], "restored");
    assert_eq!(restored["resource"], "untitled");
    assert_eq!(restored["presented"], restored["document"]);
    assert_ne!(restored["document"], closing_identity);
    cx.read(|cx| {
        let copy = documents
            .read(cx)
            .documents()
            .find(|document| document.identity().to_string() == restored["document"])
            .unwrap();
        assert_eq!(copy.model().read(cx).text(), "closed text");
        assert_eq!(copy.title(), "Notes");
    });
    let released = run_reporting(
        &shell,
        window,
        "owner.release",
        object(&[("id", &third_id)]),
        cx,
    );
    assert_eq!(released, true);

    // A failing owner keeps its captures, and the native listing shows them
    // with their owner. After a restart the same canonical owner sees them.
    std::fs::write(
        extensions.path().join("@example/recovery-owner/main.js"),
        "throw new Error('activation failed');",
    )
    .unwrap();
    start(cx);
    assert_eq!(retained_text(&second_id, cx).as_deref(), Some("second"));
    let listed = cx.update(crate::app::recovery::recoverable);
    let (sender, receiver) = std::sync::mpsc::channel();
    cx.spawn(async move |_| {
        let _ = sender.send(listed.await);
    })
    .detach();
    cx.run_until_parked();
    let listed = receiver.recv().unwrap().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].capture_key, second_id);
    assert_eq!(listed[0].owner.as_deref(), Some("@example/recovery-owner"));
    std::fs::write(
        extensions.path().join("@example/recovery-owner/main.js"),
        RECOVERY_OWNER,
    )
    .unwrap();
    start(cx);
    let listed = run_reporting(&shell, window, "owner.list", CommandArgumentValue::Null, cx);
    assert_eq!(listed, serde_json::json!([second_id]));
}
