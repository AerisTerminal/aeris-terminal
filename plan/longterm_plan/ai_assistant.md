# Aeris Assistant

The Aeris Assistant is an AI that operates the terminal. It doesn't just chat. A trader speaks or
types what they want ("split this into two vertical charts", "show me US stocks above the 20-day
moving average", "why did I lose money this week?") and the assistant carries it out through the
same typed commands the menus, the command palette and the shortcuts use. It can arrange
workspaces, build charts with indicators, run screens, research markets and news, and review the
trader's own trading history. It is voice-first: always listening for a wake word on the user's
machine, and fast enough to feel like talking to a person.

Item IDs are stable: **AI** checkpoints. A checkpoint is **Complete** only when it works through
the real desktop path on the installed app and its evidence is recorded, not when a unit test alone
passes. Status as of 2026-11-07: planned. Nothing is built yet.

## Principles

- **The assistant drives the platform; it doesn't replace it.** Every action is a typed,
  validated command executed by the component that already owns that behavior (the workspace,
  the chart pane controller, `market_runtime`, `trading_runtime`). The assistant never clicks
  through the UI, never holds its own copy of market, layout or trading state, and never bypasses
  an owner.
- **The model plans; the engine computes.** The model translates intent into commands and
  queries. Every number the user sees (indicator values, screen results, P&L, statistics) comes from
  Aeris's own deterministic code, never from the model's text. This is what makes the assistant
  precise.
- **The user approves every trade.** The assistant may prepare an order, a bracket, a cancel or a
  workflow, but anything that sends, changes or cancels an order shows the existing confirmation,
  and the user answers it. The kill switch, risk locks and the admission path stay exactly as they
  are. A workflow the assistant drafts is saved paused, and only the user arms it (see
  [workflows.md](workflows.md): no AI ever evaluates or fires a workflow).
- **Local-first and fast.** The wake word, voice activity detection and command execution run on
  the user's machine. The voice loop is built to answer in about one second.
- **Exchange data stays on the machine.** Licensed exchange data (Rithmic, a stock or options
  data partner) is never sent to a model provider or an Aeris server unless a data agreement
  explicitly allows it. The model receives commands, app structure, symbols and the user's own
  questions; numeric results stay local (see Data boundary).
- **No backend until funded.** Version 1 runs with no Aeris server: the user brings their own model
  API key, and voice runs locally. A hosted service is added only after funding (AI8).
- **Bounded.** Every model call, tool loop, audio buffer, transcript history and research job has
  an explicit limit, a timeout and a cancel path.

## What the user can do

| Area | Examples | Executed by |
| --- | --- | --- |
| Workspace | "Two vertical charts", "4 panes in a grid", "new tab called Gold", "save this layout" | Workspace tabs and `LayoutLibrary` |
| Charts | "BTC on 15 minutes", "switch to footprint", "add RSI 14 and VWAP", "dark theme", "sync crosshair" | `ChartPaneController`, chart type, studies, `AerisTheme`, tab chart sync |
| Composed setups | "Give me a clean scalping layout for NQ" | A plan of many commands, previewed, then applied as one undoable step |
| Screening | "US stocks above the 20-day MA with volume over 1M", "perps with highest funding" | Screener engine (extended, AI3) |
| Research | "What moved crude today?", "summarize the latest CPI release", "what's in the news for NVDA?" | Context sources, news (AI6), market history |
| Trade review | "Why am I losing?", "my win rate on Mondays", "average hold time on losers" | `TradingService::history`, round trips, equity samples |
| Trading | "Buy 1 MES at market", "move my stop to break-even", "flatten everything" | `TradingService` entry points, always behind confirmation |
| Workflows | "Close my position at 3:55 PM Chicago every day" | Drafts a `WorkflowDefinition`; the user saves and arms it |

## How it works

```
 Microphone ─► Wake word ─► Voice activity ─► Speech-to-text ─┐
 (local)        (local)       (local)          (local or cloud) │
                                                                 ▼
 Keyboard (assistant bar / command palette) ──────────► Assistant session
                                                                 │
                     ┌───────────────────────────────────────────┤
                     ▼                                           ▼
             Model provider (tool calling)              Context builder
             BYOK in v1, hosted in AI8                  (app structure, symbols,
                     │                                    no licensed prices)
                     ▼
             Tool calls ─► Command validator ─► Owner executes ─► Result (local)
                                   │                                   │
                                   └── trade? ─► user confirmation      ▼
                                                               Spoken / written reply
                                                               (text-to-speech, streaming)
```

### 1. Command layer (the core)

Aeris already has a typed command system: `CommandId` in `apps/desktop/src/command_registry.rs`,
the GPUI `actions!` block in `apps/desktop/src/desktop.rs`, and the command palette that dispatches
them. About 40 commands are already clean entry points (splits, close pane, workspaces, panels,
chart types, five fixed intervals, layout save, market buy and sell, cancel all, flatten, kill
switch, provider connect and disconnect).

Many actions are still reachable only through UI handlers or desktop-internal methods:
choosing any instrument, any interval, adding, removing or configuring studies, theme and
language, chart sync, screener filters, order ticket parameters, workflow editing, layout library
management and context panel keys.

AI1 promotes these to typed, parameterized commands, owned by the desktop next to `CommandId`:

- Each command has a name, a description, a JSON schema for its parameters (generated with
  `schemars`), a validation step and an executor that calls the existing owner method
  (`select_instrument`, `select_interval`, `split_pane_vertical`, `apply_theme`, the study runtime's
  typed settings, and so on).
- The same commands power the palette, shortcuts and the assistant, so there is one way to do each
  thing. No assistant-only code path changes app state.
- Commands are grouped by risk: **view** (layouts, charts, themes), **read** (history, screens,
  research), **trade** (orders, cancels, flatten, workflows). Trade commands always require
  confirmation.
- A multi-step plan ("clean scalping layout") applies as one undoable batch with a preview.

This layer is valuable without AI. It makes the palette complete and the platform scriptable.

### 2. Assistant session (`assistant_runtime`)

A new runtime crate owns the model conversation, tool-call loop, voice pipeline and research
jobs. The desktop holds a command and view handle from `DesktopServices`, like the other runtimes.

- Runs on its own background threads; nothing blocks the UI thread. Tool calls that change the app
  return to the UI thread as commands, and their results come back as typed outcomes.
- Bounded tool loop: at most 8 tool calls per request and a 30-second request deadline, then the
  assistant stops and says what it did.
- Conversation history is bounded (recent turns plus a summary) and stored locally only if the
  user turns history on.
- Model provider behind one `ModelProvider` trait: Anthropic, OpenAI and Google implementations
  over `reqwest` with streaming. A small, fast model handles simple commands; a stronger model
  handles research and multi-step plans (routing by request type).
- API keys live in the native vault, like provider logins, and never appear in logs.

### 3. Voice pipeline

The target is under one second from the end of speech to the start of the reply, and under 300 ms
for simple commands that need no spoken reply ("two vertical charts" just happens).

| Stage | Choice | Runs | Notes |
| --- | --- | --- | --- |
| Microphone capture | `cpal` | Local | 16 kHz mono ring buffer, bounded |
| Wake word ("Hey Aeris") | openWakeWord (ONNX) or Picovoice Porcupine | Local | Always on, low CPU; no audio leaves the machine before the wake word |
| Voice activity detection | Silero VAD (ONNX) | Local | Detects end of speech quickly |
| Speech-to-text | `whisper-rs` (whisper.cpp, GPU on Windows and Apple silicon), or Deepgram streaming | Local by default, cloud optional | Local is free and private; cloud is faster on weak machines |
| Text-to-speech | Kokoro or Piper (local), or OpenAI or ElevenLabs streaming | Local by default, cloud optional | Speech starts streaming before the reply is complete |
| Playback | `rodio` | Local | Barge-in: talking stops playback |

- Push-to-talk shortcut as an alternative to the wake word.
- A clear on-screen indicator whenever the microphone is listening after the wake word.
- Wake word and microphone are off by default and switched on by the user.
- Permission prompts on macOS (microphone) are handled explicitly.

### 4. Screening engine (AI3)

The current screener lists provider statistics (Hyperliquid perpetuals and spot) with category
filters and column sorting. It can't evaluate indicator conditions.

AI3 adds a typed screen query that the model fills in and Aeris evaluates:

```
Screen {
  universe: US stocks | perps | futures | watchlist | ...,
  timeframe: 1D,
  conditions: [ close > SMA(20), volume > 1_000_000, change_1d > 2% ],
  sort: change_1d desc,
  limit: 50,
}
```

- Conditions use the existing study runtime's indicator calculations, so a screen and a chart
  agree on every value.
- Bars come from `market_runtime` on demand, within bounded concurrency and the provider's request
  budgets. Large universes need a provider with bulk daily bars (a stock data partner such as
  ThetaData or AlgoSeek).
- Results show in the Screener page, are clickable into charts, and can be saved.

### 5. Research and news (AI6)

- Existing context sources (EIA, USDA, FRED, BLS, BEA, the Federal Reserve, NOAA, CFTC COT) become
  read tools the assistant can query.
- News is a new context source, fetched directly from its provider with the user's own API key
  (operating constraint 3), with bounded polling and caching.
- The assistant cites the source and time of every fact it states.

### 6. Trade review

Read-only tools over the trading owner's history API: closed trades, round trips, fills, equity
samples and risk sessions. Statistics (win rate, average win and loss, hold times, results by
time of day or weekday, behavior after a loss) are computed by Aeris, and the model explains them.

## Data boundary

This is a release gate, not legal advice; it follows the data licensing checklist in `AGENTS.md`.

| Data | Sent to the model provider? |
| --- | --- |
| User's request, app structure, command schemas, symbols, layout names | Yes |
| Public non-exchange data (context sources, crypto from public feeds) | Yes |
| News text | Yes, if the news provider's terms allow it |
| The user's own trade history and statistics | Yes, only with the user's consent (they own it) |
| Licensed exchange prices, quotes, depth, screen values (Rithmic, stock and options partners) | **No**, unless the data agreement allows it. The model gets a reference ("12 matches, shown in the Screener") and the values stay on screen |

A fully local model (for example through llama.cpp) is an option later, for users who want nothing
sent to a cloud model at all.

## Checkpoints

| ID | Checkpoint | Status |
| --- | --- | --- |
| AI1 | Typed command layer covering the whole platform | Open |
| AI2 | Assistant bar with text, BYOK model, view and read commands | Open |
| AI3 | Screening engine with indicator conditions | Open |
| AI4 | Voice: push-to-talk, local speech-to-text and text-to-speech | Open |
| AI5 | Always-on wake word and barge-in | Open |
| AI6 | Research tools: context sources and news | Open |
| AI7 | Trade commands with confirmation, and workflow drafts | Open |
| AI8 | Hosted AI service and paid tier (after funding) | Open |

### AI1 — Command layer

- [ ] Parameterized commands for instrument, any interval, studies (add, remove, configure), theme,
      language, chart sync, layout library, panels, panes and tabs.
- [ ] JSON schemas generated from the command types; validation errors are typed.
- [ ] The palette and shortcuts use the same commands.
- [ ] Batched plans apply as one undoable step with a preview.

### AI2 — Assistant bar

- [ ] `assistant_runtime` crate with a bounded session, `ModelProvider` trait and streaming.
- [ ] API key stored in the native vault; settings to choose provider and model.
- [ ] View and read commands only; replies stream into the assistant bar.

### AI3 — Screening engine

- [ ] Typed screen query, evaluated with the study runtime on `market_runtime` history.
- [ ] Bounded concurrency and provider budgets; results in the Screener page.

### AI4 and AI5 — Voice

- [ ] Push-to-talk, local speech-to-text and text-to-speech, measured latency.
- [ ] Wake word, voice activity detection and barge-in; listening indicator; off by default.

### AI6 — Research

- [ ] Context sources as read tools; one news source with the user's own key; cited answers.

### AI7 — Trading

- [ ] Order, modify, cancel and flatten commands through `TradingService`, always behind the
      existing confirmation.
- [ ] Workflow drafts from a description, saved paused for the user to review and arm.

### AI8 — Hosted service (after funding)

- [ ] A small AWS service (Lambda and API Gateway, Cognito-authenticated) that holds model keys,
      meters usage per plan and forwards model requests. It never receives market data, orders or
      account state (see [aws_architecture.md](aws_architecture.md)).
- [ ] Paid AI tier; BYOK stays available.

## Acceptance

- Every assistant action goes through a typed command and its existing owner; no assistant-only
  state or path.
- No order is sent, changed or cancelled without the user answering a confirmation.
- Every number in an answer comes from Aeris's own computation, not the model.
- Voice: the wake word works with no audio leaving the machine before it; measured end-of-speech
  to start-of-reply under one second on the reference machines (Windows x64, Apple silicon).
- Licensed exchange values never reach a model provider without an agreement that allows it.
- Model calls, tool loops, audio buffers and history are bounded, cancellable and time out.

## Costs (estimates, November 2026 public pricing; check before budgeting)

| Item | Version 1 (BYOK, local voice) | Hosted (AI8) |
| --- | --- | --- |
| Model usage | Paid by the user | About $0.50 to $2 per active user per month on small models; $5 to $10 on mid-tier models with cloud voice |
| Speech-to-text and text-to-speech | Free (local) | Free locally, or cloud about $0.004 to $0.02 per minute |
| Wake word | Free (openWakeWord); Porcupine needs a commercial licence | Same |
| Backend | None | About $20 to $50 per month at small scale, growing with usage |
| Total for Aeris | About $0 per month | 100 active users: about $50 to $1,000 per month. 1,000 active users: about $500 to $10,000 per month |

The main cost of version 1 is engineering time, mostly AI1 (the command layer) and AI3 (the screening
engine), not API spend.

## Risks

- **Investment-advice rules.** The assistant shows rankings and evidence ("strongest by 20-day
  relative strength"), not personal recommendations to buy or sell.
- **Model mistakes.** Mitigated by typed validation, previews, undo and confirmations, and by never
  letting the model produce numbers.
- **Voice on weak machines.** Local speech-to-text needs a GPU for the latency target; offer cloud
  speech-to-text as a fallback.
- **Data licences.** Partner agreements decide how much the model can see (Data boundary).

## Progress log

- 2026-11-07: Plan written. Mapped the existing command system (`CommandId`, GPUI actions, palette),
  the action entry points that need promoting to commands, the screener's current limits, the
  context sources and the trading history APIs. No AI, LLM or audio code exists yet.
