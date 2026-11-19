## The personal center: its sections and tabs, the overview, own content, private messages and the member card.

# The member's own threads or replies; $total is the number of items, $page the page number.
my-content-lead =
    { $total ->
        [one] { $total } item, page { $page }. This includes hidden and withdrawn content; only you can see its status.
       *[other] { $total } items, page { $page }. This includes hidden and withdrawn content; only you can see its status.
    }
my-content-empty = Nothing here yet.
# The count of conversations followed by the link to start one; $new_html is that link.
my-messages-lead =
    { $total ->
        [one] { $total } conversation.
       *[other] { $total } conversations.
    } { $new_html }
my-messages-new = Start a new conversation
my-messages-empty = No private messages yet.
# The heading of a private conversation; $name is the other member's username.
my-conversation-with = Conversation with { $name }
# The sender of the member's own messages in a conversation.
my-conversation-me = Me
my-conversation-empty = No messages in this conversation yet.
my-conversation-older = Older messages
my-conversation-newer = Newer messages
my-conversation-reply = Reply
my-send = Send
my-conversation-deleting = This member is closing their account and cannot receive private messages.
my-conversation-deleted = This member's account has been deleted; you can no longer send them private messages.
my-messages-back = Back to conversations
my-compose-lead = Send a private message to a member. If you already have a conversation with them, the message continues it.
my-compose-recipient = Recipient's username
my-compose-body = Message
# $max is the longest message in characters.
my-compose-body-hint = Up to { $max } characters, plain text
# The greeting on the overview: $name_html is the member's username, $date the day they joined.
my-overview-greeting = Hello, { $name_html }. You joined on { $date }.
my-overview-public-profile = View public profile
my-overview-stats = My statistics
# Points held back for open activities; $credit is the site's name for its points.
my-overview-frozen = Frozen { $credit }
my-card-nav = My center navigation
# The dot after a card entry with something unread.
my-card-unread = Unread
# Instead of a title or name the viewer cannot open any more.
my-unavailable = Content not available
# $time is when the thread was bookmarked.
my-favorited-at = Bookmarked { $time }
my-favorites-empty = No bookmarks yet.
my-notifications-read-all = Mark all as read
my-notifications-unread = Unread
my-notifications-open-record = View record
my-notifications-open-content = View content
my-notifications-mark-read = Mark as read
my-notifications-empty = No notifications yet.
# Top-level sections of the personal center.
my-section-home = Home
my-section-content = Content
# Pages of the personal center (tabs).
my-page-overview = Overview
my-page-growth = Growth history
my-page-prizes = Prizes
my-page-invites = Invites
my-page-following-threads = Followed threads
my-page-following-forums = Followed forums
my-page-following-members = Followed members
my-page-following-feed = Following feed
my-page-attachments = Attachments
my-page-attachments-trash = Attachment bin
my-page-api = API
my-page-preferences = Notification preferences
my-page-reading = Reading preferences
my-page-style = Personal style
my-page-blocked-users = Blocked members
my-page-forum-filters = Forum filters
my-page-feed = Feed address
my-page-reports = Reports
my-page-sanctions = Account record
my-page-data = Data requests
# Notification category tabs.
my-filter-replies = Replies and mentions
my-filter-following = Following
my-filter-system = System
# The author shown on a governance notification whose operator is unknown.
my-notification-staff = Moderators
my-error-notification-page = Invalid notification page
my-error-notification-id = Invalid notification or time
my-error-notification-bound = Invalid notification range or time
my-description-overview = Your content, notifications and account settings
my-error-page-number = Invalid page number
my-description-content = Content you posted, including hidden and withdrawn content
my-description-messages = One-to-one conversations with other members
my-compose-title = New conversation
my-compose-description = Send a private message to a member
my-error-message-rate = You are sending private messages too often, please try again later
my-error-too-many-actions = Too many actions, please try again later
my-error-log-in = Please log in first
my-error-too-often = You are doing this too often, please try again later
my-favorites-title = My bookmarks
my-favorites-description = Threads you bookmarked
my-error-reply-unavailable = The reply is not available
# Titles of the small responses after liking or downvoting.
my-like-title = Like
my-downvote-title = Downvote
my-notifications-description = Your interactions, moderation and account notifications
my-error-request-key = Invalid request, refresh the page
# What happened, after the actor's name in the notification list.
my-notice-reply = replied to you
my-notice-mention = mentioned you
my-notice-quote = quoted your post
my-notice-follow-thread = replied to a thread you follow
my-notice-follow-forum = posted a thread in a forum you follow
my-notice-moderation = Moderation result
my-notice-report-result = Report outcome
my-notice-sanction = Account sanction notice
my-content-unavailable = Content unavailable
my-error-frozen = Your account is frozen
my-error-interaction-target = Invalid target or request, refresh the page
my-error-read-only-like = The site is read-only for now; liking is unavailable
my-error-read-only-downvote = The site is read-only for now; downvoting is unavailable
my-error-request-used-refresh = This request was already used for something else, refresh the page
my-error-like-own = You cannot like your own content
my-error-downvote-own = You cannot downvote your own thread
my-error-downvote-unavailable = You cannot downvote this thread for now
my-error-request-used = This request was already used for something else
my-error-recipient-deleting = This member is closing their account and cannot receive private messages
# Blocked by the recipient; the block itself is not revealed.
my-error-recipient-refuses = This member does not accept your private messages
my-error-message-busy = Private messages are busy, please try again later
my-error-message-length = A private message must be 1 to 2,000 characters
my-error-message-self = You cannot send a private message to yourself
my-error-request-invalid = Invalid request
my-error-recipient-deleted = This member's account has been deleted; you can no longer send them private messages
my-error-message-key-used = This send request was already used for other content; refresh the page and send again
my-error-member-not-found = Member not found
# A message started from a profile whose username, left as prefilled, now names someone else.
my-error-recipient-renamed = This username now belongs to a different member than the one you started writing to. Check the recipient and send again
my-error-too-many-attempts = Too many attempts, please try again later
# A report result or sanction notification addressed to someone else (asserted by tests, so it stays a message).
my-error-notification-owner = The notification's recipient is not the reporter or the sanctioned member
# Why the private message entry on a profile is unavailable.
my-message-entry-read-only = Private messages are unavailable while the site is read-only
my-message-entry-verify-email = Verify your email before sending private messages
