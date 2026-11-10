use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use serde_json::Value;

use super::types::HistoryEntry;

pub(super) const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
pub(super) const PREVIEW_CHARS: usize = 4_000;

pub(super) struct ScanBudget {
    pub bytes: u64,
    pub records: u64,
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self {
            bytes: u64::MAX,
            records: u64::MAX,
        }
    }
}

#[derive(Default, Debug)]
pub(super) struct ScanReport {
    pub malformed: usize,
    pub oversized: usize,
    pub partial: bool,
    pub changed: bool,
    pub limited: bool,
    pub unsupported: bool,
}

impl ScanReport {
    pub fn incomplete(&self) -> bool {
        self.malformed > 0
            || self.oversized > 0
            || self.partial
            || self.changed
            || self.limited
            || self.unsupported
    }

    pub fn diagnostic(&self) -> Option<String> {
        self.incomplete().then(|| format!(
            "Partial history coverage: {} malformed records, {} oversized records; partial trailing record: {}; file changed: {}; scan limit reached: {}; unsupported Pi format: {}",
            self.malformed, self.oversized, self.partial, self.changed, self.limited, self.unsupported,
        ))
    }
}

pub(super) fn bounded_line(
    reader: &mut impl BufRead,
    maximum: usize,
) -> io::Result<Option<(Vec<u8>, bool, bool)>> {
    let mut line = Vec::new();
    let mut oversized = false;
    let mut read_any = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(read_any.then_some((line, oversized, false)));
        }
        read_any = true;
        let end = buffer.iter().position(|&byte| byte == b'\n');
        let count = end.map_or(buffer.len(), |index| index + 1);
        if !oversized {
            if line.len().saturating_add(count) > maximum {
                oversized = true;
                line.clear();
            } else {
                line.extend_from_slice(&buffer[..count]);
            }
        }
        reader.consume(count);
        if end.is_some() {
            return Ok(Some((line, oversized, true)));
        }
    }
}

pub(super) fn scan(
    path: &Path,
    budget: &mut ScanBudget,
    mut visit: impl FnMut(Option<HistoryEntry>) -> io::Result<bool>,
) -> io::Result<ScanReport> {
    let path_metadata = std::fs::metadata(path)?;
    if !path_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Pi history is not a regular file",
        ));
    }
    let file = File::open(path)?;
    let initial = file.metadata()?;
    if !initial.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Pi history is not a regular file",
        ));
    }
    let read_limit = initial.len().min(budget.bytes);
    let record_limit = budget.records;
    budget.bytes -= read_limit;
    budget.records -= record_limit;
    let mut reader = BufReader::new(file.take(read_limit));
    let mut report = ScanReport {
        limited: read_limit < initial.len(),
        ..Default::default()
    };
    let mut line_number = 0;
    let mut stopped = false;
    while line_number < record_limit {
        let Some((line, oversized, terminated)) = bounded_line(&mut reader, MAX_RECORD_BYTES)?
        else {
            break;
        };
        line_number += 1;
        if line_number % 512 == 0 && !visit(None)? {
            stopped = true;
            break;
        }
        if !terminated {
            report.partial = true;
        }
        if oversized {
            report.oversized += 1;
            continue;
        }
        if !terminated || line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&line) {
            Ok(Value::Object(object)) => Value::Object(object),
            Ok(_) => {
                report.malformed += 1;
                continue;
            }
            Err(_) => {
                report.malformed += 1;
                continue;
            }
        };
        if value["type"] == "session" {
            let version = value.get("version").map_or(Some(1), Value::as_u64);
            if !matches!(version, Some(1..=3)) {
                report.unsupported = true;
                break;
            }
        }
        if let Some((role, text)) = readable_entry(&value) {
            if !visit(Some(HistoryEntry {
                entry_line: line_number,
                role,
                text,
                truncated: false,
            }))? {
                stopped = true;
                break;
            }
        }
    }
    if !stopped && line_number == record_limit && !reader.fill_buf()?.is_empty() {
        report.limited = true;
    }
    budget.bytes += reader.get_ref().limit();
    budget.records += record_limit - line_number;
    let file = reader.into_inner().into_inner();
    let final_metadata = file.metadata()?;
    let current_path = std::fs::metadata(path);
    report.changed = initial.len() != final_metadata.len()
        || initial.modified().ok() != final_metadata.modified().ok()
        || current_path.is_err()
        || current_path.is_ok_and(|current| {
            current.len() != initial.len()
                || current.modified().ok() != initial.modified().ok()
                || current.created().ok() != initial.created().ok()
        });
    Ok(report)
}

fn content_text(content: &Value, output: &mut String) {
    match content {
        Value::String(value) => append(output, value),
        Value::Array(blocks) => {
            for block in blocks {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str() {
                            append(output, text);
                        }
                    }
                    Some("thinking") if block["redacted"] != true => {
                        if let Some(text) = block["thinking"].as_str() {
                            append(output, text);
                        }
                    }
                    Some("toolCall") => {
                        if let Some(name) = block["name"].as_str() {
                            append(output, name);
                        }
                        argument_text(&block["arguments"], output);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn section_text(message: &Value, output: &mut String) {
    if let Some(sections) = message["sections"].as_object() {
        for section in sections.values().filter_map(Value::as_str) {
            append(output, section);
        }
    }
}

fn argument_text(value: &Value, output: &mut String) {
    match value {
        Value::String(text) => append(output, text),
        Value::Array(values) => {
            for value in values {
                argument_text(value, output);
            }
        }
        Value::Object(fields)
            if !matches!(
                value["type"].as_str(),
                Some("image" | "image_url" | "input_image" | "redacted_thinking")
            ) && value["redacted"] != true =>
        {
            for (key, value) in fields {
                let key = key.to_ascii_lowercase();
                if !key.contains("signature")
                    && !matches!(
                        key.as_str(),
                        "data"
                            | "state"
                            | "details"
                            | "opaque"
                            | "image"
                            | "images"
                            | "base64"
                            | "bytes"
                            | "buffer"
                            | "mimetype"
                            | "image_url"
                            | "imagedata"
                            | "inline_data"
                            | "inlinedata"
                            | "providerdata"
                            | "provider_data"
                            | "provideroptions"
                            | "provider_options"
                            | "deferred"
                            | "customstate"
                            | "custom_state"
                            | "attachment"
                            | "attachments"
                    )
                {
                    argument_text(value, output);
                }
            }
        }
        _ => {}
    }
}

fn append(output: &mut String, text: &str) {
    if !text.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(text);
    }
}

fn readable_entry(value: &Value) -> Option<(String, String)> {
    let kind = value["type"].as_str()?;
    let mut text = String::new();
    let role = match kind {
        "message" => {
            let message = &value["message"];
            let role = message["role"].as_str()?;
            match role {
                "bashExecution" => {
                    for field in ["command", "output"] {
                        if let Some(value) = message[field].as_str() {
                            append(&mut text, value);
                        }
                    }
                }
                "branchSummary" | "compactionSummary" => {
                    if let Some(value) = message["summary"].as_str() {
                        append(&mut text, value);
                    }
                }
                "user" | "assistant" | "toolResult" | "system" | "custom" => {
                    content_text(&message["content"], &mut text);
                }
                _ => return None,
            }
            if role == "system" {
                section_text(message, &mut text);
            }
            if role == "assistant" {
                if let Some(error) = message["errorMessage"].as_str() {
                    append(&mut text, error);
                }
            }
            sanitize(role, 80)
        }
        "compaction" | "branch_summary" => {
            append(&mut text, value["summary"].as_str()?);
            if kind == "compaction" {
                content_text(&value["systemMessage"]["content"], &mut text);
                section_text(&value["systemMessage"], &mut text);
            }
            kind.to_owned()
        }
        "custom_message" => {
            content_text(&value["content"], &mut text);
            "custom".to_owned()
        }
        "context_edit" => {
            content_text(&value["replacement"]["content"], &mut text);
            kind.to_owned()
        }
        _ => return None,
    };
    let text = sanitize(&text, usize::MAX);
    (!text.is_empty()).then_some((role, text))
}

pub(super) fn sanitize(text: &str, maximum: usize) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    let mut length = 0;
    while let Some(ch) = chars.next() {
        if matches!(
            ch,
            '\u{1b}' | '\u{90}' | '\u{98}' | '\u{9b}' | '\u{9d}' | '\u{9e}' | '\u{9f}'
        ) {
            let control = match ch {
                '\u{1b}' => chars.next(),
                '\u{90}' => Some('P'),
                '\u{98}' => Some('X'),
                '\u{9b}' => Some('['),
                '\u{9d}' => Some(']'),
                '\u{9e}' => Some('^'),
                '\u{9f}' => Some('_'),
                _ => None,
            };
            match control {
                Some('[') => {
                    for ch in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') | Some('P') | Some('_') | Some('^') | Some('X') => {
                    while let Some(ch) = chars.next() {
                        if ch == '\u{9c}' || (control == Some(']') && ch == '\u{7}') {
                            break;
                        }
                        if ch == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if (ch.is_control() && ch != '\n' && ch != '\t')
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            continue;
        }
        if length == maximum {
            break;
        }
        result.push(ch);
        length += 1;
    }
    result
}

pub(super) fn match_offset(text: &str, query_lower: &str) -> Option<usize> {
    if query_lower.is_empty() {
        return None;
    }
    let offset = text.to_lowercase().find(query_lower)?;
    let mut folded_bytes = 0;
    for (index, ch) in text.char_indices() {
        folded_bytes += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
        if folded_bytes > offset {
            return Some(index);
        }
    }
    None
}

pub(super) fn fragment(text: &str, query_lower: &str, maximum: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= maximum {
        return (text.to_owned(), false);
    }
    let matched_char =
        match_offset(text, query_lower).map_or(0, |offset| text[..offset].chars().count());
    let start = matched_char
        .saturating_sub(maximum / 3)
        .min(total.saturating_sub(maximum));
    let mut output = String::new();
    if start > 0 {
        output.push('…');
    }
    output.extend(text.chars().skip(start).take(maximum));
    if start + maximum < total {
        output.push('…');
    }
    (output, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn readable_text_excludes_signatures_images_and_extension_state() {
        let entry = json!({"type":"message", "message":{"role":"assistant", "content":[
            {"type":"text", "text":"visible", "textSignature":"secret-signature"},
            {"type":"thinking", "thinking":"reason", "thinkingSignature":"encrypted"},
            {"type":"thinking", "thinking":"redacted-payload", "redacted":true},
            {"type":"image", "data":"binary-image"},
            {"type":"toolCall", "name":"write", "arguments":{"content":"code needle", "state":"not-text", "image":{"data":"binary"}, "textSignature":"opaque"}, "thoughtSignature":"opaque"}
        ], "details":{"state":"opaque"}}});
        assert_eq!(
            readable_entry(&entry).unwrap().1,
            "visible\nreason\nwrite\ncode needle"
        );
        assert!(
            readable_entry(&json!({"type":"custom", "data":{"text":"not conversation"}})).is_none()
        );
    }

    #[test]
    fn context_replacements_and_prompt_sections_are_readable_but_not_opaque_payloads() {
        let entry = json!({"type":"context_edit", "replacement":{"content":[
            {"type":"text", "text":"replacement needle", "textSignature":"opaque"},
            {"type":"image", "data":"binary needle"}
        ], "state":"opaque"}});
        assert_eq!(readable_entry(&entry).unwrap().1, "replacement needle");
        let entry = json!({"type":"compaction", "summary":"saved summary", "systemMessage":{
            "content":"saved prompt", "sections":{"section":"saved section", "removed":null},
            "toolsAdded":[{"description":"not prompt text"}]
        }, "details":{"state":"not conversation"}});
        assert_eq!(
            readable_entry(&entry).unwrap().1,
            "saved summary\nsaved prompt\nsaved section"
        );
        let entry = json!({"type":"message", "message":{"role":"assistant", "content":[
            {"type":"toolCall", "name":"test", "arguments":{
                "content":"readable", "nested":[{"type":"image_url", "url":"opaque"},
                    {"type":"input_image", "image_url":"opaque"}],
                "provider_data":{"text":"opaque"}, "custom_state":{"text":"opaque"},
                "deferred":{"id":"opaque"}, "signature":{"text":"opaque"}
            }}
        ], "deferred":{"data":"opaque"}}});
        assert_eq!(readable_entry(&entry).unwrap().1, "test\nreadable");
        assert!(readable_entry(&json!({"type":"message", "message":{
            "role":"extension_state", "content":"not conversation"
        }}))
        .is_none());
    }

    #[test]
    fn unicode_casefold_and_match_centered_fragment_preserve_boundaries() {
        let text = format!("{}İ NEEDLE{}", "界".repeat(6000), "🦀".repeat(6000));
        let (part, truncated) = fragment(&text, "needle", 100);
        assert!(truncated);
        assert!(part.contains("NEEDLE"));
        assert!(part.chars().count() <= 102);
        assert_eq!(match_offset("İ NEEDLE", "needle"), Some(3));
        assert_eq!(match_offset("🦀İΣ NEEDLE", "needle"), Some(9));
        assert_eq!(match_offset("İ", "\u{307}"), Some(0));
        assert_eq!(match_offset("plain", "missing"), None);
    }

    #[test]
    fn strips_terminal_sequences_and_bidi_controls_without_losing_unicode() {
        assert_eq!(
            sanitize("🦀\x1b[31mred\x1b[0m\x1b]52;c;hidden\x07\u{202e}ok\0", 100),
            "🦀redok"
        );
        assert_eq!(sanitize("before\u{90}opaque\x07payload\u{9c}\u{98}opaque\u{9c}\u{9e}opaque\u{9c}\u{9f}opaque\u{9c}\x1bXopaque\x1b\\after", 100), "beforeafter");
    }

    #[test]
    fn bounded_reader_discards_oversized_lines_and_resumes() {
        let mut reader = BufReader::with_capacity(3, &b"0123456789\nok\npartial"[..]);
        assert_eq!(
            bounded_line(&mut reader, 5).unwrap(),
            Some((vec![], true, true))
        );
        assert_eq!(
            bounded_line(&mut reader, 5).unwrap(),
            Some((b"ok\n".to_vec(), false, true))
        );
        assert_eq!(
            bounded_line(&mut reader, 5).unwrap(),
            Some((vec![], true, false))
        );
    }
}
