//! Application launch, product windows, the launch gate, menus, and default keys.

use super::*;

pub(super) fn product_filesystems() -> Arc<FileSystemProviderRegistry> {
    static FILESYSTEMS: OnceLock<Arc<FileSystemProviderRegistry>> = OnceLock::new();
    FILESYSTEMS
        .get_or_init(|| {
            let mut providers = FileSystemProviderRegistry::new();
            providers
                .register("file", Arc::new(LocalFileSystemProvider::unrestricted()))
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
) -> AnyWindowHandle {
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
    let notice = recovery.as_ref().and_then(recovery_listing::startup_notice);
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
                if let Some(request) = request {
                    drop(shell.start_open_operation(request, target, true, cx));
                }
                if let Some((title, detail)) = notice {
                    shell.show_notice(PromptLevel::Warning, title, detail, cx);
                }
            });
            shell
        },
    )
    .expect("product window must open")
    .into()
}

#[cfg(test)]
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
/// session replacement presents its documents in the same turn. Saved
/// geometry is fitted to the connected displays.
pub(super) fn open_session_window(
    geometry: Option<&crate::app::window_layout::WindowGeometry>,
    cx: &mut App,
) -> anyhow::Result<(Entity<ProductShell>, AnyWindowHandle)> {
    let (window_bounds, display_id) = match geometry {
        Some(geometry) => restored_bounds(geometry, cx),
        None => (
            WindowBounds::Windowed(Bounds::centered(None, size(px(1000.), px(720.)), cx)),
            None,
        ),
    };
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            window_min_size: Some(size(px(480.), px(320.))),
            display_id,
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

/// Saved geometry fitted to the connected displays, as window options.
fn restored_bounds(
    geometry: &crate::app::window_layout::WindowGeometry,
    cx: &App,
) -> (WindowBounds, Option<DisplayId>) {
    use crate::app::window_layout::{DisplayArea, Rect, WindowState, fit_to_displays};
    let displays = cx.displays();
    let primary = cx.primary_display().map(|display| display.id());
    let areas = displays
        .iter()
        .map(|display| {
            let bounds = display.bounds();
            DisplayArea {
                uuid: display.uuid().ok().map(|uuid| uuid.to_string()),
                bounds: Rect {
                    x: f32::from(bounds.origin.x),
                    y: f32::from(bounds.origin.y),
                    width: f32::from(bounds.size.width),
                    height: f32::from(bounds.size.height),
                },
            }
        })
        .collect::<Vec<_>>();
    let primary = displays
        .iter()
        .position(|display| Some(display.id()) == primary);
    let (rect, display) = fit_to_displays(geometry, &areas, primary);
    let bounds = Bounds::new(
        point(px(rect.x), px(rect.y)),
        size(px(rect.width), px(rect.height)),
    );
    let bounds = match geometry.state {
        WindowState::Windowed => WindowBounds::Windowed(bounds),
        WindowState::Maximized => WindowBounds::Maximized(bounds),
        WindowState::Fullscreen => WindowBounds::Fullscreen(bounds),
    };
    (bounds, display.map(|index| displays[index].id()))
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
            // Every package and both config phases have registered their
            // themes, so the selected one can resolve before windows open.
            crate::app::themes::apply_at_startup(cx);
            // Restore unsaved work, then the recorded windows, before the
            // launch gate opens, so restored windows present recovered
            // documents and queued requests deduplicate against both.
            let recovery =
                crate::app::recovery::restore_at_startup(ProductShell::filesystems(cx), cx);
            cx.spawn(async move |cx| {
                let recovery = recovery.await;
                let record = match cx.update(|cx| crate::app::window_sessions::take_for_restore(cx))
                {
                    Ok(task) => task.await,
                    Err(_) => None,
                };
                let restored = match record {
                    Some(record) => {
                        let (sender, receiver) = tokio::sync::oneshot::channel();
                        let started = cx.update(|cx| {
                            sessions::restore_windows(
                                record,
                                move |result| {
                                    let _ = sender.send(result);
                                },
                                cx,
                            )
                        });
                        match started {
                            Ok(Ok(())) => receiver.await.ok().and_then(Result::ok),
                            _ => None,
                        }
                    }
                    None => None,
                };
                let _ = cx.update(|cx| open_launch_windows(restored, recovery, cx));
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

/// Open each queued request in its own window after restoration. Recovered
/// documents no window presents go to the first restored window, else to the
/// first opened one; with nothing restored or requested one window opens.
/// The last request window ends up focused, else the previously active
/// restored window. Then window recording starts and extensions learn that
/// launch finished.
fn open_launch_windows(
    restored: Option<sessions::ReplaceWindowsResult>,
    recovery: crate::app::recovery::StartupRecovery,
    cx: &mut App,
) {
    let requests = cx
        .global::<ApplicationLaunchGate>()
        .0
        .borrow_mut()
        .complete()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let restored = restored
        .map(|result| result.windows)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|window| operations::capture_window(&window.window, cx).ok())
        .collect::<Vec<_>>();
    let mut recovery = Some(recovery);
    if let Some((shell, _)) = restored.first() {
        let recovery = recovery.take().expect("recovery is unclaimed");
        let workbenches = cx.global::<ApplicationWorkbenches>();
        let unpresented = recovery
            .restored
            .iter()
            .copied()
            .filter(|document| workbenches.view_count(*document, cx) == 0)
            .collect::<Vec<_>>();
        let notice = recovery_listing::startup_notice(&recovery);
        shell.update(cx, |shell, cx| {
            for document in unpresented {
                if ProductShell::documents(cx).read(cx).get(document).is_some() {
                    shell.present_session_document(document, cx);
                }
            }
            if let Some((title, detail)) = notice {
                shell.show_notice(PromptLevel::Warning, title, detail, cx);
            }
            cx.notify();
        });
    }
    if restored.is_empty() && requests.is_empty() {
        open_product_window_with(None, recovery.take(), cx);
    }
    let mut last = None;
    for request in requests {
        last = Some(open_product_window_with(Some(request), recovery.take(), cx));
    }
    if let Some(last) = last {
        let _ = cx.update_window(last, |_, window, _| window.activate_window());
    }
    open_report_after_failed_startup(cx);
    crate::app::window_sessions::start_recording(cx);
    crate::app::extensions::host::launched(cx);
    cx.activate(true);
}

/// A package that failed at startup has no other passive sign, so each launch
/// window opens the startup report once.
fn open_report_after_failed_startup(cx: &mut App) {
    let failed = crate::app::extensions::host::startup_report(cx).is_some_and(|report| {
        report.report.entries.iter().any(|entry| {
            matches!(
                entry.result,
                crate::app::extensions::load::LoadResult::Failed(_)
            )
        })
    });
    if !failed {
        return;
    }
    for window in cx.windows() {
        let Some(shell) = window
            .downcast::<ProductShell>()
            .and_then(|window| window.entity(cx).ok())
        else {
            continue;
        };
        shell.update(cx, |shell, cx| {
            shell.extension_report_open = true;
            cx.notify();
        });
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
        cx.set_global(crate::app::settings::ApplicationSettings::default());
        cx.set_global(ApplicationWorkbenches(RefCell::new(Vec::new())));
        cx.set_global(ApplicationProtectedClosure(RefCell::new(false)));
        cx.set_global(ApplicationTerminalSessions(RefCell::new(
            TerminalSessions::default(),
        )));
        // Load the persisted tool arrangement and recorded windows before any
        // window opens so the first window restores them; a missing or
        // unreadable record keeps product defaults without delaying startup.
        // Startup restores the recorded windows once config and extensions
        // have started.
        let state_path = crate::app::record_store::StateDatabase::user_path()
            .unwrap_or_else(|_| std::env::temp_dir().join("knot-state.sqlite3"));
        let state = crate::app::record_store::StateDatabase::open(
            state_path,
            cx.background_executor().clone(),
        );
        cx.set_global(crate::app::tool_layout::ApplicationToolLayout(
            crate::app::tool_layout::ToolLayoutStore::open(&state),
        ));
        crate::app::window_sessions::install(&state, cx);
        cx.set_global(crate::app::command_history::ApplicationCommandHistory(
            crate::app::command_history::CommandHistory::open(&state),
        ));
        cx.set_global(crate::app::record_store::ApplicationState(state));
        cx.set_global(ApplicationLaunchGate(RefCell::new(LaunchGate::Pending(
            Vec::new(),
        ))));
        cx.set_global(ApplicationFileSystems(product_filesystems()));
        let commands = cx.new(ProductCommandDispatcher::new);
        cx.set_global(ApplicationProductCommands(commands));
        let extension_host = crate::app::extensions::host::install(cx);
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
                        "New Blank Window",
                        ProductCommandSource::new(NEW_WINDOW_COMMAND),
                    ),
                    MenuItem::action("Open…", ProductCommandSource::new(OPEN_COMMAND)),
                    MenuItem::action(
                        "Add Folder to Workspace…",
                        ProductCommandSource::new(commands::ADD_FOLDER_COMMAND),
                    ),
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
        let extensions_root = crate::app::extensions::package::user_extensions_root();
        extension_host.update(cx, |host, cx| {
            host.start_product_startup(None, extensions_root, cx)
        });
    });
}

pub(in crate::app) fn bind_product_keys(cx: &mut App) {
    crate::app::commands::keymaps::install(cx);
}
