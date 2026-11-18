//! Private balances, staff controls and progressively enhanced activity actions.
use super::extract::{CsrfForm, Page, csrf_forms};
use super::*;
use crate::rate_limit::Rule;
use crate::{activities as service, identity};
use axum::{
    extract::{DefaultBodyLimit, Path},
    routing::post,
};
use std::collections::HashMap;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/my/credits", get(credits))
        .route("/api/check-in", post(signin))
        .route("/api/{thread}/accept", post(accept))
        .route("/api/{thread}/vote", post(vote))
        .route("/api/{thread}/tip", post(tip))
        .route("/api/{thread}/enter", post(enter))
        .route("/api/{thread}/draw", post(draw))
        .route("/api/{thread}/entries/{uid}/exclude", post(exclude))
        .route("/api/{thread}/entries/{uid}/restore", post(restore))
        .route("/threads/{tid}/lottery", get(review))
        .route(
            "/threads/{tid}/lottery/codes",
            get(codes).post(replace_codes),
        )
        .route(
            "/threads/{tid}/lottery/codes/{prize}/{slot}",
            post(correct_code),
        )
        .route("/my/prizes", get(prizes))
        .route("/manage/credits", get(manage))
        .route("/manage/credits/rules", post(rule))
        .route("/manage/credits/adjust", post(adjust))
        .route("/manage/credits/tips", post(tip_settings))
        .layer(DefaultBodyLimit::max(16 * 1024))
}
#[allow(clippy::result_large_err)]
fn actor(state: &AppState) -> Result<identity::Actor, Response> {
    state
        .viewer
        .session
        .as_ref()
        .map(|s| s.member.actor())
        .ok_or_else(|| auth::redirect("/login?return_to=/my/credits"))
}
fn failure(state: &AppState, error: anyhow::Error) -> Response {
    if let Some(error) = error.downcast_ref::<service::ActivityFailure>() {
        error_page(state, StatusCode::UNPROCESSABLE_ENTITY, &error.0)
    } else {
        database_error(state, error)
    }
}
fn tid(segment: &str) -> Option<i64> {
    segment
        .strip_prefix("thread-")?
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
}
#[derive(Default, Deserialize)]
struct CreditQuery {
    #[serde(default)]
    before: i64,
    #[serde(default)]
    done: String,
}
struct LedgerRow {
    entry: service::LedgerItem,
    time: String,
    label: String,
    /// The row's reason: system rows by kind in this request's language (docs/architecture.md, "Languages"), the
    /// written reason of administrator adjustments as stored.
    reason: String,
}
#[derive(Template)]
#[template(path = "activities/credits.html")]
struct CreditsTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    available: i64,
    frozen: i64,
    rows: Vec<LedgerRow>,
    next_before: i64,
    checked_in: bool,
    signin_enabled: bool,
    signin: service::SigninStats,
    message: String,
    /// Growth points and level (the "My credits" page shows the member's own).
    level: crate::growth::Level,
    /// Earned today: credits and growth points listed by source (calendar day in the site time zone).
    credit_gains: Vec<super::leaderboard::GainRow>,
    growth_gains: Vec<super::leaderboard::GainRow>,
}
async fn credits(Page(state): Page, Query(query): Query<CreditQuery>) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    // "Today" always follows the site time zone: check-in, today's earnings and check-in statistics share one day
    // boundary.
    let offset = state.frame.settings.time.offset_minutes;
    let page = match service::credit_page(
        &state.db,
        actor.uid,
        query.before.max(0),
        identity::now(),
        offset,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => return failure(&state, e),
    };
    let rules = match service::rules(&state.db).await {
        Ok(r) => r,
        Err(e) => return failure(&state, e),
    };
    let signin = match service::signin_stats(&state.db, actor.uid, identity::now(), offset).await {
        Ok(stats) => stats,
        Err(e) => return failure(&state, e),
    };
    let level = match crate::growth::growth(&state.db, actor.uid).await {
        Ok(value) => state.frame.settings.growth.level(value),
        Err(e) => return failure(&state, e),
    };
    let credit_gains =
        match crate::growth::credits_today(&state.db, actor.uid, identity::now(), offset).await {
            Ok(parts) => parts
                .into_iter()
                .map(|part| super::leaderboard::GainRow {
                    label: super::leaderboard::credit_gain_label(&part.kind).into(),
                    total: part.total,
                })
                .collect(),
            Err(e) => return failure(&state, e),
        };
    let growth_gains =
        match crate::growth::today(&state.db, actor.uid, identity::now(), offset).await {
            Ok(parts) => parts
                .into_iter()
                .map(|part| super::leaderboard::GainRow {
                    label: crate::growth::kind_label(&part.kind).into(),
                    total: part.total,
                })
                .collect(),
            Err(e) => return failure(&state, e),
        };
    let rows = page
        .entries
        .into_iter()
        .map(|entry| {
            let time = crate::clock::datetime(entry.dateline, &crate::clock::current());
            let lang = crate::i18n::current();
            let activity = match entry.activity_kind {
                Some(kind @ 1..=3) => crate::content::category_label(kind),
                _ => lang.activity_ledger_activity(),
            };
            let label = match entry.kind.as_str() {
                "signin" => lang.activity_ledger_signin().into(),
                "thread" => lang.activity_ledger_thread_reward().into(),
                "reply" => lang.activity_ledger_reply_reward().into(),
                "admin_adjust" => lang.activity_ledger_adjust().into(),
                "activity_reserve" => lang.activity_ledger_reserve(activity),
                "activity_refund" => lang.activity_ledger_refund(activity),
                "activity_payout" => lang.activity_ledger_payout(activity),
                "activity_reward" => lang.activity_ledger_reward(activity),
                "tip_out" => lang.activity_ledger_tip_out().into(),
                "tip_in" => lang.activity_ledger_tip_in().into(),
                _ => lang.activity_ledger_other().into(),
            };
            let reason = service::system_reason(&entry.kind, lang)
                .map_or_else(|| entry.reason.clone(), str::to_owned);
            LedgerRow {
                entry,
                time,
                label,
                reason,
            }
        })
        .collect();
    let mut p = meta(
        &state,
        crate::i18n::current().common_my_credits(&state.frame.settings.site.credit_name),
        crate::i18n::current().activity_credits_description(&state.frame.settings.site.credit_name),
        "/my/credits",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(&state, &mut p).await {
        return error;
    }
    render(&CreditsTemplate {
        p,
        frame: crate::router::my::frame("credits"),
        available: page.available,
        frozen: page.frozen,
        rows,
        next_before: page.next_before,
        checked_in: page.checked_in,
        signin_enabled: rules.iter().any(|r| r.key == "signin" && r.enabled),
        signin,
        level,
        credit_gains,
        growth_gains,
        message: if query.done == "signin" {
            crate::i18n::current().activity_credits_signed_in().into()
        } else {
            String::new()
        },
    })
}
/// The check-in form carries only CSRF: it is idempotent per member and calendar day in the site time zone, so no
/// request key is needed.
#[derive(Deserialize)]
struct SigninForm {
    _csrf: String,
}
csrf_forms!(SigninForm);
async fn signin(Page(state): Page, CsrfForm(_form): CsrfForm<SigninForm>) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match service::check_in_live(&state.db, actor).await {
        Ok(_) => auth::redirect("/my/credits?done=signin"),
        Err(e) => failure(&state, e),
    }
}
#[derive(Deserialize)]
struct AcceptForm {
    _csrf: String,
    request_key: String,
    pid: i64,
}
csrf_forms!(AcceptForm);
async fn accept(
    Page(state): Page,
    Path(segment): Path<String>,
    CsrfForm(form): CsrfForm<AcceptForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(tid) = tid(&segment) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    match service::accept_live(&state.db, actor, tid, form.pid, &form.request_key).await {
        Ok(_) => auth::redirect(&format!("/thread-{tid}")),
        Err(e) => failure(&state, e),
    }
}
async fn vote(
    Page(state): Page,
    Path(segment): Path<String>,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(tid) = tid(&segment) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    let mut choices = Vec::new();
    for (k, v) in &form {
        if k == "choice" {
            if let Ok(n) = v.parse() {
                choices.push(n)
            }
        } else if v == "1"
            && let Some(n) = k.strip_prefix("choice_").and_then(|s| s.parse().ok())
        {
            choices.push(n)
        }
    }
    match service::vote_live(
        &state.db,
        actor,
        tid,
        choices,
        form.get("request_key").map(String::as_str).unwrap_or(""),
    )
    .await
    {
        Ok(_) => auth::redirect(&format!("/thread-{tid}")),
        Err(e) => failure(&state, e),
    }
}
struct RuleRow {
    rule: service::Rule,
    label: &'static str,
}
#[derive(Template)]
#[template(path = "activities/manage.html")]
struct ManageTemplate<'a> {
    p: PageMeta<'a>,
    rules: Vec<RuleRow>,
    tips: service::TipSettings,
    tip_amounts: String,
    max_tiers: usize,
    request_key: String,
    /// Request key for the growth point adjustment form, separate from the credit adjustment.
    growth_request_key: String,
    /// The "Growth points" section: point values, levels and leaderboard refresh interval
    /// (`settings.growth.*`, `settings.ranks.*`).
    growth: super::manage_settings::SectionView,
    message: String,
}
async fn manage(Page(state): Page, Query(query): Query<CreditQuery>) -> Response {
    if !state
        .viewer
        .session
        .as_ref()
        .is_some_and(|s| s.member.is_admin)
    {
        return error_page(
            &state,
            StatusCode::FORBIDDEN,
            &crate::i18n::current()
                .activity_manage_error_admin(&state.frame.settings.site.credit_name),
        );
    }
    let rules = match service::rules(&state.db).await {
        Ok(r) => r
            .into_iter()
            .map(|rule| {
                let t = crate::i18n::current();
                let label = match rule.key.as_str() {
                    "signin" => t.site_credit_rule_signin(),
                    "thread" => t.site_credit_rule_thread(),
                    _ => t.site_credit_rule_reply(),
                };
                RuleRow { rule, label }
            })
            .collect(),
        Err(e) => return failure(&state, e),
    };
    // The edit page reads current values from the database, not the in-process cache.
    let (tips, growth) = match crate::runtime::read(&state.db).await {
        Ok(value) => (
            value.tip.clone(),
            super::manage_settings::growth_section(&value),
        ),
        Err(e) => return auth::identity_error(&state, e),
    };
    render(&ManageTemplate {
        tip_amounts: service::amounts_text(&tips.amounts),
        tips,
        max_tiers: service::MAX_TIERS,
        p: meta(
            &state,
            crate::i18n::current().activity_manage_title(&state.frame.settings.site.credit_name),
            crate::i18n::current().activity_manage_description().into(),
            "/manage/credits",
            "",
            "",
        ),
        rules,
        request_key: identity::random_token(),
        growth_request_key: identity::random_token(),
        growth,
        message: match query.done.as_str() {
            "saved" => crate::i18n::current().activity_manage_saved().into(),
            "growth" => crate::i18n::current()
                .activity_manage_growth_adjusted()
                .into(),
            "growth-settings" => crate::i18n::current().activity_manage_growth_saved().into(),
            _ => String::new(),
        },
    })
}
#[derive(Deserialize)]
struct RuleForm {
    _csrf: String,
    key: String,
    #[serde(default)]
    enabled: bool,
    amount: i64,
    daily_limit: i64,
    version: i64,
}
csrf_forms!(RuleForm);
async fn rule(Page(state): Page, CsrfForm(form): CsrfForm<RuleForm>) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match service::update_rule(
        &state.db,
        &state.global,
        actor,
        &form.key,
        form.enabled,
        form.amount,
        form.daily_limit,
        form.version,
        identity::now(),
    )
    .await
    {
        Ok(_) => auth::redirect("/manage/credits?done=saved"),
        Err(e) => failure(&state, e),
    }
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
async fn adjust(Page(state): Page, CsrfForm(form): CsrfForm<AdjustForm>) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match service::adjust(
        &state.db,
        actor,
        form.uid,
        form.amount,
        &form.request_key,
        &form.reason,
        identity::now(),
    )
    .await
    {
        Ok(_) => auth::redirect("/manage/credits?done=saved"),
        Err(e) => failure(&state, e),
    }
}

#[derive(Deserialize)]
struct TipSettingsForm {
    _csrf: String,
    #[serde(default)]
    enabled: bool,
    amounts: String,
    daily_limit: i64,
}
csrf_forms!(TipSettingsForm);
async fn tip_settings(Page(state): Page, CsrfForm(form): CsrfForm<TipSettingsForm>) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match service::save_tip_settings(
        &state.db,
        &state.global,
        actor,
        form.enabled,
        &form.amounts,
        form.daily_limit,
    )
    .await
    {
        Ok(_) => auth::redirect("/manage/credits?done=saved"),
        Err(e) => failure(&state, e),
    }
}

#[derive(Deserialize)]
struct TipForm {
    _csrf: String,
    request_key: String,
    amount: i64,
}
csrf_forms!(TipForm);
/// Tipping is a money operation, with a separate limit of 10 per account per minute (in process). Native form
/// submission; success redirects 303 back to the first post.
async fn tip(
    Page(state): Page,
    Path(segment): Path<String>,
    CsrfForm(form): CsrfForm<TipForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(tid) = tid(&segment) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    match state
        .limit(&[Rule::new("credit-tip", actor.uid, 10, 60)])
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return error_page(
                &state,
                StatusCode::TOO_MANY_REQUESTS,
                crate::i18n::current().activity_error_rate(),
            );
        }
        Err(unavailable) => return unavailable,
    }
    match service::tip_live(&state.db, actor, tid, form.amount, &form.request_key).await {
        Ok(_) => auth::redirect(&format!("/thread-{tid}#floor-1")),
        Err(e) => failure(&state, e),
    }
}

pub(super) struct TipperView {
    pub name: String,
    pub href: String,
}
/// The tip section of the thread page: the button appears only when tipping is possible; the summary shows as soon as
/// anyone has tipped.
pub(super) struct TipView {
    pub tid: i64,
    pub can_tip: bool,
    pub tipped: bool,
    pub amounts: Vec<i64>,
    pub count: i64,
    pub total: i64,
    pub tippers: Vec<TipperView>,
    /// There are more tippers than names listed.
    pub more: bool,
    pub request_key: String,
}
/// `blocked_by_author` means the thread author has blocked the reader; when the reader has blocked the thread author,
/// `thread.blocked` is true.
pub(super) async fn tip_view(
    state: &AppState,
    thread: &crate::content::Thread,
    viewer: &crate::interaction::ViewerThreadState,
) -> anyhow::Result<TipView> {
    let tid = thread.id as i64;
    // Count, total and thread author state come from the thread details; whether the member has tipped comes from the
    // reader state row (docs/queries.md, "Thread page").
    let summary = service::tip_summary(&state.db, tid, (thread.tips, thread.tip_total)).await?;
    let member = state.viewer.session.as_ref().map(|s| &s.member);
    let mut can_tip = false;
    let mut tipped = false;
    let mut amounts = Vec::new();
    if let Some(member) = member
        && let Some(author) = thread.authorid
        && member.can_contribute()
        && member.uid != author
    {
        tipped = viewer.tipped;
        // Display uses the settings in the page frame (process cache); the tip transaction is still rechecked by the
        // write gate reading the database.
        let settings = state.frame.settings.tip.clone();
        if settings.enabled
            && !tipped
            && !thread.withdrawn
            && !thread.erased
            && !thread.blocked
            && !thread.author_deleted
            && !thread.author_unavailable
            && !viewer.blocked_by_author
            // Asked last, as it is the one query: during the cooling period of an account deletion or content
            // erasure the author's account is still active, but the tip transaction refuses every amount
            // (`privacy::funding_allowed`).
            && !crate::privacy::close_or_erase_pending(&state.db, author).await?
        {
            can_tip = true;
            amounts = settings.amounts;
        }
    }
    let more = summary.count > summary.recent.len() as i64;
    Ok(TipView {
        tid,
        can_tip,
        tipped,
        amounts,
        count: summary.count,
        total: summary.total,
        tippers: summary
            .recent
            .into_iter()
            .map(|tipper| TipperView {
                href: if tipper.deleted {
                    String::new()
                } else {
                    crate::urls::space(tipper.uid)
                },
                name: tipper.username,
            })
            .collect(),
        more,
        request_key: identity::random_token(),
    })
}

#[derive(Template)]
#[template(path = "activities/card.html")]
struct CardTemplate<'a> {
    card: service::ActivityCard,
    csrf: &'a str,
    request_key: String,
    can_contribute: bool,
    t: crate::i18n::Lang,
    /// The site's name for its points in this request's language (docs/architecture.md, "Languages").
    credit: &'a str,
}
impl CardTemplate<'_> {
    /// The link to the accepted bounty reply, for the `$link_html` slot of `activity-card-accepted`.
    fn accepted_link(&self) -> crate::i18n::Markup {
        crate::i18n::Markup::trusted(format!(
            "<a href=\"{}/posts/{}\">{}</a>",
            crate::site::base(),
            self.card.accepted_pid,
            crate::i18n::Markup::text(self.t.activity_card_accepted_link())
        ))
    }
}
/// The activity card on the thread page: regular threads (category 0) query nothing; activity threads read only the
/// activity row and that kind's detail table (docs/queries.md, "Thread page").
pub(super) async fn load_card(
    state: &AppState,
    thread: &crate::content::Thread,
) -> anyhow::Result<Option<service::ActivityCard>> {
    let member = state.viewer.session.as_ref().map(|s| &s.member);
    service::card_for_thread(
        &state.db,
        thread.id as i64,
        (thread.category, thread.withdrawn),
        member.map(|m| m.uid),
    )
    .await
}

pub(super) fn card_html(
    state: &AppState,
    card: Option<service::ActivityCard>,
) -> anyhow::Result<String> {
    let member = state.viewer.session.as_ref().map(|s| &s.member);
    let Some(card) = card else {
        return Ok(String::new());
    };
    Ok(CardTemplate {
        card,
        csrf: &state.viewer.csrf,
        request_key: identity::random_token(),
        can_contribute: member.is_some_and(|m| m.can_contribute()),
        t: crate::i18n::current(),
        credit: &state.frame.settings.site.credit_name,
    }
    .render()?)
}

/// One-click entry: `confirm_N=1` confirms each participation requirement, written the same way as poll multi-select.
/// Idempotent per member and thread, so no request key is needed.
async fn enter(
    Page(state): Page,
    Path(segment): Path<String>,
    CsrfForm(form): CsrfForm<HashMap<String, String>>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(tid) = tid(&segment) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    let confirmed: Vec<i64> = form
        .iter()
        .filter(|(_, value)| value.as_str() == "1")
        .filter_map(|(key, _)| key.strip_prefix("confirm_")?.parse().ok())
        .collect();
    match service::enter_live(&state.db, actor, tid, &confirmed).await {
        Ok(_) => auth::redirect(&format!("/thread-{tid}")),
        Err(e) => failure(&state, e),
    }
}
#[derive(Deserialize)]
struct PlainForm {
    _csrf: String,
}
csrf_forms!(PlainForm);
/// Early draw by the author: locks the list and generates the seed; the background job does the draw and payout within
/// seconds.
async fn draw(
    Page(state): Page,
    Path(segment): Path<String>,
    CsrfForm(_form): CsrfForm<PlainForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(tid) = tid(&segment) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    match service::draw_now_live(&state.db, actor, tid).await {
        Ok(_) => auth::redirect(&format!("/thread-{tid}")),
        Err(e) => failure(&state, e),
    }
}
#[derive(Deserialize)]
struct ExcludeForm {
    _csrf: String,
    reason: String,
}
csrf_forms!(ExcludeForm);
async fn exclude(
    Page(state): Page,
    Path((segment, uid)): Path<(String, i64)>,
    CsrfForm(form): CsrfForm<ExcludeForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(tid) = tid(&segment) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    match service::exclude_live(&state.db, actor, tid, uid, &form.reason).await {
        Ok(()) => auth::redirect(&format!("/threads/{tid}/lottery")),
        Err(e) => failure(&state, e),
    }
}
async fn restore(
    Page(state): Page,
    Path((segment, uid)): Path<(String, i64)>,
    CsrfForm(_form): CsrfForm<PlainForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(tid) = tid(&segment) else {
        return error_page(
            &state,
            StatusCode::NOT_FOUND,
            crate::i18n::current().topic_error_not_found(),
        );
    };
    match service::restore_live(&state.db, actor, tid, uid).await {
        Ok(()) => auth::redirect(&format!("/threads/{tid}/lottery")),
        Err(e) => failure(&state, e),
    }
}

#[derive(Default, Deserialize)]
struct AfterQuery {
    #[serde(default)]
    after: i64,
}
#[derive(Template)]
#[template(path = "activities/review.html")]
struct ReviewTemplate<'a> {
    p: PageMeta<'a>,
    page: service::ReviewPage,
    message: String,
}
/// The entry list: only the author and administrators can view it; only the author can remove entrants, before the
/// draw.
async fn review(
    Page(state): Page,
    Path(tid): Path<i64>,
    Query(query): Query<AfterQuery>,
) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect(&format!("/login?return_to=%2Fthreads%2F{tid}%2Flottery"));
    };
    let page = match service::review_page(
        &state.db,
        session.member.uid,
        session.member.is_admin,
        tid,
        query.after,
        identity::now(),
    )
    .await
    {
        Ok(Some(page)) => page,
        Ok(None) => {
            return error_page(
                &state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().activity_error_lottery(),
            );
        }
        Err(e) => return failure(&state, e),
    };
    render(&ReviewTemplate {
        p: meta(
            &state,
            crate::i18n::current().activity_review_title().into(),
            crate::i18n::current().activity_review_description().into(),
            &format!("/threads/{tid}/lottery"),
            "",
            "",
        ),
        page,
        message: String::new(),
    })
}

#[derive(Template)]
#[template(path = "activities/codes.html")]
struct CodesTemplate<'a> {
    p: PageMeta<'a>,
    board: service::CodeBoard,
    message: String,
}
async fn codes_page(state: &AppState, tid: i64, message: String, status: StatusCode) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect(&format!(
            "/login?return_to=%2Fthreads%2F{tid}%2Flottery%2Fcodes"
        ));
    };
    let board = match service::code_board(
        &state.db,
        &state.config.identity.two_factor,
        session.member.uid,
        session.member.is_admin,
        tid,
    )
    .await
    {
        Ok(Some(board)) => board,
        Ok(None) => {
            return error_page(
                state,
                StatusCode::NOT_FOUND,
                crate::i18n::current().activity_error_lottery(),
            );
        }
        Err(e) => return failure(state, e),
    };
    let mut response = render(&CodesTemplate {
        p: meta(
            state,
            crate::i18n::current().activity_codes_title().into(),
            crate::i18n::current().activity_codes_description().into(),
            &format!("/threads/{tid}/lottery/codes"),
            "",
            "",
        ),
        board,
        message,
    });
    *response.status_mut() = status;
    response
}
/// The author sees this lottery's redemption codes in plaintext; administrators only see how they were handed out. The
/// page is `private, no-store`.
async fn codes(Page(state): Page, Path(tid): Path<i64>) -> Response {
    codes_page(&state, tid, String::new(), StatusCode::OK).await
}
#[derive(Deserialize)]
struct ReplaceCodesForm {
    _csrf: String,
    prize: i64,
    codes: String,
}
csrf_forms!(ReplaceCodesForm);
async fn replace_codes(
    Page(state): Page,
    Path(tid): Path<i64>,
    CsrfForm(form): CsrfForm<ReplaceCodesForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let list: Vec<String> = form
        .codes
        .lines()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .collect();
    match service::replace_codes_live(
        &state.db,
        &state.config.identity.two_factor,
        actor,
        tid,
        form.prize,
        &list,
    )
    .await
    {
        Ok(()) => {
            codes_page(
                &state,
                tid,
                crate::i18n::current().activity_codes_replaced().into(),
                StatusCode::OK,
            )
            .await
        }
        Err(e) => code_failure(&state, tid, e).await,
    }
}
#[derive(Deserialize)]
struct CorrectCodeForm {
    _csrf: String,
    code: String,
    expected: i64,
}
csrf_forms!(CorrectCodeForm);
async fn correct_code(
    Page(state): Page,
    Path((tid, prize, slot)): Path<(i64, i64, i64)>,
    CsrfForm(form): CsrfForm<CorrectCodeForm>,
) -> Response {
    let actor = match actor(&state) {
        Ok(a) => a,
        Err(r) => return r,
    };
    match service::correct_code_live(
        &state.db,
        &state.config.identity.two_factor,
        actor,
        tid,
        prize,
        slot,
        &form.code,
        form.expected,
    )
    .await
    {
        Ok(()) => {
            codes_page(
                &state,
                tid,
                crate::i18n::current().activity_codes_done().into(),
                StatusCode::OK,
            )
            .await
        }
        Err(e) => code_failure(&state, tid, e).await,
    }
}
/// When the redemption-code form fails, return to the redemption-code page and show the reason; the codes entered are
/// not refilled, so they do not linger on the page.
async fn code_failure(state: &AppState, tid: i64, error: anyhow::Error) -> Response {
    match error.downcast_ref::<service::ActivityFailure>() {
        Some(failure) => {
            codes_page(
                state,
                tid,
                failure.0.clone(),
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        None => database_error(state, error),
    }
}

#[derive(Default, Deserialize)]
struct BeforeQuery {
    #[serde(default)]
    before: i64,
}
struct PrizeRow {
    item: service::PrizeItem,
    assigned: String,
    revised: String,
}
#[derive(Template)]
#[template(path = "activities/prizes.html")]
struct PrizesTemplate<'a> {
    p: PageMeta<'a>,
    frame: crate::router::my::Frame,
    items: Vec<PrizeRow>,
    next_before: Option<i64>,
}
/// "My prizes": the member's own redemption codes (shown decrypted) and other prizes. The page is
/// `private, no-store`.
async fn prizes(Page(state): Page, Query(query): Query<BeforeQuery>) -> Response {
    let Some(session) = state.viewer.session.as_ref() else {
        return auth::redirect("/login?return_to=%2Fmy%2Fprizes");
    };
    let (items, next_before) = match service::my_prizes(
        &state.db,
        &state.config.identity.two_factor,
        session.member.uid,
        query.before,
    )
    .await
    {
        Ok(page) => page,
        Err(e) => return failure(&state, e),
    };
    let clock = crate::clock::current();
    let items = items
        .into_iter()
        .map(|item| PrizeRow {
            assigned: crate::clock::datetime(item.assigned_at, &clock),
            revised: crate::clock::datetime(item.revised_at, &clock),
            item,
        })
        .collect();
    let mut p = meta(
        &state,
        crate::i18n::current().activity_prizes_title().into(),
        crate::i18n::current().activity_prizes_description().into(),
        "/my/prizes",
        "",
        "",
    );
    if let Some(error) = crate::router::my::fill_shell(&state, &mut p).await {
        return error;
    }
    render(&PrizesTemplate {
        p,
        frame: crate::router::my::frame("prizes"),
        items,
        next_before,
    })
}
