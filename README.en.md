# XiaoLi · 小狸

<p align="center">
  <img src="src/assets/mochi-app-icon.png" width="128" height="128" alt="XiaoLi mascot with purple-gray hair, a fox-cat mask, and looped ahoge" />
</p>

XiaoLi is a compact, draggable, resizable companion that monitors Codex request model, requested reasoning effort, tokens, cached input, timing, and explicit server-reroute evidence.

> Current release: `v0.1.0-beta.3`. XiaoLi is an independent community project. It is not affiliated with or endorsed by OpenAI.

[中文](README.md) · [Portable downloads](https://github.com/XuYing1128/XiaoLi/releases) · [Evidence reference](docs/STATUS_AND_EVIDENCE.md) · [Troubleshooting](docs/TROUBLESHOOTING.md)

## What XiaoLi can verify

- The model and effort requested when the active turn started.
- A model or effort selected during an active turn as a pending next-turn setting.
- An explicit server route only when a `model/rerouted` event was observed.
- Structured token usage, cached-input share, context share, reasoning output, elapsed time, an estimated active TTFT window, and exact terminal timing when reported.
- Conservative local behavior deviations without pretending that behavior identifies a server model.

XiaoLi does not proxy Codex traffic, intercept requests, modify conversations, store message bodies, or patch the Codex shell.

## Status display

Color is never the only signal. The window and tray pair every color with a symbol, text, tooltip, and accessible name.

| Display | Meaning | Important limit |
| --- | --- | --- |
| `✓ Normal` (green) | Active request model and effort match effective task settings; collection is healthy | Green does not independently authenticate the physical server model |
| `! Needs confirmation` (yellow) | A next-turn change is pending, request or token fields are missing, or parsing is partial | Open the conversation for the exact reason |
| `≈ Suspected degradation` (yellow) | At least two independent signals repeatedly deviate from the local same-configuration baseline | It cannot prove that a request was served by 5.5 or with lower effort |
| `× Error` (red) | Request evidence explicitly conflicts, an explicit reroute violates policy, or collection has a deterministic fatal error | This is an evidence-backed conflict, not a style warning |
| `– Idle` (gray) | Codex is not running or no turn is active | XiaoLi keeps waiting in the background |
| `↝ Server rerouted` | An explicit `model/rerouted` event was captured | This is the highest route-evidence level XiaoLi exposes |
| `◇ No server reroute observed` | XiaoLi did not capture an explicit reroute event | It does not prove that physical routing did not change |
| `◷ Pending next turn` | Settings changed while a turn was active | The current turn keeps its original request; the new value applies to a later turn |
| `◌ Learning X/30` | Fewer than 30 healthy samples exist in the matching bucket | No degradation decision is made yet; learning alone does not turn the main status yellow |
| `✓ Behavior consistent` | Enough samples exist and conservative thresholds were not crossed | Behavior consistency is not model authentication |
| `● Collector healthy / warning / pending / failed / offline or unknown` | The collector is healthy, has parse warnings or partial fields, has a deterministic failure, or cannot yet report runtime state | This badge describes collection only; parse warnings do not discard evidence that still parsed correctly |
| `… Waiting for first segment / output / model segment` | Structured item timing or output tokens are not yet sufficient for the corresponding metric | This is a metric waiting state, not a fabricated zero and not an error by itself |
| `⟳ Refreshing / coalesced / timed out / failed` | A manual refresh is scanning, merged into an in-flight refresh, did not return within 15 seconds, or returned an explicit error | This is an interaction notice rather than a server-model conclusion; timeout and failure keep the last valid snapshot, and failure shows a concise reason |

The same guide is built into the app under `… → Status and evidence`.

## Portable setup

### Windows x64

1. Download `XiaoLi-v0.1.0-beta.3-Windows-x64-portable.zip` from [Releases](https://github.com/XuYing1128/XiaoLi/releases).
2. Extract it to a permanent folder.
3. Run `XiaoLi.exe`. The GUI writes or repairs the current-user Codex plugin configuration without Node.js or administrator access.
4. Open `/hooks` in Codex, review the local `xiaoli-model-monitor` hook, and explicitly trust it. XiaoLi never bypasses Codex's hook trust prompt.
5. If Codex was already running, start a new task or restart Codex before beginning a turn.

The beta is unsigned, so SmartScreen may warn. Verify the published SHA-256 and use the operating system's normal confirmation flow. XiaoLi never asks you to disable SmartScreen.

### macOS Universal

Extract `XiaoLi-v0.1.0-beta.3-macOS-universal.app.zip`, move `XiaoLi.app` to Applications, and open it. The beta is ad-hoc signed but not notarized. If Gatekeeper blocks it, verify the download and use Privacy & Security's “Open Anyway” control. Do not disable Gatekeeper.

### Linux x64

Extract the tarball, or extract the ZIP and grant the AppImage execute permission once:

```bash
chmod +x XiaoLi-x86_64.AppImage
./XiaoLi-x86_64.AppImage
```

Always-on-top, tray, and absolute positioning are best effort under Wayland and more complete under X11.

## Accuracy boundaries

`activeRequest.model` and `activeRequest.effort` are request evidence. Effort is always labeled “requested”; reasoning tokens are usage, not a measurement of actual thinking intensity.

Only an explicit `model/rerouted` event creates route evidence. Timing, token, cache, and behavioral characteristics can only create a yellow warning because network latency, queuing, tools, cache, input shape, and system load are confounders.

An active TTFT is displayed as “waiting for first segment” or as an estimated `A–B` window derived from structured model-item timing. Only a terminal structured report is labeled exact. End-to-end output rate includes waiting and tools; model-phase rate uses the union of Reasoning and AgentMessage time intervals. Neither is pure server generation TPS.

See [Status and evidence](docs/STATUS_AND_EVIDENCE.md) for the thresholds and examples.

## Plugin and CLI

The `xiaoli-model-monitor` plugin uses the same Rust executable, with no Node runtime. After the first install, an upgrade, or moving the portable executable, review and trust the updated command in Codex `/hooks`; writing configuration does not mean the hook was automatically activated.

```text
xiaoli --hook-capture
xiaoli --mcp-server
xiaoli --install-plugin
xiaoli --uninstall-plugin
```

Read-only MCP tools expose the active summary, one session's detail, and a monitor-card payload. Hook payloads contain event type, thread/turn IDs, requested model, and timestamps only; requested effort comes from structured `turn_context` evidence.

Current-state MCP results must come from the running XiaoLi collector over IPC and carry `snapshotSource: liveMonitorIpc`. If XiaoLi is offline or IPC is unavailable, the tool returns an explicit error instead of presenting an old disk snapshot as the current model.

Probe and lifecycle commands:

```text
xiaoli --probe-once [--sessions-root PATH] [--session-index PATH] [--state-root PATH]
xiaoli --show
xiaoli --hidden
xiaoli --stop
```

## Privacy and local data

- Windows: `%LOCALAPPDATA%\XiaoLi`
- macOS: `~/Library/Application Support/XiaoLi`
- Linux: `$XDG_DATA_HOME/xiaoli` or `~/.local/share/xiaoli`

SQLite stores rebuildable cursors, aggregates, and behavior samples. Raw rollout files remain the source of truth. Probe output and compatibility logs exclude prompt/reply bodies and full working-directory paths.

## Development

```powershell
pnpm install --frozen-lockfile
pnpm run check
pnpm run build
cargo fmt --manifest-path .\src-tauri\Cargo.toml -- --check
cargo clippy --manifest-path .\src-tauri\Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path .\src-tauri\Cargo.toml --locked
```

See [Development](docs/DEVELOPMENT.md), [Contributing](CONTRIBUTING.md), [Troubleshooting](docs/TROUBLESHOOTING.md), and [Changelog](CHANGELOG.md).

## License

XiaoLi is source-available under [PolyForm Noncommercial 1.0.0](LICENSE). Commercial use is not licensed. Because of that restriction, XiaoLi is not “open source” under the OSI definition. Character assets also follow [ASSET_PROVENANCE.md](ASSET_PROVENANCE.md); third-party dependencies retain their own licenses.
