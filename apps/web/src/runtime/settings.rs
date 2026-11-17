//! All scalar settings: structure, per-item decoding and encoding.
//!
//! Keys are always `settings.{group}.{field}` (docs/settings-and-caching.md, "Keys and groups"); the Rust structures
//! are split by the same groups, and field names equal the last segment of the key, so `settings.site.name` is
//! `settings.site.name`. Default values exist only once, in the rows the migrations seed; there is no `Default` here.
//!
//! Decoding and encoding each list every key; a unit test decodes the encoded result again, and any mismatch between
//! the two (a missing key, a misspelled key, a type mismatch) turns red at once. Write validation is "encode, then
//! decode by the read rules", so the error the form reports and the error page say the same sentence.
use super::kv::{Result, Rows, SettingsError, flag_text};

pub const REGISTRATION_MODES: &[&str] = &["closed", "invite", "open"];
pub const DATE_FORMATS: &[&str] = &["ymd-dash", "ymd-cn", "mdy-slash", "locale"];
pub const LIST_SORTS: &[&str] = &["latest", "created"];
pub const THREAD_VIEWS: &[&str] = &["nested", "flat"];
pub const AVATAR_SHAPES: &[&str] = &["rounded", "circle"];
/// Upper bound on the in-process cache lifetime: one day. Saving in the admin area clears the cache on the spot; the
/// lifetime only backstops changes that bypass the admin area.
pub const MAX_CACHE_TTL_SECONDS: i64 = 86_400;
/// Login methods the login page may additionally show (`settings.login.methods` and the domain's `login_methods`);
/// password login is always there.
pub const LOGIN_METHODS: &[&str] = &["sms", "google", "github"];
/// Participation conditions (`settings.login.participation` and the domain's `participation`); meeting any one of them
/// allows participation, and an empty set requires none.
pub const PARTICIPATION: &[&str] = &["email", "mobile", "google", "github"];
/// Accounts with two-factor authentication enabled when logging in by SMS or a third party: `require` behaves like
/// password login, `skip` treats SMS and the third party as the second factor and logs in directly.
pub const EXTERNAL_TWO_FACTOR: &[&str] = &["require", "skip"];
/// Maximum number of tip tiers, shared by the admin form and validation.
pub const MAX_TIERS: usize = 8;
/// Maximum number of invite code prefixes and characters per prefix (`settings.invite.prefixes`), shared by the admin
/// notes and validation.
pub const MAX_INVITE_PREFIXES: usize = 50;
pub const INVITE_PREFIX_CHARS: std::ops::RangeInclusive<usize> = 2..=6;
/// Names not allowed for site verification: this site's `<head>` already outputs them, case-insensitive.
pub const VERIFICATION_RESERVED: &[&str] = &[
    "description",
    "robots",
    "viewport",
    "htmx-config",
    "csp-nonce",
    "composer-published",
];

/// All scalar settings. The page frame, the write gate and the admin area all use this one, decoded from the same
/// set of rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteSettings {
    pub account: AccountSettings,
    pub api: ApiSettings,
    pub cache: CacheSettings,
    pub debug: DebugSettings,
    pub display: DisplaySettings,
    pub domains: DomainSettings,
    pub email: EmailSettings,
    pub feed: FeedSettings,
    pub forum: ForumSettings,
    pub governance: GovernanceSettings,
    pub growth: GrowthSettings,
    pub invite: InviteSettings,
    pub limit: LimitSettings,
    pub links: LinkSettings,
    pub login: LoginSettings,
    pub maintenance: MaintenanceSettings,
    pub posting: PostingSettings,
    pub ranks: RankSettings,
    pub reading: ReadingDefaults,
    pub seo: SeoSettings,
    pub signup: SignupSettings,
    pub site: SiteInfo,
    pub sms: SmsSettings,
    pub time: TimeSettings,
    pub tip: TipSettings,
    pub username: UsernameSettings,
}

// The `Default`s on the group structures are all zero values and empty strings, used only to build
// [`SiteSettings::blank`]; they are not the default value of any setting.

/// Account security (admin "Security › Account security").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountSettings {
    pub session_idle_days: i64,
    pub session_absolute_days: i64,
    pub verification_ttl_hours: i64,
    pub reset_ttl_minutes: i64,
    pub email_change_ttl_hours: i64,
    pub password_min_length: i64,
    pub login_ip_per_5min: i64,
    pub login_name_per_5min: i64,
    pub two_factor_ip_per_5min: i64,
    pub password_change_account_per_5min: i64,
    pub password_change_ip_per_5min: i64,
    pub forgot_email_per_minute: i64,
    pub forgot_email_per_hour: i64,
    pub forgot_ip_per_hour: i64,
    pub reset_token_per_5min: i64,
    pub reset_ip_per_5min: i64,
}

/// Member API (admin "Global settings › Registration & permissions").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiSettings {
    /// When off, all API requests return 403; tokens members already generated are kept.
    pub enabled: bool,
    pub key_ttl_days: i64,
    pub requests_per_minute: i64,
    pub writes_per_minute: i64,
    pub auth_failures_per_5min: i64,
    pub max_body_kib: i64,
}

/// Cache lifetimes (admin "Global settings › Search & performance").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheSettings {
    /// Lifetime of the in-process global cache; 0 means the in-process cache is not used (every request reads the
    /// database as usual).
    pub ttl_seconds: i64,
    /// Floor cache (Redis), only effective when Redis is enabled.
    pub page_ttl_seconds: i64,
    /// Lists, sidebar hot and latest, and feeds (Redis), only effective when Redis is enabled.
    pub list_ttl_seconds: i64,
    /// Site-wide statistics and the newest member for anonymous visitors are bucketed by this duration.
    pub statistics_seconds: i64,
    /// Forum statistics are bucketed by this duration (it only has a read point once the forums project is done).
    pub forum_statistics_seconds: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugSettings {
    pub panel: bool,
}

/// Display settings (admin "Appearance › Display").
/// Multi-domain sites (the switch of the admin "Multiple domains" module). Not changed on the global settings page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomainSettings {
    /// When on, sites are split by Host and path; when off, the domain table is not read and there is only the main
    /// site.
    pub enabled: bool,
}

/// Mail sending switch (at the top of the admin "Mail settings" page, not changed on the global settings page).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmailSettings {
    /// When off, the mail queue pauses delivery: events are queued as usual and are sent once it is turned back on;
    /// rules such as e-mail verification are unchanged. Seeded as off: a new site has no mail configured yet.
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplaySettings {
    pub threads_per_page: i64,
    pub hot_days: i64,
    pub hot_count: i64,
    pub latest_count: i64,
    pub recent_members: i64,
    pub highlight_min_likes: i64,
    pub highlight_count: i64,
    pub ranks_shown: i64,
    pub replies_per_page: i64,
    pub sub_replies_per_page: i64,
    pub footer_metrics: bool,
    /// How many thumbnails each thread list row shows (static images in the first post, in order of appearance); 0
    /// shows none.
    pub list_thumbnails: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedSettings {
    pub items: i64,
}

/// Site-level forum settings (the "Forum settings" at the top of the admin `/manage/forums`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForumSettings {
    pub directory_statistics: bool,
}

/// Deadlines for reports, restorations and appeals (admin "Security › Anti-spam & governance").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GovernanceSettings {
    pub appeal_window_days: i64,
    pub appeal_handling_days: i64,
    pub restoration_cooldown_days: i64,
    pub restoration_due_days: i64,
}

/// Growth points (admin "Global settings › Points").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrowthSettings {
    pub thread: i64,
    pub reply: i64,
    pub like: i64,
    pub digest: i64,
    pub accept: i64,
    /// Level thresholds, first item 0, strictly increasing; stored comma-separated in the database.
    pub levels: Vec<i64>,
    /// Level names corresponding one to one to the thresholds; stored one per line in the database.
    pub level_names: Vec<String>,
    /// Level gates (docs/roles.md, "Level gates"): the required level number, 1 for no limit, not more
    /// than the number of levels. Staff are not limited.
    /// Posting external links (both this and the new-member observation period must be satisfied).
    pub link_level: i64,
    /// Generating personal invite codes.
    pub invite_level: i64,
    /// Starting lotteries and red packets.
    pub activity_level: i64,
}

/// Invitations (admin "Global settings › Registration & permissions").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InviteSettings {
    /// Allows members to issue new invite codes.
    pub enabled: bool,
    pub daily_limit: i64,
    pub active_limit: i64,
    pub ttl_days: i64,
    /// Invite code prefixes set by administrators: any invite code starting with one of them can register, is not
    /// consumed and can be reused.
    /// Stored one per line in the database; the empty string means none.
    pub prefixes: Vec<String>,
}

impl InviteSettings {
    /// Which prefix `code` starts with; when several match, the longest one is taken (the registration log records
    /// it). Case-sensitive, like invite codes.
    pub fn prefix_of(&self, code: &str) -> Option<&str> {
        self.prefixes
            .iter()
            .filter(|prefix| code.starts_with(prefix.as_str()))
            .max_by_key(|prefix| prefix.len())
            .map(String::as_str)
    }
}

/// Rate limits; only the counts are exposed, and the windows are written in the key names (admin
/// "Security › Anti-spam & governance").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LimitSettings {
    pub message_per_minute: i64,
    pub message_peer_per_minute: i64,
    pub interaction_per_minute: i64,
    pub interaction_per_hour: i64,
    pub social_per_5min: i64,
    pub report_daily: i64,
    pub restoration_daily: i64,
}

/// Friend link application rules (admin "Appearance › Friend links").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkSettings {
    /// Whether member applications are open. When off, the application page shows
    /// "Link applications are closed for now." and the directory and home page show no application entry; existing
    /// applications and links are unaffected.
    pub applications: bool,
    pub home_count: i64,
    pub live_per_member: i64,
    pub reject_cooldown_days: i64,
    pub min_account_days: i64,
    pub apply_per_hour: i64,
    pub apply_ip_per_hour: i64,
}

/// Retention periods (admin "Tools & maintenance › Maintenance").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaintenanceSettings {
    pub outbox_retention_days: i64,
    pub prepared_mail_retention_days: i64,
    pub request_retention_days: i64,
}

/// Posting quotas and anti-spam (admin "Security › Anti-spam & governance").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PostingSettings {
    pub first_day_threads: i64,
    pub first_day_replies: i64,
    pub daily_threads: i64,
    pub daily_replies: i64,
    pub new_member_hours: i64,
    pub body_max_bytes: i64,
    pub edit_window_hours: i64,
    pub thread_interval_seconds: i64,
    pub reply_interval_seconds: i64,
    pub hourly_limit: i64,
    pub duplicate_check: bool,
    pub duplicate_window_minutes: i64,
    pub new_member_link_block: bool,
    pub new_member_link_days: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RankSettings {
    pub refresh_seconds: i64,
}

/// Site default reading preferences: used by anonymous visitors and by members who have not chosen an item (a NULL
/// column in `gonof_home_reading_preference` means following the site).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadingDefaults {
    /// `latest` or `created`.
    pub list_sort: String,
    /// `nested` or `flat`.
    pub thread_view: String,
    pub title_latest: bool,
    pub title_new_tab: bool,
    /// `rounded` or `circle`.
    pub avatar_shape: String,
}

/// Search (the SEO settings block of admin "Global settings › Search & performance").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeoSettings {
    /// Allow search engines to index: shared by robots, the sitemap and page meta information.
    pub indexing: bool,
    /// robots.txt text (output when indexing is on), LF between lines.
    pub robots: String,
    /// Site verification meta tags, one `name=value` per line.
    pub verification: String,
}

/// Registration (admin "Global settings › Registration & permissions").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignupSettings {
    /// `closed` / `invite` / `open`.
    pub mode: String,
    pub ip_hourly_limit: i64,
}

/// Site-wide defaults of login methods and participation conditions (admin "Global settings › Login methods"). When the
/// same-named column of a domain row is not NULL, the domain wins; see [`crate::login`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginSettings {
    /// Methods the login page additionally shows, a subset of [`LOGIN_METHODS`], comma-separated in that constant's
    /// order; the empty string means password login only.
    pub methods: String,
    /// Participation conditions, a subset of [`PARTICIPATION`], comma-separated in that constant's order; the empty
    /// string requires none.
    pub participation: String,
    /// One of [`EXTERNAL_TWO_FACTOR`].
    pub external_two_factor: String,
}

/// SMS verification code limits (admin "Global settings › Login methods"). "Per day" splits days in the site time zone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SmsSettings {
    pub mobile_interval_seconds: i64,
    pub mobile_daily_limit: i64,
    pub ip_hourly_limit: i64,
    pub site_daily_limit: i64,
    pub code_ttl_minutes: i64,
    pub max_attempts: i64,
}

/// Site information (admin "Global settings › Site settings"; the description is edited in the SEO block of
/// "Search & performance").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SiteInfo {
    /// Empty shows the product name in every language (docs/architecture.md, "Languages"); use
    /// [`SiteInfo::display_name`].
    pub name: String,
    /// Empty shows the language's default description; use [`SiteInfo::description_in`].
    pub description: String,
    /// The site's interface language, a tag compiled into the binary (`en`, `zh-CN`; docs/architecture.md,
    /// "Languages").
    /// Another domain can override it with its own column.
    pub language: String,
    /// Header brand text; empty shows the product name ([`SiteInfo::header_brand`]).
    pub brand_text: String,
    /// The name of the site's points; empty shows the language's default ([`SiteInfo::credit_word`]).
    pub credit_name: String,
    /// The name of the weekly activity figure; empty shows the language's default ([`SiteInfo::index_word`]).
    pub index_name: String,
    /// Site contact e-mail, shown below the body of the contact page `/contact`; the empty string means not shown.
    pub contact_email: String,
    /// Site URL used for external links; empty means it is derived from the request `Host`. Only affects canonical,
    /// the sitemap, URLs returned by the API and links in mails, and **does not affect** the cookie's Secure flag or
    /// the CSRF same-origin check — those two are derived from the current connection, so a wrong value here only
    /// makes links point to the wrong place.
    pub url: String,
    pub read_only: bool,
    /// Empty shows the language's default notice; use [`SiteInfo::read_only_notice`].
    pub read_only_message: String,
}

/// Values left empty fall back to the product name or to the language's default words (docs/architecture.md,
/// "Languages"). The settings keep the empty string; only display substitutes.
impl SiteInfo {
    /// The site language (decoding guarantees a compiled-in tag).
    pub fn lang(&self) -> crate::i18n::Lang {
        crate::i18n::Lang::parse(&self.language).unwrap_or_default()
    }

    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            crate::i18n::PRODUCT_NAME
        } else {
            &self.name
        }
    }

    pub fn header_brand(&self) -> &str {
        if self.brand_text.is_empty() {
            crate::i18n::PRODUCT_NAME
        } else {
            &self.brand_text
        }
    }

    pub fn credit_word(&self, lang: crate::i18n::Lang) -> String {
        credit_word(&self.credit_name, lang)
    }

    pub fn index_word(&self, lang: crate::i18n::Lang) -> String {
        if self.index_name.is_empty() {
            lang.brand_index_name().to_owned()
        } else {
            self.index_name.clone()
        }
    }

    pub fn description_in(&self, lang: crate::i18n::Lang) -> String {
        if self.description.is_empty() {
            lang.brand_site_description().to_owned()
        } else {
            self.description.clone()
        }
    }

    pub fn read_only_notice(&self, lang: crate::i18n::Lang) -> String {
        if self.read_only_message.is_empty() {
            lang.brand_read_only_message().to_owned()
        } else {
            self.read_only_message.clone()
        }
    }
}

/// Time (the time settings of admin "Global settings › Site settings").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeSettings {
    /// The site time zone's offset from UTC in minutes. Records are always UTC.
    pub offset_minutes: i64,
    /// Date format: `ymd-dash` / `ymd-cn` / `mdy-slash`. The enum name is stored rather than a format string — storing
    /// the format string directly would let administrators inject arbitrary input into `chrono::format`.
    pub date_format: String,
    pub clock24: bool,
    /// Whether recent times are shown as "3 hours ago".
    pub relative: bool,
    /// Range (hours) to which relative time applies; beyond it absolute time is shown.
    pub relative_hours: i64,
}

/// Thread tips (admin "Global settings › Points").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TipSettings {
    pub enabled: bool,
    /// Ascending, unique positive integer tiers; stored comma-separated in the database.
    pub amounts: Vec<i64>,
    /// Daily tip spending limit per member.
    pub daily_limit: i64,
}

/// Renaming (admin "Global settings › Registration & permissions").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsernameSettings {
    pub cooldown_days: i64,
    pub history_days: i64,
}

impl SiteSettings {
    /// Whether any value the page shows was left empty and needs its default (see [`Self::fill_defaults`]).
    pub fn needs_defaults(&self) -> bool {
        let site = &self.site;
        site.name.is_empty()
            || site.description.is_empty()
            || site.read_only_message.is_empty()
            || site.brand_text.is_empty()
            || site.credit_name.is_empty()
            || site.index_name.is_empty()
            || self.growth.level_names.is_empty()
    }

    /// The copy pages read: values left empty replaced by the product name or the words of `lang`
    /// (docs/architecture.md, "Languages"). Only the page frame's own copy gets this; admin forms read the database
    /// and keep the empty strings.
    pub fn fill_defaults(&mut self, lang: crate::i18n::Lang) {
        let site = &mut self.site;
        site.name = site.display_name().to_owned();
        site.description = site.description_in(lang);
        site.read_only_message = site.read_only_notice(lang);
        site.brand_text = site.header_brand().to_owned();
        site.credit_name = site.credit_word(lang);
        site.index_name = site.index_word(lang);
        if self.growth.level_names.is_empty() {
            self.growth.level_names = (1..=self.growth.levels.len())
                .map(|number| lang.brand_level_name(number))
                .collect();
        }
    }

    /// Placeholder while the page frame has not read the settings yet: all zero values and empty strings, **not the
    /// defaults**. Only the initial value of `SiteFrame` and [`scalar_keys`] (which only takes key names) use it; when
    /// the page frame fails to load, the page uses the simple error page that does not depend on settings
    /// (`router::error_page`), so these zero values are never used as settings.
    pub(crate) fn blank() -> Self {
        Self {
            account: AccountSettings::default(),
            api: ApiSettings::default(),
            cache: CacheSettings::default(),
            debug: DebugSettings::default(),
            display: DisplaySettings::default(),
            domains: DomainSettings::default(),
            email: EmailSettings::default(),
            feed: FeedSettings::default(),
            forum: ForumSettings::default(),
            governance: GovernanceSettings::default(),
            growth: GrowthSettings::default(),
            invite: InviteSettings::default(),
            limit: LimitSettings::default(),
            links: LinkSettings::default(),
            login: LoginSettings::default(),
            maintenance: MaintenanceSettings::default(),
            posting: PostingSettings::default(),
            ranks: RankSettings::default(),
            reading: ReadingDefaults::default(),
            seo: SeoSettings::default(),
            signup: SignupSettings::default(),
            site: SiteInfo::default(),
            sms: SmsSettings::default(),
            time: TimeSettings::default(),
            tip: TipSettings::default(),
            username: UsernameSettings::default(),
        }
    }

    /// Lifetime (seconds) of the in-process global cache; 0 means no caching.
    pub fn cache_ttl(&self) -> u64 {
        u64::try_from(self.cache.ttl_seconds).unwrap_or(0)
    }
}

// ---- Text value rules ---------------------------------------------------------------------------

/// A comma-separated set: every item is in `allowed`, unique, and ordered as in `allowed` (the encoded result is
/// unique; the form joins them in order before saving).
pub fn set_shape(value: &str, allowed: &[&str], allow_empty: bool) -> bool {
    if value.is_empty() {
        return allow_empty;
    }
    let mut last = None;
    value.split(',').all(|item| {
        let Some(position) = allowed.iter().position(|candidate| *candidate == item) else {
            return false;
        };
        let ordered = last.is_none_or(|previous| position > previous);
        last = Some(position);
        ordered
    })
}

/// Whether the set contains this item (the set is encoded by [`set_shape`]).
pub fn set_contains(value: &str, item: &str) -> bool {
    !value.is_empty() && value.split(',').any(|candidate| candidate == item)
}

fn chars_between(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.chars().count())
}

pub(crate) fn email_shape(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= 190
            && value.split_once('@').is_some_and(|(user, domain)| {
                !user.is_empty() && domain.contains('.') && !domain.starts_with('.')
            }))
}

/// The empty string, or an already normalized forum URL: an HTTP(S) origin plus an optional path prefix
/// (`config::normalize_forum_url`).
fn site_url_shape(value: &str) -> bool {
    value.is_empty()
        || (value.chars().count() <= 255
            && crate::config::normalize_forum_url(value).is_ok_and(|normal| normal == value))
}

/// robots.txt: ≤100 lines, ≤4000 characters, no control characters except newlines and tabs.
fn robots_shape(value: &str) -> bool {
    value.chars().count() <= 4000
        && (value.is_empty() || value.split('\n').count() <= 100)
        && !value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
}

/// Site verification: the empty string, or ≤10 lines, each `name=value`.
pub(crate) fn verification_shape(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let lines: Vec<&str> = value.split('\n').collect();
    lines.len() <= 10
        && lines.iter().all(|line| {
            line.split_once('=').is_some_and(|(name, content)| {
                (1..=64).contains(&name.len())
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                    && !VERIFICATION_RESERVED
                        .iter()
                        .any(|reserved| reserved.eq_ignore_ascii_case(name))
                    && (1..=128).contains(&content.len())
                    && content
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            })
        })
}

/// Comma-separated decimal integers in canonical form (no spaces or plus signs).
fn parse_list(value: &str) -> Option<Vec<i64>> {
    let numbers = value
        .split(',')
        .map(|part| part.parse::<i64>().ok())
        .collect::<Option<Vec<_>>>()?;
    (list_text(&numbers) == value).then_some(numbers)
}

pub fn list_text(values: &[i64]) -> String {
    values
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Tip tiers: 1 to [`MAX_TIERS`] of them, each 1 to 1 billion, strictly ascending.
pub fn valid_tip_amounts(amounts: &[i64]) -> bool {
    (1..=MAX_TIERS).contains(&amounts.len())
        && amounts
            .iter()
            .all(|amount| (1..=crate::activities::MAX_CREDIT).contains(amount))
        && amounts.windows(2).all(|pair| pair[0] < pair[1])
}

/// Level thresholds: 1 to 20 levels, first item 0, strictly increasing (reusing the validation in `growth.rs`).
fn valid_levels(levels: &[i64]) -> bool {
    (1..=20).contains(&levels.len())
        && levels[0] == 0
        && levels.windows(2).all(|pair| pair[0] < pair[1])
}

/// Invite code prefixes: the empty string, or one per line, at most 50, all different, each 2–6 English letters,
/// digits, underscores or hyphens.
fn invite_prefixes_shape(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let prefixes: Vec<&str> = text.split('\n').collect();
    let mut sorted = prefixes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    prefixes.len() <= MAX_INVITE_PREFIXES
        && sorted.len() == prefixes.len()
        && prefixes.iter().all(|prefix| {
            INVITE_PREFIX_CHARS.contains(&prefix.len())
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}

/// Level names: the empty string (every level uses the language's default name), or one per line, each 1 to 20
/// characters and not blank.
fn valid_level_names(text: &str) -> bool {
    text.is_empty()
        || text
            .split('\n')
            .all(|name| !name.trim().is_empty() && name.chars().count() <= 20)
}

/// A one-line brand word of at most `max` characters, possibly empty.
fn brand_word(value: &str, max: usize) -> bool {
    chars_between(value, 0, max) && !value.contains(['\r', '\n'])
}

// ---- Decoding -----------------------------------------------------------------------------------

/// Decodes all scalar settings from the rows fetched by one read (primary key `IN` over [`scalar_keys`]). Any missing
/// row, parse failure or out-of-range value is an error; there is no fallback to defaults.
pub fn decode(r: &Rows) -> Result<SiteSettings> {
    // Requirements are written in this request's language (they reach the error page and the admin forms).
    let t = crate::i18n::current();
    let settings = SiteSettings {
        account: AccountSettings {
            session_idle_days: r.int(
                "settings.account.session_idle_days",
                1,
                90,
                t.config_check_account_session_idle_days(),
            )?,
            session_absolute_days: r.int(
                "settings.account.session_absolute_days",
                1,
                365,
                t.config_check_account_session_absolute_days(),
            )?,
            verification_ttl_hours: r.int(
                "settings.account.verification_ttl_hours",
                1,
                168,
                t.config_check_account_verification_ttl_hours(),
            )?,
            reset_ttl_minutes: r.int(
                "settings.account.reset_ttl_minutes",
                10,
                1440,
                t.config_check_account_reset_ttl_minutes(),
            )?,
            email_change_ttl_hours: r.int(
                "settings.account.email_change_ttl_hours",
                1,
                168,
                t.config_check_account_email_change_ttl_hours(),
            )?,
            password_min_length: r.int(
                "settings.account.password_min_length",
                8,
                64,
                t.config_check_account_password_min_length(),
            )?,
            login_ip_per_5min: r.int(
                "settings.account.login_ip_per_5min",
                5,
                100,
                t.config_check_account_login_ip_per_5min(),
            )?,
            login_name_per_5min: r.int(
                "settings.account.login_name_per_5min",
                3,
                30,
                t.config_check_account_login_name_per_5min(),
            )?,
            two_factor_ip_per_5min: r.int(
                "settings.account.two_factor_ip_per_5min",
                5,
                100,
                t.config_check_account_two_factor_ip_per_5min(),
            )?,
            password_change_account_per_5min: r.int(
                "settings.account.password_change_account_per_5min",
                3,
                20,
                t.config_check_account_password_change_account_per_5min(),
            )?,
            password_change_ip_per_5min: r.int(
                "settings.account.password_change_ip_per_5min",
                5,
                100,
                t.config_check_account_password_change_ip_per_5min(),
            )?,
            forgot_email_per_minute: r.int(
                "settings.account.forgot_email_per_minute",
                1,
                5,
                t.config_check_account_forgot_email_per_minute(),
            )?,
            forgot_email_per_hour: r.int(
                "settings.account.forgot_email_per_hour",
                1,
                20,
                t.config_check_account_forgot_email_per_hour(),
            )?,
            forgot_ip_per_hour: r.int(
                "settings.account.forgot_ip_per_hour",
                5,
                100,
                t.config_check_account_forgot_ip_per_hour(),
            )?,
            reset_token_per_5min: r.int(
                "settings.account.reset_token_per_5min",
                3,
                30,
                t.config_check_account_reset_token_per_5min(),
            )?,
            reset_ip_per_5min: r.int(
                "settings.account.reset_ip_per_5min",
                5,
                100,
                t.config_check_account_reset_ip_per_5min(),
            )?,
        },
        api: ApiSettings {
            enabled: r.flag("settings.api.enabled", t.config_check_api_enabled())?,
            key_ttl_days: r.int(
                "settings.api.key_ttl_days",
                1,
                365,
                t.config_check_api_key_ttl_days(),
            )?,
            requests_per_minute: r.int(
                "settings.api.requests_per_minute",
                1,
                10000,
                t.config_check_api_requests_per_minute(),
            )?,
            writes_per_minute: r.int(
                "settings.api.writes_per_minute",
                1,
                10000,
                t.config_check_api_writes_per_minute(),
            )?,
            auth_failures_per_5min: r.int(
                "settings.api.auth_failures_per_5min",
                5,
                100,
                t.config_check_api_auth_failures_per_5min(),
            )?,
            max_body_kib: r.int(
                "settings.api.max_body_kib",
                1,
                1024,
                t.config_check_api_max_body_kib(),
            )?,
        },
        cache: CacheSettings {
            ttl_seconds: r.int(
                "settings.cache.ttl_seconds",
                0,
                MAX_CACHE_TTL_SECONDS,
                t.config_check_cache_ttl_seconds(),
            )?,
            page_ttl_seconds: r.int(
                "settings.cache.page_ttl_seconds",
                1,
                3600,
                t.config_check_cache_page_ttl_seconds(),
            )?,
            list_ttl_seconds: r.int(
                "settings.cache.list_ttl_seconds",
                1,
                3600,
                t.config_check_cache_list_ttl_seconds(),
            )?,
            statistics_seconds: r.int(
                "settings.cache.statistics_seconds",
                30,
                86400,
                t.config_check_cache_statistics_seconds(),
            )?,
            forum_statistics_seconds: r.int(
                "settings.cache.forum_statistics_seconds",
                30,
                86400,
                t.config_check_cache_forum_statistics_seconds(),
            )?,
        },
        debug: DebugSettings {
            panel: r.flag("settings.debug.panel", t.config_check_debug_panel())?,
        },
        display: DisplaySettings {
            threads_per_page: r.int(
                "settings.display.threads_per_page",
                10,
                100,
                t.config_check_display_threads_per_page(),
            )?,
            hot_days: r.int(
                "settings.display.hot_days",
                1,
                30,
                t.config_check_display_hot_days(),
            )?,
            hot_count: r.int(
                "settings.display.hot_count",
                1,
                20,
                t.config_check_display_hot_count(),
            )?,
            latest_count: r.int(
                "settings.display.latest_count",
                1,
                20,
                t.config_check_display_latest_count(),
            )?,
            recent_members: r.int(
                "settings.display.recent_members",
                1,
                20,
                t.config_check_display_recent_members(),
            )?,
            highlight_min_likes: r.int(
                "settings.display.highlight_min_likes",
                1,
                100,
                t.config_check_display_highlight_min_likes(),
            )?,
            highlight_count: r.int(
                "settings.display.highlight_count",
                0,
                10,
                t.config_check_display_highlight_count(),
            )?,
            ranks_shown: r.int(
                "settings.display.ranks_shown",
                10,
                100,
                t.config_check_display_ranks_shown(),
            )?,
            replies_per_page: r.int(
                "settings.display.replies_per_page",
                10,
                100,
                t.config_check_display_replies_per_page(),
            )?,
            sub_replies_per_page: r.int(
                "settings.display.sub_replies_per_page",
                5,
                50,
                t.config_check_display_sub_replies_per_page(),
            )?,
            footer_metrics: r.flag(
                "settings.display.footer_metrics",
                t.config_check_display_footer_metrics(),
            )?,
            list_thumbnails: r.int(
                "settings.display.list_thumbnails",
                0,
                5,
                t.config_check_display_list_thumbnails(),
            )?,
        },
        domains: DomainSettings {
            enabled: r.flag("settings.domains.enabled", t.config_check_domains_enabled())?,
        },
        email: EmailSettings {
            enabled: r.flag("settings.email.enabled", t.config_check_email_enabled())?,
        },
        feed: FeedSettings {
            items: r.int("settings.feed.items", 10, 100, t.config_check_feed_items())?,
        },
        forum: ForumSettings {
            directory_statistics: r.flag(
                "settings.forum.directory_statistics",
                t.config_check_forum_directory_statistics(),
            )?,
        },
        governance: GovernanceSettings {
            appeal_window_days: r.int(
                "settings.governance.appeal_window_days",
                1,
                365,
                t.config_check_governance_appeal_window_days(),
            )?,
            appeal_handling_days: r.int(
                "settings.governance.appeal_handling_days",
                1,
                90,
                t.config_check_governance_appeal_handling_days(),
            )?,
            restoration_cooldown_days: r.int(
                "settings.governance.restoration_cooldown_days",
                0,
                365,
                t.config_check_governance_restoration_cooldown_days(),
            )?,
            restoration_due_days: r.int(
                "settings.governance.restoration_due_days",
                1,
                90,
                t.config_check_governance_restoration_due_days(),
            )?,
        },
        growth: GrowthSettings {
            thread: r.int(
                "settings.growth.thread",
                0,
                1_000_000,
                t.config_check_growth_thread(),
            )?,
            reply: r.int(
                "settings.growth.reply",
                0,
                1_000_000,
                t.config_check_growth_reply(),
            )?,
            like: r.int(
                "settings.growth.like",
                0,
                1_000_000,
                t.config_check_growth_like(),
            )?,
            digest: r.int(
                "settings.growth.digest",
                0,
                1_000_000,
                t.config_check_growth_digest(),
            )?,
            accept: r.int(
                "settings.growth.accept",
                0,
                1_000_000,
                t.config_check_growth_accept(),
            )?,
            levels: {
                let text = r.text(
                    "settings.growth.levels",
                    t.config_check_growth_levels(),
                    |value| parse_list(value).is_some_and(|levels| valid_levels(&levels)),
                )?;
                parse_list(&text).expect("just validated")
            },
            level_names: {
                let text = r.text(
                    "settings.growth.level_names",
                    t.config_check_growth_level_names(),
                    valid_level_names,
                )?;
                if text.is_empty() {
                    Vec::new()
                } else {
                    text.split('\n').map(str::to_owned).collect()
                }
            },
            link_level: r.int(
                "settings.growth.link_level",
                1,
                20,
                t.config_check_growth_level_gate(),
            )?,
            invite_level: r.int(
                "settings.growth.invite_level",
                1,
                20,
                t.config_check_growth_level_gate(),
            )?,
            activity_level: r.int(
                "settings.growth.activity_level",
                1,
                20,
                t.config_check_growth_level_gate(),
            )?,
        },
        invite: InviteSettings {
            enabled: r.flag("settings.invite.enabled", t.config_check_invite_enabled())?,
            daily_limit: r.int(
                "settings.invite.daily_limit",
                1,
                100,
                t.config_check_invite_daily_limit(),
            )?,
            active_limit: r.int(
                "settings.invite.active_limit",
                1,
                100,
                t.config_check_invite_active_limit(),
            )?,
            ttl_days: r.int(
                "settings.invite.ttl_days",
                1,
                365,
                t.config_check_invite_ttl_days(),
            )?,
            prefixes: {
                let text = r.text(
                    "settings.invite.prefixes",
                    t.config_check_invite_prefixes(),
                    invite_prefixes_shape,
                )?;
                if text.is_empty() {
                    Vec::new()
                } else {
                    text.split('\n').map(str::to_owned).collect()
                }
            },
        },
        limit: LimitSettings {
            message_per_minute: r.int(
                "settings.limit.message_per_minute",
                1,
                1000,
                t.config_check_limit_message_per_minute(),
            )?,
            message_peer_per_minute: r.int(
                "settings.limit.message_peer_per_minute",
                1,
                1000,
                t.config_check_limit_message_peer_per_minute(),
            )?,
            interaction_per_minute: r.int(
                "settings.limit.interaction_per_minute",
                1,
                1000,
                t.config_check_limit_interaction_per_minute(),
            )?,
            interaction_per_hour: r.int(
                "settings.limit.interaction_per_hour",
                1,
                100_000,
                t.config_check_limit_interaction_per_hour(),
            )?,
            social_per_5min: r.int(
                "settings.limit.social_per_5min",
                1,
                1000,
                t.config_check_limit_social_per_5min(),
            )?,
            report_daily: r.int(
                "settings.limit.report_daily",
                1,
                100,
                t.config_check_limit_report_daily(),
            )?,
            restoration_daily: r.int(
                "settings.limit.restoration_daily",
                1,
                100,
                t.config_check_limit_restoration_daily(),
            )?,
        },
        links: LinkSettings {
            applications: r.flag(
                "settings.links.applications",
                t.config_check_links_applications(),
            )?,
            home_count: r.int(
                "settings.links.home_count",
                1,
                50,
                t.config_check_links_home_count(),
            )?,
            live_per_member: r.int(
                "settings.links.live_per_member",
                1,
                20,
                t.config_check_links_live_per_member(),
            )?,
            reject_cooldown_days: r.int(
                "settings.links.reject_cooldown_days",
                0,
                365,
                t.config_check_links_reject_cooldown_days(),
            )?,
            min_account_days: r.int(
                "settings.links.min_account_days",
                0,
                3650,
                t.config_check_links_min_account_days(),
            )?,
            apply_per_hour: r.int(
                "settings.links.apply_per_hour",
                1,
                100,
                t.config_check_links_apply_per_hour(),
            )?,
            apply_ip_per_hour: r.int(
                "settings.links.apply_ip_per_hour",
                1,
                1000,
                t.config_check_links_apply_ip_per_hour(),
            )?,
        },
        login: LoginSettings {
            methods: r.text(
                "settings.login.methods",
                t.config_check_login_methods(),
                |v| set_shape(v, LOGIN_METHODS, true),
            )?,
            participation: r.text(
                "settings.login.participation",
                t.config_check_login_participation(),
                |v| set_shape(v, PARTICIPATION, true),
            )?,
            external_two_factor: r.choice(
                "settings.login.external_two_factor",
                EXTERNAL_TWO_FACTOR,
                t.config_check_login_external_two_factor(),
            )?,
        },
        maintenance: MaintenanceSettings {
            outbox_retention_days: r.int(
                "settings.maintenance.outbox_retention_days",
                1,
                3650,
                t.config_check_maintenance_outbox_retention_days(),
            )?,
            prepared_mail_retention_days: r.int(
                "settings.maintenance.prepared_mail_retention_days",
                1,
                3650,
                t.config_check_maintenance_prepared_mail_retention_days(),
            )?,
            request_retention_days: r.int(
                "settings.maintenance.request_retention_days",
                7,
                3650,
                t.config_check_maintenance_request_retention_days(),
            )?,
        },
        posting: PostingSettings {
            first_day_threads: r.int(
                "settings.posting.first_day_threads",
                1,
                1000,
                t.config_check_posting_first_day_threads(),
            )?,
            first_day_replies: r.int(
                "settings.posting.first_day_replies",
                1,
                1000,
                t.config_check_posting_first_day_replies(),
            )?,
            daily_threads: r.int(
                "settings.posting.daily_threads",
                1,
                1000,
                t.config_check_posting_daily_threads(),
            )?,
            daily_replies: r.int(
                "settings.posting.daily_replies",
                1,
                10_000,
                t.config_check_posting_daily_replies(),
            )?,
            new_member_hours: r.int(
                "settings.posting.new_member_hours",
                1,
                720,
                t.config_check_posting_new_member_hours(),
            )?,
            body_max_bytes: r.int(
                "settings.posting.body_max_bytes",
                1000,
                60_000,
                t.config_check_posting_body_max_bytes(),
            )?,
            edit_window_hours: r.int(
                "settings.posting.edit_window_hours",
                1,
                8760,
                t.config_check_posting_edit_window_hours(),
            )?,
            thread_interval_seconds: r.int(
                "settings.posting.thread_interval_seconds",
                0,
                3600,
                t.config_check_posting_thread_interval_seconds(),
            )?,
            reply_interval_seconds: r.int(
                "settings.posting.reply_interval_seconds",
                0,
                600,
                t.config_check_posting_reply_interval_seconds(),
            )?,
            hourly_limit: r.int(
                "settings.posting.hourly_limit",
                0,
                1000,
                t.config_check_posting_hourly_limit(),
            )?,
            duplicate_check: r.flag(
                "settings.posting.duplicate_check",
                t.config_check_posting_duplicate_check(),
            )?,
            duplicate_window_minutes: r.int(
                "settings.posting.duplicate_window_minutes",
                1,
                1440,
                t.config_check_posting_duplicate_window_minutes(),
            )?,
            new_member_link_block: r.flag(
                "settings.posting.new_member_link_block",
                t.config_check_posting_new_member_link_block(),
            )?,
            new_member_link_days: r.int(
                "settings.posting.new_member_link_days",
                1,
                90,
                t.config_check_posting_new_member_link_days(),
            )?,
        },
        ranks: RankSettings {
            refresh_seconds: r.int(
                "settings.ranks.refresh_seconds",
                10,
                86400,
                t.config_check_ranks_refresh_seconds(),
            )?,
        },
        reading: ReadingDefaults {
            list_sort: r.choice(
                "settings.reading.list_sort",
                LIST_SORTS,
                t.config_check_reading_list_sort(),
            )?,
            thread_view: r.choice(
                "settings.reading.thread_view",
                THREAD_VIEWS,
                t.config_check_reading_thread_view(),
            )?,
            title_latest: r.flag(
                "settings.reading.title_latest",
                t.config_check_reading_title_latest(),
            )?,
            title_new_tab: r.flag(
                "settings.reading.title_new_tab",
                t.config_check_reading_title_new_tab(),
            )?,
            avatar_shape: r.choice(
                "settings.reading.avatar_shape",
                AVATAR_SHAPES,
                t.config_check_reading_avatar_shape(),
            )?,
        },
        seo: SeoSettings {
            indexing: r.flag("settings.seo.indexing", t.config_check_seo_indexing())?,
            robots: r.text(
                "settings.seo.robots",
                t.config_check_seo_robots(),
                robots_shape,
            )?,
            verification: r.text(
                "settings.seo.verification",
                t.config_check_seo_verification(),
                verification_shape,
            )?,
        },
        signup: SignupSettings {
            mode: r.choice(
                "settings.signup.mode",
                REGISTRATION_MODES,
                t.config_check_signup_mode(),
            )?,
            ip_hourly_limit: r.int(
                "settings.signup.ip_hourly_limit",
                1,
                1000,
                t.config_check_signup_ip_hourly_limit(),
            )?,
        },
        site: SiteInfo {
            name: r.text("settings.site.name", t.config_check_site_name(), |v| {
                chars_between(v, 0, 80)
            })?,
            description: r.text(
                "settings.site.description",
                t.config_check_site_description(),
                |v| chars_between(v, 0, 500),
            )?,
            language: r.text(
                "settings.site.language",
                t.config_check_site_language(),
                |v| crate::i18n::Lang::parse(v).is_some(),
            )?,
            brand_text: r.text(
                "settings.site.brand_text",
                t.config_check_site_brand_text(),
                |v| brand_word(v, 40),
            )?,
            credit_name: r.text(
                "settings.site.credit_name",
                t.config_check_site_credit_name(),
                |v| brand_word(v, 10),
            )?,
            index_name: r.text(
                "settings.site.index_name",
                t.config_check_site_index_name(),
                |v| brand_word(v, 20),
            )?,
            contact_email: r.text(
                "settings.site.contact_email",
                t.config_check_site_contact_email(),
                email_shape,
            )?,
            url: r.text(
                "settings.site.url",
                t.config_check_site_url(),
                site_url_shape,
            )?,
            read_only: r.flag("settings.site.read_only", t.config_check_site_read_only())?,
            read_only_message: r.text(
                "settings.site.read_only_message",
                t.config_check_site_read_only_message(),
                |v| chars_between(v, 0, 500),
            )?,
        },
        sms: SmsSettings {
            mobile_interval_seconds: r.int(
                "settings.sms.mobile_interval_seconds",
                30,
                3600,
                t.config_check_sms_mobile_interval_seconds(),
            )?,
            mobile_daily_limit: r.int(
                "settings.sms.mobile_daily_limit",
                1,
                50,
                t.config_check_sms_mobile_daily_limit(),
            )?,
            ip_hourly_limit: r.int(
                "settings.sms.ip_hourly_limit",
                1,
                1000,
                t.config_check_sms_ip_hourly_limit(),
            )?,
            site_daily_limit: r.int(
                "settings.sms.site_daily_limit",
                1,
                1_000_000,
                t.config_check_sms_site_daily_limit(),
            )?,
            code_ttl_minutes: r.int(
                "settings.sms.code_ttl_minutes",
                1,
                30,
                t.config_check_sms_code_ttl_minutes(),
            )?,
            max_attempts: r.int(
                "settings.sms.max_attempts",
                1,
                20,
                t.config_check_sms_max_attempts(),
            )?,
        },
        time: TimeSettings {
            offset_minutes: r.int(
                "settings.time.offset_minutes",
                -12 * 60,
                14 * 60,
                t.config_check_time_offset_minutes(),
            )?,
            date_format: r.choice(
                "settings.time.date_format",
                DATE_FORMATS,
                t.config_check_time_date_format(),
            )?,
            clock24: r.flag("settings.time.clock24", t.config_check_time_clock24())?,
            relative: r.flag("settings.time.relative", t.config_check_time_relative())?,
            relative_hours: r.int(
                "settings.time.relative_hours",
                1,
                720,
                t.config_check_time_relative_hours(),
            )?,
        },
        tip: TipSettings {
            enabled: r.flag("settings.tip.enabled", t.config_check_tip_enabled())?,
            amounts: {
                let text = r.text(
                    "settings.tip.amounts",
                    t.config_check_tip_amounts(),
                    |value| parse_list(value).is_some_and(|amounts| valid_tip_amounts(&amounts)),
                )?;
                parse_list(&text).expect("just validated")
            },
            daily_limit: r.int(
                "settings.tip.daily_limit",
                1,
                crate::activities::MAX_CREDIT,
                t.config_check_tip_daily_limit(),
            )?,
        },
        username: UsernameSettings {
            cooldown_days: r.int(
                "settings.username.cooldown_days",
                1,
                365,
                t.config_check_username_cooldown_days(),
            )?,
            history_days: r.int(
                "settings.username.history_days",
                1,
                365,
                t.config_check_username_history_days(),
            )?,
        },
    };
    relations(&settings)?;
    Ok(settings)
}

/// Cross-item constraints. The error lands on the "later item", with the same message as that item's own
/// requirement.
fn relations(s: &SiteSettings) -> Result<()> {
    let t = crate::i18n::current();
    let fail = |key: &str, value: String, expected: &'static str| {
        Err(SettingsError::new(key, Some(&value), expected))
    };
    if s.account.session_absolute_days < s.account.session_idle_days {
        return fail(
            "settings.account.session_absolute_days",
            s.account.session_absolute_days.to_string(),
            t.config_check_account_session_absolute_days(),
        );
    }
    if s.api.writes_per_minute > s.api.requests_per_minute {
        return fail(
            "settings.api.writes_per_minute",
            s.api.writes_per_minute.to_string(),
            t.config_check_api_writes_per_minute(),
        );
    }
    if !s.growth.level_names.is_empty() && s.growth.level_names.len() != s.growth.levels.len() {
        return fail(
            "settings.growth.level_names",
            s.growth.level_names.join("\n"),
            t.config_check_growth_level_names(),
        );
    }
    for (key, level) in [
        ("settings.growth.link_level", s.growth.link_level),
        ("settings.growth.invite_level", s.growth.invite_level),
        ("settings.growth.activity_level", s.growth.activity_level),
    ] {
        if level > s.growth.levels.len() as i64 {
            return fail(key, level.to_string(), t.config_check_growth_level_gate());
        }
    }
    if s.limit.message_peer_per_minute > s.limit.message_per_minute {
        return fail(
            "settings.limit.message_peer_per_minute",
            s.limit.message_peer_per_minute.to_string(),
            t.config_check_limit_message_peer_per_minute(),
        );
    }
    if s.limit.interaction_per_hour < s.limit.interaction_per_minute {
        return fail(
            "settings.limit.interaction_per_hour",
            s.limit.interaction_per_hour.to_string(),
            t.config_check_limit_interaction_per_hour(),
        );
    }
    if s.links.apply_ip_per_hour < s.links.apply_per_hour {
        return fail(
            "settings.links.apply_ip_per_hour",
            s.links.apply_ip_per_hour.to_string(),
            t.config_check_links_apply_ip_per_hour(),
        );
    }
    if s.posting.daily_threads < s.posting.first_day_threads {
        return fail(
            "settings.posting.daily_threads",
            s.posting.daily_threads.to_string(),
            t.config_check_posting_daily_threads(),
        );
    }
    if s.posting.daily_replies < s.posting.first_day_replies {
        return fail(
            "settings.posting.daily_replies",
            s.posting.daily_replies.to_string(),
            t.config_check_posting_daily_replies(),
        );
    }
    if s.tip
        .amounts
        .last()
        .is_some_and(|max| s.tip.daily_limit < *max)
    {
        return fail(
            "settings.tip.daily_limit",
            s.tip.daily_limit.to_string(),
            t.config_check_tip_daily_limit(),
        );
    }
    if s.username.history_days < s.username.cooldown_days {
        return fail(
            "settings.username.history_days",
            s.username.history_days.to_string(),
            t.config_check_username_history_days(),
        );
    }
    Ok(())
}

// ---- Encoding -----------------------------------------------------------------------------------

/// The text of every scalar settings row, in ascending key order (the same order as reading and locking by primary
/// key).
pub fn encode(s: &SiteSettings) -> Vec<(&'static str, String)> {
    let int = |value: i64| value.to_string();
    let flag = |value: bool| flag_text(value).to_owned();
    let mut rows = vec![
        (
            "settings.account.session_idle_days",
            int(s.account.session_idle_days),
        ),
        (
            "settings.account.session_absolute_days",
            int(s.account.session_absolute_days),
        ),
        (
            "settings.account.verification_ttl_hours",
            int(s.account.verification_ttl_hours),
        ),
        (
            "settings.account.reset_ttl_minutes",
            int(s.account.reset_ttl_minutes),
        ),
        (
            "settings.account.email_change_ttl_hours",
            int(s.account.email_change_ttl_hours),
        ),
        (
            "settings.account.password_min_length",
            int(s.account.password_min_length),
        ),
        (
            "settings.account.login_ip_per_5min",
            int(s.account.login_ip_per_5min),
        ),
        (
            "settings.account.login_name_per_5min",
            int(s.account.login_name_per_5min),
        ),
        (
            "settings.account.two_factor_ip_per_5min",
            int(s.account.two_factor_ip_per_5min),
        ),
        (
            "settings.account.password_change_account_per_5min",
            int(s.account.password_change_account_per_5min),
        ),
        (
            "settings.account.password_change_ip_per_5min",
            int(s.account.password_change_ip_per_5min),
        ),
        (
            "settings.account.forgot_email_per_minute",
            int(s.account.forgot_email_per_minute),
        ),
        (
            "settings.account.forgot_email_per_hour",
            int(s.account.forgot_email_per_hour),
        ),
        (
            "settings.account.forgot_ip_per_hour",
            int(s.account.forgot_ip_per_hour),
        ),
        (
            "settings.account.reset_token_per_5min",
            int(s.account.reset_token_per_5min),
        ),
        (
            "settings.account.reset_ip_per_5min",
            int(s.account.reset_ip_per_5min),
        ),
        ("settings.api.enabled", flag(s.api.enabled)),
        ("settings.api.key_ttl_days", int(s.api.key_ttl_days)),
        (
            "settings.api.requests_per_minute",
            int(s.api.requests_per_minute),
        ),
        (
            "settings.api.writes_per_minute",
            int(s.api.writes_per_minute),
        ),
        (
            "settings.api.auth_failures_per_5min",
            int(s.api.auth_failures_per_5min),
        ),
        ("settings.api.max_body_kib", int(s.api.max_body_kib)),
        ("settings.cache.ttl_seconds", int(s.cache.ttl_seconds)),
        (
            "settings.cache.page_ttl_seconds",
            int(s.cache.page_ttl_seconds),
        ),
        (
            "settings.cache.list_ttl_seconds",
            int(s.cache.list_ttl_seconds),
        ),
        (
            "settings.cache.statistics_seconds",
            int(s.cache.statistics_seconds),
        ),
        (
            "settings.cache.forum_statistics_seconds",
            int(s.cache.forum_statistics_seconds),
        ),
        ("settings.debug.panel", flag(s.debug.panel)),
        (
            "settings.display.threads_per_page",
            int(s.display.threads_per_page),
        ),
        ("settings.display.hot_days", int(s.display.hot_days)),
        ("settings.display.hot_count", int(s.display.hot_count)),
        ("settings.display.latest_count", int(s.display.latest_count)),
        (
            "settings.display.recent_members",
            int(s.display.recent_members),
        ),
        (
            "settings.display.highlight_min_likes",
            int(s.display.highlight_min_likes),
        ),
        (
            "settings.display.highlight_count",
            int(s.display.highlight_count),
        ),
        ("settings.display.ranks_shown", int(s.display.ranks_shown)),
        (
            "settings.display.replies_per_page",
            int(s.display.replies_per_page),
        ),
        (
            "settings.display.sub_replies_per_page",
            int(s.display.sub_replies_per_page),
        ),
        (
            "settings.display.footer_metrics",
            flag(s.display.footer_metrics),
        ),
        (
            "settings.display.list_thumbnails",
            int(s.display.list_thumbnails),
        ),
        ("settings.domains.enabled", flag(s.domains.enabled)),
        ("settings.email.enabled", flag(s.email.enabled)),
        ("settings.feed.items", int(s.feed.items)),
        (
            "settings.forum.directory_statistics",
            flag(s.forum.directory_statistics),
        ),
        (
            "settings.governance.appeal_window_days",
            int(s.governance.appeal_window_days),
        ),
        (
            "settings.governance.appeal_handling_days",
            int(s.governance.appeal_handling_days),
        ),
        (
            "settings.governance.restoration_cooldown_days",
            int(s.governance.restoration_cooldown_days),
        ),
        (
            "settings.governance.restoration_due_days",
            int(s.governance.restoration_due_days),
        ),
        ("settings.growth.thread", int(s.growth.thread)),
        ("settings.growth.reply", int(s.growth.reply)),
        ("settings.growth.like", int(s.growth.like)),
        ("settings.growth.digest", int(s.growth.digest)),
        ("settings.growth.accept", int(s.growth.accept)),
        ("settings.growth.levels", list_text(&s.growth.levels)),
        (
            "settings.growth.level_names",
            s.growth.level_names.join("\n"),
        ),
        ("settings.growth.link_level", int(s.growth.link_level)),
        ("settings.growth.invite_level", int(s.growth.invite_level)),
        (
            "settings.growth.activity_level",
            int(s.growth.activity_level),
        ),
        ("settings.invite.enabled", flag(s.invite.enabled)),
        ("settings.invite.daily_limit", int(s.invite.daily_limit)),
        ("settings.invite.active_limit", int(s.invite.active_limit)),
        ("settings.invite.ttl_days", int(s.invite.ttl_days)),
        ("settings.invite.prefixes", s.invite.prefixes.join("\n")),
        (
            "settings.limit.message_per_minute",
            int(s.limit.message_per_minute),
        ),
        (
            "settings.limit.message_peer_per_minute",
            int(s.limit.message_peer_per_minute),
        ),
        (
            "settings.limit.interaction_per_minute",
            int(s.limit.interaction_per_minute),
        ),
        (
            "settings.limit.interaction_per_hour",
            int(s.limit.interaction_per_hour),
        ),
        (
            "settings.limit.social_per_5min",
            int(s.limit.social_per_5min),
        ),
        ("settings.limit.report_daily", int(s.limit.report_daily)),
        (
            "settings.limit.restoration_daily",
            int(s.limit.restoration_daily),
        ),
        ("settings.links.applications", flag(s.links.applications)),
        ("settings.links.home_count", int(s.links.home_count)),
        (
            "settings.links.live_per_member",
            int(s.links.live_per_member),
        ),
        (
            "settings.links.reject_cooldown_days",
            int(s.links.reject_cooldown_days),
        ),
        (
            "settings.links.min_account_days",
            int(s.links.min_account_days),
        ),
        ("settings.links.apply_per_hour", int(s.links.apply_per_hour)),
        (
            "settings.links.apply_ip_per_hour",
            int(s.links.apply_ip_per_hour),
        ),
        (
            "settings.maintenance.outbox_retention_days",
            int(s.maintenance.outbox_retention_days),
        ),
        (
            "settings.maintenance.prepared_mail_retention_days",
            int(s.maintenance.prepared_mail_retention_days),
        ),
        (
            "settings.maintenance.request_retention_days",
            int(s.maintenance.request_retention_days),
        ),
        (
            "settings.posting.first_day_threads",
            int(s.posting.first_day_threads),
        ),
        (
            "settings.posting.first_day_replies",
            int(s.posting.first_day_replies),
        ),
        (
            "settings.posting.daily_threads",
            int(s.posting.daily_threads),
        ),
        (
            "settings.posting.daily_replies",
            int(s.posting.daily_replies),
        ),
        (
            "settings.posting.new_member_hours",
            int(s.posting.new_member_hours),
        ),
        (
            "settings.posting.body_max_bytes",
            int(s.posting.body_max_bytes),
        ),
        (
            "settings.posting.edit_window_hours",
            int(s.posting.edit_window_hours),
        ),
        (
            "settings.posting.thread_interval_seconds",
            int(s.posting.thread_interval_seconds),
        ),
        (
            "settings.posting.reply_interval_seconds",
            int(s.posting.reply_interval_seconds),
        ),
        ("settings.posting.hourly_limit", int(s.posting.hourly_limit)),
        (
            "settings.posting.duplicate_check",
            flag(s.posting.duplicate_check),
        ),
        (
            "settings.posting.duplicate_window_minutes",
            int(s.posting.duplicate_window_minutes),
        ),
        (
            "settings.posting.new_member_link_block",
            flag(s.posting.new_member_link_block),
        ),
        (
            "settings.posting.new_member_link_days",
            int(s.posting.new_member_link_days),
        ),
        (
            "settings.ranks.refresh_seconds",
            int(s.ranks.refresh_seconds),
        ),
        ("settings.reading.list_sort", s.reading.list_sort.clone()),
        (
            "settings.reading.thread_view",
            s.reading.thread_view.clone(),
        ),
        (
            "settings.reading.title_latest",
            flag(s.reading.title_latest),
        ),
        (
            "settings.reading.title_new_tab",
            flag(s.reading.title_new_tab),
        ),
        (
            "settings.reading.avatar_shape",
            s.reading.avatar_shape.clone(),
        ),
        ("settings.seo.indexing", flag(s.seo.indexing)),
        ("settings.seo.robots", s.seo.robots.clone()),
        ("settings.seo.verification", s.seo.verification.clone()),
        ("settings.signup.mode", s.signup.mode.clone()),
        (
            "settings.signup.ip_hourly_limit",
            int(s.signup.ip_hourly_limit),
        ),
        ("settings.site.name", s.site.name.clone()),
        ("settings.site.description", s.site.description.clone()),
        ("settings.site.language", s.site.language.clone()),
        ("settings.site.brand_text", s.site.brand_text.clone()),
        ("settings.site.credit_name", s.site.credit_name.clone()),
        ("settings.site.index_name", s.site.index_name.clone()),
        ("settings.site.contact_email", s.site.contact_email.clone()),
        ("settings.site.url", s.site.url.clone()),
        ("settings.site.read_only", flag(s.site.read_only)),
        (
            "settings.site.read_only_message",
            s.site.read_only_message.clone(),
        ),
        ("settings.login.methods", s.login.methods.clone()),
        (
            "settings.login.participation",
            s.login.participation.clone(),
        ),
        (
            "settings.login.external_two_factor",
            s.login.external_two_factor.clone(),
        ),
        (
            "settings.sms.mobile_interval_seconds",
            int(s.sms.mobile_interval_seconds),
        ),
        (
            "settings.sms.mobile_daily_limit",
            int(s.sms.mobile_daily_limit),
        ),
        ("settings.sms.ip_hourly_limit", int(s.sms.ip_hourly_limit)),
        ("settings.sms.site_daily_limit", int(s.sms.site_daily_limit)),
        ("settings.sms.code_ttl_minutes", int(s.sms.code_ttl_minutes)),
        ("settings.sms.max_attempts", int(s.sms.max_attempts)),
        ("settings.time.offset_minutes", int(s.time.offset_minutes)),
        ("settings.time.date_format", s.time.date_format.clone()),
        ("settings.time.clock24", flag(s.time.clock24)),
        ("settings.time.relative", flag(s.time.relative)),
        ("settings.time.relative_hours", int(s.time.relative_hours)),
        ("settings.tip.enabled", flag(s.tip.enabled)),
        ("settings.tip.amounts", list_text(&s.tip.amounts)),
        ("settings.tip.daily_limit", int(s.tip.daily_limit)),
        (
            "settings.username.cooldown_days",
            int(s.username.cooldown_days),
        ),
        (
            "settings.username.history_days",
            int(s.username.history_days),
        ),
    ];
    rows.sort_by(|a, b| a.0.cmp(b.0));
    rows
}

/// All scalar settings keys, in ascending key order. This is what the page frame, the write gate and saving read
/// with primary key `IN` (`scalar_sql` in `runtime.rs`): taken directly from [`encode`], and a unit test ensures that
/// encoding and decoding correspond key by key, so adding a key only needs the usual entries in decoding and encoding
/// (plus a migration seed), and reading and locking pick it up automatically without missing it.
pub fn scalar_keys() -> Vec<&'static str> {
    encode(&SiteSettings::blank())
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

/// Encodes, then decodes by the read rules: validation before writing uses the same rules as reading.
pub fn check(candidate: &SiteSettings) -> Result<SiteSettings> {
    let rows = Rows::from_pairs(
        encode(candidate)
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    );
    decode(&rows)
}

/// The name of points: the site's own (`settings.site.credit_name`), or the language's default when it is empty.
pub fn credit_word(name: &str, lang: crate::i18n::Lang) -> String {
    if name.is_empty() {
        lang.brand_credit_name().to_owned()
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A set of values identical to the migration seeds, only for unit tests (integration tests read from the
    /// database).
    pub(crate) fn seeded() -> SiteSettings {
        let rows: Vec<(String, String)> = [
            ("settings.account.session_idle_days", "14"),
            ("settings.account.session_absolute_days", "30"),
            ("settings.account.verification_ttl_hours", "24"),
            ("settings.account.reset_ttl_minutes", "60"),
            ("settings.account.email_change_ttl_hours", "24"),
            ("settings.account.password_min_length", "12"),
            ("settings.account.login_ip_per_5min", "30"),
            ("settings.account.login_name_per_5min", "10"),
            ("settings.account.two_factor_ip_per_5min", "30"),
            ("settings.account.password_change_account_per_5min", "5"),
            ("settings.account.password_change_ip_per_5min", "30"),
            ("settings.account.forgot_email_per_minute", "1"),
            ("settings.account.forgot_email_per_hour", "5"),
            ("settings.account.forgot_ip_per_hour", "20"),
            ("settings.account.reset_token_per_5min", "10"),
            ("settings.account.reset_ip_per_5min", "30"),
            ("settings.api.enabled", "true"),
            ("settings.api.key_ttl_days", "365"),
            ("settings.api.requests_per_minute", "120"),
            ("settings.api.writes_per_minute", "20"),
            ("settings.api.auth_failures_per_5min", "30"),
            ("settings.api.max_body_kib", "128"),
            // One hour: admin saves clear the cache right away; the lifetime bounds how stale direct edits can get.
            ("settings.cache.ttl_seconds", "3600"),
            ("settings.cache.page_ttl_seconds", "60"),
            ("settings.cache.list_ttl_seconds", "60"),
            ("settings.cache.statistics_seconds", "600"),
            ("settings.cache.forum_statistics_seconds", "600"),
            ("settings.debug.panel", "false"),
            ("settings.display.threads_per_page", "20"),
            ("settings.display.hot_days", "7"),
            ("settings.display.hot_count", "10"),
            ("settings.display.latest_count", "10"),
            ("settings.display.recent_members", "8"),
            ("settings.display.highlight_min_likes", "1"),
            ("settings.display.highlight_count", "3"),
            ("settings.display.ranks_shown", "50"),
            ("settings.display.replies_per_page", "50"),
            ("settings.display.sub_replies_per_page", "20"),
            ("settings.display.footer_metrics", "true"),
            ("settings.display.list_thumbnails", "3"),
            ("settings.domains.enabled", "false"),
            ("settings.email.enabled", "false"),
            ("settings.feed.items", "30"),
            ("settings.forum.directory_statistics", "true"),
            ("settings.governance.appeal_window_days", "30"),
            ("settings.governance.appeal_handling_days", "7"),
            ("settings.governance.restoration_cooldown_days", "30"),
            ("settings.governance.restoration_due_days", "7"),
            ("settings.growth.thread", "3"),
            ("settings.growth.reply", "1"),
            ("settings.growth.like", "1"),
            ("settings.growth.digest", "10"),
            ("settings.growth.accept", "5"),
            ("settings.growth.levels", "0,20,80,200,500,1200,3000,8000"),
            ("settings.growth.level_names", ""),
            ("settings.growth.link_level", "1"),
            ("settings.growth.invite_level", "1"),
            ("settings.growth.activity_level", "1"),
            ("settings.invite.enabled", "true"),
            ("settings.invite.daily_limit", "5"),
            ("settings.invite.active_limit", "5"),
            ("settings.invite.ttl_days", "7"),
            ("settings.invite.prefixes", ""),
            ("settings.limit.message_per_minute", "30"),
            ("settings.limit.message_peer_per_minute", "10"),
            ("settings.limit.interaction_per_minute", "60"),
            ("settings.limit.interaction_per_hour", "600"),
            ("settings.limit.social_per_5min", "60"),
            ("settings.limit.report_daily", "10"),
            ("settings.limit.restoration_daily", "5"),
            ("settings.links.applications", "true"),
            ("settings.links.home_count", "12"),
            ("settings.links.live_per_member", "2"),
            ("settings.links.reject_cooldown_days", "7"),
            ("settings.links.min_account_days", "30"),
            ("settings.links.apply_per_hour", "5"),
            ("settings.links.apply_ip_per_hour", "20"),
            ("settings.maintenance.outbox_retention_days", "30"),
            ("settings.maintenance.prepared_mail_retention_days", "7"),
            ("settings.maintenance.request_retention_days", "30"),
            ("settings.posting.first_day_threads", "1"),
            ("settings.posting.first_day_replies", "5"),
            ("settings.posting.daily_threads", "5"),
            ("settings.posting.daily_replies", "20"),
            ("settings.posting.new_member_hours", "24"),
            ("settings.posting.body_max_bytes", "60000"),
            ("settings.posting.edit_window_hours", "24"),
            ("settings.posting.thread_interval_seconds", "0"),
            ("settings.posting.reply_interval_seconds", "0"),
            ("settings.posting.hourly_limit", "0"),
            ("settings.posting.duplicate_check", "false"),
            ("settings.posting.duplicate_window_minutes", "60"),
            ("settings.posting.new_member_link_block", "false"),
            ("settings.posting.new_member_link_days", "3"),
            ("settings.ranks.refresh_seconds", "60"),
            ("settings.reading.list_sort", "latest"),
            ("settings.reading.thread_view", "nested"),
            ("settings.reading.title_latest", "false"),
            ("settings.reading.title_new_tab", "false"),
            ("settings.reading.avatar_shape", "rounded"),
            ("settings.seo.indexing", "false"),
            ("settings.seo.robots", "User-agent: *\nDisallow: /manage\nDisallow: /my\nDisallow: /data\nDisallow: /appeals\nDisallow: /search\nDisallow: /api\nDisallow: /login\nDisallow: /register\nDisallow: /forgot-password\nDisallow: /reset-password\nDisallow: /verify-email"),
            ("settings.seo.verification", ""),
            ("settings.signup.mode", "invite"),
            ("settings.signup.ip_hourly_limit", "20"),
            ("settings.site.name", ""),
            ("settings.site.description", ""),
            ("settings.site.language", "en"),
            ("settings.site.brand_text", ""),
            ("settings.site.credit_name", ""),
            ("settings.site.index_name", ""),
            ("settings.site.contact_email", ""),
            ("settings.site.url", ""),
            ("settings.site.read_only", "false"),
            ("settings.site.read_only_message", ""),
            ("settings.login.methods", ""),
            ("settings.login.participation", "email"),
            ("settings.login.external_two_factor", "require"),
            ("settings.sms.mobile_interval_seconds", "60"),
            ("settings.sms.mobile_daily_limit", "5"),
            ("settings.sms.ip_hourly_limit", "10"),
            ("settings.sms.site_daily_limit", "500"),
            ("settings.sms.code_ttl_minutes", "5"),
            ("settings.sms.max_attempts", "5"),
            ("settings.time.offset_minutes", "480"),
            ("settings.time.date_format", "locale"),
            ("settings.time.clock24", "true"),
            ("settings.time.relative", "false"),
            ("settings.time.relative_hours", "72"),
            ("settings.tip.enabled", "false"),
            ("settings.tip.amounts", "1,5,10,20"),
            ("settings.tip.daily_limit", "100"),
            ("settings.username.cooldown_days", "30"),
            ("settings.username.history_days", "90"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        decode(&Rows::from_pairs(rows)).expect("the seed values are all valid")
    }

    #[test]
    fn encoding_covers_every_decoded_key_exactly_once() {
        let settings = seeded();
        let rows = encode(&settings);
        assert_eq!(
            rows.len(),
            134,
            "the encoding writes one row per scalar setting, 134 in all"
        );
        let mut keys: Vec<_> = rows.iter().map(|(key, _)| *key).collect();
        keys.dedup();
        assert_eq!(keys.len(), 134, "the encoding repeats a key");
        assert!(keys.iter().all(|key| key.starts_with("settings.")));
        for excluded in [
            "settings.mail.",
            "settings.navigation.",
            "settings.layout.",
            "settings.domain_layout.",
            "settings.layout_draft.",
            "settings.policy.",
        ] {
            assert!(
                !keys.iter().any(|key| key.starts_with(excluded)),
                "{excluded} is not a scalar setting"
            );
        }
        // Encoding then decoding equals the original value: the keys, types and values of both lists correspond one
        // to one.
        assert_eq!(check(&settings).unwrap(), settings);
        // The key list used for reading and locking is exactly the encoded keys, independent of the values (the
        // zero-value placeholder has the same keys).
        assert_eq!(scalar_keys(), keys);
    }

    #[test]
    fn every_row_is_required() {
        let rows = encode(&seeded());
        for skip in 0..rows.len() {
            let partial = Rows::from_pairs(
                rows.iter()
                    .enumerate()
                    .filter(|(index, _)| *index != skip)
                    .map(|(_, (key, value))| ((*key).to_owned(), value.clone()))
                    .collect(),
            );
            let error = decode(&partial).expect_err("a missing row is an error");
            assert_eq!(error.key, rows[skip].0);
            assert_eq!(error.found, None);
        }
    }

    #[test]
    fn hard_bounds_and_relations_are_enforced() {
        let mut value = seeded();
        value.cache.ttl_seconds = 0;
        assert!(
            check(&value).is_ok(),
            "a lifetime of 0 turns the process cache off and is valid"
        );
        value.cache.ttl_seconds = MAX_CACHE_TTL_SECONDS + 1;
        assert_eq!(check(&value).unwrap_err().key, "settings.cache.ttl_seconds");

        let mut value = seeded();
        value.signup.mode = "anything".into();
        assert_eq!(check(&value).unwrap_err().key, "settings.signup.mode");

        let mut value = seeded();
        value.posting.daily_threads = 0;
        value.posting.first_day_threads = 1;
        assert_eq!(
            check(&value).unwrap_err().key,
            "settings.posting.daily_threads"
        );
        value.posting.daily_threads = 2;
        value.posting.first_day_threads = 3;
        assert_eq!(
            check(&value).unwrap_err().key,
            "settings.posting.daily_threads"
        );

        let mut value = seeded();
        value.username.history_days = 10;
        assert_eq!(
            check(&value).unwrap_err().key,
            "settings.username.history_days"
        );

        let mut value = seeded();
        assert!(
            value.growth.level_names.is_empty(),
            "the seed leaves the level names empty"
        );
        value.growth.level_names = vec!["Level".into(); 7];
        assert_eq!(
            check(&value).unwrap_err().key,
            "settings.growth.level_names"
        );
        value.growth.level_names.push("Top level".into());
        assert!(check(&value).is_ok());

        let mut value = seeded();
        value.site.language = "fr".into();
        assert_eq!(check(&value).unwrap_err().key, "settings.site.language");
        value.site.language = "zh-CN".into();
        assert!(check(&value).is_ok());
        value.site.brand_text = "x".repeat(41);
        assert_eq!(check(&value).unwrap_err().key, "settings.site.brand_text");
        value.site.brand_text = "Sample\nForum".into();
        assert_eq!(check(&value).unwrap_err().key, "settings.site.brand_text");
        value.site.brand_text = "Sample Forum".into();
        value.site.credit_name = "x".repeat(11);
        assert_eq!(check(&value).unwrap_err().key, "settings.site.credit_name");

        // Level thresholds cannot exceed the number of levels: 8 levels are seeded, and level 9 does not exist.
        let mut value = seeded();
        value.growth.invite_level = 9;
        assert_eq!(
            check(&value).unwrap_err().key,
            "settings.growth.invite_level"
        );
        value.growth.invite_level = 8;
        assert!(check(&value).is_ok());

        let mut value = seeded();
        value.tip.daily_limit = 10;
        assert_eq!(check(&value).unwrap_err().key, "settings.tip.daily_limit");
        value.tip.amounts = vec![5, 1];
        assert_eq!(check(&value).unwrap_err().key, "settings.tip.amounts");

        let mut value = seeded();
        value.site.url = "https://example.com/".into();
        assert_eq!(check(&value).unwrap_err().key, "settings.site.url");
        value.site.url = "https://example.com".into();
        assert!(check(&value).is_ok());

        let mut value = seeded();
        value.seo.verification = "google-site-verification=abc_DEF-1".into();
        assert!(check(&value).is_ok());
        for bad in ["Robots=x", "a=b c", "=x", "a=", "a b=c"] {
            value.seo.verification = bad.into();
            assert!(check(&value).is_err(), "{bad}");
        }
        let mut value = seeded();
        value.seo.robots = "User-agent: *\r\nDisallow: /".into();
        assert_eq!(check(&value).unwrap_err().key, "settings.seo.robots");
    }

    /// Invite code prefixes: the empty table is valid; each is 2–6 letters, digits, underscores or hyphens, all
    /// different, at most 50;
    /// matching is case-sensitive, and when several match the longest is taken.
    #[test]
    fn invite_prefixes_are_bounded_and_matched_longest_first() {
        let mut value = seeded();
        assert!(value.invite.prefixes.is_empty());
        value.invite.prefixes = vec!["ho".into(), "hophot".into(), "Ab_9-".into()];
        let checked = check(&value).unwrap();
        assert_eq!(checked.invite.prefixes, value.invite.prefixes);
        assert_eq!(checked.invite.prefix_of("hophoto43saf242"), Some("hophot"));
        assert_eq!(checked.invite.prefix_of("hophx"), Some("ho"));
        assert_eq!(checked.invite.prefix_of("ho"), Some("ho"));
        assert_eq!(
            checked.invite.prefix_of("HOPHoto"),
            None,
            "matching is case-sensitive"
        );
        assert_eq!(
            checked.invite.prefix_of("xhoph"),
            None,
            "only the start is matched"
        );
        for bad in [
            vec!["h".to_owned()],
            vec!["h".repeat(7)],
            vec!["ho ph".to_owned()],
            // Not ASCII: the Chinese for "coin prefix", four three-byte characters.
            vec!["\u{91d1}\u{5e01}\u{524d}\u{7f00}".to_owned()],
            vec!["hoph".to_owned(), "hoph".to_owned()],
            vec!["hoph".to_owned(), String::new()],
            (0..51).map(|n| format!("p{n:03}")).collect(),
        ] {
            value.invite.prefixes = bad.clone();
            assert_eq!(
                check(&value).unwrap_err().key,
                "settings.invite.prefixes",
                "{bad:?}"
            );
        }
        value.invite.prefixes = (0..50).map(|n| format!("p{n:03}")).collect();
        assert!(check(&value).is_ok());
    }
}
