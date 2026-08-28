//! 系统托盘图标。
//!
//! 图标来源：构建期嵌入的 img/icon.ico（资源 ID 1）；素材未提供时回退到
//! 系统库存图标（IDI_INFORMATION）栅格化为 RGBA —— 用的是系统图标，
//! 不做任何自绘。
//!
//! 不挂原生菜单：挂了菜单，右键会被 tray-icon 的 TrackPopupMenu 抢先接管，
//! 收不到 TrayIconEvent；不挂则左右键都只发事件，由主循环转给浮窗。

use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct Tray {
    _tray: TrayIcon,
}

fn make_icon() -> Option<Icon> {
    // 构建期嵌入了 icon.ico 时走这里。
    if let Ok(icon) = Icon::from_resource(1, Some((16, 16))) {
        return Some(icon);
    }
    // 素材未提供：回退到系统库存图标。
    let (rgba, w, h) = stock_icon_rgba()?;
    Icon::from_rgba(rgba, w, h).ok()
}

impl Tray {
    pub fn new() -> Option<Tray> {
        let mut builder = TrayIconBuilder::new()
            .with_tooltip("win-night-shift")
            .with_menu_on_left_click(false);
        if let Some(icon) = make_icon() {
            builder = builder.with_icon(icon);
        }
        Some(Tray {
            _tray: builder.build().ok()?,
        })
    }
}

/// 把系统库存图标（信息图标）栅格化为 32bpp RGBA。
fn stock_icon_rgba() -> Option<(Vec<u8>, u32, u32)> {
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS, GetDC, HGDIOBJ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, GetIconInfo, LoadIconW, ICONINFO, IDI_INFORMATION,
    };
    unsafe {
        let hicon = LoadIconW(None, IDI_INFORMATION).ok()?;
        let mut info = ICONINFO::default();
        if GetIconInfo(hicon, &mut info).is_err() {
            let _ = DestroyIcon(hicon);
            return None;
        }
        let mut bm = BITMAP::default();
        if GetObjectW(
            HGDIOBJ(info.hbmColor.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut _),
        ) == 0
        {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
            let _ = DestroyIcon(hicon);
            return None;
        }
        let (w, h) = (bm.bmWidth.max(1) as u32, bm.bmHeight.max(1) as u32);

        let read_32bpp = |hbm: windows::Win32::Graphics::Gdi::HBITMAP| -> Option<Vec<u8>> {
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // 自顶向下
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let hdc = GetDC(None);
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let lines = GetDIBits(
                hdc,
                hbm,
                0,
                h,
                Some(buf.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            let _ = ReleaseDC(None, hdc);
            (lines == h as i32).then_some(buf)
        };

        let color = read_32bpp(info.hbmColor);
        let mask = if info.hbmMask.is_invalid() {
            None
        } else {
            read_32bpp(info.hbmMask)
        };
        let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        let _ = DestroyIcon(hicon);

        let mut rgba = color?;
        // BGRA -> RGBA。
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        // 彩色位图 alpha 全 0 时，用掩码位图推导透明度（掩码 0=不透明）。
        let alpha_all_zero = rgba.chunks_exact(4).all(|px| px[3] == 0);
        if alpha_all_zero {
            if let Some(mask) = mask {
                for (px, mk) in rgba.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
                    px[3] = if mk[0] == 0 { 255 } else { 0 };
                }
            } else {
                for px in rgba.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }
        }
        Some((rgba, w, h))
    }
}
