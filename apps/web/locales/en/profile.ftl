## Public profiles and member collections (templates/user/).

# Tabs of a public profile.
profile-view-overview = Recent activity
profile-view-threads = Threads
profile-view-replies = Replies
profile-deleted = This account has been deleted
# When a member joined; $date is a date.
profile-joined = Joined { $date }
# A verification badge; $category is the verified kind (merchant, blogger and so on).
profile-verified = Verified { $category }
profile-frozen = Frozen
profile-deleting = Being deleted
profile-report = Report profile
profile-edit = Edit profile
profile-following = Following
profile-followers = Followers
# Growth is earned by contributing content and decides the member's level.
profile-growth = Growth
profile-level-progress = Level progress
# Under the stats: $level and $name are the next level, $threshold its growth threshold, $remaining what is still needed.
profile-level-remaining = { $remaining } growth to Lv{ $level } { $name } ({ $threshold })
profile-top-level = Highest level reached
profile-level-rules = About levels
profile-tabs = Member content
# The number of items in a profile tab.
profile-total =
    { $count ->
        [one] { $count } item
       *[other] { $count } items
    }
profile-reply-number = Reply #{ $position }
profile-no-threads = No public threads yet.
profile-no-replies = No public replies yet.
profile-no-activity = No recent activity.
profile-sidebar = Profile navigation
profile-space = Profile
profile-forums = Forums
profile-error-not-found = Member not found
profile-error-private = This profile is not public
profile-online = Online
# When the member was last seen; $when is today, yesterday, N days ago or a date.
profile-last-seen = Last seen { $when }
# Recent activity on a profile: what the member did before the thread title.
profile-action-thread = Posted
profile-action-reply = Replied to
# A profile tab's title: $name is the member, $collection the tab (threads, replies).
profile-collection = { $name }: { $collection }
# Admin area: honor groups.
profile-manage-honors = Honor groups
profile-manage-honors-description = Create, edit and disable honor groups; grant and revoke them per member
# $credit is the site's name for its points.
profile-manage-honors-lead = Honor groups are granted by administrators and only show as a badge on thread floors and profiles, with the group description on hover. They carry no permissions and cannot be bought with { $credit }. Disabled groups are no longer shown, but their grants are kept; groups cannot be deleted. Frozen, expired and deleting members are not shown.
profile-manage-member-uid = Member UID
profile-manage-grant-or-revoke = Grant or revoke
# $order is the group's sort value.
profile-manage-order-value = Order { $order }
profile-manage-enabled = Enabled
profile-manage-disabled = Disabled
profile-manage-honors-empty = No honor groups yet.
profile-manage-create-heading = New honor group
profile-manage-name = Group name
profile-manage-name-help = 1–10 characters: the badge text shown on floors.
profile-manage-description = Group description
profile-manage-description-help = 1–60 characters, shown when hovering over the badge.
profile-manage-order = Order
profile-manage-order-help = Lower comes first; a member with several groups has them in this order.
profile-manage-create = Create
# $name is the group name.
profile-manage-honor-title = Honor group · { $name }
profile-manage-honor-description = Edit an honor group and see its holders
profile-manage-enabled-help = Once disabled, the group is no longer shown; its grants are kept and come back when it is enabled again.
profile-manage-save = Save changes
profile-manage-back = Back to honor groups
profile-manage-holders = Holders
profile-manage-granted = Granted { $time }
profile-manage-expires = valid until { $time }
profile-manage-holders-empty = No member holds this honor group yet.
profile-manage-holders-pages = Holder pages
profile-manage-member-description = Grant, renew and revoke a member's honor groups
# $state is the member's account state, such as Normal.
profile-manage-account-state = Account: { $state }
profile-manage-profile-link = Profile
profile-manage-valid-until = Valid until
profile-manage-renew = Renew
profile-manage-revoke-reason = Reason for revoking
profile-manage-revoke = Revoke
profile-manage-member-empty = This member has no honor groups yet.
profile-manage-grant-heading = Grant an honor group
# $link_html links to the honor group list.
profile-manage-no-choices = No honor group is enabled. Create or enable one under { $link_html } first.
profile-manage-expires-help = Leave empty for no expiry. A date is the last valid day; after it the badge disappears by itself. Renew groups the member already holds above.
profile-manage-grant = Grant
# Notices after saving.
profile-manage-done-created = Honor group created.
profile-manage-done-saved = Saved.
profile-manage-done-unchanged = Nothing changed, nothing saved.
profile-manage-done-granted = Granted.
profile-manage-done-revoked = Revoked.
# A grant without an end date.
profile-manage-no-expiry = No expiry
# Account states and whether a grant is shown.
profile-manage-state-deleting = Deletion in progress
profile-manage-state-frozen = Frozen
profile-manage-state-normal = Normal
profile-manage-hidden-disabled = Group disabled, not shown
profile-manage-hidden-expired = Expired, not shown
profile-manage-hidden-deleting = Deletion in progress, not shown
profile-manage-hidden-frozen = Frozen, not shown
profile-manage-shown = Shown
profile-manage-error-order = The order must be between 0 and 9999
profile-manage-error-no-honor = No such honor group
profile-manage-error-no-member = No such member, or the account is deleted
profile-manage-error-date = Invalid expiry date
profile-manage-error-name = The group name must be 1 to 10 characters on one line
profile-manage-error-description = The group description must be 1 to 60 characters on one line
profile-manage-error-name-taken = An honor group with this name already exists
profile-manage-error-changed = This honor group was just changed. Please refresh and fill in the form again
profile-manage-error-deleting = This account is being deleted and cannot be granted honor groups
profile-manage-error-choose = Choose an honor group
profile-manage-error-past = The expiry date must be after today
profile-manage-error-disabled = This honor group is disabled and cannot be granted
