//! App and website logos for the breakdown list. One system, no per-app work:
//!
//! - **Websites**: the Chrome extension sends each site's favicon (from Chrome's own cache, no
//!   internet) to the widget, which saves it as `data\icons\<domain>.png`.
//! - **Apps**: we ask the Windows shell for the app's icon, the same way the Start menu and
//!   taskbar do. For Store/packaged apps (Settings, Calculator, WhatsApp...) the real logo is in
//!   the app package, not the .exe, so we use the app's "AppUserModelID" (`shell:AppsFolder\<id>`).
//!   For normal apps we use the .exe path. Each app's "icon source" is remembered in `apps.json`.
//!   Apps that have no source yet are filled in by looking at running processes.
//!
//! WIC (Windows Imaging Component) turns PNGs/bitmaps into pixels that Direct2D can draw.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, SIZE};
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap, ID2D1HwndRenderTarget};
use windows::Win32::Graphics::Gdi::{DeleteObject, HPALETTE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHDefExtractIconW, SIIGBF_BIGGERSIZEOK,
    SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

use crate::{tracker, util};

/// Set by the server thread when a new favicon arrives, so the UI retries missing icons.
pub static CHANGED: AtomicBool = AtomicBool::new(false);

pub fn dir() -> PathBuf {
    let d = util::app_dir().join("icons");
    let _ = fs::create_dir_all(&d);
    d
}

pub fn site_icon_path(domain: &str) -> PathBuf {
    dir().join(format!("{domain}.png"))
}

/// Only plain domain characters, so a bad header can't write outside the icons folder.
pub fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() < 200
        && !d.starts_with('.')
        && !d.contains("..")
        && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// Where to get a running process's logo: `shell:AppsFolder\<AppUserModelID>` for packaged
/// apps, otherwise its exe path.
pub fn icon_source(pid: u32, exe_path: &str) -> String {
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = GetApplicationUserModelId(h, &mut len, PWSTR(buf.as_mut_ptr())).is_ok();
            let _ = CloseHandle(h);
            if ok && len > 1 {
                let id = String::from_utf16_lossy(&buf[..len as usize - 1]);
                return format!("shell:AppsFolder\\{id}");
            }
        }
    }
    exe_path.to_string()
}

/// For app keys like "app:discord.exe" that have no icon source yet, look for a running
/// process with that exe name. Returns (key, source) pairs it could find.
pub fn find_running(keys: &[String]) -> Vec<(String, String)> {
    let wanted: HashSet<String> = keys.iter().filter_map(|k| k.strip_prefix("app:")).map(str::to_string).collect();
    let mut found = Vec::new();
    if wanted.is_empty() {
        return found;
    }
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return found };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut seen = HashSet::new();
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let exe = String::from_utf16_lossy(&e.szExeFile[..n]).to_ascii_lowercase();
            if wanted.contains(&exe) && seen.insert(exe.clone()) {
                if let Some(path) = tracker::exe_path(e.th32ProcessID) {
                    found.push((format!("app:{exe}"), icon_source(e.th32ProcessID, &path)));
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    found
}

/// app key ("app:code.exe") -> icon source (exe path or shell:AppsFolder\...), saved in apps.json.
pub struct AppPaths {
    map: HashMap<String, String>,
}

impl AppPaths {
    fn file() -> PathBuf {
        util::app_dir().join("apps.json")
    }

    pub fn load() -> AppPaths {
        let map = fs::read_to_string(Self::file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        AppPaths { map }
    }

    /// Returns true if this changed something (so the UI should reload icons).
    pub fn remember(&mut self, key: &str, source: &str) -> bool {
        if source.is_empty() || self.map.get(key).map(String::as_str) == Some(source) {
            return false;
        }
        self.map.insert(key.to_string(), source.to_string());
        if let Ok(json) = serde_json::to_vec_pretty(&self.map) {
            util::write_atomic(&Self::file(), &json);
        }
        true
    }
}

pub struct IconLoader {
    wic: IWICImagingFactory,
}

impl IconLoader {
    pub fn new() -> Option<IconLoader> {
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok().map(|wic| IconLoader { wic }) }
    }

    pub unsafe fn load(&self, rt: &ID2D1HwndRenderTarget, key: &str, paths: &AppPaths) -> Option<ID2D1Bitmap> {
        if let Some(site) = key.strip_prefix("site:") {
            let domain = site.split('/').next().unwrap_or(site); // "play.google.com/console" -> host
            return self.from_file(rt, &site_icon_path(domain));
        }
        let source = paths.map.get(key)?;
        self.from_shell(rt, source).or_else(|| self.from_exe(rt, source))
    }

    /// The icon Windows itself shows for this app (works for exe paths and shell:AppsFolder ids).
    unsafe fn from_shell(&self, rt: &ID2D1HwndRenderTarget, source: &str) -> Option<ID2D1Bitmap> {
        let w = util::wide(source);
        let item: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
        let hbm = item.GetImage(SIZE { cx: 32, cy: 32 }, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let bmp = self.wic.CreateBitmapFromHBITMAP(hbm, HPALETTE::default(), WICBitmapUsePremultipliedAlpha);
        let _ = DeleteObject(hbm);
        self.to_d2d(rt, &bmp.ok()?.cast().ok()?)
    }

    unsafe fn from_file(&self, rt: &ID2D1HwndRenderTarget, path: &PathBuf) -> Option<ID2D1Bitmap> {
        if !path.exists() {
            return None;
        }
        let w = util::wide(path.to_str()?);
        let decoder = self
            .wic
            .CreateDecoderFromFilename(PCWSTR(w.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        self.to_d2d(rt, &frame.cast().ok()?)
    }

    /// Fallback: the first icon inside the exe.
    unsafe fn from_exe(&self, rt: &ID2D1HwndRenderTarget, exe: &str) -> Option<ID2D1Bitmap> {
        let w = util::wide(exe);
        let mut icon = HICON::default();
        SHDefExtractIconW(PCWSTR(w.as_ptr()), 0, 0, Some(&mut icon), None, 32).ok().ok()?;
        if icon.is_invalid() {
            return None;
        }
        let bmp = self.wic.CreateBitmapFromHICON(icon);
        let _ = DestroyIcon(icon);
        self.to_d2d(rt, &bmp.ok()?.cast().ok()?)
    }

    unsafe fn to_d2d(&self, rt: &ID2D1HwndRenderTarget, src: &IWICBitmapSource) -> Option<ID2D1Bitmap> {
        let conv = self.wic.CreateFormatConverter().ok()?;
        conv.Initialize(
            src,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )
        .ok()?;
        rt.CreateBitmapFromWicBitmap(&conv, None).ok()
    }
}
