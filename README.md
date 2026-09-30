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
greyed out with the reason rather than failing with a 403. The YAML is
syntax-highlighted and has a Format button that rewrites what you typed —
which also answers whether it parses at all, without writing to the cluster to
find out. Editing it applies with Server-Side Apply; when another field
manager owns what you changed, the refusal names the fields and their owner.

Pod exec has its own permissions: WebSocket exec uses `get` on `pods/exec`,
while SPDY uses `create` (newer API servers also require `create` for
WebSockets). When an upgrade is refused, Shell and Exec fall back to a local
`kubectl` using the selected context, namespace and container. Install
`kubectl` on Beacon's PATH to use this compatibility path. Shell keeps a real
PTY for interactive input and resizing, and closing the pane ends the child.
Permission preflight is a hint; EKS webhook rules can be incomplete.

Pods also get logs, a one-shot command runner and an interactive shell — the
shell's command is a box you can change, empty meaning bash falling back to
sh, and it sits beside the error when one did not exist. The shell is in
colour: the default command exports `TERM=xterm-256color`, without which
nothing in a container emits any, and the sixteen ANSI colours come from a
real palette rather than from theme tokens, which had collapsed them onto
eight. Ports
forward to localhost and keep running while you work elsewhere. CPU and memory
come from metrics-server, Helm releases are read straight out of the cluster,
and every cluster you connect to stays connected. See
[docs/DESIGN.md](docs/DESIGN.md) for the architecture and the milestone plan.

## Tabs

A tab is one view into one cluster, and several tabs can point at the same
cluster — so "Pods here, Deployments over there, and another cluster beside
them" is three tabs rather than three windows. Each tab keeps its own kind,
namespace, filter, selection and detail panel. The cluster tree stays on the
left of the tabs; selecting a cluster opens its existing tab or a new one, and
the active cluster expands to show its resource navigation. Clicking a resource
or choosing it in the command palette activates its tab on that cluster, or
opens one if none exists. Right-click a resource and choose **Open in new tab**
to open another copy, carrying the current namespace scope.

The `+` sits just after the last tab while the tabs still fit, and moves to
the right edge once they do not — a button inside a scrolling row can be
scrolled out of reach, and one you cannot find is worse than one that is not
where you expected. The bar says which case it is in through its own scroll
offset.

```
⌘T          another tab on the cluster in front
⌘W          close this tab
ctrl-Tab    next tab, ctrl-shift-Tab the previous one
```

`Esc` closes the detail panel, except while the shell inside it has focus —
there Escape belongs to the shell, and the terminal swallows every key it is
given, `⌘K` included. `⌘` is `Ctrl` off macOS.

Keyboard shortcuts need something to be focused: GPUI dispatches a key along
the path from the focused node upwards, so `BeaconApp` holds a focus handle
on its root and takes focus when the window opens, and every `on_action` lives
on that same root. An action handler on a child of it — `ClusterView`, say —
is below the focused node and never runs. The cluster tree and `ctx` in the
palette go to a cluster, opening a tab only when none is on it. `⌘T` or a
sidebar resource's **Open in new tab** action opens another tab on that cluster;
selecting the cluster itself does not duplicate a tab.

The connection is not per tab. A `ClusterSession` — client, discovery cache,
permission cache, port forwards — is keyed by cluster and shared, and the watch
registry refcounts, so two tabs on the same cluster and kind are one watch. A
background tab keeps watching: that is what makes coming back to it instant,
and it is the reason to have the tab at all. What it stops is the one-second
Age clock and the ten-second metrics poll, which are a repaint and a request
that nobody is looking at. Closing a tab drops its watches; the session stays,
so opening that cluster again does not reconnect.

Click **watches** in the bottom-right status bar to inspect the current
cluster's watches: resource kind, namespace, selectors, cached object count,
and subscriber count. Idle watches stay visible until their 30-second grace
period ends. Click **clusters** to see connected sessions, health, API server,
tab and watch counts, and port forwards; click a session to switch to it.
Both lists refresh while open and inspect existing state without starting
additional watches.

## Waiting

A table being filled shows the skeleton rows, not an empty grid: "nothing
here" and "not here yet" should not look the same. The first batch from any
watched namespace ends it, so rows appear as they arrive rather than all at
once — and so does the session going Degraded, because a watch that cannot
start never sends a first batch and the placeholder would otherwise spin for
as long as the app is open. Everything else that waits — connecting, fetching
an object, looking for events, reading Helm releases — says so with a
spinner.

## Namespaces

A cluster opens on the namespace its kubeconfig context names, or on `default`
when it names none — the same fallback `kubectl` uses, and a great deal less
than every namespace of a busy cluster.

The picker in the toolbar has two click targets per row, and the difference
between them is the whole design: **the tick box adds and removes, the name
picks that one and nothing else** and closes the menu. Multi-select is what
you get for reaching for a checkbox, so the ordinary case — one namespace,
chosen by name — stays a single click. Unticking the last one lands on all of
them rather than on nothing. Everywhere else, `#` in the palette included, is
single-select as it always was.

Several namespaces are several watches, not one cluster-wide watch filtered
down. That costs a connection each, and buys the thing multi-select is mostly
for: a cluster-wide list is refused outright for anyone whose RBAC is
namespaced, which is exactly the person picking namespaces by hand.

Right-click an object row for actions specific to its kind. Pods offer logs,
exec, shell and declared ports; workloads offer restart or scale where the API
supports them; ConfigMaps and Secrets open directly to their Data tab. YAML,
copying the name and applicable write actions remain available across kinds.
Write actions respect the current RBAC answer, and a menu action stays bound
to the object that was right-clicked.

## ConfigMaps and Secrets

Both get a **Data** tab: one text box per key, instead of the YAML pane. That
is the whole point — every value of a Secret is base64, which is not
something a person can edit, and a ConfigMap's multi-line values are folded
into a YAML block scalar where the indentation is syntax.

A value that is not text — a TLS key, a keystore — is described and not
offered for editing, because putting it through a text box would corrupt it
on save. A Secret's values start covered, and Save is disabled until they are
revealed. Saving sends the whole object through the same Server-Side Apply
path the YAML tab uses, so a conflict reads the same either way and the
values nobody touched travel back exactly as they arrived.

## The sidebar

Built-in kinds are filed under the seven headings people already think in —
Workloads, Config, Network and so on — because nothing in the API says a
`Lease` is a coordination primitive and an `Endpoint` is networking; that is
a table, not a rule.

Everything the table does not name is filed under **its own API group**, one
collapsed heading each: `argoproj.io`, `traefik.io`, `flowcontrol.apiserver.k8s.io`.
The active cluster expands in the window's sidebar. Built-in sections appear
directly under it, while extension groups are gathered under **Custom
resources**; less common Kubernetes API groups stay with the built-in sections.
The group is the one piece of structure the cluster really gives us, and the
one people already use. Rows drop the group from their label, since the
heading above them has just said it — but search still matches and shows the
qualified name, because a flat list of results has no heading to lean on.

## The command palette

`⌘K` (`Ctrl+K` off macOS). A prefix decides what the list is, so there is no
mode to be in and nothing to remember being in:

```
(nothing)   objects of the kind on screen
@           resource kinds, including CRDs
#           namespaces — scopes to exactly that one
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
  between patch releases, so upgrading is its own task, not a side effect. Its
  `tree-sitter-yaml` feature is what highlights the YAML pane.
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
