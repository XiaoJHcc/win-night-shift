//! 拖动强度拉条时的实时色温预览（mscms 系统通道，绝对色温）。
//!
//! 背景（2026-09，Win11 25H2）：外部写 CloudStore 只落盘、屏幕不跟随，唯一的
//! 注册表触发是 state 键状态跳变（必闪）。曾用 MagSetFullscreenColorEffect
//! DWM 相对矩阵做预览，但有已知缺陷（冷拉偏色、矩阵驻留污染截屏），已退役。
//!
//! 本通道（逆向确认 + 实测，细节见 RESEARCH-nightlight-api.md）：本机夜间模式
//! 引擎（explorer 内 BlueLightReduction.dll）在 `ShouldUseDESPath`=false 时走
//! `mscms.dll` 的延迟加载导出 **ordinal 204 = `InternalSetDeviceTemperature`**，
//! 签名 `(HDC, float kelvin, u32=0, u32=0) -> BOOL`，HDC 按屏 CreateDCW。
//! 与系统夜灯同一条应用路径：色彩完全一致、截屏行为同系统夜灯。
//! （另一条 DES 路径 StartNightLightTransition 本机色彩映射错误，不可用。）
//!
//! 生命周期（实测）：效果是**粘性的绝对设定**——DeleteDC、进程退出均不回弹，
//! 保持到下一次有人设色温。因此：
//!  * 拖动 = 直接设目标色温，松手落盘后屏幕与注册表天然一致，零闪烁零交接；
//!  * 夜灯开关切换、设置 App 拖条时引擎重应用注册表值，自然覆盖我们的预览
//!    （`disengage` 因此只清跟踪、不动硬件）；
//!  * 需要主动收敛的时刻（松手写失败扳回、外部非回声变更、拖动中退出）调
//!    `restore_system`，把硬件设回系统应有值；
//!  * 崩溃/被杀会残留预览色温：首次进入预览时写 `PreviewEngaged` 注册表标志，
//!    正常收尾清除；下次启动发现残留标志即按系统应有值恢复一次兜底。
//!    即使兜底失效，任何一次引擎重应用（夜灯开关切换等）也会覆盖残留。
//!
//! 降级：mscms/ordinal 204 缺失（非夜灯机型）时 `set_preview` 静默不做事，
//! `engaged` 恒 false，松手走 flyout 的开关循环降级（`nightlight::reapply`）。
//!
//! 状态跟踪：`APPLIED`（系统当前显示的强度）只在启动/开关/外部变更时按
//! 注册表校准（那些时刻系统必然重新应用了注册表值），供降级路径判断。

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE,
    DISPLAY_DEVICE_STATE_FLAGS, HDC,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

/// InternalSetDeviceTemperature 未按名导出，只按序号（mscms 延迟加载导入，
/// 逆向自 BlueLightReduction.dll 的导入表）。
const SET_TEMPERATURE_ORDINAL: usize = 204;
/// 夜间模式关着时的中性色温。
const NEUTRAL_KELVIN: u32 = 6500;

/// 崩溃残留兜底标志：预览生效期间为 1，正常收尾删除。
const FLAG_KEY: &str = r"Software\win-night-shift";
const FLAG_VALUE: &str = "PreviewEngaged";

type SetDeviceTemperatureFn = unsafe extern "system" fn(HDC, f32, u32, u32) -> i32;

struct Mscms {
    _h: HMODULE,
    set: SetDeviceTemperatureFn,
}

thread_local! {
    /// None = mscms 通道不可用（加载/取址失败），预览整体降级。
    static MSCMS: RefCell<Option<Mscms>> = const { RefCell::new(None) };
    /// 每台活动显示器一个 HDC 缓存（init/面板打开时重建，见 refresh_monitors）。
    static MONITORS: RefCell<Vec<HDC>> = const { RefCell::new(Vec::new()) };
    /// 系统当前实际显示的强度（0-100）；夜间模式关着或未知为 None。
    static APPLIED: Cell<Option<u32>> = const { Cell::new(None) };
    /// 预览目标强度；Some 即预览生效中。
    static PREVIEW: Cell<Option<u32>> = const { Cell::new(None) };
    /// 我们认知的夜间模式开关状态（回声判定用）。
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    /// 最后一次由我们落盘的强度（回声判定用）：外部变更通知到达时，
    /// 若注册表值仍等于它，说明那只是我们自己松手写入的回声，
    /// 预览必须保持生效，不能复位。
    static LAST_WRITTEN: Cell<Option<u32>> = const { Cell::new(None) };
}

/// 启动时调用一次：加载 mscms、建 HDC 缓存、处理崩溃残留、按注册表校准
/// APPLIED/ENABLED。通道缺失不致命——预览不可用，松手时降级为开关循环
/// 应用（见 flyout）。
pub fn init() {
    unsafe {
        let load = (|| -> Option<Mscms> {
            let h = LoadLibraryExW(w!("mscms.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            let set: SetDeviceTemperatureFn = std::mem::transmute(GetProcAddress(
                h,
                windows::core::PCSTR(SET_TEMPERATURE_ORDINAL as *const u8),
            )?);
            Some(Mscms { _h: h, set })
        })();
        MSCMS.with(|m| *m.borrow_mut() = load);
    }
    refresh_monitors();
    // 上次退出时预览没来得及收尾（崩溃/被杀）：色温残留，按系统应有值恢复。
    if crate::reg::read_dword(FLAG_KEY, FLAG_VALUE) == Some(1) {
        crate::reg::delete_value(FLAG_KEY, FLAG_VALUE);
        if let Some(k) = system_expected_kelvin() {
            set_all(k);
        }
    }
    let enabled = crate::nightlight::get_enabled().unwrap_or(false);
    set_system_state(enabled, enabled.then(|| crate::nightlight::get_strength()).flatten());
}

/// 重建每显示器 HDC 缓存（init 与每次面板打开时调用，覆盖显示器热插拔；
/// DeleteDC 不影响已设的粘性色温，预览中途重建安全）。
pub fn refresh_monitors() {
    MONITORS.with(|mons| {
        let mut mons = mons.borrow_mut();
        for hdc in mons.drain(..) {
            unsafe {
                let _ = DeleteDC(hdc);
            }
        }
        unsafe {
            for i in 0..16u32 {
                let mut dev = DISPLAY_DEVICEW {
                    cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                    ..Default::default()
                };
                if !EnumDisplayDevicesW(PCWSTR::null(), i, &mut dev, 0).as_bool() {
                    break;
                }
                if dev.StateFlags & DISPLAY_DEVICE_ACTIVE == DISPLAY_DEVICE_STATE_FLAGS(0) {
                    continue;
                }
                let hdc = CreateDCW(
                    w!("DISPLAY"),
                    PCWSTR(dev.DeviceName.as_ptr()),
                    PCWSTR::null(),
                    None,
                );
                if !hdc.is_invalid() {
                    mons.push(hdc);
                }
            }
        }
    });
}

/// 预览是否生效中（拖过拉条且尚未被系统状态变化接管）。
pub fn engaged() -> bool {
    PREVIEW.with(|p| p.get()).is_some()
}

/// 系统当前显示的强度（跟踪值，见模块头注释）。
pub fn applied() -> Option<u32> {
    APPLIED.with(|a| a.get())
}

/// 校准系统状态：启动、开关切换、外部变更后调用。
/// `strength` 为系统此刻实际显示的强度（夜间模式关着传 None）。
pub fn set_system_state(enabled: bool, strength: Option<u32>) {
    ENABLED.with(|e| e.set(enabled));
    APPLIED.with(|a| a.set(if enabled { strength } else { None }));
    LAST_WRITTEN.with(|w| w.set(None));
}

/// 记录一次由我们发起的强度落盘（松手写入），供回声判定。
pub fn record_written(strength: u32) {
    LAST_WRITTEN.with(|w| w.set(Some(strength)));
}

/// 外部变更通知是否为「我们自己松手写入」的回声：开关状态与注册表强度
/// 都与我们的记录一致。是回声则预览必须保持。
pub fn is_echo(enabled: bool, strength: Option<u32>) -> bool {
    ENABLED.with(|e| e.get()) == enabled
        && strength.is_some()
        && LAST_WRITTEN.with(|w| w.get()) == strength
}

/// 拖动拉条：把所有显示器设为强度对应的色温，立即生效。
/// 夜间模式关着（预览会造成「屏幕暖但系统夜灯关」的不一致态）、通道不可用
/// 或没有可用 HDC 时静默不做事（松手时走开关循环降级路径）。
pub fn set_preview(strength: u32) {
    if !ENABLED.with(|e| e.get()) || MSCMS.with(|m| m.borrow().is_none()) {
        return;
    }
    if MONITORS.with(|mons| mons.borrow().is_empty()) {
        return;
    }
    let first_engage = PREVIEW.with(|p| p.get()).is_none();
    set_all(crate::nightlight::strength_to_kelvin(strength));
    PREVIEW.with(|p| p.set(Some(strength)));
    if first_engage {
        // 崩溃残留兜底标志：从这次拖动到正常收尾之间被杀，下次启动恢复。
        crate::reg::write_dword(FLAG_KEY, FLAG_VALUE, 1);
    }
}

/// 清除预览跟踪（不动硬件）。夜灯开关切换、外部变更接管时调用——引擎
/// 会按注册表值重应用，自然覆盖预览色温，主动恢复反而多一次跳变。
pub fn disengage() {
    if PREVIEW.with(|p| p.take()).is_some() {
        crate::reg::delete_value(FLAG_KEY, FLAG_VALUE);
    }
}

/// 把硬件色温收敛回系统应有值并清除预览跟踪（预览未生效时为空操作）。
/// 松手写注册表失败（扳回）、外部非回声变更、进程退出交接时调用。
pub fn restore_system() {
    let Some(preview) = PREVIEW.with(|p| p.take()) else {
        return;
    };
    crate::reg::delete_value(FLAG_KEY, FLAG_VALUE);
    let Some(expected) = system_expected_kelvin() else {
        return;
    };
    if expected != crate::nightlight::strength_to_kelvin(preview) {
        set_all(expected);
    }
}

/// 进程退出前调用（main 消息循环结束后）：拖动中退出（预览值未落盘）时
/// 把硬件恢复系统应有值；已松手的情况下预览值==注册表值，restore_system
/// 对比相等不动硬件——粘性残留即正确状态，进程直接退出。
pub fn shutdown() {
    restore_system();
}

/// 系统应有色温：夜灯开取注册表强度对应的开尔文，关取中性 6500；
/// 夜灯开着但强度读不出时返回 None（不猜，免得改错）。
fn system_expected_kelvin() -> Option<u32> {
    if crate::nightlight::get_enabled().unwrap_or(false) {
        crate::nightlight::get_strength()
            .map(crate::nightlight::strength_to_kelvin)
    } else {
        Some(NEUTRAL_KELVIN)
    }
}

/// 对缓存的所有 HDC 设定色温；有失败（显示器被拔掉等）则重建缓存重试一次。
fn set_all(kelvin: u32) {
    let failed = MSCMS.with(|m| {
        let borrow = m.borrow();
        let Some(mscms) = borrow.as_ref() else {
            return false;
        };
        MONITORS.with(|mons| {
            let mut failed = false;
            for &hdc in mons.borrow().iter() {
                if unsafe { (mscms.set)(hdc, kelvin as f32, 0, 0) } == 0 {
                    failed = true;
                }
            }
            failed
        })
    });
    if failed {
        refresh_monitors();
        MSCMS.with(|m| {
            let borrow = m.borrow();
            let Some(mscms) = borrow.as_ref() else {
                return;
            };
            MONITORS.with(|mons| {
                for &hdc in mons.borrow().iter() {
                    unsafe {
                        (mscms.set)(hdc, kelvin as f32, 0, 0);
                    }
                }
            });
        });
    }
}
