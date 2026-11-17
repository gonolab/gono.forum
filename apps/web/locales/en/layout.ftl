# Block titles a layout may name (layout.rs, TITLE_KEYS; docs/skins.md, "Places and blocks"). The hot and latest
# titles read like the sidebar cards they replace.
block-title-hot = Popular threads
block-title-latest = Latest threads
block-title-active = Recently active
block-title-featured = Featured threads
block-title-pinned = Pinned threads
block-title-boards = Boards
block-title-new-members = Newest members
block-title-ranks = Leaderboard
block-title-honors = Honors
block-title-verified = Verified members
block-title-moderators = Moderators
block-title-stats = Site statistics
block-title-links = Links
block-title-outline = In this thread
block-title-notice = Notice
block-title-navigation = Navigation

# Problems of a layout that is saved or imported; $path names the field, such as places.home.aside[2].view.
layout-error-unknown = { $path } is not something this version of Gono Forum knows.
layout-error-invalid = { $path } has a value that is not allowed.
layout-error-missing = { $path } is missing.
layout-error-too-many = { $path } has more blocks than it takes; a place takes at most its own limit and a page at most 10 blocks.
layout-error-duplicate = { $path } repeats the id of another block.
layout-error-not-here = { $path } cannot be shown in this place.
layout-error-too-long = { $path } is too long.

# Texts of the block views (templates/blocks/).
block-empty = Nothing here yet.
block-rank-place = Place
block-rank-member = Member
block-rank-score = Score
block-stats-today = Today
block-board-threads = { $count ->
        [one] { $count } thread
       *[other] { $count } threads
    }
block-board-today = { $count } today
# A page of a thread in its outline: floors $from to $to.
block-outline-page = Page { $page }: floors { $from }–{ $to }
block-moderator-trainee = trainee

# Appearance › Layout (docs/skins.md, "Editing a layout").
layout-page-title = Layout
layout-page-description = Choose the blocks each place of the home, board and thread pages shows.
layout-heading-site = Layout of the site
# $domain is the domain's host.
layout-heading-domain = Layout of { $domain }
layout-lead-site = Each place shows its blocks from the first layout that sets it: a domain's own, the site's, the skin's, then the default. Changes go into a draft that only you can preview until you publish it.
layout-lead-domain = Places this domain does not change follow the site. Changes go into the domain's draft; preview it on this domain.
layout-field-target = Layout of
layout-target-site = The whole site
layout-target-open = Open
layout-draft-pending = There is an unpublished draft.
layout-draft-none = No draft: what you see is what visitors get. The first change starts a draft.
layout-draft-previewing = You are previewing it.
layout-preview = Preview the draft
layout-publish = Publish the draft
layout-discard = Discard the draft
layout-published = The layout was published.
layout-draft-saved = Saved to the draft.
layout-draft-discarded = The draft was discarded.
layout-preview-bar = You are previewing a layout draft. Visitors still see the published layout.
# $count is the most blocks the place takes.
layout-capacity = { $count ->
        [one] It takes { $count } block.
       *[other] It takes up to { $count } blocks.
    }
layout-state-own-site = The site sets this place.
layout-state-own-domain = This domain sets this place.
# $origin names where the blocks come from (layout-from-*).
layout-state-inherited = This place follows { $origin }.
layout-from-site = the site's layout
layout-from-skin = the skin's layout
layout-from-core = the default layout
layout-place-empty = No blocks.
layout-move-up = Move up
layout-move-down = Move down
layout-remove = Remove
layout-inherit-site = Use the skin's or the default blocks
layout-inherit-domain = Follow the site
layout-own = Change this place
layout-add-source = Add a block
layout-add-next = Next
layout-add-title = Add a block
# $source is the kind of block and $place the place.
layout-add-heading = Add { $source } to { $place }
layout-add-submit = Add to the draft
layout-add-cancel = Back to the layout
layout-place-home-aside = Home page aside
layout-place-home-aside-note = Beside the thread list of the home page, its later pages and search.
layout-place-board-aside = Board page aside
layout-place-board-aside-note = Beside the thread list of board pages; "this page" means the board.
layout-place-thread-aside = Thread page aside
layout-place-thread-aside-note = Beside a thread's floors; "this page" means the thread's board or the thread.
layout-place-head-home = Home page head
layout-place-head-board = Board page head
layout-place-head-thread = Thread page head
layout-place-head-note = At the top of the main column.
layout-place-footer = Footer
layout-place-footer-note = Beside the policy links and the line "Built on Gono Forum", which always stay.
layout-place-side-nav = Side navigation
layout-place-side-nav-note = Shown only under a skin with side navigation.
layout-core-member-card = Member card
layout-core-recent-local = Recently viewed
layout-core-site-overview = Site statistics with the newest members
layout-core-friend-links = Friend links
layout-core-board-head = Board head
layout-core-nav-boards = Board tree
layout-core-nav-links = Navigation links
layout-core-markdown = Text
layout-source-threads = Threads
layout-source-boards = Boards
layout-source-members-recent = Newest members
layout-source-members-rank = Leaderboard
layout-source-members-honors = Honor holders
layout-source-members-verified = Verified members
layout-source-moderators = Moderators
layout-source-site-stats = Statistics
layout-source-site-links = Links
layout-source-thread-outline = Thread outline
layout-view-mini-list = Short list
layout-view-list = List
layout-view-cards = Cards
layout-view-tree = Tree
layout-view-grid = Grid
layout-view-directory = Directory
layout-view-avatars = Avatars
layout-view-table = Table
layout-view-figures = Figures
layout-view-compact = One line
layout-view-links = Links
layout-view-timeline = Dates
layout-view-progress = Pages
layout-view-fixed = Card
layout-show-all = Everyone
layout-show-guests = Visitors who are not signed in
layout-show-members = Signed-in members
layout-field-view = View
layout-field-scope = From
layout-field-sort = Order
layout-field-days = Hot over
layout-field-days-hint = Only for hot threads.
layout-field-category = Kind
layout-field-category-hint = Only for the latest replies and the newest threads.
layout-field-limit = How many
layout-field-parent = Boards
layout-field-counts = Show thread counts
layout-field-last-post = Show each board's latest thread
layout-field-rank-board = Leaderboard
layout-field-period = Period
layout-field-honor = Honor group
layout-field-verified-kind = Verification
layout-field-board = Board
layout-field-links-place = Links
layout-field-title = Title
layout-field-title-hint = A title from the list, or your own below. Blocks in an aside need one.
layout-field-title-en = Own title in English
layout-field-title-zh = Own title in Simplified Chinese
layout-field-body-en = Text in English (Markdown)
layout-field-body-hint = At most 4 KB per language; English is shown where no other language is written.
layout-field-body-zh = Text in Simplified Chinese (Markdown)
layout-field-show = Shown to
layout-field-keep = Keep on phones
layout-value-site = The site's setting
layout-value-current = This page's board
layout-value-whole-site = The whole site
# $days is a number of days.
layout-value-days = { $days ->
        [one] { $days } day
       *[other] { $days } days
    }
layout-value-any = Any
layout-value-top = Top-level boards
layout-value-every-honor = Every honor group
layout-value-links-home = Home page links
layout-value-links-directory = Every listed link
layout-value-own-title = Own title
layout-sort-latest = Latest replies
layout-sort-created = Newest threads
layout-sort-hot = Hot
layout-sort-featured = Featured
layout-sort-pinned = Pinned
layout-switch-title = Change the skin
# $skin is the new skin's name.
layout-switch-heading = Change to { $skin }
layout-switch-lead = This layout sets these places of its own. Keep them under the new skin, or show the new skin's blocks there instead:
layout-switch-hidden = The new skin would not show these blocks, because it has no side navigation:
layout-switch-keep = Keep my layout
layout-switch-skin = Use the new skin's layout
layout-switch-site-skin = the site's skin
layout-domain-link = Layout of this domain
layout-domain-lead = Change the blocks of single places for this domain; the others follow the site.
