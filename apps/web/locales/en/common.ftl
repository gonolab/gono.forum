## Text shared by many front pages: pagination, the sidebar thread cards, list separators and the error pages.

common-pagination = Pagination
# The accessible name of a page link; $number is the page number.
common-page-number = Page { $number }
common-hot-threads = Popular threads
common-reply-count =
    { $count ->
        [one] { $count } reply
       *[other] { $count } replies
    }
# The popular card when nothing was replied to within the window; $days is the window in days.
common-hot-empty =
    { $days ->
        [one] No new replies in the last day
       *[other] No new replies in the last { $days } days
    }
common-latest-threads = Latest threads
common-pinned = Pinned
# Screen reader text of the lock icon on a locked thread in a list; the icon is hidden unless a skin shows it.
common-locked = Locked
# Between the items of an inline list of names.
common-list-separator = {", "}
# Between a label and what it introduces.
common-colon = {": "}
common-back-home = Back to home
# The plain error page shown when the site settings cannot be read: which settings row is broken.
common-plain-key = Row
# After the key of the broken settings row; the slots are the table and column names in code.
common-plain-key-where = {" "}(table { $table_html }, column { $column_html })
common-plain-found = Value now
common-plain-expected = Expected
common-plain-settings-hint = Site settings never fall back to defaults: correct this row in the database, then reload the page (the admin settings pages cannot read it either).
common-breadcrumb = Breadcrumb
common-featured = Featured
# Thread types (categories).
common-category-bounty = Bounty
common-category-red-packet = Red packet
common-category-lottery = Lottery
common-category-poll = Poll
common-category-news = News
common-category-normal = Discussion
# Member roles.
common-role-admin = Administrator
common-role-super-moderator = Global moderator
common-role-moderator = Moderator
common-role-intern = Trainee moderator
common-role-reviewer = Reviewer
common-role-member = Member
common-error-page-not-found = Page not found
# The page title from page 2 on; $name is the page's own title.
common-paged-title = { $name } - Page { $page }
# The whole response when a page template fails to render.
common-error-render = This page cannot be shown right now
common-error-no-such-page = This page does not exist
# The first item of breadcrumbs.
common-home = Home
# Heading and description of error pages.
common-error-request-failed = The request could not be completed
common-error-settings = The site settings are invalid
# The letter in a visitor's placeholder avatar.
common-guest-initial = G
# The name shown for an author whose account was deleted.
common-deleted-user = Deleted user
# Thread titles shown instead of a title that is gone.
common-erased-title = Content permanently erased
common-withdrawn-title = Thread withdrawn
# The status of a member's own post in their content list.
common-status-withdrawn = Withdrawn
common-status-hidden = Hidden
common-status-members = Members only
common-status-public = Public
# The link to the member's points page; $credit is the site's name for its points.
common-my-credits = My { $credit }
common-previous-page = Previous
common-next-page = Next
# A short note in parentheses after a label.
common-parenthesized = {" "}({ $text })
common-error-form = Invalid form
# The empty first option of a select.
common-choose = Choose
common-error-session = Your session has expired
# What a member must do before they can post, when the site's participation rules are not met (an error message).
common-need-email = Please verify your email first
common-need-mobile = This site needs a phone number first
common-need-email-mobile = Please verify your email or add a phone number first
common-need-google = This site needs a linked Google account first
common-need-github = This site needs a linked GitHub account first
common-need-google-github = This site needs a linked Google or GitHub account first
common-need-email-google = Please verify your email or link a Google account first
common-need-email-github = Please verify your email or link a GitHub account first
common-need-email-google-github = Please verify your email or link a Google or GitHub account first
common-need-mobile-google = Please add a phone number or link a Google account first
common-need-mobile-github = Please add a phone number or link a GitHub account first
common-need-mobile-google-github = Please add a phone number or link a Google or GitHub account first
common-need-email-mobile-google = Please verify your email, add a phone number or link a Google account first
common-need-email-mobile-github = Please verify your email, add a phone number or link a GitHub account first
common-need-any = Please verify your email, add a phone number or link another account first
# The plain answer to a request whose address cannot be read.
common-error-bad-address = Invalid request address
# The cache is down: what a page or answer says.
common-error-redis = Redis is temporarily unavailable
common-error-unavailable = The service is temporarily unavailable. Please try again later.
# The answer of /healthz when a dependency fails; $backend is MySQL, SQLite or Redis.
common-health-unavailable = { $backend } unavailable: { $detail }
common-health-timeout = { $backend } did not answer within two seconds
# A save whose cache invalidation broadcast failed (503).
common-cache-unpublished = Saved, but the other processes could not be notified ({ $detail }).
common-cache-unpublished-ttl = {" "}The cache lifetime read before saving was { $ttl ->
    [one] { $ttl } second
   *[other] { $ttl } seconds
}.
common-cache-unpublished-limit = {" "}Other processes refresh when their own old caches expire. If an earlier lifetime change did not reach them either, old caches may stay for up to { $seconds } seconds.
common-cache-member-frame = Saved, but the member display preference cache could not be cleared; refresh in a moment
