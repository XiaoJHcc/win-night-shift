//! 一次性图标生成器：把 img/moon.svg（lucide moon，ISC 许可）渲染成三份 .ico：
//!
//!   img/icon.ico       —— 琥珀色，exe 应用图标（build.rs 嵌入为资源 ID 1）
//!   img/tray-white.ico —— 纯白托盘图标，深色任务栏用（运行时 include_bytes!）
//!   img/tray-black.ico —— 近黑托盘图标，浅色任务栏用
//!
//! 托盘图标的配色依据是系统任务栏主题（亮 → 深 glyph、暗 → 白 glyph，
//! 与系统自带托盘图标一致），主题判定见 src/tray.rs 的 taskbar_is_light。
//!
//! 用法：`cargo run --example makeicon`
//! 同时输出 img/preview-*.png 便于人工检查渲染效果。

use std::fs;

/// 进 ico 的尺寸档位；256 由系统自动缩放给高分屏。
const SIZES: &[u32] = &[16, 24, 32, 48, 64, 256];

/// 应用图标颜色：琥珀色，亮/暗背景下都可见。
const APP_COLOR: &str = "#F5B942";
/// 托盘 glyph 颜色：与系统自带托盘图标一致——深色任务栏纯白、
/// 浅色任务栏近黑（取色自系统托盘图标，#181919）。
const TRAY_WHITE: &str = "#FFFFFF";
const TRAY_BLACK: &str = "#181919";

fn main() {
    let svg = fs::read_to_string("img/moon.svg").expect("read img/moon.svg");

    // 应用图标：边距 2px（大尺寸展示用，贴边难看），笔画保持 lucide 原始 2。
    let app = svg
        .replace("viewBox=\"0 0 24 24\"", "viewBox=\"-2 -2 28 28\"")
        .replace("currentColor", APP_COLOR);
    write_ico(&app, "img/icon.ico", "icon");

    // 托盘图标：边距收紧到 1px（16px 档位 glyph 要占满），笔画保持
    // lucide 原始 2（加粗试过，用户反馈偏粗）。
    let tray_base = svg
        .replace("viewBox=\"0 0 24 24\"", "viewBox=\"-1 -1 26 26\"");
    write_ico(
        &tray_base.replace("currentColor", TRAY_WHITE),
        "img/tray-white.ico",
        "tray-white",
    );
    write_ico(
        &tray_base.replace("currentColor", TRAY_BLACK),
        "img/tray-black.ico",
        "tray-black",
    );
}

fn write_ico(svg: &str, path: &str, preview_prefix: &str) {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
        .expect("parse svg");
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in SIZES {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
        let scale = size as f32 / tree.size().width();
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        // tiny-skia 输出是预乘 alpha，转成普通 RGBA 再进 ico。
        let mut rgba = Vec::with_capacity((size * size * 4) as usize);
        for p in pixmap.pixels() {
            let c = p.demultiply();
            rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        let img = ico::IconImage::from_rgba_data(size, size, rgba);
        dir.add_entry(ico::IconDirEntry::encode(&img).expect("encode ico entry"));
        // 16/48 两档出目检图。
        if size == 16 || size == 48 {
            pixmap
                .save_png(format!("img/preview-{preview_prefix}-{size}.png"))
                .expect("save preview");
        }
    }
    let mut out = fs::File::create(path).expect("create ico");
    dir.write(&mut out).expect("write ico");
    println!("{path} written ({} sizes)", SIZES.len());
}
