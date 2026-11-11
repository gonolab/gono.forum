## Built-in policy texts, shown while the site has not written its own in that language.
## Plain, generic wording; each site replaces them with its own in the admin area. Bodies are Markdown.

# The built-in terms of use; $site is the site name.
policy-terms-body = By using { $site } you agree to these rules. Keep your account safe, post only content you have the right to share, and respect other members. You can report content that breaks the rules through the report link, and ask for a review of a sanction through the appeal page.
policy-contact-body = For questions about the site, you can email the administrators. For reporting content, appealing a sanction or a request about your own data, please use the matching links below: you can follow the progress on the site, which is faster than email.
# $days is how long a former username stays reserved after the account is deleted.
policy-privacy-body = In your account settings you can choose who sees your profile, set notification preferences and manage your block list. When you arrive through a referral link, the site keeps the most recent referral mark in a cookie for up to 30 days. The mark is cleared when you sign up, and the site and referral channel are recorded in the sign-up log so administrators can count sign-ups. The data request page lets you export your data, delete your account and erase your content; deletion and erasure have a 7-day cooling-off period. Export files are kept for 7 days. After deletion the sign-up log is cleared, records needed for moderation and points are kept, and your former username stays reserved for { $days } days. When a discussion is kept anonymously, information you wrote into your posts yourself is not removed automatically.
policy-rules-body = Be friendly, describe your question accurately and post in the right forum. Do not post spam, personal attacks or other people's private information. Reports and appeals are handled by staff with the right permissions.
policy-verified-title = How to get verified
# The built-in explainer of the verified member directory, Markdown.
policy-verified-body =
    Verification is granted by the administrators after a manual check and can be withdrawn when the conditions are no longer met. It only means the identity was checked; it does not vouch for any service or deal.
    
    - **Merchants**: a lawful business with public contact details and an official website, operating for at least 3 months. Write to the site email from an address on your website's domain, giving your username and your website.
    - **Agents**: public contact details, steady clients and a good reputation, some activity on this site, and at least 3 months in the trade.
    - **Bloggers and webmasters**: an independent website that has existed for at least a year, with lawful content.
    - **Channel owners**: a channel that has existed for at least a year, with real subscribers and lawful content.
    
    Except for merchants, please contact the administrators through the [contact page](/contact) or the site email, with your username and material that can be checked.
policy-links-title = How to apply
# The built-in link application notes, Markdown; the numbers come from the link application settings.
policy-links-body =
    - First add a link to this site on your home page or links page, then log in and submit your site's name, category and address. If our link is not on your home page, give the address of the page it is on.
    - An administrator reviews each application by hand. Approved links are listed on this page, and some are also shown in the home page sidebar.
    - Each website is listed once, and each account can have at most { $live } links pending or listed at a time. A rejected website cannot apply again for { $cooldown } days.
    - You need a verified email and an account at least { $age } days old. If your site removes its link to us, we take down the matching link here too.
policy-updated = Last updated: { $date }
policy-channels = Contact channels
# $link_html links to the member's reports.
policy-channel-report = Reporting content: log in and choose "Report" on a thread, reply or profile; follow the progress in { $link_html }.
policy-channel-report-link = My reports
# $records_html links to the member's account records, $appeal_html to the appeal page for frozen accounts.
policy-channel-appeal = Appealing a sanction: log in and appeal from your { $records_html }; if your account is frozen, use { $appeal_html }.
policy-channel-appeal-records = account records
# $requests_html links to the member's data requests, $access_html to the data request page for frozen accounts.
policy-channel-data = Your own data (export, account deletion, erasing your posts): log in and use { $requests_html }; if your account is frozen, use { $access_html }.
policy-channel-data-requests = Data requests
policy-nav = Community policies
policy-error-too-long = The title or body is too long
policy-manage-customized = customized
# $items lists the navigation items that are hidden, joined with policy-manage-hidden-separator.
policy-manage-hidden = These items are not shown right now: { $items }. They come back by themselves once the forum is enabled again or made public or visible to signed-in members; no need to save again.
policy-manage-hidden-separator = {"; "}
# One hidden navigation item: its text, its target and why it is hidden.
policy-manage-hidden-item = “{ $label }” ({ $target }, { $reason })
policy-manage-hidden-disabled = the forum is disabled
policy-manage-hidden-private = the forum is kept for the admin area only
policy-manage-hidden-parent = the parent forum is disabled or admin-only
policy-manage-title = Title
policy-manage-body = Body
policy-manage-navigation = Navigation
policy-manage-navigation-help = One item per line: text | forum:forum number, or one of the site addresses /, /search, /terms, /privacy, /rules and /contact. Leave empty for the default forum order; at most 12 items.
policy-manage-verified-help = Shown as the "How to apply" part of the verified member directory /verified; the body is Markdown. Leave both the title and the body empty to use the built-in text.
policy-manage-links-help = Shown as the notes on the friend link directory /links and the application page /links/apply; the body is Markdown. Leave both the title and the body empty to use the built-in text.
policy-manage-heading = Policy
policy-manage-help = The body is Markdown; public pages use this version as soon as it is saved.
policy-manage-not-found = No such policy
