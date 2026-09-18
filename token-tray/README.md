# Token Tray

Windows 任务栏和 macOS 菜单栏 token 统计小工具，直接只读读取 Claude Code/Codex 的本地会话文件并计算。

## 功能

- 任务栏仅展示当天 token 总量，使用千位逗号格式。
- 点击任务栏数字打开详情面板，查看周期概览、输入/输出/cache 和按应用统计。
- 详情面板支持关闭按钮和点击窗口外自动隐藏。
- 同一台电脑只允许一个实例运行，重复启动会聚焦已有详情面板。
- Rust 后台每 5 秒扫描本地会话文件，再通过事件同时推送给任务栏和详情面板。
- 详情面板支持 Escape 关闭；点击“刷新”才会主动触发一次读取。
- 点击数字可手动刷新；数据变化使用平滑增长动画。
- 客户端正在追加 JSONL 时跳过未完成的尾行，下一轮自动重试。
- Claude Code 按消息 Token 快照去重，Codex 按累计快照计算增量，避免重复累加。
- Windows 和 macOS 默认启用开机自启，可通过托盘菜单关闭。
- 读取失败时保留上一次成功的数据，托盘悬停提示会显示 token、最近同步时间和错误状态。
- 自动发现 Claude Code/Codex 的常见本地会话目录，兼容 `CLAUDE_CONFIG_DIR` 和 `CODEX_HOME`。
- 发布版启动时检查 GitHub Release；发现已签名更新后自动下载、安装并重启。
- 诊断日志只记录生命周期、同步结果类别和事件错误，不记录 token 数值、密钥、请求内容或数据库路径。

## 本地 Token 数据来源

默认自动发现并递归读取 `.jsonl` 文件：

```text
Claude Code: %USERPROFILE%\.claude\projects\**\*.jsonl
Codex:       %USERPROFILE%\.codex\sessions\**\*.jsonl
             %USERPROFILE%\.codex\archived_sessions\**\*.jsonl

macOS/Linux:
Claude Code: ~/.claude/projects/**/*.jsonl、~/.config/claude/projects/**/*.jsonl
Codex:       ~/.codex/sessions/**/*.jsonl、~/.codex/archived_sessions/**/*.jsonl
```

也支持使用 `CLAUDE_CONFIG_DIR`（可用逗号分隔多个目录）和 `CODEX_HOME` 指定配置根目录。读取失败时保留上一次成功的数据；详情页会标注“本地日志估算”。

## 自定义余额

详情面板的“余额”卡片支持按请求模板读取自定义接口。点击卡片右上角“配置”，应用会创建并打开：

```text
Windows: %APPDATA%\com.token-tray.app\balance.json
macOS:   ~/Library/Application Support/com.token-tray.app/balance.json
```

配置示例：

```json
{
  "name": "PhotonMark",
  "request": {
    "url": "https://codex.photonmark.com/api/v1/services/pay/status",
    "method": "GET",
    "headers": {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  "extractor": {
    "path": "balance_usd",
    "unit": "USD"
  }
}
```

上面的 `extractor` 使用 JSON 路径读取余额字段。也支持把 extractor 写成 JSON 字符串形式的函数，例如：

```json
"extractor": "function(response) { return { remaining: +response.balance_usd, unit: \"USD\" }; }"
```

请求头中的 `{{apiKey}}` 会替换为配置里的 API Key；也可以改用 `apiKeyEnv`，从环境变量读取密钥。余额请求不会把 API Key、响应正文或请求内容写入诊断日志，接口响应仅接受 JSON，超时时间为 8 秒。

## 中转站 Token 统计

统计面板会单独读取 `%APPDATA%\com.token-tray.app\relay.json`，按配置中的服务请求分别展示 `pay` 和 `boost` 的 `5h`、`24h`、`7d`、`all` token 统计。点击“配置”会自动创建并打开该文件。

配置示例：

```json
{
  "name": "PhotonMark",
  "apiKey": "",
  "request": {
    "url": "https://codex.photonmark.com/api/v1/services/{{service}}/status",
    "method": "GET",
    "headers": {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  "services": [
    { "id": "pay", "name": "Pay", "apiKey": "" },
    { "id": "boost", "name": "Boost", "apiKey": "" }
  ]
}
```

`{{service}}` 会替换成服务 ID。每个服务可以分别配置 `apiKey` 或 `apiKeyEnv`；旧配置中的全局 `apiKey`/`apiKeyEnv` 仍会作为没有服务专属密钥时的回退。

## 开发

需要 Node.js、pnpm 和 Rust。

```bash
pnpm install
pnpm tauri dev
```

## 构建 Windows 安装包

```bash
pnpm tauri build
```

产物位于 `src-tauri/target/release/bundle/`，包含 NSIS 和 MSI 安装包。

## GitHub Actions 发布

推送 `v*` 标签会在 Windows 和 macOS runner 上构建并发布安装包及更新清单。仓库 Actions Secrets 需要配置：

- `TAURI_SIGNING_PRIVATE_KEY`：本地保管的 Tauri updater 私钥内容。
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`：如果私钥设置了密码则配置，否则留空。

私钥不应提交到仓库；客户端只内置公钥。

诊断日志位于 Tauri 的应用日志目录下的 `token-tray.log`。

## 注意事项

- 开发模式不会写入开机自启配置。
- Windows 版本将窗口挂载到任务栏，因此需要在任务栏位置变化后重新定位。
- 本地会话目录不存在或文件格式不兼容时，工具会保留当前显示并在悬停提示错误。
