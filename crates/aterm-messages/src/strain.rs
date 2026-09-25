// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STRAIN — explaining very heavy system use on the band (design §10.14,
//! rulings 206–212). §10.6 covers aterm's OWN heavy jobs; this is a machine
//! loaded by something else.
//!
//! # The rule (ruling 206)
//!
//! The glass carries a strain row only while all four hold:
//!
//! 1. **FELT** — aterm measured slow HARDWARE input in a focused, on-screen
//!    window ([`StrainTracker::note_key`], [`StrainTracker::note_freeze`],
//!    [`StrainTracker::felt`]).
//! 2. **HEAVY** — a resource is past its enter line in two consecutive
//!    readings ([`StrainTracker::observe`], [`StrainTracker::verdict`]).
//! 3. **NAMED** — the cause is named from what was measured ([`group`]).
//! 4. **NOT OWN** — the cause is not the session receiving the keys, not an
//!    aterm job that already has a row, and never aterm itself.
//!
//! Anything else is a record ([`Hold::LogOnly`]) or nothing. Load average and
//! swap totals are never inputs. Nothing is sampled until FELT is suspected:
//! a calm tracker schedules nothing ([`StrainTracker::next_sample`]).
//!
//! # Split
//!
//! This module is the whole policy — pure, clockless (every instant is the
//! host's), integer-only and platform-free. The samplers (`aterm-sysprobe`)
//! read the machine into a [`Reading`] and a list of [`ProcRow`]s; the host
//! (`strain_host.rs`) feeds typing samples, arms the one deadline
//! [`StrainTracker::next_sample`] names, and performs the [`StrainOut`]:
//! a post, a restate, a fold (a withdraw — a Vanish, never ✓ — then its
//! record) or a record.
//!
//! # Honest names
//!
//! An unprivileged process cannot read another user's CPU time (on this m1
//! 224 of 506 pids answer EPERM: `mds_stores`, `WindowServer`, `kernel_task`
//! among them). Such time is never pinned on a daemon: it is the machine's
//! busy time minus everything visible, and its name is
//! [`StrainConfig::services_noun`] — `macOS services`.

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use crate::model::{Hold, Load, Message, MessageId, Meter, Severity, tags};
use crate::text::{clip, truncate};
use crate::{
    Duration, FEEL_MIN_HITCHES, FEEL_MIN_KEYS, FEEL_RING, FEEL_WINDOW, FREEZE_MS,
    GLASS_TITLE_CHARS, GLASS_TITLE_WORDS, HITCH_MS, Instant, PIECE_SEP, SLOW_KEY_MS, STALE_STRAIN,
    STRAIN_CLEAR_READINGS, STRAIN_CRITICAL_EVERY, STRAIN_ENTER_READINGS, STRAIN_GLASS_MAX,
    STRAIN_IDLE, STRAIN_KEEPALIVE, STRAIN_KEY, STRAIN_MIN_GLASS, STRAIN_NAMED_MEMORY_PCT,
    STRAIN_NAMED_MILLICORES, STRAIN_NAMED_SHARE_PCT, STRAIN_NO_KEY_CALM, STRAIN_ONSET,
    STRAIN_QUIET_ANY, STRAIN_QUIET_SAME, STRAIN_RECORD_AFTER, STRAIN_RECORD_WINDOW,
    STRAIN_RECORDS_PER_WINDOW, STRAIN_RESTATE_PERMILLE, STRAIN_SAMPLE_EVERY, STRAIN_SCAN_EVERY,
    STRAIN_UNFELT_CALM, STRAIN_UNFELT_FOLD,
};

// ---------------------------------------------------------------------------
// Inputs: what a sampler reads.
// ---------------------------------------------------------------------------

/// The kernel's memory-pressure level (macOS `kern.memorystatus_vm_pressure_level`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryLevel {
    /// Nothing to reclaim.
    Normal,
    /// The kernel is compressing and swapping.
    Warn,
    /// The kernel is about to kill processes.
    Critical,
}

impl MemoryLevel {
    /// The Details word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

/// The machine's thermal state (`NSProcessInfo.thermalState`; on Linux a
/// zone past its passive trip reads [`Thermal::Serious`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Thermal {
    /// Cool.
    Nominal,
    /// Warm; nothing throttled.
    Fair,
    /// Throttled.
    Serious,
    /// Throttled hard.
    Critical,
}

impl Thermal {
    /// The Details word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Fair => "fair",
            Self::Serious => "serious",
            Self::Critical => "critical",
        }
    }
}

/// Linux pressure-stall information, each the `avg10` share in permille;
/// `None` where the kernel has no such file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Psi {
    /// `/proc/pressure/cpu` `some`.
    pub cpu_some_pm: Option<u16>,
    /// `/proc/pressure/memory` `some`.
    pub memory_some_pm: Option<u16>,
    /// `/proc/pressure/memory` `full`.
    pub memory_full_pm: Option<u16>,
    /// `/proc/pressure/io` `full`.
    pub io_full_pm: Option<u16>,
}

/// One reading of the machine. Every field a platform cannot read is `None`,
/// and a `None` field is never heavy. Counters are CUMULATIVE: the tracker
/// takes the deltas between readings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// When it was read (the host's clock).
    pub at: Instant,
    /// Logical cores.
    pub cores: u16,
    /// Physical memory, MiB (`hw.memsize`).
    pub mem_mib: u32,
    /// `(busy, total)` CPU ticks over all cores since boot.
    pub busy_ticks: Option<(u64, u64)>,
    /// The kernel's pressure level.
    pub pressure: Option<MemoryLevel>,
    /// The memory gauge, permille: how little the kernel has left
    /// (`1000 − 10·memorystatus_level` — that level is the share still
    /// FREE, so this is a pressure gauge, not a count of bytes in use).
    pub mem_used_pm: Option<u16>,
    /// Memory in use, MiB, as Activity Monitor counts it (app memory +
    /// wired + compressed; Linux: total − available). The words `N GB` and
    /// `N GB in use` read this and are left out when it is `None`.
    pub mem_used_mib: Option<u32>,
    /// Pages swapped in plus out since boot.
    pub swap_pages: Option<u64>,
    /// KiB per page.
    pub page_kib: u32,
    /// The thermal state.
    pub thermal: Option<Thermal>,
    /// Low Power Mode.
    pub low_power: Option<bool>,
    /// Linux PSI.
    pub psi: Option<Psi>,
}

impl Reading {
    /// A reading of nothing — every field `None` — at `at`: what a platform
    /// with no sampler reads.
    #[must_use]
    pub fn empty(at: Instant) -> Self {
        Self {
            at,
            cores: 0,
            mem_mib: 0,
            busy_ticks: None,
            pressure: None,
            mem_used_pm: None,
            mem_used_mib: None,
            swap_pages: None,
            page_kib: 0,
            thermal: None,
            low_power: None,
            psi: None,
        }
    }
}

/// One process in a sweep. `cpu_ns` is the process's cumulative CPU time
/// (user + system), `None` where the kernel refused it (EPERM: another
/// user's process); such time counts toward [`Culprit::Services`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcRow {
    /// The pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// Owned by the person running aterm.
    pub uid_is_ours: bool,
    /// The short command name (`comm`, at most 16 characters).
    pub name: String,
    /// The executable's path, where it was read (the top few only).
    pub bundle: Option<String>,
    /// Cumulative CPU nanoseconds, `None` when unreadable.
    pub cpu_ns: Option<u64>,
    /// Physical footprint, KiB, `None` when unreadable.
    pub footprint_kib: Option<u64>,
}

/// One aterm session, as the host knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRef {
    /// The session's shell.
    pub shell_pid: u32,
    /// Its foreground program's name.
    pub program: String,
    /// Its tab number in its window, 1-based.
    pub tab: u16,
    /// The session the person is typing into.
    pub receiving_keys: bool,
    /// In another window than the focused one.
    pub elsewhere: bool,
}

/// An aterm job that already has a live row (its load words explain it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobRef<'a> {
    /// The job's process.
    pub pid: u32,
    /// Its row.
    pub id: MessageId,
    /// Its row's title.
    pub title: &'a str,
}

// ---------------------------------------------------------------------------
// Causes.
// ---------------------------------------------------------------------------

/// A closed map of well-known background work, matched on the command name
/// of a process the person owns (never another user's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// `fileproviderd`.
    FileSync,
    /// `bird`, `cloudd`.
    ICloudSync,
    /// `photoanalysisd`, `mediaanalysisd`.
    PhotosAnalysis,
    /// `mdworker_shared`.
    SpotlightIndexing,
    /// Linux: `tracker-miner-fs-3`, `baloo_file`.
    FileIndexing,
    /// Linux: `packagekitd`.
    SystemUpdate,
}

impl Family {
    /// The family of a command name, if it is one.
    #[must_use]
    pub fn of(name: &str) -> Option<Self> {
        Some(match name {
            "fileproviderd" => Self::FileSync,
            "bird" | "cloudd" => Self::ICloudSync,
            "photoanalysisd" | "mediaanalysisd" => Self::PhotosAnalysis,
            "mdworker_shared" => Self::SpotlightIndexing,
            "tracker-miner-fs-3" | "baloo_file" => Self::FileIndexing,
            "packagekitd" => Self::SystemUpdate,
            _ => return None,
        })
    }

    /// The band's words for it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::FileSync => "file sync",
            Self::ICloudSync => "iCloud sync",
            Self::PhotosAnalysis => "Photos analysis",
            Self::SpotlightIndexing => "Spotlight indexing",
            Self::FileIndexing => "file indexing",
            Self::SystemUpdate => "system update",
        }
    }
}

/// Who the load is: one group of processes ([`group`]), or the resource
/// itself when no group is big enough to name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Culprit {
    /// Processes under an aterm session's shell.
    Session {
        /// What is heavy there: the session's foreground program when the
        /// heaviest process is it or runs under it, else the heaviest
        /// process's own name ([`group`]) — never an idle shell.
        program: String,
        /// Its tab.
        tab: u16,
        /// In another window.
        elsewhere: bool,
        /// The person is typing into it: their own command, never explained.
        receiving_keys: bool,
    },
    /// Under an aterm job that already has this row.
    OwnJob(MessageId),
    /// aterm itself: a defect to fix, never explained on the band.
    Aterm,
    /// An app bundle, helpers summed in.
    App(String),
    /// A known family of background work.
    Family(Family),
    /// A lone process, by its command name.
    Process(String),
    /// Busy time aterm cannot attribute: the machine's own services.
    Services,
    /// No group is big enough to name: the title names the resource.
    Resource,
}

/// The resource under strain, highest priority first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StrainKind {
    /// Memory full, swapping.
    Memory,
    /// Throttled by heat.
    Heat,
    /// Every core busy.
    Cpu,
    /// I/O stalled (Linux PSI).
    Disk,
    /// Low Power Mode under load.
    LowPower,
}

impl StrainKind {
    /// Every kind, highest priority first.
    pub const ALL: [StrainKind; 5] = [
        Self::Memory,
        Self::Heat,
        Self::Cpu,
        Self::Disk,
        Self::LowPower,
    ];

    /// The priority: higher outranks lower.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Memory => 4,
            Self::Heat => 3,
            Self::Cpu => 2,
            Self::Disk => 1,
            Self::LowPower => 0,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Memory => 0,
            Self::Heat => 1,
            Self::Cpu => 2,
            Self::Disk => 3,
            Self::LowPower => 4,
        }
    }

    /// The title's words when no culprit is named.
    #[must_use]
    pub const fn resource_words(self) -> &'static str {
        match self {
            Self::Memory => "low memory",
            Self::Heat => "heat",
            Self::Cpu => "CPU load",
            Self::Disk => "disk load",
            Self::LowPower => "Low Power Mode",
        }
    }

    /// The load slot's words when a culprit is named.
    #[must_use]
    pub const fn load(self) -> Load {
        match self {
            Self::Memory => Load::Memory,
            Self::Disk => Load::Disk,
            Self::Heat | Self::Cpu | Self::LowPower => Load::Cpu,
        }
    }
}

/// Whether the tracker may sample: the switch is on and some window is
/// focused and on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    /// `explain_heavy_load` (ruling 212).
    pub enabled: bool,
    /// Some window is focused and its band is on screen.
    pub focused_on_screen: bool,
}

impl Gate {
    /// Both hold.
    #[must_use]
    pub const fn open(self) -> bool {
        self.enabled && self.focused_on_screen
    }
}

/// The platform's name for unattributable busy time on macOS.
pub const MACOS_SERVICES: &str = "macOS services";
/// …and everywhere else.
pub const SYSTEM_SERVICES: &str = "system services";

/// What the host tells the tracker once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrainConfig {
    /// [`MACOS_SERVICES`] or [`SYSTEM_SERVICES`].
    pub services_noun: &'static str,
    /// Rows on the glass; `false` writes the records only (calibration).
    pub glass: bool,
    /// aterm's own pid (0: unknown).
    pub self_pid: u32,
}

/// What the host performs after a tracker call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrainOut {
    /// Nothing.
    None,
    /// Post the strain row ([`STRAIN_KEY`]).
    Post(Message),
    /// Restate the strain row in place (an identical one re-arms its
    /// staleness cap and repaints nothing).
    Restate(Message),
    /// Withdraw the strain row (a Vanish), then post the record, if any (the
    /// record cap may have swallowed it).
    Fold {
        /// The episode's record.
        record: Option<Message>,
    },
    /// Post a record (never on the glass).
    Record(Message),
}

/// Why an episode did not reach the glass: the closed list its record's
/// `not shown:` line reads from (ruling 209).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotShown {
    /// Nobody was typing.
    NoTyping,
    /// The load was the session the person typed into.
    OwnCommand,
    /// An aterm job's own row explains it.
    ExplainedBy(String),
    /// A strain row folded not long ago.
    Quiet,
    /// Typing was slow and nothing was heavy.
    NoHeavyCause,
    /// aterm itself was the load: a defect, never explained.
    AtermItself,
    /// The glass is off (the records-only rollout).
    RecordsOnly,
}

impl NotShown {
    /// The words after `not shown: `.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::NoTyping => "no typing".into(),
            Self::OwnCommand => "your own command".into(),
            Self::ExplainedBy(title) => format!("explained by {title}"),
            Self::Quiet => "quiet after last episode".into(),
            Self::NoHeavyCause => "no heavy cause".into(),
            Self::AtermItself => "aterm itself".into(),
            Self::RecordsOnly => "records only".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Grouping (pure).
// ---------------------------------------------------------------------------

/// A CPU leader is taken only from a sweep whose machine was at least this
/// busy (permille): the lowest line a CPU-derived kind may still hold at
/// (Low Power Mode's clear line).
const LOADED_BUSY_PM: u16 = 450;
/// The ppid chain is walked at most this far.
const CHAIN_HOPS: usize = 16;
/// A sweep is TORN — a baseline only — when processes that burned at least
/// this much (milli-cores, at the last sweep) exited inside its window…
const TORN_SWEEP_MC: u32 = 500;
/// …and that is at least this share (percent) of the machine's busy time:
/// the load itself went away. Less than that is churn (a build starting
/// and ending compilers), and the exits' time is credited to their groups.
const TORN_SHARE_PCT: u32 = 75;
/// A command name is clipped to this.
const COMM_CHARS: usize = 16;
/// An app's name is clipped to this.
const APP_CHARS: usize = 20;

/// One group's measured load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Who.
    pub culprit: Culprit,
    /// CPU nanoseconds summed over its measured processes.
    pub cpu_ns: u64,
    /// Footprint KiB summed.
    pub footprint_kib: u64,
}

/// The `Name` of the first `/<Name>.app/` in `path`.
fn app_name(path: &str) -> Option<&str> {
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len().saturating_sub(1);
    parts
        .iter()
        .take(last)
        .find_map(|p| p.strip_suffix(".app"))
        .filter(|n| !n.is_empty())
}

/// Which group `row` belongs to — the first match wins: a session (the ppid
/// chain reaches its shell), an aterm job with a row, aterm itself, another
/// user's process (the services), an app bundle, a known family, the lone
/// process. A session's match also says WHICH session (its index), so the
/// session is named from its own shell, never looked up again by its words.
fn classify(
    row: &ProcRow,
    by_pid: &HashMap<u32, &ProcRow>,
    sessions: &[SessionRef],
    own: &[JobRef<'_>],
    self_pid: u32,
) -> (Culprit, Option<usize>) {
    let mut job = None;
    let mut aterm = false;
    let mut pid = row.pid;
    for _ in 0..=CHAIN_HOPS {
        if let Some((i, s)) = sessions
            .iter()
            .enumerate()
            .find(|(_, s)| s.shell_pid == pid)
        {
            let culprit = Culprit::Session {
                program: clip(&s.program, COMM_CHARS),
                tab: s.tab,
                elsewhere: s.elsewhere,
                receiving_keys: s.receiving_keys,
            };
            return (culprit, Some(i));
        }
        if job.is_none() {
            job = own.iter().find(|j| j.pid == pid).map(|j| j.id);
        }
        if self_pid != 0 && pid == self_pid {
            aterm = true;
            break;
        }
        match by_pid.get(&pid) {
            Some(r) if r.ppid != pid && r.ppid != 0 => pid = r.ppid,
            _ => break,
        }
    }
    let culprit = if let Some(id) = job {
        Culprit::OwnJob(id)
    } else if aterm {
        Culprit::Aterm
    } else if !row.uid_is_ours {
        Culprit::Services
    } else if let Some(app) = row.bundle.as_deref().and_then(app_name) {
        if app.to_ascii_lowercase().starts_with("aterm") {
            Culprit::Aterm
        } else {
            Culprit::App(clip(app, APP_CHARS))
        }
    } else if let Some(f) = Family::of(&row.name) {
        Culprit::Family(f)
    } else {
        Culprit::Process(clip(&row.name, COMM_CHARS))
    };
    (culprit, None)
}

/// Group a sweep's rows ([`ProcRow::cpu_ns`] read as the time over the
/// sweep's window), heaviest CPU first. A row measuring nothing is left out:
/// its time is the services' by subtraction.
#[must_use]
pub fn group(
    rows: &[ProcRow],
    sessions: &[SessionRef],
    own: &[JobRef<'_>],
    self_pid: u32,
) -> Vec<Group> {
    group_members(rows, sessions, own, self_pid).0
}

/// [`group`], and each measured pid's group (its culprit): what the tracker
/// remembers so a process that exits before the next sweep is still
/// credited to its own group.
fn group_members(
    rows: &[ProcRow],
    sessions: &[SessionRef],
    own: &[JobRef<'_>],
    self_pid: u32,
) -> (Vec<Group>, HashMap<u32, Culprit>) {
    let by_pid: HashMap<u32, &ProcRow> = rows.iter().map(|r| (r.pid, r)).collect();
    let mut at: HashMap<Culprit, usize> = HashMap::new();
    let mut out: Vec<Group> = Vec::new();
    // Per group: its heaviest row, by CPU then footprint, and that row's
    // session.
    let mut heaviest: Vec<(&ProcRow, Option<usize>)> = Vec::new();
    let mut member: Vec<(u32, usize)> = Vec::new();
    let weight = |r: &ProcRow| (r.cpu_ns.unwrap_or(0), r.footprint_kib.unwrap_or(0));
    for row in rows {
        if row.cpu_ns.is_none() && row.footprint_kib.is_none() {
            continue;
        }
        let (culprit, session) = classify(row, &by_pid, sessions, own, self_pid);
        let i = *at.entry(culprit.clone()).or_insert_with(|| {
            out.push(Group {
                culprit,
                cpu_ns: 0,
                footprint_kib: 0,
            });
            heaviest.push((row, session));
            out.len() - 1
        });
        member.push((row.pid, i));
        out[i].cpu_ns = out[i].cpu_ns.saturating_add(row.cpu_ns.unwrap_or(0));
        out[i].footprint_kib = out[i]
            .footprint_kib
            .saturating_add(row.footprint_kib.unwrap_or(0));
        if weight(row) > weight(heaviest[i].0) {
            heaviest[i] = (row, session);
        }
    }
    for (g, (top, session)) in out.iter_mut().zip(heaviest) {
        if let Culprit::Session { program, .. } = &mut g.culprit
            && let Some(s) = session.and_then(|i| sessions.get(i))
        {
            *program = heavy_name(top, s, &by_pid);
        }
    }
    // Two panes of one tab may now say the same words: one group.
    let mut merged: Vec<Group> = Vec::with_capacity(out.len());
    let mut seen: HashMap<Culprit, usize> = HashMap::new();
    let mut into: Vec<usize> = Vec::with_capacity(out.len());
    for g in out {
        if let Some(&m) = seen.get(&g.culprit) {
            merged[m].cpu_ns = merged[m].cpu_ns.saturating_add(g.cpu_ns);
            merged[m].footprint_kib = merged[m].footprint_kib.saturating_add(g.footprint_kib);
            into.push(m);
        } else {
            seen.insert(g.culprit.clone(), merged.len());
            into.push(merged.len());
            merged.push(g);
        }
    }
    let of_pid = member
        .into_iter()
        .map(|(pid, i)| (pid, merged[into[i]].culprit.clone()))
        .collect();
    merged.sort_by_key(|g| Reverse(g.cpu_ns));
    (merged, of_pid)
}

/// What a session's load is called (ruling 207): its foreground program
/// when the heaviest process under the shell is that program or runs under
/// it (`cargo`, not the `rustc` it spawned); otherwise the heaviest process
/// itself — twelve `yes &` at a zsh prompt are `yes`, never the idle `zsh`
/// that happens to hold the terminal, and a background build under a
/// foreground editor is the build, not the editor.
fn heavy_name(top: &ProcRow, s: &SessionRef, by_pid: &HashMap<u32, &ProcRow>) -> String {
    let program = clip(&s.program, COMM_CHARS);
    let mut pid = top.pid;
    for _ in 0..=CHAIN_HOPS {
        if pid == s.shell_pid {
            break;
        }
        let Some(r) = by_pid.get(&pid) else {
            break;
        };
        if clip(r.name.trim_start_matches('-'), COMM_CHARS) == program {
            return program;
        }
        if r.ppid == pid || r.ppid == 0 {
            break;
        }
        pid = r.ppid;
    }
    if top.pid == s.shell_pid || top.name.is_empty() {
        // Only the shell itself (or an unnamed row) is measured: the
        // published program is all there is to say.
        return program;
    }
    clip(&top.name, COMM_CHARS)
}

// ---------------------------------------------------------------------------
// Words (closed lists; ruling 207).
// ---------------------------------------------------------------------------

/// The title's fixed head.
const TITLE_HEAD: &str = "Typing slowed by ";
/// The cause's share of a glass title.
const CAUSE_WORDS: usize = GLASS_TITLE_WORDS - 3;
/// …in characters.
const CAUSE_CHARS: usize = GLASS_TITLE_CHARS - TITLE_HEAD.len();
/// Words a strain title never says (the grep guard's list, ruling 207).
const BLATHER: [&str; 5] = ["may", "detected", "consider", "fyi", "usage"];

/// Whether `s` says anything from the blather list: one of its words, a
/// `%`, or `load average`.
#[must_use]
pub fn says_blather(s: &str) -> bool {
    let lower = s.to_lowercase();
    s.contains('%')
        || lower.contains("load average")
        || lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| BLATHER.contains(&w))
}

/// A measured name made safe for a glass title: sanitized, no clause seam
/// or `%`, at most `words` words and `chars` characters, no trailing period;
/// `None` when nothing is left or it would say blather.
fn safe_name(raw: &str, words: usize, chars: usize) -> Option<String> {
    let clean: String = clip(raw, 64)
        .chars()
        .map(|c| match c {
            ':' | ';' | '%' => ' ',
            '\u{2014}' => '-',
            c => c,
        })
        .collect();
    let joined = clean
        .split_whitespace()
        .take(words)
        .collect::<Vec<_>>()
        .join(" ");
    let cut = truncate(joined.trim_end_matches('.'), chars);
    let cut = cut.trim_end_matches('.').trim().to_string();
    (!cut.is_empty() && cut.chars().any(char::is_alphanumeric) && !says_blather(&cut))
        .then_some(cut)
}

/// The words for a culprit, `None` where none may be said (an own job, aterm,
/// the resource, or a name that did not survive [`safe_name`]).
fn culprit_words(c: &Culprit, cfg: &StrainConfig) -> Option<String> {
    match c {
        Culprit::Session {
            program,
            tab,
            elsewhere,
            ..
        } => {
            // `in another window` would be a seventh word: the six-word
            // form keeps the fact and spends two.
            let tail = if *elsewhere {
                " (another window)".to_string()
            } else {
                format!(" in tab {tab}")
            };
            let prog = safe_name(program, 1, CAUSE_CHARS.saturating_sub(tail.len()))?;
            Some(format!("{prog}{tail}"))
        }
        Culprit::App(name) => safe_name(name, CAUSE_WORDS, APP_CHARS.min(CAUSE_CHARS)),
        Culprit::Family(f) => Some(f.words().to_string()),
        Culprit::Process(name) => safe_name(name, CAUSE_WORDS, COMM_CHARS.min(CAUSE_CHARS)),
        Culprit::Services => Some(cfg.services_noun.to_string()),
        Culprit::OwnJob(_) | Culprit::Aterm | Culprit::Resource => None,
    }
}

/// The cause's words: the culprit's, else the resource's.
fn cause_words(kind: StrainKind, c: &Culprit, cfg: &StrainConfig) -> (String, bool) {
    match culprit_words(c, cfg) {
        Some(w) => (w, true),
        None => (kind.resource_words().to_string(), false),
    }
}

/// The strain row's title: `Typing slowed by <cause>` — at most six words
/// and 48 characters, drawn from closed lists and measured names made safe.
#[must_use]
pub fn title(kind: StrainKind, c: &Culprit, cfg: &StrainConfig) -> String {
    format!("{TITLE_HEAD}{}", cause_words(kind, c, cfg).0)
}

/// A span as a record says it: `40 s`, `3m 12s`, `1h 5m`.
#[must_use]
pub fn span_words(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, s % 3600 / 60)
    }
}

/// Milli-cores as `7.1`.
fn cores_words(mc: u32) -> String {
    let tenths = mc.saturating_add(50) / 100;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// MiB as `6.2 GB` (or `512 MB` below a gigabyte).
fn mib_words(mib: u64) -> String {
    if mib < 1024 {
        format!("{mib} MB")
    } else {
        let tenths = (mib * 10 + 512) / 1024;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    }
}

/// Why a message may not carry a LEVEL meter: a measured level is admitted
/// on the strain row alone (tag `system`, key [`STRAIN_KEY`]) — the host's
/// `attention` reads this (ruling 208).
#[must_use]
pub fn level_fault(msg: &Message) -> Option<&'static str> {
    let level = msg.meter.as_ref().is_some_and(|m| m.level);
    (level && !(msg.tag == tags::SYSTEM && msg.key.as_deref() == Some(STRAIN_KEY)))
        .then_some("a level off the strain row")
}

/// A level row's accessible DESCRIPTION: its load words and its stats. The
/// accessible NAME is the title, which changes only with the cause, so a
/// screen reader is not re-told the numbers every two seconds.
#[must_use]
pub fn level_description(msg: &Message) -> String {
    let Some(m) = msg.meter.as_ref() else {
        return String::new();
    };
    match (m.load.map(Load::words), m.stats.is_empty()) {
        (Some(l), false) => format!("{l}{PIECE_SEP}{}", m.stats),
        (Some(l), true) => l.to_string(),
        (None, _) => m.stats.clone(),
    }
}

// ---------------------------------------------------------------------------
// The tracker.
// ---------------------------------------------------------------------------

/// One typing sample.
#[derive(Clone, Copy, Debug)]
struct Sample {
    at: Instant,
    ms: u32,
    freeze: bool,
}

/// One kind's hysteresis.
#[derive(Clone, Copy, Debug, Default)]
struct KindState {
    heavy: bool,
    run: u8,
}

impl KindState {
    fn step(&mut self, enter: bool, clear: bool) {
        if self.heavy {
            self.run = if clear { self.run + 1 } else { 0 };
            if self.run >= STRAIN_CLEAR_READINGS {
                *self = Self::default();
            }
        } else {
            self.run = if enter { self.run + 1 } else { 0 };
            if self.run >= STRAIN_ENTER_READINGS {
                *self = Self {
                    heavy: true,
                    run: 0,
                };
            }
        }
    }
}

/// A leading group, swapped only after a challenger leads two scans. While
/// a row shows (`sticky`), the leader's NAMED flag moves the same way: it
/// is a change of cause (culprit ↔ resource words, the load slot, the
/// accessible name), so a share hovering at the line never flips the title
/// scan by scan.
#[derive(Clone, Debug, Default)]
struct Leader {
    now: Option<(Culprit, bool)>,
    challenger: Option<(Culprit, u8)>,
    /// Consecutive scans the leader's NAMED flag has read the other way.
    flip: u8,
}

impl Leader {
    fn offer(&mut self, top: Option<(Culprit, bool)>, sticky: bool) {
        let Some((c, named)) = top else {
            return;
        };
        match &mut self.now {
            Some((cur, n)) if *cur == c => {
                self.challenger = None;
                if *n == named {
                    self.flip = 0;
                } else if !sticky || self.flip + 1 >= 2 {
                    *n = named;
                    self.flip = 0;
                } else {
                    self.flip += 1;
                }
            }
            Some(_) if sticky => {
                let count = match &self.challenger {
                    Some((ch, k)) if *ch == c => k + 1,
                    _ => 1,
                };
                if count >= 2 {
                    self.now = Some((c, named));
                    self.challenger = None;
                    self.flip = 0;
                } else {
                    self.challenger = Some((c, count));
                }
            }
            _ => {
                self.now = Some((c, named));
                self.challenger = None;
                self.flip = 0;
            }
        }
    }
}

/// One group's rate over the last sweep.
#[derive(Clone, Debug)]
struct Held {
    culprit: Culprit,
    mc: u32,
    mib: u32,
}

/// The previous reading's counters, for the deltas.
#[derive(Clone, Copy, Debug)]
struct Counters {
    at: Instant,
    busy_ticks: Option<(u64, u64)>,
    swap_pages: Option<u64>,
}

/// The episode state.
#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Calm,
    Suspect,
    Open {
        since: Instant,
        kind: StrainKind,
        cause: Culprit,
        pending_lower: u8,
    },
}

/// What the glass shows now.
#[derive(Clone, Debug)]
struct Shown {
    msg: Message,
    gauge: u16,
    at: Instant,
}

/// The last fold, for the quiet periods.
#[derive(Clone, Debug)]
struct Quiet {
    at: Instant,
    kind: StrainKind,
    cause: Culprit,
}

/// The strain engine: FELT from typing samples, HEAVY from readings, NAMED
/// from sweeps, one row and its records out. Clockless and integer-only;
/// bounded (a [`FEEL_RING`]-sample ring, a record window of
/// [`STRAIN_RECORDS_PER_WINDOW`], the last sweep's groups).
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent latches on their own clocks (critical pushed, critical seen)"
)]
pub struct StrainTracker {
    cfg: StrainConfig,
    ring: [Option<Sample>; FEEL_RING],
    head: usize,
    last_key: Option<Instant>,
    state: State,
    episode_start: Option<Instant>,
    felt_at: Option<Instant>,
    keys_unfelt: u32,
    ep_keys: u32,
    ep_slow: u32,
    ep_worst: u32,
    worst_turn: Option<(String, u32)>,
    // readings
    prev: Option<Counters>,
    last_reading_at: Option<Instant>,
    readings: u32,
    kinds: [KindState; 5],
    /// Each kind is past its ENTER line on the latest reading.
    entering: [bool; 5],
    busy_pm: Option<u16>,
    swap_kib_s: Option<u32>,
    last: Option<Reading>,
    critical_pushed: bool,
    critical_seen: bool,
    // sweeps
    scan_base: Option<(Instant, HashMap<u32, u64>)>,
    /// Each pid's CPU rate over the last sweep's window, milli-cores, and
    /// its group.
    rates: HashMap<u32, (u32, Culprit)>,
    groups: Vec<Held>,
    /// `groups` came from a sweep of a loaded machine this episode: a
    /// clearing machine's sweeps no longer replace them.
    groups_loaded: bool,
    services_mc: u32,
    leader_cpu: Leader,
    leader_mem: Leader,
    job_titles: Vec<(MessageId, String)>,
    // outcomes
    why: Option<(NotShown, StrainKind, Culprit)>,
    shown: Option<Shown>,
    quiet: Option<Quiet>,
    records: VecDeque<Instant>,
    dropped: u32,
    last_critical_record: Option<Instant>,
}

/// `a + d`, finite whatever `d` is.
fn plus(a: Instant, d: Duration) -> Instant {
    a.checked_add(d).unwrap_or(a)
}

/// `a − b`, zero when `b` is later.
fn since(a: Instant, b: Instant) -> Duration {
    a.saturating_duration_since(b)
}

/// A permille delta ratio, clamped to 1000.
fn permille(num: u64, den: u64) -> u16 {
    if den == 0 {
        return 0;
    }
    u16::try_from((u128::from(num) * 1000 / u128::from(den)).min(1000)).unwrap_or(1000)
}

impl StrainTracker {
    /// A calm tracker.
    #[must_use]
    pub fn new(cfg: StrainConfig) -> Self {
        Self {
            cfg,
            ring: [None; FEEL_RING],
            head: 0,
            last_key: None,
            state: State::Calm,
            episode_start: None,
            felt_at: None,
            keys_unfelt: 0,
            ep_keys: 0,
            ep_slow: 0,
            ep_worst: 0,
            worst_turn: None,
            prev: None,
            last_reading_at: None,
            readings: 0,
            kinds: [KindState::default(); 5],
            entering: [false; 5],
            busy_pm: None,
            swap_kib_s: None,
            last: None,
            critical_pushed: false,
            critical_seen: false,
            scan_base: None,
            rates: HashMap::new(),
            groups: Vec::new(),
            groups_loaded: false,
            services_mc: 0,
            leader_cpu: Leader::default(),
            leader_mem: Leader::default(),
            job_titles: Vec::new(),
            why: None,
            shown: None,
            quiet: None,
            records: VecDeque::with_capacity(STRAIN_RECORDS_PER_WINDOW),
            dropped: 0,
            last_critical_record: None,
        }
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> StrainConfig {
        self.cfg
    }

    /// Change the configuration (a hot reload). Turning the glass off
    /// while a row shows leaves it to fold as usual.
    pub fn set_config(&mut self, cfg: StrainConfig) {
        self.cfg = cfg;
    }

    /// The state's word, for `ctl metrics`: `calm`, `suspect` or `open`.
    #[must_use]
    pub fn state_word(&self) -> &'static str {
        match self.state {
            State::Calm => "calm",
            State::Suspect => "suspect",
            State::Open { .. } => "open",
        }
    }

    /// One closed HARDWARE key: `lag_ms` from its arrival to its window's
    /// present. `ctl send`, `feed`, a tainted present and a remote session's
    /// keys are never samples (the host filters them).
    pub fn note_key(&mut self, at: Instant, lag_ms: u32) {
        self.push(Sample {
            at,
            ms: lag_ms,
            freeze: false,
        });
        self.last_key = Some(at);
        if self.state != State::Calm {
            self.ep_keys = self.ep_keys.saturating_add(1);
            if lag_ms >= SLOW_KEY_MS {
                self.ep_slow = self.ep_slow.saturating_add(1);
            }
            self.ep_worst = self.ep_worst.max(lag_ms);
        }
        let felt = self.after_note(at);
        if !felt && self.state != State::Calm {
            self.keys_unfelt = self.keys_unfelt.saturating_add(1);
        }
    }

    /// One main-loop turn of `ms` that ended near a hardware key; `owner`
    /// names what held the turn (the watchdog's owner). Only a turn of at
    /// least [`FREEZE_MS`] is a hitch.
    pub fn note_freeze(&mut self, at: Instant, ms: u32, owner: &str) {
        if ms < FREEZE_MS {
            return;
        }
        self.push(Sample {
            at,
            ms,
            freeze: true,
        });
        if self.worst_turn.as_ref().is_none_or(|(_, w)| ms > *w) {
            self.worst_turn = Some((clip(owner, 32), ms));
        }
        self.after_note(at);
    }

    fn push(&mut self, s: Sample) {
        self.ring[self.head] = Some(s);
        self.head = (self.head + 1) % FEEL_RING;
    }

    /// Calm → Suspect when FELT; else keep the FELT clock. Returns FELT.
    fn after_note(&mut self, at: Instant) -> bool {
        let felt = self.felt(at);
        if felt {
            if self.state == State::Calm {
                self.begin(at);
            }
            self.felt_at = Some(at);
            self.keys_unfelt = 0;
        }
        felt
    }

    /// The samples in the window ending at `now`: `(keys, slow, hitches,
    /// worst key)`.
    ///
    /// A hitch is one STALL, counted once: each freeze spans `[end − ms,
    /// end]` and each key of at least [`HITCH_MS`] its `[arrival, present]`,
    /// and overlapping spans are one hitch. A key typed into a frozen turn
    /// waits out that same turn — the key and the freeze (or two keys in one
    /// stall) are one event, never the two a lone stall would need for FELT.
    fn window(&self, now: Instant) -> (usize, usize, usize, u32) {
        let mut out = (0, 0, 0, 0);
        let mut spans = [(now, now); FEEL_RING];
        let mut n = 0;
        for s in self.ring.iter().flatten() {
            if s.at > now || since(now, s.at) >= FEEL_WINDOW {
                continue;
            }
            if !s.freeze {
                out.0 += 1;
                out.1 += usize::from(s.ms >= SLOW_KEY_MS);
                out.3 = out.3.max(s.ms);
            }
            if s.freeze || s.ms >= HITCH_MS {
                let from =
                    s.at.checked_sub(Duration::from_millis(u64::from(s.ms)))
                        .unwrap_or(s.at);
                spans[n] = (from, s.at);
                n += 1;
            }
        }
        let spans = &mut spans[..n];
        spans.sort_unstable();
        let mut end: Option<Instant> = None;
        for &(from, to) in spans.iter() {
            match end {
                Some(e) if from <= e => end = Some(e.max(to)),
                _ => {
                    out.2 += 1;
                    end = Some(to);
                }
            }
        }
        out
    }

    /// FELT at `now`: at least [`FEEL_MIN_KEYS`] keys in the trailing
    /// [`FEEL_WINDOW`] with at least half of them slow, or at least
    /// [`FEEL_MIN_HITCHES`] hitches.
    #[must_use]
    pub fn felt(&self, now: Instant) -> bool {
        let (keys, slow, hitches, _) = self.window(now);
        (keys >= FEEL_MIN_KEYS && slow * 2 >= keys) || hitches >= FEEL_MIN_HITCHES
    }

    /// A new episode: Suspect, every measurement fresh.
    fn begin(&mut self, at: Instant) {
        let (keys, slow, _, worst) = self.window(at);
        self.state = State::Suspect;
        self.episode_start = Some(at);
        self.felt_at = Some(at);
        self.keys_unfelt = 0;
        self.ep_keys = u32::try_from(keys).unwrap_or(u32::MAX);
        self.ep_slow = u32::try_from(slow).unwrap_or(u32::MAX);
        self.ep_worst = worst;
        self.prev = None;
        self.last_reading_at = None;
        self.readings = 0;
        self.kinds = [KindState::default(); 5];
        self.entering = [false; 5];
        self.busy_pm = None;
        self.swap_kib_s = None;
        self.critical_pushed = false;
        self.critical_seen = false;
        self.scan_base = None;
        self.rates.clear();
        self.groups.clear();
        self.groups_loaded = false;
        self.services_mc = 0;
        self.leader_cpu = Leader::default();
        self.leader_mem = Leader::default();
        self.why = None;
        self.shown = None;
    }

    /// Back to calm: nothing armed, the sweep baseline and groups dropped.
    fn calm(&mut self) {
        self.state = State::Calm;
        self.episode_start = None;
        self.scan_base = None;
        self.rates.clear();
        self.groups.clear();
        self.groups_loaded = false;
        self.shown = None;
        self.why = None;
        self.worst_turn = None;
        self.last_reading_at = None;
    }

    /// When the host should take the next reading — `Some` only while
    /// Suspect or Open with the gate open (the first at once, then every
    /// [`STRAIN_SAMPLE_EVERY`]); a calm tracker arms nothing.
    ///
    /// A Suspect's reading lands ON its onset ([`STRAIN_ONSET`] after FELT
    /// began) when the cadence would pass it, so a row that may show shows
    /// then, not up to a reading later.
    #[must_use]
    pub fn next_sample(&self, now: Instant, gate: Gate) -> Option<Instant> {
        if !gate.open() || self.state == State::Calm {
            return None;
        }
        let Some(last) = self.last_reading_at else {
            return Some(now);
        };
        let next = plus(last, STRAIN_SAMPLE_EVERY);
        let onset = self
            .episode_start
            .filter(|_| self.state == State::Suspect)
            .map(|s| plus(s, STRAIN_ONSET))
            .filter(|o| last < *o && *o < next);
        Some(onset.unwrap_or(next))
    }

    /// Whether the next reading should carry a process sweep: every
    /// [`STRAIN_SCAN_EVERY`]th of an episode, the first included.
    #[must_use]
    pub fn wants_scan(&self) -> bool {
        self.state != State::Calm && self.readings.is_multiple_of(STRAIN_SCAN_EVERY)
    }

    /// The heaviest kind now, by priority.
    #[must_use]
    pub fn verdict(&self) -> Option<StrainKind> {
        StrainKind::ALL
            .into_iter()
            .find(|k| self.kinds[k.index()].heavy)
    }

    /// The highest-priority kind that is heavy AND still past its enter
    /// line on the latest reading. A kind that is heavy only by its
    /// hysteresis (memory at Warn after the swapping stopped) never blocks
    /// a lower kind that is rising.
    fn rising(&self) -> Option<StrainKind> {
        StrainKind::ALL
            .into_iter()
            .find(|k| self.kinds[k.index()].heavy && self.entering[k.index()])
    }

    /// The machine's busy core-time rate, milli-cores.
    fn busy_mc(&self) -> u32 {
        let cores = self.last.as_ref().map_or(0, |r| u32::from(r.cores));
        u32::from(self.busy_pm.unwrap_or(0)) * cores
    }

    /// A sweep's rows (cumulative `cpu_ns`), with the sessions and the jobs
    /// that have rows. The first sweep of an episode is the baseline; each
    /// later one groups the deltas. Call it before [`Self::observe`] for a
    /// reading that carried a sweep.
    pub fn scanned(
        &mut self,
        at: Instant,
        rows: &[ProcRow],
        sessions: &[SessionRef],
        own: &[JobRef<'_>],
    ) {
        if self.state == State::Calm {
            return;
        }
        let base: HashMap<u32, u64> = rows
            .iter()
            .filter_map(|r| r.cpu_ns.map(|ns| (r.pid, ns)))
            .collect();
        let Some((prev_at, prev)) = self.scan_base.replace((at, base)) else {
            return;
        };
        let dt_us = u64::try_from(since(at, prev_at).as_micros()).unwrap_or(u64::MAX);
        if dt_us == 0 {
            return;
        }
        let delta: Vec<ProcRow> = rows
            .iter()
            .map(|r| ProcRow {
                cpu_ns: r
                    .cpu_ns
                    .map(|ns| prev.get(&r.pid).map_or(ns, |p| ns.saturating_sub(*p))),
                ..r.clone()
            })
            .collect();
        let rate = |ns: u64| u32::try_from(ns / dt_us).unwrap_or(u32::MAX);
        let (grouped, of_pid) = group_members(&delta, sessions, own, self.cfg.self_pid);
        let rates: HashMap<u32, (u32, Culprit)> = delta
            .iter()
            .filter_map(|r| {
                let mc = rate(r.cpu_ns?);
                Some((r.pid, (mc, of_pid.get(&r.pid)?.clone())))
            })
            .collect();
        // Processes that were busy at the last sweep and exited inside this
        // one's window: their time is gone with them, and the services'
        // subtraction would take it. Each is credited its last rate, to its
        // own group, out of the busy time nothing visible accounts for — a
        // build's compilers come and go every sweep, and the build is still
        // what loads the machine.
        let vanished = exited(&self.rates, &rates);
        let vanished_mc = vanished
            .iter()
            .map(|(_, mc)| *mc)
            .fold(0, u32::saturating_add);
        self.rates = rates;
        // A TORN sweep: the load itself exited (measured live: twelve `yes`
        // killed mid-window read `macOS services 7.5 cores` under a title
        // that named `yes`). Its window says nothing about who loads the
        // machine now: a baseline only.
        if vanished_mc >= TORN_SWEEP_MC
            && u64::from(vanished_mc) * 100 >= u64::from(TORN_SHARE_PCT) * u64::from(self.busy_mc())
        {
            return;
        }
        let mib = |kib: u64| u32::try_from(kib / 1024).unwrap_or(u32::MAX);
        let mut held: Vec<Held> = grouped
            .iter()
            .map(|g| Held {
                culprit: g.culprit.clone(),
                mc: rate(g.cpu_ns),
                mib: mib(g.footprint_kib),
            })
            .collect();
        // The services: what other users' readable processes used, plus the
        // machine's busy time nothing visible — nor the exits' credit —
        // accounts for.
        let visible: u32 = held.iter().map(|h| h.mc).fold(0, u32::saturating_add);
        let unexplained = self.busy_mc().saturating_sub(visible);
        let credit = credit_exits(&mut held, vanished, vanished_mc, unexplained);
        let explicit = held
            .iter()
            .find(|h| h.culprit == Culprit::Services)
            .map_or(0, |h| h.mc);
        self.services_mc = explicit.saturating_add(unexplained - credit);
        held.retain(|h| h.culprit != Culprit::Services);
        held.push(Held {
            culprit: Culprit::Services,
            mc: self.services_mc,
            mib: 0,
        });
        held.sort_by_key(|h| Reverse(h.mc));
        // A sweep names a leader only while its resource is loaded: the
        // sweeps of a machine that is clearing would otherwise crown
        // whatever a calm machine runs (its services at 1.9 of 2 busy cores
        // are "named") in the seconds before the fold.
        let sticky = matches!(self.state, State::Open { .. });
        let cpu_loaded = self.busy_pm.is_some_and(|b| b >= LOADED_BUSY_PM);
        if cpu_loaded {
            self.leader_cpu
                .offer(lead_cpu(&held, self.busy_mc()), sticky);
        }
        let last = self.last.as_ref();
        let mem_loaded = last.is_some_and(|r| {
            r.pressure.is_some_and(|p| p >= MemoryLevel::Warn)
                || r.psi
                    .and_then(|p| p.memory_some_pm)
                    .is_some_and(|pm| pm >= 20)
        });
        if mem_loaded {
            let mem_mib = last.map_or(0, |r| r.mem_mib);
            self.leader_mem.offer(lead_mem(&held, mem_mib), sticky);
        }
        // Keep what Details reads: the top eight by CPU, the top three by
        // memory — from a loaded machine's sweep once the episode has had
        // one, so the `top:` line (and the episode's record) says what
        // loaded the machine, not what a calm one runs as it clears.
        if self.groups_loaded && !(cpu_loaded || mem_loaded) {
            self.job_titles = own.iter().map(|j| (j.id, clip(j.title, 40))).collect();
            return;
        }
        self.groups_loaded |= cpu_loaded || mem_loaded;
        let mut by_mem = held.clone();
        by_mem.sort_by_key(|h| Reverse(h.mib));
        held.truncate(8);
        for h in by_mem.into_iter().take(3) {
            if !held.iter().any(|x| x.culprit == h.culprit) {
                held.push(h);
            }
        }
        self.groups = held;
        self.job_titles = own.iter().map(|j| (j.id, clip(j.title, 40))).collect();
    }

    /// A push from the kernel: memory pressure went Critical. While calm it
    /// is a record (at most one per [`STRAIN_CRITICAL_EVERY`]; macOS shows
    /// its own dialog); during an episode it is an escalation.
    pub fn pushed_critical(&mut self, at: Instant) -> StrainOut {
        if self.state != State::Calm {
            self.critical_pushed = true;
            self.critical_seen = true;
            return StrainOut::None;
        }
        if self
            .last_critical_record
            .is_some_and(|t| since(at, t) < STRAIN_CRITICAL_EVERY)
        {
            return StrainOut::None;
        }
        self.last_critical_record = Some(at);
        let msg = Message::new(tags::SYSTEM, Severity::Info, "Memory pressure critical")
            .key(STRAIN_KEY)
            .hold(Hold::LogOnly)
            .line(format!("not shown: {}", NotShown::NoTyping.words()));
        self.record(at, msg)
            .map_or(StrainOut::None, StrainOut::Record)
    }

    /// The switch went off (or the host stops for good): an open row folds
    /// with its record; a Suspect ends silently; nothing is armed after.
    pub fn park(&mut self, now: Instant) -> StrainOut {
        let out = match self.state {
            State::Open { .. } => self.fold(now),
            State::Suspect | State::Calm => StrainOut::None,
        };
        self.calm();
        self.prev = None;
        out
    }

    /// One reading. Returns what the host performs.
    pub fn observe(&mut self, r: &Reading, gate: Gate) -> StrainOut {
        if !gate.enabled {
            return self.park(r.at);
        }
        if self.state == State::Calm {
            return StrainOut::None;
        }
        // The host stopped sampling past the row's staleness cap (the gate
        // closed: another app focused, the window hidden): the row faded
        // long ago. The episode ends where sampling stopped — its record and
        // the quiet period never count the time away.
        if let Some(prev) = self.last_reading_at
            && since(r.at, prev) >= STALE_STRAIN
        {
            return self.lapse(prev);
        }
        self.rates(r);
        self.readings = self.readings.saturating_add(1);
        self.last_reading_at = Some(r.at);
        self.step_kinds(r);
        if r.pressure == Some(MemoryLevel::Critical) {
            self.critical_seen = true;
        }
        if self.felt(r.at) {
            self.felt_at = Some(r.at);
        }
        match self.state.clone() {
            State::Calm => StrainOut::None,
            State::Suspect => self.suspect_step(r),
            State::Open {
                since: open_since,
                kind,
                cause,
                pending_lower,
            } => self.open_step(r, open_since, kind, &cause, pending_lower),
        }
    }

    /// No reading for [`STALE_STRAIN`] (the gate closed, or the probe went
    /// quiet): the episode ends at its last reading, and the tracker is calm
    /// again with nothing armed. The host calls this on any turn of its loop
    /// — it arms nothing for it — so an unfocused window's episode does not
    /// sit Open until the person comes back.
    pub fn lapsed(&mut self, now: Instant) -> StrainOut {
        let last = self.last_reading_at.or(self.episode_start);
        match last {
            Some(t) if self.state != State::Calm && since(now, t) >= STALE_STRAIN => self.lapse(t),
            _ => StrainOut::None,
        }
    }

    /// End the episode at `at` (its last reading).
    fn lapse(&mut self, at: Instant) -> StrainOut {
        match self.state {
            State::Open { .. } => self.fold(at),
            State::Suspect => {
                let start = self.episode_start.unwrap_or(at);
                self.end_suspect(at, start)
            }
            State::Calm => StrainOut::None,
        }
    }

    /// The deltas since the previous reading.
    fn rates(&mut self, r: &Reading) {
        let prev = self.prev.replace(Counters {
            at: r.at,
            busy_ticks: r.busy_ticks,
            swap_pages: r.swap_pages,
        });
        let Some(Counters {
            at,
            busy_ticks: ticks,
            swap_pages: swap,
        }) = prev
        else {
            self.busy_pm = None;
            self.swap_kib_s = None;
            self.last = Some(r.clone());
            return;
        };
        self.busy_pm = match (ticks, r.busy_ticks) {
            (Some((b0, t0)), Some((b1, t1))) if t1 > t0 => {
                Some(permille(b1.saturating_sub(b0), t1 - t0))
            }
            _ => None,
        };
        let dt_ms = u64::try_from(since(r.at, at).as_millis()).unwrap_or(0);
        self.swap_kib_s = match (swap, r.swap_pages) {
            (Some(s0), Some(s1)) if dt_ms > 0 => {
                u32::try_from(s1.saturating_sub(s0) * u64::from(r.page_kib) * 1000 / dt_ms).ok()
            }
            _ => None,
        };
        self.last = Some(r.clone());
    }

    /// Each kind's enter and clear lines against this reading (a `None`
    /// field is never heavy, and never keeps a kind heavy).
    fn step_kinds(&mut self, r: &Reading) {
        let busy = self.busy_pm;
        let swap = self.swap_kib_s;
        let psi = r.psi.unwrap_or_default();
        let ge = |v: Option<u16>, line: u16| v.is_some_and(|v| v >= line);
        let lt = |v: Option<u16>, line: u16| v.is_none_or(|v| v < line);
        // Memory.
        let mac_enter = match r.pressure {
            Some(MemoryLevel::Critical) => true,
            Some(MemoryLevel::Warn) => swap.is_some_and(|s| s >= 4096),
            _ => false,
        };
        let mem_enter = mac_enter || ge(psi.memory_some_pm, 100) || ge(psi.memory_full_pm, 50);
        let mem_clear = r
            .pressure
            .is_none_or(|p| p == MemoryLevel::Normal && swap.is_none_or(|s| s < 512))
            && lt(psi.memory_some_pm, 20);
        self.kinds[StrainKind::Memory.index()].step(mem_enter, mem_clear);
        // Heat.
        let heat_enter = r.thermal.is_some_and(|t| t >= Thermal::Serious);
        let heat_clear = r.thermal.is_none_or(|t| t <= Thermal::Fair);
        self.kinds[StrainKind::Heat.index()].step(heat_enter, heat_clear);
        // CPU.
        let cpu_enter = ge(busy, 850) || ge(psi.cpu_some_pm, 400);
        let cpu_clear = lt(busy, 650) && lt(psi.cpu_some_pm, 150);
        self.kinds[StrainKind::Cpu.index()].step(cpu_enter, cpu_clear);
        // Disk (PSI only).
        let disk_enter = ge(psi.io_full_pm, 200);
        let disk_clear = lt(psi.io_full_pm, 50);
        self.kinds[StrainKind::Disk.index()].step(disk_enter, disk_clear);
        // Low Power Mode: only when nothing above is heavy.
        let above = StrainKind::ALL[..4]
            .iter()
            .any(|k| self.kinds[k.index()].heavy);
        let lpm = r.low_power == Some(true);
        let lpm_enter = lpm && ge(busy, 600) && !above;
        let lpm_clear = !lpm || lt(busy, 450) || above;
        self.kinds[StrainKind::LowPower.index()].step(lpm_enter, lpm_clear);
        self.entering = [mem_enter, heat_enter, cpu_enter, disk_enter, lpm_enter];
    }

    /// The cause of a `kind` of strain: the named leader, else the resource.
    fn cause(&self, kind: StrainKind) -> Culprit {
        let leader = match kind {
            StrainKind::Memory => &self.leader_mem,
            StrainKind::Disk => return Culprit::Resource,
            StrainKind::Heat | StrainKind::Cpu | StrainKind::LowPower => &self.leader_cpu,
        };
        match &leader.now {
            Some((c, true)) => c.clone(),
            _ => Culprit::Resource,
        }
    }

    /// Why a cause may not reach the glass, if it may not.
    fn suppressed(&self, cause: &Culprit) -> Option<NotShown> {
        match cause {
            Culprit::Session {
                receiving_keys: true,
                ..
            } => Some(NotShown::OwnCommand),
            Culprit::OwnJob(id) => Some(NotShown::ExplainedBy(
                self.job_titles
                    .iter()
                    .find(|(j, _)| j == id)
                    .map_or_else(|| "an aterm job".to_string(), |(_, t)| t.clone()),
            )),
            Culprit::Aterm => Some(NotShown::AtermItself),
            _ => None,
        }
    }

    /// Whether a quiet period keeps this cause off the glass: any row
    /// [`STRAIN_QUIET_ANY`], the same cause [`STRAIN_QUIET_SAME`] after the
    /// last fold — unless memory pressure is Critical or the kind outranks
    /// the folded one.
    fn quiet_blocks(&self, now: Instant, kind: StrainKind, cause: &Culprit) -> bool {
        let Some(q) = &self.quiet else {
            return false;
        };
        let escalated = self.critical_pushed
            || self
                .last
                .as_ref()
                .is_some_and(|r| r.pressure == Some(MemoryLevel::Critical))
            || kind.rank() > q.kind.rank();
        if escalated {
            return false;
        }
        let d = since(now, q.at);
        d < STRAIN_QUIET_ANY || (d < STRAIN_QUIET_SAME && q.kind == kind && q.cause == *cause)
    }

    fn suspect_step(&mut self, r: &Reading) -> StrainOut {
        let now = r.at;
        let start = self.episode_start.unwrap_or(now);
        let unfelt = self
            .felt_at
            .is_none_or(|t| since(now, t) >= STRAIN_UNFELT_CALM);
        let idle = self
            .last_key
            .is_none_or(|t| since(now, t) >= STRAIN_NO_KEY_CALM);
        if unfelt || idle {
            return self.end_suspect(now, start);
        }
        let held = since(now, start) >= STRAIN_ONSET
            && self
                .felt_at
                .is_some_and(|t| since(now, t) <= STRAIN_SAMPLE_EVERY);
        if !held {
            return StrainOut::None;
        }
        // HEAVY, and still past its enter line NOW: a machine that is
        // clearing never opens a row (nor names a cause for its record).
        let Some(kind) = self.rising() else {
            return StrainOut::None;
        };
        let cause = self.cause(kind);
        if let Some(reason) = self.suppressed(&cause) {
            self.why = Some((reason, kind, cause));
            return StrainOut::None;
        }
        if self.quiet_blocks(now, kind, &cause) {
            self.why = Some((NotShown::Quiet, kind, cause));
            return StrainOut::None;
        }
        self.why = None;
        self.state = State::Open {
            since: now,
            kind,
            cause: cause.clone(),
            pending_lower: 0,
        };
        if !self.cfg.glass {
            return StrainOut::None;
        }
        let msg = self.row(kind, &cause);
        self.shown = Some(Shown {
            gauge: self.gauge(kind),
            msg: msg.clone(),
            at: now,
        });
        StrainOut::Post(msg)
    }

    /// A Suspect ends at `now`: one record when typing was FELT slow for at
    /// least [`STRAIN_RECORD_AFTER`] (a Suspect always lasts longer than
    /// that before it ends, so the felt span is the line; a lone blip is no
    /// episode and spends none of the record cap).
    fn end_suspect(&mut self, now: Instant, start: Instant) -> StrainOut {
        let felt_for = self
            .felt_at
            .map_or(Duration::ZERO, |t| since(t.min(now), start));
        let why = self.why.take();
        let out = if felt_for >= STRAIN_RECORD_AFTER {
            let msg = match why {
                Some((reason, kind, cause)) => {
                    // aterm as the load is a defect to fix: its record says
                    // so, in the same form — never `no heavy cause`.
                    let words = if reason == NotShown::AtermItself {
                        "aterm".to_string()
                    } else {
                        cause_words(kind, &cause, &self.cfg).0
                    };
                    let mut lines = self.details(kind);
                    lines.push(format!("not shown: {}", reason.words()));
                    Message::new(
                        tags::SYSTEM,
                        self.record_severity(),
                        format!("{TITLE_HEAD}{words} for {}", span_words(felt_for)),
                    )
                    .lines(lines)
                }
                None => self.felt_no_heavy(felt_for, &NotShown::NoHeavyCause),
            };
            let msg = msg.key(STRAIN_KEY).hold(Hold::LogOnly);
            self.record(now, msg)
                .map_or(StrainOut::None, StrainOut::Record)
        } else {
            StrainOut::None
        };
        self.calm();
        out
    }

    /// `Typing slow for 40 s, no heavy cause`, with the turn that held the
    /// loop longest.
    fn felt_no_heavy(&self, felt_for: Duration, reason: &NotShown) -> Message {
        let mut lines = Vec::new();
        if let Some(l) = self.typing_line() {
            lines.push(l);
        }
        if let Some((owner, ms)) = &self.worst_turn {
            lines.push(format!("turn: {owner} {ms} ms"));
        }
        lines.push(format!("not shown: {}", reason.words()));
        Message::new(
            tags::SYSTEM,
            Severity::Info,
            format!("Typing slow for {}, no heavy cause", span_words(felt_for)),
        )
        .lines(lines)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the open row's whole step: the fold rules, the kind and culprit rules, and the restate rule, read once in order"
    )]
    fn open_step(
        &mut self,
        r: &Reading,
        open_since: Instant,
        kind: StrainKind,
        cause: &Culprit,
        pending_lower: u8,
    ) -> StrainOut {
        let now = r.at;
        let on = since(now, open_since);
        let verdict = self.verdict();
        if on >= STRAIN_MIN_GLASS {
            let idle = self.last_key.is_none_or(|t| since(now, t) >= STRAIN_IDLE);
            let unfelt = self
                .felt_at
                .is_none_or(|t| since(now, t) >= STRAIN_UNFELT_FOLD)
                && self.keys_unfelt >= u32::try_from(FEEL_MIN_KEYS).unwrap_or(u32::MAX);
            if verdict.is_none() || idle || unfelt || on >= STRAIN_GLASS_MAX {
                return self.fold(now);
            }
        }
        // The kind: a higher one at once (while it is rising — one heavy
        // only by its hysteresis is not news), a lower one after two
        // readings.
        let (kind, pending_lower) = match (self.rising(), verdict) {
            (Some(v), _) if v.rank() > kind.rank() => (v, 0),
            (_, Some(v)) if v != kind && !self.kinds[kind.index()].heavy => {
                if pending_lower + 1 >= 2 {
                    (v, 0)
                } else {
                    (kind, pending_lower + 1)
                }
            }
            _ => (kind, 0),
        };
        // The culprit: the leader (swapped after two scans by `scanned`),
        // never a suppressed one — the person switching to the loaded tab
        // makes it their own command, and the row goes.
        let next = self.cause(kind);
        let cause = if let Some(reason) = self.suppressed(&next) {
            if on >= STRAIN_MIN_GLASS {
                self.why = Some((reason, kind, next));
                return self.fold(now);
            }
            cause.clone()
        } else {
            next
        };
        self.state = State::Open {
            since: open_since,
            kind,
            cause: cause.clone(),
            pending_lower,
        };
        if !self.cfg.glass {
            return StrainOut::None;
        }
        let msg = self.row(kind, &cause);
        let gauge = self.gauge(kind);
        let Some(shown) = self.shown.as_mut() else {
            self.shown = Some(Shown {
                gauge,
                msg: msg.clone(),
                at: now,
            });
            return StrainOut::Post(msg);
        };
        let words_changed = shown.msg.title != msg.title
            || shown.msg.severity != msg.severity
            || shown.msg.meter.as_ref().map(|m| (m.load, &m.stats))
                != msg.meter.as_ref().map(|m| (m.load, &m.stats));
        let moved = gauge.abs_diff(shown.gauge) >= STRAIN_RESTATE_PERMILLE;
        if words_changed || moved {
            *shown = Shown {
                gauge,
                msg: msg.clone(),
                at: now,
            };
            return StrainOut::Restate(msg);
        }
        if since(now, shown.at) >= STRAIN_KEEPALIVE {
            shown.at = now;
            return StrainOut::Restate(shown.msg.clone());
        }
        StrainOut::None
    }

    /// Open → calm: the row withdraws (a Vanish) and the episode's record
    /// is written; the quiet periods start.
    fn fold(&mut self, now: Instant) -> StrainOut {
        let State::Open {
            since: open_since,
            kind,
            cause,
            ..
        } = self.state.clone()
        else {
            return StrainOut::None;
        };
        let start = self.episode_start.unwrap_or(open_since);
        let (words, _) = cause_words(kind, &cause, &self.cfg);
        let mut lines = self.details(kind);
        let shown = self.shown.is_some();
        lines.push(match (shown, &self.why) {
            (true, _) => format!("on glass {}", span_words(since(now, open_since))),
            (false, Some((reason, ..))) => format!("not shown: {}", reason.words()),
            (false, None) => format!("not shown: {}", NotShown::RecordsOnly.words()),
        });
        let msg = Message::new(
            tags::SYSTEM,
            self.record_severity(),
            format!("{TITLE_HEAD}{words} for {}", span_words(since(now, start))),
        )
        .key(STRAIN_KEY)
        .hold(Hold::LogOnly)
        .lines(lines);
        self.quiet = Some(Quiet {
            at: now,
            kind,
            cause,
        });
        let record = self.record(now, msg);
        self.calm();
        if shown {
            StrainOut::Fold { record }
        } else {
            record.map_or(StrainOut::None, StrainOut::Record)
        }
    }

    fn record_severity(&self) -> Severity {
        if self.critical_seen {
            Severity::Warn
        } else {
            Severity::Info
        }
    }

    /// A record through the cap: at most [`STRAIN_RECORDS_PER_WINDOW`] per
    /// trailing [`STRAIN_RECORD_WINDOW`]; past it `None`, counted, and the
    /// next record admitted says ` (+N more)`.
    fn record(&mut self, at: Instant, mut msg: Message) -> Option<Message> {
        while self
            .records
            .front()
            .is_some_and(|t| since(at, *t) >= STRAIN_RECORD_WINDOW)
        {
            self.records.pop_front();
        }
        if self.records.len() >= STRAIN_RECORDS_PER_WINDOW {
            self.dropped = self.dropped.saturating_add(1);
            return None;
        }
        self.records.push_back(at);
        if self.dropped > 0 {
            msg.title = clip(&format!("{} (+{} more)", msg.title, self.dropped), 120);
            self.dropped = 0;
        }
        Some(msg)
    }

    /// The gauge for `kind`, permille.
    fn gauge(&self, kind: StrainKind) -> u16 {
        let r = self.last.as_ref();
        match kind {
            StrainKind::Memory => r.and_then(|r| r.mem_used_pm).unwrap_or(0).min(1000),
            StrainKind::Disk => r
                .and_then(|r| r.psi)
                .and_then(|p| p.io_full_pm)
                .unwrap_or(0)
                .min(1000),
            StrainKind::Heat | StrainKind::Cpu | StrainKind::LowPower => self.busy_pm.unwrap_or(0),
        }
    }

    /// The stats beside the gauge (never spoken).
    fn stats(&self, kind: StrainKind) -> String {
        let cores = self.last.as_ref().map_or(0, |r| r.cores);
        let busy = format!("{} of {cores} cores", cores_words(self.busy_mc()));
        match kind {
            StrainKind::Cpu | StrainKind::LowPower => busy,
            StrainKind::Heat => format!("throttled{PIECE_SEP}{busy}"),
            StrainKind::Memory => {
                let swap = format!(
                    "swap {} MB/s",
                    self.swap_kib_s.map_or(0, |s| s.saturating_add(512) / 1024)
                );
                match self.last.as_ref().and_then(|r| r.mem_used_mib) {
                    Some(used) => format!("{} GB{PIECE_SEP}{swap}", (u64::from(used) + 512) / 1024),
                    None => swap,
                }
            }
            StrainKind::Disk => format!("I/O stall {}%", (self.gauge(StrainKind::Disk) + 5) / 10),
        }
    }

    /// A group's words in Details: the culprit's, an own job's title,
    /// `aterm`.
    fn group_words(&self, c: &Culprit) -> String {
        match c {
            Culprit::OwnJob(id) => self
                .job_titles
                .iter()
                .find(|(j, _)| j == id)
                .map_or_else(|| "an aterm job".to_string(), |(_, t)| t.clone()),
            Culprit::Aterm => "aterm".to_string(),
            c => culprit_words(c, &self.cfg).unwrap_or_else(|| "a process".to_string()),
        }
    }

    fn typing_line(&self) -> Option<String> {
        (self.ep_keys > 0).then(|| {
            format!(
                "typing: {} of {} keys over {SLOW_KEY_MS} ms{PIECE_SEP}worst {} ms",
                self.ep_slow, self.ep_keys, self.ep_worst
            )
        })
    }

    /// Details: facts only, at most four here (the record adds its fifth).
    fn details(&self, kind: StrainKind) -> Vec<String> {
        let mut lines = Vec::with_capacity(5);
        let top: Vec<String> = if kind == StrainKind::Memory {
            let mut by_mem: Vec<&Held> = self.groups.iter().filter(|h| h.mib > 0).collect();
            by_mem.sort_by_key(|h| Reverse(h.mib));
            by_mem
                .iter()
                .take(3)
                .map(|h| {
                    format!(
                        "{} {}",
                        self.group_words(&h.culprit),
                        mib_words(u64::from(h.mib))
                    )
                })
                .collect()
        } else {
            self.groups
                .iter()
                .filter(|h| h.mc >= 50)
                .take(3)
                .enumerate()
                .map(|(i, h)| {
                    let unit = if i == 0 { " cores" } else { "" };
                    format!(
                        "{} {}{unit}",
                        self.group_words(&h.culprit),
                        cores_words(h.mc)
                    )
                })
                .collect()
        };
        if !top.is_empty() {
            lines.push(format!("top: {}", top.join(PIECE_SEP)));
        }
        if let Some(l) = self.typing_line() {
            lines.push(l);
        }
        if let Some(r) = self.last.as_ref() {
            if let Some(p) = r.pressure {
                let mut l = format!("memory: pressure {}", p.word());
                if let Some(used) = r.mem_used_mib {
                    let _ = write!(l, "{PIECE_SEP}{} in use", mib_words(u64::from(used)));
                }
                if let Some(s) = self.swap_kib_s {
                    let _ = write!(l, "{PIECE_SEP}swap {} MB/s", s.saturating_add(512) / 1024);
                }
                lines.push(l);
            }
            match (r.thermal, r.low_power) {
                (None, None) => {}
                (t, lpm) => {
                    let mut parts = Vec::new();
                    if let Some(t) = t {
                        parts.push(t.word().to_string());
                    }
                    if let Some(on) = lpm {
                        parts.push(format!("Low Power Mode {}", if on { "on" } else { "off" }));
                    }
                    lines.push(format!("heat: {}", parts.join(PIECE_SEP)));
                }
            }
        }
        lines.truncate(4);
        lines
    }

    /// The strain row for `kind` and `cause`.
    fn row(&self, kind: StrainKind, cause: &Culprit) -> Message {
        let (words, named) = cause_words(kind, cause, &self.cfg);
        let critical = self.critical_pushed
            || self
                .last
                .as_ref()
                .is_some_and(|r| r.pressure == Some(MemoryLevel::Critical));
        let severity = if critical {
            Severity::Warn
        } else {
            Severity::Info
        };
        let meter = Meter {
            load: named.then(|| kind.load()),
            ..Meter::level(self.gauge(kind), self.stats(kind))
        };
        Message::new(tags::SYSTEM, severity, format!("{TITLE_HEAD}{words}"))
            .key(STRAIN_KEY)
            .hold(Hold::Live {
                stale_after: STALE_STRAIN,
            })
            .meter(meter)
            .no_excerpt()
            .lines(self.details(kind))
    }
}

/// The processes of `before` (each pid's last rate and group) that are not
/// in `now`: what exited inside the sweep's window, summed per group.
fn exited(
    before: &HashMap<u32, (u32, Culprit)>,
    now: &HashMap<u32, (u32, Culprit)>,
) -> Vec<(Culprit, u32)> {
    let mut out: Vec<(Culprit, u32)> = Vec::new();
    for (pid, (mc, culprit)) in before {
        if now.contains_key(pid) {
            continue;
        }
        match out.iter_mut().find(|(c, _)| c == culprit) {
            Some((_, v)) => *v = v.saturating_add(*mc),
            None => out.push((culprit.clone(), *mc)),
        }
    }
    out
}

/// Credit the exits' last rates to their own groups, pro rata, out of the
/// `unexplained` busy time (never more): returns the milli-cores credited.
fn credit_exits(
    held: &mut Vec<Held>,
    vanished: Vec<(Culprit, u32)>,
    vanished_mc: u32,
    unexplained: u32,
) -> u32 {
    let credit = vanished_mc.min(unexplained);
    if credit == 0 {
        return 0;
    }
    for (culprit, mc) in vanished {
        let share =
            u32::try_from(u64::from(mc) * u64::from(credit) / u64::from(vanished_mc.max(1)))
                .unwrap_or(0);
        match held.iter_mut().find(|h| h.culprit == culprit) {
            Some(h) => h.mc = h.mc.saturating_add(share),
            None => held.push(Held {
                culprit,
                mc: share,
                mib: 0,
            }),
        }
    }
    credit
}

/// The CPU leader of a sweep: the heaviest group, NAMED when it uses at
/// least a core and [`STRAIN_NAMED_SHARE_PCT`] of the busy core-time.
fn lead_cpu(held: &[Held], busy_mc: u32) -> Option<(Culprit, bool)> {
    let top = held.iter().max_by_key(|h| h.mc)?;
    let named = top.mc >= STRAIN_NAMED_MILLICORES
        && u64::from(top.mc) * 100 >= u64::from(STRAIN_NAMED_SHARE_PCT) * u64::from(busy_mc);
    Some((top.culprit.clone(), named))
}

/// The memory leader: the largest footprint, NAMED past
/// [`STRAIN_NAMED_MEMORY_PCT`] of the RAM.
fn lead_mem(held: &[Held], mem_mib: u32) -> Option<(Culprit, bool)> {
    let top = held
        .iter()
        .filter(|h| h.culprit != Culprit::Services)
        .max_by_key(|h| h.mib)?;
    let named = mem_mib > 0
        && u64::from(top.mib) * 100 >= u64::from(STRAIN_NAMED_MEMORY_PCT) * u64::from(mem_mib);
    Some((top.culprit.clone(), named))
}

#[cfg(test)]
mod tests;
