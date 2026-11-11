## Activities on threads (bounties, red packets, lotteries, polls, news sources), the points and growth pages,
## check-in, lottery entries, codes and prizes, and the errors of these actions.

# The activity card on a thread page. $time is a date and time.
activity-card-deadline = Ends { $time }
# $budget_html and $remaining_html are the amounts in bold; $credit is the site's name for its points.
activity-card-budget = Budget { $budget_html } { $credit } · { $remaining_html } { $credit } not yet paid out
activity-card-bounty = The author can accept one valid reply, top-level or nested, which gets the whole bounty at once; if none is accepted by the deadline, the budget is refunded.
# $link_html links to the accepted reply.
activity-card-accepted = Accepted { $link_html }.
activity-card-accepted-link = this reply
# $button is the label of the accept button under replies.
activity-card-accept-hint = Click “{ $button }” under the reply you want to accept.
activity-card-random-packet = Random red packet
activity-card-equal-packet = Equal red packet
activity-card-packet-shares =
    { $shares ->
        [one] { $shares } share, { $left } left. Each member other than the author claims one with their first valid reply, top-level or nested.
       *[other] { $shares } shares, { $left } left. Each member other than the author claims one with their first valid reply, top-level or nested.
    }
activity-card-packet-mine = You received { $amount } { $credit }.
activity-card-entry-click = One-click entry
activity-card-entry-reply = A valid reply enters automatically
# Continues the entry mode: how many have entered.
activity-card-entries =
    { $count ->
        [one] , once per member other than the author; { $count } entry
       *[other] , once per member other than the author; { $count } entries
    }
activity-card-excluded = , { $count } removed by the author
activity-card-winners =
    { $count ->
        [one] , { $count } winner.{" "}
       *[other] , { $count } winners.{" "}
    }
# Who takes no part in the draw.
activity-card-ineligible-reply-review = At the draw, entrants without a valid reply, with a frozen account or removed by the author are left out.
activity-card-ineligible-reply = At the draw, entrants without a valid reply or with a frozen account are left out.
activity-card-ineligible-click-review = At the draw, entrants with a frozen account or removed by the author are left out.
activity-card-ineligible-click = At the draw, entrants with a frozen account are left out.
activity-card-prize-winners =
    { $count ->
        [one] { $count } winner
       *[other] { $count } winners
    }
activity-card-prize-each = { $amount } { $credit } each
# Prize types on the card and the codes page.
activity-prize-code = Redemption code
activity-prize-item = Other prize
activity-card-review = The author reviews entries before the draw and can remove entries that do not meet the requirements, with a reason.
activity-card-early-draw = The author can draw before the deadline.
activity-card-drawn-early = It was due to end { $time }; the author drew early.
activity-card-requirements = Entry requirements
activity-card-requirements-note = The author sets and reviews the requirements; the site does not check them.
# $reason is what the author wrote.
activity-card-my-excluded = The author removed your entry: { $reason }
activity-card-entered = You have entered.
activity-card-won-credit = {" "}You won { $amount } { $credit }.
activity-card-won = You won: { $prize }
activity-card-comma = {", "}
activity-card-view-prize = view your prize
activity-card-confirm = I confirm: { $requirement }
activity-card-enter = Enter with one click
activity-card-confirm-enter = Confirm the requirements above and enter
activity-card-draw = Draw now (cannot be undone)
activity-card-entries-review = Entries and review
activity-card-results = Results
# An amount of points; $credit is the site's name for its points.
activity-amount = { $amount } { $credit }
activity-card-poll =
    { $voters ->
        [one] { $voters } voter
       *[other] { $voters } voters
    } · { $max ->
        [one] up to { $max } choice
       *[other] up to { $max } choices
    } · Votes are anonymous and cannot be changed once submitted.
activity-card-option-votes =
    { $option }: { $votes ->
        [one] { $votes } vote
       *[other] { $votes } votes
    }
activity-card-voted = You have voted.
activity-card-vote = Submit your vote (it cannot be changed)
activity-card-source = Source:{" "}
activity-card-original-author = Original author: { $author }
# The state of an activity, after its type on the card.
activity-state-cancelled = Ended early; the unpaid budget was refunded
activity-state-expired = Expired; the unpaid budget was refunded
activity-state-settled = Finished
activity-state-drawing = Drawing
activity-state-news = News source
activity-state-closed = Closed, waiting for settlement
activity-state-open = In progress
# The page of a lottery's redemption codes and other prizes.
activity-codes-title = Codes and prize delivery
activity-codes-description = How a lottery's redemption codes and other prizes were given out
activity-codes-owner = Only you and each winner can see the codes.
activity-codes-replace-note = {" "}Before the draw you can replace all codes of a prize at once; the number of codes must equal the winners.
activity-codes-correct-note = {" "}After the draw you can correct codes already given out one by one; the winner sees when a code was corrected.
activity-codes-admin = Administrators only see how prizes were given out, not the codes.
activity-codes-unreadable = This code cannot be decrypted right now
activity-codes-given-to = Given to { $name }
activity-codes-given = Given out
activity-codes-not-given = Not given out
activity-codes-corrected = Corrected
activity-codes-correct-label = Correct the code
activity-codes-correct = Correct
activity-codes-replace-label = Replace all (one per line, { $count } in total)
activity-codes-replace = Replace
activity-back-to-thread = Back to the thread
# The page listing a lottery's entries.
activity-review-title = Entries
activity-review-description = A lottery's entries and the author's review
activity-review-by-reply = Entry by reply
activity-review-total =
    { $count ->
        [one] { $count } entry
       *[other] { $count } entries
    }
activity-review-excluded = , { $count } removed
activity-review-end = .{" "}
activity-review-can-edit = Until the draw starts you can remove entries or undo a removal. A removal needs a reason, which the removed member can see.
activity-review-locked = The entries are locked or you are not the author, so you can only view them.
activity-review-no-review = This lottery does not use author review.
activity-review-entry-post = Entry reply
activity-review-removed = Removed: { $reason }
activity-review-restore = Undo removal
activity-review-reason = Reason for removal
activity-review-remove = Remove
activity-review-empty = No entries yet.
# The member's own lottery prizes (redemption codes and other prizes).
activity-prizes-title = My prizes
activity-prizes-description = Redemption codes and other prizes won in lotteries
activity-prizes-intro = Redemption codes and other prizes you won in lotteries are listed here; { $credit } prizes go straight to your balance and show up in your { $credit } history. Only you and the lottery's author can see a code, so please do not pass it on.
activity-prizes-code = Code:{" "}
activity-prizes-unreadable = This code cannot be decrypted right now. Please contact the lottery's author.
activity-prizes-revised = The author corrected this code ({ $time }).
activity-prizes-item = Other prize: the site does not deliver it, so contact the author as the note says.
activity-prizes-empty = No redemption codes or other prizes yet.
activity-prizes-earlier = Earlier prizes
# The member's growth history.
activity-growth-title = Growth history
activity-growth-description = Every change in your growth
activity-growth = Growth
# $remaining is how much growth is still missing, $next the growth the next level needs.
activity-growth-next = { $remaining } to the next level ({ $next })
# $link_html links to the points rules.
activity-growth-intro = Growth comes only from contributing content. Every change is recorded here and cannot be altered. See { $link_html } for the rules.
activity-growth-after = now { $value }
activity-growth-view-post = View post
activity-growth-empty = No growth history yet.
activity-ledger-earlier = Earlier entries
# The member's points page; $credit is the site's name for its points.
activity-credits-description = Your { $credit } balance and permanent points history
# $available_html and $frozen_html are the amounts in bold.
activity-credits-balance = { $available_html } { $credit } available · { $frozen_html } { $credit } frozen for activities
activity-credits-checked-in = You have checked in today (by calendar day in the site time zone { $zone }).
activity-credits-check-in = Check in today
activity-credits-signin-off = Check-in rewards are not turned on.
activity-credits-frozen-note = Frozen { $credit } are set aside for activity rewards; whatever is unpaid when an activity ends early or expires goes back to your available balance.
activity-credits-growth = Growth
activity-credits-level-progress = Level progress
activity-credits-next = { $remaining } more growth to the next level ({ $next })
activity-credits-top-level = You are at the highest level.
activity-credits-today = Earned today
activity-credits-none = None
activity-credits-day-note = Counted by calendar day in the site time zone { $zone }.{" "}
activity-credits-signed-in = Check-in processed. The points history shows the reward.
activity-signin-stats = Check-in stats
activity-signin-streak = Current streak
activity-signin-longest = Longest streak
activity-signin-month = This month
activity-signin-total = All time
activity-signin-days =
    { $count ->
        [one] { $count } day
       *[other] { $count } days
    }
activity-signin-times =
    { $count ->
        [one] { $count } check-in
       *[other] { $count } check-ins
    }
# The month of the check-in calendar; $year and $month are numbers.
activity-signin-caption = { $year }-{ $month }
# Weekdays of the check-in calendar: the full name and the column heading.
activity-weekday-mon = Monday
activity-weekday-mon-short = Mon
activity-weekday-tue = Tuesday
activity-weekday-tue-short = Tue
activity-weekday-wed = Wednesday
activity-weekday-wed-short = Wed
activity-weekday-thu = Thursday
activity-weekday-thu-short = Thu
activity-weekday-fri = Friday
activity-weekday-fri-short = Fri
activity-weekday-sat = Saturday
activity-weekday-sat-short = Sat
activity-weekday-sun = Sunday
activity-weekday-sun-short = Sun
activity-signin-checked = Checked in
activity-signin-note = Counted by calendar day in the site time zone { $zone }; a streak counts up to yesterday until you check in today. Making up missed days and the check-in message board are not available yet.
# One row of the points history.
activity-ledger-available = Available
activity-ledger-frozen = Frozen
activity-ledger-after = now { $available } available / { $frozen } frozen
activity-ledger-thread = View thread #{ $tid }
activity-ledger-empty = No points history yet.
# The kind of a points history row; $activity is the activity type (bounty, red packet, lottery) or "Activity".
activity-ledger-activity = Activity
activity-ledger-signin = Check-in reward
activity-ledger-thread-reward = Thread reward
activity-ledger-reply-reward = Reply reward
activity-ledger-adjust = Adjusted by an administrator
activity-ledger-reserve = { $activity } budget frozen
activity-ledger-refund = { $activity } refund
activity-ledger-payout = { $activity } payout
activity-ledger-reward = { $activity } prize
activity-ledger-tip-out = Tipped a thread
activity-ledger-tip-in = Received a tip
activity-ledger-other = Points change
activity-error-rate = Too many requests. Please try again later
activity-error-lottery = The lottery does not exist or you cannot view it
activity-codes-replaced = The codes have been replaced.
activity-codes-done = The code has been corrected.
# Sources of points earned today.
activity-gain-reward = Activity prizes
activity-gain-refund = Activity refunds
activity-gain-other = Other
# The kind of a growth history row.
activity-growth-publish = Posted
activity-growth-like = Liked
activity-growth-unlike = Like removed
activity-growth-digest = Featured
activity-growth-undigest = No longer featured
activity-growth-accept = Accepted answer
activity-growth-revoke = Taken back (hidden or withdrawn)
activity-growth-restore = Given back (restored)
# Errors of activity actions.
activity-error-category = Choose a valid thread type
activity-error-deadline = The activity deadline must be in the future
# $credit is the site's name for its points.
activity-error-budget = The activity budget must be between 1 and 1,000,000,000 { $credit }
activity-error-shares = The number of shares must be between 1 and 10,000 and no more than the budget
activity-error-distribution = Invalid red packet split
activity-error-divisible = The budget of an equal red packet or a lottery must divide evenly by the number of shares
activity-error-poll-options = A poll needs 2 to 20 options
activity-error-poll-max = Invalid number of choices for the poll
activity-error-poll-option = Poll options cannot be empty, repeated or longer than 255 characters
activity-error-news-url = Enter an HTTP or HTTPS link to the news source
activity-error-news-link = Invalid news source link
activity-error-news-fields = News needs a source name and an author, each up to 255 characters
activity-error-entry-mode = Choose how to enter the lottery
activity-error-requirements-mode = Only one-click lotteries can have entry requirements
activity-error-requirements-count = A lottery can have up to 10 entry requirements
activity-error-requirement = Each entry requirement must be 1 to 200 characters
activity-error-prizes-count = A lottery needs 1 to 10 prizes
activity-error-prize-name = Prize names must be 1 to 60 characters
activity-error-prize-note = A prize note cannot be longer than 200 characters
activity-error-prize-winners = Each prize needs 1 to 10,000 winners
activity-error-prize-amount = A { $credit } prize must give 1 to 1,000,000,000 { $credit } per winner
activity-error-prize-total = { $credit } prizes cannot add up to more than 1,000,000,000 { $credit }
activity-error-prize-codes = A code prize needs exactly one code per winner
activity-error-code-format = A code must be 1 to 255 letters, digits or symbols, without quotes or backslashes
activity-error-code-duplicate = Codes cannot repeat within one lottery
activity-error-prize-item = Other prizes give no { $credit } and carry no code
activity-error-prize-kind = Invalid prize type
activity-error-winners-total = There cannot be more than 10,000 winners in total
activity-error-non-credit = Code prizes and other prizes cannot have more than 200 winners in total
activity-error-member = The account does not exist
activity-error-sign-in = Please sign in again
activity-error-session = Your session has expired or your account is frozen
activity-error-api-key = The API token was revoked or has expired
activity-error-funding = Your account is being deleted, so it cannot start paid activities or receive rewards for now
activity-error-accept-request = Invalid accept request
activity-error-reply = The reply does not exist
activity-error-accept-owner = Only the bounty's author can accept other members' replies
activity-error-bounty = The bounty does not exist
activity-error-not-your-bounty = This thread is not your bounty
activity-error-request-reused = This request ID was already used for another action
activity-error-thread = The activity thread cannot be accessed
activity-error-bounty-ended = The bounty has ended
activity-error-accept-valid = Only a reply that is currently valid can be accepted
activity-error-recipient = This reply's author cannot receive activity rewards right now
activity-error-vote-choice = Choose a poll option
activity-error-poll = The poll does not exist
activity-error-not-poll = This thread is not a poll
activity-error-voted = You have already voted. Each account votes once and cannot change its vote
activity-error-poll-closed = The poll has ended or cannot be accessed
activity-error-vote-max = You chose more options than this poll allows
activity-error-vote-option = The poll option does not exist
activity-error-identity = The activity must match the thread's type and author
activity-error-remaining = The unpaid activity budget is out of range
activity-error-packet-shares = The remaining red packet shares are out of range
activity-error-packet-amount = A red packet amount must be positive
activity-error-ledger-reused = This request was already used for another points change
activity-error-transfer = Invalid reward recipient or amount
activity-error-signin-off = Check-in rewards are not turned on
# Balance errors; $credit is the site's name for its points.
activity-error-deleting = An account being deleted cannot change its { $credit } balance
activity-error-balance-range = The { $credit } balance is out of range
activity-error-frozen-range = The frozen { $credit } are out of range
activity-error-insufficient = Not enough { $credit } available
# The stored reason of system points history rows: shown by kind, in the viewer's language.
activity-reason-signin = Daily check-in reward
activity-reason-thread = Reward for posting a thread
activity-reason-reply = Reward for posting a reply
activity-reason-reserve = Activity budget frozen
activity-reason-payout = Activity prize paid out
activity-reason-reward = Activity prize received
activity-reason-refund = Unpaid activity budget refunded
activity-reason-tip-out = Tipped a thread
activity-reason-tip-in = Received a tip
activity-error-not-lottery = This thread is not a lottery
activity-error-codes-save = Codes cannot be saved right now. Please try again later
activity-error-no-lottery = The lottery does not exist
activity-error-not-click = This lottery has no one-click entry; a valid reply enters automatically
activity-error-own-lottery = Authors cannot enter their own lottery
activity-error-lottery-closed = The lottery has closed or cannot be accessed
activity-error-confirm = Please confirm every entry requirement
activity-error-entrant = The entrant does not exist
activity-error-exclude-reason = Enter a reason of 1 to 200 characters for the removal
activity-error-review-owner = Only the lottery's author can review entries
activity-error-no-review = This lottery does not use author review
activity-error-locked = The draw has started and the entries are locked
activity-error-not-entered = This member has not entered
activity-error-draw-owner = Only the lottery's author can draw
activity-error-no-early-draw = This lottery cannot be drawn early; it draws automatically at the deadline
activity-error-draw-closed = The lottery has closed and will be drawn automatically
activity-error-codes-owner = Only the lottery's author can change the codes
activity-error-codes-drawn = The draw has started; codes already given out can only be corrected one by one
activity-error-prize = The prize does not exist
activity-error-not-code-prize = This prize is not a code prize
activity-error-code-count = The number of codes must equal the winners, one code per winner
activity-error-code-slots = The code slots do not match
activity-error-code-slot = The code slot does not exist
activity-error-not-code-slot = This slot is not a code
activity-error-code-unsent = This code has not been given out yet. Before the draw, replace the codes as a whole
activity-error-code-revised = The code was just corrected. Please reload before changing it
# Errors in the prize field of the editor; $row is the prize's line number, the type words are the ones this language accepts.
activity-error-prize-line = Write prize { $row } as “name | winners | type | { $credit } each | note”
activity-error-prize-line-winners = The winners of prize { $row } must be a number
activity-error-prize-line-kind = The type of prize { $row } can only be { $credit }, { $code } or { $item }
activity-error-prize-line-amount = The { $credit } each of prize { $row } must be a number
activity-error-prize-line-codes = There are more codes than code prize winners; one code per winner
activity-error-tip-request = Invalid tip request. Please reload the page
activity-error-tip-own = You cannot tip your own thread
activity-error-tip-off = Tipping is not turned on
activity-error-tip-amount = Choose a valid tip amount
activity-error-tip-recipient = The thread's author cannot receive tips right now
activity-error-tip-withdrawn = The thread was withdrawn and cannot be tipped
activity-error-tip-blocked = This thread cannot be tipped right now
activity-error-tipped = You have already tipped this thread
activity-error-tip-daily = You have reached today's tipping limit
# The reason of the points granted to test accounts by the command-line activity plan (shown like an administrator's).
activity-reason-seed = Initial points for the test environment
# The points page of the admin area; $credit is the site's name for its points.
activity-manage-heading = { $credit } rules and adjustments
# $zone is the site time zone, such as UTC+8.
activity-manage-rules-intro = Rules start out off. Daily limits count per calendar day in the site time zone { $zone }; when what is left of the day's limit is less than a full award, nothing is awarded that time.
activity-manage-rule-enabled = Enable rewards
activity-manage-rule-amount = { $credit } per award
activity-manage-rule-daily = Daily { $credit } limit
# $rule is the name of the rule.
activity-manage-rule-save = Save rule: { $rule }
activity-manage-tips = Thread tips
activity-manage-tips-intro = Members can only tip the starter of a thread, never themselves; once per thread per member, the author receives the full amount, and tips cannot be taken back. Daily limits count per calendar day in the site time zone { $zone }.
activity-manage-tips-enabled = Enable tips
activity-manage-tip-amounts = Amounts (comma-separated, small to large, at most { $max })
activity-manage-tip-daily = Tips per member per day ({ $credit })
activity-manage-tips-save = Save tip settings
activity-manage-adjust = Adjust a member's { $credit }
activity-manage-uid = Member UID
activity-manage-amount = Amount (positive to add, negative to deduct)
activity-manage-reason = Reason
activity-manage-adjust-note = Adjustments stay in the ledger permanently and cannot deduct balances frozen for activities.
activity-manage-adjust-submit = Adjust and record
activity-manage-growth-save = Save growth settings
activity-manage-growth-adjust = Adjust a member's growth points
# $rules_html links to the points rules page.
activity-manage-growth-note = Growth points are separate from { $credit }. Adjustments stay in the growth ledger and the action log permanently and cannot go below 0. See { $rules_html } for the rules.
activity-manage-growth-submit = Adjust growth points
activity-manage-error-admin = Only administrators can manage { $credit }
activity-manage-title = { $credit } management
activity-manage-description = Reward rules and ledger adjustments
activity-manage-saved = Saved.
activity-manage-growth-adjusted = Growth points adjusted.
activity-manage-growth-saved = Growth settings saved.
activity-manage-error-growth-admin = Only administrators can adjust growth points
activity-manage-error-growth-input = An adjustment needs the target account, a non-zero whole number of growth points (at most 1000000 either way) and a reason of up to 200 characters
activity-manage-error-growth-account = Only active accounts' growth points can be adjusted
activity-manage-error-growth-reused = This request was already used for another growth point adjustment
activity-manage-error-growth-negative = Growth points cannot go below 0
activity-manage-error-rule-amounts = The amount per award and the daily { $credit } limit of a reward rule must be positive whole numbers
activity-manage-error-rule-limit = The daily { $credit } limit cannot be smaller than the amount per award, or nothing could ever be awarded
activity-manage-error-rule-stale = The reward rule was changed meanwhile; refresh and try again
activity-manage-error-adjust-input = An adjustment needs the target account, a non-zero whole amount and a reason
activity-manage-error-adjust-deleting = Accounts being deleted cannot have their { $credit } balance changed
activity-manage-error-rules-admin = Only administrators can adjust { $credit } rules and balances
