// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code footer: aterm's facts about a Claude Code session, written
//! into the rule under Claude Code's input box, right-aligned the way Claude
//! writes its own effort tag (`‹top effort›` below) into the rule above it:
//!
//! ```text
//! ────────────────────────────────────────────── ‹top effort› ─
//! ❯ fix the footer
//! ───────────────── ◆ Opus 5.5 xhigh   ⌂ ~/aterm   ⎇ main ─
//!   ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← for agents
//! ```
//!
//! Model and effort under one mark, then the working directory (home as `~`),
//! then the branch. Only cells that are plain rule glyphs are covered
//! ([`rule_run`]), and Claude's own footer row under the rule is never touched.
//! A pane too narrow for every fact gives them up in a fixed order, the model
//! last ([`fit_rule`], `facts_ladder`).
//!
//! Claude Code's own rows cannot be switched off from outside (no setting or
//! variable removes them, and a statusLine only adds a row), so the facts do not
//! come from a statusLine; the usage tap ([`super::tap`]) is one only on a line
//! aterm itself relaunches. They come from what Claude Code already keeps:
//!
//! * `<claude dir>/sessions/<pid>.json` maps the process to its session id and
//!   launch directory, so a pane finds its own transcript when several sessions
//!   share one directory ([`session_of_pid`]). The path and branch follow the
//!   transcript's newest main-thread `cwd` since the process started, else the
//!   launch directory.
//! * The transcript carries the model and effort: every answer's
//!   (`message.model`, `effort`) and the result Claude writes for its own
//!   `/model`, `/effort` and `/fast` ([`tail_facts`]). The transcript is the
//!   conversation's, shared by every process that resumed it, so only a row
//!   written since this process started, and since it entered that session,
//!   speaks for it; the model is the process's, carried across `/clear` and
//!   `/resume` as [`TailCache`] describes.
//! * The process's own `--model` ([`launch_facts`]), when the transcript since
//!   its start names no model yet.
//! * Claude Code's launch card on screen ([`launch_card`]), when nothing above
//!   named them, kept for that process and session only ([`FactsOwner`]).
//! * The branch, read live from `.git/HEAD` ([`git_head`]), so a checkout shows
//!   at once.
//! * The session's usage ([`super::session_usage`], via [`FooterCache::usage`]):
//!   the conversation's tokens and working time (not floored at the process's
//!   start, as model and effort are) as one merged total, last in the run
//!   (`Σ 7h52m ↑395M ↓2.0M`; `[claude_footer] usage = false` hides it), and,
//!   while one stands, the limit wall Claude Code wrote, first in the run
//!   (`⧗ 5h ♻ 3pm`).
//! * Laid in by the window, not read here: a live upgrade waiting on the person
//!   at this composer ([`FooterFacts::upgrade`]), first in the run, and the
//!   account the process runs on ([`FooterFacts::account`]), only where the
//!   roster lists two or more.
//!
//! Nothing is written anywhere and the user's Claude settings are not touched.
//! Vendor output is read as a third party's: an unrecognised rule is left as
//! drawn, a moved file or result shape yields no fact, and facts are keyed to one
//! process (pid and start time, [`parse_session_entry`]), so a relaunched build
//! is read afresh.
//!
//! The mode row has `columns - 4` cells; Claude wraps the pill and its cycle hint
//! by word and shows only the first line, so a narrow pane cuts the hint between
//! words (never with `…`) or drops it, and cuts the pill itself when narrower
//! still ([`is_cut_pill`]). aterm writes nothing on that row, but recognising it
//! decides where the mode is read and whether a live REPL is up ([`is_mode_row`],
//! [`live_repl_row`], [`cycle_hint_len`]).

use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aterm_json::Value;
use aterm_primer::CLAUDE_TOP_EFFORT_KEY;

/// How much of a transcript's end one read window takes; one turn with a large
/// tool result can be hundreds of KiB. [`TailCache`] reads further back a
/// window at a time; [`read_tail_facts`] reads this window alone.
pub const TAIL_BYTES: u64 = 512 * 1024;

/// Longest transcript line [`tail_facts`] will parse. A row past this is a
/// tool result, never a model or effort carrier worth a large parse.
const MAX_LINE_BYTES: usize = 256 * 1024;

/// Longest `sessions/<pid>.json` read: the file is a few hundred bytes.
const MAX_SESSION_FILE_BYTES: u64 = 64 * 1024;

/// How many directories [`git_head`] climbs looking for `.git`.
const MAX_GIT_CLIMB: usize = 64;

/// Longest `HEAD` or `.git` file read: both are one short line.
const MAX_GIT_FILE_BYTES: u64 = 4096;

/// The mark before model + effort.
pub const MODEL_MARK: char = '\u{25C6}'; // ◆
/// The mark before the working directory.
pub const PATH_MARK: char = '\u{2302}'; // ⌂
/// The mark before the branch.
pub const BRANCH_MARK: char = '\u{2387}'; // ⎇
/// The mark before the account the process runs on.
pub const ACCOUNT_MARK: char = '\u{2386}'; // ⎆
/// The mark before a limit wall the session hit.
pub const WALL_MARK: char = '\u{29D7}'; // ⧗
/// The mark before the session's usage.
pub const USAGE_MARK: char = '\u{03A3}'; // Σ
/// The mark inside a limit wall that stands for "resets" (`⧗ 5h ♻ 3pm`). Not
/// `Emoji_Presentation`, so it draws as one narrow text cell.
pub const RESET_MARK: char = '\u{267B}'; // ♻
/// The mark before a live upgrade that waits on the person at this composer
/// ([`UpgradeNote`]): the band's mark for a build waiting to come in (`aterm
/// vX is ready`).
pub const UPGRADE_MARK: char = '\u{21E3}'; // ⇣

/// Most transcript bytes one [`FooterCache::usage`] folds, so a long session
/// catches up over several reads without holding the resolver thread. Until
/// caught up, the footer names no tokens.
pub const FOLD_BUDGET: u64 = 64 * 1024 * 1024;

/// The pitch the facts are set at, in cells ([`set_facts`]). A fact glyph is
/// drawn at seven tenths of its cell (`aterm_render::SCRIPT_SCALE`), so one per
/// cell reads letter by letter; four fifths of a cell is the scaled face's own
/// pitch plus a little air.
pub const FACT_PITCH: f32 = 0.8;

/// The space between two facts, in pitches: three, so a fact's words (one
/// pitch apart) hold together and the facts read as four, not as one line.
pub const GAP: f32 = 3.0;

/// The steps a cell is divided into for a fact glyph's sideways move
/// ([`SetGlyph::nudge`]): `aterm_core::terminal::Script::NUDGES_PER_CELL`,
/// which the painter asserts is this.
pub const NUDGES_PER_CELL: i8 = 16;

/// Rule glyphs left at the left end of the run aterm writes into ([`rule_run`]),
/// so the rule still reads as a rule however much it carries.
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
    /// The running build's version, as Claude Code records it (`2.1.283`),
    /// when the file carries one.
    pub version: Option<String>,
    /// The process's start (unix seconds), from the file's `procStart` — the
    /// floor [`tail_facts`] takes where the kernel's own start cannot be read.
    pub proc_start: Option<u64>,
    /// When the running image registered (unix seconds, from `startedAt` ms).
    /// An in-place `execve` keeps pid and kernel start time but writes a new
    /// `startedAt`, so the floor moves with the image ([`floor_of`]).
    pub started_at: Option<u64>,
}

/// Whose facts these are: the transcript floor and session they were read for.
/// A kept launch card ([`launch_card`]) belongs to one owner, so a new floor or
/// session id drops it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FactsOwner {
    /// The floor (unix seconds) under the rows that speak for the process.
    pub floor: Option<u64>,
    /// The session id the registry names.
    pub session_id: String,
}

/// The facts the footer shows. Every one is optional: a fresh session has no
/// transcript row yet, a directory may not be a repository, and the footer
/// shows what it knows rather than a placeholder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FooterFacts {
    /// The model's display name (`Opus 5.5`).
    pub model: Option<String>,
    /// The effort level (`xhigh`).
    pub effort: Option<String>,
    /// Where the session's Bash tool stands, else its launch directory, home
    /// shown as `~` (`~/aterm`).
    pub path: Option<String>,
    /// The checked-out branch, or a short commit id when HEAD is detached.
    pub branch: Option<String>,
    /// The working directory when the repository read failed with `EPERM`
    /// (macOS folder privacy consent). Not shown; the host raises consent
    /// attention ([`GitHeadRead::Denied`]).
    pub repo_read_denied: Option<PathBuf>,
    /// The Claude Code build this process runs (`2.1.283`). Not shown; names
    /// the build in the drift log, and a launch card is taken only from this
    /// build ([`FooterFacts::filled_from`]).
    pub version: Option<String>,
    /// The command resuming this process's own conversation (`claude --resume
    /// <id>` with its launch flags, [`crate::harness::resume::of_entry`]), for
    /// the frozen-program remedy; never `--continue`, which can pick a sibling
    /// tab's conversation. [`facts_for_entry_at`] leaves it unset: it needs the
    /// argv [`read_pid`]'s caller supplies.
    pub resume: Option<String>,
    /// The session's usage ([`super::session_usage`]); only
    /// [`FooterCache::usage`] fills it.
    pub usage: Option<super::session_usage::UsageFacts>,
    /// Nothing since the process started decided the model — no answer, no
    /// model result, no `--model`: its launch card may name it.
    pub model_open: bool,
    /// Nothing since the process started decided the effort.
    pub effort_open: bool,
    /// While [`Self::effort_open`]: the model a launch card must name for its
    /// effort to count, set when a model switch named no effort.
    pub effort_for: Option<String>,
    /// Whose facts these are, when the registry named a session.
    pub owner: Option<FactsOwner>,
    /// The live upgrade waiting on the person here, in the words the composer
    /// now calls for ([`UpgradeNote::words`]). Laid in by the window from its
    /// upgrade host's row for the tab, never read off Claude Code's files.
    /// Shown first, and given up as a light's reason is ([`facts_ladder`]).
    pub upgrade: Option<UpgradeWords>,
    /// The roster's label for the login this process runs under
    /// ([`super::accounts::footer_label`]: only where two or more accounts
    /// are listed). Never read off Claude Code's files: only the window's
    /// resolver fills it, from the process's own exec environment. Shown
    /// after the model, whole or not at all ([`facts_ladder`]).
    pub account: Option<String>,
}

/// One wording of an [`UpgradeNote`]: whole, and short for a narrow rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UpgradeWords {
    /// `Claude Code 2.1.286 is ready · waits while you use this tab`.
    pub whole: String,
    /// `2.1.286 waits while you use this tab`.
    pub short: String,
}

/// What the footer says of a live upgrade that waits only on the person at
/// this composer (`upgrade_status::Row::pause_note`): what holds it, never when
/// it moves, since how long a person's hand holds it differs by rung. The
/// window picks one of two wordings from its own read of the composer as it
/// draws ([`Self::words`]), so the note follows the person from typing a prompt
/// through the pause that leaves it unsent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UpgradeNote {
    /// While the composer holds no draft: `… waits while you use this tab`.
    /// `None` for a move the draft held at its look: the composer read empty
    /// since, the draft was sent (the turn it started holds the move) or
    /// cleared, and the next look says what holds it now.
    pub using: Option<UpgradeWords>,
    /// While a draft stands in the composer: `… waits until your draft is
    /// sent or cleared`.
    pub draft: UpgradeWords,
}

impl UpgradeNote {
    /// The words for a composer that holds a person's draft (`draft`) or
    /// none, if any.
    #[must_use]
    pub fn words(&self, draft: bool) -> Option<&UpgradeWords> {
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
    /// The model `--model` names, only when it is a real id
    /// (`claude-opus-5-5`); an alias resolves per account and build.
    pub model: Option<String>,
    /// The model is pinned from launch: any `--model`, or an environment pin
    /// (`ANTHROPIC_MODEL`, `ANTHROPIC_DEFAULT_<FAMILY>_MODEL`). Claude then
    /// restores no conversation's model onto it. A restoring launch resume
    /// also pins, which the transcript decides ([`TailCache`]).
    pub pinned: bool,
    /// The conversation the launch resumed (`--resume`, `--continue`), when
    /// it resumed one ([`LaunchResume`]).
    pub resume: Option<LaunchResume>,
}

/// The conversation a process's launch resumed
/// ([`crate::harness::upgrade::launch_resume`]). Claude restores its model at
/// launch unless [`LaunchFacts::pinned`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LaunchResume {
    /// `--resume <id>` / `-r <id>`: the process's first session is the
    /// conversation it names.
    Session(String),
    /// A resume that names no conversation the process keeps: `--continue`,
    /// `-c`, a bare `--resume` (Claude's picker), or any resume with
    /// `--fork-session` (the conversation takes a new id).
    Unnamed,
}

/// What `argv` (argv[0] included) says about the model: the last `--model`
/// Claude's parser takes ([`crate::harness::upgrade::launch_model`]), when a
/// real id. Harness model moves relaunch with one (`relaunch::with_model`).
#[must_use]
pub fn launch_facts(argv: &[String]) -> LaunchFacts {
    launch_facts_of(argv, &[])
}

/// [`launch_facts`], and whether the process's environment at exec (`env`,
/// `KEY=value` entries) pins its model ([`LaunchFacts::pinned`]).
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
    LaunchFacts {
        model,
        pinned,
        resume,
    }
}

/// One marked value in the footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The mark ([`MODEL_MARK`], [`PATH_MARK`], [`BRANCH_MARK`]).
    pub mark: char,
    /// The value after it.
    pub text: String,
}

/// The Claude Code directory of one process, from its environment at exec
/// (`config_dir`, its raw `CLAUDE_CONFIG_DIR`; `home`, its raw `HOME`) and the
/// directory it runs in (`cwd`, asked only for a relative one), by the one
/// resolver every reader asks (`upgrade_drive::claude_home_dir`, as Claude Code
/// resolves `CLAUDE_CONFIG_DIR ?? join(homedir(), ".claude")`): a set one
/// absolute as it is, relative (empty included) under `cwd` (`None` where that
/// is not read), an unset one `$HOME/.claude` (`None` with no absolute `HOME`).
/// Never aterm's own environment: aterm strips `CLAUDE_*` from its shells.
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

/// Open `path` for reading, non-blocking, so a FIFO planted where a `HEAD`
/// or a transcript should be cannot park the reader thread forever.
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

/// `file` if it is a regular file, judged on the open handle (no
/// check-then-open race).
fn regular(file: File) -> Option<File> {
    file.metadata().ok()?.is_file().then_some(file)
}

/// Open `path` for reading only if it is a regular file ([`regular`]).
pub(crate) fn open_regular(path: &Path) -> Option<File> {
    regular(open_nonblocking(path).ok()?)
}

/// At most `cap` bytes of `file`, as text.
fn read_capped(file: File, cap: u64) -> Option<String> {
    let mut text = String::new();
    file.take(cap).read_to_string(&mut text).ok()?;
    Some(text)
}

/// At most `cap` bytes of the regular file at `path`, as text.
pub(crate) fn read_small(path: &Path, cap: u64) -> Option<String> {
    read_capped(open_regular(path)?, cap)
}

/// The registry entry for Claude Code process `pid`, or `None` when that pid
/// is not a Claude Code session (or its file cannot be read). `started` is the
/// kernel's start time of `pid` (unix seconds) where the caller could read it
/// — see [`parse_session_entry`].
pub fn session_of_pid(claude_dir: &Path, pid: u32, started: Option<u64>) -> Option<SessionEntry> {
    let path = claude_dir.join("sessions").join(format!("{pid}.json"));
    #[cfg(test)]
    tests::before_registry_read(&path);
    parse_session_entry(&read_small(&path, MAX_SESSION_FILE_BYTES)?, pid, started)
}

/// [`session_of_pid`]'s parse, pure. The file must name the same `pid`, and,
/// when both are known, its `procStart` must match the kernel start second,
/// so a stale row from a reused pid is rejected.
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

/// Whether `s` has the shape of a Claude Code session id — the transcript's
/// file stem, so letters, digits and `-` only.
#[must_use]
pub fn is_session_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Runs of whitespace collapsed to one space, ends trimmed.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A unix time as Claude Code records `procStart` — `ps -o lstart=` in UTC
/// and the C locale — whitespace-squashed: `Wed Sep 23 20:13:55 2026`.
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

/// Claude Code's project directory name for `cwd`: every character that is
/// not ASCII alphanumeric becomes `-` (`/Users//ana/aterm` → `-Users-ana-aterm`).
pub fn project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The transcript of `entry`: `<claude dir>/projects/<slug>/<id>.jsonl`, and
/// when the slug guess misses (a vendor spelling change), a scan of the
/// project directories for the id. `None` until the session writes one.
pub fn transcript_path(claude_dir: &Path, entry: &SessionEntry) -> Option<PathBuf> {
    #[cfg(test)]
    tests::on_transcript_lookup();
    let projects = claude_dir.join("projects");
    let file = format!("{}.jsonl", entry.session_id);
    let direct = projects.join(project_slug(&entry.cwd)).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|dir| dir.path().join(&file))
        .find(|candidate| candidate.is_file())
}

/// [`transcript_path`] for a reader that must not guess between two: the
/// project's own `<claude dir>/projects/<slug>/<id>.jsonl`, else the one
/// project directory holding the id (two of them are none). The supervisor's
/// wall confirmation reads through this (`supervise/wall_confirm.rs`), so a
/// file planted under another project's name can neither stand in for the
/// transcript nor be picked over it.
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

/// What the last [`TAIL_BYTES`] of the transcript at `path` say about the
/// model and effort, counting only rows written no earlier than `since`
/// ([`tail_facts`]).
pub fn read_tail_facts(path: &Path, since: Option<u64>) -> Option<TailFacts> {
    Some(tail_facts(&read_tail(path)?, since))
}

/// The most bytes one refresh reads as an append to a transcript it has read
/// before ([`TailCache`]'s carry); a larger append is read as a fresh tail.
const MAX_APPEND_BYTES: u64 = 16 * 1024 * 1024;

/// How many bytes before the carried end are checked unchanged before an
/// append is read on top of what was read before.
const GUARD_BYTES: u64 = 4096;

/// How far back from its end a transcript is read afresh for a fact the
/// newest rows do not decide ([`TailCache`]).
const MAX_BACK_BYTES: u64 = 16 * 1024 * 1024;

/// The resolver's per-session transcript memo, keyed by process and image
/// start. The registry and repository are read on every refresh; only an
/// unchanged transcript tail is reused.
///
/// A fresh read looks back a [`TAIL_BYTES`] window at a time while the newest
/// rows decide nothing, up to [`MAX_BACK_BYTES`]. A grown transcript is read as
/// an append (same file, process and floor, [`GUARD_BYTES`] before the old end
/// unchanged, under [`MAX_APPEND_BYTES`]): only new whole rows are scanned and
/// what they leave undecided is carried, so one huge row cannot blank a model
/// named above it. Anything else is read afresh.
///
/// Model and effort are the process's, not the conversation's. `/clear` and
/// in-REPL `/resume` give the process a new session id with the same
/// `startedAt`, so the cache keeps what the process last decided and whether
/// its model is pinned, and a silent new session inherits it
/// ([`TailCache::for_process`]). On a switch the session left is read once more
/// and searched for a model choice, which pins ([`chose_model_since`]).
///
/// A switch came after the process was last seen in the session it left. A
/// conversation whose file or first main-chain row predates that moment is a
/// `/resume` ([`existed_before`]), which restores its model unless the process
/// pinned its own ([`conversation_model`]); `/branch` copies carry `forkedFrom`
/// and restore nothing. Rows in a switched-into session speak for the process
/// only from that moment ([`TailCache::session_floor`]); at first sight the
/// process's floor stands in.
///
/// Pins, which no session switch undoes: [`LaunchFacts::pinned`], a choice in a
/// left session, a switch into an older conversation holding an answer (a
/// restore), and at first sight a first session that predates the floor and
/// holds an answer. At first sight the restored model is named only when the
/// launch resumed this session ([`LaunchFacts::resume`]).
///
/// Limits: two switches between reads look like one; another process's rows
/// written between the last read and the switch are taken as this process's; a
/// first sight (e.g. after an update handoff) knows nothing of earlier pins.
/// Process facts survive [`TailCache::clear`]; another process or a new floor
/// starts them afresh.
#[derive(Debug, Default)]
pub struct TailCache {
    entry: Option<(TailKey, TailRead)>,
    #[cfg(unix)]
    carry: Option<Carry>,
    process: Option<ProcessFacts>,
    #[cfg(test)]
    reads: usize,
    #[cfg(test)]
    appends: usize,
}

/// One read of a transcript since the floor: what it decided, whether it met
/// a row from before the floor (the conversation was resumed), and where the
/// Bash tool stands ([`last_cwd_since`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TailRead {
    facts: TailFacts,
    resumed: bool,
    cwd: Option<PathBuf>,
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
    /// wall-clock time of the newest read that found it there. A switch out
    /// of `session` came after it.
    seen: u64,
    facts: TailFacts,
    /// The process's model is pinned by what this cache has read of it: a
    /// choice in a session it left ([`chose_model_since`]), or a restore — a
    /// switch into, or a first session that is, a resumed conversation with
    /// an answer to restore ([`TailCache`]). The launch's own pin
    /// ([`LaunchFacts::pinned`]) is not kept here: it comes with every read.
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
    // A same-length rewrite can preserve mtime, while a replacement can
    // preserve both mtime and length. On Unix, ctime and inode cover them.
    // Other targets keep reading rather than trusting an incomplete stamp.
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
        // Without a change-time and file identity the key cannot be trusted:
        // cache on Unix only.
        #[cfg(not(unix))]
        {
            let _ = (path, pid, started, since, metadata);
            return None;
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
    /// [`Self::read_since`]'s facts alone.
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
    /// and identifying this exact regular file. A read that races a writer is
    /// returned for this refresh but never remembered; the next one retries.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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

    /// The transcript read afresh, newest first, a [`TAIL_BYTES`] window at a
    /// time, until both facts are decided, a row before the floor is met, the
    /// file's start, or [`MAX_BACK_BYTES`] — so an answer above one huge row
    /// (a pasted image, a large tool result) is still found, however the
    /// reads fell between them. The carry is seeded from what it read.
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
        let mut hi = len;
        // The head of a row a window's start cut: completed by the next,
        // older, window — dropped when it is already longer than any row
        // this reads ([`MAX_LINE_BYTES`]), which the next window's cut tail
        // then cannot parse either.
        let mut cut: Vec<u8> = Vec::new();
        // Just past the newest whole row: where an append starts.
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
            let (older, floored) = scan(&window[from..], Some(since));
            facts = facts.over(older);
            resumed |= floored;
            // The newest stamped row: a window of only oversized rows
            // has none, so older windows are searched until one does.
            if cwd.is_none() {
                cwd = last_cwd_since(&window[from..], Some(since));
            }
            cut = if from > MAX_LINE_BYTES {
                Vec::new()
            } else {
                window[..from].to_vec()
            };
            if floored || facts.settled() || lo == 0 || len - lo >= MAX_BACK_BYTES {
                break lo == 0;
            }
            hi = lo;
        };
        let read = TailRead {
            facts,
            resumed,
            cwd,
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
        let (newer, floored) = scan(&appended[..whole], Some(since));
        let cwd = last_cwd_since(&appended[..whole], Some(since));
        // A row stamped before the floor in the append puts every row above
        // it — the carried ones too — before this process.
        let facts = if floored {
            TailRead {
                facts: newer,
                resumed: true,
                cwd,
            }
        } else {
            TailRead {
                facts: newer.over(std::mem::take(&mut carry.facts.facts)),
                resumed: carry.facts.resumed,
                cwd: cwd.or_else(|| carry.facts.cwd.take()),
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

    /// Forget every transcript read: the next read is a fresh one. What the
    /// process decided is kept; it is keyed by the process itself.
    pub fn clear(&mut self) {
        self.entry = None;
        #[cfg(unix)]
        {
            self.carry = None;
        }
    }

    /// What this cache keeps of the process (`pid`, `started`, `floor`).
    fn process_of(&self, pid: u32, started: Option<u64>, floor: u64) -> Option<&ProcessFacts> {
        self.process
            .as_ref()
            .filter(|p| p.pid == pid && p.started == started && p.floor == floor)
    }

    /// The floor under `session`'s rows that speak for the process: its own
    /// floor at first sight, or, in a session it switched into, the moment it
    /// was last seen in the session it left (never below the floor).
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
    /// process's effort standing only for that model. The result is remembered
    /// as the process's decision, and [`Switch::now`] as when it was last seen
    /// in `session`.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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
                // The switch came after the process was last seen in the
                // session it left: `session_floor` is that moment.
                let restored = (!pinned && !switch.launch_pinned)
                    .then(|| (switch.restored)(Some(session_floor)))
                    .flatten();
                // The restore sets the override every later resume obeys.
                let pinned = pinned || restored.is_some();
                let process = match restored {
                    Some(model) => restored_facts(model).over(process),
                    None => process,
                };
                (tail.over(process), pinned)
            }
            None => {
                // A launch pin needs no reading: it comes with every read.
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

/// The facts a restore leaves: the restored model, with the effort open for
/// that model only ([`TailFacts::over`]), or unknown if the model is unreadable.
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
    }
}

/// How [`TailCache::for_process`] reads a session switch of one process, or
/// its first sight of it.
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
    /// or no answer. Given the switch moment ([`resumed_since`]), or `None` at
    /// first sight, where the floor stands in ([`resumed_model`]).
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

/// Every row of the transcript at `path`, newest first, a [`TAIL_BYTES`]
/// window at a time, until `visit` breaks, the file's start, or
/// [`MAX_BACK_BYTES`]. Oversized rows are met once as [`Row::Oversized`].
/// `Some(true)` when the file's start was reached, `None` when unreadable.
fn rows_back(
    path: &Path,
    mut visit: impl FnMut(Row<'_>) -> std::ops::ControlFlow<()>,
) -> Option<bool> {
    let mut file = open_regular(path)?;
    let len = file.metadata().ok()?.len();
    let mut hi = len;
    let mut cut: Vec<u8> = Vec::new();
    // The newest bytes of the next window continue an oversized row whose
    // end was met already: the rest of it is not a row of its own.
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
            // The oversized row starts in this window: its head is the
            // newest segment here.
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

/// Whether the transcript at `path` holds, since `floor`, a model choice by
/// the process: a `/model` command row or a `/fast` result that moved the
/// model. Either pins the model across an in-REPL `/resume` ([`TailCache`]).
/// Searched back to the first row before the floor, or [`MAX_BACK_BYTES`].
fn chose_model_since(path: &Path, floor: u64) -> bool {
    use crate::harness::upgrade_models::{ModelResultKind, model_result_of};
    use std::ops::ControlFlow::{Break, Continue};
    let mut chose = false;
    rows_back(path, |row| {
        // A command row and its result are short: an oversized row is
        // neither.
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

/// How much of a transcript's head [`head_stamp`] reads: its first rows
/// are a few short ones (`mode`, `permission-mode`, the first system row).
const HEAD_BYTES: u64 = 64 * 1024;

/// The stamp of the first stamped row among the whole rows of the file's first
/// [`HEAD_BYTES`]. With `own_rows`, only a main-chain message row (`user`,
/// `assistant`, `system`) without `forkedFrom` counts: `/branch` copies keep
/// their originals' stamps, and its copied `memory-mode` rows are not the
/// file's own. `None` when there is no such stamp or the head is unreadable.
fn head_stamp(file: &mut File, own_rows: bool) -> Option<u64> {
    let head = read_range(file, 0, HEAD_BYTES)?;
    // A full read may end mid-row: only rows a newline closes count.
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

/// Whether the transcript at `path` began before `floor` ([`head_stamp`]): a
/// conversation older than the process, which it resumed. `false` when
/// unreadable.
fn began_before(path: &Path, floor: u64) -> bool {
    open_regular(path)
        .and_then(|mut file| head_stamp(&mut file, false))
        .is_some_and(|at| at < floor)
}

/// Whether the transcript at `path` existed before `moment` (the last sighting
/// in the session switched out of): created before it, where the filesystem
/// keeps a birth time, or its first own main-chain row ([`head_stamp`]) is
/// older. `/clear` and `/branch` sessions are created by the switch, so an
/// existing one was resumed. `false` when unreadable.
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

/// The top-level stamp at an oversized row's end: the last `"timestamp":"…"`
/// in `part`. `None` when it names none.
fn last_stamp(part: &[u8]) -> Option<u64> {
    const KEY: &[u8] = b"\"timestamp\":\"";
    let at = part.windows(KEY.len()).rposition(|w| w == KEY)? + KEY.len();
    let rest = &part[at..];
    let end = rest.iter().position(|&b| b == b'"')?;
    std::str::from_utf8(&rest[..end])
        .ok()
        .and_then(crate::harness::upgrade_models::parse_utc)
}

/// At first sight: when the transcript is a conversation resumed from before
/// `floor` (`met_before`, or [`began_before`]), the model Claude restores
/// ([`conversation_model`]); `None` otherwise or with no answer to restore.
fn resumed_model(path: &Path, floor: u64, met_before: bool) -> Option<Said> {
    (met_before || began_before(path, floor))
        .then(|| conversation_model(path, floor))
        .flatten()
}

/// At a switch after `moment`: when the transcript existed before it
/// ([`existed_before`]), the model Claude restored ([`conversation_model`]);
/// `None` for a created session or no answer to restore.
fn resumed_since(path: &Path, moment: u64) -> Option<Said> {
    existed_before(path, moment)
        .then(|| conversation_model(path, moment))
        .flatten()
}

/// The model a resumed conversation last answered with before `before`, read as
/// Claude reads it: the newest main-chain `assistant` row, not meta, whose
/// model is not `<synthetic>`. [`Said::Unread`] when that model is not plainly
/// an id, an answer has no stamp, an oversized row may be that answer, or the
/// search ran out; never an older answer's model. `None` when there is no
/// answer before `before` (Claude restores nothing).
fn conversation_model(path: &Path, before: u64) -> Option<Said> {
    use std::ops::ControlFlow::{Break, Continue};
    let mut found = None;
    let whole = rows_back(path, |row| {
        let raw = match row {
            Row::Whole(raw) => raw,
            Row::Oversized(part) => {
                // Bare `"assistant"` is JSON structure (`type` or `role`);
                // inside a string the quotes would be escaped.
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
            // Written since `before`: the process's own, after it took the
            // conversation up, or not certainly before the switch.
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

/// The resolver's per-session usage fold
/// ([`super::session_usage::SessionUsage`]), so each read folds only what was
/// appended. Dropping it releases the fold. The transcript tail lives in
/// [`TailCache`], which the host keeps for the process's life.
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
}

impl Default for FooterCache {
    fn default() -> Self {
        Self {
            usage: None,
            refreshed: None,
            wall_basis: None,
            budget: FOLD_BUDGET,
        }
    }
}

/// The main transcript's newest limit-notice row per window, and the newest
/// response served in any of the session's files
/// ([`super::session_usage::wall_of`]'s two readings).
type WallBasis = (
    std::collections::BTreeMap<String, super::usage::LimitNoticeRow>,
    Option<i64>,
);

impl FooterCache {
    /// The usage of the conversation in `transcript` (the one the same registry
    /// read named, [`read_pid`]), folded on from the last read unless no file
    /// moved ([`super::session_usage::SessionUsage::moved`]). A new transcript
    /// starts from nothing. Tokens are named only once every followed file is
    /// read to its end, and the wall comes from the last caught-up read. A
    /// session with no transcript yet shows nothing, never the previous one's.
    /// The fold goes with the cache.
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
                _ => usage.refresh(self.budget),
            };
            self.refreshed = Some(refresh);
            if refresh.caught_up {
                let notices = usage.main().fold().limit_notices().clone();
                self.wall_basis = Some((notices, usage.served_at()));
            }
        }
        self.shown(now, offset_at)
    }

    /// What the fold as last read shows at `now`.
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

    /// Drop the fold and what was read from it.
    fn forget(&mut self) {
        self.usage = None;
        self.refreshed = None;
        self.wall_basis = None;
    }
}

/// The whole rows in the last [`TAIL_BYTES`] of the transcript at `path`.
fn read_tail(path: &Path) -> Option<Vec<u8>> {
    let mut file = open_regular(path)?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    let mut bytes = read_range(&mut file, start, len)?;
    // A tail that starts mid-file starts mid-line: that first fragment is
    // never a whole row.
    if start > 0 {
        let first = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |nl| nl + 1);
        bytes.drain(..first);
    }
    Some(bytes)
}

/// Bytes `from..to` of `file` (fewer, if it shrank meanwhile).
fn read_range(file: &mut File, from: u64, to: u64) -> Option<Vec<u8>> {
    read_range_then(file, from, to, &[])
}

/// Bytes `from..to` of `file` (fewer if it shrank), then `then`, in one buffer
/// sized for both.
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

/// How many `sessions/<pid>.json` files [`shell_cwds`] reads at most.
const MAX_SESSION_FILES: usize = 256;

/// Where the Bash tool of each live Claude Code session launched in `launch`
/// stands, per its transcript ([`last_cwd`]): the directory its next command
/// starts in, which the shell's own cwd does not track. Every live session
/// whose registry entry names that launch directory counts (a superset is
/// what callers want). `launch` itself is left out; empty when unreadable.
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

/// The `cwd` of the newest main-thread row that carries one: Claude Code
/// stamps rows with its working directory, which follows the Bash tool.
/// Sidechain rows are skipped; a relative or empty value is not a directory.
pub fn last_cwd(body: &[u8]) -> Option<PathBuf> {
    last_cwd_since(body, None)
}

/// [`last_cwd`] of the rows since `since` (unix seconds, the process's floor
/// as [`scan`] takes it): a row stamped before it ends the search, and an
/// unstamped row is skipped.
fn last_cwd_since(body: &[u8], since: Option<u64>) -> Option<PathBuf> {
    for raw in body.rsplit(|&b| b == b'\n') {
        if raw.is_empty() || raw.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(line) = std::str::from_utf8(raw) else {
            continue;
        };
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

/// What the transcript since the process started says about one of the
/// footer's facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Said {
    /// Nothing since the start names it: the launch flag or the launch card
    /// may ([`facts_for_entry_at`]).
    #[default]
    Unsaid,
    /// The newest statement is one this build cannot read (an unparsed `/model`
    /// result, effort `auto`): unknown, and nothing older stands in.
    Unread,
    /// The newest statement names it, as the footer shows it.
    Is(String),
}

impl Said {
    fn decided(&self) -> bool {
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
    /// The model: the newest of an answer's `message.model` and a model
    /// result's name (`Opus 5.5`), as the footer shows it.
    pub model: Said,
    /// The effort: the newest of an answer's `effort`, an `/effort` result and
    /// a model result's `with … effort`; across a switch that named no effort,
    /// only an effort read for the new model ([`Self::effort_for`]).
    pub effort: Said,
    /// A model switch that named no effort while the effort was open, and the
    /// model switched to. Claude runs each model at its own effort, so the
    /// earlier effort stands only if the older rows' model is this one
    /// ([`Self::over`]); `Some` when nothing older named a model, leaving it
    /// to the launch flag or card ([`facts_for_entry_at`]).
    effort_for: Option<String>,
}

impl TailFacts {
    /// `self` (newer rows) over `older`: what the newer rows decided stands,
    /// and a switch in them that named no effort takes the older effort only
    /// when the older rows' model is the one it switched to.
    fn over(self, older: Self) -> Self {
        let (effort, effort_for) = match self.effort_for {
            None if self.effort.decided() => (self.effort, None),
            None => (older.effort, older.effort_for),
            Some(to) => match &older.model {
                // Nothing older names the model before the switch: still open.
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
                // Another model, or one this build cannot read, ran before.
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
        }
    }

    /// Nothing older can change what these rows say.
    fn settled(&self) -> bool {
        self.model.decided() && self.effort.decided() && self.effort_for.is_none()
    }

    /// A row naming the model in force (`said`), met newest first: sets the
    /// model if nothing newer did, and settles a newer effort-less switch.
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

    /// The effort, decided by a row with no model of its own.
    fn decide_effort(&mut self, said: Said) {
        if said == Said::Unread {
            self.effort_for = None;
        }
        self.effort = said;
    }
}

/// The row Claude Code writes when a `/model` choice is made: a `type:user` row
/// whose `message.content` starts with this tag; its result is the next such
/// row ([`COMMAND_STDOUT`]). A picker that changed nothing writes `type:system`
/// `local_command` rows (`Kept model as …`), from which [`scan`] reads nothing.
const MODEL_COMMAND: &str = "<command-name>/model</command-name>";

/// The row Claude Code writes for an `/effort` command, its result after it.
const EFFORT_COMMAND: &str = "<command-name>/effort</command-name>";

/// The tag every local command's row opens with (`/model`, `/effort`,
/// `/fast`, …): a command's result is the row directly under it.
const COMMAND_NAME: &str = "<command-name>";

/// The tag a local command's result row opens with.
const COMMAND_STDOUT: &str = "<local-command-stdout>";

/// The effort levels the footer names, as `/effort` takes them; the top-effort
/// mode ([`CLAUDE_TOP_EFFORT_KEY`]) shows as `xhigh`, the level it runs at.
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

/// A model as a result or launch card spells it, as the footer shows it:
/// `Opus 5.5 (default)` → `Opus 5.5` (also dropping ` (1M context)` and
/// ` (recommended)`); an id goes through [`model_display`]. `None` for anything
/// not plainly a name (at most 64 chars of letters, digits, spaces, `.-_()[]`).
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

/// What an `/effort` result says: its level ([`effort_level`]), or
/// [`Said::Unread`] for `auto` or an unknown level. `None` when not an effort
/// result.
fn effort_result(content: &str) -> Option<Said> {
    let body = content
        .trim_start()
        .strip_prefix(COMMAND_STDOUT)?
        .trim_start();
    let body = crate::harness::upgrade_models::strip_sgr(body);
    if body.starts_with(aterm_phase::anchor("effort.auto")) {
        return Some(Said::Unread);
    }
    ["effort.set", "effort.current"].into_iter().find_map(|id| {
        let rest = body.strip_prefix(aterm_phase::anchor(id))?;
        let word: String = rest
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        Some(effort_level(&word).map_or(Said::Unread, |level| Said::Is(level.to_owned())))
    })
}

/// A local command's result row folded into `facts`, newest first. A model
/// result ([`crate::harness::upgrade_models::model_result_of`]) names the model
/// ([`TailFacts::meet_model`]) and any `with … effort`; without that clause the
/// earlier effort stands only for the same model. An `/effort` result names
/// the effort. Returns whether a readable model was named, so [`scan`] can
/// pair a `/model` command row with its own result.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "ClaudeFooterModel",
        action = "Read",
        project = "conformance_footer_model::project"
    )
)]
fn fold_result(content: &str, facts: &mut TailFacts) -> bool {
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
        return readable;
    }
    if !facts.effort.decided()
        && let Some(said) = effort_result(content)
    {
        facts.decide_effort(said);
    }
    false
}

/// [`read_tail_facts`]'s scan, pure: rows newest first, stopping once both facts
/// are decided. Sidechain rows and `<synthetic>` models are skipped.
///
/// The model comes from an answer's `message.model` or a `/model`/`/fast`
/// result ([`fold_result`]), so a choice shows at once. A `/model` command row
/// whose next row is not a readable model result makes model and effort
/// [`Said::Unread`]. A `/fast` result without `model set to` decides nothing.
/// The effort comes from an answer's `effort`, an `/effort` result, or a model
/// result's `with … effort`; a switch without it keeps the earlier effort only
/// for the same model ([`TailFacts`]).
///
/// `since` is the process's start (unix seconds). The file is in append order,
/// so the scan stops at the first row stamped before it: that row and all
/// above belong to a predecessor. Unstamped rows are attributed to no one.
/// `None` reads the tail as is; [`facts_for_entry_at`] does not show that.
pub fn tail_facts(body: &[u8], since: Option<u64>) -> TailFacts {
    scan(body, since).0
}

/// [`tail_facts`], and whether the scan stopped at a row before the floor.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "ClaudeFooterModel",
        action = "Read",
        project = "conformance_footer_model::project"
    )
)]
fn scan(body: &[u8], since: Option<u64>) -> (TailFacts, bool) {
    let mut facts = TailFacts::default();
    // The command or result row met last — the one directly under the next
    // command row up — was a readable model result: that command's own.
    let mut under = false;
    for raw in body.rsplit(|&b| b == b'\n') {
        if facts.settled() {
            break;
        }
        if raw.is_empty() || raw.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(line) = std::str::from_utf8(raw) else {
            continue;
        };
        // Cheap prefilters: most rows carry neither fact, and a parse per row
        // over half a MiB of tool output is the cost worth skipping.
        let command = line.contains(COMMAND_STDOUT) || line.contains(COMMAND_NAME);
        let model_open = !facts.model.decided() || facts.effort_for.is_some();
        let wants_model = model_open && (command || line.contains("\"assistant\""));
        let wants_effort = !facts.effort.decided() && (command || line.contains("\"effort\""));
        if !wants_model && !wants_effort {
            continue;
        }
        // Only a possible command or result row keeps its string content;
        // others keep `type`, `isSidechain`, `timestamp`, `effort`, `message.model`.
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
                if content.starts_with(MODEL_COMMAND) && !answered {
                    // No readable result under it: a choice this build
                    // cannot read, which may have moved the effort too.
                    facts.meet_model(Said::Unread);
                    if !facts.effort.decided() {
                        facts.decide_effort(Said::Unread);
                    }
                } else if content.starts_with(EFFORT_COMMAND) && !facts.effort.decided() {
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
            // The model this answer ran with: its effort below is for it.
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

/// A value safe to paint as-is: short, and only letters, digits, `-`, `.`,
/// `_`, `[`, `]`.
fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '[' | ']'))
}

/// A model id's display name: `claude-opus-5-5` → `Opus 5.5`,
/// `claude-haiku-4-5-20251001` → `Haiku 4.5` (trailing date dropped),
/// `claude-opus-5-5[1m]` → `Opus 5.5`. Other shapes are shown unchanged.
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

/// The branch `cwd` is in, read from `.git` without running `git`. A worktree's
/// `.git` file is followed; a detached HEAD shows seven hex digits. `None`
/// outside a repository or when the read was refused.
pub fn git_head(cwd: &Path) -> Option<String> {
    match read_git_head(cwd) {
        GitHeadRead::Found(branch) => Some(branch),
        GitHeadRead::Absent | GitHeadRead::Denied => None,
    }
}

/// What [`read_git_head`] found for a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHeadRead {
    /// The branch.
    Found(String),
    /// Not inside a repository (or not one this bounded read can parse).
    Absent,
    /// A read failed with `EPERM` (macOS folder privacy consent); the host
    /// raises consent attention rather than showing nothing.
    Denied,
}

/// `EPERM`, the kernel's answer to a read a privacy consent refuses (distinct
/// from an ordinary `EACCES` permission bit).
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

/// Whether `path` is inside a file-provider domain of `home` (iCloud Drive, a
/// third-party sync provider): the one class a Full Disk Access grant does not
/// cover, whose open raises a system dialog with no timeout that parks the
/// opening thread (here the footer-resolver thread every Claude session
/// shares). The path is resolved first (`realpath` is not gated,
/// docs/DESIGN-macos-tcc-prompts-2026-08-30.md §1.5), so a symlink out of an
/// ordinary folder is caught; the predicate itself is lexical. No `home`:
/// nothing to fence against.
pub(crate) fn under_file_provider_domain(home: Option<&Path>, path: &Path) -> bool {
    let Some(home) = home else {
        return false;
    };
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    atpkg::protected::under_file_provider_domain(&home, &resolved)
}

/// [`git_head`], saying why there is no answer when the kernel refused a read.
pub fn read_git_head(cwd: &Path) -> GitHeadRead {
    read_git_head_in(aterm_types::dirs::home_dir().as_deref(), cwd)
}

/// [`read_git_head`] against an explicit `home`, so a test needs no `$HOME`.
///
/// A `cwd` inside a file-provider domain is `Absent` before anything is opened:
/// the session's own program raises the same dialog for that folder, but this
/// thread must not be the one parked on it.
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
                // A worktree's `gitdir:` file can point into a provider domain
                // from outside one.
                if under_file_provider_domain(home, &git_dir) {
                    return GitHeadRead::Absent;
                }
                return match read_head(&git_dir.join("HEAD")) {
                    Err(()) => GitHeadRead::Denied,
                    Ok(Some(head)) => {
                        branch_of_head(&head).map_or(GitHeadRead::Absent, GitHeadRead::Found)
                    }
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

/// The `HEAD` file's text (bounded, regular files only, like [`read_small`]);
/// `Err(())` when the open was refused with `EPERM`.
fn read_head(path: &Path) -> Result<Option<String>, ()> {
    match open_nonblocking(path) {
        Ok(file) => Ok(regular(file).and_then(|file| read_capped(file, MAX_GIT_FILE_BYTES))),
        Err(error) if is_eperm(&error) => Err(()),
        Err(_) => Ok(None),
    }
}

/// The git directory `.git` names: itself when it is a directory, the
/// `gitdir:` target when it is a worktree's file.
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

/// A filesystem name fit for a chrome row: non-empty, at most 128 chars, no
/// control characters.
fn printable(name: &str) -> Option<String> {
    (!name.is_empty() && name.chars().count() <= 128 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

/// The floor under the transcript rows that speak for the process `entry`
/// registers: the later of its start (the kernel's, else the registry's
/// `procStart`) and its image's `startedAt`.
#[must_use]
pub fn floor_of(started: Option<u64>, entry: &SessionEntry) -> Option<u64> {
    match (started.or(entry.proc_start), entry.started_at) {
        (Some(process), Some(image)) => Some(process.max(image)),
        (process, image) => process.or(image),
    }
}

/// Everything the footer shows for process `pid` given its registry `entry`
/// ([`session_of_pid`]) and command-line facts (`launch`, [`launch_facts`]).
/// File I/O: keep off the event loop. `read_at` is the wall clock no later than
/// the registry read, recorded as when the process was last seen in the
/// session ([`TailCache`]). With no start known ([`floor_of`]) the transcript
/// is not used.
///
/// Order: what the transcript says since the process entered this session
/// ([`tail_facts`]); else what the process decided in the session it left
/// ([`TailCache`]); else its `--model`; else open ([`FooterFacts::model_open`],
/// [`FooterFacts::effort_open`]) for the launch card to fill
/// ([`FooterFacts::filled_from`]).
pub fn facts_for_entry_at(
    claude_dir: &Path,
    pid: u32,
    started: Option<u64>,
    entry: &SessionEntry,
    launch: &LaunchFacts,
    cache: &mut TailCache,
    read_at: SystemTime,
) -> FooterFacts {
    // With no floor no transcript row speaks for the process: not looked up.
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
}

/// [`facts_for_entry_at`] over the caller's transcript lookup
/// ([`transcript_path`]), shared with the usage fold so both read one file.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
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
) -> FooterFacts {
    let floor = floor_of(started, entry);
    // With no floor the transcript stays out rather than risk a
    // predecessor's model ([`facts_for_entry_at`]).
    let path = floor.and(transcript);
    // The rows of the current session that speak for the process: since it
    // entered that session.
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
    // Where the Bash tool stands, else the launch directory: the path and the
    // branch both describe it.
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
    let (branch, repo_read_denied) = match read_git_head(&dir) {
        GitHeadRead::Found(branch) => (Some(branch), None),
        GitHeadRead::Absent => (None, None),
        GitHeadRead::Denied => (None, Some(dir.clone())),
    };
    let (model, model_open) = match tail.model {
        Said::Is(model) => (Some(model), false),
        Said::Unread => (None, false),
        Said::Unsaid => (launch.model.clone(), launch.model.is_none()),
    };
    let (effort, effort_open, effort_for) = match (tail.effort, tail.effort_for) {
        (Said::Is(effort), None) => (Some(effort), false, None),
        (Said::Unread, _) => (None, false, None),
        (Said::Unsaid, None) => (None, true, None),
        // An effort-less switch with no older model named: the launch model's
        // `--model` decides, else the card may.
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
    FooterFacts {
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
    }
}

/// Claude Code's launch card, read for the footer only under a live REPL
/// ([`live_repl_row`]) and only when it is the newest card with no user
/// message under it (`aterm_phase::launch_card`). `None` when the model row
/// is not plainly a name.
#[must_use]
pub fn launch_card(rows: &[String]) -> Option<aterm_phase::LaunchCard> {
    live_repl_row(rows)?;
    let card = aterm_phase::launch_card(rows)?;
    Some(aterm_phase::LaunchCard {
        model: footer_model_name(&card.model)?,
        effort: card
            .effort
            .as_deref()
            .and_then(effort_level)
            .map(str::to_owned),
        version: card.version,
    })
}

impl FooterFacts {
    /// These facts filled from `card` (the kept launch card) where still open:
    /// the model while [`Self::model_open`], the effort while
    /// [`Self::effort_open`] and, after an effort-less switch, only from a card
    /// naming that model ([`Self::effort_for`]). Only a card of this build
    /// (`version`) counts.
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
        out
    }

    /// The model and the effort [`Self::filled_from`] shows, borrowed — these
    /// facts' own, or the card's where it fills them — for a caller that
    /// needs them without a copy of the facts (the footer's repaint key,
    /// every frame).
    #[must_use]
    pub fn filled_model_effort<'a>(
        &'a self,
        card: Option<&'a aterm_phase::LaunchCard>,
    ) -> (Option<&'a str>, Option<&'a str>) {
        let (mut model, mut effort) = (self.model.as_deref(), self.effort.as_deref());
        if let Some(card) = card.filter(|c| self.version.as_deref() == Some(c.version.as_str())) {
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

/// `cwd` as the footer shows it: `~/…` under `home` (unless home is `/`),
/// else whole. `None` for a path containing a control character.
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

/// `path` cut from the front to its last directory (`~/src/aterm` →
/// `…/aterm`), for a pane too narrow for all of it. `None` when that is no
/// shorter, or there is no directory to keep.
#[must_use]
pub fn elide_path(path: &str) -> Option<String> {
    let (_, last) = path.trim_end_matches('/').rsplit_once('/')?;
    let short = format!("\u{2026}/{last}");
    (!last.is_empty() && short.chars().count() < path.chars().count()).then_some(short)
}

/// Timing for one footer read ([`read_pid`]): `read_at` (no later than the
/// registry read), `now` (unix seconds, for the limit wall) and `offset_at`,
/// a zone's UTC offset at an instant (`None`: local), injected because it
/// reads system zone data ([`super::session_usage::wall_of`]).
pub struct ReadClock<'a> {
    /// No later than the registry read.
    pub read_at: SystemTime,
    /// Unix seconds, for the limit wall.
    pub now: i64,
    /// A zone's offset from UTC at an instant.
    pub offset_at: &'a dyn Fn(Option<&str>, i64) -> Option<i64>,
}

/// One footer read of live Claude Code process `pid`, as `aterm-gui`'s
/// `claude_footer::resolve_into` makes it: its facts ([`facts_for_entry_at`]),
/// the usage folded on in `fold` (at most [`FOLD_BUDGET`] bytes per call; the
/// limit wall stands while its reset is ahead of `clock.now` and nothing was
/// served since) and the resume line ([`super::resume::of_entry`], under the
/// conversation's model record in the harness's state root `state`). `launch`
/// is asked once with the registry entry for its launch facts and argv.
///
/// One registry read feeds all of it, so model, usage and resume line always
/// describe one conversation. A read landing mid-write yields no facts (the
/// host keeps the last good ones) and keeps the fold.
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
) -> Option<FooterFacts> {
    let Some(entry) = session_of_pid(claude_dir, pid, started) else {
        tail.clear();
        return None;
    };
    let (launch, argv) = launch(&entry);
    // One lookup of the transcript for the facts and the fold alike.
    let transcript = transcript_path(claude_dir, &entry);
    let mut facts = facts_for_entry_from(
        claude_dir,
        pid,
        started,
        &entry,
        transcript.as_deref(),
        &launch,
        tail,
        clock.read_at,
    );
    facts.usage = fold.usage(transcript, clock.now, clock.offset_at);
    facts.resume = super::resume::of_entry(&argv, &entry, started, state);
    Some(facts)
}

/// Where Claude Code's permission-mode row is: under the composer's bottom rule
/// (`aterm_phase::phase::composer_bottom`), before the first blank row, the row
/// opening with a mode pill ([`is_mode_row`]). Found by structure, never by the
/// rebindable `(shift+tab to cycle)` hint. The lights read the mode from it
/// (`harness::lights`) and the launch card is read only under one.
pub fn mode_row(rows: &[String]) -> Option<usize> {
    mode_row_under(rows, aterm_phase::phase::composer_bottom(rows)?)
}

/// [`mode_row`] under the composer bottom rule the caller has found (row
/// `bottom`), so a reader that has it does not look for it again.
#[must_use]
pub fn mode_row_under(rows: &[String], bottom: usize) -> Option<usize> {
    under_composer(rows, bottom).find(|&i| is_mode_row(&rows[i]))
}

/// The first row under the composer bottom rule `bottom` that opens with a
/// pill glyph ([`opens_with_pill_glyph`]), whether or not this build knows
/// the pill.
#[must_use]
pub fn pill_row_under(rows: &[String], bottom: usize) -> Option<usize> {
    under_composer(rows, bottom).find(|&i| opens_with_pill_glyph(&rows[i]))
}

/// The rows under the composer's bottom rule `bottom`, before the first blank.
fn under_composer(rows: &[String], bottom: usize) -> impl Iterator<Item = usize> + '_ {
    (bottom + 1..rows.len()).take_while(|&i| !rows[i].trim().is_empty())
}

/// Where a live REPL's footer row is, under the composer's bottom rule: its mode
/// row ([`mode_row`]), or where Claude draws no pill, the pill-less row
/// ([`is_bare_footer_row`]). A box left on the main screen by a predecessor has
/// `Press Ctrl-C again to exit` there instead.
#[must_use]
pub fn live_repl_row(rows: &[String]) -> Option<usize> {
    let bottom = aterm_phase::phase::composer_bottom(rows)?;
    mode_row_under(rows, bottom)
        .or_else(|| under_composer(rows, bottom).find(|&i| is_bare_footer_row(&rows[i])))
}

/// Whether `row` is Claude Code's pill-less footer row: after a two-space
/// indent, its left hint slot (`? for shortcuts`, `esc to clear`, `esc to
/// interrupt`). Completion lists and pickers under the rule do not match.
#[must_use]
pub fn is_bare_footer_row(row: &str) -> bool {
    row.strip_prefix("  ").is_some_and(|rest| {
        ["footer.shortcuts", "footer.clear", "busy.interrupt"]
            .into_iter()
            .any(|id| rest.starts_with(aterm_phase::anchor(id)))
    })
}

/// Whether `row` is Claude Code's permission-mode row: it opens with a known
/// pill ([`pill_indicator`]), and any `(<key> to cycle)` hint after it is
/// closed or cut between words ([`cycle_hint_len`]). A hint cut mid-word, or
/// the pill-less row, is not a mode row.
#[must_use]
pub fn is_mode_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("  ") else {
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

/// `rest` past the pill (`<glyph> <indicator> on`) it opens with, if it
/// opens with that one.
fn after_pill<'a>(rest: &'a str, pill: &Pill) -> Option<&'a str> {
    rest.strip_prefix(pill.glyph)?
        .strip_prefix(' ')?
        .strip_prefix(pill.text)
}

/// How many chars of `after` (the row past a pill) are the `(<key> to cycle)`
/// hint: all of it, or the head a narrow pane leaves, cut only between words
/// and ending at the row's end or the blanks before a separator. `0` when no
/// hint follows.
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

/// One of the vendor's mode pills: its glyph and the anchor
/// (`aterm_phase::anchor`) of the words after it.
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

/// The vendor's mode pills. `⏸` goes only with plan and manual mode, `⏵⏵`
/// with the rest — the pairing the 2.1.282 mode table draws.
pub(super) const PILLS: [Pill; 6] = [
    pill("\u{23F5}\u{23F5}", "mode.bypass"),
    pill("\u{23F5}\u{23F5}", "mode.auto"),
    pill("\u{23F5}\u{23F5}", "mode.accept_edits"),
    pill("\u{23F5}\u{23F5}", "mode.dont_ask"),
    pill("\u{23F8}", "mode.plan"),
    pill("\u{23F8}", "mode.manual"),
];

/// The anchor id (`mode.auto`) of the known pill `row` opens with, shared by
/// the footer, the lights (`harness::lights`) and the supervisor's
/// `footer_mode`.
#[must_use]
pub fn pill_indicator(row: &str) -> Option<&'static str> {
    let rest = row.strip_prefix("  ")?;
    PILLS
        .iter()
        .find(|pill| after_pill(rest, pill).is_some())
        .map(|pill| pill.id)
}

/// Whether `row` opens like a mode pill, whatever its indicator. With
/// [`pill_indicator`] answering `None`, the vendor renamed or added a mode, and
/// the host logs the drift.
#[must_use]
pub fn opens_with_pill_glyph(row: &str) -> bool {
    row.strip_prefix("  ")
        .is_some_and(|rest| PILLS.iter().any(|pill| rest.starts_with(pill.glyph)))
}

/// Whether `row` is a known pill cut short by a narrow pane
/// (`⏵⏵ bypass permissi`): geometry, not drift.
#[must_use]
pub fn is_cut_pill(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("  ") else {
        return false;
    };
    let rest = rest.trim_end();
    !rest.is_empty()
        && PILLS
            .iter()
            .any(|pill| format!("{} {}", pill.glyph, pill.text).starts_with(rest))
}

/// The footer's marked values, in order: a live upgrade waiting on the person
/// here ([`FooterFacts::upgrade`]), whole; a limit wall while it stands (a
/// narrow row drops values from the back, and the wall says why nothing
/// moves); model and effort under one mark; the account
/// ([`FooterFacts::account`]); the working directory; the branch; and the
/// session's tokens last, the first a narrow row gives up. A value the facts
/// lack is left out, not shown as a placeholder.
pub fn segments(facts: &FooterFacts) -> Vec<Segment> {
    let mut out: Vec<Segment> = facts
        .upgrade
        .as_ref()
        .map(|note| upgrade_segment(&note.whole))
        .into_iter()
        .collect();
    out.extend(fact_segments(facts));
    out
}

/// The upgrade note's segment, `text` its whole or its short words.
fn upgrade_segment(text: &str) -> Segment {
    Segment {
        mark: UPGRADE_MARK,
        text: text.to_owned(),
    }
}

/// [`segments`] but the upgrade note: what Claude Code's own files say.
fn fact_segments(facts: &FooterFacts) -> Vec<Segment> {
    let usage = facts.usage.as_ref();
    let model_effort = match (&facts.model, &facts.effort) {
        (Some(model), Some(effort)) => Some(format!("{model} {effort}")),
        (Some(one), None) | (None, Some(one)) => Some(one.clone()),
        (None, None) => None,
    };
    [
        (
            WALL_MARK,
            usage
                .and_then(|u| u.wall.as_ref())
                .map(super::session_usage::wall_text),
        ),
        (MODEL_MARK, model_effort),
        (ACCOUNT_MARK, facts.account.clone()),
        (PATH_MARK, facts.path.clone()),
        (BRANCH_MARK, facts.branch.clone()),
        (
            USAGE_MARK,
            usage.and_then(|u| {
                super::session_usage::usage_text_at(u, super::session_usage::UsageGrain::Total)
            }),
        ),
    ]
    .into_iter()
    .filter_map(|(mark, text)| Some(Segment { mark, text: text? }))
    .collect()
}

/// The model alone as a segment — the rung a narrow rule gives up last
/// ([`facts_ladder`]).
fn model_alone(facts: &FooterFacts) -> Option<Segment> {
    facts.model.as_ref().map(|model| Segment {
        mark: MODEL_MARK,
        text: model.clone(),
    })
}

/// The cells `text` takes on the glass: a wide char two, a combining mark
/// or a control none (the painter leaves those out).
#[must_use]
pub fn text_width(text: &str) -> usize {
    text.chars()
        .filter(|c| !c.is_control())
        .map(aterm_grapheme::char_width)
        .sum()
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
    /// Its cell in the set run (its lead cell: a wide char takes the next
    /// one too).
    pub cell: usize,
    /// The cells it takes: 1, or 2 for a wide char.
    pub cols: usize,
    /// How far its glyph is drawn from the middle of its cells, in
    /// [`NUDGES_PER_CELL`]ths of a cell (negative: left).
    pub nudge: i8,
    /// A fact's mark (`◆`, `⌂`, ...) rather than a character of its value.
    pub mark: bool,
}

/// The facts set as a line of small type ([`set_facts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetFacts {
    /// The cells the line takes, a whole number: the rule meets it on a cell
    /// boundary at either end.
    pub cells: usize,
    /// Every glyph, in reading order, each in a cell of its own.
    pub glyphs: Vec<SetGlyph>,
}

/// Set the facts as a line of type: each fact a mark, a space and its value,
/// its letters and words [`FACT_PITCH`] apart, the facts [`GAP`] pitches apart
/// whatever their lengths, and the line exactly as long as that, so the rule
/// runs on up to it.
///
/// One character still takes one cell, so each glyph stands in a cell of its
/// own and is moved ([`SetGlyph::nudge`]) from that cell's middle to where the
/// line puts it. A space takes no cell, so a tighter pitch shortens the line
/// rather than leaving slack in the gaps. The length is rounded up to a whole
/// cell by setting the line a hair looser; glyphs that need more cells than
/// that are set as loose as they need, every letter alike, the gaps still
/// [`GAP`] pitches. Glyphs take the cells nearest their places, in order, with
/// the largest move as small as it can be. Where a move would pass the nudge's
/// reach (a word of some eighty letters) the line is set a cell looser until
/// none does; at one cell a pitch nothing moves.
#[must_use]
pub fn set_facts(segments: &[Segment]) -> SetFacts {
    // Each glyph's place on the line, in pitches from its start.
    let mut glyphs: Vec<SetGlyph> = Vec::new();
    let mut place: Vec<f32> = Vec::new();
    let mut line = 0.0f32;
    for (i, segment) in segments.iter().enumerate() {
        if i > 0 {
            line += GAP;
        }
        let chars = std::iter::once((segment.mark, true))
            .chain(std::iter::once((' ', false)))
            .chain(segment.text.chars().map(|ch| (ch, false)));
        for (ch, mark) in chars {
            let cols = if ch.is_control() {
                0
            } else {
                aterm_grapheme::char_width(ch)
            };
            if cols == 0 {
                continue;
            }
            if ch.is_whitespace() {
                line += cols as f32;
                continue;
            }
            place.push(line);
            glyphs.push(SetGlyph {
                ch,
                cell: 0,
                cols,
                nudge: 0,
                mark,
            });
            line += cols as f32;
        }
    }
    let ink: usize = glyphs.iter().map(|g| g.cols).sum();
    if glyphs.is_empty() {
        return SetFacts::default();
    }
    let reach = f32::from(i8::MAX) / f32::from(NUDGES_PER_CELL);
    // A line of at most a few hundred pitches; the hair under `ceil` keeps a
    // whole product (`25 * 0.8`) from rounding up a cell.
    let mut cells = ((line * FACT_PITCH - 1e-3).ceil() as usize).max(ink);
    loop {
        let pitch = cells as f32 / line;
        // The cells before each glyph's, and the cell it would lead from if
        // it stood centred on its place: an isotonic fit of the difference
        // (L-infinity, the midpoint of the running max and the running min
        // from the end) keeps them in order with the largest move least.
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
            return SetFacts { cells, glyphs: set };
        }
        cells += 1;
    }
}

/// The run of the composer's bottom rule aterm may write into: the widest run
/// of `coverable` cells (plain rule glyphs in the rule's colours), rightmost of
/// equals. Other ink bounds the run. `None` when the row has no rule glyph.
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

/// The cells of `run` ([`rule_run`]) aterm's text may take: all but
/// [`RULE_LEAD`] glyphs at the left and one glyph at the right.
#[must_use]
pub fn rule_room(run: &std::ops::Range<usize>) -> usize {
    run.len().saturating_sub(RULE_LEAD + 1)
}

/// The widths of the lights' block (`claude_lights`): chips, whole and short
/// titles (each including its two-cell gap), and whether the title is a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightsWidth {
    /// The chips, one blank apart.
    pub chips: usize,
    /// `Light: what  `.
    pub title: usize,
    /// `what  `.
    pub short: usize,
    /// The title says why (a stopped mode return, a refusal, a switch note)
    /// rather than what the pointer or selection shows; it outranks the branch
    /// and the path ([`fit_rule`]).
    pub reason: bool,
}

/// Which of the lights' titles is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleFit {
    /// `Light: what`.
    Full,
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

/// The fewest cells a rule of `segments`, `title` and (with `chips`) the lights
/// takes: ` <chips>  <title> `, [`RULE_SEP`] glyphs, ` <facts> `. Lights stand
/// at the left end, facts at the right.
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
                TitleFit::Short => l.short,
                TitleFit::None => 0,
            };
            title + l.chips + 2
        }
        None => 0,
    };
    facts + lights + if facts > 0 && lights > 0 { RULE_SEP } else { 0 }
}

/// The facts as a narrow rule gives them up: the usage first (its total
/// narrowed to its runtime alone, `Σ 7h52m`, then gone), then the path cut to
/// `…/<dir>`, the branch, the path, the effort and the model, a limit wall
/// standing alone last. The account ([`FooterFacts::account`]) stands whole
/// beside whatever fits without it, or not at all (a clipped label would name
/// another account): it takes only the tokens' room. A rule too narrow for the
/// wall gives up the wall, not everything behind it: the same rungs follow
/// without it, so a narrow pane keeps the model it showed before the limit
/// (Claude's own screen carries the notice), and the model alone is the last
/// rung ([`model_alone`]). Never empty: the whole facts are the first rung.
///
/// A live upgrade waiting on the person here ([`FooterFacts::upgrade`]) gives
/// way as a light's reason does ([`fit_rule`]): whole beside the whole facts,
/// then short, taking the tokens', account's, branch's and path's room (never
/// the effort's, the model's or a wall's), then gone.
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
    for rung in plain.iter().filter(|rung| top_of(rung).eq(top_of(whole))) {
        let rung = led(&note.short, rung);
        if !out.contains(&rung) {
            out.push(rung);
        }
    }
    out.extend(plain);
    out
}

/// The model-and-effort segment and a limit wall of `rung`: what a reason —
/// a light's, or the upgrade's note — never takes the room of ([`fit_rule`]).
fn top_of(rung: &[Segment]) -> impl Iterator<Item = &Segment> {
    rung.iter()
        .filter(|s| s.mark == MODEL_MARK || s.mark == WALL_MARK)
}

/// [`facts_ladder`] of the facts alone, with no upgrade note.
fn plain_ladder(facts: &FooterFacts) -> Vec<Vec<Segment>> {
    let whole = fact_segments(facts);
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
        .and_then(elide_path)
        .map(|text| Segment {
            mark: PATH_MARK,
            text,
        });
    let model = model_alone(facts);
    // The `Σ` segment narrows to the runtime alone before it goes.
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
                .chain(std::iter::once(Segment {
                    mark: USAGE_MARK,
                    text,
                }))
                .collect()
        });
    // Where the account does not fit, the rule with no account: the whole
    // facts and their runtime form without it. None with no account.
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

/// What of the facts and lights fits `room` cells of the rule ([`rule_room`]).
/// Order of giving way:
///
/// * a transient title (what the pointer, the keyboard selection or a toggle
///   in flight shows) goes first, whole then short then gone, so the facts
///   stay whole at 80 columns while a light is hovered;
/// * a light's reason ([`LightsWidth::reason`]: where a mode return stopped, a
///   refusal) goes whole, then, short, takes the tokens', account's, branch's
///   and path's room (never the effort's, the model's or a limit wall's); a
///   reason too long even beside the model and effort goes, and the facts come
///   back;
/// * a live upgrade's note ([`FooterFacts::upgrade`]) gives way the same way
///   ([`facts_ladder`]), and yields to a light's reason where the two do not
///   fit together;
/// * then the facts give way ([`facts_ladder`]), then the chips, and the model
///   goes last.
///
/// Chips stand at the left end after [`RULE_LEAD`], the title right of them,
/// facts right-aligned, so chips never move. Tries are measured in place and
/// only the fitting one is copied out.
#[must_use]
pub fn fit_rule(facts: &FooterFacts, room: usize, lights: Option<LightsWidth>) -> RuleFit {
    let ladder = facts_ladder(facts);
    let model = model_alone(facts);
    let chips = lights.is_some();
    let reason = lights.is_some_and(|l| l.reason);
    let whole = ladder[0].as_slice();
    let titled = [TitleFit::Full, TitleFit::Short]
        .into_iter()
        .filter(|_| chips)
        .map(|title| (whole, title, chips));
    let reasons = ladder[1..]
        .iter()
        .filter(|rung| reason && top_of(rung).eq(top_of(whole)))
        .map(|rung| (rung.as_slice(), TitleFit::Short, chips));
    let bare = ladder
        .iter()
        .map(|rung| (rung.as_slice(), TitleFit::None, chips));
    // Then the model without the chips, and chips narrower than the model
    // still drawn where the model cannot be.
    let last = [(model.as_slice(), false), (&[][..], true)]
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

/// The most cells the chips may take in a rule of `room` cells: what remains
/// beside the model alone, when it fits. The lights' block is built to it and
/// the keyboard selects only chips that fit.
#[must_use]
pub fn chips_room(facts: &FooterFacts, room: usize) -> usize {
    let model_w = rule_width(model_alone(facts).as_slice(), TitleFit::None, false, None);
    let beside = if model_w > 0 && model_w <= room {
        model_w + RULE_SEP
    } else {
        0
    };
    room.saturating_sub(beside + 2)
}

/// The environment actions of `ClaudeFooterModel`
/// (`aterm_spec::derive::claude_footer_model_model`): everything but `Read` is
/// Claude Code's or a person's, and is waived here; `Read` is bound where the
/// reader runs. The Tier-1 bind (`tests/conformance_footer_model.rs`) performs
/// each on disk. Proof and test builds only.
#[cfg(any(test, feature = "spec-anchors"))]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Flag",
        reason = "The process's own `--model` at its launch: Claude Code's argv, which the reader reads (`launch_facts`) and never writes. The Tier-1 bind launches with it exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "LaunchResume",
        reason = "A launch with `--resume <conversation>`, which Claude restores its model from: Claude Code's step. The Tier-1 bind launches so and writes the conversation on disk exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "DrawCard",
        reason = "Claude Code drawing its launch card on the pane: the vendor's screen, which the host reads (`launch_card`). The Tier-1 bind draws a measured 2.1.283 card exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Answer",
        reason = "An answer Claude Code writes into the transcript: the vendor's row. The Tier-1 bind appends one in 2.1.283's shape exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "ChooseA",
        reason = "A person's `/model` choice and the result row Claude Code writes for it: the vendor's step. The Tier-1 bind appends the command and a readable result exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "ChooseB",
        reason = "A person's `/model` choice of the other model, and its result row: the vendor's step. The Tier-1 bind appends the command and a readable result exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "ChooseUnread",
        reason = "A `/model` choice whose result this build cannot read: the vendor's step. The Tier-1 bind appends the command and an unquoted result exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Flood",
        reason = "A row larger than the reader's tail window (a pasted image): the vendor's step. The Tier-1 bind appends a 600 KiB image prompt exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Clear",
        reason = "`/clear`: Claude Code rewrites `sessions/<pid>.json` with a new session id under the same process. The Tier-1 bind rewrites the registry exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "ClaudeFooterModel",
        action = "Resume",
        reason = "An in-REPL `/resume` into the next conversation, restored by Claude unless pinned: the vendor's step. The Tier-1 bind rewrites the registry exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
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
