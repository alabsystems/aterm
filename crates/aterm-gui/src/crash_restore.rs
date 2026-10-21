// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S CRASH-RESTORE JOURNAL — the exact screen and scrollback after an
//! unplanned window death (increments 4 and 5 of the crash-restore plan; the headless half
//! is `aterm-restore`, the keeper's custody is `aterm-keeper`'s companions).
//!
//! A seamless update carries each session's checkpoint across. A SIGKILL, a jetsam
//! kill, a Force Quit or a crash leaves nothing to carry from: the process that would
//! write it is gone, so before this the keeper's recovery brought every screen back
//! blank (the META rung, `seamless::recovered_checkpoint`) and asked its program to
//! redraw. So this window writes, continuously and at bounded cost, what a restore
//! needs into a place that outlives it: one anonymous shared-memory REGION per session
//! (`aterm_uds::shm::Region`), whose read-only custody descriptor the keeper holds
//! beside the session's master (never mapping it). After this window dies the keeper
//! offers the next window both; that window maps the region read-only, rebuilds the
//! screen in a scratch engine ([`restore_offered`]) and adopts it through the landing
//! an update uses.
//!
//! WHO WRITES WHAT (the plan's §2.2; one writer per word):
//!
//! * the PARSE THREAD (the reader's parse loop, `spawn.rs`), inside the term-lock hold
//!   that processed each slice and after `Terminal::process`: the slice's bytes, every
//!   resize the engine applied before them (the temporal recorder's watermark,
//!   `temporal::SpineMark`, writing to the tail instead of a channel), the verified
//!   OSC 133/633 marks it counted, and the keyframes the policy calls for (captured at
//!   the end of a hold, encoded and copied after it). A slice that panics the parser
//!   never enters the tail, so a restore never replays the slice that killed its
//!   writer;
//! * the GATHER THREAD: `stream_gathered` after every `read()`, before the batch is
//!   sent, so a restore counts what was read and never processed;
//! * the HOST's resize, config and cell-size paths, in the hold of the engine mutation:
//!   the resize word ([`SessionJournal::host_resized`]) and the cell size and config
//!   fingerprint ([`SessionJournal::host_settings`]), then a nudge down the session's
//!   reader lane (`manual_reset::ResetLane::nudge`) so the parse thread records the
//!   resize, or keyframes the new settings (K4), with no output to wait for;
//! * the JOURNAL WORKER (one thread per window, [`HistoryWorker`]): the history ring and
//!   its words (module section "THE HISTORY RING" below), and the control slot (module
//!   section "THE CONTROL SLOT").
//!
//! The region is `Send` and not `Sync`, so they share it through one lock
//! (`aterm_restore::Shared`) taken per primitive store. The parse thread's own state
//! (the writer, the policy, the watermark) sits behind a second lock no other thread
//! takes, held only inside the term lock or after it: term → journal → region, and the
//! host and gather paths take only the region's. The worker's ring state is taken out of
//! its slot for a tick, so it holds no lock of its own while it takes the term lock; the
//! one lock both sides share is the leaf that says how far the ring has exported.
//!
//! THE HISTORY RING (increment 5: the exact scrollback). A keyframe carries a few hundred
//! lines of history at most (more would cost the parse thread under the term lock), so the
//! scrollback above them rides a ring of LZ4 chunks of its own, written by the journal
//! worker, never by the parse thread:
//!
//! * The worker wakes on a session's first history growth since its last tick and ticks
//!   it at most once a second (every five while its history floods, whose lines the
//!   engine mostly evicts before anyone could scroll to them), at once on K1's
//!   quiescence (the burst ended: catch up) and on a clear (the ring holds lines the
//!   person just erased: zero them). Idle, it waits with no timeout.
//! * A tick reads the history's fence under a brief term lock and exports what the ring
//!   has not reached, a chunk of at most 1024 lines per term-lock hold, encoded and copied
//!   with no lock held. A clear starts the ring over and zeroes every byte it held; a
//!   reflow or a reveal (a new numbering) starts it over once the new identity has held
//!   two seconds, so a window drag re-exports once; a cap too small for what the engine
//!   retains grows by the plan's §2.6 formula; an engine that evicted past the ring's
//!   reach restarts it at its oldest row, so the ring's rows never hold a hole.
//! * A keyframe's bridge is only what the ring lacks ([`bridge_lines`]): none once the
//!   worker has caught up, so the common keyframe is the visible screen's 9 µs. Its shell
//!   marks and blocks still reach the bridge's cap above the screen, whatever the bridge
//!   (`Terminal::checkpoint_journal_marked`): the ring carries their lines as text alone.
//! * The restore joins the ring below the restored lines with the update handoff's own
//!   join ([`journal_history`]): the lines it takes are imported after the recovery's
//!   restore drain exactly as an update's sidecar is after Commit, and every line it
//!   cannot take is counted (`status history_lost=`, the recovery row, the ledger). The
//!   import then keeps what the pane's own retention keeps, with the update's one
//!   uncounted wrinkle (`handoff_history`'s module doc): a history at its line limit
//!   keeps up to one hot ring (1,000 lines) fewer of its oldest lines until new output
//!   refills the ring.
//! * An adopted session's export waits for its import ([`SessionJournal::import_settled`]),
//!   which puts older lines below the carried ones, then exports them all and keyframes
//!   the renumbering the import made: a recovered shell that crashes again keeps its
//!   scrollback.
//!
//! THE MEMORY SHED (increment 6). A session's history ring is the journal's one large
//! cost: up to a few MiB at a full scrollback, every page of it shared memory charged to
//! this window, whose footprint jetsam reads. Under memory pressure the journal worker
//! takes rings away ([`ShedPolicy`]: the largest under WARN until half is gone, every one
//! under CRITICAL, back one at a time once the pressure has eased a minute). A shed ring
//! exports nothing, and the keyframes carry their own bridge of history, as before the
//! ring. Shared pages stay resident for as long as their region lives (`madvise` cannot
//! give them back), so a ring that touched any is shed by replacing the session's region
//! with a fresh one that never touches its ring ([`SessionJournal::replace_region`]): a
//! keyframe of the engine as it stands in a region laid out anew, the old one closed with
//! a SUPERSEDED record and unmapped, and its custody handed to the keeper, which closes the
//! old one. The screen still restores exactly; a crash meanwhile counts the scrollback
//! above the bridge lost (`status history_lost=`). What every region costs is counted at
//! the store, page by page (`aterm_restore::Touched`): `status journal_bytes=` per session,
//! `metrics journal_resident_bytes=` for the window. Until a replacement lands, a clear
//! still zeroes the ring at once in the region it lives in; a replacement the parser
//! defers (K5) waits for the program's next bytes, as the parse loop does, keeping the
//! region it created for that try; and a custody descriptor the keeper link's queue is too
//! full to take is kept and offered again, never dropped
//! (`keeper_link::CompanionOffer::Busy`), the old region kept mapped meanwhile, since the
//! keeper still holds it, so a clear made then erases it at once
//! (`aterm_restore::erase_region`). Until both have happened the old region is still in
//! memory: `status journal_ring=shedding`.
//!
//! THE CONTROL SLOT (increment 7). An update carries each session's turn ledger across
//! (`handoff_carry`); a crash left none, so a manager's `history` in the next window could
//! not find the turn it had just driven. So the region's 64 KiB control slot holds the
//! ledger's crash carry (`handoff_carry::CrashLedger`: the records, their floors and this
//! process's turn-id count, no alt-screen archive, which changes every frame and would
//! describe another screen than the restored one), LZ4-compressed in one astream step
//! record (`aterm_restore::ControlWriter`), its oldest records shed when the whole ledger
//! does not fit (a few hundred records of long prompts fill it), the carry then vouching
//! for none of their ids and `status journal_ledger_lag=` counting them:
//!
//! * The journal worker writes it, at most every two seconds and only after the ledger
//!   took a record (its watch wakes the worker, [`SessionJournal::carry_ledger`]), with no
//!   lock held but the ledger's for the clone of its records. An idle session costs
//!   nothing. An adopted session's ledger (an update's carry, a crash's) is written at the
//!   spawn, before the registration hands the keeper the region. The shed's replacement
//!   region is given the slot's record before its custody goes to the keeper.
//! * A death inside a write leaves the slot empty (its commit word is 0 until the copy and
//!   the length are stored), so the restore carries no ledger, never a wrong one. One
//!   slot: that death loses the carry before the write too, the session's whole ledger
//!   (microseconds a carry). A record landing in the two seconds before a death is lost
//!   with it.
//! * Beside the slot, the region's `ctl_count` word holds the process's turn-id count,
//!   stored by every mint before its id is anyone's ([`note_turn_ids`]) into every live
//!   journal's region; no control write touches it.
//! * The restore reads both apart from the screen's rungs ([`restore_offered`]), so the
//!   ledger rides a META restore too, and adopts the slot's carry as an update adopts its
//!   sidecar (`Adopted::control`: `history` prints the records `carried=1`). The turn-id
//!   count goes on above every region's count plus the update's reserve
//!   ([`continue_carried_turns`]), past every id the dead window minted in any session,
//!   and each carried ledger stops vouching for every id below that: the dead window may
//!   have recorded turns after its last carry, or torn it, so a `history` or a resumed
//!   `since-turn=` below the floor is told (`unheld_below=`, a GAP), never left to read no
//!   turn as no turn landed. A record's `arch=` mark names the dead window's archive
//!   origin, which no later archive takes, so `offscreen since=` with it reads from the
//!   start and says why.
//!
//! COST WHILE IDLE: nothing. The parse loop waits for its next batch with a timeout
//! (K1's 200 ms) only while the session has tail past its keyframe that a quiet wait can
//! keyframe; an idle session blocks as before and wakes zero times. One whose output
//! stopped inside a sequence no keyframe carries (an unterminated DECRQSS or XTGETTCAP
//! query, an over-cap OSC, an APC under way) has tail no capture can rebase until more
//! bytes arrive, so its deferred keyframe (K5) parks it (`KeyframePolicy::dirty`) and it
//! blocks too, rather than retry under the terminal lock five times a second.
//!
//! ON WHEREVER THE KEEPER LINK IS (`[keeper] enabled`), with no setting of its own, and
//! only when the keeper this window booted with takes companions, or when none
//! answered: an update's candidate says no boot HELLO at all (it registers at its
//! Commit), and a window whose keeper was down at boot is linked to the next one that
//! answers, which the link tells every region it keeps beside its master. The plan made
//! a journal wait for a WELCOME stating `CAP_JOURNAL`, which would leave every session
//! of an updated window, and of a window booted while its keeper was down, with no
//! restore. So `journal=live` says the session writes its region, not that a keeper
//! holds it yet; the keeper's own `status journals=` counts the regions it holds. A
//! region that cannot be created leaves the session without one, said by `status
//! journal=off journal_off=…`; a journal whose writes fail stops and asks the keeper to
//! drop its region (COMPANION_RELEASE), so a stale region is never restored as if it
//! were current.
//!
//! LOCAL ONLY: no control verb reads a region and status reports counts only — that is
//! what keeps a region on the host, since this crate also runs the fleet's listener. The
//! census guard `restore-stays-local` keeps `aterm-restore` out of the closure of every
//! crate that carries bytes off the host (the fabric's link, the fleet's TLS crate, the
//! HTTP stream, the fabric endpoint). The region is this window's memory; its raw tail can hold the
//! shell-integration nonce as program output, as the master the keeper holds does, and
//! a restored shell re-keys (the checkpoint carries no nonce). Its control slot holds the
//! turn ledger's texts as `history` prints them (at most 512 bytes a record), which an
//! update's sidecar puts on disk for a moment and this never does.

#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::Mutex;
use std::sync::OnceLock;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
use aterm_core::config::TerminalConfig;
#[cfg(unix)]
use aterm_core::grid::{HistoryFence, ResizePolicy};
#[cfg(unix)]
use aterm_core::terminal::{Terminal, TerminalCheckpoint};
#[cfg(unix)]
use aterm_restore::writer::{
    store_cell_px, store_config_fp, store_ctl_count, store_gathered, store_resize_word,
};
#[cfg(unix)]
use aterm_restore::{
    Geometry, HistoryIdentity, HistoryWriter, JournalWriter, KeyframeError, KeyframeInput,
    KeyframePolicy, REGION_LEN, Reason, RecoverPolicy, Recovery, Shared, Touched, WriteError,
    WriterIdentity, adaptive_hist_cap,
};

#[cfg(unix)]
use crate::temporal::{SpineMark, SpinePre, SpineResize, SpineSink, SpineWatermark};

/// The store a window journals through: its read-write mapping, counting the pages it
/// makes resident, shared by the threads that write it (module doc).
#[cfg(unix)]
pub(crate) type Store = Shared<Touched<aterm_uds::shm::Region>>;

/// The bytes every region this window maps keeps resident, summed (each region's
/// `aterm_restore::Touched` adds its pages here and takes them back when it is unmapped).
#[cfg(unix)]
fn resident_meter() -> &'static Arc<AtomicU64> {
    static METER: OnceLock<Arc<AtomicU64>> = OnceLock::new();
    METER.get_or_init(|| Arc::new(AtomicU64::new(0)))
}

/// `metrics journal_resident_bytes=`: the RAM this window's crash-restore regions cost it,
/// every page their writers touched (the plan's increment 6). Shared pages are charged to the
/// window that maps them, which the kernel's memory limits read; the keeper holds each
/// region unmapped, so a region the shed replaced leaves this count at once, and the
/// keeper's last descriptor of it goes when the new region's COMPANION reaches it.
pub(crate) fn resident_bytes() -> u64 {
    #[cfg(unix)]
    {
        resident_meter().load(Ordering::Relaxed)
    }
    #[cfg(not(unix))]
    {
        0
    }
}

// ---------------------------------------------------------------------------
// The fingerprint of what the engine runs under
// ---------------------------------------------------------------------------

/// The hash of the host config an engine was built or reconfigured with: FNV-1a over
/// its `Debug` rendering, which names every field (floats included) in a fixed order,
/// and a fixed word for no config at all (the boot session's placeholder path). Two
/// windows of one build running one config agree on it; two builds that render a
/// field differently disagree, which only costs a restore its exactness, never its
/// screen.
#[cfg(unix)]
pub(crate) fn config_hash(cfg: Option<&TerminalConfig>) -> u64 {
    match cfg {
        Some(cfg) => aterm_hash::fnv1a_64(format!("{cfg:?}").as_bytes()),
        None => aterm_hash::fnv1a_64(b"aterm-crash-restore:no-config"),
    }
}

/// The `config_fp` a keyframe and the header carry: the config's hash and the cell
/// size folded together, never 0 (a header word of 0 is a region whose host never
/// stored one, which a restore reads as "not checked").
///
/// The cell size is in it because an inline image in the tail advances the cursor by
/// the rows its pixel height spans: a font zoom changes how later output lays out, so
/// it forces a keyframe (K4) like a config change does, and a restorer replays under
/// the dead window's cell size (the header's `cell_px`, [`restore_offered`]).
#[cfg(unix)]
pub(crate) fn config_fp(config_hash: u64, cell_px: Option<(u16, u16)>) -> u64 {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&config_hash.to_le_bytes());
    bytes[8..].copy_from_slice(&aterm_restore::layout::cell_px_word(cell_px).to_le_bytes());
    aterm_hash::fnv1a_64(&bytes).max(1)
}

/// The engine a restore replays into: exactly the one a live session of this window is
/// built as (`spawn::new_live_terminal`: the configured scrollback store, the config,
/// the cell size, DEC 1007's host baseline), with no callbacks, so the replay reaches
/// the state the dead window's engine reached and its replies, clipboard writes and
/// bells go nowhere.
#[cfg(unix)]
fn scratch_engine(
    rows: u16,
    cols: u16,
    cfg: Option<&TerminalConfig>,
    cell_px: Option<(u16, u16)>,
) -> Terminal {
    crate::spawn::new_live_terminal(rows, cols, cfg, cell_px)
}

// ---------------------------------------------------------------------------
// The session's slot: what `status` reads
// ---------------------------------------------------------------------------

/// Which rung a keeper-recovered session landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only a unix keeper recovery restores a session")
)]
pub(crate) enum Rung {
    /// The screen, modes and parser carry exactly as the dead window had them, every
    /// byte it read replayed (no size pulse for a main-screen session).
    Exact,
    /// A screen from the region with a loss counted (a keyframe alone, an overflowed
    /// tail, a byte read and never processed, a resize or a mark the tail missed, a kitty
    /// image command in the tail, a tail recorded under another config, a region the
    /// memory shed had superseded when the window died): the program is pulsed to redraw
    /// over it.
    Screen,
    /// No screen: a blank one carrying the keeper's META scalars, as before the
    /// journal (no region, a refused one, a second failed restore, a panic).
    Meta,
}

impl Rung {
    /// The `status restore=` word.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Screen => "screen",
            Self::Meta => "meta",
        }
    }
}

/// What a keeper recovery restored for one session, and what it could not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestoreReport {
    pub(crate) rung: Rung,
    /// Why it is not exact (`-` when it is): the first loss, or why there is no screen.
    pub(crate) why: &'static str,
    /// The keeper's poison brake as the offer stated it.
    pub(crate) attempts: u8,
    /// PTY bytes the dead window read and never committed to its tail.
    pub(crate) in_flight_lost: u64,
    /// The tail the keyframe needed had been overwritten.
    pub(crate) tail_overflow: bool,
    /// Verified shell-integration marks left in the tail (a replay drops them).
    pub(crate) marks_lost: u64,
    /// Host resizes neither the keyframe nor the tail reached.
    pub(crate) resizes_unjournaled: u64,
    /// History lines the restore could not bring back (the join's count, the session's
    /// first `status history_lost=`). On the META rung, every line the region held when
    /// the restore read it before landing no screen (`cells`: [`history_held`]); 0 when
    /// nothing of the region was read (no region, the brake, a header or keyframe this
    /// build cannot read, a panic), where nothing says how much there was: the band row
    /// says such a screen's scrollback is not here, and `restore_why=` says why.
    pub(crate) history_lost: u64,
}

impl RestoreReport {
    /// The META rung, for `why`.
    #[cfg(unix)]
    pub(crate) fn meta(why: &'static str, attempts: u8) -> Self {
        Self {
            rung: Rung::Meta,
            why,
            attempts,
            in_flight_lost: 0,
            tail_overflow: false,
            marks_lost: 0,
            resizes_unjournaled: 0,
            history_lost: 0,
        }
    }

    /// The `status` words after `journal=`: `restore=<rung> restore_why=<why>
    /// in_flight_lost=<n> tail_overflow=<0|1> marks_lost=<n> resizes_unjournaled=<n>`.
    pub(crate) fn status_words(&self) -> String {
        format!(
            "restore={} restore_why={} in_flight_lost={} tail_overflow={} marks_lost={} \
             resizes_unjournaled={}",
            self.rung.word(),
            self.why,
            self.in_flight_lost,
            u8::from(self.tail_overflow),
            self.marks_lost,
            self.resizes_unjournaled,
        )
    }
}

/// How many keeper-recovered sessions landed on each rung (the band row, the
/// recovery census).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RungCounts {
    pub(crate) exact: u32,
    pub(crate) screen: u32,
    pub(crate) meta: u32,
}

impl RungCounts {
    /// Count one more on `rung`.
    #[cfg(unix)]
    pub(crate) fn note(&mut self, rung: Rung) {
        let slot = match rung {
            Rung::Exact => &mut self.exact,
            Rung::Screen => &mut self.screen,
            Rung::Meta => &mut self.meta,
        };
        *slot = slot.saturating_add(1);
    }

    /// How many screens came back from a region (exact or not).
    pub(crate) fn screens(self) -> u32 {
        self.exact.saturating_add(self.screen)
    }
}

/// What a keeper recovery's restores came to: the rungs (the band row's
/// [`RungCounts`]) and the fields `status` reports for each recovered session, summed, for
/// the recovery census's ledger row (`aterm_update::recovery_ledger::Restores`), so the
/// ledger shows which loss real crashes hit, the signal the plan's follow-up work waits
/// for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RestoreTally {
    pub(crate) counts: RungCounts,
    pub(crate) in_flight_lost: u64,
    /// Sessions whose keyframe's tail had been overwritten.
    pub(crate) tail_overflows: u32,
    pub(crate) marks_lost: u64,
    pub(crate) resizes_unjournaled: u64,
    /// History lines the restores could not bring back (the joins' counts), and in how
    /// many sessions: the band row's scrollback words, and the signal a disk tier for the
    /// history would wait for (the plan's §8).
    pub(crate) history_lost: u64,
    pub(crate) history_short: u32,
    /// Of `history_lost`, the lines of sessions on the META rung, whose whole scrollback
    /// went with their screen: the band row says those apart ("the blank ones' scrollback
    /// is not here") and counts only the restored screens' ([`Self::screens_history_lost`]).
    pub(crate) meta_history_lost: u64,
    /// How many sessions each `restore_why` word named (none for an exact one).
    pub(crate) whys: std::collections::BTreeMap<&'static str, u32>,
}

impl RestoreTally {
    /// Count one more recovered session, restored as `report` says.
    #[cfg(unix)]
    pub(crate) fn note(&mut self, report: &RestoreReport) {
        self.counts.note(report.rung);
        self.in_flight_lost = self.in_flight_lost.saturating_add(report.in_flight_lost);
        self.tail_overflows = self
            .tail_overflows
            .saturating_add(u32::from(report.tail_overflow));
        self.marks_lost = self.marks_lost.saturating_add(report.marks_lost);
        self.resizes_unjournaled = self
            .resizes_unjournaled
            .saturating_add(report.resizes_unjournaled);
        self.history_lost = self.history_lost.saturating_add(report.history_lost);
        if report.rung == Rung::Meta {
            self.meta_history_lost = self.meta_history_lost.saturating_add(report.history_lost);
        }
        self.history_short = self
            .history_short
            .saturating_add(u32::from(report.history_lost > 0));
        if report.rung != Rung::Exact {
            let n = self.whys.entry(report.why).or_insert(0);
            *n = n.saturating_add(1);
        }
    }

    /// The history lines the restored screens could not bring back, the band row's count
    /// (every line of a blank screen's is said apart, in words).
    pub(crate) fn screens_history_lost(&self) -> u64 {
        self.history_lost.saturating_sub(self.meta_history_lost)
    }

    /// The ledger row's form of it.
    #[cfg(unix)]
    pub(crate) fn ledger(&self) -> aterm_update::recovery_ledger::Restores {
        aterm_update::recovery_ledger::Restores {
            exact: self.counts.exact,
            screen: self.counts.screen,
            meta: self.counts.meta,
            in_flight_lost: self.in_flight_lost,
            tail_overflows: self.tail_overflows,
            marks_lost: self.marks_lost,
            resizes_unjournaled: self.resizes_unjournaled,
            history_lost: self.history_lost,
            history_short: self.history_short,
            whys: self
                .whys
                .iter()
                .map(|(word, n)| ((*word).to_string(), *n))
                .collect(),
            // The offers the admission refused never reach the tally: the recovery counts
            // them (`keeper_link::Recovery::ledger`).
            refused: 0,
        }
    }
}

/// The session's journal slot (`SessionCtx::crash_journal`): its journal once one is
/// installed, why it has none, and what a keeper recovery restored for it.
#[derive(Default)]
pub(crate) struct JournalSlot {
    #[cfg(unix)]
    journal: OnceLock<Arc<SessionJournal>>,
    off: OnceLock<String>,
    restored: OnceLock<RestoreReport>,
}

/// A live journal as [`JournalSlot::status_words`] reads it: why it broke
/// (`None` while it writes), then its tail, its ring's lag, its ring's word
/// (`live`, `shedding` or `shed`), the bytes its region holds resident, and the ledger's
/// records its control slot does not hold.
type JournalSample = (Option<String>, (u64, u64, &'static str, usize, u64));

impl JournalSlot {
    /// Install the session's journal (once, at its spawn).
    #[cfg(unix)]
    pub(crate) fn install(&self, journal: Arc<SessionJournal>) {
        let _ = self.journal.set(journal);
    }

    /// The session's journal, if it has one.
    #[cfg(unix)]
    pub(crate) fn get(&self) -> Option<&Arc<SessionJournal>> {
        self.journal.get()
    }

    /// The host resized the session's engine (the caller holds its lock, in the resize's
    /// hold): [`SessionJournal::host_resized`] when it has a journal, else nothing.
    pub(crate) fn host_resized(&self, t: &aterm_core::terminal::Terminal) {
        #[cfg(unix)]
        if let Some(journal) = self.journal.get() {
            journal.host_resized(t);
        }
        #[cfg(not(unix))]
        let _ = t;
    }

    /// The host changed the session engine's settings (the caller holds its lock, in the
    /// setter's hold): [`SessionJournal::host_settings`] when it has a journal.
    pub(crate) fn host_settings(
        &self,
        t: &aterm_core::terminal::Terminal,
        cfg: Option<&aterm_core::config::TerminalConfig>,
    ) {
        #[cfg(unix)]
        if let Some(journal) = self.journal.get() {
            journal.host_settings(t, cfg);
        }
        #[cfg(not(unix))]
        let _ = (t, cfg);
    }

    /// The session runs without a journal, for `why` (a region that could not be made).
    pub(crate) fn turn_off(&self, why: String) {
        let _ = self.off.set(why);
    }

    /// What a keeper recovery restored for this session.
    pub(crate) fn note_restored(&self, report: RestoreReport) {
        let _ = self.restored.set(report);
    }

    /// What a keeper recovery restored for this session, if it was recovered.
    #[cfg(all(test, unix))]
    pub(crate) fn restored(&self) -> Option<&RestoreReport> {
        self.restored.get()
    }

    /// The `status` words: `journal=live` with `journal_tail=` (the tail a restore now
    /// would replay; 0 once an idle session's quiet keyframe has caught up),
    /// `journal_ring_lag=` (history lines the history ring has not exported yet; 0 once
    /// the journal worker has caught the history up), `journal_ring=<live|shedding|shed>`
    /// (`shed`: memory pressure took the history ring away, so a crash brings back the
    /// screen and the keyframe's few hundred lines of history and counts the rest lost,
    /// until the pressure has eased a minute and the ring is exported again; `shedding`:
    /// the shed's old region, the ring's pages and the lines they hold, is still in memory,
    /// since it still waits to be replaced or the keeper link has not taken its
    /// replacement's custody yet, whether or not the pressure has eased since),
    /// `journal_bytes=` (the RAM the session's region costs this window, every page
    /// written) and `journal_ledger_lag=` (turn records of the session's ledger the region's
    /// control slot does not hold: taken since its last carry, at most two seconds, or shed
    /// from it to fit, [`SessionJournal::ledger_lag`]; 0 once it holds every record the
    /// ledger does), or `journal=off` with a `journal_off=`
    /// reason when the session has none
    /// (the keeper off: `journal_off=keeper-off`), then a recovered session's
    /// [`RestoreReport::status_words`].
    pub(crate) fn status_words(&self) -> String {
        #[cfg(unix)]
        let live: Option<JournalSample> = self.journal.get().map(|j| {
            (
                j.broken(),
                (
                    j.tail_bytes(),
                    j.ring_lag(),
                    if j.ring_shedding() {
                        "shedding"
                    } else if j.ring_shed() {
                        "shed"
                    } else {
                        "live"
                    },
                    j.resident_bytes(),
                    j.ledger_lag(),
                ),
            )
        });
        #[cfg(not(unix))]
        let live: Option<JournalSample> = None;
        let mut words = match (live, self.off.get()) {
            (Some((None, (tail, lag, ring, bytes, ledger_lag))), _) => format!(
                "journal=live journal_tail={tail} journal_ring_lag={lag} journal_ring={ring} \
                 journal_bytes={bytes} journal_ledger_lag={ledger_lag}"
            ),
            (Some((Some(why), _)), _) => format!(
                "journal=off journal_off={}",
                aterm_control::wire::pct_encode(&why)
            ),
            (None, Some(why)) => format!(
                "journal=off journal_off={}",
                aterm_control::wire::pct_encode(why)
            ),
            (None, None) => "journal=off journal_off=keeper-off".to_string(),
        };
        if let Some(report) = self.restored.get() {
            words.push(' ');
            words.push_str(&report.status_words());
        }
        words
    }
}

// ---------------------------------------------------------------------------
// The writer side
// ---------------------------------------------------------------------------

/// The parse thread's state for one session's region: the writer, the keyframe policy
/// and the resize watermark. Behind [`SessionJournal`]'s lock, which only the parse
/// thread (and the spawn, before the reader starts) takes.
#[cfg(unix)]
struct Writer {
    writer: JournalWriter<Store>,
    policy: KeyframePolicy,
    /// The journal's own resize watermark (never the temporal recorder's): seeded at
    /// creation, caught up before each slice's bytes and before each keyframe.
    mark: SpineMark,
    /// The clock the policy is handed (milliseconds since this journal's creation).
    epoch: Instant,
    /// Milliseconds a test has moved the clock on ([`SessionJournal::pass_ms`]), so a
    /// quiet keyframe is reached without sleeping. Always 0 outside a test.
    skew_ms: u64,
    /// The seam policy the session's engine resizes under (`pty_resize_policy`), which
    /// every RESIZE record replays under.
    resize_policy: ResizePolicy,
    /// The config fingerprint the parse thread last took from the host
    /// ([`SessionJournal::host_fp`]): what a keyframe is stamped with.
    config_fp: u64,
    /// The verified marks counted since the last decision (K6's evidence).
    slice_marks: u64,
    /// The last keyframe did not fit its slot: the next is captured with no bridge.
    no_bridge: bool,
    /// Keyframes written, and captures deferred (K5), for the log.
    keyframes: u64,
    deferred: u64,
    /// The development seam [`HOLD_KEYFRAMES_SEAM`]: no keyframe past the first but a
    /// forced one.
    hold: bool,
    /// Which region `writer` writes: 0 for the session's first, one more for each the
    /// memory shed put in its place ([`SessionJournal::replace_region`]). A keyframe
    /// captured for one region is never written into the next: the shed's own keyframe,
    /// taken later, already holds the engine the capture saw.
    region_gen: u64,
    /// The history's clear generation the region `writer` writes is settled to: no byte
    /// from before that clear is in it. The fence's at the region's first keyframe (a fresh
    /// region holds nothing from before it), then each clear's once its forced keyframe is
    /// written and the clear settled out of the region ([`SessionJournal::write`]). Behind
    /// the engine's while a recorded clear's keyframe is captured but not yet written, or
    /// deferred: a replacement then ([`SessionJournal::replace_region`]) skips that write,
    /// and with it the settle, so it erases the region it supersedes.
    settled_clear_gen: u64,
}

/// The journal's tail as a [`SpineSink`]: a resize the watermark hands it becomes a
/// RESIZE record (under the session's seam policy), a lost count a RESIZES_LOST record,
/// and the bytes BYTES records.
#[cfg(unix)]
struct TailSink<'a> {
    writer: &'a mut JournalWriter<Store>,
    policy: &'a mut KeyframePolicy,
    resize_policy: ResizePolicy,
    now_ms: u64,
    synthesized: bool,
    failed: Option<WriteError>,
}

#[cfg(unix)]
impl SpineSink for TailSink<'_> {
    fn resize(&mut self, resize: SpineResize) -> bool {
        let (rows, cols) = resize.to;
        match self.writer.append_resize(
            rows,
            cols,
            self.resize_policy,
            resize.lifetime,
            resize.ordinal,
        ) {
            Ok(()) => {
                // A width change or a rows grow replays inexactly (K7): keyframed soon
                // after. The live geometry recorded once (no `from`) counts as one.
                let regrid = resize
                    .from
                    .is_none_or(|(from_rows, from_cols)| cols != from_cols || rows > from_rows);
                self.policy.note_resize(self.now_ms, regrid);
                true
            }
            Err(e) => {
                self.failed = Some(e);
                false
            }
        }
    }

    fn resizes_lost(&mut self, n: u64) -> bool {
        match self
            .writer
            .append_resizes_lost(u32::try_from(n).unwrap_or(u32::MAX))
        {
            Ok(()) => true,
            Err(e) => {
                self.failed = Some(e);
                false
            }
        }
    }

    fn bytes(&mut self, bytes: &[u8]) -> bool {
        let appended = if self.synthesized {
            self.writer.append_synthesized(bytes)
        } else {
            self.writer.append_slice(bytes)
        };
        match appended {
            Ok(()) => true,
            Err(e) => {
                self.failed = Some(e);
                false
            }
        }
    }
}

#[cfg(unix)]
impl Writer {
    /// Whether a keyframe for `reason` is written: always, but under
    /// [`HOLD_KEYFRAMES_SEAM`], where only a forced one is (a clear's, whose settle the
    /// privacy claim needs, a config change's, without which every restore would count
    /// the setter, and an import's renumbering, without which the history ring would never
    /// join the held keyframe again).
    fn admits(&self, reason: Reason) -> bool {
        !self.hold || matches!(reason, Reason::Clear | Reason::Forced | Reason::Renumbered)
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(self.skew_ms)
    }

    /// Take the host's newest fingerprint: a change forces a keyframe (K4).
    fn sync_host(&mut self, host_fp: u64) {
        if host_fp != 0 && host_fp != self.config_fp {
            self.config_fp = host_fp;
            self.policy.note_config(host_fp);
        }
    }

    /// Record every resize the engine applied up to `pre`, before anything after it.
    fn catch_up(&mut self, t: &Terminal, pre: SpinePre) -> Result<(), WriteError> {
        let now_ms = self.now_ms();
        let Self {
            writer,
            policy,
            mark,
            resize_policy,
            ..
        } = self;
        let mut sink = TailSink {
            writer,
            policy,
            resize_policy: *resize_policy,
            now_ms,
            synthesized: false,
            failed: None,
        };
        if mark.catch_up(&mut sink, pre, t) {
            Ok(())
        } else {
            Err(sink.failed.unwrap_or(WriteError::Sequence))
        }
    }

    /// Capture a keyframe of `t` (the caller holds its lock) for `reason`: every resize
    /// first, so none the keyframe holds is recorded after it; the projection; the
    /// watermark seeded to it. `Ok(None)`: the capture was deferred (K5: the parser holds
    /// a sequence no keyframe carries whole), and is retried at the next slice end.
    ///
    /// `exported` is how far the history ring has exported the engine's history, and under
    /// which identity ([`SessionJournal::exported`]).
    fn capture(
        &mut self,
        t: &Terminal,
        reason: Reason,
        exported: Option<(HistoryIdentity, u64)>,
    ) -> Result<Option<Captured>, WriteError> {
        let pre = SpinePre::take(t);
        self.catch_up(t, pre)?;
        self.mark.seed(pre);
        let fence = t.history_fence();
        let cap = if self.no_bridge {
            0
        } else {
            reason.bridge_cap()
        };
        let bridge = cap.min(usize::try_from(bridge_lines(&fence, exported)).unwrap_or(usize::MAX));
        // The shell marks and blocks reach the cap whatever the bridge (none at all once
        // the ring has caught up): the ring brings their lines back as text alone, at the
        // rows the marks name, and a keyframe of a caught-up shell would otherwise carry
        // no prompt above its screen (`Terminal::checkpoint_journal_marked`).
        let Some(checkpoint) = t.checkpoint_journal_marked(bridge, cap) else {
            self.policy.keyframe_deferred(reason);
            self.deferred = self.deferred.saturating_add(1);
            return Ok(None);
        };
        Ok(Some(Captured {
            checkpoint,
            fence,
            lifetime: t.resize_lifetime(),
            ordinal: t.resize_ordinal(),
            bridge: u32::try_from(bridge).unwrap_or(u32::MAX),
            config_fp: self.config_fp,
            integration_on: t.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
            reason,
            tail_at: self.writer.tail_commit(),
            region_gen: self.region_gen,
        }))
    }
}

/// The history a keyframe must carry itself, before its bridge cap (the plan's increment
/// 5, step 2): the lines between where the history ring's export has reached and the
/// history's end, `fence.end - max(exported end, fence.oldest)`, when the ring was
/// exported under the fence's own identity; every line of the history otherwise (no
/// export yet, or one a reflow, reveal or clear left under an older identity, whose rows
/// name other lines now).
///
/// A keyframe's bridge exists to meet the ring: the restore joins the ring's lines below
/// the lines the checkpoint carries, and only when the two meet (`handoff_history::join`,
/// its `Outrun` otherwise). So a session the ring has caught up with keyframes with no
/// bridge at all, 9 µs under the lock for a shell's visible screen instead of about
/// 100 µs for 512 lines, and one the ring lags carries the lag, up to the cap. A ring
/// further behind than the cap (a flood, which the worker exports every 5 s) makes a
/// restore meanwhile count the history it cannot join; the worker catches up at
/// quiescence (K1 nudges it), after which the ring meets the newest keyframe again with
/// no new keyframe needed: a ring may run past the keyframe, never short of its bridge.
///
/// The ring carries lines as text alone. What R4's bridge also carried for those lines,
/// the completed shell marks and output blocks whose prompts sit in them, the keyframe
/// still carries to the cap whatever the bridge ([`Writer::capture`]). And a Kitty
/// unscroll in the tail, which R4's bridge fed with history lines, finds a scratch engine
/// holding none: the restore counts it and is not exact, so the program is pulsed to
/// redraw (`aterm_restore::recover::Losses::unscrolls_replayed`, `restore_why=unscroll`).
#[cfg(unix)]
fn bridge_lines(fence: &HistoryFence, exported: Option<(HistoryIdentity, u64)>) -> u64 {
    match exported {
        Some((identity, end)) if identity == fence_identity(fence) => {
            fence.end.saturating_sub(end.max(fence.oldest))
        }
        _ => fence.lines(),
    }
}

/// The identity half of `fence` (`aterm_restore::HistoryIdentity::of`, field by field): read
/// under the term lock by the capture and the worker's chunk reads, so it calls nothing a
/// lexical lock census could mistake for another `of` that takes a lock.
#[cfg(unix)]
fn fence_identity(fence: &HistoryFence) -> HistoryIdentity {
    HistoryIdentity {
        renumber_epoch: fence.renumber_epoch,
        clear_gen: fence.clear_gen,
        reveal_gen: fence.reveal_gen,
        cols: fence.cols,
    }
}

/// A keyframe captured under the term lock, written after it ([`SessionJournal::write`]).
/// The tail cannot move between the two: only the parse thread appends, and it writes a
/// capture before it processes anything else.
#[cfg(unix)]
pub(crate) struct Captured {
    checkpoint: TerminalCheckpoint,
    fence: HistoryFence,
    lifetime: u64,
    ordinal: u64,
    bridge: u32,
    config_fp: u64,
    /// The engine's shell-integration posture was `On` (a nonce in use): what the replay
    /// authorizes a nonce of its own for (`aterm_restore::RecoverPolicy::scratch_nonce`).
    integration_on: bool,
    reason: Reason,
    /// The tail's commit when captured: the keyframe's `tail_from`.
    tail_at: u64,
    /// The region it was captured for ([`Writer::region_gen`]).
    region_gen: u64,
}

/// What a hold read before processing its bytes, and what its records have counted so
/// far ([`SessionJournal::record`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlicePre {
    spine: SpinePre,
    marks: u64,
    clear_gen: u64,
    /// The history's end (`HistoryFence::end`) as of the last record: a record that moves
    /// it tells the journal worker the history grew.
    hist_end: u64,
    /// Something reached the tail in this hold, so a decision is due at its end.
    recorded: bool,
}

/// A fresh region for a session's journal, metered ([`resident_meter`]), and its read-only
/// custody descriptor: at a session's creation, and when the memory shed replaces its region.
///
/// # Errors
///
/// The region could not be created.
#[cfg(unix)]
fn new_region() -> Result<(Touched<aterm_uds::shm::Region>, OwnedFd), String> {
    // SAFETY: the backing object is modified only through this `Region`, which the
    // journal alone holds (behind `Shared`), and it is never resized after `create`'s one
    // `ftruncate` (macOS refuses a second). Unlinking the name and the custody
    // descriptor's `O_RDONLY` mode do not prevent aliases from reopening it. Exclusion
    // instead relies on custody going only to the keeper link (`take_custody`,
    // `hand_custody`), whose keeper holds it unmapped (the census guard
    // `keeper-no-mmap`, its clippy rows) and hands it on only beside an orphaned master,
    // after every window that claimed that master, this one included, has died: no
    // payload read through an alias happens while this `Region` lives and writes. A region
    // the shed replaced keeps only this window's synchronized count stores and privacy
    // erase until a SYNC confirms the keeper accepted the successor. The keeper never
    // maps a region while this window is alive, so the same exclusion holds for the
    // retired mapping until its last descriptor closes.
    let (region, custody) = unsafe { aterm_uds::shm::Region::create(REGION_LEN) }
        .map_err(|e| format!("region: {e}"))?;
    Ok((
        Touched::new(region, Some(Arc::clone(resident_meter()))),
        custody,
    ))
}

/// Who writes session `id`'s regions: this process, at its birth, under its build, for the
/// session's sid (header facts a restore reports, never trusts).
#[cfg(unix)]
fn writer_identity(id: u64, sid: &str, birth_us: u64) -> WriterIdentity<'_> {
    WriterIdentity {
        pid: std::process::id(),
        birth_us,
        local_id: id,
        build: crate::build_info::build_number().parse().unwrap_or(0),
        sid,
    }
}

/// One session's journal (module doc).
#[cfg(unix)]
pub(crate) struct SessionJournal {
    /// The region, shared with the gather thread and the host paths.
    store: Store,
    writer: Mutex<Writer>,
    /// PTY bytes the gather threads have read (all of this session's readers).
    gathered: AtomicU64,
    /// The hash of the config the host last applied to the engine ([`config_hash`]).
    cfg_hash: AtomicU64,
    /// The fingerprint the host last stored ([`config_fp`]); the parse thread takes it
    /// at its next turn ([`Writer::sync_host`]).
    host_fp: AtomicU64,
    /// The read-only custody descriptor, until the keeper link takes it with the
    /// session's registration.
    custody: Mutex<Option<JournalCustody>>,
    /// The session's reader lane, for the host paths' nudges.
    lane: Arc<crate::manual_reset::ResetLane>,
    /// The session's id, for the log and the companion's release.
    id: u64,
    /// Tail bytes past the newest keyframe, as the parse thread last left them: what a
    /// restore now would replay (`status journal_tail=`; 0 once a quiet keyframe has
    /// caught an idle session up).
    tail: AtomicU64,
    /// Why the journal stopped (a write the region refused): set once, and from then on
    /// nothing is written and the keeper is asked to drop the region.
    broken: OnceLock<String>,
    // -- the history ring (increment 5; module doc "THE HISTORY RING") --------------
    /// The ring's writer and the export's state: taken out by the thread that exports for
    /// the length of one tick (the journal worker; a test), so no lock of it is ever held
    /// while the term lock is taken. `None` only while a tick runs.
    ring: Mutex<Option<Ring>>,
    /// How far the ring has exported the history, and under which identity: what a
    /// keyframe's bridge is sized against ([`bridge_lines`]). A leaf lock: the parse thread
    /// reads it inside the term lock and its own, the exporting thread writes it holding
    /// neither.
    exported: Mutex<Option<(HistoryIdentity, u64)>>,
    /// [`RING_GREW`], [`RING_NUDGED`] and [`RING_CLEARED`]: what the parse thread and the
    /// host told the worker since its last tick of this session.
    ring_signal: AtomicU8,
    /// The ring's export waits for the session's history import (an adopted session's
    /// carry, imported after Commit or after a recovery's restore drain): exporting first
    /// would export the carried lines alone and miss the older ones the import puts below
    /// them. [`Self::import_settled`] opens it.
    export_held: AtomicBool,
    /// The history's end as the parse thread last saw it, for `status journal_ring_lag=`.
    seen_end: AtomicU64,
    /// The history's oldest row as the parse thread last saw it, beside [`Self::seen_end`]:
    /// a ring with nothing exported lags by the history's lines, never by its end row.
    seen_oldest: AtomicU64,
    /// The import attached older history, renumbering it with no output: the parse thread
    /// takes it at its next turn and forces a keyframe (`Reason::Renumbered`).
    renumbered: AtomicBool,
    /// The worker this session's ring is exported by, once [`Self::start_export`] gave it
    /// one: notified when the history grows, at quiescence and at a clear.
    worker: OnceLock<Arc<HistoryWorker>>,
    /// The ring's clock (milliseconds since this journal's creation), and what a test has
    /// moved it on by ([`Self::pass_ms`]).
    ring_epoch: Instant,
    ring_skew_ms: AtomicU64,
    /// Where a test stopped the ring's clock ([`Self::hold_ring_clock`]).
    #[cfg(test)]
    ring_held_ms: OnceLock<u64>,
    // -- the memory shed (increment 6; section "THE MEMORY SHED" below) ---------------
    /// The session's history ring is SHED: memory pressure took it away. Nothing is
    /// exported, keyframes carry their bridge of history as a region with no ring does,
    /// and the region the ring had touched is replaced by one that never touches it
    /// ([`Self::replace_region`]). Cleared when the pressure has eased
    /// ([`Self::request_unshed`]).
    history_shed: AtomicBool,
    /// The shed ring's region is still to be replaced: the pages the ring wrote are still
    /// this window's (`status journal_ring=shedding`, until the replacement lands or the
    /// shed eases first).
    replace_owed: AtomicBool,
    /// A replacement's predecessor may still be the keeper's: custody is queued or
    /// buffered, or its COMPANION has no accepted SYNC receipt yet. That mapping remains
    /// counted and privacy-clearable (`status journal_ring=shedding`) until confirmation.
    custody_owed: AtomicBool,
    /// The region's replacement waits for the program's next bytes: its last try found the
    /// parser holding a sequence no keyframe carries whole (K5), which only more bytes can
    /// end. Set by the worker under the term lock; the parse thread clears it at its next
    /// recorded slice and wakes the worker, so a session parked mid-sequence costs the
    /// worker nothing until then, as it costs its own parse loop nothing (module doc, COST
    /// WHILE IDLE).
    replace_parked: AtomicBool,
    /// Regions the shed replaced, for the log and the tests.
    regions_replaced: AtomicU64,
    /// A test's keeper link (`keeper_link::TestLink`, a queue the test fills and drains),
    /// offered replaced regions' custody in place of the process's
    /// ([`Self::link_custody_to`]).
    #[cfg(test)]
    test_link: Mutex<Option<Arc<crate::keeper_link::TestLink>>>,
    /// The session's sid and this process's birth, which every region it lays out names
    /// in its header (`aterm_restore::WriterIdentity`).
    sid: String,
    birth_us: u64,
    // -- the control slot (increment 7; section "THE CONTROL SLOT" below) -------------
    /// The session's turn ledger, whose crash carry the region's control slot holds
    /// ([`Self::carry_ledger`]).
    ledger: OnceLock<std::sync::Weak<Mutex<crate::turn_ledger::TurnLedger>>>,
    /// The ledger took a record since the worker last carried it (set by the ledger's
    /// watch, under its lock; taken by [`Self::control_tick`]).
    ledger_moved: AtomicBool,
    /// The control slot's writer and what it last carried: the journal worker's, and the
    /// spawn's before the worker has the session. A leaf lock.
    control: Mutex<Control>,
    /// The process's turn-id count as the region's `ctl_count` word holds it
    /// ([`Self::hold_turn_ids`]), including a predecessor no keeper has confirmed replacing.
    /// Its lock spans each store and the replacement's swap/layout, so no mint is lost.
    /// Minting takes only primitive store locks. Replacement can take the leaf control
    /// lock after dropping term/writer; control never takes this lock or a ledger lock.
    turn_ids: Mutex<TurnCountCustody>,
    /// An old boot authority this session's first accepted new journal retires.
    /// Bound before any export worker or registration, never changed afterward.
    recovery_count: OnceLock<Arc<AtomicBool>>,
}

#[cfg(unix)]
impl SessionJournal {
    /// Create session `id`'s region, lay it out and write its FIRST keyframe of `term`
    /// (the engine as the spawn left it: fresh, or hydrated from an adoption), before
    /// the reader starts and before anyone is told the region exists. `master` names the
    /// PTY whose seam policy every RESIZE record replays under; `cfg` is the config the
    /// engine was built with; `lane` is the session's reader lane.
    ///
    /// # Errors
    ///
    /// The region could not be created or laid out (the session then runs without one,
    /// and `status` says why).
    pub(crate) fn create(
        id: u64,
        sid: &str,
        term: &Mutex<Terminal>,
        master: i32,
        cfg: Option<&TerminalConfig>,
        lane: Arc<crate::manual_reset::ResetLane>,
    ) -> Result<Arc<Self>, String> {
        let (region, custody) = new_region()?;
        let store = Shared::new(region);
        let pid = std::process::id();
        let birth_us = i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_process_birth)
            .map_or(0, |b| {
                b.seconds
                    .saturating_mul(1_000_000)
                    .saturating_add(b.microseconds)
            });
        let writer = JournalWriter::create(
            store.clone(),
            Geometry::v1(),
            &writer_identity(id, sid, birth_us),
        )
        .map_err(|e| format!("layout: {e}"))?;
        // Asked before the term lock: the answer takes the PTY registry's own lock.
        let resize_policy = crate::app_render::pty_resize_policy(master);
        let cfg_hash = config_hash(cfg);
        let epoch = Instant::now();
        let journal = Arc::new(Self {
            store,
            writer: Mutex::new(Writer {
                writer,
                policy: KeyframePolicy::new(aterm_restore::layout::TAIL_CAP_V1, 0, 0),
                mark: SpineMark::attach(Arc::new(SpineWatermark::default())),
                epoch,
                skew_ms: 0,
                resize_policy,
                config_fp: 0,
                slice_marks: 0,
                no_bridge: false,
                keyframes: 0,
                deferred: 0,
                hold: aterm_types::dev_seam!(HOLD_KEYFRAMES_SEAM)
                    .is_some_and(|v| !v.is_empty() && v != "0"),
                region_gen: 0,
                settled_clear_gen: 0,
            }),
            gathered: AtomicU64::new(0),
            cfg_hash: AtomicU64::new(cfg_hash),
            host_fp: AtomicU64::new(0),
            custody: Mutex::new(Some(JournalCustody {
                region: custody,
                len: u64::try_from(REGION_LEN).unwrap_or(u64::MAX),
                receipt: None,
            })),
            lane,
            id,
            tail: AtomicU64::new(0),
            broken: OnceLock::new(),
            ring: Mutex::new(Some(Ring::new(&Geometry::v1()))),
            exported: Mutex::new(None),
            ring_signal: AtomicU8::new(0),
            export_held: AtomicBool::new(false),
            seen_end: AtomicU64::new(0),
            seen_oldest: AtomicU64::new(0),
            renumbered: AtomicBool::new(false),
            worker: OnceLock::new(),
            ring_epoch: epoch,
            ring_skew_ms: AtomicU64::new(0),
            #[cfg(test)]
            ring_held_ms: OnceLock::new(),
            history_shed: AtomicBool::new(false),
            replace_owed: AtomicBool::new(false),
            custody_owed: AtomicBool::new(false),
            replace_parked: AtomicBool::new(false),
            regions_replaced: AtomicU64::new(0),
            #[cfg(test)]
            test_link: Mutex::new(None),
            sid: sid.to_string(),
            birth_us,
            ledger: OnceLock::new(),
            ledger_moved: AtomicBool::new(false),
            control: Mutex::new(Control::default()),
            turn_ids: Mutex::new(TurnCountCustody::default()),
            recovery_count: OnceLock::new(),
        });
        let captured = {
            let t = crate::term_lock(term);
            journal.note_seen(&t.history_fence());
            let cell_px = t.host_cell_pixel_size();
            let fp = config_fp(cfg_hash, cell_px);
            store_cell_px(&journal.store, cell_px).map_err(|e| format!("cell size: {e}"))?;
            store_config_fp(&journal.store, fp).map_err(|e| format!("config: {e}"))?;
            journal.host_fp.store(fp, Ordering::Release);
            let mut w = journal.lock();
            w.config_fp = fp;
            // A fresh region: nothing in it is from before the history's last clear.
            w.settled_clear_gen = t.history_fence().clear_gen;
            let now_ms = w.now_ms();
            w.policy = KeyframePolicy::new(Geometry::v1().tail_cap, now_ms, fp);
            // The keyframe holds the geometry as it stands: no resize before it rides
            // the tail.
            w.mark.seed(SpinePre::take(&t));
            w.capture(&t, Reason::Quiescence, None)
                .map_err(|e| format!("first keyframe: {e}"))?
        };
        if let Some(captured) = captured {
            journal.write(captured);
        }
        if let Some(why) = journal.broken.get() {
            return Err(why.clone());
        }
        // The process's turn-id count rides the region from here on (section "THE CONTROL
        // SLOT"), before anyone is told the region exists.
        enlist_turn_ids(&journal);
        Ok(journal)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Writer> {
        self.writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Why the journal stopped, if it did.
    pub(crate) fn broken(&self) -> Option<String> {
        self.broken.get().cloned()
    }

    /// Tail bytes past the newest keyframe ([`Self::tail`]).
    pub(crate) fn tail_bytes(&self) -> u64 {
        self.tail.load(Ordering::Relaxed)
    }

    /// Stop journaling for `why`: nothing more is written, and the keeper is asked to
    /// drop the region (COMPANION_RELEASE), so a region that stopped mid-way is never
    /// restored as if it were the session's last state. The gather thread's count goes
    /// on, so a restore of it before the release lands counts what it missed.
    fn stop(&self, why: String) {
        if self.broken.set(why.clone()).is_ok() {
            aterm_log::warn!(
                "crash restore: session {}'s journal stopped ({why}); its region is released \
                 and a crash now restores it blank",
                self.id
            );
            crate::keeper_link::release_companion(self.id);
        }
    }

    /// The custody descriptor and the region's length, for the session's registration
    /// (once).
    pub(crate) fn note_custody_registered(&self) {
        self.turn_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .registered = true;
    }

    #[cfg(test)]
    pub(crate) fn take_custody(&self) -> Option<(OwnedFd, u64)> {
        self.take_custody_with_receipt()
            .map(|held| (held.region, held.len))
    }

    pub(crate) fn bind_recovery_count(&self, accepted: Option<Arc<AtomicBool>>) {
        if let Some(accepted) = accepted {
            // The sole production caller binds once, before start_export/register.
            // A duplicate binding cannot grant either observer any authority.
            let _ = self.recovery_count.set(accepted);
        }
    }

    fn companion_receipt(&self) -> Arc<aterm_keeper::client::CompanionReceipt> {
        Arc::new(match self.recovery_count.get() {
            Some(accepted) => {
                aterm_keeper::client::CompanionReceipt::reporting_confirmation(Arc::clone(accepted))
            }
            None => aterm_keeper::client::CompanionReceipt::default(),
        })
    }

    pub(crate) fn take_custody_with_receipt(&self) -> Option<JournalCustody> {
        if self.broken.get().is_some() {
            return None;
        }
        let mut held = self
            .custody
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()?;
        if held.receipt.is_none() && self.recovery_count.get().is_some() {
            held.receipt = Some(self.companion_receipt());
        }
        Some(held)
    }

    // -- the gather thread ----------------------------------------------------

    /// `n` PTY bytes were read: store the running count before the batch is sent.
    pub(crate) fn gathered(&self, n: usize) {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        let total = self
            .gathered
            .fetch_add(n, Ordering::Relaxed)
            .saturating_add(n);
        let _ = store_gathered(&self.store, total);
    }

    // -- the host's paths -------------------------------------------------------

    /// The host resized the engine (the caller holds its lock, in the resize's hold):
    /// store the resize word, then nudge the parse thread to record the resize now,
    /// whether or not the program writes again.
    pub(crate) fn host_resized(&self, t: &Terminal) {
        let _ = store_resize_word(&self.store, t.resize_lifetime(), t.resize_ordinal());
        let _ = self.lane.nudge();
    }

    /// The host changed the engine's settings (the caller holds its lock, in the
    /// setter's hold): `cfg` when it applied a config, `None` for a cell-size change.
    /// Stores the cell size, then the fingerprint folding it in, and nudges the parse
    /// thread, which keyframes under them (K4). Nothing when nothing moved.
    pub(crate) fn host_settings(&self, t: &Terminal, cfg: Option<&TerminalConfig>) {
        if let Some(cfg) = cfg {
            self.cfg_hash
                .store(config_hash(Some(cfg)), Ordering::Relaxed);
        }
        let cell_px = t.host_cell_pixel_size();
        let fp = config_fp(self.cfg_hash.load(Ordering::Relaxed), cell_px);
        if fp == self.host_fp.load(Ordering::Acquire) {
            return;
        }
        let _ = store_cell_px(&self.store, cell_px);
        let _ = store_config_fp(&self.store, fp);
        self.host_fp.store(fp, Ordering::Release);
        let _ = self.lane.nudge();
    }

    // -- the parse thread -------------------------------------------------------

    /// Take what the host told the parse thread since its last turn: the newest config
    /// fingerprint (a change forces a keyframe, K4) and an import's renumbering (K4,
    /// `Reason::Renumbered`).
    fn sync(&self, w: &mut Writer) {
        w.sync_host(self.host_fp.load(Ordering::Acquire));
        if self.renumbered.swap(false, Ordering::AcqRel) {
            w.policy.note_renumbered();
        }
    }

    /// Read before a hold processes its bytes (the caller holds the term lock).
    pub(crate) fn pre(&self, t: &Terminal) -> SlicePre {
        let fence = t.history_fence();
        SlicePre {
            spine: SpinePre::take(t),
            marks: t.shell_marks_verified(),
            clear_gen: fence.clear_gen,
            hist_end: fence.end,
            recorded: false,
        }
    }

    /// Record `bytes` the hold just processed (the caller holds the term lock, the
    /// hold `pre` was read in): the verified marks they moved first, then every resize
    /// before them and the bytes; a clear among them makes the next keyframe forced and
    /// wakes the history ring's worker to zero the ring; history they pushed tells the
    /// worker the ring has lines to export.
    /// `synthesized`: a foreground handback's or a manual reset's bytes, replayed but
    /// not counted as PTY bytes.
    pub(crate) fn record(&self, t: &Terminal, pre: &mut SlicePre, bytes: &[u8], synthesized: bool) {
        if bytes.is_empty() || self.broken.get().is_some() {
            return;
        }
        let mut cleared = false;
        let mut grew = false;
        let failed = {
            let mut guard = self.lock();
            let w = &mut *guard;
            self.sync(w);
            let marks = t.shell_marks_verified();
            let delta = marks.saturating_sub(pre.marks);
            pre.marks = marks;
            w.slice_marks = w.slice_marks.saturating_add(delta);
            let now_ms = w.now_ms();
            let failed = match w.writer.note_marks(delta) {
                Err(e) => Some(e),
                Ok(()) => {
                    let Writer {
                        writer,
                        policy,
                        mark,
                        resize_policy,
                        ..
                    } = w;
                    let mut sink = TailSink {
                        writer,
                        policy,
                        resize_policy: *resize_policy,
                        now_ms,
                        synthesized,
                        failed: None,
                    };
                    if mark.record_to(&mut sink, pre.spine, t, bytes) {
                        None
                    } else {
                        Some(sink.failed.unwrap_or(WriteError::Sequence))
                    }
                }
            };
            if failed.is_none() {
                let fence = t.history_fence();
                if fence.clear_gen != pre.clear_gen {
                    // An ED3 or a RIS: its keyframe is forced at this hold's end, and
                    // after it the clear is settled out of the region.
                    pre.clear_gen = fence.clear_gen;
                    w.policy.note_clear();
                    cleared = true;
                }
                if fence.end != pre.hist_end {
                    pre.hist_end = fence.end;
                    grew = true;
                }
                if grew || cleared {
                    self.note_seen(&fence);
                }
                self.tail
                    .store(w.writer.tail_since_keyframe(), Ordering::Relaxed);
            }
            failed
        };
        if let Some(e) = failed {
            self.stop(format!("tail: {e}"));
            return;
        }
        // After the journal's lock, never inside it: the worker's lock is its own.
        if self.replace_parked.load(Ordering::Relaxed)
            && self.replace_parked.swap(false, Ordering::AcqRel)
        {
            // A shed region's replacement waited for bytes that could end the sequence
            // that deferred it (K5): these may have.
            self.notify_worker();
        }
        if cleared {
            // The ring still holds the lines the person cleared, and so does a superseded
            // region the keeper still holds: the worker zeroes the one and erases the other
            // at once rather than at its next tick.
            self.ring_signal.fetch_or(RING_CLEARED, Ordering::AcqRel);
            self.nudge_ring();
        } else if grew {
            self.ring_grew();
        }
        pre.recorded = true;
    }

    /// The end of a hold that recorded something (the caller still holds the term
    /// lock): the policy's decision, and the keyframe it calls for captured. Write it
    /// with [`Self::write`] once the lock is released.
    pub(crate) fn decide(&self, t: &Terminal, pre: &SlicePre) -> Option<Captured> {
        if !pre.recorded || self.broken.get().is_some() {
            return None;
        }
        let captured = {
            let mut guard = self.lock();
            let w = &mut *guard;
            let now_ms = w.now_ms();
            let marks = std::mem::take(&mut w.slice_marks);
            let decision = w
                .policy
                .after_slice(now_ms, w.writer.tail_since_keyframe(), marks);
            decision
                .keyframe
                .filter(|reason| w.admits(*reason))
                .map(|reason| w.capture(t, reason, self.ring_exported()))
        };
        self.captured(captured)
    }

    fn captured(&self, captured: Option<Result<Option<Captured>, WriteError>>) -> Option<Captured> {
        match captured {
            Some(Ok(captured)) => captured,
            Some(Err(e)) => {
                self.stop(format!("tail: {e}"));
                None
            }
            None => None,
        }
    }

    /// Write a captured keyframe (the term lock released): copy it into its slot and
    /// commit it; after a clear's keyframe, settle the clear out of the region. A
    /// keyframe too large for its slot is retried with no bridge at the next slice end or
    /// quiet wait (the plan's "giant screens" rule); one that does not fit even then
    /// stops the journal.
    pub(crate) fn write(&self, captured: Captured) {
        if self.broken.get().is_some() {
            return;
        }
        let failed = {
            let mut guard = self.lock();
            let w = &mut *guard;
            if w.region_gen != captured.region_gen {
                // The memory shed replaced the region between the capture and this write,
                // and keyframed the engine itself, later: this capture is stale there.
                return;
            }
            if w.writer.tail_commit() != captured.tail_at {
                // Never: only this thread appends, and it writes a capture first. Were
                // it to happen, the keyframe's `tail_from` would skip what came between.
                w.policy.keyframe_deferred(captured.reason);
                return;
            }
            let input = KeyframeInput {
                checkpoint: &captured.checkpoint,
                fence: captured.fence,
                resize_lifetime: captured.lifetime,
                resize_ordinal: captured.ordinal,
                bridge_lines: captured.bridge,
                config_fp: captured.config_fp,
                integration_on: captured.integration_on,
            };
            match w.writer.write_keyframe(&input) {
                Ok(_) => {
                    let now_ms = w.now_ms();
                    w.policy.keyframe_written(now_ms);
                    w.no_bridge = false;
                    w.keyframes = w.keyframes.saturating_add(1);
                    self.tail
                        .store(w.writer.tail_since_keyframe(), Ordering::Relaxed);
                    if captured.reason == Reason::Clear {
                        match w.writer.settle_clear() {
                            Ok(()) => {
                                w.settled_clear_gen = captured.fence.clear_gen;
                                None
                            }
                            Err(e) => Some(format!("clear: {e}")),
                        }
                    } else {
                        None
                    }
                }
                Err(KeyframeError::TooLarge { .. }) if captured.bridge > 0 => {
                    // Not the parser's to clear: the retry, with no bridge, can succeed on
                    // the next quiet with nothing new to process.
                    w.no_bridge = true;
                    w.policy.keyframe_retry(captured.reason);
                    None
                }
                Err(e) => Some(format!("keyframe: {e}")),
            }
        };
        if let Some(why) = failed {
            self.stop(why);
        }
    }

    /// Whether the session has tail past its keyframe that a quiet wait can keyframe
    /// (the parse loop then waits for its next batch with a [`QUIET`] timeout, and
    /// otherwise blocks): not while a K5 deferral parks it (module doc).
    pub(crate) fn dirty(&self) -> bool {
        self.broken.get().is_none() && {
            let w = self.lock();
            !w.hold && w.policy.dirty()
        }
    }

    /// The parse loop's wait timed out: a quiet keyframe (K1) or an owed one (K4, K5).
    pub(crate) fn on_quiet(&self, term: &Mutex<Terminal>) {
        if self.broken.get().is_some() {
            return;
        }
        let captured = {
            let t = crate::term_lock(term);
            let mut guard = self.lock();
            let w = &mut *guard;
            self.sync(w);
            let now_ms = w.now_ms();
            w.policy
                .on_quiet(now_ms)
                .keyframe
                .filter(|reason| w.admits(*reason))
                .map(|reason| w.capture(&t, reason, self.ring_exported()))
        };
        if let Some(captured) = self.captured(captured) {
            self.write_quiet(captured);
        }
    }

    /// A host path nudged the parse thread ([`Self::host_resized`],
    /// [`Self::host_settings`]): record the resizes now, take the new settings, and
    /// write a keyframe owed for them (K4).
    pub(crate) fn on_nudge(&self, term: &Mutex<Terminal>) {
        if self.broken.get().is_some() {
            return;
        }
        let captured = {
            let t = crate::term_lock(term);
            let mut guard = self.lock();
            let w = &mut *guard;
            self.sync(w);
            match w.catch_up(&t, SpinePre::take(&t)) {
                Err(e) => Some(Err(e)),
                Ok(()) => {
                    self.tail
                        .store(w.writer.tail_since_keyframe(), Ordering::Relaxed);
                    let now_ms = w.now_ms();
                    w.policy
                        .on_quiet(now_ms)
                        .keyframe
                        .filter(|reason| w.admits(*reason))
                        .map(|reason| w.capture(&t, reason, self.ring_exported()))
                }
            }
        };
        if let Some(captured) = self.captured(captured) {
            self.write_quiet(captured);
        }
    }

    /// Write a keyframe a quiet wait or a nudge captured, and when it is K1's (the burst
    /// ended), wake the history ring's worker: the ring catches up now, once, rather than
    /// at the worker's next tick (the plan's "once at quiescence"), so it meets this
    /// keyframe's bridge as soon as it can, and a flood's backlog is caught up at the
    /// normal pace from here.
    fn write_quiet(&self, captured: Captured) {
        let quiescence = captured.reason == Reason::Quiescence;
        self.write(captured);
        if quiescence {
            self.nudge_ring();
        }
    }

    /// Move the policy's clock, and the ring's, on by `ms` (a test reaches K1's quiet or
    /// the ring's settle without sleeping).
    #[cfg(test)]
    pub(crate) fn pass_ms(&self, ms: u64) {
        let mut w = self.lock();
        w.skew_ms = w.skew_ms.saturating_add(ms);
        self.ring_skew_ms.fetch_add(ms, Ordering::Relaxed);
    }

    /// Keyframes written and captures deferred, for a test's view of the policy.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (u64, u64) {
        let w = self.lock();
        (w.keyframes, w.deferred)
    }

    /// A copy of the region's bytes as they stand: what a restorer would map if this
    /// window died now.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> aterm_restore::VecStore {
        use aterm_restore::{RegionStore as _, RegionView as _};
        let len = self.store.len();
        let copy = aterm_restore::VecStore::new(len);
        let mut bytes = vec![0u8; len];
        self.store.read(0, &mut bytes).expect("the region reads");
        copy.write(0, &bytes).expect("the copy takes it");
        copy
    }
}

// ---------------------------------------------------------------------------
// The history ring: the journal worker (the plan's increment 5)
// ---------------------------------------------------------------------------

/// [`SessionJournal::ring_signal`]: the history grew since the worker's last tick.
#[cfg(unix)]
const RING_GREW: u8 = 1;
/// [`SessionJournal::ring_signal`]: export now (quiescence, a clear, the export's start
/// or its release after an import).
#[cfg(unix)]
const RING_NUDGED: u8 = 2;
/// [`SessionJournal::ring_signal`]: the parse thread recorded a clear (an ED3, a RIS), with
/// [`RING_NUDGED`]: a superseded region the keeper still holds is erased now
/// ([`SessionJournal::tend_custody`]). Taken by that erase's tick alone, so it never takes a
/// nudge the export is owed.
#[cfg(unix)]
const RING_CLEARED: u8 = 4;
/// The worker exports a growing history at most this often.
#[cfg(unix)]
const TICK_MS: u64 = 1_000;
/// …and a flooding one at most this often: a flood's lines are mostly evicted by the
/// engine before anyone could scroll to them, and exporting them would cost the worker
/// a ring's worth of LZ4 a second for lines the next second evicts. Quiescence (K1)
/// catches the export up.
#[cfg(unix)]
const FLOOD_TICK_MS: u64 = 5_000;
/// A reflow or a reveal renumbers the history, and the ring must start over under the
/// new identity: it waits for the identity to hold this long first, so a window drag
/// (sixty reflows a second) re-exports the history once, after the drag, not sixty
/// times. A restore meanwhile counts the history it cannot join.
#[cfg(unix)]
const SETTLE_MS: u64 = 2_000;
/// The most chunks one tick exports for one session: 32k lines, each chunk read under
/// its own brief term-lock hold, so a deep history is caught up over a few ticks and no
/// tick holds the worker on one session for long.
#[cfg(unix)]
const MAX_CHUNKS_PER_TICK: usize = 32;
/// A history that grew by more than one tick can export since the last tick floods.
#[cfg(unix)]
const FLOOD_LINES: u64 = (MAX_CHUNKS_PER_TICK * aterm_restore::records::CHUNK_LINES) as u64;

/// The sooner of two deadlines, either of which may be none.
#[cfg(unix)]
fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The export's state of one session's history ring, kept beside its journal
/// ([`SessionJournal::ring`]) and touched only by the thread that exports it.
#[cfg(unix)]
struct Ring {
    writer: HistoryWriter,
    /// The ring's physical capacity: no cap is larger.
    hist_cap_phys: usize,
    /// The identity a reflow or a reveal moved the history to, and since when (ms),
    /// while the export waits [`SETTLE_MS`] for it to hold.
    settling: Option<(HistoryIdentity, u64)>,
    /// When the last tick ran (ms on the ring's clock).
    last_tick: Option<u64>,
    /// The history's end at the last tick: how much it grew since (the flood measure).
    end_at_tick: u64,
    /// The last tick saw a flood: the next waits [`FLOOD_TICK_MS`].
    flooding: bool,
    /// When work the last tick left is due (a backlog, a settle, a reflow in flight);
    /// `None` when only a signal brings the next tick.
    next_due: Option<u64>,
    /// The corpus's encoded bytes per line, smoothed across ticks and resets (0 before any
    /// line): what the adaptive cap is sized by.
    bytes_per_line: u64,
    /// The ring is shed and the pages it touched are still the region's: when the region's
    /// replacement is next tried (ms on the ring's clock; module section "THE MEMORY
    /// SHED"). `None` when no replacement is owed.
    replace_due: Option<u64>,
    /// The fresh region (and its custody descriptor) a deferred replacement created and
    /// could not use yet, kept for its next try: a try the parser defers again then costs
    /// one brief term-lock hold, never another `shm_open`, `ftruncate` and `mmap` and their
    /// undoing. Never written, so it costs this window no resident page.
    spare: Option<(Touched<aterm_uds::shm::Region>, OwnedFd)>,
    /// A fresh region's descriptor a full queue handed back. Count-custody separately
    /// retains the region the keeper may still hold, including after queue admission.
    custody_owed: Option<OwedCustody>,
}

/// A replaced region's custody descriptor the keeper link's queue was too full to take
/// (`keeper_link::CompanionOffer::Busy`: a keeper slow to read, under the very pressure
/// being shed). Dropping it would leave the keeper holding the superseded region for good,
/// so every later crash would restore the stale screen of the shed, never the session's
/// own: it is offered again ([`SessionJournal::tend_custody`]).
#[cfg(unix)]
struct OwedCustody {
    custody: OwnedFd,
    due: u64,
}

#[cfg(unix)]
impl Ring {
    fn new(geometry: &Geometry) -> Self {
        Self {
            writer: HistoryWriter::new(geometry),
            hist_cap_phys: geometry.hist_cap_phys,
            settling: None,
            last_tick: None,
            end_at_tick: 0,
            flooding: false,
            next_due: None,
            bytes_per_line: 0,
            replace_due: None,
            spare: None,
            custody_owed: None,
        }
    }

    /// The ring of a fresh region (the shed's replacement): empty, under no identity, its
    /// words the fresh region's zeros, so its first export after the shed eases starts it
    /// over under the history's identity. The corpus's rate is kept: it is the program's,
    /// not the region's.
    fn fresh(&self, geometry: &Geometry) -> Self {
        Self {
            bytes_per_line: self.bytes_per_line,
            ..Self::new(geometry)
        }
    }

    /// The adaptive cap for a scrollback of `line_limit` lines at the corpus's rate
    /// ([`adaptive_hist_cap`], the plan's §2.6).
    fn cap_for(&self, line_limit: Option<usize>) -> usize {
        adaptive_hist_cap(line_limit, self.bytes_per_line, self.hist_cap_phys)
    }

    /// Fold the ring's current rate into the smoothed one (a quarter weight a tick).
    fn note_rate(&mut self) {
        if let Some(rate) = self.writer.bytes_per_line() {
            self.bytes_per_line = if self.bytes_per_line == 0 {
                rate
            } else {
                (self.bytes_per_line.saturating_mul(3).saturating_add(rate)) / 4
            };
        }
    }
}

/// What one chunk's read under the term lock found.
#[cfg(unix)]
enum ChunkRead {
    /// The history's identity moved, or a reflow holds its store: the next tick sees why.
    Moved,
    /// Nothing left to export.
    Done,
    /// The engine evicted lines the ring had not reached: the ring starts over at the
    /// engine's oldest row, so its rows stay a run with no hole.
    Hole(u64),
    /// The next chunk's lines.
    Lines(Vec<aterm_core::scrollback::Line>),
}

#[cfg(unix)]
impl SessionJournal {
    /// How far the ring has exported the history, and under which identity.
    fn ring_exported(&self) -> Option<(HistoryIdentity, u64)> {
        *self
            .exported
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn publish_ring_end(&self, exported: Option<(HistoryIdentity, u64)>) {
        *self
            .exported
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = exported;
    }

    /// History lines past where the ring has reached (`status journal_ring_lag=`): 0 once
    /// the export has caught the session's history up, the whole history while it waits
    /// for an import or a settle, or before its first tick. Counted from the history's
    /// oldest row, never from row 0: with nothing exported the lag is the history's lines,
    /// not its end row, which a shell past its line limit has long since left behind.
    pub(crate) fn ring_lag(&self) -> u64 {
        let reached = self
            .ring_exported()
            .map_or(0, |(_, end)| end)
            .max(self.seen_oldest.load(Ordering::Relaxed));
        self.seen_end
            .load(Ordering::Relaxed)
            .saturating_sub(reached)
    }

    /// The parse thread's view of the history's span, for [`Self::ring_lag`].
    fn note_seen(&self, fence: &HistoryFence) {
        self.seen_oldest.store(fence.oldest, Ordering::Relaxed);
        self.seen_end.store(fence.end, Ordering::Relaxed);
    }

    /// The ring's clock: milliseconds since the journal's creation (or where a test
    /// stopped it, [`Self::hold_ring_clock`]), plus a test's skew.
    fn ring_now_ms(&self) -> u64 {
        #[cfg(test)]
        if let Some(&held) = self.ring_held_ms.get() {
            return held.saturating_add(self.ring_skew_ms.load(Ordering::Relaxed));
        }
        self.ring_elapsed_ms()
            .saturating_add(self.ring_skew_ms.load(Ordering::Relaxed))
    }

    /// Milliseconds since the journal's creation.
    fn ring_elapsed_ms(&self) -> u64 {
        u64::try_from(self.ring_epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Stop the ring's clock where it reads now: from here only [`Self::pass_ms`] moves
    /// it, so the stamps a test reads against it ([`Self::ring_due`], the custody's and
    /// the control slot's dues) do not depend on how long a loaded machine took between
    /// two of the test's steps. For a test that runs the ring's ticks itself: a worker
    /// thread waits on the clock moving.
    #[cfg(test)]
    pub(crate) fn hold_ring_clock(&self) {
        let _ = self.ring_held_ms.set(self.ring_elapsed_ms());
    }

    fn notify_worker(&self) {
        if let Some(worker) = self.worker.get() {
            worker.wake_ring_worker();
        }
    }

    /// The parse thread pushed history: tell the worker, once per tick (the first growth
    /// since its last tick wakes it; the rest only find the bit set).
    fn ring_grew(&self) {
        if self.export_held.load(Ordering::Acquire) {
            return;
        }
        if self.ring_signal.fetch_or(RING_GREW, Ordering::AcqRel) & RING_GREW == 0 {
            self.notify_worker();
        }
    }

    /// Export now ([`RING_NUDGED`]).
    fn nudge_ring(&self) {
        self.ring_signal.fetch_or(RING_NUDGED, Ordering::AcqRel);
        self.notify_worker();
    }

    /// Hand the session's history ring to the window's journal worker
    /// ([`HistoryWorker::ring_worker`]), its export held for an import when `held` (an adopted
    /// session: [`Self::import_settled`] opens it). A window whose worker could not start
    /// journals screens with no ring, and its restores count the history they lose.
    pub(crate) fn start_export(self: &Arc<Self>, term: &Arc<Mutex<Terminal>>, held: bool) {
        if let Some(worker) = HistoryWorker::ring_worker() {
            self.start_export_with(&worker, term, held);
        }
    }

    /// [`Self::start_export`] on `worker`.
    pub(crate) fn start_export_with(
        self: &Arc<Self>,
        worker: &Arc<HistoryWorker>,
        term: &Arc<Mutex<Terminal>>,
        held: bool,
    ) {
        self.export_held.store(held, Ordering::Release);
        if self.worker.set(Arc::clone(worker)).is_ok() {
            worker.enlist_ring(Arc::downgrade(self), Arc::downgrade(term));
        }
        if !held {
            self.nudge_ring();
        } else if self.ledger_moved.load(Ordering::Acquire) {
            // A record that landed before the worker had the session told nobody.
            self.notify_worker();
        }
    }

    /// The session's history import settled (`handoff_history::run_imports`, on its
    /// worker; the main thread's `Wake::HandoffHistorySettled`): its older history is now
    /// below the carried lines, under the renumbering the attach made. The export starts
    /// (the ring takes the whole history, imported lines included), and the parse thread
    /// keyframes the renumbered history (`Reason::Renumbered`) with no output to wait for,
    /// so the newest keyframe and the ring share an identity and a crash of THIS window
    /// joins them: an idle recovered shell crashing again keeps its scrollback.
    pub(crate) fn import_settled(&self) {
        self.export_held.store(false, Ordering::Release);
        self.renumbered.store(true, Ordering::Release);
        let _ = self.lane.nudge();
        self.nudge_ring();
    }

    /// When the worker should next export this session's ring, on its clock: now for a
    /// nudge, a tick after the last for a growth (five while it floods), or what the last
    /// tick left due; `None` for nothing to do (an idle session costs the worker nothing),
    /// a held export or a stopped journal. A shed ring exports nothing: it is due only for
    /// the region replacement it still owes, at once for a clear (the ring's lines are
    /// zeroed in the region they still live in), never while the replacement waits for the
    /// program's next bytes (module section "THE MEMORY SHED"). A replaced region's custody
    /// the keeper link could not take yet is due again on its own clock, shed or not, and at
    /// once for a clear while the region it superseded is still to be erased.
    pub(crate) fn ring_due(&self, now_ms: u64) -> Option<u64> {
        if self.broken.get().is_some() {
            return None;
        }
        let signal = self.ring_signal.load(Ordering::Acquire);
        let ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ring = ring.as_ref()?;
        let queued = ring.custody_owed.as_ref().map(|owed| owed.due);
        let retired = self
            .turn_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retired
            .as_ref()
            .map(|old| {
                if old.receipt.confirmed() || (!old.erased && signal & RING_CLEARED != 0) {
                    now_ms
                } else {
                    old.due
                }
            });
        let custody = earliest(queued, retired);
        let export = if self.history_shed.load(Ordering::Acquire) {
            ring.replace_due.and_then(|due| {
                if signal & RING_NUDGED != 0 {
                    Some(now_ms)
                } else if self.replace_parked.load(Ordering::Acquire) {
                    None
                } else {
                    Some(due)
                }
            })
        } else if self.export_held.load(Ordering::Acquire) {
            None
        } else if signal & RING_NUDGED != 0 {
            Some(now_ms)
        } else {
            let interval = if ring.flooding {
                FLOOD_TICK_MS
            } else {
                TICK_MS
            };
            let grew = (signal & RING_GREW != 0).then(|| {
                ring.last_tick
                    .map_or(now_ms, |at| at.saturating_add(interval))
            });
            earliest(grew, ring.next_due)
        };
        earliest(export, custody)
    }

    /// One export tick of this session's history ring (the plan's increment 5, step 1):
    ///
    /// 1. Read the history's fence and line limit under a brief term lock.
    /// 2. If the fence's identity moved: start the ring over under it — at once on first
    ///    sight and for a clear (zeroing every byte the ring held: the privacy claim), only
    ///    once it has held [`SETTLE_MS`] for a reflow or a reveal.
    /// 3. Grow the cap when the ring is evicting lines the engine still holds and the
    ///    corpus's rate asks for more (an adaptive cap that only grows: shared pages a
    ///    smaller ring would stop touching stay resident anyway).
    /// 4. Export `[max(end row, oldest), end)` in chunks of at most 1024 lines, each read
    ///    under its own term-lock hold and encoded (LZ4, the step record) and copied with
    ///    no term lock held, at most [`MAX_CHUNKS_PER_TICK`] chunks.
    /// 5. Publish how far it reached, for the keyframes' bridge.
    ///
    /// The ring is taken out of its slot for the tick, so no lock of it is held while the
    /// term lock is. A store the region refuses stops the journal, as a tail store does.
    ///
    /// A shed ring's tick exports nothing: it zeroes the ring for a clear and replaces the
    /// region while a replacement is owed ([`Self::shed_tick`]). Any tick first tends a
    /// replaced region's custody the keeper link could not take before
    /// ([`Self::tend_custody`]).
    pub(crate) fn export_tick(&self, term: &Mutex<Terminal>) {
        if self.broken.get().is_some() {
            return;
        }
        let shed = self.history_shed.load(Ordering::Acquire);
        let Some(mut ring) = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let now = self.ring_now_ms();
        let outcome = self.tend_custody(&mut ring, term, now).and_then(|()| {
            if shed {
                self.shed_tick(&mut ring, term, now)
            } else if self.export_held.load(Ordering::Acquire) {
                Ok(())
            } else {
                let nudged = self.ring_signal.swap(0, Ordering::AcqRel) & RING_NUDGED != 0;
                let outcome = self.ring_tick(&mut ring, term, now, nudged);
                ring.last_tick = Some(now);
                ring.note_rate();
                outcome
            }
        });
        *self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ring);
        if let Err(why) = outcome {
            self.stop(why);
        }
    }

    fn ring_tick(
        &self,
        ring: &mut Ring,
        term: &Mutex<Terminal>,
        now: u64,
        nudged: bool,
    ) -> Result<(), String> {
        let (fence, limit) = {
            let t = crate::term_lock(term);
            (t.history_fence(), t.scrollback_line_limit())
        };
        let grown = fence.end.saturating_sub(ring.end_at_tick);
        // A nudge is quiescence, a clear or a start: never a flood, whatever came before.
        ring.flooding = !nudged && ring.last_tick.is_some() && grown > FLOOD_LINES;
        ring.end_at_tick = fence.end;
        ring.next_due = None;
        let identity = fence_identity(&fence);
        let current = ring.writer.identity();
        if fence.detached {
            // A reflow holds the store, so no line can be read; but a clear made meanwhile
            // still empties and zeroes the ring at once (the privacy claim, plan §5 claim
            // 4): `erase_scrollback` bumps the clear generation even while the store is
            // out, and a window drag can keep it out for as long as the drag lasts. The
            // ring starts over under the detached fence's identity; the attached fence
            // after the reflow renumbers it again (the settle), and nothing is published
            // to bridge against meanwhile.
            if current.cols != 0 && identity.clear_gen != current.clear_gen {
                let cap = ring.writer.cap();
                ring.writer
                    .reset_ring(&self.store, identity, fence.oldest, cap, true)
                    .map_err(|e| format!("history: {e}"))?;
                self.publish_ring_end(None);
            }
            ring.next_due = Some(now.saturating_add(TICK_MS));
            return Ok(());
        }
        if identity == current {
            ring.settling = None;
        } else {
            let first = current.cols == 0;
            let cleared = !first && identity.clear_gen != current.clear_gen;
            if !first && !cleared {
                match ring.settling {
                    Some((settling, since))
                        if settling == identity && now.saturating_sub(since) >= SETTLE_MS => {}
                    Some((settling, since)) if settling == identity => {
                        ring.next_due = Some(since.saturating_add(SETTLE_MS));
                        return Ok(());
                    }
                    _ => {
                        ring.settling = Some((identity, now));
                        ring.next_due = Some(now.saturating_add(SETTLE_MS));
                        // The ring names rows the history no longer numbers that way.
                        self.publish_ring_end(None);
                        return Ok(());
                    }
                }
            }
            ring.settling = None;
            let cap = ring.cap_for(limit);
            ring.writer
                .reset_ring(&self.store, identity, fence.oldest, cap, cleared)
                .map_err(|e| format!("history: {e}"))?;
            self.publish_ring_end(Some((identity, fence.oldest)));
        }
        let want = ring.cap_for(limit);
        if want > ring.writer.cap()
            && ring
                .writer
                .first_row()
                .is_some_and(|first| first > fence.oldest)
        {
            let cap = want
                .max(ring.writer.cap().saturating_mul(2))
                .min(ring.hist_cap_phys);
            ring.writer
                .reset_ring(&self.store, identity, fence.oldest, cap, false)
                .map_err(|e| format!("history: {e}"))?;
            self.publish_ring_end(Some((identity, fence.oldest)));
        }

        let mut steps = 0;
        let mut caught_up = false;
        while steps < MAX_CHUNKS_PER_TICK {
            steps += 1;
            let from = ring.writer.end_row();
            let read = {
                let t = crate::term_lock(term);
                let f = t.history_fence();
                if f.detached || fence_identity(&f) != identity {
                    ChunkRead::Moved
                } else if from < f.oldest {
                    ChunkRead::Hole(f.oldest)
                } else if from >= f.end {
                    ChunkRead::Done
                } else {
                    match t.history_lines_since_fence(&f, from, aterm_restore::records::CHUNK_LINES)
                    {
                        Ok(lines) => ChunkRead::Lines(lines),
                        Err(aterm_core::grid::HistoryFenceBroken::Evicted) => {
                            ChunkRead::Hole(t.history_fence().oldest)
                        }
                        Err(aterm_core::grid::HistoryFenceBroken::Moved) => ChunkRead::Moved,
                    }
                }
            };
            match read {
                ChunkRead::Moved => {
                    ring.next_due = Some(now.saturating_add(TICK_MS));
                    break;
                }
                ChunkRead::Done => {
                    caught_up = true;
                    break;
                }
                ChunkRead::Hole(oldest) => {
                    let cap = ring.writer.cap();
                    ring.writer
                        .reset_ring(&self.store, identity, oldest, cap, false)
                        .map_err(|e| format!("history: {e}"))?;
                }
                ChunkRead::Lines(mut lines) => {
                    // As an update's export does: a line over the record cap because of
                    // its links is kept with its links dropped, never refused at import.
                    crate::handoff_history::strip_over_cap_links(&mut lines, identity.cols);
                    self.append_split(&mut ring.writer, from, &lines)?;
                }
            }
        }
        if !caught_up && ring.next_due.is_none() {
            let interval = if ring.flooding {
                FLOOD_TICK_MS
            } else {
                TICK_MS
            };
            ring.next_due = Some(now.saturating_add(interval));
        }
        self.publish_ring_end(Some((identity, ring.writer.end_row())));
        Ok(())
    }

    /// Append `lines` (the first at row `from`) as one chunk, or as halves when the chunk
    /// is larger than the ring (a wide, heavily attributed screen's lines can be). A
    /// single line larger than the whole ring starts the ring over past it: the lines
    /// before it are lost, and the restore's join counts them.
    fn append_split(
        &self,
        writer: &mut HistoryWriter,
        from: u64,
        lines: &[aterm_core::scrollback::Line],
    ) -> Result<(), String> {
        match writer.append_chunk(&self.store, from, lines) {
            Ok(_) => Ok(()),
            Err(WriteError::TooLarge { .. }) if lines.len() > 1 => {
                let (older, newer) = lines.split_at(lines.len() / 2);
                self.append_split(writer, from, older)?;
                self.append_split(writer, from.saturating_add(older.len() as u64), newer)
            }
            Err(WriteError::TooLarge { .. }) => {
                let (identity, cap) = (writer.identity(), writer.cap());
                writer
                    .reset_ring(&self.store, identity, from.saturating_add(1), cap, false)
                    .map_err(|e| format!("history: {e}"))
            }
            Err(e) => Err(format!("history: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// The control slot (the plan's increment 7)
// ---------------------------------------------------------------------------

/// The journal worker carries a session's turn ledger into its region at most this often
/// (ms), and only when the ledger took a record since its last carry.
#[cfg(unix)]
const CONTROL_MS: u64 = 2_000;

/// The control slot's writer and what it last carried ([`SessionJournal::control`]).
#[cfg(unix)]
#[derive(Default)]
struct Control {
    writer: aterm_restore::ControlWriter,
    /// The ledger's generation the slot holds a carry of; `None` before the first.
    generation: Option<u64>,
    /// How many of that ledger's records, its oldest, the carry left out to fit the slot
    /// (`handoff_carry::CrashLedger::encode`): records the slot does not hold either.
    shed: u64,
    /// When the slot was last written (ms on the ring's clock): the next carry waits
    /// [`CONTROL_MS`] from it.
    at: Option<u64>,
}

/// Every live journal, whose region holds the process's turn-id count
/// ([`note_turn_ids`]): enlisted at its creation ([`enlist_turn_ids`]), gone with it.
#[cfg(unix)]
static TURN_ID_HOLDERS: Mutex<Vec<std::sync::Weak<SessionJournal>>> = Mutex::new(Vec::new());

/// Enlist `journal` with the journals that hold the turn-id count, then store the count
/// as it stands: enlisted first, so an id minted meanwhile is either stored by its mint
/// ([`note_turn_ids`] finds the journal) or read here (the mint ran before the enlistment,
/// under the same lock). Once per journal, at its creation, before the keeper is told the
/// region exists.
#[cfg(unix)]
fn enlist_turn_ids(journal: &Arc<SessionJournal>) {
    {
        let mut holders = TURN_ID_HOLDERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        holders.retain(|held| held.strong_count() > 0);
        holders.push(Arc::downgrade(journal));
    }
    journal.hold_turn_ids(crate::control::turn_ids_minted());
}

/// THE TURN-ID COUNT RIDES EVERY REGION (the plan's increment 7): the process's turn-id
/// count moved (`control::mint_turn_id`, `control::raise_turn_ids`), so every live
/// journal's region is given it (its `ctl_count` word, which no control write touches).
/// The mint calls this before the id it minted is anyone's but its own thread's (before the
/// ledger lock it mints under lets go, before the lease or a reply names it): a crash
/// restore then continues the count past every id this process ever showed anyone
/// ([`continue_carried_turns`]), whether the dead window's last carry was two seconds or an
/// hour old, the id minted in a tab that has since closed or in one whose journal stopped,
/// or the slot torn by a death inside its write. Only a crash that leaves no journaled
/// session leaves no count. A region the memory shed superseded, which the keeper still
/// holds while its replacement's custody is owed (retried every second), keeps the count
/// as of the swap: the recovery's reserve (`control::TURN_ID_RESERVE`) goes on past it. A
/// lock and a store per live journal, per turn.
#[cfg(unix)]
pub(crate) fn note_turn_ids() {
    let journals: Vec<Arc<SessionJournal>> = TURN_ID_HOLDERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(std::sync::Weak::upgrade)
        .collect();
    if journals.is_empty() {
        return;
    }
    let count = crate::control::turn_ids_minted();
    for journal in journals {
        journal.hold_turn_ids(count);
    }
}

#[cfg(unix)]
impl SessionJournal {
    /// Store `count` into the region's `ctl_count` word when it is above the count the word
    /// holds ([`Self::turn_ids`], held across the store): the word only rises, whichever
    /// thread stores and in whatever order the threads that read the count get here. A
    /// stopped journal stores nothing (its region is the keeper's to drop); a store the
    /// region refuses is left, as the gather thread's is.
    pub(crate) fn hold_turn_ids(&self, count: u64) {
        if self.broken.get().is_some() {
            return;
        }
        let mut held = self
            .turn_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let retiring = held.retired.is_some();
        held.retire_confirmed();
        if retiring && held.retired.is_none() {
            self.custody_owed.store(false, Ordering::Release);
        }
        if count > held.count
            && store_ctl_count(&self.store, count).is_ok()
            && held
                .retired
                .as_ref()
                .is_none_or(|old| store_ctl_count(&old.store, count).is_ok())
        {
            held.count = count;
        }
    }

    /// The turn-id count the region's `ctl_count` word holds ([`Self::hold_turn_ids`]).
    #[cfg(test)]
    pub(crate) fn turn_ids_held(&self) -> u64 {
        self.turn_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .count
    }
}

#[cfg(unix)]
impl SessionJournal {
    /// CARRY THE SESSION'S TURN LEDGER into its region's control slot (the plan's increment
    /// 7), so a crash restore brings back the turns driven into the session
    /// (`handoff_carry::CrashLedger`): watch `turns`, whose every record wakes the journal
    /// worker ([`Self::ledger_moved`]), and when it already carries anything (an adopted
    /// session's carried ledger, or the floor of one that could not be carried) write it
    /// now, on the spawning thread, before the session's registration hands the keeper the
    /// region: a crash right after an update or a recovery then still carries it. Once per
    /// journal; at its spawn, before the export starts.
    pub(crate) fn carry_ledger(
        self: &Arc<Self>,
        turns: &Arc<Mutex<crate::turn_ledger::TurnLedger>>,
    ) {
        if self.ledger.set(Arc::downgrade(turns)).is_err() {
            return;
        }
        let journal = Arc::downgrade(self);
        let first = {
            let mut ledger = turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ledger.watch(Arc::new(move || {
                if let Some(journal) = journal.upgrade() {
                    journal.ledger_moved();
                }
            }));
            ledger.carries_anything().then(|| {
                (
                    ledger.generation(),
                    crate::handoff_carry::CrashLedger::of(
                        &ledger,
                        crate::control::turn_ids_minted(),
                    ),
                )
            })
        };
        if let Some((generation, carry)) = first {
            self.write_control(generation, &carry);
        }
    }

    /// The ledger took a record (its watch, under its lock): tell the worker, once per
    /// carry (the first record since the last carry wakes it; the rest find the flag set).
    /// Takes no lock but the worker's queue, a leaf.
    fn ledger_moved(&self) {
        if !self.ledger_moved.swap(true, Ordering::AcqRel) {
            self.notify_worker();
        }
    }

    /// When the worker should next carry the ledger, on the ring's clock: at once after a
    /// record, or [`CONTROL_MS`] after the last carry; `None` while no record landed since
    /// (an idle session costs the worker nothing) or once the journal stopped.
    pub(crate) fn control_due(&self, now_ms: u64) -> Option<u64> {
        if self.broken.get().is_some() || !self.ledger_moved.load(Ordering::Acquire) {
            return None;
        }
        let at = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .at;
        Some(at.map_or(now_ms, |at| at.saturating_add(CONTROL_MS)))
    }

    /// One carry of the ledger (the journal worker, once [`Self::control_due`]): taken under
    /// the ledger's lock when it moved since the slot's carry (a clone of its records),
    /// encoded with no lock held, and written into the slot. A record landing meanwhile
    /// sets the flag again, so it is carried at the next turn.
    pub(crate) fn control_tick(&self) {
        if self.broken.get().is_some() || !self.ledger_moved.swap(false, Ordering::AcqRel) {
            return;
        }
        let Some(turns) = self.ledger.get().and_then(std::sync::Weak::upgrade) else {
            return;
        };
        let carried = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation;
        let taken = {
            let ledger = turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (Some(ledger.generation()) != carried).then(|| {
                (
                    ledger.generation(),
                    crate::handoff_carry::CrashLedger::of(
                        &ledger,
                        crate::control::turn_ids_minted(),
                    ),
                )
            })
        };
        if let Some((generation, carry)) = taken {
            self.write_control(generation, &carry);
        }
    }

    /// Write `carry` (the ledger at `generation`) into the control slot, its oldest records
    /// shed until its record fits the slot (and counted, [`Self::ledger_lag`]). A store the
    /// region refuses stops the journal, as a tail store's refusal does.
    fn write_control(&self, generation: u64, carry: &crate::handoff_carry::CrashLedger) {
        let geometry = Geometry::v1();
        let cap = geometry.ctl_cap();
        let encoded = carry.encode(|json| {
            aterm_restore::records::encode_control(1, json).is_ok_and(|record| record.len() <= cap)
        });
        let now = self.ring_now_ms();
        let mut control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        control.at = Some(now);
        let Some((bytes, shed)) = encoded else {
            aterm_log::warn!(
                "crash restore: session {}'s turn ledger does not fit its control slot even \
                 with every record shed; a crash restores it with the ledger it last carried",
                self.id
            );
            return;
        };
        match control.writer.write(&self.store, &geometry, &bytes) {
            Ok(_) => {
                control.generation = Some(generation);
                control.shed = u64::try_from(shed).unwrap_or(u64::MAX);
            }
            Err(e) => {
                drop(control);
                self.stop(format!("control: {e}"));
            }
        }
    }

    /// `status journal_ledger_lag=`: the session's turn records its region's control slot
    /// does not hold: those the ledger took since the slot's last carry (each carried at most
    /// two seconds after it lands), and the oldest ones that carry left out because the whole
    /// ledger did not fit the slot (they stay out while the ledger is that long: a few
    /// hundred records of long prompts fill it). 0 only once the slot holds every record the
    /// ledger holds: a crash now restores them all. An eviction past the ledger's cap since
    /// the last carry can count one record twice until the next.
    pub(crate) fn ledger_lag(&self) -> u64 {
        let Some(turns) = self.ledger.get().and_then(std::sync::Weak::upgrade) else {
            return 0;
        };
        let (carried, shed) = {
            let control = self
                .control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (control.generation, control.shed)
        };
        let generation = turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation();
        generation
            .wrapping_sub(carried.unwrap_or(0))
            .saturating_add(shed)
    }

    /// Give the fresh region the shed swapped in ([`Self::replace_region`], before its
    /// custody goes to the keeper) the control slot's record of `old`, the region it
    /// replaces. The independent count-custody lock already seeded the fresh count and
    /// retains any predecessor the keeper can still offer; carrying this slot writes no count.
    fn carry_control_into(&self, old: &Store) -> Result<(), String> {
        self.control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .writer
            .carry_into(old, &self.store, &Geometry::v1())
            .map_err(|e| format!("control: {e}"))?;
        Ok(())
    }
}

/// One descriptor and the read receipt belonging to that exact descriptor, including
/// a replacement racing its session's initial registration.
#[cfg(unix)]
pub(crate) struct JournalCustody {
    pub(crate) region: OwnedFd,
    pub(crate) len: u64,
    pub(crate) receipt: Option<Arc<aterm_keeper::client::CompanionReceipt>>,
}

/// Count stores that a keeper may still offer, independent of the ring. Mint takes
/// only count -> store. Replacement acquires term -> writer -> count, drops term and
/// writer, then copies under count -> control -> store; control never owns a reverse
/// count/ledger/term/writer edge. Initial registration uses keeper -> count and releases
/// count before taking custody; handoff releases custody before taking keeper.
#[cfg(unix)]
#[derive(Default)]
struct TurnCountCustody {
    count: u64,
    registered: bool,
    retired: Option<RetiredCount>,
}

#[cfg(unix)]
struct RetiredCount {
    store: Store,
    receipt: Arc<aterm_keeper::client::CompanionReceipt>,
    handed: bool,
    clear_gen: u64,
    erased: bool,
    due: u64,
}

#[cfg(unix)]
impl TurnCountCustody {
    fn retire_confirmed(&mut self) {
        if self
            .retired
            .as_ref()
            .is_some_and(|old| old.receipt.confirmed())
        {
            self.retired = None;
        }
    }

    fn waiting_for_read(&mut self) -> bool {
        self.retire_confirmed();
        self.retired.as_ref().is_some_and(|old| old.handed)
    }

    fn receipt(&self) -> Option<Arc<aterm_keeper::client::CompanionReceipt>> {
        self.retired.as_ref().map(|old| Arc::clone(&old.receipt))
    }
}

// ---------------------------------------------------------------------------
// The memory shed (the plan's increment 6)
// ---------------------------------------------------------------------------

/// A shed region replacement that failed is tried again this long after (ms): a region
/// could not be created (descriptors or shared memory exhausted under the very pressure
/// being shed), which no quick retry would cure.
#[cfg(unix)]
const REPLACE_RETRY_MS: u64 = 10_000;

/// Swap the shared region with its gather count seeded before another primitive store
/// can reach it. The seed's store-error policy is the replacement caller's: a valid
/// fixed-size writable region accepts this header word; no new error channel is added.
#[cfg(unix)]
fn swap_gathered_region<S: aterm_restore::RegionStore>(
    store: &Shared<S>,
    region: S,
    gathered: &AtomicU64,
) -> Result<S, S> {
    store.swap_in(region, |fresh| {
        let gathered = gathered.load(Ordering::Acquire);
        let _ = store_gathered(fresh, gathered);
    })
}

/// Lay out the fresh region `store` now holds for a shed session ([`SessionJournal::
/// replace_region`], under the session's term and journal locks): the v1 header naming
/// `identity`, the stream count continued from `processed` (the old region's: the gather
/// thread's running count, which the swap seeded, runs from the session's first byte, so a
/// count from 0 would read the session's whole output as in flight), and the history ring
/// shed (a cap of 0). Every store goes to the region's own leaf lock.
///
/// A function of its own, named apart from the session's `create`, which takes the term
/// lock: the lock census identifies a callee by its name, and a `create` called under the
/// term lock would read as the term lock taken twice.
///
/// # Errors
///
/// A store the region refused.
#[cfg(unix)]
fn lay_out_shed_region(
    store: &Store,
    identity: &WriterIdentity<'_>,
    processed: u64,
) -> Result<JournalWriter<Store>, String> {
    let mut writer = JournalWriter::create(store.clone(), Geometry::v1(), identity)
        .map_err(|e| format!("layout: {e}"))?;
    writer
        .resume_stream(processed)
        .map_err(|e| format!("stream: {e}"))?;
    writer.shed_history().map_err(|e| format!("history: {e}"))?;
    Ok(writer)
}

/// What one try at replacing a shed session's region came to.
#[cfg(unix)]
enum Replaced {
    /// The session journals into a fresh region whose ring is shed; the old one is
    /// superseded, unmapped, and the keeper told to close it.
    Done,
    /// The parser holds a sequence no keyframe carries whole (K5): nothing changed; tried
    /// again once the program's next bytes are recorded, and no sooner than K1's quiet.
    Deferred,
    /// No region could be created: nothing changed, tried again after
    /// [`REPLACE_RETRY_MS`].
    Unavailable(String),
}

#[cfg(unix)]
impl SessionJournal {
    /// Whether the session's history ring is shed (`status journal_ring=shed`, or
    /// `shedding` while the shed's old region is still in memory, [`Self::ring_shedding`]).
    pub(crate) fn ring_shed(&self) -> bool {
        self.history_shed.load(Ordering::Acquire)
    }

    /// Whether the shed's old region is still in memory (`status journal_ring=shedding`):
    /// the shed ring's region still waits to be replaced, its pages still this window's, or
    /// it was replaced but the keeper link has not taken the fresh region's custody yet, so
    /// the keeper still holds the old one and this window keeps it mapped ([`OwedCustody`]),
    /// whether or not the pressure has eased since.
    pub(crate) fn ring_shedding(&self) -> bool {
        (self.ring_shed() && self.replace_owed.load(Ordering::Acquire))
            || self.custody_owed.load(Ordering::Acquire)
    }

    /// `status journal_bytes=`: the bytes of every page this session's region has written,
    /// what it keeps resident in this window.
    pub(crate) fn resident_bytes(&self) -> usize {
        self.store.with(Touched::touched_bytes)
    }

    /// Written pages intersecting the history ring, including pages a privacy clear
    /// zeroed: those pages stay accounted to this region until it is unmapped. The
    /// shed policy ranks and replaces by this count, not the logical ring's contents.
    pub(crate) fn ring_bytes(&self) -> usize {
        let geometry = Geometry::v1();
        self.store
            .with(|region| region.touched_bytes_in(geometry.hist_off, geometry.hist_cap_phys))
    }

    /// Regions the shed replaced.
    #[cfg(test)]
    pub(crate) fn regions_replaced(&self) -> u64 {
        self.regions_replaced.load(Ordering::Relaxed)
    }

    /// SHED the history ring (the journal worker, on [`ShedPolicy`]'s word): nothing more
    /// is exported, and from the parse thread's next keyframe on every keyframe carries its
    /// own bridge of history (up to 512 lines, as a region with no ring does), since there
    /// is no ring to meet. When the ring has written any page of the region
    /// ([`Self::ring_bytes`]), the region is replaced at the worker's next tick of this
    /// session ([`Self::replace_region`]): those pages stay resident for as long as the
    /// region lives, zeroed by a clear or not. A ring that wrote none (an idle shell, an
    /// export held for its import, a region the shed already replaced) is shed in place, its
    /// region already costing nothing for it.
    pub(crate) fn request_shed(&self) {
        if self.history_shed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.publish_ring_end(None);
        let now = self.ring_now_ms();
        let touched = self.ring_bytes() > 0;
        if let Some(ring) = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            && touched
        {
            ring.replace_due = Some(now);
            self.replace_owed.store(true, Ordering::Release);
        }
        self.notify_worker();
    }

    /// The pressure has eased (the worker, on [`ShedPolicy`]'s word): the ring is exported
    /// again from its next tick. A replaced region's ring starts fresh and takes the whole
    /// history under its identity; an in-place shed left the ring as it was, and the tick's
    /// own rules (a moved identity's settle, an eviction's hole) start it over where the
    /// history moved meanwhile.
    pub(crate) fn request_unshed(&self) {
        if !self.history_shed.swap(false, Ordering::AcqRel) {
            return;
        }
        let spare = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .and_then(|ring| {
                ring.replace_due = None;
                ring.spare.take()
            });
        self.replace_owed.store(false, Ordering::Release);
        self.replace_parked.store(false, Ordering::Release);
        // Unmapped with no lock held.
        drop(spare);
        self.nudge_ring();
    }

    /// A shed ring's tick, while its region's replacement is owed:
    ///
    /// 1. A clear made since the shed (an ED3, a RIS) empties and zeroes the ring in the
    ///    region it still lives in, at once, as an exporting ring's tick does (the privacy
    ///    claim, plan §5 claim 4). The replacement would take the ring's lines out of this
    ///    window, but it can wait for as long as the program sits mid-sequence (K5) or no
    ///    region can be created, and the keeper holds the old region until its COMPANION
    ///    lands: the lines the person cleared must not stay in it meanwhile. (Once the
    ///    replacement has landed, a COMPANION the keeper link could not queue yet keeps the
    ///    old region for a clear to erase: [`SessionJournal::tend_custody`].)
    /// 2. The region is replaced when the replacement is due, unless it waits for the
    ///    program's next bytes ([`SessionJournal::replace_parked`]).
    ///
    /// The tick takes the session's signal (the nudge that brought it, for a clear), so a
    /// shed ring is not due again until the next one.
    fn shed_tick(&self, ring: &mut Ring, term: &Mutex<Terminal>, now: u64) -> Result<(), String> {
        self.ring_signal.store(0, Ordering::Release);
        let Some(due) = ring.replace_due else {
            return Ok(());
        };
        let fence = crate::term_lock(term).history_fence();
        let identity = fence_identity(&fence);
        let current = ring.writer.identity();
        if current.cols != 0 && identity.clear_gen != current.clear_gen {
            let cap = ring.writer.cap();
            ring.writer
                .reset_ring(&self.store, identity, fence.oldest, cap, true)
                .map_err(|e| format!("history: {e}"))?;
        }
        if due > now || self.replace_parked.load(Ordering::Acquire) {
            return Ok(());
        }
        match self.replace_region(term, ring)? {
            Replaced::Done => {
                ring.replace_due = None;
                self.replace_owed.store(false, Ordering::Release);
            }
            Replaced::Deferred => {
                // A parser carry parks until its next bytes; a pending keeper receipt
                // remains pollable. Neither case starts another mapping before K1's quiet.
                ring.replace_due = Some(now.saturating_add(aterm_restore::policy::QUIET_MS));
            }
            Replaced::Unavailable(why) => {
                aterm_log::warn!(
                    "crash restore: session {}'s history ring is shed but its region could not \
                     be replaced ({why}); its pages stay resident until a retry succeeds",
                    self.id
                );
                ring.replace_due = Some(now.saturating_add(REPLACE_RETRY_MS));
            }
        }
        Ok(())
    }

    /// REPLACE THE REGION with a fresh one whose history ring is shed, so the pages the old
    /// ring touched leave this window: shared pages stay resident for as long as their
    /// region lives (`madvise` cannot give them back, the plan's D3 M-R5), so a new region is
    /// the only shed. On the journal worker, with the ring out of its slot (`ring`, the
    /// caller's), so no export can write into either region meanwhile:
    ///
    /// 1. Create the fresh region, holding no lock (microseconds: `shm_open`, `ftruncate`,
    ///    `mmap`; nothing resident until written), or take the one a deferred try kept.
    /// 2. Under the term lock, then the journal's (the parse thread's order: term, journal,
    ///    region), so no slice is processed and no host path stores a word meanwhile:
    ///    capture a keyframe of the engine as it stands (deferred when the parser holds a
    ///    sequence no keyframe carries whole: the region kept unwritten for the next try,
    ///    and the replacement parked until the parse thread records the program's next
    ///    bytes, [`SessionJournal::replace_parked`]); close the old
    ///    region with a SUPERSEDED record; swap the fresh region in under every handle,
    ///    seeding it with the gather thread's count in the same hold
    ///    (`aterm_restore::Shared::swap_in`); lay it out, continue the old region's stream
    ///    count in it (the gather thread's count runs from the session's first byte, so a
    ///    count from 0 would read the session's whole output as in flight), shed its ring
    ///    (a history cap of 0), store the host's words (cell size, config fingerprint,
    ///    resize word) and write the keyframe into it; and start the writer, the keyframe
    ///    policy and the resize
    ///    watermark over from that keyframe. The keyframe is written inside the term lock,
    ///    unlike the parse thread's (captured inside, written after): once per session per
    ///    pressure episode, about 100 µs with its 512-line bridge, and a slice processed
    ///    between the capture and the write would land in a tail with no keyframe under it.
    /// 3. With term/writer released and count-custody still held, copy the control slot
    ///    ([`Self::carry_control_into`]), then publish an unpublished initial descriptor
    ///    under the same count boundary as registration. Release count-custody before
    ///    entering the keeper link. A full queue hands back the fresh descriptor for
    ///    another offer. Queue admission retains the predecessor too: count stores and
    ///    privacy clears continue until an accepted SYNC receipt permits retirement.
    ///    A further replacement waits while that receipt is pending, so at most one
    ///    keeper-reachable predecessor is retained; Busy intermediates were never sent.
    ///
    /// A window that dies before the keeper took the COMPANION leaves the keeper the old
    /// region: its restore brings back the screen the old region held, pulsed, never as
    /// exact (the SUPERSEDED record, `restore_why=superseded`), or no screen when a clear
    /// erased it while the custody was owed, or when the replacement itself erased it: a
    /// clear recorded into it whose forced keyframe was captured but not yet written (or was
    /// deferred) is never settled out of it, since that write finds the region replaced and
    /// is skipped ([`Writer::settled_clear_gen`]). Its independent count continues through a
    /// full queue, an unread COMPANION and a privacy erase; the erased image takes META
    /// with that scalar alone. A legacy keeper without SYNC cannot authorize retirement.
    ///
    /// # Errors
    ///
    /// A store the fresh or the old region refused, or a keyframe that does not fit its slot
    /// even with no bridge: the journal stops (its caller's `stop`, which asks the keeper to
    /// drop the session's region), as a tail store's refusal does.
    fn replace_region(&self, term: &Mutex<Terminal>, ring: &mut Ring) -> Result<Replaced, String> {
        if self
            .turn_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiting_for_read()
        {
            // One queued predecessor is the bound. Busy intermediate regions were never
            // offered to the keeper and may still be replaced below.
            return Ok(Replaced::Deferred);
        }
        let (region, custody) = match ring.spare.take().map_or_else(new_region, Ok) {
            Ok(created) => created,
            Err(why) => return Ok(Replaced::Unavailable(why)),
        };
        let mut custody = Some(custody);
        let geometry = Geometry::v1();
        let (old, mut counts, erase) = {
            let t = crate::term_lock(term);
            let mut guard = self.lock();
            let w = &mut *guard;
            self.sync(w);
            let fence = t.history_fence();
            let pre = SpinePre::take(&t);
            let bridge = aterm_restore::policy::BRIDGE_CAP
                .min(usize::try_from(fence.lines()).unwrap_or(usize::MAX));
            let Some(checkpoint) =
                t.checkpoint_journal_marked(bridge, aterm_restore::policy::BRIDGE_CAP)
            else {
                // Parked under the term lock, so the parse thread's next slice, which needs
                // that lock, finds it and wakes the worker: no slice can end the sequence
                // between this capture and the park unseen. The fresh region is kept for
                // that try.
                self.replace_parked.store(true, Ordering::Release);
                ring.spare = Some((region, custody.take().expect("deferred custody")));
                return Ok(Replaced::Deferred);
            };
            w.writer
                .append_superseded()
                .map_err(|e| format!("supersede: {e}"))?;
            let mut counts = self
                .turn_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let old = Shared::new(
                swap_gathered_region(&self.store, region, &self.gathered)
                    .map_err(|_| "a fresh region of another length".to_string())?,
            );
            let mut writer = lay_out_shed_region(
                &self.store,
                &writer_identity(self.id, &self.sid, self.birth_us),
                w.writer.stream_processed(),
            )?;
            let cell_px = t.host_cell_pixel_size();
            store_cell_px(&self.store, cell_px).map_err(|e| format!("cell size: {e}"))?;
            store_config_fp(&self.store, w.config_fp).map_err(|e| format!("config: {e}"))?;
            if t.resize_lifetime() != 0 {
                store_resize_word(&self.store, t.resize_lifetime(), t.resize_ordinal())
                    .map_err(|e| format!("resize: {e}"))?;
            }
            let integration_on =
                t.shell_integration_posture() == aterm_core::terminal::ShellIntegrationPosture::On;
            let input = KeyframeInput {
                checkpoint: &checkpoint,
                fence,
                resize_lifetime: t.resize_lifetime(),
                resize_ordinal: t.resize_ordinal(),
                bridge_lines: u32::try_from(bridge).unwrap_or(u32::MAX),
                config_fp: w.config_fp,
                integration_on,
            };
            match writer.write_keyframe(&input) {
                Ok(_) => {}
                Err(KeyframeError::TooLarge { .. }) if bridge > 0 => {
                    // The plan's giant-screens rule, at once rather than at the next slice:
                    // nothing else may land in this tail before its first keyframe.
                    let bare = t
                        .checkpoint_journal_marked(0, 0)
                        .ok_or_else(|| "keyframe: no bare capture".to_string())?;
                    writer
                        .write_keyframe(&KeyframeInput {
                            checkpoint: &bare,
                            bridge_lines: 0,
                            ..input
                        })
                        .map_err(|e| format!("keyframe: {e}"))?;
                }
                Err(e) => return Err(format!("keyframe: {e}")),
            }
            let now_ms = w.now_ms();
            w.writer = writer;
            w.policy = KeyframePolicy::new(geometry.tail_cap, now_ms, w.config_fp);
            w.mark.seed(pre);
            w.slice_marks = 0;
            w.no_bridge = false;
            w.keyframes = w.keyframes.saturating_add(1);
            w.region_gen = w.region_gen.wrapping_add(1);
            // A clear the old region holds unsettled: its keyframe, captured but not yet
            // written (or deferred), is skipped as stale now, and its settle with it. The
            // fresh region's keyframe is of the engine after it.
            let unsettled = w.settled_clear_gen != fence.clear_gen;
            w.settled_clear_gen = fence.clear_gen;
            self.tail.store(0, Ordering::Relaxed);
            // The count lock spans swap, layout and seeding: no mint can store into
            // the fresh region before layout zeroes it, or miss the predecessor.
            store_ctl_count(&self.store, counts.count)
                .map_err(|e| format!("turn-id count: {e}"))?;
            let retained = counts.registered && counts.retired.is_none();
            if retained {
                counts.retired = Some(RetiredCount {
                    store: old.clone(),
                    receipt: self.companion_receipt(),
                    handed: false,
                    clear_gen: fence.clear_gen,
                    erased: false,
                    due: self.ring_now_ms().saturating_add(TICK_MS),
                });
            }
            (old, counts, unsettled && retained)
        };
        // No term/writer guard survives here. The count guard keeps first registration
        // from taking the old descriptor while the fresh control slot is copied.
        // Control never takes the count/ledger/term/writer locks while held.
        self.carry_control_into(&old)?;
        if erase && let Some(retired) = &mut counts.retired {
            // The region the keeper holds until it reads the fresh one's COMPANION still
            // holds every line that clear took (its keyframes' bridges, the tail's records
            // before it): erased now, after its control slot was carried, as a clear made
            // later erases it ([`Self::tend_custody`]). A region no keeper can hold (one
            // never registered, or one between that the keeper was never told) goes with
            // its last mapping below.
            retired
                .store
                .with(aterm_restore::erase_region)
                .map_err(|e| format!("erase: {e}"))?;
            retired.erased = true;
        }
        if !counts.registered {
            let mut slot = self
                .custody
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if slot.is_some() {
                *slot = Some(JournalCustody {
                    region: custody.take().expect("unpublished custody"),
                    len: u64::try_from(REGION_LEN).unwrap_or(u64::MAX),
                    receipt: None,
                });
            }
        }
        let receipt = counts.receipt();
        drop(counts);
        ring.custody_owed = None;
        *ring = ring.fresh(&geometry);
        self.publish_ring_end(None);
        self.regions_replaced.fetch_add(1, Ordering::Relaxed);
        match custody.and_then(|custody| self.hand_custody(custody, receipt)) {
            None => {
                let mut counts = self
                    .turn_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(old) = &mut counts.retired {
                    old.handed = true;
                }
                self.custody_owed
                    .store(counts.retired.is_some(), Ordering::Release);
            }
            Some(busy) => {
                ring.custody_owed = Some(OwedCustody {
                    custody: busy,
                    due: self.ring_now_ms().saturating_add(TICK_MS),
                });
                self.custody_owed.store(true, Ordering::Release);
            }
        }
        drop(old);
        Ok(Replaced::Done)
    }

    /// Tend a replaced region's custody the keeper link's queue could not take
    /// ([`OwedCustody`]), first in every tick while one is owed:
    ///
    /// 1. Read the clear generation under term, release term, then erase any retired
    ///    payload under count-custody ([`aterm_restore::erase_region`]). The counter-only
    ///    marker preserves ids while every screen/control reader takes META.
    /// 2. Offer a full queue's descriptor when due. Keep the predecessor's count stores
    ///    and privacy handling after queue admission until a matching, unrefused SYNC
    ///    confirms replacement. Polling and retry clocks authorize no retirement.
    ///
    /// # Errors
    ///
    /// The erase's store refused (never, for a mapping this window writes): the journal
    /// stops, which asks the keeper to drop the session's region.
    fn tend_custody(
        &self,
        ring: &mut Ring,
        term: &Mutex<Terminal>,
        now: u64,
    ) -> Result<(), String> {
        let retiring = self
            .turn_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retired
            .is_some();
        if !retiring && ring.custody_owed.is_none() {
            return Ok(());
        }
        let clear_gen = crate::term_lock(term).history_fence().clear_gen;
        let receipt = {
            let mut counts = self
                .turn_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counts.retire_confirmed();
            if let Some(old) = &mut counts.retired {
                if !old.erased && clear_gen != old.clear_gen {
                    old.store
                        .with(aterm_restore::erase_region)
                        .map_err(|e| format!("erase: {e}"))?;
                    old.erased = true;
                }
                old.due = now.saturating_add(TICK_MS);
            }
            self.custody_owed.store(
                counts.retired.is_some() || ring.custody_owed.is_some(),
                Ordering::Release,
            );
            counts.receipt()
        };
        self.ring_signal.fetch_and(!RING_CLEARED, Ordering::AcqRel);
        let Some(owed) = ring.custody_owed.as_mut() else {
            return Ok(());
        };
        if owed.due > now {
            return Ok(());
        }
        let Some(OwedCustody { custody, .. }) = ring.custody_owed.take() else {
            return Ok(());
        };
        match self.hand_custody(custody, receipt) {
            None => {
                let mut counts = self
                    .turn_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(old) = &mut counts.retired {
                    old.handed = true;
                }
                self.custody_owed
                    .store(counts.retired.is_some(), Ordering::Release);
            }
            Some(busy) => {
                ring.custody_owed = Some(OwedCustody {
                    custody: busy,
                    due: now.saturating_add(TICK_MS),
                });
            }
        }
        Ok(())
    }

    /// Hand a fresh region's read-only custody descriptor on, as its first one went: to the
    /// session's registration when the keeper link has not registered the session yet (it
    /// then takes this one with the master), else to the keeper link, which keeps it with the
    /// master in place of the old one (closing the old one's descriptor) and sends the
    /// keeper a COMPANION, on which the keeper closes the old region's. The descriptor comes
    /// back when the link's queue is full (a keeper slow to read, as one paged out under the
    /// pressure being shed is): the caller keeps it and offers it again
    /// ([`Ring::custody_owed`]), since the keeper would otherwise hold the superseded region
    /// for good.
    ///
    /// The custody lock is released before the link's is taken: the link takes the
    /// session's custody while holding its own (`keeper_link::register`), so the two are
    /// never held in the other order. A registration that takes the slot between the two
    /// steps queues its REGISTER (and, finding the slot empty, a COMPANION_RELEASE) before
    /// the link's lock is free again, so this COMPANION follows them and stands.
    fn hand_custody(
        &self,
        custody: OwnedFd,
        receipt: Option<Arc<aterm_keeper::client::CompanionReceipt>>,
    ) -> Option<OwnedFd> {
        let custody = {
            let mut slot = self
                .custody
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if slot.is_some() {
                *slot = Some(JournalCustody {
                    region: custody,
                    len: u64::try_from(REGION_LEN).unwrap_or(u64::MAX),
                    receipt,
                });
                return None;
            }
            custody
        };
        let len = u64::try_from(REGION_LEN).unwrap_or(u64::MAX);
        match self.offer_companion(custody, len, receipt.clone()) {
            crate::keeper_link::CompanionOffer::Handed => None,
            crate::keeper_link::CompanionOffer::Busy(busy) => Some(busy),
            crate::keeper_link::CompanionOffer::Unlinked(unsent) => {
                // No link carries the session (it was released, or no link runs): kept, as
                // a session's first descriptor is, for a registration that may still come.
                *self
                    .custody
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(JournalCustody {
                    region: unsent,
                    len,
                    receipt,
                });
                None
            }
        }
    }

    /// Offer the keeper link a fresh region's custody (`keeper_link::replace_companion`),
    /// or the test link a test gave this session ([`Self::link_custody_to`]).
    fn offer_companion(
        &self,
        custody: OwnedFd,
        len: u64,
        receipt: Option<Arc<aterm_keeper::client::CompanionReceipt>>,
    ) -> crate::keeper_link::CompanionOffer {
        #[cfg(test)]
        if let Some(link) = self
            .test_link
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return match receipt {
                Some(receipt) => link.replace_companion_checked(self.id, custody, len, receipt),
                None => link.replace_companion(self.id, custody, len),
            };
        }
        match receipt {
            Some(receipt) => {
                crate::keeper_link::replace_companion_checked(self.id, custody, len, receipt)
            }
            None => crate::keeper_link::replace_companion(self.id, custody, len),
        }
    }

    /// Offer this session's replaced regions' custody to `link` in place of the process's
    /// keeper link (a test's, whose queue it fills and drains).
    #[cfg(test)]
    pub(crate) fn link_custody_to(&self, link: Arc<crate::keeper_link::TestLink>) {
        self.note_custody_registered();
        *self
            .test_link
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(link);
    }

    /// Whether a deferred replacement's fresh region is kept for its next try
    /// ([`Ring::spare`]).
    #[cfg(test)]
    pub(crate) fn spare_kept(&self) -> bool {
        self.ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|ring| ring.spare.is_some())
    }

    /// Whether replaced custody remains owed: queued locally or awaiting the keeper's
    /// accepted read. This is the same status the production shedding marker reads.
    #[cfg(test)]
    pub(crate) fn custody_owed(&self) -> bool {
        self.custody_owed.load(Ordering::Acquire)
    }
}

/// A memory-pressure level, as the OS reports it (`memory_pressure`'s source on macOS) and
/// the `pressure` verb injects it in a development build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressureLevel {
    /// Pressure eased.
    Normal,
    /// The system asks processes to cap their growth.
    Warn,
    /// The tier before swap I/O and the out-of-memory dialog.
    Critical,
}

impl PressureLevel {
    /// The word `pressure level=` says.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }

    /// The level a `pressure` injection names.
    pub(crate) fn parse(word: &str) -> Option<Self> {
        match word {
            "normal" => Some(Self::Normal),
            "warn" => Some(Self::Warn),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// How long the pressure must have stayed eased before the first shed ring comes back (ms).
/// A pressure episode comes in waves (a WARN, a NORMAL, a WARN again while the build that
/// caused it is still linking), and every ring brought back re-exports its whole history
/// and touches its pages again: coming back at the first NORMAL would put the memory back
/// just in time for the next WARN to shed it, paying the LZ4 of every history twice.
#[cfg(unix)]
const RING_BACK_HOLD_MS: u64 = 60_000;
/// Then one ring at a time, this far apart (ms): each one's re-export runs at the worker's
/// own pace (32 chunks a tick), so at most one deep history's catch-up starts a second, and
/// a window of thirty tabs does not re-touch its few hundred MiB at once.
#[cfg(unix)]
const RING_BACK_GAP_MS: u64 = 1_000;

/// One session's ring as the shed policy weighs it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RingSize {
    /// The session's id.
    pub(crate) id: u64,
    /// The bytes its ring touched ([`SessionJournal::ring_bytes`]).
    pub(crate) bytes: usize,
    /// Already shed.
    pub(crate) shed: bool,
}

/// What a pass of the journal worker does to the rings ([`ShedPolicy::plan`]).
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShedPlan {
    /// The sessions whose rings are shed now, largest first.
    pub(crate) shed: Vec<u64>,
    /// The session whose ring comes back now.
    pub(crate) back: Option<u64>,
}

/// THE MEMORY SHED POLICY (the plan's increment 6): which history rings memory pressure
/// takes away, and when they come back. A pure state machine the journal worker runs and
/// hands its clock (milliseconds) and every session's ring size.
///
/// The rings are the journal's one large cost: a fresh tab's region is 16 KiB, its ring up
/// to a few MiB at a full history (the plan's §6), all of it charged to this window, whose
/// footprint jetsam reads. The keyframes and the tail stay (the screen restores exactly
/// with or without a ring), so a shed costs a crash only the scrollback above a keyframe's
/// bridge, counted (`status history_lost=`), never the screen:
///
/// | level | rings shed |
/// |---|---|
/// | WARN | the largest, largest first, until those left hold at most half of what all held: the system asks for growth to stop, and the scrollback's own WARN shed keeps half too (`pressure_shed_divisor`) |
/// | CRITICAL | every ring, and every ring a session opens while the level holds |
/// | NORMAL | none; after the pressure has stayed eased [`RING_BACK_HOLD_MS`], the rings come back one at a time, [`RING_BACK_GAP_MS`] apart, oldest session first |
///
/// A new report of pressure while the rings wait to come back cancels the wait.
#[cfg(unix)]
#[derive(Clone, Debug, Default)]
pub(crate) struct ShedPolicy {
    /// The level last reported, `None` before any.
    level: Option<PressureLevel>,
    /// When the pressure eased with a ring shed (ms), while it stays eased.
    eased_at: Option<u64>,
    /// When the last ring came back.
    last_back: Option<u64>,
}

#[cfg(unix)]
impl ShedPolicy {
    /// The level last reported.
    pub(crate) fn level(&self) -> Option<PressureLevel> {
        self.level
    }

    /// One pass at `now`, with `reported` the level reported since the last pass (if any)
    /// and `rings` every session's ring.
    pub(crate) fn plan(
        &mut self,
        reported: Option<PressureLevel>,
        now: u64,
        rings: &[RingSize],
    ) -> ShedPlan {
        if let Some(level) = reported {
            self.level = Some(level);
            self.eased_at = match level {
                // A repeated NORMAL keeps the first one's hold.
                PressureLevel::Normal => self.eased_at.or(Some(now)),
                PressureLevel::Warn | PressureLevel::Critical => None,
            };
        }
        let mut unshed: Vec<RingSize> = rings.iter().filter(|r| !r.shed).copied().collect();
        unshed.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.id.cmp(&b.id)));
        let shed = match (self.level, reported) {
            (Some(PressureLevel::Critical), _) => unshed.iter().map(|r| r.id).collect(),
            (Some(PressureLevel::Warn), Some(PressureLevel::Warn)) => {
                let total: usize = unshed.iter().map(|r| r.bytes).sum();
                let mut left = total;
                unshed
                    .iter()
                    .take_while(|r| {
                        let over = r.bytes > 0 && left > total / 2;
                        left -= r.bytes;
                        over
                    })
                    .map(|r| r.id)
                    .collect()
            }
            _ => Vec::new(),
        };
        let back = self
            .back_due(rings)
            .filter(|due| *due <= now)
            .and_then(|_| {
                let id = rings.iter().filter(|r| r.shed).map(|r| r.id).min()?;
                self.last_back = Some(now);
                Some(id)
            });
        ShedPlan { shed, back }
    }

    /// When the next shed ring comes back (ms), `None` while none waits to.
    pub(crate) fn back_due(&self, rings: &[RingSize]) -> Option<u64> {
        if self.level != Some(PressureLevel::Normal) || !rings.iter().any(|r| r.shed) {
            return None;
        }
        let held = self.eased_at?.saturating_add(RING_BACK_HOLD_MS);
        Some(
            self.last_back
                .map_or(held, |at| held.max(at.saturating_add(RING_BACK_GAP_MS))),
        )
    }
}

/// THE JOURNAL WORKER: one thread per window process that exports every session's
/// history ring (the plan's increment 5, step 1), at `qos::Role::Responsive` because it
/// takes the term lock the UI thread contends (the rule at `qos.rs`'s `Role`). It holds
/// no session alive (weak references, pruned as sessions go) and costs nothing while no
/// history grows: with nothing due it waits with no timeout, and only a session's signal
/// (the first growth since its last tick, a quiescence, a clear, an export's start or
/// release) wakes it.
///
/// It is also where the MEMORY SHED runs (the plan's increment 6): the main thread hands it
/// each pressure level the OS reports ([`note_memory_pressure`]), and each pass runs
/// [`ShedPolicy`] over every session's ring, sheds what it names and brings back what has
/// waited long enough. Here and not on the main thread, because a shed replaces a
/// session's region, and only the thread that exports a ring can be sure no export writes
/// into either region meanwhile (it holds the ring out of its slot for the replacement, as
/// for a tick); the main thread is spared the term-lock holds too.
///
/// And it writes every session's CONTROL SLOT (the plan's increment 7): the session's turn
/// ledger, carried at most every [`CONTROL_MS`] while it takes records
/// ([`SessionJournal::control_tick`]), woken by the ledger's first record since the last
/// carry. The plan gave this to the main thread; it is here instead because a carry clones
/// and encodes up to a whole ledger and compresses it, which the UI thread should not pay
/// for, and because the shed's replacement region must be given the slot before its
/// custody goes to the keeper, which only this thread can order.
#[cfg(unix)]
pub(crate) struct HistoryWorker {
    ring_queue: Mutex<WorkerState>,
    wake: std::sync::Condvar,
    ring_joiner: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The shed policy's clock (milliseconds since the worker started), and what a test has
    /// moved it on by ([`Self::pass_shed_ms`]).
    shed_epoch: Instant,
    shed_skew_ms: AtomicU64,
    /// What the `pressure` verb reads of the policy, published at the end of each pass: the
    /// level last reported ([`level_code`]), and when the next shed ring comes back (on the
    /// shed clock; `u64::MAX` for none).
    shed_level: AtomicU8,
    shed_back_at: AtomicU64,
}

#[cfg(unix)]
#[derive(Default)]
struct WorkerState {
    sessions: Vec<(
        std::sync::Weak<SessionJournal>,
        std::sync::Weak<Mutex<Terminal>>,
    )>,
    /// A signal arrived since the worker last looked.
    pending: bool,
    shutdown: bool,
    /// The pressure level reported since the last pass (the newest wins: the policy acts
    /// on where the pressure stands).
    shed_reported: Option<PressureLevel>,
    /// Who waits for that level to be applied (the `pressure` verb's injection).
    shed_waiters: Vec<std::sync::mpsc::Sender<()>>,
}

/// [`HistoryWorker::shed_level`]'s code for a level (0: none reported yet).
#[cfg(unix)]
fn level_code(level: Option<PressureLevel>) -> u8 {
    match level {
        None => 0,
        Some(PressureLevel::Normal) => 1,
        Some(PressureLevel::Warn) => 2,
        Some(PressureLevel::Critical) => 3,
    }
}

/// The window's journal worker, once its first session's export started it
/// ([`HistoryWorker::ring_worker`]): `Some(None)` when its thread could not start.
#[cfg(unix)]
static RING_WORKER: OnceLock<Option<Arc<HistoryWorker>>> = OnceLock::new();

#[cfg(unix)]
impl HistoryWorker {
    /// The window's worker, started at its first session's export. `None` when the
    /// thread could not start (logged once): every session then journals its screen with
    /// no history ring.
    ///
    /// A worker started after the OS reported a pressure level begins from that level
    /// ([`REPORTED_PRESSURE`]): a CRITICAL reported before the window's first export sheds
    /// the rings its sessions open, as one reported later would.
    fn ring_worker() -> Option<Arc<Self>> {
        let mut started = false;
        let worker = RING_WORKER
            .get_or_init(|| {
                started = true;
                match Self::start_ring_worker() {
                    Ok(worker) => Some(worker),
                    Err(e) => {
                        aterm_log::warn!(
                            "crash restore: the journal worker could not start ({e}); a crash \
                             restores screens without their scrollback"
                        );
                        None
                    }
                }
            })
            .clone();
        if started && let Some(worker) = &worker {
            seed_pressure_in(&REPORTED_PRESSURE, worker);
        }
        worker
    }

    /// Start a worker on its own thread.
    ///
    /// # Errors
    ///
    /// The thread's spawn refusal.
    pub(crate) fn start_ring_worker() -> std::io::Result<Arc<Self>> {
        let worker = Arc::new(Self {
            ring_queue: Mutex::new(WorkerState::default()),
            wake: std::sync::Condvar::new(),
            ring_joiner: Mutex::new(None),
            shed_epoch: Instant::now(),
            shed_skew_ms: AtomicU64::new(0),
            shed_level: AtomicU8::new(0),
            shed_back_at: AtomicU64::new(u64::MAX),
        });
        let run = Arc::clone(&worker);
        let handle = std::thread::Builder::new()
            .name("aterm-journal".to_string())
            .spawn(move || run.run_ring_worker())?;
        *worker
            .ring_joiner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
        Ok(worker)
    }

    fn enlist_ring(
        &self,
        journal: std::sync::Weak<SessionJournal>,
        term: std::sync::Weak<Mutex<Terminal>>,
    ) {
        self.ring_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .push((journal, term));
    }

    fn wake_ring_worker(&self) {
        self.ring_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending = true;
        self.wake.notify_one();
    }

    /// The OS reported memory pressure `level`: the next pass applies [`ShedPolicy`] to it,
    /// then answers `applied` (when given), after the regions it shed were replaced (a
    /// replacement the parser defers waits for the program's next bytes, then is tried no
    /// sooner than K1's quiet after its last try, [`SessionJournal::shed_tick`]). Called for
    /// each level macOS reports (`note_memory_pressure`), and on every unix for a worker that
    /// starts after a level was reported, with the level it begins from
    /// ([`HistoryWorker::ring_worker`]).
    pub(crate) fn note_shed_pressure(
        &self,
        level: PressureLevel,
        applied: Option<std::sync::mpsc::Sender<()>>,
    ) {
        let mut state = self
            .ring_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.shed_reported = Some(level);
        state.shed_waiters.extend(applied);
        state.pending = true;
        drop(state);
        self.wake.notify_one();
    }

    /// The shed policy's clock: milliseconds since the worker started, plus a test's skew.
    fn shed_now_ms(&self) -> u64 {
        u64::try_from(self.shed_epoch.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(self.shed_skew_ms.load(Ordering::Relaxed))
    }

    /// Move the shed policy's clock on by `ms` and wake the worker (a test reaches the hold
    /// before shed rings come back without sleeping).
    #[cfg(test)]
    pub(crate) fn pass_shed_ms(&self, ms: u64) {
        self.shed_skew_ms.fetch_add(ms, Ordering::Relaxed);
        self.wake_ring_worker();
    }

    /// The `pressure` verb's view of the shed: the level last reported, how many sessions
    /// the worker exports for and how many of their rings are shed, and in how many
    /// milliseconds the next shed ring comes back.
    fn shed_view(&self) -> (Option<PressureLevel>, usize, usize, Option<u64>) {
        let journals: Vec<Arc<SessionJournal>> = self
            .ring_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sessions
            .iter()
            .filter_map(|(journal, _)| journal.upgrade())
            .collect();
        let level = match self.shed_level.load(Ordering::Relaxed) {
            1 => Some(PressureLevel::Normal),
            2 => Some(PressureLevel::Warn),
            3 => Some(PressureLevel::Critical),
            _ => None,
        };
        let back_at = self.shed_back_at.load(Ordering::Relaxed);
        let back_in = (back_at != u64::MAX).then(|| back_at.saturating_sub(self.shed_now_ms()));
        let shed = journals.iter().filter(|j| j.ring_shed()).count();
        (level, journals.len(), shed, back_in)
    }

    /// End the worker's thread and wait for it (a test's own worker).
    #[cfg(test)]
    pub(crate) fn stop_ring_worker(&self) {
        self.ring_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutdown = true;
        self.wake.notify_one();
        let handle = self
            .ring_joiner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    fn run_ring_worker(&self) {
        crate::qos::set_self(crate::qos::Role::Responsive);
        let mut policy = ShedPolicy::default();
        loop {
            let (sessions, reported, waiters) = {
                let mut state = self
                    .ring_queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.shutdown {
                    return;
                }
                state.pending = false;
                state.sessions.retain(|(journal, term)| {
                    journal.strong_count() > 0 && term.strong_count() > 0
                });
                (
                    state.sessions.clone(),
                    state.shed_reported.take(),
                    std::mem::take(&mut state.shed_waiters),
                )
            };
            let live: Vec<(Arc<SessionJournal>, Arc<Mutex<Terminal>>)> = sessions
                .iter()
                .filter_map(|(journal, term)| Some((journal.upgrade()?, term.upgrade()?)))
                .collect();
            // The shed policy first, so a ring it sheds is replaced by this very pass.
            let rings = |live: &[(Arc<SessionJournal>, Arc<Mutex<Terminal>>)]| -> Vec<RingSize> {
                live.iter()
                    .map(|(journal, _)| RingSize {
                        id: journal.id,
                        bytes: journal.ring_bytes(),
                        shed: journal.ring_shed(),
                    })
                    .collect()
            };
            let shed_now = self.shed_now_ms();
            let plan = policy.plan(reported, shed_now, &rings(&live));
            for (journal, _) in &live {
                if plan.shed.contains(&journal.id) {
                    journal.request_shed();
                }
                if plan.back == Some(journal.id) {
                    journal.request_unshed();
                }
            }
            if !plan.shed.is_empty() || plan.back.is_some() {
                aterm_log::info!(
                    "crash restore: memory {}: {} history ring(s) shed, {} back",
                    policy.level().map_or("-", PressureLevel::word),
                    plan.shed.len(),
                    u8::from(plan.back.is_some())
                );
            }
            // The soonest any session's next tick is due, in milliseconds from now.
            let mut wake_in: Option<u64> = None;
            for (journal, term) in &live {
                let now = journal.ring_now_ms();
                if journal.ring_due(now).is_some_and(|due| due <= now) {
                    journal.export_tick(term);
                }
                if journal.control_due(now).is_some_and(|due| due <= now) {
                    journal.control_tick();
                }
                let now = journal.ring_now_ms();
                if let Some(due) = earliest(journal.ring_due(now), journal.control_due(now)) {
                    let gap = due.saturating_sub(now);
                    wake_in = Some(wake_in.map_or(gap, |soonest| soonest.min(gap)));
                }
            }
            let back_at = policy.back_due(&rings(&live));
            if let Some(at) = back_at {
                let gap = at.saturating_sub(self.shed_now_ms());
                wake_in = Some(wake_in.map_or(gap, |soonest| soonest.min(gap)));
            }
            self.shed_level
                .store(level_code(policy.level()), Ordering::Relaxed);
            self.shed_back_at
                .store(back_at.unwrap_or(u64::MAX), Ordering::Relaxed);
            for waiter in waiters {
                let _ = waiter.send(());
            }
            drop(live);
            let deadline = wake_in.map(|ms| Instant::now() + std::time::Duration::from_millis(ms));
            let mut state = self
                .ring_queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !state.pending && !state.shutdown {
                match deadline {
                    None => {
                        state = self
                            .wake
                            .wait(state)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    Some(at) => {
                        let now = Instant::now();
                        if now >= at {
                            break;
                        }
                        state = self
                            .wake
                            .wait_timeout(state, at - now)
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .0;
                    }
                }
            }
        }
    }
}

/// The memory-pressure level the OS last reported to this window ([`note_memory_pressure`]),
/// whether a worker runs or not, and whether the window's journal worker has been told it:
/// what `pressure level=` says, and where a worker that starts after the report begins
/// ([`HistoryWorker::ring_worker`]). Both sides decide under this lock, so a level reported
/// while the worker starts reaches it exactly once.
static REPORTED_PRESSURE: PressureCell = std::sync::Mutex::new((None, false));

/// The level the OS last reported and whether the journal worker was told it
/// ([`REPORTED_PRESSURE`], or a test's own).
type PressureCell = std::sync::Mutex<(Option<PressureLevel>, bool)>;

/// The OS reported memory pressure `level` (the main thread's `Wake::MemoryPressure` arm,
/// beside the scrollback's own shed): it is kept as the window's level, and the window's
/// journal worker runs [`ShedPolicy`] on it at its next pass. `applied` is answered once it
/// has, or at once when no worker runs (no session has exported a ring, so there is nothing
/// to shed yet; the worker begins from this level when it starts). Only macOS's event loop
/// has that arm, so the Linux and Windows builds leave this uncalled.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn note_memory_pressure(
    level: PressureLevel,
    applied: Option<std::sync::mpsc::Sender<()>>,
) {
    #[cfg(unix)]
    note_pressure_in(
        &REPORTED_PRESSURE,
        || RING_WORKER.get().cloned().flatten(),
        level,
        applied,
    );
    #[cfg(not(unix))]
    {
        *REPORTED_PRESSURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (Some(level), false);
        if let Some(applied) = applied {
            let _ = applied.send(());
        }
    }
}

/// [`note_memory_pressure`] over `pressure_cell`, with `worker` the window's journal worker
/// if one runs, asked under the cell's lock: [`seed_pressure_in`] decides under the same
/// lock, after the worker is published, so a level reported while the worker starts
/// reaches it exactly once, by one side or the other.
#[cfg(unix)]
fn note_pressure_in(
    pressure_cell: &PressureCell,
    worker: impl FnOnce() -> Option<Arc<HistoryWorker>>,
    level: PressureLevel,
    applied: Option<std::sync::mpsc::Sender<()>>,
) {
    let mut held = pressure_cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *held = (Some(level), false);
    if let Some(worker) = worker() {
        worker.note_shed_pressure(level, applied);
        held.1 = true;
        return;
    }
    drop(held);
    if let Some(applied) = applied {
        let _ = applied.send(());
    }
}

/// A journal worker that just started begins from the level `pressure_cell` holds, unless
/// it was told it already ([`note_pressure_in`]).
#[cfg(unix)]
fn seed_pressure_in(pressure_cell: &PressureCell, worker: &HistoryWorker) {
    let mut held = pressure_cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let (Some(level), false) = *held {
        worker.note_shed_pressure(level, None);
        held.1 = true;
    }
}

/// The `pressure` verb's line: `level=<-|normal|warn|critical> journals=<n> rings_shed=<n>
/// journal_resident_bytes=<n> rings_back_in_ms=<-|n>`: the level the OS last reported
/// (`-`: none since launch), the sessions the journal worker exports for and how many of
/// their history rings memory pressure shed, the RAM every region of this window costs it
/// ([`resident_bytes`]), and in how many milliseconds the next shed ring comes back (`-`
/// while none waits to).
pub(crate) fn pressure_words() -> String {
    let level = REPORTED_PRESSURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .0;
    #[cfg(unix)]
    let (journals, shed, back_in) =
        RING_WORKER
            .get()
            .cloned()
            .flatten()
            .map_or((0, 0, None), |worker| {
                let (_, journals, shed, back_in) = worker.shed_view();
                (journals, shed, back_in)
            });
    #[cfg(not(unix))]
    let (journals, shed, back_in): (usize, usize, Option<u64>) = (0, 0, None);
    format!(
        "level={} journals={journals} rings_shed={shed} journal_resident_bytes={} \
         rings_back_in_ms={}",
        level.map_or("-", PressureLevel::word),
        resident_bytes(),
        back_in.map_or_else(|| "-".to_string(), |ms| ms.to_string()),
    )
}

/// Whether this build accepts an injected level (`pressure warn|critical|normal`): a
/// development build only, like every `dev_seam!` and `appearance dark|light`.
const PRESSURE_INJECTION: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// The `pressure` verb's arguments: none (read), or one level to inject, which only a build
/// with `injection` admits.
fn pressure_request(rest: &str, injection: bool) -> Result<Option<PressureLevel>, &'static str> {
    const USAGE: &str = "usage: pressure [normal|warn|critical]";
    let mut words = rest.split_whitespace();
    let level = match (words.next(), words.next()) {
        (None, _) => None,
        (Some(word), None) => Some(PressureLevel::parse(word).ok_or(USAGE)?),
        (Some(_), Some(_)) => return Err(USAGE),
    };
    if level.is_some() && !injection {
        return Err("pressure normal|warn|critical needs a development build (dev-seams)");
    }
    Ok(level)
}

/// The `pressure [normal|warn|critical]` control verb, on a control worker. Bare, it
/// answers [`pressure_words`]. With a level, a development build injects it as the OS's
/// memory-pressure source reports one (`Wake::MemoryPressure`: the scrollback's shed and
/// the journal's) and answers once the journal worker applied it, so a driver reads the
/// shed's effect (`status journal_bytes=`, `metrics journal_resident_bytes=`, the window's
/// footprint) right after. Only macOS has a source to stand in for.
pub(crate) fn cmd_pressure(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    rest: &str,
) -> String {
    let level = match pressure_request(rest, PRESSURE_INJECTION) {
        Ok(level) => level,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    if let Some(level) = level {
        #[cfg(target_os = "macos")]
        if let Err(e) =
            crate::control::control_media::call_main(proxy, |applied| crate::Wake::MemoryPressure {
                level,
                applied: Some(applied),
            })
        {
            return format!("ERR {e}\n");
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (proxy, level);
            return "ERR pressure: no memory-pressure source on this platform\n".to_string();
        }
    }
    format!("OK {}\n", pressure_words())
}

/// How long the parse loop waits for its next batch while its session is dirty: K1's
/// quiet.
#[cfg(unix)]
pub(crate) const QUIET: std::time::Duration =
    std::time::Duration::from_millis(aterm_restore::policy::QUIET_MS);

/// Development seam `$ATERM_DEBUG_CRASH_JOURNAL_HOLD_KEYFRAMES` (`aterm_types::dev_seam!`,
/// debug builds only): every journal of this window writes its first keyframe and then
/// none but a forced one ([`Writer::admits`]), so every byte past it rides the tail. An
/// idle session's quiet keyframe (K1) otherwise empties its tail within 200 ms, so a live
/// test could crash a window with a tail to replay only by racing that keyframe: under
/// this seam its restore replays the session's whole output since the first keyframe
/// across a real SIGKILL, deterministically (the live lane `keeper_restore_live_headless`).
#[cfg(unix)]
const HOLD_KEYFRAMES_SEAM: &str = "ATERM_DEBUG_CRASH_JOURNAL_HOLD_KEYFRAMES";

// ---------------------------------------------------------------------------
// The restorer side
// ---------------------------------------------------------------------------

/// What a restore produced for one offered session: the checkpoint to adopt (with its
/// repaint flag), and the report.
#[cfg(unix)]
pub(crate) struct Restore {
    pub(crate) checkpoint: TerminalCheckpoint,
    /// Not exact: the program is pulsed to redraw over it.
    pub(crate) repaint: bool,
    pub(crate) report: RestoreReport,
    /// The history older than the checkpoint's own: the ring's lines the join takes (the
    /// carry the import reads after the recovery's restore drain) and the lines it could
    /// not take, counted ([`journal_history`]).
    pub(crate) history: crate::handoff_history::AdoptedHistory,
}

/// Restore the session the keeper offered with `companion` (the plan's §2.7): map the
/// region read-only, read its header, replay it into a scratch engine built as this
/// window builds a live one (`cfg`, the dead window's cell size), price the result
/// against the recovery's aggregate cell budget (`used_cells`), and join the history ring
/// below it ([`journal_history`]: the carry the import reads, the lines counted lost).
/// `attempts` is the keeper's poison brake: 0 replays the tail, 1 restores the keyframe
/// alone (the previous restorer may have died replaying it), 2 or more never reaches a
/// region.
///
/// The ring's chunks a join takes are moved out of the restore, already copied out of the
/// dead mapping (which is dropped before this returns): at most the ring, a few MiB per
/// session, held until the session's import decodes them one frame at a time, as an
/// update's sidecar is held open until its import reads it.
///
/// Whatever the screen's rung, once the header holds, the region's control slot is read
/// too ([`control_carry`]): the session's turn ledger, adopted as an update's sidecar is
/// ([`Restoration::control`]), and the dead window's turn-id count beside it
/// ([`Restoration::turn_ids`]).
///
/// The screen is the META rung's report, for the reason there is no screen: no region, a
/// brake at 2, a region that will not map or whose header this build does not read, no
/// valid keyframe, a screen over the budget, or a panic inside the restore (caught here,
/// so it costs this session its screen now rather than the next launch its recovery).
///
/// # Safety
///
/// The region is mapped read-only for the length of this call (`Region::map_readonly`'s
/// contract): until it returns, nothing may modify, resize or invalidate the object
/// behind `companion` through any other mapping or descriptor. A keeper's offer meets
/// that by construction (it is made only beside an orphaned master, after every window
/// that claimed it, the region's only writers, has died); a caller holding a live writer
/// of the region keeps it inert for the call.
#[cfg(unix)]
pub(crate) unsafe fn restore_offered(
    companion: Option<aterm_keeper::client::OfferedCompanion>,
    attempts: u8,
    cfg: Option<&TerminalConfig>,
    used_cells: &mut u64,
) -> Restoration {
    let mut control = None;
    let mut turn_ids = 0;
    // SAFETY: this function's own contract, passed on.
    let screen = unsafe {
        restore_screen(
            companion,
            attempts,
            cfg,
            used_cells,
            &mut control,
            &mut turn_ids,
        )
    };
    Restoration {
        screen,
        control,
        turn_ids,
    }
}

/// What a keeper recovery made of one offered session's region ([`restore_offered`]): its
/// screen, or the META rung's report, and, apart from either, read once the region's header
/// held, whatever the screen's rung: its turn ledger's crash carry (the control slot,
/// increment 7) and the dead window's turn-id count (the `ctl_count` word,
/// [`note_turn_ids`]).
#[cfg(unix)]
pub(crate) struct Restoration {
    pub(crate) screen: Result<Restore, RestoreReport>,
    pub(crate) control: Option<crate::handoff_carry::ControlCarry>,
    /// The dead window's turn-id count as its last mint stored it: past every id it used
    /// (0: none stored, or no header read). [`counted_carry`] folds it into the carry.
    pub(crate) turn_ids: u64,
}

/// [`restore_offered`]'s screen, with the control slot's carry left in `control` and the
/// region's turn-id count in `turn_ids` once the region's header holds.
///
/// # Safety
///
/// [`restore_offered`]'s.
#[cfg(unix)]
unsafe fn restore_screen(
    companion: Option<aterm_keeper::client::OfferedCompanion>,
    attempts: u8,
    cfg: Option<&TerminalConfig>,
    used_cells: &mut u64,
    control: &mut Option<crate::handoff_carry::ControlCarry>,
    turn_ids: &mut u64,
) -> Result<Restore, RestoreReport> {
    use std::os::fd::AsFd as _;
    if attempts >= 2 {
        return Err(RestoreReport::meta("attempts", attempts));
    }
    let Some(companion) = companion else {
        return Err(RestoreReport::meta("no-region", attempts));
    };
    if companion.kind != aterm_keeper::wire::COMPANION_JOURNAL {
        return Err(RestoreReport::meta("not-a-journal", attempts));
    }
    let len = usize::try_from(companion.len).map_err(|_| RestoreReport::meta("size", attempts))?;
    // SAFETY: the caller's contract (this function's `# Safety`): the object is stable and
    // unmodified until this call returns, and the mapping is dropped before it does.
    let region = unsafe { aterm_uds::shm::Region::map_readonly(companion.region.as_fd(), len) }
        .map_err(|_| RestoreReport::meta("map", attempts))?;
    // The descriptor is done with: the mapping keeps the object until it is dropped.
    drop(companion);
    // A privacy-cleared predecessor carries no screen or ledger, but its turn count
    // must still prevent an id used after the replacement from naming another exchange.
    *turn_ids = aterm_restore::read_erased_turn_count(&region).unwrap_or(0);
    let header =
        aterm_restore::read_header(&region).map_err(|_| RestoreReport::meta("header", attempts))?;
    // THE CONTROL SLOT (increment 7), read apart from the screen's rungs: the session's
    // turn ledger and the dead window's turn-id count ride the restore whether or not a
    // screen comes back.
    *control = control_carry(&region, &header.geometry);
    *turn_ids = header.ctl_count;
    let cell_px = header.cell_px;
    let policy = RecoverPolicy {
        tail: attempts == 0,
        config_fp: config_fp(config_hash(cfg), cell_px),
        scratch_nonce: aterm_core::shell_integration::generate_nonce()
            .into_parts()
            .0,
    };
    // Inside the screen reader's panic fence (`reader_guard`), not a bare `catch_unwind`:
    // the window's panic hook then files no `crash-<pid>.log` for a panic the restore
    // catches, which this launch's crash scan (`logging::take_crash_evidence`, after the
    // keeper's boot) would otherwise take as the DEAD window's crash, showing this
    // window's backtrace on the recovery row and renaming the dead window's own report
    // unseen. The panic is logged once here instead.
    let recovery = crate::reader_guard::guarded(|| {
        aterm_restore::recover(&region, policy, |rows, cols| {
            #[cfg(test)]
            tests::seam::replay_reached();
            scratch_engine(rows, cols, cfg, cell_px)
        })
    })
    .map_err(|panic| {
        aterm_log::warn!(
            "crash restore: the restore of a recovered session panicked at {} ({}); it takes \
             the META rung, its screen blank",
            panic.location,
            panic.message
        );
        RestoreReport::meta("panic", attempts)
    })?;
    drop(region);
    let mut restored = match recovery {
        Recovery::Restored(restored) => restored,
        Recovery::Meta(aterm_restore::MetaWhy::Header(_)) => {
            return Err(RestoreReport::meta("header", attempts));
        }
        Recovery::Meta(aterm_restore::MetaWhy::NoKeyframe(_)) => {
            return Err(RestoreReport::meta("no-keyframe", attempts));
        }
    };
    let cp = &restored.checkpoint;
    if crate::seamless::admit_restored_dimensions(
        used_cells,
        cp.rows,
        cp.cols,
        restored_history_lines(&restored),
        cp.alt_grid.is_some(),
    )
    .is_err()
    {
        // The screen goes blank, and with it every history line the region held: read,
        // so counted (`status history_lost=`, the ledger), not left at 0.
        return Err(RestoreReport {
            history_lost: history_held(&restored),
            ..RestoreReport::meta("cells", attempts)
        });
    }
    let exact = restored.exact && attempts == 0;
    let history = journal_history(&mut restored);
    let losses = &restored.losses;
    let report = RestoreReport {
        rung: if exact { Rung::Exact } else { Rung::Screen },
        why: if exact {
            "-"
        } else {
            first_loss(&restored, attempts)
        },
        attempts,
        in_flight_lost: losses.in_flight_lost,
        tail_overflow: losses.tail_overflow,
        marks_lost: losses.marks_lost,
        resizes_unjournaled: losses.resizes_unjournaled,
        history_lost: history.dropped,
    };
    Ok(Restore {
        checkpoint: restored.checkpoint,
        repaint: !exact,
        report,
        history,
    })
}

/// THE HISTORY JOIN of a restored session (the plan's increment 5, step 3, and its
/// §3.2 N3): the update handoff's own join (`handoff_history::join`, bound to
/// `NativeUpdateHistoryCarry`'s `Park`), called verbatim on what the restore read
/// (`aterm_restore::Restored::join_facts`): the head is the keyframe's fence moved on by
/// the replay, the carried lines the checkpoint's own, the export the ring's run. Its
/// verdict is the session's history carry:
///
/// - the ring's first `take` lines from the export's first row (the chunks holding them,
///   moved out of the restore: a crash restore's sidecar, `HistoryCarry::journal`),
///   imported below the checkpoint's lines after the recovery's restore drain exactly as
///   an update's sidecar is after Commit (the keys reserved at the adopt, the import's
///   claim fencing a clear the recovered shell sends meanwhile);
/// - `dropped` (the session's `status history_lost=`): the head's lines older than the
///   ring's run (the cap's evictions), or, when the two do not meet (a ring behind the
///   bridge, under a flood or a settle; a ring under another identity), every line the
///   head names and the checkpoint does not carry. Never lines silently gone.
///
/// A clear inside the replayed tail joins nothing and counts nothing: the person erased
/// what the ring held. A reflow or reveal inside it counts the keyframe's lines below its
/// bridge.
#[cfg(unix)]
fn journal_history(
    restored: &mut aterm_restore::Restored,
) -> crate::handoff_history::AdoptedHistory {
    journal_history_within(restored, crate::handoff_history::MAX_EXPORT_LINES)
}

/// [`journal_history`] carrying at most the newest `max` lines of the ring's run below the
/// checkpoint's own, as an update's export keeps the newest
/// `handoff_history::MAX_EXPORT_LINES` of a deeper history
/// (`aterm_restore::RingExport::newest`): the join counts every older line as dropped,
/// and the import is never handed more than a sidecar may hold, which it would refuse
/// whole, after the recovery row had said the scrollback came back. A ring of
/// compressible lines holds well over a million in its 6.9 MiB.
///
/// Only the chunks holding the lines taken leave the restore
/// (`aterm_restore::Restored::ring_chunks_for`): a chunk wholly below the first line taken
/// (lines the dead engine had evicted, or older than `max`) is never decoded, so the
/// import skips fewer lines than one chunk holds.
#[cfg(unix)]
fn journal_history_within(
    restored: &mut aterm_restore::Restored,
    max: u64,
) -> crate::handoff_history::AdoptedHistory {
    use crate::handoff_history::{AdoptedHistory, ExportFacts, HistoryCarry};
    let (carry, dropped) = match restored.join_facts() {
        Err(aterm_restore::HeadLost::Cleared) => (None, 0),
        Err(aterm_restore::HeadLost::Moved { lines }) => (None, lines),
        Ok(facts) => {
            let export = facts.export.map(|export| export.newest(max));
            let joined = crate::handoff_history::join(
                export.map(|export| ExportFacts {
                    fence: export.fence,
                    first: export.first,
                    lines: export.lines,
                }),
                facts.head,
                facts.carried,
            );
            let carry = export.filter(|_| joined.take > 0).map(|export| {
                let (range, skip) = restored.ring_chunks_for(&export, joined.take);
                let chunks: Vec<_> = restored.ring.chunks.drain(range).collect();
                HistoryCarry::journal(chunks, export.fence.cols, skip, joined.take)
            });
            (carry, joined.dropped)
        }
    };
    AdoptedHistory {
        carry,
        dropped,
        lost: dropped,
        ..AdoptedHistory::default()
    }
}

/// Every history line the region told the restore the dead window held, as far as it
/// says: the head the replay moved on (the keyframe's history and what the tail pushed),
/// or, past a clear or a reflow in the tail, the keyframe's lines below its bridge and
/// what the scratch engine holds now. What a restore that lands no screen at all (the
/// META rung after the region was read: a screen over the cell budget) loses of the
/// scrollback, counted as a restored screen's join counts its own.
#[cfg(unix)]
fn history_held(restored: &aterm_restore::Restored) -> u64 {
    match restored.join_facts() {
        Ok(facts) => facts.head.lines(),
        Err(aterm_restore::HeadLost::Cleared) => restored.carried_lines,
        Err(aterm_restore::HeadLost::Moved { lines }) => {
            lines.saturating_add(restored.carried_lines)
        }
    }
}

/// The history of a session on the META rung (`keeper_link`'s `admit_recovered_journaled`):
/// nothing to import, and the lines its report counts lost (`status history_lost=` from
/// its registration on, as a restored session's join counts its own).
#[cfg(unix)]
pub(crate) fn meta_history(report: &RestoreReport) -> crate::handoff_history::AdoptedHistory {
    crate::handoff_history::AdoptedHistory {
        dropped: report.history_lost,
        lost: report.history_lost,
        ..crate::handoff_history::AdoptedHistory::default()
    }
}

/// The session's turn ledger out of a dead window's region (increment 7): its control
/// slot's carry (`aterm_restore::read_control`: the newest complete record, its CRC and
/// step checked), decoded as an update's sidecar is (`handoff_carry::decode_crash_carry`).
/// `None` for an empty slot (a session that never took a turn, or a death inside the
/// slot's write) or a refused one, which is said: a slot this build's writer left is never
/// refused, so a refusal is damage or another build's layout.
#[cfg(unix)]
fn control_carry<V: aterm_restore::RegionView + ?Sized>(
    view: &V,
    geometry: &Geometry,
) -> Option<crate::handoff_carry::ControlCarry> {
    use aterm_restore::ControlVerdict;
    // An update's sidecar past this is dropped unread; a crash carry is held to the same.
    const _: () = assert!(
        aterm_restore::records::MAX_CONTROL_CARRY as u64 <= crate::handoff_carry::MAX_SIDECAR_BYTES
    );
    match aterm_restore::read_control(view, geometry) {
        ControlVerdict::Valid { carry, .. } => {
            match crate::handoff_carry::decode_crash_carry(&carry) {
                Ok(carry) => Some(carry),
                Err(why) => {
                    aterm_log::warn!(
                        "crash restore: a recovered session's turn ledger is not carried: {why}"
                    );
                    None
                }
            }
        }
        ControlVerdict::Empty | ControlVerdict::NoSlot => None,
        ControlVerdict::Length(len) => {
            aterm_log::warn!(
                "crash restore: a recovered session's control slot names a {len}-byte record; \
                 its turn ledger is not carried"
            );
            None
        }
        ControlVerdict::Unreadable => {
            aterm_log::warn!(
                "crash restore: a recovered session's control slot could not be read; its turn \
                 ledger is not carried"
            );
            None
        }
        ControlVerdict::Record(fault) => {
            aterm_log::warn!(
                "crash restore: a recovered session's control record was refused ({fault:?}); \
                 its turn ledger is not carried"
            );
            None
        }
    }
}

/// The control carry a recovered session is adopted with ([`Restoration`]): the slot's,
/// its turn-id count raised to the region's `ctl_count` word, which a mint stores before
/// its id is anyone's ([`note_turn_ids`]) and no control write touches. A region with a
/// count and no carry (its slot torn by a death inside a write, or a session that never
/// took a turn) is adopted with an empty carry holding the count alone: its ledger holds
/// no record and, once the recovery continues the count ([`continue_carried_turns`]),
/// vouches for no id below where the count goes on, as an empty slot's ledger would
/// (`handoff_carry::adopted_ledger`). No count and no carry: none.
#[cfg(unix)]
pub(crate) fn counted_carry(
    control: Option<crate::handoff_carry::ControlCarry>,
    turn_ids: u64,
) -> Option<crate::handoff_carry::ControlCarry> {
    let turn_ids = turn_ids.min(crate::handoff_carry::MAX_TURN_ID);
    match control {
        Some(mut carry) => {
            carry.turn_ids = carry.turn_ids.max(turn_ids);
            Some(carry)
        }
        None => (turn_ids > 0).then(|| crate::handoff_carry::ControlCarry {
            turn_ids,
            ..crate::handoff_carry::ControlCarry::default()
        }),
    }
}

/// TURN IDS GO ON AFTER A CRASH (increment 7), at the keeper recovery's boot, before this
/// process mints a turn id: the turn-id count continues above every recovered session's
/// count ([`counted_carry`]: its region's count word, which its process stored at every
/// mint, its carry's own count and its highest record) plus the update's reserve
/// (`control::TURN_ID_RESERVE`), and every carried ledger stops vouching for the ids below
/// where the count goes on (its `unheld_below` raised there). The dead window kept
/// recording turns after its last carry (at most two seconds before its death, a turn
/// still open then, or a carry its death tore), and nothing says how many: its records may
/// be gone, not absent, so `history` says the floor (`unheld_below=`) and a resumed
/// `since-turn=` below it is sent a GAP, as for a ledger an update could not carry whole.
/// The count word makes the count go on past every id the dead window minted, in any of
/// its sessions, the closed and the unjournaled included, since every live journal's region
/// holds it ([`note_turn_ids`]); the reserve is margin for a region an older build wrote,
/// whose only count is its carry's, up to two seconds old. A recovery with no count at all
/// (no session of the dead window kept a journal) changes nothing: the count starts over,
/// and each session's empty ledger then says its own floor
/// (`handoff_carry::adopted_ledger`).
#[cfg(unix)]
pub(crate) fn continue_carried_turns(adopted: &mut [crate::spawn::Adopted]) {
    let mut carries: Vec<&mut crate::handoff_carry::ControlCarry> = adopted
        .iter_mut()
        .filter_map(|a| a.control.as_mut())
        .collect();
    if let Some(ceiling) = carried_turn_ceiling(&mut carries) {
        crate::control::raise_turn_ids(ceiling);
    }
}

/// [`continue_carried_turns`]'s arithmetic: where the turn-id count goes on (`None` with no
/// carry), with every carry's floor raised to one past it.
#[cfg(unix)]
fn carried_turn_ceiling(carries: &mut [&mut crate::handoff_carry::ControlCarry]) -> Option<u64> {
    let minted = carries
        .iter()
        .map(|carry| carry.turn_ids.max(carry.high_turn_id().unwrap_or(0)))
        .max()?;
    let ceiling = minted
        .saturating_add(crate::control::TURN_ID_RESERVE)
        .min(crate::handoff_carry::MAX_TURN_ID);
    let floor = ceiling.saturating_add(1);
    for carry in carries.iter_mut() {
        carry.unheld_below = carry.unheld_below.max(floor);
    }
    Some(ceiling)
}

/// Every history line a restored checkpoint carries, for its price against the cell
/// budget: the active grid's (`history_lines`) and, while the alternate screen is up, the
/// saved primary's. Those ride in `alt_grid` with no count of their own (the keyframe
/// bridge and every line the replay pushed sit on the primary, `checkpoint_journal`'s and
/// `checkpoint_continuing_swallowing`'s own layout), and `history_lines` is the alternate
/// grid's, always 0: charged by it alone, a recovery of many `vim` or `less` sessions
/// over deep primaries would pass the aggregate the cap exists to hold. The scratch
/// engine's fence after the replay counts the primary's lines exactly
/// (`Restored::carried_lines`, its scrollback).
#[cfg(unix)]
fn restored_history_lines(restored: &aterm_restore::Restored) -> u32 {
    let cp = &restored.checkpoint;
    if cp.modes.alternate_screen {
        cp.history_lines
            .saturating_add(u32::try_from(restored.carried_lines).unwrap_or(u32::MAX))
    } else {
        cp.history_lines
    }
}

/// The first reason a restored screen is not exact, as one `status restore_why=` word.
#[cfg(unix)]
fn first_loss(restored: &aterm_restore::Restored, attempts: u8) -> &'static str {
    let l = &restored.losses;
    if attempts > 0 {
        "keyframe-only"
    } else if l.superseded {
        // The memory shed had moved the session to a fresh region, and the window died
        // before the keeper took its COMPANION: this is the region it left.
        "superseded"
    } else if l.tail_overflow {
        "tail-overflow"
    } else if l.tail_fault.is_some() {
        "tail-fault"
    } else if l.in_flight_lost > 0 {
        "in-flight"
    } else if l.tail_unreplayed > 0 {
        "tail-unreplayed"
    } else if l.resizes_lost > 0 || l.resizes_unjournaled > 0 {
        "resizes"
    } else if l.regrids_replayed > 0 {
        "regrid"
    } else if l.marks_lost > 0 {
        "marks"
    } else if l.engine_replaced {
        "engine-replaced"
    } else if l.graphics_replayed > 0 {
        "graphics"
    } else if l.unscrolls_replayed > 0 {
        "unscroll"
    } else if l.swallowed.is_some() {
        "swallowed"
    } else if l.config_moved {
        "config-moved"
    } else {
        // Nothing lost: the tail was recorded under another config than this window
        // runs (a tab with a theme of its own that was writing when it died, or a
        // config edited between the two windows).
        "config"
    }
}

#[cfg(all(test, unix))]
#[path = "crash_restore_tests.rs"]
mod tests;
