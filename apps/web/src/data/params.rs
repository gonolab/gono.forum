//! The parameters of each data source (docs/data.md, "Sources"): closed enumerations, checked strictly and written
//! back in one normalized form. The same parser reads a block's `params` in a layout document and the query string
//! of `/data/v1/<source>`; only the layout may say `current` (the page's own board or thread), and the query string
//! writes numbers and booleans as text.
use serde_json::{Map, Value, json};

use super::Source;

/// Where parameters come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A block of a layout document or skin manifest: JSON values, `current` allowed.
    Layout,
    /// The query string of `/data/v1`: text values, no `current`.
    Api,
}

/// A parameter that is unknown, of the wrong kind or out of range; `name` is the parameter's key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamError {
    pub name: String,
}

fn bad(name: &str) -> ParamError {
    ParamError { name: name.into() }
}

/// A list's scope: the whole site (this domain's boards), one board (with its child boards for a top-level one), or
/// the page's own (the home page: the site; a board page: that board; a thread page: the thread's board).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Site,
    Board(i64),
    Current,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Latest,
    Created,
    Hot,
    Featured,
    Pinned,
}

impl Sort {
    pub const ALL: [Self; 5] = [
        Self::Latest,
        Self::Created,
        Self::Hot,
        Self::Featured,
        Self::Pinned,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Latest => "latest",
            Self::Created => "created",
            Self::Hot => "hot",
            Self::Featured => "featured",
            Self::Pinned => "pinned",
        }
    }
}

/// The window of hot threads: the site's setting (`settings.display.hot_days`) or a number of days.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Days {
    Site,
    Days(i64),
}

pub const HOT_DAYS: [i64; 4] = [1, 3, 7, 30];

/// How many records: the site's setting (`settings.display.hot_count` for hot threads, `latest_count` for the other
/// thread lists) or 1 to [`MAX_LIMIT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Site,
    Count(i64),
}

pub const MAX_LIMIT: i64 = 20;
const DEFAULT_LIMIT: i64 = 10;

/// The kinds of thread `category` takes, by the codes records use for `kind` (category 1 to 5).
pub const KINDS: [&str; 5] = ["bounty", "redpacket", "lottery", "poll", "news"];

/// A board, a thread or the page's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Current,
    Id(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parent {
    Top,
    Board(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkPlace {
    Home,
    Directory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Threads {
    pub scope: Scope,
    pub sort: Sort,
    /// Only for `hot`.
    pub days: Option<Days>,
    /// Category 1 to 5; only with `latest` and `created`, as on list pages.
    pub category: Option<i64>,
    pub limit: Limit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boards {
    pub parent: Parent,
    pub counts: bool,
    pub last_post: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rank {
    pub board: crate::ranks::Board,
    pub period: crate::ranks::Period,
    pub limit: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Params {
    Threads(Threads),
    Boards(Boards),
    MembersRecent {
        limit: i64,
    },
    MembersRank(Rank),
    MembersHonors {
        honor: Option<i64>,
        limit: i64,
    },
    MembersVerified {
        kind: Option<crate::directory::Category>,
        limit: i64,
    },
    Moderators {
        board: Target,
    },
    SiteStats {
        scope: Scope,
    },
    SiteLinks {
        place: LinkPlace,
    },
    ThreadOutline {
        tid: Target,
    },
    /// The personal sources take no parameters.
    Me,
}

/// Reads one parameter map, removing what it takes, so that keys left over are unknown.
struct Reader {
    map: Map<String, Value>,
    origin: Origin,
}

impl Reader {
    fn take(&mut self, name: &str) -> Option<Value> {
        self.map.remove(name)
    }

    /// A text value.
    fn text(&mut self, name: &str) -> Result<Option<String>, ParamError> {
        match self.take(name) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(bad(name)),
        }
    }

    /// A positive integer: a JSON number in a layout, digits in the query string.
    fn integer(value: &Value, origin: Origin) -> Option<i64> {
        match (value, origin) {
            (Value::Number(number), Origin::Layout) => number.as_i64(),
            (Value::String(text), Origin::Api)
                if !text.is_empty()
                    && text.len() <= 18
                    && text.bytes().all(|b| b.is_ascii_digit())
                    && !text.starts_with('0') =>
            {
                text.parse().ok()
            }
            _ => None,
        }
        .filter(|number| *number > 0)
    }

    fn id(&mut self, name: &str) -> Result<Option<i64>, ParamError> {
        match self.take(name) {
            None => Ok(None),
            Some(value) => Self::integer(&value, self.origin)
                .map(Some)
                .ok_or_else(|| bad(name)),
        }
    }

    fn boolean(&mut self, name: &str) -> Result<bool, ParamError> {
        match (self.take(name), self.origin) {
            (None, _) => Ok(false),
            (Some(Value::Bool(value)), Origin::Layout) => Ok(value),
            (Some(Value::String(text)), Origin::Api) if text == "true" => Ok(true),
            (Some(Value::String(text)), Origin::Api) if text == "false" => Ok(false),
            _ => Err(bad(name)),
        }
    }

    /// `"site"` or an integer from `allowed`.
    fn site_or(
        &mut self,
        name: &str,
        allowed: impl Fn(i64) -> bool,
    ) -> Result<Option<Option<i64>>, ParamError> {
        match self.take(name) {
            None => Ok(None),
            Some(Value::String(text)) if text == "site" => Ok(Some(None)),
            Some(value) => match Self::integer(&value, self.origin) {
                Some(number) if allowed(number) => Ok(Some(Some(number))),
                _ => Err(bad(name)),
            },
        }
    }

    fn limit(&mut self) -> Result<i64, ParamError> {
        match self.take("limit") {
            None => Ok(DEFAULT_LIMIT),
            Some(value) => Self::integer(&value, self.origin)
                .filter(|limit| *limit <= MAX_LIMIT)
                .ok_or_else(|| bad("limit")),
        }
    }

    /// `current` (layouts only) or an id.
    fn target(&mut self, name: &str) -> Result<Target, ParamError> {
        match self.take(name) {
            None if self.origin == Origin::Layout => Ok(Target::Current),
            None => Err(bad(name)),
            Some(Value::String(text)) if text == "current" && self.origin == Origin::Layout => {
                Ok(Target::Current)
            }
            Some(value) => Self::integer(&value, self.origin)
                .map(Target::Id)
                .ok_or_else(|| bad(name)),
        }
    }

    fn scope(&mut self) -> Result<Scope, ParamError> {
        let default = match self.origin {
            Origin::Layout => Scope::Current,
            Origin::Api => Scope::Site,
        };
        match self.text("scope")?.as_deref() {
            None => Ok(default),
            Some("site") => Ok(Scope::Site),
            Some("current") if self.origin == Origin::Layout => Ok(Scope::Current),
            Some(other) => other
                .strip_prefix("board:")
                .filter(|digits| {
                    !digits.is_empty()
                        && digits.len() <= 18
                        && !digits.starts_with('0')
                        && digits.bytes().all(|b| b.is_ascii_digit())
                })
                .and_then(|digits| digits.parse().ok())
                .map(Scope::Board)
                .ok_or_else(|| bad("scope")),
        }
    }

    fn finish(self) -> Result<(), ParamError> {
        match self.map.keys().next() {
            Some(unknown) => Err(bad(unknown)),
            None => Ok(()),
        }
    }
}

impl Params {
    /// Reads and checks the parameters of `source`; unknown keys, wrong kinds and values outside their lists are
    /// errors naming the key.
    pub fn parse(
        source: Source,
        map: &Map<String, Value>,
        origin: Origin,
    ) -> Result<Self, ParamError> {
        let mut reader = Reader {
            map: map.clone(),
            origin,
        };
        let params = match source {
            Source::Threads => {
                let scope = reader.scope()?;
                let sort = match reader.text("sort")? {
                    None => Sort::Latest,
                    Some(name) => Sort::ALL
                        .into_iter()
                        .find(|sort| sort.name() == name)
                        .ok_or_else(|| bad("sort"))?,
                };
                let days = match (
                    reader.site_or("days", |days| HOT_DAYS.contains(&days))?,
                    sort,
                ) {
                    (None, Sort::Hot) => Some(Days::Site),
                    (Some(None), Sort::Hot) => Some(Days::Site),
                    (Some(Some(days)), Sort::Hot) => Some(Days::Days(days)),
                    (None, _) => None,
                    (Some(_), _) => return Err(bad("days")),
                };
                let category = match reader.text("category")? {
                    None => None,
                    Some(kind) if matches!(sort, Sort::Latest | Sort::Created) => Some(
                        KINDS
                            .iter()
                            .position(|known| *known == kind)
                            .map(|index| index as i64 + 1)
                            .ok_or_else(|| bad("category"))?,
                    ),
                    Some(_) => return Err(bad("category")),
                };
                let limit =
                    match reader.site_or("limit", |limit| (1..=MAX_LIMIT).contains(&limit))? {
                        None => Limit::Count(DEFAULT_LIMIT),
                        Some(None) => Limit::Site,
                        Some(Some(limit)) => Limit::Count(limit),
                    };
                Self::Threads(Threads {
                    scope,
                    sort,
                    days,
                    category,
                    limit,
                })
            }
            Source::Boards => {
                let parent = match reader.take("parent") {
                    None => Parent::Top,
                    Some(Value::String(text)) if text == "top" => Parent::Top,
                    Some(value) => Reader::integer(&value, origin)
                        .map(Parent::Board)
                        .ok_or_else(|| bad("parent"))?,
                };
                Self::Boards(Boards {
                    parent,
                    counts: reader.boolean("counts")?,
                    last_post: reader.boolean("last_post")?,
                })
            }
            Source::MembersRecent => Self::MembersRecent {
                limit: reader.limit()?,
            },
            Source::MembersRank => {
                let board = match reader.text("board")? {
                    None => crate::ranks::Board::Growth,
                    Some(name) => crate::ranks::Board::parse(&name).ok_or_else(|| bad("board"))?,
                };
                let period = match reader.text("period")? {
                    None => crate::ranks::Period::Month,
                    Some(name) => {
                        crate::ranks::Period::parse(&name).ok_or_else(|| bad("period"))?
                    }
                };
                Self::MembersRank(Rank {
                    board,
                    period,
                    limit: reader.limit()?,
                })
            }
            Source::MembersHonors => Self::MembersHonors {
                honor: reader.id("honor")?,
                limit: reader.limit()?,
            },
            Source::MembersVerified => Self::MembersVerified {
                kind: match reader.text("kind")? {
                    None => None,
                    Some(name) => Some(
                        crate::directory::Category::ALL
                            .into_iter()
                            .find(|category| category.slug() == name)
                            .ok_or_else(|| bad("kind"))?,
                    ),
                },
                limit: reader.limit()?,
            },
            Source::Moderators => Self::Moderators {
                board: reader.target("board")?,
            },
            Source::SiteStats => Self::SiteStats {
                scope: reader.scope()?,
            },
            Source::SiteLinks => Self::SiteLinks {
                place: match reader.text("place")?.as_deref() {
                    None | Some("home") => LinkPlace::Home,
                    Some("directory") => LinkPlace::Directory,
                    Some(_) => return Err(bad("place")),
                },
            },
            Source::ThreadOutline => Self::ThreadOutline {
                tid: reader.target("tid")?,
            },
            Source::MeCard | Source::MeCounts | Source::MeCheckin => Self::Me,
        };
        reader.finish()?;
        Ok(params)
    }

    /// The parameters in their normalized form, every key written, as the envelope's `params` and in saved
    /// layouts.
    pub fn json(&self) -> Value {
        fn scope(scope: Scope) -> Value {
            match scope {
                Scope::Site => json!("site"),
                Scope::Board(fid) => json!(format!("board:{fid}")),
                Scope::Current => json!("current"),
            }
        }
        fn target(target: Target) -> Value {
            match target {
                Target::Current => json!("current"),
                Target::Id(id) => json!(id),
            }
        }
        match self {
            Self::Threads(threads) => json!({
                "scope": scope(threads.scope),
                "sort": threads.sort.name(),
                "days": match threads.days {
                    None => Value::Null,
                    Some(Days::Site) => json!("site"),
                    Some(Days::Days(days)) => json!(days),
                },
                "category": threads.category.map(|category| KINDS[(category - 1) as usize]),
                "limit": match threads.limit {
                    Limit::Site => json!("site"),
                    Limit::Count(limit) => json!(limit),
                },
            }),
            Self::Boards(boards) => json!({
                "parent": match boards.parent {
                    Parent::Top => json!("top"),
                    Parent::Board(fid) => json!(fid),
                },
                "counts": boards.counts,
                "last_post": boards.last_post,
            }),
            Self::MembersRecent { limit } => json!({"limit": limit}),
            Self::MembersRank(rank) => json!({
                "board": rank.board.slug(),
                "period": rank.period.slug(),
                "limit": rank.limit,
            }),
            Self::MembersHonors { honor, limit } => json!({"honor": honor, "limit": limit}),
            Self::MembersVerified { kind, limit } => {
                json!({"kind": kind.map(crate::directory::Category::slug), "limit": limit})
            }
            Self::Moderators { board } => json!({"board": target(*board)}),
            Self::SiteStats { scope: value } => json!({"scope": scope(*value)}),
            Self::SiteLinks { place } => json!({"place": match place {
                LinkPlace::Home => "home",
                LinkPlace::Directory => "directory",
            }}),
            Self::ThreadOutline { tid } => json!({"tid": target(*tid)}),
            Self::Me => json!({}),
        }
    }

    /// The normalized parameters as a layout stores them: `null` values left out, so that reading them back gives
    /// the same parameters.
    pub fn stored(&self) -> Map<String, Value> {
        match self.json() {
            Value::Object(map) => map
                .into_iter()
                .filter(|(_, value)| !value.is_null())
                .collect(),
            _ => Map::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(source: Source, params: Value) -> Result<Params, ParamError> {
        Params::parse(source, params.as_object().unwrap(), Origin::Layout)
    }

    fn api(source: Source, pairs: &[(&str, &str)]) -> Result<Params, ParamError> {
        let map = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), json!(value)))
            .collect();
        Params::parse(source, &map, Origin::Api)
    }

    #[test]
    fn thread_parameters_default_and_normalize() {
        let params = layout(Source::Threads, json!({})).unwrap();
        assert_eq!(
            params.json(),
            json!({"scope": "current", "sort": "latest", "days": null, "category": null, "limit": 10})
        );
        let hot = layout(
            Source::Threads,
            json!({"sort": "hot", "days": 7, "limit": "site"}),
        )
        .unwrap();
        assert_eq!(
            hot.json(),
            json!({"scope": "current", "sort": "hot", "days": 7, "category": null, "limit": "site"})
        );
        assert_eq!(
            layout(Source::Threads, Value::Object(hot.stored())).unwrap(),
            hot,
            "a normalized form reads back the same"
        );
        let api_hot = api(
            Source::Threads,
            &[("sort", "hot"), ("days", "30"), ("scope", "board:4")],
        )
        .unwrap();
        assert_eq!(
            api_hot.json(),
            json!({"scope": "board:4", "sort": "hot", "days": 30, "category": null, "limit": 10})
        );
        assert_eq!(
            api(Source::Threads, &[]).unwrap().json()["scope"],
            "site",
            "the query string has no page"
        );
        let poll = layout(
            Source::Threads,
            json!({"sort": "created", "category": "poll"}),
        )
        .unwrap();
        assert_eq!(poll.json()["category"], "poll");
        assert!(matches!(
            poll,
            Params::Threads(Threads {
                category: Some(4),
                ..
            })
        ));
    }

    #[test]
    fn thread_parameters_refuse_what_no_index_serves() {
        for (params, name) in [
            (json!({"sort": "random"}), "sort"),
            (json!({"sort": "latest", "days": 7}), "days"),
            (json!({"sort": "hot", "days": 14}), "days"),
            (json!({"sort": "hot", "category": "poll"}), "category"),
            (json!({"sort": "featured", "category": "poll"}), "category"),
            (json!({"category": "event"}), "category"),
            (json!({"limit": 0}), "limit"),
            (json!({"limit": 21}), "limit"),
            (json!({"limit": "10"}), "limit"),
            (json!({"scope": "board:0"}), "scope"),
            (json!({"scope": "board:x"}), "scope"),
            (json!({"scope": "everywhere"}), "scope"),
            (json!({"page": 2}), "page"),
        ] {
            assert_eq!(
                layout(Source::Threads, params.clone()),
                Err(bad(name)),
                "{params}"
            );
        }
        for (pairs, name) in [
            (&[("scope", "current")][..], "scope"),
            (&[("limit", "010")][..], "limit"),
            (&[("limit", "+5")][..], "limit"),
            (&[("days", "7")][..], "days"),
        ] {
            assert_eq!(api(Source::Threads, pairs), Err(bad(name)), "{pairs:?}");
        }
    }

    #[test]
    fn other_sources_take_their_own_parameters() {
        assert_eq!(
            layout(Source::Boards, json!({"counts": true}))
                .unwrap()
                .json(),
            json!({"parent": "top", "counts": true, "last_post": false})
        );
        assert_eq!(
            api(Source::Boards, &[("parent", "3"), ("last_post", "true")])
                .unwrap()
                .json(),
            json!({"parent": 3, "counts": false, "last_post": true})
        );
        assert_eq!(
            api(Source::Boards, &[("counts", "yes")]),
            Err(bad("counts"))
        );
        assert_eq!(
            layout(
                Source::MembersRank,
                json!({"board": "likes", "period": "24h", "limit": 5})
            )
            .unwrap()
            .json(),
            json!({"board": "likes", "period": "24h", "limit": 5})
        );
        assert_eq!(
            layout(Source::MembersRank, json!({"period": "day"})),
            Err(bad("period"))
        );
        assert_eq!(
            layout(Source::MembersVerified, json!({"kind": "blogger"}))
                .unwrap()
                .json(),
            json!({"kind": "blogger", "limit": 10})
        );
        assert_eq!(
            layout(Source::Moderators, json!({})).unwrap().json(),
            json!({"board": "current"})
        );
        assert_eq!(api(Source::Moderators, &[]), Err(bad("board")));
        assert_eq!(
            api(Source::ThreadOutline, &[("tid", "103")])
                .unwrap()
                .json(),
            json!({"tid": 103})
        );
        assert_eq!(
            api(Source::ThreadOutline, &[("tid", "current")]),
            Err(bad("tid"))
        );
        assert_eq!(
            layout(Source::SiteLinks, json!({"place": "directory"}))
                .unwrap()
                .json(),
            json!({"place": "directory"})
        );
        assert_eq!(layout(Source::MeCard, json!({})).unwrap(), Params::Me);
        assert_eq!(layout(Source::MeCard, json!({"uid": 3})), Err(bad("uid")));
    }

    #[test]
    fn stored_parameters_leave_out_nulls() {
        let params = layout(Source::Threads, json!({"sort": "pinned", "limit": 5})).unwrap();
        let stored = params.stored();
        assert!(!stored.contains_key("days"));
        assert!(!stored.contains_key("category"));
        assert_eq!(
            layout(Source::Threads, Value::Object(stored)).unwrap(),
            params
        );
    }
}
