# The section of the most liked replies above the replies of a thread.
topic-highlights = Top replies
topic-highlight-likes =
    { $count ->
        [one] { $count } like
       *[other] { $count } likes
    }
topic-highlight-view = View reply
# Above the replies of a thread: how many members tipped it and the total; $credit is the site's name for its points.
topic-tip-summary =
    { $count ->
        [one] { $count } tip
       *[other] { $count } tips
    } · { $total } { $credit } in total
# After the list of tippers when not all of them are shown.
topic-tip-more = {" "}and others
topic-members-only = Members only
topic-move = Move thread
# The reply count next to the thread title.
topic-reply-count =
    { $count ->
        [one] { $count } reply
       *[other] { $count } replies
    }
# The view count next to the reply count under the thread title; approximate (docs/queries.md, "Views").
topic-view-count =
    { $count ->
        [one] { $count } view
       *[other] { $count } views
    }
# The badge next to the thread author's name on the opening post.
topic-author = Author
topic-quote = Quote
topic-opening-permalink = Permalink to the opening post
topic-blocked-content = Content from a member you blocked. Click to show
topic-edited = Edited
topic-reply-view = Reply view
topic-reply = Reply
# Switches one post between the flat and the nested view; $position is the post number.
topic-switch-view-label = Switch view: post #{ $position }
topic-switch-view = Switch view
topic-floor-permalink = Permalink to post #{ $position }
# Above a nested reply in the flat view: the top-level reply it belongs to.
topic-root-floor = In reply #{ $position }
# The bounty author picks the reply that wins the bounty.
topic-accept = Accept this reply
topic-sub-replies = Replies to post #{ $position }
topic-sub-pagination = Reply pages
topic-no-replies = No replies yet.
topic-post-reply = Post a reply
topic-locked = This thread is locked. New replies are paused.
topic-login-to-reply = Log in and verify your email to reply.{" "}
topic-sidebar = Account
topic-edit = Edit
topic-manage = Moderate
topic-restore = Restore
topic-withdraw = Withdraw
topic-restore-note = Restoring makes this content and its attachments public again.
topic-withdraw-note = Withdrawing takes this content and its attachments down. The post number and other members' replies stay.
topic-restore-confirm = Confirm restore
topic-withdraw-confirm = Confirm withdrawal
topic-report = Report
topic-tip = Tip
topic-tip-title =
    Tip · { $count ->
        [one] { $count } member tipped
       *[other] { $count } members tipped
    }
topic-tip-hint = Tip the author. Once per thread; it cannot be undone
# A tip amount button; $credit is the site's name for its points.
topic-tip-amount = { $amount } { $credit }
topic-tipped = Tipped
topic-tipped-title =
    Tipped · { $count ->
        [one] { $count } member tipped
       *[other] { $count } members tipped
    }
topic-unfavorite = Remove bookmark
topic-favorite = Bookmark
topic-favorite-count =
    { $count ->
        [one] { $count } bookmark
       *[other] { $count } bookmarks
    }
topic-favorited = Bookmarked
# After the bookmark label when the bookmark cannot be changed (a locked or withdrawn thread).
topic-favorite-locked-note = {" "}(cannot be changed right now)
topic-favorite-locked = The bookmark cannot be changed right now
topic-unlike = Unlike
topic-like = Like
# The accessible name of a like count that cannot be changed.
topic-like-count =
    { $count ->
        [one] { $count } like
       *[other] { $count } likes
    }
topic-like-own-note = {" "}(you cannot like your own post)
topic-like-own = You cannot like your own post
topic-like-locked = Liking is not available right now
topic-undownvote = Remove downvote
topic-downvote = Downvote
# Only the thread author sees the downvote count of the opening post.
topic-downvote-count =
    { $count ->
        [one] { $count } downvote (only you can see this)
       *[other] { $count } downvotes (only you can see this)
    }
topic-downvote-private = Only you can see the downvote count
topic-error-not-found = Thread not found
topic-error-view = Invalid reply view
topic-error-sub-page = Invalid reply page
topic-error-root-page = That reply is not on this page
topic-error-no-sub-page = That page of replies does not exist
# The two reply views of a thread.
topic-view-flat = Flat
topic-view-nested = Nested
topic-error-reply-target = The post you are replying to does not exist
topic-error-reply-closed = This post cannot take more replies
topic-error-quote = The quoted post is not available
topic-error-locked = This thread is locked. New replies are paused
# The link text the editor starts with when quoting a post.
topic-quote-link = Quote
# Shown instead of a body that is gone.
topic-tombstone-erased = The author permanently erased this text. The post stays in the discussion.
topic-tombstone-withdrawn-thread = The author withdrew this thread. The replies stay.
topic-tombstone-hidden = This reply is hidden.
topic-tombstone-withdrawn = This reply was withdrawn. The discussion stays.
# Above a nested reply: whom it answers; $position is that post's number.
topic-reply-to = Reply to { $author } · #{ $position }
# The link text of a quoted post in a body.
topic-quoted-post = Quoted post
# A top reply whose text has no excerpt (only images, for example).
topic-highlight-fallback = View the reply
