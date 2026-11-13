// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded successor progress: journal records for review and, when the outgoing process offers to
//! read them, live [`Report`]s on the readiness channel by which it judges the successor's progress
//! while every session is parked. Nothing here admits a handoff.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Waiting,
    #[cfg(unix)]
    Readers,
    #[cfg(unix)]
    Proof,
    #[cfg(unix)]
    CommitWaiter,
    #[cfg(unix)]
    Write,
    Written,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Gate fields are a snapshot at the start of one readiness visit. Later phase
/// records retain that snapshot; a concurrent control worker may already have advanced.
pub(crate) struct Record {
    pub phase: Phase,
    pub control_pending: bool,
    pub control_failed: bool,
    pub restore_pending: bool,
    pub manifest_windows: usize,
    pub pending_adoptions: usize,
    pub first_present_pending: bool,
    pub unpainted_windows: usize,
}

#[derive(Default)]
pub(crate) struct Progress {
    last: Option<Record>,
    emitted: u8,
    terminal: u8,
}

impl Progress {
    // An oscillating gate cannot fill the journal indefinitely. Terminal records retain
    // their own slots, so exhausting waiting records does not hide the observed outcome.
    const LIMIT: u8 = 64;

    pub fn note(&mut self, record: Record, emit: &mut impl FnMut(Record)) {
        if self.last == Some(record) {
            return;
        }
        self.last = Some(record);
        let terminal = match record.phase {
            Phase::Written => 1,
            Phase::Rejected => 2,
            _ => 0,
        };
        if terminal != 0 {
            if self.terminal & terminal == 0 {
                self.terminal |= terminal;
                emit(record);
            }
        } else if self.emitted < Self::LIMIT {
            self.emitted = self.emitted.saturating_add(1);
            emit(record);
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

fn identity() -> (u32, &'static Option<aterm_uds::process::ProcessStart>) {
    static START: std::sync::OnceLock<Option<aterm_uds::process::ProcessStart>> =
        std::sync::OnceLock::new();
    let pid = std::process::id();
    let start = START.get_or_init(|| {
        i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_process_start)
    });
    (pid, start)
}

pub(crate) fn log(record: Record) {
    let (pid, start) = identity();
    aterm_log::info!(
        "overlap handoff: progress pid={pid} start={start:?} build={} {record:?}",
        crate::build_info::build_number(),
    );
    #[cfg(unix)]
    report(record.phase.label());
}

pub(crate) fn milestone(stamp: crate::metrics::ClaimStamp) {
    let (pid, start) = identity();
    aterm_log::info!(
        "overlap handoff: milestone pid={pid} start={start:?} build={} stamp={}",
        crate::build_info::build_number(),
        stamp.as_str(),
    );
    #[cfg(unix)]
    report(stamp.as_str());
}

#[cfg(unix)]
impl Phase {
    const fn label(self) -> &'static str {
        match self {
            Self::Waiting => "gate:waiting",
            Self::Readers => "gate:readers",
            Self::Proof => "gate:proof",
            Self::CommitWaiter => "gate:commit_waiter",
            Self::Write => "gate:write",
            Self::Written => "gate:written",
            Self::Rejected => "gate:rejected",
        }
    }
}

/// Opens a progress report on the readiness channel; the adoption proof opens with `ASR1`.
#[cfg(unix)]
const REPORT_MAGIC: &[u8; 4] = b"ASP1";

/// Flag bit: this successor's event loop runs (winit's first `resumed` came, so its native launch
/// finished). A parent ignores flag bits it does not know.
#[cfg(unix)]
const REPORT_LAUNCHED: u8 = 1;

/// Flag bit: this successor presented its first window (`metrics::ClaimStamp::FirstPresent`), so
/// its native launch, its first window and surface and a first frame all ran: its bytes start
/// here. A parent ignores flag bits it does not know.
#[cfg(unix)]
const REPORT_PRESENTED: u8 = 2;

#[cfg(unix)]
const REPORT_LABEL_MAX: usize = crate::seamless::READY_WIRE_LEN - 6;

/// One progress report a successor writes on the readiness channel before its proof, when the
/// outgoing process offered to read them (`seamless::ENV_READY_CAPS`).
///
/// Wire: `ASP1`, a flag byte, a label length, then the label's ASCII bytes, zero-padded to the
/// proof's length. The label is diagnostic text (a [`crate::metrics::ClaimStamp`] or a gate
/// [`Phase`]); the parent's judgment reads only that a report arrived and its flags, launched and
/// presented.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    launched: bool,
    presented: bool,
    len: u8,
    label: [u8; REPORT_LABEL_MAX],
}

#[cfg(unix)]
impl Report {
    /// A report named `label` (its printable ASCII, cut at the wire's room).
    #[must_use]
    pub(crate) fn new(label: &str, launched: bool) -> Self {
        let mut bytes = [0u8; REPORT_LABEL_MAX];
        let mut len = 0usize;
        for (slot, byte) in bytes
            .iter_mut()
            .zip(label.bytes().filter(u8::is_ascii_graphic))
        {
            *slot = byte;
            len += 1;
        }
        Self {
            launched,
            presented: false,
            len: u8::try_from(len).unwrap_or(u8::MAX),
            label: bytes,
        }
    }

    /// This report, saying whether the successor had presented its first window when it wrote it.
    #[must_use]
    pub(crate) const fn with_presented(mut self, presented: bool) -> Self {
        self.presented = presented;
        self
    }

    /// A report named `label`, with this process's own launch and first present as they stand.
    #[must_use]
    fn now(label: &str) -> Self {
        Self::new(label, crate::metrics::launch_finished())
            .with_presented(crate::metrics::first_present_done())
    }

    /// Whether the successor's native launch had finished when it wrote this report.
    #[must_use]
    pub(crate) const fn launched(&self) -> bool {
        self.launched
    }

    /// Whether the successor had presented its first window when it wrote this report.
    #[must_use]
    pub(crate) const fn presented(&self) -> bool {
        self.presented
    }

    /// The report's label.
    #[must_use]
    pub(crate) fn label(&self) -> &str {
        std::str::from_utf8(&self.label[..usize::from(self.len)]).unwrap_or("")
    }

    #[must_use]
    pub(crate) fn to_wire(self) -> [u8; crate::seamless::READY_WIRE_LEN] {
        let mut wire = [0u8; crate::seamless::READY_WIRE_LEN];
        wire[..4].copy_from_slice(REPORT_MAGIC);
        wire[4] = (if self.launched { REPORT_LAUNCHED } else { 0 })
            | if self.presented { REPORT_PRESENTED } else { 0 };
        wire[5] = self.len;
        wire[6..].copy_from_slice(&self.label);
        wire
    }

    /// The report `wire` carries, or `None` for any other wire, the adoption proof among them.
    /// Unknown flag bits and label bytes outside printable ASCII are ignored, not refused: a later
    /// successor's report stays a report to this parent.
    #[must_use]
    pub(crate) fn from_wire(wire: &[u8; crate::seamless::READY_WIRE_LEN]) -> Option<Self> {
        if &wire[..4] != REPORT_MAGIC {
            return None;
        }
        let len = usize::from(wire[5]).min(REPORT_LABEL_MAX);
        let label = wire[6..6 + len]
            .iter()
            .map(|&byte| char::from(if byte.is_ascii_graphic() { byte } else { b'?' }))
            .collect::<String>();
        Some(
            Self::new(&label, wire[4] & REPORT_LAUNCHED != 0)
                .with_presented(wire[4] & REPORT_PRESENTED != 0),
        )
    }
}

/// The most reports one armed channel carries. With the claim report and the proof they fit in
/// 4096 bytes, below any pipe's buffer, so a report never blocks this process's main thread,
/// whether or not the outgoing process is reading.
#[cfg(unix)]
const MAX_REPORTS: u16 = 96;

#[cfg(unix)]
const _: () = assert!((MAX_REPORTS as usize + 2) * crate::seamless::READY_WIRE_LEN <= 4096);

// Before its proof a successor reports at most once `armed`, each milestone once, `resumed` once
// and the gate's bounded records (its progress is cleared only once the proof is written or the
// handoff is refused). The cap must sit above that, or it would silence a healthy successor that
// is still reporting and the outgoing process would end its attempt as stalled.
#[cfg(unix)]
const _: () = assert!(
    2 + crate::metrics::ClaimStamp::COUNT + Progress::LIMIT as usize + 2 < MAX_REPORTS as usize
);

/// Progress reports to one readiness channel, which the caller owns and keeps open while it holds
/// this.
#[cfg(unix)]
pub(crate) struct Reporter {
    fd: i32,
    sent: u16,
}

#[cfg(unix)]
impl Reporter {
    #[must_use]
    pub(crate) const fn on(fd: i32) -> Self {
        Self { fd, sent: 0 }
    }

    /// Write one report; `false` once the cap is spent or the channel refused a write, after which
    /// this reporter writes nothing more.
    pub(crate) fn send(&mut self, report: Report) -> bool {
        if self.sent >= MAX_REPORTS {
            return false;
        }
        self.sent += 1;
        let wire = report.to_wire();
        let written = loop {
            // SAFETY: one write of a fixed local buffer to a descriptor the caller keeps open. It
            // is at most PIPE_BUF bytes, so a pipe takes all of it or none.
            let wrote = unsafe { libc::write(self.fd, wire.as_ptr().cast(), wire.len()) };
            if wrote < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            break wrote;
        };
        if written == isize::try_from(wire.len()).unwrap_or(-1) {
            true
        } else {
            self.sent = MAX_REPORTS;
            false
        }
    }
}

#[cfg(unix)]
static ARMED: std::sync::Mutex<Option<Reporter>> = std::sync::Mutex::new(None);

/// Report progress on the readiness channel `fd` from now on, starting with an `armed` report. The
/// caller owns `fd` and calls [`disarm`] before closing it (`seamless::ReadySignal`'s drop).
#[cfg(unix)]
pub(crate) fn arm(fd: i32) {
    let mut reporter = Reporter::on(fd);
    if reporter.send(Report::now("armed")) {
        *ARMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reporter);
    }
}

/// Stop reporting on `fd`. A report in flight finishes first, so `fd` may be closed on return.
#[cfg(unix)]
pub(crate) fn disarm(fd: i32) {
    let mut armed = ARMED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if armed.as_ref().is_some_and(|reporter| reporter.fd == fd) {
        *armed = None;
    }
}

/// The readiness channel armed for reports, if any.
#[cfg(all(test, unix))]
pub(crate) fn armed_fd() -> Option<i32> {
    ARMED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|reporter| reporter.fd)
}

/// Report `label` on the armed readiness channel; nothing when none is armed.
#[cfg(unix)]
pub(crate) fn report(label: &str) {
    let mut armed = ARMED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(reporter) = armed.as_mut()
        && !reporter.send(Report::now(label))
    {
        *armed = None;
    }
}

/// One report on a channel not (yet) armed: the launched lane's claim, written the moment the
/// grant lands, before intake builds the readiness signal that arms the channel.
#[cfg(target_os = "macos")]
pub(crate) fn report_once(fd: std::os::fd::BorrowedFd<'_>, label: &str) -> bool {
    use std::os::fd::AsRawFd as _;
    Reporter::on(fd.as_raw_fd()).send(Report::now(label))
}

#[cfg(test)]
#[path = "handoff_progress_tests.rs"]
mod tests;
