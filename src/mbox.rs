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

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

unsafe extern "C" {
    fn ctime_r(clock: *const libc::time_t, buf: *mut libc::c_char) -> *mut libc::c_char;
}

/// Appends one RFC message payload to an mbox file with exclusive `flock`
/// locking.
///
/// Preconditions: `raw_mail` must not include an mbox `From ` separator.
/// Postconditions: the message is written with a generated separator, escaped
/// body lines that begin with `From `, and a final blank line. Parent
/// directories are created when needed.
pub fn append_message(path: &Path, raw_mail: &[u8], sender: &str) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create quarantine directory {}: {err}", parent.display()))?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| format!("failed to open quarantine mbox {}: {err}", path.display()))?;

    lock_exclusive(&file)?;
    let formatted = format_mbox_message(raw_mail, sender, &ctime_now());
    let write_result = file
        .write_all(&formatted)
        .map_err(|err| format!("failed to append quarantine mbox {}: {err}", path.display()));
    let unlock_result = unlock(&file);

    write_result?;
    unlock_result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSummary {
    pub id: usize,
    pub offset: usize,
    pub length: usize,
    pub envelope_from: String,
    pub from: String,
    pub subject: String,
    pub date: String,
    pub slac_verdict: String,
    pub slac_probability: String,
    pub slac_action: String,
}

#[derive(Debug, Clone)]
struct MessageSpan {
    offset: usize,
    length: usize,
    envelope_from: String,
    raw: Vec<u8>,
}

/// Scans an mbox file and returns lightweight summaries in file order.
///
/// Missing mbox files are treated as empty mailboxes. Message ids are scan
/// indexes and are only stable until the mailbox is rewritten.
pub fn scan(path: &Path) -> Result<Vec<MessageSummary>, String> {
    let bytes = read_existing(path)?;
    Ok(parse_spans(&bytes)
        .into_iter()
        .enumerate()
        .map(|(id, span)| summarize_span(id, &span))
        .collect())
}

/// Reads one message payload by current scan index.
///
/// The returned bytes exclude the mbox separator line. Callers should refresh
/// indexes after any move or external mailbox write.
pub fn read_message(path: &Path, id: usize) -> Result<Vec<u8>, String> {
    let bytes = read_existing(path)?;
    let spans = parse_spans(&bytes);
    spans
        .get(id)
        .map(|span| span.raw.clone())
        .ok_or_else(|| format!("message id {id} not found in {}", path.display()))
}

pub struct MovedMessage {
    pub summary: MessageSummary,
    pub raw: Vec<u8>,
}

/// Moves one message from `source` to `destination`, transforming only the
/// destination copy.
///
/// Preconditions: `id` is a current scan index for `source`. The source mbox is
/// locked while selecting, appending, and rewriting. The destination append
/// takes its own lock. On success, the selected message no longer exists in the
/// source mailbox.
pub fn move_message_transform<F>(
    source: &Path,
    id: usize,
    destination: &Path,
    transform: F,
) -> Result<MovedMessage, String>
where
    F: FnOnce(&[u8], &MessageSummary) -> Vec<u8>,
{
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(source)
        .map_err(|err| format!("failed to open source mbox {}: {err}", source.display()))?;

    lock_exclusive(&file)?;
    let result = move_message_locked(&mut file, source, id, destination, transform);
    let unlock_result = unlock(&file);

    let moved = result?;
    unlock_result?;
    Ok(moved)
}

/// Performs the source rewrite while the source mailbox lock is held.
///
/// The transform closure lets review/TUI code add correction headers to the
/// destination copy without changing the bytes used to identify the source span.
fn move_message_locked<F>(
    file: &mut File,
    source: &Path,
    id: usize,
    destination: &Path,
    transform: F,
) -> Result<MovedMessage, String>
where
    F: FnOnce(&[u8], &MessageSummary) -> Vec<u8>,
{
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))
        .map_err(|err| format!("failed to seek source mbox {}: {err}", source.display()))?;
    file.read_to_end(&mut bytes)
        .map_err(|err| format!("failed to read source mbox {}: {err}", source.display()))?;

    let spans = parse_spans(&bytes);
    let span = spans
        .get(id)
        .ok_or_else(|| format!("message id {id} not found in {}", source.display()))?;
    let summary = summarize_span(id, span);
    let destination_raw = transform(&span.raw, &summary);
    append_message(destination, &destination_raw, &span.envelope_from)?;

    let mut rewritten = Vec::with_capacity(bytes.len().saturating_sub(span.length));
    rewritten.extend_from_slice(&bytes[..span.offset]);
    rewritten.extend_from_slice(&bytes[span.offset + span.length..]);

    file.set_len(0)
        .map_err(|err| format!("failed to truncate source mbox {}: {err}", source.display()))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|err| format!("failed to seek source mbox {}: {err}", source.display()))?;
    file.write_all(&rewritten)
        .map_err(|err| format!("failed to rewrite source mbox {}: {err}", source.display()))?;

    Ok(MovedMessage {
        summary,
        raw: destination_raw,
    })
}

fn read_existing(path: &Path) -> Result<Vec<u8>, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(format!("failed to read mbox {}: {err}", path.display())),
    }
}

/// Parses raw mbox bytes into message spans and RFC message payloads.
///
/// Assumes mbox-style escaping has protected body lines that would otherwise
/// look like separator lines.
fn parse_spans(bytes: &[u8]) -> Vec<MessageSpan> {
    let mut delimiter_offsets = Vec::new();
    let mut offset = 0usize;

    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if offset == 0 {
            if line.starts_with(b"From ") {
                delimiter_offsets.push(offset);
            }
        } else if line.starts_with(b"From ") {
            delimiter_offsets.push(offset);
        }
        offset += line.len();
    }

    let mut spans = Vec::new();
    for (index, start) in delimiter_offsets.iter().copied().enumerate() {
        let end = delimiter_offsets
            .get(index + 1)
            .copied()
            .unwrap_or(bytes.len());
        let delimiter_end = bytes[start..end]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|relative| start + relative + 1)
            .unwrap_or(end);
        let envelope_from = parse_envelope_from(&bytes[start..delimiter_end]);
        spans.push(MessageSpan {
            offset: start,
            length: end - start,
            envelope_from,
            raw: bytes[delimiter_end..end].to_vec(),
        });
    }

    spans
}

fn summarize_span(id: usize, span: &MessageSpan) -> MessageSummary {
    MessageSummary {
        id,
        offset: span.offset,
        length: span.length,
        envelope_from: span.envelope_from.clone(),
        from: header_value(&span.raw, "From").unwrap_or_default(),
        subject: header_value(&span.raw, "Subject").unwrap_or_default(),
        date: header_value(&span.raw, "Date").unwrap_or_default(),
        slac_verdict: header_value(&span.raw, "X-SLAC-Verdict").unwrap_or_default(),
        slac_probability: header_value(&span.raw, "X-SLAC-Spam-Probability").unwrap_or_default(),
        slac_action: header_value(&span.raw, "X-SLAC-Action").unwrap_or_default(),
    }
}

fn header_value(raw_mail: &[u8], name: &str) -> Option<String> {
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

fn parse_envelope_from(delimiter_line: &[u8]) -> String {
    let line = String::from_utf8_lossy(delimiter_line);
    line.trim_end()
        .strip_prefix("From ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("MAILER-DAEMON")
        .to_string()
}

fn lock_exclusive(file: &File) -> Result<(), String> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("failed to lock quarantine mbox: {}", std::io::Error::last_os_error()))
    }
}

fn unlock(file: &File) -> Result<(), String> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("failed to unlock quarantine mbox: {}", std::io::Error::last_os_error()))
    }
}

fn format_mbox_message(raw_mail: &[u8], sender: &str, ctime: &str) -> Vec<u8> {
    let safe_sender = sanitize_sender(sender);
    let mut formatted = Vec::with_capacity(raw_mail.len() + 128);
    formatted.extend_from_slice(format!("From {safe_sender} {ctime}\n").as_bytes());

    for line in raw_mail.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"From ") {
            formatted.push(b'>');
        }
        formatted.extend_from_slice(line);
    }

    if !raw_mail.ends_with(b"\n") {
        formatted.push(b'\n');
    }
    formatted.push(b'\n');
    formatted
}

fn sanitize_sender(sender: &str) -> String {
    let trimmed = sender.trim();
    if trimmed.is_empty() {
        return "MAILER-DAEMON".to_string();
    }

    trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_graphic() && !ch.is_ascii_whitespace() {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn ctime_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as libc::time_t)
        .unwrap_or(0);
    ctime(seconds).unwrap_or_else(|| seconds.to_string())
}

fn ctime(seconds: libc::time_t) -> Option<String> {
    let mut timestamp = seconds;
    let mut buffer = [0 as libc::c_char; 32];
    let ptr = unsafe { ctime_r(&mut timestamp, buffer.as_mut_ptr()) };
    if ptr.is_null() {
        return None;
    }

    let text = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .trim_end()
        .to_string();
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    fn temp_path(name: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!("slac-test-{}-{}", std::process::id(), name))
    }

    #[test]
    fn formats_mbox_message_and_escapes_from_lines() {
        let formatted = format_mbox_message(
            b"Subject: hi\n\nFrom not a separator\nbody",
            "sender@example.com",
            "Wed Jun  3 12:00:00 2026",
        );
        let text = String::from_utf8(formatted).unwrap();
        assert!(text.starts_with("From sender@example.com Wed Jun  3 12:00:00 2026\n"));
        assert!(text.contains("\n>From not a separator\n"));
        assert!(text.ends_with("body\n\n"));
    }

    #[test]
    fn sanitizes_empty_sender() {
        let formatted = format_mbox_message(b"Subject: hi\n\nbody\n", "", "now");
        let text = String::from_utf8(formatted).unwrap();
        assert!(text.starts_with("From MAILER-DAEMON now\n"));
    }

    #[test]
    fn scans_mbox_summaries() {
        let path = temp_path("scan.mbox");
        let content = [
            format_mbox_message(
                b"From: sender@example.com\nSubject: First\nX-SLAC-Verdict: ham\nX-SLAC-Spam-Probability: 0.010\nX-SLAC-Action: deliver\n\nbody\n",
                "sender@example.com",
                "Wed Jun  3 12:00:00 2026",
            ),
            format_mbox_message(
                b"From: spammer@example.net\nSubject: Second\nX-SLAC-Verdict: spam\nX-SLAC-Spam-Probability: 0.990\nX-SLAC-Action: quarantine\n\nbody\n",
                "spammer@example.net",
                "Wed Jun  3 12:01:00 2026",
            ),
        ]
        .concat();
        fs::write(&path, content).unwrap();

        let summaries = scan(&path).unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].subject, "First");
        assert_eq!(summaries[1].slac_verdict, "spam");
        assert_eq!(summaries[1].slac_action, "quarantine");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn moves_one_message_between_mboxes() {
        let source = temp_path("source.mbox");
        let destination = temp_path("destination.mbox");
        let source_content = [
            format_mbox_message(
                b"From: first@example.com\nSubject: First\n\nbody\n",
                "first@example.com",
                "Wed Jun  3 12:00:00 2026",
            ),
            format_mbox_message(
                b"From: second@example.com\nSubject: Second\n\nbody\n",
                "second@example.com",
                "Wed Jun  3 12:01:00 2026",
            ),
        ]
        .concat();
        fs::write(&source, source_content).unwrap();
        let _ = fs::remove_file(&destination);

        let summary =
            move_message_transform(&source, 0, &destination, |raw, _summary| raw.to_vec())
                .unwrap()
                .summary;
        assert_eq!(summary.subject, "First");
        assert_eq!(scan(&source).unwrap()[0].subject, "Second");
        assert_eq!(scan(&destination).unwrap()[0].subject, "First");

        let _ = fs::remove_file(source);
        let _ = fs::remove_file(destination);
    }

    #[test]
    fn move_transform_changes_destination_copy_only() {
        let source = temp_path("source-transform.mbox");
        let destination = temp_path("destination-transform.mbox");
        let source_content = format_mbox_message(
            b"From: first@example.com\nSubject: First\n\nbody\n",
            "first@example.com",
            "Wed Jun  3 12:00:00 2026",
        );
        fs::write(&source, source_content).unwrap();
        let _ = fs::remove_file(&destination);

        let moved = move_message_transform(&source, 0, &destination, |raw, _summary| {
            let mut transformed = b"X-Test: yes\n".to_vec();
            transformed.extend_from_slice(raw);
            transformed
        })
        .unwrap();
        assert!(String::from_utf8(moved.raw).unwrap().starts_with("X-Test: yes\n"));
        assert!(read_message(&destination, 0)
            .unwrap()
            .starts_with(b"X-Test: yes\n"));
        assert!(scan(&source).unwrap().is_empty());

        let _ = fs::remove_file(source);
        let _ = fs::remove_file(destination);
    }
}
