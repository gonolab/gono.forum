//! Retry limit shared by the three Outbox queues for mail, content notifications and media.
//!
//! After `MAX_ATTEMPTS` consecutive transient failures, automatic retries stop and the status becomes the existing
//! `failed`. `status` has only four values, pending, processing, sent and failed (`db::source_guards` pins the
//! literals written in `SET`), so "attempts exhausted" and "no longer eligible" share `failed` and are
//! told apart by a fixed prefix of `last_error`: the former can be scheduled for retry in the admin area, the latter
//! cannot. The admin task page only shows the redacted label derived from this, never the raw `last_error`.

/// Retry limit for transient failures, counted by the Outbox's `attempts`.
pub const MAX_ATTEMPTS: i64 = 20;

const EXHAUSTED_PREFIX: &str = "gave up after ";
const EXHAUSTED_MARK: &str = " attempts";

/// The text written into `last_error` when attempts are exhausted, with the last failure reason, truncated to the
/// column width of 1000 characters.
pub fn exhausted_error(attempts: i64, last: &str) -> String {
    format!("{EXHAUSTED_PREFIX}{attempts}{EXHAUSTED_MARK}: {last}")
        .chars()
        .take(1000)
        .collect()
}

/// Whether this `failed` is due to exhausted retries.
pub fn is_exhausted(last_error: Option<&str>) -> bool {
    last_error.is_some_and(|error| {
        error
            .strip_prefix(EXHAUSTED_PREFIX)
            .and_then(|rest| rest.split_once(EXHAUSTED_MARK))
            .is_some_and(|(count, _)| {
                !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit())
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_failures_are_told_apart_by_their_fixed_prefix() {
        let error = exhausted_error(
            MAX_ATTEMPTS,
            "SMTP connection timed out; check the server and port",
        );
        assert_eq!(
            error,
            "gave up after 20 attempts: SMTP connection timed out; check the server and port"
        );
        assert!(is_exhausted(Some(&error)));
        for other in [
            None,
            Some(""),
            Some("the recipient turned off this kind of email notification"),
            Some("gave up after  attempts: no count"),
            Some("gave up after x attempts: not a number"),
            Some("content notification event is malformed"),
        ] {
            assert!(!is_exhausted(other), "{other:?}");
        }
        // Truncation counts characters: "\u{957f}" (the Chinese for "long") takes three bytes in UTF-8.
        assert_eq!(
            exhausted_error(20, &"\u{957f}".repeat(2000))
                .chars()
                .count(),
            1000
        );
    }
}
