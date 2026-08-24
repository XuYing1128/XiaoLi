# 小狸 XiaoLi Codex 插件

这是小狸的只读 Codex 插件源。hook 与 MCP 均由同一份 Rust `xiaoli` 可执行文件提供，不需要 Node.js，也不会读取或保存 prompt、回复正文、完整 cwd、工具正文或 transcript 路径。

## 安装

不要直接使用仓库模板中的 `{{XIAOLI_EXECUTABLE}}` 占位符。运行便携包内的命令：

```text
xiaoli --install-plugin
```

安装器会复制插件清单到当前用户目录，并把 `.mcp.json` 与 hook 命令改写成当前 `xiaoli` 可执行文件的绝对路径。之后请在 Codex `/hooks` 中审阅并信任该命令；安装器不会绕过 Codex 的 hook 信任确认。移动便携目录后再次运行该命令即可修复路径，并需要重新审阅新的命令哈希。已运行的 Codex 请新建任务或重启后加载。卸载：

```text
xiaoli --uninstall-plugin
```

## 证据边界

- 模型和 effort 只表示本回合请求配置。
- 只有显式 `model/rerouted` 才显示“服务器已重路由”。
- “未见服务器重路由”不等于物理模型已经独立确认。
- token、缓存、TTFT、速度与行为偏离只用于遥测或黄色“疑似降质”，不能生成虚假的服务器重路由。
- MCP 只返回正在运行的小狸采集器通过 IPC 提供的快照，并标记 `snapshotSource: liveMonitorIpc`。小狸离线时会明确报错，不会把磁盘旧快照冒充为实时状态。

## 本地校验

```powershell
cargo test --manifest-path src-tauri/Cargo.toml
python "$HOME/.codex/skills/.system/plugin-creator/scripts/validate_plugin.py" plugin/xiaoli-model-monitor
```
