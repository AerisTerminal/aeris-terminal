# S2-22 decision: SQLite WAL for desktop transactional metadata

**Date:** 2026-08-04  
**Status:** accepted for the scoped desktop catalog  
**Evidence command:** `tools/run_embedded_store_spike.sh`

## Decision

Use exact-pinned `rusqlite 0.40.1` with its bundled SQLite `3.53.2` in WAL
mode for desktop workspace state, cache manifests, schema/migration state,
provider recovery checkpoints, and local OMS/order-intent metadata.

This decision does not store tick, depth, bar, or derived-history payloads as
database rows. Those remain separately versioned, immutable, checksummed
segments under `S2-23`. It also does not authorize SQLite work on the GPUI
thread.

`redb 4.1.0` remains a measured pure-Rust comparison, not the selected store.
Reconsider it only if a concrete SQLite limitation survives schema, query, and
correctness review and reproduces under the same workload.

## Evidence

The checked-in lane runs five fresh databases per candidate in an optimized
build. Each run performs 64 individually durable workspace updates, one
4,096-entry cache-manifest transaction, and 128 individually durable paper OMS
intent transactions. It then closes, reopens, and verifies all state.
The runner builds the release binary itself and records the Git revision plus a
content hash of every tracked and untracked, non-ignored workspace file, so a
dirty evidence run cannot be mislabeled as its parent commit.
Databases are created beneath the report directory, and the report records that
canonical benchmark root and its filesystem type; it never uses the system
temporary directory for durability or latency evidence.

The 2026-08-04 release-profile run used Linux 7.0.0-28-generic on an Intel
Core i7-13700K and the host's ext-family workspace filesystem. It recorded:

| Boundary | SQLite WAL | redb |
|---|---:|---:|
| Workspace workload p50 | 17.329 ms | 15.682 ms |
| Catalog workload p50 | 3.345 ms | 3.851 ms |
| Durable order commit p50 | 267.2 µs | 245.5 µs |
| Durable order commit p99 | 332.4 µs | 335.7 µs |
| Reopen p50 | 0.164 ms | 0.257 ms |
| Database size p50 | 684,032 bytes | 987,136 bytes |

These numbers qualify only the named host/filesystem/build/workload. They are
selection evidence, not product latency claims. The JSON artifact is generated
at `.cache/evidence/stage_2_embedded_store_spike_Linux.json` and deliberately
remains outside version control.

Both candidates passed:

- version-1 to version-2 migration;
- committed reopen and explicit rollback invisibility;
- abrupt-process-exit recovery with committed state present and uncommitted
  state absent;
- deterministic storage-capacity failure without a false successful commit.

The corruption probe truncates a populated database to one third of its durable
length. SQLite returned a controlled integrity error. `redb 4.1.0` panicked in
its page manager; the evidence harness caught the panic and treated it as
detection, but a library panic on hostile/corrupt local state is unacceptable
for the production terminal boundary and materially favors SQLite.

The abrupt-exit probe is not a power-cut/torn-write test, and deterministic
capacity limits are not a physically full filesystem. Power-loss, torn-write,
real disk-full, backup/restore, and destructive migration matrices remain
required before live-order use.

## Correctness and schema rationale

The selected workload is relational metadata, not a generic key/value cache.
SQLite provides one transaction across workspace revisions, manifests, recovery
checkpoints, and order-intent state; strict tables, constraints, indexed queries,
explicit migrations, integrity checks, and mature inspection/recovery tooling
are directly useful. Implementing those properties over redb would add custom
secondary indexes, constraint code, migration conventions, and diagnostic
tooling that must all be proven separately.

SQLite runs with `journal_mode=WAL`, `synchronous=FULL`, foreign keys enabled,
and bounded busy timeouts. A successful performance result never permits
weakening those settings for OMS state.

## Dependency, packaging, and maintenance review

- `rusqlite 0.40.1` is MIT licensed. Its exact dependency is
  `libsqlite3-sys 0.38.1`; the `bundled` feature compiles a known SQLite source
  instead of silently using an older system library.
- The bundle reports SQLite `3.53.2`, newer than the `3.51.3` fix for the
  WAL-reset corruption defect. Runtime startup must continue asserting the
  reviewed minimum/fixed version and fail closed on drift.
- SQLite is public domain. The wrapper contains the C/FFI boundary; first-party
  crates retain `unsafe_code = "forbid"` and must expose a narrow safe storage
  interface rather than SQLite handles.
- `redb 4.1.0` is MIT OR Apache-2.0, requires Rust 1.89, and is pure Rust with a
  stable documented file format. Those are positive properties but do not
  outweigh the relational fit and observed corrupt-file panic.
- The bundled SQLite release build passed on the current Linux host. An
  attempted Windows GNU cross-check stopped earlier in the existing dependency
  graph because the host lacks `x86_64-w64-mingw32-gcc`; no Windows packaging
  pass is claimed. Windows and macOS builds plus crash/corruption runs remain
  required CI/release evidence before shipping the store on those platforms.

Upgrades require an exact version/checksum diff, SQLite release/security review,
WAL regression review, migration compatibility run, corruption/disk-full lane,
and the same five-run workload comparison. A dependency update cannot silently
change the bundled SQLite version or durability configuration.

## Security boundary

SQLite and redb do not by themselves satisfy Axiusflow's encryption policy.
Database files live in a per-user private application directory with restrictive
permissions. Provider credentials never enter the database and remain in the OS
credential vault. Sensitive workspace, account-binding, and order payloads must
be encrypted and authenticated at the application-record boundary with a
versioned key from the OS vault before live use; plaintext searchable columns
are limited to the minimum non-secret routing/index metadata. Logs, crash
reports, and diagnostics must redact both values and database pages.

SQLCipher is not adopted by this decision. It would add a separate native crypto
and packaging boundary and requires its own threat model, dependency review, and
cross-platform evidence if application-record encryption proves insufficient.

## Next implementation boundary

`S2-23` creates a dedicated desktop-local storage crate with:

- a narrow transactional catalog interface backed by this SQLite profile;
- atomic manifest publication only after an immutable segment is synced and
  checksummed;
- entitlement/account-scoped keys, invalidation, retention, quarantine, and
  secure-deletion behavior;
- no history payload table and no database access on the UI thread.

## Primary references

- [SQLite WAL documentation and WAL-reset fix](https://sqlite.org/wal.html)
- [SQLite 3.51.3 release notes](https://sqlite.org/releaselog/3_51_3.html)
- [`rusqlite` repository and bundled-version notes](https://github.com/rusqlite/rusqlite)
- [`redb` repository, design status, and license](https://github.com/cberner/redb)
