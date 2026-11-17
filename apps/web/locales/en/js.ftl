## Script text (docs/translating.md, "Rules for messages"): plain text, { $var }, or one select on a variable.

# The theme switch in the header; the label is read by screen readers, the title shown on hover.
js-theme-light = Theme: light
js-theme-dark = Theme: dark
js-theme-system = Theme: system
js-theme-title = { $label }, click to switch
# The buttons at the bottom right of long pages.
js-jump-top = Back to top
js-jump-bottom = Go to bottom
# The posting editor (composer.js): toolbar buttons.
js-editor-bold = Bold
js-editor-italic = Italic
js-editor-strike = Strikethrough
js-editor-heading = Heading
js-editor-quote = Quote
js-editor-ul = Bulleted list
js-editor-ol = Numbered list
js-editor-code = Code
js-editor-link = Link
js-editor-attach = Upload images
js-editor-spoiler = Spoiler
js-editor-emoji = Emoji
js-editor-mention = Mention
js-editor-preview = Preview
js-editor-fullscreen = Full screen
js-editor-placeholder = Write your thoughts…
js-editor-toolbar = Formatting
js-editor-choose-image = Choose images
js-editor-body-required = Please write something.
# $max is the byte limit of a post.
js-editor-body-too-long = The post cannot be longer than { $max } bytes. Please shorten it and post again.
js-editor-uploads-unsettled = An image is still uploading or being checked. Post once it is in the text, or remove it.
# Who will see a new thread, under the editor.
js-editor-audience-choose = Choose who can see it first
js-editor-audience-members = visible to signed-in members only
js-editor-audience-public = visible to visitors who are not signed in too
# Must equal the members-only note after a forum name in the forum list (editor-forum-members-only), without its leading space.
js-editor-members-only-suffix = (members only)
js-editor-audience-summary = Posting to “{ $forum }” · { $audience }
# Placeholder text inserted by the toolbar when nothing is selected.
js-editor-text = text
js-editor-code-text = code
js-editor-spoiler-text = spoiler
js-editor-link-text = link text
# Local drafts.
js-editor-draft-blocked = The browser does not allow saving a draft. Copy your text before leaving.
js-editor-draft-conflict = The content has changed since your draft. The old draft is kept; compare before you go on editing.
js-editor-draft-old = View the local draft of the old version
js-editor-draft-old-label = Local draft of the old version
js-editor-draft-use-current = Keep editing the current version
js-editor-draft-editing-current = Editing the current version; copy whatever you want to keep from the old draft above.
js-editor-draft-restored = Restored this account's local draft.
js-editor-attachment = Attachment
js-editor-published = Posted.
js-editor-preview-failed = The preview failed. Check that you are signed in and try again.
js-editor-preview-error = The preview cannot be loaded. Please try again.
# Mentions.
js-editor-mention-placeholder = Search user names
js-editor-mention-search = Search users to mention
js-editor-mention-results = Matching users
js-editor-mention-none = No users found.
js-editor-mention-error = Users cannot be searched right now. Please try again.
# Image uploads in the editor.
js-editor-upload-waiting = Waiting to upload
js-editor-upload-progress = Upload progress of { $name }
js-editor-attachment-remove = Remove from the post
js-editor-attachment-cancel = Cancel attachment
js-editor-retry = Retry
js-editor-attachment-insert = Insert attachment
js-editor-upload-interrupted = The upload was interrupted; retrying reuses the same request.
js-editor-upload-timeout = The upload timed out. Please try again.
# $status is the HTTP status code.
js-editor-upload-failed = The upload failed ({ $status }). Check the file and that you are signed in.
js-editor-upload-unreadable = The upload result cannot be read. Please try again.
js-editor-state-ready = Ready
js-editor-state-processing = Checking the file…
js-editor-state-pending = Waiting for the file check…
js-editor-state-rejected = The file did not pass the check.
js-editor-state-purging = Removing the attachment…
js-editor-state-purged = The attachment was removed.
js-editor-state-working = Processing…
js-editor-check-pending = The file check has not finished yet; you can check again later.
js-editor-check-error = The attachment status cannot be read. Please try again.
js-editor-cancel-failed = The attachment could not be cancelled. Please try again.
# The activity deadline in the editor.
js-deadline-expired = The draft's deadline has passed, so it was set to 3 days from now.
# The poll options in the editor; $n is the option's number.
js-poll-option = Option { $n }
js-poll-remove = Remove { $option }
js-poll-min = Please fill in at least two options.
js-poll-max = Choices per voter
js-poll-too-many = A poll has at most 20 options. Paste fewer lines.
# Lottery prizes in the editor; $n is the prize's number, $credit the site's name for its points.
js-prize-name-pipe = A prize name cannot contain the bar |.
js-prize-name-length = Prize names must be 1 to 60 characters.
js-prize-code-count = The number of codes must equal this prize's winners.
js-prize-code-duplicate = Codes cannot repeat within one lottery.
js-prize-code-format = A code must be 1 to 255 visible ASCII characters, without double quotes or backslashes.
js-prize-legend = Prize { $n }
js-prize-remove = Remove prize { $n }
js-prize-note-long = A prize note can be up to 200 characters.
js-prize-add = Add a prize
js-prize-add-custom = Add a custom prize
js-prize-mode-custom = Prizes are drawn in this order, one prize per winner at most. Only { $credit } prizes count toward the frozen budget.
js-prize-mode-simple = { $credit } are split evenly by the total budget and the number of winners. You can also set custom prizes, each with its own winners.
# The red packet preview in the editor; $total, $count and $each are amounts, $credit the site's name for its points.
js-packet-too-many = There cannot be more shares than the total budget; each share gets at least 1 { $credit }.
js-packet-random =
    { $count ->
        [1] { $total } { $credit } in { $count } share, at least 1 { $credit } per share.
       *[other] { $total } { $credit } split at random into { $count } shares, at least 1 { $credit } per share.
    }
js-packet-indivisible = { $total } { $credit } cannot be split evenly into { $count } shares. Adjust the total budget or the shares.
js-packet-equal =
    { $count ->
        [1] { $total } { $credit } in { $count } share of { $each } { $credit }.
       *[other] { $total } { $credit } split into { $count } shares of { $each } { $credit } each.
    }
# Copying an invite code.
js-copy-done = Copied
js-copy-selected = Selected; please copy it yourself
# The "recently viewed" card: a thread without a title, the list after clearing, and relative times ($n is a number).
js-recent-untitled = Thread #{ $id }
js-recent-cleared = Browsing history cleared
js-time-just-now = just now
js-time-minutes-ago =
    { $n ->
        [one] { $n } minute ago
       *[other] { $n } minutes ago
    }
js-time-hours-ago =
    { $n ->
        [one] { $n } hour ago
       *[other] { $n } hours ago
    }
# Unread threads in lists (docs/skins.md, "Unread"): the words after an unread title, for screen readers (a skin may
# show them), with the number of new replies when the reader opened the thread before; and the status after the
# reading progress on this device was cleared in the reading settings.
js-unread = Unread
js-unread-replies =
    { $count ->
        [one] { $count } new reply
       *[other] { $count } new replies
    }
js-read-cleared = Reading progress on this device cleared
# The title of an embedded video player when the page gives none.
js-video-title = YouTube video
# The debug panel (debug.js): the status next to a query's copy button after copying it, and when the browser does
# not allow copying and the SQL is selected instead.
js-debug-copied = Copied
js-debug-copy-selected = SQL selected; press ⌘C / Ctrl+C to copy it
# The timeline behavior module (docs/skins.md, "Behavior modules"): the slider over a thread's floors.
js-timeline-label = Floor in this thread
js-timeline-hint = Arrow keys move one floor, Page Up and Page Down one page, Home and End to the first and last floor. Enter goes there; Escape goes back.
# The number shown on the slider's handle.
js-timeline-short = { $position } / { $last }
# Read by screen readers as the slider's value; $date is the date shown near that floor.
js-timeline-value = Floor { $position } of { $last }, { $date }
js-timeline-value-plain = Floor { $position } of { $last }
# The progress behavior module: the reading progress of a thread.
js-progress-label = Reading progress
js-progress-value = Floor { $position } of { $last }
# The docked-composer behavior module: the button on the bar at the bottom of a thread page.
js-dock-open = Write a reply
js-dock-close = Hide the reply box
