//! Bulk-imports member profiles into a test environment; unlike `seed::import_fixture`, this does not require an empty
//! content database.
//! This is a destructive operation: before importing it empties all existing accounts in `gonof_common_member`
//! (effectively resetting every account), so it must only run in test environments that allow resets, never against a
//! database with real user data.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use rand::RngCore;
use rand::rngs::OsRng;
use serde::Deserialize;

use crate::db::Database;
use crate::identity::rules::{AccountState, Credentials, check_username};

fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow::anyhow!("password hashing failed: {error}"))
}

/// The member credential rules (`identity::rules::Credentials`, checked by code before every member write) require
/// accounts with an e-mail address, or the site owner's account, to have a non-empty password digest; this generates
/// a random password that is never written to disk or sent anywhere and is discarded at once, keeping only its
/// Argon2id digest: the account exists, but nobody knows a password that logs in.
fn random_unusable_password_hash() -> Result<String> {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let password = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    hash_password(&password)
}

/// Computes `count` unusable placeholder digests before the transaction opens. Argon2 is slow, and SQLite has a single
/// writer for the whole site, so it is not computed inside the write transaction; it runs on a blocking thread so it
/// does not occupy an async thread.
async fn unusable_password_hashes(count: usize) -> Result<Vec<String>> {
    tokio::task::spawn_blocking(move || {
        (0..count)
            .map(|_| random_unusable_password_hash())
            .collect()
    })
    .await
    .context("the placeholder password hash task failed")?
}

/// Temporary switch for terminal echo: turns ECHO off on construction and restores the original setting on drop.
///
/// When standard input is not a terminal (pipe, redirect) [`EchoOff::new`] returns `None`; just read as usual.
pub struct EchoOff {
    fd: std::os::fd::RawFd,
    original: libc::termios,
}

impl EchoOff {
    pub fn new(fd: std::os::fd::RawFd) -> Result<Option<Self>> {
        // SAFETY: isatty/tcgetattr/tcsetattr only read and write the descriptor passed by the caller and the termios
        // on the stack.
        if unsafe { libc::isatty(fd) } != 1 {
            return Ok(None);
        }
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot read the terminal settings");
        }
        let original = unsafe { original.assume_init() };
        let mut silent = original;
        // Only echo is turned off; ECHONL stays, so the line still breaks after Enter.
        silent.c_lflag &= !(libc::ECHO | libc::ECHOE | libc::ECHOK);
        silent.c_lflag |= libc::ECHONL;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &silent) } != 0 {
            return Err(std::io::Error::last_os_error()).context("cannot turn off terminal echo");
        }
        Ok(Some(Self { fd, original }))
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: restores the settings read at construction; the descriptor is still owned by the caller.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

/// Reads one line of password from standard input without echoing it on a terminal; strips the trailing newline.
pub fn read_password_line() -> Result<String> {
    use std::io::BufRead;
    let _echo = EchoOff::new(libc::STDIN_FILENO)?;
    let mut password = String::new();
    std::io::stdin().lock().read_line(&mut password)?;
    Ok(password.trim_end_matches(['\n', '\r']).to_owned())
}

/// Creates a new file readable and writable only by its owner (0600): the permission is applied at creation, and umask
/// can only make it stricter, never looser. If the target already exists (symbolic links included) it errors, without
/// overwriting or keeping the old file's permissions.
pub fn create_secret_file(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| {
            format!(
                "cannot create the output file {} (if it exists, use another path or delete it first)",
                path.display()
            )
        })
}

/// Sets the password of a uid to plaintext chosen by the caller (only for setting an administrator password by hand on
/// a local test database).
pub async fn set_password(db: &Database, uid: i64, password: &str) -> Result<()> {
    ensure!(
        password.chars().count() >= 12 && password.chars().count() <= 128,
        "the password must be 12-128 characters"
    );
    let hash = hash_password(password)?;
    let mut transaction = db.begin_write().await?;
    // Two cases are refused: setting a password on a deleted or deleting account, and setting one on a regular
    // account without an e-mail address (synthetic authors, seed authors). After locking the member row the code
    // decides, with the same credential combination rules as the web entry points.
    let member = crate::db::query(
        "SELECT email, mobile, emailstatus, adminid, account_state FROM gonof_common_member WHERE uid = ? FOR UPDATE",
    )
    .bind(uid)
    .fetch_optional(db.observe(&mut *transaction))
    .await?
    .with_context(|| format!("uid={uid} does not exist"))?;
    let state: String = member.try_get("account_state")?;
    ensure!(
        AccountState::parse(&state) == Some(AccountState::Active),
        "uid={uid} is deleted or being deleted; cannot set a password"
    );
    let email: Option<String> = member.try_get("email")?;
    let mobile: Option<String> = member.try_get("mobile")?;
    let linked = crate::login::linked(db.observe(&mut *transaction), uid).await?;
    Credentials {
        password: Some(&hash),
        email: email.as_deref(),
        mobile: mobile.as_deref(),
        external: linked.google || linked.github,
        emailstatus: member.try_get("emailstatus")?,
        adminid: member.try_get("adminid")?,
    }
    .check()
    .with_context(|| {
        format!("uid={uid} has no e-mail and is not the site owner; cannot set a password")
    })?;
    let result = crate::db::query("UPDATE gonof_common_member SET password = ?, credentials_version = credentials_version + 1 WHERE uid = ?")
        .bind(&hash)
        .bind(uid)
        .execute(db.observe(&mut *transaction))
        .await?;
    ensure!(result.rows_affected() == 1, "uid={uid} does not exist");
    let timestamp = crate::identity::now();
    // Same as a web password change: old cookies, plus account recovery and private data access links issued before the
    // change, are invalidated together.
    let revoked = crate::db::query("UPDATE gonof_common_member_token SET revoked_at = GREATEST(dateline, ?) WHERE uid = ? AND purpose IN ('password_reset','email_change','appeal_access','privacy_access') AND consumed_at IS NULL AND revoked_at IS NULL")
        .bind(timestamp)
        .bind(uid)
        .execute(db.observe(&mut *transaction))
        .await?
        .rows_affected();
    let version: i64 = crate::db::query_scalar(
        "SELECT credentials_version FROM gonof_common_member WHERE uid = ?",
    )
    .bind(uid)
    .fetch_one(db.observe(&mut *transaction))
    .await?;
    // A web password change leaves an audit record, so the command line leaves one too; otherwise this would be the only
    // credential change with no trace.
    // `operationuid` stays empty: the actor is whoever runs the command on this machine, not the account owner. The type
    // keeps the `identity.` prefix, so account deletion erases it together with the other credential records.
    crate::db::query(
        "INSERT INTO gonof_common_log (uid, operationuid, type, data, source, dateline)
         VALUES (?, NULL, 'identity.password.set', ?, 'cli', ?)",
    )
    .bind(uid)
    .bind(
        serde_json::json!({"target_uid": uid, "credentials_version": version, "revoked_tokens": revoked}).to_string(),
    )
    .bind(timestamp)
    .execute(db.observe(&mut *transaction))
    .await?;
    transaction.commit().await?;
    Ok(())
}

#[derive(Deserialize)]
struct ProfileFixture {
    roles: Vec<ProfileEntry>,
}

#[derive(Deserialize)]
struct ProfileEntry {
    uid: i64,
    username: String,
    bio: String,
    #[serde(default)]
    regdate: i64,
}

/// Reads the uids/usernames in the profile list, so other bulk operations (such as API token provisioning) reuse the
/// same list.
pub fn read_uid_username_pairs(path: &Path) -> Result<Vec<(i64, String)>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the profile list: {}", path.display()))?;
    let fixture: ProfileFixture = serde_json::from_str(&raw)
        .with_context(|| format!("cannot parse the profile list: {}", path.display()))?;
    Ok(fixture
        .roles
        .into_iter()
        .map(|entry| (entry.uid, entry.username))
        .collect())
}

/// Import result: the number of members actually inserted this time.
pub struct ImportSummary {
    pub inserted: usize,
}

/// Bulk-imports member accounts from a JSON profile list; uid=1 is treated as the super administrator, everyone else
/// as a regular member.
/// Before importing, all existing accounts in `gonof_common_member` are emptied (reset), then the accounts in the list
/// are inserted in the same transaction.
/// For test environments only. `confirm_database` must equal the name of the currently connected database before
/// anything happens: if the configuration or `GONOF_DATABASE_URL` points at the wrong database, the command stops
/// before emptying anything.
pub async fn import_member_profiles(
    db: &Database,
    path: &Path,
    confirm_database: &str,
) -> Result<ImportSummary> {
    let current: Option<String> = match db.backend() {
        crate::db::Backend::MySql => {
            crate::db::query_scalar("SELECT DATABASE()")
                .fetch_one(db.executor())
                .await?
        }
        // SQLite has no database name; use the database file name without its extension (as in
        // `Pool::database_name`).
        crate::db::Backend::Sqlite => db.pool.database_name(),
    };
    let current = current.unwrap_or_default();
    ensure!(
        !current.is_empty() && confirm_database == current,
        "the import clears all members of database `{current}`; confirm by naming it (--confirm-reset={current})"
    );
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the profile list: {}", path.display()))?;
    let fixture: ProfileFixture = serde_json::from_str(&raw)
        .with_context(|| format!("cannot parse the profile list: {}", path.display()))?;
    ensure!(!fixture.roles.is_empty(), "the profile list is empty");

    let mut seen_uids = std::collections::HashSet::new();
    let mut seen_usernames = std::collections::HashSet::new();
    for entry in &fixture.roles {
        ensure!(
            entry.uid > 0,
            "the profile list has an invalid uid: {}",
            entry.uid
        );
        let username = entry.username.trim();
        ensure!(
            !username.is_empty(),
            "the profile list has an empty username (uid={})",
            entry.uid
        );
        ensure!(
            seen_uids.insert(entry.uid),
            "duplicate uid in the profile list: {}",
            entry.uid
        );
        ensure!(
            seen_usernames.insert(username.to_owned()),
            "duplicate username in the profile list: {username}"
        );
    }

    // One placeholder digest per account (the extra one is discarded when uid=1 keeps its old digest), computed before
    // the transaction opens.
    let mut hashes = unusable_password_hashes(fixture.roles.len())
        .await?
        .into_iter();
    let mut next_hash = || {
        hashes
            .next()
            .context("not enough placeholder password hashes")
    };

    // Foreign key checks are switched off below; that is a session variable that a transaction rollback cannot undo.
    // This connection is closed after use, not returned to the pool: whichever step fails midway,
    // `FOREIGN_KEY_CHECKS = 0` cannot go back to the pool with the connection and be picked up by another request.
    let mut transaction = db.begin_write_closing().await?;

    // Before the reset, keep the e-mail address and password digest of the existing uid=1 account, so after the new
    // profile replaces it, the administrator password that logged in to the test site before the reset still works;
    // only when there is no old account is an unusable placeholder digest made.
    let existing_admin_credentials =
        crate::db::query("SELECT email, password FROM gonof_common_member WHERE uid = 1")
            .fetch_optional(db.observe(&mut *transaction))
            .await?
            .map(|row| {
                anyhow::Ok((
                    row.try_get::<Option<String>, _>("email")?,
                    row.try_get::<Option<String>, _>("password")?,
                ))
            })
            .transpose()?;

    // Foreign key checks are switched off only for emptying the accounts table; gonof_common_member is referenced by
    // foreign keys from many tables (posts, notifications, reports…), and resetting accounts in a test environment does
    // not empty those tables too.
    // SQLite cannot switch foreign key checks off inside a transaction, so checks are deferred to commit instead: if
    // references are still dangling after the listed accounts are added back, the whole transaction rolls back, which
    // is stricter than MySQL. No foreign key referencing the member table has a cascading action, so deferred checks
    // cannot delete rows along the way.
    match db.backend() {
        crate::db::Backend::MySql => {
            crate::db::query("SET FOREIGN_KEY_CHECKS = 0")
                .execute(db.observe(&mut *transaction))
                .await?;
            crate::db::query("DELETE FROM gonof_common_member")
                .execute(db.observe(&mut *transaction))
                .await?;
            crate::db::query("SET FOREIGN_KEY_CHECKS = 1")
                .execute(db.observe(&mut *transaction))
                .await?;
        }
        crate::db::Backend::Sqlite => {
            crate::db::query("PRAGMA defer_foreign_keys = ON")
                .written_for(crate::db::Backend::Sqlite)
                .execute(db.observe(&mut *transaction))
                .await?;
            crate::db::query("DELETE FROM gonof_common_member")
                .execute(db.observe(&mut *transaction))
                .await?;
        }
    }

    let sql = "INSERT INTO gonof_common_member (uid, username, bio, adminid, regdate, email, emailstatus, password) VALUES (?, ?, ?, ?, ?, ?, ?, ?)";
    for entry in &fixture.roles {
        let adminid: i64 = if entry.uid == 1 { 1 } else { 0 };
        // The credential rules require a non-empty password digest before an e-mail address/verified state is allowed;
        // uid=1 reuses its old credentials from before the reset, and every other account gets a placeholder e-mail
        // address and is marked verified, so the e-mail threshold of the like/favorite/reply endpoints lets it through;
        // these passwords cannot log in and only exist to satisfy the constraints.
        let (email, password) = if adminid == 1 {
            match &existing_admin_credentials {
                Some((email, Some(password))) => (email.clone(), Some(password.clone())),
                _ => (None, Some(next_hash()?)),
            }
        } else {
            (
                Some(format!("member{}@gonof.test", entry.uid)),
                Some(next_hash()?),
            )
        };
        let emailstatus: i64 = if email.is_some() { 1 } else { 0 };
        let regdate = if entry.regdate > 0 {
            entry.regdate
        } else {
            i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
            )?
        };
        // The member value rules: username length and leading/trailing whitespace, and the credential combination.
        // The username is only trimmed, not passed through normalize_username.
        check_username(entry.username.trim())?;
        Credentials {
            password: password.as_deref(),
            email: email.as_deref(),
            mobile: None,
            external: false,
            emailstatus,
            adminid,
        }
        .check()?;
        crate::db::query(sql)
            .bind(entry.uid)
            .bind(entry.username.trim())
            .bind(&entry.bio)
            .bind(adminid)
            .bind(regdate)
            .bind(email)
            .bind(emailstatus)
            .bind(password)
            .execute(db.observe(&mut *transaction))
            .await?;
    }
    transaction.commit().await?;

    // AUTO_INCREMENT is DDL and commits implicitly, so it must stay outside the transaction; otherwise later real
    // registrations might collide with this batch's fixed uids.
    let next_uid: i64 =
        crate::db::query_scalar("SELECT COALESCE(MAX(uid), 0) + 1 FROM gonof_common_member")
            .fetch_one(db.executor())
            .await?;
    match db.backend() {
        crate::db::Backend::MySql => {
            crate::db::query(&format!(
                "ALTER TABLE gonof_common_member AUTO_INCREMENT = {next_uid}"
            ))
            .execute(db.executor())
            .await?;
        }
        // SQLite keeps AUTOINCREMENT in sqlite_sequence; explicitly inserting large IDs raises it automatically, and
        // emptying the table does not lower it; set it to this batch's largest uid, so the next registered member gets
        // MAX(uid)+1, as in MySQL.
        crate::db::Backend::Sqlite => {
            crate::db::query(
                "UPDATE sqlite_sequence SET seq = ? WHERE name = 'gonof_common_member'",
            )
            .bind(next_uid - 1)
            .execute(db.executor())
            .await?;
        }
    }

    Ok(ImportSummary {
        inserted: fixture.roles.len(),
    })
}

/// Patch: for accounts already imported whose e-mail address is unverified (other than uid=1), fills in a placeholder
/// e-mail address, verified state and an unusable password digest, meeting the like/favorite/reply endpoints' threshold
/// of "e-mail verified or administrator". Accounts that already have an e-mail address are skipped, not overwritten.
/// Returns the number of accounts actually updated.
pub async fn enable_interactions(db: &Database, path: &Path) -> Result<usize> {
    let members = read_uid_username_pairs(path)?;
    let hashes = unusable_password_hashes(members.len()).await?;
    let mut transaction = db.begin_write().await?;
    // Only regular accounts are patched: deleted accounts also have an empty e-mail address, and the condition keeps
    // them out.
    let sql = "UPDATE gonof_common_member SET email = ?, emailstatus = 1, password = ?
               WHERE uid = ? AND uid <> 1 AND email IS NULL AND account_state = ?";
    let mut updated = 0usize;
    for ((uid, _username), password) in members.into_iter().zip(hashes) {
        let email = format!("member{uid}@gonof.test");
        Credentials {
            password: Some(&password),
            email: Some(&email),
            mobile: None,
            external: false,
            emailstatus: 1,
            adminid: 0,
        }
        .check()?;
        let result = crate::db::query(sql)
            .bind(&email)
            .bind(&password)
            .bind(uid)
            .bind(AccountState::Active.value())
            .execute(db.observe(&mut *transaction))
            .await?;
        updated += usize::try_from(result.rows_affected()).unwrap_or(0);
    }
    transaction.commit().await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::PermissionsExt;

    fn echo_enabled(fd: std::os::fd::RawFd) -> bool {
        let mut current = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(unsafe { libc::tcgetattr(fd, current.as_mut_ptr()) }, 0);
        unsafe { current.assume_init() }.c_lflag & libc::ECHO != 0
    }

    /// The set-password help promises no echo: the terminal does not echo while the password is read, and
    /// echo is restored afterwards.
    #[test]
    fn password_prompt_turns_terminal_echo_off_and_restores_it() {
        let (mut master, mut slave) = (0, 0);
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty failed");
        let mut master = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        assert!(
            echo_enabled(slave.as_raw_fd()),
            "a new terminal echoes by default"
        );
        let guard = EchoOff::new(slave.as_raw_fd())
            .unwrap()
            .expect("a pseudo-terminal is a terminal");
        assert!(!echo_enabled(slave.as_raw_fd()));
        master.write_all(b"secret-passphrase\n").unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&slave)
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "secret-passphrase\n");
        drop(guard);
        assert!(
            echo_enabled(slave.as_raw_fd()),
            "echo is restored after reading"
        );
        // With echo off, the terminal only echoes a newline (ECHONL); the password itself is never written back to the
        // screen.
        unsafe {
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let mut echoed = Vec::new();
        let mut buffer = [0u8; 256];
        while let Ok(read) = master.read(&mut buffer) {
            if read == 0 {
                break;
            }
            echoed.extend_from_slice(&buffer[..read]);
        }
        assert!(
            !String::from_utf8_lossy(&echoed).contains("secret"),
            "the passphrase was echoed: {echoed:?}"
        );
        // Input that is not a terminal (pipe, redirect) does not change the settings.
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(EchoOff::new(file.as_raw_fd()).unwrap().is_none());
    }

    /// The file enable-api-keys writes plaintext tokens to is 0600 from creation, and an existing file is not
    /// overwritten.
    #[test]
    fn secret_files_are_created_owner_only_and_never_overwritten() {
        let directory = std::env::temp_dir().join(format!(
            "gonof_secret_{}_{}",
            std::process::id(),
            crate::identity::random_token()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("keys.json");
        // umask is process-wide and cannot be changed while tests run in parallel; with the common 022, `fs::write`
        // gives 0644.
        let mut file = create_secret_file(&path).unwrap();
        file.write_all(b"[]\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "only the owner can read and write");
        assert!(
            create_secret_file(&path).is_err(),
            "an existing file must not be overwritten"
        );
        let link = directory.join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(
            create_secret_file(&link).is_err(),
            "symbolic links are not followed"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"[]\n");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
