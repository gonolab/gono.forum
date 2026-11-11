//! When the header forum navigation does not fit, items are folded into a trailing "More" menu.
//!
//! With many forums (a long navigation document), items that do not fit at 901–1200px would otherwise be clipped
//! silently by the single-row height and `overflow: hidden` of `.masthead__board-list`. Which items stay in the
//! header and which go into "More" is decided by the server with fixed rules, not by a script measuring widths:
//!
//! - Each item's width is estimated from its text by character class, never by language (docs/architecture.md,
//!   "Languages"): full-width characters (Chinese etc.) at 14px (exactly 1em), supplementary-plane characters
//!   (emoji etc.) at 20px, and ASCII by class from the widths measured in the browser at 14px on 2026-11-09
//!   (system-ui, rounded up a little): narrow letters and punctuation 5.3px, a space 4px, lowercase 8.2px,
//!   uppercase 10px, digits and other symbols 8.7px, the wide `m w M W @ %` 12.5px; plus 18px of left and right
//!   padding, and no less than `min-width: 3.9em` (54.6px). Items are 2px apart, matching `frame.css`.
//! - Each of the four desktop widths has a budget: the space the header navigation gets at that width, measured
//!   on the tightest visitor header (the two buttons on the right and the brand text of [`Header::MEASURED`])
//!   minus another 4px (measured on 2026-11-11). The header navigation is shown only from 901px up, and from
//!   1200px the container no longer grows. A page whose brand text or buttons read differently (another language,
//!   the brand text setting, another domain's name) moves the budget by the difference of the estimated widths
//!   against that measured header, so the measured header still gets exactly these budgets. A signed-in member's
//!   header is narrower (a 30px avatar), so the visitor's stays the tightest in every language.
//! - If a tier fits all items, no "More" is needed; if it does not, the "More" button takes 58px (measured with
//!   the label of [`Header::MEASURED`]; another label moves it by the estimated text difference) and the header
//!   holds only the leading items that fit. Each item records "from which tier on it stays in the header"
//!   (`fit`); an item that fits in no tier gets 0 and appears only in "More". CSS shows or hides the two copies
//!   at the same four breakpoints (`frame.css`).
//!
//! - The trailing "All forums" (`/forums`, shown only when it fits) comes after all navigation items with the
//!   lowest priority and never pushes navigation items out: it stays in the header in a tier only when all
//!   navigation items stay in the header and it still fits after them; in a tier that already has "More" it goes to
//!   the end of "More"; in a tier where the navigation items exactly fill the space and there is no "More", it is
//!   not shown (eleven two-character forums at 1200px). In the mobile drawer it is always one item. See
//!   [`fit_tail`].
//!
//! Known limitation: when a custom logo is wider than the default brand text, the budget is too large; the
//! overflow is still caught by the original `overflow: hidden`.
use super::NavigationLink;

/// (Viewport lower bound, header navigation budget in px.) Items in the first tier stay in the header at
/// every desktop width. Measured on [`Header::MEASURED`].
pub(super) const TIERS: [(u16, f32); 4] =
    [(901, 425.0), (1000, 524.0), (1100, 624.0), (1200, 724.0)];
const GAP: f32 = 2.0;
/// The "More" toggle of [`Header::MEASURED`]: its text, a 12px chevron and 18px of padding.
const MORE: f32 = 58.0;
const PADDING: f32 = 18.0;
const MIN_WIDTH: f32 = 54.6;
/// `.masthead__action`: 10px of padding and a 1px border on each side.
const BUTTON_PADDING: f32 = 22.0;
/// The brand text is 16px and semibold against the 14px navigation text: estimated widths are scaled by
/// 16 / 14 and another 5% for the weight.
const BRAND_SCALE: f32 = 16.0 / 14.0 * 1.05;

/// The texts on the header that compete with the navigation for room: the brand text, the visitor's two buttons
/// and the label of "More".
#[derive(Clone, Copy, Debug)]
pub(super) struct Header<'a> {
    pub brand: &'a str,
    pub log_in: &'a str,
    pub register: &'a str,
    pub more: &'a str,
}

impl Header<'_> {
    /// The header the budgets in [`TIERS`] and [`MORE`] were measured on: a Simplified Chinese header whose brand
    /// text and labels are four, two, two and two full-width characters ("sample forum", "log in", "register",
    /// "more").
    pub(super) const MEASURED: Header<'static> = Header {
        brand: "\u{793a}\u{4f8b}\u{8bba}\u{575b}",
        log_in: "\u{767b}\u{5f55}",
        register: "\u{6ce8}\u{518c}",
        more: "\u{66f4}\u{591a}",
    };

    /// How much more room this header's brand text and buttons take than the measured one (negative when
    /// less).
    fn extra(&self) -> f32 {
        let taken = |header: &Header| {
            text(header.brand) * BRAND_SCALE
                + text(header.log_in)
                + text(header.register)
                + 2.0 * BUTTON_PADDING
        };
        taken(self) - taken(&Self::MEASURED)
    }

    /// The toggle has no minimum width that matters (the chevron makes it wider), so it moves by the text alone.
    fn more(&self) -> f32 {
        MORE + text(self.more) - text(Self::MEASURED.more)
    }
}

/// The estimated text width at 14px, by character class (see the module documentation).
fn text(label: &str) -> f32 {
    label
        .chars()
        .map(|c| match c {
            ' ' => 4.0,
            'm' | 'w' | 'M' | 'W' | '@' | '%' => 12.5,
            'i' | 'j' | 'l' | 'r' | 't' | 'f' | 'I' | '!' | '\'' | '(' | ')' | ',' | '.' | '/'
            | ':' | ';' | '[' | ']' | '\\' | '{' | '|' | '}' => 5.3,
            'a'..='z' => 8.2,
            'A'..='Z' => 10.0,
            c if c.is_ascii() => 8.7,
            c if u32::from(c) > 0xFFFF => 20.0,
            _ => 14.0,
        })
        .sum()
}

fn width(label: &str) -> f32 {
    (text(label) + PADDING).max(MIN_WIDTH)
}

/// The budgets of this header: the measured ones moved by [`Header::extra`].
fn tiers(header: &Header) -> impl Iterator<Item = (u16, f32)> {
    let extra = header.extra();
    TIERS
        .into_iter()
        .map(move |(tier, budget)| (tier, budget - extra))
}

/// Fills in `fit` for each item by the rules above: the lowest viewport tier at which it stays in the header;
/// 0 means it only goes into "More".
pub(super) fn fit(links: &mut [NavigationLink], header: &Header) {
    let widths: Vec<f32> = links.iter().map(|link| width(&link.label)).collect();
    let total =
        |count: usize| widths[..count].iter().sum::<f32>() + GAP * count.saturating_sub(1) as f32;
    let count = links.len();
    for link in links.iter_mut() {
        link.fit = 0;
    }
    let more = header.more();
    for (tier, budget) in tiers(header) {
        let inline = if total(count) <= budget {
            count
        } else {
            (0..count)
                .rev()
                .find(|&shown| total(shown) + GAP + more <= budget)
                .unwrap_or(0)
        };
        for link in links[..inline].iter_mut() {
            if link.fit == 0 {
                link.fit = tier;
            }
        }
    }
}

/// The trailing low-priority item ("All forums"): it stays in the header only in a tier where all navigation
/// items stay in the header and it still fits after them; records the lowest such tier, or 0 if none.
/// It does not take part in [`more_until`]: whether "More" appears depends only on the navigation items, so
/// in tiers without "More" it is not shown when it does not fit, and in tiers with "More" it is at the end
/// of "More".
pub(super) fn fit_tail(links: &[NavigationLink], tail: &mut NavigationLink, header: &Header) {
    let total = links.iter().map(|link| width(&link.label)).sum::<f32>()
        + GAP * links.len().saturating_sub(1) as f32;
    let with_tail = if links.is_empty() {
        width(&tail.label)
    } else {
        total + GAP + width(&tail.label)
    };
    tail.fit = tiers(header)
        .find(|(_, budget)| with_tail <= *budget)
        .map_or(0, |(tier, _)| tier);
}

impl NavigationLink {
    /// Stays in the header at every desktop width and never goes into "More".
    pub(super) fn always_inline(&self) -> bool {
        self.fit == TIERS[0].0
    }
}

/// The width up to which "More" appears: from this tier on everything fits and "More" is hidden; 0 means it
/// is needed at every width. Only meaningful when some item is not `always_inline`.
pub(super) fn more_until(links: &[NavigationLink]) -> u16 {
    if links.iter().any(|link| link.fit == 0) {
        0
    } else {
        links.iter().map(|link| link.fit).max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Chinese forum names, written as escapes: these tests are about full-width text, estimated at 14px a
    // character. Two characters: "daily", "cool stuff", "hosting", "releases", "site affairs", "community"; three:
    // "large models"; four: "open-source projects", "domain trading"; five: "prompt engineering".
    const DAILY: &str = "\u{65e5}\u{5e38}";
    const COOL: &str = "\u{9177}\u{54c1}";
    const HOSTING: &str = "\u{4e3b}\u{673a}";
    const RELEASES: &str = "\u{7248}\u{672c}";
    const SITE: &str = "\u{7ad9}\u{52a1}";
    const COMMUNITY: &str = "\u{793e}\u{533a}";
    const MODELS: &str = "\u{5927}\u{6a21}\u{578b}";
    const OPEN_SOURCE: &str = "\u{5f00}\u{6e90}\u{9879}\u{76ee}";
    const DOMAINS: &str = "\u{57df}\u{540d}\u{4ea4}\u{6613}";
    const PROMPTS: &str = "\u{63d0}\u{793a}\u{8bcd}\u{5de5}\u{7a0b}";
    /// "All forums" in Chinese: four full-width characters (74px).
    const ALL_FORUMS: &str = "\u{5168}\u{90e8}\u{7248}\u{5757}";

    fn links(labels: &[&str]) -> Vec<NavigationLink> {
        let mut links: Vec<NavigationLink> = labels
            .iter()
            .map(|label| NavigationLink {
                label: (*label).to_owned(),
                href: format!("/{label}"),
                active: false,
                fit: 0,
            })
            .collect();
        fit(&mut links, &Header::MEASURED);
        links
    }

    #[test]
    fn six_default_forums_always_stay_inline() {
        let links = links(&[DAILY, COOL, "AI", HOSTING, RELEASES, SITE]);
        assert!(links.iter().all(NavigationLink::always_inline));
    }

    /// Ten forums: at 901px only 5 of them fit in the visitor header and the rest go into "More", which disappears
    /// once all fit.
    #[test]
    fn ten_forums_split_by_tier_and_more_disappears_once_all_fit() {
        let links = links(&[
            DAILY,
            MODELS,
            PROMPTS,
            OPEN_SOURCE,
            DOMAINS,
            COOL,
            "AI",
            HOSTING,
            RELEASES,
            SITE,
        ]);
        let fits: Vec<u16> = links.iter().map(|link| link.fit).collect();
        // Cumulative widths 54.6, 116.6, 206.6, 282.6, 358.6, 415.2, 471.8, 528.4, 585, 641.6.
        // 901 (425): adding the 2px gap and the 58px "More", only the first 5 items fit; 1000 (524) fits 6;
        // 1100 (624) fits 8; 1200 (724) fits all, and "More" disappears.
        assert_eq!(
            fits,
            [901, 901, 901, 901, 901, 1000, 1100, 1100, 1200, 1200]
        );
        assert_eq!(more_until(&links), 1200);
    }

    #[test]
    fn labels_that_never_fit_live_only_in_more() {
        // "A very, very long forum name" in Chinese (nine full-width characters), six times over.
        let long =
            "\u{5f88}\u{957f}\u{5f88}\u{957f}\u{7684}\u{7248}\u{5757}\u{540d}\u{79f0}".repeat(6);
        let links = links(&[DAILY, &long, SITE]);
        assert_eq!(links[0].fit, 901);
        assert_eq!(links[1].fit, 0);
        // Order is kept: items after one that does not fit also go into "More"; the header never skips items.
        assert_eq!(links[2].fit, 0);
        assert_eq!(more_until(&links), 0);
    }

    fn tail() -> NavigationLink {
        NavigationLink {
            label: ALL_FORUMS.into(),
            href: "/forums".into(),
            active: false,
            fit: 0,
        }
    }

    /// Six two-character forums leave room in the 901 tier (337.6 + 2 + 74 <= 425), so "All forums" stays in the
    /// header at every width; with eleven forums it fits in no tier, and the forums still never go into "More" for it.
    #[test]
    fn directory_link_never_pushes_forums_into_more() {
        let links = links(&[DAILY, COOL, "AI", HOSTING, RELEASES, SITE]);
        let mut directory = tail();
        fit_tail(&links, &mut directory, &Header::MEASURED);
        // All navigation items always stay in the header, so "More" never appears (`nav_more` looks only at
        // navigation items).
        assert!(links.iter().all(NavigationLink::always_inline));
        assert_eq!(directory.fit, 901);
        // Eleven forums: the navigation items fit completely only at the 1200 tier (698.2); adding 2 + 74 exceeds
        // 724, so it stays in the header in no tier and appears only in "More".
        let crowded = super::tests::links(&[
            DAILY,
            MODELS,
            PROMPTS,
            OPEN_SOURCE,
            DOMAINS,
            COOL,
            "AI",
            HOSTING,
            RELEASES,
            SITE,
            COMMUNITY,
        ]);
        let mut directory = tail();
        fit_tail(&crowded, &mut directory, &Header::MEASURED);
        assert_eq!(directory.fit, 0);
        // Empty navigation document: only the item itself.
        let mut alone = tail();
        fit_tail(&[], &mut alone, &Header::MEASURED);
        assert_eq!(alone.fit, 901);
    }

    #[test]
    fn estimate_is_never_narrower_than_the_rendered_link() {
        // Measured: two full-width characters 54.6 (the minimum width), three 60, four 74, five 88; ASCII
        // estimated generously at 9px.
        assert_eq!(width(DAILY), MIN_WIDTH);
        assert_eq!(width(MODELS), 60.0);
        assert_eq!(width(DOMAINS), 74.0);
        assert_eq!(width(PROMPTS), 88.0);
        // English, measured in the browser at 14px on 2026-11-09: General 68.16, Showcase 83.41, Releases 75.66,
        // All forums 82.99, Community Feedback 158.74.
        for (label, measured) in [
            ("General", 68.16),
            ("Showcase", 83.41),
            ("Releases", 75.66),
            ("All forums", 82.99),
            ("Community Feedback", 158.74),
        ] {
            let estimated = width(label);
            assert!(
                (measured..measured * 1.12).contains(&estimated),
                "{label}: estimated {estimated}, measured {measured}"
            );
        }
    }

    /// The measured header gets exactly the measured budgets; an English header (brand "Gono Forum", "Log in /
    /// Sign up", "More") gets less room by the estimated difference (measured values in the test).
    #[test]
    fn budgets_move_by_the_difference_against_the_measured_header() {
        assert_eq!(Header::MEASURED.extra(), 0.0);
        assert_eq!(Header::MEASURED.more(), MORE);
        let english = Header {
            brand: "Gono Forum",
            log_in: "Log in",
            register: "Sign up",
            more: "More",
        };
        let extra = english.extra();
        // Measured on 2026-11-11: brand 116.78 against 88, buttons 61.8 + 71.69 against 50 + 50.
        let measured = (116.78 - 88.0) + (61.8 + 71.69 - 100.0);
        assert!(
            (measured..measured + 20.0).contains(&extra),
            "estimated {extra}, measured {measured}"
        );
        // The "More" toggle measured 63.03 against 58 for the two full-width characters of the measured header.
        assert!(english.more() >= 63.03, "{}", english.more());
        // Fewer English forums stay in the header at 901 than the same count of two-character Chinese ones.
        let mut links: Vec<NavigationLink> =
            ["General", "Showcase", "AI", "Hosting", "Releases", "Site"]
                .iter()
                .map(|label| NavigationLink {
                    label: (*label).to_owned(),
                    href: format!("/{label}"),
                    active: false,
                    fit: 0,
                })
                .collect();
        fit(&mut links, &english);
        assert!(links.iter().any(|link| link.fit > 901));
        assert_eq!(links[0].fit, 901);
    }
}
