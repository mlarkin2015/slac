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

use crate::classifier::{self, Classification, Verdict};
use crate::config::{self, QuarantineVerdictPolicy};
use crate::corrections;
use crate::delivery;
use crate::log::Logger;
use crate::mail_headers;
use crate::mbox;
use crate::prompt;
use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MailAction {
    Deliver,
    Quarantine,
}

impl MailAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deliver => "deliver",
            Self::Quarantine => "quarantine",
        }
    }
}

pub fn run(config_path: Option<&Path>, logger: &Logger) -> Result<(), String> {
    let config = match config::load(config_path) {
        Ok((config, loaded_path)) => {
            match loaded_path {
                Some(path) => logger.debug(&format!("loaded config from {}", path.display())),
                None => logger.debug("using built-in default config"),
            }
            config
        }
        Err(err) => {
            logger.err(&format!(
                "config load failed; using built-in defaults and delivering anyway: {err}"
            ));
            config::Config::default()
        }
    };

    let mut raw_mail = Vec::new();
    std::io::stdin()
        .read_to_end(&mut raw_mail)
        .map_err(|err| format!("failed to read message from stdin: {err}"))?;
    logger.debug(&format!("read {} bytes from stdin", raw_mail.len()));

    let correction_examples = if config.feedback.enabled {
        match corrections::load_recent(config.feedback.max_examples, config.feedback.max_bytes) {
            Ok(examples) => {
                if !examples.is_empty() {
                    logger.debug(&format!(
                        "loaded {} correction feedback examples",
                        examples.len()
                    ));
                }
                examples
            }
            Err(err) => {
                logger.err(&format!(
                    "failed to load correction feedback; classifying without it: {err}"
                ));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let mut action = MailAction::Deliver;
    let mut final_mail = raw_mail.clone();
    let prompt = prompt::build_prompt_with_corrections(&raw_mail, &config, &correction_examples);
    match classifier::classify(&prompt, &config.llm) {
        Ok(classification) => {
            action = decide_action(&classification, &config);
            let reasons = if classification.reasons.is_empty() {
                "none".to_string()
            } else {
                classification.reasons.join("; ")
            };
            logger.info(&format!(
                "classification verdict={} probability={:.3} threshold={:.3} observe_only={} action={} reasons={}",
                classification.verdict.as_str(),
                classification.spam_probability,
                config.classification.spam_threshold,
                config.classification.observe_only,
                action.as_str(),
                reasons
            ));
            if config.classification.add_headers {
                final_mail = mail_headers::with_classification_headers(
                    &raw_mail,
                    &classification,
                    config.classification.spam_threshold,
                    action.as_str(),
                );
            }
        }
        Err(err) => {
            logger.err(&format!("classification failed; delivering anyway: {err}"));
            if config.classification.add_headers {
                final_mail = mail_headers::with_error_headers(&raw_mail, &err);
            }
        }
    }

    if action == MailAction::Quarantine {
        let path = PathBuf::from(delivery::expand_template(&config.quarantine.path));
        let sender = env::var("SENDER").unwrap_or_default();
        match mbox::append_message(&path, &final_mail, &sender) {
            Ok(()) => {
                logger.info(&format!("quarantined message to {}", path.display()));
                return Ok(());
            }
            Err(err) => {
                logger.err(&format!(
                    "quarantine failed; delivering normally instead: {err}"
                ));
            }
        }
    }

    let status = delivery::deliver(&final_mail, &config.delivery)?;
    if status == 0 {
        logger.info("delivery command succeeded");
        Ok(())
    } else {
        Err(format!("delivery command exited with status {status}"))
    }
}

fn decide_action(classification: &Classification, config: &config::Config) -> MailAction {
    if config.classification.observe_only {
        return MailAction::Deliver;
    }

    if classification.spam_probability < config.classification.spam_threshold {
        return MailAction::Deliver;
    }

    if verdict_matches_policy(&classification.verdict, &config.quarantine.require_verdict) {
        MailAction::Quarantine
    } else {
        MailAction::Deliver
    }
}

fn verdict_matches_policy(verdict: &Verdict, policy: &QuarantineVerdictPolicy) -> bool {
    match policy {
        QuarantineVerdictPolicy::Spam => *verdict == Verdict::Spam,
        QuarantineVerdictPolicy::SpamOrUnsure => {
            *verdict == Verdict::Spam || *verdict == Verdict::Unsure
        }
        QuarantineVerdictPolicy::Any => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn classification(probability: f32, verdict: Verdict) -> Classification {
        Classification {
            spam_probability: probability,
            verdict,
            reasons: Vec::new(),
        }
    }

    #[test]
    fn observe_mode_always_delivers() {
        let mut config = Config::default();
        config.classification.observe_only = true;
        assert_eq!(
            decide_action(&classification(1.0, Verdict::Spam), &config),
            MailAction::Deliver
        );
    }

    #[test]
    fn quarantines_spam_above_threshold_when_enabled() {
        let mut config = Config::default();
        config.classification.observe_only = false;
        config.classification.spam_threshold = 0.95;
        assert_eq!(
            decide_action(&classification(0.99, Verdict::Spam), &config),
            MailAction::Quarantine
        );
    }

    #[test]
    fn does_not_quarantine_ham_above_threshold_with_spam_policy() {
        let mut config = Config::default();
        config.classification.observe_only = false;
        assert_eq!(
            decide_action(&classification(0.99, Verdict::Ham), &config),
            MailAction::Deliver
        );
    }
}
