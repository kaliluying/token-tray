# Windows 任务栏 Token 统计工具调研

调研日期：2026-08-19

## 结论先行

建议不要直接复制某一个完整项目，而是采用“新建一个小型 WPF 项目 + 选择性复用 MIT 项目中的实现”的方式：

1. 以 [`fmdz387/costats`](https://github.com/fmdz387/costats) 作为主要参考和候选代码基座。它已经覆盖 Windows 托盘、WPF 浮窗、Codex/Claude 本地 JSONL 扫描、去重、单文件发布和 Windows Credential Manager。
2. 从 [`DiMY-CN/CodexQuotaMonitor`](https://github.com/DiMY-CN/CodexQuotaMonitor) 借鉴“贴靠任务栏、固定任务栏高度、DPI/显示器变化后重新定位”的交互和实现思路，但该仓库未发现明确开源许可证，不建议直接复制代码或整体 fork。
3. 如果产品还要显示“订阅额度/剩余百分比/重置时间”，增加独立的 Provider 适配层；可参考 [`wojtekmaj/ai-usage`](https://github.com/wojtekmaj/ai-usage)、[`RHPLUSSEUNG/codex-usage-monitor`](https://github.com/RHPLUSSEUNG/codex-usage-monitor) 和 [`PowerUserZ/OpenTokenUsage`](https://github.com/PowerUserZ/OpenTokenUsage) 的数据模型与解析方式。
4. 第一版应优先做本地日志统计，避免依赖未公开、容易变化的额度接口。额度查询作为可选能力加入，并明确显示“估算值”和“服务端额度”不是同一个指标。

这里的“token 统计”需要区分两种产品含义：

- **消耗统计**：从本地会话 JSONL 中读取 input/output/cache token，统计当前会话、今日、7 天、30 天和总量。
- **订阅额度**：读取服务端返回的 5 小时/7 天窗口剩余百分比和重置时间。

推荐同时支持，但数据源、错误处理和 UI 展示必须分开。

## 候选项目

| 项目 | 技术与功能 | 许可证 | 可复用判断 |
|---|---|---|---|
| [`fmdz387/costats`](https://github.com/fmdz387/costats) | .NET 10 WPF；托盘图标；任务栏附近浮窗；Codex/Claude 日志统计；可选 Copilot；单文件发布 | MIT | **首选基座/首选参考** |
| [`phun333/pi-infobar`](https://github.com/phun333/pi-infobar) | Windows WPF；本地 session 聚合；tokens/cost/languages/models/projects；托盘面板 | MIT | 可复用统计模型和 Windows UI 思路，目标数据格式不同 |
| [`kesuhiro74/TokenCheckerWin`](https://github.com/kesuhiro74/TokenCheckerWin) | Windows WinForms；Claude/Codex/Copilot；托盘/状态显示；每日费用 | MIT | 可参考 Provider、状态文本和轻量托盘 UI |
| [`DiMY-CN/CodexQuotaMonitor`](https://github.com/DiMY-CN/CodexQuotaMonitor) | C# WPF；固定贴靠任务栏；Codex `app-server` 的 `account/rateLimits/read` | 未发现明确许可证 | **只参考，不直接复制** |
| [`RHPLUSSEUNG/codex-usage-monitor`](https://github.com/RHPLUSSEUNG/codex-usage-monitor) | .NET 8 WinForms；Codex 5 小时/周额度；托盘；告警；主题和样式 | MIT | 可参考 quota 展示、告警和托盘交互 |
| [`nek0der/CodexBarWin`](https://github.com/nek0der/CodexBarWin) | .NET 10 WinUI 3；多 Provider；托盘；通过 WSL 调用 CodexBar CLI | MIT | 功能完整，但 WSL 前置条件不适合第一版 |
| [`wojtekmaj/ai-usage`](https://github.com/wojtekmaj/ai-usage) | Windows 原生壳 + Rust core；Claude/Codex/Copilot；凭据存储；通知；多语言 | 仓库 Cargo workspace 标注 MIT；需核对完整发布文件 | 适合参考跨 Provider 核心模型，不建议第一版引入 Rust |
| [`PowerUserZ/OpenTokenUsage`](https://github.com/PowerUserZ/OpenTokenUsage) | Rust + Tauri 2；插件式 Provider；20+ AI 工具；跨平台 | MIT | Provider 抽象值得借鉴，整体技术栈偏重 |
| [`weltern/Clawdmeter`](https://github.com/weltern/Clawdmeter) | Python/PySide6；Claude 本地 transcript；文件跟踪；token 去重；统计页 | MIT | 解析和去重经验有价值，不适合做 Windows 原生基座 |
| [`racase/agentbar`](https://github.com/racase/agentbar) | Electron；Windows tray；本地 usage reader；AI agent 状态 | MIT | 可参考前端交互，运行时和包体偏重 |
| [`psinghmanager/g4-Claw-counter`](https://github.com/psinghmanager/g4-Claw-counter) | Python 标准库；Claude/Codex；文件 watcher；SQLite；浮窗和费用 | 自定义 BSD 衍生条款 | 不建议作为基座；若复用必须逐条审查附加条款 |
| [`PacifAIst/Offtoco`](https://github.com/PacifAIst/Offtoco) | 离线文本 tokenizer；Windows 右键菜单；GPT/Claude/Gemini | GPL-3.0 | 只适合借鉴 tokenizer 需求；不适合并入 MIT/闭源路线 |

## 重点项目源码观察

### `costats`：最接近目标的可复用部分

源码路径和能力：

- `src/costats.App/Services/TrayHost.cs`：创建托盘图标、右键菜单、显示/隐藏浮窗、刷新和退出。
- `src/costats.App/Services/TaskbarPositionService.cs`：通过 Windows `SHAppBarMessage(ABM_GETTASKBARPOS)` 获取任务栏边缘和工作区，计算浮窗位置。
- `src/costats.Infrastructure/Usage/UsageLogScanner.cs`：以共享读写方式打开仍在追加的 JSONL，限制单行大小，忽略坏行，支持 Codex `token_count` 和 Claude `assistant.message.usage`。
- Codex 统计采用“每个 session 的累计值做 delta”，避免重复把周期性 token 快照相加。
- Claude 统计使用 message/request 去重；这种逻辑对实时追加和历史重扫都很重要。
- 项目使用 `H.NotifyIcon.Wpf`、`NHotkey.Wpf`、`CommunityToolkit.Mvvm`，并配置 win-x64/win-arm64 自包含单文件发布。

它最适合作为第一版的工程骨架，但需要先固定产品范围：其现有 UI 偏“额度/费用综合面板”，不一定适合直接拿来做极简任务栏数字条。

来源：[`costats README`](https://github.com/fmdz387/costats/blob/master/README.md)、[`TrayHost.cs`](https://github.com/fmdz387/costats/blob/master/src/costats.App/Services/TrayHost.cs)、[`TaskbarPositionService.cs`](https://github.com/fmdz387/costats/blob/master/src/costats.App/Services/TaskbarPositionService.cs)、[`UsageLogScanner.cs`](https://github.com/fmdz387/costats/blob/master/src/costats.Infrastructure/Usage/UsageLogScanner.cs)、[`LICENSE`](https://github.com/fmdz387/costats/blob/master/LICENSE)。

### `CodexQuotaMonitor`：任务栏体验最贴近，但授权不清晰

README 明确描述了一个贴在主任务栏左侧、保持任务栏高度、不会出现在普通任务栏应用列表中的 WPF 浮窗；它还通过本机 `codex.exe app-server --listen stdio://` 调用 `account/rateLimits/read`，根据窗口时长识别 5 小时和周额度。

这正好解决“显示在任务栏上”而不是“只在通知区域有图标”的体验问题。但仓库 README 明确写有“未提供开源许可证”，所以应当只把它当作行为和 API 风险的参考，不把代码、资源或二进制直接放入新项目。

来源：[`CodexQuotaMonitor README`](https://github.com/DiMY-CN/CodexQuotaMonitor/blob/native-wpf/README.md)、[`TaskbarPlacementCalculator.cs`](https://github.com/DiMY-CN/CodexQuotaMonitor/blob/native-wpf/src/CodexQuotaMonitor.Wpf/TaskbarPlacementCalculator.cs)、[`QuotaReader.cs`](https://github.com/DiMY-CN/CodexQuotaMonitor/blob/native-wpf/src/CodexQuotaMonitor.Wpf/QuotaReader.cs)。

### `ai-usage` / `OpenTokenUsage`：Provider 抽象参考

这两个项目体现了另一条路线：把 Claude、Codex、Copilot 等服务统一成 Provider，Provider 负责凭据、请求和响应解析，UI 只消费标准化的 usage snapshot。

这种抽象适合后续扩展，但第一版不应照搬完整跨平台架构。建议只保留以下最小接口：

```text
IUsageSource.ReadAsync(timeRange)
  -> UsageSnapshot

UsageSnapshot
  - provider
  - inputTokens
  - outputTokens
  - cacheReadTokens
  - cacheWriteTokens
  - fiveHourQuota   (optional)
  - weeklyQuota      (optional)
  - resetAt          (optional)
  - sourceStatus
  - lastUpdatedAt
```

来源：[`ai-usage README`](https://github.com/wojtekmaj/ai-usage/blob/main/README.md)、[`ai-usage Codex provider`](https://github.com/wojtekmaj/ai-usage/blob/main/core/ai-usage-core/src/providers/codex.rs)、[`OpenTokenUsage README`](https://github.com/PowerUserZ/OpenTokenUsage/blob/main/README.md)、[`OpenTokenUsage plugin API`](https://github.com/PowerUserZ/OpenTokenUsage/tree/main/docs/plugins)。

## 许可证与复用边界

### 可以直接作为参考或代码来源的项目

MIT 项目通常允许复制、修改和发布，但要保留原版权和许可证文本，并同时检查其第三方依赖许可证：

- `costats`
- `pi-infobar`
- `TokenCheckerWin`
- `codex-usage-monitor`
- `CodexBarWin`
- `ai-usage`
- `OpenTokenUsage`
- `Clawdmeter`
- `agentbar`

“允许复用”不等于“适合整仓库复制”。建议只提取必要模块，并在新项目中维护 `THIRD-PARTY-NOTICES.md`，记录来源仓库、commit、文件路径、许可证和改动说明。

### 不建议直接复用的项目

- `CodexQuotaMonitor`：没有明确开源许可证；公开可见不代表获得复制、修改和再发布授权。
- `claude-usage-tray`：克隆内容中没有发现许可证文件，按“保留所有权利”处理。
- `g4-Claw-counter`：许可证虽然允许若干使用方式，但包含额外名称、归属、分发和界面告知条款，复用成本和合规风险都高。
- `Offtoco`：GPL-3.0；如果把其代码链接或合并进整体程序，可能让分发方案受到 GPL 义务约束。除非新项目也接受 GPL，否则只借鉴功能，不复制代码。

## 推荐方案

### 产品定义

第一版定义为 Windows 10/11 常驻工具，默认显示一条非常短的状态：

```text
Codex 5H 72%  ·  WK 41%  ·  今日 128K
```

建议分为两层：

1. **任务栏浮窗**：只展示当前选择的 Provider、额度和一项 token 消耗摘要；高度跟随任务栏，默认左侧或右侧可选。
2. **点击后的详细面板**：展示各 Provider、当前会话、今日、7 天、30 天、input/output/cache、费用估算、最后更新时间和错误状态。

默认不保存原始会话内容，只保存聚合结果和设置。

### 推荐技术栈

- C# / .NET 8 或 .NET 10，WPF。
- `H.NotifyIcon.Wpf`：通知区域图标。
- 自己实现轻量任务栏浮窗定位，参考 `costats` 和 `CodexQuotaMonitor` 的公开行为，不复制无许可证代码。
- `System.Text.Json`：解析 JSONL。
- Windows Credential Manager：只在需要服务端额度时保存凭据；不把 token 写进日志或普通 settings JSON。
- JSON 文件保存设置；SQLite 暂不作为第一版依赖，除非需要长期历史图表或大量会话索引。
- 自包含单文件发布；后续再加 MSI/MSIX 和代码签名。

### 建议的模块边界

```text
App / Tray
  ├─ TaskbarWidgetWindow
  ├─ TrayHost
  ├─ SettingsWindow
  └─ SingleInstanceGuard

Application
  ├─ UsageRefreshCoordinator
  ├─ UsageSnapshotAggregator
  └─ AlertEvaluator

Infrastructure
  ├─ LocalLogSources
  │   ├─ CodexJsonlSource
  │   └─ ClaudeJsonlSource
  ├─ QuotaSources
  │   ├─ CodexQuotaSource (optional)
  │   └─ ClaudeQuotaSource (optional)
  ├─ CredentialStore
  └─ SettingsStore

Core
  ├─ UsageSnapshot
  ├─ TokenTotals
  ├─ QuotaWindow
  └─ ProviderStatus
```

### 数据源策略

**本地消耗量**：

- Codex：扫描 `%USERPROFILE%\\.codex\\sessions`、`archived_sessions` 和 `CODEX_HOME` 覆盖路径，解析 `event_msg` / `token_count`，按 session 累计值计算增量。
- Claude：扫描 `%USERPROFILE%\\.claude\\projects` 或 `CLAUDE_CONFIG_DIR`，解析 assistant message 的 usage；按 message/request 或稳定 event id 去重。
- 文件可能正在写入，必须使用 `FileShare.ReadWrite`、容错 JSON 解析、增量扫描和取消令牌。

**服务端额度**：

- Codex 第一优先参考本机 `codex.exe app-server` 的 rate limit 读取方式；它不需要新建一套 token 存储，但依赖桌面版/CLI 的内部协议，必须隔离在可失败的 Adapter 中。
- Claude 和 Copilot 的内部 OAuth/API endpoint 都应视为不稳定接口，默认关闭或标记为 experimental；接口异常时保留上次有效值并显示 stale 状态。

### 里程碑

1. **M0：需求冻结和样本采集**
   - 明确“消耗 token”“订阅额度”“费用估算”是否全部需要。
   - 用脱敏的 Codex/Claude JSONL 建立 fixture；不得提交真实 auth 文件和原始对话内容。
2. **M1：任务栏/托盘壳**
   - 单实例、托盘图标、任务栏贴靠、DPI、多显示器、任务栏位置变化、启动项。
3. **M2：本地 token 统计**
   - Codex/Claude parser、去重、实时追加、今日/7 天/30 天聚合、错误和 stale 状态。
4. **M3：详细面板和设置**
   - Provider 开关、刷新间隔、显示模式、主题、历史、费用估算开关、隐私说明。
5. **M4：可选额度 Provider**
   - 先接 Codex，之后按真实需求接 Claude/Copilot；每个 Provider 都必须有独立 fixture 和失败回退。
6. **M5：发布与安全**
   - self-contained x64；安装器；更新策略；第三方声明；代码签名；不上传遥测。

## 主要风险

- **指标混淆**：本地 token 消耗量和订阅额度百分比不能相互推导，UI 必须标出来源和更新时间。
- **内部接口变化**：Codex/Claude 的 app-server、OAuth 和 usage endpoint 不是稳定公共 API，必须允许 Provider 单独失效。
- **日志格式变化**：JSONL schema 是应用内部格式，解析器要宽松、带版本/样本测试，并保存 last-known-good 结果。
- **凭据安全**：只读本地凭据；日志中脱敏；设置页面不回显 token；不把原始 Authorization header 写入诊断文件。
- **任务栏兼容性**：Windows 任务栏可位于四个边缘，也可能有多显示器、缩放、自动隐藏和不同高度；定位必须基于工作区而非硬编码屏幕底部。
- **许可证合规**：复制 MIT 代码需要保留通知；无许可证项目不能直接复制；GPL 代码不能随意并入非 GPL 分发方案。

## 最终建议

如果目标是尽快做出可用版本：采用 `costats` 的 WPF/托盘/JSONL 扫描思路，自己重做一个更小的任务栏窗口，并把额度 Provider 设计成可插拔模块。

如果目标只是显示 Codex 的 5 小时/周额度，而不是统计真实 token 消耗：可以缩小范围，直接实现一个类似 `CodexQuotaMonitor` 的原生 WPF 小工具，但不要复制该仓库代码；先验证 `codex.exe app-server` 在目标 Codex 版本上的兼容性。

如果目标是多 Provider、跨平台或长期产品化：参考 `ai-usage` / `OpenTokenUsage` 的 Provider 抽象，但 Windows 第一版仍建议保留 C# WPF 单体，避免同时引入 Rust、Tauri、WSL 和多套发布链路。

