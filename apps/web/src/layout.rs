//! Places and blocks (docs/skins.md, "Places and blocks"): the `places` of a layout document, which blocks each fixed
//! place of a page shows. A block is a data source with parameters and a view, or one of the core blocks that the
//! templates had written by hand before layouts.
//!
//! A page's places come from, place by place: the domain's override document, the site's document, the skin's
//! manifest (`[layout]`), and the core default layout ([`core_default`]), which renders the pages as they were before
//! layouts. Documents and skins are checked strictly when they are saved or imported ([`Places::parse`]) and leniently
//! when pages read them ([`Places::read`]): a block that no longer passes is skipped and logged, never an error page.
use std::sync::LazyLock;

use serde_json::{Map, Value, json};

use crate::data::{
    MAX_BLOCKS_PER_PAGE, Source,
    params::{Origin, Params},
};

/// A fixed place on a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    HomeAside,
    BoardAside,
    ThreadAside,
    HeadHome,
    HeadBoard,
    HeadThread,
    Footer,
    SideNav,
}

impl Place {
    pub const ALL: [Self; 8] = [
        Self::HomeAside,
        Self::BoardAside,
        Self::ThreadAside,
        Self::HeadHome,
        Self::HeadBoard,
        Self::HeadThread,
        Self::Footer,
        Self::SideNav,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::HomeAside => "home.aside",
            Self::BoardAside => "board.aside",
            Self::ThreadAside => "thread.aside",
            Self::HeadHome => "page.head.home",
            Self::HeadBoard => "page.head.board",
            Self::HeadThread => "page.head.thread",
            Self::Footer => "footer",
            Self::SideNav => "side.nav",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|place| place.name() == name)
    }

    /// The most blocks the place takes.
    pub fn capacity(self) -> usize {
        match self {
            Self::HomeAside | Self::BoardAside => 8,
            Self::ThreadAside => 6,
            Self::HeadHome | Self::HeadBoard | Self::HeadThread => 2,
            Self::Footer | Self::SideNav => 4,
        }
    }

    pub fn aside(self) -> bool {
        matches!(self, Self::HomeAside | Self::BoardAside | Self::ThreadAside)
    }

    /// Whether a block of this kind and view may sit here (docs/skins.md, "Place rules").
    pub fn accepts(self, kind: Kind, view: &str) -> bool {
        match (self, kind) {
            (Self::ThreadAside, Kind::Source(Source::ThreadOutline)) => true,
            (_, Kind::Source(Source::ThreadOutline)) => false,
            (place, Kind::Source(_)) if place.aside() => true,
            (place, Kind::Core(core)) if place.aside() => !matches!(core, Core::BoardHead),
            (Self::HeadHome | Self::HeadThread, Kind::Core(Core::Markdown)) => true,
            (Self::HeadBoard, Kind::Core(Core::Markdown | Core::BoardHead)) => true,
            (
                Self::HeadHome | Self::HeadBoard | Self::HeadThread,
                Kind::Source(Source::SiteStats),
            ) => view == "compact",
            (Self::Footer, Kind::Core(Core::Markdown)) => true,
            (Self::Footer, Kind::Source(Source::SiteLinks | Source::SiteStats)) => true,
            (Self::SideNav, Kind::Core(Core::NavBoards | Core::NavLinks | Core::Markdown)) => true,
            _ => false,
        }
    }
}

/// The pages that have places, and the places each one shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Board,
    Thread,
}

impl Page {
    pub const ALL: [Self; 3] = [Self::Home, Self::Board, Self::Thread];

    pub fn places(self) -> [Place; 4] {
        match self {
            Self::Home => [
                Place::SideNav,
                Place::HeadHome,
                Place::HomeAside,
                Place::Footer,
            ],
            Self::Board => [
                Place::SideNav,
                Place::HeadBoard,
                Place::BoardAside,
                Place::Footer,
            ],
            Self::Thread => [
                Place::SideNav,
                Place::HeadThread,
                Place::ThreadAside,
                Place::Footer,
            ],
        }
    }
}

/// The blocks the templates wrote by hand before layouts, each with its one view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Core {
    /// The signed-in member's card, or the sign-in card for visitors.
    MemberCard,
    /// The visitor's recently viewed threads, which `site.js` fills from local storage.
    RecentLocal,
    /// The site statistics with the newest members.
    SiteOverview,
    /// The friend links with the link to apply for one.
    FriendLinks,
    /// A board page's head: name, icon, follow and filter controls, description, moderators and posting guide.
    BoardHead,
    /// The board tree with the new-thread button.
    NavBoards,
    /// The entries of the header navigation document.
    NavLinks,
    /// Text written in Markdown, per language.
    Markdown,
}

impl Core {
    pub const ALL: [Self; 8] = [
        Self::MemberCard,
        Self::RecentLocal,
        Self::SiteOverview,
        Self::FriendLinks,
        Self::BoardHead,
        Self::NavBoards,
        Self::NavLinks,
        Self::Markdown,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::MemberCard => "member-card",
            Self::RecentLocal => "recent-local",
            Self::SiteOverview => "site-overview",
            Self::FriendLinks => "friend-links",
            Self::BoardHead => "board-head",
            Self::NavBoards => "nav-boards",
            Self::NavLinks => "nav-links",
            Self::Markdown => "markdown",
        }
    }

    /// The one view of the block.
    pub fn view(self) -> &'static str {
        match self {
            Self::MemberCard | Self::SiteOverview => "card",
            Self::RecentLocal | Self::FriendLinks | Self::NavLinks => "list",
            Self::BoardHead => "head",
            Self::NavBoards => "tree",
            Self::Markdown => "prose",
        }
    }
}

/// What a block shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Source(Source),
    Core(Core),
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Source(source) => source.name(),
            Self::Core(core) => core.name(),
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Source::parse(name).map(Self::Source).or_else(|| {
            Core::ALL
                .into_iter()
                .find(|core| core.name() == name)
                .map(Self::Core)
        })
    }

    pub fn views(self) -> &'static [&'static str] {
        match self {
            Self::Source(source) => source.views(),
            Self::Core(core) => match core {
                Core::MemberCard | Core::SiteOverview => &["card"],
                Core::RecentLocal | Core::FriendLinks | Core::NavLinks => &["list"],
                Core::BoardHead => &["head"],
                Core::NavBoards => &["tree"],
                Core::Markdown => &["prose"],
            },
        }
    }

    /// Core cards carry their own heading; data-source blocks and Markdown take a title.
    pub fn titled(self) -> bool {
        matches!(self, Self::Source(_) | Self::Core(Core::Markdown))
    }
}

/// The translation keys a block title may name (`block-title-*` in `locales/<language>/layout.ftl`).
pub const TITLE_KEYS: [&str; 16] = [
    "block-title-hot",
    "block-title-latest",
    "block-title-active",
    "block-title-featured",
    "block-title-pinned",
    "block-title-boards",
    "block-title-new-members",
    "block-title-ranks",
    "block-title-honors",
    "block-title-verified",
    "block-title-moderators",
    "block-title-stats",
    "block-title-links",
    "block-title-outline",
    "block-title-notice",
    "block-title-navigation",
];

/// The text of a title key in `t`.
pub fn title_text(key: &str, t: crate::i18n::Lang) -> &'static str {
    match key {
        "block-title-hot" => t.block_title_hot(),
        "block-title-latest" => t.block_title_latest(),
        "block-title-active" => t.block_title_active(),
        "block-title-featured" => t.block_title_featured(),
        "block-title-pinned" => t.block_title_pinned(),
        "block-title-boards" => t.block_title_boards(),
        "block-title-new-members" => t.block_title_new_members(),
        "block-title-ranks" => t.block_title_ranks(),
        "block-title-honors" => t.block_title_honors(),
        "block-title-verified" => t.block_title_verified(),
        "block-title-moderators" => t.block_title_moderators(),
        "block-title-stats" => t.block_title_stats(),
        "block-title-links" => t.block_title_links(),
        "block-title-outline" => t.block_title_outline(),
        "block-title-notice" => t.block_title_notice(),
        _ => t.block_title_navigation(),
    }
}

/// Text written per language: English required, the others by language tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texts(pub Vec<(String, String)>);

impl Texts {
    /// The text in `tag`, or the English one.
    pub fn get(&self, tag: &str) -> &str {
        self.0
            .iter()
            .find(|(lang, _)| lang == tag)
            .or_else(|| self.0.iter().find(|(lang, _)| lang == "en"))
            .map_or("", |(_, text)| text.as_str())
    }

    fn json(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(lang, text)| (lang.clone(), json!(text)))
                .collect(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Title {
    Key(&'static str),
    Text(Texts),
}

impl Title {
    pub fn text(&self, t: crate::i18n::Lang) -> String {
        match self {
            Self::Key(key) => title_text(key, t).to_owned(),
            Self::Text(texts) => texts.get(t.tag()).to_owned(),
        }
    }
}

/// Who sees a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    All,
    Guests,
    Members,
}

impl Show {
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Guests => "guests",
            Self::Members => "members",
        }
    }

    pub fn shows(self, signed_in: bool) -> bool {
        match self {
            Self::All => true,
            Self::Guests => !signed_in,
            Self::Members => signed_in,
        }
    }
}

/// One block of a place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Unique in its document; written as `data-block`.
    pub id: String,
    pub kind: Kind,
    /// The parameters of a data-source block.
    pub params: Option<Params>,
    pub view: &'static str,
    pub title: Option<Title>,
    pub show: Show,
    /// Kept when the aside folds away on phones (`panel--keep`).
    pub keep_on_phone: bool,
    /// A Markdown block's text.
    pub body: Option<Texts>,
}

/// Most characters of a title written as text.
pub const TITLE_CHARS: usize = 40;
/// Most bytes of a Markdown block per language.
pub const MARKDOWN_BYTES: usize = 4096;

/// Why a layout was refused, with the path of the field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutError {
    pub path: String,
    pub problem: Problem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Not a key, place, source, view or value this core knows.
    Unknown,
    /// Of the wrong kind or out of range.
    Invalid,
    /// A required field is missing.
    Missing,
    /// More blocks than the place, or a page, takes.
    TooMany,
    /// An id used twice.
    Duplicate,
    /// A block the place does not take.
    NotHere,
    /// A text longer than allowed.
    TooLong,
}

impl Problem {
    pub const ALL: [Self; 7] = [
        Self::Unknown,
        Self::Invalid,
        Self::Missing,
        Self::TooMany,
        Self::Duplicate,
        Self::NotHere,
        Self::TooLong,
    ];

    /// The code a stored finding keeps (worded when shown).
    pub fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Invalid => "invalid",
            Self::Missing => "missing",
            Self::TooMany => "too_many",
            Self::Duplicate => "duplicate",
            Self::NotHere => "not_here",
            Self::TooLong => "too_long",
        }
    }
}

impl LayoutError {
    fn at(path: impl Into<String>, problem: Problem) -> Self {
        Self {
            path: path.into(),
            problem,
        }
    }

    /// A finding stored as codes, read back for wording; an unknown code words as invalid.
    pub fn from_code(path: &str, code: &str) -> Self {
        Self {
            path: path.to_owned(),
            problem: Problem::ALL
                .into_iter()
                .find(|problem| problem.code() == code)
                .unwrap_or(Problem::Invalid),
        }
    }

    /// The problem in the request's language.
    pub fn message(&self, t: crate::i18n::Lang) -> String {
        let path = self.path.clone();
        match self.problem {
            Problem::Unknown => t.layout_error_unknown(path),
            Problem::Invalid => t.layout_error_invalid(path),
            Problem::Missing => t.layout_error_missing(path),
            Problem::TooMany => t.layout_error_too_many(path),
            Problem::Duplicate => t.layout_error_duplicate(path),
            Problem::NotHere => t.layout_error_not_here(path),
            Problem::TooLong => t.layout_error_too_long(path),
        }
    }
}

fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn valid_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let first = parts.next().unwrap_or("");
    (2..=3).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

/// Texts by language: an object of language tags to strings, English required, each within `limit` (characters or
/// bytes as `chars` says).
fn texts(value: &Value, path: &str, limit: usize, chars: bool) -> Result<Texts, LayoutError> {
    let object = value
        .as_object()
        .ok_or_else(|| LayoutError::at(path, Problem::Invalid))?;
    let mut found = Vec::new();
    for (tag, text) in object {
        let at = format!("{path}.{tag}");
        if !valid_tag(tag) {
            return Err(LayoutError::at(at, Problem::Unknown));
        }
        let text = text
            .as_str()
            .ok_or_else(|| LayoutError::at(&at, Problem::Invalid))?
            .trim();
        let size = if chars {
            text.chars().count()
        } else {
            text.len()
        };
        if text.is_empty() {
            return Err(LayoutError::at(at, Problem::Invalid));
        }
        if size > limit {
            return Err(LayoutError::at(at, Problem::TooLong));
        }
        found.push((tag.clone(), text.to_owned()));
    }
    if !found.iter().any(|(tag, _)| tag == "en") {
        return Err(LayoutError::at(format!("{path}.en"), Problem::Missing));
    }
    Ok(Texts(found))
}

impl Block {
    /// Reads one block strictly; `path` is where it sits (`places.home.aside[2]`).
    pub fn parse(value: &Value, place: Place, path: &str) -> Result<Self, LayoutError> {
        let object = value
            .as_object()
            .ok_or_else(|| LayoutError::at(path, Problem::Invalid))?;
        for key in object.keys() {
            if !matches!(
                key.as_str(),
                "id" | "source" | "params" | "view" | "title" | "show" | "keep_on_phone" | "body"
            ) {
                return Err(LayoutError::at(format!("{path}.{key}"), Problem::Unknown));
            }
        }
        let id = match object.get("id") {
            Some(Value::String(id)) if valid_id(id) => id.clone(),
            Some(_) => return Err(LayoutError::at(format!("{path}.id"), Problem::Invalid)),
            None => return Err(LayoutError::at(format!("{path}.id"), Problem::Missing)),
        };
        let kind = match object.get("source") {
            Some(Value::String(name)) => Kind::parse(name)
                .ok_or_else(|| LayoutError::at(format!("{path}.source"), Problem::Unknown))?,
            Some(_) => return Err(LayoutError::at(format!("{path}.source"), Problem::Invalid)),
            None => return Err(LayoutError::at(format!("{path}.source"), Problem::Missing)),
        };
        let views = kind.views();
        if views.is_empty() {
            // The personal sources have no views: they never reach a page.
            return Err(LayoutError::at(format!("{path}.source"), Problem::NotHere));
        }
        let view = match object.get("view") {
            None if matches!(kind, Kind::Core(_)) => views[0],
            None => return Err(LayoutError::at(format!("{path}.view"), Problem::Missing)),
            Some(Value::String(name)) => views
                .iter()
                .copied()
                .find(|view| view == name)
                .ok_or_else(|| LayoutError::at(format!("{path}.view"), Problem::Unknown))?,
            Some(_) => return Err(LayoutError::at(format!("{path}.view"), Problem::Invalid)),
        };
        if !place.accepts(kind, view) {
            return Err(LayoutError::at(format!("{path}.source"), Problem::NotHere));
        }
        let params = match (kind, object.get("params")) {
            (Kind::Source(source), params) => {
                let map = match params {
                    None => Map::new(),
                    Some(Value::Object(map)) => map.clone(),
                    Some(_) => {
                        return Err(LayoutError::at(format!("{path}.params"), Problem::Invalid));
                    }
                };
                Some(
                    Params::parse(source, &map, Origin::Layout).map_err(|error| {
                        LayoutError::at(format!("{path}.params.{}", error.name), Problem::Invalid)
                    })?,
                )
            }
            (Kind::Core(_), None) => None,
            (Kind::Core(_), Some(_)) => {
                return Err(LayoutError::at(format!("{path}.params"), Problem::Unknown));
            }
        };
        let title = match object.get("title") {
            None if kind.titled() && place.aside() => {
                return Err(LayoutError::at(format!("{path}.title"), Problem::Missing));
            }
            None => None,
            Some(_) if !kind.titled() => {
                return Err(LayoutError::at(format!("{path}.title"), Problem::Unknown));
            }
            Some(Value::Object(title)) => {
                let at = format!("{path}.title");
                match (title.get("key"), title.get("text"), title.len()) {
                    (Some(Value::String(key)), None, 1) => Some(Title::Key(
                        TITLE_KEYS
                            .iter()
                            .copied()
                            .find(|known| known == key)
                            .ok_or_else(|| {
                                LayoutError::at(format!("{at}.key"), Problem::Unknown)
                            })?,
                    )),
                    (None, Some(text), 1) => Some(Title::Text(texts(
                        text,
                        &format!("{at}.text"),
                        TITLE_CHARS,
                        true,
                    )?)),
                    _ => return Err(LayoutError::at(at, Problem::Invalid)),
                }
            }
            Some(_) => return Err(LayoutError::at(format!("{path}.title"), Problem::Invalid)),
        };
        let show = match object.get("show") {
            None => Show::All,
            Some(Value::String(name)) => [Show::All, Show::Guests, Show::Members]
                .into_iter()
                .find(|show| show.name() == name)
                .ok_or_else(|| LayoutError::at(format!("{path}.show"), Problem::Unknown))?,
            Some(_) => return Err(LayoutError::at(format!("{path}.show"), Problem::Invalid)),
        };
        let keep_on_phone = match object.get("keep_on_phone") {
            None => false,
            Some(Value::Bool(keep)) => *keep,
            Some(_) => {
                return Err(LayoutError::at(
                    format!("{path}.keep_on_phone"),
                    Problem::Invalid,
                ));
            }
        };
        let body = match (kind, object.get("body")) {
            (Kind::Core(Core::Markdown), Some(body)) => {
                Some(texts(body, &format!("{path}.body"), MARKDOWN_BYTES, false)?)
            }
            (Kind::Core(Core::Markdown), None) => {
                return Err(LayoutError::at(format!("{path}.body"), Problem::Missing));
            }
            (_, Some(_)) => return Err(LayoutError::at(format!("{path}.body"), Problem::Unknown)),
            (_, None) => None,
        };
        Ok(Self {
            id,
            kind,
            params,
            view,
            title,
            show,
            keep_on_phone,
            body,
        })
    }

    /// The block as a document stores it: only what differs from the defaults is left out (`params` keeps every
    /// key but nulls, so the stored form reads back as the same block).
    pub fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert("id".into(), json!(self.id));
        object.insert("source".into(), json!(self.kind.name()));
        if let Some(params) = &self.params {
            object.insert("params".into(), Value::Object(params.stored()));
        }
        object.insert("view".into(), json!(self.view));
        if let Some(title) = &self.title {
            object.insert(
                "title".into(),
                match title {
                    Title::Key(key) => json!({"key": key}),
                    Title::Text(texts) => json!({"text": texts.json()}),
                },
            );
        }
        if self.show != Show::All {
            object.insert("show".into(), json!(self.show.name()));
        }
        if self.keep_on_phone {
            object.insert("keep_on_phone".into(), json!(true));
        }
        if let Some(body) = &self.body {
            object.insert("body".into(), body.json());
        }
        Value::Object(object)
    }
}

/// The places a document (or skin) sets, in [`Place::ALL`] order; a place it leaves out comes from the next source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places(pub Vec<(Place, Vec<Block>)>);

impl Places {
    /// Reads `places` strictly, for saving and importing: every problem refuses the whole document, naming the field.
    pub fn parse(value: &Value) -> Result<Self, LayoutError> {
        let object = value
            .as_object()
            .ok_or_else(|| LayoutError::at("places", Problem::Invalid))?;
        let mut places = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        for (name, blocks) in object {
            let path = format!("places.{name}");
            let place =
                Place::parse(name).ok_or_else(|| LayoutError::at(&path, Problem::Unknown))?;
            let blocks = blocks
                .as_array()
                .ok_or_else(|| LayoutError::at(&path, Problem::Invalid))?;
            if blocks.len() > place.capacity() {
                return Err(LayoutError::at(path, Problem::TooMany));
            }
            let mut read = Vec::new();
            for (index, block) in blocks.iter().enumerate() {
                let at = format!("{path}[{index}]");
                let block = Block::parse(block, place, &at)?;
                if ids.contains(&block.id) {
                    return Err(LayoutError::at(format!("{at}.id"), Problem::Duplicate));
                }
                ids.push(block.id.clone());
                read.push(block);
            }
            places.push((place, read));
        }
        places.sort_by_key(|(place, _)| *place);
        let places = Self(places);
        for page in Page::ALL {
            let count: usize = page
                .places()
                .iter()
                .filter_map(|place| places.get(*place))
                .map(<[Block]>::len)
                .sum();
            if count > MAX_BLOCKS_PER_PAGE {
                return Err(LayoutError::at("places", Problem::TooMany));
            }
        }
        Ok(places)
    }

    /// Reads `places` leniently, for pages: a place or block that no longer passes the checks (a view removed by an
    /// upgrade, a parameter out of range) is left out and logged; `owner` says whose document it is.
    pub fn read(value: &Value, owner: &str) -> Self {
        let Some(object) = value.as_object() else {
            if !value.is_null() {
                tracing::error!(
                    owner,
                    "a layout's places are not an object; the next layout is used"
                );
            }
            return Self::default();
        };
        let mut places = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        for (name, blocks) in object {
            let Some(place) = Place::parse(name) else {
                tracing::error!(owner, place = %name, "a layout names a place this version does not have; skipped");
                continue;
            };
            let Some(blocks) = blocks.as_array() else {
                tracing::error!(owner, place = %name, "a layout's place is not a list; skipped");
                continue;
            };
            let mut read = Vec::new();
            for (index, block) in blocks.iter().enumerate() {
                let path = format!("places.{name}[{index}]");
                match Block::parse(block, place, &path) {
                    Ok(block) if read.len() < place.capacity() && !ids.contains(&block.id) => {
                        ids.push(block.id.clone());
                        read.push(block);
                    }
                    Ok(block) => {
                        tracing::error!(owner, block = %block.id, "a block past its place's limit or with a repeated id is skipped");
                    }
                    Err(error) => {
                        tracing::error!(owner, path = %error.path, problem = ?error.problem, "a block that no longer passes its checks is skipped");
                    }
                }
            }
            places.push((place, read));
        }
        places.sort_by_key(|(place, _)| *place);
        Self(places)
    }

    pub fn get(&self, place: Place) -> Option<&[Block]> {
        self.0
            .iter()
            .find(|(own, _)| *own == place)
            .map(|(_, blocks)| blocks.as_slice())
    }

    /// Sets one place's blocks, or removes the place (so it inherits again) with `None`.
    pub fn set(&mut self, place: Place, blocks: Option<Vec<Block>>) {
        self.0.retain(|(own, _)| *own != place);
        if let Some(blocks) = blocks {
            self.0.push((place, blocks));
            self.0.sort_by_key(|(place, _)| *place);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The places as a document stores them.
    pub fn json(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(place, blocks)| {
                    (
                        place.name().to_owned(),
                        Value::Array(blocks.iter().map(Block::json).collect()),
                    )
                })
                .collect(),
        )
    }
}

/// Where a page's place came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    Domain,
    Site,
    Skin,
    Core,
}

/// The blocks of `place`: the first of the domain's document, the site's, the skin's manifest and the core default
/// that sets it.
pub fn resolve<'a>(
    place: Place,
    domain: Option<&'a Places>,
    site: Option<&'a Places>,
    skin: Option<&'a Places>,
) -> (&'a [Block], Taken) {
    for (places, origin) in [
        (domain, Taken::Domain),
        (site, Taken::Site),
        (skin, Taken::Skin),
    ] {
        if let Some(blocks) = places.and_then(|places| places.get(place)) {
            return (blocks, origin);
        }
    }
    (core_default().get(place).unwrap_or(&[]), Taken::Core)
}

/// The core default layout: what the templates showed before layouts. The asides of the home page (member card,
/// hot and latest threads, recently viewed, site statistics with the newest members, friend links), of board pages
/// (member card, hot and latest threads of the board) and of thread pages (member card), and the board head of board
/// pages; the other places are empty.
pub fn core_default() -> &'static Places {
    static CORE: LazyLock<Places> = LazyLock::new(|| {
        let core = |id: &str, core: Core, keep: bool| json!({"id": id, "source": core.name(), "keep_on_phone": keep});
        let hot = json!({
            "id": "hot", "source": "threads", "view": "mini-list", "title": {"key": "block-title-hot"},
            "params": {"scope": "current", "sort": "hot", "days": "site", "limit": "site"},
        });
        let latest = json!({
            "id": "latest", "source": "threads", "view": "mini-list", "title": {"key": "block-title-latest"},
            "params": {"scope": "current", "sort": "created", "limit": "site"},
        });
        let document = json!({
            "home.aside": [
                core("member-card", Core::MemberCard, false),
                hot,
                latest,
                core("recent-local", Core::RecentLocal, false),
                core("site-overview", Core::SiteOverview, true),
                core("friend-links", Core::FriendLinks, false),
            ],
            "board.aside": [
                core("member-card-board", Core::MemberCard, false),
                {"id": "hot-board", "source": "threads", "view": "mini-list", "title": {"key": "block-title-hot"},
                 "params": {"scope": "current", "sort": "hot", "days": "site", "limit": "site"}},
                {"id": "latest-board", "source": "threads", "view": "mini-list", "title": {"key": "block-title-latest"},
                 "params": {"scope": "current", "sort": "created", "limit": "site"}},
            ],
            "thread.aside": [core("member-card-thread", Core::MemberCard, false)],
            "page.head.home": [],
            "page.head.board": [core("board-head", Core::BoardHead, false)],
            "page.head.thread": [],
            "footer": [],
            "side.nav": [],
        });
        Places::parse(&document).expect("the core default layout passes its own checks")
    });
    &CORE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places(value: Value) -> Result<Places, LayoutError> {
        Places::parse(&value)
    }

    #[test]
    fn the_core_default_layout_reads_back_the_same() {
        let core = core_default();
        assert_eq!(Places::parse(&core.json()).unwrap(), *core);
        assert_eq!(core.get(Place::HomeAside).unwrap().len(), 6);
        assert_eq!(core.get(Place::Footer), Some(&[][..]));
    }

    #[test]
    fn documents_are_checked_strictly_with_the_field_path() {
        let hot = json!({"id": "hot", "source": "threads", "view": "mini-list", "title": {"key": "block-title-hot"}});
        for (value, path, problem) in [
            (json!({"side.bar": []}), "places.side.bar", Problem::Unknown),
            (
                json!({"home.aside": [{"id": "x", "source": "threads", "view": "mini-list", "title": {"key": "block-title-hot"}, "colour": 1}]}),
                "places.home.aside[0].colour",
                Problem::Unknown,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "weather", "view": "list"}]}),
                "places.home.aside[0].source",
                Problem::Unknown,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "threads", "view": "poster", "title": {"key": "block-title-hot"}}]}),
                "places.home.aside[0].view",
                Problem::Unknown,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "threads", "view": "list", "title": {"key": "block-title-hot"}, "params": {"limit": 99}}]}),
                "places.home.aside[0].params.limit",
                Problem::Invalid,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "threads", "view": "list"}]}),
                "places.home.aside[0].title",
                Problem::Missing,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "member-card", "title": {"key": "block-title-hot"}}]}),
                "places.home.aside[0].title",
                Problem::Unknown,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "me.card", "view": "card"}]}),
                "places.home.aside[0].source",
                Problem::NotHere,
            ),
            (
                json!({"footer": [{"id": "x", "source": "threads", "view": "list"}]}),
                "places.footer[0].source",
                Problem::NotHere,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "thread.outline", "view": "timeline", "title": {"key": "block-title-outline"}}]}),
                "places.home.aside[0].source",
                Problem::NotHere,
            ),
            (
                json!({"page.head.home": [{"id": "x", "source": "site.stats", "view": "figures"}]}),
                "places.page.head.home[0].source",
                Problem::NotHere,
            ),
            (
                json!({"home.aside": [hot.clone(), hot.clone()]}),
                "places.home.aside[1].id",
                Problem::Duplicate,
            ),
            (
                json!({"home.aside": [{"id": "Hot", "source": "member-card"}]}),
                "places.home.aside[0].id",
                Problem::Invalid,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "markdown", "title": {"text": {"en": "Notice"}}}]}),
                "places.home.aside[0].body",
                Problem::Missing,
            ),
            (
                json!({"footer": [{"id": "x", "source": "markdown", "body": {"zh-CN": "text"}}]}),
                "places.footer[0].body.en",
                Problem::Missing,
            ),
            (
                json!({"footer": [{"id": "x", "source": "markdown", "body": {"en": "a".repeat(4097)}}]}),
                "places.footer[0].body.en",
                Problem::TooLong,
            ),
            (
                json!({"home.aside": [{"id": "x", "source": "markdown", "body": {"en": "text"}, "title": {"text": {"en": "x".repeat(41)}}}]}),
                "places.home.aside[0].title.text.en",
                Problem::TooLong,
            ),
            (
                json!({"page.head.home": [{"id": "a", "source": "markdown", "body": {"en": "1"}}, {"id": "b", "source": "markdown", "body": {"en": "2"}}, {"id": "c", "source": "markdown", "body": {"en": "3"}}]}),
                "places.page.head.home",
                Problem::TooMany,
            ),
        ] {
            assert_eq!(
                places(value.clone()),
                Err(LayoutError {
                    path: path.into(),
                    problem
                }),
                "{value}"
            );
        }
        // Eight in the aside and three in the footer make eleven blocks on the home page.
        let many: Vec<Value> = (0..8)
            .map(|index| json!({"id": format!("m{index}"), "source": "member-card"}))
            .collect();
        let footer: Vec<Value> = (0..3)
            .map(|index| json!({"id": format!("f{index}"), "source": "markdown", "body": {"en": "x"}}))
            .collect();
        assert_eq!(
            places(json!({"home.aside": many, "footer": footer}))
                .unwrap_err()
                .problem,
            Problem::TooMany
        );
    }

    #[test]
    fn stored_blocks_read_back_the_same() {
        let value = json!({
            "home.aside": [
                {"id": "hot-week", "source": "threads", "params": {"sort": "hot", "days": 7}, "view": "list",
                 "title": {"text": {"en": "This week", "zh-CN": "\u{672c}\u{5468}"}}, "show": "members", "keep_on_phone": true},
                {"id": "notice", "source": "markdown", "title": {"key": "block-title-notice"}, "body": {"en": "**Hello**"}},
            ],
            "footer": [{"id": "links", "source": "site.links", "params": {"place": "directory"}, "view": "links"}],
        });
        let parsed = places(value).unwrap();
        assert_eq!(Places::parse(&parsed.json()).unwrap(), parsed);
        let hot = &parsed.get(Place::HomeAside).unwrap()[0];
        assert_eq!(hot.show, Show::Members);
        // The stored parameters are normalized.
        assert_eq!(hot.json()["params"]["scope"], "current");
        assert_eq!(hot.json()["params"]["limit"], 10);
    }

    #[test]
    fn pages_skip_what_no_longer_passes() {
        let value = json!({
            "home.aside": [
                {"id": "hot", "source": "threads", "view": "carousel", "title": {"key": "block-title-hot"}},
                {"id": "me", "source": "member-card"},
            ],
            "nowhere": [],
        });
        let read = Places::read(&value, "test");
        assert_eq!(read.get(Place::HomeAside).unwrap().len(), 1);
        assert_eq!(read.get(Place::HomeAside).unwrap()[0].id, "me");
        assert!(Places::read(&json!(null), "test").is_empty());
    }

    #[test]
    fn places_come_from_the_domain_then_the_site_then_the_skin_then_the_core() {
        let one = |id: &str| {
            places(json!({"footer": [{"id": id, "source": "markdown", "body": {"en": "x"}}]}))
                .unwrap()
        };
        let (domain, site, skin) = (one("domain"), one("site"), one("skin"));
        assert_eq!(
            resolve(Place::Footer, Some(&domain), Some(&site), Some(&skin)).1,
            Taken::Domain
        );
        assert_eq!(
            resolve(Place::Footer, None, Some(&site), Some(&skin)).1,
            Taken::Site
        );
        assert_eq!(
            resolve(Place::Footer, None, None, Some(&skin)).1,
            Taken::Skin
        );
        let (blocks, origin) = resolve(Place::HomeAside, Some(&domain), Some(&site), Some(&skin));
        assert_eq!(origin, Taken::Core);
        assert_eq!(blocks.len(), 6);
    }
}
