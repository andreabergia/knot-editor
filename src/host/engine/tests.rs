fn test_failure(code: &'static str) -> HostFailure {
    HostFailure {
        code,
        message: code.into(),
        details: serde_json::Value::Null,
    }
}

use crate::app::declared_host as declared;
use crate::host::declarations::{DeclaredRequest, Handler, Responder};
fn operation<H: Handler>(args: H::Arguments) -> DeclaredRequest {
    DeclaredRequest::new::<H>(args)
}
fn success<H: Handler>(value: H::Success) -> super::super::declarations::DeclaredResponse {
    Responder::<H>::new().respond(Ok(value))
}

fn keybindings_applied(error: Option<String>) -> super::super::declarations::DeclaredResponse {
    Responder::<declared::mutate_keybindings>::new().respond(error.map_or(Ok(()), |message| {
        Err(declared::KeymapError::invalid(message))
    }))
}

fn clipboard_operation(text: &str) -> DeclaredRequest {
    struct Reader(String);
    impl super::super::declarations::ArgumentReader for Reader {
        fn optional_integer(
            &mut self,
            _: usize,
        ) -> Result<Option<u64>, super::super::declarations::DecodeError> {
            unreachable!()
        }
        fn command_arguments(
            &mut self,
            _: usize,
        ) -> Result<CommandArgumentValue, super::super::declarations::DecodeError> {
            unreachable!()
        }
        fn outcome(
            &mut self,
            _: usize,
        ) -> Result<CommandOutcome, super::super::declarations::DecodeError> {
            unreachable!()
        }

        fn range(
            &mut self,
            _: usize,
        ) -> Result<Option<ByteRange>, super::super::declarations::DecodeError> {
            unreachable!()
        }
        fn edits(
            &mut self,
            _: usize,
        ) -> Result<Vec<TextEdit>, super::super::declarations::DecodeError> {
            unreachable!()
        }
        fn contributions(
            &mut self,
            _: usize,
        ) -> Result<Vec<EditorContribution>, super::super::declarations::DecodeError> {
            unreachable!()
        }

        fn optional_text(
            &mut self,
            _: usize,
            _: &'static str,
        ) -> Result<Option<String>, super::super::declarations::DecodeError> {
            unreachable!()
        }
        fn text_list(
            &mut self,
            _: usize,
        ) -> Result<Vec<String>, super::super::declarations::DecodeError> {
            unreachable!()
        }

        fn integer(&mut self, _: usize) -> Result<u64, super::super::declarations::DecodeError> {
            unreachable!()
        }

        fn text(
            &mut self,
            _: usize,
            _: &'static str,
        ) -> Result<String, super::super::declarations::DecodeError> {
            Ok(self.0.clone())
        }
        fn destination(
            &mut self,
            _: usize,
        ) -> Result<
            Option<super::super::protocol::DocumentDestination>,
            super::super::declarations::DecodeError,
        > {
            unreachable!()
        }
    }
    super::super::declarations::registry()
        .get("writeClipboardText")
        .unwrap()
        .decode(&mut Reader(text.into()))
        .unwrap()
}

fn clipboard_written() -> super::super::declarations::DeclaredResponse {
    super::super::declarations::DeclaredResponse {
        operation: "writeClipboardText",
        result: Ok(Ok(super::super::declarations::EncodedValue::Undefined)),
    }
}

use super::*;
use std::{
    collections::{BTreeMap, HashSet},
    num::NonZeroUsize,
    process::Command,
    sync::mpsc,
    time::{Duration, Instant},
};

use crate::host::lifecycle::ExtensionState;
use crate::host::protocol::{
    BufferHandle, ExtensionId, ExtensionLifecycleId, HostResponse, RequestId,
};

fn key(extension: u64) -> ExtensionKey {
    ExtensionKey::new(ExtensionId::new(extension), ExtensionLifecycleId::new(1))
}

fn package_graph(root: &str, entry: &str, files: &[(&str, &str)]) -> ModuleGraph {
    ModuleGraph::new(
        root,
        entry,
        files
            .iter()
            .map(|(path, source)| ((*path).into(), Arc::from(*source)))
            .collect(),
    )
    .unwrap()
}

fn config_graph(entries: &[&str], files: &[(&str, &str)]) -> ModuleGraph {
    ModuleGraph::with_entries(
        "file:///config/knot/",
        entries,
        files
            .iter()
            .map(|(path, source)| ((*path).into(), Arc::from(*source)))
            .collect(),
    )
    .unwrap()
}

#[test]
fn keybinding_calls_are_void_and_one_turn_sends_one_ordered_batch() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(180);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/keybindings.js",
            r#"
                import * as knot from "knot";
                if (knot.keybinding("Cmd-C", "copy") !== undefined) throw new Error("set returned a value");
                if (knot.keybinding("Cmd-C", { name: "copy", arguments: { format: "plain" } }, { view: "editor" }) !== undefined) throw new Error("set returned a value");
                if (knot.removeKeybinding("Cmd-C", { view: "editor" }) !== undefined) throw new Error("remove returned a value");
            "#,
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    let mutations = &request
        .operation
        .arguments::<declared::mutate_keybindings>()
        .expect("keybinding batch")
        .mutations;
    assert_eq!(mutations.len(), 3);
    assert!(
        matches!(&mutations[0], KeybindingMutation::Set { key, view: None, command: Some(command) } if key == "Cmd-C" && command.name.as_ref() == "copy")
    );
    assert!(
        matches!(&mutations[1], KeybindingMutation::Set { view: Some(view), command: Some(command), .. } if view == "editor" && command.arguments != CommandArgumentValue::Null)
    );
    assert!(
        matches!(&mutations[2], KeybindingMutation::Remove { view: Some(view), .. } if view == "editor")
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(keybindings_applied(None)),
    })
    .unwrap();
    execution.wait().unwrap();
    assert!(
        pool.receive_request_timeout(Duration::from_millis(20))
            .is_none()
    );
    pool.shutdown();
}

#[test]
fn keybinding_batch_settles_before_await_continuation_and_rejects_turn_errors() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(181);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/keybinding-await.js",
            r#"
                import * as knot from "knot";
                knot.keybinding("cmd-a", "file.new");
                await knot.workbench.writeClipboardText("wait");
                knot.keybinding("cmd-b", "file.open");
            "#,
        )
        .unwrap();
    let first = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    let second = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    let (host_request, batch) = if first.operation.descriptor.name == "writeClipboardText" {
        (first, second)
    } else {
        (second, first)
    };
    assert!(
        batch
            .operation
            .arguments::<declared::mutate_keybindings>()
            .is_some()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: host_request.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    assert!(
        pool.receive_request_timeout(Duration::from_millis(20))
            .is_none()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: batch.id,
        result: Ok(keybindings_applied(None)),
    })
    .unwrap();
    let next = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(
        matches!(next.operation.arguments::<declared::mutate_keybindings>(), Some(declared::KeybindingArguments { mutations }) if matches!(&mutations[..], [KeybindingMutation::Set { key, .. }] if key == "cmd-b"))
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: next.id,
        result: Ok(keybindings_applied(Some("invalid view kind".into()))),
    })
    .unwrap();
    let error = execution.wait().unwrap_err();
    assert!(error.message().contains("invalid view kind"));
    pool.shutdown();
}

#[test]
fn invalid_keybinding_input_is_queued_for_batch_diagnostics() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(182);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/invalid-keybinding.js",
            r#"
                import * as knot from "knot";
                knot.keybinding("cmd-r", "file.open");
                if (knot.keybinding("cmd-n", 42) !== undefined) throw new Error("invalid call returned a value");
            "#,
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(
        matches!(request.operation.arguments::<declared::mutate_keybindings>(), Some(declared::KeybindingArguments { mutations })
        if matches!(&mutations[..], [KeybindingMutation::Set { .. }, KeybindingMutation::Invalid(message)]
            if message.contains("command must be a string or object")))
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(keybindings_applied(Some(
            "keybinding operation 2: command must be a string or object".into(),
        ))),
    })
    .unwrap();
    let error = execution.wait().unwrap_err();
    assert_eq!(
        error.source(),
        Some("file:///fixtures/invalid-keybinding.js")
    );
    assert!(error.message().contains("operation 2"));
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/invalid-binding-arguments.js",
            r#"
                import * as knot from "knot";
                knot.keybinding("cmd-r", { name: "file.open", arguments: { marker: Infinity } });
            "#,
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(
        matches!(request.operation.arguments::<declared::mutate_keybindings>(), Some(declared::KeybindingArguments { mutations })
        if matches!(&mutations[..], [KeybindingMutation::Invalid(message)]
            if message.contains("finite numbers")))
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(keybindings_applied(Some(
            "keybinding operation 1: command arguments require finite numbers".into(),
        ))),
    })
    .unwrap();
    assert!(
        execution
            .wait()
            .unwrap_err()
            .message()
            .contains("finite numbers")
    );
    pool.shutdown();
}

#[test]
fn personal_config_phases_share_one_awaited_lifecycle() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let mut inbox = pool.take_event_inbox().unwrap();
    let runtime = key(90);
    let graph = config_graph(
        &["pre-init.js", "post-init.js"],
        &[
            (
                "counter.mjs",
                "export let count = 0; export function increment() { count++; }",
            ),
            (
                "pre-init.js",
                "import * as knot from 'knot'; const { commands } = knot; import { increment } from './counter.mjs'; globalThis.phaseOrder = ['pre']; increment(); await Promise.resolve(); globalThis.configCommand = await commands.register('knot.config.test', () => {});",
            ),
            (
                "post-init.js",
                "import { count, increment } from './counter.mjs'; if (count !== 1 || !globalThis.configCommand) throw new Error('lost pre-init state'); increment(); await Promise.resolve(); globalThis.phaseOrder.push('post');",
            ),
        ],
    );
    pool.load_personal_config(runtime, IsolateConfig::default(), graph)
        .unwrap();
    let pre = pool
        .evaluate_personal_config_phase(runtime, ConfigPhase::PreInit)
        .unwrap();
    let Some(EngineEvent::Request(request)) = inbox.receive_timeout(Duration::from_secs(1)) else {
        panic!("personal config registration request was not received");
    };
    assert_eq!(
        request.operation,
        operation::<declared::register_command>(declared::RegisterCommandArguments {
            name: "knot.config.test".into(),
            title: "knot.config.test".into(),
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::register_command>(
            CommandRegistrationId::new(100),
        )),
    })
    .unwrap();
    pre.wait().unwrap();
    let extension = key(94);
    pool.load_package(
        extension,
        IsolateConfig::default(),
        package_graph(
            "file:///extensions/%40example/during-config/",
            "main.js",
            &[("main.js", "globalThis.extensionStarted = true;")],
        ),
    )
    .unwrap()
    .wait()
    .unwrap();
    pool.evaluate_personal_config_phase(runtime, ConfigPhase::PostInit)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(
        pool.execute(runtime, "verify.js", "phaseOrder.join(',')")
            .unwrap()
            .wait()
            .unwrap()
            .value
            .as_ref(),
        "pre,post"
    );
    pool.unload(runtime).unwrap();
    assert!(matches!(
        inbox.receive_timeout(Duration::from_secs(1)),
        Some(EngineEvent::LifecycleEnded { key: ended, reason: LifecycleEnd::Unloaded }) if ended == runtime
    ));
    assert!(
        pool.evaluate_personal_config_phase(runtime, ConfigPhase::PostInit)
            .is_err()
    );
    pool.unload(extension).unwrap();
    pool.shutdown();
}

#[test]
fn personal_config_runtime_blocks_early_post_init_import() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(95);
    let graph = config_graph(
        &["pre-init.js", "post-init.js"],
        &[
            ("pre-init.js", "import './helper.js';"),
            ("helper.js", "import './post-init.js';"),
            ("post-init.js", "globalThis.startedEarly = true;"),
        ],
    );
    pool.load_personal_config(runtime, IsolateConfig::default(), graph)
        .unwrap();
    let error = pool
        .evaluate_personal_config_phase(runtime, ConfigPhase::PreInit)
        .unwrap()
        .wait()
        .unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::ModuleResolution);
    assert_eq!(error.source(), Some("file:///config/knot/helper.js"));
    assert_eq!(error.line(), Some(1));
    pool.unload(runtime).unwrap();
    pool.shutdown();
}

#[test]
fn personal_config_phase_errors_keep_their_source_and_can_be_unloaded() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    for (index, failing_phase) in [ConfigPhase::PreInit, ConfigPhase::PostInit]
        .into_iter()
        .enumerate()
    {
        let runtime = key(91 + index as u64);
        let graph = config_graph(
            &["pre-init.js", "post-init.js"],
            &[
                (
                    "pre-init.js",
                    if failing_phase == ConfigPhase::PreInit {
                        "\nthrow new Error('pre boom');"
                    } else {
                        "globalThis.ready = true;"
                    },
                ),
                ("post-init.js", "\nthrow new Error('post boom');"),
            ],
        );
        pool.load_personal_config(runtime, IsolateConfig::default(), graph)
            .unwrap();
        let pre = pool
            .evaluate_personal_config_phase(runtime, ConfigPhase::PreInit)
            .unwrap()
            .wait();
        let error = if failing_phase == ConfigPhase::PreInit {
            pre.unwrap_err()
        } else {
            pre.unwrap();
            pool.evaluate_personal_config_phase(runtime, ConfigPhase::PostInit)
                .unwrap()
                .wait()
                .unwrap_err()
        };
        assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
        assert_eq!(error.line(), Some(2));
        assert_eq!(
            error.source(),
            Some(if failing_phase == ConfigPhase::PreInit {
                "file:///config/knot/pre-init.js"
            } else {
                "file:///config/knot/post-init.js"
            })
        );
        pool.unload(runtime).unwrap();
    }
    pool.shutdown();
}

#[test]
fn personal_config_can_have_only_post_init() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(93);
    let graph = config_graph(
        &["post-init.js"],
        &[(
            "post-init.js",
            "await Promise.resolve(); globalThis.postOnly = true;",
        )],
    );
    pool.load_personal_config(runtime, IsolateConfig::default(), graph)
        .unwrap();
    assert!(
        pool.evaluate_personal_config_phase(runtime, ConfigPhase::PreInit)
            .is_err()
    );
    pool.evaluate_personal_config_phase(runtime, ConfigPhase::PostInit)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(
        pool.execute(runtime, "verify.js", "postOnly")
            .unwrap()
            .wait()
            .unwrap()
            .value
            .as_ref(),
        "true"
    );
    pool.unload(runtime).unwrap();
    pool.shutdown();
}

#[test]
fn package_modules_support_local_imports_and_top_level_await() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(50);
    let graph = package_graph(
        "file:///extensions/%40example/tools/",
        "dist/main.js",
        &[
            (
                "dist/main.js",
                "import * as knot from 'knot'; const { editor } = knot; import { value } from './helper.js'; import { suffix } from '../suffix.mjs'; await Promise.resolve(); globalThis.packageValue = value + suffix + ':' + typeof editor.activeBuffer;",
            ),
            ("dist/helper.js", "export const value = 'ready';"),
            ("suffix.mjs", "export const suffix = '!';"),
        ],
    );
    pool.load_package(runtime, IsolateConfig::default(), graph)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(
        &*pool
            .execute(runtime, "verify.js", "packageValue")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "ready!:function"
    );
    let changed = pool
        .execute_fixture_module(
            runtime,
            "file:///extensions/%40example/tools/other.js",
            "export {};",
        )
        .unwrap()
        .wait()
        .unwrap_err();
    assert!(changed.message().contains("cannot change after load"));
    pool.unload(runtime).unwrap();
    pool.shutdown();
}

#[test]
fn package_module_sources_are_isolated_per_lifecycle() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let first = key(51);
    let second = key(52);
    for (runtime, root, value) in [
        (first, "file:///extensions/%40example/first/", "first"),
        (second, "file:///extensions/%40example/second/", "second"),
    ] {
        let graph = package_graph(
            root,
            "main.js",
            &[
                (
                    "main.js",
                    "import { value } from './shared.js'; globalThis.packageValue = value;",
                ),
                (
                    "shared.js",
                    if value == "first" {
                        "export const value = 'first';"
                    } else {
                        "export const value = 'second';"
                    },
                ),
            ],
        );
        pool.load_package(runtime, IsolateConfig::default(), graph)
            .unwrap()
            .wait()
            .unwrap();
    }
    assert_eq!(
        &*pool
            .execute(first, "check.js", "packageValue")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "first"
    );
    assert_eq!(
        &*pool
            .execute(second, "check.js", "packageValue")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "second"
    );
    pool.unload(first).unwrap();
    pool.unload(second).unwrap();
    pool.shutdown();
}

#[test]
fn package_module_imports_stay_within_the_supplied_graph() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    for (index, (source, expected)) in [
        ("import 'bare-package';", "relative or knot"),
        ("import '../other/secret.js';", "escapes package"),
        (
            "import 'file:///extensions/%40example/other/secret.js';",
            "relative or knot",
        ),
        ("import './missing.js';", "package module not found"),
        ("import 'knot:bootstrap';", "not importable"),
        ("import 'knot:editor';", "relative or knot"),
    ]
    .into_iter()
    .enumerate()
    {
        let runtime = key(60 + index as u64);
        let graph = package_graph(
            "file:///extensions/%40example/tools/",
            "main.js",
            &[("main.js", source)],
        );
        let failure = pool
            .load_package(runtime, IsolateConfig::default(), graph)
            .unwrap()
            .wait()
            .unwrap_err();
        assert_eq!(failure.kind(), RuntimeErrorKind::ModuleResolution);
        assert!(failure.message().contains(expected), "{failure}");
        assert_eq!(
            failure.source(),
            Some("file:///extensions/%40example/tools/main.js")
        );
        assert_eq!(failure.line(), Some(1));
        assert!(failure.column().is_some());
        pool.unload(runtime).unwrap();
    }
    pool.shutdown();
}

#[test]
fn package_module_errors_report_generated_locations_and_dynamic_import_rejection() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let root = "file:///extensions/%40example/tools/";
    let runtime = key(70);
    let graph = package_graph(
        root,
        "main.js",
        &[
            ("main.js", "import './bad.js';"),
            ("bad.js", "export const = 1;"),
        ],
    );
    let syntax = pool
        .load_package(runtime, IsolateConfig::default(), graph)
        .unwrap()
        .wait()
        .unwrap_err();
    assert_eq!(syntax.kind(), RuntimeErrorKind::Compilation);
    assert_eq!(
        syntax.source(),
        Some("file:///extensions/%40example/tools/bad.js")
    );
    assert_eq!(syntax.line(), Some(1));
    assert!(syntax.column().is_some());
    pool.unload(runtime).unwrap();

    let runtime = key(71);
    let graph = package_graph(
        root,
        "main.js",
        &[("main.js", "\nthrow new Error('package boom');")],
    );
    let thrown = pool
        .load_package(runtime, IsolateConfig::default(), graph)
        .unwrap()
        .wait()
        .unwrap_err();
    assert_eq!(thrown.kind(), RuntimeErrorKind::Rejection);
    assert_eq!(
        thrown.source(),
        Some("file:///extensions/%40example/tools/main.js")
    );
    assert_eq!(thrown.line(), Some(2));
    assert!(thrown.column().is_some());
    pool.unload(runtime).unwrap();

    let runtime = key(72);
    let graph = package_graph(
        root,
        "main.js",
        &[
            ("main.js", "await import('./other.js');"),
            ("other.js", "export {};"),
        ],
    );
    let dynamic = pool
        .load_package(runtime, IsolateConfig::default(), graph)
        .unwrap()
        .wait()
        .unwrap_err();
    assert!(
        dynamic
            .message()
            .contains("dynamic imports are unavailable"),
        "{dynamic}"
    );
    pool.unload(runtime).unwrap();

    let runtime = key(73);
    let graph = package_graph(
        root,
        "main.js",
        &[
            ("main.js", "import './child.js';"),
            ("child.js", "\nthrow new Error('child boom');"),
        ],
    );
    let child = pool
        .load_package(runtime, IsolateConfig::default(), graph)
        .unwrap()
        .wait()
        .unwrap_err();
    assert_eq!(
        child.source(),
        Some("file:///extensions/%40example/tools/child.js")
    );
    assert_eq!(child.line(), Some(2));
    assert!(child.column().is_some());
    pool.unload(runtime).unwrap();
    pool.shutdown();
}

#[test]
fn failed_package_entry_can_be_rolled_back_and_pending_entry_unloaded() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let mut inbox = pool.take_event_inbox().unwrap();
    let root = "file:///extensions/%40example/tools/";
    let runtime = key(80);
    let graph = package_graph(
        root,
        "main.js",
        &[("main.js", "throw new Error('failed entry');")],
    );
    let failed = pool
        .load_package(runtime, IsolateConfig::default(), graph)
        .unwrap()
        .wait()
        .unwrap_err();
    assert!(failed.message().contains("failed entry"));
    pool.unload(runtime).unwrap();
    assert!(
        matches!(inbox.receive_timeout(Duration::from_secs(2)), Some(EngineEvent::LifecycleEnded {
        key: ended,
        reason: LifecycleEnd::Unloaded,
    }) if ended == runtime)
    );

    let runtime = key(81);
    let graph = package_graph(
        root,
        "main.js",
        &[("main.js", "await new Promise(() => {});")],
    );
    let pending = pool
        .load_package(runtime, IsolateConfig::default(), graph)
        .unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    pool.unload(runtime).unwrap();
    assert_eq!(
        pending.wait().unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );
    assert!(
        matches!(inbox.receive_timeout(Duration::from_secs(2)), Some(EngineEvent::LifecycleEnded {
        key: ended,
        reason: LifecycleEnd::Unloaded,
    }) if ended == runtime)
    );
    pool.shutdown();
}

fn wait_for_state(pool: &RuntimePool, key: ExtensionKey, expected: ExtensionState) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = pool.scheduler_handle.state(key).unwrap();
        if state == expected {
            return;
        }
        assert!(Instant::now() < deadline, "state remained {state:?}");
        std::thread::yield_now();
    }
}

fn execute_captured_fixture(
    pool: &RuntimePool,
    runtime: ExtensionKey,
    module: impl AsRef<str>,
    source: &str,
) -> RuntimeCommandExecution {
    let source = format!(
        "import * as knot from 'knot'; const {{ editor, commands }} = knot; await commands.register('fixture.setup', async () => {{\n{source}\n}});"
    );
    let setup = pool
        .execute_fixture_module(runtime, module.as_ref(), source.as_str())
        .unwrap();
    let registration_request = pool.receive_request().unwrap();
    let registration = CommandRegistrationId::new(900);
    assert!(
        registration_request
            .operation
            .arguments::<declared::register_command>()
            .is_some()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: registration_request.id,
        result: Ok(success::<declared::register_command>(registration)),
    })
    .unwrap();
    setup.wait().unwrap();
    pool.invoke_command(
        runtime,
        CommandInvocation {
            id: CommandInvocationId::new(900),
            registration,
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            arguments: CommandArgumentValue::Null,
            capture: Some(crate::host::protocol::CommandCapture {
                authority: CommandInvocationId::new(900),
                window: "fixture-window".into(),
                view: None,
                tool: None,
            }),
        },
        Some(BufferHandle::new(3)),
    )
    .unwrap()
}

fn install_buffer_listener(
    pool: &RuntimePool,
    runtime: ExtensionKey,
    subscription: BufferSubscriptionId,
    module: &str,
) {
    let execution = execute_captured_fixture(
        pool,
        runtime,
        module,
        r#"
            const buffer = await editor.activeBuffer();
            globalThis.events = [];
            await buffer.onDidChange(globalThis.listener);
        "#,
    );
    let active = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: active.id,
        result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
            3,
        )))),
    })
    .unwrap();
    let subscribe = pool.receive_request().unwrap();
    assert_eq!(
        subscribe.operation,
        operation::<declared::subscribe_buffer_changes>(declared::BufferArguments {
            buffer: BufferHandle::new(3)
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: subscribe.id,
        result: Ok(success::<declared::subscribe_buffer_changes>(subscription)),
    })
    .unwrap();
    assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
}

#[test]
fn facade_rejects_malformed_coordinates_revisions_and_json_before_transport() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(190);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = execute_captured_fixture(
        &pool,
        runtime,
        "file:///fixtures/validation.js",
        r#"
      const buffer = await editor.activeBuffer();
      async function expect(operation, name) {
        try { await operation(); } catch (error) { if (error.name === name) return; throw error; }
        throw new Error(`expected ${name}`);
      }
      for (const value of [undefined, null, "1", true, {}, 1n]) {
        await expect(() => buffer.applyEdits([], { ifRevision: value }), "TypeError");
        await expect(() => buffer.contributions.replace([], { ifRevision: value }), "TypeError");
      }
      for (const value of [-1, 1.5, Infinity, NaN, Number.MAX_SAFE_INTEGER + 1]) {
        await expect(() => buffer.applyEdits([], { ifRevision: value }), "RangeError");
        await expect(() => buffer.snapshot({ startByteOffset: 0, endByteOffset: value }), "RangeError");
      }
      await expect(() => buffer.applyEdits([]), "TypeError");
      await expect(() => buffer.contributions.replace([]), "TypeError");
      await expect(() => buffer.snapshot({ startByteOffset: 2, endByteOffset: 1 }), "RangeError");
      await expect(() => buffer.applyEdits([{ range: { startByteOffset: 0, endByteOffset: 0 }, text: 4 }], { ifRevision: 0 }), "TypeError");
      await expect(() => buffer.contributions.replace([{ range: { startByteOffset: 0, endByteOffset: 0 }, decoration: "other" }], { ifRevision: 0 }), "TypeError");
      const cyclic = {}; cyclic.self = cyclic;
      const custom = { toJSON() { return {}; } };
      const sparse = new Array(2);
      const symbolKey = { [Symbol()]: 1 };
      const extra = []; extra.hidden = () => {};
      for (const value of [undefined, () => {}, Symbol(), 1n, NaN, Infinity, new Date(), cyclic, custom, sparse, symbolKey, extra]) {
        // Undefined at the root defaults to null, so use it in a record.
        await expect(() => commands.invoke("never", { value }), "TypeError");
      }
      await expect(() => commands.invalidArguments(4), "TypeError");
      const snapshot = await buffer.snapshot();
      for (const value of ["0", {}, true, undefined]) {
        await expect(() => snapshot.byteOffsetAtUtf16(value), "TypeError");
        await expect(() => snapshot.utf16OffsetAtByte(value), "TypeError");
      }
      for (const value of [-1, 0.5, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
        await expect(() => snapshot.byteOffsetAtUtf16(value), "RangeError");
        await expect(() => snapshot.utf16OffsetAtByte(value), "RangeError");
      }
      Array.prototype[1] = 99;
      try { await expect(() => snapshot.byteOffsetAtUtf16(1), "RangeError"); }
      finally { delete Array.prototype[1]; }
      await expect(() => snapshot.utf16OffsetAtByte(1), "RangeError");
    "#,
    );
    let active = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: active.id,
        result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
            3,
        )))),
    })
    .unwrap();
    let snapshot = pool.receive_request().unwrap();
    assert!(
        snapshot
            .operation
            .arguments::<declared::snapshot>()
            .is_some()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: snapshot.id,
        result: Ok(success::<declared::snapshot>(TextSnapshot {
            text: SnapshotText::from_utf8("😀"),
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 4,
            },
            revision: 0,
        })),
    })
    .unwrap();
    assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
    assert!(
        pool.receive_request_timeout(Duration::from_millis(20))
            .is_none()
    );
    pool.shutdown();
}

#[test]
fn every_disposer_shares_native_acknowledgement_and_retains_failure() {
    for failed in [false, true] {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(191);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let setup = execute_captured_fixture(
            &pool,
            runtime,
            "file:///fixtures/owned.js",
            r#"
          const { workbench } = knot;
          globalThis.buffer = await editor.activeBuffer();
          globalThis.owned = [
            await commands.register("owned", () => { throw new Error("disposed command ran"); }),
            await buffer.onDidChange(() => { throw new Error("disposed listener ran"); }),
            buffer.contributions,
            await workbench.registerView({ id: "example.outline", title: "Outline", content: { kind: "tree", provider: { getChildren() { throw new Error("disposed tree ran"); } } } }),
            await editor.registerCompletionProvider("owned", { provideCompletions() { throw new Error("disposed completion ran"); } }),
          ];
        "#,
        );
        let responses = [
            success::<declared::active_buffer>(Some(BufferHandle::new(3))),
            success::<declared::register_command>(CommandRegistrationId::new(901)),
            success::<declared::subscribe_buffer_changes>(BufferSubscriptionId::new(902)),
            success::<declared::register_tree_provider>(TreeProviderRegistrationId::new(903)),
            success::<declared::register_completion_provider>(
                CompletionProviderRegistrationId::new(904),
            ),
        ];
        for value in responses {
            let request = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(value),
            })
            .unwrap();
        }
        assert_eq!(setup.wait().unwrap(), CommandOutcome::Completed);
        let disposal = pool.execute(runtime, "dispose.js", r#"
          (async () => {
            globalThis.cleanup = owned.map((owner) => {
              const promise = owner.dispose();
              if (promise !== owner.dispose()) throw new Error("cleanup promise changed");
              return promise;
            });
            for (const mutation of [() => buffer.contributions.replace([], { ifRevision: 0 }), () => owned[3].invalidate()]) {
              let inactive = false;
              try { await mutation(); } catch (error) { if (error.code !== "OwnershipDisposed") throw error; inactive = true; }
              if (!inactive) throw new Error("ownership still active");
            }
            globalThis.cleanupResults = await Promise.allSettled(cleanup);
            for (let index = 0; index < owned.length; index++) {
              if (owned[index].dispose() !== cleanup[index]) throw new Error("settled cleanup promise changed");
            }
          })()
        "#).unwrap();
        let operations = [
            (
                success::<declared::unregister_command>(()),
                test_failure("CommandNotFound"),
            ),
            (
                success::<declared::unsubscribe_buffer_changes>(()),
                test_failure("OwnershipDisposed"),
            ),
            (
                success::<declared::dispose_editor_contributions>(()),
                test_failure("BufferClosed"),
            ),
            (
                success::<declared::unregister_tree_provider>(()),
                test_failure("TreeProviderNotFound"),
            ),
            (
                success::<declared::unregister_completion_provider>(()),
                test_failure("CompletionProviderNotFound"),
            ),
        ];
        for (value, error) in operations {
            let request = pool.receive_request().unwrap();
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: if failed { Err(error) } else { Ok(value) },
            })
            .unwrap();
        }
        disposal.wait().unwrap();
        let expected = if failed {
            "rejected,rejected,rejected,rejected,rejected"
        } else {
            "fulfilled,fulfilled,fulfilled,fulfilled,fulfilled"
        };
        assert_eq!(
            &*pool
                .execute(
                    runtime,
                    "verify-cleanup.js",
                    "cleanupResults.map(result => result.status).join(',')"
                )
                .unwrap()
                .wait()
                .unwrap()
                .value,
            expected
        );
        assert!(
            pool.receive_request_timeout(Duration::from_millis(20))
                .is_none()
        );
        pool.dispatch_buffer_change(
            runtime,
            BufferSubscriptionId::new(902),
            BufferChange {
                buffer: BufferHandle::new(3),
                before_revision: 0,
                revision: 1,
                edits: vec![],
            },
        )
        .unwrap();
        pool.execute(runtime, "listener-drained.js", "42")
            .unwrap()
            .wait()
            .unwrap();
        let tree = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: TreeProviderRegistrationId::new(903),
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: None,
                    generation: 1,
                },
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(tree.result.unwrap_err().message.contains("disposed"));
        let completion = pool
            .request_completions(
                runtime,
                CompletionRequest {
                    registration: CompletionProviderRegistrationId::new(904),
                    buffer: BufferHandle::new(3),
                    revision: 0,
                    cursor_byte_offset: 0,
                    prefix: "".into(),
                    generation: 1,
                },
            )
            .unwrap()
            .wait()
            .unwrap();
        assert!(completion.result.unwrap_err().message.contains("disposed"));
        pool.shutdown();
    }
}

#[test]
fn invalidation_cleanup_waits_for_earlier_native_acknowledgements() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(192);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let setup = pool.execute_fixture_module(runtime, "file:///fixtures/ordering.js", r#"
      import { workbench } from "knot";
      globalThis.api = workbench;
      globalThis.tree = await workbench.registerView({ id: "example.outline", title: "Outline", content: { kind: "tree", provider: { getChildren() { return []; } } } });
    "#).unwrap();
    let request = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::register_tree_provider>(
            TreeProviderRegistrationId::new(905),
        )),
    })
    .unwrap();
    setup.wait().unwrap();
    let work = pool
        .execute(
            runtime,
            "ordered-disposal.js",
            r#"
      (async () => {
        const mutation = tree.invalidate("child");
        const cleanup = tree.dispose();
        const acknowledged = cleanup.then(() => api.writeClipboardText("cleanup acknowledged"));
        await Promise.all([mutation, cleanup, acknowledged]);
      })()
    "#,
        )
        .unwrap();
    let invalidate = pool.receive_request().unwrap();
    let dispose = pool.receive_request().unwrap();
    assert!(
        invalidate
            .operation
            .arguments::<declared::invalidate_tree_provider>()
            .is_some()
    );
    assert!(
        dispose
            .operation
            .arguments::<declared::unregister_tree_provider>()
            .is_some()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: dispose.id,
        result: Ok(success::<declared::unregister_tree_provider>(())),
    })
    .unwrap();
    assert!(
        pool.receive_request_timeout(Duration::from_millis(20))
            .is_none()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: invalidate.id,
        result: Ok(success::<declared::invalidate_tree_provider>(())),
    })
    .unwrap();
    let acknowledgement = pool.receive_request().unwrap();
    assert_eq!(
        acknowledgement.operation,
        clipboard_operation("cleanup acknowledged")
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: acknowledgement.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    work.wait().unwrap();
    pool.shutdown();
}

#[test]
fn native_integers_are_checked_before_v8_conversion() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(193);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let setup = execute_captured_fixture(
        &pool,
        runtime,
        "file:///fixtures/exact-number.js",
        r#"
      const buffer = await editor.activeBuffer();
      let rejected = false;
      try { await buffer.snapshot(); } catch (error) { if (error.code !== "UnsupportedOperation") throw error; rejected = true; }
      if (!rejected) throw new Error("rounded revision accepted");
    "#,
    );
    let request = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
            3,
        )))),
    })
    .unwrap();
    let request = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::snapshot>(TextSnapshot {
            text: SnapshotText::from_utf8(""),
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 0,
            },
            revision: MAX_JS_INTEGER + 1,
        })),
    })
    .unwrap();
    assert_eq!(setup.wait().unwrap(), CommandOutcome::Completed);
    pool.shutdown();
}

#[test]
fn provider_self_disposal_acknowledges_without_waiting_for_its_callback() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(194);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let setup = pool.execute_fixture_module(runtime, "file:///fixtures/self-disposal.js", r#"
      import { editor, workbench } from "knot";
      globalThis.treeCalls = 0;
      globalThis.completionCalls = 0;
      globalThis.tree = await workbench.registerView({ id: "example.outline", title: "Outline", content: { kind: "tree", provider: {
        async getChildren() { treeCalls++; await tree.dispose(); globalThis.treeReleased = true; return []; },
      } } });
      globalThis.completion = await editor.registerCompletionProvider("self", {
        async provideCompletions() { completionCalls++; await completion.dispose(); globalThis.completionReleased = true; return []; },
      });
    "#).unwrap();
    for value in [
        success::<declared::register_tree_provider>(TreeProviderRegistrationId::new(906)),
        success::<declared::register_completion_provider>(CompletionProviderRegistrationId::new(
            907,
        )),
    ] {
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(value),
        })
        .unwrap();
    }
    setup.wait().unwrap();
    let tree = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: TreeProviderRegistrationId::new(906),
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: None,
                generation: 1,
            },
        )
        .unwrap();
    let release = pool.receive_request().unwrap();
    assert!(
        release
            .operation
            .arguments::<declared::unregister_tree_provider>()
            .is_some()
    );
    let queued = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: TreeProviderRegistrationId::new(906),
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: None,
                generation: 2,
            },
        )
        .unwrap();
    drop(queued);
    // Native unregistration drops its receiver before replying to self-disposal.
    drop(tree);
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: release.id,
        result: Ok(success::<declared::unregister_tree_provider>(())),
    })
    .unwrap();
    assert_eq!(
        &*pool
            .execute(
                runtime,
                "verify-tree-self-disposal.js",
                "`${treeReleased}:${treeCalls}`"
            )
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "true:1"
    );
    let completion = pool
        .request_completions(
            runtime,
            CompletionRequest {
                registration: CompletionProviderRegistrationId::new(907),
                buffer: BufferHandle::new(3),
                revision: 0,
                cursor_byte_offset: 0,
                prefix: "".into(),
                generation: 1,
            },
        )
        .unwrap();
    let release = pool.receive_request().unwrap();
    assert!(
        release
            .operation
            .arguments::<declared::unregister_completion_provider>()
            .is_some()
    );
    drop(completion);
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: release.id,
        result: Ok(success::<declared::unregister_completion_provider>(())),
    })
    .unwrap();
    assert_eq!(
        &*pool
            .execute(
                runtime,
                "verify-completion-self-disposal.js",
                "`${completionReleased}:${completionCalls}`"
            )
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "true:1"
    );
    assert!(
        pool.capsule(runtime)
            .unwrap()
            .local_state
            .pending
            .lock()
            .unwrap()
            .is_empty()
    );
    pool.shutdown();
}

#[test]
fn provider_results_require_iterables_and_validate_only_recognized_item_fields() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(195);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let setup = pool.execute_fixture_module(runtime, "file:///fixtures/provider-data.js", r#"
      import { editor, workbench } from "knot";
      await workbench.registerView({ id: "example.outline", title: "Outline", content: { kind: "tree", provider: {
        getChildren(parent) {
          if (parent === "prototype") { Object.prototype.error = "ambient error"; Object.prototype.toJSON = () => { throw new Error("ambient toJSON evaluated"); }; }
          if (parent === "arraylike") return { 0: {}, length: 1 };
          if (parent === "string") return "children";
          if (parent === "missing") return [{ id: "id", label: "label" }];
          if (parent === "null-state") return [{ id: "id", label: "label", collapsibleState: null }];
          if (parent === "wrong-description") return [{ id: "id", label: "label", collapsibleState: "none", description: 4 }];
          if (parent === "bad-token") return [{ id: "id", label: "label", collapsibleState: "none", icon: "other" }];
          return new Set([{ id: "id", label: "label", collapsibleState: "none", unknown: () => {}, toJSON() { throw new Error("unknown item field evaluated"); } }]);
        },
      } } });
      await editor.registerCompletionProvider("data", {
        provideCompletions(context) {
          if (context.prefix === "arraylike") return { length: 1, 0: {} };
          if (context.prefix === "string") return "candidates";
          if (context.prefix === "missing") return [{ label: "label" }];
          return new Set([{ label: "label", insertText: "label", unknown: () => {}, toJSON() { throw new Error("unknown item field evaluated"); } }]);
        },
      });
    "#).unwrap();
    for value in [
        success::<declared::register_tree_provider>(TreeProviderRegistrationId::new(908)),
        success::<declared::register_completion_provider>(CompletionProviderRegistrationId::new(
            909,
        )),
    ] {
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(value),
        })
        .unwrap();
    }
    setup.wait().unwrap();
    for parent in [
        "arraylike",
        "string",
        "missing",
        "null-state",
        "wrong-description",
        "bad-token",
        "valid",
        "prototype",
    ] {
        let result = pool
            .request_tree_children(
                runtime,
                TreeChildrenRequest {
                    registration: TreeProviderRegistrationId::new(908),
                    window: "1".into(),
                    view: crate::host::protocol::ViewId::new(1),
                    parent_id: Some(parent.into()),
                    generation: 1,
                },
            )
            .unwrap()
            .wait()
            .unwrap()
            .result;
        assert_eq!(
            result.is_ok(),
            matches!(parent, "valid" | "prototype"),
            "{parent}: {result:?}"
        );
    }
    for prefix in ["arraylike", "string", "missing", "valid"] {
        let result = pool
            .request_completions(
                runtime,
                CompletionRequest {
                    registration: CompletionProviderRegistrationId::new(909),
                    buffer: BufferHandle::new(3),
                    revision: 0,
                    cursor_byte_offset: 0,
                    prefix: prefix.into(),
                    generation: 1,
                },
            )
            .unwrap()
            .wait()
            .unwrap()
            .result;
        assert_eq!(result.is_ok(), prefix == "valid", "{prefix}: {result:?}");
    }
    pool.execute(
        runtime,
        "restore-prototype.js",
        "delete Object.prototype.toJSON; delete Object.prototype.error",
    )
    .unwrap()
    .wait()
    .unwrap();
    pool.shutdown();
}

#[test]
fn registration_response_racing_unload_cannot_publish_late_ownership() {
    for operation in [
        "commands.register('late', () => {})",
        "editor.registerCompletionProvider('late', { provideCompletions() { return []; } })",
        "workbench.registerView({ id: 'example.outline', title: 'Outline', content: { kind: 'tree', provider: { getChildren() { return []; } } } })",
    ] {
        let pool = RuntimePool::new(PoolConfig::single_worker());
        let runtime = key(196);
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let source = format!(
            "import {{commands, editor, workbench}} from 'knot'; await {operation}; globalThis.published = true;"
        );
        let registration = pool
            .execute_fixture_module(runtime, "file:///fixtures/late-registration.js", source)
            .unwrap();
        let request = pool.receive_request().unwrap();
        // Request delivery precedes the worker yielding; unload cancels a
        // parked root but can terminate JavaScript that is still running.
        wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
        pool.unload(runtime).unwrap();
        assert_eq!(
            registration.wait().unwrap_err().kind(),
            RuntimeErrorKind::Cancelled
        );
        let value = match request.operation {
            operation
                if operation
                    .arguments::<declared::register_command>()
                    .is_some() =>
            {
                success::<declared::register_command>(CommandRegistrationId::new(1))
            }
            operation
                if operation
                    .arguments::<declared::register_completion_provider>()
                    .is_some() =>
            {
                success::<declared::register_completion_provider>(
                    CompletionProviderRegistrationId::new(1),
                )
            }
            operation
                if operation
                    .arguments::<declared::register_tree_provider>()
                    .is_some() =>
            {
                success::<declared::register_tree_provider>(TreeProviderRegistrationId::new(1))
            }
            _ => unreachable!(),
        };
        assert_eq!(
            pool.respond(HostResponse {
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                id: request.id,
                result: Ok(value)
            }),
            Err(RuntimeResponseError::WrongExtension)
        );
        pool.shutdown();
    }
}

#[test]
fn application_observation_installs_before_queued_delivery_and_self_disposes() {
    use super::super::protocol::{ApplicationSnapshot, WorkbenchObservation};
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(209);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let setup = pool.execute_fixture_module(runtime, "file:///fixtures/application-admission.js", r#"
import { documents, workbench } from "knot";
globalThis.calls = 0;
const observation = await workbench.observe(async (event) => {
  if (!globalThis.activationFinished || event.sequence <= observation.watermark) throw new Error("startup observation gap");
  globalThis.calls++;
  await documents.list();
  await observation.dispose();
  globalThis.disposed = true;
});
globalThis.activationFinished = true;
"#).unwrap();
    let admission = pool.receive_request().unwrap();
    assert_eq!(
        admission.operation,
        operation::<declared::observe_workbench>(())
    );
    let subscription = WorkbenchSubscriptionId::new(911);
    let event = |sequence| ApplicationEvent {
        sequence,
        change: ApplicationChange::DocumentClosed {
            document: "document".into(),
        },
    };
    pool.dispatch_application_change(runtime, subscription, event(8))
        .unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: admission.id,
        result: Ok(success::<declared::observe_workbench>(
            WorkbenchObservation {
                subscription,
                snapshot: ApplicationSnapshot::default(),
                watermark: 7,
            },
        )),
    })
    .unwrap();
    setup.wait().unwrap();
    let query = pool.receive_request().unwrap();
    assert_eq!(query.operation, operation::<declared::list_documents>(()));
    pool.dispatch_application_change(runtime, subscription, event(9))
        .unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: query.id,
        result: Ok(success::<declared::list_documents>(vec![])),
    })
    .unwrap();
    let disposal = pool.receive_request().unwrap();
    assert_eq!(
        disposal.operation,
        operation::<declared::unobserve_workbench>(declared::WorkbenchSubscription {
            subscription
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: disposal.id,
        result: Ok(success::<declared::unobserve_workbench>(())),
    })
    .unwrap();
    assert_eq!(
        &*pool
            .execute(
                runtime,
                "verify-observation.js",
                "`${activationFinished}:${disposed}:${calls}`"
            )
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "true:true:1"
    );
    pool.shutdown();
}

#[test]
fn buffer_listener_self_disposal_rejects_queued_deliveries_without_deadlock() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(197);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let setup = execute_captured_fixture(
        &pool,
        runtime,
        "file:///fixtures/listener-self-disposal.js",
        r#"
      const buffer = await editor.activeBuffer();
      globalThis.listenerCalls = 0;
      globalThis.subscription = await buffer.onDidChange(async () => {
        listenerCalls++;
        await subscription.dispose();
        globalThis.listenerReleased = true;
      });
    "#,
    );
    for value in [
        success::<declared::active_buffer>(Some(BufferHandle::new(3))),
        success::<declared::subscribe_buffer_changes>(BufferSubscriptionId::new(910)),
    ] {
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(value),
        })
        .unwrap();
    }
    assert_eq!(setup.wait().unwrap(), CommandOutcome::Completed);
    for revision in 1..=2 {
        pool.dispatch_buffer_change(
            runtime,
            BufferSubscriptionId::new(910),
            BufferChange {
                buffer: BufferHandle::new(3),
                before_revision: revision - 1,
                revision,
                edits: vec![],
            },
        )
        .unwrap();
    }
    let release = pool.receive_request().unwrap();
    assert!(
        release
            .operation
            .arguments::<declared::unsubscribe_buffer_changes>()
            .is_some()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: release.id,
        result: Ok(success::<declared::unsubscribe_buffer_changes>(())),
    })
    .unwrap();
    assert_eq!(
        &*pool
            .execute(
                runtime,
                "verify-listener-disposal.js",
                "`${listenerReleased}:${listenerCalls}`"
            )
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "true:1"
    );
    assert!(
        pool.capsule(runtime)
            .unwrap()
            .local_state
            .pending
            .lock()
            .unwrap()
            .is_empty()
    );
    pool.shutdown();
}

#[test]
fn contribution_cleanup_waits_for_its_accepted_replacement_acknowledgement() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(198);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let setup = execute_captured_fixture(
        &pool,
        runtime,
        "file:///fixtures/contribution-order.js",
        r#"
      globalThis.buffer = await editor.activeBuffer();
      globalThis.api = knot.workbench;
    "#,
    );
    let request = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
            3,
        )))),
    })
    .unwrap();
    assert_eq!(setup.wait().unwrap(), CommandOutcome::Completed);
    let work = pool
        .execute(
            runtime,
            "ordered-contribution-disposal.js",
            r#"
      (async () => {
        const replacement = buffer.contributions.replace([], { ifRevision: 0 });
        const cleanup = buffer.contributions.dispose();
        const acknowledged = cleanup.then(() => api.writeClipboardText("contribution cleanup"));
        await Promise.all([replacement, cleanup, acknowledged]);
      })()
    "#,
        )
        .unwrap();
    let replace = pool.receive_request().unwrap();
    let dispose = pool.receive_request().unwrap();
    assert!(
        replace
            .operation
            .arguments::<declared::replace_editor_contributions>()
            .is_some()
    );
    assert!(
        dispose
            .operation
            .arguments::<declared::dispose_editor_contributions>()
            .is_some()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: dispose.id,
        result: Ok(success::<declared::dispose_editor_contributions>(())),
    })
    .unwrap();
    assert!(
        pool.receive_request_timeout(Duration::from_millis(20))
            .is_none()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: replace.id,
        result: Ok(success::<declared::replace_editor_contributions>(())),
    })
    .unwrap();
    let acknowledged = pool.receive_request().unwrap();
    assert_eq!(
        acknowledged.operation,
        clipboard_operation("contribution cleanup")
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: acknowledged.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    work.wait().unwrap();
    pool.shutdown();
}

#[test]
fn native_json_transport_preserves_deep_plain_data_without_custom_conversion() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(199);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = execute_captured_fixture(
        &pool,
        runtime,
        "file:///fixtures/plain-json.js",
        r#"
      const items = [42];
      Object.defineProperty(items, "0", { value: 42, enumerable: false });
      let data = { toJSON: "literal field", items, plain: Object.assign(Object.create(null), { enabled: true }) };
      for (let index = 0; index < 300; index++) data = { next: data };
      if ((await commands.invoke("plain-child", data)).kind !== "completed") throw new Error("plain JSON failed");
    "#,
    );
    let request = pool.receive_request().unwrap();
    let command = &request
        .operation
        .arguments::<declared::invoke_command>()
        .expect("command invocation")
        .command;
    let mut data = &command.arguments;
    for _ in 0..300 {
        let CommandArgumentValue::Object(object) = data else {
            panic!("lost JSON depth");
        };
        data = object.get("next").unwrap();
    }
    let CommandArgumentValue::Object(object) = data else {
        panic!("lost JSON leaf");
    };
    assert_eq!(
        object.get("toJSON"),
        Some(&CommandArgumentValue::String("literal field".into()))
    );
    assert_eq!(
        object.get("items"),
        Some(&CommandArgumentValue::Array(vec![
            CommandArgumentValue::Number(42.0)
        ]))
    );
    assert_eq!(
        object.get("plain"),
        Some(&CommandArgumentValue::Object(
            std::collections::BTreeMap::from([(
                "enabled".into(),
                CommandArgumentValue::Boolean(true)
            )])
        ))
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::invoke_command>(
            CommandInvokeDispatch::Outcome {
                outcome: CommandOutcome::Completed,
            },
        )),
    })
    .unwrap();
    assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
    pool.shutdown();
}

#[test]
fn native_numeric_decoder_rejects_coercion_and_precision_loss() {
    let capsule = RuntimeCapsule::new(key(200), IsolateConfig::default()).unwrap();
    let data = capsule.data.lock().unwrap();
    let data = data.as_ref().unwrap();
    let mut locker = data.isolate.lock();
    let scope = pin!(v8::HandleScope::new(&mut *locker));
    let mut scope = scope.init();
    let context = v8::Local::new(&scope, &data.context);
    let scope = &mut v8::ContextScope::new(&mut scope, context);
    let values = [
        v8::String::new(scope, "1").unwrap().into(),
        v8::Boolean::new(scope, true).into(),
        v8::null(scope).into(),
        v8::undefined(scope).into(),
    ];
    for value in values {
        assert!(matches!(
            value_u64(scope, value, "coordinate"),
            Err(BoundaryError::Type(_))
        ));
    }
    for number in [
        -1.0,
        0.5,
        f64::NAN,
        f64::INFINITY,
        MAX_JS_INTEGER as f64 + 1.0,
    ] {
        let value = v8::Number::new(scope, number);
        assert!(matches!(
            value_u64(scope, value.into(), "coordinate"),
            Err(BoundaryError::Range(_))
        ));
    }
    let maximum = v8::Number::new(scope, MAX_JS_INTEGER as f64);
    assert_eq!(
        value_u64(scope, maximum.into(), "coordinate").unwrap(),
        MAX_JS_INTEGER
    );
}

#[test]
fn native_dispatch_callbacks_are_not_available_to_extension_code() {
    let capsule = RuntimeCapsule::new(key(201), IsolateConfig::default()).unwrap();
    let report = capsule
        .execute(
            "private-dispatch.js",
            r#"
        ["__knotNativeBindings", "__knotDispatchBufferChange", "__knotInvokeCommand",
         "__knotCancelCommand", "__knotRequestTreeChildren", "__knotRequestCompletions"]
            .every((name) => !Object.hasOwn(globalThis, name))
    "#,
        )
        .unwrap();
    assert_eq!(&*report.value, "true");
}

#[test]
fn private_dispatch_handles_ignore_author_globals_and_teardown_rejects_new_requests() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(202);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let first = execute_captured_fixture(
        &pool,
        runtime,
        "file:///fixtures/dispatch-handles.js",
        r#"
        globalThis.commandCalls = (globalThis.commandCalls ?? 0) + 1;
        globalThis.__knotInvokeCommand = () => { throw new Error("forged dispatch"); };
    "#,
    );
    assert_eq!(first.wait().unwrap(), CommandOutcome::Completed);
    let second = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: CommandInvocationId::new(901),
                registration: CommandRegistrationId::new(900),
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(second.wait().unwrap(), CommandOutcome::Completed);
    assert_eq!(
        &*pool
            .execute(runtime, "verify-private-dispatch.js", "commandCalls")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "2"
    );
    pool.shutdown();

    let capsule = RuntimeCapsule::new(key(203), IsolateConfig::default()).unwrap();
    {
        let data = capsule.data.lock().unwrap();
        let data = data.as_ref().unwrap();
        let mut locker = data.isolate.lock();
        let scope = pin!(v8::HandleScope::new(&mut *locker));
        let mut scope = scope.init();
        let context = v8::Local::new(&scope, &data.context);
        let scope = &mut v8::ContextScope::new(&mut scope, context);
        capsule.status.store(DISPOSED, Ordering::Release);
        let request = v8::Function::new(scope, host_request_callback).unwrap();
        let operation = v8::String::new(scope, "writeClipboardText").unwrap();
        let text = v8::String::new(scope, "cleanup").unwrap();
        let result = request
            .call(
                scope,
                v8::undefined(scope).into(),
                &[v8::null(scope).into(), operation.into(), text.into()],
            )
            .unwrap();
        let promise = v8::Local::<v8::Promise>::try_from(result).unwrap();
        promise.mark_as_handled();
        assert_eq!(promise.state(), v8::PromiseState::Rejected);
        let error = v8::Local::<v8::Object>::try_from(promise.result(scope)).unwrap();
        assert_eq!(string_property(scope, error, "code").unwrap(), "Cancelled");
        assert!(capsule.local_state.pending.lock().unwrap().is_empty());
    }
    capsule.dispose();
}

#[test]
fn process_initialization_is_idempotent() {
    initialize();
    initialize();
}

#[test]
fn capsule_preserves_globals_and_drains_microtasks() {
    let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
    capsule
        .execute(
            "first.js",
            "globalThis.count = 41; Promise.resolve().then(() => count++);",
        )
        .unwrap();
    let report = capsule.execute("second.js", "count").unwrap();
    assert_eq!(&*report.value, "42");
}

#[test]
fn scheduled_fixture_modules_resolve_static_imports_and_preserve_state() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();

    pool.execute_fixture_module(
        runtime,
        "file:///fixtures/counter.js",
        "export let count = 0; export function increment() { count++; }",
    )
    .unwrap()
    .wait()
    .unwrap();
    pool.execute_fixture_module(
        runtime,
        "file:///fixtures/first.js",
        r#"
            import { count, increment } from "./counter.js";
            increment();
            globalThis.firstModuleCount = count;
        "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    pool.execute_fixture_module(
        runtime,
        "file:///fixtures/second.js",
        r#"
            import { count, increment } from "./counter.js";
            increment();
            globalThis.secondModuleCount = count;
        "#,
    )
    .unwrap()
    .wait()
    .unwrap();

    assert_eq!(
        &*pool
            .execute(
                runtime,
                "verify.js",
                "`${firstModuleCount}:${secondModuleCount}`"
            )
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "1:2"
    );
    pool.shutdown();
}

#[test]
fn public_facade_is_semantic_and_private_bindings_are_hidden() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    pool.execute_fixture_module(
        runtime,
        "file:///fixtures/facade.js",
        r#"
            import * as knot from "knot";
            if (typeof Deno !== "undefined") throw new Error("Deno is exposed");
            if (typeof globalThis.__knotNativeBindings !== "undefined") {
                throw new Error("private native bindings are exposed");
            }
            if (Object.keys(knot).sort().join(",") !== "commands,documents,editor,keybinding,recovery,removeKeybinding,storage,windows,workbench,workspaces") {
                throw new Error("unexpected public exports");
            }
            if (!Object.isFrozen(knot.editor) || !Object.isFrozen(knot.commands) || !Object.isFrozen(knot.workbench) || !Object.isFrozen(knot.storage)) {
                throw new Error("facade objects are mutable");
            }
            if (Object.keys(knot.storage).sort().join(",") !== "delete,list,read,write") {
                throw new Error("storage facade is incomplete");
            }
            if (!Object.isFrozen(knot.recovery) || Object.keys(knot.recovery).sort().join(",") !== "list,release,restore,retain") {
                throw new Error("recovery facade is incomplete");
            }
            if (typeof knot.editor.activeBuffer !== "function"
                || typeof knot.keybinding !== "function"
                || typeof knot.removeKeybinding !== "function"
                || typeof knot.editor.registerCompletionProvider !== "function"
                || typeof knot.commands.invalidArguments !== "function"
                || typeof knot.commands.invoke !== "function"
                || typeof knot.commands.register !== "function"
                || typeof knot.workbench.registerView !== "function") {
                throw new Error("facade shape is incomplete");
            }
        "#,
    )
    .unwrap()
    .wait()
    .unwrap();

    let unsupported = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/unsupported.js",
            r#"
                import * as knot from "knot"; const { workbench } = knot;
                await workbench.writeClipboardText("wait");
            "#,
        )
        .unwrap();
    let request = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Err(test_failure("UnsupportedOperation")),
    })
    .unwrap();
    let unsupported = unsupported.wait().unwrap_err();
    assert_eq!(unsupported.kind(), RuntimeErrorKind::Rejection);
    assert!(unsupported.message().contains("UnsupportedOperation"));
    pool.shutdown();
}

#[test]
fn buffer_requests_and_typed_responses_round_trip() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(40);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = execute_captured_fixture(
        &pool,
        runtime,
        "file:///fixtures/buffers.js",
        r#"
                    const buffer = await editor.activeBuffer();
                globalThis.snapshot = await buffer.snapshot({ startByteOffset: 1, endByteOffset: 7 });
                globalThis.edit = await buffer.applyEdits([{
                  range: { startByteOffset: 2, endByteOffset: 5 }, text: "λ",
                }], { ifRevision: 7 });
                await buffer.contributions.replace([{
                  range: { startByteOffset: 0, endByteOffset: 2 },
                  decoration: "warning", gutter: "error", command: "fixture.command",
                }], { ifRevision: 8 });
                const disposable = await buffer.onDidChange(() => {});
                await disposable.dispose();
                await buffer.contributions.dispose();
            "#,
    );

    let active = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: active.id,
        result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
            3,
        )))),
    })
    .unwrap();

    let request = pool.receive_request().unwrap();
    assert_eq!(
        request.operation,
        operation::<declared::snapshot>(declared::SnapshotArguments {
            buffer: BufferHandle::new(3),
            range: Some(ByteRange {
                start_byte_offset: 1,
                end_byte_offset: 7
            }),
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::snapshot>(TextSnapshot {
            text: SnapshotText::from_utf8("é中z"),
            range: ByteRange {
                start_byte_offset: 1,
                end_byte_offset: 7,
            },
            revision: 7,
        })),
    })
    .unwrap();

    let request = pool.receive_request().unwrap();
    assert_eq!(
        request.operation,
        operation::<declared::apply_edits>(declared::EditArguments {
            buffer: BufferHandle::new(3),
            edits: vec![TextEdit {
                range: ByteRange {
                    start_byte_offset: 2,
                    end_byte_offset: 5
                },
                text: "λ".into(),
            }],
            if_revision: 7,
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::apply_edits>(8)),
    })
    .unwrap();

    let request = pool.receive_request().unwrap();
    assert_eq!(
        request.operation,
        operation::<declared::replace_editor_contributions>(declared::ContributionArguments {
            buffer: BufferHandle::new(3),
            contributions: vec![EditorContribution {
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 2
                },
                decoration: Some(DecorationToken::Warning),
                gutter: Some(GutterToken::Error),
                command: Some("fixture.command".into()),
            }],
            if_revision: 8,
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::replace_editor_contributions>(())),
    })
    .unwrap();

    let request = pool.receive_request().unwrap();
    assert_eq!(
        request.operation,
        operation::<declared::subscribe_buffer_changes>(declared::BufferArguments {
            buffer: BufferHandle::new(3)
        })
    );
    let subscription = BufferSubscriptionId::new(12);
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::subscribe_buffer_changes>(subscription)),
    })
    .unwrap();
    let request = pool.receive_request().unwrap();
    assert_eq!(
        request.operation,
        operation::<declared::unsubscribe_buffer_changes>(declared::SubscriptionArguments {
            subscription
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::unsubscribe_buffer_changes>(())),
    })
    .unwrap();
    let request = pool.receive_request().unwrap();
    assert_eq!(
        request.operation,
        operation::<declared::dispose_editor_contributions>(declared::BufferArguments {
            buffer: BufferHandle::new(3)
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::dispose_editor_contributions>(())),
    })
    .unwrap();
    assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
    pool.execute(runtime, "verify-buffers.js", r#"
        if (snapshot.text !== "é中z" || snapshot.revision !== 7) throw new Error("snapshot");
        if (snapshot.byteOffsetAtUtf16(1) !== 2 || snapshot.utf16OffsetAtByte(5) !== 2) throw new Error("unicode");
        if (edit.revision !== 8) throw new Error("edit");
    "#).unwrap().wait().unwrap();
    pool.shutdown();
}

#[test]
fn external_utf16_snapshot_storage_is_shared_and_released_on_unload() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtimes = [key(41), key(42)];
    let text: Arc<[u16]> = "héllo".encode_utf16().collect::<Vec<_>>().into();
    let weak = Arc::downgrade(&text);
    for runtime in runtimes {
        pool.load(runtime, IsolateConfig::default()).unwrap();
        let execution = execute_captured_fixture(
            &pool,
            runtime,
            format!("file:///fixtures/external-{}.js", runtime.extension.value()),
            r#"
                            const buffer = await editor.activeBuffer();
                    globalThis.heldSnapshot = await buffer.snapshot();
                "#,
        );
        let active = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: active.id,
            result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
                1,
            )))),
        })
        .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText(Arc::clone(&text)),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 6,
                },
                revision: 0,
            })),
        })
        .unwrap();
        assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
    }
    drop(text);
    assert_eq!(weak.strong_count(), 2);
    pool.unload(runtimes[0]).unwrap();
    assert_eq!(weak.strong_count(), 1);
    pool.unload(runtimes[1]).unwrap();
    assert_eq!(weak.strong_count(), 0);
    pool.shutdown();
}

#[test]
fn buffer_change_callbacks_are_serial_and_survive_listener_failure() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(44);
    let subscription = BufferSubscriptionId::new(9);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    pool.execute(
        runtime,
        "listener.js",
        r#"
        globalThis.listener = async (event) => {
          events.push(event.revision);
          if (event.revision === 1) throw new Error("expected listener failure");
        };
    "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    install_buffer_listener(&pool, runtime, subscription, "file:///fixtures/listener.js");
    for revision in 1..=2 {
        pool.dispatch_buffer_change(
            runtime,
            subscription,
            BufferChange {
                buffer: BufferHandle::new(3),
                before_revision: revision - 1,
                revision,
                edits: vec![TextEdit {
                    range: ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 0,
                    },
                    text: revision.to_string(),
                }],
            },
        )
        .unwrap();
    }
    pool.execute(
        runtime,
        "verify-events.js",
        r#"
        if (events.join(",") !== "1,2") throw new Error("events reordered");
    "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    let metrics = pool.buffer_change_queue_metrics(runtime).unwrap();
    assert!(metrics.max_depth >= 1);
    assert!(metrics.max_enqueue_to_start_lag > Duration::ZERO);
    pool.shutdown();
}

#[test]
fn finite_slow_buffer_change_burst_is_lossless_and_measured() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(45);
    let subscription = BufferSubscriptionId::new(10);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    pool.execute(
        runtime,
        "slow-listener.js",
        r#"
        globalThis.listener = async (event) => {
          if (event.revision === 1) await event.buffer.snapshot();
          events.push(event.revision);
        };
    "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    install_buffer_listener(
        &pool,
        runtime,
        subscription,
        "file:///fixtures/slow-listener.js",
    );
    for revision in 1..=8 {
        pool.dispatch_buffer_change(
            runtime,
            subscription,
            BufferChange {
                buffer: BufferHandle::new(3),
                before_revision: revision - 1,
                revision,
                edits: vec![],
            },
        )
        .unwrap();
    }
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    let queued = pool.buffer_change_queue_metrics(runtime).unwrap();
    assert!(queued.max_depth >= 7, "metrics: {queued:?}");
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::snapshot>(TextSnapshot {
            text: SnapshotText::from_utf8(""),
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 0,
            },
            revision: 1,
        })),
    })
    .unwrap();
    pool.execute(
        runtime,
        "verify-burst.js",
        r#"
        if (events.join(",") !== "1,2,3,4,5,6,7,8") throw new Error("events lost");
    "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    let metrics = pool.buffer_change_queue_metrics(runtime).unwrap();
    assert!(metrics.max_depth >= 7, "metrics: {metrics:?}");
    assert!(metrics.max_enqueue_to_start_lag > Duration::ZERO);
    pool.shutdown();
}

#[test]
fn awaiting_buffer_listener_yields_single_worker_to_another_extension() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let waiting = key(46);
    let neighbor = key(47);
    let subscription = BufferSubscriptionId::new(11);
    pool.load(waiting, IsolateConfig::default()).unwrap();
    pool.load(neighbor, IsolateConfig::default()).unwrap();
    pool.execute(
        waiting,
        "async-listener.js",
        r#"
        globalThis.listener = async (event) => {
          await event.buffer.snapshot();
          events.push(event.revision);
        };
    "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    install_buffer_listener(
        &pool,
        waiting,
        subscription,
        "file:///fixtures/async-listener.js",
    );
    pool.dispatch_buffer_change(
        waiting,
        subscription,
        BufferChange {
            buffer: BufferHandle::new(3),
            before_revision: 0,
            revision: 1,
            edits: vec![],
        },
    )
    .unwrap();
    let request = pool.receive_request().unwrap();
    wait_for_state(&pool, waiting, ExtensionState::AwaitingHostWork);
    assert_eq!(
        &*pool
            .execute(neighbor, "neighbor.js", "6 * 7")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "42"
    );
    pool.respond(HostResponse {
        extension: waiting.extension,
        lifecycle: waiting.lifecycle,
        id: request.id,
        result: Ok(success::<declared::snapshot>(TextSnapshot {
            text: SnapshotText::from_utf8(""),
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 0,
            },
            revision: 1,
        })),
    })
    .unwrap();
    pool.execute(
        waiting,
        "verify-async-event.js",
        r#"
        if (events.join(",") !== "1") throw new Error("listener did not resume");
    "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    pool.shutdown();
}

#[test]
fn unloading_a_buffer_listener_awaiting_host_work_rejects_its_late_response() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(49);
    let subscription = BufferSubscriptionId::new(12);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    pool.execute(
        runtime,
        "unload-listener.js",
        "globalThis.listener = async (event) => { await event.buffer.snapshot(); };",
    )
    .unwrap()
    .wait()
    .unwrap();
    install_buffer_listener(
        &pool,
        runtime,
        subscription,
        "file:///fixtures/unload-listener.js",
    );
    pool.dispatch_buffer_change(
        runtime,
        subscription,
        BufferChange {
            buffer: BufferHandle::new(3),
            before_revision: 0,
            revision: 1,
            edits: vec![],
        },
    )
    .unwrap();
    let request = pool.receive_request().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);

    pool.unload(runtime).unwrap();
    assert_eq!(
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8(""),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 0
                },
                revision: 1
            })),
        }),
        Err(RuntimeResponseError::WrongExtension)
    );
    pool.shutdown();
}

#[test]
fn stale_lifecycle_cannot_dispatch_a_buffer_change() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let stale = key(48);
    pool.load(stale, IsolateConfig::default()).unwrap();
    pool.unload(stale).unwrap();
    let replacement = ExtensionKey::new(stale.extension, ExtensionLifecycleId::new(2));
    pool.load(replacement, IsolateConfig::default()).unwrap();
    assert!(matches!(
        pool.dispatch_buffer_change(
            stale,
            BufferSubscriptionId::new(1),
            BufferChange {
                buffer: BufferHandle::new(1),
                before_revision: 0,
                revision: 1,
                edits: vec![],
            },
        ),
        Err(RuntimePoolError::Scheduler(
            SchedulerError::StaleLifecycle { .. }
        ))
    ));
    pool.shutdown();
}

#[test]
fn host_response_resumes_a_pending_root_and_drains_microtasks() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/request.js",
            r#"
                import * as knot from "knot"; const { workbench } = knot;
                const buffer = await workbench.writeClipboardText("wait");
                await Promise.resolve();
                globalThis.responseValue = buffer === undefined;
            "#,
        )
        .unwrap();
    let request = pool.receive_request().unwrap();
    assert_eq!(request.extension, runtime.extension);
    assert_eq!(request.lifecycle, runtime.lifecycle);
    assert_eq!(request.id, RequestId::new(1));
    assert_eq!(request.operation, clipboard_operation("wait"));
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    execution.wait().unwrap();
    assert_eq!(
        &*pool
            .execute(runtime, "verify-response.js", "responseValue")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "true"
    );
    pool.shutdown();
}

#[test]
fn declared_responses_preserve_structured_errors_precision_and_operation_pairing() {
    use crate::host::declarations::{DeclaredResponse, EncodedValue, HostError};
    #[derive(knot_handler_macros::HostError)]
    enum ProbeError {
        #[error(code = "ProbeFailed", message = "probe diagnostic")]
        RenamableVariant { revision: u64 },
    }
    let cases = [
        (
            "structured",
            DeclaredResponse {
                operation: "writeClipboardText",
                result: Err(ProbeError::RenamableVariant { revision: 7 }.into_failure()),
            },
            r#"if (error.name !== "KnotHostError" || error.code !== "ProbeFailed" || error.message !== "probe diagnostic" || error.details.revision !== 7) throw error;"#,
        ),
        (
            "precision",
            DeclaredResponse {
                operation: "writeClipboardText",
                result: Ok(Ok(EncodedValue::Json(
                    serde_json::json!({"revision": MAX_JS_INTEGER + 1}),
                ))),
            },
            r#"if (error.name !== "KnotHostError" || error.code !== "UnsupportedOperation") throw error;"#,
        ),
        (
            "error-detail-precision",
            DeclaredResponse {
                operation: "writeClipboardText",
                result: Err(crate::host::declarations::HostFailure {
                    code: "ProbeFailed",
                    message: "probe diagnostic".into(),
                    details: serde_json::json!({"revision": MAX_JS_INTEGER + 1}),
                }),
            },
            r#"if (error.code !== "UnsupportedOperation") throw error;"#,
        ),
        (
            "pairing",
            DeclaredResponse {
                operation: "openDocument",
                result: Ok(Ok(EncodedValue::Undefined)),
            },
            r#"if (error.code !== "UnsupportedOperation") throw error;"#,
        ),
    ];
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(719);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    for (name, response, check) in cases {
        let execution = pool
            .execute_fixture_module(
                runtime,
                format!("file:///fixtures/declared-{name}.js"),
                format!(
                    r#"
            import {{ workbench }} from "knot";
            let rejected = false;
            try {{ await workbench.writeClipboardText("probe"); }}
            catch (error) {{ rejected = true; {check} }}
            if (!rejected) throw new Error("invalid native response accepted");
        "#
                ),
            )
            .unwrap();
        let request = pool.receive_request().unwrap();
        pool.respond(HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result: Ok(response),
        })
        .unwrap();
        execution.wait().unwrap();
    }
    pool.shutdown();
}

#[test]
fn host_error_response_rejects_the_request_and_propagates_from_the_root() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/request-error.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    let request = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Err(test_failure("RevisionConflict")),
    })
    .unwrap();
    let error = execution.wait().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
    assert!(error.message().contains("RevisionConflict"));
    assert_eq!(error.source(), Some("file:///fixtures/request-error.js"));
    assert_eq!(
        &*pool
            .execute(runtime, "recover-after-host-error.js", "6 * 7")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "42"
    );
    pool.shutdown();
}

#[test]
fn multiple_host_requests_settle_out_of_order_exactly_once() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/concurrent-requests.js",
            r#"
                import * as knot from "knot"; const { workbench } = knot;
                globalThis.responseOrder = [];
                await Promise.all([
                  workbench.writeClipboardText("wait").then(() => responseOrder.push(1)),
                  workbench.writeClipboardText("wait").then(() => responseOrder.push(2)),
                ]);
                globalThis.responsesAreDistinct = responseOrder.join(",") === "2,1";
            "#,
        )
        .unwrap();
    let first = pool.receive_request().unwrap();
    let second = pool.receive_request().unwrap();
    assert_eq!(first.id, RequestId::new(1));
    assert_eq!(second.id, RequestId::new(2));
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: second.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    assert_eq!(
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: second.id,
            result: Ok(clipboard_written()),
        }),
        Err(RuntimeResponseError::UnknownRequest)
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: first.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    execution.wait().unwrap();
    assert_eq!(
        &*pool
            .execute(runtime, "verify-order.js", "responsesAreDistinct")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "true"
    );
    assert_eq!(
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: first.id,
            result: Ok(clipboard_written()),
        }),
        Err(RuntimeResponseError::UnknownRequest)
    );
    pool.shutdown();
}

#[test]
fn root_rejection_rejects_and_removes_sibling_request_resolvers() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/rejected-siblings.js",
            r#"
                import * as knot from "knot"; const { workbench } = knot;
                await Promise.all([workbench.writeClipboardText("wait"), workbench.writeClipboardText("wait")]);
            "#,
        )
        .unwrap();
    let first = pool.receive_request().unwrap();
    let second = pool.receive_request().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: first.id,
        result: Err(test_failure("BufferClosed")),
    })
    .unwrap();
    let error = execution.wait().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
    assert!(error.message().contains("BufferClosed"));
    assert_eq!(
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: second.id,
            result: Ok(clipboard_written()),
        }),
        Err(RuntimeResponseError::UnknownRequest)
    );
    pool.shutdown();
}

#[test]
fn response_identity_is_validated_without_consuming_the_request() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/identity.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    let request = pool.receive_request().unwrap();
    for response in [
        HostResponse {
            extension: ExtensionId::new(99),
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(clipboard_written()),
        },
        HostResponse {
            extension: runtime.extension,
            lifecycle: ExtensionLifecycleId::new(99),
            id: request.id,
            result: Ok(clipboard_written()),
        },
        HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: RequestId::new(99),
            result: Ok(clipboard_written()),
        },
    ] {
        assert!(pool.respond(response).is_err());
    }
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    execution.wait().unwrap();
    pool.shutdown();
}

#[test]
fn single_worker_progresses_while_another_extension_awaits_host_work() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let waiting = key(1);
    let neighbor = key(2);
    pool.load(waiting, IsolateConfig::default()).unwrap();
    pool.load(neighbor, IsolateConfig::default()).unwrap();
    let delayed = pool
        .execute_fixture_module(
            waiting,
            "file:///fixtures/delayed.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    let request = pool.receive_request().unwrap();
    assert_eq!(
        &*pool
            .execute(neighbor, "neighbor.js", "6 * 7")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "42"
    );
    pool.respond(HostResponse {
        extension: waiting.extension,
        lifecycle: waiting.lifecycle,
        id: request.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    delayed.wait().unwrap();
    pool.shutdown();
}

#[test]
fn sequential_host_requests_keep_the_root_active_until_the_last_response() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/sequential.js",
            r#"
                import * as knot from "knot"; const { workbench } = knot;
                await Promise.resolve();
                await workbench.writeClipboardText("wait");
                globalThis.firstResponse = true;
                await workbench.writeClipboardText("wait");
                globalThis.secondResponse = true;
            "#,
        )
        .unwrap();
    let first = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: first.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    let second = pool.receive_request().unwrap();
    assert_eq!(second.id, RequestId::new(2));
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: second.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    execution.wait().unwrap();
    assert_eq!(
        &*pool
            .execute(
                runtime,
                "verify-sequential.js",
                "`${firstResponse}:${secondResponse}`"
            )
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "true:true"
    );
    pool.shutdown();
}

#[test]
fn mismatched_typed_response_rejects_the_javascript_request() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/mismatched-response.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    let request = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::dispose_editor_contributions>(())),
    })
    .unwrap();
    let error = execution.wait().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
    assert!(error.message().contains("UnsupportedOperation"));
    pool.shutdown();
}

#[test]
fn unload_settles_waiting_execution_and_rejects_stale_responses() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let first = key(1);
    pool.load(first, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            first,
            "file:///fixtures/unload-pending.js",
            r#"
                import * as knot from "knot"; const { workbench } = knot;
                await Promise.all([workbench.writeClipboardText("wait"), workbench.writeClipboardText("wait")]);
            "#,
        )
        .unwrap();
    let first_request = pool.receive_request().unwrap();
    let _second_request = pool.receive_request().unwrap();
    wait_for_state(&pool, first, ExtensionState::AwaitingHostWork);
    pool.unload(first).unwrap();
    let error = execution.wait().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Cancelled);
    assert_eq!(
        pool.respond(HostResponse {
            extension: first.extension,
            lifecycle: first.lifecycle,
            id: first_request.id,
            result: Ok(clipboard_written()),
        }),
        Err(RuntimeResponseError::WrongExtension)
    );

    let replacement = ExtensionKey::new(first.extension, ExtensionLifecycleId::new(2));
    pool.load(replacement, IsolateConfig::default()).unwrap();
    let replacement_execution = pool
        .execute_fixture_module(
            replacement,
            "file:///fixtures/replacement.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    let replacement_request = pool.receive_request().unwrap();
    assert_eq!(replacement_request.id, RequestId::new(1));
    assert_eq!(
        pool.respond(HostResponse {
            extension: first.extension,
            lifecycle: first.lifecycle,
            id: replacement_request.id,
            result: Ok(clipboard_written()),
        }),
        Err(RuntimeResponseError::WrongExtension)
    );
    pool.respond(HostResponse {
        extension: replacement.extension,
        lifecycle: replacement.lifecycle,
        id: replacement_request.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    replacement_execution.wait().unwrap();
    pool.shutdown();
}

#[test]
fn shutdown_and_forced_termination_settle_pending_executions() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/shutdown-pending.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    pool.receive_request().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    pool.shutdown();
    assert_eq!(
        execution.wait().unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );

    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(2);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let execution = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/terminated-pending.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    let request = pool.receive_request().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    assert!(pool.termination_handle(runtime).unwrap().terminate());
    assert_eq!(
        execution.wait().unwrap_err().kind(),
        RuntimeErrorKind::Terminated
    );
    assert_eq!(
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(clipboard_written()),
        }),
        Err(RuntimeResponseError::WrongExtension)
    );
    pool.shutdown();
}

#[test]
fn awaited_queued_executions_report_unload_cancellation() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let waiting = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/awaited-queued.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    pool.receive_request().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);

    let script = pool.execute(runtime, "queued.js", "42").unwrap();
    let command = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: CommandInvocationId::new(1),
                registration: CommandRegistrationId::new(1),
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: None,
            },
            None,
        )
        .unwrap();
    let provider = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: TreeProviderRegistrationId::new(1),
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: None,
                generation: 1,
            },
        )
        .unwrap();

    pool.unload(runtime).unwrap();
    assert_eq!(
        pollster::block_on(waiting).unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );
    assert_eq!(
        pollster::block_on(script).unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );
    assert_eq!(
        pollster::block_on(command).unwrap(),
        CommandOutcome::Cancelled
    );
    assert_eq!(
        pollster::block_on(provider).unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );
    pool.shutdown();
}

#[test]
fn awaited_queued_executions_report_shutdown_and_fatal_failure() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let waiting = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/awaited-shutdown.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    pool.receive_request().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    let queued = pool.execute(runtime, "queued-shutdown.js", "42").unwrap();
    pool.shutdown();
    assert_eq!(
        pollster::block_on(waiting).unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );
    assert_eq!(
        pollster::block_on(queued).unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );

    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(2);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let waiting = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/awaited-fatal.js",
            "import * as knot from 'knot'; const { workbench } = knot; await workbench.writeClipboardText('wait');",
        )
        .unwrap();
    pool.receive_request().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    let queued = pool.execute(runtime, "queued-fatal.js", "42").unwrap();
    assert!(pool.termination_handle(runtime).unwrap().terminate());
    assert_eq!(
        pollster::block_on(waiting).unwrap_err().kind(),
        RuntimeErrorKind::Terminated
    );
    assert_eq!(
        pollster::block_on(queued).unwrap_err().kind(),
        RuntimeErrorKind::LifecycleFailed
    );
    pool.shutdown();
}

#[test]
fn facade_maps_every_host_error_to_a_stable_javascript_name() {
    let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
    for (index, wire_name) in super::super::declarations::registry()
        .error_codes()
        .into_iter()
        .enumerate()
    {
        let expected_name = "KnotHostError";
        capsule.set_test_host_error(test_failure(wire_name));
        let report = capsule
            .execute_fixture_module(
                &format!("file:///fixtures/host-error-{index}.js"),
                r#"
                    import * as knot from "knot"; const { workbench } = knot;
                    try {
                        await workbench.writeClipboardText("wait");
                    } catch (error) {
                        globalThis.hostErrorName = error.name;
                        globalThis.hostErrorCode = error.code;
                    }
                "#,
            )
            .unwrap();
        assert_eq!(&*report.value, "undefined");
        assert_eq!(
            &*capsule
                .execute("host-error-name.js", "hostErrorName")
                .unwrap()
                .value,
            expected_name
        );
        assert_eq!(
            &*capsule
                .execute("host-error-code.js", "hostErrorCode")
                .unwrap()
                .value,
            wire_name
        );
    }

    capsule
        .execute_fixture_module(
            "file:///fixtures/invalid-arguments.js",
            r#"
                import * as knot from "knot"; const { commands } = knot;
                try { commands.invalidArguments("expected"); }
                catch (error) { globalThis.hostErrorName = error.name; }
            "#,
        )
        .unwrap();
    assert_eq!(
        &*capsule
            .execute("invalid-arguments-name.js", "hostErrorName")
            .unwrap()
            .value,
        "InvalidCommandArgumentsError"
    );
}

#[test]
fn fixture_module_failures_are_scoped_and_classified() {
    let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();

    let invalid = capsule.execute_fixture_module("not a URL", "").unwrap_err();
    assert_eq!(invalid.kind(), RuntimeErrorKind::InvalidModuleSpecifier);

    let invalid_import = capsule
        .execute_fixture_module(
            "file:///fixtures/invalid-import.js",
            "import 'bare-specifier';",
        )
        .unwrap_err();
    assert_eq!(invalid_import.kind(), RuntimeErrorKind::ModuleResolution);
    assert!(
        invalid_import
            .message()
            .contains("invalid fixture module specifier")
    );

    let missing = capsule
        .execute_fixture_module(
            "file:///fixtures/missing-import.js",
            "import './missing.js';",
        )
        .unwrap_err();
    assert_eq!(missing.kind(), RuntimeErrorKind::ModuleResolution);
    assert!(missing.message().contains("fixture module not found"));

    let private = capsule
        .execute_fixture_module(
            "file:///fixtures/private-import.js",
            "import { activeBuffer } from 'knot:bootstrap'; void activeBuffer;",
        )
        .unwrap_err();
    assert_eq!(private.kind(), RuntimeErrorKind::ModuleResolution);
    assert!(private.message().contains("not importable by extensions"));

    let removed = capsule
        .execute_fixture_module(
            "file:///fixtures/removed-import.js",
            "import 'knot:editor';",
        )
        .unwrap_err();
    assert_eq!(removed.kind(), RuntimeErrorKind::ModuleResolution);
    assert!(removed.message().contains("module not found"));

    let syntax = capsule
        .execute_fixture_module("file:///fixtures/syntax.js", "export const = 1;")
        .unwrap_err();
    assert_eq!(syntax.kind(), RuntimeErrorKind::Compilation);
    assert_eq!(syntax.source(), Some("file:///fixtures/syntax.js"));

    let thrown = capsule
        .execute_fixture_module(
            "file:///fixtures/thrown.js",
            "throw new Error('expected module throw');",
        )
        .unwrap_err();
    assert_eq!(thrown.kind(), RuntimeErrorKind::Rejection);
    assert!(thrown.message().contains("expected module throw"));

    let rejected = capsule
        .execute_fixture_module(
            "file:///fixtures/rejected.js",
            "await Promise.reject(new Error('expected module rejection'));",
        )
        .unwrap_err();
    assert_eq!(rejected.kind(), RuntimeErrorKind::Rejection);
    assert!(rejected.message().contains("expected module rejection"));

    assert_eq!(
        &*capsule.execute("recover.js", "6 * 7").unwrap().value,
        "42"
    );
}

#[test]
fn exceptions_and_rejections_are_scoped_to_a_turn() {
    let capsule = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
    let compilation = capsule.execute("syntax.js", "const =").unwrap_err();
    assert_eq!(compilation.kind(), RuntimeErrorKind::Compilation);
    assert_eq!(compilation.source(), Some("syntax.js"));

    let thrown = capsule
        .execute(
            "throw.js",
            "globalThis.afterThrow = 0; Promise.resolve().then(() => afterThrow = 1); throw new Error('expected throw')",
        )
        .unwrap_err();
    assert_eq!(thrown.kind(), RuntimeErrorKind::Exception);
    assert!(thrown.message().contains("expected throw"));
    assert_eq!(thrown.source(), Some("throw.js"));
    assert_eq!(thrown.line(), Some(1));
    assert_eq!(thrown.column(), Some(74));
    assert!(
        thrown
            .stack()
            .is_some_and(|stack| stack.contains("throw.js"))
    );
    assert_eq!(
        &*capsule
            .execute("after-throw.js", "afterThrow")
            .unwrap()
            .value,
        "1"
    );

    let rejected = capsule
        .execute(
            "reject.js",
            "Promise.reject(new Error('expected rejection one')); Promise.reject(new Error('expected rejection two'))",
        )
        .unwrap_err();
    assert_eq!(rejected.kind(), RuntimeErrorKind::Rejection);
    assert!(rejected.message().contains("expected rejection one"));
    assert!(rejected.message().contains("expected rejection two"));
    assert_eq!(rejected.source(), Some("reject.js"));
    assert_eq!(rejected.line(), Some(1));

    assert_eq!(
        &*capsule.execute("recover.js", "6 * 7").unwrap().value,
        "42"
    );
}

#[test]
fn external_utf16_storage_is_retained_until_isolate_disposal() {
    let text: Arc<[u16]> = "héllo".encode_utf16().collect::<Vec<_>>().into();
    let weak = Arc::downgrade(&text);
    let first = RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap();
    let second = RuntimeCapsule::new(key(2), IsolateConfig::default()).unwrap();
    first
        .hold_external_utf16("held", Arc::clone(&text))
        .unwrap();
    second
        .hold_external_utf16("held", Arc::clone(&text))
        .unwrap();
    drop(text);
    assert_eq!(weak.strong_count(), 2);
    assert_eq!(&*first.execute("read.js", "held").unwrap().value, "héllo");
    first.dispose();
    assert_eq!(weak.strong_count(), 1);
    second.dispose();
    assert!(weak.upgrade().is_none());
}

#[test]
fn pooled_capsule_moves_between_workers_and_retains_state() {
    enum Work {
        Execute {
            capsule: Arc<RuntimeCapsule>,
            source: &'static str,
            result: mpsc::Sender<ExecutionReport>,
        },
        Block {
            started: mpsc::Sender<ThreadId>,
            release: mpsc::Receiver<()>,
        },
    }

    initialize();
    let pool = SchedulerPool::new(
        PoolConfig::new(NonZeroUsize::new(2).unwrap()),
        |turn: Turn<Work>| match turn.work {
            Work::Execute {
                capsule,
                source,
                result,
            } => {
                result
                    .send(capsule.execute("movement.js", source).unwrap())
                    .unwrap();
                TurnOutcome::Completed
            }
            Work::Block { started, release } => {
                started.send(std::thread::current().id()).unwrap();
                release.recv().unwrap();
                TurnOutcome::Completed
            }
        },
    );
    let handle = pool.handle();
    let capsule = Arc::new(RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap());
    for admitted in [key(1), key(2), key(3)] {
        handle.admit(admitted).unwrap();
        handle.finish_loading(admitted, Ok(())).unwrap();
    }

    let (first_sender, first_result) = mpsc::channel();
    let (_, first_completion) = handle
        .enqueue_root(
            key(1),
            Work::Execute {
                capsule: Arc::clone(&capsule),
                source: "globalThis.count = 1; count",
                result: first_sender,
            },
        )
        .unwrap();
    let first_worker = first_result.recv().unwrap().worker;
    assert_eq!(
        first_completion.recv().unwrap(),
        CompletionOutcome::Completed
    );

    let blockers: Vec<_> = [key(2), key(3)]
        .into_iter()
        .map(|key| {
            let (started_sender, started) = mpsc::channel();
            let (release, release_receiver) = mpsc::channel();
            let (_, completion) = handle
                .enqueue_root(
                    key,
                    Work::Block {
                        started: started_sender,
                        release: release_receiver,
                    },
                )
                .unwrap();
            (started, release, completion)
        })
        .collect();
    let blocker_workers: Vec<_> = blockers
        .iter()
        .map(|(started, _, _)| started.recv().unwrap())
        .collect();
    assert_eq!(
        blocker_workers
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len(),
        2
    );
    let free_index = blocker_workers
        .iter()
        .position(|worker| *worker != first_worker)
        .unwrap();
    blockers[free_index].1.send(()).unwrap();
    assert_eq!(
        blockers[free_index].2.recv().unwrap(),
        CompletionOutcome::Completed
    );

    let (second_sender, second_result) = mpsc::channel();
    let (_, second_completion) = handle
        .enqueue_root(
            key(1),
            Work::Execute {
                capsule: Arc::clone(&capsule),
                source: "++count",
                result: second_sender,
            },
        )
        .unwrap();
    let second = second_result.recv().unwrap();
    assert_ne!(second.worker, first_worker);
    assert_eq!(&*second.value, "2");
    assert_eq!(
        second_completion.recv().unwrap(),
        CompletionOutcome::Completed
    );

    blockers[1 - free_index].1.send(()).unwrap();
    assert_eq!(
        blockers[1 - free_index].2.recv().unwrap(),
        CompletionOutcome::Completed
    );
    pool.shutdown();
}

#[test]
fn movement_stress_alternates_workers_before_disposal() {
    type Command = Option<(&'static str, mpsc::Sender<ExecutionReport>)>;

    let capsule = Arc::new(RuntimeCapsule::new(key(1), IsolateConfig::default()).unwrap());
    capsule.execute("init.js", "globalThis.count = 0").unwrap();
    let mut workers = Vec::new();
    for index in 0..2 {
        let (sender, receiver) = mpsc::channel::<Command>();
        let capsule = Arc::clone(&capsule);
        let worker = std::thread::Builder::new()
            .name(format!("knot-movement-stress-{index}"))
            .spawn(move || {
                while let Some((source, result)) = receiver.recv().unwrap() {
                    result
                        .send(capsule.execute("movement-stress.js", source).unwrap())
                        .unwrap();
                }
            })
            .unwrap();
        workers.push((sender, worker));
    }

    let mut worker_ids = HashSet::new();
    for index in 0..500 {
        let (result_sender, result) = mpsc::channel();
        workers[index % 2]
            .0
            .send(Some(("++count", result_sender)))
            .unwrap();
        worker_ids.insert(result.recv().unwrap().worker);
    }
    assert_eq!(worker_ids.len(), 2);
    assert_eq!(
        &*capsule.execute("result.js", "count").unwrap().value,
        "500"
    );
    for (sender, worker) in workers {
        sender.send(None).unwrap();
        worker.join().unwrap();
    }
    capsule.dispose();
    assert_eq!(
        capsule.execute("disposed.js", "count").unwrap_err().kind(),
        RuntimeErrorKind::Disposed
    );
}

#[test]
fn independent_isolates_execute_javascript_concurrently() {
    let pool = RuntimePool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
    let first = key(1);
    let second = key(2);
    pool.load(first, IsolateConfig::default()).unwrap();
    pool.load(second, IsolateConfig::default()).unwrap();
    let barrier = Arc::new(TestGate::default());
    pool.capsule(first)
        .unwrap()
        .install_test_barrier(Arc::clone(&barrier));
    pool.capsule(second)
        .unwrap()
        .install_test_barrier(Arc::clone(&barrier));

    let first_execution = pool
        .execute(first, "first.js", "__knotTestBarrier(); 1")
        .unwrap();
    let second_execution = pool
        .execute(second, "second.js", "__knotTestBarrier(); 2")
        .unwrap();
    let first_report = first_execution.wait().unwrap();
    let second_report = second_execution.wait().unwrap();
    assert_ne!(first_report.worker, second_report.worker);
    assert_eq!((&*first_report.value, &*second_report.value), ("1", "2"));
    assert!(!barrier.timed_out.load(Ordering::Acquire));
    pool.shutdown();
}

fn install_semantic_providers(
    pool: &RuntimePool,
    runtime: ExtensionKey,
    tree_registration: TreeProviderRegistrationId,
    completion_registration: CompletionProviderRegistrationId,
) {
    let setup = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/semantic-providers.js",
            r#"
                import * as knot from "knot"; const { editor, workbench } = knot;
                for (const name of ["__knotRequestTreeChildren", "__knotRequestCompletions"]) {
                  if (Object.hasOwn(globalThis, name)) throw new Error("private provider dispatch exposed");
                  globalThis[name] = () => { throw new Error("untrusted provider dispatch replacement"); };
                }
                globalThis.providerEvents = [];
                globalThis.treeProvider = await workbench.registerView({
                  id: "example.outline",
                  title: "Outline",
                  content: { kind: "tree", provider: {
                    async getChildren(parentId) {
                      globalThis.providerEvents.push(`tree-${parentId ?? "root"}-start`);
                      if (parentId === "await") await workbench.writeClipboardText("tree wait");
                      if (parentId === "failure") throw new Error("expected tree failure");
                      globalThis.providerEvents.push(`tree-${parentId ?? "root"}-end`);
                      return [{
                        id: parentId === null ? "root" : `${parentId}.child`,
                        label: "Tree item",
                        description: "fixture",
                        icon: "symbol",
                        collapsibleState: parentId === null ? "expanded" : "none",
                      }];
                    },
                  } },
                });
                globalThis.completionProvider = await editor.registerCompletionProvider(
                  "fixture",
                  {
                    async provideCompletions(context) {
                      globalThis.providerEvents.push(`completion-${context.prefix}`);
                      if (context.prefix === "await") await context.buffer.snapshot();
                      if (context.prefix === "failure") {
                        throw new Error("expected completion failure");
                      }
                      return [{ label: `${context.prefix}Item`, insertText: "inserted" }];
                    },
                  },
                );
            "#,
        )
        .unwrap();
    let tree = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap_or_else(|| panic!("no registerView request: {:?}", pool.diagnostics()));
    assert_eq!(
        tree.operation,
        operation::<declared::register_tree_provider>(declared::TreeArguments {
            id: "example.outline".into(),
            title: "Outline".into(),
            content: crate::host::protocol::ViewContentKind::Tree,
            actions: Vec::new(),
            location: crate::app::tool_layout::StoredLocation::Left,
            order: None,
            visible: true,
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: tree.id,
        result: Ok(success::<declared::register_tree_provider>(
            tree_registration,
        )),
    })
    .unwrap();
    let completion = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        completion.operation,
        operation::<declared::register_completion_provider>(declared::CompletionArguments {
            label: "fixture".into()
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: completion.id,
        result: Ok(success::<declared::register_completion_provider>(
            completion_registration,
        )),
    })
    .unwrap();
    setup.wait().unwrap();
}

#[test]
fn semantic_provider_callbacks_preserve_typed_generations_and_recover_from_failures() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(61);
    let tree_registration = TreeProviderRegistrationId::new(91);
    let completion_registration = CompletionProviderRegistrationId::new(92);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    install_semantic_providers(&pool, runtime, tree_registration, completion_registration);

    let tree = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: tree_registration,
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: None,
                generation: 8,
            },
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(tree.registration, tree_registration);
    assert_eq!(tree.generation, 8);
    let item = &tree.result.unwrap()[0];
    assert_eq!(item.id, "root");
    assert_eq!(item.icon, Some(crate::host::protocol::TreeIcon::Symbol));
    assert_eq!(
        item.collapsible_state,
        crate::host::protocol::TreeCollapsibleState::Expanded
    );

    let tree_failure = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: tree_registration,
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: Some("failure".into()),
                generation: 9,
            },
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(
        tree_failure
            .result
            .unwrap_err()
            .message
            .contains("expected tree failure")
    );

    let completion = pool
        .request_completions(
            runtime,
            CompletionRequest {
                registration: completion_registration,
                buffer: BufferHandle::new(3),
                revision: 11,
                cursor_byte_offset: 4,
                prefix: "pre".into(),
                generation: 12,
            },
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(completion.registration, completion_registration);
    assert_eq!(completion.revision, 11);
    assert_eq!(completion.generation, 12);
    assert_eq!(completion.result.unwrap()[0].label, "preItem");

    let completion_failure = pool
        .request_completions(
            runtime,
            CompletionRequest {
                registration: completion_registration,
                buffer: BufferHandle::new(3),
                revision: 13,
                cursor_byte_offset: 7,
                prefix: "failure".into(),
                generation: 14,
            },
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(
        completion_failure
            .result
            .unwrap_err()
            .message
            .contains("expected completion failure")
    );

    let invalidate = pool
        .execute(
            runtime,
            "invalidate-tree.js",
            "globalThis.treeProvider.invalidate('root')",
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        request.operation,
        operation::<declared::invalidate_tree_provider>(declared::TreeInvalidationArguments {
            registration: tree_registration,
            parent_id: Some("root".into()),
            window: None,
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::invalidate_tree_provider>(())),
    })
    .unwrap();
    invalidate.wait().unwrap();
    pool.shutdown();
}

#[test]
fn provider_roots_queue_serially_and_a_waiting_callback_releases_the_worker() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(62);
    let neighbor = key(63);
    let tree_registration = TreeProviderRegistrationId::new(93);
    let completion_registration = CompletionProviderRegistrationId::new(94);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    pool.load(neighbor, IsolateConfig::default()).unwrap();
    install_semantic_providers(&pool, runtime, tree_registration, completion_registration);

    let tree = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: tree_registration,
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: Some("await".into()),
                generation: 1,
            },
        )
        .unwrap();
    let suspended = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(suspended.operation, clipboard_operation("tree wait"));
    let completion = pool
        .request_completions(
            runtime,
            CompletionRequest {
                registration: completion_registration,
                buffer: BufferHandle::new(4),
                revision: 2,
                cursor_byte_offset: 3,
                prefix: "queued".into(),
                generation: 2,
            },
        )
        .unwrap();
    assert_eq!(
        &*pool
            .execute(neighbor, "neighbor.js", "6 * 7")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "42"
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: suspended.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    tree.wait().unwrap();
    completion.wait().unwrap();
    assert_eq!(
        &*pool
            .execute(
                runtime,
                "provider-order.js",
                "globalThis.providerEvents.join(',')"
            )
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "tree-await-start,tree-await-end,completion-queued"
    );
    pool.shutdown();
}

#[test]
fn dropped_removed_and_unloaded_provider_requests_release_runtime_state() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(64);
    let tree_registration = TreeProviderRegistrationId::new(95);
    let completion_registration = CompletionProviderRegistrationId::new(96);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    install_semantic_providers(&pool, runtime, tree_registration, completion_registration);

    let blocker = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: tree_registration,
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: Some("await".into()),
                generation: 2,
            },
        )
        .unwrap();
    let blocker_request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    let queued = pool
        .request_completions(
            runtime,
            CompletionRequest {
                registration: completion_registration,
                buffer: BufferHandle::new(1),
                revision: 1,
                cursor_byte_offset: 0,
                prefix: "queued-drop".into(),
                generation: 3,
            },
        )
        .unwrap();
    drop(queued);
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: blocker_request.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    blocker.wait().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::Idle);
    assert!(
        !pool
            .execute(
                runtime,
                "queued-provider-cancelled.js",
                "globalThis.providerEvents.join(',')",
            )
            .unwrap()
            .wait()
            .unwrap()
            .value
            .contains("completion-queued-drop")
    );

    let dropped = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: tree_registration,
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: Some("await".into()),
                generation: 4,
            },
        )
        .unwrap();
    let pending = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    drop(dropped);
    wait_for_state(&pool, runtime, ExtensionState::Idle);
    assert_eq!(
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: pending.id,
            result: Ok(clipboard_written()),
        }),
        Err(RuntimeResponseError::UnknownRequest)
    );

    let disposal = pool
        .execute(
            runtime,
            "dispose-providers.js",
            "Promise.all([globalThis.treeProvider.dispose(), globalThis.completionProvider.dispose()])",
        )
        .unwrap();
    for expected in [
        operation::<declared::unregister_tree_provider>(declared::TreeRegistrationArguments {
            registration: tree_registration,
        }),
        operation::<declared::unregister_completion_provider>(
            declared::CompletionRegistrationArguments {
                registration: completion_registration,
            },
        ),
    ] {
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(request.operation, expected);
        let result = match request.operation {
            operation
                if operation
                    .arguments::<declared::unregister_tree_provider>()
                    .is_some() =>
            {
                success::<declared::unregister_tree_provider>(())
            }
            operation
                if operation
                    .arguments::<declared::unregister_completion_provider>()
                    .is_some() =>
            {
                success::<declared::unregister_completion_provider>(())
            }
            _ => unreachable!(),
        };
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(result),
        })
        .unwrap();
    }
    disposal.wait().unwrap();
    let removed = pool
        .request_tree_children(
            runtime,
            TreeChildrenRequest {
                registration: tree_registration,
                window: "1".into(),
                view: crate::host::protocol::ViewId::new(1),
                parent_id: None,
                generation: 5,
            },
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(removed.result.unwrap_err().message.contains("disposed"));

    let removed_completion = pool
        .request_completions(
            runtime,
            CompletionRequest {
                registration: completion_registration,
                buffer: BufferHandle::new(1),
                revision: 1,
                cursor_byte_offset: 0,
                prefix: "removed".into(),
                generation: 6,
            },
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(
        removed_completion
            .result
            .unwrap_err()
            .message
            .contains("disposed")
    );

    let unload_runtime = key(65);
    let unload_tree = TreeProviderRegistrationId::new(97);
    let unload_completion = CompletionProviderRegistrationId::new(98);
    pool.load(unload_runtime, IsolateConfig::default()).unwrap();
    install_semantic_providers(&pool, unload_runtime, unload_tree, unload_completion);
    let pending_unload = pool
        .request_completions(
            unload_runtime,
            CompletionRequest {
                registration: unload_completion,
                buffer: BufferHandle::new(1),
                revision: 1,
                cursor_byte_offset: 0,
                prefix: "await".into(),
                generation: 7,
            },
        )
        .unwrap();
    let unload_request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    wait_for_state(&pool, unload_runtime, ExtensionState::AwaitingHostWork);
    pool.unload(unload_runtime).unwrap();
    assert_eq!(
        pending_unload.wait().unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );
    assert_eq!(
        pool.respond(HostResponse {
            extension: unload_runtime.extension,
            lifecycle: unload_runtime.lifecycle,
            id: unload_request.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8(""),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 0
                },
                revision: 1
            })),
        }),
        Err(RuntimeResponseError::WrongExtension)
    );
    pool.shutdown();
}

#[test]
fn command_registration_delivers_arguments_and_captured_buffer() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(51);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let registration = CommandRegistrationId::new(81);
    let setup = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/command-registration.js",
            r#"
                import * as knot from "knot"; const { commands } = knot;
                globalThis.commandRegistration = await commands.register(
                  "knot.fixture.arguments",
                  async (context) => {
                    globalThis.commandArguments = context.arguments;
                    globalThis.commandSnapshot = await context.buffer.snapshot();
                  },
                );
            "#,
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(request.invocation, None);
    assert_eq!(
        request.operation,
        operation::<declared::register_command>(declared::RegisterCommandArguments {
            name: "knot.fixture.arguments".into(),
            title: "knot.fixture.arguments".into(),
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::register_command>(registration)),
    })
    .unwrap();
    setup.wait().unwrap();

    let arguments = CommandArgumentValue::Object(BTreeMap::from([
        ("enabled".into(), CommandArgumentValue::Boolean(true)),
        (
            "values".into(),
            CommandArgumentValue::Array(vec![
                CommandArgumentValue::Null,
                CommandArgumentValue::Number(3.5),
                CommandArgumentValue::String("lambda-λ".into()),
            ]),
        ),
    ]));
    let invocation = CommandInvocationId::new(901);
    let execution = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: invocation,
                registration,
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments,
                capture: None,
            },
            Some(BufferHandle::new(17)),
        )
        .unwrap();
    let snapshot = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(snapshot.invocation, Some(invocation));
    assert_eq!(
        snapshot.operation,
        operation::<declared::snapshot>(declared::SnapshotArguments {
            buffer: BufferHandle::new(17),
            range: None,
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: snapshot.id,
        result: Ok(success::<declared::snapshot>(TextSnapshot {
            text: SnapshotText::from_utf8("captured"),
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 8,
            },
            revision: 4,
        })),
    })
    .unwrap();
    assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
    pool.execute(
        runtime,
        "verify-command-registration.js",
        r#"
            if (!globalThis.commandArguments.enabled
                || globalThis.commandArguments.values[1] !== 3.5
                || globalThis.commandArguments.values[2] !== "lambda-λ") {
              throw new Error("command arguments were not preserved");
            }
            if (globalThis.commandSnapshot.text !== "captured"
                || globalThis.commandSnapshot.revision !== 4) {
              throw new Error("captured buffer was not used");
            }
        "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    pool.shutdown();
}

#[test]
fn command_registration_errors_are_stable_and_disposal_removes_the_handler() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(52);
    pool.load(runtime, IsolateConfig::default()).unwrap();

    let duplicate = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/duplicate-command.js",
            r#"
                import * as knot from "knot"; const { commands } = knot;
                await commands.register("fixture.copy", () => {});
            "#,
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Err(test_failure("CommandNameInUse")),
    })
    .unwrap();
    let error = duplicate.wait().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Rejection);
    assert!(error.message().contains("CommandNameInUse"), "{error}");

    let registration = CommandRegistrationId::new(82);
    let setup = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/disposable-command.js",
            r#"
                import * as knot from "knot"; const { commands } = knot;
                globalThis.disposableCommand = await commands.register(
                  "knot.fixture.disposable",
                  () => { globalThis.disposedHandlerRan = true; },
                );
            "#,
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::register_command>(registration)),
    })
    .unwrap();
    setup.wait().unwrap();

    let disposal = pool
        .execute(
            runtime,
            "dispose-command.js",
            "globalThis.disposableCommand.dispose()",
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        request.operation,
        operation::<declared::unregister_command>(declared::CommandRegistrationArguments {
            registration
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::unregister_command>(())),
    })
    .unwrap();
    disposal.wait().unwrap();

    let outcome = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: CommandInvocationId::new(902),
                registration,
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: None,
            },
            None,
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::HandlerFailure { .. }));
    pool.execute(
        runtime,
        "verify-disposed-command.js",
        "if (globalThis.disposedHandlerRan) throw new Error('disposed handler ran')",
    )
    .unwrap()
    .wait()
    .unwrap();
    pool.shutdown();
}

#[test]
fn command_same_runtime_nesting_preserves_order_and_frame_attribution() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(53);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let outer_registration = CommandRegistrationId::new(83);
    let inner_registration = CommandRegistrationId::new(84);
    let setup = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/nested-commands.js",
            r#"
                import * as knot from "knot"; const { commands, editor } = knot;
                globalThis.commandEvents = [];
                await commands.register("knot.fixture.outer", async () => {
                  globalThis.commandEvents.push("outer-before");
                  const outcome = await commands.invoke("knot.fixture.inner", null);
                  globalThis.commandEvents.push(`inner-${outcome.kind}`);
                  await editor.activeBuffer();
                  globalThis.commandEvents.push("outer-after");
                });
                await commands.register("knot.fixture.inner", async (context) => {
                  globalThis.commandEvents.push("inner-before");
                  await context.buffer.snapshot();
                  globalThis.commandEvents.push("inner-after");
                });
            "#,
        )
        .unwrap();
    for registration in [outer_registration, inner_registration] {
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_command>(registration)),
        })
        .unwrap();
    }
    setup.wait().unwrap();

    let outer = CommandInvocationId::new(903);
    let inner = CommandInvocationId::new(904);
    let execution = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: outer,
                registration: outer_registration,
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: None,
            },
            Some(BufferHandle::new(23)),
        )
        .unwrap();
    let invoke = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(invoke.invocation, Some(outer));
    assert!(
        invoke
            .operation
            .arguments::<declared::invoke_command>()
            .is_some()
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: invoke.id,
        result: Ok(success::<declared::invoke_command>(
            CommandInvokeDispatch::Inline {
                invocation: inner,
                registration: inner_registration,
            },
        )),
    })
    .unwrap();

    let snapshot = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(snapshot.invocation, Some(inner));
    assert_eq!(
        snapshot.operation,
        operation::<declared::snapshot>(declared::SnapshotArguments {
            buffer: BufferHandle::new(23),
            range: None,
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: snapshot.id,
        result: Ok(success::<declared::snapshot>(TextSnapshot {
            text: SnapshotText::from_utf8("nested"),
            range: ByteRange {
                start_byte_offset: 0,
                end_byte_offset: 6,
            },
            revision: 1,
        })),
    })
    .unwrap();

    let completion = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(completion.invocation, Some(inner));
    assert_eq!(
        completion.operation,
        operation::<declared::complete_inline_command>(declared::InlineArguments {
            invocation: inner,
            outcome: CommandOutcome::Completed,
        })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: completion.id,
        result: Ok(success::<declared::complete_inline_command>(())),
    })
    .unwrap();

    let active = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(active.invocation, Some(outer));
    assert_eq!(
        active.operation,
        operation::<declared::active_buffer>(declared::ContextArguments { target: None })
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: active.id,
        result: Ok(success::<declared::active_buffer>(Some(BufferHandle::new(
            23,
        )))),
    })
    .unwrap();
    assert_eq!(execution.wait().unwrap(), CommandOutcome::Completed);
    assert_eq!(
        &*pool
            .execute(
                runtime,
                "nested-order.js",
                "globalThis.commandEvents.join(',')"
            )
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "outer-before,inner-before,inner-after,inner-completed,outer-after"
    );
    pool.shutdown();
}

#[test]
fn command_cancellation_rejects_suspended_host_work_and_prevents_late_resume() {
    assert_command_cancellation(false);
}

#[test]
fn command_cancellation_before_active_publication_is_not_lost() {
    assert_command_cancellation(true);
}

fn assert_command_cancellation(cancel_before_publication: bool) {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(54);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let registration = CommandRegistrationId::new(85);
    let setup = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/cancelled-command.js",
            r#"
                import * as knot from "knot"; const { commands } = knot;
                await commands.register("knot.fixture.cancelled", async (context) => {
                  if (!Object.isFrozen(context.signal) || "abort" in context.signal) throw new Error("unsupported signal interface");
                  let rejected = false;
                  try { context.signal.addEventListener("abort", null); } catch (error) { if (!(error instanceof TypeError)) throw error; rejected = true; }
                  if (!rejected) throw new Error("invalid listener accepted");
                  context.signal.addEventListener("abort", () => {
                    context.signal.addEventListener("abort", () => { globalThis.abortReplay = true; });
                    throw new Error("isolated abort listener failure");
                  });
                  context.signal.addEventListener("abort", (event) => {
                    if (!Object.isFrozen(event) || event.target !== context.signal || event.type !== "abort") throw new Error("invalid abort event");
                    globalThis.abortListenerRan = true;
                  });
                  await context.buffer.snapshot();
                  globalThis.cancelledCommandResumed = true;
                });
                globalThis.__knotCancelCommand = () => { throw new Error("untrusted dispatch replacement"); };
            "#,
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::register_command>(registration)),
    })
    .unwrap();
    setup.wait().unwrap();

    let gate = Arc::new(std::sync::Barrier::new(2));
    if cancel_before_publication {
        *pool
            .capsule(runtime)
            .unwrap()
            .local_state
            .command_start_gate
            .lock()
            .unwrap() = Some(gate.clone());
    }
    let invocation = CommandInvocationId::new(905);
    let execution = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: invocation,
                registration,
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: None,
            },
            Some(BufferHandle::new(29)),
        )
        .unwrap();
    let suspended = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(suspended.invocation, Some(invocation));
    if cancel_before_publication {
        gate.wait();
    }
    pool.cancel_command(runtime, invocation).unwrap();
    if cancel_before_publication {
        gate.wait();
    }
    assert_eq!(execution.wait().unwrap(), CommandOutcome::Cancelled);
    assert_eq!(
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: suspended.id,
            result: Ok(success::<declared::snapshot>(TextSnapshot {
                text: SnapshotText::from_utf8("late"),
                range: ByteRange {
                    start_byte_offset: 0,
                    end_byte_offset: 4,
                },
                revision: 1,
            })),
        }),
        Err(RuntimeResponseError::UnknownRequest)
    );
    pool.execute(
        runtime,
        "verify-command-cancellation.js",
        "if (globalThis.cancelledCommandResumed || !globalThis.abortListenerRan || globalThis.abortReplay) throw new Error('cancelled command or listener replay')",
    )
    .unwrap()
    .wait()
    .unwrap();
    pool.shutdown();
}

#[test]
fn command_cancellation_consumes_an_already_queued_response_without_ending_the_lifecycle() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(213);
    let blocker = key(214);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    pool.load(blocker, IsolateConfig::default()).unwrap();
    let registration = CommandRegistrationId::new(85);
    let setup = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/cancellation-response.js",
            r#"
import { commands, documents, workbench } from "knot";
await commands.register("cancel-response", async () => {
  try { await workbench.writeClipboardText("pending"); }
  catch (error) { globalThis.cancelledCode = error.code; await documents.list(); }
});
"#,
        )
        .unwrap();
    let request = pool.receive_request().unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: request.id,
        result: Ok(success::<declared::register_command>(registration)),
    })
    .unwrap();
    setup.wait().unwrap();
    let invocation = CommandInvocationId::new(905);
    let execution = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: invocation,
                registration,
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: None,
            },
            None,
        )
        .unwrap();
    let pending = pool.receive_request().unwrap();
    wait_for_state(&pool, runtime, ExtensionState::AwaitingHostWork);
    let gate = Arc::new(TestGate::default());
    pool.capsule(blocker)
        .unwrap()
        .install_test_barrier(gate.clone());
    let blocked = pool
        .execute(blocker, "block-worker.js", "__knotTestBarrier()")
        .unwrap();
    {
        let entered = gate.entered.lock().unwrap();
        let (_entered, timeout) = gate
            .ready
            .wait_timeout_while(entered, Duration::from_secs(1), |entered| *entered == 0)
            .unwrap();
        assert!(!timeout.timed_out());
    }
    pool.cancel_command(runtime, invocation).unwrap();
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: pending.id,
        result: Ok(clipboard_written()),
    })
    .unwrap();
    gate.wait();
    blocked.wait().unwrap();
    let followup = pool.receive_request().unwrap();
    assert_eq!(
        followup.operation,
        operation::<declared::list_documents>(())
    );
    pool.respond(HostResponse {
        extension: runtime.extension,
        lifecycle: runtime.lifecycle,
        id: followup.id,
        result: Ok(success::<declared::list_documents>(vec![])),
    })
    .unwrap();
    assert_eq!(execution.wait().unwrap(), CommandOutcome::Cancelled);
    assert_eq!(
        &*pool
            .execute(runtime, "check-lifecycle.js", "cancelledCode")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "Cancelled"
    );
    pool.shutdown();
}

#[test]
fn command_handlers_classify_invalid_arguments_and_failures() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(55);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let invalid_registration = CommandRegistrationId::new(86);
    let failing_registration = CommandRegistrationId::new(87);
    let setup = pool
        .execute_fixture_module(
            runtime,
            "file:///fixtures/command-failures.js",
            r#"
                import * as knot from "knot"; const { commands } = knot;
                await commands.register("knot.fixture.invalid", () => {
                  commands.invalidArguments("expected object argument");
                });
                await commands.register("knot.fixture.failure", () => {
                  throw new Error("expected handler failure");
                });
            "#,
        )
        .unwrap();
    for registration in [invalid_registration, failing_registration] {
        let request = pool
            .receive_request_timeout(Duration::from_secs(1))
            .unwrap();
        pool.respond(HostResponse {
            extension: runtime.extension,
            lifecycle: runtime.lifecycle,
            id: request.id,
            result: Ok(success::<declared::register_command>(registration)),
        })
        .unwrap();
    }
    setup.wait().unwrap();

    let invalid = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: CommandInvocationId::new(906),
                registration: invalid_registration,
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::String("wrong".into()),
                capture: None,
            },
            None,
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(matches!(
        invalid,
        CommandOutcome::InvalidArgument { ref message }
            if message.contains("expected object argument")
    ));

    let failure = pool
        .invoke_command(
            runtime,
            CommandInvocation {
                id: CommandInvocationId::new(907),
                registration: failing_registration,
                extension: runtime.extension,
                lifecycle: runtime.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: None,
            },
            None,
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(matches!(
        failure,
        CommandOutcome::HandlerFailure { ref message }
            if message.contains("expected handler failure")
    ));
    pool.shutdown();
}

#[test]
fn command_arguments_reject_non_json_values_before_dispatch() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(58);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    pool.execute_fixture_module(
        runtime,
        "file:///fixtures/invalid-command-arguments.js",
        r#"
            import * as knot from "knot"; const { commands } = knot;
            const cyclic = {};
            cyclic.self = cyclic;
            for (const value of [NaN, () => {}, cyclic]) {
              let rejected = false;
              try {
                await commands.invoke("knot.fixture.missing", value);
              } catch (error) {
                rejected = error instanceof TypeError;
              }
              if (!rejected) throw new Error("invalid command arguments were accepted");
            }
        "#,
    )
    .unwrap()
    .wait()
    .unwrap();
    assert!(
        pool.receive_request_timeout(Duration::from_millis(20))
            .is_none()
    );
    pool.shutdown();
}

#[test]
fn command_forced_termination_is_fatal_and_does_not_poison_a_neighbor() {
    let pool = RuntimePool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
    let runaway = key(56);
    let neighbor = key(57);
    pool.load(runaway, IsolateConfig::default()).unwrap();
    pool.load(neighbor, IsolateConfig::default()).unwrap();
    let registration = CommandRegistrationId::new(88);
    let setup = pool
        .execute_fixture_module(
            runaway,
            "file:///fixtures/runaway-command.js",
            r#"
                import * as knot from "knot"; const { commands } = knot;
                await commands.register("knot.fixture.runaway", () => {
                  __knotTestEntered();
                  while (true) {}
                });
            "#,
        )
        .unwrap();
    let request = pool
        .receive_request_timeout(Duration::from_secs(1))
        .unwrap();
    pool.respond(HostResponse {
        extension: runaway.extension,
        lifecycle: runaway.lifecycle,
        id: request.id,
        result: Ok(success::<declared::register_command>(registration)),
    })
    .unwrap();
    setup.wait().unwrap();

    let (entered_sender, entered) = mpsc::channel();
    pool.capsule(runaway)
        .unwrap()
        .install_test_entered_signal(entered_sender);
    let termination = pool.termination_handle(runaway).unwrap();
    let execution = pool
        .invoke_command(
            runaway,
            CommandInvocation {
                id: CommandInvocationId::new(908),
                registration,
                extension: runaway.extension,
                lifecycle: runaway.lifecycle,
                arguments: CommandArgumentValue::Null,
                capture: None,
            },
            None,
        )
        .unwrap();
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(termination.terminate());
    assert_eq!(
        execution.wait().unwrap_err().kind(),
        RuntimeErrorKind::Terminated
    );
    assert_eq!(
        &*pool
            .execute(neighbor, "neighbor-after-command.js", "6 * 7")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "42"
    );
    assert!(pool.execute(runaway, "late-command.js", "1").is_err());
    pool.shutdown();
}

#[test]
fn forced_termination_is_fatal_and_does_not_poison_a_neighbor() {
    let pool = RuntimePool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
    let runaway = key(1);
    let neighbor = key(2);
    pool.load(runaway, IsolateConfig::default()).unwrap();
    pool.load(neighbor, IsolateConfig::default()).unwrap();
    let (entered_sender, entered) = mpsc::channel();
    pool.capsule(runaway)
        .unwrap()
        .install_test_entered_signal(entered_sender);
    let retained: Arc<[u16]> = "worker-owned".encode_utf16().collect::<Vec<_>>().into();
    let retained_weak = Arc::downgrade(&retained);
    pool.capsule(runaway)
        .unwrap()
        .hold_external_utf16("held", retained)
        .unwrap();
    let termination = pool.termination_handle(runaway).unwrap();
    let execution = pool
        .execute(
            runaway,
            "runaway.js",
            "__knotTestEntered(); while (true) {}",
        )
        .unwrap();
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(termination.terminate());
    let error = execution.wait().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Terminated);
    assert!(retained_weak.upgrade().is_none());
    assert_eq!(
        &*pool
            .execute(neighbor, "neighbor.js", "6 * 7")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "42"
    );
    assert!(pool.execute(runaway, "late.js", "1").is_err());
    pool.shutdown();
}

#[test]
fn unload_terminates_running_javascript_before_disposal() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let (entered_sender, entered) = mpsc::channel();
    pool.capsule(runtime)
        .unwrap()
        .install_test_entered_signal(entered_sender);
    let execution = pool
        .execute(
            runtime,
            "runaway.js",
            "__knotTestEntered(); while (true) {}",
        )
        .unwrap();
    let queued = pool.execute(runtime, "queued.js", "1").unwrap();
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    pool.unload(runtime).unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    let error = execution.wait().unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::Terminated);
    assert_eq!(
        queued.wait().unwrap_err().kind(),
        RuntimeErrorKind::Cancelled
    );
    assert!(pool.execute(runtime, "late.js", "1").is_err());
    pool.shutdown();
}

#[test]
fn unload_releases_external_state_before_returning() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let runtime = key(1);
    pool.load(runtime, IsolateConfig::default()).unwrap();
    let text: Arc<[u16]> = "retained".encode_utf16().collect::<Vec<_>>().into();
    let weak = Arc::downgrade(&text);
    pool.capsule(runtime)
        .unwrap()
        .hold_external_utf16("held", Arc::clone(&text))
        .unwrap();
    drop(text);
    assert_eq!(weak.strong_count(), 1);
    pool.unload(runtime).unwrap();
    assert!(weak.upgrade().is_none());
    pool.shutdown();
}

#[test]
fn stale_unload_cannot_dispose_a_replacement_lifecycle() {
    let pool = RuntimePool::new(PoolConfig::single_worker());
    let old = key(1);
    let replacement = ExtensionKey::new(old.extension, ExtensionLifecycleId::new(2));
    pool.load(old, IsolateConfig::default()).unwrap();
    pool.unload(old).unwrap();
    pool.load(replacement, IsolateConfig::default()).unwrap();

    assert!(matches!(
        pool.unload(old),
        Err(RuntimePoolError::Scheduler(
            SchedulerError::StaleLifecycle { .. }
        ))
    ));
    assert_eq!(
        &*pool
            .execute(replacement, "replacement.js", "6 * 7")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "42"
    );
    pool.shutdown();
}

#[test]
fn heap_exhaustion_probe_runs_in_a_sacrificial_process() {
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("host::engine::tests::heap_exhaustion_probe_child")
        .arg("--nocapture")
        .env("KNOT_RUN_HEAP_EXHAUSTION_PROBE", "1")
        .status()
        .expect("heap-exhaustion probe process must start");
    assert!(status.success(), "heap limit must fail only its isolate");
}

#[test]
fn heap_exhaustion_probe_child() {
    if std::env::var_os("KNOT_RUN_HEAP_EXHAUSTION_PROBE").is_none() {
        return;
    }
    let pool = RuntimePool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
    let exhausted = key(1);
    let neighbor = key(2);
    pool.load(exhausted, IsolateConfig::with_heap_limit(5 * 1024 * 1024))
        .unwrap();
    pool.load(neighbor, IsolateConfig::default()).unwrap();
    let error = pool
        .execute(
            exhausted,
            "heap.js",
            "let value = ''; while (true) { value += '0123456789abcdef'; }",
        )
        .unwrap()
        .wait()
        .unwrap_err();
    assert_eq!(error.kind(), RuntimeErrorKind::HeapLimitExceeded);
    assert!(pool.execute(exhausted, "late.js", "1").is_err());
    assert_eq!(
        &*pool
            .execute(neighbor, "neighbor.js", "6 * 7")
            .unwrap()
            .wait()
            .unwrap()
            .value,
        "42"
    );
    pool.shutdown();
}
