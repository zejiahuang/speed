//! Fetching the rule document.
//!
//! The kernel deliberately ships no HTTP client. On Android the download happens
//! in Kotlin, where the platform already has one and where the app owns the
//! user's data budget and proxy settings. On a host the pragmatic equivalent is
//! `curl`: it is present nearly everywhere, it handles TLS, proxies and
//! redirects, and using it costs no dependency at all.

use std::process::Command;
use std::time::Duration;

use watt_rules::update::fetch_failed;
use watt_rules::{Fetcher, Result, RuleError};

/// Default program used to retrieve the document.
pub const DEFAULT_PROGRAM: &str = "curl";

/// Fetches the rule document by running `curl`.
#[derive(Debug, Clone)]
pub struct CurlFetcher {
    program: String,
    timeout: Duration,
}

impl Default for CurlFetcher {
    fn default() -> Self {
        Self::new(DEFAULT_PROGRAM, Duration::from_secs(60))
    }
}

impl CurlFetcher {
    /// Build a fetcher that runs `program`, giving up after `timeout`.
    pub fn new(program: impl Into<String>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            timeout,
        }
    }
}

impl Fetcher for CurlFetcher {
    fn fetch(&mut self, url: &str) -> Result<Vec<u8>> {
        // -f makes an HTTP error status a failure instead of a body, -s silences
        // the progress meter, -S puts the error back on stderr, -L follows
        // redirects. Without -f a 404 page would be parsed as a rule document.
        let timeout = self.timeout.as_secs().to_string();
        let output = Command::new(&self.program)
            .args(["-fsSL", "--max-time", &timeout, url])
            .output()
            .map_err(|err| fetch_failed(format!("could not run {}: {err}", self.program)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(fetch_failed(format!(
                "{} exited with {}: {}",
                self.program,
                output.status,
                stderr.trim()
            )));
        }

        if output.stdout.is_empty() {
            return Err(RuleError::Empty);
        }

        Ok(output.stdout)
    }
}
