use crate::host::protocol::TextEdit;

use super::*;

#[gpui::test]
fn handles_are_monotonic_and_invalidated_buffers_stay_invalid(cx: &mut gpui::TestAppContext) {
    let mut registry = BufferRegistry::new();
    let first_handle = registry.register(WeakEntity::new_invalid());
    let second_handle = registry.register(WeakEntity::new_invalid());
    assert_ne!(first_handle, second_handle);

    registry.invalidate(first_handle);
    assert!(cx.read(|cx| registry.resolve(first_handle, cx)).is_err());

    let third_handle = registry.register(WeakEntity::new_invalid());
    assert_ne!(third_handle, first_handle);
    assert_ne!(third_handle, second_handle);
}

#[gpui::test]
fn opening_the_same_model_reuses_its_handle(cx: &mut gpui::TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("text"));
    let other = cx.new(|_| BufferModel::from_text("other"));
    let mut registry = BufferRegistry::new();
    let first = registry.open(&model);
    assert_eq!(registry.open(&model), first);
    assert_eq!(registry.handle_for(&model), Some(first));
    assert_ne!(registry.open(&other), first);
    assert_eq!(registry.buffers.len(), 2);
}

#[test]
fn recovered_text_is_dirty_against_its_baseline_without_undo_history() {
    let mut model = BufferModel::from_recovered_text("on disk", "recovered");
    assert_eq!(model.text(), "recovered");
    assert!(!model.content_matches_revision(0));
    assert!(model.take_pending_change().is_none());
    assert!(!model.undo().unwrap());

    let unchanged = BufferModel::from_recovered_text("same", "same");
    assert!(unchanged.content_matches_revision(0));
}

#[test]
fn replacement_is_one_public_commit() {
    let mut model = BufferModel::from_text("abc");
    assert!(model.replace(1..2, "XYZ").unwrap());
    assert_eq!(model.text(), "aXYZc");
    assert_eq!(model.revision(), 1);
    assert_eq!(model.edit_seq(), 2);
    assert_eq!(
        model.buffer.retained_edits(),
        0,
        "stabilized edits are discarded"
    );

    assert!(!model.replace(0..0, "").unwrap());
    assert_eq!(model.revision(), 1);
}

#[test]
fn history_replay_reports_one_edit_covering_its_primitives() {
    let mut model = BufferModel::from_text("<one two three>");
    let edit = |start, end, text: &str| TextEdit {
        range: ByteRange {
            start_byte_offset: start,
            end_byte_offset: end,
        },
        text: text.into(),
    };
    assert!(
        model
            .apply_edits(&[edit(1, 4, "1"), edit(9, 14, "3")], 0)
            .unwrap()
    );
    assert_eq!(model.text(), "<1 two 3>");
    model.pending_changes.clear();

    assert!(model.undo().unwrap());
    assert_eq!(
        model.take_pending_change().unwrap().edits,
        vec![edit(1, 8, "one two three")]
    );
    assert!(model.redo().unwrap());
    assert_eq!(
        model.take_pending_change().unwrap().edits,
        vec![edit(1, 14, "1 two 3")]
    );
    assert_eq!(model.whole_text_reads(), 1, "only the assertion above");
}

#[test]
fn line_queries_and_utf16_conversions() {
    let model = BufferModel::from_text("a😀\r\nβ\n");
    assert_eq!(model.line_count(), 3);
    assert_eq!(model.line_range(0), 0..6);
    assert_eq!(model.read(model.line_range(0)), "a😀\r");
    assert_eq!(model.line_range(1), 7..9);
    assert_eq!(model.line_range(2), 10..10);
    assert_eq!(model.line_of_offset(8), 1);
    assert_eq!(model.utf16_of_offset(7), 5);
    assert_eq!(model.offset_of_utf16(5), 7);
    assert_eq!(model.len(), 10);
    assert!(!model.is_char_boundary(2));
}

#[test]
fn line_ending_follows_the_first_terminator() {
    assert_eq!(BufferModel::from_text("").line_ending(), "\n");
    assert_eq!(BufferModel::from_text("a\r").line_ending(), "\n");
    assert_eq!(BufferModel::from_text("a\nb\r\n").line_ending(), "\n");
    assert_eq!(BufferModel::from_text("a\r\nb\n").line_ending(), "\r\n");
    assert_eq!(BufferModel::from_text("\r\n").line_ending(), "\r\n");
}

#[test]
fn grouped_history_replays_through_revisions_notifications_and_anchors() {
    let mut model = BufferModel::from_text("tail");
    let tail = model.add_view_position(0..4);
    let grouping = EditGrouping {
        context: 7,
        kind: super::super::history::EditGroupKind::Typing,
    };

    assert!(model.replace_grouped(0..0, "a", grouping).unwrap());
    assert!(model.replace_grouped(1..1, "β", grouping).unwrap());
    assert_eq!(model.text(), "aβtail");
    assert_eq!(model.resolve_view_position(tail), Some(3..7));
    assert_eq!(model.revision(), 2);
    assert!(!model.content_matches_revision(1));
    assert!(model.content_matches_revision(2));
    model.pending_changes.clear();

    assert!(model.undo().unwrap());
    assert_eq!(model.text(), "tail");
    assert_eq!(model.resolve_view_position(tail), Some(0..4));
    assert_eq!(model.revision(), 3);
    assert!(model.content_matches_revision(0));
    assert_eq!(
        model.take_pending_change(),
        Some(CommittedBufferChange {
            before_revision: 2,
            revision: 3,
            edits: vec![TextEdit {
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 3,
                },
                text: "".into(),
            }],
        })
    );

    assert!(model.redo().unwrap());
    assert_eq!(model.text(), "aβtail");
    assert_eq!(model.resolve_view_position(tail), Some(3..7));
    assert_eq!(model.revision(), 4);
    assert!(model.content_matches_revision(2));
    assert_eq!(
        model.take_pending_change().unwrap().edits,
        vec![TextEdit {
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 0,
            },
            text: "aβ".into(),
        }]
    );
}

#[test]
fn histories_are_independent_and_divergent_edits_clear_redo() {
    let mut first = BufferModel::from_text("one");
    let mut second = BufferModel::from_text("two");
    first.replace(3..3, "!").unwrap();
    second.replace(3..3, "?").unwrap();

    assert!(first.undo().unwrap());
    assert_eq!(first.text(), "one");
    assert_eq!(second.text(), "two?");
    first.replace(3..3, ".").unwrap();
    assert!(!first.redo().unwrap());
    assert_eq!(first.text(), "one.");
    assert!(second.undo().unwrap());
    assert_eq!(second.text(), "two");
}

#[test]
fn multiline_unicode_replacement_preserves_other_view_positions_and_commit_text() {
    let prefix = "fn main() {\n    ";
    let selected = "let café = \"👨‍👩‍👧‍👦\";\n    println!(\"中\");";
    let suffix = "\n}\n";
    let mut model = BufferModel::from_text(format!("{prefix}{selected}{suffix}"));
    let start = prefix.len();
    let end = start + selected.len();
    let following_view = model.add_view_position(end..end + suffix.len());
    let replacement = "// e\u{301} and 日本語\n    return;";

    assert!(model.replace(start..end, replacement).unwrap());
    assert_eq!(model.text(), format!("{prefix}{replacement}{suffix}"));
    let new_end = start + replacement.len();
    assert_eq!(
        model.resolve_view_position(following_view),
        Some(new_end..new_end + suffix.len())
    );
    let change = model.take_pending_change().unwrap();
    assert_eq!((change.before_revision, change.revision), (0, 1));
    assert_eq!(change.edits.len(), 1);
    assert_eq!(change.edits[0].range.start_byte_offset, start);
    assert_eq!(change.edits[0].range.end_byte_offset, end);
    assert_eq!(change.edits[0].text, replacement);
    assert!(model.take_pending_change().is_none());
}

#[test]
fn deleting_all_text_and_typing_again_keeps_persistent_view_positions_valid() {
    let mut model = BufferModel::from_text("# Notes\n\n你好 😀\n");
    let position = model.add_view_position(2..model.text().len());
    assert!(model.replace(0..model.text().len(), "").unwrap());
    assert_eq!(model.resolve_view_position(position), Some(0..0));
    assert!(model.replace(0..0, "新しい\n").unwrap());
    let position = model.resolve_view_position(position).unwrap();
    let text = model.text();
    assert!(position.start <= position.end && position.end <= text.len());
    assert!(text.is_char_boundary(position.start));
    assert!(text.is_char_boundary(position.end));
    assert_eq!(model.revision(), 2);
}

#[test]
fn read_only_models_reject_local_and_extension_text_edits() {
    let mut model = BufferModel::from_read_only_text("generated text");
    let extension_edit = TextEdit {
        range: ByteRange {
            start_byte_offset: 0,
            end_byte_offset: 9,
        },
        text: "changed".into(),
    };

    assert_eq!(model.access_policy, BufferAccessPolicy::ReadOnly);
    assert_eq!(
        model.replace(0..9, "changed"),
        Err(BufferAccessError::ReadOnly)
    );
    assert_eq!(
        model.apply_edits(&[extension_edit], 0),
        Err(BufferAccessError::ReadOnly)
    );
    assert_eq!(model.text(), "generated text");
    assert_eq!(model.revision(), 0);
    assert_eq!(model.take_pending_change(), None);
}

#[test]
fn read_only_models_keep_non_text_operations_available() {
    let mut model = BufferModel::from_read_only_text("generated text");
    let position = model.add_view_position(0..9);

    model
        .replace_contributions(
            ContributionSource::fixture(),
            &[EditorContribution {
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 9,
                },
                decoration: Some(DecorationToken::Info),
                gutter: None,
                command: None,
            }],
            0,
        )
        .unwrap();

    assert_eq!(
        model.snapshot(None).unwrap().text.to_utf8(),
        "generated text"
    );
    assert_eq!(model.resolve_view_position(position), Some(0..9));
    assert_eq!(model.resolved_contributions().len(), 1);
    model.close();
    assert!(!model.is_open());
}

#[test]
fn contribution_sets_stabilize_and_replace_atomically() {
    let mut model = BufferModel::from_text("abc def");
    let source = ContributionSource {
        extension: ExtensionId::new(7),
        lifecycle: ExtensionLifecycleId::new(3),
    };
    model
        .replace_contributions(
            source,
            &[EditorContribution {
                range: ByteRange {
                    start_byte_offset: 4,
                    end_byte_offset: 7,
                },
                decoration: Some(DecorationToken::Warning),
                gutter: None,
                command: None,
            }],
            0,
        )
        .unwrap();

    assert!(model.replace(0..0, "++").unwrap());
    assert_eq!(
        model.resolved_contributions(),
        [ResolvedEditorContribution {
            range: ByteRange {
                start_byte_offset: 6,
                end_byte_offset: 9,
            },
            source,
            decoration: Some(DecorationToken::Warning),
            gutter: None,
            command: None,
        }]
    );

    assert_eq!(
        model.replace_contributions(source, &[], 0),
        Err(ContributionError::RevisionConflict)
    );
    assert_eq!(model.resolved_contributions().len(), 1);
    assert_eq!(
        model.replace_contributions(
            source,
            &[EditorContribution {
                range: ByteRange {
                    start_byte_offset: 7,
                    end_byte_offset: 99,
                },
                decoration: Some(DecorationToken::Error),
                gutter: None,
                command: None,
            }],
            model.revision(),
        ),
        Err(ContributionError::InvalidRange)
    );
    assert_eq!(model.resolved_contributions().len(), 1);

    model
        .replace_contributions(source, &[], model.revision())
        .unwrap();
    assert!(model.resolved_contributions().is_empty());
}

#[test]
fn replacing_one_contribution_source_preserves_other_sources() {
    let mut model = BufferModel::from_text("abcdef");
    let extension_source = ContributionSource {
        extension: ExtensionId::new(7),
        lifecycle: ExtensionLifecycleId::new(3),
    };
    for (source, range, decoration) in [
        (ContributionSource::fixture(), 0..2, DecorationToken::Info),
        (extension_source, 2..4, DecorationToken::Warning),
    ] {
        model
            .replace_contributions(
                source,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: range.start,
                        end_byte_offset: range.end,
                    },
                    decoration: Some(decoration),
                    gutter: None,
                    command: None,
                }],
                model.revision(),
            )
            .unwrap();
    }

    model
        .replace_contributions(
            extension_source,
            &[EditorContribution {
                range: ByteRange {
                    start_byte_offset: 4,
                    end_byte_offset: 6,
                },
                decoration: Some(DecorationToken::Error),
                gutter: None,
                command: None,
            }],
            model.revision(),
        )
        .unwrap();

    assert_eq!(model.contributions.sets.len(), 2);
    assert_eq!(model.contributions.metadata.len(), 2);
    assert_eq!(model.anchored_ranges.len(), 2);
    assert_eq!(
        model.resolved_contributions(),
        [
            ResolvedEditorContribution {
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 2,
                },
                source: ContributionSource::fixture(),
                decoration: Some(DecorationToken::Info),
                gutter: None,
                command: None,
            },
            ResolvedEditorContribution {
                range: ByteRange {
                    start_byte_offset: 4,
                    end_byte_offset: 6,
                },
                source: extension_source,
                decoration: Some(DecorationToken::Error),
                gutter: None,
                command: None,
            },
        ]
    );
}

#[test]
fn empty_buffer_contributions_are_rejected_recoverably() {
    let mut model = BufferModel::from_text("");
    let source = ContributionSource::fixture();

    assert_eq!(
        model.replace_contributions(
            source,
            &[EditorContribution {
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 0,
                },
                decoration: Some(DecorationToken::Info),
                gutter: None,
                command: None,
            }],
            0,
        ),
        Err(ContributionError::InvalidRange)
    );
    assert!(model.contributions.sets.is_empty());
    assert!(model.contributions.metadata.is_empty());
    assert!(model.anchored_ranges.is_empty());
}

#[test]
fn contribution_ownership_rejects_stale_lifecycles_and_cleans_up() {
    let mut model = BufferModel::from_text("abc");
    let extension = ExtensionId::new(7);
    let source = ContributionSource {
        extension,
        lifecycle: ExtensionLifecycleId::new(3),
    };
    let stale_source = ContributionSource {
        extension,
        lifecycle: ExtensionLifecycleId::new(2),
    };
    model
        .replace_contributions(
            source,
            &[EditorContribution {
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 3,
                },
                decoration: Some(DecorationToken::Error),
                gutter: None,
                command: None,
            }],
            0,
        )
        .unwrap();

    assert_eq!(
        model.dispose_contributions(stale_source),
        Err(ContributionError::NotFound)
    );
    assert_eq!(model.resolved_contributions().len(), 1);
    assert!(!model.remove_contribution_lifecycle(extension, ExtensionLifecycleId::new(2)));
    assert!(model.remove_contribution_lifecycle(extension, ExtensionLifecycleId::new(3)));
    assert!(model.resolved_contributions().is_empty());
}

#[test]
fn stabilization_prunes_consumed_contribution_metadata() {
    let mut model = BufferModel::from_text("abc def");
    let source = ContributionSource::fixture();
    model
        .replace_contributions(
            source,
            &[EditorContribution {
                range: ByteRange {
                    start_byte_offset: 5,
                    end_byte_offset: 6,
                },
                decoration: Some(DecorationToken::Warning),
                gutter: None,
                command: None,
            }],
            0,
        )
        .unwrap();

    assert!(model.replace(4..7, "").unwrap());

    assert!(model.resolved_contributions().is_empty());
    assert!(model.contributions.metadata.is_empty());
    assert!(model.contributions.sets[&source].anchored_ranges.is_empty());
    assert!(model.anchored_ranges.is_empty());
}

#[test]
fn decoration_precedence_is_application_owned_and_deterministic() {
    let mut model = BufferModel::from_text("abc");
    for (source, decoration) in [
        (ContributionSource::fixture(), DecorationToken::Warning),
        (
            ContributionSource {
                extension: ExtensionId::new(7),
                lifecycle: ExtensionLifecycleId::new(3),
            },
            DecorationToken::Info,
        ),
    ] {
        model
            .replace_contributions(
                source,
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 3,
                    },
                    decoration: Some(decoration),
                    gutter: None,
                    command: None,
                }],
                0,
            )
            .unwrap();
    }

    assert_eq!(
        model
            .resolved_contributions()
            .into_iter()
            .filter_map(|contribution| contribution.decoration)
            .collect::<Vec<_>>(),
        [DecorationToken::Info, DecorationToken::Warning]
    );
}

#[test]
fn checked_reads_reject_invalid_utf8_ranges_without_touching_the_buffer() {
    let model = BufferModel::from_text("aé中");

    assert_eq!(
        model.read_checked(ByteRange {
            start_byte_offset: 1,
            end_byte_offset: 3,
        }),
        Ok("é".into())
    );
    assert_eq!(
        model.read_checked(ByteRange {
            start_byte_offset: 2,
            end_byte_offset: 3,
        }),
        Err(BufferAccessError::InvalidRange)
    );
    assert_eq!(
        model.read_checked(ByteRange {
            start_byte_offset: 4,
            end_byte_offset: 3,
        }),
        Err(BufferAccessError::InvalidRange)
    );
    assert_eq!(model.text(), "aé中");
    assert_eq!(model.revision(), 0);
}

#[test]
fn checked_reads_reject_closed_buffers() {
    let mut model = BufferModel::from_text("abc");
    model.snapshot(None).unwrap();
    model.close();
    assert!(matches!(
        model.snapshot(None),
        Err(BufferAccessError::Closed)
    ));

    assert_eq!(
        model.read_checked(ByteRange {
            start_byte_offset: 0,
            end_byte_offset: 3,
        }),
        Err(BufferAccessError::Closed)
    );
}

#[test]
fn snapshots_and_atomic_edits_use_public_revisions() {
    let mut model = BufferModel::from_text("aé中z");
    let snapshot = model
        .snapshot(Some(ByteRange {
            start_byte_offset: 1,
            end_byte_offset: 6,
        }))
        .unwrap();
    assert_eq!(snapshot.text.to_utf8(), "é中");
    assert_eq!(snapshot.revision, 0);

    assert!(
        model
            .apply_edits(
                &[
                    TextEdit {
                        range: ByteRange {
                            start_byte_offset: 0,
                            end_byte_offset: 1,
                        },
                        text: "A".into(),
                    },
                    TextEdit {
                        range: ByteRange {
                            start_byte_offset: 6,
                            end_byte_offset: 7,
                        },
                        text: "Z".into(),
                    },
                ],
                0,
            )
            .unwrap()
    );
    assert_eq!(model.text(), "Aé中Z");
    assert_eq!(model.revision(), 1);
}

#[test]
fn edits_reject_invalid_batches_without_partial_mutation() {
    let mut model = BufferModel::from_text("abcdef");
    let overlapping = [
        TextEdit {
            range: ByteRange {
                start_byte_offset: 1,
                end_byte_offset: 3,
            },
            text: "X".into(),
        },
        TextEdit {
            range: ByteRange {
                start_byte_offset: 2,
                end_byte_offset: 4,
            },
            text: "Y".into(),
        },
    ];
    assert_eq!(
        model.apply_edits(&overlapping, 0),
        Err(BufferAccessError::InvalidEditBatch)
    );
    assert_eq!(model.text(), "abcdef");
    assert_eq!(model.revision(), 0);

    assert_eq!(
        model.apply_edits(&[], 1),
        Err(BufferAccessError::RevisionConflict)
    );
}

#[test]
fn semantically_empty_batches_do_not_commit() {
    let mut model = BufferModel::from_text("abcdef");
    let edits = [TextEdit {
        range: ByteRange {
            start_byte_offset: 1,
            end_byte_offset: 3,
        },
        text: "bc".into(),
    }];
    assert!(!model.apply_edits(&edits, 0).unwrap());
    assert!(!model.apply_edits(&[], 0).unwrap());
    assert_eq!(model.text(), "abcdef");
    assert_eq!(model.revision(), 0);
}

#[test]
fn snapshots_preserve_unicode_and_reject_split_scalar_boundaries() {
    let text = "ASCII 中 العربية e\u{301} 👩‍💻";
    let model = BufferModel::from_text(text);
    let full = model.snapshot(None).unwrap();
    assert_eq!(full.text.to_utf8(), text);

    let emoji_start = text.find('👩').unwrap();
    let emoji_end = text.len();
    assert_eq!(
        model
            .snapshot(Some(ByteRange {
                start_byte_offset: emoji_start,
                end_byte_offset: emoji_end,
            }))
            .unwrap()
            .text
            .to_utf8(),
        "👩‍💻"
    );
    assert_eq!(
        model.snapshot(Some(ByteRange {
            start_byte_offset: emoji_start + 1,
            end_byte_offset: emoji_end,
        })),
        Err(BufferAccessError::InvalidRange)
    );
}

#[test]
fn snapshot_cache_reuses_storage_and_invalidates_on_edit() {
    let mut model = BufferModel::from_text("abc");
    let first = model.snapshot(None).unwrap();
    let second = model.snapshot(None).unwrap();
    let (SnapshotText(first), SnapshotText(second)) = (first.text, second.text);
    assert!(Arc::ptr_eq(&first, &second));

    assert!(model.replace(1..2, "B").unwrap());
    let third = model.snapshot(None).unwrap();
    let SnapshotText(third) = third.text;
    assert!(!Arc::ptr_eq(&first, &third));
    assert_eq!(String::from_utf16(&third).unwrap(), "aBc");
}

#[test]
fn buffer_subscriptions_are_scoped_by_buffer_and_lifecycle() {
    let mut registry = BufferSubscriptionRegistry::new();
    let first_buffer = BufferHandle::new(1);
    let second_buffer = BufferHandle::new(2);
    let extension = ExtensionId::new(7);
    let first_lifecycle = ExtensionLifecycleId::new(3);
    let second_lifecycle = ExtensionLifecycleId::new(4);
    let first = registry.subscribe(first_buffer, extension, first_lifecycle);
    let second = registry.subscribe(first_buffer, extension, second_lifecycle);
    registry.subscribe(second_buffer, ExtensionId::new(8), first_lifecycle);

    assert!(!registry.unsubscribe(first, extension, second_lifecycle));
    assert_eq!(registry.for_buffer(first_buffer).count(), 2);

    registry.remove_lifecycle(extension, first_lifecycle);
    assert_eq!(
        registry.for_buffer(first_buffer).collect::<Vec<_>>(),
        [BufferSubscription {
            id: second,
            buffer: first_buffer,
            extension,
            lifecycle: second_lifecycle,
        }]
    );
    assert_eq!(registry.for_buffer(second_buffer).count(), 1);

    registry.remove_buffer(second_buffer);
    assert_eq!(registry.for_buffer(second_buffer).count(), 0);
}

fn rust_syntax() -> Option<SyntaxState> {
    use crate::app::syntax::SyntaxLanguage;
    let highlights = "\"fn\" @keyword\n(identifier) @variable\n(line_comment) @comment\n";
    let language = Arc::new(SyntaxLanguage::new("rust", highlights, "").unwrap());
    Some(SyntaxState::new(language, Arc::new(|_| None)))
}

fn syntax_names(model: &BufferModel) -> Vec<(String, &'static str)> {
    model
        .syntax_highlights(0..model.len())
        .into_iter()
        .map(|(range, highlight)| (model.read(range), highlight.name()))
        .collect()
}

#[gpui::test]
fn syntax_follows_every_commit_path(cx: &mut gpui::TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("fn a() {}"));
    let fresh = |text: &str, cx: &mut gpui::TestAppContext| {
        let model = cx.new(|_| BufferModel::from_text(text));
        model.update(cx, |model, cx| model.set_syntax(rust_syntax(), cx));
        model.read_with(cx, |model, _| syntax_names(model))
    };
    model.update(cx, |model, cx| model.set_syntax(rust_syntax(), cx));
    assert_eq!(
        model.read_with(cx, |model, _| syntax_names(model)),
        [("fn".into(), "keyword"), ("a".into(), "variable")]
    );

    model.update(cx, |model, _| model.replace(0..0, "// x\n").unwrap());
    let expected = fresh("// x\nfn a() {}", cx);
    assert_eq!(expected[0], ("// x".into(), "comment"));
    assert_eq!(
        model.read_with(cx, |model, _| syntax_names(model)),
        expected
    );

    model.update(cx, |model, _| {
        let edits = [TextEdit {
            range: ByteRange {
                start_byte_offset: 8,
                end_byte_offset: 9,
            },
            text: "bc".into(),
        }];
        assert!(model.apply_edits(&edits, model.revision()).unwrap());
    });
    assert_eq!(
        model.read_with(cx, |model, _| syntax_names(model)),
        fresh("// x\nfn bc() {}", cx)
    );

    model.update(cx, |model, _| assert!(model.undo().unwrap()));
    assert_eq!(
        model.read_with(cx, |model, _| syntax_names(model)),
        expected
    );
    model.update(cx, |model, _| assert!(model.redo().unwrap()));
    assert_eq!(
        model.read_with(cx, |model, _| syntax_names(model)),
        fresh("// x\nfn bc() {}", cx)
    );
}

#[gpui::test]
fn an_over_budget_parse_continues_in_the_background(cx: &mut gpui::TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("fn a() { let x = 1; }\n".repeat(20_000)));
    model.update(cx, |model, cx| {
        model.set_syntax_budget(Duration::ZERO);
        model.set_syntax(rust_syntax(), cx);
    });
    let generation = model.read_with(cx, |model, _| {
        assert!(model.syntax_highlights(0..2).is_empty());
        model.syntax_generation()
    });

    // A commit while the parse runs is rebased into its result.
    model.update(cx, |model, cx| {
        model.replace(0..0, "// c\n").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    model.read_with(cx, |model, _| {
        assert_ne!(model.syntax_generation(), generation);
        let names: Vec<_> = model
            .syntax_highlights(0..12)
            .into_iter()
            .map(|(range, highlight)| (model.read(range), highlight.name()))
            .collect();
        assert_eq!(
            names,
            [
                ("// c".into(), "comment"),
                ("fn".into(), "keyword"),
                ("a".into(), "variable")
            ]
        );
    });
}

#[gpui::test]
fn closing_or_clearing_syntax_releases_it(cx: &mut gpui::TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("fn a() {}"));
    model.update(cx, |model, cx| {
        model.set_syntax(rust_syntax(), cx);
        let parsed = model.syntax_generation();
        model.set_syntax(rust_syntax(), cx);
        assert_ne!(model.syntax_generation(), parsed);
        model.set_syntax(None, cx);
        assert_eq!(model.syntax_generation(), None);
        model.set_syntax(rust_syntax(), cx);
        model.close();
        assert_eq!(model.syntax_generation(), None);
    });
}
