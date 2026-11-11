## The posting editor: new threads, replies, edits, activity fields, attachments in the editor, and the errors of
## posting and editing.

# The sidebar card next to the posting page.
editor-sidebar-title = Share and discuss
editor-sidebar-markdown = Markdown is supported. Images can go into the post once they pass the check, and you can post once your email is verified.
editor-sidebar-limits = Titles take up to 255 characters and posts up to 60,000 bytes.
# An edit conflict: the version saved meanwhile, shown above the form.
editor-conflict-summary = View the saved version
editor-conflict-reopen = Reopen the saved version (copy the changes you want to keep first)
editor-replying = Replying to the selected post
editor-cancel-reply = Cancel
editor-quoting = Quoting the selected post
editor-quote-source = View source
editor-cancel-quote = Cancel quote
editor-title = Title
editor-body = Content
editor-forum = Forum
editor-choose-forum = Choose a forum
# After a forum's name in the forum list; the thread will be shown to members only there.
editor-forum-members-only = {" "}(members only)
editor-guidance = Posting guide
editor-guidance-hint = Choose a forum to see its posting guide
# When editing a thread whose forum decides who sees it; $note says why.
editor-audience-note = Who can see it: { $note }
editor-audience = Who can see it
editor-audience-forum = Same as the forum
editor-audience-members = Always members only
editor-audience-help = “Same as the forum” lets visitors read it in a public forum; “Always members only” keeps the title, body and replies visible to signed-in members only, even after a move to a public forum.
editor-moderation-reason = Reason for this moderator edit
editor-category = Thread type
# Shown for bounties, red packets and lotteries; $credit is the site's name for its points.
editor-activity-note = Bounties, red packets and lotteries are paid in { $credit }, and the budget is frozen when you publish (a lottery freezes only its { $credit } prizes). Lotteries are free to enter; amounts, prizes and rules cannot be changed after publishing.
# $level is the level name required to start red packets and lotteries.
editor-activity-level = Starting a red packet or lottery requires { $level }; staff are not limited in the forums they manage.
editor-budget = Total budget ({ $credit })
editor-shares = Red packet shares / lottery winners
editor-simple-lottery = When { $credit } are split evenly, the total budget must divide evenly by the number of winners.
editor-entry-mode = How to enter
editor-entry-reply = A valid reply enters automatically
editor-entry-click = One-click entry
editor-requirements = Entry requirements (one per line, up to 10; entrants confirm each one, the site does not check them)
editor-review = Let me review entries before the draw (I can remove entrants with a reason; shown on the card)
editor-early-draw = Let me draw before the deadline (shown on the card)
editor-distribution = Red packet split
editor-distribution-equal = Equal
editor-distribution-random = Random
editor-packet-note = An equal red packet's budget must divide evenly by the shares; a random one gives at least 1 { $credit } per share.
editor-source-name = Source name
editor-source-url = Source link
editor-source-author = Original author
editor-add-image = Add an image
editor-upload-image = Upload image
# Without scripts, under the form: who will see the new thread.
editor-fallback-members = Once posted, only signed-in members can see it
editor-fallback-public = Once posted, visitors who are not signed in can see it too
editor-save = Save changes
editor-submit-thread = Post thread
editor-submit-reply = Post reply
editor-preview = Preview
# $zone is the site time zone, such as UTC+8.
editor-deadline = Activity deadline (site time zone { $zone })
editor-deadline-days =
    { $days ->
        [one] In { $days } day
       *[other] In { $days } days
    }
editor-deadline-custom = Custom
editor-deadline-at = Deadline
editor-prizes = Custom prizes (optional)
# Example lines of the prize field: name | winners | type | points each | note. The type word must be the one this language accepts ($code, $credit, $item).
editor-prizes-example-code = First prize | 1 | { $code } | | Annual membership
editor-prizes-example-credit = Second prize | 3 | { $credit } | 100
editor-prizes-example-item = Souvenir | 2 | { $item } | | Message the author after winning
editor-prizes-help = One per line: name, winners, type, { $credit } each and a note, separated by |. Up to { $max }. Leave empty to split { $credit } evenly by the total budget and the number of winners.
editor-codes = Redemption codes (one per line)
editor-codes-help = Fill them in the order of the code prizes, one code per winner. Only you and that winner can see a code, and codes are not kept in the browser draft.
editor-add-custom-prize = Add a custom prize
editor-prize-name = Prize name
editor-prize-winners = Winners
editor-prize-kind = Prize type
# Prize types; also the words accepted in the type column of the prize field (the points type is the site's name for its points).
editor-prize-kind-code = Code
editor-prize-kind-item = Other
editor-prize-amount = { $credit } each
editor-prize-codes-help = One code per winner. Only you and that winner can see a code, and codes are not kept in the browser draft.
editor-prize-note = Note (optional)
editor-remove-prize = Remove prize
editor-poll-mode = Voting
editor-poll-single = Single choice
editor-poll-multiple = Multiple choice
editor-poll-max = Choices per voter (1 for single choice)
editor-poll-lines = Options (one per line, 2 to 20)
editor-poll-options = Options
editor-poll-remove = Remove
editor-poll-add = Add option
editor-poll-help = 2 to 20 options. You can paste several lines at once.
# The state of an image attachment in the editor.
editor-upload-ready = Passed the check
editor-upload-pending = Waiting for the check
editor-upload-processing = Checking
editor-upload-rejected = Did not pass the check
editor-upload-purging = Being removed
editor-upload-purged = Removed
editor-upload-insert = Insert into post
# The page shown after uploading without scripts.
editor-upload-title = Attachment check
editor-upload-description = Upload and check progress
editor-upload-help = You can keep editing once the file passes the check. Refreshing does not upload it again, and your title and text are kept.
editor-upload-refresh = Refresh and keep editing
# Page headings of the editor.
editor-heading-edit-thread = Edit thread
editor-heading-edit-reply = Edit reply
editor-heading-new-thread = New thread
editor-heading-new-reply = Reply
editor-description = Share your thoughts
# Why the visibility of a thread cannot be changed while editing it (after "Who can see it: ").
editor-scope-reserved = this thread is not public right now, and its visibility cannot be changed here.
editor-scope-members = this forum is members only, so the thread is shown to signed-in members only.
editor-scope-closed = this forum is not public right now, and neither is the thread.
editor-error-visibility = Visibility can only be public or members only
editor-error-forum = Forum not found
editor-error-sign-in = Please sign in first
editor-error-target = The target does not exist or cannot be accessed
editor-error-publish = Posting failed for now. Keep your text and try again
editor-error-parent = The parent post does not exist
# $reason says why the form token was rejected.
editor-error-stale = The form has expired. Check your text and try again. Reason: { $reason }
editor-error-sign-in-to-post = Please sign in to post. Your text has been kept
editor-error-thread = The thread cannot be accessed
editor-error-reply = The reply cannot be accessed
editor-error-body-long = The post is too long
editor-error-edit-target = The post to edit does not exist
editor-error-sign-in-to-edit = Please sign in again. Your edits have been kept
editor-error-reference = A reference in the post is invalid
editor-error-attachments = A post can reference at most { $max } attachments
editor-error-mentions = A post can mention at most { $max } accounts
editor-error-quotes = A post can quote at most { $max } posts
# ContentWriteError: what went wrong when posting or replying.
editor-error-unauthorized = Please sign in to post
editor-error-not-found = The thread, forum or post you are replying to does not exist or cannot be accessed
editor-error-title = Choose a forum and enter a valid title
# $level is the level name required to start red packets and lotteries.
editor-error-activity-level = You need to reach { $level } to start red packets and lotteries
editor-error-reply-target = The reply target is invalid
editor-error-closed = The thread is closed to new replies
editor-error-body = The post is empty or too long
editor-error-request-key = The request ID is invalid. Please reload the form
editor-error-time = The posting time is invalid
editor-error-references = A reference in the post is invalid, or it mentions or quotes too many
editor-error-attachment-ids = Too many attachments or an invalid attachment ID
editor-error-frozen = Your account is frozen and cannot post
# The fallback message of the participation rules (the site needs a verified email).
editor-error-verify = Verify your email to post
editor-error-request-reused = This request was already used for other content. Please use a new form
editor-error-new-member-threads = You have used up your new-member thread quota
editor-error-new-member-replies = You have used up your new-member reply quota
editor-error-daily-threads = You have used up today's thread quota
editor-error-daily-replies = You have used up today's reply quota
editor-error-attachment-missing = The attachment does not exist or cannot be used
editor-error-attachment-unusable = The attachment did not pass the check, has expired or is not yours
editor-error-attachment-trashed = The attachment is in the trash. Restore it first
# Anti-spam rules.
editor-error-thread-interval = You are posting threads too fast. Please wait a moment
editor-error-reply-interval = You are replying too fast. Please wait a moment
editor-error-hourly = You have reached the hourly posting limit. Please try again later
editor-error-duplicate = You recently posted the same text. Please do not post it again
# New members cannot post links to other sites yet; $days is the observation period after signing up, $level a level name.
editor-error-links-days =
    { $days ->
        [one] You can post links to other sites { $days } day after signing up
       *[other] You can post links to other sites { $days } days after signing up
    }
editor-error-links-level = You can post links to other sites once you reach { $level }
editor-error-links-days-level =
    { $days ->
        [one] You can post links to other sites { $days } day after signing up, once you reach { $level }
       *[other] You can post links to other sites { $days } days after signing up, once you reach { $level }
    }
# Editing and moderating posts.
editor-error-edit-references = An attachment or mention reference is invalid
editor-error-edit-attachments = Too many attachments
editor-error-edit-empty = Provide at least a title, a body or a visibility
editor-error-edit-title = The title is empty or too long
editor-error-edit-author = The author of this content has changed. Please reopen the form
editor-error-edit-role = Trainee moderators and reviewers cannot edit content on the author's behalf
editor-error-edit-reply-title = A reply cannot change the thread title
editor-error-edit-reply-visibility = A reply cannot change who can see the thread
editor-error-edit-reserved-visibility = The thread's visibility is in a reserved state and cannot be changed by editing
editor-error-intern-restore = This request needs withdrawn content restored, which trainee moderators cannot do. Please pass it to a moderator or administrator
editor-error-reason-long = The reason cannot be longer than 500 characters
editor-error-reviewer-actions = Reviewers can only hide or unhide reported content
editor-error-intern-actions = Trainee moderators can only hide, unhide, lock and unlock
editor-error-reserved-post = The content is in a reserved state, so moderation cannot hide or restore it
editor-error-thread-action = Locking, pinning and featuring must target the first post of a thread
editor-error-reserved-thread = The thread is in a reserved state, so this moderation action cannot be used
editor-error-edit-request = The target, version or request ID is invalid
editor-error-reason-required = Moderation actions need a reason of 1 to 500 characters
editor-error-own-hidden = You cannot edit or unhide your own content that moderation hid. Please pass it to another moderator or an administrator
editor-error-window = The time to edit, withdraw or restore this content has passed
editor-error-version = The content or thread has changed. Compare with the latest version and submit again
editor-error-thread-moved = The thread has moved. Please reopen the page
editor-error-post-moved = The content has moved. Please reload
editor-error-erased = The content was permanently erased and cannot be edited or restored
editor-error-edit-request-reused = This request ID was already used for another action. Please reopen the form
editor-error-edit-attachment-unusable = The attachment did not pass the check, has expired, or you cannot add a reference to it
editor-error-version-range = The content version is out of range
# What a moderator did, in the notice to the author (followed by the moderator's reason).
editor-moderation-edited = Edited by staff
editor-moderation-hidden = Content hidden
editor-moderation-unhidden = Content unhidden
editor-moderation-restored = Content restored
editor-moderation-locked = Thread locked
editor-moderation-unlocked = Thread unlocked
editor-moderation-pinned = Thread pinned
editor-moderation-unpinned = Thread unpinned
editor-moderation-featured = Thread featured
editor-moderation-unfeatured = Thread no longer featured
editor-moderation-withdrawn = Content withdrawn
# The text stored with a moderation notice: what was done and the moderator's reason.
editor-moderation-notice = { $action }: { $reason }
