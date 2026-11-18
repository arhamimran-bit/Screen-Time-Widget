//! "What is the user doing right now?" Called once per second by the UI timer.
//!
//! 1. Ask Windows for the foreground window and which .exe owns it.
//! 2. If it's Chrome, use the site the extension last reported instead.
//! 3. Look the app/site up in the config to get its category and idle rule.
//! 4. Decide if this second counts: recent keyboard/mouse input, or a video playing
//!    where the rule allows that.

use std::collections::HashMap;
use std::ffi::c_void;

use windows::core::{w, Interface, PCWSTR, PWSTR};
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::Foundation::{CloseHandle, BOOL, FALSE, HWND, LPARAM, TRUE};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowTextLengthW,
    GetWindowThreadProcessId, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

use crate::config::Config;
use crate::{calls, server, util};

pub struct Activity {
    pub key: String,
    pub name: String,
    pub cat: usize,
    /// false = user is idle, so this second isn't being added.
    pub counting: bool,
    /// Full exe path for apps (used to pull out the app's logo).
    pub exe_path: Option<String>,
    pub pid: u32,
    /// true = counted because a call is using the microphone (see calls.rs).
    pub call: bool,
}

/// Foreground processes we never count: lock screen, Start menu, search, notification panels.
const IGNORED: &[&str] = &[
    "lockapp.exe",
    "applicationframehost.exe", // an empty Store-app frame (the real app wasn't found)
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "searchapp.exe",
    "shellexperiencehost.exe",
    "shellhost.exe",
];

/// explorer.exe draws the desktop and taskbar too. Only real File Explorer windows count.
const SHELL_WINDOW_CLASSES: &[&str] = &[
    "Progman",                             // desktop
    "WorkerW",                             // desktop (with wallpaper)
    "Shell_TrayWnd",                       // taskbar
    "Shell_SecondaryTrayWnd",              // taskbar on a second monitor
    "XamlExplorerHostIslandWindow",        // Alt+Tab / Task View
    "TopLevelWindowForOverflowXamlIsland", // hidden tray icons
    "NotifyIconOverflowWindow",
];

/// `names` caches "C:\...\foo.exe" -> "Foo App" so we read the exe's description only once.
pub fn detect(config: &Config, names: &mut HashMap<String, String>) -> Option<Activity> {
    let fg = foreground_exe();
    let fg_exe = fg.as_ref().map(|(p, _, _)| p.rsplit(['\\', '/']).next().unwrap_or(p).to_ascii_lowercase());
    if fg_exe.as_deref() == Some("lockapp.exe") {
        return None; // laptop locked: nothing counts, not even a call
    }

    // During a call, the call wins, whichever window or screen you're clicking on.
    let calls = calls::active();
    if !calls.is_empty() {
        let c = calls.iter().find(|c| Some(&c.exe) == fg_exe.as_ref()).unwrap_or(&calls[0]);
        if let Some(a) = call_activity(config, names, c) {
            return Some(a);
        }
    }

    let (path, pid, class) = fg?;
    let exe = fg_exe.unwrap_or_default();
    if IGNORED.contains(&exe.as_str()) || (exe == "explorer.exe" && SHELL_WINDOW_CLASSES.contains(&class.as_str())) {
        return None; // desktop, taskbar, Start menu...: "Not tracking"
    }

    let mut key = format!("app:{exe}");
    let mut site_name = None;
    let mut playing = false;
    if exe == "chrome.exe" {
        if let Some(b) = server::browser() {
            if let Some(site_key) = config.site_key(&b.url) {
                site_name = util::host_of(&b.url);
                key = site_key;
                playing = b.playing;
            }
        }
    }

    if config.is_ignored(&key) {
        return None;
    }
    let rule = config.rule_for(&key);
    let name = rule.name.or(site_name).unwrap_or_else(|| {
        names.entry(path.clone()).or_insert_with(|| describe(&path, &exe)).clone()
    });
    // Counting = you used keyboard/mouse recently, OR (where the rule allows) a video/audio is
    // playing: a video in a Chrome tab, or sound from the app in front (VLC, Zoom...).
    // The sound check only runs once you're past the idle time, so it costs nothing normally.
    let counting = idle_secs() < rule.idle_secs
        || (rule.video && (playing || (!key.starts_with("site:") && app_playing_sound(&exe))));
    let exe_path = key.starts_with("app:").then_some(path);
    Some(Activity { key, name, cat: rule.cat, counting, exe_path, pid, call: false })
}

/// The time goes to the call app: Zoom, Discord... or, for a call in Chrome, the site of the tab
/// that holds the microphone (e.g. meet.google.com, reported by the extension).
/// Always counting: you're in the call even when just listening.
fn call_activity(config: &Config, names: &mut HashMap<String, String>, c: &calls::CallApp) -> Option<Activity> {
    let mut key = format!("app:{}", c.exe);
    let mut site_name = None;
    if c.exe == "chrome.exe" {
        if let Some(url) = server::call_url() {
            if let Some(site_key) = config.site_key(&url) {
                site_name = util::host_of(&url);
                key = site_key;
            }
        }
    }
    if config.is_ignored(&key) {
        return None;
    }
    let rule = config.rule_for(&key);
    let name = rule.name.or(site_name).unwrap_or_else(|| {
        names.entry(c.path.clone()).or_insert_with(|| describe(&c.path, &c.exe)).clone()
    });
    let exe_path = key.starts_with("app:").then(|| c.path.clone());
    Some(Activity { key, name, cat: rule.cat, counting: true, exe_path, pid: c.pid, call: true })
}

/// Is any process with this exe name currently playing sound? Uses the same per-app audio
/// sessions that the Windows volume mixer shows. Matching by exe name (not process id) handles
/// apps that play audio from a helper process.
fn app_playing_sound(exe: &str) -> bool {
    unsafe {
        let Ok(devices) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return false;
        };
        let Ok(speaker) = devices.GetDefaultAudioEndpoint(eRender, eMultimedia) else { return false };
        let Ok(manager) = speaker.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { return false };
        let Ok(sessions) = manager.GetSessionEnumerator() else { return false };
        for i in 0..sessions.GetCount().unwrap_or(0) {
            let Ok(session) = sessions.GetSession(i) else { continue };
            if session.GetState().ok() != Some(AudioSessionStateActive) {
                continue;
            }
            let Ok(s2) = session.cast::<IAudioSessionControl2>() else { continue };
            let Ok(pid) = s2.GetProcessId() else { continue };
            let name = exe_path(pid).map(|p| p.rsplit(['\\', '/']).next().unwrap_or("").to_ascii_lowercase());
            if name.as_deref() == Some(exe) {
                return true;
            }
        }
        false
    }
}

/// Seconds since the last keyboard or mouse input anywhere on the system.
fn idle_secs() -> u64 {
    unsafe {
        let mut lii = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if !GetLastInputInfo(&mut lii).as_bool() {
            return 0;
        }
        (GetTickCount().wrapping_sub(lii.dwTime) / 1000) as u64
    }
}

/// (exe path, process id, window class) of the window in front.
fn foreground_exe() -> Option<(String, u32, String)> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = window_pid(hwnd);
        if pid == 0 || pid == GetCurrentProcessId() || is_desktop_overlay(hwnd) {
            return None;
        }
        let mut path = exe_path(pid)?;
        if path.to_ascii_lowercase().ends_with("\\applicationframehost.exe") {
            // Store apps (Settings, Calculator...) are hosted in a frame; the real app owns a child window.
            let mut found: (u32, u32) = (pid, 0);
            let _ = EnumChildWindows(hwnd, Some(find_real_child), LPARAM(&mut found as *mut _ as isize));
            if found.1 != 0 {
                if let Some(p) = exe_path(found.1) {
                    path = p;
                    pid = found.1;
                }
            }
        }
        let mut class = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut class).max(0) as usize;
        Some((path, pid, String::from_utf16_lossy(&class[..n])))
    }
}

/// Desktop widgets and overlays (clocks, weather, other widget apps) use untitled "tool
/// windows" that never appear on the taskbar. When you close an app, Windows sometimes hands
/// focus to one of them. They aren't something you're using, so they count as "Not tracking".
unsafe fn is_desktop_overlay(hwnd: HWND) -> bool {
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let tool = ex & WS_EX_TOOLWINDOW.0 != 0;
    let app_window = ex & WS_EX_APPWINDOW.0 != 0;
    tool && !app_window && GetWindowTextLengthW(hwnd) == 0
}

unsafe extern "system" fn find_real_child(child: HWND, lp: LPARAM) -> BOOL {
    let found = &mut *(lp.0 as *mut (u32, u32));
    let pid = window_pid(child);
    if pid != 0 && pid != found.0 {
        found.1 = pid;
        return FALSE;
    }
    TRUE
}

unsafe fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    pid
}

pub unsafe fn exe_path(pid: u32) -> Option<String> {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
    let _ = CloseHandle(h);
    r.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Friendly app name: the exe's "File description" (e.g. "Discord"), else "Notepad" from notepad.exe.
fn describe(path: &str, exe: &str) -> String {
    if exe == "explorer.exe" {
        return "File Explorer".into(); // its exe says "Windows Explorer"
    }
    if let Some(d) = unsafe { file_description(path) } {
        let d = d.trim();
        if !d.is_empty() {
            return d.to_string();
        }
    }
    let stem = exe.trim_end_matches(".exe");
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => exe.to_string(),
    }
}

unsafe fn file_description(path: &str) -> Option<String> {
    let wpath = util::wide(path);
    let p = PCWSTR(wpath.as_ptr());
    let size = GetFileVersionInfoSizeW(p, None);
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    GetFileVersionInfoW(p, 0, size, data.as_mut_ptr() as *mut c_void).ok()?;

    let mut ptr: *mut c_void = std::ptr::null_mut();
    let mut len = 0u32;
    let mut lang = (0x0409u16, 0x04B0u16);
    if VerQueryValueW(data.as_ptr() as *const c_void, w!("\\VarFileInfo\\Translation"), &mut ptr, &mut len)
        .as_bool()
        && len >= 4
    {
        let t = ptr as *const u16;
        lang = (*t, *t.add(1));
    }
    let sub = util::wide(&format!("\\StringFileInfo\\{:04x}{:04x}\\FileDescription", lang.0, lang.1));
    if !VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(sub.as_ptr()), &mut ptr, &mut len).as_bool()
        || len == 0
    {
        return None;
    }
    let s = std::slice::from_raw_parts(ptr as *const u16, len as usize);
    Some(String::from_utf16_lossy(s).trim_end_matches('\0').to_string())
}
