use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DynamicRegexConfig {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default = "default_sync_interval_secs")]
    pub sync_interval_secs: u64,
    #[serde(default)]
    pub fallback_cache_path: String,
}

fn default_sync_interval_secs() -> u64 {
    3600
}

impl Default for DynamicRegexConfig {
    fn default() -> Self {
        Self {
            urls: Vec::new(),
            sync_interval_secs: default_sync_interval_secs(),
            fallback_cache_path: String::new(),
        }
    }
}

/// A single regex rule from a remote source or local cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DynamicRegexRule {
    pub pattern: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Flexible remote payload representation supporting:
/// 1. `{"rules": [{"pattern": "..."}, ...]}`
/// 2. `{"patterns": ["...", ...]}`
/// 3. Top-level array: `["pattern1", "pattern2"]`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RemoteRegexPayload {
    Rules { rules: Vec<DynamicRegexRule> },
    Patterns { patterns: Vec<String> },
    List(Vec<String>),
}

impl RemoteRegexPayload {
    /// Extracts all pattern strings from the payload.
    pub fn into_patterns(self) -> Vec<String> {
        match self {
            Self::Rules { rules } => rules
                .into_iter()
                .map(|r| r.pattern)
                .collect(),
            Self::Patterns { patterns } => patterns,
            Self::List(patterns) => patterns,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_payload_deserialization_rules() {
        let json = r#"{"rules": [{"pattern": "^test.*", "description": "test rule"}]}"#;
        let payload: RemoteRegexPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.into_patterns(), vec!["^test.*"]);
    }

    #[test]
    fn test_payload_deserialization_patterns() {
        let json = r#"{"patterns": ["^abc$", "def.*"]}"#;
        let payload: RemoteRegexPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.into_patterns(), vec!["^abc$", "def.*"]);
    }

    #[test]
    fn test_payload_deserialization_list() {
        let json = r#"["pattern1", "pattern2"]"#;
        let payload: RemoteRegexPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.into_patterns(), vec!["pattern1", "pattern2"]);
    }
}
