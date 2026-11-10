use super::*;
use crate::pi_history::types::{
    HistoryAction, HistoryMatch, HistoryResult, HistoryScope, HistorySource,
};

const REQUEST: &[u8] = b"{\"synthetic\":true}\n";

struct FakeIo {
    input: io::Cursor<Vec<u8>>,
    written: Vec<u8>,
    read_chunk: usize,
    write_chunk: usize,
    pending_at_eof: bool,
    zero_writes: bool,
    cancel_on_wait: Option<Arc<AtomicBool>>,
    waits: usize,
}

impl FakeIo {
    fn new(input: Vec<u8>) -> Self {
        Self {
            input: io::Cursor::new(input),
            written: Vec::new(),
            read_chunk: 8192,
            write_chunk: 512,
            pending_at_eof: false,
            zero_writes: false,
            cancel_on_wait: None,
            waits: 0,
        }
    }
}

impl Read for FakeIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = bytes.len().min(self.read_chunk);
        match self.input.read(&mut bytes[..count])? {
            0 if self.pending_at_eof => Err(io::ErrorKind::WouldBlock.into()),
            count => Ok(count),
        }
    }
}

impl Write for FakeIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.write_chunk == 0 && !self.zero_writes {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = bytes.len().min(self.write_chunk);
        self.written.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl HelperIo for FakeIo {
    fn wait(&mut self, _writing: bool) -> io::Result<()> {
        self.waits += 1;
        if let Some(cancelled) = self.cancel_on_wait.take() {
            cancelled.store(true, Ordering::Release);
        }
        Ok(())
    }
}

fn hello() -> HistoryEvent {
    HistoryEvent::Hello {
        version: PROTOCOL_VERSION,
    }
}

fn encode(events: &[HistoryEvent]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn exchange_fake(
    stream: &mut FakeIo,
    cancelled: &AtomicBool,
) -> (Vec<HistoryEvent>, io::Result<()>) {
    let control = Control {
        cancelled,
        deadline: Instant::now() + Duration::from_secs(5),
    };
    let mut events = Vec::new();
    let result = exchange(stream, REQUEST, &control, &mut |event| {
        events.push(event);
        Ok(())
    });
    (events, result)
}

#[test]
fn fragmented_protocol_streams_hello_progress_result_and_done() {
    let events = vec![
        hello(),
        HistoryEvent::Progress {
            scanned: 1,
            total: 2,
        },
        HistoryEvent::Result {
            result: HistoryResult {
                source: HistorySource {
                    id: "synthetic-source".into(),
                    name: "synthetic".into(),
                    locations: vec![],
                },
                matches: vec![HistoryMatch {
                    entry_line: 1,
                    role: "user".into(),
                    excerpt: "synthetic 🦀 needle".into(),
                }],
                truncated: false,
            },
        },
        HistoryEvent::Done { truncated: false },
    ];
    for read_chunk in [1, 8192] {
        let mut stream = FakeIo::new(encode(&events));
        stream.read_chunk = read_chunk;
        stream.write_chunk = 3;
        let (received, result) = exchange_fake(&mut stream, &AtomicBool::new(false));
        result.unwrap();
        assert_eq!(received, events);
        assert_eq!(stream.written, REQUEST);
    }
}

#[test]
fn invalid_hello_never_sends_a_request() {
    for bytes in [
        b"not json\n".as_slice(),
        b"\n",
        b"\xff\n",
        b"{\"type\":\"hello\"}\n",
        b"{\"type\":\"hello\",\"version\":2}\n",
        b"{\"type\":\"done\",\"truncated\":false}\n",
        b"{\"type\":\"progress\",\"scanned\":0,\"total\":0}\n",
        b"{\"type\":\"unknown\"}\n",
    ] {
        let mut stream = FakeIo::new(bytes.to_vec());
        let (events, result) = exchange_fake(&mut stream, &AtomicBool::new(false));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(events.is_empty());
        assert!(stream.written.is_empty());
    }
}

#[test]
fn completion_requires_done_and_complete_json_lines_without_another_hello() {
    for (tail, kind) in [
        (b"".as_slice(), io::ErrorKind::UnexpectedEof),
        (b"{", io::ErrorKind::UnexpectedEof),
        (
            b"{\"type\":\"done\",\"truncated\":false}",
            io::ErrorKind::UnexpectedEof,
        ),
        (
            b"{\"type\":\"hello\",\"version\":1}\n",
            io::ErrorKind::InvalidData,
        ),
        (b"{invalid}\n", io::ErrorKind::InvalidData),
    ] {
        let mut bytes = encode(&[hello()]);
        bytes.extend_from_slice(tail);
        let mut stream = FakeIo::new(bytes);
        let (events, result) = exchange_fake(&mut stream, &AtomicBool::new(false));
        assert_eq!(result.unwrap_err().kind(), kind);
        assert_eq!(events, vec![hello()]);
        assert_eq!(stream.written, REQUEST);
    }
    let mut stream = FakeIo::new(encode(&[
        hello(),
        HistoryEvent::Error {
            message: "synthetic failure".into(),
        },
        HistoryEvent::Done { truncated: true },
    ]));
    let (events, result) = exchange_fake(&mut stream, &AtomicBool::new(false));
    result.unwrap();
    assert!(matches!(events.last(), Some(HistoryEvent::Done { .. })));
}

#[test]
fn event_byte_limit_excludes_the_newline_and_bounds_unterminated_output() {
    let mut line = serde_json::to_vec(&HistoryEvent::Error {
        message: "synthetic".into(),
    })
    .unwrap();
    line.resize(MAX_EVENT_BYTES, b' ');
    let mut bytes = encode(&[hello()]);
    bytes.extend_from_slice(&line);
    bytes.push(b'\n');
    bytes.extend_from_slice(&encode(&[HistoryEvent::Done { truncated: true }]));
    let mut stream = FakeIo::new(bytes);
    let (events, result) = exchange_fake(&mut stream, &AtomicBool::new(false));
    result.unwrap();
    assert_eq!(events.len(), 3);
    for newline in [false, true] {
        let mut bytes = encode(&[hello()]);
        bytes.extend_from_slice(&vec![b' '; MAX_EVENT_BYTES + 1]);
        if newline {
            bytes.push(b'\n');
        }
        let mut stream = FakeIo::new(bytes);
        let (_, result) = exchange_fake(&mut stream, &AtomicBool::new(false));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(stream.input.position() as usize <= MAX_EVENT_BYTES + 8192);
    }
}

#[test]
fn request_size_includes_newline_and_json_escaping() {
    let mut request = HistoryRequest {
        scope: HistoryScope::Session,
        session: "synthetic".into(),
        workspace_id: None,
        query: String::new(),
        action: HistoryAction::Search,
    };
    let overhead = request_bytes(&request).unwrap().len();
    request.query = "x".repeat(MAX_REQUEST_BYTES - overhead);
    let bytes = request_bytes(&request).unwrap();
    assert_eq!(bytes.len(), MAX_REQUEST_BYTES);
    assert_eq!(bytes.last(), Some(&b'\n'));
    request.query.push('x');
    assert_eq!(
        request_bytes(&request).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    request.query = "\0".repeat(MAX_REQUEST_BYTES);
    assert_eq!(
        request_bytes(&request).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn cancellation_is_checked_during_hello_write_and_event_waits() {
    for stage in ["hello", "write", "zero-write", "events"] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut stream = FakeIo::new(if stage == "hello" {
            vec![]
        } else {
            encode(&[hello()])
        });
        stream.pending_at_eof = true;
        stream.cancel_on_wait = Some(Arc::clone(&cancelled));
        if stage == "write" || stage == "zero-write" {
            stream.write_chunk = 0;
            stream.zero_writes = stage == "zero-write";
        }
        let (_, result) = exchange_fake(&mut stream, &cancelled);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
        assert_eq!(stream.waits, 1);
    }
}

#[test]
fn expired_deadlines_prevent_reads_and_writes() {
    let cancelled = AtomicBool::new(false);
    let control = Control {
        cancelled: &cancelled,
        deadline: Instant::now() + Duration::from_secs(5),
    };
    let mut stream = FakeIo::new(encode(&[hello()]));
    assert_eq!(
        EventReader::new()
            .read(&mut stream, &control, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(
        write_request(&mut stream, REQUEST, &control, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(stream.input.position(), 0);
    assert!(stream.written.is_empty());
}

#[test]
fn lifetime_deadline_cancels_discovery_using_the_same_flag() {
    let cancelled = Arc::new(AtomicBool::new(false));
    bounded_lifetime(&cancelled, Duration::from_secs(5), |_| Ok(())).unwrap();
    assert!(!cancelled.load(Ordering::Acquire));
    let error = bounded_lifetime(&cancelled, Duration::from_millis(20), |control| {
        while !control.cancelled.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Err::<(), _>(io::ErrorKind::ConnectionAborted.into())
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(cancelled.load(Ordering::Acquire));
}

#[test]
fn closed_consumer_stops_streaming_before_done() {
    let mut stream = FakeIo::new(encode(&[
        hello(),
        HistoryEvent::Progress {
            scanned: 0,
            total: 1,
        },
        HistoryEvent::Done { truncated: false },
    ]));
    let cancelled = AtomicBool::new(false);
    let control = Control {
        cancelled: &cancelled,
        deadline: Instant::now() + Duration::from_secs(5),
    };
    let mut emitted = 0;
    let error = exchange(&mut stream, REQUEST, &control, &mut |_| {
        emitted += 1;
        if emitted == 2 {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(emitted, 2);
}

#[test]
fn cancellation_reaps_the_owned_local_helper() {
    let test_name = concat!(module_path!(), "::helper_fixture");
    let test_name = test_name.split_once("::").unwrap().1;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test_name, "--nocapture"])
        .env("HERDR_HISTORY_TRANSPORT_TEST_CHILD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::configure_background_command(&mut command);
    let child = command.spawn().unwrap();
    let pid = child.id();
    let mut stream = HelperStream::local(child).unwrap();
    let cancelled = AtomicBool::new(true);
    let control = Control {
        cancelled: &cancelled,
        deadline: Instant::now() + Duration::from_secs(5),
    };
    assert_eq!(
        exchange(&mut stream, REQUEST, &control, &mut |_| Ok(()))
            .unwrap_err()
            .kind(),
        io::ErrorKind::ConnectionAborted
    );
    drop(stream);
    assert!(!crate::platform::process_exists(pid));
}

#[test]
fn helper_fixture() {
    if std::env::var_os("HERDR_HISTORY_TRANSPORT_TEST_CHILD").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        std::thread::sleep(Duration::from_secs(30));
    }
}
