//! "During a call, the call wins."
//!
//! With a laptop + monitor you might be in a Zoom/Meet call on one screen while clicking around
//! on the other. Windows only tells us which window has focus, so the call would get no time.
//! Instead: while an app is using the **microphone** (it stays in use when you're muted), the
//! time goes to that call app, whatever window you click on.
//!
//! Windows records microphone use per app in the registry (the same data behind the mic icon on
//! the taskbar): `...\CapabilityAccessManager\ConsentStore\microphone`. An app is using the mic
//! right now when its `LastUsedTimeStop` is 0. Desktop apps live under `NonPackaged` (the key name
//! is the exe path with `#` instead of `\`); Store apps (WhatsApp, Teams...) are keyed by package.

use std::ffi::c_void;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_QWORD,
};

use crate::{icons, tracker, util};

const MIC_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone");

/// An app that is using the microphone right now.
pub struct CallApp {
    /// Lowercase exe name, e.g. "zoom.exe".
    pub exe: String,
    pub path: String,
    pub pid: u32,
}

enum MicUser {
    /// Desktop app: full exe path.
    Exe(String),
    /// Store app: package family name, e.g. "5319275A.WhatsAppDesktop_cv1g1gvanyjgm".
    Package(String),
}

/// Apps currently using the microphone. Cheap when nothing is (just a few registry reads).
pub fn active() -> Vec<CallApp> {
    let users = unsafe { mic_users() };
    if users.is_empty() {
        return Vec::new();
    }
    // Only trust apps that are actually running: if an app crashed mid-call, Windows never
    // writes its "stop" time, so the registry alone could say "in use" forever.
    let mut found = Vec::new();
    for (pid, exe) in running_processes() {
        for u in &users {
            let hit = match u {
                MicUser::Exe(path) => path.rsplit('\\').next().is_some_and(|n| n.eq_ignore_ascii_case(&exe)),
                MicUser::Package(pfn) => icons::aumid(pid).is_some_and(|id| id.starts_with(&format!("{pfn}!"))),
            };
            if hit && !found.iter().any(|c: &CallApp| c.exe == exe) {
                if let Some(path) = unsafe { tracker::exe_path(pid) } {
                    found.push(CallApp { exe: exe.clone(), path, pid });
                }
            }
        }
    }
    found
}

unsafe fn mic_users() -> Vec<MicUser> {
    let mut users = Vec::new();
    let mut root = HKEY::default();
    if RegOpenKeyExW(HKEY_CURRENT_USER, MIC_KEY, 0, KEY_READ, &mut root).is_err() {
        return users;
    }
    for name in subkeys(root) {
        if name.eq_ignore_ascii_case("NonPackaged") {
            let mut np = HKEY::default();
            if RegOpenKeyExW(root, w!("NonPackaged"), 0, KEY_READ, &mut np).is_ok() {
                for exe_key in subkeys(np) {
                    if in_use(np, &exe_key) {
                        users.push(MicUser::Exe(exe_key.replace('#', "\\")));
                    }
                }
                let _ = RegCloseKey(np);
            }
        } else if in_use(root, &name) {
            users.push(MicUser::Package(name));
        }
    }
    let _ = RegCloseKey(root);
    users
}

/// In use = started at some point and not stopped since.
unsafe fn in_use(key: HKEY, sub: &str) -> bool {
    qword(key, sub, w!("LastUsedTimeStop")) == Some(0) && qword(key, sub, w!("LastUsedTimeStart")).unwrap_or(0) > 0
}

unsafe fn qword(key: HKEY, sub: &str, value: PCWSTR) -> Option<u64> {
    let sub = util::wide(sub);
    let mut v = 0u64;
    let mut size = 8u32;
    RegGetValueW(
        key,
        PCWSTR(sub.as_ptr()),
        value,
        RRF_RT_REG_QWORD,
        None,
        Some(&mut v as *mut u64 as *mut c_void),
        Some(&mut size),
    )
    .is_ok()
    .then_some(v)
}

unsafe fn subkeys(key: HKEY) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0.. {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        if RegEnumKeyExW(key, i, PWSTR(buf.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None).is_err() {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    out
}

/// (pid, lowercase exe name) of every running process.
fn running_processes() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, String::from_utf16_lossy(&e.szExeFile[..n]).to_ascii_lowercase()));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}
