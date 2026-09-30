// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE CRASH JOURNAL (PTY keeper P1, `docs/DESIGN-pty-keeper-2026-09-26.md`
//! §5.5): the layout a window keeps on disk for as long as it runs, so a run
//! that does not end cleanly — killed, a fatal signal, a panic — leaves the next
//! launch something to reopen. Until this existed the only saved layout was the
//! one written at the last QUIT (`restore::write`), and the next boot consumed it,
//! so a crashed run left nothing behind: every window, tab and folder went with
//! the shells.
//!
//! WHAT IT HOLDS is `session.toml`'s content — the same [`RestoreManifest`], the
//! same codec, so a journal is itself a valid `session.toml` — plus a `[journal]`
//! table naming its writer (pid, build, when) and whether that writer had itself
//! reopened a crash journal less than [`PROBATION`] ago. No environment (owner
//! decision 9): cwds, titles and the user's session metadata, the classes
//! `session.toml` already holds — and, since P6a (owner direction 2026-09-27:
//! relaunch the agents after a crash, a kill or a restart), on a terminal leaf
//! whose shell hosts one, the agent's relaunch record
//! ([`crate::restore::AgentRestore`]: program, argv, conversation, folder); and,
//! in the `[journal]` table alone, which leaves had a program other than their
//! shell running and its name as `status program=` publishes it
//! ([`JournalHeader::programs`], D16), so a relaunch whose tabs sat at their
//! prompts lost nothing and says so quietly.
//!
//! WHERE: `journal-<pid>-<nanos>.toml` beside `session.toml`, `0600`, with
//! `journal-<pid>-<nanos>.lock` beside it. The lock is created under a pending
//! name, `flock`ed, then renamed into place — the crash marker's pattern
//! (`crash_signal::markers::create_locked`) — and held for the writer's life, so
//! another launch tells a live writer's journal from a dead one's by trying it.
//! Every image is published by a create-new temporary (`O_NOFOLLOW`, `0600`), an
//! `fsync`, a `rename` over the name and a directory `fsync`: a death mid-write
//! leaves the previous image whole, and nothing is ever written through a link.
//!
//! WHEN (the writer, [`Lane`]): at most once per [`MIN_WRITE_INTERVAL`], only
//! when the captured layout differs from the last one written, and nothing while
//! idle — a capture runs only after a wake that was not a timer (input, a window
//! event, PTY output, a control request) and at the trailing edge of the
//! interval. It follows `restore_session` exactly as the quit-time restore does:
//! off, or headless, and nothing is written (and a journal already written is
//! withdrawn); a clean quit removes it once the quit's own layout is written.
//!
//! WHO READS IT ([`claim_at_boot`]): the next launch that reads `session.toml`.
//! A journal whose writer still holds its lock is never touched. A dead writer's
//! journal is TAKEN by renaming it to a claim name — the rename is the single-use
//! point, so of two launches racing for it one finds nothing — and reopened only
//! when (a) there is no quit layout to reopen, (b) its writer's end was unclean by
//! that run's crash marker ([`classify_death`]: a panic report, a signal banner,
//! or an empty marker its dead owner never removed — every clean exit removes
//! its own), and (c) the brake lets it through: a writer that had itself
//! reopened a crash journal and then CRASHED within [`PROBATION`] is skipped,
//! and so is one that was STOPPED within it for the second time in a row
//! ([`JournalHeader::second_chance`], ruling 285) — a kill is no evidence that
//! the layout stops aterm (day five, D18: a SIGKILL 70 s after a relaunch
//! dropped both tabs for good), but a layout that hangs aterm until it is
//! force-quit is, the second time. So a layout that stops aterm cannot loop,
//! and one that was merely killed comes back once more. Anything else
//! taken is set aside with a line in the log; an unclean end's journal that
//! cannot be read, and one the brake skips, are said on the band as well.
//!
//! The derived machine is `CrashJournalClaim`
//! (`aterm_spec::derive::crash_journal_claim_model`); Tier-1 is
//! `crash_journal_conformance.rs`.
#![cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "the journal's files, lock and claim are the unix lane's (the lock is the crash \
                  marker's `flock`); elsewhere the lane is inert and nothing reads them"
    )
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::restore::RestoreManifest;

/// At most one journal write per this interval (design §5.5), and at most one
/// capture: a capture that finds nothing new still waits it out.
pub(crate) const MIN_WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// At most one journal write per this interval when nothing but titles moved.
/// A program that animates its title — an agent's spinner — changes it many
/// times a second, and at the layout's own interval that was a write every 2 s
/// for as long as it ran (measured 2026-09-26: a 10 Hz title, 24 writes in
/// 50 s). A title is the least of what a restore carries: the shell sets its
/// own the moment it starts.
pub(crate) const TITLE_WRITE_INTERVAL: Duration = Duration::from_secs(30);

/// How long a launch that reopened a crash journal must run before its own
/// journal may be reopened in turn (the brake, design §5.5).
pub(crate) const PROBATION: Duration = Duration::from_secs(90);

/// The largest journal a claim reads. A layout is kilobytes; this is a bound on
/// a file nobody should have made, never a limit a real layout meets.
const MAX_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;

/// Every journal-family name starts with this (after an optional `.`).
const NAME_PREFIX: &str = "journal-";

/// One run's journal: the writer's pid and its run's start as its crash marker
/// names it (else the instant the journal was armed), nanoseconds since the
/// epoch — together unique across pid reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct JournalId {
    pub(crate) pid: u32,
    pub(crate) nanos: u128,
}

impl JournalId {
    /// A journal of `pid`, armed now.
    pub(crate) fn now(pid: u32) -> Self {
        Self {
            pid,
            nanos: unix_nanos(SystemTime::now()),
        }
    }

    /// THIS run's journal: named for the start its crash marker carries
    /// (`crash_signal::own_marker_nanos`), so a claim finds exactly that marker
    /// — under its consumed `.seen` name too, when a launch that does not
    /// claim read the evidence first ([`classify_death`]). A run with no armed
    /// marker is named for now, which reads its marker as before.
    pub(crate) fn of_this_run(pid: u32) -> Self {
        #[cfg(unix)]
        let start = crate::crash_signal::own_marker_nanos();
        #[cfg(not(unix))]
        let start = None;
        Self::named(pid, start)
    }

    /// `pid`'s journal, named for `start` (its crash marker's), else for now.
    pub(crate) fn named(pid: u32, start: Option<u128>) -> Self {
        start.map_or_else(|| Self::now(pid), |nanos| Self { pid, nanos })
    }

    /// `journal-<pid>-<nanos>.toml`.
    pub(crate) fn file_name(self) -> String {
        format!("{NAME_PREFIX}{}-{}.toml", self.pid, self.nanos)
    }

    /// `journal-<pid>-<nanos>.lock`: the writer's lifetime lock.
    pub(crate) fn lock_name(self) -> String {
        format!("{NAME_PREFIX}{}-{}.lock", self.pid, self.nanos)
    }

    /// Parse a [`Self::file_name`]; `None` for anything else.
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::parse_stem(name.strip_prefix(NAME_PREFIX)?.strip_suffix(".toml")?)
    }

    /// Parse a [`Self::lock_name`]; `None` for anything else.
    fn parse_lock(name: &str) -> Option<Self> {
        Self::parse_stem(name.strip_prefix(NAME_PREFIX)?.strip_suffix(".lock")?)
    }

    fn parse_stem(stem: &str) -> Option<Self> {
        let (pid, nanos) = stem.split_once('-')?;
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !digits(pid) || !digits(nanos) {
            return None;
        }
        Some(Self {
            pid: pid.parse().ok()?,
            nanos: nanos.parse().ok()?,
        })
    }

    /// The prefix of this journal's write temporaries:
    /// `.journal-<pid>-<nanos>.w-<seed>.tmp`.
    fn temp_prefix(self) -> String {
        format!(".{NAME_PREFIX}{}-{}.w-", self.pid, self.nanos)
    }
}

/// Nanoseconds since the epoch; 0 for a clock before it.
fn unix_nanos(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

/// The journal's own table: who wrote it, and whether it is on probation.
/// Every field is additive (`serde(default)`), so a journal written by another
/// build — an older one before an update, a newer one before a rollback —
/// reads.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct JournalHeader {
    #[serde(default)]
    pub(crate) pid: u32,
    /// The writer's version and build number, for the person reading the file.
    #[serde(default)]
    pub(crate) build: String,
    #[serde(default)]
    pub(crate) written_unix_ms: u64,
    /// The writer reopened a crash journal at its boot and had not yet run
    /// [`PROBATION`] when this image was written.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) probation: bool,
    /// THE SECOND CHANCE (ruling 285; additive, absent = `false`): the
    /// writer's own boot reopened a journal whose writer had been on
    /// probation and was STOPPED (killed, not crashed) — the brake let that
    /// layout through once. Meaningful only with [`Self::probation`]: a
    /// writer that dies inside its probation with this mark is skipped
    /// however it died, so a layout that hangs aterm until it is force-quit
    /// comes back at most once more.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) second_chance: bool,
    /// THE PROGRAMS (D16, ruling 282; additive, absent tolerated): every
    /// terminal leaf whose foreground was a job — a program other than its
    /// login shell at the prompt — when this image was captured, by its place
    /// in the layout. ABSENT is UNKNOWN (a writer from before this field): the
    /// reopened row keeps saying the programs were lost. PRESENT AND EMPTY is
    /// the quiet case: every leaf sat at its shell's prompt, so a relaunch lost
    /// nothing a person was running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) programs: Option<Vec<LeafProgram>>,
}

/// One terminal leaf that had a program running (D16): the window and tab it
/// sits in (their places in the image's own layout, `restored_tabs` order),
/// and the program's name as `status program=` publishes it — empty when the
/// name was not resolved yet.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeafProgram {
    #[serde(default)]
    pub(crate) window: u32,
    #[serde(default)]
    pub(crate) tab: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) program: String,
    /// P6a (ruling 293; additive, absent = `false`): this leaf's restore
    /// carries its agent's relaunch record ([`crate::restore::TerminalLeafRestore::agent`]),
    /// so the program is that agent, which the next launch relaunches on its
    /// conversation ([`Reopened::resumed`]) instead of losing it. An older
    /// writer's entry reads as a program lost, as it always did.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) agent: bool,
}

/// The most leaves' programs one image names: every leaf a layout may carry
/// has at most one entry, so a longer list was not written by aterm.
const MAX_PROGRAM_ENTRIES: usize = 4096;

/// The longest program name kept from a journal (`session_program`'s cap).
const MAX_PROGRAM_NAME: usize = 32;

/// A journal is operator-writable: keep at most [`MAX_PROGRAM_ENTRIES`], and
/// a name only when it is the token `status program=` would publish (else it
/// reads as unnamed — the program still ran).
fn sanitize_programs(programs: &mut Vec<LeafProgram>) {
    programs.truncate(MAX_PROGRAM_ENTRIES);
    for entry in programs.iter_mut() {
        let token = entry.program.len() <= MAX_PROGRAM_NAME
            && entry
                .program
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b));
        if !token {
            entry.program.clear();
        }
    }
}

impl JournalHeader {
    /// This header naming the programs its image's leaves were running.
    pub(crate) fn with_programs(mut self, programs: Vec<LeafProgram>) -> Self {
        self.programs = Some(programs);
        self
    }

    /// This process's header, written now.
    pub(crate) fn now(pid: u32, probation: bool) -> Self {
        Self {
            pid,
            build: format!(
                "{} ({})",
                aterm_types::version::APP_VERSION,
                crate::build_info::BUILD_NUMBER
            ),
            written_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| {
                    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                }),
            probation,
            second_chance: false,
            programs: None,
        }
    }

    /// This header, marked as a second chance's ([`Self::second_chance`]).
    pub(crate) fn with_second_chance(mut self, second_chance: bool) -> Self {
        self.second_chance = second_chance;
        self
    }
}

/// The `[journal]` table alone, as a document.
#[derive(Serialize, Deserialize)]
struct HeaderDoc {
    journal: JournalHeader,
}

/// One journal image: the layout exactly as `session.toml` holds it, then the
/// `[journal]` table — a new top-level table after the layout's own, so the
/// text parses as a `session.toml` too.
pub(crate) fn encode(manifest: &RestoreManifest, header: &JournalHeader) -> Result<String, String> {
    let mut text = manifest.to_toml()?;
    let head = aterm_toml::to_string(&HeaderDoc {
        journal: header.clone(),
    })
    .map_err(|error| format!("serialize the journal header: {error}"))?;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&head);
    Ok(text)
}

/// Read one journal image back: its header and its layout, the layout through
/// the same fail-safe parse `session.toml` takes.
pub(crate) fn decode(text: &str) -> Result<(JournalHeader, RestoreManifest), String> {
    let mut header = aterm_toml::from_str::<HeaderDoc>(text)
        .map_err(|error| format!("no readable [journal] table ({error})"))?
        .journal;
    if let Some(programs) = header.programs.as_mut() {
        sanitize_programs(programs);
    }
    let manifest = RestoreManifest::from_toml(text)
        .ok_or_else(|| "its layout is not one this build reads".to_string())?;
    Ok((header, manifest))
}

/// How a journal's writer ended, by its run's crash evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeathClass {
    /// Its crash marker was left empty: no fatal signal and no exit path
    /// (SIGKILL, Force Quit, jetsam, a watchdog, power loss).
    Killed,
    /// Its crash marker carries a fatal signal's banner.
    Signal,
    /// The panic hook filed its crash report.
    Panic,
}

impl DeathClass {
    /// The row's first line.
    pub(crate) fn sentence(self) -> &'static str {
        match self {
            Self::Killed => {
                "aterm was stopped last time without quitting (a force quit, a kill or the \
                 system), not a crash"
            }
            Self::Signal => "aterm crashed last time: a fatal signal",
            Self::Panic => "aterm crashed last time: a panic",
        }
    }

    /// A crash (the error class) rather than a kill (the warning class).
    pub(crate) fn crashed(self) -> bool {
        !matches!(self, Self::Killed)
    }
}

/// Where a claim looks for how a journal's writer ended: the log dir the crash
/// markers and panic reports live in, and what this launch's own install-time
/// sweep removed before the claim ran (`crash_signal::markers::swept_at_install`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Evidence<'a> {
    pub(crate) log_dir: Option<&'a Path>,
    #[cfg(unix)]
    pub(crate) swept: &'a [crate::crash_signal::markers::MarkerName],
}

/// A journal this launch reopens.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reopened {
    /// The layout: every reopened journal's windows, newest writer first.
    pub(crate) manifest: RestoreManifest,
    /// How the newest reopened writer ended.
    pub(crate) class: DeathClass,
    /// The journals it came from, newest first.
    pub(crate) sources: Vec<JournalId>,
    /// The writer of each of `manifest`'s windows, in order: the pid of the
    /// journal it came from. A window's pool ids are its writer's, so a shell
    /// the PTY keeper kept is matched only to its own writer's leaves
    /// (`keeper_link::place_by_owner`).
    pub(crate) writers: Vec<u32>,
    /// The leaves that had a program running, their windows counted in this
    /// merged layout; `None` when any source journal did not say (an older
    /// writer's), which reads as today's "programs lost" (D16, ruling 282).
    pub(crate) programs: Option<Vec<LeafProgram>>,
    /// A source journal's writer was itself on probation and was stopped
    /// (ruling 285): the brake let it through once, and this launch's own
    /// journal carries [`JournalHeader::second_chance`] through its
    /// probation.
    pub(crate) second_chance: bool,
}

/// What a reopened layout lost (D16): the tabs a program was running in, and
/// the names of those programs that were known (deduplicated, in order).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lost {
    pub(crate) tabs: usize,
    pub(crate) named: Vec<String>,
    /// Leaves whose program's name was not resolved yet.
    pub(crate) unnamed: usize,
}

/// The reopened layout's terminal panes whose saved folder is not here
/// ([`Reopened::panes_without_folder`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PanesWithoutFolder {
    /// Panes whose restore asks for a folder no shell can start in: each
    /// starts in the home folder, and a Messages row names the folder.
    pub(crate) gone: usize,
    /// Panes a program held
    /// ([`crate::restore::TerminalLeafRestore::folder_may_be_remote`]): the
    /// folder may be another machine's, so the restore does not ask for it
    /// and no row names it.
    pub(crate) unasked: usize,
}

impl PanesWithoutFolder {
    /// Every pane's folder is here.
    pub(crate) const NONE: Self = Self {
        gone: 0,
        unasked: 0,
    };
}

impl Reopened {
    /// Windows and tabs the layout reopens.
    pub(crate) fn counts(&self) -> (usize, usize) {
        let windows = self.manifest.windows.len();
        let tabs = self
            .manifest
            .windows
            .iter()
            .map(|window| window.restored_tabs.len().max(window.tabs.len()))
            .sum();
        (windows, tabs)
    }

    /// THE AGENTS THAT COME BACK (P6a, ruling 293): one `(window, tab)` per
    /// terminal leaf whose restore carries an agent the next launch
    /// relaunches on its conversation — when it relaunches at all
    /// (`relaunching`: the supervisor host will,
    /// [`crate::harness_host::HostHandle::relaunches_restored`]) and the
    /// record is one the relaunch brings back
    /// ([`crate::restore::AgentRestore::resumes`]). In layout order.
    pub(crate) fn resumed(&self, relaunching: bool) -> Vec<(u32, u32)> {
        fn walk(
            node: &crate::restore::RestoredSplitTree,
            place: (u32, u32),
            out: &mut Vec<(u32, u32)>,
        ) {
            match node {
                crate::restore::RestoredSplitTree::Leaf {
                    view: crate::restore::RestoredView::Terminal(leaf),
                } => {
                    if leaf
                        .agent
                        .as_deref()
                        .is_some_and(crate::restore::AgentRestore::resumes)
                    {
                        out.push(place);
                    }
                }
                crate::restore::RestoredSplitTree::Leaf { .. } => {}
                crate::restore::RestoredSplitTree::Split { first, second, .. } => {
                    walk(first, place, out);
                    walk(second, place, out);
                }
            }
        }
        let mut out = Vec::new();
        if !relaunching {
            return out;
        }
        for (window, layout) in self.manifest.windows.iter().enumerate() {
            for (tab, restored) in layout.restored_tabs.iter().enumerate() {
                let place = (
                    u32::try_from(window).unwrap_or(u32::MAX),
                    u32::try_from(tab).unwrap_or(u32::MAX),
                );
                walk(&restored.root, place, &mut out);
            }
        }
        out
    }

    /// The layout's terminal panes that name a folder no shell can start in
    /// (`not_found`: the check the spawn itself makes,
    /// [`crate::spawn_folder::folder_fault`]) — what the reopened row counts
    /// rather than claim every tab came back in its folder (audit #7 finding
    /// 48). The tree a window restores from: its canonical tabs, else the
    /// legacy mirror (`restore_into_window`). A pane a program held
    /// ([`crate::restore::TerminalLeafRestore::folder_may_be_remote`]) counts
    /// apart, as [`PanesWithoutFolder::unasked`]: its restore does not ask for
    /// that folder.
    pub(crate) fn panes_without_folder(
        &self,
        not_found: impl Fn(&str) -> bool,
    ) -> PanesWithoutFolder {
        fn walk(
            node: &crate::restore::RestoredSplitTree,
            not_found: &dyn Fn(&str) -> bool,
            out: &mut PanesWithoutFolder,
        ) {
            match node {
                crate::restore::RestoredSplitTree::Leaf {
                    view: crate::restore::RestoredView::Terminal(leaf),
                } if leaf.cwd.as_deref().is_some_and(not_found) => {
                    if leaf.folder_may_be_remote() {
                        out.unasked += 1;
                    } else {
                        out.gone += 1;
                    }
                }
                crate::restore::RestoredSplitTree::Leaf { .. } => {}
                crate::restore::RestoredSplitTree::Split { first, second, .. } => {
                    walk(first, not_found, out);
                    walk(second, not_found, out);
                }
            }
        }
        let mut out = PanesWithoutFolder::NONE;
        for window in &self.manifest.windows {
            if window.restored_tabs.is_empty() {
                out.gone += window
                    .tabs
                    .iter()
                    .flat_map(crate::restore::PaneLayout::leaves)
                    .filter(|leaf| leaf.cwd().is_some_and(&not_found))
                    .count();
            } else {
                for tab in &window.restored_tabs {
                    walk(&tab.root, &not_found, &mut out);
                }
            }
        }
        out
    }

    /// What was lost with the programs, or `None` when the journals did not
    /// say (today's wording), with no agent relaunched ([`Self::lost_given`]).
    pub(crate) fn lost(&self) -> Option<Lost> {
        self.lost_given(false)
    }

    /// What was lost with the programs, or `None` when the journals did not
    /// say (today's wording). A leaf is counted only when its window and tab
    /// are in the layout reopened; two programs in one tab's splits are one
    /// tab. An agent the next launch relaunches on its conversation
    /// (`relaunching`, [`Self::resumed`]) is not lost (P6a, ruling 293): an
    /// entry the writer marked as its leaf's agent ([`LeafProgram::agent`])
    /// is left out, as many in a tab as that tab has agents coming back.
    pub(crate) fn lost_given(&self, relaunching: bool) -> Option<Lost> {
        let programs = self.programs.as_ref()?;
        let mut coming_back = self.resumed(relaunching);
        let mut tabs = Vec::new();
        let mut named: Vec<String> = Vec::new();
        let mut unnamed = 0usize;
        for entry in programs {
            let Some(window) = self.manifest.windows.get(entry.window as usize) else {
                continue;
            };
            if entry.tab as usize >= window.restored_tabs.len().max(window.tabs.len()) {
                continue;
            }
            if entry.agent
                && let Some(at) = coming_back
                    .iter()
                    .position(|place| *place == (entry.window, entry.tab))
            {
                coming_back.remove(at);
                continue;
            }
            if !tabs.contains(&(entry.window, entry.tab)) {
                tabs.push((entry.window, entry.tab));
            }
            if entry.program.is_empty() {
                unnamed += 1;
            } else if !named.contains(&entry.program) {
                named.push(entry.program.clone());
            }
        }
        Some(Lost {
            tabs: tabs.len(),
            named,
            unnamed,
        })
    }
}

/// A taken journal worth a row: an unclean end's layout this launch did NOT
/// reopen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    /// It could not be read (a torn file after a power loss, a newer schema, a
    /// planted link): set aside.
    Unreadable {
        id: JournalId,
        class: DeathClass,
        path: PathBuf,
        error: String,
    },
    /// Its writer had reopened a crash journal and CRASHED within
    /// [`PROBATION`], or was stopped within it a second time in a row
    /// ([`JournalHeader::second_chance`]): skipped, so a layout that stops
    /// aterm cannot loop. What the skipped layout held rides along for the
    /// row (day five, D18: it named nothing).
    Relapsed {
        id: JournalId,
        class: DeathClass,
        /// Windows and tabs the skipped layout held.
        windows: usize,
        tabs: usize,
        /// What ran in them ([`Reopened::lost`]; `None`: its writer did not say).
        lost: Option<Lost>,
    },
}

/// What one launch's claim did.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BootClaim {
    pub(crate) reopened: Option<Reopened>,
    pub(crate) notes: Vec<Note>,
    /// Every journal this launch took, reopened or not.
    pub(crate) taken: Vec<JournalId>,
    /// Taken and set aside without a row, each with why (the log's).
    pub(crate) set_aside: Vec<(JournalId, &'static str)>,
}

/// The journal directory: beside `session.toml`, so it follows a development
/// build's `ATERM_STATE_HOME` as the manifest does (ruling 406: an instance with its
/// own state root never claims another's journals). `None` when no data dir
/// resolves, or when that seam is relative or empty (then nothing is claimed
/// and the journal is off: ruling 408).
pub(crate) fn journal_dir() -> Option<PathBuf> {
    crate::restore::manifest_path().and_then(|path| path.parent().map(Path::to_path_buf))
}

#[cfg(all(test, unix))]
pub(crate) use unix::classify_death;
#[cfg(unix)]
pub(crate) use unix::{JournalOwner, claim_at_boot};

#[cfg(not(unix))]
pub(crate) fn claim_at_boot(
    _dir: &Path,
    _evidence: &Evidence<'_>,
    _own_pid: u32,
    _quit_layout: bool,
) -> BootClaim {
    BootClaim::default()
}

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        BootClaim, DeathClass, Evidence, JournalHeader, JournalId, MAX_JOURNAL_BYTES, NAME_PREFIX,
        Note, Reopened, decode, encode, unix_nanos,
    };
    use crate::crash_signal::markers::{self, Owner};
    use crate::restore::RestoreManifest;

    /// THE WRITER'S HALF: this process's journal, its lifetime lock held.
    #[derive(Debug)]
    pub(crate) struct JournalOwner {
        dir: PathBuf,
        id: JournalId,
        /// The `flock` on `journal-<pid>-<nanos>.lock`, held until this process
        /// ends (however it ends) or [`Self::retire`] runs. Close-on-exec: a
        /// shell this process spawns must not keep it alive.
        _lock: OwnedFd,
    }

    impl JournalOwner {
        /// Arm this process's journal in `dir`: the lock file created under a
        /// pending name, locked, then renamed into place, before any image
        /// exists — so no claim ever sees a journal whose lock is free while its
        /// writer lives.
        pub(crate) fn create(dir: &Path, id: JournalId) -> Result<Self, String> {
            fs::create_dir_all(dir).map_err(|error| format!("mkdir {}: {error}", dir.display()))?;
            let (fd, _) = markers::create_locked(dir, &id.lock_name()).ok_or_else(|| {
                format!(
                    "could not create and lock {}",
                    dir.join(id.lock_name()).display()
                )
            })?;
            // SAFETY: `create_locked` returns a descriptor it opened for this
            // call and nobody else owns; the `OwnedFd` closes it exactly once.
            let lock = unsafe { OwnedFd::from_raw_fd(fd) };
            Ok(Self {
                dir: dir.to_path_buf(),
                id,
                _lock: lock,
            })
        }

        #[cfg(test)]
        pub(crate) fn id(&self) -> JournalId {
            self.id
        }

        /// The journal's path.
        pub(crate) fn path(&self) -> PathBuf {
            self.dir.join(self.id.file_name())
        }

        /// Publish one image: a create-new, no-follow, `0600` temporary, its
        /// bytes `fsync`ed, renamed over the journal's name, the directory
        /// `fsync`ed. The name holds the previous whole image until the rename,
        /// and the rename replaces a link rather than writing through it. The
        /// probation mark rides the header, so a settle is a publish too.
        #[cfg_attr(
            test,
            aterm_spec::refines(
                machine = "CrashJournalClaim",
                action = "Write",
                project = "aterm_gui::crash_journal_conformance::Rig::project"
            )
        )]
        #[cfg_attr(
            test,
            aterm_spec::refines(
                machine = "CrashJournalClaim",
                action = "Settle",
                project = "aterm_gui::crash_journal_conformance::Rig::project"
            )
        )]
        pub(crate) fn publish(
            &self,
            manifest: &RestoreManifest,
            header: &JournalHeader,
        ) -> Result<(), String> {
            let text = encode(manifest, header)?;
            publish_bytes(&self.dir, self.id, text.as_bytes())
        }

        /// Take the image down but keep the lock: `restore_session` was turned
        /// off while this process runs, and a crash must not reopen the layout.
        pub(crate) fn withdraw(&self) -> Result<(), String> {
            remove_if_present(&self.path())
        }

        /// THE CLEAN QUIT: remove the image, then the lock file, then release
        /// the lock. In that order: a death between the steps leaves a dead lock
        /// with no image, which the next claim clears, never an image with no
        /// lock to try.
        #[cfg_attr(
            test,
            aterm_spec::refines(
                machine = "CrashJournalClaim",
                action = "Quit",
                project = "aterm_gui::crash_journal_conformance::Rig::project"
            )
        )]
        pub(crate) fn retire(self) -> Result<(), String> {
            let removed = remove_if_present(&self.path());
            let _ = fs::remove_file(self.dir.join(self.id.lock_name()));
            let _ = sync_dir(&self.dir);
            drop(self);
            removed
        }
    }

    fn remove_if_present(path: &Path) -> Result<(), String> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("remove {}: {error}", path.display())),
        }
    }

    fn next_seed() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = u64::try_from(unix_nanos(std::time::SystemTime::now()) & u128::from(u64::MAX))
            .unwrap_or(0);
        ordinal ^ nanos.rotate_left(23)
    }

    fn publish_bytes(dir: &Path, id: JournalId, bytes: &[u8]) -> Result<(), String> {
        let target = dir.join(id.file_name());
        let (temporary, mut file) = create_temporary(dir, id)?;
        let result = (|| {
            file.write_all(bytes)
                .map_err(|error| format!("write {}: {error}", temporary.display()))?;
            fsync(&file).map_err(|error| format!("sync {}: {error}", temporary.display()))?;
            drop(file);
            fs::rename(&temporary, &target)
                .map_err(|error| format!("publish {}: {error}", target.display()))?;
            sync_dir(dir).map_err(|error| format!("sync {}: {error}", dir.display()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    /// A plain `fsync(2)`, not std's `F_FULLFSYNC`: the journal exists for a
    /// PROCESS death, which the page cache already survives; this orders the
    /// bytes before the rename for a machine that loses power too, without
    /// flushing the drive's cache every two seconds while a layout moves. A
    /// torn image after a power loss is read as unreadable and said.
    fn fsync(file: &File) -> std::io::Result<()> {
        // SAFETY: `fsync` on a descriptor this `File` owns for the call.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    fn sync_dir(dir: &Path) -> std::io::Result<()> {
        fsync(&File::open(dir)?)
    }

    fn create_temporary(dir: &Path, id: JournalId) -> Result<(PathBuf, File), String> {
        for _ in 0..64 {
            let candidate = dir.join(format!("{}{}.tmp", id.temp_prefix(), next_seed()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&candidate)
            {
                Ok(file) => return Ok((candidate, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("create {}: {error}", candidate.display())),
            }
        }
        Err("could not allocate a journal temporary".to_string())
    }

    /// Whether `pid` names a live process (`kill(pid, 0)`; `EPERM` is alive).
    fn pid_alive(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // SAFETY: signal 0 performs the existence and permission checks only.
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Whether `id`'s writer is gone: its lock can be taken, or — its lock file
    /// missing — its pid names no process. A lock that cannot be tried for
    /// another reason is no evidence, and the journal is left alone.
    fn writer_dead(dir: &Path, id: JournalId) -> bool {
        let lock = dir.join(id.lock_name());
        match markers::probe(&lock) {
            Owner::Dead => true,
            Owner::Live => false,
            Owner::Unknown => {
                matches!(fs::symlink_metadata(&lock), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
                    && !pid_alive(id.pid)
            }
        }
    }

    /// HOW `id`'S WRITER ENDED, by its run's crash evidence in the log dir —
    /// `None` when nothing says the end was unclean, which is what every clean
    /// exit leaves: `atexit` and `clean_exit_now` remove the run's own crash
    /// marker (the quit, and the update parent's `_exit` after Commit). Only
    /// evidence of THIS run counts: a marker of the same pid named for a later
    /// start than the journal's (a later process that reused the pid) is not
    /// its, and everything older was consumed or swept at the writer's own
    /// windowed boot.
    ///
    /// * a panic report `crash-<pid>.log` written after the journal's start —
    ///   [`DeathClass::Panic`] (the unwind ran `atexit`, so no marker is left);
    /// * the run's marker `crash-marker-<pid>-<nanos>-*.log`, the newest named
    ///   no later than the journal: non-empty — [`DeathClass::Signal`]; empty —
    ///   [`DeathClass::Killed`] (its owner is dead, and never removed it);
    /// * a legacy non-empty `crash-signal-<pid>-<nanos>.log` — a signal;
    /// * a dead empty marker of the run that this launch's own install-time
    ///   sweep removed before the claim (a dev or test start's, which is no row
    ///   but is still this evidence) — killed;
    /// * the run's OWN marker, exactly (its `<nanos>` is the journal's, which
    ///   [`super::JournalId::of_this_run`] names it for), under the `.seen` name
    ///   an earlier windowed launch that did not claim gave it when it read the
    ///   evidence — an update's successor, a launch with `restore_session` off —
    ///   by its length as above. Only the exact marker: an older run of the same
    ///   pid keeps its consumed marker for eight launches. A consumed PANIC
    ///   report is not read: a worker thread's panic files one while the
    ///   process lives on, so one under `.seen` says nothing about the end.
    pub(crate) fn classify_death(evidence: &Evidence<'_>, id: JournalId) -> Option<DeathClass> {
        let mut class = evidence
            .swept
            .iter()
            .any(|marker| marker.pid == id.pid && marker.nanos <= id.nanos)
            .then_some(DeathClass::Killed);
        let Some(log_dir) = evidence.log_dir else {
            return class;
        };
        let Ok(entries) = fs::read_dir(log_dir) else {
            return class;
        };
        let armed = std::time::UNIX_EPOCH
            + std::time::Duration::from_nanos(u64::try_from(id.nanos).unwrap_or(u64::MAX));
        let panic_report = format!("crash-{}.log", id.pid);
        let legacy_prefix = format!("crash-signal-{}-", id.pid);
        let mut own_marker: Option<(u128, u64)> = None;
        let mut own_consumed: Option<u64> = None;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // `DirEntry::metadata` does not traverse a link on unix.
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.file_type().is_file() {
                continue;
            }
            if name == panic_report {
                if meta.len() > 0 && meta.modified().is_ok_and(|modified| modified >= armed) {
                    class = class.max(Some(DeathClass::Panic));
                }
            } else if let Some(marker) = markers::parse(name) {
                if marker.pid == id.pid
                    && marker.nanos <= id.nanos
                    && own_marker.is_none_or(|(nanos, _)| marker.nanos > nanos)
                {
                    own_marker = Some((marker.nanos, meta.len()));
                }
            } else if let Some(consumed) = name.strip_suffix(".seen").and_then(markers::parse) {
                if consumed.pid == id.pid && consumed.nanos == id.nanos {
                    own_consumed = Some(meta.len());
                }
            } else if name.starts_with(&legacy_prefix)
                && name.ends_with(".log")
                && meta.len() > 0
                && name[legacy_prefix.len()..name.len() - 4]
                    .parse::<u128>()
                    .is_ok_and(|nanos| nanos <= id.nanos)
            {
                class = class.max(Some(DeathClass::Signal));
            }
        }
        if let Some(len) = own_marker.map(|(_, len)| len).or(own_consumed) {
            class = class.max(Some(if len > 0 {
                DeathClass::Signal
            } else {
                DeathClass::Killed
            }));
        }
        class
    }

    /// Read a taken journal: no link followed, a regular file, bounded.
    fn read_taken(path: &Path) -> Result<String, String> {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| "a NUL in its path".to_string())?;
        // SAFETY: `c_path` is a live NUL-terminated string for the call.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(format!("not opened ({})", std::io::Error::last_os_error()));
        }
        // SAFETY: `fd` was opened above and is owned by this `File` from here.
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file
            .metadata()
            .map_err(|error| format!("not inspected ({error})"))?;
        if !meta.file_type().is_file() {
            return Err("not a regular file".to_string());
        }
        if meta.len() > MAX_JOURNAL_BYTES || meta.size() > MAX_JOURNAL_BYTES {
            return Err(format!("{} bytes, more than any layout", meta.len()));
        }
        let mut text = String::new();
        file.take(MAX_JOURNAL_BYTES)
            .read_to_string(&mut text)
            .map_err(|error| format!("not read ({error})"))?;
        Ok(text)
    }

    /// One candidate an unclean end left.
    struct Candidate {
        id: JournalId,
        class: DeathClass,
        header: JournalHeader,
        manifest: RestoreManifest,
    }

    /// THE READER'S HALF: take every dead writer's journal in `dir` and decide
    /// what this launch reopens (the rules on this module). `quit_layout` is
    /// whether this launch already has the last quit's `session.toml` to
    /// reopen, which wins; `own_pid` names this launch's claim temporaries.
    ///
    /// Every journal taken is renamed away before it is read and removed after,
    /// so it is single-use whatever this launch then does — a crash mid-restore
    /// cannot reopen it again, and a second launch racing this one finds
    /// nothing under the name. A journal whose writer's lock is held is never
    /// renamed. Leftovers of dead writers and dead claimers (a lock with no
    /// journal, a write temporary, a claim file) are cleared on the way.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "CrashJournalClaim",
            action = "Claim",
            project = "aterm_gui::crash_journal_conformance::Rig::project"
        )
    )]
    pub(crate) fn claim_at_boot(
        dir: &Path,
        evidence: &Evidence<'_>,
        own_pid: u32,
        quit_layout: bool,
    ) -> BootClaim {
        let mut claim = BootClaim::default();
        let Ok(entries) = fs::read_dir(dir) else {
            return claim;
        };
        let mut journals = Vec::new();
        let mut locks = Vec::new();
        let mut leftovers = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(id) = JournalId::parse(name) {
                journals.push(id);
            } else if let Some(id) = JournalId::parse_lock(name) {
                locks.push(id);
            } else if name.starts_with(&format!(".{NAME_PREFIX}")) {
                leftovers.push(name.to_string());
            }
        }
        journals.sort_unstable();
        let mut candidates = Vec::new();
        for id in journals {
            if !writer_dead(dir, id) {
                continue;
            }
            let Ok(taken) = take(dir, id, own_pid) else {
                continue;
            };
            claim.taken.push(id);
            let _ = fs::remove_file(dir.join(id.lock_name()));
            let read = read_taken(&taken);
            let _ = fs::remove_file(&taken);
            if quit_layout {
                claim
                    .set_aside
                    .push((id, "the last quit's layout was reopened instead"));
                continue;
            }
            let Some(class) = classify_death(evidence, id) else {
                claim
                    .set_aside
                    .push((id, "no crash evidence of its run (a clean end leaves none)"));
                continue;
            };
            match read.and_then(|text| decode(&text)) {
                Err(error) => claim.notes.push(Note::Unreadable {
                    id,
                    class,
                    path: dir.join(id.file_name()),
                    error,
                }),
                // THE BRAKE (ruling 285): a crash inside the probation, or a
                // second stop in a row inside it.
                Ok((header, manifest))
                    if header.probation && (class.crashed() || header.second_chance) =>
                {
                    let skipped = Reopened {
                        writers: vec![id.pid; manifest.windows.len()],
                        manifest,
                        class,
                        sources: vec![id],
                        programs: header.programs,
                        second_chance: header.second_chance,
                    };
                    let (windows, tabs) = skipped.counts();
                    claim.notes.push(Note::Relapsed {
                        id,
                        class,
                        windows,
                        tabs,
                        lost: skipped.lost(),
                    });
                }
                Ok((_, manifest)) if manifest.is_empty() => {
                    claim.set_aside.push((id, "its layout was empty"));
                }
                Ok((header, manifest)) => candidates.push(Candidate {
                    id,
                    class,
                    header,
                    manifest,
                }),
            }
        }
        // A dead writer's lock with no journal beside it (it died before its
        // first image, or between the quit's two removals).
        for id in locks {
            if !dir.join(id.file_name()).exists() && writer_dead(dir, id) {
                let _ = fs::remove_file(dir.join(id.lock_name()));
            }
        }
        clear_leftovers(dir, &leftovers);
        claim.reopened = merge(candidates, &mut claim.set_aside);
        let _ = sync_dir(dir);
        claim
    }

    /// Rename `id`'s journal to a claim name only this launch uses. `Err` when
    /// it is gone (another launch took it) or cannot be renamed.
    fn take(dir: &Path, id: JournalId, own_pid: u32) -> Result<PathBuf, ()> {
        let from = dir.join(id.file_name());
        for _ in 0..64 {
            let to = dir.join(format!(
                ".{}.claimed-{own_pid}-{}.tmp",
                id.file_name(),
                next_seed()
            ));
            if fs::symlink_metadata(&to).is_ok() {
                continue;
            }
            return match fs::rename(&from, &to) {
                Ok(()) => Ok(to),
                Err(error) => {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        crate::logging::stderr_line!(
                            "aterm-gui: crash journal {} not taken: {error}",
                            from.display()
                        );
                    }
                    Err(())
                }
            };
        }
        Err(())
    }

    /// Clear the dotted leftovers of writers and claimers that are gone: a
    /// write temporary whose journal's writer is dead, a claim file whose
    /// claimer is, a pending lock nobody holds.
    fn clear_leftovers(dir: &Path, names: &[String]) {
        for name in names {
            let path = dir.join(name);
            let dead = if let Some(rest) = name.strip_prefix(&format!(".{NAME_PREFIX}")) {
                if let Some((stem, _)) = rest.split_once(".w-") {
                    JournalId::parse_stem(stem).is_some_and(|id| writer_dead(dir, id))
                } else if let Some((_, claimer)) = rest.split_once(".claimed-") {
                    claimer
                        .split('-')
                        .next()
                        .and_then(|pid| pid.parse::<u32>().ok())
                        .is_some_and(|pid| !pid_alive(pid))
                } else if rest.ends_with(".lock.pending") {
                    markers::probe(&path) == Owner::Dead
                } else {
                    false
                }
            } else {
                false
            };
            if dead {
                let _ = fs::remove_file(&path);
            }
        }
    }

    /// Every candidate's windows in one layout, newest writer first, up to the
    /// window cap a layout may carry; `None` with none.
    fn merge(
        mut candidates: Vec<Candidate>,
        set_aside: &mut Vec<(JournalId, &'static str)>,
    ) -> Option<Reopened> {
        candidates.sort_by(|a, b| {
            (b.header.written_unix_ms, b.id.nanos).cmp(&(a.header.written_unix_ms, a.id.nanos))
        });
        let newest = candidates.first()?;
        let class = newest.class;
        let mut windows = Vec::new();
        let mut sources = Vec::new();
        let mut writers = Vec::new();
        let mut programs = Some(Vec::new());
        let mut second_chance = false;
        for candidate in candidates {
            if windows.len() + candidate.manifest.windows.len() > crate::restore::MAX_WINDOWS {
                set_aside.push((candidate.id, "more windows than a layout carries"));
                continue;
            }
            // One source that did not say makes the whole layout unknown.
            let offset = u32::try_from(windows.len()).unwrap_or(u32::MAX);
            programs = programs
                .zip(candidate.header.programs)
                .map(|(mut all, own)| {
                    all.extend(own.into_iter().map(|mut entry| {
                        entry.window = entry.window.saturating_add(offset);
                        entry
                    }));
                    all
                });
            // A writer on probation that reached here was stopped, once
            // (the brake skipped every other): this is its second chance.
            second_chance |= candidate.header.probation;
            sources.push(candidate.id);
            writers.extend(std::iter::repeat_n(
                candidate.id.pid,
                candidate.manifest.windows.len(),
            ));
            windows.extend(candidate.manifest.windows);
        }
        Some(Reopened {
            manifest: RestoreManifest::new(windows),
            class,
            sources,
            writers,
            programs,
            second_chance,
        })
    }
}

/// THE WRITER'S LANE on the App: when to capture, whether a capture is worth a
/// write, the brake's probation, and the writer thread that does the I/O. The
/// main thread only captures and compares; every file operation is the
/// thread's.
#[derive(Debug)]
pub(crate) struct Lane {
    /// The journal directory, or `None` for an inert lane (headless, tests, no
    /// data dir, a platform without the lock).
    dir: Option<PathBuf>,
    pid: u32,
    /// Something may have changed since the last capture.
    dirty: bool,
    /// No capture before this instant.
    not_before: Option<Instant>,
    /// This launch reopened a crash journal; until this instant its own
    /// journal carries the probation mark.
    probation_until: Option<Instant>,
    /// That journal was a second chance ([`Reopened::second_chance`]): the
    /// probation mark carries [`JournalHeader::second_chance`] too.
    second_chance: bool,
    /// The image last handed to the writer, and when.
    last: Option<Written>,
    published_at: Option<Instant>,
    /// A capture found only titles moved, inside [`TITLE_WRITE_INTERVAL`]: one
    /// more capture is owed at that interval's end.
    title_owed: bool,
    writer: Option<writer::Writer>,
    /// Images handed to the writer (the M9 write count).
    pub(crate) writes: u64,
    /// Captures taken.
    pub(crate) captures: u64,
}

/// The image last handed to the writer.
#[derive(Clone, Debug, PartialEq)]
struct Written {
    manifest: RestoreManifest,
    probation: bool,
    /// The leaves that had a program running (D16): a program starting or
    /// ending is a change worth an image, like a moved folder.
    programs: Vec<LeafProgram>,
}

impl Lane {
    /// A lane that writes into `dir` as `pid`, or an inert one for `None`.
    pub(crate) fn new(dir: Option<PathBuf>, pid: u32) -> Self {
        Self {
            dir: if cfg!(unix) { dir } else { None },
            pid,
            dirty: true,
            not_before: None,
            probation_until: None,
            second_chance: false,
            last: None,
            published_at: None,
            title_owed: false,
            writer: None,
            writes: 0,
            captures: 0,
        }
    }

    /// A lane that never writes (the test App's, which the frame-latency bench
    /// builds too).
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn inert() -> Self {
        Self::new(None, 0)
    }

    pub(crate) fn is_active(&self) -> bool {
        self.dir.is_some()
    }

    /// Something that was not a timer woke the loop: the layout, a cwd or a
    /// title may have moved.
    pub(crate) fn note_activity(&mut self) {
        self.dirty = true;
    }

    /// Whether a capture was owed, clearing it (tests of what marks it).
    #[cfg(test)]
    pub(crate) fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// This launch reopened a crash journal at `now`: its own journal carries
    /// the probation mark until [`PROBATION`] has passed — marked a second
    /// chance's when the brake let that journal through once
    /// ([`Reopened::second_chance`], ruling 285).
    pub(crate) fn begin_probation(&mut self, now: Instant, second_chance: bool) {
        self.probation_until = Some(now + PROBATION);
        self.second_chance = second_chance;
    }

    fn on_probation(&self, now: Instant) -> bool {
        self.probation_until.is_some_and(|until| now < until)
    }

    /// The last image carries a probation mark that has run out: rewrite it.
    fn settle_due(&self, now: Instant) -> bool {
        self.last
            .as_ref()
            .is_some_and(|written| written.probation && !self.on_probation(now))
    }

    /// When an owed title-only image may be written.
    fn title_due_at(&self) -> Option<Instant> {
        self.title_owed
            .then(|| self.published_at.map(|at| at + TITLE_WRITE_INTERVAL))
            .flatten()
    }

    /// Whether a capture is due now.
    pub(crate) fn capture_due(&self, now: Instant) -> bool {
        self.dir.is_some()
            && self.not_before.is_none_or(|at| now >= at)
            && (self.dirty
                || self.settle_due(now)
                || self.title_due_at().is_some_and(|at| now >= at))
    }

    /// Offer a fresh capture taken at `now`. A leaf whose cwd and title came
    /// back empty because its engine was busy (`restore_session_meta`'s
    /// try-lock) keeps what the last image had for it; the result is handed to
    /// the writer only when it differs from the last image, or the probation
    /// mark has changed — and when only titles differ, not before
    /// [`TITLE_WRITE_INTERVAL`] since the last image (the capture is owed then
    /// instead). `programs` names the leaves that had a program running
    /// ([`JournalHeader::programs`]); a change there is never title-only.
    /// Answers whether it was handed over.
    pub(crate) fn offer(
        &mut self,
        mut manifest: RestoreManifest,
        programs: Vec<LeafProgram>,
        now: Instant,
    ) -> bool {
        self.captures = self.captures.saturating_add(1);
        self.dirty = false;
        self.not_before = Some(now + MIN_WRITE_INTERVAL);
        if let Some(last) = &self.last {
            heal_busy_leaves(&mut manifest, &last.manifest);
        }
        let written = Written {
            manifest,
            probation: self.on_probation(now),
            programs,
        };
        if self.last.as_ref() == Some(&written) {
            self.title_owed = false;
            return false;
        }
        if let Some(last) = &self.last
            && last.probation == written.probation
            && last.programs == written.programs
            && same_but_titles(&last.manifest, &written.manifest)
            && self
                .published_at
                .is_some_and(|at| now < at + TITLE_WRITE_INTERVAL)
        {
            self.title_owed = true;
            return false;
        }
        let Some(dir) = self.dir.clone() else {
            return false;
        };
        let header = JournalHeader::now(self.pid, written.probation)
            .with_second_chance(written.probation && self.second_chance)
            .with_programs(written.programs.clone());
        let pid = self.pid;
        let writer = self
            .writer
            .get_or_insert_with(|| writer::Writer::spawn(dir, JournalId::of_this_run(pid)));
        writer.publish(written.manifest.clone(), header);
        self.last = Some(written);
        self.published_at = Some(now);
        self.title_owed = false;
        self.writes = self.writes.saturating_add(1);
        true
    }

    /// `restore_session` went off while this process runs: take the image
    /// down (the lock stays, so the journal can come back).
    pub(crate) fn withdraw(&mut self) {
        self.title_owed = false;
        if self.last.take().is_some() {
            if let Some(writer) = self.writer.as_ref() {
                writer.withdraw();
            }
            // Turned back on, the layout is captured again at once.
            self.dirty = true;
            self.not_before = None;
        }
    }

    /// The next instant the loop must wake for this lane — the trailing edge
    /// of a pending capture, the end of probation with a marked image, an owed
    /// title — each no earlier than the write interval allows, and never one
    /// already past, which the capture that is due now consumes.
    pub(crate) fn next_wake(&self, now: Instant) -> Option<Instant> {
        self.dir.as_ref()?;
        let floor = |at: Instant| self.not_before.map_or(at, |not_before| not_before.max(at));
        let capture = self.dirty.then_some(now);
        let settle = self
            .probation_until
            .filter(|_| self.last.as_ref().is_some_and(|w| w.probation));
        [capture, settle, self.title_due_at()]
            .into_iter()
            .flatten()
            .map(floor)
            .filter(|at| *at > now)
            .min()
    }

    /// THE CLEAN QUIT'S HALF: remove this process's journal and wait for it,
    /// at most `patience` (a wedged disk must not hold the exit). A lane that
    /// never wrote has nothing to remove.
    pub(crate) fn retire(&mut self, patience: Duration) -> Result<(), String> {
        self.last = None;
        match self.writer.take() {
            Some(writer) => writer.retire(patience),
            None => Ok(()),
        }
    }
}

/// Whether `a` and `b` differ in nothing but their terminal leaves' titles (the
/// engine's OSC title; the user's `meta set` title is the layout's).
fn same_but_titles(a: &RestoreManifest, b: &RestoreManifest) -> bool {
    use crate::restore::{PaneLayout, RestoredSplitTree, RestoredView};
    fn blank_tree(node: &mut RestoredSplitTree) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } => leaf.title.clear(),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                blank_tree(first);
                blank_tree(second);
            }
        }
    }
    fn blank_pane(node: &mut PaneLayout) {
        match node {
            PaneLayout::Leaf { title, .. } => title.clear(),
            PaneLayout::Split { first, second, .. } => {
                blank_pane(first);
                blank_pane(second);
            }
        }
    }
    fn blank(manifest: &RestoreManifest) -> RestoreManifest {
        let mut manifest = manifest.clone();
        for window in &mut manifest.windows {
            for tab in &mut window.restored_tabs {
                blank_tree(&mut tab.root);
            }
            for pane in &mut window.tabs {
                blank_pane(pane);
            }
        }
        manifest
    }
    a == b || blank(a) == blank(b)
}

/// Give each terminal leaf of `fresh` whose cwd and title both came back empty
/// — the signature of a capture that met its engine busy — what `last` had for
/// the same session. A program cannot un-report a directory, so an empty cwd
/// after a reported one is always the busy engine, never news.
fn heal_busy_leaves(fresh: &mut RestoreManifest, last: &RestoreManifest) {
    use std::collections::BTreeMap;

    use crate::restore::{PaneLayout, RestoredSplitTree, RestoredView};
    /// What a session's leaf last said: its cwd and title, by `local_id`.
    type Known = BTreeMap<u64, (Option<String>, String)>;
    fn collect(node: &RestoredSplitTree, out: &mut Known) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } => {
                if let Some(id) = leaf.local_id {
                    out.insert(id, (leaf.cwd.clone(), leaf.title.clone()));
                }
            }
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                collect(first, out);
                collect(second, out);
            }
        }
    }
    fn heal_leaf(cwd: &mut Option<String>, title: &mut String, id: Option<u64>, known: &Known) {
        if cwd.is_none()
            && title.is_empty()
            && let Some((before_cwd, before_title)) = id.and_then(|id| known.get(&id))
        {
            cwd.clone_from(before_cwd);
            title.clone_from(before_title);
        }
    }
    fn heal_tree(node: &mut RestoredSplitTree, known: &Known) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } => heal_leaf(&mut leaf.cwd, &mut leaf.title, leaf.local_id, known),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                heal_tree(first, known);
                heal_tree(second, known);
            }
        }
    }
    // The legacy mirror (`WindowLayout::tabs`) carries the same leaves.
    fn heal_pane(node: &mut PaneLayout, known: &Known) {
        match node {
            PaneLayout::Leaf {
                cwd,
                title,
                local_id,
                ..
            } => heal_leaf(cwd, title, *local_id, known),
            PaneLayout::Split { first, second, .. } => {
                heal_pane(first, known);
                heal_pane(second, known);
            }
        }
    }
    let mut known = Known::new();
    for window in &last.windows {
        for tab in &window.restored_tabs {
            collect(&tab.root, &mut known);
        }
    }
    if known.is_empty() {
        return;
    }
    for window in &mut fresh.windows {
        for tab in &mut window.restored_tabs {
            heal_tree(&mut tab.root, &known);
        }
        for pane in &mut window.tabs {
            heal_pane(pane, &known);
        }
    }
}

/// The writer thread: owns the journal (and its lock) once it has written
/// once, publishes the newest image it has been handed, and ends at the
/// quit's retire.
mod writer {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{JournalHeader, JournalId};
    use crate::restore::RestoreManifest;

    enum Job {
        Publish(Box<RestoreManifest>, JournalHeader),
        Withdraw,
        Retire(mpsc::Sender<Result<(), String>>),
    }

    #[derive(Debug)]
    pub(super) struct Writer {
        tx: mpsc::Sender<Job>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl std::fmt::Debug for Job {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(match self {
                Self::Publish(..) => "Publish",
                Self::Withdraw => "Withdraw",
                Self::Retire(_) => "Retire",
            })
        }
    }

    impl Writer {
        pub(super) fn spawn(dir: PathBuf, id: JournalId) -> Self {
            let (tx, rx) = mpsc::channel::<Job>();
            let thread = std::thread::Builder::new()
                .name("aterm-crash-journal".into())
                .spawn(move || run(&dir, id, &rx))
                .map_err(|error| {
                    aterm_log::warn!("crash journal: writer thread not spawned ({error})");
                })
                .ok();
            Self { tx, thread }
        }

        pub(super) fn publish(&self, manifest: RestoreManifest, header: JournalHeader) {
            let _ = self.tx.send(Job::Publish(Box::new(manifest), header));
        }

        pub(super) fn withdraw(&self) {
            let _ = self.tx.send(Job::Withdraw);
        }

        pub(super) fn retire(mut self, patience: Duration) -> Result<(), String> {
            let (reply, answer) = mpsc::channel();
            if self.tx.send(Job::Retire(reply)).is_err() || self.thread.is_none() {
                return Err("the journal writer is not running".to_string());
            }
            match answer.recv_timeout(patience) {
                Ok(result) => {
                    if let Some(thread) = self.thread.take() {
                        let _ = thread.join();
                    }
                    result
                }
                Err(_) => Err(format!(
                    "the journal writer did not answer within {} ms",
                    patience.as_millis()
                )),
            }
        }
    }

    #[cfg(unix)]
    fn run(dir: &std::path::Path, id: JournalId, rx: &mpsc::Receiver<Job>) {
        crate::qos::set_self(crate::qos::Role::Background);
        let mut owner: Option<super::JournalOwner> = None;
        let mut said = false;
        while let Ok(mut job) = rx.recv() {
            // The newest state wins: a burst of images is one write, and a
            // withdraw after an image (or an image after a withdraw) is the
            // later one. A retire is always the last job sent.
            while let Ok(next) = rx.try_recv() {
                job = next;
            }
            match job {
                Job::Publish(manifest, header) => {
                    if owner.is_none() {
                        match super::JournalOwner::create(dir, id) {
                            Ok(created) => owner = Some(created),
                            Err(error) if !said => {
                                said = true;
                                aterm_log::warn!("crash journal: not armed ({error})");
                            }
                            Err(_) => {}
                        }
                    }
                    if let Some(owner) = owner.as_ref()
                        && let Err(error) = owner.publish(&manifest, &header)
                        && !said
                    {
                        said = true;
                        aterm_log::warn!("crash journal: not written ({error})");
                    }
                }
                Job::Withdraw => {
                    if let Some(owner) = owner.as_ref()
                        && let Err(error) = owner.withdraw()
                    {
                        aterm_log::warn!("crash journal: not withdrawn ({error})");
                    }
                }
                Job::Retire(reply) => {
                    let _ = reply.send(owner.take().map_or(Ok(()), super::JournalOwner::retire));
                    return;
                }
            }
        }
    }

    #[cfg(not(unix))]
    fn run(_dir: &std::path::Path, _id: JournalId, rx: &mpsc::Receiver<Job>) {
        while let Ok(job) = rx.recv() {
            if let Job::Retire(reply) = job {
                let _ = reply.send(Ok(()));
                return;
            }
        }
    }
}

/// The layouts the tests build, on EVERY platform: `message_reporters`'
/// tests draw a reopened layout with them, and those compile off unix too,
/// where [`tests`] (the unix lane's files, locks and claim) does not exist.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::restore::{self, RestoreManifest};

    /// One terminal leaf as the capture writes it.
    fn leaf(local: u64, cwd: Option<&str>, title: &str) -> restore::RestoredSplitTree {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                cwd: cwd.map(str::to_string),
                title: title.to_string(),
                profile: None,
                local_id: Some(local),
                user_title: None,
                description: None,
                icon: None,
                role: None,
                attention: None,
                questions: None,
                identity: None,
                agent: None,
                held: false,
            },
        ))
    }

    /// A one-window layout: one tab per `(cwd, title)`.
    pub(crate) fn layout(tabs: &[(&str, &str)]) -> RestoreManifest {
        RestoreManifest::new(vec![restore::WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            show: restore::WindowShow::UNKNOWN,
            tabs: Vec::new(),
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: Some(0),
            restored_tabs: tabs
                .iter()
                .enumerate()
                .map(|(index, (cwd, title))| restore::RestoredTab {
                    root: leaf(index as u64, Some(cwd), title),
                    focused_path: Vec::new(),
                    zoomed: false,
                })
                .collect(),
        }])
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    pub(crate) use super::fixtures::layout;
    use super::{
        BootClaim, DeathClass, Evidence, JournalHeader, JournalId, JournalOwner, Lane,
        MIN_WRITE_INTERVAL, Note, PROBATION, PanesWithoutFolder, TITLE_WRITE_INTERVAL,
        claim_at_boot, classify_death, decode, encode, same_but_titles,
    };
    use crate::crash_signal::{MarkerOwner, markers};
    use crate::restore::{self, RestoreManifest};

    pub(crate) fn scratch(tag: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(tag)
            .tempdir()
            .expect("scratch dir")
    }

    /// The cwds a layout reopens, in order.
    pub(crate) fn cwds(manifest: &RestoreManifest) -> Vec<String> {
        manifest
            .windows
            .iter()
            .flat_map(|window| &window.restored_tabs)
            .filter_map(|tab| match &tab.root {
                restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Terminal(leaf),
                } => leaf.cwd.clone(),
                _ => None,
            })
            .collect()
    }

    /// A pid that WAS a process and is not one any more.
    pub(crate) fn reaped_pid() -> u32 {
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    }

    /// The crash marker a run arms at start: empty, named for its pid and its
    /// start — the start its journal is named for too
    /// (`JournalId::of_this_run`).
    pub(crate) fn marker(logs: &Path, id: JournalId) -> PathBuf {
        let path = logs.join(markers::file_name(id.pid, id.nanos, MarkerOwner::Other));
        std::fs::write(&path, b"").unwrap();
        path
    }

    fn evidence(logs: &Path) -> Evidence<'_> {
        Evidence {
            log_dir: Some(logs),
            swept: &[],
        }
    }

    /// A run (a dead pid's, so its lock dies with the owner value) that armed
    /// its journal and published `manifest`, and its crash marker.
    fn run(dir: &Path, logs: &Path, manifest: &RestoreManifest, probation: bool) -> JournalOwner {
        let id = JournalId::now(reaped_pid());
        marker(logs, id);
        let owner = JournalOwner::create(dir, id).expect("armed");
        owner
            .publish(manifest, &JournalHeader::now(id.pid, probation))
            .expect("published");
        owner
    }

    /// [`run`] with the header its writer wrote (D16: which leaves ran a
    /// program).
    fn run_said(
        dir: &Path,
        logs: &Path,
        manifest: &RestoreManifest,
        programs: Option<Vec<super::LeafProgram>>,
    ) -> JournalOwner {
        let id = JournalId::now(reaped_pid());
        marker(logs, id);
        let owner = JournalOwner::create(dir, id).expect("armed");
        let mut header = JournalHeader::now(id.pid, false);
        header.programs = programs;
        owner.publish(manifest, &header).expect("published");
        owner
    }

    fn vim_in(window: u32, tab: u32) -> super::LeafProgram {
        super::LeafProgram {
            window,
            tab,
            program: "vim".to_string(),
            agent: false,
        }
    }

    /// D16 (ruling 282), TIER-1 through the real journal: an image whose
    /// writer said no leaf ran a program is written, the writer killed, the
    /// journal claimed and read back, and the reopened row is a RECORD. The
    /// negative controls through the same files: a leaf that ran vim keeps the
    /// warning and names it, and an image with no word on programs (an older
    /// writer's) reads as today's "programs lost".
    #[test]
    fn a_relaunch_whose_tabs_sat_at_their_prompts_is_quiet_through_the_journal() {
        use crate::message_reporters::{
            JOURNAL_LOSS, JOURNAL_NOTHING_RAN, journal_reopened_message,
        };
        use aterm_messages::{Hold, Severity};
        let manifest = layout(&[("/work/a", "zsh"), ("/work/b", "zsh")]);
        let reopen = |programs: Option<Vec<super::LeafProgram>>| {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            die(run_said(dir.path(), logs.path(), &manifest, programs));
            claim_here(dir.path(), logs.path())
                .reopened
                .expect("reopened")
        };

        let quiet = reopen(Some(Vec::new()));
        assert_eq!(
            quiet.programs,
            Some(Vec::new()),
            "`programs = []` round-trips"
        );
        let row =
            journal_reopened_message(&quiet, None, None, None, false, PanesWithoutFolder::NONE);
        assert_eq!((row.severity, row.hold), (Severity::Info, Hold::LogOnly));
        assert_eq!(row.detail[0], JOURNAL_NOTHING_RAN);

        let lost = reopen(Some(vec![vim_in(0, 1)]));
        assert_eq!(lost.programs, Some(vec![vim_in(0, 1)]));
        let row =
            journal_reopened_message(&lost, None, None, None, false, PanesWithoutFolder::NONE);
        assert_eq!(row.severity, Severity::Warn);
        assert_ne!(row.hold, Hold::LogOnly);
        assert!(row.detail[0].starts_with("vim was running in 1 of 2 tabs"));

        let older = reopen(None);
        assert_eq!(older.programs, None);
        let row =
            journal_reopened_message(&older, None, None, None, false, PanesWithoutFolder::NONE);
        assert_eq!(row.severity, Severity::Warn);
        assert_eq!(row.detail[0], JOURNAL_LOSS);
    }

    /// AUDIT #7 FINDING 48, TIER-1 through the real journal: a layout whose
    /// second tab's folder is gone by the time it is reopened is written, the
    /// writer killed, the journal claimed and read back — and the reopened row
    /// COUNTS the pane that could not open its folder, by the check the spawn
    /// itself makes (`spawn_folder::folder_fault`), instead of saying both
    /// tabs came back in their folders, and the loss sentence above it no
    /// longer says ONLY the scrollback was lost (audit #8). NEGATIVE CONTROL
    /// through the same files: with both folders there the row reads as it
    /// always did. The same pane while ssh held it is not counted, and the
    /// row claims no folders: that folder may be another machine's, and its
    /// shell starts in the default folder (audit #8) — unless the pane
    /// carries the agent aterm hosted there, which ran on this machine.
    #[test]
    fn a_reopened_layout_counts_the_panes_whose_folder_is_gone() {
        use crate::message_reporters::{
            JOURNAL_NOTHING_RAN, JOURNAL_NOTHING_RAN_FOLDERLESS, journal_reopened_message,
        };
        let folders = scratch("cj-folders");
        let here = folders.path().join("here");
        let gone = folders.path().join("gone");
        std::fs::create_dir(&here).unwrap();
        std::fs::create_dir(&gone).unwrap();
        let (here, gone_str) = (here.to_str().unwrap(), gone.to_str().unwrap());
        let manifest = layout(&[(here, "zsh"), (gone_str, "zsh")]);
        let reopen_from = |manifest: &RestoreManifest, programs: Vec<super::LeafProgram>| {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            die(run_said(dir.path(), logs.path(), manifest, Some(programs)));
            claim_here(dir.path(), logs.path())
                .reopened
                .expect("reopened")
        };
        let reopen = || reopen_from(&manifest, Vec::new());
        let spawn_verdict = |dir: &str| crate::spawn_folder::folder_fault(dir).is_some();
        let counted = |gone, unasked| PanesWithoutFolder { gone, unasked };

        let whole = reopen();
        assert_eq!(
            whole.panes_without_folder(spawn_verdict),
            PanesWithoutFolder::NONE
        );
        let row =
            journal_reopened_message(&whole, None, None, None, false, PanesWithoutFolder::NONE);
        assert_eq!(row.detail[0], JOURNAL_NOTHING_RAN);
        assert_eq!(
            row.detail[1],
            "2 tabs in 1 window restored in their folders from its crash journal"
        );

        std::fs::remove_dir(&gone).unwrap();
        let reopened = reopen();
        let homeless = reopened.panes_without_folder(spawn_verdict);
        assert_eq!(homeless, counted(1, 0), "the tab whose folder is gone");
        let row = journal_reopened_message(&reopened, None, None, None, false, homeless);
        assert_eq!(row.detail[0], JOURNAL_NOTHING_RAN_FOLDERLESS);
        assert_eq!(
            row.detail[1],
            "2 tabs in 1 window restored from its crash journal; 1 pane could not open its folder"
        );
        assert!(
            row.detail
                .iter()
                .all(|line| !line.contains("in their folders")),
            "{:?}",
            row.detail
        );

        // The same pane while ssh held it (audit #8): that folder may be
        // another machine's, so it is not counted, and the restored line
        // claims no folders. `held` rides the journal's files.
        let mut ssh = manifest.clone();
        if let restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(leaf),
        } = &mut ssh.windows[0].restored_tabs[1].root
        {
            leaf.held = true;
        }
        let ssh_ran = || {
            vec![super::LeafProgram {
                window: 0,
                tab: 1,
                program: "ssh".to_string(),
                agent: false,
            }]
        };
        let reopened = reopen_from(&ssh, ssh_ran());
        assert!(
            matches!(
                &reopened.manifest.windows[0].restored_tabs[1].root,
                restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Terminal(leaf),
                } if leaf.held
            ),
            "held survives the journal"
        );
        let unasked = reopened.panes_without_folder(spawn_verdict);
        assert_eq!(unasked, counted(0, 1));
        let row = journal_reopened_message(&reopened, None, None, None, false, unasked);
        assert!(
            row.detail[0].starts_with("ssh was running in 1 of 2 tabs"),
            "{:?}",
            row.detail
        );
        assert_eq!(
            row.detail[1],
            "2 tabs in 1 window restored from its crash journal"
        );
        // Unless the pane carries the agent aterm hosted there: it ran on
        // this machine, in that folder, so the gone folder is counted.
        let mut hosted = ssh.clone();
        if let restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(leaf),
        } = &mut hosted.windows[0].restored_tabs[1].root
        {
            leaf.agent = Some(Box::new(restore::AgentRestore {
                pid: 4242,
                start: "Sat Sep 27 01:02:03 2026".into(),
                program: "/opt/claude/bin/claude".into(),
                argv: vec!["/opt/claude/bin/claude".into()],
                session: None,
                cwd: gone_str.to_owned(),
                version: None,
                codex: None,
            }));
        }
        assert_eq!(
            reopen_from(&hosted, Vec::new()).panes_without_folder(spawn_verdict),
            counted(1, 0)
        );
    }

    /// AUDIT #7 FINDING 48 — EVERY PANE OF THE TREE A WINDOW RESTORES FROM is
    /// counted: the second pane of a split tab whose folder is gone, and the
    /// legacy mirror's panes when a window has no canonical tabs (split or
    /// not). NEGATIVE CONTROLS: the same layouts with every folder there count
    /// nothing, and a window that HAS canonical tabs is counted from them
    /// alone, never from its legacy mirror too.
    #[test]
    fn every_pane_of_the_restored_tree_is_counted_split_and_legacy() {
        use restore::{PaneLayout, RestoredSplitTree, SplitKind};
        let reopened = |manifest: RestoreManifest| super::Reopened {
            manifest,
            class: DeathClass::Killed,
            sources: vec![JournalId { pid: 9, nanos: 9 }],
            writers: vec![9],
            programs: None,
            second_chance: false,
        };
        let gone = |dir: &str| dir.starts_with("/gone");
        let none = |_: &str| false;

        // A split tab (/here | /gone/a) beside a one-pane tab (/here/too).
        let mut split = layout(&[("/here", "zsh"), ("/gone/a", "zsh"), ("/here/too", "zsh")]);
        let tabs = &mut split.windows[0].restored_tabs;
        let second = tabs.remove(1).root;
        let first = tabs[0].root.clone();
        tabs[0].root = RestoredSplitTree::Split {
            axis: SplitKind::Vertical,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        };
        let split = reopened(split);
        assert_eq!(split.counts(), (1, 2));
        let counted = |gone| PanesWithoutFolder { gone, unasked: 0 };
        assert_eq!(
            split.panes_without_folder(gone),
            counted(1),
            "the split's second pane"
        );
        assert_eq!(split.panes_without_folder(none), counted(0));

        let leg = |cwd: &str| PaneLayout::Leaf {
            cwd: Some(cwd.to_string()),
            title: "zsh".to_string(),
            focused: false,
            local_id: None,
        };
        let mirror = vec![
            PaneLayout::Split {
                dir: SplitKind::Horizontal,
                ratio: 0.5,
                first: Box::new(leg("/here")),
                second: Box::new(leg("/gone/b")),
            },
            leg("/gone/c"),
        ];
        let mut legacy = layout(&[]);
        legacy.windows[0].tabs = mirror.clone();
        let legacy = reopened(legacy);
        assert_eq!(
            legacy.panes_without_folder(gone),
            counted(2),
            "the legacy mirror"
        );
        assert_eq!(legacy.panes_without_folder(none), counted(0));

        let mut both = layout(&[("/here", "zsh"), ("/here/too", "zsh")]);
        both.windows[0].tabs = mirror;
        assert_eq!(
            reopened(both).panes_without_folder(gone),
            counted(0),
            "canonical tabs win; the mirror is not counted beside them"
        );
    }

    /// P6a, RULING 293 — TIER-1 through the real journal: a killed run whose
    /// tab 2 hosted Claude (its leaf carries the relaunch record and the
    /// capture marked its program as that agent) is written, the writer
    /// killed, the journal claimed and read back, and the reopened row says
    /// what comes back. With the relaunch on and nothing else running it is a
    /// RECORD: only scrollback was lost. Beside a lost vim the warning names
    /// vim and its sentence says aterm starts Claude again. NEGATIVE CONTROLS through the
    /// same files: with the relaunch off (the harness off, or headless) the
    /// agent is a program lost as before; an agent that never registered a
    /// conversation is lost too (the relaunch refuses it); and an entry
    /// marked as an agent in a tab whose leaf carries none counts as lost.
    #[test]
    fn an_agent_the_relaunch_brings_back_is_not_lost_through_the_journal() {
        use crate::message_reporters::{JOURNAL_ONLY_SCROLLBACK, journal_reopened_message};
        use aterm_messages::{Hold, Severity};
        let claude = |session: Option<&str>| restore::AgentRestore {
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: session.map(str::to_string),
            cwd: "/work/b".into(),
            version: Some("2.1.283".into()),
            codex: None,
        };
        let agent_in = |tab: u32| super::LeafProgram {
            window: 0,
            tab,
            program: "claude".to_string(),
            agent: true,
        };
        let reopen = |agent: Option<restore::AgentRestore>, programs: Vec<super::LeafProgram>| {
            let mut manifest = layout(&[("/work/a", "zsh"), ("/work/b", "claude")]);
            manifest.fill_agents(&|id| (id == 1).then(|| agent.clone()).flatten());
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            die(run_said(dir.path(), logs.path(), &manifest, Some(programs)));
            claim_here(dir.path(), logs.path())
                .reopened
                .expect("reopened")
        };
        const CONVERSATION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";

        let only = reopen(Some(claude(Some(CONVERSATION))), vec![agent_in(1)]);
        assert_eq!(
            only.programs,
            Some(vec![agent_in(1)]),
            "`agent = true` round-trips"
        );
        assert_eq!(only.resumed(true), vec![(0, 1)]);
        let row = journal_reopened_message(&only, None, None, None, true, PanesWithoutFolder::NONE);
        assert_eq!((row.severity, row.hold), (Severity::Info, Hold::LogOnly));
        assert_eq!(row.title, "Tabs restored after aterm stopped");
        assert_eq!(
            row.detail[0],
            format!(
                "aterm starts Claude again on its conversation in tab 2; {JOURNAL_ONLY_SCROLLBACK}"
            )
        );
        assert!(
            row.detail.iter().all(|l| !l.contains("did not survive,")),
            "{:?}",
            row.detail
        );

        let beside = reopen(
            Some(claude(Some(CONVERSATION))),
            vec![vim_in(0, 0), agent_in(1)],
        );
        let row =
            journal_reopened_message(&beside, None, None, None, true, PanesWithoutFolder::NONE);
        assert_eq!(row.severity, Severity::Warn);
        assert_eq!(row.title, "Tabs restored, vim lost");
        // One sentence (ruling 314): the agent coming back rides detail[0],
        // never the technical lines under it.
        assert_eq!(
            row.detail[0],
            "vim was running in 1 of 2 tabs and did not survive, nor did the scrollback; \
             aterm starts Claude again on its conversation in tab 2",
            "{:?}",
            row.detail
        );
        assert!(
            row.detail[1..].iter().all(|l| !l.contains("starts Claude")),
            "{:?}",
            row.detail
        );

        // NEGATIVE CONTROLS.
        let off =
            journal_reopened_message(&only, None, None, None, false, PanesWithoutFolder::NONE);
        assert_eq!(off.severity, Severity::Warn);
        assert_eq!(off.title, "Tabs restored, claude lost");
        assert!(
            off.detail.iter().all(|l| !l.contains("resumes")),
            "{:?}",
            off.detail
        );
        let unregistered = reopen(Some(claude(None)), vec![agent_in(1)]);
        assert!(unregistered.resumed(true).is_empty());
        let row = journal_reopened_message(
            &unregistered,
            None,
            None,
            None,
            true,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(row.title, "Tabs restored, claude lost");
        let planted = reopen(None, vec![agent_in(1)]);
        let row =
            journal_reopened_message(&planted, None, None, None, true, PanesWithoutFolder::NONE);
        assert_eq!(row.title, "Tabs restored, claude lost");
    }

    /// Two dead writers' journals merge: each one's leaves keep their own
    /// window (rebased into the merged layout), and ONE journal that did not
    /// say makes the whole relaunch unknown — never quiet.
    #[test]
    fn merged_journals_rebase_their_programs_and_one_silent_writer_is_unknown() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        die(run_said(
            dir.path(),
            logs.path(),
            &layout(&[("/a", "vim")]),
            Some(vec![vim_in(0, 0)]),
        ));
        die(run_said(
            dir.path(),
            logs.path(),
            &layout(&[("/b", "vim")]),
            Some(vec![vim_in(0, 0)]),
        ));
        let merged = claim_here(dir.path(), logs.path())
            .reopened
            .expect("reopened");
        assert_eq!(merged.programs, Some(vec![vim_in(0, 0), vim_in(1, 0)]));
        assert_eq!(merged.lost().map(|lost| lost.tabs), Some(2));

        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        die(run_said(
            dir.path(),
            logs.path(),
            &layout(&[("/a", "zsh")]),
            Some(Vec::new()),
        ));
        die(run_said(
            dir.path(),
            logs.path(),
            &layout(&[("/b", "zsh")]),
            None,
        ));
        let merged = claim_here(dir.path(), logs.path())
            .reopened
            .expect("reopened");
        assert_eq!(merged.programs, None);
        assert_eq!(merged.lost(), None);
    }

    /// A journal is operator-writable: a name that is not a program token is
    /// dropped (the leaf still ran something), never shown.
    #[test]
    fn a_planted_program_name_reads_as_unnamed() {
        let mut header = JournalHeader::now(1, false);
        header.programs = Some(vec![super::LeafProgram {
            window: 0,
            tab: 0,
            program: "s-1234 `rm -rf`".to_string(),
            agent: false,
        }]);
        let text = encode(&layout(&[("/a", "zsh")]), &header).unwrap();
        let (read, _) = decode(&text).unwrap();
        assert_eq!(read.programs.unwrap()[0].program, "");
    }

    /// The owner dies with no exit path (a SIGKILL): its lock goes with it.
    /// Waits for the release itself — the lock's open file description can
    /// outlive this `drop` for as long as a child another test thread forked in
    /// the meantime has not reached its `exec` (close-on-exec closes it there).
    pub(crate) fn die(owner: JournalOwner) {
        let lock = owner.path().with_file_name(owner.id().lock_name());
        drop(owner);
        await_released(&lock);
    }

    /// Wait until nobody holds `lock` (bounded: a failure, never a skip).
    pub(crate) fn await_released(lock: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while markers::probe(lock) == markers::Owner::Live {
            assert!(
                Instant::now() < deadline,
                "{} still held 10 s after its owner went",
                lock.display()
            );
            std::thread::yield_now();
        }
    }

    fn claim_here(dir: &Path, logs: &Path) -> BootClaim {
        claim_at_boot(dir, &evidence(logs), std::process::id(), false)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn names_round_trip_and_nothing_else_parses() {
        let id = JournalId {
            pid: 4242,
            nanos: 1_790_309_523_499_676_000,
        };
        assert_eq!(id.file_name(), "journal-4242-1790309523499676000.toml");
        assert_eq!(JournalId::parse(&id.file_name()), Some(id));
        assert_eq!(JournalId::parse_lock(&id.lock_name()), Some(id));
        for stranger in [
            "journal-4242-7.lock",
            "journal-x-7.toml",
            "journal-4242-.toml",
            ".journal-4242-7.toml",
            "session.toml",
            "journal-4242-7.toml.seen",
        ] {
            assert_eq!(JournalId::parse(stranger), None, "{stranger}");
        }
    }

    /// A journal is a `session.toml` too: the layout parses through the quit
    /// layout's own reader, and the header rides beside it.
    #[test]
    fn a_journal_is_a_session_toml_with_a_header() {
        let manifest = layout(&[("/work/a", "vim"), ("/work/b", "zsh")]);
        let header = JournalHeader::now(7, true);
        let text = encode(&manifest, &header).unwrap();
        assert_eq!(RestoreManifest::from_toml(&text), Some(manifest.clone()));
        assert_eq!(decode(&text).unwrap(), (header, manifest));
        assert!(
            decode(&RestoreManifest::new(Vec::new()).to_toml().unwrap()).is_err(),
            "a plain session.toml has no header and is no journal"
        );
    }

    /// THE CASE THIS EXISTS FOR: a run that was killed (its lock gone with it,
    /// its crash marker left empty) is reopened — its tabs, in their folders —
    /// and taken once: the next launch finds nothing.
    #[test]
    fn a_killed_runs_journal_reopens_once() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let manifest = layout(&[("/work/a", "vim"), ("/work/b", "zsh")]);
        let owner = run(dir.path(), logs.path(), &manifest, false);
        let id = owner.id();
        die(owner); // SIGKILL: no retire, the lock released by the kernel
        let claim = claim_here(dir.path(), logs.path());
        let reopened = claim.reopened.expect("reopened");
        assert_eq!(reopened.class, DeathClass::Killed);
        assert_eq!(reopened.sources, vec![id]);
        assert_eq!(cwds(&reopened.manifest), vec!["/work/a", "/work/b"]);
        assert_eq!(reopened.counts(), (1, 2));
        assert!(claim.notes.is_empty());
        assert!(
            names(dir.path()).is_empty(),
            "the journal, its lock and the claim are gone: {:?}",
            names(dir.path())
        );
        assert_eq!(
            claim_at_boot(dir.path(), &evidence(logs.path()), 1, false),
            BootClaim::default()
        );
    }

    /// P6a (2026-09-27): a killed run's journal brings back, on the leaf whose
    /// shell hosted it, the agent's relaunch record — what the next launch
    /// relaunches in that reopened tab. NEGATIVE CONTROL: the leaf whose shell
    /// hosted none comes back with none.
    #[test]
    fn a_killed_runs_journal_brings_back_its_agent() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let claude = restore::AgentRestore {
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/opt/claude/bin/claude".into(),
            argv: vec![
                "/opt/claude/bin/claude".into(),
                "--dangerously-skip-permissions".into(),
            ],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
            cwd: "/work/b".into(),
            version: Some("2.1.283".into()),
            codex: None,
        };
        let mut manifest = layout(&[("/work/a", "zsh"), ("/work/b", "claude")]);
        manifest.fill_agents(&|id| (id == 1).then(|| claude.clone()));
        die(run(dir.path(), logs.path(), &manifest, false));
        let reopened = claim_here(dir.path(), logs.path())
            .reopened
            .expect("reopened");
        let agents: Vec<Option<restore::AgentRestore>> = reopened.manifest.windows[0]
            .restored_tabs
            .iter()
            .map(|tab| match &tab.root {
                restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Terminal(leaf),
                } => leaf.agent.as_deref().cloned(),
                _ => None,
            })
            .collect();
        assert_eq!(agents, vec![None, Some(claude)]);
    }

    /// A clean quit removes its journal (and its lock): nothing to reopen. A
    /// run whose crash marker went with a clean `_exit` — the update parent's
    /// after Commit — leaves its journal behind, and it is taken and set aside,
    /// never reopened.
    #[test]
    fn a_clean_end_is_never_reopened() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let owner = run(dir.path(), logs.path(), &layout(&[("/q", "zsh")]), false);
        owner.retire().unwrap();
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
        assert_eq!(claim_here(dir.path(), logs.path()), BootClaim::default());

        let owner = run(dir.path(), logs.path(), &layout(&[("/h", "zsh")]), false);
        let id = owner.id();
        // The hand-off: `clean_exit_now` removes the run's marker; the journal
        // stays.
        for entry in std::fs::read_dir(logs.path()).unwrap().flatten() {
            std::fs::remove_file(entry.path()).unwrap();
        }
        die(owner);
        let claim = claim_here(dir.path(), logs.path());
        assert_eq!(claim.reopened, None);
        assert_eq!(claim.taken, vec![id]);
        assert_eq!(claim.set_aside.len(), 1, "{:?}", claim.set_aside);
        assert!(names(dir.path()).is_empty());
    }

    /// A RUNNING window's journal is never taken, by any number of launches;
    /// of two launches racing for a dead one, one takes it and the other finds
    /// nothing. NEGATIVE CONTROL: the same journal with its owner dead is
    /// taken — the lock, not the file's existence, is what kept it.
    #[test]
    fn a_second_instance_never_steals_a_journal() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let live = run(dir.path(), logs.path(), &layout(&[("/live", "zsh")]), false);
        for _ in 0..2 {
            let claim = claim_here(dir.path(), logs.path());
            assert!(claim.taken.is_empty(), "{claim:?}");
        }
        assert!(live.path().exists());
        die(live);
        let first = claim_here(dir.path(), logs.path());
        let second = claim_here(dir.path(), logs.path());
        assert_eq!(first.taken.len(), 1, "control: a dead owner's is taken");
        assert!(first.reopened.is_some());
        assert!(second.taken.is_empty() && second.reopened.is_none());

        // Two claimers in two threads against one dead journal: exactly one
        // takes it.
        die(run(
            dir.path(),
            logs.path(),
            &layout(&[("/race", "zsh")]),
            false,
        ));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let racers: Vec<_> = (0..2)
            .map(|_| {
                let (dir, logs) = (dir.path().to_path_buf(), logs.path().to_path_buf());
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    claim_here(&dir, &logs).taken.len()
                })
            })
            .collect();
        let taken: usize = racers.into_iter().map(|r| r.join().unwrap()).sum();
        assert_eq!(taken, 1, "one image, one taker");
    }

    /// A crash, then an UPDATE: the journal an older build wrote — another
    /// build string, the legacy layout schema — reopens on this one.
    #[test]
    fn another_builds_journal_reopens() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let id = JournalId::now(reaped_pid());
        marker(logs.path(), id);
        die(JournalOwner::create(dir.path(), id).unwrap());
        let older = format!(
            "schema = 1\n\n[[windows]]\nrows = 30\ncols = 100\nactive_tab = 0\n\n\
             [[windows.tabs]]\nkind = \"leaf\"\ncwd = \"/from/0.80\"\ntitle = \"zsh\"\n\
             focused = true\n\n\
             [journal]\npid = {}\nbuild = \"0.80.0 (1234)\"\nwritten_unix_ms = 1\n",
            id.pid
        );
        std::fs::write(dir.path().join(id.file_name()), older).unwrap();
        let claim = claim_here(dir.path(), logs.path());
        let reopened = claim.reopened.expect("an older build's journal reopens");
        assert_eq!(reopened.manifest.windows[0].rows, 30);
        assert_eq!(
            reopened.manifest.first_leaf_cwd().as_deref(),
            Some("/from/0.80"),
            "{:?}",
            reopened.manifest
        );
    }

    /// An unreadable journal of an unclean end is set aside and SAID; a
    /// journal planted as a link is never followed. NEGATIVE CONTROL: the same
    /// kind of run with a whole image reopens.
    #[test]
    fn an_unreadable_journal_is_said_and_never_followed() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let owner = run(dir.path(), logs.path(), &layout(&[("/torn", "zsh")]), false);
        let (id, path) = (owner.id(), owner.path());
        die(owner);
        std::fs::write(&path, b"schema = 2\n[[windows]\n").unwrap();
        let claim = claim_here(dir.path(), logs.path());
        assert_eq!(claim.reopened, None);
        assert!(
            matches!(
                &claim.notes[..],
                [Note::Unreadable { id: noted, class: DeathClass::Killed, .. }] if *noted == id
            ),
            "{:?}",
            claim.notes
        );

        let secret = scratch("cj-secret");
        let target = secret.path().join("elsewhere.toml");
        std::fs::write(
            &target,
            encode(
                &layout(&[("/secret", "zsh")]),
                &JournalHeader::now(1, false),
            )
            .unwrap(),
        )
        .unwrap();
        let owner = run(dir.path(), logs.path(), &layout(&[("/x", "zsh")]), false);
        let path = owner.path();
        die(owner);
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let claim = claim_here(dir.path(), logs.path());
        assert_eq!(claim.reopened, None, "a link is never followed");
        assert!(matches!(&claim.notes[..], [Note::Unreadable { .. }]));
        assert!(target.exists(), "the link's target is untouched");

        die(run(
            dir.path(),
            logs.path(),
            &layout(&[("/whole", "zsh")]),
            false,
        ));
        assert!(
            claim_here(dir.path(), logs.path()).reopened.is_some(),
            "control"
        );
    }

    /// THE BRAKE (ruling 285): a journal written by a launch that had
    /// reopened a crash journal and CRASHED within its 90 s is skipped and
    /// said, naming what the skipped layout held; one that was STOPPED within
    /// them (a kill, day five's D18) comes back once — marked a second chance
    /// — and a second stop in a row inside the next 90 s is skipped too.
    /// NEGATIVE CONTROL: the same launch's journal after it settled reopens.
    #[test]
    fn a_relapse_inside_probation_is_skipped_and_said() {
        use super::LeafProgram;
        let python = || {
            Some(vec![LeafProgram {
                window: 0,
                tab: 1,
                program: "Python".to_string(),
                agent: false,
            }])
        };
        // Written on probation, `second_chance` as given, ended by `banner`
        // in its crash marker (empty: killed).
        let relapse = |second_chance: bool, banner: &[u8]| {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            let id = JournalId::now(reaped_pid());
            std::fs::write(marker(logs.path(), id), banner).unwrap();
            let owner = JournalOwner::create(dir.path(), id).expect("armed");
            let mut header = JournalHeader::now(id.pid, true).with_second_chance(second_chance);
            header.programs = python();
            owner
                .publish(&layout(&[("/a", "zsh"), ("/b", "python3")]), &header)
                .expect("published");
            die(owner);
            let claim = claim_here(dir.path(), logs.path());
            (claim, dir, logs)
        };

        // A first stop inside the probation: reopened, a second chance.
        let (claim, _dir, _logs) = relapse(false, b"");
        let reopened = claim
            .reopened
            .expect("a kill is no evidence the layout stops aterm");
        assert!(reopened.second_chance, "{reopened:?}");
        assert!(claim.notes.is_empty(), "{:?}", claim.notes);

        // The second stop in a row: skipped and said, naming what it held.
        let (claim, _dir, _logs) = relapse(true, b"");
        assert_eq!(claim.reopened, None);
        match &claim.notes[..] {
            [
                Note::Relapsed {
                    class,
                    windows,
                    tabs,
                    lost: Some(lost),
                    ..
                },
            ] => {
                assert_eq!(*class, DeathClass::Killed);
                assert_eq!((*windows, *tabs), (1, 2));
                assert_eq!(lost.named, ["Python"]);
            }
            other => panic!("{other:?}"),
        }

        // A crash inside the probation, first time or not: skipped.
        let (claim, _dir, _logs) = relapse(false, b"aterm: fatal signal 11");
        assert_eq!(claim.reopened, None);
        assert!(
            matches!(
                &claim.notes[..],
                [Note::Relapsed {
                    class: DeathClass::Signal,
                    ..
                }]
            ),
            "{:?}",
            claim.notes
        );

        // Settled: reopened, no second chance.
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let owner = run(
            dir.path(),
            logs.path(),
            &layout(&[("/settled", "zsh")]),
            true,
        );
        owner
            .publish(
                &layout(&[("/settled", "zsh")]),
                &JournalHeader::now(owner.id().pid, false),
            )
            .unwrap();
        die(owner);
        let settled = claim_here(dir.path(), logs.path())
            .reopened
            .expect("control");
        assert!(!settled.second_chance);
    }

    /// A header written before the second-chance mark reads as `false`, and
    /// the mark rides only a probation image (ruling 285, additive).
    #[test]
    fn the_second_chance_mark_is_additive() {
        let text = encode(
            &layout(&[("/a", "zsh")]),
            &JournalHeader::now(7, true).with_second_chance(true),
        )
        .unwrap();
        assert!(text.contains("second_chance = true"), "{text}");
        let (header, _) = decode(&text).unwrap();
        assert!(header.probation && header.second_chance);
        let old = text.replace("second_chance = true\n", "");
        assert!(!old.contains("second_chance"));
        let (header, _) = decode(&old).unwrap();
        assert!(header.probation && !header.second_chance);
        let plain = encode(&layout(&[("/a", "zsh")]), &JournalHeader::now(7, false)).unwrap();
        assert!(!plain.contains("second_chance"), "{plain}");
    }

    /// The last quit's layout wins: every dead journal is taken and set aside.
    #[test]
    fn a_quit_layout_sets_every_journal_aside() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        die(run(
            dir.path(),
            logs.path(),
            &layout(&[("/a", "zsh")]),
            false,
        ));
        let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, true);
        assert_eq!(claim.reopened, None);
        assert_eq!(claim.taken.len(), 1);
        assert!(names(dir.path()).is_empty());
    }

    /// Two windows that died together (a WindowServer death takes every GUI
    /// process) both come back: the newest writer's windows first.
    #[test]
    fn every_crashed_runs_windows_come_back_newest_first() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let older = run(
            dir.path(),
            logs.path(),
            &layout(&[("/older", "zsh")]),
            false,
        );
        std::thread::sleep(Duration::from_millis(2));
        let newer = run(
            dir.path(),
            logs.path(),
            &layout(&[("/newer", "zsh")]),
            false,
        );
        let (older_id, newer_id) = (older.id(), newer.id());
        die(older);
        die(newer);
        let reopened = claim_here(dir.path(), logs.path()).reopened.unwrap();
        assert_eq!(reopened.sources, vec![newer_id, older_id]);
        assert_eq!(cwds(&reopened.manifest), vec!["/newer", "/older"]);
        // Each window names its own writer: a shell the PTY keeper kept is
        // placed only in its own window's panes (`keeper_link::place_by_owner`).
        assert_ne!(newer_id.pid, older_id.pid);
        assert_eq!(reopened.writers, vec![newer_id.pid, older_id.pid]);
    }

    /// The run's own evidence, and only its: a panic report written after the
    /// journal was armed, a signal banner in its marker, an empty marker left or
    /// swept at this launch's install. A marker of the same pid armed AFTER the
    /// journal (a later process that reused the pid) and a panic report older
    /// than the journal are not this run's.
    #[test]
    fn the_death_class_is_read_from_the_runs_own_evidence() {
        let logs = scratch("cj-logs");
        let id = JournalId::now(reaped_pid());
        let none = Evidence {
            log_dir: Some(logs.path()),
            swept: &[],
        };
        assert_eq!(
            classify_death(&none, id),
            None,
            "a clean exit leaves nothing"
        );

        let later = logs
            .path()
            .join(markers::file_name(id.pid, id.nanos + 1, MarkerOwner::App));
        std::fs::write(&later, b"").unwrap();
        assert_eq!(classify_death(&none, id), None, "a later process's marker");
        std::fs::remove_file(&later).unwrap();

        let own = marker(logs.path(), id);
        assert_eq!(classify_death(&none, id), Some(DeathClass::Killed));
        std::fs::write(&own, b"aterm: fatal signal 11").unwrap();
        assert_eq!(classify_death(&none, id), Some(DeathClass::Signal));
        std::fs::remove_file(&own).unwrap();

        let report = logs.path().join(format!("crash-{}.log", id.pid));
        std::fs::write(&report, b"aterm-gui crashed").unwrap();
        assert_eq!(classify_death(&none, id), Some(DeathClass::Panic));
        let before = JournalId {
            pid: id.pid,
            nanos: super::unix_nanos(std::time::SystemTime::now()) + 60_000_000_000,
        };
        assert_eq!(
            classify_death(&none, before),
            None,
            "a report older than the journal is not its run's"
        );
        std::fs::remove_file(&report).unwrap();

        let swept = [markers::MarkerName {
            pid: id.pid,
            nanos: id.nanos - 5,
            owner: MarkerOwner::Other,
        }];
        let with_sweep = Evidence {
            log_dir: Some(logs.path()),
            swept: &swept,
        };
        assert_eq!(classify_death(&with_sweep, id), Some(DeathClass::Killed));
    }

    /// A CRASH WHOSE EVIDENCE ANOTHER LAUNCH READ FIRST. Two instances; one is
    /// killed (or takes a fatal signal), and before anyone relaunches it the
    /// other one's seamless update boots a successor. That successor does not
    /// claim (it adopts), but its boot still reads every run's crash and kill
    /// evidence (`take_kill_evidence`/`take_crash_evidence`), renaming the dead
    /// run's marker to `.seen`. The next launch that claims must still reopen
    /// the dead run's layout: its journal is named for its run's start
    /// (`JournalId::of_this_run`), so the claim finds exactly that marker under
    /// its consumed name. Before this, the claim read only live names, set the
    /// journal aside as a clean end and deleted it. NEGATIVE CONTROL: an
    /// EARLIER run of the same pid whose consumed marker is still kept is not
    /// this run's evidence — a clean hand-off's journal beside it stays aside.
    #[test]
    fn a_crash_whose_evidence_another_launch_read_first_still_reopens() {
        for (banner, class) in [
            (&b""[..], DeathClass::Killed),
            (&b"aterm: fatal signal 11"[..], DeathClass::Signal),
        ] {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            // The run's marker is named for its start, and so is its journal.
            let start = super::unix_nanos(std::time::SystemTime::now());
            let id = JournalId::named(reaped_pid(), Some(start));
            assert_eq!(id.nanos, start);
            let own = logs
                .path()
                .join(markers::file_name(id.pid, start, MarkerOwner::App));
            std::fs::write(&own, banner).unwrap();
            let owner = JournalOwner::create(dir.path(), id).expect("armed");
            owner
                .publish(
                    &layout(&[("/survived", "zsh")]),
                    &JournalHeader::now(id.pid, false),
                )
                .expect("published");
            die(owner);
            // The update's successor boots: it reads the evidence, claims nothing.
            let read = if banner.is_empty() {
                crate::logging::take_kill_evidence_in(logs.path(), std::process::id()).is_some()
            } else {
                crate::logging::take_crash_evidence_in(logs.path()).is_some()
            };
            assert!(
                read && !own.exists(),
                "control: the evidence was read first"
            );
            let claim = claim_here(dir.path(), logs.path());
            let reopened = claim
                .reopened
                .unwrap_or_else(|| panic!("{class:?}: set aside as {:?}", claim.set_aside));
            assert_eq!(reopened.class, class);
            assert_eq!(cwds(&reopened.manifest), vec!["/survived"]);
        }

        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let id = JournalId::now(reaped_pid());
        let earlier = logs.path().join(format!(
            "{}.seen",
            markers::file_name(id.pid, id.nanos - 7, MarkerOwner::App)
        ));
        std::fs::write(&earlier, b"").unwrap();
        let owner = JournalOwner::create(dir.path(), id).expect("armed");
        owner
            .publish(
                &layout(&[("/handed", "zsh")]),
                &JournalHeader::now(id.pid, false),
            )
            .expect("published");
        die(owner); // the hand-off's `_exit`: its own marker went with it
        let claim = claim_here(dir.path(), logs.path());
        assert_eq!(claim.reopened, None, "an earlier run's consumed marker");
        assert_eq!(claim.taken, vec![id]);
    }

    /// Leftovers of the dead are cleared; a live writer's are not.
    #[test]
    fn a_claim_clears_the_dead_writers_leftovers() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let dead_id = JournalId::now(reaped_pid());
        die(JournalOwner::create(dir.path(), dead_id).unwrap());
        let temp = dir.path().join(format!("{}9.tmp", dead_id.temp_prefix()));
        std::fs::write(&temp, b"half").unwrap();
        let claim_file = dir
            .path()
            .join(format!(".journal-1-1.toml.claimed-{}-3.tmp", reaped_pid()));
        std::fs::write(&claim_file, b"x").unwrap();
        let live = JournalOwner::create(dir.path(), JournalId::now(std::process::id())).unwrap();
        let live_temp = dir.path().join(format!("{}9.tmp", live.id().temp_prefix()));
        std::fs::write(&live_temp, b"half").unwrap();

        let _ = claim_here(dir.path(), logs.path());
        assert!(!temp.exists() && !claim_file.exists());
        assert!(!dir.path().join(dead_id.lock_name()).exists());
        assert!(live_temp.exists(), "a live writer's temporary stays");
        assert!(dir.path().join(live.id().lock_name()).exists());
    }

    /// NO LOSS (`CrashJournalClaim`'s `NoLoss`, on the real kernel): a reader
    /// racing a writer that rewrites the image hundreds of times finds a whole
    /// image under the name at every read — the rename replaces the old image in
    /// one step, so a death at any instant of a rewrite leaves one to reopen.
    #[test]
    fn a_rewrite_never_leaves_the_name_without_a_whole_image() {
        let dir = scratch("cj-noloss");
        let owner = JournalOwner::create(dir.path(), JournalId::now(std::process::id())).unwrap();
        owner
            .publish(&layout(&[("/0", "zsh")]), &JournalHeader::now(1, false))
            .unwrap();
        let path = owner.path();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (path, stop) = (path.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut reads = 0usize;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                        panic!("read {reads}: the name is empty ({error})")
                    });
                    decode(&text).unwrap_or_else(|error| panic!("read {reads}: torn ({error})"));
                    reads += 1;
                }
                reads
            })
        };
        for rewrite in 1..=400 {
            let cwd = format!("/{rewrite}");
            owner
                .publish(
                    &layout(&[(cwd.as_str(), "zsh")]),
                    &JournalHeader::now(1, false),
                )
                .unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let reads = reader.join().expect("every read found a whole image");
        assert!(reads > 0, "the reader raced the writer");
        owner.retire().unwrap();
    }

    /// THE BOOT ORDER, read off `main_entry`: the claim reads the crash evidence
    /// BEFORE `take_crash_evidence`/`take_kill_evidence` consume it (they rename
    /// every marker and report to `.seen`, which the claim does not read — an
    /// installed app's kill would then never reopen), and the quit's retire
    /// comes after the quit's own layout write.
    #[test]
    fn main_entry_claims_before_the_evidence_is_consumed_and_retires_after_the_write() {
        let source = include_str!("lib.rs");
        let main = &source[source
            .find("pub fn main_entry(")
            .expect("main_entry exists")..];
        let at = |needle: &str| {
            main.find(needle)
                .unwrap_or_else(|| panic!("main_entry calls {needle}"))
        };
        assert!(at("crash_journal::claim_at_boot(") < at("logging::take_crash_evidence()"));
        assert!(at("crash_journal::claim_at_boot(") < at("logging::take_kill_evidence()"));
        assert!(at("restore::write(&manifest)") < at(".retire(std::time::Duration"));
    }

    /// Images are private and published whole: `0600`, no temporary left.
    #[test]
    fn an_image_is_private_and_whole() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("cj-dir");
        let owner = JournalOwner::create(dir.path(), JournalId::now(std::process::id())).unwrap();
        owner
            .publish(&layout(&[("/p", "zsh")]), &JournalHeader::now(1, false))
            .unwrap();
        let mode = std::fs::metadata(owner.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(names(dir.path()).len(), 2, "{:?}", names(dir.path()));
        owner.retire().unwrap();
    }

    /// THE WRITE RATE: nothing while idle, at most one image per interval,
    /// only when the layout moved, the trailing edge woken for, a busy leaf's
    /// cwd and title kept, and the probation mark settled by a rewrite.
    #[test]
    fn the_lane_writes_only_changes_at_the_bounded_rate() {
        let dir = scratch("cj-lane");
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
        let t0 = Instant::now();
        assert!(lane.capture_due(t0), "the first capture is due");
        assert!(lane.offer(layout(&[("/a", "zsh")]), Vec::new(), t0));
        assert!(!lane.capture_due(t0), "idle: nothing is due");
        assert_eq!(lane.next_wake(t0), None, "and nothing is woken for");

        lane.note_activity();
        let t1 = t0 + Duration::from_millis(500);
        assert!(!lane.capture_due(t1), "inside the interval");
        assert_eq!(
            lane.next_wake(t1),
            Some(t0 + MIN_WRITE_INTERVAL),
            "the trailing edge"
        );
        let t2 = t0 + MIN_WRITE_INTERVAL;
        assert!(lane.capture_due(t2));
        assert!(
            !lane.offer(layout(&[("/a", "zsh")]), Vec::new(), t2),
            "an unchanged capture is no write"
        );
        lane.note_activity();
        let t3 = t2 + MIN_WRITE_INTERVAL;
        let mut busy = layout(&[("/a", "zsh")]);
        if let restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(leaf),
        } = &mut busy.windows[0].restored_tabs[0].root
        {
            leaf.cwd = None;
            leaf.title.clear();
        }
        assert!(
            !lane.offer(busy, Vec::new(), t3),
            "a busy engine's empty leaf keeps its last cwd and title"
        );
        lane.note_activity();
        let t4 = t3 + MIN_WRITE_INTERVAL;
        assert!(lane.offer(layout(&[("/a", "zsh"), ("/b", "zsh")]), Vec::new(), t4));
        assert_eq!(lane.writes, 2);
        lane.retire(Duration::from_secs(5)).unwrap();

        // Probation: marked while it runs, rewritten unmarked at its end.
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
        lane.begin_probation(t0, false);
        assert!(lane.offer(layout(&[("/p", "zsh")]), Vec::new(), t0));
        assert_eq!(lane.next_wake(t0), Some(t0 + PROBATION));
        assert!(!lane.capture_due(t0 + Duration::from_secs(30)));
        assert!(lane.capture_due(t0 + PROBATION), "the settle is due");
        assert!(
            lane.offer(layout(&[("/p", "zsh")]), Vec::new(), t0 + PROBATION),
            "the same layout, rewritten without the mark"
        );
        assert_eq!(lane.next_wake(t0 + PROBATION), None);
        lane.retire(Duration::from_secs(5)).unwrap();
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    }

    /// A TITLE THAT NEVER STOPS (an agent's spinner) is written once per
    /// [`TITLE_WRITE_INTERVAL`], not once per capture; the last title is not
    /// lost — the capture it is owed comes at the interval's end, woken for
    /// even with nothing else moving; and a layout change inside the interval
    /// is still written at the layout's own rate. NEGATIVE CONTROL: the same
    /// spinner's captures are every one a change.
    /// A program starting or ending in a leaf is a change worth an image even
    /// inside the title interval (D16): the journal must not keep saying a tab
    /// sat at its prompt while vim ran in it.
    #[test]
    fn a_program_starting_is_written_like_a_moved_folder() {
        let dir = scratch("cj-lane");
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        let t0 = Instant::now();
        assert!(lane.offer(layout(&[("/a", "zsh")]), Vec::new(), t0));
        let t1 = t0 + MIN_WRITE_INTERVAL;
        assert!(
            lane.offer(layout(&[("/a", "vim")]), vec![vim_in(0, 0)], t1),
            "a program started: written though only the title moved"
        );
        let t2 = t1 + MIN_WRITE_INTERVAL;
        assert!(
            !lane.offer(layout(&[("/a", "vim")]), vec![vim_in(0, 0)], t2),
            "nothing moved"
        );
        let t3 = t2 + MIN_WRITE_INTERVAL;
        assert!(
            lane.offer(layout(&[("/a", "vim")]), Vec::new(), t3),
            "it ended: written"
        );
        lane.retire(Duration::from_secs(5)).expect("retired");
    }

    #[test]
    fn a_title_that_animates_is_written_at_the_title_rate() {
        let dir = scratch("cj-title");
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
        let t0 = Instant::now();
        assert!(lane.offer(layout(&[("/a", "spin 0")]), Vec::new(), t0));
        let mut t = t0;
        let mut written = 1;
        for frame in 1..=60 {
            t += MIN_WRITE_INTERVAL;
            lane.note_activity();
            assert!(lane.capture_due(t));
            let title = format!("spin {frame}");
            assert!(
                !same_but_titles(&layout(&[("/a", "spin 0")]), &layout(&[("/b", "spin 0")])),
                "control: a cwd is no title"
            );
            assert!(
                layout(&[("/a", "spin 0")]) != layout(&[("/a", title.as_str())]),
                "control: every frame is a change"
            );
            written += usize::from(lane.offer(layout(&[("/a", title.as_str())]), Vec::new(), t));
        }
        assert_eq!(written, 5, "120 s of spinner: at 0, 30, 60, 90 and 120 s");

        // A new tab inside the title interval: the layout's own rate.
        t += MIN_WRITE_INTERVAL;
        lane.note_activity();
        assert!(lane.offer(layout(&[("/a", "spin 60"), ("/b", "zsh")]), Vec::new(), t));
        let published = t;

        // The spinner's last frame, then nothing: owed, woken for, written.
        t += MIN_WRITE_INTERVAL;
        lane.note_activity();
        assert!(!lane.offer(layout(&[("/a", "done"), ("/b", "zsh")]), Vec::new(), t));
        assert!(
            !lane.capture_due(t + MIN_WRITE_INTERVAL),
            "idle, and not yet due"
        );
        let due = published + TITLE_WRITE_INTERVAL;
        assert_eq!(lane.next_wake(t), Some(due));
        assert!(lane.capture_due(due));
        assert!(lane.offer(layout(&[("/a", "done"), ("/b", "zsh")]), Vec::new(), due));
        assert_eq!(lane.next_wake(due), None);
        lane.retire(Duration::from_secs(5)).unwrap();
    }

    /// The writer thread publishes what the lane hands it, as this process,
    /// holding its lock, and the quit's retire removes it all.
    #[test]
    fn the_lane_publishes_on_its_thread_and_retires() {
        let dir = scratch("cj-thread");
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
        assert!(lane.offer(layout(&[("/t", "zsh")]), Vec::new(), Instant::now()));
        let deadline = Instant::now() + Duration::from_secs(10);
        let journal = loop {
            let found = names(dir.path())
                .into_iter()
                .find(|name| JournalId::parse(name).is_some());
            if let Some(found) = found {
                break dir.path().join(found);
            }
            assert!(Instant::now() < deadline, "the writer never published");
            std::thread::yield_now();
        };
        let (header, manifest) = decode(&std::fs::read_to_string(&journal).unwrap()).unwrap();
        assert_eq!(header.pid, std::process::id());
        assert_eq!(cwds(&manifest), vec!["/t"]);
        let id = JournalId::parse(journal.file_name().unwrap().to_str().unwrap()).unwrap();
        assert_eq!(
            markers::probe(&dir.path().join(id.lock_name())),
            markers::Owner::Live,
            "the writer holds its lock"
        );
        lane.retire(Duration::from_secs(5)).unwrap();
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    }
}
