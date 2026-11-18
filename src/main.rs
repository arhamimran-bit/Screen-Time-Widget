// Release builds have no console window; debug builds keep one so println! works.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Screen Time Widget: entry point.
//!
//! Big picture:
//!   1. `server` starts a tiny background thread that listens on 127.0.0.1 for the Chrome extension.
//!   2. `ui::run` creates the widget window and runs the Windows *message loop* forever.
//!      Windows sends the window messages (paint, mouse click, timer...), and a 1-second timer
//!      calls `tracker` to see what's in the foreground and add time to today's totals.

mod autostart;
mod calls;
mod config;
mod dates;
mod icons;
mod server;
mod settings;
mod storage;
mod tracker;
mod ui;
mod util;

use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn main() {
    unsafe {
        // Only one copy may run, otherwise two copies would both count the same seconds.
        // Two checks: a named mutex, and (in case the first copy was started in a way that can't
        // see that mutex, e.g. as admin) whether a widget window is already on the screen.
        let mutex = CreateMutexW(None, true, w!("Local\\ScreenTimeWidget.SingleInstance"));
        if mutex.is_err() || GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        if FindWindowW(w!("ScreenTimeWidget"), None).is_ok_and(|h| !h.0.is_null()) {
            return;
        }
        let _mutex = mutex;
        // Draw sharply on high-DPI screens instead of letting Windows blur-scale us.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // COM is needed by ShellExecute (used to open the config file).
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    server::start();
    ui::run();
}
