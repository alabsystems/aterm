// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Spotlight read whole (SD-93): the family's cost over a split window, summed from its
//! Apple-signed coalitions ([`reading`]), and each local volume's indexing state from
//! `mdutil -s -a`, which needs no root ([`volumes`]), run at most once a minute ([`Cadence`])
//! over a port with an inert test arm ([`MdutilProbe`]). The index's size on disk is root's to
//! read: aterm's root LAN broker walks each volume's store and the host hands its answer in
//! ([`StoreRead`], SD-94).
//!
//! The family's figures come from the coalition rows of the split, never from the process
//! sweep: the system-owned `mds` (root) and `mds_stores` (`_mds_stores`) read `refused` there.
//! So a reading exists only on a split tick, and a family process inside another lead's
//! coalition is not counted.

use crate::explain::{CoalRow, Lead, Voucher};
use crate::model::MemoryCharge;

/// The family is busy from this much reported CPU counter growth, millicores: the Spotlight
/// activity's own `CpuAtLeast` bar ([`crate::explain::ACTIVITIES`] reads it here).
pub const CPU_BAR_MC: i64 = 200;

/// The family is busy from this much read from storage, bytes per second: the Spotlight
/// activity's own `ReadAtLeast` bar.
pub const READ_BAR_BPS: u64 = 5_000_000;

/// `mdutil -s -a` runs at most this often, ms, and while the family is busy or crossed a bar
/// since the last run, this often.
pub const VOLUMES_EVERY_MS: u64 = 60_000;

/// While the family stays under its bars, `mdutil -s -a` runs this often, ms.
pub const VOLUMES_QUIET_MS: u64 = 300_000;

/// A volume reading older than this is not shown, ms.
pub const VOLUMES_STALE_MS: u64 = 600_000;

/// A split reading of the family is carried to later readings for at most this long, ms on the
/// host's monotonic clock.
pub const READING_CARRIED_MS: u64 = 60_000;

/// A store answer older than this is not shown, ms: three of the root broker's walks, which it
/// makes at most once every ten minutes.
pub const STORE_STALE_MS: u64 = 1_800_000;

/// The cannot lines a store answer meets ([`cannot`]): the store's folder, which only root reads,
/// read by the root broker for us. `index_progress` is not among them: whether root can read
/// indexing progress at all is owed a measurement made as root on macOS 27 (`mdutil -s -v`, the
/// store's item counts over time), which no agent of this lab may make (SD-94), so it stays
/// `unsupported`.
pub const STORE_MEETS: &[&str] = &["index_folder"];

/// What the host runs `mdutil` with: every local volume's status, which needs no root.
pub const MDUTIL_ARGS: [&str; 2] = ["-s", "-a"];

/// The family's signing identifiers, matched exactly and only under Apple's voucher (the
/// Spotlight activity's members, [`crate::explain::ACTIVITIES`], read them here): read with
/// `codesign -dv` on macOS 27.2.1 (26B131). No prefix match, which would let in a look-alike
/// such as `SpotlightSettingsIntentsExtension`. There is no plain `spotlightknowledged`.
pub const MEMBERS: &[&str] = &[
    "com.apple.mds",
    "com.apple.mds_stores",
    "com.apple.mdworker",
    "com.apple.mdworker_shared",
    "com.apple.corespotlightd",
    "com.apple.managedcorespotlightd",
    "com.apple.spotlightknowledged.graph",
    "com.apple.spotlightknowledged.importer",
    "com.apple.spotlightknowledged.updater",
];

/// One volume's indexing state, as `mdutil -s` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Indexing {
    /// `Indexing enabled.`
    Enabled,
    /// `Indexing disabled.` or `Indexing and searching disabled.`
    Disabled,
    /// Any other status line, as printed (`Error: unknown indexing state.`), or `""` where the
    /// volume's block held none. Never read as disabled.
    Unknown(String),
}

impl Indexing {
    /// The word `sys` prints: `on`, `off` or `unknown`.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Enabled => "on",
            Self::Disabled => "off",
            Self::Unknown(_) => "unknown",
        }
    }

    /// The word `sys --json` carries: `enabled`, `disabled` or `unknown`.
    #[must_use]
    pub fn json_word(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// One volume `mdutil` reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Its mount point, as printed (`/System/Volumes/Data`).
    pub path: String,
    /// Its indexing state.
    pub indexing: Indexing,
}

/// Where the root broker's walk of a store stopped short: what it counted is a lower bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreCut {
    /// At its most entries.
    Entries,
    /// At its longest time.
    Time,
    /// At a folder or entry it could not read (or a folder deeper than it enters).
    Unreadable,
}

impl StoreCut {
    /// Its word: `entries`, `time` or `unreadable`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Entries => "entries",
            Self::Time => "time",
            Self::Unreadable => "unreadable",
        }
    }

    /// The reason a cannot line gives where every store was cut by it:
    /// `unavailable:partial_<word>`, an unavailable word ([`crate::sys::UNAVAILABLE_WORDS`]).
    #[must_use]
    pub fn partial_why(self) -> &'static str {
        match self {
            Self::Entries => "unavailable:partial_entries",
            Self::Time => "unavailable:partial_time",
            Self::Unreadable => "unavailable:partial_unreadable",
        }
    }
}

/// One volume's Spotlight store as the root broker walked it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    /// The volume's mount point.
    pub volume: String,
    /// The store's size on disk, bytes: its entries' allocated blocks.
    pub bytes: u64,
    /// The entries counted.
    pub entries: u64,
    /// Where the walk stopped short; `None` for the whole store.
    pub cut: Option<StoreCut>,
}

/// The root broker's answer as the host last read it: each volume's store, and when the walk
/// behind it ended, ms on the host's monotonic clock (the answer's age taken off the time the
/// host asked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreRead {
    /// Each local volume with a store.
    pub stores: Vec<Store>,
    /// When the walk ended, ms.
    pub at_ms: u64,
}

/// The family's state, the `await sys spotlight=` levels in rank order. [`state`] reads them in
/// this order of precedence: indexing, then off, then idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Under both bars, and some volume read is not disabled (or none was read).
    Idle,
    /// At or over either bar, whatever the volumes read: the family is working.
    Indexing,
    /// Under both bars, and every volume read reads indexing disabled.
    Off,
}

impl State {
    /// The levels, by rank: what `await sys spotlight=` takes.
    pub const WORDS: [&'static str; 3] = ["idle", "indexing", "off"];

    /// Its word.
    #[must_use]
    pub fn word(self) -> &'static str {
        Self::WORDS[self.rank()]
    }

    /// Its rank in [`Self::WORDS`].
    #[must_use]
    pub fn rank(self) -> usize {
        match self {
            Self::Idle => 0,
            Self::Indexing => 1,
            Self::Off => 2,
        }
    }
}

/// One family coalition's share of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    /// Its lead's signing identifier, one of [`MEMBERS`].
    pub signing_id: &'static str,
    /// Its lead's pid.
    pub lead_pid: u32,
    /// Its coalition's reported CPU counter growth over the window, millicores.
    pub cpu_mc: i64,
    /// What it read from storage, bytes per second.
    pub read_bps: u64,
    /// Its live members' footprint, bytes, where read.
    pub footprint: Option<u64>,
    /// Its live members in the reading's sweep: how many processes it is now.
    pub live: u32,
    /// Its CPU since it began, ns ([`CoalRow::cpu_total_ns`]): exited members' work included.
    pub cpu_total_ns: u64,
}

/// The family over one split window, with the volumes as last read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The split's wall clock, unix ms.
    pub at_unix_ms: i64,
    /// The window, ms.
    pub window_ms: u64,
    /// The family's reported CPU counter growth over the window, millicores: published
    /// counters, which neither floor nor bound the CPU it ran inside the window
    /// ([`crate::published_cpu`]).
    pub cpu_mc: i64,
    /// What it read from storage, bytes per second.
    pub read_bps: u64,
    /// Its footprint: the shares' footprints summed, with how many shares were unread.
    pub footprint: MemoryCharge,
    /// Each coalition's share, busiest first.
    pub shares: Vec<Share>,
    /// Each volume `mdutil -s -a` reported, where read and no older than
    /// [`VOLUMES_STALE_MS`].
    pub volumes: Option<Vec<Volume>>,
    /// How old [`Self::volumes`] is at the reading, ms.
    pub volumes_age_ms: Option<u64>,
    /// Each volume's store as the root broker last walked it, where read and no older than
    /// [`STORE_STALE_MS`].
    pub store: Option<Vec<Store>>,
    /// How old [`Self::store`] is at the reading, ms.
    pub store_age_ms: Option<u64>,
    /// How old the root broker's last answer is at the reading, ms, where it is older than
    /// [`STORE_STALE_MS`] and so not carried in [`Self::store`]: it answered, long ago.
    pub store_stale_ms: Option<u64>,
    /// Its state ([`state`]).
    pub state: State,
}

/// The volumes `mdutil -s -a` (or `mdutil -s <volume>`) printed: each line that does not start
/// with whitespace and ends with `:` opens a volume's block, and each other line, trimmed, is
/// that volume's status. A block of no status line reads [`Indexing::Unknown`] `""`; one of
/// several that all read the same state reads that state, and one whose status lines differ
/// reads [`Indexing::Unknown`] holding its lines joined by `; `, never disabled.
///
/// # Errors
/// Nothing was printed, or a status line came before any volume.
pub fn volumes(text: &str) -> Result<Vec<Volume>, String> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let header = !line.starts_with(char::is_whitespace) && line.trim_end().ends_with(':');
        if header {
            let path = line.trim_end().trim_end_matches(':').to_owned();
            out.push((path, Vec::new()));
        } else {
            let Some((_, status)) = out.last_mut() else {
                return Err(format!(
                    "mdutil: a status before any volume: {}",
                    line.trim()
                ));
            };
            status.push(line.trim().to_owned());
        }
    }
    if out.is_empty() {
        return Err("mdutil printed no volume".into());
    }
    Ok(out
        .into_iter()
        .map(|(path, lines)| {
            let states: Vec<Indexing> = lines.iter().map(|l| status(l)).collect();
            let indexing = match states.as_slice() {
                [] => Indexing::Unknown(String::new()),
                [one] => one.clone(),
                many if many.iter().all(|s| *s == many[0]) => many[0].clone(),
                _ => Indexing::Unknown(lines.join("; ")),
            };
            Volume { path, indexing }
        })
        .collect())
}

fn status(line: &str) -> Indexing {
    match line {
        "Indexing enabled." => Indexing::Enabled,
        "Indexing disabled." | "Indexing and searching disabled." => Indexing::Disabled,
        other => Indexing::Unknown(other.to_owned()),
    }
}

/// Whether the family's `cpu_mc` and `read_bps` are at or over either bar.
#[must_use]
pub fn over_a_bar(cpu_mc: i64, read_bps: u64) -> bool {
    cpu_mc >= CPU_BAR_MC || read_bps >= READ_BAR_BPS
}

/// The family's state: [`State::Indexing`] at or over either bar, whatever the volumes read (a
/// busy family is working, and `corespotlightd` and `spotlightknowledged` index apart from the
/// volumes' indexing); else [`State::Off`] where volumes were read, there are some, and every
/// one reads disabled; else [`State::Idle`]. Volumes unread never make it off.
#[must_use]
pub fn state(cpu_mc: i64, read_bps: u64, volumes: Option<&[Volume]>) -> State {
    if over_a_bar(cpu_mc, read_bps) {
        State::Indexing
    } else if volumes
        .is_some_and(|v| !v.is_empty() && v.iter().all(|v| v.indexing == Indexing::Disabled))
    {
        State::Off
    } else {
        State::Idle
    }
}

/// The volumes as last read: what they were and when, ms on the host's monotonic clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumesRead {
    /// Each volume.
    pub volumes: Vec<Volume>,
    /// When `mdutil` ran, ms.
    pub at_ms: u64,
}

/// The family over a split's coalition `rows` (the window `window_ms` long ending at
/// `at_unix_ms`), with `volumes` as last read and `now_ms` the reading's monotonic clock. A
/// row counts where [`family_lead`] names it the family's.
#[must_use]
pub fn reading(
    rows: &[CoalRow],
    volumes: Option<&VolumesRead>,
    now_ms: u64,
    at_unix_ms: i64,
    window_ms: u64,
) -> Reading {
    reading_of(rows, MEMBERS, volumes, now_ms, at_unix_ms, window_ms)
}

/// The lead of `r` where the row is the family's whose signing ids are `members`: its
/// ownership is not mixed (a mixed coalition's growth has no single owner), and its lead is
/// Apple-signed under one of `members`, matched exactly.
fn family_lead<'r>(r: &'r CoalRow, members: &[&str]) -> Option<&'r Lead> {
    r.lead.as_ref().filter(|l| {
        !r.ownership_mixed
            && l.voucher == Voucher::Apple
            && l.signing_id
                .as_deref()
                .is_some_and(|id| members.contains(&id))
    })
}

/// [`reading`] of the family whose signing ids are `members`.
pub(crate) fn reading_of(
    rows: &[CoalRow],
    members: &[&'static str],
    volumes: Option<&VolumesRead>,
    now_ms: u64,
    at_unix_ms: i64,
    window_ms: u64,
) -> Reading {
    let mut shares: Vec<Share> = rows
        .iter()
        .filter_map(|r| {
            let lead = family_lead(r, members)?;
            let id = lead.signing_id.as_deref()?;
            let signing_id = members.iter().copied().find(|m| *m == id)?;
            Some(Share {
                signing_id,
                lead_pid: lead.pid,
                cpu_mc: r.cpu_mc,
                read_bps: r.read_bps,
                footprint: r.footprint,
                live: r.live,
                cpu_total_ns: r.cpu_total_ns,
            })
        })
        .collect();
    shares.sort_by(|a, b| b.cpu_mc.cmp(&a.cpu_mc).then(a.lead_pid.cmp(&b.lead_pid)));
    let cpu_mc = shares.iter().map(|s| s.cpu_mc).sum();
    let read_bps = shares
        .iter()
        .fold(0u64, |t, s| t.saturating_add(s.read_bps));
    let mut footprint = MemoryCharge::default();
    for s in &shares {
        footprint.add(s.footprint);
    }
    Reading {
        at_unix_ms,
        window_ms,
        cpu_mc,
        read_bps,
        footprint,
        shares,
        volumes: None,
        volumes_age_ms: None,
        store: None,
        store_age_ms: None,
        store_stale_ms: None,
        state: State::Idle,
    }
    .with_volumes(volumes, now_ms)
}

/// The Spotlight activity's cannot lines (`index_progress`, `index_folder`) for reading `r`. Each
/// one [`STORE_MEETS`] is dropped where `r` carries the root broker's store answer and that
/// answer read a store whole (or found none on a mount table read whole); where every store it
/// lists was cut short, the line stays, its reason the bound that cut the first
/// (`why=unavailable:partial_time`), as a lower bound alone does not read the folder.
#[must_use]
pub fn cannot(r: &Reading) -> Vec<(&'static str, &'static str)> {
    let met = r.store.as_ref().map(|stores| {
        if stores.is_empty() || stores.iter().any(|s| s.cut.is_none()) {
            None
        } else {
            stores.iter().find_map(|s| s.cut)
        }
    });
    crate::explain::ActivityId::Spotlight
        .activity()
        .cannot
        .iter()
        .filter_map(|&(what, why)| match met {
            Some(cut) if STORE_MEETS.contains(&what) => cut.map(|cut| (what, cut.partial_why())),
            _ => Some((what, why)),
        })
        .collect()
}

/// The port `mdutil` is run through, with [`MDUTIL_ARGS`]: the host's live arm runs it under a
/// short deadline; a test's arm counts.
#[derive(Clone, Copy, Debug)]
pub struct MdutilProbe {
    /// What `mdutil -s -a` printed, from a run that exited 0; one line why not otherwise.
    pub status_all: fn() -> Result<String, String>,
}

impl MdutilProbe {
    /// Runs nothing: every volume stays unread.
    pub const INERT: Self = Self {
        status_all: || Err("inert".into()),
    };
}

/// When `mdutil -s -a` runs: at the first reading, then at most once every
/// [`VOLUMES_EVERY_MS`] while the family is busy or crossed a bar since the last run, and every
/// [`VOLUMES_QUIET_MS`] while it stays quiet. Never twice inside [`VOLUMES_EVERY_MS`].
#[derive(Clone, Debug)]
pub struct Cadence {
    /// The floor between runs, ms: [`VOLUMES_EVERY_MS`].
    every_ms: u64,
    /// The gap between runs while quiet, ms: [`VOLUMES_QUIET_MS`].
    quiet_ms: u64,
    last_run_ms: Option<u64>,
    crossed: bool,
    last: Option<VolumesRead>,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            every_ms: VOLUMES_EVERY_MS,
            quiet_ms: VOLUMES_QUIET_MS,
            last_run_ms: None,
            crossed: false,
            last: None,
        }
    }
}

impl Cadence {
    /// Whether a run is due at `now_ms`.
    #[must_use]
    pub fn due(&self, now_ms: u64) -> bool {
        let Some(last) = self.last_run_ms else {
            return true;
        };
        let since = now_ms.saturating_sub(last);
        since >= self.every_ms && (self.crossed || since >= self.quiet_ms)
    }

    /// Notes the family `busy` (at or over a bar) at `now_ms`, runs `probe` where a run is due,
    /// and returns the volumes as last read. A run that fails or prints what [`volumes`] cannot
    /// read leaves them unread until the next run.
    pub fn step(&mut self, now_ms: u64, busy: bool, probe: &MdutilProbe) -> Option<&VolumesRead> {
        self.crossed |= busy;
        if self.due(now_ms) {
            self.last_run_ms = Some(now_ms);
            self.crossed = false;
            self.last = (probe.status_all)()
                .and_then(|text| volumes(&text))
                .ok()
                .map(|volumes| VolumesRead {
                    volumes,
                    at_ms: now_ms,
                });
        }
        self.last.as_ref()
    }

    /// The volumes as last read.
    #[must_use]
    pub fn last(&self) -> Option<&VolumesRead> {
        self.last.as_ref()
    }
}

/// How often the host asks the root broker for Spotlight's stores, ms: the broker's own walk
/// cadence (`aterm_lan`'s `WALK_EVERY`, ten minutes), so each ask can read a new walk and no ask
/// makes the broker walk more often.
pub const STORE_ASK_EVERY_MS: u64 = 600_000;

/// The root broker's store answer as its port reads it: each store and the answer's age, ms; or
/// one line why there is none.
pub type StoreAnswer = Result<(Vec<Store>, u64), String>;

/// The port the root broker is asked through ([`StoreAsk`]): the host's live arm asks
/// `aterm_uds::lan::spotlight_stores`; `None` asks nothing (a unit test's inert arm, and a host
/// with no Spotlight).
#[derive(Clone, Copy, Debug)]
pub struct StoreProbe {
    /// Ask the broker, blocking until it answers or its client gives up.
    pub ask: Option<fn() -> StoreAnswer>,
}

impl StoreProbe {
    /// Asks nothing.
    pub const INERT: Self = Self { ask: None };
}

/// When the host asks aterm's root LAN broker for Spotlight's stores (SD-94): at most once every
/// [`STORE_ASK_EVERY_MS`], one ask at a time, each on a thread of its own, so a broker slow to
/// answer (a walk waits at most its bound; a broker gone, its client's own hang bound) never
/// holds the reading. An ask that fails (no broker installed, one too old to know the request,
/// or the broker's error) is no answer: nothing is logged, and it is asked again at the next
/// turn.
#[derive(Debug, Default)]
pub struct StoreAsk {
    asked_ms: Option<u64>,
    pending: Option<(u64, std::sync::mpsc::Receiver<StoreAnswer>)>,
}

impl StoreAsk {
    /// At mono `now_ms`: the answer of the ask in flight, where it arrived; else, where an ask
    /// is due and `probe` asks, a new ask, whose answer a later step returns.
    pub fn step(&mut self, now_ms: u64, probe: &StoreProbe) -> Option<StoreRead> {
        if let Some((asked_ms, answer)) = &self.pending {
            let asked_ms = *asked_ms;
            return match answer.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Ok(answer) => {
                    self.pending = None;
                    Self::read(answer, asked_ms)
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    None
                }
            };
        }
        let ask = probe.ask?;
        if self
            .asked_ms
            .is_some_and(|at| now_ms.saturating_sub(at) < STORE_ASK_EVERY_MS)
        {
            return None;
        }
        self.asked_ms = Some(now_ms);
        let (answered, answer) = std::sync::mpsc::channel();
        let started = std::thread::Builder::new()
            .name("vitals-spotlight-store".into())
            .spawn(move || {
                // The step that would read it dropped its end: nothing is waiting.
                let _ = answered.send(ask());
            });
        if started.is_ok() {
            self.pending = Some((now_ms, answer));
        }
        None
    }

    /// `answer` as read by an ask made at mono `asked_ms`: its walk ended its age before the
    /// broker answered, which was no earlier than the ask, so the walk is dated no later than it
    /// ended.
    #[must_use]
    pub fn read(answer: StoreAnswer, asked_ms: u64) -> Option<StoreRead> {
        answer.ok().map(|(stores, age_ms)| StoreRead {
            stores,
            at_ms: asked_ms.saturating_sub(age_ms),
        })
    }
}

/// What the model keeps of the family between readings: the last split reading with the mono
/// ms it was made at, carried for [`READING_CARRIED_MS`], and the day's history.
#[derive(Clone, Debug, Default)]
pub struct Family {
    last: Option<(Reading, u64)>,
    /// Shared with every snapshot that carries it: a reading takes a reference, and a note
    /// copies the rings only while a snapshot still holds the previous day.
    history: std::sync::Arc<crate::history::SpotlightHistory>,
    /// The root broker's store answer as the host last handed it in ([`Self::note_store`]).
    store: Option<StoreRead>,
}

impl Family {
    /// The family at a reading of mono `now_ms`, with `volumes` as last read: from `fresh`'s
    /// rows where this reading made a split by coalition (which the history keeps), else the
    /// last split reading carried, its volumes refreshed; `None` before the first and once the
    /// last was made more than [`READING_CARRIED_MS`] before `now_ms` on the monotonic clock,
    /// so a wall clock set back never carries it longer.
    ///
    /// The history takes only split readings, which the host makes while a window in view
    /// shows the bars, or a `sys` query, a `sys why` or a spotlight wait asks for one: a span
    /// with none of those holds no bucket, which reads unread, never idle.
    pub fn absorb(
        &mut self,
        fresh: Option<&crate::explain::WhyFacts>,
        volumes: Option<&VolumesRead>,
        now_ms: u64,
    ) -> Option<Reading> {
        let store = self.store.as_ref();
        if let Some(f) = fresh {
            let r = reading(
                &f.split.rows,
                volumes,
                now_ms,
                f.at_unix_ms,
                f.window_ns / 1_000_000,
            )
            .with_store(store, now_ms);
            std::sync::Arc::make_mut(&mut self.history).note(&r);
            self.last = Some((r, now_ms));
        } else {
            self.last = self
                .last
                .take()
                .filter(|(_, made_ms)| now_ms.saturating_sub(*made_ms) <= READING_CARRIED_MS)
                .map(|(r, made_ms)| {
                    (
                        r.with_volumes(volumes, now_ms).with_store(store, now_ms),
                        made_ms,
                    )
                });
        }
        self.last.as_ref().map(|(r, _)| r.clone())
    }

    /// Whether the last reading has the family at or over a bar.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.last
            .as_ref()
            .is_some_and(|(r, _)| over_a_bar(r.cpu_mc, r.read_bps))
    }

    /// The day's history.
    #[must_use]
    pub fn history(&self) -> &std::sync::Arc<crate::history::SpotlightHistory> {
        &self.history
    }

    /// Take the history the host kept in an earlier run.
    pub fn restore_history(&mut self, history: crate::history::SpotlightHistory) {
        self.history = std::sync::Arc::new(history);
    }

    /// Take the root broker's store answer the host read; later readings carry it while it is
    /// no older than [`STORE_STALE_MS`].
    pub fn note_store(&mut self, store: StoreRead) {
        self.store = Some(store);
    }
}

impl Reading {
    /// The family's footprint, bytes, where every share's was read (`0` with no share); `None`
    /// where any was not, since the sum is then a lower bound.
    #[must_use]
    pub fn footprint_bytes(&self) -> Option<u64> {
        (self.footprint.unread == 0).then_some(self.footprint.bytes)
    }

    /// Whether no share's footprint was read, though there are shares.
    #[must_use]
    pub fn footprint_unread(&self) -> bool {
        self.footprint.members > 0 && self.footprint.unread == self.footprint.members
    }

    /// The index's size on disk: every store's bytes summed, and whether any walk stopped short
    /// (the sum is then a lower bound). `None` without the root broker's answer.
    #[must_use]
    pub fn index_bytes(&self) -> Option<(u64, Option<StoreCut>)> {
        let store = self.store.as_ref()?;
        let bytes = store.iter().fold(0u64, |t, s| t.saturating_add(s.bytes));
        Some((bytes, store.iter().find_map(|s| s.cut)))
    }

    /// The same figures with the root broker's `store` answer as read at mono `now_ms`, where it
    /// is no older than [`STORE_STALE_MS`]; an older one leaves only its age
    /// ([`Self::store_stale_ms`]).
    #[must_use]
    pub fn with_store(mut self, store: Option<&StoreRead>, now_ms: u64) -> Self {
        let age = |s: &StoreRead| now_ms.saturating_sub(s.at_ms);
        let fresh = store.filter(|s| age(s) <= STORE_STALE_MS);
        self.store = fresh.map(|s| s.stores.clone());
        self.store_age_ms = fresh.map(age);
        self.store_stale_ms = store.map(age).filter(|ms| *ms > STORE_STALE_MS);
        self
    }

    /// The same figures with `volumes` as read at mono `now_ms`, and the state they give.
    #[must_use]
    pub fn with_volumes(mut self, volumes: Option<&VolumesRead>, now_ms: u64) -> Self {
        let fresh = volumes.filter(|v| now_ms.saturating_sub(v.at_ms) <= VOLUMES_STALE_MS);
        self.volumes = fresh.map(|v| v.volumes.clone());
        self.volumes_age_ms = fresh.map(|v| now_ms.saturating_sub(v.at_ms));
        self.state = state(self.cpu_mc, self.read_bps, self.volumes.as_deref());
        self
    }
}

#[cfg(test)]
#[path = "spotlight_tests.rs"]
mod tests;
