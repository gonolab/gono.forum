//! Plain-text encoding and decoding of the settings table, one value per row.
//!
//! `svalue` stores the scalar as text: booleans `true`/`false`, integers in decimal, strings as is (the empty string
//! is a valid value; "not filled in" for `settings.site.contact_email` and `settings.site.url` is the empty string).
//! Booleans are written as `true`/`false` rather than `1`/`0` to match the output of `JSON_UNQUOTE(JSON_EXTRACT(…))`
//! for JSON booleans.
//!
//! **No fallbacks** (docs/settings-and-caching.md, "Reading settings"): a missing row, a parse failure or a
//! value outside the hard bounds always reports a [`SettingsError`] saying which key, what the database holds now
//! and what it should be, never falling back to a default in code. Defaults exist only once, in the migrations;
//! the ranges here are hard bounds (validation ranges), not defaults. Reads and writes share this validation: on
//! write the candidate value is encoded into a row and decoded again by the same rules, so any value that can be
//! read can be written, and vice versa.
use crate::db::{self, DbRow};

/// A settings row that cannot be read or does not comply. Used by both the error page and form hints: `expected` is
/// the value requirement written for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError {
    pub key: String,
    /// `None` means the database has no such row.
    pub found: Option<String>,
    pub expected: &'static str,
}

impl SettingsError {
    pub fn new(key: &str, found: Option<&str>, expected: &'static str) -> Self {
        Self {
            key: key.to_owned(),
            found: found.map(str::to_owned),
            expected,
        }
    }

    /// What the database holds now, for the error page: a missing row, an empty string and the text are described
    /// differently, and overly long text is truncated.
    pub fn found_text(&self) -> String {
        let t = crate::i18n::current();
        match &self.found {
            None => t.config_settings_missing().into(),
            Some(value) if value.is_empty() => t.config_settings_empty().into(),
            Some(value) => {
                let shown: String = value.chars().take(200).collect();
                if shown.len() < value.len() {
                    t.config_settings_truncated(shown, value.chars().count())
                } else {
                    t.config_settings_text(shown)
                }
            }
        }
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&crate::i18n::current().config_settings_error(
            self.key.as_str(),
            self.found_text(),
            self.expected,
        ))
    }
}

impl std::error::Error for SettingsError {}

pub type Result<T> = std::result::Result<T, SettingsError>;

/// The rows fetched in one read (scalar settings by primary key `IN`, documents by range), indexed by `skey`.
///
/// There are a hundred-odd scalar settings rows, looked up by binary search on the key name; SQL returns the rows in
/// ascending primary key order, and `collect` sorts once more just in case.
pub struct Rows(Vec<(String, String)>);

impl Rows {
    pub fn collect(rows: &[DbRow]) -> anyhow::Result<Self> {
        let pairs = rows
            .iter()
            .map(|row| Ok((db::text(row, "skey")?, db::text(row, "svalue")?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self::from_pairs(pairs))
    }

    pub fn from_pairs(mut pairs: Vec<(String, String)>) -> Self {
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        Self(pairs)
    }

    /// The value of one key, if the read found it.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw(key)
    }

    /// The keys and values of one family (`prefix` ends with `.`), in key order.
    pub fn with_prefix<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
        let start = self.0.partition_point(|(name, _)| name.as_str() < prefix);
        self.0[start..]
            .iter()
            .take_while(move |(name, _)| name.starts_with(prefix))
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    fn raw(&self, key: &str) -> Option<&str> {
        self.0
            .binary_search_by(|(name, _)| name.as_str().cmp(key))
            .ok()
            .map(|index| self.0[index].1.as_str())
    }

    fn present(&self, key: &str, expected: &'static str) -> Result<&str> {
        self.raw(key)
            .ok_or_else(|| SettingsError::new(key, None, expected))
    }

    /// Strings are taken as is, the empty string is a valid value; an error when `valid` fails.
    pub fn text(
        &self,
        key: &str,
        expected: &'static str,
        valid: impl Fn(&str) -> bool,
    ) -> Result<String> {
        let value = self.present(key, expected)?;
        if valid(value) {
            Ok(value.to_owned())
        } else {
            Err(SettingsError::new(key, Some(value), expected))
        }
    }

    /// Only `true` and `false` are accepted.
    pub fn flag(&self, key: &str, expected: &'static str) -> Result<bool> {
        match self.present(key, expected)? {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(SettingsError::new(key, Some(other), expected)),
        }
    }

    /// A decimal integer in the closed range `min..=max`. Some settings go straight into an SQL `LIMIT` or time
    /// window, so out-of-range values are always errors.
    pub fn int(&self, key: &str, min: i64, max: i64, expected: &'static str) -> Result<i64> {
        let value = self.present(key, expected)?;
        match value.parse::<i64>() {
            Ok(number) if (min..=max).contains(&number) && number.to_string() == value => {
                Ok(number)
            }
            _ => Err(SettingsError::new(key, Some(value), expected)),
        }
    }

    /// Enums must be in the allowlist: if `settings.signup.mode` held a value the database does not know, every
    /// equality check on the consuming side would fail, which amounts to open registration, so it is an error rather
    /// than let through.
    pub fn choice(&self, key: &str, allowed: &[&str], expected: &'static str) -> Result<String> {
        self.text(key, expected, |value| allowed.contains(&value))
    }
}

pub fn flag_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// The half-open range of a key family: `['prefix.', 'prefix/')`.
///
/// The trailing `.` (0x2E) becomes `/` (0x2F), which under `utf8mb4_bin` just passes every key of the family without
/// taking in neighboring keys without the dot. **No `LIKE 'prefix%'`**: `_` is LIKE's single-character wildcard, and
/// mail template kinds contain underscores (`email_verification`), so LIKE would sweep in other families.
pub fn range(prefix: &str) -> (String, String) {
    debug_assert!(prefix.ends_with('.'), "prefix must end with '.': {prefix}");
    (
        prefix.to_owned(),
        format!("{}/", &prefix[..prefix.len() - 1]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(pairs: &[(&str, &str)]) -> Rows {
        Rows::from_pairs(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn scalars_round_trip() {
        let value = rows(&[
            ("a.name", "Sample Community"),
            ("a.blank", ""),
            ("a.on", "true"),
            ("a.off", "false"),
            ("a.offset", "-300"),
            ("a.hours", "72"),
        ]);
        assert_eq!(
            value.text("a.name", "x", |_| true).unwrap(),
            "Sample Community"
        );
        // The empty string is a valid value.
        assert_eq!(value.text("a.blank", "x", |_| true).unwrap(), "");
        assert!(value.flag("a.on", "x").unwrap());
        assert!(!value.flag("a.off", "x").unwrap());
        assert_eq!(value.int("a.offset", -720, 840, "x").unwrap(), -300);
        assert_eq!(value.int("a.hours", 1, 720, "x").unwrap(), 72);
    }

    /// Missing rows, bad values and out-of-range values are all errors, carrying the key name, the text in the
    /// database and the value requirement; no fallback to any default.
    #[test]
    fn missing_bad_and_out_of_range_values_fail_with_details() {
        let value = rows(&[
            ("a.on", "TRUE"),
            ("a.off", "1"),
            ("a.offset", "abc"),
            ("a.hours", "0"),
            ("a.padded", " 5"),
            ("a.plus", "+5"),
            ("a.mode", "xyz"),
        ]);
        let missing = value.flag("a.missing", "true or false").unwrap_err();
        assert_eq!(missing.found, None);
        assert!(missing.to_string().contains("a.missing"));
        assert!(missing.to_string().contains("no such row"));
        for key in ["a.on", "a.off"] {
            let error = value.flag(key, "true or false").unwrap_err();
            assert_eq!(error.key, key);
            assert!(error.found.is_some());
        }
        let error = value.int("a.offset", 0, 10, "0 to 10").unwrap_err();
        assert_eq!(error.found.as_deref(), Some("abc"));
        assert_eq!(error.expected, "0 to 10");
        assert!(value.int("a.hours", 1, 720, "x").is_err());
        assert!(value.int("a.padded", 1, 720, "x").is_err());
        assert!(value.int("a.plus", 1, 720, "x").is_err());
        assert!(
            value
                .choice("a.mode", &["closed", "invite", "open"], "x")
                .is_err()
        );
        assert!(value.text("a.missing", "x", |_| true).is_err());
    }

    #[test]
    fn found_text_names_blank_and_long_values() {
        assert_eq!(
            SettingsError::new("k", Some(""), "x").found_text(),
            "an empty string"
        );
        // Three-byte characters ("\u{957f}", the Chinese for "long"), so the count shown is in characters, not bytes.
        let long = "\u{957f}".repeat(250);
        assert!(
            SettingsError::new("k", Some(&long), "x")
                .found_text()
                .contains("250 characters in all")
        );
    }

    #[test]
    fn ranges_cover_the_family_and_nothing_else() {
        let (low, high) = range("settings.");
        assert_eq!((low.as_str(), high.as_str()), ("settings.", "settings/"));
        let inside = |key: &str| key >= low.as_str() && key < high.as_str();
        assert!(inside("settings.site.name") && inside("settings.time.relative_hours"));
        // Neighboring keys are all outside the range: bare keys without the dot, other families, and similarly named
        // families with a longer suffix.
        for key in [
            "settings",
            "settingsx.site",
            "site.setting.site_name",
            "identity.bootstrap_admin",
            "mail.transport",
        ] {
            assert!(!inside(key), "{key} must fall outside the settings. range");
        }
        // Template family: `LIKE 'settings.mail.email_verification.%'` would also take in emailXverification (`_` is
        // the single-character wildcard); a range does not.
        let (low, high) = range("settings.mail.email_verification.");
        let inside = |key: &str| key >= low.as_str() && key < high.as_str();
        assert!(inside("settings.mail.email_verification.subject"));
        assert!(!inside("settings.mail.emailXverification.subject"));
        assert!(!inside("settings.mail.content.subject"));
    }

    #[test]
    fn flags_encode_as_json_shaped_text() {
        assert_eq!(flag_text(true), "true");
        assert_eq!(flag_text(false), "false");
    }
}
