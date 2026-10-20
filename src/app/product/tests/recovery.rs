//! Durable unsaved-document recovery through the product document lifecycle.
use super::*;
use crate::app::recovery::{ApplicationRecovery, SnapshotKind, SnapshotState};

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
async fn generated_documents_are_never_captured(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let model = cx.new(|_| BufferModel::from_text("generated"));
    let document = documents.update(cx, |documents, cx| {
        documents.create_generated("Search Results", model.clone(), cx)
    });
    let identity = cx.read(|cx| documents.read(cx).get(document).unwrap().identity());
    // Generated documents stay clean even after their text is replaced.
    model.update(cx, |model, cx| {
        model.replace(0..0, "prefix ").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    flush(cx).await;
    assert!(
        recovery_store(cx)
            .automatic(identity)
            .await
            .unwrap()
            .is_none()
    );
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
