## Tools and maintenance in the admin area: the operation log, background tasks and version updates.

maintenance-previous = Previous
maintenance-next = Next
maintenance-logs-title = Operation log
# Meta description of the operation log page.
maintenance-logs-description = Records of content management and moderator appointments
maintenance-logs-kind = Type
maintenance-logs-kind-all = All
maintenance-logs-kind-content = Content
maintenance-logs-kind-identity = Accounts
maintenance-logs-kind-report = Reports
maintenance-logs-kind-sanction = Sanctions
maintenance-logs-kind-appeal = Appeals
maintenance-logs-kind-restore = Restoration requests
maintenance-logs-kind-organization = Forums
maintenance-logs-kind-invite = Invitations
maintenance-logs-kind-site = Site settings
maintenance-logs-kind-mail = Mail
maintenance-logs-kind-task = Background tasks
maintenance-logs-kind-privacy = Data requests
maintenance-logs-operator = Operator UID
maintenance-logs-target = Target member UID
maintenance-logs-from = From
maintenance-logs-until = Until
maintenance-logs-filter = Filter
# $total is the number of records, $page the current page.
maintenance-logs-summary =
    { $total ->
        [one] { $total } record, page { $page }. Only records within your management scope are shown.
       *[other] { $total } records, page { $page }. Only records within your management scope are shown.
    }
maintenance-logs-empty = No operations recorded yet.
maintenance-logs-pagination = Log pages
# An event the registry does not know.
maintenance-event-other = Management action
# The outcome of a logged operation.
maintenance-logs-result-unchanged = No change
maintenance-logs-result-resolved = Handled
maintenance-logs-result-accepted = Accepted
maintenance-logs-result-rejected = Not accepted
maintenance-logs-result-done = Done
# The target of a logged operation; $id is the record number.
maintenance-logs-target-appeal = Appeal #{ $id }
maintenance-logs-target-report = Report #{ $id }
maintenance-logs-target-sanction = Sanction #{ $id }
maintenance-logs-target-task = Task #{ $id }
maintenance-logs-target-data-request = Data request #{ $id }
# The operator of an operation done without a member, such as from the command line.
maintenance-logs-system = System
maintenance-logs-error-page = The log page number is not valid.
maintenance-logs-error-kind = The log type is not valid.
maintenance-logs-error-member = The member UID is not valid.
maintenance-logs-error-date = The log date is not valid.
# The labels of audit events on the operation log page.
maintenance-event-content-thread-publish = Thread posted
maintenance-event-content-reply-publish = Reply posted
maintenance-event-content-edit = Content edited
maintenance-event-content-withdraw = Withdrawn by author
maintenance-event-content-restore = Withdrawal restored
maintenance-event-content-hide = Content hidden
maintenance-event-content-unhide = Content unhidden
maintenance-event-content-lock = Thread locked
maintenance-event-content-unlock = Thread unlocked
maintenance-event-content-pin = Thread pinned
maintenance-event-content-unpin = Thread unpinned
maintenance-event-content-feature = Thread featured
maintenance-event-content-unfeature = Feature removed
maintenance-event-content-thread-move = Thread moved
maintenance-event-content-thread-tags = Thread tags changed
maintenance-event-identity-role-assign = Moderator appointment
maintenance-event-report-submit = Report submitted
maintenance-event-report-resolve = Report handled
maintenance-event-sanction-issue = Sanction issued
maintenance-event-sanction-revoke = Sanction revoked
maintenance-event-sanction-expire = Sanction expired
maintenance-event-appeal-submit = Appeal submitted
maintenance-event-appeal-resolve = Appeal handled
maintenance-event-restore-submit = Restoration request submitted
maintenance-event-restore-resolve = Restoration request handled
maintenance-event-organization-forum-save = Forum saved
maintenance-event-organization-tag-save = Tag saved
maintenance-event-organization-tag-merge = Tags merged
maintenance-event-organization-tag-alias-remove = Tag alias removed
maintenance-event-invite-batch-create = Invite codes generated
maintenance-event-invite-status-change = Invite code changed
maintenance-event-site-settings-save = Site settings saved
maintenance-event-site-document-save = Navigation or policy saved
maintenance-event-skin-import = Skin imported
maintenance-event-skin-select = Skin chosen
maintenance-event-skin-delete = Skin deleted
maintenance-event-skin-colors = Colors changed
maintenance-event-layout-save = Layout published
maintenance-event-site-branding-save = Brand images updated
maintenance-event-mail-settings-save = Mail settings saved
maintenance-event-mail-template-save = Mail template saved
maintenance-event-mail-test-request = Test email requested
maintenance-event-mail-test-result = Test email result
maintenance-event-task-retry = Background task retried
maintenance-event-privacy-request = Data request submitted
maintenance-event-privacy-cancel = Data request cancelled
maintenance-event-privacy-retry = Data request retried
maintenance-event-privacy-complete = Data request completed
maintenance-event-identity-profile-update = Profile updated
maintenance-event-identity-privacy-update = Privacy settings updated
maintenance-event-identity-email-change-request = Email change requested
maintenance-event-identity-email-change = Email changed
maintenance-event-api-key-create = API token created
maintenance-event-api-key-rotate = API token regenerated
maintenance-event-api-key-revoke = API token revoked
maintenance-event-identity-password-change = Password changed
maintenance-event-identity-password-set = Password set from the command line
maintenance-event-identity-password-request = Password reset requested
maintenance-event-identity-password-reset = Password reset
maintenance-event-identity-session-logout-all = Signed out of all devices
maintenance-event-activity-lottery-draw = Lottery drawn
maintenance-event-activity-lottery-exclude = Lottery entrant excluded
maintenance-event-activity-lottery-restore = Lottery entrant restored
maintenance-event-activity-lottery-codes = Lottery redemption codes replaced
maintenance-event-activity-lottery-correct = Lottery prize corrected
maintenance-event-credits-rule = Reward rule saved
maintenance-event-credits-tip = Tipping settings saved
maintenance-event-growth-adjust = Growth adjusted
maintenance-event-feed-token-create = Feed link created
maintenance-event-feed-token-rotate = Feed link regenerated
maintenance-event-feed-token-members = Feed link scope changed
maintenance-event-feed-token-disable = Feed link turned off
maintenance-event-feed-token-revoke = Feed link revoked
maintenance-event-identity-bootstrap-admin = First administrator created
maintenance-event-identity-register = Account registered
maintenance-event-identity-login = Signed in
maintenance-event-identity-email-verify = Email verified
maintenance-event-identity-email-resend = Verification email resent
maintenance-event-identity-invite-generate = Invite code created by member
maintenance-event-identity-invite-revoke = Invite code revoked by member
maintenance-event-identity-avatar-update = Avatar changed
maintenance-event-identity-custom-css = Personal style saved
maintenance-event-identity-reading-preferences = Reading preferences saved
maintenance-event-identity-notification-preferences = Notification preferences saved
maintenance-event-identity-external-link = External sign-in linked
maintenance-event-identity-external-unlink = External sign-in unlinked
maintenance-event-identity-mobile-bind = Phone number added
maintenance-event-identity-mobile-change = Phone number changed
maintenance-event-identity-mobile-unbind = Phone number removed
maintenance-event-identity-two-factor-enable = Two-factor authentication turned on
maintenance-event-identity-two-factor-disable = Two-factor authentication turned off
maintenance-event-identity-two-factor-recovery-regenerate = Recovery codes regenerated
maintenance-event-identity-two-factor-lockout = Two-factor sign-in locked
maintenance-event-identity-two-factor-reset = Two-factor authentication reset
maintenance-event-identity-verification-grant = Verification granted
maintenance-event-identity-verification-update = Verification edited
maintenance-event-identity-verification-renew = Verification renewed
maintenance-event-identity-verification-revoke = Verification revoked
maintenance-event-identity-honor-grant = Honor group granted
maintenance-event-identity-honor-renew = Honor group renewed
maintenance-event-identity-honor-revoke = Honor group revoked
maintenance-event-interaction-thread = Thread follow changed
maintenance-event-interaction-forum = Forum follow changed
maintenance-event-interaction-member = Member follow changed
maintenance-event-interaction-block = Member block changed
maintenance-event-interaction-forum-filter = Blocked forums changed
maintenance-event-mail-switch = Site mail turned on or off
maintenance-event-media-upload = Attachment uploaded
maintenance-event-media-cancel = Attachment processing cancelled
maintenance-event-media-trash = Attachment moved to the trash
maintenance-event-media-restore = Attachment restored from the trash
maintenance-event-media-purge-non-image = Non-image attachment queued for removal
maintenance-event-media-storage-save = Attachment storage saved
maintenance-event-organization-forum-order = Forum order changed
maintenance-event-organization-forum-delete = Forum deleted
maintenance-event-organization-forum-merge = Forum merge requested
maintenance-event-organization-forum-merge-done = Forum merge finished
maintenance-event-site-url-cli = Forum address set from the command line
maintenance-event-site-domains-switch = Multiple domains turned on or off
maintenance-event-site-domain-create = Domain added
maintenance-event-site-domain-save = Domain saved
maintenance-event-site-domain-delete = Domain deleted
maintenance-event-site-honor-create = Honor group created
maintenance-event-site-honor-update = Honor group edited
maintenance-event-site-links-add = Friend link added
maintenance-event-site-links-apply = Friend link application submitted
maintenance-event-site-links-withdraw = Friend link withdrawn by applicant
maintenance-event-site-links-approve = Friend link application approved
maintenance-event-site-links-reject = Friend link application rejected
maintenance-event-site-links-update = Friend link edited
maintenance-event-site-links-remove = Friend link taken down
maintenance-event-sms-site-daily-limit = Site-wide daily SMS limit reached
maintenance-tasks-title = Background tasks
# Meta description of the task page.
maintenance-tasks-description = Delivery and processing tasks
maintenance-tasks-kind-prefix = Task type prefix
maintenance-tasks-status = Status
maintenance-tasks-all = All
maintenance-tasks-status-pending = Waiting
maintenance-tasks-status-processing = Processing
maintenance-tasks-status-sent = Done
maintenance-tasks-status-failed = Stopped
maintenance-tasks-status-exhausted = Out of retries
maintenance-tasks-filter = Filter
# $total is the number of matching tasks.
maintenance-tasks-summary =
    { $total ->
        [one] { $total } task. Finished tasks, tasks whose credentials expired and tasks refused for security reasons cannot be delivered again.
       *[other] { $total } tasks. Finished tasks, tasks whose credentials expired and tasks refused for security reasons cannot be delivered again.
    }
# $count is how many times the task was tried.
maintenance-tasks-attempts =
    { $count ->
        [one] Tried { $count } time
       *[other] Tried { $count } times
    }
# $time is when the task is tried next.
maintenance-tasks-next = Next attempt { $time }
# $time is when the current worker's lease ends.
maintenance-tasks-lease = Lease until { $time }
maintenance-tasks-retry-reason = Reason for retrying
maintenance-tasks-retry = Schedule retry
maintenance-tasks-empty = No tasks match.
maintenance-tasks-pagination = Task pages
# Kinds of background tasks.
maintenance-tasks-kind-media-process = Attachment inspection and processing
maintenance-tasks-kind-media-purge = Attachment cleanup
maintenance-tasks-kind-notification = Reply, follow and mention notifications
maintenance-tasks-kind-email = Mail delivery
maintenance-tasks-kind-privacy = Data request processing
maintenance-tasks-kind-other = Background delivery task
# What a task is about, followed by its number.
maintenance-tasks-target-member = Member
maintenance-tasks-target-attachment = Attachment
maintenance-tasks-target-post = Post
maintenance-tasks-target-thread = Thread
maintenance-tasks-target-data-request = Data request
maintenance-tasks-target-forum = Forum
maintenance-tasks-target-other = Item
# Redacted summaries of a task's last error.
maintenance-tasks-error-exhausted = Automatic retries stopped; schedule a retry once the problem is fixed.
maintenance-tasks-error-gone = The target or credentials are no longer valid; this task will not be delivered.
maintenance-tasks-error-temporary = The service is temporarily unavailable; waiting to retry.
maintenance-tasks-error-status = The task status is not valid.
maintenance-tasks-error-stale = The task status has changed. Refresh and try again.
maintenance-tasks-error-not-retryable = The task is finished, no longer valid or running, so it cannot be retried.
maintenance-tasks-error-not-found = The task does not exist.
maintenance-updates-title = Version updates
# Meta description of the update page.
maintenance-updates-description = Check for new versions and upgrade in one click
maintenance-updates-started = The upgrade has started. Downloading and installing take a little while; the forum restarts by itself when done, and this page refreshes every few seconds.
# $step is the current upgrade step.
maintenance-updates-running = { $step }…
# $reason is why the upgrade stopped.
maintenance-updates-failure = The upgrade did not finish: { $reason }. The forum is still running the previous version.
# $version_html is the running version.
maintenance-updates-version = Current version: { $version_html }
# A detail in parentheses after the text before it.
maintenance-updates-parenthesized = {" "}({ $text })
# $url_html is the release source address, $key_html the configuration key.
maintenance-updates-source = Release source: { $url_html } (deployment configuration { $key_html }; GitHub Releases by default)
# $from and $to are versions.
maintenance-updates-last-switched = Last upgrade: switched from { $from } to { $to } with forum.install.sh rollback
# $from and $to are versions.
maintenance-updates-last-rolled-back = Last upgrade: from { $from } to { $to } failed because the new version did not start; switched back to { $from } automatically
# $from and $to are versions.
maintenance-updates-last-upgraded = Last upgrade: from { $from } to { $to }
# $from and $to are versions; $command_html is the command that finishes the upgrade.
maintenance-updates-last-incomplete = Last upgrade: from { $from } to { $to }, but gono-forum-cli does not report { $to }. Run { $command_html } on the server to put both programs in place.
maintenance-updates-check = Check for updates
maintenance-updates-never-checked = Not checked yet. A check reads the release source's checksums.txt once; it downloads no program and changes nothing.
# $time is when the check ran, $error why it failed.
maintenance-updates-check-failed = Check at { $time } failed: { $error }
# $time is when the check ran.
maintenance-updates-check-no-package = Check at { $time }: the release source has no package for this platform.
# $time is when the check ran, $version_html the newer version.
maintenance-updates-check-newer = Check at { $time }: version { $version_html } is available.
# $time is when the check ran, $version_html the newest version at the source.
maintenance-updates-check-latest = Check at { $time }: up to date (the newest version at the release source is { $version_html }).
# $version is the version to upgrade to.
maintenance-updates-apply = Upgrade to { $version }
maintenance-updates-apply-lead = The package is downloaded, its SHA-256 checked and the new program put in place; the forum then restarts by itself and is unreachable for a few seconds. If the new version fails to start twice, the current version comes back automatically.
maintenance-updates-apply-lead-previous = The package is downloaded, its SHA-256 checked and the new program put in place; the forum then restarts by itself and is unreachable for a few seconds. If the new version fails to start twice, the current version comes back automatically (the program of the version before it is replaced).
maintenance-updates-backup = If the new version changes the database structure, the old version may not understand the new database after switching back, and you have to restore from a backup. Back up the database before upgrading.
maintenance-updates-blocked = This instance cannot upgrade in one click:
# $command_html is the forum.install.sh command.
maintenance-updates-blocked-hint = Instances installed with forum.install.sh can run { $command_html } on the server.
# Why one-click upgrade is unavailable.
maintenance-updates-blocker-source = This program was built from source, not from a release package; update it with git and cargo
maintenance-updates-blocker-systemd = The program was not started by systemd, so nothing would start the new version after it exits (only instances installed with forum.install.sh can)
maintenance-updates-blocker-writable = The service account cannot write to the program directory, so the program files cannot be replaced
maintenance-updates-blocker-switch = An upgrade or rollback with forum.install.sh did not finish (the program directory still holds switch-backup/); run forum.install.sh upgrade or rollback on the server first
# Between the reasons when an upgrade is refused.
maintenance-updates-blocker-separator = {"; "}
maintenance-updates-error-platform = There are no release packages for this platform (only Linux x86_64 and arm64)
maintenance-updates-error-status-lock = The upgrade status is unavailable
maintenance-updates-error-running = An upgrade is already running
maintenance-updates-error-no-release = There is no newer version to upgrade to; check for updates first
maintenance-updates-error-changed = The update check has found another release since this page was shown (another check ran in the meantime), so nothing was installed. Look at the page again and confirm the upgrade there.
# Upgrade steps; $package is the package file, $version the new version.
maintenance-updates-downloading = Downloading { $package }
maintenance-updates-installing = SHA-256 checked, installing { $version }
maintenance-updates-restarting = { $version } installed, restarting
# Errors of checking and upgrading; $path is a file, $url an address.
maintenance-updates-error-read = cannot read { $path }
maintenance-updates-error-too-large = { $path } is larger than { $limit } bytes
maintenance-updates-error-url = the upgrade address is not valid
maintenance-updates-error-https = upgrades only use HTTPS: { $url }
maintenance-updates-error-no-location = { $url } redirected without giving an address
maintenance-updates-error-location = the redirect address is not valid
maintenance-updates-error-not-found = { $url } was not found (HTTP 404)
# $status is the HTTP status code.
maintenance-updates-error-status = { $url } answered HTTP { $status }
maintenance-updates-error-redirects = { $url } redirected too many times
maintenance-updates-error-checksums = reading checksums.txt failed
maintenance-updates-error-checksums-utf8 = checksums.txt is not UTF-8
maintenance-updates-error-download = downloading the release package failed
# $expected and $actual are SHA-256 digests.
maintenance-updates-error-digest = the SHA-256 of the package does not match checksums.txt (expected { $expected }, got { $actual }); the upgrade was abandoned
maintenance-updates-error-entry-size = a tar entry size is not valid
maintenance-updates-error-entry-large = an entry in the package is too large
maintenance-updates-error-incomplete = the package is incomplete
# $name is a program file name.
maintenance-updates-error-missing = the package has no { $name }
maintenance-updates-error-read-package = reading the package failed
maintenance-updates-error-write = cannot write { $path }
# $expected is the expected version line, $reported what the new program printed.
maintenance-updates-error-version = the new version does not run on this machine or reports another version (expected “{ $expected }”, got “{ $reported }”); the upgrade was abandoned
maintenance-updates-no-output = no output
maintenance-updates-error-state-dir = cannot create the upgrade state directory
maintenance-updates-error-replace = replacing the program files failed; the upgrade was abandoned
