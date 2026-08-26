use gpui::{App, Entity, SharedString};

use super::model::BufferModel;
use super::resource::ResourceUri;
use super::search_results::SearchResultsController;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct OpenBufferId(u64);

impl OpenBufferId {
    #[cfg(test)]
    pub(crate) const fn from_value(value: u64) -> Self {
        Self(value)
    }

    pub(crate) fn value(self) -> u64 {
        self.0
    }
}

pub(crate) struct OpenBufferEntry {
    id: OpenBufferId,
    title: SharedString,
    model: Entity<BufferModel>,
    resource: Option<OpenBufferResource>,
    #[allow(
        dead_code,
        reason = "retained with the generated model as its semantic companion"
    )]
    search_results: Option<SearchResultsController>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OpenBufferResource {
    uri: ResourceUri,
    persisted_revision: u64,
}

impl OpenBufferResource {
    pub(crate) fn uri(&self) -> &ResourceUri {
        &self.uri
    }

    #[cfg(test)]
    pub(crate) fn persisted_revision(&self) -> u64 {
        self.persisted_revision
    }
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

    pub(crate) fn resource(&self) -> Option<&OpenBufferResource> {
        self.resource.as_ref()
    }

    pub(crate) fn is_dirty(&self, cx: &App) -> bool {
        self.resource
            .as_ref()
            .is_some_and(|resource| self.model.read(cx).revision() != resource.persisted_revision)
    }

    pub(crate) fn search_results(&self) -> Option<&SearchResultsController> {
        self.search_results.as_ref()
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
            resource: None,
            search_results: None,
        });
        self.selected.get_or_insert(id);
        id
    }

    pub(crate) fn add_resource(
        &mut self,
        title: impl Into<SharedString>,
        model: Entity<BufferModel>,
        uri: ResourceUri,
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
            resource: Some(OpenBufferResource {
                uri,
                persisted_revision: 0,
            }),
            search_results: None,
        });
        self.selected.get_or_insert(id);
        id
    }

    pub(crate) fn add_search_results(
        &mut self,
        controller: SearchResultsController,
    ) -> OpenBufferId {
        let id = OpenBufferId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("open buffer identity space exhausted");
        self.entries.push(OpenBufferEntry {
            id,
            title: controller.title().into(),
            model: controller.model().clone(),
            resource: None,
            search_results: Some(controller),
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

    /// Record exactly the revision whose bytes were successfully persisted.
    ///
    /// The model may have advanced while the write was pending. In that case
    /// the captured revision still becomes the persisted revision and the
    /// entry remains dirty.
    pub(crate) fn mark_persisted(
        &mut self,
        id: OpenBufferId,
        model: &Entity<BufferModel>,
        uri: &ResourceUri,
        revision: u64,
    ) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        if &entry.model != model {
            return false;
        }
        let Some(resource) = entry.resource.as_mut() else {
            return false;
        };
        if &resource.uri != uri {
            return false;
        }
        resource.persisted_revision = revision;
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

    #[gpui::test]
    fn generated_and_search_buffers_remain_resource_less(cx: &mut TestAppContext) {
        let source = cx.new(|_| BufferModel::from_text("source"));
        let generated = cx.new(|_| BufferModel::from_text("generated"));
        let controller = cx.update(|cx| {
            SearchResultsController::search(
                "source",
                OpenBufferId::from_value(99),
                "Source",
                source,
                cx,
            )
        });
        let mut buffers = OpenBufferCollection::new();

        let generated_id = buffers.add("Generated", generated);
        let search_id = buffers.add_search_results(controller);

        for id in [generated_id, search_id] {
            let entry = buffers.entries().find(|entry| entry.id() == id).unwrap();
            assert!(entry.resource().is_none());
            assert!(!cx.read(|cx| entry.is_dirty(cx)));
        }
    }

    #[gpui::test]
    fn resource_dirty_state_tracks_the_persisted_revision(cx: &mut TestAppContext) {
        let uri = ResourceUri::parse("mem://workspace/notes.txt").unwrap();
        let model = cx.new(|_| BufferModel::from_text("notes"));
        let mut buffers = OpenBufferCollection::new();
        let id = buffers.add_resource("notes.txt", model.clone(), uri.clone());

        let entry = buffers.entries().find(|entry| entry.id() == id).unwrap();
        assert_eq!(entry.resource().unwrap().uri(), &uri);
        assert_eq!(entry.resource().unwrap().persisted_revision(), 0);
        assert!(!cx.read(|cx| entry.is_dirty(cx)));

        model.update(cx, |model, _| {
            model.replace(0..0, "updated ").unwrap();
        });
        assert!(cx.read(|cx| {
            buffers
                .entries()
                .find(|entry| entry.id() == id)
                .unwrap()
                .is_dirty(cx)
        }));

        let revision = model.read_with(cx, |model, _| model.revision());
        assert!(buffers.mark_persisted(id, &model, &uri, revision));
        let entry = buffers.entries().find(|entry| entry.id() == id).unwrap();
        assert_eq!(entry.resource().unwrap().persisted_revision(), revision);
        assert!(!cx.read(|cx| entry.is_dirty(cx)));
    }

    #[gpui::test]
    fn edit_racing_a_save_keeps_the_resource_dirty(cx: &mut TestAppContext) {
        let uri = ResourceUri::parse("mem://workspace/notes.txt").unwrap();
        let model = cx.new(|_| BufferModel::from_text("notes"));
        let mut buffers = OpenBufferCollection::new();
        let id = buffers.add_resource("notes.txt", model.clone(), uri.clone());

        model.update(cx, |model, _| {
            model.replace(0..0, "first ").unwrap();
        });
        let captured_revision = model.read_with(cx, |model, _| model.revision());
        model.update(cx, |model, _| {
            model.replace(0..0, "second ").unwrap();
        });

        assert!(buffers.mark_persisted(id, &model, &uri, captured_revision));
        let entry = buffers.entries().find(|entry| entry.id() == id).unwrap();
        assert_eq!(
            entry.resource().unwrap().persisted_revision(),
            captured_revision
        );
        assert!(cx.read(|cx| entry.is_dirty(cx)));
    }

    #[gpui::test]
    fn persisted_revision_rejects_stale_resource_identity(cx: &mut TestAppContext) {
        let uri = ResourceUri::parse("mem://workspace/notes.txt").unwrap();
        let other_uri = ResourceUri::parse("mem://workspace/other.txt").unwrap();
        let model = cx.new(|_| BufferModel::from_text("notes"));
        let other_model = cx.new(|_| BufferModel::from_text("other"));
        let mut buffers = OpenBufferCollection::new();
        let id = buffers.add_resource("notes.txt", model.clone(), uri.clone());

        assert!(!buffers.mark_persisted(id, &other_model, &uri, 1));
        assert!(!buffers.mark_persisted(id, &model, &other_uri, 1));
        assert!(!buffers.mark_persisted(OpenBufferId(u64::MAX), &model, &uri, 1));
        assert_eq!(
            buffers
                .entries()
                .find(|entry| entry.id() == id)
                .unwrap()
                .resource()
                .unwrap()
                .persisted_revision(),
            0
        );
    }
}
