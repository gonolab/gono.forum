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
