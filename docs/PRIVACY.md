# 隐私与本地数据

小狸是旁路、本地优先的监视器和审计工作台。被动 Codex 采集只读取已持久化的结构化事件和当前用户插件 hook，不代理 Codex 请求，也不修改 Codex 会话。只有用户为自己有权测试的 endpoint 配置独立 profile/凭据并确认预算后，中转审计才主动发起网络请求。

## 会处理的数据

- session/thread/turn ID。
- 根任务与子任务关联。
- 请求模型和请求 effort。
- 结构化事件类型与时间戳。
- Token 计数、缓存输入 Token、上下文窗口。
- item 类型、item ID、结构化开始/完成时间。
- 明确 reroute 的模型链和关联证据类型。
- `session_meta.model_provider`、经脱敏的 endpoint 类别与只包含 `auth_mode` 的来源证据。
- 会话历史所需的派生指标、结束时间和经截断 ID。
- `RelayProfile` 的本地别名、协议、声称模型与规范化 Base URL（不含 userinfo、query 或 fragment）。
- 用户明确选择的私有 probe pack 规范化绝对路径、版本和 SHA-256。题包正文只在保存校验和审计启动时短暂读取。
- 为真实会话对照保存的本地 relay profile ID。会话绑定使用的 endpoint 作用域指纹（规范化协议、主机、有效端口和 API 基础路径的单向哈希）仅存在于内存刷新过程，不进入 SQLite、报告、日志、快照或 MCP；userinfo、query 和 fragment 不参与作用域也不会保存。
- 主动审计的参数、CSPRNG `runSeed`、请求/样本计数、四证据轴、原因、置信度和限制。

## 不会进入持久化状态、日志或公共输出的数据

- Codex prompt、自动生成审计 prompt 和私有题包任务正文。
- 助手回复正文。
- 推理正文。
- 命令、工具输入或工具输出正文。
- 完整工作目录路径。采集器只会在内存中短暂读取它，并提取末级目录名作为标题兜底；完整路径不会进入快照、日志或 SQLite。
- 原始 reroute 原因正文。
- 私人参考图。
- Codex OAuth token、Codex API Key 或任何认证文件中除 `auth_mode` 外的字段。
- 中转 API Key、Authorization 头、完整 endpoint query、原始审计 prompt 和原始中转回复正文。
- 中转返回的工具调用、URL、HTML、脚本、代码或“给检测器的指令”。这些内容不会被执行。

采集器会流式读取并解析原始结构化 JSONL，但只把白名单字段复制到状态。解析 `item_completed` 时只提取 item 类型、ID 和结构化时间；即使原事件还包含正文，正文也不会进入状态结构或数据库。

## 本地文件

| 文件 | 内容 | 可否重建 |
| --- | --- | --- |
| `monitor.db` | 解析游标、会话派生历史、聚合、Codex 行为基线、脱敏 RelayProfile（含用户选择的题包路径/版本/哈希，不含正文）、参考摘要元数据与审计报告 | rollout 相关部分可重建；用户 profile/报告不可从 rollout 重建 |
| `latest-snapshot.json` | 兼容与诊断使用的最新派生快照；实时 MCP 不从此文件读取 | 可以 |
| `monitor.jsonl` | 兼容状态变化日志，5 MiB 轮换，保留 3 个备份 | 不需要重建 |
| `ipc-endpoint.json` | 当前用户 IPC 传输和端点 | 每次启动更新 |
| UI preferences | 主题、置顶、紧凑/展开尺寸与位置 | 可以重置 |

API Key 不在上表任何文件中。默认只放在进程内存；用户明确勾选后，只保存到 Windows Credential Manager、macOS Keychain 或 Linux Secret Service，SQLite 只保存不含 Key 的凭据引用。Linux Secret Service 不可用时回退内存并提示，不回退到明文文件。

普通 `RelayBaselineSummary` 和未验证导入项只是本地参考元数据。用户可以另行显式导入 Ed25519 公钥作为本地信任锚；只有由该 key 签名、完整分布与参数通过验证且未过期的包，才会保存到独立的可信静态基线表并用于低置信指纹 scorer。公钥、签名和规范化一词计数可以持久化，但包不包含原始 prompt/回复；中转响应不能写入可信表。撤销信任锚会同步移除由它验证的 scorer 包。Release 还内置从公开来源整理的小型版本化分布，只作跨协议、低置信实验排名。可用于中/高置信度配对统计的参考，仍只来自用户明确授权后在同一次审计中实时调用的匹配官方 profile。

默认目录：

- Windows：`%LOCALAPPDATA%\XiaoLi`
- macOS：`~/Library/Application Support/XiaoLi`
- Linux：`$XDG_DATA_HOME/xiaoli` 或 `~/.local/share/xiaoli`

首次 Windows 迁移只读导入旧 Mochi Meter 的可兼容 UI 偏好和历史样本，不导入旧解析游标，不删除旧数据库、日志或备份。

## IPC 权限

- Windows 命名管道名称包含当前用户 SID，ACL 仅允许当前用户。
- Unix socket 放在当前用户目录，父目录权限为 `0700`，socket 为 `0600`。
- endpoint 文件不包含凭据，只用于本机组件发现。
- hook 处理不访问网络；标准输入、解析、本地 IPC 与原子 fallback 共享 150ms 单调时钟硬上限，失败或超时会 fail-open，不阻塞 Codex。

## Probe 和日志脱敏

`--probe-once` 不写生产缓存或日志。快照会包含 thread/turn ID，因为它们是会话关联的公共接口，但不会包含 prompt、回复正文或完整 cwd。

公开 issue 前请仍然检查诊断输出中的 thread/turn ID 是否需要手动脱敏。仓库的截图和 fixtures 使用虚构 ID。

## 网络

小狸没有项目云服务，也没有遥测上传。被动 Codex 采集不会由小狸发起外部网络请求。

主动中转审计是例外：

- 只请求用户确认的 profile origin，HTTP 重定向关闭，API Key 不会随 3xx 转发到其他 host。
- 非 localhost 的明文 HTTP endpoint 只允许手动连接测试或审计，并在每次发送凭据前要求用户确认窃听风险；定时审计会拒绝它。localhost 可用于本地 mock/开发。
- 官方配对只在用户另行配置官方 profile/凭据并确认额外预算时发起。
- 定时审计默认关闭，启用后受绑定 profile、每次预算和每月请求上限保护。

GitHub Release 下载、操作系统自身服务或 Codex 本身的网络行为不属于小狸采集器。

## 删除本地数据

先退出小狸，再删除对应平台状态目录即可清除 SQLite、快照、日志、会话派生历史、profile 和审计报告。这样不会删除 Codex rollout。重新启动后，小狸会从原始结构化真源重建可重建的采集缓存，但无法重建已删除的 profile 和审计报告。

如果 profile 明确保存过系统凭据，应优先在工作台删除 profile，让小狸同时删除对应系统凭据。直接删除状态目录不保证清理独立的操作系统凭据库条目。

若只想移除 Codex 插件而保留本地行为基线、参考元数据和审计报告：

```text
xiaoli --uninstall-plugin
```
