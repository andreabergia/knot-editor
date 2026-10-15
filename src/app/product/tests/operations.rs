//! Semantic resource completion through the product owner.

use super::super::{OpenCommit, OpenFailure};
use super::*;
use crate::app::open::OpenResourceError;

fn begin_open(
    shell: &Entity<ProductShell>,
    uri: &str,
    cx: &mut TestAppContext,
) -> tokio::sync::oneshot::Receiver<Result<OpenCommit, OpenFailure>> {
    let target = cx.read(|cx| current_open_target(shell, cx));
    shell.update(cx, |shell, cx| {
        shell.start_open_operation(
            OpenRequest::from_uri_for_product(ResourceUri::parse(uri).unwrap()),
            target,
            false,
            cx,
        )
    })
}

#[gpui::test]
fn semantic_open_returns_the_actual_created_or_reused_presentation(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    let original = cx.read(|cx| current_open_target(&shell, cx));
    let mut pending = begin_open(&shell, "mem://product/notes.txt", cx);
    cx.run_until_parked();
    let OpenCommit::Document { document, target } = pending.try_recv().unwrap().unwrap() else {
        panic!("file Open returned a workspace");
    };
    assert_ne!(document, placeholder);
    assert_ne!(target.tab, original.tab);
    cx.read(|cx| {
        assert!(target.matches_presentation(&shell.read(cx).workbench, cx));
        assert_eq!(target.surface, TabSurfaceId::Document(document));
        assert_eq!(
            documents
                .read(cx)
                .get(document)
                .unwrap()
                .model()
                .read(cx)
                .text(),
            "loaded text"
        );
    });
    // Admission from another tab must still return the reused file presentation.
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, _| {
            assert!(workbench.activate_tab(original.pane, original.tab));
        })
    });
    let mut reopened = begin_open(&shell, "mem://product/notes.txt", cx);
    cx.run_until_parked();
    let OpenCommit::Document {
        document: shared,
        target: selected,
    } = reopened.try_recv().unwrap().unwrap()
    else {
        panic!("file Open returned a workspace");
    };
    assert_eq!(document, shared);
    assert_eq!(target.pane, selected.pane);
    assert_eq!(target.tab, selected.tab);
    assert_eq!(target.presentation, selected.presentation);
    assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 2);
}

#[gpui::test]
fn semantic_open_returns_committed_roots_and_preserves_resource_errors(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (filesystems, _) = memory_filesystems_with_provider();
    cx.set_global(ApplicationFileSystems(filesystems));
    let placeholder = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(placeholder).unwrap().model().clone());
    let (shell, _) = product_window(placeholder, model, cx);
    let mut pending = begin_open(&shell, "mem://product/src", cx);
    cx.run_until_parked();
    let OpenCommit::Workspace(workspace) = pending.try_recv().unwrap().unwrap() else {
        panic!("folder Open returned a document");
    };
    assert_eq!(workspace.generation(), 1);
    assert_eq!(workspace.root().unwrap().to_string(), "mem://product/src");
    assert!(cx.read(|cx| shell.read(cx).workspace.is_current(&workspace)));
    let mut invalid = begin_open(&shell, "mem://product/invalid.txt", cx);
    cx.run_until_parked();
    assert!(matches!(
        invalid.try_recv().unwrap(),
        Err(OpenFailure::Resource(OpenResourceError::InvalidUtf8 { .. }))
    ));
    let mut unsupported = begin_open(&shell, "other://product/notes.txt", cx);
    cx.run_until_parked();
    assert!(matches!(
        unsupported.try_recv().unwrap(),
        Err(OpenFailure::Resource(OpenResourceError::Provider(_)))
    ));
    assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 1);
    assert!(cx.read(|cx| shell.read(cx).workspace.is_current(&workspace)));
}

#[gpui::test]
async fn semantic_open_rejects_a_replaced_presentation_without_creating_a_document(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let (_, provider) = memory_filesystems_with_provider();
    let (release, gate) = tokio::sync::oneshot::channel();
    let provider = Arc::new(GatedFileSystemProvider {
        inner: provider,
        read_release: Mutex::new(Some(gate)),
        replace_release: Mutex::new(None),
    });
    let mut filesystems = FileSystemProviderRegistry::new();
    filesystems.register("mem", provider.clone()).unwrap();
    cx.set_global(ApplicationFileSystems(Arc::new(filesystems)));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, _) = product_window(document, model.clone(), cx);
    let pending = begin_open(&shell, "mem://product/notes.txt", cx);
    wait_for_api_condition(cx, |_| provider.read_release.lock().unwrap().is_none());
    let target = cx.read(|cx| current_open_target(&shell, cx));
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, cx| {
            assert!(workbench.replace_tab_document(target.pane, target.tab, document, model, cx));
        })
    });
    release.send(()).unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(OpenFailure::InvalidTarget)
    ));
    assert_eq!(cx.read(|cx| documents.read(cx).documents().count()), 1);
}
