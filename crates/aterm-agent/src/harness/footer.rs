// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code footer.

use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aterm_json::Value;
use aterm_primer::CLAUDE_TOP_EFFORT_KEY;

/// How much of a transcript's end one read window takes.
pub const TAIL_BYTES: u64 = 512 * 1024;

const MAX_LINE_BYTES: usize = 256 * 1024;

const MAX_SESSION_FILE_BYTES: u64 = 64 * 1024;

const MAX_GIT_CLIMB: usize = 64;

const MAX_GIT_FILE_BYTES: u64 = 4096;

/// The mark before model + effort.
pub const MODEL_MARK: char = '\u{25C6}';
/// The mark before the working directory.
pub const PATH_MARK: char = '\u{2302}';
/// The mark before the branch.
pub const BRANCH_MARK: char = '\u{2387}';
/// The mark before the account the process runs on.
pub const ACCOUNT_MARK: char = '\u{2386}';
/// The mark before the selected account's subscription plan.
pub const PLAN_MARK: char = '\u{25c7}';
/// The mark before the selected account's weekly quota.
pub const WEEKLY_MARK: char = '\u{25f7}';
/// The mark before a limit wall the session hit.
pub const WALL_MARK: char = '\u{29D7}';
/// The mark before the session's context window indicator.
pub const CONTEXT_MARK: char = '\u{25D0}';
/// The mark before the session's usage.
pub const USAGE_MARK: char = '\u{03A3}';
/// The mark inside a limit wall that stands for "resets" (`⧗ 5h ♻ 3pm`).
pub const RESET_MARK: char = '\u{267B}';
/// The mark before a live upgrade that waits on the person at this composer ([`UpgradeNote`]).
pub const UPGRADE_MARK: char = '\u{21E3}';

/// Most transcript bytes one [`FooterCache::usage`] folds.
pub const FOLD_BUDGET: u64 = 64 * 1024 * 1024;

/// The pitch the facts are set at, in cells ([`set_facts`]).
pub const FACT_PITCH: f32 = 0.8;

/// The space between two facts, in pitches.
pub const GAP: f32 = 3.0;

/// The steps a cell is divided into for a fact glyph's sideways move ([`SetGlyph::nudge`]).
pub const NUDGES_PER_CELL: i8 = 16;

/// Rule glyphs left at the left end of the run aterm writes into ([`rule_run`]).
pub const RULE_LEAD: usize = 3;

/// Rule glyphs between the lights and the facts, when both are drawn.
pub const RULE_SEP: usize = 3;

/// What one Claude Code process's registry file says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// The session id — the transcript's file stem.
    pub session_id: String,
    /// The directory the session runs in.
    pub cwd: PathBuf,
    /// The running build's version, as Claude Code records it (`2.1.283`).
    pub version: Option<String>,
    /// The process's start (unix seconds), from the file's `procStart`.
    pub proc_start: Option<u64>,
    /// When the running image registered (unix seconds, from `startedAt` ms).
    pub started_at: Option<u64>,
}

/// Whose facts these are: the transcript floor and session they were read for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FactsOwner {
    /// The floor (unix seconds) under the rows that speak for the process.
    pub floor: Option<u64>,
    /// The session id the registry names.
    pub session_id: String,
}

/// The facts the footer shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FooterFacts {
    /// The model's display name (`Opus 5.5`).
    pub model: Option<String>,
    /// The effort level (`xhigh`).
    pub effort: Option<String>,
    /// Where the session's Bash tool stands, else its launch directory, home shown as `~`.
    pub path: Option<String>,
    /// The checked-out branch, or a short commit id when HEAD is detached.
    pub branch: Option<String>,
    /// The working directory when the repository read failed with `EPERM`.
    pub repo_read_denied: Option<PathBuf>,
    /// The Claude Code build this process runs (`2.1.283`).
    pub version: Option<String>,
    /// The command resuming this process's own conversation (`claude --resume <id>` with its launch flags, [`crate::harness::resume::of_entry`]), for the frozen-program remedy.
    pub resume: Option<String>,
    /// The session's usage ([`super::session_usage`]); only [`FooterCache::usage`] fills it.
    pub usage: Option<super::session_usage::UsageFacts>,
    /// Nothing since the process started decided the model.
    pub model_open: bool,
    /// Nothing since the process started decided the effort.
    pub effort_open: bool,
    /// While [`Self::effort_open`]: the model a launch card must name for its effort to count.
    pub effort_for: Option<String>,
    /// Whose facts these are, when the registry named a session.
    pub owner: Option<FactsOwner>,
    /// The live upgrade waiting on the person here, in the words the composer now calls for.
    pub upgrade: Option<UpgradeWords>,
    /// The roster's label for the login this process runs under.
    pub account: Option<String>,
    /// What the newest statement since the process started says of the top-effort key.
    pub top_effort: Option<bool>,
    /// The facts the person hid in this session by a click.
    pub off: FactMask,
    /// Volatile email, subscription and weekly quota from the selected process's own profile.
    pub subscription: Option<super::profile::SubscriptionDisplay>,
    /// The window that binds this process's model ([`super::profile::plan_usage_for`]), from the same read as [`Self::subscription`] ([`super::profile::subscription_and_plan_for`]) and replaced with it.
    pub plan: Option<super::profile::PlanUsage>,
    /// The context left, as read off the agent's own indicator on the glass (`ctx 84% left`).
    pub context: Option<String>,
}

/// One of the footer's facts a person can act on in the bottom rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FactKind {
    /// The model's name (`Opus 5.5`).
    Model,
    /// The effort level (`xhigh`).
    Effort,
    /// The working directory (`~/aterm`).
    Path,
    /// The branch (`main`).
    Branch,
    /// What is left of the context window (`ctx 84% left`).
    Context,
    /// The session's usage (`Σ 6m33s ↑20M ↓82k`); never hidden by a click.
    Usage,
}

impl FactKind {
    const fn bit(self) -> u8 {
        match self {
            Self::Model => 1,
            Self::Effort => 2,
            Self::Path => 4,
            Self::Branch => 8,
            Self::Context => 16,
            Self::Usage => 0,
        }
    }

    /// The mark its segment leads with; none for the effort, which shares the model's.
    #[must_use]
    pub const fn mark(self) -> Option<char> {
        match self {
            Self::Model => Some(MODEL_MARK),
            Self::Effort => None,
            Self::Path => Some(PATH_MARK),
            Self::Branch => Some(BRANCH_MARK),
            Self::Context => Some(CONTEXT_MARK),
            Self::Usage => Some(USAGE_MARK),
        }
    }
}

/// A set of hidden facts ([`FooterFacts::off`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FactMask(u8);

impl FactMask {
    /// The model hidden.
    pub const MODEL: Self = Self(FactKind::Model.bit());
    /// The effort hidden.
    pub const EFFORT: Self = Self(FactKind::Effort.bit());
    /// The working directory hidden.
    pub const PATH: Self = Self(FactKind::Path.bit());
    /// The branch hidden.
    pub const BRANCH: Self = Self(FactKind::Branch.bit());
    /// The context window's indicator hidden.
    pub const CONTEXT: Self = Self(FactKind::Context.bit());

    /// Whether `kind` is in the set (never [`FactKind::Usage`]).
    #[must_use]
    pub const fn has(self, kind: FactKind) -> bool {
        kind.bit() != 0 && self.0 & kind.bit() != 0
    }

    /// The set with `kind` in it (`on`) or out of it; [`FactKind::Usage`] changes nothing.
    #[must_use]
    pub const fn with(self, kind: FactKind, on: bool) -> Self {
        if on {
            Self(self.0 | kind.bit())
        } else {
            Self(self.0 & !kind.bit())
        }
    }

    /// Both sets together.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Nothing hidden.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// One wording of an [`UpgradeNote`]: whole, and short for a narrow rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UpgradeWords {
    /// `Claude Code 2.1.286 is ready · waits while you use this tab`.
    pub whole: String,
    /// `2.1.286 waits while you use this tab`.
    pub short: String,
}

/// What the footer says of a live upgrade that waits only on the person at this composer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UpgradeNote {
    /// While the composer holds no draft: `… waits while you use this tab`.
    pub using: Option<UpgradeWords>,
    /// While a draft stands in the composer: `… waits until your draft is sent or cleared`.
    pub draft: UpgradeWords,
}

impl UpgradeNote {
    /// The words for a composer that holds a person's draft (`draft`) or none, if any.
    #[must_use]
    pub const fn words(&self, draft: bool) -> Option<&UpgradeWords> {
        if draft {
            Some(&self.draft)
        } else {
            self.using.as_ref()
        }
    }
}

/// What the process's own command line says ([`launch_facts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct LaunchFacts {
    /// The model `--model` names, only when it is a real id (`claude-opus-5-5`).
    pub model: Option<String>,
    /// The model is pinned from launch: any `--model`, or an environment pin.
    pub pinned: bool,
    /// The conversation the launch resumed (`--resume`, `--continue`), when it resumed one.
    pub resume: Option<LaunchResume>,
    /// The `CLAUDE_CONFIG_DIR` the process's environment at exec named, raw.
    pub config: Option<String>,
}

/// The conversation a process's launch resumed ([`crate::harness::upgrade::launch_resume`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LaunchResume {
    /// `--resume <id>` / `-r <id>`: the process's first session is the conversation it names.
    Session(String),
    /// A resume that names no conversation the process keeps.
    Unnamed,
}

/// What `argv` (`argv[0]` included) says about the model.
#[must_use]
pub fn launch_facts(argv: &[String]) -> LaunchFacts {
    launch_facts_of(argv, &[])
}

/// [`launch_facts`], and whether the process's environment at exec (`env`, `KEY=value` entries) pins its model.
#[must_use]
pub fn launch_facts_of(argv: &[String], env: &[String]) -> LaunchFacts {
    let flag = crate::harness::upgrade::launch_model(argv);
    let env_pin = env.iter().any(|kv| {
        kv.split_once('=').is_some_and(|(key, value)| {
            !value.is_empty()
                && (key == "ANTHROPIC_MODEL"
                    || key
                        .strip_prefix("ANTHROPIC_DEFAULT_")
                        .is_some_and(|rest| rest.ends_with("_MODEL")))
        })
    });
    let pinned = flag.as_deref().is_some_and(|m| !m.is_empty()) || env_pin;
    let model = flag
        .filter(|m| {
            crate::harness::upgrade::is_model_id(m)
                && crate::harness::upgrade_models::family_version(m).is_some()
        })
        .map(|m| model_display(&m));
    let resume = crate::harness::upgrade::launch_resume(argv)
        .map(|id| id.map_or(LaunchResume::Unnamed, LaunchResume::Session));
    let config = env.iter().find_map(|kv| {
        kv.split_once('=')
            .filter(|(key, _)| *key == crate::harness::accounts::HOME_ENV)
            .map(|(_, value)| value.to_string())
    });
    LaunchFacts {
        model,
        pinned,
        resume,
        config,
    }
}

/// One marked value in the footer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segment {
    /// The mark ([`MODEL_MARK`], [`PATH_MARK`], [`BRANCH_MARK`]).
    pub mark: char,
    /// The value after it.
    pub text: String,
    /// Where the model-and-effort segment's effort starts in `text` (a byte offset).
    pub split: Option<usize>,
    /// Whether the head and the tail (`split`) are a hidden fact's stub.
    pub quiet: (bool, bool),
}

impl Segment {
    /// A shown value with no split: `mark` and `text`.
    #[must_use]
    pub fn plain(mark: char, text: impl Into<String>) -> Self {
        Self {
            mark,
            text: text.into(),
            ..Self::default()
        }
    }

    fn shown_part(&self) -> (char, &str) {
        let text = self.text.as_str();
        let shown = match (self.split, self.quiet) {
            (_, (false, false)) => text,
            (Some(at), (true, false)) => text.get(at..).unwrap_or(""),
            (Some(at), (false, true)) => text.get(..at).unwrap_or("").trim_end(),
            (None, (false, true)) => text,
            (_, (true, _)) => "",
        };
        (self.mark, shown)
    }
}

/// What a hidden fact says after its greyed value.
pub const OFF_STUB: &str = "off";

/// The Claude Code directory of one process, resolved from `config_dir` (`CLAUDE_CONFIG_DIR`), `home` (`HOME`), and `cwd`.
pub fn claude_dir_of(
    config_dir: Option<&str>,
    home: Option<&str>,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    use super::upgrade_drive::{HomeVars, claude_home_dir};
    claude_home_dir(
        None,
        HomeVars {
            config: config_dir,
            home,
        },
        cwd,
    )
}

fn open_nonblocking(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

fn regular(file: File) -> Option<File> {
    file.metadata().ok()?.is_file().then_some(file)
}

/// Open `path` for reading only if it is a regular file ([`regular`]).
pub(crate) fn open_regular(path: &Path) -> Option<File> {
    regular(open_nonblocking(path).ok()?)
}

fn read_capped(file: File, cap: u64) -> Option<String> {
    let mut text = String::new();
    file.take(cap).read_to_string(&mut text).ok()?;
    Some(text)
}

/// At most `cap` bytes of the regular file at `path`, as text.
pub(crate) fn read_small(path: &Path, cap: u64) -> Option<String> {
    read_capped(open_regular(path)?, cap)
}

/// The registry entry for Claude Code process `pid`, or `None` when that pid is not a Claude Code session.
pub fn session_of_pid(claude_dir: &Path, pid: u32, started: Option<u64>) -> Option<SessionEntry> {
    let path = claude_dir.join("sessions").join(format!("{pid}.json"));
    #[cfg(test)]
    tests::before_registry_read(&path);
    parse_session_entry(&read_small(&path, MAX_SESSION_FILE_BYTES)?, pid, started)
}

/// [`session_of_pid`]'s parse, pure.
pub fn parse_session_entry(text: &str, pid: u32, started: Option<u64>) -> Option<SessionEntry> {
    let value: Value = aterm_json::from_str(text).ok()?;
    let obj = value.as_object()?;
    if obj.get("pid").and_then(Value::as_u64) != Some(u64::from(pid)) {
        return None;
    }
    let recorded = obj.get("procStart").and_then(Value::as_str);
    if let (Some(started), Some(recorded)) = (started, recorded)
        && squash(recorded) != lstart_utc(started)
    {
        return None;
    }
    let session_id = obj.get("sessionId").and_then(Value::as_str)?;
    if !is_session_id(session_id) {
        return None;
    }
    let cwd = obj.get("cwd").and_then(Value::as_str)?;
    let version = obj
        .get("version")
        .and_then(Value::as_str)
        .filter(|v| is_word(v))
        .map(str::to_owned);
    Some(SessionEntry {
        session_id: session_id.to_owned(),
        cwd: PathBuf::from(cwd),
        version,
        proc_start: recorded.and_then(crate::harness::upgrade_models::parse_lstart),
        started_at: obj
            .get("startedAt")
            .and_then(Value::as_u64)
            .map(|ms| ms / 1000),
    })
}

/// Whether `s` has the shape of a Claude Code session id — the transcript's file stem.
#[must_use]
pub fn is_session_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A unix time as Claude Code records `procStart`.
pub fn lstart_utc(secs: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(i64::try_from(days).unwrap_or(0));
    let weekday = DAYS[usize::try_from(days % 7).unwrap_or(0)];
    let month = MONTHS[usize::try_from(m - 1).unwrap_or(0).min(11)];
    format!(
        "{weekday} {month} {d} {:02}:{:02}:{:02} {y}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Claude Code's project directory name for `cwd`.
pub fn project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The transcript of `entry`.
pub fn transcript_path(claude_dir: &Path, entry: &SessionEntry) -> Option<PathBuf> {
    transcript_lookup(claude_dir, entry).map(|(path, _)| path)
}

/// [`transcript_path`] and whether it is CERTAIN (plan §2.1).
pub fn transcript_lookup(claude_dir: &Path, entry: &SessionEntry) -> Option<(PathBuf, bool)> {
    #[cfg(test)]
    tests::on_transcript_lookup();
    let projects = claude_dir.join("projects");
    let file = format!("{}.jsonl", entry.session_id);
    let direct = projects.join(project_slug(&entry.cwd)).join(&file);
    if direct.is_file() {
        return Some((direct, true));
    }
    let mut found = std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|dir| dir.path().join(&file))
        .filter(|candidate| candidate.is_file());
    let first = found.next()?;
    Some((first, found.next().is_none()))
}

/// [`transcript_path`] for a reader that must not guess between two.
pub fn sole_transcript_path(claude_dir: &Path, entry: &SessionEntry) -> Option<PathBuf> {
    let projects = claude_dir.join("projects");
    let file = format!("{}.jsonl", entry.session_id);
    let direct = projects.join(project_slug(&entry.cwd)).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    let mut found = std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|dir| dir.path().join(&file))
        .filter(|candidate| candidate.is_file());
    let one = found.next()?;
    found.next().is_none().then_some(one)
}

/// What the last [`TAIL_BYTES`] of the transcript at `path` say about the model and effort, counting only rows written no earlier than `since`.
pub fn read_tail_facts(path: &Path, since: Option<u64>) -> Option<TailFacts> {
    Some(tail_facts(&read_tail(path)?, since))
}

/// The most bytes one refresh reads as an append to a transcript it has read
/// before ([`TailCache`]'s carry); a larger append is read as a fresh tail.
#[cfg(unix)]
const MAX_APPEND_BYTES: u64 = 16 * 1024 * 1024;

/// How many bytes before the carried end are checked unchanged before an
/// append is read on top of what was read before.
#[cfg(unix)]
const GUARD_BYTES: u64 = 4096;

const MAX_BACK_BYTES: u64 = 16 * 1024 * 1024;

/// The resolver's per-session transcript memo, keyed by process and image start.
#[derive(Debug, Default)]
pub struct TailCache {
    entry: Option<(TailKey, TailRead)>,
    #[cfg(unix)]
    carry: Option<Carry>,
    process: Option<ProcessFacts>,
    /// The excerpts of the last read ([`TailCache::excerpts`]).
    excerpts: Option<super::excerpt::Excerpts>,
    /// The transcript's length as the last read found it
    /// ([`TailCache::transcript_len`]).
    len: Option<u64>,
    #[cfg(test)]
    reads: usize,
    #[cfg(all(test, unix))]
    appends: usize,
}

/// One read of a transcript since the floor: what it decided, whether it met
/// a row from before the floor (the conversation was resumed), where the
/// Bash tool stands ([`last_cwd_since`]), and the survey's excerpts of the
/// rows read, floor or no floor ([`super::excerpt::Scan`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TailRead {
    facts: TailFacts,
    resumed: bool,
    cwd: Option<PathBuf>,
    excerpts: super::excerpt::Scan,
}

/// What one Claude Code process last decided, whichever of its sessions said
/// it ([`TailCache`]).
#[derive(Debug)]
struct ProcessFacts {
    pid: u32,
    started: Option<u64>,
    floor: u64,
    /// The session the process was last read in.
    session: String,
    /// The floor under `session`'s rows that speak for the process
    /// ([`TailCache::session_floor`]): the process's own floor, or — for a
    /// session it switched into — the moment it was last seen in the session
    /// it left.
    session_floor: u64,
    /// When the process was last seen in `session` (unix seconds): the
    /// wall-clock time of the newest read that found it there.
    seen: u64,
    facts: TailFacts,
    /// The process's model is pinned by what this cache has read of it: a
    /// choice in a session it left ([`chose_model_since`]), or a restore — a
    /// switch into, or a first session that is, a resumed conversation with
    /// an answer to restore ([`TailCache`]).
    pinned: bool,
}

/// What [`TailCache`] carries across appends (unix only: it needs the file's
/// identity).
#[cfg(unix)]
#[derive(Debug)]
struct Carry {
    path: PathBuf,
    pid: u32,
    started: Option<u64>,
    since: u64,
    dev: u64,
    ino: u64,
    /// Where the rows read so far end: just past the last whole row.
    read_to: u64,
    /// A hash of the [`GUARD_BYTES`] before `read_to`.
    guard: u64,
    /// What those rows said.
    facts: TailRead,
}

#[cfg(unix)]
fn guard_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

#[derive(Debug, PartialEq, Eq)]
struct TailKey {
    path: PathBuf,
    pid: u32,
    started: Option<u64>,
    since: u64,
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl TailKey {
    fn of(
        path: &Path,
        pid: u32,
        started: Option<u64>,
        since: u64,
        metadata: &Metadata,
    ) -> Option<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        #[cfg(not(unix))]
        {
            let _ = (path, pid, started, since, metadata);
            None
        }
        #[cfg(unix)]
        Some(Self {
            path: path.to_owned(),
            pid,
            started,
            since,
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            dev: metadata.dev(),
            ino: metadata.ino(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        })
    }
}

impl TailCache {
    fn read(
        &mut self,
        path: &Path,
        pid: u32,
        started: Option<u64>,
        since: u64,
    ) -> Option<TailFacts> {
        self.read_since(path, pid, started, since)
            .map(|read| read.facts)
    }

    /// Read the current tail, or reuse the previous parse only after opening
    /// and identifying this exact regular file.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClaudeFooterModel",
            action = "Read",
            project = "conformance_footer_model::project"
        )
    )]
    fn read_since(
        &mut self,
        path: &Path,
        pid: u32,
        started: Option<u64>,
        since: u64,
    ) -> Option<TailRead> {
        let read = self.read_tail_since(path, pid, started, since);
        self.excerpts = read.as_ref().map(|read| read.excerpts.excerpts());
        read
    }

    fn read_tail_since(
        &mut self,
        path: &Path,
        pid: u32,
        started: Option<u64>,
        since: u64,
    ) -> Option<TailRead> {
        let mut file = match open_regular(path) {
            Some(file) => file,
            None => {
                self.clear();
                return None;
            }
        };
        let before = match file.metadata() {
            Ok(before) => before,
            Err(_) => {
                self.clear();
                return None;
            }
        };
        self.len = Some(before.len());
        let key = TailKey::of(path, pid, started, since, &before);
        if let Some(key) = key.as_ref()
            && let Some((old, facts)) = self.entry.as_ref()
            && old == key
            && file
                .metadata()
                .ok()
                .and_then(|after| TailKey::of(path, pid, started, since, &after))
                .as_ref()
                == Some(key)
        {
            return Some(facts.clone());
        }
        self.entry = None;
        #[cfg(test)]
        {
            self.reads += 1;
        }
        let facts = match self.read_append(&mut file, path, pid, started, since, &before) {
            Some(facts) => facts,
            None => self.read_fresh(&mut file, path, pid, started, since, &before)?,
        };
        if let (Some(key), Some(after)) = (key, file.metadata().ok())
            && TailKey::of(path, pid, started, since, &after).as_ref() == Some(&key)
        {
            self.entry = Some((key, facts.clone()));
        }
        Some(facts)
    }

    fn read_fresh(
        &mut self,
        file: &mut File,
        path: &Path,
        pid: u32,
        started: Option<u64>,
        since: u64,
        meta: &Metadata,
    ) -> Option<TailRead> {
        #[cfg(unix)]
        {
            self.carry = None;
        }
        let len = meta.len();
        let mut facts = TailFacts::default();
        let mut resumed = false;
        let mut cwd = None;
        let mut facts_done = false;
        let mut excerpts = super::excerpt::Scan::default();
        let mut hi = len;
        let mut cut: Vec<u8> = Vec::new();
        let mut read_to: Option<u64> = None;
        let from_start = loop {
            let lo = hi.saturating_sub(TAIL_BYTES);
            let window = read_range_then(file, lo, hi, &cut)?;
            let own = window.len() - cut.len();
            let from = if lo > 0 {
                window
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(window.len(), |nl| nl + 1)
            } else {
                0
            };
            if read_to.is_none()
                && let Some(nl) = window[..own].iter().rposition(|&b| b == b'\n')
            {
                read_to = Some(lo + nl as u64 + 1);
            }
            let rows: Vec<&str> = rows_of(&window[from..]).collect();
            if !facts_done {
                let (older, floored) = scan(&rows, Some(since));
                facts = facts.over(older);
                resumed |= floored;
                if cwd.is_none() {
                    cwd = last_cwd_since(rows.iter().copied(), Some(since));
                }
                facts_done = floored || facts.settled();
            }
            if !excerpts.settled() {
                excerpts = excerpts.over(super::excerpt::Scan::of(&rows));
            }
            cut = if from > MAX_LINE_BYTES {
                Vec::new()
            } else {
                window[..from].to_vec()
            };
            if (facts_done && excerpts.settled()) || lo == 0 || len - lo >= MAX_BACK_BYTES {
                break lo == 0;
            }
            hi = lo;
        };
        let read = TailRead {
            facts,
            resumed,
            cwd,
            excerpts,
        };
        #[cfg(unix)]
        if let Some(read_to) = read_to.or(from_start.then_some(0)) {
            use std::os::unix::fs::MetadataExt as _;
            let g = read_to.min(GUARD_BYTES);
            if let Some(guard) = read_range(file, read_to - g, read_to) {
                self.carry = Some(Carry {
                    path: path.to_owned(),
                    pid,
                    started,
                    since,
                    dev: meta.dev(),
                    ino: meta.ino(),
                    read_to,
                    guard: guard_hash(&guard),
                    facts: read.clone(),
                });
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (path, pid, started, read_to, from_start);
        }
        Some(read)
    }

    /// The rows appended since the carried read, on top of what it carried —
    /// or `None` when this is not that file of that process grown (read it
    /// afresh).
    #[cfg(unix)]
    fn read_append(
        &mut self,
        file: &mut File,
        path: &Path,
        pid: u32,
        started: Option<u64>,
        since: u64,
        meta: &Metadata,
    ) -> Option<TailRead> {
        use std::os::unix::fs::MetadataExt as _;
        let carry = self.carry.as_mut()?;
        let len = meta.len();
        if carry.path != path
            || carry.pid != pid
            || carry.started != started
            || carry.since != since
            || carry.dev != meta.dev()
            || carry.ino != meta.ino()
            || len <= carry.read_to
            || len - carry.read_to > MAX_APPEND_BYTES
        {
            return None;
        }
        let g = carry.read_to.min(GUARD_BYTES);
        let bytes = read_range(file, carry.read_to - g, len)?;
        let g = usize::try_from(g).ok()?;
        if bytes.len() < g || guard_hash(&bytes[..g]) != carry.guard {
            return None;
        }
        let appended = &bytes[g..];
        let whole = appended
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |nl| nl + 1);
        let rows: Vec<&str> = rows_of(&appended[..whole]).collect();
        let (newer, floored) = scan(&rows, Some(since));
        let cwd = last_cwd_since(rows.iter().copied(), Some(since));
        let excerpts =
            super::excerpt::Scan::of(&rows).over(std::mem::take(&mut carry.facts.excerpts));
        let facts = if floored {
            TailRead {
                facts: newer,
                resumed: true,
                cwd,
                excerpts,
            }
        } else {
            TailRead {
                facts: newer.over(std::mem::take(&mut carry.facts.facts)),
                resumed: carry.facts.resumed,
                cwd: cwd.or_else(|| carry.facts.cwd.take()),
                excerpts,
            }
        };
        let end = g + whole;
        carry.read_to += whole as u64;
        carry.guard = guard_hash(&bytes[end.saturating_sub(GUARD_BYTES as usize)..end]);
        carry.facts = facts.clone();
        #[cfg(test)]
        {
            self.appends += 1;
        }
        Some(facts)
    }

    #[cfg(not(unix))]
    fn read_append(
        &mut self,
        _file: &mut File,
        _path: &Path,
        _pid: u32,
        _started: Option<u64>,
        _since: u64,
        _meta: &Metadata,
    ) -> Option<TailRead> {
        None
    }

    /// Forget every transcript read: the next read is a fresh one.
    pub fn clear(&mut self) {
        self.entry = None;
        self.excerpts = None;
        self.len = None;
        #[cfg(unix)]
        {
            self.carry = None;
        }
    }

    /// The survey's excerpts of the transcript the last read read ([`super::excerpt`]).
    #[must_use]
    pub const fn excerpts(&self) -> Option<&super::excerpt::Excerpts> {
        self.excerpts.as_ref()
    }

    /// How long the transcript the last read opened was, in bytes; `None` when it opened none.
    #[must_use]
    pub const fn transcript_len(&self) -> Option<u64> {
        self.len
    }

    fn process_of(&self, pid: u32, started: Option<u64>, floor: u64) -> Option<&ProcessFacts> {
        self.process
            .as_ref()
            .filter(|p| p.pid == pid && p.started == started && p.floor == floor)
    }

    fn session_floor(&self, pid: u32, started: Option<u64>, floor: u64, session: &str) -> u64 {
        match self.process_of(pid, started, floor) {
            Some(p) if p.session == session => p.session_floor,
            Some(p) => p.seen.max(floor),
            None => floor,
        }
    }

    /// `tail` (what `session` says since its [`Self::session_floor`]) over what
    /// this process last decided in any session, by the rules of [`TailCache`]:
    /// on a switch the session left is read again ([`Switch::left`]), and an
    /// unpinned model takes the restored one ([`Switch::restored`]), the
    /// process's effort standing only for that model.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClaudeFooterModel",
            action = "Read",
            project = "conformance_footer_model::project"
        )
    )]
    fn for_process(
        &mut self,
        pid: u32,
        started: Option<u64>,
        floor: u64,
        session: &str,
        tail: TailFacts,
        switch: Switch<
            impl FnOnce(&str, u64) -> (TailFacts, bool),
            impl FnOnce(Option<u64>) -> Option<Said>,
        >,
    ) -> TailFacts {
        let session_floor = self.session_floor(pid, started, floor, session);
        let switch_now = switch.now;
        let (facts, pinned) = match self
            .process
            .take()
            .filter(|p| p.pid == pid && p.started == started && p.floor == floor)
        {
            Some(before) if before.session == session => (tail.over(before.facts), before.pinned),
            Some(before) => {
                let (left, chose_there) = (switch.left)(&before.session, before.session_floor);
                let pinned = before.pinned || chose_there;
                let process = left.over(before.facts);
                let restored = (!pinned && !switch.launch_pinned)
                    .then(|| (switch.restored)(Some(session_floor)))
                    .flatten();
                let pinned = pinned || restored.is_some();
                let process = match restored {
                    Some(model) => restored_facts(model).over(process),
                    None => process,
                };
                (tail.over(process), pinned)
            }
            None => {
                let restored = (!switch.launch_pinned)
                    .then(|| (switch.restored)(None))
                    .flatten();
                let pinned = restored.is_some();
                let shown = restored.filter(|_| switch.launch_resumed);
                (
                    match shown {
                        Some(model) => tail.over(restored_facts(model)),
                        None => tail,
                    },
                    pinned,
                )
            }
        };
        self.process = Some(ProcessFacts {
            pid,
            started,
            floor,
            session: session.to_owned(),
            session_floor,
            seen: switch_now,
            facts: facts.clone(),
            pinned,
        });
        facts
    }
}

fn restored_facts(model: Said) -> TailFacts {
    let effort_for = model.shown().map(str::to_owned);
    TailFacts {
        effort: if effort_for.is_some() {
            Said::Unsaid
        } else {
            Said::Unread
        },
        model,
        effort_for,
        top: None,
    }
}

struct Switch<L, R> {
    /// When this read happened (unix seconds, the wall clock): from now on,
    /// when the process was last seen in the current session.
    now: u64,
    /// The session left (its id), re-read since its session floor: what it
    /// decided, and whether it holds a model choice ([`chose_model_since`]).
    left: L,
    /// The process's launch pins its model ([`LaunchFacts::pinned`]).
    launch_pinned: bool,
    /// The process's launch resumed the current session
    /// ([`LaunchFacts::resume`]: it names this session, or names none).
    launch_resumed: bool,
    /// When the current session is a conversation that predates the process
    /// taking it up, the model Claude restores from it; `None` for no session
    /// or no answer.
    restored: R,
}

/// One row [`rows_back`] meets.
#[derive(Clone, Copy)]
enum Row<'a> {
    /// A whole row of at most [`MAX_LINE_BYTES`].
    Whole(&'a [u8]),
    /// A row longer than [`MAX_LINE_BYTES`], met once, by the part the read
    /// holds: always its end (where Claude writes `type` and `timestamp`), and
    /// its start too when one window held it whole.
    Oversized(&'a [u8]),
}

fn rows_back(
    path: &Path,
    mut visit: impl FnMut(Row<'_>) -> std::ops::ControlFlow<()>,
) -> Option<bool> {
    let mut file = open_regular(path)?;
    let len = file.metadata().ok()?.len();
    let mut hi = len;
    let mut cut: Vec<u8> = Vec::new();
    let mut inside = false;
    loop {
        let lo = hi.saturating_sub(TAIL_BYTES);
        let window = read_range_then(&mut file, lo, hi, &cut)?;
        let first_nl = window.iter().position(|&b| b == b'\n');
        let from = if lo > 0 {
            first_nl.map_or(window.len(), |nl| nl + 1)
        } else {
            0
        };
        let mut rows = window[from..].rsplit(|&b| b == b'\n');
        if inside && (lo == 0 || first_nl.is_some()) {
            rows.next();
            inside = false;
        }
        for raw in rows {
            let row = match raw.len() {
                0 => continue,
                n if n > MAX_LINE_BYTES => Row::Oversized(raw),
                _ => Row::Whole(raw),
            };
            if visit(row).is_break() {
                return Some(false);
            }
        }
        cut = if from > MAX_LINE_BYTES {
            // The end of a row longer than any this reads, whose start is in
            // an older window: met now, in its place, and once.
            if !inside {
                inside = true;
                if visit(Row::Oversized(&window[..from])).is_break() {
                    return Some(false);
                }
            }
            Vec::new()
        } else {
            window[..from].to_vec()
        };
        if lo == 0 || len - lo >= MAX_BACK_BYTES {
            return Some(lo == 0);
        }
        hi = lo;
    }
}

fn chose_model_since(path: &Path, floor: u64) -> bool {
    use crate::harness::upgrade_models::{ModelResultKind, model_result_of};
    use std::ops::ControlFlow::{Break, Continue};
    let mut chose = false;
    rows_back(path, |row| {
        let Row::Whole(raw) = row else {
            return Continue(());
        };
        let Ok(line) = std::str::from_utf8(raw) else {
            return Continue(());
        };
        if !line.contains(MODEL_COMMAND) && !line.contains(COMMAND_STDOUT) {
            return Continue(());
        }
        let Some(obj) = aterm_json::from_str::<Value>(line)
            .ok()
            .filter(|v| v.get("isSidechain").and_then(Value::as_bool) != Some(true))
        else {
            return Continue(());
        };
        match obj
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(crate::harness::upgrade_models::parse_utc)
        {
            Some(at) if at < floor => return Break(()),
            Some(_) => {}
            None => return Continue(()),
        }
        let content = (obj.get("type").and_then(Value::as_str) == Some("user"))
            .then(|| {
                obj.get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_str)
            })
            .flatten()
            .map(str::trim_start);
        chose = content.is_some_and(|c| {
            c.starts_with(MODEL_COMMAND)
                || model_result_of(c).is_some_and(|r| r.kind != ModelResultKind::Kept)
        });
        if chose { Break(()) } else { Continue(()) }
    });
    chose
}

const HEAD_BYTES: u64 = 64 * 1024;

fn head_stamp(file: &mut File, own_rows: bool) -> Option<u64> {
    let head = read_range(file, 0, HEAD_BYTES)?;
    let whole = if head.len() as u64 == HEAD_BYTES {
        head.iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |nl| nl + 1)
    } else {
        head.len()
    };
    head[..whole]
        .split(|&b| b == b'\n')
        .filter(|raw| !raw.is_empty())
        .find_map(|raw| {
            let value = aterm_json::from_str::<Value>(std::str::from_utf8(raw).ok()?).ok()?;
            let at = value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::harness::upgrade_models::parse_utc)?;
            let message = matches!(
                value.get("type").and_then(Value::as_str),
                Some("user" | "assistant" | "system")
            );
            let not_own = !message
                || value.get("forkedFrom").is_some()
                || value.get("isSidechain").and_then(Value::as_bool) == Some(true);
            (!own_rows || !not_own).then_some(at)
        })
}

fn began_before(path: &Path, floor: u64) -> bool {
    open_regular(path)
        .and_then(|mut file| head_stamp(&mut file, false))
        .is_some_and(|at| at < floor)
}

fn existed_before(path: &Path, moment: u64) -> bool {
    let Some(mut file) = open_regular(path) else {
        return false;
    };
    let born = file
        .metadata()
        .ok()
        .and_then(|meta| meta.created().ok())
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map(|at| at.as_secs());
    born.is_some_and(|at| at < moment) || head_stamp(&mut file, true).is_some_and(|at| at < moment)
}

fn last_stamp(part: &[u8]) -> Option<u64> {
    const KEY: &[u8] = b"\"timestamp\":\"";
    let at = part.windows(KEY.len()).rposition(|w| w == KEY)? + KEY.len();
    let rest = &part[at..];
    let end = rest.iter().position(|&b| b == b'"')?;
    std::str::from_utf8(&rest[..end])
        .ok()
        .and_then(crate::harness::upgrade_models::parse_utc)
}

fn resumed_model(path: &Path, floor: u64, met_before: bool) -> Option<Said> {
    (met_before || began_before(path, floor))
        .then(|| conversation_model(path, floor))
        .flatten()
}

fn resumed_since(path: &Path, moment: u64) -> Option<Said> {
    existed_before(path, moment)
        .then(|| conversation_model(path, moment))
        .flatten()
}

fn conversation_model(path: &Path, before: u64) -> Option<Said> {
    use std::ops::ControlFlow::{Break, Continue};
    let mut found = None;
    let whole = rows_back(path, |row| {
        let raw = match row {
            Row::Whole(raw) => raw,
            Row::Oversized(part) => {
                const ANSWER: &[u8] = b"\"assistant\"";
                if !part.windows(ANSWER.len()).any(|w| w == ANSWER) {
                    return Continue(());
                }
                if last_stamp(part).is_some_and(|at| at >= before) {
                    return Continue(());
                }
                found = Some(Said::Unread);
                return Break(());
            }
        };
        let Ok(line) = std::str::from_utf8(raw) else {
            return Continue(());
        };
        if !line.contains("\"assistant\"") {
            return Continue(());
        }
        let Ok(value) = aterm_json::from_str::<Value>(line) else {
            return Continue(());
        };
        if value.get("type").and_then(Value::as_str) != Some("assistant")
            || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || value.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            return Continue(());
        }
        let Some(model) = value
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .filter(|m| !m.starts_with('<'))
        else {
            return Continue(());
        };
        let at = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(crate::harness::upgrade_models::parse_utc);
        found = Some(match at {
            Some(at) if at >= before => return Continue(()),
            Some(_) if is_word(model) => Said::Is(model_display(model)),
            _ => Said::Unread,
        });
        Break(())
    });
    match (found, whole) {
        (Some(said), _) => Some(said),
        (None, Some(true)) => None,
        (None, _) => Some(Said::Unread),
    }
}

/// The directories beside one read's facts ([`FooterCache::dirs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactDirs {
    /// Where the session's Bash tool stands, else its launch directory.
    pub cwd: PathBuf,
    /// Its repository's root ([`GitHeadRead::Found`]).
    pub repo_root: Option<PathBuf>,
}

/// The resolver's per-session usage fold ([`super::session_usage::SessionUsage`]).
#[derive(Debug)]
pub struct FooterCache {
    usage: Option<super::session_usage::SessionUsage>,
    /// The last refresh of `usage`, reused while none of its files moved.
    refreshed: Option<super::session_usage::Refresh>,
    /// The limit-wall inputs (limit-notice rows and served time) as of the
    /// last read that caught up; a prefix may hold a notice already lifted.
    wall_basis: Option<WallBasis>,
    /// Transcript bytes one read folds at most ([`FOLD_BUDGET`]).
    budget: u64,
    /// The session log's step tap on the main transcript
    /// ([`super::steps::StepFold`]): `None` until the glue arms it
    /// ([`Self::set_floor`]); it outlives a moved path, which rebinds it.
    steps: Option<super::steps::StepFold>,
    /// Whether the last registry read's transcript path was certain
    /// ([`transcript_lookup`]): an uncertain one's steps are partial.
    steps_certain: bool,
    /// The directories the last read that found the process stood in
    /// ([`Self::dirs`]).
    dirs: Option<FactDirs>,
    /// How many folds this cache has dropped: a new fold's generations start
    /// over, so [`Self::traffic_generation`] names the fold too.
    dropped: u64,
}

impl Default for FooterCache {
    fn default() -> Self {
        Self {
            usage: None,
            refreshed: None,
            wall_basis: None,
            budget: FOLD_BUDGET,
            steps: None,
            steps_certain: true,
            dirs: None,
            dropped: 0,
        }
    }
}

type WallBasis = (
    std::collections::BTreeMap<String, super::usage::LimitNoticeRow>,
    Option<i64>,
);

impl FooterCache {
    /// The usage of the conversation in `transcript` (the one the same registry read named, [`read_pid`]), folded on from the last read unless no file moved.
    pub fn usage(
        &mut self,
        transcript: Option<PathBuf>,
        now: i64,
        offset_at: &dyn Fn(Option<&str>, i64) -> Option<i64>,
    ) -> Option<super::session_usage::UsageFacts> {
        let Some(path) = transcript else {
            self.forget();
            return None;
        };
        if self.usage.as_ref().is_none_or(|u| u.transcript() != path) {
            self.forget();
            self.usage = Some(super::session_usage::SessionUsage::new(path));
        }
        if let Some(usage) = self.usage.as_mut() {
            let refresh = match self.refreshed {
                Some(last) if !usage.moved() => last,
                _ => usage.refresh_tapped(self.budget, self.steps.as_mut()),
            };
            self.refreshed = Some(refresh);
            if refresh.caught_up {
                let notices = usage.main().fold().limit_notices().clone();
                self.wall_basis = Some((notices, usage.served_at()));
            }
        }
        self.shown(now, offset_at)
    }

    fn shown(
        &self,
        now: i64,
        offset_at: &dyn Fn(Option<&str>, i64) -> Option<i64>,
    ) -> Option<super::session_usage::UsageFacts> {
        use super::session_usage::{UsageFacts, model_tokens, wall_of};
        let usage = self.usage.as_ref()?;
        let refresh = self.refreshed?;
        let wall = self
            .wall_basis
            .as_ref()
            .and_then(|(rows, served)| wall_of(rows, *served, now, offset_at));
        let (models, busy_ms) = if refresh.caught_up {
            (
                model_tokens(usage.total_fold().per_model()),
                usage.busy_ms(),
            )
        } else {
            ((Vec::new(), super::session_usage::More::default()), 0)
        };
        UsageFacts::of(models, wall, Some(&refresh)).map(|facts| facts.with_runtime(busy_ms))
    }

    /// ARM the step tap: rows of the main transcript older than `floor_ms` (wall ms: the process's start, `floor_of` × 1000) or at or below the log's watermark `given` for their file are history, folded into the end and never queued.
    pub fn set_floor(&mut self, floor_ms: u64, given: Option<super::steps::StepMark>) {
        match self.steps.as_mut() {
            Some(tap) => tap.set_floor(floor_ms, given),
            None => {
                let mut tap = super::steps::StepFold::new(floor_ms, given);
                if let Some(usage) = self.usage.as_ref() {
                    usage.bind_tap(&mut tap);
                }
                self.steps = Some(tap);
            }
        }
    }

    /// Everything the step tap queued since the last take, and the main chain's end.
    pub fn take_steps(&mut self) -> super::steps::StepBatch {
        let mut batch = self
            .steps
            .as_mut()
            .map(super::steps::StepFold::take)
            .unwrap_or_default();
        if !self.steps_certain {
            for step in &mut batch.steps {
                step.state = super::steps::StepState::Partial;
            }
        }
        batch
    }

    /// Whether the step tap is armed ([`Self::set_floor`]).
    #[must_use]
    pub const fn steps_armed(&self) -> bool {
        self.steps.is_some()
    }

    /// DISARM the step tap: what it queued and its binding go.
    pub fn disarm_steps(&mut self) {
        self.steps = None;
    }

    /// Whether the armed step tap has bound a file.
    #[must_use]
    pub fn steps_bound(&self) -> bool {
        self.steps
            .as_ref()
            .is_some_and(super::steps::StepFold::is_bound)
    }

    /// What one registry read says of the tap ([`read_pid`]).
    pub fn note_entry(
        &mut self,
        floor_ms: Option<u64>,
        session: &str,
        found: Option<(&Path, bool)>,
    ) {
        if let Some(tap) = self.steps.as_mut() {
            if let Some(floor) = floor_ms {
                tap.raise_floor(floor);
            }
            let sid = aterm_hash::fnv1a_64(session.as_bytes());
            match found {
                Some((path, _)) => tap.note_found(sid, super::session_usage::path_fp(path)),
                None => tap.note_vacant(sid),
            }
        }
        self.steps_certain = found.is_some_and(|(_, certain)| certain);
    }

    /// The directories the last read that found the process stood in ([`read_pid`]).
    #[must_use]
    pub const fn dirs(&self) -> Option<&FactDirs> {
        self.dirs.as_ref()
    }

    /// The session's traffic over time ([`super::session_usage::SessionUsage::traffic`]), only once the fold has caught up.
    #[must_use]
    pub fn traffic(&self) -> Option<super::session_usage::TrafficView> {
        self.caught_up_usage()
            .map(super::session_usage::SessionUsage::traffic)
    }

    /// The session's agents and workflow runs with what their files say of their calls in flight (`session_usage::SessionUsage::flights`), under [`Self::traffic`]'s rule.
    #[must_use]
    pub fn flights(&self) -> Option<super::flight::FlightTable> {
        self.caught_up_usage()
            .map(super::session_usage::SessionUsage::flights)
    }

    /// What moves whenever [`Self::flights`] would (`None` until caught up).
    #[must_use]
    pub fn flight_generation(&self) -> Option<(u64, u64)> {
        self.caught_up_usage()
            .map(|usage| (self.dropped, usage.flight_generation()))
    }

    /// What moves whenever [`Self::traffic`] would, under the same rule.
    #[must_use]
    pub fn traffic_generation(&self) -> Option<(u64, u64)> {
        self.caught_up_usage()
            .map(|usage| (self.dropped, usage.traffic_generation()))
    }

    fn caught_up_usage(&self) -> Option<&super::session_usage::SessionUsage> {
        self.refreshed
            .filter(|refresh| refresh.caught_up)
            .and(self.usage.as_ref())
    }

    fn forget(&mut self) {
        if self.usage.take().is_some() {
            self.dropped = self.dropped.wrapping_add(1);
        }
        self.refreshed = None;
        self.wall_basis = None;
    }
}

fn read_tail(path: &Path) -> Option<Vec<u8>> {
    let mut file = open_regular(path)?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    let mut bytes = read_range(&mut file, start, len)?;
    if start > 0 {
        let first = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |nl| nl + 1);
        bytes.drain(..first);
    }
    Some(bytes)
}

fn read_range(file: &mut File, from: u64, to: u64) -> Option<Vec<u8>> {
    read_range_then(file, from, to, &[])
}

fn read_range_then(file: &mut File, from: u64, to: u64, then: &[u8]) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(from)).ok()?;
    let want = to.saturating_sub(from);
    let room = usize::try_from(want)
        .unwrap_or(0)
        .saturating_add(then.len());
    let mut bytes = Vec::with_capacity(room);
    file.take(want).read_to_end(&mut bytes).ok()?;
    bytes.extend_from_slice(then);
    Some(bytes)
}

const MAX_SESSION_FILES: usize = 256;

/// Where the Bash tool of each live Claude Code session launched in `launch` stands, per its transcript.
pub fn shell_cwds(claude_dir: &Path, launch: &Path) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(claude_dir.join("sessions")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for file in dir.filter_map(Result::ok).take(MAX_SESSION_FILES) {
        let name = file.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".json"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(entry) = read_small(&file.path(), MAX_SESSION_FILE_BYTES)
            .and_then(|text| parse_session_entry(&text, pid, None))
        else {
            continue;
        };
        if entry.cwd != launch || !super::upgrade_drive::alive(pid) {
            continue;
        }
        if let Some(cwd) = transcript_path(claude_dir, &entry)
            .and_then(|path| read_tail(&path))
            .and_then(|body| last_cwd(&body))
            && cwd != launch
            && !out.contains(&cwd)
        {
            out.push(cwd);
        }
    }
    out
}

/// The `cwd` of the newest main-thread row that carries one.
pub fn last_cwd(body: &[u8]) -> Option<PathBuf> {
    last_cwd_since(rows_of(body), None)
}

fn last_cwd_since<'a>(
    rows: impl IntoIterator<Item = &'a str>,
    since: Option<u64>,
) -> Option<PathBuf> {
    for line in rows {
        if !line.contains("\"cwd\"") {
            continue;
        }
        let Ok(value) = super::transcript::metadata(line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        if obj.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(since) = since {
            match obj
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::harness::upgrade_models::parse_utc)
            {
                Some(at) if at < since => return None,
                Some(_) => {}
                None => continue,
            }
        }
        if let Some(cwd) = obj.get("cwd").and_then(Value::as_str) {
            let cwd = PathBuf::from(cwd);
            if cwd.is_absolute() {
                return Some(cwd);
            }
        }
    }
    None
}

/// What the transcript since the process started says about one of the footer's facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Said {
    /// Nothing since the start names it: the launch flag or the launch card may.
    #[default]
    Unsaid,
    /// The newest statement is one this build cannot read.
    Unread,
    /// The newest statement names it, as the footer shows it.
    Is(String),
}

impl Said {
    const fn decided(&self) -> bool {
        !matches!(self, Self::Unsaid)
    }

    /// The value to show, when the newest statement names one.
    #[must_use]
    pub fn shown(&self) -> Option<&str> {
        match self {
            Self::Is(value) => Some(value),
            Self::Unsaid | Self::Unread => None,
        }
    }
}

/// What the end of a transcript says about the model and effort.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailFacts {
    /// The model: the newest of an answer's `message.model` and a model result's name (`Opus 5.5`), as the footer shows it.
    pub model: Said,
    /// The effort: the newest of an answer's `effort`, an `/effort` result and a model result's `with … effort`.
    pub effort: Said,
    /// A model switch that named no effort while the effort was open, and the
    /// model switched to.
    effort_for: Option<String>,
    /// The top-effort key ([`CLAUDE_TOP_EFFORT_KEY`]) on or off, from the newest statement of it.
    pub top: Option<bool>,
}

impl TailFacts {
    fn over(self, older: Self) -> Self {
        let (effort, effort_for) = match self.effort_for {
            None if self.effort.decided() => (self.effort, None),
            None => (older.effort, older.effort_for),
            Some(to) => match &older.model {
                Said::Unsaid => (
                    if self.effort.decided() {
                        self.effort
                    } else {
                        older.effort
                    },
                    Some(to),
                ),
                Said::Is(before) if *before == to => {
                    if self.effort.decided() {
                        (self.effort, None)
                    } else {
                        (older.effort, older.effort_for)
                    }
                }
                Said::Is(_) | Said::Unread => (Said::Unread, None),
            },
        };
        let model = if self.model.decided() {
            self.model
        } else {
            older.model
        };
        Self {
            model,
            effort,
            effort_for,
            top: self.top.or(older.top),
        }
    }

    const fn settled(&self) -> bool {
        self.model.decided() && self.effort.decided() && self.effort_for.is_none()
    }

    fn meet_model(&mut self, said: Said) {
        if let Some(to) = self.effort_for.take()
            && said != Said::Is(to)
        {
            self.effort = Said::Unread;
        }
        if !self.model.decided() {
            self.model = said;
        }
    }

    fn decide_effort(&mut self, said: Said) {
        if said == Said::Unread {
            self.effort_for = None;
        }
        self.effort = said;
    }
}

const MODEL_COMMAND: &str = "<command-name>/model</command-name>";

const EFFORT_COMMAND: &str = "<command-name>/effort</command-name>";

const COMMAND_NAME: &str = "<command-name>";

const COMMAND_STDOUT: &str = "<local-command-stdout>";

/// The effort levels the footer names, as `/effort` takes them.
#[must_use]
pub fn effort_level(word: &str) -> Option<&'static str> {
    match word {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | CLAUDE_TOP_EFFORT_KEY => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

/// A model as a result or launch card spells it, as the footer shows it.
#[must_use]
pub fn footer_model_name(display: &str) -> Option<String> {
    let mut name = display.trim();
    loop {
        let before = name;
        for suffix in [" (default)", " (1M context)", " (recommended)"] {
            if let Some(rest) = name.strip_suffix(suffix) {
                name = rest.trim_end();
            }
        }
        if name == before {
            break;
        }
    }
    if name.starts_with("claude-") && is_word(name) {
        return Some(model_display(name));
    }
    let plain = (1..=64).contains(&name.len())
        && !name.contains("  ")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " .-_()[]".contains(c));
    plain.then(|| name.to_owned())
}

/// What an `/effort` result says ([`effort_result`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum EffortSaid {
    /// A level set or shown (`Set effort level to …`, `Current effort level:
    /// …`, `Effort level set to auto`), and the top-effort key's state when
    /// the result ends with ` · <Key> on|off`.
    Level { said: Said, top: Option<bool> },
    /// The key's own `/effort <key> on|off` result: on or off for this
    /// session, and the level that stays.
    Top { on: bool, stays: Said },
    /// The key refused (`<Key> needs dynamic workflows enabled …`, `<Key>
    /// isn't available on <model>. …`): it answers its command and decides
    /// neither the effort nor the key.
    Refused,
}

impl EffortSaid {
    fn effort(&self) -> Option<Said> {
        match self {
            Self::Level { said, .. } => Some(said.clone()),
            Self::Top { stays, .. } => Some(stays.clone()),
            Self::Refused => None,
        }
    }

    const fn top(&self) -> Option<bool> {
        match self {
            Self::Level { top, .. } => *top,
            Self::Top { on, .. } => Some(*on),
            Self::Refused => None,
        }
    }
}

fn past_key(text: &str) -> Option<&str> {
    let key = CLAUDE_TOP_EFFORT_KEY.as_bytes();
    text.as_bytes()
        .get(..key.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(key))
        .then(|| &text[key.len()..])
}

fn key_suffix(body: &str) -> Option<bool> {
    let (_, last) = body.rsplit_once(" \u{b7} ")?;
    let rest = past_key(last)?;
    let word = |w: &str| {
        rest.strip_prefix(w)
            .is_some_and(|after| !after.starts_with(|c: char| c.is_ascii_alphanumeric()))
    };
    if word(aterm_phase::anchor("effort.key_on")) {
        Some(true)
    } else if word(aterm_phase::anchor("effort.key_off")) {
        Some(false)
    } else {
        None
    }
}

fn level_word(text: &str) -> Said {
    let word: String = text
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    effort_level(&word).map_or(Said::Unread, |level| Said::Is(level.to_owned()))
}

fn effort_result(content: &str) -> Option<EffortSaid> {
    use aterm_phase::anchor;
    let body = content
        .trim_start()
        .strip_prefix(COMMAND_STDOUT)?
        .trim_start();
    let body = crate::harness::upgrade_models::strip_sgr(body);
    if let Some(rest) = past_key(&body) {
        if let Some(after) = rest.strip_prefix(anchor("effort.top_on")) {
            let stays = anchor("effort.stays");
            let stays = after
                .find(stays)
                .map_or(Said::Unread, |at| level_word(&after[at + stays.len()..]));
            return Some(EffortSaid::Top { on: true, stays });
        }
        if let Some(after) = rest.strip_prefix(anchor("effort.top_off")) {
            return Some(EffortSaid::Top {
                on: false,
                stays: level_word(after),
            });
        }
        if rest.starts_with(anchor("effort.top_needs"))
            || rest.starts_with(anchor("effort.top_unavailable"))
        {
            return Some(EffortSaid::Refused);
        }
    }
    let top = key_suffix(&body);
    if body.starts_with(anchor("effort.auto")) {
        return Some(EffortSaid::Level {
            said: Said::Unread,
            top,
        });
    }
    ["effort.set", "effort.current"].into_iter().find_map(|id| {
        let rest = body.strip_prefix(anchor(id))?;
        Some(EffortSaid::Level {
            said: level_word(rest),
            top,
        })
    })
}

/// Which facts a local command's result row answered ([`fold_result`]), so
/// [`scan`] can pair a command row with its own result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Answered {
    /// A readable model was named.
    model: bool,
    /// It is an `/effort` result, a refused key included.
    effort: bool,
}

/// A local command's result row folded into `facts`, newest first. A model
/// result ([`crate::harness::upgrade_models::model_result_of`]) names the model
/// ([`TailFacts::meet_model`]) and any `with … effort`; without that clause the
/// earlier effort stands only for the same model.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ClaudeFooterModel",
        action = "Read",
        project = "conformance_footer_model::project"
    )
)]
fn fold_result(content: &str, facts: &mut TailFacts) -> Answered {
    use crate::harness::upgrade_models::model_result_of;
    if let Some(result) = model_result_of(content) {
        let model = result
            .display
            .as_deref()
            .and_then(footer_model_name)
            .map_or(Said::Unread, Said::Is);
        let readable = matches!(model, Said::Is(_));
        facts.meet_model(model.clone());
        if !facts.effort.decided() {
            match (result.effort.as_deref(), model) {
                (Some(word), _) => facts.decide_effort(
                    effort_level(word).map_or(Said::Unread, |level| Said::Is(level.to_owned())),
                ),
                (None, Said::Is(to)) => facts.effort_for = Some(to),
                (None, _) => facts.decide_effort(Said::Unread),
            }
        }
        return Answered {
            model: readable,
            effort: false,
        };
    }
    let Some(said) = effort_result(content) else {
        return Answered::default();
    };
    if facts.top.is_none() {
        facts.top = said.top();
    }
    if !facts.effort.decided()
        && let Some(effort) = said.effort()
    {
        facts.decide_effort(effort);
    }
    Answered {
        model: false,
        effort: true,
    }
}

/// [`read_tail_facts`]'s scan, pure: rows newest first, stopping once both facts are decided.
pub fn tail_facts(body: &[u8], since: Option<u64>) -> TailFacts {
    scan(&rows_of(body).collect::<Vec<_>>(), since).0
}

fn rows_of(body: &[u8]) -> impl Iterator<Item = &str> {
    body.rsplit(|&b| b == b'\n')
        .filter(|raw| !raw.is_empty() && raw.len() <= MAX_LINE_BYTES)
        .filter_map(|raw| std::str::from_utf8(raw).ok())
}

/// [`tail_facts`] of `rows` ([`rows_of`]), and whether the scan stopped at a
/// row before the floor.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ClaudeFooterModel",
        action = "Read",
        project = "conformance_footer_model::project"
    )
)]
fn scan(rows: &[&str], since: Option<u64>) -> (TailFacts, bool) {
    let mut facts = TailFacts::default();
    let mut under = Answered::default();
    for &line in rows {
        if facts.settled() {
            break;
        }
        let command = line.contains(COMMAND_STDOUT) || line.contains(COMMAND_NAME);
        let model_open = !facts.model.decided() || facts.effort_for.is_some();
        let wants_model = model_open && (command || line.contains("\"assistant\""));
        let wants_effort = !facts.effort.decided() && (command || line.contains("\"effort\""));
        if !wants_model && !wants_effort {
            continue;
        }
        let value = if command {
            super::transcript::command(line)
        } else {
            super::transcript::metadata(line)
        };
        let Ok(value) = value else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        if obj.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(since) = since {
            match obj
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::harness::upgrade_models::parse_utc)
            {
                Some(at) if at < since => return (facts, true),
                Some(_) => {}
                None => continue,
            }
        }
        let kind = obj.get("type").and_then(Value::as_str);
        if command
            && kind == Some("user")
            && let Some(content) = obj
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
        {
            let content = content.trim_start();
            if content.starts_with(COMMAND_STDOUT) {
                under = fold_result(content, &mut facts);
                continue;
            }
            if content.starts_with(COMMAND_NAME) {
                let answered = std::mem::take(&mut under);
                if content.starts_with(MODEL_COMMAND) && !answered.model {
                    facts.meet_model(Said::Unread);
                    if !facts.effort.decided() {
                        facts.decide_effort(Said::Unread);
                    }
                } else if content.starts_with(EFFORT_COMMAND)
                    && !answered.effort
                    && !facts.effort.decided()
                {
                    facts.decide_effort(Said::Unread);
                }
                continue;
            }
        }
        if kind == Some("assistant")
            && let Some(model) = obj
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .filter(|m| !m.starts_with('<') && is_word(m))
        {
            facts.meet_model(Said::Is(model_display(model)));
        }
        if !facts.effort.decided()
            && let Some(effort) = obj.get("effort").and_then(Value::as_str)
            && is_word(effort)
        {
            facts.decide_effort(Said::Is(effort_level(effort).unwrap_or(effort).to_owned()));
        }
    }
    (facts, false)
}

fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '[' | ']'))
}

/// A model id's display name.
pub fn model_display(id: &str) -> String {
    let base = id.split('[').next().unwrap_or(id);
    let Some(rest) = base.strip_prefix("claude-") else {
        return id.to_owned();
    };
    let mut parts = rest.split('-');
    let Some(family) = parts
        .next()
        .filter(|f| f.chars().all(|c| c.is_ascii_alphabetic()))
    else {
        return id.to_owned();
    };
    let version: Vec<&str> = parts
        .filter(|p| p.len() < 8 && p.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let mut name = String::with_capacity(family.len() + 6);
    let mut chars = family.chars();
    if let Some(first) = chars.next() {
        name.extend(first.to_uppercase());
        name.push_str(chars.as_str());
    }
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

/// The branch `cwd` is in, read from `.git` without running `git`.
pub fn git_head(cwd: &Path) -> Option<String> {
    match read_git_head(cwd) {
        GitHeadRead::Found(branch, _) => Some(branch),
        GitHeadRead::Absent | GitHeadRead::Denied => None,
    }
}

/// What [`read_git_head`] found for a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHeadRead {
    /// The branch, and the repository's root: the directory the climb found `.git` in.
    Found(String, PathBuf),
    /// Not inside a repository (or not one this bounded read can parse).
    Absent,
    /// A read failed with `EPERM` (macOS folder privacy consent).
    Denied,
}

fn is_eperm(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Whether `path` is inside a file-provider domain of `home` (iCloud Drive, a third-party sync provider) or another app's data.
pub(crate) fn under_file_provider_domain(home: Option<&Path>, path: &Path) -> bool {
    let Some(home) = home else {
        return false;
    };
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    atpkg::protected::under_file_provider_domain(&home, &resolved)
        || atpkg::protected::under_app_data(&home, &resolved)
}

/// [`git_head`], saying why there is no answer when the kernel refused a read.
pub fn read_git_head(cwd: &Path) -> GitHeadRead {
    read_git_head_in(aterm_types::dirs::home_dir().as_deref(), cwd)
}

fn read_git_head_in(home: Option<&Path>, cwd: &Path) -> GitHeadRead {
    if under_file_provider_domain(home, cwd) {
        return GitHeadRead::Absent;
    }
    let mut dir = cwd;
    for _ in 0..MAX_GIT_CLIMB {
        let dot_git = dir.join(".git");
        match std::fs::metadata(&dot_git) {
            Err(error) if is_eperm(&error) => return GitHeadRead::Denied,
            Err(_) => {}
            Ok(meta) => {
                let Some(git_dir) = resolve_git_dir_of(&dot_git, &meta) else {
                    return GitHeadRead::Absent;
                };
                if under_file_provider_domain(home, &git_dir) {
                    return GitHeadRead::Absent;
                }
                return match read_head(&git_dir.join("HEAD")) {
                    Err(()) => GitHeadRead::Denied,
                    Ok(Some(head)) => branch_of_head(&head).map_or(GitHeadRead::Absent, |branch| {
                        GitHeadRead::Found(branch, dir.to_path_buf())
                    }),
                    Ok(None) => GitHeadRead::Absent,
                };
            }
        }
        let Some(parent) = dir.parent() else {
            return GitHeadRead::Absent;
        };
        dir = parent;
    }
    GitHeadRead::Absent
}

fn read_head(path: &Path) -> Result<Option<String>, ()> {
    match open_nonblocking(path) {
        Ok(file) => Ok(regular(file).and_then(|file| read_capped(file, MAX_GIT_FILE_BYTES))),
        Err(error) if is_eperm(&error) => Err(()),
        Err(_) => Ok(None),
    }
}

fn resolve_git_dir_of(dot_git: &Path, meta: &std::fs::Metadata) -> Option<PathBuf> {
    if meta.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    let text = read_small(dot_git, MAX_GIT_FILE_BYTES)?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        dot_git.parent()?.join(target)
    })
}

/// The branch a `HEAD` file names, pure.
pub fn branch_of_head(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return printable(name);
    }
    (head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| head[..7].to_owned())
}

fn printable(name: &str) -> Option<String> {
    (!name.is_empty() && name.chars().count() <= 128 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

/// The floor under the transcript rows that speak for the process `entry` registers.
#[must_use]
pub fn floor_of(started: Option<u64>, entry: &SessionEntry) -> Option<u64> {
    match (started.or(entry.proc_start), entry.started_at) {
        (Some(process), Some(image)) => Some(process.max(image)),
        (process, image) => process.or(image),
    }
}

/// Everything the footer shows for process `pid` given its registry `entry` ([`session_of_pid`]) and command-line facts.
pub fn facts_for_entry_at(
    claude_dir: &Path,
    pid: u32,
    started: Option<u64>,
    entry: &SessionEntry,
    launch: &LaunchFacts,
    cache: &mut TailCache,
    read_at: SystemTime,
) -> FooterFacts {
    let transcript = floor_of(started, entry).and_then(|_| transcript_path(claude_dir, entry));
    facts_for_entry_from(
        claude_dir,
        pid,
        started,
        entry,
        transcript.as_deref(),
        launch,
        cache,
        read_at,
    )
    .0
}

/// [`facts_for_entry_at`] over the caller's transcript lookup
/// ([`transcript_path`]), shared with the usage fold so both read one file,
/// and the directories beside the facts ([`FactDirs`]).
#[allow(clippy::too_many_arguments)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ClaudeFooterModel",
        action = "Read",
        project = "conformance_footer_model::project"
    )
)]
fn facts_for_entry_from(
    claude_dir: &Path,
    pid: u32,
    started: Option<u64>,
    entry: &SessionEntry,
    transcript: Option<&Path>,
    launch: &LaunchFacts,
    cache: &mut TailCache,
    read_at: SystemTime,
) -> (FooterFacts, FactDirs) {
    let floor = floor_of(started, entry);
    let path = floor.and(transcript);
    let since = floor.map(|floor| cache.session_floor(pid, started, floor, &entry.session_id));
    let read = match (since, path) {
        (Some(since), Some(path)) => cache
            .read_since(path, pid, started, since)
            .unwrap_or_default(),
        _ => {
            cache.clear();
            TailRead::default()
        }
    };
    let met_before = read.resumed;
    let dir = read.cwd.clone().unwrap_or_else(|| entry.cwd.clone());
    let tail = match floor {
        Some(floor) => cache.for_process(
            pid,
            started,
            floor,
            &entry.session_id,
            read.facts,
            Switch {
                now: read_at
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |at| at.as_secs()),
                left: |left: &str, since: u64| {
                    let left = SessionEntry {
                        session_id: left.to_owned(),
                        ..entry.clone()
                    };
                    transcript_path(claude_dir, &left).map_or_else(
                        || (TailFacts::default(), false),
                        |path| {
                            (
                                TailCache::default()
                                    .read(&path, pid, started, since)
                                    .unwrap_or_default(),
                                chose_model_since(&path, since),
                            )
                        },
                    )
                },
                launch_pinned: launch.pinned,
                launch_resumed: match &launch.resume {
                    Some(LaunchResume::Session(id)) => *id == entry.session_id,
                    Some(LaunchResume::Unnamed) => true,
                    None => false,
                },
                restored: |moment: Option<u64>| {
                    path.and_then(|path| match moment {
                        Some(moment) => resumed_since(path, moment),
                        None => resumed_model(path, floor, met_before),
                    })
                },
            },
        ),
        None => read.facts,
    };
    let (branch, repo_read_denied, repo_root) = match read_git_head(&dir) {
        GitHeadRead::Found(branch, root) => (Some(branch), None, Some(root)),
        GitHeadRead::Absent => (None, None, None),
        GitHeadRead::Denied => (None, Some(dir.clone()), None),
    };
    let top_effort = tail.top;
    let (model, model_open) = match tail.model {
        Said::Is(model) => (Some(model), false),
        Said::Unread => (None, false),
        Said::Unsaid => (launch.model.clone(), launch.model.is_none()),
    };
    let (effort, effort_open, effort_for) = match (tail.effort, tail.effort_for) {
        (Said::Is(effort), None) => (Some(effort), false, None),
        (Said::Unread, _) => (None, false, None),
        (Said::Unsaid, None) => (None, true, None),
        (Said::Is(effort), Some(to)) => (
            (launch.model.as_deref() == Some(to.as_str())).then_some(effort),
            false,
            None,
        ),
        (Said::Unsaid, Some(to)) => match launch.model.as_deref() {
            Some(before) if before != to => (None, false, None),
            _ => (None, true, Some(to)),
        },
    };
    let facts = FooterFacts {
        model,
        effort,
        path: home_path(&dir, aterm_types::dirs::home_dir().as_deref()),
        branch,
        repo_read_denied,
        version: entry.version.clone(),
        resume: None,
        usage: None,
        model_open,
        effort_open,
        effort_for,
        owner: Some(FactsOwner {
            floor,
            session_id: entry.session_id.clone(),
        }),
        upgrade: None,
        account: None,
        top_effort,
        off: FactMask::default(),
        subscription: None,
        plan: None,
        context: None,
    };
    (
        facts,
        FactDirs {
            cwd: dir,
            repo_root,
        },
    )
}

/// Claude Code's launch card, read for the footer only under a live REPL ([`live_repl_row`]) and only when it is the newest card with no user message under it.
#[must_use]
pub fn launch_card(rows: &[String]) -> Option<aterm_phase::LaunchCard> {
    launch_card_read(rows).map(|(_, read)| read.card)
}

/// [`launch_card`] with the reader that read it (Claude Code's, agy's or Muse Code's) and the rows it was read from.
#[must_use]
pub fn launch_card_read(
    rows: &[String],
) -> Option<(aterm_phase::Program, aterm_phase::LaunchCardRead)> {
    let named = |read: aterm_phase::LaunchCardRead| {
        let card = read.card;
        Some(aterm_phase::LaunchCardRead {
            card: aterm_phase::LaunchCard {
                model: footer_model_name(&card.model)?,
                effort: card
                    .effort
                    .as_deref()
                    .and_then(effort_level)
                    .map(str::to_owned),
                version: card.version,
            },
            ..read
        })
    };
    if let Some(read) = live_repl_row(rows).and_then(|_| aterm_phase::launch_card_read(rows)) {
        return Some((aterm_phase::Program::Claude, named(read)?));
    }
    if let Some(read) = aterm_phase::agy::launch_card_read(rows) {
        return Some((aterm_phase::Program::Agy, named(read)?));
    }
    let read = aterm_phase::muse::launch_card_read(rows)?;
    Some((aterm_phase::Program::Muse, named(read)?))
}

impl FooterFacts {
    /// These facts filled from `card` (the kept launch card) where still open.
    #[must_use]
    pub fn filled_from(&self, card: Option<&aterm_phase::LaunchCard>) -> Self {
        let mut out = self.clone();
        let (model, effort) = self.filled_model_effort(card);
        if out.model.as_deref() != model {
            out.model = model.map(str::to_owned);
        }
        if out.effort.as_deref() != effort {
            out.effort = effort.map(str::to_owned);
        }
        if out.version.is_none()
            && let Some(card) = card
            && !card.version.is_empty()
        {
            out.version = Some(card.version.clone());
        }
        out
    }

    /// The model and the effort [`Self::filled_from`] shows, borrowed.
    #[must_use]
    pub fn filled_model_effort<'a>(
        &'a self,
        card: Option<&'a aterm_phase::LaunchCard>,
    ) -> (Option<&'a str>, Option<&'a str>) {
        let (mut model, mut effort) = (self.model.as_deref(), self.effort.as_deref());
        if let Some(card) = card.filter(|c| {
            self.version
                .as_deref()
                .is_none_or(|v| v == c.version.as_str())
        }) {
            if self.model_open && model.is_none() {
                model = Some(&card.model);
            }
            if self.effort_open
                && effort.is_none()
                && self.effort_for.as_ref().is_none_or(|to| *to == card.model)
            {
                effort = card.effort.as_deref();
            }
        }
        (model, effort)
    }
}

/// `cwd` as the footer shows it: `~/…` under `home` (unless home is `/`), else whole.
#[must_use]
pub fn home_path(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let home = home.filter(|home| home.parent().is_some());
    let shown = match home.and_then(|home| cwd.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.to_string_lossy()),
        None => cwd.to_string_lossy().into_owned(),
    };
    (!shown.is_empty() && !shown.chars().any(char::is_control)).then_some(shown)
}

/// `path` cut from the front to its last directory (`~/src/aterm` → `…/aterm`), for a pane too narrow for all of it.
#[must_use]
pub fn elide_path(path: &str) -> Option<String> {
    let (_, last) = path.trim_end_matches('/').rsplit_once('/')?;
    let short = format!("\u{2026}/{last}");
    (!last.is_empty() && short.chars().count() < path.chars().count()).then_some(short)
}

/// Timing for one footer read ([`read_pid`]).
pub struct ReadClock<'a> {
    /// No later than the registry read.
    pub read_at: SystemTime,
    /// Unix seconds, for the limit wall.
    pub now: i64,
    /// A zone's offset from UTC at an instant.
    pub offset_at: &'a dyn Fn(Option<&str>, i64) -> Option<i64>,
}

/// One footer read of live Claude Code process `pid`, as `aterm-gui`'s `claude_footer::resolve_into` makes it.
#[allow(clippy::too_many_arguments)]
pub fn read_pid(
    claude_dir: &Path,
    pid: u32,
    started: Option<u64>,
    launch: impl FnOnce(&SessionEntry) -> (LaunchFacts, Vec<String>),
    tail: &mut TailCache,
    fold: &mut FooterCache,
    clock: &ReadClock<'_>,
    state: Option<&Path>,
    user_home: Option<&Path>,
) -> Option<FooterFacts> {
    let Some(entry) = session_of_pid(claude_dir, pid, started) else {
        tail.clear();
        return None;
    };
    let (launch, argv) = launch(&entry);
    let found = transcript_lookup(claude_dir, &entry);
    fold.note_entry(
        floor_of(started, &entry).map(|s| s.saturating_mul(1000)),
        &entry.session_id,
        found
            .as_ref()
            .map(|(path, certain)| (path.as_path(), *certain)),
    );
    let transcript = found.map(|(path, _)| path);
    let (mut facts, dirs) = facts_for_entry_from(
        claude_dir,
        pid,
        started,
        &entry,
        transcript.as_deref(),
        &launch,
        tail,
        clock.read_at,
    );
    fold.dirs = Some(dirs);
    facts.usage = fold.usage(transcript, clock.now, clock.offset_at);
    let home = user_home.map_or_else(Vec::new, |user_home| {
        super::resume::home_env(user_home, launch.config.as_deref(), Some(claude_dir))
    });
    let home: Vec<(&str, &str)> = home.iter().map(|(n, v)| (*n, v.as_str())).collect();
    facts.resume = super::resume::of_entry(&argv, &entry, started, state)
        .and_then(|line| super::resume::with_env(&home, &line));
    Some(facts)
}

/// Where Claude Code's permission-mode row is.
pub fn mode_row(rows: &[String]) -> Option<usize> {
    mode_row_under(rows, aterm_phase::phase::composer_bottom(rows)?)
}

/// [`mode_row`] under the composer bottom rule the caller has found (row `bottom`).
#[must_use]
pub fn mode_row_under(rows: &[String], bottom: usize) -> Option<usize> {
    under_composer(rows, bottom).find(|&i| is_mode_row(&rows[i]))
}

/// The first row under the composer bottom rule `bottom` that opens with a pill glyph ([`opens_with_pill_glyph`]), whether or not this build knows the pill.
#[must_use]
pub fn pill_row_under(rows: &[String], bottom: usize) -> Option<usize> {
    under_composer(rows, bottom).find(|&i| opens_with_pill_glyph(&rows[i]))
}

fn under_composer(rows: &[String], bottom: usize) -> impl Iterator<Item = usize> + '_ {
    (bottom + 1..rows.len()).take_while(|&i| !rows[i].trim().is_empty())
}

/// Where a live REPL's footer row is, under the composer's bottom rule.
#[must_use]
pub fn live_repl_row(rows: &[String]) -> Option<usize> {
    let bottom = aterm_phase::phase::composer_bottom(rows)?;
    mode_row_under(rows, bottom)
        .or_else(|| under_composer(rows, bottom).find(|&i| is_bare_footer_row(&rows[i])))
}

/// Whether Claude Code's screen says its context indicator has GONE (as after `/compact`), for the context fact.
#[must_use]
pub fn context_gone(rows: &[String]) -> bool {
    let Some(bottom) = aterm_phase::phase::composer_bottom(rows) else {
        return false;
    };
    let width = rows[bottom].trim_end().chars().count();
    rows.get(bottom + 1).is_some_and(|row| {
        (is_mode_row(row) || is_bare_footer_row(row)) && row.trim_end().chars().count() + 3 < width
    }) && aterm_phase::phase::context_left_shown(rows).is_none()
        && aterm_phase::prompt::parse_prompt_v2(rows).is_none()
}

/// Whether `row` is Claude Code's pill-less footer row.
#[must_use]
pub fn is_bare_footer_row(row: &str) -> bool {
    row.strip_prefix("  ").is_some_and(|rest| {
        ["footer.shortcuts", "footer.clear", "busy.interrupt"]
            .into_iter()
            .any(|id| rest.starts_with(aterm_phase::anchor(id)))
    })
}

/// Whether `row` is Claude Code's permission-mode row.
#[must_use]
pub fn is_mode_row(row: &str) -> bool {
    let Some(rest) = pill_lead(row) else {
        return false;
    };
    PILLS.iter().any(|pill| {
        after_pill(rest, pill).is_some_and(|after| {
            after
                .strip_prefix(" (")
                .is_none_or(|hint| hint.contains(')'))
                || cycle_hint_len(after) > 0
        })
    })
}

const VIM_INSERT: &str = aterm_phase::anchor_text("vim.insert");

fn pill_lead(row: &str) -> Option<&str> {
    let rest = row.strip_prefix("  ")?;
    Some(after_vim_insert(rest).unwrap_or(rest))
}

fn after_vim_insert(rest: &str) -> Option<&str> {
    rest.strip_prefix(VIM_INSERT)?.strip_prefix(' ')
}

/// Whether `row` opens with vim's INSERT indicator before its pill.
#[must_use]
pub fn vim_insert(row: &str) -> bool {
    row.strip_prefix("  ")
        .and_then(after_vim_insert)
        .is_some_and(|after| PILLS.iter().any(|pill| after.starts_with(pill.glyph)))
}

fn after_pill<'a>(rest: &'a str, pill: &Pill) -> Option<&'a str> {
    rest.strip_prefix(pill.glyph)?
        .strip_prefix(' ')?
        .strip_prefix(pill.text)
}

fn cycle_hint_len(after: &str) -> usize {
    let Some(hint) = after.strip_prefix(" (") else {
        return 0;
    };
    let line = &hint[..hint.find("  ").unwrap_or(hint.len())];
    if let Some(close) = line.find(')') {
        return if line[..close].ends_with(aterm_phase::anchor_text("mode.cycle")) {
            2 + line[..=close].chars().count()
        } else {
            0
        };
    }
    let cut = line.trim_end();
    let key_ok = |key: &str| !key.is_empty() && !key.contains('(');
    let whole_words = match cut.split_once(' ') {
        None => key_ok(cut),
        Some((key, rest)) => key_ok(key) && rest == "to",
    };
    if whole_words {
        2 + cut.chars().count()
    } else {
        0
    }
}

/// One of the vendor's mode pills.
pub(super) struct Pill {
    pub(super) glyph: &'static str,
    /// The anchor id (`mode.auto`): what [`pill_indicator`] answers.
    pub(super) id: &'static str,
    /// The anchor's text (`auto mode on`).
    text: &'static str,
}

const fn pill(glyph: &'static str, id: &'static str) -> Pill {
    Pill {
        glyph,
        id,
        text: aterm_phase::anchor_text(id),
    }
}

/// The vendor's mode pills.
pub(super) const PILLS: [Pill; 6] = [
    pill("\u{23F5}\u{23F5}", "mode.bypass"),
    pill("\u{23F5}\u{23F5}", "mode.auto"),
    pill("\u{23F5}\u{23F5}", "mode.accept_edits"),
    pill("\u{23F5}\u{23F5}", "mode.dont_ask"),
    pill("\u{23F8}", "mode.plan"),
    pill("\u{23F8}", "mode.manual"),
];

/// The anchor id (`mode.auto`) of the known pill `row` opens with, shared by the footer, the lights (`harness::lights`) and the supervisor's `footer_mode`.
#[must_use]
pub fn pill_indicator(row: &str) -> Option<&'static str> {
    let rest = pill_lead(row)?;
    PILLS
        .iter()
        .find(|pill| after_pill(rest, pill).is_some())
        .map(|pill| pill.id)
}

/// Whether `row` opens like a mode pill, whatever its indicator.
#[must_use]
pub fn opens_with_pill_glyph(row: &str) -> bool {
    pill_lead(row).is_some_and(|rest| PILLS.iter().any(|pill| rest.starts_with(pill.glyph)))
}

/// Whether `row` is a known pill cut short by a narrow pane (`⏵⏵ bypass permissi`).
#[must_use]
pub fn is_cut_pill(row: &str) -> bool {
    let Some(rest) = pill_lead(row) else {
        return false;
    };
    let rest = rest.trim_end();
    !rest.is_empty()
        && PILLS
            .iter()
            .any(|pill| prefix_of_parts(rest, &[pill.glyph, " ", pill.text]))
}

fn prefix_of_parts(cut: &str, parts: &[&str]) -> bool {
    let mut rest = cut;
    for part in parts {
        if rest.len() <= part.len() {
            return part.starts_with(rest);
        }
        let Some(after) = rest.strip_prefix(part) else {
            return false;
        };
        rest = after;
    }
    rest.is_empty()
}

/// What aterm draws over Claude's bypass pill (`⏵⏵ bypass permissions on`) on the glass.
pub const YOLO_ON: &str = "YOLO on";
/// What it draws over the manual-mode pill (`⏸ manual mode on`), the mode that asks before each tool.
pub const YOLO_OFF: &str = "YOLO off";

/// Claude's mode pill as [`relabel_under`] found it, in PANE COLUMNS of the GUI's pane text (one char per cell), for the glass to relabel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relabel {
    /// The mode row.
    pub row: usize,
    /// Claude's pill, its glyph to its `on`: `start..end`.
    pub start: usize,
    pub end: usize,
    /// Where the pill's words start (after the glyph and its space).
    pub words: usize,
    /// The end of the row's left items, from the pill's end to the first two blanks.
    pub run_end: usize,
    /// [`YOLO_ON`] or [`YOLO_OFF`].
    pub label: &'static str,
}

impl Relabel {
    /// How far the run moves left: the pill's cells less the label's.
    #[must_use]
    pub const fn shift(&self) -> usize {
        (self.end - self.start).saturating_sub(self.label.len())
    }

    /// The column after the label.
    #[must_use]
    pub const fn label_end(&self) -> usize {
        self.start + self.label.len()
    }

    /// The grid column under glass column `col` of the relabelled row.
    #[must_use]
    pub const fn grid_col(&self, col: usize) -> usize {
        let shift = self.shift();
        if col < self.label_end() || col >= self.run_end {
            col
        } else if col + shift < self.run_end {
            col + shift
        } else {
            self.start
        }
    }
}

/// Claude Code's mode pill to relabel under the composer bottom rule `bottom`.
#[must_use]
pub fn relabel_under(rows: &[String], bottom: usize) -> Option<Relabel> {
    let row = pill_row_under(rows, bottom)?;
    let text = &rows[row];
    if !is_mode_row(text) {
        return None;
    }
    let rest = pill_lead(text)?;
    let (pill, label) = [("mode.bypass", YOLO_ON), ("mode.manual", YOLO_OFF)]
        .into_iter()
        .find_map(|(id, label)| {
            let pill = PILLS.iter().find(|p| p.id == id)?;
            let after = after_pill(rest, pill)?;
            (after.is_empty() || after.starts_with(' ')).then_some((pill, label))
        })?;
    let start = text[..text.len() - rest.len()].chars().count();
    let words = start + pill.glyph.chars().count() + 1;
    let end = words + pill.text.chars().count();
    let len = text.trim_end().chars().count();
    let mut run_end = len;
    let mut blank_before = false;
    for (k, c) in text.chars().enumerate().take(len).skip(end) {
        if c == ' ' && blank_before {
            run_end = k - 1;
            break;
        }
        blank_before = c == ' ';
    }
    Some(Relabel {
        row,
        start,
        end,
        words,
        run_end: run_end.max(end),
        label,
    })
}

const FOOTER_ROWS_MAX: usize = 4;

/// Claude Code's agents panel under the composer bottom rule `bottom`, read in PANE CELLS.
#[must_use]
pub fn agents_panel_under(
    rows: &[String],
    bottom: usize,
) -> Option<aterm_phase::agents_panel::AgentsPanel> {
    agents_panel_under_with(rows, bottom, aterm_phase::agents_panel::one_cell)
}

/// [`agents_panel_under`], its spans counted in `width`'s columns.
#[must_use]
pub fn agents_panel_under_with(
    rows: &[String],
    bottom: usize,
    width: aterm_phase::agents_panel::Width,
) -> Option<aterm_phase::agents_panel::AgentsPanel> {
    claude_agents_panel_under_with(rows, bottom, width)
        .or_else(|| agy_agents_panel_under_with(rows, bottom, width))
}

fn claude_agents_panel_under_with(
    rows: &[String],
    bottom: usize,
    width: aterm_phase::agents_panel::Width,
) -> Option<aterm_phase::agents_panel::AgentsPanel> {
    use aterm_phase::agents_panel::{AgentsPanel, RowKind, parse_row};
    let blank = |i: usize| rows.get(i).is_some_and(|r| r.trim().is_empty());
    let read = |i: usize| rows.get(i).and_then(|r| parse_row(r, width));
    let leads =
        |i: usize| read(i).is_some_and(|r| matches!(r.kind, RowKind::Main | RowKind::Workflow));
    let first = bottom.checked_add(1)?;
    let footer = rows.get(first)?;
    let known = is_mode_row(footer)
        || is_bare_footer_row(footer)
        || is_cut_pill(footer)
        || opens_with_pill_glyph(footer);
    if footer.trim().is_empty() || (!known && read(first).is_some()) {
        return None;
    }
    if !known && !footer.starts_with("  ") {
        return None;
    }
    let mut at = first + 1;
    while at < rows.len() && !blank(at) && !leads(at) {
        if at - first >= FOOTER_ROWS_MAX {
            return None;
        }
        at += 1;
    }
    if blank(at) {
        at += 1;
    }
    if !leads(at) {
        return None;
    }
    let mut panel = AgentsPanel::EMPTY;
    while at < rows.len() {
        let parsed = match read(at) {
            Some(parsed) => parsed,
            None if blank(at) && read(at + 1).is_some_and(|r| r.kind == RowKind::Workflow) => {
                at += 1;
                continue;
            }
            None => break,
        };
        let Ok(row) = u16::try_from(at) else { break };
        if !panel.push(row, parsed) {
            break;
        }
        at += 1;
    }
    Some(panel)
}

fn agy_agents_panel_under_with(
    rows: &[String],
    bottom: usize,
    width: aterm_phase::agents_panel::Width,
) -> Option<aterm_phase::agents_panel::AgentsPanel> {
    use aterm_phase::agents_panel::{AgentsPanel, Bullet, RowKind, parse_row};
    let (first, close) = aterm_phase::agy::panel_bounds_under(rows, bottom)?;
    let mut panel = AgentsPanel::EMPTY;
    for (at, line) in rows.iter().enumerate().take(close).skip(first) {
        let Some(parsed) = parse_row(line, width) else {
            continue;
        };
        if parsed.kind != RowKind::Agent
            || parsed.bullet.map(|(_, b)| b) != Some(Bullet::Filled)
            || !line
                .trim_start()
                .strip_prefix("● ")
                .is_some_and(|rest| rest.starts_with("Agent("))
        {
            continue;
        }
        let Ok(row) = u16::try_from(at) else {
            break;
        };
        if !panel.push(row, parsed) {
            break;
        }
    }
    (!panel.is_empty()).then_some(panel)
}

/// The footer's marked values, in order.
pub fn segments(facts: &FooterFacts) -> Vec<Segment> {
    let mut out: Vec<Segment> = facts
        .upgrade
        .as_ref()
        .map(|note| upgrade_segment(&note.whole))
        .into_iter()
        .collect();
    out.extend(fact_segments(facts, true));
    out
}

fn upgrade_segment(text: &str) -> Segment {
    Segment::plain(UPGRADE_MARK, text)
}

fn model_effort_segment(facts: &FooterFacts, stubs: bool) -> Option<Segment> {
    let off = facts.off;
    let model = facts.model.as_deref();
    let effort = facts.effort.as_deref();
    let hide_model = model.is_some() && off.has(FactKind::Model);
    let hide_effort = effort.is_some() && off.has(FactKind::Effort);
    let segment = |text: String, split: Option<usize>, quiet: (bool, bool)| Segment {
        mark: MODEL_MARK,
        text,
        split,
        quiet,
    };
    match (model, effort) {
        (None, None) => None,
        (Some(model), None) => match (hide_model, stubs) {
            (false, _) => Some(segment(model.to_owned(), None, (false, false))),
            (true, true) => Some(segment(format!("{model} {OFF_STUB}"), None, (true, false))),
            (true, false) => None,
        },
        (None, Some(effort)) => match (hide_effort, stubs) {
            (false, _) => Some(segment(effort.to_owned(), Some(0), (false, false))),
            (true, true) => Some(segment(
                format!("{effort} {OFF_STUB}"),
                Some(0),
                (false, true),
            )),
            (true, false) => None,
        },
        (Some(model), Some(effort)) => match (hide_model, hide_effort, stubs) {
            (false, false, _) => Some(segment(
                format!("{model} {effort}"),
                Some(model.len() + 1),
                (false, false),
            )),
            (true, false, true) => Some(segment(
                format!("{model} {OFF_STUB} {effort}"),
                Some(model.len() + OFF_STUB.len() + 2),
                (true, false),
            )),
            (true, false, false) => Some(segment(effort.to_owned(), Some(0), (false, false))),
            (false, true, true) => Some(segment(
                format!("{model} {effort} {OFF_STUB}"),
                Some(model.len() + 1),
                (false, true),
            )),
            (false, true, false) => Some(segment(model.to_owned(), None, (false, false))),
            (true, true, true) => Some(segment(
                format!("{model} {OFF_STUB} {effort} {OFF_STUB}"),
                Some(model.len() + OFF_STUB.len() + 2),
                (true, true),
            )),
            (true, true, false) => None,
        },
    }
}

fn fact_segments(facts: &FooterFacts, stubs: bool) -> Vec<Segment> {
    let usage = facts.usage.as_ref();
    let hidable = |kind: FactKind, value: &Option<String>| -> Option<Segment> {
        let mark = kind.mark()?;
        let value = value.as_ref()?;
        if !facts.off.has(kind) {
            return Some(Segment::plain(mark, value.clone()));
        }
        stubs.then(|| Segment {
            quiet: (true, false),
            ..Segment::plain(mark, format!("{value} {OFF_STUB}"))
        })
    };
    let plain = |mark: char, text: Option<String>| text.map(|text| Segment::plain(mark, text));
    [
        plain(
            WALL_MARK,
            usage
                .and_then(|u| u.wall.as_ref())
                .map(super::session_usage::wall_text),
        ),
        plain(
            ACCOUNT_MARK,
            facts.subscription.as_ref().map(|s| {
                s.email
                    .clone()
                    .unwrap_or_else(|| "account unavailable".into())
            }),
        ),
        plain(
            PLAN_MARK,
            facts.subscription.as_ref().and_then(|s| s.plan.clone()),
        ),
        plain(
            WEEKLY_MARK,
            facts.subscription.as_ref().map(|s| {
                s.weekly.clone().unwrap_or_else(|| {
                    if usage
                        .and_then(|u| u.wall.as_ref())
                        .is_some_and(|w| w.label == "7d")
                    {
                        "weekly 0% left".into()
                    } else {
                        s.weekly_gap
                            .map_or("weekly unavailable", super::profile::WeeklyGap::words)
                            .into()
                    }
                })
            }),
        ),
        model_effort_segment(facts, stubs),
        plain(
            ACCOUNT_MARK,
            facts
                .account
                .clone()
                .filter(|_| facts.subscription.is_none()),
        ),
        hidable(FactKind::Path, &facts.path),
        hidable(FactKind::Branch, &facts.branch),
        hidable(FactKind::Context, &facts.context),
        plain(
            USAGE_MARK,
            usage.and_then(|u| {
                super::session_usage::usage_text_at(u, super::session_usage::UsageGrain::Total)
            }),
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn model_alone(facts: &FooterFacts) -> Option<Segment> {
    facts
        .model
        .as_ref()
        .filter(|_| !facts.off.has(FactKind::Model))
        .map(|model| Segment::plain(MODEL_MARK, model.clone()))
}

/// The cells `text` takes on the glass: a wide char two, a combining mark or a control none.
#[must_use]
pub fn text_width(text: &str) -> usize {
    aterm_grapheme::str_grid_columns(text)
}

/// The cells `segments` take on the glass, set ([`set_facts`]).
#[must_use]
pub fn segments_width(segments: &[Segment]) -> usize {
    set_facts(segments).cells
}

/// One glyph of the facts as [`set_facts`] places it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetGlyph {
    /// The character.
    pub ch: char,
    /// Its cell in the set run (its lead cell: a wide char takes the next one too).
    pub cell: usize,
    /// The cells it takes: 1, or 2 for a wide char.
    pub cols: usize,
    /// How far its glyph is drawn from the middle of its cells, in [`NUDGES_PER_CELL`]ths of a cell.
    pub nudge: i8,
    /// A fact's mark (`◆`, `⌂`, ...) rather than a character of its value.
    pub mark: bool,
    /// A character of the model-and-effort segment's effort, past its [`Segment::split`].
    pub tail: bool,
    /// A character of a hidden fact's greyed stub, its value and [`OFF_STUB`] ([`Segment::quiet`]).
    pub quiet: bool,
}

/// The facts set as a line of small type ([`set_facts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetFacts {
    /// The cells the line takes, a whole number.
    pub cells: usize,
    /// Every glyph, in reading order, each in a cell of its own.
    pub glyphs: Vec<SetGlyph>,
    /// Native emoji clusters at their set lead cell.
    pub clusters: Vec<(usize, Box<str>)>,
    /// Native combining marks at their set base cell.
    pub combining: Vec<(usize, Box<[char]>)>,
}

/// Set the facts as a line of type.
#[must_use]
pub fn set_facts(segments: &[Segment]) -> SetFacts {
    let mut glyphs: Vec<SetGlyph> = Vec::new();
    let mut place: Vec<f32> = Vec::new();
    let mut clusters = Vec::new();
    let mut combining: Vec<(usize, Vec<char>)> = Vec::new();
    let mut native_pitch = false;
    let mut line = 0.0f32;
    for (i, segment) in segments.iter().enumerate() {
        if i > 0 {
            line += GAP;
        }
        let quiet = |tail: bool| {
            if tail {
                segment.quiet.1
            } else {
                segment.quiet.0
            }
        };
        let of_text = |own: bool, at: usize| {
            let tail = own && segment.split.is_some_and(|split| at >= split);
            (tail, own && quiet(tail))
        };
        let mut mark_buf = [0; 4];
        let mark_text = segment.mark.encode_utf8(&mut mark_buf);
        for (text, mark, own) in [
            (mark_text as &str, true, false),
            (" ", false, false),
            (&segment.text, false, true),
        ] {
            let mut grid = aterm_grapheme::GridColumns::new();
            let mut base = None;
            for cluster in aterm_grapheme::split_graphemes(text) {
                if cluster.is_emoji || cluster.text.contains('\u{20e3}') {
                    native_pitch |= cluster.width == 2
                        && (cluster.codepoint_count > 1
                            || cluster
                                .text
                                .chars()
                                .next()
                                .is_some_and(aterm_grapheme::is_emoji_presentation));
                    grid.advance_str(cluster.text);
                    let index = glyphs.len();
                    let (tail, quiet) = of_text(own, cluster.byte_offset);
                    place.push(line);
                    glyphs.push(SetGlyph {
                        ch: cluster.text.chars().next().unwrap_or(' '),
                        cell: 0,
                        cols: cluster.width,
                        nudge: 0,
                        mark,
                        tail,
                        quiet,
                    });
                    if cluster.codepoint_count > 1 {
                        clusters.push((index, Box::<str>::from(cluster.text)));
                    }
                    line += cluster.width as f32;
                    base = Some(index);
                    continue;
                }
                for (at, ch) in cluster
                    .text
                    .char_indices()
                    .filter(|(_, ch)| !ch.is_control())
                {
                    match grid.step(ch) {
                        aterm_grapheme::GridStep::Opens { width, .. } => {
                            if ch.is_whitespace() {
                                line += width as f32;
                                base = None;
                                continue;
                            }
                            let (tail, quiet) = of_text(own, cluster.byte_offset + at);
                            base = Some(glyphs.len());
                            place.push(line);
                            glyphs.push(SetGlyph {
                                ch,
                                cell: 0,
                                cols: width,
                                nudge: 0,
                                mark,
                                tail,
                                quiet,
                            });
                            line += width as f32;
                        }
                        step => {
                            if let Some(index) = base {
                                if step == (aterm_grapheme::GridStep::Joins { widened: true }) {
                                    line += (2 - glyphs[index].cols) as f32;
                                    glyphs[index].cols = 2;
                                }
                                if let Some((_, marks)) =
                                    combining.last_mut().filter(|(at, _)| *at == index)
                                {
                                    marks.push(ch);
                                } else {
                                    combining.push((index, vec![ch]));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let ink: usize = glyphs.iter().map(|g| g.cols).sum();
    if glyphs.is_empty() {
        return SetFacts::default();
    }
    let reach = f32::from(i8::MAX) / f32::from(NUDGES_PER_CELL);
    let pitch = if native_pitch { 1.0 } else { FACT_PITCH };
    let mut cells = ((line * pitch - 1e-3).ceil() as usize).max(ink);
    loop {
        let pitch = cells as f32 / line;
        let mut before = Vec::with_capacity(glyphs.len());
        let mut used = 0usize;
        for g in &glyphs {
            before.push(used);
            used += g.cols;
        }
        let want: Vec<f32> = glyphs
            .iter()
            .zip(&place)
            .zip(&before)
            .map(|((g, &at), &b)| {
                (at + g.cols as f32 / 2.0) * pitch - g.cols as f32 / 2.0 - b as f32
            })
            .collect();
        let mut high = Vec::with_capacity(want.len());
        let mut most = f32::MIN;
        for &w in &want {
            most = most.max(w);
            high.push(most);
        }
        let mut low = vec![0.0f32; want.len()];
        let mut least = f32::MAX;
        for (i, &w) in want.iter().enumerate().rev() {
            least = least.min(w);
            low[i] = least;
        }
        let slack = (cells - ink) as f32;
        let mut fits = true;
        let mut set = glyphs.clone();
        for (i, g) in set.iter_mut().enumerate() {
            let lead = ((high[i] + low[i]) / 2.0).clamp(0.0, slack).round() as usize + before[i];
            let moved =
                (place[i] + g.cols as f32 / 2.0) * pitch - (lead as f32 + g.cols as f32 / 2.0);
            if moved.abs() > reach {
                fits = false;
                break;
            }
            g.cell = lead;
            g.nudge = (moved * f32::from(NUDGES_PER_CELL)).round() as i8;
        }
        if fits {
            return SetFacts {
                cells,
                clusters: clusters
                    .into_iter()
                    .map(|(index, text)| (set[index].cell, text))
                    .collect(),
                combining: combining
                    .into_iter()
                    .map(|(index, marks)| (set[index].cell, marks.into_boxed_slice()))
                    .collect(),
                glyphs: set,
            };
        }
        cells += 1;
    }
}

/// Where one fact lies in the facts as set ([`fact_spans`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactSpan {
    /// Which fact.
    pub kind: FactKind,
    /// Its cells in the set run ([`SetFacts::cells`]), `start..end`.
    pub cells: (u16, u16),
    /// The hidden facts a click on it shows again.
    pub restores: FactMask,
}

/// Every fact of one rule's facts, in reading order ([`fact_spans`]).
pub type FactSpans = [Option<FactSpan>; 6];

fn glyph_fact(segment: &Segment, tail: bool, quiet: bool) -> Option<(FactKind, FactMask)> {
    let kind = if tail {
        FactKind::Effort
    } else {
        match segment.mark {
            MODEL_MARK => FactKind::Model,
            PATH_MARK => FactKind::Path,
            BRANCH_MARK => FactKind::Branch,
            CONTEXT_MARK => FactKind::Context,
            USAGE_MARK => FactKind::Usage,
            _ => return None,
        }
    };
    let restores = if quiet {
        FactMask::default().with(kind, true)
    } else {
        FactMask::default()
    };
    Some((kind, restores))
}

/// Where each fact of `segments`, as [`set_facts`] set them (`set`), lies.
#[must_use]
pub fn fact_spans(set: &SetFacts, segments: &[Segment]) -> FactSpans {
    let mut spans: FactSpans = [None; 6];
    let room = spans.len();
    let mut n = 0usize;
    let mut segment: Option<&Segment> = None;
    let mut next = 0usize;
    let mut mark_at: Option<usize> = None;
    let cell = |at: usize| u16::try_from(at).unwrap_or(u16::MAX);
    for glyph in &set.glyphs {
        if glyph.mark {
            segment = segments.get(next);
            next += 1;
            mark_at = Some(glyph.cell);
            continue;
        }
        let Some((kind, restores)) = segment.and_then(|s| glyph_fact(s, glyph.tail, glyph.quiet))
        else {
            mark_at = None;
            continue;
        };
        let start = mark_at.take().unwrap_or(glyph.cell);
        let end = glyph.cell + glyph.cols;
        match n.checked_sub(1).and_then(|last| spans[last].as_mut()) {
            Some(span) if span.kind == kind && span.restores == restores && start == glyph.cell => {
                span.cells.1 = cell(end);
            }
            _ if n < room => {
                spans[n] = Some(FactSpan {
                    kind,
                    cells: (cell(start), cell(end)),
                    restores,
                });
                n += 1;
            }
            _ => break,
        }
    }
    spans
}

/// Where the subscription/account group (`⎆`, `◇`, `◷`) of `segments`, as [`set_facts`] set them (`set`), lies.
#[must_use]
pub fn account_span(set: &SetFacts, segments: &[Segment]) -> Option<(u16, u16)> {
    let mut segment: Option<&Segment> = None;
    let mut next = 0usize;
    let mut mark_at: Option<usize> = None;
    let mut span: Option<(u16, u16)> = None;
    let cell = |at: usize| u16::try_from(at).unwrap_or(u16::MAX);
    for glyph in &set.glyphs {
        if glyph.mark {
            segment = segments.get(next);
            next += 1;
            mark_at = Some(glyph.cell);
            continue;
        }
        let is_acct =
            segment.is_some_and(|s| matches!(s.mark, ACCOUNT_MARK | PLAN_MARK | WEEKLY_MARK));
        if !is_acct {
            mark_at = None;
            continue;
        }
        let start = mark_at.take().unwrap_or(glyph.cell);
        let end = glyph.cell + glyph.cols;
        span = Some(match span {
            Some((s, _)) => (s, cell(end)),
            None => (cell(start), cell(end)),
        });
    }
    span
}

/// The cells of the set run ([`SetFacts::cells`]) an underline under `span`'s fact takes.
#[must_use]
pub fn drawn_cells(set: &SetFacts, span: &FactSpan) -> std::ops::Range<usize> {
    let n = isize::from(NUDGES_PER_CELL);
    let own = usize::from(span.cells.0)..usize::from(span.cells.1);
    let mut edges: Option<(isize, isize)> = None;
    for glyph in set.glyphs.iter().filter(|g| own.contains(&g.cell)) {
        let left = glyph.cell as isize * n + isize::from(glyph.nudge);
        let right = left + glyph.cols as isize * n;
        edges = Some(edges.map_or((left, right), |(l, r)| (l.min(left), r.max(right))));
    }
    let Some((left, right)) = edges else {
        return 0..0;
    };
    let first = |edge: isize| {
        (edge - n / 2 + n - 1)
            .div_euclid(n)
            .clamp(0, set.cells as isize) as usize
    };
    first(left)..first(right)
}

/// The run of the composer's bottom rule aterm may write into.
#[must_use]
pub fn rule_run(coverable: &[bool]) -> Option<std::ops::Range<usize>> {
    let mut best: Option<std::ops::Range<usize>> = None;
    let mut at = 0;
    while at < coverable.len() {
        if !coverable[at] {
            at += 1;
            continue;
        }
        let start = at;
        while at < coverable.len() && coverable[at] {
            at += 1;
        }
        if best.as_ref().is_none_or(|b| at - start >= b.len()) {
            best = Some(start..at);
        }
    }
    best
}

/// The cells of `run` ([`rule_run`]) aterm's text may take.
#[must_use]
pub fn rule_room(run: &std::ops::Range<usize>) -> usize {
    run.len().saturating_sub(RULE_LEAD + 1)
}

/// How many leaner forms a light's title may have between its whole and its short form.
pub const LEAN_FORMS: usize = 4;

/// The widths of the lights' block (`claude_lights`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightsWidth {
    /// The chips, one blank apart.
    pub chips: usize,
    /// `Light: what  `.
    pub title: usize,
    /// The whole title with parts given up, widest first, each still more than the short form.
    pub lean: [usize; LEAN_FORMS],
    /// `what  `.
    pub short: usize,
    /// The title says why (a stopped mode return, a refusal, a switch note) rather than what the pointer or selection shows.
    pub reason: bool,
}

/// Which of the lights' titles is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleFit {
    /// `Light: what`.
    Full,
    /// The `n`th of the whole title's leaner forms ([`LightsWidth::lean`]).
    Lean(u8),
    /// `what` alone.
    Short,
    /// None.
    None,
}

/// What of aterm's text goes into the rule ([`fit_rule`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFit {
    /// The facts' marked values.
    pub segments: Vec<Segment>,
    /// The lights' title.
    pub title: TitleFit,
    /// Whether the lights' chips are drawn.
    pub chips: bool,
}

impl RuleFit {
    /// The fewest cells it takes ([`rule_width`]).
    #[must_use]
    pub fn width(&self, lights: Option<LightsWidth>) -> usize {
        rule_width(&self.segments, self.title, self.chips, lights)
    }
}

fn rule_width(
    segments: &[Segment],
    title: TitleFit,
    chips: bool,
    lights: Option<LightsWidth>,
) -> usize {
    let facts = if segments.is_empty() {
        0
    } else {
        segments_width(segments) + 2
    };
    let lights = match lights.filter(|_| chips) {
        Some(l) => {
            let title = match title {
                TitleFit::Full => l.title,
                TitleFit::Lean(n) => l.lean.get(usize::from(n)).copied().unwrap_or(0),
                TitleFit::Short => l.short,
                TitleFit::None => 0,
            };
            title + l.chips + 2
        }
        None => 0,
    };
    facts + lights + if facts > 0 && lights > 0 { RULE_SEP } else { 0 }
}

fn facts_ladder(facts: &FooterFacts) -> Vec<Vec<Segment>> {
    let plain = plain_ladder(facts);
    let Some(note) = &facts.upgrade else {
        return plain;
    };
    let led = |text: &str, rung: &[Segment]| -> Vec<Segment> {
        std::iter::once(upgrade_segment(text))
            .chain(rung.iter().cloned())
            .collect()
    };
    let whole = &plain[0];
    let mut out = vec![led(&note.whole, whole)];
    for rung in plain.iter().filter(|rung| same_top(rung, whole)) {
        let rung = led(&note.short, rung);
        if !out.contains(&rung) {
            out.push(rung);
        }
    }
    out.extend(plain);
    out
}

fn top_of(rung: &[Segment]) -> impl Iterator<Item = &Segment> {
    let subscription = rung.iter().any(|s| s.mark == WEEKLY_MARK);
    rung.iter().filter(move |s| {
        s.mark == WALL_MARK
            || (!subscription && s.mark == MODEL_MARK)
            || (subscription && (s.mark == ACCOUNT_MARK || s.mark == WEEKLY_MARK))
    })
}

fn same_top(a: &[Segment], b: &[Segment]) -> bool {
    let shown = |rung| {
        top_of(rung)
            .map(Segment::shown_part)
            .filter(|(_, text)| !text.is_empty())
    };
    shown(a).eq(shown(b))
}

fn plain_ladder(facts: &FooterFacts) -> Vec<Vec<Segment>> {
    let mut out = vec![fact_segments(facts, true)];
    for rung in bare_ladder(facts) {
        if !out.contains(&rung) {
            out.push(rung);
        }
    }
    out
}

fn bare_ladder(facts: &FooterFacts) -> Vec<Vec<Segment>> {
    if facts.subscription.is_some() {
        return subscription_ladder(facts);
    }
    let whole = fact_segments(facts, false);
    let mark = |mark: char| whole.iter().find(|s| s.mark == mark).cloned();
    let (wall, model_effort, account, path, branch) = (
        mark(WALL_MARK),
        mark(MODEL_MARK),
        mark(ACCOUNT_MARK),
        mark(PATH_MARK),
        mark(BRANCH_MARK),
    );
    let short = facts
        .path
        .as_deref()
        .filter(|_| !facts.off.has(FactKind::Path))
        .and_then(elide_path)
        .map(|text| Segment::plain(PATH_MARK, text));
    let model = model_alone(facts);
    let runtime: Option<Vec<Segment>> = facts
        .usage
        .as_ref()
        .and_then(|u| {
            super::session_usage::usage_text_at(u, super::session_usage::UsageGrain::Runtime)
        })
        .map(|text| {
            whole
                .iter()
                .filter(|s| s.mark != USAGE_MARK)
                .cloned()
                .chain(std::iter::once(Segment::plain(USAGE_MARK, text)))
                .collect()
        });
    let mut unnamed: Vec<Vec<Segment>> = [Some(&whole), runtime.as_ref()]
        .into_iter()
        .flatten()
        .filter(|_| account.is_some())
        .map(|rung| {
            rung.iter()
                .filter(|s| s.mark != ACCOUNT_MARK)
                .cloned()
                .collect()
        })
        .collect();
    let mut out = vec![whole];
    let mut push = |rung: Vec<Segment>| {
        if !out.contains(&rung) {
            out.push(rung);
        }
    };
    if let Some(rung) = runtime {
        push(rung);
    }
    for wall in [wall.as_ref(), None] {
        let rung = |named: bool,
                    model: Option<&Segment>,
                    path: Option<&Segment>,
                    branch: Option<&Segment>| {
            [
                wall,
                model,
                account.as_ref().filter(|_| named),
                path,
                branch,
            ]
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<Segment>>()
        };
        let model_effort = model_effort.as_ref();
        let steps = [
            Some((model_effort, path.as_ref(), branch.as_ref())),
            short
                .as_ref()
                .map(|s| (model_effort, Some(s), branch.as_ref())),
            Some((model_effort, path.as_ref(), None)),
            short.as_ref().map(|s| (model_effort, Some(s), None)),
            Some((model_effort, None, None)),
            Some((model.as_ref(), None, None)),
        ];
        for (model, path, branch) in steps.into_iter().flatten() {
            // Where the account does not fit beside the whole path, the
            // tokens come back without it. With no account this pushes nothing.
            if account.is_some() {
                push(rung(true, model, path, branch));
                std::mem::take(&mut unnamed).into_iter().for_each(&mut push);
            }
            push(rung(false, model, path, branch));
        }
        if let Some(wall) = wall {
            push(vec![wall.clone()]);
        }
    }
    out
}

fn subscription_ladder(facts: &FooterFacts) -> Vec<Vec<Segment>> {
    let mut rung = fact_segments(facts, false);
    let mut out = vec![rung.clone()];
    for mark in [
        CONTEXT_MARK,
        USAGE_MARK,
        BRANCH_MARK,
        PATH_MARK,
        MODEL_MARK,
        PLAN_MARK,
    ] {
        rung.retain(|segment| segment.mark != mark);
        if out.last() != Some(&rung) {
            out.push(rung.clone());
        }
    }
    for limit in [24, 18, 12, 8] {
        let mut short = rung.clone();
        if shorten_subscription_account(&mut short, limit) && out.last() != Some(&short) {
            out.push(short);
        }
    }
    let mut narrow = rung.clone();
    shorten_subscription_account(&mut narrow, 8);
    if narrow_subscription_weekly(&mut narrow) && out.last() != Some(&narrow) {
        out.push(narrow);
    }
    if let Some(wall) = rung.iter().find(|s| s.mark == WALL_MARK).cloned() {
        out.push(vec![wall]);
        let mut without_wall = facts.clone();
        if let Some(usage) = &mut without_wall.usage {
            usage.wall = None;
        }
        out.extend(subscription_ladder(&without_wall));
    }
    out
}

fn shorten_subscription_account(segments: &mut [Segment], limit: usize) -> bool {
    let Some(account) = segments.iter_mut().find(|s| s.mark == ACCOUNT_MARK) else {
        return false;
    };
    if text_width(&account.text) <= limit {
        return false;
    }
    account.text = aterm_grapheme::grid_graphemes(&account.text)
        .take_while(|cluster| cluster.end_col <= limit.saturating_sub(1))
        .map(|cluster| cluster.text)
        .chain(std::iter::once("…"))
        .collect();
    true
}

/// Narrow the weekly segment to its narrow form ([`super::profile::narrow_weekly`]: an aged
/// figure keeps its age, a reason its `weekly ?`), where it has one.
fn narrow_subscription_weekly(segments: &mut [Segment]) -> bool {
    let Some(weekly) = segments.iter_mut().find(|s| s.mark == WEEKLY_MARK) else {
        return false;
    };
    let Some(text) = super::profile::narrow_weekly(&weekly.text) else {
        return false;
    };
    weekly.text = text;
    true
}

fn subscription_reservation(facts: &FooterFacts, room: usize) -> usize {
    let segments = fact_segments(facts, false);
    let wall = segments.iter().find(|s| s.mark == WALL_MARK).cloned();
    let mut essentials: Vec<_> = segments
        .into_iter()
        .filter(|s| matches!(s.mark, ACCOUNT_MARK | WEEKLY_MARK))
        .collect();
    shorten_subscription_account(&mut essentials, 8);
    narrow_subscription_weekly(&mut essentials);
    let with_wall: Vec<_> = wall.iter().cloned().chain(essentials.clone()).collect();
    [with_wall.as_slice(), wall.as_slice(), essentials.as_slice()]
        .into_iter()
        .filter(|segments| !segments.is_empty())
        .map(|segments| rule_width(segments, TitleFit::None, false, None))
        .find(|width| width + RULE_SEP + 3 <= room)
        .unwrap_or(0)
}

/// What of the facts and lights fits `room` cells of the rule ([`rule_room`]).
#[must_use]
pub fn fit_rule(facts: &FooterFacts, room: usize, lights: Option<LightsWidth>) -> RuleFit {
    let ladder = facts_ladder(facts);
    let model = model_alone(facts);
    let chips = lights.is_some();
    let reason = lights.is_some_and(|l| l.reason);
    let whole = ladder[0].as_slice();
    let alone = ladder
        .iter()
        .map(Vec::as_slice)
        .find(|rung| rule_width(rung, TitleFit::None, chips, lights) <= room);
    let has = |title: TitleFit| match (title, lights) {
        (TitleFit::Full, _) => true,
        (TitleFit::Lean(n), Some(l)) => l.lean.get(usize::from(n)).is_some_and(|w| *w > 0),
        (TitleFit::Short, Some(l)) => l.short > 0,
        _ => false,
    };
    let titled = [
        TitleFit::Full,
        TitleFit::Lean(0),
        TitleFit::Lean(1),
        TitleFit::Lean(2),
        TitleFit::Lean(3),
        TitleFit::Short,
    ]
    .into_iter()
    .filter(|title| chips && has(*title))
    .filter_map(|title| Some((alone?, title, chips)));
    let reasons = ladder[1..]
        .iter()
        .filter(|rung| reason && same_top(rung, whole))
        .map(|rung| (rung.as_slice(), TitleFit::Short, chips));
    let bare = ladder
        .iter()
        .map(|rung| (rung.as_slice(), TitleFit::None, chips));
    let last = if facts.subscription.is_some() {
        [(&[][..], true), (model.as_slice(), false)]
    } else {
        [(model.as_slice(), false), (&[][..], true)]
    }
    .into_iter()
    .filter(|_| chips)
    .map(|(segments, chips)| (segments, TitleFit::None, chips));
    let (segments, title, chips) = titled
        .chain(reasons)
        .chain(bare)
        .chain(last)
        .find(|&(segments, title, chips)| rule_width(segments, title, chips, lights) <= room)
        .unwrap_or((&[], TitleFit::None, false));
    RuleFit {
        segments: segments.to_vec(),
        title,
        chips,
    }
}

/// The most cells the chips may take in a rule of `room` cells.
#[must_use]
pub fn chips_room(facts: &FooterFacts, room: usize) -> usize {
    let reserved = if facts.subscription.is_some() {
        subscription_reservation(facts, room)
    } else {
        rule_width(model_alone(facts).as_slice(), TitleFit::None, false, None)
    };
    let beside = if reserved > 0 && reserved <= room {
        reserved + RULE_SEP
    } else {
        0
    };
    room.saturating_sub(beside + 2)
}

/// Where Claude Code's top-effort tag ([`CLAUDE_TOP_EFFORT_KEY`]) stands in the composer's TOP rule `row`.
#[must_use]
pub fn top_rule_tag(row: &str) -> Option<(usize, usize)> {
    if !aterm_phase::phase::is_rule(row) {
        return None;
    }
    let chars: Vec<char> = row.trim_end().chars().collect();
    let n = chars.len();
    if n < 4 || chars[n - 1] != '\u{2500}' || chars[n - 2] != ' ' {
        return None;
    }
    let zone = (1..n - 2)
        .rev()
        .find(|&i| chars[i] == '\u{2500}' && chars[i - 1] == '\u{2500}')?
        + 1;
    let key: Vec<char> = CLAUDE_TOP_EFFORT_KEY.chars().collect();
    let labels = &chars[zone..n - 2];
    let mut at = 0;
    while at < labels.len() {
        if labels[at] == ' ' {
            at += 1;
            continue;
        }
        let end = (at..labels.len())
            .find(|&i| labels[i] == ' ')
            .unwrap_or(labels.len());
        if labels[at..end] == key[..] {
            return Some((zone + at, zone + end));
        }
        at = end;
    }
    None
}

/// What Claude Code answered an `/effort <key> on|off` with on the screen ([`top_effort_answer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TopAnswer {
    /// `<Key> on (this session only): …`.
    On,
    /// `<Key> off. Effort stays …`.
    Off,
    /// `<Key> needs dynamic workflows enabled (see /config). …`.
    NeedsWorkflows,
    /// `<Key> isn't available on <model>. …`.
    Unavailable,
}

const TOP_ANSWER_ROWS: usize = 8;

/// What Claude Code answered an `/effort <key> on|off` with ([`top_effort_reply`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TopReply {
    pub answer: TopAnswer,
    pub model: Option<String>,
}

fn is_effort_hint(text: &str) -> bool {
    text.ends_with(aterm_phase::anchor("effort.hint_tail"))
}

/// The answer to an `/effort <key> on|off` that the screen `rows` show above the composer's top rule.
#[must_use]
pub fn top_effort_answer(rows: &[String], top_rule: usize) -> Option<TopAnswer> {
    top_effort_reply(rows, top_rule).map(|reply| reply.answer)
}

/// The answer to an `/effort <key> on|off` that the screen `rows` show above the composer's top rule.
#[must_use]
pub fn top_effort_reply(rows: &[String], top_rule: usize) -> Option<TopReply> {
    use aterm_phase::anchor;
    let top_rule = top_rule.min(rows.len());
    let mut end = top_rule;
    while end > 0 {
        let text = rows[end - 1].trim();
        if !(text.is_empty() || is_effort_hint(text)) {
            break;
        }
        end -= 1;
    }
    let first = end.saturating_sub(TOP_ANSWER_ROWS);
    let at = (first..end).rev().find_map(|i| {
        let text = rows[i].trim_start();
        if text.is_empty() || text.starts_with(['\u{276F}', '\u{23FA}', '\u{25CF}', '\u{273B}']) {
            return Some(None);
        }
        text.starts_with('\u{23BF}').then_some(Some(i))
    })??;
    let head = rows[at].trim_start().strip_prefix('\u{23BF}')?;
    let mut body = head.trim().to_owned();
    for more in &rows[at + 1..end] {
        body.push(' ');
        body.push_str(more.trim());
    }
    let rest = past_key(&body)?;
    let answer = if rest.starts_with(anchor("effort.top_on")) {
        TopAnswer::On
    } else if rest.starts_with(anchor("effort.top_off")) {
        TopAnswer::Off
    } else if rest.starts_with(anchor("effort.top_needs")) {
        TopAnswer::NeedsWorkflows
    } else {
        let named = rest.strip_prefix(anchor("effort.top_unavailable"))?;
        let id = named.split_whitespace().next().unwrap_or("");
        let id = id.strip_suffix('.').unwrap_or(id);
        return Some(TopReply {
            answer: TopAnswer::Unavailable,
            model: (!id.is_empty()).then(|| id.to_owned()),
        });
    };
    Some(TopReply {
        answer,
        model: None,
    })
}

/// The environment actions of `ClaudeFooterModel`.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Flag",
        reason = "The process's own `--model` at its launch: Claude Code's argv, which the reader reads (`launch_facts`) and never writes. The Tier-1 bind launches with it exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "LaunchResume",
        reason = "A launch with `--resume <conversation>`, which Claude restores its model from: Claude Code's step. The Tier-1 bind launches so and writes the conversation on disk exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "DrawCard",
        reason = "Claude Code drawing its launch card on the pane: the vendor's screen, which the host reads (`launch_card`). The Tier-1 bind draws a measured 2.1.283 card exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Answer",
        reason = "An answer Claude Code writes into the transcript: the vendor's row. The Tier-1 bind appends one in 2.1.283's shape exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "ChooseA",
        reason = "A person's `/model` choice and the result row Claude Code writes for it: the vendor's step. The Tier-1 bind appends the command and a readable result exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "ChooseB",
        reason = "A person's `/model` choice of the other model, and its result row: the vendor's step. The Tier-1 bind appends the command and a readable result exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "ChooseUnread",
        reason = "A `/model` choice whose result this build cannot read: the vendor's step. The Tier-1 bind appends the command and an unquoted result exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Flood",
        reason = "A row larger than the reader's tail window (a pasted image): the vendor's step. The Tier-1 bind appends a 600 KiB image prompt exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Clear",
        reason = "`/clear`: Claude Code rewrites `sessions/<pid>.json` with a new session id under the same process. The Tier-1 bind rewrites the registry exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Resume",
        reason = "An in-REPL `/resume` into the next conversation, restored by Claude unless pinned: the vendor's step. The Tier-1 bind rewrites the registry exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "ResumeLate",
        reason = "A first `/resume` into a conversation another process began since this one started: the vendor's step. The Tier-1 bind writes D and rewrites the registry exactly there."
    )
)]
#[doc(hidden)]
pub fn claude_footer_model_environment() {}

#[cfg(test)]
#[path = "footer_tests.rs"]
mod tests;
