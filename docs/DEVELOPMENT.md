# 开发指南

## 架构

```text
Codex rollout JSONL ─┐
Codex plugin hooks ──┼─> Rust collector ─> V4 snapshot ─┬─> Tauri window/tray
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

## 依赖

- Rust stable，Windows 使用 MSVC toolchain。
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
