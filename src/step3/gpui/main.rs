// Step 3 — gpui 3-pane shell with a placeholder editor.
//
// Layout-reach spike (roadmap item 6): confirm a 3-pane resizable shell —
// left panel, editor center, right panel — is buildable on gpui 0.2.2's
// *public* layout + drag API, as an extension author would. The editor
// center is a placeholder; the editor widget itself is step 4.
//
// No-internal-path scorecard (preliminary):
//   - flex/column layout:        div().flex().flex_row()      ✓ pub (Styled)
//   - fixed/relative sizing:     w(px(..)), flex_1()           ✓ pub (Styled)
//   - selectable single-row list: uniform_list                 ✓ pub
//   - row click → highlight:      div().id(..).on_click(...)   ✓ pub (StatefulInteractiveElement)
//   - hover styling:              .hover(|s| s.bg(..))         ✓ pub (InteractiveElement)
//   - conditional styling:       .when(cond, |d| d..)         ✓ pub (FluentBuilder, all IntoElement)
//   - resize divider:             custom (no built-in splitter) ⚠ built from on_drag + on_drag_move + on_drop
//
// All public, no pub(crate)/fork/backdoor. The splitter being absent from
// the framework is itself a (cosmetic) finding recorded in the plan.

use gpui::{prelude::FluentBuilder, *};

actions!(knot, [Quit]);

const MIN_PANE: f32 = 120.;

/// Drag value carried by the active drag while a pane divider is being dragged.
/// `which` identifies which divider (0 = left/center, 1 = center/right).
#[derive(Clone, Copy)]
struct DividerDrag {
    which: usize,
}

/// Invisible drag-ghost view gpui renders while the drag is in progress.
/// Required by `on_drag`'s constructor; we don't want a visible ghost.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

struct Shell {
    left_files: Vec<SharedString>,
    left_selected: Option<usize>,
    right_outline: Vec<SharedString>,
    right_selected: Option<usize>,
    left_width: f32,
    right_width: f32,
    /// `(which, start_x, start_width)` captured on the first drag-move event
    /// of an in-progress divider drag; cleared on drop.
    drag_origin: Option<(usize, Pixels, f32)>,
}

impl Shell {
    fn new() -> Self {
        Shell {
            left_files: vec![
                "src/lib.rs".into(),
                "src/main.rs".into(),
                "src/step3/gpui/main.rs".into(),
                "docs/roadmap.md".into(),
                "docs/design.md".into(),
                "docs/step3-framework-comparison.md".into(),
                "Cargo.toml".into(),
                "AGENTS.md".into(),
            ],
            left_selected: Some(2),
            right_outline: vec![
                "struct Shell".into(),
                "  left_files".into(),
                "  left_selected".into(),
                "  right_outline".into(),
                "  right_selected".into(),
                "  left_width".into(),
                "  right_width".into(),
                "  drag_origin".into(),
                "impl Render".into(),
                "  render()".into(),
                "fn main()".into(),
            ],
            right_selected: None,
            left_width: 260.,
            right_width: 220.,
            drag_origin: None,
        }
    }

    /// Build a single selectable row: a div wrapping `label`; clicking selects it,
    /// the selected row gets a highlight bg, all rows get a hover bg.
    fn row(
        id_prefix: &'static str,
        ix: usize,
        label: SharedString,
        selected: Option<usize>,
        entity: Entity<Shell>,
        which_pane: usize,
    ) -> AnyElement {
        let is_selected = selected == Some(ix);
        div()
            .id((id_prefix, ix))
            .w_full()
            .px_2()
            .py_1()
            .text_sm()
            .child(label)
            .when(is_selected, |d| {
                d.bg(rgb(0x2a4a7a)).text_color(rgb(0xffffff))
            })
            .hover(|s| s.bg(rgb(0x222222)))
            .on_click(move |_ev, _window, cx| {
                entity.update(cx, |s, cx| {
                    if which_pane == 0 {
                        s.left_selected = Some(ix);
                    } else {
                        s.right_selected = Some(ix);
                    }
                    cx.notify();
                });
            })
            .into_any_element()
    }

    /// A pane that hosts a selectable single-row list.
    fn pane(
        &self,
        id_prefix: &'static str,
        items: &[SharedString],
        selected: Option<usize>,
        entity: Entity<Shell>,
        which_pane: usize,
    ) -> impl IntoElement {
        let items: Vec<SharedString> = items.to_vec();
        uniform_list(
            id_prefix,
            items.len(),
            move |range, _window, _cx| {
                range
                    .map(|ix| {
                        Self::row(
                            id_prefix,
                            ix,
                            items[ix].clone(),
                            selected,
                            entity.clone(),
                            which_pane,
                        )
                    })
                    .collect()
            },
        )
        .h_full()
    }

    /// A draggable pane-divider handle. The handle itself does no resize math;
    /// the root listens for `DividerDrag` drag-move/drop events.
    fn divider(which: usize) -> impl IntoElement {
        div()
            .id(("divider", which))
            .w(px(6.))
            .h_full()
            .bg(rgb(0x333333))
            .cursor_ew_resize()
            .on_drag(DividerDrag { which }, |_value, _offset, _window, cx| {
                cx.new(|_| DragGhost)
            })
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let left_selected = self.left_selected;
        let right_selected = self.right_selected;
        let left_files = self.left_files.clone();
        let right_outline = self.right_outline.clone();

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xd4d4d4))
            // Drag resize listeners on the root: on_drag_move fires (capture phase)
            // for every move while a DividerDrag is active, regardless of mouse
            // position, so the root — not the handle — owns the resize math.
.on_drag_move::<DividerDrag>(cx.listener(
                |this, event: &DragMoveEvent<DividerDrag>, _window, cx| {
                    let which = event.drag(cx).which;
                    let pos_x = event.event.position.x;
                    let (_, start_x, start_w) = match this.drag_origin {
                        Some(o) => o,
                        None => {
                            let sw = if which == 0 {
                                this.left_width
                            } else {
                                this.right_width
                            };
                            let o = (which, pos_x, sw);
                            this.drag_origin = Some(o);
                            o
                        }
                    };
let delta = f32::from(pos_x - start_x);
                let new_w = if which == 0 {
                    start_w + delta
                } else {
                    start_w - delta
                }
                .max(MIN_PANE);
                    if which == 0 {
                        this.left_width = new_w;
                    } else {
                        this.right_width = new_w;
                    }
                    cx.notify();
                },
            ))
            .on_drop::<DividerDrag>(cx.listener(|this, _value: &DividerDrag, _window, cx| {
                this.drag_origin = None;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .h_full()
                    .w(px(self.left_width))
                    .bg(rgb(0x252526))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .child("EXPLORER"),
                    )
                    .child(self.pane(
                        "left",
                        &left_files,
                        left_selected,
                        entity.clone(),
                        0,
                    )),
            )
            .child(Self::divider(0))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .bg(rgb(0x1e1e1e))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .h_full()
                            .w_full()
                            .items_center()
                            .justify_center()
                            .text_color(rgb(0x666666))
                            .child("editor placeholder"),
                    ),
            )
            .child(Self::divider(1))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .h_full()
                    .w(px(self.right_width))
                    .bg(rgb(0x252526))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .child("OUTLINE"),
                    )
                    .child(self.pane(
                        "right",
                        &right_outline,
                        right_selected,
                        entity,
                        1,
                    )),
            )
    }
}

fn main() {
    Application::new().run(|app: &mut App| {
        app.on_action(|_action: &Quit, app: &mut App| app.quit());

        app.key_bindings().borrow_mut().add_bindings([
            KeyBinding::new("cmd-q", Quit, None),
        ]);

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), app);
        app.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|_| Shell::new()),
        )
        .unwrap();

        app.set_menus(vec![Menu {
            name: "Knot".into(),
            items: vec![MenuItem::action("Quit Knot", Quit)],
        }]);
    });
}