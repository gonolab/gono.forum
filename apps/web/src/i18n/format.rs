//! Numbers and dates by language with ICU4X (the full built-in data, so a new translation needs no data files;
//! docs/architecture.md, "Languages").
use std::sync::OnceLock;

use fluent_bundle::FluentValue;
use icu_datetime::{
    DateTimeFormatter, DateTimeFormatterPreferences,
    fieldsets::{MDT, YMD, YMDT},
    input::{Date, DateTime, Time},
    options::TimePrecision,
};
use icu_decimal::{
    DecimalFormatter,
    input::Decimal,
    options::{DecimalFormatterOptions, GroupingStrategy},
};
use icu_locale_core::{Locale, preferences::extensions::unicode::keywords::HourCycle};
use intl_memoizer::{Memoizable, concurrent::IntlLangMemoizer};

/// What a date style `locale` time shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateField {
    /// Year, month, day, hours and minutes (detail pages).
    DateTime,
    /// Month, day, hours and minutes, without the year (lists).
    Short,
    /// Year, month and day.
    Date,
}

/// Per-language formatters, each built on first use and kept for the life of the process: index 0 is the 12-hour
/// clock, 1 the 24-hour clock (the `settings.time.clock24` setting).
pub(super) struct Dates {
    locale: Locale,
    /// Whether numbers group their digits (`meta-number-grouping`, docs/architecture.md, "Languages").
    grouping: bool,
    decimal: OnceLock<Option<DecimalFormatter>>,
    datetime: [OnceLock<Option<DateTimeFormatter<YMDT>>>; 2],
    short: [OnceLock<Option<DateTimeFormatter<MDT>>>; 2],
    date: OnceLock<Option<DateTimeFormatter<YMD>>>,
}

impl Dates {
    /// Whether numbers group their digits, for the browser script to format the same way.
    pub(super) fn grouping(&self) -> bool {
        self.grouping
    }

    pub(super) fn new(locale: Locale, grouping: bool) -> Self {
        Self {
            locale,
            grouping,
            decimal: OnceLock::new(),
            datetime: [OnceLock::new(), OnceLock::new()],
            short: [OnceLock::new(), OnceLock::new()],
            date: OnceLock::new(),
        }
    }

    fn preferences(&self, clock24: bool) -> DateTimeFormatterPreferences {
        let mut preferences: DateTimeFormatterPreferences = (&self.locale).into();
        preferences.hour_cycle = Some(if clock24 {
            HourCycle::H23
        } else {
            HourCycle::H12
        });
        preferences
    }

    /// A formatter that cannot be built (no data for this locale, which the full data set makes impossible in
    /// practice) leaves the time empty rather than failing the page; the caller already treats an empty string as
    /// "no time".
    pub(super) fn format(
        &self,
        field: DateField,
        clock24: bool,
        at: chrono::NaiveDateTime,
    ) -> String {
        use chrono::{Datelike, Timelike};
        let date = (|| {
            Date::try_new_iso(
                at.year(),
                u8::try_from(at.month()).ok()?,
                u8::try_from(at.day()).ok()?,
            )
            .ok()
        })();
        let time = Time::try_new(
            u8::try_from(at.hour()).unwrap_or(0),
            u8::try_from(at.minute()).unwrap_or(0),
            0,
            0,
        )
        .ok();
        let (Some(date), Some(time)) = (date, time) else {
            return String::new();
        };
        let input = DateTime { date, time };
        let hour = usize::from(clock24);
        match field {
            DateField::DateTime => self.datetime[hour]
                .get_or_init(|| {
                    DateTimeFormatter::try_new(
                        self.preferences(clock24),
                        YMDT::medium().with_time_precision(TimePrecision::Minute),
                    )
                    .ok()
                })
                .as_ref()
                .map(|formatter| formatter.format(&input).to_string()),
            DateField::Short => self.short[hour]
                .get_or_init(|| {
                    DateTimeFormatter::try_new(
                        self.preferences(clock24),
                        MDT::medium().with_time_precision(TimePrecision::Minute),
                    )
                    .ok()
                })
                .as_ref()
                .map(|formatter| formatter.format(&input).to_string()),
            DateField::Date => self
                .date
                .get_or_init(|| {
                    DateTimeFormatter::try_new(self.preferences(clock24), YMD::medium()).ok()
                })
                .as_ref()
                .map(|formatter| formatter.format(&input.date).to_string()),
        }
        .unwrap_or_default()
    }
}

/// ICU options for a language that groups digits or not.
fn options(grouping: bool) -> DecimalFormatterOptions {
    if grouping {
        GroupingStrategy::Auto
    } else {
        GroupingStrategy::Never
    }
    .into()
}

/// An integer in the language's digits and grouping.
pub(super) fn number(dates: &Dates, value: i64) -> String {
    dates
        .decimal
        .get_or_init(|| {
            DecimalFormatter::try_new((&dates.locale).into(), options(dates.grouping)).ok()
        })
        .as_ref()
        .map_or_else(
            || value.to_string(),
            |formatter| formatter.format(&Decimal::from(value)).to_string(),
        )
}

/// The ICU number formatter kept in each Fluent bundle's memoizer (the memoizer is what knows the bundle's language).
struct BundleDecimal(DecimalFormatter);

impl Memoizable for BundleDecimal {
    type Args = ();
    type Error = String;

    fn construct(lang: unic_langid::LanguageIdentifier, _: ()) -> Result<Self, Self::Error> {
        let tag = lang.to_string();
        let locale: Locale = tag.parse().map_err(|error| format!("{error}"))?;
        // Built on the first formatted number, after every language has loaded.
        let grouping = super::Lang::parse(&tag).is_none_or(|lang| lang.language().dates.grouping);
        DecimalFormatter::try_new((&locale).into(), options(grouping))
            .map(Self)
            .map_err(|error| format!("{error}"))
    }
}

/// Fluent's value formatter (`FluentBundle::set_formatter`): whole numbers go through ICU so `{ $count }` reads
/// `1,234` in English; fractions and other values keep Fluent's own formatting. Plural selection is unaffected (it
/// still sees the number).
pub(super) fn number_value(value: &FluentValue, memoizer: &IntlLangMemoizer) -> Option<String> {
    let FluentValue::Number(number) = value else {
        return None;
    };
    if number.options.minimum_fraction_digits.is_some()
        || number.value.fract() != 0.0
        || number.value.abs() > 9_007_199_254_740_991.0
    {
        return None;
    }
    // Checked above: a whole number within the exactly representable range.
    #[allow(clippy::cast_possible_truncation)]
    let whole = number.value as i64;
    memoizer
        .with_try_get::<BundleDecimal, _, _>((), |formatter| {
            formatter.0.format(&Decimal::from(whole)).to_string()
        })
        .ok()
}
