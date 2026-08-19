# Token Tray

Token Tray 是一个 Windows 任务栏和 macOS 菜单栏 Token 统计工具，读取 CC Switch 的 SQLite 数据库，只读展示当天 Token 总量。

## 功能

- Windows 任务栏显示当天 Token，支持逗号格式和增长动画
- macOS 顶部菜单栏显示当天 Token
- Windows 和 macOS 默认开机启动，可从托盘菜单关闭
- Windows Explorer 重启后自动恢复任务栏挂载
- 只读访问 `~/.cc-switch/cc-switch.db`，不会修改 CC Switch 数据

## 开发

```bash
cd token-tray
pnpm install
pnpm tauri dev
```

构建安装包：

```bash
pnpm tauri build
```

详细调研记录见 `docs/research-token-taskbar.md`。
