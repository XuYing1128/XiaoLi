import { invoke } from "@tauri-apps/api/core";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type ThemeName = "cute" | "minimal";
type StatusLevel = "green" | "yellow" | "red" | "gray";
type ConnectionOriginKind = "officialChatGpt" | "officialOpenAiApi" | "officialAnthropicApi"
  | "managedProvider" | "customEndpoint" | "localEndpoint" | "unknown";
type ConnectionAuthMode = "chatGpt" | "apiKey" | "external" | "unknown";
type ConnectionOriginConfidence = "configured" | "partial" | "unknown";
type EndpointClass = "officialChatGpt" | "officialOpenAi" | "officialAnthropic"
  | "managedProvider" | "customEndpoint" | "localEndpoint" | "unknown";

interface ConnectionOriginSnapshot {
  kind: ConnectionOriginKind;
  authMode: ConnectionAuthMode;
  confidence: ConnectionOriginConfidence;
  providerId?: string;
  endpointClass: EndpointClass;
  evidence: string[];
  limitations: string[];
}

interface TokenUsage {
  inputTokens?: number;
  cachedInputTokens?: number;
  cacheWriteInputTokens?: number;
  outputTokens?: number;
  reasoningOutputTokens?: number;
  totalTokens?: number;
}

interface RequestEvidence {
  model?: string;
  effort?: string;
  source?: string;
}

interface RouteHop {
  fromModel?: string;
  toModel?: string;
  reason?: string;
  timestamp?: string;
  association?: string;
}

type TtftEvidenceKind = "pending" | "estimatedWindow" | "exactTerminal";

interface TtftEvidence {
  kind: TtftEvidenceKind;
  lowerMs?: number;
  upperMs?: number;
}

interface QualityFactor {
  code: string;
  direction?: string;
  observed?: number;
  baselineMedian?: number;
  mad?: number;
  robustDeviation?: number;
  unit?: string;
}

interface QualityAssessment {
  state: "learning" | "consistent" | "suspectedDegradation";
  baselineKey?: string;
  baselineSampleCount: number;
  consecutiveHits: number;
  factors: QualityFactor[];
  comparator?: {
    requestedModel?: string;
    comparedModel?: string;
    sampleCount?: number;
    relativeDistance?: number;
  };
  limitations: string[];
}

interface ConversationSnapshot {
  threadId: string;
  turnId?: string;
  parentThreadId?: string;
  kind: string;
  title: string;
  sourceTimestamp?: string;
  activeRequest: RequestEvidence;
  pendingNextTurn?: RequestEvidence;
  serverRoute: { model?: string; evidence: string; observedAt?: string; chain: RouteHop[] };
  usage: {
    last: TokenUsage;
    cumulative: TokenUsage;
    lastCacheInputShare?: number;
    cacheInputShare?: number;
    contextWindow?: number;
    contextInputShare?: number;
  };
  timing: {
    elapsedMs?: number;
    ttftMs?: number;
    durationMs?: number;
    ttftEvidence: TtftEvidence;
    modelActiveMs?: number;
    endToEndOutputRate?: number;
    modelPhaseOutputRate?: number;
    observedOutputRate?: number;
  };
  qualityAssessment: QualityAssessment;
  connectionOrigin: ConnectionOriginSnapshot;
  status: { level: StatusLevel; code: string; explanation: string };
  anomalies: string[];
}

interface MonitorSnapshotV5 {
  schemaVersion: number;
  checkedAt: string;
  codexRunning: boolean;
  collectorHealth: { level: StatusLevel; parseWarnings: number; lastError?: string };
  conversations: ConversationSnapshot[];
}

interface UiState {
  snapshot: MonitorSnapshotV5;
  expanded: boolean;
  topmost: boolean;
  theme: ThemeName;
  refreshing: boolean;
  connected: boolean;
  openThreads: Set<string>;
  openAdvanced: Map<string, boolean>;
  autoOpenedRootId?: string;
  menuOpen: boolean;
  statusGuideOpen: boolean;
  refreshNotice?: string;
  pluginNotice?: string;
}

interface UiPreferencesV2 {
  version: number;
  theme: ThemeName;
  topmost: boolean;
  expanded: boolean;
  compactBounds?: { width: number; height: number };
  expandedBounds?: { width: number; height: number };
}

interface RefreshCommandResult {
  status: "completed" | "coalesced";
  snapshot: unknown;
}

interface PluginInstallStatus {
  ok: boolean;
  changed?: boolean;
  message?: string;
  error?: string;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
    __MOCHI_MOCK__?: unknown;
    __XIAOLI_MOCK__?: unknown;
  }
}

const STATUS_ORDER: Record<StatusLevel, number> = { gray: 0, green: 1, yellow: 2, red: 3 };
const STATUS_COPY: Record<StatusLevel, { short: string; long: string; symbol: string }> = {
  green: { short: "正常", long: "配置一致", symbol: "✓" },
  yellow: { short: "待确认", long: "需要确认", symbol: "!" },
  red: { short: "异常", long: "明确冲突", symbol: "×" },
  gray: { short: "空闲", long: "暂无活动", symbol: "–" },
};
const URL_OPTIONS = new URLSearchParams(window.location.search);
const MOCK_QUERY = URL_OPTIONS.get("mock");
const IS_TAURI = Boolean(window.__TAURI_INTERNALS__);
const existingMount = document.querySelector<HTMLElement>("#app");
const app = existingMount ?? document.createElement("div");
if (!existingMount) {
  app.id = "app";
  document.body.replaceChildren(app);
}

document.documentElement.lang = "zh-CN";
document.title = "小狸 · XiaoLi";

const state: UiState = {
  snapshot: emptySnapshot(),
  expanded: Boolean(MOCK_QUERY && URL_OPTIONS.get("expanded") === "1"),
  topmost: true,
  theme: readStoredTheme(),
  refreshing: false,
  connected: false,
  openThreads: new Set<string>(),
  openAdvanced: new Map<string, boolean>(),
  autoOpenedRootId: undefined,
  menuOpen: false,
  statusGuideOpen: Boolean(MOCK_QUERY && URL_OPTIONS.get("guide") === "1"),
};
let unlistenSnapshot: UnlistenFn | undefined;
let unlistenPreferences: UnlistenFn | undefined;
let unlistenPluginInstall: UnlistenFn | undefined;
let safetyPoll: number | undefined;
let renderFrame: number | undefined;
let scrollRestoreFrame: number | undefined;
let scrollInteractionGeneration = 0;
let scrollPointerActive = false;
let resizeObserver: ResizeObserver | undefined;
let snapshotEventRevision = 0;
let snapshotLoadSerial = 0;
let refreshRequestSerial = 0;
let preferencesEventRevision = 0;
let preferencesLoadSerial = 0;
let statusGuideReturnFocus: HTMLElement | null = null;

function emptySnapshot(): MonitorSnapshotV5 {
  return {
    schemaVersion: 5,
    checkedAt: new Date().toISOString(),
    codexRunning: false,
    collectorHealth: { level: "gray", parseWarnings: 0 },
    conversations: [],
  };
}

function mockSnapshot(): MonitorSnapshotV5 {
  const now = Date.now();
  const base: ConversationSnapshot = {
    threadId: "00000000-0000-4000-8000-000000000101",
    turnId: "00000000-0000-4000-8000-000000000201",
    kind: "root",
    title: "小狸界面优化示例",
    sourceTimestamp: new Date(now - 4_300).toISOString(),
    activeRequest: { model: "gpt-5.6-sol", effort: "ultra", source: "hook + turn_context" },
    serverRoute: { evidence: "notObserved", chain: [] },
    usage: {
      last: {
        inputTokens: 164_857,
        cachedInputTokens: 162_560,
        outputTokens: 415,
        reasoningOutputTokens: 97,
        totalTokens: 165_272,
      },
      cumulative: {
        inputTokens: 7_742_738,
        cachedInputTokens: 7_191_936,
        outputTokens: 21_785,
        reasoningOutputTokens: 9_536,
        totalTokens: 7_764_523,
      },
      cacheInputShare: 0.9289,
      contextWindow: 258_400,
    },
    timing: {
      elapsedMs: 48_200,
      ttftEvidence: { kind: "estimatedWindow", lowerMs: 7_800, upperMs: 9_958 },
      modelActiveMs: 19_400,
      endToEndOutputRate: 8.6,
      modelPhaseOutputRate: 21.4,
      observedOutputRate: 8.6,
    },
    qualityAssessment: {
      state: "learning",
      baselineKey: "gpt-5.6-sol|ultra|128k+|257-1024|no-tools",
      baselineSampleCount: 18,
      consecutiveHits: 0,
      factors: [],
      limitations: ["同桶健康样本不足 30 个，暂不判断行为一致性"],
    },
    connectionOrigin: {
      kind: "officialChatGpt",
      authMode: "chatGpt",
      confidence: "configured",
      providerId: "openai",
      endpointClass: "officialChatGpt",
      evidence: ["sessionProvider", "authMode"],
      limitations: [],
    },
    status: {
      level: "green",
      code: "request_consistent",
      explanation: "Hook 与 rollout 记录的本回合请求值一致",
    },
    anomalies: [],
  };
  if (MOCK_QUERY === "collector-red") {
    return {
      schemaVersion: 5,
      checkedAt: new Date().toISOString(),
      codexRunning: false,
      collectorHealth: { level: "red", parseWarnings: 0, lastError: "命名管道不可用" },
      conversations: [],
    };
  }
  if (MOCK_QUERY === "origin-custom" || MOCK_QUERY === "origin-unknown") {
    const originFixture: ConversationSnapshot = {
      ...base,
      connectionOrigin: MOCK_QUERY === "origin-custom" ? {
        kind: "customEndpoint",
        authMode: "apiKey",
        confidence: "configured",
        providerId: "community-relay",
        endpointClass: "customEndpoint",
        evidence: ["sessionProvider", "providerEndpoint", "authMode"],
        limitations: ["physicalModelUnproven"],
      } : normalizeConnectionOrigin(undefined),
    };
    return {
      schemaVersion: 5,
      checkedAt: new Date().toISOString(),
      codexRunning: true,
      collectorHealth: { level: "green", parseWarnings: 0 },
      conversations: [originFixture],
    };
  }
  if (MOCK_QUERY === "route-red-generic" || MOCK_QUERY === "route-conflict") {
    const explicitRoute: ConversationSnapshot = {
      ...base,
      serverRoute: {
        model: "gpt-5.6-terra",
        evidence: "model/rerouted",
        observedAt: new Date(now - 2_000).toISOString(),
        chain: [{ fromModel: "gpt-5.6-sol", toModel: "gpt-5.6-terra", timestamp: new Date(now - 2_000).toISOString() }],
      },
      status: MOCK_QUERY === "route-conflict"
        ? { level: "red", code: "server_reroute_conflict", explanation: "服务器重路由目标与任务策略冲突" }
        : { level: "red", code: "hook_context_conflict", explanation: "Hook 与 turn_context 请求证据冲突" },
    };
    return {
      schemaVersion: 5,
      checkedAt: new Date().toISOString(),
      codexRunning: true,
      collectorHealth: { level: "green", parseWarnings: 0 },
      conversations: [explicitRoute],
    };
  }
  if (!["multi", "hierarchy", "pending", "scroll"].includes(MOCK_QUERY ?? "")) {
    return {
      schemaVersion: 5,
      checkedAt: new Date().toISOString(),
      codexRunning: true,
      collectorHealth: { level: "green", parseWarnings: 0 },
      conversations: [base],
    };
  }
  const root: ConversationSnapshot = {
    ...base,
    threadId: "019d-terra-demo-root",
    turnId: "turn-terra",
    title: "检查模型切换",
    activeRequest: { model: "gpt-5.6-terra", effort: "high", source: "hook" },
    pendingNextTurn: { model: "gpt-5.6-sol", effort: "ultra", source: "thread_settings" },
    usage: {
      last: { inputTokens: 18_420, cachedInputTokens: 14_880, outputTokens: 892, totalTokens: 19_312 },
      cumulative: { inputTokens: 81_224, cachedInputTokens: 61_800, outputTokens: 4_106, totalTokens: 85_330 },
      cacheInputShare: 0.7608,
      contextWindow: 258_400,
    },
    timing: { elapsedMs: 26_100, ttftEvidence: { kind: "pending" }, endToEndOutputRate: 34.2, observedOutputRate: 34.2 },
    status: { level: "yellow", code: "pending_next_turn", explanation: "Sol 已选择，会在下一回合生效" },
  };
  const child: ConversationSnapshot = {
    ...base,
    threadId: "019d-child-demo",
    turnId: "turn-child",
    parentThreadId: root.threadId,
    kind: "subagent",
    title: "子任务 · 核对 token",
    activeRequest: { model: "gpt-5.6-terra", effort: "high", source: "turn_context" },
    usage: {
      last: { inputTokens: 8_240, cachedInputTokens: 7_680, outputTokens: 364, totalTokens: 8_604 },
      cumulative: { inputTokens: 8_240, cachedInputTokens: 7_680, outputTokens: 364, totalTokens: 8_604 },
      cacheInputShare: 0.932,
      contextWindow: 258_400,
    },
    timing: { elapsedMs: 12_400, ttftEvidence: { kind: "pending" }, endToEndOutputRate: 29.4, observedOutputRate: 29.4 },
    status: { level: "green", code: "request_consistent", explanation: "子任务请求值一致" },
  };
  if (MOCK_QUERY === "scroll") {
    const scrollRoot: ConversationSnapshot = {
      ...root,
      threadId: "scroll-root",
      turnId: "turn-scroll-root",
      title: "滚动锚点回归",
      pendingNextTurn: undefined,
      status: { level: "green", code: "request_consistent", explanation: "根任务请求值一致" },
    };
    const scrollChildren = Array.from({ length: 12 }, (_, index): ConversationSnapshot => ({
      ...child,
      threadId: `scroll-child-${index + 1}`,
      turnId: `turn-scroll-child-${index + 1}`,
      parentThreadId: scrollRoot.threadId,
      title: `子智能体 ${index + 1} · token 核对`,
    }));
    return {
      schemaVersion: 5,
      checkedAt: new Date().toISOString(),
      codexRunning: true,
      collectorHealth: { level: "green", parseWarnings: 0 },
      conversations: [scrollRoot, ...scrollChildren],
    };
  }
  if (MOCK_QUERY === "hierarchy") {
    const hierarchyIssue = (threadId: string, title: string, parentThreadId?: string): ConversationSnapshot => ({
      ...base,
      threadId,
      turnId: `turn-${threadId}`,
      parentThreadId,
      kind: "subagent",
      title,
      status: { level: "yellow", code: "hierarchy_unresolved", explanation: "父会话缺失、失效或会话层级形成环" },
    });
    return {
      schemaVersion: 5,
      checkedAt: new Date().toISOString(),
      codexRunning: true,
      collectorHealth: { level: "green", parseWarnings: 0 },
      conversations: [
        root,
        child,
        hierarchyIssue("orphan-parentless", "父 ID 缺失的子智能体"),
        hierarchyIssue("orphan-missing", "找不到父会话的子任务", "missing-parent"),
        hierarchyIssue("orphan-self", "自环子任务", "orphan-self"),
        hierarchyIssue("cycle-a", "环路子任务 A", "cycle-b"),
        hierarchyIssue("cycle-b", "环路子任务 B", "cycle-a"),
      ],
    };
  }
  if (MOCK_QUERY === "pending") {
    return {
      schemaVersion: 5,
      checkedAt: new Date().toISOString(),
      codexRunning: true,
      collectorHealth: { level: "green", parseWarnings: 0 },
      conversations: [root, child],
    };
  }
  return {
    schemaVersion: 5,
    checkedAt: new Date().toISOString(),
    codexRunning: true,
    collectorHealth: { level: "green", parseWarnings: 0 },
    conversations: [base, root, child],
  };
}

function asRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function pick(record: Record<string, unknown>, ...keys: string[]): unknown {
  for (const key of keys) if (record[key] !== undefined && record[key] !== null) return record[key];
  return undefined;
}

function textValue(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function numberValue(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }
  return undefined;
}

function booleanValue(value: unknown, fallback: boolean): boolean {
  if (typeof value === "boolean") return value;
  if (value === "true" || value === 1) return true;
  if (value === "false" || value === 0) return false;
  return fallback;
}

function normalizeLevel(value: unknown, fallback: StatusLevel = "yellow"): StatusLevel {
  const normalized = textValue(value)?.toLowerCase();
  if (!normalized) return fallback;
  if (["green", "healthy", "ok", "success", "normal"].includes(normalized)) return "green";
  if (["red", "error", "critical", "failed", "mismatch"].includes(normalized)) return "red";
  if (["gray", "grey", "idle", "offline", "stopped"].includes(normalized)) return "gray";
  if (["yellow", "warning", "pending", "unknown", "stale", "partial"].includes(normalized)) return "yellow";
  return fallback;
}

function normalizeTokenUsage(value: unknown): TokenUsage {
  const raw = asRecord(value);
  return {
    inputTokens: numberValue(pick(raw, "inputTokens", "input_tokens")),
    cachedInputTokens: numberValue(pick(raw, "cachedInputTokens", "cached_input_tokens")),
    cacheWriteInputTokens: numberValue(pick(raw, "cacheWriteInputTokens", "cache_write_input_tokens")),
    outputTokens: numberValue(pick(raw, "outputTokens", "output_tokens")),
    reasoningOutputTokens: numberValue(pick(raw, "reasoningOutputTokens", "reasoning_output_tokens")),
    totalTokens: numberValue(pick(raw, "totalTokens", "total_tokens")),
  };
}

function normalizeRequest(value: unknown): RequestEvidence {
  const raw = asRecord(value);
  return { model: textValue(raw.model), effort: textValue(raw.effort), source: textValue(raw.source) };
}

function normalizeHop(value: unknown): RouteHop {
  const raw = asRecord(value);
  return {
    fromModel: textValue(pick(raw, "fromModel", "from_model")),
    toModel: textValue(pick(raw, "toModel", "to_model")),
    reason: textValue(raw.reason),
    timestamp: textValue(raw.timestamp),
    association: textValue(raw.association),
  };
}

function normalizeAnomaly(value: unknown): string {
  if (typeof value === "string") return value;
  const raw = asRecord(value);
  return textValue(pick(raw, "explanation", "message", "code")) ?? "未分类的行为偏离";
}

function normalizeTtftEvidence(value: unknown, exactMs?: number): TtftEvidence {
  const raw = asRecord(value);
  const kind = textValue(raw.kind);
  if (kind === "estimatedWindow") {
    return {
      kind,
      lowerMs: numberValue(pick(raw, "lowerMs", "lower_ms")),
      upperMs: numberValue(pick(raw, "upperMs", "upper_ms")),
    };
  }
  if (kind === "exactTerminal" || exactMs !== undefined) return { kind: "exactTerminal" };
  return { kind: "pending" };
}

function normalizeQualityFactor(value: unknown): QualityFactor {
  const raw = asRecord(value);
  return {
    code: textValue(raw.code) ?? "unknown",
    direction: textValue(raw.direction),
    observed: numberValue(raw.observed),
    baselineMedian: numberValue(pick(raw, "baselineMedian", "baseline_median")),
    mad: numberValue(raw.mad),
    robustDeviation: numberValue(pick(raw, "robustDeviation", "robust_deviation")),
    unit: textValue(raw.unit),
  };
}

function normalizeQualityAssessment(value: unknown): QualityAssessment {
  const raw = asRecord(value);
  const stateValue = textValue(raw.state);
  const qualityState: QualityAssessment["state"] = stateValue === "suspectedDegradation"
    ? "suspectedDegradation"
    : stateValue === "consistent" ? "consistent" : "learning";
  const comparatorRaw = asRecord(raw.comparator);
  const limitationsRaw = raw.limitations;
  return {
    state: qualityState,
    baselineKey: textValue(pick(raw, "baselineKey", "baseline_key")),
    baselineSampleCount: numberValue(pick(raw, "baselineSampleCount", "baseline_sample_count")) ?? 0,
    consecutiveHits: numberValue(pick(raw, "consecutiveHits", "consecutive_hits")) ?? 0,
    factors: Array.isArray(raw.factors) ? raw.factors.map(normalizeQualityFactor) : [],
    comparator: Object.keys(comparatorRaw).length > 0 ? {
      requestedModel: textValue(pick(comparatorRaw, "requestedModel", "requested_model")),
      comparedModel: textValue(pick(comparatorRaw, "comparedModel", "compared_model")),
      sampleCount: numberValue(pick(comparatorRaw, "sampleCount", "sample_count")),
      relativeDistance: numberValue(pick(comparatorRaw, "relativeDistance", "relative_distance")),
    } : undefined,
    limitations: Array.isArray(limitationsRaw) ? limitationsRaw.map(normalizeAnomaly) : [],
  };
}

function enumValue<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  const normalized = textValue(value);
  return normalized && (allowed as readonly string[]).includes(normalized) ? normalized as T : fallback;
}

function normalizeConnectionOrigin(value: unknown): ConnectionOriginSnapshot {
  const raw = asRecord(value);
  const evidence = Array.isArray(raw.evidence) ? raw.evidence.map(normalizeAnomaly) : [];
  const limitations = Array.isArray(raw.limitations) ? raw.limitations.map(normalizeAnomaly) : [];
  return {
    kind: enumValue(pick(raw, "kind"), [
      "officialChatGpt", "officialOpenAiApi", "officialAnthropicApi", "managedProvider",
      "customEndpoint", "localEndpoint", "unknown",
    ] as const, "unknown"),
    authMode: enumValue(pick(raw, "authMode", "auth_mode"), ["chatGpt", "apiKey", "external", "unknown"] as const, "unknown"),
    confidence: enumValue(raw.confidence, ["configured", "partial", "unknown"] as const, "unknown"),
    providerId: textValue(pick(raw, "providerId", "provider_id")),
    endpointClass: enumValue(pick(raw, "endpointClass", "endpoint_class"), [
      "officialChatGpt", "officialOpenAi", "officialAnthropic", "managedProvider",
      "customEndpoint", "localEndpoint", "unknown",
    ] as const, "unknown"),
    evidence,
    limitations,
  };
}

function normalizeConversation(value: unknown, index: number): ConversationSnapshot {
  const raw = asRecord(value);
  const route = asRecord(pick(raw, "serverRoute", "server_route"));
  const usage = asRecord(raw.usage);
  const timing = asRecord(raw.timing);
  const exactTtft = numberValue(pick(timing, "ttftMs", "ttft_ms", "timeToFirstTokenMs", "time_to_first_token_ms"));
  const status = asRecord(raw.status);
  const threadId = textValue(pick(raw, "threadId", "thread_id")) ?? `unknown-${index + 1}`;
  const activeRequest = normalizeRequest(pick(raw, "activeRequest", "active_request"));
  const pendingRaw = pick(raw, "pendingNextTurn", "pending_next_turn");
  const anomaliesRaw = raw.anomalies;
  const conversation: ConversationSnapshot = {
    threadId,
    turnId: textValue(pick(raw, "turnId", "turn_id")),
    parentThreadId: textValue(pick(raw, "parentThreadId", "parent_thread_id")),
    kind: textValue(raw.kind) ?? "unknown",
    title: textValue(raw.title) ?? `任务 ${shortId(threadId)}`,
    sourceTimestamp: textValue(pick(raw, "sourceTimestamp", "source_timestamp")),
    activeRequest,
    pendingNextTurn: pendingRaw ? normalizeRequest(pendingRaw) : undefined,
    serverRoute: {
      model: textValue(route.model),
      evidence: textValue(route.evidence) ?? "unknown",
      observedAt: textValue(pick(route, "observedAt", "observed_at")),
      chain: Array.isArray(route.chain) ? route.chain.map(normalizeHop) : [],
    },
    usage: {
      last: normalizeTokenUsage(usage.last),
      cumulative: normalizeTokenUsage(usage.cumulative),
      lastCacheInputShare: numberValue(pick(usage, "lastCacheInputShare", "last_cache_input_share")),
      cacheInputShare: numberValue(pick(usage, "cacheInputShare", "cache_input_share")),
      contextWindow: numberValue(pick(usage, "contextWindow", "context_window")),
      contextInputShare: numberValue(pick(usage, "contextInputShare", "context_input_share")),
    },
    timing: {
      elapsedMs: numberValue(pick(timing, "elapsedMs", "elapsed_ms")),
      ttftMs: exactTtft,
      durationMs: numberValue(pick(timing, "durationMs", "duration_ms")),
      ttftEvidence: normalizeTtftEvidence(pick(timing, "ttftEvidence", "ttft_evidence"), exactTtft),
      modelActiveMs: numberValue(pick(timing, "modelActiveMs", "model_active_ms")),
      endToEndOutputRate: numberValue(pick(timing, "endToEndOutputRate", "end_to_end_output_rate", "observedOutputRate", "observed_output_rate")),
      modelPhaseOutputRate: numberValue(pick(timing, "modelPhaseOutputRate", "model_phase_output_rate")),
      observedOutputRate: numberValue(pick(timing, "observedOutputRate", "observed_output_rate", "endToEndOutputRate", "end_to_end_output_rate")),
    },
    qualityAssessment: normalizeQualityAssessment(pick(raw, "qualityAssessment", "quality_assessment")),
    connectionOrigin: normalizeConnectionOrigin(pick(raw, "connectionOrigin", "connection_origin")),
    status: {
      level: normalizeLevel(status.level, "yellow"),
      code: textValue(status.code) ?? "unknown",
      explanation: textValue(status.explanation) ?? "监视器没有提供完整的状态说明",
    },
    anomalies: Array.isArray(anomaliesRaw) ? anomaliesRaw.map(normalizeAnomaly) : [],
  };
  conversation.status.level = deriveConversationLevel(conversation);
  return conversation;
}

function normalizeSnapshot(value: unknown): MonitorSnapshotV5 {
  const envelope = asRecord(value);
  const raw = envelope.snapshot ? asRecord(envelope.snapshot) : envelope;
  const collector = asRecord(pick(raw, "collectorHealth", "collector_health"));
  const conversations = Array.isArray(raw.conversations) ? raw.conversations.map(normalizeConversation) : [];
  return {
    schemaVersion: numberValue(pick(raw, "schemaVersion", "schema_version")) ?? 5,
    checkedAt: textValue(pick(raw, "checkedAt", "checked_at")) ?? new Date().toISOString(),
    codexRunning: booleanValue(pick(raw, "codexRunning", "codex_running"), conversations.length > 0),
    collectorHealth: {
      level: normalizeLevel(collector.level, conversations.length > 0 ? "yellow" : "gray"),
      parseWarnings: numberValue(pick(collector, "parseWarnings", "parse_warnings")) ?? 0,
      lastError: textValue(pick(collector, "lastError", "last_error")),
    },
    conversations,
  };
}

function deriveConversationLevel(conversation: ConversationSnapshot): StatusLevel {
  let level = conversation.status.level;
  const active = conversation.activeRequest;
  const pending = conversation.pendingNextTurn;
  if (!active.model || !active.effort) level = maxLevel(level, "yellow");
  if (pending && differsFromActive(active, pending)) level = maxLevel(level, "yellow");
  if (conversation.qualityAssessment.state === "suspectedDegradation") level = maxLevel(level, "yellow");
  // A reroute event is evidence, not automatically a policy violation. The
  // collector owns the red status decision because it knows the task policy.
  return level;
}

function normalizeIdentifier(value: string | undefined): string {
  return value?.trim().toLowerCase() ?? "";
}

function differsFromActive(active: RequestEvidence, pending: RequestEvidence): boolean {
  const modelChanged = Boolean(pending.model) && normalizeIdentifier(pending.model) !== normalizeIdentifier(active.model);
  const effortChanged = Boolean(pending.effort) && normalizeIdentifier(pending.effort) !== normalizeIdentifier(active.effort);
  return modelChanged || effortChanged;
}

function isExplicitRouteEvidence(evidence: string, chain: RouteHop[]): boolean {
  const normalized = evidence.toLowerCase();
  return normalized.includes("reroute") || normalized === "model/rerouted" || chain.length > 0;
}

const ROUTE_CONFLICT_CODES = new Set([
  "route_conflict",
  "server_route_conflict",
  "route_policy_conflict",
  "reroute_policy_conflict",
  "server_reroute_conflict",
]);

function isRoutePolicyConflict(conversation: ConversationSnapshot): boolean {
  if (!isExplicitRouteEvidence(conversation.serverRoute.evidence, conversation.serverRoute.chain)) return false;
  if (conversation.status.level !== "red") return false;
  const code = conversation.status.code.trim().toLowerCase().replace(/[\s./-]+/g, "_");
  return ROUTE_CONFLICT_CODES.has(code);
}

function maxLevel(left: StatusLevel, right: StatusLevel): StatusLevel {
  return STATUS_ORDER[left] >= STATUS_ORDER[right] ? left : right;
}

function readStoredTheme(): ThemeName {
  if (MOCK_QUERY && URL_OPTIONS.get("theme") === "minimal") return "minimal";
  try {
    const current = localStorage.getItem("xiaoli-theme");
    const legacy = localStorage.getItem("mochi-meter-theme");
    return (current ?? legacy) === "minimal" ? "minimal" : "cute";
  }
  catch { return "cute"; }
}

function storeTheme(theme: ThemeName): void {
  try { localStorage.setItem("xiaoli-theme", theme); }
  catch { /* The Rust side also receives the preference. */ }
}

function shortId(value: string | undefined): string {
  if (!value) return "—";
  return value.length <= 10 ? value : value.slice(0, 8);
}

function escapeHtml(value: unknown): string {
  return String(value ?? "")
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;").replace(/'/g, "&#039;");
}

function friendlyModel(model: string | undefined): string {
  if (!model) return "未知模型";
  const known: Record<string, string> = {
    "gpt-5.6-sol": "Sol", "gpt-5.6-terra": "Terra", "gpt-5.6-luna": "Luna",
  };
  return known[normalizeIdentifier(model)] ?? model;
}

function formatTokens(value: number | undefined): string {
  if (value === undefined || !Number.isFinite(value)) return "—";
  const absolute = Math.abs(value);
  if (absolute >= 1_000_000) return `${trimDecimal(value / 1_000_000)}m`;
  if (absolute >= 1_000) return `${trimDecimal(value / 1_000)}k`;
  return Math.round(value).toLocaleString("zh-CN");
}

function trimDecimal(value: number): string {
  return value.toFixed(value >= 100 ? 0 : value >= 10 ? 1 : 2).replace(/\.0+$|(?<=\.[0-9])0$/, "");
}

function formatPercent(value: number | undefined): string {
  if (value === undefined || !Number.isFinite(value)) return "—";
  const percentage = Math.abs(value) <= 1 ? value * 100 : value;
  return `${Math.max(0, Math.min(100, percentage)).toFixed(percentage >= 99.95 ? 0 : 1)}%`;
}

function formatDuration(value: number | undefined): string {
  if (value === undefined || !Number.isFinite(value)) return "—";
  if (value < 1_000) return `${Math.round(value)}ms`;
  const seconds = value / 1_000;
  if (seconds < 60) return `${trimDecimal(seconds)}s`;
  return `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s`;
}

function formatRate(value: number | undefined): string {
  return value === undefined || !Number.isFinite(value) ? "—" : `${trimDecimal(value)} tok/s`;
}

function ttftDisplay(timing: ConversationSnapshot["timing"]): { value: string; tooltip: string } {
  if (timing.ttftEvidence.kind === "exactTerminal" && timing.ttftMs !== undefined) {
    return { value: formatDuration(timing.ttftMs), tooltip: "任务终态结构化事件报告的精确 TTFT" };
  }
  if (timing.ttftEvidence.kind === "estimatedWindow") {
    const lower = timing.ttftEvidence.lowerMs;
    const upper = timing.ttftEvidence.upperMs;
    if (lower !== undefined && upper !== undefined) {
      return {
        value: `约 ${formatDuration(lower)}–${formatDuration(upper)}`,
        tooltip: "首个模型片段从开始到完成形成的可信区间；任一端都不是精确首 token 时间",
      };
    }
  }
  return { value: "等待首段", tooltip: "首个模型片段尚未完成，当前无法形成 TTFT 估算区间" };
}

function qualityShortLabel(quality: QualityAssessment): string {
  if (quality.state === "suspectedDegradation") return `疑似降质 · ${quality.factors.length} 项信号`;
  if (quality.state === "consistent") return `行为一致 · ${quality.baselineSampleCount} 个同桶样本`;
  return `学习中 ${Math.min(quality.baselineSampleCount, 30)}/30`;
}

function qualityFactorLabel(code: string): string {
  const labels: Record<string, string> = {
    ttft_high: "TTFT 偏高",
    ttftHigh: "TTFT 偏高",
    model_rate_low: "模型阶段速率偏低",
    modelPhaseOutputRateLow: "模型阶段速率偏低",
    reasoning_share_low: "推理输出占比偏低",
    reasoningOutputShareLow: "推理输出占比偏低",
    reasoning_phase_share_low: "推理阶段时长占比偏低",
    reasoningPhaseShareLow: "推理阶段时长占比偏低",
  };
  return labels[code] ?? code;
}

function formatQualityNumber(value: number | undefined, unit?: string): string {
  if (value === undefined || !Number.isFinite(value)) return "未知";
  if (unit === "ms") return formatDuration(value);
  if (unit === "ratio" || unit === "percent") return formatPercent(value);
  if (unit === "tok/s") return formatRate(value);
  return `${trimDecimal(value)}${unit ? ` ${unit}` : ""}`;
}

function parseDate(value: string | undefined): number | undefined {
  if (!value) return undefined;
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? undefined : parsed;
}

function relativeAge(value: string | undefined): string {
  const timestamp = parseDate(value);
  if (timestamp === undefined) return "时间未知";
  const seconds = Math.max(0, Math.round((Date.now() - timestamp) / 1_000));
  if (seconds < 5) return "刚刚";
  if (seconds < 60) return `${seconds}秒前`;
  if (seconds < 3_600) return `${Math.floor(seconds / 60)}分钟前`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3_600)}小时前`;
  return `${Math.floor(seconds / 86_400)}天前`;
}

function exactTimestamp(value: string | undefined): string {
  const timestamp = parseDate(value);
  if (timestamp === undefined) return "未知";
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
  }).format(timestamp);
}

function usageTotal(usage: TokenUsage): number | undefined {
  if (usage.totalTokens !== undefined) return usage.totalTokens;
  if (usage.inputTokens === undefined && usage.outputTokens === undefined) return undefined;
  return (usage.inputTokens ?? 0) + (usage.outputTokens ?? 0);
}

function cacheShare(conversation: ConversationSnapshot, scope: "last" | "cumulative" = "cumulative"): number | undefined {
  const usage = conversation.usage[scope];
  if (usage.inputTokens && usage.cachedInputTokens !== undefined) return usage.cachedInputTokens / Math.max(usage.inputTokens, 1);
  return scope === "last" ? conversation.usage.lastCacheInputShare : conversation.usage.cacheInputShare;
}

function aggregateUsage(conversations: ConversationSnapshot[]): { total?: number; cacheShare?: number } {
  let total = 0, input = 0, cached = 0;
  let hasTotal = false, hasCache = false;
  for (const conversation of uniqueConversations(conversations)) {
    const usage = conversation.usage.cumulative;
    const conversationTotal = usageTotal(usage);
    if (conversationTotal !== undefined) { total += conversationTotal; hasTotal = true; }
    if (usage.inputTokens !== undefined && usage.cachedInputTokens !== undefined) {
      input += usage.inputTokens; cached += usage.cachedInputTokens; hasCache = true;
    }
  }
  return { total: hasTotal ? total : undefined, cacheShare: hasCache && input > 0 ? cached / input : undefined };
}

function uniqueConversations(conversations: ConversationSnapshot[]): ConversationSnapshot[] {
  const seen = new Set<string>();
  return conversations.filter((conversation) => {
    if (seen.has(conversation.threadId)) return false;
    seen.add(conversation.threadId);
    return true;
  });
}

function conversationFamily(root: ConversationSnapshot, conversations: ConversationSnapshot[]): ConversationSnapshot[] {
  const familyIds = new Set<string>([root.threadId]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const conversation of conversations) {
      if (!isRootConversation(conversation) && !familyIds.has(conversation.threadId) && conversation.parentThreadId && familyIds.has(conversation.parentThreadId)) {
        familyIds.add(conversation.threadId);
        changed = true;
      }
    }
  }
  return uniqueConversations(conversations.filter((conversation) => familyIds.has(conversation.threadId)));
}

function isRootConversation(conversation: ConversationSnapshot): boolean {
  const kind = normalizeIdentifier(conversation.kind);
  if (kind === "root") return true;
  if (kind === "subagent") return false;
  // Older snapshots sometimes omitted kind. Only a parentless unknown kind is
  // allowed to fall back to a root window; a parentless subagent never is.
  return !conversation.parentThreadId;
}

function rootConversations(conversations: ConversationSnapshot[]): ConversationSnapshot[] {
  return uniqueConversations(conversations).filter(isRootConversation);
}

function abnormalConversations(conversations: ConversationSnapshot[]): ConversationSnapshot[] {
  const unique = uniqueConversations(conversations);
  const roots = unique.filter(isRootConversation);
  const childrenByParent = new Map<string, ConversationSnapshot[]>();
  for (const conversation of unique) {
    if (!conversation.parentThreadId) continue;
    const siblings = childrenByParent.get(conversation.parentThreadId) ?? [];
    siblings.push(conversation);
    childrenByParent.set(conversation.parentThreadId, siblings);
  }
  const reachable = new Set<string>();
  const queue = [...roots];
  while (queue.length > 0) {
    const current = queue.shift();
    if (!current || reachable.has(current.threadId)) continue;
    reachable.add(current.threadId);
    for (const child of childrenByParent.get(current.threadId) ?? []) {
      if (!reachable.has(child.threadId)) queue.push(child);
    }
  }
  // Missing parents, parentless subagents, self loops, and multi-node cycles
  // are all unreachable from a real root and must remain visible as abnormal.
  return unique.filter((conversation) => !reachable.has(conversation.threadId));
}

function childrenFor(parent: ConversationSnapshot, conversations: ConversationSnapshot[]): ConversationSnapshot[] {
  return conversations.filter((conversation) => !isRootConversation(conversation) && conversation.parentThreadId === parent.threadId);
}

function familyLevel(root: ConversationSnapshot, conversations: ConversationSnapshot[]): StatusLevel {
  return conversationFamily(root, conversations)
    .reduce((level, conversation) => maxLevel(level, conversation.status.level), root.status.level);
}

function summaryLevel(snapshot: MonitorSnapshotV5, roots: ConversationSnapshot[]): StatusLevel {
  let level = snapshot.collectorHealth.level;
  if (snapshot.collectorHealth.parseWarnings > 0) level = maxLevel(level, "yellow");
  if (snapshot.codexRunning) {
    for (const conversation of snapshot.conversations) level = maxLevel(level, conversation.status.level);
    if (roots.length === 0 && snapshot.conversations.length > 0) level = maxLevel(level, "yellow");
  }
  if ((!snapshot.codexRunning || snapshot.conversations.length === 0) && level === "green") return "gray";
  return level;
}

function routeState(conversation: ConversationSnapshot): { explicit: boolean; targetMissing: boolean; targetModel?: string; label: string; tooltip: string } {
  const explicit = isExplicitRouteEvidence(conversation.serverRoute.evidence, conversation.serverRoute.chain);
  const activeModel = friendlyModel(conversation.activeRequest.model);
  const lastHop = conversation.serverRoute.chain[conversation.serverRoute.chain.length - 1];
  const targetModel = conversation.serverRoute.model ?? lastHop?.toModel;
  if (explicit && targetModel) {
    const reason = lastHop?.reason ? `\n原因：${lastHop.reason}` : "";
    return {
      explicit: true,
      targetMissing: false,
      targetModel,
      label: `${activeModel} → ${friendlyModel(targetModel)}`,
      tooltip: `服务器已重路由\n证据：${conversation.serverRoute.evidence}${reason}`,
    };
  }
  if (explicit) {
    const chain = conversation.serverRoute.chain
      .map((hop) => [hop.fromModel, hop.toModel].filter(Boolean).map(friendlyModel).join(" → "))
      .filter(Boolean)
      .join(" → ");
    return {
      explicit: true,
      targetMissing: true,
      label: chain || `${activeModel} → 目标未知`,
      tooltip: `已捕获明确的 model/rerouted 证据，但目标模型字段缺失。\n证据：${conversation.serverRoute.evidence}`,
    };
  }
  return {
    explicit: false,
    targetMissing: false,
    label: "未见服务器重路由",
    tooltip: "小狸没有捕获到明确的 model/rerouted 事件。这里只能确认本回合请求模型；这不证明服务器物理模型没有变化。",
  };
}

const ORIGIN_KIND_COPY: Record<ConnectionOriginKind, { label: string; short: string }> = {
  officialChatGpt: { label: "官方 ChatGPT 登录", short: "ChatGPT" },
  officialOpenAiApi: { label: "官方 OpenAI API", short: "OpenAI" },
  officialAnthropicApi: { label: "官方 Anthropic API", short: "Anthropic" },
  managedProvider: { label: "托管提供商", short: "托管" },
  customEndpoint: { label: "自定义端点", short: "自定义" },
  localEndpoint: { label: "本地端点", short: "本地" },
  unknown: { label: "连接来源未知", short: "未知" },
};

const ORIGIN_CONFIDENCE_COPY: Record<ConnectionOriginConfidence, { label: string; short: string }> = {
  configured: { label: "配置证据完整", short: "配置" },
  partial: { label: "仅有部分证据", short: "部分" },
  unknown: { label: "证据不足", short: "不足" },
};

const AUTH_MODE_COPY: Record<ConnectionAuthMode, string> = {
  chatGpt: "ChatGPT 登录",
  apiKey: "API Key",
  external: "外部认证",
  unknown: "未知",
};

function connectionOriginDisplay(origin: ConnectionOriginSnapshot): {
  label: string;
  compact: string;
  tooltip: string;
  className: string;
} {
  const kind = ORIGIN_KIND_COPY[origin.kind];
  const confidence = ORIGIN_CONFIDENCE_COPY[origin.confidence];
  const evidence = origin.evidence.length > 0 ? origin.evidence.join("、") : "无";
  const limitations = origin.limitations.length > 0 ? origin.limitations.join("、") : "无";
  return {
    label: `${kind.label} · ${confidence.label}`,
    compact: `${kind.short} · ${confidence.short}`,
    className: origin.kind === "unknown" ? "is-unknown" : origin.kind === "customEndpoint" || origin.kind === "localEndpoint" ? "is-custom" : "is-known",
    tooltip: [
      `连接来源：${kind.label}`,
      `置信度：${confidence.label}`,
      `认证方式：${AUTH_MODE_COPY[origin.authMode]}`,
      `Provider：${origin.providerId ?? "未知"}`,
      `证据：${evidence}`,
      `限制：${limitations}`,
      "这是连接配置来源证据，不是服务器物理模型身份证明。",
    ].join("\n"),
  };
}

function compactOriginSummary(conversations: ConversationSnapshot[]): { label: string; tooltip: string; className: string } {
  const unique = new Map<string, ConnectionOriginSnapshot>();
  for (const conversation of conversations) {
    const origin = conversation.connectionOrigin;
    unique.set(`${origin.kind}:${origin.confidence}`, origin);
  }
  if (unique.size === 1) {
    const display = connectionOriginDisplay([...unique.values()][0]);
    return { label: display.compact, tooltip: display.tooltip, className: display.className };
  }
  if (unique.size === 0) {
    const display = connectionOriginDisplay(normalizeConnectionOrigin(undefined));
    return { label: display.compact, tooltip: display.tooltip, className: display.className };
  }
  const descriptions = [...unique.values()].map((origin) => connectionOriginDisplay(origin));
  return {
    label: `${unique.size} 种来源 · 混合证据`,
    tooltip: `${descriptions.map((item) => item.label).join("\n")}\n连接来源不等于服务器物理模型身份。`,
    className: "is-mixed",
  };
}

function pendingLabel(conversation: ConversationSnapshot): string | undefined {
  const pending = conversation.pendingNextTurn;
  if (!pending || !differsFromActive(conversation.activeRequest, pending)) return undefined;
  const pieces: string[] = [];
  if (pending.model && normalizeIdentifier(pending.model) !== normalizeIdentifier(conversation.activeRequest.model)) pieces.push(friendlyModel(pending.model));
  if (pending.effort && normalizeIdentifier(pending.effort) !== normalizeIdentifier(conversation.activeRequest.effort)) pieces.push(pending.effort);
  return pieces.join(" · ") || "新配置";
}

function pendingRequestLabel(conversation: ConversationSnapshot): string | undefined {
  const pending = conversation.pendingNextTurn;
  if (!pending || !differsFromActive(conversation.activeRequest, pending)) return undefined;
  return `${friendlyModel(pending.model ?? conversation.activeRequest.model)} · ${pending.effort ?? conversation.activeRequest.effort ?? "effort 未知"}`;
}

function conciseExplanation(value: string | undefined, maxLength = 46): string {
  const normalized = value?.replace(/\s+/g, " ").trim() || "需要检查任务状态";
  return normalized.length <= maxLength ? normalized : `${normalized.slice(0, maxLength - 1)}…`;
}

function collectorIssueText(snapshot: MonitorSnapshotV5): string | undefined {
  if (snapshot.collectorHealth.lastError) return conciseExplanation(snapshot.collectorHealth.lastError);
  if (snapshot.collectorHealth.level === "red") return "采集器发生确定性故障";
  if (snapshot.collectorHealth.parseWarnings > 0) return `${snapshot.collectorHealth.parseWarnings} 个解析警告`;
  if (snapshot.collectorHealth.level === "yellow") return "采集数据不完整，等待确认";
  return undefined;
}

function snapshotExplanation(snapshot: MonitorSnapshotV5): string {
  const collectorIssue = collectorIssueText(snapshot);
  let collectorLevel = snapshot.collectorHealth.level;
  if (snapshot.collectorHealth.lastError) collectorLevel = "red";
  else if (snapshot.collectorHealth.parseWarnings > 0) collectorLevel = maxLevel(collectorLevel, "yellow");
  const worst = snapshot.conversations.reduce<ConversationSnapshot | undefined>((current, item) => {
    if (!current || STATUS_ORDER[item.status.level] > STATUS_ORDER[current.status.level]) return item;
    return current;
  }, undefined);
  if (collectorIssue && (!worst || STATUS_ORDER[collectorLevel] >= STATUS_ORDER[worst.status.level])) return collectorIssue;
  return worst ? `${worst.title}：${conciseExplanation(worst.status.explanation)}` : "当前状态需要确认";
}

function buildConversationTooltip(conversation: ConversationSnapshot): string {
  const route = routeState(conversation);
  const origin = connectionOriginDisplay(conversation.connectionOrigin);
  const usage = conversation.usage.cumulative;
  const hops = conversation.serverRoute.chain
    .map((hop) => `${friendlyModel(hop.fromModel)} → ${friendlyModel(hop.toModel)}${hop.reason ? `：${hop.reason}` : ""}`)
    .join("\n");
  return [
    `Thread：${conversation.threadId}`,
    `Turn：${conversation.turnId ?? "未知"}`,
    `Parent：${conversation.parentThreadId ?? "无"}`,
    `来源时间：${exactTimestamp(conversation.sourceTimestamp)}`,
    `请求模型：${conversation.activeRequest.model ?? "未知"}`,
    `请求 effort：${conversation.activeRequest.effort ?? "未知"}`,
    `路由证据：${route.label}`,
    `连接来源：${origin.label}`,
    "连接来源不是服务器物理模型身份证明。",
    conversation.status.level === "green" || conversation.status.level === "gray"
      ? ""
      : `状态说明：${conversation.status.explanation}`,
    hops ? `重路由链：\n${hops}` : "",
    `累计 token：${usageTotal(usage)?.toLocaleString("zh-CN") ?? "未知"}`,
    `推理输出 token：${usage.reasoningOutputTokens?.toLocaleString("zh-CN") ?? "未知"}`,
    `行为判断：${qualityShortLabel(conversation.qualityAssessment)}`,
  ].filter(Boolean).join("\n");
}

type IconName = "pin" | "expand" | "collapse" | "hide" | "close" | "refresh" | "theme" | "shield" | "route" | "more" | "reset" | "help";

function icon(name: IconName): string {
  const paths: Record<IconName, string> = {
    pin: '<path d="M7.5 2.5h5l-.8 3.1 2.1 2.1v1.2H10.7V15l-.7 1.5L9.3 15V8.9H6.2V7.7l2.1-2.1-.8-3.1Z"/>',
    expand: '<path d="m5 7 5 5 5-5"/>', collapse: '<path d="m5 12 5-5 5 5"/>',
    hide: '<path d="M4.5 10h11"/>', close: '<path d="m6 6 8 8m0-8-8 8"/>',
    refresh: '<path d="M14.8 6.4A5.6 5.6 0 1 0 15.3 12M14.8 6.4V2.9m0 3.5h-3.5"/>',
    more: '<circle cx="5" cy="10" r="1"/><circle cx="10" cy="10" r="1"/><circle cx="15" cy="10" r="1"/>',
    reset: '<path d="M4 8.4A6.3 6.3 0 1 1 5.4 14M4 8.4V4.3m0 4.1h4.1"/><path d="M10 6.5v4l2.7 1.6"/>',
    theme: '<path d="M10 3.2a6.8 6.8 0 1 0 0 13.6c1.1 0 1.8-.6 1.8-1.4 0-.4-.2-.7-.4-1-.3-.4-.5-.7-.5-1.2 0-.8.7-1.4 1.5-1.4h1.4c1.8 0 3.2-1.4 3.2-3.2C17 5.6 13.9 3.2 10 3.2Z"/><circle cx="6.8" cy="8.2" r=".8"/><circle cx="9.2" cy="5.9" r=".8"/><circle cx="12.4" cy="6.2" r=".8"/>',
    shield: '<path d="M10 2.7 16 5v4.4c0 3.6-2.4 6.4-6 7.9-3.6-1.5-6-4.3-6-7.9V5l6-2.3Z"/><path d="M10 7.1v3.4m0 2.5h.01"/>',
    route: '<path d="M4 5h7.5a3.5 3.5 0 0 1 0 7H7m1.8-9L12 5 8.8 7M8 10l-3.2 2L8 14"/>',
    help: '<circle cx="10" cy="10" r="7"/><path d="M7.8 7.6A2.4 2.4 0 0 1 10.2 5c1.6 0 2.7.9 2.7 2.3 0 1.2-.7 1.8-1.7 2.4-.8.5-1.2 1-1.2 2M10 14.7h.01"/>',
  };
  return `<svg viewBox="0 0 20 20" aria-hidden="true" focusable="false">${paths[name]}</svg>`;
}

function renderMochi(level: StatusLevel): string {
  const copy = STATUS_COPY[level];
  return `<div class="status-stack status-${level}" data-status-avatar aria-label="状态：${copy.short}" title="${copy.long}" data-tauri-drag-region="deep">
    <span class="mascot-avatar" aria-hidden="true"><span class="mascot-fallback"><i class="eye eye-left"></i><i class="eye eye-right"></i><i class="mouth"></i></span></span>
    <span class="status-word">${copy.short}</span>
  </div>`;
}

function renderCompactSummary(snapshot: MonitorSnapshotV5, roots: ConversationSnapshot[]): string {
  const overallLevel = summaryLevel(snapshot, roots);
  const collectorIssue = collectorIssueText(snapshot);
  if (!snapshot.codexRunning) {
    const secondary = collectorIssue ?? "Codex 当前未运行";
    return `<div class="compact-primary" title="${escapeHtml(secondary)}">${collectorIssue ? "监视器采集异常" : "等待 Codex"}</div><div class="compact-secondary compact-alert status-${overallLevel}">${escapeHtml(secondary)}</div>`;
  }
  if (roots.length === 0) {
    const abnormal = abnormalConversations(snapshot.conversations);
    if (abnormal.length > 0) {
      const usage = aggregateUsage(snapshot.conversations);
      return `<div class="compact-primary" title="父会话缺失、失效或会话层级形成环">${abnormal.length} 个层级异常任务 · ${overallLevel === "red" ? "异常" : "待确认"}</div><div class="compact-secondary compact-alert status-${overallLevel}"><span>父会话未找到 / 层级异常</span><span class="compact-wide-metrics"><span class="metric-separator">·</span>${formatTokens(usage.total)} tok</span></div>`;
    }
    const warning = collectorIssue ?? "当前没有活动回合";
    return `<div class="compact-primary">等待活动回合</div><div class="compact-secondary">${escapeHtml(warning)}</div>`;
  }
  if (roots.length === 1) {
    const conversation = roots[0];
    const family = conversationFamily(conversation, snapshot.conversations);
    const familyUsage = aggregateUsage(family);
    const pending = pendingRequestLabel(conversation);
    const route = routeState(conversation);
    const model = friendlyModel(conversation.activeRequest.model);
    const effort = conversation.activeRequest.effort ?? "effort 未知";
    const routeConflict = isRoutePolicyConflict(conversation);
    const origin = compactOriginSummary(family);
    const explanation = overallLevel === "yellow" || overallLevel === "red" ? snapshotExplanation(snapshot) : undefined;
    const tooltip = [buildConversationTooltip(conversation), explanation ? `状态说明：${explanation}` : ""].filter(Boolean).join("\n");
    const requestEvidence = pending
      ? `<span class="phase-label">本回合</span><span class="model-flow">${escapeHtml(model)}</span><span class="dot-separator">·</span><span class="effort">${escapeHtml(effort)}（请求）</span>`
      : route.explicit && route.targetModel
        ? `<span class="model-flow">${escapeHtml(model)} <span class="route-arrow ${routeConflict ? "is-conflict" : ""}">→ ${escapeHtml(friendlyModel(route.targetModel))}</span></span><span class="dot-separator">·</span><span class="effort">${escapeHtml(effort)}（请求）</span>`
        : `<span class="model-flow">${escapeHtml(model)}</span><span class="dot-separator">·</span><span class="effort">${escapeHtml(effort)}（请求）</span>`;
    const metrics = `<span>${formatTokens(familyUsage.total)} tok</span><span class="metric-separator">·</span><span>缓存输入 ${formatPercent(familyUsage.cacheShare)}</span>`;
    const secondary = pending
      ? `<span class="pending-next-line"><span class="phase-label">下一回合</span> ${escapeHtml(pending)}（待生效）</span>${explanation ? `<span class="compact-wide-alert status-${overallLevel}"><span class="metric-separator">·</span>${escapeHtml(explanation)}</span>` : ""}`
      : explanation
        ? `<span class="compact-alert status-${overallLevel}" title="${escapeHtml(explanation)}">${escapeHtml(explanation)}</span><span class="compact-wide-metrics"><span class="metric-separator">·</span>${metrics}</span>`
        : `${metrics}<span class="route-wide-label ${route.targetMissing ? "status-yellow" : ""}"${route.targetMissing ? ' style="color:var(--yellow)"' : ""}><span class="metric-separator">·</span>${route.targetMissing ? "已重路由，目标未知" : route.explicit ? "服务器已重路由" : "未见服务器重路由"}</span>`;
    return `<div class="compact-primary" title="${escapeHtml(tooltip)}">${requestEvidence}<span class="compact-wide-title"><span class="dot-separator">·</span>${escapeHtml(conversation.title)}</span>
        <span class="route-mark ${route.explicit ? "is-explicit" : "is-unknown"} ${route.targetMissing ? "status-yellow" : ""} ${routeConflict ? "is-conflict" : ""}"${route.targetMissing && !routeConflict ? ' style="color:var(--yellow)"' : ""} title="${escapeHtml(route.tooltip)}" aria-label="${escapeHtml(route.label)}">${icon(route.explicit ? "route" : "shield")}</span>
        <span class="compact-origin ${origin.className}" title="${escapeHtml(origin.tooltip)}" aria-label="${escapeHtml(origin.label)}">${escapeHtml(origin.label)}</span>
      </div><div class="compact-secondary" title="${escapeHtml(explanation ?? (pending ? `下一回合 ${pending}（待生效）` : ""))}">${secondary}</div>`;
  }
  const counts = roots.reduce((result, conversation) => {
    result[familyLevel(conversation, snapshot.conversations)] += 1;
    return result;
  }, { green: 0, yellow: 0, red: 0, gray: 0 } as Record<StatusLevel, number>);
  const usage = aggregateUsage(snapshot.conversations);
  const statusParts = [counts.green ? `${counts.green} 正常` : "", counts.yellow ? `${counts.yellow} 待确认` : "", counts.red ? `${counts.red} 异常` : ""].filter(Boolean);
  const explanation = overallLevel === "yellow" || overallLevel === "red" ? snapshotExplanation(snapshot) : undefined;
  const origin = compactOriginSummary(snapshot.conversations);
  const metrics = `<span>${formatTokens(usage.total)} tok</span><span class="metric-separator">·</span><span>缓存输入 ${formatPercent(usage.cacheShare)}</span>`;
  return `<div class="compact-primary" title="${escapeHtml([statusParts.join(" · "), explanation].filter(Boolean).join("\n"))}"><span>${roots.length} 个对话</span><span class="dot-separator">·</span><span class="conversation-counts">${escapeHtml(statusParts.join(" · "))}</span><span class="compact-origin ${origin.className}" title="${escapeHtml(origin.tooltip)}" aria-label="${escapeHtml(origin.label)}">${escapeHtml(origin.label)}</span></div>
    <div class="compact-secondary" title="${escapeHtml(explanation ?? "")}">${explanation ? `<span class="compact-alert status-${overallLevel}">${escapeHtml(explanation)}</span><span class="compact-wide-metrics"><span class="metric-separator">·</span>${metrics}</span>` : metrics}</div>`;
}

function renderControlButtons(): string {
  return `<div class="window-controls" aria-label="窗口控制">
    <button class="icon-button progressive-control ${state.topmost ? "is-active" : ""}" type="button" data-action="topmost" data-focus-key="control-topmost" aria-label="${state.topmost ? "取消保持置顶" : "保持置顶"}" aria-pressed="${state.topmost}" title="${state.topmost ? "已保持置顶" : "保持置顶"}">${icon("pin")}</button>
    <button class="icon-button persistent-control" type="button" data-action="expand" data-focus-key="control-expand" aria-label="${state.expanded ? "折叠详情" : "展开详情"}" aria-expanded="${state.expanded}" title="${state.expanded ? "折叠" : "展开"}">${icon(state.expanded ? "collapse" : "expand")}</button>
    <button class="icon-button persistent-control" type="button" data-action="hide" data-focus-key="control-hide" aria-label="最小化到托盘" title="最小化到托盘">${icon("hide")}</button>
    <button class="icon-button progressive-control" type="button" data-action="more" data-focus-key="control-more" aria-label="更多操作" aria-haspopup="menu" aria-expanded="${state.menuOpen}" title="更多操作">${icon("more")}</button>
  </div>`;
}

function metricValues(conversation: ConversationSnapshot): Array<{ key: string; label: string; value: string; tooltip: string }> {
  const last = conversation.usage.last;
  const cumulative = conversation.usage.cumulative;
  const elapsed = conversation.timing.durationMs ?? conversation.timing.elapsedMs;
  const contextUsed = conversation.usage.contextInputShare ?? (
    conversation.usage.contextWindow && last.inputTokens !== undefined
      ? last.inputTokens / conversation.usage.contextWindow
      : undefined
  );
  const ttft = ttftDisplay(conversation.timing);
  const endToEndRate = conversation.timing.endToEndOutputRate ?? conversation.timing.observedOutputRate;
  const modelPhaseRate = conversation.timing.modelPhaseOutputRate;
  return [
    { key: "last", label: "本次 token", value: formatTokens(usageTotal(last)), tooltip: "结构化 token_count 事件的本次用量" },
    { key: "total", label: "累计 token", value: formatTokens(usageTotal(cumulative)), tooltip: "当前任务的结构化累计值" },
    { key: "cache", label: "缓存输入", value: formatPercent(cacheShare(conversation, "last")), tooltip: `${formatTokens(last.cachedInputTokens)} / ${formatTokens(last.inputTokens)} 输入 token；不等于服务端请求命中率` },
    { key: "reasoning", label: "输出 / 推理", value: `${formatTokens(last.outputTokens)} / ${formatTokens(last.reasoningOutputTokens)}`, tooltip: "reasoning output 是输出 token 的子集，不重复累加" },
    { key: "context", label: "本次窗口占比", value: formatPercent(contextUsed), tooltip: `上下文窗口 ${formatTokens(conversation.usage.contextWindow)} token；不是官方“剩余百分比”` },
    { key: "duration", label: conversation.timing.durationMs !== undefined ? "回合耗时" : "已运行", value: formatDuration(elapsed), tooltip: conversation.timing.durationMs !== undefined ? "task_complete 报告的完整回合耗时" : "活动回合的本地观测经过时间" },
    { key: "ttft", label: "TTFT", value: ttft.value, tooltip: ttft.tooltip },
    { key: "rate", label: "端到端输出速率", value: endToEndRate === undefined ? "等待输出" : formatRate(endToEndRate), tooltip: "本回合 output token 除以已运行或终态耗时；包含排队、网络和工具等待，不是服务器纯生成 TPS" },
    { key: "model-rate", label: "模型阶段速率（估算）", value: modelPhaseRate === undefined ? "等待模型片段" : formatRate(modelPhaseRate), tooltip: `只用 Reasoning/AgentMessage 结构化时间区间的并集作为分母；模型阶段 ${formatDuration(conversation.timing.modelActiveMs)}` },
  ];
}

function collectorCopy(snapshot: MonitorSnapshotV5): { level: StatusLevel; text: string; tooltip: string } {
  const health = snapshot.collectorHealth;
  if (health.level === "red") return { level: "red", text: "采集故障", tooltip: health.lastError ?? "采集器发生确定性故障" };
  if (health.parseWarnings > 0) return { level: "yellow", text: `${health.parseWarnings} 个解析警告`, tooltip: health.lastError ?? "部分日志行无法解析；未损坏的结构化证据仍会显示" };
  if (health.level === "yellow") return { level: "yellow", text: "采集待确认", tooltip: "采集数据不完整，等待确认" };
  if (!snapshot.codexRunning) return { level: "gray", text: "Codex 未运行", tooltip: "采集器正在静默等待 Codex" };
  return { level: health.level, text: health.level === "gray" ? "采集状态未知" : "采集正常", tooltip: health.level === "gray" ? "采集器尚未报告健康状态" : "Hook 与 rollout 增量采集器工作正常" };
}

function setText(scope: ParentNode, selector: string, value: string): void {
  const element = scope.querySelector<HTMLElement>(selector);
  if (element && element.textContent !== value) element.textContent = value;
}

function setHidden(element: HTMLElement | null, hidden: boolean): void {
  if (element && element.hidden !== hidden) element.hidden = hidden;
}

function statusClass(element: HTMLElement, base: string, level: StatusLevel): void {
  const next = `${base} status-${level}`;
  if (element.className !== next) element.className = next;
}

function createConversationCard(threadId: string): HTMLDetailsElement {
  const card = document.createElement("details");
  card.className = "conversation-card";
  card.dataset.threadId = threadId;
  card.dataset.scrollKey = `thread:${threadId}`;
  card.innerHTML = `<summary data-focus-key="thread:${escapeHtml(threadId)}">
      <span class="conversation-status status-gray" aria-hidden="true">–</span>
      <span class="conversation-copy"><span class="conversation-title-line"><span class="kind-label" hidden>子任务</span><strong class="conversation-title"></strong><time></time><span class="origin-mini is-unknown"></span></span>
      <span class="conversation-model-line"><span class="request-line"></span><span class="pending-mini" hidden></span><span class="conversation-family-stats"></span></span></span>
      <span class="status-badge status-gray"><span class="status-symbol" aria-hidden="true">–</span><span class="status-label">空闲</span></span><span class="details-chevron" aria-hidden="true">${icon("expand")}</span>
    </summary>
    <div class="conversation-details">
      <div class="root-detail-label" hidden>根任务证据</div>
      <div class="evidence-block">
        <div class="evidence-line"><span class="evidence-label">本回合请求</span><strong class="request-evidence"></strong></div>
        <div class="evidence-line"><span class="evidence-label">服务器路由</span><span class="route-evidence is-unknown"><span class="route-icon">${icon("shield")}</span><span class="route-label"></span></span></div>
        <div class="evidence-line"><span class="evidence-label">连接来源</span><span class="origin-evidence is-unknown"><span class="origin-label"></span></span></div>
        <div class="pending-callout" role="status" hidden><span class="pending-clock" aria-hidden="true">◷</span><span>下一回合：<strong class="pending-value"></strong>（待生效）</span></div>
        <div class="source-note"></div>
      </div>
      <details class="advanced-details" open><summary data-focus-key="advanced:${escapeHtml(threadId)}">完整 token 与性能指标<span aria-hidden="true">⌄</span></summary><div class="advanced-content"><div class="metric-grid" aria-label="Token 与性能指标">${metricValues({
        threadId, kind: "root", title: "", activeRequest: {}, serverRoute: { evidence: "unknown", chain: [] },
        usage: { last: {}, cumulative: {} }, timing: { ttftEvidence: { kind: "pending" } },
        qualityAssessment: { state: "learning", baselineSampleCount: 0, consecutiveHits: 0, factors: [], limitations: [] },
        connectionOrigin: normalizeConnectionOrigin(undefined),
        status: { level: "gray", code: "", explanation: "" }, anomalies: [],
      }).map((metric) => `<div class="metric" data-metric="${metric.key}" data-focus-key="metric:${escapeHtml(threadId)}:${metric.key}" tabindex="0"><span class="metric-label"></span><strong class="metric-value"></strong></div>`).join("")}</div>
      <details class="quality-box" data-focus-key="quality:${escapeHtml(threadId)}">
        <summary><span class="quality-dot" aria-hidden="true"></span><strong class="quality-title">行为学习中</strong><span class="quality-samples"></span><span aria-hidden="true">⌄</span></summary>
        <div class="quality-content"><p class="quality-explanation"></p><dl class="quality-factors"></dl><p class="quality-comparator" hidden></p><ul class="quality-limitations"></ul></div>
      </details>
      <div class="anomaly-box" role="note" hidden><strong>兼容异常提示</strong><ul></ul></div>
      <dl class="identity-list"><div><dt>Thread</dt><dd class="identity-thread"></dd></div><div><dt>Turn</dt><dd class="identity-turn"></dd></div><div><dt>源事件</dt><dd class="identity-time"></dd></div></dl>
      </div></details>
    </div>
    <div class="subtask-list" hidden></div>`;
  return card;
}

function updateConversationCard(
  card: HTMLDetailsElement,
  conversation: ConversationSnapshot,
  isChild: boolean,
  visibleLevel: StatusLevel,
  descendantCount: number,
  familyUsage?: { total?: number; cacheShare?: number },
): void {
  card.dataset.threadId = conversation.threadId;
  card.classList.toggle("is-child", isChild);
  card.classList.toggle("root-session", !isChild);
  const shouldOpen = state.openThreads.has(conversation.threadId);
  if (card.open !== shouldOpen) card.open = shouldOpen;

  const summary = card.querySelector<HTMLElement>("summary");
  if (summary) {
    summary.dataset.focusKey = `thread:${conversation.threadId}`;
    summary.title = buildConversationTooltip(conversation);
    summary.setAttribute("aria-label", `${isChild ? "子智能体" : "根会话"} ${conversation.title}，${STATUS_COPY[visibleLevel].short}`);
  }
  const status = card.querySelector<HTMLElement>(".conversation-status");
  if (status) { statusClass(status, "conversation-status", visibleLevel); status.textContent = STATUS_COPY[visibleLevel].symbol; }
  const kind = card.querySelector<HTMLElement>(".kind-label");
  if (kind) { kind.hidden = !isChild; kind.textContent = conversation.kind === "subagent" ? "子智能体" : "子会话"; }
  setText(card, ".conversation-title", conversation.title);
  setText(card, ".conversation-title-line time", relativeAge(conversation.sourceTimestamp));
  const origin = connectionOriginDisplay(conversation.connectionOrigin);
  const originMini = card.querySelector<HTMLElement>(".origin-mini");
  if (originMini) {
    originMini.className = `origin-mini ${origin.className}`;
    originMini.textContent = origin.compact;
    originMini.title = origin.tooltip;
    originMini.setAttribute("aria-label", origin.label);
  }
  setText(card, ".request-line", `${friendlyModel(conversation.activeRequest.model)} · ${conversation.activeRequest.effort ?? "effort 未知"}（请求）`);
  const pending = pendingLabel(conversation);
  const pendingMini = card.querySelector<HTMLElement>(".pending-mini");
  setHidden(pendingMini, !pending);
  if (pendingMini && pending) pendingMini.textContent = `下一回合 ${pending}（待生效）`;

  const familyStats = card.querySelector<HTMLElement>(".conversation-family-stats");
  if (familyStats) {
    familyStats.hidden = isChild;
    if (!isChild) {
      const children = descendantCount > 0 ? ` · ${descendantCount} 子任务` : "";
      familyStats.textContent = `${formatTokens(familyUsage?.total)} tok · 缓存 ${formatPercent(familyUsage?.cacheShare)}${children}`;
    }
  }
  const badge = card.querySelector<HTMLElement>(".status-badge");
  if (badge) {
    statusClass(badge, "status-badge", visibleLevel);
    badge.title = visibleLevel === conversation.status.level
      ? conversation.status.explanation
      : `${conversation.status.explanation}；所属子任务中有${STATUS_COPY[visibleLevel].long}`;
  }
  setText(card, ".status-symbol", STATUS_COPY[visibleLevel].symbol);
  const visibleStatusLabel = visibleLevel === "yellow" && conversation.qualityAssessment.state === "suspectedDegradation"
    ? "疑似降质"
    : STATUS_COPY[visibleLevel].short;
  setText(card, ".status-label", visibleStatusLabel);
  const rootLabel = card.querySelector<HTMLElement>(".root-detail-label");
  setHidden(rootLabel, isChild);
  const advanced = card.querySelector<HTMLDetailsElement>(":scope > .conversation-details > .advanced-details");
  if (advanced) {
    advanced.dataset.advancedThreadId = conversation.threadId;
    const advancedSummary = advanced.querySelector<HTMLElement>(":scope > summary");
    if (advancedSummary) advancedSummary.dataset.focusKey = `advanced:${conversation.threadId}`;
    advanced.classList.toggle("is-inline", isChild);
    if (!state.openAdvanced.has(conversation.threadId)) state.openAdvanced.set(conversation.threadId, isChild || descendantCount === 0);
    const shouldOpenAdvanced = state.openAdvanced.get(conversation.threadId) ?? false;
    if (advanced.open !== shouldOpenAdvanced) advanced.open = shouldOpenAdvanced;
  }

  setText(card, ".request-evidence", `${friendlyModel(conversation.activeRequest.model)} · ${conversation.activeRequest.effort ?? "未知"}（请求）`);
  const route = routeState(conversation);
  const routeElement = card.querySelector<HTMLElement>(".route-evidence");
  if (routeElement) {
    const routeConflict = isRoutePolicyConflict(conversation);
    routeElement.className = `route-evidence ${route.explicit ? "is-explicit" : "is-unknown"}${route.targetMissing ? " status-yellow" : ""}${routeConflict ? " is-conflict" : ""}`;
    routeElement.style.color = route.targetMissing && !routeConflict ? "var(--yellow)" : "";
    routeElement.title = route.tooltip;
  }
  const routeIcon = card.querySelector<HTMLElement>(".route-icon");
  if (routeIcon && routeIcon.dataset.kind !== String(route.explicit)) {
    routeIcon.dataset.kind = String(route.explicit);
    routeIcon.innerHTML = icon(route.explicit ? "route" : "shield");
  }
  setText(card, ".route-label", route.label);
  const originEvidence = card.querySelector<HTMLElement>(".origin-evidence");
  if (originEvidence) {
    originEvidence.className = `origin-evidence ${origin.className}`;
    originEvidence.title = origin.tooltip;
  }
  setText(card, ".origin-label", origin.label);
  const pendingCallout = card.querySelector<HTMLElement>(".pending-callout");
  setHidden(pendingCallout, !pending);
  if (pending) setText(card, ".pending-value", pending);
  setText(card, ".source-note", `请求证据：${conversation.activeRequest.source ?? "未知来源"} · ${relativeAge(conversation.sourceTimestamp)}`);

  for (const metric of metricValues(conversation)) {
    const element = card.querySelector<HTMLElement>(`.metric[data-metric="${metric.key}"]`);
    if (!element) continue;
    element.title = metric.tooltip;
    element.dataset.focusKey = `metric:${conversation.threadId}:${metric.key}`;
    setText(element, ".metric-label", metric.label);
    setText(element, ".metric-value", metric.value);
  }
  const quality = conversation.qualityAssessment;
  const qualityBox = card.querySelector<HTMLDetailsElement>(".quality-box");
  if (qualityBox) {
    qualityBox.dataset.state = quality.state;
    qualityBox.dataset.focusKey = `quality:${conversation.threadId}`;
    qualityBox.title = quality.state === "suspectedDegradation"
      ? "点击查看观测值、历史中位数、MAD、样本数和证据限制"
      : "点击查看本机行为基线状态与证据限制";
  }
  setText(card, ".quality-title", qualityShortLabel(quality));
  setText(card, ".quality-samples", `${quality.baselineSampleCount} 样本 · 连续命中 ${quality.consecutiveHits}`);
  setText(card, ".quality-explanation", quality.state === "suspectedDegradation"
    ? "至少两个独立信号相对本机同配置历史出现单向偏离，因此标为黄色提醒；这不是服务器模型身份确认。"
    : quality.state === "consistent"
      ? "当前观测落在本机同配置历史范围内。行为一致不代表服务器物理模型已被独立验证。"
      : `同桶健康样本需达到 30 个才会启用判断，目前为 ${quality.baselineSampleCount}/30。`);
  const factors = card.querySelector<HTMLDListElement>(".quality-factors");
  const factorSignature = JSON.stringify(quality.factors);
  if (factors && factors.dataset.signature !== factorSignature) {
    factors.dataset.signature = factorSignature;
    const nodes: Node[] = [];
    for (const factor of quality.factors) {
      const term = document.createElement("dt");
      term.textContent = qualityFactorLabel(factor.code);
      const detail = document.createElement("dd");
      const deviation = factor.robustDeviation === undefined ? "未知" : `${trimDecimal(factor.robustDeviation)}× MAD`;
      detail.textContent = `观测 ${formatQualityNumber(factor.observed, factor.unit)} · 历史中位数 ${formatQualityNumber(factor.baselineMedian, factor.unit)} · MAD ${formatQualityNumber(factor.mad, factor.unit)} · 偏离 ${deviation}`;
      nodes.push(term, detail);
    }
    if (nodes.length === 0) {
      const term = document.createElement("dt"); term.textContent = "当前信号";
      const detail = document.createElement("dd"); detail.textContent = quality.state === "learning" ? "等待足够的完整健康样本" : "未触发保守异常阈值";
      nodes.push(term, detail);
    }
    factors.replaceChildren(...nodes);
  }
  const comparator = card.querySelector<HTMLElement>(".quality-comparator");
  setHidden(comparator, !quality.comparator);
  if (comparator && quality.comparator) {
    const distance = quality.comparator.relativeDistance === undefined ? "未知" : formatPercent(quality.comparator.relativeDistance);
    comparator.textContent = `统计比较：行为更接近本机 ${friendlyModel(quality.comparator.comparedModel)} 请求样本（${quality.comparator.sampleCount ?? 0} 个，距离差 ${distance}）。这仍不能证明实际模型。`;
  }
  const limitations = card.querySelector<HTMLUListElement>(".quality-limitations");
  const limitationValues = quality.limitations.length > 0
    ? quality.limitations
    : ["行为统计只用于黄色提醒，永远不会创建服务器重路由证据。"];
  const limitationSignature = limitationValues.join("\u001f");
  if (limitations && limitations.dataset.signature !== limitationSignature) {
    limitations.dataset.signature = limitationSignature;
    limitations.replaceChildren(...limitationValues.map((value) => {
      const item = document.createElement("li"); item.textContent = value; return item;
    }));
  }
  const anomaly = card.querySelector<HTMLElement>(".anomaly-box");
  setHidden(anomaly, conversation.anomalies.length === 0 || quality.state === "suspectedDegradation");
  const anomalyList = anomaly?.querySelector<HTMLUListElement>("ul");
  const anomalySignature = conversation.anomalies.join("\u001f");
  if (anomalyList && anomalyList.dataset.signature !== anomalySignature) {
    anomalyList.dataset.signature = anomalySignature;
    anomalyList.replaceChildren(...conversation.anomalies.map((value) => {
      const item = document.createElement("li"); item.textContent = value; return item;
    }));
  }
  const thread = card.querySelector<HTMLElement>(".identity-thread");
  if (thread) { thread.textContent = conversation.threadId; thread.title = conversation.threadId; }
  const turn = card.querySelector<HTMLElement>(".identity-turn");
  if (turn) { turn.textContent = conversation.turnId ?? "未知"; turn.title = conversation.turnId ?? "未知"; }
  setText(card, ".identity-time", exactTimestamp(conversation.sourceTimestamp));
}

function directCards(container: HTMLElement): Map<string, HTMLDetailsElement> {
  const cards = new Map<string, HTMLDetailsElement>();
  for (const child of Array.from(container.children)) {
    if (child instanceof HTMLDetailsElement && child.dataset.threadId) cards.set(child.dataset.threadId, child);
  }
  return cards;
}

function placeCardAfter(
  container: HTMLElement,
  card: HTMLDetailsElement,
  previousCard: HTMLDetailsElement | null,
): HTMLDetailsElement {
  const expected = previousCard ? previousCard.nextElementSibling : container.firstElementChild;
  if (expected !== card) container.insertBefore(card, expected);
  return card;
}

function placeElementAfter<T extends HTMLElement>(
  container: HTMLElement,
  element: T,
  previousElement: Element | null,
): T {
  const expected = previousElement ? previousElement.nextElementSibling : container.firstElementChild;
  if (expected !== element) container.insertBefore(element, expected);
  return element;
}

function syncChildren(
  parent: ConversationSnapshot,
  container: HTMLElement,
  all: ConversationSnapshot[],
  visited: Set<string>,
): void {
  const children = childrenFor(parent, all).filter((child) => !visited.has(child.threadId));
  const existing = directCards(container);
  const desired = new Set(children.map((child) => child.threadId));
  let previousCard: HTMLDetailsElement | null = null;
  for (const child of children) {
    visited.add(child.threadId);
    let card = existing.get(child.threadId);
    if (!card) card = createConversationCard(child.threadId);
    const family = conversationFamily(child, all);
    updateConversationCard(card, child, true, familyLevel(child, all), Math.max(0, family.length - 1));
    previousCard = placeCardAfter(container, card, previousCard);
    const nested = card.querySelector<HTMLElement>(":scope > .subtask-list");
    if (nested) {
      syncChildren(child, nested, all, visited);
      nested.hidden = nested.childElementCount === 0;
      nested.setAttribute("aria-label", `${child.title} 的子任务`);
    }
  }
  for (const [threadId, card] of existing) if (!desired.has(threadId)) card.remove();
}

function syncAbnormalList(container: HTMLElement, abnormal: ConversationSnapshot[]): void {
  const existing = directCards(container);
  const desired = new Set(abnormal.map((conversation) => conversation.threadId));
  let previousCard: HTMLDetailsElement | null = null;
  for (const conversation of abnormal) {
    let card = existing.get(conversation.threadId);
    if (!card) card = createConversationCard(conversation.threadId);
    updateConversationCard(card, conversation, true, maxLevel("yellow", conversation.status.level), 0);
    previousCard = placeCardAfter(container, card, previousCard);
    const nested = card.querySelector<HTMLElement>(":scope > .subtask-list");
    if (nested) {
      nested.replaceChildren();
      nested.hidden = true;
    }
  }
  for (const [threadId, card] of existing) if (!desired.has(threadId)) card.remove();
}

interface ScrollAnchor {
  key?: string;
  offset: number;
  top: number;
}

function captureScrollAnchor(scroller: HTMLElement): ScrollAnchor {
  const scrollerRect = scroller.getBoundingClientRect();
  const viewportTop = scrollerRect.top + 1;
  const viewportBottom = scrollerRect.bottom - 1;
  let selected: { element: HTMLElement; rect: DOMRect; distance: number; depth: number } | undefined;
  for (const element of Array.from(scroller.querySelectorAll<HTMLElement>("[data-scroll-key]"))) {
    const rect = element.getBoundingClientRect();
    if (rect.bottom <= viewportTop || rect.top >= viewportBottom) continue;
    let depth = 0;
    for (let parent = element.parentElement; parent && parent !== scroller; parent = parent.parentElement) depth += 1;
    const distance = Math.abs(rect.top - scrollerRect.top);
    if (!selected || distance < selected.distance - .5 || (Math.abs(distance - selected.distance) <= .5 && depth > selected.depth)) {
      selected = { element, rect, distance, depth };
    }
  }
  if (selected) return {
    key: selected.element.dataset.scrollKey,
    offset: selected.rect.top - scrollerRect.top,
    top: scroller.scrollTop,
  };
  return { offset: 0, top: scroller.scrollTop };
}

function restoreScrollAnchor(scroller: HTMLElement, anchor: ScrollAnchor): void {
  if (!anchor.key) { scroller.scrollTop = anchor.top; return; }
  const element = Array.from(scroller.querySelectorAll<HTMLElement>("[data-scroll-key]"))
    .find((candidate) => candidate.dataset.scrollKey === anchor.key);
  if (!element) { scroller.scrollTop = anchor.top; return; }
  const delta = element.getBoundingClientRect().top - scroller.getBoundingClientRect().top - anchor.offset;
  if (Math.abs(delta) > .5) scroller.scrollTop += delta;
}

function markScrollInteraction(): void {
  scrollInteractionGeneration += 1;
  if (scrollRestoreFrame !== undefined) {
    window.cancelAnimationFrame(scrollRestoreFrame);
    scrollRestoreFrame = undefined;
  }
}

function syncConversationList(snapshot: MonitorSnapshotV5, roots: ConversationSnapshot[]): void {
  const scroller = app.querySelector<HTMLElement>(".conversation-scroll");
  if (!scroller) return;
  const rootIds = new Set(roots.map((root) => root.threadId));
  if (roots.length === 0) state.autoOpenedRootId = undefined;
  else if (state.autoOpenedRootId && !rootIds.has(state.autoOpenedRootId)) state.autoOpenedRootId = undefined;
  if (state.expanded && roots.length === 1 && state.autoOpenedRootId !== roots[0].threadId) {
    state.openThreads.add(roots[0].threadId);
    state.autoOpenedRootId = roots[0].threadId;
  }
  const anchor = captureScrollAnchor(scroller);
  const restoreGeneration = scrollInteractionGeneration;
  const focusKey = (document.activeElement as HTMLElement | null)?.dataset.focusKey;
  const existing = directCards(scroller);
  const desired = new Set(roots.map((root) => root.threadId));
  const visited = new Set<string>();
  let previousRoot: HTMLDetailsElement | null = null;
  for (const root of roots) {
    visited.add(root.threadId);
    let card = existing.get(root.threadId);
    if (!card) card = createConversationCard(root.threadId);
    const family = conversationFamily(root, snapshot.conversations);
    updateConversationCard(card, root, false, familyLevel(root, snapshot.conversations), Math.max(0, family.length - 1), aggregateUsage(family));
    previousRoot = placeCardAfter(scroller, card, previousRoot);
    const children = card.querySelector<HTMLElement>(":scope > .subtask-list");
    if (children) {
      syncChildren(root, children, snapshot.conversations, visited);
      children.hidden = children.childElementCount === 0;
      children.setAttribute("aria-label", `${root.title} 的子任务`);
    }
  }
  for (const [threadId, card] of existing) if (!desired.has(threadId)) card.remove();

  const abnormal = abnormalConversations(snapshot.conversations);
  let orphanGroup = scroller.querySelector<HTMLElement>(":scope > .orphan-group");
  if (abnormal.length > 0) {
    if (!orphanGroup) {
      orphanGroup = document.createElement("section");
      orphanGroup.className = "orphan-group";
      orphanGroup.dataset.scrollKey = "orphans";
      orphanGroup.innerHTML = `<header><span class="conversation-status status-yellow" aria-hidden="true">!</span><span><strong>父会话未找到 / 层级异常</strong><small>父会话缺失、失效或层级形成环；所有活动项均保留</small></span></header><div class="orphan-list"></div>`;
    }
    placeElementAfter(scroller, orphanGroup, previousRoot);
    setText(orphanGroup, "header strong", `父会话未找到 / 层级异常 · ${abnormal.length}`);
    const list = orphanGroup.querySelector<HTMLElement>(".orphan-list");
    if (list) syncAbnormalList(list, abnormal);
  } else orphanGroup?.remove();

  let empty = scroller.querySelector<HTMLElement>(":scope > .empty-state");
  const isEmpty = roots.length === 0 && abnormal.length === 0;
  if (isEmpty) {
    if (!empty) {
      empty = document.createElement("div"); empty.className = "empty-state";
      empty.innerHTML = '<span class="empty-mochi" aria-hidden="true"></span><strong></strong><span></span>';
      scroller.appendChild(empty);
    }
    setText(empty, "strong", snapshot.codexRunning ? "等待新回合" : "Codex 还没有启动");
    setText(empty, ":scope > span:last-child", snapshot.codexRunning ? "新回合开始后会在这里出现" : "启动 Codex 后小窗会自动更新");
  } else empty?.remove();

  const canRestore = restoreGeneration === scrollInteractionGeneration && !scrollPointerActive;
  if (canRestore) restoreScrollAnchor(scroller, anchor);
  if (focusKey && !(document.activeElement instanceof HTMLElement && document.activeElement.dataset.focusKey === focusKey)) {
    const target = Array.from(app.querySelectorAll<HTMLElement>("[data-focus-key]"))
      .find((element) => element.dataset.focusKey === focusKey);
    target?.focus({ preventScroll: true });
    if (canRestore) restoreScrollAnchor(scroller, anchor);
  }
  if (scrollRestoreFrame !== undefined) window.cancelAnimationFrame(scrollRestoreFrame);
  scrollRestoreFrame = window.requestAnimationFrame(() => {
    scrollRestoreFrame = undefined;
    if (restoreGeneration !== scrollInteractionGeneration || scrollPointerActive) return;
    restoreScrollAnchor(scroller, anchor);
  });
}

function mountShell(): void {
  app.innerHTML = `<main class="monitor-shell is-compact" aria-label="小狸 XiaoLi Codex 模型监视器">
    <header class="compact-bar" data-tauri-drag-region="deep">
      <span class="drag-grip" data-tauri-drag-region="deep" aria-hidden="true"><i></i><i></i><i></i><i></i><i></i><i></i></span>
      <div class="avatar-host" data-tauri-drag-region="deep">${renderMochi("gray")}</div>
      <div class="compact-copy" data-tauri-drag-region="deep"></div>
      <div class="controls-host" data-tauri-drag-region="false">${renderControlButtons()}</div>
    </header>
    <section class="expanded-panel" aria-label="活动对话详情">
      <header class="panel-heading"><div><strong>活动对话</strong><span class="count-pill">0</span></div><span class="collector-health status-gray"><i aria-hidden="true"></i><span></span></span></header>
      <div class="conversation-scroll" aria-label="活动对话与子任务" tabindex="-1"></div>
      <footer class="panel-footer">
        <button class="text-button" type="button" data-action="theme" data-focus-key="footer-theme">${icon("theme")}<span></span></button>
        <button class="text-button refresh-button" type="button" data-action="refresh" data-focus-key="footer-refresh">${icon("refresh")}<span></span></button>
        <span class="refresh-notice" role="status" hidden></span>
        <time class="checked-at"></time>
      </footer>
    </section>
    <div class="more-menu" role="menu" aria-label="更多操作" hidden>
      <button type="button" role="menuitem" data-action="theme" data-focus-key="menu-theme">${icon("theme")}<span>切换主题</span></button>
      <button type="button" role="menuitem" data-action="refresh" data-focus-key="menu-refresh">${icon("refresh")}<span>立即刷新</span></button>
      <button type="button" role="menuitem" data-action="reset-position" data-focus-key="menu-reset">${icon("reset")}<span>重置窗口位置</span></button>
      <button type="button" role="menuitem" data-action="status-guide" data-focus-key="menu-status-guide">${icon("help")}<span>状态与证据说明</span></button>
      <button class="danger-button" type="button" role="menuitem" data-action="exit" data-focus-key="menu-exit">${icon("close")}<span>退出小狸</span></button>
    </div>
    <section class="status-guide" role="dialog" aria-modal="true" aria-labelledby="status-guide-title" hidden>
      <header><div><strong id="status-guide-title">小狸状态与证据说明</strong><span>颜色表示配置与采集状态，不是服务器物理模型认证</span></div><button class="icon-button" type="button" data-action="status-guide" aria-label="关闭状态说明" title="关闭">${icon("close")}</button></header>
      <div class="status-guide-scroll">
        <article class="status-guide-item status-green"><i aria-hidden="true">✓</i><div><strong>正常</strong><p>本回合请求模型、请求 effort 与任务生效设置一致，采集字段完整。仍可能同时显示“未见服务器重路由”。</p></div></article>
        <article class="status-guide-item status-yellow"><i aria-hidden="true">!</i><div><strong>待确认</strong><p>下一回合配置待生效、字段缺失或解析警告时出现，说明里会给出具体原因。单纯“学习中”保持中性，不会把主状态变黄。</p></div></article>
        <article class="status-guide-item status-yellow"><i aria-hidden="true">≈</i><div><strong>疑似降质</strong><p>至少两个独立行为指标连续偏离本机同配置历史。只能作为黄色统计提醒，不能证明实际模型变成 5.5。</p></div></article>
        <article class="status-guide-item status-red"><i aria-hidden="true">×</i><div><strong>异常</strong><p>同一回合请求证据明确冲突、显式重路由违反策略，或采集器发生无法继续工作的确定性故障。</p></div></article>
        <article class="status-guide-item status-gray"><i aria-hidden="true">–</i><div><strong>空闲</strong><p>Codex 未运行，或当前没有活动回合。小狸会继续在后台等待结构化事件。</p></div></article>
        <article class="status-guide-item route-guide"><i aria-hidden="true">↝</i><div><strong>服务器已重路由</strong><p>只在捕获明确的 <code>model/rerouted</code> 事件时出现，并显示请求模型到服务器目标的链。</p></div></article>
        <article class="status-guide-item status-yellow"><i aria-hidden="true">!</i><div><strong>已重路由，目标未知</strong><p>捕获到明确的 <code>model/rerouted</code>，但事件没有可显示的目标模型。这里只确认发生过重路由；小狸不会根据请求值、速度或文本特征补猜目标。</p></div></article>
        <article class="status-guide-item route-guide"><i aria-hidden="true">◇</i><div><strong>未见服务器重路由</strong><p>只表示小狸没有捕获显式 reroute 事件；它不证明服务器物理模型没有发生变化。</p></div></article>
        <article class="status-guide-item route-guide"><i aria-hidden="true">⌁</i><div><strong>连接来源</strong><p>显示官方登录、官方 API、自定义、本地或未知端点，并区分配置证据、部分证据和证据不足。它只说明连接配置，不能证明服务器物理模型。</p></div></article>
        <article class="status-guide-item route-guide"><i aria-hidden="true">◷</i><div><strong>下一回合待生效</strong><p>活动回合中修改模型或 effort 后，本回合继续保持原请求值，新值要到下一回合开始才成为活动请求。</p></div></article>
        <article class="status-guide-item route-guide"><i aria-hidden="true">◌</i><div><strong>学习中 / 行为一致</strong><p>同桶健康样本少于 30 个时只学习；达到门槛后可比较 TTFT、模型阶段速率与推理比例。行为一致也不等于路由已认证。</p></div></article>
        <article class="status-guide-item route-guide"><i aria-hidden="true">●</i><div><strong>采集徽标</strong><p>“采集正常、解析警告、采集待确认、采集故障、Codex 未运行 / 采集状态未知”只描述采集器健康与运行环境；解析警告会保留仍可解析的证据。</p></div></article>
        <article class="status-guide-item route-guide"><i aria-hidden="true">…</i><div><strong>等待首段 / 等待输出 / 等待模型片段</strong><p>结构化时序或 token 尚不足以计算 TTFT、端到端速率或模型阶段速率。它是指标等待态，不会被显示成 0，也不单独表示异常。</p></div></article>
        <article class="status-guide-item route-guide"><i aria-hidden="true">↻</i><div><strong>刷新中 / 已合并 / 超时 / 失败</strong><p>刷新请求在后台单飞执行，重复点击只合并一次尾随刷新。超时或失败都会保留上一份有效快照；失败提示会显示精简原因，二者都不会伪装成服务器模型结论。</p></div></article>
      </div>
    </section>
    <div class="resize-grip" role="separator" aria-orientation="horizontal" aria-label="调整窗口大小；左右键调宽度，上下键调高度，Shift 加速" aria-keyshortcuts="ArrowLeft ArrowRight ArrowUp ArrowDown" title="拖动或使用方向键调整窗口大小" tabindex="0" data-focus-key="resize-grip"><span aria-hidden="true"></span></div>
  </main><div class="sr-only" aria-live="polite" aria-atomic="true"></div>`;
  resizeObserver = new ResizeObserver(([entry]) => {
    if (!entry) return;
    const width = entry.contentRect.width;
    const height = entry.contentRect.height;
    const baseWidth = state.expanded ? 440 : 304;
    const baseHeight = state.expanded ? 500 : 72;
    const rawScale = Math.sqrt(Math.max(.1, width / baseWidth) * Math.max(.1, height / baseHeight));
    // Compact windows may be very wide while remaining at the 68 DIP minimum.
    // Cap by usable height so width alone cannot enlarge the avatar/text stack
    // until it is clipped; expanded mode has enough vertical room for 1.18.
    const heightScaleLimit = state.expanded ? 1.18 : Math.max(.98, (height - 10) / 58);
    const scale = Math.max(.98, Math.min(1.18, rawScale, heightScaleLimit));
    document.documentElement.style.setProperty("--ui-scale", scale.toFixed(3));
    document.documentElement.dataset.wide = String(width >= (state.expanded ? 480 : 390));
  });
  resizeObserver.observe(app);
}

function updateControls(): void {
  const topmost = app.querySelector<HTMLButtonElement>('[data-action="topmost"]');
  if (topmost) {
    topmost.classList.toggle("is-active", state.topmost);
    topmost.setAttribute("aria-pressed", String(state.topmost));
    topmost.setAttribute("aria-label", state.topmost ? "取消保持置顶" : "保持置顶");
    topmost.title = state.topmost ? "已保持置顶" : "保持置顶";
  }
  const expand = app.querySelector<HTMLButtonElement>('[data-action="expand"]');
  if (expand) {
    expand.setAttribute("aria-expanded", String(state.expanded));
    expand.setAttribute("aria-label", state.expanded ? "折叠详情" : "展开详情");
    expand.title = state.expanded ? "折叠" : "展开";
    if (expand.dataset.expanded !== String(state.expanded)) {
      expand.dataset.expanded = String(state.expanded);
      expand.innerHTML = icon(state.expanded ? "collapse" : "expand");
    }
  }
  const more = app.querySelector<HTMLButtonElement>('[data-action="more"]');
  more?.setAttribute("aria-expanded", String(state.menuOpen));
}

function renderNow(): void {
  const snapshot = state.snapshot;
  const roots = rootConversations(snapshot.conversations);
  const level = summaryLevel(snapshot, roots);
  document.documentElement.dataset.theme = state.theme;
  document.documentElement.dataset.expanded = String(state.expanded);
  const shell = app.querySelector<HTMLElement>(".monitor-shell");
  if (!shell) return;
  shell.className = `monitor-shell ${state.expanded ? "is-expanded" : "is-compact"}${state.menuOpen ? " menu-open" : ""}${state.statusGuideOpen ? " guide-open" : ""}`;
  shell.dataset.theme = state.theme;
  const avatar = app.querySelector<HTMLElement>("[data-status-avatar]");
  if (avatar) {
    statusClass(avatar, "status-stack", level);
    avatar.setAttribute("aria-label", `状态：${STATUS_COPY[level].short}`);
    avatar.title = STATUS_COPY[level].long;
    setText(avatar, ".status-word", STATUS_COPY[level].short);
  }
  const compactCopy = app.querySelector<HTMLElement>(".compact-copy");
  if (compactCopy) compactCopy.innerHTML = renderCompactSummary(snapshot, roots);
  updateControls();

  const collector = collectorCopy(snapshot);
  setText(app, ".count-pill", String(roots.length));
  const collectorHealth = app.querySelector<HTMLElement>(".collector-health");
  if (collectorHealth) { statusClass(collectorHealth, "collector-health", collector.level); collectorHealth.title = collector.tooltip; }
  setText(app, ".collector-health span", collector.text);
  if (state.expanded) syncConversationList(snapshot, roots);
  const panel = app.querySelector<HTMLElement>(".expanded-panel");
  panel?.setAttribute("aria-hidden", String(!state.expanded));
  const themeButton = app.querySelector<HTMLButtonElement>(".panel-footer [data-action=\"theme\"]");
  if (themeButton) themeButton.title = `切换为${state.theme === "cute" ? "极简" : "手绘"}主题`;
  setText(themeButton ?? app, "span", state.theme === "cute" ? "手绘" : "极简");
  const refresh = app.querySelector<HTMLButtonElement>(".refresh-button");
  if (refresh) {
    refresh.disabled = state.refreshing;
    refresh.classList.toggle("is-refreshing", state.refreshing);
    refresh.setAttribute("aria-busy", String(state.refreshing));
  }
  setText(refresh ?? app, "span", state.refreshing ? "刷新中" : "刷新");
  const refreshNotice = app.querySelector<HTMLElement>(".refresh-notice");
  const visibleNotice = state.refreshNotice ?? state.pluginNotice;
  setHidden(refreshNotice, !visibleNotice);
  if (refreshNotice) refreshNotice.textContent = visibleNotice ?? "";
  const checkedAt = app.querySelector<HTMLTimeElement>(".checked-at");
  if (checkedAt) { checkedAt.textContent = relativeAge(snapshot.checkedAt); checkedAt.title = `检查时间：${exactTimestamp(snapshot.checkedAt)}`; checkedAt.dateTime = snapshot.checkedAt; }
  const menu = app.querySelector<HTMLElement>(".more-menu");
  setHidden(menu, !state.menuOpen);
  if (menu) menu.setAttribute("aria-hidden", String(!state.menuOpen));
  const statusGuide = app.querySelector<HTMLElement>(".status-guide");
  setHidden(statusGuide, !state.statusGuideOpen);
  if (statusGuide) statusGuide.setAttribute("aria-hidden", String(!state.statusGuideOpen));
  for (const element of app.querySelectorAll<HTMLElement>(".compact-bar, .expanded-panel, .more-menu, .resize-grip")) {
    element.inert = state.statusGuideOpen;
  }
  const compactBar = app.querySelector<HTMLElement>(".compact-bar");
  if (compactBar) {
    if (state.statusGuideOpen) compactBar.setAttribute("aria-hidden", "true");
    else compactBar.removeAttribute("aria-hidden");
  }
  if (panel) panel.setAttribute("aria-hidden", String(state.statusGuideOpen || !state.expanded));
  setText(app, ".sr-only", `${STATUS_COPY[level].short}，${roots.length} 个活动对话，更新于 ${relativeAge(snapshot.checkedAt)}`);
}

function render(): void {
  if (renderFrame !== undefined) return;
  renderFrame = window.requestAnimationFrame(() => {
    renderFrame = undefined;
    renderNow();
  });
}

async function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args);
}

function errorSnapshot(error: unknown): MonitorSnapshotV5 {
  const message = error instanceof Error ? error.message : String(error);
  return {
    ...state.snapshot,
    checkedAt: new Date().toISOString(),
    collectorHealth: { level: "red", parseWarnings: state.snapshot.collectorHealth.parseWarnings, lastError: `无法读取快照：${message}` },
  };
}

async function loadSnapshot(): Promise<void> {
  const requestId = ++snapshotLoadSerial;
  const eventRevisionAtStart = snapshotEventRevision;
  try {
    const snapshot = normalizeSnapshot(await invokeCommand<unknown>("get_snapshot"));
    if (requestId !== snapshotLoadSerial || eventRevisionAtStart !== snapshotEventRevision) return;
    state.snapshot = snapshot;
    state.connected = true;
  } catch (error) {
    if (requestId !== snapshotLoadSerial || eventRevisionAtStart !== snapshotEventRevision) return;
    if (!IS_TAURI || MOCK_QUERY || window.__XIAOLI_MOCK__ || window.__MOCHI_MOCK__) {
      state.snapshot = normalizeSnapshot(window.__XIAOLI_MOCK__ ?? window.__MOCHI_MOCK__ ?? mockSnapshot());
      if (URL_OPTIONS.get("details") === "all") {
        for (const conversation of state.snapshot.conversations) state.openThreads.add(conversation.threadId);
      } else if (URL_OPTIONS.get("details") === "1") {
        const firstRoot = rootConversations(state.snapshot.conversations)[0];
        if (firstRoot) state.openThreads.add(firstRoot.threadId);
      }
      state.connected = false;
    } else {
      state.snapshot = errorSnapshot(error);
      state.connected = false;
    }
  }
  render();
}

async function toggleExpanded(): Promise<void> {
  state.expanded = !state.expanded;
  if (!state.expanded) state.menuOpen = false;
  render();
  try {
    const result = await invokeCommand<unknown>("toggle_expanded");
    if (typeof result === "boolean") state.expanded = result;
  } catch (error) { if (IS_TAURI) console.error("toggle_expanded failed", error); }
  render();
}

async function toggleTopmost(): Promise<void> {
  const desired = !state.topmost;
  try {
    const result = await invokeCommand<unknown>("set_topmost", { value: desired });
    state.topmost = typeof result === "boolean" ? result : desired;
  } catch (error) {
    if (!IS_TAURI) state.topmost = desired;
    else console.error("set_topmost failed", error);
  }
  render();
}

async function toggleTheme(): Promise<void> {
  const desired: ThemeName = state.theme === "cute" ? "minimal" : "cute";
  state.theme = desired;
  storeTheme(desired);
  render();
  try { await invokeCommand("set_theme", { theme: desired }); }
  catch (error) { if (IS_TAURI) console.error("set_theme failed", error); }
}

function normalizePreferences(value: unknown): UiPreferencesV2 | undefined {
  const raw = asRecord(value);
  if (Object.keys(raw).length === 0) return undefined;
  const theme = textValue(raw.theme) === "minimal" ? "minimal" : "cute";
  return {
    version: numberValue(raw.version) ?? 2,
    theme,
    topmost: booleanValue(raw.topmost, state.topmost),
    expanded: booleanValue(raw.expanded, state.expanded),
  };
}

function applyPreferences(value: unknown): void {
  const preferences = normalizePreferences(value);
  if (!preferences) return;
  state.theme = preferences.theme;
  state.topmost = preferences.topmost;
  state.expanded = preferences.expanded;
  if (!state.expanded) state.menuOpen = false;
  storeTheme(preferences.theme);
  render();
}

async function loadPreferences(): Promise<void> {
  const requestId = ++preferencesLoadSerial;
  const eventRevisionAtStart = preferencesEventRevision;
  try {
    const preferences = await invokeCommand<unknown>("get_ui_preferences");
    if (requestId !== preferencesLoadSerial || eventRevisionAtStart !== preferencesEventRevision) return;
    applyPreferences(preferences);
  }
  catch (error) { if (IS_TAURI) console.error("get_ui_preferences failed", error); }
}

function applyPluginInstallStatus(value: unknown): void {
  const raw = asRecord(value);
  const status: PluginInstallStatus = {
    ok: booleanValue(raw.ok, false),
    changed: booleanValue(raw.changed, false),
    message: textValue(raw.message),
    error: textValue(raw.error),
  };
  state.pluginNotice = !status.ok
    ? `${status.message || "Codex 插件自动安装失败"}${status.error ? `：${conciseExplanation(status.error, 42)}` : ""}`
    : status.changed
      ? (status.message || "Codex 插件已安装")
      : undefined;
  render();
}

async function loadPluginInstallStatus(): Promise<void> {
  if (!IS_TAURI) return;
  try {
    const status = await invokeCommand<unknown>("get_plugin_install_status");
    if (status) applyPluginInstallStatus(status);
  } catch (error) {
    console.error("get_plugin_install_status failed", error);
  }
}

async function refreshNow(): Promise<void> {
  if (state.refreshing) return;
  const requestId = ++refreshRequestSerial;
  const eventRevisionAtStart = snapshotEventRevision;
  state.refreshing = true;
  state.refreshNotice = undefined;
  render();
  let timeoutId: number | undefined;
  try {
    const timeout = new Promise<"timeout">((resolve) => {
      timeoutId = window.setTimeout(() => resolve("timeout"), 15_000);
    });
    const command = IS_TAURI
      ? invokeCommand<RefreshCommandResult>("refresh_now")
      : Promise.resolve({ status: "completed" as const, snapshot: mockSnapshot() });
    const result = await Promise.race([command, timeout]);
    if (result === "timeout") {
      if (requestId === refreshRequestSerial) refreshRequestSerial += 1;
      state.refreshNotice = "本次刷新超时，已保留上一份有效数据";
      return;
    }
    if (requestId !== refreshRequestSerial || eventRevisionAtStart !== snapshotEventRevision) {
      state.refreshNotice = result.status === "coalesced" ? "已合并重复刷新，实时数据已更新" : undefined;
      return;
    }
    state.snapshot = normalizeSnapshot(result.snapshot);
    state.connected = true;
    state.refreshNotice = result.status === "coalesced" ? "已合并重复刷新" : undefined;
  }
  catch (error) {
    if (requestId !== refreshRequestSerial || eventRevisionAtStart !== snapshotEventRevision) return;
    const message = error instanceof Error ? error.message : String(error);
    state.refreshNotice = `刷新失败：${conciseExplanation(message, 34)}`;
  }
  finally {
    if (timeoutId !== undefined) window.clearTimeout(timeoutId);
    state.refreshing = false;
    render();
  }
}

type ActionOrigin = "pointer" | "keyboard" | "programmatic";

function closeMoreMenu(restoreFocus: boolean): void {
  if (!state.menuOpen) return;
  state.menuOpen = false;
  render();
  if (restoreFocus) {
    window.requestAnimationFrame(() => {
      app.querySelector<HTMLButtonElement>('[data-action="more"]')?.focus({ preventScroll: true });
    });
  }
}

function setStatusGuideOpen(open: boolean): void {
  if (open === state.statusGuideOpen) return;
  if (open) {
    const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    statusGuideReturnFocus = active?.closest(".more-menu")
      ? app.querySelector<HTMLButtonElement>('[data-action="more"]')
      : active;
    closeMoreMenu(false);
  }
  state.statusGuideOpen = open;
  render();
  window.requestAnimationFrame(() => {
    if (open) {
      app.querySelector<HTMLButtonElement>('.status-guide [data-action="status-guide"]')?.focus({ preventScroll: true });
    } else {
      const target = statusGuideReturnFocus;
      statusGuideReturnFocus = null;
      if (target?.isConnected) target.focus({ preventScroll: true });
      else app.querySelector<HTMLButtonElement>('[data-action="more"]')?.focus({ preventScroll: true });
    }
  });
}

async function runAction(action: string, origin: ActionOrigin = "programmatic"): Promise<void> {
  switch (action) {
    case "expand": await toggleExpanded(); break;
    case "topmost": await toggleTopmost(); break;
    case "theme": closeMoreMenu(origin === "keyboard"); await toggleTheme(); break;
    case "refresh": closeMoreMenu(origin === "keyboard"); await refreshNow(); break;
    case "more":
      if (state.menuOpen) { closeMoreMenu(origin === "keyboard"); break; }
      if (!state.expanded) await toggleExpanded();
      state.menuOpen = true;
      render();
      requestAnimationFrame(() => app.querySelector<HTMLElement>('.more-menu [role="menuitem"]')?.focus({ preventScroll: true }));
      break;
    case "reset-position":
      closeMoreMenu(origin === "keyboard");
      try { await invokeCommand("reset_window_position"); }
      catch (error) { if (IS_TAURI) console.error("reset_window_position failed", error); }
      break;
    case "status-guide":
      setStatusGuideOpen(!state.statusGuideOpen);
      break;
    case "hide":
      closeMoreMenu(false);
      try { await invokeCommand("hide_to_tray"); }
      catch (error) { if (IS_TAURI) console.error("hide_to_tray failed", error); }
      break;
    case "exit":
      try { await invokeCommand("exit_app"); }
      catch (error) { if (IS_TAURI) console.error("exit_app failed", error); }
      break;
  }
}

app.addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof Element)) return;
  const button = target.closest<HTMLButtonElement>("button[data-action]");
  if (!button || button.disabled) {
    if (state.menuOpen && !target.closest(".more-menu")) closeMoreMenu(false);
    return;
  }
  const origin: ActionOrigin = event.detail === 0 ? "keyboard" : "pointer";
  void runAction(button.dataset.action ?? "", origin);
});

app.addEventListener("contextmenu", (event) => {
  const target = event.target;
  if (!(target instanceof Element) || !target.closest(".monitor-shell") || target.closest("input, textarea, [contenteditable=true], .identity-list dd")) return;
  event.preventDefault();
  if (!state.menuOpen) void runAction("more", "pointer");
});

app.addEventListener("wheel", (event) => {
  const target = event.target;
  if (target instanceof Element && target.closest(".conversation-scroll")) markScrollInteraction();
}, { passive: true });

app.addEventListener("pointerdown", (event) => {
  const target = event.target;
  if (!(target instanceof Element)) return;
  if (target.closest(".conversation-scroll")) {
    scrollPointerActive = true;
    markScrollInteraction();
  }
  if (!target.closest(".resize-grip") || event.button !== 0) return;
  event.preventDefault();
  if (IS_TAURI) void getCurrentWindow().startResizeDragging("SouthEast").catch((error) => console.error("startResizeDragging failed", error));
}, { passive: false });

window.addEventListener("pointerup", () => {
  if (!scrollPointerActive) return;
  scrollPointerActive = false;
  markScrollInteraction();
}, { passive: true });
window.addEventListener("pointercancel", () => {
  if (!scrollPointerActive) return;
  scrollPointerActive = false;
  markScrollInteraction();
}, { passive: true });

app.addEventListener("toggle", (event) => {
  const details = event.target;
  if (!(details instanceof HTMLDetailsElement)) return;
  const advancedThreadId = details.dataset.advancedThreadId;
  if (advancedThreadId) {
    state.openAdvanced.set(advancedThreadId, details.open);
    return;
  }
  const threadId = details.dataset.threadId;
  if (!threadId) return;
  if (details.open) state.openThreads.add(threadId);
  else state.openThreads.delete(threadId);
}, true);

async function attachSnapshotListener(): Promise<void> {
  try {
    unlistenSnapshot = await listen<unknown>("monitor://snapshot", (event) => {
      snapshotEventRevision += 1;
      state.snapshot = normalizeSnapshot(event.payload);
      state.connected = true;
      state.refreshNotice = undefined;
      render();
    });
  } catch (error) { if (IS_TAURI) console.error("monitor://snapshot listener failed", error); }
  try {
    unlistenPreferences = await listen<unknown>("monitor://preferences", (event) => {
      preferencesEventRevision += 1;
      applyPreferences(event.payload);
    });
  } catch (error) { if (IS_TAURI) console.error("monitor://preferences listener failed", error); }
  try {
    unlistenPluginInstall = await listen<unknown>("monitor://plugin-install", (event) => {
      applyPluginInstallStatus(event.payload);
    });
  } catch (error) { if (IS_TAURI) console.error("monitor://plugin-install listener failed", error); }
}

function runMockFocusRegression(): void {
  if (IS_TAURI || !MOCK_QUERY || URL_OPTIONS.get("focusTest") !== "1") return;
  if (renderFrame !== undefined) {
    window.cancelAnimationFrame(renderFrame);
    renderFrame = undefined;
  }
  renderNow();
  const focusTarget = URL_OPTIONS.get("focusTarget") === "orphan" ? "orphan" : "advanced";
  const targetSelector = focusTarget === "orphan"
    ? '.orphan-group .conversation-card > summary[data-focus-key^="thread:"]'
    : '.advanced-details > summary[data-focus-key^="advanced:"]';
  const focusElement = app.querySelector<HTMLElement>(targetSelector);
  if (!focusElement) {
    document.documentElement.dataset.focusRegression = "missing-target";
    return;
  }
  const focusKey = focusElement.dataset.focusKey;
  focusElement.focus({ preventScroll: true });
  state.snapshot = normalizeSnapshot(mockSnapshot());
  renderNow();
  document.documentElement.dataset.focusRegressionTarget = focusTarget;
  document.documentElement.dataset.focusRegression =
    focusKey && (document.activeElement as HTMLElement | null)?.dataset.focusKey === focusKey ? "pass" : "fail";
}

function runMockScrollRegression(): void {
  if (IS_TAURI || MOCK_QUERY !== "scroll" || URL_OPTIONS.get("scrollTest") !== "1") return;
  if (renderFrame !== undefined) {
    window.cancelAnimationFrame(renderFrame);
    renderFrame = undefined;
  }
  renderNow();
  if (scrollRestoreFrame !== undefined) {
    window.cancelAnimationFrame(scrollRestoreFrame);
    scrollRestoreFrame = undefined;
  }
  const scroller = app.querySelector<HTMLElement>(".conversation-scroll");
  const rootCard = app.querySelector<HTMLElement>('[data-thread-id="scroll-root"]');
  if (!scroller || !rootCard) {
    document.documentElement.dataset.scrollRegression = "missing-target";
    return;
  }
  scroller.scrollTop = Math.round(Math.max(0, scroller.scrollHeight - scroller.clientHeight) * .62);
  const anchor = captureScrollAnchor(scroller);
  const anchorElement = Array.from(scroller.querySelectorAll<HTMLElement>("[data-scroll-key]"))
    .find((element) => element.dataset.scrollKey === anchor.key);
  if (!anchorElement || !anchor.key?.startsWith("thread:scroll-child-")) {
    document.documentElement.dataset.scrollRegression = "shallow-anchor";
    return;
  }
  const beforeOffset = anchorElement.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
  const beforeRootHeight = rootCard.getBoundingClientRect().height;
  state.snapshot = {
    ...state.snapshot,
    checkedAt: new Date().toISOString(),
    conversations: state.snapshot.conversations.map((conversation) =>
      conversation.threadId === "scroll-root"
        ? {
            ...conversation,
            pendingNextTurn: { model: "gpt-5.6-sol", effort: "ultra", source: "mock-height-change" },
            status: { level: "yellow", code: "pending_next_turn", explanation: "下一回合配置待生效" },
          }
        : conversation),
  };
  renderNow();
  const afterOffset = anchorElement.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
  const growth = rootCard.getBoundingClientRect().height - beforeRootHeight;
  const deviation = Math.abs(afterOffset - beforeOffset);
  document.documentElement.dataset.scrollRegressionAnchor = anchor.key;
  document.documentElement.dataset.scrollRegressionGrowth = growth.toFixed(2);
  document.documentElement.dataset.scrollRegressionDeviation = deviation.toFixed(2);
  document.documentElement.dataset.scrollRegression = growth > 2 && deviation <= 2 ? "pass" : "fail";
}

function cleanup(): void {
  unlistenSnapshot?.();
  unlistenPreferences?.();
  unlistenPluginInstall?.();
  if (safetyPoll !== undefined) window.clearInterval(safetyPoll);
  if (renderFrame !== undefined) window.cancelAnimationFrame(renderFrame);
  if (scrollRestoreFrame !== undefined) window.cancelAnimationFrame(scrollRestoreFrame);
  resizeObserver?.disconnect();
}

async function resizeWindowFromKeyboard(key: string, accelerated: boolean): Promise<void> {
  if (!IS_TAURI) return;
  const step = accelerated ? 32 : 8;
  try {
    const windowHandle = getCurrentWindow();
    const [physicalSize, scaleFactor] = await Promise.all([windowHandle.innerSize(), windowHandle.scaleFactor()]);
    const width = physicalSize.width / scaleFactor + (key === "ArrowRight" ? step : key === "ArrowLeft" ? -step : 0);
    const height = physicalSize.height / scaleFactor + (key === "ArrowDown" ? step : key === "ArrowUp" ? -step : 0);
    await windowHandle.setSize(new LogicalSize(Math.max(1, width), Math.max(1, height)));
  } catch (error) {
    console.error("keyboard window resize failed", error);
  }
}

window.addEventListener("beforeunload", cleanup, { once: true });
window.addEventListener("keydown", (event) => {
  const target = event.target;
  if (target instanceof Element && target.closest(".resize-grip") && ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) {
    event.preventDefault();
    void resizeWindowFromKeyboard(event.key, event.shiftKey);
    return;
  }
  if (state.statusGuideOpen && event.key === "Tab") {
    const guide = app.querySelector<HTMLElement>(".status-guide");
    const focusable = guide
      ? Array.from(guide.querySelectorAll<HTMLElement>('button:not(:disabled), [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])'))
        .filter((element) => !element.hidden)
      : [];
    if (focusable.length > 0) {
      event.preventDefault();
      const current = focusable.indexOf(document.activeElement as HTMLElement);
      const next = event.shiftKey
        ? (current <= 0 ? focusable.length - 1 : current - 1)
        : (current < 0 || current === focusable.length - 1 ? 0 : current + 1);
      focusable[next]?.focus({ preventScroll: true });
    }
    return;
  }
  if (target instanceof Element && target.closest(".conversation-scroll") && ["ArrowDown", "ArrowUp", "PageDown", "PageUp", "Home", "End", " "].includes(event.key)) {
    markScrollInteraction();
  }
  if (event.shiftKey && event.key === "F10") {
    event.preventDefault();
    if (!state.menuOpen) void runAction("more", "keyboard");
    return;
  }
  if (event.key === "Escape" && state.statusGuideOpen) { event.preventDefault(); setStatusGuideOpen(false); return; }
  if (event.key === "Escape" && state.menuOpen) { event.preventDefault(); closeMoreMenu(true); return; }
  if (state.menuOpen && ["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
    const items = Array.from(app.querySelectorAll<HTMLButtonElement>('.more-menu [role="menuitem"]:not(:disabled)'));
    if (items.length > 0) {
      event.preventDefault();
      const current = items.indexOf(document.activeElement as HTMLButtonElement);
      const next = event.key === "Home" ? 0
        : event.key === "End" ? items.length - 1
          : event.key === "ArrowDown" ? (current + 1 + items.length) % items.length
            : (current - 1 + items.length) % items.length;
      items[next]?.focus({ preventScroll: true });
    }
    return;
  }
  if (event.key === "Escape" && state.expanded) void toggleExpanded();
});

mountShell();
render();
void (async () => {
  // Register both fast-path listeners before the initial reads. Revision and
  // request guards above prevent a slow read from overwriting a newer event.
  await attachSnapshotListener();
  await Promise.all([loadSnapshot(), loadPreferences(), loadPluginInstallStatus()]);
  runMockFocusRegression();
  runMockScrollRegression();
})();

// Events are the fast path. This slow poll repairs a missed event without rescanning rollout bodies.
safetyPoll = window.setInterval(() => {
  if (!document.hidden && !state.refreshing) void loadSnapshot();
}, 30_000);
