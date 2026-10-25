//! Application launch, product windows, the launch gate, menus, and default keys.

use super::*;

pub(super) fn product_filesystems() -> Arc<FileSystemProviderRegistry> {
    static FILESYSTEMS: OnceLock<Arc<FileSystemProviderRegistry>> = OnceLock::new();
    FILESYSTEMS
        .get_or_init(|| {
            let runtime = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .thread_name("knot-product-filesystem")
                    .build()
                    .expect("product filesystem runtime starts"),
            );
            let mut providers = FileSystemProviderRegistry::new();
            providers
                .register(
                    "file",
                    Arc::new(LocalFileSystemProvider::unrestricted(runtime)),
                )
                .expect("the product file provider is unique");
            Arc::new(providers)
        })
        .clone()
}

pub(crate) fn open_product_window(request: Option<OpenRequest>, cx: &mut App) {
    open_product_window_with(request, None, cx);
}

/// Open a product window, presenting documents restored at startup. Without an
/// open request the first restored document replaces the untitled placeholder.
pub(super) fn open_product_window_with(
    request: Option<OpenRequest>,
    recovery: Option<crate::app::recovery::StartupRecovery>,
    cx: &mut App,
) {
    let documents = cx.global::<ApplicationDocuments>().0.clone();
    let mut restored = recovery
        .as_ref()
        .map(|recovery| recovery.restored.clone())
        .unwrap_or_default()
        .into_iter();
    let document = match request {
        None => restored.next(),
        Some(_) => None,
    }
    .unwrap_or_else(|| create_untitled_document(&documents, cx));
    let model = documents.read(cx).get(document).unwrap().model().clone();
    let restored = restored
        .filter_map(|document| {
            documents
                .read(cx)
                .get(document)
                .map(|entry| (document, entry.model().clone()))
        })
        .collect::<Vec<_>>();
    let status = recovery.as_ref().and_then(recovery_listing::startup_status);
    let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(320.))),
            ..Default::default()
        },
        move |window, cx| {
            let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            let shell = cx.new(|_| ProductShell::new(workbench));
            shell.update(cx, |shell, cx| shell.observe_extension_host(window, cx));
            install_protected_window_close(&shell, window, cx);
            shell.update(cx, |shell, cx| shell.restore_fresh_window_layout(cx));
            let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
            let tab = pane.active_tab();
            let target = OpenTarget {
                window: window.window_handle(),
                workbench: shell.read(cx).workbench.downgrade(),
                pane: pane.id(),
                tab: tab.id(),
                surface: tab.surface_id(),
                presentation: tab.presentation(),
            };
            shell.update(cx, |shell, cx| {
                shell.workbench.update(cx, |workbench, cx| {
                    for (document, model) in restored {
                        workbench.open_tab_for_document(target.pane, document, model, cx);
                    }
                    workbench.activate_tab(target.pane, target.tab);
                });
            });
            shell.read(cx).focus_active_surface(window, cx);
            shell.update(cx, |shell, cx| {
                // The recovery status outlives the launch open's own status.
                let opened =
                    request.map(|request| shell.start_open_operation(request, target, true, cx));
                let Some(status) = status else {
                    return;
                };
                let task = cx.spawn(async move |this, cx| {
                    if let Some(opened) = opened {
                        let _ = opened.await;
                    }
                    let _ = this.update(cx, |shell, cx| {
                        shell.status = status.into();
                        cx.notify();
                    });
                });
                shell.tasks.push(task);
            });
            shell
        },
    )
    .expect("product window must open");
}

pub(super) fn open_tab_transfer_window(cx: &mut App) -> anyhow::Result<WindowHandle<ProductShell>> {
    let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
    open_tab_transfer_window_with_bounds(bounds, cx)
}

pub(super) fn open_tab_tear_out_window(
    pointer: Point<Pixels>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<ProductShell>> {
    let mut bounds = Bounds::new(
        pointer - point(px(140.), px(40.)),
        size(px(1000.), px(720.)),
    );
    if let Some(display) = cx
        .displays()
        .into_iter()
        .find(|display| display.bounds().contains(&pointer))
    {
        let display = display.bounds();
        bounds.size.width = bounds.size.width.min(display.size.width).max(px(480.));
        bounds.size.height = bounds.size.height.min(display.size.height).max(px(320.));
        bounds.origin.x = bounds
            .origin
            .x
            .max(display.left())
            .min((display.right() - bounds.size.width).max(display.left()));
        bounds.origin.y = bounds
            .origin
            .y
            .max(display.top())
            .min((display.bottom() - bounds.size.height).max(display.top()));
    }
    open_tab_transfer_window_with_bounds(bounds, cx)
}

pub(super) fn open_tab_transfer_window_with_bounds(
    bounds: Bounds<Pixels>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<ProductShell>> {
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(320.))),
            focus: !cfg!(target_os = "macos"),
            show: !cfg!(target_os = "macos"),
            ..Default::default()
        },
        |window, cx| {
            let workbench = cx.new(|_| Workbench::empty());
            let shell = cx.new(|_| ProductShell::new(workbench));
            shell.update(cx, |shell, cx| shell.observe_extension_host(window, cx));
            install_protected_window_close(&shell, window, cx);
            shell
        },
    )
}

/// Open an empty product window that inherits the fresh-window tool layout;
/// session replacement presents its documents in the same turn.
pub(super) fn open_session_window(
    cx: &mut App,
) -> anyhow::Result<(Entity<ProductShell>, AnyWindowHandle)> {
    let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(320.))),
            focus: false,
            ..Default::default()
        },
        |window, cx| {
            let workbench = cx.new(|_| Workbench::empty());
            cx.global::<ApplicationWorkbenches>()
                .register(&workbench, window.window_handle());
            let shell = cx.new(|_| ProductShell::new(workbench));
            shell.update(cx, |shell, cx| shell.observe_extension_host(window, cx));
            install_protected_window_close(&shell, window, cx);
            shell.update(cx, |shell, cx| shell.restore_fresh_window_layout(cx));
            shell
        },
    )?;
    Ok((handle.entity(cx)?, handle.into()))
}

pub(super) fn dispatch_launch_request(request: Option<OpenRequest>, cx: &mut App) {
    let ready_request = cx
        .global::<ApplicationLaunchGate>()
        .0
        .borrow_mut()
        .request(request);
    if let Some(request) = ready_request {
        open_product_window(request, cx);
        cx.activate(true);
    }
}

pub(in crate::app) fn finish_product_startup(
    result: Result<(), crate::app::personal_config::ConfigDiagnostic>,
    cx: &mut App,
) {
    match result {
        Ok(()) => {
            // Restore unsaved work before the launch gate opens, so queued open
            // requests deduplicate against restored documents.
            let recovery =
                crate::app::recovery::restore_at_startup(ProductShell::filesystems(cx), cx);
            cx.spawn(async move |cx| {
                let recovery = recovery.await;
                let _ = cx.update(|cx| {
                    let requests = cx
                        .global::<ApplicationLaunchGate>()
                        .0
                        .borrow_mut()
                        .complete();
                    let mut recovery = Some(recovery);
                    for request in requests {
                        open_product_window_with(request, recovery.take(), cx);
                    }
                    cx.activate(true);
                });
            })
            .detach();
        }
        Err(diagnostic) => {
            cx.global::<ApplicationLaunchGate>().0.borrow_mut().fail();
            eprintln!("[knot] {}", diagnostic.clipboard_text().replace('\n', ": "));
            crate::app::personal_config_error::open(diagnostic, cx);
        }
    }
}

pub(crate) fn run(initial_request: Option<OpenRequest>) {
    let application = Application::new();
    let (open_requests, mut incoming_requests) = tokio::sync::mpsc::unbounded_channel();
    application.on_open_urls(move |urls| {
        for url in urls {
            match OpenRequest::from_url(&url) {
                Ok(request) => {
                    let _ = open_requests.send(request);
                }
                Err(error) => eprintln!("[knot] cannot open {url}: {error}"),
            }
        }
    });
    application.on_reopen(|cx| {
        dispatch_launch_request(None, cx);
    });
    application.run(move |cx| {
        let documents = cx.new(|_| DocumentCollection::new());
        cx.set_global(ApplicationDocuments(documents));
        // Install durable recovery before any document exists so every created
        // document is observed and protected from its first edit.
        let recovery_path = crate::app::recovery::RecoveryStore::user_path()
            .unwrap_or_else(|_| std::env::temp_dir().join("knot-recovery.sqlite3"));
        cx.set_global(crate::app::recovery::ApplicationRecovery(
            crate::app::recovery::RecoveryService::new(crate::app::recovery::RecoveryStore::open(
                recovery_path,
                cx.background_executor().clone(),
            )),
        ));
        cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
        cx.set_global(ApplicationProtectedClosure(RefCell::new(false)));
        cx.set_global(ApplicationTerminalSessions(RefCell::new(
            TerminalSessions::default(),
        )));
        // Load the persisted tool arrangement before any window opens so the
        // first window restores it; a missing or unreadable record keeps
        // product defaults without delaying startup.
        let layout_path = crate::app::tool_layout::ToolLayoutStore::user_path()
            .unwrap_or_else(|_| std::env::temp_dir().join("knot-tool-layout.sqlite3"));
        cx.set_global(crate::app::tool_layout::ApplicationToolLayout(
            crate::app::tool_layout::ToolLayoutStore::open(
                layout_path,
                cx.background_executor().clone(),
            ),
        ));
        cx.set_global(ApplicationLaunchGate(RefCell::new(LaunchGate::Pending(
            Vec::new(),
        ))));
        cx.set_global(ApplicationFileSystems(product_filesystems()));
        let commands = cx.new(ProductCommandDispatcher::new);
        cx.set_global(ApplicationProductCommands(commands));
        let extension_host = crate::app::extension_host::install(cx);
        bind_product_keys(cx);
        cx.set_menus(vec![
            Menu {
                name: "Knot".into(),
                items: vec![MenuItem::action(
                    "Quit Knot",
                    ProductCommandSource::new(QUIT_COMMAND),
                )],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New", ProductCommandSource::new(NEW_COMMAND)),
                    MenuItem::action(
                        "New Terminal",
                        ProductCommandSource::new(NEW_TERMINAL_COMMAND),
                    ),
                    MenuItem::action(
                        "Move Terminal to New Window",
                        ProductCommandSource::new(MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND),
                    ),
                    MenuItem::action("New Window", ProductCommandSource::new(NEW_WINDOW_COMMAND)),
                    MenuItem::action("Open…", ProductCommandSource::new(OPEN_COMMAND)),
                    MenuItem::action("Save", ProductCommandSource::new(SAVE_COMMAND)),
                    MenuItem::action("Save As…", ProductCommandSource::new(SAVE_AS_COMMAND)),
                    MenuItem::action(
                        "Recover Unsaved Documents…",
                        ProductCommandSource::new(commands::RECOVER_COMMAND),
                    ),
                    MenuItem::action("Close Tab", ProductCommandSource::new(CLOSE_TAB_COMMAND)),
                    MenuItem::action(
                        "Close Window",
                        ProductCommandSource::new(CLOSE_WINDOW_COMMAND),
                    ),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::action("Undo", ProductCommandSource::new(UNDO_COMMAND)),
                    MenuItem::action("Redo", ProductCommandSource::new(REDO_COMMAND)),
                    MenuItem::action("Cut", ProductCommandSource::new(CUT_COMMAND)),
                    MenuItem::action("Copy", ProductCommandSource::new(COPY_COMMAND)),
                    MenuItem::action("Paste", ProductCommandSource::new(PASTE_COMMAND)),
                    MenuItem::action("Select All", ProductCommandSource::new(SELECT_ALL_COMMAND)),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action(
                        "Split Right",
                        ProductCommandSource::new(SPLIT_HORIZONTAL_COMMAND),
                    ),
                    MenuItem::action(
                        "Split Down",
                        ProductCommandSource::new(SPLIT_VERTICAL_COMMAND),
                    ),
                ],
            },
        ]);
        let first_request = initial_request.or_else(|| incoming_requests.try_recv().ok());
        if let Some(request) = first_request {
            dispatch_launch_request(Some(request), cx);
        }
        cx.spawn(async move |cx| {
            while let Some(request) = incoming_requests.recv().await {
                let _ = cx.update(|cx| dispatch_launch_request(Some(request), cx));
            }
        })
        .detach();
        let extensions_root = crate::app::extension_package::user_extensions_root();
        extension_host.update(cx, |host, cx| {
            host.start_product_startup(None, extensions_root, cx)
        });
    });
}

pub(in crate::app) fn bind_product_keys(cx: &mut App) {
    crate::app::keymaps::install(cx);
}
