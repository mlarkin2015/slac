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

use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct CorrectionExample {
    #[serde(default)]
    pub timestamp_unix: u64,
    #[serde(default)]
    pub from_mailbox: String,
    #[serde(default)]
    pub to_mailbox: String,
    #[serde(default)]
    pub corrected_verdict: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub original_slac_verdict: String,
    #[serde(default)]
    pub original_slac_probability: String,
    #[serde(default)]
    pub original_slac_action: String,
    #[serde(default)]
    pub reason: String,
}

/// Loads recent correction records for prompt feedback.
///
/// Reads `~/.local/share/slac/corrections.jsonl` newest-first, then restores
/// chronological order for the returned slice. `max_examples` and `max_bytes`
/// are hard caps for prompt safety. Missing history is not an error.
pub fn load_recent(max_examples: usize, max_bytes: usize) -> Result<Vec<CorrectionExample>, String> {
    if max_examples == 0 || max_bytes == 0 {
        return Ok(Vec::new());
    }

    let path = correction_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(format!(
                "failed to read correction history {}: {err}",
                path.display()
            ))
        }
    };

    let mut examples = Vec::new();
    let mut used_bytes = 0usize;
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if examples.len() >= max_examples || used_bytes >= max_bytes {
            break;
        }

        let example: CorrectionExample = serde_json::from_str(line)
            .map_err(|err| format!("failed to parse correction history line: {err}"))?;
        used_bytes += line.len();
        examples.push(example);
    }

    examples.reverse();
    Ok(examples)
}

fn correction_path() -> Result<PathBuf, String> {
    let home = env::var("HOME")
        .map_err(|_| "HOME must be set to read correction history".to_string())?;
    Ok(PathBuf::from(home).join(".local/share/slac/corrections.jsonl"))
}

/// Collapses whitespace and bounds a correction field before inserting it into
/// the prompt. Assumes display fidelity is less important than prompt budget.
pub fn compact_value(value: &str, max_chars: usize) -> String {
    let mut compacted = String::new();
    let mut last_space = false;
    for ch in value.chars().take(max_chars) {
        if ch.is_whitespace() {
            if !last_space {
                compacted.push(' ');
            }
            last_space = true;
        } else {
            compacted.push(ch);
            last_space = false;
        }
    }
    compacted.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compacts_whitespace_and_bounds_length() {
        assert_eq!(compact_value("one\n  two\tthree", 99), "one two three");
        assert_eq!(compact_value("abcdef", 3), "abc");
    }

    #[test]
    fn parses_current_correction_record_shape() {
        let parsed: CorrectionExample = serde_json::from_str(
            r#"{"timestamp_unix":1,"from_mailbox":"inbox","to_mailbox":"spam","corrected_verdict":"spam","from":"a@example.com","subject":"Bad","original_slac_verdict":"ham","original_slac_probability":"0.120","original_slac_action":"deliver","reason":"missed phishing"}"#,
        )
        .unwrap();
        assert_eq!(parsed.corrected_verdict, "spam");
        assert_eq!(parsed.original_slac_verdict, "ham");
    }
}
