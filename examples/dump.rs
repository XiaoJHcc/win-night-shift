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
    if args.iter().any(|a| a == "--keys") {
        dump_keys();
    }
    println!("night light enabled: {:?}", nightlight::get_enabled());
    println!("night light strength: {:?}", nightlight::get_strength());
    println!("dark mode: {}", theme::is_dark());
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
