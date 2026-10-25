//! Native commands that edit the window's ordered workspace roots.
//!
//! Every edit is a replacement of the whole root set computed from the live
//! state on the foreground, so retained roots keep their identity and the
//! explorer keeps their expansion. Remove and Move act on the explorer's
//! selected root: the root row itself or the root shown above the selection.

use super::*;
use crate::app::resource::ResourceUri;
use crate::host::protocol::CommandOutcome;

impl ProductShell {
    /// Choose a directory in the window's transient slot and append it.
    pub(super) fn start_add_folder(
        &mut self,
        target: &ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = match self.begin_folder_picker(None, window, cx) {
            Ok(receiver) => receiver,
            Err(_) => {
                self.status = "another interaction is open in this window".into();
                cx.notify();
                return;
            }
        };
        let handle = target.window;
        let task = cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(TransientOutcome::Folder(path)) => path,
                Ok(TransientOutcome::Failed(message)) => {
                    Self::set_status(&this, format!("cannot choose folder: {message}"), cx);
                    return;
                }
                _ => return,
            };
            let (sender, resolved) = tokio::sync::oneshot::channel();
            let _ = cx.update(|cx| {
                operations::resolve_directory(
                    path,
                    move |result| {
                        let _ = sender.send(result);
                    },
                    cx,
                )
            });
            let Ok(result) = resolved.await else {
                return;
            };
            Self::in_window(&this, handle, cx, |shell, _, cx| match result {
                Ok(uri) => shell.add_workspace_root(uri, cx),
                Err(error) => {
                    use crate::host::declarations::HostError;
                    shell.status = format!("cannot add folder: {}", error.code()).into();
                    cx.notify();
                }
            });
        });
        self.tasks.push(task);
    }

    pub(super) fn add_workspace_root(&mut self, uri: ResourceUri, cx: &mut Context<Self>) {
        let mut roots = self.root_uris();
        if roots.contains(&uri) {
            self.status = format!("{uri} is already in the workspace").into();
            cx.notify();
            return;
        }
        roots.push(uri);
        self.replace_workspace_roots(roots, cx);
    }

    pub(super) fn remove_selected_root(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        let index = self.selected_root_index(cx)?;
        let mut roots = self.root_uris();
        roots.remove(index);
        self.replace_workspace_roots(roots, cx);
        Ok(())
    }

    /// Move the selected root one position earlier (`up`) or later.
    pub(super) fn move_selected_root(
        &mut self,
        up: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        let index = self.selected_root_index(cx)?;
        let mut roots = self.root_uris();
        let destination = if up {
            index.checked_sub(1)
        } else {
            Some(index + 1).filter(|destination| *destination < roots.len())
        }
        .ok_or(CommandOutcome::Unavailable)?;
        roots.swap(index, destination);
        self.replace_workspace_roots(roots, cx);
        Ok(())
    }

    fn root_uris(&self) -> Vec<ResourceUri> {
        self.workspace
            .snapshot()
            .roots()
            .iter()
            .map(|root| root.uri.clone())
            .collect()
    }

    fn selected_root_index(&self, cx: &App) -> Result<usize, CommandOutcome> {
        let tree = self
            .workspace_tree
            .as_ref()
            .ok_or(CommandOutcome::Unavailable)?;
        let selected = tree
            .read(cx)
            .selected_root()
            .ok_or(CommandOutcome::Unavailable)?
            .id;
        self.workspace
            .snapshot()
            .roots()
            .iter()
            .position(|root| root.id == selected)
            .ok_or(CommandOutcome::Unavailable)
    }
}
