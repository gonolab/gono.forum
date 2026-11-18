## Forums, moderator appointments, moving threads and content management in the admin area.

forum-previous = Previous
forum-next = Next
forum-content-title = Content
# Meta description of the content management page.
forum-content-description = Manage threads, replies and their status
forum-content-kind = Type
forum-content-kind-all = Threads and replies
forum-content-thread = Thread
forum-content-reply = Reply
# $position is the floor number of the reply.
forum-content-reply-number = Reply #{ $position }
forum-content-forum = Forum
forum-content-forum-all = All forums you manage
forum-content-state = Status
forum-content-state-all = All statuses
forum-content-state-visible = Public
forum-content-state-hidden = Hidden / not public
forum-content-state-deleted = Withdrawn
forum-content-keyword = Keyword
forum-content-keyword-placeholder = Title, body or username
forum-content-filter = Filter
# $total is the number of matching posts, $page the current page.
forum-content-summary =
    { $total ->
        [one] { $total } item, page { $page }
       *[other] { $total } items, page { $page }
    }
forum-content-locked = Locked
forum-content-pinned = Pinned
forum-content-featured = Featured
forum-content-empty = No content matches the filter.
forum-content-pagination = Content pages
# The effective public state of a post in the content list and detail page.
forum-content-status-erased = Permanently erased
forum-content-status-withdrawn = Withdrawn
forum-content-status-hidden = Hidden / not public
forum-content-status-public = Public
forum-content-error-filter = The content filter is not valid.
forum-content-error-revisions-page = The revision page number is not valid.
forum-content-error-forbidden = This account does not have this management permission.
forum-content-error-session = Your session has expired. Please sign in again.
forum-content-error-withdraw = The admin area hides content; only the author can withdraw it, from their own pages.
forum-content-error-reason = Please give a reason of 1–500 characters.
forum-detail-title = Content details and revisions
# Meta description of the content detail page.
forum-detail-description = The original text of the content and the management actions
# $links_html are the links to the pending requests, such as "#3, #5".
forum-detail-restorations = The author has pending restoration requests: { $links_html }.
forum-detail-edit = Edit this content
forum-detail-public = View on the site
forum-detail-original = Current text
forum-detail-erased = The body has been permanently erased and cannot be edited or restored.
forum-detail-action = Action
forum-detail-choose = Choose an action
forum-detail-hide = Hide
forum-detail-unhide = Unhide
forum-detail-restore = Restore withdrawn content
forum-detail-lock = Lock
forum-detail-unlock = Unlock
forum-detail-pin = Pin (level 1)
forum-detail-pin2 = Pin (level 2)
forum-detail-pin3 = Pin (level 3)
forum-detail-unpin = Unpin
forum-detail-feature = Feature
forum-detail-unfeature = Unfeature
forum-detail-reason = Reason
# $pid is the post id shown on this page.
forum-detail-target = The action applies to PID { $pid } on this page. Hiding a parent post also keeps its replies out of public view; restoring content still respects any existing hidden state.
forum-detail-pin-scope = Pin scope: level 1 pins only in this forum; level 2 pins above page 1 of every forum under the same parent (the parent and all its child forums); level 3 pins above page 1 of every forum on the site. The home page sorts by pin level and does not list them separately.
forum-detail-submit = Apply and record the reason
forum-detail-revisions = Internal revisions
# $total is the number of revisions, $page the current page.
forum-detail-revisions-summary =
    { $total ->
        [one] { $total } revision, page { $page }
       *[other] { $total } revisions, page { $page }
    }
# $version is the revision number.
forum-detail-revision = Version { $version }
forum-detail-no-revisions = No earlier revisions.
forum-detail-revisions-pagination = Revision pages
forum-moderators-title = Moderators
# Meta description of the moderator appointment page.
forum-moderators-description = Appoint and remove global moderators, forum moderators, trainee moderators and reviewers
forum-moderators-lead = Administrators can appoint and remove global moderators, forum moderators, trainee moderators and reviewers. In the forums they are appointed to, trainee moderators can only hide and unhide, lock and unlock, handle reports, and handle restoration requests that only need unhiding; reviewers only handle reports, across the whole site. After a role or its forums change, the account must sign in again.
forum-moderators-username = Username
forum-moderators-search = Find account
# $total is the number of matching accounts.
forum-moderators-total =
    { $total ->
        [one] { $total } account
       *[other] { $total } accounts
    }
forum-moderators-role-member = Member
forum-moderators-role-global = Global moderator
forum-moderators-role-moderator = Forum moderator
forum-moderators-role-intern = Trainee moderator
forum-moderators-role-reviewer = Reviewer
forum-moderators-edit = Set role
forum-moderators-empty = No accounts found.
forum-moderators-pagination = Account pages
# $name_html is the username.
forum-moderators-appoint = Appoint { $name_html }
forum-moderators-role = Role
forum-moderators-forums = Forums (forum moderators and trainee moderators only; choose at least one)
forum-moderators-inheritance = A parent forum's moderators also manage all its child forums, so there is no need to tick those; a child forum can also have its own moderators.
forum-moderators-reason = Reason
forum-moderators-submit = Appoint and record the reason
forum-moderators-error-form = The appointment form is not valid.
forum-moderators-error-query = The account search is not valid.
# $title is the thread title.
forum-move-heading = Move thread · { $title }
forum-move-title = Move thread
# Meta description of the move page.
forum-move-description = Move a thread to another forum
forum-move-lead = Moving needs management rights in both the source and the target forum. After the move, the thread and its posts stay reachable at their old addresses.
forum-move-target = Target forum
forum-move-reason = Reason
forum-move-submit = Move
forum-move-back = Back to thread
forum-settings-title = Forum settings
forum-settings-saved = Forum settings saved.
forum-settings-lead = Site-wide settings for forums. They are saved with the site settings, separately from saving a single forum.
forum-settings-directory = Show thread counts and today's posts on the forum directory
# $link_html links to the directory page /forums.
forum-settings-directory-help = Two numbers for each forum on the directory page { $link_html }.
# Follows forum-settings-directory-help in the same paragraph; $seconds is the forum statistics cache duration.
forum-settings-directory-refresh =
    {" "}Only public content readable by anonymous visitors counts, and a parent forum includes its child forums. The numbers refresh with the forum statistics cache duration in "Global settings › Search and performance" (currently { $seconds ->
        [one] { $seconds } second).
       *[other] { $seconds } seconds).
    }
forum-settings-save = Save forum settings
forum-forums-title = Forums
# Meta description of the forum management page.
forum-forums-description = Forum hierarchy and access settings
forum-forums-lead = Forums have at most two levels, and each parent forum has at most 10 child forums. Parent forum pages, feeds, follow notifications and the admin content filter all include the threads of child forums, and a child forum follows its parent's stricter access scope. A parent forum's moderators also manage all its child forums. Thread counts and today's posts count only public content readable by anonymous visitors, and a parent forum includes its child forums.
forum-forums-deleted = The forum has been deleted.
forum-forums-merging = Merge queued: a background task moves the threads to the target forum in batches and then disables this forum.
forum-forums-order-saved = Order saved.
forum-forums-child = Child forum
forum-forums-top = Top-level forum
forum-forums-disabled = Disabled
forum-forums-members = Members only
forum-forums-private = Admin only
forum-forums-public = Public
# $threads is the forum's thread count, $today its posts today.
forum-forums-count = Threads { $threads } · Today { $today }
forum-forums-no-count = Not counted
forum-forums-view = View
# Followed by the forum's moderators.
forum-forums-moderators = Moderators:{" "}
forum-forums-moderators-none = none
forum-forums-order = Order
forum-forums-empty = No forums yet.
forum-forums-order-help = Lower numbers come first; equal numbers go by forum id. Saving stores every number here at once, changes only the forums whose number changed, and does not rewrite threads. The moderator column of a child forum lists only direct appointments; the parent forum's moderators manage it too.
forum-forums-order-reason = Reason for the new order
forum-forums-order-save = Save order
forum-forums-create = Create forum
forum-forums-edit = Edit forum
forum-forums-name = Name
forum-forums-slug = Address
forum-forums-parent = Parent forum
forum-forums-parent-none = None (top-level forum)
forum-forums-field-description = Description
forum-forums-guidance = Posting guidance
forum-forums-status = Status
forum-forums-enabled = Enabled
forum-forums-visibility = Access
forum-forums-visibility-public = Everyone
forum-forums-visibility-members = Signed-in members
forum-forums-visibility-private = Admin area only
# $link_html links to the brand image page.
forum-forums-icon-help = Upload images under { $link_html }, then come back to choose the forum icon.
forum-forums-icon-help-link = Brand and images
forum-forums-icon = Icon
forum-forums-icon-default = Default icon
forum-forums-color = Color
forum-forums-color-use = Give this forum a color
forum-forums-color-help = Skins can use it for the forum's badges, lines and icons; it is adjusted for contrast in light and dark mode.
forum-forums-glyph = Built-in icon
forum-forums-glyph-none = None
forum-forums-glyph-help = An icon from the built-in set that skins can show beside the forum's name.
# $own is the number of this forum's threads a change of parent rewrites.
forum-forums-rewrite =
    Changing the parent rewrites the copies on this forum's { $own ->
        [one] { $own } thread
       *[other] { $own } threads
    } in the same transaction. With many threads the save takes longer, and posting in this forum pauses meanwhile. Other changes do not rewrite threads.
forum-forums-save = Save forum
forum-removal-title = Delete or merge
# $id is the merge task number, $target the target forum, $link_html links to the task.
forum-removal-merging = Merge task #{ $id } is running: this forum's threads are being moved to “{ $target }” in batches, and this forum will be disabled afterwards. See { $link_html } for progress.
forum-removal-merging-link = the background task
forum-removal-lead = A forum without threads or child forums can be deleted: its moderator appointments, member follows and personal filters are removed with it, and navigation entries that point to it are no longer shown. Merge a forum that has threads: a background task moves all its threads to the target forum in batches, keeping thread numbers, post numbers and links, then disables this forum (you can delete it afterwards). Move the child forums of a forum elsewhere first.
forum-removal-delete-reason = Reason for deleting
forum-removal-delete = Delete forum
forum-removal-target = Merge into
forum-removal-merge-reason = Reason for merging
forum-removal-merge = Merge threads into the target forum
# Markers after a forum's name in drop-downs of move and merge targets.
forum-option-disabled = {" "}(disabled)
forum-option-private = {" "}(admin only)
forum-option-members = {" "}(signed-in members)
# $name is a moderator's username; trainee moderators are marked in the forum's moderator list.
forum-moderator-intern = { $name } (trainee)
# Written into the thread author's notification when a thread is moved; $reason is the moderator's reason.
forum-notice-thread-moved = Your thread was moved to another forum: { $reason }
forum-error-admin-only = Only administrators can configure forums.
forum-error-order-number = Order numbers must be whole numbers.
forum-error-taken = That name or address is already taken. Choose another one.
forum-error-hierarchy-changed = The forum hierarchy has changed. Please submit again.
forum-error-hierarchy-retry = The forum hierarchy has changed. Please try again.
forum-error-forbidden = This account does not have this permission.
forum-error-forum-permission = You need management rights in this forum.
forum-error-reason = Please give a reason of at most 500 characters.
forum-error-fields = The forum name, address, level or access is not valid.
forum-error-look = The forum color must be a hex color such as #0f766e, and the icon one of the built-in icons.
forum-error-levels = Forums have at most two levels; a forum cannot be its own parent or have a child forum as its parent.
forum-error-children = A parent forum can have at most 10 child forums.
forum-error-parent-merging = That forum is being merged into another one and cannot take child forums. Choose another parent.
forum-error-stale = The forum has been changed. Refresh and try again.
forum-error-has-children = This forum still has child forums and cannot become a child forum.
forum-error-icon = Choose an image attachment that passed inspection.
forum-error-icon-trashed = The image is in the trash. Restore it first.
forum-error-domain-path = The forum address is the same as a sub-path registered for multi-domain sites (on a host that also registered its root path). Choose another one.
forum-error-order-form = The order form is incomplete. Refresh and try again.
forum-error-forum-id = The forum id or version is not valid.
forum-error-delete-admin = Only administrators can delete forums.
forum-error-delete-children = This forum still has child forums and cannot be deleted. Move or delete the child forums first.
forum-error-delete-threads = This forum still has threads and cannot be deleted. Merge a forum with content into another forum first.
forum-error-delete-merge-target = A merge task is moving threads into this forum. Wait for it to finish before deleting.
forum-error-merge-target = Choose another forum as the merge target.
forum-error-merge-admin = Only administrators can merge forums.
forum-error-merge-children = A forum with child forums cannot be merged. Move the child forums elsewhere first.
forum-error-merge-running = This forum already has a merge task in progress.
forum-error-merge-target-busy = The target forum is itself being merged elsewhere. Choose another target.
forum-error-merge-source-busy = A merge task is still moving threads into this forum. Wait for it to finish before merging this forum elsewhere.
forum-error-request-key = This request key was already used for another action.
forum-move-error-not-found = The thread does not exist.
forum-move-error-forbidden = Moving threads needs forum management rights.
forum-move-error-target = The move target or request key is not valid.
forum-move-error-stale = The thread was changed or moved. Refresh the page.
forum-moderators-error-input = Choose a valid role and its forums, and give a reason of 1–500 characters.
forum-moderators-error-request = This request was already used for another appointment. Open the form again.
forum-moderators-error-admin = Administrator roles cannot be changed on this page.
forum-moderators-error-stale = The account or its permissions have changed. Check the current details and submit again.
forum-moderators-error-account = Only registered accounts can be appointed; synthetic authors cannot get management rights.
forum-moderators-error-forum = One of the chosen forums does not exist.
forum-moderators-error-version = The account version is out of range.
# Names of the built-in board glyphs (organization/glyphs.rs), shown in the forum form.
forum-glyph-chat = Chat
forum-glyph-megaphone = Megaphone
forum-glyph-question = Question
forum-glyph-lightbulb = Light bulb
forum-glyph-bug = Bug
forum-glyph-wrench = Wrench
forum-glyph-code = Code
forum-glyph-book = Book
forum-glyph-graduation = Graduation cap
forum-glyph-newspaper = Newspaper
forum-glyph-calendar = Calendar
forum-glyph-people = People
forum-glyph-coffee = Coffee
forum-glyph-star = Star
forum-glyph-image = Image
forum-glyph-music = Music
forum-glyph-film = Film
forum-glyph-gamepad = Game controller
forum-glyph-palette = Palette
forum-glyph-briefcase = Briefcase
forum-glyph-tag = Tag
forum-glyph-globe = Globe
forum-glyph-pin = Map pin
forum-glyph-archive = Archive
forum-glyph-heart = Heart
forum-glyph-home = Home
forum-glyph-flag = Flag
forum-glyph-shield = Shield
forum-glyph-leaf = Leaf
forum-glyph-rocket = Rocket
forum-glyph-trophy = Trophy
forum-glyph-lock = Lock
