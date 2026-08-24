---
name: model-monitor
description: Inspect current Codex task request models, explicit server reroute evidence, requested effort, token usage, cache share, timing, and XiaoLi quality assessments through read-only MCP tools. Use when the user asks what model or effort a Codex task is using, whether it changed, or requests XiaoLi status/details.
---

# 小狸 Codex 模型监视

Use this skill only for read-only inspection of Model Monitor telemetry.

## Workflow

1. Call `get_monitor_summary` for the current overview.
2. If the user names a task or needs turn-level detail, call `get_session_detail` with its full `threadId` and, when available, `turnId`.
3. Call `render_monitor_card` only when a compact user-facing card is useful. Choose `cute` or `minimal` to match the requested style.

## Evidence rules

- Treat `requestedModel` and `requestedEffort` as request settings, not independent proof of physical backend identity or actual thinking depth.
- Only describe a model as server-rerouted when `modelEvidence` is `server-rerouted`. A persisted or inferred observation is weaker and must keep its label.
- If there is no reroute notification, say “未见服务器重路由”. Explain that this only means XiaoLi did not capture an explicit event; it is not physical-model confirmation.
- Always describe effort as requested effort. `reasoningOutputTokens`, duration, throughput, output style, and cache share are telemetry; none independently identifies the model or effort.
- A model or effort changed during an active turn is pending for the next turn unless the active turn itself reports the new request or a server reroute notification.

## Token rules

- `reasoningOutputTokens` is a subset of output tokens; never add it to output again.
- `cachedInputTokens` is a subset of input tokens. Report `cachedInputPercent` as cached-input share, not as a request-level cache hit rate.
- `observedTokensPerSecond` is an end-to-end observation when present, not guaranteed server generation speed.
- `qualityAssessment.suspectedDegradation` is a conservative local statistical warning, not proof that the server changed models or reduced effort.

## Privacy and safety

- The tools are read-only. Do not ask them to change Codex settings or sessions.
- Do not infer or expose prompt text, message bodies, cwd, transcript paths, or raw rollout paths. These fields are intentionally excluded from tool results.
- If health is partial, stale, or unavailable, surface that limitation before drawing conclusions.
- Accept current-state answers only when the tool result says `snapshotSource: liveMonitorIpc`. If XiaoLi reports offline, say that current status is unavailable; never substitute a cached snapshot as live evidence.
