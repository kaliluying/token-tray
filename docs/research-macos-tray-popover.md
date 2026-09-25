# macOS 菜单栏/托盘 Popover、全屏 Space 与原生动画调研

调研日期：2026-09-19
范围：GitHub 上有明确许可证的 macOS 菜单栏/面板项目；阅读其 README、相关源码、issue 和提交记录，重点关注 `NSPanel`/`NSWindow` 层级、Spaces 行为、焦点、锚点、关闭策略和动画。
当前项目：`/Users/gemaolin/code/token-tray`，实际 Tauri 工程位于 `token-tray/`。

下文只做源码调研，不复制外部代码，也没有修改现有实现。

## 结论先行

当前问题的关键不是 React 内容是否渲染，而是“原生窗口是否被正确放进全屏 Space 的窗口层级，以及何时才 order out”。适合 `Tauri 2 + React + Rust` 的方案是：

1. **详情窗口应当是真正的 `NSPanel`**，而不是只把普通 Tauri `NSWindow` 的 style mask 改成 `NonactivatingPanel`。`ahkohd/tauri-nspanel` 已经提供了 Tauri 窗口转成 `NSPanel` 和 PanelBuilder 两条路径，最接近当前技术栈。
2. **全屏显示至少需要 `FullScreenAuxiliary`**。如果详情面板要像菜单栏工具一样跨所有 Space 可用，组合 `CanJoinAllSpaces`；如果只想把普通窗口移动到当前 Space，才考虑 `MoveToActiveSpace`。两者不是同一个语义，不应盲目同时使用。
3. **交互面板不要为了显示而激活整个应用**。优先 `NonactivatingPanel` + `orderFrontRegardless()`；需要键盘输入时，再让面板成为 key window。当前项目应避免用 Tauri 的 `set_focus()` 打开 macOS 详情窗口，因为这条路径可能激活应用并切换 Space。
4. **层级先用 `.floating` 或 `.statusBar`，仍被全屏内容覆盖时再提升到 `.screenSaver`**。Maccy 的 issue/提交记录证明 `.statusBar` 可能低于 Chrome 的高层 UI；但 `screenSaver` 也会带来输入法、游戏、演示模式等副作用，不能无条件使用。
5. **关闭必须分成“开始收回动画”和“真正隐藏窗口”两个阶段**。CodexBar 的状态机和抽屉动画是最值得借鉴的部分：先做 layer transform/alpha 动画，动画完成后再 `orderOut`；使用 generation/token 丢弃过期的关闭回调，避免快速点击时旧定时器把新窗口关掉。
6. **锚点应取真实状态栏按钮的屏幕坐标，并做显示器和可见区域校验**。Popover 项目用 `NSStatusItem` button 的 window frame 计算箭头位置；CodexBar 还会验证锚点是否仍在目标屏幕，失败时回退到无箭头面板。

对当前项目的优先级建议：先把详情窗口迁移到 `tauri-nspanel` 的实际 `NSPanel`，保留 React 负责内容和 reduced-motion；然后把打开/收回改成原生 content layer 的 transform 动画，最后再处理焦点失去、外部点击和多显示器回退。不要继续叠加 `activate`、重试或延迟显示来掩盖原生窗口类别和层级问题。

## 候选项目对照

| 项目 | 许可证 | 关键实现 | 对当前项目的价值 | 主要限制 |
|---|---|---|---|---|
| [`ahkohd/tauri-nspanel`](https://github.com/ahkohd/tauri-nspanel) | Apache-2.0 / MIT 双许可证 | Tauri 窗口转 `NSPanel`；PanelBuilder；`NonactivatingPanel`；`FullScreenAuxiliary`；窗口层级和 collection behavior API | **最适合作为 Tauri 原生桥接层** | 需要重新处理 Tauri 窗口创建/转换；高层级会影响输入法等系统 UI |
| [`p0deje/Maccy`](https://github.com/p0deje/Maccy) | MIT | `NSPanel`；`.screenSaver`；`.moveToActiveSpace`；`.fullScreenAuxiliary`；失去 key 后关闭 | **最接近可交互菜单栏面板的完整窗口策略** | Swift/AppKit；它的“当前 Space”依赖 panel 成为 key，不能直接照搬到非激活 Tauri 窗口 |
| [`iSapozhnik/Popover`](https://github.com/iSapozhnik/Popover) | MIT | `NSStatusItem` + `NSPanel`；真实按钮锚点；`statusBar` 层；外部点击 monitor；`orderFrontRegardless` | **适合参考状态栏定位和关闭策略** | 只有 `CanJoinAllSpaces`，没有 `FullScreenAuxiliary`，不能作为全屏 Space 的完整方案 |
| [`steipete/CodexBar`](https://github.com/steipete/CodexBar) | MIT | 当前 Codex/Claude 用量菜单栏应用；SwiftUI + AppKit；源码和 README 都是产品级参考 | **适合参考用量产品的菜单栏信息层级** | 主菜单路径不是当前 Tauri 详情面板的同一种原生窗口实现 |
| [`bob-zebedy/CodexBar`](https://github.com/bob-zebedy/CodexBar) | GPL-3.0 | `NSPopover` 与 fallback `NSPanel` 双路径；opening/shown/closing 状态机；`CABasicAnimation` drawer transform | **动画和竞态处理最有参考价值** | GPL-3.0；只能借鉴行为和结构，不能直接复制代码到非 GPL 分发方案 |

## 1. `tauri-nspanel`：当前 Tauri 项目的首选参考

### README 与实现路径

README 明确将普通 Tauri window 转为 panel，或者用 `PanelBuilder` 创建 panel；PanelBuilder 的源码顺序是“先创建 Tauri webview window，再调用 `to_panel::<T>()`，最后应用 panel 配置”。这比在已有普通 `NSWindow` 上追加一个 style bit 更接近 AppKit 的真实模型。

关键源码：

- [`README.md`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/README.md)：安装、`tauri_panel!`、`PanelBuilder` 和 `show_and_make_key()` 示例。
- [`src/builder.rs`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/src/builder.rs)：`PanelBuilder`、`no_activate`、`level`、`style_mask`、`collection_behavior`、`hides_on_deactivate`。
- [`examples/panel_style_mask.rs`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/examples/panel_style_mask.rs)：`nonactivating_panel()`、borderless、HUD 和 utility window 的组合。
- [`examples/panel_levels.rs`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/examples/panel_levels.rs)：`Normal`、`Floating`、`Status` 和自定义层级的比较。
- [`examples/collection_behavior.rs`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/examples/collection_behavior.rs)：`CanJoinAllSpaces`、`Stationary`、`IgnoresCycle`、`FullScreenAuxiliary` 和 `Transient` 的组合。

### 全屏示例的实际组合

项目专门增加了一个 fullscreen 示例，提交为 [`5a06fec`](https://github.com/ahkohd/tauri-nspanel/commit/5a06fec)。示例 README 给出的组合是：

```text
ActivationPolicy.Accessory
PanelLevel.Floating
StyleMask.NonactivatingPanel
FullScreenAuxiliary + CanJoinAllSpaces
hidesOnDeactivate = false
```

它还明确提醒：全屏/最大化操作不能用于配置为全屏浮层的 panel，否则可能导致崩溃。对当前项目来说，详情窗口应该保持普通固定尺寸，不能暴露最大化和 fullscreen 操作。

### issue 提供的风险证据

- [`#123`](https://github.com/ahkohd/tauri-nspanel/issues/123)：macOS 27 上 panel 点击后表现得像普通窗口，焦点从 Arc 浏览器切走。issue 中的复现代码仍显式使用 `nonactivating_panel()`，说明“声明了 nonactivating”不代表未来 macOS 版本完全不会改变焦点行为，必须做真实系统回归测试。
- [`#104`](https://github.com/ahkohd/tauri-nspanel/issues/104)：高 window level 会阻塞输入法。当前项目如果采用 `.screenSaver`，需要把中文输入法、Emoji/候选框、全屏视频和游戏作为验收项，而不是只验证窗口可见。

### 对当前项目的判断

当前 `lib.rs` 已经在普通 Tauri webview 上追加 `NonactivatingPanel`、设置 `NSScreenSaverWindowLevel`、写入 fullscreen collection behavior。这些动作与该项目的 fullscreen 示例方向一致，但仍有两个结构性差异：

1. 当前对象仍然是 Tauri 创建的普通 webview window，不是 `tauri-nspanel` 提供的真正 `NSPanel` 类型。
2. 当前 React CSS 动画只能改变 webview 内容层；它不会改变 WindowServer 中原生窗口的 frame、层级或 Space 归属。

因此，若问题仍然出现，下一步应优先验证“实际 window class、level、collection behavior 和 order/front 时机”，而不是继续调整 CSS easing。

## 2. Maccy：可交互菜单栏 NSPanel 的完整对照

### README、源码与许可证

Maccy README 说明它是原生 macOS 剪贴板管理器，MIT 许可证见 [`LICENSE`](https://github.com/p0deje/Maccy/blob/master/LICENSE)。2.0 版本讨论记录明确写出 UI 从 AppKit + NSMenu 改为 SwiftUI + NSPanel，见 [`2.0 beta Discussion #818`](https://github.com/p0deje/Maccy/discussions/818)。

核心源码是 [`Maccy/FloatingPanel.swift`](https://github.com/p0deje/Maccy/blob/master/Maccy/FloatingPanel.swift)：

- 创建 `NSPanel` 时使用 `nonactivatingPanel`、`resizable`、`closable`、`fullSizeContentView`。
- `isFloatingPanel = true`，`hidesOnDeactivate = false`，`animationBehavior = .none`。
- 当前源码使用：

  ```text
  level = .screenSaver
  collectionBehavior = [.auxiliary, .stationary, .moveToActiveSpace, .fullScreenAuxiliary]
  ```

- 打开时 `orderFrontRegardless()` 后 `makeKey()`；这说明它选择“面板本身可交互并成为 key”，而不是激活主应用窗口。
- 重写 `resignKey()`，失去 key 后关闭；关闭回调统一清理 `isPresented` 和状态栏高亮。
- `verticallyResize()` 用 `NSAnimationContext` + `animator().setFrame()` 平滑调整窗口几何。

### issue 与提交如何解释 window level

Maccy 的 [`#1403`](https://github.com/p0deje/Maccy/issues/1403) 报告 popup 被 Chrome autofill 下拉框盖住。修复提交 [`205191f`](https://github.com/p0deje/Maccy/commit/205191f) 把 `level` 从 `.statusBar` 提高到 `.screenSaver`，提交说明指出 Chrome autofill 在 window layer 999，而 `screenSaver` 为 1000。

这里有两个可执行结论：

1. `alwaysOnTop` 不是充分条件；最终是否可见取决于 AppKit window level、Space collection behavior 和对方应用使用的层级。
2. `.screenSaver` 是有代价的“最后一级工具”，不是普通菜单栏 popover 的默认配置。对 token-tray，应该先确认 `.floating`/`.statusBar` 在目标全屏应用下是否足够；只有确实被覆盖时才提升。

### `MoveToActiveSpace` 与 `CanJoinAllSpaces`

Maccy 选择 `MoveToActiveSpace`，并没有使用 `CanJoinAllSpaces`。这适合“每次从菜单栏/快捷键打开时，面板出现在当前 Space”的语义；但它依赖 panel 的显示和 key/focus 时机。

如果应用是非激活的 accessory app，`MoveToActiveSpace` 不能简单理解为“总会移动到用户当前 Space”。面板没有成为当前 Space 的有效窗口时，系统没有充分理由替它迁移。当前 token-tray 的目标是“在其他 App 全屏时可靠可见”，所以 `FullScreenAuxiliary + CanJoinAllSpaces` 更稳妥；如果将来改为严格的单 Space 面板，再单独验证 `MoveToActiveSpace` 与焦点策略。

## 3. `iSapozhnik/Popover`：状态栏锚点与外部点击

这是一个 MIT 的 Swift Package，README 和许可证见 [`README.md`](https://github.com/iSapozhnik/Popover/blob/master/README.md) 与 [`LICENSE`](https://github.com/iSapozhnik/Popover/blob/master/LICENSE)。它不解决全屏 Space 的全部问题，但定位和关闭策略很清楚。

### Window 配置

[`PopoverWindow.swift`](https://github.com/iSapozhnik/Popover/blob/master/Sources/Popover/PopoverWindow.swift) 创建 `NSPanel` 时使用：

```text
styleMask = [.nonactivatingPanel]
level = .statusBar
animationBehavior = .utilityWindow
collectionBehavior = [.canJoinAllSpaces, .ignoresCycle]
hasShadow = true
```

它没有 `FullScreenAuxiliary`，所以适合普通菜单栏 popover，不足以作为当前全屏修复的唯一依据。

### 锚点算法

[`PopoverWindowController.swift`](https://github.com/iSapozhnik/Popover/blob/master/Sources/Popover/PopoverWindowController.swift) 从 `statusItem.button?.window?.frame` 取得状态栏按钮位置，根据当前鼠标所在屏幕的 frame 计算 x/y，并限制右边缘 margin；同时计算 arrow 的 x 位置。其核心思想是“锚定真实 status item button 的 window 坐标”，不是把窗口固定到 `NSScreen.main` 或主应用窗口中心。

当前项目已有 `tray.rect()` + `position_below_anchor()` 的方向是对的，但应继续保留以下回退：

- tray rect 不可用时使用鼠标所在屏幕或 details 当前屏幕；
- 位置必须夹紧到对应 monitor 的 work area；
- 全屏 Space 下不要把“主窗口所在 Space”当作唯一定位依据。

### 不激活与关闭

提交 [`dee3296`](https://github.com/iSapozhnik/Popover/commit/dee3296) 增加了 `show(withFocus:)`：有焦点时 `showWindow` + `makeKey`，无焦点时使用 `orderFrontRegardless()`。这个提交还把全局鼠标 monitor 的关闭行为和 `keepPopoverVisible` 分开，说明“显示、不抢焦点、点击外部关闭”是三个独立状态，不应由一个 `isVisible` 布尔值粗略代替。

## 4. CodexBar：动画状态机和层级化面板的参考

### MIT 的上游产品

[`steipete/CodexBar`](https://github.com/steipete/CodexBar) 是与当前产品领域最接近的开源菜单栏用量工具，README 描述了 Codex/Claude 的菜单栏用量、额度和本地统计，仓库许可证为 MIT，见 [`LICENSE`](https://github.com/steipete/CodexBar/blob/main/LICENSE)。它适合参考信息层级、菜单栏状态压缩和本地/服务端数据分层，但不应把它的整套 UI 架构引入当前 Tauri 项目。

### GPL fork 中的 drawer 动画实现

`bob-zebedy/CodexBar` 的 GPL-3.0 源码包含更直接的 popover/panel 动画实现：

- [`StatusItemController.swift`](https://github.com/bob-zebedy/CodexBar/blob/main/CodexBar/Controllers/StatusItemController.swift)：将菜单面板状态分为 `hidden`、`opening`、`shown`、`closing`；打开和关闭都先改变动画状态，再在完成回调中改变最终可见状态。
- [`SidePanelSupport.swift`](https://github.com/bob-zebedy/CodexBar/blob/main/CodexBar/Controllers/SidePanelSupport.swift)：`SidePanelDrawerAnimator` 对 content view 的 `CALayer` 使用 `CABasicAnimation(keyPath: "transform")`，同时更新 model layer；入口动画前强制 layout/display，并在 `CATransaction.flush()` 后开始动画。
- 同一个文件中的 `SidePanelDrawerPresenter` 用 `visibilityGeneration` 防止过期的收回回调关闭刚刚重新打开的面板；收回动画完成后才 `orderOut`，然后把 transform 复位。
- 面板工厂使用 `borderless + nonactivatingPanel`、`transient + canJoinAllSpaces + fullScreenAuxiliary`、`hidesOnDeactivate = false`、`animationBehavior = .none`。
- 主菜单路径设置 `NSPopover.animates = false`，自己控制淡入淡出；fallback panel 才负责跨 Space 和复杂定位。

这套设计对当前项目的直接启示是：如果用户看到“文字动了但白框没动”，最可靠的层级是 native content layer，而不是只给内部文本加 transition。React/CSS 可以保留为网页内容层的视觉降级，但整个白色面板应当由 shell layer 或 native `CALayer` 统一做 transform/opacity。

由于该仓库是 GPL-3.0，当前项目只能借鉴上述行为、状态机和验收标准，不能直接拷贝 `SidePanelSupport.swift` 等实现，除非项目明确采用兼容的 GPL 分发方案。

## 5. 关键方案比较

### Window level

| 层级 | 适用 | 证据与风险 |
|---|---|---|
| `.floating` | 普通浮动工具窗口 | `tauri-nspanel` fullscreen 示例默认推荐的起点；对系统 UI 侵入较小 |
| `.statusBar` | 常规菜单栏 popover | `Popover` 使用；但 Maccy 的 #1403 证明它可能低于其他应用的高层下拉 UI |
| `.screenSaver` | 需要覆盖全屏应用或高层 UI 的临时面板 | Maccy commit `205191f` 使用；`tauri-nspanel` issue #104 报告可能阻塞输入法，必须限制使用范围 |

对 token-tray 的建议是保留一个可切换策略：默认 `.floating`/`.statusBar`，在检测到目标是全屏 Space 且普通层级不可见时使用 `.screenSaver`。但不要在每次打开时激活整个应用来“保证可见”。

### Collection behavior

| 行为 | 语义 | 适合当前项目的判断 |
|---|---|---|
| `CanJoinAllSpaces` | 面板加入所有 Space | 菜单栏工具最稳妥；当前全屏需求建议保留 |
| `CanJoinAllApplications` | 面板可跨应用显示 | 对 accessory/tray 详情面板有帮助；应与全屏验收一起测试 |
| `FullScreenAuxiliary` | 允许和全屏主窗口同时显示 | 当前需求的必要项，不能只依赖 `alwaysOnTop` |
| `MoveToActiveSpace` | 面板迁移到打开时的活动 Space | 适合 Maccy 的“当前 Space”语义，但要求显示/key 时机正确；非激活面板上需单独回归 |
| `Stationary` | 在 Exposé/Mission Control 中保持稳定 | 适合短暂浮层；可能增加面板的存在感，需与 `Transient` 取舍 |
| `Transient` | 临时面板，不参与普通窗口管理 | 适合详情/抽屉；如果用户需要面板持续固定，则不要使用 |

经验上，`CanJoinAllSpaces + FullScreenAuxiliary` 是“无论用户在哪个 Space 都能打开”的策略；`MoveToActiveSpace + FullScreenAuxiliary` 是“每次打开只跟随当前 Space”的策略。不能把两者的产品语义混为一谈。

### 激活、焦点和交互

- 只读浮层：`NonactivatingPanel`、`canBecomeKey = false`、`orderFrontRegardless()`，点击不抢走编辑器焦点。
- 详情面板含按钮、滚动、输入：实际 `NSPanel`，允许成为 key，但不要调用 `NSApp.activate(ignoringOtherApps:)`；面板成为 key 与主应用激活是两个概念。
- 全屏启动路径：先设置 level/collection/style，再定位、显示、order front，最后触发前端 opening 事件。不要在窗口仍 hidden 时先发 opening 事件。

当前 `token-tray` 已经在 [`lib.rs`](../token-tray/src-tauri/src/lib.rs) 使用 Accessory、主线程 AppKit 配置和 `orderFrontRegardless()`，方向正确；但真正的 `NSPanel` 化和 native layer 动画仍是最值得验证的下一步。

### 关闭策略

推荐状态机：

```text
hidden -> opening -> shown -> closing -> hidden
```

关闭时应满足：

1. 取消/递增旧的 generation；
2. 将 content layer 设为收回起点；
3. 播放 transform + opacity；
4. 动画 completion 再 `orderOut`/`hide`；
5. reset model layer，避免下一次打开从上次的 transform 开始；
6. 如果 opening 在 closing 期间到达，取消旧动画并以新 generation 重新打开。

不要把 `hidesOnDeactivate = true` 当作唯一的点击外关闭方案。它可能让 `isVisible` 与“屏幕上是否可见”不一致，进而出现第一次点击不打开、第二次才打开的问题。更稳妥的是 `hidesOnDeactivate = false` + 本地/全局点击 monitor 或 `resignKey`/`Focused(false)` 的延迟判断。

### 锚点定位

菜单栏点击路径：

1. 取得 `NSStatusItem.button` 的 window/screen 坐标；
2. 以按钮中点作为 x 锚点，向下放置详情面板；
3. 根据目标 screen 的 visible/work area 做左右、上下夹紧；
4. 锚点无效时回退到鼠标所在屏幕，不回退到固定的 `NSScreen.main`；
5. 多显示器和 Retina 下统一使用屏幕坐标，避免 logical/physical 混用。

快捷键或无法取得 status item button 时，建议采用无箭头 fallback panel，并在报告/日志中区分“真实托盘锚点”和“回退定位”。

## 6. 对当前 Tauri 2 + React + Rust 的可执行建议

### P0：先解决原生窗口类别与全屏可见性

建议新增一个只负责 macOS 详情面板的 Rust 适配层，优先评估 `tauri-nspanel`：

```text
Rust
  DetailsPanelController
    - create/convert actual NSPanel
    - set style mask on main thread
    - set level and collection behavior
    - show without activating app
    - orderOut only after close animation
    - generation for open/close races

React
  DetailsPanel content
    - data and buttons
    - visual shell animation as fallback
    - prefers-reduced-motion
```

建议的第一版 native 配置：

```text
styleMask: borderless | nonactivatingPanel
level: floating or statusBar
collectionBehavior: canJoinAllSpaces | canJoinAllApplications | fullScreenAuxiliary
hidesOnDeactivate: false
animationBehavior: none
```

如果真实全屏应用仍覆盖面板，再单独切换 `screenSaver`，同时验证输入法和系统 UI。不要同时引入 `activateIgnoringOtherApps` 作为补丁。

### P1：把“窗口可见”与“内容动画”解耦

当前 React 的 [`App.tsx`](../token-tray/src/App.tsx) 监听 opening/closing 事件，CSS 的 [`App.css`](../token-tray/src/App.css) 给 `.details-shell` 做 transform/opacity，这是合理的内容层起点。但要满足“白框整体移动”，应确保：

- 白色背景、border、shadow 和 radius 都在同一个 shell/layer 内；
- opening 事件发生在 native window 已 `show/orderFront` 且首帧 layout 完成之后；
- closing 不要先 `hide()`，而是等 transform/opacity completion；
- 如果 WebView compositor 在某个 macOS 版本上不能稳定对整个 shell 做 transform，改为 native `contentView.layer` 的 `CABasicAnimation`，参考 GPL fork 的 `SidePanelDrawerAnimator` 行为，但自行实现；
- 继续保留 `prefers-reduced-motion`，减少动态时将动画时长压到近似 0，而不是只删除某个文字 transition。

推荐动画参数：打开 180–240ms 的 ease-out，收回 120–180ms 的 ease-in；位移 6–12pt，缩放 0.97–1.0，避免夸张弹跳。动画只是视觉层，不能承担 Space/层级修复。

### P1：焦点与点击外关闭

保留当前 generation 防竞态，但把关闭触发统一到三类来源：

1. 面板内明确关闭按钮；
2. tray/status item 再次点击；
3. 面板失焦后延迟检查：面板是否仍 visible、是否重新获得 key、鼠标是否位于 tray 锚点或面板内。

如果迁移到实际 `NSPanel`，优先使用 `resignKey`/window delegate 作为 native 信号，并保留当前 Rust 的 200ms 左右 grace period 作为 WebView/托盘点击过渡保护。不要仅依赖 Tauri `Focused(false)`，因为 nonactivating panel 的 key/focus 语义与普通 window 不同。

### P2：验收矩阵

每次改动至少验证：

| 场景 | 验收条件 |
|---|---|
| 普通桌面点击托盘 | 面板贴在托盘下方，白框整体弹出，点击按钮可用 |
| Safari/Chrome 全屏 Space | 不切回第一个桌面，面板可见且可交互 |
| 面板内点击 | 不出现第二个窗口，不因失焦立即收回 |
| 点击外部 | 播放收回动画，动画完成后才消失 |
| 快速开关 | 不出现旧定时器关闭新面板，不残留透明/偏移状态 |
| 中文输入法/候选框 | `.screenSaver` 层级不阻塞候选框；如阻塞，记录为层级副作用 |
| 多显示器/Retina | 以实际 tray screen 定位，不跳到主屏 |
| Reduce Motion | 开合不出现明显位移残影 |
| 退出/重启 | 只有一个 tray 进程，旧窗口状态不会复活 |

## 7. 最终建议与复用边界

### 推荐采用

- **代码级首选参考**：`ahkohd/tauri-nspanel`，因为它直接解决 Tauri 2 到 `NSPanel` 的桥接，并有 fullscreen 示例。
- **窗口行为首选参考**：Maccy `FloatingPanel.swift`，尤其是 `NonactivatingPanel`、`hidesOnDeactivate = false`、`resignKey` 关闭、`MoveToActiveSpace + FullScreenAuxiliary` 和 level 处理。
- **定位首选参考**：`iSapozhnik/Popover`，尤其是真实 `NSStatusItem` button 坐标、work area 夹紧、`orderFrontRegardless` 和外部点击 monitor。
- **动画状态机首选参考**：CodexBar fork 的 drawer presenter 思路；只借鉴状态机、generation、layer transform 和 completion 时序，不复制 GPL 代码。

### 不推荐

- 继续只改 CSS easing 来解决全屏不可见；这是两个不同层次的问题。
- 无条件使用 `MoveToActiveSpace`，同时又要求 nonactivating 且不激活应用；这会使“当前 Space”行为依赖窗口是否真正成为 key。
- 无条件使用 `.screenSaver`；它可能盖住输入法或其他系统 UI。
- 使用公开源码但没有许可证的项目作为代码来源。本报告列出的候选都有明确许可证；GPL 项目也已明确标记为只能做行为参考。

### 建议的落地顺序

1. 用 `tauri-nspanel` 做一个最小 isolated details panel spike，只验证实际 `NSPanel`、全屏可见、点击交互和不激活主应用。
2. 在 spike 上逐项比较 `Floating/Status` 与 `ScreenSaver`，记录 `CGWindowList` 层级、全屏 Safari/Chrome、输入法和多显示器结果。
3. 将验证通过的 native 配置合并回当前 Rust controller；React 保留内容和 reduced-motion，不再负责原生窗口 show/hide 时序。
4. 用 `CABasicAnimation` 或同等 native layer 动画实现整个白框的弹出/收回，`orderOut` 只放在 completion。
5. 最后再微调 Apple 风格 easing、位移、圆角和 shadow，避免视觉参数掩盖窗口层级问题。

## 主要一手来源索引

- [`tauri-nspanel README`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/README.md)、[`fullscreen README`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/examples/fullscreen/README.md)、[`collection_behavior.rs`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/examples/collection_behavior.rs)、[`panel_levels.rs`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/examples/panel_levels.rs)、[`panel_style_mask.rs`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/examples/panel_style_mask.rs)、[`PanelBuilder`](https://github.com/ahkohd/tauri-nspanel/blob/v2.1/src/builder.rs)、[`fullscreen commit 5a06fec`](https://github.com/ahkohd/tauri-nspanel/commit/5a06fec)、[`issue #123`](https://github.com/ahkohd/tauri-nspanel/issues/123)、[`issue #104`](https://github.com/ahkohd/tauri-nspanel/issues/104)。
- [`Maccy README`](https://github.com/p0deje/Maccy/blob/master/README.md)、[`FloatingPanel.swift`](https://github.com/p0deje/Maccy/blob/master/Maccy/FloatingPanel.swift)、[`LICENSE`](https://github.com/p0deje/Maccy/blob/master/LICENSE)、[`Discussion #818`](https://github.com/p0deje/Maccy/discussions/818)、[`issue #1403`](https://github.com/p0deje/Maccy/issues/1403)、[`commit 205191f`](https://github.com/p0deje/Maccy/commit/205191f)。
- [`Popover README`](https://github.com/iSapozhnik/Popover/blob/master/README.md)、[`PopoverWindow.swift`](https://github.com/iSapozhnik/Popover/blob/master/Sources/Popover/PopoverWindow.swift)、[`PopoverWindowController.swift`](https://github.com/iSapozhnik/Popover/blob/master/Sources/Popover/PopoverWindowController.swift)、[`Popover.swift`](https://github.com/iSapozhnik/Popover/blob/master/Sources/Popover/Popover.swift)、[`LICENSE`](https://github.com/iSapozhnik/Popover/blob/master/LICENSE)、[`commit dee3296`](https://github.com/iSapozhnik/Popover/commit/dee3296)。
- [`steipete/CodexBar README`](https://github.com/steipete/CodexBar/blob/main/README.md)、[`LICENSE`](https://github.com/steipete/CodexBar/blob/main/LICENSE)、[`StatusItemController.swift`](https://github.com/steipete/CodexBar/blob/main/Sources/CodexBar/StatusItemController.swift)。
- [`bob-zebedy/CodexBar SidePanelSupport.swift`](https://github.com/bob-zebedy/CodexBar/blob/main/CodexBar/Controllers/SidePanelSupport.swift)、[`StatusItemController.swift`](https://github.com/bob-zebedy/CodexBar/blob/main/CodexBar/Controllers/StatusItemController.swift)、[`LICENSE`](https://github.com/bob-zebedy/CodexBar/blob/main/LICENSE)。
- [Apple `NSWindow` collection behavior: `canJoinAllSpaces`](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/canjoinallspaces)、[`canJoinAllApplications`](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/canjoinallapplications)、[`fullScreenAuxiliary`](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/fullscreenauxiliary)、[`moveToActiveSpace`](https://developer.apple.com/documentation/appkit/nswindow/collectionbehavior-swift.struct/movetoactivespace)。
