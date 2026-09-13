//! 诊断：打印当前系统状态（夜间模式开关/强度、亮暗模式）。
//! 用法：cargo run --example dump
//!
//! 直接 include 业务模块，未被调用的公开 API 会报 dead_code，统一豁免。
#![allow(dead_code)]

#[path = "../src/reg.rs"]
mod reg;
#[path = "../src/nightlight.rs"]
mod nightlight;
#[path = "../src/theme.rs"]
mod theme;

fn main() {
    // dump --set <0-100>：先设置强度再打印（端到端验证用）。
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--set") {
        let v: u32 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(50);
        println!("set_strength({v}) -> {}", nightlight::set_strength(v));
    }
    if let Some(pos) = args.iter().position(|a| a == "--set-perdevice") {
        let v: u32 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(50);
        println!(
            "set_strength_perdevice_only({v}) -> {}",
            nightlight::set_strength_perdevice_only(v)
        );
    }
    if let Some(pos) = args.iter().position(|a| a == "--set-perdevice-fixts") {
        let v: u32 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(50);
        println!(
            "set_strength_perdevice_fixts({v}) -> {}",
            nightlight::set_strength_perdevice_fixts(v)
        );
    }
    if args.iter().any(|a| a == "--poke") {
        println!("poke_state() -> {}", nightlight::poke_state());
    }
    // dump --cycle <ms>：实验用。夜间模式开关翻转一次再翻回（间隔 ms），
    // 测试「状态跳变触发系统重新应用 settings」的最小间隔与闪屏观感。
    if let Some(pos) = args.iter().position(|a| a == "--cycle") {
        let ms: u64 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(300);
        let was_on = nightlight::get_enabled().unwrap_or(false);
        println!("enabled before: {was_on}");
        println!("flip away -> {}", nightlight::set_enabled(!was_on));
        std::thread::sleep(std::time::Duration::from_millis(ms));
        println!("flip back -> {}", nightlight::set_enabled(was_on));
    }
    // dump --poke-struct <ms>：实验用。state blob 保持字节 18=0x15（开）不变，
    // 先删除 23/24 的 10 00 结构标记、间隔 ms 后再插回（两个 blob 都是系统
    //  canonical 形态），测试「结构变化但逻辑状态不变」是否触发重新应用。
    if let Some(pos) = args.iter().position(|a| a == "--poke-struct") {
        let ms: u64 = args.get(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(50);
        println!("poke_struct({ms}) -> {}", poke_struct(ms));
    }
    if args.iter().any(|a| a == "--fresh") {
        println!("make_fresh() -> {}", make_fresh());
    }
    if args.iter().any(|a| a == "--keys") {
        dump_keys();
    }
    println!("night light enabled: {:?}", nightlight::get_enabled());
    println!("night light strength: {:?}", nightlight::get_strength());
    println!("dark mode: {}", theme::is_dark());
}

/// 实验用（dump --poke-struct）：state blob 保持字节 18=0x15（开）不变，
/// 先删除 23/24 的 `10 00` 结构标记、间隔 ms 后再插回。两个中间 blob 都是
/// 系统自己会产生的 canonical 形态（开=43 字节有标记 / 关=41 字节无标记），
/// 不制造未知布局。用于测试系统的重新应用触发器认不认「结构差分」。
fn poke_struct(ms: u64) -> bool {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current\default$windows.data.bluelightreduction.bluelightreductionstate\windows.data.bluelightreduction.bluelightreductionstate";
    let Some(data) = reg::read_binary(KEY, "Data") else {
        return false;
    };
    if data.len() != 43 || data[18] != 0x15 || data[23] != 0x10 || data[24] != 0x00 {
        println!("unexpected state blob shape, abort: {:02X?}", data);
        return false;
    }
    let mut stripped = data.clone();
    stripped.splice(23..25, []);
    if !reg::write_binary(KEY, "Data", &stripped) {
        return false;
    }
    std::thread::sleep(std::time::Duration::from_millis(ms));
    reg::write_binary(KEY, "Data", &data)
}

/// 实验用（dump --fresh）：把主设置键 blob 回退成「系统重建后的新鲜形态」——
/// 移除 CF 28 色温字段、字节 18 改回 0x15。用于复现「夜间模式已开启 +
/// 新鲜 blob 时，拖强度拉条实时不生效」的场景。
fn make_fresh() -> bool {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current\default$windows.data.bluelightreduction.settings\windows.data.bluelightreduction.settings";
    let Some(mut data) = reg::read_binary(KEY, "Data") else {
        println!("read failed");
        return false;
    };
    let Some(i) = data.windows(2).position(|w| w == [0xCF, 0x28]) else {
        println!("no CF 28, already fresh?");
        return false;
    };
    println!("before (len={}): {:02X?}", data.len(), data);
    data.splice(i..i + 4, []);
    data[18] = 0x15;
    println!("after  (len={}): {:02X?}", data.len(), data);
    reg::write_binary(KEY, "Data", &data)
}

/// 列出 CloudStore 下所有夜间模式相关键：解析出的色温/强度、blob 时间戳字节、
/// 注册表最后写入时间。用于对比系统设置与本工具各写了哪把键。
fn dump_keys() {
    const CURRENT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CloudStore\Store\DefaultAccount\Current";
    for parent in reg::enum_subkeys(CURRENT) {
        if !parent.contains("bluelightreduction") {
            continue;
        }
        let parent_path = format!(r"{CURRENT}\{parent}");
        for leaf in reg::enum_subkeys(&parent_path) {
            let full = format!(r"{parent_path}\{leaf}");
            let Some(data) = reg::read_binary(&full, "Data") else {
                println!("{parent} (no Data)");
                continue;
            };
            // CF 28 标记后两字节是色温。
            let temp = data
                .windows(2)
                .position(|w| w == [0xCF, 0x28])
                .filter(|&i| i + 3 < data.len())
                .map(|i| {
                    let (lo, hi) = (data[i + 2], data[i + 3]);
                    ((lo >> 1) as u32 & 0x3F) | ((hi as u32) << 6)
                });
            let ts: Vec<String> = data[10..15.min(data.len())]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect();
            let state = data.get(18).map(|b| format!("{b:02X}"));
            let lw = reg::key_last_write(&full)
                .map(|t| format!("{t:016X}"))
                .unwrap_or_else(|| "?".into());
            println!(
                "{parent}\n    len={} ts[10..14]={} state[18]={state:?} kelvin={temp:?} lastwrite={lw}",
                data.len(),
                ts.join(" ")
            );
        }
    }
}
