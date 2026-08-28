//! 构建期资源：img/icon.ico 存在则嵌入为 exe 应用图标（资源 ID 1，供
//! 运行期 `Icon::from_resource(1, ..)` 读取）；素材未提供时跳过，托盘回退
//! 到系统库存图标（见 src/tray.rs）。UI 不做任何自绘，图标只来自 .ico。

fn main() {
    println!("cargo:rerun-if-changed=img/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = std::path::Path::new("img/icon.ico");
    if icon.exists() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon.to_str().unwrap());
        res.compile().expect("embed application icon");
    }
}
