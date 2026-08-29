# AGENTS.md

面向开发者的项目说明。README 只保留用户向内容（功能/运行/构建），
开发需要知道的内部机制都在这里。

## 项目简介

win-night-shift：常驻托盘的 Windows 小工具（Rust + WinUI 3 XAML 岛浮窗），
功能为深色模式开关、夜间模式（Night Light）开关与色温强度拉条、开机自启。
`#![windows_subsystem = "windows"]`，无控制台窗口。

## 常用命令

```sh
cargo build --release        # 产物 target/release/win-night-shift.exe
cargo test                   # nightlight.rs 的 blob 编解码单测
cargo run --example makeicon # 重新生成 img/*.ico（改图标素材/颜色后跑）
cargo run --example dump     # 诊断：dump CloudStore 夜间模式键
```

## 模块结构

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 入口、DPI 感知（Per-Monitor-V2）、隐藏消息窗口、消息循环、托盘事件分发、`TRAY_ICON_DIRTY` |
| `src/flyout.rs` | WinUI 3 托盘浮窗（XAML 岛、托盘锚定定位、卡片样式、失焦收起） |
| `src/tray.rs` | 托盘图标与右键菜单（muda：开机自启勾选/设置/退出）；图标按任务栏主题选白/黑 |
| `src/nightlight.rs` | 夜间模式 CloudStore 注册表 blob 读写（开关 + 强度） |
| `src/theme.rs` | Personalize 键亮暗切换 + `ImmersiveColorSet` 广播 |
| `src/autostart.rs` | HKCU\...\Run 开机自启 |
| `src/watch.rs` | 注册表变更监听线程（CloudStore / Personalize），命中后投 `WM_SETTINGS_CHANGED` 给隐藏窗口 |
| `src/reg.rs` | HKCU 注册表读写最小封装 |
| `build.rs` | 嵌入 `img/icon.ico`（琥珀色，exe 应用图标，资源 ID 1） |
| `examples/makeicon.rs` | 一次性图标生成器（resvg 渲染 lucide moon → ico） |

## 图标管线

素材 `img/moon.svg`（lucide moon，ISC 许可），`cargo run --example makeicon`
生成三份多尺寸 ico（16/24/32/48/64/256）：

- `img/icon.ico` —— 琥珀色 `#F5B942`，exe 应用图标（build.rs 嵌入为资源 ID 1）；
- `img/tray-white.ico` / `img/tray-black.ico` —— 托盘图标，`include_bytes!`
  内嵌进 exe（`src/tray.rs`），缺失会编译失败（素材应入库）。

托盘图标配色跟随任务栏主题：深色任务栏用白 glyph、浅色用近黑 `#181919`
（取色自系统托盘图标；图标颜色本身没有可查询的系统值）。主题依据是
`HKCU\...\Themes\Personalize` 的 `SystemUsesLightTheme`。

渲染要点（踩过的坑，勿回退）：

- 托盘素材边距 1px（`viewBox="-1 -1 26 26"`）、笔画保持 lucide 原始 2
  （边距 2px 被反馈偏小，笔画 2.5 被反馈偏粗）；
- 托盘取图标时按 `SM_CXSMICON` 从内嵌 ico 挑**原尺寸档**解码为 RGBA，
  全程不缩放——`Icon::from_resource` 让系统缩放会糊，已弃用该路径；
- 亮/暗主题切换即时生效：watch 线程监听 Personalize 键 → 投
  `WM_SETTINGS_CHANGED` → wndproc 置 `TRAY_ICON_DIRTY` → 主循环调
  `Tray::refresh_icon()` 重算并 `set_icon`。

## 夜间模式（CloudStore）读写策略

夜间模式没有公开 API，使用未文档化的 CloudStore 注册表 blob（社区逆向，
Win10 2004+ / Win11 通用，写入立即生效）。blob 操作均带格式校验，
格式不符时静默失败不 panic。实测确认的策略（Win11）：

- **写入只写主设置键**（`default$...bluelightreduction.settings`），与系统
  设置 App 的写入面一致：只写主键即可让屏幕色温即时生效且长期保持。
  per-device 键是系统维护的派生副本（系统落盘时会把主键值同步过去），
  外部写它对显示没有效果，本工具不写它——早期版本高频同时写主键+per-device
  曾被系统判定冲突、把夜间模式打回关闭。
- **读取取「注册表最后写入时间最新」的那把设置键**：系统设置拖强度拉条只写
  主设置键且延迟落盘（拖动时不写，关闭设置页后才刷入），多把键的值可能互相
  矛盾，不能按固定优先级读。
- 系统设置 App 对拉条的修改是**延迟落盘**的：面板会在系统实际写入注册表时
  （通常是关闭设置页后）实时跟随，而不是拖动的当下——这是系统行为，不是
  面板漏更新。

## 外部变更监听与面板同步

watch 线程用 `RegNotifyChangeKeyValue` 监听 CloudStore（夜间模式）与
Personalize（亮暗）两处键，落盘时投 `WM_SETTINGS_CHANGED` 给隐藏消息窗口，
主线程据此同步面板控件并刷新托盘图标（见上文图标管线）。`RegNotifyChangeKeyValue`
是一次性的，每次触发后重新挂。面板每次打开也会现读一次系统状态；拖动拉条
期间不回写，避免把滑条从用户手下拽走。

## 其他约定

- 强度拉条拖动时注册表写入做 300ms 节流（密集写入会被系统判定冲突），
  松手时补写最终值。
- winui3 crate 从 git 拉取（crates.io 的 0.4.5 缺 `UI_Xaml_Hosting` 等
  feature），首次构建较慢。
- UI 依赖系统已装的 WindowsAppRuntime（WinUI 3 运行时），运行时缺失时
  不做回退，直接退出。
