# CSS

The platform theme. This is the exact CSS the Theme System (`C:\Users\devraj\Downloads\design_system`)
hands out with its **Copy CSS** button, and it matches the token blocks in
`crates/ui/design_system/platform.css` value for value.

## Rules

- **Always use these tokens. Never hardcode** a color, alpha tint, radius or border width in UI code.
- Colors come from `theme.colors.<token>` (the CSS name with `-` → `_`, e.g. `--book-bid-fill` → `book_bid_fill`).
  Do not derive a shade with `.with_alpha(..)`; if a state has no token, add one here first.
- Radii come from `RadiusToken` (`Sm` = `--radius-small`, `Default`, `Medium`, `Compact`, `Full` = `--radius-large`),
  never `rounded(px(N))`.
- Borders are 1px and use `theme.dimensions.border_width` (`--border-width`), never `border_1()` or `px(1.0)`.
- Neutral text has three roles: `text-default` for content and labels, `text-secondary` for
  supporting text (AA 4.5:1 on every light and dark surface), and `text-muted` for placeholders and
  disabled text. Interactive items use `text-interactive` / `text-hover` / `text-active`.
- Use tokens for what they name: fill with its paired text (`book-bid-fill` + `book-bid-text`,
  `negative-subtle` + `text-negative`), `buy-*` / `sell-*` for market-order buttons, `buy-subtle-*` / `sell-subtle-*` (with their
  `-foreground` text) for resting-order buttons and long / short position fills, `warning` +
  `warning-foreground` for the flatten action, `buy-bubble` /
  `sell-bubble` (outlined with `bullish` / `bearish`) for big-trade chart bubbles, `hover-bg` for
  hover and `active-bg` for a selected or open state. Press paints nothing of its own. Hover
  changes the fill only, no shadows.
- Dark `surface-secondary` is a translucent tint of `surface`.
  It only appears over an opaque `surface` (the window root) or `surface-popover`; never use it
  as the fill of a layer that floats over other content.
- Floating layers (menus, dialogs, popovers, toasts, hints, floating toolbars) use
  `popover_surface` and `popover_highlight` from `native_ui/theme.rs`: an opaque `surface-popover`
  fill, a 1px `border-popover` outer edge, and a 1px `border-popover-highlight` inner edge
  painted above the content.
- Tooltips (`terminal_ui::tooltip`) use the secondary surface: `surface-secondary` resolved over
  `surface-popover`, a 1px `border-secondary` edge and `text-default`. A one-line
  label is a `radius-large` pill; rich content (label/value rows) uses `radius-medium`. A tooltip
  opens beside its trigger on the caller's side (bottom by default, toward the content for edge
  sidebars), never at the cursor; top and bottom flip when the window edge leaves no room. It fades
  and slides in from the trigger side and fades out when the pointer leaves, presses or scrolls.
- Large workspace panels (the symbol, indicator and chart settings panels, the indicator settings
  dialogs and the header account menu) keep the popover edge and highlight but fill with the main
  `surface` (`surface_panel`, `MenuPanel::on_surface`, or `Dialog::panel_surface`). Their content
  sits on titled, borderless `surface-secondary` group cards: no outlined blocks and no hairlines
  between rows. Size and style settings are dropdowns, not tab groups.
- Nothing inside a `surface-popover` layer rests on plain `surface` in dark mode. Fields and setting
  rows use `field_surface` (`surface`, `surface-secondary` in dark), and buttons use the `Secondary`
  variant.
- Button labels always use the normal platform weight, trade buttons included.
- A fill resolved with `.over(..)` is opaque only over an opaque backdrop. Resolve a translucent
  surface over the `surface` beneath it first (the title bar uses `title_bar_fill`).
- Menus, dialogs and popovers never dim, tint or blur what is behind them: their dismiss scrims
  paint nothing. A popover stays inside the window, flipping to the other side of its trigger
  when there is no room.
- Focus is one style everywhere: the control's 1px border turns `ring-primary` (no
  thicker ring, glow or background change).
- Typography uses the bundled Inter faces (Regular 400, Medium 500, Bold 700) for platform text and
  the bundled Faculty Glyphic Regular face only for brand/display text. The normal platform weight
  (`--font-weight-normal`) is Regular 400; emphasis and strong text use Bold 700. Changing numbers use Inter's
  tabular figures (`tnum`). Geist Mono (Regular, Medium, Bold) is bundled as `--font-mono`
  (`platform_mono_font_family()`) for numbers in data panels only: order book rows, time and sales
  rows, position/order/trade values and screener numeric columns. Headers and labels stay Inter.
  Its 0.600em digits are narrower than Inter's 0.648em tabular digits, so the existing column widths
  hold. There is no HK Grotesk dependency. The CSS
  `--font-sans` and native `platform_font_family()` both resolve to Inter; keep the web mirror in
  sync.
- To change a value, change it in the Theme System, then update this file and `platform.css` together
  (see "Design system coordination" in `AGENTS.md`). The three must stay identical.

## Components

Controls come from `apps/desktop/src/native_ui`, never hand-built divs. Ask for a component and
variant **by name**; the component owns every color, border, radius, hover, press, disabled and
focus state. Callers add only layout (flex, gaps, margins, an explicit width).

**`Button`** (`button.rs`), the Theme System `Button`. Variants:

| Variant | Look | Use for |
| --- | --- | --- |
| `Default` | `button-fill` neutral fill | the main neutral action |
| `Primary` | brand `primary` blue fill | the action that commits a form or dialog (Apply, Create, Save, Set) |
| `Secondary` | `.ui-button-secondary`: `surface-secondary` + `border-secondary`, `text-default`; hover / open lay `hover-bg` / `active-bg` over the fill and keep the border; disabled `disabled-bg` + `text-muted` | controls inside popover layers (chart settings triggers and swatches, the symbol menu's provider switcher, settings dialog actions) |
| `Outline` | `surface` + `border`, `text-default`; hover drops the border and fills `hover-bg` | Cancel, select triggers, steppers, filters on window surfaces |
| `Ghost` | no fill or border, `text-default`; hover fills `hover-bg` | toolbar, header and icon actions |
| `Destructive` | `danger` fill | irreversible actions (Delete, Reset to defaults) |
| `Buy` / `Sell` | `buy` / `sell` fill | trade orders only |

Sizes: `Sm` (24px, `text-xs`), `Default` (28px), `Lg` (32px, `--radius-default`). A button with only
an icon is square. No control paints a press state: hover is the only pointer feedback, and a click
shows itself by changing the control's state (selected, open, text), never by flashing a second fill.
States and modifiers: `selected` (a toggle that is on), `text_toggle` (the on state
changes only the text: `text-interactive` → `text-active`, no fill; hover adds `hover-bg` in both states
and never changes the text; chart-header panel toggles), `open` (a trigger whose menu
is open), `disabled`, `loading`, `round` (circle or pill), `danger_on_hover` (close / delete icons),
`trigger` (label left, caret right), `full_width`. Every button label, trade buttons included, uses the
normal platform weight; there is no bold button. `close_button` is the one close control.

**`MenuPanel`** + **`MenuAnchor`** + **`MenuRow`** (`menu.rs`): every dropdown, context menu, flyout and
select menu. The panel is a popover layer (see Rules). Place a panel `At(point)`, `Anchored { Below | Above, Start | End | Stretch }` from a trigger, or
`InFlow`. Rows: `MenuRow::compact` (menus) and `MenuRow::search_result` (search lists), with
`checked`, `highlighted`, `disabled`, `destructive`, `leading`, `detail`, `trailing`. Rows rest on
`surface-popover`. A `destructive` row
keeps the shared neutral row fills and colours only its label and glyph: `text-danger` at rest,
`danger-hover` while hovered or highlighted, and `danger-disabled` with no hover while disabled.
Pass glyphs through `leading_icon` so the row colours them.

**`Dialog`** + **`ConfirmationDialog`** (`dialog.rs`): every modal, drawn as a popover layer;
`panel_surface` fills an indicator settings dialog with `surface` instead. Sizes `Sm` (420), `Md` (480), `Lg`
(560); `title` / `subtitle` or a custom `header`; `footer_leading` plus `action`s (Cancel first, then the
committing action). `ConfirmationTone::Destructive` uses `Destructive`, `Positive` uses `Primary`.

**`TabList`** + **`Tab`** (`tab.rs`), the Theme System `.ui-tabs` / `.ui-tab`: tabs side by side
(`gap-4`) over a 1px `border` baseline, with no track fill; each tab has no fill, a transparent 2px
underline and `text-interactive` text, and hover / press change the text only (`text-hover` /
`text-active`, never `hover-bg`); the selected tab's underline and text turn `primary`. Use for navigation tabs and one-tap presets such as order quantities;
a single-choice setting in a settings panel or indicator dialog is a dropdown instead. `compact()` on both the
list and its tabs in dense panel headers (bottom panel, side panels);
`TabList::sidebar` + `Tab::sidebar` for vertical section navigation.

**`Switch`** / **`SwitchRow`** (`switch.rs`): every on/off setting, never an "On" / "Off" button.

**`CheckboxRow`** (`checkbox.rs`): an option that refines the setting above it (a legend's values
under its switch), never a setting of its own. A 16px `--radius-small` box: `surface` with a
`border` outline when clear, `primary` with a `primary-foreground` tick when checked; hover
fills `hover-bg` / `primary-hover`; disabled uses `disabled-bg` / `primary-disabled` and `text-muted`.
The whole row toggles, and focus uses the shared ring.

**`Input`** (`input.rs`), the Theme System `Input`: a `border` field on `surface` (`surface-secondary` in
dark), `hover-bg` on hover, the shared focus ring, and a `danger` border with a 3px `danger-ring` halo
while invalid.

## Theme CSS

```css
:root {
  /* Surfaces */
  --surface: #ffffff;
  --surface-secondary: #fafafa;
  /* Floating layers (menus, dialogs, tooltips) */
  --surface-popover: #ffffff;

  /* Borders */
  --border: #0a0a0a33;
  --border-secondary: #0a0a0a1a;
  --border-subtle: #0a0a0a12;
  /* Floating-layer edge: a 1px outer edge plus a 1px inner highlight. */
  --border-popover: var(--border);
  --border-popover-highlight: #ffffffcc;
  --border-width: 1px;

  /* Text */
  --text-default: #0a0a0a;
  --text-secondary: #0a0a0a94;
  --text-muted: #0a0a0a40;
  --text-positive: var(--positive);
  --text-negative: var(--negative);
  --text-danger: var(--danger);
  --text-warning: #ff6900;
  /* Text on interactive items (buttons, tabs, links). Keep these values in
     step with --icon / --icon-active: normal text = normal icon colour,
     hover/active text = active icon colour. They are separate tokens on
     purpose, never point one at the other. */
  --text-interactive: #0a0a0a94;
  --text-hover: #0a0a0a;
  --text-active: #0a0a0a;

  /* Interaction states — hover / selected fill, pressed fill, disabled fill. */
  --hover-bg: #0a0a0a0d;
  --active-bg: #0a0a0a0d;
  --disabled-bg: #0a0a0a0d;

  /* Icons — normal and hovered/pressed/selected */
  --icon: #0a0a0a94;
  --icon-active: #0a0a0a;

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
  --primary: #006EDD;
  --primary-hover: color-mix(in srgb, var(--primary) 80%, transparent);
  --primary-active: #0050b2;
  --primary-disabled: color-mix(in srgb, var(--primary) 30%, var(--surface));
  --primary-disabled-foreground: color-mix(in srgb, var(--primary) 65%, var(--surface));
  --primary-subtle: color-mix(in srgb, var(--primary) 12%, var(--surface));
  --primary-foreground: #ffffff;
  --danger: #f7525f;
  --danger-hover: #e5404d;
  --danger-disabled: #ffc9c9;
  --danger-disabled-foreground: #ffa2a2;
  --danger-ring: #ffc9c9;
  --danger-foreground: #ffffff;
  --warning-hover: #e05c00;
  --warning-foreground: #ffffff;

  /* Buttons — neutral default button */
  --button-fill: #0a0a0af2;
  --button-fill-hover: #0a0a0ada;
  --button-fill-active: #0a0a0ac2;
  --button-fill-foreground: #fafafaf2;
  --button-fill-subtle: #0a0a0a14;

  /* Trade — dedicated buy / sell buttons. Hover and press darken the fill; no shadows. */
  --buy: #089981;
  --buy-hover: #07876f;
  --buy-active: #056f5c;
  --buy-disabled: #b3e3da;
  --buy-disabled-foreground: #5cbfae;
  --buy-foreground: #ffffff;
  --sell: #f7525f;
  --sell-hover: #e5404d;
  --sell-active: #c9303c;
  --sell-disabled: #ffc9c9;
  --sell-disabled-foreground: #ffa2a2;
  --sell-foreground: #ffffff;
  /* Tinted buy / sell for resting order types (limit, stop). */
  --buy-subtle: color-mix(in srgb, var(--buy) 12%, var(--surface));
  --buy-subtle-hover: color-mix(in srgb, var(--buy) 20%, var(--surface));
  --buy-subtle-active: color-mix(in srgb, var(--buy) 28%, var(--surface));
  --buy-subtle-foreground: color-mix(in srgb, var(--buy) 70%, #000000);
  --sell-subtle: color-mix(in srgb, var(--sell) 12%, var(--surface));
  --sell-subtle-hover: color-mix(in srgb, var(--sell) 20%, var(--surface));
  --sell-subtle-active: color-mix(in srgb, var(--sell) 28%, var(--surface));
  --sell-subtle-foreground: color-mix(in srgb, var(--sell) 70%, #000000);

  /* Order book — depth-bar fills, the price text beside them, and the text drawn on top of a fill. */
  --book-bid-fill: color-mix(in srgb, var(--primary) 40%, var(--surface));
  --book-bid-text: var(--primary);
  --book-ask-fill: color-mix(in srgb, #ff2f5e 40%, var(--surface));
  --book-ask-text: color-mix(in srgb, #ff2f5e 75%, #000000);
  --book-fill-text: #000000;

  /* Focus */
  --ring-primary: var(--primary);

  /* Radius */
  --radius-small: 4px;
  --radius-compact: 6px;
  --radius-default: 8px;
  --radius-medium: 14px;
  --radius-large: 999px;

  /* Chart */
  --bullish: var(--positive);
  --bearish: var(--negative);
  /* Big-trade bubble fills; the bubble outline uses bullish / bearish. */
  --buy-bubble: color-mix(in srgb, #089981 35%, transparent);
  --sell-bubble: color-mix(in srgb, #f7525f 35%, transparent);
}

.dark {
  /* Surfaces */
  --surface: #000000;
  --surface-secondary: #fafafa0a;
  --surface-popover: #010101;

  /* Borders */
  --border: #2E2E2E;
  --border-secondary: #fafafa26;
  --border-subtle: #fafafa12;
  --border-popover: #000000cc;
  --border-popover-highlight: #ffffff1a;
  --border-width: 1px;

  /* Text */
  --text-default: #ededed;
  --text-secondary: #fafafa8c;
  --text-muted: #fafafa59;
  --text-positive: var(--positive);
  --text-negative: var(--negative);
  --text-danger: #ffa2a2;
  --text-warning: #ffb86a;
  --text-interactive: #fafafa8c;
  --text-hover: #ededed;
  --text-active: #ededed;

  /* Interaction states */
  --hover-bg: #fafafa13;
  --active-bg: #fafafa26;
  --disabled-bg: #fafafa0d;

  /* Icons */
  --icon: #fafafa8c;
  --icon-active: #ededed;

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
  --primary: #006EDD;
  --primary-hover: color-mix(in srgb, var(--primary) 80%, transparent);
  --primary-active: #0050b2;
  --primary-disabled: color-mix(in srgb, var(--primary) 35%, var(--surface));
  --primary-disabled-foreground: color-mix(in srgb, var(--primary) 65%, white);
  --primary-subtle: color-mix(in srgb, var(--primary) 22%, var(--surface));
  --primary-foreground: #ffffff;
  --danger: #f7525f;
  --danger-hover: #f96a75;
  --danger-disabled: #6b3135;
  --danger-disabled-foreground: #d0646c;
  --danger-ring: #c10007;
  --danger-foreground: #ffffff;
  --warning-hover: #ff7b1f;
  --warning-foreground: #ffffff;

  /* Buttons — neutral default button */
  --button-fill: #fafafad9;
  --button-fill-hover: #fafafac3;
  --button-fill-active: #fafafaad;
  --button-fill-foreground: #0a0a0ad9;
  --button-fill-subtle: #fafafa14;

  /* Trade — dedicated buy / sell buttons. Hover and press brighten the fill; no shadows. */
  --buy: #089981;
  --buy-hover: #0aad92;
  --buy-active: #0bc0a2;
  --buy-disabled: #0f4a41;
  --buy-disabled-foreground: #3f9e8e;
  --buy-foreground: #ffffff;
  --sell: #f7525f;
  --sell-hover: #f96a75;
  --sell-active: #fb838c;
  --sell-disabled: #6b3135;
  --sell-disabled-foreground: #ae8e90;
  --sell-foreground: #ffffff;
  /* Tinted buy / sell for resting order types (limit, stop). */
  --buy-subtle: color-mix(in srgb, var(--buy) 18%, var(--surface));
  --buy-subtle-hover: color-mix(in srgb, var(--buy) 26%, var(--surface));
  --buy-subtle-active: color-mix(in srgb, var(--buy) 34%, var(--surface));
  --buy-subtle-foreground: color-mix(in srgb, var(--buy) 70%, #ffffff);
  --sell-subtle: color-mix(in srgb, var(--sell) 18%, var(--surface));
  --sell-subtle-hover: color-mix(in srgb, var(--sell) 26%, var(--surface));
  --sell-subtle-active: color-mix(in srgb, var(--sell) 34%, var(--surface));
  --sell-subtle-foreground: color-mix(in srgb, var(--sell) 70%, #ffffff);

  /* Order book — depth-bar fills, the price text beside them, and the text drawn on top of a fill. */
  --book-bid-fill: color-mix(in srgb, var(--primary) 35%, var(--surface));
  --book-bid-text: color-mix(in srgb, var(--primary) 80%, #ffffff);
  --book-ask-fill: color-mix(in srgb, #ff2f5e 35%, var(--surface));
  --book-ask-text: color-mix(in srgb, #ff2f5e 85%, #ffffff);
  --book-fill-text: #ffffff;

  /* Focus */
  --ring-primary: var(--primary);

  /* Chart */
  --bullish: var(--positive);
  --bearish: var(--negative);
  /* Big-trade bubble fills; the bubble outline uses bullish / bearish. */
  --buy-bubble: color-mix(in srgb, #089981 35%, transparent);
  --sell-bubble: color-mix(in srgb, #f7525f 35%, transparent);
}
```
