## Multi-domain sites in the admin area.

domain-title = Multi-domain sites
domain-lead = When turned on, the site is split by the domain and path a visitor uses: each domain shows the top-level forums ticked for it (child forums follow their parent) and has its own site name, logo, site icon and search engine verification codes. The row whose address equals the forum address is the main domain; unregistered domains show what the main domain shows. The scope only affects what is displayed; it is not a permission.
domain-state-on = on
domain-state-off = off
# $state_html is the bold word "on", $url_html the forum address.
domain-enabled-status = Currently { $state_html }. The forum address (the main domain) is { $url_html }; change it under “Global settings › Site”.
domain-switch-off = Turn off multi-domain sites
# $state_html is the bold word "off".
domain-disabled-status = Currently { $state_html }: every visit shows all forums, as on the main site.
# Follows domain-disabled-status; $link_html links to the site settings.
domain-disabled-need-url = {" "}Before turning it on, fill in the forum address under { $link_html }.
# Follows domain-disabled-status; $url_html is the forum address.
domain-disabled-will-use = {" "}Turning it on makes the forum address { $url_html } the main domain; if the table has no row for it yet, one is created with all top-level forums ticked.
domain-switch-on = Turn on multi-domain sites
domain-link-settings-site = Global settings › Site
domain-link-site = Site
domain-link-branding = Brand images
domain-link-search = Search and performance
domain-link-site-settings = the site-wide settings
domain-list-title = Domains
# $names are top-level forums no enabled domain shows.
domain-unshown = No enabled domain shows { $names }, so visitors cannot reach them.
domain-list-main = main domain
domain-list-disabled = disabled
# $target is "this domain" or "the main domain".
domain-list-canonical = canonical points to { $target }
# $forums are the forums the domain shows.
domain-list-forums = Shows: { $forums }
domain-list-no-forums = (no forums ticked)
domain-delete = Delete
domain-list-empty = No domains registered yet.
domain-list-empty-hint = {" "}The main domain row is created when multi-domain sites are turned on.
domain-add = Add domain
domain-main = Main domain
domain-edit = Edit domain
# Page title when editing a domain; $url is its address.
domain-edit-title = Edit domain { $url }
domain-saved = Saved.
domain-deleted = Deleted.
domain-switched-on = Multi-domain sites are on.
domain-switched-off = Multi-domain sites are off. The domain table is kept and takes effect again when they are turned back on.
# $names are top-level forums no enabled domain shows.
domain-saved-unshown = Saved. Note: no enabled domain shows { $names }, so visitors cannot reach them.
domain-confirm-title = Confirm the path change of this domain
domain-confirm-page = Page
domain-confirm-current = Current address
domain-confirm-new = Address after saving
# $path is the new path, such as /feedback.
domain-confirm-nginx = nginx configuration for the new path { $path } (passed through as is; the application strips the prefix):
# $path is the old path.
domain-confirm-redirect = Make the old path { $path } a 301 redirect to the new path:
domain-confirm-step-nginx = Add the nginx configuration for the new path first and keep the old one;
domain-confirm-step-type = type confirm on this page and save;
domain-confirm-step-check = open this domain at the new address and check that it works;
domain-confirm-step-root = before handing the old root path to another program, make sure no old links still use it.
domain-confirm-step-redirect = change the old path in nginx to the 301 above, and decide later when to remove it.
domain-confirm-mistyped = Type confirm (in lowercase) in the confirmation box, then save.
# $from and $to are the old and new paths.
domain-confirm-message = The path of this domain changes from { $from } to { $to }. Saving takes effect at once: pages under the old path are no longer served by this domain. When you are sure, type confirm below and save again.
domain-root-path = the root path /
# Pages in the path change confirmation table.
domain-page-home = Home
domain-page-login = Sign in
domain-page-feed = Feed
domain-page-sitemap = Sitemap
domain-page-health = Health check
# $url_html is the forum address; the other slots link to the settings pages named.
domain-main-lead = The address { $url_html } follows the forum address; change it under { $settings_html }. The site name, logo, site icon and search engine verification codes come from the global settings ({ $site_html }, { $branding_html }, { $search_html }). The main domain's canonical always points to itself.
domain-field-url = Address
domain-field-url-placeholder = https://demo2.com or https://demo.com/feedback
domain-field-url-help = An HTTP(S) address, optionally with a path; each path segment uses only lowercase letters, digits and -. One host can register one root path plus several sub-paths that do not contain each other; when the root path is registered, the first segment of a sub-path must avoid forum addresses and system paths.
domain-field-name = Site name
domain-field-name-help = Used for the header text, title suffix, sharing information, feeds and the manifest.
domain-field-canonical = canonical points to
domain-canonical-own = this domain
domain-canonical-main = the main domain
domain-field-canonical-help = When it points to the main domain, the main domain must also show the forums this domain shows, and this domain has no sitemap. A forum can have only one other domain pointing to itself as its canonical home.
domain-field-use-main = Use the main site's
# $id is the attachment number of the current image.
domain-field-current-image = Current image #{ $id }
domain-field-favicon = Site icon
# $link_html links to the brand image page.
domain-field-favicon-help = Square, at least 32×32. Upload and inspect the image under { $link_html } first.
domain-field-verification = Search engine verification codes
domain-field-verification-help = One “name=value” per line, output only on this domain's home page.
domain-field-language = Language
# $name is the site language's name.
domain-field-language-site = Same as the site ({ $name })
domain-field-language-help = The interface language of this domain and the language of the account emails it sends.
domain-field-login = Sign-in options
# $link_html links to the site-wide sign-in settings.
domain-field-custom = Customize (otherwise { $link_html } apply)
domain-field-login-help = Which methods the sign-in page shows besides password sign-in; with Customize ticked and no method chosen, only password sign-in is offered.
domain-field-participation = Participation requirements
domain-field-participation-help = What members must meet on this domain to post, reply, send messages, interact, upload and so on; meeting any one is enough. When customized, tick at least one. Administrators are exempt.
domain-field-enabled = Enabled
domain-field-enabled-help = A disabled domain is treated like an unregistered one (it shows the main domain's forums).
domain-field-forums = Top-level forums shown
domain-field-forums-empty = No top-level forums yet.
domain-field-forums-help = Child forums follow their parent.
domain-field-confirm = Type confirm to save
domain-save = Save
domain-back = Back to the list
domain-error-not-found = The domain does not exist.
domain-error-form-number = A number in the form is not valid.
domain-error-needs-confirm = The path of the domain changed and must be confirmed before saving.
domain-error-moved = The path of this domain was just changed. Confirm again with the current value.
domain-error-forbidden = Only administrators can configure multi-domain sites.
# $max is the most domains the table can hold.
domain-error-too-many = At most { $max } domains can be registered.
# $url is a registered address.
domain-error-same-place = { $url } is the same location as an address already registered (a different scheme counts too); it cannot be registered twice.
# $host is a host name with its port.
domain-error-two-roots = Host { $host } can register only one root path.
# $first and $second are two registered addresses.
domain-error-nested = The paths of { $first } and { $second } contain each other; sub-paths of the same host cannot be nested.
# $url is a registered address, $segment the first segment of its path.
domain-error-reserved = { $url }: the first path segment { $segment } is a system path; this host registered its root path, so its sub-paths must avoid system paths.
# $url is a registered address, $segment the first segment of its path.
domain-error-forum-slug = { $url }: the first path segment { $segment } is a forum address; this host registered its root path, so its sub-paths must avoid forum addresses.
# $url is a registered address, $forums the forums the main domain lacks.
domain-error-canonical-main = The canonical of { $url } points to the main domain, so the main domain must also show its forums (missing: { $forums }).
# $forum is a forum name, $url the domain already showing it as its canonical home.
domain-error-two-owners = Forum { $forum } is already shown by { $url } as its canonical home; a forum can have only one canonical domain.
domain-error-verification = Search engine verification codes take one “name=value” per line, at most 10 lines; names are 1–64 letters, digits, dots, underscores or hyphens, and values 1–128 letters, digits, underscores or hyphens.
domain-error-id = The domain id or version is not valid.
domain-error-disabled = Turn on multi-domain sites first.
domain-error-stale = This domain has been changed. Refresh and try again.
domain-error-forums = Only existing top-level forums can be chosen (child forums follow their parent).
domain-error-url-length = The address can be at most 255 characters.
domain-error-url = The address must be an HTTP(S) address with a host name; with a path, each segment uses only lowercase letters, digits and - and is at most 64 characters.
domain-error-is-main = This is the forum address (the main domain); the main domain row is already in the table.
domain-error-name = The site name needs 1 to 80 characters.
domain-error-canonical = canonical can only point to this domain or the main domain.
domain-error-image-id = The image number is not valid.
domain-error-login = Sign-in options can only be chosen from SMS sign-in.
domain-error-participation = Tick at least one custom participation requirement (verified email, linked phone number).
domain-error-language = Only a built-in language can be chosen.
domain-error-delete-main = The main domain row cannot be deleted; its address follows the forum address.
domain-error-image = Choose an image you uploaded that passed inspection.
domain-error-image-trashed = The image is in the trash. Restore it first.
domain-error-favicon = The site icon must be square, at least 32×32.
domain-error-enable-without-url = Fill in the forum address under “Global settings › Site” before turning on multi-domain sites.
domain-error-clear-url = The forum address cannot be cleared while multi-domain sites are on; turn them off first.
domain-error-url-registered = The new forum address is already registered for multi-domain sites (a different scheme counts too). Delete that row there first.
domain-error-url-conflict = The new forum address conflicts with other addresses registered for multi-domain sites (a duplicate root path on one host, nested sub-paths, or a sub-path named like a forum address). Adjust those rows first.
# Errors loading the domain table, shown on the error page until the row is repaired; $value is the stored value.
domain-error-load-status = The status value in the domain table is not valid: { $value }
# $value is the stored canonical value.
domain-error-load-canonical = The canonical value in the domain table is not valid: { $value }
