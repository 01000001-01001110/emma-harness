//! Today, as `YYYY-MM-DD`, without a date crate.
//!
//! There is no `chrono`, `time`, or `jiff` in this workspace's lock file, and
//! the wiki needs exactly one thing from a calendar: the date on a `created:`
//! line and a `log.md` header. That is Howard Hinnant's civil-from-days, which
//! is fifteen lines and exact for every year this program will see. Pulling a
//! date crate into the tree to format one string would be the more expensive
//! answer, not the safer one.
//!
//! **UTC, not local time.** A wiki is committed to a repository and read on
//! other machines; a log whose ordering depends on the reader's timezone is a
//! log that disagrees with itself across a team.

use std::time::{SystemTime, UNIX_EPOCH};

/// Year, month and day for the given Unix day number (days since 1970-01-01).
///
/// Split out of [`iso_from_days`] because a caller that wants to write the
/// same date a different way — `May 18` on a status line — must not own a
/// second copy of the calendar.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

/// `YYYY-MM-DD` for the given Unix day number (days since 1970-01-01).
pub fn iso_from_days(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today in UTC. A clock before the epoch reads as the epoch rather than
/// panicking: a wrong date on a memory is a blemish, a crash is a lost memory.
pub fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_from_days(secs.div_euclid(86_400))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendar_is_right_at_the_edges_a_naive_one_gets_wrong() {
        assert_eq!(iso_from_days(0), "1970-01-01");
        assert_eq!(iso_from_days(59), "1970-03-01");
        // 2000 is a leap year, 1900 was not, 2100 will not be.
        assert_eq!(iso_from_days(11_016), "2000-02-29");
        assert_eq!(iso_from_days(20_691), "2026-08-26");
        assert_eq!(iso_from_days(-1), "1969-12-31");
        assert_eq!(iso_from_days(47_541), "2100-03-01");
    }

    #[test]
    fn a_day_number_before_the_epoch_still_reads_as_a_date_rather_than_wrapping() {
        // `div_euclid` in `today` and the negative-era branch here are the two
        // halves of this. Integer division truncating toward zero would put
        // 1969-12-31 in 1970.
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!((-1i64).div_euclid(86_400), -1);
    }

    #[test]
    fn today_is_an_iso_date_shaped_string() {
        let t = today();
        assert_eq!(t.len(), 10, "{t}");
        assert!(t.starts_with("20"), "{t}");
        assert_eq!(t.matches('-').count(), 2, "{t}");
    }
}
