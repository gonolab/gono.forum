## Reports, appeals, restoration requests, sanctions and data requests on the member side.

# $id is the request number, $status its state such as Pending.
governance-restoration-heading = Restoration request #{ $id } · { $status }
# $state is how the content was handled when the request was made, $time when it was submitted.
governance-restoration-submitted = State when requested: { $state } · Submitted: { $time }
governance-current-state = Current state: { $state }
governance-request-reason = Reason for the request
# $time is the date by which a decision is expected.
governance-due = Expected decision by: { $time }
governance-overdue-waiting = Overdue, still waiting for a moderator
governance-resolution = Decision
governance-restorations-back = Back to restoration requests
governance-restore-thread = Request to restore a thread
governance-restore-reply = Request to restore a reply
# The rules under the restoration form: $cooldown and $due are days, $daily the most requests per member in 24 hours.
governance-restore-rules =
    Explain why this content should be restored, in up to 1,000 characters; you cannot change it after submitting. Each piece of content can have only one pending request at a time, and after a rejection you cannot ask again for { $cooldown ->
        [one] { $cooldown } day
       *[other] { $cooldown } days
    }. Each member can submit up to { $daily } requests in 24 hours. Requests are reviewed by the forum's moderators, trainee moderators, global moderators or administrators; whoever hid the content stays out of the review. Expect a decision within { $due ->
        [one] { $due } day.
       *[other] { $due } days.
    }
governance-restore-submit = Submit request
governance-my-restorations = My restoration requests
governance-back-to-list = Back to the list
governance-restorations = Restoration requests
# $threads_html and $posts_html link to the member's threads and replies; $total is the number of requests.
governance-restorations-lead =
    When a moderator hides your content, or you withdrew it and the author's editing window has passed, you can request its restoration from { $threads_html } or { $posts_html }. Only moderators review these requests; they are separate from reports and sanction appeals. { $total ->
        [one] { $total } request.
       *[other] { $total } requests.
    }
governance-my-threads = My threads
governance-my-replies = My replies
# In a list of requests; $time is the date by which a decision is expected.
governance-due-short = expected by { $time }
governance-overdue = Overdue
governance-restorations-empty = No restoration requests yet.
governance-data-access = Verify your email for data requests
governance-data-access-lead = This entry is only for frozen members: once you verify your current email you can view and submit your own data requests, without logging in to the forum. Members who are not frozen should log in and use the “Data and account closure” page with their current password.
governance-data-access-passwordless = If your account has no password (for example, you signed up with a text message code, Google or GitHub), the email is only sent to an address you have verified.
governance-data-access-email = Current email
governance-data-access-send = Send verification email
governance-data-access-confirm-lead = Once you confirm, you can handle your own data requests for the rest of the email link's one hour. Sending a new email, changing your account credentials, lifting the freeze or the start of account closure ends it.
governance-data-access-confirm = Continue to my data requests
governance-data-back = Back to data and account closure
governance-report-view-target = View what is reported
governance-report-reason = Reason for the report
governance-report-rules = Describe the problem, in up to 1,000 characters. Only reviewers can see who reported it, and a pending report on the same target is not submitted twice.
governance-report-submit = Submit report
governance-my-reports = My reports
governance-report-attachments = Report an attachment separately
governance-report-status = Status
# Report states.
governance-report-pending = Pending review
governance-report-resolved = Resolved
governance-report-dismissed = Not upheld
governance-filter = Filter
# A report in a list; $target is what was reported, such as Thread.
governance-report-kind = { $target } report
governance-reports-empty = No matching reports.
governance-appeals = Account appeals
governance-appeals-confirm-lead = Once you confirm, you can view your sanctions, submit appeals and check their outcome. This verification is valid for one hour.
governance-appeals-confirm = Continue to appeals
governance-appeals-lead = You can appeal even while your account is frozen. Enter your current email and we will send you a verification link.
governance-appeals-email = Account email
governance-appeals-send = Send appeal verification email
# $window and $handling are days.
governance-appeals-rules =
    Each sanction can be appealed once within { $window ->
        [one] { $window } day
       *[other] { $window } days
    } of taking effect. An appeal is accepted as soon as it is submitted and should be decided within { $handling ->
        [one] { $handling } day.
       *[other] { $handling } days.
    } Sending a new email cancels the earlier appeal link and access.
governance-back-to-login = Back to login
governance-my-appeals = My appeals
governance-appeals-exit = Leave appeal access
# $window and $handling are days.
governance-appeals-window =
    Each sanction can be appealed once within { $window ->
        [one] { $window } day
       *[other] { $window } days
    } of taking effect. An appeal is accepted as soon as it is submitted and should be decided within { $handling ->
        [one] { $handling } day.
       *[other] { $handling } days.
    }
governance-account-uid = Account UID { $uid }
# When a sanction took effect and when it ends.
governance-sanction-period = In effect: { $start } · { $end }
# $status is the state of the appeal, such as Pending.
governance-appeal-state = Appeal: { $status }
governance-appeal-submit = Submit appeal
governance-sanctions-empty = No sanctions on record.
governance-my-sanctions = My account record
governance-sanction-ended = Ended: { $time }
governance-sanction-appeal = Appeal and outcome
governance-data = Data and account closure
# $uid is the account's number, $posts how many of its posts are not permanently erased, $balance its available points and $credit the site's name for them.
governance-data-summary =
    Account UID { $uid } has { $posts ->
        [one] { $posts } post
       *[other] { $posts } posts
    } not yet permanently erased and { $balance } { $credit } available.
governance-data-restricted = You are handling this account's data requests through email verification and are not logged in to the forum.
governance-data-exit = Leave data access
governance-data-request = Make a request
governance-data-export-note = The export ZIP contains your structured records, your original Markdown and the attachments that passed checks, and can be downloaded for 7 days. Quarantined attachments, attachments that failed checks and attachments already cleaned up are only listed with their status.
# $credit is the site's name for its points.
governance-data-cooling-note = Closing the account and permanently erasing content have a 7-day cooling-off period; you can cancel explicitly before they start, and logging in does not cancel them. First settle your funded activities, pending rewards and frozen { $credit }. During the cooling-off period you can use the forum as usual, but cannot start new funded activities or pending rewards.
# $credit is the site's name for its points, $days how long the username stays reserved.
governance-data-close-note =
    By default, closing the account keeps your posts and removes your account details and byline; you can also choose to erase your post text. Closure handles all your content as it is at that time: the remaining { $credit } are given up in one ledger entry, and your email, password, bio and avatar are cleared; your username stays reserved for { $days ->
        [one] { $days } day
       *[other] { $days } days
    } so no one else can take it. Information you wrote into your posts yourself is not removed by anonymization.
governance-data-erase-note = Erasing on its own keeps the account and only erases the posts you had when you asked, with their revision history; edits to those posts during the cooling-off period are erased too, and posts made after the request are kept. Permanent erasure keeps the floors and other people's replies and cannot be undone. Necessary moderation, appeal and ledger history is kept only for staff with permission; other people's own text is never rewritten.
governance-data-kind = Request type
governance-data-kind-export = Export my data
governance-data-kind-erase = Keep the account, permanently erase my existing content
governance-data-kind-close = Close my account
governance-data-mode = Content on closure
governance-data-mode-retain = Keep my posts anonymously
governance-data-mode-erase = Permanently erase my post text and revisions
governance-data-admin = Administrator accounts cannot close themselves.
governance-data-password = Current password
governance-data-confirm = I have read what is handled, the cooling-off period and that it cannot be undone.
governance-data-submit = Submit data request
governance-data-history = Request history
# $time is the earliest moment the request is carried out.
governance-data-due = Earliest start: { $time }
# Shown for a request whose processing failed for now (the only error the processing records).
governance-data-failed = Processing failed for now and will be retried automatically; administrators can see the task status.
governance-data-download = Download my ZIP
governance-data-cancel = Cancel this request
governance-data-empty = No data requests yet.
# A reason left empty, too long or with control characters (reports, appeals, restoration requests).
governance-error-reason = Enter a valid reason within the length shown on the page
# When the site only requires a verified email to take part.
governance-error-verify-email = Verify your email before doing this
# What is reported.
governance-target-thread = Thread
governance-target-reply = Reply
governance-target-profile = Profile
governance-target-attachment = Attachment
governance-target-content = Content
# Headings of the report form.
governance-report-thread = Report a thread
governance-report-reply = Report a reply
governance-report-profile = Report a profile
governance-report-attachment = Report an attachment
governance-report-content = Report content
governance-error-report-target = Invalid report target
# The avatar listed among the attachments of a reported profile.
governance-report-current-avatar = Current avatar
governance-error-report-key = Invalid report request, refresh the page
governance-error-report-changed = The same request cannot change what is reported
governance-error-report-quota = You have reached the limit of reports for 24 hours; please wait for your pending reports to be handled
governance-error-report-own = You cannot report your own content; edit or withdraw it instead
governance-error-report-filter = Invalid report filter
# States of appeals and restoration requests.
governance-status-accepted = Accepted
governance-status-rejected = Not accepted
governance-status-pending = Pending
governance-error-page = Invalid page
governance-error-appeal-key = Invalid appeal request, refresh the page and try again
governance-error-appeal-exists = This sanction has already been appealed; check the progress of the appeal
governance-error-appeal-window = The appeal period for this sanction has passed
governance-error-appeal-key-used = The same appeal request cannot be used for another sanction
# How content was handled when a restoration was requested.
governance-state-hidden = Hidden by moderators
governance-state-withdrawn = Withdrawn
governance-state-both = Hidden and withdrawn
governance-state-unknown = Unknown state
governance-status-unknown = Unknown
# Why a restoration cannot be requested.
governance-restore-self = The withdrawal is still within the author's window; restore it yourself on the thread page, no request needed
governance-restore-public = This content is not hidden or withdrawn, so there is nothing to restore
governance-restore-inherited = This reply is out of sight because its thread or parent floor is hidden; the reply itself was not acted on, so it cannot be requested on its own
governance-restore-erased = The content was permanently erased and cannot be restored
governance-restore-reserved = The content is in a reserved state, so a restoration cannot be requested
# The current state of content in a restoration request.
governance-target-erased = Permanently erased
governance-target-hidden-withdrawn = Hidden, withdrawn
governance-target-hidden = Hidden
governance-target-withdrawn = Withdrawn
governance-target-inherited = Hidden with its thread or parent floor
governance-error-restoration-key = Invalid restoration request, refresh the page
governance-error-restoration-changed = The same request cannot change what is asked
governance-error-restoration-cooldown = The last restoration request for this content was not accepted; you cannot ask again during the cooldown
governance-error-restoration-quota = You have reached the limit of restoration requests for 24 hours; please wait for your pending requests to be handled
governance-error-page-number = Invalid page number
# Sanction states.
governance-sanction-active = In effect
governance-sanction-revoked = Revoked
governance-sanction-expired = Expired
governance-sanction-no-expiry = No automatic expiry
# The refusal of a write while muted.
governance-muted-forever = You are muted until a moderator lifts it; meanwhile you can still read, bookmark, export your data and appeal
# $until is a date and time in the site time zone.
governance-muted-until = You are muted until { $until }; meanwhile you can still read, bookmark, export your data and appeal
# Sanction kinds.
governance-sanction-warning = Warning
governance-sanction-mute = Mute
governance-sanction-freeze = Account freeze
governance-error-sanction-page = Invalid page of sanctions
# Sanction notifications in the notification list; $reason is what the operator wrote, $until when it ends.
governance-notice-warning = Your account received a warning: { $reason }
governance-notice-mute = Your account was muted: { $reason }; until { $until }
governance-notice-mute-forever = Your account was muted: { $reason }; until an administrator lifts it
governance-notice-freeze = Your account was frozen: { $reason }; until { $until }
governance-notice-freeze-forever = Your account was frozen: { $reason }; until an administrator lifts it
governance-notice-freeze-revoked = Your account freeze was revoked: { $reason }
governance-notice-mute-revoked = Your mute was revoked: { $reason }
governance-notice-warning-revoked = Your warning was revoked: { $reason }
# A sanction that ended by itself; $reason is one of the two sentences below.
governance-notice-mute-expired = Your mute has expired. { $reason }
governance-notice-freeze-expired = Your account freeze has expired. { $reason }
# Also kept in the notification row (in the site language) when the sanction ends by itself.
governance-notice-mute-over = You can post as usual again.
governance-notice-freeze-over = Please log in again.
# The reason of a sanction ended by an accepted appeal; also kept in the notification row.
governance-notice-appeal-ended = Appeal accepted; see the appeal record for the decision.
governance-notice-appeal-accepted = Your appeal was accepted: { $reason }
governance-notice-appeal-rejected = Your appeal was not accepted: { $reason }
# Refusal at login while frozen; $reason is the reason of the freeze, $until when it ends.
governance-frozen-forever = Your account is frozen until an administrator lifts it. Reason: { $reason }
governance-frozen-until = Your account is frozen until { $until }. Reason: { $reason }
# Links in the member's own content list to the restoration request of hidden or withdrawn content.
governance-restore-pending-link = Restoration request pending
governance-restore-rejected-link = Restoration request not accepted
governance-restore-link = Request restoration
governance-restore-self-link = Restore it yourself on the post
governance-restore-accepted-link = Restoration request accepted
# Data request kinds in the request history.
governance-data-label-export = Data export
governance-data-label-erase = Permanent erasure of my content
# Data request states.
governance-data-status-cooling = Cooling-off period, can be cancelled
governance-data-status-pending = Waiting to run
governance-data-status-processing = Processing
governance-data-status-ready = Ready to download
governance-data-status-completed = Completed
governance-data-status-cancelled = Cancelled
governance-data-status-expired = Export expired
governance-data-status-failed = Failed, waiting to retry
# $credit is the site's name for its points.
governance-error-unsettled = First finish your funded activities, pending draws or bounty rewards, and settle your frozen { $credit }
governance-error-data-request = Invalid data request
governance-error-wrong-password = The current password is incorrect
governance-error-data-key-used = This request was already used for something else
governance-error-data-erasing = Your data is being permanently erased; you cannot make a new request for now
governance-error-data-active = A data request of the same kind is already in progress; view or cancel it first
governance-error-data-started = Data processing that has already started cannot be cancelled
governance-error-export-unfinished = The export archive was not finished
governance-data-description = Export and permanent deletion of your own data
governance-error-data-confirm = Please confirm that you have read what is handled and from when it can no longer be cancelled.
governance-error-export-missing = The export file is not available; please request a new export
governance-data-access-description = While frozen, verify your current email to handle your own data requests
governance-data-access-confirm-note = Confirm to continue to your data requests; verifying does not log you in to the forum.
governance-data-access-invalid = This link is invalid, already used or expired; please request a new one.
governance-data-access-sent = If this email belongs to an account that can make data requests, we will send a verification email.
governance-data-access-expired = Your access has expired or the account's state has changed; please verify again.
governance-appeals-description = Access your sanctions and appeals through your account email
governance-appeals-sent = If this email belongs to an account with sanctions, we will send an appeal verification email. Please check your inbox and spam folder.
governance-appeals-list-description = Sanctions and appeals
governance-appeals-expired = Your appeal access has expired or the account's state has changed; please verify your email again.
governance-appeals-too-many = Too many requests, please try again later.
governance-appeals-invalid = This appeal link is invalid, already used or expired; please request a new one.
governance-appeal-title = Sanction appeal
governance-appeal-description = Appeal reason, progress and outcome
governance-report-description = Submit a report for review
governance-reports-description = Reports you submitted and how they were handled
governance-report-number = Report #{ $id }
governance-report-detail-description = Review and outcome of the report
governance-sanctions-description = Warnings, mutes, freezes and revocations
governance-restorations-description = Restoration requests for your content and their outcome
governance-restore-title = Request restoration of content
governance-restore-description = Ask moderators to restore your content that was hidden or withdrawn
governance-restoration-number = Restoration request #{ $id }
governance-restoration-description = Restoration request and outcome
# Placeholders written by permanent erasure, in the site language.
governance-erased-attachment = Deleted attachment
governance-erased-text = Deleted
governance-erased-subject = Content deleted
# A poll option after erasure; $number is the option's number.
governance-erased-option = Option { $number }
# The ledger entry giving up the remaining points on account closure; $credit is the site's name for its points.
governance-close-ledger = Remaining { $credit } given up on account closure
# README.txt of a personal data export; $site is the site's name.
governance-export-readme = Personal data export from { $site }. The JSON files are structured records, markdown holds your original text, and attachments are named by AID, matching the file names and types in attachments.json. Quarantined attachments, attachments that failed checks and cleaned up attachments are only listed with their status, without the original files; if there is an attachments-missing.json, the AIDs in it had been cleaned up or could not be found in storage at export time. Text and revisions that were permanently erased are not restored. This package contains no passwords, tokens, internal logs or the identities of other people's reporters.
# Report results and restoration request results in the notification list; $reason is what the reviewer wrote.
governance-notice-report-resolved = Your report was handled: { $reason }
governance-notice-report-dismissed = Your report was not upheld: { $reason }
governance-notice-restoration-accepted = Your restoration request was accepted: { $reason }
governance-notice-restoration-rejected = Your restoration request was not accepted: { $reason }
# Admin area: reports, appeals, restoration requests, sanctions and data requests.
governance-manage-restorations-description = Queue of members' content restoration requests
# $days is the number of days within which requests are handled.
governance-manage-restorations-lead =
    Requests from members about their own content that was hidden, or withdrawn after the editing window. Shown by forum permissions; pending ones in the order they came in. Expect them to be handled within { $days ->
        [one] { $days } day.
       *[other] { $days } days.
    }
# $id is the request number, $state how the content was handled when it was requested, $status the request's state.
governance-manage-restoration-row = Restoration request #{ $id } · { $state } · { $status }
# $uid is the member, $pid the post.
governance-manage-restoration-target = Member UID { $uid } · post #{ $pid }
governance-manage-submitted = Submitted: { $time }
# $time is the date by which the request should be handled.
governance-manage-due = Due: { $time }
governance-manage-restorations-empty = No restoration requests in this state.
governance-manage-restoration-description = Review a restoration request
governance-manage-member-uid = Member UID { $uid }
governance-manage-current-content = Current content
governance-manage-view-in-content = View in content management
governance-manage-restoration-gone = The content no longer exists; the request is kept.
governance-manage-restoration-recuse = You made this request or hid the content. Please leave it to another moderator or an administrator.
governance-manage-result = Outcome
governance-manage-restoration-accept = Accept and restore the content
governance-manage-restoration-reject = Reject and leave it as it is
# $days is the cooldown after a rejection.
governance-manage-restoration-rules =
    Accepting lifts the moderation hiding and the withdrawal at once; the conclusion goes into the action log as the reason and the author is notified. If the content was already restored, only the conclusion is recorded. A rejection also notifies the author, who cannot request this content again for { $days ->
        [one] { $days } day.
       *[other] { $days } days.
    } Keep other people's private information out of the conclusion.
governance-manage-restoration-submit = Confirm and notify the author
governance-manage-appeals = Appeals
governance-manage-appeals-description = Review and handle members' appeals
# $days is the number of days within which appeals get an answer.
governance-manage-appeals-lead =
    Handled in the order they came in; an appeal is accepted for review on submission. Expect an answer within { $days ->
        [one] { $days } day.
       *[other] { $days } days.
    }
# $id is the appeal number, $status its state.
governance-manage-appeal-row = Appeal #{ $id } · { $status }
# $uid is the member, $sanction the sanction appealed.
governance-manage-appeal-target = Member UID { $uid } · sanction #{ $sanction }
governance-manage-appeals-empty = No appeals in this state.
# When an appeal or request was received for review.
governance-appeal-received = Received: { $time }
governance-appeal-back-records = Back to account records
governance-appeal-back-list = Back to appeals
governance-manage-appeal-sanctions-link = All sanctions and the current account state
governance-appeal-overdue = Overdue, waiting for an administrator
governance-manage-appeal-accept = Accept and lift this sanction
governance-manage-appeal-reject = Reject and keep this sanction
governance-manage-appeal-rules = The member gets the conclusion by email and site notification. Accepting lifts only this sanction; other sanctions stay in effect.
governance-manage-appeal-submit = Confirm and notify the member
governance-appeal-result-note = The decision is sent to your current email address, and you can also see it on this page.
governance-appeal-handled = Handled: { $time }
# $link_html links to the login page.
governance-appeal-login-again = If all freezes have been lifted, please { $link_html }. Sessions on old devices are not restored.
governance-appeal-login-link = log in again
governance-appeal-reason = Reason for the appeal
# $deadline is the last moment an appeal can be submitted.
governance-appeal-rules = Explain the facts that should be reviewed. Each sanction can be appealed only once, and the appeal cannot be changed after submitting. Appeal deadline: { $deadline }.
governance-appeal-none = This sanction has no appeal. Appeal deadline: { $deadline }.
# $id is the report number, $status its state.
governance-report-heading = Report #{ $id } · { $status }
governance-report-current-target = Current target
governance-report-view-current = View the current content
governance-manage-report-moderate = Review and manage the content
governance-manage-report-sanction-author = Handle the author's account
governance-report-target-gone = The reported target is not publicly available right now; the report and its handling are kept.
governance-manage-report-recuse = This report is about your own content. Please leave it to another moderator or an administrator.
governance-manage-report-result = Review result
governance-manage-report-choose = Choose a result
governance-manage-report-resolution = Explanation for the reporter
governance-manage-report-rules = To hide or restore content or to sanction an account, use the management links above first, then write this conclusion. The reporter is notified of it, so leave out other people's private information.
governance-manage-report-submit = Confirm and notify the reporter
governance-report-back = Back to reports
governance-manage-reports = Report review
governance-manage-reports-lead = Reports are shown by your moderation permissions. Profile and attachment reports are handled by administrators and reviewers; thread and reply reports by forum permissions, and reviewers can handle them site-wide.
governance-manage-data = Data requests
governance-manage-data-description = Data request progress and retrying failures
governance-manage-data-lead = Administrators can follow progress and retry failed tasks. Export archives can only be downloaded by the member who asked; permanent erasure cannot be undone once it starts.
governance-manage-data-retry-reason = Reason for retrying
governance-manage-data-retry = Retry this request
governance-manage-sanctions = Account sanctions
governance-manage-member-uid-label = Member UID
governance-manage-sanctions-view = View account
governance-manage-frozen = Account is frozen
governance-manage-not-frozen = Account is not frozen
governance-manage-not-muted = Not muted
governance-manage-muted-forever = Muted until revoked
governance-manage-muted-until = Muted until { $time }
governance-manage-sanction-kind = Sanction
governance-manage-sanction-hours = Mute or freeze length (hours)
# Help under the length field, for administrators (who can also warn and freeze) and for global moderators.
governance-manage-hours-help-admin = Enter 0 for a warning. A mute or freeze of 1–8760 hours ends by itself; 0 keeps it in effect until it is revoked.
governance-manage-hours-help = A mute of 1–8760 hours ends by itself; 0 keeps it in effect until it is revoked.
governance-manage-sanction-reason = Reason
# The rules under the sanction form, for global moderators and for administrators (with the freeze sentence).
governance-manage-sanction-rules = The reason is shown to the member. While muted, a member can still log in, read, favorite, export data and appeal, but cannot post, reply, edit, send messages, like, tip, join activities or change the avatar, bio and username.
governance-manage-sanction-rules-admin = The reason is shown to the member. While muted, a member can still log in, read, favorite, export data and appeal, but cannot post, reply, edit, send messages, like, tip, join activities or change the avatar, bio and username. Freezing revokes existing sessions and account verification links at once; after it is lifted the member must log in again.
governance-manage-sanction-submit = Confirm and notify the member
governance-manage-revoke-reason = Reason for revoking
governance-manage-sanction-revoke = Revoke and notify the member
governance-manage-error-not-staff = This account has no moderation permissions
governance-manage-error-sanction-staff = Only administrators and global moderators can manage account sanctions
governance-manage-error-sanction-admin = Only administrators can manage account sanctions
governance-manage-error-appeal-filter = Invalid filter or page
governance-manage-error-appeal-admin = Only administrators can handle appeals
governance-manage-error-appeal-result = Choose how to handle the appeal
governance-manage-error-appeal-handled = An administrator already handled this appeal. Please refresh to see the result
governance-manage-error-appeal-time = Invalid handling time
governance-manage-error-choose-result = Choose a result
governance-manage-error-report-moved = The thread was moved. Please refresh and review again
governance-manage-error-report-relocated = The content was moved. Please refresh and review again
governance-manage-error-report-own = This report is about your own content. Please leave it to another moderator or an administrator
governance-manage-error-report-forum = You cannot review reports in this forum
governance-manage-error-report-profile = Only administrators and reviewers can review account and attachment reports
governance-manage-error-report-own-profile = This report is about your own profile or attachment. Please leave it to an administrator
governance-manage-error-report-handled = This report was already handled. Please see the latest result
governance-manage-error-report-time = The handling time cannot be before the report time
governance-manage-error-restoration-filter = Invalid request filter
governance-manage-error-restoration-handled = This request was already handled. Please see the latest result
governance-manage-error-restoration-recuse = You made this request or hid the content. Please leave it to another moderator or an administrator
governance-manage-error-restoration-time = The handling time cannot be before the request time
governance-manage-error-restoration-moved = The thread was moved. Please refresh and handle it again
governance-manage-error-sanction-input = Invalid member, sanction, length or request ID
governance-manage-error-gmod-mute-only = Global moderators can only mute; warnings and freezes are for administrators
governance-manage-error-sanction-replay = The same request cannot change the sanction
governance-manage-error-sanction-deleting = The account is being deleted; no new sanctions can be added
governance-manage-error-sanction-admin-target = Administrator accounts cannot be handled on the sanctions page
governance-manage-error-mute-gmod = Global moderators cannot be muted
governance-manage-error-sanction-stale = The account has changed. Please refresh and confirm again
governance-manage-error-already-frozen = The account is already frozen. Please handle the existing sanction first
governance-manage-error-already-muted = The account is already muted. Please revoke the existing mute first
governance-manage-error-sanction-term = Invalid sanction length
governance-manage-error-revoke-admin = Only administrators can revoke sanctions
governance-manage-error-gmod-revoke-mute-only = Global moderators can only revoke mutes; warnings and freezes are for administrators
governance-manage-error-revoke-time = Invalid revoking time
governance-manage-error-sanction-ended = The sanction has already ended. Please refresh to see it
governance-manage-error-retry-reason = Enter a reason for retrying of 1 to 500 characters
governance-manage-error-retry-failed-only = Only failed requests can be retried
governance-manage-error-retry-leased = The task still holds a valid run lease
