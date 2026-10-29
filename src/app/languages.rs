//! Language registrations, document language detection, and the syntax state
//! each buffer's language attaches.
//!
//! Extensions and personal config register languages: a language ID, the
//! compiled-in grammar it parses with, the file names, extensions, and shebang
//! interpreters it claims, and its queries. Registrations follow keymap owner
//! precedence: personal config outranks installed packages, which outrank
//! bundled ones, and a later registration outranks an earlier one of the same
//! owner kind. The highest registration of an ID defines that language, and
//! the highest registration claiming a pattern decides detection, so removing
//! a registration reveals the next.
//!
//! A buffer records a language ID, explicit or detected, independently of
//! registration: an unregistered ID paints and reports as `plaintext` until
//! something registers it. Every registration change re-detects open documents whose
//! language is not explicit and reattaches syntax wherever the resolved
//! language changed.

use std::{collections::HashMap, sync::Arc};

use gpui::{App, Entity, EntityId, Global, WeakEntity};

use crate::host::protocol::{ExtensionId, ExtensionLifecycleId, LanguageInfo};

use super::{
    documents::ApplicationDocuments,
    model::BufferModel,
    resource::ResourceUri,
    syntax::{LanguageResolver, SyntaxLanguage, SyntaxLanguageError, SyntaxState},
};

/// The language a buffer reports when its ID is missing or unregistered,
/// as LSP names it.
pub(crate) const PLAIN_TEXT: &str = "plaintext";

/// How far detection reads into a first line for a shebang.
const SHEBANG_LIMIT: usize = 256;

/// Lower precedence first.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum LanguagePriority {
    Bundled,
    Installed,
    Personal,
}

/// The extension lifecycle that owns a registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LanguageOwner {
    pub(crate) priority: LanguagePriority,
    pub(crate) extension: ExtensionId,
    pub(crate) lifecycle: ExtensionLifecycleId,
}

/// A language as an extension registers it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LanguageDefinition {
    pub(crate) info: LanguageInfo,
    pub(crate) highlights: String,
    pub(crate) injections: String,
}

struct Registration {
    id: u64,
    owner: LanguageOwner,
    info: LanguageInfo,
    language: Arc<SyntaxLanguage>,
}

impl Registration {
    /// Precedence among registrations; later registrations come later in the
    /// list, so the index breaks ties within a priority.
    fn rank(&self, index: usize) -> (LanguagePriority, usize) {
        (self.owner.priority, index)
    }
}

/// The languages injections and buffers resolve, rebuilt on every change.
#[derive(Default)]
struct LanguageTable {
    languages: HashMap<String, Arc<SyntaxLanguage>>,
    /// Lowercase file extension to language ID.
    extensions: HashMap<String, String>,
}

impl LanguageTable {
    /// The language an injection names: a language ID, compared exactly and
    /// then in lowercase, or else a file extension, as Markdown fences often
    /// name `js` or `py`.
    fn resolve_injection(&self, name: &str) -> Option<Arc<SyntaxLanguage>> {
        let lowercase = name.to_lowercase();
        self.languages
            .get(name)
            .or_else(|| self.languages.get(&lowercase))
            .or_else(|| {
                self.extensions
                    .get(&lowercase)
                    .and_then(|id| self.languages.get(id))
            })
            .cloned()
    }
}

#[derive(Default)]
pub(crate) struct LanguageRegistry {
    /// In registration order.
    registrations: Vec<Registration>,
    next_id: u64,
    table: Arc<LanguageTable>,
}

impl LanguageRegistry {
    /// Compile and add a registration, returning its ID.
    pub(crate) fn register(
        &mut self,
        owner: LanguageOwner,
        definition: LanguageDefinition,
    ) -> Result<u64, SyntaxLanguageError> {
        let language = SyntaxLanguage::new(
            &definition.info.grammar,
            &definition.highlights,
            &definition.injections,
        )?;
        self.next_id += 1;
        self.registrations.push(Registration {
            id: self.next_id,
            owner,
            info: definition.info,
            language: Arc::new(language),
        });
        self.rebuild();
        Ok(self.next_id)
    }

    /// Remove registration `id` of one extension lifecycle. Returns false when
    /// it owns none.
    pub(crate) fn unregister(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        id: u64,
    ) -> bool {
        self.remove(|registration| {
            registration.id == id
                && registration.owner.extension == extension
                && registration.owner.lifecycle == lifecycle
        })
    }

    /// Remove every registration of one extension lifecycle.
    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        self.remove(|registration| {
            registration.owner.extension == extension && registration.owner.lifecycle == lifecycle
        })
    }

    fn remove(&mut self, matches: impl Fn(&Registration) -> bool) -> bool {
        let before = self.registrations.len();
        self.registrations
            .retain(|registration| !matches(registration));
        let changed = self.registrations.len() != before;
        if changed {
            self.rebuild();
        }
        changed
    }

    /// The registration that defines each language ID, sorted by ID.
    fn effective(&self) -> Vec<&Registration> {
        let mut effective: HashMap<&str, (usize, &Registration)> = HashMap::new();
        for (index, registration) in self.registrations.iter().enumerate() {
            let entry = effective
                .entry(&registration.info.id)
                .or_insert((index, registration));
            if registration.rank(index) > entry.1.rank(entry.0) {
                *entry = (index, registration);
            }
        }
        let mut effective: Vec<_> = effective
            .into_values()
            .map(|(_, registration)| registration)
            .collect();
        effective.sort_by(|a, b| a.info.id.cmp(&b.info.id));
        effective
    }

    fn rebuild(&mut self) {
        let languages = self
            .effective()
            .into_iter()
            .map(|registration| {
                (
                    registration.info.id.clone(),
                    Arc::clone(&registration.language),
                )
            })
            .collect();
        let mut extensions: HashMap<String, (usize, String)> = HashMap::new();
        for (index, registration) in self.registrations.iter().enumerate() {
            for extension in &registration.info.file_extensions {
                let rank = registration.rank(index);
                let entry = extensions
                    .entry(extension.to_lowercase())
                    .or_insert_with(|| (index, registration.info.id.clone()));
                if rank > self.registrations[entry.0].rank(entry.0) {
                    *entry = (index, registration.info.id.clone());
                }
            }
        }
        self.table = Arc::new(LanguageTable {
            languages,
            extensions: extensions
                .into_iter()
                .map(|(extension, (_, id))| (extension, id))
                .collect(),
        });
    }

    /// The language each registered ID resolves to, sorted by ID.
    pub(crate) fn list(&self) -> Vec<LanguageInfo> {
        self.effective()
            .into_iter()
            .map(|registration| registration.info.clone())
            .collect()
    }

    pub(crate) fn resolve(&self, id: &str) -> Option<Arc<SyntaxLanguage>> {
        self.table.languages.get(id).cloned()
    }

    /// Resolves injected language names against the registrations as they
    /// are now.
    pub(crate) fn resolver(&self) -> LanguageResolver {
        let table = Arc::clone(&self.table);
        Arc::new(move |name| table.resolve_injection(name))
    }

    /// The language a file called `file_name` whose text starts with
    /// `first_line` should use: a registered file name, then the longest
    /// registered extension, then the shebang interpreter. The highest
    /// registration claiming a pattern wins it.
    pub(crate) fn detect(&self, file_name: Option<&str>, first_line: &str) -> Option<String> {
        let best = |claim: &dyn Fn(&LanguageInfo) -> Option<usize>| {
            self.registrations
                .iter()
                .enumerate()
                .filter_map(|(index, registration)| {
                    claim(&registration.info).map(|specificity| {
                        let (priority, order) = registration.rank(index);
                        ((priority, specificity, order), &registration.info.id)
                    })
                })
                .max_by_key(|(rank, _)| *rank)
                .map(|(_, id)| id.clone())
        };
        if let Some(name) = file_name {
            let lowercase = name.to_lowercase();
            let detected = best(&|info| {
                info.file_names
                    .iter()
                    .any(|known| known == name)
                    .then_some(0)
            })
            .or_else(|| {
                best(&|info| {
                    info.file_extensions
                        .iter()
                        .filter(|extension| {
                            let extension = extension.to_lowercase();
                            lowercase.len() > extension.len()
                                && lowercase.ends_with(&extension)
                                && lowercase[..lowercase.len() - extension.len()].ends_with('.')
                        })
                        .map(String::len)
                        .max()
                })
            });
            if detected.is_some() {
                return detected;
            }
        }
        let interpreter = interpreter(first_line)?;
        // `python3.12` also matches a registered `python`.
        let unversioned = interpreter.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        best(&|info| {
            info.interpreters
                .iter()
                .any(|known| known == interpreter)
                .then_some(1)
                .or_else(|| {
                    info.interpreters
                        .iter()
                        .any(|known| known == unversioned)
                        .then_some(0)
                })
        })
    }
}

/// The program a `#!` line runs, looking through `env` and its options.
fn interpreter(first_line: &str) -> Option<&str> {
    let mut words = first_line.strip_prefix("#!")?.split_whitespace();
    let program = words.next()?.rsplit('/').next()?;
    if program != "env" {
        return Some(program);
    }
    words.find(|word| !word.starts_with('-') && !word.contains('='))
}

/// The application's language registrations, and the buffers whose syntax
/// follows them.
#[derive(Default)]
pub(crate) struct ApplicationLanguages {
    registry: LanguageRegistry,
    /// Every live buffer whose language has been resolved, so a registration
    /// change can reattach its syntax.
    models: HashMap<EntityId, WeakEntity<BufferModel>>,
}

impl Global for ApplicationLanguages {}

pub(crate) fn register(
    owner: LanguageOwner,
    definition: LanguageDefinition,
    cx: &mut App,
) -> Result<u64, SyntaxLanguageError> {
    let id = cx
        .default_global::<ApplicationLanguages>()
        .registry
        .register(owner, definition)?;
    refresh(cx);
    Ok(id)
}

pub(crate) fn unregister(
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
    id: u64,
    cx: &mut App,
) -> bool {
    let removed = cx
        .default_global::<ApplicationLanguages>()
        .registry
        .unregister(extension, lifecycle, id);
    if removed {
        refresh(cx);
    }
    removed
}

pub(crate) fn remove_lifecycle(
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
    cx: &mut App,
) {
    if cx.has_global::<ApplicationLanguages>()
        && cx
            .global_mut::<ApplicationLanguages>()
            .registry
            .remove_lifecycle(extension, lifecycle)
    {
        refresh(cx);
    }
}

/// The language a buffer reports: its ID while registered, else plain text.
pub(crate) fn effective_language(model: &BufferModel, cx: &App) -> String {
    model
        .language()
        .filter(|id| {
            cx.try_global::<ApplicationLanguages>()
                .is_some_and(|languages| languages.registry.resolve(id).is_some())
        })
        .unwrap_or(PLAIN_TEXT)
        .to_owned()
}

pub(crate) fn list(cx: &App) -> Vec<LanguageInfo> {
    cx.try_global::<ApplicationLanguages>()
        .map(|languages| languages.registry.list())
        .unwrap_or_default()
}

/// Detect a document's language from its resource name and first line,
/// unless its language is explicit, and attach the matching syntax.
pub(crate) fn detect_document(
    model: &Entity<BufferModel>,
    uri: Option<&ResourceUri>,
    cx: &mut App,
) {
    redetect(model, uri, cx);
    attach(model, false, cx);
}

/// Make `language` a document's explicit language, or return the document to
/// detection with `None`.
pub(crate) fn set_document_language(
    model: &Entity<BufferModel>,
    uri: Option<&ResourceUri>,
    language: Option<String>,
    cx: &mut App,
) {
    match language {
        Some(language) => model.update(cx, |model, _| model.set_language(Some(language), true)),
        None => model.update(cx, |model, _| model.set_language(None, false)),
    }
    detect_document(model, uri, cx);
}

/// Give a buffer that is not a document, such as a text frame's, a fixed
/// language.
pub(crate) fn set_language(model: &Entity<BufferModel>, language: String, cx: &mut App) {
    model.update(cx, |model, _| model.set_language(Some(language), true));
    attach(model, false, cx);
}

fn redetect(model: &Entity<BufferModel>, uri: Option<&ResourceUri>, cx: &mut App) {
    if model.read(cx).language_is_explicit() {
        return;
    }
    let file_name = uri.and_then(|uri| uri.as_url().path_segments()?.next_back());
    let first_line = shebang_line(model.read(cx));
    let detected = cx
        .default_global::<ApplicationLanguages>()
        .registry
        .detect(file_name, &first_line);
    model.update(cx, |model, _| model.set_language(detected, false));
}

/// The first line when it is a shebang, read no further than the limit.
fn shebang_line(model: &BufferModel) -> String {
    if model.len() < 2 || !model.is_char_boundary(2) || model.read(0..2) != "#!" {
        return String::new();
    }
    let mut end = model.line_range(0).end.min(SHEBANG_LIMIT);
    while !model.is_char_boundary(end) {
        end -= 1;
    }
    model.read(0..end)
}

/// Give `model` a fresh syntax state when the language its ID resolves to
/// differs from the one it parses. After a registration change, a language
/// that injects reparses too, since its injections may resolve differently.
fn attach(model: &Entity<BufferModel>, registration_changed: bool, cx: &mut App) {
    let languages = cx.default_global::<ApplicationLanguages>();
    if !languages.models.contains_key(&model.entity_id()) {
        languages
            .models
            .retain(|_, model| model.upgrade().is_some());
        languages
            .models
            .insert(model.entity_id(), model.downgrade());
    }
    let resolved = model
        .read(cx)
        .language()
        .and_then(|id| cx.global::<ApplicationLanguages>().registry.resolve(id));
    let current = model.read(cx).syntax_language();
    let unchanged = match (&resolved, current) {
        (Some(resolved), Some(current)) => {
            Arc::ptr_eq(resolved, current) && !(registration_changed && resolved.can_inject())
        }
        (None, None) => true,
        _ => false,
    };
    if unchanged {
        return;
    }
    let resolver = cx.global::<ApplicationLanguages>().registry.resolver();
    model.update(cx, |model, cx| {
        model.set_syntax(
            resolved.map(|language| SyntaxState::new(language, resolver)),
            cx,
        )
    });
}

/// Follow a registration change: re-detect every open document whose
/// language is not explicit, then reattach every tracked buffer's syntax.
///
/// This reads `ApplicationDocuments`, so a registration change must never
/// happen while the document collection is being updated; the collection
/// itself only calls `detect_document`, which does not refresh.
fn refresh(cx: &mut App) {
    let documents = cx
        .try_global::<ApplicationDocuments>()
        .map(|documents| {
            documents
                .0
                .read(cx)
                .documents()
                .map(|document| (document.model().clone(), document.resource_uri().cloned()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (model, uri) in &documents {
        redetect(model, uri.as_ref(), cx);
    }
    let models: Vec<_> = cx
        .default_global::<ApplicationLanguages>()
        .models
        .values()
        .filter_map(WeakEntity::upgrade)
        .collect();
    for model in models {
        if model.read(cx).is_open() {
            attach(&model, true, cx);
        }
    }
    super::application_observation::changed(cx);
}

#[cfg(test)]
mod tests;
