# Cross-cutting history and code-quality analysis

Repository: `C:\Users\devraj\Downloads\Softwares\aeris-terminal` (branch `main`, 4 commits ahead of `origin/main` at analysis time).
Method: read-only. Git history mining (`git log --numstat`, `-S`, `-G`, `-L`, hunk-header function attribution with a Rust diff driver supplied through a temporary attributes file), and Python heuristic parsers that strip comments and string literals, brace-match functions and structs, and mask `#[cfg(test)]` modules. No cargo commands were run. Helper scripts are in `C:\Users\devraj\AppData\Local\Temp\aeris_audit\scripts\` (`git_stats.py`, `fn_fix.py`, `topics.py`, `census.py`, `dup.py`, `parallel.py`, `impls.py`, `grepnt.py`).

Scope of the code census: `apps/`, `crates/`, `tools/` (305 `.rs` files). `third_party/`, `target/`, `.cache/` and `provider_kit/` are excluded. "Non-test" means outside `tests/` directories, `*_tests.rs`/`tests.rs` files and `#[cfg(test)] mod` blocks.

## Summary

- The history is 1,188 non-merge commits over 75 days (2026-07-31 to 2026-10-13, 68 active days), with a median of 15 minutes between consecutive commits. `fix` is the largest commit type: 402 commits (34%), almost twice the 208 `feat` commits. Refactor is 61 (5%) and test is 33 (3%).
- The fix:feat ratio peaked at 2.75 in September and is 1.57 for 1 to 13 October. The last 500 commits have a ratio of 1.77 and the last 200 have 1.66. Ten days produced 15 or more fix commits each, and 17 days produced 10 or more; the maximum was 28 on 2026-09-12.
- Repeat fixes are measurable. **164 of 402 fix commits (41%) modify a non-trivial function that another fix commit modified in the previous six commits** (284 such pairs, after excluding constructors, `render`, and the pinned-revision test in `naming_check`). In several functions nearly every change is a fix: `history.rs::accept_history_completion` (7 of 8 commits), `accept` in the removed Level 1 broker module (8 of 10), `realtime.rs::accept_indexed_trades` (6 of 7), and `view.rs::chart_legend_row` (15 of 21).
- Fixes are often feature-sized or bundled. The median fix touches 119 lines and 4 files. 65 fix commits exceed 500 changed lines and 30 exceed 1,000. 93 fix commits span three or more top-level directories, and 118 fix subjects bundle unrelated outcomes ("X, Y and Z"). Only 238 of 402 fix commits (59%) add or change a `#[test]`.
- Generation fencing is everywhere and has no shared type. The code has 4,693 occurrences of `generation` across 117 files and 228 distinct generation-related identifiers. 136 non-test fields and parameters store a raw `u64`/`u32`/`usize` generation, and there are 208 non-test generation comparisons. The shared `aeris_application::MarketGeneration<T>` (412 lines) is used in only 3 desktop files, and its `require_resnapshot` method has no callers.
- State is concentrated in a few very large types: `WorkspaceSurface` (96 fields, about 262 methods), `TerminalApp` (63 fields, about 260 methods across 13 files), `AerisChartView` (49 fields, about 227 methods), and market_runtime `Coordinator` (29 fields, about 300 methods across 9 files and about 11,000 lines of impl). These four types sit in the files with the most fix commits.
- Function length is not the problem. The workspace enables clippy `pedantic` (`too_many_lines` = 100), and function lengths stop sharply at about 110 lines: only 5 non-test functions exceed 150 lines, and 4 of those are in the Rithmic adapter with an explicit `allow`. The size problem is at file and type level: 12 non-test files exceed 3,000 lines.
- Errors are mostly strings. There are 1,196 non-test occurrences of `Result<…, String>`. Several recent fixes exist only because a string error could not express a distinction such as superseded versus failed, or which demand scope failed.

## Commit history statistics (tables)

### Commit type distribution

Type is taken from the `type(scope):` prefix. "(none)" means a commit with no conventional prefix (178 of them are from August, before the convention was adopted). 5 reverts exist in total: 3 `revert(...)` commits and 2 git-generated `Revert "..."` commits counted under "(none)".

| Type | All 1,188 | Last 500 (2026-09-13 to 10-13) | Last 200 (2026-10-02 to 10-13) |
|---|---:|---:|---:|
| fix | 402 (33.8%) | 204 (40.8%) | 98 (49.0%) |
| feat | 208 (17.5%) | 115 (23.0%) | 59 (29.5%) |
| (none) | 188 | 3 | 0 |
| docs | 119 | 36 | 18 |
| chore | 80 | 69 | 5 |
| refactor | 61 (5.1%) | 41 (8.2%) | 7 (3.5%) |
| test | 33 (2.8%) | 12 | 4 |
| migration | 29 | 0 | 0 |
| style | 13 | 9 | 4 |
| ci | 12 | 3 | 3 |
| perf + performance | 13 | 4 | 1 |
| build | 6 | 2 | 1 |
| revert | 3 | 2 | 0 |
| **fix:feat** | **1.93** | **1.77** | **1.66** |

### Fix:feat by month and week

| Month | fix | feat | refactor | fix:feat |
|---|---:|---:|---:|---:|
| 2026-08 | 74 | 60 | 10 | 1.23 (178 untyped commits in this month, so the ratio is understated) |
| 2026-09 | 223 | 81 | 15 | **2.75** |
| 2026-10 (1 to 13) | 105 | 67 | 36 | 1.57 |

| Week starting | commits | fix | feat | refactor |
|---|---:|---:|---:|---:|
| 08-17 | 59 | 20 | 17 | 6 |
| 08-24 | 44 | 25 | 8 | 1 |
| 08-31 | 202 | **85** | 22 | 5 |
| 09-07 | 116 | **75** | 16 | 7 |
| 09-14 | 35 | 19 | 5 | 1 |
| 09-21 | 114 | 22 | 24 | 2 |
| 09-28 | 182 | **74** | 33 | 33 |
| 10-05 | 125 | 56 | 43 | 5 |
| 10-12 (2 days) | 23 | 15 | 8 | 0 |

Days with the most fix commits: 09-12 (28), 09-13 (24), 09-28 (21), 10-02 (20), 10-09 (19). The busiest day overall was 10-02 with 76 commits.

### Fix scopes (top)

desktop 96, ui 50, chart 30 + charts 16, market 30, trading 16, auth 10, (empty) 10, account 9, order-book 9, rithmic 8, release 8, streaming 8, platform 7, engine 7, ci 7, watchlist 6, lifecycle 6, workspace 5.

### Fix shape

| Metric | Value |
|---|---|
| Lines changed per fix (median / p75 / p90) | 119 / 307 / 743 |
| Fix commits > 500 lines / > 1,000 lines | 65 / 30 |
| Largest "fixes" | `ef4b9789c` fix(market): bound provider stalls (24 files, +1802/-373); `86492bea4` fix(market_runtime): drive Level 1 broker candles from trades (11 files, +1392/-197); `7bf8f1c02` fix(market): close out FX/CFD broker market-data defects (+690/-206, 9 numbered defects) |
| Files per fix (median / p90) | 4 / 11 |
| Fix commits spanning 3 or more top-level dirs | 93 |
| Fix subjects bundling several outcomes (", " or " and ") | 118 |
| Fix commits that add or change `#[test]` lines | 238 of 402 (59%) |
| Fix commits re-touching a function fixed 6 or fewer commits earlier | **164 of 402 (41%)**, 284 pairs |
| Reverts | 5 (2,850 lines removed in total) |
| Duplicate commit subjects | 5 pairs, including `c07a1588c`/`4992cefda` "reconnect catalog demand across generation advances" and `3860761fc`/`2643f6449` "delegate style reset to Nucleus" |

### Reverts

| Commit | Date | Subject | Size | What it undid |
|---|---|---|---|---|
| `c1f739061` | 09-03 | Revert "fix(chart): keep legend rows on one clipped line…" | -82 | `7db18c4cd` |
| `97c3f3133` | 09-05 | Revert "fix(chart): stabilize live OHLC legend layout" | -31 | `b7b1596d9` |
| `3e690ff8e` | 09-06 | revert(market): remove unsuccessful history loading changes | 15 files, +241/-1150 | 7 fix commits made between 06:04 and 07:43 the same morning (`0a64dd290`, `146fef874`, `9b820977`, `44daa3c8`, `bec68d368`, `93fd90e5a`, `bcb82ce2e`, `7fabf699a`); Coinbase was replaced by Hyperliquid 12 hours later (`f0c857793`) |
| `8a903dc77` | 09-14 | revert(desktop): remove live workspace tab values | -158 | `47704f2c` |
| `077eca9f9` | 09-14 | revert(desktop): remove device-sized vector renderer | +210/-1250 | renderer feature |

### Charts pin churn (noise in the fix statistics)

- 139 commits change the `aeris_charts_*` (formerly Nucleus) revision. Each bump edits 5 identical `rev =` lines in `Cargo.toml`, `Cargo.lock`, and an `expected_revision` string in `tools/naming_check/src/main.rs` (135 commits change it). This is why `naming_check/src/main.rs` (66 fix commits), `Cargo.lock` (45) and `Cargo.toml` (38) rank among the fix hotspots.
- 80 commits touch only `Cargo.toml`, `Cargo.lock` and `naming_check`. Every chart defect takes two repositories, a push, and a pin bump.

## Fix hotspots and recurring themes

### Top 40 files by fix commits

| # | File | fix | all | fix share |
|---:|---|---:|---:|---:|
| 1 | apps/desktop/src/desktop.rs | 72 | 183 | 39% |
| 2 | crates/ui/chart_integration/src/view.rs | 67 | 170 | 39% |
| 3 | tools/naming_check/src/main.rs | 66 | 252 | 26% (mostly pin strings) |
| 4 | apps/desktop/src/main.rs | 65 | 214 | 30% |
| 5 | apps/desktop/src/desktop/workspace_surface.rs | 52 | 123 | 42% |
| 6 | Cargo.lock | 45 | 283 | pins |
| 7 | apps/desktop/src/components/chart_context_menus.rs | 43 | 89 | **48%** |
| 8 | apps/desktop/src/desktop/tests.rs | 40 | 91 | 44% |
| 9 | apps/desktop/src/desktop/workspace_tabs.rs | 40 | 88 | 45% |
| 10 | Cargo.toml | 38 | 212 | pins |
| 11 | crates/ui/chart_integration/src/view/tests.rs | 38 | 88 | 43% |
| 12 | apps/desktop/src/engine_market_worker.rs | 37 | 87 | 43% |
| 13 | apps/desktop/src/components/terminal_view.rs | 36 | 90 | 40% |
| 14 | apps/desktop/src/components/terminal_chrome.rs | 30 | 77 | 39% |
| 15 | crates/ui/design_system/src/lib.rs | 24 | 67 | 36% |
| 16 | plan/plan.md | 23 | 106 | |
| 17 | crates/market_runtime/src/market_service/coordinator.rs | 20 | 42 | 48% |
| 18 | the removed Level 1 broker module (market_service) | 20 | 38 | **53%** |
| 19 | apps/desktop/src/components/chart_surface.rs | 19 | 47 | 40% |
| 20 | crates/market_runtime/src/market_service/realtime.rs | 18 | 34 | **53%** |
| 21 | apps/desktop/src/account.rs | 18 | 32 | 56% |
| 22 | crates/market_runtime/src/market_service/mod.rs | 17 | 47 | 36% |
| 23 | crates/ui/terminal_ui/src/order_book_view.rs | 17 | 32 | 53% |
| 24 | apps/desktop/src/native_ui/menu.rs | 16 | 30 | 53% |
| 25 | apps/desktop/src/assets.rs | 16 | 59 | 27% |
| 26 | apps/desktop/src/desktop/local_state.rs | 15 | 42 | 36% |
| 27 | apps/desktop/src/market_worker.rs | 15 | 66 | 23% |
| 28 | crates/ui/design_system/platform.css | 15 | 39 | 38% |
| 29 | crates/platform_runtime/src/lifecycle.rs | 15 | 32 | 47% |
| 30 | apps/engine/src/market_service/tests.rs (retired) | 15 | 25 | 60% |
| 31 | apps/engine/src/market_service.rs (retired) | 15 | 52 | 29% |
| 32 | ARCHITECTURE.md | 14 | 117 | |
| 33 | apps/desktop/src/components/workspace_layout.rs | 13 | 60 | 22% |
| 34 | CSS.md | 13 | 23 | 57% |
| 35 | apps/desktop/src/components/drawing_toolbar.rs | 13 | 29 | 45% |
| 36 | apps/desktop/src/engine_market_worker/publications.rs | 12 | 23 | 52% |
| 37 | the removed Level 1 broker module tests (market_service) | 12 | 18 | 67% |
| 38 | crates/contracts/src/messages.rs | 11 | 35 | 31% |
| 39 | crates/market_runtime/src/market_service/tests.rs | 11 | 27 | 41% |
| 40 | crates/trading_runtime/src/tests.rs | 11 | 34 | 32% |

### Function-level fix hotspots

Attribution uses the function named in the hunk header, so a hunk is credited to the preceding `fn`. Constructor counts therefore mostly reflect new struct fields.

| Function | fix / all | Interpretation |
|---|---:|---|
| chart_integration `view.rs::empty_with_theme`, `with_replay_and_theme` | 22/62, 22/64 | Two constructors of `AerisChartView` (49 fields) and the struct definition after them. Every new chart property means editing both constructors (exact-duplicate block `view.rs:1183-1200` matches `1288-1305`). |
| `view.rs::chart_legend_row` | **15/21** | Legend layout: 13 legend fixes and 2 reverts |
| desktop `main.rs::new`, `desktop.rs::current_plan_limits` (precedes the `WorkspaceSurface` struct) | 16/69, 17/47 | God-struct field churn |
| `terminal_view.rs::render`, `workspace_tab` | 15/39, 10/15 | Tab and workspace chrome |
| `native_ui/menu.rs::render`, `terminal_chrome.rs::header_controls` | 12/21, 11/32 | Menu and button interaction states |
| `workspace_surface.rs::apply_publication` | 9/21 | Market publication into the surface |
| `accept` (the removed Level 1 broker module) | **8/10** | Streaming event intake |
| `history.rs::accept_history_completion` | **7/8** | History completion semantics |
| `view.rs::apply_pending_data`, `install_replay_recovery` | 7/23, 6/21 | Chart data bridge recovery |
| `realtime.rs::accept_indexed_trades`, `publish_rithmic_live` | **6/7**, 6/10 | Live-tail and forming-bar seam |
| `history.rs::schedule_history_retry`, `enqueue_history_request` | 6/11, 6/11 | History retry |
| `coordinator.rs::run_coordinator` | 6/14 | Main runtime loop |
| `engine_market_worker/publications.rs::apply_pushed_event` | 6/11 | Desktop bridge |

### Recurring themes in the top-15 files (clustered fix subjects)

Counts are fix-commit subjects matching each theme across the whole history.

| Theme | fix commits | Representative subjects (chronological) |
|---|---:|---|
| Recovery, reconnect, resume, stall | 38 | "survive engine restarts without stranding charts", "harden provider recovery and switching", "preserve market recovery…", "recover market connections", "restore live demand on workspace resume", "bound provider stalls and preserve recovery under load", "recover conflicting live tails", "recover stalled live series", "retire superseded recoveries quietly" |
| Persistence and restore | 45 | "persist chart state and retain sessions", "harden chart preference persistence", "await chart preferences on shutdown", "fence durable shutdown", "recover interrupted saves", "restored area charts keep their translucent fill", "keep restored charts auto-scaled after timeframe changes" |
| Order book and DOM | 33 | "honest DOM loading states", "stabilize DOM identity", "restore live multi-level presentation", "use semantic side colors", then "use solid status fills", then "restore subtle status fills", then "quiet bid and ask fills further" (same day, 09-28) |
| Menus, popups, dialogs | 34 | "enlarge the instrument and indicator search menus", then "span … across the window", then "keep … as a slightly wider card" (3 commits on 08-22); "restore fixed-size menus"; "keep chart context menus compact on large screens"; "close popups on outside press"; "drop the dialog dimming and keep popups inside the window" |
| Symbol, interval, timeframe switch | 14 | see chain A below |
| Legend | 13 + 2 reverts | see chain D |
| Stale, superseded, generation, fence, retire | 18 | "fenced generations", "fence durable shutdown", "fence refresh rotation across shutdown", "rebase durable provider generations", "retire startup catalog fences", "keep order flow off the superseded chart" |
| History loading (lazy, backfill, edge) | 19 | see chain C |
| Forming candle, live tail | 8 | see chain B |
| Footprint and order flow | 7 | see chain E |
| Icons and marks (sizes, DPR, glyph centering) | about 74 subjects match | "rasterize exchange and series marks at display DPR", "…at display size", "draw restored … marks as authored", "restore colored series … via RenderOnce" (4 commits on 08-24) |
| Typography | 6 | 09-12: "use static DM Sans regular font", then "unify platform typography", then "restore DM Sans", then "restore original HK Grotesk faces", then "restore HK Grotesk weight contract" (5 fixes in one day) |
| Watchlist | 9 | 09-13: 7 watchlist and side-panel fixes in one day ("preserve symbol add intent", "restore live data and panel layout", "isolate panel layout and selection", "restore interaction and split layout", "reserve independent panel region", "dock watchlist beside order book", "show active chart selection") |
| Auth and sign-in (now disabled) | 24 | 09-05: 12 auth fixes in one day; the feature is now disabled per AGENTS.md |

## Repeat-fix chains

Each chain lists the topic, the commits in order, what each step added, and the root cause.

### A. Chart symbol and interval switch: the chart entity is replaced and state is carried over field by field

| Commit | Date | Step |
|---|---|---|
| `472d87137` | 08-30 | stream every bar and carry the open candle across switches |
| `677ae1fd8` | 08-31 | initialize switches and calendar bars live |
| `5828e8213` | 08-31 | resume the open candle on switches |
| `3cfa308a0` | 08-31 | refresh dormant timeframes before publication |
| `05539b7f6` | 08-31 | retain axis precision across market switches |
| `872b1a19` / `23ca70fb4` | 09-07 | "Harden market data switching", "harden provider recovery and switching" |
| `78158e13a` | 09-23 | preserve drawing time across timeframe switches |
| `c08f3e27c` | 09-23 | submit quick timeframe changes once |
| `5d07263cb` | 10-10 | keep keyboard focus when a series switch replaces the chart |
| `daffe4fcd` | 10-10 | keep restored charts auto-scaled after timeframe changes |
| `79a65bc7f` | 10-10 | only demand chart streams the provider publishes (recovery asked the removed FX/CFD broker for trade prints and showed "Chart unavailable") |
| `0a230a8a0` | 10-11 | keep drawings per instrument |
| `771a8d732` | 10-12 | open unoffered intervals at the nearest offered one; retire superseded recoveries quietly (adds `MarketRecoveryFailure::{Superseded, Failed(String)}`) |
| `f504ab868` | 10-12 | keep order flow off the superseded chart during interval and symbol switches (+206 test lines) |

What was layered: `WorkspaceSurface::install_snapshot_chart` (`apps/desktop/src/desktop/workspace_surface.rs:2983`) builds a new `AerisChartView` and then imperatively re-applies about 15 properties: chrome, drawing-dialog close, drawings, restored or retained state, indicators, order-line extension, time zone, market session, trade-print capability, price-alert lines, viewport, revision observers, the switch state, the trade tape, and studies. Its line history since 10-05 has 9 commits, 4 of them fixes. The in-flight switch is spread across `rithmic_switch: RithmicSwitchState {Idle, Pending, Swapping, Initializing}` (a data-less enum) plus four parallel `Option` fields: `rithmic_pending_interval`, `rithmic_pending_product`, `rithmic_pending_sequence`, and `rithmic_previous_selection` (`apps/desktop/src/desktop.rs:875-883`). These fields have "rithmic" names but are used for every provider since `f0c85779`/`67acd6b5`.

Root cause: there is no explicit chart-subscription lifecycle type. Its states would be Requested, Resolving, Installing, Live, Superseded, and Failed, with the payload carried inside each variant. There is also no single "chart presentation state" value that a replacement chart receives. Each property that is forgotten during a swap becomes a separate fix.

### B. Live tail, forming candle, and history/live seam

| Commit | Date | Step |
|---|---|---|
| `3214f054e` | 08-30 | keep Coinbase streaming across quiet buckets |
| `c172224bf` | 08-30 | stop a covering repair from reading as a stale snapshot |
| `472d87137` / `5828e8213` / `d4e55f01b` | 08-30/31 | carry, resume, and initialize the live candle |
| `f935462a` | 08-31 | reconcile Coinbase candles after live rollovers |
| (provider replaced) | 09-06 | Coinbase replaced by Hyperliquid |
| `13c697edd` | 09-09 | align history depth (touches `publish_rithmic_live`) |
| `86492bea4` | 10-02 | drive Level 1 broker candles from trades (+1392 lines, labeled fix) |
| `b006b38f3` | 10-02 | recover conflicting live tails |
| `a1f5a2919` | 10-02 | reconcile covered live tails (same day, 10 lines) |
| `4f885153e` | 10-02 | recover stalled live series: "series recovery discarded the result of its history request, so a failed or unanswered reload left the series in AwaitingHistory forever". Adds a bounded retry ticket and a **30 s liveness watchdog** |
| `dfe6c2877` | 10-12 | keep Level 1 broker charts live when older history runs out and cover the forming candle |

What was layered: `LiveHistoryState {AwaitingHistory, Ready, Reseeding}` (`crates/market_runtime/src/market_service/mod.rs:729`) exists, but its transitions are driven from many call sites. One transition (history failure while `AwaitingHistory`) was missing, so a series could stay there indefinitely. The fix added a timer-based watchdog on top instead of making the state machine total. Per-provider handoff types (`TradeLiveHandoff` with 24 fields, `CandleHandoffSeed`, `TradeHandoffSeed`) and provider-named paths inside the shared `realtime.rs` (`rithmic_connecting`, `rithmic_online`, `rithmic_recovering`, `publish_rithmic_live`, `flush_hyperliquid_demand`) mean the seam is implemented twice: once for trade-built series and once for provider-candle series.

Root cause: history/live seam rules are not expressed as one total state machine with exhaustive transition tests. Rules were discovered provider by provider (Coinbase, then Hyperliquid, Rithmic, and the removed Level 1 broker) during live use.

### C. On-demand history loading and completion semantics

| Commit | Date | Step |
|---|---|---|
| `ede297ebe` | 08-31 | retry retained history initialization |
| `29cf0e2b0` | 09-06 03:38 | bound viewport history recovery |
| `2d59ee16e` | 09-06 04:23 | retain recovery under history pressure |
| `0a64dd290` … `7fabf699a` | 09-06 06:04 to 07:43 | 7 fixes (provisional Coinbase history, shared demand, readiness, union viewport demand, beyond-edge demand, complete lazy snapshots, cross-workspace sync, cached loading) |
| `3e690ff8e` | 09-06 07:45 | **revert of all 7** (-1150 lines) |
| `7071595b6` | 10-01 | exhaust unavailable older Level 1 broker history |
| `70cfb3a01` | 10-08 | load Level 1 broker weekly candles with out-of-range closes (one bad row failed the whole page) |
| `9e01ce99e` | 10-12 | keep the delivered range when a candle snapshot stops short (the Level 1 broker's stream never sends SNAPSHOT_END; history retried the same cut-off snapshot forever) |
| `dfe6c2877` | 10-12 | older-history page failure no longer moves a healthy live chart into recovery ("Demand errors carry a scope") |

Function evidence: `history.rs::accept_history_completion` has 7 fix commits out of 8 total; `schedule_history_retry` and `enqueue_history_request` have 6 of 11 each.

Root cause: the completion contract for a history request is implicit per provider. That contract covers exhausted, truncated, empty, partial, failed, and superseded outcomes, and which demand scope failed. Failures are `String`s, so scope and kind were added late. Provider snapshot semantics were not captured as recorded fixtures before the code depended on them, contrary to the AGENTS.md market-data rule.

### D. Chart legend layout: 3 attempts and 2 reverts

| Commit | Date | Step |
|---|---|---|
| `4a3bba924` → `ed4820d84` → `80752bd2e` | 08-28 to 08-30 | hide untruthful values, wrap legends in pane, put load in symbol legend |
| `7db18c4cd` | 09-03 | keep legend rows on one clipped line, reverted the same day by `c1f739061` |
| `b7b1596d9` | 09-05 | stabilize live OHLC legend layout, reverted by `97c3f3133` |
| `23b68a93d`, `a2f7d228b` | 09-12 | legend id overflow, move study settings to legend |
| `afdbab511` | 10-11 | keep legend values on one line so they stop jumping (third attempt at the 09-03 goal) |
| `45099f0b9` then `fc785d8ce` | 10-11 | "color only the close in the symbol legend", then "keep the symbol legend OHLC neutral" (reversal in the same day) |
| `97ac79568` | 10-13 | legend asset title one size larger, regular weight |

Root cause: legend layout is computed in Terminal (`chart_integration/src/view.rs`, `chart_legend_row` 15 of 21 commits are fixes) without a written layout specification or snapshot tests. Each visual tweak is decided commit by commit.

### E. Footprint and order-flow history (each step bumps the Aeris Charts pin)

| Commit | Date | Step |
|---|---|---|
| `ee0d91492` | 09-27 | make footprint studies opt-in |
| `c947d2095` | 09-28 | decouple delta studies from footprint |
| `696e44abf` | 10-04 | footprint shows only tape-covered bars; "a gap, rewrite, or session change still installs a covering image of the current window" |
| `8beb45a39` | 10-05 | keep footprint history across settings, chart type, gaps, and reconnects; "only a same-session window rewrite installs a covering image" |
| `e944204a1` | 10-05 | color footprint volume by bar direction |
| `bd25c41a9` | 10-05 | "a same-session tape reset or rewrite … wip[ed] every older bar"; now replaces only the covered span (`replace_order_flow_window`) |
| `f504ab868` | 10-12 | tape projected into the superseded chart during switches |

Root cause: the contract between the runtime tape (an 8-minute sliding window), the chart's accumulated footprint, and the provider generation and session was redefined three times in two days (10-04 and 10-05). Each redefinition required an Aeris Charts change plus a pin bump. The tape's identity (session, generation, chart instance) is not part of one typed key.

### F. Provider catalog and startup generation fences

| Commit | Date | Step |
|---|---|---|
| `37425450b` | 08-25 | restore the Rithmic autoload by honoring the catalog-ready contract |
| `21fe1cf3e` | 08-25 | advance the Rithmic session on newer catalog installs |
| `eb4833017` | 09-04 | resolve catalog demand on session retry |
| `c07a1588c` + `4992cefda` | 09-04 | "reconnect catalog demand across generation advances" (committed twice) |
| `4c4fa05e0` | 09-06 | recover initial Rithmic startup |
| `ae242941f` | 09-14 | rebase durable provider generations |
| `b88110d78` | 09-27 | retain order book depth demand during startup resolution |
| `6fc015970` | 10-02 | retire startup catalog fences |

What was layered: a sentinel constant `STARTUP_CATALOG_COMMAND_GENERATION` (39 references). The desktop also checks `message.contains(RITHMIC_CATALOG_READY_MESSAGE)` (`apps/desktop/src/desktop.rs:3242`), which means control flow depends on matching message text.

Root cause: the generation values are raw integers with ad-hoc sentinel values and a separate "floor" (`ProviderSessionSlot::generation_floor`). Startup has no typed "no generation yet" state.

### G. Same-day UI interaction-state churn (2026-10-09)

`64c150b1d`, then `a0d707141`, `bea3191e3`, `b1bfbb199`, `fb404b38a`, `1107dcb93`, `6af2116de`, and `6b2014862`: eight fix commits in one day that all touch `native_ui/button.rs::paint_states`, `button_appearance`, `selected`, and `native_ui/menu.rs::accepts_input` (press fill, destructive rows, disabled rows, toggles, header toggles).

Root cause: interaction states are special-cased per control variant in code instead of being one data-driven table of (variant, state) to token. Similar runs: 08-22 (menu width, 3 commits), 08-24 (icon rasterization, 5 commits), 09-12 (typography, 5 commits), 09-28 (order-book fills, 4 commits).

### H. Level 1 broker recovery scoping (fix of a fix within hours)

`5e868c13c` "scope Level 1 broker recovery and publish level one books" (15 files), then the same day `03dbd7132` "keep Level 1 broker recovery scoped and typed" (9 files, +210/-103), then `4f885153e`, then `dfe6c2877` "Demand errors carry a scope". The second commit re-typed what the first one had just introduced as a string.

## Code smell census (tables)

### Size

| Metric | Value |
|---|---|
| `.rs` files in scope | 305 |
| Non-blank lines | 239,486 (167,600 non-test, 71,886 test) |
| Non-test functions | 6,793 |
| Non-test functions > 80 / > 150 / > 300 physical lines | 179 / **5** / 0 |
| Length distribution near the lint limit | 97 functions at 85-99 lines, 30 at 100-104, 14 at 105-109, 3 at 110-114, then almost none. `clippy::pedantic` `too_many_lines` (100) is enforced and functions are shaped to fit under it |
| `#[allow(clippy::too_many_lines)]` | 10 (all listed below) |
| Non-test files > 3,000 lines | 12 |

Functions over 150 lines (the entire list), plus the next band:

| Lines | Location | fn |
|---:|---|---|
| 265 | crates/adapters/rithmic_protocol/src/provider_session/canonical_market.rs:594 | convert (allow too_many_lines) |
| 186 | crates/market_runtime/src/rithmic_realtime.rs:1209 | run_demand (allow) |
| 175 | the removed FX/CFD broker trading capture example:339 | scenario (example, allow) |
| 161 | crates/adapters/rithmic_protocol/src/provider_session/session_worker.rs:358 | handle_session_message (allow) |
| 158 | crates/market_runtime/src/rithmic_realtime.rs:297 | run_catalog_session (allow) |
| 149 | crates/adapters/rithmic_protocol/src/protocol.rs:1229 | every_outbound_request (allow) |
| 128 | crates/adapters/rithmic_protocol/src/market.rs:217 | decode_depth_by_order_snapshot (allow) |
| 120 | crates/adapters/hyperliquid_market/src/candles.rs:122 | decode_candle_page (allow) |
| 112 | crates/platform_runtime/src/bin/aeris_launcher.rs:98 | run |
| 112 | crates/market_runtime/src/market_service/realtime.rs:931 | accept_provider_candle |
| 111 | apps/desktop/src/desktop/local_state.rs:328 | sanitize_workspace |
| 109 | crates/market_runtime/src/study.rs:3912 / 2945 | reconcile_market_leases / execute_ready_range_with_live |
| 109 | apps/desktop/src/account.rs:867 | apply_account_response |
| 108 | the removed Level 1 broker module:700 | run_catalog |
| 108 | crates/market_runtime/src/market_service/realtime.rs:2275 | accept_indexed_trades |
| 107 | crates/market_runtime/src/market_service/history.rs:1126 | history_completed |
| 107 | apps/desktop/src/desktop/workspace_tabs.rs:2994 | on_key_down |
| 106 | crates/market_runtime/src/hyperliquid_realtime.rs:402 | handle_catalog_select |
| 103 | apps/desktop/src/desktop/workspace_surface.rs:91 | workspace_surface_from_initialization (allow) |
| 103 | crates/ui/chart_integration/src/view.rs:2696 | rebuild |
| 102 | crates/market_runtime/src/market_service/coordinator.rs:54 | run_coordinator |
| 102 | crates/market_runtime/src/market_service/history.rs:881 | enqueue_history_request |
| 102 | apps/desktop/src/engine_market_worker/selection_commands.rs:157 | handle_startup_catalog_event |
| 100 | apps/desktop/src/desktop/workspace_surface.rs:3334 | apply_market_worker_message |

Largest non-test files: `workspace_surface.rs` 6,330; `desktop.rs` 5,733; `trading_runtime/src/lib.rs` 5,605; `coordinator.rs` 5,183 (includes inline tests); `realtime.rs` 5,179 (includes inline tests); `study.rs` 4,249; `platform_runtime/lifecycle.rs` 4,233; `rithmic_protocol/provider_session.rs` 3,962; `trading_runtime/store.rs` 3,689; `study_sdk/lib.rs` 3,605; `workspace_tabs.rs` 3,365; `market_worker.rs` 3,311; `chart_integration/view.rs` 3,261; `order_book_view.rs` 3,117.

### God types (fields and methods)

| Type | Fields | Methods (impl blocks) | Impl lines | Files |
|---|---:|---:|---:|---:|
| `WorkspaceSurface` (apps/desktop/src/desktop.rs:802) | **96** (37 `Option`, 8 `pending_*`) | 262 | 5,581 | 4 |
| `TerminalApp` (desktop.rs:4955, `allow(struct_excessive_bools)`) | **63** (10 `bool`, 17 `Option`) | 260 | 5,684 | 13 |
| `AerisChartView` (chart_integration/src/view.rs:1055) | **49** (15 `Option`, 3 `pending_*`) | 227 | 3,632 | 7 |
| market_runtime `Coordinator` (coordinator.rs:308) | 29 | about 300 | about 11,000 | 9 |
| `WorkspaceSurfaceInitialization` | 30 | | | |
| `TradingSnapshot` (trading_runtime) | 29 | | | |
| Level 1 broker `Worker` | 26 | 82 (with the FX/CFD broker `Worker`) | | |
| `StoredState` (trading store) | 26 | | | |
| `WorkspaceChartAppearanceState` (contracts, `allow(struct_excessive_bools)`) | 25 | | | |
| `TradeLiveHandoff` (market_service/mod.rs:759) | 24 (8 `Option`, `dirty`) | | | |
| `HeaderState` (desktop.rs:3249) | 24 | | | |

`WorkspaceSurface` alone combines the chart entity, order book, time and sales, the context panel and its credential dialog, side panels, menus and flyout bounds, symbol, indicator, timeframe, and time-zone inputs, the provider/product/interval switch, viewport restore, persistence dirtiness, price alerts, trading PnL, order quantity, studies, and diagnostics. It also holds 7 free-form status strings (`chart_state_message`, `symbol_message`, `series_message`, `replay_label`, `worker_label`, `bridge_label`, `subscription_id`).

### State held as flags and shadow fields

Clippy `struct_excessive_bools` pushes this state into `Option<…>` fields, so bool counts alone understate it.

| Pattern (non-test field declarations) | Count |
|---|---:|
| `pending_*` fields | 89 |
| `last_*` / `latest_*` / `previous_*` / `prev_*` shadow fields | 156 / 28 / 38 / 6 (261 across 60 files) |
| `force_*`, `skip_*`, `suppress_*`, `awaiting_*` | 7 / 2 / 1 / 2 |
| `Option<bool>` tri-states | 18 (10 in contracts) |
| Raw-integer generation fields and parameters (`u64`/`u32`/`usize`) | 136 (market_runtime 71, account_runtime 35, desktop 20) |
| `AtomicU64` generation counters | 8 sites (the two removed broker modules, runtime `ws_generation`, mod.rs, desktop account, workspace persistence) |

Most flag-like fields per file: desktop.rs 17 shadow fields + 10 `pending_*`; chart_integration view.rs 16 + 3; workspace_surface.rs 13; `feed_diagnostics.rs` 10; `hyperliquid_realtime.rs` 9 + 24 `pending_` references; `market_service/mod.rs` 9.

Example of state held in flags: `ProviderSessionSlot` (`crates/market_runtime/src/market_service/mod.rs:591`) holds `accepted: Option<ProviderDemand>`, `pending: Option<ProviderDemand>`, `stop_pending: Option<ProviderGeneration>`, `engaged: bool`, `demand_dirty: bool`, `authorization: Option<bool>`, `suspended: bool`, `generation_floor: u64`, and `catalog_degraded: Option<ProviderGeneration>`. That is nine independent fields encoding one provider session lifecycle.

### Lifecycle vocabulary per file

| File | lines | generation | epoch | stale | supersed/retire | recover | `pending_` |
|---|---:|---:|---:|---:|---:|---:|---:|
| market_runtime/market_service/coordinator.rs | 5,183 | 422 | 0 | 23 | 6 | 50 | 3 |
| market_runtime/market_service/realtime.rs | 5,179 | 413 | 0 | 2 | 9 | 83 | 36 |
| market_runtime/rithmic_realtime.rs | 2,071 | 327 | 0 | 2 | 19 | 21 | 39 |
| apps/desktop/market_worker.rs | 3,311 | 200 | 0 | 16 | 35 | 44 | 3 |
| market_runtime/market_service/history.rs | 2,379 | 185 | 0 | 1 | 2 | 19 | 0 |
| apps/desktop/engine_market_worker.rs | 2,874 | 137 | 2 | 3 | 6 | 61 | 42 |
| market_runtime/market_service (removed Level 1 broker) | 2,692 | 103 | 59 | 0 | 8 | 22 | 9 |
| market_runtime/hyperliquid_realtime.rs | 2,036 | 89 | 7 | 1 | 10 | 4 | 24 |
| apps/desktop/desktop/workspace_surface.rs | 6,330 | 73 | 2 | 7 | 8 | 28 | 55 |
| chart_integration/bridge.rs | 676 | 67 | 0 | 8 | 1 | **102** | 18 |
| market_runtime/market_service (removed FX/CFD broker) | 2,320 | 62 | 51 | 2 | 10 | 16 | 0 |

The repository has 228 distinct generation identifiers. The most common are `session_generation` 451, `provider_generation` 416, `ProviderGeneration` 215, `selection_generation` 166, `publication_generation` 164, `search_generation` 157, `install_generation` 153, `SessionGeneration` 113, `command_generation` 106, `request_generation` 63, `GenerationId` 60, `display_generation` 36, and `layout_generation` 36. Newtypes exist (`GenerationId`, `ProviderGeneration`, `SessionGeneration`, `MarketGeneration<T>`, `MarketPublicationGeneration`), but most comparisons are on raw `u64`. Every provider defines its own event enum with a leading raw `u64` generation in each variant (`RithmicRealtimeEvent`, `HyperliquidRealtimeEvent` and the two removed brokers' `RealtimeEvent`), and each provider has its own `Recovering(u64, …)` payload shape.

Chart and series lifecycle has at least 8 separate representations: `SeriesLoadState` (contracts), `ChartState {Loading, Ready, Stale, Recovering, AwaitingData, Error}` (desktop market_worker), `RithmicSwitchState` (desktop), `MarketRecoveryFailure` (desktop, added 10-12), `LiveHistoryState` (market_runtime), `ProviderSessionSlot` flags (market_runtime), `ChartDataBridge::pending_resnapshot` (chart_integration), and the per-provider `Recovering` events.

### Discarded results and defaults

Non-test counts per crate:

| Crate | `let _ =` | `.ok();` | `unwrap_or_default()` | `.expect(` | `.clone()` | `#[allow(` |
|---|---:|---:|---:|---:|---:|---:|
| apps/desktop | 101 | 3 | 28 | 12 | 830 | 3 |
| crates/market_runtime | 79 | 0 | 15 (+6 in examples) | 7 | 382 | 4 |
| crates/platform_runtime | 57 | 2 | 0 | 7 | 33 | 8 |
| crates/ui/chart_integration | 24 | 1 | 21 | 3 | 35 | 1 |
| crates/adapters/rithmic_protocol | 16 | 0 | 0 | 50 | 68 | 9 |
| crates/trading_runtime | 14 | 2 | 6 | 144 | 308 | 0 |
| crates/account_runtime | 13 | 3 | 6 | 0 | 27 | 2 |
| removed FX/CFD broker adapter | 10 | 0 | 11 | 6 | 12 | 1 |
| others | 8 | 2 | 11 | 25 | | 12 |
| **Total** | **322** | 13 | 98 | 254 | | 40 |

`.unwrap()` outside tests: 0, because clippy denies it.

`let _ =` by callee: `send` 55, `join` 20, `try_send` 18, `shutdown` 17, `update` 15, `stop` 14, `kill` 11, and `wait` 11. Most of these are acceptable close or teardown calls. The ones that hide real failures:

- `crates/market_runtime/src/market_service/realtime.rs:2663, 2706, 3784, 3853`: `let _ = self.send_rithmic_demand();` discards a `Result<(), String>` from re-issuing provider demand during recovery. A failure leaves no state and no diagnostic.
- `crates/market_runtime/src/market_service/history.rs:818, 1111, 1164, 1183, 1254`: `let _ = self.cancel_live_history_reseed(…)` (5 sites).
- `crates/market_runtime/src/hyperliquid_realtime.rs:327, 388, 454, 471`: `let _ = publish_catalog(…)`.
- `crates/market_runtime/src/market_service/coordinator.rs:465` and `realtime.rs:2736, 3918`: `let _ = self.engine.end_provider_session(provider, generation)`.
- `crates/ui/chart_integration/src/engine_bridge.rs:268, 278, 342`: `let _ = engine.set_series_data(…)`, which drops chart data installation errors.
- `crates/ui/chart_integration/src/view.rs:1274`: `ChartDataBridge::try_new(…).ok()` in the chart constructor turns bridge creation failure into a missing bridge.
- `apps/desktop/src/engine_market_worker/runtime.rs:34, 426`: `let _ = market.remove_consumer(…)`.
- `crates/account_runtime/src/account_service/mod.rs`: `let _ = self.retire_unusable_vault_material(vault)` (5 sites).

`unwrap_or_default` on wire data: The removed FX/CFD broker adapter had `frame.payload.as_deref().unwrap_or_default()` at 10 sites. These are low risk, because `decode_typed` validates first and an empty payload then fails `require_nested_fields`. `crates/trading_runtime/src/store.rs:2292-2296` writes CSV export columns from JSON `as_str().unwrap_or_default()`, so missing ids export as empty strings. `crates/ui/chart_integration/src/view/drawings.rs:79-89` uses `serde_json::from_value(…).unwrap_or_default()` for persisted drawing fields, so corrupt data silently resets.

Errors as strings: 1,196 non-test `Result<…, String>` occurrences (trading_runtime 323, market_runtime 324, desktop 197, platform_runtime 97). Control flow on message text: `apps/desktop/src/desktop.rs:3242` (`message.contains(RITHMIC_CATALOG_READY_MESSAGE)`), `crates/account_runtime/src/account_service/mod.rs:1840` (`detail.contains("timed out") || detail.contains("cancelled")`), and `crates/market_runtime/src/hyperliquid_realtime.rs:1209` (`reason.starts_with("hyperliquid channel is unsupported:")`).

`#[allow(...)]` inventory (non-test, 40 total): `clippy::too_many_lines` 10, `unsafe_code` 8, all in `crates/platform_runtime` Windows FFI (`session_shutdown.rs:361`, `power_notifications.rs:135, 252`, `network_notifications.rs:191, 505`, `io_cancellation.rs:10`, `display_timing.rs:407`, `atomic_file.rs:17`). This is possible because `crates/platform_runtime/Cargo.toml:61-62` overrides the workspace `forbid` with `unsafe_code = "deny"`, which conflicts with the AGENTS.md invariant "Workspace-wide `unsafe_code` remains forbidden", `clippy::too_many_arguments` 7, `non_snake_case` 7 (Hyperliquid JSON wire structs in `hyperliquid_market/src/meta.rs`, `context.rs` and `candles.rs`, where `#[serde(rename)]` would avoid the allow), `clippy::struct_excessive_bools` 4 (`TerminalApp`, `WorkspaceChartAppearanceState`, `ProviderPresentationDescriptor`, `ChartLegendOptions`), `dead_code, clippy::all, clippy::pedantic` 2 (generated protobuf modules in `rithmic_protocol/src/lib.rs:10` and the removed FX/CFD broker adapter's `lib.rs:3`).

TODO, FIXME, HACK, XXX, "workaround" and "for now" comments: **0**. Deferred work is not marked in code.

### Approximate dead public API

Of 4,389 non-test `pub` items, **34 are referenced nowhere except their definition**. `apps/desktop` is a lib plus bin crate, so `pub` items there avoid `dead_code` warnings. The strongest candidates:

- `apps/desktop/src/trading.rs`: `dispatch_simulated_market_order:218`, `cancel_simulated_order:441`, `reprice_simulated_order:482`, `kill_simulated_account:576`, `unlock_simulated_account:596`, `prepare_simulated_market_order:650`, `flatten_simulated_account:764`. These are seven simulated-trading entry points with no callers.
- `apps/desktop/src/account.rs:101` `prepare_for_process_lifecycle`; `apps/desktop/src/market_worker.rs:1359` `ui_diagnostics_sender`.
- `crates/application/src/generation.rs:372` `MarketGeneration::require_resnapshot`; `crates/application/src/stream.rs:60` `into_parts`.
- `crates/provider_history/src/handoff.rs:76, 161` `into_items`, `require_snapshot_after`; `coverage.rs:131` `confirmed_empty_ranges`.
- `crates/market_engine/src/lib.rs:1129` `visible_consumer_count`; `crates/platform_runtime/src/market_socket.rs:174` `shutdown_handle`; `crates/observability/src/lib.rs:293` `try_record_chain`; `crates/context_runtime/src/lib.rs:329` `delete_api_key`.
- `crates/adapters/rithmic_protocol`: order-session methods `request_login_info`, `request_accounts`, `request_trade_routes`, `submit_order`, `cancel_all_orders`, `replay_executions`, `request_fill_history`, `discover_and_login_orders`, and `discover_and_login_pnl`. These may be intentionally ahead of Rithmic onboarding.
- `crates/ui/design_system/src/lib.rs:15` `PLATFORM_CSS`; `crates/study_sdk/src/lib.rs:170` `STUDY_SDK_API_VERSION`.

A further 334 `pub` items are used only within their own file, so their visibility is wider than needed.

### Tests versus code per crate

All 1,873 `#[test]` functions. The workspace has no property-testing, model-checking or snapshot dependencies (`proptest`, `quickcheck`, `loom`, `insta`, `arbitrary`, or `shuttle`). Integration-test directories exist only in `contracts` (1 file), `provider_history` (2) and `study_sdk` (1).

| Crate | Non-test lines | Test lines | Test/code |
|---|---:|---:|---:|
| apps/desktop | 55,271 | 17,025 | 0.31 |
| crates/market_runtime | 30,446 | 15,476 | 0.51 |
| crates/adapters/rithmic_protocol | 18,588 | 6,202 | 0.33 |
| crates/trading_runtime | 12,886 | 4,129 | 0.32 |
| crates/platform_runtime | 9,815 | 4,176 | 0.43 |
| crates/ui/chart_integration | 6,982 | 5,564 | 0.80 |
| removed FX/CFD broker adapter | 4,852 | 3,566 | 0.73 |
| crates/account_runtime | 3,870 | 3,255 | 0.84 |
| crates/market_engine | 3,063 | 1,173 | 0.38 |
| crates/study_sdk | 2,621 | 1,436 | 0.55 |
| crates/ui/terminal_ui | 2,455 | 1,059 | 0.43 |
| crates/adapters/hyperliquid_market | 2,430 | 1,492 | 0.61 |
| **crates/context_runtime** | 2,166 | 313 | **0.14** |
| crates/provider_history | 1,900 | 1,032 | 0.54 |
| removed Level 1 broker adapter | 1,804 | 915 | 0.51 |
| **crates/application** (shared generation and replay model) | 1,692 | 187 | **0.11** |
| crates/domain/market_data | 1,583 | 862 | 0.54 |
| crates/observability | 1,313 | 461 | 0.35 |
| **crates/contracts** | 1,093 | 202 | **0.18** |
| **crates/domain/trading** | 1,041 | 232 | **0.22** |
| crates/ui/design_system | 881 | 424 | 0.48 |
| crates/domain/account | 422 | 157 | 0.37 |
| **crates/domain/instruments** | 320 | 0 | **0.00** |
| tools/naming_check | 106 | 2,548 | (architecture assertions) |
| **Total** | 167,600 | 71,886 | 0.43 |

The desktop holds 33% of the code and the top fix hotspots, and has the lowest test ratio among the large crates. Many desktop tests are long state-setup scenarios (for example, `f504ab868` added 206 test lines to cover one switch race), which suggests there is no reusable scripted-provider or replay harness.

## Duplication clusters

Detection: exact matching over whitespace-normalized significant lines (6-line window), plus structural matching with identifiers and numbers normalized (10-line window). Non-test code only.

- Exact: 6,374 duplicated line pairs out of 121,515 significant lines (about 5%) across 201 file pairs. Structural: 13,175 pairs (about 11%) across 320 file pairs; the cross-file results include false positives from struct-literal patterns.
- Most duplication is **within one file**, which matches the large-file pattern.

| Cluster | Exact dup lines (blocks) | Structural | Example locations |
|---|---:|---:|---|
| market_runtime `realtime.rs` | 521 (63) | 298 | `3458-3484` ≈ `3699-3725` (22 lines); `2808-2853` ≈ `3049-3094` (rithmic vs. candle-provider connect/online handlers) |
| trading_runtime `store.rs` | 483 (56) | **1,371 (120)** | `1751-1768` ≈ `2017-2035`; `1122-1176` ≈ `1217-1271` (per-table row codecs) |
| study_sdk `lib.rs` | 272 (36) | 759 | `1843-1858` ≈ `1883-1898` |
| platform_runtime `lifecycle.rs` | 186 (24) | 84 | `1496-1516` ≈ `1633-1653` ≈ `1660-1680` (three copies) |
| desktop `drawing_toolbar.rs` | 156 (20) | 583 | `255-263` ≈ `330-338` ≈ `406-414` (three copies) |
| account_runtime `account_service/mod.rs` | 140 (21) | | `2482-2494` ≈ `2567-2579`; `828-899` |
| desktop `desktop.rs` | 135 (17) | 235 | `4301-4354` ≈ `4403-4416`; `2672-2685` ≈ `2835-2848` |
| desktop `terminal_chrome.rs` | 119 (15) | 100 | `1262-1274` ≈ `1311-1323` ≈ `1366-1378` |
| market_engine `series_store.rs` | 115 (13) | | `767-816` ≈ `830-875` |
| market_runtime `coordinator.rs` | 114 (17) | 87 | `381-392` ≈ `766-777` |
| market_runtime `runtime.rs` | 113 (13) | 184 | `204-283` ≈ `231-368` (provider start variants) |
| terminal_ui `order_book_view.rs` | 109 (12) | 118 | `1541-1557` ≈ `1618-1634` (bid/ask mirror) |
| rithmic `canonical_market.rs` | 108 (11) | 84 | `744-767` ≈ `782-805` ≈ `830-843` |
| rithmic `order_plant.rs` | 83 | 125 | `1257-1281` ≈ `1351-1375` (25 lines) |
| rithmic `market.rs` | 80 | | `895-917` ≈ `1346-1368` (23 lines) |
| chart_integration `view.rs` | 78 (7) | | `1183-1200` ≈ `1288-1305` (the two `AerisChartView` constructors) |
| **cross-file:** market_runtime FX/CFD broker module ↔ Level 1 broker module | 70 (8) | | FX/CFD `2103-2129` ≈ Level 1 `2506-2532`; `queue_completion` and `flush_completions` are identical in both |
| cross-file: desktop `big_trades_dialog.rs` ↔ `volume_dialog.rs` | | 133 (8) | settings dialog scaffolding |
| cross-file: rithmic `order_session.rs` ↔ `session.rs` | | 98 (7) | login/handshake |

**Parallel implementations, not textual copies.** The four provider realtime workers (`hyperliquid_realtime.rs` 2,036 lines, `rithmic_realtime.rs` 2,071, the removed FX/CFD broker module 2,320, the removed Level 1 broker module 2,692) share only 18 function names. Same-named functions are mostly dissimilar: `run` has a body similarity of 0.35 across all four, `recover` 0.35 (the two removed broker modules), `reconcile` 0.18, `start_record` 0.10, and `run_catalog` 0.08. All four implement the same lifecycle (connect, generation, recover, reconcile, demand diff, publish) independently, each with its own event enum and its own `Recovering` payload shape. The shared "provider-neutral" coordinator still contains provider-specific paths: `realtime.rs` has `rithmic_connecting`, `rithmic_online`, `rithmic_trade`, `rithmic_series_recovering`, `rithmic_recovering`, `rithmic_failed`, `publish_rithmic_live`, `flush_rithmic_demand`, `hyperliquid_demand`, and `flush_hyperliquid_demand`. There are 167 non-test provider-name string literals outside the adapters (market_runtime 104, desktop 36). Examples include `apps/desktop/src/engine_market_worker/replay_conversion.rs:209-327` (`if series.provider_id == "rithmic"` and the two removed brokers' ids) and `engine_market_worker.rs:696` (`if provider == "hyperliquid"`), even though AGENTS.md requires a provider-neutral desktop.

Desktop `market_worker.rs` (3,311 lines) and `engine_market_worker.rs` (2,874 lines plus 2,101 in submodules) share no function names, so they are not duplicates. They are two stacked adaptation layers between `market_runtime` and the GPUI surface, and each has its own generation and recovery vocabulary (200 and 137 `generation` mentions). The name `engine_market_worker` dates from the retired `apps/engine` process (retired 09-08/09).

## Systemic root causes

### 1. Subscription and recovery lifecycles are implicit and split across about 7 layers

Evidence: a chart's data passes through the provider adapter, then a provider worker (4 separate implementations), the market_runtime `Coordinator` (realtime, history and publication), `MarketEngine`, desktop `engine_market_worker`, desktop `market_worker` (with its `MarketGeneration` and coalescing queues), `WorkspaceSurface` (`pending_*` and `rithmic_pending_*` fields), and finally `AerisChartView` and `ChartDataBridge` before reaching Aeris Charts. There are at least 8 lifecycle representations (listed in the census). `ProviderSessionSlot` encodes one session in 9 independent fields. `RithmicSwitchState` is a data-less enum with 4 parallel `Option` fields. Recovery and resume fixes number 38, and persistence and restore fixes number 45. Chains A, B, C and F each show a transition that was missing in one layer and then patched in another. The clearest case is the `AwaitingHistory` dead state, which was fixed with a 30 s watchdog (`4f885153e`). The `recover` vocabulary appears 1,109 times in 76 files.

### 2. Generation fencing is reimplemented ad hoc with no shared typed abstraction

Evidence: 4,693 `generation` occurrences, 228 distinct identifiers, 136 raw-integer generation fields and parameters, 208 non-test comparisons, 8 `AtomicU64` counters, and 26 hand-written increments. There are also an `epoch` vocabulary (321 occurrences; the two removed broker modules use both) and a `STARTUP_CATALOG_COMMAND_GENERATION` sentinel (39 references). The shared `MarketGeneration<T>` is used in 3 files, and its `require_resnapshot` method has no callers. There are 18 "supersede, stale, generation, fence, retire" fix commits, and 41 commits introduced `superseded` and 82 introduced `stale_` through pickaxe search. Each new consumer (order flow, alerts, tabs, watchlist, screener, studies) adds its own generation and later a fix for when it goes stale (for example `f504ab868`, `6f9b25f54`, `94c090baf`).

### 3. God objects make every change touch the same few files

Evidence: `WorkspaceSurface` has 96 fields and 262 methods, `TerminalApp` 63 fields and 260 methods, `AerisChartView` 49 fields and 227 methods, and `Coordinator` about 300 methods over about 11,000 lines. Their files are the top fix hotspots (`desktop.rs` 72, `view.rs` 67, desktop `main.rs` 65, `workspace_surface.rs` 52). Their constructors are among the most-fixed "functions" (22 fixes each for the two `AerisChartView` constructors). The median fix touches 4 files, 93 fixes span 3 or more top-level directories, and 41% of fixes re-touch a function another fix changed in the previous 6 commits. The 100-line function lint keeps functions short, but complexity has moved into very large types and files (12 files over 3,000 lines).

### 4. Errors are strings, so fixes add distinctions one at a time

Evidence: 1,196 non-test `Result<…, String>` occurrences. Several fixes exist only to add a distinction a typed error would have carried from the start: `771a8d732` (Superseded versus Failed surfaced as "Chart unavailable"), `03dbd7132` (make Level 1 broker recovery "typed" hours after `5e868c13c`), `dfe6c2877` ("Demand errors carry a scope"), and `79a65bc7f` ("Chart unavailable" from a capability mismatch). Control flow also branches on message text in three places. There are 322 non-test `let _ =` discards, including demand re-issue (`send_rithmic_demand` ×4), reseed cancellation (×5), catalog publication (×4), and chart data installation (×3).

### 5. Provider semantics are discovered in production, without recorded fixtures or replay

Evidence: Coinbase was replaced by Hyperliquid on 09-06, the same morning a 7-commit history fix series was reverted (-1,150 lines). The Level 1 broker's streaming behaviour was learned one fix at a time: SNAPSHOT_END missing (`9e01ce99e`), out-of-range weekly closes (`70cfb3a01`), newest-first snapshots, and `fromTime` excluding the open candle (`dfe6c2877`). The FX/CFD broker's "close out market-data defects" commit fixed 9 numbered defects (`7bf8f1c02`), and Hyperliquid trade replays on resubscribe were fixed in `32a167dc0`. The workspace has no property-based, model-checking or snapshot test dependencies, only 4 integration-test files, and a test/code ratio of 0.11 for `application` (the shared replay and generation model) and 0.31 for desktop. 41% of fix commits add or change no `#[test]`.

### 6. Provider differences leak into shared layers instead of sitting behind one provider contract

Evidence: there are four independent realtime workers with 0.08 to 0.35 body similarity for same-named functions such as `run`, `recover` and `reconcile`, four separate `*RealtimeEvent` enums, and provider-named functions inside the shared `realtime.rs`. The desktop has 36 provider string literals, including branches in `replay_conversion.rs`, despite the provider-neutral rule. Switch-state fields in the desktop are still named `rithmic_*` but serve all providers. Each fix therefore has to be repeated per provider or patched in shared code with a provider branch (for example `79a65bc7f`, "only demand chart streams the provider publishes").

### 7. Fix commits are batched and feature-sized, and the commit rhythm is reactive

Evidence: the median gap between commits is 15 minutes, with up to 28 fix commits per day. Fix size is 119 lines at the median and 743 at p90; 30 fixes exceed 1,000 lines, and 118 bundle several outcomes. Visual decisions flip within a day: legend colors (`45099f0b9` then `fc785d8ce`), fonts (5 fixes on 09-12), order-book fills (4 fixes on 09-28), and button and menu states (8 fixes on 10-09). There are 5 reverts, 2 duplicated commit subjects, and 0 TODO markers. Large fixes hide design changes, and the absence of TODOs hides known gaps. Both make it harder to see that a given fix is the fourth attempt at the same problem.

### 8. Two-repository chart ownership multiplies the cost of each chart fix

Evidence: 139 charts-pin commits, each editing 5 identical `rev` lines, `Cargo.lock`, and an `expected_revision` string in `naming_check` (135 commits). 80 commits touch only those files. Chains D and E needed a pin bump on nearly every step. This follows from a deliberate rule in AGENTS.md (chart behaviour lives in Aeris Charts), but without a stable host contract and contract tests in Aeris Charts, each behavioural nuance needs a Terminal change, a Charts change, a pin bump, and often a follow-up fix.
