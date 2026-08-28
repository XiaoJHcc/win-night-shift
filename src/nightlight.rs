//! Windows 夜间模式（Night Light）控制。
//!
//! 没有公开 API，使用未文档化的 CloudStore 注册表 blob（社区逆向，Win10 2004+
//! 与 Win11 通用，写入后立即生效）：
//!
//! - 开关：`...\bluelightreductionstate` 键的 `Data`。
//!   `data[18] == 0x15` 为开、`0x13` 为关；翻转时把字节 10..14 中第一个非 0xFF
//!   的 +1（最后修改时间戳），并在索引 23 处插入/删除 `0x10 0x00` 两字节。
//! - 强度：CloudStore 下同一设置同时存在多把键——主设置
//!   `default$...bluelightreduction.settings`、每个显示设备的
//!   `{guid}$...settingsperdevice`（**实际生效的是它**）、旧版命名
//!   `default$...bluelightreduction.bluelightreduction.settings`。
//!   blob 中 `CF 28` 标记后的两个字节是色温（1200–6500K）：
//!   `lo = ((K & 0x3F) << 1) | 0x80`，`hi = K >> 6`。
//!   所有设置键一起原地改这两字节并推进内嵌时间戳，不动排程等字段。
//!
//! 所有操作对 blob 做长度/标记校验，格式不符时静默失败（返回 false/None）。

use crate::reg;

const CLOUDSTORE_CURRENT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current";
const STATE_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current\default$windows.data.bluelightreduction.bluelightreductionstate\windows.data.bluelightreduction.bluelightreductionstate";
/// 旧版命名的设置键，仅在枚举不到任何设置键时用于模板重建。
const SETTINGS_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current\default$windows.data.bluelightreduction.bluelightreduction.settings\windows.data.bluelightreduction.settings";

const MIN_KELVIN: u32 = 1200;
const MAX_KELVIN: u32 = 6500;

/// 强度 0–100 ↔ 色温线性映射（强度越大越暖）。
pub fn strength_to_kelvin(strength: u32) -> u32 {
    let strength = strength.min(100);
    MAX_KELVIN - strength * (MAX_KELVIN - MIN_KELVIN) / 100
}

fn kelvin_to_strength(kelvin: u32) -> u32 {
    let k = kelvin.clamp(MIN_KELVIN, MAX_KELVIN);
    (MAX_KELVIN - k) * 100 / (MAX_KELVIN - MIN_KELVIN)
}

fn encode_kelvin(k: u32) -> (u8, u8) {
    ((((k & 0x3F) << 1) | 0x80) as u8, (k >> 6) as u8)
}

fn decode_kelvin(lo: u8, hi: u8) -> u32 {
    ((lo >> 1) as u32 & 0x3F) | ((hi as u32) << 6)
}

/// 读取夜间模式开关状态；blob 缺失或格式不符返回 None。
pub fn get_enabled() -> Option<bool> {
    let data = reg::read_binary(STATE_KEY, "Data")?;
    match *data.get(18)? {
        0x15 => Some(true),
        0x13 => Some(false),
        _ => None,
    }
}

/// 开/关夜间模式。格式校验不过或写注册表失败返回 false。
pub fn set_enabled(enable: bool) -> bool {
    let Some(mut data) = reg::read_binary(STATE_KEY, "Data") else {
        return false;
    };
    if toggle_state_blob(&mut data, enable).is_none() {
        return false;
    }
    reg::write_binary(STATE_KEY, "Data", &data)
}

/// 纯 blob 操作，便于单测。已是目标状态时原样返回 Ok(false)。
fn toggle_state_blob(data: &mut Vec<u8>, enable: bool) -> Option<bool> {
    if data.len() < 25 {
        return None;
    }
    let currently_on = match data[18] {
        0x15 => true,
        0x13 => false,
        _ => return None,
    };
    if currently_on == enable {
        return Some(false);
    }
    // 最后修改时间戳（5 字节变长编码，分散在 10..14）：第一个非 0xFF 的字节 +1。
    for i in 10..15 {
        if data[i] != 0xFF {
            data[i] += 1;
            break;
        }
    }
    data[18] = if enable { 0x15 } else { 0x13 };
    if enable {
        // 在索引 23 处插入 0x10 0x00。
        data.splice(23..23, [0x10, 0x00]);
    } else {
        // 删除索引 23、24（应为 0x10 0x00，不符则说明格式已变，放弃）。
        if data[23] != 0x10 || data[24] != 0x00 {
            return None;
        }
        data.drain(23..25);
    }
    Some(true)
}

/// 读取当前强度 0–100；优先 per-device（当前显示设备实际生效的那把），
/// 其次主设置键，最后旧版键。都找不到返回 None。
pub fn get_strength() -> Option<u32> {
    let mut keys = settings_keys();
    // perdevice 优先：多显示器/换显示器时它是实际生效值。
    keys.sort_by_key(|k| !k.contains("settingsperdevice"));
    for k in keys {
        if let Some(data) = reg::read_binary(&k, "Data") {
            if let Some(s) = parse_strength(&data) {
                return Some(s);
            }
        }
    }
    None
}

fn parse_strength(data: &[u8]) -> Option<u32> {
    let idx = find_temp_marker(data)?;
    Some(kelvin_to_strength(decode_kelvin(data[idx], data[idx + 1])))
}

/// 定位 `CF 28` 标记，返回其后色温低字节的索引。
fn find_temp_marker(data: &[u8]) -> Option<usize> {
    data.windows(2)
        .position(|w| w == [0xCF, 0x28])
        .map(|i| i + 2)
        .filter(|&i| i + 1 < data.len())
}

/// CloudStore `Current` 下的夜间模式设置类键（完整路径，含叶键）。
/// 同一设置会同时存在多把：主设置 `default$...bluelightreduction.settings`、
/// 每个显示设备的 `{guid}$...settingsperdevice`、旧版命名
/// `default$...bluelightreduction.bluelightreduction.settings`。
/// 实际生效的是 per-device。只写新式键（主设置 + per-device），与系统
/// 设置 App 的写入面保持一致，避免旧版键参与 CloudStore 合并造成冲突；
/// 只有在新式键全不存在（老系统）时才退到旧版键。
/// 叶键名不能从父键名推导（旧版父键的叶名与父名不同），统一枚举子键。
fn settings_keys() -> Vec<String> {
    let mut modern = Vec::new();
    let mut legacy = Vec::new();
    for name in reg::enum_subkeys(CLOUDSTORE_CURRENT) {
        let Some(is_legacy) = classify_settings_parent(&name) else {
            continue;
        };
        let parent = format!(r"{CLOUDSTORE_CURRENT}\{name}");
        for leaf in reg::enum_subkeys(&parent) {
            let full = format!(r"{parent}\{leaf}");
            if is_legacy {
                legacy.push(full);
            } else {
                modern.push(full);
            }
        }
    }
    if modern.is_empty() { legacy } else { modern }
}

/// 纯函数，便于单测：`Current` 子键名 → 是否设置类键。
/// Some(false)=新式（主设置 / per-device），Some(true)=旧版命名，None=无关键。
fn classify_settings_parent(name: &str) -> Option<bool> {
    let (_, suffix) = name.split_once('$')?;
    if !suffix.starts_with("windows.data.bluelightreduction.") || suffix.contains("state") {
        return None;
    }
    if suffix == "windows.data.bluelightreduction.bluelightreduction.settings" {
        return Some(true);
    }
    if suffix.ends_with("settings") || suffix.ends_with("settingsperdevice") {
        return Some(false);
    }
    None
}

/// 设置强度 0–100：写入所有设置类 blob（主设置 + 各设备 perdevice + 旧版键），
/// 并推进 blob 内嵌时间戳，让系统的 CloudStore 监听接受这次修改。
/// 一把都找不到（用户从未碰过夜间模式设置）时按模板重建主设置键。
pub fn set_strength(strength: u32) -> bool {
    let kelvin = strength_to_kelvin(strength);
    let keys = settings_keys();
    if keys.is_empty() {
        return reg::write_binary(SETTINGS_KEY, "Data", &build_settings_template(kelvin));
    }
    let mut any = false;
    for k in keys {
        let Some(mut data) = reg::read_binary(&k, "Data") else {
            continue;
        };
        let Some(idx) = find_temp_marker(&data) else {
            continue;
        };
        let (lo, hi) = encode_kelvin(kelvin);
        data[idx] = lo;
        data[idx + 1] = hi;
        bump_timestamp(&mut data);
        any = reg::write_binary(&k, "Data", &data) || any;
    }
    any
}

/// blob 内嵌最后修改时间戳（字节 10..14 变长编码）：第一个非 0xFF 字节 +1。
fn bump_timestamp(data: &mut [u8]) {
    for b in data.iter_mut().take(15).skip(10) {
        if *b != 0xFF {
            *b += 1;
            break;
        }
    }
}

/// 用户从未碰过夜间模式设置时，settings blob 不存在。
/// 按 Ben N 逆向的 21H2 格式重建完整 blob：排程关闭、0:00–0:00 占位、
/// 给定色温。结构：10 字节头 + 5 字节变长 Unix 时间戳 + 固定字段。
fn build_settings_template(kelvin: u32) -> Vec<u8> {
    let mut d = vec![0x43, 0x42, 0x01, 0x00, 0x0A, 0x02, 0x01, 0x00, 0x2A, 0x06];
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    d.push((t & 0x7F) as u8 | 0x80);
    d.push(((t >> 7) & 0x7F) as u8 | 0x80);
    d.push(((t >> 14) & 0x7F) as u8 | 0x80);
    d.push(((t >> 21) & 0x7F) as u8 | 0x80);
    d.push((t >> 28) as u8);
    d.extend_from_slice(&[0x2A, 0x2B, 0x0E, 0x1D, 0x43, 0x42, 0x01, 0x00]);
    // 排程关闭：不写 0x02 0x01。
    d.extend_from_slice(&[0xCA, 0x14, 0x0E, 0x00, 0x2E, 0x00, 0x00, 0xCA, 0x1E, 0x0E, 0x00, 0x2E, 0x00, 0x00]);
    d.extend_from_slice(&[0xCF, 0x28]);
    let (lo, hi) = encode_kelvin(kelvin);
    d.push(lo);
    d.push(hi);
    d.extend_from_slice(&[0xCA, 0x32, 0x00, 0xCA, 0x3C, 0x00, 0x00, 0x00, 0x00, 0x00]);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个「关」状态的 state blob（长度 41，布局与真实系统一致的部分：
    /// 18 号状态字节、23/24 可插拔字节）。
    fn off_state_blob() -> Vec<u8> {
        let mut d = vec![0u8; 41];
        d[10] = 0x80;
        d[11] = 0x9A;
        d[12] = 0xFF;
        d[13] = 0x81;
        d[14] = 0x07;
        d[18] = 0x13;
        d
    }

    #[test]
    fn state_toggle_roundtrip() {
        let mut d = off_state_blob();
        // 关 -> 开：插入 2 字节，时间戳进位，状态字节翻转。
        assert_eq!(toggle_state_blob(&mut d, true), Some(true));
        assert_eq!(d.len(), 43);
        assert_eq!(d[18], 0x15);
        assert_eq!(d[23], 0x10);
        assert_eq!(d[24], 0x00);
        assert_eq!(d[10], 0x81); // 第一个非 0xFF 字节 +1
        assert_eq!(d[12], 0xFF); // 0xFF 被跳过不进位

        // 开 -> 关：回到原样（除时间戳 +1）。
        assert_eq!(toggle_state_blob(&mut d, false), Some(true));
        assert_eq!(d.len(), 41);
        assert_eq!(d[18], 0x13);

        // 幂等：已是目标状态不再改动。
        let before = d.clone();
        assert_eq!(toggle_state_blob(&mut d, false), Some(false));
        assert_eq!(d, before);
    }

    #[test]
    fn state_toggle_rejects_bad_layout() {
        // 太短。
        let mut short = vec![0u8; 10];
        assert_eq!(toggle_state_blob(&mut short, true), None);
        // 状态字节非法。
        let mut bad = off_state_blob();
        bad[18] = 0x42;
        assert_eq!(toggle_state_blob(&mut bad, true), None);
        // 已是关 → 幂等，不看 23/24。
        let mut bad2 = off_state_blob();
        bad2[23] = 0xAA;
        assert_eq!(toggle_state_blob(&mut bad2, false), Some(false));
        // 先开（成功），破坏 23，再关 → 拒绝。
        let mut bad3 = off_state_blob();
        assert_eq!(toggle_state_blob(&mut bad3, true), Some(true));
        bad3[23] = 0xAA;
        assert_eq!(toggle_state_blob(&mut bad3, false), None);
    }

    #[test]
    fn kelvin_codec_roundtrip() {
        for k in [1200, 3055, 3900, 4750, 6500] {
            let (lo, hi) = encode_kelvin(k);
            assert_eq!(decode_kelvin(lo, hi), k);
        }
    }

    #[test]
    fn strength_kelvin_mapping() {
        assert_eq!(strength_to_kelvin(0), 6500);
        assert_eq!(strength_to_kelvin(100), 1200);
        assert_eq!(strength_to_kelvin(50), 3850);
        assert_eq!(kelvin_to_strength(6500), 0);
        assert_eq!(kelvin_to_strength(1200), 100);
        // 强度 -> 色温 -> 强度 往返稳定。
        for s in [0u32, 1, 25, 50, 75, 99, 100] {
            assert_eq!(kelvin_to_strength(strength_to_kelvin(s)), s);
        }
    }

    #[test]
    fn template_parses_back() {
        let blob = build_settings_template(3900);
        assert_eq!(parse_strength(&blob), Some(kelvin_to_strength(3900)));
    }

    #[test]
    fn marker_not_found() {
        assert_eq!(parse_strength(&[0xCF]), None);
        assert_eq!(parse_strength(&[]), None);
    }

    #[test]
    fn settings_key_filter() {
        // 主设置、per-device 是新式键；旧版命名标 legacy；state 类与无关键排除。
        assert_eq!(
            classify_settings_parent(r"default$windows.data.bluelightreduction.settings"),
            Some(false)
        );
        assert_eq!(
            classify_settings_parent(
                r"{ddbff2b1-f073-4719-a70e-3f8c302b8ca8}$windows.data.bluelightreduction.settingsperdevice"
            ),
            Some(false)
        );
        assert_eq!(
            classify_settings_parent(
                r"default$windows.data.bluelightreduction.bluelightreduction.settings"
            ),
            Some(true)
        );
        for name in [
            r"default$windows.data.bluelightreduction.bluelightreductionstate",
            r"{ddbff2b1-f073-4719-a70e-3f8c302b8ca8}$windows.data.bluelightreduction.bluelightreductionstateperdevice",
            r"default$windows.data.cortana.settings",
            "no-dollar-sign",
        ] {
            assert_eq!(classify_settings_parent(name), None, "{name}");
        }
    }
}
