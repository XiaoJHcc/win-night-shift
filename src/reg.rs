//! HKCU 注册表读写的最小封装（二进制与 DWORD）。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::{PWSTR, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE,
    KEY_SET_VALUE, REG_BINARY, REG_DWORD, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
};

pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn open(subkey: &str, sam: REG_SAM_FLAGS) -> Option<HKEY> {
    let sub = wide(subkey);
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, sam, &mut hkey)
    };
    (rc == ERROR_SUCCESS).then_some(hkey)
}

/// 打开（不存在则创建）子键，用于首次写入。
fn create(subkey: &str) -> Option<HKEY> {
    let sub = wide(subkey);
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            Default::default(),
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
    };
    (rc == ERROR_SUCCESS).then_some(hkey)
}

/// 读 REG_BINARY；键/值不存在或类型不符返回 None。
pub fn read_binary(subkey: &str, value: &str) -> Option<Vec<u8>> {
    let hkey = open(subkey, KEY_QUERY_VALUE)?;
    let name = wide(value);
    let mut ty = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut size),
        )
    };
    if rc != ERROR_SUCCESS || ty != REG_BINARY || size == 0 {
        unsafe { let _ = RegCloseKey(hkey); }
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        )
    };
    unsafe { let _ = RegCloseKey(hkey); }
    if rc == ERROR_SUCCESS {
        buf.truncate(size as usize);
        Some(buf)
    } else {
        None
    }
}

/// 写 REG_BINARY；键不存在时创建。
pub fn write_binary(subkey: &str, value: &str, data: &[u8]) -> bool {
    let hkey = match open(subkey, KEY_SET_VALUE).or_else(|| create(subkey)) {
        Some(h) => h,
        None => return false,
    };
    let name = wide(value);
    let rc = unsafe { RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, REG_BINARY, Some(data)) };
    unsafe { let _ = RegCloseKey(hkey); }
    rc == ERROR_SUCCESS
}

/// 读 REG_DWORD；缺失返回 None。
pub fn read_dword(subkey: &str, value: &str) -> Option<u32> {
    let hkey = open(subkey, KEY_QUERY_VALUE)?;
    let name = wide(value);
    let mut ty = REG_VALUE_TYPE::default();
    let mut data = 0u32;
    let mut size = 4u32;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut size),
        )
    };
    unsafe { let _ = RegCloseKey(hkey); }
    (rc == ERROR_SUCCESS && ty == REG_DWORD).then_some(data)
}

/// 写 REG_DWORD。
pub fn write_dword(subkey: &str, value: &str, data: u32) -> bool {
    let hkey = match open(subkey, KEY_SET_VALUE).or_else(|| create(subkey)) {
        Some(h) => h,
        None => return false,
    };
    let name = wide(value);
    let bytes = data.to_le_bytes();
    let rc = unsafe { RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&bytes)) };
    unsafe { let _ = RegCloseKey(hkey); }
    rc == ERROR_SUCCESS
}

/// 读 REG_SZ 是否存在（自启项探测用）。
pub fn value_exists(subkey: &str, value: &str) -> bool {
    let Some(hkey) = open(subkey, KEY_QUERY_VALUE) else {
        return false;
    };
    let name = wide(value);
    let rc = unsafe {
        RegQueryValueExW(hkey, PCWSTR(name.as_ptr()), None, None, None, None)
    };
    unsafe { let _ = RegCloseKey(hkey); }
    rc == ERROR_SUCCESS
}

/// 写/删 REG_SZ（自启项用）。
pub fn write_string(subkey: &str, value: &str, data: &str) -> bool {
    let Some(hkey) = open(subkey, KEY_SET_VALUE).or_else(|| create(subkey)) else {
        return false;
    };
    let name = wide(value);
    let content = wide(data);
    let bytes =
        unsafe { std::slice::from_raw_parts(content.as_ptr() as *const u8, content.len() * 2) };
    let rc = unsafe { RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes)) };
    unsafe { let _ = RegCloseKey(hkey); }
    rc == ERROR_SUCCESS
}

/// 删除值；值本就不存在也算成功。
pub fn delete_value(subkey: &str, value: &str) -> bool {
    let Some(hkey) = open(subkey, KEY_SET_VALUE) else {
        return true;
    };
    let name = wide(value);
    let _rc = unsafe { RegDeleteValueW(hkey, PCWSTR(name.as_ptr())) };
    unsafe { let _ = RegCloseKey(hkey); }
    true
}

/// 枚举子键名（CloudStore 按设备/账户分键，需枚举）。
pub fn enum_subkeys(subkey: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(hkey) = open(subkey, KEY_ENUMERATE_SUB_KEYS) else {
        return out;
    };
    let mut index = 0u32;
    loop {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let rc = unsafe {
            RegEnumKeyExW(
                hkey,
                index,
                Some(PWSTR(buf.as_mut_ptr())),
                &mut len,
                None,
                Some(PWSTR::null()),
                None,
                None,
            )
        };
        if rc != ERROR_SUCCESS {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
        index += 1;
    }
    unsafe { let _ = RegCloseKey(hkey); }
    out
}
