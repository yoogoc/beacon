# Beacon TODO

[返回中文 README](../README.zh-CN.md) · [English README](../README.md)

根据 2026-10-13 对 Lens、Krust、Monokle 和 K9s 的比较及 Beacon 当前源码整理。
近期顺序：配置引用关系 → 结构化日志 → Job/CronJob 操作 → Helm 详情。
条目完成并验证后勾选；后续方向在实现前另行设计，版本安排按实际进度确定。

## P1 · 近期

### 01 · ConfigMap / Secret 被谁使用

- [x] 增加 Used by 视图，列出引用配置的 Pod 和工作负载，并可跳转到对应资源。
- [x] 显示引用的 key、环境变量和挂载路径；解析 env、envFrom、volume、projected、imagePullSecrets 等实际引用，覆盖常规容器、initContainers 和工作负载 Pod 模板。
- [x] 展示缺失引用，并在编辑或删除时提供影响范围查看入口。

### 02 · 结构化日志

- [x] 自动识别 JSON / logfmt，提供单行字段检视和原始日志查看。
- [x] 增加等级过滤、正则搜索、字段筛选和显示字段选择。
- [x] 增加时间范围查询和重要日志书签。
- [x] 明确导出全部日志与当前筛选结果的操作；继续支持现有容器选择、Previous、聚合日志和下载流程。

### 03 · Job / CronJob 操作

- [x] CronJob 支持立即执行、暂停和恢复。
- [x] Job 支持复制重跑，生成新的 Job 并保留原执行记录。
- [x] 操作后可直接追踪 Job 状态、关联 Pod 和日志；沿用现有权限预检与确认交互。

### 04 · Helm release 详情

- [x] 从现有 release 列表进入详情，显示 values 和渲染后的 manifest。
- [x] 显示历史 revision、状态和版本信息。
- [x] 支持 revision 之间的 values / manifest 差异审阅。

## P2 · 接着完善

### 05 · Node 维护

- [x] 增加 Cordon / Uncordon 操作。
- [x] Drain 前展示受影响 Pod、PDB 阻塞、DaemonSet 和本地存储情况。
- [x] 显示驱逐进度、各目标结果与失败原因。

### 06 · YAML Schema 智能编辑

- [ ] 根据集群 Kubernetes 版本和 CRD Schema 提供字段补全、枚举值和必填字段提示。
- [ ] 提供字段说明、类型校验和行内错误定位。
- [ ] 复用现有高亮、折叠、格式化、服务端 dry-run 和保存前 diff 流程。

### 07 · 单资源指标趋势

- [ ] 增加 Pod / Node / Container 的 CPU 和内存历史图表。
- [ ] 显示使用量与 requests / limits 的对照，并关联重启和 OOM 事件。
- [ ] 分别处理 Metrics API 会话采样与 Prometheus 历史数据，显示数据来源和可用范围。

### 08 · Argo Workflows 运行操作

- [ ] 支持从模板提交 Workflow，并填写和审阅运行参数。
- [ ] 支持 Retry、Resubmit、Suspend / Resume、Stop / Terminate。
- [ ] 按运行状态和权限提供可用操作，操作后继续追踪现有 DAG、节点详情和日志。

### 09 · RBAC 权限查看与解释

- [x] 提供 ServiceAccount / User / Group 的权限查看与 Can I 查询。
- [x] 展示主体 → Binding → Role / ClusterRole → Rules 的权限来源。
- [x] 在 403 错误中解释相关 verb、资源和 subresource，并可跳转到权限详情。

### 10 · 全局资源搜索与收藏

- [x] 支持在已连接集群中跨资源类型、命名空间和集群搜索对象。
- [x] 增加集群和资源收藏，并可快速打开。
- [x] 与现有命令面板、列表筛选和 Saved filters 衔接，保持默认启动不连接集群的行为。

### 11 · 端口转发增强

- [ ] 支持以 Service / Deployment 为目标，解析可用的后端 Pod。
- [ ] 提供固定本地端口设置与冲突提示。
- [ ] 保存常用转发配置，支持显式恢复；Pod 替换后重新选择目标并恢复转发。
- [ ] 复用现有转发会话列表、连接数和停止操作。

### 12 · Helm 生命周期操作

- [ ] 支持安装和升级，选择 chart 版本及 values，并在执行前审阅渲染结果与差异。
- [ ] 支持回滚到指定 revision 和卸载 release。
- [ ] 显示操作进度、结果与失败原因，操作后刷新 release 详情和历史。

## P3 · 后续方向

### GitOps

- [ ] Argo CD Application 专属视图：Health / Sync、目标 revision 和管理资源树。
- [ ] 对比期望状态与实时状态，并支持从资源反查所属 Application。
- [ ] 设计 Argo CD 同步、刷新等操作；后续评估 Flux 集成。

### 本地配置工作台

- [ ] 打开 YAML 文件或目录，识别多文档和多个资源。
- [ ] 按目标资源展示预览、dry-run 和 diff，应用后分别显示成功与失败结果。
- [ ] 支持 Helm values / Kustomize overlay 选择与渲染预览，复用已有资源对比能力。

### 诊断与扩展

- [ ] 汇总缺失引用、Service 无后端、异常权限等可解释问题，并跳转到对应资源和字段。
- [ ] 设计按资源类型配置的自定义命令与快捷键。
- [ ] 评估 AI / MCP 集成和 CRD 展示扩展，单独确定首期范围。

## 贯穿各阶段的质量任务

- [ ] 固定 macOS、Windows x86_64 / ARM64、Linux 的关键交互回归，覆盖输入法、剪贴板、文件对话框和窗口操作。
- [ ] 覆盖系统 / 应用 / 集群代理、EKS 凭据、Exec 回退和断线恢复场景。
- [ ] 建立大资源列表、持续 Watch 更新和长时间日志的性能基准，记录首屏耗时、内存和滚动表现。
- [ ] 新增写操作使用模拟资源验证，覆盖权限不足、失败、取消与部分成功结果。
- [ ] 延续主应用、独立窗口、设置页之间一致的组件、主题、错误与空状态设计。

## 参考

- [Lens：节点维护](https://docs.lenshq.io/k8slens/using-lens/nodes/) · [Helm 管理](https://docs.lenshq.io/k8slens/how-to/manage-helm-charts/)
- [Krust：资源拓扑](https://krust.io/docs/topology/) · [结构化日志](https://krust.io/docs/logs/)
- [Monokle：功能说明与维护状态](https://github.com/kubeshop/monokle) · [Schema 智能编辑](https://docs.monokle.io/resource-editing/)
- [K9s：功能说明](https://k9scli.io/) · [自定义命令](https://k9scli.io/topics/plugins/)
