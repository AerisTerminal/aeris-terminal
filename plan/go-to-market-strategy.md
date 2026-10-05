# Go-to-Market Strategy: Native Trading Platform

Prepared October 2026. Research-based. Prices and program terms change often, so confirm each one before you quote it to a partner. Items marked "(verify)" came from secondary sources.

---

## 1. The edge, in one sentence

**"The only polished, native trading platform on Windows, macOS and Linux. It installs in about 14 MB, uses 100–200 MB of RAM, and can carry your brand."**

### Why this edge is real

- Most futures desktop platforms are Windows-only. This includes NinjaTrader desktop, Quantower, Deepcharts (Volumetrica), R|Trader Pro and Jigsaw. Sierra Chart is Windows-first, with Linux support.
- Only MotiveWave and Bookmap run on macOS and Linux today, and both are Java-based. ATAS X is in beta on macOS.
- Tradesea, the newer platform that prop firms partner with, is web and mobile only. Its desktop version is "coming soon."
- ProjectX ended third-party prop firm licensing at the end of February 2026. Firms like Bulenox, Tradeify, Lucid Trading, Alpha Futures, TradeDay and Phidias had to find new platforms.
- Apple is phasing out Rosetta 2. In macOS 28 it stops working for most apps, so Windows trading apps wrapped for Mac become less reliable.

### How to prove the edge (claims alone won't win meetings)

Measure everything on the same machine and publish the method:

- Install size, cold start time, RAM after loading 10 charts, chart frames per second, and the time from clicking an order to seeing it on screen.
- Compare against at least NinjaTrader, Quantower and Sierra Chart. On Mac and Linux, compare against those same platforms running in a virtual machine.
- Never describe the product as "vibe coded." Describe the process instead: a headless core library, a test suite, Rithmic conformance once you pass it, and signed releases.

---

## 2. Market segments, in priority order

| Priority | Segment | Why | Gate |
|---|---|---|---|
| 1 | **Futures prop firms** | Lost ProjectX; need branded platforms; decide fast; buy B2B | Rithmic conformance |
| 2 | **Small and mid futures brokers / IBs** | List many third-party platforms; some openly invite new ones | Rithmic conformance |
| 3 | **Crypto exchange broker programs** | Pay revenue share on routed volume; some need no company | Indian legal check on rebates |
| 4 | **cTrader brokers and prop firms (forex/CFD)** | cTrader Open API is free, uses OAuth and works with any cTrader broker | Spotware app approval, then the integration (`plan/trading_roadmap.md` T9) |
| 5 | **Large brokers (Schwab, IBKR Web API, TradeStation)** | Big brands, but need a US entity, traction and due diligence | Revenue and a US entity |

### What not to do

- Don't pitch yourself as an MT5 replacement to MT5 brokers. MetaQuotes doesn't allow third-party terminals on MT4/MT5 accounts, and the challengers (TradeLocker, Match-Trader, DXtrade, cTrader) sell a full server and back office, not just a front end.
- Don't approach Topstep. It runs a closed ecosystem.
- Deprioritise Optimus Futures, EdgeClear, Ironbeam and Cannon, since each has its own platform, and NinjaTrader/Tradovate, which is a direct competitor.
- Don't promote offshore forex brokers to Indian users, and block US users from offshore crypto-derivatives integrations.
- Don't advertise support for a broker before that broker approves you.

---

## 3. Target list

What Aeris can do on each route today, and the work left to make it trade, is tracked in
`plan/trading_roadmap.md` §9 (Broker coverage). As of October 2026, market data works through
Rithmic (Test system only), tastytrade (Level 1) and Hyperliquid. No live broker or exchange
account can trade yet.

### Futures prop firms (primary)

| # | Target | Hook for the first message |
|---|---|---|
| 1 | **Bulenox** | Rithmic-only and lists 21 platforms, including tiny vendors; lost ProjectX |
| 2 | *(removed: Lucid Trading, by founder's decision)* | |
| 3 | **Tradeify** | Only 4 Rithmic front ends, and Sierra is the only native desktop among them; lost ProjectX |
| 4 | **Alpha Futures** | Actively changing platforms in 2026; dropped NinjaTrader/Tradovate on July 12, 2026 |
| 5 | **Tradesea-partner firms**: BluSky, EdgeProp, Halcyon, P1 Futures, Shark Futures, YRM Prop, QT Futures, Edmond, FundYourEdge | Already buy an outsourced front end; Tradesea has no desktop yet |
| 6 | **Take Profit Trader** | No platform of its own; 11 Rithmic platforms |
| 7 | **Phidias** | Rithmic-only; lost ProjectX |
| 8 | **TradeDay** | Lost ProjectX (TradeDayX); uses Rithmic and Tradovate Prop |
| 9 | **My Funded Futures, Elite Trader Funding, Earn2Trade** | Multi-platform; still confirming feed details (verify) |

### Futures brokers and introducing brokers

| # | Target | Hook |
|---|---|---|
| 1 | **Discount Trading** | Its Rithmic page says "Have your own platform? reach out to our team" |
| 2 | **AMP Futures** | Lists 50+ platforms, including small vendors; already pays to give Quantower to clients free |
| 3 | **Stage 5 Trading** | Has bought a branded white label before (S5 BookMap) |

### Crypto revenue programs (fastest income)

| Program | Why | Caution |
|---|---|---|
| **Hyperliquid builder codes** | Open to anyone with no company and no approval; you set a fee of up to 0.1% on perps; needs 100 USDC | Front end blocks US users |
| **Bybit API Broker** | Replies within 48 hours; 30–50% of net fees; supports OAuth; registered with India's FIU | Volume thresholds for the higher tiers |
| **Kraken API Partner** (launched July 2026) | Explicitly targets trading terminals; lifetime commission; serves the US | Rates not public; India availability unclear |
| **Bitget** | Broker ID issued the same day; 25–50% | Paused new Indian sign-ups (Feb 2026) |
| **OKX Broker** | OAuth with PKCE suits desktop apps; up to 50% | Doesn't serve India |

Before taking any exchange rebates, ask an Indian lawyer or CA whether earning them makes you a crypto service provider that must register with the FIU.

### Crypto login routes (user logs in; exchange supplies data and execution)

Market data is public on every exchange below (WebSocket order books and trades, no login needed). The login only matters for orders, positions and balances.

| Rank | Exchange | How the user logs in | Notes |
|---|---|---|---|
| 1 | **OKX** | OAuth 2.0 with PKCE (no server or client secret needed), or "Fast API" (one click creates a trade-only key) | Best fit for a desktop app. Access token lasts 1 hour, refresh token 3 days. Apply as an OAuth broker; review within about 2 days. Tag orders with the broker code for up to 50% commission. Not available to Indian or US users |
| 2 | **Bybit** | OAuth (broker approval first), which returns a trade key for the user; or a pasted API key usable only on your platform | Send the Broker ID with every order (`referer`). 30–50% of net fees at volume tiers. FIU-registered, so usable by Indian users |
| 3 | **KuCoin** | "Fast API" through KuCoin OAuth 2.0, one-click authorization | API Broker program; FIU-registered |
| 4 | **Hyperliquid** | User's wallet signs once to approve a builder fee and an agent (API) wallet; the agent wallet then trades | No company or approval needed; 100 USDC in perps account; fee up to 0.1% on perps, 1% on spot; agent key can't withdraw. Block US users |
| 5 | **Binance** | Binance OAuth exists, but trading access comes through the Binance Link program; most terminals use pasted API keys with a Link ID on orders | Biggest liquidity; FIU-registered (2024). Apply to Link and ask whether OAuth trading scope is available for desktop terminals |
| 6 | **Kraken** | Pasted API key (Kraken Connect OAuth is in legacy docs; confirm in the partner application) | API Partner Program (July 2026) explicitly targets trading terminals; lifetime commission; serves the US |
| 7 | **Coinbase Advanced Trade** | OAuth 2.0; user picks which portfolio to grant; spot orders must send `retail_portfolio_id` | Clean US option; spot, US futures and international derivatives. No rebate program found |
| 8 | **Bitget, Gate** | Pasted API key with broker ID (Bitget ND broker uses sub-accounts) | Gate pays up to 60%. Bitget paused new Indian sign-ups (Feb 2026) |
| 9 | **Deribit** | Pasted API key (client credentials) | Options leader; good for an options-chain feature |
| India | **Delta Exchange India, CoinDCX** | Pasted API key | Domestic, FIU-registered, INR. Delta India has crypto futures and options and an active algo community; ask about a partner arrangement |

Rules for every route:
- Ask users for trade-only keys and refuse keys with withdrawal permission. Check permissions on connect.
- Keep keys and tokens in the OS keychain, sign requests on the device, and never send keys to your servers.
- OAuth flows that need a fixed redirect URL or an IP whitelist (OKX code mode, Bybit) may need a small relay server; prefer PKCE where offered.
- Geo-block per exchange: US users off offshore derivatives, Indian users only on FIU-registered venues.

### Forex/CFD (user logs in; broker supplies data and execution)

| Rank | Route | Login | Notes |
|---|---|---|---|
| 1 | **cTrader Open API** (IC Markets, Pepperstone, FP Markets, FxPro, BlackBull and others) | OAuth 2.0 | Free; Spotware reviews each new app; one integration covers many brokers. No US brokers |
| 2 | **TradeLocker Public API** | Email, password and server, exchanged for a JWT | Used by 320+ brokers and 1,281+ prop firms; join the Developer Program for multi-user apps; quotes by REST polling |
| 3 | **DXtrade** | Broker-issued credentials | Each broker must enable API access and may whitelist IPs |
| 4 | **Match-Trader Platform API** | Credentials or one-time token | REST plus WebSocket; brokers can disable it |
| 5 | **OANDA v20** | Token; OAuth via the Partner Form | US-regulated; mature API |
| 6 | **Interactive Brokers TWS API** | User's local TWS or IB Gateway | No fee or approval; works for US users |
| 7 | **Capital.com** | User API key | Self-serve; no vendor program; 10-minute sessions; 40 streaming instruments |
| 8 | **IG (outside US)** | User API key and credentials | Vendor terms not public |
| — | **tastyfx (IG US)** | Unclear | Same group as tastytrade (IG Group); ask your tastytrade contact for an introduction |
| — | **FOREX.com** | Credentials after support enables API | US-regulated; terms unclear |

**Cautions:**
- **US users:** retail forex is legal only through firms registered with the CFTC (OANDA, FOREX.com, IBKR, tastyfx).
- **India, trading:** RBI does not allow residents to send margin abroad for forex, and many of these brokers are on RBI's Alert List.
- **India, marketing:** the Alert List also covers sites that promote unauthorised platforms. Don't market forex integrations to Indian residents, and get Indian legal advice.
- **Credentials:** routes that need user passwords should store them in the operating system keychain, and you should check each broker's terms on third-party access.

---

## 4. The offer

### For prop firms and brokers (B2B)

1. **Free listing:** you appear in their list of supported platforms, and their traders pay you directly. This builds the relationship.
2. **Paid pilot (60–90 days):** setup fee waived, $500–750 a month, their branding, up to 250 active accounts. It turns into Starter automatically if they're happy.
3. **White-label Starter:** $2,500–5,000 setup and a $750–1,500 monthly minimum that includes 250 active accounts, then $2–4 per active account per month.
4. **Growth:** about $10,000 setup and a $2,500–4,000 monthly minimum. Includes custom branding, a display for the firm's risk rules and priority support.
5. **Enterprise:** custom pricing, with source-code escrow and a written support agreement.

**Exclusivity (any tier, only if the firm asks):**
- **Price:** at least **$100,000 per year**, renewed yearly. Never sell it as a one-time or permanent fee.
- **Charged on top:** the exclusivity fee buys only the exclusivity. Setup, the monthly minimum and
  per-account fees are still charged in full.
- **Why it costs this much:** every firm turned away is lost revenue, and an exclusive firm gets a
  dedicated branded build that Aeris develops, distributes and maintains for it alone. Price the fee
  as the revenue expected from 3–5 other firms over the same term.
- **Term:** 12 months; renewal at Aeris's option.
- **Scope:** one named category only, for example "no other futures prop-firm white label." Plain
  listings with other firms, brokers, and direct sales to traders stay open.
- **Earned, not permanent:** if the firm misses its agreed seat or revenue minimum, exclusivity ends
  automatically.
- **Ownership:** Aeris keeps all code and intellectual property in every case. The firm receives a
  non-exclusive (or, with this fee, category-exclusive), non-transferable licence to its branded
  build, not ownership of the product.

For a large firm such as FTMO, which bought OANDA for about $422 million (Finance Magnates,
September 2026), $100,000 a year is a small line item, so don't discount it.

**Reference points:**
- Match-Trader white label: $2,500–4,000 a month.
- TradeLocker: about $5,000 a month for 1,000 live accounts.
- An old Quantower white-label promotion: $1,990 a month.
- MT5 white label in year one: roughly $40,000–90,000.

A new vendor has to start below these to win pilots. Test these numbers with 3–5 firms before publishing them.

**What prop firms need from a white label:**
- Their logo, colours and name, and their own installer.
- A display of their rules that also warns the trader before an order would break one: daily loss limit, trailing drawdown, maximum contracts and news blackout windows. The firm's server-side risk system stays in charge of actually enforcing the rules.
- One-click connection with their Rithmic credentials.

### For traders (end-user pricing)

The plans published at https://aeristerminal.com/pricing/ are defined in `plan/pricing_strategy.md`,
which is the authoritative source:

| Plan | Price | Includes |
|---|---|---|
| Free | $0 forever, no card | Hyperliquid plus 1 Rithmic login, 1 live account, unlimited simulated accounts, 1 workspace with 4 charts, footprint on 1 chart |
| Pro | $39/mo or $29/mo yearly | Multiple connections, 3 live accounts, unlimited workspaces and indicators, full order flow |
| Prop | $69/mo or $49/mo yearly | Pro plus the multi-account copier and up to 20 live accounts |

**Open decision:** this plan's founder lifetime licence ($499–799 for the first 300–500 buyers, used
in §7 and §8) conflicts with `plan/pricing_strategy.md`, which skips lifetime licences at launch and
offers 40% off a yearly plan instead. Settle it before launching a founder offer.

**Reference points:** Quantower $70/mo, ATAS Pro €69.95/mo, Deepcharts $69/mo, Sierra Chart $36–56/mo. Lifetime licences elsewhere cost $990–1,999. Consider regional pricing in rupees for India.

### Pricing rules

- Charge US brokers for using the software (a flat fee or per active user). Don't charge per trade or per new account, because that kind of payment can trigger US broker registration rules. Get legal advice before anything else.
- Revenue share on evaluation fees is common with prop firms, but have a lawyer check each contract anyway.
- Keep market data flowing straight from the broker or Rithmic to the user's computer. Don't relay data through your own servers, or you may need exchange distribution licences. Confirm this with a lawyer before adding any hosted data features.

---

## 5. Sales kit (build before outreach)

1. **Two-minute demo video:** install, connect, chart, place an order, switch symbols, then the same steps side by side with a competitor.
2. **Benchmark table** from Section 1, with the method published.
3. **A one-page PDF for each segment** (prop firm, broker, crypto exchange).
4. **Security and architecture page:**
   - A diagram showing where data goes. Nothing passes through your servers.
   - Tokens stored in the operating system's keychain.
   - Signed releases and updates.
   - Order confirmations, size limits, a kill switch and rate limits.
   - A statement that the AI never places orders.
   - A named security contact.
5. **White-label demo:** one installer branded with the target firm's logo, sent with the first email.
6. **Proof points:** tastytrade production approval, Rithmic conformance once passed, user counts and zero incidents.

---

## 6. Outreach

### Who to contact

Founder, CEO, CTO, or head of partnerships or business development. Find them through LinkedIn, X and the firm's Discord. Use only publicly listed business contacts.

### Cadence

Day 0 email or DM, then a follow-up on day 3, day 7 and day 14 with new value each time (for example, a branded build or a benchmark). Stop after four touches.

### Cold email template (prop firm)

> **Subject:** Native desktop platform for [Firm] traders on Mac and Linux
>
> Hi [Name],
>
> Since ProjectX went Topstep-only, [Firm] traders mostly use Windows-only or web platforms. We built a native desktop platform that runs on Windows, macOS and Linux. It installs in about 14 MB and uses 100–200 MB of RAM.
>
> I made a version with [Firm]'s branding so you can see it: [link]. Here's a 2-minute video: [link].
>
> We're offering a 90-day pilot: no setup fee, your branding, and your rules shown to traders before they break them. Would a 20-minute call next week make sense?
>
> [Your name]
> [Product] · [Website]

### Broker version: change the hook

> "Your Mac and Linux clients currently run Windows platforms in virtual machines or move to other brokers. We keep them trading with you, with no development work on your side."

### 30-minute meeting structure

1. **5 min:** their situation. Which platforms they offer, what traders complain about, how many traders use Mac or Linux.
2. **10 min:** live demo, branded with their logo.
3. **5 min:** benchmarks and security.
4. **5 min:** pilot terms.
5. **5 min:** next steps, with a date for the pilot decision.

### Objections and answers

| Objection | Answer |
|---|---|
| "You're too small / what if you disappear?" | Source-code escrow, a written support agreement and a pilot with no long commitment |
| "Is it reliable?" | Passed Rithmic conformance, test suite, signed releases, live on tastytrade with zero incidents |
| "Why not Quantower or Tradesea?" | Native Mac and Linux, a branded desktop app, a tiny footprint and a lower price |
| "You're not a US company" | Invoice as an export of services from India for now, and form a US entity once contracts justify it |
| "Who holds the data?" | Nobody but the trader. Data goes from your systems straight to their computer |

---

## 7. Funding the plan with almost no money

1. **Crypto revenue first:** Hyperliquid builder codes (100 USDC) and Bybit/Kraken applications, after the Indian legal check.
2. **Founder lifetime licences:** cash up front from early users.
3. **Paid pilots** with prop firms.
4. **Indian invoicing:** ask a CA about GST registration and a Letter of Undertaking (LUT), so that export invoices are zero-rated.
5. **US entity:** form it only when contracts or broker requirements justify the yearly cost.
6. **Rithmic costs:** ask Rithmic for the dev-kit, API and conformance costs in writing. One broker lists the Rithmic API at $99.99/mo. Also ask whether they accept an Indian individual or company.

---

## 8. 90-day plan

| Weeks | Actions |
|---|---|
| 1–2 | Build the sales kit (benchmarks, video, one-pagers, security page, white-label theming). Register a cTrader Open API app. Apply to Hyperliquid, Bybit and Kraken. Get Indian legal and CA advice. Confirm Rithmic dev-kit terms and costs. |
| 3–6 | Run Rithmic conformance when Rithmic onboarding allows. Without waiting for Rithmic, build cTrader market data and the shared live venue on cTrader demo accounts (`plan/trading_roadmap.md` T9). Send the first outreach wave to 15 prop firms and 3 brokers. Launch the founder lifetime offer. Post once in each community (Show HN, r/algotrading, r/FuturesTrading, NexusFi). |
| 7–12 | Run 2 pilots. Get listed on rithmic.com/platforms and by Discount Trading or AMP. Ship cTrader trading after demo qualification on two brokers. Send the second outreach wave to cTrader brokers and the remaining prop firms. Reapply to TradeStation with traction. |

### Targets by day 90

- 2 paid pilots
- 1 broker listing
- 300 weekly active users
- First recurring revenue
- Zero order incidents

---

## 9. Main risks

| Risk | Mitigation |
|---|---|
| Fail or delay Rithmic conformance | Build a test suite first that simulates disconnects, partial fills, rejections and rate limits. Build cTrader in parallel so a Rithmic delay does not stall live trading |
| A bug that sends a wrong order | Order confirmations, size limits, kill switch, paper testing for every broker, terms of service that cover execution errors |
| Solo-founder bus factor | Escrow, documentation, signed and reproducible builds |
| Legal exposure (crypto rebates, US broker payments, data licensing) | Indian and US legal checks before signing or taking any revenue share |
| A competitor copies the native desktop approach | Move fast on prop-firm partnerships; each branded deployment makes switching harder for the firm |
