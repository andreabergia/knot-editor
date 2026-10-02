//! Dependency planning and startup outcomes for installed extensions.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;

use super::extension_package::{DiscoveryReport, InstalledPackage};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadResult {
    Loaded,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadEntry {
    pub directory: PathBuf,
    pub name: Option<String>,
    pub result: LoadResult,
}

#[derive(Clone, Default)]
pub struct LoadReport {
    /// Validation failures first, then package attempts in deterministic ready order.
    pub entries: Vec<LoadEntry>,
}

/// A started lifecycle. Dropping it before successful entry completion rolls it back.
pub struct StartupAttempt<F: Future<Output = Result<(), String>>, C: FnOnce()> {
    completion: Option<F>,
    rollback: Option<C>,
}

impl<F: Future<Output = Result<(), String>>, C: FnOnce()> StartupAttempt<F, C> {
    pub fn new(completion: F, rollback: C) -> Self {
        Self {
            completion: Some(completion),
            rollback: Some(rollback),
        }
    }

    async fn finish(mut self) -> Result<(), String> {
        let result = self.completion.take().unwrap().await;
        if result.is_ok() {
            self.rollback.take();
        }
        result
    }
}

impl<F: Future<Output = Result<(), String>>, C: FnOnce()> Drop for StartupAttempt<F, C> {
    fn drop(&mut self) {
        if let Some(rollback) = self.rollback.take() {
            rollback();
        }
    }
}

pub struct DependencyPlan {
    packages: BTreeMap<String, InstalledPackage>,
    failures: BTreeMap<String, String>,
    diagnostics: Vec<LoadEntry>,
}

impl DependencyPlan {
    pub fn new(discovery: DiscoveryReport) -> Self {
        let packages = discovery
            .packages
            .into_iter()
            .map(|package| (package.manifest.name.clone(), package))
            .collect::<BTreeMap<_, _>>();
        let mut failures = BTreeMap::new();
        for (name, package) in &packages {
            if package
                .manifest
                .requires
                .iter()
                .any(|dependency| dependency == name)
            {
                failures.insert(name.clone(), format!("self-dependency: {name}"));
            } else if let Some(missing) = package
                .manifest
                .requires
                .iter()
                .find(|dependency| !packages.contains_key(*dependency))
            {
                failures.insert(
                    name.clone(),
                    format!("missing required extension {missing}"),
                );
            }
        }
        for component in cyclic_components(&packages) {
            let cause = format!("dependency cycle among: {}", component.join(", "));
            for name in component {
                failures.entry(name).or_insert_with(|| cause.clone());
            }
        }
        let mut diagnostics = discovery
            .diagnostics
            .into_iter()
            .map(|diagnostic| LoadEntry {
                directory: diagnostic.directory,
                name: diagnostic.name,
                result: LoadResult::Failed(diagnostic.cause),
            })
            .collect::<Vec<_>>();
        diagnostics.sort_by(|left, right| left.directory.cmp(&right.directory));
        Self {
            packages,
            failures,
            diagnostics,
        }
    }

    pub async fn execute<Start, F, C>(self, start: Start) -> LoadReport
    where
        Start: FnMut(&InstalledPackage) -> StartupAttempt<F, C>,
        F: Future<Output = Result<(), String>>,
        C: FnOnce(),
    {
        self.execute_with_progress(start, |_| {}).await
    }

    pub async fn execute_with_progress<Start, F, C, Progress>(
        self,
        mut start: Start,
        mut on_entry: Progress,
    ) -> LoadReport
    where
        Start: FnMut(&InstalledPackage) -> StartupAttempt<F, C>,
        F: Future<Output = Result<(), String>>,
        C: FnOnce(),
        Progress: FnMut(&LoadEntry),
    {
        let mut entries = Vec::new();
        let mut record = |entry| {
            on_entry(&entry);
            entries.push(entry);
        };
        for diagnostic in self.diagnostics {
            record(diagnostic);
        }
        let mut outcomes = BTreeMap::<String, Result<(), String>>::new();
        for (name, cause) in self.failures {
            let package = &self.packages[&name];
            record(LoadEntry {
                directory: package.directory.clone(),
                name: Some(name.clone()),
                result: LoadResult::Failed(cause.clone()),
            });
            outcomes.insert(name, Err(cause));
        }
        while outcomes.len() < self.packages.len() {
            let (name, package) = self
                .packages
                .iter()
                .find(|(name, package)| {
                    !outcomes.contains_key(*name)
                        && package
                            .manifest
                            .requires
                            .iter()
                            .all(|dependency| outcomes.contains_key(dependency))
                })
                .expect("all remaining packages have resolved dependencies");
            let result = if let Some((dependency, Err(cause))) = package
                .manifest
                .requires
                .iter()
                .filter_map(|dependency| {
                    outcomes
                        .get(dependency)
                        .map(|outcome| (dependency, outcome))
                })
                .find(|(_, outcome)| outcome.is_err())
            {
                Err(format!(
                    "required extension {dependency} did not load: {cause}"
                ))
            } else {
                start(package).finish().await
            };
            record(LoadEntry {
                directory: package.directory.clone(),
                name: Some(name.clone()),
                result: match &result {
                    Ok(()) => LoadResult::Loaded,
                    Err(cause) => LoadResult::Failed(cause.clone()),
                },
            });
            outcomes.insert(name.clone(), result);
        }
        LoadReport { entries }
    }
}

fn cyclic_components(packages: &BTreeMap<String, InstalledPackage>) -> Vec<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut finished = Vec::new();
    for name in packages.keys() {
        let mut stack = vec![(name.clone(), false)];
        while let Some((node, exit)) = stack.pop() {
            if exit {
                finished.push(node);
            } else if seen.insert(node.clone()) {
                stack.push((node.clone(), true));
                let mut dependencies = packages[&node]
                    .manifest
                    .requires
                    .iter()
                    .filter(|dependency| packages.contains_key(*dependency))
                    .cloned()
                    .collect::<Vec<_>>();
                dependencies.sort();
                for dependency in dependencies.into_iter().rev() {
                    stack.push((dependency, false));
                }
            }
        }
    }
    let mut reverse = packages
        .keys()
        .map(|name| (name.clone(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for (name, package) in packages {
        for dependency in &package.manifest.requires {
            if let Some(dependents) = reverse.get_mut(dependency) {
                dependents.push(name.clone());
            }
        }
    }
    let mut assigned = BTreeSet::new();
    let mut cycles = Vec::new();
    for name in finished.into_iter().rev() {
        if !assigned.insert(name.clone()) {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![name];
        while let Some(node) = stack.pop() {
            component.push(node.clone());
            for dependent in &reverse[&node] {
                if assigned.insert(dependent.clone()) {
                    stack.push(dependent.clone());
                }
            }
        }
        if component.len() > 1 {
            component.sort();
            cycles.push(component);
        }
    }
    cycles.sort();
    cycles
}

#[cfg(test)]
mod tests {
    use std::future::{Future, pending, ready};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use super::*;
    use crate::app::extension_package::{Manifest, discover};
    use crate::host::{
        lifecycle::{ExtensionKey, ExtensionState},
        module_graph::ModuleGraph,
        pool::{ExtensionConfig, ExtensionEvent, ExtensionExit, ExtensionPool},
        protocol::{ExtensionId, ExtensionLifecycleId},
        scheduler::PoolConfig,
    };

    fn package(name: &str, requires: &[&str]) -> InstalledPackage {
        InstalledPackage {
            directory: PathBuf::from(name),
            manifest: Manifest {
                name: name.into(),
                version: "1.0.0".into(),
                main: "main.js".into(),
                requires: requires
                    .iter()
                    .map(|dependency| (*dependency).into())
                    .collect(),
                display_name: None,
                description: None,
                author: None,
                website: None,
                license: None,
            },
            sources: BTreeMap::new(),
        }
    }

    fn plan(packages: Vec<InstalledPackage>) -> DependencyPlan {
        DependencyPlan::new(DiscoveryReport {
            packages,
            diagnostics: Vec::new(),
        })
    }

    #[test]
    fn loads_ready_packages_in_stable_dependency_order() {
        let plan = plan(vec![
            package("@test/d", &["@test/b", "@test/c"]),
            package("@test/c", &[]),
            package("@test/b", &["@test/a"]),
            package("@test/a", &[]),
        ]);
        let mut started = Vec::new();
        let report = pollster::block_on(plan.execute(|package| {
            started.push(package.manifest.name.clone());
            StartupAttempt::new(ready(Ok(())), || {})
        }));
        assert_eq!(started, ["@test/a", "@test/b", "@test/c", "@test/d"]);
        assert!(
            report
                .entries
                .iter()
                .all(|entry| entry.result == LoadResult::Loaded)
        );
    }

    #[test]
    fn reports_missing_self_and_cycle_without_blocking_independent_packages() {
        let plan = plan(vec![
            package("@test/a", &["@test/b"]),
            package("@test/b", &["@test/a"]),
            package("@test/c", &["@test/b"]),
            package("@test/d", &[]),
            package("@test/missing", &["@test/absent"]),
            package("@test/self", &["@test/self"]),
        ]);
        let mut started = Vec::new();
        let report = pollster::block_on(plan.execute(|package| {
            started.push(package.manifest.name.clone());
            StartupAttempt::new(ready(Ok(())), || {})
        }));
        assert_eq!(started, ["@test/d"]);
        let result = |name| {
            &report
                .entries
                .iter()
                .find(|entry| entry.name.as_deref() == Some(name))
                .unwrap()
                .result
        };
        assert!(
            matches!(result("@test/a"), LoadResult::Failed(cause) if cause.contains("dependency cycle among"))
        );
        assert!(
            matches!(result("@test/b"), LoadResult::Failed(cause) if cause.contains("@test/a"))
        );
        assert!(
            matches!(result("@test/c"), LoadResult::Failed(cause) if cause.contains("@test/b"))
        );
        assert_eq!(result("@test/d"), &LoadResult::Loaded);
        assert!(
            matches!(result("@test/missing"), LoadResult::Failed(cause) if cause.contains("@test/absent"))
        );
        assert!(
            matches!(result("@test/self"), LoadResult::Failed(cause) if cause.contains("self-dependency"))
        );
    }

    #[test]
    fn identifies_each_cycle_without_marking_its_dependents_as_members() {
        let packages = vec![
            package("@test/a", &["@test/b"]),
            package("@test/b", &["@test/c"]),
            package("@test/c", &["@test/a"]),
            package("@test/d", &["@test/b"]),
            package("@test/e", &["@test/f"]),
            package("@test/f", &["@test/e"]),
        ]
        .into_iter()
        .map(|package| (package.manifest.name.clone(), package))
        .collect();
        assert_eq!(
            cyclic_components(&packages),
            [
                vec!["@test/a", "@test/b", "@test/c"],
                vec!["@test/e", "@test/f"]
            ]
        );
    }

    #[test]
    fn startup_failure_rolls_back_and_propagates_to_dependents() {
        let plan = plan(vec![
            package("@test/a", &[]),
            package("@test/b", &["@test/a"]),
            package("@test/c", &[]),
        ]);
        let rolled_back = Arc::new(AtomicUsize::new(0));
        let mut started = Vec::new();
        let report = pollster::block_on(plan.execute(|package| {
            let name = package.manifest.name.clone();
            started.push(name.clone());
            let rollback = rolled_back.clone();
            StartupAttempt::new(
                ready(if name == "@test/a" {
                    Err("entry rejected".into())
                } else {
                    Ok(())
                }),
                move || {
                    rollback.fetch_add(1, Ordering::SeqCst);
                },
            )
        }));
        assert_eq!(started, ["@test/a", "@test/c"]);
        assert_eq!(rolled_back.load(Ordering::SeqCst), 1);
        assert_eq!(
            report
                .entries
                .iter()
                .map(|entry| entry.name.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["@test/a", "@test/b", "@test/c"]
        );
        assert!(
            matches!(&report.entries[1].result, LoadResult::Failed(cause) if cause.contains("entry rejected"))
        );
        assert_eq!(report.entries[2].result, LoadResult::Loaded);
    }

    #[test]
    fn duplicate_dependency_declaration_is_a_discovery_failure() {
        let root = tempfile::tempdir().unwrap();
        let invalid = root.path().join("@test/invalid");
        std::fs::create_dir_all(&invalid).unwrap();
        std::fs::write(invalid.join("main.js"), "export {};").unwrap();
        std::fs::write(
            invalid.join("knot.jsonc"),
            r#"{
            "name":"@test/invalid", "version":"1.0.0", "main":"main.js",
            "requires":["@test/base", "@test/base"]
        }"#,
        )
        .unwrap();
        let report = pollster::block_on(
            DependencyPlan::new(discover(root.path()))
                .execute(|_| StartupAttempt::new(ready(Ok(())), || {})),
        );
        assert!(
            matches!(&report.entries[0].result, LoadResult::Failed(cause) if cause.contains("duplicate requires"))
        );
    }

    struct Noop;
    impl Wake for Noop {
        fn wake(self: Arc<Self>) {}
    }

    #[test]
    fn cancellation_drops_the_active_startup_and_rolls_it_back() {
        let rolled_back = Arc::new(AtomicUsize::new(0));
        let plan = plan(vec![package("@test/a", &[])]);
        let mut run = Box::pin(plan.execute(|_| {
            let rollback = rolled_back.clone();
            StartupAttempt::new(pending::<Result<(), String>>(), move || {
                rollback.fetch_add(1, Ordering::SeqCst);
            })
        }));
        let waker = Waker::from(Arc::new(Noop));
        assert!(matches!(
            run.as_mut().poll(&mut Context::from_waker(&waker)),
            Poll::Pending
        ));
        drop(run);
        assert_eq!(rolled_back.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_entry_rolls_back_the_existing_host_lifecycle() {
        let (pool, mut inbox) = ExtensionPool::new(PoolConfig::single_worker());
        let key = ExtensionKey::new(ExtensionId::new(1), ExtensionLifecycleId::new(1));
        let plan = plan(vec![package("@test/a", &[])]);
        let graph = ModuleGraph::new(
            "file:///extensions/%40test/a/",
            "main.js",
            BTreeMap::from([(
                "main.js".into(),
                Arc::from("throw new Error('startup failed');"),
            )]),
        )
        .unwrap();
        let owner = pool.clone();
        let report = pollster::block_on(plan.execute(move |_| {
            let runner = owner.clone();
            let rollback = owner.clone();
            let graph = graph.clone();
            StartupAttempt::new(
                async move {
                    runner
                        .load_package(key, ExtensionConfig::default(), graph)
                        .map_err(|error| error.to_string())?
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(())
                },
                move || {
                    let _ = rollback.unload(key);
                },
            )
        }));
        assert!(
            matches!(&report.entries[0].result, LoadResult::Failed(cause) if cause.contains("startup failed"))
        );
        assert!(
            matches!(pollster::block_on(inbox.receive()), Some(ExtensionEvent::LifecycleEnded {
            key: ended,
            reason: ExtensionExit::Unloaded,
        }) if ended == key)
        );
        assert_eq!(
            pool.diagnostics().extensions[0].state,
            ExtensionState::Stopping
        );
        assert!(pool.unload(key).is_err());
    }
}
