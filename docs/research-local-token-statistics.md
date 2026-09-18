# 本地 Token 统计实现调研

调研日期：2026-09-17
范围：`ccusage`、`costats`、`Clawdmeter`、`OpenTokenUsage` 的当前公开源码、README、许可证，以及当前 `token-tray` 的 CC Switch SQLite 读取实现。
源码版本：下文外部源码链接尽量固定到 2026-09-17 调研时的 commit；上游日志格式属于应用内部实现，未来可能变化。

## 结论先行

1. 本地文件统计的核心不是简单地“逐行把 `total_tokens` 相加”，而是先区分事件类型：Claude 的 assistant 流式记录通常重复同一 `message.id`，Codex 的 `total_token_usage` 是会反复出现的累计快照。
2. `ccusage` 的 Claude 实现较完整：支持默认目录和 `CLAUDE_CONFIG_DIR`，按 `message.id + requestId + sessionId` 去重，处理 sidechain 重放，并把 `usage` 的四类 token 汇总到日报。坏 JSONL 行会跳过；正在写入的最后半行不会阻塞整次扫描，但源码没有显式的共享读锁或快照机制。
3. `costats` 同时有两条路径：`UsageLogScanner` 面向 5 小时/7 天窗口；`LogDigestor` 面向最近 30 个日历日，按本地日期、模型聚合。它对 Codex 累计快照做 delta，对 Claude 以 `messageId + requestId` 去重；文件以 `FileShare.ReadWrite` 打开、限制单行 512 KiB、坏行跳过。
4. `Clawdmeter` 的 token 解析最明确地处理了 Claude 流式快照：同一 assistant message 的每个 token bucket 取最大值，再以最早 timestamp 归属窗口；它还用 `(size, mtime)` 做文件级缓存，并跨文件消除 session resume 重放。它的 headline “work” 是 `input + output`，cache token 另行展示，不等于 `total`。
5. `OpenTokenUsage` 没有在插件中重新实现日志解析；它调用固定版本的 `ccusage ... daily --json`，消费 `{ daily: [...] }`，按 `date`、`totalTokens` 和 `modelBreakdowns` 展示 31 天趋势。因此它继承 ccusage 的去重、格式和误差边界，也增加了外部 package runner、版本和超时风险。
6. 对 `token-tray`，最小风险方案是保留 CC Switch SQLite 作为默认来源，新增一个隔离的“本地 JSONL 估算来源”适配器，不把两种来源的数值直接混加；先支持一类 provider，明确标记 `source`、时区、错误和“估算”状态。

## 1. 对照表

| 项目 | 本地路径/入口 | 事件与 token 字段 | 去重/累计快照 | 按日聚合 | 正在写入时的处理 | 许可证 |
|---|---|---|---|---|---|---|
| `ccusage` | Claude：`~/.config/claude/projects/`、`~/.claude/projects/`；`CLAUDE_CONFIG_DIR` 可覆盖并用逗号分隔多目录 | Claude JSONL：顶层 `timestamp`、`sessionId`、`requestId`、`isSidechain`；`message.id`、`message.model`、`message.usage.*` | Claude 按 message/request/session 去重；重复候选保留 token 总量更大的记录；sidechain 重放特殊处理 | timestamp 按配置时区转 `YYYY-MM-DD`，再按 date/project/model 累计 | 整文件读取；只解析包含 `"usage":{"` 的行；反序列化失败的行跳过 | MIT（`apps/ccusage/LICENSE`） |
| `costats` | Codex：`~/.codex/sessions` 或 `CODEX_HOME/sessions`，另扫 `archived_sessions`；Claude：`~/.claude/projects`、`~/.config/claude/projects` 或 `CLAUDE_CONFIG_DIR` | Claude `assistant.message.usage`；Codex `event_msg.payload.type=token_count`，读取 `info.last_token_usage` 或 `info.total_token_usage` | Claude `messageId + requestId`；Codex 每文件/会话保存上一累计值，使用当前值减上一值 | `LogDigestor` 统计最近 30 个日历日，按 `DateOnly + model`；普通 ISO timestamp 快速路径直接取前 10 位 | `FileShare.ReadWrite`；单行 512 KiB 上限；坏 JSON、超长行和文件访问错误跳过 | MIT |
| `Clawdmeter` | `Path.home()/.claude/projects/**/*.jsonl`；未在 token parser 中看到 `CLAUDE_CONFIG_DIR` 覆盖 | `assistant.message.usage`：`input_tokens`、`output_tokens`、`cache_read_input_tokens`、`cache_creation_input_tokens` | 同一 `message.id` 的多个记录按每 bucket 取 max；session resume 跨文件重放也在全局扫描时折叠；保留最早时间 | Stats 扫描 lifetime，再以本地 `datetime.fromtimestamp(ts).date()` 建 `day_tokens`；窗口统计另用 5h/7d | `json.loads` 失败跳过；`OSError` 返回空；文件按 `(size, mtime)` 缓存 | MIT |
| `OpenTokenUsage` | 插件把 `CLAUDE_CONFIG_DIR` 作为 `homePath` 传给 ccusage；自身不直接遍历 transcript | `host.ccusage.query()` 的 `{daily:[...]}`；常用 `date`、`totalTokens`、`totalCost`、`modelBreakdowns` | 继承 ccusage；插件只累计模型 breakdown，不做原始事件去重 | 查询“今天 + 前 30 天”，按 ccusage 日报排序后取最近 31 天 | runner 状态为 `no_runner`/`runner_failed`；host 从命令输出中提取最后一个合法 JSON | MIT（fork 仓库许可证） |

外部一手来源索引：

- [`ccusage README`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/README.md)、[`Claude paths.rs`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/claude/src/paths.rs)、[`Claude lib.rs`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/claude/src/lib.rs)、[`Claude daily.rs`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/claude/src/daily.rs)、[`Claude types.rs`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/crates/ccusage-core/src/types.rs)、[`Claude source README`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/claude/src/README.md)、[`LICENSE`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/apps/ccusage/LICENSE)。
- [`costats README`](https://github.com/fmdz387/costats/blob/39f1f088e35c0ad66d365945b0b9b844dc2a0090/README.md)、[`UsageLogScanner.cs`](https://github.com/fmdz387/costats/blob/39f1f088e35c0ad66d365945b0b9b844dc2a0090/src/costats.Infrastructure/Usage/UsageLogScanner.cs)、[`LogDigestor.cs`](https://github.com/fmdz387/costats/blob/39f1f088e35c0ad66d365945b0b9b844dc2a0090/src/costats.Infrastructure/Expense/LogDigestor.cs)、[`ExpenseAnalyzer.cs`](https://github.com/fmdz387/costats/blob/39f1f088e35c0ad66d365945b0b9b844dc2a0090/src/costats.Infrastructure/Expense/ExpenseAnalyzer.cs)、[`LICENSE`](https://github.com/fmdz387/costats/blob/39f1f088e35c0ad66d365945b0b9b844dc2a0090/LICENSE)。
- [`Clawdmeter transcript.py`](https://github.com/weltern/Clawdmeter/blob/1083e89fbc87d6eb1fa8040c5af0be13fe4abf89/src/transcript.py)、[`stats.py`](https://github.com/weltern/Clawdmeter/blob/1083e89fbc87d6eb1fa8040c5af0be13fe4abf89/src/stats.py)、[`usage_history.py`](https://github.com/weltern/Clawdmeter/blob/1083e89fbc87d6eb1fa8040c5af0be13fe4abf89/src/usage_history.py)、[`README`](https://github.com/weltern/Clawdmeter/blob/1083e89fbc87d6eb1fa8040c5af0be13fe4abf89/README.md)、[`LICENSE`](https://github.com/weltern/Clawdmeter/blob/1083e89fbc87d6eb1fa8040c5af0be13fe4abf89/LICENSE)。
- [`OpenTokenUsage Claude plugin`](https://github.com/PowerUserZ/OpenTokenUsage/blob/1f9eec12fd04368a6bfc2c371a57a6c11eef96b4/plugins/claude/plugin.js)、[`ccusage host API`](https://github.com/PowerUserZ/OpenTokenUsage/blob/1f9eec12fd04368a6bfc2c371a57a6c11eef96b4/docs/plugins/api.md)、[`host_api.rs`](https://github.com/PowerUserZ/OpenTokenUsage/blob/1f9eec12fd04368a6bfc2c371a57a6c11eef96b4/src-tauri/src/plugin_engine/host_api.rs)、[`LICENSE`](https://github.com/PowerUserZ/OpenTokenUsage/blob/1f9eec12fd04368a6bfc2c371a57a6c11eef96b4/LICENSE)。

## 2. `ccusage`：事件级去重 + 日报

### 2.1 文件路径与发现

当前 Claude adapter 默认检查：

- `~/.config/claude/projects/`
- `~/.claude/projects/`

设置 `CLAUDE_CONFIG_DIR` 后，默认路径被替换；变量可以是一个目录或逗号分隔的多个目录。传入的路径如果本身就是 `projects/`，源码会向上规范化。`projects/` 下递归扫描 `.jsonl`，兼容：

```text
projects/{project}/{sessionId}/{file}.jsonl
projects/{project}/{sessionId}.jsonl
```

来源：[`paths.rs`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/claude/src/paths.rs)、[`Claude source README`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/claude/src/README.md)。

### 2.2 Claude JSONL 事件与 token 字段

源码的结构化模型是：

```json
{
  "timestamp": "2026-01-09T10:00:00.000Z",
  "sessionId": "session-alpha",
  "requestId": "req-alpha-1",
  "isSidechain": false,
  "message": {
    "role": "assistant",
    "id": "msg-alpha-1",
    "model": "claude-sonnet-4-20250514",
    "usage": {
      "input_tokens": 100,
      "output_tokens": 50,
      "cache_creation_input_tokens": 25,
      "cache_read_input_tokens": 10,
      "cache_creation": {
        "ephemeral_5m_input_tokens": 0,
        "ephemeral_1h_input_tokens": 25
      }
    }
  },
  "costUSD": 0.12,
  "version": "1.0.0"
}
```

`TokenUsageRaw` 读取四类主字段：

- `input_tokens`
- `output_tokens`
- `cache_read_input_tokens`
- `cache_creation_input_tokens`

如果存在嵌套的 `cache_creation`，cache write 使用 `ephemeral_5m_input_tokens + ephemeral_1h_input_tokens`，否则回退到 `cache_creation_input_tokens`。`totalTokens` 是这四类 token 加总；`costUSD` 只用于成本展示，不是 token 计数。源码 fixture 也确认了上述 JSONL shape。

此外，当前代码会从 `message.usage.iterations` 中识别 `type=advisor_message` 的 advisor 记录，并按其自己的 model 单独加入；`isSidechain=true` 的记录可能是旁路对话或 parent message 重放，不能简单按行累加。

来源：[`types.rs`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/crates/ccusage-core/src/types.rs)、[`lib.rs` 的 read loop](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/claude/src/lib.rs)、[`fixture`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/apps/ccusage/test/fixtures/claude/projects/project-alpha/session-alpha/chat.jsonl)。

### 2.3 去重与“累计快照”

Claude 路径不是按累计快照做 delta，而是先构造 usage entry，再去重：

- 首选 identity 是 `message.id + requestId + 有效 sessionId`。
- 同一 key 出现多个候选时，优先保留四类 token 加总更大的候选；如果总量相同，优先保留带 `speed` 的记录。
- 如果 `requestId` 缺失，会退化为 `message.id + sessionId`；sidechain 重放还会用 timestamp/session 规则匹配，避免把 parent message 的 cache read 再算一遍。
- 不同 `message.id` 的真实 sidechain 响应仍然计入。

这是比“看到同一个 `message.id` 就丢掉后续行”更稳妥的策略，因为流式记录的 usage 可能逐步增长，后出现的重放副本也可能比原始记录完整。

### 2.4 日聚合与写入容错

日报路径为每个有效 entry 解析 timestamp，根据 `--timezone` 或本地配置生成 date，再按 `date` 或 `(date, project)` 分组，对每个 entry 的四类 token和成本求和。周/月视图是在日报或 summary 行之上按 ISO 日期截取/计算 bucket，并非依赖另一个持久化数据库。

文件处理的实际行为：

- 先用字节 marker `"usage":{"` 过滤不可能是 usage 的行，再做 typed `serde_json` 解析。
- `fs::read(path)` 一次读完整文件；文件读取失败直接跳过该文件。
- 单行 JSON 解析失败、timestamp 无法解析、字段结构不合法时跳过该行。
- 正在追加的最后半行通常会因为 JSON 不完整而被跳过；下次扫描文件变完整后会重新读取。源码没有 `FileShare.ReadWrite`、文件锁、增量 offset 或“读到最后完整 newline 为止”的显式机制，因此这属于宽松容错，不是严格一致性快照。

来源：[`daily.rs`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/claude/src/daily.rs)、[`common/jsonl.rs`](https://github.com/ccusage/ccusage/blob/3c5556a775ebcf1e59844d4283c8c5b30529c290/rust/adapters/common/src/jsonl.rs)。

## 3. `costats`：Codex delta + Claude 日切片

### 3.1 两条统计路径

`costats` 的 README 展示“每日 token + cost”和“30 日滚动 token + cost”，源码实际由两部分组成：

- `UsageLogScanner`：给托盘/额度状态提供 5 小时和 7 天窗口。
- `ExpenseAnalyzer -> LogDigestor`：读取最近 30 个日历日，输出 `DailyBreakdown`，再计算今天、30 日窗口和成本。

因此不能只看 `UsageLogScanner.cs` 就得出“costats 没有日报”；日报在 `LogDigestor.cs`。

### 3.2 Claude 格式、token 和去重

`LogDigestor` 只接受 `type=assistant` 且有 `message.usage` 的 JSONL 行，读取：

```text
timestamp
message.model
message.id
message.usage.input_tokens
message.usage.output_tokens
message.usage.cache_read_input_tokens
message.usage.cache_creation_input_tokens
requestId
```

它将 Claude token 放入 `TokenLedger` 的 `StandardInput`、`CachedInput`、`CacheWriteInput`、`GeneratedOutput`，按 `(DateOnly, model)` 聚合；若同时有 `message.id` 与 `requestId`，重复的 `(messageId, requestId)` 会被跳过。

### 3.3 Codex 格式与累计快照 delta

`UsageLogScanner` 和 `LogDigestor` 都寻找：

```json
{
  "type": "event_msg",
  "timestamp": "2026-01-09T10:00:00Z",
  "payload": {
    "type": "token_count",
    "info": {
      "total_token_usage": {
        "input_tokens": 1200,
        "cached_input_tokens": 800,
        "output_tokens": 100
      }
    }
  }
}
```

优先级是：

1. 有 `last_token_usage` 时，直接把它当作本次增量。
2. 只有 `total_token_usage` 时，保存当前累计值，计算 `current - previous`。
3. 对累计值回退或异常负差值使用 `Math.Max(0, delta)`；因此 session 重置不会产生负数，但如果无法识别新 session，也可能把重置后的值当成零增量。

字段兼容 `cached_input_tokens` 与 `cache_read_input_tokens`；输入、cache、输出的 delta 加入当天与 30 日模型切片。`session_meta`/`turn_context` 用来补足会话身份或模型上下文。

来源：[`UsageLogScanner.cs`](https://github.com/fmdz387/costats/blob/39f1f088e35c0ad66d365945b0b9b844dc2a0090/src/costats.Infrastructure/Usage/UsageLogScanner.cs)、[`LogDigestor.cs`](https://github.com/fmdz387/costats/blob/39f1f088e35c0ad66d365945b0b9b844dc2a0090/src/costats.Infrastructure/Expense/LogDigestor.cs)。

### 3.4 日聚合与写入容错

`LogDigestor`：

- 从今天回溯 29 天作为 30 日窗口；文件先按 `LastWriteTimeUtc < since - 1 day` 粗筛。
- Claude 以 timestamp 解析 `DateOnly`；通常的 ISO timestamp 直接取前 10 个字符，其他格式才走 `DateTimeOffset` 并转本地时间。这意味着带 UTC offset 的标准 ISO 字符串不会在快速路径中重新换算时区，需在产品中明确是否接受这一语义。
- Codex 直接按 `sessions/YYYY/MM/DD/*.jsonl` 目录枚举日期。
- 按 `DateOnly + model` 形成 slices，`ExpenseAnalyzer` 再把当天 slices 与全部 slices 分别求和。
- 文件用 `FileShare.ReadWrite` 打开，可以在写入者保持打开时读取；单行最多 512 KiB。
- EOF 的未换行尾行会被交给 JSON parser；若还没写完则捕获 `JsonException` 并跳过，下次轮询会重新读完整文件。
- 文件访问错误、坏 JSON 和超长行不阻塞其他文件。

注意：去重集合上限为 200,000；达到上限会清空集合。超大历史文件在清空后若再次出现旧的重复 key，理论上可能重复计数，这是“有界内存”与“全历史严格去重”的取舍。

## 4. `Clawdmeter`：消息级 max 合并 + 文件缓存

### 4.1 token 定义

`transcript.py` 从 `~/.claude/projects/**/*.jsonl` 读取 assistant message 的 `message.usage`：

```text
input       = input_tokens
output      = output_tokens
cache_read  = cache_read_input_tokens
cache_write = cache_creation_input_tokens
```

它明确区分两个口径：

- `work = input + output`：5 小时/7 天 headline，排除巨大 cache read，避免把上下文重读误当成新增工作量。
- `total = input + output + cache_read + cache_write`：详情 breakdown 使用。

### 4.2 去重与流式快照

Claude Code 一个 assistant message 可能拆成多条 JSONL 记录，每条都重复 `message.id` 但 usage 是不同时间点的快照。`Clawdmeter` 的 `merge_counts` 对四个 bucket 分别取最大值：

```text
merged.input       = max(previous.input, new.input)
merged.output      = max(previous.output, new.output)
merged.cache_read  = max(previous.cache_read, new.cache_read)
merged.cache_write = max(previous.cache_write, new.cache_write)
```

同时保存该 message 的最早 timestamp。这样既不会把流式快照逐行相加，也不会因为先看到 all-zero 重放而丢掉后面更完整的记录。session resume 把记录复制到另一个 transcript 文件时，account-wide scan 会跨文件按 `message.id` 再合并。

### 4.3 缓存、窗口和按日统计

- `account_window_tokens()` 只扫描 mtime 在最近 7 天内的文件；每个文件以 `(size, mtime)` 为 key 缓存解析结果。
- 窗口统计先跨全部文件按 message 合并，再按 timestamp 计算 5 小时和 7 天。
- Stats 的 `scan_events(0.0)` 可以做 lifetime 扫描；`build_aggregate` 对每一条已经去重的 `(timestamp, model, project, input, output, cache_read, cache_write)` 计算模型成本，并以本地 `datetime.fromtimestamp(ts).date()` 填充 `day_tokens`。
- 月度图表从月初到今天逐日补齐；累计 lifetime value 和每日日值另行维护。

### 4.4 正在写入与自身历史文件

transcript parser 使用普通文本 `open`，没有共享读取标志；它依赖 `json.loads` 失败即跳过和下一轮重扫来应对尾部半行。读取失败返回空结果。

另外，Clawdmeter 会把 API 轮询快照另存为 `%APPDATA%/Clawdmeter/usage_history.jsonl`（没有可写 AppData 时回退 `~/.clawdmeter`）：

- 每 300 秒最多追加一条 PII-free JSONL snapshot。
- 记录 `ts`、5h/7d 百分比、5h/7d token、extra usage 和 plan。
- 读取坏行跳过，写入失败忽略；启动时按 90 天 retention 清理。

这份 `usage_history.jsonl` 是产品自己的“服务端窗口快照历史”，不是 Claude transcript，也不应与 transcript token 总量直接相加。

来源：[`transcript.py`](https://github.com/weltern/Clawdmeter/blob/1083e89fbc87d6eb1fa8040c5af0be13fe4abf89/src/transcript.py)、[`stats.py`](https://github.com/weltern/Clawdmeter/blob/1083e89fbc87d6eb1fa8040c5af0be13fe4abf89/src/stats.py)、[`usage_history.py`](https://github.com/weltern/Clawdmeter/blob/1083e89fbc87d6eb1fa8040c5af0be13fe4abf89/src/usage_history.py)。

## 5. `OpenTokenUsage`：ccusage 的 JSON adapter，而非新的 parser

Claude plugin 的 `queryTokenUsage()` 做的事情很窄：

1. 生成今天往前 30 天的日期字符串，即包含今天在内的 31 个日历日。
2. 调用 `ctx.host.ccusage.query({ since, homePath })`。
3. 期望返回 `{ status: "ok", data: { daily: [...] } }`。
4. `collectUsageChartPoints()` 读取每个 day 的 `date`、`totalTokens`，排序后保留最后 31 个点。
5. `collectModelUsage()` 从 `models` 或 `modelBreakdowns` 中读取 `totalTokens`；如果没有，则把 `inputTokens`、`cachedInputTokens`、`cacheCreationTokens`、`cacheReadTokens`、`outputTokens`、`reasoningOutputTokens` 相加。

Rust host API 随后以 `ccusage@20.0.14` 运行 `ccusage claude daily --json --order desc` 或 `ccusage codex daily --json --order desc`，runner 顺序为 `bunx -> pnpm dlx -> yarn dlx -> npm exec -> npx`，并有旧版本 fallback。stdout 可以包含日志，host 会提取最后一个合法 JSON，并规范成 `{daily: [...]}`。

因此：

- OpenTokenUsage 的日报字段是 ccusage 的 JSON 输出契约，不是它自己的事件 schema。
- 它不重复实现去重或 Codex delta；这些全部继承 ccusage。
- 没有 package runner 时返回 `no_runner`；runner 存在但执行失败时返回 `runner_failed`。
- 它不直接调用 Claude/Codex provider API 来计算本地 token，但 package runner 可能为了下载 CLI 访问 registry。

来源：[`plugin.js`](https://github.com/PowerUserZ/OpenTokenUsage/blob/1f9eec12fd04368a6bfc2c371a57a6c11eef96b4/plugins/claude/plugin.js)、[`Plugin API`](https://github.com/PowerUserZ/OpenTokenUsage/blob/1f9eec12fd04368a6bfc2c371a57a6c11eef96b4/docs/plugins/api.md)、[`host_api.rs`](https://github.com/PowerUserZ/OpenTokenUsage/blob/1f9eec12fd04368a6bfc2c371a57a6c11eef96b4/src-tauri/src/plugin_engine/host_api.rs)。

## 6. 与当前 `token-tray` CC Switch SQLite 读取的差异

当前仓库的本地实现位于 [`token-tray/src-tauri/src/usage.rs`](../token-tray/src-tauri/src/usage.rs)；README 也明确说明默认只读 CC Switch 数据库。

| 维度 | 当前 `token-tray` | 本地 JSONL 方案 |
|---|---|---|
| 来源 | 自动发现或 `CC_SWITCH_DB_PATH` 指定的 SQLite；默认候选含 `~/.cc-switch/cc-switch.db`、应用数据目录等 | Claude `projects/**/*.jsonl`、Codex `sessions/**/*.jsonl`，还需考虑 `CODEX_HOME`、`CLAUDE_CONFIG_DIR` 和 archived 目录 |
| 读取方式 | `SQLITE_OPEN_READ_ONLY`，busy timeout 750ms；5 秒后台同步 | 文件遍历和逐行 JSON 解析；需处理写入竞争、尾部半行、坏行和大文件 |
| schema | 动态识别 rollup/log 表；兼容多组日期、provider、input/output/cache 列名 | 应用内部 JSONL schema，字段位置和事件类型随 CLI 版本变化 |
| token 语义 | SQL 行已经是 CC Switch 侧的请求/日聚合结果；`total_tokens` 可直接汇总 | 必须区分事件、流式重复记录和 Codex 累计快照；不能只做 `SUM` |
| 去重 | 当前查询对数据库结果求和，没有 transcript message/request 级去重逻辑 | Claude 需 message/request/session 去重；Codex 需累计快照 delta；sidechain/subagent 需明确策略 |
| 按日 | 读取 `date_key`；今天使用 SQLite `date('now','localtime')`，最近 7 天使用 `-6 days`；递归 CTE 补齐无数据日期 | timestamp 转本地日期；不同项目对 timezone 的默认行为不同，必须统一并测试跨午夜事件 |
| 失败状态 | 保留上一次成功 snapshot，记录 `database_not_found`、`database_unavailable`、`unsupported_schema`、`query_failed` | 建议保留 last-known-good，并额外区分 path missing、permission/read error、malformed lines、schema drift、partial tail |
| 数据边界 | CC Switch 已经接收到并写入统计库的请求 | CLI 本地日志的估算值；上游可能还没有写完、可能缺失 subagent 或含不可靠的 input/output 字段 |

当前 SQLite 日聚合的关键行为是：先把识别出的表转成统一 `date_key/app_type/token columns`，再 `UNION ALL`；随后对 total/month/7d/today/by-app 求和，并用递归 CTE 为 7 天序列补零。它与 JSONL 方案的根本区别是：SQLite 读取器信任上游数据库的行语义，JSONL 读取器必须自己建立事件身份和快照语义。

## 7. 推荐的最小改造方案（仅方案，不在本次调研中实施）

### 7.1 保持现有来源隔离

保留当前 CC Switch SQLite 作为默认 `source=CC Switch`，新增独立的 `source=Local JSONL (estimated)`。不要把 SQLite 与 JSONL 的同一天数值直接相加：两者很可能覆盖同一请求，后者还可能是未经服务端确认的估算。

推荐内部结果仍归一到现有 `UsageSnapshot` 形状：

```text
UsageSource.read() ->
  today / month / total / lastSevenDays
  daily[]
  byApp[] 或 byProvider[]
  source
  updatedAt
  error / confidence
```

第一版可以只做一个 `LocalJsonlSource`，先选需求最明确的一类 provider；不要一开始复制完整 ccusage 的多 provider、pricing、session、advisor 和 quota 体系。

### 7.2 解析规则

建议先实现以下最小且可验证的规则：

1. **Claude**：递归扫描 `CLAUDE_CONFIG_DIR`（若无则 `~/.claude/projects`）；只接受 `type=assistant`、有 timestamp、`message.role=assistant`、`message.usage` 的行。
2. **Codex**：扫描 `CODEX_HOME/sessions` 和必要的 `archived_sessions`；只接受 `event_msg.payload.type=token_count`。
3. **Claude 去重**：key 至少包含 provider、effective session id、message id、request id；同一 message 的四个 bucket 取 max，而不是相加；没有 id 时才使用文件路径 + 行号作为保守 fallback。
4. **Codex delta**：按文件/会话保存上一条累计 `total_token_usage`；计算非负 delta。发现 session id 变化或累计值回退时重置基线，避免跨 session 相减。
5. **写入容错**：以可共享读取方式打开；限制单行大小；只消费完整 JSON 行；尾部半行暂不计入，下一次扫描重试；坏行不影响其他文件。
6. **按日**：统一使用用户本地时区，并把 timezone 作为结果元数据/测试条件；不要混用“字符串取前十位”和“转本地时间”两种隐式语义。
7. **刷新与缓存**：先做全量正确版本；确认耗时后再加 `(path, size, mtime)` 缓存。缓存失效必须覆盖文件增长、mtime 精度不足、文件替换和删除。
8. **显示**：UI 明确写“本地日志估算”，不把它当成 CC Switch 服务端统计或订阅 quota；解析错误只显示摘要，不记录原始对话和凭据。

### 7.3 最小 fixture 与验收

至少准备脱敏 fixture 覆盖：

- Claude 一个 message 的 3 条流式记录，output/cache 逐步增长。
- Claude 同一 message 在 resume 文件中重放，原记录与重放记录 token 不同。
- Claude sidechain/subagent，确认是独立 message 才计入。
- Codex 两条累计快照、一次 `last_token_usage`、一次 session reset。

## 8. 本次实现决策

用户明确要求将 Token 统计改为读取本地文件计算，因此本项目本轮采用本地 JSONL 作为默认来源，而不是保留 CC Switch SQLite 默认来源：

- Claude Code：递归读取 `projects/**/*.jsonl`，按 `sessionId + message.id + requestId` 去重，同一消息的各 token bucket 取最大值。
- Codex：读取 `sessions/**/*.jsonl` 和 `archived_sessions/**/*.jsonl`，优先使用 `last_token_usage`，否则对 `total_token_usage` 做非负增量。
- 两类文件都只读处理；坏行和追加中的尾部半行跳过，下一轮重新扫描；结果明确标记为“本地日志估算”。
- PhotonMark/余额接口仍是独立的服务端查询，不与本地 Token 消耗量相加。

这样满足本次产品要求，但本地日志统计仍不等价于服务端账单或订阅额度；后续如需兼容 CC Switch 数据库，应作为单独来源切换，而不是与 JSONL 直接相加。
- 无换行尾部半行、坏 JSON、超长行、文件被替换/删除。
- UTC 跨本地午夜、夏令时切换（如果目标平台涉及）。
- 没有数据的日期仍输出连续 7 天序列。

验收要分别核对：input、output、cache read、cache creation、total 的口径；不要只用一个 total 数字判断正确。

## 8. 风险与不应过度承诺的地方

- **日志不是服务端真值**：ccusage 的官方 issue [#866](https://github.com/ccusage/ccusage/issues/866) 记录了 Claude Code JSONL 中 input/output 可能是流式占位或不完整值的情况；cache 字段更可靠不代表其他字段准确。UI 应写“日志估算”。
- **subagent/sidechain 仍是边界**：ccusage 曾有 subtask 统计讨论（[#313](https://github.com/ccusage/ccusage/issues/313)）；不同版本的日志布局、sidechain replay 和 tool result token 位置可能改变。
- **格式无稳定公共 schema**：README/源码虽然可作为当前实现依据，但 Claude/Codex JSONL 是 CLI 内部文件；解析器需要宽松、可观测、带 fixture 回归，不应把字段名视为长期 API。
- **文件并发一致性**：共享读权限只能降低“打不开”的概率，不能保证读到一个原子文件快照；全量重扫 + 尾行跳过 + last-known-good 是可接受的桌面工具策略。
- **路径和保留期**：Claude Code 默认日志保留期可能限制历史范围；多 config/profile、归档目录和环境变量必须显式处理。
- **时区**：按文件名日期、timestamp 前十位、本地时间和 UTC 可能得到不同的“今天”；本产品已有 SQLite localtime 语义，JSONL adapter 应明确对齐或显示来源差异。
- **资源占用**：全量读大 JSONL 会造成内存峰值；应先粗筛 mtime/marker，超过规模后再考虑增量索引，但索引本身要处理文件截断与替换。
- **隐私**：transcript 含对话、工具参数和路径；产品只应保存聚合结果，不上传原文，不把原始行写进诊断日志。
- **许可证**：本次核实的四个仓库均提供 MIT，但如果复制代码仍需保留对应版权和许可证，并检查依赖许可；“参考实现”与“直接复制源码”应在交付中分开记录。

## 9. 最终建议

当前阶段不建议替换 CC Switch SQLite 读取器。若要增加本地文件统计，建议先做一个可关闭的独立 JSONL provider，复用上述事件语义但不复制整个外部项目：

```text
CC Switch SQLite  ->  已入库请求统计（默认、相对稳定）
Local JSONL       ->  本地日志估算（可选、需去重和容错）
Quota/API         ->  服务端窗口/额度（独立展示）
```

最小第一步是 Claude 或 Codex 二选一，加上脱敏 fixture、日切换和 last-known-good；验证数值与 ccusage/costats 的差异后，再决定是否加入第二个 provider、cache 成本和更长历史。不要把“本地 token 消耗”“CC Switch 已聚合消耗”“服务端 quota 百分比”放在同一个无来源标识的数字上。
