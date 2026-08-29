# win-night-shift

**常驻托盘的 Windows 小工具：深色模式 / 夜间模式 / 色温拉条。**

## 功能

- **深色模式**：即 Windows 11 系统设置「个性化 → 颜色 → 选择模式 → 深色」的快捷开关。
- **夜间模式**：即 Windows 11 系统设置「系统 → 显示 → 夜间模式」的快捷开关。
- **原彩显示**：众所周知 Windows 没有原彩显示，所以这个是空占位。
- **色温拉条**：夜间模式的色温强度拉条，拖动时实时生效，松手时写入注册表。

## 运行

直接双击 `win-night-shift.exe`，托盘出现图标即在运行。
左键点托盘图标弹出浮窗面板（快捷开关），右键点弹出菜单（开机自启）。

> UI 依赖系统已装的 WindowsAppRuntime（WinUI 3 运行时；Win10 1809+）

## 构建

```sh
cargo build --release
# 产物：target/release/win-night-shift.exe
```

`#![windows_subsystem = "windows"]` 已去掉控制台窗口。
winui3 crate 从 git 拉取（crates.io 版本缺 UI_Xaml_Hosting 等 feature），首次构建较慢。

应用/托盘图标已内嵌在仓库（`img/*.ico`），构建时自动打包进 exe。

开发相关的内部机制（模块结构、CloudStore 读写策略、图标管线等）见 [AGENTS.md](AGENTS.md)。
