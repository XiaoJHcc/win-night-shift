//! 实验：夜间模式是否落在 GPU gamma LUT 上，以及 SetDeviceGammaRamp 与其是
//! 「替换」还是「叠加」。
//!
//! 用法：
//!   cargo run --example gamma            读取并打印当前 LUT 采样
//!   cargo run --example gamma -- --set 2800 --hold 8
//!       保存当前 LUT → 刷上 2800K 的暖色 LUT → 保持 8 秒 → 恢复原 LUT
//!
//! 判定：
//!  * 夜间模式开启时读到的 LUT 非线性（蓝通道被压）→ 夜间模式走 LUT，
//!    SetDeviceGammaRamp 会替换它（预览可行）；
//!  * 读到的 LUT 是线性的 → 夜间模式不走 LUT（DWM 色彩变换等），
//!    刷 LUT 会叠加成双重暖色，预览方案需要换 MagSetFullscreenColorTransform。

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICE_STATE_FLAGS,
    CreateDCW, DeleteDC, EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE,
};
use windows::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};

/// Tanner Helland 色温 → RGB 增益（0..1），预览够用；与系统夜间模式的
/// 精确曲线不一致，只影响预览与最终效果的细微差别。
fn kelvin_to_rgb(k: u32) -> (f64, f64, f64) {
    let t = f64::from(k).clamp(1000.0, 40000.0) / 100.0;
    let r = if t <= 66.0 {
        1.0
    } else {
        (329.698727446 * (t - 60.0).powf(-0.1332047592) / 255.0).clamp(0.0, 1.0)
    };
    let g = if t <= 66.0 {
        ((99.4708025861 * t.ln() - 161.1195681661) / 255.0).clamp(0.0, 1.0)
    } else {
        (288.1221695283 * (t - 60.0).powf(-0.0755148492) / 255.0).clamp(0.0, 1.0)
    };
    let b = if t >= 66.0 {
        1.0
    } else if t <= 19.0 {
        0.0
    } else {
        ((138.5177312231 * (t - 10.0).ln() - 305.0447927307) / 255.0).clamp(0.0, 1.0)
    };
    (r, g, b)
}

fn ramp_for_kelvin(k: u32) -> [u16; 768] {
    let (r, g, b) = kelvin_to_rgb(k);
    let mut ramp = [0u16; 768];
    for i in 0..256 {
        let v = i as f64 / 255.0;
        ramp[i] = (v * r * 65535.0).round() as u16;
        ramp[256 + i] = (v * g * 65535.0).round() as u16;
        ramp[512 + i] = (v * b * 65535.0).round() as u16;
    }
    ramp
}

fn print_samples(tag: &str, ramp: &[u16; 768]) {
    println!("{tag}:");
    for i in [0usize, 1, 64, 128, 192, 255] {
        println!(
            "  i={i:3}  R={:5} G={:5} B={:5}{}",
            ramp[i],
            ramp[256 + i],
            ramp[512 + i],
            if ramp[i] == (i * 257) as u16
                && ramp[256 + i] == (i * 257) as u16
                && ramp[512 + i] == (i * 257) as u16
            {
                "  (线性)"
            } else {
                ""
            }
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let set_k: Option<u32> = args
        .iter()
        .position(|a| a == "--set")
        .and_then(|p| args.get(p + 1))
        .and_then(|s| s.parse().ok());
    let hold: u64 = args
        .iter()
        .position(|a| a == "--hold")
        .and_then(|p| args.get(p + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    unsafe {
        // 枚举所有显示设备，逐个试 gamma LUT（GetDC(NULL) 只覆盖主显示器，
        // 且桌面 DC 上 GetDeviceGammaRamp 经常直接失败）。
        let mut saved: Vec<([u16; 32], [u16; 768])> = Vec::new(); // (设备名, 原 LUT)
        let mut i = 0u32;
        loop {
            let mut dev = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            if !EnumDisplayDevicesW(PCWSTR::null(), i, &mut dev, 0).as_bool() {
                break;
            }
            i += 1;
            if dev.StateFlags & DISPLAY_DEVICE_ACTIVE == DISPLAY_DEVICE_STATE_FLAGS(0) {
                continue;
            }
            let name_len = dev
                .DeviceName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(dev.DeviceName.len());
            let name = String::from_utf16_lossy(&dev.DeviceName[..name_len]);
            let hdc = CreateDCW(
                PCWSTR::null(),
                PCWSTR(dev.DeviceName.as_ptr()),
                PCWSTR::null(),
                None,
            );
            if hdc.is_invalid() {
                println!("{name}: CreateDC failed");
                continue;
            }
            let mut orig = [0u16; 768];
            if !GetDeviceGammaRamp(hdc, orig.as_mut_ptr().cast()).as_bool() {
                println!("{name}: GetDeviceGammaRamp failed（该显示器不支持 LUT）");
                let _ = DeleteDC(hdc);
                continue;
            }
            print_samples(&format!("{name} 当前 LUT"), &orig);
            let mut devname = [0u16; 32];
            devname[..name_len].copy_from_slice(&dev.DeviceName[..name_len]);
            saved.push((devname, orig));
            let _ = DeleteDC(hdc);
        }

        if let Some(k) = set_k {
            let warm = ramp_for_kelvin(k);
            for (name, _) in &saved {
                let hdc = CreateDCW(PCWSTR::null(), PCWSTR(name.as_ptr()), PCWSTR::null(), None);
                if hdc.is_invalid() {
                    continue;
                }
                let ok = SetDeviceGammaRamp(hdc, warm.as_ptr().cast()).as_bool();
                println!("{}: 刷入 {k}K -> {ok}", String::from_utf16_lossy(name));
                let _ = DeleteDC(hdc);
            }
            println!("保持 {hold}s 后恢复原 LUT……");
            std::thread::sleep(std::time::Duration::from_secs(hold));
            for (name, orig) in &saved {
                let hdc = CreateDCW(PCWSTR::null(), PCWSTR(name.as_ptr()), PCWSTR::null(), None);
                if hdc.is_invalid() {
                    continue;
                }
                let ok = SetDeviceGammaRamp(hdc, orig.as_ptr().cast()).as_bool();
                println!("{}: 恢复原 LUT -> {ok}", String::from_utf16_lossy(name));
                let _ = DeleteDC(hdc);
            }
        }
    }
}
