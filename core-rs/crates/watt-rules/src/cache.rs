//! On-disk cache for the downloaded rule document.
//!
//! The cache is a pair of files inside one directory:
//!
//! * `rules.json` — the payload exactly as received, so a future parser fix can
//!   re-read old data without a network round trip.
//! * `rules.meta.json` — fetch time, upstream version, size and validator.
//!
//! Writes are atomic: the payload is written to a temporary file and then
//! renamed over the target, so a crash or a killed process can never leave a
//! truncated rule file that would fail to parse on the next start.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Result, RuleError};
use crate::ruleset::{RuleSet, RuleSource};

/// Payload file name inside the cache directory.
pub const PAYLOAD_FILE: &str = "rules.json";
/// Sidecar metadata file name inside the cache directory.
pub const META_FILE: &str = "rules.meta.json";

/// Sidecar metadata describing the cached payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheMeta {
    /// Seconds since the Unix epoch when the payload was fetched.
    #[serde(default)]
    pub fetched_at_unix: u64,
    /// Upstream `meta.version` at fetch time.
    #[serde(default)]
    pub version: String,
    /// Payload size in bytes.
    #[serde(default)]
    pub bytes: usize,
    /// HTTP `ETag` or `Last-Modified`, when the server supplied one.
    #[serde(default)]
    pub etag: Option<String>,
}

impl CacheMeta {
    /// Fetch time as a [`SystemTime`], when it is known.
    pub fn fetched_at(&self) -> Option<SystemTime> {
        if self.fetched_at_unix == 0 {
            None
        } else {
            Some(UNIX_EPOCH + Duration::from_secs(self.fetched_at_unix))
        }
    }

    /// Age of the cached payload relative to `now`.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        let fetched = self.fetched_at()?;
        now.duration_since(fetched).ok()
    }
}

/// Directory backed rule cache.
#[derive(Debug, Clone)]
pub struct RuleCache {
    dir: PathBuf,
}

impl RuleCache {
    /// Create a cache rooted at `dir`. The directory is not created yet.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn payload_path(&self) -> PathBuf {
        self.dir.join(PAYLOAD_FILE)
    }

    pub fn meta_path(&self) -> PathBuf {
        self.dir.join(META_FILE)
    }

    /// Create the cache directory if it is missing.
    pub fn ensure_dir(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        Ok(())
    }

    /// Read the raw cached payload, or `None` when nothing has been cached yet.
    pub fn read_raw(&self) -> Result<Option<Vec<u8>>> {
        match fs::read(self.payload_path()) {
            Ok(bytes) if !bytes.is_empty() => Ok(Some(bytes)),
            Ok(_) => Ok(None),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(RuleError::Io(err)),
        }
    }

    /// Read and parse the cached payload.
    pub fn load(&self) -> Result<Option<RuleSet>> {
        match self.read_raw()? {
            Some(bytes) => Ok(Some(RuleSet::from_slice(&bytes, RuleSource::Cache)?)),
            None => Ok(None),
        }
    }

    /// Read the sidecar metadata, or `None` when it is absent or unreadable.
    ///
    /// A corrupt sidecar is not fatal: the payload is still usable, so the caller
    /// only loses age information.
    pub fn meta(&self) -> Option<CacheMeta> {
        let bytes = fs::read(self.meta_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Age of the cached payload, or `None` when it has never been fetched.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        self.meta()?.age(now)
    }

    /// True when the cache is missing or older than `max_age`.
    pub fn is_stale(&self, now: SystemTime, max_age: Duration) -> bool {
        match self.age(now) {
            Some(age) => age > max_age,
            None => true,
        }
    }

    /// Parse `raw` and, only if it is valid, replace the cache contents.
    ///
    /// Parsing first means a bad download can never destroy a good cache.
    pub fn store(
        &self,
        raw: &[u8],
        etag: Option<String>,
        fetched_at: SystemTime,
    ) -> Result<RuleSet> {
        let rules = RuleSet::from_slice(raw, RuleSource::Cache)?;
        self.ensure_dir()?;

        let tmp = self.dir.join(format!("{PAYLOAD_FILE}.tmp"));
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(raw)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, self.payload_path())?;

        let meta = CacheMeta {
            fetched_at_unix: fetched_at
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            version: rules.meta().version.clone(),
            bytes: raw.len(),
            etag,
        };
        let meta_tmp = self.dir.join(format!("{META_FILE}.tmp"));
        {
            let mut file = fs::File::create(&meta_tmp)?;
            file.write_all(serde_json::to_vec_pretty(&meta)?.as_slice())?;
            file.sync_all()?;
        }
        fs::rename(&meta_tmp, self.meta_path())?;

        Ok(rules)
    }

    /// Remove the cached payload and sidecar. Missing files are not an error.
    pub fn clear(&self) -> Result<()> {
        for path in [self.payload_path(), self.meta_path()] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(RuleError::Io(err)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &str = r#"{
      "meta": { "version": "9.9.9", "update_time": "2026/01/01 00:00" },
      "groups": [
        { "group": "test", "entries": [ { "id": "1", "name": "T", "domains": ["t.example"], "ips": ["203.0.113.9"], "port": "443" } ] }
      ]
    }"#;

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("watt-rules-{tag}-{unique}"))
    }

    #[test]
    fn round_trips_a_payload() {
        let dir = temp_dir("roundtrip");
        let cache = RuleCache::new(&dir);
        assert!(cache.load().unwrap().is_none(), "empty cache must load as None");

        let stored = cache.store(SMALL.as_bytes(), Some("\"abc\"".into()), SystemTime::now()).unwrap();
        assert_eq!(stored.meta().version, "9.9.9");

        let loaded = cache.load().unwrap().expect("cached payload must load");
        assert_eq!(loaded.meta().version, "9.9.9");
        assert_eq!(loaded.stats().entries, 1);

        let meta = cache.meta().expect("sidecar must exist");
        assert_eq!(meta.version, "9.9.9");
        assert_eq!(meta.bytes, SMALL.len());
        assert_eq!(meta.etag.as_deref(), Some("\"abc\""));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_invalid_payload_without_clobbering_the_cache() {
        let dir = temp_dir("reject");
        let cache = RuleCache::new(&dir);
        cache.store(SMALL.as_bytes(), None, SystemTime::now()).unwrap();

        let err = cache.store(b"{ not json", None, SystemTime::now()).unwrap_err();
        assert!(matches!(err, RuleError::Json(_)), "got {err:?}");

        let still_there = cache.load().unwrap().expect("previous payload must survive");
        assert_eq!(still_there.meta().version, "9.9.9");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reports_staleness() {
        let dir = temp_dir("stale");
        let cache = RuleCache::new(&dir);
        let now = SystemTime::now();
        cache.store(SMALL.as_bytes(), None, now).unwrap();

        assert!(!cache.is_stale(now, Duration::from_secs(60)));
        assert!(cache.is_stale(now + Duration::from_secs(120), Duration::from_secs(60)));

        cache.clear().unwrap();
        assert!(cache.is_stale(now, Duration::from_secs(60)));

        fs::remove_dir_all(&dir).ok();
    }
}
