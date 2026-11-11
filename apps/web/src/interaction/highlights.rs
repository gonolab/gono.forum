use crate::db::Database;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub pid: i64,
    pub count: i64,
}

/// Rules for highlighted replies (headed "Top replies"): the minimum number of likes and the maximum number shown
/// (`settings.display.highlight_min_likes`, `highlight_count`; 0 means none shown).
/// Computed per request, not in the floor cache.
#[derive(Clone, Copy, Debug)]
pub struct HighlightRules {
    pub min_likes: i64,
    pub count: usize,
}

impl HighlightRules {
    pub fn from_settings(settings: &crate::runtime::SiteSettings) -> Self {
        Self {
            min_likes: settings.display.highlight_min_likes,
            count: usize::try_from(settings.display.highlight_count).unwrap_or(0),
        }
    }
}

/// Rank only the actual displayed page; visibility has already been applied to these posts.
pub async fn highlights(
    db: &Database,
    posts: &[crate::content::Post],
    rules: HighlightRules,
) -> anyhow::Result<Vec<Highlight>> {
    let ids: Vec<_> = posts
        .iter()
        .filter(|p| !p.hidden && !p.withdrawn && !p.erased && !p.blocked)
        .map(|p| p.pid as i64)
        .collect();
    let reactions = super::reactions(db, None, &ids).await?;
    Ok(rank_highlights(posts, &reactions, rules))
}

/// Reuse the same reaction snapshot as the page's individual reaction controls.
pub fn rank_highlights(
    posts: &[crate::content::Post],
    reactions: &HashMap<i64, super::ReactionState>,
    rules: HighlightRules,
) -> Vec<Highlight> {
    let mut ranked: Vec<_> = posts
        .iter()
        .filter(|p| !p.hidden && !p.withdrawn && !p.erased && !p.blocked)
        .filter_map(|p| {
            reactions
                .get(&(p.pid as i64))
                .filter(|r| r.count > 0 && r.count >= rules.min_likes)
                .map(|r| {
                    (
                        p.position,
                        Highlight {
                            pid: p.pid as i64,
                            count: r.count,
                        },
                    )
                })
        })
        .collect();
    ranked.sort_by_key(|(position, h)| (std::cmp::Reverse(h.count), *position, h.pid));
    ranked
        .into_iter()
        .take(rules.count)
        .map(|(_, h)| h)
        .collect()
}
