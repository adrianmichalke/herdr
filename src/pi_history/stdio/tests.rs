use super::*;
use crate::pi_history::test_support::{location, request, FixtureDir};
use crate::pi_history::types::HistorySource;
use serde_json::json;

fn catalog(path: &std::path::Path) -> Catalog {
    let id = format!("pi:{}", "a".repeat(64));
    let mut catalog = Catalog::default();
    catalog.sources.insert(
        id.clone(),
        CatalogSource {
            source: HistorySource {
                id,
                name: "Synthetic Pi history".into(),
                locations: vec![location(path, "one", "w1")],
            },
            path: path.into(),
        },
    );
    catalog
}

fn run(request: &HistoryRequest, catalog: Catalog) -> Vec<HistoryEvent> {
    let mut input = serde_json::to_vec(request).unwrap();
    input.push(b'\n');
    let mut output = Vec::new();
    serve(&mut &input[..], &mut output, |_, _| Ok(catalog)).unwrap();
    output
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            assert!(line.len() <= MAX_EVENT_BYTES);
            serde_json::from_slice(line).unwrap()
        })
        .collect()
}

fn message(text: &str) -> String {
    let mut value = serde_json::to_string(
        &json!({"type":"message", "message":{"role":"user", "content":text}}),
    )
    .unwrap();
    value.push('\n');
    value
}

#[test]
fn searches_raw_pre_compaction_and_inactive_branches_without_applying_context_edits() {
    let dir = FixtureDir::new();
    let text = [
        json!({"type":"session", "version":3}),
        json!({"type":"message", "id":"old", "parentId":null, "message":{"role":"user", "content":"NEEDLE before compaction"}}),
        json!({"type":"context_edit", "targetId":"old", "replacement":null}),
        json!({"type":"compaction", "summary":"not the original", "firstKeptEntryId":"new"}),
        json!({"type":"message", "id":"inactive", "parentId":"old", "message":{"role":"assistant", "content":[{"type":"text", "text":"needle inactive branch"}]}}),
        json!({"type":"message", "id":"new", "parentId":"old", "message":{"role":"user", "content":"active branch without keyword"}}),
        json!({"type":"custom", "data":{"text":"needle opaque state"}}),
        json!({"type":"message", "message":{"role":"user", "content":[{"type":"image", "data":"needle image"}]}}),
    ].iter().map(|value| format!("{value}\n")).collect::<String>();
    let path = dir.write("raw.jsonl", &text);
    let mut sources = catalog(&path);
    sources
        .sources
        .values_mut()
        .next()
        .unwrap()
        .source
        .locations
        .push(location(&path, "two", "w2"));
    let events = run(&request(), sources);
    assert!(matches!(
        events.first(),
        Some(HistoryEvent::Hello { version: 1 })
    ));
    let results: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            HistoryEvent::Result { result } => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].matches.len(), 2);
    assert_eq!(results[0].source.locations.len(), 2);
    assert_eq!(results[0].matches[0].entry_line, 2);
    assert_eq!(results[0].matches[1].entry_line, 5);
    assert!(matches!(
        events.last(),
        Some(HistoryEvent::Done { truncated: false })
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
}

#[test]
fn searches_beyond_the_former_physical_record_prefix_limit() {
    let dir = FixtureDir::new();
    let path = dir.write(
        "long.jsonl",
        format!("{}{}", "\n".repeat(100_001), message("needle at the end")),
    );
    let events = run(&request(), catalog(&path));
    assert!(events.iter().any(|event| matches!(event, HistoryEvent::Result { result } if result.matches[0].entry_line == 100_002)));
    assert!(matches!(
        events.last(),
        Some(HistoryEvent::Done { truncated: false })
    ));
}

#[test]
fn searches_beyond_the_former_file_byte_prefix_limit() {
    use std::io::{Seek, SeekFrom, Write};

    let dir = FixtureDir::new();
    let path = dir.write("large.jsonl", "");
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    file.write_all(b"\n").unwrap();
    file.write_all(message("needle after oversized record").as_bytes())
        .unwrap();
    drop(file);
    let events = run(&request(), catalog(&path));
    assert!(events.iter().any(|event| matches!(event, HistoryEvent::Result { result } if result.matches[0].entry_line == 2)));
    assert!(events.iter().any(|event| matches!(event, HistoryEvent::Unavailable { message, .. } if message.contains("1 oversized"))));
}

#[test]
fn malformed_partial_and_oversized_records_report_partial_coverage_and_continue() {
    let dir = FixtureDir::new();
    let text = format!(
        "{}not json\n{}\n{}{{\"type\":",
        message("needle first"),
        "x".repeat(super::super::parser::MAX_RECORD_BYTES + 1),
        message("needle after bad records")
    );
    let path = dir.write("raw.jsonl", text);
    let events = run(&request(), catalog(&path));
    let result = events
        .iter()
        .find_map(|event| {
            if let HistoryEvent::Result { result } = event {
                Some(result)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(result.matches.len(), 2);
    assert!(result.truncated);
    assert!(events.iter().any(|event| matches!(event, HistoryEvent::Unavailable { message, .. } if message.contains("1 malformed") && message.contains("1 oversized") && message.contains("partial trailing record: true"))));
    assert!(matches!(
        events.last(),
        Some(HistoryEvent::Done { truncated: true })
    ));
}

#[test]
fn bounded_previews_center_on_match_and_supply_working_page_anchors() {
    let dir = FixtureDir::new();
    let text = (1..=40)
        .map(|line| {
            message(&if line == 20 {
                format!("{}NEEDLE{}", "界".repeat(6000), "🦀".repeat(6000))
            } else {
                format!("entry {line}")
            })
        })
        .collect::<String>();
    let path = dir.write("raw.jsonl", &text);
    let mut req = request();
    req.action = HistoryAction::Preview {
        source_id: format!("pi:{}", "a".repeat(64)),
        anchor: 20,
    };
    let events = run(&req, catalog(&path));
    let preview = events
        .iter()
        .find_map(|event| {
            if let HistoryEvent::Preview { preview } = event {
                Some(preview)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(preview.matched_entry, PREVIEW_BEFORE);
    assert_eq!(preview.entries[preview.matched_entry].entry_line, 20);
    assert!(preview.entries[preview.matched_entry]
        .text
        .contains("NEEDLE"));
    assert!(preview.entries[preview.matched_entry].truncated);
    assert_eq!(preview.previous, Some(15));
    assert_eq!(preview.next, Some(29));
    assert_eq!(preview.entries.len(), PREVIEW_BEFORE + PREVIEW_AFTER + 1);
    assert!(preview
        .entries
        .iter()
        .all(|entry| entry.text.chars().count() <= PREVIEW_CHARS + 2));
    for anchor in [preview.previous.unwrap(), preview.next.unwrap()] {
        req.action = HistoryAction::Preview {
            source_id: preview.source.id.clone(),
            anchor,
        };
        let events = run(&req, catalog(&path));
        let page = events
            .iter()
            .find_map(|event| {
                if let HistoryEvent::Preview { preview } = event {
                    Some(preview)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(page.entries[page.matched_entry].entry_line, anchor);
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
}

#[test]
fn preview_and_resolve_only_use_handles_in_the_refreshed_catalog() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", message("needle"));
    let mut req = request();
    req.action = HistoryAction::Resolve {
        source_id: format!("pi:{}", "a".repeat(64)),
    };
    assert!(run(&req, catalog(&path))
        .iter()
        .any(|event| matches!(event, HistoryEvent::Resolved { .. })));
    for action in [
        HistoryAction::Resolve {
            source_id: format!("pi:{}", "b".repeat(64)),
        },
        HistoryAction::Preview {
            source_id: format!("pi:{}", "b".repeat(64)),
            anchor: 1,
        },
    ] {
        req.action = action;
        let events = run(&req, catalog(&path));
        assert!(!events.iter().any(|event| matches!(
            event,
            HistoryEvent::Preview { .. } | HistoryEvent::Resolved { .. }
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, HistoryEvent::Unavailable { .. })));
    }
    req.action = HistoryAction::Preview {
        source_id: path.display().to_string(),
        anchor: 1,
    };
    let events = run(&req, catalog(&path));
    assert!(matches!(events[1], HistoryEvent::Error { .. }));
}

#[test]
fn result_and_record_byte_limits_are_explicit_and_do_not_load_unbounded_text() {
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", message("needle").repeat(MAX_MATCHES + 10));
    let events = run(&request(), catalog(&path));
    let result = events
        .iter()
        .find_map(|event| {
            if let HistoryEvent::Result { result } = event {
                Some(result)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(result.matches.len(), MAX_MATCHES);
    assert!(result.truncated);
    let mut budget = ScanBudget {
        bytes: 8,
        records: 100,
    };
    assert!(scan(&path, &mut budget, |_| Ok(true)).unwrap().limited);
    assert_eq!(budget.bytes, 0);
    let mut budget = ScanBudget {
        bytes: 100_000,
        records: 1,
    };
    assert!(scan(&path, &mut budget, |_| Ok(true)).unwrap().limited);
    assert_eq!(budget.records, 0);
}

#[test]
fn unterminated_valid_json_is_not_committed_even_when_completed_during_scan() {
    use std::io::Write;
    let dir = FixtureDir::new();
    let partial = message("unfinished needle");
    let path = dir.write(
        "raw.jsonl",
        format!("{}{}", message("first needle"), partial.trim_end()),
    );
    let mut entries = Vec::new();
    let report = scan(&path, &mut ScanBudget::default(), |entry| {
        if let Some(entry) = entry {
            if entries.is_empty() {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(format!("\n{}", message("new needle")).as_bytes())
                    .unwrap();
            }
            entries.push(entry);
        }
        Ok(true)
    })
    .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].text, "first needle");
    assert!(report.partial);
    assert!(report.changed);
    let path = dir.write("raw.jsonl", partial.trim_end());
    let events = run(&request(), catalog(&path));
    assert!(!events
        .iter()
        .any(|event| matches!(event, HistoryEvent::Result { .. })));
    assert!(matches!(
        events.last(),
        Some(HistoryEvent::Done { truncated: true })
    ));
}

#[test]
fn record_budget_counts_unreadable_and_empty_records_without_scanning_a_later_match() {
    let dir = FixtureDir::new();
    let path = dir.write(
        "raw.jsonl",
        format!(
            "\n{{\"type\":\"custom\",\"data\":{{}}}}\nnot json\n{}",
            message("later needle")
        ),
    );
    let mut budget = ScanBudget {
        bytes: 100_000,
        records: 3,
    };
    let mut entries = Vec::new();
    let report = scan(&path, &mut budget, |entry| {
        entries.extend(entry);
        Ok(true)
    })
    .unwrap();
    assert!(entries.is_empty());
    assert!(report.limited);
    assert_eq!(report.malformed, 1);
    assert_eq!(budget.records, 0);
    let complete = message("needle ending at the byte limit");
    let path = dir.write("raw.jsonl", &complete);
    let mut budget = ScanBudget {
        bytes: complete.len() as u64 - 1,
        records: 10,
    };
    let report = scan(&path, &mut budget, |entry| {
        assert!(entry.is_none());
        Ok(true)
    })
    .unwrap();
    assert!(report.partial);
    assert!(report.limited);
    assert_eq!(budget.bytes, 0);
}

#[test]
fn growth_and_truncation_during_scan_are_reported_without_following_new_bytes() {
    use std::io::Write;
    let dir = FixtureDir::new();
    let path = dir.write("raw.jsonl", message("first needle"));
    let mut lines = 0;
    let report = scan(&path, &mut ScanBudget::default(), |entry| {
        if entry.is_some() {
            lines += 1;
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(message("appended needle").as_bytes())
                .unwrap();
        }
        Ok(true)
    })
    .unwrap();
    assert_eq!(lines, 1);
    assert!(report.changed);
    let report = scan(&path, &mut ScanBudget::default(), |entry| {
        if entry.is_some() {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(0)
                .unwrap();
        }
        Ok(true)
    })
    .unwrap();
    assert!(report.changed);
}

#[test]
fn invalid_or_oversized_requests_never_attempt_catalog_discovery() {
    for input in [vec![b'x'; MAX_REQUEST_BYTES as usize + 10], b"{}\n".to_vec(), b"{\"scope\":\"space\",\"session\":\"one\",\"query\":\"needle\",\"action\":{\"kind\":\"search\"}}\n".to_vec()] {
        let mut output = Vec::new();
        serve(&mut &input[..], &mut output, |_, _| panic!("invalid request must not discover sessions")).unwrap();
        let lines: Vec<_> = output.split(|&byte| byte == b'\n').filter(|line| !line.is_empty()).collect();
        assert_eq!(lines.len(), 3);
        assert!(matches!(serde_json::from_slice::<HistoryEvent>(lines[1]).unwrap(), HistoryEvent::Error { .. }));
    }
}

#[test]
fn read_only_history_remains_byte_identical_and_unsupported_versions_are_reported() {
    let dir = FixtureDir::new();
    let text = message("read-only needle");
    let path = dir.write("raw.jsonl", &text);
    let original = std::fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(&path, readonly).unwrap();
    let events = run(&request(), catalog(&path));
    assert!(events
        .iter()
        .any(|event| matches!(event, HistoryEvent::Result { .. })));
    assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    std::fs::set_permissions(&path, original).unwrap();
    dir.write(
        "raw.jsonl",
        format!("{{\"type\":\"session\",\"version\":999}}\n{text}"),
    );
    let events = run(&request(), catalog(&path));
    assert!(!events
        .iter()
        .any(|event| matches!(event, HistoryEvent::Result { .. })));
    assert!(events.iter().any(|event| matches!(event, HistoryEvent::Unavailable { message, .. } if message.contains("unsupported Pi format: true"))));
}

#[test]
fn event_size_guard_accounts_for_serialization_and_newline_without_partial_writes() {
    let mut output = Vec::new();
    assert!(write_event(
        &mut output,
        &HistoryEvent::Error {
            message: "\t".repeat(MAX_EVENT_BYTES / 2)
        }
    )
    .is_err());
    assert!(output.is_empty());
    write_event(
        &mut output,
        &HistoryEvent::Error {
            message: "🦀\n".repeat(1000),
        },
    )
    .unwrap();
    assert!(output.len() <= MAX_EVENT_BYTES);
    assert!(output.ends_with(b"\n"));
}

#[test]
fn disconnected_output_stops_before_any_catalog_work() {
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        serve(&mut &b"{}\n"[..], &mut Closed, |_, _| panic!(
            "closed transport"
        ))
        .unwrap_err()
        .kind(),
        io::ErrorKind::BrokenPipe
    );
}
