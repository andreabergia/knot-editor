//! Reopen Closed Tab: each window keeps its last closed tabs as layout tabs
//! with their pane and position, and recreates them through the same per-kind
//! step as a restored layout.
//!
//! Untitled documents are not kept because their text is gone, and tools are
//! not kept because hiding one already remembers where it was.

use super::*;
use crate::app::window_layout::{LayoutTab, WindowLayout};
use crate::app::workbench::ArrangedTab;
use crate::host::protocol::CommandOutcome;

const CLOSED_TAB_LIMIT: usize = 20;

pub(super) struct ClosedTab {
    tab: LayoutTab,
    pane: PaneId,
    index: usize,
}

impl ProductShell {
    /// Remember a tab that is about to close.
    pub(super) fn remember_closed_tab(&mut self, pane: PaneId, tab: TabId, cx: &App) {
        let workbench = self.workbench.read(cx);
        let Some(group) = workbench.pane(pane) else {
            return;
        };
        let Some(index) = group.tabs().iter().position(|item| item.id() == tab) else {
            return;
        };
        let tab = WindowLayout::capture_tab(
            workbench,
            &group.tabs()[index],
            Self::documents(cx).read(cx),
            &|id| Self::terminal_session_directory(id, cx),
            cx,
        );
        let kept = match &tab {
            LayoutTab::Document { uri, .. } => uri.is_some(),
            LayoutTab::Terminal { .. } => true,
            LayoutTab::Tool { .. } => false,
        };
        if !kept {
            return;
        }
        self.closed_tabs.push(ClosedTab { tab, pane, index });
        if self.closed_tabs.len() > CLOSED_TAB_LIMIT {
            self.closed_tabs.remove(0);
        }
    }

    /// Recreate the most recently closed tab: in its pane at its position
    /// when that pane still exists, otherwise in the focused pane. A document
    /// reopens its file and a terminal starts in its last directory.
    pub(super) fn reopen_closed_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        let closed = self.closed_tabs.pop().ok_or(CommandOutcome::Unavailable)?;
        let ClosedTab { tab, pane, index } = closed;
        match tab {
            LayoutTab::Terminal { directory, pinned } => {
                let directory = directory
                    .and_then(|uri| url::Url::parse(&uri).ok())
                    .and_then(|url| url.to_file_path().ok())
                    .filter(|directory| directory.is_dir());
                let (id, view) = match directory {
                    Some(directory) => self.create_terminal_in(Some(directory), cx),
                    None => self.create_terminal(cx),
                };
                self.install_reopened(
                    pane,
                    index,
                    ArrangedTab::Terminal(id, Some(view)),
                    pinned,
                    cx,
                );
                self.focus_active_surface(window, cx);
                Ok(())
            }
            LayoutTab::Document {
                uri: Some(uri),
                view,
                language,
                pinned,
                ..
            } => {
                let uri = crate::app::resource::ResourceUri::parse(&uri)
                    .map_err(|_| CommandOutcome::InvalidTarget)?;
                let live = Self::documents(cx).read(cx).document_for_resource(&uri);
                if let Some(document) = live {
                    self.install_reopened_document(
                        pane, index, document, view, language, pinned, cx,
                    );
                    self.focus_active_surface(window, cx);
                    return Ok(());
                }
                let providers = Self::filesystems(cx);
                let task = cx.spawn(async move |this, cx| {
                    let loaded = crate::app::open::load_resource(providers, uri).await;
                    let _ = this.update(cx, |this, cx| match loaded {
                        Ok(resource) => {
                            if let Some(document) = Self::resource_document(resource, cx) {
                                this.install_reopened_document(
                                    pane, index, document, view, language, pinned, cx,
                                );
                                if let Some(window) = this.window {
                                    let _ = cx.update_window(window, |_, window, cx| {
                                        this.focus_active_surface(window, cx)
                                    });
                                }
                            }
                        }
                        Err(error) => this.show_notice(
                            PromptLevel::Warning,
                            "Reopen failed",
                            Some(error.to_string()),
                            cx,
                        ),
                    });
                });
                self.tasks.push(task);
                Ok(())
            }
            LayoutTab::Document { uri: None, .. } | LayoutTab::Tool { .. } => {
                Err(CommandOutcome::Unavailable)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn install_reopened_document(
        &mut self,
        pane: PaneId,
        index: usize,
        document: DocumentId,
        view: crate::app::editor::EditorViewState,
        language: Option<String>,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((model, uri)) = Self::documents(cx)
            .read(cx)
            .get(document)
            .map(|live| (live.model().clone(), live.resource_uri().cloned()))
        else {
            return;
        };
        if let Some(language) = language
            && !model.read(cx).language_is_explicit()
        {
            crate::app::languages::set_document_language(&model, uri.as_ref(), Some(language), cx);
            crate::app::application_observation::changed(cx);
        }
        self.install_reopened(
            pane,
            index,
            ArrangedTab::Document {
                document,
                model,
                view,
            },
            pinned,
            cx,
        );
    }

    fn install_reopened(
        &mut self,
        pane: PaneId,
        index: usize,
        tab: ArrangedTab,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        self.workbench.update(cx, |workbench, cx| {
            workbench.insert_arranged(pane, index, tab, pinned, cx);
        });
        cx.notify();
    }
}
