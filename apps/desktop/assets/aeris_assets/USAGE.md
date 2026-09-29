# Brand asset usage: Aeris

App name **Aeris**, app id **`aeris`** (used in desktop file names). Which logo file goes where, and the rules that keep it from rendering cropped, blurry or invisible.
**Agents: read this before placing any logo or icon.** If a case isn't covered here, ask, don't guess.

## Source files

These three SVGs are the masters. Never edit, recolour, crop or re-export them by hand.

| File | What it is | Canvas |
| --- | --- | --- |
| `Logo.svg` | **Main logo.** The full app-icon lockup: dark `#262626` rounded tile, highlight gradient, border ring, drop shadow, inner light arrow tile. | 54 × 54 (tile is 48 × 48 at x=3, the rest is shadow room) |
| `Logomark-dark.svg` | **Logomark, dark.** Flat `#0A0A0A` rounded square with the arrow **cut out** (transparent). | 40 × 48 (mark is the 40 × 40 square at y=4) |
| `Logomark-white.svg` | **Logomark, white.** Same shape in white at 84 % opacity, arrow cut out. | 40 × 48 (same) |

The arrow in both logomarks is a hole. Whatever is behind the mark shows through it. That's fine in UI, where you control the surface. Favicons can't rely on it (see below).

## What to use where

| Context | Use | Never use |
| --- | --- | --- |
| Browser tab / favicon | `icons/favicon.svg` + `icons/favicon.ico` | `Logo.svg`: its shadow and border ring turn to mush at 16 px |
| Google search result icon | Same favicon files (Google reads the page's `<link rel="icon">`) | `Logo.svg`: Google crops icons to a **circle** and slices through the tile's padding and border (this happened before) |
| In-app UI on a **light** surface (header, nav, sign-in) | `Logomark-dark.svg` | `Logomark-white.svg` (invisible on light) |
| In-app UI on a **dark** surface | `Logomark-white.svg` | `Logomark-dark.svg` (invisible on dark) |
| UI that follows the theme | Both, toggled: `<img src="Logomark-dark.svg" class="dark:hidden">` + `<img src="Logomark-white.svg" class="hidden dark:block">` | A single mark for both themes |
| Desktop app icon: Windows | `icons/desktop/windows/aeris.ico` (+ `windows/store/*` for Microsoft Store / MSIX) | Logomark files: desktop is always the main logo |
| Desktop app icon: macOS | `icons/desktop/macos/aeris.icns` or `macos/AppIcon.iconset/` (Xcode) | `desktop/png/*`: not on Apple's grid, looks oversized in the Dock |
| Desktop app icon: Linux | `icons/desktop/linux/hicolor/**` + `linux/aeris.desktop` | Logomark files |
| iOS home screen | `icons/apple-touch-icon.png` | `Logo.svg` or any PNG with transparent corners (iOS fills them black) |
| Android / PWA install | `icons/site.webmanifest` → `icon-192.png`, `icon-512.png`, `icon-maskable-512.png` | A non-maskable icon under `purpose: "maskable"` (it gets cropped) |
| Marketing, splash, about screen, large hero | `Logo.svg` | Logomark rasters from `icons/favicon/` (hinted for small sizes only) |
| Social / Open Graph image | Not generated yet. Ask for a 1200 × 630 composition. | Any file in `icons/` stretched to fit |

## Generated icons (`icons/`)

Everything in `icons/` is **generated** by `npm run icons` (`scripts/build-icons.mjs`). Don't edit these files. Change the source SVG or the script and regenerate.

### Browser tab and search: hinted logomark

| File | Contents |
| --- | --- |
| `favicon.svg` | Adaptive. Light browser chrome and crawlers: dark square + solid white arrow. Dark chrome: white logomark as drawn (`prefers-color-scheme`). Drawn on a 16 px grid so edges land on whole pixels. |
| `favicon.ico` | Dark-surface square + solid white arrow at 16, 20, 24, 32, 40, 48, 64: every size browsers and Windows pick at 100 / 125 / 150 / 200 % scaling. 32-bit BMP entries (widest compatibility). |
| `favicon-16x16.png`, `favicon-32x32.png` | Dark square + white arrow, for tools that want PNG. |
| `favicon/favicon-{16…128}.png` | Dark square + white arrow at every common size. |
| `favicon/favicon-white-{16…128}.png` | White logomark as drawn (arrow cut out) at every common size. Dark surfaces only. |

These are **not** plain scaled copies of the SVG. At small sizes the source geometry lands between pixels (e.g. 4.4 px) and blurs, so the script rebuilds the mark per size with the square edges and arrow bars snapped to whole pixels. Rules:

- **Pick the exact pixel size: display size × device pixel ratio.** A 16 px icon on a 125 % screen needs `favicon-20.png`. Use `srcset` with 1x / 1.25x / 1.5x / 2x candidates. Never let the browser stretch a smaller file up.
- **The favicon's square is the dark-theme surface colour** (`--surface` in `.dark`, currently `#1f1f1f`), not the source mark's near-black `#0A0A0A`. The build script reads it from `src/app/globals.css`, so change the token and re-run `npm run icons`. Never hard-code a different dark.
- **The favicon's arrow is filled solid white, not cut out.** DuckDuckGo, dark search themes and dark browser tabs draw favicons with nothing behind them. A cut-out arrow would show the dark page and the icon would vanish. On light backgrounds the filled version looks identical to the source mark. Never regenerate the favicon from `Logomark-dark.svg` directly.
- One ICO can't switch themes, so the ICO and PNGs are the dark-square version: readable on light and dark. Only `favicon.svg` switches (Safari ignores it).

### Home screen and PWA: main logo

| File | Contents |
| --- | --- |
| `apple-touch-icon.png` | 180 × 180, opaque. `#262626` tile fills the square, inner arrow tile centred at Logo's proportions. iOS rounds the corners itself. |
| `icon-192.png`, `icon-512.png` | `Logo.svg` as drawn, transparent corners (`purpose: "any"`). |
| `icon-maskable-512.png` | 512 × 512, opaque, inner arrow tile shrunk into Android's safe zone so circle / squircle / rounded-square masks never cut it. |
| `site.webmanifest` | Lists the three icons above. |

### Desktop: main logo only

Desktop icons always use `Logo.svg`, never a logomark. The logomarks are for websites, tabs and search.

| Path | For | Contents |
| --- | --- | --- |
| `desktop/windows/aeris.ico` | Windows exe, installer, shortcuts, taskbar, Alt-Tab, title bar | 16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 128, 256: every size Windows picks at 100–400 % scaling. 32-bit BMP entries, PNG at 256. |
| `desktop/windows/store/*.png` | Microsoft Store, MSIX, WinUI | `Square30x30Logo` … `Square310x310Logo`, `StoreLogo` (names match Tauri and Visual Studio). |
| `desktop/macos/aeris.icns` | macOS app bundle (`Contents/Resources`), Electron, Tauri | ic07–ic14 (Apple's `iconutil` chunk types), 32–1024. **Framed to Apple's Big Sur+ grid:** tile 824 × 824 centred in 1024. |
| `desktop/macos/AppIcon.iconset/` | Xcode asset catalog, `iconutil -c icns` | `icon_16x16.png` … `icon_512x512@2x.png`, same framing. |
| `desktop/macos/aeris-1024.png` | macOS master / App Store | 1024 × 1024, same framing. |
| `desktop/linux/hicolor/{16…512}x{…}/apps/aeris.png` | Linux desktops (GNOME, KDE, …), Flatpak, Snap, AppImage | freedesktop hicolor layout: 16, 22, 24, 32, 48, 64, 96, 128, 256, 512. Copy to `/usr/share/icons/hicolor/`. The file name is the app id, `aeris`. |
| `desktop/linux/hicolor/scalable/apps/aeris.svg` | Linux, any size | `Logo.svg`. |
| `desktop/linux/aeris.desktop` | Linux launcher / app menu | `Name=Aeris`, `Icon=aeris` (resolves through hicolor), `Exec=aeris`. Change `Exec=` to your binary's path or name. Install to `/usr/share/applications/`. |
| `desktop/png/{16…1024}x{…}.png` | Electron, Tauri, custom setups | Logo as drawn: 16, 20, 22, 24, 30, 32, 36, 40, 44, 48, 64, 96, 128, 256, 512, 1024. |

Windows and Linux use `Logo.svg` as drawn. macOS icons are re-framed because Logo's tile fills about 89 % of its canvas and Apple's grid is about 80 %. Never use the Windows/Linux files on macOS or the reverse.

Not generated yet (they need a non-square layout, so ask first): Windows `Wide310x150Logo` and splash screen, macOS DMG background, Linux AppStream screenshots.

`Logo.svg` is gradients and soft shadows, so it can't be pixel-snapped. At 16–24 px it's inherently soft; from 32 px up it's sharp.

## HTML head

This site gets these automatically from `src/app/favicon.ico`, `src/app/icon.svg` and `src/app/apple-icon.png` (Next.js file conventions). For any other site:

```html
<link rel="icon" href="/brand-assets/icons/favicon.ico" sizes="16x16 20x20 24x24 32x32 40x40 48x48 64x64">
<link rel="icon" href="/brand-assets/icons/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/brand-assets/icons/apple-touch-icon.png">
<link rel="manifest" href="/brand-assets/icons/site.webmanifest">
```

## Don'ts

- Don't use `Logo.svg` below 32 px, as a favicon, or anywhere it can be cropped to a circle.
- Don't put a logomark on a surface of the same tone (dark on dark, white on light).
- Don't fill the logomark's arrow hole with a colour or add a background tile behind it in UI. The favicon files are the one exception: their arrow is solid white by design.
- Don't add padding around the favicon to "make room". The mark fills its square on purpose, so a circle crop lands inside the rounded square and shows a clean dark circle with the white arrow.
- Don't recolour, rotate, outline, add shadows to, or stretch any mark.
- Don't scale a raster up. Pick a larger file, or use the SVG.
- Don't hand-edit anything in `icons/`. Regenerate with `npm run icons`.
