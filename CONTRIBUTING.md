# 为小狸贡献

感谢你改进小狸。提交前请先阅读 [证据边界](docs/STATUS_AND_EVIDENCE.md) 和 [设计系统](DESIGN.md)。

## 不可破坏的规则

- 请求模型不能称为服务器实际模型。
- effort 必须标记为请求值。
- 只有显式 `model/rerouted` 可以创建服务器路由证据。
- 行为、速度、Token、缓存和文本风格最多产生黄色提醒。
- 不读取、存储或输出 prompt、回复、推理、命令或工具正文。
- 解析语义变化必须处理旧 EOF 游标。
- 实时刷新不能在 UI 主线程扫描或持刷新锁更新托盘。

## 开始开发

1. Fork 并创建功能分支。
2. 安装 Rust stable、Node.js 20+ 和 pnpm 9+。
3. 运行 [开发指南](docs/DEVELOPMENT.md#本地检查) 中的全部检查。
4. 为 bug 加回归 fixture；为协议字段加 camelCase/snake_case 兼容测试。
5. 更新 README、参考文档和 CHANGELOG。

## 提交内容

不要提交：

- 真实 rollout、SQLite、monitor 日志或 probe 输出。
- 真实 thread/turn ID、SID、用户名或绝对用户路径。
- 浏览器 profile、生产截图或私人参考图。
- `target`、`dist`、`node_modules` 或本地发行包。

截图必须使用合成 fixture。示例 ID 应明显虚构，例如 `00000000-0000-4000-8000-000000000101`。

## Pull Request 检查表

- [ ] TypeScript check 与生产构建通过。
- [ ] `cargo fmt`、`clippy -D warnings`、`cargo test --locked` 通过。
- [ ] 插件 validator、hook/MCP self-test 和 `--probe-once` 通过。
- [ ] 新增行为判断不会创建虚假 reroute。
- [ ] 刷新期间 UI heartbeat、拖动、滚动与关闭仍响应。
- [ ] 无真实 ID、路径、数据库、截图或 secret。
- [ ] 文档和状态说明已同步。

## 许可证

提交即表示你有权贡献该内容，并同意它按 [PolyForm Noncommercial 1.0.0](LICENSE) 提供。不要提交与该非商业分发不兼容的代码或素材。
