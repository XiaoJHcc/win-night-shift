# 夜间模式私有 API 逆向笔记（2026-09-13，Win11 25H2）

本文件记录一次完整逆向调查的结论，供后续会话直接续用。
起因：25H2 上外部写 CloudStore 只落盘不实时应用（唯一触发是 state 键状态跳变，
跳变必闪），需要系统设置 App 同款的实时色温通道。

## 架构（实测确认）

- **explorer.exe** 加载 `C:\Windows\System32\Windows.Shell.BlueLightReduction.dll`，
  内驻夜间模式引擎：WinRT 类
  `Windows.Internal.Shell.BlueLightReduction.BlueLightReductionManager`
  （service-host 组件，经 CloudStoreDataWatcher 监听 CloudStore，
  内部 `ColorTemperatureControl` 执行应用：`SetTargetTemperature(float)`、
  `SetPreviewTemperatureChanges(bool)`、`RefreshAllMonitorTemperatures`、
  `ShouldUseDESPath`）。
- 引擎应用路径二选一：DES（Display Enhancement Service，svchost，
  `Microsoft.Graphics.Display.DisplayEnhancementService.dll`）或 "Dem" HDC 路径。
  本机 LUT 无线性外暖色（夜灯非 gamma LUT 路径），`EnableModernNightLight=0`
  （白点路径未启用）。引擎实际路径**仍未最终定位**（D3DKMT 级别嫌疑）。
- 客户端 API 模块：`C:\Windows\System32\Windows.Internal.Graphics.Display.DisplayEnhancementManagement.dll`
  （explorer 也在用），WinRT 类
  `Windows.Internal.Graphics.Display.DisplayEnhancementManagement.DisplayEnhancementManagement`。

## DEM WinRT API 调用方法（probe3/probe4 实测可用）

1. `LoadLibraryExW` + `DllGetActivationFactory(类名)`（绕开注册，普通进程可调）。
2. 工厂 QI 静态接口 `{22771028-1658-4E5E-AB77-303691A86FDA}`，
   槽位 6 = `FromIdAsync(HSTRING monitorId, IAsyncOperation**)`。
   **monitorId 必须是显示设备接口路径**（`EnumDisplayDevicesW(adapter, 0, &dev, 1)`
   的 DeviceID，形如 `\\?\DISPLAY#ICD753C#5&162ffb0&0&UID4353#{e6f07b5f-...}`；
   `\\.\DISPLAY1` 形式返回 E_INVALIDARG）。
3. 异步操作：**状态在单独的 IAsyncInfo 接口**（IID `00000036-...-046`，
   槽位 7=get_Status、8=get_ErrorCode，轮询即可）；**put_Completed/get_Completed/
   GetResults 在另一接口**（IID `{BD9CB39A-6F09-5209-A656-1306F0DDF5C9}`，
   槽位 6=put_Completed、8=GetResults）。op 上还有第三接口
   `{7A900AF8-...}`（含 Dismiss 方法）。
4. GetResults 得到实例，主接口 IID **`{CDA29A3E-9E7E-4B86-8F5F-368AA0710008}`**
   （IDisplayEnhancementManagement）。

## IDisplayEnhancementManagement 虚表（槽位，已逐槽符号解析）

- 6 `get_IsNightLightCapable(bool*)`；7 `get_IsNightLightOverridden(bool*)`；
  8 `StartNightLightTransition(...)`；9/10 add/remove_IsNightLightOverriddenChanged；
- 11 `get_EnableModernNightLight(bool*)`；
- 12..19 AdaptiveColor 系列（Capability/Policy/Strength/On 的 get/put）；
- 20 `get_CurrentWhitePoint(ChromaticityXY* out)`（两个 float）；
  21 `put_CurrentWhitePoint(ChromaticityXY)`（按值传两个 float，打包一个 u64）；
- 22..33 各 Changed 事件 add/remove；34+ 亮度系列。

## 实测结论与坑

- `put_CurrentWhitePoint`：hr=0 但**本机无视觉效果**（只写服务内目标值，
  本机 EnableModernNightLight=0，白点路径未启用）。D65 = (0.3127, 0.3290)。
- `StartNightLightTransition(float kelvin, double durationMs, u32 enum)`：语义已确认
  （见上节）。**本机色彩映射错误（过饱和黄），不可用**；恢复调用为
  `(6500, 0, 0)`。效果驱动级、粘性到注销。~~未完成语义映射前禁止再调~~
  语义已明，但本机改走 mscms 通道（见下节），无需再调。
- 服务 `DisplayEnhancementService` 普通用户无权重启（需管理员）。
- RPC 面（服务 PDB 符号）：`DeManagementRpcServerSetCurrentWhitePoint`、
  `DeManagementRpcServerStartNightLightTransition`、`GetIsNightLightCapable` 等。
  服务端有 `NightLightManagerImpl::SetTargetTransition(ColorTarget)`。

## StartNightLightTransition 参数语义（2026-09-13 反汇编确认）

链路：BLR `ColorTemperatureControl::SetTemperatureDem`（blr RVA 0x2CC0C）
→ winrt consume 包装（0x2CF68，vtable 槽位 8）→ DEM 客户端
（dem RVA 0xF4E0，float/double/enum 原样透传）→ RPC → DES 服务端
（des RVA 0x1A4A0：组 `DeManagementTransitionSettingType{double ms; float k; int 1}`
→ `NightLightManagerImpl::SetTargetTransition`（des RVA 0x39030，入队 + 信号工作线程））。

- **arg1 float = 目标色温（开尔文绝对值，6500=中性）**——引擎 lambda
  （blr 0x2C7B0）把温度 float 直接装 xmm1；
- **arg2 double = 过渡时长（毫秒）**——`cvtsi2sd(duration_ms)` 装 xmm2；
- **arg3 enum**：0=Instant(0ms) / 1=Fast(2000ms) / 2=Gradual(120000ms)，
  映射见 BLR `GetTransitionTime`（blr 0x2C610）。
- 系统拉条拖动路径 `SetTargetTemperatureOnMonitorImmediate`（blr 0x2CB80）：
  DES 路径就是 `(kelvin, 0.0, 0)`。
- 旧 probe「无法恢复」之谜解开了：所有恢复尝试都把 6500 放进了 double
  （时长）、float 传了 0 —— 目标 0K 非法被忽略。真正的恢复 = `(6500, 0, 0)`。
- **但本机 DES 路径色彩映射错误**：probe5 实测 (2700K/1500K) 呈现为
  过饱和「鲜艳黄」而非系统夜灯暖色（6500 恢复有效）。推测本机 DES 服务端
  缺少显示器适配数据（引擎在本机根本不走 DES 路径，见下节），该路径不可用。

## 本机真实系统通道：mscms!InternalSetDeviceTemperature（ordinal 204）

引擎另一路（"Dem" HDC 路径，`ShouldUseDESPath`=false 时）走
`mscms.dll` 延迟加载导出 **ordinal 204 = `InternalSetDeviceTemperature`**
（同批 ordinal 206 = `InternalGetDeviceGammaCapability`）：

- 签名（从两处调用点确认）：`BOOL InternalSetDeviceTemperature(HDC, float kelvin, u32=0, u32=0)`，
  非 0 成功。HDC 由 `CreateDCW("DISPLAY", \\.\DISPLAYn, ...)` 按屏创建。
- **probe6 实测（2026-09-13）**：2700K/1500K 呈现为与系统夜灯一致的合理暖色，
  6500K 恢复有效。**这就是本机引擎的实际应用通道。**
- **生命周期（probe7 实测）**：效果**粘性**——DeleteDC + 进程退出后不回弹，
  保持到下一次有人设色温。恢复工具：probe8（全屏 set 6500 或指定 K）。
- 天然覆写路径：系统夜灯开关切换 / 系统设置拖条时引擎重应用注册表值，
  会覆盖本通道的任何残留（崩溃残留非永久损坏，下次引擎应用即自愈）。

## 预览层重做方向（已定，待实施）

「拖动走 mscms 系统通道实时预览 + 松手写 CloudStore 落盘」：

- 拖动：每活动显示器 `CreateDCW` + `set(strength_to_kelvin(v), 0, 0)`，即时生效，
  色彩与系统夜灯完全一致，截屏行为 = 系统夜灯（干净）；
- 松手：现有 `nightlight::set_strength` 落盘；mscms 效果粘性，无需交接，
  屏幕自然保持目标值；
- 退出/取消：若预览值 ≠ 系统应有值（夜灯开?注册表 K:6500），set 应有值恢复；
- 崩溃残留：粘性无法自动回收，但任何引擎重应用（夜灯开关切换等）即覆盖；
  可选「预览生效」标志 + 下次启动恢复的兜底；
- Mag 矩阵 preview 模块（相对矩阵偏色 + 截屏污染）整体退役，可留作
  mscms 缺失时的降级（init 探测 ordinal 204）。

## 备选路线（未展开）

- 驱动 explorer 引擎本体：BlueLightReductionManager 是 service-host 组件，
  经 IProfferService 发布服务（PublishServices/v_QueryService 在基类
  CServiceHostComponent 实现，服务 GUID 在 BLR dll 常量里，可从反汇编提取），
  客户端（设置 App）经 shell 的 IServiceProvider::QueryService 取得。
  管理器实例接口 `{A4B478CF-3CCF-4DF0-A436-4B3B1474998B}` 只有宿主回调
  （OnMonitorConnected 等），色温控制在发布的服务接口上（应即
  ColorTemperatureControl 的公开虚方法：SetTargetTemperature(float)、
  SetPreviewTemperatureChanges(bool)）。

## 工具与产物（target/ 下）

- `examples/probe.rs`~`probe4.rs`：激活/枚举/白点/transition 探针（probe3 最完整）。
- `examples/probe5.rs`：StartNightLightTransition 语义验证（DES 路径，本机色彩错误）。
- `examples/probe6.rs`：mscms ordinal 204 通道验证（正确暖色，通过）。
- `examples/probe7.rs`：mscms 通道生命周期测试（粘性确认）。
- `examples/probe8.rs`：mscms 恢复工具（全屏 set 指定 K，默认 6500）。
- `target/dia2dump/dia2dump.exe`：自编译 DIA2Dump（NoRegCoCreate 改造，源码在
  target/dia2dump-src/）；PDB：target/{blr,dem,des}.pdb 对应 publics 文本。
- 关键 PDB GUID（符号服务器路径用）：BLR dll {E59E86CE-1F81-BB50-CBF8-77BDE67456A6}1；
  DEM {735444B0-4D24-7257-5CB4-C67B0289513F}1；
  DES {292FB257-B4BB-8F68-BFFA-5BD01ABB968C}1。
- 反汇编：target/dem-disasm.txt（DEM 全量）、target/des-disasm.txt（DES 全量）、
  target/blr-disasm.txt（BLR 全量）、target/blr-imports.txt。
  dumpbin 注意：经 git bash 调用时用 `-disasm:nobytes` 短横线参数形式
  （`/option` 会被 MSYS 路径转换吃掉；`//option` dumpbin 不认）。

## 当前代码状态

- `src/preview.rs`（DWM Mag 矩阵预览）已实现并入 release，但**有已知缺陷**：
  相对矩阵往冷拉时蓝增益>1 偏色（用户实测确认）；且松手后矩阵长期驻留
  会污染截屏。方向 A 的收尾方案（松手立即 disengage + 一次开关循环交接）
  未实施。用户产品要求：静止状态必须是系统暖色（截屏干净）+ 拖动实时预览。
