//! Foreground metadata comparison and lifecycle-owned application observation.

use std::collections::HashMap;

use crate::host::{
    lifecycle::ExtensionKey,
    protocol::{
        ApplicationChange, ApplicationEvent, ApplicationSnapshot, WorkbenchObservation,
        WorkbenchSubscriptionId,
    },
};

struct ApplicationChanges(gpui::Entity<()>);
impl gpui::Global for ApplicationChanges {}

pub(crate) fn signal(cx: &mut gpui::App) -> gpui::Entity<()> {
    use gpui::AppContext;
    if let Some(global) = cx.try_global::<ApplicationChanges>() {
        return global.0.clone();
    }
    let signal = cx.new(|_| ());
    cx.set_global(ApplicationChanges(signal.clone()));
    signal
}

pub(crate) fn changed(cx: &mut gpui::App) {
    if let Some(signal) = cx
        .try_global::<ApplicationChanges>()
        .map(|global| global.0.clone())
    {
        signal.update(cx, |_, cx| cx.notify());
    }
}

#[derive(Default)]
pub(crate) struct ApplicationObservation {
    snapshot: ApplicationSnapshot,
    sequence: u64,
    next_subscription: u64,
    subscriptions: HashMap<WorkbenchSubscriptionId, ExtensionKey>,
}

impl ApplicationObservation {
    pub(crate) fn commit(&mut self, snapshot: ApplicationSnapshot) -> Vec<ApplicationEvent> {
        let mut changes = Vec::new();
        for document in &snapshot.documents {
            match self
                .snapshot
                .documents
                .iter()
                .find(|old| old.id == document.id)
            {
                None => changes.push(ApplicationChange::DocumentOpened {
                    document: document.clone(),
                }),
                Some(old) if old != document => changes.push(ApplicationChange::DocumentChanged {
                    document: document.clone(),
                }),
                _ => {}
            }
        }
        for window in &snapshot.windows {
            match self
                .snapshot
                .windows
                .iter()
                .find(|old| old.target == window.target)
            {
                None => changes.push(ApplicationChange::WindowOpened {
                    window: window.clone(),
                }),
                Some(old) => {
                    if old.panes != window.panes || old.current_target != window.current_target {
                        changes.push(ApplicationChange::PresentationsChanged {
                            window: window.clone(),
                        });
                    }
                    if old.workspace != window.workspace {
                        changes.push(ApplicationChange::WorkspaceChanged {
                            workspace: window.workspace.clone(),
                        });
                    }
                }
            }
        }
        for window in &self.snapshot.windows {
            if !snapshot
                .windows
                .iter()
                .any(|new| new.target == window.target)
            {
                changes.push(ApplicationChange::WindowClosed {
                    window: window.target.clone(),
                });
            }
        }
        for document in &self.snapshot.documents {
            if !snapshot.documents.iter().any(|new| new.id == document.id) {
                changes.push(ApplicationChange::DocumentClosed {
                    document: document.id.clone(),
                });
            }
        }
        if snapshot.active_window != self.snapshot.active_window
            || snapshot.active_target != self.snapshot.active_target
        {
            changes.push(ApplicationChange::ActiveTargetChanged {
                active_window: snapshot.active_window.clone(),
                active_target: snapshot.active_target.clone(),
            });
        }
        self.snapshot = snapshot;
        changes
            .into_iter()
            .map(|change| self.push(change))
            .collect()
    }

    /// Sequence a change that snapshot comparison cannot derive.
    pub(crate) fn push(&mut self, change: ApplicationChange) -> ApplicationEvent {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("application event sequence exhausted");
        ApplicationEvent {
            sequence: self.sequence,
            change,
        }
    }

    pub(crate) fn subscribe(&mut self, key: ExtensionKey) -> WorkbenchObservation {
        self.next_subscription = self
            .next_subscription
            .checked_add(1)
            .expect("application subscription identity exhausted");
        let subscription = WorkbenchSubscriptionId::new(self.next_subscription);
        self.subscriptions.insert(subscription, key);
        WorkbenchObservation {
            subscription,
            snapshot: self.snapshot.clone(),
            watermark: self.sequence,
        }
    }

    pub(crate) fn subscriptions(
        &self,
    ) -> impl Iterator<Item = (WorkbenchSubscriptionId, ExtensionKey)> + '_ {
        self.subscriptions
            .iter()
            .map(|(subscription, key)| (*subscription, *key))
    }

    pub(crate) fn unsubscribe(
        &mut self,
        subscription: WorkbenchSubscriptionId,
        key: ExtensionKey,
    ) -> bool {
        if self.subscriptions.get(&subscription) != Some(&key) {
            return false;
        }
        self.subscriptions.remove(&subscription);
        true
    }

    pub(crate) fn remove_lifecycle(&mut self, key: ExtensionKey) {
        self.subscriptions.retain(|_, owner| *owner != key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::protocol::*;

    fn document(revision: u64, dirty: bool) -> DocumentInfo {
        DocumentInfo {
            id: "document".into(),
            title: "Untitled".into(),
            kind: DocumentKind::Untitled,
            resource_uri: None,
            dirty,
            revision,
            buffer: BufferHandle::new(1),
        }
    }

    #[test]
    fn baseline_watermark_and_ordered_changes_have_no_duplicate_initial_event() {
        let mut owner = ApplicationObservation::default();
        let first = ApplicationSnapshot {
            documents: vec![document(0, false)],
            ..Default::default()
        };
        let opened = owner.commit(first.clone());
        assert!(matches!(
            opened[0].change,
            ApplicationChange::DocumentOpened { .. }
        ));
        let key = ExtensionKey::new(ExtensionId::new(1), ExtensionLifecycleId::new(1));
        let observation = owner.subscribe(key);
        assert_eq!(observation.snapshot, first);
        assert_eq!(observation.watermark, opened[0].sequence);
        assert!(owner.commit(first).is_empty());
        let changed = owner.commit(ApplicationSnapshot {
            documents: vec![document(1, true)],
            ..Default::default()
        });
        assert_eq!(changed[0].sequence, observation.watermark + 1);
        assert!(
            matches!(&changed[0].change, ApplicationChange::DocumentChanged { document } if document.dirty && document.revision == 1)
        );
        let clean = owner.commit(ApplicationSnapshot {
            documents: vec![document(2, false)],
            ..Default::default()
        });
        assert!(
            matches!(&clean[0].change, ApplicationChange::DocumentChanged { document } if !document.dirty && document.revision == 2)
        );
        let closed = owner.commit(ApplicationSnapshot::default());
        assert!(
            matches!(&closed[0].change, ApplicationChange::DocumentClosed { document } if document == "document")
        );
        assert_eq!(closed[0].sequence, changed[0].sequence + 2);
    }

    #[test]
    fn workspace_focus_and_presentation_changes_use_their_own_taxonomy() {
        let mut owner = ApplicationObservation::default();
        let target = WindowTarget {
            window: "window".into(),
        };
        let pane = PaneTarget {
            window: target.clone(),
            workbench: "workbench".into(),
            pane: "pane".into(),
        };
        let view = PresentationTarget {
            pane: pane.clone(),
            tab: "tab".into(),
            presentation: "view".into(),
        };
        let window = WindowInfo {
            target: target.clone(),
            current_target: Some(view.clone()),
            panes: vec![PaneInfo {
                target: pane,
                selected: view.clone(),
                tabs: vec![PresentationInfo {
                    target: view.clone(),
                    kind: SurfaceKind::Document,
                    document: Some("document".into()),
                    buffer: Some(BufferHandle::new(1)),
                    tool: None,
                }],
            }],
            workspace: WorkspaceInfo {
                window: target.clone(),
                roots: vec![],
                generation: 0,
            },
        };
        let mut snapshot = ApplicationSnapshot {
            documents: vec![document(0, false)],
            windows: vec![window],
            active_window: Some(target.clone()),
            active_target: Some(view.clone()),
        };
        let opened = owner.commit(snapshot.clone());
        assert!(matches!(
            opened[0].change,
            ApplicationChange::DocumentOpened { .. }
        ));
        assert!(matches!(
            opened[1].change,
            ApplicationChange::WindowOpened { .. }
        ));
        assert!(matches!(
            opened[2].change,
            ApplicationChange::ActiveTargetChanged { .. }
        ));
        snapshot.windows[0].workspace.roots.push(WorkspaceRootInfo {
            id: "root".into(),
            uri: "mem://root/".into(),
        });
        snapshot.windows[0].workspace.generation = 1;
        let roots = owner.commit(snapshot.clone());
        assert_eq!(roots.len(), 1);
        assert!(matches!(
            roots[0].change,
            ApplicationChange::WorkspaceChanged { .. }
        ));
        snapshot.windows[0].current_target = None;
        snapshot.active_target = None;
        let focus = owner.commit(snapshot.clone());
        assert!(matches!(
            focus[0].change,
            ApplicationChange::PresentationsChanged { .. }
        ));
        assert!(matches!(
            focus[1].change,
            ApplicationChange::ActiveTargetChanged {
                active_target: None,
                ..
            }
        ));
        let mut split = snapshot.windows[0].panes[0].clone();
        split.target.pane = "split".into();
        snapshot.windows[0].panes.push(split);
        let presentations = owner.commit(snapshot);
        assert!(matches!(
            presentations[0].change,
            ApplicationChange::PresentationsChanged { .. }
        ));
        let closed = owner.commit(ApplicationSnapshot::default());
        assert!(matches!(
            closed[0].change,
            ApplicationChange::WindowClosed { .. }
        ));
        assert!(matches!(
            closed[1].change,
            ApplicationChange::DocumentClosed { .. }
        ));
        assert!(matches!(
            closed[2].change,
            ApplicationChange::ActiveTargetChanged {
                active_window: None,
                ..
            }
        ));
    }

    #[test]
    fn disposal_and_unload_remove_only_the_owning_lifecycle() {
        let mut owner = ApplicationObservation::default();
        let first = ExtensionKey::new(ExtensionId::new(1), ExtensionLifecycleId::new(1));
        let second = ExtensionKey::new(ExtensionId::new(1), ExtensionLifecycleId::new(2));
        let a = owner.subscribe(first);
        let b = owner.subscribe(second);
        assert!(!owner.unsubscribe(a.subscription, second));
        assert!(owner.unsubscribe(a.subscription, first));
        assert!(!owner.unsubscribe(a.subscription, first));
        assert_eq!(
            owner.subscriptions().collect::<Vec<_>>(),
            vec![(b.subscription, second)]
        );
        owner.remove_lifecycle(first);
        assert_eq!(owner.subscriptions().count(), 1);
        owner.remove_lifecycle(second);
        assert_eq!(owner.subscriptions().count(), 0);
    }
}
