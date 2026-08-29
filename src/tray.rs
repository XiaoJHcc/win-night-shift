//! 系统托盘图标。
//!
//! 图标来源：构建期嵌入的 img/icon.ico（资源 ID 1）；素材未提供时回退到
//! 系统库存图标（IDI_INFORMATION）栅格化为 RGBA —— 用的是系统图标，
//! 不做任何自绘。
//!
//! 左右键分工：
//!  * 左键不挂菜单（`with_menu_on_left_click(false)`）：只发 TrayIconEvent，
//!    由主循环拨动浮窗；
//!  * 右键挂 muda 菜单，由 tray-icon 内置的 TrackPopupMenu 弹出——
//!    Win11 下即系统样式的圆角右键菜单（开机自启勾选 / 设置 / 退出）。
//!    菜单项点击经 `MenuEvent` 通道投出，由 `handle_menu_events` 处理。

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows::Win32::UI::WindowsAndMessaging::PostQuitMessage;

pub struct Tray {
    _tray: TrayIcon,
    /// 「开机自启」勾选项。muda 在投事件前已自动翻转勾选态（见
    /// muda::platform_impl::windows 的 menu_selected），处理时现读即为目标值。
    autostart_item: CheckMenuItem,
    /// 「退出」项的 id，用于事件匹配。
    quit_id: MenuId,
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
        // 右键菜单：开机自启（勾选）/ 设置（预留，禁用态）/ 退出。
        // 勾选初值现读注册表；此后勾选态只经本菜单改动，二者不会失步。
        let menu = Menu::new();
        let autostart_item =
            CheckMenuItem::new("开机自启", true, crate::autostart::is_autostart(), None);
        let settings_item = MenuItem::new("设置", false, None);
        let quit_item = MenuItem::new("退出", true, None);
        let quit_id = quit_item.id().clone();
        menu.append(&autostart_item).ok()?;
        menu.append(&settings_item).ok()?;
        menu.append(&PredefinedMenuItem::separator()).ok()?;
        menu.append(&quit_item).ok()?;

        let mut builder = TrayIconBuilder::new()
            .with_tooltip("win-night-shift")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false);
        if let Some(icon) = make_icon() {
            builder = builder.with_icon(icon);
        }
        Some(Tray {
            _tray: builder.build().ok()?,
            autostart_item,
            quit_id,
        })
    }

    /// 处理右键菜单项点击（主循环每轮泵一次）。
    pub fn handle_menu_events(&self) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.quit_id {
                // 本函数跑在消息循环所在线程，直接投 WM_QUIT 即可。
                unsafe { PostQuitMessage(0) };
            } else if event.id == *self.autostart_item.id() {
                // muda 投事件前已翻转勾选态，is_checked 即用户意图；
                // 写注册表失败时把控件扳回。
                let v = self.autostart_item.is_checked();
                if !crate::autostart::set_autostart(v) {
                    self.autostart_item.set_checked(!v);
                }
            }
            // 「设置」为预留项，禁用态，不会产生事件。
        }
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
