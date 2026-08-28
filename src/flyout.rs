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
use windows::core::{h, Interface, Ref, Result, BOOL, HSTRING};
use windows::Foundation::PropertyValue;
use windows::Graphics::{RectInt32, SizeInt32};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_DEFAULT, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClientRect, PostMessageW, PostQuitMessage, SetForegroundWindow,
    SetWindowPos, SystemParametersInfoW, SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOZORDER,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WA_INACTIVE, WM_ACTIVATE, WM_USER,
};
use winui3::bootstrap::PackageDependency;
use winui3::Microsoft::UI::Dispatching::DispatcherQueueController;
use winui3::Microsoft::UI::Windowing::{AppWindow, OverlappedPresenter};
use winui3::Microsoft::UI::Xaml::Controls::Primitives::{
    RangeBaseValueChangedEventArgs, RangeBaseValueChangedEventHandler,
};
use winui3::Microsoft::UI::Xaml::Controls::{
    AppBarButton, Border, ColumnDefinition, CommandBarLabelPosition, FontIcon, Grid, Orientation,
    Slider, StackPanel, TextBlock, ToggleSwitch, XamlControlsResources,
};
use winui3::Microsoft::UI::Xaml::Hosting::{DesktopWindowXamlSource, WindowsXamlManager};
use winui3::Microsoft::UI::Xaml::Input::PointerEventHandler;
use winui3::Microsoft::UI::Xaml::Media::{Brush, DesktopAcrylicBackdrop};
use winui3::Microsoft::UI::Xaml::{
    Application, CornerRadius, FrameworkElement, GridLength, GridUnitType, HorizontalAlignment,
    LaunchActivatedEventArgs, RoutedEventHandler, Thickness, UIElement, VerticalAlignment,
};
use winui3::{XamlApp, XamlAppOverrides};

/// 面板逻辑尺寸（96dpi 基准），高度按下列常量累加得出。
///
/// 不做运行时测量：`DesktopWindowXamlSource` 的内容树量不出可用的高度
/// （手动 `Measure` 早于模板套用、`ActualHeight` 是布局后的值、`DesiredSize` 返回 0），
/// 而各部分的尺寸本就来自 WinUI 主题资源里的固定值，直接累加即可。
/// 改布局时同步改这里。
const PANEL_W: i32 = 320;

/// 设置行高：开关/拉条的 `MinHeight`（32）上下各留 4。
const ROW_H: i32 = 40;
/// 卡片上下内边距各 10，见 make_card。
const CARD_PAD_V: i32 = 10;
/// 普通单行卡片总高（深色模式/开机自启）：行高 + 上下内边距 + 上下 1px 描边。
/// 描边在内边距外侧（Border 的 Padding 不含 BorderThickness），漏算它会让面板偏矮、
/// 最下面一张卡的底边描边被窗口下缘裁掉。
const CARD_H: i32 = CARD_PAD_V * 2 + ROW_H + 2;
/// 夜间模式卡片：开关行 + 拉条行，两行。
const NIGHT_CARD_H: i32 = CARD_PAD_V * 2 + ROW_H * 2 + 2;
/// 标题行：FontSize 14 的单行文本约 20，加下边距 2。
const TITLE_H: i32 = 22;
/// 标题与各卡片间距，见 make_content 里 panel 的 Spacing。
const CARD_GAP: i32 = 8;
/// 内容区上下内边距各 12，见 make_content 里 content 的 Padding。
const CONTENT_PAD_V: i32 = 12;
/// 底栏高度：`AppBarThemeCompactHeight`。
const FOOTER_H: i32 = 48;
/// 内容区底边分隔线 1px。
const SEP_H: i32 = 1;

/// 面板高度 = 内容区 + 分隔线 + 底栏。三张卡片：夜间模式双行卡 + 两张单行卡，
/// 标题加三卡共四个子元素、三个间距。
const PANEL_H: i32 =
    CONTENT_PAD_V * 2 + TITLE_H + CARD_GAP * 3 + NIGHT_CARD_H + CARD_H * 2 + SEP_H + FOOTER_H;
/// 面板与托盘图标之间的间距（逻辑像素）。
const GAP: i32 = 8;

/// 强度拉条定宽：系统设置里的拉条不随布局拉伸，固定宽度让各卡片右侧控件边缘对齐。
const SLIDER_W: f64 = 130.0;

/// 面板中的设置控件。
struct Items {
    night: ToggleSwitch,
    strength: Slider,
    strength_label: TextBlock,
    dark: ToggleSwitch,
    autostart: ToggleSwitch,
}

/// 强度标签文案：百分比 + 约值色温。
fn strength_text(p: u32) -> String {
    format!("强度 {p}%（约 {}K）", crate::nightlight::strength_to_kelvin(p))
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
            let _ = self.night.SetIsOn(night);
            let _ = self.strength.SetValue2(f64::from(strength));
            let _ = self.dark.SetIsOn(crate::theme::is_dark());
            let _ = self.autostart.SetIsOn(crate::autostart::is_autostart());
            let _ = self
                .strength_label
                .SetText(&HSTRING::from(strength_text(strength)));
        });
    }
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

/// 展开/收起动画帧驱动（hidden 窗口 WM_TIMER 周期调用，见 main.rs）。
///
/// 当前三张卡片高度固定、无展开态，timer 不会被启动；保留此入口与 lock-ime
/// 的消息循环结构对齐，后续给卡片加展开区时由它逐帧驱动窗口尺寸动画。
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
    if msg == WM_ACTIVATE && (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
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

/// 从 Application 资源字典取主题画刷并应用。
///
/// 这些键在亮/暗主题下自动解析成不同的值，配色随系统主题切换，无需自维护调色板。
/// 查不到时静默跳过、不算失败：控件退化为无背景/无描边仍可用，
/// 为一个配色让整个面板建不起来不划算。
fn set_brush<F>(key: &str, apply: F) -> Result<()>
where
    F: FnOnce(&Brush) -> Result<()>,
{
    let boxed = PropertyValue::CreateString(&HSTRING::from(key))?;
    if let Ok(b) = Application::Current()
        .and_then(|a| a.Resources())
        .and_then(|r| r.Lookup(&boxed))
        .and_then(|v| v.cast::<Brush>())
    {
        apply(&b)?;
    }
    Ok(())
}

/// Win11 设置页那种卡片：圆角 + 描边 + 主题背景。
///
/// 三个键都来自 WinUI 主题资源，与「系统 › 屏幕」里的卡片同源：
///  * `CardBackgroundFillColorDefaultBrush` —— 卡片底色
///  * `CardStrokeColorDefaultBrush` —— 1px 描边
///  * 圆角 8 —— 对应 `OverlayCornerRadius` 档位。设置页里的卡片、快速设置面板里的
///    分组块用的都是这一档；`ControlCornerRadius`（4）是按钮/输入框那种控件级圆角，
///    用在卡片上会明显偏小。
fn make_card() -> Result<Border> {
    let card = Border::new()?;
    set_brush("CardBackgroundFillColorDefaultBrush", |b| {
        card.SetBackground(b)
    })?;
    set_brush("CardStrokeColorDefaultBrush", |b| card.SetBorderBrush(b))?;
    card.SetBorderThickness(Thickness {
        Left: 1.0,
        Top: 1.0,
        Right: 1.0,
        Bottom: 1.0,
    })?;
    card.SetCornerRadius(CornerRadius {
        TopLeft: 8.0,
        TopRight: 8.0,
        BottomRight: 8.0,
        BottomLeft: 8.0,
    })?;
    card.SetPadding(Thickness {
        Left: 14.0,
        Top: f64::from(CARD_PAD_V),
        Right: 14.0,
        Bottom: f64::from(CARD_PAD_V),
    })?;
    Ok(card)
}

/// 卡片左侧的单行标签。
fn make_label(text: &str) -> Result<TextBlock> {
    let tb = TextBlock::new()?;
    tb.SetText(&HSTRING::from(text))?;
    tb.SetVerticalAlignment(VerticalAlignment::Center)?;
    Ok(tb)
}

/// 「左标签 + 右控件」的设置行：Star/Auto 两列，行高 `min_h`。
fn make_setting_row(label: &str, min_h: f64) -> Result<Grid> {
    let row = Grid::new()?;
    row.SetHorizontalAlignment(HorizontalAlignment::Stretch)?;
    row.SetMinHeight(min_h)?;
    for t in [GridUnitType::Star, GridUnitType::Auto] {
        let col = ColumnDefinition::new()?;
        col.SetWidth(GridLength {
            Value: 1.0,
            GridUnitType: t,
        })?;
        row.ColumnDefinitions()?.Append(&col)?;
    }
    let tb = make_label(label)?;
    Grid::SetColumn(&tb, 0)?;
    row.Children()?.Append(&tb)?;
    Ok(row)
}

/// 把控件放进设置行右列（垂直居中、靠右，防止列比控件宽时贴左）。
fn set_row_control(row: &Grid, ctl: &UIElement) -> Result<()> {
    let fe = ctl.cast::<FrameworkElement>()?;
    fe.SetVerticalAlignment(VerticalAlignment::Center)?;
    fe.SetHorizontalAlignment(HorizontalAlignment::Right)?;
    Grid::SetColumn(&fe, 1)?;
    row.Children()?.Append(&fe)?;
    Ok(())
}

/// 单开关卡片（深色模式 / 开机自启）：左标签 + 右开关。
fn make_switch_card(label: &str, sw: &ToggleSwitch) -> Result<Border> {
    let row = make_setting_row(label, f64::from(ROW_H))?;
    set_row_control(&row, &sw.cast()?)?;
    let card = make_card()?;
    card.SetChild(&row)?;
    Ok(card)
}

/// 夜间模式卡片：第一行「启用夜间模式」开关，第二行强度拉条，
/// 拉条左侧标签实时显示百分比与约值色温。
fn make_night_card(items: &Items) -> Result<Border> {
    let rows = StackPanel::new()?;

    let sw_row = make_setting_row("启用夜间模式", f64::from(ROW_H))?;
    set_row_control(&sw_row, &items.night.cast()?)?;
    rows.Children()?.Append(&sw_row)?;

    // 强度行：标签内容随拉条值变化，用 Items 里持有的那个 TextBlock。
    let st_row = Grid::new()?;
    st_row.SetHorizontalAlignment(HorizontalAlignment::Stretch)?;
    st_row.SetMinHeight(f64::from(ROW_H))?;
    for t in [GridUnitType::Star, GridUnitType::Auto] {
        let col = ColumnDefinition::new()?;
        col.SetWidth(GridLength {
            Value: 1.0,
            GridUnitType: t,
        })?;
        st_row.ColumnDefinitions()?.Append(&col)?;
    }
    items
        .strength_label
        .SetVerticalAlignment(VerticalAlignment::Center)?;
    Grid::SetColumn(&items.strength_label, 0)?;
    st_row.Children()?.Append(&items.strength_label)?;
    set_row_control(&st_row, &items.strength.cast()?)?;
    rows.Children()?.Append(&st_row)?;

    let card = make_card()?;
    card.SetChild(&rows)?;
    Ok(card)
}

/// 底栏：右对齐的退出图标按钮。
///
/// 不设背景、不设边框——背景即浮窗基底（亚克力本身），分隔线归上方内容区的底边。
///
/// 左右内边距与内容区取同一个值：`ContentDialog` 模板里 `CommandSpace.Padding`
/// 和内容区 Padding 绑的是同一个键 `ContentDialogPadding`，底栏并非通栏无边距；
/// 少了它，悬停底板会贴到浮窗边缘。上下不留，由 `FOOTER_H` 给高度即可。
fn make_footer() -> Result<Border> {
    let bar = StackPanel::new()?;
    bar.SetOrientation(Orientation::Horizontal)?;
    bar.SetHorizontalAlignment(HorizontalAlignment::Right)?;
    bar.SetVerticalAlignment(VerticalAlignment::Center)?;

    // U+E711 Cancel，取自 Segoe Fluent Icons，与系统底栏同款字形。
    let quit = make_command_button("\u{E711}", "退出")?;
    quit.Click(&RoutedEventHandler::new(|_, _| {
        // 回调跑在消息循环所在线程，直接投 WM_QUIT 即可。
        unsafe { PostQuitMessage(0) };
        Ok(())
    }))?;
    bar.Children()?.Append(&quit)?;

    let footer = Border::new()?;
    footer.SetMinHeight(f64::from(FOOTER_H))?;
    let pad = f64::from(CONTENT_PAD_V);
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

/// 内容区：标题 + 三张设置卡片，底边带分隔线。
///
/// 抬亮的是**内容区**而非底栏，这是照 `ContentDialog` 模板的归属：
/// 内容区 `Background = ContentDialogTopOverlay`（→ `LayerFillColorAltBrush`），
/// 底栏 `Background = {TemplateBinding Background}` 即对话框基底、不做抬亮，
/// 视觉上是「上亮下透」。浮窗坐在亚克力上，故换成 `LayerOnAcrylic` 那一支。
///
/// 分隔线同样归内容区：模板里 `BorderThickness="0,0,0,1"` 挂在内容区**底边**。
fn make_content(items: &Items) -> Result<Border> {
    let panel = StackPanel::new()?;
    panel.SetSpacing(f64::from(CARD_GAP))?;

    let title = TextBlock::new()?;
    title.SetText(h!("win-night-shift"))?;
    title.SetFontSize(14.0)?;
    title.SetFontWeight(windows::UI::Text::FontWeights::SemiBold()?)?;
    title.SetMargin(Thickness {
        Left: 2.0,
        Top: 0.0,
        Right: 0.0,
        Bottom: 2.0,
    })?;
    panel.Children()?.Append(&title)?;

    panel.Children()?.Append(&make_night_card(items)?)?;
    panel
        .Children()?
        .Append(&make_switch_card("深色模式", &items.dark)?)?;
    panel
        .Children()?
        .Append(&make_switch_card("开机自启", &items.autostart)?)?;

    let content = Border::new()?;
    set_brush("LayerOnAcrylicFillColorDefaultBrush", |b| {
        content.SetBackground(b)
    })?;
    set_brush("CardStrokeColorDefaultBrush", |b| content.SetBorderBrush(b))?;
    content.SetBorderThickness(Thickness {
        Left: 0.0,
        Top: 0.0,
        Right: 0.0,
        Bottom: 1.0,
    })?;
    let pad = f64::from(CONTENT_PAD_V);
    content.SetPadding(Thickness {
        Left: pad,
        Top: pad,
        Right: pad,
        Bottom: pad,
    })?;
    content.SetChild(&panel)?;
    Ok(content)
}

fn make_toggle() -> Result<ToggleSwitch> {
    let sw = ToggleSwitch::new()?;
    // 默认样式带 MinWidth≈156（为 On/Off 文本预留），会让所在 Auto 列吃掉标签的宽度。
    // 清掉，让列宽等于开关实际宽度。
    sw.SetMinWidth(0.0)?;
    Ok(sw)
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

    // 必须在任何 make_card / set_brush 之前：控件模板与主题画刷都在这份字典里。
    // 合并到 Application 级而非 root，set_brush 走的正是 Application::Current().Resources()。
    // 前提是 XamlApp::compose 已在 WindowsXamlManager 之前建立带元数据 provider
    // 的 Application，否则此处激活失败返回 E_FAIL。
    if let (Ok(res), Ok(app)) = (XamlControlsResources::new(), Application::Current()) {
        app.Resources()?.MergedDictionaries()?.Append(&res)?;
    }

    // 各控件的状态先留默认，与值一并在 SetContent 之后由 sync 落实（原因见 Items::sync）。
    let strength = Slider::new()?;
    strength.SetMinimum(0.0)?;
    strength.SetMaximum(100.0)?;
    strength.SetStepFrequency(1.0)?;
    strength.SetWidth(SLIDER_W)?;

    let items = Items {
        night: make_toggle()?,
        strength,
        strength_label: TextBlock::new()?,
        dark: make_toggle()?,
        autostart: make_toggle()?,
    };
    bind_controls(&items)?;

    root.Children()?.Append(&make_content(&items)?)?;
    root.Children()?.Append(&make_footer()?)?;

    src.SetContent(&root)?;

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
        visible: false,
        hidden_at: None,
    })
}

/// 把控件事件绑到注册表写入。所有回调先查 `is_syncing`：程序化写值（sync）不触发回写。
/// 写入失败（返回 false）时 refresh 一遍，把控件扳回真实状态。
fn bind_controls(items: &Items) -> Result<()> {
    bind_switch(&items.night, |v| {
        if !crate::nightlight::set_enabled(v) {
            refresh();
        }
    })?;
    bind_switch(&items.dark, |v| {
        if !crate::theme::set_dark(v) {
            refresh();
        }
    })?;
    bind_switch(&items.autostart, |v| {
        if !crate::autostart::set_autostart(v) {
            refresh();
        }
    })?;

    let label = items.strength_label.clone();
    items.strength.ValueChanged(&RangeBaseValueChangedEventHandler::new(
        move |_, args: Ref<'_, RangeBaseValueChangedEventArgs>| {
            if is_syncing() {
                return Ok(());
            }
            let p = args.ok()?.NewValue()?.round().clamp(0.0, 100.0) as u32;
            // 标签实时更新，但注册表写做节流：拖动会高频触发 ValueChanged，
            // 对 CloudStore 的密集外部写入会被系统判定冲突（实测会把夜间模式
            // 打回关闭）。两次写至少间隔 WRITE_THROTTLE，期间的值记入 PENDING，
            // 松手（PointerCaptureLost）时补写最后一笔。
            let _ = label.SetText(&HSTRING::from(strength_text(p)));
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

fn bind_switch<F: Fn(bool) + Send + 'static>(sw: &ToggleSwitch, f: F) -> Result<()> {
    let sw2 = sw.clone();
    sw.Toggled(&RoutedEventHandler::new(move |_, _| {
        if !is_syncing() {
            f(sw2.IsOn().unwrap_or(false));
        }
        Ok(())
    }))?;
    Ok(())
}
