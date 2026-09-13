//! mscms 通道生命周期测试：设 2700K → DeleteDC → 进程退出，**不恢复**。
//! 观察屏幕是否自动回弹（决定预览层的退出交接与崩溃残留策略）。
//! 若未回弹：开关一次系统夜灯，或再跑 `cargo run --example probe6` 走完即恢复。
//! 用法：cargo run --example probe7
#![allow(dead_code)]

use windows::core::{w, PCWSTR};
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

fn wstr(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn main() {
    unsafe {
        let h = LoadLibraryExW(w!("mscms.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).expect("mscms");
        let proc = GetProcAddress(h, windows::core::PCSTR(204usize as *const u8)).expect("204");
        type SetTempFn = unsafe extern "system" fn(
            windows::Win32::Graphics::Gdi::HDC,
            f32,
            u32,
            u32,
        ) -> i32;
        let set_temp: SetTempFn = std::mem::transmute(proc);

        for i in 0..16u32 {
            let mut dev = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            if !EnumDisplayDevicesW(PCWSTR::null(), i, &mut dev, 0).as_bool() {
                break;
            }
            if dev.StateFlags & DISPLAY_DEVICE_ACTIVE
                == windows::Win32::Graphics::Gdi::DISPLAY_DEVICE_STATE_FLAGS(0)
            {
                continue;
            }
            let name = wstr(&dev.DeviceName);
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let hdc =
                CreateDCW(w!("DISPLAY"), PCWSTR(wide.as_ptr()), PCWSTR::null(), None);
            if hdc.is_invalid() {
                continue;
            }
            let r = set_temp(hdc, 2700.0, 0, 0);
            println!("{name}: set(2700K) -> {r}，DeleteDC 后退出");
            let _ = DeleteDC(hdc);
        }
        println!("进程退出，未恢复。请观察屏幕是否自动回弹到正常色温。");
    }
}
