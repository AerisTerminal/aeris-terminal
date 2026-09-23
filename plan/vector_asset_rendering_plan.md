# Asceify Native Vector Asset Rendering Plan

## Purpose

Asceify needs one reliable native rendering path for static vector artwork used by:

- theme-colored interface icons;
- Asceify brand marks and wordmarks;
- broker, exchange, venue, and company marks;
- chart-series glyphs;
- future trusted product artwork.

The product goal is:

> Every supported vector asset renders with its intended colors, aspect ratio, transparency, and
> edge quality at the actual device-pixel size of the GPUI element, without blocking the UI thread
> or allowing rendering work and memory to grow without bounds.

This plan replaces the current special-case colored-SVG workaround with a reusable Asceify-owned
presentation component. It deliberately does not introduce Vello. GPUI already owns the window,
graphics device, image atlas, and presentation schedule, and its pinned `SvgRenderer` already uses
`usvg` plus `resvg` for static SVG parsing and rasterization. A second `wgpu` renderer would create a
second graphics lifecycle without removing the need to upload the result into GPUI.

## Current state and concrete failure modes

The repository currently has two distinct GPUI paths:

1. `gpui::svg()` renders an SVG as a monochrome alpha mask and applies the element's text color.
   This is correct for theme-neutral UI glyphs that use `currentColor`.
2. `gpui::img()` decodes a full-color SVG into a raster image. The image is produced at the SVG's
   intrinsic size, so enlarging it can stretch a smaller bitmap and expose blur or pixelation.

`apps/desktop/src/components/terminal_chrome.rs` works around the second behavior through
`ColoredSvgMark`, `rasterize_colored_svg`, and `MARK_CACHE`. That workaround proves the basic
approach but is not the long-term owner because:

- it lives in terminal chrome even though onboarding, dialogs, symbol menus, and future surfaces
  need the same behavior;
- it handles only one square logical size rather than independent width and height;
- it finds an intrinsic width by scanning the first 768 bytes of XML for `width="..."`;
- it does not derive or enforce the `viewBox` aspect ratio;
- it parses and rasterizes synchronously while rendering a GPUI element;
- its process-global `HashMap` has no entry or decoded-byte bound;
- its key uses raw floating-point bit patterns instead of final integer device-pixel dimensions;
- cache lock poisoning silently disables reuse;
- a rasterization failure falls back to `img(path)`, which restores the quality problem the helper
  was created to avoid;
- failures are not classified or surfaced through bounded diagnostics;
- the current exact-size rasterization test does not exercise every brand wordmark, aspect-ratio
  mode, DPI scale, cache transition, or malformed asset.

Remote profile photos are a separate concern. They currently use GPUI's asynchronous image loader
and must not be folded into the trusted bundled-vector path merely because a remote URL may return
an SVG payload.

## Architecture decision

### Rendering ownership

- `apps/desktop/src/assets.rs` remains the single inventory and byte source for trusted embedded
  desktop artwork.
- A new Asceify-owned module under `apps/desktop/src/native_ui` owns vector presentation,
  requested-size calculation, derived-image lifecycle, bounded caching, and placeholder behavior.
- GPUI remains the only window renderer and the only owner that uploads `RenderImage` data to the
  platform graphics backend.
- GPUI's pinned `SvgRenderer` remains the SVG parser/rasterizer boundary. Asceify does not depend
  directly on `vello`, `vello_svg`, `wgpu`, `usvg`, `resvg`, or `tiny-skia` for this feature.
- The desktop vector cache owns only derived presentation images. It does not become a second asset
  inventory or store canonical account data.
- Network retrieval and account-profile validation remain outside the vector presentation module.

### Supported rendering classes

| Class | Source | Renderer | Color behavior | Intended use |
| --- | --- | --- | --- | --- |
| `MonochromeIcon` | Trusted embedded SVG | `gpui::svg()` | GPUI applies semantic theme color | Controls and tool glyphs |
| `VectorImage` | Trusted embedded SVG | GPUI `SvgRenderer` to exact device pixels | Preserve all SVG colors and alpha | Brand, broker, exchange, company, and series marks |
| `ProfilePhoto` | Validated HTTPS resource | Existing bounded GPUI image path | Preserve decoded raster pixels | User avatars |

Untrusted remote SVG is not a fourth general rendering class. It must either be rejected or
normalized by the website/account boundary into a bounded raster representation before the desktop
displays it.

## Target data flow

```text
AsceifyAssets
      |
      v
validated VectorAsset metadata
      |
      v
logical bounds + window scale -> exact integer pixel request
      |
      v
bounded/coalescing background raster work
      |
      v
bounded derived RenderImage cache
      |
      v
GPUI img(ImageSource::Render) -> GPUI graphics backend
```

Only the final immutable `Arc<RenderImage>` crosses into element rendering. SVG parsing,
rasterization, disk access, and network access do not run on the GPUI UI thread.

## Asset contract

### Typed identity

Every bundled full-color vector must have a stable typed identity. The implementation should reuse
the existing `BrandIcon`, `ExchangeLogo`, and `SeriesIcon` inventories and introduce another typed
inventory only when a concrete company/broker asset family exists. Call sites must not construct
arbitrary embedded paths.

Each vector identity exposes metadata equivalent to:

```rust
pub(crate) struct VectorAssetSpec {
    pub(crate) path: SharedString,
    pub(crate) view_box_width: u32,
    pub(crate) view_box_height: u32,
    pub(crate) fit: VectorFit,
}

pub(crate) enum VectorFit {
    Contain,
    Cover,
    Stretch,
}
```

The exact API may be refined during implementation, but these semantics are required:

- `Contain` preserves the entire asset and its aspect ratio.
- `Cover` preserves aspect ratio and permits intentional clipping.
- `Stretch` is explicit and is forbidden as a silent default.
- Square convenience constructors are permitted only for assets whose declared `viewBox` is square.
- View-box metadata is validated against the embedded file by tests; it is not rediscovered by
  scanning XML during every render.

### Bundled SVG validation

Every trusted vector asset must pass deterministic tests before it can enter the inventory:

- valid UTF-8 static SVG;
- one finite, positive `viewBox`;
- no zero or negative dimensions;
- declared metadata agrees with the `viewBox`;
- no `<script>`, event handlers, animation, or foreign content;
- no external URL references;
- no linked or embedded raster `<image>` unless a later reviewed product requirement explicitly
  adds a bounded policy for it;
- no SVG `<text>` in brand or broker artwork; text must be converted to paths so font availability
  cannot change the result;
- no uncontrolled font, stylesheet, or filesystem dependency;
- no asset larger than a reviewed encoded-byte limit;
- no duplicate asset identity or embedded path;
- theme-neutral icons use `currentColor` and do not contain hard-coded color values;
- full-color vectors do not rely on `currentColor` unless the asset specification explicitly binds
  a semantic color input.

The validator should understand XML/SVG structure through the selected renderer/parser boundary or
a small reviewed validation layer. It must not grow into a second SVG renderer.

## Device-pixel sizing and visual quality

### Size calculation

The component receives logical GPUI bounds and the current window scale factor. It must:

1. reject non-finite, zero, or negative logical dimensions;
2. multiply logical width and height by the scale factor;
3. round to stable positive integer device-pixel dimensions;
4. preserve the declared aspect ratio according to `VectorFit`;
5. enforce per-edge and decoded-byte limits before scheduling work;
6. use those final integer dimensions as the cache key and raster target.

The source SVG's `width` and `height` attributes are not authoritative for display quality. The
`viewBox` defines geometry; GPUI layout plus the window scale defines the requested raster size.

### Quality rules

- Never enlarge a raster generated below the current device-pixel target as the settled result.
- A cached higher-resolution rendition may be downsampled temporarily while an exact rendition is
  pending; a lower-resolution rendition must not be stretched as a silent fallback.
- Common fixed sizes should be prewarmed when practical so normal headers and menus do not flash a
  placeholder on first use.
- Alpha must remain premultiplied in the format expected by GPUI.
- Color-space conversion must follow GPUI's existing image path; the vector module must not invent a
  separate color-management policy.
- Layout dimensions stay stable while an image is loading or fails, preventing surrounding controls
  from shifting.
- DPI changes and moving a window between displays request a new exact rendition without discarding
  a still-usable higher-resolution cached result prematurely.
- Aspect ratio is tested independently from raster success. A successfully decoded but distorted
  logo is a failure.

## Background work and cancellation

Rasterization can be CPU-intensive for complex SVGs, so it must run through GPUI background work
rather than inside `RenderOnce::render` or another UI-thread callback.

The derived-image owner must provide:

- request coalescing by exact asset/pixel key;
- a small explicit maximum number of concurrent raster jobs;
- a bounded pending queue;
- newest-request-wins cancellation or supersession for a surface undergoing repeated resize/DPI
  changes;
- generation fencing so an old job cannot replace a newer requested rendition;
- no detached task that can mutate GPUI state directly;
- UI notification only after the result is installed through the GPUI-owned foreground boundary;
- clean shutdown without waiting synchronously on the UI thread.

Queue overflow behavior must be explicit: preserve the best valid cached rendition or placeholder,
record one coalesced diagnostic, and allow a later render request to retry. It must not allocate an
unbounded task per frame.

Initial concurrency, queue, edge, entry, and byte limits must be selected conservatively, encoded as
named constants, and justified by focused measurements before they are described as production
capacity. Tests must prove the limits; benchmarks must measure representative assets.

## Cache contract

### Key

The minimum cache identity is:

```text
(asset identity, content revision, device-pixel width, device-pixel height, render mode)
```

For embedded assets, a stable build-time content revision or digest prevents stale reuse if a future
persistent cache is introduced. The initial cache remains process-local; no disk cache is needed for
small bundled vectors.

### Bounds and eviction

The cache must be bounded by both:

- number of decoded renditions; and
- estimated decoded bytes, including width × height × four-byte pixel storage.

Least-recently-used eviction is appropriate for completed images. Pending work has a separate bound
and does not count as a completed entry. Failed requests use a short bounded negative-cache/backoff
entry so a malformed asset cannot be reparsed every frame.

Eviction must release the Asceify-held `Arc<RenderImage>` and invoke the appropriate GPUI image
drop/removal path when required. Tests must cover shared references so an image still in use is not
invalidated unsafely.

Cache behavior must remain correct when:

- the same asset appears in several windows;
- windows have different DPI scale factors;
- a window changes displays;
- several controls request the same key concurrently;
- a job fails or panics;
- the cache reaches its entry limit before its byte limit, and vice versa;
- the application shuts down while work is pending.

## Failure behavior and diagnostics

The rendering API returns classified failures rather than silently swallowing them. Required
categories include:

- unknown embedded asset;
- invalid metadata or dimensions;
- SVG parse failure;
- rasterization failure;
- request exceeds configured resource bounds;
- work queue saturated;
- retired/superseded request;
- renderer unavailable during shutdown.

The visible fallback is a stable, product-owned placeholder sized to the requested layout. It must
not call `img(svg_path)` and recreate the known low-resolution path.

Diagnostics must include the non-sensitive typed asset identity and failure category, never raw
remote payloads, profile URLs containing secrets, or SVG contents. Repeated identical failures must
be rate-limited/coalesced.

## Remote profile-photo policy

User profile presentation is account data, not a bundled vector asset.

- Keep initials as the permanent layout-stable fallback.
- Continue accepting only validated HTTPS URLs within the existing length and syntax bounds.
- Apply response-body, decoded-dimension, redirect, timeout, and content-type limits in the existing
  image/network boundary.
- Prefer normalized PNG or WebP profile-photo responses from the website/account service.
- Do not execute scripts, resolve external SVG resources, load SVG fonts, or allow SVG to access the
  filesystem or network.
- If native remote-SVG support ever becomes a hard product requirement, add an account-runtime-owned
  sanitize/normalize step and pass only bounded raster bytes to the desktop. Do not let the GPUI
  presentation component fetch arbitrary dependencies from SVG markup.

Because authentication is shared with the sibling website repository, any change to the profile
photo contract must inspect and verify both repositories and the complete website-to-account-runtime
to-desktop path.

## Implementation phases

### Phase A — characterize and lock the contract

- [ ] Inventory every current SVG and classify it as monochrome or full-color.
- [ ] Record intended `viewBox`, aspect ratio, rendering size, and call sites for each full-color
  asset through typed code rather than a separate manually maintained document.
- [ ] Add structural validation for all current SVG inventories.
- [ ] Add regression fixtures for viewBox-only SVG, width/height SVG, gradient, transparency,
  clipping/mask behavior used by real assets, malformed XML, and forbidden external content.
- [ ] Add a deterministic contact-sheet test or development surface containing every current asset.
- [ ] Capture the current broken cases at representative logical sizes and DPI factors so the final
  comparison proves the issue was fixed rather than merely rearranged.

Exit criteria:

- every current asset has exactly one rendering classification;
- malformed/unsafe assets fail before product use;
- the known blurry, recolored, or distorted cases have deterministic reproduction coverage.

### Phase B — reusable vector component and bounded owner

- [ ] Add `apps/desktop/src/native_ui/vector_image.rs`.
- [ ] Define typed asset specifications and aspect-ratio-preserving size calculation.
- [ ] Introduce one presentation-owned derived-image store initialized once by desktop startup.
- [ ] Schedule SVG parsing/rasterization through bounded GPUI background work.
- [ ] Render completed results through `ImageSource::Render` only.
- [ ] Add layout-stable loading and error placeholders.
- [ ] Implement request coalescing, exact generation fencing, cancellation/supersession, and bounded
  negative caching.
- [ ] Implement entry-and-byte-bounded LRU eviction.
- [ ] Add sanitized, coalesced diagnostics.

Exit criteria:

- the UI thread performs no SVG parsing or rasterization;
- the same exact request produces one job and one shared immutable result;
- cache and pending work remain within tested limits under repeated resize and multi-window demand;
- failures cannot re-enter the intrinsic-size `img(svg)` path.

### Phase C — migrate all bundled full-color artwork

- [ ] Replace `ColoredSvgMark` and `MARK_CACHE` in `terminal_chrome.rs`.
- [ ] Migrate series glyphs.
- [ ] Migrate exchange/broker marks in the terminal header, symbol menu, and terminal view.
- [ ] Migrate Asceify brand marks in onboarding and About UI.
- [ ] Add wordmark usage through an aspect-ratio-preserving constructor before any product surface
  begins using it.
- [ ] Remove `svg_intrinsic_width`, raw float cache keys, and the old SVG fallback path.
- [ ] Remove all superseded imports, tests, and helper code.
- [ ] Add an architecture assertion in `tools/naming_check` if needed to prevent another unbounded
  full-color SVG cache or direct Vello/wgpu renderer from appearing in desktop presentation code.

Exit criteria:

- every bundled full-color vector uses the shared component;
- no full-color SVG is rendered through intrinsic-size `img(path)`;
- there is one derived-vector cache and one implementation path.

### Phase D — quality, performance, and real GPUI verification

- [ ] Render every vector asset at 100%, 125%, 150%, 175%, 200%, and 300% Windows scaling.
- [ ] Verify small controls, large onboarding marks, rectangular wordmarks, and mixed-DPI window
  movement.
- [ ] Add deterministic golden images for representative assets and pixel-difference tolerances that
  catch blur, cropping, recoloring, and alpha regressions without masking real defects.
- [ ] Measure cold parse+raster latency and warm-cache lookup latency for simple, typical, and most
  complex bundled assets in an optimized build.
- [ ] Soak repeated resize/DPI requests and assert queue, task, cache-entry, and decoded-byte bounds.
- [ ] Verify eviction and re-render behavior after sustained use.
- [ ] Exercise the real installed GPUI window visually; unit tests alone are not proof of final
  composition, interpolation, or display-scale behavior.

Exit criteria:

- no accepted asset is visibly pixelated, stretched, clipped unexpectedly, or recolored at tested
  sizes and scales;
- measured cold work does not block the UI thread;
- warm rendering reuses cached output;
- sustained demand stays within all configured work and memory bounds;
- the real GPUI path has been visually confirmed and the exact tested configuration is reported.

### Phase E — profile-photo hardening when product work requires it

- [ ] Inspect native and website response contracts together.
- [ ] Define accepted raster media types and explicit encoded/decoded limits.
- [ ] Normalize provider avatars at the website/account boundary if inconsistent remote formats cause
  product failures.
- [ ] Preserve initials through loading and all failure states.
- [ ] Add redirect, timeout, oversized-body, malformed-image, and content-type mismatch tests.
- [ ] Verify browser sign-in, account runtime, vault-backed session restoration, and desktop avatar
  presentation end to end when the contract changes.

This phase must not delay the bundled vector fix unless a reproduced profile-photo defect is part of
the same delivery.

## Verification commands

Focused iteration should include:

```text
cargo fmt --all -- --check
cargo check -p asceify_desktop --locked
cargo test -p asceify_desktop --locked
cargo clippy -p asceify_desktop --all-targets --all-features --locked -- -D warnings
cargo test -p asceify_naming_check --locked
```

Before completed native delivery:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

The final report must separate deterministic test results, measured performance results, visual GPUI
confirmation, and installed-app confirmation. Compilation must not be presented as visual proof.

## Acceptance against repository principles

### Proper architecture

- GPUI remains the only platform renderer and graphics-device owner.
- The asset inventory remains authoritative in `AsceifyAssets`.
- One native UI module owns all derived full-color vector images.
- Account/network state does not move into presentation code.
- No Vello, `wgpu`, duplicate SVG renderer, or parallel asset inventory is introduced.

### Clean code

- Typed assets replace arbitrary paths at call sites.
- The brittle XML header scan and float-bit keys are removed.
- Errors are classified and visible through one stable placeholder contract.
- The old workaround is deleted after migration.

### Scalable

- Raster work, concurrency, pending requests, decoded images, decoded bytes, retries, and diagnostics
  are bounded.
- Identical work is coalesced.
- Resize and DPI churn use cancellation/supersession rather than accumulating jobs.
- Capacity claims require optimized measurements.

### Maintainable

- Monochrome icons, bundled full-color vectors, and remote photos have explicit separate contracts.
- Aspect ratio and fit behavior are data, not implicit call-site conventions.
- Every asset is covered through inventory-wide validation and a contact sheet.
- The rendering component exposes product semantics rather than renderer internals.

### Built for the long run

- Output is deterministic across supported platforms through the pinned GPUI SVG stack.
- Cache keys include dimensions and content identity.
- Failure, eviction, shutdown, multi-window, and mixed-DPI behavior are specified and tested.
- Dependency expansion is reserved for a measured requirement the existing renderer cannot satisfy.

## Vello re-evaluation gate

Vello should be reconsidered only if a concrete feature requires continuously changing vector scenes
rather than static assets, for example:

- thousands of vector paths changing every frame;
- continuous arbitrary zoom where exact-size cached raster renditions cannot meet measured frame-time
  or memory goals;
- vector animation or scene composition unavailable through GPUI primitives;
- a future GPUI renderer exposes supported Vello/wgpu device and texture interoperability.

Before adoption, a focused prototype must prove all of the following:

- shared graphics-device ownership or zero-copy texture interoperability with every supported GPUI
  backend;
- no blocking GPU readback into `RenderImage`;
- correct device-loss and multi-window lifecycle;
- bounded memory and submission work;
- better measured frame time or quality than the GPUI-native pipeline on representative Asceify
  workloads;
- no regression to release packaging, startup time, binary size, or supported hardware.

Without that evidence, Vello remains outside the dependency graph.

## Non-goals

- Building a browser-grade SVG engine.
- Supporting SVG scripting, animation, arbitrary external resources, or runtime fonts.
- Using SVG as a transport for untrusted account data.
- Replacing Nucleus chart rendering.
- Replacing GPUI's renderer or platform backends.
- Persisting a disk cache for the initial bundled-asset workload.
- Adding speculative company/broker inventories before actual assets and product surfaces exist.

## Delivery checkpoint

The first complete delivery is Phases A through D for bundled artwork. It is complete only when the
old `ColoredSvgMark` implementation is removed, all current full-color assets use the shared bounded
path, focused and workspace gates pass, optimized cache/work measurements are reported, and the real
GPUI result has been visually inspected at representative Windows scale factors.

Profile-photo changes remain a separately coordinated Phase E unless a concrete reproduced avatar
format failure requires them in the same task.
