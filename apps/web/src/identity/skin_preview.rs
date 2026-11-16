//! The skin preview of an administrator (docs/skins.md, "Preview"): which skin and palette to show instead of the
//! site's, sealed into a cookie with the cookie key, so it lives only in that browser and needs no database row.
//! The page frame applies it only while the signed-in member is the administrator who started it.
use serde::{Deserialize, Serialize};

use super::{Identity, IdentityError, Result, now};

/// How long a preview lasts without being renewed: one working session.
pub const PREVIEW_TTL: i64 = 8 * 3600;
const AAD: &[u8] = b"gonof.skin-preview.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkinPreview {
    pub uid: i64,
    pub skin: String,
    pub palette: String,
    pub expires_at: i64,
}

impl Identity {
    pub fn seal_skin_preview(&self, uid: i64, skin: &str, palette: &str) -> Result<String> {
        self.config
            .cookie
            .seal_value(
                AAD,
                &SkinPreview {
                    uid,
                    skin: skin.to_owned(),
                    palette: palette.to_owned(),
                    expires_at: now() + PREVIEW_TTL,
                },
            )
            .map_err(IdentityError::Internal)
    }

    /// The preview `uid` started; `None` when the cookie is empty, expired, undecryptable or someone else's.
    pub fn open_skin_preview(&self, raw: &str, uid: i64) -> Option<SkinPreview> {
        let timestamp = now();
        self.config
            .cookie
            .open_value::<SkinPreview>(AAD, raw)
            .filter(|preview| {
                preview.uid == uid
                    && preview.expires_at > timestamp
                    && preview.expires_at <= timestamp + PREVIEW_TTL
            })
    }
}
