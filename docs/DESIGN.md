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
| **M3** ✅ | Dock 布局、详情面板（Overview/YAML 只读/Events）、命令面板 | ⌘K 覆盖了应用里**全部**四种导航（kind / namespace / cluster / 对象），详情面板三个 tab 都跑通 | 2w |
| **M4** ✅ | 日志流、写操作（delete/scale/restart/SSA apply）、权限预检 | 无权限动作带原因置灰（截图验证）；apply 冲突对真实 API server 验证过（dry run，未写入） | 2w |
| **M5** ✅ | port-forward、exec（先一次性命令）、多集群并行 | 转发真的通了（curl 过去拿到 argocd-server 的响应）；exec 与 kubectl 一致；多集群按 session 缓存，内存未在 3 集群下实测 | 3w |
| **M6** ✅ | 交互式终端、metrics、Helm release 列表 | 容器里真的开出了 shell（提示符、命令、输出、ANSI 颜色）；CPU/内存与 `kubectl top` 一致；Helm 列表与 `helm list -A` 一致 | 4w+ |

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

1. **Helm 支持**：~~建议调 CLI~~ → **决定解析 Secret**。理由在真正写到这一步才清楚：Beacon 已经有一个
   认证好的、能用的 client，而调 CLI 会把 §5 那个 PATH 问题原样搬回来——从 Finder 启动的 GUI
   同样找不到 `helm`。见 `beacon-kube/helm.rs`。
2. **指标来源**：**决定 metrics-server**，按建议做了。`metrics.k8s.io` 的类型确实要自己写。
   Prometheus 没做。
3. **是否做插件系统**：**决定不做**，按建议。


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


---

## 附：M3 实现记录（2026-09-23）

`cargo test --workspace` 147 passed，`cargo clippy --workspace --all-targets` 干净。

### 落地的东西

| 位置 | 内容 |
|---|---|
| `beacon-ui/palette.rs` | 命令面板。前缀即路由：无前缀=当前 kind 内的对象，`@`=资源类型，`#`=命名空间，`ctx `=集群，`>`=命令。段内用 nucleo 模糊匹配，列表截到 50 条——5000 个 pod 里没人读第 51 行，他们会继续打字 |
| `beacon-ui/detail.rs` | 详情面板三个 tab。Overview 读表格里已有的那份（元数据、标签、注解、Pod 的容器、status 的标量字段）；YAML **重新拉一次完整对象**——store 里是 slim 过的，而这个 tab 的全部意义就是被剥掉的那部分；Events 用 `involvedObject.uid` 单独起一个 watch |
| `beacon-kube/session.rs` | `get_object`：按需取完整对象 |
| `beacon-kube/watch.rs` | `WatchKey::events_about(uid, ns)`。按 **UID** 而不是名字：Deployment 的 pod 名字会被回收，上一个占用这个名字的对象的事件不是这个对象的事件 |
| `beacon-columns/event.rs` | Event 的读法。这是唯一一张**没有 Name 列**的内置表——事件的名字是个带时间戳的哈希，kubectl 也不打它 |
| `beacon-ui/cluster.rs` | 表格/详情的可拖分栏，选中行即打开详情，换 kind 关掉详情 |

### 与设计的三处分歧

1. **用 `v_resizable` 而不是 `DockArea`**（§6.1）。现在只有一个底部面板要排，DockArea 的可序列化多面板布局
   等到真有多个面板（M5 的终端、port-forward 列表）再上。分栏位置已经跨"关掉再打开"保留。
2. **YAML 没有语法高亮**。gpui-component 0.6.4 的 `tree-sitter-yaml` feature 依赖一个
   **还没发布的 `tree-sitter` 版本**（要 0.26.13，crates.io 上最新 0.26.12），开不了。
   代码照样 `.language("yaml")`——未知语言会退化成纯文本而不是 panic——所以将来打开 feature 是改一行。
3. **Overview 不是"按 Pod 强类型渲染"那么细**（§6.4 说的探针/QoS/挂载）。现在是：元数据 + Pod 的容器
   （镜像、状态、重启次数）+ `status` 的标量字段拍平一层。最后一条是通用的，Deployment 的副本数、
   Service 的 clusterIP、PVC 的 phase 都能看见，性价比比逐个 kind 写渲染器高得多。

### 新踩的坑

1. **`Command` 的 `on_query` 回调是渲染时才装上的**。在面板第一次渲染之前调 `set_query`，
   重算根本不会发生。真实使用没问题（用户打字时早就渲染过了），但写验证脚本时会得到一个
   "No results found" 然后怀疑人生。
2. **Event 的"最后一次发生"藏在四个字段里**：`series.lastObservedTime` → `lastTimestamp` →
   `eventTime` → `firstTimestamp`。顺序读错的话，一个连续失败了一周的 pod 会显示成五天前的事。
3. **kubectl 把 Event 的 involvedObject kind 小写**（`pod/api-7f9` 而不是 `Pod/api-7f9`），
   因为那是你要打回 `kubectl get` 里的形式。

### 明确没做 / 没验的

- **⌘K 这个键本身没按过**。和 M2 一样，这台机器上的输入自动化工具在会话里不可用。
  变通验证：临时让面板在连接后自动打开并预填查询，把 `@`、`#`、`>` 三个 section 各截了一张图，
  确认分组标题、列表内容和顺序都对，然后把临时代码删干净。前缀路由和排序本身有单测覆盖。
- **YAML 只读**。编辑要走 Server-Side Apply，那是 M4。
- `>` 段目前只有四个动作（切主题、开关详情、清过滤、复制名字）——写操作（delete/scale/restart）
  要等 M4 的权限预检一起做，现在放进去就是一个点了会 403 的菜单。
- 侧边栏的 ★ 收藏（§6.1 画了）、面板布局持久化到磁盘。


---

## 附：M4 实现记录（2026-09-23）

`cargo test --workspace` 179 passed，`cargo clippy --workspace --all-targets` 干净。

### 落地的东西

| 位置 | 内容 |
|---|---|
| `beacon-kube/access.rs` | 权限预检。用 `SelfSubjectRulesReview` 一次拿到一个命名空间的全部规则，而不是每个按钮一次 `SelfSubjectAccessReview`——后者是每行每动词一个往返 |
| `beacon-kube/ops.rs` | delete / restart / scale / apply，以及冲突消息的解析 |
| `beacon-kube/logs.rs` | 日志流 + 环形缓冲（5 万行 / 10MB，丢最旧的），按 16ms 合批，和 watch 一样 |
| `beacon-ui/actions.rs` | 某个 kind 上能做什么，每项标好这个用户能不能做 |
| `beacon-ui/prompt.rs` | 删除前的确认、扩缩容的数字输入 |
| `beacon-ui/detail.rs` | Logs tab；YAML 变成可编辑 + Apply + 冲突展示 |
| `beacon-kube/examples/watch.rs` | `--apply-check`：**dry-run** 的 SSA，用来对真实 API server 验证冲突路径而不写入任何东西 |

### 两条关于 RBAC 的事实，都很容易搞错

1. **`pods` 上的规则不覆盖 `pods/log`**。子资源在 RBAC 里是分开命名的，所以"能列 pod"
   完全没说"能看日志"。`deployments` 和 `deployments/scale` 同理——这就是截图里
   "Scale 被置灰但 Restart 没有"那种情况存在的原因。
2. **答案可以是"不完整"的**。webhook 授权器没法枚举自己的规则，这时 `incomplete: true`。
   Beacon 的选择是**照常启用**：一个用户能读懂的 403，好过一个谁也解释不了的灰按钮。

### 真机验证

- **权限置灰**：两张截图对照。管理员身份下 `>` 里 "Delete" 正常可选；把规则换成只读后
  变成 "Delete — You do not have delete on pods in this namespace"，灰掉且不可确认。
- **apply 冲突**：`--apply-check Deployment default/guestbook-ui` 对本机 k3s 跑 dry-run apply，
  真实拿到 argocd-controller 的冲突并解析出 manager 和字段；同一命令对没有冲突的对象
  返回 "dry run applied cleanly"。**全程没有写入**（复查过 replicas 没变）。
- **日志**：`argo-workflows-server` 的实时日志，2001 行，跟到尾部。

### 踩到的坑

**冲突消息有两种格式，而且哪里都没写。** 我按文档印象实现了多行的那种：

```text
Apply failed with 2 conflicts: conflicts with "kubectl-client-side-apply" using v1:
- .spec.replicas
- .metadata.labels.team
```

对着真集群一跑，**单个冲突是一行**，字段跟在冒号后面，而且是单数的 `conflict with`：

```text
Apply failed with 1 conflict: conflict with "argocd-controller" using apps/v1: .spec.replicas
```

只按多行格式解析的话，最常见的情况（就冲突了一个字段）会解析出空的 manager 和空的字段列表，
界面上只剩一句"这个 apply 冲突了"。两种格式现在都有测试，用的是从真集群抓来的原文。

### 与设计的分歧

**冲突展示的是"字段 + 谁拥有 + 你填的值"，不是整对象的 unified diff**（§6.4 写的是"展示 diff"）。
理由是前者才是要做的决定：`.spec.replicas` 归 argocd-controller，你想改成 3。
整个对象的 diff 里，这一行会淹没在一百行没变的 YAML 中间。
API server 写列表键用 `containers[name="app"]` 这种语法，`beacon-columns` 的求值器不认，
那种字段只显示路径不显示值——还是有用的那一半。

### 明确没做 / 没验的

- **按键仍然没按过**（连续第三个里程碑）：这台机器上的输入自动化工具不可用。
  用临时插桩把面板/详情/只读规则预置好截图，然后把插桩删干净（`grep BEACON_DEMO` 为空）。
- **真正的写入没在真集群上执行过**。delete / restart / scale / 非 dry-run 的 apply 都只有
  单测和 dry-run 覆盖——在别人的开发集群上点"删除"不是我该替他做的决定。
- 日志的 grep 高亮和下载到文件（§6.4 列了）、`--force-conflicts` 之外的冲突合并策略。
- 权限缓存没有失效机制：RoleBinding 改了要重连才知道。


---

## 附：M5 / M6 实现记录（2026-09-23）

`cargo test --workspace` 221 passed，`cargo clippy --workspace --all-targets` 干净。

### 落地的东西

| 位置 | 内容 |
|---|---|
| `beacon-kube/forward.rs` | port-forward。一个本地监听器，每条连接一条独立隧道（`kubectl port-forward` 也是这样，隧道是一条到 API server 的 WebSocket，多路复用会把流搅在一起）。转发属于 **session** 而不是视图——它存在的意义就是让你把浏览器指过去，切个资源就断掉是没法用的 |
| `beacon-kube/exec.rs` | 一次性命令 + 输出。带超时和输出上限 |
| `beacon-kube/terminal.rs` | 交互式会话的管道：一个字节流出来，两个 channel 进去（stdin 和 resize） |
| `beacon-kube/metrics.rs` | `metrics.k8s.io`。有意思的是数量解析：`1500n`、`128974848`、`123Mi`、`129e6` 四种写法都要认 |
| `beacon-kube/helm.rs` | Helm release，从 Secret 里读 |
| `beacon-ui/terminal.rs` | VT 解析 + 网格 + 按键编码。解析和网格用 `alacritty_terminal`（Zed 和 Alacritty 同款）；自己写的是两头：字节进来怎么画，按键出去编成什么 |
| `beacon-ui/cluster.rs` | 侧边栏多了 "Cluster tools"：Helm Releases 和 Port Forwards 两个不是资源类型的列表 |
| `beacon-ui/app.rs` | 多集群：session 按 context 缓存，切回去是重建视图而不是重连 |

### 真机验证

- **port-forward**：对 `argocd-server:8080` 开一条转发，`curl` 本地端口拿到了 argocd-server 的
  307 重定向——真的通了，不是"看起来开了"。
- **exec**：三种结局都对上了 kubectl——正常输出、非零退出（只有 stderr）、**起不来**
  （distroless 镜像里没有 `ls`，报 OCI 的 "executable file not found"）。
- **metrics**：`k3s: 335m 2.8Gi`，`kubectl top node` 是 `335m / 2873Mi`。
- **Helm**：7 个 release，与 `helm list -A` 的名字/命名空间/revision/status/chart/appVersion 全部一致。
- **交互式终端**：在 `argocd-server` 里开出真 shell，跑了 `echo hello from $(hostname)`、
  `ls -1 /etc | head -3` 和一段 ANSI 彩色输出——提示符、命令回显、输出、绿色和粗体红色、光标块
  全都对。按键是程序注入的（见下）。

### 踩到的坑

1. **exec 的状态在第三个 channel 上**。stdout/stderr 都空的时候，"命令跑完什么也没输出" 和
   "命令根本起不来" 长得一模一样。distroless 镜像里没有 `ls`，不读状态 channel 的话界面上就是一片空白。
   `exec.rs` 和 `terminal.rs` 都补上了。
2. **Helm 的 payload base64 了两层**。Helm 把 gzip 过的 JSON 编一次，API 又把 Secret 的值编一次。
   只解一层拿到的是 `H4sI...`——那正是 base64 过的 gzip 的样子，也是这个 bug 的长相。
3. **终端的尺寸消息会跑在 shell 前面**。面板一量完就发 resize，但那时 `exec` 还没建出进程，
   没有 TTY 可应用，于是 shell 从默认 80 列起步、在错误的位置折行。现在 400ms 后补发一次。
4. **prepaint 里不能更新自己的 entity**。测量面板尺寸的 canvas 回调跑在自己的布局过程中，
   entity 处于 leased 状态，`update` 会被**静默丢弃**——表现是网格看起来该 resize 却一直没动。
   改成 prepaint 写进一个 `Cell`，下一帧 render 开头再应用。

### 与设计的分歧

- **Helm 读 Secret 而不是调 CLI**（§10 原建议调 CLI），理由见上。
- **“3 个集群内存 < 400MB” 的做法**：session 缓存（client、discovery、权限、转发），
  但 **watch 跟着视图走**。这正是 §0.3 “只 watch 正在看的东西” 的延伸——切回去时 registry 的
  30s linger 还在，watch 直接复用。代价是切换要重建视图（很快），好处是挂 N 个集群的常驻内存
  基本只有 N 份 discovery 缓存。

### 明确没做 / 没验的

- **没有真的按过键**（连续第四个里程碑，输入自动化工具不可用）。终端的按键编码是纯函数，
  9 个单测覆盖了 ctrl 组合、方向键的两种模式、Alt 前缀、backspace 是 DEL 不是 BS；
  从编码到容器再回到网格这条链路，是用临时插桩把按键**程序化注入**跑通并截图的，之后把插桩删干净。
  没验的只剩 GPUI 的 KeyDownEvent 接到编码器那几行。
- **3 集群 400MB 没实测**——手上只有一个集群。
- 终端的选区/复制粘贴/滚动回看、日志的 grep 高亮和下载、Prometheus 指标、插件系统。
- 写操作在真集群上仍然只跑过 dry-run（见 M4 记录）。

---

## 附：打包与图标实现记录（2026-09-23）

### 落地的东西

- **图标**：`assets/app-icon/beacon-icon.svg` 是唯一源文件，`scripts/icons.sh` 从它渲染出
  `beacon.icns` / `beacon.ico` / `icon-{16..1024}.png`。图形是七边形环（Kubernetes 那顶舵轮）
  + 七个顶点上的节点 + 中心的暖色灯和两道光束。
- **打包**：`crates/beacon/Cargo.toml` 的 `[package.metadata.packager]`，一套配置覆盖
  app/dmg/deb/appimage/nsis。
- **CI**：新增 `.github/workflows/package.yml`（六个矩阵项 + release job）；原有的
  `ci.yml` 补了一步"打包配置里列的图标文件都还在"的检查。
- 新文档 `docs/PACKAGING.md`。

### 关于图标，两条不是审美偏好的事

1. **每个尺寸单独从 SVG 渲染，不从 1024 缩**。16px 那一档，七边形环和光束在双缩放下会被一起
   抹平；按目标尺寸渲染时 librsvg 至少还按几何形状去抗锯齿。
2. **冷底 + 暖灯是为了 16px**。第一版整张图都是蓝的（环、光束、灯都是冷色），缩到 16px 就是
   一团看不出内容的蓝；把灯和光束换成琥珀色之后，**色相对比是唯一扛过重采样的东西** ——
   16px 上环确实没了，但"深蓝底 + 一点暖光"仍然认得出是哪个 app。这跟 roam 用暖红指针配冷蓝
   底是同一个理由。

### 实测

- macOS arm64：`--formats app,dmg` 产出 `Beacon.app` 和 11.9 MB 的
  `Beacon_0.1.0_aarch64.dmg`；Info.plist 的 identifier 与 `ProjectDirs` 一致；
  bundle 里的 icns 与源文件 **sha256 相同**；DMG 挂载后有 `/Applications` 链接；
  **从 bundle 启动开了窗口并连上 k3s 集群**（截了图）。
- macOS x86_64：交叉编译 + 打包通过，产出 12.5 MB 的 `Beacon_0.1.0_x64.dmg`，
  Rosetta 下启动同样出界面、连上集群。所以 CI 里 macOS 两项都不是 `unproven`。

### 踩到的坑

1. **`--formats dmg` 单独跑会把 `.app` 吃掉**。它把 app 挪进 DMG 之后不留副本，
   目录里只剩 dmg。要两个产物就写 `app,dmg`。
2. **SOCKS 代理让 DMG 打不出来**。cargo-packager 用 `curl` 去下 `create-dmg`，而这台机器的
   `all_proxy` 是 socks5，curl 报 `SOCKS feature disabled`。删掉
   `~/Library/Caches/.cargo-packager/` 之后**复现过一次**，`env -u all_proxy` 再跑就成功。
   缓存命中后不再联网，所以这个坑只在第一次出现——也因此很容易被误判成"偶发"。
3. **`.deb` 的 depends 不会被自动推导**。不配就产出一个能装、跑不起来的包。
   照着 `.github/actions/linux-deps` 的 `-dev` 列表写了运行时对应物；没有列 libssl，
   因为 Cargo.lock 里只有 `openssl-probe` 而没有 `openssl-sys`（kube 走 rustls）。

### 明确没做 / 没验的

- **签名与公证**：没有 Developer ID 证书，整条路一次都没跑过；配置里那行
  `signing-identity` 是注释掉的。
- **Linux 与 Windows 的打包一次都没跑过**，所以 package.yml 里这四项标了
  `continue-on-error: true`。AppImage 的 `APPIMAGE_EXTRACT_AND_RUN` 是从 roam 抄来的修法，
  在这里没验证过。
- `.deb` 的 depends **没有在干净的 Debian 上装过**。
- 两个 arm64 runner label（`ubuntu-24.04-arm` / `windows-11-arm`）在本机无从验证；
  拿不到 runner 时的失败长得很像构建失败。

---

## 附：多标签页实现记录（2026-09-24）

### 落地的东西

- `BeaconApp` 从"一个 `Connection`"变成 `Vec<Tab>` + `active`。一个 tab 是
  **一个集群的一个视图**，自己的 kind、namespace、过滤、选中行和详情面板都在
  `ClusterView` 里，所以"这边看 Pod，那边看另一个集群的 Deployment"是两个 tab。
- tab 栏用 `gpui_kit::component::tab::{TabBar, Tab}`，每个 tab 一个 `×`，右端一个 `+`。
- 新增按键：`⌘T` 开一个同集群的 tab、`⌘W` 关、`ctrl-tab` / `ctrl-shift-tab` 前后切。
  palette 的 `>` 里也加了这两条命令。
- **标题栏的 picker 和 palette 的 `ctx` 现在是两件事**：picker 换*这个* tab 指向的集群，
  `ctx` 是"去那个集群"——有 tab 就切过去，没有才新开。同一个集群开两个 tab 要显式按 `⌘T`，
  不会因为重复选同一个集群而莫名多出来。

### 连接不跟着 tab 走

`ClusterSession`（client、discovery 缓存、权限缓存、端口转发）按集群存在
`BeaconApp::sessions` 里共享，**关掉 tab 不会关掉 session**——重连才是慢的那一步。
watch 反过来：它属于 view，关 tab 就停。

这两条加上 registry 本来就有的引用计数，意味着同一个集群的两个 tab 看同一个 kind 时
只有一个 watch。实测：default + k3s-mirror 三个 tab（其中两个在 k3s-mirror 上，
分别看 Pod 和 Deployment），状态栏是 `3 watches · 3 tabs · 2 clusters` ——
3 = namespaces + Pod + Deployment，两个 tab 各自起的 namespaces watch 合并成了一个。

### 后台 tab 保留 watch，停掉两个定时器

§0.3 说"只 watch 正在看的东西"。有了 tab 之后这条的边界变了：**后台 tab 继续 watch**，
因为那正是 tab 存在的意义——切回去是即时且正确的，而不是重新 LIST 一遍再等一会儿。
用户现在显式控制着这个集合，关掉 tab 就停。

停掉的是两个**纯粹为了重画**的定时器：Age 列那个 1 秒的 clock，和 10 秒一次的 metrics 轮询
（后者是实打实的一个请求，而且没人在看答案）。这就是 `ClusterView::set_visible` 的全部内容。

### tab 标签：kind 在前，集群在后

第一版是 `集群 · kind`，实测立刻暴露问题：kubeconfig 里那些 EKS ARN 长到把 kind 整个挤掉，
三个 tab 全都显示 `arn:aws:eks:us-east-1:…`，等于什么都没说。现在 kind 放在 `prefix`
（不参与收缩），集群名做 label（超了就省略号）——`Pod · default` 和
`Service · local-k3s-with-a…` 至少能分清谁是谁。集群全名在标题栏和状态栏里都有。

### 真机验证

只有 `default` 这一个本地 k3s 是能碰的，kubeconfig 里其余全是别人的生产集群。为了真正验到
**多集群**而不是只验多 tab，把 `default` 这个 context 复制成了一份临时 kubeconfig，
里面三个名字（`default` / `k3s-mirror` / 一个故意很长的名字）都指向同一台本地 k3s，
用 `KUBECONFIG=` 指过去跑——三个**不同的 ClusterId**，也就是三个真的 session。
用户的 kubeconfig 一个字没动。

验到的：三个 tab 分别停在 Pod / Deployment / Service；切回 tab 0 时列表、侧边栏选中项都还在；
关掉中间那个之后剩两个、active 落回正确的位置；`⌘T` 那条路径（`new_tab`）确实走
"reusing the session"；palette 里 `>tab` 列出两条新命令。全部截了图。

### 踩到的坑

**截图截到的是过期的一帧。** 前几次截图一直显示 "Connecting"，而日志明明已经连上并且换过
kind 了。在 `render_body` 里临时打了一行 log 才确认：render 一直在跑、状态是对的，
`screencapture -l` 对一个**从来没被激活过、且被终端挡住**的窗口返回的是缓存的旧画面。
之前几个里程碑没撞上，是因为那时是 `open -n Beacon.app` 启动的——`open` 会把 app 激活。
修法是截图前先 `osascript` 把进程提到前台。这个坑值得记：它看起来完全像一个 UI 不刷新的 bug。

### 明确没做 / 没验的

- **按键仍然没有真的按过**（输入自动化工具依旧不可用）。`⌘T`/`⌘W`/`ctrl-tab` 是
  `on_action` 接到 `new_tab`/`close`/`step` 上的，这三个函数本身是程序化调用并截图验过的；
  没验的是 GPUI 把 keystroke 派发到 action 的那几行。
- **没有连过第二个真实的远端集群**。多集群这一条是用三个指向同一台本地 k3s 的 context 验的：
  ClusterId、session、discovery、watch 都是各自独立的真货，但"两个不同 API server"
  这件事本身没验。
- tab 不能拖拽重排，关掉的 tab 不能撤销，tab 太多时也没有滚动或溢出菜单
  （`TabBar` 支持 `track_scroll` 和 `menu`，还没接）。
- tab 集合不持久化：重启回到 kubeconfig 的 current-context 一个 tab。

### 补：详情面板的关闭按钮（2026-09-24）

M3 起详情面板就只能从 palette 的"Show or hide the details panel"关掉，鼠标根本够不着 ——
`DetailClosed` 在 `ClusterView` 里有订阅者，却**没有任何地方 emit 它**，另一半一直没写。
现在 tab 条右端有一个 `×`，`on_click` 就是 `cx.emit(DetailClosed)`。

实测：打开面板（Overview/YAML/Events/Logs/Exec/Shell 六个 tab 加一个 `×`），
走 emit 那条路之后面板消失、表格恢复整高，状态栏的 watch 数从 3 掉回 2 ——
面板的 events watch 确实被释放了，不是只把它藏起来。按钮本身仍然没有被真的点过
（输入自动化依旧不可用），验的是 listener 里那一行 emit 之后发生的全部事情。

### 补：从 Finder 启动时连不上 EKS（2026-09-24）

**症状**：从终端 `cargo run` 一切正常，双击 app 打开就连不上 EKS ——
`auth error: unable to run auth exec: No such file or directory`。

**原因**：`shell_env` 问的是 `$SHELL -lc`，而 **`zsh -lc` 不读 `.zshrc`**。
它只读 `.zshenv` / `.zprofile` / `.zlogin`，偏偏 `brew shellenv`、mise、asdf、nvm、pnpm、krew
基本都装在 `.zshrc` 里。EKS 的 kubeconfig 是 `command: aws` 且没有 `env:`，
`aws` 在 `/opt/homebrew/bin`，于是找不到。

**这个 bug 为什么一直没被发现**：在终端里验 `zsh -lc 'echo $PATH'` 看起来完全正常 ——
终端自己的交互式 shell 早就把那些目录放进 PATH 了，子进程直接继承，`-lc` 只是原样传下去。
只有在**什么都没得继承**的时候差异才出现，而那正是这个模块存在的唯一理由。

**修法**：改成**交互式 + 登录** shell（`-i -l -c`），并用一个 marker 把答案包起来 ——
交互式 rc 文件可能往 stdout 打任何东西（banner、更新提示），所以取两个 marker 之间那段，
其余全丢掉。退出码故意不检查：交互式 shell 因为 rc 文件最后一条命令而非零退出很常见，
只要 marker 在，中间那段就是答案。`-l -c` 作为回退保留，而且是双重有用的：拒绝 `-i` 的 shell
会立刻失败、不花时间；而慢到超时的 `.zshrc`，回退恰恰不读它。超时从 3s 提到 5s
（本机交互式 ~0.5s，但带 nvm/conda 的 profile 动辄好几秒），每次尝试各一份预算。

**验证时差点被 `open` 骗了。** 第一次用 `open -n Beacon.app` 测，**没打补丁也能连上**，
差点得出"Finder 启动其实没问题"的结论。实际是 **`open` 会把调用者的环境传给被启动的 app** ——
它继承了我终端里的完整 PATH。从 `env -i` 里调 `open` 才是真的 Finder 等价物：
打补丁前复现出一模一样的报错，打补丁后连上并 discovered 111 kinds。教训是
**从终端发起的任何"模拟 Finder"都要先确认它真的没继承环境**，这跟前面截图那个坑是同一类错误。

### 补：YAML 面板的 Format（2026-09-24）

YAML 面板是可编辑的，改完直接 SSA 提交。之前编辑过、粘贴过的内容长什么样就是什么样，
而且**只有按了 Apply 才知道它能不能解析**。现在 Apply 旁边多了一个 Format：
把编辑器里的内容重新解析、重新序列化一遍。

它顺带解决第二件事 —— 用的是 `apply` 那条完全相同的解析路径，所以按一下就等于问
"这玩意儿到底是不是合法 YAML"，不用往集群写一次才知道。实测一段坏缩进按 Format，
面板直接给出 `This is not valid YAML: error: line 2 column 14: mapping values are not
allowed in this context` 加一段带 caret 的定位，文本原封不动。

**key 顺序不动。** 一开始写的测试断言它会按字母序排（因为 `serde_json::Value` 默认是
BTreeMap），结果测试挂了：依赖树里 `gpui-pre` 打开了 `serde_json/preserve_order`，
Value 其实是 IndexMap。这反而是对的行为 —— 一个悄悄把别人 manifest 重排成字母序的
formatter 比没有 formatter 更糟。改的只有形状：flow style 展开成 block、缩进和引号统一。
（面板加载出来的那份顺序又是另一回事：`metadata` 走的是 `ObjectMeta` 这个强类型结构体的
字段顺序，`spec`/`status` 才是 `data` 里那个 IndexMap。这里没有细究，因为 Format 不动顺序，
它是哪种顺序都不影响。）

注释和空行不保留，因为 `serde_json::Value` 里没有它们的位置。这不是新增的损失 ——
Apply 本来发出去的就是解析后的对象，解析丢掉的东西本来也到不了集群。tooltip 里写明了，
并且有一个测试把这个行为钉住，免得哪天悄悄变了。

4 个单测：flow 展开且保持顺序、幂等、丢注释、非法 YAML 的报错前缀。按钮本身没有被真的
点过（老问题），验的是 listener 里那一行往后的全部。
