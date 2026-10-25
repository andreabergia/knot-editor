//! Session replacement: one revalidated foreground commit that swaps the
//! document presentations of a set of windows.
//!
//! Admission claims the protected-closure slot, so a replacement never
//! interleaves with closure prompts, quit, or another replacement. Staging
//! validates roots, reads destination resources, and loads retained captures
//! off the foreground without creating documents. The commit then revalidates
//! every captured window, the exact set of presented source documents, and the
//! retained revision of each document whose final view is removed. Any mismatch
//! rejects with nothing changed; otherwise the windows change in one turn, so
//! observers see only the committed result.

use super::super::extension_host::handlers::{AccessError, OpenError, ReplaceError};
use super::*;
use crate::app::recovery::{RestoreOutcome, RestoredResource, StagedRestore};
use crate::host::{declarations::HostError as _, protocol::WindowTarget};

/// One document presented in a window leaving its current session, with the
/// capture that retains its text when its final view is removed.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub(crate) struct SuspendedDocument {
    pub(crate) document: String,
    pub(crate) capture: Option<String>,
}

/// One document reference of a destination window.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub(crate) struct SessionEntry {
    pub(crate) uri: Option<String>,
    pub(crate) document: Option<String>,
    pub(crate) capture: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub(crate) struct DestinationWindow {
    /// An existing window to reuse; `None` creates a window.
    pub(crate) target: Option<String>,
    pub(crate) roots: Vec<String>,
    pub(crate) entries: Vec<SessionEntry>,
    #[serde(default, deserialize_with = "index")]
    pub(crate) selected: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub(crate) struct ReplaceWindowsRequest {
    pub(crate) suspend: Vec<SuspendedDocument>,
    pub(crate) windows: Vec<DestinationWindow>,
    #[serde(default)]
    pub(crate) release: Vec<String>,
    #[serde(default, deserialize_with = "index")]
    pub(crate) focus: Option<usize>,
}

/// Transported JSON numbers are doubles; the facade validates them as
/// nonnegative safe integers.
fn index<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<usize>, D::Error> {
    use serde::Deserialize;
    Ok(Option::<f64>::deserialize(deserializer)?.map(|value| value as usize))
}

/// How one destination entry was resolved.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum EntryOutcome {
    /// A live document already held the identity or resource and was presented.
    Live { document: String },
    /// The entry's capture was restored as a new live document.
    Restored {
        document: String,
        resource: &'static str,
    },
    /// The entry's file was opened.
    Opened { document: String },
    /// The clean file or untitled document no longer exists; nothing was presented.
    Missing,
    /// Staging this entry failed; it was skipped and the rest committed.
    Failed { code: &'static str, message: String },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct ReplacedWindow {
    pub(crate) window: WindowTarget,
    pub(crate) entries: Vec<EntryOutcome>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct ReplaceWindowsResult {
    pub(crate) windows: Vec<ReplacedWindow>,
    /// Whether every restored dirty document reached durable automatic
    /// protection before resolution.
    pub(crate) protected: bool,
}

/// An entry's staged content, read before the commit.
enum Staged {
    /// A live document matched at staging; the commit checks it again.
    Live,
    Restore(StagedRestore),
    Open(OpenResource),
    Missing,
    Failed(OpenError, String),
}

struct CapturedWindow {
    shell: Entity<ProductShell>,
    handle: AnyWindowHandle,
}

impl CapturedWindow {
    fn is_live(&self, cx: &mut App) -> bool {
        cx.windows().contains(&self.handle)
            && cx
                .update_window(self.handle, |root, _, _| {
                    root.entity_id() == self.shell.entity_id()
                })
                .unwrap_or(false)
    }

    fn target(&self) -> WindowTarget {
        WindowTarget {
            window: self.handle.window_id().as_u64().to_string(),
        }
    }
}

fn capture(window: &str, cx: &mut App) -> Result<CapturedWindow, AccessError> {
    let (shell, handle) = operations::capture_window(
        &WindowTarget {
            window: window.to_owned(),
        },
        cx,
    )?;
    Ok(CapturedWindow { shell, handle })
}

fn parse_uri(uri: &str) -> Result<super::super::resource::ResourceUri, OpenError> {
    super::super::resource::ResourceUri::parse(uri).map_err(|_| OpenError::InvalidUri)
}

/// Replace the document presentations of the given windows in one commit.
pub(crate) fn replace_windows(
    request: ReplaceWindowsRequest,
    owner: String,
    caller: operations::OperationCaller,
    completion: impl FnOnce(Result<ReplaceWindowsResult, ReplaceError>) + 'static,
    cx: &mut App,
) -> Result<(), ReplaceError> {
    if ProductShell::protected_closure_is_active(cx) {
        return Err(ReplaceError::OperationInProgress);
    }
    let mut roots = Vec::new();
    for window in &request.windows {
        if window.roots.len() > 1 {
            return Err(ReplaceError::TooManyWorkspaceRoots);
        }
        roots.push(
            window
                .roots
                .iter()
                .map(|uri| parse_uri(uri))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    let mut named = HashSet::new();
    let mut destinations = Vec::new();
    for window in &request.windows {
        destinations.push(match &window.target {
            Some(target) => {
                if !named.insert(target.clone()) {
                    return Err(AccessError::InvalidTarget.into());
                }
                Some(capture(target, cx)?)
            }
            None => None,
        });
    }
    let mut released = Vec::new();
    for target in &request.release {
        if !named.insert(target.clone()) {
            return Err(AccessError::InvalidTarget.into());
        }
        released.push(capture(target, cx)?);
    }
    if request
        .focus
        .is_some_and(|focus| focus >= request.windows.len())
    {
        return Err(AccessError::InvalidTarget.into());
    }
    ProductShell::set_protected_closure_active(true, cx);
    let providers = ProductShell::filesystems(cx);
    let live = live_entries(&request, cx);
    cx.spawn(async move |cx| {
        let staged = stage(&request, roots, live, &owner, providers, cx).await;
        let committed = cx.update(|cx| {
            ProductShell::set_protected_closure_active(false, cx);
            let (roots, entries) = staged?;
            commit(
                &request,
                &owner,
                &caller,
                destinations,
                released,
                roots,
                entries,
                cx,
            )
        });
        let (result, restored) = match committed {
            Ok(Ok(committed)) => committed,
            Ok(Err(error)) => return completion(Err(error)),
            Err(_) => return completion(Err(AccessError::Cancelled.into())),
        };
        // Restored dirty documents are durably protected before resolution so
        // the caller may release the captures they came from.
        let protected = if restored {
            match cx.update(|cx| crate::app::recovery::drain(cx)) {
                Ok(drain) => drain.await.is_ok(),
                Err(_) => false,
            }
        } else {
            true
        };
        completion(Ok(ReplaceWindowsResult {
            protected,
            ..result
        }));
    })
    .detach();
    Ok(())
}

/// The live document an entry refers to, by identity or resource.
fn live_document(entry: &SessionEntry, cx: &App) -> Option<DocumentId> {
    let documents = ProductShell::documents(cx);
    let documents = documents.read(cx);
    entry
        .document
        .as_deref()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .and_then(|id| {
            documents
                .document_by_identity(id)
                .map(|document| document.id())
        })
        .or_else(|| {
            entry
                .uri
                .as_deref()
                .and_then(|uri| parse_uri(uri).ok())
                .and_then(|uri| documents.document_for_resource(&uri))
        })
}

fn live_entries(request: &ReplaceWindowsRequest, cx: &App) -> Vec<Vec<bool>> {
    request
        .windows
        .iter()
        .map(|window| {
            window
                .entries
                .iter()
                .map(|entry| live_document(entry, cx).is_some())
                .collect()
        })
        .collect()
}

type StagedWindows = (
    Vec<Vec<super::super::resource::ResourceUri>>,
    Vec<Vec<Staged>>,
);

async fn stage(
    request: &ReplaceWindowsRequest,
    roots: Vec<Vec<super::super::resource::ResourceUri>>,
    live: Vec<Vec<bool>>,
    owner: &str,
    providers: Arc<FileSystemProviderRegistry>,
    cx: &mut AsyncApp,
) -> Result<StagedWindows, ReplaceError> {
    let mut validated = Vec::new();
    for window in roots {
        let mut directories = Vec::new();
        for uri in window {
            match super::super::open::load_resource_kind(
                providers.clone(),
                uri,
                Some(ResourceKind::Directory),
            )
            .await
            {
                Ok(OpenResource::Directory(uri)) => directories.push(uri),
                Ok(_) => return Err(OpenError::ResourceWrongKind.into()),
                Err(error) => return Err(operations::resource_error(error).into()),
            }
        }
        validated.push(directories);
    }
    let mut staged = Vec::new();
    for (window, live) in request.windows.iter().zip(live) {
        let mut entries = Vec::new();
        for (entry, live) in window.entries.iter().zip(live) {
            entries.push(if live {
                Staged::Live
            } else {
                stage_entry(entry, owner, providers.clone(), cx).await
            });
        }
        staged.push(entries);
    }
    Ok((validated, staged))
}

async fn stage_entry(
    entry: &SessionEntry,
    owner: &str,
    providers: Arc<FileSystemProviderRegistry>,
    cx: &mut AsyncApp,
) -> Staged {
    use crate::app::recovery::RestoreError;
    if let Some(capture) = &entry.capture {
        let restore = cx.update(|cx| {
            crate::app::recovery::stage_retained(
                owner.to_owned(),
                capture.clone(),
                providers.clone(),
                cx,
            )
        });
        let Ok(restore) = restore else {
            return Staged::Failed(AccessError::Cancelled.into(), "cancelled".into());
        };
        return match restore.await {
            Ok(staged) => Staged::Restore(staged),
            Err(RestoreError::Unavailable) => Staged::Failed(
                OpenError::DocumentNotFound,
                "the retained capture is no longer available".into(),
            ),
            Err(RestoreError::Resource(error)) => {
                let message = error.to_string();
                Staged::Failed(operations::resource_error(error), message)
            }
            Err(RestoreError::Store(message)) => {
                Staged::Failed(OpenError::ProviderFailure, message)
            }
        };
    }
    let Some(uri) = &entry.uri else {
        return Staged::Missing;
    };
    let uri = match parse_uri(uri) {
        Ok(uri) => uri,
        Err(error) => return Staged::Failed(error, format!("invalid URI {uri}")),
    };
    match super::super::open::load_resource_kind(providers, uri, Some(ResourceKind::File)).await {
        Ok(OpenResource::Missing(_)) => Staged::Missing,
        Ok(resource) => Staged::Open(resource),
        Err(error) => {
            let message = error.to_string();
            Staged::Failed(operations::resource_error(error), message)
        }
    }
}

fn source_documents(sources: &[&CapturedWindow], cx: &App) -> Vec<DocumentId> {
    let mut documents = Vec::new();
    for window in sources {
        for pane in window.shell.read(cx).workbench.read(cx).panes() {
            for tab in pane.tabs() {
                if let Some(document) = tab.document_id()
                    && !documents.contains(&document)
                {
                    documents.push(document);
                }
            }
        }
    }
    documents
}

fn source_view_count(sources: &[&CapturedWindow], document: DocumentId, cx: &App) -> usize {
    sources
        .iter()
        .map(|window| {
            window
                .shell
                .read(cx)
                .workbench
                .read(cx)
                .view_count(document)
        })
        .sum()
}

/// Whether a document's text would be lost if it stopped being live.
fn needs_capture(document: &super::super::documents::Document, cx: &App) -> bool {
    document.is_dirty(cx)
        || (matches!(document.state(), DocumentState::Untitled { .. })
            && !document.model().read(cx).text().is_empty())
}

fn conflict(reason: &str) -> ReplaceError {
    ReplaceError::Conflict {
        reason: reason.to_owned(),
    }
}

#[allow(clippy::too_many_arguments)]
fn commit(
    request: &ReplaceWindowsRequest,
    owner: &str,
    caller: &operations::OperationCaller,
    destinations: Vec<Option<CapturedWindow>>,
    released: Vec<CapturedWindow>,
    roots: Vec<Vec<super::super::resource::ResourceUri>>,
    staged: Vec<Vec<Staged>>,
    cx: &mut App,
) -> Result<(ReplaceWindowsResult, bool), ReplaceError> {
    if !caller.is_live(cx) {
        return Err(AccessError::Cancelled.into());
    }
    let sources = destinations
        .iter()
        .flatten()
        .chain(released.iter())
        .collect::<Vec<_>>();
    if sources.iter().any(|window| !window.is_live(cx)) {
        return Err(AccessError::InvalidTarget.into());
    }
    let documents = ProductShell::documents(cx);
    let presented = source_documents(&sources, cx);
    let identities = presented
        .iter()
        .filter_map(|document| documents.read(cx).get(*document))
        .map(|document| document.identity().to_string())
        .collect::<HashSet<_>>();
    let suspended = request
        .suspend
        .iter()
        .map(|document| document.document.clone())
        .collect::<HashSet<_>>();
    if identities != suspended || suspended.len() != request.suspend.len() {
        return Err(conflict("the windows' documents changed"));
    }
    // Entries that present a live document keep it live, so it never needs a
    // capture even when its source views are removed.
    let kept = request
        .windows
        .iter()
        .flat_map(|window| &window.entries)
        .filter_map(|entry| live_document(entry, cx))
        .collect::<HashSet<_>>();
    let workbenches = cx.global::<ApplicationWorkbenches>();
    for document in &presented {
        let total = workbenches.view_count(*document, cx);
        if kept.contains(document) || source_view_count(&sources, *document, cx) < total {
            continue;
        }
        let Some(state) = documents.read(cx).get(*document) else {
            continue;
        };
        if !needs_capture(state, cx) {
            continue;
        }
        let identity = state.identity();
        let revision = state.model().read(cx).revision();
        let retained = request
            .suspend
            .iter()
            .find(|suspended| suspended.document == identity.to_string())
            .and_then(|suspended| suspended.capture.as_deref())
            .is_some_and(|capture| {
                crate::app::recovery::captures_revision(owner, capture, identity, revision, cx)
            });
        if !retained {
            return Err(conflict("a document changed after it was retained"));
        }
    }

    let mut created = Vec::new();
    for _ in destinations.iter().filter(|window| window.is_none()) {
        match launch::open_session_window(cx) {
            Ok((shell, handle)) => created.push(CapturedWindow { shell, handle }),
            Err(_) => {
                for window in created {
                    let _ = cx.update_window(window.handle, |_, window, _| window.remove_window());
                }
                return Err(AccessError::InvalidTarget.into());
            }
        }
    }
    let mut created = created.into_iter();
    let destinations = destinations
        .into_iter()
        .map(|window| window.unwrap_or_else(|| created.next().expect("created per new window")))
        .collect::<Vec<_>>();

    for window in destinations.iter().chain(released.iter()) {
        window.shell.update(cx, |shell, cx| {
            shell.next_open_generation();
            shell
                .workbench
                .update(cx, |workbench, _| workbench.remove_document_tabs());
            cx.notify();
        });
    }
    for window in &released {
        let terminals = window
            .shell
            .read(cx)
            .workbench
            .read(cx)
            .tab_snapshots()
            .iter()
            .any(|tab| matches!(tab.surface, TabSurfaceId::Terminal(_)));
        if terminals {
            // A window with a terminal stays open, so a switch never stops it.
            window.shell.update(cx, |shell, cx| {
                let _ = shell.replace_workspace_roots(Vec::new(), cx);
                shell.status = "session moved to another window".into();
                cx.notify();
            });
        } else {
            let _ = cx.update_window(window.handle, |_, window, cx| {
                tab_drag::cancel_source_window(window, cx);
                window.remove_window();
            });
        }
    }

    let mut restored = false;
    let mut outcomes = Vec::new();
    for ((window, (destination, roots)), staged) in destinations
        .iter()
        .zip(request.windows.iter().zip(roots))
        .zip(staged)
    {
        let mut entries = Vec::new();
        let mut presented = Vec::new();
        for (entry, staged) in destination.entries.iter().zip(staged) {
            let (outcome, document) = resolve(entry, staged, &mut restored, cx);
            entries.push(outcome);
            presented.push(document);
        }
        let handle = window.handle;
        window.shell.update(cx, |shell, cx| {
            let _ = shell.replace_workspace_roots(roots, cx);
            let mut tabs = Vec::new();
            for document in presented {
                tabs.push(document.map(|document| shell.present_session_document(document, cx)));
            }
            let selected = destination
                .selected
                .and_then(|index| tabs.get(index).copied().flatten())
                .or_else(|| tabs.iter().flatten().next().copied());
            if let Some((pane, tab)) = selected {
                shell
                    .workbench
                    .update(cx, |workbench, _| workbench.activate_tab(pane, tab));
            }
            if shell.workbench.read(cx).panes().is_empty() {
                shell.replace_empty_workbench(handle, cx);
            }
            shell.status = "session opened".into();
            cx.notify();
        });
        outcomes.push(ReplacedWindow {
            window: window.target(),
            entries,
        });
    }

    let workbenches = cx.global::<ApplicationWorkbenches>();
    let unpresented = presented
        .into_iter()
        .filter(|document| workbenches.view_count(*document, cx) == 0)
        .collect::<Vec<_>>();
    documents.update(cx, |documents, cx| {
        for document in unpresented {
            documents.remove(document, cx);
        }
    });

    for (index, window) in destinations.iter().enumerate() {
        let focused = request.focus == Some(index);
        let shell = window.shell.clone();
        let _ = cx.update_window(window.handle, |_, native, cx| {
            if focused {
                native.activate_window();
            }
            shell.update(cx, |shell, cx| {
                if focused {
                    shell.focus_active_surface(native, cx);
                }
                shell.observe_application_focus(native, cx);
            });
        });
    }
    Ok((
        ReplaceWindowsResult {
            windows: outcomes,
            protected: true,
        },
        restored,
    ))
}

/// Resolve one staged entry into its outcome and the document to present.
fn resolve(
    entry: &SessionEntry,
    staged: Staged,
    restored: &mut bool,
    cx: &mut App,
) -> (EntryOutcome, Option<DocumentId>) {
    let identity = |document: DocumentId, cx: &App| {
        ProductShell::documents(cx)
            .read(cx)
            .get(document)
            .map(|document| document.identity().to_string())
            .unwrap_or_default()
    };
    // One source of truth per file: a live document always wins over staged
    // content, including documents restored by hot exit or open elsewhere.
    if let Some(document) = live_document(entry, cx) {
        let outcome = EntryOutcome::Live {
            document: identity(document, cx),
        };
        return (outcome, Some(document));
    }
    match staged {
        Staged::Live => (
            EntryOutcome::Failed {
                code: "DocumentNotFound",
                message: "the document closed while the session was opening".into(),
            },
            None,
        ),
        Staged::Restore(staged) => match crate::app::recovery::commit_staged(staged, cx) {
            Ok(RestoreOutcome::Restored { document, resource }) => {
                *restored = true;
                let resource = match resource {
                    RestoredResource::Untitled => "untitled",
                    RestoredResource::Unchanged => "unchanged",
                    RestoredResource::Missing => "missing",
                    RestoredResource::Changed => "changed",
                };
                (
                    EntryOutcome::Restored {
                        document: identity(document, cx),
                        resource,
                    },
                    Some(document),
                )
            }
            Ok(
                RestoreOutcome::AlreadyOpen { document }
                | RestoreOutcome::Preserved {
                    owner: document, ..
                },
            ) => (
                EntryOutcome::Live {
                    document: identity(document, cx),
                },
                Some(document),
            ),
            Err(error) => (
                EntryOutcome::Failed {
                    code: "RecoveryUnavailable",
                    message: error.to_string(),
                },
                None,
            ),
        },
        Staged::Open(OpenResource::File { uri, text, version }) => {
            let documents = ProductShell::documents(cx);
            let model = cx.new(|_| BufferModel::from_text(text));
            let document = documents.update(cx, |documents, cx| {
                documents.create_persisted(resource_title(&uri), model, uri, 0, version, cx)
            });
            (
                EntryOutcome::Opened {
                    document: identity(document, cx),
                },
                Some(document),
            )
        }
        Staged::Open(_) | Staged::Missing => (EntryOutcome::Missing, None),
        Staged::Failed(error, message) => (
            EntryOutcome::Failed {
                code: error.code(),
                message,
            },
            None,
        ),
    }
}

impl ProductShell {
    /// Present a document in this window's normal document group, reusing an
    /// existing tab.
    fn present_session_document(
        &mut self,
        document: DocumentId,
        cx: &mut Context<Self>,
    ) -> (PaneId, TabId) {
        let existing = self.workbench.read(cx).panes().iter().find_map(|pane| {
            pane.tabs()
                .iter()
                .find(|tab| tab.document_id() == Some(document))
                .map(|tab| (pane.id(), tab.id()))
        });
        if let Some(existing) = existing {
            return existing;
        }
        let model = Self::documents(cx)
            .read(cx)
            .get(document)
            .expect("presented documents are live")
            .model()
            .clone();
        self.workbench.update(cx, |workbench, cx| {
            workbench.open_document_in_normal_group(document, model, cx)
        })
    }
}

/// Activate a product window and focus its active surface.
pub(crate) fn focus_window(target: &WindowTarget, cx: &mut App) -> Result<(), AccessError> {
    let (shell, handle) = operations::capture_window(target, cx)?;
    cx.update_window(handle, |_, window, cx| {
        window.activate_window();
        shell.read(cx).focus_active_surface(window, cx);
    })
    .map_err(|_| AccessError::InvalidTarget)
}
