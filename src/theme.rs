//! 亮/暗模式切换：Personalize 键 + ImmersiveColorSet 广播。

use crate::reg;
use windows::core::w;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    SendNotifyMessageW, HWND_BROADCAST, WM_SETTINGCHANGE,
};

const PERSONALIZE_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// 当前是否深色模式（以应用主题为准；值缺失时视为浅色）。
pub fn is_dark() -> bool {
    reg::read_dword(PERSONALIZE_KEY, "AppsUseLightTheme") == Some(0)
}

/// 切换亮/暗：应用与系统（任务栏等）一起切，并广播让所有进程即时刷新。
pub fn set_dark(dark: bool) -> bool {
    if is_dark() == dark {
        return true;
    }
    let v = u32::from(!dark);
    let ok = reg::write_dword(PERSONALIZE_KEY, "AppsUseLightTheme", v)
        && reg::write_dword(PERSONALIZE_KEY, "SystemUsesLightTheme", v);
    if ok {
        unsafe {
            let _ = SendNotifyMessageW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
            );
        }
    }
    ok
}
