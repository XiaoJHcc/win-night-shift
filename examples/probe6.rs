//! mscms!InternalSetDeviceTemperature（ordinal 204）直调验证。
//!
//! 背景：blr 反汇编显示引擎的「Dem 路径」= mscms.dll ordinal 204，
//! 签名 InternalSetDeviceTemperature(HDC, float kelvin, u32, u32) -> BOOL（非 0 成功）。
//! 引擎拉条拖动时若 ShouldUseDESPath=false 就走它（kelvin, 0, 0）；
//! SetTemperatureDem 的测试路径也用它 (HDC, 6500.0f, 0, 0) 复位。
//! 本机 DES 路径（StartNightLightTransition）色温呈现为过饱和黄，
//! 疑似本机引擎实际走这条 mscms 路径，故直调验证。
//!
//! 序列（每步 3 秒）：2700K → 1500K → 恢复 6500K。restore 固定 6500（中性）。
//! 用法：cargo run --example probe6
#![allow(dead_code)]

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE, HDC,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

fn wstr(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// 活动显示器的 GDI 设备名（\\.\DISPLAY1 形式）。
fn display_names() -> Vec<String> {
    let mut names = Vec::new();
    unsafe {
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
            names.push(wstr(&dev.DeviceName));
        }
    }
    names
}

fn main() {
    unsafe {
        let h: HMODULE =
            LoadLibraryExW(w!("mscms.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).expect("load mscms");
        // ordinal 204 = InternalSetDeviceTemperature。
        let proc = GetProcAddress(h, windows::core::PCSTR(204usize as *const u8))
            .expect("ordinal 204");
        type SetTempFn = unsafe extern "system" fn(HDC, f32, u32, u32) -> i32;
        let set_temp: SetTempFn = std::mem::transmute(proc);

        let mut dcs = Vec::new();
        for name in display_names() {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let hdc = CreateDCW(w!("DISPLAY"), PCWSTR(wide.as_ptr()), PCWSTR::null(), None);
            if hdc.is_invalid() {
                println!("{name}: CreateDCW failed");
                continue;
            }
            println!("{name}: HDC ok");
            dcs.push((name, hdc));
        }
        if dcs.is_empty() {
            println!("无可用 HDC，退出");
            return;
        }

        let apply = |kelvin: f32| {
            for (name, hdc) in &dcs {
                let r = set_temp(*hdc, kelvin, 0, 0);
                println!("  {name}: set({kelvin}K) -> {r}");
            }
        };
        let pause = |secs: u64| std::thread::sleep(std::time::Duration::from_secs(secs));

        println!("[1/3] 2700K：应变为合理暖色（同系统夜灯观感）");
        apply(2700.0);
        pause(3);
        println!("[2/3] 1500K：更暖");
        apply(1500.0);
        pause(3);
        println!("[3/3] 恢复 6500K");
        apply(6500.0);

        for (_, hdc) in dcs {
            let _ = DeleteDC(hdc);
        }
        println!("完成。屏幕应已恢复中性。");
    }
}
