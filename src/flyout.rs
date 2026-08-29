//! Win11 风格托盘浮窗：用 WinUI 3（XAML Islands）绘制，设置页直接做进浮窗。
//!
//! 走的是「调用系统原生 UI」而非手工仿造：控件、亚克力、圆角、动画全部由
//! WindowsAppRuntime 框架包提供，本进程只增约 150KB，且不引入 .NET。
//!
//! 运行时不存在时（未装框架包的旧系统）`init` 返回 false，调用方直接退出进程，
//! 不做回退（见 main.rs）。
//!
//! # 构建顺序不可调换
//! `Create → XamlSource::Initialize → SetContent → SetSystemBackdrop
//!  → Show → 取 HWND → 窗口外观/失焦监听`
//!
//! 两处顺序踩过坑：
//!  * backdrop 必须在 `SetContent` 之后——内容树为空时挂 backdrop 会**静默段错误**；
//!  * `GetWindowFromWindowId` 必须在 `Show` 之后——窗口未显示时 HWND 尚未实体化，
//!    返回 `E_POINTER`。

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};
use windows::core::{h, IInspectable, Interface, Ref, Result, BOOL, HSTRING};
use windows::Foundation::{PropertyValue, TypedEventHandler};
use windows::Graphics::{RectInt32, SizeInt32};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_DEFAULT, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClientRect, PostMessageW, SetForegroundWindow, SetWindowPos, SystemParametersInfoW, SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOZORDER,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WA_INACTIVE, WM_ACTIVATE, WM_USER,
};
use winui3::bootstrap::PackageDependency;
use winui3::Microsoft::UI::Dispatching::DispatcherQueueController;
use winui3::Microsoft::UI::Windowing::{AppWindow, OverlappedPresenter};
use winui3::Microsoft::UI::Xaml::Controls::Primitives::{
    RangeBaseValueChangedEventArgs, RangeBaseValueChangedEventHandler, ToggleButton,
};
use winui3::Microsoft::UI::Xaml::Controls::{
    AppBarButton, Border, ColumnDefinition, CommandBarLabelPosition, FontIcon, Grid, Orientation,
    Slider, StackPanel, TextBlock, XamlControlsResources,
};
use winui3::Microsoft::UI::Xaml::Hosting::{DesktopWindowXamlSource, WindowsXamlManager};
use winui3::Microsoft::UI::Xaml::Input::PointerEventHandler;
use winui3::Microsoft::UI::Xaml::Media::{Brush, DesktopAcrylicBackdrop};
use winui3::Microsoft::UI::Xaml::{
    Application, CornerRadius, ElementTheme, FrameworkElement, GridLength, GridUnitType,
    HorizontalAlignment, LaunchActivatedEventArgs, ResourceDictionary, RoutedEventHandler,
    TextAlignment, Thickness, VerticalAlignment,
};
use winui3::{XamlApp, XamlAppOverrides};

/// 面板逻辑尺寸（96dpi 基准），高度按下列常量累加得出。
///
/// 布局照快速设置面板：顶部三枚瓦片 → 分隔线 → 拉条 → 分隔线 → 底栏，
/// 全部直接坐在亚克力上，无卡片。开机自启不在面板里，挪到了托盘右键菜单。
///
/// 不做运行时测量：`DesktopWindowXamlSource` 的内容树量不出可用的高度
/// （手动 `Measure` 早于模板套用、`ActualHeight` 是布局后的值、`DesiredSize` 返回 0），
/// 而各部分的尺寸本就来自 WinUI 主题资源里的固定值，直接累加即可。
/// 改布局时同步改这里。
const PANEL_W: i32 = 320;

/// 瓦片按钮高，与快速设置的瓦片同形。
const TILE_H: i32 = 48;
/// 瓦片与其下方文字标签的间距。
const TILE_LABEL_GAP: i32 = 8;
/// 瓦片下方文字标签行高（FontSize 12）。
const LABEL_H: i32 = 20;
/// 拉条行高：控件的 `MinHeight`（32）上下各留 4。
const ROW_H: i32 = 40;
/// 分隔线高 1px，与相邻区块的间距。
const SEP_H: i32 = 1;
const SEP_GAP: i32 = 12;
/// 面板横向内边距（瓦片区/拉条共用），快速设置取 16 一档。
const PAD_H: i32 = 16;
/// 顶部内边距。
const TOP_PAD: i32 = 16;
/// 底栏高度：`AppBarThemeCompactHeight`。
const FOOTER_H: i32 = 48;

/// 面板高度 = 顶边距 + 瓦片区 + 分隔线 + 拉条行 + 分隔线 + 底栏。
/// 底栏前那条分隔线只有上间距（底栏自带留白）。
const PANEL_H: i32 = TOP_PAD
    + (TILE_H + TILE_LABEL_GAP + LABEL_H)
    + (SEP_GAP + SEP_H + SEP_GAP)
    + ROW_H
    + (SEP_GAP + SEP_H)
    + FOOTER_H;
/// 面板与托盘图标之间的间距（逻辑像素）。
const GAP: i32 = 8;

/// 面板中的设置控件。
struct Items {
    night: ToggleButton,
    strength: Slider,
    /// 拉条右侧的色温值文本，随拉条实时更新。
    kelvin: TextBlock,
    dark: ToggleButton,
    /// 原彩（True Tone）：仅预留占位，禁用态，无逻辑。
    truetone: ToggleButton,
}

/// 色温值文本：如「6500K」。
fn kelvin_text(p: u32) -> String {
    format!("{}K", crate::nightlight::strength_to_kelvin(p))
}

impl Items {
    /// 从系统现读状态回写所有控件。
    ///
    /// 只能在控件已进可视化树（`SetContent` 之后）调用：程序化写入需在模板套用后进行；
    /// 写入期间用 `with_syncing` 压住事件回调，否则 SetIsOn/SetValue2 触发的
    /// Toggled/ValueChanged 会把刚读出来的值又写回注册表（无害但多写一次）。
    fn sync(&self) {
        // 读不到（blob 缺失/格式不符）时给中性默认值。
        let night = crate::nightlight::get_enabled().unwrap_or(false);
        let strength = crate::nightlight::get_strength().unwrap_or(50);
        with_syncing(|| {
            let _ = set_checked(&self.night, night);
            let _ = self.strength.SetValue2(f64::from(strength));
            let _ = self.kelvin.SetText(&HSTRING::from(kelvin_text(strength)));
            let _ = set_checked(&self.dark, crate::theme::is_dark());
        });
    }
}

/// 程序化设置瓦片选中态：`IReference<bool>` 装箱。
fn set_checked(btn: &ToggleButton, v: bool) -> Result<()> {
    let rv: windows::Foundation::IReference<bool> = PropertyValue::CreateBoolean(v)?.cast()?;
    btn.SetIsChecked(Some(&rv))
}

struct Flyout {
    // 以下三个句柄必须保活到进程结束：drop 会卸载运行时/XAML 上下文。
    _dep: PackageDependency,
    _dqc: DispatcherQueueController,
    _mgr: WindowsXamlManager,
    win: AppWindow,
    _src: DesktopWindowXamlSource,
    hwnd: HWND,
    items: Items,
    /// 显式设了主题画刷的元素集合，主题切换时重刷（见 apply_theme_brushes）。
    themed: Themed,
    visible: bool,
    /// 上次收起的时刻，用于识别「点托盘收起」这一手势，见 `toggle_at`。
    hidden_at: Option<std::time::Instant>,
}

thread_local! {
    static FLYOUT: RefCell<Option<Flyout>> = const { RefCell::new(None) };
    /// 程序化写控件期间置位，控件事件回调据此跳过（防写回与双向同步回环）。
    static SYNCING: Cell<bool> = const { Cell::new(false) };
    /// 强度滑条写注册表的节流状态：上次写入时刻与待补写的值。
    static LAST_WRITE: Cell<Instant> = Cell::new(Instant::now());
    static PENDING_STRENGTH: Cell<Option<u32>> = const { Cell::new(None) };
    /// 调试钉住：置位时失焦不收起（仅 debug 构建，见 init）。
    static PINNED: Cell<bool> = const { Cell::new(false) };
}

/// 强度写注册表的最小间隔（毫秒）。拖动期间的密集写入会被 CloudStore
/// 判定为冲突（实测把夜间模式打回关闭），必须限速。
const WRITE_THROTTLE: Duration = Duration::from_millis(300);

fn is_syncing() -> bool {
    SYNCING.with(|c| c.get())
}

fn with_syncing(f: impl FnOnce()) {
    SYNCING.with(|c| c.set(true));
    f();
    SYNCING.with(|c| c.set(false));
}

/// 启动时调用一次：引导运行时并预建面板。返回 false 表示 WindowsAppRuntime
/// 未就位，调用方直接退出进程（不做回退）。
///
/// 预建而非按需建，是为了把约 40ms 的首帧开销挪到启动阶段，
/// 让点击托盘时只剩 `Show`（约 10ms，无感）。
pub fn init() -> bool {
    // 调试钩子（与 main.rs 的 WNS_DEBUG_FLYOUT 联动）：自动弹出时一并钉住，
    // 不因失焦收起，方便截图/自动化校对布局。仅 debug 构建生效。
    #[cfg(debug_assertions)]
    if std::env::var_os("WNS_DEBUG_FLYOUT").is_some() {
        PINNED.with(|c| c.set(true));
    }
    match build() {
        Ok(f) => {
            FLYOUT.with(|c| *c.borrow_mut() = Some(f));
            true
        }
        Err(_) => false,
    }
}

/// 托盘被点击：面板已显示则隐藏，否则移到托盘图标上方并显示。
///
/// `tray_rect` 为 `Shell_NotifyIconGetRect` 返回的**物理像素**矩形。
pub fn toggle_at(tray_rect: tray_icon::Rect) {
    /// 「刚刚收起」的判定窗口。
    ///
    /// 面板显示时点托盘，任务栏会先抢走焦点，`WM_ACTIVATE` 抢在托盘事件之前
    /// 把面板收起；等托盘事件到达，`visible` 已是 false，单看它会把这一下
    /// 判成「打开」，于是面板闪一下又弹回来。落在这个窗口内的托盘点击
    /// 视为那次收起的后续，不再打开。
    ///
    /// 取 300ms：足够覆盖失焦到托盘事件之间的间隔（实测在几毫秒量级，
    /// 但要留出系统繁忙时的余量），又短于人有意「关掉再打开」的最快节奏。
    const DISMISS_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

    FLYOUT.with(|c| {
        let mut borrow = c.borrow_mut();
        let Some(f) = borrow.as_mut() else { return };
        if f.visible {
            f.hide();
            return;
        }
        if f.hidden_at.is_some_and(|t| t.elapsed() < DISMISS_GRACE) {
            return; // 这一下点击就是刚才那次收起的起因。
        }
        f.show_at(tray_rect);
    });
}

/// 隐藏面板（失焦时调用）。
pub fn hide() {
    FLYOUT.with(|c| {
        if let Some(f) = c.borrow_mut().as_mut() {
            f.hide();
        }
    });
}

/// 从系统现读状态同步全部控件（写入失败扳回控件时调用）。
pub fn refresh() {
    FLYOUT.with(|c| {
        if let Some(f) = c.borrow().as_ref() {
            f.items.sync();
        }
    });
}

/// 外部改动（watch 线程投来的注册表变更通知）：面板可见时现读系统状态回写控件。
///
/// 两个保护：
///  * 面板隐藏时直接返回——下次打开时 show_at 会 sync，无需现在做；
///  * 滑条正被按住（PointerCaptures 非空）时不同步——此时注册表里可能还是
///    节流前的旧值，回写会把滑条从用户手下拽走。松手后补写最终值引发的
///    回声通知读到的就是当前值，sync 自然是无操作。
pub fn on_external_change() {
    FLYOUT.with(|c| {
        let borrow = c.borrow();
        let Some(f) = borrow.as_ref() else { return };
        if !f.visible || slider_captured(&f.items.strength) {
            return;
        }
        f.items.sync();
    });
}

/// 滑条是否正被指针按住（拖动中）。
fn slider_captured(slider: &Slider) -> bool {
    slider
        .PointerCaptures()
        .and_then(|v| v.Size())
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// 展开/收起动画帧驱动（hidden 窗口 WM_TIMER 周期调用，见 main.rs）。
///
/// 当前面板高度固定、无展开态，timer 不会被启动；保留此入口与 lock-ime
/// 的消息循环结构对齐，后续加展开区时由它逐帧驱动窗口尺寸动画。
pub fn on_anim_tick() {}

impl Flyout {
    fn hide(&mut self) {
        let _ = self.win.Hide();
        self.visible = false;
        self.hidden_at = Some(std::time::Instant::now());
    }

    /// 依托盘图标位置定位并显示。坐标全程用物理像素，与 `tray_rect` 一致。
    fn show_at(&mut self, tray_rect: tray_icon::Rect) {
        // 定好位再显示，避免弹出瞬间先在上一次的旧位置闪一下。
        self.place(&tray_rect);
        // 每次打开都从系统现读状态刷新控件：开关可能被系统设置等外部途径改动。
        self.items.sync();

        if self.win.ShowWithActivation(true).is_err() {
            return;
        }
        self.visible = true;

        // ShowWithActivation 抢不过任务栏的前台锁定：托盘点击后前台仍是
        // explorer，面板始终非激活，失焦判定会立刻把它收起。必须显式夺取。
        unsafe {
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    /// 把窗口放到托盘图标上方：宽不变，右缘对齐托盘图标，底边贴在图标上方。
    ///
    /// 位置与尺寸用一次 `MoveAndResize` 原子完成——拆成 ResizeClient + Move 两步时，
    /// DWM 可能在两步之间合成出「尺寸已变、位置未变」的中间帧。
    fn place(&self, tray_rect: &tray_icon::Rect) {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as i32;
        let s = |v: i32| v * dpi / 96;
        let (pw, ph) = (s(PANEL_W), s(PANEL_H));

        let icon_right = tray_rect.position.x as i32 + tray_rect.size.width as i32;
        let mut x = icon_right - pw;
        let mut y = tray_rect.position.y as i32 - ph - s(GAP);

        // 钳制到工作区，避免被任务栏遮挡或跑出屏幕（任务栏在侧边/顶部时同样成立）。
        if let Some(wa) = work_area() {
            x = x.clamp(wa.left, (wa.right - pw).max(wa.left));
            y = y.clamp(wa.top, (wa.bottom - ph).max(wa.top));
        }
        // 本窗口无标题栏/边框（BorderAndTitleBar 已关），外框即客户区，
        // MoveAndResize 的尺寸语义差异在此不成立。
        let _ = self.win.MoveAndResize(RectInt32 {
            X: x,
            Y: y,
            Width: pw,
            Height: ph,
        });
        // 跨 DPI 显示器移动时尺寸会变，岛子窗口不自动跟随，需手动铺满。
        unsafe { position_island(self.hwnd) }
    }
}

/// 仅用于建立带样式 provider 的 Application 上下文；窗口由本模块自己建，
/// 故 OnLaunched 无需做任何事（也不会调用 Application::Start）。
struct NullApp;

impl XamlAppOverrides for NullApp {
    fn OnLaunched(
        &self,
        _base: &Application,
        _args: Option<&LaunchActivatedEventArgs>,
    ) -> Result<()> {
        Ok(())
    }
}

/// 子类化标识。同一窗口可挂多个子类，靠这个 id 区分。
const SUBCLASS_ID: usize = 1;

/// 自定义消息：收起浮窗。
///
/// 用于把 `Hide` 挪出 `WM_ACTIVATE` 的处理过程——在窗口过程内部同步调
/// `AppWindow::Hide` 会重入窗口管理逻辑。
const WM_FLYOUT_DISMISS: u32 = WM_USER + 1;

/// 浮窗窗口过程的子类化钩子：失焦即收起。
///
/// `WM_ACTIVATE` 的 wParam 低位为 `WA_INACTIVE` 表示本窗口正在失去激活。
/// 这是 Win32 层面的判据，不经 WinUI 的激活树，因而不受
/// 「Hide 之后再也回不到 Activated」那个行为的影响。
///
/// 收起动作经 `WM_FLYOUT_DISMISS` 绕一手，原因见该常量。
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_ACTIVATE
        && (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE
        && !PINNED.with(|c| c.get())
    {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_FLYOUT_DISMISS, WPARAM(0), LPARAM(0));
        }
    }
    if msg == WM_FLYOUT_DISMISS {
        hide();
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// 给浮窗加上 Win11 的圆角与边框。
///
/// `OverlappedPresenter::CreateForContextMenu` 只提供行为，视觉外框要自己向 DWM 要。
/// 这是官方文档 apply-rounded-corners 的 Example 4「Rounding the corners of a menu」
/// 所用的方案：
///  * `DWMWCP_ROUND` —— 标准圆角（8px），与系统输入法面板、右键菜单等浮窗一致。
///    半径由 DWM 内部规定、不可指定数值：ROUND=8、ROUNDSMALL=4，只能二选一，
///    这也是系统所有窗口圆角能保持统一的原因；
///  * `DWMWA_BORDER_COLOR` = `DWMWA_COLOR_DEFAULT` —— 交还系统绘制边框，
///    亮暗主题下自动取对应颜色。
///
/// 必须在 HWND 实体化之后调用。失败不致命（旧系统退化为直角无边框）。
fn apply_window_chrome(hwnd: HWND) {
    unsafe {
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const _,
            std::mem::size_of_val(&pref) as u32,
        );
        let color = DWMWA_COLOR_DEFAULT;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &color as *const _ as *const _,
            std::mem::size_of_val(&color) as u32,
        );
    }
}

/// 主显示器工作区（已扣除任务栏），物理像素。
fn work_area() -> Option<RECT> {
    let mut rc = RECT::default();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rc as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_ok().then_some(rc)
}

/// 摆放 XAML 岛的输入站点子窗口：铺满父窗口客户区。
///
/// `DesktopWindowXamlSource` 的内容宿主在一个子窗口（InputSiteWindowClass）里，
/// 它只在首次显示时取父窗口客户区尺寸；之后窗口尺寸变化它也不跟随，
/// 需要手动摆放。正常情况只有一个子窗口；重复调用无害。
unsafe fn position_island(hwnd: HWND) {
    let mut rc = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rc) }.is_err() {
        return;
    }
    unsafe {
        let _ = EnumChildWindows(
            Some(hwnd),
            Some(enum_child_layout),
            LPARAM(&rc as *const RECT as isize),
        );
    }
}

unsafe extern "system" fn enum_child_layout(child: HWND, lparam: LPARAM) -> BOOL {
    let rc = unsafe { &*(lparam.0 as *const RECT) };
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    unsafe {
        let _ = SetWindowPos(child, None, 0, 0, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
    BOOL(1)
}

/// 显式设了主题画刷的元素集合，供主题切换时重刷（见 apply_theme_brushes）。
struct Themed {
    /// 三条分隔线：底色即线色。
    separators: Vec<Border>,
    /// 主题画刷的来源字典：挂进 Application 的那份 XamlControlsResources。
    /// 激活失败时为 None，set_brush 回退到顶层 Lookup。
    dict: Option<ResourceDictionary>,
}

/// 按当前实际主题取主题画刷并应用。
///
/// 优先在 XamlControlsResources 的 ThemeDictionaries 里按 `theme` 精确取
/// （"Dark"/"Light" 字典）：直接对顶层资源字典 Lookup 会走 "Default" 主题字典，
/// 拿到的是解析那一刻的固化值，不随主题切换。取不到（旧运行时键缺失、
/// XCR 未挂上）时回退顶层 Lookup；仍查不到时静默跳过、不算失败——
/// 控件退化为无背景/无描边仍可用，为一个配色让整个面板建不起来不划算。
fn set_brush<F>(
    dict: Option<&ResourceDictionary>,
    theme: ElementTheme,
    key: &str,
    apply: F,
) -> Result<()>
where
    F: FnOnce(&Brush) -> Result<()>,
{
    let boxed = PropertyValue::CreateString(&HSTRING::from(key))?;
    let brush = (|| -> Result<Brush> {
        if let Some(d) = dict {
            let which = if theme == ElementTheme::Dark {
                "Dark"
            } else {
                "Light"
            };
            let k = PropertyValue::CreateString(&HSTRING::from(which))?;
            if let Ok(td) = d
                .ThemeDictionaries()
                .and_then(|m| m.Lookup(&k))
                .and_then(|v| v.cast::<ResourceDictionary>())
            {
                if let Ok(b) = td.Lookup(&boxed).and_then(|v| v.cast::<Brush>()) {
                    return Ok(b);
                }
            }
        }
        Application::Current()?
            .Resources()?
            .Lookup(&boxed)?
            .cast::<Brush>()
    })();
    if let Ok(b) = brush {
        apply(&b)?;
    }
    Ok(())
}

/// 显式画刷的统一应用点：分隔线颜色。
///
/// 分隔线键取自 WinUI 主题资源 `DividerStrokeColorDefaultBrush`
/// （MenuFlyoutSeparator 等系统分隔线同源）。
///
/// 必须集中在这一处、且能被反复调用：控件模板里的 ThemeResource 在系统主题
/// 切换时会自动重解析，而代码里 SetBackground 上去的画刷不会——它固化着
/// 取出那一刻的主题色，曾导致运行期间切换亮/暗后「模板部分（文字/开关）
/// 已跟随、显式画刷仍是旧主题色」的混搭。构建时应用一次，之后由 root 的
/// `ActualThemeChanged` 事件回调按新主题重刷（见 build）。
fn apply_theme_brushes(t: &Themed, theme: ElementTheme) {
    let dict = t.dict.as_ref();
    for sep in &t.separators {
        let _ = set_brush(dict, theme, "DividerStrokeColorDefaultBrush", |b| {
            sep.SetBackground(b)
        });
    }
}

/// 快速设置式瓦片：48 高的 ToggleButton + 下方居中文字标签。
///
/// 瓦片即 `ToggleButton` 默认模板——它的 Checked 态背景就是
/// `AccentFillColorDefaultBrush`（强调色底+反白前景），正是快速设置里
/// 「已开启」瓦片的样子，不需要自绘。圆角用 `ControlCornerRadius`（4）小圆角，
/// 与按钮/输入框等控件级圆角一致。
///
/// 图标归一化：FontIcon 默认字号 20（`icon.cpp` 的 `g_ClientCoreFontSize`），
/// 系统瓦片图标是 16，这里显式压到 16。
fn make_tile(btn: &ToggleButton, label: &str) -> Result<StackPanel> {
    btn.SetHeight(f64::from(TILE_H))?;
    btn.SetHorizontalAlignment(HorizontalAlignment::Stretch)?;
    btn.SetCornerRadius(CornerRadius {
        TopLeft: 4.0,
        TopRight: 4.0,
        BottomRight: 4.0,
        BottomLeft: 4.0,
    })?;

    let tb = TextBlock::new()?;
    tb.SetText(&HSTRING::from(label))?;
    tb.SetFontSize(12.0)?;
    tb.SetHorizontalAlignment(HorizontalAlignment::Center)?;
    tb.SetMargin(Thickness {
        Left: 0.0,
        Top: f64::from(TILE_LABEL_GAP),
        Right: 0.0,
        Bottom: 0.0,
    })?;

    let tile = StackPanel::new()?;
    tile.Children()?.Append(btn)?;
    tile.Children()?.Append(&tb)?;
    Ok(tile)
}

/// 瓦片按钮本体：图标为唯一内容。
fn make_tile_button(glyph: &str) -> Result<ToggleButton> {
    let btn = ToggleButton::new()?;
    let icon = FontIcon::new()?;
    icon.SetGlyph(&HSTRING::from(glyph))?;
    icon.SetFontSize(16.0)?;
    btn.SetContent(&icon.cast::<IInspectable>()?)?;
    Ok(btn)
}

/// 顶部瓦片区：深色模式 / 夜间模式 / 原彩（预留），三列等宽、列间距 8。
fn make_tiles(items: &Items) -> Result<Grid> {
    let grid = Grid::new()?;
    grid.SetColumnSpacing(8.0)?;
    grid.SetMargin(Thickness {
        Left: f64::from(PAD_H),
        Top: f64::from(TOP_PAD),
        Right: f64::from(PAD_H),
        Bottom: 0.0,
    })?;
    for _ in 0..3 {
        let col = ColumnDefinition::new()?;
        col.SetWidth(GridLength {
            Value: 1.0,
            GridUnitType: GridUnitType::Star,
        })?;
        grid.ColumnDefinitions()?.Append(&col)?;
    }
    let defs = [
        (&items.dark, "深色模式"),
        (&items.night, "夜间模式"),
        (&items.truetone, "原彩"),
    ];
    for (i, (btn, label)) in defs.iter().enumerate() {
        let tile = make_tile(btn, label)?;
        Grid::SetColumn(&tile, i as i32)?;
        grid.Children()?.Append(&tile)?;
    }
    Ok(grid)
}

/// 通栏分隔线：1px 高、通栏拉伸；线色由 apply_theme_brushes 按主题应用。
/// 快速设置的分隔线贴边贯通，因此不随内容区内边距缩进。
fn make_separator(top: f64, bottom: f64) -> Result<Border> {
    let sep = Border::new()?;
    sep.SetHeight(f64::from(SEP_H))?;
    sep.SetHorizontalAlignment(HorizontalAlignment::Stretch)?;
    sep.SetMargin(Thickness {
        Left: 0.0,
        Top: top,
        Right: 0.0,
        Bottom: bottom,
    })?;
    Ok(sep)
}

/// 卡片左侧的单行标签。
fn make_label(text: &str) -> Result<TextBlock> {
    let tb = TextBlock::new()?;
    tb.SetText(&HSTRING::from(text))?;
    tb.SetVerticalAlignment(VerticalAlignment::Center)?;
    Ok(tb)
}

/// 底栏：右对齐的设置图标按钮（预留占位，暂无设置页，不绑动作）。
///
/// 不设背景、不设边框——背景即浮窗基底（亚克力本身），与上方区块的分隔
/// 由独立的分隔线元素承担（见 populate_root）。
///
/// 左右内边距与内容区取同一个值：底栏并非通栏无边距，
/// 少了它，悬停底板会贴到浮窗边缘。上下不留，由 `FOOTER_H` 给高度即可。
fn make_footer() -> Result<Border> {
    let bar = StackPanel::new()?;
    bar.SetOrientation(Orientation::Horizontal)?;
    bar.SetHorizontalAlignment(HorizontalAlignment::Right)?;
    bar.SetVerticalAlignment(VerticalAlignment::Center)?;

    // U+E713 Setting（齿轮），取自 Segoe Fluent Icons，与系统各处设置入口同款字形。
    let settings = make_command_button("\u{E713}", "设置")?;
    bar.Children()?.Append(&settings)?;

    let footer = Border::new()?;
    footer.SetMinHeight(f64::from(FOOTER_H))?;
    let pad = f64::from(PAD_H);
    footer.SetPadding(Thickness {
        Left: pad,
        Top: 0.0,
        Right: pad,
        Bottom: 0.0,
    })?;
    footer.SetChild(&bar)?;
    Ok(footer)
}

/// 底栏图标按钮。用 `AppBarButton` 而非自绘 Button，是因为命令栏这一档的
/// 尺寸、悬停/按下反馈、图标字号在 Win11 里由一组主题资源统一规定
/// （`AppBarThemeMinHeight` = 48、图标 16pt、`SymbolThemeFontFamily`），
/// AppBarButton 的默认模板正是这些资源的消费者——系统各处底栏之所以高度一致，
/// 靠的就是它，自己写死数值必然对不上。
///
/// `LabelPosition = Collapsed` 收起文字标签只留图标，与设置浮窗、快速设置面板
/// 底部那排图标同形；文字退化为悬停提示，语义不丢。
///
/// **图标字号不要覆盖**。模板把图标套在 `Height=16` 的 Viewbox 里
/// （`AppBarButton_themeresources.xaml:366`，高度取 `AppBarButtonContentHeight`），
/// Viewbox 是按**整个文本框**等比缩放到 16，不是按字号。`FontIcon` 默认字号 20
/// （`icon.cpp` 的 `g_ClientCoreFontSize`），由 Viewbox 压到 16 —— 归一化已经做完了。
/// 手动设成 16 只会让自然文本框变小、被 Viewbox 反向放大，图标显著偏大。
///
/// 唯一覆盖的是宽度：模板默认 68 是为文字标签预留的，标签收起后只剩 16px 图标，
/// 68 宽会让两个图标间距远大于系统底栏。取 40 是因为模板的悬停底板
/// （`AppBarButtonInnerBorder`）边距为 `2,6,2,6`：40 宽 × 48 高（
/// `AppBarThemeCompactHeight`）扣掉后正好是 **36×36 的正方形**悬停区，
/// 与系统底栏图标按钮同形。换任何别的宽度，悬停区都会变成长方形。
fn make_command_button(glyph: &str, label: &str) -> Result<AppBarButton> {
    let b = AppBarButton::new()?;
    let icon = FontIcon::new()?;
    icon.SetGlyph(&HSTRING::from(glyph))?;
    b.SetIcon(&icon)?;
    b.SetLabel(&HSTRING::from(label))?;
    b.SetLabelPosition(CommandBarLabelPosition::Collapsed)?;
    b.SetWidth(40.0)?;
    Ok(b)
}

/// 把各区块装进根面板：瓦片区 → 分隔线 → 强度拉条 → 分隔线 → 底栏。
/// 全部直接坐在亚克力基底上，不用卡片。开机自启不在此处，见托盘右键菜单（tray.rs）。
/// `separators` 收集显式设主题画刷的分隔线，供 apply_theme_brushes 重刷。
fn populate_root(root: &StackPanel, items: &Items, separators: &mut Vec<Border>) -> Result<()> {
    let tiles = make_tiles(items)?;
    root.Children()?.Append(&tiles)?;

    let sep1 = make_separator(f64::from(SEP_GAP), f64::from(SEP_GAP))?;
    root.Children()?.Append(&sep1)?;
    separators.push(sep1);

    // 强度拉条行：左「色温」标签 + 拉条拉伸 + 右侧实时色温值。
    let pad = f64::from(PAD_H);
    let row = Grid::new()?;
    row.SetMinHeight(f64::from(ROW_H))?;
    row.SetMargin(Thickness {
        Left: pad,
        Top: 0.0,
        Right: pad,
        Bottom: 0.0,
    })?;
    for t in [GridUnitType::Auto, GridUnitType::Star, GridUnitType::Auto] {
        let col = ColumnDefinition::new()?;
        col.SetWidth(GridLength {
            Value: 1.0,
            GridUnitType: t,
        })?;
        row.ColumnDefinitions()?.Append(&col)?;
    }

    let caption = make_label("色温")?;
    Grid::SetColumn(&caption, 0)?;
    row.Children()?.Append(&caption)?;

    items
        .strength
        .SetHorizontalAlignment(HorizontalAlignment::Stretch)?;
    items.strength.SetVerticalAlignment(VerticalAlignment::Center)?;
    items.strength.SetMargin(Thickness {
        Left: 8.0,
        Top: 0.0,
        Right: 8.0,
        Bottom: 0.0,
    })?;
    Grid::SetColumn(&items.strength, 1)?;
    row.Children()?.Append(&items.strength)?;

    // 值右对齐：1200K..6500K 恒为 5 字符，给足定宽避免数字跳动时整行抖动。
    items.kelvin.SetMinWidth(44.0)?;
    items.kelvin.SetTextAlignment(TextAlignment::Right)?;
    items
        .kelvin
        .SetVerticalAlignment(VerticalAlignment::Center)?;
    Grid::SetColumn(&items.kelvin, 2)?;
    row.Children()?.Append(&items.kelvin)?;

    root.Children()?.Append(&row)?;

    // 底栏前的分隔线只留上间距：底栏自身高度已含留白。
    let sep2 = make_separator(f64::from(SEP_GAP), 0.0)?;
    root.Children()?.Append(&sep2)?;
    separators.push(sep2);

    root.Children()?.Append(&make_footer()?)?;
    Ok(())
}

fn build() -> Result<Flyout> {
    winui3::init_apartment(winui3::ApartmentType::SingleThreaded)?;
    // 挂载系统已装的 WindowsAppRuntime 框架包；缺失时在此返回 Err。
    let dep = PackageDependency::initialize()?;
    let dqc = DispatcherQueueController::CreateOnCurrentThread()?;
    let dq = dqc.DispatcherQueue()?;
    // compose 必须在 WindowsXamlManager 之前：它内部创建的
    // XamlControlsXamlMetaDataProvider 才是 WinUI 控件样式（圆角、Fluent 外观）的来源。
    // 反过来先 Initialize，manager 会自建一个不带 provider 的 Application，
    // 控件就退化成无主题的方角样式。
    XamlApp::compose(NullApp)?;
    let mgr = WindowsXamlManager::InitializeForCurrentThread()?;

    // CreateForContextMenu：WinUI 专为上下文菜单/浮窗提供的 presenter。
    // 它只管行为（无标题栏、不进 Alt+Tab、不抢激活），**不负责视觉外框**——
    // 圆角与边框归 DWM 管，见下方 Show 之后的 apply_window_chrome。
    let presenter = OverlappedPresenter::CreateForContextMenu()?;
    let win = AppWindow::CreateWithPresenter(&presenter)?;
    win.AssociateWithDispatcherQueue(&dq)?;
    win.SetTitle(h!("win-night-shift"))?;
    win.SetIsShownInSwitchers(false)?; // 不进 Alt+Tab / 任务栏
    // 有意覆盖 CreateForContextMenu 的预设（该方法文档的配置表里此项为 false）：
    // 托盘浮窗要压在任务栏之上，不置顶会被任务栏盖住。
    presenter.SetIsAlwaysOnTop(true)?;
    let _ = presenter.SetBorderAndTitleBar(false, false);
    win.ResizeClient(SizeInt32 {
        Width: PANEL_W,
        Height: PANEL_H,
    })?;

    let src = DesktopWindowXamlSource::new()?;
    src.Initialize(win.Id()?)?;

    let root = StackPanel::new()?;

    // 必须在任何 make_tile / set_brush 之前：控件模板与主题画刷都在这份字典里。
    // 合并到 Application 级而非 root；画刷另从这份字典的 ThemeDictionaries 按
    // 实际主题精确取（见 set_brush），故保留字典本身一份引用。
    // 前提是 XamlApp::compose 已在 WindowsXamlManager 之前建立带元数据 provider
    // 的 Application，否则此处激活失败返回 E_FAIL。
    let dict = match (XamlControlsResources::new(), Application::Current()) {
        (Ok(res), Ok(app)) => {
            app.Resources()?.MergedDictionaries()?.Append(&res)?;
            Some(res.cast::<ResourceDictionary>()?)
        }
        _ => None,
    };

    // 各控件的状态先留默认，与值一并在 SetContent 之后由 sync 落实（原因见 Items::sync）。
    //
    // 瓦片图标取自 Segoe Fluent Icons，与系统对应入口同款：
    //  * 深色模式 U+E790 Color —— 「个性化 › 颜色」页（深色模式设置所在地）的图标；
    //  * 夜间模式 U+E708 QuietHours（月亮）—— 快速设置「夜间模式」瓦片的图标；
    //  * 原彩 U+E706 Brightness —— 随环境光调节，取亮度图标；仅预留，禁用。
    let strength = Slider::new()?;
    strength.SetMinimum(0.0)?;
    strength.SetMaximum(100.0)?;
    strength.SetStepFrequency(1.0)?;

    let truetone = make_tile_button("\u{E706}")?;
    truetone.SetIsEnabled(false)?;

    let items = Items {
        night: make_tile_button("\u{E708}")?,
        strength,
        kelvin: TextBlock::new()?,
        dark: make_tile_button("\u{E790}")?,
        truetone,
    };
    bind_controls(&items)?;

    let mut separators = Vec::new();
    populate_root(&root, &items, &mut separators)?;
    let themed = Themed { separators, dict };

    src.SetContent(&root)?;

    // 显式画刷按当前实际主题应用一次；此后由 ActualThemeChanged 跟随系统主题
    // 切换重刷（固化的画刷曾导致运行期间切换亮/暗后停在旧主题色，
    // 见 apply_theme_brushes）。
    let root_fe = root.cast::<FrameworkElement>()?;
    apply_theme_brushes(&themed, root_fe.ActualTheme().unwrap_or(ElementTheme::Light));
    root_fe.ActualThemeChanged(&TypedEventHandler::new(
        |fe: Ref<'_, FrameworkElement>, _: Ref<'_, IInspectable>| {
            if let Ok(theme) = fe.ok().and_then(|e| e.ActualTheme()) {
                FLYOUT.with(|c| {
                    if let Some(f) = c.borrow().as_ref() {
                        apply_theme_brushes(&f.themed, theme);
                    }
                });
            }
            Ok(())
        },
    ))?;

    // 初值必须等到内容树挂上宿主、模板套用之后再写，详见 Items::sync。
    items.sync();

    // 亚克力必须在 SetContent 之后；失败不致命（旧系统降级为纯色）。
    if let Ok(b) = DesktopAcrylicBackdrop::new() {
        let _ = src.SetSystemBackdrop(&b);
    }

    // Show 一次让 HWND 实体化，随后立即隐藏——首帧开销在启动时付掉。
    win.Show()?;
    let hwnd = unsafe { winui3::interop::GetWindowFromWindowId(win.Id()?)? };
    apply_window_chrome(hwnd);

    // 失焦即收起。走 Win32 的 WM_ACTIVATE 而非 WinUI 的 InputActivationListener：
    // 后者在本场景下不可用——窗口一旦 Hide 过，ShowWithActivation 就再也无法让它
    // 回到 Activated（实测每次 Show 后 State 恒为 Deactivated），
    // 于是失焦不再产生状态跳变，事件只在进程首次显示时触发过一次。
    // 与关闭方式无关，toggle 关闭同样如此。
    //
    // 子类化而非改窗口过程：AppWindow 的 HWND 由 WinUI 创建并持有自己的窗口过程，
    // SetWindowSubclass 是官方为这种「插一手别人的窗口」提供的接口，
    // 消息会先过我们、再交还原过程。
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0);
    }

    win.Hide()?;

    Ok(Flyout {
        _dep: dep,
        _dqc: dqc,
        _mgr: mgr,
        win,
        _src: src,
        hwnd,
        items,
        themed,
        visible: false,
        hidden_at: None,
    })
}

/// 把控件事件绑到注册表写入。所有回调先查 `is_syncing`：程序化写值（sync）不触发回写。
/// 写入失败（返回 false）时 refresh 一遍，把控件扳回真实状态。
fn bind_controls(items: &Items) -> Result<()> {
    bind_tile(&items.night, |v| {
        if !crate::nightlight::set_enabled(v) {
            refresh();
        }
    })?;
    bind_tile(&items.dark, |v| {
        if !crate::theme::set_dark(v) {
            refresh();
        }
    })?;

    let kelvin = items.kelvin.clone();
    items.strength.ValueChanged(&RangeBaseValueChangedEventHandler::new(
        move |_, args: Ref<'_, RangeBaseValueChangedEventArgs>| {
            if is_syncing() {
                return Ok(());
            }
            let p = args.ok()?.NewValue()?.round().clamp(0.0, 100.0) as u32;
            // 色温值实时更新，但注册表写做节流：拖动会高频触发 ValueChanged，
            // 对 CloudStore 的密集外部写入会被系统判定冲突（实测会把夜间模式
            // 打回关闭）。两次写至少间隔 WRITE_THROTTLE，期间的值记入 PENDING，
            // 松手（PointerCaptureLost）时补写最后一笔。
            let _ = kelvin.SetText(&HSTRING::from(kelvin_text(p)));
            let now = Instant::now();
            let due = LAST_WRITE.with(|c| {
                now.duration_since(c.get()).as_millis() >= WRITE_THROTTLE.as_millis()
            });
            if due {
                LAST_WRITE.with(|c| c.set(now));
                PENDING_STRENGTH.with(|c| c.set(None));
                crate::nightlight::set_strength(p);
            } else {
                PENDING_STRENGTH.with(|c| c.set(Some(p)));
            }
            Ok(())
        },
    ))?;
    items.strength.PointerCaptureLost(&PointerEventHandler::new(|_, _| {
        if let Some(p) = PENDING_STRENGTH.with(|c| c.take()) {
            LAST_WRITE.with(|c| c.set(Instant::now()));
            crate::nightlight::set_strength(p);
        }
        Ok(())
    }))?;
    Ok(())
}

/// 瓦片开关：Click 只在用户点按时触发（程序化 SetIsChecked 不触发），
/// 天然避开 sync 回环；is_syncing 判定仅作保险。
fn bind_tile<F: Fn(bool) + Send + 'static>(btn: &ToggleButton, f: F) -> Result<()> {
    let b = btn.clone();
    btn.Click(&RoutedEventHandler::new(move |_, _| {
        if !is_syncing() {
            f(b.IsChecked().ok().and_then(|r| r.Value().ok()).unwrap_or(false));
        }
        Ok(())
    }))?;
    Ok(())
}
