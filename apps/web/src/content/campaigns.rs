//! Bulk-provisions the five kinds of activity threads for test environments: bounty, red packet, lottery, poll and
//! news. Calls the bookkeeping and activity functions already exported by the `activities` module directly (it
//! does not re-implement validation or transaction logic); it only replaces the web session layer (CSRF, cookies)
//! with batch calls a script can drive.
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    activities,
    content::writes::{ContentWriter, NewThread, WriteConfig},
    db::Database,
    identity::Actor,
};

async fn actor_of(db: &Database, uid: i64) -> Result<Actor> {
    let credentials_version: i64 = crate::db::query_scalar(
        "SELECT credentials_version FROM gonof_common_member WHERE uid = ?",
    )
    .bind(uid)
    .fetch_one(db.executor())
    .await?;
    Ok(Actor {
        uid,
        credentials_version,
        api_key: None,
    })
}

fn now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_secs(),
    )
    .expect("current time fits in i64")
}

#[derive(Deserialize)]
struct CreditGrant {
    uid: i64,
    amount: i64,
}

#[derive(Deserialize)]
struct CampaignSpec {
    label: String,
    owner_uid: i64,
    forum_slug: String,
    title: String,
    body: String,
    kind: i64,
    #[serde(default)]
    budget: i64,
    #[serde(default)]
    shares: i64,
    #[serde(default = "default_distribution")]
    distribution: String,
    #[serde(default)]
    deadline_offset_seconds: i64,
    #[serde(default = "default_max_choices")]
    max_choices: i64,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    source_url: String,
    #[serde(default)]
    source_name: String,
    #[serde(default)]
    source_author: String,
}
fn default_distribution() -> String {
    "equal".into()
}
fn default_max_choices() -> i64 {
    1
}

#[derive(Serialize)]
pub struct CreatedCampaign {
    pub label: String,
    pub tid: i64,
    pub pid: i64,
}

#[derive(Deserialize)]
struct VoteSpec {
    tid: i64,
    uid: i64,
    choices: Vec<i64>,
}

#[derive(Deserialize)]
struct AcceptSpec {
    tid: i64,
    pid: i64,
    owner_uid: i64,
}

#[derive(Deserialize, Default)]
struct CampaignPlan {
    #[serde(default)]
    credits: Vec<CreditGrant>,
    #[serde(default)]
    campaigns: Vec<CampaignSpec>,
    #[serde(default)]
    votes: Vec<VoteSpec>,
    #[serde(default)]
    accepts: Vec<AcceptSpec>,
}

pub struct PlanSummary {
    pub credits_granted: usize,
    pub campaigns_published: Vec<CreatedCampaign>,
    pub votes_cast: usize,
    pub bounties_accepted: usize,
}

async fn resolve_fid(db: &Database, slug: &str) -> Result<i64> {
    crate::db::query_scalar("SELECT fid FROM gonof_forum_forum WHERE slug = ?")
        .bind(slug)
        .fetch_optional(db.executor())
        .await?
        .with_context(|| format!("forum not found: {slug}"))
}

/// Executes a JSON plan in bulk: grant initial credits, create activity threads, vote, accept bounty replies.
/// `founder_uid` must be an account with adminid=1; it is used to grant the initial credits (`adjust` requires an
/// administrator).
pub async fn run_plan(db: &Database, founder_uid: i64, path: &Path) -> Result<PlanSummary> {
    // A command-line tool: what it writes for members to read (ledger reasons, prize names) is in the site language
    // (docs/architecture.md, "Languages").
    let language = crate::runtime::read(db)
        .await
        .map_err(anyhow::Error::new)?
        .site
        .lang();
    let site = crate::site::Site {
        language,
        ..crate::site::Site::default()
    };
    crate::site::scope(site, run_plan_in_site(db, founder_uid, path)).await
}

async fn run_plan_in_site(db: &Database, founder_uid: i64, path: &Path) -> Result<PlanSummary> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the activity plan: {}", path.display()))?;
    let plan: CampaignPlan = serde_json::from_str(&raw)
        .with_context(|| format!("cannot parse the activity plan: {}", path.display()))?;

    let founder = actor_of(db, founder_uid).await?;
    let timestamp = now();

    let mut credits_granted = 0usize;
    for grant in &plan.credits {
        let key = crate::identity::token_hash(&format!("seed-grant:{}", grant.uid));
        activities::adjust(
            db,
            founder,
            grant.uid,
            grant.amount,
            &key,
            crate::i18n::current().activity_reason_seed(),
            timestamp,
        )
        .await
        .with_context(|| format!("granting points failed: uid={}", grant.uid))?;
        credits_granted += 1;
    }

    // The category and the initiator of an activity row must be exactly the category and the author of the thread
    // (checked by `activities::publish`, not by a trigger), so the post and its activity
    // must be created in one go with the same ContentWriter as the web pages and the member API; a plain post
    // cannot be created first with the activity attached afterwards.
    let writer = ContentWriter::new(db.clone(), WriteConfig::default()).with_source("seed-cli");
    let mut campaigns_published = Vec::with_capacity(plan.campaigns.len());
    for spec in &plan.campaigns {
        let fid = resolve_fid(db, &spec.forum_slug).await?;
        let actor = actor_of(db, spec.owner_uid).await?;
        let request_key = crate::identity::token_hash(&format!("seed-campaign:{}", spec.label));
        // The deadline counts from this run, but it is part of the request: when a run stopped partway and the plan is
        // run again, an activity that was already created keeps its own deadline, so its request replays instead of
        // conflicting with itself.
        let deadline = match created_deadline(db, actor.uid, &request_key).await? {
            Some(deadline) => deadline,
            None => timestamp + spec.deadline_offset_seconds,
        };
        let activity_spec = activities::ActivitySpec {
            kind: spec.kind,
            budget: spec.budget,
            shares: spec.shares,
            distribution: spec.distribution.clone(),
            deadline,
            max_choices: spec.max_choices,
            options: spec.options.clone(),
            source_url: spec.source_url.clone(),
            source_name: spec.source_name.clone(),
            source_author: spec.source_author.clone(),
            ..Default::default()
        };
        let published = writer
            .publish_actor(
                actor,
                NewThread {
                    visibility: Default::default(),
                    activity: activity_spec,
                    fid,
                    title: spec.title.clone(),
                    body: spec.body.clone(),
                    attachment_ids: Vec::new(),
                    request_key,
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
            .with_context(|| format!("creating the activity failed: {}", spec.label))?;
        campaigns_published.push(CreatedCampaign {
            label: spec.label.clone(),
            tid: published.tid,
            pid: published.pid,
        });
    }

    let mut votes_cast = 0usize;
    for vote in &plan.votes {
        let actor = actor_of(db, vote.uid).await?;
        let key = crate::identity::token_hash(&format!("seed-vote:{}:{}", vote.tid, vote.uid));
        activities::vote(db, actor, vote.tid, vote.choices.clone(), &key, timestamp)
            .await
            .with_context(|| format!("voting failed: tid={} uid={}", vote.tid, vote.uid))?;
        votes_cast += 1;
    }

    let mut bounties_accepted = 0usize;
    for accept in &plan.accepts {
        let actor = actor_of(db, accept.owner_uid).await?;
        let key =
            crate::identity::token_hash(&format!("seed-accept:{}:{}", accept.tid, accept.pid));
        activities::accept(db, actor, accept.tid, accept.pid, &key, timestamp)
            .await
            .with_context(|| {
                format!(
                    "accepting the bounty reply failed: tid={} pid={}",
                    accept.tid, accept.pid
                )
            })?;
        bounties_accepted += 1;
    }

    Ok(PlanSummary {
        credits_granted,
        campaigns_published,
        votes_cast,
        bounties_accepted,
    })
}

/// The deadline of the activity a member already created with this request key, while the key is remembered.
async fn created_deadline(db: &Database, uid: i64, request_key: &str) -> Result<Option<i64>> {
    Ok(crate::db::query_scalar(
        "SELECT a.deadline FROM gonof_forum_post_request r JOIN gonof_forum_post p ON p.pid=r.pid JOIN gonof_forum_activity a ON a.tid=p.tid WHERE r.uid=? AND r.request_key=?",
    )
    .bind(uid)
    .bind(request_key)
    .fetch_optional(db.executor())
    .await?)
}

/// Settles due lotteries, bounties and red packets (lottery draws and so on); `limit` is the maximum number of
/// activities handled per run.
pub async fn settle_due(db: &Database, limit: i64) -> Result<usize> {
    activities::process_due(db, now(), limit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan that stopped partway can be run again: the activity it already created keeps its deadline, so its
    /// request replays with the same thread instead of conflicting, and the rest of the plan goes on.
    #[tokio::test]
    async fn a_plan_run_again_after_a_partial_run_replays_what_it_created() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        crate::content::seed_forums(db).await.unwrap();
        crate::test_support::raise_posting_quotas(db).await.unwrap();
        for (uid, adminid) in [(9831, 1), (9832, 0)] {
            crate::db::query("INSERT INTO gonof_common_member(uid,username,password,email,emailstatus,adminid,regdate) VALUES(?,?,'synthetic-hash',?,1,?,1)")
                .bind(uid)
                .bind(format!("plan-{uid}"))
                .bind(format!("plan-{uid}@example.test"))
                .bind(adminid)
                .execute(db.executor())
                .await
                .unwrap();
        }
        let slug: String =
            crate::db::query_scalar("SELECT slug FROM gonof_forum_forum ORDER BY fid LIMIT 1")
                .fetch_one(db.executor())
                .await
                .unwrap();
        let plan = |second_forum: &str| {
            serde_json::json!({"campaigns": [
                {"label": "first-poll", "owner_uid": 9832, "forum_slug": slug, "title": "First poll",
                 "body": "Pick one", "kind": 4, "deadline_offset_seconds": 86_400, "options": ["Yes", "No"]},
                {"label": "second-poll", "owner_uid": 9832, "forum_slug": second_forum, "title": "Second poll",
                 "body": "Pick again", "kind": 4, "deadline_offset_seconds": 86_400, "options": ["Up", "Down"]},
            ]})
            .to_string()
        };
        let path = services.root.join("plan.json");
        std::fs::create_dir_all(&services.root).unwrap();
        std::fs::write(&path, plan("no-such-forum")).unwrap();
        assert!(run_plan(db, 9831, &path).await.is_err());
        let first: i64 = crate::db::query_scalar(
            "SELECT tid FROM gonof_forum_thread WHERE subject='First poll'",
        )
        .fetch_one(db.executor())
        .await
        .unwrap();
        // The next run starts later, so its own deadlines differ from those of the first run.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        std::fs::write(&path, plan(&slug)).unwrap();
        let summary = run_plan(db, 9831, &path).await.unwrap();
        assert_eq!(summary.campaigns_published.len(), 2);
        assert_eq!(summary.campaigns_published[0].tid, first);
        assert_eq!(
            crate::db::query_scalar::<i64>(
                "SELECT COUNT(*) FROM gonof_forum_thread WHERE subject='First poll'"
            )
            .fetch_one(db.executor())
            .await
            .unwrap(),
            1
        );
        services.cleanup().await.unwrap();
    }
}
