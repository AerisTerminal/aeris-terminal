# Axiusflow Platform Design System

**Status:** Governing platform contract
**Applies to:** GPUI desktop terminal, Origin Charts integration, and Rust/WASM surfaces

## 1. Purpose

Axiusflow has one typed design system for application surfaces, controls, text,
and financial charts. The design system defines shared visual primitives. Each
component independently owns its layout, behavior, accessibility state, and
animation.

The Rust theme is the runtime source of truth. Rendering code consumes resolved
typed values and never performs token-name lookup while painting.

Page-specific composition and behavior belong to the components that implement
them rather than this shared contract.

## 2. Naming contract

All Axiusflow-owned identifiers use `snake_case`, including theme fields,
manifest entries, recipes, serialized names, debug output, and generated
accessors.

The canonical identifier is stored once. The checked CSS manifest prefixes that
identifier with `--` without changing its spelling:


| Canonical identifier | Generated CSS custom property |
| -------------------- | ----------------------------- |
| `chart_candle_up`    | `--chart_candle_up`           |
| `app_header_height`  | `--app_header_height`         |
| `radius_default`     | `--radius_default`            |


CSS custom properties permit underscores. No name transformation or separately
maintained export spelling is allowed.
Rust type declarations retain the language-standard `UpperCamelCase` form.

## 3. Theme model

`AxiusflowTheme` is one fully resolved theme revision:

```text
axiusflow_theme
├── mode
├── colors
├── dimensions
└── radii
```

- `mode` is `Light` or `Dark`.
- `colors` contains resolved sRGB values with alpha.
- `dimensions` contains logical-pixel measurements and their source values.
- `radii` contains exactly the three platform radius choices.
- A theme switch replaces the complete theme value atomically.
- GPUI, Origin Charts, exports, screenshots, and web surfaces consume the same
  resolved revision.

Tokens retain their canonical identifier, source expression, and resolved
runtime value. Source expressions document the approved color or measurement;
components consume only the typed resolved value.

## 4. Color system



### 4.1 Core colors

The neutral hierarchy follows Twenty's Display P3 gray scale. Axiusflow keeps
its own semantic names and its existing product and financial colors.

| Identifier             | Light                                      | Dark                                         |
| ---------------------- | ------------------------------------------ | -------------------------------------------- |
| `background`           | `color(display-p3 1 1 1)`                  | `color(display-p3 0.09 0.09 0.09)`           |
| `foreground`           | `color(display-p3 0.2 0.2 0.2)`            | `color(display-p3 0.922 0.922 0.922)`        |
| `surface_primary`      | `background`                               | `background`                                 |
| `surface_secondary`    | `color(display-p3 0.988 0.988 0.988)`      | `color(display-p3 0.106 0.106 0.106)`        |
| `surface_tertiary`     | `color(display-p3 0.945 0.945 0.945)`      | `color(display-p3 0.114 0.114 0.114)`        |
| `surface_quaternary`   | `color(display-p3 0.922 0.922 0.922)`      | `color(display-p3 0.133 0.133 0.133)`        |
| `card`, `popover`      | `surface_secondary`                        | `surface_secondary`                          |
| `secondary`, `muted`   | `surface_tertiary`                         | `surface_tertiary`                           |
| `accent`               | `surface_tertiary`                         | `surface_tertiary`                           |
| `border`               | `color(display-p3 0.945 0.945 0.945)`      | `color(display-p3 0.114 0.114 0.114)`        |
| `input`                | `color(display-p3 0.922 0.922 0.922)`      | `color(display-p3 0.133 0.133 0.133)`        |
| `input_surface`        | `color(display-p3 0.988 0.988 0.988)`      | `color(display-p3 0.106 0.106 0.106)`        |
| `primary`              | `#3e63dd`                                  | `#3e63dd`                                    |
| `primary_foreground`   | `oklch(0.97 0.014 254.604)`                | same                                         |
| `ring`                 | `oklch(0.708 0 0)`                         | `oklch(0.556 0 0)`                           |

### 4.2 Text, icon, and interaction colors

| Identifier                      | Light                                 | Dark                                  |
| ------------------------------- | ------------------------------------- | ------------------------------------- |
| `text_secondary`, `icon_color`  | `color(display-p3 0.4 0.4 0.4)`       | `color(display-p3 0.702 0.702 0.702)` |
| `text_muted`, `muted_foreground` | `color(display-p3 0.6 0.6 0.6)`       | `color(display-p3 0.506 0.506 0.506)` |
| `text_placeholder`              | `color(display-p3 0.702 0.702 0.702)` | `color(display-p3 0.4 0.4 0.4)`       |
| `text_unavailable`              | `color(display-p3 0.8 0.8 0.8)`       | `color(display-p3 0.298 0.298 0.298)` |
| `interactive_neutral_hover_bg`  | black at 3.9%                         | white at 5.9%                         |
| `interactive_neutral_active_bg` | black at 7.8%                         | white at 10.2%                        |

Neutral hover and active backgrounds are alpha overlays. This makes the same
state readable on the root, card, popover, and input surfaces without a white
hover disappearing on a white surface. Their foreground tokens resolve to
`foreground`.


`primary` is the sole product accent. Components may derive interaction states
from it, but may not create another product-accent family.

### 4.3 Trading and chart colors


| Identifier          | Light                        | Dark                  |
| ------------------- | ---------------------------- | --------------------- |
| `profit`            | `oklch(0.683 0.151 160.997)` | same                  |
| `loss`              | `oklch(0.674 0.215 18.124)`  | same                  |
| `warning`           | `oklch(0.769 0.165 70.08)`   | same                  |
| `info`              | `oklch(0.555 0.245 266.681)` | same                  |
| `feature`           | `oklch(0.541 0.247 293.009)` | same                  |
| `chart_candle_up`   | `profit`                     | `profit`              |
| `chart_candle_down` | `loss`                       | `loss`                |
| `chart_volume_up`   | `profit` at 34% alpha        | `profit` at 32% alpha |
| `chart_volume_down` | `loss` at 30% alpha          | `loss` at 28% alpha   |
| `chart_axis_text`   | `#0a0a0a`                    | `var(--foreground)`   |
| `chart_crosshair`   | `#9598a1`                    | `#2e2e2e`             |


The neutral chart palette is:


| Identifier | Value              |
| ---------- | ------------------ |
| `chart_1`  | `oklch(0.87 0 0)`  |
| `chart_2`  | `oklch(0.556 0 0)` |
| `chart_3`  | `oklch(0.439 0 0)` |
| `chart_4`  | `oklch(0.371 0 0)` |
| `chart_5`  | `oklch(0.269 0 0)` |


Origin Charts receives resolved values from the active `AxiusflowTheme`:


| Origin role                      | Theme field                                      |
| -------------------------------- | ------------------------------------------------ |
| background                       | `background` or the component's declared surface |
| axis text                        | `chart_axis_text`                                |
| grid and border                  | `border`                                         |
| candle up/down                   | `chart_candle_up`, `chart_candle_down`           |
| volume up/down                   | `chart_volume_up`, `chart_volume_down`           |
| positive/negative annotation     | `profit`, `loss`                                 |
| informational/warning annotation | `info`, `warning`                                |
| strategy or feature annotation   | `feature`                                        |
| additional series                | `chart_1` through `chart_5`                      |


Origin renderers do not maintain an independent color theme.

### 4.4 Color resolution

- OKLCH conversion is centralized and follows CSS Color 4 conversion semantics.
- Converted channels are gamut-clamped before they become GPUI or Origin values.
- Alpha is stored independently from RGB channels.
- Components do not embed approximate replacements for canonical colors.
- Literal colors are allowed only in the theme definition and its tests.



## 5. Radius system

The platform exposes exactly three radii:


| Identifier       | Logical pixels | Use                                                 |
| ---------------- | -------------- | --------------------------------------------------- |
| `radius_sm`      | 4              | Explicitly dense or small controls                  |
| `radius_default` | 6              | Normal controls, panels, and surfaces               |
| `radius_full`    | 999            | Pills, circles, avatars, and fully rounded elements |


No radius alias, component-local radius token, semantic synonym, or additional
radius is valid. Components use `radius_default` unless the element clearly
meets the `radius_sm` or `radius_full` rule.

## 6. Typography

The application uses Inter Variable for interface text and numeric data.

- Bundle and register the variable font before the first application frame.
- Support weights 100 through 900 in normal style.
- Use tabular numerals for prices, quantities, percentages, timestamps, and
other aligned numeric values.
- Use the platform-native sans fallback only if font registration fails.
- Components own their text hierarchy; the shared theme has one interface font
family.



## 7. Shared dimensions

`app_header_height` is 44 logical pixels, sourced as `2.75rem` for surfaces
that export the theme to a web runtime. One rem resolves to 16 logical pixels.
OS display scaling is applied after logical layout.

Component-specific widths, heights, grids, breakpoints, and runtime
measurements remain with the owning component. They are not global tokens.

## 8. Component rules

- Start standard native controls and layout primitives from GPUI Component.
- Use typed theme fields instead of copying literal colors or measurements.
- Keep component state, focus behavior, keyboard interaction, animation, and
responsive layout inside the component implementation.
- Use `background` and `foreground` for the application root.
- Use `surface_primary` for the application header and sidebars.
- Use `card`/`card_foreground` or `popover`/`popover_foreground` for their named
surfaces.
- Use `input_surface` for input backgrounds and `input` for input borders.
- Use the dedicated interaction tokens for neutral hover and active states.
- Use `text_unavailable` for disabled content and `icon_color` for neutral icons.
- Use `border` and `ring` for their semantic roles.
- Disabled controls must not respond to activation.
- Financial charts use Origin Charts and the mapping in section 4.3.

If a new component needs a visual value that is not shared, keep the value
component-local. Promote it into the design system only after multiple
components share the same semantic need.

## 9. Implementation contract

The design-system crate provides:

- `ThemeMode` for light/dark selection;
- `ThemeColor` for resolved sRGB and alpha;
- `ThemeColors` for shared semantic and chart colors;
- `ThemeDimensions` for genuinely shared measurements;
- `RadiusToken` for the closed radius set;
- canonical token metadata and `axiusflow_theme.css` for CSS consumers.

The CSS manifest emits `--{canonical_identifier}`. It does not own a second
naming table and does not transform underscores. A parity test requires every
Rust light and dark source expression to appear in the manifest, preventing the
native and CSS contracts from drifting. Debug inspectors and serialized
inventories show the canonical identifier unchanged.

## 10. Validation

Design-system changes must verify:

1. light and dark token resolution;
2. canonical identifiers and exact optional CSS export names;
3. OKLCH conversion and alpha handling;
4. the 4/6/999 radius values and closed radius set;
5. `app_header_height` resolution to 44 logical pixels;
6. Origin chart mapping for both modes;
7. representative GPUI rendering in both modes;
8. absence of component-local copies of shared constants.

Golden rendering tests should use stable content, logical dimensions, theme,
and scale factor. A deliberate visual change updates the governing token and
its evidence together.

## 11. Extension rule

A shared token may be added only when it has:

1. a semantic purpose not represented by an existing token;
2. one canonical `snake_case` identifier;
3. light and dark behavior where applicable;
4. a source expression and resolved value;
5. tests covering resolution and consumers.

The radius set is closed and cannot be extended. Page-specific design,
component geometry, and one-off animation contracts do not belong in this
document.
