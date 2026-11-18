//! Debug information for the current request, which can be turned off; not persisted and not aggregated across
//! requests.
pub mod driver;
pub mod explain;
mod process;
pub mod trace;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use askama::{
    Template,
    filters::{Html, HtmlSafeOutput, escape},
};
use axum::{
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::db::Database;
use explain::IndexUsage;
use process::ProcessSample;
use trace::{QueryRecord, QuerySource, RequestProfile};

/// Only written into the response by the primary database identity middleware; client request headers cannot grant
/// access to the panel.
#[derive(Clone)]
pub struct Authorized;

/// State of the debug layer: the database (for `EXPLAIN`), the Redis switch and the in-process global cache (to read
/// `settings.debug.panel`).
#[derive(Clone)]
pub struct DebugState {
    pub database: Database,
    pub redis_enabled: bool,
    pub global: crate::cache::GlobalCache,
}

/// Mounted outside the entry layer (`site::entry`), so what site resolution reads is part of the request too; each
/// request decides whether to collect by the admin switch `settings.debug.panel` ("Tools & maintenance › Maintenance",
/// docs/settings-and-caching.md, "Profiling"); there is no configuration or environment switch for it. The switch is
/// read inside the request's profile, so the panel lists that read when it goes to the database (a cache miss, or a
/// cache lifetime of 0); when the settings cannot be read (bad value, missing row) nothing is collected, and the page
/// itself shows the settings error page. Static/protocol responses are left unchanged.
pub async fn middleware(State(state): State<DebugState>, request: Request, next: Next) -> Response {
    if request.method() == Method::HEAD || skipped_before_entry(request.uri().path()) {
        return next.run(request).await;
    }
    let request_path = request.uri().to_string();
    let method = request.method().to_string();
    let started = Instant::now();
    let surrounding = trace::current();
    let profile = Arc::new(RequestProfile::new());
    trace::scope(profile.clone(), async move {
        let settings = state.global.settings(&state.database).await.ok();
        let Some(settings) = settings.filter(|block| block.settings.debug.panel) else {
            return trace::resume(surrounding, next.run(request)).await;
        };
        let before = ProcessSample::capture();
        let response = next.run(request).await;
        if profile.skipped() || response.extensions().get::<Authorized>().is_none() {
            return response;
        }
        // The panel speaks the request's language and links under its prefix, which the entry layer resolved inside.
        let site = response
            .extensions()
            .get::<crate::site::Site>()
            .cloned()
            .unwrap_or_default();
        let page = Page {
            request_path,
            method,
            started,
            before,
        };
        crate::site::scope(site, with_panel(state, &settings, &profile, page, response)).await
    })
    .await
}

/// Requests that never get a panel, told apart before anything is read: a health check must not wait here for a
/// database connection to read the switch. This layer sees the path before a sub-path prefix is stripped, so only the
/// root addresses are matched here (and the health check under any prefix); the entry layer checks the stripped path
/// again ([`unprofiled`]).
fn skipped_before_entry(path: &str) -> bool {
    unprofiled_path(path) || path.ends_with("/healthz")
}

/// Static files, the health check, robots.txt and sensitive pages (`urls::sensitive_page`): the panel neither collects
/// nor shows anything for them. `path` has no sub-path prefix.
pub(crate) fn unprofiled_path(path: &str) -> bool {
    path == "/static"
        || path.starts_with("/static/")
        || crate::urls::sensitive_page(path)
        || matches!(path, "/healthz" | "/robots.txt")
}

/// For the entry layer, once the prefix is stripped and the path is one of [`unprofiled_path`]: runs the rest of the
/// request without collecting, and keeps the panel off its response (what site resolution recorded is dropped).
pub(crate) async fn unprofiled<F: Future>(future: F) -> F::Output {
    if let Some(profile) = trace::current() {
        profile.skip();
    }
    trace::without(future).await
}

/// What the middleware noted about the request before running it.
struct Page {
    request_path: String,
    method: String,
    started: Instant,
    before: ProcessSample,
}

/// Adds the panel to an HTML page answered to an administrator.
async fn with_panel(
    state: DebugState,
    settings: &crate::cache::SettingsBlock,
    profile: &RequestProfile,
    page: Page,
    response: Response,
) -> Response {
    // The panel names the credit-rules cache after the site's points.
    let credit = settings.settings.site.credit_word(crate::i18n::current());
    let DebugState {
        database,
        redis_enabled,
        ..
    } = state;
    let Page {
        request_path,
        method,
        started,
        before,
    } = page;
    let html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
        });
    if !html
        || response.status().is_redirection()
        || response.status() == StatusCode::NO_CONTENT
        || response.headers().contains_key(header::CONTENT_ENCODING)
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "failed to read the page response for the debug panel");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                crate::i18n::current().common_error_render(),
            )
                .into_response();
        }
    };
    // The full SSR body has been generated; rendering the debug panel itself and network transfer are not counted
    // in the page time.
    let elapsed = started.elapsed();
    let after = ProcessSample::capture();
    let Ok(html) = std::str::from_utf8(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let Some(position) = html.rfind("</body>") else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let records = profile.snapshot();
    let diagnostic_started = Instant::now();
    let index_usage = explain::collect(&database, &records).await;
    let template = DebugTemplate::new(
        request_path,
        method,
        parts.status.as_u16(),
        elapsed,
        before,
        after,
        records,
    );
    let template = if redis_enabled {
        template.with_redis(profile.redis_snapshot())
    } else {
        template
    }
    .with_blocks(profile.block_snapshot(), &credit)
    .with_layout(profile.layout_snapshot())
    .with_index_usage(index_usage, diagnostic_started.elapsed());
    let panel = match template.render() {
        Ok(panel) => panel,
        Err(error) => {
            tracing::error!(%error, "failed to render the debug panel");
            return Response::from_parts(parts, Body::from(bytes));
        }
    };
    let mut output = String::with_capacity(html.len() + panel.len());
    output.push_str(&html[..position]);
    output.push_str(&panel);
    output.push_str(&html[position..]);
    // Debug data is delivered only with this response; after the body is modified, the original length and
    // entity tag must not be kept.
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::ETAG);
    parts.headers.remove(header::LAST_MODIFIED);
    parts
        .headers
        .insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    Response::from_parts(parts, Body::from(output))
}

/// One read of the in-process global cache: block name, whether it hit, and how long ago it was loaded.
struct BlockUse {
    sequence: usize,
    block: String,
    /// The display name from the registry (`cache::registry`) in this request's language.
    label: String,
    outcome: &'static str,
    /// A block that hit sends no SQL; a reload from the source and an uncached read both do.
    hit: bool,
    age: String,
}

struct RedisCommand {
    sequence: usize,
    command: String,
    key: String,
    elapsed: String,
    outcome: String,
    failed: bool,
}

#[derive(Template)]
#[template(path = "debug/panel.html")]
struct DebugTemplate {
    t: crate::i18n::Lang,
    request_path: String,
    method: String,
    status: u16,
    duration: String,
    sql_count: usize,
    sql_duration: String,
    rss: String,
    rss_change: String,
    cpu: String,
    cpu_user: String,
    cpu_system: String,
    index_duration: String,
    /// With Redis disabled the whole section is not rendered: there are no round trips to show.
    redis_enabled: bool,
    redis_count: usize,
    redis_duration: String,
    redis_hits: usize,
    redis_misses: usize,
    redis_commands: Vec<RedisCommand>,
    /// In-process global cache: which data did not hit the database this time (all "uncached" when
    /// `settings.cache.ttl_seconds` is 0).
    block_hits: usize,
    blocks: Vec<BlockUse>,
    /// The page layout's blocks with their times, and those skipped with the reason.
    layout: Vec<LayoutUse>,
    queries: Vec<DebugQuery>,
}

struct LayoutUse {
    sequence: usize,
    place: &'static str,
    id: String,
    source: &'static str,
    elapsed: String,
    /// Empty when shown.
    skipped: &'static str,
}

struct DebugQuery {
    sequence: usize,
    driver: bool,
    database: String,
    sql: HtmlSafeOutput<String>,
    elapsed: String,
    rows_returned: u64,
    rows_affected: String,
    error: String,
    cancelled: bool,
    expansion_error: String,
    parameters: String,
    index_status: &'static str,
    index_summary: String,
}

impl DebugTemplate {
    fn new(
        request_path: String,
        method: String,
        status: u16,
        elapsed: Duration,
        before: ProcessSample,
        after: ProcessSample,
        records: Vec<QueryRecord>,
    ) -> Self {
        let cpu_user = duration_delta(before.cpu_user, after.cpu_user);
        let cpu_system = duration_delta(before.cpu_system, after.cpu_system);
        let cpu = cpu_user
            .zip(cpu_system)
            .and_then(|(user, system)| user.checked_add(system));
        let sql_duration = records.iter().map(|query| query.elapsed).sum();
        let t = crate::i18n::current();
        Self {
            t,
            request_path,
            method,
            status,
            duration: duration(elapsed),
            sql_count: records.len(),
            sql_duration: duration(sql_duration),
            rss: after.rss_bytes.map(memory).unwrap_or_else(unavailable),
            rss_change: before
                .rss_bytes
                .zip(after.rss_bytes)
                .map(|(before, after)| {
                    format!("{:+.2} MiB", (after as f64 - before as f64) / 1_048_576.0)
                })
                .unwrap_or_else(unavailable),
            cpu: cpu.map(duration).unwrap_or_else(unavailable),
            cpu_user: cpu_user.map(duration).unwrap_or_else(unavailable),
            cpu_system: cpu_system.map(duration).unwrap_or_else(unavailable),
            index_duration: duration(Duration::ZERO),
            redis_enabled: false,
            redis_count: 0,
            redis_duration: duration(Duration::ZERO),
            redis_hits: 0,
            redis_misses: 0,
            redis_commands: Vec::new(),
            block_hits: 0,
            blocks: Vec::new(),
            layout: Vec::new(),
            queries: records
                .into_iter()
                .map(|query| DebugQuery {
                    sequence: query.sequence,
                    driver: query.source == QuerySource::Driver,
                    database: query.backend.label().into(),
                    parameters: if query.expansion_error.is_some() {
                        format!("{:?}", query.parameters)
                    } else {
                        String::new()
                    },
                    sql: sql_html(&if query.expansion_error.is_some() {
                        query.sql
                    } else {
                        query.expanded_sql
                    }),
                    elapsed: duration(query.elapsed),
                    rows_returned: query.rows_returned,
                    rows_affected: query
                        .rows_affected
                        .map(|count| count.to_string())
                        .unwrap_or_else(unavailable),
                    error: query.error.unwrap_or_default(),
                    cancelled: query.cancelled,
                    expansion_error: query.expansion_error.unwrap_or_default(),
                    index_status: "skipped",
                    index_summary: t.debug_explain_not_collected().into(),
                })
                .collect(),
        }
    }

    fn with_blocks(mut self, records: Vec<trace::BlockRecord>, credit: &str) -> Self {
        use trace::BlockOutcome;
        let t = self.t;
        self.block_hits = records
            .iter()
            .filter(|record| record.outcome == BlockOutcome::Hit)
            .count();
        self.blocks = records
            .into_iter()
            .map(|record| BlockUse {
                sequence: record.sequence,
                label: crate::cache::registry::slot(&record.block)
                    .map(|entry| entry.label(t, credit))
                    .unwrap_or_default(),
                block: record.block,
                outcome: match record.outcome {
                    BlockOutcome::Hit => t.debug_block_hit(),
                    BlockOutcome::Filled => t.debug_block_filled(),
                    BlockOutcome::Uncached => t.debug_block_uncached(),
                },
                hit: record.outcome == BlockOutcome::Hit,
                age: record
                    .age
                    .map(|age| t.debug_block_age(age.as_secs()))
                    .unwrap_or_default(),
            })
            .collect();
        self
    }

    fn with_layout(mut self, records: Vec<trace::LayoutRecord>) -> Self {
        use trace::LayoutSkip;
        let t = self.t;
        self.layout = records
            .into_iter()
            .map(|record| LayoutUse {
                sequence: record.sequence,
                place: record.place,
                id: record.id,
                source: record.source,
                elapsed: duration(record.elapsed),
                skipped: match record.skipped {
                    None => "",
                    Some(LayoutSkip::Audience) => t.debug_layout_skip_audience(),
                    Some(LayoutSkip::Empty) => t.debug_layout_skip_empty(),
                    Some(LayoutSkip::Budget) => t.debug_layout_skip_budget(),
                    Some(LayoutSkip::Failed) => t.debug_layout_skip_failed(),
                },
            })
            .collect();
        self
    }

    /// Not called with Redis disabled: the whole section is not rendered.
    fn with_redis(mut self, records: Vec<trace::RedisRecord>) -> Self {
        use trace::RedisOutcome;
        self.redis_enabled = true;
        self.redis_count = records.len();
        self.redis_duration = duration(records.iter().map(|record| record.elapsed).sum());
        self.redis_hits = records
            .iter()
            .filter(|record| record.outcome == RedisOutcome::Hit)
            .count();
        self.redis_misses = records
            .iter()
            .filter(|record| record.outcome == RedisOutcome::Miss)
            .count();
        self.redis_commands = records
            .into_iter()
            .map(|record| RedisCommand {
                sequence: record.sequence,
                command: record.command,
                key: record.key,
                elapsed: duration(record.elapsed),
                failed: matches!(record.outcome, RedisOutcome::Failed(_)),
                outcome: match record.outcome {
                    RedisOutcome::Hit => self.t.debug_redis_hit().into(),
                    RedisOutcome::Miss => self.t.debug_redis_miss().into(),
                    RedisOutcome::Ok => self.t.debug_query_ok().into(),
                    RedisOutcome::Failed(message) => message,
                },
            })
            .collect();
        self
    }

    fn with_index_usage(mut self, usage: Vec<IndexUsage>, elapsed: Duration) -> Self {
        self.index_duration = duration(elapsed);
        for (query, usage) in self.queries.iter_mut().zip(usage) {
            query.index_status = usage.status;
            query.index_summary = usage.summary;
        }
        self
    }
}

fn unavailable() -> String {
    crate::i18n::current().debug_unavailable().into()
}

fn sql_html(sql: &str) -> HtmlSafeOutput<String> {
    // Escape HTML with Askama first, then encode CR; browsers do not normalize a CR in a character reference to LF.
    // So carriage returns in plain SQL text and inside literals can be copied intact through textContent.
    HtmlSafeOutput(
        escape(sql, Html)
            .unwrap()
            .to_string()
            .replace('\r', "&#13;"),
    )
}
fn duration(value: Duration) -> String {
    format!("{:.3} ms", value.as_secs_f64() * 1000.0)
}
fn memory(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
}
fn duration_delta(before: Option<Duration>, after: Option<Duration>) -> Option<Duration> {
    after
        .zip(before)
        .and_then(|(after, before)| after.checked_sub(before))
}

/// Error text shown directly to visitors: the full error chain, with passwords in connection strings redacted.
///
/// MySQL and Redis errors such as a missing table are output as is, not collapsed into one generic message — seeing
/// `1146 … Table 'x' doesn't exist` is what lets you locate the problem on the spot, and hiding the database name buys
/// little security. Credentials must still be blocked: connection failures put the URL into the message, and a form
/// like `redis://:password@host` on an error page would publish the password.
pub fn visible_error(error: &anyhow::Error) -> String {
    // Join the error chain layer by layer, skipping those already contained in the previous layer: sqlx's Display
    // already includes the driver's original text, and using `{:#}` directly would print the same
    // `1146 … doesn't exist` twice.
    let mut text = String::new();
    for cause in error.chain() {
        let message = cause.to_string();
        if message.is_empty() || text.contains(&message) {
            continue;
        }
        if !text.is_empty() {
            text.push_str(": ");
        }
        text.push_str(&message);
    }
    // Same limit as when delivery failure reasons are written to the database, so one unusually long error cannot
    // blow up the page.
    let text: String = redact_credentials(&text).chars().take(1000).collect();
    text
}

/// Replaces the password in URL userinfo with `***`, changing nothing else.
///
/// Only `scheme://[user][:password]@` is recognized: userinfo comes before `@`, and `@` only counts if it comes before
/// a path separator or whitespace, otherwise `http://host/a@b` would be misread.
fn redact_credentials(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(scheme_end) = rest.find("://") {
        let authority = scheme_end + 3;
        output.push_str(&rest[..authority]);
        rest = &rest[authority..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\'' | ')'))
            .unwrap_or(rest.len());
        match rest[..end].find('@') {
            Some(at) => {
                let userinfo = &rest[..at];
                match userinfo.find(':') {
                    Some(colon) => output.push_str(&format!("{}:***", &userinfo[..colon])),
                    None => output.push_str(userinfo),
                }
                rest = &rest[at..];
            }
            None => {
                output.push_str(&rest[..end]);
                rest = &rest[end..];
            }
        }
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_masked_but_the_rest_of_the_error_survives() {
        let cases = [
            (
                "error: pool timed out: mysql://gonof:s3cr3t@192.0.2.10:3306/gonof",
                "error: pool timed out: mysql://gonof:***@192.0.2.10:3306/gonof",
            ),
            (
                "Redis connection failed: redis://:example-redis-password@192.0.2.10:6380/0 timed out",
                "Redis connection failed: redis://:***@192.0.2.10:6380/0 timed out",
            ),
            // URLs without userinfo are kept as is, otherwise site URLs in errors would get chewed up.
            (
                "canonical https://example.com/thread-1 cannot be parsed",
                "canonical https://example.com/thread-1 cannot be parsed",
            ),
            // An `@` in the path is not userinfo.
            ("http://192.0.2.10/a@b 404", "http://192.0.2.10/a@b 404"),
            // A user name without a password is left alone: it is not a credential by itself.
            (
                "mysql://root@127.0.0.1:3308/mysql",
                "mysql://root@127.0.0.1:3308/mysql",
            ),
            // Several connection strings in one message must all be handled.
            (
                "mysql://a:b@h1 and redis://:c@h2 are both unavailable",
                "mysql://a:***@h1 and redis://:***@h2 are both unavailable",
            ),
            // Key diagnostic information must stay exactly as it was.
            (
                "1146 (42S02): Table 'gonof.gonof_common_setting' doesn't exist",
                "1146 (42S02): Table 'gonof.gonof_common_setting' doesn't exist",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_credentials(input), expected, "input: {input}");
        }
    }

    #[test]
    fn the_whole_error_chain_reaches_the_page() {
        let error = anyhow::anyhow!("1146 (42S02): Table 'x' doesn't exist")
            .context("mysql://u:p@h/db read failed");
        let text = visible_error(&error);
        assert!(text.contains("1146 (42S02)"), "{text}");
        assert!(text.contains("read failed"), "{text}");
        assert!(
            text.contains("mysql://u:***@h/db") && !text.contains(":p@"),
            "{text}"
        );
    }

    #[test]
    fn sql_html_escapes_markup_and_preserves_literal_carriage_returns() {
        assert_eq!(
            sql_html("SELECT '<script>&#13;\r\n'").to_string(),
            "SELECT &#39;&#60;script&#62;&#38;#13;&#13;\n&#39;"
        );
    }

    #[test]
    fn driver_batches_show_reported_rows_without_inventing_success() {
        let profile = RequestProfile::new();
        profile.record_driver(
            crate::db::Backend::MySql,
            "SET NAMES utf8mb4;".into(),
            Duration::from_micros(15),
            2,
            Some(0),
        );
        let html = DebugTemplate::new(
            "/".into(),
            "GET".into(),
            200,
            Duration::from_millis(1),
            ProcessSample::default(),
            ProcessSample::default(),
            profile.snapshot(),
        )
        .render()
        .unwrap();
        assert!(html.contains("data-debug-query-status>No result reported"));
        assert!(!html.contains("data-debug-query-status>Succeeded"));
        assert!(html.contains("Driver time 0.015 ms"));
        assert!(html.contains("Rows returned by the driver 2 · affected 0"));
        assert!(html.contains("Unavailable"));
    }

    #[test]
    fn missing_or_reset_resource_counters_remain_unavailable() {
        let template = DebugTemplate::new(
            "/".into(),
            "GET".into(),
            200,
            Duration::ZERO,
            ProcessSample::default(),
            ProcessSample::default(),
            Vec::new(),
        );
        assert_eq!(template.rss, "Unavailable");
        assert_eq!(template.rss_change, "Unavailable");
        assert_eq!(template.cpu, "Unavailable");
        assert_eq!(
            duration_delta(Some(Duration::from_secs(2)), Some(Duration::from_secs(1))),
            None
        );
    }

    #[test]
    fn formats_process_deltas_without_claiming_request_allocations() {
        let before = ProcessSample {
            rss_bytes: Some(2_097_152),
            cpu_user: Some(Duration::from_millis(8)),
            cpu_system: Some(Duration::from_millis(3)),
        };
        let after = ProcessSample {
            rss_bytes: Some(1_048_576),
            cpu_user: Some(Duration::from_millis(12)),
            cpu_system: Some(Duration::from_millis(4)),
        };
        let template = DebugTemplate::new(
            "/".into(),
            "GET".into(),
            200,
            Duration::from_millis(20),
            before,
            after,
            Vec::new(),
        );
        assert_eq!(template.rss, "1.00 MiB");
        assert_eq!(template.rss_change, "-1.00 MiB");
        assert_eq!(template.cpu, "5.000 ms");
    }
}
