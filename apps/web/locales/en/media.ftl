## Attachments: uploads, the member's attachment library and trash, and the errors of the attachment service.

# The member's attachment library and trash.
media-usage = Storage used
media-usage-kind = Type
media-usage-files = Files
media-usage-size = Size
media-usage-note = Note
media-usage-total = Total
media-usage-total-note = In use, unused and in the trash; there is no overall storage limit
# $used and $max are sizes such as "1.5 MiB"; $zone is the site time zone.
media-daily-quota = Today's upload quota: { $used } used of { $max } (by calendar day in the site time zone { $zone }; deleting, restoring and cleanup do not give today's quota back).
# $unused is the name of the "unused" row.
media-trash-note =
    { $days ->
        [one] Attachments in the trash are kept for { $days } day and then removed automatically; until then you can restore them, and they go back to “{ $unused }”. Deleting permanently removes them at once and cannot be undone.
       *[other] Attachments in the trash are kept for { $days } days and then removed automatically; until then you can restore them, and they go back to “{ $unused }”. Deleting permanently removes them at once and cannot be undone.
    }
# $link_html links to the attachment trash.
media-kept-note =
    { $count ->
        [one] { $count } attachment. Only attachments that passed the check and are not used by a post or avatar can be deleted; deleted ones go to the { $link_html }.
       *[other] { $count } attachments. Only attachments that passed the check and are not used by a post or avatar can be deleted; deleted ones go to the { $link_html }.
    }
media-trash-link = attachment trash
media-uploaded = Uploaded { $date }
media-expires = Removed automatically { $date }
media-trash = Delete
media-restore = Restore
media-delete = Delete permanently
media-trash-empty = The trash is empty.
media-empty = You have not uploaded any attachments yet.
# Rows of the storage table: the label and its note.
media-in-use = In use
media-in-use-note = Used by posts, avatars or site images
media-unused = Unused
media-unused-note = Removed automatically if not used within 7 days of uploading or leaving a post
media-in-trash = Trash
media-in-trash-note = Removed automatically when they expire; until then they can be restored
media-checking = Being checked
media-checking-note = Counted at the original size, not included in the total
media-rejected = Did not pass the check
media-rejected-note = Not stored, so they take no space
# The state of one attachment in the library.
media-state-trash = In the trash
# $detail is why the file did not pass the check.
media-state-rejected = Did not pass the check: { $detail }
media-state-posts =
    { $count ->
        [one] In use: { $count } post
       *[other] In use: { $count } posts
    }
media-state-avatar = In use: avatar
media-state-site = In use: site image or forum icon
media-state-expires = Unused, removed automatically after { $date }
media-state-expiring = Unused, about to be removed automatically
media-blocked = Attachments in use cannot be deleted. Edit the post to remove it first
media-error-page = Invalid page number
media-description = Your uploaded attachments, storage use and trash
media-error-rate = Too many requests. Please try again later
media-error-verify = Verify your email to upload
media-error-format = Invalid upload format
media-error-fields = Too many upload fields
media-error-token-order = the form token was not sent before the file
media-error-one-file = Choose one file at a time
media-error-upload-rate = Too many uploads. Please try again later
media-error-interrupted = The file upload was interrupted
media-error-too-large = The file is larger than the upload limit
media-error-staging = The file could not be stored temporarily
media-error-field = Invalid upload field
media-error-field-long = An upload field is too long
media-error-utf8 = Please use UTF-8
media-error-no-file = Choose a file
# The editor's attachment row after cancelling it.
media-cancelled = Attachment cancelled
# Plain-text answers for list thumbnails.
media-thumbnail-missing = Thumbnail not found
media-thumbnail-unavailable = The thumbnail is temporarily unavailable
media-thumbnail-failed = The thumbnail could not be read
media-error-unsupported = Only JPG, PNG, WebP and GIF images can be uploaded for now
# MediaError: what went wrong with an attachment.
media-error-forbidden = Your account is not allowed to do this
media-error-not-found = The attachment does not exist or cannot be accessed
media-error-internal = The attachment service is temporarily unavailable
media-error-staging-file = Invalid temporary upload file
media-error-request-key = Invalid upload request ID
media-error-filename = Invalid file name or attachment description
media-error-size = The file is empty or larger than the upload limit
media-error-request-reused = This upload request was already used for a different file or description
media-error-daily = You have used up today's upload quota
media-error-no-storage = The administrator has not set up attachment storage yet
media-error-credentials-ref = Invalid storage credentials reference
media-error-credentials = The storage credentials are unavailable
media-error-credentials-mode = The storage credentials file must have permissions 0600
media-error-credentials-format = Invalid storage credentials format
media-error-profile = The storage settings this attachment uses no longer exist
media-error-storage = The S3 storage operation failed. Check the connection, permissions and settings, then try again
media-error-avatar = An avatar must be an image you uploaded that passed the check and has not expired
media-error-image-trashed = The image is in the trash. Restore it first
media-error-trash-ready = Only attachments that passed the check can go to the trash
media-error-trash-in-use = The attachment is still used by a post, avatar, brand image or forum icon. Edit the post to remove it before deleting
media-error-trash-expired = The attachment is past the trash retention period and cannot be restored
media-error-animation-size = After processing, the animation is over the { $mib } MiB limit
# Why an attachment did not pass the check, stored with the attachment in the site language when the check ends.
media-check-crashed = The file check process exited abnormally
media-check-no-result = The file check process wrote no result
media-check-incomplete = The file check result is incomplete
media-check-timeout = Processing the image or file structure timed out
media-check-paused = The check or storage failed several times; paused until an administrator schedules a retry
media-check-retrying = The check or storage is temporarily unavailable; it will retry automatically
media-check-unsupported = This format is no longer supported
media-check-source = Invalid quarantined file ID
media-check-mismatch = The quarantined file does not match the upload record
media-check-failed = The file failed the format, structure or resource check
media-error-cancel-in-use = The attachment is still used by a post, avatar, brand image or forum, so it cannot be cancelled
media-error-purge-in-use = The attachment is still used by a post, avatar, brand image or forum, so it cannot be removed
media-manage-storage-heading = Attachment storage
media-manage-storage-intro-configured = Images and attachments use S3-compatible storage. Credentials are set; leave them empty to keep the current ones. New keys for the same endpoint and bucket also replace the keys earlier attachments there are read with, so the old keys can be revoked once saved.
media-manage-storage-intro = Images and attachments use S3-compatible storage. Fill in the connection details from your storage provider.
media-manage-storage-endpoint = Endpoint
media-manage-storage-region = Region
media-manage-storage-bucket = Bucket
media-manage-storage-prefix = Object prefix
media-manage-storage-path-style = Use path-style addressing
media-manage-storage-secret-dropped = For safety the Secret Key is not filled back in; enter it again before saving or testing again.
media-manage-storage-save = Save settings
media-manage-storage-test = Test connection
media-manage-storage-description = S3-compatible attachment storage
media-manage-storage-tested = Connection test passed: the storage can be read and written
media-manage-storage-saved = Storage settings saved; new uploads use them
media-manage-error-endpoint = The S3 endpoint must be a full HTTP(S) URL
media-manage-error-endpoint-https = The S3 endpoint must use HTTPS (HTTP only for a loopback service on this machine) and cannot contain credentials or a path
media-manage-error-bucket = The S3 region, bucket or object prefix is not valid
media-manage-error-keys-first = The first setup needs both the Access Key and the Secret Key
media-manage-error-keys-both = To change the credentials, fill in both the Access Key and the Secret Key
media-manage-error-keys-shape = The S3 credentials are not valid
media-manage-error-stale = The storage settings were updated meanwhile; refresh and try again
media-manage-error-mismatch = What S3 returned differs from what was written
media-manage-s3-construction = the request could not be built; check the endpoint and region
media-manage-s3-timeout = the connection timed out
media-manage-s3-unreachable = the endpoint cannot be reached (connection refused, name not resolved or no network)
media-manage-s3-not-sent = the request could not be sent; check the endpoint
media-manage-s3-unrecognized = the response was not understood; make sure the endpoint is an S3-compatible API
media-manage-s3-no-bucket = the bucket does not exist
media-manage-s3-denied = access denied; check that the keys can read and write this bucket
media-manage-s3-no-key = the Access Key does not exist
media-manage-s3-signature = the signature does not match; check the Secret Key
# $code and $message come from the storage service.
media-manage-s3-service-message = the service returned { $code }: { $message }
media-manage-s3-service = the service returned { $code }
media-manage-s3-service-error = the service returned an error
media-manage-s3-unknown = unknown error
media-manage-s3-write = write
media-manage-s3-read = read-back
# $step is media-manage-s3-write or media-manage-s3-read; $reason why it failed.
media-manage-s3-failed = S3 { $step } failed: { $reason }
media-manage-brand-upload = Upload an image
media-manage-brand-upload-submit = Upload and check
media-manage-brand-intro = Preview and apply an image once its check is done. The favicon and the app icon must be square, the app icon at least 512×512; share images are output at 1200×630.
media-manage-brand-refresh = Refresh check status
# $label is the purpose of the image.
media-manage-brand-current-alt = { $label }, current image
media-manage-brand-builtin = Using the built-in image
media-manage-brand-apply-image = Apply image
media-manage-brand-restore = Restore the built-in image
media-manage-brand-current-image = Current image
media-manage-brand-apply = Apply
media-manage-brand-recent = Recent uploads
media-manage-brand-none = No uploads yet.
media-manage-slot-favicon = Favicon
media-manage-slot-app-icon = App icon
media-manage-slot-share = Share image
media-manage-slot-email = Email brand image
media-manage-error-slot = Invalid brand purpose or image
media-manage-error-brand-stale = The brand image was changed meanwhile; refresh and try again
media-manage-error-choose-checked = Choose an image you uploaded that passed its check
media-manage-error-trashed = The image is in the trash; restore it first
media-manage-error-icon-size = Icons must be square: the app icon at least 512×512, the favicon at least 32×32
media-manage-error-image-only = Brand images and forum icons must be images
media-manage-brand-description = Upload and set the forum's brand images
media-manage-brand-error-aid = Invalid image number
media-manage-error-upload-form = The upload form is not valid
media-manage-error-one-image = Choose one image at a time
media-manage-error-receive = Receiving the image failed
media-manage-error-too-large = The image is over the size limit
media-manage-error-staging = The image could not be staged
media-manage-error-field = Invalid form field
media-manage-error-field-long = The field is too long
media-manage-error-upload-field = Invalid upload field
media-manage-error-choose = Choose an image
# The description stored with an uploaded brand image.
media-manage-brand-upload-description = Forum brand image
