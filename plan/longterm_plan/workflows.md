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
- **No AI.** The user builds every workflow from the closed set of blocks. Indicators are engine
  calculations on the owner's own market data, never a model's output.
- **Venue-neutral.** Blocks name an account and an instrument, never a provider. A workflow that
  works on a practice account works unchanged on a broker account.
- **Bounded.** At most 64 workflows, 32 blocks each, four triggers each, four checks per If, 50
  firings a session, and a bounded firing history per workflow.

## Checkpoints

| ID | Checkpoint | Status |
| --- | --- | --- |
| W1 | Engine: graph model, store, owner commands and evaluation | **Complete** |
| W2 | Desktop Workflows page: board, inspector, list | **In progress** |
| W3 | Several triggers, joined checks and firing notices | **In progress** |
| W4 | Position management and order-flow blocks | Open |
| W5 | Simulation before arming | Open |
| W6 | Stock and options venues | Open; needs a PF11 broker |
| W7 | Indicator triggers and checks (EMA, SMA) | Open |

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
- [x] Blocks drawn as folder-tab cards (category tab, accent tab, title, inset value panel) on a
      dotted board; the block palette sits under the workflow list; opening a workflow fits
      every block in view.
- [x] Four example workflows open as unsaved drafts with prices left to set: breakout buy,
      morning dip buy, flat before the close, exit and lock.
- [ ] The page exercised by hand on the running app with a Hyperliquid practice account.
- [x] Symbol search on the page, so a workflow can name any symbol the catalog offers, not only
      one already open in a chart. The page owns its own catalog-search consumer; choosing a
      market installs it so the trading owner registers its terms before the workflow saves.
- [x] A market whose owner quote interest lapses and returns in the same provider generation
      (a paused and re-armed workflow, a changed ticket) applies its quotes again: the owner
      drops the applied quote cursor with the quote book entry.
- [x] `workflow_probe` updated to the graph API, with a check that moving blocks keeps the armed
      revision; run live on the Hyperliquid BTC practice feed (below).
- [x] Probe rerun from a clean, committed source tree (`2b5ee13f`).

### W3 — Several triggers, joined checks and firing notices

AI authoring, the former W3, is dropped: workflows are built only from blocks.

- [x] Up to four triggers per workflow; any of them fires it, and triggers due at the same moment
      fire it once. New trigger: every N minutes, counted from midnight UTC.
- [x] An If holds up to four checks joined by All or Any. New checks: price range (bid at or above
      the low and ask at or below the high) and day of week in Chicago or UTC.
- [x] V31 stores the trigger list and joined conditions in place; V31 has not been released, so
      no new migration is needed.
- [x] Board and inspector: several trigger sections, numbered checks with All/Any, range inputs,
      weekday toggles, multi-line card summaries; strings in all ten languages.
- [x] The workflow list and the inspector fold away from toolbar buttons to give the board the
      width.
- [x] Each new firing shows a toast on the workspace with the workflow's name and what it did,
      announced once through the desktop trading store.
- [x] `workflow_probe` timed close uses a time-of-day trigger and an every-minute trigger due at
      the same minute, joined with a weekday check, and checks it fires once.

### W4 — Position management and order flow

- [ ] Time in trade, profit in ticks or currency, pullback from the best price.
- [ ] Position opened and position closed triggers.
- [ ] Move stop and partial exit actions; break-even and trailing-stop rules on entries.
- [ ] Quantity by fixed size or by risk; repeat without a limit, with re-arming.
- [ ] Trade prints into the trading owner through a bounded queue; delta and volume conditions
      over the trade tape (PF5).
- [ ] Buying power and margin checks once PF2 projects them.

### W5 — Simulation

- [ ] Run a workflow over the chart's candle history and show each firing it would have made
      before the user arms it, through the same evaluation path the owner uses.
- [ ] Recorded sessions (T6) once they exist.

### W6 — More venues

- [ ] A stock and options broker on PF11, so the same workflows reach stock and options accounts.

### W7 — Indicators

- [ ] Completed candles into the trading owner for armed workflows that need them.
- [ ] Fixed-point EMA and SMA in the engine, tested against the chart's indicator values.
- [ ] Moving-average cross triggers and price-against-average checks.

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
| 2026-11-02 | W2 | Card-style blocks, dotted board, fit-to-view, palette in the sidebar, four examples | Examples opened and inspected on the running app; fmt, workspace clippy, naming check, workspace tests |
| 2026-11-02 | W3 | Up to four triggers, every-N-minutes trigger, All/Any checks, price range and weekday checks, foldable list and inspector, firing toasts; AI authoring dropped | fmt, workspace clippy, naming check, 3,162 workspace tests; panels folded and restored on the running app; `workflow_probe` live on Hyperliquid BTC practice: all 10 behavior checks passed (time and every-minute triggers at the same minute fired once, 8 ms after the minute); `source_clean` failed on the uncommitted tree |
| 2026-11-02 | W3 | `workflow_probe` rerun on `7c001ed2` | All 11 checks passed with a clean source tree; entry filled at the owner ask with an active bracket; time and every-minute triggers fired once, 7 ms after the minute |
| 2026-11-02 | W2 | Symbol search on the Workflows page; stale quote cursor after a lapsed interest fixed | ETH searched, saved and armed on the running app; regression test `a_workflow_rearmed_after_its_quote_interest_lapsed_still_fills`; `workflow_probe` now pauses and re-arms the ETH entry before it is due: without the fix the entry was cancelled with no error (the desktop symptom), with it all 11 behavior checks passed and the entry filled 71 ms after the minute; `source_clean` failed on the uncommitted tree |
