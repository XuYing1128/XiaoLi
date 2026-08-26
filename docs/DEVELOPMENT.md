# 开发指南

## 架构

```text
Codex rollout JSONL ─┐
Codex plugin hooks ──┼─> Rust collector ─> V5 snapshot ─┬─> Tauri window/tray/workbench
session_index.jsonl ─┘          │                      ├─> read-only MCP
                               └─> SQLite derived cache└─> probe/compat log
```

关键边界：

- `collector.rs` 只解析结构化字段，rollout 始终只读。
- `model.rs` 定义稳定快照协议。
- `metrics.rs` 负责去重用量、时序区间和保守行为基线。
- `persistence.rs` 保存可重建派生状态；格式变更必须使旧游标失效或迁移。
- `app.rs` 通过单后台刷新协调器串行化扫描与持久化，释放锁后才发 UI/托盘事件。
- `ipc.rs` 隔离当前用户 hook、MCP 和生命周期命令。
- `src/main.ts` 使用以 threadId 为键的稳定 DOM 更新，保留滚动、焦点和折叠状态。
- `connection.rs` 只用脱敏的 provider、endpoint 类别和 auth mode 判断配置来源，不根据行为猜测。
- `relay_transport.rs` 封装三种协议、禁止重定向、限制响应并将凭据绑定原 origin。
- `relay_audit.rs` 是无网络、无 UI 的确定性统计/scorer；`audit_manager.rs` 使用独立工作线程与取消标志。
- `src/workbench.ts` 只渲染结构化脱敏字段，不执行中转响应或把不可信字符串写入 HTML。

## 依赖

- Rust 1.89.0（发布与 CI 固定版本），Windows 使用 MSVC toolchain。
- Node.js 20+ 与 pnpm 9+ 只用于开发前端，不是发布运行时依赖。
- Tauri 2 系统依赖；Linux 需要 WebKitGTK 4.1、AppIndicator、librsvg 与 patchelf。

## 本地检查

```powershell
pnpm install --frozen-lockfile
pnpm run check
pnpm run build
cargo fmt --manifest-path .\src-tauri\Cargo.toml -- --check
cargo clippy --manifest-path .\src-tauri\Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path .\src-tauri\Cargo.toml --locked
```

启动开发窗口：

```powershell
pnpm run tauri:dev
```

## Fixture 测试范围

- 超过 2 MiB 且 `turn_context` 远离尾部的 rollout。
- UTF-8 跨块、半行、坏 JSON、截断、替换、删除与跨日期目录。
- 根任务、并发子智能体和 `subagent_history_start_ordinal` 历史隔离。
- 中途模型/effort 切换与下一回合生效。
- 一次/多次显式 reroute。
- `last_token_usage`、累计差分、计数回退和子任务去重。
- `item_completed` 模型区间并集、TTFT 窗口和终态精确覆盖。
- 基线不足、单信号、双信号连续命中、健康清除和 5.5 对照门槛。
- 刷新单飞、尾随合并和发布阶段锁已释放。
- 来源识别覆盖官方 OAuth/API、托管 provider、自定义/本地端点与冲突证据。
- 本地 mock relay 覆盖三协议、SSE 半包/断流、429、超时、取消、重定向、超大响应和错误 usage。
- 审计覆盖 CSPRNG 顺序、档位硬预算、JSD、标准/深度档的 MMD permutation、无实时官方配对时身份轴证据不足，以及取消后不开新请求。

当前中转质量 fixture 覆盖结构化 JSON、长上下文 nonce 检索、算术/约束推理、多语言、工具选择和状态保持六个域。工具 fixture 必须验证三种协议的真实 tool schema 与受限函数名/字符串参数 scorer，同时断言调用、URL、代码从不执行且原始响应正文不持久化；状态 fixture 只验证单次请求所携带的 `system / user / assistant / user` 多消息历史，不得声称跨网络会话状态机或物理模型证明。`RelayBaselineSummary` 的导入/列出/删除测试只验证元数据隔离，不得把它当成 scorer 输入。`community_baseline.rs` 的编译时参考只能产生低置信相对排名：测试必须断言它不改变 `overallVerdict`、不改变四轴状态，且永远不写入 `actualModel`。

## 前端 mock

Vite 页面支持合成 query fixture，用于状态、层级、滚动和焦点回归。mock 中只能使用明显虚构的 thread/turn ID，禁止复制生产快照。

## 缓存版本

只要新增此前被忽略的 rollout 事件或改变分页/ordinal 语义，就必须提升解析缓存版本。原因是旧缓存可能停在 EOF；如果不失效，它不会回读旧字节，从而让新字段永久缺失。

## 发布构建

- Windows：`tauri build --no-bundle`，再组装便携 ZIP。
- macOS：分别构建 Intel/Apple Silicon，合并 universal 二进制，生成 ad-hoc 签名 `.app.zip`。
- Linux：构建 AppImage，再同时生成 tar.gz 与 ZIP。
- 统一生成 SHA-256、SPDX JSON SBOM 和第三方许可证 HTML。

发布只从干净 staging clone 白名单导入源码。不要在历史工作目录运行 `git add .`。

## 安全发布扫描

提交前必须检查：

- `target`、`dist`、`node_modules`。
- browser profile、SQLite、state、probe、smoke report。
- 真实 thread/turn ID、SID 和绝对用户路径。
- 真实截图和私人参考图。
- 凭据与高熵 secret。

CI 与发行说明见 `.github/workflows/ci.yml` 和 `.github/workflows/release.yml`。
