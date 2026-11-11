//! In-process image bytes: stores the final response bytes by `(slot, attachment id, size)` (registry
//! [`super::registry::BRAND_IMAGES`], [`super::registry::DISPLAY_IMAGES`]).
//!
//! Only rebuildable data is stored, one copy per process. Callers first look up the current binding in the database
//! and fetch by the attachment id they get, so after an image is replaced, reset to the default or unbound, old
//! entries are no longer hit and get pushed out first-in first-out; both the total and each entry have a limit, and
//! entries over the per-entry limit are not cached.
pub struct ImageCache {
    inner: std::sync::Mutex<Inner>,
    max_entries: usize,
    max_bytes: usize,
    max_entry_bytes: usize,
}

/// `(slot, attachment id, size)`.
type Key = (String, i64, String);

/// Indexed by key (2026-11-08: previously a linear scan of up to 2048 entries under the lock, and a hit copied all
/// the bytes); `order` records insertion order, entries are pushed out first-in first-out.
#[derive(Default)]
struct Inner {
    entries: std::collections::HashMap<Key, (bytes::Bytes, String)>,
    order: std::collections::VecDeque<Key>,
    total: usize,
}

impl std::fmt::Debug for ImageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("ImageCache")
            .field("entries", &inner.entries.len())
            .finish()
    }
}

impl ImageCache {
    pub fn with_limits(max_entries: usize, max_bytes: usize, max_entry_bytes: usize) -> Self {
        Self {
            inner: std::sync::Mutex::default(),
            max_entries,
            max_bytes,
            max_entry_bytes,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Cache contents can be rebuilt: data left by a previous lock holder that panicked can be used as is.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// On a hit the bytes are shared (only the reference count goes up), not copied.
    pub fn get(&self, slot: &str, aid: i64, variant: &str) -> Option<(bytes::Bytes, String)> {
        self.lock()
            .entries
            .get(&(slot.to_owned(), aid, variant.to_owned()))
            .cloned()
    }

    pub fn insert(&self, slot: &str, aid: i64, variant: &str, bytes: &bytes::Bytes, mime: &str) {
        if bytes.len() > self.max_entry_bytes {
            return;
        }
        let key: Key = (slot.to_owned(), aid, variant.to_owned());
        let mut inner = self.lock();
        if let Some((old, _)) = inner.entries.remove(&key) {
            inner.total -= old.len();
            inner.order.retain(|existing| existing != &key);
        }
        inner.total += bytes.len();
        inner.order.push_back(key.clone());
        inner.entries.insert(key, (bytes.clone(), mime.to_owned()));
        while inner.entries.len() > self.max_entries || inner.total > self.max_bytes {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if let Some((evicted, _)) = inner.entries.remove(&oldest) {
                inner.total -= evicted.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRIES: usize = 32;
    const BYTES: usize = 8 << 20;
    const ENTRY_BYTES: usize = 2 << 20;

    #[test]
    fn keyed_by_binding_and_bounded() {
        let cache = ImageCache::with_limits(ENTRIES, BYTES, ENTRY_BYTES);
        let bytes = |value: &[u8]| bytes::Bytes::copy_from_slice(value);
        cache.insert("favicon", 7, "32", &bytes(b"a"), "image/png");
        assert_eq!(
            cache.get("favicon", 7, "32"),
            Some((bytes(b"a"), "image/png".into()))
        );
        assert_eq!(cache.get("favicon", 8, "32"), None);
        assert_eq!(cache.get("app_icon", 7, "32"), None);
        cache.insert("favicon", 7, "32", &bytes(b"b"), "image/png");
        assert_eq!(cache.get("favicon", 7, "32").unwrap().0, &b"b"[..]);
        cache.insert(
            "logo",
            9,
            "source",
            &bytes(&vec![0; ENTRY_BYTES + 1]),
            "image/png",
        );
        assert_eq!(
            cache.get("logo", 9, "source"),
            None,
            "an entry over the per-entry limit is not cached"
        );
        for aid in 0..(BYTES / ENTRY_BYTES + 1) as i64 {
            cache.insert(
                "share",
                100 + aid,
                "source",
                &bytes(&vec![0; ENTRY_BYTES]),
                "image/png",
            );
        }
        let inner = cache.lock();
        assert!(inner.entries.values().map(|(b, _)| b.len()).sum::<usize>() <= BYTES);
        assert_eq!(
            inner.total,
            inner.entries.values().map(|(b, _)| b.len()).sum::<usize>()
        );
        assert!(
            !inner.entries.keys().any(|k| k.1 == 7),
            "the oldest entries are evicted first"
        );
        drop(inner);
        for aid in 0..40 {
            cache.insert("favicon", 1000 + aid, "32", &bytes(b"x"), "image/png");
        }
        assert_eq!(cache.lock().entries.len(), ENTRIES);
        assert_eq!(cache.lock().order.len(), ENTRIES);
    }
}
