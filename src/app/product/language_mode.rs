//! Native Set Language Mode: choose the captured document's language from the
//! window's transient picker.
//!
//! The picker lists Auto Detect, which returns the document to detection,
//! followed by every registered language.

use super::*;
use crate::host::protocol::{CommandOutcome, PickerItem};

pub(super) const LANGUAGE_MODE_TITLE: &str = "Set Language Mode";

impl ProductShell {
    /// Offer the registered languages for the captured document and make the
    /// selection its explicit language.
    pub(crate) fn start_language_mode(
        &mut self,
        target: &ProductCommandTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        let TabSurfaceId::Document(document) = target.surface else {
            return Err(CommandOutcome::Unavailable);
        };
        let model = Self::documents(cx)
            .read(cx)
            .get(document)
            .ok_or(CommandOutcome::InvalidTarget)?
            .model()
            .clone();
        let current = model
            .read(cx)
            .language_is_explicit()
            .then(|| crate::app::languages::effective_language(model.read(cx), cx));
        let mut choices = vec![None];
        let mut items = vec![PickerItem {
            id: "0".into(),
            label: "Auto Detect".into(),
            description: Some(
                if current.is_none() {
                    "detect from the file name and first line (current)"
                } else {
                    "detect from the file name and first line"
                }
                .into(),
            ),
        }];
        for language in crate::app::languages::list(cx) {
            let description = if current.as_deref() == Some(&language.id) {
                format!("{} (current)", language.id)
            } else {
                language.id.clone()
            };
            items.push(PickerItem {
                id: choices.len().to_string(),
                label: language.name,
                description: Some(description),
            });
            choices.push(Some(language.id));
        }
        let Ok(receiver) =
            self.begin_owned_picker(None, LANGUAGE_MODE_TITLE.into(), items, window, cx)
        else {
            cx.notify();
            return Ok(());
        };
        let task = cx.spawn(async move |this, cx| {
            let Ok(TransientOutcome::Picker(id)) = receiver.await else {
                return;
            };
            let Some(choice) = id
                .parse::<usize>()
                .ok()
                .and_then(|index| choices.get(index).cloned())
            else {
                return;
            };
            let _ = this.update(cx, |_, cx| {
                let Some(uri) = Self::documents(cx)
                    .read(cx)
                    .get(document)
                    .map(|document| document.resource_uri().cloned())
                else {
                    return;
                };
                let model = model.clone();
                crate::app::languages::set_document_language(&model, uri.as_ref(), choice, cx);
                crate::app::application_observation::changed(cx);
                cx.notify();
            });
        });
        self.tasks.push(task);
        Ok(())
    }
}
