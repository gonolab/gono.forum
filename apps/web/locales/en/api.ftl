## The member API (/api/v1): the human-readable `message` of error responses, in the request's language; the
## `code` values are fixed identifiers.

api-error-rate = Too many requests. Please try again later
api-error-internal = The service failed for now. Please try again later
api-error-unauthorized = The account's state has changed
api-error-forbidden = This account does not have this permission
api-error-not-found = The record does not exist or cannot be accessed
api-error-unavailable = The service is temporarily unavailable. Please try again later
api-error-endpoint = No such endpoint
api-error-method = Method not supported
api-error-bearer = Send a Bearer token in the Authorization header
api-error-token = The token is invalid, turned off or expired
api-error-account = The account is frozen or deleted
api-error-disabled = The site has turned off the API
api-error-content-type = The request body must be application/json
api-error-body-size = The request body is larger than the limit
api-error-body = The request body cannot be read
# $error is the JSON parser's own message.
api-error-json = The JSON cannot be parsed: { $error }
api-error-idempotency = Idempotency-Key must be 1 to 200 visible ASCII characters
api-error-visibility = visibility can only be public or members
api-error-forum = The forum does not exist or cannot be accessed
api-error-thread = The thread does not exist or cannot be accessed
api-error-reply = The reply does not exist or cannot be accessed
api-error-nested-first = The first post cannot be the parent of a nested reply. Use the thread reply endpoint
api-error-activity = The thread is not an activity or cannot be accessed
api-error-edit-empty = Provide at least title, body or visibility
api-error-edit-first = Use the thread edit endpoint for the first post
