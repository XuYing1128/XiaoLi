# 变更记录

本项目使用语义化版本。首个公开版本是 beta，接口仍可能在保持证据边界的前提下调整。

## [0.2.0-beta.1] - 2026-08-27

### 新增

- `MonitorSnapshotV5`：每个会话增加独立 `connectionOrigin`，分类官方 ChatGPT、官方 OpenAI/Anthropic API、托管 provider、自定义/本地 endpoint 和未知来源。
- 解析缓存格式 10：回放 rollout 首部以恢复旧会话的 `session_meta.model_provider`。
- 独立小狸工作台，包含总览、会话历史、中转检测、基线和检测原理五个页面。
- 会话派生历史：保存请求模型/effort、来源分类、Token、缓存、时序和证据状态，不保存消息正文。
- `RelayProfile`、系统凭据库可选保存和 OpenAI Responses、OpenAI Chat Completions、Anthropic Messages 三协议适配。
- 连接、快速、标准和深度审计档位，硬请求上限分别为 6 / 150 / 320 / 720。
- 操作系统 CSPRNG `runSeed`、10 个任务家族 × 4 种语言的单 Token 分布探针、base-2 JSD、usage 算术检查和 bootstrap 统计原语。
- 无实时官方 Key 时，可使用随 Release 编译的公开社区分布做低置信、跨协议的实验性相对排名；它不改变四条证据轴或总裁决。
- 独立 `AuditManager`：不共用 Codex 刷新锁，单飞管理请求/输入/输出预算，支持进度、取消、报告列表和脱敏持久化。
- 定时快速审计：默认关闭，显式绑定 profile/系统凭据，在用户本地时间附近抖动 ±30 分钟，并受月度请求硬上限保护。
- MCP 增加连接来源和已有中转审计报告的只读查询；MCP 不能启动会消耗额度的审计。
- 用户私有 probe pack：严格 JSON schema、文件大小/任务数/字符串上限、启动前版本与 SHA-256 复核；只持久化路径/版本/哈希，完整计划超预算时拒绝启动而不截断题包。
- 六个质量域覆盖结构化 JSON、真实三协议工具 schema、长上下文 nonce、约束推理、同请求多消息状态和多语言；只在本地评分受限结构，不执行工具或保存原始响应。

### 证据口径

- 中转报告固定拆分为协议兼容、计量一致性、行为质量和模型身份四条独立证据轴，不计算“真模型概率”。
- 无同次实时匹配官方基线时，依赖参考的质量与身份结论必须返回 `insufficientEvidence`，不将协议成功或社区排名伪造为 `consistent`。
- 行为指纹、速度、Token 和 API 自报型号都不能填充 `actualModel`；它们也不能创建 Codex `serverRoute`。
- 通过报告的结论固定保留“真实物理模型未获密码学证明”。
- 自定义 endpoint 只表示可进行中转检测，不自动表示恶意、注水或降质。

### 安全与隐私

- API Key 默认只保留在进程内存；只有用户明确选择时才使用 Windows Credential Manager、macOS Keychain 或 Linux Secret Service，且绝不回退为明文文件。
- 不读取或复用 Codex ChatGPT OAuth token 或 Codex API Key。
- 非 localhost 的明文 HTTP endpoint 只允许逐次确认后的手动请求，定时审计拒绝它；HTTP 重定向关闭，凭据只发往用户确认的原 origin。
- 响应正文按不可信输入处理：限制 JSON/SSE/总响应大小，不执行工具、URL、HTML、脚本、代码或中转给检测器的指令。
- OpenAI Responses SSE 检查严格递增的 `sequence_number` 与终态后数据，Chat Completions 按 choice 跟踪唯一完成状态；Anthropic Messages 检查 message/block 全生命周期，乱序、重复、未闭合或伪终态都不会标为协议正常。
- 审计报告、SQLite、日志和事件不包含 API Key、prompt、回复正文、完整 cwd 或原始端点 query。
- 用户导入基线默认标记为未签名、低置信；中转样本不能污染官方/社区基线命名空间。
- 真实会话只按规范化协议、主机、有效端口和 API 基础路径的私有端点作用域绑定本地 profile；同主机不同端口/路径、冲突证据或多 profile 歧义均不会绑定。
- 预算预览对全部随机指纹生成变体取真实请求体最大上界，执行 seed 的实际预留必须不超过预览；未来 seed 不进入活动任务投影，完成报告才保留复核值。

### 工作台与文档

- 中英文 README 同时展示 Codex 监视和中转审计的绿/黄/红/灰状态。
- 新增 [工作台指南](docs/WORKBENCH.md) 与 [中转审计方法](docs/RELAY_AUDIT.md)，明确提示注入、选择性诚实、缓存、模型更新和跨 provider 漂移限制。
- 新增独立的“疑似选择性服务”对照：只在主动审计一致且同 profile 真实会话满足保守门槛时标黄，不改写四轴或模型身份结论。
- 继续以 PolyForm Noncommercial 1.0.0 作为非商业 source-available 许可，并保留与 OpenAI/Anthropic 无隶属或官方背书的声明。

### 发布

- 继续只发布 Windows x64 便携 ZIP、macOS Universal `.app.zip`、Linux x64 AppImage ZIP/tar.gz 和插件 ZIP，不发布 NSIS/MSI/DMG/DEB/RPM 安装器。
- 发布集合必须同时包含 `SHA256SUMS.txt`、SPDX JSON SBOM 和完整第三方许可目录；任一平台失败时不发布残缺 prerelease。
- 三平台 Rust 构建统一重映射工作区、用户目录、Cargo 与 Rustup 路径；打包器会扫描可执行文件或 app bundle，并拒绝含未重映射构建机私有路径的产物。

## [0.1.0-beta.3] - 2026-08-25

### 修复

- 修复 Windows 客户端已经连接并写入请求、但小狸服务线程尚未接受管道实例时，MCP、控制命令或 hook 读取响应会立即报 `ERROR_PIPE_BUSY (231)` 的竞态。
- 客户端现在用同一个硬截止时间分别处理启用 `PIPE_NOWAIT` 前的 `ERROR_PIPE_BUSY (231)`，以及服务端已接受但尚无响应数据时的 `ERROR_NO_DATA (232)`；`PeekNamedPipe` 会把依赖层映射成零字节读取的真实断连与暂时空管道区分开。
- IPC 连接、写入、刷新和读取错误增加阶段说明，便于定位故障，同时不输出会话正文或私人路径。

### 测试

- 新增双门禁 Windows 命名管道回归测试，分别覆盖服务端接受前和接受后、响应前的客户端等待窗口。
- 新增 `231 / 232 / 109` 精确分类测试与真实对端关闭回归，避免未来把断连误判为暂时无数据。

### 准确性

- 模型、effort、显式服务器重路由与行为统计的证据契约不变；修复只恢复实时 IPC，不提高任何结论的证据等级。

## [0.1.0-beta.2] - 2026-08-25

### 修复

- 修复 Windows 实时 IPC 在客户端连接后稍晚写入首字节时，被误判为连接提前关闭的问题；MCP、控制命令与 hook 现在会等待到既定超时，而不是随机回退到离线快照。
- 有界读取完成后恢复命名管道的阻塞模式，避免较大的实时快照响应出现 `WriteZero`、截断或无响应。
- 新增命名管道回归测试，覆盖延迟首字节、1 MiB 延迟读取响应、静默客户端超时恢复、分段请求、超长请求和非法 UTF-8；Unix CI 另验证未完成行的 EOF 语义。

### 准确性

- 模型与 effort 的证据口径保持不变：请求配置、待下回合生效值、显式服务器重路由和行为统计仍严格分栏。
- “未见服务器重路由”仍只表示小狸没有捕获显式 reroute 事件，不等于已经独立确认服务器物理模型。

## [0.1.0-beta.1] - 2026-08-25

### 新增

- 产品统一更名为“小狸 / XiaoLi”。
- Windows、macOS Universal 与 Linux x64 便携包发布流程。
- 无 Node 的 Rust hook、MCP server、插件安装与卸载入口。
- `MonitorSnapshotV4`：TTFT 证据、模型阶段时间、两种观测速率与结构化 `qualityAssessment`。
- 本机同配置行为基线 V2、连续检查点门槛和可选 5.5 统计对照。
- 应用内完整状态与证据说明。
- 中英文 README、快速开始、参考、隐私、开发和故障排查文档。

### 修复

- 修复刷新锁反转导致窗口永久“未响应”。
- 扫描、SQLite 和日志移出 UI 线程；重复请求单飞合并并补一次尾随刷新。
- 15 秒交互超时恢复刷新按钮并保留最后有效快照。
- 活动回合不再把 TTFT、输出速率统一显示成无法解释的破折号。
- 解析 `item_completed` 的结构化时序，并使旧 EOF 缓存失效重建。

### 准确性

- 无显式 reroute 时统一显示“未见服务器重路由”。
- 请求模型、待生效模型、显式服务器路由与行为统计严格分栏。
- 行为异常永远不能生成服务器 reroute 或声称实际模型/effort。

### 分发

- 使用 PolyForm Noncommercial 1.0.0，明确标注 source-available。
- 原私人参考图不进入仓库或 Release；公开再创作授权确认记录在素材来源文件。
