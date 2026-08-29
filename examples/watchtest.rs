//! 手动验证：`RegNotifyChangeKeyValue` 对 CloudStore 夜间模式键的监听是否会触发。
//!
//! 用法：先运行本程序（默认等待 300 秒），期间用系统设置或 `dump --set N`
//! 改动夜间模式强度/开关。每次收到通知打印一次时刻，超时退出。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_SUCCESS, WAIT_OBJECT_0};
use windows::Win32::System::Registry::{
    RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY,
    REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, REG_SAM_FLAGS,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

fn main() {
    let sub: Vec<u16> = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            None,
            REG_SAM_FLAGS(KEY_NOTIFY.0),
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            println!("open failed");
            return;
        }
        let event = CreateEventW(None, false, false, PCWSTR::null()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let mut n = 0u32;
        while let Some(remain) = deadline.checked_duration_since(std::time::Instant::now()) {
            let rc = RegNotifyChangeKeyValue(
                hkey,
                true,
                REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET,
                Some(event),
                true,
            );
            if rc != ERROR_SUCCESS {
                println!("re-arm failed: {rc:?}");
                break;
            }
            let r = WaitForSingleObject(event, remain.as_millis() as u32);
            if r == WAIT_OBJECT_0 {
                n += 1;
                println!("notification #{n}");
            } else {
                break; // 超时
            }
        }
        println!("total notifications: {n}");
        let _ = RegCloseKey(hkey);
    }
}
