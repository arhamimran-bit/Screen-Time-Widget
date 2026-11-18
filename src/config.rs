//! Categories and matching rules, loaded from `data\config.json` (in the project folder).
//! On first run that file is created from `config.default.json` (baked into the exe).

use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::util;

const DEFAULT: &str = include_str!("../config.default.json");

#[derive(Deserialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub day_starts_at_hour: u32,
    pub categories: Vec<Category>,
    pub other: Category,
}

#[derive(Deserialize, Clone)]
pub struct Category {
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub icon: String,
    /// "more" (going up is good, like Work), "less" (like Entertainment), or "" (neutral).
    /// Only used to colour trend arrows green/red.
    #[serde(default)]
    pub goal: String,
    #[serde(default = "default_idle")]
    pub idle_secs: u64,
    #[serde(default)]
    pub count_while_video_plays: bool,
    #[serde(default)]
    pub apps: Vec<AppRule>,
    #[serde(default)]
    pub sites: Vec<SiteRule>,
}

#[derive(Deserialize, Clone)]
pub struct AppRule {
    pub name: String,
    pub exe: String,
    pub idle_secs: Option<u64>,
    pub count_while_video_plays: Option<bool>,
}

#[derive(Deserialize, Clone)]
pub struct SiteRule {
    pub name: String,
    pub domain: String,
    pub idle_secs: Option<u64>,
    pub count_while_video_plays: Option<bool>,
}

fn default_idle() -> u64 {
    300
}

/// What the config says about one app or site.
pub struct Rule {
    /// Category index; `categories.len()` means "Other".
    pub cat: usize,
    /// Display name from the config, if the item is listed there.
    pub name: Option<String>,
    pub idle_secs: u64,
    /// Keep counting with no input while a video plays.
    pub video: bool,
}

/// "/console/u/1" is inside rule path "/console"; "/consoles" is not. Empty rule path = whole site.
fn path_matches(path: &str, rule_path: &str) -> bool {
    rule_path.is_empty() || path == rule_path || path.starts_with(&format!("{rule_path}/"))
}

pub fn path() -> PathBuf {
    util::app_dir().join("config.json")
}

pub fn modified() -> Option<SystemTime> {
    fs::metadata(path()).and_then(|m| m.modified()).ok()
}

impl Config {
    pub fn builtin() -> Config {
        serde_json::from_str(DEFAULT).expect("config.default.json is valid")
    }

    /// Read config.json (creating it on first run). Err holds a readable message.
    pub fn load() -> Result<Config, String> {
        let p = path();
        if !p.exists() {
            let _ = fs::write(&p, DEFAULT);
        }
        let text = fs::read_to_string(&p).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| format!("config.json: {e}"))
    }

    /// Key to store time under for a web page: "site:host", or "site:host/path" when a
    /// config rule names a path (e.g. "play.google.com/console" vs the Play Store itself).
    pub fn site_key(&self, url: &str) -> Option<String> {
        let (host, path) = util::split_url(url)?;
        let mut best: Option<String> = None;
        for s in self.categories.iter().flat_map(|c| &c.sites) {
            let Some((d, dp)) = util::split_url(&s.domain) else { continue };
            let host_hit = host == d || host.ends_with(&format!(".{d}"));
            if !dp.is_empty() && host_hit && path_matches(&path, &dp) && best.as_ref().map_or(true, |b| dp.len() > b.len()) {
                best = Some(dp);
            }
        }
        Some(format!("site:{host}{}", best.unwrap_or_default()))
    }

    /// The category insights are about: "Work" if it exists, else the first one.
    pub fn focus_cat(&self) -> usize {
        self.categories.iter().position(|c| c.name.eq_ignore_ascii_case("work")).unwrap_or(0)
    }

    /// Number of categories including "Other".
    pub fn n_cats(&self) -> usize {
        self.categories.len() + 1
    }

    pub fn cat(&self, i: usize) -> &Category {
        self.categories.get(i).unwrap_or(&self.other)
    }

    /// `key` is "app:code.exe" or "site:youtube.com".
    pub fn rule_for(&self, key: &str) -> Rule {
        if let Some(exe) = key.strip_prefix("app:") {
            for (i, c) in self.categories.iter().enumerate() {
                if let Some(a) = c.apps.iter().find(|a| a.exe.eq_ignore_ascii_case(exe)) {
                    return Rule {
                        cat: i,
                        name: Some(a.name.clone()),
                        idle_secs: a.idle_secs.unwrap_or(c.idle_secs),
                        video: a.count_while_video_plays.unwrap_or(c.count_while_video_plays),
                    };
                }
            }
        } else if let Some(site) = key.strip_prefix("site:") {
            // A site key is "host" or "host/path" (see `site_key`).
            let (host, path) = match site.find('/') {
                Some(i) => (&site[..i], &site[i..]),
                None => (site, ""),
            };
            // Most specific match wins, so "keep.google.com" beats "google.com".
            let mut best: Option<(usize, &SiteRule, usize)> = None;
            for (i, c) in self.categories.iter().enumerate() {
                for s in &c.sites {
                    let Some((d, dp)) = util::split_url(&s.domain) else { continue };
                    let hit = (host == d || host.ends_with(&format!(".{d}"))) && path_matches(path, &dp);
                    let score = d.len() + dp.len();
                    if hit && best.map_or(true, |(_, _, len)| score > len) {
                        best = Some((i, s, score));
                    }
                }
            }
            if let Some((i, s, _)) = best {
                let c = &self.categories[i];
                return Rule {
                    cat: i,
                    name: Some(s.name.clone()),
                    idle_secs: s.idle_secs.unwrap_or(c.idle_secs),
                    video: s.count_while_video_plays.unwrap_or(c.count_while_video_plays),
                };
            }
        }
        Rule {
            cat: self.categories.len(),
            name: None,
            idle_secs: self.other.idle_secs,
            video: self.other.count_while_video_plays,
        }
    }
}
