use super::*;

fn open_store(path: &std::path::Path, cx: &gpui::TestAppContext) -> RecoveryStore {
    RecoveryStore::open(path.to_owned(), cx.background_executor.clone())
}

fn draft(document_id: uuid::Uuid, text: &str) -> SnapshotDraft {
    SnapshotDraft {
        document_id,
        title: "Untitled".into(),
        resource_uri: None,
        state: SnapshotState::Untitled,
        provider_version: None,
        text: text.into(),
        captured_at_ms: now_ms(),
    }
}

#[gpui::test]
async fn automatic_captures_survive_database_reopen(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.sqlite3");
    let id = uuid::Uuid::new_v4();
    let store = open_store(&path, cx);
    assert!(!path.exists());
    store.submit_automatic(draft(id, "unsaved"));
    store.flush().await.unwrap();
    assert!(path.exists());
    drop(store);
    let reopened = open_store(&path, cx);
    let stored = reopened.automatic(id).await.unwrap().unwrap();
    assert_eq!(stored.text, "unsaved");
    assert_eq!(stored.kind, SnapshotKind::Automatic);
    assert_eq!(stored.state, SnapshotState::Untitled);
    assert_eq!(reopened.last_error(), None);
}

#[gpui::test]
async fn coalesced_captures_persist_only_the_latest(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.sqlite3");
    let id = uuid::Uuid::new_v4();
    let store = open_store(&path, cx);
    store.submit_automatic(draft(id, "first"));
    store.submit_automatic(draft(id, "second"));
    store.flush().await.unwrap();
    assert_eq!(
        open_store(&path, cx)
            .automatic(id)
            .await
            .unwrap()
            .unwrap()
            .text,
        "second"
    );
}

#[gpui::test]
async fn identical_captures_skip_writes(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.sqlite3");
    let id = uuid::Uuid::new_v4();
    let store = open_store(&path, cx);
    store.submit_automatic(draft(id, "same"));
    store.flush().await.unwrap();
    std::fs::remove_file(&path).unwrap();
    store.submit_automatic(draft(id, "same"));
    store.flush().await.unwrap();
    assert!(!path.exists());
}

#[gpui::test]
async fn retiring_a_document_removes_its_automatic_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.sqlite3");
    let id = uuid::Uuid::new_v4();
    let store = open_store(&path, cx);
    store.submit_automatic(draft(id, "discarded"));
    store.flush().await.unwrap();
    assert!(open_store(&path, cx).automatic(id).await.unwrap().is_some());
    store.retire_automatic(id);
    store.flush().await.unwrap();
    assert!(open_store(&path, cx).automatic(id).await.unwrap().is_none());
}

#[gpui::test]
async fn retained_rows_are_immutable_and_survive_automatic_captures(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.sqlite3");
    let id = uuid::Uuid::new_v4();
    let store = open_store(&path, cx);
    assert!(
        store
            .retain("owner".into(), "retained-1".into(), draft(id, "checkpoint"))
            .await
            .unwrap()
            .is_some()
    );
    // Reusing the same opaque key never overwrites the immutable capture.
    assert!(
        store
            .retain("owner".into(), "retained-1".into(), draft(id, "newer"))
            .await
            .unwrap()
            .is_none()
    );
    // A later automatic capture is a separate role and does not replace it.
    store.submit_automatic(draft(id, "automatic"));
    store.flush().await.unwrap();
    let snapshots = open_store(&path, cx).snapshots().await.unwrap();
    let retained: Vec<_> = snapshots
        .iter()
        .filter(|snapshot| snapshot.kind == SnapshotKind::Retained)
        .collect();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].text, "checkpoint");
    let automatic = snapshots
        .iter()
        .find(|snapshot| snapshot.kind == SnapshotKind::Automatic)
        .unwrap();
    assert_eq!(automatic.text, "automatic");
    assert!(
        store
            .release("owner".into(), "retained-1".into())
            .await
            .unwrap()
    );
    assert!(
        open_store(&path, cx)
            .snapshots()
            .await
            .unwrap()
            .iter()
            .all(|snapshot| snapshot.kind != SnapshotKind::Retained)
    );
    // Releasing does not touch the automatic protection.
    assert_eq!(
        open_store(&path, cx)
            .automatic(id)
            .await
            .unwrap()
            .unwrap()
            .text,
        "automatic"
    );
}

#[gpui::test]
async fn discard_removes_either_role_and_transfer_moves_protection_atomically(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.sqlite3");
    let orphan = uuid::Uuid::new_v4();
    let store = open_store(&path, cx);
    store.submit_automatic(draft(orphan, "orphan"));
    store.flush().await.unwrap();
    assert!(
        store
            .retain("owner".into(), "kept".into(), draft(orphan, "kept"))
            .await
            .unwrap()
            .is_some()
    );

    let copy = draft(uuid::Uuid::new_v4(), "orphan");
    store
        .transfer_automatic(orphan, copy.clone())
        .await
        .unwrap();
    let reopened = open_store(&path, cx);
    assert!(reopened.automatic(orphan).await.unwrap().is_none());
    assert_eq!(
        reopened
            .automatic(copy.document_id)
            .await
            .unwrap()
            .unwrap()
            .text,
        "orphan"
    );
    // The retained capture of the same document is a separate role.
    assert!(
        reopened
            .snapshot(SnapshotKind::Retained, "kept".into())
            .await
            .unwrap()
            .is_some()
    );

    assert!(
        store
            .discard(SnapshotKind::Retained, "kept".into())
            .await
            .unwrap()
    );
    assert!(
        store
            .discard(SnapshotKind::Automatic, copy.document_id.to_string())
            .await
            .unwrap()
    );
    assert!(
        !store
            .discard(SnapshotKind::Automatic, copy.document_id.to_string())
            .await
            .unwrap()
    );
    assert!(open_store(&path, cx).snapshots().await.unwrap().is_empty());
}

#[gpui::test]
async fn incompatible_or_corrupt_databases_are_preserved(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let future = dir.path().join("future.sqlite3");
    let connection = Connection::open(&future).unwrap();
    connection.execute_batch(SCHEMA).unwrap();
    connection
        .pragma_update(None, "application_id", APPLICATION_ID)
        .unwrap();
    connection.pragma_update(None, "user_version", 999).unwrap();
    drop(connection);
    let store = open_store(&future, cx);
    store.submit_automatic(draft(uuid::Uuid::new_v4(), "kept"));
    assert!(store.flush().await.is_err());
    assert!(store.last_error().is_some());
    // The incompatible database is preserved, not replaced.
    let connection = Connection::open(&future).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 999);
    drop(connection);

    let corrupt = dir.path().join("corrupt.sqlite3");
    std::fs::write(&corrupt, "not a database").unwrap();
    let store = open_store(&corrupt, cx);
    store.submit_automatic(draft(uuid::Uuid::new_v4(), "kept"));
    assert!(store.flush().await.is_err());
    assert_eq!(std::fs::read(&corrupt).unwrap(), b"not a database");
}

#[gpui::test]
async fn interrupted_capture_leaves_the_previous_row_durable(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.sqlite3");
    let id = uuid::Uuid::new_v4();
    let store = open_store(&path, cx);
    store.submit_automatic(draft(id, "durable"));
    store.flush().await.unwrap();
    // A newer capture is still inside its debounce window when the process
    // disappears; the durable row is the previously committed one.
    store.submit_automatic(draft(id, "in flight"));
    drop(store);
    assert_eq!(
        open_store(&path, cx)
            .automatic(id)
            .await
            .unwrap()
            .unwrap()
            .text,
        "durable"
    );
    // The next capture commits the new text.
    let reopened = open_store(&path, cx);
    reopened.submit_automatic(draft(id, "in flight"));
    reopened.flush().await.unwrap();
    assert_eq!(
        open_store(&path, cx)
            .automatic(id)
            .await
            .unwrap()
            .unwrap()
            .text,
        "in flight"
    );
}

#[gpui::test]
async fn write_failure_keeps_the_capture_and_reports(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "not a directory").unwrap();
    let path = blocker.join("recovery.sqlite3");
    let store = open_store(&path, cx);
    store.submit_automatic(draft(uuid::Uuid::new_v4(), "unsaved"));
    assert!(store.flush().await.is_err());
    assert!(store.last_error().is_some());
    // Nothing durable was written and the pending capture is retained for a
    // later retry; editing is never blocked by the failure.
    assert!(!path.exists());
}

#[test]
fn coalescing_never_exceeds_the_maximum_interval() {
    let start = Instant::now();
    let settle = Duration::from_millis(1000);
    let max_interval = Duration::from_millis(5000);
    // A change well after the burst began is capped by the interval.
    let late = start + Duration::from_millis(4500);
    assert_eq!(
        next_due(late, start, settle, max_interval),
        start + max_interval
    );
    // A settled edit uses the trailing-edge delay.
    let quiet = start + Duration::from_millis(2000);
    assert_eq!(next_due(quiet, quiet, settle, max_interval), quiet + settle);
}
