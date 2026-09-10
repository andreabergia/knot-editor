use std::collections::HashMap;

use gpui::{App, Entity, Global, SharedString};

use super::filesystem::ResourceVersion;
use super::model::BufferModel;
use super::resource::ResourceUri;
use super::search_results::SearchResultsController;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct DocumentId(u64);

impl DocumentId {
    #[cfg(test)]
    pub(crate) const fn from_value(value: u64) -> Self {
        Self(value)
    }

    pub(crate) fn value(self) -> u64 {
        self.0
    }
}

/// A document's persistence relationship.
#[allow(
    dead_code,
    reason = "all document states are modeled before every product entry path creates them"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DocumentState {
    /// A user document with no destination yet.
    ///
    /// It is clean at creation and dirty whenever its current content state no
    /// longer matches the state recorded at `clean_revision`.
    Untitled { clean_revision: u64 },
    /// A user document associated with a destination that does not exist yet.
    ///
    /// It remains dirty until the first successful persistence transitions it
    /// to [`DocumentState::Persisted`].
    Destination { uri: ResourceUri },
    /// A user document whose contents have been persisted to a resource.
    ///
    /// Edits are dirty while the model's current content state differs from the
    /// state recorded at `persisted_revision`.
    Persisted {
        uri: ResourceUri,
        persisted_revision: u64,
        version: ResourceVersion,
    },
    /// Application-produced text with no persistence identity.
    ///
    /// Generated documents use the normal buffer and editor paths so their
    /// text remains selectable and navigable. They cannot be saved through
    /// document persistence and are always considered clean. Search results
    /// are currently the only generated document type.
    Generated,
}

impl DocumentState {
    pub(crate) fn resource_uri(&self) -> Option<&ResourceUri> {
        match self {
            Self::Destination { uri } | Self::Persisted { uri, .. } => Some(uri),
            Self::Untitled { .. } | Self::Generated => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn persisted_revision(&self) -> Option<u64> {
        match self {
            Self::Persisted {
                persisted_revision, ..
            } => Some(*persisted_revision),
            _ => None,
        }
    }

    pub(crate) fn persisted_version(&self) -> Option<&ResourceVersion> {
        match self {
            Self::Persisted { version, .. } => Some(version),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersistenceCapture {
    generation: u64,
    state: DocumentState,
}

pub(crate) struct Document {
    id: DocumentId,
    title: SharedString,
    model: Entity<BufferModel>,
    state: DocumentState,
    #[allow(
        dead_code,
        reason = "retained with the generated document as its semantic companion"
    )]
    search_results: Option<SearchResultsController>,
    persistence_generation: u64,
}

impl Document {
    pub(crate) fn id(&self) -> DocumentId {
        self.id
    }

    pub(crate) fn title(&self) -> &SharedString {
        &self.title
    }

    pub(crate) fn model(&self) -> &Entity<BufferModel> {
        &self.model
    }

    pub(crate) fn state(&self) -> &DocumentState {
        &self.state
    }

    pub(crate) fn resource_uri(&self) -> Option<&ResourceUri> {
        self.state.resource_uri()
    }

    pub(crate) fn is_dirty(&self, cx: &App) -> bool {
        match self.state {
            DocumentState::Untitled { clean_revision } => {
                !self.model.read(cx).content_matches_revision(clean_revision)
            }
            DocumentState::Destination { .. } => true,
            DocumentState::Persisted {
                persisted_revision, ..
            } => !self
                .model
                .read(cx)
                .content_matches_revision(persisted_revision),
            DocumentState::Generated => false,
        }
    }

    pub(crate) fn search_results(&self) -> Option<&SearchResultsController> {
        self.search_results.as_ref()
    }
}

/// The application-owned lifetime and persistence registry for documents.
pub(crate) struct DocumentCollection {
    next_id: u64,
    documents: Vec<Document>,
    resources: HashMap<ResourceUri, DocumentId>,
}

impl DocumentCollection {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            documents: Vec::new(),
            resources: HashMap::new(),
        }
    }

    #[allow(dead_code, reason = "used by the product document creation path")]
    pub(crate) fn create_untitled(
        &mut self,
        title: impl Into<SharedString>,
        model: Entity<BufferModel>,
        cx: &App,
    ) -> DocumentId {
        let clean_revision = model.read(cx).revision();
        self.insert(
            title,
            model,
            DocumentState::Untitled { clean_revision },
            None,
        )
    }

    #[allow(dead_code, reason = "used when opening a missing destination")]
    pub(crate) fn create_destination(
        &mut self,
        title: impl Into<SharedString>,
        model: Entity<BufferModel>,
        uri: ResourceUri,
    ) -> DocumentId {
        self.insert_resource(title, model, DocumentState::Destination { uri })
    }

    pub(crate) fn create_persisted(
        &mut self,
        title: impl Into<SharedString>,
        model: Entity<BufferModel>,
        uri: ResourceUri,
        persisted_revision: u64,
        version: ResourceVersion,
    ) -> DocumentId {
        self.insert_resource(
            title,
            model,
            DocumentState::Persisted {
                uri,
                persisted_revision,
                version,
            },
        )
    }

    pub(crate) fn create_generated(
        &mut self,
        title: impl Into<SharedString>,
        model: Entity<BufferModel>,
    ) -> DocumentId {
        self.insert(title, model, DocumentState::Generated, None)
    }

    pub(crate) fn create_search_results(
        &mut self,
        controller: SearchResultsController,
    ) -> DocumentId {
        self.insert(
            controller.title(),
            controller.model().clone(),
            DocumentState::Generated,
            Some(controller),
        )
    }

    pub(crate) fn documents(&self) -> impl Iterator<Item = &Document> {
        self.documents.iter()
    }

    pub(crate) fn get(&self, id: DocumentId) -> Option<&Document> {
        self.documents.iter().find(|document| document.id == id)
    }

    pub(crate) fn document_for_resource(&self, uri: &ResourceUri) -> Option<DocumentId> {
        self.resources.get(uri).copied()
    }

    #[allow(dead_code, reason = "document closure owns this lifecycle transition")]
    pub(crate) fn remove(&mut self, id: DocumentId) -> bool {
        let Some(index) = self.documents.iter().position(|document| document.id == id) else {
            return false;
        };
        let document = self.documents.remove(index);
        if let Some(uri) = document.resource_uri() {
            self.resources.remove(uri);
        }
        true
    }

    pub(crate) fn begin_persistence(
        &mut self,
        id: DocumentId,
        model: &Entity<BufferModel>,
    ) -> Option<PersistenceCapture> {
        let Some(document) = self.documents.iter_mut().find(|document| document.id == id) else {
            return None;
        };
        if &document.model != model || matches!(document.state, DocumentState::Generated) {
            return None;
        }
        document.persistence_generation = document
            .persistence_generation
            .checked_add(1)
            .expect("document persistence generation overflowed");
        Some(PersistenceCapture {
            generation: document.persistence_generation,
            state: document.state.clone(),
        })
    }

    /// Commit exactly the bytes and revision captured before asynchronous I/O.
    ///
    /// The document may have acquired newer edits, which remain dirty. Identity
    /// changes and later persistence attempts invalidate this completion.
    pub(crate) fn finish_persistence(
        &mut self,
        id: DocumentId,
        model: &Entity<BufferModel>,
        capture: &PersistenceCapture,
        title: impl Into<SharedString>,
        uri: ResourceUri,
        version: ResourceVersion,
        revision: u64,
    ) -> bool {
        if self
            .resources
            .get(&uri)
            .is_some_and(|existing| *existing != id)
        {
            return false;
        }
        let Some(document) = self.documents.iter_mut().find(|document| document.id == id) else {
            return false;
        };
        if &document.model != model
            || document.persistence_generation != capture.generation
            || document.state != capture.state
            || matches!(document.state, DocumentState::Generated)
        {
            return false;
        }
        if let Some(old_uri) = document.resource_uri() {
            self.resources.remove(old_uri);
        }
        document.title = title.into();
        document.state = DocumentState::Persisted {
            uri: uri.clone(),
            persisted_revision: revision,
            version,
        };
        self.resources.insert(uri, id);
        true
    }

    fn insert_resource(
        &mut self,
        title: impl Into<SharedString>,
        model: Entity<BufferModel>,
        state: DocumentState,
    ) -> DocumentId {
        let uri = state
            .resource_uri()
            .expect("resource document state has a URI")
            .clone();
        if let Some(existing) = self.document_for_resource(&uri) {
            return existing;
        }
        let id = self.insert(title, model, state, None);
        self.resources.insert(uri, id);
        id
    }

    fn insert(
        &mut self,
        title: impl Into<SharedString>,
        model: Entity<BufferModel>,
        state: DocumentState,
        search_results: Option<SearchResultsController>,
    ) -> DocumentId {
        let id = DocumentId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("document identity space exhausted");
        self.documents.push(Document {
            id,
            title: title.into(),
            model,
            state,
            search_results,
            persistence_generation: 0,
        });
        id
    }
}

#[derive(Clone)]
pub(crate) struct ApplicationDocuments(pub(crate) Entity<DocumentCollection>);

impl Global for ApplicationDocuments {}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::*;

    fn version(value: u8) -> ResourceVersion {
        ResourceVersion::new([value])
    }

    #[gpui::test]
    fn application_global_owns_the_document_collection(cx: &mut TestAppContext) {
        let documents = cx.new(|_| DocumentCollection::new());
        let weak = documents.downgrade();
        cx.set_global(ApplicationDocuments(documents.clone()));

        drop(documents);

        assert_eq!(
            cx.read(|cx| cx.global::<ApplicationDocuments>().0.clone()),
            weak.upgrade().unwrap()
        );
    }

    #[gpui::test]
    fn collection_strongly_owns_models_until_document_removal(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("owned"));
        let weak = model.downgrade();
        let mut documents = DocumentCollection::new();
        let id = cx.update(|cx| documents.create_untitled("Untitled", model, cx));

        assert!(weak.upgrade().is_some());
        assert!(documents.remove(id));
        assert!(weak.upgrade().is_none());
        assert!(!documents.remove(id));
    }

    #[gpui::test]
    fn states_have_explicit_dirty_semantics_and_transitions(cx: &mut TestAppContext) {
        let destination_uri = ResourceUri::parse("mem://workspace/new.txt").unwrap();
        let persisted_uri = ResourceUri::parse("mem://workspace/saved.txt").unwrap();
        let untitled_uri = ResourceUri::parse("mem://workspace/untitled.txt").unwrap();
        let untitled = cx.new(|_| BufferModel::from_text(""));
        let destination = cx.new(|_| BufferModel::from_text(""));
        let persisted = cx.new(|_| BufferModel::from_text("saved"));
        let generated = cx.new(|_| BufferModel::from_read_only_text("generated"));
        let mut documents = DocumentCollection::new();

        let untitled_id =
            cx.update(|cx| documents.create_untitled("Untitled", untitled.clone(), cx));
        let destination_id =
            documents.create_destination("new.txt", destination.clone(), destination_uri.clone());
        let persisted_id = documents.create_persisted(
            "saved.txt",
            persisted.clone(),
            persisted_uri.clone(),
            0,
            version(1),
        );
        let generated_id = documents.create_generated("Generated", generated);

        assert!(matches!(
            documents.get(untitled_id).unwrap().state(),
            DocumentState::Untitled { .. }
        ));
        assert!(cx.read(|cx| documents.get(destination_id).unwrap().is_dirty(cx)));
        assert!(!cx.read(|cx| documents.get(persisted_id).unwrap().is_dirty(cx)));
        assert!(!cx.read(|cx| documents.get(generated_id).unwrap().is_dirty(cx)));

        untitled.update(cx, |model, _| model.replace(0..0, "edit").unwrap());
        persisted.update(cx, |model, _| model.replace(0..0, "edit ").unwrap());
        assert!(cx.read(|cx| documents.get(untitled_id).unwrap().is_dirty(cx)));
        assert!(cx.read(|cx| documents.get(persisted_id).unwrap().is_dirty(cx)));

        untitled.update(cx, |model, _| model.undo().unwrap());
        persisted.update(cx, |model, _| model.undo().unwrap());
        assert!(!cx.read(|cx| documents.get(untitled_id).unwrap().is_dirty(cx)));
        assert!(!cx.read(|cx| documents.get(persisted_id).unwrap().is_dirty(cx)));
        untitled.update(cx, |model, _| model.redo().unwrap());
        persisted.update(cx, |model, _| model.redo().unwrap());
        assert!(cx.read(|cx| documents.get(untitled_id).unwrap().is_dirty(cx)));
        assert!(cx.read(|cx| documents.get(persisted_id).unwrap().is_dirty(cx)));

        let capture = documents
            .begin_persistence(destination_id, &destination)
            .unwrap();
        assert!(documents.finish_persistence(
            destination_id,
            &destination,
            &capture,
            "new.txt",
            destination_uri.clone(),
            version(2),
            0,
        ));
        assert!(matches!(
            documents.get(destination_id).unwrap().state(),
            DocumentState::Persisted { .. }
        ));
        assert!(!cx.read(|cx| documents.get(destination_id).unwrap().is_dirty(cx)));

        let untitled_revision = untitled.read_with(cx, |model, _| model.revision());
        let capture = documents.begin_persistence(untitled_id, &untitled).unwrap();
        assert!(documents.finish_persistence(
            untitled_id,
            &untitled,
            &capture,
            "untitled.txt",
            untitled_uri.clone(),
            version(3),
            untitled_revision,
        ));
        let document = documents.get(untitled_id).unwrap();
        assert_eq!(document.title(), "untitled.txt");
        assert_eq!(document.resource_uri(), Some(&untitled_uri));
        assert!(!cx.read(|cx| document.is_dirty(cx)));
    }

    #[gpui::test]
    fn normalized_resource_identity_is_deduplicated_across_states(cx: &mut TestAppContext) {
        let uri = ResourceUri::parse("MEM://workspace/src/../notes.txt").unwrap();
        let first_model = cx.new(|_| BufferModel::from_text("first"));
        let duplicate_model = cx.new(|_| BufferModel::from_text("duplicate"));
        let duplicate_weak = duplicate_model.downgrade();
        let untitled_model = cx.new(|_| BufferModel::from_text("untitled"));
        let mut documents = DocumentCollection::new();

        let first = documents.create_destination("notes.txt", first_model.clone(), uri.clone());
        let duplicate =
            documents.create_persisted("notes.txt", duplicate_model, uri.clone(), 0, version(1));

        assert_eq!(first, duplicate);
        assert_eq!(documents.documents().count(), 1);
        assert_eq!(documents.document_for_resource(&uri), Some(first));
        assert_eq!(documents.get(first).unwrap().model(), &first_model);
        assert!(duplicate_weak.upgrade().is_none());

        let untitled =
            cx.update(|cx| documents.create_untitled("Untitled", untitled_model.clone(), cx));
        let capture = documents
            .begin_persistence(untitled, &untitled_model)
            .unwrap();
        assert!(!documents.finish_persistence(
            untitled,
            &untitled_model,
            &capture,
            "notes.txt",
            uri.clone(),
            version(2),
            0,
        ));
        assert!(matches!(
            documents.get(untitled).unwrap().state(),
            DocumentState::Untitled { .. }
        ));

        assert!(documents.remove(first));
        assert_eq!(documents.document_for_resource(&uri), None);
        let reopened = documents.create_persisted(
            "notes.txt",
            cx.new(|_| BufferModel::from_text("reopened")),
            uri,
            0,
            version(3),
        );
        assert_ne!(reopened, first);
    }

    #[gpui::test]
    fn stale_persistence_completion_cannot_change_document_state(cx: &mut TestAppContext) {
        let uri = ResourceUri::parse("mem://workspace/notes.txt").unwrap();
        let model = cx.new(|_| BufferModel::from_text("notes"));
        let other_model = cx.new(|_| BufferModel::from_text("other"));
        let mut documents = DocumentCollection::new();
        let id = documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, version(1));

        assert!(documents.begin_persistence(id, &other_model).is_none());
        let stale = documents.begin_persistence(id, &model).unwrap();
        let current = documents.begin_persistence(id, &model).unwrap();
        assert!(!documents.finish_persistence(
            id,
            &model,
            &stale,
            "notes.txt",
            uri.clone(),
            version(2),
            1,
        ));
        assert!(!documents.finish_persistence(
            DocumentId(u64::MAX),
            &model,
            &current,
            "notes.txt",
            uri.clone(),
            version(2),
            1,
        ));
        assert_eq!(
            documents.get(id).unwrap().state().persisted_revision(),
            Some(0)
        );
    }

    #[gpui::test]
    fn save_as_retargets_resource_identity_only_on_a_current_completion(cx: &mut TestAppContext) {
        let old_uri = ResourceUri::parse("mem://workspace/old.txt").unwrap();
        let new_uri = ResourceUri::parse("mem://workspace/new.txt").unwrap();
        let model = cx.new(|_| BufferModel::from_text("notes"));
        let mut documents = DocumentCollection::new();
        let id =
            documents.create_persisted("old.txt", model.clone(), old_uri.clone(), 0, version(1));
        let capture = documents.begin_persistence(id, &model).unwrap();

        assert!(documents.finish_persistence(
            id,
            &model,
            &capture,
            "new.txt",
            new_uri.clone(),
            version(2),
            0,
        ));
        assert_eq!(documents.document_for_resource(&old_uri), None);
        assert_eq!(documents.document_for_resource(&new_uri), Some(id));
        assert_eq!(documents.get(id).unwrap().title(), "new.txt");
    }

    #[gpui::test]
    fn edit_racing_save_keeps_persisted_document_dirty(cx: &mut TestAppContext) {
        let uri = ResourceUri::parse("mem://workspace/notes.txt").unwrap();
        let model = cx.new(|_| BufferModel::from_text("notes"));
        let mut documents = DocumentCollection::new();
        let id = documents.create_persisted("notes.txt", model.clone(), uri.clone(), 0, version(1));

        model.update(cx, |model, _| model.replace(0..0, "first ").unwrap());
        let captured_revision = model.read_with(cx, |model, _| model.revision());
        model.update(cx, |model, _| model.replace(0..0, "second ").unwrap());

        let capture = documents.begin_persistence(id, &model).unwrap();
        assert!(documents.finish_persistence(
            id,
            &model,
            &capture,
            "notes.txt",
            uri.clone(),
            version(2),
            captured_revision,
        ));
        let document = documents.get(id).unwrap();
        assert_eq!(
            document.state().persisted_revision(),
            Some(captured_revision)
        );
        assert!(cx.read(|cx| document.is_dirty(cx)));
        model.update(cx, |model, _| model.undo().unwrap());
        assert!(!cx.read(|cx| document.is_dirty(cx)));
    }
}
