# S2-23 decision: rights-aware encrypted local history segments

**Date:** 2026-08-04
**Status:** implemented
**Evidence command:** `tools/run_local_history_store.sh`

## Decision

Use `crates/desktop_storage` as the blocking worker-side boundary for retained
desktop history. `rusqlite 0.40.1` with bundled SQLite 3.53.2 stores only the
bounded relational manifest. Immutable bars, ticks, depth, and eligible derived
payloads remain encrypted files outside the database.

The crate does not choose a market-history codec, connect to a provider, or run
on the GPUI thread. It accepts bounded opaque codec bytes plus the complete
provider/account/entitlement and invalidation identity. `S2-24` owns provider
history scheduling and `S2-25` owns startup/hydration integration.

## Publication and recovery contract

One publication performs this sequence on a single application-owned
filesystem:

1. validate scope, revisions, range, payload size, retention rights, and catalog
   capacity;
2. derive keyed catalog tokens and the immutable segment identity from the
   caller-supplied catalog key;
3. encrypt the payload with a unique random XChaCha20-Poly1305 nonce and bind the
   full identity as AEAD associated data;
4. write and sync a private staging file, atomically hard-link the immutable
   final name without replacement, make it read-only, and sync the directory;
5. commit the checksums, byte count, retention, recovery action, key verifier,
   and keyed scope/dimension tokens to SQLite.

The manifest therefore never makes a partial or unsynced segment reachable. A
crash between final-file creation and catalog commit leaves an orphan that
startup removes. Startup also clears staging files, removes unreferenced owned
segments/quarantine files, and marks a manifest whose file disappeared as
quarantined.

Reads verify the stored-file SHA-256 checksum, AEAD tag and associated identity,
payload length, and plaintext SHA-256 checksum before returning bytes. A corrupt,
oversized, missing, or unauthentic file is unavailable and only that catalog
identity is quarantined. Its recorded fallback remains either provider refetch
or live-only; stale bytes are never returned.
An exact quarantined identity can be replaced only when both the recorded and
new recovery policy are provider refetch and the same verified segment key is
present. The new synced file replaces the manifest in one SQLite statement;
live-only or wrong-key attempts remain rejected.

## Rights, privacy, and bounds

- `MemoryOnly` rights write no file or manifest. Expired retained rights remove
  the file and row before returning an explicit unavailable result. Republishing
  an existing identity under memory-only or already-expired rights first removes
  its older retained file and manifest, so policy tightening cannot leave a
  readable cache copy.
- Provider, account, entitlement, instrument, resolution, and full segment
  identities are HMAC-SHA-256 catalog tokens under a non-persisted catalog key.
  Raw account and entitlement identifiers do not enter SQLite or file names.
- Segment payloads use caller-supplied, versioned 256-bit keys. Keys are neither
  cloneable nor persisted and their byte containers zeroize on drop. The
  platform credential vault remains their owner. Secure account deletion
  rejects a key that is still referenced by another account, enforcing
  account-scoped key lifecycles instead of stranding unrelated history.
- The manifest is capped by a caller-selected bound no greater than 100,000
  entries. Each plaintext segment is capped at 64 MiB. A full catalog fails
  explicitly instead of evicting an arbitrary entitlement scope, and reopening
  with a bound below the existing row count is rejected.
- One cross-process exclusive lock owns each history root for the lifetime of a
  store instance. A competing process fails explicitly, preventing startup
  orphan recovery from racing publication or deletion.
- Entitlement and account changes can invalidate their exact scopes. Schema,
  session-calendar, adjustment, and correction changes delete only rows whose
  recorded revision differs from the current revision.

## Deletion boundary

`secure_delete_account` refuses to delete anything until caller-provided
OS-vault evidence confirms revocation of every segment key referenced by the
scope. It then removes active and quarantined files, deletes catalog rows under
SQLite `secure_delete=ON`, truncates the WAL checkpoint, and reports counts.
Expiry applies before quarantine status during reads and expiry sweeps include
both active and quarantined rows, so corruption never extends provider rights.

Key destruction provides the reliable confidentiality boundary. File unlink and
SQLite overwrite cannot promise physical erasure from SSD remapping,
copy-on-write filesystems, snapshots, backups, or forensic media recovery, so
every deletion report keeps `physical_remanence_possible=true`. Platform backup
exclusion and uninstall/key-lifecycle integration remain release work.

## Cryptography and dependency review

- Exact-pinned `chacha20poly1305 0.11.0` is the current RustCrypto pure-Rust
  implementation, licensed MIT OR Apache-2.0 with Rust 1.85 minimum. The crate
  enables only allocation and key-zeroization support.
- XChaCha20-Poly1305 uses a 192-bit nonce so independently generated random
  nonces remain practical without a durable counter. RustCrypto documents that
  XChaCha has deployed interoperability but no final authoritative standard;
  this file format is private and versioned rather than claimed as RFC 8439 wire
  compatibility.
- The implementation has no first-party unsafe code. `zeroize 1.9.0`,
  `hmac 0.12.1`, `sha2 0.10.9`, and `getrandom 0.4.3` provide key clearing,
  keyed indexes, checksums, and OS randomness.
- Upgrades require exact version/checksum review, upstream security/advisory and
  audit review, known-answer/decryption compatibility, corrupt-file behavior,
  migration/recovery evidence, and the complete lifecycle suite.

Primary upstream references:

- [RustCrypto ChaCha20Poly1305 documentation](https://docs.rs/chacha20poly1305/0.11.0/chacha20poly1305/)
- [RustCrypto AEADs repository](https://github.com/RustCrypto/AEADs/tree/master/chacha20poly1305)
- [SQLite WAL documentation](https://sqlite.org/wal.html)

## Evidence and limitations

The ten public-boundary lifecycle tests prove:

- encrypted, read-only publication followed by reopen and exact-key reads;
- catalog-key and segment-key mismatch rejection without destructive quarantine;
- absence of raw account, entitlement, and payload bytes from catalog files;
- provider/account/entitlement isolation;
- targeted corruption quarantine while another segment remains readable;
- exact-key provider-refetch replacement, quarantine persistence across restart,
  and deletion of quarantined files;
- expiry removal through both quarantined reads and background sweeps;
- source/schema/calendar/adjustment/correction plus entitlement/account
  invalidation;
- memory-only rights, expiry, explicit fallback, bounded-catalog refusal, and
  key-revocation-gated account deletion;
- rejection of key revocation while another account still references that key;
- startup cleanup of staging/orphan files and quarantine of missing manifest
  files.

This is filesystem and catalog correctness evidence on the current Linux host,
not a power-cut, torn-write, SSD secure-erase, provider-rights, Windows, macOS,
or production performance claim. An attempted
`cargo check --package axiusflow_desktop_storage --target x86_64-pc-windows-gnu`
stopped in bundled `libsqlite3-sys` before crate compilation because this host
lacks `x86_64-w64-mingw32-gcc`; no Windows pass is claimed. Those remain
explicit release gates. The safe implementation currently fails closed on
non-Unix targets because it cannot yet flush directory metadata after links,
renames, and deletions; Windows support requires a proven native
directory-flush boundary.
