# Roles and permissions

Who can do what on a Gono Forum site: visitors, members, the five staff roles, account sanctions, growth levels,
verified members and honor groups. The member API applies the same checks to API tokens ([api.md](api.md), "Account
and site rules"); settings named here are stored and read as described in
[settings-and-caching.md](settings-and-caching.md).

## Groups at a glance

| Group | How a member gets there | Stored in |
| --- | --- | --- |
| Anonymous visitor | Not signed in | — |
| Member short of the participation requirement | Signed up; by default the email is not verified yet | `gonof_common_member.emailstatus`, `mobile`, `gonof_common_member_external` |
| Member | Meets the participation requirement | — |
| Growth level 1 to N | Growth points from their own posts | `gonof_common_growth_account.growth` |
| Staff | Setup wizard (administrator) or appointed by an administrator | `gonof_common_member.adminid` 1–5, `gonof_forum_moderator` |
| Warned, muted or frozen member | Sanctioned by staff | `gonof_common_member_sanction`, `gonof_common_member.mute_until`, `freeze` |
| Verified member | Granted by an administrator | `gonof_common_member_verification` |
| Honor group holder | Granted by an administrator | `gonof_common_honor`, `gonof_common_member_honor` |

Anonymous visitors read boards whose access is "Everyone" and threads whose visibility is public. Boards for
"Signed-in members" and members-only threads need an account; boards set to "Admin area only" never appear on the
front end. Visitors cannot post or interact.

## Participation requirement

Posting threads and replies, editing one's own posts, private messages, likes, downvotes, favorites, tips, poll votes,
lottery entries, the daily check-in, following threads, boards and members, uploads, reports, invite codes and API
tokens all need the participation requirement: `settings.login.participation` (Global settings › Login methods), where
any one of `email` (a verified email), `mobile` (a bound phone number), `google` or `github` (a linked account) is
enough. The default is `email`; with none ticked (an empty value) nothing is required, and every member whose account
is active and not frozen may take part. A row in `gonof_common_site_domain` with a non-NULL `participation` overrides
it on that domain, and an empty string there likewise requires nothing. Members who meet none of them can sign in and
read what members can read; a banner under the header says what is missing. Administrators are exempt.

An email counts as verified once a link mailed to it is used: the verification link, or a password reset link.
While an account's email is unverified, it cannot link a Google or GitHub account, add a phone number or turn on
two-step verification, so a member who signed up with a password verifies their email before meeting `mobile`,
`google` or `github` that way. Whoever signed up may not own the address, and its owner can take the account over
through password recovery: resetting the password of an account whose email is unverified verifies the email and
removes the account's linked accounts, phone number and two-step verification, on top of what every reset does
(all devices signed out, the API token and the feed URL revoked). Accounts without an email, signed up by text message
or through a provider that shared none, are not affected.

## Staff roles

`gonof_common_member.adminid` holds the role: 0 member, 1 administrator, 2 global moderator, 3 moderator, 4 trainee
moderator, 5 reviewer. Staff powers need an active, unfrozen account. All five roles can enter the admin area
(`/manage`); each page narrows further, and the navigation only shows the tabs a role can open.

| Action | Administrator | Global moderator | Moderator | Trainee moderator | Reviewer |
| --- | --- | --- | --- | --- | --- |
| Hide and unhide posts | Everywhere | Everywhere | Own boards | Own boards | Reported posts |
| Lock and unlock threads | Everywhere | Everywhere | Own boards | Own boards | No |
| Pin (levels 1–3) and feature threads | Everywhere | Everywhere | Own boards | No | No |
| Restore withdrawn posts | Everywhere | Everywhere | Own boards | No | No |
| Edit other members' posts | Everywhere | Everywhere | Own boards | No | No |
| Move threads | Everywhere | Everywhere | Between own boards | No | No |
| Thread and reply reports | All | All | Own boards | Own boards | All |
| Profile and attachment reports | Yes | No | No | No | Yes |
| Restoration requests | All | All | Own boards | Own boards, unhide only | No |
| Action log | All events | Content events | Content events in own boards | Content events in own boards | No |
| Mute and lift mutes; read sanction records | Yes | Yes | No | No | No |
| Warn and freeze, lift them; decide appeals | Yes | No | No | No | No |
| Settings, skins, layouts, boards, moderators, users, invites, verified members, honor groups, data requests | Yes | No | No | No | No |

"Own boards" is defined under "Moderators and boards". Every staff action on a post needs a reason of 1 to 500
characters and is written to the action log. Profile and attachment reports can only be marked handled or dismissed.

### Assigning roles

The first administrator is the account created in the setup wizard ([install.md](install.md), "Setup wizard").
Administrators assign the other roles on Forums › Moderators (`/manage/moderators`, linked from each row of Users):

- The page sets roles 0, 2, 3, 4 and 5. It cannot grant the administrator role or change an administrator.
- Moderators and trainee moderators need at least one board; the other roles have none, and changing to them removes
  the member's appointments.
- Only accounts with both a password and an email can be appointed.
- A reason of 1 to 500 characters is required; the change is logged as `identity.role.assign`.
- A change signs the member out on every device and cancels their unused appeal-access and data-access links.

## Moderators and boards

Moderators and trainee moderators have one row in `gonof_forum_moderator` per board they are appointed to. Boards have
two levels, and an appointment on a parent board also covers its child boards; these are the member's "own boards".
Administrators and global moderators manage every board. Reviewers manage none.

- Moving a thread needs management of both the source and the target board.
- The board page header lists the moderators of the board and of its parent, with trainee moderators marked
  "(trainee)". Members whose role is no longer moderator or trainee moderator are not listed.
- Management links on pages come from the moderator roster in the process cache and can linger for up to the cache
  lifetime after an appointment is removed ([settings-and-caching.md](settings-and-caching.md), "The process cache").
  Every management action re-checks the appointment rows under lock in its own transaction.
- Staff other than administrators recuse themselves: they cannot handle reports about their own posts or profile, or
  restoration requests they submitted or whose hide they made, and cannot edit or unhide their own posts while those
  are hidden by moderation.
- Withdrawing a post is the author's own action within the edit window; staff cannot withdraw for the author.

## Trainee moderators

A trainee moderator (role 4) is appointed to boards like a moderator. There they hide and unhide posts, lock and unlock
threads, handle thread and reply reports, and accept restoration requests that only need unhiding; a request that also
needs a withdrawal restored is refused and left to a moderator. They cannot edit other members' posts, move threads,
pin, feature or restore withdrawals (`content::edits::staff_action_allowed`, `staff_can_edit`). The thread page offers
them "Moderate" but no edit or move link, and the move page returns 403. Their admin tabs are those of a moderator:
Overview, Report review, Restoration requests, Content and Action log.

## Reviewers

A reviewer (role 5) works site-wide and only handles reports, including profile and attachment reports. On a reported
reply, or the first post of a reported thread, they can hide and unhide, whether or not the report is handled
(`moderation::reported_sql`); they cannot lock, edit, move, pin or restore. They do not handle restoration requests or
appeals and do not see the action log. Their admin tabs are Overview (pending report counts), Report review, and
Content, which lists only reported posts.

## Account sanctions

Sanctions are rows in `gonof_common_member_sanction` with `kind` `warning`, `mute` or `freeze`, written only in
`moderation/sanctions.rs` and managed on Users › Account sanctions (`/manage/sanctions`). Each needs a reason of 1 to
500 characters. Nobody can sanction themselves, and administrators cannot be sanctioned. Imposing, lifting and expiry
each notify and email the member, who sees their records at `/my/sanctions`.

| Kind | Imposed and lifted by | Duration | Effect |
| --- | --- | --- | --- |
| Warning | Administrators | None | A record, a notification and an email |
| Mute | Administrators, global moderators | Up to 8760 hours, or no end | See "Mutes" |
| Freeze | Administrators | Up to 8760 hours, or no end | See "Freezes" |

### Mutes

Global moderators can only choose mute on the sanctions page and can only lift mutes. Administrators and global
moderators cannot be muted; the other staff roles can. A member has at most one mute in effect; muting them again is
refused.

`gonof_common_member.mute_until` is a copy of the mutes in effect: 0 when none, the largest BIGINT for a mute with no
end, otherwise the latest end time. `moderation/sanctions.rs` recomputes it whenever a mute is imposed, lifted or
ends, and is the only code that writes it. Write paths read it with the member row they already lock, and a mute counts
while `mute_until` is later than now, so it lifts exactly at its end time; the background task then records the end
and notifies within seconds. A mute does not sign the member out.

A muted member cannot post threads or replies, edit any post (their own, or as staff someone else's), send private
messages, like, downvote, tip, vote in polls, enter lotteries, upload images, set a new avatar, or change their
username or bio; the refusal states when the mute ends. They can still sign in, read, favorite, follow, report, check
in, withdraw and restore their own posts, request restoration, remove their avatar, create invite codes and API tokens,
export their data and appeal. Muted staff keep their moderation actions other than editing. Every page shows a banner
with the end time and a link to the records and appeal.

### Freezes

A freeze sets `gonof_common_member.freeze`, bumps the credentials version (ending every session) and revokes the
member's unused email links. A frozen member cannot sign in; the login page shows the reason and the end time. Global
moderators can be frozen; an account that is already frozen cannot be frozen again. The profile shows the account as
frozen, and verified and honor badges are hidden. A frozen member can still appeal at `/appeals` and handle data
requests at `/data/access`, both through a link sent to the account email. The data request link is the whole check:
requests made through it do not ask for a password. An account without a password (signed up with a text message code,
Google or GitHub) gets it only at a verified email, since an address never confirmed may be mistyped or someone
else's. A timed freeze is ended by the background task within seconds of its end time.

### Appeals

Each sanction can be appealed once, within `settings.governance.appeal_window_days` (default 30) of taking effect,
computed from the current setting. Signed-in members appeal from `/my/sanctions`. Frozen members enter their email at
`/appeals` and get a link valid for one hour, exchanged for a short-lived cookie; asking again cancels the old link.
The appeal text is up to 1000 characters. Only administrators decide appeals, on Review › Appeals (`/manage/appeals`),
by accepting or rejecting with a written conclusion; accepting ends the sanction if it is still in effect.
`settings.governance.appeal_handling_days` (default 7) only sets the due date shown and marks overdue appeals. Both
settings are on Security › Anti-spam & governance.

## Growth levels

Growth points are a reputation value separate from the site's points: they come only from a member's own posts and
cannot be spent or transferred. By default a thread gives 3, a reply or nested reply 1, each like received 1, a
featured thread 10 and a reply accepted for a bounty 5 (`settings.growth.*`, Global settings › Points). Hiding,
withdrawing or erasing a post takes its points back; unhiding or restoring gives them back. Administrators can adjust
a member's points with a reason. Levels come from `settings.growth.levels` (1 to 20 thresholds, default
`0,20,80,200,500,1200,3000,8000`) and `settings.growth.level_names` (empty shows "Level N" in the site language); they
are computed when read and shown as "Lv3 (Level 3)" on profiles and the leaderboard.

### Level gates

Three settings, each from 1 to the number of levels. The default 1 means no limit, and no query is made:

| Setting | Gates | Exempt |
| --- | --- | --- |
| `settings.growth.link_level` | Posting a link to another site in a thread, reply, edit or bio | Staff who manage the target board; for bios, administrators and global moderators |
| `settings.growth.invite_level` | Creating personal invite codes at `/my/invites` | All staff |
| `settings.growth.activity_level` | Starting a red packet or lottery thread | Staff who manage the target board |

- The link gate works with the new-member link block (`settings.posting.new_member_link_block` and
  `new_member_link_days`): a member must pass both, and the message names what is missing. Links to this site and its
  other domains do not count, and an edit is only refused for links it adds.
- Both look at every address a member puts in front of readers, in links, plain text and code alike: a thread's body
  and title, a reply, a news thread's source link and the member's bio. The source link is set when the thread is
  published and cannot be edited; editing the body compares only the body.
- The invite gate does not affect codes created in the admin area or invite code prefixes; the invite page shows the
  required and current level and disables the button.
- Joining other members' activities is not gated. Posting and upload quotas, private messages and friend link
  applications do not depend on level.

## Exemptions

| Rule | Exempt |
| --- | --- |
| Participation requirement | Administrators |
| Daily and first-day thread and reply quotas (`settings.posting.daily_*`, `first_day_*`) | Administrators |
| Posting intervals, hourly limit, duplicate check, new-member link block | Staff who manage the target board |
| Link and activity level gates | Staff who manage the target board |
| New-member link block and link level gate on bios | Administrators and global moderators |
| Invite level gate | All staff |

"Staff who manage the target board" are administrators and global moderators everywhere, and moderators and trainee
moderators in their own boards (`content::antispam::manages`). Elsewhere, and for reviewers, the member rules apply.

## Verified members

A verification marks a checked identity. A member has at most one (`gonof_common_member_verification`); only
administrators grant, edit, renew and revoke it, on Users › Verified members (`/manage/verifications`).

- A category, one of merchant, agent, blogger, webmaster or channel (shown as "Channel owner"); a title of 1 to 60
  characters; an optional website (http or https, up to 500 characters).
- An optional expiry: the form takes the last valid day, stored as midnight at the start of the next day in the site
  time zone. A new expiry must be in the future. The admin list filters all, expiring within 30 days, and expired.
- The badge shows on floors and on the profile page, which links to `/verified` and to the website
  (`rel="nofollow ugc"`). `/verified` groups members by category, 50 per group, with `?category=` for the rest; its
  explainer is edited on Users › Verification guidelines.
- Not shown while the member is frozen, the verification has expired, or the account is being deleted. It carries no
  permissions.
- Revoking needs a reason. Changes are logged as `identity.verification.*`. Data exports include the verification, and
  account deletion removes it.

## Honor groups

An honor group is community recognition: a badge with no permissions, granted only by administrators and never bought
with points. Groups are `gonof_common_honor`; grants are `gonof_common_member_honor` (primary key `(uid, honor_id)`,
with `granted_by`, `granted_at` and a nullable `expires_at`). The rules are in `honors.rs`.

- Admin pages, administrators only: Users › Honor groups (`/manage/honors`) lists and creates groups;
  `/manage/honors/{id}` edits a group and lists its holders; `/manage/honors/members/{uid}` grants, renews and revokes
  one member's groups and is linked from each row of Users.
- A group has a unique name of 1 to 10 characters, a description of 1 to 60, a sort value of 0 to 9999, and is enabled
  or disabled. Groups cannot be deleted. Saving refuses if someone changed the group since the form was read. A new
  site has no groups.
- A member can hold several groups, one grant per group. Expiry works as for verifications. A disabled group is not
  shown and cannot be newly granted; its grants are kept and show again when it is re-enabled. Frozen members can be
  granted a group; accounts being deleted cannot. Revoking needs a reason.
- Badges follow the verified badge on floors (first posts, replies and nested replies) and profile pages, in group
  order, with the description on hover. They show only while the group is enabled, the grant has not expired, the
  member is not frozen and the account is active.
- Grants, renewals and revocations are logged as `identity.honor.*` under the member; group changes as `site.honor.*`.
  Data exports include `honors.json` (group names and validity, not the granting administrator), and account deletion
  removes the member's grants.
