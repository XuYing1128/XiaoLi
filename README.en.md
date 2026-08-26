# XiaoLi · 小狸

<p align="center">
  <img src="src/assets/mochi-app-icon.png" width="128" height="128" alt="XiaoLi mascot with purple-gray hair, a fox-cat mask, and looped ahoge" />
</p>

<p align="center">A compact Codex evidence monitor plus a local workbench for conversation history and black-box relay audits.</p>

> Current release: `v0.2.0-beta.1`. XiaoLi is an independent community project. It is not affiliated with or endorsed by OpenAI, Anthropic, or any model or relay provider. It is a beta-stage black-box and local-statistics tool and cannot guarantee error-free results.

[中文](README.md) · [Portable downloads](https://github.com/XuYing1128/XiaoLi/releases) · [Workbench guide](docs/WORKBENCH.md) · [Relay-audit method](docs/RELAY_AUDIT.md) · [Signed static baselines](docs/SIGNED_BASELINES.md) · [Community references](docs/COMMUNITY_BASELINES.en.md) · [Evidence reference](docs/STATUS_AND_EVIDENCE.md)

## What XiaoLi can verify

- The model and effort requested when the active Codex turn started.
- A model or effort selected during an active turn as a pending next-turn setting.
- An explicit server route only when a `model/rerouted` event was observed.
- Structured token usage, cached-input share, context share, reasoning output, elapsed time, an estimated active TTFT window, and exact terminal timing when reported.
- The configured connection origin, using explicit provider, sanitized endpoint class, and authentication-mode evidence.
- Conservative behavioral or relay-audit deviations without pretending that behavior identifies a physical model.

XiaoLi does not proxy Codex traffic, intercept or modify conversations, store message bodies, patch the Codex shell, or silently reuse Codex OAuth/API credentials.

## The five-page local workbench

Open **XiaoLi Workbench** from the tray or compact-window menu:

- **Overview** — active conversations, deduplicated token totals, origin classes, four audit axes, and recent alerts.
- **Conversation history** — filter derived metrics by date, model, effort, origin, and status. Prompt and response bodies are not retained.
- **Relay audit** — enter an endpoint you are authorized to test, select OpenAI Responses, OpenAI Chat Completions, or Anthropic Messages, test connectivity, review hard budgets, and explicitly start an audit.
- **References** — inspect imported official/community/user material. Plain or unverified summaries remain metadata-only. A user may explicitly import an independent Ed25519 trust anchor and then verify a package whose signature covers all applicability parameters and normalized cell distributions. A verified, matching, unexpired package is only a low-confidence static fingerprint reference when no live pair exists; it cannot by itself make the overall verdict consistent or prove a physical model. Only a live official endpoint paired with the same protocol and exact model in the current audit provides medium/high-confidence statistical reference evidence.
- **Method and status** — an in-app explanation of the four evidence axes, green/yellow/red/gray states, and what each conclusion cannot prove.

The workbench is a normal, non-topmost window and is separate from the small always-on-top monitor. Active audits have their own worker and never share the Codex collector's refresh lock.

## Four independent relay-audit axes

XiaoLi deliberately does not produce a misleading “96% real model” score.

| Axis | What it checks | What it cannot prove |
| --- | --- | --- |
| Protocol compatibility | Authentication, response envelope, SSE termination, self-reported model, and error-contract behavior | A correct API surface does not authenticate the physical backend |
| Usage consistency | Usage arithmetic, subset invariants, controlled input sizes, and matched paired references when available | Hidden system prompts, reasoning, or provider wrappers can make a local absolute count unknowable |
| Behavioral quality | Structured JSON, long-context nonce retrieval, arithmetic/constraints, multilingual tasks, real tool schemas, and same-request multi-message state retention | Tool checks score only allowlisted structured function names/string arguments, never execute calls, and do not retain raw response bodies; state retention is not proof across network sessions, and style, speed, or one task cannot establish degradation |
| Model identity | The API-reported model name; with live official pairing, single-token distributions and statistical distance; without a live pair, an explicitly selected verified/matching/unexpired signed package can provide a low-confidence static comparison | Neither a fingerprint, a package signature, nor a reported name is a physical-model certificate; a static-consistent result cannot by itself make the overall verdict consistent |

Even a fully passing report says only: **no significant anomaly was found within this run's scope and reference conditions; the physical serving model was not cryptographically proven.**

## Status display

Color is never the only signal. The windows and tray pair every color with a symbol, text, tooltip, and accessible name.

### Codex monitor states

| Display | Meaning | Important limit |
| --- | --- | --- |
| `✓ Normal` (green) | Active request model and effort match effective task settings; collection is healthy | Green does not independently authenticate the physical server model |
| `! Needs confirmation` (yellow) | A next-turn change is pending, fields are missing, or parsing is partial | Open the conversation for the exact reason |
| `≈ Suspected degradation` (yellow) | At least two independent signals repeatedly deviate from a local same-configuration baseline | It cannot prove that 5.5 served the request or that effort was lowered |
| `× Error` (red) | Request evidence explicitly conflicts, an explicit reroute violates policy, or collection has a deterministic fatal error | This is an evidence-backed conflict, not a style warning |
| `– Idle` (gray) | Codex is not running or no turn is active | XiaoLi keeps waiting locally |
| `↝ Server rerouted` | An explicit `model/rerouted` event was captured | This is XiaoLi's highest route-evidence level |
| `! Rerouted, target unknown` | An explicit reroute was captured without a displayable target model | Confirms a route event only; XiaoLi does not guess the target |
| `◇ No server reroute observed` | No explicit reroute was captured | It does not prove that physical routing did not change |
| `⌁ Connection origin` | Provider, endpoint class, and auth mode form complete, partial, or conflicting evidence | Classifies configured origin only; it does not identify the physical backend |
| `◷ Pending next turn` | Settings changed while a turn was active | The current request is not rewritten |
| `◌ Learning X/30` | Too few healthy samples exist in the matching bucket | Learning alone is not an anomaly |
| `● Collection healthy / warning / needs confirmation / failed / unavailable` | The collector reports its own parsing and runtime health | Collection status does not authenticate a server model |
| `… Waiting for first segment / output / model segment` | Structured timing or token data is not yet sufficient for the metric | A waiting metric is not zero and is not an anomaly by itself |
| `↻ Refreshing / coalesced / timed out / failed` | Background refresh interaction state | The last valid snapshot is retained; this is not a server-model conclusion |

### Relay-audit states

| Display | Meaning | Important limit |
| --- | --- | --- |
| Green · consistent with reference | No significant anomaly within this run and its matched reference conditions | Not a physical-model certificate |
| Yellow · suspected padding/degradation/significant difference | Conservative multi-scale, multi-domain, or distribution thresholds were crossed | A reproducible suspicion, not proof of intent or a named replacement model |
| Yellow · anti-evasion behavior anomaly | In standard/deep mode, at least two of response-distribution collapse, unusually low/stable latency, paraphrase drift, and role/format sensitivity persist across both independent batches | Independent behavior evidence only; it never changes the four axes, overall verdict, or model identity |
| Yellow · suspected selective service | A consistent active audit conflicts with enough conservative degradation warnings from recent real turns bound to the same local profile | This is an independent statistical mismatch; it does not change the four axes or prove selective routing |
| Red · confirmed contract mismatch | Reproducible impossible usage arithmetic, a wrong self-reported model, or a stable declared protocol contradiction | Establishes a contract contradiction, not the backend's identity |
| Red · audit failed | No operation produced a parseable successful response, or the audit worker failed deterministically | No model-identity conclusion was formed |
| Gray · cancelled | The user cancelled; no new request is started | Completed structured evidence may remain, but the run is not a pass |
| Gray · insufficient evidence | Missing samples, no applicable reference, uncontrollable parameters, or an unsupported check | Gray is not a pass |

The same guide is built into the workbench under **Method and status**.

## Origin classification and automatic mode

`MonitorSnapshotV5` adds `connectionOrigin` to every conversation. Evidence precedence is the conversation's `session_meta.model_provider`, effective Codex provider/base URL, a sanitized hook endpoint class, and authentication mode parsed without retaining credential fields.

- “Official ChatGPT” or “official API” appears only when a first-party endpoint and matching authentication mode both agree.
- A custom or local endpoint means “eligible for relay testing,” not “malicious relay.”
- Conflicting CLI/environment/configuration evidence produces `unknown`; XiaoLi never guesses origin from speed, style, or token behavior.
- Official login stays in passive Codex-monitor mode. A custom endpoint still requires a separately saved `RelayProfile`, the user's own credential, and explicit budget confirmation before any active request.

## Active-audit budgets

| Mode | Scope | Hard request limit per endpoint |
| --- | --- | ---: |
| Connection | Authentication, target model catalog, basic non-streaming response, SSE | 6 (currently normally 3) |
| Quick | 8 cells × 15, small samples in all six quality domains, plus basic protocol/usage checks | 150 |
| Standard | 16 cells × 15, more samples in all six quality domains, plus full distribution comparison | 320 |
| Deep | 40 cells × 15, higher sample counts in all six quality domains, plus the stability matrix | 720 |

The UI displays request, input-token, output-token, and timeout caps before starting. A paired official comparison costs additional requests and requires a separately configured first-party profile with the same protocol and exact model plus its own credential. Without live pairing, XiaoLi can still check protocol behavior and usage arithmetic and collect six target-side deterministic probe domains; relative quality stays learning/insufficient. Model identity also stays **insufficient evidence** unless the user explicitly selects a verified, matching, unexpired signed static package. Such a package provides only a low-confidence `referenceConsistent` / `referenceDifferent` comparison: a consistent result cannot by itself make the overall verdict consistent and never proves the physical serving model. Imported summary metadata is never substituted for either a live pair or a verified scorer package. Network attempts count against the cap.

An endpoint profile may optionally reference a local private probe-pack JSON file. XiaoLi persists only its canonical path, version, and SHA-256; task prompts and expected answers are read ephemerally at audit start. A missing or changed file fails before any network request. The confirmation dialog shows the pack's additional per-endpoint requests and conservative token allowance. Built-in and private cases are never silently truncated: if the complete randomized plan exceeds any confirmed hard cap, the audit refuses to start. See the strict schema and limits in the [relay-audit method](docs/RELAY_AUDIT.md#用户私有-probe-pack).

Quick mode collects a small number of quality samples to exercise the path, but it does not have enough samples to issue a quality-consistent or degradation verdict; that axis remains learning. Standard and Deep are the modes that run the paired quality comparison at the current evidence threshold. The input preview uses the largest real wire-body reservation across every generatable randomized variant, so the reservation for the eventual CSPRNG seed can only be lower or equal. A future `runSeed` is not exposed before execution; the completed report retains it for local review. A non-localhost plaintext HTTP endpoint is allowed only for a manual connection test or audit after a per-run interception warning; scheduled audits reject it. HTTPS and localhost do not require that extra warning.

Scheduled audits are off by default. Enabling them requires an explicitly bound profile and persistent OS credential, uses Quick mode, applies a local ±30-minute jitter, and obeys a monthly request ceiling. Official pairing is a separate opt-in.

## Anti-evasion and prompt-injection boundary

Every run receives an operating-system CSPRNG seed. XiaoLi randomizes task parameters, wording, language, whitespace, and request order; fixed final prompts are not stored in the repository. It also compares original and rephrased audit cases.

For a custom, managed, or local endpoint, XiaoLi can conservatively bind a real Codex turn to exactly one saved local `RelayProfile` when the turn-bound endpoint class and private endpoint-scope fingerprint agree. That scope is the normalized scheme, host, effective port, and API base path; it excludes userinfo, query, and fragment, so different ports or API paths on one host are not treated as the same relay. The fingerprint is never exposed or persisted. Only after an active audit is consistent with its matched live reference does XiaoLi compare the profile's completed real turns from the previous 30 days. It displays an independent yellow **suspected selective service** warning only with at least 10 completed turns and when at least 5, and at least half, retain a conservative degradation warning. This comparison never changes the four axes or overall verdict and cannot prove selective routing or a physical model identity.

Relay output is untrusted data:

- Responses cannot execute tools, code, commands, HTML, scripts, URLs, or “instructions for the detector.”
- HTTP redirects are disabled, and credentials stay bound to the origin the user confirmed.
- JSON/SSE event size, total response size, depth, and retained strings are bounded.
- The UI renders untrusted values as text, not HTML.
- Relay observations, live official-pair results, unverified imported metadata, trusted signed packages, and release-pinned community distributions stay logically and physically separate. Only a user-selected verified/matching/unexpired signed package can enter the low-confidence static fingerprint scorer; built-in cross-protocol distributions still cannot change the four axes or overall verdict.

A relay that recognizes all audit traffic through TLS or traffic shape can selectively forward an honest model. No black-box client can rule that out. XiaoLi raises the cost of evasion and preserves reproducible evidence; it does not promise an unbypassable proof.

## Portable setup

### Windows 10/11 x64

1. Download `XiaoLi-v0.2.0-beta.1-Windows-x64-portable.zip` from [Releases](https://github.com/XuYing1128/XiaoLi/releases).
2. Extract it into a permanent parent folder such as `D:\Apps`; the archive already contains a top-level `XiaoLi` directory.
3. Run `D:\Apps\XiaoLi\XiaoLi.exe`.
4. The GUI writes or repairs the current-user Codex plugin path without Node.js or administrator access.
5. Open `/hooks` in Codex, review the local `xiaoli-model-monitor` hook, and explicitly trust it.

This unsigned beta may trigger SmartScreen. Verify `SHA256SUMS.txt` and use Windows' normal **More info** flow. XiaoLi never asks you to disable SmartScreen.

### macOS 12+, Intel and Apple Silicon

Extract `XiaoLi-v0.2.0-beta.1-macOS-universal.app.zip`, move `XiaoLi.app` to Applications, and open it. The beta is ad-hoc signed but not notarized. If Gatekeeper blocks it, verify the checksum and use Privacy & Security's **Open Anyway** control. Do not disable Gatekeeper.

### Linux x64

Extract `XiaoLi-v0.2.0-beta.1-Linux-x64-portable.tar.gz`, or extract `XiaoLi-v0.2.0-beta.1-Linux-x64-portable.zip` and grant the AppImage execute permission once:

```bash
cd XiaoLi
chmod +x XiaoLi-x86_64.AppImage
./XiaoLi-x86_64.AppImage
```

If normal launch reports that FUSE is unavailable, use the AppImage's extraction fallback:

```bash
./XiaoLi-x86_64.AppImage --appimage-extract-and-run
```

Always-on-top, tray, and absolute positioning are best effort under Wayland and more complete under X11. If Secret Service is unavailable, credentials remain memory-only; XiaoLi never falls back to a plaintext key file.

## Accuracy boundaries for Codex monitoring

`activeRequest.model` and `activeRequest.effort` are request evidence. Effort is always labeled “requested”; reasoning tokens are usage, not a measurement of actual thinking intensity.

Only an explicit `model/rerouted` event creates route evidence. Timing, tokens, cache, and behavior may create a yellow warning, because latency, queuing, tools, cache, input shape, and system load are confounders.

An active TTFT is an estimated `A–B` window from structured model-item timing. Only a terminal structured report is labeled exact. End-to-end output rate includes waiting and tools; model-phase rate is an estimate over the union of Reasoning and AgentMessage intervals. Neither is pure server generation TPS.

## Plugin, MCP, and CLI

The `xiaoli-model-monitor` plugin uses the same Rust executable, with no Node runtime:

```text
xiaoli --hook-capture
xiaoli --mcp-server
xiaoli --install-plugin
xiaoli --uninstall-plugin
```

Read-only MCP tools expose the active summary, session detail, monitor card, sanitized connection origin, and existing relay-audit reports. MCP cannot start a billable audit. Current-state results must come from the live XiaoLi collector over IPC and carry `snapshotSource: liveMonitorIpc`; an old disk snapshot is never presented as current.

```text
xiaoli --probe-once [--sessions-root PATH] [--session-index PATH] [--state-root PATH]
xiaoli --show
xiaoli --hidden
xiaoli --stop
```

`--probe-once` emits `MonitorSnapshotV5` without writing production logs or parse cache.

## Privacy and local data

- Windows: `%LOCALAPPDATA%\XiaoLi`
- macOS: `~/Library/Application Support/XiaoLi`
- Linux: `$XDG_DATA_HOME/xiaoli` or `~/.local/share/xiaoli`

SQLite stores rebuildable cursors, derived conversation metrics, aggregates, imported reference metadata, explicitly trusted public keys, verified normalized one-word count distributions, normalized relay profiles, and sanitized reports. A selected private probe pack contributes only its canonical local path, version, and SHA-256 to the profile/report; its body is not copied. Unverified summaries do not enter scoring. Trusted packages and relay samples use separate tables; packages are reverified and checked for expiry on load, and revoking a key removes its scoring packages. Release-pinned community distributions contain normalized counts only, not prompt/reply bodies. SQLite does not store API keys, authentication tokens, prompt/reply bodies, full working-directory paths, or raw relay responses.

API keys are memory-only by default. Explicit opt-in uses Windows Credential Manager, macOS Keychain, or Linux Secret Service. If the OS credential store is unavailable, XiaoLi remains memory-only and shows a warning.

See [Privacy](docs/PRIVACY.md) and [Relay-audit method](docs/RELAY_AUDIT.md).

## Development

```powershell
pnpm install --frozen-lockfile
pnpm run check
pnpm run build
cargo fmt --manifest-path .\src-tauri\Cargo.toml -- --check
cargo clippy --manifest-path .\src-tauri\Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path .\src-tauri\Cargo.toml --locked
```

See [Getting started](docs/GETTING_STARTED.md), [CLI and plugin reference](docs/CLI_AND_PLUGIN.md), [Development](docs/DEVELOPMENT.md), [Contributing](CONTRIBUTING.md), [Troubleshooting](docs/TROUBLESHOOTING.md), [Design](DESIGN.md), [Security](SECURITY.md), [Third-party notices](THIRD_PARTY_NOTICES.md), [Release tooling](scripts/release/README.md), and [Changelog](CHANGELOG.md).

## License

XiaoLi is source-available under [PolyForm Noncommercial 1.0.0](LICENSE). Commercial use is not licensed. Because of that restriction, XiaoLi is not “open source” under the OSI definition. Character assets also follow [ASSET_PROVENANCE.md](ASSET_PROVENANCE.md); third-party dependencies retain their own licenses.
