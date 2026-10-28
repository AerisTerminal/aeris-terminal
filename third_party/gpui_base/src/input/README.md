# Input module layout

The public Rust module remains `gpui_base::input`; the folders below organize
its implementation without adding public module-path segments.

- `base/` contains the shared text-editing engine and foundational behavior:
  state, layout, cursor, selection, movement, masking, soft-wrap display
  mapping, indentation, editor colors, native integration, and painting.
- `single_line/` contains the single-line `Input` element and `InputState`
  facade.
- `textarea/` contains the multi-line `Textarea` element and `TextareaState`
  facade.

`mod.rs` is the external seam. Keep public re-exports there so reorganizing the
implementation does not change callers' imports.

The root uses explicit `#[path]` declarations so this organization remains an
implementation detail and existing internal module relationships stay intact.
