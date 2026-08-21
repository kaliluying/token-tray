# Token Tray

Windows 任务栏和 macOS 菜单栏 token 统计小工具，直接只读读取 CC Switch 的本地统计数据库。

## 功能

- 任务栏仅展示当天 token 总量，使用千位逗号格式。
- 点击任务栏数字打开详情面板，查看周期概览、输入/输出/cache 和按应用统计。
- 详情面板支持关闭按钮和点击窗口外自动隐藏。
- 同一台电脑只允许一个实例运行，重复启动会聚焦已有详情面板。
- Rust 后台每 5 秒只读取一次数据库，再通过事件同时推送给任务栏和详情面板。
- 详情面板支持 Escape 关闭；点击“刷新”才会主动触发一次读取。
- 点击数字可手动刷新；数据变化使用平滑增长动画。
- CC Switch 正在写入数据库时自动等待，减少瞬时读取失败。
- Windows 和 macOS 默认启用开机自启，可通过托盘菜单关闭。
- 读取失败时保留上一次成功的数据，托盘悬停提示会显示 token、最近同步时间和错误状态。
- 自动发现 CC Switch 的常见安装目录，并通过表名/列名兼容可识别的未来 schema 变化。
- 发布版启动时检查 GitHub Release；发现已签名更新后自动下载、安装并重启。
- 诊断日志只记录生命周期、同步结果类别和事件错误，不记录 token 数值、密钥、请求内容或数据库路径。

## 数据来源

默认自动发现：

```text
Windows: %USERPROFILE%\.cc-switch\cc-switch.db、%APPDATA%\cc-switch\cc-switch.db、%LOCALAPPDATA%\cc-switch\cc-switch.db
macOS:   ~/.cc-switch/cc-switch.db、~/Library/Application Support/cc-switch/cc-switch.db
```

也可以通过 `CC_SWITCH_DB_PATH` 指定数据库文件。数据库以只读方式打开，不修改 CC Switch 数据。

## 自定义余额

详情面板的“余额”卡片支持按 CC Switch 的请求模板读取自定义接口。点击卡片右上角“配置”，应用会创建并打开：

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

上面的 `extractor` 等价于 CC Switch 模板中的 `+response.balance_usd`。也支持把 extractor 写成 JSON 字符串形式的 CC Switch 函数，例如：

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
- CC Switch 未安装或数据库不存在时，工具会保留当前显示并在悬停提示错误。
