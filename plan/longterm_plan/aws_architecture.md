# Aeris architecture

## Purpose

Aeris is a local-first trading terminal. It puts charts, order flow, market data, trading and
multi-pane workspaces into one fast native application. Market data and account data stay on the
user's machine; no Aeris server ever sees them.

## Platform

- Native desktop application written in Rust with GPUI. Windows first; Linux is supported.
- One application process. Market, trading, account and context work run on bounded background
  workers inside that process; the UI thread never blocks on network or disk.
- A small AWS backend serves the website and broker sign-in only (see [AWS deployment](#aws-deployment)).

## What it does

- Streams live and historical market data from Rithmic, Hyperliquid, tastytrade and cTrader.
- Draws charts with the Aeris Charts engine: candles, footprint, volume, volume profile, studies
  and drawings.
- Places and tracks orders, fills, positions and PnL through cTrader or the simulated venue.
- Shows economic and fundamentals context fetched directly from official public sources.
- Saves workspaces, tabs, layouts and chart settings locally.

## Components and how they talk

```text
                 Desktop UI (apps/desktop, crates/ui)
                 holds command and view handles only
          |              |               |               |
   market_runtime   trading_runtime  account_runtime  context_runtime
   (MarketEngine,   (orders, fills,  (Aeris account  (public economic
    provider         positions,       sign-in,        and fundamentals
    sessions,        simulated        disabled for    data)
    history)         venue)           now)
          |
   provider adapters (crates/adapters) --> exchanges and brokers
```

- **Desktop UI** renders state and sends commands. It owns no provider sessions, market state or
  trading state.
- **market_runtime** is the only owner of provider sessions and canonical candles. It opens one
  session per provider, merges on-demand history with live data, and fans updates out to every
  chart. Changing symbol, timeframe or layout never tears down a healthy session.
- **trading_runtime** is the only owner of orders, fills, positions and PnL. cTrader orders travel
  over the cTrader session that `market_runtime` already holds; there is never a second connection.
- **account_runtime** and **context_runtime** run their own background work and publish immutable
  snapshots to the UI.
- **Charts**: `crates/ui/chart_integration` passes market, trading and study data to Aeris Charts.
  The engine owns chart state, input, layout and frames; its GPUI backend paints them.
- Runtimes never touch the UI directly. They publish snapshots that the UI picks up on its next frame.
- Queues, caches and retries are bounded everywhere, and stale generations are fenced off so old
  data never overwrites new state.

## AWS deployment

All AWS infrastructure is defined with CDK in the website repository (`infra/`), region us-east-1.

| What | Services | Status |
| --- | --- | --- |
| Website `aeristerminal.com` | Route 53, ACM, private S3, CloudFront | Live |
| Broker OAuth `app.aeristerminal.com` (tastytrade, cTrader) | API Gateway, Lambda, DynamoDB, KMS, SSM, CloudWatch alarms | Live |
| Aeris account sign-in, billing, licence leases, release downloads | Planned: Cognito, Paddle, KMS, S3 | Not deployed |

- Broker OAuth only handles broker authorization tokens. It never receives market data, orders or
  account state.
- Sign-in and automatic updates are disabled in the desktop until the account backend exists.
- Deploy the site with `npm run deploy` and the stacks with `npx cdk deploy` from the website repo.

## Paths

| What | Path |
| --- | --- |
| Terminal (this repo) | `C:\Users\devraj\Downloads\Softwares\aeris-terminal` |
| Website and AWS infra | `C:\Users\devraj\Downloads\Softwares\aeris-website` |
| Chart engine | `C:\Users\devraj\Downloads\Softwares\aeris-charts` (pinned Git dependency here) |
| Rithmic Provider Kit | `C:\axiusflow-deps\provider-kit` (licensed, never in Git) |

### Terminal

| Directory | Contents |
| --- | --- |
| `apps/desktop` | The desktop application |
| `crates/market_runtime`, `crates/market_engine` | Provider sessions and canonical market state |
| `crates/trading_runtime` | Orders, fills, positions, PnL, simulated venue |
| `crates/account_runtime` | Aeris account sign-in |
| `crates/context_runtime` | Public economic and fundamentals data |
| `crates/adapters` | Rithmic, Hyperliquid, tastytrade and cTrader protocols |
| `crates/domain`, `crates/contracts`, `crates/application` | Shared types and saved-workspace format |
| `crates/provider_history` | History loading and live cutover |
| `crates/study_sdk` | Study SDK |
| `crates/platform_runtime`, `crates/observability` | OS boundary, diagnostics |
| `crates/ui` | Design system, chart integration, shared UI |
| `third_party/gpui_base` | Vendored GPUI components |
| `tools` | `naming_check` architecture checks, Windows tooling |

### Website

| Directory | Contents |
| --- | --- |
| `src` | Next.js site (static export) |
| `infra/lib` | CDK stacks: DNS, site, mail, broker OAuth |
| `infra/backend/broker_oauth` | Broker OAuth Lambda |
| `scripts` | Dev and deploy scripts |

### Chart engine

| Directory | Contents |
| --- | --- |
| `crates/aeris_charts_core`, `crates/aeris_charts_indicators` | Chart math and indicators |
| `crates/aeris_charts_engine` | Chart state, input, layout and frame building |
| `crates/aeris_charts_render`, `_render_gpui`, `_render_wgpu` | Shared draw list, GPUI and WebGPU renderers |
| `crates/aeris_charts_native` | CPU rasterizer for PNG output, parity and performance gates |
| `crates/aeris_charts_wasm`, `packages/charts` | Browser build and npm package |
