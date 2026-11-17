//! The records data sources return (docs/data.md, "Records"): view types written for this purpose, so that nothing
//! internal (a link's note or applicant, a thread's withdrawal or version) is ever serialized. Every field is always
//! present, `null` where it does not apply. Times carry the raw instant and the text of the request's language;
//! addresses are relative to the site and carry its path prefix.
use serde::Serialize;
use serde_json::{Value, json};

/// A moment: the instant in UTC to the second, the text a list shows (relative when the site says so) and the full
/// date for a tooltip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Time {
    pub at: String,
    pub text: String,
    pub title: String,
}

impl Time {
    /// The time of `seconds` in this request's time settings (`clock::current`).
    pub fn of(seconds: i64) -> Self {
        let format = crate::clock::current();
        let absolute = crate::clock::TimeFormat {
            relative_hours: None,
            ..format
        };
        Self {
            at: crate::clock::iso(seconds),
            text: crate::clock::short_datetime(seconds, &format),
            title: crate::clock::datetime(seconds, &absolute),
        }
    }
}

/// The tile shown when a member has no avatar image, and the image address by uid (transparent when the member has
/// none, so the tile shows through).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Avatar {
    pub src: Option<String>,
    pub initials: String,
    /// One of the six tile colors, 0 to 5, from the uid (the `avatar-<tone>` class).
    pub tone: i64,
}

/// A member as records name one. A deleted or unknown author has no uid or address, and the name is "Deleted user"
/// in the request's language.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemberRef {
    pub uid: Option<i64>,
    pub name: String,
    pub href: Option<String>,
    pub avatar: Avatar,
}

/// The number of avatar tile colors (`.avatar-0` to `.avatar-5`).
const TONES: i64 = 6;

impl MemberRef {
    /// A member by uid and name; `None` for a deleted or unknown author.
    pub fn of(uid: Option<i64>, name: &str) -> Self {
        let uid = uid.filter(|uid| *uid > 0);
        let name = match uid {
            Some(_) => name.to_owned(),
            None => crate::i18n::current().common_deleted_user().to_owned(),
        };
        Self {
            uid,
            href: uid.map(|uid| crate::site::href(crate::urls::space(uid))),
            avatar: Avatar {
                src: uid.map(|uid| crate::site::href(format!("/avatar/{uid}"))),
                initials: name.chars().take(2).collect(),
                tone: uid.map_or(0, |uid| uid.rem_euclid(TONES)),
            },
            name,
        }
    }
}

/// A board as records name one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BoardRef {
    pub fid: i64,
    pub name: String,
    pub href: String,
    pub parent_fid: Option<i64>,
}

impl BoardRef {
    pub fn of(forum: &crate::organization::ForumRecord) -> Self {
        Self {
            fid: forum.fid,
            name: forum.name.clone(),
            href: crate::site::href(format!("/{}", forum.slug)),
            parent_fid: forum.parent_fid,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LastReply {
    pub pid: i64,
    pub member: MemberRef,
    pub href: String,
    /// The reply's first characters as plain text.
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Thumb {
    pub src: String,
    pub width: u32,
    pub height: u32,
}

/// One thread of `threads`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ThreadRecord {
    pub tid: i64,
    pub title: String,
    pub href: String,
    pub board: Option<BoardRef>,
    /// `bounty`, `redpacket`, `lottery`, `poll`, `news` or `null`.
    pub kind: Option<&'static str>,
    /// The pin level, 0 to 3.
    pub pin: i64,
    pub featured: bool,
    pub closed: bool,
    pub members_only: bool,
    pub author: MemberRef,
    pub created: Time,
    pub last_activity: Time,
    pub replies: i64,
    pub favorites: i64,
    pub tips: i64,
    pub last_reply: Option<LastReply>,
    /// Pages of the thread in the view lists link to (the reader's flat or nested choice).
    pub pages: i64,
    /// The highest floor number, for comparing with a reader's last seen floor.
    pub max_position: i64,
    /// Approximate.
    pub views: i64,
    pub thumbs: Vec<Thumb>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BoardCounts {
    pub threads: i64,
    pub today: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LastPost {
    pub tid: i64,
    pub title: String,
    pub href: String,
    pub at: Time,
    pub member: MemberRef,
    pub summary: Option<String>,
    /// The thread's highest floor number, for comparing with the floor a reader saw last.
    pub max_position: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Image {
    pub src: String,
}

/// One board of `boards`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BoardRecord {
    pub fid: i64,
    pub slug: String,
    pub name: String,
    pub href: String,
    /// Plain text.
    pub description: String,
    pub parent_fid: Option<i64>,
    pub icon: Option<Image>,
    pub color: Option<String>,
    pub glyph: Option<String>,
    /// `public` or `members`.
    pub access: &'static str,
    /// With `counts`, for boards anonymous visitors can read; `null` otherwise.
    pub counts: Option<BoardCounts>,
    /// With `last_post`.
    pub last_post: Option<LastPost>,
    pub children: Vec<BoardRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecentMember {
    #[serde(flatten)]
    pub member: MemberRef,
    pub joined: Time,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ranked {
    pub position: i64,
    #[serde(flatten)]
    pub member: MemberRef,
    pub score: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HonorRef {
    pub id: i64,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HonorGroup {
    pub honor: HonorRef,
    pub members: Vec<MemberRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Site {
    pub href: String,
    pub host: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerifiedMember {
    #[serde(flatten)]
    pub member: MemberRef,
    pub site: Option<Site>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerifiedGroup {
    /// The verification category's code (`merchant`, `agent`, `blogger`, `webmaster`, `channel`).
    pub kind: &'static str,
    pub name: String,
    pub members: Vec<VerifiedMember>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Moderator {
    #[serde(flatten)]
    pub member: MemberRef,
    /// `moderator` or `trainee`.
    pub role: &'static str,
    /// Appointed to this board's parent rather than to the board itself.
    pub inherited: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub threads: Option<i64>,
    pub replies: Option<i64>,
    pub contributors: Option<i64>,
    pub today: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Link {
    pub name: String,
    pub url: String,
    pub domain: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Point {
    pub position: i64,
    pub at: Time,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PageMark {
    pub page: i64,
    pub from: i64,
    pub to: i64,
    pub first_at: Time,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DateMark {
    pub position: i64,
    pub page: i64,
    pub at: Time,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Outline {
    pub tid: i64,
    pub pages: i64,
    pub per_page: i64,
    pub last_position: i64,
    pub first: Point,
    pub last: Point,
    pub page_marks: Vec<PageMark>,
    pub date_marks: Vec<DateMark>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Level {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Credits {
    pub word: String,
    pub amount: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Verification {
    pub email: bool,
    pub mobile: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Card {
    #[serde(flatten)]
    pub member: MemberRef,
    pub level: Level,
    /// `admin`, `global_moderator`, `moderator`, `trainee`, `reviewer` or `member`.
    pub role: &'static str,
    pub credits: Credits,
    pub verification: Verification,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub notifications_unread: i64,
    pub messages_unread: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Checkin {
    pub today: bool,
    pub streak: i64,
}

/// The records of one source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Records {
    Threads(Vec<ThreadRecord>),
    Boards(Vec<BoardRecord>),
    Recent(Vec<RecentMember>),
    Ranked(Vec<Ranked>),
    Honors(Vec<HonorGroup>),
    Verified(Vec<VerifiedGroup>),
    Moderators(Vec<Moderator>),
    Stats(Vec<Stats>),
    Links(Vec<Link>),
    Outline(Vec<Outline>),
    Card(Vec<Card>),
    Counts(Vec<Counts>),
    Checkin(Vec<Checkin>),
}

impl Records {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Threads(items) => items.is_empty(),
            Self::Boards(items) => items.is_empty(),
            Self::Recent(items) => items.is_empty(),
            Self::Ranked(items) => items.is_empty(),
            Self::Honors(items) => items.is_empty(),
            Self::Verified(items) => items.is_empty(),
            Self::Moderators(items) => items.is_empty(),
            Self::Stats(items) => items.is_empty(),
            Self::Links(items) => items.is_empty(),
            Self::Outline(items) => items.is_empty(),
            Self::Card(items) => items.is_empty(),
            Self::Counts(items) => items.is_empty(),
            Self::Checkin(items) => items.is_empty(),
        }
    }
}

/// What one source returned: its records and, for `members.rank`, when the board was computed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub records: Records,
    pub computed_at: Option<Time>,
}

impl Output {
    pub fn new(records: Records) -> Self {
        Self {
            records,
            computed_at: None,
        }
    }
}

/// The envelope of `/data/v1` and of the page data block's `blocks`: `{"v", "source", "params", "items",
/// "generated_at"}`, and `computed_at` for `members.rank`.
pub fn envelope(source: super::Source, params: Value, output: &Output, now: i64) -> Value {
    let mut value = json!({
        "v": super::VERSION,
        "source": source.name(),
        "params": params,
        "items": output.records,
        "generated_at": crate::clock::iso(now),
    });
    if source == super::Source::MembersRank {
        value["computed_at"] = serde_json::to_value(&output.computed_at).unwrap_or(Value::Null);
    }
    value
}
