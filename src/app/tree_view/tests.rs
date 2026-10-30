use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AppContext, Focusable, Subscription, TestAppContext};

use super::{TreeProviderIdentity, TreeView, TreeViewEvent, TreeViewRegistrationError};
use crate::host::protocol::{
    CommandArgumentValue, ExtensionId, ExtensionLifecycleId, TreeAction, TreeChildrenResponse,
    TreeCollapsibleState, TreeItem, TreeItemKind, TreeProviderRegistrationId,
};

type CommandLog = Rc<RefCell<Vec<(String, CommandArgumentValue)>>>;

struct EventSubscriber {
    _events: CommandLog,
    _subscription: Subscription,
}

fn subscribe_commands(
    cx: &mut TestAppContext,
    tree: &gpui::Entity<TreeView>,
) -> (CommandLog, gpui::Entity<EventSubscriber>) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let log = events.clone();
    let sink = events.clone();
    let tree = tree.clone();
    let subscriber = cx.update(move |cx| {
        cx.new(move |cx| {
            let subscription = cx.subscribe(&tree, move |_, _, event: &TreeViewEvent, _| {
                if let TreeViewEvent::InvokeCommand {
                    command, arguments, ..
                } = event
                {
                    sink.borrow_mut().push((command.clone(), arguments.clone()));
                }
            });
            EventSubscriber {
                _events: log,
                _subscription: subscription,
            }
        })
    });
    (events, subscriber)
}

fn item(id: &str, label: &str, kind: TreeItemKind, actions: Vec<TreeAction>) -> TreeItem {
    TreeItem {
        id: id.into(),
        label: label.into(),
        description: None,
        icon: None,
        collapsible_state: TreeCollapsibleState::None,
        command: Some("example.activate".into()),
        kind,
        status_text: Some("M".into()),
        badge: Some("warning".to_owned()),
        actions,
    }
}

#[gpui::test]
fn provider_registration_enforces_view_and_ownership(cx: &mut TestAppContext) {
    let registration = TreeProviderRegistrationId::new(3);
    let owner = TreeProviderIdentity {
        extension: ExtensionId::new(7),
        lifecycle: ExtensionLifecycleId::new(2),
        registration,
    };
    let tree = cx.new(|cx| TreeView::new("outline", "1", cx));

    tree.update(cx, |tree, cx| {
        assert_eq!(
            tree.register_provider("wrong-view", owner, Vec::new(), cx),
            Err(TreeViewRegistrationError::WrongView)
        );
        tree.register_provider("outline", owner, Vec::new(), cx)
            .unwrap();
        assert!(tree.owns_provider(registration, owner.extension, owner.lifecycle));
        assert!(!tree.owns_provider(registration, owner.extension, ExtensionLifecycleId::new(1)));
        assert_eq!(
            tree.register_provider(
                "outline",
                TreeProviderIdentity {
                    extension: ExtensionId::new(8),
                    lifecycle: ExtensionLifecycleId::new(1),
                    registration: TreeProviderRegistrationId::new(4),
                },
                Vec::new(),
                cx,
            ),
            Err(TreeViewRegistrationError::ProviderInUse)
        );
        assert_eq!(
            tree.unregister_provider(TreeProviderRegistrationId::new(4), cx),
            Err(TreeViewRegistrationError::ProviderNotFound)
        );
        assert!(tree.owns_provider(registration, owner.extension, owner.lifecycle));
    });
}

#[gpui::test]
fn stale_and_disposed_tree_responses_are_ignored(cx: &mut TestAppContext) {
    let registration = TreeProviderRegistrationId::new(3);
    let tree = cx.new(|cx| TreeView::new("outline", "1", cx));
    let stale_generation = tree.update(cx, |tree, cx| {
        tree.register_provider(
            "outline",
            TreeProviderIdentity {
                extension: ExtensionId::new(7),
                lifecycle: ExtensionLifecycleId::new(2),
                registration,
            },
            Vec::new(),
            cx,
        )
        .unwrap();
        let stale_generation = tree.children[&None].generation;
        tree.invalidate(registration, None, cx).unwrap();
        stale_generation
    });

    let view = cx.read(|cx| tree.read(cx).instance_id());
    tree.update(cx, |tree, cx| {
        assert!(!tree.apply_response(
            TreeChildrenResponse {
                registration,
                window: "1".into(),
                view,
                parent_id: None,
                generation: stale_generation,
                result: Ok(Vec::new()),
            },
            cx,
        ));
        let current_generation = tree.children[&None].generation;
        assert!(tree.apply_response(
            TreeChildrenResponse {
                registration,
                window: "1".into(),
                view,
                parent_id: None,
                generation: current_generation,
                result: Ok(vec![TreeItem {
                    id: "root".into(),
                    label: "Root".into(),
                    description: None,
                    icon: None,
                    collapsible_state: TreeCollapsibleState::None,
                    command: None,
                    kind: crate::host::protocol::TreeItemKind::Item,
                    status_text: None,
                    badge: None,
                    actions: Vec::new(),
                }]),
            },
            cx,
        ));
        assert_eq!(tree.children[&None].items[0].id, "root");
        tree.unregister_provider(registration, cx).unwrap();
        assert!(!tree.apply_response(
            TreeChildrenResponse {
                registration,
                window: "1".into(),
                view,
                parent_id: None,
                generation: current_generation,
                result: Ok(Vec::new()),
            },
            cx,
        ));
        tree.register_provider(
            "outline",
            TreeProviderIdentity {
                extension: ExtensionId::new(7),
                lifecycle: ExtensionLifecycleId::new(2),
                registration,
            },
            Vec::new(),
            cx,
        )
        .unwrap();
        let lifecycle_generation = tree.children[&None].generation;
        tree.remove_lifecycle(ExtensionId::new(7), ExtensionLifecycleId::new(2), cx);
        assert!(!tree.apply_response(
            TreeChildrenResponse {
                registration,
                window: "1".into(),
                view,
                parent_id: None,
                generation: lifecycle_generation,
                result: Ok(Vec::new()),
            },
            cx,
        ));
    });
}

fn owner(registration: TreeProviderRegistrationId) -> TreeProviderIdentity {
    TreeProviderIdentity {
        extension: ExtensionId::new(7),
        lifecycle: ExtensionLifecycleId::new(2),
        registration,
    }
}

fn response(
    registration: TreeProviderRegistrationId,
    view: crate::host::protocol::ViewId,
    parent_id: Option<String>,
    generation: u64,
    items: Vec<TreeItem>,
) -> TreeChildrenResponse {
    TreeChildrenResponse {
        registration,
        window: "1".into(),
        view,
        parent_id,
        generation,
        result: Ok(items),
    }
}

fn windowed_tree(cx: &mut TestAppContext) -> (gpui::Entity<TreeView>, gpui::AnyWindowHandle) {
    let (tree, window) = cx.add_window_view(|_, cx| TreeView::new("example.git", "1", cx));
    let handle = window.update(|window, _| window.window_handle());
    (tree, handle)
}

#[gpui::test]
fn groups_fetch_children_and_are_not_activated(cx: &mut TestAppContext) {
    let registration = TreeProviderRegistrationId::new(5);
    let (tree, handle) = windowed_tree(cx);
    let (events, _subscriber) = subscribe_commands(cx, &tree);
    let generation = tree.update(cx, |tree, cx| {
        tree.register_provider("example.git", owner(registration), Vec::new(), cx)
            .unwrap();
        tree.root_generation()
    });
    let view = cx.read(|cx| tree.read(cx).instance_id());
    tree.update(cx, |tree, cx| {
        assert!(tree.apply_response(
            response(
                registration,
                view,
                None,
                generation,
                vec![
                    item("staged", "Staged", TreeItemKind::Group, Vec::new()),
                    item("notes", "notes.txt", TreeItemKind::Item, Vec::new()),
                ],
            ),
            cx,
        ));
    });

    let group_generation =
        cx.read(|cx| tree.read(cx).children[&Some("staged".to_owned())].generation);
    tree.update(cx, |tree, cx| {
        assert!(tree.apply_response(
            response(
                registration,
                view,
                Some("staged".into()),
                group_generation,
                vec![item("file", "file.rs", TreeItemKind::Item, Vec::new())],
            ),
            cx,
        ));
    });
    assert_eq!(
        cx.read(|cx| tree.read(cx).visible_labels()),
        ["Staged", "file.rs", "notes.txt"]
    );

    tree.update(cx, |tree, cx| tree.activate("staged", handle, cx));
    assert!(
        events.borrow().is_empty(),
        "a group header is not activatable"
    );

    tree.update(cx, |tree, cx| tree.activate("notes", handle, cx));
    let events = events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "example.activate");
    assert_eq!(events[0].1, CommandArgumentValue::Null);
}

#[gpui::test]
fn actions_carry_arguments_and_enter_keyboard_focus(cx: &mut TestAppContext) {
    let registration = TreeProviderRegistrationId::new(6);
    let (tree, handle) = windowed_tree(cx);
    let (events, _subscriber) = subscribe_commands(cx, &tree);
    let frame_action = TreeAction {
        label: "Refresh".into(),
        command: "example.refresh".into(),
        arguments: CommandArgumentValue::Null,
    };
    let generation = tree.update(cx, |tree, cx| {
        tree.register_provider("example.git", owner(registration), vec![frame_action], cx)
            .unwrap();
        tree.root_generation()
    });
    let view = cx.read(|cx| tree.read(cx).instance_id());
    let stage = TreeAction {
        label: "Stage".into(),
        command: "example.stage".into(),
        arguments: CommandArgumentValue::Object(
            [(
                "path".into(),
                CommandArgumentValue::String("notes.txt".into()),
            )]
            .into(),
        ),
    };
    tree.update(cx, |tree, cx| {
        assert!(tree.apply_response(
            response(
                registration,
                view,
                None,
                generation,
                vec![item(
                    "notes",
                    "notes.txt",
                    TreeItemKind::Item,
                    vec![stage.clone()],
                )],
            ),
            cx,
        ));
        assert!(tree.select_item("notes", cx));
        tree.enter_action_focus(cx);
        assert_eq!(
            tree.action_focus_for_test(),
            Some((Some("notes".into()), 0))
        );
        tree.invoke_action(stage.clone(), handle, cx);
    });
    let events = events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "example.stage");
    assert_eq!(events[0].1, stage.arguments);
}

#[gpui::test]
fn arrow_keys_move_selection_across_visible_rows(cx: &mut TestAppContext) {
    let registration = TreeProviderRegistrationId::new(9);
    let (tree, window) = windowed_tree(cx);
    let generation = tree.update(cx, |tree, cx| {
        tree.register_provider("example.git", owner(registration), Vec::new(), cx)
            .unwrap();
        tree.root_generation()
    });
    let view = cx.read(|cx| tree.read(cx).instance_id());
    tree.update(cx, |tree, cx| {
        assert!(tree.apply_response(
            response(
                registration,
                view,
                None,
                generation,
                vec![
                    item("a", "a.rs", TreeItemKind::Item, Vec::new()),
                    item("b", "b.rs", TreeItemKind::Item, Vec::new()),
                    item("c", "c.rs", TreeItemKind::Item, Vec::new()),
                ],
            ),
            cx,
        ));
    });
    cx.update_window(window, |_, window, cx| {
        tree.read(cx).focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.refresh().unwrap();

    let selected = |cx: &mut TestAppContext| cx.read(|cx| tree.read(cx).selected.clone());
    cx.simulate_keystrokes(window, "down");
    assert_eq!(selected(cx).as_deref(), Some("a"));
    cx.simulate_keystrokes(window, "down");
    assert_eq!(selected(cx).as_deref(), Some("b"));
    cx.simulate_keystrokes(window, "down");
    assert_eq!(selected(cx).as_deref(), Some("c"));
    cx.simulate_keystrokes(window, "down");
    assert_eq!(selected(cx).as_deref(), Some("c"));
    cx.simulate_keystrokes(window, "up");
    assert_eq!(selected(cx).as_deref(), Some("b"));

    // Stale rows stay navigable while a root refresh is in flight.
    tree.update(cx, |tree, cx| {
        tree.invalidate(registration, None, cx).unwrap();
    });
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "up");
    assert_eq!(selected(cx).as_deref(), Some("a"));
}

#[gpui::test]
fn loading_and_error_states_are_local_to_one_parent(cx: &mut TestAppContext) {
    use crate::host::protocol::TreeProviderError;

    let registration = TreeProviderRegistrationId::new(8);
    let tree = cx.new(|cx| TreeView::new("example.git", "1", cx));
    let generation = tree.update(cx, |tree, cx| {
        tree.register_provider("example.git", owner(registration), Vec::new(), cx)
            .unwrap();
        tree.root_generation()
    });
    let view = cx.read(|cx| tree.read(cx).instance_id());
    tree.update(cx, |tree, cx| {
        assert!(tree.apply_response(
            response(
                registration,
                view,
                None,
                generation,
                vec![
                    item("staged", "Staged", TreeItemKind::Group, Vec::new()),
                    item("notes", "notes.txt", TreeItemKind::Item, Vec::new()),
                ],
            ),
            cx,
        ));
        assert!(tree.is_loading(), "the group child request is pending");

        let group_generation = tree.children[&Some("staged".to_owned())].generation;
        assert!(tree.apply_response(
            TreeChildrenResponse {
                registration,
                window: "1".into(),
                view,
                parent_id: Some("staged".into()),
                generation: group_generation,
                result: Err(TreeProviderError {
                    message: "boom".into(),
                }),
            },
            cx,
        ));
        assert_eq!(tree.lifecycle_state(), (true, 2, false, 1));
        assert_eq!(tree.visible_labels(), ["Staged", "notes.txt"]);
    });
}

#[gpui::test]
fn root_refresh_keeps_rows_and_reloads_group_children(cx: &mut TestAppContext) {
    let registration = TreeProviderRegistrationId::new(7);
    let tree = cx.new(|cx| TreeView::new("example.git", "1", cx));
    let generation = tree.update(cx, |tree, cx| {
        tree.register_provider("example.git", owner(registration), Vec::new(), cx)
            .unwrap();
        tree.root_generation()
    });
    let view = cx.read(|cx| tree.read(cx).instance_id());
    tree.update(cx, |tree, cx| {
        assert!(tree.apply_response(
            response(
                registration,
                view,
                None,
                generation,
                vec![item("staged", "Staged", TreeItemKind::Group, Vec::new())],
            ),
            cx,
        ));
        let group_generation = tree.children[&Some("staged".to_owned())].generation;
        assert!(tree.apply_response(
            response(
                registration,
                view,
                Some("staged".into()),
                group_generation,
                vec![item("file", "file.rs", TreeItemKind::Item, Vec::new())],
            ),
            cx,
        ));
        assert_eq!(tree.visible_labels(), ["Staged", "file.rs"]);

        // A root refresh marks the root loading but keeps stale rows so
        // keyboard navigation stays usable while the refetch is pending.
        tree.invalidate(registration, None, cx).unwrap();
        assert!(tree.is_loading());
        assert_eq!(tree.visible_labels(), ["Staged", "file.rs"]);

        // The refreshed root reloads group children even when cached.
        let refreshed = tree.children[&None].generation;
        tree.apply_response(
            response(
                registration,
                view,
                None,
                refreshed,
                vec![item("staged", "Staged", TreeItemKind::Group, Vec::new())],
            ),
            cx,
        );
        let reloaded = tree.children[&Some("staged".to_owned())].generation;
        assert!(reloaded > group_generation);
    });
}
