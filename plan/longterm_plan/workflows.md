# Trading workflows

Workflows let a trader describe what Aeris should do when a market reaches a price or a time of day,
build it as a board of blocks, and have the trading owner run exactly that, on every account Aeris
routes: practice accounts (Hyperliquid and other public feeds), Rithmic demo and live accounts, and
every later venue on PF11 (see [trading_platform.md](trading_platform.md)).

Item IDs are stable: **W** checkpoints. A checkpoint is **Complete** only when it works through the
real runtime and desktop path and its probe evidence is recorded, not when a unit test alone passes.
Status as of 2026-11-02.

## Principles

- **The user accepts every trade rule.** A workflow runs only after the user saved one exact
  revision and armed it. Any change to the blocks saves a new revision, which starts paused.
- **The engine decides, deterministically.** Only `trading_runtime::workflow` evaluates an armed
  workflow, on the owner's verified quotes and injected clock. Its orders go through the same
  admission, risk and venue paths as a manual order. Nothing remote evaluates or fires a workflow.
- **AI only writes blocks.** A model may translate plain language into blocks from the closed set
  (W3). It never decides a trade, places an order or produces the readback the user accepts.
- **Venue-neutral.** Blocks name an account and an instrument, never a provider. A workflow that
  works on a practice account works unchanged on a broker account.
- **Bounded.** At most 64 workflows, 32 blocks each, 50 firings a session, and a bounded firing
  history per workflow.

## Checkpoints

| ID | Checkpoint | Status |
| --- | --- | --- |
| W1 | Engine: graph model, store, owner commands and evaluation | **Complete** |
| W2 | Desktop Workflows page: board, inspector, list | **In progress** |
| W3 | AI authoring | Open |
| W4 | Position management and order-flow blocks | Open |
| W5 | Simulation before arming | Open; needs T6 |
| W6 | Stock and options venues | Open; needs a PF11 broker |

### W1 — Engine

- [x] Workflow definition: one trigger (price crosses a level, time of day in Chicago or UTC), a
      start block and an acyclic graph of If blocks (position, time window, price above or below)
      and action blocks (enter with an optional bracket, close the position, cancel working
      orders, lock the account), with repeat limits (once, or per session with a cooldown).
- [x] Validation: every block reachable from the trigger, no cycles, no link after a lock, every
      value within its bound; the layout validated against the definition.
- [x] Store migration V31: `workflows` (definition, layout, state, counters) and
      `workflow_firings`; the layout is stored apart from the definition, so moving a block keeps
      the revision, state and firings.
- [x] Owner commands: register (a new revision starts paused), set layout, arm, pause, delete.
- [x] Evaluation on owner quotes and timers; readiness (account, terms, verified quote, risk lock,
      broker connection) checked when armed and at firing; each firing committed before its first
      action, so a stop leaves it `Interrupted` and a restart never repeats it.
- [x] Firing results record each If's answer and each action's result, in order.
- [x] Working set carries every workflow view; armed price workflows join the owner's quote
      interest.
- [x] Deterministic tests in `crates/trading_runtime/src/tests/workflows.rs`.

### W2 — Desktop Workflows page

- [x] Header page switcher entry beside Terminal and Screener; the old bottom-panel Workflows tab
      and builder dialog are removed.
- [x] Draft model (`apps/desktop/src/desktop/workflow_board.rs`): blocks, links, positions and
      typed text; `build` produces exactly the definition and layout the owner validates, naming
      the first problem otherwise; unit tests.
- [x] Board: free block placement, drag from an output dot to link, drop on empty space to unlink,
      pan, zoom around the pointer, Yes and No outputs on If blocks, live problem marking.
- [x] Inspector for the workflow (name, account, symbol, repeat), the trigger and each block;
      palette of checks and actions; list of workflows with state and firing counts.
- [x] Save, arm, pause and two-step delete; moved blocks save their layout alone.
- [x] Gates green: fmt, workspace clippy `-D warnings`, workspace tests and the naming check.
- [ ] The page exercised by hand on the running app with a Hyperliquid practice account.
- [x] `workflow_probe` updated to the graph API, with a check that moving blocks keeps the armed
      revision; run live on the Hyperliquid BTC practice feed (below).
- [x] Probe rerun from a clean, committed source tree (`2b5ee13f`).

### W3 — AI authoring

- [ ] Plain language to blocks through the user's own key or a local model; the prompt carries no
      exchange prices or account data.
- [ ] Validation errors go back to the model at most three times; the result opens as an unsaved
      draft on the board for the user to review and save.
- [ ] The readback the user accepts comes from the blocks, never from the model.

### W4 — Position management and order flow

- [ ] Time in trade, profit in ticks or currency, pullback from the best price.
- [ ] Move stop and partial exit actions.
- [ ] Delta and footprint conditions over the trade tape (PF5).
- [ ] Buying power and margin checks once PF2 projects them.

### W5 — Simulation

- [ ] Run a workflow over history and recorded sessions (T6) and show each firing it would have
      made before the user arms it.

### W6 — More venues

- [ ] A stock and options broker on PF11, so the same workflows reach stock and options accounts.

## Acceptance

A fired workflow places exactly the orders its accepted revision describes, at most once per
trigger across restarts, through the normal risk checks; any change to its blocks pauses it until
the user saves and arms the new revision.

## Progress log

| Date | Checkpoint | Change | Evidence |
| --- | --- | --- | --- |
| 2026-11-02 | W1 | Graph engine, V31 store with separate layout, owner commands and tests | `cargo test -p aeris_trading_runtime` |
| 2026-11-02 | W2 | Workflows page with board, inspector and list replaces the bottom-panel tab | fmt, workspace clippy, 3,153 workspace tests, naming check |
| 2026-11-02 | W2 | `workflow_probe` on the graph API, live Hyperliquid BTC practice | All 10 behavior checks passed (breakout fired on a live cross and filled at the owner ask with an active bracket; layout move kept revision 1 armed; timed close flattened 2 ms after the minute; restart did not refire; delete); `source_clean` failed on the uncommitted tree |
| 2026-11-02 | W2 | `workflow_probe` rerun on `2b5ee13f` | All 11 checks passed with a clean source tree; entry filled at the owner ask with an active bracket, timed close 1 ms after the minute |
