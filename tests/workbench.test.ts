import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type TauriEvent = { payload: unknown };
type TauriEventHandler = (event: TauriEvent) => void;

const tauri = vi.hoisted(() => {
  const listeners = new Map<string, TauriEventHandler>();
  return {
    invoke: vi.fn<(name: string, args?: Record<string, unknown>) => Promise<unknown>>(),
    listen: vi.fn(async (eventName: string, handler: TauriEventHandler) => {
      listeners.set(eventName, handler);
      return () => listeners.delete(eventName);
    }),
    listeners,
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauri.listen }));

const relayProfile = {
  id: "profile-relay",
  label: "测试中转",
  normalizedBaseUrl: "https://relay.example.test/v1",
  protocol: "openAiResponses",
  defaultModel: "gpt-5.6-sol",
  createdAt: "2026-08-27T00:00:00.000Z",
  updatedAt: "2026-08-27T00:00:00.000Z",
};

const baseSchedule = {
  enabled: false,
  cadence: "weekly",
  weekday: 1,
  localTime: "20:00",
  pairOfficial: false,
  monthlyRequestLimit: 1_000,
  monthlyReservedRequests: 0,
  historyRetentionDays: 180,
};

const origin = {
  kind: "customEndpoint",
  authMode: "apiKey",
  confidence: "configured",
  evidence: ["test fixture"],
  limitations: ["test only"],
};

function historyEntry(id: string, displayName: string) {
  return {
    id,
    threadId: `thread-${id}`,
    turnId: `turn-${id}`,
    displayName,
    model: "gpt-5.6-sol",
    effort: "high",
    connectionOrigin: origin,
    statusLevel: "green",
    statusText: "采集正常",
    totalTokens: 1_024,
    cacheInputShare: 0.5,
    completedAt: "2026-08-27T00:00:00.000Z",
    routeEvidence: "notObserved",
  };
}

let previewResult: Record<string, unknown>;
let startResult: Promise<unknown> | unknown;
let historyGeneration: "initial" | "refreshed";

function defaultPreview(overrides: Record<string, unknown> = {}) {
  return {
    builtInRequests: 140,
    privateProbeRequests: 0,
    plannedRequests: 140,
    conservativeInputTokens: 800_000,
    conservativeOutputTokens: 80_000,
    privateProbeInputTokens: 0,
    privateProbeOutputTokens: 0,
    fitsDeclaredBudget: true,
    ...overrides,
  };
}

function historyPage(offset: number) {
  if (offset > 0) {
    return {
      history: [historyEntry("history-2", "第二条（重复）"), historyEntry("history-3", "第三条")],
      total: 3,
    };
  }
  return {
    history: [
      historyEntry("history-1", historyGeneration === "refreshed" ? "第一条（已刷新）" : "第一条"),
      historyEntry("history-2", "第二条"),
    ],
    total: 3,
  };
}

function installInvokeFixture(): void {
  tauri.invoke.mockImplementation(async (name, args) => {
    switch (name) {
      case "get_workbench_overview":
        return {
          checkedAt: "2026-08-27T00:00:00.000Z",
          collectorLevel: "green",
          activeConversationCount: 0,
          dominantOrigin: origin,
          conversations: [],
        };
      case "list_conversation_history": {
        const filter = (args?.filter ?? {}) as Record<string, unknown>;
        return historyPage(Number(filter.offset ?? 0));
      }
      case "list_relay_profiles":
        return { profiles: [relayProfile] };
      case "list_relay_audits":
        return { audits: [], activeRuns: [] };
      case "list_relay_baselines":
        return { baselines: [], builtInCommunityBaselines: [] };
      case "get_audit_schedule":
        return baseSchedule;
      case "upsert_relay_profile":
        return { profile: args?.profile };
      case "preview_relay_audit_plan":
        return previewResult;
      case "start_relay_audit":
        return await startResult;
      default:
        throw new Error(`Unexpected Tauri command in UI test: ${name}`);
    }
  });
}

async function boot(page: "relay" | "history" = "relay"): Promise<void> {
  window.history.replaceState(null, "", `/workbench.html?page=${page}`);
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    value: {},
    configurable: true,
  });
  document.body.innerHTML = '<div id="workbench"></div>';
  await import("../src/workbench.ts");
  await vi.waitFor(() => {
    expect(document.querySelector('[data-action="profile-edit"][data-id="profile-relay"]')).not.toBeNull();
    expect(tauri.listeners.has("relay://audit-progress")).toBe(true);
  });
}

function click(selector: string): void {
  const element = document.querySelector<HTMLElement>(selector);
  if (!element) throw new Error(`Missing test element: ${selector}`);
  element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
}

function prepareRelayDraft(): void {
  click('[data-action="profile-edit"][data-id="profile-relay"]');
  const key = document.querySelector<HTMLInputElement>("#relay-api-key");
  if (!key) throw new Error("Missing API key input");
  key.value = "test-key-kept-in-memory";
}

function emit(eventName: string, payload: unknown): void {
  const listener = tauri.listeners.get(eventName);
  if (!listener) throw new Error(`Missing Tauri event listener: ${eventName}`);
  listener({ payload });
}

async function waitForAuditPreparation(): Promise<void> {
  await vi.waitFor(() => {
    expect(tauri.invoke).toHaveBeenCalledWith("preview_relay_audit_plan", expect.any(Object));
  });
}

beforeEach(() => {
  vi.resetModules();
  tauri.invoke.mockReset();
  tauri.listen.mockClear();
  tauri.listeners.clear();
  historyGeneration = "initial";
  previewResult = defaultPreview();
  startResult = {
    auditId: "audit-default",
    phase: "protocol",
    completedCases: 0,
    totalCases: 150,
    usedRequests: 0,
    tokenEstimate: 0,
    currentDetector: "protocol",
  };
  installInvokeFixture();
  Object.defineProperty(window, "confirm", {
    value: vi.fn(() => true),
    configurable: true,
    writable: true,
  });
});

afterEach(() => {
  window.dispatchEvent(new Event("beforeunload"));
  document.body.replaceChildren();
});

describe("XiaoLi workbench audit safeguards", () => {
  it("previews the complete plan and never starts when the plan exceeds its declared budget", async () => {
    previewResult = defaultPreview({
      plannedRequests: 151,
      conservativeInputTokens: 1_200_001,
      // The renderer must independently enforce the hard limits even if a
      // malformed or stale backend receipt claims the plan fits.
      fitsDeclaredBudget: true,
    });
    const confirm = vi.mocked(window.confirm);
    await boot();
    prepareRelayDraft();

    click('[data-action="audit-start"]');
    await waitForAuditPreparation();
    await vi.waitFor(() => {
      expect(document.querySelector(".notice-text")?.textContent).toContain("超出已确认预算");
    });

    expect(tauri.invoke).not.toHaveBeenCalledWith("start_relay_audit", expect.anything());
    expect(tauri.invoke).toHaveBeenCalledWith(
      "preview_relay_audit_plan",
      expect.objectContaining({
        request: expect.objectContaining({ maxRequests: 150, maxInputTokens: 1_200_000, maxOutputTokens: 120_000 }),
      }),
    );
    expect(confirm).not.toHaveBeenCalled();
    expect(document.querySelector<HTMLButtonElement>('[data-action="audit-start"]')?.disabled).toBe(false);
  });

  it("does not start or spend quota when the user declines the final budget confirmation", async () => {
    const confirm = vi.mocked(window.confirm);
    confirm.mockReturnValue(false);
    await boot();
    prepareRelayDraft();

    click('[data-action="audit-start"]');
    await waitForAuditPreparation();
    await vi.waitFor(() => {
      expect(document.querySelector(".notice-text")?.textContent).toContain("没有产生请求或额度消耗");
    });

    expect(confirm).toHaveBeenCalledTimes(1);
    expect(confirm.mock.calls[0]?.[0]).toContain("完整计划：140 次 / 端点");
    expect(tauri.invoke).not.toHaveBeenCalledWith("start_relay_audit", expect.anything());
  });

  it("keeps a newer progress event when the start command returns its older initial receipt", async () => {
    let resolveStart!: (value: unknown) => void;
    startResult = new Promise((resolve) => { resolveStart = resolve; });
    await boot();
    prepareRelayDraft();

    click('[data-action="audit-start"]');
    await vi.waitFor(() => {
      expect(tauri.invoke).toHaveBeenCalledWith("start_relay_audit", expect.any(Object));
    });

    emit("relay://audit-progress", {
      auditId: "audit-race",
      phase: "fingerprint",
      completedCases: 42,
      totalCases: 150,
      usedRequests: 41,
      tokenEstimate: 4_096,
      currentDetector: "fingerprint",
    });
    expect(document.querySelector(".progress-count")?.textContent).toBe("42 / 150");

    resolveStart({
      auditId: "audit-race",
      phase: "protocol",
      completedCases: 0,
      totalCases: 150,
      usedRequests: 0,
      tokenEstimate: 0,
      currentDetector: "protocol",
    });
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLButtonElement>('[data-action="audit-start"]')?.getAttribute("aria-busy")).toBe("false");
    });

    expect(document.querySelector(".progress-count")?.textContent).toBe("42 / 150");
    expect(document.querySelector(".progress-requests")?.textContent).toBe("41 次请求");
    expect(document.querySelector(".progress-detector")?.textContent).toBe("分布指纹");
  });
});

describe("XiaoLi workbench history stability", () => {
  it("preserves the focused row and scroll anchor across a live refresh, then deduplicates pagination", async () => {
    await boot("history");
    const firstDetail = document.querySelector<HTMLButtonElement>('[data-focus-key="history-detail:history-1"]');
    const scrollContainer = document.querySelector<HTMLElement>(".history-table-wrap");
    expect(firstDetail).not.toBeNull();
    expect(scrollContainer).not.toBeNull();
    firstDetail?.focus();
    if (scrollContainer) {
      scrollContainer.scrollTop = 137;
      scrollContainer.scrollLeft = 11;
    }

    historyGeneration = "refreshed";
    emit("monitor://history-updated", undefined);
    await vi.waitFor(() => {
      expect(document.querySelector(".history-body")?.textContent).toContain("第一条（已刷新）");
    });

    expect((document.activeElement as HTMLElement | null)?.dataset.focusKey).toBe("history-detail:history-1");
    expect(scrollContainer?.scrollTop).toBe(137);
    expect(scrollContainer?.scrollLeft).toBe(11);

    click('[data-action="history-more"]');
    await vi.waitFor(() => {
      expect(document.querySelectorAll(".history-body tr")).toHaveLength(3);
    });
    expect(document.querySelector(".history-body")?.textContent).toContain("第三条");
    expect(document.querySelectorAll('[data-focus-key="history-detail:history-2"]')).toHaveLength(1);
    expect(document.querySelector<HTMLButtonElement>('[data-action="history-more"]')?.hidden).toBe(true);
    expect(tauri.invoke).toHaveBeenCalledWith(
      "list_conversation_history",
      expect.objectContaining({ filter: expect.objectContaining({ offset: 2 }) }),
    );
  });
});
