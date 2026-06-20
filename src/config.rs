use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub llm: LlmConfig,
    pub classification: ClassificationConfig,
    pub delivery: DeliveryConfig,
    pub quarantine: QuarantineConfig,
    pub feedback: FeedbackConfig,
    pub rules: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub model: String,
    pub temperature: f32,
    pub timeout_seconds: u64,
    pub json_response_format: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClassificationConfig {
    pub observe_only: bool,
    pub add_headers: bool,
    pub spam_threshold: f32,
    pub max_prompt_bytes: usize,
    pub max_body_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DeliveryConfig {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct QuarantineConfig {
    pub path: String,
    pub require_verdict: QuarantineVerdictPolicy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FeedbackConfig {
    pub enabled: bool,
    pub max_examples: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineVerdictPolicy {
    Spam,
    SpamOrUnsure,
    Any,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            classification: ClassificationConfig::default(),
            delivery: DeliveryConfig::default(),
            quarantine: QuarantineConfig::default(),
            feedback: FeedbackConfig::default(),
            rules: default_rules(),
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8080/v1/chat/completions".to_string(),
            api_key: None,
            model: "local-model".to_string(),
            temperature: 0.0,
            timeout_seconds: 30,
            json_response_format: true,
        }
    }
}

impl Default for ClassificationConfig {
    fn default() -> Self {
        Self {
            observe_only: true,
            add_headers: true,
            spam_threshold: 0.85,
            max_prompt_bytes: 48 * 1024,
            max_body_bytes: 32 * 1024,
        }
    }
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            command: "/usr/libexec/mail.local".to_string(),
            args: vec!["-f".to_string(), "{sender}".to_string(), "{user}".to_string()],
        }
    }
}

impl Default for QuarantineConfig {
    fn default() -> Self {
        Self {
            path: "{home}/spam".to_string(),
            require_verdict: QuarantineVerdictPolicy::Spam,
        }
    }
}

impl Default for FeedbackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_examples: 20,
            max_bytes: 8 * 1024,
        }
    }
}

/// Loads SLAC configuration from an explicit path or the default search path.
///
/// Search order without an explicit path is `~/.config/slac/slac.toml`, then
/// `/etc/slac.toml`. If neither exists, built-in defaults are returned with no
/// source path. Parse/read errors are returned so MDA mode can decide whether
/// to fail open.
pub fn load(path: Option<&Path>) -> Result<(Config, Option<PathBuf>), String> {
    if let Some(path) = path {
        let text = fs::read_to_string(path)
            .map_err(|err| format!("failed to read config {}: {err}", path.display()))?;
        let config = toml::from_str(&text)
            .map_err(|err| format!("failed to parse config {}: {err}", path.display()))?;
        return Ok((config, Some(path.to_path_buf())));
    }

    for candidate in default_config_paths() {
        if candidate.exists() {
            let text = fs::read_to_string(&candidate)
                .map_err(|err| format!("failed to read config {}: {err}", candidate.display()))?;
            let config = toml::from_str(&text)
                .map_err(|err| format!("failed to parse config {}: {err}", candidate.display()))?;
            return Ok((config, Some(candidate)));
        }
    }

    Ok((Config::default(), None))
}

fn default_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(home) = env::var("HOME") {
        paths.push(PathBuf::from(home).join(".config/slac/slac.toml"));
    }
    paths.push(PathBuf::from("/etc/slac.toml"));
    paths
}

fn default_rules() -> Vec<String> {
    vec![
        "Unexpected attachments, archive files, or executable payloads increase spam likelihood."
            .to_string(),
        "Credential harvesting, urgent account warnings, and payment redirection are strong spam or phishing signals."
            .to_string(),
        "Legitimate transactional mail often has consistent sender identity, clear purpose, and matching headers."
            .to_string(),
        "Bulk marketing from unknown senders is more likely spam when unsubscribe and sender identity are unclear."
            .to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_observe_only() {
        let config = Config::default();
        assert!(config.classification.observe_only);
        assert_eq!(config.delivery.command, "/usr/libexec/mail.local");
    }
}
