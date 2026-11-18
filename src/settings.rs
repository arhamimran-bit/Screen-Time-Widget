//! Widget preferences (position, always-on-top, lock) in `data\settings.json`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::util;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    pub x: Option<i32>,
    pub y: Option<i32>,
    #[serde(default)]
    pub topmost: bool,
    #[serde(default)]
    pub locked: bool,
    /// Which view the widget shows: today, last 7 days (default) or last 30 days.
    #[serde(default)]
    pub period: Period,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Period {
    Day,
    #[default]
    Week,
    Month,
}

impl Period {
    pub const ALL: [Period; 3] = [Period::Day, Period::Week, Period::Month];

    pub fn label(self) -> &'static str {
        match self {
            Period::Day => "Today",
            Period::Week => "7D",
            Period::Month => "30D",
        }
    }
}

fn path() -> PathBuf {
    util::app_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            util::write_atomic(&path(), &json);
        }
    }
}
