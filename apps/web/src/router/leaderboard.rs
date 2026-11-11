//! Leaderboards `/ranks`, the credits rules page `/credits/rules`, the member's own growth history `/my/credits/growth`
//! and admin growth point adjustments. The rules for leaderboards and growth points are in [`crate::ranks`] and
//! [`crate::growth`].
use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::{
    growth, identity,
    ranks::{self, Board, Period},
};
use axum::routing::post;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ranks", get(ranks_page))
        .route("/credits/rules", get(rules_page))
        .route("/my/credits/growth", get(growth_page))
        .route("/manage/credits/growth", post(adjust))
}

#[derive(Default, Deserialize)]
struct RanksQuery {
    board: Option<String>,
    period: Option<String>,
}

struct RankRow {
    rank: usize,
    username: String,
    avatar: AvatarView,
    level: growth::Level,
    score: i64,
}

#[derive(Template)]
#[template(path = "site/ranks.html")]
struct RanksTemplate<'a> {
    p: PageMeta<'a>,
    board_label: &'static str,
    period_label: &'static str,
    unit: &'static str,
    boards: Vec<Tab>,
    periods: Vec<Tab>,
    rows: Vec<RankRow>,
    /// "updated …"; empty when not computed yet.
    updated: String,
    refresh_seconds: i64,
}

fn ranks_href(board: Board, period: Period) -> String {
    format!("/ranks?board={}&period={}", board.slug(), period.slug())
}

/// Leaderboards: viewable anonymously; defaults to the 30-day growth points board.
async fn ranks_page(
    Page(state): Page,
    query: Result<Query<RanksQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return error_page(
            &state,
            StatusCode::BAD_REQUEST,
            crate::i18n::current().site_ranks_error_query(),
        );
    };
    let board = match query.board.as_deref() {
        None => Board::Growth,
        Some(value) => match Board::parse(value) {
            Some(board) => board,
            None => {
                return error_page(
                    &state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().site_ranks_error_board(),
                );
            }
        },
    };
    let period = match query.period.as_deref() {
        None => Period::Month,
        Some(value) => match Period::parse(value) {
            Some(period) => period,
            None => {
                return error_page(
                    &state,
                    StatusCode::NOT_FOUND,
                    crate::i18n::current().site_ranks_error_period(),
                );
            }
        },
    };
    let viewer = state.viewer.session.as_ref().map(|s| s.member.uid);
    let snapshot = match ranks::read(&state.db, board, period, viewer).await {
        Ok(snapshot) => snapshot,
        Err(error) => return database_error(&state, error),
    };
    let clock = crate::clock::current();
    let settings = &state.frame.settings;
    // The snapshot stores 100 places; the page shows the top `settings.display.ranks_shown` (already shifted down by
    // the members' current status when the board is read).
    let shown = usize::try_from(settings.display.ranks_shown).unwrap_or(0);
    let rows = snapshot
        .standings
        .into_iter()
        .take(shown)
        .map(|standing| RankRow {
            rank: standing.rank,
            avatar: avatar_view(
                Some(standing.uid),
                standing.avatar_aid.unwrap_or(0),
                standing.avatar,
                &standing.username,
            ),
            username: standing.username,
            level: settings.growth.level(standing.growth),
            score: standing.score,
        })
        .collect();
    let boards = Board::ALL
        .into_iter()
        .map(|item| Tab {
            label: item.label(),
            href: ranks_href(item, period),
            active: item == board,
        })
        .collect();
    let periods = Period::ALL
        .into_iter()
        .map(|item| Tab {
            label: item.label(),
            href: ranks_href(board, item),
            active: item == period,
        })
        .collect();
    let path = if query.board.is_none() && query.period.is_none() {
        "/ranks".to_owned()
    } else {
        ranks_href(board, period)
    };
    let mut p = meta(
        &state,
        crate::i18n::current().site_ranks_title(board.label(), period.label()),
        crate::i18n::current().site_ranks_description().into(),
        &path,
        "",
        "",
    );
    if let Some(error) = super::member_search::fill_sidebar(&state, &mut p).await {
        return error;
    }
    render(&RanksTemplate {
        p,
        board_label: board.label(),
        period_label: period.label(),
        unit: board.unit(),
        boards,
        periods,
        rows,
        updated: snapshot
            .computed_at
            .map(|at| crate::clock::datetime(at, &clock))
            .unwrap_or_default(),
        refresh_seconds: settings.ranks.refresh_seconds,
    })
}

struct CreditRuleRow {
    label: &'static str,
    enabled: bool,
    amount: i64,
    daily_limit: i64,
}

struct LevelRow {
    number: usize,
    name: String,
    floor: i64,
}

#[derive(Template)]
#[template(path = "site/credits_rules.html")]
struct RulesTemplate<'a> {
    p: PageMeta<'a>,
    credit_rules: Vec<CreditRuleRow>,
    growth: crate::runtime::GrowthSettings,
    levels: Vec<LevelRow>,
    daily_threads: i64,
    daily_replies: i64,
    refresh_seconds: i64,
    ranks_shown: i64,
}

/// Credits rules (public page): credits rules come from the credits rule table; growth point values, levels, posting
/// quotas and leaderboard rules come from the site settings.
async fn rules_page(Page(state): Page) -> Response {
    let rules = match state
        .global
        .credit_rules(&state.db, state.cache_ttl())
        .await
    {
        Ok(rules) => (*rules).clone(),
        Err(error) => return database_error(&state, error),
    };
    let order = ["signin", "thread", "reply"];
    let mut credit_rules: Vec<_> = rules
        .into_iter()
        .filter_map(|rule| {
            let label = match rule.key.as_str() {
                "signin" => crate::i18n::current().site_credit_rule_signin(),
                "thread" => crate::i18n::current().site_credit_rule_thread(),
                "reply" => crate::i18n::current().site_credit_rule_reply(),
                _ => return None,
            };
            Some((
                order.iter().position(|key| *key == rule.key).unwrap_or(0),
                CreditRuleRow {
                    label,
                    enabled: rule.enabled,
                    amount: rule.amount,
                    daily_limit: rule.daily_limit,
                },
            ))
        })
        .collect();
    credit_rules.sort_by_key(|(index, _)| *index);
    let settings = &state.frame.settings;
    let config = &settings.growth;
    let levels = config
        .levels
        .iter()
        .enumerate()
        .map(|(index, floor)| LevelRow {
            number: index + 1,
            name: config.name_of(index).unwrap_or_default(),
            floor: *floor,
        })
        .collect();
    render(&RulesTemplate {
        p: meta(
            &state,
            crate::i18n::current().site_credits_title().into(),
            crate::i18n::current()
                .site_credits_description(state.frame.settings.site.credit_name.as_str()),
            "/credits/rules",
            "",
            "",
        ),
        credit_rules: credit_rules.into_iter().map(|(_, row)| row).collect(),
        growth: config.clone(),
        levels,
        daily_threads: settings.posting.daily_threads,
        daily_replies: settings.posting.daily_replies,
        refresh_seconds: settings.ranks.refresh_seconds,
        ranks_shown: settings.display.ranks_shown,
    })
}

#[derive(Default, Deserialize)]
struct GrowthQuery {
    #[serde(default)]
    before: i64,
}

struct GrowthRow {
    label: String,
    entry: growth::Entry,
    time: String,
    href: String,
}

#[derive(Template)]
#[template(path = "activities/growth.html")]
struct GrowthTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    level: growth::Level,
    rows: Vec<GrowthRow>,
    next_before: i64,
}

/// The member's own growth history, 50 entries per page, newest first.
async fn growth_page(Page(state): Page, Query(query): Query<GrowthQuery>) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect("/login?return_to=%2Fmy%2Fcredits%2Fgrowth");
    };
    let uid = session.member.uid;
    let page = match growth::entries(&state.db, uid, query.before.max(0)).await {
        Ok(page) => page,
        Err(error) => return database_error(&state, error),
    };
    let level = match growth::growth(&state.db, uid).await {
        Ok(value) => state.frame.settings.growth.level(value),
        Err(error) => return database_error(&state, error),
    };
    let clock = crate::clock::current();
    let rows = page
        .entries
        .into_iter()
        .map(|entry| GrowthRow {
            label: growth::kind_label(&entry.kind).to_owned(),
            time: crate::clock::datetime(entry.dateline, &clock),
            href: entry
                .pid
                .map(|pid| format!("/posts/{pid}"))
                .unwrap_or_default(),
            entry,
        })
        .collect();
    let mut p = meta(
        &state,
        crate::i18n::current().activity_growth_title().into(),
        crate::i18n::current().activity_growth_description().into(),
        "/my/credits/growth",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(&state, &mut p).await {
        return error;
    }
    render(&GrowthTemplate {
        p,
        frame: crate::router::my::frame("growth"),
        level,
        rows,
        next_before: page.next_before,
    })
}

#[derive(Deserialize)]
struct AdjustForm {
    _csrf: String,
    request_key: String,
    uid: i64,
    amount: i64,
    reason: String,
}
csrf_forms!(AdjustForm);

/// Admin adjustment of growth points: the administrator enters a reason; recorded in the history and the action log.
async fn adjust(Page(state): Page, CsrfForm(form): CsrfForm<AdjustForm>) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect("/login?return_to=%2Fmanage%2Fcredits");
    };
    if !session.member.is_admin {
        return error_page(
            &state,
            StatusCode::FORBIDDEN,
            crate::i18n::current().activity_manage_error_growth_admin(),
        );
    }
    match growth::adjust(
        &state.db,
        session.member.actor(),
        form.uid,
        form.amount,
        &form.request_key,
        &form.reason,
        identity::now(),
    )
    .await
    {
        Ok(()) => auth::redirect("/manage/credits?done=growth"),
        Err(error) => match error.downcast_ref::<crate::activities::ActivityFailure>() {
            Some(failure) => error_page(&state, StatusCode::UNPROCESSABLE_ENTITY, &failure.0),
            None => database_error(&state, error),
        },
    }
}

/// Description of the credits sources in "Earned today".
pub(super) fn credit_gain_label(kind: &str) -> &'static str {
    match kind {
        "signin" => crate::i18n::current().activity_ledger_signin(),
        "thread" => crate::i18n::current().activity_ledger_thread_reward(),
        "reply" => crate::i18n::current().activity_ledger_reply_reward(),
        "admin_adjust" => crate::i18n::current().activity_ledger_adjust(),
        "activity_reward" => crate::i18n::current().activity_gain_reward(),
        "activity_refund" => crate::i18n::current().activity_gain_refund(),
        _ => crate::i18n::current().activity_gain_other(),
    }
}

/// One row of "Earned today".
pub(super) struct GainRow {
    pub(super) label: String,
    pub(super) total: i64,
}
