## Built-in e-mail text, used when the site has not customized that kind of mail in the language.
## Account mail follows the language of the domain the member started on, the rest the site
## language. $site is the site name.

# Put before the subject of every mail sent over SMTP.
mail-subject-prefix = [{ $site }]{" "}
# The first line of every built-in mail.
mail-greeting = Hello { $username },
mail-verify-subject = Verify your email
mail-verify-instruction = Open this link to verify your email:
mail-verify-reminder = Please verify before the link expires. Sending a new link makes the old one stop working.
mail-reset-subject = Reset your password
mail-reset-instruction = Open this link to set a new password:
mail-reset-reminder = The link works only once; please use it before it expires. After the reset, every device is logged out. If you did not ask for this, you can ignore this email.
mail-email-change-subject = Confirm your new email
mail-email-change-instruction = Open this link to confirm this new email address:
mail-email-change-reminder = The link works only once. After you confirm, every device is logged out; log in again with the new email or your username. If you did not ask for this, you can ignore this email.
mail-appeal-access-subject = Confirm your email to appeal
mail-appeal-access-instruction = Open this link to see your own sanctions and appeals:
mail-appeal-access-reminder = The link is valid for one hour. It only opens your own sanctions and appeals and does not log you in. Asking again, having your account restored or changing your credentials makes the old link stop working. If you did not ask for this, you can ignore this email.
mail-privacy-access-subject = Confirm your email for a data request
mail-privacy-access-instruction = Open this link to go to your own data requests:
mail-privacy-access-reminder = The link is valid for one hour. It only allows handling your own data requests and does not log you in. If you did not ask for this, you can ignore this email.
# A time in mail: the full date and time in the site time zone, then the zone, such as "2026-10-31 20:15 (UTC+8)".
mail-time = { $time } ({ $zone })
# Content notification mail.
mail-content-subject = New forum notification
mail-content-reply = Someone replied to you
mail-content-quote = Someone quoted your post
mail-content-mention = Someone mentioned you
mail-content-thread = A thread you follow has a new reply
mail-content-forum = A forum you follow has a new thread
# $action is one of the five sentences above, $title the thread title.
mail-content-line = { $action }: { $title }
mail-content-optout = You can turn off these emails in your notification preferences.
# Private message mail. The message itself is never in the mail.
mail-message-subject = New private message
mail-message-intro =
    { $sender } sent you a private message. This conversation has { $count ->
        [one] { $count } unread message:
       *[other] { $count } unread messages:
    }
mail-message-optout = You can turn off private message emails in your notification preferences.
# New-device sign-in alert.
mail-login-subject = New sign-in to your account
mail-login-intro = Your account was just used to log in on a new device:
mail-field-time = Time: { $time }
mail-field-ip = IP: { $ip }
mail-field-device = Device: { $device }
mail-login-advice = If this was you, you can ignore this email. If it was not, change your password and log out of all devices on the account security page right away:
mail-login-optout = You can turn off sign-in alerts in your notification preferences.
# Shown when the IP address of a sign-in is not known.
mail-unknown = unknown
mail-unknown-device = Unknown device
# Account security notice. $event is one of the mail-security-* sentences below.
mail-security-subject = Account security notice
mail-security-event = Your account { $event }
mail-security-enable = turned on two-step verification. Every other device was logged out.
mail-security-disable = turned off two-step verification. From now on, logging in needs only the password.
mail-security-regenerate = generated new recovery codes. All the old recovery codes no longer work.
mail-security-lockout = had too many wrong two-step verification codes in a row and is locked for a while. Whoever entered them already knows your password.
mail-security-reset = had its two-step verification reset by a site administrator on the server. Logins on every device and your API tokens no longer work.
mail-security-mobile-bind = added the phone number { $mobile }. You can now log in with a text message code.
mail-security-mobile-change = changed its phone number from { $old } to { $new }.
mail-security-mobile-unbind = removed the phone number { $mobile }.
mail-security-password-set = set a password. Every other device was logged out.
mail-security-link = linked the { $provider } account { $name }. You can now log in with it.
mail-security-unlink = unlinked the { $provider } account { $name }.
mail-security-recovery-login =
    was logged in with a recovery code. { $remaining ->
        [one] { $remaining } code is left.
       *[other] { $remaining } codes are left.
    }
mail-security-other-provider = another service
mail-security-advice = If this was you, you can ignore this email. If it was not, reset your password through password recovery right away, then log out of all devices on the account security page:
mail-security-required = This is an account security email and cannot be turned off.
# Sanction and appeal mail. $id is the sanction number.
mail-governance-no-end = no automatic end
mail-governance-id = Sanction number: { $id }
mail-governance-reason = Reason: { $reason }
mail-governance-starts = Starts: { $time }
mail-governance-ends = Ends: { $time }
mail-governance-appeals = Appeals and results: { $link }
# $window and $handling are days from the governance settings.
mail-governance-appeal-rules =
    Even while your account is frozen, you can confirm your current email there to see your own sanctions and appeals. Each sanction can be appealed once within { $window ->
        [one] { $window } day
       *[other] { $window } days
    } after it starts; an appeal is accepted for handling when you submit it and is expected to be handled within { $handling ->
        [one] { $handling } day.
       *[other] { $handling } days.
    }
mail-governance-note = This email records this decision; for your current restrictions, see the page. After your account is restored, please log in again: old sessions and old verification links are not restored.
# Subjects of sanction and appeal mail (the {{action}} of custom templates); the first line is the governance-notice-*
# sentence.
mail-governance-warning = Your account received a warning
mail-governance-mute = Your account has been muted
mail-governance-freeze = Your account has been frozen
mail-governance-freeze-revoked = The freeze has been lifted
mail-governance-mute-revoked = The mute has been lifted
mail-governance-warning-revoked = The warning has been withdrawn
mail-governance-mute-expired = Your mute has ended
mail-governance-freeze-expired = Your freeze has ended
mail-governance-appeal-accepted = Your appeal was accepted
mail-governance-appeal-rejected = Your appeal was not accepted
mail-unknown-browser = Unknown browser
mail-unknown-system = Unknown system
mail-settings-title = Mail settings
# Meta description of the mail settings page.
mail-settings-description = How account mail is sent and the mail service settings
mail-settings-lead = Email verification, password reset and email change all use the sending settings below.
mail-settings-state-on = on
mail-settings-state-off = off
# $state_html is the bold word "on".
mail-settings-on = Sending mail: { $state_html }, delivered with the settings below. When turned off, delivery pauses and new mail keeps queueing until it is turned back on; members still have to verify their email before posting.
# $state_html is the bold word "off".
mail-settings-off = Sending mail: { $state_html } (off by default on a new site: set up the sending method below, send a test email to check it arrives, then turn it on). Verification, password reset, email change, alert and notification mail is queued and goes out once sending is on; members still have to verify their email before posting, creating invite codes and sending messages. The test email below is not affected.
mail-settings-turn-off = Turn off sending
mail-settings-turn-on = Turn on sending
mail-settings-transport = Delivery
mail-settings-transport-local = Local mail directory (nothing is sent out)
mail-settings-transport-smtp = Real delivery over SMTP
mail-settings-host = Outgoing server
mail-settings-port = Outgoing port
mail-settings-security = Outgoing encryption
mail-settings-security-tls = SSL/TLS (usually 465)
mail-settings-security-starttls = STARTTLS (usually 587)
mail-settings-username = Username / account
mail-settings-password = Password / authorization code
mail-settings-password-saved = An authorization code is saved; leave this empty to keep it.
mail-settings-password-first = When enabling for the first time, enter the authorization code from your mail provider.
mail-settings-password-note = The code is stored encrypted and never shown on the page; enter it again when you change the outgoing server or account.
mail-settings-from-address = Sender address
mail-settings-from-name = Sender name
mail-settings-incoming-host = Incoming server (POP3, SSL/TLS)
mail-settings-incoming-port = Incoming port
mail-settings-incoming-note = The incoming server is saved with the account settings; the forum sends account mail with the SMTP settings above.
mail-settings-test-to = Test recipient
mail-settings-save = Save settings
mail-settings-test = Send test email
mail-settings-switched-on = Sending is on; queued mail will go out shortly.
mail-settings-switched-off = Sending is off; new mail keeps queueing and goes out once sending is turned back on.
mail-settings-saved = Mail settings saved; account mail from now on uses them.
mail-settings-tested = The test email was handed to the outgoing server; check the inbox and the spam folder. The test did not save the settings; an unsaved authorization code must be entered again when saving.
mail-settings-error-forbidden = Only administrators can manage mail settings.
mail-settings-error-session = Your session has expired. Please sign in again.
mail-settings-error-unavailable = Mail settings are temporarily unavailable.
mail-settings-error-retry = Mail settings are temporarily unavailable. Please try again later.
mail-settings-error-sign-in = Please sign in first.
mail-settings-error-operation = This operation is not supported.
mail-settings-error-decrypt = The mail authorization code cannot be decrypted. Check [common] encryption_key in the private configuration, or save the authorization code again.
mail-settings-error-transport = Choose local mail or SMTP delivery.
mail-settings-error-security = SMTP must use TLS or STARTTLS encryption.
mail-settings-error-host = The outgoing server or port is not valid.
mail-settings-error-username = The mail account is not valid.
mail-settings-error-from-name = The sender name must be 1–80 characters without control characters.
mail-settings-error-incoming = The POP3 incoming server or port is not valid.
mail-settings-error-address = The email address is not valid.
mail-settings-error-stale = The mail settings were updated meanwhile. Refresh and try again.
mail-settings-error-password-again = Enter the authorization code again when you change the outgoing server or account.
mail-settings-error-password = The mail authorization code is not valid.
mail-settings-error-password-first = Enter the authorization code when enabling SMTP for the first time.
mail-settings-error-test-smtp = Choose SMTP delivery first.
mail-settings-error-test-wait = A test email was just requested. Wait at least a minute before sending another.
# Subject of the test email from the mail settings page (after the subject prefix).
mail-test-subject = Test email
# Body of the test email; $site is the site name, $url the forum address.
mail-test-body =
    This is a test email for the mail settings of { $site }.
    
    Site: { $url }
    If you received it, mail delivery works.
    { $site }
mail-error-site-url = Links in mail need the forum address: fill it in under “Global settings › Site” in the admin area (an HTTP(S) address such as https://example.com or https://example.com/forum).
mail-templates-title = Mail templates
# Meta description of the mail template page.
mail-templates-description = Edit and preview mail text
mail-templates-kinds = Template types
mail-templates-languages = Languages
# After a language that has its own template.
mail-templates-customized = {" "}(customized)
# $variables are the variable names this kind of mail offers.
mail-templates-help = Available variables: { $variables }. The body must keep the link variable in double braces; the preview uses sample values and sends nothing. An empty template means the built-in text is used.
mail-templates-subject = Subject
mail-templates-body = Body
mail-templates-preview = Preview
mail-templates-save = Save template
mail-templates-reset = Restore built-in text
mail-templates-preview-unsaved = Preview only; nothing was saved.
mail-templates-error-not-found = The mail template does not exist.
mail-templates-error-operation = The operation is not valid.
mail-templates-error-kind = This mail template is not supported.
mail-templates-error-marker = A template variable marker is incomplete.
mail-templates-error-unclosed = A template variable has no closing marker.
mail-templates-error-unknown = The template uses a variable that is not available.
mail-templates-error-subject = The subject cannot contain line breaks and can be at most 120 characters.
mail-templates-error-required = Fill in the subject and the body; the body can be at most 16000 bytes.
# zh-CN shows single braces, as the message did before it became a translation.
mail-templates-error-link = The body must keep the {"{{"}link{"}}"} action link.
mail-templates-error-subject-variable = Variables in the subject cannot contain line breaks.
# The kinds of mail templates.
mail-kind-email-verification = Email verification
mail-kind-password-reset = Password reset
mail-kind-email-change = Email change
mail-kind-appeal-access = Appeal verification
mail-kind-privacy-access = Data request verification
mail-kind-sanction-issued = Sanction notice
mail-kind-sanction-ended = Sanction ended
mail-kind-appeal-result = Appeal result
mail-kind-content = Content updates
mail-kind-message = Message alert
mail-kind-login-alert = Sign-in alert
mail-kind-security-notice = Security notice
# Sample values of the template preview.
mail-preview-username = Sample member
mail-preview-sender = Another member
mail-preview-title = Sample thread
mail-preview-site-name = Gono Forum
mail-preview-other = Sample content
