# Beacon

A cross-platform Kubernetes client built with Rust and [GPUI](https://www.gpui.rs),
in the spirit of Lens: browse any cluster, follow logs, edit manifests, forward
ports — natively, on macOS, Linux and Windows.

**Status: M1.** Beacon connects to a kubeconfig context, lists pods and follows
them with a watch, and switches cluster and namespace from the title bar. The
list is column-for-column what `kubectl get pods` prints. Writing to a cluster,
logs, detail panes and the command palette are still ahead; see
[docs/DESIGN.md](docs/DESIGN.md) for the architecture and the milestone plan.

## Running

```sh
cargo run -p beacon
```

Logs go to the platform data directory (`~/Library/Application Support/dev.beacon.Beacon/logs`
on macOS). `RUST_LOG=beacon=debug,kube=debug` turns up the volume.

## Checking a change against a real cluster

```sh
cargo run -p beacon-kube --example watch -- [context] [namespace]
```

Lists and follows pods with no window at all, printing the same columns the
table renders. It is the fastest way to see whether a change to the domain
layer is right, and it is only possible because `beacon-kube` does not depend
on GPUI — see below. Diffing its output against `kubectl get pods -A` is how
the column code is kept honest.

## Looking at the UI

```sh
cargo run -p beacon &
scripts/screenshot.sh out.png
```

Logs cannot verify a UI. A window that opens, logs cleanly and renders nothing
produces exactly the same output as a correct one, so anything that changes what
is on screen gets looked at. macOS only, and the terminal running it needs
Screen Recording permission.

## Layout

```
crates/
  beacon-kube/     Kubernetes domain layer — config, sessions, watches, store
  beacon-columns/  Column definitions, and what kubectl prints in each of them
  beacon-ui/       GPUI views, theme tokens, the tokio bridge
  beacon/          The binary: logging, startup, window
scripts/
  screenshot.sh    Capture the running window (see above)
```

## Two rules the architecture depends on

**`beacon-kube` never depends on GPUI.** The UI is one consumer of the domain
layer; a headless binary or an integration test is another. Most cluster logic
is far cheaper to test without a window, and it stays that way only if the
dependency cannot creep in.

**Network work lives on the tokio runtime, views live on the GPUI thread, and
they exchange nothing but messages.** `kube` runs on hyper and needs a tokio
reactor; GPUI's executor is not tokio, and polling a kube future on it panics.
`crates/beacon-ui/src/bridge.rs` is the only place the two meet. The foreground
thread never blocks, and producers coalesce their events on a frame-sized
window before sending — a first `list` of a large namespace is thousands of
events, and one render per event would stall the frame loop for seconds.

## Dependencies worth knowing about

- `gpui-kit` bundles GPUI, `gpui-base`, `gpui-component` and the icon assets as
  one pinned dependency. It is pinned exactly (`=0.6.4`): the API still moves
  between patch releases, so upgrading is its own task, not a side effect.
- `k8s-openapi` 0.28 models timestamps with [jiff](https://docs.rs/jiff), not
  chrono.
- `kube`'s `http-proxy` feature is on deliberately. kube reads `HTTPS_PROXY`
  from the environment, and with the feature off it refuses to connect at all
  rather than falling back to a direct connection — which is a confusing
  failure on any machine that has a proxy configured.

## One thing to know about writing tests here

In a module that does `use gpui_kit::*`, a test module must import what it
needs by name rather than with `use super::*`. The glob carries GPUI's own
`#[test]` macro, which shadows Rust's; the symptom is `recursion limit reached
while expanding #[test]`, which says nothing about the cause.
