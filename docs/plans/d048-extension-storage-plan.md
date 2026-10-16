# D048 — Simple durable extension storage

Status: 🚧 Checkpoints 1–2 implemented and tested; awaiting public API review.
Checkpoint 3 has not started.

Source: checkpoint 4 of the [extension platform and projects plan](extension-platform-and-projects-plan.md).
Design discussion: 2026-10-15. Relevant current boundaries:
[architecture](../architecture.md), [decisions](../decisions.md), and the
[extension host](../architecture/extension-host.md).

## Outcome

JavaScript extensions and personal configuration can persist JSON records across
Knot restarts through a small `storage` API. Storage is available during activation,
before any product window exists.

## Agreed design

- Each extension gets its own SQLite database, selected by Knot from its validated
  canonical package identity. Personal configuration gets a separate stable DB.
  JavaScript cannot choose another owner or a database path.
- Databases live under Knot's local application-data directory. Records are
  application-global, independent of windows, roots, and runtime lifecycle IDs.
- Use ordinary synchronous SQLite calls on the existing background executor,
  outside V8 and off the UI thread. Reuse a connection per database and serialize
  access to that connection. No dedicated storage service, worker, or custom queue.
- Extend the existing annotated host operations, error transport, and public
  JavaScript facade. JavaScript calls are async because native disk work runs
  outside its turn. Storage needs no command or window context.
- Application-owned data stays separate from extension databases. D048 does not
  create application metadata tables; document persistence and recovery are D050.

### Records

Keys are strings and values are JSON-compatible data. Publish read, write,
delete, and list operations with TypeScript declarations and documentation.

- Read distinguishes a missing key from a stored JSON null.
- Write replaces the value for a key. Delete of an absent key succeeds.
- List returns keys in deterministic order; no query language or pagination.
- Each write atomically replaces one JSON record. Keep related state in one
  record when it must change together. Invalid input or a database failure leaves
  that record unchanged.
- A successful mutation response means the transaction committed. Awaited calls
  preserve their natural order. Concurrent calls serialize at the connection;
  the last committed write wins. Read/modify/write across calls is not atomic.
- No public record versions or conditional updates. Extensions own the structure
  and any schema versions inside their JSON values.

### Lifetime and failures

Open databases as needed and initialize the small record schema. Use SQLite's
normal transaction and durability behavior; do not introduce a tuning framework
or custom backup system. Select the crate and record the concrete settings in the
first checkpoint's review.

Use the existing caller-lifetime checks before starting database work. Once a
transaction starts, finish it normally; cancellation or unload does not undo a
committed write. A caller that disappears may not receive its result. Reuse the
existing exact-once host settlement behavior.

Graceful application shutdown stops admitting storage work and waits for admitted
work to finish before releasing connections. This must not require a final JS
unload callback or block the foreground thread waiting for disk I/O.

Reload, package updates, and uninstall preserve data. D048 adds no automatic
cleanup or cleanup UI. Diagnostics identify the affected database and failure;
corrupt, incompatible, or unwritable storage fails without deleting or replacing
the database. The rest of the application can continue with that storage
unavailable. Any future physical schema change needs an explicit migration;
D048 needs schema initialization and rejection of unsupported schemas, not a
migration framework.

## Checkpoints

### 1. Native record storage ✅

Implement the per-owner database boundary, connection reuse, schema initialization,
and synchronous record operations. Choose the SQLite crate and simple
connection settings. Keep filesystem/database work on the background executor.

**Validation:** real temporary databases cover read/write/delete/list, JSON null
versus missing, failed-write preservation, owner isolation, reopen, and
unsupported/corrupt/unwritable storage without destructive replacement.

**Review gate:** inspect the native boundary, schema, dependency, and durability
settings before connecting the JS API.

✅ Implemented in `app::extension_storage`. Application-selected owners bind a
validated canonical package name or the separate personal-config identity to
`storage/packages/@scope/package.sqlite3` or `storage/personal-config.sqlite3`
under local application data. Binding is I/O-free; operations open lazily and run
on the existing gpui background executor. Each database reuses one mutex-guarded
connection. Typed `serde_json::Value` mutations admit only JSON data; the JS input
validation boundary remains checkpoint 2.

✅ Dependency/settings: `rusqlite` 0.40.2 with bundled SQLite; explicit read/write,
create and no-mutex open flags (the Rust mutex serializes access), five-second
busy timeout, rollback journal `DELETE`, and `synchronous=FULL`. Each write/delete
uses one SQLite statement with its normal implicit transaction and returns success
after commit.
Schema version 1 uses `application_id=0x4b4e4f54`, `user_version=1`, and a strict
`records(key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL CHECK(json_valid(value)))`
table. Only empty unowned databases initialize; mismatched ownership, versions,
or physical schema fail without replacement. Errors retain database path and cause.

✅ Seven real-database tests cover records and null/missing, stable ordering,
replacement/delete, reopen/settings, owner isolation and connection reuse,
concurrent calls, injected SQLite write failure with record preservation, unsupported
versions/foreign schemas/corruption, and unwritable locations/read-only files.

✅ Validation: `cargo test --lib extension_storage` passed all seven storage tests;
`cargo test --lib` passed all 626 tests on the final run. The first full run hit
an existing provider-unload assertion (`Terminated` versus `Cancelled`); that test
passed in isolation and the full rerun passed. `cargo fmt` and `git diff --check`
completed.

✅ Native-boundary inspection completed on continuation: canonical-owner path
selection, lazy connection reuse, strict schema/owner/version rejection, transaction
rollback and settings were inspected; the seven focused tests passed again. No
blocking finding remained before connecting the JS API. Application shutdown and
work-start cancellation coverage remain checkpoint 3.


### 2. Usable JavaScript API ✅

Connect storage through existing typed handlers and lifecycle ownership. Publish
the facade, declarations, and API documentation. Exercise both installed-package
identity and the personal-config identity; storage must work during activation.

**Validation:** real JS through the product host covers record operations, JSON
validation, atomic record replacement, isolated owners, structured errors, and data
surviving unload/reload. Run the extension API contract checks.

**Review gate:** approve the small public API and demonstrate persistence using an
ordinary JS fixture with no private application access.

✅ Added `storage.read/write/delete/list` to the frozen public facade,
TypeScript declarations and author guide. Read returns `{ found: false }` or
`{ found: true, value }`; values use the shared plain JSON validation/copying
boundary. Related state lives in one JSON record and is replaced with `write`.
`StorageUnavailable` retains structured database/cause diagnostics through the
existing error transport.

✅ Product admission binds validated canonical package names and the separate
personal-config owner to live lifecycles. Annotated handlers use existing caller
checks, require no command/window context, and await the native storage boundary.
Unload removes caller ownership while preserving the database and its connection.
No new transport operation table, service or worker was introduced.

✅ Four real-JS product-host tests cover activation without windows, installed
owner isolation, reload/new host/relocated package persistence, both personal-config
phases and reload, immutable JSON copying/validation, null versus missing, sorted
keys, atomic replacement/deletion and preservation after SQLite write failure,
and structured corruption failures with preserved bytes and an independent owner.
The checked-in `@example/storage-demo` runs through the product command bridge:
its public command copies records and activation counts, then repeats after reload.
The fixture uses only public `knot` exports.

✅ Validation: all four focused product-host storage tests passed;
`cargo test --lib` passed all 630 tests, including the ordinary demo's command
and reload demonstration. `sh scripts/check-extension-api.sh` passed the public
TypeScript contract and checked JavaScript examples. `cargo fmt` and
`git diff --check` completed.

✅ Simplified on 2026-10-16 at the user's request: removed public multi-record
batch operations and their native mutation abstraction. The API has four record
operations; atomic replacement of one JSON record covers current consumers.
All 630 library tests and the extension API checks passed after simplification.

⏸️ Public API review gate remains open. The fixture and automated demonstration
are ready; checkpoint 3 shutdown integration and lifecycle acceptance remain.

### 3. Shutdown and acceptance ⬜

Integrate graceful shutdown with admitted native storage work and finish lifecycle
coverage. Update the relevant architecture reference and validated decisions when
the implementation establishes them. Mark this and the parent plan as work lands.

**Validation:** deterministic tests cover cancellation before work starts, unload
while a transaction is in flight, shutdown with pending writes, and database reopen
after a separate process exits or is interrupted. Verify committed data survives
and interrupted record replacements never appear partially applied. Run the
relevant Rust and extension API checks; run `cargo fmt` once at the end of Rust work before
committing. Use logical commits for the implementation checkpoints.

**Review gate:** review the restart demonstration, failure diagnostics, and passing
automated checks. Stop for approval before marking D048 complete and archiving
this plan.

## Exclusions and deferred choices

No multi-record batches, multi-process coordination, single-instance enforcement,
public versions, conditional writes, SQL API, secrets, synchronization, automatic backups, or
uninstall cleanup. Application metadata, document recovery, and coordination
between recovery captures and extension records remain D050 decisions. This plan
does not preselect a cross-database recovery protocol.
