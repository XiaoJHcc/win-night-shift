//! 实验：MagSetFullscreenColorEffect（DWM 颜色矩阵，系统颜色滤镜同机制）
//! 与夜间模式是「叠加」关系时，用相对矩阵做实时预览：
//!   M = diag(TH(K_target) / TH(K_applied))
//! 期望：夜间模式（K_applied=5281K）基础上叠加相对矩阵后，屏幕看起来就是
//! K_target=2790K，且无闪烁、可连续调节。
//!
//! 用法：cargo run --example mag -- --rel 2790 5281 --hold 8
//!   叠加相对矩阵 8 秒后复位为恒等矩阵。期间屏幕应变深暖，观感接近
//!   之前开关循环应用出的 2790K。

// windows crate 未投影 MagSetFullscreenColorEffect，且工具链没带
// Magnification 导入库——运行时从 Magnification.dll 动态取址。
use windows::core::{s, w};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

#[repr(C)]
struct COLORTRANSFORMATION {
    transform: [f32; 25],
}

type MagInitializeFn = unsafe extern "system" fn() -> i32;
type MagUninitializeFn = unsafe extern "system" fn() -> i32;
type MagSetFullscreenColorEffectFn =
    unsafe extern "system" fn(pEffect: *const COLORTRANSFORMATION) -> i32;

struct Mag {
    _h: HMODULE,
    init: MagInitializeFn,
    set: MagSetFullscreenColorEffectFn,
    uninit: MagUninitializeFn,
}

impl Mag {
    unsafe fn load() -> Option<Mag> {
        unsafe {
            let h = match LoadLibraryExW(w!("Magnification.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) {
                Ok(h) => h,
                Err(e) => {
                    println!("LoadLibraryExW: {e}");
                    return None;
                }
            };
            Some(Mag {
                _h: h,
                init: std::mem::transmute(GetProcAddress(h, s!("MagInitialize"))?),
                set: std::mem::transmute(GetProcAddress(
                    h,
                    s!("MagSetFullscreenColorEffect"),
                )?),
                uninit: std::mem::transmute(GetProcAddress(h, s!("MagUninitialize"))?),
            })
        }
    }
}

/// Tanner Helland 色温 → RGB 增益。
fn kelvin_to_rgb(k: u32) -> (f32, f32, f32) {
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
    (r as f32, g as f32, b as f32)
}

fn diag_matrix(r: f32, g: f32, b: f32) -> COLORTRANSFORMATION {
    let mut m = [0f32; 25];
    m[0] = r;
    m[6] = g;
    m[12] = b;
    m[18] = 1.0;
    m[24] = 1.0;
    COLORTRANSFORMATION { transform: m }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rel = args.iter().position(|a| a == "--rel");
    let hold: u64 = args
        .iter()
        .position(|a| a == "--hold")
        .and_then(|p| args.get(p + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    unsafe {
        let Some(mag) = Mag::load() else {
            println!("Magnification.dll 加载失败");
            return;
        };
        if (mag.init)() == 0 {
            println!("MagInitialize failed");
            return;
        }
        if let Some(pos) = rel {
            let target: u32 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(2790);
            let applied: u32 = args.get(pos + 2).and_then(|s| s.parse().ok()).unwrap_or(5281);
            let (tr, tg, tb) = kelvin_to_rgb(target);
            let (ar, ag, ab) = kelvin_to_rgb(applied);
            let (r, g, b) = (tr / ar, tg / ag, tb / ab);
            println!("相对矩阵: R={r:.4} G={g:.4} B={b:.4}");
            if (mag.set)(&diag_matrix(r, g, b)) == 0 {
                println!("MagSetFullscreenColorEffect failed");
                (mag.uninit)();
                return;
            }
            if args.iter().any(|a| a == "--leak") {
                println!("已叠加，进程退出且【不复位】——观察矩阵是否残留");
                (mag.uninit)();
                return;
            }
            println!("已叠加，{hold}s 后复位……");
            std::thread::sleep(std::time::Duration::from_secs(hold));
            let ok = (mag.set)(&diag_matrix(1.0, 1.0, 1.0)) != 0;
            println!("复位恒等 -> {ok}");
        } else {
            // 无参数：复位恒等（清残留用）。
            let ok = (mag.set)(&diag_matrix(1.0, 1.0, 1.0)) != 0;
            println!("复位恒等 -> {ok}");
        }
        (mag.uninit)();
    }
}
