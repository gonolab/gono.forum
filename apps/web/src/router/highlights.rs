//! A separate summary keeps ranked replies out of the permanent floor order.
use super::*;

pub(super) struct Row {
    pub pid: i64,
    pub count: i64,
    pub position: usize,
    pub author: String,
    pub time: String,
    /// The post time as a timestamp, for the `datetime` of the shown time.
    pub dateline: u64,
    pub avatar: AvatarView,
    pub href: String,
    pub summary: String,
}

pub(super) struct Section {
    pub rows: Vec<Row>,
    pub oob: bool,
}

pub(super) async fn load(
    state: &AppState,
    thread: &Thread,
    flat: bool,
    oob: bool,
) -> anyhow::Result<Section> {
    let rules = crate::interaction::HighlightRules::from_settings(&state.frame.settings);
    let ranked = crate::interaction::highlights(&state.db, &thread.posts, rules).await?;
    render_ranked(state, thread, flat, oob, ranked).await
}

pub(super) async fn load_with_reactions(
    state: &AppState,
    thread: &Thread,
    flat: bool,
    reactions: &std::collections::HashMap<i64, crate::interaction::ReactionState>,
) -> anyhow::Result<Section> {
    let rules = crate::interaction::HighlightRules::from_settings(&state.frame.settings);
    let ranked = crate::interaction::rank_highlights(&thread.posts, reactions, rules);
    render_ranked(state, thread, flat, false, ranked).await
}

async fn render_ranked(
    state: &AppState,
    thread: &Thread,
    flat: bool,
    oob: bool,
    ranked: Vec<crate::interaction::Highlight>,
) -> anyhow::Result<Section> {
    let posts: Vec<_> = ranked
        .into_iter()
        .filter_map(|rank| {
            thread
                .posts
                .iter()
                .find(|post| {
                    post.pid as i64 == rank.pid
                        && !post.hidden
                        && !post.withdrawn
                        && !post.erased
                        && !post.blocked
                })
                .map(|post| (post, rank.count))
        })
        .collect();
    let sources: Vec<_> = posts.iter().map(|(post, _)| post.body.as_str()).collect();
    let names = crate::markdown::member_names(&state.db, &sources).await?;
    let rows = posts
        .into_iter()
        .filter_map(|(post, count)| {
            let href = urls::post_location(
                thread,
                post.pid as i64,
                flat,
                urls::Paging::from_settings(&state.frame.settings),
            )?;
            let summary = crate::markdown::summary(&post.body, &names);
            Some(Row {
                pid: post.pid as i64,
                count,
                position: post.position,
                author: post.author.clone(),
                time: post.time.clone(),
                dateline: post.dateline,
                // Highlighted replies come from the floor data; like floors, the avatar is served by uid (2026-11-08).
                avatar: list_avatar(post.authorid.filter(|_| !post.author_deleted), &post.author),
                href,
                summary: if summary.is_empty() {
                    crate::i18n::current().topic_highlight_fallback().into()
                } else {
                    summary
                },
            })
        })
        .collect();
    Ok(Section { rows, oob })
}
