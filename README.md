# win-night-shift

**常驻托盘的 Windows 小工具：夜间模式（Night Light）开关 + 强度拉条，亮/暗模式一键切换。**

UI 是 WinUI 3 托盘浮窗（XAML Islands）：控件、亚克力、圆角、失焦收起等由系统已装的
WindowsAppRuntime 框架包提供，单 exe、不引入 .NET；运行时缺失时程序直接退出（无回退）。

## 功能

- **夜间模式**：开关 + 0–100 强度拉条（拖动即生效），对应系统设置里的「夜间模式」。
- **颜色主题**：深色模式开关，应用与系统界面（任务栏等）同时切换并即时刷新。
- **开机自启**：开关，写 HKCU\...\Run。
- 托盘左右键都弹出浮窗；浮窗失焦自动收起，底栏按钮退出程序。

## 运行

直接双击 `win-night-shift.exe`，托盘出现图标即在运行。左键或右键点托盘图标弹出浮窗。

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
| `src/tray.rs` | 托盘图标（不挂菜单，左右键事件交主循环）；图标缺失时回退系统库存图标 |
| `src/nightlight.rs` | 夜间模式 CloudStore 注册表 blob 读写（开关 + 强度） |
| `src/theme.rs` | Personalize 键亮暗切换 + `ImmersiveColorSet` 广播 |
| `src/autostart.rs` | HKCU\...\Run 开机自启 |
| `src/reg.rs` | HKCU 注册表读写最小封装 |
| `build.rs` | 嵌入 `img/icon.ico`（存在时） |

## 说明

夜间模式没有公开 API，本工具使用未文档化的 CloudStore 注册表 blob（社区逆向格式，Win10 2004+ / Win11 通用，写入立即生效）。blob 操作均带格式校验，格式不符时静默失败不 panic。系统设置 App 的「夜间模式」页打开时，开关状态会即时同步；强度滑块需重开设置页才刷新（已知系统行为）。

强度拉条拖动时注册表写入做了 300ms 节流（密集写入会被系统判定冲突并把夜间模式打回关闭），松手时补写最终值。

## 待办

- 面板打开期间，系统设置里改了夜间模式开关/强度时，面板控件不跟随（仅打开浮窗时现读一次）。下版本监听 CloudStore 键变化做实时同步。
- `img/icon.ico` 图标素材待提供。
