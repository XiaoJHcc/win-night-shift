//! 注册表变更监听：系统设置 App 等外部途径改动夜间模式/亮暗主题时，通知浮窗同步。
//!
//! `RegNotifyChangeKeyValue` 是一次性的：每次触发后重新挂。监听在独立线程里
//! 阻塞等待，命中后给隐藏消息窗口投 `WM_SETTINGS_CHANGED`，由主消息循环转到
//! `flyout::on_external_change` 在 UI 线程做同步（WinUI 控件只能在本线程碰）。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_SUCCESS, HANDLE, HWND, LPARAM, WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegNotifyChangeKeyValue, HKEY, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_USER};

use crate::reg;

/// 自定义消息：被监听的注册表键发生变化（投给隐藏窗口，见 main.rs 的 wndproc）。
pub const WM_SETTINGS_CHANGED: u32 = WM_USER + 2;

/// 监听目标：子键路径 + 是否含子树。
///
/// 夜间模式的 CloudStore 键按设备/账户分键、键集随显示器增减，直接听
/// `Current` 整棵子树（`Data` 值挂在叶键上，LAST_SET 子树监听能收到）。
/// 这棵子树下还有别的设置项，会多收一些无关通知，但处理器在面板隐藏时
/// 直接返回，代价可忽略。
const WATCHES: &[(&str, bool)] = &[
    (
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current",
        true,
    ),
    // 亮/暗模式。
    (
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        false,
    ),
];

/// 启动监听线程。`notify` 为接收 `WM_SETTINGS_CHANGED` 的隐藏窗口。
/// 所有键都打不开时静默放弃——面板退化为仅打开时现读一次。
pub fn start(notify: HWND) {
    // HWND 不实现 Send，按裸值跨线程传递（窗口句柄本质是无所有权的数值）。
    let raw = notify.0 as isize;
    std::thread::spawn(move || run(HWND(raw as *mut _)));
}

fn run(notify: HWND) {
    let mut watches: Vec<(HKEY, HANDLE, bool)> = Vec::new();
    for &(path, subtree) in WATCHES {
        let Some(hkey) = reg::open_notify(path) else {
            continue;
        };
        // 自动复位事件：WaitForMultipleObjects 返回即复位，无需手动 ResetEvent。
        match unsafe { CreateEventW(None, false, false, PCWSTR::null()) } {
            Ok(event) => watches.push((hkey, event, subtree)),
            Err(_) => unsafe {
                let _ = RegCloseKey(hkey);
            },
        }
    }
    if watches.is_empty() {
        return;
    }

    let events: Vec<HANDLE> = watches.iter().map(|w| w.1).collect();
    loop {
        // 先全部重新挂好再等待：触发与重新挂之间的变更不会丢
        // （RegNotifyChangeKeyValue 挂上的瞬间起算，挂之前的状态靠打开面板时的
        // 现读兜底）。
        for &(hkey, event, subtree) in &watches {
            let rc = unsafe {
                RegNotifyChangeKeyValue(
                    hkey,
                    subtree,
                    REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET,
                    Some(event),
                    true,
                )
            };
            if rc != ERROR_SUCCESS {
                return; // 键被删或句柄失效，监听整体放弃。
            }
        }
        let r = unsafe { WaitForMultipleObjects(&events, false, INFINITE) };
        if r == WAIT_FAILED || r.0 - WAIT_OBJECT_0.0 >= events.len() as u32 {
            return;
        }
        unsafe {
            let _ = PostMessageW(Some(notify), WM_SETTINGS_CHANGED, WPARAM(0), LPARAM(0));
        }
    }
}
