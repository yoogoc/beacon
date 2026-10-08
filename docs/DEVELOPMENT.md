# Beacon 开发指南

[返回中文 README](../README.zh-CN.md) · [English README](../README.md) · [架构设计](DESIGN.md) · [打包与分发](PACKAGING.md)

## 构建环境

仓库的 `rust-toolchain.toml` 固定 Rust 1.98，包含 rustfmt 与 Clippy。
使用 rustup 时，进入仓库后会按该配置选择工具链。

| 平台 | 系统依赖 |
| --- | --- |
| macOS | Xcode Command Line Tools；`xcode-select --install` 可安装。 |
| Windows | MSVC C++ 构建工具与 Windows SDK，使用 MSVC Rust Target。 |
| Linux | Clang、CMake、pkg-config，以及 X11 / Wayland、Vulkan、字体和 ALSA 开发库。 |

Linux 的构建依赖以 [.github/actions/linux-deps/action.yml](../.github/actions/linux-deps/action.yml)
为准。Ubuntu / Debian 可按当前 CI 配置安装：

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  clang cmake pkg-config \
  libasound2-dev libfontconfig-dev libfreetype6-dev libssl-dev \
  libvulkan-dev libwayland-dev libx11-xcb-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libzstd-dev mesa-vulkan-drivers
```

运行：

```sh
cargo run --locked -p beacon
```

开发构建对依赖启用较高优化，配置位于根目录 Cargo.toml。
图形界面需要桌面会话；领域层示例可在没有窗口的环境中运行。

## 常用检查

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --all-targets
```

CI 对 macOS、Linux、Windows 执行构建与测试；检查配置见
[ci.yml](../.github/workflows/ci.yml)。

## 无界面验证 Kubernetes 行为

`beacon-kube` 的 `watch` 示例复用会话、发现和表格列，无需启动 GPUI：

```sh
# 读取一次 Pod 列表后退出
cargo run -p beacon-kube --example watch -- --context NAME --once Pod default

# 查看集群提供的资源类型
cargo run -p beacon-kube --example watch -- --context NAME --kinds

# 对指定对象执行 Server-Side Apply dry-run，不写入集群
cargo run -p beacon-kube --example watch -- --context NAME --apply-check Deployment default/my-app
```

不传 `--once` 时持续跟随 Watch。可与 `kubectl get` 对比列和状态；
有两处有意差异：缺失值显示为 `<none>`，没有自定义列的资源使用相对 Age 而非 Created At 时间戳。
这些命令使用真实集群的所选上下文；检查写操作时使用自己创建的测试资源。

## 日志与 UI 检查

应用日志使用平台数据目录，macOS 路径为
`~/Library/Application Support/dev.beacon.Beacon/logs`。
可从应用日志窗口查看，也可通过 `RUST_LOG` 增加输出：

```sh
RUST_LOG=beacon=debug,beacon_ui=debug,beacon_kube=debug,kube=debug cargo run -p beacon
```

UI 修改应检查实际画面。macOS 提供窗口截图脚本：

```sh
# Beacon 运行时，从另一个终端执行
scripts/screenshot.sh /tmp/beacon-ui.png
```

运行脚本的终端需要 Screen Recording 权限。
应用进程名称不同于 `beacon` 时，用 `BEACON_APP_NAME` 指定窗口所属应用名称。
README 的深色 / 浅色截图位于 [docs/images](images)，使用本地模拟 API 的演示数据，
不包含真实集群的对象或凭据。

可用 `BEACON_CONFIG_DIR` 为检查实例指定独立的偏好目录，并通过 `KUBECONFIG`
隔离集群配置；检查结束后关闭实例。该变量隔离应用偏好，不更改日志目录。

## 模块与开发约定

```text
crates/
├── beacon/          应用启动、日志和打包
├── beacon-ui/       GPUI 视图、主题和 Tokio 桥接
├── beacon-kube/     Kubernetes 领域层
└── beacon-columns/  表格列和资源状态格式化
assets/app-icon/     应用图标源文件及各平台产物
scripts/            截图、图标生成脚本
```

**领域层不依赖 GPUI。** Kubernetes 的认证、发现、Watch、权限和操作应留在
`beacon-kube`，使 CLI、测试和 GUI 都能复用。

**网络与视图通过消息连接。** kube / hyper 在 Tokio Runtime 上执行，
GPUI 视图在前台线程更新。`beacon-ui/src/bridge.rs` 桥接两个执行环境。
生产端对更新合批，避免首次 List 的大量对象逐条触发渲染。
系统字体等昂贵查询不应放在视图的每次渲染中。

**更换表格列需刷新布局。** `gpui-component::TableState` 缓存列布局。
更换 `ColumnSet` 后仅调用 `cx.notify()` 不够，需要 `state.refresh(cx)`。

**测试导入避免宏冲突。** 父模块使用 `gpui_kit::*` 时，
测试模块应按名称导入所需项，避免 `use super::*` 带入 GPUI 的 `#[test]` 宏，
与 Rust 内置测试宏冲突。

## 依赖与打包

`gpui-kit` 固定到上游提交 `3cc2d0d624ce124be62eb4670e04197bc78bb8a5`（0.7.1 之后），
统一引入 GPUI、组件和图标，并默认启用 `gpui-fast` 后端。
正式发布的 0.7.1 尚无这个开关，因此使用固定 Git 提交并保留 Cargo.lock；
`tree-sitter-yaml` 继续提供 YAML 高亮。升级时应一起检查这些 API。
`vendor/gpui-base` 基于该提交，保留 YAML 初始折叠补丁，并修正折叠后的滚动高度；来源与测试命令见
[BEACON-PATCHES.md](../vendor/gpui-base/BEACON-PATCHES.md)。

GPUI Fast 的 Retained Mode 会复用未变化的视图。改变未被实体或全局状态追踪的
渲染数据时，应调用 `cx.notify()`；不要在每次渲染或 prepaint 时无条件修改全局状态。
窗口拖拽的命中区域属于测量缓存，使用内部可变性更新，不触发下一帧的全局失效。
排查视图未更新时，可临时禁用视图复用进行对比：

```sh
GPUI_VIEW_RETENTION=0 cargo run --locked -p beacon
```

`k8s-openapi` 0.28 的时间类型使用 jiff。
kube 启用 `http-proxy` 与 `socks5`，支持连接配置中的代理；不要误删这些功能。

打包先构建，再执行 cargo-packager，示例与平台细节见 [PACKAGING.md](PACKAGING.md)。
应用图标源文件是 `assets/app-icon/beacon-icon.svg`，`scripts/icons.sh` 按各尺寸重新渲染；
需要 `rsvg-convert`、`magick`，生成 macOS `.icns` 还需要 `iconutil`。
