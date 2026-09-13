//! 探测 Windows.Shell.BlueLightReduction.dll 的 WinRT 类：
//! 绕过 WinRT 注册直接 DllGetActivationFactory，打印工厂/实例实现的接口 IID。
//! 用法：cargo run --example probe

use std::ffi::c_void;
use windows::core::{s, w, GUID, HSTRING, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

// IInspectable vtable 槽位（COM 三件套之后）。
const SLOT_GET_IIDS: usize = 3;
const SLOT_GET_CLASS_NAME: usize = 4;

type DllGetActivationFactoryFn =
    unsafe extern "system" fn(class_id: HSTRING, factory: *mut *mut c_void) -> i32;

unsafe fn vtbl(obj: *mut c_void, slot: usize) -> *mut c_void {
    unsafe {
        let vt = *(obj as *const *const *mut c_void);
        *vt.add(slot)
    }
}

unsafe fn get_iids(obj: *mut c_void) -> Vec<GUID> {
    unsafe {
        type GetIidsFn =
            unsafe extern "system" fn(*mut c_void, *mut u32, *mut *mut GUID) -> i32;
        let f: GetIidsFn = std::mem::transmute(vtbl(obj, SLOT_GET_IIDS));
        let mut count = 0u32;
        let mut arr: *mut GUID = std::ptr::null_mut();
        let hr = f(obj, &mut count, &mut arr);
        if hr != 0 || arr.is_null() {
            println!("  GetIids failed: hr={hr:#x}");
            return Vec::new();
        }
        let v = std::slice::from_raw_parts(arr, count as usize).to_vec();
        // 探针进程退出即回收，不调 CoTaskMemFree。
        v
    }
}

unsafe fn get_class_name(obj: *mut c_void) -> String {
    unsafe {
        type GetNameFn = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
        let f: GetNameFn = std::mem::transmute(vtbl(obj, SLOT_GET_CLASS_NAME));
        let mut h: *mut c_void = std::ptr::null_mut();
        let hr = f(obj, &mut h);
        if hr != 0 {
            return format!("<GetRuntimeClassName failed: {hr:#x}>");
        }
        // HSTRING ABI 即 *mut c_void，包一层交给 WindowsGetStringRawBuffer 解码。
        let hs: HSTRING = std::mem::transmute(h);
        let mut len = 0u32;
        let ptr = unsafe { windows::Win32::System::WinRT::WindowsGetStringRawBuffer(&hs, Some(&mut len)) };
        if ptr.0.is_null() {
            return String::from("<empty>");
        }
        let s = unsafe { std::slice::from_raw_parts(ptr.0, len as usize) };
        String::from_utf16_lossy(s)
    }
}

fn main() {
    unsafe {
        eprintln!("[1] loadlibrary");
        // 不带 WinRT 单元直接调激活工厂会崩（WRL 模块内部依赖已初始化的单元）。
        let _ = windows::Win32::System::WinRT::RoInitialize(
            windows::Win32::System::WinRT::RO_INIT_SINGLETHREADED,
        );
        let targets = [
            ("Windows.Internal.Graphics.Display.DisplayEnhancementManagement.dll",
             "Windows.Internal.Graphics.Display.DisplayEnhancementManagement.DisplayEnhancementManagement"),
            ("Windows.Shell.BlueLightReduction.dll",
             "Windows.Internal.Shell.BlueLightReduction.BlueLightReductionManager"),
        ];
        for (dll, class) in targets {
            let dll_w: Vec<u16> = dll.encode_utf16().chain(std::iter::once(0)).collect();
            let h: HMODULE = match LoadLibraryExW(
                PCWSTR::from_raw(dll_w.as_ptr()),
                None,
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            ) {
                Ok(h) => h,
                Err(e) => {
                    println!("{dll}: LoadLibrary failed: {e}");
                    continue;
                }
            };
            let Some(get_af) = GetProcAddress(h, s!("DllGetActivationFactory")) else {
                println!("{dll}: no DllGetActivationFactory");
                continue;
            };
            let get_af: DllGetActivationFactoryFn = std::mem::transmute(get_af);

            eprintln!("[3] DllGetActivationFactory({class})");
            let hs = HSTRING::from(class);
            let mut factory: *mut c_void = std::ptr::null_mut();
            let hr = get_af(hs, &mut factory);
            eprintln!("[4] hr={hr:#x} factory={factory:p}");
            if hr != 0 {
                println!("{class}: DllGetActivationFactory -> {hr:#x}");
                continue;
            }
            println!("{class}: factory = {factory:p}");
            println!("  工厂接口（静态方法接口）:");
            for iid in get_iids(factory) {
                println!("    {iid:?}");
            }

            // IActivationFactory::ActivateInstance（槽位 6）。
            type ActivateFn = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
            let activate: ActivateFn = std::mem::transmute(vtbl(factory, 6));
            let mut inst: *mut c_void = std::ptr::null_mut();
            let hr = activate(factory, &mut inst);
            if hr != 0 {
                println!("  ActivateInstance -> {hr:#x}（可能需要 FromIdAsync 之类的静态构造）");
                continue;
            }
            println!("  实例 = {inst:p}");
            println!("  RuntimeClassName: {}", get_class_name(inst));
            println!("  实例接口:");
            for iid in get_iids(inst) {
                println!("    {iid:?}");
            }
        }
    }
}
