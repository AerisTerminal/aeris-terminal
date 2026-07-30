# Axiusflow Design System and 1:1 UI Parity Contract

**Status:** Governing visual contract
**Applies to:** GPUI desktop terminal, Origin Charts GPUI adapter, Rust/WASM client, and future Axiusflow surfaces
**Reference source:** Legacy React platform `src/styles/globals.css` supplied on 2026-07-31
**Parity objective:** 1:1 visual and behavioral parity unless an intentional, reviewed design change is recorded

## 1. Purpose

The existing Axiusflow React stylesheet defines the product's visual language. The new Rust/GPUI platform must not reinterpret that language or replace it with approximate defaults. Colors, surfaces, text, interaction states, radii, typography, trading semantics, chart colors, layout measurements, and named UI recipes are preserved.

GPUI does not execute CSS, Tailwind directives, DOM selectors, pseudo-elements, or browser media/container queries. Those implementation mechanisms are removed from the native runtime, but the visual behavior they expressed remains part of this contract and must be implemented through GPUI theme values, component state, layout logic, and paint operations.

This document has two responsibilities:

1. Preserve the approved legacy token vocabulary and visual values, subject to the three-radius normalization and primary-only accent policy.
2. Define how each legacy CSS behavior is represented in GPUI without creating a second design system.

The rule is:

> Remove framework syntax, not design intent.

---

## 2. Source-of-truth policy

### 2.1 Canonical names and snake_case

All Axiusflow-owned identifiers use `snake_case`: files, directories, crates, modules, functions, variables, fields, generated accessors, recipe identifiers, serialized keys, database objects, API fields, configuration keys, and event fields.

Legacy CSS custom-property keys remain byte-for-byte compatible with the old platform, including their original hyphen or underscore style. They are compatibility keys, not a precedent for new naming. Examples:

- `--background`
- `--card-foreground`
- `--interactive_neutral_hover_bg`
- `--icon_color`
- `--chart-candle-up`
- `--token-text-primary`

Their Rust accessors use snake_case, while the canonical legacy key remains attached as metadata. For example, the accessor `chart_candle_up` maps to `--chart-candle-up`. Exporters, parity tests, generated web CSS, debug inspectors, and documentation retain the exact legacy key.

Rust type and trait declarations may use the language-standard `UpperCamelCase` form when required by Rust linting, but every module, function, field, serialized representation, and public wire identifier remains snake_case. External library and provider names are not rewritten.

### 2.2 Canonical values and the three-radius rule

Color, typography, spacing, dimension, and motion expressions preserve their original values rather than approximate conversions. The implementation pipeline stores both:

- the original expression, such as `oklch(0.683 0.151 160.997)`; and
- the resolved GPUI value used for rendering.

Resolution must follow CSS Color 4 semantics, including OKLCH conversion, sRGB gamut handling, alpha, relative colors, and `color-mix` behavior. A hand-picked visually similar RGB color is not parity.

Radius is the sole approved normalization of the supplied compatibility layer. The platform defines and exposes exactly three radius tokens:

- `--radius-sm`: 6 px, reserved for explicitly dense or small controls;
- `--radius-default`: 8 px, required for normal components, controls, panels, and surfaces;
- `--radius-full`: 999 px/pill, required for pills, circles, avatars, status dots, and every fully rounded element.

No legacy radius name remains available. No alias, semantic synonym, component-local radius token, fourth radius token, or fifth radius token may be introduced. Components must use `--radius-default` unless they satisfy the narrow small-control or fully-rounded rules above.

`--primary` is the sole product accent color. CTAs and any treatment requiring a product-specific accent use `--primary` directly or an interaction-state token derived from it, such as `--token-cta-bg-hover`. No separate numbered accent palette or dedicated accent-gradient token family may be introduced.

### 2.3 Theme inheritance

The light theme is the base `:root` value set. The dark theme overrides only values that the legacy `.dark` selector overrode. Non-overridden values inherit from the base exactly as they did in CSS.

### 2.4 No component-local copies

Components request semantic tokens. They must not copy literal colors such as `#3e63dd`, `#070a0f`, or `#16191f` into component code. Origin Charts, GPUI components, browser components, marketing surfaces, screenshots, and exports all resolve from the same token registry.

### 2.5 Generated implementation artifacts

When implementation begins, the preferred source pipeline is:

```text
design/tokens manifest
        │
        ├── generated Rust `theme_tokens`
        ├── generated web CSS custom properties
        ├── generated Origin chart theme options
        └── generated token inventory used by parity tests
```

Generated files must not be edited independently. Until that generator exists, the canonical reference values in section 4 govern implementation.

---

## 3. What is preserved and what is removed

### 3.1 Preserved without renaming

The following are preserved:

- all base light and dark tokens;
- all semantic `--color-*` aliases;
- all CTA, marketing compatibility, text, and surface tokens;
- all profit, loss, warning, information, feature, candle, volume, and chart-axis tokens;
- all interaction-neutral and icon tokens;
- exactly the three approved radius tokens (`--radius-sm`, `--radius-default`, and `--radius-full`), with every legacy radius token removed;
- all dashboard and component measurements;
- all named utility/recipe semantics;
- all hover, active, selected, highlighted, focus, fullscreen, scrollbar, and responsive behaviors;
- all animation names, durations, delays, easing, and reduced-motion behavior;
- all component-scoped runtime token names.

### 3.2 Removed only as execution syntax

These legacy mechanisms are not copied into GPUI code:

- `@import "tailwindcss"`;
- `@import "tw-animate-css"`;
- `@import "shadcn/tailwind.css"`;
- Tailwind `@theme`, `@utility`, and `@apply` directives;
- `@custom-variant dark`;
- React/DOM `data-*` selector wiring;
- HTML element selectors such as `html`, `body`, `button`, and `input`;
- CSS pseudo-elements and browser scrollbar selectors;
- CSS `:hover`, `:focus`, `:focus-visible`, and attribute selectors as implementation mechanisms;
- CSS container/media queries as implementation mechanisms;
- Next.js font-loading assumptions and public-directory URLs;
- browser units and APIs that have direct native equivalents.

Their appearance and behavior are mapped to GPUI in sections 7 through 11.

---

## 4. Canonical framework-neutral token stylesheet

This block defines the approved global token contract. It intentionally contains no Tailwind imports, directives, utility generation, DOM component rules, React-specific selectors, or superseded accent token families.

```css
:root {
  /* Typography */
  --font-sans: "Inter Variable", ui-sans-serif, system-ui, sans-serif;
  --font-heading: var(--font-sans);
  --font-display: var(--font-season-mix);

  /* Semantic aliases formerly registered through Tailwind @theme. */
  --color-background: var(--background);
  --color-foreground: var(--foreground);
  --color-card: var(--card);
  --color-card-foreground: var(--card-foreground);
  --color-popover: var(--popover);
  --color-popover-foreground: var(--popover-foreground);
  --color-primary: var(--primary);
  --color-primary-foreground: var(--primary-foreground);
  --color-secondary: var(--secondary);
  --color-secondary-foreground: var(--secondary-foreground);
  --color-muted: var(--muted);
  --color-muted-foreground: var(--muted-foreground);
  --color-accent: var(--accent);
  --color-accent-foreground: var(--accent-foreground);
  --color-destructive: var(--destructive);
  --color-border: var(--border);
  --color-input: var(--input);
  --color-ring: var(--ring);
  --color-chart-1: var(--chart-1);
  --color-chart-2: var(--chart-2);
  --color-chart-3: var(--chart-3);
  --color-chart-4: var(--chart-4);
  --color-chart-5: var(--chart-5);
  --color-profit: var(--profit);
  --color-loss: var(--loss);
  --color-warning: var(--warning);
  --color-info: var(--info);
  --color-feature: var(--feature);
  --color-chart-candle-up: var(--chart-candle-up);
  --color-chart-candle-down: var(--chart-candle-down);
  --color-chart-volume-up: var(--chart-volume-up);
  --color-chart-volume-down: var(--chart-volume-down);

  /* Radius system: these are the only radius tokens. */
  --radius-sm: 6px;
  --radius-default: 8px;
  --radius-full: 999px;

  /* Core light theme */
  --background: oklch(1 0 0);
  --foreground: oklch(0.145 0 0);
  --card: oklch(1 0 0);
  --card-foreground: oklch(0.145 0 0);
  --popover: oklch(1 0 0);
  --popover-foreground: oklch(0.145 0 0);
  --primary: #3e63dd;
  --primary-foreground: oklch(0.97 0.014 254.604);
  --secondary: oklch(0.967 0.001 286.375);
  --secondary-foreground: oklch(0.21 0.006 285.885);
  --muted: oklch(0.97 0 0);
  --muted-foreground: oklch(0.556 0 0);
  --accent: oklch(0.97 0 0);
  --accent-foreground: oklch(0.205 0 0);
  --destructive: oklch(0.577 0.245 27.325);
  --destructive-foreground: oklch(0.985 0 0);
  --border: #f5f5f5;
  --input: #f5f5f5;
  --ring: oklch(0.708 0 0);
  --chart-1: oklch(0.87 0 0);
  --chart-2: oklch(0.556 0 0);
  --chart-3: oklch(0.439 0 0);
  --chart-4: oklch(0.371 0 0);
  --chart-5: oklch(0.269 0 0);

  /* Interaction and chrome */
  --interactive_neutral_hover_bg: var(--accent);
  --interactive_neutral_hover_fg: var(--foreground);
  --interactive_neutral_active_bg: var(--muted);
  --interactive_neutral_active_fg: var(--foreground);
  --icon_color: var(--muted-foreground);
  --dashboard_header_height: 2.75rem;

  /* Trading semantics */
  --profit: oklch(0.683 0.151 160.997);
  --loss: oklch(0.674 0.215 18.124);
  --warning: oklch(0.769 0.165 70.08);
  --info: oklch(0.555 0.245 266.681);
  --feature: oklch(0.541 0.247 293.009);
  --chart-candle-up: var(--profit);
  --chart-candle-down: var(--loss);
  --chart-volume-up: oklch(from var(--profit) l c h / 34%);
  --chart-volume-down: oklch(from var(--loss) l c h / 30%);
  --chart-axis-text: #0a0a0a;

  /* Marketing compatibility tokens retained as platform aliases. */
  --token-bg-page: #ffffff;
  --token-surface: #ffffff;
  --token-surface-muted: #ffffff;
  --token-text-primary: #333333;
  --token-text-secondary: #7b7b7b;
  --token-text-on-dark: #ffffff;
  --token-text-on-light: #0a0a0a;
  --token-cta-bg: var(--primary);
  --token-cta-bg-hover: oklch(from var(--primary) calc(l - 0.06) c h);
  --token-nav-hover: rgba(0, 0, 0, 0.06);
  --token-market-up: var(--profit);
  --token-market-down: var(--loss);
  --token-chart-crosshair: rgba(0, 0, 0, 0.16);
  --token-header-cta-bg: var(--token-cta-bg);
  --token-header-cta-bg-hover: var(--token-cta-bg-hover);
  --token-header-cta-text: #ffffff;
  --token-hero-cta-text: #ffffff;
}

.dark {
  /* Core dark theme */
  --background: oklch(0.145 0 0);
  --foreground: oklch(0.985 0 0);
  --card: #070a0f;
  --card-foreground: oklch(0.985 0 0);
  --popover: #070a0f;
  --popover-foreground: oklch(0.985 0 0);
  --primary: #3e63dd;
  --primary-foreground: oklch(0.97 0.014 254.604);
  --secondary: oklch(0.274 0.006 286.033);
  --secondary-foreground: oklch(0.985 0 0);
  --muted: oklch(0.269 0 0);
  --muted-foreground: oklch(0.708 0 0);
  --accent: oklch(0.269 0 0);
  --accent-foreground: oklch(0.985 0 0);
  --destructive: oklch(0.704 0.191 22.216);
  --destructive-foreground: oklch(0.985 0 0);
  --border: #16191f;
  --input: #16191f;
  --ring: oklch(0.556 0 0);
  --chart-1: oklch(0.87 0 0);
  --chart-2: oklch(0.556 0 0);
  --chart-3: oklch(0.439 0 0);
  --chart-4: oklch(0.371 0 0);
  --chart-5: oklch(0.269 0 0);

  /* Interaction and chrome */
  --interactive_neutral_hover_bg: var(--accent);
  --interactive_neutral_hover_fg: var(--foreground);
  --interactive_neutral_active_bg: oklch(0.269 0 0);
  --interactive_neutral_active_fg: var(--foreground);
  --icon_color: var(--muted-foreground);

  /* Trading semantics */
  --profit: oklch(0.683 0.151 160.997);
  --loss: oklch(0.674 0.215 18.124);
  --warning: oklch(0.769 0.165 70.08);
  --info: oklch(0.555 0.245 266.681);
  --feature: oklch(0.541 0.247 293.009);
  --chart-candle-up: var(--profit);
  --chart-candle-down: var(--loss);
  --chart-volume-up: oklch(from var(--profit) l c h / 32%);
  --chart-volume-down: oklch(from var(--loss) l c h / 28%);
  --chart-axis-text: #ffffff;

  /* Marketing compatibility overrides */
  --token-bg-page: var(--card);
  --token-surface: var(--card);
  --token-surface-muted: var(--card);
  --token-text-primary: #e5e5e5;
  --token-text-secondary: #a1a1a1;
  --token-text-on-dark: #ffffff;
  --token-text-on-light: #0a0a0a;
  --token-cta-bg: var(--primary);
  --token-cta-bg-hover: oklch(from var(--primary) calc(l + 0.06) c h);
  --token-nav-hover: rgba(255, 255, 255, 0.08);
  --token-chart-crosshair: rgba(255, 255, 255, 0.18);
  --token-header-cta-bg: var(--token-cta-bg);
  --token-header-cta-bg-hover: var(--token-cta-bg-hover);
  --token-header-cta-text: #ffffff;
  --token-hero-cta-text: #ffffff;
}
```

### 4.1 External and component-scoped tokens

These names appeared in the legacy stylesheet but were not global theme values. They are still preserved:

| Token | Owner | Contract |
|---|---|---|
| `--font-season-mix` | marketing/display font registration | External font-family value used by `--font-display`; must be supplied before display typography is enabled. |
| `--dashboard-kpi-surface-border` | dashboard KPI grid | Resolves to `var(--border)` in light and dark themes. |
| `--accordion-panel-height` | accordion instance | Runtime measured content height used by open/close animation. |

A runtime token does not become a global color simply to make implementation easier. GPUI passes these values through component state.

---

## 5. Required GPUI token model

The native implementation uses a typed theme, not string lookup during every paint. It must still expose the exact canonical names for testing and export.

Conceptual model:

```text
axiusflow_theme
├── mode: Light | Dark
├── typography
│   ├── --font-sans
│   ├── --font-heading
│   └── --font-display
├── colors
│   ├── core surfaces and foregrounds
│   ├── semantic aliases
│   ├── trading/chart colors
│   ├── primary accent and CTA colors
│   └── marketing compatibility colors
├── radii
├── dimensions
└── canonical token manifest
    └── exact name → source expression → resolved value
```

Required value types:

- `color_token`: source CSS expression plus resolved linear/sRGB/GPUI color.
- `length_token`: pixel/rem source plus resolved device-independent pixels.
- `font_token`: ordered font-family stack and resolved GPUI font family.
- `radius_token`: device-independent pixels or full/pill sentinel.
- `gradient_token`: ordered stops with interpolation space.
- `alias_token`: reference to another canonical token, resolved with cycle detection.

Theme switching is an atomic application-level state change. Every window, component, Origin chart, popover, overlay, and cached paint resource must observe the same theme revision.

### 5.1 GPUI Component foundation

GPUI Component is Axiusflow's mandatory base component library for the native desktop interface. Standard controls and layout primitives—including docking, virtualized tables, forms, fields, buttons, tabs, menus, dialogs, popovers, themes, text, and common layouts—must start from GPUI Component rather than from parallel custom implementations.

Axiusflow wraps and themes GPUI Component through `axiusflow_theme`, the canonical tokens in this document, and snake_case style recipes. Library defaults may not create a second visual system or bypass the three-radius rule. Custom GPUI components are permitted only when GPUI Component lacks required trading-specific behavior, performance, or accessibility; those components must consume the same token and interaction contracts.

Origin Charts remains the sole financial chart engine. GPUI Component supplies the surrounding application UI but does not replace Origin for financial visualization.

---

## 6. Color and measurement parity rules

### 6.1 OKLCH

The GPUI token generator/resolver must implement CSS Color 4 OKLCH conversion. Conversion is centralized and tested against browser-computed reference values. Components may not embed converted approximations.

Relative colors must preserve the base lightness, chroma, and hue while changing only the specified channel. This applies to:

- `--chart-volume-up`;
- `--chart-volume-down`;
- `--token-cta-bg-hover`.

### 6.2 Alpha

Alpha percentages and `rgba()` values are preserved exactly. Compositing tests must use the same background token as the legacy reference.

### 6.3 Rem conversion

The legacy root size is treated as 16 CSS pixels for parity:

- `2.75rem` = 44 device-independent pixels;
- `0.875rem` = 14 pixels;
- `1.25rem` = 20 pixels;
- `0.75rem` = 12 pixels;
- `1rem` = 16 pixels;
- `1.5rem` = 24 pixels.

OS display scaling is applied after these logical measurements.

### 6.4 Radius

The radius system contains exactly three tokens and no aliases:

- `--radius-sm`: 6 px; use only for explicitly dense or small controls.
- `--radius-default`: 8 px; use for every normal component, control, panel, surface, scrollbar thumb, and other non-circular element.
- `--radius-full`: 999 px/pill behavior; use for pills, circles, avatars, status dots, and every fully rounded element.

No other radius custom property, Rust token, recipe token, semantic synonym, or component-local alias is valid. Components cannot introduce literal radius values or new radius names. When no narrow exception applies, `--radius-default` is mandatory.

---

## 7. Typography parity

### 7.1 Inter Variable

The native application bundles the same Inter Variable font bytes used by the legacy platform and registers them with GPUI at startup. Required range:

- weight 100 through 900;
- normal style;
- fallback to the native platform sans stack only if font registration fails.

The old `@font-face` URL is removed because GPUI does not load from `../../public/fonts`. The font asset and family contract remain.

### 7.2 Display font

`--font-display` remains bound to `--font-season-mix`. The display font is not silently replaced. Until the exact font asset is available, features requiring it remain behind an implementation gate or use an explicitly documented temporary fallback that cannot pass parity acceptance.

### 7.3 Named text recipes

| Legacy recipe | GPUI contract |
|---|---|
| `font-sans` | family `--font-sans` |
| `font-display` | family `--font-display`; letter spacing `-0.15px` |
| `font_numeric` | tabular numeral feature enabled |
| `text_label_md` | 14 px size, 20 px line height, weight 500 |
| `text_label_lg` | 16 px size, 24 px line height, weight 600 |
| `text_paragraph_sm` | 14 px size, 20 px line height |
| `text_paragraph_xs` | 12 px size, 16 px line height |

The root text system enables the equivalent of `rlig` and `calt` where the GPUI text stack supports those OpenType features.

---

## 8. Semantic style recipes

Legacy utility/class names remain canonical recipe identifiers even though GPUI does not use CSS classes.

| Recipe | Exact visual contract |
|---|---|
| `surface_card` | minimum width/height zero; clipped overflow; 1 px `--border`; `--radius-default`; `--card` background |
| `surface_border_b` | 1 px bottom border using `--border` |
| `hover_neutral` | 150 ms background/color transition; hover/highlight/selected uses neutral hover BG/FG tokens |
| `state_active` | active BG/FG tokens; 1 px transparent border |
| `radius_sm` | applies `--radius-sm`; only for explicitly dense or small controls |
| `radius_default` | applies `--radius-default`; standard recipe for normal components |
| `radius_full` | applies `--radius-full`; only for pills, circles, avatars, status dots, and fully rounded elements |
| `scrollbar_hide` | scrollbar occupies no visible paint and does not affect content layout |
| `font_numeric` | tabular numbers |
| `bg_diagonal_stripes` | repeating -45° pattern, 3 px transparent + 3 px stripe; 4% black light, 3% white dark |
| `af-page-bg` | `--token-bg-page` background |
| `af-surface-bg` | `--token-surface` background |
| `af-surface-muted-bg` | `--token-surface-muted` background |
| `af-text-primary` | `--token-text-primary` foreground |
| `af-text-secondary` | `--token-text-secondary` foreground |
| `af-text-on-dark` | `--token-text-on-dark` foreground |
| `af-text-on-light` | `--token-text-on-light` foreground |
| `af-bg-text-primary` | `--token-text-primary` background |
| `af-border-text-primary` | `--token-text-primary` border |
| `af-nav-hover` | hover background `--token-nav-hover` |
| `af-header-cta` | header CTA background/text and theme-specific hover tokens |
| `af-hero-cta` | hero CTA background/text and theme-specific hover tokens |

Recipes are implemented as Rust functions/traits over GPUI elements. They receive the active `axiusflow_theme`; they do not read a global string map during paint.

---

## 9. Component behavior parity

### Application root and global defaults

The GPUI application root preserves the legacy base contract:

- application background uses `--background`;
- default foreground uses `--foreground`;
- default font uses `--font-sans`;
- `rlig` and `calt` font features are enabled where supported;
- a component border that requests the default border color resolves to `--border`;
- a component using the standard focus ring resolves to `--ring` at 50% alpha;
- pointer cursor behavior follows the enabled/disabled control state;
- light/dark theme state is established before the first frame.

The old universal selector and `@apply border-border outline-ring/50` are not copied, but these defaults remain part of the component styling contract.

### 9.1 Dashboard grid widget

Canonical names:

- `dashboard_grid_widget`
- `dashboard_widget_title`
- `dashboard_widget_actions`
- `dashboard_chart_frame`

Required behavior:

- container and direct surface descendants allow minimum width zero;
- grid widget is a flex container;
- direct child fills width and height and flexes `1 1 auto`;
- direct child minimum height is 350 px;
- button-like actions do not wrap their text;
- title truncates with ellipsis on one line and uses line-height 1.2;
- actions wrap, align center, and use an 8 px gap;
- chart frame clips overflow and allows width zero;
- chart slot fills height and permits minimum height zero.

In GPUI these are component layout constraints. No `.dashboard_grid_widget` selector is executed.

### 9.2 Dashboard KPI grid

Canonical names:

- `dashboard_kpi_container`
- `dashboard_kpi_static_grid`
- `dashboard_kpi_static_item`
- `--dashboard-kpi-surface-border`

Required behavior:

| Available container width | Columns | Border behavior |
|---|---:|---|
| below 520 px | 1 | bottom separators; last item has none |
| 520–899 px | 2 | right separator except every second; final two have no bottom separator |
| 900–1179 px | 4 | right separator except every fourth; final four have no bottom separator |
| 1180 px and above | 8 | no bottom separators; right separator except every eighth |

Additional requirements:

- each item child has minimum height 128 px;
- grid clips overflow and uses layout/paint containment semantics;
- border token resolves to `--border` in both themes.

GPUI evaluates these breakpoints against the component's allocated width during layout. They are container breakpoints, not global window breakpoints.

### 9.3 Symbol menu

Canonical names:

- `symbol_menu_surface`
- `symbol_menu_overlay`
- `symbol_menu_list`

Required behavior:

- width and maximum width are the smaller of 80% of window width and 896 px;
- maximum height is the smaller of 72% of window height and 704 px;
- no shadow;
- transparent overlay with no blur or backdrop effect;
- list maximum height is the smaller of 48% of window height and 480 px.

GPUI modal configuration owns this geometry. Legacy `!important` declarations are unnecessary because there is no competing Tailwind modal rule.

### 9.4 Terminal focus behavior

Canonical component scopes:

- `chart-viewport`
- `terminal-order-book`
- `terminal-side-rail`
- `terminal-side-rail-separator`
- `terminal-watchlist`

These surfaces remain keyboard-focusable but do not paint browser-style outline, outline offset, or box shadow. Their active/keyboard state uses Axiusflow semantic tokens such as `state_active`.

Accessibility focus is not removed globally. Components that suppressed legacy focus chrome must expose selection/focus through their intended internal state and accessibility metadata. Dialogs, form fields, and controls that relied on normal focus indication retain an appropriate token-driven focus treatment.

### 9.5 Fullscreen terminal

Legacy fullscreen state was prepainted through `html[data-terminal-fullscreen='1']`. GPUI replaces this with state applied before the first window frame:

- hide app shell header;
- hide terminal toolbar;
- terminal panel occupies the complete window content bounds;
- no maximum width;
- no border or radius;
- fullscreen layer has precedence equivalent to legacy z-index 80;
- no one-frame flash of non-fullscreen chrome.

Persisted state must be loaded before first paint.

### 9.6 Scrollbars

Visible scrollbars preserve:

- 5 px width and height;
- track using `--card` with a 1 px `--border` leading edge;
- thumb using `--muted-foreground`;
- thumb uses `--radius-default`;
- 1 px `--border` thumb border;
- normal visual opacity 25%; hover opacity 45%;
- thin-scrollbar semantics.

`scrollbar_hide` suppresses the scrollbar for the specific component while preserving scrolling.

### 9.7 Numeric input

Legacy number-input spinner controls are absent in the GPUI numeric field. Increment/decrement behavior is supplied through explicit Axiusflow controls or keyboard bindings rather than native browser spinner chrome.

### 9.8 Pointer behavior

Enabled buttons and button-role controls use the pointer cursor. Disabled controls retain the disabled cursor and do not respond to activation.

### 9.9 Rounded marketing controls

The old marketing-specific radius distinction is removed. Marketing anchors, buttons, and ordinary controls use `--radius-default`, exactly like other normal components. Pills, circles, avatars, status dots, and other fully rounded elements use `--radius-full`. An explicitly dense or small control may use `--radius-sm`. No marketing or terminal-specific radius alias exists.

---

## 10. Origin Charts token integration

Origin Charts is not allowed to carry a visually independent theme. `origin_render_gpui` and the browser Origin host consume Axiusflow tokens.

Required mapping:

| Origin role | Axiusflow token |
|---|---|
| chart background | `--background` or explicit chart surface chosen by component contract |
| axis text | `--chart-axis-text` |
| default UI text | `--foreground` |
| grid/border base | `--border` |
| candle up | `--chart-candle-up` |
| candle down | `--chart-candle-down` |
| volume up | `--chart-volume-up` |
| volume down | `--chart-volume-down` |
| positive annotation | `--profit` |
| negative annotation | `--loss` |
| informational annotation | `--info` |
| warning annotation | `--warning` |
| feature/strategy annotation | `--feature` |
| palette series | `--chart-1` through `--chart-5` |
| marketing chart crosshair, where used | `--token-chart-crosshair` |

Theme changes update Origin through its options/theme contract and trigger the correct invalidation level. Render backends do not hardcode alternate colors.

The `--chart-volume-up` and `--chart-volume-down` alpha difference between light and dark is intentional and must be preserved. Axis text is `#0a0a0a` in light and `#ffffff` in dark.

---

## 11. Motion and interaction timing

### 11.1 Neutral transition

`hover_neutral` transitions background and foreground over 150 ms with CSS `ease` semantics.

### 11.2 Thinking dots

Canonical names:

- `thinking_dot`
- `terminal-thinking-dot`

Contract:

- dot size 6 px by 6 px;
- `--radius-full` so each dot is fully round;
- current foreground color;
- 1.2 second infinite ease-in-out cycle;
- second dot delayed 150 ms;
- third dot delayed 300 ms;
- at 0%, 80%, and 100%: opacity 0.35 and scale 0.75;
- at 40%: opacity 1 and scale 1.

### 11.3 Generic shimmer

Canonical name: `shimmer`.

- background position starts at `200% 0`;
- ends at `-200% 0`;
- consuming component defines duration and repetition if the legacy component did so externally.

### 11.4 Hero heading shimmer

Canonical names:

- `hero-heading-shimmer`
- `hero-text-shimmer`

Contract:

- 105° gradient;
- primary text through 42%; shimmer at 50%; primary text resumes at 58%;
- background size 220% by 100%;
- animation duration 4.2 seconds, ease-in-out, infinite;
- light shimmer mixes `--primary` at 55% with white;
- dark shimmer mixes `--primary` at 65% with white;
- text is painted with the gradient, not a foreground fill.

When the operating system requests reduced motion, animation is disabled and text renders as `--token-text-primary`.

### 11.5 Accordion

Canonical names:

- `accordion-down`
- `accordion-up`
- `--accordion-panel-height`

Open transitions from height zero/opacity zero to measured panel height/opacity one. Close reverses it. The component owns the exact duration because the supplied global stylesheet defined keyframes but not a duration.

### 11.6 Selection

Text selection background is a 45% sRGB mix of `--primary` and transparent. Native text components that support selection use the resolved equivalent.

---

## 12. React/Tailwind-to-GPUI translation ledger

Nothing in this table authorizes a visual change.

| Legacy mechanism | Why it is not copied | GPUI replacement |
|---|---|---|
| Tailwind/shadcn imports | no CSS framework in native UI | GPUI Component as the mandatory base library, wrapped by the Axiusflow design-system crate |
| `@theme inline` | Tailwind token registration | typed `axiusflow_theme` plus canonical-name manifest |
| `@utility` | Tailwind utility generation | named Rust style recipes with the same semantic identifier |
| `@apply` | Tailwind declaration expansion | direct GPUI style composition |
| `@custom-variant dark` | selector-based browser theme | application `theme_mode` and atomic theme revision |
| `@font-face` URL | browser asset loader | bundled font bytes registered with GPUI |
| `.dark` selector | DOM ancestor state | active theme selection |
| `:hover`/highlight attributes | browser selector state | GPUI hover/selection state |
| `:focus`/`:focus-visible` | browser focus painting | GPUI focus handle and component focus recipe |
| `data-*` selectors | React-to-CSS wiring | typed component/entity state |
| `@container` | browser container query | child layout based on allocated GPUI width |
| `@media (prefers-reduced-motion)` | browser media query | OS/application accessibility preference |
| `vw`, `vh`, `svh` | browser viewport units | GPUI window/content bounds |
| `overflow: hidden` | CSS clipping | GPUI clipping/scissor |
| `text-overflow: ellipsis` | browser text layout | constrained GPUI text with truncation |
| `font-variant-numeric` | CSS font feature | GPUI/OpenType tabular-number feature |
| `background-clip: text` | browser paint feature | glyph mask/gradient text paint |
| `::-webkit-scrollbar` | browser pseudo-elements | themed GPUI scrollbar component |
| number-input pseudo-elements | browser-native controls | Axiusflow numeric-input component |
| `!important` | cascade conflict resolution | single component owner; no competing cascade |
| z-index | DOM stacking context | GPUI overlay/layer order |
| CSS keyframes | browser animation engine | GPUI animation scheduler and repaint requests |
| `color-mix`/relative OKLCH | browser color resolver | centralized CSS-compatible token resolver |

### 12.1 Web client rule

The Rust/WASM web client may consume generated CSS containing these variables. It must not restore legacy React/Tailwind component CSS as a second source of truth. Native and web outputs originate from the same token manifest and recipe specification.

---

## 13. Component ownership and extension rules

### 13.1 New tokens

A new token requires:

1. a semantic purpose not already represented;
2. a canonical name consistent with the existing vocabulary of the owning category;
3. light and dark behavior;
4. source expression and resolved-value tests;
5. GPUI and web generation;
6. an update to this document or its future generated inventory.

Radius is excluded from this extension process: no new radius token or radius alias may be proposed. All radius use must resolve directly to `--radius-sm`, `--radius-default`, or `--radius-full` according to section 6.4.

A component-specific measurement remains component-scoped unless multiple components genuinely share the semantic concept.

### 13.2 New component styles

New components compose semantic tokens and named recipes. They do not introduce local one-off colors that duplicate existing tokens.

### 13.3 Intentional visual changes

A deliberate redesign requires an architecture/design decision containing:

- old token or recipe;
- proposed replacement;
- affected surfaces;
- light/dark screenshots;
- Origin chart impact;
- accessibility impact;
- migration plan;
- parity baseline update.

An implementation limitation is not by itself permission to change the design.

---

## 14. Parity validation

### 14.1 Token manifest test

A checked-in expected manifest lists every canonical token from section 4 plus component-scoped tokens from section 4.1. CI fails if a token is removed, renamed, gains a different source expression, or is unimplemented in either theme without an approved decision.

### 14.2 Color resolution test

A browser reference fixture computes every token. The Rust resolver computes the same values. Tests compare RGBA outputs within a strict rounding tolerance and specifically cover:

- all OKLCH colors;
- relative alpha volume colors;
- light and dark CTA hover relative colors;
- sRGB color mixes;
- alias chains;
- dark inheritance.

### 14.3 Typography test

Validate:

- exact font files and hashes;
- family selection;
- weight mapping;
- text size, line height, and letter spacing;
- tabular numerals;
- representative glyph measurements.

### 14.4 Component golden matrix

Capture the legacy platform and GPUI implementation at matching logical sizes, scale factors, content, and theme for:

- base surfaces and popovers;
- buttons and states;
- dashboard widgets;
- KPI layouts at each breakpoint;
- symbol menu;
- watchlist and order book;
- fullscreen terminal;
- scrollbars;
- typography recipes;
- marketing/CTA recipes when implemented;
- Origin charts and annotations.

Run at DPR/scale factors 1, 1.25, 1.5, 2, and 3 where supported.

### 14.5 Interaction parity

Automated interaction tests cover hover, active, selected, keyboard focus, fullscreen first paint, theme switching, resize breakpoints, reduced motion, disabled controls, and scrollbar behavior.

### 14.6 Acceptance rule

A component is not considered migrated merely because it uses the same token names. It must match token resolution, geometry, typography, state transitions, and relevant interaction behavior.

---

## 15. Complete preservation checklist

### Global token families

- [ ] Font aliases: sans, heading, display
- [ ] Core background and foreground
- [ ] Card and popover surfaces
- [ ] Primary, secondary, muted, accent, and destructive semantics
- [ ] Border, input, and ring
- [ ] Chart palette 1–5
- [ ] Interaction hover/active and icon tokens
- [ ] Dashboard header height
- [ ] Profit, loss, warning, info, and feature
- [ ] Candle, volume, and axis chart colors
- [ ] Marketing page/surface/text tokens
- [ ] CTA and navigation tokens
- [ ] Market and crosshair compatibility tokens
- [ ] Exactly three radius tokens: `--radius-sm`, `--radius-default`, and `--radius-full`; no aliases
- [ ] Component-scoped KPI, accordion, and display-font inputs

### Named recipes and behaviors

- [ ] Surfaces and borders
- [ ] Neutral hover and active states
- [ ] Typography and numeric styles
- [ ] Diagonal stripes
- [ ] Dashboard widget layout
- [ ] Responsive KPI grid
- [ ] Symbol menu
- [ ] Terminal focus behavior
- [ ] Fullscreen first paint
- [ ] Scrollbars and hidden-scrollbar behavior
- [ ] Numeric input behavior
- [ ] Marketing compatibility recipes
- [ ] Thinking dots, shimmer, hero shimmer, accordion, and reduced motion
- [ ] Origin chart theme mapping

The implementation checklist starts unchecked because this document defines the contract; checks are completed only by working GPUI/web code and parity evidence.

---

## 16. Final rule

The legacy stylesheet is not copied blindly into the Rust repository, and it is not discarded. Its tokens and design behavior become the Axiusflow cross-platform design contract.

The GPUI platform may use different rendering primitives, layout APIs, event models, and animation machinery, but users should not be able to identify the implementation language by looking at the interface. Unless a visual change is explicitly approved, the new platform must look and behave like the existing Axiusflow platform.
