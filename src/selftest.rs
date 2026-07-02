/*
BSD 2-Clause License

Copyright (c) 2026, Mike Larkin <mlarkin@nested.page>

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
INCLUDING NEGLIGENCE OR OTHERWISE ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*/

use crate::{classifier, config, prompt};
use std::path::Path;

const TEST_MAIL: &[u8] = b"From: slac-test@example.invalid
To: local@example.invalid
Subject: SLAC classifier probe

This is a SLAC classifier connectivity test message.
";

/// Runs a live classifier probe using the same prompt and response parser as
/// MDA mode, but never delivers or quarantines mail.
pub fn run(config_path: Option<&Path>) -> Result<(), String> {
    let (config, loaded_path) = config::load(config_path)?;

    match loaded_path {
        Some(path) => eprintln!("slac test: loaded config from {}", path.display()),
        None => eprintln!("slac test: using built-in default config"),
    }
    eprintln!("slac test: endpoint = {}", config.llm.endpoint);
    eprintln!("slac test: model = {}", config.llm.model);
    eprintln!(
        "slac test: timeout_seconds = {}",
        config.llm.timeout_seconds
    );
    eprintln!(
        "slac test: json_response_format = {}",
        config.llm.json_response_format
    );

    let probe_prompt = prompt::build_prompt_with_corrections(TEST_MAIL, &config, &[]);
    eprintln!("slac test: sending classifier probe");

    match classifier::classify(&probe_prompt, &config.llm) {
        Ok(classification) => {
            let reasons = if classification.reasons.is_empty() {
                "none".to_string()
            } else {
                classification.reasons.join("; ")
            };
            eprintln!(
                "slac test: classifier ok: verdict={} probability={:.3} reasons={}",
                classification.verdict.as_str(),
                classification.spam_probability,
                reasons
            );
            Ok(())
        }
        Err(err) => {
            for hint in classifier_failure_hints(&config.llm.endpoint, &err) {
                eprintln!("slac test: hint: {hint}");
            }
            Err(err)
        }
    }
}

fn classifier_failure_hints(endpoint: &str, error: &str) -> Vec<String> {
    if !is_404_error(error) {
        return Vec::new();
    }

    let trimmed = endpoint.trim_end_matches('/');
    let mut hints = vec![
        format!("configured endpoint is {endpoint}"),
        "SLAC posts to the endpoint exactly as configured".to_string(),
    ];

    if trimmed.ends_with("/v1") {
        hints.push(
            "this looks like an OpenAI API base URL, not the chat completions route"
                .to_string(),
        );
        hints.push(format!("try endpoint = \"{trimmed}/chat/completions\""));
    } else if trimmed.ends_with("/chat/completions") {
        let base = trimmed.trim_end_matches("/chat/completions");
        hints.push(
            "the endpoint already includes /chat/completions; verify that this server exposes that route"
                .to_string(),
        );
        hints.push(format!("the apparent API base is {base}"));
    } else {
        hints.push(
            "verify that the endpoint is the full OpenAI-compatible chat completions URL"
                .to_string(),
        );
        hints.push("common form: http://host:port/v1/chat/completions".to_string());
    }

    hints
}

fn is_404_error(error: &str) -> bool {
    error.contains("status code 404")
        || error.contains("status 404")
        || error.contains(" 404")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_for_api_base_404() {
        let hints = classifier_failure_hints(
            "http://example.com:8888/v1",
            "classifier request failed: http://example.com:8888/v1: status code 404",
        )
        .join("\n");

        assert!(hints.contains("configured endpoint is http://example.com:8888/v1"));
        assert!(hints.contains("API base URL"));
        assert!(hints.contains("http://example.com:8888/v1/chat/completions"));
    }

    #[test]
    fn hints_for_chat_completions_404() {
        let hints = classifier_failure_hints(
            "http://example.com:8888/v1/chat/completions",
            "classifier request failed: http://example.com:8888/v1/chat/completions: status code 404",
        )
        .join("\n");

        assert!(hints.contains("already includes /chat/completions"));
        assert!(hints.contains("apparent API base is http://example.com:8888/v1"));
    }

    #[test]
    fn no_hint_for_non_404() {
        let hints = classifier_failure_hints(
            "http://example.com:8888/v1",
            "classifier request failed: transport error",
        );

        assert!(hints.is_empty());
    }
}
