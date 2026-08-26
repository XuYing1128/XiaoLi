import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const mochiGray = new URL("./assets/mochi-gray.png", import.meta.url).href;
const mochiGreen = new URL("./assets/mochi-green.png", import.meta.url).href;
const mochiRed = new URL("./assets/mochi-red.png", import.meta.url).href;
const mochiYellow = new URL("./assets/mochi-yellow.png", import.meta.url).href;

type PageId = "overview" | "history" | "relay" | "baselines" | "principles";
type ThemeName = "cute" | "minimal";
type StatusLevel = "green" | "yellow" | "red" | "gray";
type OriginKind = "officialChatGpt" | "officialOpenAiApi" | "officialAnthropicApi" | "managedProvider" | "customEndpoint" | "localEndpoint" | "unknown";
type AuthMode = "chatGpt" | "apiKey" | "external" | "unknown";
type OriginConfidence = "configured" | "partial" | "unknown";
type RelayProtocol = "openAiResponses" | "openAiChatCompletions" | "anthropicMessages";
type AuditMode = "quick" | "standard" | "deep";
type AuditVerdict = "consistent" | "insufficientEvidence" | "suspectedPadding" | "suspectedDegradation" | "significantlyDifferent" | "confirmedContractMismatch" | "failed" | "cancelled";
type SelectiveServiceState = "notApplicable" | "insufficientEvidence" | "noMismatchObserved" | "suspectedSelectiveService";

interface ConnectionOriginSnapshot {
  kind: OriginKind;
  authMode: AuthMode;
  confidence: OriginConfidence;
  providerId?: string;
  endpointClass?: string;
  evidence: string[];
  limitations: string[];
}

interface AxisFinding {
  level: StatusLevel;
  state: string;
  summary: string;
  details: string[];
}

interface OverviewConversation {
  threadId: string;
  turnId?: string;
  displayName: string;
  model?: string;
  effort?: string;
  origin: ConnectionOriginSnapshot;
  statusLevel: StatusLevel;
  statusText: string;
  totalTokens?: number;
  cacheInputShare?: number;
  sourceTimestamp?: string;
  childCount: number;
}

interface WorkbenchOverview {
  checkedAt: string;
  collectorLevel: StatusLevel;
  activeConversationCount: number;
  officialCount: number;
  customCount: number;
  unknownCount: number;
  totalTokens?: number;
  cacheInputShare?: number;
  dominantOrigin: ConnectionOriginSnapshot;
  axisSummary: Record<"protocol" | "usage" | "quality" | "identity", AxisFinding>;
  conversations: OverviewConversation[];
  recentAlerts: string[];
}

interface HistoryEntry extends OverviewConversation {
  id: string;
  localAlias?: string;
  startedAt?: string;
  completedAt?: string;
  ttftMs?: number;
  outputRate?: number;
  reasoningTokens?: number;
  routeEvidence: string;
}

interface RelayProfile {
  id: string;
  label: string;
  normalizedBaseUrl: string;
  protocol: RelayProtocol;
  defaultModel: string;
  credentialRef?: string;
  privateProbePack?: PrivateProbePackReference;
  createdAt: string;
  updatedAt: string;
}

interface PrivateProbePackReference {
  path: string;
  version: string;
  sha256: string;
}

interface RelayAuditProgress {
  auditId: string;
  phase: string;
  completedCases: number;
  totalCases: number;
  usedRequests: number;
  tokenEstimate: number;
  currentDetector: string;
}

interface SelectiveServiceAssessment {
  state: SelectiveServiceState;
  sampleCount: number;
  suspiciousCount: number;
  suspiciousShare?: number;
  windowDays: number;
  reasons: string[];
  limitations: string[];
}

interface RelayAuditReport {
  auditId: string;
  profileId?: string;
  profileLabel: string;
  claimedModel: string;
  protocol: RelayProtocol;
  startedAt?: string;
  completedAt?: string;
  overallVerdict: AuditVerdict;
  confidence: string;
  protocolFindings: AxisFinding;
  usageReconciliation: AxisFinding;
  qualityFindings: AxisFinding;
  fingerprintFindings: AxisFinding;
  reasons: string[];
  limitations: string[];
  quantitativeEvidence: Array<{ label: string; value: string }>;
  selectiveServiceAssessment?: SelectiveServiceAssessment;
}

interface RelayBaseline {
  id: string;
  label: string;
  model: string;
  protocol?: RelayProtocol;
  source: "official" | "community" | "user";
  version?: string;
  sampleCount: number;
  createdAt?: string;
  expiresAt?: string;
  signed: boolean;
  builtIn: boolean;
  referenceProtocol?: string;
  scoringMode?: string;
  limitations: string[];
}

interface AuditSchedule {
  enabled: boolean;
  profileId?: string;
  officialBaselineProfileId?: string;
  cadence: "daily" | "weekly";
  weekday: number;
  localTime: string;
  pairOfficial: boolean;
  monthlyRequestLimit: number;
  historyRetentionDays: number | null;
  nextRunAt?: string;
  lastRunAt?: string;
  lastStatus?: string;
  budgetMonth?: string;
  monthlyReservedRequests: number;
}

interface BudgetPreset {
  label: string;
  requestLimit: number;
  inputTokenLimit: number;
  outputTokenLimit: number;
  timeoutMs: number;
  detectors: string[];
}

interface AuditPlanPreview {
  builtInRequests: number;
  privateProbeRequests: number;
  plannedRequests: number;
  conservativeInputTokens: number;
  conservativeOutputTokens: number;
  privateProbeInputTokens: number;
  privateProbeOutputTokens: number;
  fitsDeclaredBudget: boolean;
}

interface WorkbenchState {
  page: PageId;
  theme: ThemeName;
  connected: boolean;
  loading: Set<string>;
  overview: WorkbenchOverview;
  history: HistoryEntry[];
  historyTotal: number;
  profiles: RelayProfile[];
  audits: RelayAuditReport[];
  baselines: RelayBaseline[];
  schedule: AuditSchedule;
  selectedProfileId?: string;
  activeAudit?: RelayAuditProgress;
  credentials: Map<string, string>;
  unlisteners: UnlistenFn[];
  scheduleDraftDirty: boolean;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

const IS_TAURI = Boolean(window.__TAURI_INTERNALS__);
const QUERY = new URLSearchParams(window.location.search);
const MOCK_MODE = QUERY.get("mock") === "1";
const mount = document.querySelector<HTMLElement>("#workbench") ?? document.body.appendChild(document.createElement("div"));

const STATUS_ORDER: Record<StatusLevel, number> = { gray: 0, green: 1, yellow: 2, red: 3 };
const STATUS_TEXT: Record<StatusLevel, string> = { green: "正常", yellow: "待确认", red: "异常", gray: "证据不足" };
const ORIGIN_TEXT: Record<OriginKind, string> = {
  officialChatGpt: "OpenAI 官方 ChatGPT 登录",
  officialOpenAiApi: "OpenAI 官方 API",
  officialAnthropicApi: "Anthropic 官方 API",
  managedProvider: "托管模型提供方",
  customEndpoint: "自定义端点",
  localEndpoint: "本地端点",
  unknown: "连接来源未知",
};
const PROTOCOL_TEXT: Record<RelayProtocol, string> = {
  openAiResponses: "OpenAI Responses",
  openAiChatCompletions: "OpenAI Chat Completions",
  anthropicMessages: "Anthropic Messages",
};
const VERDICT_TEXT: Record<AuditVerdict, string> = {
  consistent: "与本次参考一致",
  insufficientEvidence: "证据不足",
  suspectedPadding: "疑似过量计数",
  suspectedDegradation: "疑似降质",
  significantlyDifferent: "与参考显著不同",
  confirmedContractMismatch: "明确契约异常",
  failed: "检测失败",
  cancelled: "已取消",
};
const SELECTIVE_SERVICE_TEXT: Record<SelectiveServiceState, string> = {
  notApplicable: "本次不适用",
  insufficientEvidence: "真实会话样本不足",
  noMismatchObserved: "未见审计期异常",
  suspectedSelectiveService: "疑似选择性服务",
};
const BUDGETS: Record<AuditMode, BudgetPreset> = {
  quick: {
    label: "快速",
    requestLimit: 150,
    inputTokenLimit: 1_200_000,
    outputTokenLimit: 120_000,
    timeoutMs: 30 * 60_000,
    detectors: ["protocol", "usage", "qualityBasic", "fingerprint"],
  },
  standard: {
    label: "标准",
    requestLimit: 320,
    inputTokenLimit: 3_000_000,
    outputTokenLimit: 300_000,
    timeoutMs: 60 * 60_000,
    detectors: ["protocol", "usage", "quality", "fingerprint", "mmd", "cacheEvasion"],
  },
  deep: {
    label: "深度",
    requestLimit: 720,
    inputTokenLimit: 8_000_000,
    outputTokenLimit: 720_000,
    timeoutMs: 120 * 60_000,
    detectors: ["protocol", "usage", "quality", "fingerprint", "mmd", "cacheEvasion", "stability", "paraphraseDrift"],
  },
};
const BUILT_IN_REQUESTS: Record<AuditMode, number> = { quick: 140, standard: 308, deep: 716 };
const EMPTY_AXIS: AxisFinding = { level: "gray", state: "notRun", summary: "尚未运行主动检测", details: [] };

function emptyOrigin(): ConnectionOriginSnapshot {
  return { kind: "unknown", authMode: "unknown", confidence: "unknown", evidence: [], limitations: ["尚未取得连接来源证据"] };
}

function emptyOverview(): WorkbenchOverview {
  return {
    checkedAt: new Date().toISOString(),
    collectorLevel: "gray",
    activeConversationCount: 0,
    officialCount: 0,
    customCount: 0,
    unknownCount: 0,
    dominantOrigin: emptyOrigin(),
    axisSummary: { protocol: { ...EMPTY_AXIS }, usage: { ...EMPTY_AXIS }, quality: { ...EMPTY_AXIS }, identity: { ...EMPTY_AXIS } },
    conversations: [],
    recentAlerts: [],
  };
}

const state: WorkbenchState = {
  page: readInitialPage(),
  theme: readTheme(),
  connected: IS_TAURI,
  loading: new Set(),
  overview: emptyOverview(),
  history: [],
  historyTotal: 0,
  profiles: [],
  audits: [],
  baselines: [],
  schedule: { enabled: false, cadence: "weekly", weekday: 1, localTime: "20:00", pairOfficial: false, monthlyRequestLimit: 1_000, monthlyReservedRequests: 0, historyRetentionDays: 180 },
  credentials: new Map(),
  unlisteners: [],
  scheduleDraftDirty: false,
};

const UI_REFRESH_TIMEOUT_MS = 15_000;
let overviewLoadSerial = 0;
let historyLoadSerial = 0;
let profilesLoadSerial = 0;
let auditsLoadSerial = 0;
let baselinesLoadSerial = 0;
let scheduleLoadSerial = 0;
let auditEventRevision = 0;
let overviewEventRevision = 0;
let overviewReloadActive = false;
let overviewReloadTrailing = false;
let dialogReturnFocus: HTMLElement | null = null;

class UiRefreshTimeoutError extends Error {
  constructor() {
    super("ui_refresh_timeout");
  }
}

function readInitialPage(): PageId {
  const page = QUERY.get("page");
  return page === "history" || page === "relay" || page === "baselines" || page === "principles" ? page : "overview";
}

function readTheme(): ThemeName {
  try { return localStorage.getItem("xiaoli-theme") === "minimal" ? "minimal" : "cute"; }
  catch { return "cute"; }
}

function icon(name: "home" | "history" | "relay" | "baseline" | "book" | "refresh" | "theme" | "shield" | "close" | "search" | "play" | "stop" | "edit" | "trash" | "check" | "key" | "clock"): string {
  const paths: Record<typeof name, string> = {
    home: '<path d="M3 10.5 12 3l9 7.5"/><path d="M5.5 9.5V21h13V9.5M9 21v-7h6v7"/>',
    history: '<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5M12 7v5l3 2"/>',
    relay: '<path d="M5 7h12l-3-3M19 17H7l3 3"/><path d="M17 7l-3 3M7 17l3-3"/>',
    baseline: '<path d="M4 20h16M6 16l3-4 3 2 5-7 2 2"/><path d="M6 4v12M19 4v16"/>',
    book: '<path d="M4 4.5A3.5 3.5 0 0 1 7.5 1H20v17H7.5A3.5 3.5 0 0 0 4 21.5z"/><path d="M4 4.5v17M8 6h8M8 10h7"/>',
    refresh: '<path d="M20 6v5h-5"/><path d="M18.1 16a8 8 0 1 1 .9-9l1 4"/>',
    theme: '<path d="M12 3a9 9 0 1 0 9 9c0-1.2-.8-2-2-2h-2.2a2 2 0 0 1-1.7-3l.8-1.3c.7-1.2-.1-2.7-1.5-2.7z"/><circle cx="7.5" cy="10" r=".8"/><circle cx="10" cy="6.5" r=".8"/><circle cx="7.5" cy="14" r=".8"/>',
    shield: '<path d="M12 2 4.5 5v6c0 5 3.2 8.7 7.5 11 4.3-2.3 7.5-6 7.5-11V5z"/><path d="m9 12 2 2 4-5"/>',
    close: '<path d="m6 6 12 12M18 6 6 18"/>',
    search: '<circle cx="10.5" cy="10.5" r="6.5"/><path d="m15.5 15.5 5 5"/>',
    play: '<path d="m8 5 11 7-11 7z"/>',
    stop: '<rect x="6" y="6" width="12" height="12" rx="2"/>',
    edit: '<path d="M4 20h4l11-11-4-4L4 16zM13.5 6.5l4 4"/>',
    trash: '<path d="M4 7h16M9 7V4h6v3M7 7l1 14h8l1-14M10 11v6M14 11v6"/>',
    check: '<path d="m5 12 4 4L19 6"/>',
    key: '<circle cx="8" cy="15" r="4"/><path d="m11 12 9-9M16 7l2 2M13 10l2 2"/>',
    clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  };
  return `<svg viewBox="0 0 24 24" aria-hidden="true">${paths[name]}</svg>`;
}

function mountShell(): void {
  mount.id = "workbench";
  mount.innerHTML = `
    <div class="workbench-shell">
      <aside class="sidebar" aria-label="工作台主导航">
        <div class="brand">
          <div class="brand-avatar-wrap" aria-hidden="true">
            <img class="brand-avatar" alt="" />
            <span class="brand-status-dot"></span>
          </div>
          <div class="brand-copy"><h1 class="brand-name">小狸</h1><span class="brand-subtitle">XiaoLi · 本地证据工作台</span></div>
        </div>
        <nav class="side-nav">
          ${navButton("overview", "home", "总览")}
          ${navButton("history", "history", "会话历史", true)}
          ${navButton("relay", "relay", "中转检测")}
          ${navButton("baselines", "baseline", "参考资料", true)}
          ${navButton("principles", "book", "检测原理")}
        </nav>
        <div class="sidebar-note"><strong>只记录指标与证据</strong>不保存提示词、回复正文、完整工作路径或 API Key。</div>
        <div class="sidebar-footer"><span class="status-dot status-gray"></span><span class="backend-status">正在连接本地采集器</span></div>
      </aside>
      <main class="workspace">
        <header class="topbar">
          <div class="topbar-copy"><span class="eyebrow">本地·只读证据</span><h1 class="page-title"></h1></div>
          <div class="topbar-actions">
            <span class="connection-chip"><span class="status-dot status-gray"></span><span class="connection-chip-text">来源未知</span></span>
            <button class="icon-button" type="button" data-action="theme" title="切换手绘/极简主题" aria-label="切换手绘/极简主题"><span class="button-icon">${icon("theme")}</span></button>
            <button class="secondary-button" type="button" data-action="refresh" title="刷新当前页"><span class="button-icon">${icon("refresh")}</span><span>刷新</span></button>
          </div>
        </header>
        <div class="notice-region" aria-live="polite"><div class="notice" hidden><span class="inline-icon">${icon("shield")}</span><span class="notice-text"></span></div></div>
        <div class="page-stack">
          ${overviewPage()}
          ${historyPage()}
          ${relayPage()}
          ${baselinesPage()}
          ${principlesPage()}
        </div>
      </main>
    </div>
    ${conversationDialog()}
    ${reportDialog()}
    <div class="sr-only" aria-live="polite"></div>`;
  const avatar = mount.querySelector<HTMLImageElement>(".brand-avatar");
  if (avatar) avatar.src = mochiGray;
  applyTheme();
}

function navButton(page: PageId, iconName: Parameters<typeof icon>[0], label: string, counted = false): string {
  return `<button class="nav-button" type="button" data-page="${page}" aria-current="false"><span class="nav-icon">${icon(iconName)}</span><span class="nav-label">${label}</span>${counted ? '<span class="nav-count">0</span>' : ""}</button>`;
}

function overviewPage(): string {
  return `<section class="page" data-page-panel="overview" aria-labelledby="overview-title">
    <h2 id="overview-title" class="sr-only">总览</h2>
    <div class="summary-grid">
      ${summaryCard("active", "活动会话", "0", "当前存在活动回合")}
      ${summaryCard("official", "官方来源", "0", "端点与认证模式均匹配")}
      ${summaryCard("custom", "自定义 / 本地", "0", "只分类，不等于恶意中转")}
      ${summaryCard("tokens", "可观测 token", "—", "根任务与子智能体去重汇总")}
    </div>
    <div class="overview-layout">
      <div class="stack">
        <article class="card origin-strip">
          <span class="origin-mark inline-icon">${icon("shield")}</span>
          <div class="origin-copy"><strong class="origin-title">连接来源待识别</strong><span class="origin-detail">小狸不会根据速度或文风猜测来源</span></div>
          <span class="status-chip status-gray">证据不足</span>
        </article>
        <article class="card card-pad">
          <div class="section-heading"><div><h2>四条证据轴</h2><p>各轴独立展示，不合成误导性的“真模型概率”。</p></div></div>
          <div class="axis-grid">
            ${axisCard("protocol", "协议兼容", "检查认证、基础响应、SSE 与错误契约")}
            ${axisCard("usage", "计量一致", "检查 usage 算术；可选实时官方配对")}
            ${axisCard("quality", "行为质量", "结构化 JSON、nonce 检索、约束推理与多语言")}
            ${axisCard("identity", "模型身份", "无实时官方配对时固定为证据不足")}
          </div>
        </article>
        <article class="card card-pad">
          <div class="section-heading"><div><h2>当前活动会话</h2><p>请求模型、effort、来源和 token 按会话展示。</p></div><time class="overview-checked-at">刚刚</time></div>
          <div class="data-table-wrap overview-table-wrap"><table class="data-table"><thead><tr><th style="width:29%">会话</th><th style="width:20%">请求配置</th><th style="width:20%">连接来源</th><th style="width:17%">Token / 缓存</th><th style="width:14%">状态</th></tr></thead><tbody class="overview-conversation-body"></tbody></table></div>
          <div class="empty-state overview-empty"><div><strong>暂无活动会话</strong><span>Codex 开始回合后，这里会显示请求证据与连接来源。</span></div></div>
        </article>
      </div>
      <aside class="stack">
        <article class="card card-pad">
          <div class="section-heading"><div><h2>本地设置</h2><p>定时审计默认关闭，不会静默消耗额度。</p></div></div>
          <form class="settings-list" id="schedule-form">
            <label class="check-row"><input id="schedule-enabled" type="checkbox" /><span>启用受预算限制的定时检查</span></label>
            <div class="form-grid two-columns">
              <label class="field"><span>检测端点</span><select id="schedule-profile"><option value="">请先选择端点</option></select></label>
              <label class="field"><span>频率</span><select id="schedule-cadence"><option value="weekly">每周</option><option value="daily">每日</option></select></label>
              <label class="field"><span>每周日期</span><select id="schedule-weekday"><option value="1">周一</option><option value="2">周二</option><option value="3">周三</option><option value="4">周四</option><option value="5">周五</option><option value="6">周六</option><option value="0">周日</option></select></label>
              <label class="field"><span>本地时间</span><input id="schedule-time" type="time" value="20:00" /></label>
               <label class="field"><span>每月请求上限</span><input id="schedule-monthly-limit" type="number" min="150" max="100000" step="1" value="1000" /><small class="field-help schedule-budget-state">本月已预留 0 / 1,000，剩余 1,000</small></label>
              <label class="field"><span>历史指标保留</span><select id="history-retention"><option value="30">30 天</option><option value="90">90 天</option><option value="180" selected>180 天</option><option value="forever">永久</option></select></label>
            </div>
            <label class="check-row"><input id="schedule-pair-official" type="checkbox" /><span>定时任务允许调用实时官方配对端点（会额外消耗）</span></label>
            <label class="field"><span>官方配对端点</span><select id="schedule-official-profile"><option value="">不启用官方配对</option></select></label>
            <p class="cell-secondary schedule-status">定时检查保持关闭；不会静默消耗额度。</p>
            <div class="form-actions"><button class="secondary-button" type="submit">保存设置</button></div>
          </form>
        </article>
        <article class="card card-pad">
          <div class="section-heading"><div><h2>最近提示</h2><p>只显示采集与证据结论，不显示对话正文。</p></div></div>
          <ul class="reason-list overview-alerts"></ul>
          <div class="empty-state overview-alerts-empty"><div><strong>没有需要处理的提示</strong><span>有明确冲突、证据缺失或疑似异常时会出现在这里。</span></div></div>
        </article>
      </aside>
    </div>
  </section>`;
}

function summaryCard(key: string, label: string, value: string, meta: string): string {
  return `<article class="card summary-card" data-summary="${key}"><span class="summary-label"><span class="status-dot status-gray"></span>${label}</span><strong class="summary-value">${value}</strong><span class="summary-meta">${meta}</span></article>`;
}

function axisCard(key: string, title: string, description: string): string {
  return `<div class="axis-card" data-axis="${key}"><div class="axis-card-header"><span class="status-dot status-gray"></span><strong>${title}</strong><span class="axis-state status-gray">未检测</span></div><p>${description}</p></div>`;
}

function historyPage(): string {
  return `<section class="page" data-page-panel="history" aria-labelledby="history-title" hidden>
    <div class="section-heading"><div><h2 id="history-title">会话历史</h2><p>只保存模型请求、来源分类、token、缓存、时序和证据状态。</p></div></div>
    <form class="card history-filter" id="history-filter-form">
      <label class="field"><span>搜索短 ID / 本地别名</span><input id="history-query" type="search" autocomplete="off" placeholder="例如 7f31 或本地别名" /></label>
      <label class="field"><span>模型</span><input id="history-model" type="text" autocomplete="off" placeholder="全部" /></label>
      <label class="field"><span>Effort</span><select id="history-effort"><option value="">全部</option><option>low</option><option>medium</option><option>high</option><option>xhigh</option><option>max</option><option>ultra</option></select></label>
      <label class="field"><span>连接来源</span><select id="history-origin"><option value="">全部</option><option value="official">官方</option><option value="custom">自定义 / 本地</option><option value="unknown">未知</option></select></label>
      <label class="field"><span>状态</span><select id="history-status"><option value="">全部</option><option value="green">正常</option><option value="yellow">待确认</option><option value="red">异常</option><option value="gray">证据不足</option></select></label>
      <div class="form-actions"><button class="primary-button" type="submit"><span class="button-icon">${icon("search")}</span>筛选</button><button class="text-button" type="button" data-action="history-reset">重置</button></div>
    </form>
    <article class="card card-pad">
      <div class="data-table-wrap history-table-wrap"><table class="data-table"><thead><tr><th style="width:24%">会话</th><th style="width:17%">时间</th><th style="width:18%">模型 / Effort</th><th style="width:16%">来源</th><th style="width:14%">Token / 缓存</th><th style="width:11%">状态</th></tr></thead><tbody class="history-body"></tbody></table></div>
      <div class="empty-state history-empty"><div><strong>还没有可显示的历史</strong><span>会话完成后指标会进入本地历史；原始提示词和回复不会入库。</span></div></div>
      <div class="history-footer"><span class="history-count">共 0 条</span><button class="text-button" type="button" data-action="history-more">加载更多</button></div>
    </article>
  </section>`;
}

function relayPage(): string {
  return `<section class="page" data-page-panel="relay" aria-labelledby="relay-title" hidden>
    <div class="section-heading"><div><h2 id="relay-title">中转检测</h2><p>手动发起协议、计量、质量与行为指纹审计。通过不等于物理模型已获证明。</p></div></div>
    <div class="relay-layout">
      <div class="stack">
        <article class="card relay-form-card">
          <div class="section-heading"><div><h2>端点与凭据</h2><p>API Key 默认仅保留在当前进程内存。</p></div><button class="text-button" type="button" data-action="profile-new">新建</button></div>
          <form class="form-grid" id="relay-profile-form" autocomplete="off">
            <input id="relay-profile-id" type="hidden" />
            <div class="form-grid two-columns">
              <label class="field"><span>名称</span><input id="relay-label" required maxlength="80" placeholder="例如 开发环境中转" /></label>
              <label class="field"><span>协议</span><select id="relay-protocol" required><option value="openAiResponses">OpenAI Responses</option><option value="openAiChatCompletions">OpenAI Chat Completions</option><option value="anthropicMessages">Anthropic Messages</option></select></label>
            </div>
            <label class="field"><span>Base URL</span><input id="relay-base-url" type="url" required spellcheck="false" autocomplete="off" placeholder="https://gateway.example.com/v1" /><small class="field-help">保存时会移除 userinfo、query 和 fragment；非本机 HTTP 仅允许逐次手动确认，定时审计会拒绝。</small></label>
            <label class="field"><span>声称模型</span><input id="relay-model" required maxlength="120" spellcheck="false" autocomplete="off" placeholder="例如 gpt-5.6-sol" /></label>
            <label class="field secret-field"><span>API Key</span><input id="relay-api-key" type="password" autocomplete="new-password" spellcheck="false" placeholder="不会写入日志或 SQLite" /><button class="text-button" type="button" data-action="toggle-secret" aria-pressed="false">显示</button></label>
            <label class="check-row"><input id="relay-keychain" type="checkbox" /><span>明确允许保存到系统凭据库；不可用时仍只保留在内存</span></label>
            <div class="field"><label for="relay-private-probe-path">私有 probe pack（可选）</label><input id="relay-private-probe-path" type="text" maxlength="4096" autocomplete="off" spellcheck="false" placeholder="本地 JSON 绝对路径，例如 D:\\probes\\my-pack.json" /><small class="field-help"><span class="private-probe-state">未选择</span> · 仅保存路径、版本和 SHA-256；任务正文只在审计开始时读取。 <button class="text-button" type="button" data-action="private-probe-clear">清除</button></small></div>
            <div class="privacy-callout"><strong>边界：</strong>小狸不读取或复用 Codex OAuth token，不执行中转返回的代码、工具、URL 或指令；私有题包只允许本地精确文本/精确 JSON scorer。</div>
            <div class="form-actions"><button class="secondary-button" type="button" data-action="connection-test"><span class="button-icon">${icon("check")}</span>连接测试</button><button class="primary-button" type="submit"><span class="button-icon">${icon("key")}</span>保存配置</button><span class="connection-test-state" role="status"></span></div>
          </form>
        </article>
        <article class="card profiles-card">
          <div class="section-heading"><div><h2>已保存端点</h2><p>列表只包含规范化 URL 和凭据引用。</p></div></div>
          <div class="profile-list"></div>
          <div class="empty-state profile-empty"><div><strong>还没有中转配置</strong><span>填写上方端点后可先做最多 6 次请求的连接测试。</span></div></div>
        </article>
      </div>
      <div class="stack">
        <article class="card audit-card">
          <div class="section-heading"><div><h2>一键审计</h2><p>开始前核对硬上限。网络重试也计入请求数。</p></div></div>
          <div class="form-grid">
            <fieldset class="field" style="border:0;padding:0;margin:0"><legend class="fieldset-label">检测档位</legend><div class="mode-switch">
              <label class="mode-option"><input type="radio" name="audit-mode" value="quick" checked /><span>快速<small>8 cells × 15</small></span></label>
              <label class="mode-option"><input type="radio" name="audit-mode" value="standard" /><span>标准<small>16 cells × 15</small></span></label>
              <label class="mode-option"><input type="radio" name="audit-mode" value="deep" /><span>深度<small>40 cells × 15</small></span></label>
            </div></fieldset>
             <label class="field"><span>可选实时官方配对端点</span><select id="audit-baseline"><option value="">不配对（自洽检查；质量/身份灰色）</option></select><small class="field-help">只列出同协议、同精确模型的第一方 profile；导入参考摘要不参与评分。</small></label>
            <div class="budget-panel" aria-label="审计预算上限">
              <div class="budget-item"><span>最大请求</span><strong class="budget-requests">150</strong></div>
              <div class="budget-item"><span>输入 token 上限</span><strong class="budget-input">1.2m</strong></div>
              <div class="budget-item"><span>输出 token 上限</span><strong class="budget-output">120k</strong></div>
            </div>
            <p class="audit-warning">检测会产生真实 API 费用。“通过”只表示本次范围内未见显著异常，无法密码学证明物理模型。</p>
            <div class="form-actions"><button class="primary-button" type="button" data-action="audit-start"><span class="button-icon">${icon("play")}</span><span class="button-label">开始审计</span></button></div>
            <div class="progress-shell" aria-live="polite" aria-atomic="false" hidden>
              <div class="progress-heading"><strong class="progress-title" id="audit-progress-title">正在准备</strong><span class="progress-count" id="audit-progress-count">0 / 0</span></div>
              <div class="progress-track" role="progressbar" aria-labelledby="audit-progress-title" aria-describedby="audit-progress-count" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0" aria-valuetext="正在准备，0 / 0"><div class="progress-bar"></div></div>
              <div class="progress-meta"><span class="progress-detector">等待调度</span><span class="progress-requests">0 次请求</span><span class="progress-tokens">0 token</span></div>
              <div class="form-actions"><button class="danger-button" type="button" data-action="audit-cancel"><span class="button-icon">${icon("stop")}</span><span class="button-label">取消检测</span></button></div>
            </div>
          </div>
        </article>
        <article class="card reports-card">
          <div class="section-heading"><div><h2>最近审计报告</h2><p>点击报告可查看四条证据轴、原因和限制。</p></div></div>
          <div class="report-list"></div>
          <div class="empty-state report-empty"><div><strong>暂无审计报告</strong><span>保存端点后选择档位，再手动确认开始。</span></div></div>
        </article>
      </div>
    </div>
  </section>`;
}

function baselinesPage(): string {
  return `<section class="page" data-page-panel="baselines" aria-labelledby="baselines-title" hidden>
    <div class="section-heading"><div><h2 id="baselines-title">参考资料</h2><p>实时官方配对是中/高置信参考；Release 内置社区分布只做低置信、跨协议的实验相对排名。</p></div></div>
    <div class="baseline-grid">
      <article class="card card-pad">
        <div class="section-heading"><div><h2>可用参考</h2><p>内置社区分布与用户导入摘要会明确区分，不会冒充实时官方参考。</p></div></div>
        <div class="baseline-list"></div>
        <div class="empty-state baseline-empty"><div><strong>还没有参考资料</strong><span>这不会阻止自洽检查；中/高置信模型身份比较需要在审计页选择实时官方端点。</span></div></div>
      </article>
      <aside class="stack">
        <article class="card card-pad">
          <div class="section-heading"><div><h2>导入参考摘要</h2><p>当前测试版只校验 JSON 结构和大小；用户导入默认标记为“未验证签名”。</p></div></div>
          <div class="form-grid"><label class="field"><span>摘要文件</span><input id="baseline-file" type="file" accept="application/json,.json" /></label><div class="privacy-callout"><strong>不参与评分：</strong>导入项只是本地元数据，不含 scorer 可用样本，也不会让模型身份轴变绿。</div><div class="form-actions"><button class="secondary-button" type="button" data-action="baseline-import">校验格式并导入元数据</button></div></div>
        </article>
        <article class="card card-pad">
          <div class="section-heading"><div><h2>参考证据边界</h2></div></div>
          <ul class="reason-list"><li>只有同次运行的实时官方配对能进入中/高置信统计比较。</li><li>内置社区参考协议不匹配，只显示低置信实验排名，不改变总裁决。</li><li>导入摘要和中转响应都不能污染内置或实时官方参考。</li></ul>
        </article>
      </aside>
    </div>
  </section>`;
}

function principlesPage(): string {
  return `<section class="page" data-page-panel="principles" aria-labelledby="principles-title" hidden>
    <article class="card principle-hero">
      <div><span class="eyebrow">最重要的口径</span><h2 id="principles-title">小狸找异常，不伪造“实测模型”</h2><p>请求配置、显式服务器重路由、连接来源和行为统计是四类不同证据。没有可验证的上游签名或证明时，黑盒软件无法密码学证明物理模型。</p></div>
      <div class="evidence-layers">
        <div class="evidence-layer"><strong>1</strong><div><strong>请求证据</strong><span>当前回合请求的 model / effort</span></div></div>
        <div class="evidence-layer"><strong>2</strong><div><strong>路由证据</strong><span>仅显式 model/rerouted 可称为服务器重路由</span></div></div>
        <div class="evidence-layer"><strong>3</strong><div><strong>来源证据</strong><span>provider、endpoint 分类与 auth mode</span></div></div>
        <div class="evidence-layer"><strong>4</strong><div><strong>行为证据</strong><span>token、时序、质量和分布差异</span></div></div>
      </div>
    </article>
    <div class="status-guide-grid">
      ${statusGuide("green", "绿色 · 该轴未见异常", "协议/usage 自洽可独立为绿；模型身份只有实时官方配对后才可显示与参考一致，仍非物理模型证明。")}
      ${statusGuide("yellow", "黄色 · 疑似异常", "可能是过量计数、实时配对下的行为降质、指纹偏离，或主动审计正常但同一中转的真实会话长期异常。这些都是保守警告，不是物理模型证明。")}
      ${statusGuide("red", "红色 · 明确契约异常", "可复现的 usage 不可能算术、协议包络/SSE 或自报型号契约矛盾。")}
      ${statusGuide("gray", "灰色 · 证据不足", "没有实时匹配参考、样本不足、参数不可控或当前协议无法检查；灰色不是通过。")}
    </div>
    <div class="method-grid">
      <article class="card method-card"><h3>如何寻找 token 注水</h3><ul><li>先验证 usage 各项算术是否自洽。</li><li>对已知 OpenAI tokenizer 计算可见 token，未知别名不宣称绝对精确。</li><li>多个输入量级对比官方配对或可解释区间。</li></ul></article>
      <article class="card method-card"><h3>如何寻找降质</h3><ul><li>使用结构化 JSON、长上下文 nonce、算术/约束推理、多语言、工具选择与状态保持六个域。</li><li>工具域通过三种协议发送真实工具 schema，只在本地评分结构化工具名/参数，绝不执行调用。</li><li>状态保持只验证同一请求内的多消息历史，不代表跨网络会话或物理模型证明；实时官方配对下至少两个域持续偏离才发出黄色提示。</li></ul></article>
      <article class="card method-card"><h3>如何寻找选择性服务</h3><ul><li>仅当一次主动审计与匹配参考一致时，才对比同一本地中转 profile 绑定的最近 30 天真实 Codex 完成回合。</li><li>至少 10 个回合，且至少 5 个、占比不低于一半仍有保守的降质警告，才显示“疑似选择性服务”。</li><li>该评估始终独立于四条证据轴，不改写审计结论，也不识别物理模型。</li></ul></article>
      <article class="card method-card"><h3>如何提高规避成本</h3><ul><li>每次本地随机化参数、改写、语言和顺序。</li><li>交错发送官方与中转请求，比较原题与改写题的差异。</li><li>检查响应缓存导致的分布和延迟方差塌缩。</li></ul></article>
      <article class="card method-card"><h3>仍然无法排除什么</h3><ul><li>中转可能通过 TLS、流量形态或题型识别审计。</li><li>能识别所有审计流量的服务可选择性转发真实模型。</li><li>因此所有报告都保留“无法密码学证明”限制。</li></ul></article>
    </div>
  </section>`;
}

function statusGuide(level: StatusLevel, title: string, copy: string): string {
  return `<article class="status-guide-card status-${level}"><h3><span class="status-dot"></span>${title}</h3><p>${copy}</p></article>`;
}

function conversationDialog(): string {
  return `<dialog class="dialog" id="conversation-dialog" aria-labelledby="conversation-dialog-title"><div class="dialog-header"><div><span class="eyebrow">只读指标</span><h2 id="conversation-dialog-title">会话证据详情</h2></div><button class="icon-button" type="button" data-action="dialog-close" aria-label="关闭会话详情"><span class="button-icon">${icon("close")}</span></button></div><div class="dialog-body conversation-dialog-body"></div></dialog>`;
}

function reportDialog(): string {
  return `<dialog class="dialog" id="report-dialog" aria-labelledby="report-dialog-title"><div class="dialog-header"><div><span class="eyebrow">本地审计报告</span><h2 id="report-dialog-title">四条证据轴</h2></div><button class="icon-button" type="button" data-action="dialog-close" aria-label="关闭审计报告"><span class="button-icon">${icon("close")}</span></button></div><div class="dialog-body report-dialog-body"></div></dialog>`;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function pick(record: Record<string, unknown>, ...keys: string[]): unknown {
  for (const key of keys) if (record[key] !== undefined && record[key] !== null) return record[key];
  return undefined;
}

function firstDefined(...values: unknown[]): unknown {
  for (const value of values) if (value !== undefined && value !== null) return value;
  return undefined;
}

function cleanText(value: unknown, maxLength = 220): string | undefined {
  if (typeof value !== "string") return undefined;
  const clean = value.replace(/[\u202A-\u202E\u2066-\u2069]/g, "").replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F]/g, " ").trim();
  if (!clean) return undefined;
  return clean.length > maxLength ? `${clean.slice(0, maxLength - 1)}…` : clean;
}

function numberValue(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }
  return undefined;
}

function booleanValue(value: unknown, fallback = false): boolean {
  if (typeof value === "boolean") return value;
  if (value === 1 || value === "true") return true;
  if (value === 0 || value === "false") return false;
  return fallback;
}

function stringList(value: unknown, limit = 20): string[] {
  return Array.isArray(value) ? value.slice(0, limit).map((item) => cleanText(item)).filter((item): item is string => Boolean(item)) : [];
}

function listFromEnvelope(value: unknown, ...keys: string[]): unknown[] {
  if (Array.isArray(value)) return value;
  const raw = asRecord(value);
  for (const key of keys) if (Array.isArray(raw[key])) return raw[key] as unknown[];
  if (Array.isArray(raw.items)) return raw.items;
  return [];
}

function normalizeLevel(value: unknown, fallback: StatusLevel = "gray"): StatusLevel {
  const normalized = cleanText(value)?.toLowerCase();
  if (!normalized) return fallback;
  if (["green", "ok", "normal", "healthy", "consistent", "compatible"].includes(normalized)) return "green";
  if (["red", "error", "failed", "critical", "mismatch", "confirmedcontractmismatch"].includes(normalized)) return "red";
  if (["yellow", "warning", "pending", "suspectedpadding", "suspecteddegradation", "significantlydifferent"].includes(normalized)) return "yellow";
  if (["gray", "grey", "idle", "unknown", "notrun", "insufficientevidence", "cancelled"].includes(normalized)) return "gray";
  return fallback;
}

function normalizeOrigin(value: unknown): ConnectionOriginSnapshot {
  const raw = asRecord(value);
  const kindValue = cleanText(pick(raw, "kind", "originKind", "origin_kind"));
  const allowedKinds: OriginKind[] = ["officialChatGpt", "officialOpenAiApi", "officialAnthropicApi", "managedProvider", "customEndpoint", "localEndpoint", "unknown"];
  const kind: OriginKind = allowedKinds.includes(kindValue as OriginKind) ? kindValue as OriginKind : "unknown";
  const authValue = cleanText(pick(raw, "authMode", "auth_mode"));
  const authMode: AuthMode = authValue === "chatGpt" || authValue === "apiKey" || authValue === "external" ? authValue : "unknown";
  const confidenceValue = cleanText(raw.confidence);
  const confidence: OriginConfidence = confidenceValue === "configured" || confidenceValue === "partial" ? confidenceValue : "unknown";
  return {
    kind,
    authMode,
    confidence,
    providerId: cleanText(pick(raw, "providerId", "provider_id"), 100),
    endpointClass: cleanText(pick(raw, "endpointClass", "endpoint_class"), 100),
    evidence: stringList(raw.evidence, 12),
    limitations: stringList(raw.limitations, 12),
  };
}

function normalizeAxis(value: unknown, fallbackSummary = "尚未运行主动检测"): AxisFinding {
  const raw = asRecord(value);
  const stateValue = cleanText(pick(raw, "state", "status", "verdict")) ?? "notRun";
  const normalizedState = stateValue.replace(/[-_\s]/g, "").toLowerCase();
  const known: Record<string, { level: StatusLevel; summary: string }> = {
    normal: { level: "green", summary: "本次协议结构与基础契约正常" },
    abnormal: { level: "red", summary: "发现可复现的协议或自报型号矛盾" },
    unabletocheck: { level: "gray", summary: "当前证据无法完成此项检查" },
    consistent: { level: "green", summary: "本次可用检查范围内未见异常" },
    usagemissing: { level: "gray", summary: "上游未返回足够的 usage 证据" },
    suspectedovercount: { level: "yellow", summary: "多个受控输入量级出现疑似过量计数" },
    contractcontradiction: { level: "red", summary: "usage 存在不可能成立的算术矛盾" },
    insufficientevidence: { level: "gray", summary: "样本或匹配参考不足" },
    learning: { level: "gray", summary: "正在积累可比较样本" },
    suspecteddegradation: { level: "yellow", summary: "至少两个独立能力域持续低于匹配参考" },
    significantlydifferent: { level: "yellow", summary: "行为分布与匹配参考显著不同" },
    selfreportedonly: { level: "gray", summary: "只有 API 自报型号，未获得行为身份参考" },
    referenceconsistent: { level: "green", summary: "本次参数下与配对参考行为一致" },
    referencedifferent: { level: "yellow", summary: "本次参数下与配对参考行为显著不同" },
    experimentalclosertoreference: { level: "yellow", summary: "实验性统计更接近另一组参考样本" },
    unproven: { level: "gray", summary: "真实物理模型未获证明" },
    notrun: { level: "gray", summary: fallbackSummary },
  };
  const inferred = known[normalizedState] ?? { level: normalizeLevel(stateValue, "gray"), summary: fallbackSummary };
  const details = [
    ...stringList(raw.reasons, 12),
    ...stringList(raw.limitations, 8).map((item) => `限制：${item}`),
    ...stringList(pick(raw, "failedDomains", "failed_domains"), 8).map((item) => `异常能力域：${item}`),
  ];
  return {
    level: normalizeLevel(raw.level, inferred.level),
    state: stateValue,
    summary: cleanText(pick(raw, "summary", "explanation", "label"), 260) ?? inferred.summary,
    details: details.slice(0, 20),
  };
}

function normalizeProtocol(value: unknown): RelayProtocol {
  const normalized = cleanText(value)?.replace(/[-_\s]/g, "").toLowerCase();
  if (normalized === "openaichatcompletions" || normalized === "chatcompletions") return "openAiChatCompletions";
  if (normalized === "anthropicmessages" || normalized === "messages") return "anthropicMessages";
  return "openAiResponses";
}

function normalizeOverviewConversation(value: unknown, index: number): OverviewConversation {
  const raw = asRecord(value);
  const activeRequest = asRecord(pick(raw, "activeRequest", "active_request"));
  const usage = asRecord(raw.usage);
  const cumulative = asRecord(usage.cumulative);
  const status = asRecord(raw.status);
  const threadId = cleanText(pick(raw, "threadId", "thread_id"), 140) ?? `unknown-${index + 1}`;
  const total = numberValue(firstDefined(
    pick(raw, "totalTokens", "total_tokens"),
    pick(cumulative, "totalTokens", "total_tokens"),
    pick(usage, "totalTokens", "total_tokens"),
  ));
  const input = numberValue(firstDefined(pick(cumulative, "inputTokens", "input_tokens"), pick(usage, "inputTokens", "input_tokens")));
  const cached = numberValue(firstDefined(pick(cumulative, "cachedInputTokens", "cached_input_tokens"), pick(usage, "cachedInputTokens", "cached_input_tokens")));
  const cacheShare = numberValue(firstDefined(pick(raw, "cacheInputShare", "cache_input_share"), pick(usage, "cacheInputShare", "cache_input_share"))) ?? (input && cached !== undefined ? cached / input : undefined);
  return {
    threadId,
    turnId: cleanText(pick(raw, "turnId", "turn_id"), 140),
    displayName: cleanText(pick(raw, "displayName", "display_name", "displayLabel", "display_label", "alias", "title"), 100) ?? `会话 ${shortId(threadId)}`,
    model: cleanText(firstDefined(raw.model, pick(raw, "requestedModel", "requested_model"), activeRequest.model), 120),
    effort: cleanText(firstDefined(raw.effort, pick(raw, "requestedEffort", "requested_effort"), activeRequest.effort), 40),
    origin: normalizeOrigin(pick(raw, "connectionOrigin", "connection_origin", "origin")),
    statusLevel: normalizeLevel(firstDefined(pick(raw, "statusLevel", "status_level"), status.level), "gray"),
    statusText: cleanText(firstDefined(pick(raw, "statusText", "status_text"), status.explanation, status.code), 160) ?? "证据不足",
    totalTokens: total,
    cacheInputShare: cacheShare,
    sourceTimestamp: cleanText(pick(raw, "sourceTimestamp", "source_timestamp", "updatedAt", "updated_at"), 80),
    childCount: Math.max(0, Math.trunc(numberValue(pick(raw, "childCount", "child_count")) ?? 0)),
  };
}

function normalizeOverview(value: unknown): WorkbenchOverview {
  const envelope = asRecord(value);
  const outer = asRecord(envelope.overview ?? envelope.snapshot ?? value);
  const collector = asRecord(pick(outer, "collectorHealth", "collector_health"));
  const rawConversations = listFromEnvelope(pick(outer, "activeConversations", "active_conversations", "conversations"), "conversations");
  const conversations = rawConversations.map(normalizeOverviewConversation);
  const counts = asRecord(pick(outer, "connectionCounts", "connection_counts"));
  const officialComputed = conversations.filter((item) => isOfficialOrigin(item.origin.kind)).length;
  const customComputed = conversations.filter((item) => isCustomOrigin(item.origin.kind)).length;
  const unknownComputed = conversations.filter((item) => item.origin.kind === "unknown").length;
  const totalTokens = numberValue(pick(outer, "totalTokens", "total_tokens")) ?? sumDefined(conversations.map((item) => item.totalTokens));
  const inputCachePairs = conversations.filter((item) => item.totalTokens !== undefined && item.cacheInputShare !== undefined);
  const cacheInputShare = numberValue(pick(outer, "cacheInputShare", "cache_input_share")) ?? weightedShare(inputCachePairs);
  const axes = asRecord(pick(outer, "axisSummary", "axis_summary"));
  return {
    checkedAt: cleanText(pick(outer, "checkedAt", "checked_at"), 80) ?? new Date().toISOString(),
    collectorLevel: normalizeLevel(firstDefined(pick(outer, "collectorLevel", "collector_level"), collector.level), conversations.length ? "green" : "gray"),
    activeConversationCount: Math.max(0, Math.trunc(numberValue(pick(outer, "activeConversationCount", "active_conversation_count")) ?? conversations.length)),
    officialCount: Math.max(0, Math.trunc(numberValue(pick(counts, "official", "officialCount", "official_count")) ?? officialComputed)),
    customCount: Math.max(0, Math.trunc(numberValue(pick(counts, "custom", "customCount", "custom_count")) ?? customComputed)),
    unknownCount: Math.max(0, Math.trunc(numberValue(pick(counts, "unknown", "unknownCount", "unknown_count")) ?? unknownComputed)),
    totalTokens,
    cacheInputShare,
    dominantOrigin: normalizeOrigin(pick(outer, "dominantOrigin", "dominant_origin", "connectionOrigin", "connection_origin") ?? conversations[0]?.origin),
    axisSummary: {
      protocol: normalizeAxis(pick(axes, "protocol", "protocolCompatibility")),
      usage: normalizeAxis(pick(axes, "usage", "usageConsistency")),
      quality: normalizeAxis(pick(axes, "quality", "behaviorQuality")),
      identity: normalizeAxis(pick(axes, "identity", "modelIdentity")),
    },
    conversations,
    recentAlerts: stringList(pick(outer, "recentAlerts", "recent_alerts", "alerts"), 12),
  };
}

function normalizeHistoryEntry(value: unknown, index: number): HistoryEntry {
  const raw = asRecord(value);
  const overview = normalizeOverviewConversation(raw, index);
  const timing = asRecord(raw.timing);
  const usage = asRecord(raw.usage);
  const last = asRecord(usage.last);
  const route = asRecord(pick(raw, "serverRoute", "server_route"));
  return {
    ...overview,
    id: cleanText(raw.id, 150) ?? `${overview.threadId}:${overview.turnId ?? index}`,
    localAlias: cleanText(pick(raw, "localAlias", "local_alias"), 80),
    startedAt: cleanText(pick(raw, "startedAt", "started_at"), 80),
    completedAt: cleanText(pick(raw, "completedAt", "completed_at"), 80),
    ttftMs: numberValue(firstDefined(pick(raw, "ttftMs", "ttft_ms"), pick(timing, "ttftMs", "ttft_ms"))),
    outputRate: numberValue(firstDefined(pick(raw, "outputRate", "output_rate"), pick(timing, "endToEndOutputRate", "end_to_end_output_rate"))),
    reasoningTokens: numberValue(firstDefined(
      pick(raw, "reasoningTokens", "reasoning_tokens"),
      pick(last, "reasoningOutputTokens", "reasoning_output_tokens"),
      pick(usage, "reasoningOutputTokens", "reasoning_output_tokens"),
    )),
    routeEvidence: cleanText(firstDefined(pick(raw, "routeEvidence", "route_evidence"), route.evidence), 120) ?? "notObserved",
  };
}

function normalizeProfile(value: unknown, index: number): RelayProfile {
  const raw = asRecord(value);
  const now = new Date().toISOString();
  const privateProbePack = normalizePrivateProbePack(pick(raw, "privateProbePack", "private_probe_pack"));
  return {
    id: cleanText(raw.id, 120) ?? `profile-${index + 1}`,
    label: cleanText(raw.label, 80) ?? `端点 ${index + 1}`,
    normalizedBaseUrl: cleanText(pick(raw, "normalizedBaseUrl", "normalized_base_url", "baseUrl", "base_url"), 260) ?? "",
    protocol: normalizeProtocol(raw.protocol),
    defaultModel: cleanText(pick(raw, "defaultModel", "default_model", "model"), 120) ?? "",
    credentialRef: cleanText(pick(raw, "credentialRef", "credential_ref"), 160),
    privateProbePack,
    createdAt: cleanText(pick(raw, "createdAt", "created_at"), 80) ?? now,
    updatedAt: cleanText(pick(raw, "updatedAt", "updated_at"), 80) ?? now,
  };
}

function normalizePrivateProbePack(value: unknown): PrivateProbePackReference | undefined {
  const raw = asRecord(value);
  const path = cleanText(raw.path, 4_096);
  if (!path) return undefined;
  return {
    path,
    version: cleanText(raw.version, 64) ?? "",
    sha256: cleanText(raw.sha256, 64) ?? "",
  };
}

function normalizeVerdict(value: unknown): AuditVerdict {
  const normalized = cleanText(value);
  const allowed: AuditVerdict[] = ["consistent", "insufficientEvidence", "suspectedPadding", "suspectedDegradation", "significantlyDifferent", "confirmedContractMismatch", "failed", "cancelled"];
  return allowed.includes(normalized as AuditVerdict) ? normalized as AuditVerdict : "insufficientEvidence";
}

function normalizeSelectiveServiceAssessment(value: unknown): SelectiveServiceAssessment | undefined {
  const raw = asRecord(value);
  const rawState = cleanText(raw.state)?.replace(/[-_\s]/g, "").toLowerCase();
  const states: Record<string, SelectiveServiceState> = {
    notapplicable: "notApplicable",
    insufficientevidence: "insufficientEvidence",
    nomismatchobserved: "noMismatchObserved",
    suspectedselectiveservice: "suspectedSelectiveService",
  };
  const assessmentState = rawState ? states[rawState] : undefined;
  if (!assessmentState) return undefined;
  return {
    state: assessmentState,
    sampleCount: Math.max(0, Math.trunc(numberValue(pick(raw, "sampleCount", "sample_count")) ?? 0)),
    suspiciousCount: Math.max(0, Math.trunc(numberValue(pick(raw, "suspiciousCount", "suspicious_count")) ?? 0)),
    suspiciousShare: numberValue(pick(raw, "suspiciousShare", "suspicious_share")),
    windowDays: Math.max(0, Math.trunc(numberValue(pick(raw, "windowDays", "window_days")) ?? 30)),
    reasons: stringList(raw.reasons, 12),
    limitations: stringList(raw.limitations, 12),
  };
}

function normalizeReport(value: unknown, index: number): RelayAuditReport {
  const raw = asRecord(value);
  const profile = asRecord(raw.profile);
  const verdict = normalizeVerdict(pick(raw, "overallVerdict", "overall_verdict", "verdict"));
  return {
    auditId: cleanText(pick(raw, "auditId", "audit_id", "id"), 140) ?? `audit-${index + 1}`,
    profileId: cleanText(firstDefined(pick(raw, "profileId", "profile_id"), profile.id), 120),
    profileLabel: cleanText(firstDefined(pick(raw, "profileLabel", "profile_label"), profile.label), 100) ?? "未命名端点",
    claimedModel: cleanText(pick(raw, "claimedModel", "claimed_model", "model"), 120) ?? "未知模型",
    protocol: normalizeProtocol(firstDefined(raw.protocol, profile.protocol)),
    startedAt: cleanText(pick(raw, "startedAt", "started_at"), 80),
    completedAt: cleanText(pick(raw, "completedAt", "completed_at"), 80),
    overallVerdict: verdict,
    confidence: cleanText(raw.confidence, 80) ?? "未评估",
    protocolFindings: normalizeAxis(pick(raw, "protocolFindings", "protocol_findings")),
    usageReconciliation: normalizeAxis(pick(raw, "usageReconciliation", "usage_reconciliation")),
    qualityFindings: normalizeAxis(pick(raw, "qualityFindings", "quality_findings")),
    fingerprintFindings: normalizeAxis(pick(raw, "fingerprintFindings", "fingerprint_findings")),
    reasons: stringList(raw.reasons, 20),
    limitations: stringList(raw.limitations, 20),
    quantitativeEvidence: normalizeReportMetrics(raw),
    selectiveServiceAssessment: normalizeSelectiveServiceAssessment(pick(raw, "selectiveServiceAssessment", "selective_service_assessment")),
  };
}

function normalizeReportMetrics(raw: Record<string, unknown>): Array<{ label: string; value: string }> {
  const metrics: Array<{ label: string; value: string }> = [];
  const parameters = asRecord(raw.parameters);
  const privateProbePack = normalizePrivateProbePack(pick(parameters, "privateProbePack", "private_probe_pack"));
  if (privateProbePack) {
    metrics.push({
      label: "私有 probe pack",
      value: `${privateProbePack.version || "版本未知"} · SHA-256 ${privateProbePack.sha256.slice(0, 8) || "待校验"}`,
    });
  }
  const usage = asRecord(pick(raw, "usageReconciliation", "usage_reconciliation"));
  const factors = Array.isArray(usage.factors) ? usage.factors.slice(0, 8) : [];
  factors.forEach((factorValue, index) => {
    const factor = asRecord(factorValue);
    const inputSize = numberValue(pick(factor, "inputSize", "input_size"));
    const relayInterval = formatConfidenceInterval(pick(factor, "relayInterval", "relay_interval"));
    const referenceInterval = formatConfidenceInterval(pick(factor, "referenceInterval", "reference_interval"));
    const tolerance = numberValue(pick(factor, "toleranceTokens", "tolerance_tokens"));
    const suspicious = booleanValue(factor.suspicious);
    const parts = [
      relayInterval ? `被测端点 CI ${relayInterval}` : "",
      referenceInterval ? `官方参考 CI ${referenceInterval}` : "",
      tolerance !== undefined ? `容差 ${formatEvidenceNumber(tolerance)} token` : "",
      suspicious ? "超过门槛" : "未超过门槛",
    ].filter(Boolean);
    if (parts.length) metrics.push({ label: inputSize === undefined ? `usage 因子 ${index + 1}` : `usage 输入规模 ${formatTokens(inputSize)}`, value: parts.join(" · ") });
  });

  const quality = asRecord(pick(raw, "qualityFindings", "quality_findings"));
  const qualitySamplesRaw = pick(quality, "baselineSampleCount", "baseline_sample_count");
  if (qualitySamplesRaw !== undefined) {
    metrics.push({ label: "质量参考有效 case", value: String(Math.max(0, Math.trunc(numberValue(qualitySamplesRaw) ?? 0))) });
  }
  const failedDomains = stringList(pick(quality, "failedDomains", "failed_domains"), 12);
  if (failedDomains.length) metrics.push({ label: "持续失败能力域", value: failedDomains.join("、") });
  const domainEvidence = firstDefined(
    pick(quality, "domainEvidence", "domain_evidence"),
    pick(quality, "domainFindings", "domain_findings"),
    quality.factors,
  );
  if (Array.isArray(domainEvidence)) {
    domainEvidence.slice(0, 12).forEach((entryValue, index) => {
      const entry = asRecord(entryValue);
      const domain = cleanText(pick(entry, "domain", "code"), 80) ?? `能力域 ${index + 1}`;
      const pairedSamples = numberValue(pick(entry, "pairedSamples", "paired_samples"));
      const relayPasses = numberValue(pick(entry, "relayPasses", "relay_passes"));
      const referencePasses = numberValue(pick(entry, "referencePasses", "reference_passes"));
      const relayRate = numberValue(pick(entry, "relayPassRate", "relay_pass_rate", "observed", "observedRate", "observed_rate"))
        ?? (pairedSamples && relayPasses !== undefined ? relayPasses / pairedSamples : undefined);
      const referenceRate = numberValue(pick(entry, "referencePassRate", "reference_pass_rate", "baseline", "baselineRate", "baseline_rate"))
        ?? (pairedSamples && referencePasses !== undefined ? referencePasses / pairedSamples : undefined);
      const relayInterval = formatConfidenceInterval(pick(entry, "relayInterval", "relay_interval", "observedInterval", "observed_interval"));
      const referenceInterval = formatConfidenceInterval(pick(entry, "referenceInterval", "reference_interval", "baselineInterval", "baseline_interval"));
      const gapInterval = formatConfidenceInterval(pick(entry, "pairedGapInterval", "paired_gap_interval"));
      const tolerance = numberValue(entry.tolerance);
      const batches = numberValue(pick(entry, "batches", "batchCount", "batch_count"));
      const batchId = cleanText(pick(entry, "batchId", "batch_id"), 80);
      const suspicious = booleanValue(entry.suspicious);
      const parts = [
        relayRate !== undefined ? `被测 ${formatPercent(relayRate)}` : "",
        referenceRate !== undefined ? `参考 ${formatPercent(referenceRate)}` : "",
        relayInterval ? `被测 CI ${relayInterval}` : "",
        referenceInterval ? `参考 CI ${referenceInterval}` : "",
        gapInterval ? `99% 通过率差 CI ${gapInterval}` : "",
        tolerance !== undefined ? `容差 ${formatPercent(tolerance)}` : "",
        pairedSamples !== undefined ? `${Math.trunc(pairedSamples)} 对样本` : "",
        batches !== undefined ? `${Math.trunc(batches)} 批` : "",
        batchId ? batchId.replace(/^quality-batch-/, "批次 ") : "",
        suspicious ? "超过保守门槛" : "未超过门槛",
      ].filter(Boolean);
      if (parts.length) metrics.push({ label: `质量域·${qualityDomainText(domain)}`, value: parts.join(" · ") });
    });
  }

  const identity = asRecord(pick(raw, "fingerprintFindings", "fingerprint_findings"));
  const eligibleCellsRaw = pick(identity, "eligibleCells", "eligible_cells");
  if (eligibleCellsRaw !== undefined) {
    metrics.push({ label: "可比较指纹 cell", value: String(Math.max(0, Math.trunc(numberValue(eligibleCellsRaw) ?? 0))) });
  }
  const meanJsd = numberValue(pick(identity, "meanJsDivergence", "mean_js_divergence", "meanJsd"));
  if (meanJsd !== undefined) metrics.push({ label: "平均 base-2 JSD", value: formatEvidenceNumber(meanJsd, 4) });
  const comparedReference = cleanText(pick(identity, "comparedReference", "compared_reference"), 160);
  if (comparedReference) metrics.push({ label: "行为比较参考", value: comparedReference });

  const mmd = asRecord(firstDefined(
    pick(identity, "stringKernelMmd", "string_kernel_mmd", "mmd", "mmdResult", "mmd_result"),
    pick(raw, "stringKernelMmd", "string_kernel_mmd", "mmd", "mmdResult", "mmd_result"),
  ));
  const statistic = numberValue(firstDefined(mmd.statistic, pick(identity, "mmdStatistic", "mmd_statistic"), pick(raw, "mmdStatistic", "mmd_statistic")));
  const pValue = numberValue(firstDefined(pick(mmd, "pValue", "p_value"), pick(identity, "mmdPValue", "mmd_p_value"), pick(raw, "mmdPValue", "mmd_p_value")));
  if (statistic !== undefined || pValue !== undefined) {
    const permutations = numberValue(firstDefined(mmd.permutations, pick(identity, "mmdPermutations", "mmd_permutations"), pick(raw, "mmdPermutations", "mmd_permutations")));
    const observedSamples = numberValue(firstDefined(pick(mmd, "observedSamples", "observed_samples"), pick(identity, "mmdObservedSamples", "mmd_observed_samples"), pick(raw, "mmdObservedSamples", "mmd_observed_samples")));
    const referenceSamples = numberValue(firstDefined(pick(mmd, "referenceSamples", "reference_samples"), pick(identity, "mmdReferenceSamples", "mmd_reference_samples"), pick(raw, "mmdReferenceSamples", "mmd_reference_samples")));
    metrics.push({
      label: "配对 MMD",
      value: [
        statistic !== undefined ? `效应量 ${formatEvidenceNumber(statistic, 4)}` : "",
        pValue !== undefined ? `p=${formatEvidenceNumber(pValue, 4)}` : "",
        permutations !== undefined ? `${Math.trunc(permutations)} 次置换` : "",
        observedSamples !== undefined && referenceSamples !== undefined ? `${Math.trunc(observedSamples)} + ${Math.trunc(referenceSamples)} 样本` : "",
      ].filter(Boolean).join(" · "),
    });
  }

  const paired = asRecord(pick(raw, "pairedBaseline", "paired_baseline"));
  const completedCases = numberValue(pick(paired, "completedCases", "completed_cases"));
  if (completedCases !== undefined) {
    const model = cleanText(paired.model, 120) ?? "模型未知";
    metrics.push({ label: "实时官方配对", value: `${model} · ${Math.trunc(completedCases)} 个完成 case` });
  }

  const community = asRecord(pick(raw, "communityBaseline", "community_baseline"));
  const communityState = cleanText(community.state, 80);
  if (communityState) {
    const closest = cleanText(pick(community, "closestModel", "closest_model"), 120);
    const runnerUp = cleanText(pick(community, "runnerUpModel", "runner_up_model"), 120);
    const improvement = numberValue(pick(community, "relativeDistanceImprovement", "relative_distance_improvement"));
    const stateText = communityState === "experimentalRelativeRanking"
      ? `低置信实验排名${closest ? `：更接近 ${closest}` : ""}`
      : "低置信社区参考：证据不足";
    metrics.push({
      label: "Release 内置社区参考",
      value: [stateText, runnerUp ? `次近 ${runnerUp}` : "", improvement !== undefined ? `相对距离改善 ${formatPercent(improvement)}` : "", "不改变总裁决"].filter(Boolean).join(" · "),
    });
    const comparisons = Array.isArray(community.comparisons) ? community.comparisons.slice(0, 6) : [];
    comparisons.forEach((entryValue) => {
      const entry = asRecord(entryValue);
      const model = cleanText(entry.model, 120) ?? "未知参考";
      const distance = numberValue(pick(entry, "meanJsDivergence", "mean_js_divergence"));
      const cells = numberValue(pick(entry, "eligibleCells", "eligible_cells"));
      const samples = numberValue(pick(entry, "referenceSamples", "reference_samples"));
      metrics.push({
        label: `社区距离·${model}`,
        value: [distance === undefined ? "JSD 不可比" : `JSD ${formatEvidenceNumber(distance, 4)}`, cells === undefined ? "" : `${Math.trunc(cells)} cells`, samples === undefined ? "" : `${Math.trunc(samples)} 参考样本`, "跨协议/低置信"].filter(Boolean).join(" · "),
      });
    });
  }
  return metrics;
}

function qualityDomainText(value: string): string {
  const normalized = value.replace(/[-_\s]/g, "").toLowerCase();
  const labels: Record<string, string> = {
    structuredoutput: "结构化输出",
    toolselection: "工具选择",
    longcontextretrieval: "长上下文检索",
    constraintreasoning: "约束推理",
    stateconsistency: "状态一致性",
    multilingual: "多语言",
  };
  return labels[normalized] ?? value;
}

function formatConfidenceInterval(value: unknown): string | undefined {
  const interval = asRecord(value);
  const lower = numberValue(interval.lower);
  const upper = numberValue(interval.upper);
  if (lower === undefined || upper === undefined) return undefined;
  const confidence = numberValue(interval.confidence);
  const prefix = confidence === undefined ? "" : `${formatEvidenceNumber(confidence * 100, 1)}% `;
  return `${prefix}[${formatEvidenceNumber(lower)}, ${formatEvidenceNumber(upper)}]`;
}

function formatEvidenceNumber(value: number, digits = 2): string {
  if (!Number.isFinite(value)) return "—";
  return value.toFixed(digits).replace(/\.0+$/, "").replace(/(\.\d*?)0+$/, "$1");
}

function normalizeBaseline(value: unknown, index: number): RelayBaseline {
  const raw = asRecord(value);
  const sourceValue = cleanText(raw.source);
  const source: RelayBaseline["source"] = sourceValue === "official" || sourceValue === "user" ? sourceValue : "community";
  return {
    id: cleanText(raw.id, 120) ?? `baseline-${index + 1}`,
    label: cleanText(raw.label, 100) ?? `基线 ${index + 1}`,
    model: cleanText(raw.model, 120) ?? "未知模型",
    protocol: raw.protocol === undefined ? undefined : normalizeProtocol(raw.protocol),
    source,
    version: cleanText(raw.version, 60),
    sampleCount: Math.max(0, Math.trunc(numberValue(pick(raw, "sampleCount", "sample_count")) ?? 0)),
    createdAt: cleanText(pick(raw, "createdAt", "created_at"), 80),
    expiresAt: cleanText(pick(raw, "expiresAt", "expires_at"), 80),
    signed: booleanValue(raw.signed, false),
    builtIn: booleanValue(pick(raw, "builtIn", "built_in"), false),
    referenceProtocol: cleanText(pick(raw, "referenceProtocol", "reference_protocol"), 160),
    scoringMode: cleanText(pick(raw, "scoringMode", "scoring_mode"), 80),
    limitations: stringList(raw.limitations, 12),
  };
}

function normalizeSchedule(value: unknown): AuditSchedule {
  const raw = asRecord(value);
  const cadence = cleanText(raw.cadence) === "daily" ? "daily" : "weekly";
  const retentionRaw = pick(raw, "historyRetentionDays", "history_retention_days");
  return {
    enabled: booleanValue(raw.enabled),
    profileId: cleanText(pick(raw, "profileId", "profile_id"), 128),
    officialBaselineProfileId: cleanText(pick(raw, "officialBaselineProfileId", "official_baseline_profile_id"), 128),
    cadence,
    weekday: Math.min(6, Math.max(0, Math.trunc(numberValue(raw.weekday) ?? 1))),
    localTime: cleanText(pick(raw, "localTime", "local_time"), 10) ?? "20:00",
    pairOfficial: booleanValue(pick(raw, "pairOfficial", "pair_official")),
    monthlyRequestLimit: Math.max(1, Math.trunc(numberValue(pick(raw, "monthlyRequestLimit", "monthly_request_limit")) ?? 1_000)),
    historyRetentionDays: retentionRaw === null || retentionRaw === "forever" ? null : Math.max(1, Math.trunc(numberValue(retentionRaw) ?? 180)),
    nextRunAt: cleanText(pick(raw, "nextRunAt", "next_run_at"), 80),
    lastRunAt: cleanText(pick(raw, "lastRunAt", "last_run_at"), 80),
    lastStatus: cleanText(pick(raw, "lastStatus", "last_status"), 100),
    budgetMonth: cleanText(pick(raw, "budgetMonth", "budget_month"), 16),
    monthlyReservedRequests: Math.max(0, Math.trunc(numberValue(pick(raw, "monthlyReservedRequests", "monthly_reserved_requests")) ?? 0)),
  };
}

function normalizeProgress(value: unknown): RelayAuditProgress | undefined {
  const raw = asRecord(value);
  const auditId = cleanText(pick(raw, "auditId", "audit_id"), 140);
  if (!auditId) return undefined;
  return {
    auditId,
    phase: cleanText(raw.phase, 80) ?? "running",
    completedCases: Math.max(0, Math.trunc(numberValue(pick(raw, "completedCases", "completed_cases")) ?? 0)),
    totalCases: Math.max(0, Math.trunc(numberValue(pick(raw, "totalCases", "total_cases")) ?? 0)),
    usedRequests: Math.max(0, Math.trunc(numberValue(pick(raw, "usedRequests", "used_requests")) ?? 0)),
    tokenEstimate: Math.max(0, Math.trunc(numberValue(pick(raw, "tokenEstimate", "token_estimate")) ?? 0)),
    currentDetector: cleanText(pick(raw, "currentDetector", "current_detector"), 100) ?? "等待调度",
  };
}

function isOfficialOrigin(kind: OriginKind): boolean {
  return kind === "officialChatGpt" || kind === "officialOpenAiApi" || kind === "officialAnthropicApi";
}

function isCustomOrigin(kind: OriginKind): boolean {
  return kind === "customEndpoint" || kind === "localEndpoint" || kind === "managedProvider";
}

function shortId(value: string): string {
  return value.length > 10 ? `${value.slice(0, 8)}…` : value;
}

function sumDefined(values: Array<number | undefined>): number | undefined {
  const defined = values.filter((value): value is number => value !== undefined);
  return defined.length ? defined.reduce((sum, value) => sum + value, 0) : undefined;
}

function weightedShare(items: OverviewConversation[]): number | undefined {
  let weight = 0;
  let weighted = 0;
  for (const item of items) {
    if (item.totalTokens === undefined || item.cacheInputShare === undefined) continue;
    weight += item.totalTokens;
    weighted += item.totalTokens * item.cacheInputShare;
  }
  return weight > 0 ? weighted / weight : undefined;
}

function maxLevel(...levels: StatusLevel[]): StatusLevel {
  return levels.reduce((current, level) => STATUS_ORDER[level] > STATUS_ORDER[current] ? level : current, "gray");
}

function verdictLevel(verdict: AuditVerdict): StatusLevel {
  if (verdict === "consistent") return "green";
  if (verdict === "confirmedContractMismatch" || verdict === "failed") return "red";
  if (verdict === "suspectedPadding" || verdict === "suspectedDegradation" || verdict === "significantlyDifferent") return "yellow";
  return "gray";
}

function reportEffectiveLevel(report: RelayAuditReport): StatusLevel {
  const selectiveLevel = report.selectiveServiceAssessment?.state === "suspectedSelectiveService" ? "yellow" : "gray";
  return maxLevel(verdictLevel(report.overallVerdict), selectiveLevel);
}

function reportHeadline(report: RelayAuditReport): string {
  return report.selectiveServiceAssessment?.state === "suspectedSelectiveService"
    ? `疑似选择性服务（四轴：${VERDICT_TEXT[report.overallVerdict]}）`
    : VERDICT_TEXT[report.overallVerdict];
}

function formatTokens(value: number | undefined): string {
  if (value === undefined || !Number.isFinite(value)) return "—";
  const absolute = Math.abs(value);
  if (absolute >= 1_000_000) return `${(value / 1_000_000).toFixed(absolute >= 10_000_000 ? 1 : 2).replace(/\.0+$/, "")}m`;
  if (absolute >= 1_000) return `${(value / 1_000).toFixed(absolute >= 100_000 ? 0 : 1).replace(/\.0$/, "")}k`;
  return Math.round(value).toLocaleString("zh-CN");
}

function formatPercent(value: number | undefined): string {
  if (value === undefined || !Number.isFinite(value)) return "—";
  const normalized = Math.abs(value) <= 1 ? value * 100 : value;
  return `${normalized.toFixed(normalized >= 10 ? 1 : 2).replace(/\.0+$/, "")}%`;
}

function formatDuration(value: number | undefined): string {
  if (value === undefined || !Number.isFinite(value)) return "—";
  if (value < 1_000) return `${Math.max(0, Math.round(value))}ms`;
  if (value < 60_000) return `${(value / 1_000).toFixed(1).replace(/\.0$/, "")}s`;
  return `${Math.floor(value / 60_000)}m ${Math.round((value % 60_000) / 1_000)}s`;
}

function relativeTime(value: string | undefined): string {
  if (!value) return "时间未知";
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return "时间未知";
  const seconds = Math.max(0, Math.round((Date.now() - timestamp) / 1_000));
  if (seconds < 10) return "刚刚";
  if (seconds < 60) return `${seconds} 秒前`;
  if (seconds < 3_600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3_600)} 小时前`;
  return new Intl.DateTimeFormat("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }).format(timestamp);
}

function expiryText(value: string | undefined): string | undefined {
  if (!value) return undefined;
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return "到期时间未知";
  const deltaMs = timestamp - Date.now();
  const absoluteSeconds = Math.abs(Math.round(deltaMs / 1_000));
  const amount = absoluteSeconds < 60
    ? "不到 1 分钟"
    : absoluteSeconds < 3_600
      ? `${Math.ceil(absoluteSeconds / 60)} 分钟`
      : absoluteSeconds < 86_400
        ? `${Math.ceil(absoluteSeconds / 3_600)} 小时`
        : `${Math.ceil(absoluteSeconds / 86_400)} 天`;
  return deltaMs >= 0 ? `${amount}后到期` : `已过期 ${amount}`;
}

function auditConfidenceText(value: string): string {
  const normalized = value.trim().toLowerCase();
  if (normalized === "high") return "高";
  if (normalized === "medium") return "中";
  if (normalized === "low") return "低";
  return value || "未评估";
}

function endpointForDisplay(value: string): string {
  try {
    const url = new URL(value);
    return `${url.protocol}//${url.host}${url.pathname.replace(/\/$/, "")}`;
  } catch { return cleanText(value, 180) ?? "未设置端点"; }
}

function normalizeEndpoint(value: string): string {
  const url = new URL(value.trim());
  if (url.protocol !== "https:" && url.protocol !== "http:") throw new Error("unsupported_protocol");
  url.username = "";
  url.password = "";
  url.search = "";
  url.hash = "";
  return url.toString().replace(/\/$/, "");
}

function isInsecureRemoteEndpoint(value: string): boolean {
  try {
    const url = new URL(value);
    const local = url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "::1" || url.hostname.endsWith(".localhost");
    return url.protocol === "http:" && !local;
  } catch {
    return false;
  }
}

function confirmInsecureRemoteRequest(profile: RelayProfile): boolean {
  if (!isInsecureRemoteEndpoint(profile.normalizedBaseUrl)) return true;
  return window.confirm(
    `“${profile.label || "该端点"}”使用非本机 HTTP。API Key 和请求可能被明文窃听。\n\n仍要仅对这个原始 origin 发起本次请求吗？`,
  );
}

function friendlyCommandError(error: unknown): string {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  const lowered = message.toLowerCase();
  if (lowered.includes("command") && (lowered.includes("not found") || lowered.includes("unknown"))) return "当前后端版本尚未提供该功能，其他页面仍可继续使用";
  if (lowered.includes("401") || lowered.includes("unauthor") || lowered.includes("api key")) return "认证失败，请检查 API Key 和端点配置";
  if (lowered.includes("403") || lowered.includes("forbidden")) return "端点拒绝了当前凭据或模型访问";
  if (lowered.includes("cannot delete") && lowered.includes("audit") && lowered.includes("active")) return "该端点正被进行中的审计使用；请先取消并等待审计结束";
  if (lowered.includes("timeout") || lowered.includes("timed out")) return "操作超时，已保留上一份有效数据";
  if (lowered.includes("connect") || lowered.includes("network") || lowered.includes("dns")) return "无法连接端点，请检查地址与网络";
  if (lowered.includes("cancel")) return "操作已取消";
  return "操作未完成，已保留现有数据，可稍后重试";
}

async function command<T>(name: string, args?: Record<string, unknown>, quiet = false): Promise<T | undefined> {
  if (!IS_TAURI) return undefined;
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    if (!quiet) showNotice(friendlyCommandError(error), "yellow");
    console.warn(`[XiaoLi workbench] ${name}: ${friendlyCommandError(error)}`);
    return undefined;
  }
}

async function withUiTimeout<T>(promise: Promise<T>, timeoutMs = UI_REFRESH_TIMEOUT_MS): Promise<T> {
  let timeoutId: number | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<T>((_resolve, reject) => {
        timeoutId = window.setTimeout(() => reject(new UiRefreshTimeoutError()), timeoutMs);
      }),
    ]);
  } finally {
    if (timeoutId !== undefined) window.clearTimeout(timeoutId);
  }
}

function invalidatePendingLoads(): void {
  overviewLoadSerial += 1;
  historyLoadSerial += 1;
  profilesLoadSerial += 1;
  auditsLoadSerial += 1;
  baselinesLoadSerial += 1;
  scheduleLoadSerial += 1;
}

function setText(selector: string, value: string, root: ParentNode = mount): void {
  const element = root.querySelector<HTMLElement>(selector);
  if (element) element.textContent = value;
}

function setVisible(element: HTMLElement | null, visible: boolean): void {
  if (element) element.hidden = !visible;
}

function replaceStatusClass(element: HTMLElement | null, level: StatusLevel): void {
  if (!element) return;
  element.classList.remove("status-green", "status-yellow", "status-red", "status-gray");
  element.classList.add(`status-${level}`);
}

function showNotice(message: string, level: StatusLevel = "yellow", timeoutMs = 5_000): void {
  const notice = mount.querySelector<HTMLElement>(".notice");
  if (!notice) return;
  setText(".notice-text", message);
  notice.dataset.level = level;
  notice.hidden = false;
  window.setTimeout(() => {
    if (notice.querySelector(".notice-text")?.textContent === message) notice.hidden = true;
  }, timeoutMs);
}

function applyTheme(): void {
  document.body.classList.toggle("theme-minimal", state.theme === "minimal");
  try { localStorage.setItem("xiaoli-theme", state.theme); }
  catch { /* Rust preferences remain authoritative when available. */ }
}

function render(): void {
  const focusKey = (document.activeElement as HTMLElement | null)?.dataset.focusKey;
  renderChrome();
  renderOverview();
  renderHistory();
  renderProfiles();
  renderAudits();
  renderBaselines();
  renderBudget();
  renderProgress();
  if (focusKey && (document.activeElement as HTMLElement | null)?.dataset.focusKey !== focusKey) {
    Array.from(mount.querySelectorAll<HTMLElement>("[data-focus-key]"))
      .find((element) => element.dataset.focusKey === focusKey)
      ?.focus({ preventScroll: true });
  }
}

function renderChrome(): void {
  const titles: Record<PageId, string> = {
    overview: "实时总览",
    history: "会话历史",
    relay: "中转检测",
    baselines: "参考资料元数据",
    principles: "检测原理与状态",
  };
  setText(".page-title", titles[state.page]);
  for (const button of mount.querySelectorAll<HTMLButtonElement>(".nav-button[data-page]")) {
    const active = button.dataset.page === state.page;
    button.classList.toggle("is-active", active);
    button.setAttribute("aria-current", active ? "page" : "false");
  }
  for (const panel of mount.querySelectorAll<HTMLElement>("[data-page-panel]")) panel.hidden = panel.dataset.pagePanel !== state.page;
  const historyButton = mount.querySelector<HTMLButtonElement>('.nav-button[data-page="history"] .nav-count');
  if (historyButton) historyButton.textContent = String(Math.min(999, state.historyTotal || state.history.length));
  const baselineButton = mount.querySelector<HTMLButtonElement>('.nav-button[data-page="baselines"] .nav-count');
  if (baselineButton) baselineButton.textContent = String(Math.min(999, state.baselines.length));

  const level = maxLevel(state.overview.collectorLevel, ...state.overview.conversations.map((item) => item.statusLevel));
  const avatar = mount.querySelector<HTMLImageElement>(".brand-avatar");
  if (avatar) avatar.src = level === "green" ? mochiGreen : level === "yellow" ? mochiYellow : level === "red" ? mochiRed : mochiGray;
  replaceStatusClass(mount.querySelector(".brand-status-dot"), level);

  const backendStatus = mount.querySelector<HTMLElement>(".sidebar-footer");
  replaceStatusClass(backendStatus?.querySelector(".status-dot") ?? null, state.connected ? state.overview.collectorLevel : "gray");
  setText(".backend-status", state.connected ? `本地采集器·${STATUS_TEXT[state.overview.collectorLevel]}` : MOCK_MODE ? "浏览器预览数据" : "本地后端未连接");

  const origin = state.overview.dominantOrigin;
  const originLevel = originEvidenceLevel(origin);
  const chip = mount.querySelector<HTMLElement>(".connection-chip");
  replaceStatusClass(chip, originLevel);
  replaceStatusClass(chip?.querySelector(".status-dot") ?? null, originLevel);
  setText(".connection-chip-text", ORIGIN_TEXT[origin.kind]);
  if (chip) chip.title = originTooltip(origin);
  applyTheme();
}

function renderOverview(): void {
  const overview = state.overview;
  updateSummary("active", String(overview.activeConversationCount), overview.collectorLevel);
  updateSummary("official", String(overview.officialCount), overview.officialCount > 0 ? "green" : "gray");
  updateSummary("custom", String(overview.customCount), "gray");
  updateSummary("tokens", formatTokens(overview.totalTokens), overview.totalTokens !== undefined ? "green" : "gray");
  const tokensMeta = mount.querySelector<HTMLElement>('[data-summary="tokens"] .summary-meta');
  if (tokensMeta) tokensMeta.textContent = `缓存输入 ${formatPercent(overview.cacheInputShare)}·去重汇总`;

  const origin = overview.dominantOrigin;
  setText(".origin-title", ORIGIN_TEXT[origin.kind]);
  const detail = [origin.providerId ? `provider ${origin.providerId}` : "", authModeText(origin.authMode), confidenceText(origin.confidence)].filter(Boolean).join("·");
  setText(".origin-detail", detail || "小狸不会根据速度或文风猜测来源");
  const originChip = mount.querySelector<HTMLElement>(".origin-strip .status-chip");
  const originLevel = originEvidenceLevel(origin);
  replaceStatusClass(originChip, originLevel);
  if (originChip) {
    originChip.textContent = origin.kind === "unknown" || origin.confidence === "unknown"
      ? "证据不足"
      : origin.confidence === "configured"
        ? "已按配置识别"
        : "部分证据";
    originChip.title = originTooltip(origin);
  }

  updateAxis("protocol", overview.axisSummary.protocol);
  updateAxis("usage", overview.axisSummary.usage);
  updateAxis("quality", overview.axisSummary.quality);
  updateAxis("identity", overview.axisSummary.identity);

  const table = mount.querySelector<HTMLElement>(".overview-table-wrap");
  const empty = mount.querySelector<HTMLElement>(".overview-empty");
  setVisible(table, overview.conversations.length > 0);
  setVisible(empty, overview.conversations.length === 0);
  const body = mount.querySelector<HTMLTableSectionElement>(".overview-conversation-body");
  if (body) body.replaceChildren(...overview.conversations.map(overviewConversationRow));
  const checked = mount.querySelector<HTMLTimeElement>(".overview-checked-at");
  if (checked) {
    checked.textContent = relativeTime(overview.checkedAt);
    checked.dateTime = overview.checkedAt;
    checked.title = `最近更新：${overview.checkedAt}`;
  }

  const alerts = mount.querySelector<HTMLUListElement>(".overview-alerts");
  if (alerts) alerts.replaceChildren(...overview.recentAlerts.map((alert) => {
    const item = document.createElement("li");
    item.textContent = alert;
    return item;
  }));
  setVisible(mount.querySelector(".overview-alerts-empty"), overview.recentAlerts.length === 0);
  setVisible(alerts, overview.recentAlerts.length > 0);
  renderScheduleForm();
}

function originEvidenceLevel(origin: ConnectionOriginSnapshot): StatusLevel {
  if (origin.kind === "unknown" || origin.confidence === "unknown") return "gray";
  return origin.confidence === "partial" ? "yellow" : "green";
}

function updateSummary(key: string, value: string, level: StatusLevel): void {
  const card = mount.querySelector<HTMLElement>(`[data-summary="${key}"]`);
  if (!card) return;
  const valueElement = card.querySelector<HTMLElement>(".summary-value");
  if (valueElement) valueElement.textContent = value;
  replaceStatusClass(card.querySelector(".status-dot"), level);
}

function updateAxis(key: string, finding: AxisFinding): void {
  const card = mount.querySelector<HTMLElement>(`[data-axis="${key}"]`);
  if (!card) return;
  replaceStatusClass(card.querySelector(".status-dot"), finding.level);
  const stateElement = card.querySelector<HTMLElement>(".axis-state");
  replaceStatusClass(stateElement, finding.level);
  if (stateElement) stateElement.textContent = axisStateText(finding);
  const copy = card.querySelector<HTMLParagraphElement>("p");
  if (copy) copy.textContent = finding.summary;
  card.title = [...finding.details, finding.summary].join("\n");
}

function axisStateText(finding: AxisFinding): string {
  if (finding.state === "notRun") return "未检测";
  if (finding.state === "learning") return "学习中";
  return STATUS_TEXT[finding.level];
}

function overviewConversationRow(item: OverviewConversation): HTMLTableRowElement {
  const row = document.createElement("tr");
  row.append(
    tableCell(item.displayName, `${shortId(item.threadId)}${item.childCount ? `·${item.childCount} 个子任务` : ""}`),
    tableCell(`${item.model ?? "未知模型"} · ${item.effort ?? "未知"}`, "本回合请求值"),
    tableCell(originShort(item.origin.kind), confidenceText(item.origin.confidence)),
    tableCell(`${formatTokens(item.totalTokens)} tok`, `缓存输入 ${formatPercent(item.cacheInputShare)}`),
  );
  const statusCell = document.createElement("td");
  statusCell.append(statusChip(item.statusLevel, STATUS_TEXT[item.statusLevel], item.statusText));
  row.append(statusCell);
  row.title = `${item.statusText}\n${originTooltip(item.origin)}`;
  return row;
}

function tableCell(primary: string, secondary?: string): HTMLTableCellElement {
  const cell = document.createElement("td");
  const main = document.createElement("span");
  main.className = "cell-primary";
  main.textContent = primary;
  cell.append(main);
  if (secondary) {
    const meta = document.createElement("span");
    meta.className = "cell-secondary";
    meta.textContent = secondary;
    cell.append(meta);
  }
  return cell;
}

function statusChip(level: StatusLevel, label: string, tooltip?: string): HTMLSpanElement {
  const chip = document.createElement("span");
  chip.className = `status-chip status-${level}`;
  chip.textContent = label;
  if (tooltip) chip.title = tooltip;
  return chip;
}

function renderHistory(): void {
  const body = mount.querySelector<HTMLTableSectionElement>(".history-body");
  if (body) body.replaceChildren(...state.history.map(historyRow));
  setVisible(mount.querySelector(".history-table-wrap"), state.history.length > 0);
  setVisible(mount.querySelector(".history-empty"), state.history.length === 0);
  setText(".history-count", `共 ${state.historyTotal || state.history.length} 条·只读指标`);
  const more = mount.querySelector<HTMLButtonElement>('[data-action="history-more"]');
  if (more) more.hidden = state.history.length === 0 || state.history.length >= state.historyTotal;
}

function historyRow(item: HistoryEntry): HTMLTableRowElement {
  const row = document.createElement("tr");
  row.append(
    tableCell(item.displayName, shortId(item.threadId)),
    tableCell(relativeTime(item.completedAt ?? item.sourceTimestamp), item.completedAt ? "已完成" : "最近事件"),
    tableCell(item.model ?? "未知模型", `${item.effort ?? "未知"}（请求）`),
    tableCell(originShort(item.origin.kind), confidenceText(item.origin.confidence)),
    tableCell(`${formatTokens(item.totalTokens)} tok`, `缓存 ${formatPercent(item.cacheInputShare)}`),
  );
  const actionCell = document.createElement("td");
  const button = document.createElement("button");
  button.type = "button";
  button.className = `text-button status-${item.statusLevel}`;
  button.dataset.action = "history-detail";
  button.dataset.historyId = item.id;
  button.dataset.focusKey = `history-detail:${item.id}`;
  button.textContent = STATUS_TEXT[item.statusLevel];
  button.title = `查看证据详情：${item.statusText}`;
  actionCell.append(button);
  row.append(actionCell);
  return row;
}

function renderProfiles(): void {
  const list = mount.querySelector<HTMLElement>(".profile-list");
  if (list) list.replaceChildren(...state.profiles.map(profileRow));
  setVisible(list, state.profiles.length > 0);
  setVisible(mount.querySelector(".profile-empty"), state.profiles.length === 0);
  renderScheduleProfileOptions();
}

function renderScheduleProfileOptions(preserveDraft = state.scheduleDraftDirty): void {
  const target = mount.querySelector<HTMLSelectElement>("#schedule-profile");
  const official = mount.querySelector<HTMLSelectElement>("#schedule-official-profile");
  if (target) {
    const current = target.value;
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "请先选择端点";
    const options = state.profiles.map((profile) => {
      const option = document.createElement("option");
      option.value = profile.id;
      option.textContent = `${profile.label}·${profile.defaultModel}`;
      return option;
    });
    target.replaceChildren(placeholder, ...options);
    const selected = preserveDraft ? current : state.schedule.profileId;
    if (selected && options.some((option) => option.value === selected)) target.value = selected;
  }
  if (official) {
    const current = official.value;
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "不启用官方配对";
    const options = state.profiles.filter(isOfficialProfile).map((profile) => {
      const option = document.createElement("option");
      option.value = profile.id;
      option.textContent = `${profile.label}·${profile.defaultModel}`;
      return option;
    });
    official.replaceChildren(placeholder, ...options);
    const selected = preserveDraft ? current : state.schedule.officialBaselineProfileId;
    if (selected && options.some((option) => option.value === selected)) {
      official.value = selected;
    }
  }
}

function profileRow(profile: RelayProfile): HTMLElement {
  const row = document.createElement("div");
  row.className = `profile-row${profile.id === state.selectedProfileId ? " is-selected" : ""}`;
  const copy = document.createElement("div");
  const title = document.createElement("span");
  title.className = "row-title";
  title.textContent = `${profile.label}·${profile.defaultModel}`;
  const meta = document.createElement("span");
  meta.className = "row-meta";
  const credentialState = profile.credentialRef
    ? "系统凭据"
    : state.credentials.has(profile.id)
      ? "当前进程内存凭据"
      : "未绑定凭据";
  const probeState = profile.privateProbePack
    ? `·题包 ${profile.privateProbePack.version}#${profile.privateProbePack.sha256.slice(0, 8)}`
    : "";
  meta.textContent = `${PROTOCOL_TEXT[profile.protocol]}·${endpointForDisplay(profile.normalizedBaseUrl)}·${credentialState}${probeState}`;
  copy.append(title, meta);
  const actions = document.createElement("div");
  actions.className = "row-actions";
  actions.append(rowAction("profile-edit", profile.id, "编辑", "edit"), rowAction("profile-delete", profile.id, "删除", "trash", true));
  row.append(copy, actions);
  return row;
}

function rowAction(action: string, id: string, label: string, iconName: Parameters<typeof icon>[0], danger = false): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = danger ? "text-button status-red" : "text-button";
  button.dataset.action = action;
  button.dataset.id = id;
  button.dataset.focusKey = `${action}:${id}`;
  button.innerHTML = `<span class="button-icon">${icon(iconName)}</span><span class="sr-only">${label}</span>`;
  button.title = label;
  button.setAttribute("aria-label", label);
  return button;
}

function renderAudits(): void {
  const list = mount.querySelector<HTMLElement>(".report-list");
  if (list) list.replaceChildren(...state.audits.map(reportRow));
  setVisible(list, state.audits.length > 0);
  setVisible(mount.querySelector(".report-empty"), state.audits.length === 0);
  renderBaselineSelect();
}

function reportRow(report: RelayAuditReport): HTMLElement {
  const level = reportEffectiveLevel(report);
  const row = document.createElement("div");
  row.className = "report-row";
  const marker = document.createElement("span");
  marker.className = `report-level status-${level}`;
  marker.setAttribute("aria-label", STATUS_TEXT[level]);
  const copy = document.createElement("div");
  const title = document.createElement("span");
  title.className = "row-title";
  title.textContent = `${report.profileLabel}·${report.claimedModel}`;
  const meta = document.createElement("span");
  meta.className = "row-meta";
  meta.textContent = `${reportHeadline(report)}·置信度 ${auditConfidenceText(report.confidence)}·${relativeTime(report.completedAt)}`;
  copy.append(title, meta);
  const button = document.createElement("button");
  button.type = "button";
  button.className = "text-button";
  button.dataset.action = "report-detail";
  button.dataset.auditId = report.auditId;
  button.dataset.focusKey = `report-detail:${report.auditId}`;
  button.textContent = "详情";
  const actions = document.createElement("div");
  actions.className = "row-actions";
  actions.append(button, rowAction("report-delete", report.auditId, "删除这份审计报告", "trash", true));
  row.append(marker, copy, actions);
  return row;
}

function renderBaselines(): void {
  const list = mount.querySelector<HTMLElement>(".baseline-list");
  if (list) list.replaceChildren(...state.baselines.map(baselineRow));
  setVisible(list, state.baselines.length > 0);
  setVisible(mount.querySelector(".baseline-empty"), state.baselines.length === 0);
}

function baselineRow(baseline: RelayBaseline): HTMLElement {
  const row = document.createElement("div");
  row.className = "baseline-row";
  const copy = document.createElement("div");
  const title = document.createElement("span");
  title.className = "row-title";
  title.textContent = `${baseline.label}·${baseline.model}`;
  const meta = document.createElement("span");
  meta.className = "row-meta";
  const protocolLabel = baseline.referenceProtocol
    ?? (baseline.protocol ? PROTOCOL_TEXT[baseline.protocol] : "参考协议未标注");
  meta.textContent = `${protocolLabel}·${baseline.sampleCount} 个样本${baseline.expiresAt ? `·${expiryText(baseline.expiresAt)}` : ""}`;
  copy.append(title, meta);
  const source = document.createElement("span");
  source.className = "baseline-source";
  const sourceLabel = baseline.builtIn
    ? "Release 内置社区参考"
    : baseline.source === "official"
      ? "官方摘要"
      : baseline.source === "community"
        ? baseline.signed ? "签名社区摘要" : "未签名社区摘要"
        : "用户导入摘要";
  source.textContent = baseline.builtIn ? `${sourceLabel}·低置信实验排名` : `${sourceLabel}·仅元数据`;
  row.title = baseline.builtIn
    ? "该公开分布随 Release 编译，但请求协议与小狸审计不匹配；它只用于低置信相对排名，不改变证据轴或总裁决。"
    : "导入摘要不交给 scorer；中/高置信模型身份统计只使用本次实时官方配对。";
  row.append(copy, source);
  if (baseline.source === "user" && !baseline.builtIn) row.append(rowAction("baseline-delete", baseline.id, "删除用户参考摘要", "trash", true));
  return row;
}

function renderBaselineSelect(): void {
  const select = mount.querySelector<HTMLSelectElement>("#audit-baseline");
  if (!select) return;
  const current = select.value;
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = "不配对（自洽检查；质量/身份灰色）";
  const selected = state.profiles.find((profile) => profile.id === state.selectedProfileId);
  const options = state.profiles.filter((profile) =>
    profile.id !== state.selectedProfileId
    && isOfficialProfile(profile)
    && (!selected || (profile.protocol === selected.protocol && profile.defaultModel === selected.defaultModel)),
  ).map((profile) => {
    const option = document.createElement("option");
    option.value = profile.id;
    option.textContent = `${profile.label}·${profile.defaultModel}·${PROTOCOL_TEXT[profile.protocol]}`;
    return option;
  });
  select.replaceChildren(placeholder, ...options);
  if (options.some((option) => option.value === current)) select.value = current;
}

function isOfficialProfile(profile: RelayProfile): boolean {
  try {
    const url = new URL(profile.normalizedBaseUrl);
    const hostname = url.hostname.toLowerCase();
    if (url.protocol !== "https:" || (url.port && url.port !== "443")) return false;
    if (hostname === "api.openai.com") return profile.protocol === "openAiResponses" || profile.protocol === "openAiChatCompletions";
    return hostname === "api.anthropic.com" && profile.protocol === "anthropicMessages";
  } catch { return false; }
}

function renderBudget(): void {
  const mode = selectedAuditMode();
  const preset = BUDGETS[mode];
  const paired = Boolean((mount.querySelector<HTMLSelectElement>("#audit-baseline")?.value));
  const multiplier = paired ? 2 : 1;
  setText(".budget-requests", paired ? `${preset.requestLimit} / 端点·${preset.requestLimit * multiplier} 总计` : String(preset.requestLimit));
  setText(".budget-input", `${formatTokens(preset.inputTokenLimit * multiplier)}${paired ? " 总计" : ""}`);
  setText(".budget-output", `${formatTokens(preset.outputTokenLimit * multiplier)}${paired ? " 总计" : ""}`);
}

function renderProgress(): void {
  const shell = mount.querySelector<HTMLElement>(".progress-shell");
  const progress = state.activeAudit;
  setVisible(shell, Boolean(progress));
  const startButton = mount.querySelector<HTMLButtonElement>('[data-action="audit-start"]');
  if (startButton) {
    const starting = state.loading.has("audit-start");
    startButton.disabled = starting || Boolean(progress);
    startButton.setAttribute("aria-busy", String(starting));
    const label = startButton.querySelector<HTMLElement>(".button-label");
    if (label) label.textContent = starting ? "正在启动" : progress ? "审计进行中" : "开始审计";
  }
  if (!shell || !progress) return;
  const ratio = progress.totalCases > 0 ? Math.min(1, progress.completedCases / progress.totalCases) : 0;
  setText(".progress-title", phaseText(progress.phase));
  setText(".progress-count", `${progress.completedCases} / ${progress.totalCases || "—"}`);
  setText(".progress-detector", detectorText(progress.currentDetector));
  setText(".progress-requests", `${progress.usedRequests} 次请求`);
  setText(".progress-tokens", `约 ${formatTokens(progress.tokenEstimate)} token`);
  const bar = shell.querySelector<HTMLElement>(".progress-bar");
  if (bar) bar.style.width = `${Math.round(ratio * 100)}%`;
  const track = shell.querySelector<HTMLElement>(".progress-track");
  if (track) {
    track.setAttribute("aria-valuenow", String(Math.round(ratio * 100)));
    track.setAttribute("aria-valuetext", `${phaseText(progress.phase)}，${progress.completedCases} / ${progress.totalCases || "未知"}，${progress.usedRequests} 次请求`);
  }
  const cancelButton = shell.querySelector<HTMLButtonElement>('[data-action="audit-cancel"]');
  if (cancelButton) {
    const cancelling = state.loading.has("audit-cancel") || progress.phase.toLowerCase().includes("cancel");
    cancelButton.disabled = cancelling;
    cancelButton.setAttribute("aria-busy", String(cancelling));
    const label = cancelButton.querySelector<HTMLElement>(".button-label");
    if (label) label.textContent = cancelling ? "正在取消" : "取消检测";
  }
}

function renderScheduleForm(forceHydrate = false): void {
  const schedule = state.schedule;
  const enabled = mount.querySelector<HTMLInputElement>("#schedule-enabled");
  const cadence = mount.querySelector<HTMLSelectElement>("#schedule-cadence");
  const weekday = mount.querySelector<HTMLSelectElement>("#schedule-weekday");
  const time = mount.querySelector<HTMLInputElement>("#schedule-time");
  const paired = mount.querySelector<HTMLInputElement>("#schedule-pair-official");
  const limit = mount.querySelector<HTMLInputElement>("#schedule-monthly-limit");
  const retention = mount.querySelector<HTMLSelectElement>("#history-retention");
  const hydrate = forceHydrate || !state.scheduleDraftDirty;
  if (hydrate) {
    if (enabled) enabled.checked = schedule.enabled;
    if (cadence) cadence.value = schedule.cadence;
    if (weekday) weekday.value = String(schedule.weekday);
    if (time) time.value = schedule.localTime;
    if (paired) paired.checked = schedule.pairOfficial;
    if (limit) {
      limit.min = String(schedule.pairOfficial ? 300 : 150);
      limit.value = String(schedule.monthlyRequestLimit);
    }
    if (retention) retention.value = schedule.historyRetentionDays === null ? "forever" : String(schedule.historyRetentionDays);
  }
  if (forceHydrate) state.scheduleDraftDirty = false;
  if (weekday?.closest<HTMLElement>(".field")) weekday.closest<HTMLElement>(".field")!.hidden = (cadence?.value ?? schedule.cadence) === "daily";
  renderScheduleProfileOptions(!hydrate);
  const next = schedule.nextRunAt ? `下次计划：${new Date(schedule.nextRunAt).toLocaleString("zh-CN")}` : schedule.enabled ? "等待后端生成下次随机执行时间" : "定时检查保持关闭；不会静默消耗额度。";
  const last = schedule.lastRunAt ? `；上次：${new Date(schedule.lastRunAt).toLocaleString("zh-CN")}（${schedule.lastStatus ?? "状态未知"}）` : "";
  setText(".schedule-status", `${next}${last}`);
  renderScheduleBudgetState();
  const saveButton = mount.querySelector<HTMLButtonElement>('#schedule-form button[type="submit"]');
  if (saveButton) {
    const busy = state.loading.has("schedule-save");
    saveButton.disabled = busy;
    saveButton.setAttribute("aria-busy", String(busy));
    saveButton.textContent = busy ? "保存中" : "保存设置";
  }
}

function renderScheduleBudgetState(pairOverride?: boolean): void {
  const paired = pairOverride ?? Boolean(mount.querySelector<HTMLInputElement>("#schedule-pair-official")?.checked);
  const input = mount.querySelector<HTMLInputElement>("#schedule-monthly-limit");
  const minimum = paired ? 300 : 150;
  if (input) {
    input.min = String(minimum);
    const current = Math.trunc(Number(input.value) || 0);
    if (current < minimum) input.value = String(minimum);
  }
  const limit = Math.max(minimum, Math.trunc(Number(input?.value) || state.schedule.monthlyRequestLimit));
  const reserved = Math.max(0, state.schedule.monthlyReservedRequests);
  const remaining = Math.max(0, limit - reserved);
  const month = state.schedule.budgetMonth ? `${state.schedule.budgetMonth} ` : "本月 ";
  setText(".schedule-budget-state", `${month}已预留 ${reserved.toLocaleString("zh-CN")} / ${limit.toLocaleString("zh-CN")}，剩余 ${remaining.toLocaleString("zh-CN")}`);
}

function originShort(kind: OriginKind): string {
  if (kind === "officialChatGpt") return "官方 ChatGPT";
  if (kind === "officialOpenAiApi") return "官方 OpenAI API";
  if (kind === "officialAnthropicApi") return "官方 Anthropic API";
  if (kind === "managedProvider") return "托管提供方";
  if (kind === "customEndpoint") return "自定义端点";
  if (kind === "localEndpoint") return "本地端点";
  return "未知";
}

function authModeText(mode: AuthMode): string {
  if (mode === "chatGpt") return "ChatGPT 登录";
  if (mode === "apiKey") return "API Key";
  if (mode === "external") return "外部认证";
  return "认证模式未知";
}

function confidenceText(confidence: OriginConfidence): string {
  if (confidence === "configured") return "配置证据完整";
  if (confidence === "partial") return "仅部分配置证据";
  return "证据不足";
}

function originTooltip(origin: ConnectionOriginSnapshot): string {
  return [ORIGIN_TEXT[origin.kind], authModeText(origin.authMode), confidenceText(origin.confidence), ...origin.evidence.map((item) => `证据：${item}`), ...origin.limitations.map((item) => `限制：${item}`)].join("\n");
}

function phaseText(phase: string): string {
  const normalized = phase.toLowerCase();
  if (normalized.includes("cancel")) return "正在取消，不再启动新请求";
  if (normalized.includes("protocol")) return "正在检查协议";
  if (normalized.includes("usage") || normalized.includes("token")) return "正在核对计量";
  if (normalized.includes("quality")) return "正在检查行为质量";
  if (normalized.includes("fingerprint") || normalized.includes("distribution")) return "正在采样行为分布";
  if (normalized.includes("final")) return "正在生成报告";
  return "审计进行中";
}

function detectorText(detector: string): string {
  const known: Record<string, string> = {
    protocol: "协议兼容",
    usage: "计量一致",
    quality: "行为质量",
    qualityBasic: "基础质量",
    fingerprint: "分布指纹",
    mmd: "MMD 分布比较",
    cacheEvasion: "缓存规避检查",
    stability: "跨批次稳定性",
    paraphraseDrift: "改写题偏移",
  };
  return known[detector] ?? (cleanText(detector, 80) || "等待调度");
}

function selectedAuditMode(): AuditMode {
  const selected = mount.querySelector<HTMLInputElement>('input[name="audit-mode"]:checked')?.value;
  return selected === "standard" || selected === "deep" ? selected : "quick";
}

function activeAuditSnapshot(): RelayAuditProgress | undefined {
  // Backend events can update this property while an awaited command is in flight.
  return state.activeAudit;
}

async function loadOverview(quiet = true): Promise<boolean> {
  const requestId = ++overviewLoadSerial;
  const eventRevisionAtStart = overviewEventRevision;
  if (MOCK_MODE) {
    state.overview = mockOverview();
    state.connected = false;
    return true;
  }
  let result = await command<unknown>("get_workbench_overview", undefined, quiet);
  let usedSnapshotFallback = false;
  if (result === undefined) {
    result = await command<unknown>("get_snapshot", undefined, true);
    usedSnapshotFallback = result !== undefined;
  }
  if (requestId !== overviewLoadSerial || eventRevisionAtStart !== overviewEventRevision || result === undefined) return false;
  const next = normalizeOverview(result);
  if (usedSnapshotFallback) {
    next.axisSummary = state.overview.axisSummary;
    if (next.recentAlerts.length === 0) next.recentAlerts = state.overview.recentAlerts;
  }
  state.overview = next;
  state.connected = true;
  return true;
}

async function loadHistory(append = false, quiet = true): Promise<boolean> {
  const requestId = ++historyLoadSerial;
  if (MOCK_MODE) {
    state.history = mockHistory();
    state.historyTotal = state.history.length;
    return true;
  }
  const filter = readHistoryFilter(append ? state.history.length : 0);
  const result = await command<unknown>("list_conversation_history", { filter }, quiet);
  if (requestId !== historyLoadSerial || result === undefined) return false;
  const raw = asRecord(result);
  const items = listFromEnvelope(result, "history", "conversations").map(normalizeHistoryEntry);
  state.history = append ? dedupeHistory([...state.history, ...items]) : items;
  state.historyTotal = Math.max(state.history.length, Math.trunc(numberValue(pick(raw, "total", "totalCount", "total_count")) ?? items.length));
  return true;
}

async function loadProfiles(quiet = true): Promise<boolean> {
  const requestId = ++profilesLoadSerial;
  if (MOCK_MODE) {
    if (state.profiles.length === 0) state.profiles = mockProfiles();
    return true;
  }
  const result = await command<unknown>("list_relay_profiles", undefined, quiet);
  if (requestId !== profilesLoadSerial || result === undefined) return false;
  state.profiles = listFromEnvelope(result, "profiles").map(normalizeProfile);
  if (state.selectedProfileId && !state.profiles.some((profile) => profile.id === state.selectedProfileId)) state.selectedProfileId = undefined;
  return true;
}

async function loadAudits(quiet = true): Promise<boolean> {
  const requestId = ++auditsLoadSerial;
  if (MOCK_MODE) {
    if (state.audits.length === 0) state.audits = mockReports();
    return true;
  }
  const result = await command<unknown>("list_relay_audits", { limit: 20 }, quiet);
  if (requestId !== auditsLoadSerial || result === undefined) return false;
  state.audits = listFromEnvelope(result, "audits", "reports").map(normalizeReport);
  state.activeAudit = listFromEnvelope(result, "activeRuns", "active_runs")
    .map((run) => normalizeProgress(asRecord(run).progress ?? run))
    .find((progress): progress is RelayAuditProgress => Boolean(progress));
  return true;
}

async function loadBaselines(quiet = true): Promise<boolean> {
  const requestId = ++baselinesLoadSerial;
  if (MOCK_MODE) {
    if (state.baselines.length === 0) state.baselines = mockBaselines();
    return true;
  }
  const result = await command<unknown>("list_relay_baselines", undefined, quiet);
  if (requestId !== baselinesLoadSerial || result === undefined) return false;
  const imported = listFromEnvelope(result, "baselines");
  const builtIn = listFromEnvelope(result, "builtInCommunityBaselines", "built_in_community_baselines");
  state.baselines = [...builtIn, ...imported].map(normalizeBaseline);
  return true;
}

async function loadSchedule(quiet = true): Promise<boolean> {
  const requestId = ++scheduleLoadSerial;
  if (MOCK_MODE) return true;
  const result = await command<unknown>("get_audit_schedule", undefined, quiet);
  if (requestId !== scheduleLoadSerial || result === undefined) return false;
  state.schedule = normalizeSchedule(result);
  return true;
}

async function refreshAll(quiet = false): Promise<void> {
  if (state.loading.has("refresh")) return;
  state.loading.add("refresh");
  setRefreshBusy(true);
  try {
    const results = await withUiTimeout(Promise.all([loadOverview(quiet), loadHistory(false, quiet), loadProfiles(quiet), loadAudits(quiet), loadBaselines(quiet), loadSchedule(quiet)]));
    if (!quiet && results.some(Boolean)) showNotice("工作台已使用最新本地证据", "green", 2_600);
  } catch (error) {
    if (error instanceof UiRefreshTimeoutError) {
      invalidatePendingLoads();
      if (!quiet) showNotice("本次刷新超时，已保留上一份有效数据，可继续使用工作台", "yellow", 7_000);
    } else throw error;
  } finally {
    state.loading.delete("refresh");
    setRefreshBusy(false);
    render();
  }
}

async function refreshCurrentPage(): Promise<void> {
  if (state.loading.has("refresh")) return;
  state.loading.add("refresh");
  setRefreshBusy(true);
  try {
    const refresh = state.page === "overview"
      ? Promise.all([loadOverview(false), loadSchedule(false)])
      : state.page === "history"
        ? loadHistory(false, false)
        : state.page === "relay"
          ? Promise.all([loadProfiles(false), loadAudits(false), loadBaselines(true)])
          : state.page === "baselines"
            ? loadBaselines(false)
            : loadOverview(true);
    await withUiTimeout(Promise.resolve(refresh));
  } catch (error) {
    if (error instanceof UiRefreshTimeoutError) {
      invalidatePendingLoads();
      showNotice("本次刷新超时，已保留上一份有效数据", "yellow", 7_000);
    } else throw error;
  } finally {
    state.loading.delete("refresh");
    setRefreshBusy(false);
    render();
  }
}

function setRefreshBusy(busy: boolean): void {
  const button = mount.querySelector<HTMLButtonElement>('[data-action="refresh"]');
  if (!button) return;
  button.disabled = busy;
  button.setAttribute("aria-busy", String(busy));
  const label = button.querySelector<HTMLElement>("span:last-child");
  if (label) label.textContent = busy ? "刷新中" : "刷新";
}

function readHistoryFilter(offset = 0): Record<string, unknown> {
  return {
    query: mount.querySelector<HTMLInputElement>("#history-query")?.value.trim() || undefined,
    model: mount.querySelector<HTMLInputElement>("#history-model")?.value.trim() || undefined,
    effort: mount.querySelector<HTMLSelectElement>("#history-effort")?.value || undefined,
    originKind: mount.querySelector<HTMLSelectElement>("#history-origin")?.value || undefined,
    statusLevel: mount.querySelector<HTMLSelectElement>("#history-status")?.value || undefined,
    limit: 100,
    offset,
  };
}

function dedupeHistory(items: HistoryEntry[]): HistoryEntry[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    if (seen.has(item.id)) return false;
    seen.add(item.id);
    return true;
  });
}

function clearProfileForm(): void {
  const form = mount.querySelector<HTMLFormElement>("#relay-profile-form");
  form?.reset();
  setInputValue("#relay-profile-id", "");
  setInputValue("#relay-private-probe-path", "");
  const protocol = mount.querySelector<HTMLSelectElement>("#relay-protocol");
  if (protocol) protocol.value = "openAiResponses";
  state.selectedProfileId = undefined;
  setText(".connection-test-state", "");
  renderPrivateProbeState();
  renderProfiles();
  renderBaselineSelect();
  mount.querySelector<HTMLInputElement>("#relay-label")?.focus();
}

function populateProfileForm(profile: RelayProfile): void {
  state.selectedProfileId = profile.id;
  setInputValue("#relay-profile-id", profile.id);
  setInputValue("#relay-label", profile.label);
  setInputValue("#relay-base-url", profile.normalizedBaseUrl);
  setInputValue("#relay-model", profile.defaultModel);
  setInputValue("#relay-api-key", "");
  setInputValue("#relay-private-probe-path", profile.privateProbePack?.path ?? "");
  const protocol = mount.querySelector<HTMLSelectElement>("#relay-protocol");
  if (protocol) protocol.value = profile.protocol;
  const keychain = mount.querySelector<HTMLInputElement>("#relay-keychain");
  if (keychain) keychain.checked = Boolean(profile.credentialRef);
  setText(".connection-test-state", profile.credentialRef ? "已关联系统凭据" : state.credentials.has(profile.id) ? "当前进程已有凭据" : "");
  renderPrivateProbeState(profile.privateProbePack);
  renderProfiles();
  renderBaselineSelect();
}

function setInputValue(selector: string, value: string): void {
  const input = mount.querySelector<HTMLInputElement>(selector);
  if (input) input.value = value;
}

function profileDraft(): RelayProfile {
  const id = mount.querySelector<HTMLInputElement>("#relay-profile-id")?.value.trim() || `profile-${crypto.randomUUID()}`;
  const existing = state.profiles.find((profile) => profile.id === id);
  const now = new Date().toISOString();
  const draft: RelayProfile = {
    id,
    label: mount.querySelector<HTMLInputElement>("#relay-label")?.value.trim() || "",
    normalizedBaseUrl: normalizeEndpoint(mount.querySelector<HTMLInputElement>("#relay-base-url")?.value ?? ""),
    protocol: normalizeProtocol(mount.querySelector<HTMLSelectElement>("#relay-protocol")?.value),
    defaultModel: mount.querySelector<HTMLInputElement>("#relay-model")?.value.trim() || "",
    createdAt: existing?.createdAt ?? now,
    updatedAt: now,
  };
  const privateProbePath = mount.querySelector<HTMLInputElement>("#relay-private-probe-path")?.value.trim();
  if (privateProbePath) {
    draft.privateProbePack = existing?.privateProbePack?.path === privateProbePath
      ? existing.privateProbePack
      : { path: privateProbePath, version: "", sha256: "" };
  }
  if (existing && sameCredentialBinding(existing, draft)) draft.credentialRef = existing.credentialRef;
  return draft;
}

function renderPrivateProbeState(reference?: PrivateProbePackReference): void {
  const path = mount.querySelector<HTMLInputElement>("#relay-private-probe-path")?.value.trim();
  const selected = reference ?? state.profiles.find((profile) => profile.id === state.selectedProfileId)?.privateProbePack;
  const matching = Boolean(path && selected?.path === path && selected.sha256);
  setText(
    ".private-probe-state",
    matching
      ? `${selected?.version || "版本未知"} · SHA-256 ${selected?.sha256.slice(0, 8)}`
      : path
        ? "保存配置后校验 schema、版本与 SHA-256"
        : "未选择",
  );
}

function sameCredentialBinding(left: RelayProfile, right: RelayProfile): boolean {
  return left.normalizedBaseUrl === right.normalizedBaseUrl && left.protocol === right.protocol;
}

function credentialForDraft(profile: RelayProfile): string | undefined {
  const typed = mount.querySelector<HTMLInputElement>("#relay-api-key")?.value;
  if (typed) return typed;
  const saved = state.profiles.find((item) => item.id === profile.id);
  return saved && sameCredentialBinding(saved, profile) ? state.credentials.get(profile.id) : undefined;
}

function validateProfileForm(): boolean {
  const form = mount.querySelector<HTMLFormElement>("#relay-profile-form");
  if (!form?.reportValidity()) return false;
  try { profileDraft(); }
  catch {
    showNotice("Base URL 无效，请输入 HTTP(S) 端点", "yellow");
    return false;
  }
  return true;
}

async function saveProfile(silent = false): Promise<RelayProfile | undefined> {
  if (!validateProfileForm()) return undefined;
  const profile = profileDraft();
  const previous = state.profiles.find((item) => item.id === profile.id);
  const credentialBindingChanged = Boolean(previous && !sameCredentialBinding(previous, profile));
  const credential = credentialForDraft(profile);
  const persistCredential = Boolean(mount.querySelector<HTMLInputElement>("#relay-keychain")?.checked);
  if (persistCredential && !credential && !profile.credentialRef) {
    showNotice("要保存到系统凭据库，请先输入 API Key", "yellow");
    return undefined;
  }
  const raw = MOCK_MODE ? profile : await command<unknown>("upsert_relay_profile", { profile, credential, persistCredential });
  if (raw === undefined) return undefined;
  const returned = asRecord(raw);
  const normalized = normalizeProfile(returned.profile ?? raw, 0);
  const merged = { ...profile, ...normalized, id: normalized.id || profile.id };
  state.profiles = [...state.profiles.filter((item) => item.id !== merged.id), merged].sort((left, right) => left.label.localeCompare(right.label, "zh-CN"));
  state.selectedProfileId = merged.id;
  if (credential && !persistCredential) state.credentials.set(merged.id, credential);
  else if (persistCredential || credentialBindingChanged) state.credentials.delete(merged.id);
  populateProfileForm(merged);
  if (persistCredential && returned.credentialRef) setInputValue("#relay-api-key", "");
  const scheduleDisabled = booleanValue(pick(returned, "scheduleDisabled", "schedule_disabled"));
  const backendWarning = cleanText(returned.warning, 240);
  if (scheduleDisabled) {
    await loadSchedule(true);
    renderScheduleForm();
    showNotice(
      `端点或凭据绑定已变化，相关定时检查已停用；请核对后重新确认${backendWarning ? `。${backendWarning}` : ""}`,
      "yellow",
      8_000,
    );
  } else if (backendWarning) {
    showNotice(backendWarning, "yellow", 7_000);
  } else if (!silent) {
    showNotice(
      isInsecureRemoteEndpoint(merged.normalizedBaseUrl)
        ? "端点已保存；非本机 HTTP 仅会在你逐次确认后发送请求，API Key 未写入小狸数据库"
        : "端点配置已保存，API Key 未写入小狸数据库",
      isInsecureRemoteEndpoint(merged.normalizedBaseUrl) ? "yellow" : "green",
    );
  }
  return merged;
}

async function testConnection(): Promise<void> {
  if (!validateProfileForm()) return;
  const profile = profileDraft();
  const credential = credentialForDraft(profile);
  if (!credential && !profile.credentialRef) {
    showNotice("连接测试需要 API Key；可只在本次进程内使用", "yellow");
    return;
  }
  if (!confirmInsecureRemoteRequest(profile)) {
    showNotice("已取消：没有向非本机 HTTP 端点发送凭据或请求", "gray");
    return;
  }
  if (!window.confirm(`对“${profile.label || "该端点"}”运行连接测试？\n\n硬上限：6 次请求；可见输入少于 1,000 token；最大输出 384 token。不调用官方配对端。`)) {
    showNotice("连接测试已取消，没有产生请求或额度消耗", "gray");
    return;
  }
  const button = mount.querySelector<HTMLButtonElement>('[data-action="connection-test"]');
  if (button) button.disabled = true;
  setText(".connection-test-state", "正在检查，最多 6 次请求…");
  try {
    const raw = MOCK_MODE ? { ok: true, level: "green", summary: "认证、基础响应与 SSE 正常", usedRequests: 4 } : await command<unknown>("test_relay_connection", { profileId: state.selectedProfileId, profile, credential });
    if (raw === undefined) return;
    const result = asRecord(raw);
    const ok = booleanValue(result.ok, cleanText(result.status)?.toLowerCase() === "ok");
    const summary = cleanText(pick(result, "summary", "message"), 180) ?? (ok ? "基础连接测试通过" : "连接测试未通过");
    const level = normalizeLevel(result.level, ok ? "green" : "yellow");
    setText(".connection-test-state", summary);
    showNotice(summary, level);
  } finally {
    if (button) button.disabled = false;
  }
}

function normalizeAuditPlanPreview(value: unknown): AuditPlanPreview {
  const preview = asRecord(asRecord(value).plan ?? value);
  return {
    builtInRequests: Math.max(0, Math.trunc(numberValue(pick(preview, "builtInRequests", "built_in_requests")) ?? 0)),
    privateProbeRequests: Math.max(0, Math.trunc(numberValue(pick(preview, "privateProbeRequests", "private_probe_requests")) ?? 0)),
    plannedRequests: Math.max(0, Math.trunc(numberValue(pick(preview, "plannedRequests", "planned_requests")) ?? 0)),
    conservativeInputTokens: Math.max(0, Math.trunc(numberValue(pick(preview, "conservativeInputTokens", "conservative_input_tokens")) ?? 0)),
    conservativeOutputTokens: Math.max(0, Math.trunc(numberValue(pick(preview, "conservativeOutputTokens", "conservative_output_tokens")) ?? 0)),
    privateProbeInputTokens: Math.max(0, Math.trunc(numberValue(pick(preview, "privateProbeInputTokens", "private_probe_input_tokens")) ?? 0)),
    privateProbeOutputTokens: Math.max(0, Math.trunc(numberValue(pick(preview, "privateProbeOutputTokens", "private_probe_output_tokens")) ?? 0)),
    fitsDeclaredBudget: booleanValue(pick(preview, "fitsDeclaredBudget", "fits_declared_budget")),
  };
}

async function previewAuditPlan(
  mode: AuditMode,
  profile: RelayProfile,
  request: Record<string, unknown>,
): Promise<AuditPlanPreview | undefined> {
  if (MOCK_MODE) {
    // The browser fixture represents a two-task private pack. Production
    // parses the selected file and returns its exact task count.
    const privateProbeRequests = profile.privateProbePack ? 2 : 0;
    const builtInRequests = BUILT_IN_REQUESTS[mode];
    const budget = BUDGETS[mode];
    return {
      builtInRequests,
      privateProbeRequests,
      plannedRequests: builtInRequests + privateProbeRequests,
      conservativeInputTokens: Math.floor(budget.inputTokenLimit * 0.8) + privateProbeRequests * 64,
      conservativeOutputTokens: Math.floor(budget.outputTokenLimit * 0.8) + privateProbeRequests * 16,
      privateProbeInputTokens: privateProbeRequests * 64,
      privateProbeOutputTokens: privateProbeRequests * 16,
      fitsDeclaredBudget: builtInRequests + privateProbeRequests <= budget.requestLimit,
    };
  }
  const raw = await command<unknown>("preview_relay_audit_plan", { request });
  return raw === undefined ? undefined : normalizeAuditPlanPreview(raw);
}

function auditPlanOverages(preview: AuditPlanPreview, budget: BudgetPreset): string {
  return [
    preview.plannedRequests > budget.requestLimit ? `请求 ${preview.plannedRequests} > ${budget.requestLimit}` : "",
    preview.conservativeInputTokens > budget.inputTokenLimit ? `保守输入 ${formatTokens(preview.conservativeInputTokens)} > ${formatTokens(budget.inputTokenLimit)}` : "",
    preview.conservativeOutputTokens > budget.outputTokenLimit ? `保守输出 ${formatTokens(preview.conservativeOutputTokens)} > ${formatTokens(budget.outputTokenLimit)}` : "",
  ].filter(Boolean).join("；");
}

async function startAudit(): Promise<void> {
  if (state.activeAudit || state.loading.has("audit-start")) {
    showNotice("已有一个审计正在运行，可先取消或等待完成", "yellow");
    return;
  }
  state.loading.add("audit-start");
  renderProgress();
  try {
    const profile = await saveProfile(true);
    if (!profile) return;
    if (!confirmInsecureRemoteRequest(profile)) {
      showNotice("已取消：没有向非本机 HTTP 端点发送凭据或审计请求", "gray");
      return;
    }
    const mode = selectedAuditMode();
    const budget = BUDGETS[mode];
    const officialBaselineProfileId = mount.querySelector<HTMLSelectElement>("#audit-baseline")?.value || undefined;
    const credential = credentialForDraft(profile);
    if (!credential && !profile.credentialRef) {
      showNotice("开始审计前请输入 API Key，或为该端点绑定系统凭据", "yellow");
      return;
    }
    const reference = officialBaselineProfileId
      ? state.profiles.find((item) => item.id === officialBaselineProfileId)
      : undefined;
    if (officialBaselineProfileId && !reference) {
      showNotice("选择的官方配对端点已不存在，请重新选择", "yellow");
      return;
    }
    const officialCredential = reference ? state.credentials.get(reference.id) : undefined;
    if (reference && !officialCredential && !reference.credentialRef) {
      showNotice("官方配对端点缺少凭据：请先编辑该端点并输入本次进程内 Key，或绑定系统凭据", "yellow", 8_000);
      return;
    }
    if (reference && (reference.protocol !== profile.protocol || reference.defaultModel !== profile.defaultModel)) {
      showNotice("官方配对要求两端使用相同协议与精确模型，请先调整端点配置", "yellow", 8_000);
      return;
    }
    const request = {
      profileId: profile.id,
      model: profile.defaultModel,
      mode,
      officialBaselineProfileId,
      maxRequests: budget.requestLimit,
      maxInputTokens: budget.inputTokenLimit,
      maxOutputTokens: budget.outputTokenLimit,
      timeoutMs: budget.timeoutMs,
      enabledDetectors: budget.detectors,
    };
    const preview = await previewAuditPlan(mode, profile, request);
    if (!preview) return;
    if (!preview.fitsDeclaredBudget) {
      const overages = auditPlanOverages(preview, budget);
      showNotice(
        `完整计划超出已确认预算（${overages || "预算不足"}；内置 ${preview.builtInRequests} + 私有题包 ${preview.privateProbeRequests}）；请减少题包任务或调整检测档位`,
        "yellow",
        10_000,
      );
      return;
    }
    const multiplier = reference ? 2 : 1;
    const confirmed = window.confirm(
      `开始${budget.label}审计？\n\n` +
      `目标：${profile.label}\n` +
      `模型：${profile.defaultModel}\n` +
      `官方配对：${reference ? `${reference.label}（使用当前进程内存或系统凭据）` : "不调用"}\n` +
      `私有题包：${profile.privateProbePack ? `${profile.privateProbePack.version} · ${profile.privateProbePack.sha256.slice(0, 8)} · 每端点增加 ${preview.privateProbeRequests} 次、保守输入 ${formatTokens(preview.privateProbeInputTokens)}、输出 ${formatTokens(preview.privateProbeOutputTokens)}` : "未使用（增加 0 次）"}\n` +
      `完整计划：${preview.plannedRequests} 次 / 端点（内置 ${preview.builtInRequests} + 私有 ${preview.privateProbeRequests}），${preview.plannedRequests * multiplier} 次总操作\n` +
      `硬上限：${budget.requestLimit} 次 / 端点，${budget.requestLimit * multiplier} 次总请求\n` +
      `输入 token 上限：${formatTokens(budget.inputTokenLimit * multiplier)} 总计\n` +
      `输出 token 上限：${formatTokens(budget.outputTokenLimit * multiplier)} 总计\n\n` +
      "网络重试也计入上限；结果不能密码学证明物理模型。",
    );
    if (!confirmed) {
      showNotice("审计已取消，没有产生请求或额度消耗", "gray");
      return;
    }
    const auditRevisionAtStart = auditEventRevision;
    const raw = MOCK_MODE ? { auditId: `audit-${crypto.randomUUID()}`, phase: "protocol", completedCases: 0, totalCases: budget.requestLimit, usedRequests: 0, tokenEstimate: 0, currentDetector: "protocol" } : await command<unknown>("start_relay_audit", { request, credential, officialCredential });
    if (raw === undefined) return;
    const progress = normalizeProgress(asRecord(raw).progress ?? raw);
    if (progress) {
      const currentProgress = activeAuditSnapshot();
      const eventProgress = currentProgress?.auditId === progress.auditId ? currentProgress : undefined;
      // The command returns the run's initial receipt; an event received during
      // the await is always the newer source, even when its counters are equal.
      if (!eventProgress && auditRevisionAtStart === auditEventRevision) state.activeAudit = progress;
      if (activeAuditSnapshot()?.auditId === progress.auditId) showNotice(`已开始${budget.label}审计，硬上限 ${budget.requestLimit} 次 / 端点`, "green");
      if (MOCK_MODE) startMockAudit(progress.auditId, budget.requestLimit);
    } else {
      const report = normalizeReport(asRecord(raw).report ?? raw, 0);
      state.audits = [report, ...state.audits.filter((item) => item.auditId !== report.auditId)];
      showNotice("审计已完成，请查看四条证据轴与真实会话对照", reportEffectiveLevel(report));
    }
  } finally {
    state.loading.delete("audit-start");
    render();
  }
}

function startMockAudit(auditId: string, total: number): void {
  let completed = 0;
  const timer = window.setInterval(() => {
    if (state.activeAudit?.auditId !== auditId) {
      window.clearInterval(timer);
      return;
    }
    completed = Math.min(total, completed + Math.max(1, Math.floor(total / 18)));
    state.activeAudit = { auditId, phase: completed < total * 0.3 ? "protocol" : completed < total * 0.7 ? "fingerprint" : "finalizing", completedCases: completed, totalCases: total, usedRequests: completed, tokenEstimate: completed * 28, currentDetector: completed < total * 0.3 ? "protocol" : completed < total * 0.7 ? "fingerprint" : "mmd" };
    renderProgress();
    if (completed >= total) {
      window.clearInterval(timer);
      state.activeAudit = undefined;
      state.audits = [mockReports()[0], ...state.audits];
      render();
      showNotice("浏览器预览审计已完成", "green");
    }
  }, 180);
}

async function cancelAudit(): Promise<void> {
  const audit = state.activeAudit;
  if (!audit || state.loading.has("audit-cancel") || audit.phase.toLowerCase().includes("cancel")) return;
  state.loading.add("audit-cancel");
  renderProgress();
  try {
    if (!MOCK_MODE) {
      const result = await command<unknown>("cancel_relay_audit", { auditId: audit.auditId });
      if (result === undefined) return;
      const cancelled = booleanValue(asRecord(result).cancelled);
      if (cancelled && state.activeAudit?.auditId === audit.auditId) {
        state.activeAudit = { ...state.activeAudit, phase: "cancellationRequested" };
        renderProgress();
        showNotice("后端已接受取消；当前在途请求结束前不会启动新请求", "gray");
        return;
      }
      await loadAudits(true);
      render();
      showNotice(
        cancelled ? "取消已确认；审计状态已刷新" : "该审计已结束或当前无法取消；已刷新最新状态",
        "gray",
      );
      return;
    }
    state.activeAudit = undefined;
    renderProgress();
    showNotice("已请求取消，已完成证据可作为 cancelled 报告保留", "gray");
  } finally {
    state.loading.delete("audit-cancel");
    renderProgress();
  }
}

async function deleteProfile(id: string): Promise<void> {
  const profile = state.profiles.find((item) => item.id === id);
  if (!profile || !window.confirm(`删除端点配置“${profile.label}”？历史审计报告会保留。`)) return;
  let scheduleDisabled = false;
  let warning: string | undefined;
  if (!MOCK_MODE) {
    const result = await command<unknown>("delete_relay_profile", { profileId: id });
    if (result === undefined) return;
    const outcome = asRecord(result);
    scheduleDisabled = booleanValue(pick(outcome, "scheduleDisabled", "schedule_disabled"));
    warning = cleanText(outcome.warning, 260);
    if (!booleanValue(outcome.deleted)) {
      await loadProfiles(true);
      renderProfiles();
      if (scheduleDisabled) {
        await loadSchedule(true);
        state.scheduleDraftDirty = false;
        renderScheduleForm(true);
      }
      showNotice(
        [
          "端点未删除，可能已不存在或正被审计使用；列表已刷新",
          scheduleDisabled ? "后端发现关联的定时检查并已将其停用" : "",
          warning ?? "",
        ].filter(Boolean).join("。"),
        "yellow",
        9_000,
      );
      return;
    }
  }
  state.profiles = state.profiles.filter((item) => item.id !== id);
  state.credentials.delete(id);
  if (state.selectedProfileId === id) clearProfileForm();
  renderProfiles();
  if (scheduleDisabled) {
    await loadSchedule(true);
    state.scheduleDraftDirty = false;
    renderScheduleForm(true);
  }
  if (warning || scheduleDisabled) {
    showNotice(
      [scheduleDisabled ? "与该端点关联的定时检查已自动停用" : "", warning ?? ""].filter(Boolean).join("。"),
      "yellow",
      9_000,
    );
  } else {
    showNotice("端点配置已删除；历史审计报告保留", "green");
  }
}

async function deleteReport(auditId: string): Promise<void> {
  const report = state.audits.find((item) => item.auditId === auditId);
  if (!report) return;
  if (!window.confirm(`删除“${report.profileLabel}”的这份审计报告？\n\n只删除报告记录，不会删除端点配置、API 凭据或会话历史。`)) return;
  if (!MOCK_MODE) {
    const result = await command<unknown>("delete_relay_audit", { auditId });
    if (result === undefined) return;
    if (!booleanValue(asRecord(result).deleted)) {
      await loadAudits(true);
      renderAudits();
      showNotice("报告不存在或仍在运行；已刷新审计列表", "yellow");
      return;
    }
  }
  state.audits = state.audits.filter((item) => item.auditId !== auditId);
  renderAudits();
  showNotice("审计报告已删除；端点、凭据和会话历史未改变", "green");
}

async function saveSchedule(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (state.loading.has("schedule-save")) return;
  state.loading.add("schedule-save");
  renderScheduleForm();
  try {
    const retentionValue = mount.querySelector<HTMLSelectElement>("#history-retention")?.value ?? "180";
    const profileId = mount.querySelector<HTMLSelectElement>("#schedule-profile")?.value || undefined;
    const officialBaselineProfileId = mount.querySelector<HTMLSelectElement>("#schedule-official-profile")?.value || undefined;
    const enabled = Boolean(mount.querySelector<HTMLInputElement>("#schedule-enabled")?.checked);
    const pairOfficial = Boolean(mount.querySelector<HTMLInputElement>("#schedule-pair-official")?.checked);
    const profile = state.profiles.find((item) => item.id === profileId);
    const officialProfile = state.profiles.find((item) => item.id === officialBaselineProfileId);
    const minimumMonthlyLimit = pairOfficial ? 300 : 150;
    if (enabled && (!profile || !profile.credentialRef)) {
      showNotice("定时检查必须绑定已保存到系统凭据库的检测端点；内存 Key 会在退出后失效", "yellow");
      return;
    }
    if (enabled && profile && isInsecureRemoteEndpoint(profile.normalizedBaseUrl)) {
      showNotice("定时检查不会向非本机 HTTP 端点自动发送 API Key；请改用 HTTPS 或 localhost", "yellow");
      return;
    }
    if (enabled && pairOfficial && (!officialProfile || !officialProfile.credentialRef || !isOfficialProfile(officialProfile))) {
      showNotice("官方配对必须选择已保存到系统凭据库的 OpenAI/Anthropic 官方端点", "yellow");
      return;
    }
    if (enabled && pairOfficial && profile && officialProfile && (officialProfile.protocol !== profile.protocol || officialProfile.defaultModel !== profile.defaultModel)) {
      showNotice("定时官方配对要求两端使用相同协议与精确模型，请先调整端点配置", "yellow", 8_000);
      return;
    }
    const schedule: AuditSchedule = {
      enabled,
      profileId,
      officialBaselineProfileId: pairOfficial ? officialBaselineProfileId : undefined,
      cadence: mount.querySelector<HTMLSelectElement>("#schedule-cadence")?.value === "daily" ? "daily" : "weekly",
      weekday: Math.min(6, Math.max(0, Math.trunc(Number(mount.querySelector<HTMLSelectElement>("#schedule-weekday")?.value) || 0))),
      localTime: mount.querySelector<HTMLInputElement>("#schedule-time")?.value || "20:00",
      pairOfficial,
      monthlyRequestLimit: Math.max(minimumMonthlyLimit, Math.trunc(Number(mount.querySelector<HTMLInputElement>("#schedule-monthly-limit")?.value) || 1_000)),
      budgetMonth: state.schedule.budgetMonth,
      monthlyReservedRequests: state.schedule.monthlyReservedRequests,
      historyRetentionDays: retentionValue === "forever" ? null : Math.max(1, Math.trunc(Number(retentionValue) || 180)),
    };
    if (enabled && profile) {
      const budget = BUDGETS.quick;
      const request = {
        profileId: profile.id,
        model: profile.defaultModel,
        mode: "quick",
        officialBaselineProfileId: pairOfficial ? officialBaselineProfileId : undefined,
        maxRequests: budget.requestLimit,
        maxInputTokens: budget.inputTokenLimit,
        maxOutputTokens: budget.outputTokenLimit,
        timeoutMs: budget.timeoutMs,
        enabledDetectors: budget.detectors,
      };
      const preview = await previewAuditPlan("quick", profile, request);
      if (!preview) return;
      if (!preview.fitsDeclaredBudget) {
        const overages = auditPlanOverages(preview, budget);
        showNotice(
          `定时快速审计的完整计划超出每次预算（${overages || "预算不足"}；内置 ${preview.builtInRequests} + 私有题包 ${preview.privateProbeRequests}）；未启用定时检查`,
          "yellow",
          10_000,
        );
        return;
      }
      const multiplier = pairOfficial ? 2 : 1;
      const runOperations = preview.plannedRequests * multiplier;
      if (!window.confirm(
        `启用${schedule.cadence === "daily" ? "每日" : "每周"}快速审计？\n\n` +
        `完整计划：${preview.plannedRequests} 次 / 端点（内置 ${preview.builtInRequests} + 私有题包 ${preview.privateProbeRequests}），每次共 ${runOperations} 次操作。\n` +
        `私有题包：${profile.privateProbePack ? `${profile.privateProbePack.version} · ${profile.privateProbePack.sha256.slice(0, 8)} · 每端点增加 ${preview.privateProbeRequests} 次` : "未使用（增加 0 次）"}。\n` +
        `每端点硬上限 ${budget.requestLimit} 次；每月硬上限 ${schedule.monthlyRequestLimit} 次。执行时间会随机抖动 ±30 分钟。`,
      )) {
        showNotice("定时检查未启用，没有产生请求或额度消耗", "gray");
        return;
      }
    }
    if (!MOCK_MODE) {
      const result = await command<unknown>("update_audit_schedule", { schedule });
      if (result === undefined) return;
      state.schedule = normalizeSchedule(asRecord(result).schedule ?? result);
    } else state.schedule = schedule;
    state.scheduleDraftDirty = false;
    renderScheduleForm(true);
    showNotice(state.schedule.enabled ? `定时检查已启用：${state.schedule.cadence === "daily" ? "每日" : "每周"} ${state.schedule.localTime}，受每月 ${state.schedule.monthlyRequestLimit} 次上限保护` : "设置已保存，定时检查保持关闭", "green");
  } finally {
    state.loading.delete("schedule-save");
    renderScheduleForm();
  }
}

async function importBaseline(): Promise<void> {
  const input = mount.querySelector<HTMLInputElement>("#baseline-file");
  const file = input?.files?.[0];
  if (!file) {
    showNotice("请先选择 JSON 参考摘要", "yellow");
    return;
  }
  if (file.size > 2 * 1024 * 1024) {
    showNotice("参考摘要超过 2 MiB 本地导入上限", "yellow");
    return;
  }
  let packageValue: unknown;
  try { packageValue = JSON.parse(await file.text()); }
  catch {
    showNotice("无法解析参考摘要 JSON，原文未被保存", "yellow");
    return;
  }
  if (packageValue === null || typeof packageValue !== "object") {
    showNotice("参考摘要结构无效", "yellow");
    return;
  }
  if (MOCK_MODE) {
    state.baselines.push(normalizeBaseline({ id: `baseline-${crypto.randomUUID()}`, label: file.name.replace(/\.json$/i, ""), model: "gpt-5.6-sol", protocol: "openAiResponses", source: "user", sampleCount: 240, signed: false }, state.baselines.length));
  } else {
    const result = await command<unknown>("import_relay_baseline", { package: packageValue });
    if (result === undefined) return;
    const verified = booleanValue(pick(asRecord(result), "signatureVerified", "signature_verified"));
    await loadBaselines(true);
    if (input) input.value = "";
    render();
    showNotice(
      verified ? "参考摘要签名标记已验证并导入；当前 beta 仍不用于评分" : "参考摘要已导入，但未验证签名；当前 beta 不用于评分",
      "yellow",
    );
    return;
  }
  if (input) input.value = "";
  render();
  showNotice("参考摘要元数据已导入；浏览器预览不验证签名，也不用于评分", "yellow");
}

async function deleteBaseline(id: string): Promise<void> {
  const baseline = state.baselines.find((item) => item.id === id && item.source === "user");
  if (!baseline || !window.confirm(`删除用户参考摘要“${baseline.label}”？`)) return;
  if (!MOCK_MODE) {
    const result = await command<unknown>("delete_relay_baseline", { baselineId: id });
    if (result === undefined) return;
    if (!booleanValue(asRecord(result).deleted)) {
      await loadBaselines(true);
      renderBaselines();
      showNotice("参考摘要未删除，可能已不存在或不属于用户可删除项；列表已刷新", "yellow", 7_000);
      return;
    }
  }
  state.baselines = state.baselines.filter((item) => item.id !== id);
  renderBaselines();
  showNotice("用户参考摘要已删除", "green");
}

async function openHistoryDetail(id: string): Promise<void> {
  const local = state.history.find((item) => item.id === id);
  if (!local) return;
  let detail = local;
  if (!MOCK_MODE) {
    const raw = await command<unknown>("get_conversation_detail", { threadId: local.threadId, turnId: local.turnId }, true);
    if (raw !== undefined) detail = normalizeHistoryEntry(asRecord(raw).conversation ?? raw, 0);
  }
  const body = document.querySelector<HTMLElement>(".conversation-dialog-body");
  if (!body) return;
  const heading = document.createElement("div");
  const title = document.createElement("h3");
  title.textContent = detail.displayName;
  title.style.margin = "0 0 4px";
  const meta = document.createElement("span");
  meta.className = "cell-secondary mono";
  meta.textContent = `Thread ${shortId(detail.threadId)}${detail.turnId ? `·Turn ${shortId(detail.turnId)}` : ""}`;
  heading.append(title, meta);
  const grid = detailGrid([
    ["请求模型", detail.model ?? "未知"],
    ["请求 effort", detail.effort ?? "未知"],
    ["连接来源", ORIGIN_TEXT[detail.origin.kind]],
    ["来源置信度", confidenceText(detail.origin.confidence)],
    ["累计 token", formatTokens(detail.totalTokens)],
    ["缓存输入", formatPercent(detail.cacheInputShare)],
    ["TTFT", formatDuration(detail.ttftMs)],
    ["端到端输出速率", detail.outputRate === undefined ? "—" : `${detail.outputRate.toFixed(2)} tok/s`],
    ["推理输出", formatTokens(detail.reasoningTokens)],
    ["路由证据", routeEvidenceText(detail.routeEvidence)],
    ["状态", `${STATUS_TEXT[detail.statusLevel]}·${detail.statusText}`],
    ["时间", detail.completedAt ?? detail.sourceTimestamp ?? "未知"],
  ]);
  const boundary = document.createElement("p");
  boundary.className = "privacy-callout";
  boundary.textContent = "本页只读取结构化指标与证据标签，不显示或请求对话正文。行为数据不能独立证明物理模型。";
  const aliasAction = document.createElement("button");
  aliasAction.type = "button";
  aliasAction.className = "text-button";
  aliasAction.dataset.action = "history-alias";
  aliasAction.dataset.historyId = detail.id;
  aliasAction.dataset.focusKey = `history-alias:${detail.id}`;
  aliasAction.textContent = detail.localAlias ? "修改本地别名" : "设置本地别名";
  aliasAction.title = "别名只保存在本机，不读取或复制 Codex 任务标题";
  body.replaceChildren(heading, grid, aliasAction, boundary);
  showDialog("conversation-dialog");
}

async function editHistoryAlias(id: string): Promise<void> {
  const item = state.history.find((entry) => entry.id === id);
  if (!item) return;
  const answer = window.prompt(
    "设置本地别名（最多 80 个字符；留空可清除）。别名只保存在小狸本机数据库中。",
    item.localAlias ?? "",
  );
  if (answer === null) return;
  const alias = answer.trim();
  if (alias.length > 80) {
    showNotice("本地别名不能超过 80 个字符", "yellow");
    return;
  }
  if (MOCK_MODE) {
    item.localAlias = alias || undefined;
    item.displayName = alias || item.displayName;
  } else {
    const result = await command<unknown>("set_conversation_alias", {
      threadId: item.threadId,
      alias: alias || null,
    });
    if (result === undefined) return;
    await loadHistory(false, true);
  }
  document.querySelector<HTMLDialogElement>("#conversation-dialog")?.close();
  renderHistory();
  showNotice(alias ? "本地别名已保存" : "本地别名已清除", "green");
}

async function openReportDetail(auditId: string): Promise<void> {
  let report = state.audits.find((item) => item.auditId === auditId);
  if (!MOCK_MODE) {
    const raw = await command<unknown>("get_relay_audit", { auditId }, true);
    if (raw !== undefined) report = normalizeReport(asRecord(raw).report ?? raw, 0);
  }
  if (!report) return;
  const body = document.querySelector<HTMLElement>(".report-dialog-body");
  if (!body) return;
  const heading = document.createElement("div");
  const title = document.createElement("h3");
  title.style.margin = "0 0 4px";
  title.textContent = `${report.profileLabel}·${report.claimedModel}`;
  const meta = document.createElement("span");
  meta.className = "cell-secondary";
  meta.textContent = `${PROTOCOL_TEXT[report.protocol]}·四轴 ${VERDICT_TEXT[report.overallVerdict]}·置信度 ${auditConfidenceText(report.confidence)}`;
  heading.append(title, meta);
  const axes = document.createElement("div");
  axes.className = "axis-grid";
  axes.append(
    reportAxis("协议兼容", report.protocolFindings),
    reportAxis("计量一致", report.usageReconciliation),
    reportAxis("行为质量", report.qualityFindings),
    reportAxis("模型身份", report.fingerprintFindings),
  );
  const selectiveService = reportSelectiveServiceSection(report.selectiveServiceAssessment);
  const metrics = reportMetricSection(report.quantitativeEvidence);
  const reasons = titledList("判定原因", report.reasons, "本报告没有额外原因记录。");
  const limitations = titledList("证据限制", report.limitations, "未提供限制说明。");
  const boundary = document.createElement("p");
  boundary.className = "privacy-callout";
  boundary.textContent = "即使四轴全部为绿色，也只表示本次范围内与参考一致；真实物理模型未获密码学证明。";
  body.replaceChildren(heading, axes, selectiveService, metrics, reasons, limitations, boundary);
  showDialog("report-dialog");
}

function reportSelectiveServiceSection(assessment: SelectiveServiceAssessment | undefined): HTMLElement {
  const section = document.createElement("section");
  section.className = "selective-service-section";
  const heading = document.createElement("h3");
  heading.textContent = "真实会话对照（独立证据）";
  const card = document.createElement("article");
  const level: StatusLevel = assessment?.state === "suspectedSelectiveService" ? "yellow" : "gray";
  card.className = "axis-card selective-service-card";
  card.dataset.level = level;
  const stateLabel = document.createElement("strong");
  stateLabel.className = `status-${level}`;
  stateLabel.textContent = assessment ? SELECTIVE_SERVICE_TEXT[assessment.state] : "未进行同一中转的真实会话对照";
  card.append(stateLabel);
  if (assessment) {
    card.append(detailGrid([
      ["对照窗口", `最近 ${assessment.windowDays} 天`],
      ["完成回合", `${assessment.sampleCount}`],
      ["保留降质警告", `${assessment.suspiciousCount}${assessment.suspiciousShare === undefined ? "" : ` · ${formatPercent(assessment.suspiciousShare)}`}`],
    ]));
    const details = [
      ...assessment.reasons,
      ...assessment.limitations.map((item) => `限制：${item}`),
    ];
    if (details.length) {
      const disclosure = document.createElement("details");
      disclosure.className = "axis-evidence-details";
      const summary = document.createElement("summary");
      summary.textContent = `查看 ${details.length} 条判定与限制`;
      const list = document.createElement("ul");
      list.className = "reason-list";
      for (const detail of details) {
        const item = document.createElement("li");
        item.textContent = detail;
        list.append(item);
      }
      disclosure.append(summary, list);
      card.append(disclosure);
    }
  }
  const boundary = document.createElement("p");
  boundary.className = "cell-secondary";
  boundary.textContent = "该对照不改写上方四轴结论；工具等负载差异可能影响真实会话指标。";
  section.append(heading, card, boundary);
  return section;
}

function detailGrid(items: Array<[string, string]>): HTMLDListElement {
  const list = document.createElement("dl");
  list.className = "detail-grid";
  for (const [label, value] of items) {
    const container = document.createElement("div");
    container.className = "detail-item";
    const term = document.createElement("dt");
    term.textContent = label;
    const description = document.createElement("dd");
    description.textContent = value;
    container.append(term, description);
    list.append(container);
  }
  return list;
}

function reportAxis(title: string, finding: AxisFinding): HTMLElement {
  const card = document.createElement("article");
  card.className = "axis-card";
  const header = document.createElement("div");
  header.className = "axis-card-header";
  const dot = document.createElement("span");
  dot.className = `status-dot status-${finding.level}`;
  const heading = document.createElement("strong");
  heading.textContent = title;
  const stateLabel = document.createElement("span");
  stateLabel.className = `axis-state status-${finding.level}`;
  stateLabel.textContent = axisStateText(finding);
  header.append(dot, heading, stateLabel);
  const summary = document.createElement("p");
  summary.textContent = finding.summary;
  card.append(header, summary);
  if (finding.details.length) {
    const disclosure = document.createElement("details");
    disclosure.className = "axis-evidence-details";
    const disclosureTitle = document.createElement("summary");
    disclosureTitle.textContent = `查看 ${finding.details.length} 条原因与限制`;
    const list = document.createElement("ul");
    list.className = "reason-list";
    for (const detail of finding.details) {
      const item = document.createElement("li");
      item.textContent = detail;
      list.append(item);
    }
    disclosure.append(disclosureTitle, list);
    card.append(disclosure);
  }
  return card;
}

function reportMetricSection(metrics: Array<{ label: string; value: string }>): HTMLElement {
  const section = document.createElement("section");
  const heading = document.createElement("h3");
  heading.textContent = "定量证据";
  heading.style.margin = "16px 0 7px";
  heading.style.fontSize = "13px";
  if (metrics.length) {
    section.append(heading, detailGrid(metrics.map((item) => [item.label, item.value])));
  } else {
    const empty = document.createElement("p");
    empty.className = "cell-secondary";
    empty.textContent = "本报告没有可显示的结构化定量字段；原因与限制仍保留在下方。";
    section.append(heading, empty);
  }
  return section;
}

function titledList(title: string, items: string[], emptyCopy: string): HTMLElement {
  const section = document.createElement("section");
  const heading = document.createElement("h3");
  heading.textContent = title;
  heading.style.margin = "16px 0 7px";
  heading.style.fontSize = "13px";
  const list = document.createElement("ul");
  list.className = "reason-list";
  for (const value of items.length ? items : [emptyCopy]) {
    const item = document.createElement("li");
    item.textContent = value;
    list.append(item);
  }
  section.append(heading, list);
  return section;
}

function showDialog(id: string): void {
  const dialog = document.querySelector<HTMLDialogElement>(`#${id}`);
  if (!dialog) return;
  if (!dialog.open) {
    dialogReturnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.showModal();
    window.requestAnimationFrame(() => dialog.querySelector<HTMLButtonElement>('[data-action="dialog-close"]')?.focus({ preventScroll: true }));
  }
}

function routeEvidenceText(value: string): string {
  const normalized = value.toLowerCase();
  return normalized.includes("reroute") ? "已捕获显式服务器重路由" : "未见服务器重路由（不证明物理路由未变）";
}

function bindDomEvents(): void {
  mount.addEventListener("click", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const nav = target.closest<HTMLButtonElement>("button[data-page]");
    if (nav) {
      const page = nav.dataset.page as PageId | undefined;
      if (page) {
        state.page = page;
        history.replaceState(null, "", `${window.location.pathname}?page=${page}${MOCK_MODE ? "&mock=1" : ""}`);
        render();
      }
      return;
    }
    const button = target.closest<HTMLButtonElement>("button[data-action]");
    if (!button || button.disabled) return;
    void handleAction(button);
  });
  mount.querySelector<HTMLFormElement>("#relay-profile-form")?.addEventListener("submit", (event) => {
    event.preventDefault();
    void saveProfile();
  });
  mount.querySelector<HTMLFormElement>("#history-filter-form")?.addEventListener("submit", (event) => {
    event.preventDefault();
    void loadHistory(false, false).then(render);
  });
  mount.querySelector<HTMLFormElement>("#schedule-form")?.addEventListener("submit", (event) => void saveSchedule(event));
  mount.querySelector<HTMLFormElement>("#schedule-form")?.addEventListener("input", () => {
    state.scheduleDraftDirty = true;
  });
  mount.addEventListener("change", (event) => {
    const target = event.target;
    if (target instanceof HTMLInputElement && target.name === "audit-mode") renderBudget();
    if (target instanceof HTMLSelectElement && target.id === "audit-baseline") renderBudget();
    if (target instanceof HTMLSelectElement && target.id === "schedule-cadence") {
      state.scheduleDraftDirty = true;
      const weekday = mount.querySelector<HTMLSelectElement>("#schedule-weekday");
      const field = weekday?.closest<HTMLElement>(".field");
      if (field) field.hidden = target.value === "daily";
    }
    if (target instanceof HTMLInputElement && target.id === "schedule-pair-official") {
      state.scheduleDraftDirty = true;
      renderScheduleBudgetState(target.checked);
    }
    if (target instanceof HTMLInputElement && target.id === "schedule-monthly-limit") {
      state.scheduleDraftDirty = true;
      renderScheduleBudgetState();
    }
  });
  mount.querySelector<HTMLInputElement>("#relay-private-probe-path")?.addEventListener("input", () => renderPrivateProbeState());
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    for (const dialog of document.querySelectorAll<HTMLDialogElement>("dialog[open]")) dialog.close();
  });
  for (const dialog of document.querySelectorAll<HTMLDialogElement>("dialog")) {
    dialog.addEventListener("click", (event) => {
      if (event.target === dialog) dialog.close();
    });
    dialog.addEventListener("close", () => {
      const target = dialogReturnFocus;
      dialogReturnFocus = null;
      if (target?.isConnected) target.focus({ preventScroll: true });
    });
  }
}

async function handleAction(button: HTMLButtonElement): Promise<void> {
  const action = button.dataset.action;
  if (action === "refresh") await refreshCurrentPage();
  else if (action === "theme") {
    state.theme = state.theme === "cute" ? "minimal" : "cute";
    applyTheme();
    await command("set_theme", { theme: state.theme }, true);
  } else if (action === "history-reset") {
    mount.querySelector<HTMLFormElement>("#history-filter-form")?.reset();
    await loadHistory(false, false);
    render();
  } else if (action === "history-more") {
    await loadHistory(true, false);
    renderHistory();
  } else if (action === "history-detail" && button.dataset.historyId) await openHistoryDetail(button.dataset.historyId);
  else if (action === "history-alias" && button.dataset.historyId) await editHistoryAlias(button.dataset.historyId);
  else if (action === "profile-new") clearProfileForm();
  else if (action === "profile-edit" && button.dataset.id) {
    const profile = state.profiles.find((item) => item.id === button.dataset.id);
    if (profile) populateProfileForm(profile);
  } else if (action === "profile-delete" && button.dataset.id) await deleteProfile(button.dataset.id);
  else if (action === "private-probe-clear") {
    setInputValue("#relay-private-probe-path", "");
    renderPrivateProbeState();
    showNotice("已从表单清除私有题包；保存配置后生效", "gray");
  }
  else if (action === "toggle-secret") {
    const input = mount.querySelector<HTMLInputElement>("#relay-api-key");
    if (input) {
      const visible = input.type === "text";
      input.type = visible ? "password" : "text";
      button.textContent = visible ? "显示" : "隐藏";
      button.setAttribute("aria-pressed", String(!visible));
    }
  } else if (action === "connection-test") await testConnection();
  else if (action === "audit-start") await startAudit();
  else if (action === "audit-cancel") await cancelAudit();
  else if (action === "report-detail" && button.dataset.auditId) await openReportDetail(button.dataset.auditId);
  else if (action === "report-delete" && button.dataset.id) await deleteReport(button.dataset.id);
  else if (action === "baseline-import") await importBaseline();
  else if (action === "baseline-delete" && button.dataset.id) await deleteBaseline(button.dataset.id);
  else if (action === "dialog-close") button.closest<HTMLDialogElement>("dialog")?.close();
}

async function attachBackendEvents(): Promise<void> {
  if (!IS_TAURI) return;
  const subscriptions: Array<[string, (payload: unknown) => void]> = [
    ["monitor://history-updated", () => { void loadHistory(false, true).then((updated) => { if (updated) renderHistory(); }); }],
    ["monitor://connection-origin", () => { scheduleOverviewReload(); }],
    ["monitor://snapshot", () => { scheduleOverviewReload(); }],
    ["relay://profiles-changed", () => { void loadProfiles(true).then((updated) => { if (updated) renderProfiles(); }); }],
    ["relay://audits-changed", () => {
      auditEventRevision += 1;
      void loadAudits(true).then((updated) => {
        if (updated) {
          renderAudits();
          renderProgress();
        }
      });
    }],
    ["relay://schedule-updated", (payload) => {
      scheduleLoadSerial += 1;
      state.schedule = normalizeSchedule(payload);
      renderScheduleForm();
    }],
    ["relay://audit-progress", (payload) => {
      auditEventRevision += 1;
      auditsLoadSerial += 1;
      const progress = normalizeProgress(payload);
      if (progress) { state.activeAudit = progress; renderProgress(); }
    }],
    ["relay://audit-completed", (payload) => {
      auditEventRevision += 1;
      auditsLoadSerial += 1;
      const envelope = asRecord(payload);
      state.activeAudit = undefined;
      state.loading.delete("audit-cancel");
      if (!envelope.report) {
        render();
        const status = cleanText(envelope.status)?.toLowerCase();
        showNotice(status === "cancelled" ? "审计已取消，未再启动新请求" : "审计未能生成报告，请查看本地状态后重试", status === "cancelled" ? "gray" : "red", 7_000);
        return;
      }
      const reportRaw = { ...asRecord(envelope.report), profileLabel: cleanText(pick(envelope, "profileLabel", "profile_label"), 100) };
      const report = normalizeReport(reportRaw, 0);
      state.audits = [report, ...state.audits.filter((item) => item.auditId !== report.auditId)];
      render();
      const persistenceFailed = cleanText(pick(envelope, "persistenceState", "persistence_state")) === "failed";
      showNotice(
        persistenceFailed
          ? `审计完成：${reportHeadline(report)}；但报告未能写入本地数据库，重启后可能丢失`
          : `审计完成：${reportHeadline(report)}`,
        persistenceFailed ? "yellow" : reportEffectiveLevel(report),
        7_000,
      );
    }],
  ];
  for (const [eventName, handler] of subscriptions) {
    try {
      const unlisten = await listen<unknown>(eventName, (event) => handler(event.payload));
      state.unlisteners.push(unlisten);
    } catch {
      console.warn(`[XiaoLi workbench] 无法订阅 ${eventName}`);
    }
  }
}

function scheduleOverviewReload(): void {
  overviewEventRevision += 1;
  if (overviewReloadActive) {
    overviewReloadTrailing = true;
    return;
  }
  void runOverviewReload();
}

async function runOverviewReload(): Promise<void> {
  overviewReloadActive = true;
  const revision = overviewEventRevision;
  try {
    const updated = await withUiTimeout(loadOverview(true));
    if (!updated || revision !== overviewEventRevision) return;
    renderChrome();
    renderOverview();
  } catch (error) {
    if (error instanceof UiRefreshTimeoutError && revision === overviewEventRevision) {
      overviewLoadSerial += 1;
      showNotice("实时总览更新超时，已保留上一份有效数据", "yellow", 6_000);
    }
  } finally {
    overviewReloadActive = false;
    if (overviewReloadTrailing) {
      overviewReloadTrailing = false;
      void runOverviewReload();
    }
  }
}

function cleanup(): void {
  for (const unlisten of state.unlisteners.splice(0)) unlisten();
  state.credentials.clear();
  setInputValue("#relay-api-key", "");
}

function mockOverview(): WorkbenchOverview {
  const official: ConnectionOriginSnapshot = { kind: "officialChatGpt", authMode: "chatGpt", confidence: "configured", providerId: "openai", endpointClass: "officialOpenAi", evidence: ["session_meta.model_provider = openai", "官方 endpoint 与 auth_mode 匹配"], limitations: ["连接来源不等于物理模型证明"] };
  const custom: ConnectionOriginSnapshot = { kind: "customEndpoint", authMode: "apiKey", confidence: "configured", providerId: "team-gateway", endpointClass: "custom", evidence: ["session_meta.model_provider = team-gateway", "base_url 非 OpenAI 第一方端点"], limitations: ["自定义端点不等于恶意中转"] };
  return {
    checkedAt: new Date(Date.now() - 4_000).toISOString(),
    collectorLevel: "green",
    activeConversationCount: 2,
    officialCount: 1,
    customCount: 1,
    unknownCount: 0,
    totalTokens: 5_827_420,
    cacheInputShare: 0.934,
    dominantOrigin: official,
    axisSummary: {
      protocol: { level: "green", state: "compatible", summary: "最近审计的基础响应、SSE 和错误契约正常", details: [] },
      usage: { level: "yellow", state: "insufficientEvidence", summary: "自定义端点尚未启用实时官方计量配对", details: ["仍可检查算术自洽性"] },
      quality: { level: "gray", state: "learning", summary: "已采集目标端探针，但没有实时官方质量参考", details: ["未启用实时官方配对，不能形成相对质量结论"] },
      identity: { level: "gray", state: "insufficientEvidence", summary: "尚未启用同协议同模型的实时官方配对", details: ["真实物理模型未获证明"] },
    },
    conversations: [
      { threadId: "demo-root-7f31a3", turnId: "demo-turn-1092", displayName: "工作台交互优化", model: "gpt-5.6-sol", effort: "ultra", origin: official, statusLevel: "green", statusText: "请求配置一致，采集正常", totalTokens: 4_961_300, cacheInputShare: 0.947, sourceTimestamp: new Date(Date.now() - 3_500).toISOString(), childCount: 2 },
      { threadId: "demo-root-98b4d1", turnId: "demo-turn-8801", displayName: "中转协议回归", model: "gpt-5.6-sol", effort: "high", origin: custom, statusLevel: "yellow", statusText: "尚未启用实时官方配对", totalTokens: 866_120, cacheInputShare: 0.861, sourceTimestamp: new Date(Date.now() - 11_000).toISOString(), childCount: 0 },
    ],
    recentAlerts: ["中转协议回归：自定义端点已识别，可手动选择匹配的实时官方配对端点。"],
  };
}

function mockHistory(): HistoryEntry[] {
  return mockOverview().conversations.map((item, index) => ({
    ...item,
    id: `history-${index + 1}`,
    startedAt: new Date(Date.now() - (index + 1) * 1_800_000).toISOString(),
    completedAt: new Date(Date.now() - index * 840_000).toISOString(),
    ttftMs: index === 0 ? 3_820 : 7_110,
    outputRate: index === 0 ? 13.4 : 7.8,
    reasoningTokens: index === 0 ? 8_420 : 1_920,
    routeEvidence: "notObserved",
  }));
}

function mockProfiles(): RelayProfile[] {
  const now = new Date().toISOString();
  return [{ id: "profile-demo-gateway", label: "开发环境中转", normalizedBaseUrl: "https://gateway.example.com/v1", protocol: "openAiResponses", defaultModel: "gpt-5.6-sol", createdAt: now, updatedAt: now }];
}

function mockReports(): RelayAuditReport[] {
  return [{
    auditId: "audit-demo-001",
    profileId: "profile-demo-gateway",
    profileLabel: "开发环境中转",
    claimedModel: "gpt-5.6-sol",
    protocol: "openAiResponses",
    startedAt: new Date(Date.now() - 920_000).toISOString(),
    completedAt: new Date(Date.now() - 610_000).toISOString(),
    overallVerdict: "consistent",
    confidence: "中",
    protocolFindings: { level: "green", state: "compatible", summary: "认证、基础响应与 SSE 结构正常", details: [] },
    usageReconciliation: { level: "green", state: "consistent", summary: "usage 算术自洽，未见明确契约矛盾", details: [] },
    qualityFindings: { level: "green", state: "consistent", summary: "配对参考下的六个质量域未见持续异常", details: [] },
    fingerprintFindings: { level: "green", state: "referenceConsistent", summary: "本次参数下与配对参考行为一致", details: ["不等于物理模型身份证明"] },
    reasons: ["本次主动审计的四条证据轴在可比范围内一致。"],
    limitations: ["中转可能识别审计流量并选择性转发真实模型。", "模型更新、system prompt 和提供方差异都会改变行为分布。"],
    quantitativeEvidence: [
      { label: "质量配对有效 case", value: "48" },
      { label: "可比较指纹 cell", value: "16" },
    ],
    selectiveServiceAssessment: {
      state: "suspectedSelectiveService",
      sampleCount: 14,
      suspiciousCount: 8,
      suspiciousShare: 8 / 14,
      windowDays: 30,
      reasons: ["主动审计与匹配参考一致，但 14 个已绑定真实回合中有 8 个保留降质警告。"],
      limitations: ["真实会话的工具、缓存、负载与输入分布与主动审计不同。", "该信号不能证明选择性路由或识别物理模型。"],
    },
  }];
}

function mockBaselines(): RelayBaseline[] {
  return [
    { id: "fpverify-demo-sol", label: "gpt-5.6-sol 公开参考", model: "gpt-5.6-sol", source: "community", version: "demo", sampleCount: 66, createdAt: "2026-07", signed: false, builtIn: true, referenceProtocol: "cursor-harness/harness-battery", scoringMode: "experimentalRelativeRanking", limitations: ["低置信跨协议排名"] },
    { id: "baseline-user-summary", label: "示例参考摘要（不参与评分）", model: "gpt-5.6-sol", protocol: "openAiResponses", source: "user", version: "demo", sampleCount: 240, createdAt: new Date(Date.now() - 5 * 86_400_000).toISOString(), signed: false, builtIn: false, limitations: ["仅用于演示元数据列表；当前 beta scorer 不读取导入摘要"] },
  ];
}

mountShell();
bindDomEvents();
render();
void attachBackendEvents();
void refreshAll(true).then(() => {
  render();
  if (!IS_TAURI && !MOCK_MODE) showNotice("当前是浏览器预览，连接 XiaoLi 本地后端后才会显示真实数据", "gray", 8_000);
});
window.addEventListener("beforeunload", cleanup, { once: true });
