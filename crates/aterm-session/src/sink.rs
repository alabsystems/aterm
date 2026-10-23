// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metered PTY write sink, input-hook wakeups, and backpressure accounting.
use std::collections::VecDeque;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::input_backlog::{InputBacklog, InputLedger};

/// Outcome of one bounded, immediate actuator egress attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImmediateWrite {
    Full,
    BusyZero,
    PartialInDoubt { accepted: usize },
    Queued { accepted: usize },
}

/// Opaque process-local version of all non-empty input attempts against one PTY sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEpoch(u64);

/// Opaque process-local version of the input someone drove into one PTY sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrivenInput(u64);

/// A watch on one sink's [`DrivenInput`] that holds no descriptor ([`SinkWriter::driven_input_watch`]).
#[derive(Clone, Debug)]
pub struct DrivenInputWatch(Arc<AtomicU64>);

impl DrivenInputWatch {
    /// The sink's [`DrivenInput`] now.
    #[must_use]
    #[inline]
    pub fn get(&self) -> DrivenInput {
        DrivenInput(self.0.load(Ordering::Acquire))
    }
}

/// Opaque process-local order of frames accepted by one PTY sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AcceptedOrder(u64);

/// Where one bulk frame is ([`BulkMeter::progress`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BulkState {
    Queued = 0,
    Writing = 1,
    Delivered = 2,
    Stopped = 3,
    Failed = 4,
}

/// One reading of a [`BulkMeter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BulkProgress {
    pub sent: u64,
    pub total: u64,
    pub state: BulkState,
}

/// Progress and a stop for one bulk frame, shared between its writer ([`SinkWriter::write_frame_metered_with_receipt`]) and watchers.
#[derive(Debug)]
pub struct BulkMeter {
    sent: AtomicU64,
    total: AtomicU64,
    stop: AtomicBool,
    state: std::sync::atomic::AtomicU8,
}

impl Default for BulkMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl BulkMeter {
    /// A meter for a frame not yet begun.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self {
            sent: AtomicU64::new(0),
            total: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            state: std::sync::atomic::AtomicU8::new(BulkState::Queued as u8),
        }
    }

    /// Ask the writer to drop what it has not yet sent (see [`SinkWriter::write_frame_metered_with_receipt`]).
    #[inline]
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        #[cfg(windows)]
        aterm_pty::release_stopped_writes(&self.stop);
    }

    /// Whether a stop was asked for.
    #[must_use]
    #[inline]
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// The frame's bytes sent, its size and its state, now.
    #[must_use]
    #[inline]
    pub fn progress(&self) -> BulkProgress {
        let state = match self.state.load(Ordering::Acquire) {
            1 => BulkState::Writing,
            2 => BulkState::Delivered,
            3 => BulkState::Stopped,
            4 => BulkState::Failed,
            _ => BulkState::Queued,
        };
        BulkProgress {
            sent: self.sent.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            state,
        }
    }

    /// Mark a frame the caller never handed to the sink as over.
    pub fn abandon(&self) {
        self.finish(BulkState::Stopped);
    }

    /// Settle a frame that was not written through [`MeteredSink::write_frame`].
    pub fn settle_unmetered(&self, delivered: bool) {
        self.finish(if delivered {
            BulkState::Delivered
        } else {
            BulkState::Failed
        });
    }

    fn finish(&self, state: BulkState) {
        self.state.store(state as u8, Ordering::Release);
    }

    fn note_sent(&self, sent: usize) {
        self.sent.store(sent as u64, Ordering::Relaxed);
    }

    fn note_spilled(meter: Option<&Self>, len: usize) {
        if let Some(meter) = meter {
            meter.note_sent(len);
        }
    }
}

const PASTE_OPEN: &[u8] = b"\x1b[200~";
const PASTE_CLOSE: &[u8] = b"\x1b[201~";

/// Where a bulk frame stopped at `off` resumes.
#[must_use]
pub fn bulk_stop_cut(bytes: &[u8], off: usize) -> (usize, usize) {
    let len = bytes.len();
    if off == 0 {
        return (0, len);
    }
    let mut keep_to = off.min(len);
    while bytes
        .get(keep_to)
        .is_some_and(|b| b & 0b1100_0000 == 0b1000_0000)
    {
        keep_to = keep_to.saturating_add(1);
    }
    let bracketed = len >= PASTE_OPEN.len().saturating_add(PASTE_CLOSE.len())
        && bytes.starts_with(PASTE_OPEN)
        && bytes.ends_with(PASTE_CLOSE);
    let tail_from = if bracketed {
        keep_to = keep_to.max(PASTE_OPEN.len());
        len.saturating_sub(PASTE_CLOSE.len())
    } else {
        len
    };
    (keep_to, tail_from.max(keep_to))
}

const METERED_SLICE: usize = 4 * 1024;

#[derive(Debug, Default, PartialEq, Eq)]
struct MeteredEnd {
    accepted: usize,
    owed: Vec<u8>,
}

fn write_metered(
    bytes: &[u8],
    meter: &BulkMeter,
    mut write: impl FnMut(&[u8]) -> io::Result<usize>,
) -> Result<MeteredEnd, (io::Error, usize)> {
    let len = bytes.len();
    let mut off = 0usize;
    let mut accepted = 0usize;
    let mut cut: Option<(usize, usize)> = None;
    while off < len {
        if cut.is_none() && meter.stop_requested() {
            cut = Some(bulk_stop_cut(bytes, off));
        }
        let limit = match cut {
            Some((keep_to, tail_from)) if off >= keep_to && off < tail_from => {
                off = tail_from;
                continue;
            }
            Some((keep_to, _)) if off < keep_to => keep_to,
            _ => len,
        };
        let Some(rest) = bytes.get(off..limit.min(off.saturating_add(METERED_SLICE))) else {
            break;
        };
        match write(rest) {
            Ok(0) => {
                let owed = if meter.stop_requested() {
                    let (keep_to, tail_from) = cut.unwrap_or_else(|| bulk_stop_cut(bytes, off));
                    let finish = bytes.get(off..keep_to.max(off)).unwrap_or_default();
                    let close = bytes.get(tail_from.max(off)..).unwrap_or_default();
                    [finish, close].concat()
                } else {
                    Vec::new()
                };
                return Ok(MeteredEnd { accepted, owed });
            }
            Ok(n) => {
                off = off.saturating_add(n);
                accepted = accepted.saturating_add(n);
                meter.note_sent(accepted);
            }
            Err(e) => return Err((e, accepted)),
        }
    }
    Ok(MeteredEnd {
        accepted,
        owed: Vec::new(),
    })
}

fn deliver_owed_through(owed: &[u8], mut write: impl FnMut(&[u8]) -> io::Result<usize>) -> usize {
    let mut off = 0;
    while let Some(rest) = owed.get(off..).filter(|rest| !rest.is_empty()) {
        match write(rest) {
            Ok(n) if n > 0 => off = off.saturating_add(n),
            _ => break,
        }
    }
    off
}

/// Result of a blocking or spill-tolerant sink write, with its accepted order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct WriteReceipt {
    accepted: usize,
    order: Option<AcceptedOrder>,
    direct: bool,
}

impl WriteReceipt {
    #[inline]
    fn new(accepted: usize, order: Option<AcceptedOrder>) -> Self {
        debug_assert_eq!(accepted == 0, order.is_none());
        Self {
            accepted,
            order,
            direct: false,
        }
    }

    #[inline]
    const fn completed(accepted: usize, intended: usize, order: AcceptedOrder) -> Self {
        Self {
            accepted,
            order: if accepted > 0 { Some(order) } else { None },
            direct: accepted > 0 && accepted == intended,
        }
    }

    /// Whether the whole frame reached the kernel directly (see the field).
    #[must_use]
    #[inline]
    pub const fn is_direct(self) -> bool {
        self.direct
    }

    /// Number of bytes accepted by this call.
    #[must_use]
    #[inline]
    pub const fn accepted(self) -> usize {
        self.accepted
    }

    /// Accepted serialization/spill order, or `None` when no bytes were accepted.
    #[must_use]
    #[inline]
    pub const fn order(self) -> Option<AcceptedOrder> {
        self.order
    }
}

/// A sink write error with any prefix accepted before it.
#[derive(Debug)]
pub struct WriteReceiptError {
    error: io::Error,
    receipt: WriteReceipt,
    refused: bool,
}

impl WriteReceiptError {
    fn new(error: io::Error, accepted: usize, order: Option<AcceptedOrder>) -> Self {
        Self {
            error,
            receipt: WriteReceipt::new(accepted, order),
            refused: false,
        }
    }

    fn after_write(error: io::Error, accepted: usize, order: AcceptedOrder) -> Self {
        Self::new(error, accepted, (accepted > 0).then_some(order))
    }

    #[cfg(unix)]
    fn refused(accepted: usize, order: Option<AcceptedOrder>) -> Self {
        Self {
            error: io::Error::new(
                io::ErrorKind::WouldBlock,
                "input queue full: the program is not reading its input",
            ),
            receipt: WriteReceipt::new(accepted, order),
            refused: true,
        }
    }

    #[cfg(unix)]
    fn no_drainer(accepted: usize, order: Option<AcceptedOrder>) -> Self {
        Self::new(
            io::Error::other("input could not be queued: no spill drainer could be arranged"),
            accepted,
            order,
        )
    }

    /// Whether the frame was refused so the calling (UI) thread would not park (the program is not reading and the input queue is full).
    #[must_use]
    #[inline]
    pub const fn is_refused(&self) -> bool {
        self.refused
    }

    /// Number of bytes accepted before the error.
    #[must_use]
    #[cfg(test)]
    #[inline]
    pub const fn accepted(&self) -> usize {
        self.receipt.accepted()
    }

    /// Accepted serialization/spill order, or `None` when no bytes moved.
    #[must_use]
    #[inline]
    pub const fn order(&self) -> Option<AcceptedOrder> {
        self.receipt.order()
    }

    /// Recover the original I/O error without reclassification or wrapping.
    #[must_use]
    #[inline]
    pub fn into_error(self) -> io::Error {
        self.error
    }
}

impl From<io::Error> for WriteReceiptError {
    fn from(error: io::Error) -> Self {
        Self::new(error, 0, None)
    }
}

impl std::fmt::Display for WriteReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for WriteReceiptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Why [`SinkWriter::discard_unread_input`] drops the input the program left unread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discard {
    Restart,
    Flush,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortWrite {
    InDoubt,
    Queue,
}

/// A point inside the sink where a test stands a racing thread ([`SinkWriter::seam_for_testing`]).
#[cfg(all(unix, any(test, debug_assertions)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seam {
    DiscardFlush,
    DrainWrite,
    FrameLock,
    WriterPark,
    DrainPark,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhenFull {
    Park,
    Refuse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Landing {
    Driven,
    Answer,
}

#[derive(Clone, Copy)]
#[cfg_attr(windows, allow(dead_code))]
enum Room<'a> {
    Wait(Option<&'a BulkMeter>),
    NoWait,
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(windows, allow(dead_code))]
enum Spilled {
    Accepted(AcceptedOrder),
    Stopped,
    NoDrainer,
}

/// [`SinkWriter::try_fence_egress`]'s hold.
#[must_use = "the fence lifts when dropped"]
pub struct EgressFence<'a> {
    _frames: std::sync::MutexGuard<'a, ()>,
}

impl std::fmt::Debug for EgressFence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressFence").finish_non_exhaustive()
    }
}

/// The byte budget of one sink's reply queue.
pub const REPLY_BUDGET_BYTES: usize = 1024 * 1024;

/// Bytes one queued reply holds against its sink's [`REPLY_BUDGET_BYTES`], from [`ReplyBudget::try_reserve`].
#[must_use = "a permit releases its bytes when dropped"]
pub struct ReplyPermit {
    shared: Arc<Shared>,
    bytes: usize,
}

impl std::fmt::Debug for ReplyPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyPermit")
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl Drop for ReplyPermit {
    fn drop(&mut self) {
        self.shared
            .reply_reserved
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// One sink's reply budget, apart from the sink ([`SinkWriter::reply_budget`]).
#[derive(Clone)]
pub struct ReplyBudget {
    shared: Arc<Shared>,
}

impl ReplyBudget {
    /// Reserve `bytes` of the reply budget for one reply about to be queued, or `None` when full.
    #[must_use]
    pub fn try_reserve(&self, bytes: usize) -> Option<ReplyPermit> {
        if self.shared.is_severed() {
            return None;
        }
        self.shared
            .reply_reserved
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                let next = held.checked_add(bytes)?;
                (held == 0 || next <= REPLY_BUDGET_BYTES).then_some(next)
            })
            .ok()?;
        Some(ReplyPermit {
            shared: Arc::clone(&self.shared),
            bytes,
        })
    }
}

impl std::fmt::Debug for ReplyBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyBudget").finish_non_exhaustive()
    }
}

static OWNED_MASTERS: AtomicUsize = AtomicUsize::new(0);

/// The number of PTY masters owned by live sinks in this process.
#[must_use]
pub fn owned_masters_open() -> usize {
    OWNED_MASTERS.load(Ordering::Acquire)
}

struct CountedMaster<T>(Option<T>);

impl<T> CountedMaster<T> {
    fn new(master: T) -> Self {
        OWNED_MASTERS.fetch_add(1, Ordering::AcqRel);
        Self(Some(master))
    }
}

impl<T> Drop for CountedMaster<T> {
    fn drop(&mut self) {
        drop(self.0.take());
        OWNED_MASTERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The one place bytes enter a session's PTY master fd.
pub struct SinkWriter {
    master: i32,
    #[cfg(unix)]
    _owned: Option<CountedMaster<OwnedFd>>,
    #[cfg(windows)]
    _owned: Option<CountedMaster<aterm_pty::OwnedMaster>>,
    ordered_egress_pending: AtomicUsize,
    driven_input: Arc<AtomicU64>,
    shared: Arc<Shared>,
}

#[cfg_attr(trust_verify, trust::paired)]
struct Shared {
    lock: Mutex<()>,
    input_epoch: AtomicU64,
    order_state: AtomicU64,
    spilled_bytes: AtomicUsize,
    cached_tty_swallows: AtomicU8,
    spill: Mutex<Spill>,
    drained: Condvar,
    arranger: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    ledger: Mutex<InputLedger>,
    input_hook: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    input_hook_armed: AtomicBool,
    input_hook_driven: AtomicBool,
    discards: AtomicU64,
    restarts: AtomicU64,
    severed: AtomicBool,
    reply_reserved: AtomicUsize,
    master_nonblocking: AtomicBool,
    #[cfg(all(unix, any(test, debug_assertions)))]
    seam: std::sync::OnceLock<Box<dyn Fn(Seam) + Send + Sync>>,
}

struct Spill {
    buf: VecDeque<u8>,
    chunk_written: usize,
    draining: bool,
    #[cfg(unix)]
    arrange_pending: bool,
    failed: bool,
}

impl SinkWriter {
    /// Largest frame admitted by [`Self::try_write_frame_immediate`].
    pub const IMMEDIATE_FRAME_MAX: usize = 16 * 1024 + 32;

    /// Wrap a borrowed PTY master fd.
    #[must_use]
    pub fn new(master: i32) -> Self {
        Self {
            master,
            _owned: None,
            ordered_egress_pending: AtomicUsize::new(0),
            driven_input: Arc::new(AtomicU64::new(0)),
            shared: Arc::new(Shared::new()),
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "fd_lifecycle",
            action = "Clone",
            reason = "RAII, no aterm method to anchor: the Clone action is std `Arc::clone(&sink)` \
                      taken by each holder (the reader thread, each window mirror, each in-flight \
                      control verb). It only increments the live strong count — there is no \
                      SinkWriter method to bind a #[refines] to. The fd-USE this clone authorizes \
                      IS modeled+anchored (UseFd -> master()/write_frame). Waived so the model's \
                      Clone action is covered (Ob.3) without inventing a no-op wrapper."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "fd_lifecycle",
            action = "DropClone",
            reason = "RAII, no aterm method to anchor: the DropClone action is the std `Drop` of an \
                      `Arc<SinkWriter>` clone; THE FIX is that the held `OwnedFd` (this field) closes \
                      the fd EXACTLY when the LAST clone drops (sink.rs:32-39, exercised by the \
                      `owned_fd_stays_open_until_last_clone_drops` regression). The close is \
                      `OwnedFd::drop`, not an aterm method, so there is nothing to #[refines]. Waived \
                      so the model's DropClone action is covered (Ob.3)."
        )
    )]
    #[must_use]
    #[cfg(unix)]
    pub fn new_owned(master: OwnedFd) -> Self {
        Self {
            master: master.as_raw_fd(),
            _owned: Some(CountedMaster::new(master)),
            ordered_egress_pending: AtomicUsize::new(0),
            driven_input: Arc::new(AtomicU64::new(0)),
            shared: Arc::new(Shared::new()),
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "fd_lifecycle",
            action = "Clone",
            reason = "RAII, no aterm method to anchor (Windows twin of the unix waiver): the Clone \
                      action is std `Arc::clone(&sink)` taken by each holder (the reader thread, \
                      each window mirror, each in-flight control verb). It only increments the \
                      live strong count — there is no SinkWriter method to bind a #[refines] to. \
                      The fd-USE this clone authorizes IS modeled+anchored (UseFd -> \
                      master()/write_frame). Waived so the model's Clone action is covered (Ob.3) \
                      without inventing a no-op wrapper."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "fd_lifecycle",
            action = "DropClone",
            reason = "RAII, no aterm method to anchor (Windows twin of the unix waiver): the \
                      DropClone action is the std `Drop` of an `Arc<SinkWriter>` clone; the held \
                      `OwnedMaster` (this field) closes the ConPTY session EXACTLY when the LAST \
                      clone drops — the same close-on-last-drop discipline as the unix `OwnedFd`. \
                      The close is `OwnedMaster::drop`, not an aterm method, so there is nothing \
                      to #[refines]. Waived so the model's DropClone action is covered (Ob.3)."
        )
    )]
    #[must_use]
    #[cfg(windows)]
    pub fn new_owned(master: aterm_pty::OwnedMaster) -> Self {
        Self {
            master: master.as_raw(),
            _owned: Some(CountedMaster::new(master)),
            ordered_egress_pending: AtomicUsize::new(0),
            driven_input: Arc::new(AtomicU64::new(0)),
            shared: Arc::new(Shared::new()),
        }
    }

    /// Projects the sink onto `fd_lifecycle_model`'s `<<fdOpen, hasOwner>>`, the witness the `UseFd` anchors on `master`/`write_frame` name.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn project_fd_state(&self) -> (bool, bool) {
        (self.master != -1, self._owned.is_some())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "fd_lifecycle",
            action = "UseFd",
            project = "aterm_session::sink::SinkWriter::project_fd_state"
        )
    )]
    #[must_use]
    pub fn master(&self) -> i32 {
        self.master
    }

    /// Jobs submitted to this sink's ordered input writer and not yet completed.
    #[inline]
    #[must_use]
    pub fn ordered_egress_count(&self) -> usize {
        self.ordered_egress_pending.load(Ordering::Acquire)
    }

    /// Claim a FIFO slot before the job is sent to the writer.
    pub fn claim_ordered_egress(&self) {
        self.ordered_egress_pending.fetch_add(1, Ordering::AcqRel);
    }

    /// Release the slot after the writer has completed its PTY write, or when submission failed and the caller is taking back the event.
    pub fn retire_ordered_egress(&self) {
        let previous = self.ordered_egress_pending.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "ordered egress retired without a claim");
    }

    /// What the line discipline will do with the next byte typed into this sink.
    #[must_use]
    pub fn tty_echo(&self) -> Option<aterm_pty::TtyEcho> {
        aterm_pty::tty_echo(self.master)
    }

    /// Cache a caller-observed `tty_swallows_input` hint so [`Self::tty_swallows_input`] can answer without a `tcgetattr` syscall on every keystroke.
    pub fn set_tty_swallows_hint(&self, swallows: bool) {
        self.shared
            .cached_tty_swallows
            .store(if swallows { 2 } else { 1 }, Ordering::Release);
    }

    /// Whether the line discipline will swallow the next byte typed into this sink.
    #[must_use]
    pub fn tty_swallows_input(&self) -> bool {
        match self.shared.cached_tty_swallows.load(Ordering::Acquire) {
            1 => false,
            2 => true,
            _ => self
                .tty_echo()
                .is_some_and(aterm_pty::TtyEcho::swallows_input),
        }
    }

    /// How much input the program at this PTY has not read.
    #[must_use]
    pub fn input_backlog(&self) -> Option<InputBacklog> {
        self.input_backlog_at(Instant::now())
    }

    /// [`Self::input_backlog`] as of `now`, which the caller reads first (step 1).
    #[must_use]
    pub fn input_backlog_at(&self, now: Instant) -> Option<InputBacklog> {
        let echo = self.tty_echo()?;
        let queued = aterm_pty::input_queue_len(self.master)?;
        let oldest = self
            .shared
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .oldest_unread_at(queued);
        let wait = oldest.map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        Some(InputBacklog {
            queued,
            spilled: self.shared.try_spill_len(),
            wait,
            canonical: echo.canonical,
            signals: echo.signals,
            output_backlog: aterm_pty::output_queue_len(self.master),
        })
    }

    /// Install the input hook.
    pub fn install_input_hook(&self, hook: impl Fn() + Send + Sync + 'static) {
        let _ = self.shared.input_hook.set(Box::new(hook));
    }

    /// Let the input hook fire again on the next write.
    pub fn rearm_input_hook(&self) {
        self.shared.input_hook_armed.store(false, Ordering::SeqCst);
    }

    /// Whether anything but the terminal's own answer to its program's query ([`Self::write_reply`]) put bytes in the input queue.
    pub fn take_input_hook_driven(&self) -> bool {
        self.shared.input_hook_driven.swap(false, Ordering::SeqCst)
    }

    /// Fire the registered input hook at most once until the next re-arm.
    pub fn wake_input_hook(&self) -> bool {
        self.shared.fire_input_hook()
    }

    /// Discard every input byte the program has not read.
    #[cfg(unix)]
    pub fn discard_unread_input(&self, why: Discard) -> Option<usize> {
        aterm_pty::input_queue_len(self.master)?;
        let (epoch, dropped) = {
            let mut s = self.shared.spill.lock().unwrap_or_else(|p| p.into_inner());
            let epoch = self
                .shared
                .discards
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            if why == Discard::Restart {
                self.shared.restarts.fetch_add(1, Ordering::AcqRel);
            }
            let unsent = s.buf.len().saturating_sub(s.chunk_written);
            s.buf.clear();
            s.chunk_written = 0;
            self.shared.spilled_bytes.store(0, Ordering::Release);
            self.shared
                .order_state
                .fetch_and(!Shared::SPILL_ACTIVE_BIT, Ordering::Release);
            #[cfg(all(unix, any(test, debug_assertions)))]
            self.shared.at_seam(Seam::DiscardFlush);
            let flushed = aterm_pty::flush_input_queue(self.master).unwrap_or(0);
            (epoch, flushed.saturating_add(unsent))
        };
        self.shared.drained.notify_all();
        self.shared
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .mark_discard(epoch);
        Some(dropped)
    }

    /// Whether the program has read input since the last discard ([`Self::discard_unread_input`]).
    #[must_use]
    pub fn read_since_discard(&self) -> bool {
        if aterm_pty::tty_passes_every_byte(self.master) != Some(true) {
            return false;
        }
        let epoch = self.shared.discard_epoch();
        let after = self
            .shared
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .accepted_since_discard(epoch);
        let Some(after) = after.filter(|n| *n > 0) else {
            return false;
        };
        let Some(queued) = aterm_pty::input_queue_len(self.master) else {
            return false;
        };
        u64::try_from(queued).unwrap_or(u64::MAX) < after && !self.shared.discarded_since(epoch)
    }

    /// How many times [`Self::discard_unread_input`] has dropped unread input.
    #[must_use]
    pub fn discards(&self) -> u64 {
        self.shared.discard_epoch()
    }

    /// How many of [`Self::discards`] were a restart's drop ([`Discard::Restart`], before `signal term|kill|hup|quit`).
    #[must_use]
    pub fn restart_discards(&self) -> u64 {
        self.shared.restarts.load(Ordering::Acquire)
    }

    /// Sever this sink's input for good.
    pub fn sever_input(&self) {
        {
            let mut s = self.shared.spill.lock().unwrap_or_else(|p| p.into_inner());
            self.shared.severed.store(true, Ordering::Release);
            if !s.buf.is_empty() {
                s.failed = true;
            }
            s.buf.clear();
            s.chunk_written = 0;
            self.shared.spilled_bytes.store(0, Ordering::Release);
            self.shared
                .order_state
                .fetch_and(!Shared::SPILL_ACTIVE_BIT, Ordering::Release);
        }
        self.shared.drained.notify_all();
        #[cfg(windows)]
        aterm_pty::release_input_writes(self.master);
    }

    /// Whether [`Self::sever_input`] has run.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn is_severed(&self) -> bool {
        self.shared.is_severed()
    }

    /// Date every input byte this sink has handed the kernel `by` earlier, so a test can stand at the stall threshold without sleeping.
    #[cfg(any(test, debug_assertions))]
    pub fn backdate_input_for_testing(&self, by: Duration) {
        self.shared
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .backdate(by);
    }

    /// Run `hook` at each [`Seam`] this sink passes.
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub fn seam_for_testing(&self, hook: impl Fn(Seam) + Send + Sync + 'static) {
        let _ = self.shared.seam.set(Box::new(hook));
    }

    /// This sink's reply budget ([`ReplyBudget`], [`REPLY_BUDGET_BYTES`]).
    #[must_use]
    pub fn reply_budget(&self) -> ReplyBudget {
        ReplyBudget {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Bytes the reply queue holds reserved right now (queued or in flight).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn reply_reserved(&self) -> usize {
        self.shared.reply_reserved.load(Ordering::Acquire)
    }

    /// Declare whether this sink's master file description carries `O_NONBLOCK`.
    pub fn note_master_nonblocking(&self, nonblocking: bool) {
        self.shared
            .master_nonblocking
            .store(nonblocking, Ordering::Relaxed);
    }

    /// Whether this sink's process-local egress buffer is drained to the kernel.
    #[must_use]
    pub fn egress_drained_to_kernel(&self) -> bool {
        self.shared.spill_is_empty()
    }

    /// Stop this sink's egress at a frame boundary, without waiting.
    #[must_use]
    pub fn try_fence_egress(&self) -> Option<EgressFence<'_>> {
        let frames = match self.shared.lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        self.shared
            .spill_is_empty()
            .then_some(EgressFence { _frames: frames })
    }

    /// How many bytes the process-local spill holds right now.
    #[cfg(all(unix, any(test, debug_assertions)))]
    #[must_use]
    pub fn spilled_bytes(&self) -> usize {
        self.shared.spill_len()
    }

    /// The spill's cap.
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub const SPILL_CAP: usize = Shared::SPILL_CAP;

    /// Block an expendable producer until every process-local spill byte has reached the kernel or the drainer has seen a dead peer.
    #[must_use]
    pub fn wait_egress_drained_to_kernel(&self) -> bool {
        let mut spill = self
            .shared
            .spill
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while spill.draining && !spill.buf.is_empty() {
            spill = self
                .shared
                .drained
                .wait(spill)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        spill.buf.is_empty() && !spill.failed
    }

    /// [`Self::wait_egress_drained_to_kernel`] for at most `limit`.
    #[must_use]
    pub fn wait_egress_drained_to_kernel_for(&self, limit: Duration) -> Option<bool> {
        let deadline = Instant::now().checked_add(limit);
        let mut spill = self
            .shared
            .spill
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while spill.draining && !spill.buf.is_empty() {
            let left = deadline.map_or(limit, |at| at.saturating_duration_since(Instant::now()));
            if left.is_zero() {
                return None;
            }
            spill = self
                .shared
                .drained
                .wait_timeout(spill, left)
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
        Some(spill.buf.is_empty() && !spill.failed)
    }

    /// Non-parking observation of [`Self::egress_drained_to_kernel`].
    #[must_use]
    pub fn try_egress_drained_to_kernel(&self) -> Option<bool> {
        match self.shared.spill.try_lock() {
            Ok(spill) => Some(spill.buf.is_empty()),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                Some(poisoned.into_inner().buf.is_empty())
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// Install the off-thread drainer arranger (`Shared::arranger`).
    pub fn install_spill_arranger(&self, arranger: impl Fn() + Send + Sync + 'static) {
        let _ = self.shared.arranger.set(Box::new(arranger));
    }

    /// Worker-side half of [`Self::install_spill_arranger`].
    #[cfg(unix)]
    pub fn arrange_pending_drainer(&self) {
        let mut s = self.shared.spill.lock().unwrap_or_else(|p| p.into_inner());
        if !s.arrange_pending {
            return;
        }
        s.arrange_pending = false;
        if !self.shared.arrange_drainer(self.master, &mut s) {
            s.draining = false;
        }
    }

    /// Windows twin: no spill drainer exists, nothing is ever pending.
    #[cfg(not(unix))]
    pub fn arrange_pending_drainer(&self) {}

    /// Current attempted-input epoch for this sink.
    #[must_use]
    pub fn input_epoch(&self) -> InputEpoch {
        InputEpoch(self.shared.input_epoch.load(Ordering::Acquire))
    }

    /// The input driven into this sink so far ([`DrivenInput`]).
    #[must_use]
    pub fn driven_input(&self) -> DrivenInput {
        DrivenInput(self.driven_input.load(Ordering::Acquire))
    }

    /// A watch on [`Self::driven_input`] that holds no descriptor and keeps nothing of the session alive but the count.
    #[must_use]
    pub fn driven_input_watch(&self) -> DrivenInputWatch {
        DrivenInputWatch(Arc::clone(&self.driven_input))
    }

    fn reserve_input_attempt(&self) {
        self.driven_input.fetch_add(1, Ordering::AcqRel);
        self.reserve_attempt();
    }

    fn reserve_attempt(&self) {
        let _ = self
            .shared
            .input_epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            });
    }

    /// Hand one bounded frame to the kernel now, without parking and without queueing anything for the drainer.
    #[cfg(unix)]
    pub fn try_write_frame_immediate(&self, bytes: &[u8]) -> ImmediateWrite {
        self.try_write_frame_immediate_with_receipt(bytes).0
    }

    /// Receipt-bearing twin of [`Self::try_write_frame_immediate`].
    #[cfg(unix)]
    pub fn try_write_frame_immediate_with_receipt(
        &self,
        bytes: &[u8],
    ) -> (ImmediateWrite, Option<AcceptedOrder>) {
        if bytes.is_empty() {
            return (ImmediateWrite::Full, None);
        }
        self.reserve_input_attempt();
        self.try_write_frame_immediate_reserved(bytes, ShortWrite::InDoubt)
    }

    /// THE GUARDED WRITE.
    #[cfg(unix)]
    pub fn try_write_frame_whole_with_receipt(
        &self,
        bytes: &[u8],
    ) -> (ImmediateWrite, Option<AcceptedOrder>) {
        if bytes.is_empty() {
            return (ImmediateWrite::Full, None);
        }
        self.reserve_input_attempt();
        self.try_write_frame_immediate_reserved(bytes, ShortWrite::Queue)
    }

    /// Windows fail-closed twin of [`Self::try_write_frame_whole_with_receipt`].
    #[cfg(windows)]
    pub fn try_write_frame_whole_with_receipt(
        &self,
        bytes: &[u8],
    ) -> (ImmediateWrite, Option<AcceptedOrder>) {
        self.try_write_frame_immediate_with_receipt(bytes)
    }

    #[cfg(unix)]
    fn try_write_frame_immediate_reserved(
        &self,
        bytes: &[u8],
        short: ShortWrite,
    ) -> (ImmediateWrite, Option<AcceptedOrder>) {
        if bytes.len() > Self::IMMEDIATE_FRAME_MAX || self.shared.is_severed() {
            return (ImmediateWrite::BusyZero, None);
        }
        if !self.shared.master_nonblocking.load(Ordering::Relaxed) {
            return (ImmediateWrite::BusyZero, None);
        }

        let fd_guard = match self.shared.lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return (ImmediateWrite::BusyZero, None);
            }
        };
        let spill_guard = match self.shared.spill.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return (ImmediateWrite::BusyZero, None);
            }
        };
        if !spill_guard.buf.is_empty() {
            return (ImmediateWrite::BusyZero, None);
        }
        let Ok(order) = self.shared.next_accepted_order(false) else {
            return (ImmediateWrite::BusyZero, None);
        };

        let mut spill_guard = spill_guard;
        let mut poke = false;
        let result = match self.shared.ledgered_nonparking(self.master, bytes, false) {
            aterm_pty::NonParkWrite::Wrote(n) if n == bytes.len() => ImmediateWrite::Full,
            aterm_pty::NonParkWrite::Wrote(n) if n > 0 && short == ShortWrite::Queue => {
                match bytes.get(n..).and_then(|rest| {
                    self.shared
                        .queue_rest_held(self.master, &mut spill_guard, rest)
                }) {
                    Some(arrange) => {
                        poke = arrange;
                        ImmediateWrite::Queued { accepted: n }
                    }
                    None => ImmediateWrite::PartialInDoubt { accepted: n },
                }
            }
            aterm_pty::NonParkWrite::Wrote(n) => ImmediateWrite::PartialInDoubt { accepted: n },
            aterm_pty::NonParkWrite::Closed
            | aterm_pty::NonParkWrite::WouldBlock
            | aterm_pty::NonParkWrite::Fatal(_) => ImmediateWrite::BusyZero,
        };
        let accepted = match result {
            ImmediateWrite::Full => bytes.len(),
            ImmediateWrite::PartialInDoubt { accepted } | ImmediateWrite::Queued { accepted } => {
                accepted
            }
            ImmediateWrite::BusyZero => 0,
        };
        drop(spill_guard);
        if poke && let Some(arranger) = self.shared.arranger.get() {
            arranger();
        }
        drop(fd_guard);
        if accepted > 0 {
            let _ = self.shared.fire_input_hook();
        }
        (result, (accepted > 0).then_some(order))
    }

    /// Windows fail-closed twin of [`Self::try_write_frame_immediate`].
    #[cfg(windows)]
    pub fn try_write_frame_immediate(&self, bytes: &[u8]) -> ImmediateWrite {
        self.try_write_frame_immediate_with_receipt(bytes).0
    }

    /// Windows fail-closed receipt-bearing twin.
    #[cfg(windows)]
    pub fn try_write_frame_immediate_with_receipt(
        &self,
        bytes: &[u8],
    ) -> (ImmediateWrite, Option<AcceptedOrder>) {
        if bytes.is_empty() {
            return (ImmediateWrite::Full, None);
        }
        self.reserve_input_attempt();
        (ImmediateWrite::BusyZero, None)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "fd_lifecycle",
            action = "UseFd",
            project = "aterm_session::sink::SinkWriter::project_fd_state"
        )
    )]
    pub fn write_frame(&self, bytes: &[u8]) -> io::Result<usize> {
        self.write_frame_with_receipt(bytes)
            .map(WriteReceipt::accepted)
            .map_err(WriteReceiptError::into_error)
    }

    /// Receipt-bearing twin of [`Self::write_frame`].
    pub fn write_frame_with_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        if bytes.is_empty() {
            return Ok(WriteReceipt::new(0, None));
        }
        self.reserve_input_attempt();
        self.write_frame_after_reserve(bytes, None)
    }

    /// [`Self::write_frame_with_receipt`] for one bulk frame, reporting progress into `meter` and honouring its stop.
    pub fn write_frame_metered_with_receipt(
        &self,
        bytes: &[u8],
        meter: &BulkMeter,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        meter.total.store(bytes.len() as u64, Ordering::Relaxed);
        if bytes.is_empty() || meter.stop_requested() {
            meter.finish(BulkState::Stopped);
            return Ok(WriteReceipt::new(0, None));
        }
        meter
            .state
            .store(BulkState::Writing as u8, Ordering::Release);
        let since = self.shared.discard_epoch();
        self.reserve_input_attempt();
        let result = self.write_frame_after_reserve(bytes, Some(meter));
        let sent = meter.sent.load(Ordering::Relaxed);
        meter.finish(match &result {
            Err(_) => BulkState::Failed,
            Ok(receipt) if receipt.accepted() == bytes.len() => BulkState::Delivered,
            Ok(_) if meter.stop_requested() || self.shared.discarded_since(since) => {
                BulkState::Stopped
            }
            Ok(_) => BulkState::Failed,
        });
        debug_assert!(sent <= bytes.len() as u64);
        result
    }

    fn write_frame_after_reserve(
        &self,
        bytes: &[u8],
        meter: Option<&BulkMeter>,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        if self.shared.is_severed() {
            return Err(Shared::severed_error().into());
        }
        let since = self.shared.discard_epoch();
        aterm_pty::settle_input(self.master);
        if !self.shared.spill_is_empty() {
            match self
                .shared
                .spill_append(self.master, bytes, Room::Wait(meter), since)?
            {
                Spilled::Accepted(order) => {
                    BulkMeter::note_spilled(meter, bytes.len());
                    return Ok(WriteReceipt::new(bytes.len(), Some(order)));
                }
                Spilled::Stopped => return Ok(WriteReceipt::new(0, None)),
                Spilled::NoDrainer => {}
            }
        }
        #[cfg(all(unix, any(test, debug_assertions)))]
        self.shared.at_seam(Seam::FrameLock);
        let guard = self
            .shared
            .lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(order) = self.shared.direct_order_if_spill_empty()? else {
            drop(guard);
            match self
                .shared
                .spill_append(self.master, bytes, Room::Wait(meter), since)?
            {
                Spilled::Accepted(order) => {
                    BulkMeter::note_spilled(meter, bytes.len());
                    return Ok(WriteReceipt::new(bytes.len(), Some(order)));
                }
                Spilled::Stopped => return Ok(WriteReceipt::new(0, None)),
                Spilled::NoDrainer => return self.write_frame_locked(bytes, meter, since),
            }
        };
        self.write_frame_body_locked(guard, bytes, order, meter, since)
    }

    fn write_frame_body_locked(
        &self,
        guard: std::sync::MutexGuard<'_, ()>,
        bytes: &[u8],
        order: AcceptedOrder,
        meter: Option<&BulkMeter>,
        since: u64,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        let Some(meter) = meter else {
            let mut off = 0;
            while off < bytes.len() {
                let Some(rest) = bytes.get(off..) else { break };
                match self
                    .shared
                    .write_stamped_blocking(self.master, rest, since, None)
                {
                    Ok(0) => break,
                    Ok(n) => off = off.saturating_add(n),
                    Err(e) => return Err(WriteReceiptError::after_write(e, off, order)),
                }
            }
            drop(guard);
            return Ok(WriteReceipt::completed(off, bytes.len(), order));
        };
        let written = write_metered(bytes, meter, |rest| {
            self.shared
                .write_stamped_blocking(self.master, rest, since, Some(meter))
        })
        .map(|end| {
            let accepted = end
                .accepted
                .saturating_add(self.deliver_owed_locked(&end.owed, since));
            meter.note_sent(accepted);
            accepted
        });
        drop(guard);
        match written {
            Ok(accepted) => Ok(WriteReceipt::completed(accepted, bytes.len(), order)),
            Err((e, accepted)) => Err(WriteReceiptError::after_write(e, accepted, order)),
        }
    }

    fn deliver_owed_locked(&self, owed: &[u8], since: u64) -> usize {
        if owed.is_empty() {
            return 0;
        }
        if self.shared.prepend_owed(self.master, owed, since) {
            return owed.len();
        }
        deliver_owed_through(owed, |rest| {
            self.shared
                .write_stamped_blocking(self.master, rest, since, None)
        })
    }

    fn write_frame_locked(
        &self,
        bytes: &[u8],
        meter: Option<&BulkMeter>,
        since: u64,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        loop {
            let guard = self
                .shared
                .lock
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let Some(order) = self.shared.direct_order_if_spill_empty()? else {
                drop(guard);
                match self
                    .shared
                    .spill_append(self.master, bytes, Room::Wait(meter), since)?
                {
                    Spilled::Accepted(order) => {
                        BulkMeter::note_spilled(meter, bytes.len());
                        return Ok(WriteReceipt::new(bytes.len(), Some(order)));
                    }
                    Spilled::Stopped => return Ok(WriteReceipt::new(0, None)),
                    Spilled::NoDrainer => continue,
                }
            };
            return self.write_frame_body_locked(guard, bytes, order, meter, since);
        }
    }

    #[cfg(unix)]
    const NONPARK_MAX: usize = 4096;

    #[cfg(unix)]
    const TRY_LOCK_SPINS: u32 = 256;

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "fd_lifecycle",
            action = "UseFd",
            project = "aterm_session::sink::SinkWriter::project_fd_state"
        )
    )]
    #[cfg(unix)]
    pub fn write_frame_nonparking(&self, bytes: &[u8]) -> io::Result<usize> {
        self.write_frame_nonparking_with_receipt(bytes)
            .map(WriteReceipt::accepted)
            .map_err(WriteReceiptError::into_error)
    }

    /// Receipt-bearing twin of [`Self::write_frame_nonparking`].
    #[cfg(unix)]
    pub fn write_frame_nonparking_with_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        if bytes.is_empty() {
            return Ok(WriteReceipt::new(0, None));
        }
        self.reserve_input_attempt();
        self.write_frame_nonparking_after_reserve(bytes, Landing::Driven)
    }

    /// The terminal's own answer to a query its program sent (DA, DSR/CPR, a colour or kitty-keyboard query).
    pub fn write_reply(&self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.reserve_attempt();
        self.write_frame_nonparking_after_reserve(bytes, Landing::Answer)
            .map(WriteReceipt::accepted)
            .map_err(WriteReceiptError::into_error)
    }

    #[cfg(unix)]
    fn write_frame_nonparking_after_reserve(
        &self,
        bytes: &[u8],
        landing: Landing,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        // Bulk takes the blocking path so `SPILL_CAP` applies (see `NONPARK_MAX`).
        if bytes.len() > Self::NONPARK_MAX {
            return self.write_frame_after_reserve(bytes, None);
        }
        self.write_frame_unparked(bytes, WhenFull::Park, landing)
    }

    #[cfg(windows)]
    fn write_frame_nonparking_after_reserve(
        &self,
        bytes: &[u8],
        _landing: Landing,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        self.write_frame_after_reserve(bytes, None)
    }

    /// The UI thread's egress.
    #[cfg(unix)]
    pub fn write_frame_interactive_with_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        if bytes.is_empty() {
            return Ok(WriteReceipt::new(0, None));
        }
        self.reserve_input_attempt();
        self.write_frame_unparked(bytes, WhenFull::Refuse, Landing::Driven)
    }

    /// Windows twin of [`Self::write_frame_interactive_with_receipt`].
    #[cfg(windows)]
    pub fn write_frame_interactive_with_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        self.write_frame_with_receipt(bytes)
    }

    #[cfg(unix)]
    fn write_frame_unparked(
        &self,
        bytes: &[u8],
        when_full: WhenFull,
        landing: Landing,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        if self.shared.is_severed() {
            return Err(Shared::severed_error().into());
        }
        let since = self.shared.discard_epoch();
        let spilled = self.shared.spill_len();
        if spilled >= Shared::SPILL_CAP {
            return match when_full {
                WhenFull::Park => self.write_frame_after_reserve(bytes, None),
                WhenFull::Refuse => Err(WriteReceiptError::refused(0, None)),
            };
        }
        if spilled != 0
            && let Spilled::Accepted(order) =
                self.shared
                    .spill_append(self.master, bytes, Room::NoWait, since)?
        {
            return Ok(WriteReceipt::new(bytes.len(), Some(order)));
        }
        let mut spins: u32 = 0;
        let acquired = loop {
            match self.shared.lock.try_lock() {
                Ok(g) => break Some(g),
                Err(std::sync::TryLockError::Poisoned(_)) => break None,
                Err(_) if spins < Self::TRY_LOCK_SPINS => {
                    std::hint::spin_loop();
                    spins = spins.saturating_add(1);
                }
                Err(_) => break None,
            }
        };
        let Some(guard) = acquired else {
            // Another writer is mid-frame (or the drainer is writing): queueing
            // behind the current holder preserves order without waiting on it.
            return self.spill_or_when_full(bytes, since, when_full);
        };
        let Some(order) = self.shared.direct_order_if_spill_empty()? else {
            drop(guard);
            return self.spill_or_when_full(bytes, since, when_full);
        };
        let whole_frame = self.shared.master_nonblocking.load(Ordering::Relaxed);
        let mut off = 0;
        while off < bytes.len() {
            // `whole_frame` comes only from the real `fcntl` result, so this cannot park.
            if !whole_frame {
                match aterm_pty::poll_writable(self.master, 0) {
                    Ok(true) => {}
                    Ok(false) => {
                        return self.spill_tail_locked(guard, bytes, off, order, since, when_full);
                    }
                    Err(e) => {
                        drop(guard);
                        if off > 0 {
                            let _ = self.shared.fire_input_hook_for(landing);
                        }
                        return Err(WriteReceiptError::after_write(e, off, order));
                    }
                }
            }
            let unit = if whole_frame {
                bytes.get(off..)
            } else {
                bytes.get(off..=off)
            };
            let Some(unit) = unit else {
                break;
            };
            let wrote = self.shared.ledgered_nonparking(self.master, unit, false);
            match wrote {
                aterm_pty::NonParkWrite::Closed => break, // peer closed mid-frame
                aterm_pty::NonParkWrite::Wrote(n) => {
                    let room = bytes.len().saturating_sub(off);
                    off = off.saturating_add(if n <= room { n } else { room });
                }
                aterm_pty::NonParkWrite::WouldBlock => {
                    return self.spill_tail_locked(guard, bytes, off, order, since, when_full);
                }
                aterm_pty::NonParkWrite::Fatal(e) => {
                    drop(guard);
                    if off > 0 {
                        let _ = self.shared.fire_input_hook_for(landing);
                    }
                    return Err(WriteReceiptError::after_write(e, off, order));
                }
            }
        }
        drop(guard);
        if off > 0 {
            let _ = self.shared.fire_input_hook_for(landing);
        }
        Ok(WriteReceipt::completed(off, bytes.len(), order))
    }

    #[cfg(unix)]
    fn spill_or_when_full(
        &self,
        bytes: &[u8],
        since: u64,
        when_full: WhenFull,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        match self
            .shared
            .spill_append(self.master, bytes, Room::NoWait, since)?
        {
            Spilled::Accepted(order) => Ok(WriteReceipt::new(bytes.len(), Some(order))),
            Spilled::NoDrainer | Spilled::Stopped => match when_full {
                WhenFull::Park => self.write_frame_locked(bytes, None, since),
                WhenFull::Refuse => Err(WriteReceiptError::no_drainer(0, None)),
            },
        }
    }

    /// Windows: a ConPTY handle (an opaque registry key) has no spill or `poll(2)`, so this is the ordered blocking [`Self::write_frame`].
    #[cfg(windows)]
    pub fn write_frame_nonparking(&self, bytes: &[u8]) -> io::Result<usize> {
        self.write_frame_nonparking_with_receipt(bytes)
            .map(WriteReceipt::accepted)
            .map_err(WriteReceiptError::into_error)
    }

    /// Windows receipt-bearing twin of [`Self::write_frame_nonparking`].
    #[cfg(windows)]
    pub fn write_frame_nonparking_with_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        self.write_frame_with_receipt(bytes)
    }

    #[cfg(unix)]
    fn spill_tail_locked(
        &self,
        guard: std::sync::MutexGuard<'_, ()>,
        bytes: &[u8],
        off: usize,
        order: AcceptedOrder,
        since: u64,
        when_full: WhenFull,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        let Some(tail) = bytes.get(off..) else {
            drop(guard);
            if off > 0 {
                let _ = self.shared.fire_input_hook();
            }
            return Ok(WriteReceipt::new(bytes.len(), Some(order)));
        };
        if self
            .shared
            .spill_prepend(self.master, tail, since, Room::NoWait)
        {
            drop(guard);
            if off > 0 {
                let _ = self.shared.fire_input_hook();
            }
            return Ok(WriteReceipt::new(bytes.len(), Some(order)));
        }
        if when_full == WhenFull::Refuse {
            drop(guard);
            if off > 0 {
                let _ = self.shared.fire_input_hook();
            }
            return Err(WriteReceiptError::no_drainer(
                off,
                (off > 0).then_some(order),
            ));
        }
        let mut off = off;
        while off < bytes.len() {
            let Some(rest) = bytes.get(off..) else { break };
            match self
                .shared
                .write_stamped_blocking(self.master, rest, since, None)
            {
                Ok(0) => {
                    drop(guard);
                    if off > 0 {
                        let _ = self.shared.fire_input_hook();
                    }
                    return Ok(WriteReceipt::new(off, (off > 0).then_some(order)));
                }
                Ok(n) => {
                    let room = bytes.len().saturating_sub(off);
                    off = off.saturating_add(if n <= room { n } else { room });
                }
                Err(e) => {
                    drop(guard);
                    if off > 0 {
                        let _ = self.shared.fire_input_hook();
                    }
                    return Err(WriteReceiptError::after_write(e, off, order));
                }
            }
        }
        drop(guard);
        if off > 0 {
            let _ = self.shared.fire_input_hook();
        }
        Ok(WriteReceipt::new(off, (off > 0).then_some(order)))
    }
}

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace SinkSpillOrder

        -- State of the packed order_state word and spill FIFO:
        -- spill_non_empty tracks whether spilled bytes await delivery;
        -- next_order is the monotonically increasing accepted-order counter.
        structure OrderState where
            spill_non_empty : Bool
            next_order : Nat

        -- Direct inline write admission under the fd lock: succeeds iff
        -- spill_non_empty is false, advancing next_order by 1.
        def tryDirectWrite (s : OrderState) : Option OrderState :=
            if s.spill_non_empty = true then
                none
            else
                some (OrderState.mk false (s.next_order + 1))

        -- Spill append admission under the spill lock: marks spill_non_empty
        -- true and advances next_order by 1.
        def spillAppend (s : OrderState) : OrderState :=
            OrderState.mk true (s.next_order + 1)

        -- Theorem 1: When spill_non_empty is true, tryDirectWrite returns none
        -- so no direct write can overtake undelivered spilled bytes.
        theorem no_direct_overtake_when_spilled (n : Nat) :
            tryDirectWrite (OrderState.mk true n) = none := by
            rfl

        -- Theorem 2: When spill_non_empty is false, tryDirectWrite mints
        -- next_order + 1 and preserves spill_non_empty = false.
        theorem direct_write_advances_when_empty (n : Nat) :
            tryDirectWrite (OrderState.mk false n) =
                some (OrderState.mk false (n + 1)) := by
            rfl

        -- Theorem 3: A spillAppend after a direct write receives strictly
        -- greater order than the direct write.
        theorem spill_after_direct_strictly_ordered (n : Nat) :
            (spillAppend (OrderState.mk false (n + 1))).next_order = n + 2 := by
            rfl

        end SinkSpillOrder
    }
}

impl Shared {
    const SPILL_ACTIVE_BIT: u64 = 1 << 63;
    const ORDER_MASK: u64 = (1 << 63) - 1;
    #[cfg(unix)]
    const SPILL_CAP: usize = 2 * 1024 * 1024;
    #[cfg(unix)]
    const DRAIN_CHUNK: usize = 8 * 1024;
    #[cfg(unix)]
    const PARK_RECHECK_MS: i32 = 100;

    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            input_epoch: AtomicU64::new(0),
            order_state: AtomicU64::new(0),
            spilled_bytes: AtomicUsize::new(0),
            cached_tty_swallows: AtomicU8::new(0),
            spill: Mutex::new(Spill {
                buf: VecDeque::new(),
                chunk_written: 0,
                draining: false,
                #[cfg(unix)]
                arrange_pending: false,
                failed: false,
            }),
            drained: Condvar::new(),
            arranger: std::sync::OnceLock::new(),
            ledger: Mutex::new(InputLedger::new(Instant::now())),
            input_hook: std::sync::OnceLock::new(),
            input_hook_armed: AtomicBool::new(false),
            input_hook_driven: AtomicBool::new(false),
            discards: AtomicU64::new(0),
            restarts: AtomicU64::new(0),
            severed: AtomicBool::new(false),
            reply_reserved: AtomicUsize::new(0),
            master_nonblocking: AtomicBool::new(false),
            #[cfg(all(unix, any(test, debug_assertions)))]
            seam: std::sync::OnceLock::new(),
        }
    }

    #[cfg(all(unix, any(test, debug_assertions)))]
    fn at_seam(&self, seam: Seam) {
        if let Some(hook) = self.seam.get() {
            hook(seam);
        }
    }

    #[cfg(unix)]
    fn next_accepted_order(&self, mark_spill_active: bool) -> io::Result<AcceptedOrder> {
        let mut cur = self.order_state.load(Ordering::Acquire);
        loop {
            let order = cur & Self::ORDER_MASK;
            if order == Self::ORDER_MASK {
                return Err(io::Error::other("PTY sink accepted-order token exhausted"));
            }
            let next_order = order + 1;
            let active = (cur & Self::SPILL_ACTIVE_BIT)
                | if mark_spill_active {
                    Self::SPILL_ACTIVE_BIT
                } else {
                    0
                };
            match self.order_state.compare_exchange_weak(
                cur,
                active | next_order,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(AcceptedOrder(next_order)),
                Err(actual) => cur = actual,
            }
        }
    }

    fn discard_epoch(&self) -> u64 {
        self.discards.load(Ordering::Acquire)
    }

    fn discarded_since(&self, since: u64) -> bool {
        self.discard_epoch() != since || self.is_severed()
    }

    fn is_severed(&self) -> bool {
        self.severed.load(Ordering::Acquire)
    }

    fn severed_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "session input severed: the session is closing",
        )
    }

    fn ledger_begin(&self, len: usize) {
        self.ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .begin(len);
    }

    fn ledger_end(&self, n: usize, fire_hook: bool) {
        let at = Instant::now();
        self.ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .end(n, at);
        if fire_hook && n > 0 {
            let _ = self.fire_input_hook();
        }
    }

    fn ledgered<R>(
        &self,
        bytes: &[u8],
        fire_hook: bool,
        write: impl FnOnce(&[u8]) -> (R, usize),
    ) -> R {
        self.ledger_begin(bytes.len());
        let (result, accepted) = write(bytes);
        self.ledger_end(accepted, fire_hook);
        result
    }

    #[cfg(unix)]
    fn ledgered_nonparking(
        &self,
        fd: i32,
        bytes: &[u8],
        fire_hook: bool,
    ) -> aterm_pty::NonParkWrite {
        self.ledgered(bytes, fire_hook, |bytes| {
            let wrote = aterm_pty::write_some_nonparking(fd, bytes);
            let accepted = match &wrote {
                aterm_pty::NonParkWrite::Wrote(n) => *n,
                _ => 0,
            };
            (wrote, accepted)
        })
    }

    fn fire_input_hook(&self) -> bool {
        self.fire_input_hook_for(Landing::Driven)
    }

    fn fire_input_hook_for(&self, landing: Landing) -> bool {
        if landing == Landing::Driven {
            self.input_hook_driven.store(true, Ordering::SeqCst);
        }
        let Some(hook) = self.input_hook.get() else {
            return false;
        };
        if self.input_hook_armed.swap(true, Ordering::SeqCst) {
            return false;
        }
        hook();
        true
    }

    fn try_spill_len(&self) -> Option<usize> {
        let unsent = |s: &Spill| s.buf.len().saturating_sub(s.chunk_written);
        match self.spill.try_lock() {
            Ok(spill) => Some(unsent(&spill)),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                Some(unsent(&poisoned.into_inner()))
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    #[cfg(unix)]
    fn write_stamped_blocking(
        &self,
        fd: i32,
        bytes: &[u8],
        since: u64,
        stop: Option<&BulkMeter>,
    ) -> io::Result<usize> {
        loop {
            if self.discarded_since(since) {
                return Ok(0);
            }
            let wrote = self.ledgered_nonparking(fd, bytes, true);
            match wrote {
                aterm_pty::NonParkWrite::Wrote(n) => return Ok(n),
                aterm_pty::NonParkWrite::Closed => return Ok(0),
                aterm_pty::NonParkWrite::Fatal(e) => return Err(e),
                aterm_pty::NonParkWrite::WouldBlock => loop {
                    #[cfg(any(test, debug_assertions))]
                    self.at_seam(Seam::WriterPark);
                    if aterm_pty::poll_writable(fd, Self::PARK_RECHECK_MS)? {
                        break;
                    }
                    if self.discarded_since(since) || stop.is_some_and(BulkMeter::stop_requested) {
                        return Ok(0);
                    }
                },
            }
        }
    }

    #[cfg(not(unix))]
    fn write_stamped_blocking(
        &self,
        fd: i32,
        bytes: &[u8],
        since: u64,
        stop: Option<&BulkMeter>,
    ) -> io::Result<usize> {
        self.write_stamped_through(bytes, since, stop, |bytes, stop| {
            aterm_pty::write_some_stoppable(fd, bytes, stop)
        })
    }

    #[cfg(any(not(unix), test))]
    fn write_stamped_through(
        &self,
        bytes: &[u8],
        since: u64,
        stop: Option<&BulkMeter>,
        blocking_write: impl FnOnce(&[u8], Option<&AtomicBool>) -> io::Result<usize>,
    ) -> io::Result<usize> {
        if self.discarded_since(since) {
            return Ok(0);
        }
        self.ledgered(bytes, true, |bytes| {
            let wrote = blocking_write(bytes, stop.map(|meter| &meter.stop));
            let accepted = wrote.as_ref().map_or(0, |n| *n);
            (wrote, accepted)
        })
    }

    #[cfg(unix)]
    fn drain_write(&self, fd: i32, chunk: &[u8], off: usize, since: u64) -> usize {
        let Some(bytes) = chunk.get(off..) else {
            return 0;
        };
        loop {
            let wrote = {
                let s = self.spill.lock().unwrap_or_else(|p| p.into_inner());
                if self.discarded_since(since) {
                    return 0;
                }
                let held = if self.master_nonblocking.load(Ordering::Relaxed) {
                    Some(s)
                } else {
                    drop(s);
                    None
                };
                #[cfg(any(test, debug_assertions))]
                self.at_seam(Seam::DrainWrite);
                let wrote = self.ledgered_nonparking(fd, bytes, false);
                let n = match wrote {
                    aterm_pty::NonParkWrite::Wrote(n) => n,
                    _ => 0,
                };
                if n > 0 {
                    let mut s = held
                        .unwrap_or_else(|| self.spill.lock().unwrap_or_else(|p| p.into_inner()));
                    if !self.discarded_since(since) {
                        s.chunk_written = off.saturating_add(n);
                    }
                }
                wrote
            };
            match wrote {
                aterm_pty::NonParkWrite::Wrote(n) => {
                    // The hook after the spill mutex drops (it runs under the
                    // fd lock, as every landing write's does).
                    if n > 0 {
                        let _ = self.fire_input_hook();
                    }
                    return n;
                }
                aterm_pty::NonParkWrite::Closed | aterm_pty::NonParkWrite::Fatal(_) => return 0,
                aterm_pty::NonParkWrite::WouldBlock => loop {
                    #[cfg(any(test, debug_assertions))]
                    self.at_seam(Seam::DrainPark);
                    match aterm_pty::poll_writable(fd, Self::PARK_RECHECK_MS) {
                        Ok(true) => break,
                        Ok(false) if !self.discarded_since(since) => {}
                        Ok(false) | Err(_) => return 0,
                    }
                },
            }
        }
    }

    fn direct_order_if_spill_empty(&self) -> io::Result<Option<AcceptedOrder>> {
        let mut cur = self.order_state.load(Ordering::Acquire);
        loop {
            if cur & Self::SPILL_ACTIVE_BIT != 0 {
                return Ok(None);
            }
            let order = cur & Self::ORDER_MASK;
            if order == Self::ORDER_MASK {
                return Err(io::Error::other("PTY sink accepted-order token exhausted"));
            }
            let next = order + 1;
            match self.order_state.compare_exchange_weak(
                cur,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Some(AcceptedOrder(next))),
                Err(actual) => cur = actual,
            }
        }
    }

    fn spill_is_empty(&self) -> bool {
        self.spilled_bytes.load(Ordering::Acquire) == 0
    }

    #[cfg(unix)]
    fn spill_len(&self) -> usize {
        self.spilled_bytes.load(Ordering::Acquire)
    }

    #[cfg(unix)]
    fn queue_rest_held(self: &Arc<Self>, master: i32, s: &mut Spill, rest: &[u8]) -> Option<bool> {
        let defer = self.arranger.get().is_some();
        if !s.draining {
            if defer {
                s.draining = true;
                s.arrange_pending = true;
            } else if !self.arrange_drainer(master, s) {
                return None;
            }
        }
        let poke = defer && s.arrange_pending;
        s.buf.extend(rest.iter().copied());
        self.spilled_bytes.store(s.buf.len(), Ordering::Release);
        if !s.buf.is_empty() {
            self.order_state
                .fetch_or(Self::SPILL_ACTIVE_BIT, Ordering::Release);
        }
        Some(poke)
    }

    #[cfg(unix)]
    fn spill_prepend(
        self: &Arc<Self>,
        master: i32,
        tail: &[u8],
        since: u64,
        room: Room<'_>,
    ) -> bool {
        let mut s = self.spill.lock().unwrap_or_else(|p| p.into_inner());
        if self.discarded_since(since) {
            return true;
        }
        let defer = matches!(room, Room::NoWait) && self.arranger.get().is_some();
        if !s.draining {
            if defer {
                s.draining = true;
                s.arrange_pending = true;
            } else if !self.arrange_drainer(master, &mut s) {
                return false;
            }
        }
        let poke = defer && s.arrange_pending;
        for b in tail.iter().rev() {
            s.buf.push_front(*b);
        }
        self.spilled_bytes.store(s.buf.len(), Ordering::Release);
        if !s.buf.is_empty() {
            self.order_state
                .fetch_or(Self::SPILL_ACTIVE_BIT, Ordering::Release);
        }
        drop(s);
        if poke && let Some(arranger) = self.arranger.get() {
            arranger();
        }
        true
    }

    #[cfg(unix)]
    fn prepend_owed(self: &Arc<Self>, master: i32, owed: &[u8], since: u64) -> bool {
        self.spill_prepend(master, owed, since, Room::Wait(None))
    }

    #[cfg(not(unix))]
    fn prepend_owed(self: &Arc<Self>, _master: i32, _owed: &[u8], _since: u64) -> bool {
        false
    }

    #[cfg(unix)]
    fn spill_append(
        self: &Arc<Self>,
        master: i32,
        bytes: &[u8],
        room: Room<'_>,
        since: u64,
    ) -> io::Result<Spilled> {
        let mut s = self.spill.lock().unwrap_or_else(|p| p.into_inner());
        if let Room::Wait(stop) = room {
            let recheck = Duration::from_millis(Self::PARK_RECHECK_MS.unsigned_abs().into());
            while s.draining && s.buf.len() > Self::SPILL_CAP && !self.discarded_since(since) {
                if stop.is_some_and(BulkMeter::stop_requested) {
                    return Ok(Spilled::Stopped);
                }
                if s.arrange_pending {
                    s.arrange_pending = false;
                    if !self.arrange_drainer(master, &mut s) {
                        s.draining = false;
                        continue;
                    }
                }
                s = match self.drained.wait_timeout(s, recheck) {
                    Ok((guard, _)) => guard,
                    Err(poison) => poison.into_inner().0,
                };
            }
        }
        if self.discarded_since(since) {
            return self
                .next_accepted_order(!s.buf.is_empty())
                .map(Spilled::Accepted);
        }
        let defer = matches!(room, Room::NoWait) && self.arranger.get().is_some();
        if !s.draining {
            if defer {
                s.draining = true;
                s.arrange_pending = true;
            } else if !self.arrange_drainer(master, &mut s) {
                return Ok(Spilled::NoDrainer);
            }
        }
        let poke = defer && s.arrange_pending;
        let will_be_non_empty = !s.buf.is_empty() || !bytes.is_empty();
        let order = self.next_accepted_order(will_be_non_empty)?;
        s.buf.extend(bytes.iter().copied());
        self.spilled_bytes.store(s.buf.len(), Ordering::Release);
        drop(s);
        if poke && let Some(arranger) = self.arranger.get() {
            arranger();
        }
        Ok(Spilled::Accepted(order))
    }

    #[cfg(windows)]
    fn spill_append(
        self: &Arc<Self>,
        _master: i32,
        _bytes: &[u8],
        _room: Room<'_>,
        _since: u64,
    ) -> io::Result<Spilled> {
        Ok(Spilled::NoDrainer)
    }

    #[cfg(unix)]
    fn arrange_drainer(self: &Arc<Self>, master: i32, s: &mut Spill) -> bool {
        let Ok(fd) = aterm_pty::dup_fd(master) else {
            return false;
        };
        let shared = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("aterm-sink-drain".into())
            .spawn(move || shared.drain_loop(fd));
        if spawned.is_err() {
            return false;
        }
        s.draining = true;
        true
    }

    #[cfg(unix)]
    fn drain_loop(self: Arc<Self>, fd: OwnedFd) {
        aterm_pty::declare_interactive_thread();
        let mut chunk: Vec<u8> = Vec::with_capacity(Self::DRAIN_CHUNK);
        loop {
            let guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            let since: u64 = {
                let mut s = self.spill.lock().unwrap_or_else(|p| p.into_inner());
                if s.buf.is_empty() {
                    s.draining = false;
                    self.spilled_bytes.store(0, Ordering::Release);
                    self.order_state
                        .fetch_and(!Self::SPILL_ACTIVE_BIT, Ordering::Release);
                    drop(s);
                    drop(guard);
                    self.drained.notify_all();
                    return;
                }
                chunk.clear();
                let (head, tail) = s.buf.as_slices();
                let take_head = head.len().min(Self::DRAIN_CHUNK);
                if let Some(prefix) = head.get(..take_head) {
                    chunk.extend_from_slice(prefix);
                }
                let rem = Self::DRAIN_CHUNK.saturating_sub(chunk.len());
                if rem > 0 {
                    let take_tail = tail.len().min(rem);
                    if let Some(prefix) = tail.get(..take_tail) {
                        chunk.extend_from_slice(prefix);
                    }
                }
                self.discard_epoch()
            };
            let mut off = 0;
            let mut dead = false;
            while off < chunk.len() {
                match self.drain_write(fd.as_raw_fd(), &chunk, off, since) {
                    n if n > 0 => off = off.saturating_add(n),
                    _ => {
                        dead = true;
                        break;
                    }
                }
            }
            drop(guard);
            {
                let mut s = self.spill.lock().unwrap_or_else(|p| p.into_inner());
                s.chunk_written = 0;
                if self.discarded_since(since) {
                    drop(s);
                    self.drained.notify_all();
                    continue;
                }
                if dead {
                    s.buf.clear();
                    s.draining = false;
                    s.failed = true;
                    self.spilled_bytes.store(0, Ordering::Release);
                    self.order_state
                        .fetch_and(!Self::SPILL_ACTIVE_BIT, Ordering::Release);
                    drop(s);
                    self.drained.notify_all();
                    return;
                }
                let len = s.buf.len();
                let take = if off <= len { off } else { len };
                for _ in 0..take {
                    let _ = s.buf.pop_front();
                }
                self.spilled_bytes.store(s.buf.len(), Ordering::Release);
                if s.buf.is_empty() {
                    self.order_state
                        .fetch_and(!Self::SPILL_ACTIVE_BIT, Ordering::Release);
                }
            }
            self.drained.notify_all();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn input_backlog_is_none_off_a_tty() {
        let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = SinkWriter::new(writer.as_raw_fd());
        assert_eq!(sink.write_frame(b"abc").expect("write"), 3);
        assert_eq!(sink.input_backlog(), None);
        assert_eq!(sink.tty_echo(), None);
        assert_eq!(SinkWriter::new(-1).input_backlog(), None);
        assert_eq!(SinkWriter::new(-1).tty_echo(), None);
    }

    #[test]
    fn the_input_hook_says_whether_anything_but_an_answer_landed() {
        let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = SinkWriter::new(writer.as_raw_fd());
        let fired = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&fired);
        sink.install_input_hook(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        assert!(!sink.take_input_hook_driven(), "nothing landed");
        assert_eq!(sink.write_reply(b"\x1b[?62c").expect("answer"), 6);
        assert_eq!(fired.load(Ordering::SeqCst), 1, "an answer fires the hook");
        assert!(!sink.take_input_hook_driven(), "but drove nothing");

        assert_eq!(sink.write_frame_nonparking(b"k").expect("a key"), 1);
        assert_eq!(fired.load(Ordering::SeqCst), 1, "the arming took the fire");
        assert!(sink.take_input_hook_driven(), "the key marked it");
        assert!(!sink.take_input_hook_driven(), "taken");

        sink.rearm_input_hook();
        assert!(sink.wake_input_hook(), "an explicit wake fires");
        assert!(sink.take_input_hook_driven(), "and marks it");
        let interactive = sink
            .write_frame_interactive_with_receipt(b"i")
            .expect("an interactive key");
        assert_eq!(interactive.accepted(), 1);
        assert!(sink.take_input_hook_driven(), "an interactive key marks it");
        assert_eq!(sink.write_frame(b"p").expect("a blocking write"), 1);
        assert!(sink.take_input_hook_driven(), "a blocking write marks it");
        assert_eq!(sink.write_reply(b"\x1b[0n").expect("answer"), 4);
        assert!(
            !sink.take_input_hook_driven(),
            "an answer still marks nothing"
        );
        drop(writer);
    }

    #[test]
    fn input_hook_fires_once_until_rearmed() {
        let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = SinkWriter::new(writer.as_raw_fd());
        assert_eq!(sink.write_frame(b"x").expect("write"), 1, "no hook yet");
        let fired = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&fired);
        sink.install_input_hook(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        sink.install_input_hook(|| panic!("the first install wins"));
        assert_eq!(fired.load(Ordering::SeqCst), 0, "installing fires nothing");

        assert_eq!(sink.write_frame(b"").expect("empty"), 0);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "an empty frame lands nothing"
        );
        assert_eq!(sink.write_frame(b"\r").expect("write"), 1);
        assert_eq!(sink.write_frame_nonparking(b"\x1b[B").expect("write"), 3);
        assert_eq!(fired.load(Ordering::SeqCst), 1, "two writes, one wake");

        sink.rearm_input_hook();
        assert_eq!(fired.load(Ordering::SeqCst), 1, "rearming fires nothing");
        assert_eq!(sink.write_frame(b"y").expect("write"), 1);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            2,
            "rearmed, the next write wakes"
        );
        drop(writer);
    }

    #[test]
    fn an_explicit_wake_shares_the_input_hooks_arming() {
        let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = SinkWriter::new(writer.as_raw_fd());
        assert!(!sink.wake_input_hook(), "no hook installed: nothing runs");
        let fired = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&fired);
        sink.install_input_hook(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        assert!(
            sink.wake_input_hook(),
            "the no-hook call armed nothing: the first wake runs"
        );
        assert!(!sink.wake_input_hook(), "fired, not rearmed: no second run");
        assert_eq!(sink.write_frame(b"\r").expect("write"), 1);
        assert_eq!(fired.load(Ordering::SeqCst), 1, "a write behind it waits");

        sink.rearm_input_hook();
        assert_eq!(sink.write_frame(b"x").expect("write"), 1);
        assert!(!sink.wake_input_hook(), "the write fired first");
        assert_eq!(fired.load(Ordering::SeqCst), 2);
        drop(writer);
    }

    #[test]
    fn write_frame_is_whole_frame_atomic_across_threads() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

        const N: u8 = 4;
        const LEN: usize = 128 * 1024; // > any default socket buffer -> forces short writes

        let reader_handle = thread::spawn(move || {
            let mut buf = vec![0u8; (N as usize) * LEN];
            reader.read_exact(&mut buf).expect("read_exact");
            buf
        });

        let mut handles = Vec::new();
        for i in 0..N {
            let s = Arc::clone(&sink);
            handles.push(thread::spawn(move || {
                let frame = vec![b'A' + i; LEN];
                assert_eq!(
                    s.write_frame(&frame).expect("write_frame"),
                    LEN,
                    "whole frame accepted"
                );
            }));
        }
        for h in handles {
            h.join().expect("writer thread");
        }
        let buf = reader_handle.join().expect("reader thread");
        drop(writer); // keep the borrowed fd alive until here

        let runs = runs_of(&buf);
        assert_eq!(
            runs.len(),
            N as usize,
            "expected {N} contiguous frames; interleaving fragmented them into {} runs",
            runs.len()
        );
        for (byte, len) in &runs {
            assert_eq!(
                *len, LEN,
                "frame for byte {byte} was split — writers interleaved"
            );
        }
        let mut distinct: Vec<u8> = runs.iter().map(|(b, _)| *b).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            N as usize,
            "every frame's byte must appear exactly once"
        );
    }

    fn runs_of(buf: &[u8]) -> Vec<(u8, usize)> {
        let mut runs: Vec<(u8, usize)> = Vec::new();
        for &b in buf {
            match runs.last_mut() {
                Some((rb, n)) if *rb == b => *n += 1,
                _ => runs.push((b, 1)),
            }
        }
        runs
    }

    #[test]
    fn receipt_twins_report_actual_acceptance_in_increasing_order() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = SinkWriter::new(writer.as_raw_fd());

        let empty = sink
            .write_frame_with_receipt(b"")
            .expect("empty blocking receipt");
        assert_eq!(empty.accepted(), 0);
        assert_eq!(empty.order(), None, "empty writes do not enter sink order");

        let blocking = sink
            .write_frame_with_receipt(b"B")
            .expect("blocking receipt");
        let nonparking = sink
            .write_frame_nonparking_with_receipt(b"N")
            .expect("non-parking receipt");

        writer.set_nonblocking(true).expect("nonblocking writer");
        sink.note_master_nonblocking(true);
        let (immediate, immediate_order) = sink.try_write_frame_immediate_with_receipt(b"I");
        assert_eq!(immediate, ImmediateWrite::Full);

        assert_eq!(blocking.accepted(), 1);
        assert_eq!(nonparking.accepted(), 1);
        let blocking_order = blocking.order().expect("blocking accepted order");
        let nonparking_order = nonparking.order().expect("non-parking accepted order");
        let immediate_order = immediate_order.expect("immediate accepted order");
        assert!(blocking_order < nonparking_order);
        assert!(nonparking_order < immediate_order);

        let mut bytes = [0_u8; 3];
        reader.read_exact(&mut bytes).expect("read receipt frames");
        assert_eq!(&bytes, b"BNI");
    }

    #[test]
    fn peer_closed_immediate_write_has_no_accepted_order() {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        writer.set_nonblocking(true).expect("nonblocking writer");
        let sink = SinkWriter::new(writer.as_raw_fd());
        sink.note_master_nonblocking(true);
        drop(reader);

        let (write, order) = sink.try_write_frame_immediate_with_receipt(b"closed");
        assert_eq!(write, ImmediateWrite::BusyZero);
        assert_eq!(order, None, "a minted-but-unaccepted token stays private");
    }

    #[test]
    fn accepted_order_follows_forced_spill_linearization_not_attempt_epoch() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
        let held = sink.shared.lock.lock().unwrap();
        let initial_epoch = sink.input_epoch();

        let blocked_sink = Arc::clone(&sink);
        let blocked = thread::spawn(move || {
            blocked_sink
                .write_frame_with_receipt(b"A")
                .expect("delayed blocking receipt")
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while sink.input_epoch() == initial_epoch {
            assert!(
                std::time::Instant::now() < deadline,
                "blocking writer never reserved its earlier attempt"
            );
            thread::yield_now();
        }

        let first = sink
            .write_frame_nonparking_with_receipt(b"B")
            .expect("later attempt spills without waiting");
        assert_eq!(first.accepted(), 1);
        assert!(!blocked.is_finished(), "fd-lock holder still blocks A");
        drop(held);

        let second = blocked.join().expect("blocking writer thread");
        assert_eq!(second.accepted(), 1);
        assert!(
            first.order().expect("B order") < second.order().expect("A order"),
            "receipts must follow spill/direct serialization, not attempt reservation"
        );

        let mut bytes = [0_u8; 2];
        reader
            .read_exact(&mut bytes)
            .expect("read reordered frames");
        assert_eq!(&bytes, b"BA", "kernel/spill order agrees with receipts");
    }

    #[test]
    fn degraded_locked_fallback_rechecks_a_racing_spill() {
        let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = SinkWriter::new(writer.as_raw_fd());
        {
            let mut spill = sink.shared.spill.lock().unwrap();
            spill.buf.push_back(b'B');
            sink.shared.spilled_bytes.store(1, Ordering::Release);
            sink.shared
                .order_state
                .store(Shared::SPILL_ACTIVE_BIT | 1, Ordering::Release);
            spill.draining = true;
        }

        let receipt = sink
            .write_frame_locked(b"A", None, sink.shared.discard_epoch())
            .expect("fallback queues behind the racing spill");
        assert_eq!(receipt.accepted(), 1);
        assert_eq!(receipt.order(), Some(AcceptedOrder(2)));
        let spill = sink.shared.spill.lock().unwrap();
        assert_eq!(spill.buf.iter().copied().collect::<Vec<_>>(), b"BA");
    }

    #[test]
    fn try_egress_drained_is_nonblocking_and_recovers_poison() {
        let (_reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
        assert_eq!(sink.try_egress_drained_to_kernel(), Some(true));

        {
            let mut spill = sink.shared.spill.lock().unwrap();
            assert_eq!(
                sink.try_egress_drained_to_kernel(),
                None,
                "contended observation must not wait"
            );
            spill.buf.push_back(b'x');
        }
        assert_eq!(sink.try_egress_drained_to_kernel(), Some(false));
        sink.shared.spill.lock().unwrap().buf.clear();

        let poison_sink = Arc::clone(&sink);
        assert!(
            thread::spawn(move || {
                let _spill = poison_sink.shared.spill.lock().unwrap();
                panic!("poison spill mutex for recovery coverage");
            })
            .join()
            .is_err()
        );
        assert_eq!(
            sink.try_egress_drained_to_kernel(),
            Some(true),
            "a poisoned try_lock already owns the guard and remains non-parking"
        );
    }

    #[test]
    fn public_writes_reserve_one_epoch_per_nonempty_attempt() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = SinkWriter::new(writer.as_raw_fd());
        let large = vec![b'L'; SinkWriter::NONPARK_MAX + 1];
        let expected_len = 2 + large.len() + 1;
        let reader_handle = thread::spawn(move || {
            let mut bytes = vec![0_u8; expected_len];
            reader.read_exact(&mut bytes).expect("read all attempts");
            bytes
        });

        let initial = sink.input_epoch().0;
        assert_eq!(sink.write_frame(b"").expect("empty blocking write"), 0);
        assert_eq!(
            sink.input_epoch().0,
            initial,
            "empty write is not an attempt"
        );

        assert_eq!(sink.write_frame(b"B").expect("blocking write"), 1);
        assert_eq!(sink.input_epoch().0, initial + 1);

        assert_eq!(
            sink.write_frame_nonparking(b"N")
                .expect("small non-parking write"),
            1
        );
        assert_eq!(sink.input_epoch().0, initial + 2);

        assert_eq!(
            sink.write_frame_nonparking(&large)
                .expect("large delegated write"),
            large.len()
        );
        assert_eq!(
            sink.input_epoch().0,
            initial + 3,
            "delegation must not reserve a second epoch"
        );
        assert_eq!(sink.driven_input().0, 3, "every write so far was driven");

        assert_eq!(sink.write_reply(b"").expect("empty reply"), 0);
        assert_eq!(sink.write_reply(b"R").expect("a reply"), 1);
        assert_eq!(sink.input_epoch().0, initial + 4, "a reply is an attempt");
        assert_eq!(sink.driven_input().0, 3, "but nobody drove it");

        let bytes = reader_handle.join().expect("reader thread");
        assert_eq!(&bytes[..2], b"BN");
        assert!(bytes[2..expected_len - 1].iter().all(|byte| *byte == b'L'));
        assert_eq!(bytes[expected_len - 1], b'R');
        drop(writer);
    }

    #[test]
    fn a_driven_input_watch_follows_its_own_sink_only() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = SinkWriter::new(writer.as_raw_fd());
        let watch = sink.driven_input_watch();
        let other = SinkWriter::new(-1).driven_input_watch();
        let (before, theirs) = (watch.get(), other.get());
        assert_eq!(before, sink.driven_input());
        assert_eq!(sink.write_frame(b"K").expect("a driven write"), 1);
        assert_ne!(watch.get(), before, "a key moves it");
        assert_eq!(watch.get(), sink.driven_input());
        let keyed = watch.get();
        assert_eq!(sink.write_reply(b"R").expect("a reply"), 1);
        assert_eq!(watch.get(), keyed, "a reply does not");
        assert_eq!(other.get(), theirs, "another sink's count stays");
        let mut bytes = [0_u8; 2];
        reader.read_exact(&mut bytes).expect("both bytes");
        assert_eq!(&bytes, b"KR");
    }

    #[test]
    fn immediate_write_refuses_contention_without_delayed_injection() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        writer.set_nonblocking(true).expect("nonblocking writer");
        let sink = SinkWriter::new(writer.as_raw_fd());
        sink.note_master_nonblocking(true);

        let held = sink.shared.lock.lock().unwrap();
        assert_eq!(
            sink.try_write_frame_immediate(b"REJECTED"),
            ImmediateWrite::BusyZero,
            "try-lock contention must accept zero bytes"
        );
        drop(held);

        assert_eq!(
            sink.try_write_frame_immediate(b"accepted"),
            ImmediateWrite::Full
        );
        let mut got = [0_u8; 8];
        reader.read_exact(&mut got).expect("read accepted frame");
        assert_eq!(&got, b"accepted");
    }

    #[test]
    fn immediate_write_refuses_spill_without_delayed_injection() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        writer.set_nonblocking(true).expect("nonblocking writer");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
        sink.note_master_nonblocking(true);

        let mut filled = 0_usize;
        loop {
            match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
                Ok(n) if n > 0 => filled += n,
                _ => break,
            }
        }
        assert!(filled > 0, "socket send buffer filled");
        assert_eq!(
            sink.write_frame_nonparking(b"OLDER").expect("spill older"),
            5
        );
        assert!(!sink.shared.spill_is_empty(), "older frame entered spill");

        let started = std::time::Instant::now();
        assert_eq!(
            sink.try_write_frame_immediate(b"REJECTED"),
            ImmediateWrite::BusyZero
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "immediate refusal must not wait for the wedged peer"
        );

        let mut got = vec![0_u8; filled + 5];
        reader
            .read_exact(&mut got)
            .expect("drain fill and older frame");
        assert_eq!(&got[filled..], b"OLDER");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let settled = {
                let spill = sink.shared.spill.lock().unwrap();
                spill.buf.is_empty() && !spill.draining
            };
            if settled {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "spill did not settle");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        assert_eq!(
            sink.try_write_frame_immediate(b"accepted"),
            ImmediateWrite::Full
        );
        let mut accepted = [0_u8; 8];
        reader
            .read_exact(&mut accepted)
            .expect("read accepted frame");
        assert_eq!(&accepted, b"accepted");
    }

    #[test]
    fn owned_fd_stays_open_until_last_clone_drops() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let owned: OwnedFd = writer.into();
        let sink = Arc::new(SinkWriter::new_owned(owned));
        let clone = Arc::clone(&sink);

        drop(sink);
        assert_eq!(
            clone.project_fd_state(),
            (true, true),
            "fd_lifecycle projection: open and owned while a clone lives"
        );
        assert_eq!(
            clone
                .write_frame(b"alive")
                .expect("write while a clone holds the fd"),
            5
        );

        drop(clone);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).expect("read_to_end");
        assert_eq!(
            &buf, b"alive",
            "peer got the bytes then EOF — fd closed on last clone drop"
        );
    }

    #[test]
    fn small_write_below_spill_cap_returns_promptly_and_preserves_order() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

        writer.set_nonblocking(true).expect("nonblocking for fill");
        let mut wedged = 0usize;
        loop {
            match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
                Ok(n) if n > 0 => wedged += n,
                _ => break,
            }
        }
        writer.set_nonblocking(false).expect("back to blocking");
        assert!(wedged > 0, "buffer filled");

        let t0 = std::time::Instant::now();
        assert_eq!(
            sink.write_frame_nonparking(b"AAAA")
                .expect("spill accepted"),
            4
        );
        assert_eq!(
            sink.write_frame_nonparking(b"BBBB")
                .expect("spill accepted"),
            4
        );
        assert_eq!(sink.write_frame(b"CCCC").expect("spill accepted"), 4);
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "small writes below a fresh spill cap must not wait for the wedge to clear"
        );

        let (completion_tx, completion_rx) = std::sync::mpsc::channel();
        let completion_sink = sink.clone();
        let completion_thread = std::thread::spawn(move || {
            completion_tx
                .send(completion_sink.wait_egress_drained_to_kernel())
                .expect("completion receiver alive");
        });

        let mut got = Vec::new();
        let expect = wedged + 12;
        let mut chunk = [0u8; 65536];
        while got.len() < expect {
            let n = reader.read(&mut chunk).expect("drain");
            assert!(n > 0, "peer closed early");
            got.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(&got[wedged..], b"AAAABBBBCCCC", "spill delivered in order");
        assert!(
            completion_rx
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("completion fence settles after drain")
        );
        completion_thread.join().expect("completion thread");

        assert_eq!(sink.write_frame_nonparking(b"D").expect("direct"), 1);
        let mut one = [0u8; 8];
        let n = reader.read(&mut one).expect("read D");
        assert_eq!(&one[..n], b"D");
    }

    #[test]
    fn egress_completion_fails_closed_when_the_spill_peer_dies() {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

        writer.set_nonblocking(true).expect("nonblocking for fill");
        let mut wedged = 0usize;
        loop {
            match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
                Ok(n) if n > 0 => wedged += n,
                _ => break,
            }
        }
        writer.set_nonblocking(false).expect("back to blocking");
        assert!(wedged > 0, "buffer filled");
        assert_eq!(sink.write_frame_nonparking(b"lost").expect("spill"), 4);
        assert!(!sink.egress_drained_to_kernel());

        drop(reader);
        assert!(
            !sink.wait_egress_drained_to_kernel(),
            "peer-death discard cannot satisfy the blocking completion fence"
        );
        assert!(
            !sink.wait_egress_drained_to_kernel(),
            "the failure is sticky: a second completion fence over the same sink still fails"
        );
        assert!(
            sink.egress_drained_to_kernel(),
            "nothing process-local remains after the discard: the polling handoff fence is settled"
        );
        assert_eq!(
            sink.try_egress_drained_to_kernel(),
            Some(true),
            "the non-parking twin agrees with the polling fence"
        );
    }

    #[test]
    fn egress_drained_predicate_follows_the_spill() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
        assert!(
            sink.egress_drained_to_kernel(),
            "a fresh sink has nothing in its process-local egress"
        );

        writer.set_nonblocking(true).expect("nonblocking for fill");
        let mut wedged = 0usize;
        loop {
            match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
                Ok(n) if n > 0 => wedged += n,
                _ => break,
            }
        }
        writer.set_nonblocking(false).expect("back to blocking");
        assert!(wedged > 0, "buffer filled");

        assert_eq!(sink.write_frame_nonparking(b"held").expect("spill"), 4);
        assert!(
            !sink.egress_drained_to_kernel(),
            "tolerated bytes trapped in the spill must read as NOT drained"
        );

        let mut sink_bytes = Vec::new();
        let mut chunk = [0u8; 65536];
        while sink_bytes.len() < wedged + 4 {
            let n = reader.read(&mut chunk).expect("drain");
            assert!(n > 0, "peer closed early");
            sink_bytes.extend_from_slice(&chunk[..n]);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !sink.egress_drained_to_kernel() {
            assert!(
                std::time::Instant::now() < deadline,
                "the spill never drained to the kernel"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            &sink_bytes[wedged..],
            b"held",
            "the held bytes reached the kernel"
        );
    }

    #[test]
    fn declared_nonblocking_master_writes_whole_frames_in_order() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
        assert!(
            !sink.shared.master_nonblocking.load(Ordering::Relaxed),
            "a fresh sink assumes the description may be BLOCKING (per-byte cadence)"
        );

        writer.set_nonblocking(true).expect("nonblocking master");
        sink.note_master_nonblocking(true);
        assert!(sink.shared.master_nonblocking.load(Ordering::Relaxed));

        assert_eq!(
            sink.write_frame_nonparking(b"whole-frame").expect("write"),
            11
        );
        assert!(
            sink.shared.spill_is_empty(),
            "an unwedged fd must not spill on the whole-frame unit"
        );
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).expect("read");
        assert_eq!(&buf[..n], b"whole-frame");

        let mut wedged = 0usize;
        loop {
            match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
                Ok(n) if n > 0 => wedged += n,
                _ => break,
            }
        }
        assert!(wedged > 0, "buffer filled");
        assert_eq!(sink.write_frame_nonparking(b"SPLIT").expect("accepted"), 5);

        let mut got = Vec::new();
        let expect = wedged + 5;
        let mut chunk = [0u8; 65536];
        while got.len() < expect {
            let n = reader.read(&mut chunk).expect("drain");
            assert!(n > 0, "peer closed early");
            got.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(&got[wedged..], b"SPLIT", "spill delivered in order");
    }

    #[test]
    fn contended_nonparking_write_concedes_within_a_bounded_spin() {
        const HOLD_CAP: std::time::Duration = std::time::Duration::from_secs(120);
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

        let holding = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let sink = Arc::clone(&sink);
            let holding = Arc::clone(&holding);
            let released = Arc::clone(&released);
            thread::spawn(move || {
                let guard = sink.shared.lock.lock().unwrap_or_else(|p| p.into_inner());
                holding.store(true, Ordering::SeqCst);
                let _ = release_rx.recv_timeout(HOLD_CAP);
                released.store(true, Ordering::SeqCst);
                drop(guard);
            })
        };
        while !holding.load(Ordering::SeqCst) {
            std::hint::spin_loop();
        }

        let t0 = std::time::Instant::now();
        assert_eq!(sink.write_frame_nonparking(b"K").expect("accepted"), 1);
        let took = t0.elapsed();
        assert!(
            !released.load(Ordering::SeqCst),
            "the write returned only after the holder let go: a wait on the holder, not a \
             bounded spin (took {took:?})"
        );
        assert!(
            took < aterm_spec::wait::MUST_HAPPEN,
            "the retry budget must be a bounded spin, not a wait on the holder (took {took:?})"
        );

        release_tx.send(()).expect("holder alive");
        holder.join().expect("holder thread");
        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).expect("read");
        assert_eq!(&buf[..n], b"K");
    }
}

#[cfg(all(test, unix))]
mod p04_direct_receipt_and_deferred_drainer_tests {
    use super::*;
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sink_and_reader() -> (Arc<SinkWriter>, UnixStream) {
        let (reader, writer) = UnixStream::pair().expect("socketpair");
        reader
            .set_read_timeout(Some(std::time::Duration::from_secs(60)))
            .expect("timeout");
        let owned: OwnedFd = writer.into();
        (Arc::new(SinkWriter::new_owned(owned)), reader)
    }

    fn read_exactly(reader: &mut UnixStream, want: usize) -> Vec<u8> {
        let mut out = vec![0u8; want];
        reader.read_exact(&mut out).expect("bytes reach the peer");
        out
    }

    #[test]
    fn a_whole_frame_on_the_direct_lane_is_direct_and_a_spilled_one_is_not() {
        let (sink, mut reader) = sink_and_reader();
        let direct = sink
            .write_frame_nonparking_with_receipt(b"a")
            .expect("write");
        assert!(
            direct.is_direct(),
            "an uncontended keystroke goes straight to the kernel"
        );
        assert_eq!(direct.accepted(), 1);
        let held = sink.shared.lock.lock().unwrap();
        let spilled = sink
            .write_frame_nonparking_with_receipt(b"b")
            .expect("spill");
        assert!(!spilled.is_direct(), "a conceded frame sits in the spill");
        assert_eq!(
            spilled.accepted(),
            1,
            "…but it IS accepted (ordered delivery)"
        );
        drop(held);
        assert_eq!(read_exactly(&mut reader, 2), b"ab");
    }

    #[test]
    fn the_egress_fence_keeps_every_frame_out_of_the_kernel_until_it_lifts() {
        let (sink, mut reader) = sink_and_reader();
        let mid_frame = sink.shared.lock.lock().unwrap();
        assert!(sink.try_fence_egress().is_none(), "a frame part-way in");
        drop(mid_frame);
        let fence = sink.try_fence_egress().expect("nothing in flight");
        assert_eq!(sink.write_frame_nonparking(b"after").expect("accepted"), 5);
        assert_eq!(sink.spilled_bytes(), 5, "queued whole behind the fence");
        reader.set_nonblocking(true).expect("nonblocking");
        assert_eq!(
            reader.read(&mut [0u8; 8]).map_err(|e| e.kind()),
            Err(std::io::ErrorKind::WouldBlock),
            "no byte reached the kernel"
        );
        reader.set_nonblocking(false).expect("blocking");
        drop(fence);
        assert_eq!(read_exactly(&mut reader, 5), b"after");
    }

    #[test]
    fn a_concession_with_an_arranger_defers_the_spawn_to_the_worker() {
        static POKES: AtomicUsize = AtomicUsize::new(0);
        let (sink, mut reader) = sink_and_reader();
        sink.install_spill_arranger(|| {
            POKES.fetch_add(1, Ordering::SeqCst);
        });
        let held = sink.shared.lock.lock().unwrap();
        let receipt = sink
            .write_frame_nonparking_with_receipt(b"xyz")
            .expect("concede to the spill");
        assert_eq!(receipt.accepted(), 3);
        assert!(!receipt.is_direct());
        assert_eq!(
            POKES.load(Ordering::SeqCst),
            1,
            "the arranger was poked exactly once"
        );
        {
            let s = sink.shared.spill.lock().unwrap();
            assert!(
                s.draining && s.arrange_pending,
                "committed under a PENDING mark, no thread yet"
            );
            assert_eq!(s.buf.len(), 3);
        }
        let receipt2 = sink
            .write_frame_nonparking_with_receipt(b"!")
            .expect("append");
        assert_eq!(receipt2.accepted(), 1);
        assert_eq!(
            POKES.load(Ordering::SeqCst),
            2,
            "pending ⇒ the poke is renewed"
        );
        drop(held);
        assert_eq!(sink.try_egress_drained_to_kernel(), Some(false));
        sink.arrange_pending_drainer();
        assert_eq!(read_exactly(&mut reader, 4), b"xyz!");
        let t0 = std::time::Instant::now();
        while sink.try_egress_drained_to_kernel() != Some(true) {
            assert!(
                t0.elapsed() < std::time::Duration::from_secs(60),
                "drainer never emptied"
            );
            std::thread::yield_now();
        }
        assert!(!sink.shared.spill.lock().unwrap().arrange_pending);
    }

    #[test]
    fn without_an_arranger_the_concession_arranges_inline_as_before() {
        let (sink, mut reader) = sink_and_reader();
        let held = sink.shared.lock.lock().unwrap();
        let receipt = sink
            .write_frame_nonparking_with_receipt(b"q")
            .expect("concede");
        assert_eq!(receipt.accepted(), 1);
        {
            let s = sink.shared.spill.lock().unwrap();
            assert!(
                s.draining && !s.arrange_pending,
                "inline arrangement: the thread exists"
            );
        }
        drop(held);
        assert_eq!(read_exactly(&mut reader, 1), b"q");
    }

    #[test]
    fn the_drainer_never_parks_in_its_write_under_the_spill_mutex() {
        for declared in [false, true] {
            let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
            writer.set_nonblocking(true).expect("nonblocking for fill");
            while let Ok(n) = aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
                if n == 0 {
                    break;
                }
            }
            writer.set_nonblocking(declared).expect("the description");
            let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
            sink.note_master_nonblocking(declared);
            let (at_tx, at_rx) = std::sync::mpsc::channel::<bool>();
            let at_tx = Mutex::new(at_tx);
            let shared = Arc::downgrade(&sink.shared);
            let fired = AtomicBool::new(false);
            sink.seam_for_testing(move |seam| {
                if seam != Seam::DrainWrite || fired.swap(true, Ordering::SeqCst) {
                    return;
                }
                let free = shared
                    .upgrade()
                    .is_some_and(|shared| shared.spill.try_lock().is_ok());
                let _ = at_tx.lock().expect("at").send(free);
            });
            assert_eq!(sink.write_frame_nonparking(b"s").expect("spilled"), 1);
            let free = at_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the drainer reached its write");
            assert_eq!(
                free, !declared,
                "declared={declared}: the mutex at the write"
            );
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let key = {
                let sink = Arc::clone(&sink);
                std::thread::spawn(move || {
                    let _ = done_tx.send(sink.write_frame_nonparking(b"k").map_err(|e| e.kind()));
                })
            };
            assert_eq!(
                done_rx
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .expect("the key never queued behind a parked drainer"),
                Ok(1)
            );
            key.join().expect("the key");
            drop(reader);
        }
    }

    #[test]
    fn the_guarded_writes_rest_defers_its_drainer_to_the_arranger() {
        static POKES: AtomicUsize = AtomicUsize::new(0);
        for arranged in [true, false] {
            let (read_end, write_end) = std::io::pipe().expect("pipe");
            let (read_end, write_end): (OwnedFd, OwnedFd) = (read_end.into(), write_end.into());
            aterm_pty::set_nonblocking(write_end.as_raw_fd(), true).expect("nonblocking");
            let mut filled = 0_usize;
            while let Ok(n) = aterm_pty::write_some(write_end.as_raw_fd(), &[b'.'; 4096]) {
                filled += n;
            }
            let sink = SinkWriter::new_owned(write_end);
            sink.note_master_nonblocking(true);
            if arranged {
                sink.install_spill_arranger(|| {
                    POKES.fetch_add(1, Ordering::SeqCst);
                });
            }
            let mut reader = std::fs::File::from(read_end);
            let room = filled.min(8 * 1024);
            let mut made = vec![0_u8; room];
            reader.read_exact(&mut made).expect("make partial room");

            let frame = vec![b'P'; SinkWriter::IMMEDIATE_FRAME_MAX];
            let (outcome, _) = sink.try_write_frame_whole_with_receipt(&frame);
            assert!(
                matches!(outcome, ImmediateWrite::Queued { accepted } if accepted < frame.len()),
                "{outcome:?}"
            );
            {
                let s = sink.shared.spill.lock().unwrap();
                assert!(s.draining, "a drainer owes the rest");
                assert_eq!(s.arrange_pending, arranged, "pending iff deferred");
            }
            if arranged {
                assert_eq!(POKES.load(Ordering::SeqCst), 1, "poked exactly once");
                sink.arrange_pending_drainer();
            }
            aterm_pty::set_nonblocking(reader.as_raw_fd(), true).expect("nonblocking reader");
            let want = filled - room + frame.len();
            let mut landed = Vec::with_capacity(want);
            let came = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                std::time::Duration::from_millis(5),
                || {
                    let mut buf = [0_u8; 16 * 1024];
                    if let Ok(n) = reader.read(&mut buf) {
                        landed.extend_from_slice(&buf[..n]);
                    }
                    (landed.len() >= want).then_some(())
                },
            );
            assert!(came.is_some(), "{arranged}: {} of {want}", landed.len());
            assert!(landed[..filled - room].iter().all(|b| *b == b'.'));
            assert!(landed[filled - room..].iter().all(|b| *b == b'P'));
        }
    }
}

#[cfg(all(test, unix))]
mod bulk_meter_tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    fn sink_and_reader() -> (Arc<SinkWriter>, UnixStream) {
        let (reader, writer) = UnixStream::pair().expect("socketpair");
        reader
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("timeout");
        let owned: OwnedFd = writer.into();
        (Arc::new(SinkWriter::new_owned(owned)), reader)
    }

    fn bracketed(body: &[u8]) -> Vec<u8> {
        [PASTE_OPEN, body, PASTE_CLOSE].concat()
    }

    #[test]
    fn a_stop_cut_keeps_the_frame_well_formed() {
        let plain = "ab\u{e9}cd".as_bytes(); // a b [c3 a9] c d
        assert_eq!(bulk_stop_cut(plain, 0), (0, plain.len()));
        assert_eq!(bulk_stop_cut(plain, 1), (1, plain.len()));
        assert_eq!(bulk_stop_cut(plain, 3), (4, plain.len()));
        let framed = bracketed(b"hello");
        let len = framed.len();
        assert_eq!(bulk_stop_cut(&framed, 2), (6, len - 6));
        assert_eq!(bulk_stop_cut(&framed, 8), (8, len - 6));
        assert_eq!(bulk_stop_cut(&framed, len - 3), (len - 3, len - 3));
        assert_eq!(bulk_stop_cut(&framed, 0), (0, len));
    }

    #[test]
    fn a_frame_stopped_before_it_begins_writes_nothing() {
        let (sink, mut reader) = sink_and_reader();
        let meter = BulkMeter::new();
        meter.request_stop();
        let receipt = sink
            .write_frame_metered_with_receipt(&bracketed(&[b'x'; 4096]), &meter)
            .expect("write");
        assert_eq!(receipt.accepted(), 0);
        assert_eq!(meter.progress().state, BulkState::Stopped);
        assert_eq!(meter.progress().sent, 0);
        sink.write_frame(b"k").expect("the next input goes through");
        let mut one = [0u8; 1];
        reader.read_exact(&mut one).expect("read");
        assert_eq!(&one, b"k", "the stopped frame sent no byte");
    }

    #[test]
    fn a_metered_frame_reports_progress_and_a_stop_drops_the_unsent_middle() {
        let (sink, mut reader) = sink_and_reader();
        let body = vec![b'p'; 1 << 20];
        let frame = bracketed(&body);
        let meter = Arc::new(BulkMeter::new());
        let writer = {
            let (sink, meter, frame) = (sink.clone(), meter.clone(), frame.clone());
            std::thread::spawn(move || {
                let receipt = sink
                    .write_frame_metered_with_receipt(&frame, &meter)
                    .expect("write");
                sink.write_frame(b"K").expect("the key after it");
                receipt
            })
        };
        let mut got = vec![0u8; 64 * 1024];
        reader.read_exact(&mut got).expect("read a prefix");
        let deadline = Instant::now() + Duration::from_secs(60);
        let progress = loop {
            let p = meter.progress();
            if p.sent >= got.len() as u64 || Instant::now() > deadline {
                break p;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(progress.state, BulkState::Writing);
        assert_eq!(progress.total, frame.len() as u64);
        assert!(progress.sent >= got.len() as u64, "{progress:?}");
        assert!(progress.sent < frame.len() as u64, "the writer is parked");
        meter.request_stop();
        let mut rest = Vec::new();
        let mut chunk = [0u8; 8192];
        while !rest.ends_with(b"K") {
            let n = reader.read(&mut chunk).expect("read the rest");
            assert!(n > 0, "peer open");
            rest.extend_from_slice(&chunk[..n]);
        }
        let receipt = writer.join().expect("writer");
        got.extend_from_slice(&rest);
        assert_eq!(got.last(), Some(&b'K'), "the key follows the paste");
        got.pop();
        assert!(got.starts_with(PASTE_OPEN));
        assert!(got.ends_with(PASTE_CLOSE), "the paste is closed");
        let middle = &got[PASTE_OPEN.len()..got.len() - PASTE_CLOSE.len()];
        assert!(middle.iter().all(|b| *b == b'p'));
        assert!(middle.len() < body.len(), "the unsent middle was dropped");
        assert_eq!(receipt.accepted(), got.len());
        assert!(!receipt.is_direct(), "a cut frame is not a whole one");
        let end = meter.progress();
        assert_eq!(end.state, BulkState::Stopped);
        assert_eq!(end.sent, got.len() as u64);
    }

    #[test]
    fn an_unstopped_metered_frame_is_the_plain_frame() {
        let (sink, mut reader) = sink_and_reader();
        let frame = bracketed(b"hello world");
        let meter = BulkMeter::new();
        let receipt = sink
            .write_frame_metered_with_receipt(&frame, &meter)
            .expect("write");
        assert!(receipt.is_direct());
        let mut got = vec![0u8; frame.len()];
        reader.read_exact(&mut got).expect("read");
        assert_eq!(got, frame);
        assert_eq!(
            meter.progress(),
            BulkProgress {
                sent: frame.len() as u64,
                total: frame.len() as u64,
                state: BulkState::Delivered,
            }
        );
    }
}

#[cfg(test)]
mod metered_body_tests {
    use super::*;

    fn bracketed(body: &[u8]) -> Vec<u8> {
        [PASTE_OPEN, body, PASTE_CLOSE].concat()
    }

    #[derive(Clone, Copy)]
    enum StopAt {
        Never,
        After(usize),
        Inside(usize),
    }

    fn windows_body(frame: &[u8], stop: StopAt) -> (MeteredEnd, usize, Vec<u8>) {
        let shared = Shared::new();
        let meter = BulkMeter::new();
        let since = shared.discard_epoch();
        let mut conpty = Vec::new();
        let mut writes = 0usize;
        let mut conpty_write = |bytes: &[u8], flag: Option<&AtomicBool>| {
            writes += 1;
            if matches!(stop, StopAt::Inside(n) if n == writes) {
                meter.request_stop();
            }
            if flag.is_some_and(|f| f.load(Ordering::Acquire)) {
                return Ok(0);
            }
            conpty.extend_from_slice(bytes);
            if matches!(stop, StopAt::After(n) if n == writes) {
                meter.request_stop();
            }
            Ok(bytes.len())
        };
        let end = write_metered(frame, &meter, |rest| {
            shared.write_stamped_through(rest, since, Some(&meter), &mut conpty_write)
        })
        .expect("an in-memory write never fails");
        let owed = deliver_owed_through(&end.owed, |rest| {
            shared.write_stamped_through(rest, since, None, &mut conpty_write)
        });
        let accepted = end.accepted + owed;
        (end, accepted, conpty)
    }

    #[test]
    fn a_stop_reaches_the_windows_write_and_the_paste_ends_closed() {
        let mut body = vec![b'p'; METERED_SLICE - PASTE_OPEN.len() - 1];
        body.extend_from_slice("\u{e9}".as_bytes());
        body.extend(vec![b'q'; 3 * METERED_SLICE]);
        let frame = bracketed(&body);
        let mut want = frame[..METERED_SLICE + 1].to_vec();
        want.extend_from_slice(PASTE_CLOSE);
        for stop in [StopAt::After(1), StopAt::Inside(2)] {
            let (end, accepted, conpty) = windows_body(&frame, stop);
            assert_eq!(
                end.owed,
                [&frame[METERED_SLICE..=METERED_SLICE], PASTE_CLOSE].concat(),
                "the write the stop reached wrote nothing; the cut's debt came back"
            );
            assert_eq!(conpty, want, "the head, the rest of `é`, the close");
            assert_eq!(accepted, conpty.len());
            assert!(std::str::from_utf8(&conpty).is_ok(), "no half character");
        }
        let (end, accepted, conpty) = windows_body(&frame, StopAt::Inside(1));
        assert!(end.owed.is_empty());
        assert_eq!((accepted, conpty.len()), (0, 0));
    }

    #[test]
    fn the_windows_twin_writes_an_unstopped_frame_whole() {
        let frame = bracketed(&vec![b'p'; 3 * METERED_SLICE]);
        let (end, accepted, conpty) = windows_body(&frame, StopAt::Never);
        assert_eq!(conpty, frame);
        assert_eq!(accepted, frame.len());
        assert!(end.owed.is_empty());
    }

    #[test]
    fn the_close_a_windows_stop_owes_waits_until_a_sever_releases_it() {
        let shared = Shared::new();
        let meter = BulkMeter::new();
        let since = shared.discard_epoch();
        let frame = bracketed(&vec![b'p'; 3 * METERED_SLICE]);
        let mut conpty = Vec::new();
        let mut writes = Vec::new();
        let mut conpty_write = |bytes: &[u8], flag: Option<&AtomicBool>| {
            writes.push(flag.is_some());
            match writes.len() {
                1 => {
                    conpty.extend_from_slice(bytes);
                    Ok(bytes.len())
                }
                2 => {
                    meter.request_stop();
                    Ok(if flag.is_some_and(|f| f.load(Ordering::Acquire)) {
                        0
                    } else {
                        bytes.len()
                    })
                }
                _ => {
                    meter.request_stop();
                    if flag.is_some_and(|f| f.load(Ordering::Acquire)) {
                        return Ok(0);
                    }
                    shared.severed.store(true, Ordering::Release);
                    Ok(0)
                }
            }
        };
        let end = write_metered(&frame, &meter, |rest| {
            shared.write_stamped_through(rest, since, Some(&meter), &mut conpty_write)
        })
        .expect("an in-memory write never fails");
        assert_eq!(end.owed, PASTE_CLOSE, "the cut owes the close");
        let owed = deliver_owed_through(&end.owed, |rest| {
            shared.write_stamped_through(rest, since, None, &mut conpty_write)
        });
        assert_eq!(
            writes,
            [true, true, false],
            "the frame's writes carry its stop; the owed close's carries none"
        );
        assert!(
            shared.is_severed(),
            "only the sever ended the close's write"
        );
        assert_eq!(owed, 0, "the severed close wrote nothing");
        assert_eq!(
            conpty,
            frame[..METERED_SLICE],
            "the program got the head alone"
        );
    }

    #[test]
    fn a_give_up_hands_back_what_the_cut_owes() {
        let gave_up_at = |frame: &[u8], stop: bool, at: usize| {
            let meter = BulkMeter::new();
            let mut off = 0usize;
            let end = write_metered(frame, &meter, |rest| {
                if off >= at {
                    if stop {
                        meter.request_stop();
                    }
                    return Ok(0);
                }
                let n = rest.len().min(at - off);
                off += n;
                Ok(n)
            })
            .expect("no error");
            assert_eq!(end.accepted, at.min(frame.len()));
            end.owed
        };
        let frame = bracketed("ab\u{e9}cd".as_bytes());
        assert_eq!(
            gave_up_at(&frame, true, 2),
            [&PASTE_OPEN[2..], PASTE_CLOSE].concat()
        );
        assert_eq!(
            gave_up_at(&frame, true, 9),
            [&frame[9..10], PASTE_CLOSE].concat()
        );
        assert_eq!(gave_up_at(&frame, true, 10), PASTE_CLOSE.to_vec());
        let into_close = frame.len() - 2;
        assert_eq!(
            gave_up_at(&frame, true, into_close),
            frame[into_close..].to_vec()
        );
        assert!(gave_up_at(&frame, true, 0).is_empty());
        assert!(gave_up_at(&frame, false, 10).is_empty());
        let plain = "ab\u{e9}cd".as_bytes();
        assert_eq!(gave_up_at(plain, true, 3), plain[3..4].to_vec());
        assert!(gave_up_at(plain, true, 4).is_empty());
    }
}

#[cfg(all(test, unix))]
mod input_io_tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn wedged() -> (Arc<SinkWriter>, UnixStream, usize) {
        let (reader, writer) = UnixStream::pair().expect("socketpair");
        reader
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("timeout");
        writer.set_nonblocking(true).expect("nonblocking master");
        let mut filled = 0usize;
        loop {
            match aterm_pty::write_some(writer.as_raw_fd(), &[b'.'; 4096]) {
                Ok(n) if n > 0 => filled += n,
                _ => break,
            }
        }
        assert!(filled > 0, "the socket took bytes before it filled");
        let owned: OwnedFd = writer.into();
        let sink = Arc::new(SinkWriter::new_owned(owned));
        sink.note_master_nonblocking(true);
        (sink, reader, filled)
    }

    fn bracketed(body: &[u8]) -> Vec<u8> {
        [PASTE_OPEN, body, PASTE_CLOSE].concat()
    }

    fn parks(sink: &SinkWriter, park: Seam) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel();
        sink.seam_for_testing(move |seam| {
            if seam == park {
                let _ = tx.send(());
            }
        });
        rx
    }

    fn parked_after(parks: &mpsc::Receiver<()>) {
        while parks.try_recv().is_ok() {}
        parks
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the blocking writer parked on the full queue");
    }

    fn fill_spill_to_the_cap(sink: &SinkWriter) -> Vec<Vec<u8>> {
        let mut accepted = Vec::new();
        for i in 0usize.. {
            let frame = vec![b'a' + (i % 26) as u8; 4097];
            match sink.write_frame_interactive_with_receipt(&frame) {
                Ok(receipt) => {
                    assert_eq!(receipt.accepted(), frame.len());
                    accepted.push(frame);
                }
                Err(e) => {
                    assert!(e.is_refused(), "the only refusal is the cap: {e}");
                    break;
                }
            }
            assert!(i < 1024, "the cap never refused");
        }
        assert!(sink.shared.spill_len() > Shared::SPILL_CAP);
        accepted
    }

    #[test]
    fn a_stop_reaches_a_paste_parked_on_a_tty_that_is_not_draining() {
        let (sink, mut reader, filled) = wedged();
        let parks = parks(&sink, Seam::WriterPark);
        let frame = bracketed(&vec![b'p'; 256 * 1024]);
        let meter = Arc::new(BulkMeter::new());
        let (tx, rx) = mpsc::channel();
        let writer = {
            let (sink, meter, frame) = (sink.clone(), meter.clone(), frame.clone());
            std::thread::spawn(move || {
                let receipt = sink.write_frame_metered_with_receipt(&frame, &meter);
                tx.send(receipt.map(|r| r.accepted()).map_err(|e| e.to_string()))
                    .expect("receiver");
            })
        };
        let mut head = vec![0u8; filled + 64 * 1024];
        reader.read_exact(&mut head).expect("the fill and a head");
        parked_after(&parks);
        let before = meter.progress();
        assert!(before.sent > 0, "the paste began: {before:?}");
        assert_eq!(before.state, BulkState::Writing, "{before:?}");
        assert!(before.sent < frame.len() as u64, "the writer is parked");

        meter.request_stop();
        let accepted = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the stop reached the parked writer")
            .expect("a stop is not an error");
        writer.join().expect("writer");
        let end = meter.progress();
        assert_eq!(end.state, BulkState::Stopped);
        assert_eq!(end.sent, accepted as u64, "the meter is the bytes accepted");
        assert!(accepted > PASTE_OPEN.len() && accepted < frame.len());

        let (tx, rx) = mpsc::channel();
        {
            let sink = sink.clone();
            std::thread::spawn(move || {
                tx.send(sink.write_frame(b"K").map_err(|e| e.to_string()))
                    .expect("receiver");
            });
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(60)).expect("K queued"),
            Ok(1)
        );

        let mut delivered = head.split_off(filled);
        let mut chunk = [0u8; 65536];
        while delivered.last() != Some(&b'K') {
            let n = reader
                .read(&mut chunk)
                .expect("the key reaches the program");
            assert!(n > 0, "peer closed early");
            delivered.extend_from_slice(&chunk[..n]);
        }
        delivered.pop();
        assert_eq!(delivered.len(), accepted, "exactly the accepted bytes");
        assert!(delivered.starts_with(PASTE_OPEN));
        assert!(
            delivered.ends_with(PASTE_CLOSE),
            "the owed close reaches the program ahead of the key"
        );
        let middle = &delivered[PASTE_OPEN.len()..delivered.len() - PASTE_CLOSE.len()];
        assert!(middle.iter().all(|b| *b == b'p'), "only the paste's text");
    }

    #[test]
    fn a_stop_ends_a_paste_waiting_for_spill_room() {
        let (sink, _reader, _filled) = wedged();
        fill_spill_to_the_cap(&sink);
        let spilled = sink.shared.spill_len();
        let meter = Arc::new(BulkMeter::new());
        let (tx, rx) = mpsc::channel();
        let writer = {
            let (sink, meter) = (sink.clone(), meter.clone());
            std::thread::spawn(move || {
                let receipt = sink.write_frame_metered_with_receipt(&bracketed(b"late"), &meter);
                tx.send(receipt.map(|r| r.accepted()).map_err(|e| e.to_string()))
                    .expect("receiver");
            })
        };
        meter.request_stop();
        let accepted = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the stop ended the room wait")
            .expect("a stop is not an error");
        writer.join().expect("writer");
        assert_eq!(accepted, 0);
        assert_eq!(meter.progress().state, BulkState::Stopped);
        assert_eq!(meter.progress().sent, 0);
        assert_eq!(sink.shared.spill_len(), spilled, "nothing was appended");
        sink.sever_input();
    }

    #[test]
    fn the_interactive_egress_refuses_at_the_spill_cap_and_never_parks() {
        let (sink, mut reader, filled) = wedged();
        let accepted = fill_spill_to_the_cap(&sink);

        let (tx, rx) = mpsc::channel();
        {
            let sink = sink.clone();
            std::thread::spawn(move || {
                let refused = sink
                    .write_frame_interactive_with_receipt(b"K")
                    .expect_err("a key into a full input queue is refused");
                tx.send((refused.is_refused(), refused.accepted(), refused.order()))
                    .expect("receiver");
            });
        }
        let (is_refused, accepted_bytes, order) = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the interactive egress did not park");
        assert!(is_refused);
        assert_eq!(accepted_bytes, 0, "no byte of a refused key is accepted");
        assert_eq!(order, None);
        let dead = SinkWriter::new(-1)
            .write_frame_interactive_with_receipt(b"x")
            .expect_err("a dead fd fails");
        assert!(
            !dead.is_refused(),
            "a dead session is a failure, not a refusal"
        );

        let (tx, rx) = mpsc::channel();
        let parked = {
            let sink = sink.clone();
            std::thread::spawn(move || {
                tx.send(sink.write_frame_nonparking(b"P").map_err(|e| e.to_string()))
                    .expect("receiver");
            })
        };

        let expect = filled + accepted.iter().map(Vec::len).sum::<usize>() + 1;
        let mut got = vec![0u8; expect];
        reader
            .read_exact(&mut got)
            .expect("everything accepted arrives");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(60)).expect("unparked"),
            Ok(1)
        );
        parked.join().expect("parked writer");
        assert!(got[..filled].iter().all(|b| *b == b'.'));
        let mut at = filled;
        for frame in &accepted {
            assert_eq!(&got[at..at + frame.len()], frame.as_slice(), "in order");
            at += frame.len();
        }
        assert_eq!(&got[at..], b"P");
        assert!(!got.contains(&b'K'), "the refused key was never delivered");

        let deadline = Instant::now() + Duration::from_secs(60);
        while !sink.egress_drained_to_kernel() {
            assert!(Instant::now() < deadline, "the spill drained");
            std::thread::sleep(Duration::from_millis(1));
        }
        let receipt = sink
            .write_frame_interactive_with_receipt(b"K")
            .expect("the retyped key goes through");
        assert_eq!(receipt.accepted(), 1);
        let mut k = [0u8; 1];
        reader.read_exact(&mut k).expect("read K");
        assert_eq!(&k, b"K");
    }

    #[test]
    fn a_large_interactive_frame_spills_instead_of_parking() {
        let (sink, mut reader, filled) = wedged();
        let frame = vec![b'L'; 3 * SinkWriter::NONPARK_MAX];
        let (tx, rx) = mpsc::channel();
        {
            let (sink, frame) = (sink.clone(), frame.clone());
            std::thread::spawn(move || {
                let receipt = sink
                    .write_frame_interactive_with_receipt(&frame)
                    .expect("spilled");
                tx.send((receipt.accepted(), receipt.is_direct()))
                    .expect("receiver");
            });
        }
        let (accepted, direct) = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the large interactive frame did not park");
        assert_eq!(accepted, frame.len());
        assert!(!direct);
        let mut got = vec![0u8; filled + frame.len()];
        reader.read_exact(&mut got).expect("delivered once read");
        assert_eq!(&got[filled..], frame.as_slice());
    }

    #[test]
    fn a_sever_ends_a_drainer_parked_on_a_full_queue() {
        let (sink, _reader, _filled) = wedged();
        let parks = parks(&sink, Seam::DrainPark);
        assert_eq!(sink.write_frame_nonparking(b"spilled").expect("spill"), 7);
        parks
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the drainer parked on the full queue");
        sink.sever_input();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the parked drainer ended at the sever",
            || {
                let s = sink.shared.spill.lock().unwrap_or_else(|p| p.into_inner());
                (!s.draining).then_some(())
            },
        );
    }

    #[test]
    fn a_sever_unblocks_every_waiter() {
        let (sink, _reader, _filled) = wedged();
        let parks = parks(&sink, Seam::WriterPark);
        let (tx, rx) = mpsc::channel::<(&str, Result<usize, String>)>();
        let spawn = |name: &'static str, bytes: Vec<u8>| {
            let (sink, tx) = (sink.clone(), tx.clone());
            std::thread::spawn(move || {
                let r = sink.write_frame(&bytes).map_err(|e| e.to_string());
                tx.send((name, r)).expect("receiver");
            })
        };
        let paste = spawn("paste", vec![b'p'; 64 * 1024]);
        parked_after(&parks);
        let mut keys = 0usize;
        while sink.shared.spill_len() <= Shared::SPILL_CAP {
            let receipt = sink
                .write_frame_interactive_with_receipt(&[b'k'; 4097])
                .expect("spilled");
            assert!(!receipt.is_direct());
            keys += 1;
            assert!(keys < 1024);
        }
        let waiter = spawn("room", b"w".to_vec());

        sink.sever_input();
        let mut returned = Vec::new();
        for _ in 0..2 {
            let (name, _) = rx
                .recv_timeout(Duration::from_secs(60))
                .expect("the sever unblocked a waiter");
            returned.push(name);
        }
        returned.sort_unstable();
        assert_eq!(returned, ["paste", "room"]);
        paste.join().expect("paste");
        waiter.join().expect("waiter");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !sink.egress_drained_to_kernel() || sink.shared.spill.lock().unwrap().draining {
            assert!(Instant::now() < deadline, "the drainer exited");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(sink.is_severed());
        assert!(
            !sink.wait_egress_drained_to_kernel(),
            "the dropped spill is a loss"
        );
        for write in [
            sink.write_frame_with_receipt(b"x").map(|r| r.accepted()),
            sink.write_frame_interactive_with_receipt(b"x")
                .map(|r| r.accepted()),
            sink.write_frame_nonparking_with_receipt(b"x")
                .map(|r| r.accepted()),
        ] {
            let error = write
                .expect_err("a severed sink takes nothing")
                .into_error();
            assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        }
        assert_eq!(
            sink.try_write_frame_immediate(b"x"),
            ImmediateWrite::BusyZero
        );
        assert!(sink.reply_budget().try_reserve(1).is_none());
        sink.sever_input();
    }

    #[test]
    fn a_waiter_for_spill_room_arranges_a_pending_drainer_itself() {
        let (sink, mut reader, filled) = wedged();
        sink.install_spill_arranger(|| {});
        let accepted = fill_spill_to_the_cap(&sink);
        {
            let s = sink.shared.spill.lock().unwrap();
            assert!(s.draining && s.arrange_pending, "only a pending mark");
        }
        let (tx, rx) = mpsc::channel();
        let waiter = {
            let sink = sink.clone();
            std::thread::spawn(move || {
                tx.send(sink.write_frame(b"W").map_err(|e| e.to_string()))
                    .expect("receiver");
            })
        };
        let expect = filled + accepted.iter().map(Vec::len).sum::<usize>() + 1;
        let mut got = vec![0u8; expect];
        reader
            .read_exact(&mut got)
            .expect("the spill drained through a drainer the waiter arranged");
        assert_eq!(got.last(), Some(&b'W'));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the waiter returned"),
            Ok(1)
        );
        waiter.join().expect("waiter");
    }

    #[test]
    fn reply_permits_bound_the_budget_and_release_on_drop() {
        let sink = SinkWriter::new(-1);
        let budget = sink.reply_budget();
        let first = budget.try_reserve(REPLY_BUDGET_BYTES / 2).expect("half");
        let second = budget
            .try_reserve(REPLY_BUDGET_BYTES / 2)
            .expect("the other half");
        assert_eq!(sink.reply_reserved(), REPLY_BUDGET_BYTES);
        assert!(budget.try_reserve(1).is_none(), "a full budget refuses");
        drop(first);
        assert_eq!(sink.reply_reserved(), REPLY_BUDGET_BYTES / 2);
        assert!(
            budget.try_reserve(REPLY_BUDGET_BYTES).is_none(),
            "an oversized reply waits for the queue to empty"
        );
        drop(second);
        let lone = budget
            .try_reserve(REPLY_BUDGET_BYTES + 1)
            .expect("a lone oversized reply is admitted into an empty queue");
        assert!(budget.try_reserve(1).is_none());
        drop(lone);
        assert_eq!(sink.reply_reserved(), 0, "released");
    }
}
