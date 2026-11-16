## Site pages: the forum directory, leaderboard, verified members, links, feeds and the points rules (templates/site/).

# Kinds of verified members.
site-verified-merchant = Merchant
site-verified-agent = Agent
site-verified-blogger = Blogger
site-verified-webmaster = Webmaster
site-verified-channel = Channel owner
site-forums-lede = Every forum you can see, grouped under its parent forum.{" "}
site-forums-statistics = Thread counts and today's posts count only content public to everyone; a parent forum includes its subforums. The figures refresh every so often.
# The word of a forum's thread count in the forum directory; $count only picks the plural form.
site-forums-threads-label =
    { $count ->
        [one] thread
       *[other] threads
    }
# A forum's thread count: $value_html is the number and $label_html the word (site-forums-threads-label), each in its
# own element so that skins can style them apart. Put them in your language's order.
site-forums-threads = { $value_html } { $label_html }
# The word of a forum's posts today in the forum directory.
site-forums-today-label = Today
# A forum's posts today: $value_html is the number and $label_html the word (site-forums-today-label), as above.
site-forums-today = { $label_html } { $value_html }
site-forums-children = Subforums of { $forum }
site-forums-empty = No forums yet.
site-forums-description = Every forum and subforum.
site-ranks = Leaderboard
site-ranks-boards = Boards
site-ranks-periods = Periods
# A board and period, such as "Growth leaderboard · 30 days".
site-ranks-title = { $board } leaderboard · { $period }
site-ranks-updated = updated { $time }
# After the board, period and update time; $rules_html links to how the boards are counted.
site-ranks-note = . Refreshed every { $seconds } seconds. Threads, replies and likes count public content only: { $rules_html }.
site-ranks-rules = how the boards are counted
site-ranks-board = { $board } leaderboard
site-ranks-rank = Rank { $rank }
site-ranks-pending = The leaderboard is being built. Please check back later.
site-ranks-empty = Nobody is on this board for this period yet.
# Leaderboards.
site-ranks-board-threads = Threads
site-ranks-board-replies = Replies
site-ranks-board-likes = Likes
# The unit after a member's score.
site-ranks-unit-growth = growth
site-ranks-unit-threads = threads
site-ranks-unit-replies = replies
site-ranks-unit-likes = likes
# Leaderboard periods.
site-ranks-period-day = 24 hours
site-ranks-period-month = 30 days
site-ranks-period-all = All time
site-ranks-error-query = Invalid leaderboard request
site-ranks-error-board = No such leaderboard
site-ranks-error-period = No such period
site-ranks-description = Members ranked by growth, threads, replies and likes. Only public content counts.
# Points rewards on the points rules page.
site-credit-rule-signin = Daily check-in
site-credit-rule-thread = New thread
site-credit-rule-reply = New reply
site-credits-title = About points
site-credits-description = How { $credit }, growth, levels and the leaderboard work.
# The points rules page (/credits/rules). $credit is the site's name for its points.
site-credits-lede = This site keeps two numbers. { $credit } are points you can spend in activities; growth is reputation, earned only by contributing content, and decides your level. The two never affect each other.
# $link_html links to the member's own points page.
site-credits-body = { $credit } are whole-number points. They cannot be withdrawn, exchanged for cash or freely transferred between members. Starting a bounty, red packet or lottery freezes the whole budget, and whatever is not paid out returns when the activity ends. Only you can see your balance and history, in { $link_html }.
site-credits-reward = Reward
site-credits-state = Status
site-credits-each = Each time
site-credits-daily = Daily cap
site-credits-on = On
site-credits-off = Off
# $zone is the site time zone, such as UTC+8.
site-credits-daily-note = Daily caps count calendar days in the site time zone, { $zone }. When what is left for the day is less than a full reward, that reward is not given.
site-credits-growth-lede = Growth is reputation. It cannot be spent, transferred or exchanged, and spending { $credit } never lowers it. These actions add growth:
site-credits-action = Action
site-credits-growth-thread = Post a thread
site-credits-growth-reply = Post a reply (top-level or nested)
site-credits-growth-like = Receive a like (once per member per post; taken back if the like is removed)
site-credits-growth-digest = Your thread is featured
site-credits-growth-accept = Your reply wins a bounty
site-credits-growth-no-signin = Checking in adds no growth.
site-credits-growth-fixed = Values are fixed when you post; changing them later only affects posts made afterwards.
site-credits-growth-no-cap = There is no separate daily cap: posting already has a quota (members can post { $threads } threads and { $replies } replies a day).
site-credits-growth-revoked = When a post is hidden by moderators, withdrawn by its author or permanently erased, all growth it brought (posting, likes, featuring, accepting) is taken back, and given back when it is unhidden or restored. Only the post itself counts: making a thread members-only, moving it to another forum or hiding the post it sits under does not affect other members' posts.
# $link_html links to the member's growth history.
site-credits-growth-ledger = Growth can go down through take-backs and administrator adjustments, and the level follows. Every change is recorded in your { $link_html } and cannot be altered.
site-credits-growth-ledger-link = growth history
site-credits-levels = Levels
site-credits-levels-lede = Levels are for display only: next to an author's name on posts, on profiles and on the leaderboard. They never change posting quotas or any permission.
site-credits-level = Level
site-credits-level-name = Name
site-credits-level-floor = Growth needed
# $link_html links to the leaderboard.
site-credits-ranks-boards = The { $link_html } has four boards: growth, threads, replies and likes, each over the last 24 hours, the last 30 days and all time. It refreshes every { $seconds } seconds and shows the top { $shown }.
site-credits-ranks-public = Threads, replies and likes count only public content everyone can see: members-only threads and forums and hidden or withdrawn content do not count. The growth board ranks net growth (all time ranks current growth).
site-credits-ranks-hidden = Frozen members, deleted members, members being deleted and members whose profile is set to "only me" are left out; members whose profile is for signed-in members only are shown to signed-in members only; members you blocked do not appear on the boards you see.
site-verified-lede = Merchants, agents, bloggers, webmasters and channel owners checked by the administrators. Verification only means the identity was checked; it does not vouch for any service or deal.
site-verified-categories = Kinds
site-verified-empty = No verified { $category } yet.
site-verified-more = More verified { $category }
site-directory-pages = Directory pages
site-contact-page = Contact page
site-contact-email = Site email
site-verified-error-query = Invalid directory request
site-verified-error-category = No such kind of verification
site-verified-title = Verified { $category } · Verified members
site-verified-description = Merchants, agents, bloggers, webmasters and channel owners checked by the administrators.
site-links-lede = Forums, blogs, directories and tools that link to this site and we link back.
site-links-apply = Apply for a link
site-links-apply-description = Submit your site to exchange links with this one.
site-links-submitted = Submitted. An administrator will review it.
site-links-withdrawn = Withdrawn or taken down.
site-links-error-rate = Too many submissions. Please try again in an hour
site-links-empty = No links yet.
site-links-closed = Link applications are closed for now.
site-links-login = Log in to apply
site-links-name = Site name
site-links-category = Category
site-links-url = URL
site-links-backlink = Page with our link
site-links-backlink-placeholder = Leave empty if it is your home page
site-links-backlink-hint = If our link is not on your home page, give the address of the page it is on.
site-links-note = Message
site-links-note-hint = For the administrators only, up to 200 characters.
site-links-submit = Submit
site-links-mine = My applications
site-links-reason = Reason: { $reason }
site-links-withdraw = Withdraw application
site-links-take-down = Take down my link
site-links-none = No applications yet.
site-links-back = Back to links
# Link categories.
site-links-kind-forum = Forums
site-links-kind-blog = Blogs
site-links-kind-directory = Directories
site-links-kind-tools = Tools
site-links-kind-other = Other
# Link application statuses.
site-links-status-pending = Pending review
site-links-status-active = Listed
site-links-status-rejected = Rejected
site-links-status-withdrawn = Withdrawn
site-links-status-removed = Taken down
site-links-error-url = Enter an address that starts with http:// or https://, has a domain name and is at most 500 characters
site-links-error-backlink = The page with our link must be on the site you are applying with
site-links-error-name = The site name must be 1 to 30 characters on one line
site-links-error-category = Choose a category
site-links-error-duplicate = This site is already listed or under review
site-links-error-expired = The form has expired. Please reload and submit again
site-links-error-self = You cannot apply with this site's own address
site-links-error-note = The message can be at most 200 characters on one line
site-links-error-closed = Link applications are closed for now
site-links-error-frozen = Your account is frozen and cannot apply
site-links-error-age = Your account is not old enough to apply yet (see the application notes)
site-links-error-limit = Your account has reached the limit of pending and listed links
site-links-error-cooldown = This site was just rejected and cannot apply again until the waiting period ends
site-feeds = Feeds
site-feeds-lede = Add these addresses to an RSS or Atom reader to get newly posted public threads.
site-feeds-body = Each feed has the { $limit } most recent threads, newest first, each with only its title, author, forum and a summary of at most { $chars } characters; open the thread to read it all. Members-only threads and forums, hidden or pending threads and threads withdrawn by their authors never appear.
site-feeds-scope = Scope
# $link_html links to the member's feed settings.
site-feeds-private = Signed-in members can create a personal feed address in { $link_html }: it leaves out forums you hid or blocked and members you blocked, and can include members-only threads.
site-feeds-private-link = Settings · Feed address
site-feeds-private-title = { $site } (personal feed)
site-feeds-private-forum-title = { $forum } - { $site } (personal feed)
site-feeds-not-found = No such feed
# The first row of the feed list: the feed of the whole site.
site-feeds-site = Whole site
site-feeds-description = Follow the newest public threads of the whole site and of each forum in an RSS or Atom reader.
# Plain-text answers of robots.txt, the sitemap and images.
site-plain-settings = The site settings are invalid: { $key } (now { $found }; expected: { $expected })
site-plain-settings-failed = Cannot read the site settings: { $detail }
site-plain-no-sitemap = Pages of this site are canonical on the main domain; there is no sitemap here
site-plain-sitemap-page = Invalid sitemap page
site-plain-no-brand-image = No such brand image
site-error-no-image = No such image
site-error-no-forum-icon = No such forum icon
# $directory_html links to the link directory; $count is how many links the home page sidebar shows.
site-manage-links-intro = Member applications go to "Pending review" and appear on { $directory_html } once approved; links marked "Show on home page" fill the home page sidebar, the first { $count } by sort value. Turn applications on or off under "Application rules" below, and edit the application notes on the "Link guidelines" tab.
site-manage-links-status = Link status
site-manage-link-on-home = on the home page
site-manage-link-order = order
site-manage-link-by-admin = Added by an administrator
site-manage-link-applicant = Applied by { $name }
# $time is when the link was submitted.
site-manage-link-submitted = submitted { $time }
site-manage-links-empty = No friend links match.
site-manage-links-pages = Link pages
site-manage-rules-saved = Application rules saved.
site-manage-rules-note = The two items counted live from the sign-up and rejection times apply retroactively at once; the built-in application notes (unless saved on the "Link guidelines" tab) show the numbers set here.
site-manage-rules-save = Save application rules
site-manage-link-add = Add directly
site-manage-link-add-submit = Add and show
site-manage-link-heading = Friend link · { $name }
# The applicant's member number.
site-manage-link-uid = {" "}(UID { $uid })
# $time is when the link was handled.
site-manage-link-handled = handled { $time }
site-manage-link-backlink = Page with our link:{" "}
site-manage-link-backlink-home = The applicant says our link is on their home page.
site-manage-link-note = Message: { $note }
site-manage-link-approve = Approve and show
site-manage-link-update = Save changes
site-manage-link-reject-reason = Reason for rejecting
site-manage-link-remove-reason = Reason for taking it down
site-manage-link-reason-help = The applicant sees it in their application history.
site-manage-link-reject = Reject application
site-manage-link-remove = Take down link
site-manage-link-description = Description
site-manage-link-description-help = Optional, shown in the directory; at most 60 characters.
site-manage-link-sort = Sort value
site-manage-link-sort-help = Smaller values come first.
site-manage-link-home = Show on home page
site-manage-link-error-order = The sort value must be a whole number
site-manage-links-error-status = No such status
site-manage-links-all = All
site-manage-links-description = Review applications, and add, edit and take down friend links
site-manage-link-not-found = No such friend link
site-manage-link-saved = Saved.
site-manage-link-unchanged = This record was already handled; nothing changed.
site-manage-link-error-action = Unknown action
site-manage-link-error-reason = The reason must be 1 to 200 characters on one line
site-manage-link-error-order-range = The sort value must be between -9999 and 9999
site-manage-link-error-description = The description can be at most 60 characters on one line
site-manage-link-error-full = 500 friend links are already shown; take some down first
# Admin area: verified members.
site-manage-verified = Verified members
site-manage-description = Grant, edit, renew and revoke member verifications
# $link_html links to the public directory.
site-manage-verified-lead = Only administrators can grant, edit, renew and revoke verifications. Frozen, expired and deleting ones are not shown publicly; the record is kept until it is renewed or revoked. The public directory is at { $link_html }; the how-to-apply text is edited on the “Verification guidelines” tab.
site-manage-member-uid = Member UID
site-manage-grant-or-edit = Grant or edit
site-manage-filters = Verification filters
site-manage-granted = Granted { $time }
site-manage-reviewed = reviewed { $time }
site-manage-expires = valid until { $time }
site-manage-empty = No verifications match.
site-manage-pages = Verification pages
# $state is the member's account state, such as Normal.
site-manage-account-state = Account: { $state }
site-manage-profile-link = Profile
site-manage-category = Category
# The verification's own text, as on the public directory.
site-manage-title = Verification text
site-manage-title-help = Plain text, such as “Official account of Example Cloud”. Shown in the directory and on the profile, and when hovering over the floor badge.
site-manage-site = Linked website
site-manage-site-help = Optional. Public links carry nofollow ugc.
site-manage-valid-until = Valid until
site-manage-expires-help = Leave empty for no expiry. A date is the last valid day; after it the verification disappears by itself. Changing it renews the verification.
site-manage-save = Save changes
site-manage-grant = Grant verification
site-manage-revoke-reason = Reason for revoking
site-manage-revoke = Revoke verification
site-manage-back = Back to verified members
# A verification without an end date.
site-manage-no-expiry = No expiry
# Filter tabs of the verification list (the categories follow them).
site-manage-all = All
site-manage-expiring = Expiring within 30 days
site-manage-expired = Expired
site-manage-done-revoked = Verification revoked.
site-manage-done-saved = Verification saved.
site-manage-done-unchanged = Nothing changed, nothing saved.
# Whether a verification is shown publicly right now.
site-manage-unverified = Not verified
site-manage-hidden-deleting = Deletion in progress, not shown
site-manage-hidden-frozen = Frozen, not shown
site-manage-hidden-expired = Expired, not shown
site-manage-shown = Shown
# Account states of the member.
site-manage-state-deleting = Deletion in progress
site-manage-state-frozen = Frozen
site-manage-state-normal = Normal
site-manage-error-no-member = No such member
site-manage-error-filter = No such filter
site-manage-error-no-account = No such member, or the account is deleted
site-manage-error-date = Invalid expiry date
site-manage-error-site-scheme = The linked website must start with http:// or https://
site-manage-error-site = The linked website must be an http or https address without a user name or password, up to 500 characters
site-manage-error-category = Choose a category
site-manage-error-title = The verification text must be 1 to 60 characters on one line
site-manage-error-deleting = This account is being deleted and cannot be verified
site-manage-error-past = The expiry date must be after today
# Errors of a member's own link application.
site-links-verify-first = Verify your email before applying
site-links-already-handled = This application has already been handled
