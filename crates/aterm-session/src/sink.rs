// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single serialization point for all bytes entering one session's PTY master
//! (design §6.3).

use std::collections::VecDeque;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::input_backlog::{InputBacklog, InputLedger};

/// Outcome of one bounded, immediate actuator egress attempt.
///
/// An `O_NONBLOCK` PTY write may accept a prefix; this type reports that rather
/// than calling a partial mutation success or a safe-to-retry refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImmediateWrite {
    /// The kernel accepted the entire frame during this call.
    Full,
    /// This call accepted zero bytes and queued nothing for later delivery.
    BusyZero,
    /// The kernel accepted this prefix immediately. No tail was queued; the
    /// action is in doubt and must never be retried automatically.
    PartialInDoubt { accepted: usize },
    /// The whole write only ([`SinkWriter::try_write_frame_whole_with_receipt`]):
    /// the kernel accepted this prefix during the call, and the rest of the frame is
    /// queued directly behind it in the sink's spill, ahead of anything
    /// written after it, for the drainer to hand the kernel as the program
    /// reads. The frame is delivered whole, in order, with no other frame's
    /// bytes inside it — unless the session's end drops the queued rest, as it
    /// drops any spilled frame, or a discard drops the frame, which takes the
    /// prefix from the kernel's queue with the rest
    /// ([`SinkWriter::discard_unread_input`]).
    Queued { accepted: usize },
}

/// Opaque process-local version of all non-empty input attempts against one PTY
/// sink. It advances before a producer can wait for the serialization lock, so
/// a queued human/controller attempt moves it even before that producer
/// reaches the kernel.
///
/// A compare token only, with no numeric accessor. On exhaustion it stops
/// rather than wrapping to an old token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEpoch(u64);

/// Opaque process-local version of the input someone drove into one PTY sink:
/// every non-empty attempt [`InputEpoch`] counts except the terminal's own
/// answers to its program's queries ([`SinkWriter::write_reply`]). A key, a
/// paste, a mouse or focus report and a control verb's bytes move it; the DA,
/// DSR, colour or keyboard-protocol reply a shell asked for as it started does
/// not. Only a compare token, like [`InputEpoch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrivenInput(u64);

/// Opaque process-local order of frames accepted by one PTY sink.
///
/// Unlike [`InputEpoch`], minted at the sink's direct-write or spill-FIFO
/// linearization point, so it follows accepted byte order even when lock
/// contention lets a later attempt serialize first. Meaningful only against
/// receipts from the same [`SinkWriter`]; the value stays private so callers
/// cannot forge one or mistake it for a terminal sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AcceptedOrder(u64);

/// Where one bulk frame is ([`BulkMeter::progress`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BulkState {
    /// Not begun: queued behind earlier input.
    Queued = 0,
    /// Its bytes are going to the child.
    Writing = 1,
    /// Every byte was accepted (by the kernel, or whole by the spill).
    Delivered = 2,
    /// A stop cut it ([`SinkWriter::write_frame_metered_with_receipt`]), it
    /// was stopped before it began, or a discard dropped its rest
    /// ([`SinkWriter::discard_unread_input`]).
    Stopped = 3,
    /// The write failed or the peer closed: the session is going.
    Failed = 4,
}

/// One reading of a [`BulkMeter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BulkProgress {
    /// Bytes accepted so far.
    pub sent: u64,
    /// The frame's bytes (0 until its write begins).
    pub total: u64,
    /// Where it is.
    pub state: BulkState,
}

/// Progress and a stop for one bulk frame, shared between its writer
/// ([`SinkWriter::write_frame_metered_with_receipt`]) and watchers. Plain
/// atomics: the writer stores once per `write(2)`, a watcher loads when it
/// samples, and neither waits on the other.
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
    pub const fn new() -> Self {
        Self {
            sent: AtomicU64::new(0),
            total: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            state: std::sync::atomic::AtomicU8::new(BulkState::Queued as u8),
        }
    }

    /// Ask the writer to drop what it has not yet sent (see
    /// [`SinkWriter::write_frame_metered_with_receipt`]). Idempotent; a frame
    /// already delivered is unaffected. On unix a writer parked on a full tty
    /// sees the stop at its next recheck ([`Shared::PARK_RECHECK_MS`]); on
    /// Windows a write of the frame blocked inside ConPTY's `WriteFile` is
    /// cancelled now (`aterm_pty::release_stopped_writes`).
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        #[cfg(windows)]
        aterm_pty::release_stopped_writes(&self.stop);
    }

    /// Whether a stop was asked for.
    #[must_use]
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// The frame's bytes sent, its size and its state, now.
    #[must_use]
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

    /// Mark a frame the caller never handed to the sink (it was stopped
    /// while queued, or its producer went away) as over.
    pub fn abandon(&self) {
        self.finish(BulkState::Stopped);
    }

    /// Settle a frame not written through
    /// [`SinkWriter::write_frame_metered_with_receipt`] (a paste the sanitizer
    /// emptied, or one sent through a plain write) so watchers see it over:
    /// `Delivered` when all of it was accepted (or it was empty), else `Failed`.
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

    /// A metered frame the spill accepted whole: all of it counts as sent.
    fn note_spilled(meter: Option<&Self>, len: usize) {
        if let Some(meter) = meter {
            meter.note_sent(len);
        }
    }
}

/// The opening and closing markers of a bracketed paste (DEC 2004).
const PASTE_OPEN: &[u8] = b"\x1b[200~";
const PASTE_CLOSE: &[u8] = b"\x1b[201~";

/// Where a bulk frame stopped at `off` resumes: `(keep_to, tail_from)`. The
/// writer finishes `off..keep_to` (the rest of a begun UTF-8 sequence and, for a
/// bracketed paste, of its opening marker), then skips to `tail_from` and writes
/// the closing marker so the application sees the paste end. `off == 0` writes
/// nothing: no byte reached the child. Pure.
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
    // Checked forms of what the guard already implies (`len` covers both
    // markers), so the verifier needs no link back to the guard.
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

/// The most one `write(2)` of a metered frame hands the kernel. A blocking tty
/// write returns only when every byte is taken, so slicing bounds both the
/// progress step and how long a stop waits. The fd lock is held across the
/// slices, so no other writer's bytes land between them.
const METERED_SLICE: usize = 4 * 1024;

/// How [`write_metered`] ended a frame.
#[derive(Debug, Default, PartialEq, Eq)]
struct MeteredEnd {
    /// Bytes `write` accepted, in order.
    accepted: usize,
    /// What a stop still owes the program when `write` gave the frame up with
    /// the stop asked (`Ok(0)` on a tty that is not draining): the rest of a
    /// begun UTF-8 sequence or opening marker, then a bracketed paste's closing
    /// marker, at most a dozen bytes ([`bulk_stop_cut`]). The caller delivers
    /// them ahead of the next frame ([`SinkWriter::deliver_owed_locked`]); only a
    /// discard or sever abandons them. Empty when nothing is owed.
    owed: Vec<u8>,
}

/// The metered body of a bulk frame: hand `bytes` to `write` in order, storing
/// the running count after each `write(2)` and cutting at the first stop seen
/// ([`bulk_stop_cut`]). Returns the accepted count and what a stop still owes
/// ([`MeteredEnd`]), or the error with the prefix accepted before it. Runs
/// holding the fd lock.
///
/// `write` must be the sink's ledgered blocking write
/// ([`Shared::write_stamped_blocking`], given this meter so a stop reaches a
/// writer parked on a full tty), never a bare `aterm_pty` call: a byte the
/// input-backlog ledger does not see cannot be dated. Every master write goes
/// through [`Shared::ledgered`], the one place the bracket is written.
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
            // A stop gave the frame up on a tty that is not draining: the
            // writer is released and the cut's debt travels on
            // (`MeteredEnd::owed`). Otherwise the peer closed mid-frame, or a
            // discard or sever dropped the rest.
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

/// Write what a stopped paste still owes ([`MeteredEnd::owed`]) through
/// `write`, a blocking write handed no stop (the stop already cut the frame and
/// must not also cut its close): the bytes accepted. A discard or sever, a
/// closed peer or a failed session ends it early. Platform-free, so the unix
/// suite runs the Windows body's delivery (`metered_body_tests`).
fn deliver_owed_through(owed: &[u8], mut write: impl FnMut(&[u8]) -> io::Result<usize>) -> usize {
    let mut off = 0;
    while let Some(rest) = owed.get(off..).filter(|rest| !rest.is_empty()) {
        match write(rest) {
            Ok(n) if n > 0 => off = off.saturating_add(n),
            // A discard or sever dropped it, the peer closed, or the
            // session failed: nothing more of this frame is owed.
            _ => break,
        }
    }
    off
}

/// Result of a blocking or spill-tolerant sink write, with its accepted order.
///
/// `order()` is `None` exactly when this call accepted no bytes (an empty frame
/// or a peer-close `0` write). A spilled frame counts as accepted: its bytes are
/// in the sink's bounded FIFO at the reported order, even if the drainer has not
/// handed them to the kernel yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct WriteReceipt {
    accepted: usize,
    order: Option<AcceptedOrder>,
    /// The whole frame reached the kernel on the direct lane in this call, so a
    /// caller asking "is my input drained to the kernel?" can skip the
    /// spill-mutex probe (`try_egress_drained_to_kernel`). `false` for spilled,
    /// split, short or empty frames.
    direct: bool,
}

impl WriteReceipt {
    fn new(accepted: usize, order: Option<AcceptedOrder>) -> Self {
        debug_assert_eq!(accepted == 0, order.is_none());
        Self {
            accepted,
            order,
            direct: false,
        }
    }

    /// A direct-lane write that ended with `accepted` of `intended` bytes in
    /// the kernel: `direct` iff the frame completed (a peer-closed short write
    /// keeps the conservative `false`).
    fn completed(accepted: usize, intended: usize, order: AcceptedOrder) -> Self {
        Self {
            accepted,
            order: (accepted > 0).then_some(order),
            direct: accepted > 0 && accepted == intended,
        }
    }

    /// Whether the whole frame reached the kernel directly (see the field).
    #[must_use]
    pub const fn is_direct(self) -> bool {
        self.direct
    }

    /// Number of bytes accepted by this call.
    #[must_use]
    pub const fn accepted(self) -> usize {
        self.accepted
    }

    /// Accepted serialization/spill order, or `None` when no bytes were accepted.
    #[must_use]
    pub const fn order(self) -> Option<AcceptedOrder> {
        self.order
    }
}

/// A sink write error with any prefix accepted before it. The frame did not
/// land in full, but the accepted prefix may still be echoed by the foreground
/// program. [`Self::into_error`] recovers the original [`io::Error`].
#[derive(Debug)]
pub struct WriteReceiptError {
    error: io::Error,
    receipt: WriteReceipt,
    /// The UI thread's egress refused the frame rather than park
    /// ([`SinkWriter::write_frame_interactive_with_receipt`]).
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

    /// The interactive egress's refusal: the program has stopped reading and the
    /// input queue is at [`Shared::SPILL_CAP`]. Nothing was accepted. Unix only:
    /// on Windows the UI thread never writes ConPTY itself.
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

    /// The interactive egress could not deliver a spilled byte because no
    /// drainer could be arranged (no arranger, and `dup(2)` or the thread spawn
    /// failed). A session or system failure, not a full queue, so
    /// [`Self::is_refused`] is `false`. Unix only.
    #[cfg(unix)]
    fn no_drainer(accepted: usize, order: Option<AcceptedOrder>) -> Self {
        Self::new(
            io::Error::other("input could not be queued: no spill drainer could be arranged"),
            accepted,
            order,
        )
    }

    /// Whether the frame was refused so the calling (UI) thread would not park
    /// (the program is not reading and the input queue is full), as opposed to
    /// failing on a dead or closing session. The GUI tells the person once that
    /// input was not sent.
    #[must_use]
    pub const fn is_refused(&self) -> bool {
        self.refused
    }

    /// Number of bytes accepted before the error.
    #[must_use]
    #[cfg(test)]
    pub const fn accepted(&self) -> usize {
        self.receipt.accepted()
    }

    /// Accepted serialization/spill order, or `None` when no bytes moved.
    #[must_use]
    pub const fn order(&self) -> Option<AcceptedOrder> {
        self.receipt.order()
    }

    /// Recover the original I/O error without reclassification or wrapping.
    #[must_use]
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

/// Why [`SinkWriter::discard_unread_input`] drops the input the program left
/// unread.
///
/// Both kinds empty the same queue and move the discard epoch, but only
/// [`Discard::Restart`] moves [`SinkWriter::restart_discards`]: aterm-gui's input
/// watch reads a moved restart count on a published stall as `signal term`'s
/// pre-signal drop and, after `RESTART_GRACE`, reports that the program survived
/// its signal. A `reset flush` sends no signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discard {
    /// The drop before a restart signal (`signal term|kill|hup|quit`,
    /// aterm-gui's `input_stall::discard_before_signal`).
    Restart,
    /// `reset flush` (aterm-gui's `manual_reset`): the input goes, and no
    /// signal follows.
    Flush,
}

/// What the immediate write does with a frame the kernel took only part of
/// ([`SinkWriter::try_write_frame_immediate_with_receipt`] and its guarded
/// twin [`SinkWriter::try_write_frame_whole_with_receipt`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortWrite {
    /// Queue nothing: the prefix is in doubt ([`ImmediateWrite::PartialInDoubt`]).
    InDoubt,
    /// Queue the rest right behind the prefix ([`ImmediateWrite::Queued`]).
    Queue,
}

/// A point inside the sink where a test stands a racing thread
/// ([`SinkWriter::seam_for_testing`]). The races at these points are decided
/// by which side of a lock a thread falls, and only a test pinned at the
/// point can show it. Tests only, here and downstream (a debug build's).
#[cfg(all(unix, any(test, debug_assertions)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seam {
    /// In [`SinkWriter::discard_unread_input`]'s critical section: the
    /// discard epoch moved and the spill emptied, the kernel's queue not
    /// flushed yet. The spill mutex is held.
    DiscardFlush,
    /// On the drainer's thread, after its look at the discard epoch and
    /// before the `write(2)` of the spilled bytes it peeked.
    DrainWrite,
    /// In a blocking frame write ([`SinkWriter::write_frame`]), after it
    /// found the spill empty and before it waits for the fd lock.
    FrameLock,
    /// A blocking writer (direct lane, metered paste) about to wait in `poll(2)` for
    /// room in a full tty input queue: at the park and before each recheck, fd lock held.
    WriterPark,
    /// The spill drainer about to wait in `poll(2)` for room in a full tty input
    /// queue: at the park and before each recheck, fd lock held.
    DrainPark,
}

/// What the non-parking body does where a frame cannot be taken without
/// waiting (the spill at `SPILL_CAP`, or no drainer to deliver a spilled byte).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhenFull {
    /// Wait in the blocking, cap-enforcing write: an expendable caller
    /// ([`SinkWriter::write_frame_nonparking_with_receipt`]).
    Park,
    /// Accept nothing and say so: the UI thread
    /// ([`SinkWriter::write_frame_interactive_with_receipt`]).
    Refuse,
}

/// Whether [`Shared::spill_append`] waits out the `SPILL_CAP` backpressure.
/// Windows compiles only the `spill_append` stub, which reads neither the meter
/// nor `NoWait`, hence the lint allowance there.
#[derive(Clone, Copy)]
#[cfg_attr(windows, allow(dead_code))]
enum Room<'a> {
    /// Wait for room: an expendable blocking caller. A metered paste's meter
    /// rides along so its stop ends the wait.
    Wait(Option<&'a BulkMeter>),
    /// Never wait: the non-parking body, which checked the cap itself.
    NoWait,
}

/// What [`Shared::spill_append`] did with a frame. Windows compiles only the
/// `NoDrainer` stub, hence the lint allowance there.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(windows, allow(dead_code))]
enum Spilled {
    /// Appended behind the spill (or dropped by a discard or sever it entered
    /// before) at this accepted order.
    Accepted(AcceptedOrder),
    /// A metered paste's stop landed while it waited for room: nothing was
    /// appended, and nothing of it will be written.
    Stopped,
    /// No drainer could be arranged: nothing appended, the caller falls back.
    NoDrainer,
}

/// [`SinkWriter::try_fence_egress`]'s hold: no frame reaches the sink's PTY
/// while it lives.
#[must_use = "the fence lifts when dropped"]
pub struct EgressFence<'a> {
    _frames: std::sync::MutexGuard<'a, ()>,
}

impl std::fmt::Debug for EgressFence<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressFence").finish_non_exhaustive()
    }
}

/// The byte budget of one sink's reply queue: terminal query replies (DA / DSR /
/// CPR / kitty and colour queries) on their way from the parser to the reply writer
/// ([`SinkWriter::reply_budget`]). A program that floods queries while not reading
/// plateaus here. Keys, pastes and reports on the GUI's ordered input writer are
/// bounded by that writer's own admission (aterm-gui's `paste_order`).
pub const REPLY_BUDGET_BYTES: usize = 1024 * 1024;

/// Bytes one queued reply holds against its sink's [`REPLY_BUDGET_BYTES`], from
/// [`ReplyBudget::try_reserve`]. Released on drop (after the write, or when the
/// reply is dropped, torn down or abandoned), so no path leaks a reservation.
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
/// It holds only the shared accounting, never the master, so its producer (the
/// PTY reader) can never be the last holder of its own session's master:
/// dropping that on the reader's thread would close a Windows pseudo console
/// that drains through that very thread.
#[derive(Clone)]
pub struct ReplyBudget {
    shared: Arc<Shared>,
}

impl ReplyBudget {
    /// Reserve `bytes` of the reply budget for one reply about to be queued, or
    /// `None` when full; the producer then drops (and counts) the reply. One
    /// reservation larger than the whole budget is admitted when nothing is held,
    /// so memory never exceeds the larger of the budget and one reply. Never
    /// waits (one CAS loop). A severed sink reserves nothing.
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

/// How many PTY masters this process's sinks own right now: one per
/// [`SinkWriter::new_owned`] whose last `Arc` clone has not dropped. Beside the
/// live session count (`metrics` prints `pty_masters=` and `pty_sessions=`) it
/// shows a master a closed tab left behind. Borrowed sinks
/// ([`SinkWriter::new`]) and the short-lived duplicates a spill drainer or
/// adoption pulse holds are not counted.
static OWNED_MASTERS: AtomicUsize = AtomicUsize::new(0);

/// The number of PTY masters owned by live sinks in this process
/// ([`OWNED_MASTERS`]).
#[must_use]
pub fn owned_masters_open() -> usize {
    OWNED_MASTERS.load(Ordering::Acquire)
}

/// A sink's owned master, counted in [`OWNED_MASTERS`] until it closes. The
/// count drops after the close, so the census never shows a still-open master
/// as gone.
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

/// The one place bytes enter a session's PTY master fd. Every writer (the GUI
/// keyboard, every control verb, the reader thread's query replies) funnels
/// through it, so two writers never interleave bytes inside one frame; frames
/// larger than `PIPE_BUF` (512 on Darwin) from two edges would otherwise shred
/// each other. Order is total per sink, fairness across writers arbitrary, and
/// every frame atomic.
///
/// ## Backpressure
///
/// The UI thread never parks here. Its egress,
/// [`SinkWriter::write_frame_interactive_with_receipt`], writes only as far as
/// the kernel takes bytes without waiting (the whole frame in one `write(2)` on
/// an `O_NONBLOCK` master, one byte per `poll(2)` `POLLOUT` check otherwise; see
/// [`SinkWriter::note_master_nonblocking`]), spills the rest to an in-order
/// buffer a detached drainer feeds out, and refuses a frame once that buffer
/// reaches `SPILL_CAP` ([`WriteReceiptError::is_refused`]). While spilled bytes
/// are undelivered every writer queues behind them, so order and atomicity
/// survive the spill. A spilled frame's `Ok(len)` means accepted for in-order
/// delivery, not delivered to the kernel.
///
/// Expendable threads park on purpose: [`SinkWriter::write_frame`] (pastes,
/// control verbs) and [`SinkWriter::write_frame_nonparking`] (the reply writer:
/// non-parking below the cap, parking at it). A multi-MiB paste parks holding the
/// fd lock, so a child that waits for a DA/DSR/CPR reply while not reading the
/// paste deadlocks with the reply queued behind it
/// ([`SinkWriter::write_frame_body_locked`] says why slicing cannot help). It is
/// escapable: `Stop paste` reaches the parked writer
/// ([`Shared::write_stamped_blocking`]), and a discard
/// ([`SinkWriter::discard_unread_input`]) or sever ([`SinkWriter::sever_input`])
/// releases every waiter. On Windows a writer inside the blocking ConPTY
/// `WriteFile` is released only once the pipe drains.
///
/// In front of the sink every queued reply holds a byte permit
/// ([`SinkWriter::reply_budget`]) and the GUI's ordered input writer admits
/// against its own budget; there is no per-edge rate limit.
///
/// ## fd ownership
///
/// A sink built with [`SinkWriter::new_owned`] owns the master fd: it closes
/// exactly when the last `Arc<SinkWriter>` clone drops, never by an out-of-band
/// `close()`. Every user of the master (the reader thread, each window's mirror,
/// each in-flight control verb) holds a clone, so the fd number cannot be recycled
/// by a later `forkpty` while anyone is parked in `read(master)` or `write_frame`,
/// which would route reads or keystrokes to the wrong session. The raw fd from
/// [`SinkWriter::master`] is valid exactly as long as the caller's clone.
/// [`SinkWriter::new`] borrows (never closes), for test stubs and `-1` sentinels.
pub struct SinkWriter {
    /// The raw master fd for write/read/resize: the owned fd's number when
    /// `_owned` is `Some`, a borrowed or sentinel number otherwise.
    master: i32,
    /// `Some` iff this sink owns the fd (built via `new_owned`); dropping the
    /// last `Arc<SinkWriter>` then closes it. `None` for borrowed fds and `-1`
    /// stubs. Held only for its `Drop`.
    #[cfg(unix)]
    _owned: Option<CountedMaster<OwnedFd>>,
    /// Windows twin of the ownership token: the RAII [`aterm_pty::OwnedMaster`]
    /// around the opaque ConPTY registry key, whose `Drop` on the last clone
    /// closes the session.
    #[cfg(windows)]
    _owned: Option<CountedMaster<aterm_pty::OwnedMaster>>,
    /// Ordered GUI input jobs still waiting for or running on this sink's
    /// per-session writer. The key path reads this sink-local count without
    /// visiting the process-wide writer registry, so a paste in another tab
    /// cannot put its registry lock on this session's typing path.
    ordered_egress_pending: AtomicUsize,
    /// The write-serialization and spill state, behind its own `Arc` so the
    /// detached drainer can hold it without the sink (the drainer pins the PTY
    /// via its own `dup(2)`'d fd; see [`Shared`]).
    shared: Arc<Shared>,
}

/// The serialization and spill state a [`SinkWriter`] and its (at most one)
/// drainer share. Lock order: `lock` then `spill`, or either alone; never `lock`
/// while holding `spill` (the drainer too takes `lock` before it peeks `spill`), so
/// the pair cannot invert.
// trust::paired: a whole-crate proof that `drained` is only waited with a guard
// from this instance's `spill` mutex, discharging `Condvar::wait`'s multi-mutex
// panic at the wait sites (`wait_egress_drained_to_kernel`, `spill_append`). Any
// other guard, an escape, or cross-instance threading decertifies it (a fatal row).
#[cfg_attr(trust_verify, trust::paired)]
struct Shared {
    /// Serializes whole frames: held for one fd write so no other writer's
    /// bytes interleave. A poisoned lock is recovered; "one frame at a time"
    /// still holds under a recovered guard.
    lock: Mutex<()>,
    /// Every non-empty producer reserves this before it can wait for `lock` or
    /// touch the PTY. `u64::MAX` is a fail-closed terminal value (never wraps).
    input_epoch: AtomicU64,
    /// [`SinkWriter::driven_input`]: advanced beside `input_epoch` by every
    /// producer but the reply writer. Only ever compared for equality, so it
    /// wraps (2^64 writes away) instead of failing closed.
    driven_input: AtomicU64,
    /// Packed `(SPILL_ACTIVE_BIT | accepted_order)`: bit 63 is set iff
    /// [`Spill::buf`] is non-empty, and bits 0..63 hold the per-sink
    /// accepted-order counter (`ORDER_MASK` is terminal, never wrapping).
    /// Packing both in one atomic lets [`Self::direct_order_if_spill_empty`]
    /// observe an empty spill and mint the next direct-write order in a single
    /// lock-free CAS under `lock`, while [`Self::spill_append`] updates both
    /// under `spill`.
    order_state: AtomicU64,
    /// Cached `Spill::buf.len()`, updated under the `spill` mutex so
    /// [`Self::spill_len`] and [`Self::spill_is_empty`] never take `spill` on
    /// the unspilled fast path.
    spilled_bytes: AtomicUsize,
    /// Optional caller-supplied hint for [`SinkWriter::tty_swallows_input`]
    /// (`0` = uncached, `1` = `false`, `2` = `true`), avoiding a `tcgetattr`
    /// syscall per keystroke when the line discipline is known.
    cached_tty_swallows: AtomicU8,
    /// The wedged-tty spill (design §6.2): bytes a non-parking writer could not
    /// hand the kernel without blocking. While non-empty every writer appends
    /// behind it (global FIFO), and one drainer thread feeds it to the fd at the
    /// pace the foreground program reads.
    spill: Mutex<Spill>,
    /// Signalled by the drainer as spill bytes are accepted, so a blocking
    /// writer waiting for room (`SPILL_CAP` backpressure) can proceed.
    drained: Condvar,
    /// Off-thread drainer arrangement. When installed, a non-parking writer that
    /// must spill with no drainer live does not `dup(2)` + spawn on its own (UI)
    /// thread: it commits the bytes under a `drainer pending` mark and calls this
    /// hook, which must not block (a `try_send` to a worker that then calls
    /// [`SinkWriter::arrange_pending_drainer`]). Absent (tests, embedders without
    /// a worker), arrangement runs inline. Blocking writers always arrange inline.
    arranger: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// When the kernel accepted each input byte ([`crate::input_backlog`]). A
    /// leaf lock: taken only by [`Self::ledger_begin`]/[`Self::ledger_end`],
    /// around each master write's one `write(2)` under `lock`, and by
    /// [`SinkWriter::input_backlog`], which holds nothing else. Never held
    /// across a syscall, so the probe cannot wait behind a parked writer.
    ledger: Mutex<InputLedger>,
    /// Called (at most once per arming) when a write puts bytes in the kernel's
    /// input queue ([`SinkWriter::install_input_hook`]).
    input_hook: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// The hook has fired since the last [`SinkWriter::rearm_input_hook`].
    input_hook_armed: AtomicBool,
    /// How many times [`SinkWriter::discard_unread_input`] has dropped unread
    /// input. Bumped under the `spill` mutex in the section that empties `buf`.
    /// Every frame takes the value on entry, and the drainer with each chunk it
    /// peeks; a writer whose value has moved stops writing that frame, since its
    /// bytes were accepted for the program the discard was for.
    discards: AtomicU64,
    /// How many of those discards were a restart's ([`Discard::Restart`]),
    /// bumped with `discards`. Only compared for a move
    /// ([`SinkWriter::restart_discards`]).
    restarts: AtomicU64,
    /// Set once, for good, by [`SinkWriter::sever_input`]: the session is
    /// closing. Frames in flight are dropped as a discard drops them
    /// ([`Self::discarded_since`]), and later writes fail at entry.
    severed: AtomicBool,
    /// Bytes held by live [`ReplyPermit`]s: the replies queued in front of this
    /// sink ([`SinkWriter::reply_budget`]).
    reply_reserved: AtomicUsize,
    /// Whether the master's open file description is known to carry
    /// `O_NONBLOCK` (declared via [`SinkWriter::note_master_nonblocking`]). It
    /// licenses the non-parking egress to hand the kernel a whole frame in one
    /// `write(2)`: only then can `write(2)` short-write or return `EAGAIN` instead
    /// of parking. `false` keeps the one-byte-per-`POLLOUT` cadence, parking-free
    /// even on a blocking description. The drainer reads it too: only a write
    /// that cannot park runs under the spill mutex ([`Self::drain_write`]).
    ///
    /// `Relaxed` suffices: written once at session setup, and a stale read only
    /// costs the slower, still parking-free cadence.
    master_nonblocking: AtomicBool,
    /// The test's hook at each [`Seam`] ([`SinkWriter::seam_for_testing`]).
    #[cfg(all(unix, any(test, debug_assertions)))]
    seam: std::sync::OnceLock<Box<dyn Fn(Seam) + Send + Sync>>,
}

/// See [`Shared::spill`].
struct Spill {
    /// Spilled bytes, oldest first. A frame is appended contiguously under the
    /// mutex and the drainer removes bytes only after the kernel accepted them,
    /// so "non-empty" is exactly "undelivered bytes exist", the FIFO predicate
    /// every writer consults.
    buf: VecDeque<u8>,
    /// How many of `buf`'s front bytes the drainer has already handed the kernel
    /// within the chunk it is still writing; zeroed when the chunk is popped or
    /// discarded. FIONREAD already counts them, so [`Shared::try_spill_len`]
    /// subtracts them. Only the drainer writes it, under the fd lock, and no writer
    /// can prepend while it is non-zero (a prepend needs an empty spill under that
    /// lock), so the front `chunk_written` bytes are always the ones it describes.
    chunk_written: usize,
    /// A drainer thread is live: spawned on first spill, exits when `buf`
    /// empties (or the peer closes), so an unwedged session has no extra thread.
    draining: bool,
    /// `draining` was set by a non-parking writer that deferred the spawn to the
    /// installed arranger (`Shared::arranger`); cleared by
    /// [`SinkWriter::arrange_pending_drainer`] once the thread exists, or on a
    /// failed spawn with `draining` rolled back so the next writer retries.
    #[cfg(unix)]
    arrange_pending: bool,
    /// Sticky for this sink's lifetime: the drainer discarded buffered bytes
    /// after a closed peer or hard error. A completion fence must not mistake
    /// the resulting empty buffer for a successful drain.
    failed: bool,
}

impl SinkWriter {
    /// Largest frame admitted by [`Self::try_write_frame_immediate`]: one
    /// bounded controller turn (at most 16 KiB of text plus bracketed-paste
    /// framing). Bounding the syscall unit keeps the immediate path from
    /// becoming a bulk-paste path by accident.
    pub const IMMEDIATE_FRAME_MAX: usize = 16 * 1024 + 32;

    /// Wrap a borrowed PTY master fd: this sink never closes it (the caller, or
    /// a `-1` sentinel, keeps ownership). For tests and stubs without a real PTY.
    #[must_use]
    pub fn new(master: i32) -> Self {
        Self {
            master,
            _owned: None,
            ordered_egress_pending: AtomicUsize::new(0),
            shared: Arc::new(Shared::new()),
        }
    }

    /// Take ownership of a PTY master fd (an [`OwnedFd`], so this crate stays
    /// `forbid(unsafe_code)`; the caller does the one `from_raw_fd`). The fd
    /// closes exactly when the last `Arc<SinkWriter>` clone drops; the caller must
    /// not `close()` it elsewhere.
    ///
    /// The `fd_lifecycle_model()` Clone/DropClone actions are std
    /// `Arc::clone`/`OwnedFd::drop`, with no aterm method to anchor, so they are
    /// waived here; `UseFd` is anchored on `master()`/`write_frame`.
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
            shared: Arc::new(Shared::new()),
        }
    }

    /// Take ownership of a PTY master (Windows twin): the RAII
    /// [`aterm_pty::OwnedMaster`] around the opaque ConPTY registry key, closed
    /// exactly when the last `Arc<SinkWriter>` clone drops. Carries the Unix
    /// constructor's waivers, since the closure gate collects per target.
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
            shared: Arc::new(Shared::new()),
        }
    }

    /// Projects the sink onto `fd_lifecycle_model`'s `<<fdOpen, hasOwner>>`, the
    /// witness the `UseFd` anchors on `master()`/`write_frame` name:
    ///
    ///   * `fd_open`: the sink names a usable master fd (`master != -1`), which
    ///     the last-drop discipline guarantees while any clone is alive.
    ///   * `owns_fd`: the sink owns the fd (built via `new_owned`).
    ///
    /// The Arc strong count (the model's `clones`) is not observable from `&self`.
    /// `owned_fd_stays_open_until_last_clone_drops` evaluates this on real state.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn project_fd_state(&self) -> (bool, bool) {
        (self.master != -1, self._owned.is_some())
    }

    /// The wrapped master fd, for callers that read/resize it directly. For an
    /// owned sink it is valid as long as the caller holds its `Arc<SinkWriter>`
    /// clone.
    ///
    /// This is the model's `UseFd` action; the last-drop discipline keeps the fd
    /// open while any clone is alive, so `usedAfterClose` never latches.
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
    /// A key checks only its own sink, never the process-wide writer registry.
    #[inline]
    #[must_use]
    pub fn ordered_egress_count(&self) -> usize {
        self.ordered_egress_pending.load(Ordering::Acquire)
    }

    /// Claim a FIFO slot before the job is sent to the writer. The GUI submits
    /// paste and key events in one order, so its next key sees this count before
    /// it can choose an inline write.
    pub fn claim_ordered_egress(&self) {
        self.ordered_egress_pending.fetch_add(1, Ordering::AcqRel);
    }

    /// Release the slot after the writer has completed its PTY write, or when
    /// submission failed and the caller is taking back the event.
    pub fn retire_ordered_egress(&self) {
        let previous = self.ordered_egress_pending.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "ordered egress retired without a claim");
    }

    /// What the line discipline will do with the next byte typed into this sink:
    /// the slave's ECHO / ICANON bits, read through the master
    /// ([`aterm_pty::tty_echo`]; one `tcgetattr`, no allocation, no blocking).
    /// `None` when the master is not a tty (a pipe fixture, the `-1` sentinel, a
    /// ConPTY key).
    ///
    /// A PTY pair has one termios, the slave's, and both BSD and Linux route the
    /// master's `tcgetattr` to it (pinned by aterm-pty's tests). aterm-gui's
    /// typed-glyph dispatch uses it: a press in canonical no-echo mode (`read -s`,
    /// `sudo`) will never be echoed, so it banks no press credit.
    #[must_use]
    pub fn tty_echo(&self) -> Option<aterm_pty::TtyEcho> {
        aterm_pty::tty_echo(self.master)
    }

    /// Cache a caller-observed `tty_swallows_input` hint so
    /// [`Self::tty_swallows_input`] can answer without a `tcgetattr` syscall
    /// on every keystroke.
    pub fn set_tty_swallows_hint(&self, swallows: bool) {
        self.shared
            .cached_tty_swallows
            .store(if swallows { 2 } else { 1 }, Ordering::Release);
    }

    /// Whether the line discipline will swallow the next byte typed into this
    /// sink: canonical no-echo (`read -s`, `sudo`, a passphrase) swallows; raw
    /// mode does not, since the program draws its own echo. A non-tty master
    /// answers `false`: withhold only on positive evidence.
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

    /// How much input the program at this PTY has not read, and a lower bound on
    /// how long the oldest of it has waited ([`crate::input_backlog`] has the words
    /// and the gate built on it). `None` when the master is not a tty (a socketpair
    /// fixture, the `-1` sentinel, a ConPTY key) and off macOS, where the kernel
    /// count is not measured.
    ///
    /// The steps run in this order so `wait` stays a lower bound:
    ///
    /// 1. `now` is read first, so the wait ends no later than the instant the
    ///    count describes (a racing write's later stamp saturates it at zero);
    /// 2. FIONREAD (`unread`) is read before the ledger: a write landing between
    ///    the two only raises `accepted`, and one inside `write(2)` counts as fully
    ///    accepted (`inflight`), so the computed offset only moves newer;
    /// 3. the ledger's stamps are taken after each syscall returns, and its losses
    ///    all date bytes later than accepted.
    ///
    /// In canonical mode the count is complete lines only; the partial line is
    /// newer than any complete one, so it too only moves the offset newer.
    ///
    /// The probe never takes the fd lock: a writer parked on a full queue holds it
    /// as long as the program stays frozen, which is exactly when the probe must
    /// answer. It takes the ledger mutex (a leaf, never held across a syscall) and
    /// only tries the spill mutex (`spilled: None` when busy). Every production
    /// write parks outside its ledger bracket (`Shared::write_stamped_blocking`),
    /// so a parked paste does not count as in flight and blind the reading.
    #[must_use]
    pub fn input_backlog(&self) -> Option<InputBacklog> {
        self.input_backlog_at(Instant::now())
    }

    /// [`Self::input_backlog`] as of `now`, which the caller reads first (step 1).
    /// A later `now` ages the oldest unread byte by the difference, which is how a
    /// test drives the `WARN_AFTER`/`STALL_AFTER` bands of a real pty without
    /// sleeping.
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

    /// Install the input hook: called when a write puts bytes in the kernel's
    /// input queue, at most once until [`Self::rearm_input_hook`], so a watcher
    /// probing [`Self::input_backlog`] gets one wake per episode and an idle
    /// session none. It can run under the fd lock, so it must not block (a
    /// `try_send`, an event-loop proxy post), like
    /// [`Self::install_spill_arranger`]. First install wins.
    pub fn install_input_hook(&self, hook: impl Fn() + Send + Sync + 'static) {
        let _ = self.shared.input_hook.set(Box::new(hook));
    }

    /// Let the input hook fire again on the next write. A watcher calls this
    /// before each probe, so a write racing the probe re-wakes it.
    pub fn rearm_input_hook(&self) {
        self.shared.input_hook_armed.store(false, Ordering::Release);
    }

    /// Fire the input hook as a landing write does (at most once until
    /// [`Self::rearm_input_hook`]) for a caller that sees unread input no write
    /// through this sink announced. Returns whether the hook ran (`false`: none
    /// installed, or already fired since the last rearm).
    ///
    /// Two kinds of byte need it: bytes queued before this sink was built (a
    /// master adopted across a seamless update, dated from the sink's birth) and
    /// bytes a writer the sink cannot see put there (another process's
    /// `TIOCSTI`). Runs on the caller's thread with no sink lock held.
    pub fn wake_input_hook(&self) -> bool {
        self.shared.fire_input_hook()
    }

    /// Discard every input byte the program has not read: the sink's spill and the
    /// kernel's input queue ([`aterm_pty::flush_input_queue`]). Returns how many
    /// bytes were dropped, or `None` (dropping nothing) when the queue cannot be
    /// measured (off macOS, or off a tty).
    ///
    /// For `signal term` and the other restarts of a frozen program (aterm-gui's
    /// `input_stall::discard_before_signal`): keys typed into it would otherwise
    /// run in the shell that takes the tty back.
    ///
    /// The spill goes first, and with it every frame in flight. Flushing only the
    /// kernel queue would wedge the session: the drainer parks in `poll(POLLOUT)`
    /// holding the fd lock, and on Darwin a flush does not wake a parked `poll`.
    /// So:
    ///
    /// - The epoch moves, the spill empties and the kernel queue is flushed in
    ///   ONE critical section of the spill mutex, the flush last. Every write
    ///   that holds that mutex across its `write(2)` therefore lands wholly
    ///   before the discard, and goes with the queue, or wholly after it: the
    ///   guarded write ([`Self::try_write_frame_whole_with_receipt`]), from its
    ///   look at the empty spill until the rest of a short write is queued, and
    ///   the drainer on a non-blocking master (which that write requires), from
    ///   its look at the epoch through its write of spilled bytes
    ///   ([`Shared::drain_write`]).
    ///   A frame begun in the kernel with its rest in the spill is dropped
    ///   whole, head and rest. Until 2026-10-16 the flush ran after the mutex
    ///   was released: a guarded write landing between the two had its head
    ///   flushed and its queued rest delivered, and the program read the end
    ///   of a text without its beginning.
    /// - A drainer that wakes after the discard finds its chunk discarded and
    ///   writes nothing.
    /// - Every parked writer re-polls on a clock ([`Shared::PARK_RECHECK_MS`]),
    ///   sees the epoch move, and abandons the rest of its frame.
    /// - A writer waiting for spill room at `SPILL_CAP` is woken, and its frame
    ///   is dropped with the rest.
    ///
    /// Both [`Discard`]s move [`Self::discards`]; only a restart moves
    /// [`Self::restart_discards`].
    ///
    /// Residual: a DIRECT write, one that found the spill empty and writes
    /// outside the spill mutex (a key, a reply, a blocking frame), is not
    /// ordered with the critical section, and neither is the drainer's write
    /// on a master that may block (whose `write(2)` can park, so it never runs
    /// under the mutex). One that took its epoch before the discard and is
    /// inside `write(2)` as the flush lands can put that write's bytes into the
    /// emptied queue; a direct frame that took its epoch inside the section
    /// and whose head the flush then drops can still queue its tail. Closing
    /// that would need the fd lock, which a parked writer holds as long as the
    /// program stays frozen.
    #[cfg(unix)]
    pub fn discard_unread_input(&self, why: Discard) -> Option<usize> {
        aterm_pty::input_queue_len(self.master)?;
        let (epoch, dropped) = {
            let mut s = self.shared.spill.lock().unwrap_or_else(|p| p.into_inner());
            // `fetch_add` wraps; the epoch is only compared for equality.
            let epoch = self
                .shared
                .discards
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            if why == Discard::Restart {
                self.shared.restarts.fetch_add(1, Ordering::AcqRel);
            }
            // The drainer's accepted-but-unpopped chunk prefix is already in
            // the kernel's queue: the flush below counts it.
            let unsent = s.buf.len().saturating_sub(s.chunk_written);
            s.buf.clear();
            s.chunk_written = 0;
            self.shared.spilled_bytes.store(0, Ordering::Release);
            self.shared
                .order_state
                .fetch_and(!Shared::SPILL_ACTIVE_BIT, Ordering::Release);
            #[cfg(all(unix, any(test, debug_assertions)))]
            self.shared.at_seam(Seam::DiscardFlush);
            // Still under the spill mutex: a guarded write or a drainer write
            // falls wholly on one side of the flush (see above). One
            // non-blocking syscall, so the hold stays short.
            let flushed = aterm_pty::flush_input_queue(self.master).unwrap_or(0);
            (epoch, flushed.saturating_add(unsent))
        };
        self.shared.drained.notify_all();
        // Marked AFTER the flush, so every byte counted after the mark was
        // accepted into the emptied queue ([`Self::read_since_discard`]).
        self.shared
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .mark_discard(epoch);
        Some(dropped)
    }

    /// Whether the program has read input since the last discard
    /// ([`Self::discard_unread_input`]): fewer bytes are queued than the kernel
    /// accepted after the discard's mark ([`InputLedger::mark_discard`]). `false`
    /// when that cannot be told: no discard yet, a discard still running, a slave
    /// that does not pass every byte, or no reading (off macOS, off a tty). A
    /// watcher holding a stall through a discard needs this to let go.
    ///
    /// The order is the reverse of [`Self::input_backlog`]'s, so the error falls on
    /// the "not read" side: the ledger is read before FIONREAD, and the epoch on
    /// both sides, so a discard running meanwhile answers `false`.
    ///
    /// Only a slave that queues every byte written can be judged
    /// ([`aterm_pty::tty_passes_every_byte`]: raw). Canonical mode counts complete
    /// lines only, and in cbreak the driver eats `^C`, `^S`, `^O`, `^V` and friends,
    /// so a Ctrl-C would look like a read.
    ///
    /// Residuals: a byte that left the queue other than by a read (the program
    /// flushing its own input) counts as read, and so does a raw reader with `VMIN`
    /// above the queued count (FIONREAD answers 0). Two racing discards can leave
    /// the older mark, and then this answers `false` until the next discard.
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

    /// How many times [`Self::discard_unread_input`] has dropped unread input;
    /// only compared for a move. A discarded queue says nothing about the program
    /// (one whose SIGTERM handler never runs survives `signal term`), so a count
    /// that moved since a stall was published means the queue was dropped, not read.
    #[must_use]
    pub fn discards(&self) -> u64 {
        self.shared.discard_epoch()
    }

    /// How many of [`Self::discards`] were a restart's drop ([`Discard::Restart`],
    /// before `signal term|kill|hup|quit`); only compared for a move. Only a moved
    /// count here starts the restart grace (see [`Discard`]). Read it before
    /// [`Self::discards`]: a restart bumps both in one critical section, so a
    /// reader that sees this move also sees the epoch move.
    #[must_use]
    pub fn restart_discards(&self) -> u64 {
        self.shared.restarts.load(Ordering::Acquire)
    }

    /// Sever this sink's input for good: the session is closing (aterm-gui's
    /// `Session::drop`, after the hang-up), and no later byte can reach a program
    /// anyone will read again.
    ///
    /// Teardown unblocks every waiter. On Windows a writer blocked inside
    /// ConPTY's `WriteFile` is cancelled, and every later write to the session
    /// writes nothing (`aterm_pty::release_input_writes`).
    /// A writer parked on a full tty (the ordered egress writer, the reply
    /// writer, the spill drainer) sees it at its next [`Shared::PARK_RECHECK_MS`]
    /// recheck and abandons its frame; one waiting for spill room at `SPILL_CAP`
    /// is woken now; the spill is dropped; and every later write fails at entry
    /// with `BrokenPipe`. Otherwise a child that outlives its hang-up in another
    /// process group would never read again, and the parked writer's clone would
    /// pin the master and the queued pastes behind it forever.
    ///
    /// Unlike [`Self::discard_unread_input`], nothing is flushed from the kernel
    /// queue and [`Self::discards`] does not move. The completion fence
    /// ([`Self::wait_egress_drained_to_kernel`]) reports the dropped spill as a
    /// loss. Idempotent.
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

    /// Whether [`Self::sever_input`] has run. For tests, here and downstream
    /// (a debug build's).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn is_severed(&self) -> bool {
        self.shared.is_severed()
    }

    /// Date every input byte this sink has handed the kernel `by` earlier, so a
    /// test can stand at the stall threshold without sleeping. Tests only (here
    /// and downstream, in a debug build).
    #[cfg(any(test, debug_assertions))]
    pub fn backdate_input_for_testing(&self, by: Duration) {
        self.shared
            .ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .backdate(by);
    }

    /// Run `hook` at each [`Seam`] this sink passes, on the thread passing it
    /// (the drainer's own at [`Seam::DrainWrite`], under the locks that point
    /// holds): where a test stands a racing thread and decides when it goes
    /// on. First install wins. Tests only, here and downstream (a debug
    /// build's).
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub fn seam_for_testing(&self, hook: impl Fn(Seam) + Send + Sync + 'static) {
        let _ = self.shared.seam.set(Box::new(hook));
    }

    /// This sink's reply budget ([`ReplyBudget`], [`REPLY_BUDGET_BYTES`]): a
    /// handle that does not keep the sink or its master alive, for the producer
    /// that must never be the last holder of its own session's master (the PTY
    /// reader).
    #[must_use]
    pub fn reply_budget(&self) -> ReplyBudget {
        ReplyBudget {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Bytes the reply queue holds reserved right now (queued or in flight).
    /// Tests only (here and downstream, in a debug build); dropped replies are
    /// counted by whoever drops them (the GUI's `reply_dropped` metric).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn reply_reserved(&self) -> usize {
        self.shared.reply_reserved.load(Ordering::Acquire)
    }

    /// Declare whether this sink's master file description carries `O_NONBLOCK`.
    ///
    /// The direct-read gather flips the master non-blocking once per session;
    /// `O_NONBLOCK` is per description, so it applies to every writer, but the sink
    /// cannot observe it. Told, the UI-thread egress writes a whole key frame in one
    /// `write(2)` instead of a `poll(2)`+`write(2)` pair per byte (a Kitty
    /// `REPORT_EVENT_TYPES` key is ~5-11 bytes per press and release).
    ///
    /// Pass `true` only when the `fcntl` succeeded, and `false` again if the
    /// description returns to blocking: `true` on a blocking description lets a
    /// frame larger than the tty's free room park the event loop in `write(2)`.
    /// `false` (the default) is always safe.
    pub fn note_master_nonblocking(&self, nonblocking: bool) {
        self.shared
            .master_nonblocking
            .store(nonblocking, Ordering::Relaxed);
    }

    /// Whether this sink's process-local egress buffer is drained to the kernel:
    /// no spill bytes wait in this process's memory for the drainer.
    ///
    /// Spilled bytes would die with the process at the seamless handoff's `_exit`,
    /// while kernel-queued bytes are the child's to read; the handoff reads this
    /// through [`Self::try_fence_egress`], which also sees a frame still being
    /// written. It answers "does this process still hold bytes", not "did they
    /// land": after the drainer discards a spill over a dead peer it reads `true`,
    /// so the handoff's deadline does not stall on an unrecoverable loss. A writer
    /// that must know whether bytes reached the kernel uses
    /// [`Self::wait_egress_drained_to_kernel`], which keeps the sticky failure.
    #[must_use]
    pub fn egress_drained_to_kernel(&self) -> bool {
        self.shared.spill_is_empty()
    }

    /// Stop this sink's egress at a frame boundary, without waiting: `Some`
    /// when no frame is part-way into the kernel and the spill holds nothing
    /// ([`Self::egress_drained_to_kernel`]), `None` otherwise. A blocking
    /// frame parked on a full tty is part-way in with the spill empty: it
    /// holds the fd lock, which this takes with `try_lock`.
    ///
    /// While the fence is held no frame can begin: a blocking writer waits
    /// for it before its first byte, a non-parking one queues whole behind
    /// it in the spill, and the drainer waits too. Dropping it lets them go
    /// on in order. The seamless handoff reads it before it `_exit`s at
    /// Commit and holds it through the `_exit`: a frame cut there (a
    /// bracketed paste left open) would have the program read every later
    /// key as pasted text, and spilled bytes would die with the process.
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

    /// How many bytes the process-local spill holds right now. For tests,
    /// here and downstream (a debug build's): a frame queued behind a
    /// wedged tty is a FACT a test waits for, not a moment it sleeps toward.
    #[cfg(all(unix, any(test, debug_assertions)))]
    #[must_use]
    pub fn spilled_bytes(&self) -> usize {
        self.shared.spill_len()
    }

    /// The spill's cap: a non-parking frame is refused once the spill holds
    /// this much. For tests, as [`Self::spilled_bytes`].
    #[cfg(all(unix, any(test, debug_assertions)))]
    pub const SPILL_CAP: usize = Shared::SPILL_CAP;

    /// Block an expendable producer until every process-local spill byte has reached
    /// the kernel or the drainer has seen a dead peer; `true` only for the first.
    ///
    /// Not for UI-thread input, which stays non-parking. Completion-correlated callers
    /// (the control thread's `paste`/`paste-bin` reply) use it after a full
    /// `write_frame`, which can mean "accepted into the spill". A dead drainer clears
    /// the buffer to unblock teardown, and the sticky `failed` bit tells that loss from
    /// a drain, so an `OK` built on this fence means every byte reached the kernel.
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

    /// [`Self::wait_egress_drained_to_kernel`] for at most `limit`: `None` when
    /// bytes are still process-local when it runs out (a program that is not
    /// reading), otherwise the same answer. For a caller that must not wait on
    /// the program for ever: the control socket's reply to a background paste
    /// (aterm-gui's `background_input`), which checks its caller and its own
    /// deadline between slices.
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

    /// Non-parking observation of [`Self::egress_drained_to_kernel`]:
    /// `Some(true)` the spill is empty, `Some(false)` bytes remain, `None`
    /// another thread holds the spill mutex. A poisoned mutex is already
    /// acquired by `try_lock`, so recovering it stays non-parking.
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

    /// Install the off-thread drainer arranger (`Shared::arranger`): a
    /// non-blocking hook a non-parking writer calls instead of spawning the
    /// drainer on its own thread. The hook's owner must then call
    /// [`Self::arrange_pending_drainer`] from a worker. First install wins.
    pub fn install_spill_arranger(&self, arranger: impl Fn() + Send + Sync + 'static) {
        let _ = self.shared.arranger.set(Box::new(arranger));
    }

    /// Worker-side half of [`Self::install_spill_arranger`]: spawn the drainer a
    /// non-parking writer marked pending. Idempotent; a spawn failure rolls the
    /// `draining` mark back so the next writer arranges again, so spilled bytes
    /// are never stranded behind a drainer that does not exist.
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

    /// Current attempted-input epoch for this sink. It observes input attempts,
    /// not terminal output: it closes human/controller interjection races but
    /// cannot make a screen classification atomic with a PTY write.
    #[must_use]
    pub fn input_epoch(&self) -> InputEpoch {
        InputEpoch(self.shared.input_epoch.load(Ordering::Acquire))
    }

    /// The input driven into this sink so far ([`DrivenInput`]): unlike
    /// [`Self::input_epoch`], the terminal's replies to its program's own
    /// queries leave it where it was.
    #[must_use]
    pub fn driven_input(&self) -> DrivenInput {
        DrivenInput(self.shared.driven_input.load(Ordering::Acquire))
    }

    /// Reserve one non-empty DRIVEN input attempt (a key, a paste, a report, a
    /// control verb's bytes): it moves [`Self::driven_input`] as well as the
    /// input epoch ([`Self::reserve_attempt`]).
    fn reserve_input_attempt(&self) {
        self.shared.driven_input.fetch_add(1, Ordering::AcqRel);
        self.reserve_attempt();
    }

    /// Reserve one non-empty input attempt, saturating: exhaustion is
    /// permanent.
    /// Every write reserves one; only a reply ([`Self::write_reply`]) reserves
    /// it without [`Self::reserve_input_attempt`].
    fn reserve_attempt(&self) {
        let _ = self
            .shared
            .input_epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            });
    }

    /// Hand one bounded frame to the kernel now, without parking and without queueing
    /// anything for the drainer: the fail-closed actuator egress. Unlike
    /// [`Self::write_frame_nonparking`], which spills when busy (so a key may land much
    /// later), a guarded actuator must act on the state it just checked. Returns
    /// [`ImmediateWrite::BusyZero`] without writing when:
    ///
    /// * another frame holds the serialization lock;
    /// * the spill mutex is contended;
    /// * older spilled bytes still await delivery; or
    /// * the non-blocking master has no room.
    ///
    /// Holding both the fd and spill locks across the one non-blocking write keeps
    /// FIFO: older work is gone and newer writers cannot overtake.
    /// [`ImmediateWrite::PartialInDoubt`] is a kernel short write, not a queued tail;
    /// the caller must not retry it (PTY writes are not transactional).
    ///
    /// Without [`Self::note_master_nonblocking`] this refuses without touching the fd,
    /// since a write on a blocking description could park. ConPTY has no non-blocking
    /// input primitive, so Windows refuses likewise.
    #[cfg(unix)]
    pub fn try_write_frame_immediate(&self, bytes: &[u8]) -> ImmediateWrite {
        self.try_write_frame_immediate_with_receipt(bytes).0
    }

    /// Receipt-bearing twin of [`Self::try_write_frame_immediate`]: a non-empty
    /// full or partial write carries the token minted under both ordering locks;
    /// refusals and empty writes carry none.
    #[cfg(unix)]
    pub fn try_write_frame_immediate_with_receipt(
        &self,
        bytes: &[u8],
    ) -> (ImmediateWrite, Option<AcceptedOrder>) {
        if bytes.is_empty() {
            return (ImmediateWrite::Full, None);
        }
        // Reserve before any wait or refusal: an epoch reader must see even an
        // attempt that ends in EAGAIN.
        self.reserve_input_attempt();
        self.try_write_frame_immediate_reserved(bytes, ShortWrite::InDoubt)
    }

    /// THE GUARDED WRITE: [`Self::try_write_frame_immediate_with_receipt`]'s
    /// refusals, then the frame WHOLE. A frame is begun now or not at all —
    /// every case that writes nothing there writes nothing here, and queues
    /// nothing a detached thread could inject later — but a frame the kernel
    /// began taking is finished in order: when the `write(2)` takes only a
    /// prefix, the rest goes to the front of the spill (empty, checked under
    /// both locks) before the fd lock is released, so the drainer hands it to
    /// the kernel as the program reads and every later frame queues behind
    /// it. One decision covers the whole frame: the caller's check, made
    /// before the call, decided all of it ([`ImmediateWrite::Queued`]).
    ///
    /// Why: a pty's input queue is small (1022 bytes on Darwin), so any frame
    /// longer than the room it has comes back short. Under the immediate
    /// write that is [`ImmediateWrite::PartialInDoubt`], never retried, and a
    /// guarded text of 1.1 KB (a supervisor's instruction to save the work)
    /// was cut at 1022 bytes every time it was typed.
    ///
    /// The drainer is arranged as the non-parking body arranges it: with an
    /// arranger installed (every production session) its spawn is deferred
    /// to the arranger, so this call never creates a thread and never waits;
    /// without one it is spawned inline, and when it cannot be the rest is
    /// not queued and the call answers [`ImmediateWrite::PartialInDoubt`]:
    /// only then are bytes of the frame lost for certain. A rest that is
    /// queued and then dropped (a discard, the session closing, a dead peer)
    /// is lost as any spilled frame's is, after this call answered.
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

    /// Windows fail-closed twin of [`Self::try_write_frame_whole_with_receipt`]:
    /// ConPTY has no non-blocking write, so it refuses as the immediate write does.
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

        // `try_lock` is required: neither a foreground write nor a drainer may
        // make an actuator wait. A poisoned mutex is already acquired, so
        // recovering its guard stays non-parking.
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
        // Reserve under both ordering locks: the syscall may still refuse, but
        // no later frame can get an earlier token than bytes this call accepts.
        let Ok(order) = self.shared.next_accepted_order(false) else {
            return (ImmediateWrite::BusyZero, None);
        };

        // Keep both guards through the syscall, so a normal writer cannot see an
        // empty spill and then race this frame for the fd. The ledger bracket
        // closes as the syscall returns, before the rest is queued and while
        // the spill mutex is still held: a discard waiting on that mutex then
        // marks the ledger at the prefix the kernel took. Closed after the
        // poke, the bracket would still count the whole frame in flight when
        // such a discard marked it, and every later byte would be under-counted
        // by the queued rest (`a_discard_behind_a_guarded_write_marks_what_the_kernel_took`).
        let mut spill_guard = spill_guard;
        let mut poke = false;
        let result = match self.shared.ledgered_nonparking(self.master, bytes, false) {
            aterm_pty::NonParkWrite::Wrote(n) if n == bytes.len() => ImmediateWrite::Full,
            // A frame begun is finished in order (`try_write_frame_whole_with_receipt`):
            // the rest to the front of the spill, still empty under both locks.
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
        // The deferred drainer's poke, after the spill mutex is released, as
        // `spill_append` makes it: a `try_send` to the arranger's worker.
        if poke && let Some(arranger) = self.shared.arranger.get() {
            arranger();
        }
        drop(fd_guard);
        if accepted > 0 {
            let _ = self.shared.fire_input_hook();
        }
        (result, (accepted > 0).then_some(order))
    }

    /// Windows fail-closed twin of [`Self::try_write_frame_immediate`]. ConPTY
    /// input is a blocking pipe write with no non-blocking form, so this refuses
    /// without writing rather than weaken the actuator deadline.
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

    /// Write a WHOLE frame atomically with respect to other writers, returning the
    /// number of bytes accepted (`== bytes.len()` on success). Holds the
    /// serialization lock until the frame is either on the wire or handed to the
    /// spill, so no other writer's bytes can appear inside this frame (a spilled
    /// tail PREPENDS, and every writer queues behind a non-empty spill). Propagates
    /// the first hard error rather than silently dropping the tail (the bug the
    /// legacy `write_all` had before `write_some`).
    ///
    /// On unix the body parks in blocking writes holding the lock: fine for bounded
    /// frames, but it carries the paste/query-reply deadlock described on
    /// [`Self::write_frame_body_locked`]. Only expendable egress threads call it, never
    /// the UI event loop.
    ///
    /// Writing through the raw master fd is `fd_lifecycle_model`'s `UseFd` action: the
    /// last-drop discipline keeps the fd open while any clone (this writer's included)
    /// lives, so a use never lands on a closed or recycled fd (`NoUseAfterClose`).
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

    /// [`Self::write_frame_with_receipt`] for one bulk frame (a large paste),
    /// reporting progress into `meter` and honouring its stop.
    ///
    /// Bytes, order and backpressure are the blocking path's; the meter adds one
    /// relaxed store after each `write(2)` and a stop check before each. A stop before
    /// the first byte writes nothing. A stop mid-frame finishes what the child is owed
    /// so the frame stays well formed ([`bulk_stop_cut`]): the begun UTF-8 sequence, a
    /// bracketed paste's opening `ESC [ 200 ~` and its closing `ESC [ 201 ~`, so the
    /// application sees the paste end; the bytes between are dropped. What the kernel
    /// took stays taken. A frame queued behind the spill is accepted whole and counts
    /// as sent, but a stop that lands while it still waits for spill room ends it with
    /// nothing appended.
    ///
    /// A stop also reaches a writer parked on a full tty: after a whole
    /// [`Shared::PARK_RECHECK_MS`] without room the frame is given up and the writer
    /// returns, and what the cut owes the program (its closing bracket above all) goes
    /// to the front of the spill ([`Self::deliver_owed_locked`]). Only a discard or a
    /// sever abandons it.
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
            // A peer that closed mid-frame (`Ok(0)`): the session is going.
            Ok(_) => BulkState::Failed,
        });
        debug_assert!(sent <= bytes.len() as u64);
        result
    }

    /// Blocking frame body after the public entry point has reserved this
    /// non-empty input attempt. The non-parking API delegates here when it must
    /// apply blocking backpressure, so that one public attempt advances the epoch
    /// exactly once rather than reserving again through [`Self::write_frame`].
    fn write_frame_after_reserve(
        &self,
        bytes: &[u8],
        meter: Option<&BulkMeter>,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        if self.shared.is_severed() {
            return Err(Shared::severed_error().into());
        }
        // The discard epoch this frame entered under, taken before any wait
        // (for spill room or for the fd lock): a frame accepted before a
        // discard is dropped with it, however long it waited to be written.
        let since = self.shared.discard_epoch();
        // A ConPTY session owes its resize guard's console client after a size
        // change: an input frame runs it here, before the fd lock, because the
        // client may wait on the console host and the host on the terminal's
        // answer to its own cursor query, which the reply writer writes under
        // that lock (`aterm_pty::settle_input`; a reply, inside `as_reply`,
        // settles nothing). A no-op off Windows.
        aterm_pty::settle_input(self.master);
        // FIFO with any SPILLED bytes: while the wedged-tty spill buffer is
        // non-empty this frame must queue BEHIND it (a direct write would overtake
        // spilled keystrokes), under the SPILL_CAP wait so a paste into a wedged
        // foreground applies real backpressure to its (expendable) thread. Checked
        // BEFORE the fd lock: while draining, the drainer may sit parked in a
        // blocking write HOLDING the fd lock, and waiting on it here would park
        // this caller behind the wedge instead of behind the cap.
        if !self.shared.spill_is_empty() {
            match self
                .shared
                .spill_append(self.master, bytes, Room::Wait(meter), since)?
            {
                Spilled::Accepted(order) => {
                    BulkMeter::note_spilled(meter, bytes.len());
                    return Ok(WriteReceipt::new(bytes.len(), Some(order)));
                }
                // A metered paste stopped while it waited for room: nothing
                // of it was appended, so nothing of it will ever be written.
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
        // Re-check and mint as one atomic step: a non-parking contender can append
        // without the fd lock, and must not obtain an earlier token after this frame
        // has committed to the direct lane.
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
                // No drainer could be arranged: degrade to the plain blocking write.
                Spilled::NoDrainer => return self.write_frame_locked(bytes, meter, since),
            }
        };
        self.write_frame_body_locked(guard, bytes, order, meter, since)
    }

    /// Write a whole frame while holding the fd lock. Callers are expendable threads
    /// (the per-session ordered egress writer, the reply writer), and
    /// [`Self::write_frame_nonparking`] for an oversized frame or a full spill. The UI
    /// thread's egress ([`Self::write_frame_interactive_with_receipt`]) never reaches
    /// it, nor does a control socket lane's: aterm-gui writes a verb's input through
    /// that egress, so a lane cannot park on a program that stopped reading.
    ///
    /// Deliberately parking (contract 1, docs/AUDIT-performance-quality-2026-08-29.md
    /// P0). A multi-MiB paste is one frame, so a wedged foreground pins this lock while
    /// the reply writer's DA/DSR/CPR answer queues behind it; a TUI blocked reading that
    /// reply never reads stdin, and neither side advances. Once `ESC [ 200 ~` is in the
    /// stream a reply either waits behind the paste tail or lands inside the envelope as
    /// pasted content, so slicing and releasing the lock cannot help; repeating whole
    /// envelopes changes paste and undo semantics, and a sideband is no general PTY
    /// answer. The wait is escapable: `Stop paste` reaches a writer parked here
    /// ([`Shared::write_stamped_blocking`]), and a discard (`signal term`/`hup`) or a
    /// sever (closing the tab) releases it.
    ///
    /// Spilling the tail on `EAGAIN` would hurt latency instead: `spill_prepend` cannot
    /// honour `SPILL_CAP` (it runs holding the fd lock), so one over-cap paste would push
    /// the spill past the cap and every later key would be refused until the program
    /// reads.
    ///
    /// One body for both platforms. On Windows a ConPTY handle is not a
    /// pollable fd and has no spill drainer (`spill_append` is a compiled
    /// no-op), so every frame takes this ordered blocking loop, and a stop or
    /// a sever cancels the blocking `WriteFile` itself
    /// ([`Shared::write_stamped_blocking`]'s Windows twin).
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
                // `get` + saturating_add (the drain_loop idiom): `off < len` makes
                // the `get` always Some, and `n <= rest.len()` (POSIX) means the
                // sum never saturates — both spellings byte-identical on every
                // real path; the write's `n` comes from `write(2)` outside this
                // crate's bundle, so it is unbounded to the verifier.
                let Some(rest) = bytes.get(off..) else { break };
                match self
                    .shared
                    .write_stamped_blocking(self.master, rest, since, None)
                {
                    // Peer closed mid-frame, or a discard dropped the rest.
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

    /// Deliver what a stopped paste still owes the program ([`MeteredEnd::owed`])
    /// ahead of every later frame, without holding the writer that gave the frame up.
    /// Called holding the fd lock, so nothing lands between the paste's last byte and
    /// these. Returns the bytes accepted for delivery.
    ///
    /// On unix they go to the front of the spill ([`Shared::spill_prepend`]): the
    /// drainer hands them over once the program reads again, and everything written
    /// later queues behind them, so the program reads its paste closed, then the next
    /// key. Only a discard or a sever drops them. Where no drainer can be arranged (and
    /// on Windows, which has no spill) they are written here, escapable only by a
    /// discard or a sever: a stop that already cut the frame does not also cut the close.
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

    /// The blocking write, taking the fd lock itself: the degraded path when no drainer
    /// can be arranged (e.g. `dup(2)` failed).
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
                    // The spill emptied while its drainer exited and arranging a
                    // replacement failed. Reacquire and recheck instead of racing
                    // a new spill with a direct write.
                    Spilled::NoDrainer => continue,
                }
            };
            return self.write_frame_body_locked(guard, bytes, order, meter, since);
        }
    }

    /// Frames larger than this skip [`Self::write_frame_nonparking`]'s fast path for
    /// the blocking [`Self::write_frame`]: their producers (a paste, a control verb, an
    /// oversized reply) run on expendable threads that must feel the
    /// [`Shared::SPILL_CAP`] backpressure. The UI thread's egress has no such limit.
    #[cfg(unix)]
    const NONPARK_MAX: usize = 4096;

    /// Spin hints a contended non-parking frame burns before it concedes the fd lock
    /// and spills: enough to cover a holder mid-frame (a few `poll`+`write` syscalls),
    /// far below the cost of conceding (see the retry site).
    #[cfg(unix)]
    const TRY_LOCK_SPINS: u32 = 256;

    /// Opportunistically non-parking [`Self::write_frame`] for an expendable thread
    /// that writes small frames (the reply writer; tests). A frame of at most
    /// [`Self::NONPARK_MAX`] returns without waiting for tty capacity while the spill
    /// is below [`Shared::SPILL_CAP`]; a full tty or a contended fd lock diverts it to
    /// the ordered spill.
    ///
    /// It parks in three bounded-memory fallbacks: a frame over `NONPARK_MAX`, any
    /// frame once the spill is at `SPILL_CAP`, and one that finds no drainer can be
    /// arranged. The UI thread uses [`Self::write_frame_interactive_with_receipt`],
    /// which refuses where this parks.
    ///
    /// Same whole-frame atomicity and FIFO as `write_frame`: while anything is
    /// spilled, every writer appends behind it. A spilled frame returns `Ok(len)`:
    /// accepted, and delivered in order unless the peer closes first.
    ///
    /// Writing through the raw master fd is `fd_lifecycle_model`'s `UseFd` action; the
    /// last-drop discipline (and the drainer's own `dup(2)`, see [`Shared`]) keeps every
    /// use on a live fd.
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
        self.write_frame_nonparking_after_reserve(bytes)
    }

    /// The terminal's own answer to a query its program sent (DA, DSR/CPR, a colour or
    /// kitty-keyboard query), written as [`Self::write_frame_nonparking`] writes. It
    /// moves [`Self::input_epoch`] like every write, so interjection guards see it,
    /// but alone leaves [`Self::driven_input`] where it was: nobody drove it.
    pub fn write_reply(&self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.reserve_attempt();
        self.write_frame_nonparking_after_reserve(bytes)
            .map(WriteReceipt::accepted)
            .map_err(WriteReceiptError::into_error)
    }

    /// [`Self::write_frame_nonparking_with_receipt`]'s body after its one
    /// reservation, shared with [`Self::write_reply`].
    #[cfg(unix)]
    fn write_frame_nonparking_after_reserve(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        // Bulk takes the blocking path so `SPILL_CAP` applies (see `NONPARK_MAX`).
        if bytes.len() > Self::NONPARK_MAX {
            return self.write_frame_after_reserve(bytes, None);
        }
        self.write_frame_unparked(bytes, WhenFull::Park)
    }

    /// Windows: the ordered blocking write, as
    /// [`Self::write_frame_nonparking_with_receipt`] takes there.
    #[cfg(windows)]
    fn write_frame_nonparking_after_reserve(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        self.write_frame_after_reserve(bytes, None)
    }

    /// The UI thread's egress: write a frame without ever parking, and refuse it when
    /// that is impossible (docs/AUDIT-typing-to-pixels-2026-08-26.md P0). At
    /// [`Shared::SPILL_CAP`] an interactive frame is refused: nothing accepted, an `Err`
    /// whose [`WriteReceiptError::is_refused`] is `true`, which the GUI reports once.
    /// Parking would freeze every window and growing the spill is unbounded memory; a
    /// refused key is visible and can be retyped once the program reads.
    ///
    /// Otherwise it is [`Self::write_frame_nonparking_with_receipt`] (the direct lane
    /// while the kernel has room, the ordered spill behind a full tty or a contended
    /// lock) with the three places that one parks changed:
    ///
    /// * the spill at `SPILL_CAP`: refused;
    /// * no drainer can be arranged (no arranger installed, and `dup(2)` or the spawn
    ///   failed): a plain failure, not a refusal; every production session's reply
    ///   writer is its arranger, so there the spawn is deferred and this does not occur;
    /// * a frame over `NONPARK_MAX`: the UI thread writes no bulk (a paste goes through
    ///   the per-session ordered writer), so a large IME commit or key binding spills
    ///   like a key. The spill may pass the cap by that one frame, never more.
    ///
    /// A frame split by a full tty spills its tail under the same deferred arrangement;
    /// only when no drainer can exist does it fail, with the head's prefix and order in
    /// the `Err`.
    #[cfg(unix)]
    pub fn write_frame_interactive_with_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        if bytes.is_empty() {
            return Ok(WriteReceipt::new(0, None));
        }
        self.reserve_input_attempt();
        self.write_frame_unparked(bytes, WhenFull::Refuse)
    }

    /// Windows twin of [`Self::write_frame_interactive_with_receipt`]: ConPTY has no
    /// non-blocking write, so this delegates to the blocking write, and the GUI's UI
    /// thread never calls it there. Its keys and reports (mouse, wheel, focus, a
    /// colour-scheme change) go to the per-session ordered writer (aterm-gui
    /// `paste_order`, `app_input::write_ui_report`); one that writer does not admit is
    /// refused, never written here.
    #[cfg(windows)]
    pub fn write_frame_interactive_with_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<WriteReceipt, WriteReceiptError> {
        self.write_frame_with_receipt(bytes)
    }

    /// The non-parking body [`Self::write_frame_nonparking_with_receipt`] and
    /// [`Self::write_frame_interactive_with_receipt`] share; `when_full` is
    /// what happens where the frame cannot be taken without waiting.
    #[cfg(unix)]
    fn write_frame_unparked(
        &self,
        bytes: &[u8],
        when_full: WhenFull,
    ) -> Result<WriteReceipt, WriteReceiptError> {
        if self.shared.is_severed() {
            return Err(Shared::severed_error().into());
        }
        // The discard epoch this frame entered under (see
        // `write_frame_after_reserve`).
        let since = self.shared.discard_epoch();
        // Bound the spill here too. This path skips the `SPILL_CAP` wait while there is
        // room, but a spill at the cap means the foreground stopped draining, and
        // appending uncapped would let a machine-rate small-frame producer grow `buf`
        // without bound. The sink never silently drops bytes it accepted (`NoLoss` in
        // conformance_fabric's `SinkNoLoss`), so at the cap it parks or refuses: an
        // expendable caller parks in the blocking, cap-enforcing `write_frame` (queued
        // behind the spill), and the UI thread is refused with nothing accepted. One
        // `spill_len` read answers both the cap and the FIFO question below.
        let spilled = self.shared.spill_len();
        if spilled >= Shared::SPILL_CAP {
            return match when_full {
                WhenFull::Park => self.write_frame_after_reserve(bytes, None),
                WhenFull::Refuse => Err(WriteReceiptError::refused(0, None)),
            };
        }
        // Undelivered spill exists → queue behind it (order), never touch the fd.
        if spilled != 0
            && let Spilled::Accepted(order) =
                self.shared
                    .spill_append(self.master, bytes, Room::NoWait, since)?
        {
            return Ok(WriteReceipt::new(bytes.len(), Some(order)));
        }
        // Spin briefly before conceding the race. Conceding is not free: with no
        // drainer and no arranger, `spill_append` runs `arrange_drainer` inline under
        // the spill mutex, so a key that lost a lock race would pay a `dup(2)` plus a
        // `pthread_create` (~50-150us on macOS) on the event loop, and later keys would
        // route through the drainer until the spill empties. The holder is a frame
        // writer or one `DRAIN_CHUNK` of the drainer, microseconds. The budget is pure
        // spin hints (no syscall, yield or park), so a holder truly parked in a
        // degraded blocking write costs only a sub-microsecond detour.
        let mut spins: u32 = 0;
        let acquired = loop {
            match self.shared.lock.try_lock() {
                Ok(g) => break Some(g),
                // Poison is not contention and never clears: concede at once.
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
        // Re-check and mint as one atomic step: after this frame commits to the
        // direct lane, a contending spill must receive a later token.
        let Some(order) = self.shared.direct_order_if_spill_empty()? else {
            drop(guard);
            return self.spill_or_when_full(bytes, since, when_full);
        };
        // The write unit per POLLOUT check. `poll` promises only that some room exists
        // (on a pty master, as little as one byte), so the unit must be one the kernel
        // can refuse without parking:
        //
        //   * `O_NONBLOCK` description (declared via `note_master_nonblocking`; the
        //     direct-read gather leaves the master so): `write(2)` short-writes or
        //     returns `EAGAIN`, so the whole remaining frame is safe and the poll is
        //     skipped, since the write reports the same "no room". One `write(2)` per
        //     frame instead of ~20-44 syscalls for one Kitty-protocol key.
        //   * otherwise: a blocking `write(2)` larger than the free room parks until
        //     every byte is taken, and a wedged foreground fills the queue with these
        //     very keys, so one byte per check is the only parking-free unit. Frames
        //     here are at most `NONPARK_MAX` (almost always ~20 bytes or fewer).
        //
        // Either way the SPILL decision is identical: a refused (or short) write
        // hands the tail to the drainer below.
        let whole_frame = self.shared.master_nonblocking.load(Ordering::Relaxed);
        let mut off = 0;
        while off < bytes.len() {
            // Only a blocking description needs this check. Skipping it on an
            // `O_NONBLOCK` one changes no outcome: `WouldBlock` reaches the same
            // `spill_tail_locked` from the same `off`, and `poll_writable` reports
            // POLLERR/POLLHUP as writable so the write surfaces the errno anyway.
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
                            let _ = self.shared.fire_input_hook();
                        }
                        return Err(WriteReceiptError::after_write(e, off, order));
                    }
                }
            }
            // `off < bytes.len()` makes both `get`s `Some`; the `else` break only
            // discharges the bounds obligation.
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
                    // `n <= unit.len() <= bytes.len() - off` (POSIX): the clamp and the
                    // saturating add never fire; they discharge the bounds obligation the
                    // verifier cannot chain through the cross-crate `n`.
                    let room = bytes.len().saturating_sub(off);
                    off = off.saturating_add(if n <= room { n } else { room });
                }
                // The whole-frame path's no-room signal (no poll precedes it), or a
                // poll race on the one-byte path: either way the tail spills from here.
                aterm_pty::NonParkWrite::WouldBlock => {
                    return self.spill_tail_locked(guard, bytes, off, order, since, when_full);
                }
                aterm_pty::NonParkWrite::Fatal(e) => {
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
        Ok(WriteReceipt::completed(off, bytes.len(), order))
    }

    /// Queue a whole frame behind the spill without waiting, for the non-parking body:
    /// the fd lock was contended, or the spill turned non-empty under it. With no
    /// drainer to deliver it, `when_full` decides: an expendable caller degrades to the
    /// blocking write, the UI thread is refused with nothing accepted.
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
            // `Stopped` needs a room wait, which `NoWait` never does.
            Spilled::NoDrainer | Spilled::Stopped => match when_full {
                WhenFull::Park => self.write_frame_locked(bytes, None, since),
                WhenFull::Refuse => Err(WriteReceiptError::no_drainer(0, None)),
            },
        }
    }

    /// Windows: a ConPTY handle (an opaque registry key) has no spill or `poll(2)`, so
    /// this is the ordered blocking [`Self::write_frame`]. Only expendable threads call
    /// it; the UI thread's keys reach ConPTY through the per-session ordered writer.
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

    /// Spill `bytes[off..]` while holding the fd lock, the only state in which a frame
    /// can be split (head in the kernel, tail spilled). The tail prepends: a frame
    /// another writer appended while this one held the lock started later, so it must
    /// drain after this tail; an append would let the drainer deliver it inside this
    /// frame. The drainer cannot hold a stale peek across this (it peeks under the same
    /// fd lock). With no drainer to arrange, an expendable caller finishes the frame
    /// inline rather than strand the tail; the UI thread (`WhenFull::Refuse`) fails,
    /// reporting the head as the accepted prefix.
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
        // Both callers pass `off < bytes.len()`, so this `get` is `Some`; the
        // unreachable `else` answers as a successful prepend would.
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
            // `off < bytes.len()` makes this `get` `Some`; the `else` break only
            // discharges the bounds obligation.
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
                // A blocking write never writes more than the slice it was
                // given (`write(2)` returns at most its count), so `n <= bytes.len() -
                // off` and this clamp is a no-op — it equals the previous
                // `n.min(bytes.len() - off)`, spelled as a visible branch so
                // `off + n <= bytes.len()` (no overflow) is derivable without
                // seeing through `min`.
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
    /// High bit of [`Self::order_state`]: set iff [`Spill::buf`] is non-empty.
    const SPILL_ACTIVE_BIT: u64 = 1 << 63;
    /// Low 63 bits of [`Self::order_state`]: the monotonic accepted-order counter.
    const ORDER_MASK: u64 = (1 << 63) - 1;
    /// Spill capacity a blocking writer waits under (backpressure for a paste into a
    /// wedged foreground); non-parking writers may pass it briefly rather than stall
    /// the UI.
    #[cfg(unix)]
    const SPILL_CAP: usize = 2 * 1024 * 1024;
    /// Bytes the drainer hands to the kernel per fd-lock acquisition.
    #[cfg(unix)]
    const DRAIN_CHUNK: usize = 8 * 1024;
    /// How long one `poll(POLLOUT)` park lasts before a parked writer looks again
    /// ([`Self::write_stamped_blocking`]). On Darwin a `tcflush` empties the queue
    /// without waking a parked poll (a fresh poll after it answers writable at once),
    /// so a park never waits for good: this covers our own discard
    /// ([`SinkWriter::discard_unread_input`]) and a program's `TCSAFLUSH`, at one
    /// `poll(2)` per 100 ms while a program is not reading a full queue.
    #[cfg(unix)]
    const PARK_RECHECK_MS: i32 = 100;

    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            input_epoch: AtomicU64::new(0),
            driven_input: AtomicU64::new(0),
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
            discards: AtomicU64::new(0),
            restarts: AtomicU64::new(0),
            severed: AtomicBool::new(false),
            reply_reserved: AtomicUsize::new(0),
            master_nonblocking: AtomicBool::new(false),
            #[cfg(all(unix, any(test, debug_assertions)))]
            seam: std::sync::OnceLock::new(),
        }
    }

    /// Run the test's hook at `seam` ([`SinkWriter::seam_for_testing`]).
    #[cfg(all(unix, any(test, debug_assertions)))]
    fn at_seam(&self, seam: Seam) {
        if let Some(hook) = self.seam.get() {
            hook(seam);
        }
    }

    /// Mint the next [`AcceptedOrder`] token, optionally setting
    /// [`Self::SPILL_ACTIVE_BIT`] in the same atomic step.
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

    /// The discard epoch ([`Self::discards`]) a frame takes as it enters the
    /// sink, and the drainer takes with each chunk.
    fn discard_epoch(&self) -> u64 {
        self.discards.load(Ordering::Acquire)
    }

    /// A discard has run since `since` was taken, or the sink was severed:
    /// whatever of that frame (or chunk) is not in the kernel yet was dropped
    /// with the queue.
    fn discarded_since(&self, since: u64) -> bool {
        self.discard_epoch() != since || self.is_severed()
    }

    /// [`SinkWriter::sever_input`] has run: the session is closing.
    fn is_severed(&self) -> bool {
        self.severed.load(Ordering::Acquire)
    }

    /// The error every write after [`SinkWriter::sever_input`] fails with.
    fn severed_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "session input severed: the session is closing",
        )
    }

    /// Open the ledger bracket: `len` bytes are about to enter `write(2)`.
    /// Called with the fd lock held, immediately before the syscall.
    fn ledger_begin(&self, len: usize) {
        self.ledger
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .begin(len);
    }

    /// Close the ledger bracket: the syscall returned with `n` bytes accepted (0 for a
    /// refusal, would-block, closed peer or error). Stamped here, still under the fd
    /// lock, so stamps are monotone and in kernel order. With `fire_hook`, bytes that
    /// landed fire the input hook after the ledger mutex drops; the interactive and
    /// non-parking paths pass `false` and fire it after releasing `self.lock`.
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

    /// THE way production code hands the master bytes: `write` runs inside the
    /// ledger bracket ([`Self::ledger_begin`] … [`Self::ledger_end`]) and returns
    /// its result with the count of bytes the kernel accepted (0 for a refusal,
    /// would-block, closed peer or error), so the ledger sees every byte the sink
    /// writes by construction: no write site spells the bracket, so none can
    /// forget it. Called with the fd lock held, so the stamp is ordered with every
    /// other write's; the ledger is a leaf lock, taken under the caller's.
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

    /// [`Self::ledgered`] over the non-parking master write.
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

    /// Run the input hook unless it already fired since the last rearm (or
    /// none is installed); returns whether it ran. The one firing rule for a
    /// landing write ([`Self::ledger_end`]) and an explicit wake
    /// ([`SinkWriter::wake_input_hook`]), so the two share one arming.
    fn fire_input_hook(&self) -> bool {
        let Some(hook) = self.input_hook.get() else {
            return false;
        };
        if self.input_hook_armed.swap(true, Ordering::AcqRel) {
            return false;
        }
        hook();
        true
    }

    /// Spill bytes not yet handed to the kernel, without waiting for the spill mutex:
    /// `None` when another thread holds it (a poisoned guard still answers). The
    /// drainer's accepted-but-unpopped chunk prefix ([`Spill::chunk_written`]) is
    /// excluded: FIONREAD already counts it, and
    /// [`crate::input_backlog::InputBacklog::unread`] adds the two.
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

    /// A blocking write with the park OUTSIDE the ledger bracket: one
    /// non-blocking `write(2)` per bracket, and on `EAGAIN` a
    /// `poll(POLLOUT)` wait between brackets. Byte-identical results — an
    /// empty slice or a `0` write is `Ok(0)`, `EINTR` retries, `EAGAIN` parks
    /// and retries, any other error propagates — on the `O_NONBLOCK` master
    /// every production session carries (spawn.rs `note_master_nonblocking`).
    ///
    /// Why not bracket the blocking call itself: a writer parked on a full
    /// queue (a paste into a frozen program, or the spill drainer behind it)
    /// would then hold its WHOLE chunk in flight for as long as the program
    /// stays frozen, and every unread count no larger than that chunk — a
    /// full raw queue, measured at 1022 bytes on Darwin 25.6, is smaller than
    /// the drainer's 8 KiB — would read "no date": the probe blind in exactly
    /// the state it exists to see. The ledger unit test
    /// `ledger_a_bracket_held_across_a_park_would_blind_the_probe` pins the
    /// arithmetic; with this helper reverted to one bracket around a
    /// blocking write, the real-pty test
    /// `a_writer_parked_on_a_full_queue_does_not_blind_the_probe` read
    /// `queued: 1022, wait: 0ns` (measured 2026-09-24). On an `O_NONBLOCK`
    /// description a write that returns `EAGAIN` accepted nothing, so parking
    /// between brackets loses nothing. On a BLOCKING description (test
    /// fixtures) the `write(2)` itself may park inside its bracket; that can
    /// only shorten a reading, never lengthen it.
    ///
    /// `since` is the discard epoch the frame (or drainer chunk) entered under: once a
    /// discard ([`SinkWriter::discard_unread_input`]) or sever
    /// ([`SinkWriter::sever_input`]) has run this answers `Ok(0)` without writing, as a
    /// closed peer does, and a park re-polls every [`Self::PARK_RECHECK_MS`], so a
    /// flushed queue cannot hold it forever.
    ///
    /// `stop` is a metered paste's [`BulkMeter`] (only [`write_metered`] passes one). A
    /// park that has waited a whole [`Self::PARK_RECHECK_MS`] without room and finds the
    /// stop asked gives up the frame with `Ok(0)`: the `Stop paste` escape from a
    /// program that stopped reading (docs/AUDIT-performance-quality-2026-08-29.md P0).
    /// The close is still owed: [`write_metered`] hands it back as [`MeteredEnd::owed`]
    /// and the body puts it at the front of the spill
    /// ([`SinkWriter::deliver_owed_locked`]), so a program that resumes after any pause
    /// reads its paste closed, then the input typed after it, not every later key as
    /// pasted text.
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
                aterm_pty::NonParkWrite::WouldBlock => {
                    // Park until the tty input queue has room (or the peer
                    // errors — reported writable, so the retry surfaces it),
                    // looking again every PARK_RECHECK_MS: a discard's flush
                    // does not wake this poll, and neither does a stop.
                    loop {
                        #[cfg(any(test, debug_assertions))]
                        self.at_seam(Seam::WriterPark);
                        if aterm_pty::poll_writable(fd, Self::PARK_RECHECK_MS)? {
                            break;
                        }
                        if self.discarded_since(since)
                            || stop.is_some_and(BulkMeter::stop_requested)
                        {
                            return Ok(0);
                        }
                    }
                }
            }
        }
    }

    /// Windows twin of the unix [`Self::write_stamped_blocking`]: ConPTY has
    /// no non-blocking write to park between, so the bracket spans the
    /// blocking call (the unix twin says why that matters there); the probe
    /// answers `None` here anyway. One helper for both of the Windows body's
    /// lanes, plain and metered, so neither can write around the ledger.
    /// `since` only ever moves for a sever here (the discard is unix-only).
    ///
    /// The escapes reach INSIDE the blocking `WriteFile`. ConPTY's input is an
    /// anonymous pipe, so a write to a program that has stopped reading blocks
    /// in the call until the console host reads again. The write is made as
    /// one `aterm_pty` can cancel: the frame's stop ([`BulkMeter::request_stop`])
    /// cancels a write of that frame, and a write of the frame that begins after
    /// the stop writes nothing; a sever ([`SinkWriter::sever_input`]) cancels any
    /// write on the session and bars every later one. It then answers what it
    /// wrote, `Ok(0)` when nothing, as the unix twin's give-up does:
    /// [`write_metered`] ends the frame there and hands back what the cut still
    /// owes (the rest of a character or opening marker, the closing
    /// `ESC [ 201 ~`), which the body writes next with no stop to drop it
    /// ([`SinkWriter::deliver_owed_locked`]). With no spill to queue those few
    /// bytes in, a program that still does not read keeps the session's ordered
    /// writer in that write, as it would for the next key, until it reads or the
    /// tab closes. Until 2026-10-19 neither escape was seen until the call
    /// returned, so `Stop paste` and a tab's close left the writer in the call,
    /// and the paste's whole rest went to the program once it read again.
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

    /// The Windows twin's body over any blocking write: the sever gate, then
    /// the one ledger bracket around a write handed the frame's stop flag.
    /// Platform-free, and handed the same `stop` the Windows body hands the
    /// twin, so the unix suite runs the twin's logic through
    /// [`write_metered`] (`metered_body_tests`), which a unix host cannot
    /// otherwise execute.
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

    /// The drainer's write of `chunk` from `off`, parked until the kernel
    /// takes some of it: the count it took, or `0` for a discard or sever
    /// since `since`, a closed peer, a broken poll or any hard error. The only
    /// `io::Error` it can hold is the `Os(errno)` variant (born in
    /// `write_some_nonparking` or `poll_writable`), dropped HERE — a
    /// trivially total drop with no boxed `Custom` payload, so the drain loop
    /// itself never touches an error value.
    ///
    /// On a master known to be non-blocking ([`Self::master_nonblocking`],
    /// which every guarded write requires), the look at the discard epoch and
    /// the `write(2)` share ONE hold of the spill mutex, the mutex a discard
    /// empties the spill and flushes the kernel's queue under
    /// ([`SinkWriter::discard_unread_input`]): a discard lands wholly before
    /// the write, which then writes nothing, or wholly after it, whose flush
    /// takes these bytes with the rest of the queue. With the look outside it
    /// (until 2026-10-16, [`Self::write_stamped_blocking`] served the drainer
    /// too) a discard could land between the two, and the drainer handed the
    /// emptied queue the middle of a frame whose head the flush had just
    /// dropped: a guarded write's rest, a split frame's tail.
    ///
    /// The hold covers one `write(2)` that cannot park, never a park: the
    /// `poll(POLLOUT)` is taken with the mutex released and the ledger
    /// bracket closed, as [`Self::write_stamped_blocking`] parks, so a writer
    /// queueing behind the spill (the UI thread's keys among them) never waits
    /// on a frozen program here, and the backlog probe keeps dating the queue
    /// this drainer waits on. On a description that may block, a `write(2)`
    /// larger than the room parks inside the syscall, so there the look and
    /// the write keep their two holds and the residual
    /// ([`SinkWriter::discard_unread_input`]'s) stays. The chunk's accepted
    /// prefix is recorded in [`Spill::chunk_written`] under the spill mutex,
    /// unless a discard took the chunk, so a discard never counts it as both
    /// queued and spilled.
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
                    // After a discard `buf` no longer holds this chunk, so the
                    // mark would describe bytes it does not have.
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
                // Park until the tty input queue has room (or the peer errors
                // — reported writable, so the retry surfaces it), looking
                // again every PARK_RECHECK_MS: a discard's flush does not wake
                // this poll.
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

    /// While the caller holds the fd lock, bind an empty spill observation to a
    /// direct-write order token in one atomic CAS on [`Self::order_state`].
    /// A contending non-parking writer that appends to the spill sets
    /// [`Self::SPILL_ACTIVE_BIT`] and increments the order in one atomic step,
    /// so observing an empty spill and minting a direct order never needs the
    /// `spill` mutex on the unspilled fast path.
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

    /// No undelivered spilled bytes: the FIFO predicate every writer consults.
    fn spill_is_empty(&self) -> bool {
        self.spilled_bytes.load(Ordering::Acquire) == 0
    }

    /// Undelivered spill length in bytes: the `SPILL_CAP` check the non-parking egress
    /// makes without taking the spill mutex.
    #[cfg(unix)]
    fn spill_len(&self) -> usize {
        self.spilled_bytes.load(Ordering::Acquire)
    }

    /// The rest of a frame the guarded write began
    /// ([`SinkWriter::try_write_frame_whole_with_receipt`]), queued in the
    /// spill `s` the caller holds — with the fd lock, and found EMPTY under
    /// both — so it is the spill's front: the drainer peeks under the fd lock
    /// and every other writer queues behind a non-empty spill, so nothing can
    /// come between the prefix the kernel took and this rest. No discard can
    /// run meanwhile: it empties the spill AND flushes the kernel's queue
    /// under this mutex ([`SinkWriter::discard_unread_input`]), so one before
    /// the caller's empty check left the prefix an empty queue, and one after
    /// the caller lets go drops prefix and rest together. The rest belongs to
    /// the program the prefix went to, and the drainer's writes of it are
    /// ordered with a discard the same way: the caller needs a non-blocking
    /// master, on which the drainer writes under the mutex
    /// ([`Self::drain_write`]).
    ///
    /// The drainer is arranged as [`Self::spill_prepend`] arranges it for
    /// [`Room::NoWait`]: one already live (it waits on the fd lock to peek)
    /// takes the rest; with an arranger installed the spawn is deferred to it
    /// and `Some(true)` asks the caller to poke it once the spill mutex is
    /// released; otherwise the drainer is spawned here. `None` when it cannot
    /// be: nothing is queued, and the prefix stands alone, in doubt.
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

    /// PREPEND a split frame's tail (or what a stopped paste still owes,
    /// [`Self::prepend_owed`]) to the spill front — callable ONLY while
    /// holding the fd lock (see `spill_tail_locked`): the holder's frame head is
    /// already on the wire, so its tail must drain before anything a concurrent
    /// writer appended meanwhile, and holding the fd lock is what guarantees the
    /// drainer has no stale peek to reorder around (it peeks under that lock).
    /// Same discard rule as [`Self::spill_append`]: a tail whose frame entered
    /// before a discard (`since`) is dropped, and reported handled.
    ///
    /// The drainer is arranged as [`Self::spill_append`] does for the same `room`: for
    /// [`Room::NoWait`] with an arranger installed the spawn is deferred to it;
    /// otherwise inline, and `false` when that fails, leaving the caller to finish the
    /// frame inline or refuse it. `room` is never waited on: holding the fd lock, a
    /// prepend cannot honour `SPILL_CAP`.
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

    /// What a stopped paste still owes the program, to the front of the spill
    /// ([`SinkWriter::deliver_owed_locked`]; caller holds the fd lock). The caller is
    /// the paste's expendable writer, so a drainer is arranged inline; `false` when
    /// none can be.
    #[cfg(unix)]
    fn prepend_owed(self: &Arc<Self>, master: i32, owed: &[u8], since: u64) -> bool {
        self.spill_prepend(master, owed, since, Room::Wait(None))
    }

    /// Windows: there is no spill to carry the owed bytes, so the caller
    /// writes them itself.
    #[cfg(not(unix))]
    fn prepend_owed(self: &Arc<Self>, _master: i32, _owed: &[u8], _since: u64) -> bool {
        false
    }

    /// Append one frame to the spill, arranging the drainer, and return its
    /// accepted-order token. [`Room::Wait`] waits out the `SPILL_CAP` backpressure, for
    /// expendable blocking callers (a metered paste's stop ends the wait with nothing
    /// appended, [`Spilled::Stopped`]); [`Room::NoWait`] is the non-parking body, which
    /// checked the cap itself. [`Spilled::NoDrainer`] means no drainer could be
    /// arranged and nothing was appended: spilling without one would strand the bytes.
    /// Token exhaustion is an error and appends nothing too.
    ///
    /// A frame that entered (`since`) before a discard or a sever is dropped here: it
    /// gets its order token and appends nothing, including one that waited for room at
    /// `SPILL_CAP` (the discard wakes it). The check shares the mutex with the
    /// discard's bump, so no pre-discard frame lands in the emptied spill. The room
    /// wait re-checks every [`Self::PARK_RECHECK_MS`], since a stop is a plain store
    /// that cannot notify the condvar.
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
                // A pending drainer (its spawn deferred to the arranger) does not
                // exist yet, and the arranger's queue is the reply writer's, which
                // may be this very waiter. Waiting for room, this thread is
                // expendable: arrange it here. A failed spawn rolls `draining` back
                // and the loop ends for the caller's fallback.
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
        // A non-parking writer may be the UI thread: with an arranger installed it
        // never runs `dup`/`pthread_create` itself; it marks the drainer pending,
        // commits, and pokes the worker after the mutex drops. Everyone else arranges
        // inline, before committing.
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

    /// Windows stub: a ConPTY handle has no `dup(2)` drainer, so spilling is never
    /// available and the caller falls through to the ordered blocking write. Never
    /// reached: `spill_is_empty()` is always true on Windows.
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

    /// Spawn the drainer thread (caller holds the spill mutex and has checked
    /// `!s.draining`), before any bytes are committed, so a `dup`/spawn failure never
    /// strands anything. The drainer's own dup'd fd keeps the write target alive
    /// independent of the sink.
    #[cfg(unix)]
    // Verified panic-free with no skip: `Builder::spawn`'s interior-nul `expect` is
    // discharged by the toolchain's SPAWN-NAMESAFE trace (trust-mir-extract
    // `mark_spawn_namesafe_calls`) because the name is a fixed literal; a non-literal
    // name fails closed. The closure verifies separately (a panic on the detached
    // drain thread unwinds to its own boundary).
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

    /// The drainer: feed spilled bytes to the fd in `DRAIN_CHUNK`s (blocking writes are
    /// fine: this thread exists to absorb the wedge), removing bytes only after the
    /// kernel accepted them, until the spill empties (exit; respawned on the next
    /// spill) or the peer closes (the rest is dropped with the session). The peek
    /// happens under the fd lock (fd lock first, then spill, as every writer takes
    /// them): a writer that split a frame holds the fd lock while it prepends the tail,
    /// so the drainer never carries a stale pre-prepend chunk that would deliver a
    /// foreign frame inside the split one.
    #[cfg(unix)]
    fn drain_loop(self: Arc<Self>, fd: OwnedFd) {
        // This thread holds `Shared.lock`, which the UI thread's key write contends,
        // for every chunk. A lock holder must not run below the thread it can stall:
        // at the inherited default QoS it is an E-core candidate whose descheduling
        // parks every key behind it.
        aterm_pty::declare_interactive_thread();
        let mut chunk: Vec<u8> = Vec::with_capacity(Self::DRAIN_CHUNK);
        loop {
            let guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            // Peek without removing, so writers keep seeing undelivered bytes and
            // append behind them. The discard epoch is read in the same critical
            // section: a discard bumps it under this mutex as it empties `buf`, so a
            // chunk and its epoch belong together.
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
                // `drain_write` keeps the `io::Error` OUT of this loop: it
                // returns the accepted byte count as a plain `usize` (0 on any
                // hard error, retrying `EINTR` internally). Its parking shape
                // matters: the gather keeps the master `O_NONBLOCK`
                // (per-description, shared by this dup'd fd), and a bare
                // EAGAIN-collapses-to-0 here would misread the full-but-alive
                // input queue of the wedged foreground — the very state this
                // drainer absorbs — as session-dead and DROP the spill. It
                // parks in poll(POLLOUT) and retries instead, the legacy
                // kernel behavior — and parks BETWEEN ledger brackets, so the
                // input-backlog probe keeps dating the queue this drainer is
                // waiting on. It also records the chunk's accepted prefix in
                // `Spill::chunk_written` under the hold its write ran in: the
                // chunk's first `off` bytes are the kernel's now, but stay in
                // `buf` until the chunk is popped below — and the next write
                // may park for as long as the program stays frozen — so the
                // probe does not count them twice.
                match self.drain_write(fd.as_raw_fd(), &chunk, off, since) {
                    // saturating_add: `n <= chunk.len() - off` (the POSIX
                    // write contract), so the sum never actually saturates —
                    // but the helper's return is opaque to the verifier here,
                    // so `n` is unbounded and the plain `+=` would carry an
                    // undischargeable overflow obligation. Behavior-identical
                    // on every real return.
                    n if n > 0 => off = off.saturating_add(n),
                    // Peer closed / hard error: the session is dead — drop the
                    // spill (a blocking write would have reported Ok(0)/Err once;
                    // these bytes were already accepted-for-delivery). Or a
                    // discard dropped this chunk: told apart below.
                    _ => {
                        dead = true;
                        break;
                    }
                }
            }
            drop(guard);
            {
                let mut s = self.spill.lock().unwrap_or_else(|p| p.into_inner());
                // The accepted prefix leaves `buf` in this same critical
                // section — popped below, or discarded with a dead peer — so
                // no probe sees the mark without its bytes, or the reverse.
                s.chunk_written = 0;
                // A discard emptied `buf` while this chunk was out: the chunk went
                // with the queue, what `buf` holds now arrived after it, and a `0`
                // from the write was the discard, not a dead peer. Peek again.
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
                // Safe though the fd lock was released above: a writer that takes it
                // sees the spill still non-empty and appends; prepends come only from
                // a writer that found the spill empty under the lock and has held it
                // since. So the front `off` bytes are exactly the chunk just written.
                // `pop_front` rather than `drain(..take)`: the verifier models
                // `pop_front` as total but mints an unmodeled row for any `drain`.
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

// Unix-gated: every test here drives a real `UnixStream::pair()` fixture. The Windows
// twin (OwnedMaster closes on last drop) runs in aterm-pty's tests/windows_smoke.rs.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::thread;

    /// A NON-TTY SINK HAS NO INPUT BACKLOG AND KNOWS NOTHING ABOUT ECHO: the
    /// socketpair fixture and the `-1` sentinel answer `None` even with bytes
    /// written and unread, so no existing fixture can ever read `pending`,
    /// `stalled`, or be refused, and a consumer's default (bank the press) is
    /// what every fixture exercises. The real-pty readings are
    /// `tests/input_backlog_pty.rs` and aterm-gui's
    /// `a_press_the_tty_will_not_echo_banks_no_credit`.
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

    /// Two landing writes wake once, an empty frame never, and after a rearm the next
    /// landing write wakes again.
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

    /// An explicit wake shares the writes' arming: a no-op with no hook, then one fire
    /// per arming, whichever of a wake or a landing write comes first.
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

    // Whole-frame atomicity: N threads each write a distinct frame LARGER than the
    // socket send buffer (so a single `write` short-writes and the loop iterates,
    // giving the kernel real opportunities to interleave two writers). Through one
    // SinkWriter the bytes must arrive as exactly N CONTIGUOUS single-byte runs;
    // without the serialization lock the runs would fragment. Driven on a stream
    // socketpair (no shell, no unsafe — fds are borrowed from owned `UnixStream`s),
    // with a concurrent reader so the oversized writes never deadlock on a full buffer.
    #[test]
    fn write_frame_is_whole_frame_atomic_across_threads() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        // Borrowed fd (the owned `writer` is dropped below) — `new` doesn't close it.
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

        const N: u8 = 4;
        const LEN: usize = 128 * 1024; // > any default socket buffer -> forces short writes

        // Drain concurrently so the oversized frames don't block on a full buffer.
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

    // Run-length summary [(byte, count), ...] of consecutive equal bytes.
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

    /// Attempt order is not acceptance order: with the fd lock held, a blocking writer
    /// reserves its epoch first and waits, and a later non-parking writer spills and
    /// completes first. Receipts and bytes both follow `B` then `A`.
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
            // Model the narrow post-arrangement race without spawning a real
            // drainer: the fallback must append, so this flag is sufficient.
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

    /// Each public non-empty write is one input attempt, even when the non-parking entry
    /// hands a bulk frame to the blocking body; empty writes reserve nothing. A reply to
    /// the program's own query is an attempt too, but not driven input.
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

    /// A contended fd lock is a zero-byte refusal, never a spill behind the holder: the
    /// frame after the holder leaves is the only one read.
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

    /// An existing spill is older in the FIFO, so an immediate frame refuses rather than
    /// join the drainer: after the wedge clears only the older spilled frame arrives.
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
            // Failure bound only: nothing drains the peer until after this call, so a
            // refusal that waited for it would never return.
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

        // A later accepted frame is the next and only byte sequence; a queued
        // refused marker would precede it in FIFO order.
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

    // A `new_owned` sink closes its fd only when the last Arc clone drops, so while any
    // clone lives the fd number cannot be recycled by a later forkpty and route a read
    // or key to the wrong session.
    #[test]
    fn owned_fd_stays_open_until_last_clone_drops() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        // The sink takes the writer end's OwnedFd as its sole owner.
        let owned: OwnedFd = writer.into();
        let sink = Arc::new(SinkWriter::new_owned(owned));
        let clone = Arc::clone(&sink);

        // A clone remains, so the fd is still open and writable.
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

        // Dropping the last clone closes the fd: the peer reads the bytes, then EOF (a
        // leak would hang here).
        drop(clone);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).expect("read_to_end");
        assert_eq!(
            &buf, b"alive",
            "peer got the bytes then EOF — fd closed on last clone drop"
        );
    }

    /// With a socketpair's send buffer filled (a wedged foreground),
    /// `write_frame_nonparking` returns promptly, and every spilled byte arrives in
    /// order once the peer drains.
    #[test]
    fn small_write_below_spill_cap_returns_promptly_and_preserves_order() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));

        // Wedge: stuff the pipe until the kernel reports no room.
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

        // A keystroke into a wedged peer must return promptly.
        let t0 = std::time::Instant::now();
        assert_eq!(
            sink.write_frame_nonparking(b"AAAA")
                .expect("spill accepted"),
            4
        );
        // Follow-ups while still wedged queue behind it, from both APIs.
        assert_eq!(
            sink.write_frame_nonparking(b"BBBB")
                .expect("spill accepted"),
            4
        );
        assert_eq!(sink.write_frame(b"CCCC").expect("spill accepted"), 4);
        assert!(
            // Failure bound only: the regression is an unbounded park, so any finite
            // deadline catches it.
            t0.elapsed() < std::time::Duration::from_secs(60),
            "small writes below a fresh spill cap must not wait for the wedge to clear"
        );

        // Completion-correlated expendable writers must park while these
        // accepted bytes still live only in the process spill.
        let (completion_tx, completion_rx) = std::sync::mpsc::channel();
        let completion_sink = sink.clone();
        let completion_thread = std::thread::spawn(move || {
            completion_tx
                .send(completion_sink.wait_egress_drained_to_kernel())
                .expect("completion receiver alive");
        });

        // Unwedge: drain everything; the spill drainer must deliver A,B,C after
        // the fill bytes, contiguous and in submission order.
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

        // The drainer settled: a fresh direct write goes straight through.
        assert_eq!(sink.write_frame_nonparking(b"D").expect("direct"), 1);
        let mut one = [0u8; 8];
        let n = reader.read(&mut one).expect("read D");
        assert_eq!(&one[..n], b"D");
    }

    /// A drainer that empties the buffer only because its peer died is not a
    /// completion: the blocking fence fails, and stays failed for later fences over the
    /// same sink. The polling fences ("does this process still hold bytes") read
    /// settled, so the update handoff's deadline does not stall on a loss `_exit`
    /// cannot recover.
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

    /// `egress_drained_to_kernel` follows the process-local spill: true with nothing
    /// spilled, false while a wedged tty's spill holds bytes, true again once the
    /// drainer empties it. The seamless handoff reads it so it never `_exit`s over input
    /// still in the spill.
    #[test]
    fn egress_drained_predicate_follows_the_spill() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
        assert!(
            sink.egress_drained_to_kernel(),
            "a fresh sink has nothing in its process-local egress"
        );

        // Wedge the tty so the next write must spill into this process's buffer.
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

        // Unwedge: drain the kernel buffer so the spill drainer can flush.
        let mut sink_bytes = Vec::new();
        let mut chunk = [0u8; 65536];
        while sink_bytes.len() < wedged + 4 {
            let n = reader.read(&mut chunk).expect("drain");
            assert!(n > 0, "peer closed early");
            sink_bytes.extend_from_slice(&chunk[..n]);
        }
        // The drainer runs on its own thread; poll the predicate until it settles.
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

    /// Declaring the master `O_NONBLOCK` switches the non-parking egress's write unit
    /// from one byte per `POLLOUT` check to the whole remaining frame without changing a
    /// delivered byte: the fast path lands inline, and a wedged foreground still spills
    /// in order. The flag defaults off, since only the owner that ran the `fcntl` knows
    /// the description's mode.
    #[test]
    fn declared_nonblocking_master_writes_whole_frames_in_order() {
        let (mut reader, writer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let sink = Arc::new(SinkWriter::new(writer.as_raw_fd()));
        assert!(
            !sink.shared.master_nonblocking.load(Ordering::Relaxed),
            "a fresh sink assumes the description may be BLOCKING (per-byte cadence)"
        );

        // Production shape: the direct-read gather flips the shared description.
        writer.set_nonblocking(true).expect("nonblocking master");
        sink.note_master_nonblocking(true);
        assert!(sink.shared.master_nonblocking.load(Ordering::Relaxed));

        // Fast path: room to spare, so the whole frame goes inline in one write.
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

        // Wedged: the whole-slice write is refused (or short-writes), and whatever
        // did not fit spills — order across the split must survive.
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

    /// A lost `try_lock` race costs a bounded spin, never a wait on the holder: the
    /// holder keeps the lock until the write has returned, so the write must concede to
    /// the spill while the lock is still held.
    ///
    /// The hold is an event, not a nap (a timed hold lets a write that waits on the lock
    /// pass): the holder releases only when told, or after `HOLD_CAP`, a hang detector
    /// for the defect, which then fails on `released`.
    #[test]
    fn contended_nonparking_write_concedes_within_a_bounded_spin() {
        /// How long the holder keeps the lock if nobody tells it to let go:
        /// only a write that waits on the holder ever sees it.
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
                // Published before the unlock, so a write that got the lock
                // only through this release reads it set.
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
        // A one-minute hang detector, never a latency budget: a wait on the holder
        // lasts its whole two-minute cap.
        assert!(
            took < aterm_spec::wait::MUST_HAPPEN,
            "the retry budget must be a bounded spin, not a wait on the holder (took {took:?})"
        );

        release_tx.send(()).expect("holder alive");
        holder.join().expect("holder thread");
        // Conceded to the spill: the drainer delivers once the holder releases.
        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).expect("read");
        assert_eq!(&buf[..n], b"K");
    }
}

#[cfg(all(test, unix))]
mod p04_direct_receipt_and_deferred_drainer_tests {
    //! The non-parking key write's receipt says whether the frame went straight to the
    //! kernel, and a concession with an arranger installed spawns no thread on the
    //! conceding (UI) thread.
    use super::*;
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A sink over one end of a socketpair; the other end reads what it wrote.
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
        // With the fd lock held here, the next non-parking frame concedes to the
        // spill and reports not direct.
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

    /// The egress fence stops egress at a frame boundary: refused while a
    /// frame is part-way in (the fd lock held, the spill empty); held, it
    /// keeps every frame out of the kernel (a non-parking one queues whole in
    /// the spill); dropped, the frame goes on.
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
        // A second concession while still pending appends behind and re-pokes, so a
        // poke the worker's queue dropped cannot strand the spill.
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
        // Nothing drains until the WORKER arranges the drainer…
        assert_eq!(sink.try_egress_drained_to_kernel(), Some(false));
        // …which is the worker-side half.
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

    /// THE DRAINER NEVER PARKS UNDER THE SPILL MUTEX. On a master that may
    /// block (no `note_master_nonblocking`), the drainer's `write(2)` into a
    /// full queue parks inside the syscall, so it runs with the spill mutex
    /// released: stood at its write (`Seam::DrainWrite`) the mutex is free,
    /// and once it is parked in the write a non-parking writer queueing
    /// behind the spill (the UI thread's next key) still appends at once. On
    /// a master declared non-blocking the write cannot park, and the look at
    /// the discard epoch and the write share one hold of the mutex
    /// (`drain_write`; a discard is then wholly before or after it, the pty
    /// test `a_discard_racing_the_drainers_write_drops_the_frame_whole`).
    ///
    /// NEGATIVE CONTROL: that declared lane, where the seam finds the mutex
    /// held. A drainer that held it on both lanes failed aterm-gui's
    /// `spilled_key_does_not_hold_later_queued_keys` on 2026-10-16 (its keys
    /// never left the FIFO writer), and fails this test's first half.
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
            // A key behind the spill, from another thread: it must come back
            // (a minute is the hang detector) while the drainer is parked.
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

    /// THE GUARDED WRITE NEVER CREATES A THREAD WHERE AN ARRANGER IS
    /// INSTALLED (every production session: its reply writer): the rest of a
    /// frame the kernel took only part of is committed under the PENDING mark
    /// and the arranger poked once, so the control thread that wrote it under
    /// the terminal lock waits for nothing; the worker's half then delivers it,
    /// whole, after the prefix. Over a pipe, whose short write is deterministic
    /// (the room a read made, smaller than the frame).
    ///
    /// NEGATIVE CONTROL: with no arranger the drainer is arranged inline (the
    /// thread exists, nothing is pending), as the non-parking body does
    /// (`without_an_arranger_the_concession_arranges_inline_as_before`).
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

/// A metered bulk frame (a large paste) reports its progress per `write(2)`, and a
/// stop cuts it into a well-formed frame.
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

    /// The cut finishes what the child is owed and drops the rest: nothing
    /// at all before the first byte; the begun UTF-8 sequence; the opening
    /// marker; and a bracketed paste's closing marker.
    #[test]
    fn a_stop_cut_keeps_the_frame_well_formed() {
        let plain = "ab\u{e9}cd".as_bytes(); // a b [c3 a9] c d
        assert_eq!(bulk_stop_cut(plain, 0), (0, plain.len()));
        assert_eq!(bulk_stop_cut(plain, 1), (1, plain.len()));
        // Mid-`é`: its continuation byte is sent, then nothing else.
        assert_eq!(bulk_stop_cut(plain, 3), (4, plain.len()));
        let framed = bracketed(b"hello");
        let len = framed.len();
        // Mid-marker: the opening marker completes, the close follows.
        assert_eq!(bulk_stop_cut(&framed, 2), (6, len - 6));
        assert_eq!(bulk_stop_cut(&framed, 8), (8, len - 6));
        // Already into the close: finish it.
        assert_eq!(bulk_stop_cut(&framed, len - 3), (len - 3, len - 3));
        assert_eq!(bulk_stop_cut(&framed, 0), (0, len));
    }

    /// A stop that lands before the frame begins writes nothing and says so.
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

    /// The meter follows the bytes the child has taken while the writer is
    /// parked on a full buffer, and a stop mid-frame ends the frame with its
    /// closing marker and drops only the unsent middle; the input after it
    /// arrives whole.
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

    /// An unstopped metered frame is byte-identical to the plain one and
    /// ends Delivered.
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

/// The metered body without a tty: what a stop owes the program, and the Windows
/// twin's half of it, on any host. The Windows body is [`write_metered`] over
/// [`Shared::write_stamped_through`], here into an in-memory ConPTY that takes every
/// slice whole.
#[cfg(test)]
mod metered_body_tests {
    use super::*;

    fn bracketed(body: &[u8]) -> Vec<u8> {
        [PASTE_OPEN, body, PASTE_CLOSE].concat()
    }

    /// When `Stop paste` is pressed in [`windows_body`].
    #[derive(Clone, Copy)]
    enum StopAt {
        Never,
        /// Between writes: after the n-th returned.
        After(usize),
        /// While the n-th write is blocked in ConPTY's `WriteFile`.
        Inside(usize),
    }

    /// The Windows metered body, as `write_frame_body_locked` runs it, with
    /// `Stop paste` pressed at `stop`: how [`write_metered`] ended the frame,
    /// the bytes accepted in all (the frame's and the owed close's), and what
    /// the program received. The stand-in for ConPTY's input answers as
    /// `aterm_pty::write_some_stoppable` does: a write whose stop flag is set
    /// when it begins, or is set while it is blocked (the stop cancels it),
    /// writes nothing and answers `Ok(0)`.
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

    /// WINDOWS: A STOP REACHES THE WRITE, AND THE PASTE STILL ENDS CLOSED.
    /// Pressed between writes or while a write is blocked in `WriteFile`, the
    /// stop ends the frame at the write it reaches: the twin hands that write
    /// the frame's own stop flag, which `aterm_pty` cancels it on. The cut's
    /// debt (the rest of the begun character, the closing `ESC [ 201 ~`)
    /// comes back to the body, which writes it with no stop to drop it. RED
    /// with the twin handing the write no flag: a stop inside a blocked write
    /// is not seen until it returns, and the rest of its slice goes through
    /// (KNOWN_ISSUES ATERM-KI-1014-6, until 2026-10-19).
    #[test]
    fn a_stop_reaches_the_windows_write_and_the_paste_ends_closed() {
        // `é` straddles the first slice boundary: its lead byte is the
        // slice's last, so the cut owes its continuation byte.
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
        // Stopped inside the first write: nothing reached the program, and
        // nothing is owed.
        let (end, accepted, conpty) = windows_body(&frame, StopAt::Inside(1));
        assert!(end.owed.is_empty());
        assert_eq!((accepted, conpty.len()), (0, 0));
    }

    /// Negative control: an unstopped frame goes through the twin whole.
    #[test]
    fn the_windows_twin_writes_an_unstopped_frame_whole() {
        let frame = bracketed(&vec![b'p'; 3 * METERED_SLICE]);
        let (end, accepted, conpty) = windows_body(&frame, StopAt::Never);
        assert_eq!(conpty, frame);
        assert_eq!(accepted, frame.len());
        assert!(end.owed.is_empty());
    }

    /// WINDOWS: THE CLOSE A STOP OWES WAITS FOR THE PROGRAM, AND ONLY THE TAB'S
    /// CLOSE RELEASES IT. The program reads the paste's first slice and then
    /// stops reading. `Stop paste` cancels the blocked second write, and the
    /// close the cut owes is written next with no stop flag: on Windows there
    /// is no spill to queue it in, so the session's ordered writer waits in
    /// `WriteFile` for room for it, as it would for any later key. Pressing
    /// `Stop paste` again reaches nothing, since the write carries no flag. The
    /// sever cancels it (`aterm_pty::release_input_writes`). Nothing more of the
    /// frame is written, and the delivery is not retried.
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
                // The program reads the first slice, then stops reading.
                1 => {
                    conpty.extend_from_slice(bytes);
                    Ok(bytes.len())
                }
                // Blocked; `Stop paste` cancels it through the frame's flag.
                2 => {
                    meter.request_stop();
                    Ok(if flag.is_some_and(|f| f.load(Ordering::Acquire)) {
                        0
                    } else {
                        bytes.len()
                    })
                }
                // The owed close, blocked: a second `Stop paste` cannot
                // reach a write that carries no flag; the sever cancels it.
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

    /// UNIX: A GIVE-UP HANDS BACK WHAT THE CUT OWES. The unix twin answers
    /// `Ok(0)` when a stop finds the tty not draining; [`write_metered`] then
    /// returns the bytes the program is still owed, for the body to deliver
    /// ahead of the next frame. Stopped in the middle of the opening marker,
    /// of a character, and of the text; and a give-up with no stop (a peer
    /// that closed) owes nothing.
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
        // Mid-marker: the rest of `ESC [ 200 ~`, then the close.
        assert_eq!(
            gave_up_at(&frame, true, 2),
            [&PASTE_OPEN[2..], PASTE_CLOSE].concat()
        );
        // Mid-`é` (open 6 + `ab` 2 + its lead byte): its continuation, the close.
        assert_eq!(
            gave_up_at(&frame, true, 9),
            [&frame[9..10], PASTE_CLOSE].concat()
        );
        // Between characters: just the close.
        assert_eq!(gave_up_at(&frame, true, 10), PASTE_CLOSE.to_vec());
        // Mid-close: its rest.
        let into_close = frame.len() - 2;
        assert_eq!(
            gave_up_at(&frame, true, into_close),
            frame[into_close..].to_vec()
        );
        // Nothing sent yet: nothing owed. No stop (a closed peer): nothing owed.
        assert!(gave_up_at(&frame, true, 0).is_empty());
        assert!(gave_up_at(&frame, false, 10).is_empty());
        // An unbracketed frame owes only the rest of a begun character.
        let plain = "ab\u{e9}cd".as_bytes();
        assert_eq!(gave_up_at(plain, true, 3), plain[3..4].to_vec());
        assert!(gave_up_at(plain, true, 4).is_empty());
    }
}

/// The input/egress contract (aterm-gui's `app_input/egress_contract_tests.rs` drives
/// it too): a stop reaches a paste parked on a tty that is not draining, the UI
/// thread's egress refuses at the spill cap instead of parking, a sever unblocks every
/// waiter, and the reply queue holds a bounded byte budget.
#[cfg(all(test, unix))]
mod input_io_tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// A sink over one end of a non-blocking socketpair filled solid (the production
    /// master's `O_NONBLOCK` description with a program that stopped reading), and the
    /// reader end (blocking, with a timeout).
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

    /// Each pass of `sink` at `park` from now on, as it passes it.
    fn parks(sink: &SinkWriter, park: Seam) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel();
        sink.seam_for_testing(move |seam| {
            if seam == park {
                let _ = tx.send(());
            }
        });
        rx
    }

    /// Wait (a hang detector) for a [`Seam::WriterPark`] passed after this call,
    /// letting go of the ones the writer reported before it.
    fn parked_after(parks: &mpsc::Receiver<()>) {
        while parks.try_recv().is_ok() {}
        parks
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the blocking writer parked on the full queue");
    }

    /// Fill the spill to `SPILL_CAP` through the interactive egress and return the
    /// frames it accepted, oldest first. Odd-sized frames, so the last accepted one
    /// carries the spill strictly past the cap (the room wait's condition) and the
    /// next is refused.
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

    /// A stop reaches a writer parked on a tty that is not draining
    /// (docs/AUDIT-performance-quality-2026-08-29.md P0): the program took the head of
    /// a bracketed paste and stopped reading; the writer (in production the session's
    /// one ordered egress thread) gives the frame up within a recheck period, the meter
    /// saying Stopped.
    ///
    /// The program still gets its close: the key typed after the stop is written while
    /// the program is paused, and when it resumes, well past a recheck period, it reads
    /// the paste's head, the closing bracket, then the key.
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
        // The program reads the fill and a little of the paste, then stops.
        let mut head = vec![0u8; filled + 64 * 1024];
        reader.read_exact(&mut head).expect("the fill and a head");
        // A park reported after the read, with nobody reading since, is on a
        // full queue: the writer took the room the read made.
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

        // The key typed after the stop, while the program is still paused:
        // it queues, and returns at once (the writer is not parked on it).
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

        // The program resumes: the paste's head, CLOSED, then the key. Read up
        // to the key (the one `K` in the stream) under the reader's hang
        // detector: the close and the key come from the drainer.
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

    /// A stop also ends a paste still waiting for room behind a spill past
    /// `SPILL_CAP`: nothing is appended, and the meter says Stopped with nothing sent.
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

    /// THE UI THREAD'S EGRESS NEVER PARKS: stall the program, fill the spill
    /// to its cap, type a key. The key is REFUSED at once — no byte accepted,
    /// no order, `is_refused` — never parked and never falsely reported
    /// delivered; everything accepted before it reaches the program in order
    /// once it reads, the refused key nowhere among it; and a key typed after
    /// the program has read goes through. The expendable-thread twin,
    /// `write_frame_nonparking`, PARKS on the same full spill (the backpressure
    /// `tests/spill_bound.rs` pins) and lands after the accepted frames — the
    /// shape that used to freeze every window when the UI thread reached it.
    #[test]
    fn the_interactive_egress_refuses_at_the_spill_cap_and_never_parks() {
        let (sink, mut reader, filled) = wedged();
        let accepted = fill_spill_to_the_cap(&sink);

        // On a helper thread, so a regression fails here rather than hanging the
        // suite: a parking body would never return.
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

        // Negative control: the expendable-thread write parks right here.
        let (tx, rx) = mpsc::channel();
        let parked = {
            let sink = sink.clone();
            std::thread::spawn(move || {
                tx.send(sink.write_frame_nonparking(b"P").map_err(|e| e.to_string()))
                    .expect("receiver");
            })
        };

        // The program reads: every accepted frame arrives in order, then the
        // parked control's byte; the refused key is nowhere.
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

    /// A UI-thread frame larger than `NONPARK_MAX` (a long IME commit, a key binding's
    /// bytes) does not take the expendable twin's blocking path: into a stalled program
    /// it spills like a key and the call returns at once.
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

    /// A SEVER ENDS A DRAINER PARKED ON A FULL QUEUE: a sever flushes nothing, so
    /// only the park's recheck of the epoch can end a drainer stood at its park.
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

    /// TEARDOWN UNBLOCKS EVERY WAITER. Three writers wait on a program that
    /// will never read again: a paste parked on the direct lane, a key waiting
    /// for spill room behind a spill past its cap, and the spill drainer. A
    /// sever returns all of them promptly; every later write fails at entry;
    /// the completion fence reports the dropped spill as a loss.
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
        // The direct-lane paste: the spill is empty, so it takes the fd lock
        // and parks in the poll loop.
        let paste = spawn("paste", vec![b'p'; 64 * 1024]);
        parked_after(&parks);
        // Keys behind it concede to the spill (the lock is held) until the
        // cap; one more blocking frame then waits for room.
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

    /// A waiter for spill room arranges a pending drainer itself. UI-thread concessions
    /// defer the spawn to the arranger (the reply writer's queue) and mark it pending; a
    /// blocking writer then waiting for room (the reply writer itself, with an oversized
    /// reply ahead of the poke) must not wait on a drainer whose arrangement sits behind
    /// it in its own queue, or the spill never drains.
    #[test]
    fn a_waiter_for_spill_room_arranges_a_pending_drainer_itself() {
        let (sink, mut reader, filled) = wedged();
        // An arranger whose worker never runs: the pending mark is all a
        // concession leaves.
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
        // The program reads everything, the spill included.
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

    /// The reply budget admits reservations up to [`REPLY_BUDGET_BYTES`] and refuses
    /// the next; a lone reservation larger than the budget is admitted only when
    /// nothing is held; dropping a permit returns its bytes.
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
