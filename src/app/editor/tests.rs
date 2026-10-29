use gpui::{
    AppContext, ClipboardItem, EntityInputHandler, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, ScrollDelta, ScrollWheelEvent, TestAppContext, point, px,
};

use super::{
    BufferModel, ContributionSource, EditorRenderingOptions, EditorView, EditorViewState,
    LINE_HEIGHT, ResolvedEditorContribution, flat_utf16_to_position,
    layout::{HighlightSpan, runs_for, shape_editor_line, x_for_index_dir},
    project_contributions, to_flat_utf16,
};
use std::sync::Arc;

use crate::{
    app::{
        completion::CompletionProviderRegistry,
        syntax::{SyntaxLanguage, SyntaxState},
        theme::{Highlight, Theme},
    },
    host::protocol::{
        BufferHandle, ByteRange, CompletionResponse, CompletionResultItem, DecorationToken,
        EditorContribution, ExtensionId, ExtensionLifecycleId, GutterToken,
    },
};

#[gpui::test]
fn ordinary_editing_keeps_projection_clipboard_and_composition_synchronous(
    cx: &mut TestAppContext,
) {
    let model = cx.new(|_| BufferModel::from_text(""));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.replace_text_in_range(None, "a", window, cx);
            view.replace_text_in_range(None, "👩‍💻", window, cx);
            view.replace_text_in_range(None, "é", window, cx);
            assert_eq!(view.text(cx), "a👩‍💻é");
            view.execute_editing_command("editor.delete-backward", window, cx);
            assert_eq!(view.text(cx), "a👩‍💻");
            view.execute_editing_command("editor.select-all", window, cx);
            view.execute_editing_command("editor.cut", window, cx);
            assert_eq!(view.text(cx), "");
            view.execute_editing_command("editor.paste", window, cx);
            assert_eq!(view.text(cx), "a👩‍💻");
            view.replace_and_mark_text_in_range(None, "に", Some(0..1), window, cx);
            assert_eq!(view.marked_range_utf16, Some(6..7));
            view.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
            assert_eq!(view.text(cx), "a👩‍💻日本");
            view.replace_text_in_range(None, "日本語", window, cx);
            assert_eq!(view.text(cx), "a👩‍💻日本語");
            assert_eq!(view.marked_range_utf16, None);
            view.execute_editing_command("editor.undo", window, cx);
            assert_eq!(view.text(cx), "a👩‍💻");
            view.execute_editing_command("editor.redo", window, cx);
            assert_eq!(view.text(cx), "a👩‍💻日本語");
            view.execute_editing_command("editor.select-all", window, cx);
            view.replace_text_in_range(None, "one\r\ntwo\rthree", window, cx);
            assert_eq!(view.text(cx), "one\ntwo\nthree");
        })
    });
}

#[gpui::test]
fn typing_deletion_and_interaction_boundaries_form_practical_undo_steps(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text(""));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.replace_text_in_range(None, "a", window, cx);
            view.replace_text_in_range(None, "β", window, cx);
            view.replace_text_in_range(None, "c", window, cx);
            view.execute_editing_command("editor.undo", window, cx);
            assert_eq!(view.text(cx), "");
            view.execute_editing_command("editor.redo", window, cx);
            assert_eq!(view.text(cx), "aβc");

            view.execute_editing_command("editor.move-document-end", window, cx);
            view.execute_editing_command("editor.delete-backward", window, cx);
            view.execute_editing_command("editor.delete-backward", window, cx);
            assert_eq!(view.text(cx), "a");
            view.execute_editing_command("editor.undo", window, cx);
            assert_eq!(view.text(cx), "aβc");

            view.execute_editing_command("editor.move-document-end", window, cx);
            view.execute_editing_command("editor.move-left", window, cx);
            view.replace_text_in_range(None, "X", window, cx);
            assert_eq!(view.text(cx), "aβXc");
            view.execute_editing_command("editor.undo", window, cx);
            assert_eq!(view.text(cx), "aβc");

            cx.write_to_clipboard(gpui::ClipboardItem::new_string(" pasted".into()));
            view.execute_editing_command("editor.paste", window, cx);
            assert_eq!(view.text(cx), "aβ pastedc");
            view.execute_editing_command("editor.undo", window, cx);
            assert_eq!(view.text(cx), "aβc");
        });
    });
}

#[gpui::test]
fn undo_and_redo_restore_the_originating_views_grouped_selection(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("abcdef"));
    let (other, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    let origin = cx.new(|cx| EditorView::new(model.clone(), cx));

    cx.update(|window, cx| {
        origin.update(cx, |view, cx| {
            view.anchor_col = 4;
            view.cursor_col = 1;
            view.has_selection = true;
            view.sync_view_position(cx);
            view.replace_text_in_range(None, "X", window, cx);
            view.replace_text_in_range(None, "Y", window, cx);
        });
        other.update(cx, |view, cx| {
            view.cursor_col = 5;
            view.sync_view_position(cx);
            view.execute_editing_command("editor.undo", window, cx);
        });
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let origin = origin.read(cx);
        let other = other.read(cx);
        assert_eq!(origin.text(cx), "abcdef");
        assert_eq!((origin.anchor_col, origin.cursor_col), (4, 1));
        assert!(origin.has_selection);
        assert!(origin.selection_reversed);
        assert!(!other.has_selection);
        assert_ne!((other.cursor_line, other.cursor_col), (0, 1));
    });

    cx.update(|window, cx| {
        other.update(cx, |view, cx| {
            view.execute_editing_command("editor.redo", window, cx);
        });
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let origin = origin.read(cx);
        assert_eq!(origin.text(cx), "aXYef");
        assert_eq!((origin.cursor_line, origin.cursor_col), (0, 3));
        assert!(!origin.has_selection);
    });
}

#[gpui::test]
fn replay_ignores_view_state_for_an_originating_view_that_was_dropped(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("text"));
    let (survivor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    let origin = cx.new(|cx| EditorView::new(model.clone(), cx));

    cx.update(|window, cx| {
        origin.update(cx, |view, cx| {
            view.execute_editing_command("editor.move-document-end", window, cx);
            view.replace_text_in_range(None, "!", window, cx);
        });
    });
    drop(origin);
    cx.run_until_parked();

    cx.update(|window, cx| {
        survivor.update(cx, |view, cx| {
            view.execute_editing_command("editor.undo", window, cx);
        });
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let survivor = survivor.read(cx);
        assert_eq!(survivor.text(cx), "text");
        assert_eq!((survivor.cursor_line, survivor.cursor_col), (0, 0));
    });
}

#[gpui::test]
fn a_newer_edit_invalidates_an_unobserved_history_view_restore(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("abcdef"));
    let (other, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    let origin = cx.new(|cx| EditorView::new(model.clone(), cx));

    cx.update(|window, cx| {
        origin.update(cx, |view, cx| {
            view.anchor_col = 4;
            view.cursor_col = 1;
            view.has_selection = true;
            view.sync_view_position(cx);
            view.replace_text_in_range(None, "X", window, cx);
        });
        other.update(cx, |view, cx| {
            view.execute_editing_command("editor.undo", window, cx);
        });
        model.update(cx, |model, cx| {
            assert!(model.replace(0..0, "z").unwrap());
            cx.notify();
        });
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let origin = origin.read(cx);
        assert_eq!(origin.text(cx), "zabcdef");
        assert_eq!((origin.cursor_line, origin.cursor_col), (0, 5));
        assert!(!origin.has_selection);
    });
}

#[gpui::test]
fn movement_uses_visual_columns_graphemes_and_pane_height(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("éééx\na\nabcde\nword next\nlast"));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
    cx.refresh().unwrap();
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.cursor_col = "ééé".len();
            view.execute_editing_command("editor.move-down", window, cx);
            assert_eq!((view.cursor_line, view.cursor_col), (1, 1));
            view.execute_editing_command("editor.move-down", window, cx);
            assert_eq!((view.cursor_line, view.cursor_col), (2, 3));
            view.execute_editing_command("editor.move-document-start", window, cx);
            view.viewport_h = 2. * super::LINE_HEIGHT;
            view.execute_editing_command("editor.select-page-down", window, cx);
            assert_eq!(view.selected_byte_range(cx), Some(0..10));
            view.execute_editing_command("editor.move-document-end", window, cx);
            assert_eq!((view.cursor_line, view.cursor_col), (4, 4));
            view.execute_editing_command("editor.move-word-left", window, cx);
            assert_eq!((view.cursor_line, view.cursor_col), (4, 0));
            view.execute_editing_command("editor.select-word-left", window, cx);
            assert_eq!((view.cursor_line, view.cursor_col), (3, 5));
        })
    });
}

#[gpui::test]
fn tabs_hit_testing_and_horizontal_reveal_share_geometry(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text(format!("\tx{}", "z".repeat(300))));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
    cx.refresh().unwrap();
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.cursor_col = 1;
            let model = view.model.read(cx);
            let tab_x = view.caret_x(model, window, cx);
            assert!(f32::from(tab_x) > 20.);
            assert_eq!(
                view.hit_test(
                    model,
                    point(
                        view.text_bounds().origin.x + tab_x,
                        view.bounds.origin.y + px(5.)
                    ),
                    window,
                    cx
                ),
                (0, 1)
            );
            view.execute_editing_command("editor.move-line-end", window, cx);
        })
    });
    cx.refresh().unwrap();
    cx.read(|cx| assert!(editor.read(cx).scroll_x > 0.));
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.execute_editing_command("editor.move-line-start", window, cx);
        })
    });
    cx.refresh().unwrap();
    cx.read(|cx| assert_eq!(editor.read(cx).scroll_x, 0.));
}

#[gpui::test]
fn mouse_selection_keeps_press_anchor_across_reversal_and_ignores_foreign_drags(
    cx: &mut TestAppContext,
) {
    let model = cx.new(|_| BufferModel::from_text("alpha beta\nnext"));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
    cx.refresh().unwrap();
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            let position = |view: &mut EditorView, col, cx: &gpui::App| {
                view.cursor_col = col;
                point(
                    view.text_bounds().origin.x + view.caret_x(view.model.read(cx), window, cx),
                    view.bounds.origin.y + px(5.),
                )
            };
            let start = position(view, 3, cx);
            let end = position(view, 8, cx);
            let before = position(view, 1, cx);
            view.on_mouse_move(
                &MouseMoveEvent {
                    position: end,
                    modifiers: Modifiers::default(),
                    pressed_button: Some(MouseButton::Left),
                },
                window,
                cx,
            );
            assert!(!view.has_selection);
            view.on_mouse_down(
                &MouseDownEvent {
                    position: start,
                    modifiers: Modifiers::default(),
                    button: MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                },
                window,
                cx,
            );
            for p in [end, start, before] {
                view.on_mouse_move(
                    &MouseMoveEvent {
                        position: p,
                        modifiers: Modifiers::default(),
                        pressed_button: Some(MouseButton::Left),
                    },
                    window,
                    cx,
                );
            }
            assert_eq!(view.selected_byte_range(cx), Some(1..3));
            view.on_mouse_down(
                &MouseDownEvent {
                    position: start,
                    modifiers: Modifiers::default(),
                    button: MouseButton::Left,
                    click_count: 2,
                    first_mouse: false,
                },
                window,
                cx,
            );
            assert_eq!(view.selected_byte_range(cx), Some(0..5));
            view.on_mouse_down(
                &MouseDownEvent {
                    position: start,
                    modifiers: Modifiers::default(),
                    button: MouseButton::Left,
                    click_count: 3,
                    first_mouse: false,
                },
                window,
                cx,
            );
            assert_eq!(view.selected_byte_range(cx), Some(0..11));
        })
    });
}

#[gpui::test]
fn crlf_is_one_navigation_and_deletion_boundary(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("a\r\nb"));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.execute_editing_command("editor.move-line-end", window, cx);
            assert_eq!((view.cursor_line, view.cursor_col), (0, 1));
            view.execute_editing_command("editor.move-right", window, cx);
            assert_eq!((view.cursor_line, view.cursor_col), (1, 0));
            view.execute_editing_command("editor.delete-backward", window, cx);
            assert_eq!(view.text(cx), "ab");
        })
    });
}

#[gpui::test]
fn inserted_newlines_follow_a_crlf_buffer(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("a\r\nb"));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.execute_editing_command("editor.move-line-end", window, cx);
            view.execute_editing_command("editor.insert-newline", window, cx);
            assert_eq!(view.text(cx), "a\r\n\r\nb");
            assert_eq!((view.cursor_line, view.cursor_col), (1, 0));

            cx.write_to_clipboard(ClipboardItem::new_string("1\n2\r\n3".into()));
            view.execute_editing_command("editor.paste", window, cx);
            assert_eq!(view.text(cx), "a\r\n1\r\n2\r\n3\r\nb");
            assert_eq!((view.cursor_line, view.cursor_col), (3, 1));

            view.replace_text_in_range(None, "\n", window, cx);
            assert_eq!(view.text(cx), "a\r\n1\r\n2\r\n3\r\n\r\nb");

            view.execute_editing_command("editor.delete-backward", window, cx);
            view.execute_editing_command("editor.delete-forward", window, cx);
            assert_eq!(view.text(cx), "a\r\n1\r\n2\r\n3b");
        })
    });
}

#[gpui::test]
fn composition_is_cleared_by_navigation_and_external_edits(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("abc"));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
            view.execute_editing_command("editor.move-right", window, cx);
            assert_eq!(view.marked_range_utf16, None);
            view.replace_and_mark_text_in_range(None, "本", Some(1..1), window, cx);
            view.execute_editing_command("editor.select-all", window, cx);
            assert_eq!(view.marked_range_utf16, None);
            view.replace_and_mark_text_in_range(None, "日", Some(1..1), window, cx);
            view.select_reveal_and_focus(
                ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: "日".len(),
                },
                window,
                cx,
            );
            assert_eq!(view.marked_range_utf16, None);
            view.replace_and_mark_text_in_range(None, "語", Some(1..1), window, cx);
            assert!(view.marked_range_utf16.is_some());
        })
    });
    model.update(cx, |model, cx| {
        model.replace(0..0, "prefix").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(editor.read(cx).marked_range_utf16, None));
}

#[gpui::test]
fn editing_a_large_buffer_never_reads_the_whole_text(cx: &mut TestAppContext) {
    let text = (0..100_000)
        .map(|line| format!("line {line} with some words"))
        .collect::<Vec<_>>()
        .join("\n");
    let model = cx.new(|_| BufferModel::from_text(text));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    let (other, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    cx.refresh().unwrap();
    cx.update(|window, cx| {
        editor.update(cx, |view, cx| {
            view.focus.focus(window);
            view.restore_view_state(
                EditorViewState {
                    anchor: 50_000,
                    caret: 50_000,
                    top: 49_000,
                    scroll_x: 0.,
                },
                cx,
            );
            for command in [
                "editor.move-word-right",
                "editor.move-word-left",
                "editor.move-down",
                "editor.move-page-down",
                "editor.move-line-end",
                "editor.select-left",
                "editor.select-up",
                "editor.delete-backward",
                "editor.delete-forward",
                "editor.insert-newline",
                "editor.undo",
                "editor.redo",
            ] {
                assert!(
                    view.execute_editing_command(command, window, cx),
                    "{command}"
                );
            }
            view.replace_text_in_range(None, "typed", window, cx);
            view.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
            let selection = view.selected_text_range(false, window, cx).unwrap();
            view.text_for_range(selection.range, &mut None, window, cx);
            view.replace_text_in_range(None, "日", window, cx);
        })
    });
    cx.simulate_event(ScrollWheelEvent {
        position: cx.read(|cx| editor.read(cx).interaction_bounds().center()),
        delta: ScrollDelta::Pixels(point(px(-30.), px(-200.))),
        ..Default::default()
    });
    cx.refresh().unwrap();
    cx.run_until_parked();

    cx.read(|cx| {
        assert!(other.read(cx).paint_count > 0);
        assert_eq!(model.read(cx).whole_text_reads(), 0);
    });
}

#[gpui::test]
fn horizontal_scroll_is_bounded_by_the_widest_shaped_line(cx: &mut TestAppContext) {
    let text = format!("{}\nshort", "x".repeat(400));
    let model = cx.new(|_| BufferModel::from_text(text));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    cx.refresh().unwrap();
    cx.simulate_event(ScrollWheelEvent {
        position: cx.read(|cx| editor.read(cx).interaction_bounds().center()),
        delta: ScrollDelta::Pixels(point(px(-100_000.), px(0.))),
        ..Default::default()
    });
    let long_width = cx.read(|cx| {
        let view = editor.read(cx);
        let limit = view.max_line_width + 2. - f32::from(view.text_bounds().size.width);
        assert!(limit > 0.);
        assert_eq!(view.scroll_x, limit);
        view.max_line_width
    });

    model.update(cx, |model, cx| {
        model.replace(0..400, "x").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    // Text changes forget widths; the repaint measures the remaining lines.
    cx.read(|cx| assert!(editor.read(cx).max_line_width < long_width / 10.));
}

#[gpui::test]
fn constructs_from_arbitrary_buffer_text(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("generated\ntext"));
    let editor = cx.new(|cx| EditorView::new(model.clone(), cx));

    cx.read(|cx| {
        let editor = editor.read(cx);
        assert_eq!(editor.model(), &model);
        assert_eq!(editor.line_count, 2);
        assert!(editor.rendering.show_gutter_markers);
    });
}

#[gpui::test]
fn completion_acceptance_rechecks_the_captured_revision(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("fo"));
    let editor = cx.new(|cx| EditorView::new(model.clone(), cx));
    let mut registry = CompletionProviderRegistry::new();
    registry.register(
        ExtensionId::new(1),
        ExtensionLifecycleId::new(1),
        "fixture".into(),
    );

    let request = editor.update(cx, |editor, cx| {
        editor.cursor_col = 2;
        editor
            .start_completion(BufferHandle::new(1), registry.snapshot(), cx)
            .remove(0)
            .1
    });
    editor.update(cx, |editor, cx| {
        assert!(editor.apply_completion_response(
            request.buffer,
            CompletionResponse {
                registration: request.registration,
                revision: request.revision,
                generation: request.generation,
                result: Ok(vec![CompletionResultItem {
                    label: "foo".into(),
                    insert_text: "foo".into(),
                }]),
            },
            cx,
        ));
    });
    model.update(cx, |model, _| {
        assert!(model.replace(0..0, "x").unwrap());
    });
    editor.update(cx, |editor, cx| editor.accept_selected_completion(cx));

    cx.read(|cx| assert_eq!(model.read(cx).text(), "xfo"));

    let model = cx.new(|_| BufferModel::from_text("fo"));
    let editor = cx.new(|cx| EditorView::new(model.clone(), cx));
    let request = editor.update(cx, |editor, cx| {
        editor.cursor_col = 2;
        editor
            .start_completion(BufferHandle::new(2), registry.snapshot(), cx)
            .remove(0)
            .1
    });
    editor.update(cx, |editor, cx| {
        assert!(editor.apply_completion_response(
            request.buffer,
            CompletionResponse {
                registration: request.registration,
                revision: request.revision,
                generation: request.generation,
                result: Ok(vec![CompletionResultItem {
                    label: "foo".into(),
                    insert_text: "foo".into(),
                }]),
            },
            cx,
        ));
        editor.accept_selected_completion(cx);
    });
    cx.read(|cx| assert_eq!(model.read(cx).text(), "foo"));
}

#[gpui::test]
fn action_click_pointer_jitter_does_not_start_text_selection(cx: &mut TestAppContext) {
    let model = cx.new(|_| {
        let mut model = BufferModel::from_read_only_text("result");
        model
            .replace_contributions(
                ContributionSource::fixture(),
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 6,
                    },
                    decoration: None,
                    gutter: None,
                    command: Some("fixture.action".into()),
                }],
                0,
            )
            .unwrap();
        model
    });
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
    cx.refresh().unwrap();
    let click = cx.read(|cx| {
        let bounds = editor.read(cx).interaction_bounds();
        point(bounds.origin.x + px(20.), bounds.origin.y + px(10.))
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.on_mouse_down(
                &MouseDownEvent {
                    position: click,
                    modifiers: Modifiers::default(),
                    button: MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                },
                window,
                cx,
            );
            editor.on_mouse_move(
                &MouseMoveEvent {
                    position: point(click.x + px(3.), click.y),
                    modifiers: Modifiers::default(),
                    pressed_button: Some(MouseButton::Left),
                },
                window,
                cx,
            );
            assert_eq!(editor.selected_byte_range(cx), None);
        });
    });
}

#[gpui::test]
fn read_only_text_keeps_normal_selection_copy_and_scroll_behavior(cx: &mut TestAppContext) {
    let text = (0..100)
        .map(|line| format!("result {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let model = cx.new(|_| BufferModel::from_read_only_text(text.clone()));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    cx.refresh().unwrap();

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.select_reveal_and_focus(
                ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 8,
                },
                window,
                cx,
            );
            let selection = editor.selected_text_range(false, window, cx).unwrap();
            assert_eq!(selection.range, 0..8);
            assert_eq!(
                editor.text_for_range(selection.range, &mut None, window, cx),
                Some("result 0".into())
            );

            editor.replace_text_in_range(None, "changed", window, cx);
        });
    });
    cx.simulate_event(ScrollWheelEvent {
        position: cx.read(|cx| editor.read(cx).interaction_bounds().center()),
        delta: ScrollDelta::Lines(point(0., -5.)),
        ..Default::default()
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let editor = editor.read(cx);
        assert_eq!(editor.model().read(cx).text(), text);
        assert!(editor.scroll > 0.);
    });
}

#[test]
fn contribution_projection_indexes_multiline_ranges() {
    let model = BufferModel::from_text("aa\nbbbb\ncc");
    let (decorations, gutters, actions) = project_contributions(
        &model,
        vec![ResolvedEditorContribution {
            range: ByteRange {
                start_byte_offset: 1,
                end_byte_offset: 9,
            },
            source: ContributionSource::fixture(),
            decoration: Some(DecorationToken::Warning),
            gutter: Some(GutterToken::Warning),
            command: Some("fixture.action".into()),
        }],
    );

    assert_eq!(
        decorations
            .iter()
            .map(|decoration| (decoration.line, decoration.start, decoration.end))
            .collect::<Vec<_>>(),
        [(0, 1, 2), (1, 0, 4), (2, 0, 1)]
    );
    assert_eq!(gutters.len(), 1);
    assert_eq!(gutters[0].line, 0);
    assert!(actions.iter().all(|action| {
        action.range
            == ByteRange {
                start_byte_offset: 1,
                end_byte_offset: 9,
            }
    }));
    assert_eq!(
        actions
            .iter()
            .map(|action| (action.line, action.start, action.end, action.gutter))
            .collect::<Vec<_>>(),
        [(0, 1, 2, true), (1, 0, 4, false), (2, 0, 1, false)]
    );
}

#[gpui::test]
async fn flat_utf16_offsets_round_trip_across_emoji_lines(cx: &mut TestAppContext) {
    let family = "👨‍👩‍👧‍👦";
    let first_line = "a😀b";
    let second_line = format!("{family}z");
    let model = cx.new(|_| BufferModel::from_text(format!("{first_line}\n{second_line}")));
    let editor = cx.new(|cx| EditorView::new(model, cx));

    cx.read(|cx| {
        let model = editor.read(cx).model.read(cx);
        let second_line_start = 5;
        let family_utf16_len = family.chars().map(char::len_utf16).sum::<usize>();

        assert_eq!(flat_utf16_to_position(model, 1), (0, 1));
        assert_eq!(flat_utf16_to_position(model, 3), (0, "a😀".len()));
        assert_eq!(flat_utf16_to_position(model, second_line_start), (1, 0));
        assert_eq!(
            flat_utf16_to_position(model, second_line_start + family_utf16_len),
            (1, family.len())
        );

        for (line, byte_col) in [
            (0, 0),
            (0, 1),
            (0, "a😀".len()),
            (0, first_line.len()),
            (1, 0),
            (1, family.len()),
            (1, second_line.len()),
        ] {
            let utf16 = to_flat_utf16(model, line, byte_col);
            assert_eq!(flat_utf16_to_position(model, utf16), (line, byte_col));
        }
    });
}

#[gpui::test]
fn view_state_round_trips_and_clamps_to_shorter_multibyte_text(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("one\ntwo\nthree\nfour"));
    let editor = cx.new(|cx| EditorView::new(model.clone(), cx));
    let state = EditorViewState {
        anchor: 4,
        caret: 9,
        top: 8,
        scroll_x: 14.,
    };
    editor.update(cx, |editor, cx| editor.restore_view_state(state, cx));
    cx.read(|cx| {
        let editor = editor.read(cx);
        assert_eq!(editor.view_state(cx), state);
        assert_eq!(editor.selected_byte_range(cx), Some(4..9));
        assert_eq!(editor.scroll, 2. * LINE_HEIGHT);
    });

    // Offsets past the end clamp, and offsets inside a scalar snap back.
    let shorter = cx.new(|_| BufferModel::from_text("é😀\nb"));
    let editor = cx.new(|cx| EditorView::new(shorter, cx));
    editor.update(cx, |editor, cx| {
        editor.restore_view_state(
            EditorViewState {
                anchor: 3,
                caret: 400,
                top: 400,
                scroll_x: -5.,
            },
            cx,
        )
    });
    cx.read(|cx| {
        assert_eq!(
            editor.read(cx).view_state(cx),
            EditorViewState {
                anchor: 2,
                caret: "é😀\nb".len(),
                top: "é😀\n".len(),
                scroll_x: 0.,
            }
        );
    });
}

#[gpui::test]
fn views_share_model_state_and_keep_presentation_state_independent(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("one\ntwo\nthree"));
    let first = cx.new(|cx| EditorView::new(model.clone(), cx));
    let second = cx.new(|cx| {
        EditorView::new_with_options(
            model.clone(),
            1,
            EditorRenderingOptions {
                show_gutter_markers: false,
            },
            cx,
        )
    });

    first.update(cx, |view, cx| {
        view.cursor_line = 1;
        view.cursor_col = 2;
        view.anchor_line = 0;
        view.anchor_col = 1;
        view.has_selection = true;
        view.scroll = 20.;
        view.sync_view_position(cx);
    });
    second.update(cx, |view, cx| {
        view.cursor_line = 2;
        view.cursor_col = 3;
        view.scroll = 40.;
        view.sync_view_position(cx);
    });

    model.update(cx, |model, cx| {
        assert!(model.replace(0..0, "shared\n").unwrap());
        model
            .replace_contributions(
                ContributionSource::fixture(),
                &[EditorContribution {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 6,
                    },
                    decoration: Some(DecorationToken::Warning),
                    gutter: Some(GutterToken::Warning),
                    command: None,
                }],
                model.revision(),
            )
            .unwrap();
        cx.notify();
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let first = first.read(cx);
        let second = second.read(cx);

        assert_eq!(first.model, second.model);
        assert_eq!(first.text(cx), "shared\none\ntwo\nthree");
        assert_eq!(first.decorations.len(), 1);
        assert_eq!(first.decorations.len(), second.decorations.len());
        assert_eq!(first.gutter_markers.len(), second.gutter_markers.len());

        assert_eq!((first.cursor_line, first.cursor_col), (2, 2));
        assert_eq!((second.cursor_line, second.cursor_col), (3, 3));
        assert!(first.has_selection);
        assert!(!second.has_selection);
        assert_eq!(first.scroll, 20.);
        assert_eq!(second.scroll, 40.);
        assert!(first.rendering.show_gutter_markers);
        assert!(!second.rendering.show_gutter_markers);
        assert_ne!(first.focus, second.focus);
    });
}

#[gpui::test]
fn selection_tracks_a_deletion_from_another_view(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("0123456789"));
    let first = cx.new(|cx| EditorView::new(model.clone(), cx));
    let second = cx.new(|cx| EditorView::new(model.clone(), cx));

    second.update(cx, |view, cx| {
        view.anchor_col = 4;
        view.cursor_col = 7;
        view.has_selection = true;
        view.sync_view_position(cx);
    });
    model.update(cx, |model, cx| {
        assert!(model.replace(1..2, "").unwrap());
        cx.notify();
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let first = first.read(cx);
        let second = second.read(cx);
        assert_eq!(first.text(cx), "023456789");
        assert_eq!((second.anchor_line, second.anchor_col), (0, 3));
        assert_eq!((second.cursor_line, second.cursor_col), (0, 6));
        assert!(second.has_selection);
    });
}

fn span(range: std::ops::Range<usize>, name: &str) -> HighlightSpan {
    HighlightSpan {
        range,
        highlight: Highlight::named(name),
    }
}

/// Each run as `(len, color, underline color)`.
fn run_styles(runs: &[gpui::TextRun]) -> Vec<(usize, gpui::Hsla, Option<gpui::Hsla>)> {
    runs.iter()
        .map(|run| {
            let underline = run.underline.and_then(|underline| underline.color);
            (run.len, run.color, underline)
        })
        .collect()
}

#[test]
fn layered_runs_apply_layers_in_order_and_fill_gaps() {
    let theme = Theme::dark();
    let color = |name| theme.style(Highlight::named(name)).color.unwrap();
    let error = Some(theme.underline_color(Highlight::named("diagnostic.error")));
    let fg = theme.foreground;
    let text = "let x = \"é\";";
    let syntax = [
        span(0..3, "keyword"),
        span(4..5, "variable"),
        span(8..12, "string"),
    ];
    let semantic = [span(0..3, "type")];
    let decorations = [span(6..11, "diagnostic.error")];

    let runs = runs_for(text, &[&syntax, &semantic, &decorations], &theme);
    assert_eq!(
        run_styles(&runs),
        [
            (3, color("type"), None),
            (1, fg, None),
            (1, color("variable"), None),
            (1, fg, None),
            (2, fg, error),
            (3, color("string"), error),
            (1, color("string"), None),
            (1, fg, None),
        ]
    );
    assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
}

#[test]
fn layered_runs_merge_equal_styles_and_clip_to_the_line() {
    let theme = Theme::dark();
    let keyword = theme.style(Highlight::named("keyword")).color.unwrap();
    let layer = [
        span(0..2, "keyword"),
        span(2..4, "keyword"),
        span(5..99, "keyword"),
    ];
    assert_eq!(
        run_styles(&runs_for("abcd e", &[&layer], &theme)),
        [
            (4, keyword, None),
            (1, theme.foreground, None),
            (1, keyword, None)
        ]
    );
    assert!(runs_for("", &[&layer], &theme).is_empty());
    assert_eq!(
        run_styles(&runs_for("plain", &[], &theme)),
        [(5, theme.foreground, None)]
    );

    let comment = [span(0..5, "comment")];
    let runs = runs_for("plain", &[&comment], &theme);
    assert_eq!(runs[0].font.style, gpui::FontStyle::Italic);
}

#[gpui::test]
fn layered_runs_keep_tab_and_rtl_geometry(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text(""));
    let (_, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
    let theme = Theme::dark();
    cx.update(|window, _| {
        for text in ["\tfoo\tbar(baz)\t", "مرحبا بالعالم", "\tשלום world"] {
            let layer: Vec<HighlightSpan> = text
                .char_indices()
                .map(|(start, ch)| span(start..start + ch.len_utf8(), "string"))
                .collect();
            let plain =
                shape_editor_line(window, text.into(), px(14.), &runs_for(text, &[], &theme));
            let layered = shape_editor_line(
                window,
                text.into(),
                px(14.),
                &runs_for(text, &[&layer], &theme),
            );
            assert_eq!(layered.width, plain.width, "{text:?}");
            for (index, _) in text.char_indices() {
                assert_eq!(
                    x_for_index_dir(&layered, index, text),
                    x_for_index_dir(&plain, index, text),
                    "{text:?} at {index}"
                );
            }
        }
    });
}

#[gpui::test]
fn overlapping_decorations_underline_with_the_most_severe(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("abcdefgh"));
    let editor = cx.new(|cx| EditorView::new(model.clone(), cx));
    let decoration = |range: std::ops::Range<usize>, token| EditorContribution {
        range: ByteRange {
            start_byte_offset: range.start,
            end_byte_offset: range.end,
        },
        decoration: Some(token),
        gutter: None,
        command: None,
    };
    model.update(cx, |model, cx| {
        model
            .replace_contributions(
                ContributionSource::fixture(),
                &[
                    decoration(0..4, DecorationToken::Warning),
                    decoration(1..3, DecorationToken::Warning),
                    decoration(2..6, DecorationToken::Error),
                ],
                model.revision(),
            )
            .unwrap();
        cx.notify();
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let theme = cx.global::<Theme>();
        let underline = |name| Some(theme.underline_color(Highlight::named(name)));
        let fg = theme.foreground;
        assert_eq!(
            run_styles(&editor.read(cx).line_runs(model.read(cx), 0, "abcdefgh", cx)),
            [
                (2, fg, underline("diagnostic.warning")),
                (4, fg, underline("diagnostic.error")),
                (2, fg, None),
            ]
        );
    });
}

#[gpui::test]
fn gutter_markers_reserve_a_column_beside_the_text(cx: &mut TestAppContext) {
    for (show_gutter_markers, gutter) in [(true, 16.), (false, 0.)] {
        let model = cx.new(|_| BufferModel::from_text("abc"));
        let (editor, cx) = cx.add_window_view(|_, cx| {
            EditorView::new_with_options(
                model,
                0,
                EditorRenderingOptions {
                    show_gutter_markers,
                },
                cx,
            )
        });
        cx.refresh().unwrap();
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                let bounds = view.interaction_bounds();
                let text = view.text_bounds();
                assert_eq!(text.origin.x - bounds.origin.x, px(gutter));
                assert_eq!(text.size.width, bounds.size.width - px(gutter));
                view.cursor_col = 1;
                let x = view.caret_x(view.model.read(cx), window, cx);
                let y = bounds.origin.y + px(5.);
                let model = view.model.read(cx);
                assert_eq!(
                    view.hit_test(model, point(text.origin.x + x, y), window, cx),
                    (0, 1)
                );
                assert_eq!(
                    view.hit_test(model, point(bounds.origin.x, y), window, cx),
                    (0, 0)
                );
            })
        });
    }
}

#[gpui::test]
fn syntax_highlights_paint_beneath_decorations(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("fn ab() {}\nlet c;"));
    let editor = cx.new(|cx| EditorView::new(model.clone(), cx));
    model.update(cx, |model, cx| {
        let highlights = "\"fn\" @keyword\n(function_item name: (identifier) @function)\n";
        let language = SyntaxLanguage::new("rust", highlights, "").unwrap();
        model.set_syntax(
            Some(SyntaxState::new(Arc::new(language), Arc::new(|_| None))),
            cx,
        );
        let warning = EditorContribution {
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 5,
            },
            decoration: Some(DecorationToken::Warning),
            gutter: None,
            command: None,
        };
        model
            .replace_contributions(ContributionSource::fixture(), &[warning], model.revision())
            .unwrap();
        cx.notify();
    });
    cx.run_until_parked();

    let runs = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            let view = editor.read(cx);
            let model = model.read(cx);
            view.cache_syntax(model, 0..model.line_count());
            run_styles(&view.line_runs(model, 0, &model.read(model.line_range(0)), cx))
        })
    };
    let (keyword, function, warning, fg) = cx.read(|cx| {
        let theme = cx.global::<Theme>();
        let color = |name| theme.style(Highlight::named(name)).color.unwrap();
        (
            color("keyword"),
            color("function"),
            Some(theme.underline_color(Highlight::named("diagnostic.warning"))),
            theme.foreground,
        )
    });
    assert_eq!(
        runs(cx),
        [
            (2, keyword, warning),
            (1, fg, warning),
            (2, function, warning),
            (5, fg, None),
        ]
    );

    // An edit changes the syntax generation, so the cached visible range is
    // not reused for the new text. The anchored warning grows with it.
    model.update(cx, |model, cx| {
        model.replace(3..5, "abcd").unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        runs(cx),
        [
            (2, keyword, warning),
            (1, fg, warning),
            (4, function, warning),
            (5, fg, None),
        ]
    );
}

#[gpui::test]
fn caret_geometry_and_painting_share_one_syntax_query(cx: &mut TestAppContext) {
    let model = cx.new(|_| BufferModel::from_text("fn f() {}\n".repeat(200)));
    let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
    model.update(cx, |model, cx| {
        let language = SyntaxLanguage::new("rust", "\"fn\" @keyword\n", "").unwrap();
        model.set_syntax(
            Some(SyntaxState::new(Arc::new(language), Arc::new(|_| None))),
            cx,
        );
    });
    cx.run_until_parked();
    let queries = |cx: &mut gpui::VisualTestContext| cx.read(|cx| model.read(cx).syntax_queries());

    // An edit invalidates the cache; caret geometry before the next paint
    // queries the visible lines, and painting reuses that query.
    let before = queries(cx);
    cx.update(|window, cx| {
        model.update(cx, |model, cx| {
            model.replace(0..0, "// ").unwrap();
            cx.notify();
        });
        editor.update(cx, |view, cx| {
            view.caret_x(view.model.read(cx), window, cx);
        })
    });
    cx.refresh().unwrap();
    cx.run_until_parked();
    assert_eq!(queries(cx), before + 1);

    // Scrolling by a line stays within the overscan.
    editor.update(cx, |view, cx| {
        view.scroll += LINE_HEIGHT;
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(queries(cx), before + 1);
}
