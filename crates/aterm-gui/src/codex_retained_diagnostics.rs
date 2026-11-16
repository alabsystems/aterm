// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private stderr capture; only closed diagnostic classifications can leave it.
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const LIMIT: usize = 32 * 1024;
const MAX_COORDINATE: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Completion {
    Open,
    Eof,
    ReadError,
    Unavailable,
}
struct State {
    bytes: VecDeque<u8>,
    truncated: bool,
    completion: Completion,
}
pub(super) struct Capture {
    state: Mutex<State>,
    changed: Condvar,
}
impl Capture {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                bytes: VecDeque::new(),
                truncated: false,
                completion: Completion::Open,
            }),
            changed: Condvar::new(),
        }
    }
    fn append(&self, bytes: &[u8]) {
        if let Ok(mut state) = self.state.lock() {
            if state.completion != Completion::Open {
                return;
            }
            for byte in bytes {
                if state.bytes.len() == LIMIT {
                    state.bytes.pop_front();
                    state.truncated = true;
                }
                state.bytes.push_back(*byte);
            }
        }
    }
    fn finish(&self, completion: Completion) {
        if let Ok(mut state) = self.state.lock()
            && state.completion == Completion::Open
        {
            state.completion = completion;
        }
        self.changed.notify_all();
    }
    pub(super) fn unavailable(&self) {
        self.finish(Completion::Unavailable);
    }
    pub(super) fn report(&self, wait: Duration) -> Report {
        let deadline = Instant::now() + wait;
        let Ok(mut state) = self.state.lock() else {
            return Report::unavailable();
        };
        while state.completion == Completion::Open {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let Ok((updated, timeout)) = self.changed.wait_timeout(state, remaining) else {
                return Report::unavailable();
            };
            state = updated;
            if timeout.timed_out() {
                break;
            }
        }
        let bytes: Vec<_> = state.bytes.iter().copied().collect();
        classify(&bytes, state.truncated, state.completion)
    }
}

/// Wrap only the actual stderr pipe; preserve every read result and its lifetime.
pub(super) struct Stderr<R> {
    input: R,
    capture: Arc<Capture>,
}
impl<R> Stderr<R> {
    pub(super) fn new(input: R, capture: Arc<Capture>) -> Self {
        Self { input, capture }
    }
}
impl<R: Read> Read for Stderr<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let result = self.input.read(output);
        match &result {
            Ok(0) if !output.is_empty() => self.capture.finish(Completion::Eof),
            Ok(count) => self.capture.append(&output[..*count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => self.capture.finish(Completion::ReadError),
        }
        result
    }
}
impl<R> Drop for Stderr<R> {
    fn drop(&mut self) {
        self.capture.unavailable();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Unknown,
    RustPanic,
    NonunwindingPanic,
    DoublePanic,
    StackOverflow,
    AllocationFailure,
    MallocAbort,
}
impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::RustPanic => "rust-panic",
            Self::NonunwindingPanic => "nonunwinding-panic",
            Self::DoublePanic => "double-panic",
            Self::StackOverflow => "stack-overflow",
            Self::AllocationFailure => "allocation-failure",
            Self::MallocAbort => "malloc-abort",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    StdPanicking,
    CorePanicking,
    StdRuntime,
    SourceMessageProcessor,
    SourceConfigManager,
    SourceAuthMember,
    SourceLifetime,
    SourceRetirement,
    SourceRetainedBackend,
}
impl Site {
    fn from_alias(path: &[u8]) -> Option<Self> {
        // Exact compile-time aliases only. Never copy a path or accept a suffix.
        match path {
            b"library/std/src/panicking.rs" => Some(Self::StdPanicking),
            b"library/core/src/panicking.rs" => Some(Self::CorePanicking),
            b"library/std/src/rt.rs" => Some(Self::StdRuntime),
            b"app-server/src/message_processor.rs" => Some(Self::SourceMessageProcessor),
            b"app-server/src/config_manager_service.rs" => Some(Self::SourceConfigManager),
            b"alab-core/glue/auth_native_member.rs" => Some(Self::SourceAuthMember),
            b"alab-core/src/native_source_lifetime.rs" => Some(Self::SourceLifetime),
            b"alab-core/src/native_source_retirement.rs" => Some(Self::SourceRetirement),
            b"alab-aterm/src/frontend_retained_backend.rs" => Some(Self::SourceRetainedBackend),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::StdPanicking => "std-panicking",
            Self::CorePanicking => "core-panicking",
            Self::StdRuntime => "std-runtime",
            Self::SourceMessageProcessor => "source-message-processor",
            Self::SourceConfigManager => "source-config-manager",
            Self::SourceAuthMember => "source-auth-member",
            Self::SourceLifetime => "source-lifetime",
            Self::SourceRetirement => "source-retirement",
            Self::SourceRetainedBackend => "source-retained-backend",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Location {
    site: Site,
    line: u32,
    column: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Report {
    kind: Kind,
    completion: Completion,
    truncated: bool,
    location: Option<Location>,
}
impl Report {
    fn unavailable() -> Self {
        Self {
            kind: Kind::Unknown,
            completion: Completion::Unavailable,
            truncated: false,
            location: None,
        }
    }
}
impl fmt::Display for Report {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        let capture = match self.completion {
            Completion::Open => "partial",
            Completion::Eof => "eof",
            Completion::ReadError => "read-error",
            Completion::Unavailable => "unavailable",
        };
        write!(
            output,
            "stderr-kind={} capture={capture} truncated={}",
            self.kind.label(),
            self.truncated,
        )?;
        match self.location {
            Some(location) => write!(
                output,
                " site={} line={} column={}",
                location.site.label(),
                location.line,
                location.column,
            ),
            None => write!(output, " site=unknown line=none column=none"),
        }
    }
}

fn coordinate(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > 7 || bytes[0] == b'0' {
        return None;
    }
    let mut value = 0_u32;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }
    (value <= MAX_COORDINATE).then_some(value)
}
fn panic_header(line: &[u8]) -> Option<&[u8]> {
    let location = if let Some(thread) = line.strip_prefix(b"thread '") {
        let marker = b" panicked at ";
        let index = thread
            .windows(marker.len())
            .rposition(|part| part == marker)?;
        let prefix = &thread[..index];
        if !prefix.ends_with(b"'") {
            let before_end = prefix.strip_suffix(b")")?;
            let start = before_end.windows(3).rposition(|part| part == b"' (")?;
            if !decimal(&before_end[start + 3..], 20) {
                return None;
            }
        }
        &thread[index + marker.len()..]
    } else if let Some(location) = line.strip_prefix(b"panicked at ") {
        location
    } else {
        line.strip_prefix(b"aborting due to panic at ")?
    };
    location.strip_suffix(b":")
}
fn location(bytes: &[u8]) -> Option<Location> {
    let column_separator = bytes.iter().rposition(|byte| *byte == b':')?;
    let column = coordinate(&bytes[column_separator + 1..])?;
    let rest = &bytes[..column_separator];
    let line_separator = rest.iter().rposition(|byte| *byte == b':')?;
    Some(Location {
        site: Site::from_alias(&rest[..line_separator])?,
        line: coordinate(&rest[line_separator + 1..])?,
        column,
    })
}
fn decimal(bytes: &[u8], maximum: usize) -> bool {
    !bytes.is_empty()
        && bytes.len() <= maximum
        && bytes[0] != b'0'
        && bytes.iter().all(u8::is_ascii_digit)
        && bytes
            .iter()
            .try_fold(0_u64, |value, byte| {
                value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
            })
            .is_some()
}
fn malloc_message(line: &[u8]) -> Option<&[u8]> {
    let opening = line.iter().position(|byte| *byte == b'(')?;
    let name = &line[..opening];
    if name.is_empty()
        || name.len() > 128
        || !name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(byte))
    {
        return None;
    }
    let rest = &line[opening + 1..];
    let closing = rest.iter().position(|byte| *byte == b')')?;
    let mut identity = rest[..closing].split(|byte| *byte == b',');
    if !decimal(identity.next()?, 10) {
        return None;
    }
    if let Some(thread) = identity.next() {
        let hex = thread.strip_prefix(b"0x")?;
        if hex.is_empty() || hex.len() > 16 || !hex.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
    }
    if identity.next().is_some() {
        return None;
    }
    rest[closing + 1..].strip_prefix(b" malloc: ")
}
fn classify(bytes: &[u8], truncated: bool, completion: Completion) -> Report {
    let bytes = if truncated {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |end| &bytes[end + 1..])
    } else {
        bytes
    };
    let mut panic = false;
    let mut first_location = None;
    let mut nonunwinding = false;
    let mut double = false;
    let mut stack = false;
    let mut allocation = false;
    let mut malloc_error = false;
    let mut malloc_breakpoint = false;
    for fragment in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line = if let Some(line) = fragment.strip_suffix(b"\n") {
            line
        } else if completion == Completion::Eof {
            fragment
        } else {
            break;
        };
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(header) = panic_header(line) {
            if !panic {
                first_location = location(header);
            }
            panic = true;
        }
        nonunwinding |= line == b"thread caused non-unwinding panic. aborting."
            || line == b"panic in a function that cannot unwind";
        double |= line == b"thread panicked while processing panic. aborting."
            || line == b"panic in a destructor during cleanup";
        stack |= line == b"fatal runtime error: stack overflow, aborting";
        if let Some(size) = line
            .strip_prefix(b"memory allocation of ")
            .and_then(|rest| rest.strip_suffix(b" bytes failed"))
        {
            allocation |= !size.is_empty() && size.iter().all(u8::is_ascii_digit);
        }
        if let Some(message) = malloc_message(line) {
            malloc_error |= message.starts_with(b"*** error for object ");
            malloc_breakpoint |= message == b"*** set a breakpoint in malloc_error_break to debug";
        }
    }
    let kind = if stack {
        Kind::StackOverflow
    } else if double {
        Kind::DoublePanic
    } else if nonunwinding {
        Kind::NonunwindingPanic
    } else if malloc_error && malloc_breakpoint {
        Kind::MallocAbort
    } else if allocation {
        Kind::AllocationFailure
    } else if panic {
        Kind::RustPanic
    } else {
        Kind::Unknown
    };
    Report {
        kind,
        completion,
        truncated,
        location: first_location,
    }
}

#[cfg(test)]
#[path = "codex_retained_diagnostics_tests.rs"]
mod tests;
