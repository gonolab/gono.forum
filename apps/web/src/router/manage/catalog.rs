//! Admin catalog: the left-hand modules, which module each tab belongs to, and reverse lookup by address.
//!
//! The routes are a given; this only declares "which address belongs to which tab of which module", parsed once per
//! request by [`super::super::meta`] for `layout/manage.html` to render the two-level navigation.
//! The model follows `Section` / `Entry` / `menu` in `router/my.rs`.
//!
//! Visibility has a single source of truth, [`Entry::admin_only`]: modules carry no switch of their own, and a module
//! with no visible tab for the current role does not appear at all. This rules out the self-contradictory and
//! unfalsifiable state of "the module says moderators can see it, but all its tabs are administrator-only".

/// A top-level module on the left.
pub(in crate::router) struct Module {
    pub id: &'static str,
    /// The suffix of `<symbol id="manage-icon-{icon}">` in `partials/manage_icons.html`.
    pub icon: &'static str,
}

/// A page under a module; top tabs render in this order, and the first visible tab of a module is where the module
/// lands.
pub(in crate::router) struct Entry {
    pub id: &'static str,
    pub path: &'static str,
    pub module: &'static str,
    pub access: Access,
}

impl Module {
    /// The module's name in this request's language.
    pub fn title(&self) -> &'static str {
        let t = crate::i18n::current();
        match self.id {
            "workbench" => t.manage_module_workbench(),
            "global" => t.manage_module_global(),
            "languages" => t.manage_module_languages(),
            "appearance" => t.manage_module_appearance(),
            "review" => t.manage_module_review(),
            "users" => t.manage_module_users(),
            "forum" => t.manage_module_forum(),
            "domains" => t.manage_module_domains(),
            "security" => t.manage_module_security(),
            _ => t.manage_module_tools(),
        }
    }
}

impl Entry {
    /// The tab's name in this request's language. The points tab is named after the site's points
    /// (`settings.site.credit_name`), which only the page frame knows: `layout/manage.html` puts it in through
    /// `manage-tab-credits`; here it is the language's default word.
    pub fn title(&self) -> &'static str {
        let t = crate::i18n::current();
        match self.id {
            "credits" => t.brand_credit_name(),
            "overview" => t.manage_tab_overview(),
            "settings" => t.manage_tab_settings(),
            "settings-registration" => t.manage_tab_settings_registration(),
            "login-methods" => t.manage_tab_login_methods(),
            "settings-search" => t.manage_tab_settings_search(),
            "policy-terms" => t.manage_tab_policy_terms(),
            "policy-privacy" => t.manage_tab_policy_privacy(),
            "policy-rules" => t.manage_tab_policy_rules(),
            "policy-contact" => t.manage_tab_policy_contact(),
            "storage" => t.manage_tab_storage(),
            "languages" => t.manage_tab_languages(),
            "appearance-parameters" => t.manage_tab_appearance_parameters(),
            "skins" => t.manage_tab_skins(),
            "navigation" => t.manage_tab_navigation(),
            "branding" => t.manage_tab_branding(),
            "links" => t.manage_tab_links(),
            "policy-links" => t.manage_tab_policy_links(),
            "reports" => t.manage_tab_reports(),
            "restorations" => t.manage_tab_restorations(),
            "appeals" => t.manage_tab_appeals(),
            "users" => t.manage_tab_users(),
            "registration-sources" => t.manage_tab_registration_sources(),
            "invites" => t.manage_tab_invites(),
            "sanctions" => t.manage_tab_sanctions(),
            "data-requests" => t.manage_tab_data_requests(),
            "verifications" => t.manage_tab_verifications(),
            "honors" => t.manage_tab_honors(),
            "policy-verified" => t.manage_tab_policy_verified(),
            "content" => t.manage_tab_content(),
            "forums" => t.manage_tab_forums(),
            "moderators" => t.manage_tab_moderators(),
            "domains" => t.manage_tab_domains(),
            "security-posting" => t.manage_tab_security_posting(),
            "security-account" => t.manage_tab_security_account(),
            "mail" => t.manage_tab_mail(),
            "mail-templates" => t.manage_tab_mail_templates(),
            "logs" => t.manage_tab_logs(),
            "tasks" => t.manage_tab_tasks(),
            "maintenance" => t.manage_tab_maintenance(),
            _ => t.manage_tab_updates(),
        }
    }
}

/// Which roles can see a tab (the roles are described in docs/roles.md, "Staff roles").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::router) enum Access {
    /// Administrators only.
    Admin,
    /// Administrators and global moderators: account sanctions (global moderators can only mute).
    Sanctions,
    /// The four content-managing roles (administrator, global moderator, moderator, trainee moderator), excluding
    /// reviewers.
    Content,
    /// All five staff roles: overview, report review and content (a reviewer's content list only has reported items).
    Staff,
}

pub(in crate::router) const MODULES: &[Module] = &[
    Module {
        id: "workbench",
        icon: "home",
    },
    Module {
        id: "global",
        icon: "settings",
    },
    // Interface languages: right after the global settings.
    Module {
        id: "languages",
        icon: "language",
    },
    Module {
        id: "appearance",
        icon: "layout",
    },
    Module {
        id: "review",
        icon: "check",
    },
    Module {
        id: "users",
        icon: "users",
    },
    Module {
        id: "forum",
        icon: "message",
    },
    // Multi-domain sites: at the same level as forum management, placed after it.
    Module {
        id: "domains",
        icon: "globe",
    },
    Module {
        id: "security",
        icon: "shield",
    },
    Module {
        id: "tools",
        icon: "tool",
    },
];

/// Tab titles ([`Entry::title`]) are short names; tests assert on their zh-CN text.
pub(in crate::router) const PAGES: &[Entry] = &[
    Entry {
        id: "overview",
        path: "/manage",
        module: "workbench",
        access: Access::Staff,
    },
    Entry {
        id: "settings",
        path: "/manage/settings",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "settings-registration",
        path: "/manage/settings/registration",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "login-methods",
        path: "/manage/login",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "settings-search",
        path: "/manage/settings/search",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "policy-terms",
        path: "/manage/policies/terms",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "policy-privacy",
        path: "/manage/policies/privacy",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "policy-rules",
        path: "/manage/policies/rules",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "policy-contact",
        path: "/manage/policies/contact",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "credits",
        path: "/manage/credits",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "storage",
        path: "/manage/storage",
        module: "global",
        access: Access::Admin,
    },
    Entry {
        id: "languages",
        path: "/manage/languages",
        module: "languages",
        access: Access::Admin,
    },
    Entry {
        id: "appearance-parameters",
        path: "/manage/appearance/parameters",
        module: "appearance",
        access: Access::Admin,
    },
    Entry {
        id: "skins",
        path: "/manage/skins",
        module: "appearance",
        access: Access::Admin,
    },
    Entry {
        id: "navigation",
        path: "/manage/navigation",
        module: "appearance",
        access: Access::Admin,
    },
    Entry {
        id: "branding",
        path: "/manage/branding",
        module: "appearance",
        access: Access::Admin,
    },
    Entry {
        id: "links",
        path: "/manage/links",
        module: "appearance",
        access: Access::Admin,
    },
    Entry {
        id: "policy-links",
        path: "/manage/policies/links",
        module: "appearance",
        access: Access::Admin,
    },
    Entry {
        id: "reports",
        path: "/manage/reports",
        module: "review",
        access: Access::Staff,
    },
    Entry {
        id: "restorations",
        path: "/manage/restorations",
        module: "review",
        access: Access::Content,
    },
    Entry {
        id: "appeals",
        path: "/manage/appeals",
        module: "review",
        access: Access::Admin,
    },
    Entry {
        id: "users",
        path: "/manage/users",
        module: "users",
        access: Access::Admin,
    },
    Entry {
        id: "registration-sources",
        path: "/manage/registration-sources",
        module: "users",
        access: Access::Admin,
    },
    Entry {
        id: "invites",
        path: "/manage/invites",
        module: "users",
        access: Access::Admin,
    },
    Entry {
        id: "sanctions",
        path: "/manage/sanctions",
        module: "users",
        access: Access::Sanctions,
    },
    Entry {
        id: "data-requests",
        path: "/manage/data-requests",
        module: "users",
        access: Access::Admin,
    },
    Entry {
        id: "verifications",
        path: "/manage/verifications",
        module: "users",
        access: Access::Admin,
    },
    Entry {
        id: "honors",
        path: "/manage/honors",
        module: "users",
        access: Access::Admin,
    },
    Entry {
        id: "policy-verified",
        path: "/manage/policies/verified",
        module: "users",
        access: Access::Admin,
    },
    Entry {
        id: "content",
        path: "/manage/content",
        module: "forum",
        access: Access::Staff,
    },
    Entry {
        id: "forums",
        path: "/manage/forums",
        module: "forum",
        access: Access::Admin,
    },
    Entry {
        id: "moderators",
        path: "/manage/moderators",
        module: "forum",
        access: Access::Admin,
    },
    Entry {
        id: "domains",
        path: "/manage/domains",
        module: "domains",
        access: Access::Admin,
    },
    Entry {
        id: "security-posting",
        path: "/manage/security/posting",
        module: "security",
        access: Access::Admin,
    },
    Entry {
        id: "security-account",
        path: "/manage/security/account",
        module: "security",
        access: Access::Admin,
    },
    Entry {
        id: "mail",
        path: "/manage/mail",
        module: "security",
        access: Access::Admin,
    },
    Entry {
        id: "mail-templates",
        path: "/manage/mail/templates/email_verification",
        module: "security",
        access: Access::Admin,
    },
    Entry {
        id: "logs",
        path: "/manage/logs",
        module: "tools",
        access: Access::Content,
    },
    Entry {
        id: "tasks",
        path: "/manage/tasks",
        module: "tools",
        access: Access::Admin,
    },
    Entry {
        id: "maintenance",
        path: "/manage/maintenance",
        module: "tools",
        access: Access::Admin,
    },
    Entry {
        id: "updates",
        path: "/manage/updates",
        module: "tools",
        access: Access::Admin,
    },
];

/// Addresses that no [`Entry::path`] prefix covers → (address prefix, target [`Entry::id`]).
pub(in crate::router) const ALIASES: &[(&str, &str)] = &[
    // The eleven kinds all have different addresses, and the longest prefix would wrongly land on /manage/mail.
    ("/manage/mail/templates", "mail-templates"),
    // The admin entry and the author entry share a handler; the admin side belongs to the "Content" tab.
    ("/manage/threads", "content"),
];

pub(in crate::router) struct ModuleLink {
    pub title: &'static str,
    pub icon: &'static str,
    pub href: &'static str,
    pub current: bool,
}

pub(in crate::router) struct TabLink {
    pub title: &'static str,
    /// The [`Entry::id`], so the template can name the points tab after the site's points.
    pub page: &'static str,
    pub href: &'static str,
    pub current: bool,
}

/// The page frame data for one admin request, all computed in `meta()`; the template only renders.
pub(in crate::router) struct ManageFrame {
    pub module_title: &'static str,
    pub modules: Vec<ModuleLink>,
    pub tabs: Vec<TabLink>,
}

fn module(id: &str) -> Option<&'static Module> {
    MODULES.iter().find(|m| m.id == id)
}

fn module_pages(module: &str) -> impl Iterator<Item = &'static Entry> {
    PAGES.iter().filter(move |e| e.module == module)
}

fn visible(entry: &Entry, role: i64) -> bool {
    match entry.access {
        Access::Admin => role == 1,
        Access::Sanctions => matches!(role, 1 | 2),
        Access::Content => matches!(role, 1..=4),
        Access::Staff => matches!(role, 1..=5),
    }
}

/// Prefix check by path segment: `/manage/content` does not match `/manage/contents`.
fn under(path: &str, prefix: &str) -> bool {
    path == prefix || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
}

/// Whether a candidate prefix covers the address.
///
/// The admin root `/manage` is the overview page's own address, not the parent of every admin page: treating it as a
/// prefix would degrade "longest prefix" into always matching, so an unregistered admin page would silently highlight
/// as the overview and the omission would go unnoticed. Matching only itself makes an unregistered address fall back
/// to a single column, which is obvious at a glance.
fn covers(path: &str, prefix: &str) -> bool {
    if prefix == "/manage" {
        path == prefix
    } else {
        under(path, prefix)
    }
}

/// Catalog addresses and aliases go into one candidate table, and the longest prefix wins.
///
/// Candidates matching the same address are necessarily nested, so byte length and segment count sort the same way,
/// and one `max_by_key` suffices. Aliases get no "priority" but compete in the same table, so a longer entry added
/// later cannot be silently bypassed.
fn entry_for_path(path: &str) -> Option<&'static Entry> {
    let id = PAGES
        .iter()
        .map(|entry| (entry.path, entry.id))
        .chain(ALIASES.iter().copied())
        .filter(|(prefix, _)| covers(path, prefix))
        .max_by_key(|(prefix, _)| prefix.len())?
        .1;
    PAGES.iter().find(|entry| entry.id == id)
}

/// Not staff (`role` passed as 0), not under `/manage`, an address not in the catalog, or the current entry invisible
/// to this role → `None`, and `layout/manage.html` falls back to a single column. `role` is the `adminid` of active
/// staff.
pub(in crate::router) fn frame(path: &str, role: i64) -> Option<ManageFrame> {
    // Check the path first: moderators also go through meta() on every front-end page, and this is cheaper than
    // scanning the catalog.
    if !(1..=5).contains(&role) || !under(path, "/manage") {
        return None;
    }
    let current = entry_for_path(path)?;
    // When role and access control disagree, fall back to a single column rather than render navigation that 403s
    // when clicked.
    if !visible(current, role) {
        return None;
    }
    let active = module(current.module)?;
    let modules = MODULES
        .iter()
        .filter_map(|m| {
            let landing = module_pages(m.id).find(|e| visible(e, role))?;
            Some(ModuleLink {
                title: m.title(),
                icon: m.icon,
                href: landing.path,
                current: m.id == active.id,
            })
        })
        .collect();
    let tabs = module_pages(active.id)
        .filter(|e| visible(e, role))
        .map(|e| TabLink {
            title: e.title(),
            page: e.id,
            href: e.path,
            current: e.id == current.id,
        })
        .collect();
    Some(ManageFrame {
        module_title: active.title(),
        modules,
        tabs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// When the sprite lacks a symbol, `<use>` silently draws nothing, and there is no other automatic safeguard.
    const ICONS: &str = include_str!("../../../templates/partials/manage_icons.html");

    #[test]
    fn catalog_is_internally_consistent() {
        let modules: HashSet<_> = MODULES.iter().map(|m| m.id).collect();
        assert_eq!(modules.len(), MODULES.len(), "module ids must be unique");
        let ids: HashSet<_> = PAGES.iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), PAGES.len(), "tab ids must be unique");
        let paths: HashSet<_> = PAGES.iter().map(|e| e.path).collect();
        assert_eq!(paths.len(), PAGES.len(), "tab paths must be unique");

        for entry in PAGES {
            assert!(
                modules.contains(entry.module),
                "the module of {} does not exist",
                entry.id
            );
            assert!(
                entry.path == "/manage" || entry.path.starts_with("/manage/"),
                "{} is not under /manage",
                entry.id
            );
        }
        for m in MODULES {
            assert!(
                module_pages(m.id).next().is_some(),
                "{} has no tabs, so its sidebar link would point nowhere",
                m.id
            );
            assert!(
                ICONS.contains(&format!("id=\"manage-icon-{}\"", m.icon)),
                "{} has the icon {}, which is not in the sprite",
                m.id,
                m.icon
            );
        }
        for (prefix, target) in ALIASES {
            assert!(
                ids.contains(target),
                "alias {prefix} points to the missing {target}"
            );
            assert!(
                prefix.starts_with("/manage/"),
                "alias {prefix} is not under /manage"
            );
            assert!(
                !paths.contains(prefix),
                "alias {prefix} repeats a tab path and is redundant"
            );
        }
    }

    #[test]
    fn path_lookup_folds_details_and_aliases_onto_their_tab() {
        let cases = [
            ("/manage", Some("overview")),
            ("/manage/users/9902", Some("users")),
            ("/manage/content/1234", Some("content")),
            ("/manage/reports/7", Some("reports")),
            ("/manage/appeals/7", Some("appeals")),
            ("/manage/restorations/7", Some("restorations")),
            ("/manage/tasks/9", Some("tasks")),
            ("/manage/policies/privacy", Some("policy-privacy")),
            ("/manage/mail", Some("mail")),
            // The alias is longer than /manage/mail, so all eleven kinds land on mail templates.
            ("/manage/mail/templates/content", Some("mail-templates")),
            (
                "/manage/mail/templates/email_verification",
                Some("mail-templates"),
            ),
            ("/manage/threads/55/organization", Some("content")),
            // Compared by segment: must not be swallowed by a sibling address such as /manage/content.
            ("/manage/credits", Some("credits")),
            // The admin root only matches itself: an unregistered admin page falls back to a single column instead of
            // silently highlighting as the overview.
            ("/manage/nothing", None),
            ("/my/reports/7", None),
            ("/", None),
        ];
        for (path, expected) in cases {
            assert_eq!(
                entry_for_path(path).map(|e| e.id),
                expected,
                "{path} lands on the wrong tab"
            );
        }
    }

    #[test]
    fn nine_mail_kinds_all_land_on_the_template_tab() {
        for (kind, _) in crate::mail::templates::KINDS {
            let path = format!("/manage/mail/templates/{kind}");
            assert_eq!(
                entry_for_path(&path).map(|e| e.id),
                Some("mail-templates"),
                "{path} lands on the wrong tab"
            );
        }
    }

    #[test]
    fn frame_narrows_to_the_current_role() {
        let admin = frame("/manage", 1).expect("an administrator sees the workbench");
        assert_eq!(admin.modules.len(), MODULES.len());
        assert_eq!(admin.module_title, "Workbench");

        // Moderators and trainee moderators only have "Workbench", "Review", "Forums" and "Tools & maintenance", one
        // tab each.
        let staff = frame("/manage", 3).expect("a moderator sees the workbench");
        assert_eq!(staff.modules.len(), 4);
        assert_eq!(staff.tabs.len(), 1);
        for module in &staff.modules {
            let entry = frame(module.href, 3).expect("the landing tab is visible to a moderator");
            assert_eq!(entry.tabs.iter().filter(|t| t.current).count(), 1);
        }
        let intern = frame("/manage", 4).expect("a trainee moderator sees the workbench");
        assert_eq!(
            intern.modules.iter().map(|m| m.href).collect::<Vec<_>>(),
            staff.modules.iter().map(|m| m.href).collect::<Vec<_>>()
        );

        // Global moderators have "Users" on top of what moderators have, containing only account sanctions (mute
        // only).
        let global = frame("/manage", 2).expect("a global moderator sees the workbench");
        assert_eq!(global.modules.len(), 5);
        let users =
            frame("/manage/sanctions", 2).expect("a global moderator reaches account sanctions");
        assert_eq!(users.tabs.len(), 1);
        assert!(frame("/manage/users", 2).is_none());

        // Reviewers only have the overview, report review and content (only reported items); restoration requests and
        // the operation log are not given.
        let reviewer = frame("/manage", 5).expect("a reviewer sees the workbench");
        assert_eq!(reviewer.modules.len(), 3);
        let review = frame("/manage/reports", 5).expect("a reviewer reaches report review");
        assert_eq!(review.tabs.len(), 1);
        assert!(frame("/manage/restorations", 5).is_none());
        assert!(frame("/manage/content/9", 5).is_some());
        assert!(frame("/manage/logs", 5).is_none());

        // The current state is always one place on the left and one tab, so the template never renders two
        // aria-current.
        for f in [&admin, &staff, &reviewer] {
            assert_eq!(f.modules.iter().filter(|m| m.current).count(), 1);
            assert_eq!(f.tabs.iter().filter(|t| t.current).count(), 1);
        }

        // Each module lands on its first tab visible to that role.
        for (link, m) in admin.modules.iter().zip(MODULES) {
            assert_eq!(link.href, module_pages(m.id).next().unwrap().path);
        }
    }

    #[test]
    fn frame_is_absent_outside_the_backend() {
        assert!(
            frame("/manage", 0).is_none(),
            "an ordinary member gets no admin frame"
        );
        assert!(
            frame("/my", 1).is_none(),
            "a front-end address gets no admin frame"
        );
        assert!(
            frame("/manage/nothing", 1).is_none(),
            "an unregistered address falls back to a single column"
        );
        // When a moderator reaches an administrator tab, fall back to a single column rather than render navigation
        // that 403s when clicked.
        assert!(frame("/manage/appeals", 3).is_none());
        assert!(frame("/manage/appeals", 1).is_some());
    }
}
