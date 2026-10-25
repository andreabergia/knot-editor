//! Native listing of recoverable unsaved documents.
//!
//! The listing is the extension-independent path for snapshots that were not
//! restored silently at startup: retained captures (labelled with their owner,
//! including orphans whose owner no longer references them), older duplicates
//! of one resource, and snapshots whose resource could not be read. It occupies the
//! window's transient slot as a native picker, then asks whether to restore or
//! discard the selected snapshot.

use super::*;
use crate::app::recovery::{
    RestoreError, RestoreOutcome, RestoredResource, SnapshotKind, SnapshotState, StoredSnapshot,
};
use crate::host::protocol::PickerItem;

pub(super) const LISTING_TITLE: &str = "Recover Unsaved Documents";

impl ProductShell {
    /// List recoverable snapshots in the captured window and act on the selection.
    pub(crate) fn start_recovery_listing(
        &mut self,
        target: &ProductCommandTarget,
        cx: &mut Context<Self>,
    ) {
        if self.transient.is_some() {
            self.status = "another interaction is open in this window".into();
            cx.notify();
            return;
        }
        let window = target.window;
        let listing = crate::app::recovery::recoverable(cx);
        let providers = Self::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let snapshots = match listing.await {
                Ok(snapshots) if snapshots.is_empty() => {
                    Self::set_status(&this, "no unsaved documents to recover", cx);
                    return;
                }
                Ok(snapshots) => snapshots,
                Err(error) => {
                    Self::set_status(&this, format!("recovery unavailable: {error}"), cx);
                    return;
                }
            };
            let now = crate::app::recovery::now_ms();
            let items = snapshots
                .iter()
                .enumerate()
                .map(|(index, snapshot)| PickerItem {
                    id: index.to_string(),
                    label: snapshot.title.clone(),
                    description: Some(describe_snapshot(snapshot, now)),
                })
                .collect();
            let Some(Ok(receiver)) = Self::in_window(&this, window, cx, |shell, window, cx| {
                shell.begin_owned_picker(None, LISTING_TITLE.into(), items, window, cx)
            }) else {
                return;
            };
            let Ok(TransientOutcome::Picker(id)) = receiver.await else {
                return;
            };
            let Some(snapshot) = id.parse::<usize>().ok().and_then(|index| snapshots.get(index))
            else {
                return;
            };
            let title = snapshot.title.clone();
            let Some(answer) = Self::in_window(&this, window, cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Info,
                    &format!("Recover “{title}”?"),
                    Some("Restore opens the recovered text as an unsaved document. Discard permanently deletes it."),
                    &[
                        PromptButton::new("Restore"),
                        PromptButton::new("Discard"),
                        PromptButton::cancel("Cancel"),
                    ],
                    cx,
                )
            }) else {
                return;
            };
            let (kind, key) = (snapshot.kind, snapshot.capture_key.clone());
            match answer.await.unwrap_or(2) {
                0 => {
                    let Ok(restore) = cx.update(|cx| {
                        crate::app::recovery::restore(kind, key, providers, cx)
                    }) else {
                        return;
                    };
                    let outcome = restore.await;
                    Self::in_window(&this, window, cx, |shell, window, cx| {
                        shell.present_recovered(outcome, &title, window, cx)
                    });
                }
                1 => {
                    let Ok(discard) =
                        cx.update(|cx| crate::app::recovery::discard_snapshot(kind, key, cx))
                    else {
                        return;
                    };
                    let status = match discard.await {
                        Ok(_) => format!("discarded recovered {title}"),
                        Err(error) => format!("discard failed: {error}"),
                    };
                    Self::set_status(&this, status, cx);
                }
                _ => {}
            }
        });
        self.tasks.push(task);
    }

    fn present_recovered(
        &mut self,
        outcome: Result<RestoreOutcome, RestoreError>,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (document, status) = match outcome {
            Ok(RestoreOutcome::Restored { document, resource }) => (
                document,
                match resource {
                    RestoredResource::Untitled | RestoredResource::Unchanged => {
                        format!("recovered {title}")
                    }
                    RestoredResource::Missing => {
                        format!("recovered {title}; the file no longer exists")
                    }
                    RestoredResource::Changed => {
                        format!("recovered {title}; the file changed since it was captured")
                    }
                },
            ),
            Ok(RestoreOutcome::AlreadyOpen { document }) => {
                (document, format!("{title} is already open"))
            }
            Ok(RestoreOutcome::Preserved { document, .. }) => (
                document,
                format!("{title} is already open; the recovered text opened separately"),
            ),
            Err(error) => {
                self.status = format!("recovery failed: {error}").into();
                cx.notify();
                return;
            }
        };
        self.present_document_in_focused_pane(document, cx);
        self.focus_active_surface(window, cx);
        self.status = status.into();
        cx.notify();
    }

    /// Show a document in the focused pane, reusing this window's existing tab.
    pub(super) fn present_document_in_focused_pane(
        &mut self,
        document: DocumentId,
        cx: &mut Context<Self>,
    ) {
        let Some(window) = self.window else {
            return;
        };
        let Some(pane) = self.workbench.read(cx).focused_pane() else {
            return;
        };
        let tab = pane.active_tab();
        let target = OpenTarget {
            window,
            workbench: self.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            surface: tab.surface_id(),
            presentation: tab.presentation(),
        };
        self.present_open_document(document, &target, false, cx);
    }

    fn set_status(this: &WeakEntity<Self>, status: impl Into<SharedString>, cx: &mut AsyncApp) {
        let status = status.into();
        let _ = this.update(cx, |shell, cx| {
            shell.status = status;
            cx.notify();
        });
    }

    fn in_window<R>(
        this: &WeakEntity<Self>,
        window: AnyWindowHandle,
        cx: &mut AsyncApp,
        access: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) -> R,
    ) -> Option<R> {
        let shell = this.upgrade()?;
        cx.update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| access(shell, window, cx))
        })
        .ok()
    }
}

/// Status line for a window that received startup recovery.
pub(super) fn startup_status(recovery: &crate::app::recovery::StartupRecovery) -> Option<String> {
    if let Some(error) = &recovery.error {
        return Some(format!("recovery unavailable: {error}"));
    }
    let restored = recovery.restored.len();
    match (restored, recovery.remaining) {
        (0, 0) => None,
        (_, 0) => Some(format!(
            "recovered {}",
            plural(restored, "unsaved document")
        )),
        (_, remaining) => Some(format!(
            "{} can be recovered with File › {LISTING_TITLE}…",
            plural(remaining, "unsaved document")
        )),
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn describe_snapshot(snapshot: &StoredSnapshot, now_ms: i64) -> String {
    let state = match snapshot.state {
        SnapshotState::Untitled => "unsaved",
        SnapshotState::Destination => "new file",
        SnapshotState::Persisted => "modified",
    };
    let mut parts = vec![
        state.to_owned(),
        snapshot
            .resource_uri
            .clone()
            .unwrap_or_else(|| "untitled".into()),
        captured_age(now_ms.saturating_sub(snapshot.captured_at_ms)),
    ];
    if snapshot.kind == SnapshotKind::Retained {
        parts.push(match &snapshot.owner {
            Some(owner) => format!("retained by {owner}"),
            None => "retained".into(),
        });
    }
    parts.join(" · ")
}

fn captured_age(elapsed_ms: i64) -> String {
    let minutes = elapsed_ms / 60_000;
    match minutes {
        ..1 => "captured just now".into(),
        1..60 => format!("captured {minutes} min ago"),
        60..1440 => format!("captured {} h ago", minutes / 60),
        _ => format!("captured {} d ago", minutes / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::{captured_age, describe_snapshot};
    use crate::app::recovery::{SnapshotKind, SnapshotState, StoredSnapshot};

    #[test]
    fn listing_descriptions_show_state_location_age_and_role() {
        let snapshot = StoredSnapshot {
            kind: SnapshotKind::Retained,
            capture_key: "key".into(),
            owner: Some("@example/projects".into()),
            document_id: uuid::Uuid::new_v4(),
            title: "notes.txt".into(),
            resource_uri: Some("file:///notes.txt".into()),
            state: SnapshotState::Persisted,
            provider_version: None,
            generation: 1,
            captured_at_ms: 0,
            text: "text".into(),
        };
        assert_eq!(
            describe_snapshot(&snapshot, 3 * 60 * 60_000),
            "modified · file:///notes.txt · captured 3 h ago · retained by @example/projects"
        );
        assert_eq!(captured_age(30_000), "captured just now");
        assert_eq!(captured_age(5 * 60_000), "captured 5 min ago");
        assert_eq!(captured_age(2 * 1440 * 60_000), "captured 2 d ago");
    }
}
