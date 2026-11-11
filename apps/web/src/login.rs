//! Sign-in methods and participation conditions: the site-wide settings `settings.login.*` are the default; when a
//! request lands on another registered domain and that row's column of the same name is not NULL, the domain wins. The
//! primary domain, unregistered Hosts and a disabled multi-domain setup all use the site-wide settings.
//!
//! Outside a request (background tasks, the command line) there is no site, so the site-wide settings always apply.

use crate::identity::Member;
use crate::runtime::settings::{SiteSettings, set_contains};

/// Additional methods shown on the sign-in page and their names in this request's language, in the order of
/// [`crate::runtime::settings::LOGIN_METHODS`].
pub fn method_labels() -> [(&'static str, &'static str); 3] {
    let t = crate::i18n::current();
    [
        ("sms", t.account_manage_method_sms()),
        ("google", t.account_manage_method_google()),
        ("github", t.account_manage_method_github()),
    ]
}

/// Participation conditions and their names in this request's language, in the order of
/// [`crate::runtime::settings::PARTICIPATION`].
pub fn participation_labels() -> [(&'static str, &'static str); 4] {
    let t = crate::i18n::current();
    [
        ("email", t.account_manage_participation_email()),
        ("mobile", t.account_manage_participation_mobile()),
        ("google", t.account_manage_participation_google()),
        ("github", t.account_manage_participation_github()),
    ]
}

/// The sign-in methods in effect for this request (a comma-separated set; an empty string means password sign-in
/// only).
pub fn methods(settings: &SiteSettings) -> String {
    crate::site::other(|row| row.login_methods.clone())
        .flatten()
        .unwrap_or_else(|| settings.login.methods.clone())
}

/// Whether this request's sign-in page shows SMS sign-in.
pub fn sms_enabled(settings: &SiteSettings) -> bool {
    set_contains(&methods(settings), "sms")
}

/// Whether this request has this third-party sign-in checked (`google` / `github`). Whether the credentials are
/// configured is decided separately by [`crate::oauth`].
pub fn provider_checked(settings: &SiteSettings, provider: &str) -> bool {
    set_contains(&methods(settings), provider)
}

/// Which third-party accounts the member has bound. Only read when the participation conditions need it
/// ([`linked`]); otherwise all false.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Linked {
    pub google: bool,
    pub github: bool,
}

/// Read the member's bound third-party accounts: by the leftmost column of the unique key `(uid, provider)`, at
/// most two rows. The caller wraps `executor` (`db.observe(&mut **tx)` or `db.executor()`), so the query counts
/// toward the debug panel and the footer.
pub async fn linked<'c, E>(executor: E, uid: i64) -> Result<Linked, sqlx::Error>
where
    E: crate::db::Executor<'c>,
{
    let providers: Vec<String> =
        crate::db::query_scalar("SELECT provider FROM gonof_common_member_external WHERE uid=?")
            .bind(uid)
            .fetch_all(executor)
            .await?;
    Ok(Linked {
        google: providers.iter().any(|provider| provider == "google"),
        github: providers.iter().any(|provider| provider == "github"),
    })
}

/// Judge participation conditions after the write transaction's locking read of the member row: e-mail and mobile
/// number are on the member row; the binding table is queried only when neither is met and this request's
/// conditions include google or github. The member row is already locked, so this is serialized with unbinding
/// (which locks the member row first).
pub async fn permits<'c, E>(
    executor: E,
    uid: i64,
    email_verified: bool,
    has_mobile: bool,
) -> Result<bool, sqlx::Error>
where
    E: crate::db::Executor<'c>,
{
    let participation = Participation::current();
    if !participation.needs_links(email_verified, has_mobile) {
        return Ok(participation.allows(email_verified, has_mobile));
    }
    Ok(participation.linked_by(linked(executor, uid).await?))
}

/// Participation conditions: posting threads, replies, private messages, likes, downvotes and tips, joining
/// campaigns, uploading, generating invite codes and creating API tokens all require any one of them. Administrators
/// are exempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Participation {
    pub email: bool,
    pub mobile: bool,
    pub google: bool,
    pub github: bool,
}

impl Participation {
    /// Parse a set encoded by [`crate::runtime::settings::set_shape`]; unknown items are ignored (already validated
    /// when decoding).
    pub fn parse(set: &str) -> Self {
        Self {
            email: set_contains(set, "email"),
            mobile: set_contains(set, "mobile"),
            google: set_contains(set, "google"),
            github: set_contains(set, "github"),
        }
    }

    /// The participation conditions in effect for this request: computed by the entry layer together with the site
    /// ([`crate::site::Site::participation`]). Outside a request (background tasks, the command line, tests calling
    /// services directly) it is the pre-change definition [`Self::EMAIL`].
    pub fn current() -> Self {
        crate::site::current().participation
    }

    /// The pre-change definition: only a verified e-mail counts. For tests and entry points outside a request.
    pub const EMAIL: Self = Self {
        email: true,
        mobile: false,
        google: false,
        github: false,
    };

    /// Whether the member meets any of them (account status and admin roles are not considered; those are judged by
    /// [`Member::can_contribute`]). Third-party bindings come from [`Member::linked`], filled in by the code that
    /// reads the member according to [`Self::needs_links`].
    pub fn met_by(&self, member: &Member) -> bool {
        self.allows(member.email_verified, member.mobile.is_some()) || self.linked_by(member.linked)
    }

    /// The two items on the member row: `emailstatus = 1`, `mobile IS NOT NULL`.
    pub fn allows(&self, email_verified: bool, has_mobile: bool) -> bool {
        (self.email && email_verified) || (self.mobile && has_mobile)
    }

    /// The two items on the binding table.
    pub fn linked_by(&self, linked: Linked) -> bool {
        (self.google && linked.google) || (self.github && linked.github)
    }

    /// The binding table only needs to be read when neither item on the member row is met and third-party bindings
    /// count.
    pub fn needs_links(&self, email_verified: bool, has_mobile: bool) -> bool {
        (self.google || self.github) && !self.allows(email_verified, has_mobile)
    }

    /// The error when not met: when only e-mail is required, each entry point keeps its original wording; otherwise
    /// it says which of the items this site requires.
    pub fn message(&self, email_only: &'static str) -> &'static str {
        if *self == Self::EMAIL {
            email_only
        } else {
            self.requirement()
        }
    }

    /// The same hint in a page's language, for the banner under the header; error texts use [`Self::requirement`],
    /// which words it in the current language.
    pub fn requirement_in(&self, lang: impl std::borrow::Borrow<crate::i18n::Lang>) -> String {
        let lang = *lang.borrow();
        match (self.email, self.mobile, self.google, self.github) {
            (true, false, false, false) => lang.frame_need_email(),
            (false, true, false, false) => lang.frame_need_mobile(),
            (true, true, false, false) => lang.frame_need_email_mobile(),
            (false, false, true, false) => lang.frame_need_google(),
            (false, false, false, true) => lang.frame_need_github(),
            (false, false, true, true) => lang.frame_need_google_github(),
            (true, false, true, false) => lang.frame_need_email_google(),
            (true, false, false, true) => lang.frame_need_email_github(),
            (true, false, true, true) => lang.frame_need_email_google_github(),
            (false, true, true, false) => lang.frame_need_mobile_google(),
            (false, true, false, true) => lang.frame_need_mobile_github(),
            (false, true, true, true) => lang.frame_need_mobile_google_github(),
            (true, true, true, false) => lang.frame_need_email_mobile_google(),
            (true, true, false, true) => lang.frame_need_email_mobile_github(),
            _ => lang.frame_need_any(),
        }
        .to_owned()
    }

    /// The hint when not met: "Please verify your email or add a phone number first".
    pub fn requirement(&self) -> &'static str {
        match (self.email, self.mobile, self.google, self.github) {
            (true, false, false, false) => crate::i18n::current().common_need_email(),
            (false, true, false, false) => crate::i18n::current().common_need_mobile(),
            (true, true, false, false) => crate::i18n::current().common_need_email_mobile(),
            (false, false, true, false) => crate::i18n::current().common_need_google(),
            (false, false, false, true) => crate::i18n::current().common_need_github(),
            (false, false, true, true) => crate::i18n::current().common_need_google_github(),
            (true, false, true, false) => crate::i18n::current().common_need_email_google(),
            (true, false, false, true) => crate::i18n::current().common_need_email_github(),
            (true, false, true, true) => crate::i18n::current().common_need_email_google_github(),
            (false, true, true, false) => crate::i18n::current().common_need_mobile_google(),
            (false, true, false, true) => crate::i18n::current().common_need_mobile_github(),
            (false, true, true, true) => crate::i18n::current().common_need_mobile_google_github(),
            (true, true, true, false) => crate::i18n::current().common_need_email_mobile_google(),
            (true, true, false, true) => crate::i18n::current().common_need_email_mobile_github(),
            _ => crate::i18n::current().common_need_any(),
        }
    }
}

impl Default for Participation {
    fn default() -> Self {
        Self::EMAIL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participation_parses_the_stored_set() {
        assert_eq!(Participation::parse("email"), Participation::EMAIL);
        let both = Participation::parse("email,mobile");
        assert!(both.email && both.mobile);
        let mobile = Participation::parse("mobile");
        assert!(!mobile.email && mobile.mobile);
        assert_eq!(mobile.requirement(), "This site needs a phone number first");
        assert_eq!(
            both.requirement(),
            "Please verify your email or add a phone number first"
        );
        let abroad = Participation::parse("email,google,github");
        assert!(abroad.email && abroad.google && abroad.github && !abroad.mobile);
        assert_eq!(
            abroad.requirement(),
            "Please verify your email or link a Google or GitHub account first"
        );
        assert!(abroad.needs_links(false, true));
        assert!(!abroad.needs_links(true, false));
        assert!(
            !both.needs_links(false, false),
            "without third-party sign-in the link table is not read"
        );
        assert!(abroad.linked_by(Linked {
            google: false,
            github: true
        }));
        assert!(!mobile.linked_by(Linked {
            google: true,
            github: true
        }));
    }
}
