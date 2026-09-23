# Beacon — 基于 Rust + GPUI 的跨平台 Kubernetes 客户端设计

> 目标：一个类 Lens 的桌面 K8s 客户端，原生渲染、冷启动 < 500ms、上万对象不卡顿。
> 本文是实现前的架构定稿，含技术选型、关键难点的解法、模块划分和里程碑。

---

## 0. 结论先行：三个决定全局的设计选择

1. **数据层完全动态**。不为每种资源写 Rust 类型，全部走 `Discovery` + `DynamicObject`，
   列表的列由"列定义表"驱动（内置资源用 kubectl 风格硬编码列，CRD 读 `additionalPrinterColumns`）。
   只在需要计算的少数地方（Pod 状态、Deployment 就绪数）按需反序列化成强类型。
   —— 这是 Lens 能一天支持任意 CRD 的原因，也是自研最容易做错的地方。
2. **两个 runtime，一条单向数据流**。kube-rs 依赖 tokio reactor，GPUI 有自己的 executor，
   两者不能混用。用一个独立的 tokio runtime 跑所有网络任务，通过 channel 把**合批后的增量**
   推给 GPUI 前台线程。前台线程永不 `block_on`。
3. **按需订阅 watch**。只 watch 用户当前正在看的 GVK（加少量常驻：Namespace/Node/Event），
   带引用计数和延迟关闭。Lens 的内存问题主要来自全量订阅。

---

## 1. 技术选型（版本已在本机 `cargo check` 验证通过，rustc 1.98.1 / macOS）

| 领域 | 选型 | 版本 | 说明 |
|---|---|---|---|
| UI 全家桶 | `gpui-kit` | =0.6.4 | **单一依赖**，re-export gpui + gpui-base + gpui-component + 图标资源，并 pin 好匹配的 GPUI 版本 |
| （其下）渲染 | `gpui-pre` | 0.3.5 | GPUI 的预发布系列，由 gpui-kit 带入，不直接依赖 |
| （其下）组件 | `gpui-component` | 0.6.4 | Table(虚拟滚动/排序/列宽)、Dock、**command(命令面板)**、sidebar、tree、chart/plot、Sheet/Dialog、Rope+Tree-sitter 代码编辑器 |
| K8s 客户端 | `kube` | 4.x | features: `runtime, ws, gzip, oidc, http-proxy`。**`http-proxy` 不是可选项**：设了 `HTTPS_PROXY` 而没开这个 feature，kube 会直接拒绝连接而不是回退成直连 |
| K8s 类型 | `k8s-openapi` | 0.28 | feature `latest`，仅用于按需强类型解析。**注意 0.28 的时间戳用 `jiff` 而非 chrono** |
| 异步 | `tokio` | 1.x | `rt-multi-thread, net, macros, sync` |
| 桥接 channel | `futures` | 0.3 | `futures::channel::mpsc` 跨 runtime 安全 |
| 模糊搜索 | `nucleo-matcher` | 0.3 | Helix/Zed 同款算法。用的是 `nucleo` 里的**匹配器**而不是完整的 `nucleo`：后者带一套多线程增量前端，而这里的列表本来就在内存里，每个视图起一个线程池不值 |
| 终端模拟 | `alacritty_terminal` | 0.25 | exec 终端的 VT 解析（M5 才引入） |
| 错误 | `anyhow` + `thiserror` | — | 领域层 thiserror，UI 层 anyhow |
| 日志 | `tracing` + `tracing-subscriber` | — | 文件 + 可开关的面板输出 |

> 注：crates.io 上另有一个独立的 `gpui 0.2.x`，**它不是 gpui-component 用的那个**——
> `gpui-component 0.6.4` 依赖的是 `gpui-pre 0.3.5`。同时写 `gpui = "0.2"` 和
> `gpui-component = "0.6"` 会拉进两套互不相干的 GPUI，类型不通用。正确做法是只依赖
> `gpui-kit`，用 `gpui_kit::*`（即 GPUI）和 `gpui_kit::component::*`。
> API 仍在快速演进，**必须提交 `Cargo.lock` 并 pin 到精确补丁版本**，升级当成独立任务做。

### 跨平台现状（必须早验证，不要留到最后）

- **macOS**：最成熟，Metal 后端，开发主力平台。
- **Linux**：X11/Wayland 可用，字体渲染和输入法需要实测。
- **Windows**：gpui-component 声明支持 x86_64；实际渲染/IME/文件对话框需要自己跑一遍。

**行动项：M0 就把 GitHub Actions 三平台 build matrix 建起来**，哪怕只 build 一个空窗口。
跨平台问题越晚发现代价越高，尤其是 IME、剪贴板、窗口装饰这类 GPUI 平台层差异。

---

## 2. Workspace 结构

```
beacon/
├── Cargo.toml                 # workspace，统一 [workspace.dependencies]
├── Cargo.lock                 # 必须提交
├── crates/
│   ├── beacon-kube/           # 领域层：无 UI 依赖，可独立单测
│   │   ├── config.rs          #   kubeconfig 解析、context 枚举、认证
│   │   ├── session.rs         #   ClusterSession：一个集群的全部运行时状态
│   │   ├── discovery.rs       #   GVK 发现 + 能力(verbs/scope)缓存
│   │   ├── watch.rs           #   按需 watch + 引用计数 + 合批
│   │   ├── store.rs           #   ResourceStore + Delta
│   │   ├── ops.rs             #   delete / scale / apply(SSA) / cordon / evict
│   │   ├── logs.rs            #   日志流 + 环形缓冲
│   │   ├── exec.rs            #   exec/attach 会话
│   │   ├── forward.rs         #   port-forward 管理器
│   │   └── access.rs          #   SelfSubjectAccessReview 权限预检
│   ├── beacon-columns/        # 列定义：内置表 + CRD printer columns + JSONPath 求值
│   ├── beacon-term/           # 终端模拟（M5）
│   ├── beacon-ui/             # GPUI 视图层
│   │   ├── app.rs             #   根视图、Dock 布局、全局 action
│   │   ├── bridge.rs          #   tokio ↔ gpui 桥
│   │   ├── palette.rs         #   Cmd+K 命令面板
│   │   ├── sidebar.rs         #   集群/命名空间/资源树
│   │   ├── table.rs           #   通用资源表格
│   │   ├── detail/            #   详情面板：Overview / YAML / Events / Logs / Shell
│   │   └── theme.rs           #   主题 token
│   └── beacon/                # bin：装配 + 平台入口
└── docs/DESIGN.md
```

**分层铁律**：`beacon-kube` 不得依赖 `gpui`。它应该能被一个 CLI 二进制复用——这既是架构约束，
也是测试手段（很多逻辑用 headless 测试比用 UI 测试快十倍）。

---

## 3. 异步桥接：整个项目最关键的 200 行

### 3.1 为什么不能混用

`kube` → `hyper` → 需要 tokio 的 reactor 在当前线程上下文中注册 I/O。
GPUI 的 `background_executor()` 不是 tokio runtime，在里面直接 poll kube future 会 panic
（"there is no reactor running"）。

### 3.2 方案

进程内维护一个**全局 multi-thread tokio Runtime**，所有网络任务在其中；
跨界通信用 `futures::channel::mpsc`（runtime 无关）。

```rust
// beacon-ui/src/bridge.rs
pub struct Bridge {
    rt: Arc<tokio::runtime::Runtime>,
}

impl gpui::Global for Bridge {}

impl Bridge {
    /// 在 tokio 侧启动一个长任务，把它产生的消息流接到某个 gpui Entity 上。
    /// 消息在 tokio 侧已经合批，一次 update 对应一帧。
    pub fn stream_into<T, M, F, Fut>(
        &self,
        entity: &Entity<T>,
        cx: &mut Context<T>,
        spawn: F,
        mut apply: impl FnMut(&mut T, M, &mut Context<T>) + 'static,
    ) -> Task<()>
    where
        T: 'static,
        M: Send + 'static,
        F: FnOnce(mpsc::UnboundedSender<M>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.rt.spawn(spawn(tx));

        // gpui 0.2.x: AsyncFnOnce(WeakEntity<T>, &mut AsyncApp) -> R
        cx.spawn(async move |this, cx| {
            while let Some(msg) = rx.next().await {
                if this.update(cx, |view, cx| apply(view, msg, cx)).is_err() {
                    break; // 视图已销毁，任务自然收敛
                }
            }
        })
    }
}
```

### 3.3 三条必须遵守的规则

1. **前台永不阻塞**：任何 `block_on`、同步文件 IO、YAML 序列化大对象，都丢到
   `cx.background_executor()` 或 tokio 侧。
2. **合批在 tokio 侧做**，不在 UI 侧做。一个 5000 pod 的集群初次 list 会产生 5000 个事件，
   逐个 `cx.notify()` 会让界面卡死几秒。做法：

   ```rust
   // beacon-kube/src/watch.rs —— 时间窗合批
   let mut pending = Vec::new();
   let mut tick = tokio::time::interval(Duration::from_millis(16));
   loop {
       tokio::select! {
           Some(ev) = stream.next() => {
               pending.push(to_delta(ev));
               if pending.len() >= 512 { flush(&tx, &mut pending); }
           }
           _ = tick.tick(), if !pending.is_empty() => flush(&tx, &mut pending),
       }
   }
   ```
3. **视图销毁即任务取消**：`this.update()` 返回 `Err` 时退出循环；
   `ClusterSession` 内部用 `JoinSet`，Drop 时整组 abort。

---

## 4. 数据层设计

### 4.1 ClusterSession

一个 kubeconfig context = 一个 `ClusterSession`。多集群 = 多个 session 并行，互不影响。

```rust
pub struct ClusterSession {
    pub id: ClusterId,                     // context name，全局唯一键
    client: kube::Client,
    discovery: Arc<RwLock<DiscoveryCache>>,// GVK -> (ApiResource, ApiCapabilities)
    watches: Mutex<HashMap<WatchKey, WatchHandle>>,
    tasks: JoinSet<()>,
    health: watch::Sender<Health>,         // Connected / Degraded / Reconnecting(err)
}

#[derive(Hash, Eq, PartialEq, Clone)]
pub struct WatchKey {
    gvk: GroupVersionKind,
    namespace: Option<String>,             // None = all namespaces
    selector: Option<String>,
}
```

**按需订阅 + 引用计数**：

```rust
impl ClusterSession {
    pub fn subscribe(&self, key: WatchKey) -> Subscription {
        // 已有 watch → refcount += 1，立刻把 store 快照作为 Delta::Reset 发给新订阅者
        // 无 → 启动 watcher 任务
    }
}
// Subscription Drop -> refcount -= 1；归零后延迟 30s 再真正停止
// （用户在资源类型之间来回切换时不重建 watch，避免反复全量 list）
```

### 4.2 动态资源与 Store

```rust
pub struct ResourceStore {
    objects: HashMap<ObjectRef, Arc<DynamicObject>>,
    rev: u64,
}

pub enum Delta {
    Reset(Vec<Arc<DynamicObject>>),   // 初次 list 或 watch desync 后重建
    Upsert(Arc<DynamicObject>),
    Remove(ObjectRef),
}
```

UI 侧只持有 `Arc<DynamicObject>`，克隆是一次原子加。

**内存优化（大集群必做）**：写入 store 前剥离两样东西，能省 40%~60% 内存：

```rust
fn slim(mut obj: DynamicObject) -> DynamicObject {
    obj.metadata.managed_fields = None;
    if let Some(a) = obj.metadata.annotations.as_mut() {
        a.remove("kubectl.kubernetes.io/last-applied-configuration");
    }
    obj
}
```
查看 YAML 时再单独 GET 一次完整对象即可。

### 4.3 watch 的 desync 处理

`kube::runtime::watcher` 已经处理了 410 Gone → 重新 list → 发 `Event::Init/InitApply/InitDone`。
我们把 `InitDone` 翻译成 `Delta::Reset`，UI 直接整表替换。
K8s ≥ 1.27 可以打开 `Config::default().streaming_lists()`，初次加载走 watch bookmark，
大集群首屏快很多。**但它依赖 API server 的 `WatchList` feature gate，没开的集群是整个 list 失败
而不是降级**，所以要先做一次能力探测再打开。M1 暂时走 `ListWatch` + `page_size(500)`。

### 4.4 列定义（`beacon-columns`）

```rust
pub struct ColumnSet { pub columns: Vec<ColumnDef> }

pub struct ColumnDef {
    pub header: String,
    pub width: ColumnWidth,
    pub source: ColumnSource,
}

pub enum ColumnSource {
    /// CRD 的 additionalPrinterColumns，运行时 JSONPath 求值
    JsonPath(String),
    /// 内置资源的特殊计算：Pod 的 Ready/Status/Restarts、Deployment 的 Up-to-date 等
    Computed(fn(&DynamicObject) -> CellValue),
    Age,
    Name,
    Namespace,
}
```

解析顺序：内置表 → CRD printer columns → 兜底（Name/Namespace/Age）。
`Computed` 里按需 `serde_json::from_value::<Pod>(...)`，只对当前可见的几十行做，不对全量做。

### 4.5 权限预检

切换 namespace 时批量发一次 `SelfSubjectRulesReview`，缓存结果。
UI 上没权限的动作**置灰并给 tooltip**，而不是点了之后弹 403。这是体验上和 Lens 拉开差距的细节。

---

## 5. 认证：必须专门处理的坑

kube-rs 支持 kubeconfig 里的 exec credential plugin（`aws eks get-token`、`gke-gcloud-auth-plugin`、
`azure kubelogin`）。**但 GUI 应用从 Finder / 开始菜单启动时，进程 PATH 不包含
`/opt/homebrew/bin`、`~/.local/bin` 等目录，plugin 会找不到而报一个很难懂的错。**

解法（按优先级）：
1. 启动时执行 `$SHELL -lc 'echo $PATH'` 拿到用户登录 shell 的 PATH，注入自身环境（Zed 就是这么做的）；
2. 设置里允许用户追加自定义 PATH；
3. 认证失败时给出**可诊断的错误**：显示实际执行的命令、PATH、stderr，而不是 "authentication failed"。

其余认证方式（client cert、token、oidc）kube-rs 直接支持，注意 token 过期要能刷新后自动重连。

---

## 6. UI 架构

### 6.1 布局

```
┌──────────────────────────────────────────────────────────────┐
│ [集群 ▾ prod-us-west]  [ns ▾ default]   ⌘K   ● Connected     │  顶栏
├────────────┬─────────────────────────────────────────────────┤
│ ★ 收藏      │  Pods  (1,284)            [搜索…]  [刷新] [+]   │
│ Workloads  │ ┌─────────────────────────────────────────────┐ │
│  Pods      │ │ Name        Ready  Status   Restarts  Age   │ │
│  Deploy…   │ │ api-7f9…    2/2    Running  0         3d    │ │
│ Config     │ │ …                                           │ │  uniform_list
│ Network    │ └─────────────────────────────────────────────┘ │  虚拟滚动
│ Storage    ├─────────────────────────────────────────────────┤
│ CRDs ▸     │ Overview │ YAML │ Events │ Logs │ Shell         │  详情 Dock
│            │ …                                               │
└────────────┴─────────────────────────────────────────────────┘
```

用 `gpui-component` 的 `DockArea`：左侧 Panel + 中间 Tabs + 底部可折叠详情，布局可序列化保存。

### 6.2 命令面板（⌘K）—— 一等公民

Lens 的交互重心在鼠标；Beacon 的差异化定位是**键盘优先**。面板统一入口：

- `>` 执行动作（delete / restart / scale / port-forward…）
- `@` 跳转资源类型（含 CRD）
- `#` 切 namespace
- `ctx ` 切集群
- 直接输入 = 在当前资源类型内模糊搜索（`nucleo`）

gpui-component 自带 `command` 模块（命令面板）和 `sidebar` / `tree`，先在它上面搭，
只把 Beacon 特有的前缀路由（`>` / `@` / `#` / `ctx `）和 `nucleo` 匹配接上去。

### 6.3 表格性能

- 行虚拟化：`uniform_list`，只渲染可见行 + overscan。
- 视图模型只存 `Vec<ObjectRef>`（过滤排序后的索引），不存对象副本。
- 过滤/排序在 `background_executor` 上做，结果回前台整体替换；输入框 debounce 100ms。
- `cx.notify()` 只在一批 delta 应用完后调用一次。

### 6.4 详情面板

| Tab | 实现要点 |
|---|---|
| Overview | 从 `DynamicObject` 按需解析成强类型渲染；Pod 显示容器/探针/QoS/挂载 |
| YAML | gpui-component 代码编辑器 + Tree-sitter YAML；编辑后走 **Server-Side Apply**（`Patch::Apply` + fieldManager `beacon`），冲突时展示 diff 并提供 force 选项 |
| Events | 对该对象 `field_selector=involvedObject.uid=…` 单独 watch |
| Logs | `Api::log_stream` + `follow=true`；环形缓冲上限（如 10MB / 50k 行）；支持多容器、previous、时间戳、grep 高亮、下载 |
| Shell | M5，见下 |

---

## 7. 终端与 port-forward（M5）

### exec 终端

```
Api::exec(name, cmd, AttachParams::interactive_tty())
  -> AttachedProcess { stdin: AsyncWrite, stdout: AsyncRead, ... }
  -> alacritty_terminal::Term 解析 VT 序列，维护 grid
  -> GPUI 渲染 grid（等宽字体 + 每格背景/前景色）
  -> 键盘事件编码回 stdin；窗口 resize -> AttachedProcess 的 terminal_size channel
```
这是整个项目**工作量最大的单点**（终端渲染、选区、复制粘贴、滚动缓冲、颜色）。
建议：M5 先做只读的 `kubectl exec -- <一次性命令>` 输出展示，完整交互终端放 M6。
备选方案：直接调用系统终端执行 `kubectl exec -it`，先解决有无问题。

### port-forward

```rust
// 本地监听 -> 每个连接建一条 portforward 流
let listener = TcpListener::bind(("127.0.0.1", local_port)).await?;
let pf = api.portforward(&pod, &[remote_port]).await?;
tokio::io::copy_bidirectional(&mut tcp_stream, &mut pf.take_stream(remote_port).unwrap()).await
```
UI 里一个"转发列表"面板管理生命周期，Pod 消失时自动关闭并提示。

---

## 8. 里程碑

| 阶段 | 内容 | 验收标准 | 估时 |
|---|---|---|---|
| **M0** ✅ | workspace 骨架、三平台 CI、空窗口、主题 token、tracing、登录 shell PATH 恢复 | 三平台各产出一个能开的窗口（macOS 已验证，Linux/Windows 待 CI 首跑） | 1w |
| **M1** ✅ | kubeconfig 解析、context 切换、Pod 列表 + watch、tokio 桥 | 输出与 `kubectl get pods -A` 逐格一致；切 context/namespace 不泄漏任务。5000 pod 的 60fps 未实测，见附录 | 2w |
| **M2** ✅ | Discovery、通用表格、列定义、CRD 支持、namespace 过滤、搜索 | 17 种资源（含 4 个 CRD）与 `kubectl get` 逐格一致；CRD 的 printer columns 运行时读取，无需改代码 | 2w |
| **M3** | Dock 布局、详情面板（Overview/YAML 只读/Events）、命令面板 | ⌘K 可完成 90% 的导航操作 | 2w |
| **M4** | 日志流、写操作（delete/scale/restart/SSA apply）、权限预检 | 无权限动作正确置灰；apply 冲突有 diff | 2w |
| **M5** | port-forward、exec（先一次性命令）、多集群并行 | 同时连 3 个集群内存 < 400MB | 3w |
| **M6** | 交互式终端、metrics（CPU/内存图表）、Helm release 列表 | — | 4w+ |

M1→M4 完成即是一个**日常可用**的只读为主客户端，这是最该追求的第一个可用里程碑。

---

## 9. 风险清单

| 风险 | 影响 | 缓解 |
|---|---|---|
| GPUI API 不稳定（0.2.x） | 升级时大面积改动 | pin 补丁版本；UI 封装薄，业务逻辑全在 `beacon-kube` |
| Windows/Linux 渲染或 IME 问题 | 跨平台目标落空 | M0 就建三平台 CI，每个里程碑都在三平台手测一次 |
| 缺终端/图表组件 | M5/M6 工作量翻倍 | 先用降级方案（调系统终端、用简单折线图） |
| 大集群内存 | 卡顿/OOM | slim 对象、按需订阅、上限保护（> N 对象提示加过滤器） |
| exec plugin PATH | 一部分用户完全连不上 | §5 的 shell PATH 注入 + 可诊断错误 |
| 多集群并发把 API server 打爆 | 被限流 | 每集群限流（`tower` limit 层）、退避重试、可配置并发 watch 上限 |

---

## 10. 待定的产品决策

1. **Helm 支持**：调 `helm` CLI（简单、可靠）还是解析 Secret 里的 release 数据（无外部依赖）？建议前者。
2. **指标来源**：metrics-server（`metrics.k8s.io`，需自定义 Rust 类型，k8s-openapi 不含）还是 Prometheus？建议先做前者。
3. **是否做插件系统**：Lens 的插件是它的护城河。若要做，早期就得把 UI 抽象成可扩展的 panel registry，代价不小。建议 v1 不做。


---

## 附：M0 实现记录（2026-09-20）

已落地，`cargo test --workspace` 14 passed，`cargo clippy --workspace --all-targets` 干净，
macOS 上 `./target/debug/beacon` 能开窗并正确读出 kubeconfig。

实现过程中修正了本文最初的三处事实错误：

1. **GPUI 的依赖方式**：crates.io 上的 `gpui 0.2.x` 与 `gpui-component` 用的不是同一套。
   `gpui-component 0.6.4` 依赖 `gpui-pre 0.3.5`。只依赖 `gpui-kit`，见 §1。
2. **k8s-openapi 0.28 用 jiff**，不是 chrono。`Time` 内是 `jiff::Timestamp`。
3. **gpui-component 已自带** `command`（命令面板）、`sidebar`、`tree`、`chart`/`plot`，
   §6.2 不必从零实现。

尚未验证的事项，按优先级：

- Linux / Windows 能否编译和渲染 —— CI 首次运行时揭晓，`.github/actions/linux-deps`
  的 apt 列表是按 GPUI 的 x11/wayland/font-kit/vulkan feature 推的，可能需要按报错增补。
- 冷启动 500ms 目标：当前 debug 构建约 1.1s 到窗口可见，其中登录 shell 查询占 ~180ms。
  release 构建和把 shell 查询挪到后台（只在首次连接前阻塞）都还没做。


---

## 附：M1 实现记录（2026-09-20）

`cargo test --workspace` 75 passed，`cargo clippy --workspace --all-targets` 干净。

### 落地的东西

| 位置 | 内容 |
|---|---|
| `beacon-kube/session.rs` | `ClusterSession`：按 context 建 client、用 `/version` 探活、健康状态（Connecting/Connected/Degraded）。健康是**计数**而不是闩锁——一个没权限的 watch 不该让整个集群看起来是坏的 |
| `beacon-kube/watch.rs` | 按需 watch：引用计数、30s 延迟关闭、16ms/512 条合批。事件→Delta 的翻译抽成了 `Coalescer`，可以脱离网络单测 |
| `beacon-kube/store.rs` | `ResourceStore` + `Delta`，`slim()` 剥 managedFields 和 last-applied |
| `beacon-kube/error.rs` | 连接失败展开整条 error chain；识别出是 exec 插件跑不起来时附上当前 PATH（§5） |
| `beacon-columns/pod.rs` | kubectl `printPod` 的移植：READY / STATUS / RESTARTS，含 sidecar、init 容器、Terminating、NodeLost、`5 (8d ago)` |
| `beacon-columns/path.rs` | printer columns 的点号子集求值；filter/通配符一律返回"无"，而不是返回一个看起来像对的值 |
| `beacon-ui/table.rs` | 基于 gpui-component `TableState` 的通用资源表，虚拟滚动 + 列排序 |
| `beacon-ui/cluster.rs` | 一个 `ClusterView` = 一个 session。换 context 就是换 entity，session 的 Drop 把 watch 全 abort 掉 |
| `beacon-kube/examples/watch.rs` | 无窗口跑同一套逻辑。"`beacon-kube` 不依赖 GPUI"这条铁律的兑现，也是改动最快的验证手段 |

### 实测

本机 k3s v1.33.3，39 个 pod（含 Completed / Error / 636 次重启 / Unknown）。
`examples/watch` 的输出与 `kubectl get pods -A` **逐行逐格 diff 为空**。
窗口截图确认了全命名空间和单命名空间两种列布局。

### 又踩到的坑

1. **`gpui_kit::*` 会把 GPUI 的 `#[test]` 带进测试模块**，遮蔽 Rust 自带的，报错是
   "recursion limit reached while expanding `#[test]`"。测试模块按名字导入，别 `use super::*`。
   （gpui-kit 的 lib.rs 里写了这件事，但错误信息完全看不出来。）
2. **gpui-component 的 Table 列宽是固定像素，没有 flex**。`ColumnWidth::Flex` 在 UI 层折算成
   一个初始像素宽（列本身可拖拽）。想要真正的自适应要自己算可用宽度。
3. kube 的 `http-proxy`，见 §1。

### 明确没做 / 没验的

- **5000 pod 的 60fps 没有实测**——手上只有 39 个 pod 的集群。能说的只有：5000 条目的索引重建
  加上按计算列排序，在 dev profile 下是个位数毫秒（`beacon-ui` 整个测试套件含 4 次 5000 级重建
  跑完是 0.03s），渲染本身由 `uniform_list` 虚拟化。真机大集群仍需验证，这是 M2 的第一件事。
- **按计算列排序在大集群上是前台全量 resolve**：每来一批 delta 就对全量对象算一次 Status/Restarts。
  §6.3 说的"过滤/排序放 background_executor"还没做，M2 补。
- 搜索框（M2）、CRD printer columns 的接线（M2，求值器已就位）、Node/Event 常驻 watch。
- **命名空间选择器只能选它列得出来的**。多租户集群上用户往往没有集群级
  `list namespaces` 权限——现在的表现是选择器只剩"All namespaces"、状态栏显示 Degraded
  并给出原因，不会假装正常，但也没法手输一个自己有权限的命名空间。M4 做权限预检时一并解决。


---

## 附：M2 实现记录（2026-09-23）

`cargo test --workspace` 127 passed，`cargo clippy --workspace --all-targets` 干净。

### 落地的东西

| 位置 | 内容 |
|---|---|
| `beacon-kube/discovery.rs` | 聚合 discovery（一两个请求拿全量，不行再退回逐 group 查）；只保留 **list + watch 都支持**的 kind——整套架构建在 watch 上，watch 不了的 kind 给出来就是一张永远空的表 |
| 同上 | CRD 的 printer columns **按需单个 GET**（`<plural>.<group>`），不 list 全部：一个 CRD 带着整份 OpenAPI schema，为了四个列名搬几 MB 不划算。结果按 GVK 缓存，"没有"也缓存 |
| `beacon-columns/builtin.rs` | kubectl 内置表的移植：Pod / Deployment / StatefulSet / DaemonSet / ReplicaSet / Job / CronJob / Service / Ingress / Node / Namespace / ConfigMap / Secret / ServiceAccount / PVC / RoleBinding |
| `beacon-columns/printer.rs` | CRD 自述的列：跳过 `priority > 0`（那是 `-o wide` 的东西），`type: date` 渲染成时长而不是时间戳 |
| `beacon-columns/path.rs` | JSONPath 子集扩到了**过滤器和通配符**：`conditions[?(@.type=="Accepted")].status` 和 `addresses[*].value` 是真实 CRD 里最常见的两种写法，只支持点号路径的话 gateway-api 这类 CRD 最重要的那列会是空的 |
| `beacon-ui/catalog.rs` | 98 个 kind 分成 Workloads / Config / Network / Storage / Access Control / Cluster / Custom Resources；分组是张表不是规则——`Lease` 是协调原语、`Endpoints` 是网络，API 里没有任何字段这么说 |
| `beacon-ui/cluster.rs` | 侧边栏选 kind → 换 `WatchKey` + `ColumnSet`，其余自动跟上。列先用本地已知的立刻出表，CRD 自述的列到了再换上去，**不重新 list** |
| `beacon-ui/table.rs` | 行过滤（模糊，匹配 `namespace/name`）。没选排序列时按匹配分排，选了就尊重用户选的 |

### 实测

对本机 k3s v1.33.3，把 `examples/watch --once` 的输出和 `kubectl get` 逐格 diff：

**17 种资源完全一致**，包括 4 个 CRD（Application / Addon / Workflow / WorkflowTemplate）——
其中 Application 正确隐藏了 priority=10 的两列，Addon 正确地没有 Age 列（CRD 没声明）。
脚本在 `/tmp` 里没留，逻辑是：Beacon 输出 NAME 在前、kubectl `-A` 输出 NAMESPACE 在前，换列后排序再 diff。

### 与 kubectl 的两处有意分歧

1. **空值**：kubectl 在内置表里用 `<none>`，但 CRD 的列和 PVC 的 storageclass 这些地方直接留空。
   Beacon 一律 `<none>`。GUI 里的空单元格和"渲染挂了"长得一样。
2. **Age vs Created At**：kubectl 对 Role/ClusterRole 和没声明列的 CRD 打的是 `CREATED AT`（完整时间戳）。
   Beacon 一律给 Age。一个窗口里并排十五种资源的时候，和其他行一致的相对时间比精确字符串有用。

### 新踩的坑

1. **gpui-component 的 `TableState` 会缓存列布局**。换了 `ColumnSet` 只 `notify()` 没用——
   表头保持上一个 kind 的形状，多出来的列根本不画。必须调 `state.refresh(cx)`。
   （症状是 Pod 表只显示 Name/Namespace/Ready 三列，剩下三列凭空消失。）
2. **`type: date` 的转换在服务端**。apiextensions 的 tableconvertor 把这类值转成时长*再*返回，
   所以 kubectl 看到的已经是 `5d`。我们直接读对象，得自己转。
3. **watch 的"列完了"和"本来就是空的"必须能区分**。原来 registry 给新订阅者先发一个空 `Reset`
   当作预热，结果订阅者没法区分这两件事——空命名空间会先闪一下"没有内容"。
   现在只有在初次 list 完成后才预热，`Reset` 一律意味着"这就是全部"。

### 明确没做 / 没验的

- **本会话没能点**：侧边栏点击、搜索框输入这两条路径是代码正确、但没有真的点过——
  这台机器上的 computer-use 工具在这次会话里断开了。变通验证过 CRD 那条：临时把启动 kind
  改成 `Application` 跑了一次，截图确认列是 CRD 自述的那四列，然后改回来。
- **排序/过滤仍然在前台线程**。§6.3 说要搬到 `background_executor`，暂时没搬，因为测下来
  5000 条目的索引重建 + 按计算列排序 + 连打 7 个字符的模糊过滤，整个 `beacon-ui` 测试套件
  跑完是 0.03s。等有真实大集群数据再说。
- 收藏夹、列的显示/隐藏、`-o wide` 那些 priority > 0 的列。
- `..` 递归下降和非等值过滤器的 JSONPath——printer columns 里没见过，遇到了一律当"没匹配上"。
