//! win-night-shift —— 常驻托盘的 Windows 小工具：
//! 夜间模式（Night Light）开关与强度拉条、亮/暗模式切换、开机自启。
//! 设置 UI 为 WinUI 3 托盘浮窗（XAML Islands），依赖系统已装的 WindowsAppRuntime；
//! 运行时缺失时不做回退，直接退出。
#![windows_subsystem = "windows"]

mod autostart;
mod flyout;
mod nightlight;
mod reg;
mod theme;
mod tray;
mod watch;

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_DESTROY, WM_TIMER, WNDCLASSW,
};

/// 浮窗动画 timer。当前面板高度固定、无展开动画，timer 不会被启动；
/// 保留分发与 lock-ime 的消息循环结构对齐（见 flyout::on_anim_tick）。
pub const TIMER_FLYOUT_ANIM: usize = 1;

fn main() {
    // 声明 Per-Monitor-V2 DPI 感知：必须在创建任何窗口之前调用，
    // 否则浮窗会被系统位图拉伸，在高 DPI 下发虚。
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }

    // 隐藏消息窗口：浮窗动画的 WM_TIMER 宿主（见 flyout::on_anim_tick），
    // 兼作注册表变更通知的接收窗口（见 watch.rs）。
    let Some(hidden) = create_hidden_window() else {
        return;
    };

    // 浮窗需在托盘之前初始化：预建面板把首帧开销挪到启动阶段。
    // WindowsAppRuntime 缺失等初始化失败时不做回退，直接退出。
    if !flyout::init() {
        return;
    }

    // 托盘必须在消息循环所在线程创建。
    let Some(_tray) = tray::Tray::new() else {
        return;
    };

    // 注册表变更监听：系统设置等外部改动时投 WM_SETTINGS_CHANGED 给隐藏窗口。
    watch::start(hidden);

    // 调试钩子：置 WNS_DEBUG_FLYOUT 时启动后立即弹出浮窗（截图校对布局用），
    // 且失焦不收起（见 flyout::init 的 PINNED）。锚点取屏幕右下角附近，
    // 模拟真实托盘位置。
    #[cfg(debug_assertions)]
    if std::env::var_os("WNS_DEBUG_FLYOUT").is_some() {
        flyout::toggle_at(tray_icon::Rect {
            position: tray_icon::dpi::PhysicalPosition { x: 1600.0, y: 1100.0 },
            size: tray_icon::dpi::PhysicalSize { width: 24, height: 24 },
        });
    }

    let mut msg = MSG::default();
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break; // 0 = WM_QUIT，-1 = 错误。
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 托盘不挂原生菜单，左右键都只发事件：一律拨动浮窗。
        // 只认 Up：一次点击的 Down/Up 都响应会触发两次。
        while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
            if let tray_icon::TrayIconEvent::Click {
                rect,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = event
            {
                flyout::toggle_at(rect);
            }
        }
    }
}

/// 创建一个 message-only 隐藏窗口，用于接收 WM_TIMER。
fn create_hidden_window() -> Option<HWND> {
    unsafe {
        let hmodule = GetModuleHandleW(None).ok()?;
        let hinstance = HINSTANCE(hmodule.0);
        let class_name = w!("win_night_shift_hidden_window");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("win-night-shift"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )
        .ok()
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            if wparam.0 == TIMER_FLYOUT_ANIM {
                // 周期 timer，动画播完由 flyout 自行 KillTimer。
                flyout::on_anim_tick();
            }
            LRESULT(0)
        }
        // 被监听的注册表键变了（系统设置等外部改动）：可见时同步浮窗控件。
        m if m == watch::WM_SETTINGS_CHANGED => {
            flyout::on_external_change();
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
