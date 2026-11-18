//! Tiny calendar-date type (no external crates). Dates are converted to a day number
//! (days since 1970-01-01) so "add 7 days" or "which weekday" is just arithmetic.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Date {
    pub y: i32,
    pub m: u32,
    pub d: u32,
}

impl Date {
    /// "2026-11-17" -> Date
    pub fn parse(s: &str) -> Option<Date> {
        let mut it = s.split('-');
        let date = Date {
            y: it.next()?.parse().ok()?,
            m: it.next()?.parse().ok()?,
            d: it.next()?.parse().ok()?,
        };
        (1..=12).contains(&date.m).then_some(date)
    }

    /// Days since 1970-01-01 (Howard Hinnant's algorithm).
    pub fn days(self) -> i64 {
        let y = self.y as i64 - (self.m <= 2) as i64;
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let m = self.m as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + self.d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }

    pub fn from_days(z: i64) -> Date {
        let z = z + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let y = (yoe + era * 400 + (m <= 2) as i64) as i32;
        Date { y, m, d }
    }

    pub fn add(self, n: i64) -> Date {
        Date::from_days(self.days() + n)
    }

}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_add() {
        let d = Date::parse("2026-11-17").unwrap();
        assert_eq!(Date::from_days(d.days()), d);
        assert_eq!(Date::parse("2028-03-01").unwrap().add(-1).to_string(), "2028-02-29");
        assert_eq!(Date::parse("2026-03-01").unwrap().add(-1).to_string(), "2026-02-28");
        assert_eq!(Date::parse("2026-12-31").unwrap().add(1).to_string(), "2027-01-01");
    }
}
