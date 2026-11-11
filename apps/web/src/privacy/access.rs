use super::*;
use crate::db::Tx;
const ACCESS_SECONDS: i64 = 3600;
#[derive(Clone)]
pub enum Access {
    Session(Actor),
    Token(String),
}
fn digest(raw: &str, ticket: bool) -> String {
    if ticket {
        identity::token_hash(raw)
    } else {
        identity::token_hash(&format!("privacy.cookie:{raw}"))
    }
}
/// Whether the email link may stand in for signing in, from the member row's `password` and `emailstatus`. The link is
/// the whole check: data requests made through it ask for no password. A member with a password gets it whether or
/// not the email is verified. A member without one (signed up with a text message code, Google or GitHub) cannot sign
/// in to set one while frozen, so the link is their way in, but only at a verified email: an address never confirmed
/// may be mistyped or someone else's, and its owner would then control this account's data requests. Checked when the
/// link is issued and on every use, like the freeze (see `docs/roles.md`, "Freezes").
fn email_link_allowed(member: &DbRow) -> Result<bool> {
    Ok(member.try_get::<Option<String>, _>("password")?.is_some()
        || member.try_get::<i64, _>("emailstatus")? == 1)
}
pub(super) async fn owner(
    db: &Database,
    tx: &mut Tx<'_>,
    access: &Access,
    now: i64,
    ticket: bool,
) -> Result<i64> {
    let uid = match access {
        Access::Session(a) => a.uid,
        Access::Token(raw) => {
            if !valid_key(raw) {
                return Err(IdentityError::Unauthorized);
            }
            db::query_scalar("SELECT uid FROM gonof_common_member_token WHERE token_hash=? AND purpose='privacy_access'").bind(digest(raw,ticket)).fetch_optional(db.observe(&mut **tx)).await?.ok_or(IdentityError::Unauthorized)?
        }
    };
    let m=db::query("SELECT email,emailstatus,password,account_state,credentials_version,`freeze` FROM gonof_common_member WHERE uid=? FOR UPDATE").bind(uid).fetch_optional(db.observe(&mut **tx)).await?.ok_or(IdentityError::Unauthorized)?;
    if db::text(&m, "account_state")? != "active" {
        return Err(IdentityError::Unauthorized);
    }
    match access {
        Access::Session(a) => {
            if m.try_get::<i64, _>("freeze")? != 0
                || m.try_get::<i64, _>("credentials_version")? != a.credentials_version
            {
                return Err(IdentityError::Unauthorized);
            }
        }
        Access::Token(raw) => {
            let token=db::query("SELECT email,dateline,expires_at FROM gonof_common_member_token WHERE uid=? AND token_hash=? AND purpose='privacy_access' AND consumed_at IS NULL AND revoked_at IS NULL FOR UPDATE").bind(uid).bind(digest(raw,ticket)).fetch_optional(db.observe(&mut **tx)).await?.ok_or(IdentityError::Unauthorized)?;
            // Email credentials only act on behalf of frozen members; after unfreezing it is back to the normal entry
            // of login plus current password.
            if m.try_get::<i64, _>("freeze")? == 0
                || !email_link_allowed(&m)?
                || m.try_get::<Option<String>, _>("email")? != Some(db::text(&token, "email")?)
                || token.try_get::<i64, _>("dateline")? > now
                || token.try_get::<i64, _>("expires_at")? <= now
            {
                return Err(IdentityError::Unauthorized);
            }
        }
    }
    Ok(uid)
}
pub async fn request_access(db: &Database, email: &str, now: i64) -> Result<()> {
    let Ok(email) = identity::normalize_email(email) else {
        return Ok(());
    };
    let mut tx = db.begin_write().await?;
    let m=db::query("SELECT uid,username,account_state,password,emailstatus,`freeze` FROM gonof_common_member WHERE email=? FOR UPDATE").bind(&email).fetch_optional(db.observe(&mut *tx)).await?;
    let Some(m) = m else {
        return Ok(());
    };
    // Tickets are only issued to frozen members: members who are not frozen sign in and submit with their current
    // password, and cannot bypass the password.
    if db::text(&m, "account_state")? != "active"
        || m.try_get::<i64, _>("freeze")? == 0
        || !email_link_allowed(&m)?
    {
        return Ok(());
    }
    let uid: i64 = m.try_get("uid")?;
    let recent:i64=db::query_scalar("SELECT COUNT(*) FROM gonof_common_member_token WHERE uid=? AND purpose='privacy_access' AND dateline>?").bind(uid).bind(now-60).fetch_one(db.observe(&mut *tx)).await?;
    if recent > 0 {
        return Ok(());
    }
    revoke_unused_tickets(db, &mut tx, uid, now).await?;
    let raw = identity::random_token();
    insert(
        db,
        &mut tx,
        uid,
        &email,
        &digest(&raw, true),
        now,
        now + ACCESS_SECONDS,
    )
    .await?;
    db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,'privacy_access','member',?,?,?,?)").bind(format!("privacy.access:{}",identity::token_hash(&raw))).bind(uid).bind(crate::mail::identity_payload(&email,&db::text(&m,"username")?,&raw)).bind(now).bind(now).execute(db.observe(&mut *tx)).await?;
    tx.commit().await?;
    Ok(())
}
/// Only voids email tickets not yet redeemed; sessions already exchanged for a cookie are unaffected. Otherwise anyone
/// who knows someone's email could request once a minute and invalidate that person's data-request session each time.
///
/// Cookie credentials and tickets share the same table and purpose and are never marked consumed, so they can only be
/// told apart by the redemption correspondence: `exchange_access` consumes the ticket while inserting the cookie row,
/// so the cookie row's issue time is the ticket's consumption time and its expiry is taken from the ticket. The caller
/// holds the member row lock and one account gets at most one ticket per 60 seconds, so no two tickets are issued in
/// the same second and the correspondence is unique. Expired rows are unusable anyway and are left alone.
async fn revoke_unused_tickets(db: &Database, tx: &mut Tx<'_>, uid: i64, now: i64) -> Result<()> {
    let rows = db::query("SELECT id,dateline,expires_at,consumed_at FROM gonof_common_member_token WHERE uid=? AND purpose='privacy_access' AND revoked_at IS NULL AND expires_at>? FOR UPDATE")
        .bind(uid).bind(now).fetch_all(db.observe(&mut **tx)).await?;
    let mut exchanged = std::collections::HashSet::new();
    let mut unconsumed = Vec::new();
    for row in &rows {
        let expires: i64 = row.try_get("expires_at")?;
        match row.try_get::<Option<i64>, _>("consumed_at")? {
            Some(consumed) => {
                exchanged.insert((consumed, expires));
            }
            None => unconsumed.push((
                row.try_get::<i64, _>("id")?,
                row.try_get::<i64, _>("dateline")?,
                expires,
            )),
        }
    }
    for (id, dateline, expires) in unconsumed {
        if exchanged.contains(&(dateline, expires)) {
            continue;
        }
        db::query("UPDATE gonof_common_member_token SET revoked_at=GREATEST(dateline,?) WHERE id=? AND consumed_at IS NULL AND revoked_at IS NULL")
            .bind(now).bind(id).execute(db.observe(&mut **tx)).await?;
    }
    Ok(())
}
async fn insert(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    email: &str,
    hash: &str,
    now: i64,
    expires: i64,
) -> Result<()> {
    use identity::rules::{TokenPurpose, insert_token};
    insert_token(
        db,
        tx,
        uid,
        TokenPurpose::PrivacyAccess,
        hash,
        email,
        now,
        expires,
    )
    .await?;
    Ok(())
}
pub async fn check_access(db: &Database, raw: &str, now: i64) -> Result<()> {
    let mut tx = db.begin_write().await?;
    owner(db, &mut tx, &Access::Token(raw.into()), now, true).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn exchange_access(db: &Database, raw: &str, now: i64) -> Result<(String, i64)> {
    let mut tx = db.begin_write().await?;
    let uid = owner(db, &mut tx, &Access::Token(raw.into()), now, true).await?;
    let t=db::query("SELECT email,expires_at FROM gonof_common_member_token WHERE token_hash=? AND purpose='privacy_access' FOR UPDATE").bind(digest(raw,true)).fetch_one(db.observe(&mut *tx)).await?;
    // Redeemed only once (guaranteed here, not by the schema): `owner` was already checked under the lock, and the
    // conditional update blocks once more.
    let consumed = db::query("UPDATE gonof_common_member_token SET consumed_at=? WHERE token_hash=? AND uid=? AND purpose='privacy_access' AND consumed_at IS NULL AND revoked_at IS NULL AND dateline<=? AND expires_at>?")
        .bind(now).bind(digest(raw,true)).bind(uid).bind(now).bind(now).execute(db.observe(&mut *tx)).await?.rows_affected();
    if consumed != 1 {
        return Err(IdentityError::Unauthorized);
    }
    let next = identity::random_token();
    let expires: i64 = t.try_get("expires_at")?;
    insert(
        db,
        &mut tx,
        uid,
        &db::text(&t, "email")?,
        &digest(&next, false),
        now,
        expires,
    )
    .await?;
    tx.commit().await?;
    Ok((next, expires - now))
}
pub async fn exit_access(db: &Database, raw: &str, now: i64) -> Result<()> {
    if valid_key(raw) {
        db::query("UPDATE gonof_common_member_token SET revoked_at=GREATEST(dateline,?) WHERE token_hash=? AND purpose='privacy_access' AND consumed_at IS NULL AND revoked_at IS NULL").bind(now).bind(digest(raw,false)).execute(db.executor()).await?;
    }
    Ok(())
}
