use crate::config::{self, Config};
use crate::delivery;
use crate::mail_headers;
use crate::mbox::{self, MessageSummary};
use ring::digest;
use serde::Serialize;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxKind {
    Inbox,
    Spam,
}

#[derive(Debug, Serialize)]
struct CorrectionRecord<'a> {
    timestamp_unix: u64,
    from_mailbox: &'a str,
    to_mailbox: &'a str,
    corrected_verdict: &'a str,
    source_path: String,
    destination_path: String,
    message_id: usize,
    message_sha256: &'a str,
    message_path: String,
    envelope_from: &'a str,
    from: &'a str,
    subject: &'a str,
    date: &'a str,
    original_slac_verdict: &'a str,
    original_slac_probability: &'a str,
    original_slac_action: &'a str,
    corrected_header: String,
    reason: &'a str,
}

/// CLI implementation for listing one mailbox as tab-separated summaries.
///
/// `mailbox` must be `inbox` or `spam`. The printed ids are scan indexes and
/// should be used immediately with `show`/`move`.
pub fn list(config_path: Option<&Path>, mailbox: Option<&str>) -> Result<(), String> {
    let config = load_config(config_path)?;
    let mailbox = parse_mailbox(mailbox)?;
    let summaries = summaries(&config, mailbox)?;

    println!("mailbox\tid\tprob\tverdict\taction\tdate\tfrom\tsubject");
    for summary in summaries {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            mailbox.as_str(),
            summary.id,
            summary.slac_probability,
            summary.slac_verdict,
            summary.slac_action,
            compact(&summary.date),
            compact(&summary.from),
            compact(&summary.subject),
        );
    }

    Ok(())
}

/// CLI implementation for writing one selected message payload to stdout.
///
/// Preconditions: `id` is a current scan index for the chosen mailbox.
pub fn show(config_path: Option<&Path>, mailbox: Option<&str>, id: Option<usize>) -> Result<(), String> {
    let config = load_config(config_path)?;
    let mailbox = parse_mailbox(mailbox)?;
    let id = id.ok_or("show requires --id N")?;
    let message = read(&config, mailbox, id)?;
    std::io::stdout()
        .write_all(&message)
        .map_err(|err| format!("failed to write message to stdout: {err}"))?;
    Ok(())
}

pub fn move_message(
    config_path: Option<&Path>,
    mailbox: Option<&str>,
    id: Option<usize>,
    to_mailbox: Option<&str>,
    reason: Option<&str>,
) -> Result<(), String> {
    let config = load_config(config_path)?;
    let from = parse_mailbox(mailbox)?;
    let to = parse_mailbox(to_mailbox)?;
    let id = id.ok_or("move requires --id N")?;
    let reason = reason.unwrap_or("").trim();
    let moved = move_between(&config, from, id, to, reason)?;

    println!(
        "moved {} id={} to {}: {}",
        from.as_str(),
        id,
        to.as_str(),
        moved.subject
    );
    Ok(())
}

/// Loads configuration for review/TUI operations.
///
/// Unlike MDA mode, review commands return config errors directly because they
/// are user-invoked tools.
pub fn load_config(config_path: Option<&Path>) -> Result<Config, String> {
    config::load(config_path).map(|(config, _)| config)
}

/// Returns message summaries for a logical mailbox using configured paths.
pub fn summaries(config: &Config, mailbox: MailboxKind) -> Result<Vec<MessageSummary>, String> {
    let path = mailbox_path(mailbox, config)?;
    mbox::scan(&path)
}

/// Reads one message payload from a logical mailbox by current scan index.
pub fn read(config: &Config, mailbox: MailboxKind, id: usize) -> Result<Vec<u8>, String> {
    let path = mailbox_path(mailbox, config)?;
    mbox::read_message(&path, id)
}

/// Moves one message between logical mailboxes and records correction data.
///
/// Postconditions: the destination copy has user-correction headers, the
/// original receipt-time `X-SLAC-*` headers are preserved, correction metadata
/// is appended to JSONL, and a corrected `.eml` snapshot is stored by SHA-256.
pub fn move_between(
    config: &Config,
    from: MailboxKind,
    id: usize,
    to: MailboxKind,
    reason: &str,
) -> Result<MessageSummary, String> {
    if from == to {
        return Err("source and destination mailboxes must differ".to_string());
    }

    let source_path = mailbox_path(from, config)?;
    let destination_path = mailbox_path(to, config)?;
    let corrected_verdict = corrected_verdict(to);
    let moved = mbox::move_message_transform(&source_path, id, &destination_path, |raw, _summary| {
        mail_headers::with_correction_headers(raw, corrected_verdict, reason)
    })?;
    append_correction(
        from,
        to,
        &source_path,
        &destination_path,
        &moved.summary,
        &moved.raw,
        corrected_verdict,
        reason,
    )?;
    Ok(moved.summary)
}

/// Parses a CLI mailbox selector. Only `inbox` and `spam` are valid.
pub fn parse_mailbox(value: Option<&str>) -> Result<MailboxKind, String> {
    match value {
        Some("inbox") => Ok(MailboxKind::Inbox),
        Some("spam") => Ok(MailboxKind::Spam),
        Some(other) => Err(format!("unknown mailbox {other}; expected inbox or spam")),
        None => Err("--mailbox requires inbox or spam".to_string()),
    }
}

fn mailbox_path(mailbox: MailboxKind, config: &Config) -> Result<PathBuf, String> {
    match mailbox {
        MailboxKind::Inbox => Ok(PathBuf::from(format!("/var/mail/{}", username()?))),
        MailboxKind::Spam => Ok(PathBuf::from(delivery::expand_template(&config.quarantine.path))),
    }
}

fn username() -> Result<String, String> {
    env::var("USER")
        .or_else(|_| env::var("LOGNAME"))
        .map_err(|_| "USER or LOGNAME must be set to resolve inbox path".to_string())
}

fn correction_root() -> Result<PathBuf, String> {
    let home = env::var("HOME").map_err(|_| "HOME must be set to write correction history".to_string())?;
    Ok(PathBuf::from(home).join(".local/share/slac"))
}

fn correction_path() -> Result<PathBuf, String> {
    Ok(correction_root()?.join("corrections.jsonl"))
}

fn correction_message_path(message_sha256: &str) -> Result<PathBuf, String> {
    Ok(correction_root()?
        .join("corrections/messages")
        .join(format!("{message_sha256}.eml")))
}

/// Writes correction metadata and the corrected message snapshot.
///
/// Assumes `raw_message` is the destination copy after correction headers have
/// been added. The JSONL record stays compact and references the full snapshot
/// via SHA-256 for later feedback or fine-tuning workflows.
fn append_correction(
    from: MailboxKind,
    to: MailboxKind,
    source_path: &Path,
    destination_path: &Path,
    summary: &MessageSummary,
    raw_message: &[u8],
    corrected_verdict: &str,
    reason: &str,
) -> Result<(), String> {
    let message_sha256 = sha256_hex(raw_message);
    let message_path = correction_message_path(&message_sha256)?;
    if let Some(parent) = message_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create correction message directory {}: {err}", parent.display()))?;
    }
    fs::write(&message_path, raw_message)
        .map_err(|err| format!("failed to write correction message {}: {err}", message_path.display()))?;

    let path = correction_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create correction directory {}: {err}", parent.display()))?;
    }

    let record = CorrectionRecord {
        timestamp_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
        from_mailbox: from.as_str(),
        to_mailbox: to.as_str(),
        corrected_verdict,
        source_path: source_path.display().to_string(),
        destination_path: destination_path.display().to_string(),
        message_id: summary.id,
        message_sha256: &message_sha256,
        message_path: message_path.display().to_string(),
        envelope_from: &summary.envelope_from,
        from: &summary.from,
        subject: &summary.subject,
        date: &summary.date,
        original_slac_verdict: &summary.slac_verdict,
        original_slac_probability: &summary.slac_probability,
        original_slac_action: &summary.slac_action,
        corrected_header: mail_headers::header_value(raw_message, "X-SLAC-User-Correction")
            .unwrap_or_default(),
        reason,
    };

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("failed to open correction history {}: {err}", path.display()))?;
    serde_json::to_writer(&mut file, &record)
        .map_err(|err| format!("failed to serialize correction history: {err}"))?;
    file.write_all(b"\n")
        .map_err(|err| format!("failed to write correction history {}: {err}", path.display()))
}

fn corrected_verdict(to: MailboxKind) -> &'static str {
    match to {
        MailboxKind::Inbox => "ham",
        MailboxKind::Spam => "spam",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = digest::digest(&digest::SHA256, bytes);
    let mut encoded = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn compact(value: &str) -> String {
    let mut compacted = String::new();
    let mut last_space = false;
    for ch in value.chars().take(180) {
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

impl MailboxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Spam => "spam",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Inbox => Self::Spam,
            Self::Spam => Self::Inbox,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_sha256_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn maps_destination_to_corrected_verdict() {
        assert_eq!(corrected_verdict(MailboxKind::Spam), "spam");
        assert_eq!(corrected_verdict(MailboxKind::Inbox), "ham");
    }
}
