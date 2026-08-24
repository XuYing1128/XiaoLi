# 状态、模型证据与疑似降质

本文解释小狸如何从结构化事件得到每个状态，以及哪些结论不能从当前公开接口得到。若界面与本文冲突，以本文的证据边界和 `MonitorSnapshotV4` 数据为准。

## 四条独立证据线

```text
hook / turn_context ──> activeRequest ──> 本回合请求模型与请求 effort
线程设置变化 ────────> pendingNextTurn ──> 下一回合待生效
model/rerouted ──────> serverRoute ─────> 明确服务器重路由链

token + item timing + 本地历史 ─────────> qualityAssessment（只允许黄色统计提醒）
```

这四条线不会相互冒充。尤其是：

- 请求模型不是服务器物理模型的独立回执。
- `pendingNextTurn` 不会改写已经启动的回合。
- 没有显式 reroute 只表示没有观察到事件。
- 行为特征永远不会创建 `serverRoute`。

## 所有界面状态

### 正常，绿色 `✓`

满足以下条件时显示：

- 当前回合的规范化请求模型与任务生效设置精确一致。
- 请求 effort 与任务生效设置精确一致。
- hook 与 `turn_context` 没有稳定冲突。
- 采集字段完整，没有阻断性错误。
- 没有等待下一回合生效的不同设置。
- 没有满足保守门槛的行为偏离。

绿色只表示“配置一致且采集健康”。它可以和灰色的“未见服务器重路由”徽标同时出现。

### 待确认，黄色 `!`

黄色必须附带具体原因。常见原因：

- 活动回合中选择了不同模型或 effort，正在等待下一回合。
- 请求模型、effort、Token 或结构化时间字段缺失。
- 少数 JSONL 行损坏，但采集器仍能继续。
- 父会话缺失或子任务层级形成环。

黄色不是“已经降级”的同义词。

### 疑似降质，黄色 `≈`

它是 `qualityAssessment.state = suspectedDegradation` 的用户界面名称。必须同时满足：

1. 当前桶有至少 30 个完整、健康、无解析警告、无显式 reroute 的终态样本。
2. 活动回合已经产生至少 128 个输出 Token。
3. 至少两个独立信号族出现方向正确的异常。
4. 每个信号越过本机历史中位数的 `4 × MAD`。
5. 两个检查点相隔至少 2 秒，且输出增加至少 64 Token。
6. 两个检查点连续命中。

参与投票的单向信号只有：

| 信号 | 异常方向 | 为什么只看这个方向 |
| --- | --- | --- |
| TTFT | 更高 | 更快的首响应不是降质证据 |
| 模型阶段输出速率 | 更低 | 更快的速率不是降质证据 |
| reasoning/output 比例 | 更低 | 更多推理输出不是 effort 降低证据 |
| 推理阶段时长占比 | 更低 | 更长的推理阶段不是 effort 降低证据 |

缓存输入占比不投票，因为缓存变化本身可能显著改变速度。工具等待也不进入模型阶段速率分母。

MAD 是“中位绝对偏差”，用于描述本机历史分布的典型波动。如果某项 MAD 为零、字段缺失或不可比较，该项不投票。黄色提示需要两个健康检查点才清除，以免状态闪烁。

展开“疑似降质”会显示：

- 当前观测值。
- 同桶历史中位数。
- MAD。
- 偏离倍数与方向。
- 样本数。
- 连续命中次数。
- 限制说明。

### 异常，红色 `×`

红色只用于明确冲突或确定性故障：

- 同一个 turn 的 hook 请求值与 `turn_context` 在稳定后明确不同。
- 捕获到显式服务器重路由，目标违反该任务的选择策略。
- 采集器发生无法继续增量读取、存储或发布快照的确定性故障。

显式 reroute 本身默认使用中性蓝紫色。只有目标与策略冲突时才冒泡为红色。

### 空闲，灰色 `–`

Codex 没有运行，或当前没有活动回合。完成记录不会保留在主列表中；合格终态样本只进入本地行为基线。

### 刷新中、已合并、刷新超时与刷新失败 `⟳`

这三个是交互提示，不是模型证据或采集器主状态：

- `刷新中`：单飞后台任务正在扫描，界面仍可拖动、滚动或隐藏。
- `已合并重复刷新`：点击与已在运行的刷新合并，后台最多再补一次尾随刷新。
- `本次刷新超时`：15 秒内未向按钮返回；按钮恢复，上一份有效快照保留。它不会伪装成红色采集故障。
- `刷新失败：原因`：后台命令明确返回错误；按钮恢复并保留上一份有效快照，界面只显示经过截断的精简原因。它表示这次交互没有完成，不会改变或猜测服务器模型。

### 服务器已重路由，蓝紫 `↝`

只有明确 `model/rerouted` 事件能触发。显示格式：

```text
Sol（请求） → 5.5（服务器已重路由）
```

多次 reroute 会保留有序链。Tooltip 提供事件时间与经过脱敏的关联方式；兼容日志不保存原始原因正文。

### 未见服务器重路由，灰色 `◇`

固定含义：小狸没有捕获明确的 `model/rerouted` 事件。它不等价于以下任何结论：

- 服务器确认使用了请求模型。
- 服务器从未发生物理路由变化。
- 请求没有经过安全缓冲。
- effort 得到了服务器独立确认。

### 下一回合待生效，蓝色 `◷`

如果在 Terra 回合活动期间选择 Sol，界面立即显示：

```text
本回合：Terra · high（请求）
下一回合：Sol · ultra（待生效）
```

当前回合的后续片段继续属于 Terra 请求。只有新的 hook 或 `turn_context` 开始下一回合后，Sol 才成为 `activeRequest`。

### 学习中与行为一致

- `learning`：同桶合格样本不足 30 个；不做异常结论，也不会仅因为正在学习就把主状态变黄。
- `consistent`：样本足够，当前检查点没有越过保守触发门槛。

这两个状态都不提供服务器模型身份认证。

### 采集徽标

展开页右上角的采集徽标有五类文案：

- `采集正常`：hook 与 rollout 增量采集器工作正常。
- `N 个解析警告`：本次或当前活动回合有无法解析的结构化行；仍能解析的证据继续保留。
- `采集待确认`：字段或采集状态部分不完整，但采集器仍可继续。
- `采集故障`：采集器报告确定性错误，Tooltip 显示脱敏后的原因。
- `Codex 未运行` / `采集状态未知`：运行环境尚不可用或采集器尚未报告健康状态。

这些文案只描述采集链路，不证明服务器模型身份。

### 指标等待态

- `等待首段`：尚未完成首个模型 item，无法形成 TTFT 估算区间。
- `等待输出`：尚无输出 Token，端到端速率不可计算。
- `等待模型片段`：尚无完整 Reasoning/AgentMessage 结构化区间，模型阶段速率不可计算。

等待态不会显示伪造的 `0`，也不会单独把任务判为异常。

## 可选的 5.5 行为比较

只有本机同时拥有不少于 30 个明确“请求 gpt-5.5”的同桶健康样本，并且至少三个共同指标可比较时，才会计算对照距离。如果到 5.5 基线的稳健距离比到当前请求模型基线至少低 30%，界面可以追加：

> 行为统计上更接近本机 gpt-5.5 请求样本。

这仍然不是“实际模型为 5.5”。网络、服务负载、输入类型和版本变化都可能产生相似分布。

## Token 和缓存字段

- `last_token_usage` 优先作为本次增量。
- `total_token_usage` 校验累计值，并在增量缺失时安全差分。
- `reasoning_output_tokens` 是输出 Token 的子集，不重复加入总量。
- `cached_input_tokens` 是输入 Token 的子集，不重复加入总量。
- `缓存输入比例 = cached_input_tokens / input_tokens`，不是服务端请求命中率。
- 根会话汇总自身和所属子任务，并按线程/回合去重。

## TTFT 和速率

| 显示 | 数据来源 | 准确称呼 |
| --- | --- | --- |
| `等待首段` | 首个模型 item 尚未完成 | 尚无可显示区间 |
| `约 A–B` | 首个 Reasoning/AgentMessage item 的开始与完成时间 | 活动 TTFT 估算区间 |
| 精确毫秒值 | `task_complete.time_to_first_token_ms` | 终态精确 TTFT |
| 端到端输出速率 | `output_tokens / elapsed_or_duration` | 含排队、网络、工具等待的观测值 |
| 模型阶段速率（估算） | `output_tokens / union(model_item_intervals)` | 排除工具区间后的本地估算，不是纯生成 TPS |

采集器只读取 `item_completed` 的 item 类型、ID 和结构化时间，不读取或保存推理、回复、命令和工具正文。

## `MonitorSnapshotV4` 核心结构

```text
MonitorSnapshotV4
  schemaVersion = 4
  checkedAt
  codexRunning
  collectorHealth
  conversations[]
    threadId / turnId / parentThreadId / kind / title
    activeRequest { model, effort, source }
    pendingNextTurn { model, effort }?
    serverRoute { model?, evidence, observedAt?, chain[] }
    usage { last, cumulative, cacheInputShare, contextWindow }
    timing
      elapsedMs / durationMs?
      ttftMs?                       # 仅精确终态
      ttftEvidence { kind, lowerMs?, upperMs? }
      modelActiveMs?
      endToEndOutputRate?
      modelPhaseOutputRate?
      observedOutputRate?           # 兼容别名
    qualityAssessment
      state / baselineKey / baselineSampleCount / consecutiveHits
      factors[] / comparator? / limitations[]
    status { level, code, explanation }
    anomalies[]                     # 简短兼容文案
```

`serverRoute.evidence` 只使用：

- `notObserved`
- `explicitReroute`

具体关联来源保存在 route chain 的 `association` 字段中，不混入证据等级。

## 相关文档

- [快速开始](GETTING_STARTED.md)
- [CLI 与插件参考](CLI_AND_PLUGIN.md)
- [隐私说明](PRIVACY.md)
- [故障排查](TROUBLESHOOTING.md)
