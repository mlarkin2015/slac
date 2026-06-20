use crate::config::LlmConfig;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct Classification {
    pub spam_probability: f32,
    pub verdict: Verdict,
    #[serde(default, deserialize_with = "deserialize_reasons")]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Spam,
    Ham,
    Unsure,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Ham => "ham",
            Self::Unsure => "unsure",
        }
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat<'a>>,
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    content: String,
}

/// Sends a bounded, already-constructed classification prompt to an
/// OpenAI-compatible chat completions endpoint.
///
/// Assumptions: `prompt` already treats mail content as untrusted text and has
/// been size-limited by the caller. This function does not enable tools or
/// function calling. Postcondition: successful results have passed local JSON
/// schema and probability-range validation.
pub fn classify(prompt: &str, config: &LlmConfig) -> Result<Classification, String> {
    let request = ChatRequest {
        model: &config.model,
        messages: vec![
            Message {
                role: "system",
                content: "You classify email for spam filtering. Return strict JSON only.",
            },
            Message {
                role: "user",
                content: prompt,
            },
        ],
        temperature: config.temperature,
        response_format: config
            .json_response_format
            .then_some(ResponseFormat { kind: "json_object" }),
    };

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(config.timeout_seconds))
        .build();
    let mut http_request = agent.post(&config.endpoint).set("Content-Type", "application/json");

    if let Some(api_key) = &config.api_key {
        http_request = http_request.set("Authorization", &format!("Bearer {api_key}"));
    }

    let response = http_request
        .send_json(serde_json::to_value(&request).map_err(|err| err.to_string())?)
        .map_err(|err| format!("classifier request failed: {err}"))?;

    let chat: ChatResponse = response
        .into_json()
        .map_err(|err| format!("classifier response was not valid chat JSON: {err}"))?;

    let content = chat
        .choices
        .first()
        .ok_or("classifier response had no choices")?
        .message
        .content
        .trim();

    parse_classification(content)
}

/// Parses the assistant message body into a normalized `Classification`.
///
/// Accepts raw JSON or JSON wrapped in a Markdown fence because several local
/// models produce fenced JSON despite strict prompting. Postcondition: returned
/// probabilities are always in `0.0..=1.0`; `reasons` is normalized to a vector.
pub fn parse_classification(content: &str) -> Result<Classification, String> {
    let normalized = normalize_json_content(content);
    let parsed: Classification = serde_json::from_str(normalized).map_err(|err| {
        format!(
            "classifier content was not valid classification JSON: {err}; content_prefix={:?}",
            content_prefix(content)
        )
    })?;
    if !(0.0..=1.0).contains(&parsed.spam_probability) {
        return Err(format!(
            "spam_probability out of range: {}",
            parsed.spam_probability
        ));
    }
    Ok(parsed)
}

fn normalize_json_content(content: &str) -> &str {
    let trimmed = content.trim();
    let Some(after_opening_fence) = trimmed.strip_prefix("```") else {
        return trimmed;
    };

    let after_language = after_opening_fence
        .strip_prefix("json")
        .unwrap_or(after_opening_fence)
        .trim_start();

    after_language
        .strip_suffix("```")
        .unwrap_or(after_language)
        .trim()
}

fn content_prefix(content: &str) -> String {
    let mut prefix: String = content.chars().take(240).collect();
    if content.chars().count() > 240 {
        prefix.push_str("...");
    }
    prefix
}

fn deserialize_reasons<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Reasons {
        Many(Vec<String>),
        One(String),
    }

    Ok(match Option::<Reasons>::deserialize(deserializer)? {
        Some(Reasons::Many(reasons)) => reasons,
        Some(Reasons::One(reason)) => vec![reason],
        None => Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_classification() {
        let parsed = parse_classification(
            r#"{"spam_probability":0.91,"verdict":"spam","reasons":["credential phishing"]}"#,
        )
        .unwrap();
        assert_eq!(parsed.verdict, Verdict::Spam);
        assert_eq!(parsed.reasons.len(), 1);
    }

    #[test]
    fn rejects_out_of_range_probability() {
        let err =
            parse_classification(r#"{"spam_probability":1.5,"verdict":"spam","reasons":[]}"#)
                .unwrap_err();
        assert!(err.contains("out of range"));
    }

    #[test]
    fn parses_fenced_json_classification() {
        let parsed = parse_classification(
            r#"```json
{"spam_probability":0.0,"verdict":"ham","reasons":["mailing list"]}
```"#,
        )
        .unwrap();
        assert_eq!(parsed.verdict, Verdict::Ham);
    }

    #[test]
    fn parses_string_reason_classification() {
        let parsed = parse_classification(
            r#"{"spam_probability":0.12,"verdict":"ham","reasons":"legitimate sender"}"#,
        )
        .unwrap();
        assert_eq!(parsed.verdict, Verdict::Ham);
        assert_eq!(parsed.reasons, vec!["legitimate sender"]);
    }
}
