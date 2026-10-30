<p align="center">
  <img src="assets/app-icon/icon-256.png" width="112" alt="Beacon icon">
</p>

<h1 align="center">Beacon</h1>

<p align="center">
  <strong>English</strong> · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <strong>A native Kubernetes desktop client</strong><br>
  Multiple clusters · Live resource views · Pod terminals · Custom themes
</p>

<p align="center">
  <a href="https://github.com/yoogoc/beacon/actions/workflows/ci.yml"><img src="https://github.com/yoogoc/beacon/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/Rust-1.98-F74C00?style=flat&logo=rust&logoColor=white" alt="Rust 1.98">
  <img src="https://img.shields.io/badge/UI-GPUI-3B82F6?style=flat" alt="Built with GPUI">
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/License-Apache--2.0-22C55E?style=flat" alt="License declared in Cargo: Apache-2.0"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#editing-resources">Editing resources</a> ·
  <a href="docs/USAGE.md">User guide (中文)</a> ·
  <a href="https://github.com/yoogoc/beacon/releases">Downloads</a> ·
  <a href="docs/DEVELOPMENT.md">Developer guide (中文)</a>
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/workspace-dark.jpg">
    <source media="(prefers-color-scheme: light)" srcset="docs/images/workspace-light.jpg">
    <img src="docs/images/workspace-light.jpg" alt="Beacon: cluster sidebar, resource tabs, namespace and label filters, Pod container states, and resource metrics" width="1200">
  </picture>
</p>

<p align="center">
  <sub>Current application UI rendered with local demo data; images follow your browser's color preference.</sub>
</p>

<details>
  <summary><strong>Cluster overview · Capacity and trends</strong></summary>

  <p align="center">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/images/cluster-overview-dark.jpg">
      <source media="(prefers-color-scheme: light)" srcset="docs/images/cluster-overview-light.jpg">
      <img src="docs/images/cluster-overview-light.jpg" alt="Cluster CPU, memory and Pod capacity, usage trends, and node health" width="1200">
    </picture>
  </p>
</details>

<details>
  <summary><strong>Argo Workflows · Adaptive execution canvas</strong></summary>

  <p align="center">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/images/workflow-dark.jpg">
      <source media="(prefers-color-scheme: light)" srcset="docs/images/workflow-light.jpg">
      <img src="docs/images/workflow-light.jpg" alt="Workflow details and an execution canvas with zoom, search, and a minimap" width="1200">
    </picture>
  </p>
</details>

Built with Rust and GPUI, Beacon brings clusters, resource lists, details, and container tools into one desktop workspace.
It uses your existing kubeconfig and cluster permissions, discovering both built-in resources and CRDs from the cluster without additional plugins.

## Features

| Feature | What you can do |
| --- | --- |
| **Cluster workspace** | Switch contexts from the sidebar and keep multiple clusters connected. Recognize EKS names, set aliases and custom icons, disconnect, and reconnect. |
| **Cluster overview** | Inspect health, CPU / memory / Pod capacity, requests and declared limits, node summaries, and recent warning events. Use session samples with Kubernetes Metrics API or 30m / 1h / 6h history with Prometheus; open node and event details in a side overlay. |
| **Resources and CRDs** | Follow live Watch updates, use kubectl-style columns for built-in resources, and read CRD `additionalPrinterColumns` dynamically. |
| **Nodes** | Inspect taints, EKS node groups, and resident Pods. Cordon / uncordon nodes, review affected Pods and disruption budgets before draining, and follow eviction results. |
| **Search and filters** | Fuzzy search, sorting, namespace multi-select, and Kubernetes label selectors. Filter Pod / Deployment status, Service type, Ingress class, PVC attributes, Secret type, and CRD scope. |
| **Global search and favorites** | Search resources across connected clusters and namespaces; favorite clusters and objects for quick access. |
| **Personalized lists** | Choose visible columns, drag headers to reorder, and resize columns. Layout and sorting save per cluster and resource type; name and reuse filter combinations. |
| **Related resources** | Navigate from Deployments to ReplicaSets and Pods, Services to EndpointSlices and Pods, and PVCs to their consuming Pods in a live Related detail tab. |
| **Configuration references** | See which Pods and workloads use a ConfigMap or Secret, including referenced keys, environment variables, and mount paths; inspect missing references and jump to consuming resources. |
| **Resource details** | Overview, YAML, and Events; selectable values; collapsible labels and annotations; container ports, requests and limits, probes, environment, and mounts; clickable owner links. |
| **YAML editor** | Schema-aware completion, field documentation, inline diagnostics, formatting, and configurable field folding. Review changes before saving; version-checked Patch is the default, with optional Server-Side Apply. |
| **Argo Workflows** | Dedicated resource group, Workflow and template graphs, CronWorkflow history, and separate node details. Submit templates with parameters; retry, resubmit, suspend, resume, stop, or terminate runs with review and permission checks. |
| **Pod tools** | Logs with keyword search and highlighting, pause / resume follow, jump to latest, word wrap, container selection, and download. Run commands, open Shell, and forward ports in a separate bottom panel. Filled / hollow markers show regular / init container states. |
| **Structured logs** | Inspect JSON / logfmt fields, filter by level, regex, fields, and time range, bookmark lines, and export matching retained logs. Available for Pod and aggregated logs. |
| **Pod files** | Open Files from a Pod's context menu in its own tab; browse running containers, preview text and metadata, review diffs before saving, upload, choose a download folder, download directories as tar archives, rename, create directories, and confirm multi-file deletion. Requires `pods/exec` access, a container shell and GNU/BusyBox file tools; archive downloads require `tar`. |
| **Aggregated logs** | Follow Pods selected by a workload or label, with Pod / container prefixes and timestamps. New replicas join automatically; search, pause, wrap, and download separate log files to a chosen folder. |
| **Deployment history** | Inspect retained revisions, images, and creation times; review a template diff before rolling back and follow rollout progress. |
| **Jobs and CronJobs** | Run CronJobs immediately, suspend or resume schedules, and rerun a Job as a new Job while keeping its original record. Follow the resulting Job, Pods, and logs. |
| **Debug containers** | Add an ephemeral container to a running Pod, choose its image, target container, and security profile, then open its terminal. |
| **Network paths** | Explore Ingress → Service → EndpointSlice → Pod relationships, inspect missing references, readiness, and port mismatches, and navigate between resources. |
| **Resource transfer** | Copy or move resources across namespaces or kubeconfig clusters in a separate, three-step window. Review target names, conflicts, dependencies, and masked Secret manifests; create and verify targets before separately confirming source cleanup. Existing targets are never overwritten; persistent data and generated credentials require separate migration. |
| **Resource comparison** | Choose resources across connected clusters or namespaces and compare them in a separate, read-only diff window. Runtime fields are excluded and Secret values concealed by default. |
| **Resource operations** | Create YAML resources with server validation; review diffs before saving changes. Restart, scale, and delete resources, including bulk Deployment restart and multi-resource deletion with per-target results. Row and right-click menus share resource-specific operations and permission checks. |
| **ConfigMaps and Secrets** | Search a virtualized key list, edit one value at a time with syntax highlighting, folding, resizable height, and horizontal scrolling. Stage whole key/value deletions and restore them before saving. Secret values start concealed; TLS details include subjects, issuers, validity, algorithms, key strength, extensions, and public keys. |
| **Metrics** | View Pod / Node / container CPU and memory trends, requests and limits, and restart / OOM markers. Use Kubernetes Metrics API session samples or Prometheus history. |
| **Helm** | Inspect release values, manifests, history, and revision diffs. Install, upgrade, roll back, and uninstall with review and progress reporting; lifecycle operations use a local `helm` CLI. |
| **RBAC inspection** | Run server-backed **Can I?** checks for the current user or another subject, trace bindings to roles and rules, and inspect permissions from a 403 error. |
| **Port forwarding** | Forward from Pods, Services, or Deployments, choose local ports, save presets, and explicitly restore them. Backend Pod replacement can select a new target and resume the forward. |
| **Updates** | Automatic checks, optional automatic downloads, signed packages, Stable / Development channels, and restart confirmation that protects unapplied edits. |
| **Appearance and connections** | Light, dark, and named custom themes; locally saved fonts and colors; global and per-cluster proxies. |
| **Language** | English and Simplified Chinese, following the system by default. Switch in Settings → Appearance; open windows and menus update immediately. |
| **Diagnostics and keyboard navigation** | Command palette, searchable shortcut guide, separate application log window, and connection / Watch inspection from the status bar. |

## Quick start

### Download a build

Check [Releases](https://github.com/yoogoc/beacon/releases) for available platform artifacts.
Pushes to `main` do not automatically build or publish packages.
Formal `vX.Y.Z` tags must match the Cargo version and publish a Release marked as
**Latest** after all platform packages succeed. See the [release procedure (中文)](docs/PACKAGING.md#版本规则与正式发版).
Manually triggered packaging runs publish artifacts in
[Actions](https://github.com/yoogoc/beacon/actions/workflows/package.yml).

| Platform | Package formats | Verification |
| --- | --- | --- |
| macOS · Apple Silicon / Intel | `.app`, `.dmg` | Building, packaging, and launch verified locally; signing and notarization remain unverified. |
| Linux · x86_64 / ARM64 | `.deb`, AppImage | Packaging jobs configured; not yet verified locally. |
| Windows · x86_64 / ARM64 | NSIS installer | Packaging jobs configured; not yet verified locally. |

Available artifacts depend on Release and Actions results. See [Packaging and distribution (中文)](docs/PACKAGING.md) for details.

### Run from source

Install the Rust toolchain and platform build dependencies. The repository pins Rust 1.98 in `rust-toolchain.toml`.
macOS requires Xcode Command Line Tools. Linux system dependencies and the Windows MSVC environment are described in the
[developer guide (中文)](docs/DEVELOPMENT.md).

```sh
git clone https://github.com/yoogoc/beacon.git
cd beacon
cargo run --locked
```

Beacon reads `KUBECONFIG`, falling back to `~/.kube/config`. To select a configuration:

```sh
KUBECONFIG=/path/to/kubeconfig cargo run --locked
```

In **Settings → Kubeconfigs**, add multiple local files, watch a folder,
or import pasted YAML. Preview and select contexts before adding them. Source
changes save automatically; linked files stay in place, and pasted configurations
are stored privately in Beacon's configuration folder. In **Settings → Workspace**,
choose collapsible source groups or a flat list with source labels. Source indicators
appear when contexts come from multiple sources; folder file names distinguish
matching contexts within the same source.

On your first launch:

1. Click a cluster name in the sidebar to connect.
2. Expand a resource group and select Pods, Deployments, or another resource type.
3. Click a resource to inspect its details, or right-click for available actions.

**Beacon starts without connecting to a cluster and opens no resource list until you select one.**
Kubeconfig credential helpers must be available on PATH. If a cluster rejects WebSocket Exec upgrades,
Shell / Exec can automatically fall back to a locally installed `kubectl`.

## Workspace and shortcuts

Selecting a resource type reuses a tab for the **same cluster and resource type** in the focused pane. Right-click and choose **Open in new tab** for another view.
Each tab keeps its own namespace, search, filters, and details. The sidebar sits outside the tabs; drag the dividers to resize the sidebar, right-side details, and bottom Pod panel.

Right-click a tab to split right or down, or move it to a separate window. Splits can be nested and resized.
Drag tabs between groups or windows to merge them; drop near a pane's edge to create a split.
Moving a tab preserves its view, YAML draft, and Pod tools. Windows share cluster connections and Watches.

| Action | macOS | Windows / Linux |
| --- | --- | --- |
| Command palette | `⌘ K` | `Ctrl K` |
| New / close tab | `⌘ T` / `⌘ W` | `Ctrl T` / `Ctrl W` |
| Next / previous tab | `Ctrl Tab` / `Ctrl Shift Tab` | `Ctrl Tab` / `Ctrl Shift Tab` |
| Split right / down | `⌘ ⌥ →` / `⌘ ⌥ ↓` | `Ctrl Alt →` / `Ctrl Alt ↓` |
| Application settings | `⌘ ,` | `Ctrl ,` |
| Application logs | `⌘ Shift L` | `Ctrl Shift L` |
| Shortcut guide | `F1` | `F1` |

The command palette accepts `@resource-type`, `#namespace`, `ctx cluster`, and `>command`.
For example, `@dep` finds Deployment and `#kube-sys` finds kube-system.
Open **Help → Keyboard shortcuts** for the full shortcut list and focus behavior.

## Editing resources

### YAML

1. Open a resource's **YAML** tab and edit with completion, field documentation, and inline diagnostics.
2. Click **Save** to prepare a diff. By default, Beacon saves only changed fields with a Patch that checks the resource UID and version.
3. Review the side-by-side diff and choose **Confirm & save** to write the changes.

If the resource changed on the server, your draft stays intact. **Review latest** carries your edits onto the latest resource while preserving fields you did not edit; review the new diff before confirming.
Choose **Server-Side Apply** when you need field ownership management. Only an actual ownership conflict offers a separate, reviewed force-apply action.

### ConfigMaps and Secrets

Open **Data**, search for a key, and select it to read or edit its value. The key list renders only visible rows, and editors and syntax analysis load on demand. Unsaved edits stay intact when switching keys.

- Content-aware highlighting, code folding, and a draggable height handle keep configuration values readable.
- Long lines scroll horizontally by default. **Wrap lines** toggles wrapping and carries the choice across keys in the current detail view.
- **Delete key** stages removal of the whole key/value pair; **Restore key** undoes it. Click **Save** to review edits and deletions together before confirming. Key deletion uses a version-checked Patch.

Secret values require **Reveal** before reading, editing, deleting, or saving. Binary values can be deleted as whole entries; text editing is available only for UTF-8 values. Untouched values keep their original encoding.

## Make it yours

**Application settings:** use **Beacon → Settings…** in the macOS menu bar, or **Menu → Settings…** on Windows / Linux.
Choose a theme, adjust fonts and text / button colors, save multiple local themes, and configure a global HTTP, HTTPS, or SOCKS5 proxy.

**Updates:** open **Settings → Updates** or **Check for updates…** in the application menu. Installation always asks before restarting. Linux `.deb` builds provide a package download for manual installation.

**Cluster settings:** right-click a cluster and open **Cluster settings…**.
Set an alias, choose a preset or imported SVG icon, configure a separate proxy, and select Kubernetes Metrics API / Prometheus / Disabled as the metrics source.
Prometheus supports a bearer token and four editable queries, with a test action before saving.

Cluster proxies override the global proxy. **Save and reconnect** applies connection changes while preserving resource tabs.
Configuration paths, proxy behavior, and Prometheus label requirements are covered in the [configuration guide (中文)](docs/USAGE.md#应用与集群配置).

## Development

Beacon uses GPUI Kit 0.7.3 with GPUI Fast 0.1.7, pinned to the upstream release
commit. Automatic view retention stays disabled by default as a workaround for
paint-cache overflows observed in earlier GPUI Fast versions; layout and text
optimizations remain enabled. YAML highlighting and
initial field folding remain enabled. See the [development guide (中文)](docs/DEVELOPMENT.md#依赖与打包)
for dependency pins, patch tests, and rendering diagnostics.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --features beacon-ui/ui-tests -- -D warnings
cargo test --locked --workspace --features beacon-ui/ui-tests -- --test-threads=1
```

The `ui-tests` feature includes headless interaction checks backed by a local mock Kubernetes API. Run these alongside the workspace tests when changing UI or resource operations.

| Crate | Responsibility |
| --- | --- |
| `beacon` | Application startup, logging, and platform packaging. |
| `beacon-ui` | GPUI views, themes, resource catalog, command palette, and asynchronous bridge. |
| `beacon-kube` | Configuration, connections, discovery, Watch, permissions, and Kubernetes operations; no GPUI dependency. |
| `beacon-columns` | Built-in and CRD table columns, field formatting, and container states. |
| `beacon-updater` | Update discovery, downloads, package signature verification, and platform installation. |

Network tasks run on Tokio; views run on the GPUI thread. Messages carry batched updates between them.
Tabs for a cluster share its connection, and identical resource subscriptions share a Watch.

## Documentation

The detailed guides below are currently written in Chinese.

| Document | Contents |
| --- | --- |
| [User guide](docs/USAGE.md) | Tabs, namespaces, filters, details, Pod tools, resource operations, settings, and logs. |
| [Developer guide](docs/DEVELOPMENT.md) | Build dependencies, checks, headless validation, UI screenshots, and development conventions. |
| [Design notes](docs/DESIGN.md) | Initial architecture, technology choices, and milestones. |
| [TODO list](docs/TODO.md) | Prioritized feature plans, implementation scope, and quality tasks. |
| [Packaging and distribution](docs/PACKAGING.md) | Platform artifacts, CI releases, signing, and verification status. |

The project declares **Apache-2.0** in its Cargo metadata. See the [icon notes](crates/beacon-ui/assets/README.md) for UI icon sources and licensing.
