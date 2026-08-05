# Provider priority and terminal edge

**Date:** 2026-08-05
**Status:** accepted

## Context

Axiusflow has a partially integrated Coinbase reference path, preliminary access
conversations for several commercial providers, and substantial historical work
for cloud and raw-packet topologies. Pursuing all paths simultaneously obscures
the first shippable terminal and makes provider-neutral contracts difficult to
validate.

Rithmic has offered access through R|Protocol. The accepted package, agreement
state, Test credentials, installed schema semantics, entitlements, and provider
certification are not yet verified. IQFeed access is preliminary. R|API+ is a
separate native product and is not interchangeable with R|Protocol.

## Decision

Rithmic Test through R|Protocol WSS/Protobuf is the first commercial provider
target. The initial Rithmic milestone is a read-only headless core followed by a
lightweight chart and DOM terminal. Orders, account state, positions, OMS, risk,
and execution are excluded.

Coinbase is frozen after its current desktop one-minute history-to-live path is
stabilized. It remains a BTC-USD and ETH-USD regression/reference feed. It will
not receive additional symbols, timeframes, depth, analytics, or cloud routing.

IQFeed and CQG are deferred. R|API+ is deferred independently. Any reordering
requires a new decision based on verified access, licensing, semantics, product
fit, and implementation cost.

Provider topology is direct-to-device. The user's terminal connects to the
provider under the user's authorization. Axiusflow cloud does not receive,
persist, relay, or redistribute that market data in this phase.

The terminal edge is transparent speed and correctness: direct-provider data,
bounded local processing, visible latency boundaries, queue pressure, gaps,
recovery state, provenance, and deterministic replay. Provider timestamp age is
reported as clock-relative age and is not represented as network latency.

No claim of superiority over ATAS, Sierra Chart, TOS, or another product is made
without comparable measurements on named hardware and workloads.

## Consequences

- Provider-neutral trade, quote, depth, bar, provenance, recovery, and
  publication contracts precede Rithmic UI work.
- Rithmic wire decoding remains inside a kit-optional adapter boundary.
- Proprietary kits, guides, schemas, generated bindings, credentials, and
  licensed captures remain outside tracked source unless license review permits
  otherwise.
- Ordinary workspace builds pass without the kit through an explicit unavailable
  backend; authorized kit-enabled validation uses a private lane later.
- The public runtime surface is sealed and read-only. It cannot send raw
  protobuf or order/execution templates.
- A deterministic headless gate and lightweight diagnostics gate precede main
  Rithmic UI integration.
- Coinbase correctness regressions remain release blockers, but Coinbase feature
  expansion does not compete with the Rithmic path.
- Cloud market-data, packet acceleration, and execution work cannot silently
  return to active scope through implementation convenience.
- Descoped artifacts are cleaned in Stage 0 of `docs/platform_creation_plan.md`
  (delete retired AF_XDP/DPDK from `main`; quarantine deferred cloud MD build
  surfaces). Deferred items must not remain as workspace keep-alives.

## Revisit conditions

Revisit this decision only when one of the following is documented:

- Rithmic access or licensing makes the selected product path unavailable;
- deterministic or authorized Test evidence shows required continuity or depth
  semantics cannot be implemented safely;
- another provider supplies verified access and materially better coverage for
  the bounded read-only terminal;
- a separately funded and licensed cloud-market-data or execution product is
  approved.

Until then, `docs/platform_creation_plan.md` is the authoritative execution
order.
