# Aeris pricing: free plan, tiers and launch

Research from vendor pricing pages and 2026 reviews (NinjaTrader, Quantower, ATAS, Bookmap, Sierra
Chart, DeepCharts, TopstepX, trade copiers) and the ChartMogul / ProductLed 2026 conversion report
(200 products). Competitor prices were checked in September 2026 and must be re-verified before
they are published.

> **Recommendation: yes to a real free plan, and sell scale, not safety.** Every serious competitor
> except Sierra Chart and DeepCharts has a permanent free tier, and prop firms give their traders
> free platforms. A paid-only Aeris starts behind. Because Aeris is local-first and traders pay their
> own market data, a free user costs about $0 until 10,000 monthly users and $0.015 each after that.
> Free users are cheap acquisition, not a cost problem.

| Plan | Price |
| --- | --- |
| Free | $0, usable forever |
| Pro | $39/mo ($29/mo yearly) |
| Prop | $69/mo ($49/mo yearly) |
| Our cost per free user | ~$0 under 10k monthly users |

## Website pricing section requirement

When the pricing section of the Aeris website is designed, it **must include a competitor comparison**
built from the [How competitors price](#how-competitors-price) table below, so visitors can see how
Aeris pricing compares with the platforms and copiers they already know. In particular:

- Show Aeris Free next to the competitors' free tiers, and Pro and Prop next to their paid tiers.
- Show the Prop plan against the cost of a platform plus a separately bought trade copier
  ("platform and copier for the price of a copier").
- Re-verify every competitor price on the vendor's own pricing page immediately before publishing,
  state the date checked, and keep the comparison factual (no claims about competitors that their
  own pages do not support).
- Compare only Aeris features that have shipped; mark anything else as coming soon.

## Recommended plans

| | Free | Pro | Prop (platform + copier) |
| --- | --- | --- | --- |
| Price | $0 forever, no card | $39/mo or $29/mo yearly ($348) | $69/mo or $49/mo yearly ($588) |
| Market connections | Hyperliquid + 1 Rithmic login | Multiple | Multiple |
| Live trading accounts | 1 (unlimited simulated) | 3 | Up to 20, packs above that |
| Workspaces / charts | 1 workspace, 4 charts | Unlimited | Unlimited |
| Indicators per chart | 3 | Unlimited | Unlimited |
| DOM, chart trading, hotkeys, brackets | Included | Included | Included |
| Flatten, kill switch, daily-loss lock | Always free | Included | Included |
| Prop-firm rule profiles + meters | 1 profile | Unlimited | Unlimited, per account |
| Order flow (footprint, CVD, big trades) | Footprint on 1 chart | Full | Full |
| Heatmap, replay, journal (as they ship) | No | Included | Included |
| Multi-account trade copier | No | No | Included |
| Support | Community | Email | Priority |

### Why these lines

- Limits go on scale and power: accounts, charts, indicators, order-flow depth. That is exactly what
  a trader outgrows as they add prop accounts.
- Flatten, kill switch and daily-loss lock stay free. Protecting accounts is the brand promise, and
  paywalling safety costs trust and word of mouth.
- A footprint on one chart lets order-flow traders see what they would be paying for; Quantower and
  ATAS hide it completely.

### Why these prices

- Pro at $39 sits under Quantower ($70), ATAS Pro (€70), DeepCharts ($59+) and Bookmap Global ($49):
  the right place for a new entrant.
- Prop at $69 costs less than the platform plus a copier bought separately (for example ATAS Pro plus
  Tradesyncer Basic is about $125/mo). "Platform and copier for the price of a copier" is the pitch.
- Stay above $10: Paddle's $0.50 per transaction is 6.3% at $39 but 10% at $9, and Paddle
  custom-prices sub-$10 products.

## How competitors price

| Product | Free tier | Paid | Note |
| --- | --- | --- | --- |
| NinjaTrader | Yes: charting, sim, live trading | $99/mo or $1,499 lifetime; Order Flow+ $59/mo | Earns on brokerage commissions, not software |
| Quantower | Yes: 1 connection, 2 indicators/chart, DOM | $40/mo crypto; $70/mo All-in-One ($49 annual); $1,690 lifetime | Free to Topstep and several broker customers |
| ATAS | Yes: 2 assets, 3 indicators/chart | €25 / €70 / €90 per month (€20–50 annual); €999–1,999 lifetime | 14-day trial; footprint only from Pro |
| Bookmap | Yes: 1 crypto instrument | $19 / $49 / $99 per month; $1,990 lifetime | Free tier is crypto-only |
| Sierra Chart | No | $26–56/mo, up to 35% off yearly | Cheapest serious futures platform |
| DeepCharts | No (free through Phidias prop firm) | $59–125/mo | Includes risk manager and a beta trade copier |
| TopstepX | Free to Topstep traders | Bundled with evaluations | Prop firms give platforms away |
| Trade copiers | No | Tradesyncer $49–149/mo; Copilink $29–38/mo; Replikanto $299 once | Sold separately from the platform |

## Free plan vs trial: what the data says

Freemium converts 3–5% of sign-ups (8–12% is great); a no-card trial 4–6% (10–15% great); a
card-required trial 25–35%, but with far fewer sign-ups. Per 1,000 site visitors, freemium produced
about 5 paying customers against 3.6 for a normal trial. The reverse trial (full product for 14
days, then drop to Free) is statistically about the same as freemium, but it lets every new user try
the copier and full order flow, which are the features they would pay for.

## Monthly revenue per 10,000 free sign-ups

Assumes 60% Pro and 40% Prop, half on yearly billing (blended ~$44 per payer). Conversion rates are
the 2026 benchmark bands, not Aeris data.

| | Low (2%) | Good (4%) | Great (8%) |
| --- | ---: | ---: | ---: |
| Free sign-ups | 10,000 | 10,000 | 10,000 |
| Paying users | 200 | 400 | 800 |
| Gross MRR (60% Pro, 40% Prop, half on yearly) | $8,800 | $17,600 | $35,200 |
| Paddle fees (~6%) | -$530 | -$1,060 | -$2,110 |
| AWS at this size | ~$35 | ~$35 | ~$35 |
| **Net MRR** | **~$8,200** | **~$16,500** | **~$33,000** |

## Launch sequence

- [ ] Now until live Rithmic trading: free public beta with everything unlocked; collect sign-ups
      and usage data.
- [ ] Enforce plan limits through the existing signed entitlement lease; plans are server-side data,
      not a new build.
- [ ] When live trading ships: turn on Free / Pro / Prop and a 14-day full-Prop trial on first
      sign-in, no card.
- [ ] Founder offer for the first buyers: yearly plan locked at 40% off for as long as they stay
      subscribed.
- [ ] Pitch prop firms a bundled or white-label deal (per-account fee or revenue share), like Topstep
      with Quantower.
- [ ] Replace the site's Pro $29 / Elite $59 table (Elite differs only by workspace count) with these
      plans and the competitor comparison required above.

## Sell only what ships

Live trading needs Rithmic onboarding (batch T5), the copier is still open in T2, and order flow is
T3. Charging before live trading works would sell simulated trading only. Keep the beta free, price
plans on the site as "at launch", and start billing when Pro features are real.

## Open questions to settle

- Ask Rithmic whether a conformed platform owes any per-user vendor fee; users normally pay the
  $25/mo connection through their broker or prop firm.
- Lifetime licenses: every competitor sells them at $1,000–2,000. Skip at launch; they trade
  recurring revenue for one-time cash.
