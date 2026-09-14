# AxiusFlow Study SDK

`axiusflow_study_sdk` is the author-facing Rust facade for native studies that execute inside the
AxiusFlow Study Runtime. A study declares durable settings, dependencies, outputs, invalidation,
and a trusted Rust calculation function. The runtime owns scheduling, bounded buffers, state
accounting, transaction/rollback, and provider demand derived from those declarations.

## Trust and packaging model

Native studies are **statically linked trusted code**. AxiusFlow does not scan a plugin directory,
load arbitrary DLLs/shared libraries, or define a public native ABI. A production build approves a
study by pinning/reviewing its crate and dependency closure, linking it into the application, and
adding its `TrustedStudyPackage` descriptor to the bounded product allowlist in
`apps/desktop/src/study_packages.rs`. The signed application build is the trust boundary.

This is not a sandbox. Approved native Rust executes in-process and can use whatever operating
system capabilities its reviewed dependency graph exposes. The Study SDK deliberately does not
hand study calculations provider/account credentials, provider-session handles, `MarketEngine`,
GPUI objects, Nucleus engine/render handles, or GPU state, which prevents accidental ownership
leakage through the supported API. Untrusted or user-installable executable code would require a
separate sandbox/process/WASM design and is outside this native package model.

The product registry is immutable and bounded to `MAXIMUM_TRUSTED_STUDY_PACKAGES`. A descriptor
contains only a stable identifier, the SDK compatibility epoch, the newest implementation revision
created by that package, and a restore function. Package restore receives only durable
dependencies/settings and must return a normal `NativeStudyRegistration`. The registry rejects a
package that changes its durable identifier, dependency graph, or settings during restore, and it
contains restore panics so one approved package cannot unwind through workspace reconstruction.
Restore callbacks are part of the reviewed static package boundary: they must be deterministic,
bounded, and free of network, disk, process, or other blocking I/O.

```rust
use axiusflow_study_sdk::{
    STUDY_SDK_COMPATIBILITY_EPOCH, TrustedStudyPackage,
};

pub const PACKAGE: TrustedStudyPackage = TrustedStudyPackage::new(
    "example.my_study",
    STUDY_SDK_COMPATIBILITY_EPOCH,
    1,
    restore,
);
# use axiusflow_study_sdk::{NativeStudyRegistration, StudyDependency, StudySdkError, StudySettingValue};
# use std::collections::BTreeMap;
# fn restore(_: u32, _: Vec<StudyDependency>, _: BTreeMap<String, StudySettingValue>) -> Result<NativeStudyRegistration, StudySdkError> { unimplemented!() }
```

Product integration then adds the reviewed package's descriptor to the static allowlist; there is
no runtime installation step.

## Compatibility and durable revisions

There are three separate compatibility concepts:

- Cargo/package semver governs **source API** compatibility for authors. `STUDY_SDK_API_VERSION`
  reports the SDK crate version compiled into the product; it is informational, not a durable
  workspace compatibility key.
- `STUDY_SDK_COMPATIBILITY_EPOCH` fences an incompatible change to the static package/restore
  contract. A package descriptor must declare the exact host epoch.
- `implementation_revision` belongs to one study identifier and is part of durable workspace
  state. Start at revision `1`. Bump it when persisted settings, dependency meaning, output
  identifiers/order, or formula semantics change incompatibly. Do not bump it for performance-only
  refactors or transient checkpoint/state-layout changes that preserve the durable contract.

Restore is exact. A package must explicitly handle every old implementation revision it still
supports and reconstruct the same durable dependencies/settings/output interface. It must return
`UnsupportedImplementationRevision` for revisions it no longer supports. The registry never
silently upgrades persisted settings or changes market demand during restore. A future durable
migration that really changes those values must be an explicit product migration of workspace
state, including downstream output references when output identifiers change.

## Execution and ownership

Study calculations receive immutable inputs, borrowed live non-bar views, validated settings,
bounded output buffers, and optional runtime-owned opaque state. The runtime catches native
calculation panics/errors at its boundary and commits state/output transactionally, so a rejected
execution cannot partially advance recursive state or output generations. State factories must
provide exact runtime byte accounting, including owned heap capacity; runtime configuration also
bounds study count, dependencies, outputs, points, and total state/output memory.

`NativeStudyState::new` is intentionally limited to `Copy` state, where creating an execution
candidate cannot preserve shared mutable aliases to the committed value. State that owns heap data
or uses shared containers must use `NativeStudyState::new_transactional` and provide an explicit
candidate-clone function. That function must make all mutable state independent of the committed
instance; a shallow `Arc<Mutex<_>>`/interior-mutable clone is not a valid transactional clone.
This remains a reviewed trusted-native contract rather than a sandbox boundary.

Market dependencies are declarations, not provider handles. `MarketEngine` remains the only owner
of canonical market demand and series state, while `market_runtime` exclusively owns provider
sessions. Requesting Quotes/Trades/Depth extends the MarketEngine-owned stream demand and exposes
borrowed canonical views during execution. Current non-bar
state can recalculate a bar-aligned study; pure non-bar output timelines are intentionally not part
of this contract until a concrete product study requires them.

For recursive studies, keep live append/current-tail work bounded. Do not convert or rescan the
entire canonical history on every live update merely to fit a formula API. Use an existing
Nucleus-owned indexed primitive when the shared formula exists; otherwise retain bounded state and
checkpoint historical repair deliberately. Hard gaps must remain explicit rather than being
interpolated silently.

## Examples

The examples compile against the SDK facade only:

- `examples/stateless.rs` — dirty-range stateless scalar output from canonical fixed-point bars.
- `examples/stateful.rs` — runtime-owned transactional state with explicit clone and byte accounting.
- `examples/multi_output.rs` — two stable outputs grouped under one study presentation.
- `examples/multi_timeframe.rs` — two market dependencies with explicit timestamp alignment.
- `examples/market_microstructure.rs` — bar-aligned use of borrowed quote/trade/depth state.

Check all examples with:

```text
cargo check -p axiusflow_study_sdk --examples --locked
```

The built-in studies in `src/lib.rs` are production examples of settings metadata, recursive
Nucleus state, hard-gap handling, rich scalar presentation, and durable revision restoration. The
SDK surface tests in `tests/native_sdk_surface.rs` additionally prove stateful, study-on-study, and
quote/trade/depth contracts without importing desktop, provider, GPUI, or Nucleus render owners.

## Qualification expectations

Before approving a package for the product allowlist, validate its revision restoration, settings
editor contract, dirty-range behavior, hard gaps, output identity, runtime byte accounting, panic /
error rollback (including restore-callback panic containment), and representative sustained load.
Workspace close/reopen and current-series rebind must reconstruct the same durable study graph
without duplicate provider demand or chart series. Missing packages are preserved as durable
workspace state and must not prevent unrelated studies from restoring.
