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
    println!("night light enabled: {:?}", nightlight::get_enabled());
    println!("night light strength: {:?}", nightlight::get_strength());
    println!("dark mode: {}", theme::is_dark());
}
