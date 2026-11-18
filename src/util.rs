//! Small helpers shared by the other modules.

use std::fs;
use std::path::{Path, PathBuf};

/// Rust string -> null-terminated UTF-16, which is what Windows APIs expect.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Where config, settings and daily data live: the `data` folder inside the project
/// (the path is baked in at build time), so the user can browse it from VS Code.
/// Set the `STW_DATA_DIR` environment variable to use another folder (for testing with fake data).
pub fn app_dir() -> PathBuf {
    let dir = std::env::var_os("STW_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("data"));
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Write via a temp file + rename, so a crash mid-write never leaves a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, bytes).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// 11520 -> "3h 12m", 300 -> "5m", 20 -> "<1m".
pub fn fmt_dur(secs: u64) -> String {
    let mins = secs / 60;
    if secs > 0 && secs < 60 {
        "<1m".to_string()
    } else if mins >= 60 {
        format!("{}h {}m", mins / 60, mins % 60)
    } else {
        format!("{}m", mins)
    }
}

/// "https://www.YouTube.com/watch?v=1" -> "youtube.com". Also accepts a bare "youtube.com".
/// Returns None for non-web pages like chrome://newtab.
pub fn host_of(url: &str) -> Option<String> {
    split_url(url).map(|(host, _)| host)
}

/// "https://play.google.com/console/u/1?x" -> ("play.google.com", "/console/u/1").
/// The path has no trailing slash ("" for the home page).
pub fn split_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let rest = if let Some(i) = url.find("://") {
        let scheme = &url[..i];
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return None;
        }
        &url[i + 3..]
    } else {
        url
    };
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host.rsplit('@').next()?; // drop "user:pass@"
    let host = host.split(':').next()?; // drop ":3000"
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    if host.is_empty() {
        None
    } else {
        Some((host, path.trim_end_matches('/').to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(host_of("https://www.YouTube.com/watch?v=1").as_deref(), Some("youtube.com"));
        assert_eq!(host_of("chrome://newtab/"), None);
        assert_eq!(
            split_url("https://play.google.com/console/u/1/home?pli=1"),
            Some(("play.google.com".into(), "/console/u/1/home".into()))
        );
        assert_eq!(split_url("play.google.com/console"), Some(("play.google.com".into(), "/console".into())));
        assert_eq!(split_url("roadmap.sh"), Some(("roadmap.sh".into(), "".into())));
    }
}
