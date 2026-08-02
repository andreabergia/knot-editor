use gpui::{Entity, SharedString};

use super::model::BufferModel;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct OpenBufferId(u64);

impl OpenBufferId {
    pub(crate) fn value(self) -> u64 {
        self.0
    }
}

pub(crate) struct OpenBufferEntry {
    id: OpenBufferId,
    title: SharedString,
    model: Entity<BufferModel>,
}

impl OpenBufferEntry {
    pub(crate) fn id(&self) -> OpenBufferId {
        self.id
    }

    pub(crate) fn title(&self) -> &SharedString {
        &self.title
    }

    pub(crate) fn model(&self) -> &Entity<BufferModel> {
        &self.model
    }
}

/// Strongly owns the buffer models visible in the application shell.
pub(crate) struct OpenBufferCollection {
    next_id: u64,
    entries: Vec<OpenBufferEntry>,
    selected: Option<OpenBufferId>,
}

impl OpenBufferCollection {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            entries: Vec::new(),
            selected: None,
        }
    }

    pub(crate) fn add(
        &mut self,
        title: impl Into<SharedString>,
        model: Entity<BufferModel>,
    ) -> OpenBufferId {
        let id = OpenBufferId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("open buffer identity space exhausted");
        self.entries.push(OpenBufferEntry {
            id,
            title: title.into(),
            model,
        });
        self.selected.get_or_insert(id);
        id
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &OpenBufferEntry> {
        self.entries.iter()
    }

    pub(crate) fn selected_id(&self) -> Option<OpenBufferId> {
        self.selected
    }

    pub(crate) fn selected(&self) -> Option<&OpenBufferEntry> {
        let selected = self.selected?;
        self.entries.iter().find(|entry| entry.id == selected)
    }

    pub(crate) fn select(&mut self, id: OpenBufferId) -> bool {
        if !self.entries.iter().any(|entry| entry.id == id) {
            return false;
        }
        self.selected = Some(id);
        true
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::*;

    #[gpui::test]
    fn entries_have_monotonic_identities_and_strong_model_ownership(cx: &mut TestAppContext) {
        let first_model = cx.new(|_| BufferModel::from_text("first"));
        let first_weak = first_model.downgrade();
        let second_model = cx.new(|_| BufferModel::from_text("second"));
        let mut buffers = OpenBufferCollection::new();

        let first = buffers.add("First", first_model);
        let second = buffers.add("Second", second_model.clone());

        assert_ne!(first, second);
        assert_eq!(buffers.selected_id(), Some(first));
        assert_eq!(buffers.selected().unwrap().title(), "First");
        assert!(first_weak.upgrade().is_some());

        assert!(buffers.select(second));
        assert_eq!(buffers.selected().unwrap().model(), &second_model);
        assert!(!buffers.select(OpenBufferId(u64::MAX)));
        assert_eq!(buffers.selected_id(), Some(second));
    }
}
