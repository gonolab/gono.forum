## Following, followers, blocks, forum filters and notification preferences.

# $total is the number of entries, $page the page number.
social-collection-lead =
    { $total ->
        [one] { $total } entry, page { $page }.
       *[other] { $total } entries, page { $page }.
    }
social-block-note = Blocking only affects what you see and your ordinary notifications; it does not stop them from reading your public content. Moderation and account security notifications still reach you.
social-collection-empty = Nothing here yet.
# $total is the number of followers, $page the page number.
social-followers-lead =
    { $total ->
        [one] { $total } follower, page { $page }. Only you can see this list. To stop someone following you, block them from their profile; blocking also removes the follows between you.
       *[other] { $total } followers, page { $page }. Only you can see this list. To stop someone following you, block them from their profile; blocking also removes the follows between you.
    }
social-followers-empty = No one follows you yet.
# $days is how many days back the feed reaches.
social-feed-lead =
    { $days ->
        [one] New threads from members you follow in the last day, newest first. There are no notifications or unread marks here.
       *[other] New threads from members you follow in the last { $days } days, newest first. There are no notifications or unread marks here.
    }
# The word of a thread's reply count in the following feed; $count only picks the plural form.
social-feed-replies-label =
    { $count ->
        [one] reply
       *[other] replies
    }
# A thread's reply count in the following feed: $value_html is the number and $label_html the word
# (social-feed-replies-label), each in its own element so that skins can style them apart. Put them in your
# language's order.
social-feed-replies = { $value_html } { $label_html }
social-feed-empty =
    { $days ->
        [one] Members you follow have not posted a new thread in the last day.
       *[other] Members you follow have not posted a new thread in the last { $days } days.
    }
social-feed-no-older = No older threads.
social-feed-latest = Back to the latest
social-feed-older = Older
social-forum-blocked = Blocked forum
social-forum-filters-manage = Manage forum filters
social-forum-filters-lead = Hide: the home page, the sidebar, search and the site-wide part of your personal feed leave out threads from these forums; opening the forum or a thread directly still works. Block: on top of that, opening the forum or one of its threads directly only shows a notice, and the forum no longer appears in profiles, bookmarks and follows, notifications or the forum choice when posting. A setting on a parent forum covers all its subforums. Only what you see is affected.
# $names lists the subforums the setting also covers.
social-forum-filters-children = Includes subforums: { $names }
social-forum-filters-empty = You have not hidden or blocked any forum yet; you can do it at the top of a forum page.
social-preferences-lead = Choose on-site and email notifications separately. Changes do not resend past notifications, and existing on-site notifications are kept.
# $link_html links to email verification.
social-preferences-verify = Content emails you turn on are only sent once your current email is verified. { $link_html }
social-preferences-site = On site
social-preferences-email = By email
social-preferences-message-email = Email reminder
social-preferences-message-note = One email when a conversation gets unread messages, skipped if you read them within 10 minutes; after you have read them, you are reminded again only when new messages arrive. The email does not include the message text. On-site unread reminders are always on.
social-preferences-login = Login alerts
social-preferences-login-email = Email me when a new device logs in
social-preferences-login-note = Sent when someone logs in from a browser that has not used this account before, with the time, the IP address and a summary of the browser.
social-preferences-unaffected = Account and moderation notices such as email verification, password recovery, changing your email, appeals and moderation decisions are not affected by these options.
social-preferences-save = Save notification preferences
social-follow-unavailable = Cannot follow for now
social-unblock = Unblock
social-block = Block member
social-unfollow = Unfollow
social-follow = Follow
# Page titles of the follow and block lists.
social-title-threads = Followed threads
social-title-forums = Followed forums
social-title-blocked = Blocked members
social-title-members = Followed members
social-followers-description = Members who follow you
social-error-cursor = Invalid page cursor
social-feed-description = New threads from members you follow
social-preferences-description = Choose on-site and email content notifications
social-preferences-saved = Notification preferences saved
social-error-save-too-many = Too many saves, please try again later
social-error-preferences-version = Invalid notification preferences version
social-error-preferences-format = Invalid notification options
# Buttons of the forum filter at the top of a forum page.
social-filter-hide = Hide
social-filter-block = Block
social-filter-unhide = Unhide
social-filter-hide-instead = Hide instead
# Instead of the name of a parent forum the member can no longer see.
social-filter-parent = Parent forum
# $parent is the parent forum's name.
social-filter-inherited-hide = Hidden along with the parent forum “{ $parent }”.
social-filter-inherited-block = Blocked along with the parent forum “{ $parent }”.
social-filter-hidden-note = Hidden from your timeline: the home page, the sidebar, search and the site-wide part of your personal feed leave out this forum's threads; opening the forum directly still works.
# $parent and $forum are forum names.
social-filter-blocked-parent = You blocked the parent forum “{ $parent }”, so its subforum “{ $forum }” is not shown either.
social-filter-unblock-parent = Unblock the parent forum
social-filter-blocked-forum = You blocked the forum “{ $forum }”. It no longer appears on the home page, in search, profiles, notifications or the forum choice when posting, and opening it directly only shows this notice.
social-filter-blocked-title = Blocked: { $forum }
social-filter-unavailable = Forum not available
social-filter-remove = Remove
social-filter-description = Manage hidden and blocked forums
social-error-filter-option = Invalid forum filter option
# Kinds of content notifications on the preferences page.
social-preference-reply = Replies
social-preference-quote = Quotes
social-preference-mention = Mentions
social-preference-thread = New replies in threads you follow
social-preference-forum = New threads in forums you follow
social-error-preferences-changed = Your notification preferences were changed elsewhere, refresh and try again
social-error-preferences-range = Notification preferences version out of range
# A forum's filter state in the list of filtered forums.
social-filter-mode-hide = Hidden
social-filter-mode-block = Blocked
social-error-filter-target = Invalid forum or request
social-error-target = Invalid target or request
social-error-block-self = You cannot block yourself
social-error-follow-self = You cannot follow yourself
social-error-read-only = The site is read-only for now; following members is unavailable
social-error-blocked = You have blocked this member; unblock them first
social-error-follow-limit = You can follow at most 500 members
