use super::*;
use gpui::TestAppContext;
use std::{
    future::Future,
    path::Path,
    time::{Duration, Instant},
};

fn host(root: &Path, cx: &mut TestAppContext) -> Entity<ProductExtensionHost> {
    cx.update(|cx| {
        let commands = cx.new(super::super::product_commands::ProductCommandDispatcher::new);
        cx.set_global(ApplicationProductCommands(commands));
        let host = install(cx);
        host.update(cx, |host, cx| {
            host.set_test_storage_root(root.to_owned(), cx);
        });
        host
    })
}

fn finish<F: Future + 'static>(future: F, cx: &mut TestAppContext) -> F::Output {
    let (sender, receiver) = std::sync::mpsc::channel();
    cx.spawn(async move |_| {
        let _ = sender.send(future.await);
    })
    .detach();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        match receiver.try_recv() {
            Ok(value) => return value,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(error) => panic!("completion dropped: {error}"),
        }
        assert!(Instant::now() < deadline, "storage JS did not finish");
        std::thread::yield_now();
    }
}

fn write_package(root: &Path, name: &str, source: &str) {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("knot.jsonc"),
        serde_json::json!({ "name": name, "apiVersion": 1, "version": "1.0.0", "main": "main.js" })
            .to_string(),
    )
    .unwrap();
    std::fs::write(directory.join("main.js"), source).unwrap();
}

fn startup(host: &Entity<ProductExtensionHost>, root: &Path, cx: &mut TestAppContext) {
    let completed = host.update(cx, |host, cx| {
        host.start_installed_extensions_with_completion(root.to_owned(), false, cx)
            .unwrap()
    });
    finish(completed, cx).unwrap();
    cx.read(|cx| {
        let report = host.read(cx).startup_report.as_ref().unwrap();
        assert!(!report.report.entries.is_empty());
        for entry in &report.report.entries {
            assert!(
                matches!(entry.result, LoadResult::Loaded),
                "{}: {:?}",
                entry.directory.display(),
                entry.result
            );
        }
        assert!(cx.windows().is_empty(), "activation requires no window");
    });
}

#[gpui::test]
fn installed_storage_is_usable_during_activation_and_survives_reload(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    let packages = root.path().join("extensions");
    let data = root.path().join("storage");
    write_package(
        &packages,
        "@example/storage-demo",
        include_str!("../../../examples/extensions/@example/storage-demo/dist/main.js"),
    );
    write_package(
        &packages,
        "@other/storage-demo",
        r#"
        import { storage } from 'knot';
        if ((await storage.read('example')).found) throw Error('owner leaked');
        const previous = await storage.read('activations');
        await storage.write('activations', previous.found ? previous.value + 1 : 1);
    "#,
    );
    let host = host(&data, cx);
    startup(&host, &packages, cx);
    startup(&host, &packages, cx);
    let database = data.join("packages/@example/storage-demo.sqlite3");
    let count = || {
        let connection = rusqlite::Connection::open(&database).unwrap();
        let json: String = connection
            .query_row(
                "SELECT value FROM records WHERE key = 'activations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&json)
            .unwrap()
            .as_f64()
            .unwrap()
    };
    assert_eq!(count(), 2.0);
    cx.update(|cx| {
        drop(cx.remove_global::<ApplicationExtensionHost>());
    });
    drop(host);
    cx.run_until_parked();
    // A new application host and relocated installed directory use the same owner DB.
    let relocated = root.path().join("relocated");
    std::fs::rename(&packages, &relocated).unwrap();
    let host = self::host(&data, cx);
    startup(&host, &relocated, cx);
    assert_eq!(count(), 3.0);
}

#[gpui::test]
fn storage_json_validation_and_failed_writes_cross_the_product_host(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    let packages = root.path().join("extensions");
    let data = root.path().join("storage");
    write_package(
        &packages,
        "@example/validation",
        r#"
        import { storage, workbench } from 'knot';
        const cyclic = {}; cyclic.self = cyclic;
        const sparse = []; sparse.length = 1;
        const extra = [1]; extra.extra = 2;
        let conversions = 0;
        const hook = { toJSON() { conversions++; return 1; } };
        const invalid = [undefined, NaN, Infinity, 1n, () => {}, Symbol(), new Date(),
          cyclic, sparse, extra, hook, { field: undefined }, { [Symbol()]: 1 }, new (class {})()];
        for (const value of invalid) {
          let failed = false;
          try { await storage.write('invalid', value); } catch (error) { if (!(error instanceof TypeError)) throw error; failed = true; }
          if (!failed) throw Error('invalid JSON accepted');
        }
        if (conversions !== 0) throw Error('toJSON invoked');
        for (const key of [undefined, null, 1, {}]) {
          for (const operation of [() => storage.read(key), () => storage.write(key, 1), () => storage.delete(key)]) {
            let failed = false;
            try { await operation(); } catch (error) { if (!(error instanceof TypeError)) throw error; failed = true; }
            if (!failed) throw Error('invalid key accepted');
          }
        }
        if ((await storage.list()).length !== 0) throw Error('invalid input mutated DB');
        const value = { items: [1, null, true, 'λ'], nested: { ok: true } };
        const pending = storage.write('b', value);
        value.items[0] = 9;
        await pending;
        const copied = await storage.read('b');
        if (!Object.isFrozen(copied) || !Object.isFrozen(copied.value.items) || copied.value.items[0] !== 1) throw Error('record was not copied');
        await storage.write('a', 1);
        await storage.delete('a');
        await storage.write('a', null);
        await storage.write('', false);
        await storage.delete('absent');
        const listed = await storage.list();
        if (!Object.isFrozen(listed) || listed.join(',') !== ',a,b') throw Error('key order changed');
        const nullable = await storage.read('a');
        const missing = await storage.read('missing');
        if (!nullable.found || nullable.value !== null || missing.found || 'value' in missing) throw Error('null/missing lost');
        await storage.write('a', 42);
        await storage.delete('b');
        if ((await storage.read('a')).value !== 42 || (await storage.read('b')).found) throw Error('replace/delete failed');
        await workbench.writeClipboardText('ready');

        let failed = false;
        try {
          await storage.write('a', 43);
        } catch (error) {
          if (error.name !== 'KnotHostError' || error.code !== 'StorageUnavailable' ||
              !error.details.database.endsWith('validation.sqlite3') ||
              !error.details.cause.includes('injected storage failure')) throw error;
          failed = true;
        }
        if (!failed || (await storage.read('a')).value !== 42) throw Error('failed write changed the record');
    "#,
    );
    let host = host(&data, cx);
    host.update(cx, |host, _| host.hold_clipboard_writes = true);
    let completed = host.update(cx, |host, cx| {
        host.start_installed_extensions_with_completion(packages, false, cx)
            .unwrap()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| !host.read(cx).deferred_view_requests.is_empty()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not reach transaction boundary"
        );
        std::thread::yield_now();
    }
    let database = data.join("packages/@example/validation.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_write BEFORE INSERT ON records WHEN NEW.key = 'a' BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END").unwrap();
    host.update(cx, |host, cx| {
        host.hold_clipboard_writes = false;
        for request in std::mem::take(&mut host.deferred_view_requests) {
            host.handle_request(request, cx);
        }
    });
    finish(completed, cx).unwrap();
    cx.read(|cx| {
        assert!(matches!(
            host.read(cx)
                .startup_report
                .as_ref()
                .unwrap()
                .report
                .entries[0]
                .result,
            LoadResult::Loaded
        ));
    });
    connection
        .execute_batch("DROP TRIGGER reject_write")
        .unwrap();
}

#[gpui::test]
fn personal_config_storage_has_a_separate_stable_owner(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    let packages = root.path().join("extensions");
    write_package(
        &packages,
        "@example/config",
        "import { storage } from 'knot'; await storage.write('owner', 'package');",
    );
    let host = host(&root.path().join("storage"), cx);
    startup(&host, &packages, cx);
    for generation in 1..=2 {
        let key = ExtensionKey::new(ExtensionId::new(901), ExtensionLifecycleId::new(generation));
        let graph = ModuleGraph::with_entries("file:///config/", &["pre-init.js", "post-init.js"], BTreeMap::from([
            ("pre-init.js".into(), Arc::from(format!(r#"
                import {{ storage }} from 'knot';
                const owner = await storage.read('owner');
                if (owner.found && owner.value !== 'personal') throw Error('package leaked into config');
                const prior = await storage.read('count');
                if ((prior.found ? prior.value : 0) !== {}) throw Error('config reload lost data');
                await storage.write('owner', 'personal');
                await storage.write('count', {});
            "#, generation - 1, generation))),
            ("post-init.js".into(), Arc::from(format!(r#"
                import {{ storage }} from 'knot';
                if ((await storage.read('count')).value !== {}) throw Error('phases do not share storage');
            "#, generation))),
        ])).unwrap();
        host.update(cx, |host, _| host.load_personal_config(key, graph).unwrap());
        for phase in [HostConfigPhase::PreInit, HostConfigPhase::PostInit] {
            let execution = cx.read(|cx| {
                host.read(cx)
                    .evaluate_personal_config_phase(key, phase)
                    .unwrap()
            });
            finish(execution, cx).unwrap();
        }
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
    }
    assert!(root.path().join("storage/personal-config.sqlite3").exists());
}

#[gpui::test]
fn storage_failure_is_structured_and_does_not_replace_corrupt_data(cx: &mut TestAppContext) {
    let root = tempfile::tempdir().unwrap();
    let packages = root.path().join("extensions");
    let data = root.path().join("storage");
    let bad = data.join("packages/@example/broken.sqlite3");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, b"keep corrupt database").unwrap();
    write_package(
        &packages,
        "@example/broken",
        r#"
        import { storage } from 'knot';
        let failed = false;
        try { await storage.write('x', 1); } catch (error) {
          if (error.name !== 'KnotHostError' || error.code !== 'StorageUnavailable' ||
              !error.details.database.endsWith('broken.sqlite3') || typeof error.details.cause !== 'string') throw error;
          failed = true;
        }
        if (!failed) throw Error('corruption did not fail');
    "#,
    );
    write_package(
        &packages,
        "@example/healthy",
        "import { storage } from 'knot'; await storage.write('ok', true);",
    );
    let host = host(&data, cx);
    startup(&host, &packages, cx);
    assert_eq!(std::fs::read(bad).unwrap(), b"keep corrupt database");
    assert!(data.join("packages/@example/healthy.sqlite3").exists());
}
