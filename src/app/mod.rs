//! Knot's gpui application shell.
//!
//! The shell contains resizable explorer, editor, and outline panes plus a
//! status area for the active buffer and extension runtime.

use gpui::{prelude::FluentBuilder, *};
use std::time::Duration;

use crate::host::{
    ExtensionRequestInbox, ExtensionRuntimeControl, ExtensionRuntimeParts, ExtensionRuntimeThread,
    V8Host,
    protocol::{
        ExtensionId, HostOperation, HostRequest, HostRequestError, HostResponse, HostResponseValue,
    },
};

mod editor;
pub mod model;

use editor::EditorView;
use model::{BufferModel, BufferRegistry};

actions!(knot, [Quit]);

const MIN_PANE: f32 = 120.;
const RUNTIME_PROBE_SOURCE: &str = r#"
import { editor } from "knot:editor";

globalThis.knotActiveBuffer = await editor.activeBuffer();
if (!globalThis.knotActiveBuffer) {
  throw new Error("Knot has no active editor buffer");
}
"#;

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
    /// Owned as an `Entity` so the custom editor element can read its state
    /// during each paint.
    editor: Entity<EditorView>,
    buffer_registry: BufferRegistry,
    heartbeat: u64,
    runtime_state: SharedString,
    latest_runtime_error: Option<SharedString>,
    runtime_control: ExtensionRuntimeControl,
    runtime_thread: Option<ExtensionRuntimeThread>,
    background_executor: BackgroundExecutor,
    _runtime_bridge_task: Task<()>,
    _runtime_execution_task: Task<()>,
    _model_subscription: Subscription,
    _heartbeat_task: Task<()>,
    /// Name of the fixture currently loaded (shown in a thin status header
    /// above the editor so it's visible at a glance which fixture is running).
    fixture_name: String,
}

impl Shell {
    /// Construct the shell, preloading the editor with a fixture chosen
    /// from `argv[1]` (default `rust_sample`) so the editor pane has real
    /// styled text to render. Fixture resolution is relative to the crate
    /// root so the binary runs from any cwd.
    fn new(runtime: ExtensionRuntimeParts, cx: &mut Context<Self>) -> Self {
        let fixture_name = std::env::args()
            .nth(1)
            .unwrap_or_else(|| "rust_sample".into());
        let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("bench/fixtures")
            .join(format!("{fixture_name}.kfx"));
        let fixture = crate::view::fixture::Fixture::load(&fixture_path).unwrap_or_else(|e| {
            eprintln!("[knot] failed to load fixture {fixture_path:?}: {e}");
            crate::view::fixture::Fixture::from_lines(vec![format!(
                "(no fixture at {fixture_path:?}: {e})"
            )])
        });
        let model = cx.new(|_| BufferModel::from_text(fixture.lines.join("\n")));
        let mut buffer_registry = BufferRegistry::new();
        let handle = buffer_registry.open(&model);
        buffer_registry.set_active(Some(handle));
        let editor = cx.new(|cx| EditorView::from_fixture(&fixture, model.clone(), cx));
        let model_subscription = cx.observe(&model, |_this, _model, cx| cx.notify());
        let ExtensionRuntimeParts {
            control: runtime_control,
            requests,
            thread: runtime_thread,
        } = runtime;
        let runtime_bridge_task = Self::spawn_runtime_bridge(requests, runtime_control.clone(), cx);
        let runtime_execution = runtime_control.execute_fixture_module(
            "file:///fixtures/gpui-runtime-probe.js",
            RUNTIME_PROBE_SOURCE,
        );
        let runtime_execution_task = cx.spawn(async move |this, cx| {
            let result = runtime_execution.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.runtime_state = "running".into();
                        this.latest_runtime_error = None;
                    }
                    Err(error) => {
                        this.runtime_state = "failed".into();
                        this.latest_runtime_error = Some(format!("{error:?}").into());
                    }
                }
                cx.notify();
            });
        });
        let heartbeat_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.heartbeat = this.heartbeat.wrapping_add(1);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Shell {
            left_files: vec![
                "src/lib.rs".into(),
                "src/main.rs".into(),
                "src/app/mod.rs".into(),
                "src/app/editor.rs".into(),
                "src/app/model.rs".into(),
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
            editor,
            buffer_registry,
            heartbeat: 0,
            runtime_state: "starting".into(),
            latest_runtime_error: None,
            runtime_control,
            runtime_thread: Some(runtime_thread),
            background_executor: cx.background_executor().clone(),
            _runtime_bridge_task: runtime_bridge_task,
            _runtime_execution_task: runtime_execution_task,
            _model_subscription: model_subscription,
            _heartbeat_task: heartbeat_task,
            fixture_name,
        }
    }

    fn spawn_runtime_bridge(
        mut requests: ExtensionRequestInbox,
        control: ExtensionRuntimeControl,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut failure = None;
            while let Some(request) = requests.receive().await {
                let response =
                    match this.update(cx, |this, _cx| this.dispatch_host_request(request)) {
                        Ok(response) => response,
                        Err(_) => return,
                    };
                if let Err(error) = control.respond(response) {
                    failure = Some(format!("runtime response failed: {error:?}"));
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.runtime_state = "closed".into();
                if let Some(error) = failure {
                    this.latest_runtime_error = Some(error.into());
                }
                cx.notify();
            });
        })
    }

    fn dispatch_host_request(&mut self, request: HostRequest) -> HostResponse {
        let result = match request.operation {
            HostOperation::ActiveBuffer => Ok(HostResponseValue::ActiveBuffer(
                self.buffer_registry.active_handle(),
            )),
            HostOperation::Snapshot { .. } | HostOperation::ApplyEdits { .. } => {
                Err(HostRequestError::UnsupportedOperation)
            }
        };

        HostResponse {
            extension: request.extension,
            id: request.id,
            result,
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
        uniform_list(id_prefix, items.len(), move |range, _window, _cx| {
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
        })
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

impl Drop for Shell {
    fn drop(&mut self) {
        self.runtime_control.request_shutdown();
        if let Some(thread) = self.runtime_thread.take() {
            self.background_executor
                .spawn(async move { thread.shutdown() })
                .detach();
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let left_selected = self.left_selected;
        let right_selected = self.right_selected;
        let left_files = self.left_files.clone();
        let right_outline = self.right_outline.clone();
        let revision = self
            .buffer_registry
            .active_handle()
            .and_then(|handle| self.buffer_registry.resolve(handle).ok())
            .map(|model| model.read(cx).revision().to_string())
            .unwrap_or_else(|| "closed".into());
        let runtime_error = self
            .latest_runtime_error
            .clone()
            .unwrap_or_else(|| "none".into());

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
                    .child(self.pane("left", &left_files, left_selected, entity.clone(), 0)),
            )
            .child(Self::divider(0))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(rgb(0x1e1e1e))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x888888))
                            .bg(rgb(0x252526))
                            .child(self.fixture_name.clone()),
                    )
                    .child(div().flex_1().child(self.editor.clone()))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_4()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(0x9d9d9d))
                            .bg(rgb(0x252526))
                            .child(format!("revision {revision}"))
                            .child(format!("runtime {}", self.runtime_state))
                            .child(format!("error {runtime_error}"))
                            .child(format!("heartbeat {}", self.heartbeat)),
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
                    .child(self.pane("right", &right_outline, right_selected, entity, 1)),
            )
    }
}

pub fn run() {
    let runtime = V8Host::new()
        .spawn_extension(ExtensionId::new(1))
        .into_parts();

    Application::new().run(move |app: &mut App| {
        app.on_action(|_action: &Quit, app: &mut App| app.quit());

        app.key_bindings()
            .borrow_mut()
            .add_bindings([KeyBinding::new("cmd-q", Quit, None)]);

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), app);
        app.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| Shell::new(runtime, cx)),
        )
        .unwrap();

        app.set_menus(vec![Menu {
            name: "Knot".into(),
            items: vec![MenuItem::action("Quit Knot", Quit)],
        }]);
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{AppContext, Entity, TestAppContext};

    use super::Shell;
    use crate::host::{V8Host, protocol::ExtensionId};

    async fn wait_for_runtime_state(
        shell: &Entity<Shell>,
        expected: &str,
        cx: &mut TestAppContext,
    ) {
        while cx.read(|cx| shell.read(cx).runtime_state != expected) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
    }

    #[gpui::test]
    async fn foreground_bridge_resolves_the_displayed_active_buffer(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(7))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = cx.read(|cx| shell.read(cx).runtime_control.clone());
        let execution = control.execute_fixture_script(
            "verify-gpui-active-buffer.js",
            r#"
                if (!globalThis.knotActiveBuffer) {
                    throw new Error("gpui did not return its active buffer");
                }
            "#,
        );

        execution.await.unwrap();

        let (resolved, displayed) = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            let resolved = shell.buffer_registry.resolve(handle).unwrap();
            let displayed = shell.editor.read(cx).model().clone();
            (resolved, displayed)
        });
        assert_eq!(resolved, displayed);
    }

    #[gpui::test]
    async fn heartbeat_progresses_before_and_after_the_runtime_bridge_closes(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(8))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));

        wait_for_runtime_state(&shell, "running", cx).await;
        let first = cx.read(|cx| shell.read(cx).heartbeat);
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        let second = cx.read(|cx| shell.read(cx).heartbeat);
        assert!(second > first);

        let control = cx.read(|cx| shell.read(cx).runtime_control.clone());
        control.request_shutdown();
        wait_for_runtime_state(&shell, "closed", cx).await;

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        let third = cx.read(|cx| shell.read(cx).heartbeat);
        assert!(third > second);
    }
}
