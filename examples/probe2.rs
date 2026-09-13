//! 探测 DisplayEnhancementManagement.FromIdAsync：用各种候选显示器 ID 调静态
//! 方法，拿到实例后打印 RuntimeClassName 与接口 IID。
//! 用法：cargo run --example probe2

use std::ffi::c_void;
use windows::core::{s, w, GUID, HSTRING, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

const STATICS_IID: GUID = GUID::from_values(
    0x22771028,
    0x1658,
    0x4E5E,
    [0xAB, 0x77, 0x30, 0x36, 0x91, 0xA8, 0x6F, 0xDA],
);

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
            println!("  QI {iid:?} -> {hr:#x}");
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

/// 候选显示器 ID：GDI 名（\\.\DISPLAY1）与设备接口路径。
fn candidate_ids() -> Vec<String> {
    let mut ids = Vec::new();
    unsafe {
        for i in 0..8u32 {
            let mut dev = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            if !EnumDisplayDevicesW(PCWSTR::null(), i, &mut dev, 0).as_bool() {
                break;
            }
            if dev.StateFlags & windows::Win32::Graphics::Gdi::DISPLAY_DEVICE_ACTIVE
                == windows::Win32::Graphics::Gdi::DISPLAY_DEVICE_STATE_FLAGS(0)
            {
                continue;
            }
            let name = wstr(&dev.DeviceName);
            ids.push(name);
            // EDD_GET_DEVICE_INTERFACE_NAME (flag=1)：DeviceID 为设备接口路径。
            let mut dev2 = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let name_w: Vec<u16> = dev.DeviceName.iter().cloned().take_while(|&c| c != 0).collect();
            if EnumDisplayDevicesW(PCWSTR(dev.DeviceName.as_ptr()), 0, &mut dev2, 1).as_bool() {
                let _ = name_w;
                ids.push(wstr(&dev2.DeviceID));
            }
        }
    }
    ids
}

fn wstr(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

static COMPLETED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
        let hr = get_af(
            HSTRING::from(
                "Windows.Internal.Graphics.Display.DisplayEnhancementManagement.DisplayEnhancementManagement",
            ),
            &mut factory,
        );
        if hr != 0 {
            println!("get factory -> {hr:#x}");
            return;
        }
        let statics = qi(factory, &STATICS_IID);
        if statics.is_null() {
            println!("no statics interface");
            return;
        }
        println!("statics = {statics:p}");

        for id in candidate_ids() {
            println!("尝试 ID: {id}");
            type FromIdFn =
                unsafe extern "system" fn(*mut c_void, HSTRING, *mut *mut c_void) -> i32;
            let from_id: FromIdFn = std::mem::transmute(vtbl(statics, 6));
            let mut op: *mut c_void = std::ptr::null_mut();
            let hr = from_id(statics, HSTRING::from(&id), &mut op);
            println!("  FromIdAsync -> hr={hr:#x} op={op:p}");
            if hr != 0 || op.is_null() {
                continue;
            }
            // 拿到的 op 指针是默认接口，不是 IAsyncOperation——先 QI 出
            // pinterface IAsyncOperation<DisplayEnhancementManagement>。
            let op_a = qi(op, &GUID::from_values(0xBD9CB39A, 0x6F09, 0x5209, [0xA6, 0x56, 0x13, 0x06, 0xF0, 0xDD, 0xF5, 0xC9]));
            let op_b = qi(op, &GUID::from_values(0x7A900AF8, 0xB975, 0x45F7, [0x8C, 0x93, 0x3A, 0xE1, 0x7D, 0xF5, 0xC5, 0xD0]));
            println!("  QI A(BD9CB39A)={op_a:p} QI B(7A900AF8)={op_b:p}");
            let op = if !op_b.is_null() { op_b } else { op_a };
            if op.is_null() {
                continue;
            }
            // 不轮询（防止轮询本身阻塞 RPC 重入）：先等 3s，再读一次状态，
            // 然后把 GetResults 放到看门狗线程里调用，防挂死。
            std::thread::sleep(std::time::Duration::from_secs(3));
            type GetStatusFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
            let get_status: GetStatusFn = std::mem::transmute(vtbl(op, 7));
            let mut status = -1i32;
            let ghr = get_status(op, &mut status);
            type GetErrFn = unsafe extern "system" fn(*mut c_void, *mut i32) -> i32;
            let get_err: GetErrFn = std::mem::transmute(vtbl(op, 8));
            let mut errc = 0i32;
            let _ = get_err(op, &mut errc);
            println!("  3s 后: get_Status hr={ghr:#x} status={status}（1=完成 3=错误）ErrorCode={errc:#x}");

            // 有些 AsyncOperation 实现是惰性启动：注册完成回调才真正开始跑。
            // 手工实现 IAsyncOperationCompletedHandler<T>（IID 来自注册表）。
            static HANDLER_IID: GUID = GUID::from_values(
                0x00BD3501,
                0xAFC9,
                0x4000,
                [0x9C, 0x36, 0x6E, 0x14, 0x07, 0x9D, 0x79, 0xA4],
            );
            const IID_IUNKNOWN: GUID =
                GUID::from_values(0, 0, 0, [0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
            unsafe extern "system" fn h_qi(
                this: *mut c_void,
                iid: *const GUID,
                out: *mut *mut c_void,
            ) -> i32 {
                unsafe {
                    let iid = &*iid;
                    if *iid == HANDLER_IID || *iid == IID_IUNKNOWN {
                        *out = this;
                        return 0;
                    }
                    *out = std::ptr::null_mut();
                    -2147467262 // E_NOINTERFACE
                }
            }
            unsafe extern "system" fn h_addref(_this: *mut c_void) -> u32 {
                2
            }
            unsafe extern "system" fn h_release(_this: *mut c_void) -> u32 {
                1
            }
            unsafe extern "system" fn h_invoke(
                _this: *mut c_void,
                _op: *mut c_void,
                status: i32,
            ) -> i32 {
                println!("  >>> 完成回调触发! status={status}");
                COMPLETED.store(true, std::sync::atomic::Ordering::SeqCst);
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
            let handler_box: Box<&[*mut c_void; 4]> = Box::new(&HANDLER_VTBL.0);
            let handler = Box::into_raw(handler_box) as *mut c_void;
            type PutCompletedFn = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;
            let put_completed: PutCompletedFn = std::mem::transmute(vtbl(op, 11));
            let phr = put_completed(op, handler);
            println!("  put_Completed -> {phr:#x}");
            for _ in 0..50 {
                if COMPLETED.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let mut status2 = -1i32;
            let _ = get_status(op, &mut status2);
            println!("  回调等待后 status={status2}");

            let op_addr = op as usize;
            let (tx, rx) = std::sync::mpsc::channel::<(i32, usize)>();
            std::thread::spawn(move || {
                let op = op_addr as *mut c_void;
                unsafe {
                    type GetResultsFn =
                        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
                    let get_results: GetResultsFn = std::mem::transmute(vtbl(op, 13));
                    let mut inst: *mut c_void = std::ptr::null_mut();
                    let hr = get_results(op, &mut inst);
                    let _ = tx.send((hr, inst as usize));
                }
            });
            match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok((hr, inst)) => {
                    println!("  GetResults -> hr={hr:#x} inst={inst:#x}");
                    if hr == 0 && inst != 0 {
                        let inst = inst as *mut c_void;
                        println!("  RuntimeClassName: {}", get_class_name(inst));
                        for iid in get_iids(inst) {
                            println!("    {iid:?}");
                        }
                    }
                }
                Err(_) => println!("  GetResults 挂死（5s 看门狗超时）"),
            }
            continue;
        }
    }
}
