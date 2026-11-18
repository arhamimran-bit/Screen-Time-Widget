//! Today's totals, saved as one small JSON file per day:
//! `data\days\2026-11-17.json` (in the project folder).
//!
//! Each app/site gets its total seconds plus a 24-slot array (seconds per hour) for the chart.
//! Categories are NOT stored: they're worked out from config.json when drawing, so editing
//! the config re-sorts today's history too.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime};

use crate::util;

#[derive(Serialize, Deserialize, Default)]
pub struct Day {
    pub date: String,
    /// key ("app:code.exe" / "site:youtube.com") -> time
    pub items: BTreeMap<String, ItemTime>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ItemTime {
    pub name: String,
    pub secs: u64,
    pub hours: Vec<u32>,
}

fn days_dir() -> PathBuf {
    let d = util::app_dir().join("days");
    let _ = fs::create_dir_all(&d);
    d
}

impl Day {
    pub fn load(date: &str) -> Day {
        let path = days_dir().join(format!("{date}.json"));
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Day>(&s).ok())
            .filter(|d| d.date == date)
            .unwrap_or_else(|| Day { date: date.to_string(), items: BTreeMap::new() })
    }

    /// Saved as normal JSON, laid out one app/site per line so it's compact and easy to read:
    /// ```text
    /// {
    ///   "date": "2026-11-18",
    ///   "items": {
    ///     "app:code.exe": {"name":"VS Code","secs":5400,"hours":[0,0,...]},
    ///   }
    /// }
    /// ```
    pub fn save(&self) {
        util::write_atomic(&days_dir().join(format!("{}.json", self.date)), self.to_json().as_bytes());
    }

    fn to_json(&self) -> String {
        // Serializing strings and plain structs can't fail.
        let mut out = format!("{{\n  \"date\": {},\n  \"items\": {{", serde_json::to_string(&self.date).unwrap_or_default());
        for (i, (key, item)) in self.items.iter().enumerate() {
            out.push_str(if i == 0 { "\n    " } else { ",\n    " });
            let k = serde_json::to_string(key).unwrap_or_default();
            let v = serde_json::to_string(item).unwrap_or_default();
            out.push_str(&format!("{k}: {v}"));
        }
        out.push_str("\n  }\n}\n");
        out
    }

    pub fn add(&mut self, key: &str, name: &str, slot: usize, secs: u64) {
        let it = self.items.entry(key.to_string()).or_insert_with(|| ItemTime {
            name: name.to_string(),
            secs: 0,
            hours: vec![0; 24],
        });
        if it.hours.len() != 24 {
            it.hours.resize(24, 0);
        }
        it.name = name.to_string();
        it.secs += secs;
        it.hours[slot.min(23)] += secs as u32;
    }
}

/// The earliest day with saved data ("YYYY-MM-DD"), used so weekly/monthly averages
/// don't count days from before the widget was installed.
pub fn first_day() -> Option<String> {
    fs::read_dir(days_dir())
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
        .filter(|name| name.len() == 10)
        .min()
}

/// Returns ("YYYY-MM-DD", chart slot 0..24) for right now.
/// If the day starts at e.g. 4 AM, then 2 AM still belongs to yesterday.
pub fn logical_now(day_start_hour: u32) -> (String, usize) {
    let start = day_start_hour.min(23);
    let now = unsafe { GetLocalTime() };
    let slot = ((now.wHour as u32 + 24 - start) % 24) as usize;
    let d = if (now.wHour as u32) < start { minus_one_day(now) } else { now };
    (format!("{:04}-{:02}-{:02}", d.wYear, d.wMonth, d.wDay), slot)
}

pub fn now_minute() -> u32 {
    unsafe { GetLocalTime() }.wMinute as u32
}

fn minus_one_day(st: SYSTEMTIME) -> SYSTEMTIME {
    unsafe {
        let mut ft = FILETIME::default();
        if SystemTimeToFileTime(&st, &mut ft).is_err() {
            return st;
        }
        // FILETIME counts 100-nanosecond ticks.
        let t = ((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64) - 864_000_000_000;
        let ft = FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
        let mut out = SYSTEMTIME::default();
        if FileTimeToSystemTime(&ft, &mut out).is_err() {
            return st;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_layout_loads_back_identically() {
        let mut d = Day { date: "2026-11-18".into(), items: BTreeMap::new() };
        d.add("app:code.exe", "VS Code", 21, 5400);
        d.add("site:youtube.com", "Yo\"uTube", 3, 60);
        let back: Day = serde_json::from_str(&d.to_json()).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), serde_json::to_string(&d).unwrap());
        // Old pretty-printed files must keep loading too.
        let old: Day = serde_json::from_str(&serde_json::to_string_pretty(&d).unwrap()).unwrap();
        assert_eq!(old.items["app:code.exe"].secs, 5400);
        let empty = Day { date: "2026-11-19".into(), items: BTreeMap::new() };
        assert!(serde_json::from_str::<Day>(&empty.to_json()).is_ok());
    }
}
