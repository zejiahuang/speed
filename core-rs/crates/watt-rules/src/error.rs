//! Error type shared by the rule engine.

use std::fmt;

/// Failures that can occur while loading, validating or storing a rule set.
#[derive(Debug)]
pub enum RuleError {
    /// The payload was not valid JSON, or did not match the expected shape.
    Json(serde_json::Error),
    /// Reading or writing the on-disk cache failed.
    Io(std::io::Error),
    /// The document parsed but contained no usable routing entries.
    NoUsableEntries,
    /// The payload was empty.
    Empty,
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleError::Json(err) => write!(f, "rule document is not valid JSON: {err}"),
            RuleError::Io(err) => write!(f, "rule cache i/o failed: {err}"),
            RuleError::NoUsableEntries => f.write_str("rule document has no usable entries"),
            RuleError::Empty => f.write_str("rule payload is empty"),
        }
    }
}

impl std::error::Error for RuleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RuleError::Json(err) => Some(err),
            RuleError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for RuleError {
    fn from(value: serde_json::Error) -> Self {
        RuleError::Json(value)
    }
}

impl From<std::io::Error> for RuleError {
    fn from(value: std::io::Error) -> Self {
        RuleError::Io(value)
    }
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, RuleError>;
