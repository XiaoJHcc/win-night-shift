//! 开机自启：读写 HKCU\Software\Microsoft\Windows\CurrentVersion\Run。

use crate::reg;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "win-night-shift";

/// 当前是否已注册开机自启。
pub fn is_autostart() -> bool {
    reg::value_exists(RUN_KEY, VALUE_NAME)
}

/// 设置或清除开机自启。
pub fn set_autostart(enabled: bool) -> bool {
    if enabled {
        let Some(exe) = std::env::current_exe().ok() else {
            return false;
        };
        reg::write_string(RUN_KEY, VALUE_NAME, &format!("\"{}\"", exe.to_string_lossy()))
    } else {
        reg::delete_value(RUN_KEY, VALUE_NAME)
    }
}
