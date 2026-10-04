# Beacon patches to gpui-base 0.6.4

This directory contains the published `gpui-base` 0.6.4 source from crates.io
(upstream commit `3c387ae0a3e9b14ee39fe98be2b51a882800aa16`, `crates/base`).
Its Apache 2.0 license is preserved in `LICENSE-APACHE`.

Beacon pins gpui-kit 0.6.4. That release supports manual editor folding but
does not expose an API to collapse selected sections when a document loads.
The workspace Cargo patch keeps the same version and adds this capability.

Only `src/input/base/state.rs` differs from the published source:

- `EditorState::set_initial_folded_lines` accepts zero-based header lines.
- Pending folds are applied when synchronous or asynchronous syntax parsing
  produces their ranges. Successful requests are consumed, preserving manual
  unfolding on subsequent parses.
- Reloading, editing, or disabling folding cancels remaining pending requests.
- Two regression tests cover delayed parsing, manual unfolding, unchanged
  document contents, and cancellation.

Beacon calls the API once when it loads resource YAML. Remove this vendor patch
when the pinned upstream version offers an equivalent supported API.

Run the patch's editor regression tests from the workspace root:

```sh
cargo test --manifest-path vendor/gpui-base/Cargo.toml --target-dir target --lib initial_folds
```

The vendored library is excluded from workspace membership; this command creates
its own ignored lockfile without changing Beacon's pinned dependency resolution.
