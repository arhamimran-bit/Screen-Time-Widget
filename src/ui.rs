//! The widget window: creating it, handling Windows messages, and drawing it.
//!
//! How a Windows desktop app works, in one paragraph:
//! You register a window "class" with a callback (`wndproc`), create a window, then sit in a
//! *message loop* (`GetMessage` -> `DispatchMessage`). Windows calls `wndproc` for everything
//! that happens: WM_PAINT (redraw), WM_LBUTTONDOWN (click), WM_TIMER (our 1-second tick), etc.
//! All app state lives in `App`, which only the UI thread touches.
//!
//! Drawing uses Direct2D (shapes, images) and DirectWrite (text), both built into Windows.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use windows::core::{w, Result, PCSTR, PCWSTR};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Config};
use crate::dates::Date;
use crate::icons::{self, AppPaths, IconLoader};
use crate::settings::{Period, Settings};
use crate::storage::{self, Day};
use crate::tracker::{self, Activity};
use crate::{autostart, util};

// ---------- Layout (in DIPs: 1 DIP = 1 pixel at 100% scaling) ----------
const W: f32 = 460.0;
const H: f32 = 222.0;
const LEFT_W: f32 = 284.0;
const PANEL_W: f32 = W - LEFT_W;
const CHART_L: f32 = 18.0;
const CHART_R: f32 = 244.0;
const CHART_T: f32 = 104.0;
const CHART_B: f32 = 190.0;
// Today / 7D / 30D switch, top-left.
const SEG_X: f32 = 14.0;
const SEG_T: f32 = 10.0;
const SEG_B: f32 = 30.0;
const SEG_W: [f32; 3] = [48.0, 34.0, 40.0];
const ROWS_TOP: f32 = 8.0;
const DETAIL_HEAD: f32 = 44.0;
const LIST_TOP: f32 = DETAIL_HEAD + 2.0;
const LIST_ROW: f32 = 36.0;
const ICON: f32 = 20.0;

const TICK_TIMER: usize = 1;
const ANIM_TIMER: usize = 2;
const ANIM_MS: f32 = 220.0;

// ---------- Colours ----------
const fn rgb(hex: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}
const BG: D2D1_COLOR_F = rgb(0x1C1C1E);
const PANEL: D2D1_COLOR_F = rgb(0x242427);
const TEXT: D2D1_COLOR_F = rgb(0xF2F2F7);
const TEXT2: D2D1_COLOR_F = rgb(0xD1D1D6);
const SUB: D2D1_COLOR_F = rgb(0x8E8E93);
const GRID: D2D1_COLOR_F = rgb(0x333337);
const HOVER: D2D1_COLOR_F = rgb(0x323236);
const SEG_BG: D2D1_COLOR_F = rgb(0x2A2A2D);
const SEG_ON: D2D1_COLOR_F = rgb(0x434347);
const WHITE: D2D1_COLOR_F = rgb(0xFFFFFF);
const RED: D2D1_COLOR_F = rgb(0xFF6961);
const GREEN: D2D1_COLOR_F = rgb(0x4CD964);

fn alpha(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * a, ..c }
}

fn parse_color(s: &str) -> D2D1_COLOR_F {
    u32::from_str_radix(s.trim().trim_start_matches('#'), 16).map(rgb).unwrap_or(SUB)
}

// Icon glyphs from the "Segoe Fluent Icons" font that ships with Windows 11.
const CHEVRON_RIGHT: &str = "\u{E76C}";
const CHEVRON_LEFT: &str = "\u{E76B}";

// ---------- State ----------
#[derive(Clone, Copy, PartialEq)]
enum Hit {
    None,
    Row(usize),
    Back,
    Period(usize),
}

struct Fonts {
    seg: IDWriteTextFormat,
    now: IDWriteTextFormat,
    total: IDWriteTextFormat,
    suffix: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    axis: IDWriteTextFormat,
    name: IDWriteTextFormat,
    time: IDWriteTextFormat,
    head: IDWriteTextFormat,
    head_r: IDWriteTextFormat,
    item: IDWriteTextFormat,
    item_r: IDWriteTextFormat,
    empty: IDWriteTextFormat,
    letter: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    chevron: IDWriteTextFormat,
}

/// Window-bound drawing objects. Recreated if Windows says the target was lost.
struct Gfx {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    /// Logos per item key. None = no logo found (yet).
    icons: RefCell<HashMap<String, Option<ID2D1Bitmap>>>,
}

struct App {
    hwnd: HWND,
    dpi: u32,
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    fonts: Fonts,
    gfx: Option<Gfx>,
    icon_loader: Option<IconLoader>,
    app_paths: AppPaths,
    /// Apps whose logo source we've already checked this session.
    icon_checked: HashSet<String>,

    config: Config,
    config_mtime: Option<SystemTime>,
    config_error: Option<String>,
    settings: Settings,
    /// Today (kept in memory and saved every 30s).
    day: Day,
    /// Past days loaded from disk for the 7D/30D views. Past days never change.
    day_cache: HashMap<String, Day>,
    /// First day with any data: averages don't count days before the widget existed.
    first_day: Date,
    /// Numbers for the current view, rebuilt once per tick (not every animation frame).
    summary: Option<Summary>,
    names: HashMap<String, String>,
    current: Option<Activity>,

    last_tick: Instant,
    carry_ms: u64,
    last_save: Instant,
    ticks: u64,

    /// Which category the slide-over detail panel shows.
    detail_cat: usize,
    /// 0 = summary visible, 1 = detail visible; animates between.
    progress: f32,
    target: f32,
    last_anim: Instant,
    /// Detail list scroll position (DIPs), animated towards scroll_target.
    scroll: f32,
    scroll_target: f32,

    hover: Hit,
    press: Option<POINT>,
    tracking_mouse: bool,
}

struct ItemView {
    name: String,
    key: String,
    secs: u64,
}

struct CatView {
    name: String,
    color: D2D1_COLOR_F,
    icon: String,
    /// +1 = more is better (Work), -1 = less is better (Entertainment), 0 = neutral.
    goal: i8,
    /// Per-day average (Today: today's total).
    secs: u64,
    /// Change vs the previous 7/30 days (per-day average). None in Today view.
    trend: Option<i64>,
    items: Vec<ItemView>,
}

/// Everything one frame needs. In 7D/30D, all times are **averages per day**.
struct Summary {
    period: Period,
    avg: u64,
    cats: Vec<CatView>,
    /// slots[hour][category] = seconds. Today: actual. 7D/30D: average for that hour of the day.
    slots: Vec<Vec<u32>>,
    now_slot: usize,
    y_max: u64,
    /// One-line insight under the big number.
    insight: String,
    insight_color: D2D1_COLOR_F,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` with the app state. Returns None if the state is already in use (Windows can call
/// wndproc re-entrantly, e.g. while a menu or window drag is open).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

// ---------- Startup ----------
pub fn run() {
    unsafe {
        let hinst = GetModuleHandleW(None).expect("module handle");
        let class = w!("ScreenTimeWidget");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: HINSTANCE(hinst.0),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        enable_dark_menus();

        let settings = Settings::load();
        // TOOLWINDOW: no taskbar button. NOACTIVATE: clicking the widget doesn't steal focus
        // from the app you're using (so tracking isn't interrupted).
        let mut ex = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if settings.topmost {
            ex |= WS_EX_TOPMOST;
        }
        let hwnd = CreateWindowExW(
            ex,
            class,
            w!("Screen Time"),
            WS_POPUP,
            0,
            0,
            100,
            100,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(hinst.0),
            None,
        )
        .expect("create window");

        // Windows 11 look: rounded corners, subtle border.
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const _,
            std::mem::size_of_val(&corner) as u32,
        );
        let border: u32 = 0x003A3A3C; // COLORREF is 0x00BBGGRR
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border as *const _ as *const _,
            std::mem::size_of_val(&border) as u32,
        );

        let dpi = GetDpiForWindow(hwnd).max(96);
        let (w, h) = (scale(W, dpi), scale(H, dpi));
        let (x, y) = match (settings.x, settings.y) {
            (Some(x), Some(y)) if on_screen(x, y, w, h) => (x, y),
            _ => default_position(w),
        };
        let _ = SetWindowPos(hwnd, HWND::default(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);

        let d2d: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).expect("Direct2D");
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("DirectWrite");
        let fonts = create_fonts(&dw).expect("fonts");

        let (config, config_error) = match Config::load() {
            Ok(c) => (c, None),
            Err(e) => (Config::builtin(), Some(e)),
        };
        let (date, _) = storage::logical_now(config.day_starts_at_hour);
        let first_day = storage::first_day()
            .and_then(|d| Date::parse(&d))
            .or_else(|| Date::parse(&date))
            .unwrap_or(Date { y: 2026, m: 1, d: 1 });
        let now = Instant::now();
        let app = App {
            hwnd,
            dpi,
            d2d,
            dw,
            fonts,
            gfx: None,
            icon_loader: IconLoader::new(),
            app_paths: AppPaths::load(),
            icon_checked: HashSet::new(),
            config_mtime: config::modified(),
            config,
            config_error,
            settings,
            day: Day::load(&date),
            day_cache: HashMap::new(),
            first_day,
            summary: None,
            names: HashMap::new(),
            current: None,
            last_tick: now,
            carry_ms: 0,
            last_save: now,
            ticks: 0,
            detail_cat: 0,
            progress: 0.0,
            target: 0.0,
            last_anim: now,
            scroll: 0.0,
            scroll_target: 0.0,
            hover: Hit::None,
            press: None,
            tracking_mouse: false,
        };
        APP.with(|a| *a.borrow_mut() = Some(app));

        SetTimer(hwnd, TICK_TIMER, 1000, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn scale(v: f32, dpi: u32) -> i32 {
    (v * dpi as f32 / 96.0).round() as i32
}

unsafe fn on_screen(x: i32, y: i32, w: i32, h: i32) -> bool {
    let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
    !MonitorFromRect(&r, MONITOR_DEFAULTTONULL).is_invalid()
}

/// Top-right corner of the primary screen's work area (excludes the taskbar).
unsafe fn default_position(w: i32) -> (i32, i32) {
    let mut wa = RECT::default();
    let _ = SystemParametersInfoW(
        SPI_GETWORKAREA,
        0,
        Some(&mut wa as *mut _ as *mut _),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    );
    (wa.right - w - 24, wa.top + 24)
}

/// Make right-click menus dark. Uses an unnamed uxtheme export (ordinal 135) that
/// Windows itself uses; if it's missing we just get a light menu.
unsafe fn enable_dark_menus() {
    let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
    if let Some(f) = GetProcAddress(ux, PCSTR(135 as *const u8)) {
        let set_preferred_app_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
        set_preferred_app_mode(2); // ForceDark
    }
    if let Some(f) = GetProcAddress(ux, PCSTR(136 as *const u8)) {
        let flush_menu_themes: extern "system" fn() = std::mem::transmute(f);
        flush_menu_themes();
    }
}

unsafe fn create_fonts(dw: &IDWriteFactory) -> Result<Fonts> {
    let text = w!("Segoe UI Variable Text");
    let display = w!("Segoe UI Variable Display");
    let icons = w!("Segoe Fluent Icons");
    let lead = DWRITE_TEXT_ALIGNMENT_LEADING;
    let trail = DWRITE_TEXT_ALIGNMENT_TRAILING;
    let center = DWRITE_TEXT_ALIGNMENT_CENTER;
    let reg = DWRITE_FONT_WEIGHT_REGULAR;
    let semi = DWRITE_FONT_WEIGHT_SEMI_BOLD;
    Ok(Fonts {
        seg: font(dw, text, 11.5, semi, center, false)?,
        now: font(dw, text, 12.0, reg, trail, true)?,
        total: font(dw, display, 34.0, semi, lead, false)?,
        suffix: font(dw, text, 12.5, reg, lead, false)?,
        sub: font(dw, text, 11.5, reg, lead, true)?,
        axis: font(dw, text, 10.0, reg, lead, false)?,
        name: font(dw, text, 12.0, reg, lead, true)?,
        time: font(dw, display, 17.0, semi, lead, true)?,
        head: font(dw, text, 13.5, semi, lead, true)?,
        head_r: font(dw, text, 12.5, reg, trail, false)?,
        item: font(dw, text, 12.5, reg, lead, true)?,
        item_r: font(dw, text, 12.0, reg, trail, false)?,
        empty: font(dw, text, 12.0, reg, center, false)?,
        letter: font(dw, text, 11.0, semi, center, false)?,
        icon: font(dw, icons, 15.0, reg, center, false)?,
        chevron: font(dw, icons, 10.0, reg, center, false)?,
    })
}

unsafe fn font(
    dw: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
    ellipsis: bool,
) -> Result<IDWriteTextFormat> {
    let f = dw.CreateTextFormat(
        family,
        None,
        weight,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        size,
        w!("en-us"),
    )?;
    f.SetTextAlignment(align)?;
    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    if ellipsis {
        let sign = dw.CreateEllipsisTrimmingSign(&f)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        f.SetTrimming(&trimming, &sign)?;
    }
    Ok(f)
}

// ---------- Message handling ----------
extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                match wp.0 {
                    TICK_TIMER => with_app(|a| a.on_tick()),
                    ANIM_TIMER => with_app(|a| a.on_anim()),
                    _ => None,
                };
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                with_app(|a| a.paint());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_LBUTTONDOWN => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                with_app(|a| a.press = Some(pt));
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = mouse_xy(lp);
                if with_app(|a| a.on_mouse_move(x, y)) == Some(true) {
                    // Hand the drag to Windows: it moves the window as if you grabbed a title bar.
                    let _ = ReleaseCapture();
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                let (x, y) = mouse_xy(lp);
                with_app(|a| a.on_click(x, y));
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xffff) as i16 as f32;
                with_app(|a| a.on_wheel(delta));
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_app(|a| {
                    a.tracking_mouse = false;
                    a.set_hover(Hit::None);
                });
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let hand = with_app(|a| a.hover != Hit::None).unwrap_or(false);
                if hand && (lp.0 & 0xffff) as u32 == HTCLIENT {
                    SetCursor(LoadCursorW(None, IDC_HAND).unwrap_or_default());
                    return LRESULT(1);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_RBUTTONUP => {
                show_menu(hwnd);
                LRESULT(0)
            }
            WM_EXITSIZEMOVE => {
                with_app(|a| a.save_position());
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                with_app(|a| a.dpi = (wp.0 & 0xffff) as u32);
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                with_app(|a| a.gfx = None);
                LRESULT(0)
            }
            WM_SIZE => {
                with_app(|a| a.gfx = None);
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                if wp.0 as u32 == PBT_APMSUSPEND {
                    with_app(|a| a.save_all());
                }
                LRESULT(1)
            }
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION => {
                with_app(|a| a.save_all());
                LRESULT(0)
            }
            WM_DESTROY => {
                with_app(|a| a.save_all());
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn mouse_xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

const ID_TOPMOST: usize = 1;
const ID_LOCK: usize = 2;
const ID_AUTOSTART: usize = 3;
const ID_CONFIG: usize = 4;
const ID_DATA: usize = 5;
const ID_EXIT: usize = 6;

unsafe fn show_menu(hwnd: HWND) {
    let Some((topmost, locked)) = with_app(|a| (a.settings.topmost, a.settings.locked)) else { return };
    let Ok(menu) = CreatePopupMenu() else { return };
    let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
    let _ = AppendMenuW(menu, MF_STRING | check(topmost), ID_TOPMOST, w!("Always on top"));
    let _ = AppendMenuW(menu, MF_STRING | check(locked), ID_LOCK, w!("Lock position"));
    let _ = AppendMenuW(menu, MF_STRING | check(autostart::is_enabled()), ID_AUTOSTART, w!("Start with Windows"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_CONFIG, w!("Edit categories (config.json)"));
    let _ = AppendMenuW(menu, MF_STRING, ID_DATA, w!("Open data folder"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd); // required so the menu closes when you click elsewhere
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    match cmd.0 as usize {
        ID_TOPMOST => {
            let on = with_app(|a| {
                a.settings.topmost = !a.settings.topmost;
                a.settings.save();
                a.settings.topmost
            })
            .unwrap_or(false);
            let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
            let _ = SetWindowPos(hwnd, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        ID_LOCK => {
            with_app(|a| {
                a.settings.locked = !a.settings.locked;
                a.settings.save();
            });
        }
        ID_AUTOSTART => autostart::set(!autostart::is_enabled()),
        ID_CONFIG => open_path(hwnd, &config::path().display().to_string(), true),
        ID_DATA => open_path(hwnd, &util::app_dir().display().to_string(), false),
        ID_EXIT => {
            let _ = DestroyWindow(hwnd);
        }
        _ => {}
    }
}

/// Open a file/folder with its default app; fall back to Notepad for .json with no default.
unsafe fn open_path(hwnd: HWND, path: &str, notepad_fallback: bool) {
    let p = util::wide(path);
    let r = ShellExecuteW(hwnd, w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    if (r.0 as isize) <= 32 && notepad_fallback {
        ShellExecuteW(hwnd, w!("open"), w!("notepad.exe"), PCWSTR(p.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

impl App {
    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn dip(&self, px: i32) -> f32 {
        px as f32 * 96.0 / self.dpi as f32
    }

    // ----- tracking -----
    fn on_tick(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_tick).as_millis() as u64;
        self.last_tick = now;
        // A big gap means the laptop slept or the app froze: don't count it.
        if elapsed < 5000 {
            self.carry_ms += elapsed;
        }
        let secs = self.carry_ms / 1000;
        self.carry_ms %= 1000;

        self.ticks += 1;
        if self.ticks % 3 == 0 {
            self.check_config();
        }

        let (date, slot) = storage::logical_now(self.config.day_starts_at_hour);
        if date != self.day.date {
            self.day.save();
            self.day = Day::load(&date);
        }

        self.current = tracker::detect(&self.config, &mut self.names);
        let new_favicon = icons::CHANGED.swap(false, Ordering::Relaxed);
        let mut reload_icons = false;
        if let Some(a) = &self.current {
            if a.counting && secs > 0 {
                self.day.add(&a.key, &a.name, slot, secs);
            }
            // Once per app per session: (re)check where its logo comes from.
            if let Some(p) = &a.exe_path {
                if self.icon_checked.insert(a.key.clone()) {
                    reload_icons |= self.app_paths.remember(&a.key, &icons::icon_source(a.pid, p));
                }
            }
        }
        // Shortly after start, then every 5 minutes: find logos for apps in the data
        // by looking at running processes (so you don't have to open each app first).
        if self.ticks % 300 == 2 {
            reload_icons |= self.fill_icons_from_running();
        }
        if let Some(g) = &self.gfx {
            if reload_icons {
                g.icons.borrow_mut().clear();
            } else if new_favicon {
                g.icons.borrow_mut().retain(|_, v| v.is_some());
            }
        }

        if now.duration_since(self.last_save) >= Duration::from_secs(30) {
            self.day.save();
            self.last_save = now;
        }
        self.summary = None;
        self.invalidate();
    }

    fn fill_icons_from_running(&mut self) -> bool {
        let mut keys: Vec<String> = std::iter::once(&self.day)
            .chain(self.day_cache.values())
            .flat_map(|d| d.items.keys())
            .filter(|k| k.starts_with("app:") && !self.icon_checked.contains(*k))
            .cloned()
            .collect();
        keys.sort();
        keys.dedup();
        let mut changed = false;
        for (key, source) in icons::find_running(&keys) {
            changed |= self.app_paths.remember(&key, &source);
            self.icon_checked.insert(key);
        }
        changed
    }

    /// Reload config.json when it changes on disk (checked every 3 seconds).
    fn check_config(&mut self) {
        let m = config::modified();
        if m == self.config_mtime {
            return;
        }
        self.config_mtime = m;
        self.summary = None;
        match Config::load() {
            Ok(c) => {
                self.config = c;
                self.config_error = None;
                if self.detail_cat >= self.config.n_cats() {
                    self.detail_cat = 0;
                    self.progress = 0.0;
                    self.target = 0.0;
                }
            }
            Err(e) => self.config_error = Some(e),
        }
    }

    fn save_all(&mut self) {
        self.day.save();
        self.settings.save();
    }

    fn save_position(&mut self) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        self.settings.x = Some(r.left);
        self.settings.y = Some(r.top);
        self.settings.save();
    }

    // ----- mouse -----
    fn hit(&self, x: f32, y: f32) -> Hit {
        if (SEG_T - 2.0..SEG_B + 2.0).contains(&y) {
            let mut sx = SEG_X;
            for (i, w) in SEG_W.iter().enumerate() {
                if (sx..sx + w).contains(&x) {
                    return Hit::Period(i);
                }
                sx += w;
            }
        }
        if x < LEFT_W || self.progress != self.target {
            return Hit::None;
        }
        if self.target == 0.0 {
            let n = self.config.n_cats();
            let row_h = (H - ROWS_TOP * 2.0) / n as f32;
            let i = ((y - ROWS_TOP) / row_h).floor();
            if i >= 0.0 && (i as usize) < n {
                return Hit::Row(i as usize);
            }
        } else if y < DETAIL_HEAD {
            return Hit::Back;
        }
        Hit::None
    }

    fn set_hover(&mut self, h: Hit) {
        if self.hover != h {
            self.hover = h;
            self.invalidate();
        }
    }

    /// Returns true when a drag should start.
    fn on_mouse_move(&mut self, x: i32, y: i32) -> bool {
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking_mouse = true;
        }
        if let Some(p) = self.press {
            let mut cur = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cur);
            }
            if !self.settings.locked && ((cur.x - p.x).abs() > 4 || (cur.y - p.y).abs() > 4) {
                self.press = None;
                self.set_hover(Hit::None);
                return true;
            }
        }
        let h = self.hit(self.dip(x), self.dip(y));
        self.set_hover(h);
        false
    }

    fn on_click(&mut self, x: i32, y: i32) {
        if self.press.take().is_none() {
            return;
        }
        match self.hit(self.dip(x), self.dip(y)) {
            Hit::Row(i) => {
                self.detail_cat = i;
                self.scroll = 0.0;
                self.scroll_target = 0.0;
                self.animate_to(1.0);
            }
            Hit::Back => self.animate_to(0.0),
            Hit::Period(i) => {
                self.settings.period = Period::ALL[i];
                self.settings.save();
                self.summary = None;
                self.invalidate();
            }
            Hit::None => {}
        }
    }

    /// Mouse wheel scrolls the breakdown list.
    fn on_wheel(&mut self, delta: f32) {
        if self.target != 1.0 {
            return;
        }
        let n = self.summary.as_ref().and_then(|s| s.cats.get(self.detail_cat)).map_or(0, |c| c.items.len());
        let max = max_scroll(n);
        self.scroll_target = (self.scroll_target - delta / 120.0 * LIST_ROW * 1.5).clamp(0.0, max);
        self.start_anim_timer();
    }

    // ----- animation (panel slide + smooth scroll) -----
    fn animate_to(&mut self, target: f32) {
        self.target = target;
        self.hover = Hit::None;
        self.summary = None; // the insight line follows the open category
        self.start_anim_timer();
    }

    fn start_anim_timer(&mut self) {
        self.last_anim = Instant::now();
        unsafe {
            SetTimer(self.hwnd, ANIM_TIMER, 15, None);
        }
    }

    fn on_anim(&mut self) {
        let now = Instant::now();
        let step = now.duration_since(self.last_anim).as_secs_f32() * 1000.0 / ANIM_MS;
        self.last_anim = now;
        if self.progress < self.target {
            self.progress = (self.progress + step).min(self.target);
        } else {
            self.progress = (self.progress - step).max(self.target);
        }
        let ds = self.scroll_target - self.scroll;
        self.scroll = if ds.abs() < 0.5 { self.scroll_target } else { self.scroll + ds * 0.3 };
        if self.progress == self.target && self.scroll == self.scroll_target {
            unsafe {
                let _ = KillTimer(self.hwnd, ANIM_TIMER);
            }
        }
        self.invalidate();
    }

    // ----- numbers for the current view -----
    fn today(&self) -> Date {
        Date::parse(&self.day.date).unwrap_or(self.first_day)
    }

    fn day_ref(&self, date: Date) -> Option<&Day> {
        if date == self.today() {
            Some(&self.day)
        } else {
            self.day_cache.get(&date.to_string())
        }
    }

    /// Load past days from disk into the cache (once; past days never change).
    fn ensure_loaded(&mut self, start: Date, len: usize) {
        let today = self.today();
        if self.day_cache.len() > 120 {
            self.day_cache.clear();
        }
        for i in 0..len {
            let date = start.add(i as i64);
            if date >= today {
                break;
            }
            let key = date.to_string();
            if !self.day_cache.contains_key(&key) {
                let d = Day::load(&key);
                self.day_cache.insert(key, d);
            }
        }
    }

    /// Days in [start, start+len) that count toward an average:
    /// not before the first tracked day, not after today.
    fn counted_days(&self, start: Date, len: usize) -> u64 {
        let from = start.days().max(self.first_day.days());
        let to = (start.days() + len as i64 - 1).min(self.today().days());
        (to - from + 1).max(0) as u64
    }

    fn compute(&mut self) -> Summary {
        let period = self.settings.period;
        let today = self.today();
        let n = self.config.n_cats();
        let (now_slot, now_min) = (storage::logical_now(self.config.day_starts_at_hour).1, storage::now_minute());

        // Rolling windows that end today: 1, 7 or 30 days. "prev" = the window right before it.
        let len: usize = match period {
            Period::Day => 1,
            Period::Week => 7,
            Period::Month => 30,
        };
        let start = today.add(1 - len as i64);
        // Today view compares against the 7 days before today; 7D/30D against the previous window.
        let (prev_start, prev_len) = if period == Period::Day { (today.add(-7), 7) } else { (start.add(-(len as i64)), len) };
        self.ensure_loaded(prev_start, prev_len);
        self.ensure_loaded(start, len);

        let mut rules: HashMap<String, (usize, Option<String>)> = HashMap::new();
        let mut rule = |key: &str, cfg: &Config| -> (usize, Option<String>) {
            rules
                .entry(key.to_string())
                .or_insert_with(|| {
                    let r = cfg.rule_for(key);
                    (r.cat, r.name)
                })
                .clone()
        };

        let mut cats: Vec<CatView> = (0..n)
            .map(|i| {
                let c = self.config.cat(i);
                let goal = match c.goal.trim().to_ascii_lowercase().as_str() {
                    "more" => 1,
                    "less" => -1,
                    _ => 0,
                };
                CatView { name: c.name.clone(), color: parse_color(&c.color), icon: c.icon.clone(), goal, secs: 0, trend: None, items: Vec::new() }
            })
            .collect();
        let mut slot_sum = vec![vec![0u64; n]; 24];
        // display name -> (seconds, key of its biggest entry, that entry's seconds)
        let mut merged: Vec<HashMap<String, (u64, String, u64)>> = (0..n).map(|_| HashMap::new()).collect();
        let mut total = 0u64;

        for i in 0..len {
            let Some(day) = self.day_ref(start.add(i as i64)) else { continue };
            for (key, it) in &day.items {
                if self.config.is_ignored(key) {
                    continue;
                }
                let (cat, name) = rule(key, &self.config);
                total += it.secs;
                cats[cat].secs += it.secs;
                let e = merged[cat].entry(name.unwrap_or_else(|| it.name.clone())).or_insert((0, key.clone(), 0));
                e.0 += it.secs;
                if it.secs > e.2 {
                    e.1 = key.clone();
                    e.2 = it.secs;
                }
                for (h, s) in it.hours.iter().take(24).enumerate() {
                    slot_sum[h][cat] += *s as u64;
                }
            }
        }

        // Previous window per category (for the ▲/▼ arrows) and per hour (for Today's "usually peaks").
        // In Today view the comparison only counts up to this time of day, so a morning isn't
        // compared against whole days.
        let mut prev_cat = vec![0u64; n];
        let mut prev_slot_sum = vec![vec![0u64; n]; 24];
        for i in 0..prev_len {
            let Some(day) = self.day_ref(prev_start.add(i as i64)) else { continue };
            for (key, it) in &day.items {
                if self.config.is_ignored(key) {
                    continue;
                }
                let (cat, _) = rule(key, &self.config);
                prev_cat[cat] += if period == Period::Day {
                    let before: u64 = it.hours.iter().take(now_slot).map(|&s| s as u64).sum();
                    before + it.hours.get(now_slot).map_or(0, |&s| s as u64 * now_min as u64 / 60)
                } else {
                    it.secs
                };
                for (h, s) in it.hours.iter().take(24).enumerate() {
                    prev_slot_sum[h][cat] += *s as u64;
                }
            }
        }

        let div = self.counted_days(start, len).max(1);
        let prev_div = self.counted_days(prev_start, prev_len);
        // Arrows only once there's enough history to be meaningful (the user prefers accurate
        // arrows over early ones): a full previous week for Today/7D, two weeks for 30D.
        let min_prev_days = if period == Period::Month { 14 } else { 7 };
        for (i, (c, m)) in cats.iter_mut().zip(merged).enumerate() {
            c.secs /= div;
            if prev_div >= min_prev_days {
                c.trend = Some(c.secs as i64 - (prev_cat[i] / prev_div) as i64);
            }
            c.items = m
                .into_iter()
                .map(|(name, (secs, key, _))| ItemView { name, key, secs: secs / div })
                .filter(|it| it.secs >= 1)
                .collect();
            c.items.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.name.cmp(&b.name)));
        }
        let slots: Vec<Vec<u32>> = slot_sum.iter().map(|row| row.iter().map(|&s| (s / div) as u32).collect()).collect();

        // Insight line about Work (or the category whose breakdown is open).
        let focus = if self.target == 1.0 { self.detail_cat } else { self.config.focus_cat() }.min(n - 1);
        let fc = &cats[focus];
        // "Work peaks 8pm–11pm": the 3-hour window of the day with the most time in this category.
        // 7D/30D use the window itself; Today uses your last 7 days ("usually").
        let hourly: Vec<u64> = if period == Period::Day {
            prev_slot_sum.iter().map(|row| row[focus] / prev_div.max(1)).collect()
        } else {
            slots.iter().map(|row| row[focus] as u64).collect()
        };
        let window = |h: usize| (0..3).map(|k| hourly[(h + k) % 24]).sum::<u64>();
        let best = (0..24).max_by_key(|&h| (window(h), std::cmp::Reverse(h))).unwrap_or(0);
        let usually = if period == Period::Day { "usually " } else { "" };
        let insight = if window(best) >= 60 {
            format!("{} {usually}peaks {}–{}", fc.name, hour_label(best as u32), hour_label(((best + 3) % 24) as u32))
        } else {
            String::new()
        };
        let insight_color = SUB;

        let y_max = if period == Period::Day {
            3600
        } else {
            let max = slots.iter().map(|c| c.iter().map(|&v| v as u64).sum::<u64>()).max().unwrap_or(0);
            [600, 1200, 1800, 3600].into_iter().find(|&m| m >= max).unwrap_or(3600)
        };

        Summary { period, avg: total / div, cats, slots, now_slot, y_max, insight, insight_color }
    }

    // ----- drawing -----
    fn paint(&mut self) {
        if self.gfx.is_none() {
            self.gfx = unsafe { self.create_gfx() }.ok();
        }
        if self.summary.is_none() {
            self.summary = Some(self.compute());
        }
        let (Some(g), Some(s)) = (&self.gfx, &self.summary) else { return };
        if let Err(e) = unsafe { self.draw(g, s) } {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.gfx = None;
            }
        }
    }

    unsafe fn create_gfx(&self) -> Result<Gfx> {
        let mut rc = RECT::default();
        GetClientRect(self.hwnd, &mut rc)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            // Software rendering: avoids loading GPU drivers into our process (keeps RAM tiny).
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_UNKNOWN, alphaMode: D2D1_ALPHA_MODE_UNKNOWN },
            dpiX: self.dpi as f32,
            dpiY: self.dpi as f32,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U {
                width: (rc.right - rc.left).max(1) as u32,
                height: (rc.bottom - rc.top).max(1) as u32,
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        let rt = self.d2d.CreateHwndRenderTarget(&props, &hprops)?;
        let brush = rt.CreateSolidColorBrush(&WHITE, None)?;
        Ok(Gfx { rt, brush, icons: RefCell::new(HashMap::new()) })
    }

    unsafe fn draw(&self, g: &Gfx, s: &Summary) -> Result<()> {
        g.rt.BeginDraw();
        g.rt.SetTransform(&Matrix3x2::identity());
        g.rt.Clear(Some(&BG));

        self.draw_left(g, s);

        fill(g, LEFT_W, 0.0, W, H, PANEL);
        g.rt.PushAxisAlignedClip(&rect(LEFT_W, 0.0, W, H), D2D1_ANTIALIAS_MODE_ALIASED);
        let e = ease(self.progress);
        if e < 1.0 {
            self.draw_rows(g, s, -e * PANEL_W);
        }
        if e > 0.0 {
            self.draw_detail(g, s, (1.0 - e) * PANEL_W);
        }
        g.rt.PopAxisAlignedClip();

        g.rt.EndDraw(None, None)
    }

    /// Left side: Today/7D/30D switch, live status, big number, insight, chart by hour of day.
    unsafe fn draw_left(&self, g: &Gfx, s: &Summary) {
        let f = &self.fonts;

        // Segmented Today / 7D / 30D control.
        let seg_total: f32 = SEG_W.iter().sum();
        round(g, SEG_X - 2.0, SEG_T - 2.0, SEG_X + seg_total + 2.0, SEG_B + 2.0, 8.0, SEG_BG);
        let mut sx = SEG_X;
        for (i, (p, w)) in Period::ALL.iter().zip(SEG_W).enumerate() {
            let on = *p == s.period;
            if on {
                round(g, sx, SEG_T, sx + w, SEG_B, 6.0, SEG_ON);
            }
            let col = if on { TEXT } else if self.hover == Hit::Period(i) { TEXT2 } else { SUB };
            text(g, p.label(), &f.seg, sx, SEG_T, sx + w, SEG_B, col);
            sx += w;
        }

        // Live status: what's being tracked right now.
        let (label, dot, label_color) = match (&self.config_error, &self.current) {
            (Some(_), _) => ("config.json error".to_string(), RED, RED),
            (None, Some(a)) if a.counting => (a.name.clone(), parse_color(&self.config.cat(a.cat).color), TEXT2),
            (None, Some(a)) => (format!("{} · idle", a.name), SUB, SUB),
            (None, None) => ("Not tracking".to_string(), SUB, SUB),
        };
        let right = LEFT_W - 16.0;
        let max_w = right - (SEG_X + seg_total + 26.0);
        let tw = self.text_width(&label, &f.now).min(max_w);
        text(g, &label, &f.now, right - max_w, SEG_T, right, SEG_B, label_color);
        g.brush.SetColor(&dot);
        g.rt.FillEllipse(
            &D2D1_ELLIPSE { point: D2D_POINT_2F { x: right - tw - 9.0, y: (SEG_T + SEG_B) / 2.0 }, radiusX: 3.5, radiusY: 3.5 },
            &g.brush,
        );

        // Big number + "avg / day", then the insight line.
        let big = util::fmt_dur(s.avg);
        text(g, &big, &f.total, 16.0, 34.0, LEFT_W - 16.0, 80.0, TEXT);
        let bw = self.text_width(&big, &f.total);
        let suffix = if s.period == Period::Day { "today" } else { "avg / day" };
        text(g, suffix, &f.suffix, 16.0 + bw + 7.0, 52.0, LEFT_W - 16.0, 72.0, SUB);
        text(g, &s.insight, &f.sub, 17.0, 78.0, LEFT_W - 16.0, 94.0, s.insight_color);

        // Chart grid + y labels.
        let ch = CHART_B - CHART_T;
        for (frac, secs) in [(0.0, 0), (0.5, s.y_max / 2), (1.0, s.y_max)] {
            let y = CHART_B - frac * ch;
            line(g, CHART_L, y, CHART_R, y, GRID);
            text(g, &axis_label(secs), &f.axis, CHART_R + 6.0, y - 8.0, LEFT_W, y + 8.0, SUB);
        }
        let slot_w = (CHART_R - CHART_L) / 24.0;
        // A label every 3 hours. Dotted guides: stronger at 6/12/18, fainter at 3/9/15/21.
        let start_h = self.config.day_starts_at_hour.min(23);
        for slot in (0..24usize).step_by(3) {
            let x = CHART_L + slot as f32 * slot_w;
            if slot > 0 {
                let col = if slot % 6 == 0 { GRID } else { alpha(GRID, 0.55) };
                let mut y = CHART_T;
                while y < CHART_B {
                    line(g, x, y, x, (y + 2.0).min(CHART_B), col);
                    y += 5.0;
                }
            }
            text(g, &hour_label((start_h + slot as u32) % 24), &f.axis, x + 1.0, CHART_B + 3.0, x + 40.0, CHART_B + 19.0, SUB);
        }
        let hx = CHART_L + s.now_slot as f32 * slot_w;
        fill(g, hx, CHART_T, hx + slot_w, CHART_B, alpha(WHITE, 0.04));

        // Stacked bars per hour. In detail view, other categories fade out.
        let bar_w = slot_w * 0.62;
        let e = ease(self.progress);
        let y_max = s.y_max.max(1) as f32;
        for (slot, cats) in s.slots.iter().enumerate() {
            let top_cat = cats.iter().rposition(|&v| v > 0);
            let bx = CHART_L + slot as f32 * slot_w + (slot_w - bar_w) / 2.0;
            let mut y = CHART_B;
            for (c, &v) in cats.iter().enumerate() {
                if v == 0 {
                    continue;
                }
                let h = ((v as f32 / y_max).min(1.0) * ch).max(1.5).min(y - CHART_T);
                let fade = if c == self.detail_cat { 1.0 } else { 1.0 - 0.8 * e };
                let col = alpha(s.cats[c].color, fade);
                if Some(c) == top_cat {
                    let r = (bar_w / 2.0).min(h / 2.0).min(2.0);
                    round(g, bx, y - h, bx + bar_w, y, r, col);
                    fill(g, bx, y - h / 2.0, bx + bar_w, y, col);
                } else {
                    fill(g, bx, y - h, bx + bar_w, y, col);
                }
                y -= h;
            }
        }
    }

    /// Right side, summary: one row per category. Click a row to slide in its breakdown.
    unsafe fn draw_rows(&self, g: &Gfx, s: &Summary, off: f32) {
        let f = &self.fonts;
        let n = s.cats.len() as f32;
        let row_h = (H - ROWS_TOP * 2.0) / n;
        let (x, r) = (LEFT_W + off, W + off);
        let sum: u64 = s.cats.iter().map(|c| c.secs).sum();
        for (i, c) in s.cats.iter().enumerate() {
            let y0 = ROWS_TOP + i as f32 * row_h;
            let cy = y0 + row_h / 2.0;
            if self.hover == Hit::Row(i) {
                round(g, x + 6.0, y0 + 2.0, r - 6.0, y0 + row_h - 2.0, 10.0, HOVER);
            }
            round(g, x + 14.0, cy - 15.0, x + 44.0, cy + 15.0, 8.0, c.color);
            text(g, &c.icon, &f.icon, x + 14.0, cy - 15.0, x + 44.0, cy + 15.0, WHITE);

            text(g, &c.name, &f.name, x + 56.0, cy - 17.0, r - 24.0, cy - 1.0, SUB);

            // Time, then (7D/30D) the trend vs the previous window, or (Today) the % share.
            let time = util::fmt_dur(c.secs);
            text(g, &time, &f.time, x + 56.0, cy - 3.0, r - 24.0, cy + 19.0, TEXT);
            let tx = x + 56.0 + self.text_width(&time, &f.time) + 6.0;
            if let Some(d) = c.trend {
                let (t, col) = if d.abs() < 60 {
                    ("=".to_string(), SUB)
                } else {
                    (format!("{}{}", if d > 0 { "▲" } else { "▼" }, util::fmt_dur(d.unsigned_abs())), trend_color(d, c.goal))
                };
                text(g, &t, &f.sub, tx, cy - 1.0, r - 24.0, cy + 18.0, col);
            } else if sum > 0 {
                let pct = format!("{}%", (c.secs * 100 + sum / 2) / sum);
                text(g, &pct, &f.sub, tx, cy - 1.0, r - 24.0, cy + 18.0, SUB);
            }
            text(g, CHEVRON_RIGHT, &f.chevron, r - 24.0, cy - 8.0, r - 10.0, cy + 8.0, SUB);
        }
    }

    /// Right side, detail: every app/site in one category with its logo. Scrolls with the wheel.
    unsafe fn draw_detail(&self, g: &Gfx, s: &Summary, off: f32) {
        let f = &self.fonts;
        let Some(c) = s.cats.get(self.detail_cat) else { return };
        let (x, r) = (LEFT_W + off, W + off);
        let per_day = if s.period == Period::Day { "" } else { "/day" };

        if self.hover == Hit::Back {
            round(g, x + 6.0, 6.0, r - 6.0, DETAIL_HEAD - 4.0, 8.0, HOVER);
        }
        text(g, CHEVRON_LEFT, &f.chevron, x + 12.0, 14.0, x + 26.0, 32.0, SUB);
        let right_txt = format!("{}{per_day}", util::fmt_dur(c.secs));
        let rw = self.text_width(&right_txt, &f.head_r);
        text(g, &c.name, &f.head, x + 30.0, 12.0, r - 14.0 - rw - 8.0, 34.0, TEXT);
        text(g, &right_txt, &f.head_r, r - 14.0 - rw - 4.0, 12.0, r - 14.0, 34.0, SUB);
        line(g, x + 12.0, DETAIL_HEAD, r - 12.0, DETAIL_HEAD, GRID);

        if c.items.is_empty() {
            text(g, "Nothing here yet", &f.empty, x, 90.0, r, 120.0, SUB);
            return;
        }

        let scroll = self.scroll.clamp(0.0, max_scroll(c.items.len()));
        g.rt.PushAxisAlignedClip(&rect(x, LIST_TOP, r, H), D2D1_ANTIALIAS_MODE_ALIASED);
        let max = c.items[0].secs.max(1) as f32;
        let (bl, br) = (x + 14.0, r - 14.0);
        for (j, it) in c.items.iter().enumerate() {
            let y = LIST_TOP + 6.0 + j as f32 * LIST_ROW - scroll;
            if y + LIST_ROW < LIST_TOP || y > H {
                continue;
            }
            // Logo (or a coloured letter if we don't have one yet).
            let ir = rect(bl, y + 1.0, bl + ICON, y + 1.0 + ICON);
            match self.icon_for(g, &it.key) {
                Some(bmp) => g.rt.DrawBitmap(&bmp, Some(&ir), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                None => {
                    round(g, ir.left, ir.top, ir.right, ir.bottom, 5.0, alpha(c.color, 0.22));
                    let letter: String = it.name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
                    text(g, &letter, &f.letter, ir.left, ir.top, ir.right, ir.bottom, c.color);
                }
            }
            let nx = bl + ICON + 9.0;
            text(g, &it.name, &f.item, nx, y + 1.0, r - 62.0, y + 1.0 + ICON, TEXT2);
            text(g, &util::fmt_dur(it.secs), &f.item_r, r - 62.0, y + 1.0, br, y + 1.0 + ICON, SUB);
            let by = y + ICON + 5.0;
            round(g, nx, by, br, by + 2.5, 1.25, GRID);
            let frac = (it.secs as f32 / max).clamp(0.03, 1.0);
            round(g, nx, by, nx + (br - nx) * frac, by + 2.5, 1.25, c.color);
        }
        g.rt.PopAxisAlignedClip();

        // Thin scrollbar when the list is longer than the panel.
        let content = c.items.len() as f32 * LIST_ROW + 6.0;
        let view = H - LIST_TOP;
        if content > view {
            let thumb = (view * view / content).max(18.0);
            let ty = LIST_TOP + (view - thumb) * (scroll / max_scroll(c.items.len()).max(1.0));
            round(g, r - 5.0, ty + 2.0, r - 2.5, ty + thumb - 2.0, 1.25, alpha(WHITE, 0.2));
        }
    }

    /// Logo for an item key, loaded once and cached with the render target.
    unsafe fn icon_for(&self, g: &Gfx, key: &str) -> Option<ID2D1Bitmap> {
        if let Some(v) = g.icons.borrow().get(key) {
            return v.clone();
        }
        let bmp = self.icon_loader.as_ref().and_then(|l| l.load(&g.rt, key, &self.app_paths));
        g.icons.borrow_mut().insert(key.to_string(), bmp.clone());
        bmp
    }

    fn text_width(&self, s: &str, fmt: &IDWriteTextFormat) -> f32 {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dw.CreateTextLayout(&w, fmt, 1000.0, 50.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_err() {
                return 0.0;
            }
            m.width
        }
    }
}

// ---------- Helpers ----------
fn max_scroll(n_items: usize) -> f32 {
    (n_items as f32 * LIST_ROW + 6.0 - (H - LIST_TOP)).max(0.0)
}

/// Green when a category moves the way you want (Work up, Entertainment down), red otherwise.
fn trend_color(delta: i64, goal: i8) -> D2D1_COLOR_F {
    match (goal as i64) * delta.signum() {
        1 => GREEN,
        -1 => RED,
        _ => SUB,
    }
}

fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

unsafe fn fill(g: &Gfx, l: f32, t: f32, r: f32, b: f32, c: D2D1_COLOR_F) {
    g.brush.SetColor(&c);
    g.rt.FillRectangle(&rect(l, t, r, b), &g.brush);
}

unsafe fn round(g: &Gfx, l: f32, t: f32, r: f32, b: f32, radius: f32, c: D2D1_COLOR_F) {
    g.brush.SetColor(&c);
    let rr = D2D1_ROUNDED_RECT { rect: rect(l, t, r, b), radiusX: radius, radiusY: radius };
    g.rt.FillRoundedRectangle(&rr, &g.brush);
}

unsafe fn line(g: &Gfx, x0: f32, y0: f32, x1: f32, y1: f32, c: D2D1_COLOR_F) {
    g.brush.SetColor(&c);
    g.rt.DrawLine(D2D_POINT_2F { x: x0, y: y0 }, D2D_POINT_2F { x: x1, y: y1 }, &g.brush, 1.0, None);
}

#[allow(clippy::too_many_arguments)]
unsafe fn text(g: &Gfx, s: &str, fmt: &IDWriteTextFormat, l: f32, t: f32, r: f32, b: f32, c: D2D1_COLOR_F) {
    if s.is_empty() {
        return;
    }
    let w: Vec<u16> = s.encode_utf16().collect();
    g.brush.SetColor(&c);
    g.rt.DrawText(&w, fmt, &rect(l, t, r, b), &g.brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
}

/// Chart axis: 0 -> "0", 1800 -> "30m", 3600 -> "1h".
fn axis_label(secs: u64) -> String {
    if secs == 0 {
        "0".into()
    } else if secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}m", secs / 60)
    }
}

fn hour_label(h: u32) -> String {
    match h {
        0 => "12am".into(),
        12 => "12pm".into(),
        h if h < 12 => format!("{h}am"),
        h => format!("{}pm", h - 12),
    }
}
