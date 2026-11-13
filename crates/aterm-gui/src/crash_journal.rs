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

/// At most one journal write per this interval when only titles changed. A title whose
/// leading spinner frame alone turned has not changed ([`same_but_spinner_frames`]).
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
                for tab in &window.tabs {
                    tab.visit_leaves(&mut |leaf| {
                        if leaf.cwd().is_some_and(&not_found) {
                            out.gone += 1;
                        }
                    });
                }
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
    use crate::durable::Flush;
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
            // The removal has happened; report its persistence failure without changing
            // the quit's result.
            sync_dir_logged(&self.dir, "the quit removed this run's image");
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
        // The take-over has happened; report its persistence failure and continue settling
        // the predecessor's lock.
        sync_dir_logged(dir, "the successor took over its predecessor's image");
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
        let handle = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)?;
        match flush {
            Some(flush) => crate::durable::finish(&handle, flush),
            None => aterm_durable::primitive::fsync(&handle),
        }
    }

    /// [`sync_dir`]'s plain `fsync(2)` (aterm-durable's, which astream-sys counts `plain`)
    /// after `step` has taken effect: a folder the volume
    /// cannot flush, or open, is accepted
    /// (`aterm_durable::DirFlushed::classify`, astream-sys's `dir_flush_unavailable`), and a
    /// failed flush is logged.
    fn sync_dir_logged(dir: &Path, step: &str) {
        if let Err(error) = aterm_durable::DirFlushed::classify(sync_dir(dir, None)) {
            aterm_log::warn!(
                "crash journal {}: {step}, but flushing the folder failed, so a \
                 power loss may take it back: {error}",
                dir.display()
            );
        }
    }

    fn create_temporary(dir: &Path, id: JournalId) -> Result<(PathBuf, File), String> {
        for _ in 0..64 {
            let candidate = dir.join(format!("{}{}.tmp", id.temp_prefix(), next_seed()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
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
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            if name == panic_report || name == consumed_report {
                let Ok(meta) = entry.metadata() else { continue };
                if meta.file_type().is_file()
                    && meta.len() > 0
                    && let Ok(modified) = meta.modified()
                {
                    reports.push((modified, name == consumed_report, entry.path()));
                }
            } else if let Some(marker) = markers::parse(name) {
                if marker.pid != id.pid {
                    continue;
                }
                if marker.nanos > id.nanos {
                    next_run = Some(next_run.map_or(marker.nanos, |n| n.min(marker.nanos)));
                } else if own_marker.is_none_or(|(nanos, _)| marker.nanos > nanos)
                    && let Ok(meta) = entry.metadata()
                    && meta.file_type().is_file()
                {
                    own_marker = Some((marker.nanos, meta.len()));
                }
            } else if let Some(consumed) = name.strip_suffix(".seen").and_then(markers::parse) {
                if consumed.pid != id.pid {
                    continue;
                }
                if consumed.nanos > id.nanos {
                    next_run = Some(next_run.map_or(consumed.nanos, |n| n.min(consumed.nanos)));
                } else if consumed.nanos == id.nanos
                    && let Ok(meta) = entry.metadata()
                    && meta.file_type().is_file()
                {
                    own_consumed = Some(meta.len());
                }
            } else if name.starts_with(&legacy_prefix)
                && name.ends_with(".log")
                && {
                    let s = &name[legacy_prefix.len()..name.len() - 4];
                    !s.is_empty()
                        && s.bytes().all(|b| b.is_ascii_digit())
                        && s.parse::<u128>().is_ok_and(|nanos| nanos <= id.nanos)
                }
                && entry
                    .metadata()
                    .is_ok_and(|meta| meta.file_type().is_file() && meta.len() > 0)
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
        let mut text =
            String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
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
        // The claim has happened; report its persistence failure while preserving the
        // reopen decision.
        sync_dir_logged(dir, "the boot claimed the dead runs' images");
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
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(taken)
            .ok()?;
        let kept = dir.join(id.unreadable_name());
        let private = file.metadata().is_ok_and(|meta| {
            meta.file_type().is_file()
                && meta.nlink() == 1
                && (meta.permissions().mode() & 0o7777 == 0o600
                    || file
                        .set_permissions(fs::Permissions::from_mode(0o600))
                        .is_ok())
        });
        if !private || fs::rename(taken, &kept).is_err() {
            return None;
        }
        let mut aside: Vec<(JournalId, std::ffi::OsString)> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let other = JournalId::parse_unreadable(name.to_str()?)?;
                Some((other, name))
            })
            .collect();
        aside.sort_unstable_by_key(|(other, _)| std::cmp::Reverse(other.nanos));
        for (_, name) in aside.into_iter().skip(KEEP_UNREADABLE) {
            let _ = fs::remove_file(dir.join(name));
        }
        fs::symlink_metadata(&kept).is_ok().then_some(kept)
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
                        .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
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
        if !self.unwritten
            && self
                .last
                .as_ref()
                .is_some_and(|last| *last == written || same_but_spinner_frames(last, &written))
        {
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

/// Whether `written` is `last` but for the spinner frames its agents lead their titles with:
/// everything a restore reads is the same, each title's subject included
/// ([`title_subjects`]). A working agent turns that frame several times a second for as long as
/// its turn runs, and which frame the image holds restores nothing, so such a capture is the
/// image already written: no write, and none owed. A title whose subject moved, or one that
/// gained or lost its frame (a turn began or ended), is a title change as before
/// ([`same_but_titles`], [`TITLE_WRITE_INTERVAL`]).
fn same_but_spinner_frames(last: &Written, written: &Written) -> bool {
    last.probation == written.probation
        && last.programs == written.programs
        && last.handed == written.handed
        && same_but_titles(&last.manifest, &written.manifest)
        && title_subjects(&last.manifest) == title_subjects(&written.manifest)
}

/// The titles of `manifest`'s terminal leaves, in layout order, each as
/// [`spinner_frame_apart`] reads it.
fn title_subjects(manifest: &RestoreManifest) -> Vec<(bool, &str)> {
    use crate::restore::{PaneLayout, RestoredSplitTree, RestoredView};
    fn pane<'a>(node: &'a PaneLayout, out: &mut Vec<(bool, &'a str)>) {
        match node {
            PaneLayout::Leaf { title, .. } => out.push(spinner_frame_apart(title)),
            PaneLayout::Split { first, second, .. } => {
                pane(first, out);
                pane(second, out);
            }
        }
    }
    fn tree<'a>(node: &'a RestoredSplitTree, out: &mut Vec<(bool, &'a str)>) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } => out.push(spinner_frame_apart(&leaf.title)),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                tree(first, out);
                tree(second, out);
            }
        }
    }
    let mut out = Vec::new();
    for window in &manifest.windows {
        for tab in &window.tabs {
            pane(tab, &mut out);
        }
        for tab in &window.restored_tabs {
            tree(&tab.root, &mut out);
        }
    }
    out
}

/// `title` as whether a spinner frame leads it ([`crate::chrome_label::is_spinner_frame`]) and
/// what follows that frame, the whole title where none does: two titles that read alike here
/// differ at most in which frame leads them.
fn spinner_frame_apart(title: &str) -> (bool, &str) {
    let mut chars = title.chars();
    match chars.next() {
        Some(frame) if crate::chrome_label::is_spinner_frame(frame) => (true, chars.as_str()),
        _ => (false, title),
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
            claim_worker: a_claim_worker,
            claim_ended: a_claim_ended,
            not_claim_worker: a_not_claim_worker,
            holds: a_holds,
            sid: a_sid,
            lineage: a_lineage,
            log_ep: a_log_ep,
            land_at: a_land_at,
            autopilot_by: a_autopilot_by,
            theme: a_theme,
            lost: a_lost,
            task: a_task,
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
            claim_worker: b_claim_worker,
            claim_ended: b_claim_ended,
            not_claim_worker: b_not_claim_worker,
            holds: b_holds,
            sid: b_sid,
            lineage: b_lineage,
            log_ep: b_log_ep,
            land_at: b_land_at,
            autopilot_by: b_autopilot_by,
            theme: b_theme,
            lost: b_lost,
            task: b_task,
        } = b;
        a_cwd == b_cwd
            && a_task == b_task
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
            && a_claim_worker == b_claim_worker
            && a_claim_ended == b_claim_ended
            && a_not_claim_worker == b_not_claim_worker
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
            parked: pa,
            ephemeral: ea,
            fleet: la,
        } = a;
        let RestoredTab {
            root: rb,
            focused_path: fb,
            zoomed: zb,
            parked: pb,
            ephemeral: eb,
            fleet: lb,
        } = b;
        za == zb && pa == pb && ea == eb && la == lb && fa == fb && same_tree(ra, rb)
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
            workers_in_front: wfa,
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
            workers_in_front: wfb,
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
            && wfa == wfb
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
#[path = "crash_journal_fixtures_tests.rs"]
pub(crate) mod fixtures;

#[cfg(all(test, unix))]
#[path = "crash_journal_tests.rs"]
pub(crate) mod tests;

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
