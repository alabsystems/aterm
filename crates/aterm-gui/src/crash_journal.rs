// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Crash-recovery layout journal (`journal-<pid>-<nanos>.toml`) backed by [`RestoreManifest`].
#![cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "the journal's files, lock and claim are the unix lane's (the lock is the crash \
                  marker's `flock`); elsewhere the lane is inert and nothing reads them"
    )
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::restore::RestoreManifest;

/// Minimum interval between journal writes and captures.
pub(crate) const MIN_WRITE_INTERVAL: Duration = Duration::from_secs(2);

/// At most one journal write per this interval when only titles changed.
pub(crate) const TITLE_WRITE_INTERVAL: Duration = Duration::from_secs(30);

/// How long a launch that reopened a crash journal must run before its own journal may be reopened.
pub(crate) const PROBATION: Duration = Duration::from_secs(90);

const MAX_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;

const NAME_PREFIX: &str = "journal-";

/// One run's journal: the writer's pid and its run's start as its crash marker names it.
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

    /// This run's journal, named for the start its crash marker carries.
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

    /// `journal-<pid>-<nanos>.lock`.
    pub(crate) fn lock_name(self) -> String {
        format!("{NAME_PREFIX}{}-{}.lock", self.pid, self.nanos)
    }

    /// Parse a [`Self::file_name`].
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::parse_stem(name.strip_prefix(NAME_PREFIX)?.strip_suffix(".toml")?)
    }

    fn parse_lock(name: &str) -> Option<Self> {
        Self::parse_stem(name.strip_prefix(NAME_PREFIX)?.strip_suffix(".lock")?)
    }

    /// `journal-<pid>-<nanos>.unreadable`.
    pub(crate) fn unreadable_name(self) -> String {
        format!("{NAME_PREFIX}{}-{}.unreadable", self.pid, self.nanos)
    }

    fn parse_unreadable(name: &str) -> Option<Self> {
        Self::parse_stem(
            name.strip_prefix(NAME_PREFIX)?
                .strip_suffix(".unreadable")?,
        )
    }

    /// `<pid>-<nanos>`: the run a quit's `session.toml` names (`restore::write_quit`).
    pub(crate) fn stem(self) -> String {
        format!("{}-{}", self.pid, self.nanos)
    }

    pub(crate) fn parse_stem(stem: &str) -> Option<Self> {
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

    fn temp_prefix(self) -> String {
        format!(".{NAME_PREFIX}{}-{}.w-", self.pid, self.nanos)
    }
}

fn unix_nanos(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

/// The journal's own table.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct JournalHeader {
    #[serde(default)]
    pub(crate) pid: u32,
    /// The writer's version and build number, for the person reading the file.
    #[serde(default)]
    pub(crate) build: String,
    #[serde(default)]
    pub(crate) written_unix_ms: u64,
    /// The writer reopened a crash journal at its boot and had not yet run [`PROBATION`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) probation: bool,
    /// The writer's boot reopened a journal whose writer was on probation and was stopped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) second_chance: bool,
    /// Every terminal leaf whose foreground was a program other than its shell when the image was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) programs: Option<Vec<LeafProgram>>,
    /// The writing build names its run in `session.toml` at quit (`restore::write_quit`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stamps_quit: bool,
    /// The shells this writer was handed (additive; absent = none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) handed: Vec<Handed>,
}

/// One pair a leaf's session had before an update handed it on ([`JournalHeader::handed`]).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Handed {
    #[serde(default)]
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) pid: u32,
    #[serde(default)]
    pub(crate) was: u64,
}

/// How many updates back a leaf names the pairs it was handed as ([`JournalHeader::handed`]).
pub(crate) const MAX_HANDED_GENERATIONS: usize = 8;

/// One terminal leaf that had a program running.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeafProgram {
    #[serde(default)]
    pub(crate) window: u32,
    #[serde(default)]
    pub(crate) tab: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) program: String,
    /// This leaf's restore carries its agent's relaunch record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) agent: bool,
    /// This leaf's session was already captured at an earlier leaf of the same image.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) again: bool,
}

const MAX_PROGRAM_ENTRIES: usize = 4096;

const MAX_PROGRAM_NAME: usize = 32;

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

    /// This header naming the pairs its leaves were handed as ([`Self::handed`]).
    pub(crate) fn with_handed(mut self, handed: Vec<Handed>) -> Self {
        self.handed = handed;
        self
    }

    /// Whose pool ids the image's leaves carry.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) const fn image_writer(&self, id: JournalId) -> u32 {
        if self.pid == 0 { id.pid } else { self.pid }
    }

    /// This process's header, written now.
    pub(crate) fn now(pid: u32, probation: bool) -> Self {
        Self {
            pid,
            build: format!(
                "{} ({})",
                crate::build_info::version(),
                crate::build_info::build_number()
            ),
            written_unix_ms: aterm_time::unix_millis().unwrap_or(0),
            probation,
            second_chance: false,
            programs: None,
            stamps_quit: true,
            handed: Vec::new(),
        }
    }

    /// This header, marked as a second chance's ([`Self::second_chance`]).
    pub(crate) const fn with_second_chance(mut self, second_chance: bool) -> Self {
        self.second_chance = second_chance;
        self
    }
}

#[derive(Serialize, Deserialize)]
struct HeaderDoc {
    journal: JournalHeader,
}

/// One journal image: the layout as `session.toml` holds it.
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

/// Read one journal image back.
pub(crate) fn decode(text: &str) -> Result<(JournalHeader, RestoreManifest), String> {
    let mut header = aterm_toml::from_str::<HeaderDoc>(text)
        .map_err(|error| format!("no readable [journal] table ({error})"))?
        .journal;
    if let Some(programs) = header.programs.as_mut() {
        sanitize_programs(programs);
    }
    header
        .handed
        .truncate(MAX_PROGRAM_ENTRIES * MAX_HANDED_GENERATIONS);
    let manifest = RestoreManifest::from_toml(text)
        .ok_or_else(|| "its layout is not one this build reads".to_string())?;
    Ok((header, manifest))
}

/// How a journal's writer ended, by its run's crash evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeathClass {
    /// Its crash marker was left empty.
    Killed,
    /// Its crash marker carries a fatal signal's banner.
    Signal,
    /// The panic hook filed its crash report.
    Panic,
}

impl DeathClass {
    /// The row's first line.
    pub(crate) const fn sentence(self) -> &'static str {
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
    pub(crate) const fn crashed(self) -> bool {
        !matches!(self, Self::Killed)
    }
}

/// Where a claim looks for how a journal's writer ended.
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
    /// The writer of each of `manifest`'s windows, in order.
    pub(crate) writers: Vec<u32>,
    /// What each writer's leaves were handed as ([`JournalHeader::handed`]), by the writer.
    pub(crate) handed: Vec<(u32, Handed)>,
    /// The leaves that had a program running, their windows counted in this merged layout.
    pub(crate) programs: Option<Vec<LeafProgram>>,
    /// A source journal's writer was on probation and was stopped.
    pub(crate) second_chance: bool,
}

/// What a reopened layout lost.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lost {
    pub(crate) tabs: usize,
    pub(crate) named: Vec<String>,
    /// Programs whose name was not resolved yet, one per session however many tabs showed it.
    pub(crate) unnamed: usize,
}

/// The reopened layout's terminal panes whose saved folder is not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PanesWithoutFolder {
    /// Panes whose restore asks for a folder no shell can start in: each starts in the home folder, and a Messages row names the folder.
    pub(crate) gone: usize,
    /// Panes a program held ([`crate::restore::TerminalLeafRestore::folder_may_be_remote`]).
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

    /// One `(window, tab)` per agent the next launch relaunches on its conversation.
    pub(crate) fn resumed_agents(
        &self,
        relaunching: bool,
    ) -> Vec<((u32, u32), aterm_agent::harness::upgrade::Agent)> {
        self.resuming_leaves(relaunching, true)
    }

    /// [`Self::resumed_agents`]' places alone (tests).
    #[cfg(test)]
    pub(crate) fn resumed(&self, relaunching: bool) -> Vec<(u32, u32)> {
        self.resumed_agents(relaunching)
            .into_iter()
            .map(|(place, _)| place)
            .collect()
    }

    fn resuming_leaves(
        &self,
        relaunching: bool,
        once: bool,
    ) -> Vec<((u32, u32), aterm_agent::harness::upgrade::Agent)> {
        fn walk<'a>(
            node: &'a crate::restore::RestoredSplitTree,
            place: (u32, u32),
            seen: &mut Option<Vec<(u32, &'a str)>>,
            out: &mut Vec<((u32, u32), aterm_agent::harness::upgrade::Agent)>,
        ) {
            match node {
                crate::restore::RestoredSplitTree::Leaf {
                    view: crate::restore::RestoredView::Terminal(leaf),
                } => {
                    // A held tab's agent is the holder's: never relaunched.
                    if let Some(agent) = leaf.resumes_agent().filter(|agent| {
                        seen.as_ref()
                            .is_none_or(|seen| !seen.contains(&agent.ran_as()))
                    }) {
                        if let Some(seen) = seen {
                            seen.push(agent.ran_as());
                        }
                        out.push((
                            place,
                            agent
                                .agent()
                                .unwrap_or(aterm_agent::harness::upgrade::Agent::Claude),
                        ));
                    }
                }
                crate::restore::RestoredSplitTree::Leaf { .. } => {}
                crate::restore::RestoredSplitTree::Split { first, second, .. } => {
                    walk(first, place, seen, out);
                    walk(second, place, seen, out);
                }
            }
        }
        let mut out = Vec::new();
        if !relaunching {
            return out;
        }
        let mut seen = once.then(Vec::new);
        for (window, layout) in self.manifest.windows.iter().enumerate() {
            for (tab, restored) in layout.restored_tabs.iter().enumerate() {
                let place = (
                    u32::try_from(window).unwrap_or(u32::MAX),
                    u32::try_from(tab).unwrap_or(u32::MAX),
                );
                walk(&restored.root, place, &mut seen, &mut out);
            }
        }
        out
    }

    /// This layout as the row tells it where the host relaunches only the agents `relaunched`.
    pub(crate) fn relaunched_by(
        &self,
        relaunched: impl Fn(&crate::restore::AgentRestore) -> bool,
    ) -> Self {
        fn walk(
            node: &mut crate::restore::RestoredSplitTree,
            relaunched: &dyn Fn(&crate::restore::AgentRestore) -> bool,
        ) {
            match node {
                crate::restore::RestoredSplitTree::Leaf {
                    view: crate::restore::RestoredView::Terminal(leaf),
                } => {
                    if leaf
                        .agent
                        .as_deref()
                        .is_some_and(|agent| !relaunched(agent))
                    {
                        leaf.agent = None;
                    }
                }
                crate::restore::RestoredSplitTree::Leaf { .. } => {}
                crate::restore::RestoredSplitTree::Split { first, second, .. } => {
                    walk(first, relaunched);
                    walk(second, relaunched);
                }
            }
        }
        let mut told = self.clone();
        for window in &mut told.manifest.windows {
            for tab in &mut window.restored_tabs {
                walk(&mut tab.root, &relaunched);
            }
        }
        told
    }

    /// The layout's terminal panes whose folder no shell can start in (`not_found`, the spawn's own check [`crate::spawn_folder::folder_fault`]), counted from each window's canonical tabs, else its legacy mirror.
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

    /// What was lost with the programs, or `None` when the journals did not say.
    pub(crate) fn lost(&self) -> Option<Lost> {
        self.lost_given(false)
    }

    /// What was lost with the programs, or `None` when the journals did not say.
    pub(crate) fn lost_given(&self, relaunching: bool) -> Option<Lost> {
        let programs = self.programs.as_ref()?;
        let mut coming_back = self.resuming_leaves(relaunching, false);
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
                    .position(|(place, _)| *place == (entry.window, entry.tab))
            {
                coming_back.remove(at);
                continue;
            }
            if !tabs.contains(&(entry.window, entry.tab)) {
                tabs.push((entry.window, entry.tab));
            }
            if entry.again {
                continue;
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

/// A taken journal worth a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    /// It could not be read (a torn file after a power loss, a newer schema, a planted link).
    Unreadable {
        id: JournalId,
        class: DeathClass,
        path: Option<PathBuf>,
        error: String,
    },
    /// The writer reopened a crash journal and crashed within [`PROBATION`].
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

/// The last quit's `session.toml` a launch reopens before any crash journal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct QuitLayout {
    pub(crate) windows: usize,
    pub(crate) run: Option<JournalId>,
    pub(crate) also: Vec<JournalId>,
    /// Every window's tab count, in order.
    pub(crate) shape: Vec<usize>,
    /// When the quit wrote it, Unix milliseconds (`None`: unknown).
    pub(crate) written_unix_ms: Option<u64>,
}

/// How much later than an unstamped quit layout's date a journal image may be and still be that quit's own.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const QUIT_DATE_SLACK_MS: u64 = 2_000;

impl QuitLayout {
    /// The quit layout `manifest` (`None`: no quit layout), written by `run` at `written_unix_ms`.
    pub(crate) fn of(
        manifest: Option<&RestoreManifest>,
        run: Option<JournalId>,
        written_unix_ms: Option<u64>,
    ) -> Self {
        Self {
            windows: manifest.map_or(0, |manifest| manifest.windows.len()),
            run,
            also: Vec::new(),
            shape: manifest.map(desk_shape).unwrap_or_default(),
            written_unix_ms: manifest.and(written_unix_ms),
        }
    }

    /// This quit layout, holding behind its own quit's desk those of the other runs `also` names.
    pub(crate) fn holding(mut self, also: &[String]) -> Self {
        self.also = also
            .iter()
            .filter_map(|run| JournalId::parse_stem(run))
            .collect();
        self
    }

    /// Whether run `id`'s desk is in this quit layout, its own quit's or one it kept.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn names(&self, id: JournalId) -> bool {
        self.run == Some(id) || self.also.contains(&id)
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    const fn unstamped(&self) -> bool {
        self.windows > 0 && self.run.is_none()
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    fn may_be_its_run(&self, header: &JournalHeader, manifest: &RestoreManifest) -> bool {
        self.unstamped()
            && !header.stamps_quit
            && desk_shape(manifest) == self.shape
            && self.written_unix_ms.is_none_or(|quit| {
                header.written_unix_ms <= quit.saturating_add(QUIT_DATE_SLACK_MS)
            })
    }
}

/// A layout's shape: every window's tab count (terminal, legacy and native), in window order.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn desk_shape(manifest: &RestoreManifest) -> Vec<usize> {
    manifest
        .windows
        .iter()
        .map(|window| window.restored_tabs.len() + window.tabs.len() + window.native_tabs.len())
        .collect()
}

/// The journal directory, beside `session.toml` (so it follows a dev build's `ATERM_STATE_HOME`).
pub(crate) fn journal_dir() -> Option<PathBuf> {
    crate::restore::manifest_path().and_then(|path| path.parent().map(Path::to_path_buf))
}

#[cfg(unix)]
pub(crate) use unix::{JournalOwner, claim_at_boot, classify_death};

#[cfg(not(unix))]
pub(crate) fn claim_at_boot(
    _dir: &Path,
    _evidence: &Evidence<'_>,
    _own_pid: u32,
    _quit: QuitLayout,
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
        Note, QuitLayout, Reopened, decode, encode, unix_nanos,
    };
    use crate::crash_signal::markers::{self, Owner};
    use crate::durable::{Flush, fsync};
    use crate::restore::RestoreManifest;

    /// The writer's half: this process's journal, its lifetime lock held.
    #[derive(Debug)]
    pub(crate) struct JournalOwner {
        dir: PathBuf,
        id: JournalId,
        /// The `flock` on `journal-<pid>-<nanos>.lock`.
        _lock: OwnedFd,
    }

    impl JournalOwner {
        /// Arm this process's journal in `dir`.
        pub(crate) fn create(dir: &Path, id: JournalId) -> Result<Self, String> {
            fs::create_dir_all(dir).map_err(|error| format!("mkdir {}: {error}", dir.display()))?;
            let (fd, _) = markers::create_locked(dir, &id.lock_name()).ok_or_else(|| {
                format!(
                    "could not create and lock {}",
                    dir.join(id.lock_name()).display()
                )
            })?;
            // SAFETY: `create_locked` returns a descriptor it opened for this call and nobody else owns; the `OwnedFd` closes it exactly once.
            let lock = unsafe { OwnedFd::from_raw_fd(fd) };
            Ok(Self {
                dir: dir.to_path_buf(),
                id,
                _lock: lock,
            })
        }

        /// The successor's take-over at Commit.
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "CrashJournalClaim",
                action = "Adopt",
                project = "aterm_gui::crash_journal_conformance::Rig::project"
            )
        )]
        pub(crate) fn adopt(
            dir: &Path,
            id: JournalId,
            predecessor: u32,
            born: Option<u128>,
        ) -> Result<(Self, Option<JournalId>), String> {
            const SLACK_NANOS: u128 = 1_000_000_000;
            let owner = Self::create(dir, id)?;
            let not_before = born.map_or(0, |born| born.saturating_sub(SLACK_NANOS));
            let parent = fs::read_dir(dir)
                .map_err(|error| format!("read {}: {error}", dir.display()))?
                .flatten()
                .filter_map(|entry| JournalId::parse(entry.file_name().to_str()?))
                .filter(|found| {
                    found.pid == predecessor && *found != id && found.nanos >= not_before
                })
                .max();
            let Some(parent) = parent else {
                return Ok((owner, None));
            };
            let from = dir.join(parent.file_name());
            fs::rename(&from, owner.path())
                .map_err(|error| format!("take {}: {error}", from.display()))?;
            Ok((owner, Some(parent)))
        }

        #[cfg(test)]
        pub(crate) fn id(&self) -> JournalId {
            self.id
        }

        /// The journal's path.
        pub(crate) fn path(&self) -> PathBuf {
            self.dir.join(self.id.file_name())
        }

        /// Publish one image: create-new, no-follow `0600` temporary, `fsync`.
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "CrashJournalClaim",
                action = "Write",
                project = "aterm_gui::crash_journal_conformance::Rig::project"
            )
        )]
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
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

        /// Take the image down but keep the lock.
        pub(crate) fn withdraw(&self) -> Result<(), String> {
            remove_if_present(&self.path())
        }

        /// The clean quit: remove the image, then the lock file, then release the lock.
        #[cfg_attr(
            any(test, debug_assertions),
            aterm_spec_anchor::refines(
                machine = "CrashJournalClaim",
                action = "Quit",
                project = "aterm_gui::crash_journal_conformance::Rig::project"
            )
        )]
        pub(crate) fn retire(self) -> Result<(), String> {
            let removed = remove_if_present(&self.path());
            let _ = fs::remove_file(self.dir.join(self.id.lock_name()));
            let _ = sync_dir(&self.dir, None);
            drop(self);
            removed
        }
    }

    impl Drop for JournalOwner {
        fn drop(&mut self) {
            // SAFETY: `flock` on the descriptor this owner holds open until its field drops right after; LOCK_UN never waits.
            #[expect(
                clippy::disallowed_methods,
                reason = "the release: LOCK_UN before the descriptor closes"
            )]
            unsafe {
                libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN)
            };
        }
    }

    /// The take-over's second half, once the readers run.
    pub(crate) fn settle_adoption(dir: &Path, parent: JournalId) -> Option<PathBuf> {
        let _ = sync_dir(dir, None);
        let lock = dir.join(parent.lock_name());
        (!clear_dead_lock(&lock)).then_some(lock)
    }

    /// Remove `lock` if nobody holds it.
    pub(crate) fn clear_dead_lock(lock: &Path) -> bool {
        match markers::probe(lock) {
            Owner::Live => false,
            Owner::Dead => {
                let _ = fs::remove_file(lock);
                true
            }
            Owner::Unknown => true,
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
            aterm_durable::ordered(&file)
                .map_err(|error| format!("sync {}: {error}", temporary.display()))?;
            drop(file);
            fs::rename(&temporary, &target)
                .map_err(|error| format!("publish {}: {error}", target.display()))?;
            sync_dir(dir, Some(Flush::Ordered))
                .map_err(|error| format!("sync {}: {error}", dir.display()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn sync_dir(dir: &Path, flush: Option<Flush>) -> std::io::Result<()> {
        let handle = File::open(dir)?;
        match flush {
            Some(flush) => crate::durable::finish(&handle, flush),
            None => fsync(&handle),
        }
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

    fn writer_dead(dir: &Path, id: JournalId) -> bool {
        let lock = dir.join(id.lock_name());
        match markers::probe(&lock) {
            Owner::Dead => true,
            Owner::Live => false,
            Owner::Unknown => {
                matches!(fs::symlink_metadata(&lock), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
                    && !aterm_uds::process::pid_alive(id.pid)
            }
        }
    }

    /// How `id`'s writer ended, from its run's crash evidence in the log dir.
    pub(crate) fn classify_death(
        evidence: &Evidence<'_>,
        id: JournalId,
        next_run: Option<u128>,
    ) -> Option<DeathClass> {
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
        let at = |nanos: u128| {
            std::time::UNIX_EPOCH
                + std::time::Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
        };
        let armed = at(id.nanos);
        let panic_report = format!("crash-{}.log", id.pid);
        let consumed_report = format!("{panic_report}.seen");
        let legacy_prefix = format!("crash-signal-{}-", id.pid);
        let mut own_marker: Option<(u128, u64)> = None;
        let mut own_consumed: Option<u64> = None;
        let mut next_run = evidence
            .swept
            .iter()
            .filter(|marker| marker.pid == id.pid && marker.nanos > id.nanos)
            .map(|marker| marker.nanos)
            .chain(next_run)
            .min();
        let mut reports: Vec<(std::time::SystemTime, bool, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.file_type().is_file() {
                continue;
            }
            if name == panic_report || name == consumed_report {
                if meta.len() > 0
                    && let Ok(modified) = meta.modified()
                {
                    reports.push((modified, name == consumed_report, entry.path()));
                }
            } else if let Some(marker) = markers::parse(name) {
                if marker.pid == id.pid && marker.nanos > id.nanos {
                    next_run = Some(next_run.map_or(marker.nanos, |n| n.min(marker.nanos)));
                }
                if marker.pid == id.pid
                    && marker.nanos <= id.nanos
                    && own_marker.is_none_or(|(nanos, _)| marker.nanos > nanos)
                {
                    own_marker = Some((marker.nanos, meta.len()));
                }
            } else if let Some(consumed) = name.strip_suffix(".seen").and_then(markers::parse) {
                if consumed.pid == id.pid && consumed.nanos > id.nanos {
                    next_run = Some(next_run.map_or(consumed.nanos, |n| n.min(consumed.nanos)));
                }
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
        let its_run = |modified: std::time::SystemTime, run: Option<u128>| match run {
            Some(run) => run == id.nanos,
            None => modified >= armed && next_run.is_none_or(|n| modified < at(n)),
        };
        if reports.iter().any(|(modified, consumed, path)| {
            let head = crate::logging::panic_report_head(path);
            its_run(*modified, head.run) && (!consumed || head.main_thread)
        }) {
            class = class.max(Some(DeathClass::Panic));
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
        if meta.nlink() != 1 {
            return Err("more than one name for the file".to_string());
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

    /// Read the image at `path` (a journal this run owns).
    pub(crate) fn read_image(path: &Path) -> Result<(JournalHeader, RestoreManifest), String> {
        read_taken(path).and_then(|text| decode(&text))
    }

    struct Candidate {
        id: JournalId,
        class: DeathClass,
        header: JournalHeader,
        manifest: RestoreManifest,
    }

    /// The reader's half: take every dead writer's journal in `dir` and decide what this launch
    /// reopens.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CrashJournalClaim",
            action = "Claim",
            project = "aterm_gui::crash_journal_conformance::Rig::project"
        )
    )]
    pub(crate) fn claim_at_boot(
        dir: &Path,
        evidence: &Evidence<'_>,
        own_pid: u32,
        quit: QuitLayout,
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
        for &id in &journals {
            if !writer_dead(dir, id) {
                continue;
            }
            let Ok(taken) = take(dir, id, own_pid) else {
                continue;
            };
            claim.taken.push(id);
            let _ = fs::remove_file(dir.join(id.lock_name()));
            let read = read_taken(&taken);
            if quit.names(id) {
                let _ = fs::remove_file(&taken);
                claim.set_aside.push((
                    id,
                    "its own run's quit layout reopens instead (the quit was cut short before \
                     the journal went)",
                ));
                continue;
            }
            let next_run = journals
                .iter()
                .filter(|other| other.pid == id.pid && other.nanos > id.nanos)
                .map(|other| other.nanos)
                .min();
            let Some(class) = classify_death(evidence, id, next_run) else {
                let _ = fs::remove_file(&taken);
                claim
                    .set_aside
                    .push((id, "no crash evidence of its run (a clean end leaves none)"));
                continue;
            };
            match read.and_then(|text| decode(&text)) {
                Err(error) => claim.notes.push(Note::Unreadable {
                    id,
                    class,
                    path: keep_unreadable(dir, id, &taken),
                    error,
                }),
                Ok((header, manifest))
                    if header.probation && (class.crashed() || header.second_chance) =>
                {
                    let skipped = Reopened {
                        writers: vec![header.image_writer(id); manifest.windows.len()],
                        manifest,
                        class,
                        sources: vec![id],
                        handed: Vec::new(),
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
                Ok((header, manifest)) if quit.may_be_its_run(&header, &manifest) => {
                    claim.set_aside.push((
                        id,
                        "its desk is the quit layout's, which an older build wrote without \
                         naming its run (a quit cut short before its journal went)",
                    ));
                }
                Ok((header, manifest)) => candidates.push(Candidate {
                    id,
                    class,
                    header,
                    manifest,
                }),
            }
            let _ = fs::remove_file(&taken);
        }
        for id in locks {
            if !dir.join(id.file_name()).exists() && writer_dead(dir, id) {
                let _ = fs::remove_file(dir.join(id.lock_name()));
            }
        }
        clear_leftovers(dir, &leftovers);
        claim.reopened = merge(candidates, quit.windows, &mut claim.set_aside);
        let _ = sync_dir(dir, None);
        claim
    }

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

    /// How many unreadable images [`keep_unreadable`] keeps aside.
    pub(super) const KEEP_UNREADABLE: usize = 4;

    fn keep_unreadable(dir: &Path, id: JournalId, taken: &Path) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt as _;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(taken)
            .ok()?;
        let kept = dir.join(id.unreadable_name());
        let private = file
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_file() && meta.nlink() == 1)
            && file
                .set_permissions(fs::Permissions::from_mode(0o600))
                .is_ok();
        if !private || fs::rename(taken, &kept).is_err() {
            return None;
        }
        let mut aside: Vec<(JournalId, PathBuf)> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let other = JournalId::parse_unreadable(entry.file_name().to_str()?)?;
                Some((other, entry.path()))
            })
            .collect();
        aside.sort_unstable_by_key(|(other, _)| std::cmp::Reverse(other.nanos));
        for (_, path) in aside.into_iter().skip(KEEP_UNREADABLE) {
            let _ = fs::remove_file(path);
        }
        kept.exists().then_some(kept)
    }

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
                        .is_some_and(|pid| !aterm_uds::process::pid_alive(pid))
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

    fn merge(
        mut candidates: Vec<Candidate>,
        quit_windows: usize,
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
        let mut handed = Vec::new();
        let mut programs = Some(Vec::new());
        let mut second_chance = false;
        for candidate in candidates {
            if quit_windows + windows.len() + candidate.manifest.windows.len()
                > crate::restore::MAX_WINDOWS
            {
                set_aside.push((candidate.id, "more windows than a layout carries"));
                continue;
            }
            let writer = candidate.header.image_writer(candidate.id);
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
            second_chance |= candidate.header.probation;
            handed.extend(candidate.header.handed.iter().map(|pair| (writer, *pair)));
            sources.push(candidate.id);
            writers.extend(std::iter::repeat_n(
                writer,
                candidate.manifest.windows.len(),
            ));
            windows.extend(candidate.manifest.windows);
        }
        if sources.is_empty() {
            return None;
        }
        Some(Reopened {
            manifest: RestoreManifest::new(windows),
            class,
            sources,
            writers,
            handed,
            programs,
            second_chance,
        })
    }
}

/// The Commit's take-over, run by the Commit waiter around its release of the adopted readers.
#[derive(Debug)]
pub(crate) struct TakeOver {
    dir: PathBuf,
    id: JournalId,
    predecessor: u32,
    born: Option<u128>,
    slot: TakeOverSlot,
}

#[derive(Clone, Debug, Default)]
struct TakeOverSlot {
    taken_over: Arc<Mutex<Option<Result<Taken, String>>>>,
}

#[derive(Debug)]
struct Taken {
    owner: writer::Owned,
    journal: Option<JournalId>,
    image: Option<(JournalHeader, RestoreManifest)>,
    parent_lock: Option<PathBuf>,
}

struct Armed {
    dir: PathBuf,
    owner: writer::Owned,
    journal: Option<JournalId>,
}

impl TakeOver {
    /// The waiter's take-over, around the readers' `release`.
    pub(crate) fn run(self, release: impl FnOnce()) {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let slot = self.slot.clone();
        let armed = catch_unwind(AssertUnwindSafe(|| self.arm()));
        release();
        let taken = match armed {
            Ok(Ok(armed)) => catch_unwind(AssertUnwindSafe(|| armed.settle()))
                .map_err(|_| "its read-back panicked".to_string()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("it panicked".to_string()),
        };
        *slot
            .taken_over
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(taken);
    }

    fn take(self) -> Result<Taken, String> {
        self.arm().map(Armed::settle)
    }

    fn arm(self) -> Result<Armed, String> {
        #[cfg(unix)]
        {
            let (owner, journal) =
                JournalOwner::adopt(&self.dir, self.id, self.predecessor, self.born)?;
            Ok(Armed {
                dir: self.dir,
                owner,
                journal,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (self.id, self.predecessor, self.born);
            Ok(Armed {
                dir: self.dir,
                owner: (),
                journal: None,
            })
        }
    }
}

impl Armed {
    fn settle(self) -> Taken {
        #[cfg(unix)]
        {
            let parent_lock = self
                .journal
                .and_then(|parent| unix::settle_adoption(&self.dir, parent));
            let image = self
                .journal
                .and_then(|_| unix::read_image(&self.owner.path()).ok());
            Taken {
                owner: self.owner,
                journal: self.journal,
                image,
                parent_lock,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.dir;
            Taken {
                owner: self.owner,
                journal: self.journal,
                image: None,
                parent_lock: None,
            }
        }
    }
}

#[derive(Debug, Default)]
struct Inherited {
    /// The update's parent: the pid the shells it handed were keyed by.
    parent: u32,
    /// What the parent's image said of each of its shells, by the parent's pool id.
    leaves: std::collections::BTreeMap<u64, InheritedLeaf>,
}

#[derive(Debug, Default)]
struct InheritedLeaf {
    cwd: Option<String>,
    title: String,
    agent: Option<Box<crate::restore::AgentRestore>>,
    /// The pairs that image said this shell had been handed as, in order.
    handed: Vec<(u32, u64)>,
}

impl Inherited {
    fn of(
        parent: u32,
        journal: Option<JournalId>,
        image: Option<(JournalHeader, RestoreManifest)>,
    ) -> Self {
        use crate::restore::{RestoredSplitTree, RestoredView};
        fn collect(node: RestoredSplitTree, out: &mut Vec<(u64, InheritedLeaf)>) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(leaf),
                } => {
                    if let Some(id) = leaf.local_id {
                        out.push((
                            id,
                            InheritedLeaf {
                                cwd: leaf.cwd,
                                title: leaf.title,
                                agent: leaf.agent,
                                handed: Vec::new(),
                            },
                        ));
                    }
                }
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    collect(*first, out);
                    collect(*second, out);
                }
            }
        }
        let mut inherited = Self {
            parent,
            leaves: std::collections::BTreeMap::new(),
        };
        let (Some(journal), Some((header, manifest))) = (journal, image) else {
            return inherited;
        };
        if header.image_writer(journal) != parent {
            return inherited;
        }
        let mut leaves = Vec::new();
        for window in manifest.windows {
            for tab in window.restored_tabs {
                collect(tab.root, &mut leaves);
            }
        }
        inherited.leaves = leaves.into_iter().collect();
        for pair in header.handed {
            if let Some(leaf) = inherited.leaves.get_mut(&pair.id) {
                leaf.handed.push((pair.pid, pair.was));
            }
        }
        inherited
    }
}

/// One session the update's parent handed this run, as a capture sees it ([`Lane::carry_forward`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct HandedSession {
    pub(crate) was: u64,
    pub(crate) runs: Runs,
    pub(crate) keyed: Option<(u32, u64)>,
}

/// What this run's own status read says runs in a session (`session_timeline::AgentPublication`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Runs {
    /// Nothing read.
    Unread,
    /// An agent the supervisor hosts (`harness_host::agent_of`).
    Agent(aterm_phase::Program),
    /// A program that is no such agent (the shell at its prompt, vim, ssh).
    Other,
}

impl Runs {
    fn may_be(self, record: &crate::restore::AgentRestore) -> bool {
        let kind = record
            .agent()
            .map(aterm_agent::harness::upgrade::Agent::program)
            .filter(|program| {
                aterm_agent::harness::lanes::lanes(*program)
                    != aterm_agent::harness::lanes::Lanes::NONE
            });
        match self {
            Self::Unread => true,
            Self::Agent(program) => kind == Some(program),
            Self::Other => false,
        }
    }
}

fn each_terminal_leaf(
    manifest: &mut RestoreManifest,
    leaf: &mut dyn FnMut(&mut crate::restore::TerminalLeafRestore),
    mirror: &mut dyn FnMut(u64, &mut Option<String>, &mut String),
) {
    use crate::restore::{PaneLayout, RestoredSplitTree, RestoredView};
    fn tree(
        node: &mut RestoredSplitTree,
        leaf: &mut dyn FnMut(&mut crate::restore::TerminalLeafRestore),
    ) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(terminal),
            } => leaf(terminal),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                tree(first, leaf);
                tree(second, leaf);
            }
        }
    }
    fn legacy(
        node: &mut PaneLayout,
        mirror: &mut dyn FnMut(u64, &mut Option<String>, &mut String),
    ) {
        match node {
            PaneLayout::Leaf {
                cwd,
                title,
                local_id: Some(id),
                ..
            } => mirror(*id, cwd, title),
            PaneLayout::Leaf { .. } => {}
            PaneLayout::Split { first, second, .. } => {
                legacy(first, mirror);
                legacy(second, mirror);
            }
        }
    }
    for window in &mut manifest.windows {
        for tab in &mut window.restored_tabs {
            tree(&mut tab.root, leaf);
        }
        for pane in &mut window.tabs {
            legacy(pane, mirror);
        }
    }
}

/// How long a control verb that moved a word about typing (`hold`, `meta set|unset autopilot`).
#[cfg(not(test))]
pub(crate) const WORD_DURABLE_WAIT: Duration = Duration::from_secs(5);
#[cfg(test)]
pub(crate) const WORD_DURABLE_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

/// What the journal holds on disk of the words about typing ([`OwnerWords`]).
#[derive(Clone, Default)]
pub(crate) struct Durable {
    shared: Arc<DurableShared>,
}

#[derive(Default)]
struct DurableShared {
    state: Mutex<DurableState>,
    moved: std::sync::Condvar,
    /// Wakes the main loop ([`Durable::set_wake`]).
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Debug, Default)]
struct DurableState {
    /// A lane journals the desk.
    live: bool,
    /// Tickets handed out ([`Durable::ask`]).
    asked: u64,
    /// The newest ticket an image on disk covers ([`Durable::cover`]).
    covered: u64,
    /// The writer's newest image did not reach the disk, and the lane has not heard it.
    failed: bool,
    /// A test's own bound for the verbs ([`Durable::set_word_bound`]).
    #[cfg(test)]
    word_bound: Option<Duration>,
}

/// How a ticket's wait ended ([`Durable::wait`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settled {
    /// The image that carries the word is on disk.
    Durable,
    /// No lane journals the desk (inert, or `restore_session` off).
    Unjournaled,
    /// The bound passed first.
    Late,
}

impl std::fmt::Debug for Durable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Durable").field(&*self.lock()).finish()
    }
}

impl Durable {
    fn lock(&self) -> std::sync::MutexGuard<'_, DurableState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// What wakes the main loop for a ticket or a failed write (the App's proxy; a test's counter).
    pub(crate) fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(wake));
    }

    fn wake(&self) {
        let wake = self
            .shared
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// A ticket for the words stored before this call, and the main loop woken to capture them.
    pub(crate) fn ask(&self) -> Option<u64> {
        let ticket = {
            let mut state = self.lock();
            if !state.live {
                return None;
            }
            state.asked += 1;
            state.asked
        };
        self.wake();
        Some(ticket)
    }

    /// Wait until `ticket` is covered, no lane journals the desk any more, or `bound` passes.
    pub(crate) fn wait(&self, ticket: u64, bound: Duration) -> Settled {
        let deadline = Instant::now() + bound;
        let mut state = self.lock();
        loop {
            if state.covered >= ticket {
                return Settled::Durable;
            }
            if !state.live {
                return Settled::Unjournaled;
            }
            let now = Instant::now();
            if now >= deadline {
                return Settled::Late;
            }
            state = self
                .shared
                .moved
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The verb's answer, once its word is durable.
    pub(crate) fn settle(&self, reply: String, bound: Duration) -> String {
        let Some(ticket) = self.ask() else {
            return reply;
        };
        match self.wait(ticket, bound) {
            Settled::Durable | Settled::Unjournaled => reply,
            Settled::Late => {
                aterm_log::warn!(
                    "crash journal: a word about typing was answered before an image carrying \
                     it reached the disk ({} ms)",
                    bound.as_millis()
                );
                let line = reply.strip_suffix('\n').unwrap_or(&reply);
                format!("{line} journaled=0\n")
            }
        }
    }

    /// How long a verb waits on its word's ticket ([`Self::settle`]).
    pub(crate) fn word_bound(&self) -> Duration {
        #[cfg(test)]
        if let Some(bound) = self.lock().word_bound {
            return bound;
        }
        WORD_DURABLE_WAIT
    }

    /// A test's bound for the verbs ([`Self::word_bound`]).
    #[cfg(test)]
    pub(crate) fn set_word_bound(&self, bound: Duration) {
        self.lock().word_bound = Some(bound);
    }

    fn asked(&self) -> u64 {
        self.lock().asked
    }

    fn set_live(&self, live: bool) {
        let mut state = self.lock();
        if state.live != live {
            state.live = live;
            self.shared.moved.notify_all();
        }
    }

    fn cover(&self, up_to: u64) {
        let mut state = self.lock();
        if up_to > state.covered {
            state.covered = up_to;
            self.shared.moved.notify_all();
        }
    }

    fn note_failed(&self) {
        self.lock().failed = true;
        self.wake();
    }

    fn take_failed(&self) -> bool {
        std::mem::take(&mut self.lock().failed)
    }

    /// A test's look: the newest ticket covered.
    #[cfg(test)]
    pub(crate) fn covered(&self) -> u64 {
        self.lock().covered
    }
}

/// The writer's lane on the App.
#[derive(Debug)]
pub(crate) struct Lane {
    /// The journal directory, or `None` for an inert lane.
    dir: Option<PathBuf>,
    pid: u32,
    /// Something may have changed since the last capture.
    dirty: bool,
    /// Something that was not a timer woke the loop since the last capture.
    activity: bool,
    /// The trailing settle (lc-1).
    settle_owed: bool,
    /// No capture before this instant.
    not_before: Option<Instant>,
    /// A tab's standing words about typing moved from what the last image carries.
    owed_now: bool,
    /// What the disk holds of the words, shared with the writer and the verbs that wait on it.
    durable: Durable,
    /// The newest ticket a capture has carried ([`Self::begin_capture`]).
    confirmed: u64,
    /// The tickets read out before the capture now under way, which its offer covers.
    capturing: Option<u64>,
    /// The last image handed to the writer did not reach the disk (a full disk).
    unwritten: bool,
    /// This launch reopened a crash journal.
    probation_until: Option<Instant>,
    /// That journal was a second chance ([`Reopened::second_chance`]).
    second_chance: bool,
    /// The image last handed to the writer.
    last: Option<Written>,
    published_at: Option<Instant>,
    /// A capture found only titles moved, inside [`TITLE_WRITE_INTERVAL`].
    title_owed: bool,
    writer: Option<writer::Writer>,
    /// This run's journal's name, fixed at its first use.
    id: Option<JournalId>,
    /// The update's parent whose journal this run takes over at Commit.
    predecessor: Option<(u32, Option<u128>)>,
    /// Where the Commit waiter leaves the take-over it ran before it released the readers.
    take_over: Option<TakeOverSlot>,
    /// The journal holds the parent's image, taken at Commit.
    adopted: bool,
    /// What this run took over at its Commit and carries forward until it has facts of its own.
    inherited: Option<Inherited>,
    /// The pairs the last capture's leaves were handed as
    /// ([`JournalHeader::handed`], [`Self::carry_forward`]).
    handed: Vec<Handed>,
    /// What the disk holds: `true` from a take-over that found the parent's image until the
    /// writer has really replaced it (published an image of this run's own, or withdrawn it).
    inherited_on_disk: Arc<AtomicBool>,
    /// Images handed to the writer.
    pub(crate) writes: u64,
    /// Captures taken.
    pub(crate) captures: u64,
}

/// A tab's standing words about typing, as its terminal leaf carries them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OwnerWords {
    pub(crate) autopilot: Option<String>,
    pub(crate) land_at: Option<i64>,
    pub(crate) holds: Vec<String>,
}

impl OwnerWords {
    /// The words `leaf` carries.
    pub(crate) fn of_leaf(leaf: &crate::restore::TerminalLeafRestore) -> Self {
        Self {
            autopilot: leaf.autopilot.clone(),
            land_at: leaf.land_at,
            holds: leaf.holds.clone(),
        }
    }

    fn carried(manifest: &RestoreManifest, local_id: u64) -> Option<Self> {
        use crate::restore::{RestoredSplitTree, RestoredView};
        fn find(node: &RestoredSplitTree, local_id: u64) -> Option<OwnerWords> {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(leaf),
                } => (leaf.local_id == Some(local_id)).then(|| OwnerWords::of_leaf(leaf)),
                RestoredSplitTree::Leaf { .. } => None,
                RestoredSplitTree::Split { first, second, .. } => {
                    find(first, local_id).or_else(|| find(second, local_id))
                }
            }
        }
        manifest
            .windows
            .iter()
            .flat_map(|window| &window.restored_tabs)
            .find_map(|tab| find(&tab.root, local_id))
    }
}

impl OwnerWords {
    fn all_carried(manifest: &RestoreManifest) -> std::collections::BTreeMap<u64, Self> {
        use crate::restore::{RestoredSplitTree, RestoredView};
        fn walk(node: &RestoredSplitTree, out: &mut std::collections::BTreeMap<u64, OwnerWords>) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(leaf),
                } => {
                    if let Some(id) = leaf.local_id {
                        out.insert(id, OwnerWords::of_leaf(leaf));
                    }
                }
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    walk(first, out);
                    walk(second, out);
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        for tab in manifest
            .windows
            .iter()
            .flat_map(|window| &window.restored_tabs)
        {
            walk(&tab.root, &mut out);
        }
        out
    }
}

/// How stale Windows' end-session snapshot may grow before the loop takes it again.
pub(crate) const END_SESSION_SNAPSHOT_TTL: Duration = Duration::from_secs(2);

/// When WINDOWS' end-session snapshot is taken again.
#[derive(Debug, Default)]
pub(crate) struct EndSessionCadence {
    /// When the last snapshot was taken.
    taken: Option<Instant>,
    /// What it carries of each session's words, by local id.
    carried: std::collections::BTreeMap<u64, OwnerWords>,
    /// A word moved from what it carries.
    owed: bool,
}

#[cfg_attr(
    not(any(windows, test)),
    allow(
        dead_code,
        reason = "only the Windows lane takes the end-session snapshot; every host tests its cadence"
    )
)]
impl EndSessionCadence {
    /// Whether a snapshot was ever taken.
    pub(crate) const fn taking(&self) -> bool {
        self.taken.is_some()
    }

    /// Session `local_id`'s words are `words` now.
    pub(crate) fn note_owner_words(&mut self, local_id: u64, words: &OwnerWords) {
        if self
            .carried
            .get(&local_id)
            .is_some_and(|carried| carried != words)
        {
            self.owed = true;
        }
    }

    /// Whether the snapshot is due at `now`.
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.owed
            || self
                .taken
                .is_none_or(|at| now.saturating_duration_since(at) >= END_SESSION_SNAPSHOT_TTL)
    }

    /// A snapshot was taken at `now`.
    pub(crate) fn took(&mut self, manifest: Option<&RestoreManifest>, now: Instant) {
        self.taken = Some(now);
        self.owed = false;
        self.carried = manifest.map(OwnerWords::all_carried).unwrap_or_default();
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Written {
    manifest: RestoreManifest,
    probation: bool,
    /// The leaves that had a program running.
    programs: Vec<LeafProgram>,
    /// What its leaves were handed as ([`JournalHeader::handed`]).
    handed: Vec<Handed>,
}

impl Lane {
    /// A lane that writes into `dir` as `pid`, or an inert one for `None`.
    pub(crate) fn new(dir: Option<PathBuf>, pid: u32) -> Self {
        let dir = if cfg!(unix) { dir } else { None };
        let durable = Durable::default();
        durable.set_live(dir.is_some());
        Self {
            dir,
            pid,
            dirty: true,
            activity: false,
            settle_owed: false,
            not_before: None,
            owed_now: false,
            durable,
            confirmed: 0,
            capturing: None,
            unwritten: false,
            probation_until: None,
            second_chance: false,
            last: None,
            published_at: None,
            title_owed: false,
            writer: None,
            id: None,
            predecessor: None,
            take_over: None,
            adopted: false,
            inherited: None,
            handed: Vec::new(),
            inherited_on_disk: Arc::new(AtomicBool::new(false)),
            writes: 0,
            captures: 0,
        }
    }

    fn run_id(&mut self) -> JournalId {
        let pid = self.pid;
        *self.id.get_or_insert_with(|| JournalId::of_this_run(pid))
    }

    /// The run a quit's `session.toml` names (`restore::write_quit`).
    pub(crate) fn run(&mut self) -> Option<JournalId> {
        self.dir.is_some().then(|| self.run_id())
    }

    /// This run's journal id, for a test that plays its crash marker.
    #[cfg(test)]
    pub(crate) fn journal_id(&mut self) -> JournalId {
        self.run_id()
    }

    /// This launch is an update's candidate whose parent is `pid`, started at `born`.
    pub(crate) const fn set_predecessor(&mut self, pid: u32, born: Option<u128>) {
        self.predecessor = Some((pid, born));
    }

    /// The Commit's take-over, prepared for the Commit waiter.
    pub(crate) fn take_over_at_commit(&mut self, restore_session: bool) -> Option<TakeOver> {
        let (predecessor, born) = self.predecessor?;
        let dir = self.dir.clone()?;
        if !restore_session || self.writer.is_some() {
            return None;
        }
        let slot = TakeOverSlot::default();
        self.take_over = Some(slot.clone());
        Some(TakeOver {
            dir,
            id: self.run_id(),
            predecessor,
            born,
            slot,
        })
    }

    /// The Commit's take-over, installed at the Commit's wake.
    pub(crate) fn adopt_predecessor(&mut self, restore_session: bool) -> Option<JournalId> {
        let (predecessor, born) = self.predecessor.take()?;
        let dir = self.dir.clone()?;
        let ran = self.take_over.take().and_then(|slot| {
            slot.taken_over
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
        });
        let taken = match ran {
            Some(ran) => ran,
            None if !restore_session || self.writer.is_some() => return None,
            None => TakeOver {
                dir: dir.clone(),
                id: self.run_id(),
                predecessor,
                born,
                slot: TakeOverSlot::default(),
            }
            .take(),
        };
        let taken = match taken {
            Ok(taken) => taken,
            Err(error) => {
                aterm_log::warn!("crash journal: the update's parent's not taken over ({error})");
                return None;
            }
        };
        let id = self.run_id();
        self.inherited_on_disk
            .store(taken.journal.is_some(), Ordering::Release);
        #[cfg_attr(
            not(unix),
            expect(
                clippy::unit_arg,
                reason = "off unix there is no journal to own: `writer::Owned` is `()`"
            )
        )]
        let writer = writer::Writer::spawn_owning(
            dir,
            id,
            taken.owner,
            taken.parent_lock,
            Arc::clone(&self.inherited_on_disk),
            self.durable.clone(),
        );
        self.writer = Some(writer);
        self.adopted = taken.journal.is_some();
        self.inherited = Some(Inherited::of(predecessor, taken.journal, taken.image));
        taken.journal
    }

    /// Carry forward what this successor took over until it has facts of its own.
    pub(crate) fn carry_forward(
        &mut self,
        manifest: &mut RestoreManifest,
        adopted: &dyn Fn(u64) -> Option<HandedSession>,
    ) {
        self.handed.clear();
        let Some(inherited) = self.inherited.as_ref() else {
            return;
        };
        let parent_leaf = |id: u64| {
            let session = adopted(id)?;
            Some((session, inherited.leaves.get(&session.was)))
        };
        let heal =
            |before: Option<&InheritedLeaf>, cwd: &mut Option<String>, title: &mut String| {
                if cwd.is_none()
                    && title.is_empty()
                    && let Some(before) = before
                {
                    cwd.clone_from(&before.cwd);
                    title.clone_from(&before.title);
                }
            };
        let handed = &mut self.handed;
        let mut carried = Vec::new();
        let mut known_agent = Vec::new();
        each_terminal_leaf(
            manifest,
            &mut |leaf| {
                let Some(id) = leaf.local_id else {
                    return;
                };
                let Some((session, before)) = parent_leaf(id) else {
                    return;
                };
                carried.push(session.was);
                handed.extend(
                    std::iter::once((inherited.parent, session.was))
                        .chain(session.keyed)
                        .chain(
                            before
                                .into_iter()
                                .flat_map(|leaf| leaf.handed.iter().copied()),
                        )
                        .take(MAX_HANDED_GENERATIONS)
                        .map(|(pid, was)| Handed { id, pid, was }),
                );
                if let Some(record) = before.and_then(|before| before.agent.as_ref()) {
                    if leaf.agent.is_some() || !session.runs.may_be(record) {
                        known_agent.push(session.was);
                    } else {
                        leaf.agent = Some(record.clone());
                        leaf.held = false;
                    }
                }
                heal(before, &mut leaf.cwd, &mut leaf.title);
            },
            &mut |id, cwd, title| heal(parent_leaf(id).and_then(|(_, before)| before), cwd, title),
        );
        let Some(inherited) = self.inherited.as_mut() else {
            return;
        };
        for was in known_agent {
            if let Some(leaf) = inherited.leaves.get_mut(&was) {
                leaf.agent = None;
            }
        }
        for was in carried {
            if let Some(leaf) = inherited.leaves.get_mut(&was) {
                leaf.cwd = None;
                leaf.title.clear();
            }
        }
    }

    /// A lane that never writes (the test App's).
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn inert() -> Self {
        Self::new(None, 0)
    }

    /// The update's parent whose taken-over image is still this run's journal, or `None`.
    pub(crate) fn inherited_image_parent(&self) -> Option<u32> {
        self.inherited_on_disk
            .load(Ordering::Acquire)
            .then(|| self.inherited.as_ref().map(|inherited| inherited.parent))
            .flatten()
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.dir.is_some()
    }

    /// What the disk holds of the words about typing ([`Durable`]).
    pub(crate) fn durable(&self) -> Durable {
        self.durable.clone()
    }

    /// Whether this lane journals the desk now.
    pub(crate) fn set_live(&self, restore_session: bool) {
        self.durable.set_live(self.dir.is_some() && restore_session);
    }

    /// The writer's word since the last look.
    pub(crate) fn poll_writer(&mut self) {
        if self.durable.take_failed() && self.last.is_some() {
            self.unwritten = true;
            self.dirty = true;
        }
    }

    /// A capture begins: the tickets handed out so far ([`Durable::ask`]) are read out.
    pub(crate) fn begin_capture(&mut self) {
        self.capturing = Some(self.durable.asked());
    }

    fn ticket_owed(&self) -> bool {
        self.durable.asked() > self.confirmed
    }

    fn confirm(&mut self, covers: Option<u64>) {
        let Some(covers) = covers.filter(|covers| *covers > self.confirmed) else {
            return;
        };
        if let Some(writer) = self.writer.as_ref() {
            writer.confirm(covers);
            self.confirmed = covers;
        }
    }

    /// Something that was not a timer woke the loop.
    pub(crate) const fn note_activity(&mut self) {
        self.dirty = true;
        self.activity = true;
    }

    /// Session `local_id`'s standing words about typing are now `words` ([`OwnerWords`]).
    pub(crate) fn note_owner_words(&mut self, local_id: u64, words: &OwnerWords) -> bool {
        if self.dir.is_none() {
            return false;
        }
        let carried = self
            .last
            .as_ref()
            .and_then(|last| OwnerWords::carried(&last.manifest, local_id));
        let owed = carried.is_some_and(|carried| carried != *words);
        if owed {
            self.dirty = true;
            self.owed_now = true;
        }
        owed
    }

    /// What a restore carries of a tab's agent moved.
    pub(crate) const fn owe_agent_capture(&mut self) {
        if self.dir.is_some() {
            self.dirty = true;
            self.owed_now = true;
        }
    }

    /// Whether a capture was owed, clearing it (tests of what marks it).
    #[cfg(test)]
    pub(crate) fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// This launch reopened a crash journal at `now`.
    pub(crate) fn begin_probation(&mut self, now: Instant, second_chance: bool) {
        self.probation_until = Some(now + PROBATION);
        self.second_chance = second_chance;
    }

    fn on_probation(&self, now: Instant) -> bool {
        self.probation_until.is_some_and(|until| now < until)
    }

    fn settle_due(&self, now: Instant) -> bool {
        self.last
            .as_ref()
            .is_some_and(|written| written.probation && !self.on_probation(now))
    }

    fn title_due_at(&self) -> Option<Instant> {
        self.title_owed
            .then(|| self.published_at.map(|at| at + TITLE_WRITE_INTERVAL))
            .flatten()
    }

    /// Whether a capture is due now.
    pub(crate) fn capture_due(&self, now: Instant) -> bool {
        self.dir.is_some()
            && (self.owed_now
                || self.ticket_owed()
                || (self.not_before.is_none_or(|at| now >= at)
                    && (self.dirty
                        || self.settle_owed
                        || self.settle_due(now)
                        || self.title_due_at().is_some_and(|at| now >= at))))
    }

    /// Offer a fresh capture taken at `now`.
    pub(crate) fn offer(
        &mut self,
        mut manifest: RestoreManifest,
        programs: Vec<LeafProgram>,
        now: Instant,
    ) -> bool {
        self.captures = self.captures.saturating_add(1);
        self.dirty = false;
        self.settle_owed = std::mem::take(&mut self.activity);
        self.owed_now = false;
        let covers = self.capturing.take();
        self.not_before = Some(now + MIN_WRITE_INTERVAL);
        if let Some(last) = &self.last {
            heal_busy_leaves(&mut manifest, &last.manifest);
        }
        let written = Written {
            manifest,
            probation: self.on_probation(now),
            programs,
            handed: std::mem::take(&mut self.handed),
        };
        if !self.unwritten && self.last.as_ref() == Some(&written) {
            self.title_owed = false;
            self.confirm(covers);
            return false;
        }
        if !self.unwritten
            && let Some(last) = &self.last
            && last.probation == written.probation
            && last.programs == written.programs
            && last.handed == written.handed
            && same_but_titles(&last.manifest, &written.manifest)
            && self
                .published_at
                .is_some_and(|at| now < at + TITLE_WRITE_INTERVAL)
        {
            self.title_owed = true;
            self.confirm(covers);
            return false;
        }
        let Some(dir) = self.dir.clone() else {
            return false;
        };
        let header = JournalHeader::now(self.pid, written.probation)
            .with_second_chance(written.probation && self.second_chance)
            .with_programs(written.programs.clone())
            .with_handed(written.handed.clone());
        let id = self.run_id();
        let writer = self.writer.get_or_insert_with(|| {
            writer::Writer::spawn(
                dir,
                id,
                Arc::clone(&self.inherited_on_disk),
                self.durable.clone(),
            )
        });
        let covers = covers.map_or(self.confirmed, |covers| covers.max(self.confirmed));
        writer.publish(written.manifest.clone(), header, covers);
        self.confirmed = covers;
        self.unwritten = false;
        self.adopted = false;
        self.last = Some(written);
        self.published_at = Some(now);
        self.title_owed = false;
        self.writes = self.writes.saturating_add(1);
        true
    }

    /// `restore_session` went off while this process runs.
    pub(crate) fn withdraw(&mut self) {
        self.title_owed = false;
        self.unwritten = false;
        self.settle_owed = false;
        self.activity = false;
        self.durable.set_live(false);
        let written = self.last.take().is_some();
        if std::mem::take(&mut self.adopted) || written {
            if let Some(writer) = self.writer.as_ref() {
                writer.withdraw();
            }
            self.dirty = true;
            self.not_before = None;
        }
    }

    /// The next instant the loop must wake for this lane.
    pub(crate) fn next_wake(&self, now: Instant) -> Option<Instant> {
        self.dir.as_ref()?;
        let floor = |at: Instant| self.not_before.map_or(at, |not_before| not_before.max(at));
        let capture = (self.dirty || self.settle_owed).then_some(now);
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

    /// The clean quit's half: remove this process's journal, waiting at most `patience`.
    pub(crate) fn retire(&mut self, patience: Duration) -> Result<(), String> {
        self.durable.set_live(false);
        self.last = None;
        self.adopted = false;
        match self.writer.take() {
            Some(writer) => writer.retire(patience),
            None => Ok(()),
        }
    }
}

fn same_but_titles(a: &RestoreManifest, b: &RestoreManifest) -> bool {
    use crate::restore::{
        PaneLayout, RestoreManifest, RestoredSplitTree, RestoredTab, RestoredView,
        TerminalLeafRestore, WindowLayout,
    };
    fn same_terminal_leaf(a: &TerminalLeafRestore, b: &TerminalLeafRestore) -> bool {
        let TerminalLeafRestore {
            cwd: a_cwd,
            title: _,
            profile: a_profile,
            local_id: a_local_id,
            user_title: a_user_title,
            description: a_description,
            icon: a_icon,
            role: a_role,
            attention: a_attention,
            questions: a_questions,
            autopilot: a_autopilot,
            agent: a_agent,
            supervised: a_supervised,
            held: a_held,
            holds: a_holds,
            sid: a_sid,
            lineage: a_lineage,
            log_ep: a_log_ep,
            land_at: a_land_at,
            autopilot_by: a_autopilot_by,
            theme: a_theme,
            lost: a_lost,
        } = a;
        let TerminalLeafRestore {
            cwd: b_cwd,
            title: _,
            profile: b_profile,
            local_id: b_local_id,
            user_title: b_user_title,
            description: b_description,
            icon: b_icon,
            role: b_role,
            attention: b_attention,
            questions: b_questions,
            autopilot: b_autopilot,
            agent: b_agent,
            supervised: b_supervised,
            held: b_held,
            holds: b_holds,
            sid: b_sid,
            lineage: b_lineage,
            log_ep: b_log_ep,
            land_at: b_land_at,
            autopilot_by: b_autopilot_by,
            theme: b_theme,
            lost: b_lost,
        } = b;
        a_cwd == b_cwd
            && a_profile == b_profile
            && a_local_id == b_local_id
            && a_user_title == b_user_title
            && a_description == b_description
            && a_icon == b_icon
            && a_role == b_role
            && a_attention == b_attention
            && a_questions == b_questions
            && a_autopilot == b_autopilot
            && a_agent == b_agent
            && a_supervised == b_supervised
            && a_held == b_held
            && a_holds == b_holds
            && a_sid == b_sid
            && a_lineage == b_lineage
            && a_log_ep == b_log_ep
            && a_land_at == b_land_at
            && a_autopilot_by == b_autopilot_by
            && a_theme == b_theme
            && a_lost == b_lost
    }
    fn same_tree(a: &RestoredSplitTree, b: &RestoredSplitTree) -> bool {
        match (a, b) {
            (
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(la),
                },
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(lb),
                },
            ) => same_terminal_leaf(la, lb),
            (RestoredSplitTree::Leaf { view: va }, RestoredSplitTree::Leaf { view: vb }) => {
                va == vb
            }
            (
                RestoredSplitTree::Split {
                    axis: aa,
                    ratio: ra,
                    first: fa,
                    second: sa,
                },
                RestoredSplitTree::Split {
                    axis: ab,
                    ratio: rb,
                    first: fb,
                    second: sb,
                },
            ) => aa == ab && ra == rb && same_tree(fa, fb) && same_tree(sa, sb),
            _ => false,
        }
    }
    fn same_pane(a: &PaneLayout, b: &PaneLayout) -> bool {
        match (a, b) {
            (
                PaneLayout::Leaf {
                    cwd: ca,
                    title: _,
                    focused: fa,
                    local_id: la,
                },
                PaneLayout::Leaf {
                    cwd: cb,
                    title: _,
                    focused: fb,
                    local_id: lb,
                },
            ) => ca == cb && fa == fb && la == lb,
            (
                PaneLayout::Split {
                    dir: da,
                    ratio: ra,
                    first: fa,
                    second: sa,
                },
                PaneLayout::Split {
                    dir: db,
                    ratio: rb,
                    first: fb,
                    second: sb,
                },
            ) => da == db && ra == rb && same_pane(fa, fb) && same_pane(sa, sb),
            _ => false,
        }
    }
    fn same_tab(a: &RestoredTab, b: &RestoredTab) -> bool {
        let RestoredTab {
            root: ra,
            focused_path: fa,
            zoomed: za,
        } = a;
        let RestoredTab {
            root: rb,
            focused_path: fb,
            zoomed: zb,
        } = b;
        za == zb && fa == fb && same_tree(ra, rb)
    }
    fn same_window(a: &WindowLayout, b: &WindowLayout) -> bool {
        let WindowLayout {
            rows: ra,
            cols: ca,
            active_tab: ata,
            outer_x: oxa,
            outer_y: oya,
            maximized: ma,
            inner_px: ipa,
            presence_row: pra,
            show: sa,
            tabs: ta,
            native_tabs: nta,
            tab_order: toa,
            active_item: aia,
            restored_tabs: rta,
        } = a;
        let WindowLayout {
            rows: rb,
            cols: cb,
            active_tab: atb,
            outer_x: oxb,
            outer_y: oyb,
            maximized: mb,
            inner_px: ipb,
            presence_row: prb,
            show: sb,
            tabs: tb,
            native_tabs: ntb,
            tab_order: tob,
            active_item: aib,
            restored_tabs: rtb,
        } = b;
        ra == rb
            && ca == cb
            && ata == atb
            && oxa == oxb
            && oya == oyb
            && ma == mb
            && ipa == ipb
            && pra == prb
            && sa == sb
            && aia == aib
            && toa == tob
            && nta == ntb
            && ta.len() == tb.len()
            && rta.len() == rtb.len()
            && ta.iter().zip(tb).all(|(pa, pb)| same_pane(pa, pb))
            && rta.iter().zip(rtb).all(|(ta, tb)| same_tab(ta, tb))
    }
    let RestoreManifest {
        schema: sa,
        fold: fa,
        windows: wa,
    } = a;
    let RestoreManifest {
        schema: sb,
        fold: fb,
        windows: wb,
    } = b;
    sa == sb
        && fa == fb
        && wa.len() == wb.len()
        && wa.iter().zip(wb).all(|(wa, wb)| same_window(wa, wb))
}

fn heal_busy_leaves(fresh: &mut RestoreManifest, last: &RestoreManifest) {
    use std::collections::BTreeMap;

    use crate::restore::{PaneLayout, RestoredSplitTree, RestoredView};
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

mod writer {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{Durable, JournalHeader, JournalId};
    use crate::restore::RestoreManifest;

    enum Job {
        /// An image, and the newest ticket its capture read out before it read the desk.
        Publish(Box<RestoreManifest>, JournalHeader, u64),
        Withdraw,
        /// A capture found nothing new.
        Confirm(u64),
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
                Self::Confirm(_) => "Confirm",
                Self::Retire(_) => "Retire",
            })
        }
    }

    impl Writer {
        pub(super) fn spawn(
            dir: PathBuf,
            id: JournalId,
            inherited: Inherited,
            durable: Durable,
        ) -> Self {
            Self::start(dir, id, None, None, inherited, durable)
        }

        /// A writer that owns `owner`.
        #[cfg(unix)]
        pub(super) fn spawn_owning(
            dir: PathBuf,
            id: JournalId,
            owner: super::JournalOwner,
            parent_lock: Option<PathBuf>,
            inherited: Inherited,
            durable: Durable,
        ) -> Self {
            Self::start(dir, id, Some(owner), parent_lock, inherited, durable)
        }

        /// No journal to own off unix.
        #[cfg(not(unix))]
        pub(super) fn spawn_owning(
            dir: PathBuf,
            id: JournalId,
            _owner: (),
            _parent_lock: Option<PathBuf>,
            inherited: Inherited,
            durable: Durable,
        ) -> Self {
            Self::start(dir, id, None, None, inherited, durable)
        }

        fn start(
            dir: PathBuf,
            id: JournalId,
            owner: Option<Owned>,
            parent_lock: Option<PathBuf>,
            inherited: Inherited,
            durable: Durable,
        ) -> Self {
            let (tx, rx) = mpsc::channel::<Job>();
            let thread = std::thread::Builder::new()
                .name("aterm-crash-journal".into())
                .spawn(move || run(&dir, id, &rx, owner, parent_lock, &inherited, &durable))
                .map_err(|error| {
                    aterm_log::warn!("crash journal: writer thread not spawned ({error})");
                })
                .ok();
            Self { tx, thread }
        }

        pub(super) fn publish(
            &self,
            manifest: RestoreManifest,
            header: JournalHeader,
            covers: u64,
        ) {
            let _ = self
                .tx
                .send(Job::Publish(Box::new(manifest), header, covers));
        }

        pub(super) fn confirm(&self, covers: u64) {
            let _ = self.tx.send(Job::Confirm(covers));
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

    /// Whether the journal still holds the image its lane took over at Commit.
    pub(super) type Inherited = std::sync::Arc<std::sync::atomic::AtomicBool>;

    /// The journal a writer owns.
    #[cfg(unix)]
    pub(super) type Owned = super::JournalOwner;
    #[cfg(not(unix))]
    pub(super) type Owned = ();

    enum Image {
        Publish(Box<RestoreManifest>, JournalHeader),
        Withdraw,
    }

    #[cfg(unix)]
    fn run(
        dir: &std::path::Path,
        id: JournalId,
        rx: &mpsc::Receiver<Job>,
        owner: Option<Owned>,
        parent_lock: Option<PathBuf>,
        inherited: &Inherited,
        durable: &Durable,
    ) {
        use std::sync::atomic::Ordering;
        crate::qos::set_self(crate::qos::Role::Background);
        let mut owner = owner;
        let mut parent_lock = parent_lock;
        let mut said = false;
        let mut on_disk = false;
        let mut owed: u64 = 0;
        while let Ok(first) = rx.recv() {
            let mut image = None;
            let mut retire = None;
            for job in std::iter::once(first).chain(rx.try_iter()) {
                match job {
                    Job::Publish(manifest, header, covers) => {
                        owed = owed.max(covers);
                        image = Some(Image::Publish(manifest, header));
                    }
                    Job::Withdraw => image = Some(Image::Withdraw),
                    Job::Confirm(covers) => owed = owed.max(covers),
                    Job::Retire(reply) => retire = Some(reply),
                }
            }
            if parent_lock
                .as_deref()
                .is_some_and(super::unix::clear_dead_lock)
            {
                parent_lock = None;
            }
            if let Some(reply) = retire {
                let _ = reply.send(owner.take().map_or(Ok(()), super::JournalOwner::retire));
                return;
            }
            match image {
                Some(Image::Publish(manifest, header)) => {
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
                    match owner
                        .as_ref()
                        .map(|owner| owner.publish(&manifest, &header))
                    {
                        Some(Ok(())) => {
                            on_disk = true;
                            inherited.store(false, Ordering::Release);
                            crate::restore::settle_take();
                        }
                        Some(Err(error)) => {
                            on_disk = false;
                            if !said {
                                said = true;
                                aterm_log::warn!("crash journal: not written ({error})");
                            }
                            durable.note_failed();
                        }
                        None => {
                            on_disk = false;
                            durable.note_failed();
                        }
                    }
                }
                Some(Image::Withdraw) => {
                    on_disk = false;
                    match owner.as_ref().map(super::JournalOwner::withdraw) {
                        Some(Ok(())) => inherited.store(false, Ordering::Release),
                        Some(Err(error)) => {
                            aterm_log::warn!("crash journal: not withdrawn ({error})");
                        }
                        None => {}
                    }
                }
                None => {}
            }
            if on_disk {
                durable.cover(owed);
            }
        }
    }

    #[cfg(not(unix))]
    fn run(
        _dir: &std::path::Path,
        _id: JournalId,
        rx: &mpsc::Receiver<Job>,
        _owner: Option<Owned>,
        _parent_lock: Option<PathBuf>,
        _inherited: &Inherited,
        _durable: &Durable,
    ) {
        while let Ok(job) = rx.recv() {
            if let Job::Retire(reply) = job {
                let _ = reply.send(Ok(()));
                return;
            }
        }
    }
}

/// Test layouts, on every platform.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::restore::{self, RestoreManifest};

    fn leaf(local: u64, cwd: Option<&str>, title: &str) -> restore::RestoredSplitTree {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                sid: None,
                lineage: None,
                log_ep: None,
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
                autopilot: None,
                land_at: None,
                autopilot_by: None,
                theme: None,
                agent: None,
                supervised: None,
                lost: None,
                held: false,
                holds: Vec::new(),
            },
        ))
    }

    pub(crate) fn layout(tabs: &[(&str, &str)]) -> RestoreManifest {
        RestoreManifest::new(vec![restore::WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            inner_px: None,
            presence_row: false,
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

    use aterm_spec::wait::MUST_HAPPEN;

    pub(crate) use super::fixtures::layout;
    use super::{
        BootClaim, DeathClass, Evidence, JournalHeader, JournalId, JournalOwner, Lane,
        MIN_WRITE_INTERVAL, Note, OwnerWords, PROBATION, PanesWithoutFolder, QuitLayout, Settled,
        TITLE_WRITE_INTERVAL, claim_at_boot, classify_death, decode, encode,
    };
    use crate::crash_signal::{MarkerOwner, markers};
    use crate::restore::{self, RestoreManifest};

    pub(crate) fn scratch(tag: &str) -> aterm_tempfile::TempDir {
        let root = crate::test_scratch::process_root("aterm-cj-");
        std::fs::create_dir_all(&root).expect("the journals' root");
        aterm_tempfile::Builder::new()
            .prefix(tag)
            .tempdir_in(&root)
            .expect("scratch dir")
    }

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

    pub(crate) fn reaped_pid() -> u32 {
        let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    }

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

    fn run(dir: &Path, logs: &Path, manifest: &RestoreManifest, probation: bool) -> JournalOwner {
        let id = JournalId::now(reaped_pid());
        marker(logs, id);
        let owner = JournalOwner::create(dir, id).expect("armed");
        owner
            .publish(manifest, &JournalHeader::now(id.pid, probation))
            .expect("published");
        owner
    }

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

    fn older_header(pid: u32) -> JournalHeader {
        JournalHeader {
            build: "0.98.0 (1790607299)".to_string(),
            stamps_quit: false,
            ..JournalHeader::now(pid, false)
        }
    }

    fn run_by_older_build(
        dir: &Path,
        logs: &Path,
        manifest: &RestoreManifest,
        at: Option<u64>,
    ) -> JournalOwner {
        let id = JournalId::now(reaped_pid());
        marker(logs, id);
        let owner = JournalOwner::create(dir, id).expect("armed");
        let mut header = older_header(id.pid);
        header.written_unix_ms = at.unwrap_or(header.written_unix_ms);
        owner.publish(manifest, &header).expect("published");
        owner
    }

    fn vim_in(window: u32, tab: u32) -> super::LeafProgram {
        super::LeafProgram {
            window,
            tab,
            program: "vim".to_string(),
            agent: false,
            again: false,
        }
    }

    #[test]
    fn every_pane_of_the_restored_tree_is_counted_split_and_legacy() {
        use restore::{PaneLayout, RestoredSplitTree, SplitKind};
        let reopened = |manifest: RestoreManifest| super::Reopened {
            manifest,
            class: DeathClass::Killed,
            sources: vec![JournalId { pid: 9, nanos: 9 }],
            writers: vec![9],
            handed: Vec::new(),
            programs: None,
            second_chance: false,
        };
        let gone = |dir: &str| dir.starts_with("/gone");
        let none = |_: &str| false;

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

    #[test]
    fn only_an_agent_with_a_conversation_is_resumed_and_only_when_relaunching() {
        let claude = |session: Option<&str>| restore::AgentRestore {
            word: String::new(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: session.map(str::to_string),
            cwd: "/work/b".into(),
            version: None,
            home: None,
            login: None,
            run: None,
            cause: None,
            tab: None,
        };
        let reopened = |agent: Option<restore::AgentRestore>| {
            let mut manifest = layout(&[("/work/a", "zsh"), ("/work/b", "claude")]);
            manifest.fill_agents(&|id| (id == 1).then(|| agent.clone()).flatten());
            super::Reopened {
                manifest,
                class: DeathClass::Killed,
                sources: Vec::new(),
                writers: Vec::new(),
                handed: Vec::new(),
                programs: None,
                second_chance: false,
            }
        };
        const CONVERSATION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
        let with = reopened(Some(claude(Some(CONVERSATION))));
        assert_eq!(with.resumed(true), vec![(0, 1)]);
        assert!(with.resumed(false).is_empty(), "the relaunch off");
        assert!(reopened(Some(claude(None))).resumed(true).is_empty());
        assert!(reopened(None).resumed(true).is_empty());
        use crate::message_reporters::journal_reopened_message;
        let agent_in = |tab: u32| super::LeafProgram {
            window: 0,
            tab,
            program: "claude".to_string(),
            agent: true,
            again: false,
        };
        let mut manifest = layout(&[("/work/a", "zsh"), ("/work/b", "claude")]);
        manifest.fill_agents(&|id| (id == 1).then(|| claude(Some(CONVERSATION))));
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        die(run_said(
            dir.path(),
            logs.path(),
            &manifest,
            Some(vec![agent_in(1)]),
        ));
        let only = claim_here(dir.path(), logs.path())
            .reopened
            .expect("reopened");
        let claude_only = |agent: &restore::AgentRestore| match agent.agent() {
            Some(aterm_agent::harness::upgrade::Agent::Claude) => true,
            Some(
                aterm_agent::harness::upgrade::Agent::Codex
                | aterm_agent::harness::upgrade::Agent::Agy
                | aterm_agent::harness::upgrade::Agent::Muse,
            )
            | None => false,
        };
        assert_eq!(only.relaunched_by(claude_only).resumed(true), vec![(0, 1)]);
        let switched_off = only.relaunched_by(|_| false);
        assert!(switched_off.resumed(true).is_empty());
        let row = journal_reopened_message(
            &switched_off,
            None,
            None,
            None,
            true,
            PanesWithoutFolder::NONE,
            0,
        );
        assert_eq!(row.title, "Tabs restored, claude lost");
        assert_eq!(only.resumed(true), vec![(0, 1)], "the layout itself kept");
        let codex = restore::AgentRestore {
            word: String::new(),
            pid: 4343,
            program: "/opt/codex/bin/codex".into(),
            argv: vec!["/opt/codex/bin/codex".into()],
            session: Some("019a0b1c-2d3e-7f40-8a5b-6c7d8e9f0a1b".into()),
            cwd: "/work/c".into(),
            version: None,
            run: Some(restore::RunRestore::Codex(restore::CodexRestore {
                home: "/h/.codex".into(),
                home_env: None,
                embedded: true,
                started_ms: 0,
                alab: Some(restore::AlabRestore {
                    argv: vec![
                        "/opt/codex/bin/codex".into(),
                        "resume".into(),
                        "019a0b1c-2d3e-7f40-8a5b-6c7d8e9f0a1b".into(),
                    ],
                    session: "019a0b1c-2d3e-7f40-8a5b-6c7d8e9f0a1b".into(),
                    codex_home: None,
                    codex_auth_path: None,
                    cwd: None,
                }),
                carried: None,
            })),
            ..claude(None)
        };
        let mut manifest = layout(&[
            ("/work/a", "zsh"),
            ("/work/b", "claude"),
            ("/work/c", "codex"),
        ]);
        manifest.fill_agents(&|id| match id {
            1 => Some(claude(Some(CONVERSATION))),
            2 => Some(codex.clone()),
            _ => None,
        });
        let codex_in = |tab: u32| super::LeafProgram {
            program: "codex".to_string(),
            ..agent_in(tab)
        };
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        die(run_said(
            dir.path(),
            logs.path(),
            &manifest,
            Some(vec![agent_in(1), codex_in(2)]),
        ));
        let mixed = claim_here(dir.path(), logs.path())
            .reopened
            .expect("reopened");
        assert_eq!(mixed.resumed(true), vec![(0, 1), (0, 2)], "both on");
        let mut vendor = codex.clone();
        if let Some(restore::RunRestore::Codex(run)) = vendor.run.as_mut() {
            run.alab = None;
        }
        assert!(codex.resumes() && !vendor.resumes());
        let told = mixed.relaunched_by(claude_only);
        assert_eq!(told.resumed(true), vec![(0, 1)]);
        let row =
            journal_reopened_message(&told, None, None, None, true, PanesWithoutFolder::NONE, 0);
        assert_eq!(row.title, "Tabs restored, codex lost");
    }

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

    #[test]
    fn a_planted_program_name_reads_as_unnamed() {
        let mut header = JournalHeader::now(1, false);
        header.programs = Some(vec![super::LeafProgram {
            window: 0,
            tab: 0,
            program: "s-1234 `rm -rf`".to_string(),
            agent: false,
            again: false,
        }]);
        let text = encode(&layout(&[("/a", "zsh")]), &header).unwrap();
        let (read, _) = decode(&text).unwrap();
        assert_eq!(read.programs.unwrap()[0].program, "");
    }

    pub(crate) fn die(owner: JournalOwner) {
        drop(owner);
    }

    pub(crate) fn await_released(lock: &Path) {
        aterm_spec::wait::until_within(
            MUST_HAPPEN,
            "the dead lane's journal lock released",
            || (markers::probe(lock) != markers::Owner::Live).then_some(()),
        );
    }

    fn claim_here(dir: &Path, logs: &Path) -> BootClaim {
        claim_at_boot(
            dir,
            &evidence(logs),
            std::process::id(),
            QuitLayout::default(),
        )
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
            "journal-4242-7.unreadable",
        ] {
            assert_eq!(JournalId::parse(stranger), None, "{stranger}");
        }
    }

    #[test]
    fn a_journal_is_a_session_toml_with_a_header() {
        let manifest = layout(&[("/work/a", "vim"), ("/work/b", "zsh")]);
        let marked = super::LeafProgram {
            window: 0,
            tab: 1,
            program: "claude".to_string(),
            agent: true,
            again: true,
        };
        for programs in [None, Some(Vec::new()), Some(vec![vim_in(0, 0), marked])] {
            let mut header = JournalHeader::now(7, true);
            header.programs = programs;
            let text = encode(&manifest, &header).unwrap();
            assert_eq!(RestoreManifest::from_toml(&text), Some(manifest.clone()));
            assert_eq!(decode(&text).unwrap(), (header, manifest.clone()));
        }
        assert!(
            decode(&RestoreManifest::new(Vec::new()).to_toml().unwrap()).is_err(),
            "a plain session.toml has no header and is no journal"
        );
    }

    #[test]
    fn a_journal_image_is_only_ordered_and_the_boot_claim_untouched() {
        use aterm_durable::{Flush, seam::Recording};
        let (dir, logs) = (scratch("cj-flush"), scratch("cj-flush-logs"));
        let id = JournalId::now(reaped_pid());
        marker(logs.path(), id);
        let owner = JournalOwner::create(dir.path(), id).expect("armed");
        let recording = Recording::start();
        owner
            .publish(
                &layout(&[("/work/a", "zsh")]),
                &JournalHeader::now(id.pid, false),
            )
            .expect("published");
        assert_eq!(
            recording.take(),
            [Flush::Ordered, Flush::Ordered],
            "the image and its entry"
        );
        die(owner);
        let claim = claim_here(dir.path(), logs.path());
        assert!(claim.reopened.is_some(), "the dead run's desk reopens");
        assert_eq!(
            recording.take(),
            [],
            "the claim's entries take the plain fsync(2), not an aterm-durable arm"
        );
    }

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
            claim_at_boot(dir.path(), &evidence(logs.path()), 1, QuitLayout::default()),
            BootClaim::default()
        );
    }

    #[test]
    fn a_dead_owners_lock_is_free_while_a_child_forked_under_it_has_yet_to_exec() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let owner = run(dir.path(), logs.path(), &layout(&[("/a", "zsh")]), false);
        let id = owner.id();
        let child = crate::parked_fork::ParkedChild::fork();
        die(owner);
        let claim = claim_here(dir.path(), logs.path());
        assert!(
            child.still_parked(),
            "the claim ran while the child had yet to exec"
        );
        child.release();
        assert_eq!(claim.taken, vec![id], "the dead run's journal is taken");
    }

    #[test]
    fn a_clean_end_is_never_reopened() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let owner = run(dir.path(), logs.path(), &layout(&[("/q", "zsh")]), false);
        owner.retire().unwrap();
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
        assert_eq!(claim_here(dir.path(), logs.path()), BootClaim::default());

        let owner = run(dir.path(), logs.path(), &layout(&[("/h", "zsh")]), false);
        let id = owner.id();
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

    #[test]
    fn an_unreadable_journal_is_said_and_never_followed() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let owner = run(dir.path(), logs.path(), &layout(&[("/torn", "zsh")]), false);
        let (id, path) = (owner.id(), owner.path());
        die(owner);
        let torn: &[u8] = b"schema = 2\n[[windows]\n";
        std::fs::write(&path, torn).unwrap();
        let older: Vec<String> = (1..=super::unix::KEEP_UNREADABLE as u128)
            .map(|nanos| JournalId { pid: 1, nanos }.unreadable_name())
            .collect();
        for name in &older {
            std::fs::write(dir.path().join(name), torn).unwrap();
        }
        let claim = claim_here(dir.path(), logs.path());
        assert_eq!(claim.reopened, None);
        let kept = match &claim.notes[..] {
            [
                Note::Unreadable {
                    id: noted,
                    class: DeathClass::Killed,
                    path: Some(path),
                    ..
                },
            ] if *noted == id => path.clone(),
            notes => panic!("{notes:?}"),
        };
        assert_eq!(
            std::fs::read(&kept).ok().as_deref(),
            Some(torn),
            "the row names the image, kept aside: {}",
            kept.display()
        );
        assert_eq!(
            std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut aside = older[1..].to_vec();
        aside.push(id.unreadable_name());
        aside.sort();
        let listed = |dir: &Path| -> Vec<String> {
            names(dir)
                .into_iter()
                .filter(|name| name.ends_with(".unreadable"))
                .collect()
        };
        assert_eq!(listed(dir.path()), aside);
        let again = claim_here(dir.path(), logs.path());
        assert!(
            again.taken.is_empty() && again.notes.is_empty(),
            "{again:?}"
        );
        assert_eq!(listed(dir.path()), aside, "no claim takes or sweeps one");

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
        assert!(matches!(
            &claim.notes[..],
            [Note::Unreadable { path: None, .. }]
        ));
        assert!(target.exists(), "the link's target is untouched");
        assert!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_type().unwrap().is_symlink()),
            "a link is never kept: {:?}",
            names(dir.path())
        );
        let theirs = secret.path().join("notes.txt");
        std::fs::write(&theirs, b"token = \"their-private-line\n").unwrap();
        std::fs::set_permissions(&theirs, std::fs::Permissions::from_mode(0o644)).unwrap();
        let owner = run(dir.path(), logs.path(), &layout(&[("/y", "zsh")]), false);
        let path = owner.path();
        die(owner);
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&theirs, &path).unwrap();
        let claim = claim_here(dir.path(), logs.path());
        let [
            Note::Unreadable {
                path: None, error, ..
            },
        ] = &claim.notes[..]
        else {
            panic!("{:?}", claim.notes);
        };
        assert!(
            !error.contains("their-private-line"),
            "the row quotes the linked file: {error}"
        );
        assert_eq!(
            std::fs::metadata(&theirs).unwrap().permissions().mode() & 0o777,
            0o644,
            "the other name's file is untouched"
        );
        assert_eq!(listed(dir.path()), aside, "a hard link is never kept");
    }

    #[test]
    fn a_relapse_inside_probation_is_skipped_and_said() {
        use super::LeafProgram;
        let python = || {
            Some(vec![LeafProgram {
                window: 0,
                tab: 1,
                program: "Python".to_string(),
                agent: false,
                again: false,
            }])
        };
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

        let (claim, _dir, _logs) = relapse(false, b"");
        let reopened = claim
            .reopened
            .expect("a kill is no evidence the layout stops aterm");
        assert!(reopened.second_chance, "{reopened:?}");
        assert!(claim.notes.is_empty(), "{:?}", claim.notes);

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

    #[test]
    fn the_header_marks_are_additive() {
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

        assert!(plain.contains("stamps_quit = true"), "{plain}");
        assert!(decode(&plain).expect("decoded").0.stamps_quit);
        let theirs = encode(&layout(&[("/a", "zsh")]), &older_header(7)).expect("encoded");
        assert!(!theirs.contains("stamps_quit"), "{theirs}");
        assert!(!decode(&theirs).expect("decoded").0.stamps_quit);
    }

    #[test]
    fn a_quit_layout_does_not_set_a_crashed_desk_aside() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let desk: Vec<RestoreManifest> = (0..5)
            .map(|i| layout(&[(format!("/w{i}").as_str(), "zsh")]))
            .collect();
        let desk = RestoreManifest::new(desk.into_iter().flat_map(|m| m.windows).collect());
        let crashed = run(dir.path(), logs.path(), &desk, false);
        let crashed_id = crashed.id();
        die(crashed);
        let claim = claim_at_boot(
            dir.path(),
            &evidence(logs.path()),
            1,
            QuitLayout::of(Some(&layout(&[("/quit", "zsh")])), None, None),
        );
        let reopened = claim.reopened.expect("the crashed desk comes back");
        assert_eq!(reopened.sources, vec![crashed_id]);
        assert_eq!(
            cwds(&reopened.manifest),
            vec!["/w0", "/w1", "/w2", "/w3", "/w4"]
        );
        assert!(names(dir.path()).is_empty());
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        die(run(
            dir.path(),
            logs.path(),
            &layout(&[("/a", "zsh")]),
            false,
        ));
        let claim = claim_at_boot(
            dir.path(),
            &evidence(logs.path()),
            1,
            QuitLayout {
                windows: crate::restore::MAX_WINDOWS,
                run: None,
                also: Vec::new(),
                shape: vec![2; crate::restore::MAX_WINDOWS],
                written_unix_ms: None,
            },
        );
        assert_eq!(claim.reopened, None);
        assert_eq!(claim.set_aside.len(), 1, "{:?}", claim.set_aside);
    }

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
        assert_ne!(newer_id.pid, older_id.pid);
        assert_eq!(reopened.writers, vec![newer_id.pid, older_id.pid]);
    }

    #[test]
    fn the_death_class_is_read_from_the_runs_own_evidence() {
        let logs = scratch("cj-logs");
        let id = JournalId::now(reaped_pid());
        let none = Evidence {
            log_dir: Some(logs.path()),
            swept: &[],
        };
        assert_eq!(
            classify_death(&none, id, None),
            None,
            "a clean exit leaves nothing"
        );

        let later = logs
            .path()
            .join(markers::file_name(id.pid, id.nanos + 1, MarkerOwner::App));
        std::fs::write(&later, b"").unwrap();
        assert_eq!(
            classify_death(&none, id, None),
            None,
            "a later process's marker"
        );
        std::fs::remove_file(&later).unwrap();

        let own = marker(logs.path(), id);
        assert_eq!(classify_death(&none, id, None), Some(DeathClass::Killed));
        std::fs::write(&own, b"aterm: fatal signal 11").unwrap();
        assert_eq!(classify_death(&none, id, None), Some(DeathClass::Signal));
        std::fs::remove_file(&own).unwrap();

        file_clock_catches_up(logs.path());
        let report = logs.path().join(format!("crash-{}.log", id.pid));
        std::fs::write(&report, b"aterm-gui crashed").unwrap();
        assert_eq!(classify_death(&none, id, None), Some(DeathClass::Panic));
        let before = JournalId {
            pid: id.pid,
            nanos: super::unix_nanos(std::time::SystemTime::now()) + 60_000_000_000,
        };
        assert_eq!(
            classify_death(&none, before, None),
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
        assert_eq!(
            classify_death(&with_sweep, id, None),
            Some(DeathClass::Killed)
        );
    }

    fn file_clock_catches_up(dir: &Path) {
        let now = std::time::SystemTime::now();
        let probe = dir.join(".file-clock-probe");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            std::fs::write(&probe, b"").unwrap();
            if std::fs::metadata(&probe).unwrap().modified().unwrap() >= now {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the file clock did not reach the wall clock in a minute"
            );
            std::thread::yield_now();
        }
        std::fs::remove_file(&probe).unwrap();
    }

    fn panic_report(logs: &Path, pid: u32, main_thread: bool) -> PathBuf {
        file_clock_catches_up(logs);
        let path = logs.join(format!("crash-{pid}.log"));
        let mark = if main_thread {
            crate::logging::MAIN_THREAD_PANIC
        } else {
            ""
        };
        std::fs::write(
            &path,
            format!("aterm-gui 0.1.0 crashed at unix 1.000{mark}\npanicked at x\n"),
        )
        .unwrap();
        path
    }

    #[test]
    fn a_main_thread_panic_whose_report_another_launch_read_first_still_reopens() {
        for main_thread in [true, false] {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            let id = JournalId::now(reaped_pid());
            let owner = JournalOwner::create(dir.path(), id).expect("armed");
            owner
                .publish(
                    &layout(&[("/a", "zsh"), ("/b", "claude")]),
                    &JournalHeader::now(id.pid, false),
                )
                .expect("published");
            die(owner);
            panic_report(logs.path(), id.pid, main_thread);
            assert!(
                crate::logging::take_crash_evidence_in(logs.path(), None).is_some(),
                "another launch reads the report first"
            );
            assert!(
                logs.path()
                    .join(format!("crash-{}.log.seen", id.pid))
                    .exists(),
                "PRECONDITION: consumed"
            );
            let claim = claim_here(dir.path(), logs.path());
            if main_thread {
                let reopened = claim.reopened.expect("the crashed desk comes back");
                assert_eq!(reopened.class, DeathClass::Panic);
                assert_eq!(cwds(&reopened.manifest), vec!["/a", "/b"]);
            } else {
                assert_eq!(claim.reopened, None, "a worker's panic says nothing");
                assert_eq!(claim.set_aside.len(), 1, "{:?}", claim.set_aside);
            }
        }
    }

    #[test]
    fn a_handed_off_journal_is_not_reopened_by_a_later_run_of_the_same_pid() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let pid = reaped_pid();
        let stale = JournalId::now(pid);
        let owner = JournalOwner::create(dir.path(), stale).expect("armed");
        owner
            .publish(
                &layout(&[("/stale", "zsh")]),
                &JournalHeader::now(pid, false),
            )
            .expect("published");
        die(owner); // the hand-off: clean, the marker removed, the image left
        let mut live = JournalId::now(pid);
        while live.nanos <= stale.nanos {
            live = JournalId::now(pid);
        }
        let owner = JournalOwner::create(dir.path(), live).expect("armed");
        owner
            .publish(
                &layout(&[("/live", "zsh")]),
                &JournalHeader::now(pid, false),
            )
            .expect("published");
        die(owner);
        panic_report(logs.path(), pid, true);
        let claim = claim_here(dir.path(), logs.path());
        let reopened = claim.reopened.expect("the crashed run comes back");
        assert_eq!(reopened.class, DeathClass::Panic);
        assert_eq!(reopened.sources, vec![live]);
        assert_eq!(cwds(&reopened.manifest), vec!["/live"]);
        assert!(
            claim.set_aside.iter().any(|(id, _)| *id == stale),
            "{:?}",
            claim.set_aside
        );
        let logs = scratch("cj-logs");
        let id = JournalId::now(reaped_pid());
        let later = logs
            .path()
            .join(markers::file_name(id.pid, id.nanos + 1, MarkerOwner::App));
        std::fs::write(&later, b"").unwrap();
        panic_report(logs.path(), id.pid, true);
        assert_eq!(
            classify_death(&evidence(logs.path()), id, None),
            None,
            "a report written after a later run of the pid started is that run's"
        );
    }

    #[test]
    fn a_later_run_swept_at_install_bounds_a_pid_only_report() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let stale = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir.path(), stale).expect("armed");
        owner
            .publish(
                &layout(&[("/stale", "zsh")]),
                &JournalHeader::now(stale.pid, false),
            )
            .expect("published");
        die(owner); // the hand-off: clean, the marker removed, the image left
        panic_report(logs.path(), stale.pid, false);
        let swept = [markers::MarkerName {
            pid: stale.pid,
            nanos: stale.nanos + 1,
            owner: MarkerOwner::Other,
        }];
        let claim = claim_at_boot(
            dir.path(),
            &Evidence {
                log_dir: Some(logs.path()),
                swept: &swept,
            },
            std::process::id(),
            QuitLayout::default(),
        );
        assert_eq!(claim.reopened, None, "the later run's report is its own");
        assert_eq!(
            claim
                .set_aside
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![stale]
        );
        assert_eq!(
            classify_death(&evidence(logs.path()), stale, None),
            Some(DeathClass::Panic)
        );
    }

    #[test]
    fn a_report_that_names_its_run_is_that_runs_only() {
        for (named, reopens) in [(false, false), (true, true)] {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            let stale = JournalId::now(reaped_pid());
            let owner = JournalOwner::create(dir.path(), stale).expect("armed");
            owner
                .publish(
                    &layout(&[("/stale", "zsh")]),
                    &JournalHeader::now(stale.pid, false),
                )
                .expect("published");
            die(owner);
            let run = if named { stale.nanos } else { stale.nanos + 1 };
            crate::logging::write_crash_report(
                &logs.path().join(format!("crash-{}.log", stale.pid)),
                true,
                Some(run),
                &"panicked at x",
                &"bt",
            )
            .expect("the report");
            assert!(
                crate::logging::take_crash_evidence_in(logs.path(), None).is_some(),
                "another launch reads the report first"
            );
            let claim = claim_here(dir.path(), logs.path());
            assert_eq!(
                claim.reopened.as_ref().map(|reopened| reopened.class),
                reopens.then_some(DeathClass::Panic),
                "a report of run {run} for the journal of run {}",
                stale.nanos
            );
        }
    }

    #[test]
    fn a_successor_crash_before_its_first_image_reopens_the_predecessors_layout() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let parent = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir.path(), parent).expect("armed");
        owner
            .publish(
                &layout(&[("/a", "zsh"), ("/b", "claude")]),
                &JournalHeader::now(parent.pid, false),
            )
            .expect("published");
        die(owner); // the hand-off: `clean_exit_now` removed its marker
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(parent.pid, None);
        let own = lane.journal_id();
        marker(logs.path(), own); // the successor's run, armed at its start
        assert_eq!(
            lane.adopt_predecessor(true),
            Some(parent),
            "taken at Commit"
        );
        assert_eq!(lane.adopt_predecessor(true), None, "once");
        assert_eq!(
            names(dir.path()),
            vec![own.lock_name(), own.file_name()],
            "the parent's journal is this run's now, and its dead lock is gone"
        );
        drop(lane);
        await_released(&dir.path().join(own.lock_name()));
        let claim = claim_here(dir.path(), logs.path());
        let reopened = claim.reopened.expect("the desk comes back");
        assert_eq!(reopened.class, DeathClass::Killed);
        assert_eq!(reopened.sources, vec![own]);
        assert_eq!(cwds(&reopened.manifest), vec!["/a", "/b"]);
    }

    #[test]
    fn a_committed_successor_that_dies_before_its_commit_wake_still_reopens_the_desk() {
        use std::sync::atomic::AtomicBool;
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let parent = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir.path(), parent).expect("armed");
        owner
            .publish(
                &layout(&[("/a", "zsh"), ("/b", "claude")]),
                &JournalHeader::now(parent.pid, false),
            )
            .expect("published");
        die(owner); // the hand-off: `clean_exit_now` removed its marker
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        let own = lane.journal_id();
        marker(logs.path(), own); // the successor's run, armed at its start
        let journal = lane.take_over_at_commit(true);
        assert!(
            journal.is_some(),
            "a successor's lane prepares its take-over"
        );
        let gate = crate::spawn::DeferredReaderGate::closed();
        let reader = {
            let gate = gate.clone();
            let image = dir.path().join(own.file_name());
            std::thread::spawn(move || {
                assert!(gate.wait_until_released(&AtomicBool::new(false)));
                image.exists()
            })
        };
        crate::release_committed_readers(journal, &gate);
        assert!(
            reader.join().expect("the reader"),
            "a released reader runs only once the journal is this run's"
        );
        drop(lane);
        await_released(&dir.path().join(own.lock_name()));
        let claim = claim_here(dir.path(), logs.path());
        let reopened = claim.reopened.expect("the desk comes back");
        assert_eq!(reopened.class, DeathClass::Killed);
        assert_eq!(reopened.sources, vec![own]);
        assert_eq!(cwds(&reopened.manifest), vec!["/a", "/b"]);
        assert!(claim.set_aside.is_empty(), "{:?}", claim.set_aside);
    }

    #[test]
    fn the_commit_wake_installs_the_take_over_its_waiter_ran() {
        for waiter in [true, false] {
            let dir = scratch("cj-dir");
            let parent = JournalId::now(reaped_pid());
            let owner = JournalOwner::create(dir.path(), parent).expect("armed");
            owner
                .publish(
                    &layout(&[("/a", "zsh")]),
                    &JournalHeader::now(parent.pid, false),
                )
                .expect("published");
            die(owner);
            let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
            lane.set_predecessor(parent.pid, Some(parent.nanos));
            let own = lane.journal_id();
            if waiter {
                lane.take_over_at_commit(true).expect("prepared").run(|| {});
                assert_eq!(
                    names(dir.path()),
                    vec![own.lock_name(), own.file_name()],
                    "the waiter took it over"
                );
            }
            assert_eq!(
                lane.adopt_predecessor(true),
                Some(parent),
                "waiter={waiter}"
            );
            assert_eq!(lane.adopt_predecessor(true), None, "once");
            assert_eq!(
                names(dir.path()),
                vec![own.lock_name(), own.file_name()],
                "waiter={waiter}"
            );
            assert_eq!(
                markers::probe(&dir.path().join(own.lock_name())),
                markers::Owner::Live,
                "waiter={waiter}: the writer holds this run's journal"
            );
            lane.retire(MUST_HAPPEN).expect("retired");
            assert!(names(dir.path()).is_empty(), "waiter={waiter}");
        }
    }

    fn a_parents_journal(dir: &Path, desk: &RestoreManifest) -> (JournalId, JournalOwner) {
        let parent = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir, parent).expect("armed");
        owner
            .publish(desk, &JournalHeader::now(parent.pid, false))
            .expect("published");
        (parent, owner)
    }

    #[test]
    fn the_waiter_releases_the_readers_once_the_rename_stands() {
        let dir = scratch("cj-dir");
        let (parent, owner) = a_parents_journal(dir.path(), &layout(&[("/a", "zsh")]));
        die(owner);
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        let own = lane.journal_id();
        let journal = lane.take_over_at_commit(true).expect("prepared");
        let slot = journal.slot.clone();
        let held = slot
            .taken_over
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let gate = crate::spawn::DeferredReaderGate::closed();
        let waiter = {
            let gate = gate.clone();
            std::thread::spawn(move || crate::release_committed_readers(Some(journal), &gate))
        };
        aterm_spec::wait::until_within(
            MUST_HAPPEN,
            "the readers released while the take-over's result waits",
            || gate.is_released().then_some(()),
        );
        assert!(
            dir.path().join(own.file_name()).exists(),
            "the rename came first"
        );
        drop(held);
        waiter.join().expect("the waiter");
        assert_eq!(lane.adopt_predecessor(true), Some(parent));
        lane.retire(MUST_HAPPEN).expect("retired");
        assert!(names(dir.path()).is_empty());

        let dir = scratch("cj-dir");
        let (parent, owner) = a_parents_journal(dir.path(), &layout(&[("/a", "zsh")]));
        die(owner);
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        let own = lane.journal_id();
        let mut seen = None;
        lane.take_over_at_commit(true)
            .expect("prepared")
            .run(|| seen = Some(names(dir.path())));
        let mut expected = vec![parent.lock_name(), own.lock_name(), own.file_name()];
        expected.sort();
        assert_eq!(seen, Some(expected), "renamed, its parent's lock not swept");
        assert_eq!(names(dir.path()), vec![own.lock_name(), own.file_name()]);
    }

    #[test]
    fn the_parents_lock_goes_once_its_exit_lets_it_go() {
        for first_image in [true, false] {
            let dir = scratch("cj-dir");
            let (parent, owner) = a_parents_journal(dir.path(), &layout(&[("/a", "zsh")]));
            let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
            lane.set_predecessor(parent.pid, Some(parent.nanos));
            let own = lane.journal_id();
            lane.take_over_at_commit(true).expect("prepared").run(|| {});
            let mut held = vec![parent.lock_name(), own.lock_name(), own.file_name()];
            held.sort();
            assert_eq!(names(dir.path()), held, "a lock still held stays");
            die(owner); // its `_exit` completes
            assert_eq!(lane.adopt_predecessor(true), Some(parent));
            if first_image {
                assert!(lane.offer(layout(&[("/b", "zsh")]), Vec::new(), Instant::now()));
                let settled = vec![own.lock_name(), own.file_name()];
                aterm_spec::wait::until_within(
                    MUST_HAPPEN,
                    "the writer clears the parent's lock",
                    || (names(dir.path()) == settled).then_some(()),
                );
            }
            lane.retire(MUST_HAPPEN).expect("retired");
            assert!(
                names(dir.path()).is_empty(),
                "first_image={first_image}: {:?}",
                names(dir.path())
            );
        }
    }

    #[test]
    fn the_taken_over_image_is_named_until_the_writer_replaces_it() {
        let dir = scratch("cj-dir");
        let (parent, owner) = a_parents_journal(dir.path(), &layout(&[("/a", "zsh")]));
        die(owner);
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        assert_eq!(
            lane.inherited_image_parent(),
            None,
            "nothing taken over yet"
        );
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        assert_eq!(lane.adopt_predecessor(true), Some(parent));
        assert_eq!(lane.inherited_image_parent(), Some(parent.pid));
        assert!(lane.offer(layout(&[("/b", "zsh")]), Vec::new(), Instant::now()));
        aterm_spec::wait::until_within(
            MUST_HAPPEN,
            "the writer replaces the parent's image",
            || lane.inherited_image_parent().is_none().then_some(()),
        );
        lane.retire(MUST_HAPPEN).expect("retired");

        let fresh = scratch("cj-fresh");
        let fresh = Lane::new(Some(fresh.path().to_path_buf()), reaped_pid());
        assert_eq!(fresh.inherited_image_parent(), None, "no successor");
    }

    #[test]
    fn a_successors_own_fact_about_a_tab_ends_what_it_carries_for_good() {
        use super::{HandedSession, Runs};
        let record = |session: &str| restore::AgentRestore {
            word: String::new(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some(session.into()),
            cwd: "/b".into(),
            version: Some("2.1.283".into()),
            home: None,
            login: None,
            run: None,
            cause: None,
            tab: None,
        };
        let successor = || {
            let dir = scratch("cj-dir");
            let mut desk = layout(&[("/a", "zsh"), ("/b", "claude")]);
            desk.fill_agents(&|id| (id == 1).then(|| record("C-parent")));
            let (parent, owner) = a_parents_journal(dir.path(), &desk);
            die(owner);
            let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
            lane.set_predecessor(parent.pid, Some(parent.nanos));
            assert_eq!(lane.adopt_predecessor(true), Some(parent));
            (dir, lane)
        };
        let capture = |lane: &mut Lane, host: Option<&str>, runs: Runs, busy: bool| {
            let mut manifest = layout(&[("/own-b", "claude"), ("/own-a", "zsh")]);
            if busy {
                let restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Terminal(leaf),
                } = &mut manifest.windows[0].restored_tabs[0].root
                else {
                    unreachable!("a terminal leaf")
                };
                leaf.cwd = None;
                leaf.title.clear();
            }
            manifest.fill_agents(&|id| (id == 0).then_some(host).flatten().map(record));
            lane.carry_forward(&mut manifest, &|id| {
                (id <= 1).then_some(HandedSession {
                    was: 1 - id,
                    runs: if id == 0 { runs } else { Runs::Other },
                    keyed: None,
                })
            });
            let restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } = &manifest.windows[0].restored_tabs[0].root
            else {
                unreachable!("a terminal leaf")
            };
            let seen = (
                leaf.agent.as_ref().and_then(|agent| agent.session.clone()),
                leaf.cwd.clone(),
            );
            lane.offer(manifest, Vec::new(), Instant::now());
            seen
        };
        let claude = Runs::Agent(aterm_phase::Program::Claude);
        for (fact, host, runs) in [
            ("its host's record", Some("D-own"), claude),
            ("the shell at its prompt", None, Runs::Other),
        ] {
            let (_dir, mut lane) = successor();
            let first = capture(&mut lane, host, runs, false);
            assert_eq!(first.0, host.map(str::to_string), "{fact}: this run's own");
            assert_eq!(
                capture(&mut lane, None, claude, false).0,
                None,
                "{fact}: the parent's conversation is not named again"
            );
        }

        let (_dir, mut lane) = successor();
        for runs in [Runs::Unread, claude] {
            assert_eq!(
                capture(&mut lane, None, runs, false).0.as_deref(),
                Some("C-parent"),
                "{runs:?}: the parent's record stands"
            );
        }

        let (_dir, mut lane) = successor();
        assert_eq!(
            capture(&mut lane, None, Runs::Unread, true).1.as_deref(),
            Some("/b"),
            "a busy first capture keeps the parent's folder"
        );
        lane.withdraw();
        assert_eq!(
            capture(&mut lane, None, Runs::Unread, true).1,
            None,
            "after a withdraw, not the parent's folder"
        );
    }

    #[test]
    fn a_parents_leaf_of_an_agent_with_no_lane_is_no_agent_a_status_read_names() {
        use super::Runs;
        use aterm_phase::Program;
        let record = |word: &str| restore::AgentRestore {
            word: word.into(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: format!("/opt/{word}/bin/{word}"),
            argv: vec![format!("/opt/{word}/bin/{word}")],
            session: Some("C-parent".into()),
            cwd: "/b".into(),
            version: None,
            home: None,
            login: None,
            run: None,
            cause: None,
            tab: None,
        };
        {
            let word = "opencode";
            let leaf = record(word);
            assert!(leaf.agent().is_none(), "{word}: no agent this build knows");
            for runs in [
                Runs::Agent(Program::Agy),
                Runs::Agent(Program::Claude),
                Runs::Other,
            ] {
                assert!(!runs.may_be(&leaf), "{word} under {runs:?}");
            }
            assert!(Runs::Unread.may_be(&leaf), "{word}: unread, it stands");
        }
        let codex = restore::AgentRestore {
            run: Some(restore::RunRestore::Codex(restore::CodexRestore {
                home: "/h/.codex".into(),
                home_env: None,
                embedded: false,
                started_ms: 0,
                alab: None,
                carried: None,
            })),
            ..record("codex")
        };
        assert!(Runs::Agent(Program::Codex).may_be(&codex));
        assert!(Runs::Agent(Program::Claude).may_be(&record("")));
        let muse = restore::AgentRestore {
            session: None,
            run: Some(restore::RunRestore::Muse(restore::MuseRestore {
                session: Some("01a148f8-9301-7752-96db-fef456f88074".into()),
                workspace: Some("/b".into()),
                data: "/h/.local/share".into(),
                env: Vec::new(),
            })),
            ..record("muse")
        };
        assert!(Runs::Agent(Program::Muse).may_be(&muse));
        assert!(!Runs::Agent(Program::Claude).may_be(&muse));
        assert_eq!(record("muse").agent(), None);
        assert!(!Runs::Agent(Program::Muse).may_be(&record("muse")));
        let agy = restore::AgentRestore {
            session: None,
            run: Some(restore::RunRestore::Agy(restore::AgyRestore {
                conversation: Some("eaeec4fb-4e43-47da-b46f-8a10c0fee186".into()),
                config: "/h/.gemini/antigravity-cli".into(),
                env: Vec::new(),
            })),
            ..record("agy")
        };
        assert!(Runs::Agent(Program::Agy).may_be(&agy));
        assert!(!Runs::Agent(Program::Claude).may_be(&agy));
        assert_eq!(record("agy").agent(), None);
        assert!(!Runs::Agent(Program::Agy).may_be(&record("agy")));
        let gemini = record("gemini");
        assert!(gemini.retired(), "the retired Gemini CLI's leaf");
        assert_eq!(gemini.agent(), None, "is no agent this build hosts");
        for runs in [
            Runs::Agent(Program::Agy),
            Runs::Agent(Program::Claude),
            Runs::Other,
        ] {
            assert!(!runs.may_be(&gemini), "gemini under {runs:?}");
        }
        assert!(Runs::Unread.may_be(&gemini), "gemini: unread, it stands");
    }

    #[test]
    fn a_take_over_never_takes_an_older_run_of_the_parents_pid() {
        let setup = || {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            let pid = reaped_pid();
            let older = JournalId::now(pid);
            marker(logs.path(), older); // its crash evidence: a kill
            let owner = JournalOwner::create(dir.path(), older).expect("armed");
            owner
                .publish(
                    &layout(&[("/older", "zsh")]),
                    &JournalHeader::now(pid, false),
                )
                .expect("published");
            die(owner);
            (dir, logs, older)
        };
        let (dir, logs, older) = setup();
        let born = older.nanos + 10_000_000_000;
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(older.pid, Some(born));
        let own = lane.journal_id();
        assert_eq!(lane.adopt_predecessor(true), None, "not the parent run's");
        drop(lane);
        await_released(&dir.path().join(own.lock_name()));
        let claim = claim_here(dir.path(), logs.path());
        let reopened = claim.reopened.expect("the older desk is still offered");
        assert_eq!(reopened.sources, vec![older]);
        assert_eq!(cwds(&reopened.manifest), vec!["/older"]);

        let (dir, _logs, older) = setup();
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        lane.set_predecessor(older.pid, None);
        assert_eq!(
            lane.adopt_predecessor(true),
            Some(older),
            "the pid alone takes the older desk"
        );
    }

    #[test]
    fn a_taken_over_image_names_the_parent_as_its_leaves_writer() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let parent = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir.path(), parent).expect("armed");
        owner
            .publish(
                &layout(&[("/a", "zsh"), ("/b", "zsh")]),
                &JournalHeader::now(parent.pid, false),
            )
            .expect("published");
        die(owner);
        let successor = reaped_pid();
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), successor);
        lane.set_predecessor(parent.pid, Some(parent.nanos));
        let own = lane.journal_id();
        marker(logs.path(), own);
        assert_eq!(lane.adopt_predecessor(true), Some(parent));
        drop(lane);
        await_released(&dir.path().join(own.lock_name()));
        let reopened = claim_here(dir.path(), logs.path())
            .reopened
            .expect("the desk comes back");
        assert_eq!(reopened.sources, vec![own], "judged as the successor's run");
        assert_eq!(
            reopened.writers,
            vec![parent.pid],
            "its leaves are the parent's"
        );
        assert_eq!(
            JournalHeader {
                pid: 0,
                ..JournalHeader::now(1, false)
            }
            .image_writer(own),
            own.pid
        );
    }

    #[test]
    fn a_quit_cut_short_before_its_journal_went_opens_its_desk_once() {
        let setup = |stamped: bool| {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            let desk = layout(&[("/desk", "zsh"), ("/agent", "claude")]);
            let quitting = if stamped {
                run(dir.path(), logs.path(), &desk, false)
            } else {
                run_by_older_build(dir.path(), logs.path(), &desk, None)
            };
            let id = quitting.id();
            let session = dir.path().join("session.toml");
            let stem = id.stem();
            restore::write_quit_to(&session, &desk, stamped.then_some(stem.as_str()))
                .expect("the quit's layout");
            die(quitting); // killed between the layout write and the retire
            let other = run(
                dir.path(),
                logs.path(),
                &layout(&[("/other", "zsh")]),
                false,
            );
            let other_id = other.id();
            die(other);
            let taken = restore::take_quit_from(&session).expect("the quit layout");
            let quit = QuitLayout::of(
                Some(&taken.manifest),
                taken.run.as_deref().and_then(JournalId::parse_stem),
                taken.written_unix_ms,
            );
            assert!(quit.written_unix_ms.is_some(), "the layout is dated");
            let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, quit.clone());
            (claim, quit, id, other_id)
        };
        let (claim, quit, id, other) = setup(true);
        assert_eq!(quit.run, Some(id), "the quit names its run");
        let reopened = claim.reopened.expect("the other desk comes back");
        assert_eq!(
            reopened.sources,
            vec![other],
            "the quit's own desk opens once"
        );
        assert_eq!(cwds(&reopened.manifest), vec!["/other"]);
        assert!(claim.set_aside.iter().any(|(set, _)| *set == id));
        assert!(claim.taken.contains(&id), "and it is gone, single-use");

        let (claim, quit, id, other) = setup(false);
        assert_eq!(quit.run, None);
        assert_eq!(quit.shape, vec![2]);
        let reopened = claim.reopened.expect("the other desk comes back");
        assert_eq!(
            reopened.sources,
            vec![other],
            "an older build's quit, cut short, opens its desk once too"
        );
        assert!(claim.set_aside.iter().any(|(set, _)| *set == id));
        assert!(claim.taken.contains(&id), "and it is gone, single-use");

        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let quitting = run_by_older_build(
            dir.path(),
            logs.path(),
            &layout(&[("/desk", "zsh"), ("/agent", "claude")]),
            None,
        );
        let id = quitting.id();
        die(quitting);
        let blind = QuitLayout {
            windows: 1,
            run: None,
            also: Vec::new(),
            shape: vec![3],
            written_unix_ms: None,
        };
        let reopened = claim_at_boot(dir.path(), &evidence(logs.path()), 1, blind)
            .reopened
            .expect("reopened");
        assert_eq!(reopened.sources, vec![id], "the doubled desk");

        let newer_desk = |dated: bool| {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            let desk = layout(&[("/desk", "zsh"), ("/agent", "claude")]);
            let quit_at = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
            let quit_ms = u64::try_from(
                quit_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("after the epoch")
                    .as_millis(),
            )
            .expect("fits");
            let owner = run_by_older_build(dir.path(), logs.path(), &desk, Some(quit_ms + 900));
            let quitting = owner.id();
            let session = dir.path().join("session.toml");
            restore::write_quit_to(&session, &desk, None).expect("the quit's layout");
            std::fs::File::options()
                .write(true)
                .open(&session)
                .and_then(|file| file.set_modified(quit_at))
                .expect("dated hours ago");
            die(owner);
            let crashed = run_by_older_build(
                dir.path(),
                logs.path(),
                &layout(&[("/work", "zsh"), ("/work", "claude")]),
                None,
            );
            let crashed_id = crashed.id();
            die(crashed);
            let taken = restore::take_quit_from(&session).expect("the quit layout");
            assert_eq!(taken.run, None, "an older build's quit names no run");
            assert_eq!(
                taken.written_unix_ms.map(|ms| ms / 1000),
                Some(quit_ms / 1000),
                "dated by its file"
            );
            let quit = QuitLayout::of(
                Some(&taken.manifest),
                None,
                taken.written_unix_ms.filter(|_| dated),
            );
            let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, quit);
            (claim, quitting, crashed_id)
        };
        let (claim, quitting, crashed) = newer_desk(true);
        let reopened = claim.reopened.expect("the newer desk comes back");
        assert_eq!(reopened.sources, vec![crashed], "behind the quit, once");
        assert_eq!(cwds(&reopened.manifest), vec!["/work", "/work"]);
        assert!(
            claim.set_aside.iter().any(|(set, _)| *set == quitting),
            "the quit's own journal is still set aside: {:?}",
            claim.set_aside
        );

        let (claim, quitting, crashed) = newer_desk(false);
        assert_eq!(claim.reopened, None, "the control: the newer desk is lost");
        for id in [quitting, crashed] {
            assert!(claim.set_aside.iter().any(|(set, _)| *set == id));
        }
    }

    #[test]
    fn a_quit_beside_another_instances_quit_keeps_both_desks_once() {
        let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
        let session = dir.path().join("session.toml");
        let a_desk = RestoreManifest::new(
            layout(&[("/a/one", "zsh")])
                .windows
                .into_iter()
                .chain(layout(&[("/a/two", "zsh")]).windows)
                .collect(),
        );
        let b_desk = layout(&[("/b/one", "zsh")]);
        let a = run(dir.path(), logs.path(), &a_desk, false);
        let b = run(dir.path(), logs.path(), &b_desk, false);
        let (a_id, b_id) = (a.id(), b.id());
        restore::write_quit_to(&session, &a_desk, Some(&a_id.stem())).expect("A's layout");
        die(a);
        restore::write_quit_to(&session, &b_desk, Some(&b_id.stem())).expect("B's update");
        restore::unwrite_quit_to(&session, Some(&b_id.stem())).expect("taken back");
        let waiting = std::fs::read_to_string(&session).expect("A's desk waits");
        assert_eq!(restore::quit_run_of(&waiting), Some(a_id.stem()));
        restore::write_quit_to(&session, &b_desk, Some(&b_id.stem())).expect("B's layout");
        b.retire().expect("B retired");

        let taken = restore::take_quit_from(&session).expect("the quit layout");
        assert_eq!(
            cwds(&taken.manifest),
            ["/b/one", "/a/one", "/a/two"],
            "both desks, the later quit's first"
        );
        let quit = QuitLayout::of(
            Some(&taken.manifest),
            taken.run.as_deref().and_then(JournalId::parse_stem),
            taken.written_unix_ms,
        )
        .holding(&taken.also);
        let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, quit);
        assert_eq!(claim.taken, vec![a_id]);
        assert_eq!(
            claim.reopened, None,
            "A's journal is its quit's, whose desk is in the layout: never a second A"
        );

        restore::write_quit_to(&session, &a_desk, None).expect("unstamped");
        restore::write_quit_to(&session, &b_desk, None).expect("unstamped");
        assert_eq!(
            cwds(&restore::take_from(&session).expect("the layout")),
            ["/b/one"]
        );
    }

    #[test]
    fn a_desk_whose_build_stamps_its_quits_is_never_an_older_quits_own() {
        let now_ms = || {
            u64::try_from(
                aterm_time::since_epoch()
                    .expect("after the epoch")
                    .as_millis(),
            )
            .expect("fits")
        };
        let scenario = |marked: bool| {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            let idle_since = now_ms() - 3_600_000;
            let driver_desk = layout(&[("/work", "zsh"), ("/work", "claude")]);
            let driver = JournalId::now(reaped_pid());
            marker(logs.path(), driver);
            let owner = JournalOwner::create(dir.path(), driver).expect("armed");
            let header = if marked {
                JournalHeader::now(driver.pid, false)
            } else {
                older_header(driver.pid)
            };
            owner
                .publish(
                    &driver_desk,
                    &JournalHeader {
                        written_unix_ms: idle_since,
                        ..header
                    },
                )
                .expect("published");
            let desk = layout(&[("/desk", "zsh"), ("/agent", "claude")]);
            let older = run_by_older_build(dir.path(), logs.path(), &desk, None);
            let quitting = older.id();
            let session = dir.path().join("session.toml");
            restore::write_quit_to(&session, &desk, None).expect("the older quit's layout");
            die(older); // cut short before its journal went
            die(owner); // then the daily driver is killed
            let taken = restore::take_quit_from(&session).expect("the quit layout");
            assert_eq!(taken.run, None, "an older build's quit names no run");
            let quit = QuitLayout::of(Some(&taken.manifest), None, taken.written_unix_ms);
            assert!(
                quit.written_unix_ms
                    .is_some_and(|at| at > idle_since + super::QUIT_DATE_SLACK_MS),
                "the driver's image is older than the quit layout"
            );
            assert_eq!(
                quit.shape,
                super::desk_shape(&driver_desk),
                "and has its shape"
            );
            let claim = claim_at_boot(dir.path(), &evidence(logs.path()), 1, quit);
            (claim, quitting, driver)
        };
        let (claim, quitting, driver) = scenario(true);
        let reopened = claim.reopened.expect("the driver's desk comes back");
        assert_eq!(reopened.sources, vec![driver], "behind the quit, once");
        assert_eq!(cwds(&reopened.manifest), vec!["/work", "/work"]);
        assert!(
            claim.set_aside.iter().any(|(set, _)| *set == quitting),
            "the older quit's own journal is still set aside: {:?}",
            claim.set_aside
        );

        let (claim, quitting, driver) = scenario(false);
        assert_eq!(
            claim.reopened, None,
            "the control: the driver's desk is lost"
        );
        for id in [quitting, driver] {
            assert!(claim.set_aside.iter().any(|(set, _)| *set == id));
            assert!(claim.taken.contains(&id), "taken, so gone");
        }
    }

    #[test]
    fn a_taken_over_journal_is_rewritten_and_retired_as_the_successor_s_own() {
        let dir = scratch("cj-dir");
        let parent = JournalId::now(reaped_pid());
        let owner = JournalOwner::create(dir.path(), parent).expect("armed");
        owner
            .publish(
                &layout(&[("/a", "zsh")]),
                &JournalHeader::now(parent.pid, false),
            )
            .expect("published");
        die(owner);
        let mut off = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        off.set_predecessor(parent.pid, None);
        assert_eq!(off.adopt_predecessor(false), None, "restore_session off");
        let mut none = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        none.set_predecessor(reaped_pid(), None);
        assert_eq!(none.adopt_predecessor(true), None, "a parent with no image");
        assert!(none.retire(MUST_HAPPEN).is_ok());
        assert_eq!(
            names(dir.path()),
            vec![parent.lock_name(), parent.file_name()],
            "neither touched the parent's journal, and the second removed its own lock"
        );

        let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
        lane.set_predecessor(parent.pid, None);
        let own = lane.journal_id();
        assert_eq!(lane.adopt_predecessor(true), Some(parent));
        assert!(lane.offer(layout(&[("/mine", "zsh")]), Vec::new(), Instant::now()));
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let text = std::fs::read_to_string(dir.path().join(own.file_name())).unwrap();
            if cwds(&decode(&text).unwrap().1) == ["/mine"] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the successor's image replaces it"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(lane.retire(MUST_HAPPEN).is_ok());
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    }

    #[test]
    fn a_crash_whose_evidence_another_launch_read_first_still_reopens() {
        for (banner, class) in [
            (&b""[..], DeathClass::Killed),
            (&b"aterm: fatal signal 11"[..], DeathClass::Signal),
        ] {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
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
            let read = if banner.is_empty() {
                crate::logging::take_kill_evidence_in(logs.path(), std::process::id(), None)
                    .is_some()
            } else {
                crate::logging::take_crash_evidence_in(logs.path(), None).is_some()
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

    #[test]
    fn a_killed_runs_consumed_marker_survives_the_prune_until_its_claim() {
        let three_days_ago =
            std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 3600);
        let killed_desk = |dir: &Path, logs: &Path| {
            let id = JournalId::now(reaped_pid());
            let marker = logs.join(markers::file_name(id.pid, id.nanos, MarkerOwner::App));
            std::fs::write(&marker, b"").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&marker)
                .and_then(|file| file.set_modified(three_days_ago))
                .expect("dated at the run's start");
            let owner = JournalOwner::create(dir, id).expect("armed");
            owner
                .publish(
                    &layout(&[("/a", "zsh")]),
                    &JournalHeader::now(id.pid, false),
                )
                .expect("published");
            die(owner);
            (id, marker)
        };
        let reports_since = |logs: &Path| {
            for n in 0..crate::logging::KEEP_SEEN_CRASH_REPORTS {
                std::fs::write(
                    logs.join(format!("crash-{}.log.seen", 4_000_000 + n)),
                    b"thread 'worker' panicked",
                )
                .unwrap();
            }
        };
        for prune_between in [false, true] {
            let (dir, logs) = (scratch("cj-dir"), scratch("cj-logs"));
            let (id, marker) = killed_desk(dir.path(), logs.path());
            reports_since(logs.path());
            assert!(
                crate::logging::take_kill_evidence_in(
                    logs.path(),
                    std::process::id(),
                    Some(dir.path())
                )
                .is_some(),
                "PRECONDITION: the successor reads the killed run's marker"
            );
            let seen = marker.with_file_name(format!(
                "{}.seen",
                marker.file_name().unwrap().to_string_lossy()
            ));
            assert!(seen.exists(), "PRECONDITION: consumed, not removed");
            if prune_between {
                let _ = crate::logging::take_crash_evidence_in(logs.path(), Some(dir.path()));
                assert!(
                    seen.exists(),
                    "THE PRUNE KEPT THE MARKER OF A RUN WHOSE JOURNAL WAITS"
                );
            }
            let claim = claim_here(dir.path(), logs.path());
            let reopened = claim.reopened.unwrap_or_else(|| {
                panic!(
                    "prune between = {prune_between}: set aside as {:?}",
                    claim.set_aside
                )
            });
            assert_eq!(reopened.class, DeathClass::Killed);
            assert_eq!(cwds(&reopened.manifest), vec!["/a"]);
            assert_eq!(claim.taken, vec![id]);
            if prune_between {
                let _ = crate::logging::take_crash_evidence_in(logs.path(), Some(dir.path()));
                assert!(
                    !seen.exists(),
                    "once claimed, the marker ages out as before"
                );
            }
        }
    }

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
        let path = owner.path();
        let ordinary = super::unix::read_image(&path).expect("the sole-named image reads");
        let image = std::fs::read(&path).unwrap();
        let other = dir.path().join("another-name.toml");
        std::fs::hard_link(&path, &other).unwrap();
        for name in [&path, &other] {
            assert!(
                super::unix::read_image(name).is_err(),
                "a second name must not admit another file as a journal"
            );
        }
        assert_eq!(std::fs::read(&other).unwrap(), image);
        std::fs::remove_file(&other).unwrap();
        assert_eq!(
            super::unix::read_image(&path).expect("the sole name reads again"),
            ordinary
        );
        owner.retire().unwrap();
    }

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
        lane.retire(MUST_HAPPEN).unwrap();

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
        lane.retire(MUST_HAPPEN).unwrap();
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    }

    #[test]
    fn a_capture_activity_prompted_owes_one_trailing_settle() {
        let dir = scratch("cj-settle");
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
        let t0 = Instant::now();
        assert!(lane.offer(layout(&[("/a", "claude")]), Vec::new(), t0));
        assert_eq!(lane.next_wake(t0), None, "the first capture owes none");
        assert!(!lane.capture_due(t0 + MIN_WRITE_INTERVAL));

        lane.note_activity();
        let t1 = t0 + MIN_WRITE_INTERVAL;
        assert!(lane.capture_due(t1));
        assert!(!lane.offer(layout(&[("/a", "claude")]), Vec::new(), t1));
        let t2 = t1 + MIN_WRITE_INTERVAL;
        assert!(
            !lane.capture_due(t1 + Duration::from_millis(500)),
            "the interval stands"
        );
        assert_eq!(lane.next_wake(t1), Some(t2), "the settle is woken for");
        assert!(lane.capture_due(t2), "the settle is due");
        assert!(
            lane.offer(layout(&[("/a", "claude"), ("/b", "zsh")]), Vec::new(), t2),
            "what moved since is handed over"
        );
        assert_eq!(lane.writes, 2);
        assert_eq!(lane.next_wake(t2), None, "the settle owes none");
        assert!(!lane.capture_due(t2 + MIN_WRITE_INTERVAL), "idle again");

        let mut at = t2 + MIN_WRITE_INTERVAL;
        for _ in 0..3 {
            lane.note_activity();
            assert!(lane.capture_due(at));
            assert!(!lane.capture_due(at - Duration::from_millis(1)));
            lane.offer(layout(&[("/a", "claude"), ("/b", "zsh")]), Vec::new(), at);
            at += MIN_WRITE_INTERVAL;
        }
        assert!(lane.capture_due(at), "its settle");
        lane.offer(layout(&[("/a", "claude"), ("/b", "zsh")]), Vec::new(), at);
        assert_eq!(lane.next_wake(at), None, "and none after it");
        lane.retire(MUST_HAPPEN).unwrap();
    }

    #[test]
    fn a_word_about_typing_is_captured_at_once_inside_the_write_interval() {
        let dir = scratch("cj-owner");
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), std::process::id());
        let words = |autopilot: Option<&str>, land_at: Option<i64>, holds: &[&str]| OwnerWords {
            autopilot: autopilot.map(str::to_string),
            land_at,
            holds: holds.iter().map(|h| h.to_string()).collect(),
        };
        let with = |tab: usize, w: &OwnerWords| {
            let mut m = layout(&[("/a", "claude"), ("/b", "zsh")]);
            for (index, other) in m.windows[0].restored_tabs.iter_mut().enumerate() {
                if let restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Terminal(leaf),
                } = &mut other.root
                    && index == tab
                {
                    leaf.autopilot.clone_from(&w.autopilot);
                    leaf.land_at = w.land_at;
                    leaf.holds.clone_from(&w.holds);
                }
            }
            m
        };
        let none = OwnerWords::default();
        let t0 = Instant::now();
        assert!(lane.offer(with(0, &none), Vec::new(), t0));

        let t1 = t0 + Duration::from_millis(800);
        lane.note_activity();
        assert!(!lane.capture_due(t1), "plain activity: the interval stands");
        assert_eq!(lane.next_wake(t1), Some(t0 + MIN_WRITE_INTERVAL));
        assert!(
            !lane.note_owner_words(0, &none),
            "words the last image carries owe nothing"
        );
        let land = words(Some("land"), None, &[]);
        assert!(
            !lane.note_owner_words(7, &land),
            "a session the last image has no leaf for owes nothing"
        );
        assert!(!lane.capture_due(t1));

        assert!(lane.note_owner_words(0, &land));
        assert!(lane.capture_due(t1), "owed now, inside the interval");
        assert!(lane.offer(with(0, &land), Vec::new(), t1));
        assert_eq!(lane.writes, 2);
        assert!(!lane.note_owner_words(0, &land), "carried now");
        lane.note_activity();
        assert!(
            !lane.capture_due(t1 + Duration::from_millis(100)),
            "the capture that carried it cleared the debt"
        );

        let t2 = t1 + Duration::from_millis(300);
        let held = words(Some("land"), None, &["local review"]);
        assert!(lane.note_owner_words(0, &held));
        assert!(lane.capture_due(t2));
        assert!(lane.offer(with(0, &held), Vec::new(), t2));
        let t3 = t2 + Duration::from_millis(300);
        let off = words(Some("off"), None, &["local review"]);
        assert!(lane.note_owner_words(0, &off));
        assert!(lane.capture_due(t3));
        assert!(lane.offer(with(0, &off), Vec::new(), t3));
        let timed = words(Some("cruise"), Some(9), &[]);
        let t4 = t3 + MIN_WRITE_INTERVAL;
        assert!(lane.offer(with(0, &timed), Vec::new(), t4));
        let t5 = t4 + Duration::from_millis(300);
        let cancelled = words(Some("cruise"), None, &[]);
        assert!(lane.note_owner_words(0, &cancelled));
        assert!(lane.capture_due(t5));
        assert!(lane.offer(with(0, &cancelled), Vec::new(), t5));
        assert_eq!(lane.writes, 6);

        let journal = dir.path().join(lane.journal_id().file_name());
        let image =
            aterm_spec::wait::until_within(MUST_HAPPEN, "the image naming the cancel", || {
                std::fs::read_to_string(&journal)
                    .ok()
                    .and_then(|text| decode(&text).ok())
                    .map(|(_, manifest)| manifest)
                    .filter(|manifest| OwnerWords::carried(manifest, 0) == Some(cancelled.clone()))
            });
        assert_eq!(
            OwnerWords::carried(&image, 1),
            Some(none.clone()),
            "the other tab's leaf carries no word"
        );
        lane.retire(MUST_HAPPEN).unwrap();

        let mut inert = Lane::inert();
        assert!(!inert.note_owner_words(0, &land));
        assert!(!inert.capture_due(t1));
    }

    fn image_on_disk(
        dir: &Path,
        lane: &mut Lane,
        what: &str,
        ok: impl Fn(&RestoreManifest) -> bool,
    ) -> RestoreManifest {
        let journal = dir.join(lane.journal_id().file_name());
        aterm_spec::wait::until_within(MUST_HAPPEN, what, || {
            std::fs::read_to_string(&journal)
                .ok()
                .and_then(|text| decode(&text).ok())
                .map(|(_, manifest)| manifest)
                .filter(|manifest| ok(manifest))
        })
    }

    fn held_layout(holds: &[&str]) -> RestoreManifest {
        let mut m = layout(&[("/a", "claude")]);
        if let restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(leaf),
        } = &mut m.windows[0].restored_tabs[0].root
        {
            leaf.holds = holds.iter().map(|h| h.to_string()).collect();
        }
        m
    }

    fn read_only(dir: &Path, read_only: bool) {
        use std::os::unix::fs::PermissionsExt;
        let mode = if read_only { 0o500 } else { 0o700 };
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    #[test]
    fn an_image_the_writer_could_not_publish_is_handed_again() {
        let dir = scratch("cj-failed");
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        let t0 = Instant::now();
        assert!(lane.offer(held_layout(&[]), Vec::new(), t0));
        image_on_disk(dir.path(), &mut lane, "the first image", |_| true);

        read_only(dir.path(), true);
        let t1 = t0 + MIN_WRITE_INTERVAL;
        assert!(lane.offer(held_layout(&["local review"]), Vec::new(), t1));
        aterm_spec::wait::until_within(MUST_HAPPEN, "the writer's failure heard", || {
            lane.poll_writer();
            lane.unwritten.then_some(())
        });
        read_only(dir.path(), false);
        assert!(
            !lane.capture_due(t1 + Duration::from_millis(500)),
            "inside the interval nothing is retried"
        );
        let t2 = t1 + MIN_WRITE_INTERVAL;
        assert!(lane.capture_due(t2), "owed again at the interval");
        assert!(
            lane.offer(held_layout(&["local review"]), Vec::new(), t2),
            "the same desk's image is handed again"
        );
        image_on_disk(dir.path(), &mut lane, "the hold's image", |manifest| {
            OwnerWords::carried(manifest, 0).is_some_and(|words| !words.holds.is_empty())
        });
        let t3 = t2 + MIN_WRITE_INTERVAL;
        lane.poll_writer();
        lane.note_activity();
        assert!(
            !lane.offer(held_layout(&["local review"]), Vec::new(), t3),
            "written, the same desk is handed nothing"
        );
        lane.retire(MUST_HAPPEN).unwrap();
    }

    #[test]
    fn a_words_ticket_is_covered_once_an_image_on_disk_carries_it() {
        let dir = scratch("cj-ticket");
        let mut lane = Lane::new(Some(dir.path().to_path_buf()), reaped_pid());
        let durable = lane.durable();
        let t0 = Instant::now();
        lane.begin_capture();
        assert!(lane.offer(held_layout(&[]), Vec::new(), t0));
        image_on_disk(dir.path(), &mut lane, "the first image", |_| true);

        let first = durable.ask().expect("an active lane hands out tickets");
        let t1 = t0 + Duration::from_millis(300);
        assert!(lane.capture_due(t1), "a ticket owes a capture now");
        lane.begin_capture();
        assert!(!lane.offer(held_layout(&[]), Vec::new(), t1), "nothing new");
        assert_eq!(durable.wait(first, MUST_HAPPEN), Settled::Durable);
        assert!(!lane.capture_due(t1), "carried: owed no more");

        read_only(dir.path(), true);
        let second = durable.ask().expect("a ticket");
        lane.begin_capture();
        assert!(lane.offer(held_layout(&["local -"]), Vec::new(), t1));
        assert_eq!(
            durable.wait(second, Duration::from_millis(50)),
            Settled::Late,
            "no image on disk carries it"
        );
        aterm_spec::wait::until_within(MUST_HAPPEN, "the writer's failure heard", || {
            lane.poll_writer();
            lane.unwritten.then_some(())
        });
        assert_eq!(durable.covered(), first, "still not covered");
        read_only(dir.path(), false);
        let t2 = t1 + MIN_WRITE_INTERVAL;
        assert!(lane.capture_due(t2));
        lane.begin_capture();
        assert!(lane.offer(held_layout(&["local -"]), Vec::new(), t2));
        assert_eq!(durable.wait(second, MUST_HAPPEN), Settled::Durable);
        image_on_disk(dir.path(), &mut lane, "the hold on disk", |manifest| {
            OwnerWords::carried(manifest, 0).is_some_and(|words| !words.holds.is_empty())
        });

        let third = durable.ask().expect("a ticket");
        lane.withdraw();
        assert_eq!(durable.wait(third, MUST_HAPPEN), Settled::Unjournaled);
        assert_eq!(durable.ask(), None, "and hands out no more");
        lane.retire(MUST_HAPPEN).unwrap();

        assert_eq!(Lane::inert().durable().ask(), None);
    }

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
        lane.retire(MUST_HAPPEN).expect("retired");
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
            written += usize::from(lane.offer(layout(&[("/a", title.as_str())]), Vec::new(), t));
        }
        let spun = MIN_WRITE_INTERVAL * 60;
        assert_eq!(
            written as u128,
            1 + spun.as_millis() / TITLE_WRITE_INTERVAL.as_millis(),
            "{spun:?} of spinner: once per title interval"
        );

        t += MIN_WRITE_INTERVAL;
        lane.note_activity();
        assert!(lane.offer(layout(&[("/a", "spin 60"), ("/b", "zsh")]), Vec::new(), t));
        let published = t;

        t += MIN_WRITE_INTERVAL;
        lane.note_activity();
        assert!(!lane.offer(layout(&[("/a", "done"), ("/b", "zsh")]), Vec::new(), t));
        let settle = t + MIN_WRITE_INTERVAL;
        assert_eq!(lane.next_wake(t), Some(settle));
        assert!(!lane.offer(layout(&[("/a", "done"), ("/b", "zsh")]), Vec::new(), settle));
        assert!(
            !lane.capture_due(settle + MIN_WRITE_INTERVAL),
            "idle, and not yet due"
        );
        let due = published + TITLE_WRITE_INTERVAL;
        assert_eq!(lane.next_wake(settle), Some(due));
        assert!(lane.capture_due(due));
        assert!(lane.offer(layout(&[("/a", "done"), ("/b", "zsh")]), Vec::new(), due));
        assert_eq!(lane.next_wake(due), None);
        lane.retire(MUST_HAPPEN).unwrap();
    }
}

/// `CrashJournalClaim`'s eight actions that are the process around the journal.
#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootFresh",
    reason = "The run's own start: the crash marker it arms is crash_signal's install; the \
              runner arms one in the real log dir and the journal is armed lazily at the \
              first Write."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootFromJournal",
    reason = "The run's own start after a reopened journal: Lane::begin_probation only sets \
              the header mark the runner writes through JournalOwner::publish; its 90 s \
              clock is crash_journal::tests::the_lane_writes_only_changes_at_the_bounded_rate."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "BootSecondChance",
    reason = "The run's own start after the brake let a stopped probation writer's journal \
              through (ruling 285): Lane::begin_probation's second-chance flag only sets the \
              header mark the runner writes through JournalOwner::publish \
              (crash_journal::tests::a_relapse_inside_probation_is_skipped_and_said)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "Crash",
    reason = "A fatal signal runs no journal code: crash_signal's handler writes its banner \
              into the run's own marker, which the runner writes, and the kernel releases the \
              locks as for Die."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "HandOff",
    reason = "The update parent's _exit after Commit is seamless::commit_and_exit, which \
              removes the crash marker and ends the process; the runner removes the marker \
              and drops the owner, whose drop releases its lock by LOCK_UN."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "Die",
    reason = "A SIGKILL, a signal or a panic runs no journal code: the runner drops the owner, \
              whose lock goes as the kernel releases a dead process's (a child forked \
              under it and not yet exec'd keeps it until its exec), leaving the marker."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "FinishWrite",
    reason = "Exists only in the Buggy = 1 unlink-first writer; the shipping publish is one \
              rename, which the_defects_reject_the_real_steps pins, and a reader racing 400 real \
              rewrites always finds a whole image (crash_journal::tests::\
              a_rewrite_never_leaves_the_name_without_a_whole_image)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "CrashJournalClaim",
    action = "SuccessorDies",
    reason = "A successor's end before it took anything over runs no journal code: the \
              shipping Commit waiter takes the parent's image over before it releases a reader \
              (release_committed_readers, round eight of the update audit, finding 45; \
              crash_journal::tests::\
              a_committed_successor_that_dies_before_its_commit_wake_still_reopens_the_desk), so \
              the runner plays it only where the parent left no image."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn crash_journal_claim_waivers() {}
