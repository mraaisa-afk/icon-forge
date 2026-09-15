//! Content-addressed payload cache (ARCHITECTURE.md §3.3 stage 8).
//!
//! Key: `blake3(bytes) ‖ preset ‖ segParams ‖ version`. Payloads are
//! zstd-compressed on disk under a two-character shard directory; the
//! `cache` table maps key → payload path (the DB is the index, the
//! filesystem is the store — either can be rebuilt from the other).

use std::fs;
use std::path::{Path, PathBuf};

use blake3::Hasher;

use crate::db::Library;
use crate::IsgError;

/// Filesystem-backed cache store with a SQLite index.
pub struct CacheStore {
    root: PathBuf,
}

impl CacheStore {
    /// Store rooted at `root` (created lazily on first write).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Store root path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Computes the deterministic cache key for a payload
    /// (`blake3(bytes) ‖ preset ‖ blake3(segParams) ‖ version`).
    #[must_use]
    pub fn cache_key(bytes: &[u8], preset: &str, seg_params: &str, version: u32) -> String {
        let mut h = Hasher::new();
        h.update(seg_params.as_bytes());
        let seg = h.finalize();
        format!(
            "{}-{preset}-{}-v{version}",
            blake3::hash(bytes).to_hex(),
            &seg.to_hex()[..16],
        )
    }

    /// Compresses and stores `payload` under `key`; idempotent (a repeated
    /// put overwrites both file and row).
    pub fn put(&self, lib: &Library, key: &str, payload: &[u8]) -> crate::Result<PathBuf> {
        if key.len() < 2 {
            return Err(IsgError::Corrupt(format!("cache key too short: {key:?}")));
        }
        let shard = self.root.join(&key[..2]);
        fs::create_dir_all(&shard)?;
        let path = shard.join(format!("{key}.zst"));
        let compressed = zstd::stream::encode_all(payload, 3)?;
        // Write-then-rename so a crash never leaves a truncated payload
        // claiming a valid path.
        let tmp = shard.join(format!("{key}.zst.tmp"));
        fs::write(&tmp, &compressed)?;
        fs::rename(&tmp, &path)?;
        lib.cache_put(
            key,
            path.to_str()
                .ok_or_else(|| IsgError::Corrupt("cache path is not valid UTF-8".to_string()))?,
        )?;
        Ok(path)
    }

    /// Looks a payload up; `Ok(None)` on a cache miss (also when the row
    /// exists but its file vanished — the row is reported missing either
    /// way; the caller re-computes).
    pub fn get(&self, lib: &Library, key: &str) -> crate::Result<Option<Vec<u8>>> {
        let Some(path_str) = lib.cache_get(key) else {
            return Ok(None);
        };
        let path = PathBuf::from(path_str);
        let Ok(compressed) = fs::read(&path) else {
            return Ok(None);
        };
        let payload = zstd::stream::decode_all(&compressed[..]).map_err(IsgError::Zstd)?;
        Ok(Some(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_roundtrip_and_miss() {
        let lib = Library::open_in_memory().unwrap();
        let dir = std::env::temp_dir().join(format!("isg-cache-test-{}", std::process::id()));
        let store = CacheStore::new(&dir);

        let key = CacheStore::cache_key(b"hello world", "mono-clean", "{bg:255}", 1);
        let key2 = CacheStore::cache_key(b"different", "mono-clean", "{bg:255}", 1);
        assert_ne!(key, key2, "distinct inputs → distinct keys");
        // Key stability: same inputs, same key.
        assert_eq!(
            key,
            CacheStore::cache_key(b"hello world", "mono-clean", "{bg:255}", 1)
        );

        assert_eq!(store.get(&lib, &key).unwrap(), None, "miss before put");
        store.put(&lib, &key, b"hello world").unwrap();
        assert_eq!(
            store.get(&lib, &key).unwrap(),
            Some(b"hello world".to_vec())
        );

        // Overwrite is idempotent.
        store.put(&lib, &key, b"hello world v2").unwrap();
        assert_eq!(
            store.get(&lib, &key).unwrap(),
            Some(b"hello world v2".to_vec())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
