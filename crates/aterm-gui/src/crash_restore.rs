// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S CRASH-RESTORE JOURNAL — the exact screen and scrollback after an unplanned window death (increments 4 and 5 of the crash-restore plan; the headless half is `aterm-restore`, the keeper's custody is `aterm-keeper`'s companions).

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

/// The store a window journals through: its read-write mapping, counting the pages it makes resident, shared by the threads that write it (module doc).
#[cfg(unix)]
pub(crate) type Store = Shared<Touched<aterm_uds::shm::Region>>;

#[cfg(unix)]
fn resident_meter() -> &'static Arc<AtomicU64> {
    static METER: OnceLock<Arc<AtomicU64>> = OnceLock::new();
    METER.get_or_init(|| Arc::new(AtomicU64::new(0)))
}

/// `metrics journal_resident_bytes=`: the RAM this window's crash-restore regions cost it, every page their writers touched (the plan's increment 6).
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

/// The hash of the host config an engine was built or reconfigured with: FNV-1a over its `Debug` rendering, which names every field (floats included) in a fixed order, and a fixed word for no config at all (the boot session's placeholder path).
#[cfg(unix)]
pub(crate) fn config_hash(cfg: Option<&TerminalConfig>) -> u64 {
    match cfg {
        Some(cfg) => aterm_hash::fnv1a_64(format!("{cfg:?}").as_bytes()),
        None => aterm_hash::fnv1a_64(b"aterm-crash-restore:no-config"),
    }
}

/// The `config_fp` a keyframe and the header carry: the config's hash and the cell size folded together, never 0 (a header word of 0 is a region whose host never stored one, which a restore reads as "not checked").
#[cfg(unix)]
pub(crate) fn config_fp(config_hash: u64, cell_px: Option<(u16, u16)>) -> u64 {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&config_hash.to_le_bytes());
    bytes[8..].copy_from_slice(&aterm_restore::layout::cell_px_word(cell_px).to_le_bytes());
    aterm_hash::fnv1a_64(&bytes).max(1)
}

#[cfg(unix)]
fn scratch_engine(
    rows: u16,
    cols: u16,
    cfg: Option<&TerminalConfig>,
    cell_px: Option<(u16, u16)>,
) -> Terminal {
    crate::spawn::new_live_terminal(rows, cols, cfg, cell_px)
}

/// Which rung a keeper-recovered session landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only a unix keeper recovery restores a session")
)]
pub(crate) enum Rung {
    /// The screen, modes and parser carry exactly as the dead window had them, every byte it read replayed (no size pulse for a main-screen session).
    Exact,
    /// A screen from the region with a loss counted (a keyframe alone, an overflowed tail, a byte read and never processed, a resize or a mark the tail missed, a kitty image command in the tail, a tail recorded under another config, a region the memory shed had superseded when the window died): the program is pulsed to redraw over it.
    Screen,
    /// No screen: a blank one carrying the keeper's META scalars, as before the journal (no region, a refused one, a second failed restore, a panic).
    Meta,
}

impl Rung {
    /// The `status restore=` word.
    pub(crate) const fn word(self) -> &'static str {
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
    pub(crate) why: &'static str,
    pub(crate) attempts: u8,
    pub(crate) in_flight_lost: u64,
    pub(crate) tail_overflow: bool,
    pub(crate) marks_lost: u64,
    pub(crate) resizes_unjournaled: u64,
    pub(crate) history_lost: u64,
}

impl RestoreReport {
    /// The META rung, for `why`.
    #[cfg(unix)]
    pub(crate) const fn meta(why: &'static str, attempts: u8) -> Self {
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

    /// The `status` words after `journal=`: `restore=<rung> restore_why=<why> in_flight_lost=<n> tail_overflow=<0|1> marks_lost=<n> resizes_unjournaled=<n>`.
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

/// How many keeper-recovered sessions landed on each rung (the band row, the recovery census).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RungCounts {
    pub(crate) exact: u32,
    pub(crate) screen: u32,
    pub(crate) meta: u32,
}

impl RungCounts {
    /// Count one more on `rung`.
    #[cfg(unix)]
    pub(crate) const fn note(&mut self, rung: Rung) {
        let slot = match rung {
            Rung::Exact => &mut self.exact,
            Rung::Screen => &mut self.screen,
            Rung::Meta => &mut self.meta,
        };
        *slot = slot.saturating_add(1);
    }

    /// How many screens came back from a region (exact or not).
    pub(crate) const fn screens(self) -> u32 {
        self.exact.saturating_add(self.screen)
    }
}

/// What a keeper recovery's restores came to: the rungs (the band row's [`RungCounts`]) and the fields `status` reports for each recovered session, summed, for the recovery census's ledger row (`aterm_update::recovery_ledger::Restores`), so the ledger shows which loss real crashes hit, the signal the plan's follow-up work waits for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RestoreTally {
    pub(crate) counts: RungCounts,
    pub(crate) in_flight_lost: u64,
    pub(crate) tail_overflows: u32,
    pub(crate) marks_lost: u64,
    pub(crate) resizes_unjournaled: u64,
    pub(crate) history_lost: u64,
    pub(crate) history_short: u32,
    pub(crate) meta_history_lost: u64,
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

    /// The history lines the restored screens could not bring back, the band row's count (every line of a blank screen's is said apart, in words).
    pub(crate) const fn screens_history_lost(&self) -> u64 {
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
            refused: 0,
        }
    }
}

/// The session's journal slot (`SessionCtx::crash_journal`): its journal once one is installed, why it has none, and what a keeper recovery restored for it.
#[derive(Default)]
pub(crate) struct JournalSlot {
    #[cfg(unix)]
    journal: OnceLock<Arc<SessionJournal>>,
    off: OnceLock<String>,
    restored: OnceLock<RestoreReport>,
}

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

    /// The host resized the session's engine (the caller holds its lock, in the resize's hold): [`SessionJournal::host_resized`] when it has a journal, else nothing.
    pub(crate) fn host_resized(&self, t: &aterm_core::terminal::Terminal) {
        #[cfg(unix)]
        if let Some(journal) = self.journal.get() {
            journal.host_resized(t);
        }
        #[cfg(not(unix))]
        let _ = t;
    }

    /// The host changed the session engine's settings (the caller holds its lock, in the setter's hold): [`SessionJournal::host_settings`] when it has a journal.
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

    /// The `status` words: `journal=live` with `journal_tail=` (the tail a restore now would replay; 0 once an idle session's quiet keyframe has caught up), `journal_ring_lag=` (history lines the history ring has not exported yet; 0 once the journal worker has caught the history up), `journal_ring=<live|shedding|shed>` (`shed`: memory pressure took the history ring away, so a crash brings back the screen and the keyframe's few hundred lines of history and counts the rest lost, until the pressure has eased a minute and the ring is exported again; `shedding`: the shed's old region, the ring's pages and the lines they hold, is still in memory, since it still waits to be replaced or the keeper link has not taken its replacement's custody yet, whether or not the pressure has eased since), `journal_bytes=` (the RAM the session's region costs this window, every page written) and `journal_ledger_lag=` (turn records of the session's ledger the region's control slot does not hold: taken since its last carry, at most two seconds, or shed from it to fit, [`SessionJournal::ledger_lag`]; 0 once it holds every record the ledger does), or `journal=off` with a `journal_off=` reason when the session has none (the keeper off: `journal_off=keeper-off`), then a recovered session's [`RestoreReport::status_words`].
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

#[cfg(unix)]
struct JournalClock {
    epoch: Instant,
    skew_ms: AtomicU64,
    #[cfg(test)]
    held: bool,
}

#[cfg(unix)]
impl JournalClock {
    fn start() -> Self {
        Self {
            epoch: Instant::now(),
            skew_ms: AtomicU64::new(0),
            #[cfg(test)]
            held: false,
        }
    }

    #[cfg(test)]
    fn held() -> Self {
        Self {
            held: true,
            ..Self::start()
        }
    }

    fn now_ms(&self) -> u64 {
        #[cfg(test)]
        if self.held {
            return self.skew_ms.load(Ordering::Relaxed);
        }
        u64::try_from(self.epoch.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(self.skew_ms.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    fn pass(&self, ms: u64) {
        self.skew_ms.fetch_add(ms, Ordering::Relaxed);
    }
}

#[cfg(unix)]
struct Writer {
    writer: JournalWriter<Store>,
    policy: KeyframePolicy,
    mark: SpineMark,
    clock: Arc<JournalClock>,
    resize_policy: ResizePolicy,
    config_fp: u64,
    slice_marks: u64,
    no_bridge: bool,
    keyframes: u64,
    deferred: u64,
    hold: bool,
    region_gen: u64,
    settled_clear_gen: u64,
}

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
    const fn admits(&self, reason: Reason) -> bool {
        !self.hold || matches!(reason, Reason::Clear | Reason::Forced | Reason::Renumbered)
    }

    fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    const fn sync_host(&mut self, host_fp: u64) {
        if host_fp != 0 && host_fp != self.config_fp {
            self.config_fp = host_fp;
            self.policy.note_config(host_fp);
        }
    }

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

#[cfg(unix)]
fn bridge_lines(fence: &HistoryFence, exported: Option<(HistoryIdentity, u64)>) -> u64 {
    match exported {
        Some((identity, end)) if identity == fence_identity(fence) => {
            fence.end.saturating_sub(end.max(fence.oldest))
        }
        _ => fence.lines(),
    }
}

#[cfg(unix)]
const fn fence_identity(fence: &HistoryFence) -> HistoryIdentity {
    HistoryIdentity {
        renumber_epoch: fence.renumber_epoch,
        clear_gen: fence.clear_gen,
        reveal_gen: fence.reveal_gen,
        cols: fence.cols,
    }
}

/// A keyframe captured under the term lock, written after it ([`SessionJournal::write`]).
#[cfg(unix)]
pub(crate) struct Captured {
    checkpoint: TerminalCheckpoint,
    fence: HistoryFence,
    lifetime: u64,
    ordinal: u64,
    bridge: u32,
    config_fp: u64,
    integration_on: bool,
    reason: Reason,
    tail_at: u64,
    region_gen: u64,
}

/// What a hold read before processing its bytes, and what its records have counted so far ([`SessionJournal::record`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlicePre {
    spine: SpinePre,
    marks: u64,
    clear_gen: u64,
    hist_end: u64,
    recorded: bool,
}

#[cfg(unix)]
fn new_region() -> Result<(Touched<aterm_uds::shm::Region>, OwnedFd), String> {
    // SAFETY: the backing object is modified only through this `Region`, which the journal alone holds (behind `Shared`), and it is never resized after `create`'s one `ftruncate` (macOS refuses a second).
    let (region, custody) = unsafe { aterm_uds::shm::Region::create(REGION_LEN) }
        .map_err(|e| format!("region: {e}"))?;
    Ok((
        Touched::new(region, Some(Arc::clone(resident_meter()))),
        custody,
    ))
}

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
    store: Store,
    writer: Mutex<Writer>,
    gathered: AtomicU64,
    cfg_hash: AtomicU64,
    host_fp: AtomicU64,
    custody: Mutex<Option<JournalCustody>>,
    lane: Arc<crate::manual_reset::ResetLane>,
    id: u64,
    tail: AtomicU64,
    broken: OnceLock<String>,
    ring: Mutex<Option<Ring>>,
    exported: Mutex<Option<(HistoryIdentity, u64)>>,
    ring_signal: AtomicU8,
    export_held: AtomicBool,
    seen_end: AtomicU64,
    seen_oldest: AtomicU64,
    renumbered: AtomicBool,
    worker: OnceLock<Arc<HistoryWorker>>,
    clock: Arc<JournalClock>,
    history_shed: AtomicBool,
    replace_owed: AtomicBool,
    custody_owed: AtomicBool,
    replace_parked: AtomicBool,
    regions_replaced: AtomicU64,
    #[cfg(test)]
    test_link: Mutex<Option<Arc<crate::keeper_link::TestLink>>>,
    sid: String,
    birth_us: u64,
    ledger: OnceLock<std::sync::Weak<Mutex<crate::turn_ledger::TurnLedger>>>,
    ledger_moved: AtomicBool,
    control: Mutex<Control>,
    turn_ids: Mutex<TurnCountCustody>,
    recovery_count: OnceLock<Arc<AtomicBool>>,
}

#[cfg(unix)]
impl SessionJournal {
    /// Create session `id`'s region, lay it out and write its FIRST keyframe of `term` (the engine as the spawn left it: fresh, or hydrated from an adoption), before the reader starts and before anyone is told the region exists.
    pub(crate) fn create(
        id: u64,
        sid: &str,
        term: &Mutex<Terminal>,
        master: i32,
        cfg: Option<&TerminalConfig>,
        lane: Arc<crate::manual_reset::ResetLane>,
    ) -> Result<Arc<Self>, String> {
        Self::create_on(id, sid, term, master, cfg, lane, JournalClock::start())
    }

    /// [`Self::create`] on a held clock ([`JournalClock::held`]): only [`Self::pass_ms`] moves it, from the first keyframe on.
    #[cfg(test)]
    pub(crate) fn create_held(
        id: u64,
        sid: &str,
        term: &Mutex<Terminal>,
        master: i32,
        cfg: Option<&TerminalConfig>,
        lane: Arc<crate::manual_reset::ResetLane>,
    ) -> Result<Arc<Self>, String> {
        Self::create_on(id, sid, term, master, cfg, lane, JournalClock::held())
    }

    fn create_on(
        id: u64,
        sid: &str,
        term: &Mutex<Terminal>,
        master: i32,
        cfg: Option<&TerminalConfig>,
        lane: Arc<crate::manual_reset::ResetLane>,
        clock: JournalClock,
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
        let resize_policy = crate::app_render::pty_resize_policy(master);
        let cfg_hash = config_hash(cfg);
        let clock = Arc::new(clock);
        let journal = Arc::new(Self {
            store,
            writer: Mutex::new(Writer {
                writer,
                policy: KeyframePolicy::new(aterm_restore::layout::TAIL_CAP_V1, 0, 0),
                mark: SpineMark::attach(Arc::new(SpineWatermark::default())),
                clock: Arc::clone(&clock),
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
            clock,
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
            w.settled_clear_gen = t.history_fence().clear_gen;
            let now_ms = w.now_ms();
            w.policy = KeyframePolicy::new(Geometry::v1().tail_cap, now_ms, fp);
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

    /// The custody descriptor and the region's length, for the session's registration (once).
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

    /// `n` PTY bytes were read: store the running count before the batch is sent.
    pub(crate) fn gathered(&self, n: usize) {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        let total = self
            .gathered
            .fetch_add(n, Ordering::Relaxed)
            .saturating_add(n);
        let _ = store_gathered(&self.store, total);
    }

    /// The host resized the engine (the caller holds its lock, in the resize's hold): store the resize word, then nudge the parse thread to record the resize now, whether or not the program writes again.
    pub(crate) fn host_resized(&self, t: &Terminal) {
        let _ = store_resize_word(&self.store, t.resize_lifetime(), t.resize_ordinal());
        let _ = self.lane.nudge();
    }

    /// The host changed the engine's settings (the caller holds its lock, in the setter's hold): `cfg` when it applied a config, `None` for a cell-size change.
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

    /// Record `bytes` the hold just processed (the caller holds the term lock, the hold `pre` was read in): the verified marks they moved first, then every resize before them and the bytes; a clear among them makes the next keyframe forced and wakes the history ring's worker to zero the ring; history they pushed tells the worker the ring has lines to export.
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
        if self.replace_parked.load(Ordering::Relaxed)
            && self.replace_parked.swap(false, Ordering::AcqRel)
        {
            // A shed region's replacement waited for bytes that could end the sequence
            // that deferred it (K5): these may have.
            self.notify_worker();
        }
        if cleared {
            self.ring_signal.fetch_or(RING_CLEARED, Ordering::AcqRel);
            self.nudge_ring();
        } else if grew {
            self.ring_grew();
        }
        pre.recorded = true;
    }

    /// The end of a hold that recorded something (the caller still holds the term lock): the policy's decision, and the keyframe it calls for captured.
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

    /// Write a captured keyframe (the term lock released): copy it into its slot and commit it; after a clear's keyframe, settle the clear out of the region.
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

    /// Whether the session has tail past its keyframe that a quiet wait can keyframe (the parse loop then waits for its next batch with a [`QUIET`] timeout, and otherwise blocks): not while a K5 deferral parks it (module doc).
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

    /// A host path nudged the parse thread ([`Self::host_resized`], [`Self::host_settings`]): record the resizes now, take the new settings, and write a keyframe owed for them (K4).
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

    fn write_quiet(&self, captured: Captured) {
        let quiescence = captured.reason == Reason::Quiescence;
        self.write(captured);
        if quiescence {
            self.nudge_ring();
        }
    }

    /// Move the journal's clock (the policy's and the ring's) on by `ms`, so a test reaches K1's quiet, the ring's settle or the next tick without sleeping.
    #[cfg(test)]
    pub(crate) fn pass_ms(&self, ms: u64) {
        self.clock.pass(ms);
    }

    /// Keyframes written and captures deferred, for a test's view of the policy.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (u64, u64) {
        let w = self.lock();
        (w.keyframes, w.deferred)
    }

    /// A copy of the region's bytes as they stand: what a restorer would map if this window died now.
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

#[cfg(unix)]
const RING_GREW: u8 = 1;
#[cfg(unix)]
const RING_NUDGED: u8 = 2;
#[cfg(unix)]
const RING_CLEARED: u8 = 4;
#[cfg(unix)]
const TICK_MS: u64 = 1_000;
#[cfg(unix)]
const FLOOD_TICK_MS: u64 = 5_000;
#[cfg(unix)]
const SETTLE_MS: u64 = 2_000;
#[cfg(unix)]
const MAX_CHUNKS_PER_TICK: usize = 32;
#[cfg(unix)]
const FLOOD_LINES: u64 = (MAX_CHUNKS_PER_TICK * aterm_restore::records::CHUNK_LINES) as u64;

#[cfg(unix)]
fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(unix)]
struct Ring {
    writer: HistoryWriter,
    hist_cap_phys: usize,
    settling: Option<(HistoryIdentity, u64)>,
    last_tick: Option<u64>,
    end_at_tick: u64,
    flooding: bool,
    next_due: Option<u64>,
    bytes_per_line: u64,
    replace_due: Option<u64>,
    spare: Option<(Touched<aterm_uds::shm::Region>, OwnedFd)>,
    custody_owed: Option<OwedCustody>,
}

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

    fn fresh(&self, geometry: &Geometry) -> Self {
        Self {
            bytes_per_line: self.bytes_per_line,
            ..Self::new(geometry)
        }
    }

    fn cap_for(&self, line_limit: Option<usize>) -> usize {
        adaptive_hist_cap(line_limit, self.bytes_per_line, self.hist_cap_phys)
    }

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

#[cfg(unix)]
enum ChunkRead {
    Moved,
    Done,
    Hole(u64),
    Lines(Vec<aterm_core::scrollback::Line>),
}

#[cfg(unix)]
impl SessionJournal {
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

    /// History lines past where the ring has reached (`status journal_ring_lag=`): 0 once the export has caught the session's history up, the whole history while it waits for an import or a settle, or before its first tick.
    pub(crate) fn ring_lag(&self) -> u64 {
        let reached = self
            .ring_exported()
            .map_or(0, |(_, end)| end)
            .max(self.seen_oldest.load(Ordering::Relaxed));
        self.seen_end
            .load(Ordering::Relaxed)
            .saturating_sub(reached)
    }

    fn note_seen(&self, fence: &HistoryFence) {
        self.seen_oldest.store(fence.oldest, Ordering::Relaxed);
        self.seen_end.store(fence.end, Ordering::Relaxed);
    }

    fn ring_now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    fn notify_worker(&self) {
        if let Some(worker) = self.worker.get() {
            worker.wake_ring_worker();
        }
    }

    fn ring_grew(&self) {
        if self.export_held.load(Ordering::Acquire) {
            return;
        }
        if self.ring_signal.fetch_or(RING_GREW, Ordering::AcqRel) & RING_GREW == 0 {
            self.notify_worker();
        }
    }

    fn nudge_ring(&self) {
        self.ring_signal.fetch_or(RING_NUDGED, Ordering::AcqRel);
        self.notify_worker();
    }

    /// Hand the session's history ring to the window's journal worker ([`HistoryWorker::ring_worker`]), its export held for an import when `held` (an adopted session: [`Self::import_settled`] opens it).
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

    /// The session's history import settled (`handoff_history::run_imports`, on its worker; the main thread's `Wake::HandoffHistorySettled`): its older history is now below the carried lines, under the renumbering the attach made.
    pub(crate) fn import_settled(&self) {
        self.export_held.store(false, Ordering::Release);
        self.renumbered.store(true, Ordering::Release);
        let _ = self.lane.nudge();
        self.nudge_ring();
    }

    /// When the worker should next export this session's ring, on its clock: now for a nudge, a tick after the last for a growth (five while it floods), or what the last tick left due; `None` for nothing to do (an idle session costs the worker nothing), a held export or a stopped journal.
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

#[cfg(unix)]
const CONTROL_MS: u64 = 2_000;

#[cfg(unix)]
#[derive(Default)]
struct Control {
    writer: aterm_restore::ControlWriter,
    generation: Option<u64>,
    shed: u64,
    at: Option<u64>,
}

#[cfg(unix)]
static TURN_ID_HOLDERS: Mutex<Vec<std::sync::Weak<SessionJournal>>> = Mutex::new(Vec::new());

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

/// THE TURN-ID COUNT RIDES EVERY REGION (the plan's increment 7): the process's turn-id count moved (`control::mint_turn_id`, `control::raise_turn_ids`), so every live journal's region is given it (its `ctl_count` word, which no control write touches).
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
    /// Store `count` into the region's `ctl_count` word when it is above the count the word holds ([`Self::turn_ids`], held across the store): the word only rises, whichever thread stores and in whatever order the threads that read the count get here.
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
    /// CARRY THE SESSION'S TURN LEDGER into its region's control slot (the plan's increment 7), so a crash restore brings back the turns driven into the session (`handoff_carry::CrashLedger`): watch `turns`, whose every record wakes the journal worker ([`Self::ledger_moved`]), and when it already carries anything (an adopted session's carried ledger, or the floor of one that could not be carried) write it now, on the spawning thread, before the session's registration hands the keeper the region: a crash right after an update or a recovery then still carries it.
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

    fn ledger_moved(&self) {
        if !self.ledger_moved.swap(true, Ordering::AcqRel) {
            self.notify_worker();
        }
    }

    /// When the worker should next carry the ledger, on the ring's clock: at once after a record, or [`CONTROL_MS`] after the last carry; `None` while no record landed since (an idle session costs the worker nothing) or once the journal stopped.
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

    /// One carry of the ledger (the journal worker, once [`Self::control_due`]): taken under the ledger's lock when it moved since the slot's carry (a clone of its records), encoded with no lock held, and written into the slot.
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

    /// `status journal_ledger_lag=`: the session's turn records its region's control slot does not hold: those the ledger took since the slot's last carry (each carried at most two seconds after it lands), and the oldest ones that carry left out because the whole ledger did not fit the slot (they stay out while the ledger is that long: a few hundred records of long prompts fill it). 0 only once the slot holds every record the ledger holds: a crash now restores them all.
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

/// One descriptor and the read receipt belonging to that exact descriptor, including a replacement racing its session's initial registration.
#[cfg(unix)]
pub(crate) struct JournalCustody {
    pub(crate) region: OwnedFd,
    pub(crate) len: u64,
    pub(crate) receipt: Option<Arc<aterm_keeper::client::CompanionReceipt>>,
}

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

#[cfg(unix)]
const REPLACE_RETRY_MS: u64 = 10_000;

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

#[cfg(unix)]
enum Replaced {
    Done,
    Deferred,
    Unavailable(String),
}

#[cfg(unix)]
impl SessionJournal {
    /// Whether the session's history ring is shed (`status journal_ring=shed`, or `shedding` while the shed's old region is still in memory, [`Self::ring_shedding`]).
    pub(crate) fn ring_shed(&self) -> bool {
        self.history_shed.load(Ordering::Acquire)
    }

    /// Whether the shed's old region is still in memory (`status journal_ring=shedding`): the shed ring's region still waits to be replaced, its pages still this window's, or it was replaced but the keeper link has not taken the fresh region's custody yet, so the keeper still holds the old one and this window keeps it mapped ([`OwedCustody`]), whether or not the pressure has eased since.
    pub(crate) fn ring_shedding(&self) -> bool {
        (self.ring_shed() && self.replace_owed.load(Ordering::Acquire))
            || self.custody_owed.load(Ordering::Acquire)
    }

    /// `status journal_bytes=`: the bytes of every page this session's region has written, what it keeps resident in this window.
    pub(crate) fn resident_bytes(&self) -> usize {
        self.store.with(Touched::touched_bytes)
    }

    /// Written pages intersecting the history ring, including pages a privacy clear zeroed: those pages stay accounted to this region until it is unmapped.
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

    /// SHED the history ring (the journal worker, on [`ShedPolicy`]'s word): nothing more is exported, and from the parse thread's next keyframe on every keyframe carries its own bridge of history (up to 512 lines, as a region with no ring does), since there is no ring to meet.
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

    /// The pressure has eased (the worker, on [`ShedPolicy`]'s word): the ring is exported again from its next tick.
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
        drop(spare);
        self.nudge_ring();
    }

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
            let unsettled = w.settled_clear_gen != fence.clear_gen;
            w.settled_clear_gen = fence.clear_gen;
            self.tail.store(0, Ordering::Relaxed);
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
        self.carry_control_into(&old)?;
        if erase && let Some(retired) = &mut counts.retired {
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

    /// Offer this session's replaced regions' custody to `link` in place of the process's keeper link (a test's, whose queue it fills and drains).
    #[cfg(test)]
    pub(crate) fn link_custody_to(&self, link: Arc<crate::keeper_link::TestLink>) {
        self.note_custody_registered();
        *self
            .test_link
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(link);
    }

    /// Whether a deferred replacement's fresh region is kept for its next try ([`Ring::spare`]).
    #[cfg(test)]
    pub(crate) fn spare_kept(&self) -> bool {
        self.ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|ring| ring.spare.is_some())
    }

    /// Whether replaced custody remains owed: queued locally or awaiting the keeper's accepted read.
    #[cfg(test)]
    pub(crate) fn custody_owed(&self) -> bool {
        self.custody_owed.load(Ordering::Acquire)
    }
}

/// A memory-pressure level, as the OS reports it (`memory_pressure`'s source on macOS) and the `pressure` verb injects it in a development build.
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
    pub(crate) const fn word(self) -> &'static str {
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

#[cfg(unix)]
const RING_BACK_HOLD_MS: u64 = 60_000;
#[cfg(unix)]
const RING_BACK_GAP_MS: u64 = 1_000;

/// One session's ring as the shed policy weighs it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RingSize {
    pub(crate) id: u64,
    pub(crate) bytes: usize,
    pub(crate) shed: bool,
}

/// What a pass of the journal worker does to the rings ([`ShedPolicy::plan`]).
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShedPlan {
    pub(crate) shed: Vec<u64>,
    pub(crate) back: Option<u64>,
}

/// THE MEMORY SHED POLICY (the plan's increment 6): which history rings memory pressure takes away, and when they come back.
#[cfg(unix)]
#[derive(Clone, Debug, Default)]
pub(crate) struct ShedPolicy {
    level: Option<PressureLevel>,
    eased_at: Option<u64>,
    last_back: Option<u64>,
}

#[cfg(unix)]
impl ShedPolicy {
    /// The level last reported.
    pub(crate) const fn level(&self) -> Option<PressureLevel> {
        self.level
    }

    /// One pass at `now`, with `reported` the level reported since the last pass (if any) and `rings` every session's ring.
    pub(crate) fn plan(
        &mut self,
        reported: Option<PressureLevel>,
        now: u64,
        rings: &[RingSize],
    ) -> ShedPlan {
        if let Some(level) = reported {
            self.level = Some(level);
            self.eased_at = match level {
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

/// THE JOURNAL WORKER: one thread per window process that exports every session's history ring (the plan's increment 5, step 1), at `qos::Role::Responsive` because it takes the term lock the UI thread contends (the rule at `qos.rs`'s `Role`).
#[cfg(unix)]
pub(crate) struct HistoryWorker {
    ring_queue: Mutex<WorkerState>,
    wake: std::sync::Condvar,
    ring_joiner: Mutex<Option<std::thread::JoinHandle<()>>>,
    shed_clock: JournalClock,
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
    pending: bool,
    shutdown: bool,
    shed_reported: Option<PressureLevel>,
    shed_waiters: Vec<std::sync::mpsc::Sender<()>>,
}

#[cfg(unix)]
const fn level_code(level: Option<PressureLevel>) -> u8 {
    match level {
        None => 0,
        Some(PressureLevel::Normal) => 1,
        Some(PressureLevel::Warn) => 2,
        Some(PressureLevel::Critical) => 3,
    }
}

#[cfg(unix)]
static RING_WORKER: OnceLock<Option<Arc<HistoryWorker>>> = OnceLock::new();

#[cfg(unix)]
impl HistoryWorker {
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
    pub(crate) fn start_ring_worker() -> std::io::Result<Arc<Self>> {
        Self::start_ring_worker_on(JournalClock::start())
    }

    /// [`Self::start_ring_worker`] with its shed clock held ([`JournalClock::held`]): only [`Self::pass_shed_ms`] moves it.
    #[cfg(test)]
    pub(crate) fn start_held_ring_worker() -> std::io::Result<Arc<Self>> {
        Self::start_ring_worker_on(JournalClock::held())
    }

    fn start_ring_worker_on(shed_clock: JournalClock) -> std::io::Result<Arc<Self>> {
        let worker = Arc::new(Self {
            ring_queue: Mutex::new(WorkerState::default()),
            wake: std::sync::Condvar::new(),
            ring_joiner: Mutex::new(None),
            shed_clock,
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

    /// The OS reported memory pressure `level`: the next pass applies [`ShedPolicy`] to it, then answers `applied` (when given), after the regions it shed were replaced (a replacement the parser defers waits for the program's next bytes, then is tried no sooner than K1's quiet after its last try, [`SessionJournal::shed_tick`]).
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

    fn shed_now_ms(&self) -> u64 {
        self.shed_clock.now_ms()
    }

    /// Move the shed policy's clock on by `ms` and wake the worker (a test reaches the hold before shed rings come back without sleeping).
    #[cfg(test)]
    pub(crate) fn pass_shed_ms(&self, ms: u64) {
        self.shed_clock.pass(ms);
        self.wake_ring_worker();
    }

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

static REPORTED_PRESSURE: PressureCell = std::sync::Mutex::new((None, false));

type PressureCell = std::sync::Mutex<(Option<PressureLevel>, bool)>;

/// The OS reported memory pressure `level` (the main thread's `Wake::MemoryPressure` arm, beside the scrollback's own shed): it is kept as the window's level, and the window's journal worker runs [`ShedPolicy`] on it at its next pass.
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

/// The `pressure` verb's line: `level=<-|normal|warn|critical> journals=<n> rings_shed=<n> journal_resident_bytes=<n> rings_back_in_ms=<-|n>`: the level the OS last reported (`-`: none since launch), the sessions the journal worker exports for and how many of their history rings memory pressure shed, the RAM every region of this window costs it ([`resident_bytes`]), and in how many milliseconds the next shed ring comes back (`-` while none waits to).
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

const PRESSURE_INJECTION: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

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

/// The `pressure [normal|warn|critical]` control verb, on a control worker.
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

/// How long the parse loop waits for its next batch while its session is dirty: K1's quiet.
#[cfg(unix)]
pub(crate) const QUIET: std::time::Duration =
    std::time::Duration::from_millis(aterm_restore::policy::QUIET_MS);

#[cfg(unix)]
const HOLD_KEYFRAMES_SEAM: &str = "ATERM_DEBUG_CRASH_JOURNAL_HOLD_KEYFRAMES";

/// What a restore produced for one offered session: the checkpoint to adopt (with its repaint flag), and the report.
#[cfg(unix)]
pub(crate) struct Restore {
    pub(crate) checkpoint: TerminalCheckpoint,
    pub(crate) repaint: bool,
    pub(crate) report: RestoreReport,
    pub(crate) history: crate::handoff_history::AdoptedHistory,
}

/// Restore the session the keeper offered with `companion`.
///
/// # Safety
/// Nothing may modify, resize, or invalidate the object behind `companion` during this call.
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

/// What a keeper recovery made of one offered session's region ([`restore_offered`]): its screen, or the META rung's report, and, apart from either, read once the region's header held, whatever the screen's rung: its turn ledger's crash carry (the control slot, increment 7) and the dead window's turn-id count (the `ctl_count` word, [`note_turn_ids`]).
#[cfg(unix)]
pub(crate) struct Restoration {
    pub(crate) screen: Result<Restore, RestoreReport>,
    pub(crate) control: Option<crate::handoff_carry::ControlCarry>,
    pub(crate) turn_ids: u64,
}

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
    // SAFETY: the caller's contract (this function's `# Safety`): the object is stable and unmodified until this call returns, and the mapping is dropped before it does.
    let region = unsafe { aterm_uds::shm::Region::map_readonly(companion.region.as_fd(), len) }
        .map_err(|_| RestoreReport::meta("map", attempts))?;
    drop(companion);
    *turn_ids = aterm_restore::read_erased_turn_count(&region).unwrap_or(0);
    let header =
        aterm_restore::read_header(&region).map_err(|_| RestoreReport::meta("header", attempts))?;
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

#[cfg(unix)]
fn journal_history(
    restored: &mut aterm_restore::Restored,
) -> crate::handoff_history::AdoptedHistory {
    journal_history_within(restored, crate::handoff_history::MAX_EXPORT_LINES)
}

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

/// The history of a session on the META rung (`keeper_link`'s `admit_recovered_journaled`): nothing to import, and the lines its report counts lost (`status history_lost=` from its registration on, as a restored session's join counts its own).
#[cfg(unix)]
pub(crate) fn meta_history(report: &RestoreReport) -> crate::handoff_history::AdoptedHistory {
    crate::handoff_history::AdoptedHistory {
        dropped: report.history_lost,
        lost: report.history_lost,
        ..crate::handoff_history::AdoptedHistory::default()
    }
}

#[cfg(unix)]
fn control_carry<V: aterm_restore::RegionView + ?Sized>(
    view: &V,
    geometry: &Geometry,
) -> Option<crate::handoff_carry::ControlCarry> {
    use aterm_restore::ControlVerdict;
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

/// The control carry a recovered session is adopted with ([`Restoration`]): the slot's, its turn-id count raised to the region's `ctl_count` word, which a mint stores before its id is anyone's ([`note_turn_ids`]) and no control write touches.
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

/// TURN IDS GO ON AFTER A CRASH (increment 7), at the keeper recovery's boot, before this process mints a turn id: the turn-id count continues above every recovered session's count ([`counted_carry`]: its region's count word, which its process stored at every mint, its carry's own count and its highest record) plus the update's reserve (`control::TURN_ID_RESERVE`), and every carried ledger stops vouching for the ids below where the count goes on (its `unheld_below` raised there).
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

#[cfg(unix)]
const fn first_loss(restored: &aterm_restore::Restored, attempts: u8) -> &'static str {
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
