# 打包与分发

打包用 [cargo-packager]，配置在 `crates/beacon/Cargo.toml` 的
`[package.metadata.packager]`。一套配置覆盖 macOS / Linux / Windows 的所有格式，
所以仓库里没有手写的 bundle 脚本。

```sh
cargo install cargo-packager --locked

cargo build --release -p beacon
cargo packager -p beacon --release --formats app,dmg   # macOS
cargo packager -p beacon --release --formats deb,appimage
cargo packager -p beacon --release --formats nsis      # Windows
```

产物落在 `target/release/`（带 `--target` 时落在 `target/<triple>/release/`）：
`Beacon.app`、`Beacon_0.1.0_aarch64.dmg`。

**`--formats dmg` 单独跑会把 `.app` 吃掉** —— 它把 app 挪进 DMG 之后不会再留一份。
想同时拿到两个产物就写 `--formats app,dmg`，CI 里也是这么配的。

## 图标

`assets/app-icon/` 是完整一套：`beacon.icns`（macOS）、`beacon.ico`（Windows）、
`icon-{16,32,64,128,256,512,1024}.png`（Linux hicolor 主题），源文件是
`beacon-icon.svg`。`scripts/icons.sh` 从 SVG 重新生成全部产物，需要 `rsvg-convert`、
`magick`，`.icns` 那步还需要 macOS 的 `iconutil`。

**每个尺寸都是从 SVG 按目标尺寸渲染的，不是从 1024 缩下来的。** 图形本身是按小尺寸
可读性调的：七边形环（Kubernetes 那顶舵轮）用了一宽一窄两道描边，灯是暖色而背景是冷色 ——
**色相对比是唯一能扛过重采样到 16px 的东西**，到那个尺寸环已经糊没了，只剩深蓝底上一团暖
光。双缩放会把这两样一起抹平。

已验证：打出来的 bundle 里 `Contents/Resources/beacon.icns` 与
`assets/app-icon/beacon.icns` **字节一致**（sha256 相同）。

界面内的图标是另一回事，由 `gpui_kit::assets::Assets` 提供，在 `main.rs` 里注册。

Windows 的桌面和开始菜单快捷方式从 `beacon.exe` 读取图标。
`crates/beacon/build.rs` 在 Windows 目标构建时编译 `assets/packaging/beacon.rc`，
将 `beacon.ico` 的全部尺寸嵌入可执行文件；仅在打包配置中列出 `.ico` 不会完成这一步。
构建需要 Windows SDK 的资源编译器，缺少编译器会直接失败。
CI 与发布打包前运行 `scripts/check-windows-icon.py`，逐个校验嵌入图像与源 ICO 字节一致，
避免只检查到安装器图标而漏掉应用及快捷方式。

## 签名与公证

**证书。** 这台机器上没有 **Developer ID Application** 证书，所以 app 目前**没签名** ——
在别人的 Mac 上过不了 Gatekeeper，也**不能公证**。拿到证书（Apple Developer Program，
99 美元/年）之后把配置里那行注释打开：

```toml
[package.metadata.packager.macos]
signing-identity = "Developer ID Application: NAME (TEAMID)"
```

身份**字符串本身不是秘密** —— 任何签过名的二进制里都能读到它，所以它属于仓库；私钥
（`.p12`）永远不属于。

**公证是凭据驱动的，不写在配置里。** cargo-packager 找到下面任一组就自动公证，找不到
就打一行警告继续走：

- `APPLE_ID` + `APPLE_PASSWORD` + `APPLE_TEAM_ID`（应用专用密码）
- `APPLE_API_KEY` + `APPLE_API_ISSUER` + `APPLE_API_KEY_PATH`（App Store Connect API key）
- `APPLE_KEYCHAIN_PROFILE`（`notarytool store-credentials` 存好的 profile）

CI 上导入证书用 `APPLE_CERTIFICATE`（base64 的 .p12）+ `APPLE_CERTIFICATE_PASSWORD`。

**不开 App Sandbox。** cargo-packager 对原生二进制总是传 `--options runtime`，只在配置了
`entitlements` 时才传 `--entitlements`，所以"不写 entitlements"就是保持不沙箱的全部操作。
这不是偷懒：Beacon 要读 `~/.kube/config`，还要在 PATH 上找 kubeconfig 的 `exec` 凭据插件
并把它作为子进程跑起来，沙箱里这两件事都做不了。

## 构建与打包分开执行

cargo-packager 只打包、**不构建**。先显式 `cargo build --release -p beacon`，再跑
cargo-packager。产物跟着**当前机器的架构**走 —— 本机是 arm64，DMG 名字里的 `aarch64`
就是它。

不做 universal：那意味着把整棵依赖树（含 GPUI）编两遍再 `lipo`，而 cargo-packager 本身
没有 universal 的概念。真要做的话是加一个先编两个 target 再 lipo 到 `target/release/beacon`
的脚本，然后让 cargo-packager 打那个合并后的二进制。

## 两个会咬人的环境问题

**DMG 需要联网，而 SOCKS 代理会把它顶回来。** cargo-packager 打 DMG 时会去
raw.githubusercontent.com 下 `create-dmg`（pin 到某个 commit），缓存在
`~/Library/Caches/.cargo-packager/DMG/`。这台机器上 `all_proxy` 指向一个 SOCKS 代理，
而它调的 `curl` 不支持 —— **删掉缓存后实测复现**：

```
ERROR https://raw.githubusercontent.com/create-dmg/create-dmg/28867ba.../create-dmg:
      Connection Failed: Connect error: SOCKS feature disabled.
```

绕法（同样实测通过，重新下载并打包成功）：

```sh
env -u all_proxy -u ALL_PROXY cargo packager -p beacon --release --formats app,dmg
```

缓存命中之后不再联网，所以这个坑只在第一次、或者清过缓存之后出现。

**`license-file` 别指向不存在的文件。** `Cargo.toml` 里声明了 `license = "Apache-2.0"`，
但仓库里**没有 LICENSE 文件**。配置里因此没有 `license-file`；要发布的话这个得补上
（涉及版权署名，留给你定）。

## Windows 的 VC++ 运行库

`.cargo/config.toml` 为 `x86_64-pc-windows-msvc` 与
`aarch64-pc-windows-msvc` 启用 `-C target-feature=+crt-static`，将 VC++ 运行库
静态链接进应用。Windows 构建应显式指定 `--target`，让这些参数只作用于目标代码，
构建脚本和过程宏仍按宿主配置编译：

```sh
cargo build --locked --release -p beacon --target aarch64-pc-windows-msvc
cargo packager -p beacon --release --target aarch64-pc-windows-msvc --formats nsis
```

x64 构建将 target 换成 `x86_64-pc-windows-msvc`。如果环境设置了 `RUSTFLAGS`
或 `CARGO_ENCODED_RUSTFLAGS`，它会覆盖配置文件中的参数，必须同时保留
`-C target-feature=+crt-static`；共享 CI 在 `-D warnings` 之外显式保留此参数，
并在原生 Windows ARM64 runner 上执行构建和测试。

发布工作流在打包前用 `scripts/check-windows-runtime.ps1` 调用 MSVC 的
`dumpbin /DEPENDENTS`，发现 `VCRUNTIME`、`MSVCP`、`CONCRT` 或 `VCOMP` DLL 依赖
就拒绝生成安装包，避免构建机器已安装运行库而掩盖问题。Windows 系统 DLL 仍由操作系统提供。

旧版若启动时提示缺少 `VCRUNTIME140.dll`，可先安装微软官方的
[ARM64 VC++ 运行库](https://aka.ms/vc14/vc_redist.arm64.exe)（原生 ARM64 应用）或
[x64 VC++ 运行库](https://aka.ms/vc14/vc_redist.x64.exe)（x64 应用），再启动 Beacon。
这些链接来自[微软的运行库下载文档](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist)。

## `.deb` 的 depends 不是自动推出来的

cargo-packager **不探测 `.deb` 依赖**，只写配置里的 `depends`，不配就产出一个
"能装、跑不起来"的包 —— 两种失败里更难查的那种。配置里列的是
`.github/actions/linux-deps` 里那些 `-dev` 包的运行时对应物。

没有列 `libssl`：Cargo.lock 里只有 `openssl-probe`（纯 Rust），没有 `openssl-sys` ——
kube 走的是 rustls。也没有列 Vulkan 驱动（`mesa-vulkan-drivers` 之类）：那是用户机器上
显卡驱动的事，`libvulkan1` 这个 loader 才是应用自己的依赖。

## CI：push 时自动打包并发布

`.github/workflows/package.yml`，矩阵是 mac / windows / linux × amd64 / arm64。

触发限定在 **push 到 main、push `v*` tag、以及手动 dispatch**。其他分支和 PR 运行
`.github/workflows/ci.yml`；main 和发布标签通过可复用的同一 CI 工作流执行检查，不重复触发一份独立 CI。

发布依次执行：

1. `prepare` 校验版本并生成本次构建版本。
2. `checks` 执行发布脚本测试、格式检查、Clippy，以及 macOS / Linux / Windows 构建和测试。
3. `package` 将同一个版本写入各平台的 Cargo.toml 和 Cargo.lock，再构建和打包。
4. `release` 先创建草稿并上传全部可用产物，再公开发布。已公开的同名 Release 不允许被重跑覆盖。

| 触发 | 版本 / 标签 | GitHub 发布状态 |
| --- | --- | --- |
| push main | `0.2.0-dev.<GITHUB_RUN_NUMBER>` / `v0.2.0-dev.<GITHUB_RUN_NUMBER>` | Pre-release，不抢占 Latest |
| push `vX.Y.Z` 标签 | `X.Y.Z`，必须与源码版本完全一致 | 全部平台打包成功后发布正式 Release，标记 Latest |
| 手动 dispatch | 同样生成唯一的 `X.Y.Z-dev.<GITHUB_RUN_NUMBER>` | 只上传 Actions artifact，不发 Release |

`Cargo.toml` 的 `[workspace.package].version` 是版本唯一来源，当前下一轮开发目标为
`0.2.0-dev.0`。CI 使用发布工作流的运行编号替换 `dev.0`，不把构建编号提交回仓库；
重跑同一次工作流沿用同一编号。编号可能有空缺，不影响排序。
应用启动日志、界面版本及安装包都从这次修改后的 Cargo 版本读取；各个 workspace crate 的
锁定版本同步修改，第三方依赖的版本与校验和保持不变。
macOS 的 `assets/packaging/Info.plist` 同步生成：`CFBundleShortVersionString` 使用数字版本
（例如 `0.2.0`），`CFBundleVersion` 将运行编号编码为递增的数字版本，`BeaconVersion` 保留完整版本。
应用界面与 DMG 文件名仍使用 `0.2.0-dev.101`。
Debian 安装包使用 `0.2.0~dev.101`，使开发版的包版本低于 `0.2.0` 正式版；
Linux AppImage、Windows 安装包文件名及应用内版本继续使用完整 SemVer。
Release 说明记录完整提交 SHA、工作流链接和实际提供的平台产物，便于反馈和定位问题。

### 版本规则与正式发版

Beacon 当前处于 `0.x` 阶段，采用以下项目约定：

| 变化 | 示例 | 版本选择 |
| --- | --- | --- |
| 兼容的修复、小幅优化 | 修复 Shell、日志或布局问题 | `0.2.0 → 0.2.1` |
| 新功能，或需要说明的不兼容变更 | 新资源视图、配置格式变化 | `0.2.1 → 0.3.0` |
| 核心流程稳定，开始承诺升级兼容性 | 配置迁移、主要操作与平台支持已有明确保证 | `1.0.0` |

从 `1.0.0` 起，兼容修复升 patch，兼容的新功能升 minor，不兼容的配置 / 行为或平台支持变化升 major。
同一轮包含多种变更时取最高级别；代码重构或依赖升级本身不决定大小版本。

假设正在开发 `0.2.0`，确认可发布后：

```sh
# 同时更新 workspace、Cargo.lock 中的本地 crate 版本与 macOS 版本元数据。
python3 scripts/release.py set 0.2.0

# 检查修改并提交。先在 main 发布工作流中验证这份代码。
git diff -- Cargo.toml Cargo.lock assets/packaging/Info.plist
git add Cargo.toml Cargo.lock assets/packaging/Info.plist
git commit -m "chore(release): prepare 0.2.0"
git push origin main

# 上一步检查与打包通过后，在同一提交上创建正式标签。
git tag -a v0.2.0 -m "Beacon 0.2.0"
git push origin v0.2.0
```

创建标签时应仍在准备版本的同一提交上；标签触发后会再次执行完整检查与打包。
`v0.2.0` 配 `0.2.0-dev.0` 或 `0.2.1` 会在准备阶段失败。
本地版本脚本需要 Python 3.11 或更新版本；CI 固定使用 Python 3.12。

正式发布后，继续开发修复版或功能版时设置新的目标，例如：

```sh
python3 scripts/release.py set 0.2.1-dev.0
git add Cargo.toml Cargo.lock assets/packaging/Info.plist
git commit -m "chore(release): start 0.2.1 development"
git push origin main
```

随后 main 发布 `0.2.1-dev.<编号>`；开发新功能时将目标改成 `0.3.0-dev.0`。
每次正常 push 无需手动修改版本。正式发布说明中还应补充本轮用户可见的变化、已知问题，以及需要用户执行的迁移步骤。
历史 `main-<SHA>` Release 不会被本流程修改；首次正式发布前 GitHub 上可能仍显示旧的 Latest，
首次 `vX.Y.Z` 正式发布后由新版本接替。

发布脚本的本地验证：

```sh
python3 -m unittest discover -s scripts/tests -p 'test_*.py' -v
```

下表的"状态"一律指**在本机验证到哪一步**；各平台当前的 CI 结果以 GitHub Actions 为准。

| 矩阵项 | runner | 格式 | 本机验证到哪 |
| --- | --- | --- | --- |
| macos-arm64 | `macos-15` | app, dmg | 打包 + 挂载 DMG + 从 bundle 启动，连上集群出界面 |
| macos-amd64 | `macos-15` 上交叉编译 | app, dmg | 同上（Rosetta 下启动），产出 `Beacon_0.1.0_x64.dmg` |
| linux-amd64 | `ubuntu-24.04` | deb, appimage | **没验过** |
| linux-arm64 | `ubuntu-24.04-arm` | deb, appimage | **没验过** |
| windows-amd64 | `windows-2022` | nsis | **没验过**（没有 Windows 机器） |
| windows-arm64 | `windows-11-arm` | nsis | **没验过** |

Intel mac 用**交叉编译**而不是申请 Intel runner：macOS SDK 两个架构都能出，而 Intel
runner 正在退役。本机实测过这条路 —— 产物落在 `target/x86_64-apple-darwin/release/`，
所以工作流里的上传路径统一用 triple 目录。

CI 先用矩阵里的 target 显式执行
`cargo build --locked --release -p beacon --target <triple>`，再把同一个 target 传给 cargo-packager。
打包工具固定为本机已验证的 cargo-packager 0.11.8。
Linux 的 DEB 单独读取发布脚本生成的配置，以适配 Debian 的 `~dev` 排序；AppImage 仍从 Cargo 元数据读取配置。
构建步骤**不放在 cargo-packager 的 hook 里**：hook 在 Unix 上过 `sh`、在 Windows 上过
`cmd.exe`，没有一个 hook 字符串能在两边都正确展开 `--target`。

**开发版对 `unproven: true` 的打包平台允许失败。** 已验证的 macOS 打包失败、任意平台的
共享 CI 构建或测试失败，都会阻止发布。正式版不允许任何打包平台失败，确保不会把缺少平台安装包的版本标成 Latest。
某个平台验证成功后应将它的 `unproven` 改成 `false`。

**两个 arm64 runner label 我在这台机器上没法验证。** `ubuntu-24.04-arm` 和
`windows-11-arm` 是 GitHub 较新提供的；如果仓库拿不到它们，那两项会以"找不到 runner"失败，
而不是构建失败 —— 这两种失败看起来很像，别混。

**AppImage 的 `APPIMAGE_EXTRACT_AND_RUN: 1` 是从姊妹项目 roam 抄过来的**：linuxdeploy 要
挂载自己的 AppImage 来运行，没有 FUSE 就死在 `std::logic_error` 里，让它解压而不是挂载能
绕开。这条在 Beacon 这边**一次都没跑过**，所以 linux 两项都还是 unproven。

## 已验证 / 未验证

**已验证**（本机，arm64 macOS，cargo-packager 0.11.8）：

- `cargo build --release -p beacon` 之后
  `cargo packager -p beacon --release --formats app,dmg`，产出 `Beacon.app` 与 11.9 MB 的
  `Beacon_0.1.0_aarch64.dmg`；
- Info.plist：`CFBundleIdentifier=dev.beacon.Beacon`（与 `logging.rs` 里
  `ProjectDirs::from("dev","beacon","Beacon")` 一致，改它会让已有日志目录失联）、
  `CFBundleName=Beacon`、`0.1.0`、`LSMinimumSystemVersion=11.0`、
  `LSApplicationCategoryType=public.app-category.developer-tools`；
- bundle 里的图标与 `assets/app-icon/beacon.icns` 字节一致；
- DMG 能挂载，里面有 `Applications` 符号链接和 `Beacon.app`；
- **从 bundle 启动能开窗口**，连上 k3s 集群并列出 Pod（arm64 与 Rosetta 下的 x86_64 都试过）；
- 交叉编译 `--target x86_64-apple-darwin` 打包通过，产出 12.5 MB 的 `Beacon_0.1.0_x64.dmg`。

**未验证**：

- **签名、公证与 Gatekeeper**。没有 Developer ID 证书，这条路一次都没跑过；"在别人的
  Mac 上双击能打开"同理未验证。
- Linux 与 Windows 的构建与打包（`deb`/`appimage`/`nsis` 一个都没跑过）。
- `.deb` 的 `depends` 列表是从构建依赖推出来的，**没有在干净的 Debian 系统上装过**。

[cargo-packager]: https://github.com/crabnebula-dev/cargo-packager
