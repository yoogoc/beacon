# Beacon 使用指南

[返回中文 README](../README.zh-CN.md) · [English README](../README.md)

## 连接与工作区

Beacon 从合并后的 kubeconfig 枚举上下文，启动时不会连接任何集群。
点击侧边栏的集群名称，或在命令面板中输入 `ctx` 选择上下文进行连接。
连接成功后工作区保持空白，直到选择一个资源类型。

集群树位于标签页外，当前集群展开其资源导航。
**Cluster** 分组位于最前；`networking.k8s.io` 放在 **Network**，
`admissionregistration.k8s.io` 放在 **Config**。
自定义资源按 API Group 收在 **Custom resources** 下；搜索仍可匹配完整的资源名称和 API Group。

EKS ARN 上下文显示可读的集群名称与 EKS 标识。悬停可查看区域、账号和原始上下文；
命令面板同时搜索短名称和原始 ARN。底部显示当前集群名称、API Server 返回的 Kubernetes 版本和连接状态。

右键集群：

| 操作 | 行为 |
| --- | --- |
| **Disconnect** | 停止该集群的 Watch、Shell 和端口转发，保留资源标签页与命名空间范围。 |
| **Reconnect** | 重新认证、发现资源并刷新服务端版本，在新连接上恢复标签页。Shell 和端口转发需要重新打开。 |
| **Cluster settings…** | 设置别名、图标、连接代理和指标来源。 |

点击底部 **watches** 可查看资源类型、命名空间、选择器、缓存对象数和订阅者数；
点击 **clusters** 可查看连接状态、API Server、标签页、Watch 和端口转发，并切换集群。
这些面板查看现有状态，不会新增 Watch。

## 标签页与布局

点击资源类型会复用同一集群、同一类型的标签页。右键 **Open in new tab** 可再打开一个视图，
并带入当前命名空间范围。每个标签页分别保留资源类型、命名空间、筛选、选中项和详情状态。

`⌘T` / `Ctrl+T` 新建当前集群的标签页；尚未选择集群时打开集群选择器。
`⌘W` / `Ctrl+W` 关闭当前标签页，`Ctrl+Tab` 和 `Ctrl+Shift+Tab` 切换标签页。
没有标签页时，`⌘W` / `Ctrl+W` 会弹出退出确认框；关闭主窗口也会先确认。
选择 **Cancel** 保留窗口与连接，选择 **Quit** 退出应用。设置、日志和快捷键窗口中的关闭快捷键仅关闭对应窗口。
选择集群本身会切换到已有标签页，或在尚无标签页时创建一个。

标题栏的侧边栏按钮可折叠 / 展开集群树。拖动右侧分隔线调整宽度，重新展开时保留当前窗口中的宽度。
资源详情位于列表右侧，Pod 工具位于列表下方，两者都有独立的拖拽分隔线。

多个标签页共用一个集群连接，相同资源与范围的订阅共享 Watch。
后台标签页继续接收资源更新，但暂停 Age 的每秒刷新和指标轮询。
关闭标签页释放订阅，空闲 Watch 在 30 秒宽限期后关闭；集群连接仍保留。

## 命名空间与列表筛选

新视图默认使用 kubeconfig 上下文中的命名空间，没有配置时使用 `default`。
命名空间选择器中，点击名称会单选并关闭菜单；点击复选框可组合多个命名空间。
取消最后一项后切换到所有命名空间。多选使用各命名空间自己的 Watch，适用于只有命名空间级权限的账号。

名称搜索可与命名空间范围、Label 和资源属性筛选一起使用。

所有资源列表都提供 **Labels: All** 筛选菜单。可勾选当前命名空间范围内的 `key=value`，
多个勾选项需要同时满足；也可输入 [Kubernetes 标签选择器](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/#label-selectors)，
点击 **Apply** 或按 Enter 应用，例如 `app=web,environment in (production,qa)`。
支持 `=`、`==`、`!=`、`in`、`notin`、`key`（存在）和 `!key`（不存在）。
**Clear labels** 只清除 Label 筛选，命令面板的 **Clear filter** 清除所有列表筛选。
无效选择器会显示可复制的错误提示并保留之前的筛选。
每个标签页独立保留筛选，切换命名空间和资源 Watch 更新时继续生效。

不同资源的属性筛选如下：

| 资源 | 属性筛选 |
| --- | --- |
| Pod、Deployment | Status |
| Service | Type |
| Ingress | Class |
| PersistentVolumeClaim | Status、Volume、Access Mode、Storage Class、Volume Mode |
| Secret | Type，包含当前范围内的自定义类型 |
| CustomResourceDefinition | Scope，列表也显示 Scope 列 |

Pod 列表以实心圆表示普通容器、空心圆表示 Init 容器，颜色对应状态。
悬停标记可查看容器名称、当前状态与就绪情况。
列表首次加载时显示骨架行；Watch 返回首批数据或连接出现故障后结束等待。

## 详情与 Owner 导航

资源详情提供 **Overview / YAML / Events**。Overview 垂直展示元数据及资源的 Spec / Status；
较大的对象使用可展开区块。容器详情涵盖端口、Requests / Limits、探针、环境变量、挂载、
安全设置与运行状态，并分别展示普通、Init 和临时容器。

YAML 打开时按当前集群的设置折叠字段，初始默认折叠 `metadata.managedFields` 和 `status`。
点击行号旁的箭头可展开，切换详情标签页后保留手动展开状态。

名称和命名空间只在详情头部显示。Labels 与 Annotations 每行使用 `key=value`，
超过五项时默认收起其余项。Overview 的值可选中和复制。
错误提示也可拖拽选中后通过 `Cmd+C`（macOS）或 `Ctrl+C`（Windows / Linux）复制；
右键选择 **Copy** 可复制完整提示，包括被截断的内容。底部集群状态可右键复制连接详情。

点击有颜色的 Owner 值可跳转并定位到对应资源。Pod 的 Owner 为 ReplicaSet 时，
先显示 ReplicaSet，再异步读取并显示它的 Owner；读取失败或无权限时保留原链接。

## Pod 日志、Exec、Shell 与端口转发

从 Pod 行的右键菜单打开 **Logs / Exec / Shell**。它们在独立的底部面板显示，
可切换面板标签页、调整高度或关闭。关闭右侧资源详情不会关闭底部工具面板。

Shell 支持交互输入、终端颜色和窗口尺寸变化。默认命令尝试 bash，再回退到 sh；
也可以填写自定义命令。终端聚焦时，Escape 等按键交给容器里的程序处理。

集群拒绝 WebSocket 升级时，Exec 和 Shell 会自动使用本机 `kubectl`，
沿用所选上下文、命名空间和容器。将 `kubectl` 放在 Beacon 的 PATH 上才能使用该兼容路径。
Shell 回退使用本地 PTY，关闭面板会结束子进程。

WebSocket 和 SPDY 的权限要求可能不同，权限预检结果也可能不完整；
最终行为以服务端授权结果为准。SOCKS5 代理不支持 kubectl 的 SPDY 回退。
Pod 右键菜单也可转发已声明端口到 localhost；转发在切换页面后继续运行，断开集群时停止。

## 创建、修改与删除资源

右键菜单按类型提供适用的操作：Pod 的容器工具、工作负载的重启 / 扩缩容、
ConfigMap / Secret 的 Data，以及 YAML、复制名称、删除等通用操作。
菜单中的操作始终指向右键时的对象。权限预检发现操作不被允许时，会禁用并显示原因；
无法获取或不完整的规则不会替代服务端的最终授权检查。

**Create** 打开当前类型与命名空间的 YAML 模板：
`Validate` 使用服务端 dry-run 检查，`Create` 创建新对象并打开详情。
每次提交一个资源，已有同名对象会被拒绝；自定义资源需填写其 Schema 要求的字段。

YAML 编辑器支持语法高亮与 **Format**。保存使用 Server-Side Apply；
字段所有权冲突会说明冲突字段及 Field Manager。

勾选行或表头复选框后，选择 **Delete selected**。
确认框以可滚动的表格列出每个目标的命名空间和名称。
搜索及类型筛选会从选中集合移除隐藏项；删除携带 UID 前置条件，避免删除同名的新对象。
成功的目标清除选中，失败项保留并显示错误。

## ConfigMap、Secret 与 TLS

ConfigMap 和 Secret 的 **Data** 页按 Key 展示值，支持直接编辑文本。
非 UTF-8 数据只展示描述，不通过文本输入框修改。
Secret 值默认遮盖，揭示后才能保存；未修改的值在提交时保持原样。
Data 保存与 YAML 使用相同的 Server-Side Apply 路径及冲突处理。

`kubernetes.io/tls` 类型的 Secret 还会在 Overview 解析 `tls.crt` 中的证书链，显示：

- 主题、签发者、有效期与是否过期。
- 签名算法、公钥算法、密钥强度和扩展。
- 指纹及公钥 PEM。

此视图不读取或展示 `tls.key`。过期判断是时间检查，不代表证书链信任或主机名验证。

## 应用与集群配置

macOS 的入口为系统菜单栏 **Beacon → Settings…**；
Windows / Linux 使用应用菜单 **Menu → Settings…**。
`⌘,` / `Ctrl+,` 和命令面板的 **Open settings** 打开同一窗口。

可选择 Light、Dark 或命名的自定义主题。自定义主题可配置常规 / 等宽字体及字号，
以及文字、背景、按钮各状态等颜色。颜色填写 Hex，空值继承基础主题。
保存新的名称可保留另一份主题；保存后应用到已打开窗口，并在下次启动时恢复。

偏好存放在平台配置目录。macOS 为
`~/Library/Application Support/dev.beacon.Beacon/settings.json`；
`themes/` 保存独立的主题 JSON，`icons/` 保存导入的 SVG。
**Open config folder** 可定位目录。文件使用原子替换写入，Unix 上仅当前用户可访问。

全局代理支持使用 kubeconfig / 环境默认值、Direct，以及 HTTP / HTTPS / SOCKS5 URL。
集群代理可覆盖全局配置，或选择 **Use global proxy** 继承。
显式代理覆盖 `NO_PROXY`；Direct 绕过 kubeconfig 和环境中的代理。
这些设置也应用到凭据插件和 kubectl 回退，不修改原始 kubeconfig 或应用进程的全局环境。

右键集群的 **Cluster settings…** 可设置别名、预设图标或导入 SVG、代理和 Metrics Source。
别名与图标立即生效，原始上下文名称仍用于识别集群。
连接与指标配置在下次连接时生效；**Save and reconnect** 立即重连并保留资源标签页。

集群设置的 **YAML folding** 列出默认折叠字段，勾选表示打开 YAML 时折叠，取消全部勾选则默认全部展开。
可勾选 Labels、Annotations 和 Spec，也可用 **Add field** 添加点分字段路径，
例如 `spec.template.spec.containers`；列表内的字段路径适用于每个列表项。
**Remove** 移除规则，**Restore defaults** 恢复默认设置。规则按集群单独保存在本地，
**Save** 后下次打开资源 YAML 时生效，无需重连，不改动已打开编辑器的手动折叠状态或内容。

指标默认来自 **Kubernetes Metrics API**，也可选择 **Prometheus** 或 **Disabled**。
Prometheus 配置包含 Base URL、可选 Bearer Token 和四组 Pod / Node CPU / 内存查询：

| 查询 | 返回值 | 必需标签 |
| --- | --- | --- |
| Pod CPU / 内存 | CPU 为核数，内存为字节 | `namespace`、`pod` |
| Node CPU / 内存 | CPU 为核数，内存为字节 | `node` |

查询应返回 Instant Vector。如果 Prometheus 包含多个集群，需自行加入集群选择器。
**Test metrics source** 使用所选代理验证查询后再保存。

## 命令面板、快捷键与应用日志

`⌘K` / `Ctrl+K` 打开命令面板：

| 前缀 | 内容 |
| --- | --- |
| 无 | 当前类型的资源对象 |
| `@` | 资源类型，含 CRD |
| `#` | 命名空间，切换到单一命名空间 |
| `ctx` | 集群 |
| `>` | 命令 |

**Help → Keyboard shortcuts**、`F1` 或面板中的 `> keyboard`
打开可搜索的快捷键指南，列出当前平台的按键、功能和焦点行为。

macOS 使用 **View → App logs**，Windows / Linux 使用 **Menu → App logs**；
也可按 `⌘⇧L` / `Ctrl+Shift+L`，或选择面板的 **Open app logs**。
日志使用独立窗口，重复打开会聚焦已有窗口，无需连接集群。
支持搜索、Warn + Error / Error 筛选、暂停 / 继续、复制筛选后的行和打开历史日志目录。
窗口跟随当天最新日志文件，后台读取最多 1 MiB 尾部、保留最近 5,000 行；关闭后停止轮询。
