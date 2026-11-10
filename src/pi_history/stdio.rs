use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};

use super::catalog::{Catalog, CatalogSource};
use super::parser::{fragment, match_offset, sanitize, scan, ScanBudget, PREVIEW_CHARS};
use super::types::{
    HistoryAction, HistoryEntry, HistoryEvent, HistoryMatch, HistoryPreview, HistoryRequest,
    HistoryResult, HistoryScope, MAX_EVENT_BYTES, PROTOCOL_VERSION,
};

const MAX_REQUEST_BYTES: u64 = 16 * 1024;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_MATCHES: usize = 64;
const MAX_RESULTS: usize = 256;
const PREVIEW_BEFORE: usize = 4;
const PREVIEW_AFTER: usize = 8;

pub(crate) fn run_stdio() -> io::Result<()> {
    crate::platform::begin_cli_output();
    serve(
        &mut BufReader::new(io::stdin().lock()),
        &mut io::stdout().lock(),
        Catalog::collect,
    )
}

fn serve(
    reader: &mut impl BufRead,
    output: &mut impl Write,
    collect: impl FnOnce(
        &HistoryRequest,
        &mut dyn FnMut(HistoryEvent) -> io::Result<()>,
    ) -> io::Result<Catalog>,
) -> io::Result<()> {
    let mut emit = |event| write_event(output, &event);
    emit(HistoryEvent::Hello {
        version: PROTOCOL_VERSION,
    })?;
    let request = match read_request(reader).and_then(validate_request) {
        Ok(request) => request,
        Err(error) => {
            emit(HistoryEvent::Error {
                message: sanitize(&error.to_string(), 512),
            })?;
            return emit(HistoryEvent::Done { truncated: true });
        }
    };
    emit(HistoryEvent::Progress {
        scanned: 0,
        total: 0,
    })?;
    let catalog = match collect(&request, &mut emit) {
        Ok(catalog) => catalog,
        Err(error) => {
            emit(HistoryEvent::Error {
                message: format!(
                    "History catalog unavailable: {}",
                    sanitize(&error.to_string(), 512)
                ),
            })?;
            return emit(HistoryEvent::Done { truncated: true });
        }
    };
    let query = sanitize(&request.query, MAX_QUERY_BYTES).to_lowercase();
    let mut truncated = catalog.truncated;
    match request.action {
        HistoryAction::Search => truncated |= search(catalog, &query, &mut emit)?,
        HistoryAction::Preview { source_id, anchor } => {
            if let Some(source) = catalog.sources.get(&source_id) {
                truncated |= preview(source, anchor, &query, &mut emit)?;
            } else {
                truncated = true;
                emit(HistoryEvent::Unavailable {
                    locations: vec![],
                    message: "Pi source is no longer available in the allowed history scope".into(),
                })?;
            }
        }
        HistoryAction::Resolve { source_id } => {
            if let Some(source) = catalog.sources.get(&source_id) {
                emit(HistoryEvent::Resolved {
                    source: source.source.clone(),
                })?;
            } else {
                truncated = true;
                emit(HistoryEvent::Unavailable {
                    locations: vec![],
                    message: "Pi source is no longer associated with this history scope".into(),
                })?;
            }
        }
    }
    emit(HistoryEvent::Done { truncated })
}

fn read_request(reader: &mut impl BufRead) -> io::Result<HistoryRequest> {
    let mut line = Vec::new();
    reader
        .take(MAX_REQUEST_BYTES + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() as u64 > MAX_REQUEST_BYTES || !line.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history expects one bounded newline-terminated JSON request",
        ));
    }
    serde_json::from_slice(&line).map_err(io::Error::other)
}

fn validate_request(request: HistoryRequest) -> io::Result<HistoryRequest> {
    crate::session::parse_target_name(&request.session).map_err(io::Error::other)?;
    if request.query.len() > MAX_QUERY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history query exceeds the size limit",
        ));
    }
    if request.scope == HistoryScope::Space
        && !request
            .workspace_id
            .as_ref()
            .is_some_and(|id| !id.is_empty() && id.len() <= 256)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "current-space history requires an explicit workspace ID",
        ));
    }
    if matches!(request.action, HistoryAction::Search)
        && sanitize(&request.query, MAX_QUERY_BYTES).trim().is_empty()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history search requires a nonempty literal query",
        ));
    }
    if let HistoryAction::Preview { source_id, .. } | HistoryAction::Resolve { source_id } =
        &request.action
    {
        if source_id.len() != 67
            || !source_id.starts_with("pi:")
            || !source_id[3..].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid history source handle",
            ));
        }
    }
    Ok(request)
}

fn write_event(output: &mut impl Write, event: &HistoryEvent) -> io::Result<()> {
    let bytes = serde_json::to_vec(event)?;
    if bytes.len().saturating_add(1) > MAX_EVENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "history event exceeds the protocol size limit",
        ));
    }
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn search(
    catalog: Catalog,
    query: &str,
    emit: &mut impl FnMut(HistoryEvent) -> io::Result<()>,
) -> io::Result<bool> {
    let total = catalog.sources.len();
    let mut budget = ScanBudget::default();
    let mut result_count = 0;
    let mut truncated = false;
    emit(HistoryEvent::Progress { scanned: 0, total })?;
    for (index, source) in catalog.sources.into_values().enumerate() {
        if budget.bytes == 0 || budget.records == 0 || result_count == MAX_RESULTS {
            emit(HistoryEvent::Unavailable {
                locations: vec![],
                message: "History search limit reached; additional sources were not scanned".into(),
            })?;
            return Ok(true);
        }
        let mut matches = Vec::new();
        let mut matches_limited = false;
        let report = scan(&source.path, &mut budget, |entry| {
            let Some(entry) = entry else {
                emit(HistoryEvent::Progress {
                    scanned: index,
                    total,
                })?;
                return Ok(true);
            };
            if match_offset(&entry.text, query).is_some() {
                if matches.len() == MAX_MATCHES {
                    matches_limited = true;
                    return Ok(false);
                }
                let (excerpt, _) = fragment(&entry.text, query, 320);
                matches.push(HistoryMatch {
                    entry_line: entry.entry_line,
                    role: entry.role,
                    excerpt,
                });
            }
            Ok(true)
        });
        let source_truncated = match &report {
            Ok(report) => report.incomplete() || matches_limited,
            Err(_) => true,
        };
        truncated |= source_truncated;
        if !matches.is_empty() {
            result_count += 1;
            emit(HistoryEvent::Result {
                result: HistoryResult {
                    source: source.source.clone(),
                    matches,
                    truncated: source_truncated,
                },
            })?;
        }
        let diagnostic = match report {
            Ok(report) => report.diagnostic(),
            Err(error) => Some(format!(
                "Pi history could not be read: {}",
                sanitize(&error.to_string(), 512)
            )),
        };
        if let Some(message) = diagnostic {
            emit(HistoryEvent::Unavailable {
                locations: source.source.locations.clone(),
                message,
            })?;
        }
        if matches_limited {
            emit(HistoryEvent::Unavailable {
                locations: source.source.locations,
                message:
                    "Pi source match limit reached; additional matching records were not returned"
                        .into(),
            })?;
        }
        emit(HistoryEvent::Progress {
            scanned: index + 1,
            total,
        })?;
    }
    Ok(truncated)
}

fn preview(
    source: &CatalogSource,
    anchor: u64,
    query: &str,
    emit: &mut impl FnMut(HistoryEvent) -> io::Result<()>,
) -> io::Result<bool> {
    let mut before = VecDeque::<HistoryEntry>::new();
    let mut entries = Vec::new();
    let mut matched_entry = None;
    let mut previous = None;
    let mut next = None;
    let mut anchor_missing = false;
    let mut budget = ScanBudget::default();
    let report = scan(&source.path, &mut budget, |entry| {
        let Some(mut entry) = entry else {
            emit(HistoryEvent::Progress {
                scanned: 0,
                total: 1,
            })?;
            return Ok(true);
        };
        let (text, truncated) = fragment(&entry.text, query, PREVIEW_CHARS);
        entry.text = text;
        entry.truncated = truncated;
        if matched_entry.is_none() {
            if entry.entry_line < anchor {
                before.push_back(entry);
                if before.len() > PREVIEW_BEFORE {
                    previous = before.pop_front().map(|entry| entry.entry_line);
                }
                return Ok(true);
            }
            if anchor != 0 && entry.entry_line != anchor {
                anchor_missing = true;
                return Ok(false);
            }
            entries.extend(before.drain(..));
            matched_entry = Some(entries.len());
        } else if matched_entry.is_some_and(|matched| entries.len() > matched + PREVIEW_AFTER) {
            next = Some(entry.entry_line);
            return Ok(false);
        }
        entries.push(entry);
        Ok(true)
    });
    let (incomplete, diagnostic) = match report {
        Ok(report) => (report.incomplete(), report.diagnostic()),
        Err(error) => (
            true,
            Some(format!(
                "Pi history preview unavailable: {}",
                sanitize(&error.to_string(), 512)
            )),
        ),
    };
    if let Some(message) = diagnostic {
        emit(HistoryEvent::Unavailable {
            locations: source.source.locations.clone(),
            message,
        })?;
    }
    let Some(matched_entry) = matched_entry.filter(|_| !anchor_missing) else {
        emit(HistoryEvent::Unavailable { locations: source.source.locations.clone(), message: "Preview anchor is unavailable or the history has no readable entries; search again to refresh it".into() })?;
        return Ok(true);
    };
    let truncated = incomplete || entries.iter().any(|entry| entry.truncated);
    emit(HistoryEvent::Preview {
        preview: HistoryPreview {
            source: source.source.clone(),
            entries,
            matched_entry,
            previous,
            next,
            truncated,
        },
    })?;
    emit(HistoryEvent::Progress {
        scanned: 1,
        total: 1,
    })?;
    Ok(truncated)
}

#[cfg(test)]
mod tests;
