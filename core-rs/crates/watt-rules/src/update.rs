//! The rule document's lifecycle: what to start with, when to fetch, and what to
//! fall back to.
//!
//! Three sources, in order of preference:
//!
//! 1. **The cache.** A previous download, fresh or not. A slightly old copy of
//!    the real document still covers far more than the handful of entries that
//!    ship in the binary, so age alone is not a reason to discard it.
//! 2. **A download.** Attempted when the cache is missing or stale, and whenever
//!    the user asks for one.
//! 3. **The built-in set.** Small and hand-maintained, so the kernel can start on
//!    a device that has never been online.
//!
//! The network is behind [`Fetcher`], and deliberately so. This crate ships no
//! HTTP client: on Android the download belongs in Kotlin, where the platform
//! already has one and where the app owns the user's data budget and proxy
//! settings; a host binary plugs in `curl` instead. Putting it behind a trait is
//! also what makes the policy above testable without a network.
//!
//! ```text
//!   start ──▶ cache? ──yes──▶ run it ──▶ stale? ──yes──▶ download
//!               │                            │              │
//!               no                           no            fails
//!               │                            │              │
//!               ▼                            ▼              ▼
//!          builtin                        run it      keep running it
//! ```

use std::time::{Duration, SystemTime};

use crate::builtin::BUILTIN_RULES_JSON;
use crate::cache::RuleCache;
use crate::error::{Result, RuleError};
use crate::ruleset::{RuleSet, RuleSource};

/// Where a rule set came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleOrigin {
    /// The small set compiled into the binary.
    Builtin,
    /// A previously downloaded document, read from disk.
    Cache,
    /// A document fetched during this run.
    Downloaded,
    /// A document the operator pointed at directly, bypassing the cache.
    Provided,
}

impl RuleOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleOrigin::Builtin => "builtin",
            RuleOrigin::Cache => "cache",
            RuleOrigin::Downloaded => "downloaded",
            RuleOrigin::Provided => "provided",
        }
    }
}

impl std::fmt::Display for RuleOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rule set together with where it came from.
#[derive(Debug, Clone)]
pub struct LoadedRules {
    pub rules: RuleSet,
    pub origin: RuleOrigin,
    /// When the payload was fetched. `None` for the built-in set.
    pub fetched_at: Option<SystemTime>,
}

impl LoadedRules {
    /// Age of the document relative to `now`, when its fetch time is known.
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        self.fetched_at.and_then(|at| now.duration_since(at).ok())
    }

    /// One line for a log.
    pub fn describe(&self) -> String {
        let stats = self.rules.stats();
        format!(
            "origin={} version={} entries={} domains={}",
            self.origin,
            self.rules.meta().version,
            stats.entries,
            stats.domains
        )
    }
}

/// Something that can retrieve a rule document.
///
/// Deliberately one method with one argument. Conditional requests would need
/// response headers, and every header a fetcher could report is a header the
/// caller then has to reason about; the cache sidecar already carries an `etag`
/// field for the day a fetcher can supply one.
pub trait Fetcher {
    /// Fetch `url`, returning the raw bytes of the document.
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>>;
}

/// A [`Fetcher`] that reads a path off the local filesystem.
///
/// For a pinned deployment that serves the document from disk, and for tests
/// that would otherwise need a server.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileFetcher;

impl Fetcher for FileFetcher {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(url)?)
    }
}

/// Decides which rule document to run and when to look for a new one.
///
/// Holds no clock of its own: [`RuleUpdater::should_refresh`] takes the current
/// time, so a daemon that was suspended for a week resumes with a stale cache
/// and refreshes on its next tick rather than on a timer that drifted.
#[derive(Debug, Clone)]
pub struct RuleUpdater {
    url: String,
    cache: RuleCache,
    max_age: Duration,
}

impl RuleUpdater {
    /// Build an updater for `url`, caching under `cache`, considering a cached
    /// document stale once it is older than `max_age`.
    pub fn new(url: impl Into<String>, cache: RuleCache, max_age: Duration) -> Self {
        Self {
            url: url.into(),
            cache,
            max_age,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn cache(&self) -> &RuleCache {
        &self.cache
    }

    pub fn max_age(&self) -> Duration {
        self.max_age
    }

    /// The built-in set, which needs no disk and no network.
    pub fn builtin() -> Result<LoadedRules> {
        Ok(LoadedRules {
            rules: RuleSet::from_str(BUILTIN_RULES_JSON, RuleSource::Builtin)?,
            origin: RuleOrigin::Builtin,
            fetched_at: None,
        })
    }

    /// The set to start with, without touching the network.
    ///
    /// Prefers any cache over the built-in set, fresh or not. A cache that will
    /// not parse is treated as absent rather than fatal: the kernel has to start,
    /// and silently falling back is better than refusing to run because a file
    /// was truncated by a power cut.
    pub fn load(&self) -> Result<LoadedRules> {
        match self.cache.load() {
            Ok(Some(rules)) => Ok(LoadedRules {
                rules,
                origin: RuleOrigin::Cache,
                fetched_at: self.cache.meta().and_then(|meta| meta.fetched_at()),
            }),
            Ok(None) | Err(_) => Self::builtin(),
        }
    }

    /// Compile a document the operator supplied, ignoring cache and network.
    ///
    /// The pinned path: a deployment that serves the rule file from its own
    /// storage, or a run that must be reproducible.
    pub fn pinned(raw: &[u8]) -> Result<LoadedRules> {
        Ok(LoadedRules {
            rules: RuleSet::from_slice(raw, RuleSource::Provided)?,
            origin: RuleOrigin::Provided,
            fetched_at: None,
        })
    }

    /// True when the cache is missing or older than `max_age`.
    ///
    /// A missing cache is always stale, which is what makes the first run after
    /// installation fetch immediately.
    pub fn should_refresh(&self, now: SystemTime) -> bool {
        self.cache.is_stale(now, self.max_age)
    }

    /// Download a fresh document, cache it, and return it.
    ///
    /// A failure changes nothing on disk: the cache is only written once the
    /// payload has parsed, so the caller keeps running whatever
    /// [`RuleUpdater::load`] gave it. The error is returned rather than turned
    /// into a fallback because "the network failed" and "which rules to run" are
    /// different decisions, and only the caller knows how to report the first.
    pub fn refresh(&self, fetcher: &mut dyn Fetcher, now: SystemTime) -> Result<LoadedRules> {
        let raw = fetcher.fetch(&self.url)?;
        let rules = self.cache.store(&raw, None, now)?;
        Ok(LoadedRules {
            rules,
            origin: RuleOrigin::Downloaded,
            fetched_at: Some(now),
        })
    }

    /// Refresh if the cache is stale, and report what happened either way.
    ///
    /// The convenience the three call sites in a daemon share: the first tick
    /// after start, the periodic tick, and the user pressing "update now" all
    /// want the same thing, and all three have to keep running when it fails.
    pub fn refresh_if_stale(
        &self,
        fetcher: &mut dyn Fetcher,
        now: SystemTime,
    ) -> Option<Result<LoadedRules>> {
        if !self.should_refresh(now) {
            return None;
        }
        Some(self.refresh(fetcher, now))
    }
}

/// Failure reported by a [`Fetcher`] that wraps a command line tool.
///
/// A fetcher built on `curl` has no `io::Error` of its own to hand back, and the
/// caller should not have to invent one.
pub fn fetch_failed(message: impl Into<String>) -> RuleError {
    RuleError::Io(std::io::Error::other(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    const GOOD: &str = r#"{
      "meta": { "version": "1.2.3", "update_time": "2026/01/01 00:00" },
      "groups": [
        { "group": "test", "entries": [ { "id": "1", "name": "T", "domains": ["t.example"], "ips": ["203.0.113.9"], "port": "443" } ] }
      ]
    }"#;

    const NEWER: &str = r#"{
      "meta": { "version": "2.0.0", "update_time": "2026/02/02 00:00" },
      "groups": [
        { "group": "test", "entries": [ { "id": "1", "name": "T", "domains": ["t.example"], "ips": ["203.0.113.9"], "port": "443" } ] }
      ]
    }"#;

    enum Reply {
        Bytes(&'static str),
        Fail,
    }

    struct FakeFetcher {
        reply: Reply,
        calls: usize,
    }

    impl FakeFetcher {
        fn serving(body: &'static str) -> Self {
            Self {
                reply: Reply::Bytes(body),
                calls: 0,
            }
        }

        fn broken() -> Self {
            Self {
                reply: Reply::Fail,
                calls: 0,
            }
        }
    }

    impl Fetcher for FakeFetcher {
        fn fetch(&mut self, _url: &str) -> Result<Vec<u8>> {
            self.calls += 1;
            match self.reply {
                Reply::Bytes(body) => Ok(body.as_bytes().to_vec()),
                Reply::Fail => Err(RuleError::Empty),
            }
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("watt-update-{tag}-{unique}"))
    }

    fn updater(tag: &str, max_age: Duration) -> (RuleUpdater, PathBuf) {
        let dir = temp_dir(tag);
        let updater = RuleUpdater::new(
            "https://example.test/rules",
            RuleCache::new(&dir),
            max_age,
        );
        (updater, dir)
    }

    #[test]
    fn the_builtin_set_is_always_available() {
        let loaded = RuleUpdater::builtin().unwrap();
        assert_eq!(loaded.origin, RuleOrigin::Builtin);
        assert!(loaded.fetched_at.is_none());
        assert!(loaded.rules.stats().entries > 0, "the builtin set must be usable");
    }

    #[test]
    fn an_empty_cache_starts_on_the_builtin_set() {
        let (updater, dir) = updater("empty", Duration::from_secs(3600));
        let loaded = updater.load().unwrap();
        assert_eq!(loaded.origin, RuleOrigin::Builtin);
        assert!(updater.should_refresh(SystemTime::now()), "no cache means fetch");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_refresh_lands_in_the_cache_and_clears_the_staleness() {
        let (updater, dir) = updater("refresh", Duration::from_secs(3600));
        let now = SystemTime::now();
        let mut fetcher = FakeFetcher::serving(NEWER);

        let loaded = updater.refresh(&mut fetcher, now).unwrap();
        assert_eq!(loaded.origin, RuleOrigin::Downloaded);
        assert_eq!(loaded.rules.meta().version, "2.0.0");
        assert_eq!(fetcher.calls, 1);

        assert!(!updater.should_refresh(now));
        assert!(
            updater.refresh_if_stale(&mut fetcher, now).is_none(),
            "a fresh cache must not be fetched again"
        );
        assert_eq!(fetcher.calls, 1);

        // The next start reads it from disk rather than the network.
        let reloaded = updater.load().unwrap();
        assert_eq!(reloaded.origin, RuleOrigin::Cache);
        assert_eq!(reloaded.rules.meta().version, "2.0.0");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stale_cache_is_refreshed_and_the_old_copy_is_used_until_then() {
        let (updater, dir) = updater("stale", Duration::from_secs(60));
        let start = SystemTime::now();
        let mut fetcher = FakeFetcher::serving(GOOD);
        updater.refresh(&mut fetcher, start).unwrap();

        let later = start + Duration::from_secs(120);
        assert!(updater.should_refresh(later));
        // Until the refresh happens, the cached document is what runs.
        assert_eq!(updater.load().unwrap().rules.meta().version, "1.2.3");

        let mut newer = FakeFetcher::serving(NEWER);
        let refreshed = updater
            .refresh_if_stale(&mut newer, later)
            .expect("a stale cache must be refreshed")
            .unwrap();
        assert_eq!(refreshed.origin, RuleOrigin::Downloaded);
        assert_eq!(refreshed.rules.meta().version, "2.0.0");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_download_leaves_the_cache_alone() {
        let (updater, dir) = updater("failure", Duration::from_secs(60));
        let start = SystemTime::now();
        updater
            .refresh(&mut FakeFetcher::serving(GOOD), start)
            .unwrap();

        let mut broken = FakeFetcher::broken();
        let err = updater
            .refresh(&mut broken, start + Duration::from_secs(120))
            .expect_err("the failure must surface");
        assert!(matches!(err, RuleError::Empty));

        let still_there = updater.load().unwrap();
        assert_eq!(still_there.origin, RuleOrigin::Cache);
        assert_eq!(still_there.rules.meta().version, "1.2.3");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_cache_falls_back_to_the_builtin_set() {
        let (updater, dir) = updater("corrupt", Duration::from_secs(3600));
        updater.cache().ensure_dir().unwrap();
        std::fs::write(updater.cache().payload_path(), b"{ not json at all").unwrap();

        let loaded = updater.load().unwrap();
        assert_eq!(
            loaded.origin,
            RuleOrigin::Builtin,
            "a truncated cache must not stop the kernel from starting"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_file_fetcher_reads_a_local_document() {
        let dir = temp_dir("filefetcher");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules.json");
        std::fs::write(&path, GOOD).unwrap();

        let mut fetcher = FileFetcher;
        let raw = fetcher.fetch(path.to_str().unwrap()).unwrap();
        assert_eq!(RuleSet::from_slice(&raw, RuleSource::Provided).unwrap().meta().version, "1.2.3");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_description_names_the_origin_and_the_version() {
        let (updater, dir) = updater("describe", Duration::from_secs(3600));
        updater
            .refresh(&mut FakeFetcher::serving(GOOD), SystemTime::now())
            .unwrap();
        let described = updater.load().unwrap().describe();
        assert!(described.contains("origin=cache"), "{described}");
        assert!(described.contains("version=1.2.3"), "{described}");
        assert!(described.contains("entries=1"), "{described}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
