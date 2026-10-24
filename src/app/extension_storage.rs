//! Application-global JSON records, with one serialized SQLite connection per owner.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::{BackgroundExecutor, Task};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;

use super::extension_package::{validate_bundled_name, validate_name};

const APPLICATION_ID: i64 = 0x4b4e4f54;
const SCHEMA: &str = "CREATE TABLE records (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL CHECK(json_valid(value))) STRICT";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct StorageOwner(Owner);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Owner {
    Package(String),
    PersonalConfig,
}

impl StorageOwner {
    /// Accept only canonical installed-package identities supplied by the application.
    pub(crate) fn package(name: &str) -> Result<Self, String> {
        validate_name(name)?;
        Ok(Self(Owner::Package(name.to_owned())))
    }

    /// Accept a canonical built-in identity supplied by the application.
    pub(crate) fn bundled(name: &str) -> Result<Self, String> {
        validate_bundled_name(name)?;
        Ok(Self(Owner::Package(name.to_owned())))
    }

    pub(crate) fn personal_config() -> Self {
        Self(Owner::PersonalConfig)
    }

    fn relative_path(&self) -> PathBuf {
        match &self.0 {
            Owner::Package(name) => {
                let (scope, package) = name.split_once('/').unwrap();
                PathBuf::from("packages")
                    .join(scope)
                    .join(format!("{package}.sqlite3"))
            }
            Owner::PersonalConfig => PathBuf::from("personal-config.sqlite3"),
        }
    }
}

#[derive(Debug)]
pub(crate) enum StorageError {
    Cancelled,
    Unavailable { database: PathBuf, cause: String },
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "storage caller cancelled"),
            Self::Unavailable { database, cause } => {
                write!(f, "storage {}: {cause}", database.display())
            }
        }
    }
}

impl std::error::Error for StorageError {}

/// Constructing and binding owners performs no I/O. All operations, including lazy
/// schema initialization and connection locking, run on the background executor.
pub(crate) struct ExtensionStorage {
    root: PathBuf,
    executor: BackgroundExecutor,
    databases: Mutex<BTreeMap<StorageOwner, Arc<Database>>>,
    work: Arc<Mutex<WorkState>>,
}

impl ExtensionStorage {
    #[cfg_attr(
        test,
        allow(dead_code, reason = "test hosts use isolated storage roots")
    )]
    pub(crate) fn user_root() -> Result<PathBuf, String> {
        directories::ProjectDirs::from("", "", "Knot")
            .map(|dirs| dirs.data_local_dir().join("storage"))
            .ok_or_else(|| "cannot determine the user application-data directory".into())
    }

    pub(crate) fn new(root: PathBuf, executor: BackgroundExecutor) -> Self {
        Self {
            root,
            executor,
            databases: Mutex::new(BTreeMap::new()),
            work: Arc::new(Mutex::new(WorkState {
                accepting: true,
                pending: 0,
                drained: None,
            })),
        }
    }

    pub(crate) fn for_owner(&self, owner: StorageOwner) -> OwnerStorage {
        let database = self
            .databases
            .lock()
            .unwrap()
            .entry(owner.clone())
            .or_insert_with(|| {
                Arc::new(Database {
                    path: self.root.join(owner.relative_path()),
                    connection: Mutex::new(None),
                })
            })
            .clone();
        OwnerStorage {
            database,
            executor: self.executor.clone(),
            work: self.work.clone(),
            caller: None,
        }
    }

    /// Stop admission synchronously; wait and close connections on the executor.
    pub(crate) fn shutdown(self) -> Task<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        {
            let mut work = self.work.lock().unwrap();
            work.accepting = false;
            if work.pending == 0 {
                let _ = sender.send(());
            } else {
                work.drained = Some(sender);
            }
        }
        let databases = self.databases.into_inner().unwrap();
        let (closed, completion) = tokio::sync::oneshot::channel();
        self.executor
            .spawn(async move {
                let _ = receiver.await;
                for database in databases.into_values() {
                    database.connection.lock().unwrap().take();
                }
                let _ = closed.send(());
            })
            .detach();
        self.executor.spawn(async move {
            let _ = completion.await;
        })
    }
}

struct WorkState {
    accepting: bool,
    pending: usize,
    drained: Option<tokio::sync::oneshot::Sender<()>>,
}

struct AdmittedWork(Arc<Mutex<WorkState>>);
impl Drop for AdmittedWork {
    fn drop(&mut self) {
        let mut work = self.0.lock().unwrap();
        work.pending -= 1;
        if work.pending == 0
            && let Some(sender) = work.drained.take()
        {
            let _ = sender.send(());
        }
    }
}

#[derive(Clone)]
pub(crate) struct OwnerStorage {
    database: Arc<Database>,
    executor: BackgroundExecutor,
    work: Arc<Mutex<WorkState>>,
    caller: Option<Arc<AtomicBool>>,
}

impl OwnerStorage {
    pub(crate) fn with_caller(mut self, caller: Arc<AtomicBool>) -> Self {
        self.caller = Some(caller);
        self
    }

    fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
    ) -> Task<Result<T, StorageError>> {
        let admitted = {
            let mut state = self.work.lock().unwrap();
            if !state.accepting {
                return Task::ready(Err(StorageError::Cancelled));
            }
            state.pending += 1;
            AdmittedWork(self.work.clone())
        };
        let database = self.database.clone();
        let caller = self.caller.clone();
        self.executor.spawn(async move {
            let _admitted = admitted;
            let mut connection = database.connection.lock().unwrap();
            if caller
                .as_ref()
                .is_some_and(|caller| !caller.load(Ordering::Acquire))
            {
                return Err(StorageError::Cancelled);
            }
            database
                .run(&mut connection, work)
                .map_err(|error| StorageError::Unavailable {
                    database: database.path.clone(),
                    cause: error.to_string(),
                })
        })
    }

    #[cfg(test)]
    pub(crate) fn write_with_gate(
        &self,
        started: std::sync::mpsc::Sender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) -> Task<Result<(), StorageError>> {
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            transaction.execute("INSERT INTO records VALUES ('key', '42')", [])?;
            started.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(5)).unwrap();
            transaction.commit()?;
            Ok(())
        })
    }

    pub(crate) fn read(&self, key: String) -> Task<Result<Option<Value>, StorageError>> {
        self.run(move |connection| {
            let value: Option<String> = connection
                .query_row("SELECT value FROM records WHERE key = ?1", [key], |row| {
                    row.get(0)
                })
                .optional()?;
            value
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(Into::into)
        })
    }

    pub(crate) fn list(&self) -> Task<Result<Vec<String>, StorageError>> {
        self.run(|connection| {
            Ok(connection
                .prepare("SELECT key FROM records ORDER BY key COLLATE BINARY")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?)
        })
    }

    pub(crate) fn write(&self, key: String, value: Value) -> Task<Result<(), StorageError>> {
        self.run(move |connection| {
            connection.execute(
                "INSERT INTO records (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, serde_json::to_string(&value)?],
            )?;
            Ok(())
        })
    }

    pub(crate) fn delete(&self, key: String) -> Task<Result<(), StorageError>> {
        self.run(move |connection| {
            connection.execute("DELETE FROM records WHERE key = ?1", [key])?;
            Ok(())
        })
    }
}

struct Database {
    path: PathBuf,
    connection: Mutex<Option<Connection>>,
}

impl Database {
    fn run<T>(
        &self,
        connection: &mut Option<Connection>,
        work: impl FnOnce(&mut Connection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        if connection.is_none() {
            *connection = Some(self.open()?);
        }
        work(connection.as_mut().unwrap())
    }

    fn open(&self) -> anyhow::Result<Connection> {
        std::fs::create_dir_all(self.path.parent().unwrap())?;
        let mut connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let transaction = connection.transaction()?;
        let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let application: i64 =
            transaction.query_row("PRAGMA application_id", [], |row| row.get(0))?;
        let objects: Vec<(String, String)> = transaction
            .prepare(
                "SELECT name, sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name",
            )?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        if version == 0 && application == 0 && objects.is_empty() {
            transaction.execute_batch(SCHEMA)?;
            transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
            transaction.pragma_update(None, "user_version", 1)?;
        } else {
            anyhow::ensure!(
                version == 1
                    && application == APPLICATION_ID
                    && objects == [("records".into(), SCHEMA.into())],
                "unsupported storage schema (application {application}, version {version})"
            );
        }
        transaction.commit()?;
        // Ordinary rollback journaling and FULL synchronization make success a durable commit.
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        Ok(connection)
    }
}

#[cfg(test)]
pub(crate) fn test_executor() -> BackgroundExecutor {
    static EXECUTOR: std::sync::OnceLock<BackgroundExecutor> = std::sync::OnceLock::new();
    EXECUTOR.get_or_init(gpui::background_executor).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use serde_json::json;

    fn store(root: &std::path::Path, cx: &TestAppContext) -> ExtensionStorage {
        ExtensionStorage::new(root.to_owned(), cx.background_executor.clone())
    }

    fn package(name: &str) -> StorageOwner {
        StorageOwner::package(name).unwrap()
    }

    #[gpui::test]
    async fn records_are_durable_and_missing_differs_from_null(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let storage = store(root.path(), cx);
        let records = storage.for_owner(package("@sample/records"));
        assert!(!records.database.path.exists());
        assert_eq!(records.read("missing".into()).await.unwrap(), None);
        records.write("null".into(), Value::Null).await.unwrap();
        records
            .write("b".into(), json!([1, {"a": true}]))
            .await
            .unwrap();
        records.write("a".into(), json!("old")).await.unwrap();
        records.write("a".into(), json!("new")).await.unwrap();
        records.delete("absent".into()).await.unwrap();
        assert_eq!(records.list().await.unwrap(), ["a", "b", "null"]);
        assert_eq!(
            records.read("null".into()).await.unwrap(),
            Some(Value::Null)
        );
        records.delete("b".into()).await.unwrap();
        drop(records);
        drop(storage);
        let reopened = store(root.path(), cx).for_owner(package("@sample/records"));
        assert_eq!(reopened.read("a".into()).await.unwrap(), Some(json!("new")));
        assert_eq!(reopened.list().await.unwrap(), ["a", "null"]);
        reopened
            .run(|connection| {
                assert_eq!(
                    connection
                        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?,
                    "delete"
                );
                assert_eq!(
                    connection.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))?,
                    2
                );
                Ok(())
            })
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn owners_are_isolated_and_connections_are_reused(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let storage = store(root.path(), cx);
        let first = storage.for_owner(package("@one/records"));
        let same = storage.for_owner(package("@one/records"));
        assert!(Arc::ptr_eq(&first.database, &same.database));
        let others = [
            storage.for_owner(package("@two/records")),
            storage.for_owner(StorageOwner::personal_config()),
        ];
        first.write("key".into(), json!(1)).await.unwrap();
        for other in others {
            assert_eq!(other.read("key".into()).await.unwrap(), None);
            other.write("key".into(), json!(2)).await.unwrap();
        }
        assert_eq!(same.read("key".into()).await.unwrap(), Some(json!(1)));
        for invalid in [
            "../escape",
            "@one/../escape",
            "@one/..",
            "@One/name",
            "@knot/config",
            "/tmp/db",
        ] {
            assert!(StorageOwner::package(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn bundled_identities_use_the_reserved_scope() {
        assert!(StorageOwner::package("@knot/projects").is_err());
        assert_eq!(
            StorageOwner::bundled("@knot/projects")
                .unwrap()
                .relative_path(),
            PathBuf::from("packages/@knot/projects.sqlite3")
        );
        assert!(StorageOwner::bundled("@example/projects").is_err());
    }

    #[gpui::test]
    async fn failed_write_preserves_the_existing_record(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let records = store(root.path(), cx).for_owner(package("@one/records"));
        records
            .write("a".into(), json!({"old": true}))
            .await
            .unwrap();
        records.run(|connection| {
            connection.execute_batch("CREATE TEMP TRIGGER reject_write BEFORE UPDATE ON records BEGIN SELECT RAISE(ABORT, 'injected write failure'); END")?;
            Ok(())
        }).await.unwrap();
        let error = records
            .write("a".into(), json!({"new": true}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("injected write failure"));
        assert_eq!(
            records.read("a".into()).await.unwrap(),
            Some(json!({"old": true}))
        );
    }

    #[gpui::test]
    async fn concurrent_calls_serialize_on_one_connection(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let records = store(root.path(), cx).for_owner(package("@one/concurrent"));
        let tasks: Vec<_> = (0..20)
            .map(|index| records.write(format!("key-{index:02}"), json!(index)))
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(records.list().await.unwrap().len(), 20);
    }

    #[gpui::test]
    async fn unsupported_and_corrupt_databases_are_preserved(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let storage = store(root.path(), cx);
        for (name, setup) in [
            ("version", "PRAGMA user_version = 99"),
            (
                "foreign",
                "CREATE TABLE important (content TEXT); INSERT INTO important VALUES ('keep')",
            ),
            (
                "shape",
                "PRAGMA application_id = 1263423316; PRAGMA user_version = 1; CREATE TABLE records (other TEXT)",
            ),
        ] {
            let records = storage.for_owner(package(&format!("@one/{name}")));
            let path = records.database.path.clone();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(setup).unwrap();
            drop(connection);
            let before = std::fs::read(&path).unwrap();
            let error = records.list().await.unwrap_err();
            let StorageError::Unavailable { database, cause } = error else {
                panic!("unexpected cancellation")
            };
            assert_eq!(database, path);
            assert!(cause.contains("unsupported storage schema"));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let corrupt = storage.for_owner(package("@one/corrupt"));
        std::fs::write(&corrupt.database.path, b"not a database").unwrap();
        assert!(corrupt.list().await.is_err());
        assert_eq!(
            std::fs::read(&corrupt.database.path).unwrap(),
            b"not a database"
        );
        // A failing database does not disable another owner.
        storage
            .for_owner(StorageOwner::personal_config())
            .write("ok".into(), json!(true))
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn unwritable_location_fails_without_replacing_existing_data(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let obstruction = root.path().join("storage");
        std::fs::write(&obstruction, b"keep").unwrap();
        let records = store(&obstruction, cx).for_owner(StorageOwner::personal_config());
        let error = records.write("key".into(), json!(1)).await.unwrap_err();
        let StorageError::Unavailable { database, .. } = error else {
            panic!("unexpected cancellation")
        };
        assert_eq!(database, obstruction.join("personal-config.sqlite3"));
        assert_eq!(std::fs::read(obstruction).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[gpui::test]
    async fn read_only_database_rejects_mutation_without_replacement(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let storage = store(root.path(), cx);
        let records = storage.for_owner(StorageOwner::personal_config());
        records.write("key".into(), json!(1)).await.unwrap();
        let path = records.database.path.clone();
        drop(records);
        drop(storage);
        let before = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let records = store(root.path(), cx).for_owner(StorageOwner::personal_config());
        assert!(records.write("key".into(), json!(2)).await.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[gpui::test]
    async fn cancellation_before_execution_does_not_open_the_database(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let storage = store(root.path(), cx);
        let caller = Arc::new(AtomicBool::new(true));
        let records = storage
            .for_owner(StorageOwner::personal_config())
            .with_caller(caller.clone());
        let write = records.write("key".into(), json!(1));
        caller.store(false, Ordering::Release);
        assert!(matches!(write.await, Err(StorageError::Cancelled)));
        assert!(!records.database.path.exists());
        storage.shutdown().await;
    }

    #[test]
    fn shutdown_drains_admitted_work_and_closes_connections() {
        let root = tempfile::tempdir().unwrap();
        let storage = ExtensionStorage::new(root.path().to_owned(), test_executor());
        let caller = Arc::new(AtomicBool::new(true));
        let records = storage
            .for_owner(StorageOwner::personal_config())
            .with_caller(caller.clone());
        let (started, ready) = std::sync::mpsc::channel();
        let (resume, gate) = std::sync::mpsc::channel();
        let write = records.write_with_gate(started, gate);
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        // Once started, caller cancellation cannot undo this transaction.
        caller.store(false, Ordering::Release);
        let mut drain = storage.shutdown();
        assert!(matches!(
            pollster::block_on(records.write("late".into(), json!(9))),
            Err(StorageError::Cancelled)
        ));
        assert!(pollster::block_on(async {
            std::future::poll_fn(|cx| {
                std::task::Poll::Ready(
                    std::future::Future::poll(std::pin::Pin::new(&mut drain), cx).is_pending(),
                )
            })
            .await
        }));
        resume.send(()).unwrap();
        pollster::block_on(write).unwrap();
        pollster::block_on(drain);
        assert!(records.database.connection.lock().unwrap().is_none());
        let reopened = ExtensionStorage::new(root.path().to_owned(), test_executor());
        assert_eq!(
            pollster::block_on(
                reopened
                    .for_owner(StorageOwner::personal_config())
                    .read("key".into())
            )
            .unwrap(),
            Some(json!(42))
        );
        pollster::block_on(reopened.shutdown());
    }

    #[test]
    fn shutdown_closes_connections_after_the_waiter_is_dropped() {
        let root = tempfile::tempdir().unwrap();
        let storage = ExtensionStorage::new(root.path().to_owned(), test_executor());
        let records = storage.for_owner(StorageOwner::personal_config());
        pollster::block_on(records.write("key".into(), json!(1))).unwrap();
        drop(storage.shutdown());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while records.database.connection.lock().unwrap().is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "connection was not closed"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn storage_process_writer() {
        let Some(root) = std::env::var_os("KNOT_STORAGE_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let storage = ExtensionStorage::new(root.clone(), test_executor());
        let records = storage.for_owner(StorageOwner::personal_config());
        if std::env::var_os("KNOT_STORAGE_TEST_INTERRUPT").is_some() {
            pollster::block_on(records.run::<()>(move |connection| {
                let transaction = connection.transaction()?;
                transaction.execute(
                    "UPDATE records SET value = ?1 WHERE key = 'key'",
                    [serde_json::to_string(&"x".repeat(8_000_000))?],
                )?;
                std::fs::write(root.join("uncommitted-ready"), [])?;
                loop {
                    std::thread::park();
                }
            }))
            .unwrap();
        } else {
            pollster::block_on(records.write("key".into(), json!({"committed": true}))).unwrap();
            pollster::block_on(storage.shutdown());
        }
    }

    #[test]
    fn reopen_after_process_exit_and_interrupted_replacement() {
        use std::process::{Command, Stdio};
        let root = tempfile::tempdir().unwrap();
        let child = |interrupt: bool| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "app::extension_storage::tests::storage_process_writer",
                    "--nocapture",
                ])
                .env("KNOT_STORAGE_TEST_ROOT", root.path())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            if interrupt {
                command.env("KNOT_STORAGE_TEST_INTERRUPT", "1");
            }
            command.spawn().unwrap()
        };
        let output = child(false).wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "writer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut interrupted = child(true);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !root.path().join("uncommitted-ready").exists() {
            assert!(
                interrupted.try_wait().unwrap().is_none(),
                "writer exited before transaction"
            );
            if std::time::Instant::now() >= deadline {
                interrupted.kill().unwrap();
                interrupted.wait().unwrap();
                panic!("writer did not start transaction");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let journal_exists = root.path().join("personal-config.sqlite3-journal").exists();
        let spilled = std::fs::metadata(root.path().join("personal-config.sqlite3"))
            .is_ok_and(|metadata| metadata.len() > 1_000_000);
        interrupted.kill().unwrap();
        interrupted.wait().unwrap();
        assert!(
            journal_exists && spilled,
            "interruption must exercise on-disk rollback"
        );
        let storage = ExtensionStorage::new(root.path().to_owned(), test_executor());
        assert_eq!(
            pollster::block_on(
                storage
                    .for_owner(StorageOwner::personal_config())
                    .read("key".into())
            )
            .unwrap(),
            Some(json!({"committed": true}))
        );
        pollster::block_on(storage.shutdown());
    }
}
