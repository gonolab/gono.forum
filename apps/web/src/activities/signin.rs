//! Check-in statistics: current streak, longest streak, this month, total, and this month's calendar.
//!
//! Check-in records are simply the credit ledger rows with `kind='signin'` (`event_key = signin:{uid}:{site day}`, with
//! `:{offset minutes}` appended when a check-in made before a time zone change already holds that key).
//! Check-in itself is refused when the check-in reward is off or the account has a pending account-deletion or
//! content-erasure request (cooling-off period), so "checked in but left no record" cannot happen; these rows are
//! therefore the complete check-in history and no separate table is needed. Days are split by calendar days in the
//! site time zone (`settings.time.offset_minutes`).
use super::*;
use chrono::{Datelike, NaiveDate};

/// Days from 0001-01-01 to 1970-01-01, used to turn "day number since 1970" into a [`NaiveDate`].
const UNIX_EPOCH_FROM_CE: i64 = 719_163;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SigninStats {
    /// Consecutive check-in days up to today; up to yesterday when today has no check-in yet, and 0 when yesterday has
    /// none either.
    pub current_streak: usize,
    pub longest_streak: usize,
    pub month_count: usize,
    pub total: usize,
    pub year: i32,
    pub month: u32,
    /// This month's calendar, each week starting on Monday; cells outside this month are `None`.
    pub weeks: Vec<Vec<Option<CalendarDay>>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CalendarDay {
    pub day: u32,
    pub checked: bool,
    pub today: bool,
}

fn date_of(day: i64) -> Option<NaiveDate> {
    NaiveDate::from_num_days_from_ce_opt(i32::try_from(day + UNIX_EPOCH_FROM_CE).ok()?)
}

fn day_of(date: NaiveDate) -> i64 {
    i64::from(date.num_days_from_ce()) - UNIX_EPOCH_FROM_CE
}

/// Computes the statistics from checked-in site days (days since 1970-01-01 in the site time zone, ascending,
/// duplicates allowed). A pure function, easy to unit-test.
pub fn signin_stats_from_days(days: &[i64], today: i64) -> SigninStats {
    let mut days = days.to_vec();
    days.sort_unstable();
    days.dedup();
    let mut longest = 0;
    let mut run = 0;
    let mut previous = None;
    for &day in &days {
        run = if previous == Some(day - 1) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        previous = Some(day);
    }
    // Count back from today (from yesterday if today has no check-in) until a gap.
    let mut current = 0;
    let mut expected = if days.binary_search(&today).is_ok() {
        today
    } else {
        today - 1
    };
    let start = expected;
    for &day in days.iter().rev().skip_while(|day| **day > start) {
        if day != expected {
            break;
        }
        current += 1;
        expected -= 1;
    }
    let date = date_of(today).unwrap_or_default();
    let first = date.with_day(1).unwrap_or(date);
    let next_month = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)
    }
    .unwrap_or(first);
    let (month_start, month_end) = (day_of(first), day_of(next_month));
    let month_count = days
        .iter()
        .filter(|day| (month_start..month_end).contains(*day))
        .count();
    let mut weeks = Vec::new();
    let mut week: Vec<Option<CalendarDay>> =
        vec![None; first.weekday().num_days_from_monday() as usize];
    for day in month_start..month_end {
        week.push(Some(CalendarDay {
            day: u32::try_from(day - month_start + 1).unwrap_or_default(),
            checked: days.binary_search(&day).is_ok(),
            today: day == today,
        }));
        if week.len() == 7 {
            weeks.push(std::mem::take(&mut week));
        }
    }
    if !week.is_empty() {
        week.resize(7, None);
        weeks.push(week);
    }
    SigninStats {
        current_streak: current,
        longest_streak: longest,
        month_count,
        total: days.len(),
        year: first.year(),
        month: first.month(),
        weeks,
    }
}

/// The member's own check-in statistics. Reads all check-in times at once: the longest streak needs the full history,
/// and the rows read equal the member's total check-in days (at most 366 a year). Uses
/// `gonof_credit_rule_daily (uid, kind, dateline)` as an index-only read without table lookups; sorting and
/// deduplication happen in code, and the SQL has no `ORDER BY`.
/// Days are split in the site time zone (`offset_minutes`); past check-ins are also regrouped by the current time zone,
/// the same way the check-in itself and the credits page decide whether today is checked in.
pub async fn signin_stats(
    db: &Database,
    uid: i64,
    now: i64,
    offset_minutes: i64,
) -> Result<SigninStats> {
    let datelines: Vec<i64> = crate::db::query_scalar(
        "SELECT dateline FROM gonof_common_credit_ledger WHERE uid=? AND kind='signin'",
    )
    .bind(uid)
    .fetch_all(db.executor())
    .await?;
    let day = |at| crate::clock::day_number(at, offset_minutes);
    let days: Vec<i64> = datelines.into_iter().map(day).collect();
    Ok(signin_stats_from_days(&days, day(now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> i64 {
        day_of(NaiveDate::from_ymd_opt(y, m, d).unwrap())
    }

    #[test]
    fn empty_history_still_renders_the_current_month() {
        let stats = signin_stats_from_days(&[], day(2026, 10, 31));
        assert_eq!(
            (
                stats.current_streak,
                stats.longest_streak,
                stats.month_count,
                stats.total
            ),
            (0, 0, 0, 0)
        );
        assert_eq!((stats.year, stats.month), (2026, 10));
        // 2026-10-01 is a Thursday: the first week starts with three empty cells, and 31 days fill 5 weeks.
        assert_eq!(stats.weeks.len(), 5);
        assert!(stats.weeks[0][..3].iter().all(Option::is_none));
        assert_eq!(stats.weeks[0][3].as_ref().unwrap().day, 1);
        let last = stats.weeks[4].iter().flatten().last().unwrap();
        assert_eq!((last.day, last.today), (31, true));
        assert!(stats.weeks.iter().all(|week| week.len() == 7));
    }

    #[test]
    fn streaks_break_on_gaps_and_cross_month_boundaries() {
        let today = day(2026, 11, 2);
        let days = [
            day(2026, 9, 1),
            day(2026, 9, 2),
            day(2026, 9, 3),
            day(2026, 9, 4),
            // The streak breaks on September 5.
            day(2026, 10, 30),
            day(2026, 10, 31),
            day(2026, 11, 1),
            day(2026, 11, 2),
            day(2026, 11, 2),
        ];
        let stats = signin_stats_from_days(&days, today);
        assert_eq!(
            stats.current_streak, 4,
            "the streak crosses the month: 10/30 to 11/2"
        );
        assert_eq!(stats.longest_streak, 4);
        assert_eq!(stats.month_count, 2, "November has only the 1st and 2nd");
        assert_eq!(stats.total, 8, "a repeated day counts once");
        assert_eq!((stats.year, stats.month), (2026, 11));
    }

    #[test]
    fn current_streak_counts_to_yesterday_until_today_is_signed() {
        let days = [day(2026, 10, 28), day(2026, 10, 29), day(2026, 10, 30)];
        assert_eq!(
            signin_stats_from_days(&days, day(2026, 10, 31)).current_streak,
            3
        );
        assert_eq!(
            signin_stats_from_days(&days, day(2026, 11, 1)).current_streak,
            0,
            "missing yesterday breaks the streak"
        );
        let longest = signin_stats_from_days(
            &[
                day(2026, 1, 1),
                day(2026, 1, 2),
                day(2026, 1, 3),
                day(2026, 10, 31),
            ],
            day(2026, 10, 31),
        );
        assert_eq!((longest.current_streak, longest.longest_streak), (1, 3));
    }

    #[test]
    fn day_boundary_is_the_site_midnight() {
        // 2026-10-31 15:59:59 UTC is 23:59:59 in UTC+8, and the next second is November 1; a UTC site is still on
        // October 31.
        let before = 1_793_462_399;
        let east8 = |at| crate::clock::day_number(at, 480);
        assert_eq!(east8(before), day(2026, 10, 31));
        assert_eq!(east8(before + 1), day(2026, 11, 1));
        assert_eq!(crate::clock::day_number(before + 1, 0), day(2026, 10, 31));
        let stats = signin_stats_from_days(&[east8(before)], east8(before + 1));
        assert_eq!(
            (stats.month, stats.month_count, stats.current_streak),
            (11, 0, 1)
        );
    }
}
