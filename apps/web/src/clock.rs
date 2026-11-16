//! The single entry point for site time: display, forms and settling by "day" and "week" all live here.
//!
//! **Timestamps are always recorded in UTC**: the database holds `i64` seconds, and write paths never apply a time
//! zone offset. The time zone only takes effect at
//! three boundaries — display ([`datetime`], [`date`], [`short_datetime`]), forms
//! ([`input_value`], [`parse_input`]), and the day boundary for settling ([`day_number`], [`day_start`],
//! [`week_start`]). The whole site has a single site time zone `settings.time.offset_minutes` (all forum times follow
//! it; members have no personal time zone), and all three boundaries use it; otherwise an administrator filling a
//! form with the time seen on the page would save a value off by one time zone, and the "today" of quotas would not
//! match the "today" on the page.
//!
//! Settling only looks at the offset, not at the date style or clock format. Write transactions take the offset from
//! the settings read by the write gate (the same copy as the quotas); read-only pages take it from the process cache.
//! On the day the site time zone changes, the "day" shifts as a whole and a check-in may happen once more; accepted.
use chrono::{DateTime, FixedOffset, NaiveDateTime};

/// Time zones offered for the admin "Site time zone" setting.
///
/// None of them uses daylight saving time, so a fixed offset behaves the same as the IANA database and chrono-tz is
/// not needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zone {
    pub name: &'static str,
    pub offset_minutes: i32,
}

pub const SITE_ZONES: &[Zone] = &[
    Zone {
        name: "UTC",
        offset_minutes: 0,
    },
    Zone {
        name: "Asia/Shanghai",
        offset_minutes: 480,
    },
    Zone {
        name: "Asia/Hong_Kong",
        offset_minutes: 480,
    },
    Zone {
        name: "Asia/Taipei",
        offset_minutes: 480,
    },
    Zone {
        name: "Asia/Singapore",
        offset_minutes: 480,
    },
    Zone {
        name: "Asia/Tokyo",
        offset_minutes: 540,
    },
    Zone {
        name: "Asia/Seoul",
        offset_minutes: 540,
    },
    Zone {
        name: "Asia/Bangkok",
        offset_minutes: 420,
    },
    Zone {
        name: "Asia/Jakarta",
        offset_minutes: 420,
    },
    Zone {
        name: "Asia/Kolkata",
        offset_minutes: 330,
    },
    Zone {
        name: "Asia/Dubai",
        offset_minutes: 240,
    },
];

/// One item of the admin dropdown: an offset and the names of the time zones using it.
///
/// The setting stores only the offset, not a zone name (four zones in [`SITE_ZONES`] are UTC+8), so one item is
/// built per offset: whichever name is picked, the stored value is the same, and four separate items would only make
/// the form show a different name after saving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneChoice {
    pub offset_minutes: i64,
    pub label: String,
}

/// Options of the site time zone dropdown, ordered by offset ascending. When `current` is not in the list (the
/// database was edited directly) an extra "current value" item is added, so the dropdown does not default to the first item
/// and quietly change the time zone on save.
pub fn zone_choices(current: i64) -> Vec<ZoneChoice> {
    let t = crate::i18n::current();
    let mut offsets: Vec<i32> = SITE_ZONES.iter().map(|zone| zone.offset_minutes).collect();
    offsets.sort_unstable();
    offsets.dedup();
    let mut choices: Vec<ZoneChoice> = offsets
        .into_iter()
        .map(|offset| {
            let names: Vec<&str> = SITE_ZONES
                .iter()
                .filter(|zone| zone.offset_minutes == offset && zone.name != "UTC")
                .map(|zone| zone.name)
                .collect();
            let label = if names.is_empty() {
                zone_label(offset)
            } else {
                format!(
                    "{}{}",
                    zone_label(offset),
                    t.common_parenthesized(names.join(t.common_list_separator()))
                )
            };
            ZoneChoice {
                offset_minutes: i64::from(offset),
                label,
            }
        })
        .collect();
    if !choices
        .iter()
        .any(|choice| choice.offset_minutes == current)
        && let Ok(offset) = i32::try_from(current)
    {
        choices.push(ZoneChoice {
            offset_minutes: current,
            label: format!(
                "{}{}",
                zone_label(offset),
                t.common_parenthesized(t.config_time_zone_current())
            ),
        });
    }
    choices
}

/// "UTC+8", "UTC+5:30", "UTC-3", "UTC".
pub fn zone_label(offset_minutes: i32) -> String {
    if offset_minutes == 0 {
        return "UTC".to_owned();
    }
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let minutes = offset_minutes.unsigned_abs();
    match minutes % 60 {
        0 => format!("UTC{sign}{}", minutes / 60),
        rest => format!("UTC{sign}{}:{rest:02}", minutes / 60),
    }
}

/// Calendar day number in the site time zone: 1970-01-01 in the site time zone is 0. Used by the check-in
/// idempotency key `signin:{uid}:{day}`.
///
/// `offset_minutes` comes from `settings.time.offset_minutes` (settings decoding already limits it to -720..=840).
pub fn day_number(timestamp: i64, offset_minutes: i64) -> i64 {
    timestamp
        .saturating_add(offset_minutes.saturating_mul(60))
        .div_euclid(86_400)
}

/// UTC timestamp of midnight of the calendar day containing `timestamp` in the site time zone;
/// `[day_start, day_start + 86_400)` is "today". Daily quotas, daily caps and "Earned today" all use it.
pub fn day_start(timestamp: i64, offset_minutes: i64) -> i64 {
    day_number(timestamp, offset_minutes)
        .saturating_mul(86_400)
        .saturating_sub(offset_minutes.saturating_mul(60))
}

/// UTC timestamp of midnight on this week's Monday in the site time zone (weeks start on Monday). 1970-01-01 in the
/// site time zone is a Thursday, so the day number plus 3, modulo 7, is the number of days since this Monday.
pub fn week_start(timestamp: i64, offset_minutes: i64) -> i64 {
    let day = day_number(timestamp, offset_minutes);
    (day - (day + 3).rem_euclid(7))
        .saturating_mul(86_400)
        .saturating_sub(offset_minutes.saturating_mul(60))
}

/// Time zone and format shared by the display and form boundaries.
///
/// It is serializable so it can be encoded into cache keys: the cache stores time strings already formatted with it,
/// and changing the time zone or style does not change the content version, so without it in the key the old style
/// would keep being served.
///
/// It also carries the language (docs/architecture.md, "Languages"): relative times and the `locale` date style are
/// written in it, so every cache keyed by the format is kept per language without further changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TimeFormat {
    /// The interface language the words of relative times and the `locale` date style are written in.
    pub lang: crate::i18n::Lang,
    /// Offset from UTC in minutes. UTC+8 is 480.
    pub offset_minutes: i32,
    pub date: DateStyle,
    pub clock24: bool,
    /// Times no more than this many hours ago are shown as "3 hours ago"; `None` means always absolute time.
    pub relative_hours: Option<i64>,
}

/// Date style. An enum is stored instead of a `chrono` format string — storing a format string directly would let
/// administrators inject arbitrary input into `chrono::format`.
///
/// `Locale` follows the language (ICU4X); the other three are fixed patterns, the same in
/// every language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize)]
pub enum DateStyle {
    #[default]
    YmdDash,
    YmdChinese,
    MdySlash,
    Locale,
}

impl DateStyle {
    pub fn parse(value: &str) -> Self {
        match value {
            "ymd-cn" => Self::YmdChinese,
            "mdy-slash" => Self::MdySlash,
            "locale" => Self::Locale,
            _ => Self::YmdDash,
        }
    }
    /// The chrono pattern of the fixed styles; `None` for [`Self::Locale`]. `YmdChinese` writes the Chinese year, month
    /// and day signs (U+5E74, U+6708, U+65E5) after the numbers.
    fn full(self) -> Option<&'static str> {
        match self {
            Self::YmdDash => Some("%Y-%m-%d"),
            Self::YmdChinese => Some("%Y\u{5e74}%m\u{6708}%d\u{65e5}"),
            Self::MdySlash => Some("%m/%d/%Y"),
            Self::Locale => None,
        }
    }
    /// Compact style for lists, without the year.
    fn short(self) -> Option<&'static str> {
        match self {
            Self::YmdDash => Some("%m-%d"),
            Self::YmdChinese => Some("%m\u{6708}%d\u{65e5}"),
            Self::MdySlash => Some("%m/%d"),
            Self::Locale => None,
        }
    }
}

impl Default for TimeFormat {
    fn default() -> Self {
        Self {
            lang: crate::i18n::Lang::EN,
            offset_minutes: 8 * 60,
            date: DateStyle::YmdDash,
            clock24: true,
            relative_hours: None,
        }
    }
}

impl From<&crate::runtime::SiteSettings> for TimeFormat {
    fn from(settings: &crate::runtime::SiteSettings) -> Self {
        Self {
            // A request writes in its own language (another domain may override the site's); background work in the
            // site language.
            lang: crate::site::request_language().unwrap_or_else(|| settings.site.lang()),
            // Settings decoding already clamps the offset to -720..=840 minutes, so converting to i32 cannot
            // truncate.
            offset_minutes: settings.time.offset_minutes as i32,
            date: DateStyle::parse(&settings.time.date_format),
            clock24: settings.time.clock24,
            relative_hours: settings
                .time
                .relative
                .then_some(settings.time.relative_hours),
        }
    }
}

impl TimeFormat {
    /// Falls back to UTC when the offset is outside what chrono can represent, so one bad setting cannot wipe out
    /// every time on the page.
    fn zone(self) -> FixedOffset {
        FixedOffset::east_opt(self.offset_minutes * 60)
            .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC offset is always valid"))
    }

    fn at(self, timestamp: i64) -> Option<DateTime<FixedOffset>> {
        DateTime::from_timestamp(timestamp, 0).map(|t| t.with_timezone(&self.zone()))
    }

    fn time_part(self) -> &'static str {
        if self.clock24 { "%H:%M" } else { "%I:%M %p" }
    }

    /// One moment in this format: a fixed chrono pattern (`pattern` picks the date part from the style), or ICU4X for
    /// the `locale` style.
    fn write(
        self,
        timestamp: i64,
        pattern: fn(DateStyle) -> Option<&'static str>,
        field: crate::i18n::DateField,
        with_time: bool,
    ) -> String {
        let Some(at) = self.at(timestamp) else {
            return String::new();
        };
        match pattern(self.date) {
            Some(date) if with_time => at
                .format(&format!("{date} {}", self.time_part()))
                .to_string(),
            Some(date) => at.format(date).to_string(),
            None => self.lang.date(field, self.clock24, at.naive_local()),
        }
    }
}

/// The "3 hours ago" style when within `relative_hours` of now, otherwise `None`.
///
/// `now` is given by the caller instead of taken here: several times on one page must use the same reference point,
/// otherwise a render crossing a minute boundary would show "1 minute ago" next to "just now".
fn relative(timestamp: i64, now: i64, format: &TimeFormat) -> Option<String> {
    let window = format.relative_hours?;
    let elapsed = now.checked_sub(timestamp)?;
    // Only past times are handled; future times (activity deadlines etc.) always use the absolute style.
    if !(0..window.saturating_mul(3600)).contains(&elapsed) {
        return None;
    }
    let lang = format.lang;
    Some(match elapsed {
        ..60 => lang.time_just_now().to_owned(),
        60..3600 => lang.time_minutes_ago(elapsed / 60),
        _ => lang.time_hours_ago(elapsed / 3600),
    })
}

tokio::task_local! {
    static FORMAT: std::cell::Cell<TimeFormat>;
}

/// Sets the time format for this request. Middleware wraps the whole request, and `SiteFrame` [`set`]s it once it
/// has the site settings.
///
/// A task-local is used instead of passing it down layer by layer: time display is a cross-cutting concern, threading
/// it through would touch dozens of signatures, and most call sites render times into strings while decoding rows,
/// which would mean carrying the format all the way into query functions.
/// `metrics` does the same for the same reason.
pub async fn scope<F: std::future::Future>(future: F) -> F::Output {
    FORMAT
        .scope(std::cell::Cell::new(TimeFormat::default()), future)
        .await
}

/// Writes the format for this request. Outside a request (worker, CLI) it is silently ignored; those places use the
/// default.
pub fn set(format: TimeFormat) {
    let _ = FORMAT.try_with(|cell| cell.set(format));
}

/// The format of this request; the default outside a request.
pub fn current() -> TimeFormat {
    FORMAT.try_with(std::cell::Cell::get).unwrap_or_default()
}

/// Date and time in lists and detail pages. Returns an empty string when the timestamp cannot be represented; the
/// caller decides what placeholder to use.
///
/// With relative time on and within range it gives "3 hours ago"; otherwise always the absolute style.
pub fn datetime(timestamp: i64, format: &TimeFormat) -> String {
    if let Some(text) = relative(timestamp, crate::identity::now(), format) {
        return text;
    }
    format.write(
        timestamp,
        DateStyle::full,
        crate::i18n::DateField::DateTime,
        true,
    )
}

/// For places that only need the date (registration date etc.). Dates never use the relative style.
pub fn date(timestamp: i64, format: &TimeFormat) -> String {
    format.write(
        timestamp,
        DateStyle::full,
        crate::i18n::DateField::Date,
        false,
    )
}

/// "Last seen" on the profile page is only precise to the day: by calendar day in the site
/// time zone it shows "today", "yesterday", "N days ago" (within 30 days), and the date for anything earlier.
/// Future times (clock skew) count as today.
pub fn day_label(timestamp: i64, now: i64, format: &TimeFormat) -> String {
    let (Some(then), Some(today)) = (format.at(timestamp), format.at(now)) else {
        return String::new();
    };
    let lang = format.lang;
    match (today.date_naive() - then.date_naive()).num_days() {
        ..=0 => lang.time_today().to_owned(),
        1 => lang.time_yesterday().to_owned(),
        days @ 2..=30 => lang.time_days_ago(days),
        _ => date(timestamp, format),
    }
}

/// The `datetime` of a `<time>` element and the times of the page data block (docs/skins.md, "Times"): the
/// instant in UTC to the second, `2026-11-16T05:27:55Z`. Empty when the timestamp cannot be represented.
pub fn iso(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// The compact style without the year, as used in thread lists.
pub fn short_datetime(timestamp: i64, format: &TimeFormat) -> String {
    if let Some(text) = relative(timestamp, crate::identity::now(), format) {
        return text;
    }
    format.write(
        timestamp,
        DateStyle::short,
        crate::i18n::DateField::Short,
        true,
    )
}

/// Initial value for `<input type="datetime-local">`.
pub fn input_value(timestamp: i64, format: &TimeFormat) -> String {
    format
        .at(timestamp)
        .map(|t| t.format("%Y-%m-%dT%H:%M").to_string())
        .unwrap_or_default()
}

/// Converts the wall-clock time submitted by `<input type="datetime-local">` back to a UTC timestamp using the site
/// time zone.
pub fn parse_input(value: &str, format: &TimeFormat) -> Option<i64> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
        .ok()?
        .and_local_timezone(format.zone())
        .single()
        .map(|t| t.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-16 04:30 UTC: 12:30 that day in UTC+8, 04:30 that day in UTC.
    const NOON: i64 = 1_789_533_000;

    #[test]
    fn day_labels_follow_the_site_calendar_day() {
        let east8 = TimeFormat::default();
        assert_eq!(east8.offset_minutes, 480);
        // In UTC+8, 2026-09-16 00:10 and 23:50 the night before are only 20 minutes apart but on different
        // calendar days.
        let midnight = NOON - 12 * 3600 - 30 * 60;
        assert_eq!(day_label(midnight + 600, midnight + 900, &east8), "today");
        assert_eq!(
            day_label(midnight - 600, midnight + 600, &east8),
            "yesterday"
        );
        assert_eq!(day_label(NOON - 2 * 86400, NOON, &east8), "2 days ago");
        assert_eq!(day_label(NOON - 30 * 86400, NOON, &east8), "30 days ago");
        assert_eq!(
            day_label(NOON - 31 * 86400, NOON, &east8),
            date(NOON - 31 * 86400, &east8)
        );
        // A future time caused by clock skew is not written as "-1 days ago".
        assert_eq!(day_label(NOON + 3600, NOON, &east8), "today");
    }

    #[test]
    fn display_follows_the_configured_offset() {
        let east8 = TimeFormat::default();
        assert_eq!(datetime(NOON, &east8), "2026-09-16 12:30");
        assert_eq!(date(NOON, &east8), "2026-09-16");
        assert_eq!(short_datetime(NOON, &east8), "09-16 12:30");

        let utc = TimeFormat {
            offset_minutes: 0,
            ..TimeFormat::default()
        };
        assert_eq!(datetime(NOON, &utc), "2026-09-16 04:30");

        // Negative offsets and offsets that are not whole hours must both work.
        let kolkata = TimeFormat {
            offset_minutes: 330,
            ..TimeFormat::default()
        };
        assert_eq!(datetime(NOON, &kolkata), "2026-09-16 10:00");
        let denver = TimeFormat {
            offset_minutes: -6 * 60,
            ..TimeFormat::default()
        };
        assert_eq!(datetime(NOON, &denver), "2026-09-15 22:30");
    }

    #[test]
    fn date_crosses_midnight_with_the_offset() {
        // 2026-09-15 20:00 UTC is already the 16th in UTC+8: places that only take the date, such as the
        // registration date, would be off by a whole day around midnight if the conversion were missed.
        let evening = NOON - 8 * 3600 - 30 * 60;
        assert_eq!(
            date(
                evening,
                &TimeFormat {
                    offset_minutes: 0,
                    ..TimeFormat::default()
                }
            ),
            "2026-09-15"
        );
        assert_eq!(date(evening, &TimeFormat::default()), "2026-09-16");
    }

    #[test]
    fn form_boundary_round_trips_through_the_same_offset() {
        for offset in [0, 480, 330, -360] {
            let format = TimeFormat {
                offset_minutes: offset,
                ..TimeFormat::default()
            };
            let text = input_value(NOON, &format);
            assert_eq!(
                parse_input(&text, &format),
                Some(NOON),
                "offset {offset} does not round-trip"
            );
        }
        // The same wall-clock string means different instants in different time zones; the difference is exactly
        // the difference of their offsets.
        let east8 = parse_input("2026-09-16T12:30", &TimeFormat::default()).unwrap();
        let utc = parse_input(
            "2026-09-16T12:30",
            &TimeFormat {
                offset_minutes: 0,
                ..TimeFormat::default()
            },
        )
        .unwrap();
        assert_eq!(utc - east8, 8 * 3600);
    }

    #[test]
    fn date_style_and_clock_follow_the_setting() {
        let base = TimeFormat::default();
        assert_eq!(datetime(NOON, &base), "2026-09-16 12:30");
        assert_eq!(
            datetime(
                NOON,
                &TimeFormat {
                    date: DateStyle::YmdChinese,
                    ..base
                }
            ),
            // The Chinese year, month and day signs after the numbers.
            "2026\u{5e74}09\u{6708}16\u{65e5} 12:30"
        );
        assert_eq!(
            datetime(
                NOON,
                &TimeFormat {
                    date: DateStyle::MdySlash,
                    clock24: false,
                    ..base
                }
            ),
            "09/16/2026 12:30 PM"
        );
        // The compact style drops the year, but its clock format and date style come from the same setting.
        assert_eq!(
            short_datetime(
                NOON,
                &TimeFormat {
                    date: DateStyle::YmdChinese,
                    ..base
                }
            ),
            "09\u{6708}16\u{65e5} 12:30"
        );
        // Date-only places are not affected by the clock format.
        assert_eq!(
            date(
                NOON,
                &TimeFormat {
                    clock24: false,
                    ..base
                }
            ),
            "2026-09-16"
        );
        // Unknown values fall back to the default style instead of panicking.
        assert_eq!(DateStyle::parse("nonsense"), DateStyle::YmdDash);
    }

    #[test]
    fn relative_time_covers_only_the_recent_past() {
        let format = TimeFormat {
            relative_hours: Some(72),
            ..TimeFormat::default()
        };
        assert_eq!(relative(NOON, NOON, &format).as_deref(), Some("just now"));
        assert_eq!(
            relative(NOON - 59, NOON, &format).as_deref(),
            Some("just now")
        );
        assert_eq!(
            relative(NOON - 60, NOON, &format).as_deref(),
            Some("1 minute ago")
        );
        assert_eq!(
            relative(NOON - 3599, NOON, &format).as_deref(),
            Some("59 minutes ago")
        );
        assert_eq!(
            relative(NOON - 3600, NOON, &format).as_deref(),
            Some("1 hour ago")
        );
        // Boundary: exactly at the upper limit of the range it falls back to absolute time.
        assert!(relative(NOON - 72 * 3600 + 1, NOON, &format).is_some());
        assert_eq!(relative(NOON - 72 * 3600, NOON, &format), None);
        // Future times (activity deadlines etc.) always use the absolute style.
        assert_eq!(relative(NOON + 60, NOON, &format), None);
        // With the switch off it does not take part at all.
        assert_eq!(relative(NOON, NOON, &TimeFormat::default()), None);
    }

    #[test]
    fn settings_map_onto_the_format() {
        let mut settings = crate::runtime::settings::tests::seeded();
        // The seed values: UTC+8, 24-hour clock, no relative time, dates in the `locale` style that follows the site
        // language (en); outside a request the format takes the site language.
        let format = TimeFormat::from(&settings);
        assert_eq!(
            format,
            TimeFormat {
                date: DateStyle::Locale,
                ..TimeFormat::default()
            }
        );
        assert_eq!(format.relative_hours, None);
        settings.site.language = "zh-CN".into();
        assert_eq!(
            TimeFormat::from(&settings).lang,
            crate::i18n::Lang::parse("zh-CN").unwrap()
        );

        settings.time.offset_minutes = 0;
        settings.time.date_format = "ymd-cn".into();
        settings.time.clock24 = false;
        settings.time.relative = true;
        settings.time.relative_hours = 12;
        let format = TimeFormat::from(&settings);
        assert_eq!(format.offset_minutes, 0);
        assert_eq!(format.date, DateStyle::YmdChinese);
        assert!(!format.clock24);
        assert_eq!(format.relative_hours, Some(12));
    }

    #[test]
    fn settlement_days_follow_the_site_offset() {
        // NOON is 12:30 in UTC+8 and 04:30 UTC, the same calendar day in both time zones.
        let east8_midnight = NOON - 12 * 3600 - 30 * 60;
        assert_eq!(day_start(NOON, 480), east8_midnight);
        assert_eq!(day_start(east8_midnight, 480), east8_midnight);
        assert_eq!(day_start(east8_midnight - 1, 480), east8_midnight - 86_400);
        assert_eq!(day_start(NOON, 0), NOON - 4 * 3600 - 30 * 60);
        // UTC+8 crosses midnight while UTC is still on the same day: the "days" of the two time zones split here.
        assert_eq!(
            day_number(east8_midnight, 480),
            day_number(east8_midnight - 1, 480) + 1
        );
        assert_eq!(
            day_number(east8_midnight, 0),
            day_number(east8_midnight - 1, 0)
        );
        // Negative and non-whole-hour offsets: midnight falls on local 00:00.
        let denver = day_start(NOON, -360);
        assert_eq!(
            datetime(
                denver,
                &TimeFormat {
                    offset_minutes: -360,
                    ..TimeFormat::default()
                }
            ),
            "2026-09-15 00:00"
        );
        let kolkata = day_start(NOON, 330);
        assert_eq!(
            datetime(
                kolkata,
                &TimeFormat {
                    offset_minutes: 330,
                    ..TimeFormat::default()
                }
            ),
            "2026-09-16 00:00"
        );
        // Day numbers in UTC+8 match the `shanghai_day` from before the rework, so existing check-in idempotency
        // keys do not change.
        assert_eq!(day_number(57_599, 480), 0);
        assert_eq!(day_number(57_600, 480), 1);
        assert_eq!(day_start(57_600, 480), 57_600);
    }

    #[test]
    fn weeks_start_on_the_site_monday() {
        // 2026-09-16 is a Wednesday; this week's Monday 2026-09-14 00:00 in UTC+8 is 09-13 16:00 UTC.
        let east8 = TimeFormat::default();
        let monday = week_start(NOON, 480);
        assert_eq!(datetime(monday, &east8), "2026-09-14 00:00");
        assert_eq!(week_start(monday, 480), monday);
        assert_eq!(week_start(monday - 1, 480), monday - 7 * 86_400);
        // Monday midnight in UTC is eight hours later than in UTC+8; before the rework the activity index was
        // computed by it.
        assert_eq!(week_start(NOON, 0), monday + 8 * 3600);
        let utc = TimeFormat {
            offset_minutes: 0,
            ..TimeFormat::default()
        };
        assert_eq!(datetime(week_start(NOON, 0), &utc), "2026-09-14 00:00");
        // Monday 00:30 in UTC+8 is still Sunday in UTC: UTC+8 has entered the new week, UTC is still in the last.
        let early = monday + 1800;
        assert_eq!(week_start(early, 480), monday);
        assert_eq!(week_start(early, 0), monday + 8 * 3600 - 7 * 86_400);
    }

    #[test]
    fn zone_choices_cover_every_site_zone_once_per_offset() {
        let choices = zone_choices(480);
        let offsets: Vec<i64> = choices.iter().map(|c| c.offset_minutes).collect();
        assert_eq!(offsets, [0, 240, 330, 420, 480, 540]);
        assert_eq!(choices[0].label, "UTC");
        assert_eq!(
            choices[4].label,
            "UTC+8 (Asia/Shanghai, Asia/Hong_Kong, Asia/Taipei, Asia/Singapore)"
        );
        assert_eq!(choices[2].label, "UTC+5:30 (Asia/Kolkata)");
        // All 11 time zone names are present.
        for zone in SITE_ZONES {
            assert!(
                choices.iter().any(|c| c.label.contains(zone.name)),
                "{} is not among the choices",
                zone.name
            );
        }
        // When the database holds a value outside the list, an extra "current value" item is added so the dropdown
        // does not default to the first item.
        let odd = zone_choices(-300);
        assert_eq!(odd.len(), 7);
        assert_eq!(
            odd.last(),
            Some(&ZoneChoice {
                offset_minutes: -300,
                label: "UTC-5 (current value)".into()
            })
        );
    }

    #[test]
    fn invalid_input_is_rejected_not_guessed() {
        let format = TimeFormat::default();
        for bad in ["", "2026-09-16", "2026/09/16 12:30", "2026-09-16T25:00"] {
            assert_eq!(parse_input(bad, &format), None, "{bad} must be rejected");
        }
        // Timestamps outside what chrono can represent return an empty string instead of panicking.
        assert_eq!(datetime(i64::MAX, &format), "");
        assert_eq!(date(i64::MIN, &format), "");
    }
}
