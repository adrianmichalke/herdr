use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

use crate::pi_history::types::{HistoryEvent, HistoryRequest, MAX_EVENT_BYTES, PROTOCOL_VERSION};
use crate::platform::ChildPipe;

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(35);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Debug)]
pub(super) enum HelperTarget {
    Local,
    Ssh {
        profile_id: String,
        target: String,
        session: String,
    },
}

pub(super) fn run(
    target: HelperTarget,
    request: HistoryRequest,
    cancelled: Arc<AtomicBool>,
    mut emit: impl FnMut(HistoryEvent) -> io::Result<()>,
) -> io::Result<()> {
    bounded_lifetime(&cancelled, TOTAL_TIMEOUT, |control| {
        control.check(control.deadline)?;
        let request = request_bytes(&request)?;
        control.check(control.deadline)?;
        let mut stream = match target {
            HelperTarget::Local => {
                let mut command = Command::new(std::env::current_exe()?);
                command
                    .args(["history", "--stdio"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                crate::platform::configure_background_command(&mut command);
                HelperStream::local(command.spawn()?)?
            }
            HelperTarget::Ssh {
                profile_id,
                target,
                session,
            } => {
                let mut saved = crate::remote::connect_saved_history(
                    &profile_id,
                    &target,
                    &session,
                    Arc::clone(&cancelled),
                )?;
                crate::ipc::set_local_stream_polling(&mut saved.stream, true)?;
                HelperStream::Ssh(saved)
            }
        };
        exchange(&mut stream, &request, control, &mut emit)
    })
}

struct Control<'a> {
    cancelled: &'a AtomicBool,
    deadline: Instant,
}

impl Control<'_> {
    fn check(&self, deadline: Instant) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "history helper cancelled",
            ));
        }
        if Instant::now() >= deadline.min(self.deadline) {
            return Err(timed_out());
        }
        Ok(())
    }
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "history helper timed out")
}

fn bounded_lifetime<T>(
    cancelled: &Arc<AtomicBool>,
    timeout: Duration,
    operation: impl FnOnce(&Control<'_>) -> io::Result<T>,
) -> io::Result<T> {
    let deadline = Instant::now() + timeout;
    let expired = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (done, finished) = mpsc::sync_channel(1);
        let timer_cancelled = Arc::clone(cancelled);
        let expired = &expired;
        let watchdog = scope.spawn(move || {
            if finished.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                == Err(mpsc::RecvTimeoutError::Timeout)
            {
                expired.store(true, Ordering::Release);
                timer_cancelled.store(true, Ordering::Release);
            }
        });
        let result = operation(&Control {
            cancelled,
            deadline,
        });
        let _ = done.send(());
        watchdog
            .join()
            .map_err(|_| io::Error::other("history deadline worker panicked"))?;
        if expired.load(Ordering::Acquire) || Instant::now() >= deadline {
            cancelled.store(true, Ordering::Release);
            Err(timed_out())
        } else {
            result
        }
    })
}

fn request_bytes(request: &HistoryRequest) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; MAX_REQUEST_BYTES];
    let mut cursor = io::Cursor::new(bytes.as_mut_slice());
    serde_json::to_writer(&mut cursor, request).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "history request exceeds the 16 KiB size limit",
        )
    })?;
    let length = cursor.position() as usize;
    if length == MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "history request exceeds the 16 KiB size limit",
        ));
    }
    bytes[length] = b'\n';
    bytes.truncate(length + 1);
    Ok(bytes)
}

struct OwnedHelper(Child);

impl Drop for OwnedHelper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

enum HelperStream {
    Local {
        input: ChildPipe,
        output: ChildPipe,
        _child: OwnedHelper,
    },
    Ssh(crate::remote::SavedSshStream),
}

impl HelperStream {
    fn local(child: Child) -> io::Result<Self> {
        let mut child = OwnedHelper(child);
        let input = child
            .0
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("history helper stdin missing"))?;
        let output = child
            .0
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("history helper stdout missing"))?;
        Ok(Self::Local {
            input: ChildPipe::new_writer(input)?,
            output: ChildPipe::new_reader(output)?,
            _child: child,
        })
    }
}

trait HelperIo: Read + Write {
    fn wait(&mut self, writing: bool) -> io::Result<()>;
}

impl Read for HelperStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Local { output, .. } => output.read(bytes),
            Self::Ssh(saved) => {
                match crate::ipc::poll_local_stream_read_count(&mut saved.stream, bytes)? {
                    crate::ipc::LocalStreamReadCount::Data(count) => Ok(count),
                    crate::ipc::LocalStreamReadCount::Closed => Ok(0),
                    crate::ipc::LocalStreamReadCount::Pending => {
                        Err(io::ErrorKind::WouldBlock.into())
                    }
                }
            }
        }
    }
}

impl Write for HelperStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Local { input, .. } => input.write(bytes),
            Self::Ssh(saved) => saved.stream.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl HelperIo for HelperStream {
    fn wait(&mut self, writing: bool) -> io::Result<()> {
        match self {
            Self::Local { input, output, .. } => {
                if writing {
                    input.wait(true)
                } else {
                    output.wait(false)
                }
            }
            Self::Ssh(_) => {
                std::thread::sleep(POLL_INTERVAL);
                Ok(())
            }
        }
    }
}

fn exchange(
    stream: &mut impl HelperIo,
    request: &[u8],
    control: &Control<'_>,
    emit: &mut impl FnMut(HistoryEvent) -> io::Result<()>,
) -> io::Result<()> {
    let mut reader = EventReader::new();
    let hello = reader.read(stream, control, Instant::now() + HELLO_TIMEOUT)?;
    match hello {
        HistoryEvent::Hello { version } if version == PROTOCOL_VERSION => emit(hello)?,
        _ => return Err(invalid_event("expected history protocol v1 hello")),
    }
    write_request(stream, request, control, Instant::now() + WRITE_TIMEOUT)?;
    loop {
        let event = reader.read(stream, control, Instant::now() + IDLE_TIMEOUT)?;
        if matches!(event, HistoryEvent::Hello { .. }) {
            return Err(invalid_event("unexpected history hello after handshake"));
        }
        let done = matches!(event, HistoryEvent::Done { .. });
        emit(event)?;
        if done {
            return Ok(());
        }
    }
}

fn write_request(
    stream: &mut impl HelperIo,
    mut bytes: &[u8],
    control: &Control<'_>,
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        control.check(deadline)?;
        match stream.write(&bytes[..bytes.len().min(512)]) {
            Ok(0) => stream.wait(true)?,
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => stream.wait(true)?,
            Err(error) => return Err(error),
        }
    }
    control.check(deadline)
}

fn invalid_event(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct EventReader {
    buffer: [u8; 8192],
    offset: usize,
    available: usize,
    line: Vec<u8>,
}

impl EventReader {
    fn new() -> Self {
        Self {
            buffer: [0; 8192],
            offset: 0,
            available: 0,
            line: Vec::new(),
        }
    }

    fn read(
        &mut self,
        stream: &mut impl HelperIo,
        control: &Control<'_>,
        deadline: Instant,
    ) -> io::Result<HistoryEvent> {
        self.line.clear();
        loop {
            control.check(deadline)?;
            let buffered = &self.buffer[self.offset..self.available];
            let newline = buffered.iter().position(|byte| *byte == b'\n');
            let count = newline.unwrap_or(buffered.len());
            if count > MAX_EVENT_BYTES - self.line.len() {
                return Err(invalid_event("history event exceeds the 1 MiB size limit"));
            }
            self.line.extend_from_slice(&buffered[..count]);
            self.offset += count;
            if newline.is_some() {
                self.offset += 1;
                let event = serde_json::from_slice(&self.line)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                control.check(deadline)?;
                return Ok(event);
            }
            match stream.read(&mut self.buffer) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        if self.line.is_empty() {
                            "history helper closed before completing the protocol"
                        } else {
                            "history helper sent an unterminated JSON event"
                        },
                    ));
                }
                Ok(count) => {
                    self.offset = 0;
                    self.available = count;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => stream.wait(false)?,
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
