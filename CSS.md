# CSS

The platform theme. This is the exact CSS the Theme System (`C:\Users\devraj\Downloads\Theme_System`)
hands out with its **Copy CSS** button, and it matches the token blocks in
`crates/ui/design_system/platform.css` value for value.

## Rules

- **Always use these tokens. Never hardcode** a color, alpha tint, radius or border width in UI code.
- Colors come from `theme.colors.<token>` (the CSS name with `-` → `_`, e.g. `--book-bid-fill` → `book_bid_fill`).
  Do not derive a shade with `.with_alpha(..)`; if a state has no token, add one here first.
- Radii come from `RadiusToken` (`Sm` = `--radius-small`, `Default`, `Medium`, `Button`, `Full` = `--radius-large`),
  never `rounded(px(N))`.
- Borders use `theme.dimensions.border_width` (`--border-width`), never `border_1()` or `px(1.0)`.
- Use tokens for what they name: fill with its paired text (`book-bid-fill` + `book-bid-text`,
  `negative-subtle` + `text-negative`), `buy-*` / `sell-*` for trade buttons, `buy-bubble` /
  `sell-bubble` (outlined with `bullish` / `bearish`) for big-trade chart bubbles, `surface-overlay`
  for scrims, `hover-bg` / `active-bg` for hover and press. Hover and press change the fill only, no
  shadows.
- Tabs and segmented controls use `TabList` + `Tab` (`apps/desktop/src/native_ui/tab.rs`), the native
  Theme System `.ui-tabs` / `.ui-tab`: a `surface-raised` track with no border; unselected tabs have no fill
  and `text-interactive` text, and hover / press change the text only (`text-hover` / `text-active`, never
  `hover-bg`); the selected tab is raised onto `surface` with a `border` outline and `text-active`. Callers
  add only layout; never restyle a tab's colors, borders or radius.
- To change a value, change it in the Theme System, then update this file and `platform.css` together
  (see "Design system coordination" in `AGENTS.md`). The three must stay identical.

## Theme CSS

```css
:root {
  /* Surfaces */
  --surface: #ffffff;
  --surface-secondary: #fafafa;
  --surface-subtle: #fafafa;
  --surface-raised: #f0f0f0;
  --surface-overlay: color-mix(in srgb, #000000 50%, transparent);
  --surface-inverse: #222222;

  /* Borders */
  --border: #e5e5e5;
  --border-secondary: var(--border);
  --border-subtle: #f0f0f0;
  --border-softer: #f5f5f5;
  --border-strong: #c2c2c2;
  --border-inverse: #222222;
  --border-width: 0.5px;

  /* Text */
  --text-primary: #222222;
  --text-default: #404040;
  --text-secondary: #646465;
  --text-muted: #c2c2c2;
  --text-positive: var(--positive);
  --text-negative: var(--negative);
  --text-danger: var(--danger);
  --text-warning: #ff6900;
  /* Text on interactive items (buttons, tabs, links). Keep these values in
     step with --icon / --icon-active: normal text = normal icon colour,
     hover/active text = active icon colour. They are separate tokens on
     purpose, never point one at the other. */
  --text-interactive: #646465;
  --text-hover: #404040;
  --text-active: #404040;

  /* Interaction states — hover / selected fill, pressed fill, disabled fill. */
  --hover-bg: #f0f0f0;
  --active-bg: #e5e5e5;
  --disabled-bg: #e5e5e5;

  /* Icons — normal and hovered/pressed/selected */
  --icon: #646465;
  --icon-active: #404040;

  /* Status — gains / losses, success / failure. Shared with the main platform. */
  --positive: #089981;
  --positive-subtle: #dcf5f0;
  --negative: #f7525f;
  --negative-subtle: #ffe2e2;
  --warning: #ff6900;
  --warning-subtle: #ffedd4;
  --indigo: #615fff;
  --indigo-subtle: #e0e7ff;
  --purple: #ad46ff;
  --purple-subtle: #f3e8ff;

  /* Actions */
  --primary: #0091ff;
  --primary-hover: #0077fa;
  --primary-active: #0050b2;
  --primary-disabled: #b7d9f8;
  --primary-disabled-foreground: #5eb0ef;
  --primary-ring: #cee7fe;
  --primary-subtle: #e1f0ff;
  --primary-foreground: #ffffff;
  --danger: #f7525f;
  --danger-hover: #e5404d;
  --danger-active: #c9303c;
  --danger-disabled: #ffc9c9;
  --danger-disabled-foreground: #ffa2a2;
  --danger-ring: #ffc9c9;
  --danger-foreground: #ffffff;

  /* Buttons — neutral default button */
  --button-fill: #333333;
  --button-fill-hover: #404040;
  --button-fill-active: #222222;
  --button-fill-foreground: #ffffff;
  --button-fill-subtle: #f5f5f5;

  /* Trade — dedicated buy / sell buttons. Hover and press darken the fill; no shadows. */
  --buy: #089981;
  --buy-hover: #07876f;
  --buy-active: #056f5c;
  --buy-disabled: #b3e3da;
  --buy-disabled-foreground: #5cbfae;
  --buy-ring: #b3e3da;
  --buy-foreground: #ffffff;
  --sell: #f7525f;
  --sell-hover: #e5404d;
  --sell-active: #c9303c;
  --sell-disabled: #ffc9c9;
  --sell-disabled-foreground: #ffa2a2;
  --sell-ring: #ffc9c9;
  --sell-foreground: #ffffff;

  /* Order book — depth-bar fills and the price text drawn over them (text passes 4.5:1 on its fill). */
  --book-bid-fill: #e0f3ef;
  --book-bid-text: #067a66;
  --book-ask-fill: #fdeef0;
  --book-ask-text: #d91a2b;

  /* Focus */
  --ring: color-mix(in srgb, #c2c2c2 50%, transparent);
  --ring-primary: var(--primary);

  /* Radius */
  --radius-default: 8px;
  --radius-medium: 12px;
  --radius-small: 4px;
  --radius-large: 999px;
  --radius-button: 6px;

  /* Shadows */
  --shadow-1: 0 0 1px 0 color-mix(in srgb, #000000 20%, transparent), 0 1px 2px 0 color-mix(in srgb, #000000 5%, transparent), 0 1px 1px 0 color-mix(in srgb, #000000 1%, transparent);
  --shadow-2: 0 1px 6px 0 color-mix(in srgb, #000000 10%, transparent);
  --shadow-3: 0 2.75px 5.5px 0 color-mix(in srgb, #000000 10%, transparent);
  --shadow-dialog: 0 4px 6px -4px color-mix(in srgb, #101828 10%, transparent), 0 10px 15px -3px color-mix(in srgb, #000000 10%, transparent);

  /* Chart */
  --bullish: #089981;
  --bearish: #f7525f;
  /* Big-trade bubble fills; the bubble outline uses bullish / bearish. */
  --buy-bubble: color-mix(in srgb, #089981 35%, transparent);
  --sell-bubble: color-mix(in srgb, #f7525f 35%, transparent);
}

.dark {
  /* Surfaces */
  --surface: #1f1f1f;
  --surface-secondary: #222222;
  --surface-subtle: #2b2b2b;
  --surface-raised: #333333;
  --surface-overlay: color-mix(in srgb, #000000 60%, transparent);
  --surface-inverse: #ffffff;

  /* Borders */
  --border: #333333;
  --border-secondary: var(--border);
  --border-subtle: #333333;
  --border-softer: #2b2b2b;
  --border-strong: #404040;
  --border-inverse: #ffffff;
  --border-width: 0.5px;

  /* Text */
  --text-primary: #f5f5f5;
  --text-default: #f0f0f0;
  --text-secondary: #c2c2c2;
  --text-muted: #808080;
  --text-positive: var(--positive);
  --text-negative: var(--negative);
  --text-danger: #ffa2a2;
  --text-warning: #ffb86a;
  --text-interactive: #c2c2c2;
  --text-hover: #f0f0f0;
  --text-active: #f0f0f0;

  /* Interaction states */
  --hover-bg: #333333;
  --active-bg: #404040;
  --disabled-bg: #404040;

  /* Icons */
  --icon: #c2c2c2;
  --icon-active: #f0f0f0;

  /* Status — gains / losses, success / failure. Shared with the main platform. */
  --positive: #089981;
  --positive-subtle: #193c37;
  --negative: #f7525f;
  --negative-subtle: #532b2e;
  --warning: #ff6900;
  --warning-subtle: #7e2a0c;
  --indigo: #615fff;
  --indigo-subtle: #312c85;
  --purple: #ad46ff;
  --purple-subtle: #59168b;

  /* Actions */
  --primary: #0091ff;
  --primary-hover: #0077fa;
  --primary-active: #0050b2;
  --primary-disabled: #5eb0ef;
  --primary-disabled-foreground: #b7d9f8;
  --primary-ring: #5eb0ef;
  --primary-subtle: #0050b2;
  --primary-foreground: #ffffff;
  --danger: #f7525f;
  --danger-hover: #f96a75;
  --danger-active: #fb838c;
  --danger-disabled: #9f0712;
  --danger-disabled-foreground: #ff6467;
  --danger-ring: #c10007;
  --danger-foreground: #ffffff;

  /* Buttons — neutral default button */
  --button-fill: #f5f5f5;
  --button-fill-hover: #e0e0e0;
  --button-fill-active: #d4d4d4;
  --button-fill-foreground: #404040;
  --button-fill-subtle: #222222;

  /* Trade — dedicated buy / sell buttons. Hover and press brighten the fill; no shadows. */
  --buy: #089981;
  --buy-hover: #0aad92;
  --buy-active: #0bc0a2;
  --buy-disabled: #0f4a41;
  --buy-disabled-foreground: #3f9e8e;
  --buy-ring: #0a6b5b;
  --buy-foreground: #ffffff;
  --sell: #f7525f;
  --sell-hover: #f96a75;
  --sell-active: #fb838c;
  --sell-disabled: #5c2328;
  --sell-disabled-foreground: #c7535c;
  --sell-ring: #a3303a;
  --sell-foreground: #ffffff;

  /* Order book — depth-bar fills and the price text drawn over them (text passes 4.5:1 on its fill). */
  --book-bid-fill: #16332e;
  --book-bid-text: #22c3a6;
  --book-ask-fill: #3a2124;
  --book-ask-text: #ff6b76;

  /* Focus */
  --ring: color-mix(in srgb, #404040 50%, transparent);
  --ring-primary: var(--primary);

  /* Chart */
  --bullish: #089981;
  --bearish: #f7525f;
  /* Big-trade bubble fills; the bubble outline uses bullish / bearish. */
  --buy-bubble: color-mix(in srgb, #089981 35%, transparent);
  --sell-bubble: color-mix(in srgb, #f7525f 35%, transparent);
}
```
