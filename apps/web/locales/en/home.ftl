home-cross-pins = Pinned in other forums
# The accessible reply count of a thread in a list.
home-replies-label =
    { $count ->
        [one] { $count } reply
       *[other] { $count } replies
    }
# The accessible view count of a thread in a list.
home-views-label =
    { $count ->
        [one] { $count } view
       *[other] { $count } views
    }
# The word of the visible reply count of a pinned thread from another forum; $count only picks the plural form.
home-replies-short-label =
    { $count ->
        [one] reply
       *[other] replies
    }
# The visible reply count of a pinned thread from another forum: $value_html is the number and $label_html the word
# (home-replies-short-label), each in its own element so that skins can style them apart. Put them in your language's
# order.
home-replies-short = { $value_html } { $label_html }
# The word of the visible view count of a pinned thread from another forum; $count only picks the plural form.
home-views-short-label =
    { $count ->
        [one] view
       *[other] views
    }
# The visible view count of a pinned thread from another forum, built like home-replies-short.
home-views-short = { $value_html } { $label_html }
home-search-scope = Search in
home-search-threads = Threads
home-search-members = Members
home-forum-strip = Forums
# The first item of the mobile forum strip: every forum.
home-all = All
home-moderators = Moderators
home-guidance = Posting guidelines
home-child-forums = Subforums
home-thread-filters = Thread filters
home-thread-sort = Sort threads
home-thread-categories = Thread types
home-post = New thread
home-thread-pages = Thread pages
# The accessible name of a page link of a long thread in the list.
home-thread-page = { $title }, page { $number }
home-empty = No threads match.
home-thread-list = Threads
home-sidebar = Community overview
# The sidebar card of threads this browser opened recently (kept only in the browser).
home-recent = Recently viewed
home-recent-clear = Clear
home-recent-note = Saved only in this browser
home-friend-links = Links
home-friend-links-empty = No links yet
home-friend-links-all = All links
home-friend-links-apply = Apply for a link
home-stats = Site statistics
home-stats-threads = Threads
home-stats-replies = Replies
home-new-members = Newest members
# Sort tabs of thread lists.
home-sort-latest = Latest replies
home-sort-created = Newest threads
home-error-filter = Invalid filter
home-error-search-page = Invalid search page
home-error-search-too-long = The search term is too long
home-error-category = Invalid thread type
home-search-heading = Search: { $query }
# The member card in the home sidebar.
home-card-verify-email = Email not verified
home-card-bind-mobile = No phone number yet
home-card-shortcuts = Shortcuts
# The member's reports, appeals and other cases.
home-card-cases = My cases
home-card-settings = Settings
home-card-favorites = Bookmarks
home-card-messages = Messages
home-card-guest = Guest
home-card-guest-hint = Log in to post
home-card-checked-in = Checked in today
# The daily check-in button with its reward.
home-card-check-in = Check in +{ $amount }
home-directory-title = Forums
# Read by screen readers before the title of a board's latest thread in the home page's board directory.
home-directory-latest = Latest thread:
home-directory-none = No threads yet
