# CLI、插件与快照参考

本文是 `xiaoli` 命令行、Codex 插件和 `MonitorSnapshotV5` 公共接口的参考。

## 生命周期命令

### `xiaoli --show`

显示并置前已经运行的小狸；如果没有实例，则启动 GUI。后台刷新不会主动抢焦点。

### `xiaoli --hidden`

启动后隐藏窗口，只保留托盘和采集器。登录启动使用此模式。

### `xiaoli --stop`

通过当前用户 IPC 端点请求已有实例优雅退出。会释放托盘、watcher、SQLite 和单实例锁。

## 探针

```text
xiaoli --probe-once
  [--sessions-root PATH]
  [--session-index PATH]
  [--state-root PATH]
```

探针进行一次只读采集并把 `MonitorSnapshotV5` JSON 写到标准输出。它不会写生产 SQLite、解析游标或 `monitor.jsonl`，适合 fixture、CI 和诊断。

参数：

| 参数 | 默认值 | 用途 |
| --- | --- | --- |
| `--sessions-root` | Codex 默认 sessions 目录 | 使用隔离 rollout fixture |
| `--session-index` | Codex 默认 session index | 提供任务标题 |
| `--state-root` | 平台默认小狸状态目录 | 定位 IPC 或测试状态；probe 不写入 |

Windows PowerShell 示例：

```powershell
.\XiaoLi.exe --probe-once `
  --sessions-root .\fixtures\sessions `
  --session-index .\fixtures\session_index.jsonl
```

验证：输出必须能被 `ConvertFrom-Json` 读取，`schemaVersion` 为 `5`，且不含 prompt、回复正文或完整 cwd。

## 无 Node 插件命令

### `xiaoli --install-plugin`

在当前用户范围写入或升级 `xiaoli-model-monitor`。生成的 hook 和 MCP 配置引用当前可执行文件绝对路径。GUI 首次启动也会在后台执行同一逻辑。安装器不会绕过 Codex 的 hook 信任边界：首次安装、升级或移动程序后，请在 Codex `/hooks` 中审阅并信任命令；已运行的 Codex 需要新任务或重启后加载。

### `xiaoli --uninstall-plugin`

移除小狸自己的当前用户插件文件，不删除 rollout、SQLite、日志或 Codex 任务。

### `xiaoli --hook-capture`

从标准输入读取 Codex hook JSON，仅提取：

- 事件类型。
- session/thread/turn ID。
- 请求模型。
- 时间戳。

请求 effort 不在 hook 脱敏包中，由 rollout 的结构化 `turn_context` 作为请求证据。`--hook-capture` 不访问网络，只通过当前用户本地 IPC 端点发送给 GUI。从 hook 处理入口开始，标准输入读取、JSON 解析、本地 IPC 和原子 fallback 写入共享同一个单调时钟 150ms 硬上限；输入元数据上限是 256 KiB。超时、超大、慢输入、IPC 卡住或慢磁盘都会 fail-open，返回合法 hook 响应并让 Codex 正常继续。插件 manifest 中的宿主 `timeout` 仅是第二道兜底，不是 150ms 保证的主要实现。小狸不会保存 prompt、回复正文或完整 cwd。

### `xiaoli --mcp-server`

通过 stdio 启动只读 MCP server。它只通过当前正在运行的 GUI/采集器 IPC 读取派生快照，不修改 Codex 会话。成功结果带有 `snapshotSource: "liveMonitorIpc"`。如果 GUI 未运行或 IPC 不可用，工具会明确报离线错误；磁盘中的旧 `latest-snapshot.json` **不会**被静默冒充为当前模型或当前回合。

## MCP 工具

### `get_monitor_summary()`

返回所有活动根任务的紧凑状态，包括后代状态冒泡后的严重级别、请求模型/effort、累计 Token 和缓存输入占比。

### `get_session_detail(threadId)`

返回指定根任务及其子会话/子智能体的完整 V5 证据。找不到线程时返回结构化错误，不猜测最接近的 ID。

### `render_monitor_card(threadId?, theme?)`

返回适合 Codex 对话内展示的监视卡数据。`theme` 接受 `cute` 或 `minimal`；不提供 `threadId` 时返回所有活动根任务摘要。

### `get_connection_origin(threadId)`

`threadId` 必填。只读返回该活动线程的 provider、endpoint 分类、认证模式、置信度和限制。不返回完整 URL、环境变量值、OAuth token 或 API Key；找不到线程时返回结构化错误，不任意选择另一个会话。

### `list_relay_audits(limit?)` / `get_relay_audit(auditId)`

只读列出已有脱敏审计报告或查看单份报告。MCP 不提供启动/取消审计、保存凭据或修改 endpoint 的工具，避免在对话内隐式消耗额度。

## 插件目录

```text
xiaoli-model-monitor/
  .codex-plugin/plugin.json
  .mcp.json
  hooks/hooks.json
  skills/model-monitor/SKILL.md
  assets/
```

插件 manifest、hooks 和 MCP 配置不依赖 Node。发布包里的插件资源和 GUI 版本必须一致。

## IPC

| 平台 | 传输 | 隔离方式 |
| --- | --- | --- |
| Windows | `\\.\pipe\OpenAI.Codex.ModelMonitor.<UserSID>` | 管道 ACL 限当前用户；名称含 SID |
| macOS/Linux | Unix domain socket | 父目录 `0700`，socket `0600`，名称含有效 UID |

实际端点写入状态目录的 `ipc-endpoint.json`。Unix 单实例使用独占文件锁，Windows 使用命名互斥量。

## `MonitorSnapshotV5`

字段定义和准确性限制见 [状态与证据](STATUS_AND_EVIDENCE.md#monitorsnapshotv5-核心结构)。兼容性规则：

- V5 完整保留 V4 字段，并在每个会话增加 `connectionOrigin`。
- `timing.observedOutputRate` 映射到端到端观测速率。
- `anomalies` 继续输出简短兼容文案；详细统计位于 `qualityAssessment`。
- `serverRoute.evidence` 只接受 `notObserved` 或 `explicitReroute`。
- MCP 成功结果的 `snapshotSource: "liveMonitorIpc"` 表示响应来自正在运行的小狸进程；它不改变请求模型、服务器路由和 effort 各自的证据边界。
- 读取方必须忽略未知字段，不能因增加字段而拒绝快照。

## 刷新接口

Tauri 内部命令 `refresh_now` 是异步命令，返回：

```json
{
  "status": "completed",
  "snapshot": { "schemaVersion": 5 }
}
```

`status` 也可能为 `coalesced`，表示请求与已运行刷新合并。扫描、SQLite 和日志在后台执行；发布前已释放刷新锁。前端等待 15 秒后恢复按钮并保留上一份有效快照，后台任务仍可通过事件更新界面。
