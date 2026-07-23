//! Knot's gpui application shell.
//!
//! The shell contains resizable explorer, editor, and outline panes plus a
//! status area for the active buffer and extension runtime.

use gpui::{prelude::FluentBuilder, *};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use crate::host::{
    ExtensionRequestInbox, ExtensionRuntimeControl, ExtensionRuntimeParts, ExtensionRuntimeThread,
    V8Host,
    protocol::{
        BufferChange, CommandInvocation, CommandInvocationId, ExtensionId, HostOperation,
        HostRequest, HostRequestError, HostResponse, HostResponseValue,
    },
};

mod editor;
pub mod model;

use editor::EditorView;
use model::{
    BufferAccessError, BufferModel, BufferRegistry, BufferSubscriptionRegistry, CommandRegistry,
};

actions!(knot, [Quit]);

const MIN_PANE: f32 = 120.;
const RUNTIME_PROBE_SOURCE: &str = r#"
import { commands, editor } from "knot:editor";

globalThis.knotActiveBuffer = await editor.activeBuffer();
await commands.register("knot.fixture.edit", async (context) => {
  if (!context.buffer) throw new Error("Knot has no active editor buffer");
  context.signal.addEventListener("abort", () => {
    globalThis.knotFixtureCommandAborted = true;
  });
  const snapshot = await context.buffer.snapshot();
  await context.buffer.applyEdits(
    [{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "// command\n" }],
    { ifRevision: snapshot.revision },
  );
});
"#;

fn map_buffer_error(error: BufferAccessError) -> HostRequestError {
    match error {
        BufferAccessError::Closed => HostRequestError::BufferClosed,
        BufferAccessError::InvalidRange => HostRequestError::InvalidRange,
        BufferAccessError::InvalidEditBatch => HostRequestError::InvalidEditBatch,
        BufferAccessError::RevisionConflict => HostRequestError::RevisionConflict,
    }
}

fn map_command_error(error: model::CommandRegistryError) -> HostRequestError {
    match error {
        model::CommandRegistryError::NameInUse => HostRequestError::CommandNameInUse,
        model::CommandRegistryError::NotFound => HostRequestError::CommandNotFound,
    }
}

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
    buffer_subscriptions: BufferSubscriptionRegistry,
    command_registry: CommandRegistry,
    next_command_invocation: u64,
    active_command: Option<CommandInvocationId>,
    cancelled_commands: HashSet<CommandInvocationId>,
    command_state: SharedString,
    heartbeat: u64,
    runtime_state: SharedString,
    latest_runtime_error: Option<SharedString>,
    extension_controls: HashMap<
        (ExtensionId, crate::host::protocol::ExtensionLifecycleId),
        ExtensionRuntimeControl,
    >,
    runtime_threads: Vec<ExtensionRuntimeThread>,
    background_executor: BackgroundExecutor,
    _runtime_bridge_tasks: Vec<Task<()>>,
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
        Self::new_with_runtimes(vec![runtime], cx)
    }

    fn new_with_runtimes(runtimes: Vec<ExtensionRuntimeParts>, cx: &mut Context<Self>) -> Self {
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
        let model_subscription = cx.observe(&model, |this, model, cx| {
            this.publish_model_change(model, cx);
            cx.notify();
        });
        let mut runtimes = runtimes.into_iter();
        let ExtensionRuntimeParts {
            control: runtime_control,
            requests,
            thread: runtime_thread,
        } = runtimes
            .next()
            .expect("Knot shell requires one extension runtime");
        let mut extension_controls = HashMap::new();
        extension_controls.insert(runtime_control.identity(), runtime_control.clone());
        let mut runtime_bridge_tasks = vec![Self::spawn_runtime_bridge(
            requests,
            runtime_control.clone(),
            cx,
        )];
        let mut runtime_threads = vec![runtime_thread];
        for ExtensionRuntimeParts {
            control,
            requests,
            thread,
        } in runtimes
        {
            extension_controls.insert(control.identity(), control.clone());
            runtime_bridge_tasks.push(Self::spawn_runtime_bridge(requests, control, cx));
            runtime_threads.push(thread);
        }
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
            buffer_subscriptions: BufferSubscriptionRegistry::new(),
            command_registry: CommandRegistry::new(),
            next_command_invocation: 1,
            active_command: None,
            cancelled_commands: HashSet::new(),
            command_state: "idle".into(),
            heartbeat: 0,
            runtime_state: "starting".into(),
            latest_runtime_error: None,
            extension_controls,
            runtime_threads,
            background_executor: cx.background_executor().clone(),
            _runtime_bridge_tasks: runtime_bridge_tasks,
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
                    match this.update(cx, |this, cx| this.dispatch_host_request(request, cx)) {
                        Ok(response) => response,
                        Err(_) => return,
                    };
                if let Err(error) = control.respond(response) {
                    failure = Some(format!("runtime response failed: {error:?}"));
                    break;
                }
            }
            let (extension, lifecycle) = control.identity();
            let _ = this.update(cx, |this, cx| {
                this.command_registry.remove_lifecycle(extension, lifecycle);
                this.buffer_subscriptions
                    .remove_lifecycle(extension, lifecycle);
                this.runtime_state = "closed".into();
                if let Some(error) = failure {
                    this.latest_runtime_error = Some(error.into());
                }
                cx.notify();
            });
        })
    }

    fn dispatch_host_request(
        &mut self,
        request: HostRequest,
        cx: &mut Context<Self>,
    ) -> HostResponse {
        let cancelled = request
            .invocation
            .is_some_and(|invocation| self.cancelled_commands.contains(&invocation));
        let result = match request.operation {
            _ if cancelled => Err(HostRequestError::Cancelled),
            HostOperation::ActiveBuffer => Ok(HostResponseValue::ActiveBuffer(
                self.buffer_registry.active_handle(),
            )),
            HostOperation::Snapshot { buffer, range } => self
                .buffer_registry
                .resolve(buffer)
                .map_err(|_| HostRequestError::BufferClosed)
                .and_then(|model| {
                    model
                        .read_with(cx, |model, _| model.snapshot(range))
                        .map_err(map_buffer_error)
                })
                .map(HostResponseValue::Snapshot),
            HostOperation::ApplyEdits {
                buffer,
                edits,
                if_revision,
            } => self
                .buffer_registry
                .resolve(buffer)
                .map_err(|_| HostRequestError::BufferClosed)
                .and_then(|model| {
                    let changed = model
                        .update(cx, |model, cx| {
                            let changed = model.apply_edits(&edits, if_revision)?;
                            if changed {
                                cx.notify();
                            }
                            Ok(changed)
                        })
                        .map_err(map_buffer_error)?;
                    Ok(changed)
                })
                .map(|changed| {
                    if changed {
                        self.editor
                            .update(cx, |editor, cx| editor.refresh_from_model(cx));
                    }
                    let revision = self
                        .buffer_registry
                        .resolve(buffer)
                        .expect("buffer remains open after its own edit")
                        .read_with(cx, |model, _| model.revision());
                    HostResponseValue::AppliedEdits { revision }
                }),
            HostOperation::RegisterCommand { name } => self
                .command_registry
                .register(name, request.extension, request.lifecycle)
                .map(|registration| HostResponseValue::CommandRegistered { registration })
                .map_err(map_command_error),
            HostOperation::UnregisterCommand { registration } => self
                .command_registry
                .unregister(registration, request.extension, request.lifecycle)
                .map(|()| HostResponseValue::CommandUnregistered { registration })
                .map_err(map_command_error),
            HostOperation::SubscribeBufferChanges { buffer } => self
                .buffer_registry
                .resolve(buffer)
                .map_err(|_| HostRequestError::BufferClosed)
                .map(|_| HostResponseValue::BufferChangesSubscribed {
                    subscription: self.buffer_subscriptions.subscribe(
                        buffer,
                        request.extension,
                        request.lifecycle,
                    ),
                }),
            HostOperation::UnsubscribeBufferChanges { subscription } => {
                if self.buffer_subscriptions.unsubscribe(
                    subscription,
                    request.extension,
                    request.lifecycle,
                ) {
                    Ok(HostResponseValue::BufferChangesUnsubscribed { subscription })
                } else {
                    Err(HostRequestError::BufferClosed)
                }
            }
        };

        HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result,
        }
    }

    fn publish_buffer_change(&self, change: BufferChange) {
        for subscription in self.buffer_subscriptions.for_buffer(change.buffer) {
            if let Some(control) = self
                .extension_controls
                .get(&(subscription.extension, subscription.lifecycle))
            {
                let _ = control.dispatch_buffer_change(subscription.id, change.clone());
            }
        }
    }

    #[allow(
        dead_code,
        reason = "buffer closing is not exposed by the prototype shell yet"
    )]
    fn close_buffer(
        &mut self,
        buffer: crate::host::protocol::BufferHandle,
        cx: &mut Context<Self>,
    ) {
        self.buffer_registry
            .close(buffer, cx)
            .expect("buffer is open");
        self.buffer_subscriptions.remove_buffer(buffer);
    }

    fn publish_model_change(&mut self, model: Entity<BufferModel>, cx: &mut Context<Self>) {
        let Some(buffer) = self.buffer_registry.active_handle() else {
            return;
        };
        let active = match self.buffer_registry.resolve(buffer) {
            Ok(active) => active,
            Err(_) => return,
        };
        if active != model {
            return;
        }
        while let Some(change) = model.update(cx, |model, _| model.take_pending_change()) {
            self.publish_buffer_change(BufferChange {
                buffer,
                before_revision: change.before_revision,
                revision: change.revision,
                edits: change.edits,
            });
        }
    }

    fn invoke_fixture_command(&mut self, cx: &mut Context<Self>) {
        if self.active_command.is_some() {
            return;
        }
        let target = match self.command_registry.resolve("knot.fixture.edit") {
            Ok(target) => target,
            Err(_) => {
                self.command_state = "unavailable".into();
                cx.notify();
                return;
            }
        };
        let Some(control) = self
            .extension_controls
            .get(&(target.extension, target.lifecycle))
            .cloned()
        else {
            self.command_state = "unavailable".into();
            cx.notify();
            return;
        };
        let id = self.allocate_command_invocation();
        self.active_command = Some(id);
        self.command_state = "running".into();
        let execution = control.invoke_command(
            CommandInvocation {
                id,
                registration: target.registration,
                extension: target.extension,
                lifecycle: target.lifecycle,
            },
            self.buffer_registry.active_handle(),
        );
        cx.spawn(async move |this, cx| {
            let result = execution.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_command(id, result.is_ok(), cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn allocate_command_invocation(&mut self) -> CommandInvocationId {
        let id = CommandInvocationId::new(self.next_command_invocation);
        self.next_command_invocation = self
            .next_command_invocation
            .checked_add(1)
            .expect("command invocation space exhausted");
        id
    }

    fn finish_command(
        &mut self,
        invocation: CommandInvocationId,
        succeeded: bool,
        cx: &mut Context<Self>,
    ) {
        if self.active_command != Some(invocation) {
            return;
        }
        self.active_command = None;
        self.command_state = if self.cancelled_commands.remove(&invocation) {
            "cancelled".into()
        } else if succeeded {
            "completed".into()
        } else {
            "failed".into()
        };
        cx.notify();
    }

    fn cancel_active_command(&mut self, cx: &mut Context<Self>) {
        if let Some(invocation) = self.active_command {
            self.cancelled_commands.insert(invocation);
            self.command_state = "cancelling".into();
            cx.notify();
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
        for control in self.extension_controls.values() {
            control.request_shutdown();
        }
        for thread in self.runtime_threads.drain(..) {
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
                            .child(format!("command {}", self.command_state))
                            .child(
                                div()
                                    .id("run-fixture-command")
                                    .cursor_pointer()
                                    .text_color(rgb(0x80c0ff))
                                    .child("run command")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.invoke_fixture_command(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id("cancel-fixture-command")
                                    .cursor_pointer()
                                    .text_color(rgb(0xffb080))
                                    .child("cancel command")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_active_command(cx);
                                    })),
                            )
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
    use crate::host::{
        ExtensionRuntimeControl, V8Host,
        protocol::{ByteRange, ExtensionId, TextEdit},
    };

    async fn wait_for_runtime_state(
        shell: &Entity<Shell>,
        expected: &str,
        cx: &mut TestAppContext,
    ) {
        while cx.read(|cx| shell.read(cx).runtime_state != expected) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
    }

    async fn wait_for_command_state(
        shell: &Entity<Shell>,
        expected: &str,
        cx: &mut TestAppContext,
    ) {
        while cx.read(|cx| shell.read(cx).command_state != expected) {
            shell.next_notification(Duration::ZERO, cx).await;
        }
    }

    fn only_runtime_control(
        shell: &Entity<Shell>,
        cx: &mut TestAppContext,
    ) -> ExtensionRuntimeControl {
        cx.read(|cx| {
            shell
                .read(cx)
                .extension_controls
                .values()
                .next()
                .cloned()
                .expect("single-runtime shell has its control")
        })
    }

    #[gpui::test]
    async fn foreground_bridge_resolves_the_displayed_active_buffer(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(7))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);
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
    async fn extension_buffer_proxy_reads_and_edits_the_displayed_model(cx: &mut TestAppContext) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(9))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let control = only_runtime_control(&shell, cx);

        control
            .execute_fixture_module(
                "file:///fixtures/buffer-proxy.js",
                r#"
                    import { editor } from "knot:editor";

                    const buffer = await editor.activeBuffer();
                    const snapshot = await buffer.snapshot();
                    const start = snapshot.byteOffsetAtUtf16(0);
                    await buffer.applyEdits(
                        [{ range: { startByteOffset: start, endByteOffset: start }, text: "// extension\n" }],
                        { ifRevision: snapshot.revision },
                    );
                    const afterComment = await buffer.snapshot();
                    await buffer.applyEdits(
                        [{ range: { startByteOffset: 0, endByteOffset: 0 }, text: "😀中e\u0301" }],
                        { ifRevision: afterComment.revision },
                    );
                    const unicode = await buffer.snapshot({ startByteOffset: 0, endByteOffset: 10 });
                    if (unicode.byteOffsetAtUtf16(2) !== 4 || unicode.byteOffsetAtUtf16(3) !== 7) {
                        throw new Error("unexpected UTF-16 to byte conversion");
                    }
                    if (unicode.utf16OffsetAtByte(7) !== 3) {
                        throw new Error("unexpected byte to UTF-16 conversion");
                    }
                    for (const invalid of [
                        () => unicode.byteOffsetAtUtf16(1),
                        () => unicode.utf16OffsetAtByte(1),
                    ]) {
                        try { invalid(); throw new Error("accepted split boundary"); } catch (error) {
                            if (!(error instanceof RangeError)) throw error;
                        }
                    }
                    globalThis.extensionRevision = unicode.revision;
                "#,
            )
            .await
            .unwrap();

        let (text, revision) = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            let model = shell.buffer_registry.resolve(handle).unwrap();
            model.read_with(cx, |model, _| (model.text(), model.revision()))
        });
        assert!(text.starts_with("😀中é// extension\n"));
        assert_eq!(revision, 2);
    }

    #[gpui::test]
    async fn buffer_changes_fan_out_in_order_and_survive_listener_failures(
        cx: &mut TestAppContext,
    ) {
        let host = V8Host::new();
        let shell = cx.new(|cx| {
            Shell::new_with_runtimes(
                vec![
                    host.spawn_extension(ExtensionId::new(12)).into_parts(),
                    host.spawn_extension(ExtensionId::new(13)).into_parts(),
                ],
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let controls = cx.read(|cx| {
            shell
                .read(cx)
                .extension_controls
                .values()
                .cloned()
                .collect::<Vec<_>>()
        });
        assert_eq!(controls.len(), 2);

        for control in &controls {
            control
                .execute_fixture_module(
                    format!("file:///fixtures/subscriber-{}.js", control.identity().0.value()),
                    r#"
                        import { editor } from "knot:editor";
                        const buffer = await editor.activeBuffer();
                        globalThis.disposable = await buffer.onDidChange((event) => {
                          globalThis.events = [...(globalThis.events ?? []), `${event.revision}:${event.edits.length}`];
                          if (event.revision === 1 && globalThis.failFirst) {
                            throw new Error("expected listener failure");
                          }
                        });
                    "#,
                )
                .await
                .unwrap();
        }
        controls[1]
            .execute_fixture_script("fail-first-listener.js", "globalThis.failFirst = true")
            .await
            .unwrap();

        shell.update(cx, |shell, cx| {
            let handle = shell.buffer_registry.active_handle().unwrap();
            let model = shell.buffer_registry.resolve(handle).unwrap();
            assert!(model.update(cx, |model, cx| {
                let changed = model
                    .apply_edits(
                        &[
                            TextEdit {
                                range: ByteRange {
                                    start_byte_offset: 0,
                                    end_byte_offset: 0,
                                },
                                text: "A".into(),
                            },
                            TextEdit {
                                range: ByteRange {
                                    start_byte_offset: 1,
                                    end_byte_offset: 1,
                                },
                                text: "B".into(),
                            },
                        ],
                        model.revision(),
                    )
                    .unwrap();
                cx.notify();
                changed
            }));
            model.update(cx, |model, cx| {
                assert!(model.replace(0..1, "C"));
                cx.notify();
            });
        });
        cx.run_until_parked();

        for control in &controls {
            control
                .execute_fixture_script(
                    "verify-buffer-change-events.js",
                    "if (globalThis.events.join(',') !== '1:2,2:1') throw new Error(`missing or unordered buffer changes: ${globalThis.events}`)",
                )
                .await
                .unwrap();
        }

        controls[0]
            .execute_fixture_script(
                "dispose-buffer-listener.js",
                "globalThis.disposable.dispose()",
            )
            .await
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            cx.read(|cx| {
                shell
                    .read(cx)
                    .buffer_subscriptions
                    .for_buffer(shell.read(cx).buffer_registry.active_handle().unwrap())
                    .count()
            }),
            1
        );

        let handle = cx.read(|cx| shell.read(cx).buffer_registry.active_handle().unwrap());
        shell.update(cx, |shell, cx| shell.close_buffer(handle, cx));
        assert!(cx.read(|cx| {
            shell
                .read(cx)
                .buffer_subscriptions
                .for_buffer(handle)
                .next()
                .is_none()
        }));
    }

    #[gpui::test]
    async fn fixture_command_edits_the_displayed_buffer_without_blocking_the_bridge(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(10))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        shell.update(cx, |shell, cx| shell.invoke_fixture_command(cx));
        wait_for_command_state(&shell, "completed", cx).await;

        let text = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            shell
                .buffer_registry
                .resolve(handle)
                .unwrap()
                .read_with(cx, |model, _| model.text())
        });
        assert!(text.starts_with("// command\n"));
    }

    #[gpui::test]
    async fn fixture_command_runs_on_its_registered_extension(cx: &mut TestAppContext) {
        let host = V8Host::new();
        let shell = cx.new(|cx| {
            Shell::new_with_runtimes(
                vec![
                    host.spawn_extension(ExtensionId::new(14)).into_parts(),
                    host.spawn_extension(ExtensionId::new(15)).into_parts(),
                ],
                cx,
            )
        });
        wait_for_runtime_state(&shell, "running", cx).await;
        let (first, second) = cx.read(|cx| {
            let shell = shell.read(cx);
            let mut controls = shell.extension_controls.values().cloned();
            let first = controls
                .find(|control| control.identity().0 == ExtensionId::new(14))
                .unwrap();
            let second = shell
                .extension_controls
                .values()
                .find(|control| control.identity().0 == ExtensionId::new(15))
                .cloned()
                .unwrap();
            (first, second)
        });
        let (first_extension, first_lifecycle) = first.identity();
        shell.update(cx, |shell, _| {
            shell
                .command_registry
                .remove_lifecycle(first_extension, first_lifecycle);
        });
        second
            .execute_fixture_module(
                "file:///fixtures/second-owner-command.js",
                r#"
                    import { commands } from "knot:editor";
                    await commands.register("knot.fixture.edit", () => {
                      globalThis.ranOnSecondExtension = true;
                    });
                "#,
            )
            .await
            .unwrap();

        shell.update(cx, |shell, cx| shell.invoke_fixture_command(cx));
        wait_for_command_state(&shell, "completed", cx).await;
        second
            .execute_fixture_script(
                "verify-second-owner-command.js",
                "if (!globalThis.ranOnSecondExtension) throw new Error('command ran on the wrong extension')",
            )
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn cancelling_before_the_awaited_command_request_prevents_its_edit(
        cx: &mut TestAppContext,
    ) {
        let runtime = V8Host::new()
            .spawn_extension(ExtensionId::new(11))
            .into_parts();
        let shell = cx.new(|cx| Shell::new(runtime, cx));
        wait_for_runtime_state(&shell, "running", cx).await;
        let initial_text = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            shell
                .buffer_registry
                .resolve(handle)
                .unwrap()
                .read_with(cx, |model, _| model.text())
        });

        shell.update(cx, |shell, cx| {
            shell.invoke_fixture_command(cx);
            shell.cancel_active_command(cx);
        });
        wait_for_command_state(&shell, "cancelled", cx).await;

        only_runtime_control(&shell, cx)
            .execute_fixture_script(
                "verify-command-abort.js",
                "if (!globalThis.knotFixtureCommandAborted) throw new Error('command signal was not aborted')",
            )
            .await
            .unwrap();

        let text = cx.read(|cx| {
            let shell = shell.read(cx);
            let handle = shell.buffer_registry.active_handle().unwrap();
            shell
                .buffer_registry
                .resolve(handle)
                .unwrap()
                .read_with(cx, |model, _| model.text())
        });
        assert_eq!(text, initial_text);
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

        let control = only_runtime_control(&shell, cx);
        control.request_shutdown();
        wait_for_runtime_state(&shell, "closed", cx).await;

        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        let third = cx.read(|cx| shell.read(cx).heartbeat);
        assert!(third > second);
    }
}
