## Signing in, signing up, password recovery, email verification, invites and two-step verification at login
## (templates/auth/).

account-sms-login = Text message login
account-tabs = Account
account-registration-closed = Sign-ups are closed for now.
account-invite = Invite code
account-invite-required = This site is invite-only and needs a valid invite code
account-invite-optional = Sign-ups are open; an invite code is optional
account-username = Username
account-username-hint = 1 to 50 characters, without spaces, slashes or @
account-email = Email
account-email-hint = Never shown publicly. Verify it to create invites and post
account-password = Password
# $min is the site's minimum password length.
account-password-hint = { $min } to 128 characters, case-sensitive
account-password-confirm = Confirm password
# The sign-up consent checkbox; the two slots link to the terms of use and the privacy notice.
account-consent = I have read and agree to the { $terms_html } and the { $privacy_html }
account-consent-terms = terms of use
account-consent-privacy = privacy notice
account-login-name = Username or email
account-forgot = Forgot your password?
account-data-request = My data requests
account-appeal = Appeals
# $provider is Google or GitHub.
account-provider-register = Sign up with { $provider }
account-provider-login = Log in with { $provider }
account-register-notes = About signing up
account-login-notes = About logging in
account-note-invite-once = Each invite code can register one account.
# $link_html links to signing up with a phone number.
account-note-mobile = You can also { $link_html }: no password, and no email needed.
account-note-mobile-link = sign up with a phone number
account-note-external = Signing up with another account sets no password and uses the email that account verified.
account-note-verify = After signing up, verify your email through the email we send you.
# The three slots link to the community rules, the terms of use and the privacy notice.
account-note-read = Before signing up, please read the { $rules_html }, the { $terms_html } and the { $privacy_html }.
account-note-login-name = You can log in with your username or your email.
# $link_html links to logging in with a text message code.
account-note-sms = Accounts with a phone number can also { $link_html }.
account-note-sms-link = log in with a text message code
account-note-external-login = If your other account is not linked to an account here yet, authorize it and choose a username to sign up. If its email is already registered, log in with your password first and link it under account security.
account-note-case = Passwords are case-sensitive; usernames are not.
account-note-public-device = Log out after using a shared device.
account-forgot-title = Reset your password
account-forgot-lead = Enter the email you signed up with. We will send a reset link to it.
account-forgot-sent = If an account with this email can reset its password, we will send a reset link. Please check your inbox and spam folder.
account-forgot-submit = Send reset link
account-reset-title = Reset password
# $ttl is how long the link is valid, such as "30 minutes".
account-reset-lead = The reset link is valid for { $ttl } after it is sent and works only once. After the reset, every device is logged out and your API token is revoked.
account-reset-claim = This account's email has not been verified yet. Resetting the password verifies it and removes what was set up on the account before: linked Google or GitHub accounts, the phone number and two-step verification.
account-new-password = New password
account-new-password-confirm = Confirm new password
account-reset-again = Ask for a new reset link
account-verify-confirm = Confirm email verification
# $email_html is the email address in bold.
account-verify-to = The verification email goes to { $email_html }.
account-verify-resend = Send the verification email again
account-verify-invites = Manage my invites
account-verify-login = Log in
account-email-change-title = Confirm your new email
# $email_html is the new address in bold; $ttl how long the link is valid.
account-email-change-lead = Change your account email to { $email_html }. The confirmation link is valid for { $ttl } after it is sent and works only once.
account-email-change-note = After you confirm, every device is logged out. Log in again with the new email or your username.
account-email-change-submit = Confirm the new email
account-email-change-again = Back to your profile to ask again
account-invites-lead = Each invite code works once and is valid for { $ttl } after it is created.
# The slots are numbers in bold: how many codes can still be created today, and how many more active codes are allowed.
account-invites-quota = You can create { $daily_html } more today, and have { $active_html } more active codes.
account-invites-create = Create invite code
account-invites-list = My invite codes
account-invites-item = Invite #{ $id }
account-invites-copy = Copy
account-invites-expires = Expires: { $time }
account-invites-revoke = Revoke
account-invites-empty = No invite codes yet
account-invites-pages = Invite pages
account-mail-paused = This site has paused sending email. Emails will go out once sending resumes.
# $mobile is the masked phone number.
account-sms-sent = The code was sent to { $mobile }.
account-code = Code
account-sms-code-hint =
    The 6 digits in the text message, valid for { $minutes ->
        [one] { $minutes } minute
       *[other] { $minutes } minutes
    }
account-sms-resend-toggle = No code, or a different number?
account-mobile = Phone number
account-sms-interval-hint =
    One code per number every { $seconds ->
        [one] { $seconds } second
       *[other] { $seconds } seconds
    }
account-sms-resend = Get a new code
# Text message login works with mainland China numbers only.
account-mobile-hint = An 11-digit mainland China mobile number
account-sms-send = Get code
account-sms-password-login = Log in with username or email
account-sms-notes = About text message login
account-sms-note-how = Enter your phone number to get a code, then log in with it.
account-sms-note-register = If the number is not registered yet, choose a username after verifying it to finish signing up.
account-sms-note-closed = Sign-ups are closed for now; only accounts with a phone number can log in by text message.
account-sms-note-daily =
    Each number can get at most { $count ->
        [one] { $count } code
       *[other] { $count } codes
    } a day.
account-sms-note-two-factor = Accounts with two-step verification may also need a code from the authenticator app.
account-finish-signup = Finish signing up
account-mobile-verified = The phone number { $mobile } is verified. Choose a username to finish signing up.
account-mobile-email-hint = Optional and never shown publicly. If you fill it in, verify it by email; you can log in with your phone number either way
account-mobile-note-password = The account has no password; you log in with your phone number and a text message code. You can set a password later under account security.
account-mobile-note-time = Please finish within 30 minutes; after that you need a new code.
account-two-factor = Two-step verification
# $issuer is the account name the authenticator app shows for this site.
account-two-factor-code-hint = Open your authenticator app and enter the 6-digit code for { $issuer }
account-two-factor-submit = Verify and log in
account-two-factor-recovery-toggle = Cannot use the authenticator? Use a recovery code
account-recovery-code = Recovery code
account-recovery-code-hint = One of the 16-character recovery codes you saved when turning on two-step verification; each works once
account-recovery-submit = Log in with a recovery code
account-two-factor-restart = Enter the password again
account-two-factor-notes = About two-step verification
account-two-factor-note-why = This account has two-step verification on, so after the password you also need a code.
account-two-factor-note-refresh = The code changes every 30 seconds, and each code works once.
account-two-factor-note-time = Please finish within 5 minutes; after that you need to enter the password again.
account-two-factor-note-reset = Password recovery only resets the password; it does not turn off two-step verification.
# $provider is Google or GitHub; $account is the name of that account.
account-external-verified = Your { $provider } account { $account } is verified. Choose a username to finish signing up.
account-external-email = Email: { $email } (verified by { $provider }, never shown publicly)
account-external-email-hint = Optional and never shown publicly. If you fill it in, verify it by email; you can log in with { $provider } either way
account-external-note-password = The account has no password; you log in with { $provider }. You can set a password or add a phone number later under account security.
account-external-note-time = Please finish within 30 minutes; after that you need to authorize again.
# Why the client address could not be read from the reverse proxy header (written to the log).
account-error-no-real-ip = The request has no X-Real-IP
account-error-real-ip-repeated = X-Real-IP appears more than once
account-error-real-ip-invalid = X-Real-IP is not an IP address
# Shown when the server cannot complete an account operation for an internal reason.
account-error-unavailable = The service is temporarily unavailable
account-error-login-failed = Wrong username, email or password. Please try again
account-error-forbidden = This account cannot do this. Please check that its email is verified
account-error-not-found = The record does not exist or cannot be accessed
# Reasons a form submission failed the same-origin check, shown after "Reason:".
account-error-cross-site = The browser says this is a cross-site submission (Sec-Fetch-Site: cross-site)
# $source is the Origin or Referer of the request, $host its Host header.
account-error-origin-mismatch = The source does not match this site: the request comes from { $source }, but its Host is { $host }. If a reverse proxy rewrites Host (for example proxy_set_header Host 127.0.0.1), the two never match
account-error-no-host = The request comes from { $source } but has no Host header, so it cannot be proven to be same-origin
account-error-token-mismatch = The form token does not match this session: the page may have been open too long, or you logged out elsewhere
# $reason is one of the reasons above.
account-error-stale-refresh = The form has expired. Please refresh the page and try again. Reason: { $reason }
account-error-stale-password-reason = The form has expired. Please enter your password again and submit. Reason: { $reason }
# Page descriptions; $site is the site name.
account-login-description = Log in to { $site }
account-register-description = Sign up for { $site }
# Notices on the login page after the session ended.
account-notice-logged-out-all = All devices are logged out. Please log in again
account-notice-email-changed = Your email has changed and all devices are logged out. Log in with the new email or your username
account-notice-password-changed = Your password has changed and all devices are logged out. Log in with the new password
account-notice-password-reset = Your password has been reset and all devices are logged out. Log in with the new password
account-notice-session-ended = This device is logged out. Please log in again
account-notice-second-factor-expired = Two-step verification timed out or the login changed. Please enter your password again
account-notice-password-set = Your password is set and all devices are logged out. Please log in again
account-error-login-too-many = Too many attempts. Please try logging in later
account-error-password-mismatch = The two passwords do not match
account-error-consent = Please read and agree to the terms of use and the privacy notice first
account-error-terms-updated = The terms of use or the privacy notice changed after this page was opened. Please read them again and tick the box to agree.
account-error-register-too-many = Too many sign-up attempts. Please try again later
account-verify-description = Verify the email of your { $site } account
account-error-verify-params = The verification link is malformed
account-error-verify-invalid = The verification link is invalid or has expired
account-verify-done = Your email is verified. Please log in with that account.
account-verify-sent = The verification email is on its way again. Please check the latest email.
account-verify-admin = The administrator account can use invites now.
account-verify-already = Your email is verified.
account-verify-open-link = Please open the link in the verification email. Once it expires, you can send the email again.
account-verify-confirm-prompt = Please confirm your email verification.
account-error-verify-resend = The verification link is invalid or has expired. Please send the verification email again.
account-error-resend-wait = An email was just requested. Please wait a minute and try again.
# Shown instead of a time that cannot be displayed.
account-time-unavailable = Time unavailable
# How long a link or code stays valid, or how long to wait.
account-duration-days =
    { $n ->
        [one] { $n } day
       *[other] { $n } days
    }
account-duration-hours =
    { $n ->
        [one] { $n } hour
       *[other] { $n } hours
    }
account-duration-minutes =
    { $n ->
        [one] { $n } minute
       *[other] { $n } minutes
    }
account-duration-seconds =
    { $n ->
        [one] { $n } second
       *[other] { $n } seconds
    }
# The state of an invite code.
account-invite-used = Used
account-invite-revoked = Revoked
account-invite-frozen = Frozen
account-invite-expired = Expired
account-invite-available = Available
# Who used an invite code; $id is their user number.
account-invite-consumer = User #{ $id }
account-error-invites-need-email = Verify your email to create and see invite codes
# $level is the level name needed, $number and $name the member's current level.
account-invites-level = You can create invite codes from { $level } on; you are now Lv{ $number } ({ $name }).
account-invites-title = My invites
account-invites-description = Create and manage invite codes for { $site }
account-error-invites-page = Invalid invite page number
account-error-stale-password = The form has expired. Please enter your password again and submit
account-error-new-password-mismatch = The two new passwords do not match
account-forgot-description = Reset your { $site } password with the email you signed up with
account-error-page-params = Invalid page parameters
account-error-apply-too-many = Too many requests. Please try again later
account-reset-description = Set a new password for your { $site } account
account-error-reset-params = The reset link is malformed. Please ask for a new one
account-error-reset-invalid = The reset link is invalid or has expired. Please ask for a new one
account-error-stale-new-password = The form has expired. Please enter the new password again and submit. Reason: { $reason }
account-error-too-many = Too many attempts. Please try again later
# $time is when the lock ends.
account-error-two-factor-locked = Too many wrong codes in a row; two-step verification is locked until { $time }. If this was not you, your password has leaked: reset it through password recovery.
account-error-wrong-answer = Wrong code or recovery code. Please try again
account-error-authenticator-unavailable = The authenticator code cannot be checked right now. Please use a recovery code
account-two-factor-description = Enter the code to finish logging in to { $site }
account-error-stale-code = The form has expired. Please enter the code again. Reason: { $reason }
account-error-stale = The form has expired. Please submit it again
# The current password was wrong; the router recognizes this message to count failed attempts.
account-error-wrong-password = The current password is wrong. Please enter it again
account-error-no-password = This account has no password yet. Please set one under account security first
account-error-unauthorized = Wrong account or password, or the login has expired
account-error-verify-first = This account is not allowed to do this. Please verify your email first
account-error-no-record = The record does not exist
account-error-taken = The username, email or request ID is already in use
account-error-username = The username must be 1 to 50 characters, without whitespace, control characters, @ or path separators
# The username is the word pages show for a deleted account.
account-error-username-reserved = This name stands for deleted accounts and cannot be used as a username
account-error-email = Please enter a valid email address of up to 255 characters
account-error-password-length = The password length does not meet the requirement. Please follow the length shown on the page
account-error-registration-closed = Sign-ups are closed for now
account-error-invite-required = Please enter a valid invite code
account-error-two-factor-web = This account has two-step verification on. Please log in on the website and finish the verification
account-error-invite-invalid = The invite code is invalid or has expired
account-error-mobile-registered = This phone number is already registered. Please log in by text message
account-error-invite-request = Invalid invite request ID
account-error-invites-off = The site has stopped giving out invite codes for now
account-error-invite-level = Your growth level is not high enough to create invite codes yet
account-error-invite-daily = You have used up today's invite codes
account-error-invite-active = You have reached the limit of unused active invite codes
account-error-invite-revoke = This invite code cannot be revoked
account-error-reset-used = The reset link is invalid, used or expired. Please ask for password recovery again
account-error-credentials-version = The account's credentials version has reached its limit
account-error-email-taken = This email is already registered. Please log in with your password first, then link the account under account security
account-error-google-taken = This Google account is linked to another account
account-error-github-taken = This GitHub account is linked to another account
account-error-google-already = This account already has a Google account linked. Unlink it first, then link the new one
account-error-github-already = This account already has a GitHub account linked. Unlink it first, then link the new one
account-error-google-closed = This Google account is not linked to an account here, and sign-ups are closed for now
account-error-github-closed = This GitHub account is not linked to an account here, and sign-ups are closed for now
account-error-external-login-expired = The authorization has timed out. Please log in with the other account again
account-error-google-linked-here = This Google account is already linked to this account
account-error-github-linked-here = This GitHub account is already linked to this account
account-error-link-expired = The authorization has timed out. Please link it again
account-error-verify-email-first = Please verify your email first. Until it is verified you cannot link another account, add a phone number or turn on two-step verification
account-error-google-not-linked = This account has no Google account linked
account-error-github-not-linked = This account has no GitHub account linked
account-error-unlink-last = This account has no email or phone number, so after unlinking you could not recover the password. Please add an email or phone number first
account-error-google-other = The authorized Google account is not the one linked to this account
account-error-github-other = The authorized GitHub account is not the one linked to this account
account-error-confirm-expired = The confirmation has timed out. Please confirm with the other account again
account-error-password-exists = This account already has a password. Please use "Change password"
account-error-wrong-code = Wrong code
account-error-expired-code = The code has expired. Please get a new one
account-error-mobile-invalid = Please enter a valid mainland China mobile number (11 digits)
account-error-sms-frequent = This number has received too many text messages. Please try again later
account-error-sms-failed = The text message could not be sent. Please try again later
account-error-mobile-closed = This phone number is not registered yet, and sign-ups are closed for now
account-error-mobile-expired = The phone verification has timed out. Please get a new code
account-error-mobile-first = Please add a phone number first
account-error-mobile-linked-here = This phone number is already linked to this account
account-error-mobile-taken = This phone number is linked to another account
account-error-mobile-none = This account has no phone number linked
account-error-mobile-unbind-last = This account has no email and no linked account, so after removing the phone number you could not recover the password. Please add an email first
account-error-email-change-invalid = The email confirmation link is invalid, used or expired. Please ask to change your email again
# $provider is Google or GitHub.
account-error-provider-off = This site has not turned on { $provider } login
account-error-purpose = Invalid authorization purpose
account-error-authorize-url = Invalid authorization address
account-error-flow-login = The authorization has timed out or did not start on this site. Please log in again
account-error-flow-retry = The authorization has timed out or did not start on this site. Please try again
account-error-provider-cancelled = { $provider } authorization was cancelled
account-error-provider-failed = { $provider } authorization did not complete. Please try again
account-error-cannot-log-in = This account cannot log in right now
# $provider is Google or GitHub, $site the site name.
account-external-description = Your { $provider } account is verified; finish signing up for { $site }
account-error-stale-reason = The form has expired. Please submit it again. Reason: { $reason }
account-error-login-first = Please log in first
account-error-unlink-provider = Invalid provider to unlink
account-error-sms-wait =
    A code was just sent. Please wait { $seconds ->
        [one] { $seconds } second
       *[other] { $seconds } seconds
    } before asking again
account-error-sms-mobile-daily = This number has used up today's codes. Please try again tomorrow
account-error-sms-site-daily = Today's text message codes have run out. Please try again tomorrow or use another way
# $site is the site name.
account-sms-description = Log in to { $site } with your phone number and a text message code
account-error-sms-off = This site has not turned on text message login
account-error-sms-too-many = Too many code requests. Please try again later
account-error-code-first = Please get a code first
account-mobile-description = Your phone number is verified; finish signing up for { $site }
account-email-change-description = Confirm the email change of your { $site } account
account-error-email-change-params = The confirmation link is malformed. Please ask again
account-error-email-change-link = The confirmation link is invalid or has expired. Please ask again
account-error-stale-email-link = The form has expired. Please open the confirmation link in the email again
# A startup configuration error (the test of config.rs looks for it).
account-error-two-factor-key-shared = The keys of identity.two_factor must differ from those of identity.cookie
# Link texts inside the sign-up notes.
account-link-rules = community rules
account-link-terms = terms of use
account-link-privacy = privacy notice
# $domains_html links to the multi-domain module.
account-manage-lead-domains = Password login is always available. Set the site-wide default login entry points and participation requirements here; other domains can have their own on their edit page under { $domains_html }, and those without their own use these.
account-manage-lead = Password login is always available. Set the site-wide default login entry points and participation requirements here; once { $domains_html } is on, each domain can have its own.
# $sent_html is today's count in bold; $limit is the daily limit for the whole site.
account-manage-sms-today = { $sent_html } text message codes sent today; the whole site can send { $limit } a day.
account-manage-sms-site-limit = Today's text message codes have all been sent; further requests tell members to try again tomorrow (written to the action log). To keep sending, raise "Codes per day for the whole site" below.
# $provider is Google or GitHub.
account-manage-oauth-unconfigured = { $provider } login is ticked, but { $provider } credentials are not set up yet, so the login page does not show it.
account-manage-sms-local = Text messages are currently delivered to a local directory on the server, so members get no codes. Set up Alibaba Cloud SMS verification below before opening text message login.
account-manage-methods = Login entry points
account-manage-methods-help = Ways shown on the login page besides password login. Text message and third-party logins sign up numbers or accounts that are not registered yet, and the registration mode (invite only, closed) still applies. Google and GitHub also need their credentials set up below before they appear on the login page.
account-manage-participation = Participation requirements
account-manage-participation-help = Starting threads, replying, private messages, likes, downvotes and tips, joining activities, uploading, creating invite codes and turning on the API all need at least one of the ticked items; with none ticked, nothing is required. Administrators are exempt.
account-manage-two-factor = Two-step verification for text message and third-party logins
account-manage-two-factor-require = Same as password login: accounts with two-step verification also enter the code from their authenticator
account-manage-two-factor-skip = Treat text messages and third parties as the second factor: a correct code or a granted authorization logs in directly
account-manage-sms-interval = Time between two codes to the same number (seconds)
# The allowed range of a number field.
account-manage-range = { $min } to { $max }.
account-manage-sms-mobile-daily = Codes per number per day
account-manage-sms-mobile-daily-help = 1 to 50; days follow the site time zone.
account-manage-sms-ip-hourly = Codes per IP per hour
account-manage-sms-site-daily = Codes per day for the whole site
account-manage-sms-site-daily-help = Once reached, nothing more is sent that day; it is written to the action log and noted on this page.
account-manage-sms-ttl = Code lifetime (minutes)
account-manage-sms-attempts = Wrong attempts per code
account-manage-sms-attempts-help = After that many, the code stops working and a new one is needed.
account-manage-sms-provider = Text message provider
account-manage-sms-provider-intro = Uses "SMS verification" of the Alibaba Cloud phone number verification service: personal identity verification is enough, with the signature and template from the console (the template needs the variables code and min). Codes are created and checked by this site; Alibaba Cloud only delivers them.
account-manage-key-missing-sms = The private configuration has no [common] encryption_key yet (shared with the mail password), so the Alibaba Cloud settings cannot be saved for now.
account-manage-transport = Delivery
account-manage-access-key-id-help = Preferably a RAM sub-account allowed only the phone number verification service.
account-manage-access-secret-saved = Saved; leave empty to keep it. Fill it in again when changing the AccessKey ID.
account-manage-access-secret-new = Required the first time.
account-manage-secret-encrypted = {" "}Stored encrypted and never shown on the page.
account-manage-sign-name = Signature name
account-manage-template-code = Template CODE
account-manage-sms-provider-save = Save text message provider
account-manage-google-intro = Create an OAuth client (web application) under "APIs & Services › Credentials" in the Google Cloud console, with only the openid, email and profile scopes; no Google review is needed. Users in mainland China cannot open the Google login page.
account-manage-github-intro = Create a GitHub App under "Settings › Developer settings › GitHub Apps" on GitHub, and set Email addresses to read-only under "Permissions › Account permissions" (without it the email cannot be read, and members fill it in when signing up). A GitHub App can register at most 10 callback URLs.
account-manage-callbacks = Callback URLs to register with { $provider }:
account-manage-callbacks-none = No site has { $provider } login ticked yet. Once ticked, each site's address is listed here.
account-manage-github-over-limit = More than 10 sites have GitHub login on, more than a GitHub App can register; GitHub refuses logins on the extra sites.
account-manage-key-missing-oauth = The private configuration has no [common] encryption_key yet (shared with the mail password), so { $provider } credentials cannot be saved for now.
account-manage-client-id-help = Save it empty to turn this off and clear the secret.
account-manage-client-secret-saved = Saved; leave empty to keep it. Fill it in again when changing the Client ID.
account-manage-client-secret-new = Fill it in together the first time.
account-manage-oauth-save = Save { $provider } credentials
account-manage-description = Text message and third-party logins, participation requirements and provider credentials
account-manage-saved = Saved.
account-manage-sms-saved = Text message provider settings saved.
account-manage-google-saved = Google credentials saved.
account-manage-github-saved = GitHub credentials saved.
account-manage-error-two-factor = Two-step verification for text message and third-party logins can only be "same as password login" or "log in directly"
account-manage-error-sms-integer = Text message limits must be whole numbers
# $provider is Google or GitHub; $error is what is wrong with its credentials.
account-manage-oauth-error = { $provider }: { $error }
account-manage-method-sms = Text message login
account-manage-method-google = Google login
account-manage-method-github = GitHub login
account-manage-participation-email = Verified email
account-manage-participation-mobile = Linked phone number
account-manage-participation-google = Linked Google account
account-manage-participation-github = Linked GitHub account
account-manage-transport-local = Local directory (for development; members get no messages)
account-manage-transport-aliyun = Alibaba Cloud SMS verification
account-manage-error-transport = Delivery can only be the local directory or Alibaba Cloud SMS verification
account-manage-error-access-key-id = The AccessKey ID must be 1 to 64 letters or digits
account-manage-error-sign-name = The signature name must be 1 to 40 characters without control characters
account-manage-error-template-code = The template CODE must be 1 to 40 letters, digits or underscores
account-manage-error-decrypt = The text message service secret cannot be decrypted; check [common] encryption_key in the private configuration or save the AccessKey Secret again
account-manage-error-stale = The login method settings were updated meanwhile; refresh and try again
account-manage-error-access-key-changed = Fill in the AccessKey Secret again when changing the AccessKey ID
account-manage-error-access-secret = The AccessKey Secret is not valid
account-manage-error-access-secret-missing = Fill in the AccessKey Secret when turning on Alibaba Cloud SMS verification the first time
account-manage-error-client-id = The Client ID must be 1 to 255 visible ASCII characters without spaces
account-manage-error-client-secret-missing = Fill in the Client Secret as well the first time
account-manage-error-client-id-changed = Fill in the Client Secret again when changing the Client ID
account-manage-error-client-secret = The Client Secret is not valid
# Admin area: users, member details, invites and registration sources.
account-manage-users = Users
account-manage-users-description = Account details and admin shortcuts
account-manage-users-query = Username or email
account-manage-status = Status
account-manage-all = All
account-manage-status-active = Active members
account-manage-status-frozen = Frozen
account-manage-status-unverified = Email not verified
# Account states of a member in the admin area.
account-manage-state-deleting = Deletion in progress
account-manage-state-deleted = Deleted
account-manage-state-frozen = Frozen
account-manage-state-normal = Normal
account-manage-role = Role
account-manage-filter = Filter
# $total is the number of members found.
account-manage-users-total =
    { $total ->
        [one] { $total } member.
       *[other] { $total } members.
    }
account-manage-email-verified = Email verified
account-manage-email-unverified = Email not verified
# $mobile is the masked phone number.
account-manage-mobile = Phone { $mobile }
account-manage-no-mobile = No phone number
# $providers lists the linked sign-in providers, such as Google and GitHub.
account-manage-external = Linked to { $providers }
account-manage-joined = Joined { $time }
# Shortcuts under a member in the admin list.
account-manage-sanctions-link = Sanctions and records
account-manage-roles-link = Role and forum permissions
account-manage-invites-link = Invites
account-manage-honors-link = Honor groups
account-manage-api = API token
account-manage-api-active = Active
account-manage-api-expired = Expired
account-manage-api-created = created { $time }
account-manage-api-expires = expires { $time }
account-manage-api-last-used = last used { $time }
account-manage-revoke-reason = Reason for revoking
account-manage-api-revoke = Revoke API token
account-manage-api-none = This member has no API token.
account-manage-users-empty = No members match.
account-manage-users-pages = Member pages
account-manage-error-no-member = No such member
# A reason required by admin actions (shared by the admin pages).
account-manage-error-reason = Enter a reason of 1 to 500 characters
account-manage-error-filter = Invalid filter or page number
account-manage-error-user-filter = Invalid member filter
account-manage-invites = Invites
account-manage-invites-description = Find and issue invite codes across the site
# $link_html links to the registration settings page.
account-manage-invites-prefix-hint = Invite codes accepted by prefix (never used up, reusable) are added under “Invite code prefixes” in { $link_html }.
# Like account-manage-invites-prefix-hint, when some prefixes are set; $count is how many.
account-manage-invites-prefix-count =
    Invite codes accepted by prefix (never used up, reusable) are added under “Invite code prefixes” in { $link_html }. { $count ->
        [one] { $count } prefix is set.
       *[other] { $count } prefixes are set.
    }
account-manage-invites-prefix-link = Global settings › Registration & permissions
account-manage-invites-count = Number of codes
account-manage-invites-reason = Reason for issuing
account-manage-invites-create = Generate invite codes
account-manage-invites-owner = Inviter UID
# States of an invite code.
account-manage-invite-available = Available
account-manage-invite-frozen = Frozen
account-manage-invite-revoked = Revoked
account-manage-invite-used = Used
account-manage-invite-expired = Expired
account-manage-invites-total =
    { $total ->
        [one] { $total } invite code.
       *[other] { $total } invite codes.
    }
account-manage-invite-expires = Expires { $time }
# $uid is the member who signed up with the code.
account-manage-invite-used-by = Used by UID { $uid }
account-manage-invite-freeze = Freeze
account-manage-invite-unfreeze = Unfreeze
account-manage-invite-revoke = Revoke
account-manage-invites-empty = No invite codes match.
account-manage-invites-pages = Invite pages
account-manage-error-invite-count = Each batch can have 1 to 100 invite codes
account-manage-error-invites-off = Issuing invite codes is turned off
account-manage-error-invite-replay = This request was already sent with different values
account-manage-error-invite-ttl = Invalid invite code lifetime
account-manage-error-invite-key = Invalid invite request ID
account-manage-error-invite-action = Invalid invite code action
account-manage-error-key-used = This request ID was already used for another action
account-manage-error-invite-changed = The invite code has changed. Please refresh and try again
account-manage-error-invite-filter = Invalid invite code filter
account-manage-sources = Registration sources
account-manage-sources-description = Sign-ups counted by domain, promotion channel, inviter and invite code prefix
# $total_html is the number of members in the registration log.
account-manage-sources-lead = The registration log currently holds { $total_html } members. Each member is counted once by sign-up domain and once by promotion channel. Domains and channels missing from early logs are grouped under “Not recorded”. Members whose account deletion has completed are removed from all counts.
account-manage-sources-sites = Sign-up domains
account-manage-sources-sites-lead = Different site paths under one domain are counted separately. With multiple domains on, sign-ups through an unlisted address count under “Unlisted domain”.
account-manage-sources-site = Site address
# Column heading: number of sign-ups.
account-manage-sources-count = Sign-ups
account-manage-sources-empty = No sign-ups yet.
account-manage-sources-channels = Promotion channels
# $example_html is an example promotion link.
account-manage-sources-channels-lead = A promotion link looks like { $example_html }. Channel names are case-sensitive, 1–64 characters long, and may contain letters in any script, digits, dots, underscores and hyphens.
account-manage-sources-channels-rule = Each sign-up counts under the latest valid channel mark set in the same browser on the same site within the 30 days before signing up. Without a valid mark it counts under “No channel”.
account-manage-sources-channel = Channel
account-manage-sources-inviters = Inviters
account-manage-sources-inviters-lead = A member who signs up with a one-time invite code issued by a member counts under that inviter.
account-manage-sources-inviters-empty = No sign-ups through member invite codes yet.
account-manage-sources-prefixes = Fixed invite code prefixes
account-manage-sources-prefixes-lead = A sign-up with a reusable prefix counts under the longest prefix it matched at the time. If the same code is also a valid one-time invite code, it counts under the inviter instead.
account-manage-sources-prefix = Fixed prefix
account-manage-sources-prefixes-empty = No sign-ups through fixed prefixes yet.
# The accessible name of a section's pagination; $section is the section heading.
account-manage-sources-pages = { $section } pages
# Group labels in the registration source tables.
account-manage-sources-unlisted = Unlisted domain
account-manage-sources-no-channel = No channel
account-manage-sources-not-recorded = Not recorded
# A real channel or site name that reads like one of the group labels.
account-manage-sources-marked = { $name } (channel mark)
account-manage-sources-inviter = { $name } (UID { $uid })
account-manage-error-sources-page = Invalid page number
# The text of a verification code delivered to a local file (no SMS provider configured).
account-sms-local-file =
    Mobile number: { $mobile }
    Purpose: { $purpose }
    Code: { $code }
    Valid for: { $minutes ->
        [one] { $minutes } minute
       *[other] { $minutes } minutes
    }
