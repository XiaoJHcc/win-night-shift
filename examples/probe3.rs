//! FromIdAsync 完整调用（修正版）：IAsyncInfo 读状态、BD9CB39A 接口
//! put_Completed(槽位6)/GetResults(槽位8)。用法：cargo run --example probe3

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::{s, w, GUID, HSTRING, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};

const STATICS_IID: GUID = GUID::from_values(
    0x22771028,
    0x1658,
    0x4E5E,
    [0xAB, 0x77, 0x30, 0x36, 0x91, 0xA8, 0x6F, 0xDA],
);
/// op 上承载 put_Completed/get_Completed/GetResults 的接口（虚表确认）。
const RESULTS_IID: GUID = GUID::from_values(
    0xBD9CB39A,
    0x6F09,
    0x5209,
    [0xA6, 0x56, 0x13, 0x06, 0xF0, 0xDD, 0xF5, 0xC9],
);
const ASYNC_INFO_IID: GUID = GUID::from_values(0x36, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
/// IAsyncOperationCompletedHandler<DEM>（注册表实名确认）。
const HANDLER_IID: GUID = GUID::from_values(
    0x00BD3501,
    0xAFC9,
    0x4000,
    [0x9C, 0x36, 0x6E, 0x14, 0x07, 0x9D, 0x79, 0xA4],
);
const IID_IUNKNOWN: GUID = GUID::from_values(0, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);

static COMPLETED: AtomicBool = AtomicBool::new(false);
static COMPLETED_STATUS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

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
        let hr = f(obj, iid, &mut out);
        if hr != 0 {
            println!("  QI {iid:?} -> {hr:#010x}");
            return std::ptr::null_mut();
        }
        out
    }
}

unsafe fn get_iids(obj: *mut c_void) -> Vec<GUID> {
    unsafe {
        type GetIidsFn = unsafe extern "system" fn(*mut c_void, *mut u32, *mut *mut GUID) -> i32;
        let f: GetIidsFn = std::mem::transmute(vtbl(obj, 3));
        let mut count = 0u32;
        let mut arr: *mut GUID = std::ptr::null_mut();
        if f(obj, &mut count, &mut arr) != 0 || arr.is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(arr, count as usize).to_vec()
    }
}

unsafe fn get_class_name(obj: *mut c_void) -> String {
    unsafe {
        type GetNameFn = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
        let f: GetNameFn = std::mem::transmute(vtbl(obj, 4));
        let mut h: *mut c_void = std::ptr::null_mut();
        if f(obj, &mut h) != 0 {
            return String::from("<err>");
        }
        let hs: HSTRING = std::mem::transmute(h);
        let mut len = 0u32;
        let ptr = windows::Win32::System::WinRT::WindowsGetStringRawBuffer(&hs, Some(&mut len));
        if ptr.0.is_null() {
            return String::from("<empty>");
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr.0, len as usize))
    }
}

unsafe extern "system" fn h_qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> i32 {
    unsafe {
        let iid = &*iid;
        if *iid == HANDLER_IID || *iid == IID_IUNKNOWN {
            *out = this;
            return 0;
        }
        *out = std::ptr::null_mut();
        -2147467262
    }
}
unsafe extern "system" fn h_addref(_this: *mut c_void) -> u32 {
    2
}
unsafe extern "system" fn h_release(_this: *mut c_void) -> u32 {
    1
}
unsafe extern "system" fn h_invoke(_this: *mut c_void, _op: *mut c_void, status: i32) -> i32 {
    println!("  >>> 完成回调! status={status}");
    COMPLETED_STATUS.store(status, Ordering::SeqCst);
    COMPLETED.store(true, Ordering::SeqCst);
    0
}
struct Vtbl([*mut c_void; 4]);
unsafe impl Sync for Vtbl {}
static HANDLER_VTBL: Vtbl = Vtbl([
    h_qi as *mut c_void,
    h_addref as *mut c_void,
    h_release as *mut c_void,
    h_invoke as *mut c_void,
]);

fn load_symbols() -> std::collections::HashMap<u64, String> {
    let mut map = std::collections::HashMap::new();
    let text = std::fs::read_to_string("target/dem-publics.txt").unwrap_or_default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("PublicSymbol: [") {
            if let Some(end) = rest.find(']') {
                if let Ok(rva) = u64::from_str_radix(&rest[..end], 16) {
                    map.entry(rva).or_insert_with(|| rest[end + 1..].trim().to_string());
                }
            }
        }
    }
    map
}

fn main() {
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
            return;
        }

        for id in [
            String::from(r"\\.\DISPLAY1"),
            String::from(
                r"\\?\DISPLAY#ICD753C#5&162ffb0&0&UID4353#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}",
            ),
        ] {
            println!("尝试 ID: {id}");
            type FromIdFn =
                unsafe extern "system" fn(*mut c_void, HSTRING, *mut *mut c_void) -> i32;
            let from_id: FromIdFn = std::mem::transmute(vtbl(statics, 6));
            let mut op_raw: *mut c_void = std::ptr::null_mut();
            let hr = from_id(statics, HSTRING::from(&id), &mut op_raw);
            if hr != 0 || op_raw.is_null() {
                println!("  FromIdAsync -> {hr:#010x}");
                continue;
            }
            let op_info = qi(op_raw, &ASYNC_INFO_IID);
            let op_res = qi(op_raw, &RESULTS_IID);
            if op_res.is_null() {
                continue;
            }

            // 注册完成回调（RESULTS_IID 槽位 6 = put_Completed）。
            let handler_box: Box<&[*mut c_void; 4]> = Box::new(&HANDLER_VTBL.0);
            let handler = Box::into_raw(handler_box) as *mut c_void;
            type PutCompletedFn = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;
            let put_completed: PutCompletedFn = std::mem::transmute(vtbl(op_res, 6));
            let phr = put_completed(op_res, handler);
            println!("  put_Completed -> {phr:#010x}");

            for _ in 0..100 {
                if COMPLETED.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            println!(
                "  完成 = {} status = {}",
                COMPLETED.load(Ordering::SeqCst),
                COMPLETED_STATUS.load(Ordering::SeqCst)
            );
            if !op_info.is_null() {
                type GetStatusFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
                let get_status: GetStatusFn = std::mem::transmute(vtbl(op_info, 7));
                let mut st = -1i32;
                let _ = get_status(op_info, &mut st);
                type GetErrFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
                let get_err: GetErrFn = std::mem::transmute(vtbl(op_info, 8));
                let mut ec = 0i32;
                let _ = get_err(op_info, &mut ec);
                println!("  IAsyncInfo: status={st} errorCode={ec:#010x}");
            }

            // GetResults（RESULTS_IID 槽位 8）。
            type GetResultsFn = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
            let get_results: GetResultsFn = std::mem::transmute(vtbl(op_res, 8));
            let mut inst: *mut c_void = std::ptr::null_mut();
            let hr = get_results(op_res, &mut inst);
            println!("  GetResults -> hr={hr:#010x} inst={inst:p}");
            if hr == 0 && !inst.is_null() {
                println!("  RuntimeClassName: {}", get_class_name(inst));
                for iid in get_iids(inst) {
                    println!("    {iid:?}");
                }
                // 实例虚表逐槽位解析：in-proc 对象 → DEM 模块符号；
                // 跨进程代理 → 落在 combase 等模块。
                let dem_inst = qi(inst, &GUID::from_values(
                    0xCDA29A3E, 0x9E7E, 0x4B86, [0x8F, 0x5F, 0x36, 0x8A, 0xA0, 0x71, 0x00, 0x08],
                ));
                let target = if dem_inst.is_null() { inst } else { dem_inst };
                let vt = *(target as *const u64);
                let mut hm2 = HMODULE::default();
                let mut base2 = 0u64;
                let mut modname = String::from("?");
                if GetModuleHandleExW(
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                    PCWSTR(vt as usize as *const u16),
                    &mut hm2,
                )
                .is_ok()
                {
                    base2 = hm2.0 as u64;
                    let mut buf = [0u16; 260];
                    let n = windows::Win32::System::ProcessStatus::GetModuleFileNameExW(
                        Some(windows::Win32::System::Threading::GetCurrentProcess()),
                        Some(hm2),
                        &mut buf,
                    );
                    if n > 0 {
                        modname = String::from_utf16_lossy(&buf[..n as usize]);
                    }
                }
                println!("  实例虚表在 {modname}，rva={:#x}", vt - base2);
                let syms = load_symbols();
                for slot in 0..36u64 {
                    let f = *(vt as *const u64).add(slot as usize);
                    if base2 == 0 || f < base2 {
                        break;
                    }
                    let rva = f - base2;
                    if rva > 0x1000000 {
                        break;
                    }
                    let name = syms.get(&rva).cloned().unwrap_or_else(|| "?".into());
                    let short: String = name.chars().take(110).collect();
                    println!("    slot {slot:2} rva={rva:06x}  {short}");
                }
                break;
            }
        }
    }
}
