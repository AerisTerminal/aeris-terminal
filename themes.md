# Theme presets

Every Theme Studio theme in one place: the default High Contrast theme and the Normal preset, each with a light and a dark version. Every block has the same tokens, in the same order and with the same comments, as the High Contrast theme.

The surfaces come in three layers: `--surface` for pages and panels, `--surface-secondary` for subtle fills, and `--surface-backdrop` for the page backdrop that shows through the gaps between workspace panels. The backdrop belongs to the native desktop platform: web pages use `--surface` as their page background and never read `--surface-backdrop`.

## How to use

1. Copy the CSS blocks below into one stylesheet, **in the order shown**. High Contrast comes first because it is the fallback when no preset is selected.
2. Pick a theme with `data-theme` on `<html>`. Leave it off for High Contrast.
3. Turn dark mode on with the `dark` class on the same `<html>` element.

```html
<html>                                   <!-- High Contrast Light -->
<html class="dark">                      <!-- High Contrast Dark -->
<html data-theme="normal">               <!-- Normal Light -->
<html data-theme="normal" class="dark">  <!-- Normal Dark -->
```

```js
document.documentElement.dataset.theme = 'normal'; // switch preset
delete document.documentElement.dataset.theme;     // back to High Contrast
document.documentElement.classList.toggle('dark'); // light / dark
```

Components pick the colours up through the variables, for example `background: var(--surface)`, `color: var(--text-default)` or `background: var(--primary)`.

`--text-default` is for the main platform (the trading terminal). `--text-web-default` is for the website and landing pages: in dark mode it is a brighter `#ededed` instead of `#c1c1c1`, and in light mode it matches `--text-default`. Icons use `currentColor`, so an icon takes the same colour as the text it sits in. `--text-secondary` and `--text-muted` are shared by both.

Floating layers (menus, tooltips, popovers) and secondary controls sit on `--surface-secondary` with a 1px `--border-secondary` edge in every theme. Form dialogs and cards sit on `--surface`, with inputs on `--surface-secondary`. Use `--hover-bg-secondary` and `--active-bg-secondary` for hover and active states on `--surface-secondary` (in dark mode `--hover-bg` is flattened against `--surface`, which is darker than `--surface-secondary`).

## Themes

| Theme | `data-theme` | Light | Dark |
| --- | --- | --- | --- |
| [High Contrast (default)](#1-high-contrast-default) | none | High Contrast Light | High Contrast Dark (pure black `#000000`) |
| [Normal](#2-normal) | `normal` | Normal Light | Normal Dark (near-black `#0a0a0a`) |

## 1. High Contrast (default)

The default theme. It applies whenever no `data-theme` is set. Dark mode uses a pure black `#000000` surface with a `#232324` secondary surface and solid `#3c3c3c` / `#404040` borders. Light mode uses a strong `--border-secondary` and hover / active fills. Menus, dialogs and tooltips sit on `--surface-secondary` with a `--border-secondary` edge, like every preset.

```css
:root {
  /* Surfaces */
  --surface: #ffffff;
  --surface-secondary: #f5f6f8;
  --surface-backdrop: #e5e5e5;

  /* Borders */
  --border: #cecece;
  --border-secondary: #cecece;
  --border-subtle: #eeeeee;
  --border-width: 1px;

  /* Text */
  --text-default: #0a0a0a;
  --text-web-default: #0a0a0a;
  --text-secondary: #717171;
  --text-muted: #c2c2c2;
  --text-positive: #0b9981;
  --text-negative: #f23645;
  --text-danger: var(--danger);
  --text-warning: #ff6900;

  /* Interaction states — hover / selected fill, pressed fill, disabled fill. */
  --hover-bg: #ececec;
  --active-bg: #e1e1e1;
  --hover-bg-secondary: #e7e7e7;
  --active-bg-secondary: #dcdcdc;
  --disabled-bg: #f3f3f3;

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
  --button-fill: #161616;
  --button-fill-hover: #2e2e2e;
  --button-fill-active: #454545;
  --button-fill-foreground: #eeeeee;
  --button-fill-subtle: #ececec;

  /* Trade — dedicated buy / sell buttons. Hover and press darken the fill; no shadows. */
  --buy: var(--primary);
  --buy-hover: var(--primary-hover);
  --buy-active: var(--primary-active);
  --buy-disabled: var(--primary-disabled);
  --buy-disabled-foreground: var(--primary-disabled-foreground);
  --buy-foreground: var(--primary-foreground);
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
  --radius-default: 6px;
  --radius-large: 999px;

  /* Chart */
  --bullish: #0b9981;
  --bearish: #f23645;
  /* Big-trade bubble fills; the bubble outline uses bullish / bearish. */
  --buy-bubble: color-mix(in srgb, #0b9981 35%, transparent);
  --sell-bubble: color-mix(in srgb, #f23645 35%, transparent);
}

.dark {
  /* Surfaces */
  --surface: #000000;
  --surface-secondary: #232324;
  --surface-backdrop: #262626;

  /* Borders */
  --border: #3c3c3c;
  --border-secondary: #404040;
  --border-subtle: #121212;
  --border-width: 1px;

  /* Text */
  --text-default: #c1c1c1;
  --text-web-default: #ededed;
  --text-secondary: #898989;
  --text-muted: #575757;
  --text-positive: #0b9981;
  --text-negative: #f23645;
  --text-danger: #ffa2a2;
  --text-warning: #ffb86a;

  /* Interaction states */
  --hover-bg: #1e1e1e;
  --active-bg: #252525;
  --hover-bg-secondary: #3d3d3e;
  --active-bg-secondary: #434344;
  --disabled-bg: #0d0d0d;

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
  --button-fill: #d5d5d5;
  --button-fill-hover: #bfbfbf;
  --button-fill-active: #aaaaaa;
  --button-fill-foreground: #282828;
  --button-fill-subtle: #141414;

  /* Trade — dedicated buy / sell buttons. Hover and press brighten the fill; no shadows. */
  --buy: var(--primary);
  --buy-hover: var(--primary-hover);
  --buy-active: var(--primary-active);
  --buy-disabled: var(--primary-disabled);
  --buy-disabled-foreground: var(--primary-disabled-foreground);
  --buy-foreground: var(--primary-foreground);
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
  --bullish: #0b9981;
  --bearish: #f23645;
  /* Big-trade bubble fills; the bubble outline uses bullish / bearish. */
  --buy-bubble: color-mix(in srgb, #0b9981 35%, transparent);
  --sell-bubble: color-mix(in srgb, #f23645 35%, transparent);
}
```

## 2. Normal

Normal Light uses softer `#d9d9d9` borders (`#f0f0f0` subtle) and lighter `#f2f2f2` / `#ebebeb` hover and active fills (`#f7f7f7` disabled) than High Contrast Light (`#cecece` borders, `#ececec` / `#e1e1e1` fills), keeping borders clearly distinct above hover and active states. Normal Dark uses a near-black `#0a0a0a` surface instead of High Contrast's pure black `#000000`, with a `#292929` secondary surface, a `#262626` backdrop and solid `#404040` / `#4c4c4c` borders.

```css
[data-theme="normal"] {
  /* Surfaces */
  --surface: #ffffff;
  --surface-secondary: #f5f6f8;
  --surface-backdrop: #e5e5e5;

  /* Borders */
  --border: #d9d9d9;
  --border-secondary: #d9d9d9;
  --border-subtle: #f0f0f0;
  --border-width: 1px;

  /* Text */
  --text-default: #0a0a0a;
  --text-web-default: #0a0a0a;
  --text-secondary: #717171;
  --text-muted: #c2c2c2;
  --text-positive: #0b9981;
  --text-negative: #f23645;
  --text-danger: var(--danger);
  --text-warning: #ff6900;

  /* Interaction states — hover / selected fill, pressed fill, disabled fill. */
  --hover-bg: #f2f2f2;
  --active-bg: #ebebeb;
  --hover-bg-secondary: #ededed;
  --active-bg-secondary: #e6e6e6;
  --disabled-bg: #f7f7f7;

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
  --button-fill: #161616;
  --button-fill-hover: #2e2e2e;
  --button-fill-active: #454545;
  --button-fill-foreground: #eeeeee;
  --button-fill-subtle: #f2f2f2;

  /* Trade — dedicated buy / sell buttons. Hover and press darken the fill; no shadows. */
  --buy: var(--primary);
  --buy-hover: var(--primary-hover);
  --buy-active: var(--primary-active);
  --buy-disabled: var(--primary-disabled);
  --buy-disabled-foreground: var(--primary-disabled-foreground);
  --buy-foreground: var(--primary-foreground);
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
  --radius-default: 6px;
  --radius-large: 999px;

  /* Chart */
  --bullish: #0b9981;
  --bearish: #f23645;
  /* Big-trade bubble fills; the bubble outline uses bullish / bearish. */
  --buy-bubble: color-mix(in srgb, #0b9981 35%, transparent);
  --sell-bubble: color-mix(in srgb, #f23645 35%, transparent);
}

[data-theme="normal"].dark {
  /* Surfaces */
  --surface: #0a0a0a;
  --surface-secondary: #292929;
  --surface-backdrop: #262626;

  /* Borders */
  --border: #404040;
  --border-secondary: #4c4c4c;
  --border-subtle: #1b1b1b;
  --border-width: 1px;

  /* Text */
  --text-default: #c1c1c1;
  --text-web-default: #ededed;
  --text-secondary: #8e8e8e;
  --text-muted: #5e5e5e;
  --text-positive: #0b9981;
  --text-negative: #f23645;
  --text-danger: #ffa2a2;
  --text-warning: #ffb86a;

  /* Interaction states */
  --hover-bg: #272727;
  --active-bg: #2e2e2e;
  --hover-bg-secondary: #424242;
  --active-bg-secondary: #484848;
  --disabled-bg: #161616;

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
  --button-fill: #d6d6d6;
  --button-fill-hover: #c2c2c2;
  --button-fill-active: #adadad;
  --button-fill-foreground: #282828;
  --button-fill-subtle: #1d1d1d;

  /* Trade — dedicated buy / sell buttons. Hover and press brighten the fill; no shadows. */
  --buy: var(--primary);
  --buy-hover: var(--primary-hover);
  --buy-active: var(--primary-active);
  --buy-disabled: var(--primary-disabled);
  --buy-disabled-foreground: var(--primary-disabled-foreground);
  --buy-foreground: var(--primary-foreground);
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
  --bullish: #0b9981;
  --bearish: #f23645;
  /* Big-trade bubble fills; the bubble outline uses bullish / bearish. */
  --buy-bubble: color-mix(in srgb, #0b9981 35%, transparent);
  --sell-bubble: color-mix(in srgb, #f23645 35%, transparent);
}
```
