//! Product-facing ownership and controls for the pooled extension host.

use std::sync::Arc;

use super::{
    engine::{self, EngineEvent, LifecycleEnd, RuntimePool},
    lifecycle::ExtensionKey,
    protocol::{
        BufferChange, BufferHandle, BufferSubscriptionId, CommandInvocation, CommandInvocationId,
        CompletionRequest, HostRequest, HostResponse, TreeChildrenRequest,
    },
    scheduler::PoolConfig,
};

pub(crate) use super::engine::{
    BufferChangeQueueMetrics, RuntimeCommandExecution as ExtensionCommandExecution,
    RuntimeExecution as ExtensionExecution, RuntimePoolError as ExtensionPoolError,
    RuntimeProviderExecution as ExtensionProviderExecution,
    RuntimeResponseError as ExtensionResponseError,
};

/// Configuration for one persistent extension isolate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ExtensionConfig(engine::IsolateConfig);

impl ExtensionConfig {
    #[cfg(test)]
    pub(crate) const fn with_heap_limit(heap_limit_bytes: usize) -> Self {
        Self(engine::IsolateConfig::with_heap_limit(heap_limit_bytes))
    }
}

/// Why one admitted extension lifetime ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExtensionExit {
    StartupFailed,
    Unloaded,
    Failed(super::lifecycle::Failure),
    Shutdown,
}

/// Event emitted by the pool for foreground processing.
#[derive(Clone, Debug)]
pub(crate) enum ExtensionEvent {
    Request(HostRequest),
    LifecycleEnded {
        key: ExtensionKey,
        reason: ExtensionExit,
    },
}

/// Unique, awaitable event stream for the application foreground.
pub(crate) struct ExtensionEventInbox(engine::EngineEventInbox);

impl ExtensionEventInbox {
    pub(crate) async fn receive(&mut self) -> Option<ExtensionEvent> {
        self.0.receive().await.map(|event| match event {
            EngineEvent::Request(request) => ExtensionEvent::Request(request),
            EngineEvent::LifecycleEnded { key, reason } => ExtensionEvent::LifecycleEnded {
                key,
                reason: match reason {
                    LifecycleEnd::StartupFailed => ExtensionExit::StartupFailed,
                    LifecycleEnd::Unloaded => ExtensionExit::Unloaded,
                    LifecycleEnd::Failed(failure) => ExtensionExit::Failed(failure),
                    LifecycleEnd::Shutdown => ExtensionExit::Shutdown,
                },
            },
        })
    }

    #[cfg(test)]
    fn receive_timeout(&mut self, timeout: std::time::Duration) -> Option<ExtensionEvent> {
        self.0.receive_timeout(timeout).map(|event| match event {
            EngineEvent::Request(request) => ExtensionEvent::Request(request),
            EngineEvent::LifecycleEnded { key, reason } => ExtensionEvent::LifecycleEnded {
                key,
                reason: match reason {
                    LifecycleEnd::StartupFailed => ExtensionExit::StartupFailed,
                    LifecycleEnd::Unloaded => ExtensionExit::Unloaded,
                    LifecycleEnd::Failed(failure) => ExtensionExit::Failed(failure),
                    LifecycleEnd::Shutdown => ExtensionExit::Shutdown,
                },
            },
        })
    }
}

/// One application-owned bounded worker pool.
pub(crate) struct ExtensionPool {
    engine: RuntimePool,
}

#[allow(
    dead_code,
    reason = "fixture diagnostics and native semantic surfaces use subsets of the pooled control API"
)]
impl ExtensionPool {
    pub(crate) fn new(config: PoolConfig) -> (Arc<Self>, ExtensionEventInbox) {
        let engine = RuntimePool::new(config);
        let inbox = engine
            .take_event_inbox()
            .expect("a new extension pool owns its event inbox");
        (Arc::new(Self { engine }), ExtensionEventInbox(inbox))
    }

    pub(crate) fn load(
        &self,
        key: ExtensionKey,
        config: ExtensionConfig,
    ) -> Result<(), ExtensionPoolError> {
        self.engine.load(key, config.0)
    }

    pub(crate) fn execute_fixture_module(
        &self,
        key: ExtensionKey,
        specifier: impl Into<Arc<str>>,
        source: impl Into<Arc<str>>,
    ) -> Result<ExtensionExecution, ExtensionPoolError> {
        self.engine.execute_fixture_module(key, specifier, source)
    }

    #[cfg(test)]
    fn panic_worker_turn(
        &self,
        key: ExtensionKey,
    ) -> Result<ExtensionExecution, ExtensionPoolError> {
        self.engine.panic_worker_turn(key)
    }

    pub(crate) fn invoke_command(
        &self,
        key: ExtensionKey,
        invocation: CommandInvocation,
        active_buffer: Option<BufferHandle>,
    ) -> Result<ExtensionCommandExecution, ExtensionPoolError> {
        self.engine.invoke_command(key, invocation, active_buffer)
    }

    pub(crate) fn request_tree_children(
        &self,
        key: ExtensionKey,
        request: TreeChildrenRequest,
    ) -> Result<ExtensionProviderExecution<super::protocol::TreeChildrenResponse>, ExtensionPoolError>
    {
        self.engine.request_tree_children(key, request)
    }

    pub(crate) fn request_completions(
        &self,
        key: ExtensionKey,
        request: CompletionRequest,
    ) -> Result<ExtensionProviderExecution<super::protocol::CompletionResponse>, ExtensionPoolError>
    {
        self.engine.request_completions(key, request)
    }

    pub(crate) fn cancel_command(
        &self,
        key: ExtensionKey,
        invocation: CommandInvocationId,
    ) -> Result<(), ExtensionPoolError> {
        self.engine.cancel_command(key, invocation)
    }

    pub(crate) fn respond(&self, response: HostResponse) -> Result<(), ExtensionResponseError> {
        self.engine.respond(response)
    }

    pub(crate) fn dispatch_buffer_change(
        &self,
        key: ExtensionKey,
        subscription: BufferSubscriptionId,
        change: BufferChange,
    ) -> Result<(), ExtensionPoolError> {
        self.engine
            .dispatch_buffer_change(key, subscription, change)
    }

    pub(crate) fn buffer_change_queue_metrics(
        &self,
        key: ExtensionKey,
    ) -> Result<BufferChangeQueueMetrics, ExtensionPoolError> {
        self.engine.buffer_change_queue_metrics(key)
    }

    pub(crate) fn diagnostics(&self) -> super::scheduler::SchedulerDiagnostics {
        self.engine.diagnostics()
    }

    pub(crate) fn watchdog(
        &self,
        key: ExtensionKey,
    ) -> Result<ExtensionWatchdog, ExtensionPoolError> {
        self.engine
            .termination_handle(key)
            .map(|handle| ExtensionWatchdog { handle })
    }

    pub(crate) fn unload(&self, key: ExtensionKey) -> Result<(), ExtensionPoolError> {
        self.engine.unload(key)
    }

    pub(crate) fn shutdown(self) {
        self.engine.shutdown();
    }
}

/// Cloneable, non-V8 watchdog for forcefully interrupting one isolate.
#[allow(
    dead_code,
    reason = "watchdogs are issued to fixture and product lifecycle owners"
)]
pub(crate) struct ExtensionWatchdog {
    handle: engine::TerminationHandle,
}

#[allow(
    dead_code,
    reason = "watchdogs are issued to fixture and product lifecycle owners"
)]
impl ExtensionWatchdog {
    pub(crate) fn terminate(&self) -> bool {
        self.handle.terminate()
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, time::Duration};

    use crate::host::protocol::{ExtensionId, ExtensionLifecycleId};

    use super::*;

    fn key(extension: u64, lifecycle: u64) -> ExtensionKey {
        ExtensionKey::new(
            ExtensionId::new(extension),
            ExtensionLifecycleId::new(lifecycle),
        )
    }

    #[test]
    fn pool_owner_exposes_async_events_diagnostics_and_exact_once_unload() {
        let (pool, mut inbox) = ExtensionPool::new(PoolConfig::new(NonZeroUsize::new(2).unwrap()));
        let first = key(1, 1);
        let second = key(2, 1);
        pool.load(first, ExtensionConfig::default()).unwrap();
        pool.load(second, ExtensionConfig::default()).unwrap();

        let diagnostics = pool.diagnostics();
        assert_eq!(diagnostics.worker_count, 2);
        assert_eq!(diagnostics.extensions.len(), 2);

        let execution = pool
            .execute_fixture_module(
                first,
                "file:///fixtures/pool-owner.js",
                "globalThis.poolOwnerProbe = 42;",
            )
            .unwrap();
        execution.wait().unwrap();

        pool.unload(first).unwrap();
        assert!(matches!(
            inbox.receive_timeout(Duration::from_secs(1)),
            Some(ExtensionEvent::LifecycleEnded {
                key,
                reason: ExtensionExit::Unloaded,
            }) if key == first
        ));
        assert!(pool.unload(first).is_err());
        assert!(inbox.receive_timeout(Duration::from_millis(20)).is_none());

        pool.unload(second).unwrap();
        assert!(matches!(
            inbox.receive_timeout(Duration::from_secs(1)),
            Some(ExtensionEvent::LifecycleEnded { key, .. }) if key == second
        ));
        let pool = Arc::try_unwrap(pool).unwrap_or_else(|_| panic!("pool owner leaked"));
        pool.shutdown();
    }

    #[test]
    fn watchdog_is_lifecycle_scoped_and_v8_free_to_call() {
        let (pool, _inbox) = ExtensionPool::new(PoolConfig::single_worker());
        let runtime = key(1, 1);
        pool.load(runtime, ExtensionConfig::with_heap_limit(32 * 1024 * 1024))
            .unwrap();
        let watchdog = pool.watchdog(runtime).unwrap();
        assert!(watchdog.terminate());
        assert!(!watchdog.terminate());
        drop(watchdog);
        drop(pool);
    }

    #[test]
    fn worker_panic_finalizes_only_the_failed_lifecycle() {
        let (pool, mut inbox) = ExtensionPool::new(PoolConfig::single_worker());
        let failed = key(1, 1);
        let neighbor = key(2, 1);
        pool.load(failed, ExtensionConfig::default()).unwrap();
        pool.load(neighbor, ExtensionConfig::default()).unwrap();

        let execution = pool.panic_worker_turn(failed).unwrap();
        assert!(execution.wait().is_err());
        assert!(matches!(
            inbox.receive_timeout(Duration::from_secs(1)),
            Some(ExtensionEvent::LifecycleEnded {
                key,
                reason: ExtensionExit::Failed(_),
            }) if key == failed
        ));
        assert!(pool.watchdog(failed).is_err());

        pool.execute_fixture_module(
            neighbor,
            "file:///fixtures/panic-neighbor.js",
            "globalThis.neighborSurvived = true;",
        )
        .unwrap()
        .wait()
        .unwrap();
        pool.unload(neighbor).unwrap();
    }
}
