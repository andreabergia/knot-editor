//! Transient extension interactions own one window-local overlay.
use super::operations::empty_product_window;
use super::*;
use crate::app::extension_host::handlers::TransientError;
use crate::app::transient::{PromptText, TransientOutcome, TransientOwner};
use crate::host::protocol::{CommandInvocationId, PickerItem};

fn owner(invocation: Option<u64>) -> TransientOwner {
    TransientOwner {
        extension: ExtensionId::new(7),
        lifecycle: ExtensionLifecycleId::new(3),
        invocation: invocation.map(CommandInvocationId::new),
    }
}

fn picker_items() -> Vec<PickerItem> {
    vec![
        PickerItem {
            id: "alpha".into(),
            label: "Alpha".into(),
            description: Some("first".into()),
        },
        PickerItem {
            id: "beta".into(),
            label: "Beta".into(),
            description: Some("second".into()),
        },
    ]
}

fn settle(
    receiver: &mut tokio::sync::oneshot::Receiver<TransientOutcome>,
    cx: &mut TestAppContext,
) -> TransientOutcome {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        cx.run_until_parked();
        match receiver.try_recv() {
            Ok(outcome) => return outcome,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "transient interaction did not settle"
                );
                std::thread::yield_now();
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                panic!("transient sender dropped without an outcome")
            }
        }
    }
}

#[gpui::test]
fn picker_filters_accepts_keyboard_selection_and_restores_focus(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_surface(window, cx);
    })
    .unwrap();
    let before = cx
        .update_window(window, |_, window, cx| window.focused(cx))
        .unwrap();
    let mut receiver = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_picker(owner(None), "Pick".into(), picker_items(), window, cx)
            })
        })
        .unwrap()
        .unwrap();

    let overlap = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_input(owner(None), "Name".into(), String::new(), window, cx)
            })
        })
        .unwrap();
    assert!(matches!(overlap, Err(TransientError::OperationInProgress)));

    cx.simulate_input(window, "beta");
    cx.simulate_keystrokes(window, "enter");
    assert_eq!(
        settle(&mut receiver, cx),
        TransientOutcome::Picker("beta".into())
    );
    let after = cx
        .update_window(window, |_, window, cx| window.focused(cx))
        .unwrap();
    assert_eq!(before, after);
}

#[gpui::test]
fn input_and_native_prompts_settle_once(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);

    let mut input = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_input(owner(None), "Add path".into(), "seed".into(), window, cx)
            })
        })
        .unwrap()
        .unwrap();
    cx.simulate_keystrokes(window, "cmd-a");
    cx.simulate_input(window, "hello");
    cx.simulate_keystrokes(window, "enter");
    assert_eq!(
        settle(&mut input, cx),
        TransientOutcome::Input("hello".into())
    );

    let mut rejected = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_picker(owner(None), "Pick".into(), picker_items(), window, cx)
            })
        })
        .unwrap()
        .unwrap();
    cx.simulate_keystrokes(window, "escape");
    assert_eq!(settle(&mut rejected, cx), TransientOutcome::Dismissed);

    let mut confirmed = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_confirmation(
                    owner(None),
                    PromptText {
                        title: "Remove?".into(),
                        message: Some("fixture".into()),
                    },
                    "Remove".into(),
                    "Cancel".into(),
                    window,
                    cx,
                )
            })
        })
        .unwrap()
        .unwrap();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Remove");
    assert_eq!(settle(&mut confirmed, cx), TransientOutcome::Confirmed);

    let mut cancelled = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_confirmation(
                    owner(None),
                    PromptText {
                        title: "Remove?".into(),
                        message: None,
                    },
                    "Remove".into(),
                    "Cancel".into(),
                    window,
                    cx,
                )
            })
        })
        .unwrap()
        .unwrap();
    cx.simulate_prompt_answer("Cancel");
    assert_eq!(settle(&mut cancelled, cx), TransientOutcome::Dismissed);

    let mut message = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_message(
                    owner(None),
                    PromptText {
                        title: "Notice".into(),
                        message: None,
                    },
                    window,
                    cx,
                )
            })
        })
        .unwrap()
        .unwrap();
    cx.simulate_prompt_answer("OK");
    assert_eq!(settle(&mut message, cx), TransientOutcome::Dismissed);
}

#[gpui::test]
fn folder_picker_settles_selected_dismissed_and_failed(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_path_buf();

    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Selected(path.clone()),
    ))));
    let mut selected = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_folder_picker(Some(owner(None)), window, cx)
            })
        })
        .unwrap()
        .unwrap();
    assert_eq!(settle(&mut selected, cx), TransientOutcome::Folder(path));

    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Cancelled,
    ))));
    let mut dismissed = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_folder_picker(Some(owner(None)), window, cx)
            })
        })
        .unwrap()
        .unwrap();
    assert_eq!(settle(&mut dismissed, cx), TransientOutcome::Dismissed);

    cx.set_global(ApplicationOpenDialog(Arc::new(FixedOpenDialog(
        OpenDialogOutcome::Failed("dialog exploded".into()),
    ))));
    let mut failed = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_folder_picker(Some(owner(None)), window, cx)
            })
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        settle(&mut failed, cx),
        TransientOutcome::Failed("dialog exploded".into())
    );
}

#[gpui::test]
fn cancellation_matches_the_owner_and_two_windows_stay_independent(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (first_shell, first_window) = empty_product_window(&documents, cx);
    let (second_shell, second_window) = empty_product_window(&documents, cx);

    let mut first = cx
        .update_window(first_window, |_, window, cx| {
            first_shell.update(cx, |shell, cx| {
                shell.begin_picker(owner(None), "First".into(), picker_items(), window, cx)
            })
        })
        .unwrap()
        .unwrap();
    let mut second = cx
        .update_window(second_window, |_, window, cx| {
            second_shell.update(cx, |shell, cx| {
                shell.begin_picker(owner(None), "Second".into(), picker_items(), window, cx)
            })
        })
        .unwrap()
        .unwrap();

    // Another lifecycle cannot cancel this window's interaction.
    let other = TransientOwner {
        extension: ExtensionId::new(7),
        lifecycle: ExtensionLifecycleId::new(9),
        invocation: None,
    };
    first_shell.update(cx, |shell, cx| shell.cancel_transient(other, cx));
    cx.run_until_parked();
    assert!(first.try_recv().is_err());

    first_shell.update(cx, |shell, cx| shell.cancel_transient(owner(None), cx));
    assert_eq!(settle(&mut first, cx), TransientOutcome::Cancelled);

    // The second window still owns its own interaction.
    cx.simulate_input(second_window, "alpha");
    cx.simulate_keystrokes(second_window, "enter");
    assert_eq!(
        settle(&mut second, cx),
        TransientOutcome::Picker("alpha".into())
    );
}

#[gpui::test]
fn window_closure_releases_the_pending_interaction(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    let mut receiver = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.begin_picker(owner(None), "Pick".into(), picker_items(), window, cx)
            })
        })
        .unwrap()
        .unwrap();
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(settle(&mut receiver, cx), TransientOutcome::Cancelled);
}

#[gpui::test]
fn unloading_the_extension_settles_its_transient(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    const SOURCE: &str = r#"
import { commands, workbench } from "knot";
await commands.register("transient.await-picker", async () => {
  await workbench.showPicker({ title: "Pick", items: [{ id: "a", label: "A" }] });
});
"#;
    let documents = install_globals(cx);
    let host = cx.update(crate::app::extension_host::install);
    let key = ExtensionKey::new(ExtensionId::new(711), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("transient.await-picker")
            .is_ok()
    });
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let pending = dispatch_product_command(&shell, window, "transient.await-picker", cx);
    wait_for_api_condition(cx, |cx| shell.read(cx).transient.is_some());
    host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    cx.run_until_parked();
    cx.read(|cx| assert!(shell.read(cx).transient.is_none()));
    assert_ne!(wait_for_command(pending, cx), CommandOutcome::Completed);
}

#[gpui::test]
fn context_free_transient_requires_a_target(cx: &mut TestAppContext) {
    use crate::host::lifecycle::ExtensionKey;

    const SOURCE: &str = r#"
import { commands, workbench } from "knot";
async function rejects(action, code) {
  try { await action(); } catch (error) { if (error.code === code) return; throw error; }
  throw new Error(`expected ${code}`);
}
await rejects(() => workbench.showPicker({ title: "Pick", items: [] }), "ContextUnavailable");
await rejects(() => workbench.showInput({ title: "Name" }), "ContextUnavailable");
await rejects(() => workbench.showConfirmation({ title: "Sure?" }), "ContextUnavailable");
await rejects(() => workbench.showMessage({ title: "Notice" }), "ContextUnavailable");
await commands.register("transient.context-free-ok", () => {});
"#;
    let _documents = install_globals(cx);
    let host = cx.update(crate::app::extension_host::install);
    let key = ExtensionKey::new(ExtensionId::new(712), ExtensionLifecycleId::new(1));
    host.update(cx, |host, cx| {
        let execution = host.load_test_source(key, SOURCE).unwrap();
        cx.spawn(async move |_, _| {
            execution.await.unwrap();
        })
        .detach();
    });
    wait_for_api_condition(cx, |cx| {
        cx.global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog()
            .borrow()
            .resolve("transient.context-free-ok")
            .is_ok()
    });
}
