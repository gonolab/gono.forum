//! Value rules for activities and credits accounts (docs/architecture.md, "Data").
//!
//! The schema has no triggers on the activity table, the per-activity child tables or the credits tables, so the
//! code guarantees these rules:
//! - The values are defined here: activity state [`State`], red packet distribution [`Distribution`]. Activity state is
//!   written only through a [`State`] binding, and SQL never contains state literals (pinned by `db::source_guards`);
//! - Combination checks at creation are in [`ActivitySpec::validate`](super::ActivitySpec::validate); distribution is
//!   decided by [`Distribution::parse`];
//! - Range and relation checks on changes are the `check_*` functions here, which write paths call in the same
//!   transaction, before writing.
//!
//! To add a new mode later, such as a red packet that "enters first, then draws", add a variant to [`Distribution`];
//! every `match` is exhaustive, so the compiler points out what needs to be filled in.

use super::valid;
use anyhow::Result;

/// Activity state. `open` is in progress, `drawing` means a lottery is being drawn and paid out; the other three are
/// terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Open,
    Drawing,
    Settled,
    Cancelled,
    Expired,
}

impl State {
    pub const ALL: [Self; 5] = [
        Self::Open,
        Self::Drawing,
        Self::Settled,
        Self::Cancelled,
        Self::Expired,
    ];

    /// The value stored in `gonof_forum_activity.state`.
    pub fn value(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Drawing => "drawing",
            Self::Settled => "settled",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.value() == value)
    }
}

/// How a red packet is distributed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distribution {
    /// Equal: each share is `budget / shares`, which must divide evenly at publication.
    Equal,
    /// Random: each share is at least 1; see `campaign::random_share`.
    Random,
}

impl Distribution {
    pub const ALL: [Self; 2] = [Self::Equal, Self::Random];

    /// The value stored in `gonof_forum_redpacket.distribution` (column width 12).
    pub fn value(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Random => "random",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.value() == value)
    }
}

/// The activity's category and initiator must be exactly the thread's category and author.
pub(crate) fn check_identity(
    kind: i64,
    owner: i64,
    thread_category: i64,
    thread_author: Option<i64>,
) -> Result<()> {
    valid(
        (1..=5).contains(&kind) && thread_category == kind && thread_author == Some(owner),
        crate::i18n::current().activity_error_identity(),
    )
}

/// The unpaid budget lies between 0 and the budget (`remaining BETWEEN 0 AND budget`).
pub(crate) fn check_remaining(budget: i64, remaining: i64) -> Result<()> {
    valid(
        budget >= 0 && (0..=budget).contains(&remaining),
        crate::i18n::current().activity_error_remaining(),
    )
}

/// A red packet's remaining shares lie between 0 and the total shares, and the total is positive.
pub(crate) fn check_packet_shares(shares: i64, remaining_shares: i64) -> Result<()> {
    valid(
        shares > 0 && (0..=shares).contains(&remaining_shares),
        crate::i18n::current().activity_error_packet_shares(),
    )
}

/// Each red packet claim amount is positive.
pub(crate) fn check_claim(amount: i64) -> Result<()> {
    valid(
        amount > 0,
        crate::i18n::current().activity_error_packet_amount(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_exactly() {
        for state in State::ALL {
            assert_eq!(State::parse(state.value()), Some(state));
        }
        for mode in Distribution::ALL {
            assert_eq!(Distribution::parse(mode.value()), Some(mode));
            assert!(
                mode.value().len() <= 12,
                "the distribution column is 12 wide"
            );
        }
        for bad in ["", "OPEN", "open ", "closed"] {
            assert_eq!(State::parse(bad), None, "{bad:?}");
        }
        for bad in ["", "Equal", "equal ", "signup"] {
            assert_eq!(Distribution::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn checks_keep_the_ranges_the_triggers_had() {
        assert!(check_identity(2, 7, 2, Some(7)).is_ok());
        for (kind, owner, category, author) in [
            (2, 7, 3, Some(7)),
            (2, 7, 2, Some(8)),
            (2, 7, 2, None),
            (0, 7, 0, Some(7)),
            (6, 7, 6, Some(7)),
        ] {
            assert!(
                check_identity(kind, owner, category, author).is_err(),
                "{kind} {owner} {category} {author:?}"
            );
        }
        assert!(check_remaining(10, 0).is_ok() && check_remaining(10, 10).is_ok());
        assert!(check_remaining(10, -1).is_err() && check_remaining(10, 11).is_err());
        assert!(check_packet_shares(3, 0).is_ok() && check_packet_shares(3, 3).is_ok());
        assert!(check_packet_shares(3, -1).is_err() && check_packet_shares(3, 4).is_err());
        assert!(check_packet_shares(0, 0).is_err());
        assert!(check_claim(1).is_ok() && check_claim(0).is_err());
    }
}
