//! The skin preview of an administrator (docs/skins.md, "Preview"): which skin, palette and colors to show instead of
//! the site's, sealed into a cookie with the cookie key, so it lives only in that browser and needs no database row.
//! The page frame applies it only while the signed-in member is the administrator who started it.
use serde::{Deserialize, Serialize};

use super::{Identity, IdentityError, Result, now};
use crate::skins::selection::Colors;

/// How long a preview lasts without being renewed: one working session.
pub const PREVIEW_TTL: i64 = 8 * 3600;
const AAD: &[u8] = b"gonof.skin-preview.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkinPreview {
    pub uid: i64,
    pub skin: String,
    pub palette: String,
    /// Unsaved seed colors (docs/styles.md, "Seed colors"), or the skin's own colors; left out for a preview of a
    /// skin, which shows the colors the page would have anyway.
    #[serde(default, skip_serializing_if = "Colors::inherits")]
    pub colors: Colors,
    /// A preview of the layout draft of the site or of a domain (docs/skins.md, "Editing a layout"), shown over the
    /// skin and colors the page has anyway; left out for a preview of a skin or colors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutDraft>,
    pub expires_at: i64,
}

/// Whose layout draft a preview shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutDraft {
    /// `None` for the site's draft.
    pub domain: Option<i64>,
}

impl Identity {
    pub fn seal_skin_preview(
        &self,
        uid: i64,
        skin: &str,
        palette: &str,
        colors: Colors,
    ) -> Result<String> {
        self.config
            .cookie
            .seal_value(
                AAD,
                &SkinPreview {
                    uid,
                    skin: skin.to_owned(),
                    palette: palette.to_owned(),
                    colors,
                    layout: None,
                    expires_at: now() + PREVIEW_TTL,
                },
            )
            .map_err(IdentityError::Internal)
    }

    /// A preview of a layout draft: the page keeps `skin` and `palette`, the ones it has anyway, and shows the draft's
    /// places instead of the published ones.
    pub fn seal_layout_preview(
        &self,
        uid: i64,
        (skin, palette): (&str, &str),
        domain: Option<i64>,
    ) -> Result<String> {
        self.config
            .cookie
            .seal_value(
                AAD,
                &SkinPreview {
                    uid,
                    skin: skin.to_owned(),
                    palette: palette.to_owned(),
                    colors: Colors::Inherit,
                    layout: Some(LayoutDraft { domain }),
                    expires_at: now() + PREVIEW_TTL,
                },
            )
            .map_err(IdentityError::Internal)
    }

    /// The preview `uid` started; `None` when the cookie is empty, expired, undecryptable or someone else's.
    pub fn open_skin_preview(&self, raw: &str, uid: i64) -> Option<SkinPreview> {
        self.sealed_skin_preview(raw)
            .filter(|preview| preview.uid == uid)
    }

    /// The preview in a cookie that this site sealed and that has not expired, whoever started it. Opening it needs no
    /// database; callers that act on it still check that the signed-in member is `preview.uid`.
    pub fn sealed_skin_preview(&self, raw: &str) -> Option<SkinPreview> {
        let timestamp = now();
        self.config
            .cookie
            .open_value::<SkinPreview>(AAD, raw)
            .filter(|preview| {
                preview.expires_at > timestamp && preview.expires_at <= timestamp + PREVIEW_TTL
            })
    }
}
