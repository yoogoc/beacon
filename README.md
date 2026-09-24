<img src="assets/app-icon/icon-256.png" alt="" width="104" align="right">

# Beacon

A cross-platform Kubernetes client built with Rust and [GPUI](https://www.gpui.rs),
in the spirit of Lens: browse any cluster, follow logs, edit manifests, forward
ports — natively, on macOS, Linux and Windows.

**Status: M6 — feature complete against the milestone plan.** Beacon connects to a kubeconfig context and lists any kind the
cluster serves — built-in or custom — following each with a watch. Tables are
column-for-column what `kubectl get` prints, and a CRD's own
`additionalPrinterColumns` are read from the cluster at runtime, so a CRD
installed this morning lists correctly this afternoon. Selecting a row opens a
detail panel with Overview, YAML, Events and (for pods) Logs. Objects can be
deleted, restarted, scaled and applied — and Beacon asks the cluster what you
are allowed to do before it offers, so an action that would be refused is
greyed out with the reason rather than failing with a 403. Editing the YAML
applies it with Server-Side Apply; when another field manager owns what you
changed, the refusal names the fields and their owner.

Pods also get logs, a one-shot command runner and an interactive shell. Ports
forward to localhost and keep running while you work elsewhere. CPU and memory
come from metrics-server, Helm releases are read straight out of the cluster,
and every cluster you connect to stays connected. See
[docs/DESIGN.md](docs/DESIGN.md) for the architecture and the milestone plan.

## Tabs

A tab is one view into one cluster, and several tabs can point at the same
cluster — so "Pods here, Deployments over there, and another cluster beside
them" is three tabs rather than three windows. Each tab keeps its own kind,
namespace, filter, selection and detail panel.

```
⌘T          another tab on the cluster in front
⌘W          close this tab
ctrl-Tab    next tab, ctrl-shift-Tab the previous one
```

`⌘` is `Ctrl` off macOS. The picker in the title bar changes what *this* tab
shows; `ctx` in the palette goes to a cluster, opening a tab only when none is
on it. Two tabs on one cluster are something to ask for with `⌘T`, not
something to get by picking the same cluster twice.

The connection is not per tab. A `ClusterSession` — client, discovery cache,
permission cache, port forwards — is keyed by cluster and shared, and the watch
registry refcounts, so two tabs on the same cluster and kind are one watch. A
background tab keeps watching: that is what makes coming back to it instant,
and it is the reason to have the tab at all. What it stops is the one-second
Age clock and the ten-second metrics poll, which are a repaint and a request
that nobody is looking at. Closing a tab drops its watches; the session stays,
so opening that cluster again does not reconnect.

## The command palette

`⌘K` (`Ctrl+K` off macOS). A prefix decides what the list is, so there is no
mode to be in and nothing to remember being in:

```
(nothing)   objects of the kind on screen
@           resource kinds, including CRDs
#           namespaces
ctx         clusters — its tab, or a new one
>           commands, including the tab ones
```

Matching is fuzzy within a section: `@dep` finds Deployment, `#kube-sys` finds
kube-system.

## Running

```sh
cargo run -p beacon
```

Logs go to the platform data directory (`~/Library/Application Support/dev.beacon.Beacon/logs`
on macOS). `RUST_LOG=beacon=debug,kube=debug` turns up the volume.

## Checking a change against a real cluster

```sh
cargo run -p beacon-kube --example watch -- [--context NAME] [--once] [Kind] [namespace]
cargo run -p beacon-kube --example watch -- --kinds
cargo run -p beacon-kube --example watch -- --apply-check Deployment default/my-app
```

Lists and follows any kind with no window at all, printing the same columns the
table renders. It is the fastest way to see whether a change to the domain
layer is right, and it is only possible because `beacon-kube` does not depend
on GPUI — see below. `--once` prints the first list and exits, which is what
makes the column code checkable against `kubectl get` in a loop; that diff is
how it is kept honest.

`--apply-check` does a **dry-run** Server-Side Apply, which is how the conflict
path is exercised against a real API server without writing anything.

Beacon differs from kubectl deliberately in two places, both documented in the
design notes: an absent value renders as `<none>` rather than as a blank cell,
and a kind with no columns of its own gets an `Age` rather than kubectl's
`Created At` timestamp.

## Looking at the UI

```sh
cargo run -p beacon &
scripts/screenshot.sh out.png
```

Logs cannot verify a UI. A window that opens, logs cleanly and renders nothing
produces exactly the same output as a correct one, so anything that changes what
is on screen gets looked at. macOS only, and the terminal running it needs
Screen Recording permission.

## Packaging

```sh
cargo install cargo-packager --locked
cargo build --release -p beacon
cargo packager -p beacon --release --formats app,dmg
```

One configuration in `crates/beacon/Cargo.toml` covers all three platforms:
`.app` and `.dmg` on macOS, `.deb` and AppImage on Linux, an NSIS installer on
Windows. `.github/workflows/package.yml` runs the same two commands across six
runners on every push to main and publishes the results.

What has actually been built, what signing would take, and which platforms are
still guesses: [docs/PACKAGING.md](docs/PACKAGING.md).

The icons come from `assets/app-icon/beacon-icon.svg`; `scripts/icons.sh`
regenerates the `.icns`, the `.ico` and the PNG set from it, rendering each size
from the SVG rather than downscaling the largest one.

## Layout

```
crates/
  beacon-kube/     Kubernetes domain layer — config, sessions, discovery, watches, store
  beacon-columns/  Column definitions, and what kubectl prints in each of them
  beacon-ui/       GPUI views, the resource catalog, theme tokens, the tokio bridge
  beacon/          The binary: logging, startup, window
assets/
  app-icon/        beacon-icon.svg and everything scripts/icons.sh renders from it
scripts/
  screenshot.sh    Capture the running window (see above)
  icons.sh         Regenerate assets/app-icon/ from the SVG
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

## One thing to know about the table

`gpui-component`'s `TableState` caches its column layout. Changing the
`ColumnSet` and calling `cx.notify()` is not enough — the header keeps the
previous shape and the new columns are simply not drawn. Call
`state.refresh(cx)` whenever the columns change, not just the rows.

## One thing to know about writing tests here

In a module that does `use gpui_kit::*`, a test module must import what it
needs by name rather than with `use super::*`. The glob carries GPUI's own
`#[test]` macro, which shadows Rust's; the symptom is `recursion limit reached
while expanding #[test]`, which says nothing about the cause.
