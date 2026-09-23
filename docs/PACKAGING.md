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

## `.deb` 的 depends 不是自动推出来的

cargo-packager **不探测 `.deb` 依赖**，只写配置里的 `depends`，不配就产出一个
"能装、跑不起来"的包 —— 两种失败里更难查的那种。配置里列的是
`.github/actions/linux-deps` 里那些 `-dev` 包的运行时对应物。

没有列 `libssl`：Cargo.lock 里只有 `openssl-probe`（纯 Rust），没有 `openssl-sys` ——
kube 走的是 rustls。也没有列 Vulkan 驱动（`mesa-vulkan-drivers` 之类）：那是用户机器上
显卡驱动的事，`libvulkan1` 这个 loader 才是应用自己的依赖。

## CI：push 时自动打包并发布

`.github/workflows/package.yml`，矩阵是 mac / windows / linux × amd64 / arm64。

触发限定在 **push 到 main、push `v*` tag、以及手动 dispatch** —— 不是所有分支。每个矩阵
项都是一次完整的依赖树构建（含 GPUI），六份并行；特性分支不需要安装包，那是
`.github/workflows/ci.yml` 的活。push 到 main 时 `release` job 创建
`main-<完整 commit SHA>` 对应的 prerelease（显示名用七位短 SHA）；push `v*` tag 则创建
正式 Release 并标记为 latest；手动 dispatch 只留 Actions artifact，不发 Release。

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
`cargo build --release -p beacon --target <triple>`，再把同一个 target 传给 cargo-packager。
构建步骤**不放在 cargo-packager 的 hook 里**：hook 在 Unix 上过 `sh`、在 Windows 上过
`cmd.exe`，没有一个 hook 字符串能在两边都正确展开 `--target`。

**没验证过的平台标了 `continue-on-error: true`。** 那不是为了让徽章好看：已验证的平台一旦
坏掉照样让整个 run 变红，而没建过的平台不会把它掩盖掉。每个 `true` 都是一句关于"到底验证
到哪"的声明 —— 某个平台第一次成功出包之后，就该把它删掉。

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
