# Beacon patches to gpui-base 0.7.1

This directory contains `crates/base` from the GPUI Kit commit
`3cc2d0d624ce124be62eb4670e04197bc78bb8a5`. This snapshot follows the
0.7.1 release and includes the optional GPUI Fast backend. Its Apache 2.0
license is preserved in `LICENSE-APACHE`.

The standalone `Cargo.toml` resolves upstream workspace inheritance with the
same dependency versions, features and Clippy settings. The workspace patches
the Git source of GPUI Kit so every layer uses this matching Base implementation.

`src/input/base/state.rs` adds initial folding support:

- `EditorState::set_initial_folded_lines` accepts zero-based header lines.
- Pending folds are applied when synchronous or asynchronous syntax parsing
  produces their ranges. Successful requests are consumed, preserving manual
  unfolding on subsequent parses.
- Reloading, editing, or disabling folding cancels remaining pending requests.
- Two regression tests cover delayed parsing, manual unfolding, unchanged
  document contents, and cancellation.

`src/input/base/element.rs` sizes the scrollable content using visible display
rows instead of unfolded rows. This prevents scrolling into blank space after
folding large YAML fields. A regression test verifies the scroll height and
that the last visible line remains in the viewport at the bottom.

Beacon calls the API once when it loads resource YAML. Remove this vendor patch
when the pinned upstream version offers an equivalent supported API.

Run the patch's editor regression tests with the selected GPUI Fast backend:

```sh
cargo test --manifest-path vendor/gpui-base/Cargo.toml --target-dir target --features gpui-fast --lib initial_folds
cargo test --manifest-path vendor/gpui-base/Cargo.toml --target-dir target --features gpui-fast --lib folded_editor_scroll
```

The vendored library is excluded from workspace membership; this command creates
its own ignored lockfile without changing Beacon's pinned dependency resolution.

`SelectableText::highlights` passes UTF-8 ranges to GPUI's styled text while
retaining the original selection and clipboard contents. Beacon uses it for
literal keyword highlighting in Pod logs.

The selection regression test paints highlighted text, selects it through native
mouse events, and verifies that selection returns the original text:

```sh
cargo test --manifest-path vendor/gpui-base/Cargo.toml --target-dir target --features gpui-fast --lib selectable_text
```
