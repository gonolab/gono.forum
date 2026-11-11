## The member's own settings: account security, profile, reading preferences, personal style, personal feeds and the
## API token (templates/settings/).

settings-password-title = Change password
settings-password-lead = After the change, this device and all others are logged out. Log in again with the new password.
settings-current-password = Current password
settings-password-submit = Change password and log out
settings-password-forgot = Forgot your current password?
settings-set-password = Set a password
settings-set-password-lead-mobile = Your account has no password yet; you log in with your phone number and a text message code. With a password you can also log in with your username. Changing the phone number, linking or unlinking other accounts and turning on two-step verification all need a password first. Once it is set, every device is logged out.
settings-set-password-lead-external = Your account has no password yet; you log in with another account. With a password you can also log in with your username. Changing the phone number, linking or unlinking other accounts and turning on two-step verification all need a password first. Once it is set, every device is logged out.
# $provider is Google or GitHub.
settings-external-confirmed = Identity confirmed with { $provider }. Please set a password within 10 minutes.
settings-set-password-submit = Set password and log out
settings-code-resend = Send the code again
# $mobile is the masked phone number.
settings-code-send-to = Send a code to { $mobile }
settings-external-confirm = Confirm with { $provider }
settings-mobile-bound = { $mobile } is linked; you can log in with a text message code.
settings-mobile-none = Not linked. Add a phone number to log in with a text message code.
settings-mobile-confirm-change = Confirm the change
settings-confirm-link = Confirm linking
settings-new-mobile = New phone number
settings-mobile-hint = An 11-digit mainland China mobile number; the code goes to this number
settings-mobile-unbind = Remove phone number
settings-mobile-unbind-lead = After removing it you can no longer log in by text message. Accounts without an email cannot remove it.
settings-mobile-need-password-change = Set a password above first, then change the phone number.
settings-mobile-need-password-link = Set a password above first, then add a phone number.
settings-mobile-verify-email = Verify your email first, then add a phone number.
settings-external = Linked accounts
# $provider is Google or GitHub; $account is the name of that account.
settings-external-pending = Confirm linking the { $provider } account { $account } to this account.
# $time is when the account was linked.
settings-external-bound = { $provider }: linked to { $account } (since { $time }); you can log in with it.
settings-external-unlink = Unlink { $provider }
settings-external-none = { $provider }: not linked. Link it to log in with it.
settings-external-link = Link a { $provider } account
settings-external-need-password = Set a password above first, then link or unlink other accounts.
settings-external-verify-email = Verify your email first, then link other accounts.
# $time is when two-step verification was turned on.
settings-two-factor-enabled = On (since { $time }). After the password, logging in also needs the 6-digit code from your authenticator app or a recovery code.
settings-recovery-remaining =
    { $count ->
        [one] 1 recovery code left.
       *[other] { $count } recovery codes left.
    }
settings-recovery-remaining-low =
    { $count ->
        [one] 1 recovery code left; consider generating new ones.
       *[other] { $count } recovery codes left; consider generating new ones.
    }
# $time is when the lock ends.
settings-two-factor-locked = Too many wrong codes in a row; locked until { $time }. If this was not you, change your password.
settings-two-factor-disabled = Off. Once on, logging in needs a code from the authenticator app on your phone besides the password, so the password alone is not enough. Resetting the password does not turn it off.
settings-two-factor-need-password = Set a password first to turn on two-step verification.
settings-two-factor-verify-email = Verify your email first to turn on two-step verification.
settings-recovery-regenerate = Generate new recovery codes
settings-recovery-regenerate-lead = Generate a new set of recovery codes; all the old ones stop working. The new codes are shown only once.
settings-two-factor-disable = Turn off two-step verification
settings-two-factor-disable-lead = Once off, logging in needs only the password, and all recovery codes stop working. You need your current password and a code or recovery code.
settings-code-or-recovery = Code or recovery code
settings-two-factor-enable = Turn on two-step verification
settings-two-factor-enable-lead = You need a phone with an authenticator app. Confirm your current password to see the QR code.
settings-logout-all = Log out everywhere
settings-logout-all-lead = Log this account out of every browser and device and revoke the API token. You will need to log in again, and programs that use the API need a new token.
settings-two-factor-step-scan = 1. Scan the QR code below with an authenticator app (such as Google Authenticator, Microsoft Authenticator or 1Password).
# $link_html adds the account to the authenticator app on this device; $username_html is the account name to type in.
settings-two-factor-manual = Using an authenticator on this device? { $link_html }. If you cannot scan, choose manual entry in the app, enter { $username_html } as the account, choose "Time based" as the type, and enter this key:
settings-two-factor-add-link = Add it to the authenticator app directly
settings-two-factor-step-code = 2. Enter the 6-digit code the app shows; two-step verification turns on only after you confirm. Please finish within 10 minutes. Once it is on, other devices are logged out and this one stays logged in.
settings-two-factor-confirm = Confirm and turn on
settings-cancel = Cancel
settings-recovery-codes = Recovery codes
settings-recovery-empty = Recovery codes are shown only once, when they are generated. If you did not save them, generate a new set on the account security page; the old ones then stop working.
settings-recovery-save =
    Save the { $count ->
        [one] recovery code
       *[other] { $count } recovery codes
    } below somewhere safe, such as a password manager or on paper. If you lose your phone or cannot use the authenticator, log in with one of them instead of a code; each works once. They are not shown again after you leave or refresh this page.
settings-back-to-security = Back to account security
settings-api-lead = Once it is on, programs can post threads and replies, add favorites and like posts as you. The address is the same for every member; the token is yours alone, so keep it secret.
# A label before the address in a code block.
settings-api-endpoint = API address{": "}
settings-api-secret = Your token (shown only this once; it will not appear after a refresh):
settings-api-example = Example:
# Placeholder values inside the example request.
settings-api-example-key = any unique string
settings-api-example-title = Title
settings-api-example-body = Body
settings-api-expired = The token has expired. Please generate a new one.
settings-api-on = The API is on.
# A label before the start of a token in a code block.
settings-token = Token{": "}
# $time is when the token was created.
settings-created = Created { $time }
settings-api-expires = Expires { $time }
# $time is when the token was last used, or a word saying it was never used.
settings-api-last-used = Last used: { $time }
settings-api-off = The API is off.
settings-api-site-disabled = This site has turned the API off, so tokens cannot be created for now.
# $link_html links to the email verification page.
settings-api-need-email = Verify your email to turn on the API. { $link_html }
settings-api-rotate = Generate a new token
settings-api-rotate-lead =
    The old token stops working at once; the new one is valid for { $days ->
        [one] { $days } day
       *[other] { $days } days
    }.
settings-api-enable = Turn on the API
settings-api-enable-lead =
    Create a personal token valid for { $days ->
        [one] { $days } day
       *[other] { $days } days
    }. Posting limits, email verification and read-only rules are the same as on the website.
settings-api-enable-submit = Turn on and create a token
settings-api-disable = Turn off the API
settings-api-disable-lead = Once it is off, the token stops working at once; what you have posted stays.
settings-avatar-check = Avatar check
settings-avatar-ready = The image passed the check. Confirm to replace your current avatar.
settings-avatar-new-alt = New avatar waiting for confirmation
settings-avatar-use = Use this avatar
settings-avatar-type = Please choose a PNG, JPEG, GIF or WebP image.
settings-avatar-rejected = The image did not pass the check; your current avatar stays.
settings-avatar-processing = Checking the image. Refreshing the status does not upload it again.
settings-avatar-waiting = The image is uploaded and waiting for the check. Refreshing the status does not upload it again.
settings-avatar-refresh = Refresh the check status
settings-back-to-profile = Back to your profile
settings-feed-lead = A personal feed address lets a reader follow threads with your settings: the whole-site feed leaves out the forums you hid or blocked and the members you blocked. Each item has only the title and a summary, not the full text.
settings-feed-secret-lead = The address itself is the credential: anyone who has it can read this feed without logging in. Do not forward or publish it; if you think it leaked, generate a new one or turn it off. The address appears in the access logs of the reader service and this site.
settings-feed-addresses = Your feed addresses (shown only this once; they will not appear after a refresh):
settings-feed-on = Feed addresses are on.
settings-feed-with-members = Includes members-only threads
settings-feed-public-only = Public threads only
settings-feed-off = Feed addresses are off.
settings-feed-members = Members-only threads
settings-feed-members-lead = When this is on, the feeds also show threads only logged-in members can see (title and summary), and the single-forum addresses of members-only forums start to have items. If the address is forwarded or given to a third-party reader, whoever has it sees these threads too.
settings-feed-include-members = Include members-only threads
settings-save = Save
settings-feed-rotate = Regenerate
settings-feed-rotate-lead = The old addresses stop working at once, and "Include members-only threads" is turned off again.
settings-feed-rotate-submit = Generate new feed addresses
settings-feed-disable = Turn off
settings-feed-disable-lead = Once off, the old addresses stop working at once. Turning it on again gives new addresses.
settings-feed-disable-submit = Turn off feed addresses
settings-feed-enable = Turn on feed addresses
settings-feed-enable-lead = Create addresses that belong only to you: one for the whole site and one for each forum. By default they include public threads only.
settings-feed-enable-submit = Turn on and create addresses
# $cooldown is how long to wait between changes, such as "30 days".
settings-username-hint = 1 to 50 characters, without spaces, slashes or @. You can change it once every { $cooldown }.
settings-username-next = Next change possible: { $time }
settings-bio = Bio
settings-bio-hint = Up to 500 characters.
settings-profile-save = Save profile
settings-view-profile = View public profile
settings-privacy = Privacy
settings-space-visibility = Who can see your profile page
settings-visibility-public = Everyone
settings-visibility-members = Logged-in members
settings-visibility-private = Only me
settings-privacy-save = Save privacy settings
settings-avatar = Avatar
settings-avatar-lead = Once the image passes the check, confirm to use it. If the upload fails or you do not confirm, your current avatar stays.
settings-avatar-choose = Choose an avatar
# $size is the upload limit in megabytes.
settings-avatar-hint = PNG, JPEG, GIF or WebP, up to { $size } MB.
settings-avatar-upload = Upload and check
# $link_html links to the email verification page.
settings-avatar-need-email = Verify your email to upload an avatar. { $link_html }
settings-avatar-remove = Use the default avatar
settings-email-change = Change email
settings-current-email = Current email: { $email }. Your email is never shown publicly.
settings-pending-email = New email waiting for confirmation: { $email }
settings-email-change-lead = Once you confirm the new email, every device is logged out. Until then you keep using the current one.
settings-new-email = New email
settings-email-change-submit = Send confirmation link
settings-reading-lead = Choose how thread lists and replies are shown by default. These settings belong to your account and apply on any device you log in on; options set to "Follow the site" change with the site's defaults.
settings-reading-sort = Default order of the home page and forums
# $current is the site's current default, one of the option names.
settings-follow-site = Follow the site (now { $current })
settings-sort-latest = Latest replies
settings-sort-created = Newest threads
settings-reading-sort-hint = Used when you open the home page or a forum directly; the order tabs still switch it.
settings-reading-view = Default reply view
settings-view-flat = Flat
settings-view-nested = Two levels
settings-reading-view-hint = Used when you open a thread from lists, notifications and reply links. The thread page can still switch it.
settings-reading-title-click = Clicking a thread title in lists
settings-title-latest = Jump to the latest reply
settings-title-first = Open the first page
settings-reading-title-hint = Applies to the thread lists of the home page, forums and search; threads without replies open on the first page.
settings-reading-title-tab = Where thread titles in lists open
settings-tab-new = New tab
settings-tab-current = Same tab
settings-reading-avatar = Avatar shape
settings-shape-circle = Circle
settings-shape-rounded = Rounded square
settings-reading-avatar-hint = This changes only the avatars you see; others see them unchanged.
settings-reading-zone = Times on pages always follow the site time zone.
settings-reading-save = Save reading preferences
settings-device = This device
settings-device-lead = Saved only in this browser, not with your account.
settings-emoji-zoom = Enlarge emoji in posts on mouse hover
settings-style-lead = Your personal style applies only while you browse; nobody else sees it, including visitors to your profile. This page, the admin area, account security, login, two-step verification and similar pages do not use it, so you can always come back here to fix it.
# $max is the size limit in KiB.
settings-style-max = Up to { $max } KiB.
settings-style-save = Save personal style
settings-style-reset = Reset (clear the personal style)
settings-style-rules = Rules
# The rule list of the personal style; the *_html slots are CSS keywords in code tags.
settings-style-rule-size = Up to { $max } KiB and 1000 rules; { $media_html } is allowed but cannot be nested.
settings-style-rule-resources = Nothing can be loaded, not even from this site: { $url_html }, { $image_set_html }, { $import_html }, { $font_face_html } and at-rules other than { $media_html } are not allowed.
settings-style-rule-values = { $fixed_html } is not allowed; { $content_html } can only be { $none_html }, { $normal_html } or an empty string; { $z_index_html } can only be { $auto_html } or -1 to 20.
settings-style-rule-functions = No backslash escapes, and none of the old browsers' script forms such as { $expression_html } or { $behavior_html }; functions in values are limited to ones that make no requests, such as colors, { $var_html }, { $calc_html }, gradients, transforms and filters.
settings-style-rule-errors = If anything breaks the rules, nothing is saved and the message points to the line. You can change the site's design variables, for example { $example_html }.
# Page description; $site is the site name.
settings-security-description = Manage the password of your { $site } account
# Notices on the account security page.
settings-notice-two-factor-disabled = Two-step verification is off, and all recovery codes stop working.
settings-notice-recovery-login = You just logged in with a recovery code, and it no longer works. Generate a new set when few are left.
settings-notice-mobile-bound = Your phone number is linked; you can log in with a text message code.
settings-notice-mobile-unbound = Your phone number is removed.
# $provider is Google or GitHub.
settings-notice-external-linked = Your { $provider } account is linked; you can log in with it from now on.
settings-notice-external-unlinked = Your { $provider } account is unlinked.
settings-error-password-too-many = Too many attempts. Please try changing your password later
settings-api-created-notice = The token is created and shown only this once. Please save it now.
settings-api-never-used = never
settings-api-title = API
settings-api-description = Manage your personal API token for { $site }
settings-api-disabled-notice = The API is off, and the old token stopped working.
settings-error-api-site-disabled = This site has turned the API off, so tokens cannot be created for now
settings-error-api-need-email = Verify your email to turn on the API
settings-error-too-frequent = Too many requests. Please try again later
settings-two-factor-description = Turn on two-step verification for your { $site } account
settings-error-stale-reenter = The form has expired. Please fill it in again and submit
settings-notice-two-factor-enabled = Two-step verification is on, and other devices are logged out.
settings-notice-recovery-regenerated = New recovery codes are generated, and all the old ones stop working.
settings-recovery-description = Two-step verification recovery codes for { $site }
settings-reading-title = Reading preferences
settings-reading-description = Choose how thread lists and replies are shown by default
settings-reading-saved = Reading preferences saved
settings-error-reading-version = Invalid reading preference version
settings-error-reading-sort = The default order can only be latest replies or newest threads
settings-error-reading-view = The reply view can only be two levels or flat
settings-error-reading-option = Invalid reading option
settings-error-reading-avatar = The avatar shape can only be rounded square or circle
settings-error-save-too-many = Too many saves. Please try again later
settings-style-title = Personal style
settings-style-description = Your own CSS that applies only while you browse
settings-style-saved = Personal style saved
settings-style-cleared = Personal style cleared
settings-error-style-version = Invalid personal style version
# The name of the whole-site feed in the list of personal feed addresses.
settings-feed-whole-site = Whole site
settings-feed-whole-site-note = Leaves out the forums you hid or blocked
settings-feed-members-forum-note = Members-only forum: this address works only once "Include members-only threads" is on; while it is off it returns 404
settings-feed-created-notice = Your feed addresses are created and shown only this once. Copy them into your reader now.
settings-feed-title = Feed addresses
settings-feed-description = Manage your personal RSS / Atom feed addresses
settings-error-feed-off = Feed addresses are not on yet
settings-feed-disabled-notice = Feed addresses are off, and the old addresses stopped working.
settings-feed-saved-notice = Saved.
settings-error-visibility = Please choose a valid profile visibility
settings-error-username-reserved = This username is still protected. Please choose another one
settings-error-bio = The bio can have at most 500 characters and no control characters
settings-error-muted = While muted you cannot change your username or bio
settings-error-username-cooldown = The username was changed recently. Please try again later
settings-error-username-taken = This username is taken. Please choose another one
settings-error-email-same = The new email must differ from the current one
settings-error-email-taken = This email is in use. Please choose another one
settings-error-setup-expired = The setup has timed out. Please start turning on two-step verification again
settings-error-setup-code = Wrong code. Please check that your phone's clock is correct and try again
# The accessible name of the QR code image.
settings-two-factor-qr-label = Authenticator QR code
settings-error-two-factor-on = Two-step verification is already on
settings-error-two-factor-off = Two-step verification is not on
# Plain-text bodies of avatar image requests that fail (a line break is added).
settings-error-avatar-unavailable = The avatar is temporarily unavailable
settings-error-avatar-missing = The avatar does not exist
settings-error-avatar-read = The avatar could not be read
# $site is the site name.
settings-profile-description = Manage your { $site } profile, email and avatar
settings-no-email = No email set yet
settings-profile-saved = Profile saved
settings-privacy-saved = Privacy settings saved
settings-email-sent = A confirmation link was sent to the new email. Until you confirm, the current email stays in use.
settings-avatar-saved = Avatar updated
settings-avatar-removed = The default avatar is back
settings-error-stale-check = The form has expired. Please check your entries and try again
settings-error-stale-check-reason = The form has expired. Please check your entries and try again. Reason: { $reason }
settings-error-stale-current-password = The form has expired. Please enter your current password again and submit
settings-error-stale-avatar = The form has expired. Please choose the image again and submit. Reason: { $reason }
settings-error-avatar-need-email = Verify your email to upload an avatar
settings-error-upload-format = Invalid upload format
settings-error-upload-fields = Too many upload fields
settings-error-upload-token-order = The form token was not sent before the image
settings-error-upload-one = Please choose one image at a time
settings-error-avatar-too-many = Too many avatar uploads. Please try again later
settings-error-avatar-interrupted = The avatar upload was interrupted
settings-error-avatar-size = The avatar is larger than the image upload limit
settings-error-avatar-staging = The avatar could not be stored temporarily
settings-error-upload-field = Invalid upload field
settings-error-upload-field-long = An upload field is too long
settings-error-upload-utf8 = Please use UTF-8
settings-error-avatar-choose = Please choose an avatar image
settings-avatar-check-description = Check the avatar image and confirm using it
# Why a personal style is rejected; $line is the line number, $reason one of the reasons below.
settings-css-line = Line { $line }: { $reason }
# $max is the size limit in KiB.
settings-css-too-long = A personal style can be at most { $max } KiB
settings-css-backslash = Backslash escapes are not allowed
settings-css-control = Control characters are not allowed
settings-css-syntax = The syntax is incomplete; check that brackets and quotes come in pairs
settings-css-nested-at = No at-rules are allowed inside @media
# $name is the at-rule name without @.
settings-css-at-rule = @{ $name } is not allowed; only @media is
settings-css-rules = At most { $max } rules
# $max is how many levels of brackets and functions may nest inside one @media condition, selector or value.
settings-css-nesting = Brackets and functions can be nested at most { $max } levels deep
settings-css-media-condition = An @media condition can only contain media features
settings-css-attribute-selector = Invalid attribute selector
# $name is a CSS function name.
settings-css-selector-function = { $name }() is not allowed in selectors
settings-css-no-selector = A selector is missing
settings-css-selector = Invalid selector
settings-css-expected-property = A property name is expected here; nested rules are not supported
# $property is a CSS property name.
settings-css-property = The { $property } property is not allowed
settings-css-important-last = !important can only come at the end of the value
settings-css-bang = ! can only be followed by important
settings-css-no-value = { $property } has no value
settings-css-position = position: fixed is not allowed; position can only take fixed keywords
settings-css-content = content can only be none, normal or an empty string
settings-css-z-index = z-index can only be auto or a whole number from -1 to 20
settings-css-url = url() is not allowed
settings-css-function = { $name }() is not allowed
settings-css-value-at = @ is not allowed in values
settings-css-value = Invalid value
settings-error-style-invalid = The personal style breaks the rules
settings-error-style-conflict = The personal style was changed elsewhere. Please refresh and try again
settings-error-style-version-range = The personal style version is out of range
settings-error-reading-conflict = Your reading preferences were changed elsewhere. Please refresh and try again
settings-error-reading-version-range = The reading preference version is out of range
# Bulk API token provisioning (command line): the same uid was given twice.
settings-error-duplicate-uids = The uid list has duplicates
