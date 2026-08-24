# 隐私与本地数据

小狸是旁路、本地优先的监视器。它读取 Codex 已持久化的结构化事件和当前用户插件 hook，不代理网络请求，也不修改 Codex 会话。

## 会处理的数据

- session/thread/turn ID。
- 根任务与子任务关联。
- 请求模型和请求 effort。
- 结构化事件类型与时间戳。
- Token 计数、缓存输入 Token、上下文窗口。
- item 类型、item ID、结构化开始/完成时间。
- 明确 reroute 的模型链和关联证据类型。

## 不会进入持久化状态、日志或公共输出的数据

- prompt 正文。
- 助手回复正文。
- 推理正文。
- 命令、工具输入或工具输出正文。
- 完整工作目录路径。采集器只会在内存中短暂读取它，并提取末级目录名作为标题兜底；完整路径不会进入快照、日志或 SQLite。
- 原始 reroute 原因正文。
- 私人参考图。

采集器会流式读取并解析原始结构化 JSONL，但只把白名单字段复制到状态。解析 `item_completed` 时只提取 item 类型、ID 和结构化时间；即使原事件还包含正文，正文也不会进入状态结构或数据库。

## 本地文件

| 文件 | 内容 | 可否重建 |
| --- | --- | --- |
| `monitor.db` | 解析游标、会话聚合、最近合格行为样本与基线 | 可以，从 rollout 重建 |
| `latest-snapshot.json` | GUI、MCP 和诊断使用的最新派生快照 | 可以 |
| `monitor.jsonl` | 兼容状态变化日志，5 MiB 轮换，保留 3 个备份 | 不需要重建 |
| `ipc-endpoint.json` | 当前用户 IPC 传输和端点 | 每次启动更新 |
| UI preferences | 主题、置顶、紧凑/展开尺寸与位置 | 可以重置 |

默认目录：

- Windows：`%LOCALAPPDATA%\XiaoLi`
- macOS：`~/Library/Application Support/XiaoLi`
- Linux：`$XDG_DATA_HOME/xiaoli` 或 `~/.local/share/xiaoli`

首次 Windows 迁移只读导入旧 Mochi Meter 的可兼容 UI 偏好和历史样本，不导入旧解析游标，不删除旧数据库、日志或备份。

## IPC 权限

- Windows 命名管道名称包含当前用户 SID，ACL 仅允许当前用户。
- Unix socket 放在当前用户目录，父目录权限为 `0700`，socket 为 `0600`。
- endpoint 文件不包含凭据，只用于本机组件发现。
- hook 连接失败会 fail-open，不阻塞 Codex。

## Probe 和日志脱敏

`--probe-once` 不写生产缓存或日志。快照会包含 thread/turn ID，因为它们是会话关联的公共接口，但不会包含 prompt、回复正文或完整 cwd。

公开 issue 前请仍然检查诊断输出中的 thread/turn ID 是否需要手动脱敏。仓库的截图和 fixtures 使用虚构 ID。

## 网络

小狸自身的采集和判断不需要把数据上传到项目服务器。GitHub Release 检查、操作系统更新检查或 Codex 本身的网络行为不属于小狸采集器。当前版本没有遥测上传。

## 删除本地数据

先退出小狸，再删除对应平台状态目录即可清除派生数据。这样不会删除 Codex rollout。重新启动后，小狸会从原始结构化真源重建缓存。

若只想移除 Codex 插件而保留本地基线：

```text
xiaoli --uninstall-plugin
```
