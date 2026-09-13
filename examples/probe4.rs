//! 白点读写实验：get_CurrentWhitePoint 读当前白点 → put_CurrentWhitePoint
//! 刷入暖白点保持 6 秒 → 恢复原白点。验证这条路径是否即时生效（系统同色温
//! 管线，截屏干净）。
//! 用法：cargo run --example probe4 -- <warm|restore>

use std::ffi::c_void;
use windows::core::{s, w, GUID, HSTRING};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

const STATICS_IID: GUID = GUID::from_values(
    0x22771028, 0x1658, 0x4E5E, [0xAB, 0x77, 0x30, 0x36, 0x91, 0xA8, 0x6F, 0xDA],
);
const RESULTS_IID: GUID = GUID::from_values(
    0xBD9CB39A, 0x6F09, 0x5209, [0xA6, 0x56, 0x13, 0x06, 0xF0, 0xDD, 0xF5, 0xC9],
);
const DEM_IID: GUID = GUID::from_values(
    0xCDA29A3E, 0x9E7E, 0x4B86, [0x8F, 0x5F, 0x36, 0x8A, 0xA0, 0x71, 0x00, 0x08],
);
const ASYNC_INFO_IID: GUID = GUID::from_values(0x36, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);

/// 显示设备接口路径（EnumDisplayDevices flag=1 得到，本机两块）。
const MONITORS: [&str; 2] = [
    r"\\?\DISPLAY#ICD753C#5&162ffb0&0&UID4353#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}",
    r"\\?\DISPLAY#KUY2825#4&1ff378d5&0&UID4162#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}",
];

#[derive(Clone, Copy, Debug)]
struct Xy {
    x: f32,
    y: f32,
}

/// Krystek 普朗克轨迹近似：色温 K → CIE xy。
fn kelvin_to_xy(t: f32) -> Xy {
    let t3 = t * t * t;
    let t2 = t * t;
    let x = if t < 4000.0 {
        -0.2661239e9 / t3 - 0.2343589e6 / t2 + 0.8776956e3 / t + 0.179910
    } else {
        -3.0258469e9 / t3 + 2.1070379e6 / t2 + 0.2226347e3 / t + 0.240390
    };
    let x3 = x * x * x;
    let x2 = x * x;
    let y = if t < 2222.0 {
        -1.1063814 * x3 - 1.34811020 * x2 + 2.18555832 * x - 0.20219683
    } else if t < 4000.0 {
        -0.9549476 * x3 - 1.37418593 * x2 + 2.09137015 * x - 0.16748867
    } else {
        3.0817580 * x3 - 5.87338670 * x2 + 3.75112997 * x - 0.37001483
    };
    Xy { x, y }
}

fn pack(xy: Xy) -> u64 {
    (xy.x.to_bits() as u64) | ((xy.y.to_bits() as u64) << 32)
}

unsafe fn vtbl(obj: *mut c_void, slot: usize) -> *mut c_void {
    unsafe {
        let vt = *(obj as *const *const *mut c_void);
        *vt.add(slot)
    }
}

unsafe fn qi(obj: *mut c_void, iid: &GUID) -> *mut c_void {
    unsafe {
        type QiFn = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32;
        let f: QiFn = std::mem::transmute(vtbl(obj, 0));
        let mut out: *mut c_void = std::ptr::null_mut();
        let _ = f(obj, iid, &mut out);
        out
    }
}

unsafe fn open_instance(statics: *mut c_void, id: &str) -> *mut c_void {
    unsafe {
        type FromIdFn = unsafe extern "system" fn(*mut c_void, HSTRING, *mut *mut c_void) -> i32;
        let from_id: FromIdFn = std::mem::transmute(vtbl(statics, 6));
        let mut op_raw: *mut c_void = std::ptr::null_mut();
        if from_id(statics, HSTRING::from(id), &mut op_raw) != 0 || op_raw.is_null() {
            return std::ptr::null_mut();
        }
        let op_info = qi(op_raw, &ASYNC_INFO_IID);
        let op_res = qi(op_raw, &RESULTS_IID);
        if op_res.is_null() {
            return std::ptr::null_mut();
        }
        // 轮询完成。
        type GetStatusFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
        let get_status: GetStatusFn = std::mem::transmute(vtbl(op_info, 7));
        for _ in 0..100 {
            let mut st = 0i32;
            let _ = get_status(op_info, &mut st);
            if st != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        type GetResultsFn = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
        let get_results: GetResultsFn = std::mem::transmute(vtbl(op_res, 8));
        let mut inst: *mut c_void = std::ptr::null_mut();
        if get_results(op_res, &mut inst) != 0 || inst.is_null() {
            return std::ptr::null_mut();
        }
        qi(inst, &DEM_IID)
    }
}

fn main() {
    let warm = std::env::args().any(|a| a == "warm");
    unsafe {
        let _ = windows::Win32::System::WinRT::RoInitialize(
            windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
        );
        let h: HMODULE = LoadLibraryExW(
            w!("Windows.Internal.Graphics.Display.DisplayEnhancementManagement.dll"),
            None,
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
        .expect("load dem");
        let get_af: unsafe extern "system" fn(HSTRING, *mut *mut c_void) -> i32 =
            std::mem::transmute(GetProcAddress(h, s!("DllGetActivationFactory")).unwrap());
        let mut factory: *mut c_void = std::ptr::null_mut();
        let _ = get_af(
            HSTRING::from(
                "Windows.Internal.Graphics.Display.DisplayEnhancementManagement.DisplayEnhancementManagement",
            ),
            &mut factory,
        );
        let statics = qi(factory, &STATICS_IID);
        if statics.is_null() {
            println!("no statics");
            return;
        }

        for id in MONITORS {
            let inst = open_instance(statics, id);
            if inst.is_null() {
                println!("{id}: open failed");
                continue;
            }
            // get_IsNightLightCapable(6) / get_IsNightLightOverridden(7) /
            // get_EnableModernNightLight(11)。
            type GetBoolFn = unsafe extern "system" fn(*mut c_void, *mut u8) -> i32;
            for (slot, name) in [(6, "IsNightLightCapable"), (7, "IsNightLightOverridden"), (11, "EnableModernNightLight")] {
                let f: GetBoolFn = std::mem::transmute(vtbl(inst, slot));
                let mut v = 0u8;
                let hr = f(inst, &mut v);
                println!("{name}: hr={hr:#010x} v={v}");
            }
            // get_CurrentWhitePoint(20)：out ChromaticityXY（两个 float）。
            type GetXyFn = unsafe extern "system" fn(*mut c_void, *mut Xy) -> i32;
            let get_xy: GetXyFn = std::mem::transmute(vtbl(inst, 20));
            let mut orig = Xy { x: 0.0, y: 0.0 };
            let hr = get_xy(inst, &mut orig);
            println!("当前白点: hr={hr:#010x} x={:.4} y={:.4}", orig.x, orig.y);

            if warm {
                type PutXyFn = unsafe extern "system" fn(*mut c_void, u64) -> i32;
                let put_xy: PutXyFn = std::mem::transmute(vtbl(inst, 21));
                let hr = put_xy(inst, pack(orig));
                println!("恢复: put_CurrentWhitePoint(D65) -> {hr:#010x}");
                type StartTransFn = unsafe extern "system" fn(*mut c_void, f32, f64, u32) -> i32;
                let start: StartTransFn = std::mem::transmute(vtbl(inst, 8));
                let hr = start(inst, 0.0, 0.0, 1);
                println!("恢复: start(0,0,1) 提交 -> {hr:#010x}");
            }
        }
    }
}
