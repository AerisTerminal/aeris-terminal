# Aeris desktop brand assets

This directory contains only assets that are useful to the Aeris desktop application and its desktop packaging.

## Source SVGs

- `logo.svg` — main Aeris logo. Use for the desktop header brand lockup, About, splash/large-brand surfaces, and desktop app-icon generation.
- `logomark-dark.svg` — dark in-app logomark for light surfaces when a compact mark is explicitly required.
- `logomark-white.svg` — white in-app logomark for dark surfaces when a compact mark is explicitly required.

All source filenames are lowercase. Do not recolour, crop, rotate, outline, or hand-edit the SVG artwork.

## Desktop packaging

Only desktop packaging assets are retained under `icons/desktop/`:

- `windows/aeris.ico` and `windows/store/*` — Windows executable / installer / Store artwork.
- `macos/aeris.icns`, `macos/aeris-1024.png`, and `macos/AppIcon.iconset/*` — macOS bundle / App Store artwork.
- `linux/aeris.desktop` and `linux/hicolor/**` — Linux launcher and freedesktop icon hierarchy.
- `png/*` — generic desktop raster sizes for packaging paths that require PNG input.

Browser favicons, search icons, PWA/home-screen assets, web manifests, and other website-only outputs do not belong in this desktop asset directory and are intentionally excluded.

## Header rule

The terminal header uses `logo.svg` as the main logo and renders the `Aeris` brand name in Faculty Glyphic Regular. Do not substitute either logomark for that header logo.
