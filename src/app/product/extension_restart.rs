//! Native Restart Extension: stop and reload one package from the window's
//! transient picker.
//!
//! The picker lists every bundled and installed package in the retained startup
//! report, including failed ones. Personal config is not listed; restarting
//! Knot is its recovery path.

use super::*;
use crate::app::extensions::load::LoadResult;
use crate::host::protocol::PickerItem;

pub(super) const RESTART_TITLE: &str = "Restart Extension";

impl ProductShell {
    /// List restartable packages in the captured window and restart the selection.
    pub(crate) fn start_extension_restart(
        &mut self,
        target: &ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(report) = super::super::extensions::host::startup_report(cx) else {
            self.status = "extensions are unavailable".into();
            cx.notify();
            return;
        };
        let items = report
            .report
            .entries
            .iter()
            .filter_map(|entry| {
                let name = entry.name.clone()?;
                Some(PickerItem {
                    id: name.clone(),
                    label: name,
                    description: Some(match &entry.result {
                        LoadResult::Loaded => "loaded".into(),
                        LoadResult::Failed(cause) => format!("failed: {cause}"),
                    }),
                })
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            self.status = "no extensions to restart".into();
            cx.notify();
            return;
        }
        let receiver = match self.begin_owned_picker(None, RESTART_TITLE.into(), items, window, cx)
        {
            Ok(receiver) => receiver,
            Err(_) => {
                self.status = "another interaction is open in this window".into();
                cx.notify();
                return;
            }
        };
        let window = target.window;
        let task = cx.spawn(async move |this, cx| {
            let Ok(TransientOutcome::Picker(name)) = receiver.await else {
                return;
            };
            let Ok(restart) = cx.update(|cx| {
                super::super::extensions::host::entity(cx)
                    .map(|host| host.update(cx, |host, cx| host.restart_package(name.clone(), cx)))
            }) else {
                return;
            };
            let Some(restart) = restart else { return };
            let entry = restart.await;
            let (title, message) = match entry.result {
                LoadResult::Loaded => (format!("Restarted {name}"), None),
                LoadResult::Failed(cause) => (format!("{name} failed to restart"), Some(cause)),
            };
            Self::in_window(&this, window, cx, |shell, window, cx| {
                shell.status = title.clone().into();
                cx.notify();
                // The report needs no answer; dropping the receiver keeps it open.
                drop(window.prompt(
                    PromptLevel::Info,
                    &title,
                    message.as_deref(),
                    &[PromptButton::new("OK")],
                    cx,
                ));
            });
        });
        self.tasks.push(task);
    }
}
