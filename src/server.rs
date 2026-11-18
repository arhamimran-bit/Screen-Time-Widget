//! A tiny HTTP listener on 127.0.0.1 (only reachable from this laptop).
//! The Chrome extension POSTs `{"id", "url", "playing", "focused"}` here whenever the active tab
//! changes or a video starts/stops. The tracker reads the latest value each second.
//!
//! Each Chrome profile (work, fun, ...) runs its own copy of the extension with its own `id`.
//! Only the profile whose window is actually focused matters, so we keep one entry per profile
//! and pick the focused one.

use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use crate::icons;
use std::thread;
use std::time::{Duration, Instant};

pub const PORT: u16 = 47613;

#[derive(Deserialize, Clone, Default)]
pub struct BrowserState {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub playing: bool,
    #[serde(default = "yes")]
    pub focused: bool,
    /// URL of a tab (in any window) that's using the microphone: a Meet/Zoom-web call.
    #[serde(default)]
    pub call_url: String,
    /// URL of a tab (in any window) that's playing a video/audio right now.
    #[serde(default)]
    pub media_url: String,
}

fn yes() -> bool {
    true
}

/// One entry per Chrome profile: (state, when it was reported).
static PROFILES: Mutex<Vec<(BrowserState, Instant)>> = Mutex::new(Vec::new());

/// State of the Chrome profile whose window is focused (falls back to the latest report).
pub fn browser() -> Option<BrowserState> {
    let list = PROFILES.lock().ok()?;
    list.iter()
        .filter(|(s, _)| s.focused)
        .max_by_key(|(_, at)| *at)
        .or_else(|| list.iter().max_by_key(|(_, at)| *at))
        .map(|(s, _)| s.clone())
}

/// The tab holding the microphone, from whichever Chrome profile reported one most recently.
/// Only used while Windows confirms Chrome is actually using the mic.
pub fn call_url() -> Option<String> {
    let list = PROFILES.lock().ok()?;
    list.iter().filter(|(s, _)| !s.call_url.is_empty()).max_by_key(|(_, at)| *at).map(|(s, _)| s.call_url.clone())
}

/// The tab playing a video, from the latest Chrome report. Only trusted if it's fresh: the
/// extension reports at least every 30s, so an older one means Chrome was closed.
pub fn media_url() -> Option<String> {
    let list = PROFILES.lock().ok()?;
    list.iter()
        .filter(|(s, at)| !s.media_url.is_empty() && at.elapsed() < Duration::from_secs(70))
        .max_by_key(|(_, at)| *at)
        .map(|(s, _)| s.media_url.clone())
}

fn store(state: BrowserState) {
    let Ok(mut list) = PROFILES.lock() else { return };
    let now = Instant::now();
    // Forget profiles that haven't reported for 10 minutes (Chrome window closed).
    list.retain(|(s, at)| s.id != state.id && now.duration_since(*at) < Duration::from_secs(600));
    list.push((state, now));
}

pub fn start() {
    thread::spawn(|| {
        let Ok(listener) = TcpListener::bind(("127.0.0.1", PORT)) else { return };
        for stream in listener.incoming().flatten() {
            let _ = handle(stream);
        }
    });
}

fn handle(mut s: TcpStream) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let (head, body_start) = loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break (String::from_utf8_lossy(&buf[..i]).to_string(), i + 4);
        }
        if buf.len() > 16 * 1024 {
            return Ok(());
        }
    };

    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut content_length = 0usize;
    let mut from_extension = false;
    let mut domain = String::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            if k == "content-length" {
                content_length = v.trim().parse().unwrap_or(0).min(256 * 1024);
            } else if k == "x-stw" {
                from_extension = true;
            } else if k == "x-domain" {
                domain = v.trim().to_ascii_lowercase();
            }
        }
    }

    if request_line.starts_with("GET / ") {
        // Open http://127.0.0.1:47613/ in a browser to check the widget is running.
        return respond(&mut s, "200 OK", "Screen Time Widget is running.");
    }
    // The custom X-STW header means ordinary web pages can't send this without CORS permission.
    let is_state = request_line.starts_with("POST /state ");
    let is_icon = request_line.starts_with("POST /icon ");
    if !(is_state || is_icon) || !from_extension {
        return respond(&mut s, "404 Not Found", "");
    }

    while buf.len() < body_start + content_length {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let end = (body_start + content_length).min(buf.len());
    let body = &buf[body_start..end];

    if is_icon {
        // A site's favicon (PNG bytes) from Chrome's cache.
        if !icons::valid_domain(&domain) || body.len() < 8 || &body[1..4] != b"PNG" {
            return respond(&mut s, "400 Bad Request", "");
        }
        if !crate::util::write_atomic(&icons::site_icon_path(&domain), body) {
            return respond(&mut s, "500 Internal Server Error", "");
        }
        icons::CHANGED.store(true, Ordering::Relaxed);
    } else if let Ok(state) = serde_json::from_slice::<BrowserState>(body) {
        // Ask the extension for this site's logo whenever we don't have it yet, so a missing
        // or failed logo fixes itself on the next visit.
        let need_icon = crate::util::host_of(&state.url)
            .is_some_and(|h| icons::valid_domain(&h) && !icons::site_icon_path(&h).exists());
        store(state);
        if need_icon {
            return respond(&mut s, "200 OK", "need_icon");
        }
    }
    respond(&mut s, "200 OK", "ok")
}

fn respond(s: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
