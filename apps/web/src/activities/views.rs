use super::*;

#[derive(Clone, Debug, Serialize)]
pub struct PollOption {
    pub number: i64,
    pub label: String,
    pub votes: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Winner {
    pub uid: i64,
    pub username: String,
    /// Credits received for a credit prize; 0 for redemption codes and other prizes.
    pub amount: i64,
    pub prize_no: i64,
    pub prize: String,
}
/// One entry requirement of a one-click lottery; `number` starts at 1 and the entry form checks the items in that
/// order.
#[derive(Clone, Debug, Serialize)]
pub struct Requirement {
    pub number: i64,
    pub label: String,
}
/// One prize on the lottery card. Excludes redemption codes.
#[derive(Clone, Debug, Serialize)]
pub struct CardPrize {
    pub number: i64,
    pub name: String,
    pub winners: i64,
    pub kind: String,
    pub amount: i64,
    pub note: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct ActivityCard {
    pub tid: i64,
    pub kind: i64,
    pub label: String,
    pub owner_uid: i64,
    pub budget: i64,
    pub remaining: i64,
    pub shares: i64,
    pub remaining_shares: i64,
    pub distribution: String,
    pub deadline: i64,
    pub deadline_label: String,
    pub state: String,
    pub state_label: String,
    pub accepted_pid: i64,
    pub max_choices: i64,
    pub voters: i64,
    pub entries: i64,
    pub options: Vec<PollOption>,
    pub has_voted: bool,
    pub can_vote: bool,
    pub can_accept: bool,
    pub my_entry: bool,
    pub my_amount: i64,
    pub winners: Vec<Winner>,
    pub source_url: String,
    pub source_name: String,
    pub source_author: String,
    /// Lottery entry mode: `reply` or `click`; empty for other kinds.
    pub entry_mode: String,
    pub requirements: Vec<Requirement>,
    pub review: bool,
    pub early_draw: bool,
    /// The original deadline when the draw was held early; 0 = not drawn early.
    pub planned_deadline: i64,
    pub planned_deadline_label: String,
    /// Number of entrants removed by the author (public).
    pub excluded: i64,
    /// Reason the viewer was removed; empty when not removed. Shown only to the viewer.
    pub my_excluded: String,
    pub prizes: Vec<CardPrize>,
    /// Name and kind of the prize the viewer won; empty when not won. Redemption codes are viewed separately under
    /// "My prizes".
    pub my_prize: String,
    pub my_prize_kind: String,
    pub can_enter: bool,
    pub can_draw: bool,
    /// The viewer is the lottery author: the card links to the review page and the redemption code page.
    pub can_manage: bool,
    /// Has redemption codes or other prizes (the author has a redemption code page).
    pub has_awards: bool,
}

/// This contains the viewer's ballot/claim state and must never be cached as a
/// shared HTML fragment. Public totals reveal no voter identities or ballots.
///
/// For places that have not read the thread first, such as the member API: reads the thread row and first post and
/// re-checks readability (the same check as participation and settlement; anonymous visitors additionally require the
/// thread itself and the inherited forum to be public), then reads the card by thread kind ([`card_for_thread`]).
pub async fn card(db: &Database, tid: i64, viewer: Option<i64>) -> Result<Option<ActivityCard>> {
    let Some(r) = crate::db::query(&format!(
        "SELECT t.category,t.deleted_at,t.displayorder,t.readperm,t.visibility,t.publication_state,{},p.invisible AS first_invisible,p.deleted_at AS first_deleted FROM gonof_forum_thread t JOIN gonof_forum_post p ON p.first_tid=t.tid {} WHERE t.tid=?",
        crate::content::copies::FORUM_SCOPE_COLUMNS,
        crate::content::copies::FORUM_SCOPE_JOINS
    ))
    .bind(tid)
    .fetch_optional(db.executor())
    .await?
    else {
        return Ok(None);
    };
    let public = crate::content::copies::forum_scope(&r)?.visibility
        == crate::content::copies::ForumVisibility::Public
        && crate::db::text(&r, "visibility")? == "public";
    if !super::member_visible(&r)? || (viewer.is_none() && !public) {
        return Ok(None);
    }
    let withdrawn =
        r.try_get::<i64, _>("deleted_at")? > 0 || r.try_get::<i64, _>("first_deleted")? > 0;
    card_for_thread(db, tid, (r.try_get("category")?, withdrawn), viewer).await
}

/// For the thread page (docs/queries.md, "Thread page"): the thread has already been read within the range this visitor
/// can read (`kind` is the thread kind; `withdrawn` means the thread or first post was
/// withdrawn), so the thread and first post are not joined again to re-check readability. Normal threads of kind 0 do
/// not query the database; activity threads join only the activity row and that kind's detail table.
pub async fn card_for_thread(
    db: &Database,
    tid: i64,
    (kind, withdrawn): (i64, bool),
    viewer: Option<i64>,
) -> Result<Option<ActivityCard>> {
    let (detail, columns) = match kind {
        1 => ("gonof_forum_reward", "d.accepted_pid"),
        2 => (
            "gonof_forum_redpacket",
            "d.shares AS redpacket_shares,d.remaining_shares,d.distribution",
        ),
        3 => (
            "gonof_forum_lottery",
            "d.shares AS lottery_shares,d.entry_mode,d.requirements,d.review,d.early_draw,d.planned_deadline",
        ),
        4 => ("gonof_forum_poll", "d.max_choices,d.voters"),
        5 => (
            "gonof_forum_news",
            "d.source_url,d.source_name,d.source_author",
        ),
        _ => return Ok(None),
    };
    let sql = format!(
        "SELECT a.kind,a.owner_uid,a.budget,a.remaining,a.deadline,a.state,{columns} FROM gonof_forum_activity a LEFT JOIN {detail} d ON d.tid=a.tid WHERE a.tid=?"
    );
    let Some(r) = crate::db::query(&sql)
        .bind(tid)
        .fetch_optional(db.executor())
        .await?
    else {
        return Ok(None);
    };
    let kind: i64 = r.try_get("kind")?;
    if kind == 5 && withdrawn {
        return Ok(None);
    }
    let state: String = crate::db::text(&r, "state")?;
    let deadline: i64 = r.try_get("deadline")?;
    let owner: i64 = r.try_get("owner_uid")?;
    let open = state == "open" && deadline > crate::identity::now() && !withdrawn;
    let options = if kind == 4 {
        crate::db::query(
        "SELECT option_no,label,votes FROM gonof_forum_poll_option WHERE tid=? ORDER BY option_no",
    )
    .bind(tid)
    .fetch_all(db.executor())
    .await?
    .into_iter()
    .map(|r| {
        Ok(PollOption {
            number: r.try_get("option_no")?,
            label: crate::db::text(&r, "label")?,
            votes: r.try_get("votes")?,
        })
    })
    .collect::<Result<Vec<_>>>()?
    } else {
        vec![]
    };
    let has_voted = if let Some(uid) = viewer.filter(|_| kind == 4) {
        crate::db::query("SELECT uid FROM gonof_forum_poll_vote WHERE tid=? AND uid=?")
            .bind(tid)
            .bind(uid)
            .fetch_optional(db.executor())
            .await?
            .is_some()
    } else {
        false
    };
    let entry_mode: String = detail_column::<String>(&r, "entry_mode")?.unwrap_or_default();
    let entry_table = if kind == 2 {
        "gonof_forum_redpacket_claim"
    } else {
        super::lottery::entry_table(&entry_mode)
    };
    let mine = if let Some(uid) = viewer.filter(|_| matches!(kind, 2 | 3)) {
        crate::db::query(&format!(
            "SELECT amount FROM {entry_table} WHERE tid=? AND uid=?"
        ))
        .bind(tid)
        .bind(uid)
        .fetch_optional(db.executor())
        .await?
    } else {
        None
    };
    let my_entry = mine.is_some();
    let my_amount = mine.map(|r| r.try_get("amount")).transpose()?.unwrap_or(0);
    let entries = if matches!(kind, 2 | 3) {
        crate::db::query_scalar(&format!("SELECT COUNT(*) FROM {entry_table} WHERE tid=?"))
            .bind(tid)
            .fetch_one(db.executor())
            .await?
    } else {
        0
    };
    let lottery = if kind == 3 {
        Some(lottery_view(db, tid, viewer, &state).await?)
    } else {
        None
    };
    let state_label = match state.as_str() {
        "cancelled" => crate::i18n::current().activity_state_cancelled(),
        "expired" => crate::i18n::current().activity_state_expired(),
        "settled" => crate::i18n::current().activity_state_settled(),
        "drawing" => crate::i18n::current().activity_state_drawing(),
        _ if kind == 5 => crate::i18n::current().activity_state_news(),
        _ if !open => crate::i18n::current().activity_state_closed(),
        _ => crate::i18n::current().activity_state_open(),
    }
    .into();
    let clock = crate::clock::current();
    let deadline_label = crate::clock::datetime(deadline, &clock);
    let planned_deadline = detail_column::<i64>(&r, "planned_deadline")?.unwrap_or(0);
    let early_draw = detail_column::<i64>(&r, "early_draw")? == Some(1);
    let LotteryView {
        prizes,
        winners,
        excluded,
        my_excluded,
        my_prize,
        my_prize_kind,
    } = lottery.unwrap_or_default();
    let requirements = super::lottery::requirements(detail_column::<String>(&r, "requirements")?)?
        .into_iter()
        .enumerate()
        .map(|(index, label)| Requirement {
            number: index as i64 + 1,
            label,
        })
        .collect();
    let lottery_open = kind == 3 && open;
    Ok(Some(ActivityCard {
        tid,
        kind,
        label: crate::content::category_label(kind).into(),
        owner_uid: owner,
        budget: r.try_get("budget")?,
        remaining: r.try_get("remaining")?,
        shares: detail_column::<i64>(&r, "redpacket_shares")?
            .or(detail_column::<i64>(&r, "lottery_shares")?)
            .unwrap_or(0),
        remaining_shares: detail_column::<i64>(&r, "remaining_shares")?.unwrap_or(0),
        distribution: detail_column::<String>(&r, "distribution")?
            .unwrap_or_else(|| "equal".into()),
        deadline,
        deadline_label,
        state,
        state_label,
        accepted_pid: detail_column::<i64>(&r, "accepted_pid")?.unwrap_or(0),
        max_choices: detail_column::<i64>(&r, "max_choices")?.unwrap_or(1),
        voters: detail_column::<i64>(&r, "voters")?.unwrap_or(0),
        entries,
        options,
        has_voted,
        can_vote: kind == 4 && open && viewer.is_some() && !has_voted,
        can_accept: kind == 1 && open && viewer == Some(owner),
        my_entry,
        my_amount,
        winners,
        can_enter: lottery_open
            && entry_mode == "click"
            && viewer.is_some_and(|uid| uid != owner)
            && !my_entry,
        can_draw: lottery_open && early_draw && viewer == Some(owner),
        can_manage: kind == 3 && viewer == Some(owner),
        has_awards: prizes.iter().any(|prize| prize.kind != "credit"),
        entry_mode,
        requirements,
        review: detail_column::<i64>(&r, "review")? == Some(1),
        early_draw,
        planned_deadline,
        planned_deadline_label: if planned_deadline > 0 {
            crate::clock::datetime(planned_deadline, &clock)
        } else {
            String::new()
        },
        excluded,
        my_excluded,
        prizes,
        my_prize,
        my_prize_kind,
        source_url: detail_column::<String>(&r, "source_url")?.unwrap_or_default(),
        source_name: detail_column::<String>(&r, "source_name")?.unwrap_or_default(),
        source_author: detail_column::<String>(&r, "source_author")?.unwrap_or_default(),
    }))
}

/// Detail columns of this activity kind: the card joins only this kind's detail table; columns of other kinds are not
/// in the result and are treated as absent.
fn detail_column<T>(row: &crate::db::DbRow, name: &str) -> Result<Option<T>>
where
    T: for<'r> crate::db::Decodable<'r>,
{
    match row.try_get::<Option<T>, _>(name) {
        Ok(value) => Ok(value),
        Err(sqlx::Error::ColumnNotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The extra parts of a lottery card: prizes, removed count, the viewer's status, and the winner list grouped by prize.
#[derive(Default)]
struct LotteryView {
    prizes: Vec<CardPrize>,
    winners: Vec<Winner>,
    excluded: i64,
    my_excluded: String,
    my_prize: String,
    my_prize_kind: String,
}

async fn lottery_view(
    db: &Database,
    tid: i64,
    viewer: Option<i64>,
    state: &str,
) -> Result<LotteryView> {
    let prizes = crate::db::query("SELECT prize_no,name,winners,kind,amount,note FROM gonof_forum_lottery_prize WHERE tid=? ORDER BY prize_no")
        .bind(tid)
        .fetch_all(db.executor())
        .await?
        .iter()
        .map(|r| {
            Ok(CardPrize {
                number: r.try_get("prize_no")?,
                name: crate::db::text(r, "name")?,
                winners: r.try_get("winners")?,
                kind: crate::db::text(r, "kind")?,
                amount: r.try_get("amount")?,
                note: crate::db::text(r, "note")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let excluded: i64 =
        crate::db::query_scalar("SELECT COUNT(*) FROM gonof_forum_lottery_exclusion WHERE tid=?")
            .bind(tid)
            .fetch_one(db.executor())
            .await?;
    let my_excluded = match viewer {
        Some(uid) => crate::db::query_scalar(
            "SELECT reason FROM gonof_forum_lottery_exclusion WHERE tid=? AND uid=?",
        )
        .bind(tid)
        .bind(uid)
        .fetch_optional(db.executor())
        .await?
        .unwrap_or_default(),
        None => String::new(),
    };
    let mut view = LotteryView {
        excluded,
        my_excluded,
        ..Default::default()
    };
    if ["drawing", "settled"].contains(&state) {
        let decided: Option<String> =
            crate::db::query_scalar("SELECT winners FROM gonof_forum_activity_draw WHERE tid=?")
                .bind(tid)
                .fetch_optional(db.executor())
                .await?;
        let decided: Vec<i64> = match decided {
            Some(raw) => serde_json::from_str(&raw)?,
            None => vec![],
        };
        let plan: Vec<super::worker::Prize> = prizes
            .iter()
            .map(|p| super::worker::Prize {
                number: p.number,
                kind: p.kind.clone(),
                winners: p.winners,
                amount: p.amount,
            })
            .collect();
        let allocations = super::worker::allocate(&decided, &plan);
        let name = |number: i64| {
            prizes
                .iter()
                .find(|p| p.number == number)
                .map(|p| (p.name.clone(), p.kind.clone()))
                .unwrap_or_default()
        };
        if let Some(uid) = viewer
            && let Some(mine) = allocations.iter().find(|a| a.uid == uid)
        {
            (view.my_prize, view.my_prize_kind) = name(mine.prize);
        }
        // The winner list is public only after settlement, as before.
        if state == "settled" && !allocations.is_empty() {
            let mut names = std::collections::HashMap::new();
            for chunk in allocations.chunks(500) {
                let sql = format!(
                    "SELECT uid,username FROM gonof_common_member WHERE uid IN ({})",
                    vec!["?"; chunk.len()].join(",")
                );
                let mut query = crate::db::query(&sql);
                for allocation in chunk {
                    query = query.bind(allocation.uid);
                }
                for row in query.fetch_all(db.executor()).await? {
                    names.insert(
                        row.try_get::<i64, _>("uid")?,
                        crate::db::text(&row, "username")?,
                    );
                }
            }
            view.winners = allocations
                .iter()
                .map(|a| Winner {
                    uid: a.uid,
                    username: names.get(&a.uid).cloned().unwrap_or_default(),
                    amount: a.amount,
                    prize_no: a.prize,
                    prize: name(a.prize).0,
                })
                .collect();
        }
    }
    view.prizes = prizes;
    Ok(view)
}
