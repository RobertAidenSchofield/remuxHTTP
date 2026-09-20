use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use regex::Regex;
use remux_utils::regex::compile_safe_regexes;
use tracing::{debug, info, warn};

use crate::config_ext::{DynamicRegexConfig, RemoteRegexPayload};

/// Represents the active compiled regex state.
#[derive(Clone, Default)]
pub struct DynamicRegexState {
    patterns: Vec<String>,
    compiled: Arc<Vec<Regex>>,
}

impl DynamicRegexState {
    pub fn new(patterns: Vec<String>, compiled: Vec<Regex>) -> Self {
        Self {
            patterns,
            compiled: Arc::new(compiled),
        }
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn compiled(&self) -> &[Regex] {
        &self.compiled
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.compiled
            .iter()
            .any(|re| re.is_match(text))
    }

    pub fn matching_patterns<'a>(&'a self, text: &'a str) -> Vec<&'a str> {
        self.compiled
            .iter()
            .zip(
                self.patterns
                    .iter(),
            )
            .filter_map(|(re, pat)| {
                if re.is_match(text) {
                    Some(pat.as_str())
                } else {
                    None
                }
            })
            .collect()
    }
}

pub struct RegexSyncService {
    client: reqwest::Client,
    config: DynamicRegexConfig,
    state: Arc<ArcSwap<DynamicRegexState>>,
}

impl RegexSyncService {
    pub fn new(config: DynamicRegexConfig) -> (Self, Arc<ArcSwap<DynamicRegexState>>) {
        let state = Arc::new(ArcSwap::from_pointee(DynamicRegexState::default()));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("remux-server/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let service = Self {
            client,
            config,
            state: state.clone(),
        };

        (service, state)
    }

    pub fn with_client(
        config: DynamicRegexConfig,
        client: reqwest::Client,
    ) -> (Self, Arc<ArcSwap<DynamicRegexState>>) {
        let state = Arc::new(ArcSwap::from_pointee(DynamicRegexState::default()));
        let service = Self {
            client,
            config,
            state: state.clone(),
        };
        (service, state)
    }

    /// Resolves the concrete file path for the fallback cache.
    fn resolve_cache_file_path(fallback_path: &str) -> PathBuf {
        let path = Path::new(fallback_path);
        if path.is_dir() {
            path.join("regex_cache.json")
        } else {
            path.to_path_buf()
        }
    }

    /// Attempts to read and compile patterns from the local fallback cache file.
    pub fn load_from_cache(fallback_path: &str) -> Option<DynamicRegexState> {
        let cache_file = Self::resolve_cache_file_path(fallback_path);
        if !cache_file.exists() {
            debug!(path = %cache_file.display(), "fallback cache file does not exist");
            return None;
        }

        match fs::read_to_string(&cache_file) {
            Ok(content) => match serde_json::from_str::<RemoteRegexPayload>(&content) {
                Ok(payload) => {
                    let patterns = payload.into_patterns();
                    let (compiled, invalid) = compile_safe_regexes(&patterns);
                    if !invalid.is_empty() {
                        warn!(
                            count = invalid.len(),
                            "safely ignored invalid regex patterns in cache: {:?}",
                            invalid
                        );
                    }
                    info!(
                        path = %cache_file.display(),
                        valid_patterns = compiled.len(),
                        "successfully loaded regex rules from fallback cache"
                    );
                    Some(DynamicRegexState::new(patterns, compiled))
                }
                Err(err) => {
                    warn!(
                        path = %cache_file.display(),
                        error = %err,
                        "failed to deserialize fallback cache JSON"
                    );
                    None
                }
            },
            Err(err) => {
                warn!(
                    path = %cache_file.display(),
                    error = %err,
                    "failed to read fallback cache file"
                );
                None
            }
        }
    }

    /// Saves valid pattern strings to the configured fallback cache file atomically.
    pub fn save_to_cache(fallback_path: &str, patterns: &[String]) -> Result<()> {
        let cache_file = Self::resolve_cache_file_path(fallback_path);
        if let Some(parent) = cache_file.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create cache directory: {}", parent.display())
            })?;
        }

        let json = serde_json::to_string_pretty(&RemoteRegexPayload::Patterns {
            patterns: patterns.to_vec(),
        })?;

        let temp_file = cache_file.with_extension("tmp");
        fs::write(&temp_file, json).with_context(|| {
            format!(
                "failed to write temporary cache file: {}",
                temp_file.display()
            )
        })?;

        fs::rename(&temp_file, &cache_file).with_context(|| {
            format!("failed to rename cache file to: {}", cache_file.display())
        })?;

        debug!(path = %cache_file.display(), count = patterns.len(), "saved regex rules to cache");
        Ok(())
    }

    /// Performs a single synchronization pass.
    /// Fetches from configured remote URLs, compiles patterns safely, updates disk cache,
    /// or falls back to the disk cache upon failure.
    pub async fn sync(&self) -> Result<()> {
        let mut all_patterns = Vec::new();
        let mut any_fetch_succeeded = false;

        for url in &self
            .config
            .urls
        {
            match self
                .fetch_single_url(url)
                .await
            {
                Ok(patterns) => {
                    any_fetch_succeeded = true;
                    all_patterns.extend(patterns);
                }
                Err(err) => {
                    warn!(
                        url = %url,
                        error = %err,
                        "failed to fetch remote regex rules from URL"
                    );
                }
            }
        }

        if any_fetch_succeeded {
            // Deduplicate pattern strings while preserving order
            let mut seen = std::collections::HashSet::new();
            let deduped_patterns: Vec<String> = all_patterns
                .into_iter()
                .filter(|p| seen.insert(p.clone()))
                .collect();

            let (compiled, invalid) = compile_safe_regexes(&deduped_patterns);
            if !invalid.is_empty() {
                warn!(
                    count = invalid.len(),
                    "safely ignored invalid regex patterns: {:?}", invalid
                );
            }

            info!(
                valid_count = compiled.len(),
                invalid_count = invalid.len(),
                "successfully compiled dynamic regex rules from remote sources"
            );

            // Persist valid patterns to disk cache
            if !self
                .config
                .fallback_cache_path
                .is_empty()
            {
                if let Err(e) = Self::save_to_cache(
                    &self
                        .config
                        .fallback_cache_path,
                    &deduped_patterns,
                ) {
                    warn!(error = %e, "failed to persist regex patterns to fallback cache");
                }
            }

            self.state
                .store(Arc::new(DynamicRegexState::new(deduped_patterns, compiled)));
            Ok(())
        } else {
            // Remote sources failed or no URLs configured; fall back to disk cache
            if !self
                .config
                .fallback_cache_path
                .is_empty()
            {
                if let Some(cached_state) = Self::load_from_cache(
                    &self
                        .config
                        .fallback_cache_path,
                ) {
                    self.state
                        .store(Arc::new(cached_state));
                    return Ok(());
                }
            }

            if self
                .config
                .urls
                .is_empty()
            {
                debug!(
                    "no remote regex URLs configured and no cache loaded; regex engine is idle"
                );
            } else {
                warn!(
                    "all remote regex fetches failed and fallback cache was unavailable"
                );
            }
            Ok(())
        }
    }

    async fn fetch_single_url(&self, url: &str) -> Result<Vec<String>> {
        let res = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("HTTP request failed for {url}"))?;

        let status = res.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            warn!(url = %url, status = %status, "remote regex source forbidden (403)");
            anyhow::bail!("forbidden (403) from {url}");
        }

        if !status.is_success() {
            warn!(url = %url, status = %status, "non-200 HTTP response from remote regex source");
            anyhow::bail!("HTTP status {status} from {url}");
        }

        let body = res
            .text()
            .await
            .with_context(|| format!("failed to read response body from {url}"))?;

        let payload: RemoteRegexPayload =
            serde_json::from_str(&body).with_context(|| {
                format!("failed to deserialize remote regex payload from {url}")
            })?;

        Ok(payload.into_patterns())
    }

    /// Spawns the background synchronization task that executes on the defined interval.
    pub fn spawn_sync_worker(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Perform initial sync on startup
            if let Err(e) = self
                .sync()
                .await
            {
                warn!(error = %e, "initial dynamic regex sync failed");
            }

            let interval_secs = self
                .config
                .sync_interval_secs
                .max(1);
            let mut interval =
                tokio::time::interval(Duration::from_secs(interval_secs));
            interval
                .tick()
                .await; // First tick completes immediately

            loop {
                interval
                    .tick()
                    .await;
                debug!("running scheduled dynamic regex synchronization");
                if let Err(e) = self
                    .sync()
                    .await
                {
                    warn!(error = %e, "scheduled dynamic regex synchronization encountered an error");
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use tempfile::tempdir;

    #[test]
    fn test_dynamic_regex_state_matching() {
        let patterns = vec!["^stream_.*".to_string(), ".*\\.mkv$".to_string()];
        let (compiled, _) = compile_safe_regexes(&patterns);
        let state = DynamicRegexState::new(patterns, compiled);

        assert!(state.is_match("stream_live_1080p"));
        assert!(state.is_match("movie.mkv"));
        assert!(!state.is_match("movie.mp4"));

        let matched = state.matching_patterns("stream_live_1080p.mkv");
        assert_eq!(matched.len(), 2);
    }

    #[test]
    fn test_cache_save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let cache_path = dir
            .path()
            .join("test_regex_cache.json");
        let path_str = cache_path
            .to_str()
            .unwrap();

        let patterns = vec![
            "^valid_pattern_[0-9]+$".to_string(),
            "[invalid_pattern".to_string(), // Invalid pattern syntax
            "another_valid.*".to_string(),
        ];

        RegexSyncService::save_to_cache(path_str, &patterns).unwrap();

        let loaded =
            RegexSyncService::load_from_cache(path_str).expect("cache should load");
        assert_eq!(loaded.patterns(), &patterns);
        // The invalid pattern should be ignored safely without panic
        assert_eq!(
            loaded
                .compiled()
                .len(),
            2
        );
        assert!(loaded.is_match("valid_pattern_42"));
        assert!(loaded.is_match("another_valid_test"));
    }

    #[tokio::test]
    async fn test_sync_successful_fetch_and_cache() {
        let server = MockServer::start();
        let dir = tempdir().unwrap();
        let cache_path = dir
            .path()
            .join("cache.json");
        let path_str = cache_path
            .to_str()
            .unwrap()
            .to_string();

        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rules.json");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"patterns": ["^live_.*", ".*\\.ts$"]}"#);
        });

        let config = DynamicRegexConfig {
            urls: vec![server.url("/rules.json")],
            sync_interval_secs: 60,
            fallback_cache_path: path_str.clone(),
        };

        let (service, state) = RegexSyncService::new(config);
        service
            .sync()
            .await
            .unwrap();

        mock.assert();
        let current = state.load();
        assert!(current.is_match("live_news"));
        assert!(current.is_match("segment.ts"));
        assert!(!current.is_match("segment.mp4"));

        // Cache file should have been written
        assert!(cache_path.exists());
    }

    #[tokio::test]
    async fn test_sync_forbidden_403_falls_back_to_cache() {
        let server = MockServer::start();
        let dir = tempdir().unwrap();
        let cache_path = dir
            .path()
            .join("cache.json");
        let path_str = cache_path
            .to_str()
            .unwrap()
            .to_string();

        // Populate fallback cache with pre-existing rules
        let initial_patterns = vec!["^cached_rule_.*$".to_string()];
        RegexSyncService::save_to_cache(&path_str, &initial_patterns).unwrap();

        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/forbidden.json");
            then.status(403);
        });

        let config = DynamicRegexConfig {
            urls: vec![server.url("/forbidden.json")],
            sync_interval_secs: 60,
            fallback_cache_path: path_str,
        };

        let (service, state) = RegexSyncService::new(config);
        service
            .sync()
            .await
            .unwrap();

        mock.assert();
        let current = state.load();
        // Should have fallen back to cache rules
        assert!(current.is_match("cached_rule_123"));
        assert_eq!(current.patterns(), &["^cached_rule_.*$"]);
    }

    #[tokio::test]
    async fn test_sync_unresolvable_url_falls_back_to_cache() {
        let dir = tempdir().unwrap();
        let cache_path = dir
            .path()
            .join("cache.json");
        let path_str = cache_path
            .to_str()
            .unwrap()
            .to_string();

        let initial_patterns = vec!["^fallback_regex.*".to_string()];
        RegexSyncService::save_to_cache(&path_str, &initial_patterns).unwrap();

        let config = DynamicRegexConfig {
            urls: vec!["http://unresolvable.invalid.local/rules.json".to_string()],
            sync_interval_secs: 60,
            fallback_cache_path: path_str,
        };

        let (service, state) = RegexSyncService::new(config);
        service
            .sync()
            .await
            .unwrap();

        let current = state.load();
        assert!(current.is_match("fallback_regex_match"));
    }
}
