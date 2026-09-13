//! StartNightLightTransition 语义验证（反汇编结论，安全版）。
//!
//! 结论来源（blr/des 反汇编）：
//!   arg1 float  = 目标色温（开尔文绝对值，6500 = 中性）；
//!   arg2 double = 过渡时长（毫秒，引擎侧由 int64 ms 转 double）；
//!   arg3 enum   = ColorTransitionType：0=Instant(0ms) / 1=Fast(2000ms) / 2=Gradual(120000ms)。
//! 系统自己的拉条拖动路径（SetTargetTemperatureOnMonitorImmediate → SetTemperatureDem）
//! 就是 (kelvin, 0.0, 0)。效果是驱动级、粘性（保持到下一个 target）。
//!
//! 验证序列（每步 3 秒）：基线(restore,0,0) → (2700,0,0) → (1500,2000,1) → 恢复(restore,0,0)。
//! restore = 夜灯开 ? 注册表当前色温 : 6500。效果粘性，最后一步必须执行到位。
//! 用法：cargo run --example probe5
#![allow(dead_code)]

#[path = "../src/reg.rs"]
mod reg;
#[path = "../src/nightlight.rs"]
mod nightlight;

use std::ffi::c_void;
use windows::core::{s, w, GUID, HSTRING, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

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

fn wstr(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// 活动显示器的设备接口路径列表（EnumDisplayDevices flag=1）。
fn monitor_ids() -> Vec<String> {
    let mut ids = Vec::new();
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
            let mut mon = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            if EnumDisplayDevicesW(PCWSTR(dev.DeviceName.as_ptr()), 0, &mut mon, 1).as_bool() {
                ids.push(wstr(&mon.DeviceID));
            }
        }
    }
    ids
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
    let enabled = nightlight::get_enabled().unwrap_or(false);
    let restore_kelvin = if enabled {
        nightlight::strength_to_kelvin(nightlight::get_strength().unwrap_or(50))
    } else {
        6500
    };
    println!("夜灯开关 = {enabled}，恢复目标 = {restore_kelvin}K");

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

        let mut insts = Vec::new();
        for id in monitor_ids() {
            let inst = open_instance(statics, &id);
            println!("{id}: {}", if inst.is_null() { "open failed" } else { "ok" });
            if !inst.is_null() {
                insts.push(inst);
            }
        }
        if insts.is_empty() {
            println!("无可用 DEM 实例，退出");
            return;
        }

        let apply = |kelvin: f32, duration_ms: f64, ty: u32| {
            for &inst in &insts {
                type StartFn = unsafe extern "system" fn(*mut c_void, f32, f64, u32) -> i32;
                let start: StartFn = std::mem::transmute(vtbl(inst, 8));
                let hr = start(inst, kelvin, duration_ms, ty);
                println!("  start({kelvin}K, {duration_ms}ms, {ty}) -> {hr:#010x}");
            }
        };
        let pause = |secs: u64| {
            std::thread::sleep(std::time::Duration::from_secs(secs));
        };

        println!("[1/4] 基线：start(restore, 0, Instant)，应无可见变化");
        apply(restore_kelvin as f32, 0.0, 0);
        pause(3);
        println!("[2/4] start(2700K, 0, Instant)，屏幕应立即变暖");
        apply(2700.0, 0.0, 0);
        pause(3);
        println!("[3/4] start(1500K, 2000ms, Fast)，应 2 秒渐变到更暖");
        apply(1500.0, 2000.0, 1);
        pause(3);
        println!("[4/4] 恢复：start(restore, 0, Instant)，应回到步骤 1 状态");
        apply(restore_kelvin as f32, 0.0, 0);
        println!("完成。屏幕应已恢复 {restore_kelvin}K。");
    }
}
