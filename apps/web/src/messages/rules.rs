//! Value and participant rules of the three private-message tables (docs/architecture.md, "Data").
//!
//! The schema has no triggers, so the code guarantees:
//! - The conversation key `pair_key` is "smaller uid:larger uid", both parts positive integers without leading zeros:
//!   [`pair_key`] builds it and [`parse_pair_key`] parses it, each the inverse of the other;
//! - Before sending, participants are checked under the lock ([`check_participants`]): sender and recipient are
//!   exactly the two sides of the conversation key, and the two state rows are exactly this pair of uids, each the
//!   other's counterpart. Only values already read back under the lock are used, with no extra query;
//! - Body length and append-only columns (the conversation's last message, the state rows' read position and last
//!   time) are guaranteed by the conditional updates of `send_once` and the mark-read entry point; message rows and
//!   conversation keys never change, pinned by `db::source_guards`.

use anyhow::{Result, ensure};

/// Conversation key: both uids positive and distinct, the smaller first.
pub fn pair_key(a: i64, b: i64) -> Result<String> {
    ensure!(
        a > 0 && b > 0 && a != b,
        "the two sides of a conversation must be two different positive uids: {a}, {b}"
    );
    Ok(format!("{}:{}", a.min(b), a.max(b)))
}

/// Parses a conversation key; any bad format (leading zeros, non-digits, reversed order, both sides equal) returns
/// `None`.
pub fn parse_pair_key(key: &str) -> Option<(i64, i64)> {
    let (low, high) = key.split_once(':')?;
    let part = |text: &str| {
        (!text.is_empty() && !text.starts_with('0') && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse::<i64>().ok())
            .flatten()
    };
    let (low, high) = (part(low)?, part(high)?);
    (low < high).then_some((low, high))
}

/// Participant check before sending: the two sides of the conversation key are the sender and the recipient; the
/// state rows read back under the lock are exactly two, each naming the other side as counterpart.
/// `rows` is `(uid, counterpart_uid)`.
pub fn check_participants(
    key: &str,
    sender: i64,
    counterpart: i64,
    rows: &[(i64, i64)],
) -> Result<()> {
    let pair = parse_pair_key(key);
    ensure!(
        pair == Some((sender.min(counterpart), sender.max(counterpart))) && sender != counterpart,
        "conversation {key} is not between {sender} and {counterpart}"
    );
    let mut seen: Vec<(i64, i64)> = rows.to_vec();
    seen.sort_unstable();
    let mut expected = vec![(sender, counterpart), (counterpart, sender)];
    expected.sort_unstable();
    ensure!(
        seen == expected,
        "the state rows of conversation {key} do not match its two sides: {rows:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_keys_are_ordered_positive_and_round_trip() {
        assert_eq!(pair_key(7, 3).unwrap(), "3:7");
        assert_eq!(pair_key(3, 7).unwrap(), "3:7");
        assert_eq!(parse_pair_key("3:7"), Some((3, 7)));
        for (a, b) in [(3, 3), (0, 7), (-1, 7)] {
            assert!(pair_key(a, b).is_err(), "{a} {b}");
        }
        // Forms the former conversation trigger rejected: reversed order, leading zeros, zero, non-digits, missing
        // part, extra parts.
        for bad in [
            "7:3", "03:7", "0:7", "3:7 ", "a:7", "3", "3:7:9", ":7", "3:", "3:3", "+3:7",
        ] {
            assert_eq!(parse_pair_key(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn participants_must_be_the_pair_and_its_two_state_rows() {
        let key = "3:7";
        assert!(check_participants(key, 7, 3, &[(3, 7), (7, 3)]).is_ok());
        assert!(check_participants(key, 3, 7, &[(7, 3), (3, 7)]).is_ok());
        // The conversation is not between these two, a state row is missing, there is an extra one, or the
        // counterpart is wrong.
        assert!(check_participants(key, 7, 4, &[(4, 7), (7, 4)]).is_err());
        assert!(check_participants("3:8", 7, 3, &[(3, 7), (7, 3)]).is_err());
        assert!(check_participants(key, 7, 3, &[(7, 3)]).is_err());
        assert!(check_participants(key, 7, 3, &[(3, 7), (7, 3), (9, 3)]).is_err());
        assert!(check_participants(key, 7, 3, &[(3, 9), (7, 3)]).is_err());
        assert!(check_participants(key, 7, 7, &[(7, 7), (7, 7)]).is_err());
    }
}
