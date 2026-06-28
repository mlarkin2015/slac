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

use crate::classifier::Classification;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn with_classification_headers(
    raw_mail: &[u8],
    classification: &Classification,
    threshold: f32,
    action: &str,
) -> Vec<u8> {
    let reasons = if classification.reasons.is_empty() {
        "none".to_string()
    } else {
        classification.reasons.join(" | ")
    };

    let headers = vec![
        ("X-SLAC-Status", "classified".to_string()),
        ("X-SLAC-Action", action.to_string()),
        ("X-SLAC-Verdict", classification.verdict.as_str().to_string()),
        (
            "X-SLAC-Spam-Probability",
            format!("{:.3}", classification.spam_probability),
        ),
        ("X-SLAC-Spam-Threshold", format!("{threshold:.3}")),
        ("X-SLAC-Reasons", reasons),
    ];

    insert_headers(raw_mail, &headers)
}

/// Adds SLAC classifier failure headers while preserving the original message
/// body and existing headers. Used only for fail-open delivery paths.
pub fn with_error_headers(raw_mail: &[u8], error: &str) -> Vec<u8> {
    insert_headers(
        raw_mail,
        &[
            ("X-SLAC-Status", "classifier-error".to_string()),
            ("X-SLAC-Action", "deliver".to_string()),
            ("X-SLAC-Error", error.to_string()),
        ],
    )
}

/// Adds user-correction metadata to a moved message without modifying the
/// original receipt-time `X-SLAC-*` classification headers.
pub fn with_correction_headers(raw_mail: &[u8], corrected_verdict: &str, reason: &str) -> Vec<u8> {
    insert_headers(
        raw_mail,
        &[
            ("X-SLAC-User-Correction", corrected_verdict.to_string()),
            (
                "X-SLAC-Correction-Time",
                unix_timestamp().to_string(),
            ),
            ("X-SLAC-Correction-Reason", reason.to_string()),
        ],
    )
}

/// Returns a decoded header value, including simple folded continuation lines.
/// This is intentionally lightweight and assumes mbox messages are already
/// separated before parsing.
pub fn header_value(raw_mail: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(raw_mail);
    let header_text = text
        .split_once("\r\n\r\n")
        .map(|(headers, _)| headers)
        .or_else(|| text.split_once("\n\n").map(|(headers, _)| headers))
        .unwrap_or(&text);
    let target = format!("{}:", name.to_ascii_lowercase());
    let mut current_name = String::new();
    let mut current_value = String::new();

    for line in header_text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if !current_value.is_empty() {
                current_value.push(' ');
                current_value.push_str(line.trim());
            }
            continue;
        }

        if !current_name.is_empty() && current_name == target {
            return Some(current_value.trim().to_string());
        }

        let Some((line_name, line_value)) = line.split_once(':') else {
            current_name.clear();
            current_value.clear();
            continue;
        };
        current_name = format!("{}:", line_name.to_ascii_lowercase());
        current_value = line_value.trim().to_string();
    }

    if current_name == target {
        Some(current_value.trim().to_string())
    } else {
        None
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Inserts headers immediately before the message body separator.
///
/// Preserves CRLF vs LF style when a separator exists. If the input has no
/// header/body separator, the new headers are prepended and followed by a blank
/// line so the result is still a valid RFC-style message.
fn insert_headers(raw_mail: &[u8], headers: &[(&str, String)]) -> Vec<u8> {
    let separator = find_header_separator(raw_mail);
    let line_ending = match separator {
        Some((_, separator_len)) if separator_len == 4 => "\r\n",
        _ => "\n",
    };
    let header_block = render_headers(headers, line_ending);

    match separator {
        Some((index, separator_len)) => {
            let mut annotated = Vec::with_capacity(raw_mail.len() + header_block.len());
            annotated.extend_from_slice(&raw_mail[..index]);
            annotated.extend_from_slice(line_ending.as_bytes());
            annotated.extend_from_slice(header_block.as_bytes());
            annotated.extend_from_slice(line_ending.as_bytes());
            annotated.extend_from_slice(&raw_mail[index + separator_len..]);
            annotated
        }
        None => {
            let mut annotated = Vec::with_capacity(raw_mail.len() + header_block.len() + line_ending.len());
            annotated.extend_from_slice(header_block.as_bytes());
            annotated.extend_from_slice(line_ending.as_bytes());
            annotated.extend_from_slice(raw_mail);
            annotated
        }
    }
}

fn render_headers(headers: &[(&str, String)], line_ending: &str) -> String {
    let mut rendered = String::new();
    for (name, value) in headers {
        rendered.push_str(name);
        rendered.push_str(": ");
        rendered.push_str(&sanitize_header_value(value));
        rendered.push_str(line_ending);
    }
    rendered
}

fn sanitize_header_value(value: &str) -> String {
    let mut sanitized = String::new();
    let mut last_was_space = false;

    for ch in value.chars().take(900) {
        let replacement = if ch == '\r' || ch == '\n' || ch == '\t' || ch.is_control() {
            ' '
        } else {
            ch
        };

        if replacement == ' ' {
            if !last_was_space {
                sanitized.push(' ');
            }
            last_was_space = true;
        } else {
            sanitized.push(replacement);
            last_was_space = false;
        }
    }

    sanitized.trim().to_string()
}

fn find_header_separator(raw_mail: &[u8]) -> Option<(usize, usize)> {
    find_subslice(raw_mail, b"\r\n\r\n")
        .map(|index| (index, 4))
        .or_else(|| find_subslice(raw_mail, b"\n\n").map(|index| (index, 2)))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::{Classification, Verdict};

    #[test]
    fn inserts_headers_before_lf_body_separator() {
        let classification = Classification {
            spam_probability: 0.125,
            verdict: Verdict::Ham,
            reasons: vec!["mailing list".to_string()],
        };
        let annotated =
            with_classification_headers(b"Subject: hi\n\nbody\n", &classification, 0.85, "deliver");
        let text = String::from_utf8(annotated).unwrap();
        assert!(text.starts_with("Subject: hi\nX-SLAC-Status: classified\n"));
        assert!(text.contains("X-SLAC-Action: deliver\n"));
        assert!(text.contains("X-SLAC-Verdict: ham\n"));
        assert!(text.contains("\n\nbody\n"));
        assert!(!text.contains("\n\n\nbody\n"));
    }

    #[test]
    fn preserves_crlf_separator() {
        let annotated = with_error_headers(b"Subject: hi\r\n\r\nbody\r\n", "bad\nthing");
        let text = String::from_utf8(annotated).unwrap();
        assert!(text.starts_with("Subject: hi\r\nX-SLAC-Status: classifier-error\r\n"));
        assert!(text.contains("X-SLAC-Action: deliver\r\n"));
        assert!(text.contains("X-SLAC-Error: bad thing\r\n"));
        assert!(text.contains("\r\n\r\nbody\r\n"));
        assert!(!text.contains("\r\n\r\n\r\nbody\r\n"));
    }

    #[test]
    fn adds_correction_headers_without_replacing_original_classification() {
        let annotated = with_correction_headers(
            b"Subject: hi\nX-SLAC-Verdict: ham\n\nbody\n",
            "spam",
            "missed phishing\nlanguage",
        );
        let text = String::from_utf8(annotated).unwrap();
        assert!(text.contains("X-SLAC-Verdict: ham\n"));
        assert!(text.contains("X-SLAC-User-Correction: spam\n"));
        assert!(text.contains("X-SLAC-Correction-Reason: missed phishing language\n"));
    }
}
