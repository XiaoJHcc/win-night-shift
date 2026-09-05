//! Windows 夜间模式（Night Light）控制。
//!
//! 没有公开 API，使用未文档化的 CloudStore 注册表 blob（社区逆向，Win10 2004+
//! 与 Win11 通用，写入后立即生效）：
//!
//! - 开关：`...\bluelightreductionstate` 键的 `Data`。
//!   `data[18] == 0x15` 为开、`0x13` 为关；翻转时推进内嵌时间戳（策略见
//!   `bump_timestamp`），并在索引 23 处插入/删除 `0x10 0x00` 两字节。
//! - 强度：CloudStore 下同一设置同时存在多把键——主设置
//!   `default$...bluelightreduction.settings`、每个显示设备的
//!   `{guid}$...settingsperdevice`（由系统自己维护，见下文写入策略）、
//!   旧版命名 `default$...bluelightreduction.bluelightreduction.settings`。
//!   blob 中 `CF 28` 标记后的两个字节是色温（1200–6500K）：
//!   `lo = ((K & 0x3F) << 1) | 0x80`，`hi = K >> 6`。
//!   原地改这两字节并推进内嵌时间戳，不动排程等字段。
//!
//! 所有写入三道防线：写前结构校验（`structure_ok`，不认识的格式不写）、
//! 时间戳写真实当前时间（`bump_timestamp`）、写后回读校验（不符即失败，
//! UI 扳回控件）。宁可某次写入不生效，也不制造或扩大系统不可读的 blob。
//!
//! 所有操作对 blob 做长度/标记校验，格式不符时静默失败（返回 false/None）。
//!
//! # 写入策略（实测结论）
//! **只写主设置键**，与系统设置 App 的写入面一致：只写主键即可让屏幕色温
//! 即时生效、长期保持。per-device 键不写：
//!  * 它是系统维护的派生副本——系统落盘时会把主键值同步过去（两键
//!    注册表写入时间完全相同），外部写入对它的显示效果为零；
//!  * 内嵌时间戳全 0xFF 的 blob（系统重置后的状态）写入后被系统无视；
//!    有效时间戳的单次写入虽不生效但也不被惩罚（观察 4 分钟无重置），
//!    但早期版本在拖动中高频同时写主键+per-device 时曾被系统判定冲突、
//!    把夜间模式打回关闭——既然写了也没用，就不写，远离出事条件。
//!
//! # 读取策略（实测结论）
//! 同一时刻多把键的值可能互相矛盾：系统设置 App 拖强度拉条**只写主设置键**，
//! 而且是延迟落盘（拖动时不写，离开页面/过一阵才刷入）；per-device 键被
//! 系统冲突重置后残留旧值。因此读取不看固定优先级，而是跳过重置标记的
//! blob、取**注册表最后写入时间最新**的那把键的值。

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
    if !reg::write_binary(STATE_KEY, "Data", &data) {
        return false;
    }
    // 回读校验（同 write_kelvin_to）：失败让 UI 扳回控件，不静默失效。
    get_enabled() == Some(enable)
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
    bump_timestamp(data);
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

/// 读取当前强度 0–100。
///
/// 多把键可能并存且值互相矛盾（系统设置只写主设置键；per-device 键被系统
/// 冲突重置后残留旧值），在跳过重置 blob 后取**注册表最后写入时间最新**
/// 的那把键的值。都找不到返回 None。
pub fn get_strength() -> Option<u32> {
    let mut best: Option<(u64, u32)> = None;
    for k in all_settings_keys() {
        let Some(data) = reg::read_binary(&k, "Data") else {
            continue;
        };
        if is_reset_blob(&data) {
            continue;
        }
        let Some(s) = parse_strength(&data) else {
            continue;
        };
        let t = reg::key_last_write(&k).unwrap_or(0);
        if best.map_or(true, |(bt, _)| t > bt) {
            best = Some((t, s));
        }
    }
    Some(best?.1)
}

/// blob 内嵌时间戳（字节 10..14）全 0xFF：系统冲突重置的标记，值不可信。
fn is_reset_blob(data: &[u8]) -> bool {
    data.len() > 15 && data[10..15].iter().all(|&b| b == 0xFF)
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
/// 叶键名不能从父键名推导（旧版父键的叶名与父名不同），统一枚举子键。
///
/// 写入只用新式键（主设置 + per-device），与系统设置 App 的写入面保持一致，
/// 避免旧版键参与 CloudStore 合并造成冲突；只有在新式键全不存在（老系统）
/// 时才退到旧版键。读取则用 `all_settings_keys` 全量比较新旧（见 get_strength）。
fn settings_keys() -> Vec<String> {
    let (modern, legacy) = collect_settings_keys();
    if modern.is_empty() { legacy } else { modern }
}

/// 全部设置类键（新式 + 旧版），读取用。
fn all_settings_keys() -> Vec<String> {
    let (mut modern, legacy) = collect_settings_keys();
    modern.extend(legacy);
    modern
}

fn collect_settings_keys() -> (Vec<String>, Vec<String>) {
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
    (modern, legacy)
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

/// 设置强度 0–100：**只写主设置键**（与系统设置 App 的写入面一致），
/// 并推进 blob 内嵌时间戳，让系统的 CloudStore 监听接受这次修改。
///
/// per-device 键不写：它是系统维护的派生副本，外部写入对显示效果为零
/// （FF 时间戳的写入被系统无视；有效时间戳的单次写入也不生效）；
/// 早期版本高频同时写主键+per-device 曾被系统判定冲突打回夜间模式，
/// 既然写了没用，就远离出事条件。详见模块头注释。
/// 只写主键已实测能让屏幕色温即时生效且长期不被系统改动。
/// 一把都找不到（用户从未碰过夜间模式设置）时按模板重建主设置键。
pub fn set_strength(strength: u32) -> bool {
    let kelvin = strength_to_kelvin(strength);
    let keys: Vec<String> = settings_keys()
        .into_iter()
        .filter(|k| !k.contains("settingsperdevice"))
        .collect();
    if keys.is_empty() {
        return reg::write_binary(SETTINGS_KEY, "Data", &build_settings_template(kelvin));
    }
    let mut any = false;
    for k in keys {
        any = write_kelvin_to(&k, kelvin) || any;
    }
    any
}

/// 只写 per-device 键（诊断/对照实验用，examples/dump --set-perdevice）。
/// 注意：这条路径在生产代码里被有意避开，见 set_strength 的注释。
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_strength_perdevice_only(strength: u32) -> bool {
    let kelvin = strength_to_kelvin(strength);
    let mut any = false;
    for k in settings_keys()
        .into_iter()
        .filter(|k| k.contains("settingsperdevice"))
    {
        any = write_kelvin_to(&k, kelvin) || any;
    }
    any
}

/// 诊断（examples/dump --set-perdevice-fixts）：先把 per-device blob 的
/// 全 0xFF 内嵌时间戳修复为当前时间（5 字节变长编码，同模板）再写色温。
/// 用于复现「带有效时间戳的外部 per-device 写入」这一条件。
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_strength_perdevice_fixts(strength: u32) -> bool {
    let kelvin = strength_to_kelvin(strength);
    let mut any = false;
    for k in settings_keys()
        .into_iter()
        .filter(|k| k.contains("settingsperdevice"))
    {
        let Some(mut data) = reg::read_binary(&k, "Data") else {
            continue;
        };
        let Some(idx) = find_temp_marker(&data) else {
            continue;
        };
        if is_reset_blob(&data) {
            encode_timestamp(&mut data, current_unix_secs());
        }
        let (lo, hi) = encode_kelvin(kelvin);
        data[idx] = lo;
        data[idx + 1] = hi;
        bump_timestamp(&mut data);
        any = reg::write_binary(&k, "Data", &data) || any;
    }
    any
}

/// 原地改一把设置键的色温两字节并推进内嵌时间戳。
///
/// 三道防线：
///  1. 写入前整体结构校验——遇到不认识的格式宁可不写，避免把系统可读的
///     异常改写成系统不可读的异常（2026-09 实测过系统主设置键 blob 布局
///     与已知三种都不一样的实例，该实例最终把系统夜间模式栈拖垮）；
///  2. 时间戳按 bump_timestamp 的策略写真实时间；
///  3. 写入后回读校验——系统可能拒绝或改写这次写入（键被冲突重置等），
///     值不符上报失败，让 UI 把控件扳回真实状态，而不是静默失效。
fn write_kelvin_to(key: &str, kelvin: u32) -> bool {
    let Some(mut data) = reg::read_binary(key, "Data") else {
        return false;
    };
    if !structure_ok(&data) {
        return false;
    }
    if !set_kelvin_in_blob(&mut data, kelvin) {
        return false;
    }
    if !reg::write_binary(key, "Data", &data) {
        return false;
    }
    let (lo, hi) = encode_kelvin(kelvin);
    reg::read_binary(key, "Data")
        .and_then(|d| find_temp_marker(&d).map(|i| d[i] == lo && d[i + 1] == hi))
        .unwrap_or(false)
}

/// 纯 blob 操作，便于单测：把色温写进 settings blob 并推进内嵌时间戳。
///
/// 已有 `CF 28` 字段则原地改两字节；没有则在**唯一**的 `CA 32` 字段前插入
/// `CF 28 + 两字节色温`——系统重建后的新鲜 blob 在首次设置强度前就不含
/// 温度字段，系统首次拖强度拉条时就是在这个位置插入，照做。
/// `CA 32` 不唯一说明格式不认识，放弃。
fn set_kelvin_in_blob(data: &mut Vec<u8>, kelvin: u32) -> bool {
    let (lo, hi) = encode_kelvin(kelvin);
    match find_temp_marker(data) {
        Some(idx) => {
            data[idx] = lo;
            data[idx + 1] = hi;
        }
        None => {
            let mut it = data
                .windows(2)
                .enumerate()
                .filter(|(_, w)| *w == [0xCA, 0x32])
                .map(|(i, _)| i);
            let (Some(pos), None) = (it.next(), it.next()) else {
                return false;
            };
            data.splice(pos..pos, [0xCF, 0x28, lo, hi]);
            // 系统首次写入色温时会把字节 18 从 0x15 改为 0x19（实测逐字节对比
            // 确认）。0x15 的 blob 能被读取（开关时应用）但实时应用路径不认——
            // 写入落盘而屏幕不跟随，必须随插入一起改。其他值不认识，不动。
            if data.get(18) == Some(&0x15) {
                data[18] = 0x19;
            }
        }
    }
    bump_timestamp(data);
    true
}

/// 写入前的整体结构校验（settings blob）：已知布局的公共特征——
/// `43 42 01 00` 头、长度在正常区间。不认识就拒绝写。
fn structure_ok(data: &[u8]) -> bool {
    (32..=64).contains(&data.len()) && data.starts_with(&[0x43, 0x42, 0x01, 0x00])
}

/// blob 内嵌最后修改时间戳（字节 10..14，5 字节变长 Unix 时间）。
///
/// 取值策略：正常时间戳（2020..现在+1天）写 `max(当前时间, 原值+1)`——
/// 与系统设置 App 每次写真实当前时间的行为一致，同时保证同一秒内的
/// 连续写入严格递增；损坏值（全 0xFF 重置标记、来源不明的远未来值等，
/// 实测出现过「年 3059」）直接重写为当前时间修复。
/// 早期版本只做「第一个非 0xFF 字节 +1」：既不会自愈损坏值，长期累积
/// 还会把时间戳推进到远离真实的未来。
fn bump_timestamp(data: &mut [u8]) {
    let now = current_unix_secs();
    let next = match embedded_timestamp(data) {
        Some(t) if (UNIX_2020..=now + 86400).contains(&t) => (t + 1).max(now),
        _ => now,
    };
    encode_timestamp(data, next);
}

/// 2020-01-01 的 Unix 时间，内嵌时间戳合理性下限（Win10 2004 发布年）。
const UNIX_2020: u64 = 1577836800;

fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 解码 blob 内嵌时间戳（字节 10..14，5 字节变长 Unix 时间）；太短返回 None。
fn embedded_timestamp(data: &[u8]) -> Option<u64> {
    if data.len() < 15 {
        return None;
    }
    Some(
        (data[10] as u64 & 0x7F)
            | ((data[11] as u64 & 0x7F) << 7)
            | ((data[12] as u64 & 0x7F) << 14)
            | ((data[13] as u64 & 0x7F) << 21)
            | ((data[14] as u64) << 28),
    )
}

/// 把字节 10..14 写成给定 Unix 时间的 5 字节变长编码。调用方保证 len >= 15。
fn encode_timestamp(data: &mut [u8], t: u64) {
    data[10] = (t & 0x7F) as u8 | 0x80;
    data[11] = ((t >> 7) & 0x7F) as u8 | 0x80;
    data[12] = ((t >> 14) & 0x7F) as u8 | 0x80;
    data[13] = ((t >> 21) & 0x7F) as u8 | 0x80;
    data[14] = (t >> 28) as u8;
}

/// 用户从未碰过夜间模式设置时，settings blob 不存在。
/// 按 Ben N 逆向的 21H2 格式重建完整 blob：排程关闭、0:00–0:00 占位、
/// 给定色温。结构：10 字节头 + 5 字节变长 Unix 时间戳 + 固定字段。
fn build_settings_template(kelvin: u32) -> Vec<u8> {
    let mut d = vec![0x43, 0x42, 0x01, 0x00, 0x0A, 0x02, 0x01, 0x00, 0x2A, 0x06];
    d.resize(15, 0);
    encode_timestamp(&mut d, current_unix_secs());
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
        // 关 -> 开：插入 2 字节，时间戳推进到当前时间，状态字节翻转。
        assert_eq!(toggle_state_blob(&mut d, true), Some(true));
        assert_eq!(d.len(), 43);
        assert_eq!(d[18], 0x15);
        assert_eq!(d[23], 0x10);
        assert_eq!(d[24], 0x00);
        let now = current_unix_secs();
        let t1 = embedded_timestamp(&d).unwrap();
        assert!(t1 >= now && t1 <= now + 5);

        // 开 -> 关：回到原样（除时间戳）。
        assert_eq!(toggle_state_blob(&mut d, false), Some(true));
        assert_eq!(d.len(), 41);
        assert_eq!(d[18], 0x13);

        // 幂等：已是目标状态不再改动。
        let before = d.clone();
        assert_eq!(toggle_state_blob(&mut d, false), Some(false));
        assert_eq!(d, before);
    }

    #[test]
    fn structure_check() {
        assert!(structure_ok(&build_settings_template(3900)));
        assert!(!structure_ok(&[]));
        assert!(!structure_ok(&[0u8; 48])); // 头字节不对
        let mut long = vec![0x43, 0x42, 0x01, 0x00];
        long.resize(100, 0);
        assert!(!structure_ok(&long)); // 头对但长度超区间
    }

    #[test]
    fn set_kelvin_modifies_in_place() {
        let mut d = build_settings_template(3900);
        let len = d.len();
        assert!(set_kelvin_in_blob(&mut d, strength_to_kelvin(75)));
        assert_eq!(d.len(), len); // 已有 CF 28：原地改，长度不变
        assert_eq!(parse_strength(&d), Some(75));
    }

    #[test]
    fn set_kelvin_inserts_into_fresh_blob() {
        // 构造「新鲜 blob」：去掉模板的 CF 28 温度字段、字节 18 置为 0x15
        // （系统重建后、首次设强度前的真实形态）。
        let mut fresh = build_settings_template(3900);
        let idx = fresh
            .windows(2)
            .position(|w| w == [0xCF, 0x28])
            .unwrap();
        fresh.splice(idx..idx + 4, []);
        fresh[18] = 0x15;
        assert_eq!(parse_strength(&fresh), None);

        assert!(set_kelvin_in_blob(&mut fresh, strength_to_kelvin(25)));
        assert_eq!(parse_strength(&fresh), Some(25));
        // 插入位置：CF 28 紧跟在 CA 32 字段前。
        let ca32 = fresh.windows(2).position(|w| w == [0xCA, 0x32]).unwrap();
        assert_eq!(&fresh[ca32 - 4..ca32], &[0xCF, 0x28, fresh[ca32 - 2], fresh[ca32 - 1]]);
        // 字节 18 随插入从 0x15 改为 0x19（与系统首次写色温的行为一致），
        // 否则实时应用路径不认这次写入。
        assert_eq!(fresh[18], 0x19);
        // 已经是其他值的 blob 不动这个字节（模板为 0x1D）。
        let mut legacy = build_settings_template(3900);
        let idx = legacy.windows(2).position(|w| w == [0xCF, 0x28]).unwrap();
        legacy.splice(idx..idx + 4, []);
        assert!(set_kelvin_in_blob(&mut legacy, strength_to_kelvin(25)));
        assert_eq!(legacy[18], 0x1D);
    }

    #[test]
    fn set_kelvin_rejects_unknown_layout() {
        // 无 CF 28 也无 CA 32。
        let mut d = vec![0x43, 0x42, 0x01, 0x00];
        d.resize(40, 0);
        assert!(!set_kelvin_in_blob(&mut d, 3900));
        // CA 32 出现两次，不敢判断插入点。
        let mut d2 = build_settings_template(3900);
        let idx = d2.windows(2).position(|w| w == [0xCF, 0x28]).unwrap();
        d2.splice(idx..idx + 4, []);
        d2.extend_from_slice(&[0xCA, 0x32]); // 第二个 CA 32
        let before = d2.clone();
        assert!(!set_kelvin_in_blob(&mut d2, 3900));
        assert_eq!(d2, before); // 拒绝时不留副作用
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
    fn bump_advances_sane_timestamp() {
        let mut d = build_settings_template(3900);
        let before = embedded_timestamp(&d).unwrap();
        bump_timestamp(&mut d);
        assert!(embedded_timestamp(&d).unwrap() > before);
    }

    #[test]
    fn bump_repairs_broken_timestamp() {
        let now = current_unix_secs();
        let repaired_to_now = |d: &mut Vec<u8>| {
            bump_timestamp(d);
            embedded_timestamp(d).unwrap()
        };
        // 全 0xFF（系统冲突重置标记）：+1 无处可加，修复为当前时间。
        let mut reset = build_settings_template(3900);
        for b in reset.iter_mut().take(15).skip(10) {
            *b = 0xFF;
        }
        assert!((now..=now + 5).contains(&repaired_to_now(&mut reset)));
        // 远未来损坏值（实测出现过的「年 3059」）：同样修复。
        let mut future = build_settings_template(3900);
        future[14] = 0x80;
        assert!((now..=now + 5).contains(&repaired_to_now(&mut future)));
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
