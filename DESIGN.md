# 小狸 XiaoLi v0.1 Design System

## Product intent

小狸（XiaoLi）是一个面向 Windows、macOS 与 Linux 的紧凑 Codex 旁路监视器。界面刻意保持小巧，但必须让开发者在工作时常驻显示仍然清楚、可拖动、可缩放并可操作。

The default `cute` theme uses an original hand-drawn anime-inspired monitor character. The optional `minimal` theme preserves the same layout, interaction model, information hierarchy, and evidence labels while replacing the character with geometric status marks.

## Visual direction

### Hand-drawn theme

- Cream canvas, white surfaces, ink-brown text, dusty violet, mist blue, soft pink, and apricot-gold accents.
- Watercolor-and-ink character rendering rather than game screenshots, franchise art, or large decorative illustrations.
- Four expressions communicate healthy, warning, error, and idle states. Shape, status text, and tooltips always accompany color.
- The compact avatar occupies approximately 32–36 DIP of the visible character area, with enough surrounding space to keep the silhouette legible.
- Rounded cards use a 14 DIP radius. The character treatment is expressive; controls and data remain restrained.

### Minimal theme

- Neutral light-gray canvas and white surfaces.
- An 8 DIP corner radius and geometric status indicator.
- No character expression, while all evidence, token, hierarchy, keyboard, and color semantics remain identical to the hand-drawn theme.

## Semantic color

| Purpose | Value | Meaning |
| --- | --- | --- |
| Healthy | `#2F9C6A` | Request configuration is consistent and collection is healthy |
| Warning | `#B87812` | Pending change, incomplete evidence, parse degradation, or conservative anomaly |
| Error | `#D84861` | Explicit conflict or deterministic collector failure |
| Pending | `#5874D8` | A model or effort change is waiting for the next turn |
| Route evidence | muted blue-violet | Explicit reroute evidence without implying a policy conflict |
| Ink | `#453C48` | Primary text |
| Cream | `#FFF8F3` | Default background |
| Idle | neutral gray | Codex is not running or no active turn exists |

Color is never the only signal. Every state also has a distinct icon or avatar expression, a textual label, and a tooltip or accessible name.

## Typography and readability

Segoe UI Variable is the preferred UI face, with system fallbacks. `ResizeObserver` computes `--ui-scale` from the live window dimensions and clamps it to `0.98–1.18`.

| Information tier | CSS range |
| --- | --- |
| Primary compact model line | 13.5–17 px |
| Conversation title | 12.5–16 px |
| Body and evidence rows | 12–15 px |
| Compact secondary metrics | 12–15 px |
| Metadata | never below 10.5 px |

Interactive controls have a minimum 28 DIP hit area. Resizing may improve readability, but the UI scale cannot shrink text below these floors or enlarge it until it crowds out the evidence labels.

## Window states and sizing

The frameless window is natively draggable and resizable. Compact and expanded bounds are stored independently; restarting returns to compact mode while retaining the user's last bounds for each mode.

| State | Default | Minimum | Maximum |
| --- | --- | --- | --- |
| Compact | 304 × 72 DIP | 280 × 68 DIP | 520 × 120 DIP |
| Expanded | 440 × 500 DIP | 380 × 300 DIP | 760 × 800 DIP, further capped at 90% of the current work area |
| Tray | Window hidden | — | Status icon only |

- The avatar, model row, token row, title whitespace, and six-dot grip are native drag regions.
- Buttons, menus, cards, scrolling content, links, and selectable identifiers explicitly remain interactive and do not initiate dragging.
- A visible lower-right grip advertises native resize support.
- Expand/collapse and hide-to-tray controls remain present in compact mode. Topmost and more-actions controls appear when the control area is hovered or keyboard-focused.
- The window remembers its nearest screen-edge anchor. Expansion grows inward from that edge instead of drifting across the display.
- Position and size are saved after interaction settles, not during every pointer move. Per-monitor DPI and work-area changes clamp the window back onto a usable display without stealing focus.

## Information hierarchy

### Compact state

For one active root conversation, compact mode shows:

1. Status avatar or geometric mark, requested model, requested effort, and a visible `request` label.
2. Deduplicated cumulative token count, cache-input share, and the shortest actionable non-green explanation.
3. A compact route-evidence badge. Unknown routing remains visibly unknown.

For multiple roots, compact mode shows the conversation and severity counts plus deduplicated aggregate usage. It never chooses one conversation's model as a stand-in for all conversations.

### Expanded state and root accordion

The root conversation is the unit of folding:

- When there is exactly one root, it opens automatically the first time expanded mode is entered.
- When there are multiple roots, they start collapsed. The user may open more than one root at the same time.
- A collapsed root retains title, worst descendant status, requested model and effort, cumulative tokens, cache-input share, child count, and event age.
- An expanded root first shows the root turn's evidence, then its indented child conversations and subagents.
- Children may expose their own token, cache, TTFT, reasoning-output, timing, and route evidence details.
- Child warning/error severity bubbles to the root card, compact summary, and tray state in the same snapshot.
- A child whose parent cannot be resolved appears in a visible “parent conversation not found” warning group instead of being dropped.

## Stable scrolling and live updates

Expanded mode has one native vertical scroll container. The application does not intercept or rewrite wheel events.

- Conversation DOM nodes are keyed by `threadId`; live snapshots patch changed text and attributes instead of rebuilding the list.
- High-frequency token updates are coalesced with `requestAnimationFrame`.
- Before structural changes, the renderer records the visible anchor and offset, then restores them after reconciliation.
- Scroll position, keyboard focus, root accordion state, and child-detail state survive background refreshes.
- CSS uses a stable scrollbar gutter and a 12 px pointer lane so the thumb remains practical to grab. Smooth-scroll animation is not forced during data refresh.
- Background collection must not move focus, resize the window, or return the list to the top.

## Model evidence contract

The evidence contract is unchanged by the XiaoLi rename. Visual polish must never weaken it.

- `activeRequest.model` and `activeRequest.effort` are request evidence for the current turn.
- `pendingNextTurn` is a change waiting to become the request value of a later turn; it is not retroactively applied to the active turn.
- A server model is shown only when an explicit `model/rerouted` event or equivalent persisted explicit reroute record exists.
- Without explicit reroute evidence, the interface says “no server reroute observed”. It does not label the requested model as the physical model, and the label does not prove that no physical routing change occurred.
- Effort is always labeled as requested effort. Reasoning tokens do not prove an independently measured thinking level.
- Token, timing, cache, and behavior-baseline signals may cause a conservative warning, but they never fabricate a reroute or model identity.
- Green means the normalized request model and effort are consistent with the effective task settings and the collector is healthy. It does not mean the physical server model was independently verified.
- Explicit reroute evidence uses a neutral route color unless its target creates a defined policy conflict; only then does status become red.
- A yellow `suspectedDegradation` result requires multiple independent, one-sided deviations against a sufficiently large local bucket. It remains statistical behavior evidence, never route evidence.
- Active TTFT is shown only as pending or an estimated window. An exact TTFT label is reserved for a terminal structured report.

## Motion and accessibility

- State changes, hover feedback, and grab feedback use approximately 120 ms transitions.
- There are no looping blink, breathing, bobbing, or floating animations.
- `prefers-reduced-motion` disables nonessential transitions.
- Every icon-only control has an accessible name and tooltip, participates in keyboard focus, and has at least a 28 DIP target.
- Right-click and `Shift+F10` open the same application menu.

## Asset boundary

The character and icon masters were redrawn with image generation from a user-supplied local visual reference. The user confirmed on 2026-08-25 that they hold the rights needed for public redraw and noncommercial distribution. The local reference itself is not included in the repository, build output, portable archive, or installed application. Generation modes, prompt summaries, and transparency repair are recorded in [ASSET_PROVENANCE.md](./ASSET_PROVENANCE.md).

## Open-source reference boundary

The following projects were reviewed only for high-level interaction and organization principles:

- [Win-CodexBar](https://github.com/nesszer/Win-CodexBar): compact Windows float-bar, tray, display, and DPI patterns.
- [Starward](https://github.com/Scighost/Starward) and [Collapse](https://github.com/CollapseLauncher/Collapse): balance between character identity and dense data panels.
- [Animeko](https://github.com/open-ani/animeko): rounded-card and module hierarchy.
- [MaaAssistantArknights](https://github.com/MaaAssistantArknights/MaaAssistantArknights): organization of dense task and runtime states.
- [Mate Engine](https://github.com/shinyflvre/Mate-Engine): character grab-feedback principle only.
- [ccusage](https://github.com/ryoppippi/ccusage) and [Splitrail](https://github.com/Piebald-AI/splitrail): token-accounting and aggregation concepts.

No source code, game character, screenshot, logo, illustration, or other asset from these projects is copied or bundled. They are design references, not redistributed copyright items. Actual runtime dependencies are documented separately in [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md).

## Decision log

- The pre-release prototype established request-versus-route evidence semantics and the compact companion model before the product was renamed XiaoLi.
- XiaoLi v0.1 keeps the 304 × 72 DIP compact default, native dragging and resizing, per-state bounds, and readable font scaling.
- XiaoLi v0.1 makes root conversations the folding unit and preserves child hierarchy, orphan visibility, and descendant severity bubbling.
- XiaoLi v0.1 keeps keyed reconciliation and scroll-anchor restoration, and adds V4 timing and quality evidence.
- The `cute` theme identifier remains compatible and now selects the hand-drawn anime-inspired character system; `minimal` remains available.
