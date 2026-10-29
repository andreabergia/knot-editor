use gpui::{AppContext, TestAppContext};

use super::*;
use crate::app::{
    documents::{DocumentCollection, DocumentState},
    filesystem::ResourceVersion,
    syntax::SyntaxQuery,
    theme::Highlight,
};

const MARKDOWN_INJECTIONS: &str = r#"
(fenced_code_block
  (info_string (language) @injection.language)
  (code_fence_content) @injection.content)
"#;

fn owner(priority: LanguagePriority, extension: u64) -> LanguageOwner {
    LanguageOwner {
        priority,
        extension: ExtensionId::new(extension),
        lifecycle: ExtensionLifecycleId::new(1),
    }
}

const BUNDLED: LanguagePriority = LanguagePriority::Bundled;
const INSTALLED: LanguagePriority = LanguagePriority::Installed;
const PERSONAL: LanguagePriority = LanguagePriority::Personal;

fn definition(id: &str, name: &str) -> LanguageDefinition {
    LanguageDefinition {
        info: LanguageInfo {
            id: id.into(),
            name: name.into(),
            grammar: "rust".into(),
            file_extensions: Vec::new(),
            file_names: Vec::new(),
            interpreters: Vec::new(),
        },
        highlights: "(identifier) @variable".into(),
        injections: String::new(),
    }
}

fn claiming(
    id: &str,
    extensions: &[&str],
    names: &[&str],
    interpreters: &[&str],
) -> LanguageDefinition {
    let mut definition = definition(id, id);
    let owned = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
    definition.info.file_extensions = owned(extensions);
    definition.info.file_names = owned(names);
    definition.info.interpreters = owned(interpreters);
    definition
}

fn names(registry: &LanguageRegistry) -> Vec<(String, String)> {
    registry
        .list()
        .into_iter()
        .map(|info| (info.id, info.name))
        .collect()
}

#[test]
fn higher_owners_define_a_language_and_removal_reveals_the_next() {
    let mut registry = LanguageRegistry::default();
    registry
        .register(owner(PERSONAL, 3), definition("rust", "Personal"))
        .unwrap();
    registry
        .register(owner(BUNDLED, 1), definition("rust", "Bundled"))
        .unwrap();
    let installed = registry
        .register(owner(INSTALLED, 2), definition("rust", "Installed"))
        .unwrap();
    registry
        .register(owner(BUNDLED, 1), definition("go", "Go"))
        .unwrap();
    assert_eq!(
        names(&registry),
        [
            ("go".into(), "Go".into()),
            ("rust".into(), "Personal".into())
        ]
    );
    let personal = registry.resolve("rust").unwrap();
    assert!(Arc::ptr_eq(&personal, &registry.resolve("rust").unwrap()));

    assert!(registry.remove_lifecycle(ExtensionId::new(3), ExtensionLifecycleId::new(1)));
    assert_eq!(names(&registry)[1].1, "Installed");
    assert!(!Arc::ptr_eq(&personal, &registry.resolve("rust").unwrap()));
    // Only the owner can remove a registration.
    assert!(!registry.unregister(ExtensionId::new(1), ExtensionLifecycleId::new(1), installed));
    assert!(registry.unregister(ExtensionId::new(2), ExtensionLifecycleId::new(1), installed));
    assert_eq!(names(&registry)[1].1, "Bundled");
    assert!(!registry.unregister(ExtensionId::new(2), ExtensionLifecycleId::new(1), installed));

    // A later registration wins within one owner kind.
    registry
        .register(owner(BUNDLED, 4), definition("rust", "Later"))
        .unwrap();
    assert_eq!(names(&registry)[1].1, "Later");
}

#[test]
fn registration_errors_name_the_grammar_or_query() {
    let mut registry = LanguageRegistry::default();
    let mut unknown = definition("cobol", "COBOL");
    unknown.info.grammar = "cobol".into();
    assert_eq!(
        registry.register(owner(BUNDLED, 1), unknown),
        Err(SyntaxLanguageError::UnknownGrammar)
    );
    let mut invalid = definition("rust", "Rust");
    invalid.injections = "(not_a_node) @injection.content".into();
    assert!(matches!(
        registry.register(owner(BUNDLED, 1), invalid),
        Err(SyntaxLanguageError::InvalidQuery {
            query: SyntaxQuery::Injections,
            row: 0,
            ..
        })
    ));
    assert!(registry.list().is_empty());
}

#[test]
fn detection_prefers_names_then_longest_extensions_then_interpreters() {
    let mut registry = LanguageRegistry::default();
    for definition in [
        claiming("typescript", &["ts"], &[], &[]),
        claiming("typescript-declarations", &["d.ts"], &[], &[]),
        claiming("makefile", &["mk"], &["Makefile"], &[]),
        claiming("python", &["py"], &[], &["python"]),
        claiming("shellscript", &["sh", "bashrc"], &[], &["bash", "sh"]),
    ] {
        registry.register(owner(BUNDLED, 1), definition).unwrap();
    }
    let detect = |name: Option<&str>, first_line: &str| registry.detect(name, first_line);
    assert_eq!(detect(Some("main.TS"), ""), Some("typescript".into()));
    assert_eq!(
        detect(Some("index.d.ts"), ""),
        Some("typescript-declarations".into())
    );
    assert_eq!(detect(Some("Makefile"), ""), Some("makefile".into()));
    assert_eq!(detect(Some("makefile"), ""), None);
    assert_eq!(detect(Some(".bashrc"), ""), Some("shellscript".into()));
    assert_eq!(detect(Some("ts"), ""), None);
    // A matching file name wins over the shebang.
    assert_eq!(
        detect(Some("tool.py"), "#!/bin/bash"),
        Some("python".into())
    );
    assert_eq!(
        detect(Some("tool"), "#!/usr/bin/env -S python3.12 -u"),
        Some("python".into())
    );
    assert_eq!(detect(None, "#!/bin/sh -e"), Some("shellscript".into()));
    assert_eq!(
        detect(None, "#!/usr/bin/env FOO=1 bash"),
        Some("shellscript".into())
    );
    assert_eq!(detect(None, "# not a shebang"), None);
    assert_eq!(detect(Some("notes.txt"), ""), None);
}

#[test]
fn the_highest_registration_claiming_a_pattern_decides_detection() {
    let mut registry = LanguageRegistry::default();
    registry
        .register(owner(INSTALLED, 2), claiming("c", &["h"], &[], &[]))
        .unwrap();
    registry
        .register(owner(BUNDLED, 1), claiming("cpp", &["h", "hpp"], &[], &[]))
        .unwrap();
    assert_eq!(registry.detect(Some("a.h"), ""), Some("c".into()));
    assert_eq!(registry.detect(Some("a.hpp"), ""), Some("cpp".into()));
    registry.remove_lifecycle(ExtensionId::new(2), ExtensionLifecycleId::new(1));
    assert_eq!(registry.detect(Some("a.h"), ""), Some("cpp".into()));
}

#[test]
fn injections_resolve_by_id_case_insensitively_or_by_extension() {
    let mut registry = LanguageRegistry::default();
    registry
        .register(owner(BUNDLED, 1), claiming("rust", &["rs"], &[], &[]))
        .unwrap();
    let resolver = registry.resolver();
    let rust = registry.resolve("rust").unwrap();
    for name in ["rust", "Rust", "rs", "RS"] {
        assert!(Arc::ptr_eq(&resolver(name).unwrap(), &rust), "{name}");
    }
    assert!(resolver("python").is_none());
}

fn install(cx: &mut TestAppContext) -> Entity<DocumentCollection> {
    let documents = cx.new(|_| DocumentCollection::new());
    cx.set_global(ApplicationDocuments(documents.clone()));
    documents
}

fn register(cx: &mut TestAppContext, owner: LanguageOwner, definition: LanguageDefinition) -> u64 {
    cx.update(|cx| super::register(owner, definition, cx).unwrap())
}

fn persisted(
    documents: &Entity<DocumentCollection>,
    uri: &str,
    text: &str,
    cx: &mut TestAppContext,
) -> Entity<BufferModel> {
    let model = cx.new(|_| BufferModel::from_text(text));
    let uri = ResourceUri::parse(uri).unwrap();
    documents.update(cx, |documents, cx| {
        documents.create_persisted("file", model.clone(), uri, 0, ResourceVersion::new([1]), cx)
    });
    model
}

fn state(model: &Entity<BufferModel>, cx: &TestAppContext) -> (Option<String>, bool, bool) {
    model.read_with(cx, |model, _| {
        (
            model.language().map(str::to_owned),
            model.language_is_explicit(),
            model.syntax_generation().is_some(),
        )
    })
}

#[gpui::test]
fn documents_detect_on_open_and_follow_registration_changes(cx: &mut TestAppContext) {
    let documents = install(cx);
    let before = persisted(&documents, "mem://workspace/main.rs", "fn main() {}", cx);
    assert_eq!(state(&before, cx), (None, false, false));

    let rust = register(cx, owner(BUNDLED, 1), claiming("rust", &["rs"], &[], &[]));
    assert_eq!(state(&before, cx), (Some("rust".into()), false, true));
    let after = persisted(&documents, "mem://workspace/lib.rs", "", cx);
    assert_eq!(state(&after, cx), (Some("rust".into()), false, true));
    let script = persisted(
        &documents,
        "mem://workspace/run",
        "#!/usr/bin/env rust-script\n",
        cx,
    );
    assert_eq!(state(&script, cx).0, None);

    // An explicit language holds through registration changes, painting
    // plain text while nothing registers it.
    cx.update(|cx| {
        super::set_document_language(&before, None, Some("other".into()), cx);
    });
    assert_eq!(state(&before, cx), (Some("other".into()), true, false));
    let other = register(
        cx,
        owner(INSTALLED, 2),
        claiming("other", &["rs"], &[], &[]),
    );
    assert_eq!(state(&before, cx), (Some("other".into()), true, true));
    assert_eq!(state(&after, cx).0, Some("other".into()));

    // Removal reveals the bundled claim and drops syntax for the now
    // unregistered explicit language.
    cx.update(|cx| {
        assert!(super::unregister(
            ExtensionId::new(2),
            ExtensionLifecycleId::new(1),
            other,
            cx
        ))
    });
    assert_eq!(state(&before, cx), (Some("other".into()), true, false));
    assert_eq!(state(&after, cx), (Some("rust".into()), false, true));

    let uri = ResourceUri::parse("mem://workspace/main.rs").unwrap();
    cx.update(|cx| super::set_document_language(&before, Some(&uri), None, cx));
    assert_eq!(state(&before, cx), (Some("rust".into()), false, true));

    cx.update(|cx| super::remove_lifecycle(ExtensionId::new(1), ExtensionLifecycleId::new(1), cx));
    assert_eq!(state(&before, cx), (None, false, false));
    assert_eq!(state(&after, cx), (None, false, false));
    let _ = rust;
}

#[gpui::test]
fn save_as_redetects_unless_the_language_is_explicit(cx: &mut TestAppContext) {
    let documents = install(cx);
    register(cx, owner(BUNDLED, 1), claiming("rust", &["rs"], &[], &[]));
    register(cx, owner(BUNDLED, 1), claiming("python", &["py"], &[], &[]));
    for explicit in [false, true] {
        let model = cx.new(|_| BufferModel::from_text("x = 1"));
        let id = documents.update(cx, |documents, cx| {
            documents.create_untitled("Untitled", model.clone(), cx)
        });
        assert_eq!(state(&model, cx).0, None);
        if explicit {
            cx.update(|cx| super::set_document_language(&model, None, Some("rust".into()), cx));
        }
        let uri = ResourceUri::parse(&format!("mem://workspace/{explicit}.py")).unwrap();
        documents.update(cx, |documents, cx| {
            let capture = documents.begin_persistence(id, &model).unwrap();
            assert!(documents.finish_persistence(
                id,
                &model,
                &capture,
                ("saved.py", uri),
                (ResourceVersion::new([1]), 0),
                cx,
            ));
            assert!(matches!(
                documents.get(id).unwrap().state(),
                DocumentState::Persisted { .. }
            ));
        });
        let expected = if explicit { "rust" } else { "python" };
        assert_eq!(state(&model, cx), (Some(expected.into()), explicit, true));
    }
}

#[gpui::test]
fn plain_save_keeps_the_detected_language(cx: &mut TestAppContext) {
    let documents = install(cx);
    register(
        cx,
        owner(BUNDLED, 1),
        claiming("python", &[], &[], &["python3"]),
    );
    let model = cx.new(|_| BufferModel::from_text("#!/usr/bin/env python3\n"));
    let uri = ResourceUri::parse("mem://workspace/run").unwrap();
    let id = documents.update(cx, |documents, cx| {
        documents.create_persisted(
            "run",
            model.clone(),
            uri.clone(),
            0,
            ResourceVersion::new([1]),
            cx,
        )
    });
    assert_eq!(state(&model, cx).0, Some("python".into()));

    model.update(cx, |model, _| {
        model.replace(0..model.len(), "#!/bin/sh\n").unwrap()
    });
    let revision = model.read_with(cx, |model, _| model.revision());
    documents.update(cx, |documents, cx| {
        let capture = documents.begin_persistence(id, &model).unwrap();
        assert!(documents.finish_persistence(
            id,
            &model,
            &capture,
            ("run", uri),
            (ResourceVersion::new([2]), revision),
            cx,
        ));
    });
    assert_eq!(state(&model, cx).0, Some("python".into()));
}

#[gpui::test]
fn injecting_languages_reparse_when_registrations_change(cx: &mut TestAppContext) {
    let documents = install(cx);
    let mut markdown = claiming("markdown", &["md"], &[], &[]);
    markdown.info.grammar = "markdown".into();
    markdown.highlights = "(fenced_code_block) @markup.raw.block".into();
    markdown.injections = MARKDOWN_INJECTIONS.into();
    register(cx, owner(BUNDLED, 1), markdown);
    let text = "```rust\nlet x = 1;\n```\n";
    let model = persisted(&documents, "mem://workspace/README.md", text, cx);
    let unchanged = persisted(&documents, "mem://workspace/main.rs", "", cx);
    cx.run_until_parked();
    let highlights = |cx: &TestAppContext| {
        model.read_with(cx, |model, _| model.syntax_highlights(0..text.len()))
    };
    assert!(
        highlights(cx)
            .iter()
            .all(|(_, highlight)| *highlight == Highlight::resolve("markup.raw.block").unwrap())
    );

    register(cx, owner(BUNDLED, 1), claiming("rust", &["rs"], &[], &[]));
    let rust_generation = unchanged.read_with(cx, |model, _| model.syntax_generation());
    assert!(rust_generation.is_some());
    cx.run_until_parked();
    let variable = Highlight::resolve("variable").unwrap();
    assert!(
        highlights(cx)
            .iter()
            .any(|(range, highlight)| *highlight == variable && &text[range.clone()] == "x")
    );
    // A registration a language cannot inject keeps its parse.
    register(cx, owner(BUNDLED, 1), claiming("python", &["py"], &[], &[]));
    assert_eq!(
        unchanged.read_with(cx, |model, _| model.syntax_generation()),
        rust_generation
    );
}

#[gpui::test]
fn buffers_outside_documents_keep_a_fixed_language(cx: &mut TestAppContext) {
    install(cx);
    let model = cx.new(|_| BufferModel::from_read_only_text("fn main() {}"));
    cx.update(|cx| super::set_language(&model, "rust".into(), cx));
    assert_eq!(state(&model, cx), (Some("rust".into()), true, false));
    register(cx, owner(BUNDLED, 1), claiming("rust", &["rs"], &[], &[]));
    assert_eq!(state(&model, cx), (Some("rust".into()), true, true));
}
