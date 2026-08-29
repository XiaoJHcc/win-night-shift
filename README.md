# win-night-shift

**常驻托盘的 Windows 小工具：夜间模式（Night Light）开关 + 强度拉条，亮/暗模式一键切换。**

UI 是 WinUI 3 托盘浮窗（XAML Islands）：控件、亚克力、圆角、失焦收起等由系统已装的
WindowsAppRuntime 框架包提供，单 exe、不引入 .NET；运行时缺失时程序直接退出（无回退）。

## 功能

- **夜间模式**：开关 + 0–100 强度拉条（拖动即生效），对应系统设置里的「夜间模式」。
- **颜色主题**：深色模式开关，应用与系统界面（任务栏等）同时切换并即时刷新。
- **开机自启**：托盘右键菜单勾选项，写 HKCU\...\Run。
- 面板打开期间监听注册表变化：系统设置等外部途径改了上述项，面板控件实时跟随。
- 托盘左键弹出/收起浮窗；浮窗失焦自动收起。右键弹出系统样式菜单：
  开机自启（勾选）、设置（预留）、退出。

## 运行

直接双击 `win-night-shift.exe`，托盘出现图标即在运行。左键点托盘图标弹出浮窗，右键点弹出菜单。

依赖系统已装的 WindowsAppRuntime（WinUI 3 运行时，Win11 自带；Win10 1809+ 可另行安装）。

## 构建

```sh
cargo build --release
# 产物：target/release/win-night-shift.exe
```

`#![windows_subsystem = "windows"]` 已去掉控制台窗口。
winui3 crate 从 git 拉取（crates.io 版本缺 UI_Xaml_Hosting 等 feature），首次构建较慢。

应用/托盘图标：把 `img/icon.ico` 放进仓库后重新构建即可嵌入；未提供时托盘回退到系统库存图标。

## 模块结构

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 入口、DPI 感知、隐藏消息窗口、消息循环、托盘事件分发 |
| `src/flyout.rs` | WinUI 3 托盘浮窗（XAML 岛、托盘锚定定位、卡片样式、失焦收起） |
| `src/tray.rs` | 托盘图标与右键菜单（muda，开机自启勾选/设置/退出）；图标缺失时回退系统库存图标 |
| `src/nightlight.rs` | 夜间模式 CloudStore 注册表 blob 读写（开关 + 强度） |
| `src/theme.rs` | Personalize 键亮暗切换 + `ImmersiveColorSet` 广播 |
| `src/autostart.rs` | HKCU\...\Run 开机自启 |
| `src/watch.rs` | 注册表变更监听线程（CloudStore/Personalize），命中后投消息给隐藏窗口 |
| `src/reg.rs` | HKCU 注册表读写最小封装 |
| `build.rs` | 嵌入 `img/icon.ico`（存在时） |

## 说明

夜间模式没有公开 API，本工具使用未文档化的 CloudStore 注册表 blob（社区逆向格式，Win10 2004+ / Win11 通用，写入立即生效）。blob 操作均带格式校验，格式不符时静默失败不 panic。

实测确认的读写策略（Win11）：

- **写入只写主设置键**（`default$...bluelightreduction.settings`），与系统设置 App 的写入面一致：只写主键即可让屏幕色温即时生效且长期保持。per-device 键是系统维护的派生副本（系统落盘时会把主键值同步过去），外部写它对显示没有效果，本工具不写它——早期版本高频同时写主键+per-device 曾被系统判定冲突、把夜间模式打回关闭。
- **读取取「注册表最后写入时间最新」的那把设置键**：系统设置拖强度拉条只写主设置键且延迟落盘（拖动时不写，关闭设置页后才刷入），多把键的值可能互相矛盾，不能按固定优先级读。
- 系统设置 App 对拉条的修改是**延迟落盘**的：面板会在系统实际写入注册表时（通常是关闭设置页后）实时跟随，而不是拖动的当下——这是系统行为，不是面板漏更新。

面板打开期间，watch 线程用 `RegNotifyChangeKeyValue` 监听 CloudStore（夜间模式）、Personalize（亮暗）两处注册表键，外部改动落盘时实时同步到面板控件；拖动拉条期间不回写，避免把滑条从用户手下拽走。面板每次打开也会现读一次系统状态。

强度拉条拖动时注册表写入做了 300ms 节流（密集写入会被系统判定冲突），松手时补写最终值。

## 待办

- `img/icon.ico` 图标素材待提供。
