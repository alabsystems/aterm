// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE FOOTER — aterm's facts about a Claude Code session, on
//! the glass beside Claude Code's own (owner directions, 2026-09-24 and
//! 2026-09-28): one run of three marks, Codex-style —
//!
//! ```text
//! ────────────────────────────────────────────── ‹top effort› ─
//! ❯ fix the footer
//! ───────────────── ◆ Opus 5.5 xhigh   ⌂ ~/aterm   ⎇ main ─
//!   ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← for agents
//! ```
//!
//! model and effort under one mark, then the session's working directory
//! (home as `~`; owner, 2026-09-27: the repository's name alone "isn't
//! enough"), then the branch — written INTO the rule under Claude Code's
//! input box, right-aligned the way Claude writes its own effort tag (its
//! top-effort mode's name, `‹top effort›` above) into the rule above it
//! (owner, 2026-09-28: "the original footer from claude code should also be
//! there because I'm worried about excluding information from claude code
//! originally"). Claude's own footer
//! row under the rule is never touched, and only cells that are plain rule
//! glyphs are covered ([`rule_run`]); the rule is the pane's whole width, so
//! the facts fit at 80 columns too, and a pane too narrow for them gives
//! way in a fixed order: a light's hover or selection title first, a
//! light's REASON (a stop, a refusal) after the session's tokens, the
//! branch and the path, a limit wall after every fact but the model, the
//! model last ([`fit_rule`]).
//! The vendor rows cannot be switched
//! off from outside anyway: measured on 2.1.282, no setting or environment
//! variable removes them, and a statusLine only ADDS a row — even one that
//! prints nothing reserves a blank row. So the facts do not come from a
//! statusLine at all. They come from what Claude Code already keeps:
//!
//! * `<claude dir>/sessions/<pid>.json` maps the Claude Code PROCESS to its
//!   session id and working directory — the path the footer shows, and how a pane finds ITS
//!   transcript when several sessions share one directory ([`session_of_pid`]);
//! * the session's transcript carries the model and the effort: every
//!   answer's (`message.model`, `effort`), and — the moment one is chosen —
//!   the RESULT Claude writes for its own `/model`, `/effort` and `/fast`
//!   commands (`Set model to `Opus 5.5 (default)``, `Set effort level to
//!   …`; [`tail_facts`]). The transcript is the CONVERSATION's, shared by
//!   every process that resumed it, so only a row written since THIS process
//!   started — and since it entered that session — speaks for it; and the
//!   model is the PROCESS's, so what it decided in a session it left stands
//!   across a `/clear` until the new session says otherwise, and across an
//!   in-REPL `/resume` only where the process PINNED its model; an unpinned
//!   one runs the resumed conversation's own model, which Claude restores —
//!   and a restore pins it, at a `/resume` or at a launch `--resume`, so
//!   every later `/resume` keeps it. A `/resume` is told from a `/clear` by
//!   when the process was last seen in the session it left ([`TailCache`]);
//! * the process's own `--model` ([`launch_facts`]), when the transcript
//!   since its start names no model yet;
//! * Claude Code's launch card on the screen (`Opus 5.5 with xhigh effort ·
//!   Claude Max`, [`launch_card`]), when nothing above named them — the
//!   host reads it again while they stay open and keeps the newest reading,
//!   for that process and session only ([`FactsOwner`]);
//! * the branch is read live from `.git/HEAD` ([`git_head`]), so a checkout
//!   shows at once rather than at the next turn;
//! * the session's USAGE ([`super::session_usage`], through
//!   [`FooterCache::usage`]): its tokens per model, folded incrementally
//!   from its transcript and its subagents' — the CONVERSATION's, not floored
//!   at this process's start as model and effort are — last in the run
//!   (`Σ opus 48M in 310k out`), and, while one stands, the limit wall
//!   Claude Code wrote into the transcript, FIRST in it (`⧗ 5h limit ·
//!   resets 3pm`) so a narrow rule gives it up after every fact but the
//!   model ([`facts_ladder`]).
//!
//! Nothing here is written anywhere, and nothing reaches into the user's
//! Claude settings — the retired "decision B" install is not revived.
//!
//! VERSION DRIFT. Claude Code is replaced under a running aterm, and a
//! session can be relaunched onto a newer build mid-tab (`harness::upgrade`).
//! Everything here reads the vendor's output as a THIRD PARTY's: a rule this
//! reader does not find is left exactly as the vendor drew it, a file or a
//! result whose shape moved yields no fact rather than a guessed one, and facts
//! are keyed to one PROCESS (its pid AND its start time, [`parse_session_entry`]),
//! so a relaunched build is a new process whose facts are read afresh — never
//! the model of the process before it ([`tail_facts`]) on that process's
//! say-so. The one way the conversation's older model reaches the footer is
//! Claude's own: a relaunch that resumes the conversation with no `--model`
//! runs the model Claude restores from it, and the footer reads it the way
//! Claude does ([`TailCache`]).
//!
//! NARROW PANES (read from 2.1.284's bundled renderer, 2026-09-28). The mode
//! row is a `height: 1`, `overflow: hidden` box inside the footer's `paddingX:
//! 2`, so it has `columns - 4` cells. The pill and its hint are ONE text
//! (`<glyph> <indicator> on` plus the dim ` (<key> to cycle)`), in a box that
//! does not shrink, wrapped at those cells by word (wrap-ansi, `hard`, no
//! trim), and the row shows the first wrapped line only. So a narrow pane
//! CUTS the hint between words — `(shift+tab`, `(shift+tab to` — or drops it
//! whole; it never shortens it, and never marks the cut with `…`. Bypass
//! shows all of it from 49 columns, `(shift+tab to` from 42, `(shift+tab`
//! from 39 and the pill alone from 28 (auto mode: 40, 33, 30, 19); narrower,
//! the pill itself is cut ([`is_cut_pill`]). The items after it
//! (` · ← for agents`) are a truncating text that yields its cells first,
//! with `…`; they follow the pill's box, which is as wide as its WIDEST
//! wrapped line, so a separator can stand past a gap of blanks
//! (`bypass permissions on  · ← fo…`). The width-aware footer in the same
//! bundle, which drops the hint instead, is switched off in that build (its
//! gate returns `false`). aterm writes nothing on that row — every item
//! Claude draws there, cut or whole, stays in Claude's own cells — but which
//! row IS the mode row decides what the lights read the mode from and
//! whether a live REPL is up ([`is_mode_row`], [`live_repl_row`]): a hint cut
//! between words is read as the hint ([`cycle_hint_len`]).

use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aterm_json::Value;
use aterm_primer::CLAUDE_TOP_EFFORT_KEY;

/// How much of a transcript's END one read window takes. One assistant turn
/// with a large tool result can be hundreds of KiB, so the window is
/// generous; the resolver's reader looks further back, a window at a time,
/// while the newest rows decide nothing ([`TailCache`]), and [`read_tail_facts`]
/// reads this window alone.
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
/// The mark before a limit wall the session hit.
pub const WALL_MARK: char = '\u{29D7}'; // ⧗
/// The mark before the session's usage.
pub const USAGE_MARK: char = '\u{03A3}'; // Σ

/// How many transcript bytes one [`FooterCache::usage`] folds at most: a
/// first read of a long session catches up over a few reads rather than
/// holding the resolver thread every other session's footer waits on. Until
/// it has caught up, the footer names no tokens — a prefix of a session is
/// not its usage.
pub const FOLD_BUDGET: u64 = 64 * 1024 * 1024;

/// Spaces between the footer's marked values, as the owner's layout draws them.
pub const GAP: usize = 3;

/// Rule glyphs left at the LEFT end of the run aterm writes into ([`rule_run`]),
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
    /// When the running IMAGE registered (unix seconds), from the file's
    /// `startedAt` (milliseconds, `Date.now()` at registration). Claude Code
    /// can relaunch itself in place — `execve` with `--resume` (its provider
    /// setup's restart, 2.1.283) — which keeps the pid AND the kernel's start
    /// time, while every image writes its own `startedAt`: the floor moves
    /// with the image ([`facts_for_pid`]).
    pub started_at: Option<u64>,
}

/// Whose facts these are: the transcript floor they were read above and the
/// session they were read for. A launch card the host keeps
/// ([`launch_card`]) belongs to exactly one owner, so an in-place exec (a new
/// floor) or a new session id drops it.
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
    /// The session's working directory, home shown as `~` (`~/aterm`).
    pub path: Option<String>,
    /// The checked-out branch, or a short commit id when HEAD is detached.
    pub branch: Option<String>,
    /// The session's working directory when the kernel REFUSED the repository
    /// read with `EPERM` — on macOS, a folder privacy consent (TCC) aterm does not
    /// hold. Never shown; the host raises its consent attention for the session
    /// ([`GitHeadRead::Denied`]).
    pub repo_read_denied: Option<PathBuf>,
    /// The Claude Code build this process runs (`2.1.283`) — not shown; it
    /// names the vendor build in the host's drift log when a row it draws
    /// is not one this build reads, and a launch card is taken only from
    /// this build ([`FooterFacts::filled_from`]).
    pub version: Option<String>,
    /// The line that resumes THIS process's own conversation after a restart
    /// (`claude --resume <id>` with its launch flags,
    /// [`crate::harness::resume::of_entry`] over the SAME registry entry the
    /// model and the usage were read from, [`read_pid`]) — not shown in
    /// the footer; the window's frozen-program remedy names it (2026-09-26:
    /// never `claude --continue`, which resumes a sibling tab's conversation
    /// where two share a directory). [`facts_for_pid`] leaves it unset: it
    /// needs the process's argv, which only the caller reads.
    pub resume: Option<String>,
    /// The session's usage — tokens per model (live) and the limit wall
    /// written into its transcript, while it stands
    /// ([`super::session_usage`]); only [`FooterCache::usage`] fills it
    /// ([`read_pid`], the host's resolver).
    pub usage: Option<super::session_usage::UsageFacts>,
    /// Nothing since the process started decided the model — no answer, no
    /// model result, no `--model`: its launch card may name it.
    pub model_open: bool,
    /// Nothing since the process started decided the effort.
    pub effort_open: bool,
    /// While [`Self::effort_open`]: the model a launch card must name for its
    /// effort to fill the effort — set when the process switched models with
    /// a result that named no effort, and nothing since the floor says what
    /// ran before it: the card's effort was read for the card's model, and a
    /// model runs its own effort ([`TailFacts`]).
    pub effort_for: Option<String>,
    /// Whose facts these are, when the registry named a session.
    pub owner: Option<FactsOwner>,
}

/// What the process's own command line says ([`launch_facts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct LaunchFacts {
    /// The model its `--model` names, as the footer shows it — only a real
    /// model id (`claude-opus-5-5`): an alias (`opus`) is resolved per
    /// account, provider and build, which this cannot know.
    pub model: Option<String>,
    /// The process's model is PINNED from its launch: a `--model` of any
    /// spelling (an alias too), or an environment model pin at exec
    /// (`ANTHROPIC_MODEL`, `ANTHROPIC_DEFAULT_<FAMILY>_MODEL`). Claude then
    /// restores no conversation's model onto it — neither the one a launch
    /// `--resume` names nor one an in-REPL `/resume` switches to (2.1.284:
    /// every restore returns early on the main-loop override, which `--model`
    /// sets, and on those variables). A launch resume that restores a model
    /// pins it too, but only when the conversation holds an answer to restore
    /// — which the transcript says, not the argv ([`TailCache`]).
    pub pinned: bool,
    /// The conversation the launch RESUMED (`--resume`, `--continue`), when
    /// it resumed one ([`LaunchResume`]).
    pub resume: Option<LaunchResume>,
}

/// The conversation a process's LAUNCH resumed
/// ([`crate::harness::upgrade::launch_resume`]). Claude restores its model
/// at launch (2.1.284: every launch resume — `--resume <id>`, `--continue`,
/// the picker — runs the same restore as an in-REPL `/resume`, and puts the
/// restored model in the REPL's first state) unless [`LaunchFacts::pinned`].
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

/// What `argv` (argv[0] included) says about the process's model: the last
/// `--model` Claude's parser takes ([`crate::harness::upgrade::launch_model`]),
/// when it is a real id. Every model move the harness makes relaunches with
/// one (`relaunch::with_model`), so this names such a model before its first
/// answer.
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

/// The Claude Code directory of ONE process, from that process's own
/// environment at exec: `CLAUDE_CONFIG_DIR` when set, else `$HOME/.claude` —
/// Claude Code's rule. Never aterm's environment: aterm strips `CLAUDE_*`
/// from the shells it spawns, so its own value says nothing about a session's.
/// A relative `CLAUDE_CONFIG_DIR` resolves against a directory this cannot
/// know, so it answers `None` rather than a guess.
pub fn claude_dir_of(config_dir: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = config_dir.filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        return dir.is_absolute().then_some(dir);
    }
    let home = PathBuf::from(home.filter(|h| !h.is_empty())?);
    home.is_absolute().then(|| home.join(".claude"))
}

/// Open `path` for reading only if it is a REGULAR file, judged on the open
/// handle (no check-then-open race). The open itself is non-blocking, so a
/// FIFO planted where a `HEAD` or a transcript should be cannot park the
/// reader thread forever.
pub(crate) fn open_regular(path: &Path) -> Option<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// At most `cap` bytes of the regular file at `path`, as text.
pub(crate) fn read_small(path: &Path, cap: u64) -> Option<String> {
    let mut text = String::new();
    open_regular(path)?
        .take(cap)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
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

/// [`session_of_pid`]'s parse, pure. The file must be THIS process's, twice
/// over: it names the same `pid` it is filed under, and — when the caller
/// knows the process's start time and the file carries Claude Code's
/// `procStart` — the two name the same second. A registry row left behind by
/// a dead process whose pid was reused fails the second test even when it
/// passes the first.
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

/// What the last [`TAIL_BYTES`] of the transcript at `path` say about the
/// model and effort, counting only rows written no earlier than `since`
/// ([`tail_facts`]).
pub fn read_tail_facts(path: &Path, since: Option<u64>) -> Option<TailFacts> {
    Some(tail_facts(&read_tail(path)?, since))
}

/// The most bytes one refresh reads as an APPEND to a transcript it has read
/// before ([`TailCache`]'s carry); a larger append is read as a fresh tail.
const MAX_APPEND_BYTES: u64 = 16 * 1024 * 1024;

/// How many bytes before the carried end are checked unchanged before an
/// append is read on top of what was read before.
const GUARD_BYTES: u64 = 4096;

/// How far back from its end a transcript is read afresh for a fact the
/// newest rows do not decide ([`TailCache`]).
const MAX_BACK_BYTES: u64 = 16 * 1024 * 1024;

/// The resolver's one-session transcript memo. The session registry and the
/// repository are still read on every refresh; only an unchanged,
/// successfully read transcript tail is reused. The process and image start
/// are part of the key, so a resumed or exec'd Claude cannot inherit an old
/// model even when it keeps the same transcript path and file revision.
///
/// A read AFRESH looks back from the end a [`TAIL_BYTES`] window at a time
/// while the newest rows decide nothing, up to [`MAX_BACK_BYTES`]
/// (`TailCache::read_fresh`), so the first read of a session finds an answer
/// above a huge row however the reads fell between them.
///
/// THE CARRY. A transcript that GREW since the last read is read as an append:
/// only the new whole rows are scanned, and what they do not decide is
/// carried from the rows read before — so one huge row (a pasted image made a
/// 530,256-byte prompt row on the owner's session of 2026-09-28, more than
/// the whole [`TAIL_BYTES`] window) cannot blank a model a row above it named.
/// It is the SAME file of the SAME process under the SAME floor, grown, with
/// the [`GUARD_BYTES`] before the old end unchanged; anything else — a new
/// floor, another process, a replaced or shrunk file, an append past
/// [`MAX_APPEND_BYTES`] — is read afresh. Every appended byte is scanned, so
/// a choice made in the append is never hidden by the carry.
///
/// THE PROCESS'S OWN FACTS. The model and the effort are the PROCESS's, not
/// the conversation's: `/clear` (and an in-REPL `/resume`) gives the same
/// process a new session id — Claude rewrites `sessions/<pid>.json` with it
/// and leaves `startedAt` alone (2.1.283: `regenerateSessionId`, then the
/// session-switch hook's registry write). After a `/clear` the new
/// transcript starts empty while the process runs on the model it ran. So
/// the cache also keeps what the transcript tier last DECIDED for this
/// process (pid, kernel start and floor), the session it read that in, and
/// whether the process has PINNED its model — and a session that has not
/// said anything yet inherits it ([`TailCache::for_process`]): never the
/// launch flag or the card over a choice the process made. At the switch the
/// session LEFT is read once more, so a choice made there after the last
/// refresh is not lost, and searched back to the floor for a choice, which
/// pins the model ([`chose_model_since`]).
///
/// A `/resume` is not a `/clear`. The session it switches to is a
/// conversation that EXISTED before the switch, and Claude restores that
/// conversation's last model onto the process unless the process pinned its
/// own (2.1.284's REPL `resume`: the restore returns early on the main-loop
/// override — a `--model`, a `/model` or `/fast` move — and on an
/// environment model pin). So the process's facts carry into a resumed
/// conversation only when it pinned its model; otherwise the model is the
/// resumed conversation's own, read the way Claude reads it
/// ([`conversation_model`]), and the effort stands only if that is the model
/// it was read for.
///
/// WHEN THE SWITCH CAME. The cache records the wall-clock time of every read
/// ([`facts_for_entry_at`]), and so when the process was last SEEN in the
/// session it later left: the switch came after that moment. A `/clear`'s
/// session is created by the switch, so its file and every row in it come
/// after it; a conversation whose file was born before it, or whose first
/// stamped main-chain row is older than it, existed before the switch — a
/// `/resume` ([`existed_before`]), whoever began it: an earlier process, this
/// one before a `/clear`, or another tab since this process started. A
/// `/branch` is neither: its copies of the conversation keep their
/// originals' stamps but carry `forkedFrom`, and Claude restores nothing
/// into a branch (2.1.284: the REPL's resume skips the restore for a fork),
/// so a copy is no evidence. The session a switch entered speaks for the
/// process only from that moment on (its [`TailCache::session_floor`]):
/// what an earlier process — another tab — wrote into a resumed conversation
/// after this process started is that conversation's, never this process's
/// statement. At its FIRST SIGHT of a process the cache has no such moment,
/// and the process's floor stands in for it.
///
/// A RESTORE IS A PIN. Claude restores by setting that same main-loop
/// override (2.1.284: `Cbe` calls `overrideMainLoopModel` on every restoring
/// path), and nothing clears it at a session switch — `/clear` and the
/// resume switch mutate the one process-wide state and forget only a
/// refusal fallback. So the process is pinned from the first restore on,
/// and every later `/resume` keeps its model. What pins it, as this cache
/// keeps it: [`LaunchFacts::pinned`]; a choice found in a session it left,
/// since it entered it ([`chose_model_since`]); a switch into a conversation
/// that existed before the switch and held an answer from before it; and —
/// at the cache's FIRST SIGHT of the process — a first session that is a
/// conversation from before the process's floor holding an answer: a launch
/// `--resume` or `--continue` restored it and pinned the model (every launch
/// resume runs the same restore), or an in-REPL `/resume` this cache did not
/// see did, or found the process pinned already. At first sight the footer
/// names that restored model only when the launch resumed THIS session and
/// pinned nothing ([`LaunchFacts::resume`]: it names this session, or names
/// none — `--continue`, the picker, a fork — and is taken for this one): a
/// `/resume` the cache did not see may have kept another model, and then the
/// footer names none until the process says one.
///
/// WHAT IT CANNOT SEE. The cache judges one switch between two reads: a
/// `/resume` then another before the next read is read as the last one
/// alone. What another process wrote into a conversation between the
/// cache's last read in the session left and the switch is taken for this
/// process's own rows, and a conversation it BEGAN then for a `/clear`'s,
/// whose restore's pin is missed. And a first sight knows nothing of the
/// process's earlier sessions — the host keeps a process's cache while that
/// process lives (`aterm-gui`'s resolver), but a new aterm process (an
/// update handoff) starts from first sight, and a pin made in a session the
/// process has since left by a `/clear` — a restore, or a `/model` or
/// `/fast` choice — is not known: the next `/resume` names the resumed
/// conversation's model where Claude keeps the pinned one.
///
/// The process's facts survive [`TailCache::clear`] (a registry read that
/// lands mid-write clears the tail, not the process); another process or a
/// new floor starts them afresh.
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

/// One read of a transcript since the floor: what it decided, and whether
/// it met a row from BEFORE the floor — the file holds a conversation older
/// than the process (it was resumed), not only the process's own rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TailRead {
    facts: TailFacts,
    resumed: bool,
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
    /// When the process was last SEEN in `session` (unix seconds): the
    /// wall-clock time of the newest read that found it there. A switch out
    /// of `session` came after it.
    seen: u64,
    facts: TailFacts,
    /// The process's model is pinned by what this cache has read of it: a
    /// choice in a session it LEFT ([`chose_model_since`]), or a restore — a
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
        // Without a change-time and file identity, a same-length rewrite or
        // replacement could keep the key unchanged. Cache on Unix only.
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

    /// The transcript read afresh, NEWEST FIRST, a [`TAIL_BYTES`] window at a
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
        let read = TailRead { facts, resumed };
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

    /// The rows APPENDED since the carried read, on top of what it carried —
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
        // A row stamped before the floor in the append puts every row above
        // it — the carried ones too — before this process.
        let facts = if floored {
            TailRead {
                facts: newer,
                resumed: true,
            }
        } else {
            TailRead {
                facts: newer.over(carry.facts.facts.clone()),
                resumed: carry.facts.resumed,
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

    /// The floor under the rows of `session` that speak for the process
    /// (`pid`, `started`, `floor`): the rows it wrote since it ENTERED that
    /// session. At the cache's first sight of the process, its own floor. In
    /// a session it SWITCHED into, the moment it was last seen in the session
    /// it left ([`ProcessFacts::seen`], never below the floor): every row
    /// there from before that moment is the conversation's — another
    /// process's, or this one's from an earlier visit — and none of it was
    /// written since the switch. Kept for the session while the process
    /// stays in it.
    fn session_floor(&self, pid: u32, started: Option<u64>, floor: u64, session: &str) -> u64 {
        match self.process_of(pid, started, floor) {
            Some(p) if p.session == session => p.session_floor,
            Some(p) => p.seen.max(floor),
            None => floor,
        }
    }

    /// `tail` — what the CURRENT session (`session`)'s transcript says since
    /// its [`Self::session_floor`] — over what this process (`pid`,
    /// `started`, `floor`) last decided in any of its sessions: a fact the
    /// current session has not said yet is the process's last decision. When
    /// the session is not the one the process was last read in, the process
    /// SWITCHED sessions after it was last seen there, and `switch` says how:
    ///
    /// * the session LEFT is read again first ([`Switch::left`], given its
    ///   id and its session floor), since it may have said more after the
    ///   last read — a choice, then `/clear`, inside one refresh — and
    ///   whether it holds a model choice of the process's own, which pins the
    ///   model;
    /// * the process's model is not pinned — not by its launch
    ///   ([`Switch::launch_pinned`]), a choice in any session it left, nor an
    ///   earlier restore — and the current session is a conversation that
    ///   EXISTED before the switch and held an answer then
    ///   ([`Switch::restored`], given that moment): Claude restored that
    ///   answer's model, which stands in for the process's, with the
    ///   process's effort only if it was read for that same model — and the
    ///   restore PINS the model from here on.
    ///
    /// At FIRST SIGHT of the process (nothing kept for it), a first session
    /// that is a conversation from before the process's floor holding an
    /// answer ([`Switch::restored`], given no moment) pins it too — Claude
    /// restored it at launch, or at a `/resume` this cache did not see, or
    /// found the model pinned already — and the restored model is shown when
    /// the launch resumed this very session and pinned nothing
    /// ([`Switch::launch_resumed`]).
    ///
    /// The result is remembered as the process's decision, and the time of
    /// this read ([`Switch::now`]) as when the process was last seen in
    /// `session`. Another process, or a new floor (an image exec'd in place),
    /// inherits nothing.
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

/// The facts a RESTORE leaves on a process: the model Claude restored, and
/// the effort open for that model alone — the process's own effort stands
/// only if it was read for it ([`TailFacts::over`]) — or unknown with an
/// unreadable model.
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

/// How [`TailCache::for_process`] reads a session SWITCH of one process, or
/// its first sight of it.
struct Switch<L, R> {
    /// When this read happened (unix seconds, the wall clock): from now on,
    /// when the process was last seen in the current session.
    now: u64,
    /// The session LEFT (its id), read again since its session floor (the
    /// second argument, [`TailCache::session_floor`]): what it decided, and
    /// whether it holds a model choice of the process's own
    /// ([`chose_model_since`]).
    left: L,
    /// The process's launch pins its model ([`LaunchFacts::pinned`]).
    launch_pinned: bool,
    /// The process's launch resumed the CURRENT session
    /// ([`LaunchFacts::resume`]: it names this session, or names none).
    launch_resumed: bool,
    /// When the CURRENT session is a conversation that existed before the
    /// process took it up, the model Claude restores from it, read as Claude
    /// reads it — `None` when the session is none, or holds no answer to
    /// restore. At a switch it is given the moment the switch came after
    /// ([`resumed_since`]); at a first sight, `None`, and the process's floor
    /// stands in for that moment ([`resumed_model`]). Asked only at a switch
    /// or a first sight: it reads the transcript's head and, when resumed,
    /// back to the conversation's newest answer from before that moment.
    restored: R,
}

/// One row [`rows_back`] meets.
#[derive(Clone, Copy)]
enum Row<'a> {
    /// A whole row of at most [`MAX_LINE_BYTES`].
    Whole(&'a [u8]),
    /// A row longer than [`MAX_LINE_BYTES`] — too long to parse — met ONCE,
    /// by the part of it the read holds: always its END, which Claude writes
    /// last in a row (a row's top-level `type` and `timestamp` follow its
    /// `message`), and its start too when one window held it whole.
    Oversized(&'a [u8]),
}

/// Every row of the transcript at `path`, NEWEST FIRST, a [`TAIL_BYTES`]
/// window at a time — a row a window's start cut is completed by the next,
/// older, window — until `visit` breaks, the file's start, or
/// [`MAX_BACK_BYTES`]. A row longer than [`MAX_LINE_BYTES`] is met as
/// [`Row::Oversized`], once, in its place in the order. `Some(true)` when
/// every row to the file's start was visited, `None` when the file cannot be
/// read.
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

/// Whether the transcript at `path` holds, since `floor` — the process's
/// session floor there ([`TailCache::session_floor`]) — a model CHOICE the
/// process made — a `/model` command row (every one is a choice, its result
/// readable or not) or a `/fast` result that moved the model. Either sets
/// Claude's main-loop override, which keeps the process's model across an
/// in-REPL `/resume` ([`TailCache`]). Searched back to the first row before
/// the floor, or [`MAX_BACK_BYTES`].
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

/// How much of a transcript's HEAD [`head_stamp`] reads: its first rows
/// are a few short ones (`mode`, `permission-mode`, the first system row).
const HEAD_BYTES: u64 = 64 * 1024;

/// The stamp of the first row of the transcript at `file` that carries one,
/// among the whole rows of its first [`HEAD_BYTES`] — with `own_rows`, the
/// first MAIN-CHAIN MESSAGE row (`user`, `assistant`, `system`) a `/branch`
/// did not copy: a copy carries `forkedFrom` and keeps its original's stamp
/// (2.1.284's branch writes `{...row, forkedFrom: {sessionId,
/// messageUuid}}`), so it says when the conversation it was copied from was
/// written, not this file; and after the copies the same branch writes the
/// source's account-memory rows as `{...row, sessionId: <new>}` — a
/// `memory-mode` row keeping its original stamp and carrying no
/// `forkedFrom` — so only a message row is the file's own. The file is in
/// append order, so its head is its oldest row whatever the newest rows say
/// — a read that stops at the newest rows cannot tell. `None` when the head
/// names no such stamp, or cannot be read.
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

/// Whether the transcript at `path` BEGAN before `floor`: its first stamped
/// row ([`head_stamp`]) is stamped before it — a conversation older than
/// the process, which the process resumed (or branched from one it resumed:
/// a launch `--fork-session` restores too), not one it began. `false` when
/// the head names no stamp, or cannot be read.
fn began_before(path: &Path, floor: u64) -> bool {
    open_regular(path)
        .and_then(|mut file| head_stamp(&mut file, false))
        .is_some_and(|at| at < floor)
}

/// Whether the transcript at `path` EXISTED before `moment` — the moment the
/// process was last seen in the session it switched out of, so before the
/// switch: the file was born before it (its creation time, where the file
/// system keeps one), or its first main-chain row of its own
/// ([`head_stamp`]) is stamped before it. A `/clear`'s session is created
/// by the switch, and so is a `/branch`'s — whose copies of the conversation
/// are not its own rows — so neither existed: a session that did is a
/// conversation the switch RESUMED, begun by whoever began it (an earlier
/// process, another tab, this process before a `/clear`). `false` when it
/// cannot be read.
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

/// The top-level stamp an OVERSIZED row's end carries: the last
/// `"timestamp":"…"` in `part` (Claude writes a row's own after its
/// `message`, so nothing nested follows it). `None` when it names none.
fn last_stamp(part: &[u8]) -> Option<u64> {
    const KEY: &[u8] = b"\"timestamp\":\"";
    let at = part.windows(KEY.len()).rposition(|w| w == KEY)? + KEY.len();
    let rest = &part[at..];
    let end = rest.iter().position(|&b| b == b'"')?;
    std::str::from_utf8(&rest[..end])
        .ok()
        .and_then(crate::harness::upgrade_models::parse_utc)
}

/// At the FIRST SIGHT of a process: when the transcript at `path` is a
/// conversation RESUMED from before `floor` — the process's first read of it
/// met a row from before the floor (`met_before`), or its head says so
/// ([`began_before`]) — the model Claude restores from it
/// ([`conversation_model`]); `None` for a session the process began (a
/// `/clear`, a fresh launch), or a conversation with no answer to restore.
fn resumed_model(path: &Path, floor: u64, met_before: bool) -> Option<Said> {
    (met_before || began_before(path, floor))
        .then(|| conversation_model(path, floor))
        .flatten()
}

/// At a SWITCH that came after `moment`: when the transcript at `path` is a
/// conversation that existed before it ([`existed_before`]) — a `/resume` —
/// the model Claude restored from it: its newest main-chain answer from
/// before `moment` ([`conversation_model`]). `None` for a session the switch
/// created (a `/clear`, a `/branch`), or a conversation with no answer to
/// restore.
fn resumed_since(path: &Path, moment: u64) -> Option<Said> {
    existed_before(path, moment)
        .then(|| conversation_model(path, moment))
        .flatten()
}

/// The model a resumed conversation last answered with before the process
/// took it up, the way Claude reads it to restore it (2.1.284: the newest
/// `assistant` row that is not meta and whose `message.model` is not
/// `<synthetic>`): the newest main-chain answer stamped before `before` —
/// the process's floor at a first sight, the moment it was last seen in the
/// session it left at a switch; a row since may be the process's own,
/// written after it took the conversation up. It is [`Said::Unread`] when
/// that answer's model is not plainly an id, when an answer cannot be placed
/// (no stamp), when a row too long to parse may be that answer (it holds
/// `"assistant"`, and no stamp since `before`), or when the search ran out
/// before the file's start — never an older answer's model. `None` when the
/// conversation holds no answer from before `before`: Claude then restores
/// nothing, and the process keeps its model.
fn conversation_model(path: &Path, before: u64) -> Option<Said> {
    use std::ops::ControlFlow::{Break, Continue};
    let mut found = None;
    let whole = rows_back(path, |row| {
        let raw = match row {
            Row::Whole(raw) => raw,
            Row::Oversized(part) => {
                // `"assistant"` with its quotes bare is JSON structure — a
                // row's `type` or its message's `role` — never text inside
                // a string, where the quotes are escaped.
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

/// The resolver's one-session usage fold: the session's
/// [`super::session_usage::SessionUsage`], so each read folds only what its
/// transcripts appended. One per watched session; dropping it releases the
/// fold. The transcript TAIL is not in it: that is the process's
/// ([`TailCache`]), which the host keeps while the process lives, across a
/// lapsed or stopped watch, where the fold goes with the watch.
#[derive(Debug)]
pub struct FooterCache {
    usage: Option<super::session_usage::SessionUsage>,
    /// The last refresh of `usage`, reused while none of its files moved.
    refreshed: Option<super::session_usage::Refresh>,
    /// What the limit wall is read from — the main transcript's limit-notice
    /// rows and the session's served time — as the last read that CAUGHT UP
    /// left them. A fold still behind is a prefix: a notice in it may have
    /// been lifted by a served row it has not reached yet.
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
    /// The usage of the conversation in `transcript` — the one the SAME read
    /// of `sessions/<pid>.json` named that model and effort came from
    /// ([`read_pid`]), so the two always describe one session —
    /// folded on from where the last read left it, unless no file of it
    /// moved ([`super::session_usage::SessionUsage::moved`], `stat`s only).
    /// A new transcript (`/clear` starts one, `/resume` switches to another)
    /// starts from nothing. Tokens are named only once every followed file
    /// is read to its end, and the limit wall is read from the last read
    /// that was (a prefix can hold a notice without the response that lifted
    /// it); what is left out is said. The fold is one conversation's, and
    /// shows only what this read names: a session with no transcript yet
    /// (`/clear` writes none until its first prompt) drops it and shows
    /// nothing, never the last conversation's. A read of the registry file
    /// that lands mid-write never gets here — it yields no facts at all, the
    /// fold is kept for the next good read, and the host holds the last good
    /// facts, model and Σ together, for that same process. The fold goes
    /// with the cache — when the session leaves the resolver's watch or its
    /// tab closes.
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
        let models = if refresh.caught_up {
            model_tokens(usage.total_fold().per_model())
        } else {
            (Vec::new(), 0)
        };
        UsageFacts::of(models, wall, Some(&refresh))
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
    read_tail_open(&mut file, len)
}

fn read_tail_open(file: &mut File, len: u64) -> Option<Vec<u8>> {
    let start = len.saturating_sub(TAIL_BYTES);
    let mut bytes = read_range(file, start, len)?;
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

/// Bytes `from..to` of `file` (fewer, if it shrank meanwhile), then `then`
/// — in a walk back, the head of the row the newer window cut — in ONE
/// buffer sized for both: allocated once, never grown after the read (until
/// 2026-09-29 it was sized to the window alone, and every window but the
/// newest was allocated, then grown to twice its size to take the head).
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

/// Where the Bash tool of each live Claude Code session LAUNCHED in `launch`
/// stands, as its transcript last recorded it ([`last_cwd`]) — the directory
/// the session's next command starts in, which neither the box nor the
/// terminal shows: the shell's own directory (`meta cwd=`, OSC 7) stays where
/// Claude Code was started while its Bash tool moves. A session is one whose
/// `<claude dir>/sessions/<pid>.json` names that launch directory and whose
/// pid is alive; every such session counts (several may share a launch
/// directory, and a superset is what a check that only ADDS directories
/// wants). `launch` itself is left out. Empty when nothing could be read.
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

/// The `cwd` of the newest main-thread row of a transcript tail that carries
/// one: where the session's Bash tool stood when the row was written. Claude
/// Code stamps every row with its working directory, and that directory
/// follows the Bash tool — a `cd x; …` command's rows carry `x` (measured on
/// the owner's 2.1.28x transcripts, 2026-09-27). SIDECHAIN rows (a subagent's
/// turns, with their own directory) are skipped; a relative or empty value is
/// not a directory.
pub fn last_cwd(body: &[u8]) -> Option<PathBuf> {
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
    /// may ([`facts_for_entry_cached`]).
    #[default]
    Unsaid,
    /// The newest statement about it is one this build cannot read — a
    /// `/model` whose result it does not parse, an effort set to `auto`:
    /// unknown, and nothing older may stand in for it.
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
    /// a model result's `with … effort` (`xhigh`) — and, across a switch that
    /// named no effort, only an effort read for the model switched to
    /// ([`Self::effort_for`]).
    pub effort: Said,
    /// A model switch these rows read that named its model and NO effort
    /// (no ` with … effort` clause), while the effort was still open — and
    /// the model it switched to. An effort is read for the model it ran
    /// with, and Claude runs each model at its own effort (2.1.283: the
    /// level is capped per model — `xhigh` and `max` fall to `high` where
    /// the model lacks them — or taken from the model's settings or default,
    /// and a model with no effort levels runs none), so the effort from
    /// before the switch stands only when the model before it is this same
    /// one. The newest model the OLDER rows name decides it ([`Self::over`]);
    /// `Some` here when nothing older named one, and the process's launch
    /// flag or card then does ([`facts_for_entry_cached`]). An effort that is
    /// [`Said::Is`] meanwhile was read above the switch (an `/effort`
    /// result) and waits on the same answer.
    effort_for: Option<String>,
}

impl TailFacts {
    /// `self` (newer rows) over `older`: what the newer rows decided stands,
    /// and a switch in them that named no effort takes the older effort only
    /// when the older rows' model is the one it switched to.
    fn over(self, older: Self) -> Self {
        let model = if self.model.decided() {
            self.model
        } else {
            older.model.clone()
        };
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

    /// A row that names the model in force at it (`said`), met newest first:
    /// it names the model when nothing newer did, and it answers a newer
    /// switch that named no effort — the effort stands only if this is the
    /// model it switched to.
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

/// The row Claude Code writes when a `/model` choice is MADE (2.1.201 through
/// 2.1.283): a `type:user` row whose `message.content` starts with this tag;
/// its RESULT is the next such row ([`COMMAND_STDOUT`]). A picker that
/// changed nothing writes the same tag and a `Kept model as …` result as
/// `type:system`, `subtype:local_command` rows with a top-level `content`
/// (measured on a 2.1.278 transcript; every `Kept model as` call site in the
/// 2.1.283 binary still passes `{display:"system"}`) — rows [`scan`] reads
/// no fact from: the model and the effort are what they were.
const MODEL_COMMAND: &str = "<command-name>/model</command-name>";

/// The row Claude Code writes for an `/effort` command, its result after it.
const EFFORT_COMMAND: &str = "<command-name>/effort</command-name>";

/// The tag every local command's row opens with (`/model`, `/effort`,
/// `/fast`, …): a command's RESULT is the row directly under it.
const COMMAND_NAME: &str = "<command-name>";

/// The tag a local command's RESULT row opens with.
const COMMAND_STDOUT: &str = "<local-command-stdout>";

/// The effort levels the footer names, as Claude Code's `/effort` takes them —
/// its top-effort mode ([`CLAUDE_TOP_EFFORT_KEY`], the mode's name as well as
/// its settings key) shown as `xhigh`, the level it runs at (Claude's own table
/// maps that name to `"xhigh"`; the transcript's `effort`, the launch card and
/// the result's own gloss, `xhigh + dynamic workflow orchestration`, all say
/// `xhigh`; Claude's top rule keeps the mode's own tag).
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

/// A model as a model RESULT or the launch card spells it, as the footer
/// shows it: `Opus 5.5 (default)` → `Opus 5.5`; ` (1M context)` and `
/// (recommended)` go too, in any order, as [`model_display`] drops `[1m]`;
/// an id (`claude-fable-5-1`, the older results' form) through
/// [`model_display`]. `None` for anything that is not plainly a name — at
/// most 64 chars of letters, digits, spaces and `.-_()[]` — so a third
/// party's text never reaches the glass as a model.
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

/// What an `/effort` RESULT (`<local-command-stdout>…`, anchors
/// `effort.set`, `effort.current`, `effort.auto`) says: the level it names
/// ([`effort_level`]), or [`Said::Unread`] for `auto` (the model decides) and
/// a level this build does not know. `None` when the row is no effort result.
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

/// A local command's RESULT row folded into `facts`, newest first: a model
/// result names the model in force from it on
/// ([`crate::harness::upgrade_models::model_result_of`]) — which also
/// answers a newer switch that named no effort ([`TailFacts::meet_model`])
/// — and the effort from its `with … effort` clause. A result WITHOUT that
/// clause switched the model and named no effort: the effort from before it
/// stands only if it was read for the model it names ([`TailFacts`]'s
/// `effort_for`), and an unreadable one leaves the effort unknown. An
/// `/effort` result names the effort. Whether the result named a model the
/// reader could read: a `/model` COMMAND row directly above a readable
/// result is that result's own command, not an unreadable choice ([`scan`]).
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

/// [`read_tail_facts`]'s scan, pure: rows newest first, stopping once both
/// facts are decided. SIDECHAIN rows (a subagent's turns) are skipped, and so
/// is a `<synthetic>` model — neither is the model this session is talking to.
///
/// What decides the MODEL, newest first: an answer (`message.model`), or a
/// model RESULT Claude writes for its own `/model` or `/fast` (`Set model to
/// `Opus 5.5 (default)``, `Fast mode ON · model set to …`; [`fold_result`])
/// — so a choice shows the moment it is made (owner, 2026-09-28: "I need to
/// see the current model selection in the footer"). A command's result is
/// the command or result row DIRECTLY under it: a `/model` COMMAND row whose
/// next row down is no readable model result (the result row is written
/// after it, so it is met first) is a choice this build cannot read — model
/// and effort are [`Said::Unread`], never the ones before it — and a
/// readable result under another command (`/fast`) answers only that one. A
/// `/fast` row whose result carries no `model set to` clause did not move
/// the model and decides nothing (Claude appends the clause exactly when it
/// moves one). The EFFORT: an answer's `effort`, an `/effort` result, a
/// model result's `with … effort`. A model result without that clause
/// switched the model and named no effort, and the effort from before it
/// stands only if it was read for the same model — the model the rows above
/// name — else it is [`Said::Unread`] (a model runs its own effort:
/// [`TailFacts`]). An `/effort` command whose result cannot be read leaves
/// it unknown.
///
/// `since` is the process's start (unix seconds). The transcript belongs to
/// the conversation, and a `--resume`d process appends to the file its
/// predecessors wrote — so a row stamped before `since` is a predecessor's,
/// and so is every row above it (the file is in append order, and everything
/// this process writes comes after its fork; the stamps themselves are not
/// monotonic, which is why the order and not the clock is the argument). The
/// scan stops there: what this process has not said stays [`Said::Unsaid`]
/// rather than the model it replaced. A row with no stamp is not attributed
/// to anyone. `None` (no start known at all) reads the tail as it is — the
/// caller decides whether that may be shown ([`facts_for_pid`] does not).
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
    // The command or result row met last — the one directly UNDER the next
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
        // Main's 6be8652d3 projections: only a row that may be a command or
        // its result keeps its string content (`command`); every other row
        // keeps the metadata this scan reads (`type`, `isSidechain`,
        // `timestamp`, `effort`, `message.model`).
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

/// A value safe to paint as-is: short, and letters, digits, `-`, `.`, `_`,
/// `[`, `]` only — a transcript is written by a program, but it is still text
/// that ends up in a chrome row.
fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '[' | ']'))
}

/// A model id's display name, the vendor's own spelling:
/// `claude-opus-5-5` → `Opus 5.5`, `claude-fable-5-1` → `Fable 5.1`,
/// `claude-sonnet-5` → `Sonnet 5`, `claude-haiku-4-5-20251001` → `Haiku 4.5`
/// (a trailing date is dropped), `claude-opus-5-5[1m]` → `Opus 5.5`. An id
/// that does not have that shape is shown as it is.
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

/// The branch that `cwd` is in, read straight from `.git` — no `git`
/// process. A worktree's `.git` FILE (`gitdir: …`) is followed; a detached
/// HEAD shows its first seven hex digits. `None` outside a repository, or
/// when the read was refused.
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
    /// The kernel refused a read on the way with `EPERM` — on macOS, a folder
    /// privacy consent (TCC) aterm does not hold. Not `Absent`: the host raises
    /// its consent attention for the session instead of silently showing nothing.
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

/// [`git_head`], saying WHY there is no answer when the kernel refused a read.
pub fn read_git_head(cwd: &Path) -> GitHeadRead {
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
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if is_eperm(&error) => return Err(()),
        Err(_) => return Ok(None),
    };
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return Ok(None);
    }
    let mut text = String::new();
    Ok(file
        .take(MAX_GIT_FILE_BYTES)
        .read_to_string(&mut text)
        .ok()
        .map(|_| text))
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

/// A name from the filesystem, fit for a chrome row: non-empty, at most 128
/// chars, and no control characters (a directory or ref name may hold any
/// byte but `/` and NUL).
fn printable(name: &str) -> Option<String> {
    (!name.is_empty() && name.chars().count() <= 128 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

/// Everything the footer shows for Claude Code process `pid`, from the files
/// named in the module header. Bounded reads only — one small JSON file, one
/// transcript tail, one `HEAD` — but still file I/O: callers keep it off the
/// event loop. `started` is the pid-reuse guard [`parse_session_entry`] takes,
/// and — with the registry's own `procStart` behind it, and the running
/// image's `startedAt` above both — the floor under the transcript rows that
/// may speak for this process ([`tail_facts`]): with no start known at all,
/// the transcript stays out rather than risk a predecessor's model.
pub fn facts_for_pid(claude_dir: &Path, pid: u32, started: Option<u64>) -> Option<FooterFacts> {
    facts_for_pid_cached(claude_dir, pid, started, &mut TailCache::default())
}

/// [`facts_for_pid`] with a transcript cache owned by the host's one
/// background resolver. Every other source is still read live on every ask.
/// An unreadable registry or transcript cannot preserve a cached tail for a
/// later process; the next successful request reads it afresh.
pub fn facts_for_pid_cached(
    claude_dir: &Path,
    pid: u32,
    started: Option<u64>,
    cache: &mut TailCache,
) -> Option<FooterFacts> {
    let Some(entry) = session_of_pid(claude_dir, pid, started) else {
        cache.clear();
        return None;
    };
    Some(facts_for_entry_cached(
        claude_dir,
        pid,
        started,
        &entry,
        &LaunchFacts::default(),
        cache,
    ))
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

/// [`facts_for_entry_at`], read now.
pub fn facts_for_entry_cached(
    claude_dir: &Path,
    pid: u32,
    started: Option<u64>,
    entry: &SessionEntry,
    launch: &LaunchFacts,
    cache: &mut TailCache,
) -> FooterFacts {
    facts_for_entry_at(
        claude_dir,
        pid,
        started,
        entry,
        launch,
        cache,
        SystemTime::now(),
    )
}

/// [`facts_for_pid_cached`] for a registry `entry` the caller has read, with
/// what the process's own command line says (`launch`, [`launch_facts`]),
/// read at `read_at` — the wall clock no later than the registry read that
/// named `entry`'s session: the cache records it as when the process was
/// last seen there, which a later switch out of that session came after
/// ([`TailCache`]).
///
/// THE ORDER, newest first — never the model of the process before this one
/// on its say-so: what the transcript since the process entered its current
/// session decides ([`tail_facts`]: an answer, a model or effort result, or
/// an unreadable choice, which stays unknown); else what this same process
/// decided in the session it left — across a `/clear`, and across an
/// in-REPL `/resume` where the process pinned its model (a restore pins it
/// too); an unpinned `/resume` runs the resumed conversation's own model, as
/// Claude restores it, and so does a launch that resumed this session and
/// pinned nothing ([`TailCache`]); else the process's own `--model`; else
/// nothing, and the facts say so ([`FooterFacts::model_open`],
/// [`FooterFacts::effort_open`]) so the host may fill them from the
/// process's launch card ([`FooterFacts::filled_from`]). A switch that named
/// no effort, with nothing since the floor naming the model before it, keeps
/// the effort only for the launch model it switched from — the flag's, or
/// the card's.
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

/// [`facts_for_entry_at`] over `entry`'s transcript as the caller looked it
/// up ([`transcript_path`]; `None` when the session has written none): the
/// ONE lookup a footer read makes, which [`read_pid`] shares with the fold
/// so the model and the usage are read from the same file.
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
    // predecessor's model ([`facts_for_pid`]).
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
    let (branch, repo_read_denied) = match read_git_head(&entry.cwd) {
        GitHeadRead::Found(branch) => (Some(branch), None),
        GitHeadRead::Absent => (None, None),
        GitHeadRead::Denied => (None, Some(entry.cwd.clone())),
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
        // A switch that named no effort, and nothing since the floor names
        // the model before it: that was the launch model. Its `--model`
        // decides; without one the card may, for an effort still open.
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
        path: home_path(&entry.cwd, aterm_types::dirs::home_dir().as_deref()),
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
    }
}

/// Claude Code's launch card, read for the footer: only under a LIVE REPL —
/// a footer row this build reads under the composer ([`live_repl_row`]),
/// which a predecessor's box left on the main screen by the inline renderer
/// does not have (`Press Ctrl-C again to exit` sits there) — and only when
/// the card is the newest on the screen with no user message under it
/// (`aterm_phase::launch_card`). Its model and effort as the footer shows
/// them ([`footer_model_name`], [`effort_level`]); `None` when the model row
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
    /// These facts with what `card` — the launch card the host kept for this
    /// process — says, where nothing newer decided it: the model only while
    /// [`Self::model_open`], the effort only while [`Self::effort_open`] and
    /// — after a switch that named no effort — only from a card that names
    /// the model switched to ([`Self::effort_for`]), and only from a card of
    /// THIS build (`version`), so a predecessor's card of another build never
    /// names this process's model.
    #[must_use]
    pub fn filled_from(&self, card: Option<&aterm_phase::LaunchCard>) -> Self {
        let mut out = self.clone();
        if let Some(card) = card.filter(|c| self.version.as_deref() == Some(c.version.as_str())) {
            if self.model_open && out.model.is_none() {
                out.model = Some(card.model.clone());
            }
            if self.effort_open
                && out.effort.is_none()
                && self.effort_for.as_ref().is_none_or(|to| *to == card.model)
            {
                out.effort.clone_from(&card.effort);
            }
        }
        out
    }
}

/// `cwd` as the footer shows it: under `home`, `~` and the rest (`~/aterm`);
/// elsewhere, whole. A home of `/` is no home to abbreviate (every path is
/// under it). `None` for a path that would put a control character on the
/// glass.
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

/// When one footer read is made ([`read_pid`]): `read_at`, the wall clock no
/// later than its registry read ([`facts_for_entry_at`]); `now` (unix
/// seconds), the instant its limit wall is judged at; and `offset_at(zone,
/// t)`, a zone's offset from UTC at the instant `t` (`None`: the local zone),
/// injected because it reads the system's zone data
/// (`supervise::limit::offset_at`; see [`super::session_usage::wall_of`]).
pub struct ReadClock<'a> {
    /// No later than the registry read.
    pub read_at: SystemTime,
    /// Unix seconds, for the limit wall.
    pub now: i64,
    /// A zone's offset from UTC at an instant.
    pub offset_at: &'a dyn Fn(Option<&str>, i64) -> Option<i64>,
}

/// ONE FOOTER READ OF LIVE CLAUDE CODE PROCESS `pid`, as the window's footer
/// resolver makes it (`aterm-gui`'s `claude_footer::resolve_into` calls
/// this and nothing else): its facts ([`facts_for_entry_at`] over `tail`),
/// the session's usage folded on in `fold` ([`FooterCache::usage`], at most
/// [`FOLD_BUDGET`] bytes a call) — the limit wall is the transcript's newest
/// limit notice while its reset is ahead of `clock.now` and nothing was
/// served since — and the frozen-program remedy's resume line
/// ([`super::resume::of_entry`]). `launch` is asked, once, with the registry
/// entry the read named, for what the process's own command line says: its
/// launch facts ([`launch_facts`]) and its argv, `argv[0]` first (the host
/// reads them again when the entry names a new image).
///
/// ONE read of `sessions/<pid>.json` feeds all of it (main's 5bebdcf36,
/// ported into this one function by the review of 2026-09-28 so that the
/// torn-read tests below guard the path the window runs): the fold follows
/// the transcript the same parse named that model and effort were read
/// from, and the resume line names that same conversation, so after
/// `/clear` or `/resume` the footer never pairs one conversation's model
/// with another's Σ or wall. Claude Code rewrites that file on every status
/// change; a read that lands mid-write yields no facts (the host holds the
/// last good ones for the same process) and keeps the fold, so the next
/// good read folds only what was appended.
pub fn read_pid(
    claude_dir: &Path,
    pid: u32,
    started: Option<u64>,
    launch: impl FnOnce(&SessionEntry) -> (LaunchFacts, Vec<String>),
    tail: &mut TailCache,
    fold: &mut FooterCache,
    clock: &ReadClock<'_>,
) -> Option<FooterFacts> {
    let Some(entry) = session_of_pid(claude_dir, pid, started) else {
        tail.clear();
        return None;
    };
    let (launch, argv) = launch(&entry);
    // ONE lookup of the transcript for the facts and the fold alike.
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
    facts.resume = super::resume::of_entry(&argv, &entry, started);
    Some(facts)
}

/// Where, in a screen of `rows`, Claude Code's permission-mode row is: under
/// the composer's bottom rule (`aterm_phase::phase::composer_bottom`), before
/// the first blank row, the one that opens with a mode pill ([`is_mode_row`]).
/// Found by STRUCTURE — a column-0 composer framed by full-width rules is
/// only ever Claude Code's — and never by the `(shift+tab to cycle)` hint,
/// whose key a user can rebind. The footer never writes it: the lights read
/// the mode from it (`harness::lights`), and the launch card is read only
/// under one ([`launch_card`]).
pub fn mode_row(rows: &[String]) -> Option<usize> {
    let bottom = aterm_phase::phase::composer_bottom(rows)?;
    (bottom + 1..rows.len())
        .take_while(|&i| !rows[i].trim().is_empty())
        .find(|&i| is_mode_row(&rows[i]))
}

/// Where a LIVE REPL's own footer row is, under the composer's bottom rule:
/// its mode row ([`mode_row`]) — 2.1.283 draws a pill in every mode, default
/// mode's `⏸ manual mode on` included (measured, the `footer-manual`
/// fixture) — or, where Claude draws none (its render code draws the pill
/// only while the session may set its permission mode; a build before the
/// `manual mode` pill drew default mode bare), the PILL-LESS row, whose left
/// hint slot opens it ([`is_bare_footer_row`]). Either is a REPL that is up:
/// a predecessor's box the inline renderer leaves on the main screen has
/// `Press Ctrl-C again to exit` there instead.
#[must_use]
pub fn live_repl_row(rows: &[String]) -> Option<usize> {
    if let Some(row) = mode_row(rows) {
        return Some(row);
    }
    let bottom = aterm_phase::phase::composer_bottom(rows)?;
    (bottom + 1..rows.len())
        .take_while(|&i| !rows[i].trim().is_empty())
        .find(|&i| is_bare_footer_row(&rows[i]))
}

/// Whether `row` is Claude Code's pill-less footer row: after its two-space
/// indent, its left hint slot — `? for shortcuts` at rest, `esc to clear`
/// with a draft, `esc to interrupt` while a turn runs (anchors
/// `footer.shortcuts`, `footer.clear`, `busy.interrupt`). No other row: the
/// completion list and the pickers draw under the rule too.
#[must_use]
pub fn is_bare_footer_row(row: &str) -> bool {
    row.strip_prefix("  ").is_some_and(|rest| {
        ["footer.shortcuts", "footer.clear", "busy.interrupt"]
            .into_iter()
            .any(|id| rest.starts_with(aterm_phase::anchor(id)))
    })
}

/// Whether `row` is Claude Code's permission-mode row: it OPENS with a pill
/// this build knows (`  ⏵⏵ bypass permissions on`, [`pill_indicator`]), and
/// a `(<key> to cycle)` hint after it, when there is one, is closed — or cut
/// BETWEEN WORDS, the way a narrow pane cuts it (`(shift+tab`, `(shift+tab
/// to`; NARROW PANES, module doc, [`cycle_hint_len`]). Until 2026-09-28 a cut
/// hint read as no mode row at all, so at 39–48 columns in bypass mode the
/// lights read no mode (main's 0885b3465 found it in the painter's planner
/// this reader replaced). A hint cut mid-word is no shape the vendor draws,
/// and stays no mode row. The pill-less row — default mode's `? for
/// shortcuts`, `esc to clear` with a draft — is no mode row: it says no mode.
#[must_use]
pub fn is_mode_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("  ") else {
        return false;
    };
    PILLS.iter().any(|(glyph, indicator)| {
        rest.strip_prefix(&format!("{glyph} {indicator} on"))
            .is_some_and(|after| {
                after
                    .strip_prefix(" (")
                    .is_none_or(|hint| hint.contains(')'))
                    || cycle_hint_len(after) > 0
            })
    })
}

/// How many chars of `after` — the row past a pill — are the dim
/// `(<key> to cycle)` hint: all of it, or the head a narrow pane leaves.
/// The pill and its hint are ONE wrapped text of which the row shows the
/// first line (NARROW PANES, module doc), so the hint is cut only between
/// words — ` (<key>` or ` (<key> to` — and the line ends there: at the row's
/// end or at the blanks before a separator. `0` when no hint follows.
fn cycle_hint_len(after: &str) -> usize {
    let Some(hint) = after.strip_prefix(" (") else {
        return 0;
    };
    let line = &hint[..hint.find("  ").unwrap_or(hint.len())];
    if let Some(close) = line.find(')') {
        return if line[..close].ends_with("to cycle") {
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

/// The vendor's mode pills: glyph, indicator. `⏸` goes only with plan and
/// manual mode, `⏵⏵` with the rest — the pairing the 2.1.282 mode table draws.
const PILLS: [(&str, &str); 6] = [
    ("\u{23F5}\u{23F5}", "bypass permissions"),
    ("\u{23F5}\u{23F5}", "auto mode"),
    ("\u{23F5}\u{23F5}", "accept edits"),
    ("\u{23F5}\u{23F5}", "don't ask"),
    ("\u{23F8}", "plan mode"),
    ("\u{23F8}", "manual mode"),
];

/// The permission-mode indicator (`auto mode`) of the pill `row` opens with,
/// or `None` when it opens with none — the one reading of the pill the footer
/// and the lights (`harness::lights`) share.
#[must_use]
pub fn pill_indicator(row: &str) -> Option<&'static str> {
    let rest = row.strip_prefix("  ")?;
    PILLS
        .iter()
        .find(|(glyph, indicator)| rest.starts_with(&format!("{glyph} {indicator} on")))
        .map(|(_, indicator)| *indicator)
}

/// Whether `row` OPENS like a mode pill — a pill glyph after the indent —
/// whatever its indicator says. With [`pill_indicator`] answering `None`,
/// that is a pill this build does not know: the vendor has renamed or added a
/// mode, and the host logs it (`claude_footer`'s drift note) instead of
/// letting the lights go quietly grey.
#[must_use]
pub fn opens_with_pill_glyph(row: &str) -> bool {
    row.strip_prefix("  ")
        .is_some_and(|rest| PILLS.iter().any(|(glyph, _)| rest.starts_with(glyph)))
}

/// Whether `row` is a KNOWN pill cut short — a narrow pane shows only the
/// head of `⏵⏵ bypass permissions on`, e.g. `⏵⏵ bypass permissi`. That is
/// not the vendor's drift; it is geometry.
#[must_use]
pub fn is_cut_pill(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("  ") else {
        return false;
    };
    let rest = rest.trim_end();
    !rest.is_empty()
        && PILLS
            .iter()
            .any(|(glyph, indicator)| format!("{glyph} {indicator} on").starts_with(rest))
}

/// The footer's marked values, in order: a limit wall the session hit while
/// it stands (FIRST: a narrow row drops values from the back, and the wall
/// says why nothing moves), then model and effort under one mark, the
/// working directory, the branch, and the session's tokens LAST — the first
/// a narrow row gives up. A value the facts lack is left out, not shown as a
/// placeholder.
pub fn segments(facts: &FooterFacts) -> Vec<Segment> {
    let mut out = Vec::with_capacity(5);
    let usage = facts.usage.as_ref();
    if let Some(wall) = usage.and_then(|u| u.wall.as_ref()) {
        out.push(Segment {
            mark: WALL_MARK,
            text: super::session_usage::wall_text(wall),
        });
    }
    let model_effort = match (&facts.model, &facts.effort) {
        (Some(model), Some(effort)) => Some(format!("{model} {effort}")),
        (Some(one), None) | (None, Some(one)) => Some(one.clone()),
        (None, None) => None,
    };
    if let Some(text) = model_effort {
        out.push(Segment {
            mark: MODEL_MARK,
            text,
        });
    }
    if let Some(path) = &facts.path {
        out.push(Segment {
            mark: PATH_MARK,
            text: path.clone(),
        });
    }
    if let Some(branch) = &facts.branch {
        out.push(Segment {
            mark: BRANCH_MARK,
            text: branch.clone(),
        });
    }
    if let Some(text) = usage.and_then(super::session_usage::usage_text) {
        out.push(Segment {
            mark: USAGE_MARK,
            text,
        });
    }
    out
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

/// The cells `segments` take: each mark, a space and its value, [`GAP`]
/// apart.
#[must_use]
pub fn segments_width(segments: &[Segment]) -> usize {
    segments
        .iter()
        .map(|s| aterm_grapheme::char_width(s.mark) + 1 + text_width(&s.text))
        .sum::<usize>()
        + GAP * segments.len().saturating_sub(1)
}

/// The run of the composer's bottom rule aterm may write into: the WIDEST
/// run of `coverable` cells — plain rule glyphs in the rule's own colours,
/// the host's reading of the row (`claude_footer`) — the rightmost of equal
/// runs. Any other ink on the row is Claude's and bounds the run. `None` when
/// the row has no plain rule glyph at all.
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
/// [`RULE_LEAD`] glyphs at its left and the one glyph at its right end, which
/// stays — the way Claude leaves one `─` after its own top-effort tag.
#[must_use]
pub fn rule_room(run: &std::ops::Range<usize>) -> usize {
    run.len().saturating_sub(RULE_LEAD + 1)
}

/// The widths of the lights' block (`claude_lights`): its chips, and its
/// whole and short titles (a title carries its own two cells of gap before
/// the chips) — and whether the title is a REASON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightsWidth {
    /// The chips, one blank apart.
    pub chips: usize,
    /// `Light: what  `.
    pub title: usize,
    /// `what  `.
    pub short: usize,
    /// The title says WHY — where a mode return stopped, a refusal, what
    /// Claude said of a switch — rather than what the pointer, the selection
    /// or a toggle in flight shows: it outranks the branch and the path
    /// ([`fit_rule`]).
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
    /// The fewest cells it takes: ` <chips>  <title> `, [`RULE_SEP`] glyphs,
    /// ` <facts> ` — each group padded by one space a side, a title with its
    /// own two cells of gap, the separator only between two groups. The
    /// lights stand at the rule's LEFT end and the facts at its right, rule
    /// glyphs between.
    #[must_use]
    pub fn width(&self, lights: Option<LightsWidth>) -> usize {
        let facts = if self.segments.is_empty() {
            0
        } else {
            segments_width(&self.segments) + 2
        };
        let lights = match lights.filter(|_| self.chips) {
            Some(l) => {
                let title = match self.title {
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
}

/// The facts as a narrow rule gives them up: the session's tokens first,
/// then the path cut to `…/<dir>`, the branch, the path, the effort and the
/// model, a limit wall ([`segments`] puts it first) standing alone last. A
/// rule too narrow for the wall itself gives up the WALL, not everything
/// behind it: the same rungs follow without it, so a narrow pane keeps the
/// model it showed before a limit was hit (Claude's own screen carries the
/// live notice) — the model alone, with no wall, is the last rung.
fn facts_ladder(facts: &FooterFacts) -> Vec<Vec<Segment>> {
    let short = facts.path.as_deref().and_then(elide_path);
    let whole = segments(facts);
    let wall = whole.iter().find(|s| s.mark == WALL_MARK).cloned();
    let with = |wall: Option<&Segment>, path: Option<&str>, branch: bool, effort: bool| {
        let mut rung: Vec<Segment> = wall.into_iter().cloned().collect();
        rung.extend(segments(&FooterFacts {
            model: facts.model.clone(),
            effort: facts.effort.clone().filter(|_| effort),
            path: path.map(str::to_owned),
            branch: facts.branch.clone().filter(|_| branch),
            ..FooterFacts::default()
        }));
        rung
    };
    let path = facts.path.as_deref();
    let mut rungs = vec![whole];
    for wall in [wall.as_ref(), None] {
        rungs.push(with(wall, path, true, true));
        if short.is_some() {
            rungs.push(with(wall, short.as_deref(), true, true));
        }
        rungs.push(with(wall, path, false, true));
        if short.is_some() {
            rungs.push(with(wall, short.as_deref(), false, true));
        }
        rungs.push(with(wall, None, false, true));
        rungs.push(with(wall, None, false, false));
        if let Some(wall) = wall {
            rungs.push(vec![wall.clone()]);
        }
    }
    let mut out: Vec<Vec<Segment>> = Vec::new();
    for rung in rungs {
        if !out.contains(&rung) {
            out.push(rung);
        }
    }
    out
}

/// What of the facts and the lights fits `room` cells of the rule
/// ([`rule_room`]). The owner decided (2026-09-28) that the facts go into
/// this rule and fit at any width, 80 columns included; the ORDER in which a
/// rule too narrow for everything gives things up is this module's, and the
/// parent review's of 2026-09-28:
///
/// * a TRANSIENT title — what the pointer, the keyboard selection or a
///   toggle in flight shows — goes first, whole then short then gone, before
///   any fact: the facts stay whole at 80 columns while a light is hovered;
/// * a light's REASON ([`LightsWidth::reason`]: where a mode return stopped,
///   a refusal) outranks the tokens, the branch and the path: the whole
///   title goes, then the path is cut to `…/<dir>`, and then the reason,
///   short, takes the tokens', the branch's and the path's room — never the
///   effort's, the model's or a limit wall's; a reason too long even beside
///   the model and effort goes, and the facts come back;
/// * then the facts give way ([`facts_ladder`]) — the session's tokens, the
///   path cut to `…/<dir>`, the branch, the path, the effort, the model, a
///   limit wall standing alone last and, where even it does not fit, the
///   model back without it — then the chips, and the model goes last.
///
/// At 80 columns and wider the facts normally fit whole: the rule is the
/// pane's whole width.
///
/// The chips stand at the rule's LEFT end (after [`RULE_LEAD`] glyphs) and
/// their title right of them, the facts right-aligned at its right end: so
/// the chips never move for a title, nor for the facts — a title comes and
/// goes under the pointer, and the chip it names stays under it.
#[must_use]
pub fn fit_rule(facts: &FooterFacts, room: usize, lights: Option<LightsWidth>) -> RuleFit {
    let ladder = facts_ladder(facts);
    let chips = lights.is_some();
    let whole = ladder.first().cloned().unwrap_or_default();
    let model = ladder.last().cloned().unwrap_or_default();
    // The model-and-effort segment and a limit wall: what a reason never
    // takes the room of.
    let top = |rung: &[Segment]| {
        rung.iter()
            .filter(|s| s.mark == MODEL_MARK || s.mark == WALL_MARK)
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut tries = Vec::new();
    if chips {
        for title in [TitleFit::Full, TitleFit::Short] {
            tries.push(RuleFit {
                segments: whole.clone(),
                title,
                chips,
            });
        }
        if lights.is_some_and(|l| l.reason) {
            for rung in ladder.iter().skip(1).filter(|r| top(r) == top(&whole)) {
                tries.push(RuleFit {
                    segments: rung.clone(),
                    title: TitleFit::Short,
                    chips,
                });
            }
        }
    }
    for segments in ladder {
        tries.push(RuleFit {
            segments,
            title: TitleFit::None,
            chips,
        });
    }
    if chips {
        tries.push(RuleFit {
            segments: model,
            title: TitleFit::None,
            chips: false,
        });
        // Chips narrower than the model still draw where the model cannot.
        tries.push(RuleFit {
            segments: Vec::new(),
            title: TitleFit::None,
            chips,
        });
    }
    tries
        .into_iter()
        .find(|fit| fit.width(lights) <= room)
        .unwrap_or(RuleFit {
            segments: Vec::new(),
            title: TitleFit::None,
            chips: false,
        })
}

/// The most cells the lights' chips may take in a rule of `room` cells and
/// still be drawn ([`fit_rule`]): beside the model alone — the chips give way
/// after every fact but the model — when the model fits at all. The lights'
/// block is built to it, and the keyboard selects only a chip that fits it.
#[must_use]
pub fn chips_room(facts: &FooterFacts, room: usize) -> usize {
    let model: Vec<Segment> = facts_ladder(facts).pop().unwrap_or_default();
    let model_w = if model.is_empty() {
        0
    } else {
        segments_width(&model) + 2
    };
    let beside = if model_w > 0 && model_w <= room {
        model_w + RULE_SEP
    } else {
        0
    };
    room.saturating_sub(beside + 2)
}

/// THE WORLD `ClaudeFooterModel` READS (`aterm_spec::derive::
/// claude_footer_model_model`): every action but `Read` is Claude Code's or
/// a person's — a launch, a card, a transcript row, a registry rewrite — and
/// the footer only reads them. `Read` is bound where the reader runs
/// (`facts_for_entry_from`, which [`facts_for_entry_at`] and [`read_pid`]
/// both run; [`TailCache`]'s read and switch, [`scan`],
/// [`fold_result`]); these are the environment's, waived here, each one the
/// Tier-1 bind (`tests/conformance_footer_model.rs`) performs on disk.
/// Compiled only for proof and test builds: nothing calls it.
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
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write as _;

    /// What a test runs before a registry read, with the file's path.
    type RegistryHook = Box<dyn FnMut(&Path)>;

    thread_local! {
        /// Runs before each read of a `sessions/<pid>.json` on this thread
        /// ([`session_of_pid`]): how a test lands Claude Code's rewrite of
        /// the file inside one footer read.
        static REGISTRY_READ: std::cell::RefCell<Option<RegistryHook>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The test seam [`session_of_pid`] calls before it reads.
    pub(super) fn before_registry_read(path: &Path) {
        REGISTRY_READ.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(path);
            }
        });
    }

    thread_local! {
        /// How many transcript lookups ([`transcript_path`]) this thread
        /// has made: each one a stat of the slug's path and, where the slug
        /// misses, a scan of every project directory.
        static TRANSCRIPT_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// The test seam [`transcript_path`] calls on every lookup.
    pub(super) fn on_transcript_lookup() {
        TRANSCRIPT_LOOKUPS.with(|n| n.set(n.get() + 1));
    }

    /// One footer read of process `pid` as the window's resolver makes it
    /// ([`read_pid`]), over the process's `tail` — ONE per process, kept
    /// across reads as the window keeps it (`ProcessTails`): a fresh one per
    /// read forgets what the process chose, and asserts a model the window
    /// never shows (the review of 2026-09-28) — and the tab's `fold`, with no
    /// launch facts and no argv, read now.
    #[cfg(unix)]
    fn folded(
        claude: &Path,
        pid: u32,
        started: Option<u64>,
        tail: &mut TailCache,
        fold: &mut FooterCache,
        now: i64,
        offset_at: &dyn Fn(Option<&str>, i64) -> Option<i64>,
    ) -> Option<FooterFacts> {
        let clock = ReadClock {
            read_at: SystemTime::now(),
            now,
            offset_at,
        };
        read_pid(
            claude,
            pid,
            started,
            |_| (LaunchFacts::default(), Vec::new()),
            tail,
            fold,
            &clock,
        )
    }

    /// One footer read ([`folded`]) of process `pid` while Claude Code
    /// rewrites its registry file: the FIRST read of `sessions/<pid>.json` in
    /// it finds the file whole, and every later one finds it cut in half,
    /// mid-write. The file is whole again afterwards. The facts, and how many
    /// times the read opened the registry file.
    #[cfg(unix)]
    fn read_torn_after_first(
        claude: &Path,
        pid: u32,
        started: Option<u64>,
        tail: &mut TailCache,
        cache: &mut FooterCache,
        now: i64,
        offset_at: &dyn Fn(Option<&str>, i64) -> Option<i64>,
    ) -> (Option<FooterFacts>, usize) {
        let file = claude.join(format!("sessions/{pid}.json"));
        let whole = std::fs::read_to_string(&file).unwrap();
        let reads = std::rc::Rc::new(std::cell::Cell::new(0_usize));
        let (seen, torn, half) = (
            std::rc::Rc::clone(&reads),
            file.clone(),
            whole[..whole.len() / 2].to_owned(),
        );
        REGISTRY_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |path: &Path| {
                if path == torn {
                    seen.set(seen.get() + 1);
                    if seen.get() > 1 {
                        std::fs::write(&torn, &half).unwrap();
                    }
                }
            }));
        });
        let facts = folded(claude, pid, started, tail, cache, now, offset_at);
        REGISTRY_READ.with(|hook| *hook.borrow_mut() = None);
        std::fs::write(&file, &whole).unwrap();
        (facts, reads.get())
    }

    #[cfg(unix)]
    fn cache_test_dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "aterm-footer-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    fn assistant_row(model: &str, timestamp: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{timestamp}","effort":"high","message":{{"model":"{model}"}}}}"#
        )
    }

    #[cfg(unix)]
    #[test]
    fn tail_cache_reuses_only_one_process_and_one_unchanged_file() {
        let root = cache_test_dir("tail-cache");
        let path = root.join("session.jsonl");
        let before = "2026-09-27T11:00:00Z";
        let during = "2026-09-27T13:00:00Z";
        let after = "2026-09-27T15:00:00Z";
        let start = crate::harness::upgrade_models::parse_utc("2026-09-27T12:00:00Z").unwrap();
        let old = format!("{}\n", assistant_row("claude-opus-5", before));
        std::fs::write(&path, &old).unwrap();
        let mut cache = TailCache::default();
        let read = |cache: &mut TailCache, started, since| {
            cache.read(&path, 4242, started, since).unwrap()
        };

        // An old conversation row cannot become this process's model merely
        // because the unchanged tail is served from memory on a later tick.
        assert_eq!(read(&mut cache, Some(start), start), TailFacts::default());
        assert_eq!(read(&mut cache, Some(start), start), TailFacts::default());
        assert_eq!(cache.reads, 1, "unchanged tail is read once");

        let newer = format!("{}\n", assistant_row("claude-opus-6", during));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(newer.as_bytes())
            .unwrap();
        assert_eq!(
            read(&mut cache, Some(start), start).model.shown(),
            Some("Opus 6")
        );
        assert_eq!(cache.reads, 2, "append invalidates the tail");

        let whole = format!("{old}{}\n", assistant_row("claude-opus-7", during));
        assert_eq!(whole.len(), old.len() + newer.len());
        let old_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, &whole).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_modified))
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            old_modified
        );
        assert_eq!(
            read(&mut cache, Some(start), start).model.shown(),
            Some("Opus 7")
        );
        assert_eq!(
            cache.reads, 3,
            "same-size, same-mtime rewrite invalidates the tail"
        );

        let replacement = root.join("replacement.jsonl");
        std::fs::write(
            &replacement,
            format!("{old}{}\n", assistant_row("claude-opus-8", during)),
        )
        .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_modified))
            .unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(
            read(&mut cache, Some(start), start).model.shown(),
            Some("Opus 8")
        );
        assert_eq!(
            cache.reads, 4,
            "same-size, same-mtime replacement invalidates"
        );

        // A process restart on the identical transcript path must move the
        // floor even before the new process has written a row of its own.
        let new_start = crate::harness::upgrade_models::parse_utc("2026-09-27T14:00:00Z").unwrap();
        assert_eq!(
            read(&mut cache, Some(new_start), new_start),
            TailFacts::default()
        );
        assert_eq!(cache.reads, 5, "process start changes the key");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{}\n", assistant_row("claude-opus-9", after)).as_bytes())
            .unwrap();
        assert_eq!(
            read(&mut cache, Some(new_start), new_start).model.shown(),
            Some("Opus 9")
        );
        assert_eq!(cache.reads, 6);
        assert_eq!(cache.appends, 2, "both appends were read as appends");

        std::fs::remove_file(&path).unwrap();
        assert!(
            cache
                .read(&path, 4242, Some(new_start), new_start)
                .is_none()
        );
        assert!(cache.entry.is_none(), "failed opens must never be cached");
        std::fs::write(
            &path,
            format!("{}\n", assistant_row("claude-opus-8", after)),
        )
        .unwrap();
        assert_eq!(
            read(&mut cache, Some(new_start), new_start).model.shown(),
            Some("Opus 8")
        );
        assert_eq!(cache.reads, 7, "recovered file is read anew");
        assert_eq!(cache.appends, 2, "a recreated file is no append");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cached_tail_does_not_freeze_live_repository_facts() {
        let root = cache_test_dir("live-head");
        let cwd = root.join("work");
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        std::fs::write(cwd.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let claude = root.join("claude");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        let started = crate::harness::upgrade_models::parse_utc("2026-09-27T12:00:00Z").unwrap();
        std::fs::write(
            claude.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"cache-demo","cwd":"{}","procStart":"{}"}}"#,
                cwd.display(),
                lstart_utc(started)
            ),
        )
        .unwrap();
        std::fs::write(
            project.join("cache-demo.jsonl"),
            format!(
                "{}\n",
                assistant_row("claude-opus-5", "2026-09-27T13:00:00Z")
            ),
        )
        .unwrap();
        let mut cache = TailCache::default();
        let first = facts_for_pid_cached(&claude, 4242, Some(started), &mut cache).unwrap();
        assert_eq!(first.model.as_deref(), Some("Opus 5"));
        assert_eq!(first.branch.as_deref(), Some("main"));
        assert_eq!(cache.reads, 1);

        std::fs::write(cwd.join(".git/HEAD"), "ref: refs/heads/topic\n").unwrap();
        let second = facts_for_pid_cached(&claude, 4242, Some(started), &mut cache).unwrap();
        assert_eq!(second.model, first.model);
        assert_eq!(second.branch.as_deref(), Some("topic"));
        assert_eq!(
            cache.reads, 1,
            "git still re-reads while transcript does not"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_ids_read_as_the_vendor_spells_them() {
        assert_eq!(model_display("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_display("claude-opus-5"), "Opus 5");
        assert_eq!(model_display("claude-fable-5-1"), "Fable 5.1");
        assert_eq!(model_display("claude-sonnet-5"), "Sonnet 5");
        assert_eq!(model_display("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_display("claude-opus-5-5[1m]"), "Opus 5.5");
        assert_eq!(model_display("gpt-5-codex"), "gpt-5-codex");
    }

    #[test]
    fn a_registry_entry_must_name_the_pid_it_is_filed_under() {
        let text =
            r#"{"pid":7214,"sessionId":"28b6a7bd-7021","cwd":"/Users//ana/aterm","status":"busy"}"#;
        assert_eq!(
            parse_session_entry(text, 7214, None),
            Some(SessionEntry {
                session_id: "28b6a7bd-7021".into(),
                cwd: PathBuf::from("/Users//ana/aterm"),
                version: None,
                proc_start: None,
                started_at: None,
            })
        );
        assert_eq!(
            parse_session_entry(text, 9999, None),
            None,
            "a stale row left by a reused pid is not this process's"
        );
        let hostile = r#"{"pid":7,"sessionId":"../../etc/passwd","cwd":"/"}"#;
        assert_eq!(
            parse_session_entry(hostile, 7, None),
            None,
            "the id becomes a file name"
        );
    }

    #[test]
    fn the_slug_matches_claude_codes_project_directories() {
        assert_eq!(project_slug(Path::new("/home/dev/proj")), "-home-dev-proj");
        assert_eq!(project_slug(Path::new("/tmp/a.b c")), "-tmp-a-b-c");
    }

    #[test]
    fn the_tail_takes_the_newest_main_thread_model_and_effort() {
        let body = concat!(
            r#"{"type":"assistant","effort":"high","message":{"model":"claude-opus-5"}}"#,
            "\n",
            r#"{"type":"assistant","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"effort":"low","message":{"model":"claude-haiku-4-5"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#,
            "\n",
            r#"{"type":"user","effort":"xhigh"}"#,
            "\n",
        );
        let facts = tail_facts(body.as_bytes(), None);
        assert_eq!(facts.model.shown(), Some("Opus 5.5"));
        assert_eq!(facts.effort.shown(), Some("xhigh"));
        assert_eq!(
            tail_facts(b"", None),
            TailFacts::default(),
            "a fresh session knows neither"
        );
    }

    /// A `--resume`d process appends to its predecessors' transcript: the
    /// structure of the 2.1.278 → 2.1.280 boundary of a real conversation
    /// (an Opus 5 answer, the new process's start, then its Opus 5.5 answer).
    /// Before it has said anything, the footer shows no model of its
    /// predecessor — and a model result it writes names its own at once.
    #[test]
    fn a_resumed_process_never_shows_its_predecessors_model() {
        let old = r#"{"type":"assistant","timestamp":"2026-09-22T16:44:57.101Z","effort":"high","message":{"model":"claude-opus-5"}}"#;
        let own = r#"{"type":"assistant","timestamp":"2026-09-24T04:15:32.412Z","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z").unwrap();
        let before = format!("{old}\n");
        assert_eq!(
            tail_facts(before.as_bytes(), Some(start)),
            TailFacts::default(),
            "the previous process's answer does not speak for this one"
        );
        // Negative control: without the floor, the stale model is read.
        assert_eq!(
            tail_facts(before.as_bytes(), None).model.shown(),
            Some("Opus 5")
        );
        assert_eq!(
            tail_facts(before.as_bytes(), Some(start - 3 * 86_400))
                .model
                .shown(),
            Some("Opus 5"),
            "a row written after the start is this process's"
        );
        let after = format!("{old}\n{own}\n");
        let facts = tail_facts(after.as_bytes(), Some(start));
        assert_eq!(facts.model.shown(), Some("Opus 5.5"));
        assert_eq!(facts.effort.shown(), Some("xhigh"));
        // A choice this process made, before it answered: shown at once.
        let chose = r#"{"type":"user","timestamp":"2026-09-24T04:14:00.000Z","message":{"role":"user","content":"<local-command-stdout>Set model to `Fable 5.1` for this session only</local-command-stdout>"}}"#;
        let facts = tail_facts(format!("{old}\n{chose}\n").as_bytes(), Some(start));
        assert_eq!(facts.model.shown(), Some("Fable 5.1"));
        assert_eq!(
            facts.effort,
            Said::Unsaid,
            "a switch that names no effort moved none: the effort is what it was before \
             (nothing since the floor says it — the launch card may)"
        );
    }

    /// A `/model` COMMAND row whose result this build cannot read: the model
    /// (and the effort, which a model switch may move) is unknown until
    /// something newer names it — never the answer before the choice. A
    /// result it CAN read names the model at once, and the effort before it
    /// stands, when the result names none, only for the same model.
    #[test]
    fn an_unreadable_model_choice_voids_model_and_effort() {
        let answer = r#"{"type":"assistant","timestamp":"2026-09-24T04:13:30.000Z","effort":"xhigh","message":{"model":"claude-opus-5"}}"#;
        let command = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}"#;
        let unread = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<local-command-stdout>Model switch to X was blocked by a PreModelSwitch hook</local-command-stdout>"}}"#;
        let result = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<local-command-stdout>Set model to `Opus 5.5 (default)` and saved as your default for new sessions</local-command-stdout>"}}"#;
        let next = r#"{"type":"assistant","timestamp":"2026-09-24T04:15:32.000Z","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
        let void = TailFacts {
            model: Said::Unread,
            effort: Said::Unread,
            ..TailFacts::default()
        };
        assert_eq!(
            tail_facts(format!("{answer}\n{command}\n").as_bytes(), start),
            void
        );
        assert_eq!(
            tail_facts(format!("{answer}\n{command}\n{unread}\n").as_bytes(), start),
            void,
            "a result this build does not read is no model"
        );
        // Negative control: the same tail without the command row.
        assert_eq!(
            tail_facts(format!("{answer}\n").as_bytes(), start)
                .model
                .shown(),
            Some("Opus 5")
        );
        let chosen = tail_facts(format!("{answer}\n{command}\n{result}\n").as_bytes(), start);
        assert_eq!(
            chosen.model.shown(),
            Some("Opus 5.5"),
            "the result names the choice"
        );
        assert_eq!(
            chosen.effort,
            Said::Unread,
            "no `with … effort` clause, and the answer's xhigh was Opus 5's: \
             Opus 5.5 runs its own"
        );
        // The same switch from an answer of the model it names keeps it.
        let same = answer.replace("claude-opus-5\"", "claude-opus-5-5\"");
        let kept = tail_facts(format!("{same}\n{command}\n{result}\n").as_bytes(), start);
        assert_eq!(
            (kept.model.shown(), kept.effort.shown()),
            (Some("Opus 5.5"), Some("xhigh"))
        );
        // An unreadable result under the same command voids the effort too:
        // whether the picker moved it cannot be told.
        assert_eq!(
            tail_facts(format!("{answer}\n{command}\n{unread}\n").as_bytes(), start).effort,
            Said::Unread
        );
        assert_eq!(
            tail_facts(format!("{answer}\n{command}\n{next}\n").as_bytes(), start)
                .model
                .shown(),
            Some("Opus 5.5"),
            "the answer after the choice names the new model"
        );
        // The tag quoted anywhere but at the start of a person's row is text.
        for quoted in [
            r#"{"type":"assistant","timestamp":"2026-09-24T04:13:31.000Z","message":{"model":"<synthetic>","content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":[{"type":"tool_result","content":"<command-name>/model</command-name>"}]}}"#,
            r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"why does <command-name>/model</command-name> exist"}}"#,
            r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"it printed <local-command-stdout>Set model to `Haiku 4.5`</local-command-stdout>"}}"#,
            r#"{"type":"assistant","timestamp":"2026-09-24T04:13:31.000Z","message":{"model":"<synthetic>","content":"<local-command-stdout>Set model to `Haiku 4.5`</local-command-stdout>"}}"#,
        ] {
            assert_eq!(
                tail_facts(format!("{answer}\n{quoted}\n").as_bytes(), start)
                    .model
                    .shown(),
                Some("Opus 5"),
                "{quoted}"
            );
        }
    }

    #[test]
    fn an_unstamped_row_is_not_attributed() {
        let unstamped =
            r#"{"type":"assistant","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
        let stamped = r#"{"type":"assistant","timestamp":"2026-09-24T04:15:32.000Z","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
        assert_eq!(
            tail_facts(format!("{unstamped}\n").as_bytes(), start),
            TailFacts::default()
        );
        assert_eq!(
            tail_facts(format!("{stamped}\n").as_bytes(), start)
                .model
                .shown(),
            Some("Opus 5.5")
        );
    }

    /// THE OWNER'S OWN ROWS (2026-09-28, 2.1.283, the session the owner
    /// reported this on; its paths and ids replaced by stand-ins): a
    /// `/model` 11 s after the start and an `/effort` 11 s later, then — 14
    /// minutes on — a prompt with a pasted image and the first answer. The
    /// footer showed no model for those 14 minutes; now it names `Opus 5.5`
    /// the moment the choice is made, and `xhigh` the moment the effort is.
    /// NEGATIVE CONTROL: the `/model` command row without its result voids.
    #[test]
    fn the_owners_model_choice_names_the_model_at_once() {
        const SYSTEM: &str = r#"{"parentUuid":null,"isSidechain":false,"type":"system","subtype":"informational","content":"agents-md: no CLAUDE.md found; AGENTS.md loaded: /Users//person/aterm/AGENTS.md","isMeta":false,"timestamp":"2026-09-28T15:04:08.823Z","uuid":"00000000-0000-4000-8000-000000000010","level":"notice","userType":"external","entrypoint":"cli","cwd":"/Users//person/aterm","sessionId":"00000000-0000-4000-8000-000000000002","version":"2.1.283","gitBranch":"main"}"#;
        const MODEL_CAVEAT: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000010","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000020","type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>"},"isMeta":true,"uuid":"00000000-0000-4000-8000-000000000011","timestamp":"2026-09-28T15:04:17.798Z","userType":"external","entrypoint":"cli","cwd":"/Users//person/aterm","sessionId":"00000000-0000-4000-8000-000000000002","version":"2.1.283","gitBranch":"main"}"#;
        const MODEL_COMMAND_ROW: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000011","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000020","type":"user","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"},"uuid":"00000000-0000-4000-8000-000000000012","timestamp":"2026-09-28T15:04:17.798Z","userType":"external","entrypoint":"cli","cwd":"/Users//person/aterm","sessionId":"00000000-0000-4000-8000-000000000002","version":"2.1.283","gitBranch":"main"}"#;
        const MODEL_RESULT: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000012","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000020","type":"user","message":{"role":"user","content":"<local-command-stdout>Set model to `Opus 5.5 (default)` and saved as your default for new sessions</local-command-stdout>"},"uuid":"00000000-0000-4000-8000-000000000013","timestamp":"2026-09-28T15:04:17.798Z","userType":"external","entrypoint":"cli","cwd":"/Users//person/aterm","sessionId":"00000000-0000-4000-8000-000000000002","version":"2.1.283","gitBranch":"main"}"#;
        const EFFORT_COMMAND_ROW: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000014","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000021","type":"user","message":{"role":"user","content":"<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args></command-args>"},"uuid":"00000000-0000-4000-8000-000000000015","timestamp":"2026-09-28T15:04:28.429Z","userType":"external","entrypoint":"cli","cwd":"/Users//person/aterm","sessionId":"00000000-0000-4000-8000-000000000002","version":"2.1.283","gitBranch":"main"}"#;
        // The effort is named by its top-effort mode, filled in below from
        // [`CLAUDE_TOP_EFFORT_KEY`] (the export guard bans the joined word).
        const EFFORT_RESULT: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000015","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000021","type":"user","message":{"role":"user","content":"<local-command-stdout>Set effort level to {TOP} (this session only): xhigh + dynamic workflow orchestration</local-command-stdout>"},"uuid":"00000000-0000-4000-8000-000000000016","timestamp":"2026-09-28T15:04:28.429Z","userType":"external","entrypoint":"cli","cwd":"/Users//person/aterm","sessionId":"00000000-0000-4000-8000-000000000002","version":"2.1.283","gitBranch":"main"}"#;
        const ANSWER: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000017","isSidechain":false,"type":"assistant","effort":"xhigh","perTurnEffort":"xhigh","message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"On it."}]},"uuid":"00000000-0000-4000-8000-000000000018","timestamp":"2026-09-28T15:18:16.595Z","userType":"external","entrypoint":"cli","cwd":"/Users//person/aterm","sessionId":"00000000-0000-4000-8000-000000000002","version":"2.1.283","gitBranch":"main"}"#;
        let effort_result = EFFORT_RESULT.replace("{TOP}", CLAUDE_TOP_EFFORT_KEY);
        // `startedAt` 1790607848684 (ms): the floor, 15:04:08Z.
        let since = Some(1_790_607_848);
        let rows = |rows: &[&str]| rows.iter().map(|r| format!("{r}\n")).collect::<String>();
        let fresh = rows(&[SYSTEM]);
        assert_eq!(tail_facts(fresh.as_bytes(), since), TailFacts::default());
        let chosen = rows(&[SYSTEM, MODEL_CAVEAT, MODEL_COMMAND_ROW, MODEL_RESULT]);
        let facts = tail_facts(chosen.as_bytes(), since);
        assert_eq!(facts.model.shown(), Some("Opus 5.5"), "at 15:04:17");
        assert_eq!(
            facts.effort,
            Said::Unsaid,
            "the switch moved no effort (no `with … effort` clause): the launch card's \
             `xhigh` stays on show, filled by the host"
        );
        let effort = rows(&[
            SYSTEM,
            MODEL_CAVEAT,
            MODEL_COMMAND_ROW,
            MODEL_RESULT,
            MODEL_CAVEAT,
            EFFORT_COMMAND_ROW,
            effort_result.as_str(),
        ]);
        let facts = tail_facts(effort.as_bytes(), since);
        assert_eq!(facts.model.shown(), Some("Opus 5.5"));
        assert_eq!(facts.effort.shown(), Some("xhigh"), "at 15:04:28");
        let answered = format!("{effort}{ANSWER}\n");
        let facts = tail_facts(answered.as_bytes(), since);
        assert_eq!(
            (facts.model.shown(), facts.effort.shown()),
            (Some("Opus 5.5"), Some("xhigh")),
            "the answer at 15:18:16 agrees"
        );
        // NEGATIVE CONTROL: the command row with no result after it.
        let cut = rows(&[SYSTEM, MODEL_CAVEAT, MODEL_COMMAND_ROW]);
        assert_eq!(tail_facts(cut.as_bytes(), since).model, Said::Unread);
    }

    /// Claude's model RESULTS, as each build spells them, are the vendor's
    /// name — `(default)` and `(1M context)` off, as [`model_display`] drops
    /// `[1m]`; the older ANSI-bold result names an id. Anything that is not
    /// plainly a name is no model, and never reaches the glass.
    #[test]
    fn a_model_result_display_is_the_vendors_name() {
        let result = |text: &str| {
            let row = format!(
                r#"{{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{{"content":{}}}}}"#,
                aterm_json::to_string(&Value::String(format!(
                    "<local-command-stdout>{text}</local-command-stdout>"
                )))
                .unwrap()
            );
            tail_facts(format!("{row}\n").as_bytes(), None)
        };
        for (text, model) in [
            (
                "Set model to `Opus 5.5 (default)` and saved as your default for new sessions",
                "Opus 5.5",
            ),
            (
                "Set model to `Opus 5 (1M context) (default)` for this session only",
                "Opus 5",
            ),
            ("Set model to `Fable 5.1`", "Fable 5.1"),
            ("Set model to `Fable 5.1 (1M context)`", "Fable 5.1"),
            (
                "Set model to \u{1b}[1mclaude-fable-5-1\u{1b}[22m and saved as your default",
                "Fable 5.1",
            ),
            (
                "\u{21AF} Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok",
                "Opus 5.5",
            ),
            // As Claude writes it (2.1.283: `${Zoe(!0)} ${mrt}${grt}…`, the
            // icon in its theme's fast-mode colour): the classifier strips the
            // colour first, as the vendor's own `nnr` does.
            (FAST_ON_COLOURED, "Opus 5.5"),
        ] {
            assert_eq!(result(text).model.shown(), Some(model), "{text:?}");
        }
        // NEGATIVE CONTROL: the gate before the strip (the phrase at most 8
        // bytes in, no letter or digit before it) misses the coloured row —
        // the colour sequence puts it 26 bytes in, digits first.
        let body = FAST_ON_COLOURED;
        let at = body.find(aterm_phase::anchor("fast.on")).unwrap();
        assert!(at > 8 && body[..at].chars().any(char::is_alphanumeric));
        let with_effort = result(&format!(
            "Set model to `Opus 5.5` and saved as your default for new sessions with `{CLAUDE_TOP_EFFORT_KEY}` effort ({CLAUDE_TOP_EFFORT_KEY} applies to this session only)",
        ));
        assert_eq!(
            (with_effort.model.shown(), with_effort.effort.shown()),
            (Some("Opus 5.5"), Some("xhigh"))
        );
        // Hostile or unreadable: never a model.
        for text in [
            "Set model to `Opus\u{7}5`",
            "Set model to `Opus\n5`",
            "Set model to `A very long name that no model of anyone's has ever carried at all`",
            "Set model to ``Opus``",
            "Set model to Opus 5.5",
        ] {
            assert_eq!(result(text).model, Said::Unread, "{text:?}");
        }
        // Not a model result at all: says nothing about the model.
        for text in [
            "Cloud session couldn't switch to `Opus 5.5`",
            "Model switch to X was blocked by a PreModelSwitch hook",
            "Current model: `Opus 5.5`",
            "Kept Fast mode ON \u{00B7} model set to `Opus 5.5`",
            "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Kept Fast mode ON \u{00B7} model set to `Opus 5.5`",
            "Fast mode ON",
            // `/fast on` for a model that is already fast: no `model set to`.
            "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} $30/$150 per Mtok",
            "Fast mode OFF",
        ] {
            assert_eq!(result(text).model, Said::Unsaid, "{text:?}");
        }
    }

    /// A `/fast on` that MOVED the model, as Claude 2.1.283 writes it: the
    /// `↯` icon in its theme's fast-mode colour (a truecolor SGR), then the
    /// phrase and the model it moved to.
    const FAST_ON_COLOURED: &str = "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok";

    /// `/fast` names the model only when it MOVED it. A `/fast` whose result
    /// carries no `model set to` clause — fast mode on for a model that is
    /// already fast, fast mode off, a refusal — voids nothing: Claude moves
    /// the model under `/fast` exactly when it appends that clause (2.1.283:
    /// `willPromote` gates both), so the model and the effort before it
    /// stand. A move names no effort, so the one before it, read for another
    /// model, is unknown. (A `/model` command with no readable result is
    /// different: every one is a choice.)
    #[test]
    fn a_fast_result_names_the_model_only_when_it_moved_it() {
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
        let answer = r#"{"type":"assistant","timestamp":"2026-09-24T04:13:30.000Z","effort":"high","message":{"model":"claude-fable-5-1"}}"#;
        let command = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<command-name>/fast</command-name>\n            <command-message>fast</command-message>\n            <command-args>on</command-args>"}}"#;
        let result = |text: &str| {
            format!(
                r#"{{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{{"role":"user","content":{}}}}}"#,
                aterm_json::to_string(&Value::String(format!(
                    "<local-command-stdout>{text}</local-command-stdout>"
                )))
                .unwrap()
            )
        };
        let moved = tail_facts(
            format!("{answer}\n{command}\n{}\n", result(FAST_ON_COLOURED)).as_bytes(),
            start,
        );
        assert_eq!(
            (moved.model.shown(), moved.effort),
            (Some("Opus 5.5"), Said::Unread),
            "the move names the model at once; the effort, read for Fable 5.1, is unknown \
             for the model it moved to"
        );
        for kept in [
            "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} $30/$150 per Mtok",
            "Fast mode OFF",
            "Fast mode unavailable: not on this plan",
        ] {
            let facts = tail_facts(
                format!("{answer}\n{command}\n{}\n", result(kept)).as_bytes(),
                start,
            );
            assert_eq!(
                (facts.model.shown(), facts.effort.shown()),
                (Some("Fable 5.1"), Some("high")),
                "{kept:?}"
            );
        }
    }

    /// A `/model` picker that changed nothing, as Claude writes it (MEASURED:
    /// a 2.1.278 transcript's two rows, its ids replaced by stand-ins): the
    /// command and its `Kept model as` result are `type:system`,
    /// `subtype:local_command` rows with a top-level `content`. They decide
    /// nothing: the model and the effort are what the rows before them said.
    /// NEGATIVE CONTROL: the same command as the `type:user` row a choice
    /// writes, with no readable result, voids both.
    #[test]
    fn a_kept_model_leaves_the_facts_as_they_were() {
        const ANSWER: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000030","isSidechain":false,"type":"assistant","effort":"xhigh","message":{"model":"claude-fable-5-1","role":"assistant","content":[{"type":"text","text":"Done."}]},"uuid":"00000000-0000-4000-8000-000000000031","timestamp":"2026-09-22T17:05:50.000Z","userType":"external","entrypoint":"cli","cwd":"/Users//person/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
        const KEPT_COMMAND: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000031","isSidechain":false,"type":"system","subtype":"local_command","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>","level":"info","timestamp":"2026-09-22T17:06:02.248Z","uuid":"00000000-0000-4000-8000-000000000032","isMeta":false,"userType":"external","entrypoint":"cli","cwd":"/Users//person/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
        const KEPT_RESULT: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000032","isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout>Kept model as `Fable 5.1`</local-command-stdout>","level":"info","timestamp":"2026-09-22T17:06:02.248Z","uuid":"00000000-0000-4000-8000-000000000033","isMeta":false,"commandRun":{"command":"model","args":""},"userType":"external","entrypoint":"cli","cwd":"/Users//person/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
        let start = crate::harness::upgrade_models::parse_utc("2026-09-22T17:00:00Z");
        let before = tail_facts(format!("{ANSWER}\n").as_bytes(), start);
        let kept = tail_facts(
            format!("{ANSWER}\n{KEPT_COMMAND}\n{KEPT_RESULT}\n").as_bytes(),
            start,
        );
        assert_eq!(kept, before, "a kept model changes nothing");
        assert_eq!(
            (kept.model.shown(), kept.effort.shown()),
            (Some("Fable 5.1"), Some("xhigh"))
        );
        // NEGATIVE CONTROL: a choice's command row (`type:user`) with no
        // readable result under it is a choice this build cannot read.
        let choice = KEPT_COMMAND.replace(
            r#""type":"system","subtype":"local_command","content":"#,
            r#""type":"user","message":{"role":"user","content":"#,
        );
        let choice = choice.replacen(r#"","level""#, r#""},"level""#, 1);
        let voided = tail_facts(format!("{ANSWER}\n{choice}\n").as_bytes(), start);
        assert_eq!(
            (voided.model, voided.effort),
            (Said::Unread, Said::Unread),
            "{choice}"
        );
    }

    /// A transcript row of an ANSWER by `model`, stamped `at`, with its
    /// `effort` when the model has one.
    fn answer_at(model: &str, at: &str, effort: Option<&str>) -> String {
        let effort = effort.map_or(String::new(), |e| format!(r#","effort":"{e}""#));
        format!(
            r#"{{"type":"assistant","timestamp":"{at}"{effort},"message":{{"model":"{model}"}}}}"#
        )
    }

    /// The `type:user` row Claude writes for the local command `/<name>`.
    fn command_at(name: &str, at: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"<command-name>/{name}</command-name>\n            <command-message>{name}</command-message>\n            <command-args></command-args>"}}}}"#
        )
    }

    /// The `type:user` RESULT row of a local command, saying `text`.
    fn result_at(at: &str, text: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":{}}}}}"#,
            aterm_json::to_string(&Value::String(format!(
                "<local-command-stdout>{text}</local-command-stdout>"
            )))
            .unwrap()
        )
    }

    /// `rows`, one per line.
    fn body(rows: &[&str]) -> String {
        rows.iter().map(|r| format!("{r}\n")).collect()
    }

    /// THE EFFORT ACROSS A SWITCH (review of 2026-09-28): a model result
    /// with no ` with … effort` clause names its model and no effort, and
    /// Claude runs each model at its own effort (2.1.283: `xhigh` and `max`
    /// fall to `high` on a model without them, a model with no levels runs
    /// none — Haiku 4.5's answers carry no `effort` — and otherwise the
    /// model's own settings or default). So the effort from before the
    /// switch stands only when it was read for the model switched to — the
    /// model the rows above name. NEGATIVE CONTROLS: a switch to that same
    /// model keeps it, and a clause names it.
    #[test]
    fn a_switch_that_names_no_effort_keeps_it_only_for_the_same_model() {
        let start = crate::harness::upgrade_models::parse_utc("2026-09-28T15:00:00Z");
        let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
        let switch = |at: &str, to: &str| {
            [
                command_at("model", at),
                result_at(at, &format!("Set model to `{to}` for this session only")),
            ]
        };
        let effort_high = |at: &str| {
            [
                command_at("effort", at),
                result_at(
                    at,
                    "Set effort level to high (this session only): Deeper reasoning",
                ),
            ]
        };
        let facts = |rows: &[&str]| {
            let facts = tail_facts(body(rows).as_bytes(), start);
            (facts.model.shown().map(str::to_owned), facts.effort)
        };
        let is = |model: &str, effort: Option<&str>| {
            (
                Some(model.to_owned()),
                effort.map_or(Said::Unread, |e| Said::Is(e.to_owned())),
            )
        };
        // The reviewer's probe: Opus 5.5 at xhigh, then Haiku 4.5 — which has
        // no effort levels — and then a Haiku answer, which names none.
        let [c, r] = switch("2026-09-28T15:02:00.000Z", "Haiku 4.5");
        let haiku = answer_at(
            "claude-haiku-4-5-20251001",
            "2026-09-28T15:03:00.000Z",
            None,
        );
        assert_eq!(facts(&[&opus, &c, &r]), is("Haiku 4.5", None));
        assert_eq!(
            facts(&[&opus, &c, &r, &haiku]),
            is("Haiku 4.5", None),
            "never Haiku 4.5 xhigh, after any number of Haiku answers"
        );
        // NEGATIVE CONTROL: a switch to the model the effort was read for.
        let [c, r] = switch("2026-09-28T15:02:00.000Z", "Opus 5.5 (default)");
        assert_eq!(facts(&[&opus, &c, &r]), is("Opus 5.5", Some("xhigh")));
        // Through two switches: Sonnet 5 ran between, so a switch back to
        // Opus 5.5 does not bring Opus 5.5's xhigh back.
        let [c1, r1] = switch("2026-09-28T15:02:00.000Z", "Sonnet 5");
        let [c2, r2] = switch("2026-09-28T15:02:30.000Z", "Opus 5.5");
        assert_eq!(facts(&[&opus, &c1, &r1, &c2, &r2]), is("Opus 5.5", None));
        // An `/effort` after the switch is the new model's own.
        let [e1, e2] = effort_high("2026-09-28T15:02:10.000Z");
        assert_eq!(
            facts(&[&opus, &c1, &r1, &e1, &e2]),
            is("Sonnet 5", Some("high"))
        );
        // An `/effort` BEFORE the switch was set on the model then running:
        // kept only when that is the model switched to.
        let [e1, e2] = effort_high("2026-09-28T15:01:30.000Z");
        assert_eq!(facts(&[&opus, &e1, &e2, &c1, &r1]), is("Sonnet 5", None));
        let [c, r] = switch("2026-09-28T15:02:00.000Z", "Opus 5.5");
        assert_eq!(
            facts(&[&opus, &e1, &e2, &c, &r]),
            is("Opus 5.5", Some("high"))
        );
        // A result that names the effort names it, whatever ran before.
        let clause = result_at(
            "2026-09-28T15:02:00.000Z",
            "Set model to `Sonnet 5` for this session only with `high` effort",
        );
        let c = command_at("model", "2026-09-28T15:02:00.000Z");
        assert_eq!(facts(&[&opus, &c, &clause]), is("Sonnet 5", Some("high")));
        // With nothing since the floor naming the model before the switch,
        // the switch leaves the question to the launch model (the flag's or
        // the card's: `facts_for_entry_cached`).
        let [c, r] = switch("2026-09-28T15:02:00.000Z", "Sonnet 5");
        let alone = tail_facts(body(&[&c, &r]).as_bytes(), start);
        assert_eq!(
            (alone.effort, alone.effort_for.as_deref()),
            (Said::Unsaid, Some("Sonnet 5"))
        );
    }

    /// A COMMAND PAIRS ONLY WITH THE RESULT DIRECTLY UNDER IT (review of
    /// 2026-09-28): a readable `/fast` move under its own `/fast` command
    /// says nothing about an older `/model` choice whose result could not be
    /// read, or that has none — that choice may have moved the effort, so
    /// the effort before it is unknown. NEGATIVE CONTROL: the same `/fast`
    /// move without the unread choice keeps the effort, since it moved to the
    /// model the effort was read for.
    #[test]
    fn a_command_pairs_only_with_the_result_directly_under_it() {
        let start = crate::harness::upgrade_models::parse_utc("2026-09-28T15:00:00Z");
        let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
        let model_cmd = command_at("model", "2026-09-28T15:02:00.000Z");
        let unread = result_at("2026-09-28T15:02:00.000Z", "Set model to Fable 5.1");
        let fast_cmd = command_at("fast", "2026-09-28T15:02:30.000Z");
        let fast = result_at("2026-09-28T15:02:30.000Z", FAST_ON_COLOURED);
        let facts = |rows: &[&str]| {
            let facts = tail_facts(body(rows).as_bytes(), start);
            (facts.model, facts.effort)
        };
        // 3a: the unreadable choice alone voids both.
        assert_eq!(
            facts(&[&opus, &model_cmd, &unread]),
            (Said::Unread, Said::Unread)
        );
        // 3b: the reviewer's probe — a `/fast` move after it names the model,
        // and the effort stays unknown.
        assert_eq!(
            facts(&[&opus, &model_cmd, &unread, &fast_cmd, &fast]),
            (Said::Is("Opus 5.5".into()), Said::Unread)
        );
        // 3c: a `/model` command with NO result under it, then the move: the
        // move's result is its own command's, not the `/model` one's.
        assert_eq!(
            facts(&[&opus, &model_cmd, &fast_cmd, &fast]),
            (Said::Is("Opus 5.5".into()), Said::Unread)
        );
        // NEGATIVE CONTROL: the move alone, to the model the xhigh was read
        // for, keeps it — what voids in 3c is the unpaired `/model`.
        assert_eq!(
            facts(&[&opus, &fast_cmd, &fast]),
            (Said::Is("Opus 5.5".into()), Said::Is("xhigh".into()))
        );
    }

    /// One Claude Code process on disk, for the resolver's tests: its
    /// registry file and its sessions' transcripts, and the wall clock the
    /// resolver reads them at — its start until a test moves it ([`Self::at`]):
    /// a switch then came after the process's floor, and nothing more is known.
    #[cfg(unix)]
    struct Proc {
        root: PathBuf,
        claude: PathBuf,
        project: PathBuf,
        cwd: PathBuf,
        start: u64,
        clock: std::cell::Cell<u64>,
    }

    #[cfg(unix)]
    impl Proc {
        fn new(tag: &str, start: &str) -> Self {
            let root = cache_test_dir(tag);
            let cwd = root.join("work");
            let claude = root.join("claude");
            std::fs::create_dir_all(&cwd).unwrap();
            std::fs::create_dir_all(claude.join("sessions")).unwrap();
            let project = claude.join("projects").join(project_slug(&cwd));
            std::fs::create_dir_all(&project).unwrap();
            let start = crate::harness::upgrade_models::parse_utc(start).unwrap();
            Self {
                root,
                claude,
                project,
                cwd,
                start,
                clock: std::cell::Cell::new(start),
            }
        }

        /// The resolver's next reads happen at `stamp`.
        fn at(&self, stamp: &str) {
            self.clock
                .set(crate::harness::upgrade_models::parse_utc(stamp).unwrap());
        }

        /// The registry names `session` for the process — the same pid,
        /// `procStart` and `startedAt` whatever the session.
        fn register(&self, session: &str) {
            std::fs::write(
                self.claude.join("sessions/4242.json"),
                format!(
                    r#"{{"pid":4242,"sessionId":"{session}","cwd":"{}","procStart":"{}","startedAt":{},"version":"2.1.283"}}"#,
                    self.cwd.display(),
                    lstart_utc(self.start),
                    self.start * 1000 + 684
                ),
            )
            .unwrap();
        }

        fn write(&self, session: &str, rows: &[&str]) {
            std::fs::write(self.project.join(format!("{session}.jsonl")), body(rows)).unwrap();
        }

        fn read(&self, launch: &LaunchFacts, cache: &mut TailCache) -> FooterFacts {
            let entry = session_of_pid(&self.claude, 4242, Some(self.start)).expect("registry");
            facts_for_entry_at(
                &self.claude,
                4242,
                Some(self.start),
                &entry,
                launch,
                cache,
                UNIX_EPOCH + std::time::Duration::from_secs(self.clock.get()),
            )
        }
    }

    #[cfg(unix)]
    impl Drop for Proc {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// THE MEASURED LIVE CASE (2026-09-28, 2.1.283): a session launched
    /// with `--model claude-haiku-4-5-20251001` switched to Sonnet 5 with
    /// `/model`, the result naming no effort. Claude showed `● high ·
    /// /effort` — Sonnet 5's own — while nothing since the floor named an
    /// effort: the footer shows none until an answer or `/effort` names
    /// Sonnet 5's, and never Haiku 4.5's (none) or a card's for another
    /// model. NEGATIVE CONTROLS: launched on the model switched to, the
    /// effort stays open for the card, and an `/effort` read before the
    /// switch stands.
    #[cfg(unix)]
    #[test]
    fn a_switch_from_the_launch_model_names_no_effort_until_one_is_read() {
        const S: &str = "00000000-0000-4000-8000-000000000051";
        let proc = Proc::new("launch-switch", "2026-09-28T15:00:00Z");
        proc.register(S);
        let argv =
            |model: &str| launch_facts(&["claude".into(), "--model".into(), model.to_owned()]);
        let haiku = argv("claude-haiku-4-5-20251001");
        let cmd = command_at("model", "2026-09-28T15:01:00.000Z");
        let sonnet = result_at(
            "2026-09-28T15:01:00.000Z",
            "Set model to `Sonnet 5` for this session only",
        );
        proc.write(S, &[&cmd, &sonnet]);
        let facts = proc.read(&haiku, &mut TailCache::default());
        assert_eq!(
            (facts.model.as_deref(), facts.effort.as_deref()),
            (Some("Sonnet 5"), None)
        );
        assert!(
            !facts.effort_open,
            "Haiku 4.5's launch card may not fill it"
        );
        // An answer names Sonnet 5's effort…
        let answer = answer_at("claude-sonnet-5", "2026-09-28T15:02:00.000Z", Some("high"));
        proc.write(S, &[&cmd, &sonnet, &answer]);
        let facts = proc.read(&haiku, &mut TailCache::default());
        assert_eq!(
            (facts.model.as_deref(), facts.effort.as_deref()),
            (Some("Sonnet 5"), Some("high"))
        );
        // …and so does an `/effort`.
        let effort = [
            command_at("effort", "2026-09-28T15:01:30.000Z"),
            result_at(
                "2026-09-28T15:01:30.000Z",
                "Set effort level to medium (this session only): Balanced",
            ),
        ];
        proc.write(S, &[&cmd, &sonnet, &effort[0], &effort[1]]);
        assert_eq!(
            proc.read(&haiku, &mut TailCache::default())
                .effort
                .as_deref(),
            Some("medium")
        );
        // With no flag, the card speaks for the launch model — only when it
        // names the model switched to.
        proc.write(S, &[&cmd, &sonnet]);
        let open = proc.read(&LaunchFacts::default(), &mut TailCache::default());
        assert!(open.effort_open);
        assert_eq!(open.effort_for.as_deref(), Some("Sonnet 5"));
        let card = |model: &str, effort: Option<&str>| aterm_phase::LaunchCard {
            version: "2.1.283".into(),
            model: model.into(),
            effort: effort.map(str::to_owned),
        };
        assert_eq!(
            open.filled_from(Some(&card("Opus 5.5", Some("xhigh"))))
                .effort,
            None,
            "a card of another model"
        );
        assert_eq!(
            open.filled_from(Some(&card("Sonnet 5", Some("high"))))
                .effort
                .as_deref(),
            Some("high"),
            "the card the fullscreen renderer redraws for the new model"
        );
        // NEGATIVE CONTROL: launched on Sonnet 5, the switch is to itself —
        // the effort stays open for its card — and an `/effort` set before
        // it stands.
        let sonnet_flag = argv("claude-sonnet-5");
        let same = proc.read(&sonnet_flag, &mut TailCache::default());
        assert!(same.effort_open && same.effort.is_none());
        let before = [
            command_at("effort", "2026-09-28T15:00:30.000Z"),
            result_at(
                "2026-09-28T15:00:30.000Z",
                "Set effort level to low (this session only): Quick",
            ),
        ];
        proc.write(S, &[&before[0], &before[1], &cmd, &sonnet]);
        assert_eq!(
            proc.read(&sonnet_flag, &mut TailCache::default())
                .effort
                .as_deref(),
            Some("low")
        );
        assert_eq!(
            proc.read(&haiku, &mut TailCache::default()).effort,
            None,
            "set on Haiku 4.5, not Sonnet 5's"
        );
    }

    /// `/resume` IS NOT `/clear` (review of 2026-09-28): an in-REPL `/resume`
    /// switches the process to a conversation with rows from BEFORE the
    /// floor, and Claude restores that conversation's last model unless the
    /// process pinned its own — a `--model` (any spelling), an environment
    /// model pin, or a `/model` or `/fast` choice in any session it left
    /// (2.1.284: the REPL's restore returns early on the main-loop override
    /// and those variables). Unpinned, the footer reads the resumed
    /// conversation the way Claude does — its newest main-chain answer that
    /// is not meta or `<synthetic>` — with the process's effort only when it
    /// was read for that model; pinned, the process's facts carry. NEGATIVE
    /// CONTROL: the same switch into a session with no rows before the floor
    /// (a `/clear`) carries the process's model.
    #[cfg(unix)]
    #[test]
    fn a_resume_carries_the_model_only_where_the_process_pinned_it() {
        const A: &str = "00000000-0000-4000-8000-0000000000a1";
        const B: &str = "00000000-0000-4000-8000-0000000000b1";
        const R: &str = "00000000-0000-4000-8000-0000000000c1";
        let proc = Proc::new("resume", "2026-09-28T15:00:00Z");
        let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
        // The resumed conversation, written by an earlier process: a Fable
        // 5.1 answer, then a meta row and a synthetic one Claude skips.
        let old = answer_at("claude-fable-5-1", "2026-09-27T10:00:00.000Z", Some("high"));
        let meta = r#"{"type":"assistant","isMeta":true,"timestamp":"2026-09-27T10:00:01.000Z","message":{"model":"claude-haiku-4-5-20251001"}}"#;
        let synthetic = r#"{"type":"assistant","timestamp":"2026-09-27T10:00:02.000Z","message":{"model":"<synthetic>"}}"#;
        let resumed = [old.as_str(), meta, synthetic];
        let shown = |f: &FooterFacts| (f.model.clone(), f.effort.clone());
        let is = |m: &str, e: Option<&str>| (Some(m.to_owned()), e.map(str::to_owned));
        let run = |first: &[&str], launch: &LaunchFacts, into: &[&str]| {
            proc.register(A);
            proc.write(A, first);
            proc.write(R, into);
            let mut cache = TailCache::default();
            let before = proc.read(launch, &mut cache);
            proc.register(R);
            (before, proc.read(launch, &mut cache), cache)
        };
        // Unpinned: the process ran its default model, and the resume
        // restored Fable 5.1 — Opus 5.5's xhigh is not Fable 5.1's.
        let (before, after, mut cache) = run(&[&opus], &LaunchFacts::default(), &resumed);
        assert_eq!(shown(&before), is("Opus 5.5", Some("xhigh")));
        assert_eq!(
            shown(&after),
            is("Fable 5.1", None),
            "never the pre-resume model"
        );
        assert!(!after.model_open && !after.effort_open);
        // It is now the process's: a refresh keeps it, and an answer since
        // the floor outranks it.
        assert_eq!(
            shown(&proc.read(&LaunchFacts::default(), &mut cache)),
            is("Fable 5.1", None)
        );
        let own = answer_at(
            "claude-fable-5-1",
            "2026-09-28T15:05:00.000Z",
            Some("medium"),
        );
        proc.write(R, &[resumed[0], resumed[1], resumed[2], &own]);
        assert_eq!(
            shown(&proc.read(&LaunchFacts::default(), &mut cache)),
            is("Fable 5.1", Some("medium"))
        );
        // Unpinned, the same model: the effort was read for it, and stands.
        let fable = answer_at("claude-fable-5-1", "2026-09-28T15:01:00.000Z", Some("low"));
        let (_, after, _) = run(&[&fable], &LaunchFacts::default(), &resumed);
        assert_eq!(shown(&after), is("Fable 5.1", Some("low")));
        // A resumed conversation whose newest answer names no plain model:
        // no model, never an older answer's.
        let odd = answer_at("claude opus", "2026-09-27T10:00:03.000Z", Some("high"));
        let (_, after, _) = run(&[&opus], &LaunchFacts::default(), &[&old, &odd]);
        assert_eq!(shown(&after), (None, None));
        // A resumed conversation with NO answer at all: Claude restores
        // nothing, and the process keeps its model.
        let chose_there = [
            command_at("model", "2026-09-27T10:00:00.000Z"),
            result_at(
                "2026-09-27T10:00:00.000Z",
                "Set model to `Fable 5.1` for this session only",
            ),
        ];
        let (_, after, _) = run(
            &[&opus],
            &LaunchFacts::default(),
            &[&chose_there[0], &chose_there[1]],
        );
        assert_eq!(shown(&after), is("Opus 5.5", Some("xhigh")));
        // NEGATIVE CONTROL: no rows before the floor — a `/clear` — carries.
        let (_, after, _) = run(&[&opus], &LaunchFacts::default(), &[]);
        assert_eq!(shown(&after), is("Opus 5.5", Some("xhigh")));
        // PINNED by a choice: the process keeps what it chose.
        let chose = [
            command_at("model", "2026-09-28T15:00:30.000Z"),
            result_at(
                "2026-09-28T15:00:30.000Z",
                "Set model to `Opus 5.5` for this session only",
            ),
        ];
        let (_, after, _) = run(
            &[&chose[0], &chose[1], &opus],
            &LaunchFacts::default(),
            &resumed,
        );
        assert_eq!(shown(&after), is("Opus 5.5", Some("xhigh")));
        // …in a session it left EARLIER: the choice, `/clear`, then `/resume`.
        proc.register(A);
        proc.write(A, &[&chose[0], &chose[1]]);
        proc.write(R, &resumed);
        let mut cache = TailCache::default();
        proc.read(&LaunchFacts::default(), &mut cache);
        proc.register(B);
        proc.write(B, &[&opus.replace("15:01:00", "15:03:00")]);
        proc.read(&LaunchFacts::default(), &mut cache);
        proc.register(R);
        assert_eq!(
            shown(&proc.read(&LaunchFacts::default(), &mut cache)),
            is("Opus 5.5", Some("xhigh"))
        );
        // PINNED by the launch: a real id, an alias, an environment pin.
        let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        for launch in [
            launch_facts(&argv(&["claude", "--model", "claude-opus-5-5"])),
            launch_facts(&argv(&["claude", "--model", "opus"])),
            launch_facts_of(
                &argv(&["claude"]),
                &argv(&["ANTHROPIC_MODEL=claude-opus-5-5"]),
            ),
        ] {
            let (_, after, _) = run(&[&opus], &launch, &resumed);
            assert_eq!(shown(&after), is("Opus 5.5", Some("xhigh")), "{launch:?}");
        }
    }

    /// A RESTORE IS A PIN (review of 2026-09-28, round 3). Claude restores a
    /// resumed conversation's model by setting the main-loop override
    /// (2.1.284 `Cbe`: `overrideMainLoopModel`), the very pin its next
    /// restore returns early on, and no session switch clears it. So a
    /// process that restored once keeps that model at every later
    /// `/resume` — through an answer in the resumed conversation or none,
    /// and across a `/clear` — and so does the footer (the reviewer's R1).
    /// The pin is read from the resumed conversation's HEAD, so a read that
    /// already finds the process's own answer there (and stops before the
    /// rows from before the floor) still sees it. NEGATIVE CONTROLS: a
    /// resumed conversation with NO answer restores nothing and pins
    /// nothing, and a `/clear`ed session with answers is no resumed
    /// conversation — the next `/resume` restores in both.
    #[cfg(unix)]
    #[test]
    fn a_restore_pins_the_model_for_every_later_resume() {
        const S: &str = "00000000-0000-4000-8000-0000000000d1";
        const A: &str = "00000000-0000-4000-8000-0000000000d2";
        const B: &str = "00000000-0000-4000-8000-0000000000d3";
        const C: &str = "00000000-0000-4000-8000-0000000000d4";
        let proc = Proc::new("restore-pins", "2026-09-28T15:00:00Z");
        let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
        let fable_old = answer_at("claude-fable-5-1", "2026-09-27T10:00:00.000Z", Some("high"));
        let fable_own = answer_at("claude-fable-5-1", "2026-09-28T15:05:10.000Z", Some("high"));
        let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
        let prompt_old = r#"{"type":"user","timestamp":"2026-09-27T09:59:00.000Z","message":{"role":"user","content":"hi"}}"#;
        let model = |f: &FooterFacts| f.model.clone();
        let fable = Some("Fable 5.1".to_owned());
        let launch = LaunchFacts::default();
        // R1: Opus, `/resume A` (restored Fable 5.1, now pinned), an answer
        // in A, then `/resume B` — Claude keeps Fable 5.1.
        proc.register(S);
        proc.write(S, &[&opus]);
        proc.write(A, &[prompt_old, &fable_old]);
        proc.write(B, &[&sonnet_old]);
        let mut cache = TailCache::default();
        assert_eq!(
            model(&proc.read(&launch, &mut cache)).as_deref(),
            Some("Opus 5.5")
        );
        proc.register(A);
        assert_eq!(model(&proc.read(&launch, &mut cache)), fable, "restored");
        proc.write(A, &[prompt_old, &fable_old, &fable_own]);
        assert_eq!(model(&proc.read(&launch, &mut cache)), fable);
        proc.register(B);
        assert_eq!(
            model(&proc.read(&launch, &mut cache)),
            fable,
            "R1.3: pinned"
        );
        assert_eq!(
            model(&proc.read(&launch, &mut cache)),
            fable,
            "R1.4: a refresh"
        );
        // The same with no answer in A: the restored model is the process's.
        proc.register(S);
        proc.write(A, &[prompt_old, &fable_old]);
        let mut cache = TailCache::default();
        proc.read(&launch, &mut cache);
        proc.register(A);
        proc.read(&launch, &mut cache);
        proc.register(B);
        assert_eq!(model(&proc.read(&launch, &mut cache)), fable);
        // …and across a `/clear` between the two resumes.
        proc.register(S);
        let mut cache = TailCache::default();
        proc.read(&launch, &mut cache);
        proc.register(A);
        proc.read(&launch, &mut cache);
        proc.register(C);
        assert_eq!(
            model(&proc.read(&launch, &mut cache)),
            fable,
            "a /clear keeps it"
        );
        proc.register(B);
        assert_eq!(model(&proc.read(&launch, &mut cache)), fable);
        // The first read of A already meets the process's own answer and
        // stops there: A's head still says it was resumed, and pinned.
        proc.register(S);
        proc.write(A, &[prompt_old, &fable_old, &fable_own]);
        let mut cache = TailCache::default();
        proc.read(&launch, &mut cache);
        proc.register(A);
        assert_eq!(model(&proc.read(&launch, &mut cache)), fable);
        proc.register(B);
        assert_eq!(model(&proc.read(&launch, &mut cache)), fable);
        // NEGATIVE CONTROL: a resumed conversation with no answer restores
        // nothing — the process keeps Opus 5.5 and pins nothing — so the
        // next `/resume` restores B's Sonnet 5.
        proc.register(S);
        proc.write(A, &[prompt_old]);
        let mut cache = TailCache::default();
        proc.read(&launch, &mut cache);
        proc.register(A);
        assert_eq!(
            model(&proc.read(&launch, &mut cache)).as_deref(),
            Some("Opus 5.5")
        );
        proc.register(B);
        assert_eq!(
            model(&proc.read(&launch, &mut cache)).as_deref(),
            Some("Sonnet 5")
        );
        // NEGATIVE CONTROL: a `/clear`ed session with the process's own
        // answers began since the floor — no restore, no pin.
        proc.register(S);
        proc.write(C, &[&fable_own]);
        let mut cache = TailCache::default();
        proc.read(&launch, &mut cache);
        proc.register(C);
        assert_eq!(model(&proc.read(&launch, &mut cache)), fable);
        proc.register(B);
        assert_eq!(
            model(&proc.read(&launch, &mut cache)).as_deref(),
            Some("Sonnet 5")
        );
    }

    /// A LAUNCH RESUME PINS FROM THE START (review of 2026-09-28, round 3).
    /// Every launch resume — `--resume <id>`, `--continue`, the picker —
    /// runs the same restore as an in-REPL `/resume` (2.1.284: `kbe` then
    /// `Cbe` in each, the restored model put into the REPL's first state),
    /// so a process launched onto a conversation with an answer runs that
    /// answer's model, PINNED: an in-REPL `/resume` later keeps it (the
    /// reviewer's R2 — every live-upgrade relaunch without `--model`). The
    /// footer names the restored model at once when the launch resumed THIS
    /// session and pinned nothing; a first sight it cannot tie to the launch
    /// shows nothing, but still counts the pin. NEGATIVE CONTROLS: a
    /// `--model` or an environment pin runs no restore; a launch onto a
    /// conversation with no answer pins nothing, so the next `/resume`
    /// restores.
    #[cfg(unix)]
    #[test]
    fn a_launch_resume_pins_the_model_from_the_start() {
        const X: &str = "00000000-0000-4000-8000-0000000000e1";
        const Y: &str = "00000000-0000-4000-8000-0000000000e2";
        const Z: &str = "00000000-0000-4000-8000-0000000000e3";
        let proc = Proc::new("launch-resume", "2026-09-28T15:00:00Z");
        let fable_old = answer_at("claude-fable-5-1", "2026-09-27T10:00:00.000Z", Some("high"));
        let fable_own = answer_at("claude-fable-5-1", "2026-09-28T15:05:10.000Z", Some("high"));
        let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
        let prompt_old = r#"{"type":"user","timestamp":"2026-09-27T09:59:00.000Z","message":{"role":"user","content":"hi"}}"#;
        proc.write(X, &[prompt_old, &fable_old]);
        proc.write(Y, &[&sonnet_old]);
        proc.write(Z, &[prompt_old, &fable_old]);
        let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        let model = |f: &FooterFacts| f.model.clone();
        let fable = Some("Fable 5.1".to_owned());
        // R2: `claude --resume X`, an answer in X, then `/resume Y`.
        let resumed = launch_facts(&argv(&["claude", "--resume", X]));
        proc.register(X);
        let mut cache = TailCache::default();
        let first = proc.read(&resumed, &mut cache);
        assert_eq!(model(&first), fable, "R2.0: the restored model, at once");
        assert!(!first.model_open);
        assert!(
            first.effort.is_none() && first.effort_open,
            "the effort open for Fable 5.1's card alone"
        );
        assert_eq!(first.effort_for, fable);
        proc.write(X, &[prompt_old, &fable_old, &fable_own]);
        assert_eq!(model(&proc.read(&resumed, &mut cache)), fable, "R2.1");
        proc.register(Y);
        assert_eq!(
            model(&proc.read(&resumed, &mut cache)),
            fable,
            "R2.2: pinned"
        );
        // With no answer in X before `/resume Y`, too; and `--continue`.
        proc.write(X, &[prompt_old, &fable_old]);
        for launch in [
            resumed.clone(),
            launch_facts(&argv(&["claude", "--continue"])),
        ] {
            proc.register(X);
            let mut cache = TailCache::default();
            assert_eq!(model(&proc.read(&launch, &mut cache)), fable, "{launch:?}");
            proc.register(Y);
            assert_eq!(model(&proc.read(&launch, &mut cache)), fable, "{launch:?}");
        }
        // A first sight the launch does not name — the argv unread, or a
        // `/resume` into Z the cache did not see after `--resume X` — shows
        // no model, yet the pin stands: `/resume Y` never shows Sonnet 5.
        for launch in [LaunchFacts::default(), resumed.clone()] {
            proc.register(Z);
            let mut cache = TailCache::default();
            assert_eq!(model(&proc.read(&launch, &mut cache)), None, "{launch:?}");
            proc.register(Y);
            assert_eq!(model(&proc.read(&launch, &mut cache)), None, "{launch:?}");
        }
        // NEGATIVE CONTROL: pinned at launch, nothing is restored — the
        // flag's model, or none for an environment pin.
        let flagged = launch_facts(&argv(&[
            "claude",
            "--model",
            "claude-opus-5-5",
            "--resume",
            X,
        ]));
        let env = launch_facts_of(
            &argv(&["claude", "--resume", X]),
            &argv(&["ANTHROPIC_MODEL=claude-opus-5-5"]),
        );
        for (launch, shown) in [(flagged, Some("Opus 5.5")), (env, None)] {
            proc.register(X);
            let mut cache = TailCache::default();
            assert_eq!(model(&proc.read(&launch, &mut cache)).as_deref(), shown);
            proc.register(Y);
            assert_eq!(model(&proc.read(&launch, &mut cache)).as_deref(), shown);
        }
        // NEGATIVE CONTROL: launched onto a conversation with no answer,
        // Claude restored nothing — the next `/resume` restores Y's.
        proc.write(X, &[prompt_old]);
        proc.register(X);
        let mut cache = TailCache::default();
        assert_eq!(model(&proc.read(&resumed, &mut cache)), None);
        proc.register(Y);
        assert_eq!(
            model(&proc.read(&resumed, &mut cache)).as_deref(),
            Some("Sonnet 5")
        );
    }

    /// A `/resume` IS TOLD FROM A `/clear` BY WHEN THE SWITCH CAME (review of
    /// 2026-09-28, round 4). The resolver records when it last saw the
    /// process in the session it left, and a conversation whose first row of
    /// its own is older than that moment existed before the switch: a
    /// `/resume`, whoever began it and whenever — here D, another tab's
    /// conversation begun and answered with Fable 5.1 AFTER this process
    /// started (the reviewer's H5; the `/resume` picker lists the newest
    /// first). Claude restores D's model and pins it, so the next `/resume`
    /// keeps it; and a process that pinned its own keeps its own in D, whose
    /// rows from before the switch are D's, never the process's. A `/resume`
    /// back into a session this process began before a `/clear` restores
    /// and pins the same way. NEGATIVE CONTROLS: a `/clear` whose session has
    /// answered by the read that sees the switch is no resume, and neither is
    /// a `/branch`, whose copies keep their originals' stamps but carry
    /// `forkedFrom` (the same rows unmarked ARE a resume) — the next
    /// `/resume` restores after both; and the floor-only judge (every read
    /// at the process's start, so the switch known only to come after it)
    /// misses the pin D made, and reads D's rows as the pinned process's.
    #[cfg(unix)]
    #[test]
    fn a_resume_into_a_conversation_begun_since_the_start_pins_it() {
        const S: &str = "00000000-0000-4000-8000-0000000001a1";
        const D: &str = "00000000-0000-4000-8000-0000000001a2";
        const B: &str = "00000000-0000-4000-8000-0000000001a3";
        const C: &str = "00000000-0000-4000-8000-0000000001a4";
        const F: &str = "00000000-0000-4000-8000-0000000001a5";
        let proc = Proc::new("late-resume", "2026-09-28T15:00:00Z");
        let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
        let d_prompt = r#"{"type":"user","timestamp":"2026-09-28T15:20:00.000Z","message":{"role":"user","content":"hi"}}"#;
        let d_answer = answer_at("claude-fable-5-1", "2026-09-28T15:20:30.000Z", Some("low"));
        let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
        let chose = [
            command_at("model", "2026-09-28T15:00:30.000Z"),
            result_at(
                "2026-09-28T15:00:30.000Z",
                "Set model to `Opus 5.5` for this session only",
            ),
        ];
        proc.write(D, &[d_prompt, &d_answer]);
        proc.write(B, &[&sonnet_old]);
        let launch = LaunchFacts::default();
        let model = |f: &FooterFacts| f.model.clone();
        let shown = |f: &FooterFacts| (f.model.clone(), f.effort.clone());
        let is = |m: &str, e: Option<&str>| (Some(m.to_owned()), e.map(str::to_owned));
        let realistic = [
            "2026-09-28T15:25:00Z",
            "2026-09-28T15:30:00Z",
            "2026-09-28T15:31:00Z",
        ];
        let at_floor = ["2026-09-28T15:00:00Z"; 3];
        // S (last seen at `clock[0]`), `/resume D`, `/resume B`.
        let h5 = |clock: [&str; 3], first: &[&str]| {
            proc.register(S);
            proc.write(S, first);
            let mut cache = TailCache::default();
            proc.at(clock[0]);
            let before = proc.read(&launch, &mut cache);
            proc.register(D);
            proc.at(clock[1]);
            let in_d = proc.read(&launch, &mut cache);
            proc.register(B);
            proc.at(clock[2]);
            (before, in_d, proc.read(&launch, &mut cache))
        };
        let (before, in_d, in_b) = h5(realistic, &[&opus]);
        assert_eq!(shown(&before), is("Opus 5.5", Some("xhigh")));
        assert_eq!(
            shown(&in_d),
            is("Fable 5.1", None),
            "H5.0: restored — not Opus 5.5's xhigh, nor the other tab's `low`"
        );
        assert_eq!(
            model(&in_b).as_deref(),
            Some("Fable 5.1"),
            "H5.1: the restore pinned it"
        );
        // NEGATIVE CONTROL: the floor-only judge reads D as a `/clear` — D's
        // answer as the process's own — and restores B's Sonnet 5.
        let (_, in_d, in_b) = h5(at_floor, &[&opus]);
        assert_eq!(shown(&in_d), is("Fable 5.1", Some("low")));
        assert_eq!(model(&in_b).as_deref(), Some("Sonnet 5"));
        // PINNED by a choice: the process keeps Opus 5.5 in D and after.
        let (_, in_d, in_b) = h5(realistic, &[&chose[0], &chose[1], &opus]);
        assert_eq!(
            shown(&in_d),
            is("Opus 5.5", Some("xhigh")),
            "D's Fable 5.1 answer is another process's"
        );
        assert_eq!(model(&in_b).as_deref(), Some("Opus 5.5"));
        // NEGATIVE CONTROL: the floor-only judge names D's answer.
        let (_, in_d, _) = h5(at_floor, &[&chose[0], &chose[1], &opus]);
        assert_eq!(model(&in_d).as_deref(), Some("Fable 5.1"));

        // A `/clear` into C, which answered before the read that sees the
        // switch, is still no resume: no pin, and `/resume B` restores.
        let after = |first: (&str, &[&str]), then: &[(&str, &str)]| {
            proc.register(S);
            proc.write(S, &[&opus]);
            proc.write(first.0, first.1);
            let mut cache = TailCache::default();
            proc.at("2026-09-28T15:25:00Z");
            proc.read(&launch, &mut cache);
            then.iter()
                .map(|(session, at)| {
                    proc.register(session);
                    proc.at(at);
                    model(&proc.read(&launch, &mut cache))
                })
                .collect::<Vec<_>>()
        };
        let own = answer_at("claude-opus-5-5", "2026-09-28T15:27:00.000Z", Some("xhigh"));
        assert_eq!(
            after(
                (C, &[&own]),
                &[(C, "2026-09-28T15:28:00Z"), (B, "2026-09-28T15:29:00Z")]
            ),
            [Some("Opus 5.5".into()), Some("Sonnet 5".into())],
            "a /clear pins nothing"
        );
        // A `/branch` into F: copies of S's rows, `forkedFrom` S — Claude
        // restores nothing into a branch, so nothing is pinned.
        let branched = |row: &str| {
            format!(
                r#"{},"forkedFrom":{{"sessionId":"{S}","messageUuid":"u"}}}}"#,
                &row[..row.len() - 1]
            )
        };
        assert_eq!(
            after(
                (F, &[&branched(&opus)]),
                &[(F, "2026-09-28T15:26:00Z"), (B, "2026-09-28T15:27:00Z")]
            ),
            [Some("Opus 5.5".into()), Some("Sonnet 5".into())],
            "a /branch pins nothing"
        );
        // NEGATIVE CONTROL: the same rows unmarked are a conversation from
        // before the switch — a resume, which restored Opus 5.5 and pinned it.
        assert_eq!(
            after(
                (F, &[&opus]),
                &[(F, "2026-09-28T15:26:00Z"), (B, "2026-09-28T15:27:00Z")]
            ),
            [Some("Opus 5.5".into()), Some("Opus 5.5".into())]
        );
        // `/clear` into C, then `/resume S` — back into the session this
        // process began: Claude restores S's Opus 5.5 and pins it.
        assert_eq!(
            after(
                (C, &[]),
                &[
                    (C, "2026-09-28T15:26:00Z"),
                    (S, "2026-09-28T15:30:00Z"),
                    (B, "2026-09-28T15:31:00Z")
                ]
            ),
            [
                Some("Opus 5.5".into()),
                Some("Opus 5.5".into()),
                Some("Opus 5.5".into())
            ],
            "a resume into the process's own session pins too"
        );
    }

    /// A conversation whose FILE was born before the process was last seen
    /// in the session it left existed before the switch, whatever its rows
    /// say: another tab's `/branch`, all copies, is a `/resume` target like
    /// any other, and Claude restores (and pins) its newest answer.
    /// NEGATIVE CONTROL: the same file born AFTER that moment is judged by
    /// its rows — copies — and is no resume: `/resume B` then restores B's.
    #[cfg(unix)]
    #[test]
    fn a_conversation_born_before_the_switch_is_resumed() {
        const S: &str = "00000000-0000-4000-8000-0000000001b1";
        const X: &str = "00000000-0000-4000-8000-0000000001b2";
        const B: &str = "00000000-0000-4000-8000-0000000001b3";
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let stamp = |secs: u64| {
            crate::harness::usage::rfc3339_utc(i64::try_from(secs).unwrap()).replace('Z', ".000Z")
        };
        let proc = Proc::new("born-before", &stamp(now - 3600));
        let opus = answer_at("claude-opus-5-5", &stamp(now - 1800), Some("xhigh"));
        let copied = format!(
            r#"{{"type":"assistant","timestamp":"{}","effort":"high","message":{{"model":"claude-fable-5-1"}},"forkedFrom":{{"sessionId":"elsewhere","messageUuid":"u"}}}}"#,
            stamp(now - 1200)
        );
        let sonnet_old = answer_at("claude-sonnet-5", &stamp(now - 7 * 86_400), Some("high"));
        proc.register(S);
        proc.write(S, &[&opus]);
        proc.write(B, &[&sonnet_old]);
        // X is made now, by another tab.
        proc.write(X, &[&copied]);
        let launch = LaunchFacts::default();
        let run = |seen_in_s: u64| {
            proc.register(S);
            let mut cache = TailCache::default();
            proc.clock.set(seen_in_s);
            proc.read(&launch, &mut cache);
            [X, B]
                .into_iter()
                .zip(1..)
                .map(|(session, n)| {
                    proc.register(session);
                    proc.clock.set(seen_in_s + 10 * n);
                    proc.read(&launch, &mut cache).model
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run(now + 10),
            [Some("Fable 5.1".into()), Some("Fable 5.1".into())],
            "born before the switch: resumed, restored and pinned"
        );
        assert_eq!(
            run(now - 10),
            [Some("Opus 5.5".into()), Some("Sonnet 5".into())],
            "born after the process was last seen in S: its rows decide"
        );
    }

    /// A `/BRANCH`'S COPIED MEMORY-MODE ROW IS NOT ITS OWN (the fourth review
    /// of 2026-09-28, a minor): after the copies, which carry `forkedFrom`,
    /// Claude 2.1.284's `/branch` writes the source session's `memory-mode`
    /// rows as `{...row, sessionId: <new>}` — the original stamp kept, no
    /// `forkedFrom`. Read as the branch's own first row, that old stamp made
    /// a branch begun since the process was last seen read as a `/resume`:
    /// its copied answer "restored", a pin recorded, and the next `/resume`
    /// kept the process's model where Claude — whose fork restores nothing
    /// and pins nothing — runs the resumed conversation's. In the vendor's
    /// own row layout. NEGATIVE CONTROL: the branch's first message row of
    /// its own is its stamp, and without `own_rows` the head's first stamp is
    /// the copied one.
    #[cfg(unix)]
    #[test]
    fn a_branchs_copied_memory_mode_row_is_not_its_own() {
        const S: &str = "00000000-0000-4000-8000-0000000001c1";
        const F: &str = "00000000-0000-4000-8000-0000000001c2";
        const B: &str = "00000000-0000-4000-8000-0000000001c3";
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let stamp = |secs: u64| {
            crate::harness::usage::rfc3339_utc(i64::try_from(secs).unwrap()).replace('Z', ".000Z")
        };
        let memory_mode = |session: &str, at: u64| {
            format!(
                r#"{{"type":"memory-mode","mode":"off","afterUuid":null,"timestamp":"{}","sessionId":"{session}"}}"#,
                stamp(at)
            )
        };
        let proc = Proc::new("branch-memory", &stamp(now - 3600));
        let opus = answer_at("claude-opus-5-5", &stamp(now - 1800), Some("xhigh"));
        let copied = format!(
            r#"{{"type":"assistant","timestamp":"{}","effort":"xhigh","message":{{"model":"claude-opus-5-5"}},"forkedFrom":{{"sessionId":"{S}","messageUuid":"u"}}}}"#,
            stamp(now - 1800)
        );
        let sonnet_old = answer_at("claude-sonnet-5", &stamp(now - 7 * 86_400), Some("high"));
        proc.write(S, &[&memory_mode(S, now - 1900), &opus]);
        proc.write(B, &[&sonnet_old]);
        // The branch, begun after the process was last seen in S: the copy,
        // then the memory-mode row as the vendor writes it.
        proc.write(F, &[&copied, &memory_mode(F, now - 1900)]);
        let path = |session: &str| proc.project.join(format!("{session}.jsonl"));
        let head =
            |session: &str, own: bool| head_stamp(&mut open_regular(&path(session)).unwrap(), own);
        assert_eq!(head(F, true), None, "no message row of its own yet");
        assert_eq!(
            head(F, false),
            Some(now - 1800),
            "control: the head's first stamp is the copy's"
        );
        let own = answer_at("claude-opus-5-5", &stamp(now + 1), Some("xhigh"));
        std::fs::write(path(F), body(&[&copied, &memory_mode(F, now - 1900), &own])).unwrap();
        assert_eq!(
            head(F, true),
            Some(now + 1),
            "control: its own first message"
        );
        proc.write(F, &[&copied, &memory_mode(F, now - 1900)]);

        // The reviewer's walk: S seen, `/branch` to F, then `/resume B`.
        let launch = LaunchFacts::default();
        let mut cache = TailCache::default();
        proc.register(S);
        proc.clock.set(now - 10);
        assert_eq!(
            proc.read(&launch, &mut cache).model.as_deref(),
            Some("Opus 5.5")
        );
        proc.register(F);
        proc.clock.set(now + 5);
        assert_eq!(
            proc.read(&launch, &mut cache).model.as_deref(),
            Some("Opus 5.5"),
            "the branch runs on the process's model"
        );
        proc.register(B);
        proc.clock.set(now + 15);
        assert_eq!(
            proc.read(&launch, &mut cache).model.as_deref(),
            Some("Sonnet 5"),
            "the fork pinned nothing: `/resume B` restores B's model, as Claude does"
        );
    }

    /// AN OVERSIZED NEWEST ANSWER (review of 2026-09-28, round 3, R4): a
    /// resumed conversation's newest answer can be one row past
    /// [`MAX_LINE_BYTES`] (a Write tool call carrying a large file), which
    /// Claude restores and this reader does not parse. It is never passed
    /// over for an older answer's model: the model is unknown — however the
    /// read windows cut the row. NEGATIVE CONTROLS: an oversized row that is
    /// no answer (a large tool result) is passed over, and so is an
    /// oversized answer of the process's own, since the floor.
    #[cfg(unix)]
    #[test]
    fn an_oversized_newest_answer_is_never_passed_over() {
        const S: &str = "00000000-0000-4000-8000-0000000000f1";
        const A: &str = "00000000-0000-4000-8000-0000000000f2";
        let proc = Proc::new("oversized", "2026-09-28T15:00:00Z");
        let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
        let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
        // Claude's own key order: `message` first, `type` and `timestamp`
        // after it.
        let big_answer = |model: &str, at: &str, bytes: usize| {
            format!(
                r#"{{"parentUuid":"p","isSidechain":false,"message":{{"model":"{model}","role":"assistant","content":[{{"type":"tool_use","name":"Write","input":{{"content":"{}"}}}}]}},"type":"assistant","uuid":"u","timestamp":"{at}","effort":"high"}}"#,
                "x".repeat(bytes)
            )
        };
        let big_result = |at: &str, bytes: usize| {
            format!(
                r#"{{"parentUuid":"p","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","content":"{}"}}]}},"type":"user","uuid":"u","timestamp":"{at}"}}"#,
                "x".repeat(bytes)
            )
        };
        let after_resume = |a: &[&str]| {
            proc.register(S);
            proc.write(S, &[&opus]);
            proc.write(A, a);
            let mut cache = TailCache::default();
            proc.read(&LaunchFacts::default(), &mut cache);
            proc.register(A);
            proc.read(&LaunchFacts::default(), &mut cache).model
        };
        // 300 KB: past a row's limit, inside one read window; 1.2 MB:
        // across three windows.
        for bytes in [300 * 1024, 1200 * 1024] {
            let big = big_answer("claude-fable-5-1", "2026-09-27T10:05:00.000Z", bytes);
            assert_eq!(after_resume(&[&sonnet_old, &big]), None, "R4 at {bytes}");
            let tool = big_result("2026-09-27T10:05:00.000Z", bytes);
            assert_eq!(
                after_resume(&[&sonnet_old, &tool]).as_deref(),
                Some("Sonnet 5"),
                "a tool result at {bytes}"
            );
            // Restored Sonnet 5 runs on: its own answer, since the floor.
            let own = big_answer("claude-sonnet-5", "2026-09-28T15:06:00.000Z", bytes);
            assert_eq!(
                after_resume(&[&sonnet_old, &own]).as_deref(),
                Some("Sonnet 5"),
                "the process's own answer at {bytes}"
            );
        }
    }

    /// [`rows_back`] meets every row once, newest first, an OVERSIZED one
    /// in its place — by its end, never its start taken for a row of its
    /// own — however the read windows fall across it.
    #[cfg(unix)]
    #[test]
    fn rows_back_meets_an_oversized_row_once_in_its_place() {
        let dir = cache_test_dir("rows-back");
        let path = dir.join("t.jsonl");
        let huge = format!("{{\"h\":\"{}\",\"end\":1}}", "a".repeat(1200 * 1024));
        let big = format!("{{\"b\":\"{}\",\"end\":2}}", "b".repeat(300 * 1024));
        std::fs::write(
            &path,
            body(&["{\"n\":1}", &huge, "{\"n\":2}", &big, "{\"n\":3}"]),
        )
        .unwrap();
        let mut met = Vec::new();
        let whole = rows_back(&path, |row| {
            met.push(match row {
                Row::Whole(raw) => String::from_utf8_lossy(raw).into_owned(),
                Row::Oversized(part) => {
                    let text = String::from_utf8_lossy(part);
                    let end = text
                        .trim_end()
                        .rsplit(',')
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    format!("oversized {end}")
                }
            });
            std::ops::ControlFlow::Continue(())
        });
        assert_eq!(whole, Some(true));
        assert_eq!(
            met,
            [
                "{\"n\":3}",
                "oversized \"end\":2}",
                "{\"n\":2}",
                "oversized \"end\":1}",
                "{\"n\":1}"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What pins a process's model at launch: a `--model` of any spelling,
    /// or an environment model pin — the variables Claude's resume checks.
    /// Only a real id is SHOWN. An empty variable, or another one, pins
    /// nothing. A launch RESUME is recorded, not taken for a pin: it pins
    /// only by restoring an answer the conversation holds, which the
    /// transcript says ([`TailCache`]).
    #[test]
    fn a_launch_pins_the_model_by_flag_or_environment() {
        let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        let of = |args: &[&str], env: &[&str]| launch_facts_of(&argv(args), &argv(env));
        assert_eq!(
            of(&["claude", "--model", "claude-opus-5-5"], &[]),
            LaunchFacts {
                model: Some("Opus 5.5".into()),
                pinned: true,
                resume: None,
            }
        );
        assert_eq!(
            of(&["claude", "--model", "opus"], &[]),
            LaunchFacts {
                model: None,
                pinned: true,
                resume: None,
            }
        );
        for env in [
            "ANTHROPIC_MODEL=claude-opus-5-5",
            "ANTHROPIC_DEFAULT_OPUS_MODEL=claude-opus-5-5",
            "ANTHROPIC_DEFAULT_FABLE_MODEL=x",
        ] {
            assert!(of(&["claude"], &[env]).pinned, "{env}");
        }
        for env in [
            "ANTHROPIC_MODEL=",
            "ANTHROPIC_API_KEY=k",
            "ANTHROPIC_DEFAULT_OPUS=x",
        ] {
            assert!(!of(&["claude"], &[env]).pinned, "{env}");
        }
        assert_eq!(
            of(&["claude", "--resume", "x"], &[]),
            LaunchFacts {
                model: None,
                pinned: false,
                resume: Some(LaunchResume::Session("x".into())),
            }
        );
        assert_eq!(
            of(&["claude", "--continue", "--model", "claude-opus-5-5"], &[]),
            LaunchFacts {
                model: Some("Opus 5.5".into()),
                pinned: true,
                resume: Some(LaunchResume::Unnamed),
            }
        );
        assert_eq!(
            of(&["claude", "--resume", "x", "--fork-session"], &[]).resume,
            Some(LaunchResume::Unnamed)
        );
    }

    /// Claude's `/effort` RESULTS name the effort at once: a level as it
    /// says it, the top-effort mode ([`CLAUDE_TOP_EFFORT_KEY`]) as `xhigh` (the
    /// level it runs at). `auto` — the model decides — and a level this build
    /// does not know are unknown, never the old effort; so is an `/effort`
    /// whose result is unreadable.
    #[test]
    fn an_effort_result_names_the_effort_at_once() {
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
        let answer = r#"{"type":"assistant","timestamp":"2026-09-24T04:13:30.000Z","effort":"high","message":{"model":"claude-opus-5-5"}}"#;
        let command = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args></command-args>"}}"#;
        let result = |text: &str| {
            format!(
                r#"{{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{{"content":{}}}}}"#,
                aterm_json::to_string(&Value::String(format!(
                    "<local-command-stdout>{text}</local-command-stdout>"
                )))
                .unwrap()
            )
        };
        let effort = |text: Option<&str>| {
            let mut body = format!("{answer}\n{command}\n");
            if let Some(text) = text {
                body.push_str(&result(text));
                body.push('\n');
            }
            tail_facts(body.as_bytes(), start)
        };
        let top = format!(
            "Set effort level to {CLAUDE_TOP_EFFORT_KEY} (this session only): xhigh + dynamic workflow orchestration"
        );
        for (text, level) in [
            (
                "Set effort level to high (saved as your default for new sessions): Deeper reasoning",
                "high",
            ),
            (top.as_str(), "xhigh"),
            (
                "Current effort level: max (Maximum reasoning; this session only)",
                "max",
            ),
            ("Set effort level to \u{1b}[1mlow\u{1b}[22m: Quick", "low"),
        ] {
            let facts = effort(Some(text));
            assert_eq!(facts.effort.shown(), Some(level), "{text:?}");
            assert_eq!(
                facts.model.shown(),
                Some("Opus 5.5"),
                "an effort leaves the model"
            );
        }
        for text in [
            "Effort level set to auto for this session",
            "Set effort level to turbo: new",
        ] {
            assert_eq!(effort(Some(text)).effort, Said::Unread, "{text:?}");
        }
        assert_eq!(
            effort(None).effort,
            Said::Unread,
            "an unreadable result is no effort"
        );
        // NEGATIVE CONTROL: without the command, the answer's effort stands.
        assert_eq!(
            tail_facts(format!("{answer}\n").as_bytes(), start)
                .effort
                .shown(),
            Some("high")
        );
    }

    /// Rows a PREDECESSOR wrote — a choice and an answer, both before the
    /// floor — say nothing for this process. NEGATIVE CONTROL: with no
    /// floor, the choice is read.
    #[test]
    fn a_predecessors_choice_is_not_this_process() {
        let answer = r#"{"type":"assistant","timestamp":"2026-09-22T16:44:57.000Z","effort":"high","message":{"model":"claude-opus-5"}}"#;
        let chose = r#"{"type":"user","timestamp":"2026-09-22T16:45:00.000Z","message":{"content":"<local-command-stdout>Set model to `Fable 5.1`</local-command-stdout>"}}"#;
        let body = format!("{answer}\n{chose}\n");
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
        assert_eq!(tail_facts(body.as_bytes(), start), TailFacts::default());
        assert_eq!(
            tail_facts(body.as_bytes(), None).model.shown(),
            Some("Fable 5.1")
        );
    }

    /// THE CARRY AND THE BACKWARD READ: a model named once stays named while
    /// the same process appends rows larger than the whole tail window — the
    /// owner's 530 KiB image prompt — read as an APPEND (only the new bytes);
    /// a choice made in an append is read at once, however big what follows
    /// it; and a read afresh (a new file, a first read after both rows had
    /// landed) looks back past the huge row. NEGATIVE CONTROLS: the plain
    /// tail window does not see the answer; a new floor, a replaced file and
    /// a shrunk one are read afresh, never carried.
    #[cfg(unix)]
    #[test]
    fn an_answer_past_the_tail_window_is_carried_for_the_same_process() {
        let root = cache_test_dir("carry");
        let path = root.join("session.jsonl");
        let start = crate::harness::upgrade_models::parse_utc("2026-09-28T15:04:08Z").unwrap();
        let append = |text: &str| {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        };
        let image = format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-09-28T15:18:13.903Z\",\"message\":{{\"content\":[{{\"type\":\"image\",\"source\":\"{}\"}}]}}}}\n",
            "A".repeat(600 * 1024)
        );
        std::fs::write(
            &path,
            format!(
                "{}\n",
                assistant_row("claude-opus-5-5", "2026-09-28T15:05:00Z")
            ),
        )
        .unwrap();
        let mut cache = TailCache::default();
        let read =
            |cache: &mut TailCache, since| cache.read(&path, 4242, Some(start), since).unwrap();
        assert_eq!(read(&mut cache, start).model.shown(), Some("Opus 5.5"));
        append(&image);
        assert_eq!(
            read(&mut cache, start).model.shown(),
            Some("Opus 5.5"),
            "the answer is 600 KiB above the tail now: carried"
        );
        assert_eq!(cache.appends, 1, "read as an append");
        // Control: the plain tail window does not see it.
        assert_eq!(
            read_tail_facts(&path, Some(start)).unwrap().model,
            Said::Unsaid
        );
        // A read afresh looks back past the huge row.
        assert_eq!(
            TailCache::default()
                .read(&path, 4242, Some(start), start)
                .unwrap()
                .model
                .shown(),
            Some("Opus 5.5")
        );
        append(
            r#"{"type":"user","timestamp":"2026-09-28T15:19:00.000Z","message":{"content":"<local-command-stdout>Set model to `Fable 5.1`</local-command-stdout>"}}
"#,
        );
        append(&image);
        assert_eq!(
            read(&mut cache, start).model.shown(),
            Some("Fable 5.1"),
            "every appended byte is scanned"
        );
        assert_eq!(cache.appends, 2);
        // A replaced file (a new inode, the same rows) is read afresh.
        let copy = root.join("copy.jsonl");
        std::fs::copy(&path, &copy).unwrap();
        std::fs::rename(&copy, &path).unwrap();
        append("{\"type\":\"user\",\"timestamp\":\"2026-09-28T15:20:00.000Z\"}\n");
        assert_eq!(read(&mut cache, start).model.shown(), Some("Fable 5.1"));
        assert_eq!(cache.appends, 2, "a replaced file is no append");
        // Grown again, it carries from its own fresh read.
        append(&format!(
            "{}\n",
            assistant_row("claude-haiku-4-5", "2026-09-28T16:00:00Z")
        ));
        append(&image);
        assert_eq!(read(&mut cache, start).model.shown(), Some("Haiku 4.5"));
        assert_eq!(cache.appends, 3);
        // A new floor (an in-place exec after every row) is never carried.
        assert_eq!(read(&mut cache, start + 6 * 3600), TailFacts::default());
        // A file rewritten SHORTER is read afresh.
        assert_eq!(read(&mut cache, start).model.shown(), Some("Haiku 4.5"));
        std::fs::write(
            &path,
            format!(
                "{}\n",
                assistant_row("claude-opus-5", "2026-09-28T16:01:00Z")
            ),
        )
        .unwrap();
        assert_eq!(read(&mut cache, start).model.shown(), Some("Opus 5"));
        assert_eq!(cache.appends, 3, "a shrunk file is no append");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// THE LAUNCH TIER: only a real model id on the process's own command
    /// line; an alias is resolved per account and build, which this cannot
    /// know.
    #[test]
    fn launch_facts_take_only_a_real_id() {
        let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        let model = |args: &[&str]| launch_facts(&argv(args)).model;
        assert_eq!(
            model(&["claude", "--model", "claude-opus-5-5"]).as_deref(),
            Some("Opus 5.5")
        );
        assert_eq!(
            model(&["claude", "--model=claude-fable-5-1[1m]"]).as_deref(),
            Some("Fable 5.1")
        );
        assert_eq!(
            model(&[
                "claude",
                "--model",
                "claude-opus-5",
                "--resume",
                "x",
                "--model",
                "claude-opus-5-5"
            ])
            .as_deref(),
            Some("Opus 5.5"),
            "the last one, as Claude's parser takes it"
        );
        assert_eq!(model(&["claude", "--model", "opus"]), None);
        assert_eq!(model(&["claude", "--dangerously-skip-permissions"]), None);
    }

    /// The floor is the kernel's start, else the registry's `procStart` (the
    /// path where the kernel cannot be asked); with neither, no model.
    #[test]
    fn facts_for_pid_floors_at_the_kernel_else_the_registry_start() {
        let root = std::env::temp_dir().join(format!("aterm-footer-floor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let claude = root.join("claude");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("0b5e7a11-f100.jsonl"),
            concat!(
                r#"{"type":"assistant","timestamp":"2026-09-22T16:44:57.000Z","effort":"high","message":{"model":"claude-opus-5"}}"#,
                "\n"
            ),
        )
        .unwrap();
        let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z").unwrap();
        let entry = |proc_start: Option<u64>| {
            let stamp = proc_start.map_or(String::new(), |s| {
                format!(r#","procStart":"{}""#, lstart_utc(s))
            });
            let text = format!(
                r#"{{"pid":4242,"sessionId":"0b5e7a11-f100","cwd":"{}"{stamp}}}"#,
                cwd.display()
            );
            std::fs::write(claude.join("sessions/4242.json"), text).unwrap();
        };
        let model =
            |started: Option<u64>| facts_for_pid(&claude, 4242, started).and_then(|f| f.model);
        entry(Some(start));
        assert_eq!(model(Some(start)), None, "the kernel's start floors it");
        assert_eq!(model(None), None, "the registry's start floors it");
        entry(None);
        assert_eq!(model(None), None, "no start known: no model");
        // The path is the registry's `cwd`, whatever the transcript says.
        assert_eq!(
            facts_for_pid(&claude, 4242, None).and_then(|f| f.path),
            home_path(&cwd, aterm_types::dirs::home_dir().as_deref())
        );
        assert!(home_path(&cwd, None).is_some());
        // Negative control: a floor older than the answer reads it.
        entry(Some(start - 3 * 86_400));
        assert_eq!(model(None).as_deref(), Some("Opus 5"));
        // An image exec'd in place (same pid, same kernel start) registers
        // its own `startedAt`: the floor moves with it.
        let old_start = start - 3 * 86_400;
        std::fs::write(
            claude.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"0b5e7a11-f100","cwd":"{}","procStart":"{}","startedAt":{}}}"#,
                cwd.display(),
                lstart_utc(old_start),
                start * 1000 + 250
            ),
        )
        .unwrap();
        assert_eq!(
            model(Some(old_start)),
            None,
            "the pre-exec image's answer does not speak for the new one"
        );
        // THE LAUNCH TIER: the process's own `--model` names the model while
        // its transcript since the floor names none — the predecessor's
        // Opus 5 answer in the file notwithstanding — and closes it to the
        // launch card.
        let entry = session_of_pid(&claude, 4242, Some(old_start)).unwrap();
        let launch = launch_facts(&["claude".into(), "--model".into(), "claude-opus-5-5".into()]);
        let facts = facts_for_entry_cached(
            &claude,
            4242,
            Some(old_start),
            &entry,
            &launch,
            &mut TailCache::default(),
        );
        assert_eq!(facts.model.as_deref(), Some("Opus 5.5"));
        assert!(!facts.model_open && facts.effort_open);
        assert_eq!(
            facts.owner,
            Some(FactsOwner {
                floor: Some(start),
                session_id: "0b5e7a11-f100".into(),
            }),
            "the floor is the image's"
        );
        // With no start known at all, the launch flag is still the process's own.
        let unfloored = SessionEntry {
            proc_start: None,
            started_at: None,
            ..entry.clone()
        };
        let facts = facts_for_entry_cached(
            &claude,
            4242,
            None,
            &unfloored,
            &launch,
            &mut TailCache::default(),
        );
        assert_eq!(facts.model.as_deref(), Some("Opus 5.5"));
        // A choice this image made, unreadable, outranks the flag: unknown.
        std::fs::write(
            project.join("0b5e7a11-f100.jsonl"),
            format!(
                "{}\n",
                r#"{"type":"user","timestamp":"2026-09-24T05:00:00.000Z","message":{"content":"<command-name>/model</command-name>"}}"#
            ),
        )
        .unwrap();
        let facts = facts_for_entry_cached(
            &claude,
            4242,
            Some(old_start),
            &entry,
            &launch,
            &mut TailCache::default(),
        );
        assert_eq!(facts.model, None);
        assert!(!facts.model_open, "and nothing older may fill it");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `/clear` (and an in-REPL `/resume`) gives the SAME process a new
    /// session id — Claude rewrites `sessions/<pid>.json` with it and leaves
    /// `procStart` and `startedAt` alone — and a transcript that has said
    /// nothing yet. The model and the effort are the process's: what it chose
    /// in the session it left stands, never its `--model` (every harness
    /// relaunch carries one) nor its card. NEGATIVE CONTROL: a reader that
    /// keeps nothing of the process shows the launch flag's model after the
    /// clear. A new floor (an image exec'd in place) inherits nothing.
    #[test]
    fn a_clear_keeps_the_model_the_process_chose() {
        let root = std::env::temp_dir().join(format!("aterm-footer-clear-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let claude = root.join("claude");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        let start = crate::harness::upgrade_models::parse_utc("2026-09-28T15:04:08Z").unwrap();
        let registry = |session: &str, image: u64| {
            std::fs::write(
                claude.join("sessions/4242.json"),
                format!(
                    r#"{{"pid":4242,"sessionId":"{session}","cwd":"{}","procStart":"{}","startedAt":{},"version":"2.1.283"}}"#,
                    cwd.display(),
                    lstart_utc(start),
                    image * 1000 + 684
                ),
            )
            .unwrap();
        };
        let transcript = |session: &str, rows: &[&str]| {
            let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
            std::fs::write(project.join(format!("{session}.jsonl")), body).unwrap();
        };
        let launch = launch_facts(&["claude".into(), "--model".into(), "claude-opus-5-5".into()]);
        // Each read at the wall-clock time it names.
        let read_at = |cache: &mut TailCache, at: &str| {
            let entry = session_of_pid(&claude, 4242, Some(start)).expect("the registry");
            let at = crate::harness::upgrade_models::parse_utc(at).unwrap();
            facts_for_entry_at(
                &claude,
                4242,
                Some(start),
                &entry,
                &launch,
                cache,
                UNIX_EPOCH + std::time::Duration::from_secs(at),
            )
        };
        const A: &str = "00000000-0000-4000-8000-00000000000a";
        const B: &str = "00000000-0000-4000-8000-00000000000b";
        const C: &str = "00000000-0000-4000-8000-00000000000c";
        const CHOICE: [&str; 2] = [
            r#"{"type":"user","timestamp":"2026-09-28T15:04:17.798Z","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}"#,
            r#"{"type":"user","timestamp":"2026-09-28T15:04:17.798Z","message":{"role":"user","content":"<local-command-stdout>Set model to `Fable 5.1` for this session only</local-command-stdout>"}}"#,
        ];
        const EFFORT: &str = r#"{"type":"user","timestamp":"2026-09-28T15:04:28.429Z","message":{"role":"user","content":"<local-command-stdout>Set effort level to high (this session only): Deeper reasoning</local-command-stdout>"}}"#;
        registry(A, start);
        transcript(A, &CHOICE);
        let mut cache = TailCache::default();
        let facts = read_at(&mut cache, "2026-09-28T15:04:20Z");
        assert_eq!(
            (facts.model.as_deref(), facts.effort.as_deref()),
            (Some("Fable 5.1"), None)
        );
        // An `/effort` the resolver has not read yet, then `/clear`: a new
        // session id, its transcript not written yet. The session LEFT is
        // read again at the switch, so the effort is not lost.
        transcript(A, &[CHOICE[0], CHOICE[1], EFFORT]);
        registry(B, start);
        let facts = read_at(&mut cache, "2026-09-28T15:04:30Z");
        assert_eq!(
            (facts.model.as_deref(), facts.effort.as_deref()),
            (Some("Fable 5.1"), Some("high")),
            "the process runs what it chose, not its `--model`"
        );
        assert!(
            !facts.model_open && !facts.effort_open,
            "the card may not fill it"
        );
        assert_eq!(facts.owner.map(|o| o.session_id).as_deref(), Some(B));
        // NEGATIVE CONTROL: nothing kept of the process — the launch flag.
        assert_eq!(
            read_at(&mut TailCache::default(), "2026-09-28T15:04:30Z")
                .model
                .as_deref(),
            Some("Opus 5.5")
        );
        // NEGATIVE CONTROL: the process's facts WITHOUT the left session's
        // re-read miss the effort chosen after the last read.
        let mut stale = TailCache::default();
        registry(A, start);
        transcript(A, &CHOICE);
        read_at(&mut stale, "2026-09-28T15:04:20Z");
        transcript(A, &[CHOICE[0], CHOICE[1], EFFORT]);
        registry(B, start);
        let blind = stale.for_process(
            4242,
            Some(start),
            start,
            B,
            TailFacts::default(),
            Switch {
                now: start + 22,
                left: |_: &str, _: u64| (TailFacts::default(), false),
                launch_pinned: true,
                launch_resumed: false,
                restored: |_: Option<u64>| None,
            },
        );
        assert_eq!(blind.effort, Said::Unsaid);
        // A registry read that lands mid-write clears the TAIL, not the process.
        cache.clear();
        assert_eq!(
            read_at(&mut cache, "2026-09-28T15:05:00Z").model.as_deref(),
            Some("Fable 5.1")
        );
        // The new session speaks: its answer names the model.
        transcript(
            B,
            &[
                r#"{"type":"assistant","timestamp":"2026-09-28T15:10:00.000Z","effort":"high","message":{"model":"claude-haiku-4-5-20251001"}}"#,
            ],
        );
        assert_eq!(
            read_at(&mut cache, "2026-09-28T15:10:05Z").model.as_deref(),
            Some("Haiku 4.5")
        );
        // `/clear` again: the newest decision carries.
        registry(C, start);
        assert_eq!(
            read_at(&mut cache, "2026-09-28T15:10:10Z").model.as_deref(),
            Some("Haiku 4.5")
        );
        // An image exec'd in place registers a new `startedAt`: a new floor,
        // a new process for the footer — its own `--model`, nothing carried.
        registry(C, start + 3600);
        let facts = read_at(&mut cache, "2026-09-28T16:05:00Z");
        assert_eq!(facts.model.as_deref(), Some("Opus 5.5"));
        assert!(facts.effort_open);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_head_names_its_branch_or_a_short_commit() {
        assert_eq!(
            branch_of_head("ref: refs/heads/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            branch_of_head("ref: refs/heads/feat/footer\n").as_deref(),
            Some("feat/footer")
        );
        assert_eq!(
            branch_of_head("4a2573a96c0ffee4a2573a96c0ffee4a2573a96c\n").as_deref(),
            Some("4a2573a")
        );
        assert_eq!(branch_of_head("garbage"), None);
    }

    #[test]
    fn git_head_follows_a_worktree_file_and_climbs_to_the_top() {
        let root = std::env::temp_dir().join(format!("aterm-footer-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("repo");
        std::fs::create_dir_all(main.join(".git")).unwrap();
        std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(main.join("crates/deep")).unwrap();
        assert_eq!(git_head(&main.join("crates/deep")), Some("main".to_owned()));
        let wt_git = root.join("gitdirs/wt");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        let wt = root.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        assert_eq!(git_head(&wt), Some("feature".to_owned()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A kernel REFUSAL is its own answer, not "no repository": `EPERM` (what a
    /// macOS folder privacy consent returns) classifies as denied, while an
    /// ordinary `EACCES` permission bit and a missing file do not. Measured on
    /// the real primitive: a directory stripped of its search bit gives EACCES,
    /// which must stay `Absent` — only a consent refusal raises the host's
    /// consent attention.
    #[cfg(unix)]
    #[test]
    fn only_an_eperm_refusal_reads_as_denied() {
        assert!(is_eperm(&std::io::Error::from_raw_os_error(libc::EPERM)));
        assert!(!is_eperm(&std::io::Error::from_raw_os_error(libc::EACCES)));
        assert!(!is_eperm(&std::io::Error::from_raw_os_error(libc::ENOENT)));

        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("aterm-footer-eacces-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(read_git_head(&repo), GitHeadRead::Found("main".into()));
        std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let locked = read_git_head(&repo);
        std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            locked,
            GitHeadRead::Absent,
            "an EACCES permission bit is not a privacy-consent refusal"
        );
    }

    #[test]
    fn the_footer_is_three_marks_in_order_and_skips_what_it_lacks() {
        let full = FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        };
        assert_eq!(
            segments(&full),
            vec![
                Segment {
                    mark: MODEL_MARK,
                    text: "Opus 5.5 xhigh".into()
                },
                Segment {
                    mark: PATH_MARK,
                    text: "~/aterm".into()
                },
                Segment {
                    mark: BRANCH_MARK,
                    text: "main".into()
                },
            ]
        );
        let fresh = FooterFacts {
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        };
        assert_eq!(
            segments(&fresh).len(),
            2,
            "a value the facts lack is left out"
        );
    }

    #[test]
    fn a_long_path_keeps_its_last_directory() {
        assert_eq!(
            elide_path("~/src/github.com/aterm").as_deref(),
            Some("\u{2026}/aterm")
        );
        assert_eq!(elide_path("/opt/work/").as_deref(), Some("\u{2026}/work"));
        assert_eq!(elide_path("~/aterm"), None, "no shorter");
        assert_eq!(elide_path("~"), None);
        assert_eq!(elide_path("/"), None);
    }

    #[test]
    fn the_path_shows_home_as_a_tilde() {
        let home = Path::new("/Users//ana");
        let path = |cwd: &str| home_path(Path::new(cwd), Some(home));
        assert_eq!(path("/Users//ana/aterm").as_deref(), Some("~/aterm"));
        assert_eq!(path("/Users//ana/src/aterm").as_deref(), Some("~/src/aterm"));
        assert_eq!(path("/Users//ana").as_deref(), Some("~"));
        assert_eq!(path("/Users//anabel/x").as_deref(), Some("/Users//anabel/x"));
        assert_eq!(path("/opt/work").as_deref(), Some("/opt/work"));
        assert_eq!(
            home_path(Path::new("/w"), None).as_deref(),
            Some("/w"),
            "no home: the path whole"
        );
        assert_eq!(
            home_path(Path::new("/opt/work"), Some(Path::new("/"))).as_deref(),
            Some("/opt/work"),
            "a home of / abbreviates nothing"
        );
        assert_eq!(path("/Users//ana/a\nb"), None);
        assert_eq!(path("/Users//ana/a\u{1b}[2J"), None);
    }

    /// A LIMIT WALL goes FIRST and the tokens LAST: a narrow row drops values
    /// from the back, so it gives up the tokens first and the wall last. Both
    /// are absent, not empty, when nothing is known; each mark takes one
    /// cell, as the painter lays it out.
    #[test]
    fn the_wall_leads_and_the_usage_comes_last() {
        use crate::harness::session_usage::{ModelTokens, UsageFacts, Wall};
        let tokens = (
            vec![ModelTokens {
                label: "opus".into(),
                input: 1_200_000,
                output: 40_000,
            }],
            0,
        );
        let wall = Wall {
            label: "5h",
            resets: "3pm".into(),
        };
        let facts = FooterFacts {
            model: Some("Opus 5.5".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            usage: UsageFacts::of(tokens.clone(), Some(wall), None),
            ..FooterFacts::default()
        };
        let marks: Vec<char> = segments(&facts).iter().map(|s| s.mark).collect();
        assert_eq!(
            marks,
            [WALL_MARK, MODEL_MARK, PATH_MARK, BRANCH_MARK, USAGE_MARK]
        );
        let segs = segments(&facts);
        assert_eq!(segs[0].text, "5h limit \u{00B7} resets 3pm");
        assert_eq!(segs[4].text, "opus 1.2M in 40k out");
        for mark in [WALL_MARK, USAGE_MARK] {
            assert_eq!(aterm_grapheme::char_width(mark), 1, "{mark}");
        }
        let unwalled = FooterFacts {
            usage: UsageFacts::of(tokens, None, None),
            ..facts.clone()
        };
        assert_eq!(segments(&unwalled)[0].mark, MODEL_MARK);
        let none = FooterFacts {
            usage: None,
            ..facts
        };
        assert!(
            segments(&none)
                .iter()
                .all(|s| s.mark != USAGE_MARK && s.mark != WALL_MARK)
        );
    }

    /// END TO END over a temp Claude directory (never a real one): the fold
    /// reads the transcript THIS process's sessions file names — not the
    /// newer file a neighbour in the same project wrote — reads only what
    /// each call finds appended, sums the subagents, withholds tokens until
    /// it has caught up, and starts over for a new session under the same
    /// process (`/clear`). Tokens are the CONVERSATION's: a row written
    /// before this process started counts, while model and effort keep the
    /// process floor.
    #[cfg(unix)]
    #[test]
    fn the_folded_facts_are_this_sessions_transcript() {
        use crate::harness::session_usage::ModelTokens;
        let root = cache_test_dir("folded");
        let claude = root.join("claude");
        let project = claude.join("projects").join("-w-repo");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let started = crate::harness::upgrade_models::parse_utc("2026-09-27T12:00:00Z").unwrap();
        let name_session = |id: &str| {
            std::fs::write(
                claude.join("sessions/4242.json"),
                format!(
                    r#"{{"pid":4242,"sessionId":"{id}","cwd":"/w/repo","procStart":"{}"}}"#,
                    lstart_utc(started)
                ),
            )
            .unwrap();
        };
        name_session("abc-1");
        let row = |id: &str, model: &str, output: u64, ts: &str| {
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"effort\":\"xhigh\",\"message\":{{\"id\":\"{id}\",\"model\":\"{model}\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":{output},\"cache_read_input_tokens\":990}}}}}}\n"
            )
        };
        let append = |path: &Path, text: &str| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .unwrap();
        };
        let before = "2026-09-27T11:00:00Z";
        let during = "2026-09-27T13:00:00Z";
        let mine = project.join("abc-1.jsonl");
        std::fs::write(&mine, row("m0", "claude-opus-5-5", 2, before)).unwrap();
        append(&mine, &row("m1", "claude-opus-5-5", 40_000, during));
        std::fs::write(
            project.join("zzz-9.jsonl"),
            row("z1", "claude-opus-5-5", 9_999_999, during),
        )
        .unwrap();
        let place = |_: Option<&str>, _: i64| None;
        let mut cache = FooterCache::default();
        // The process's one transcript tail, as the window keeps it.
        let tail = std::cell::RefCell::new(TailCache::default());
        let read = |cache: &mut FooterCache| {
            folded(
                &claude,
                4242,
                Some(started),
                &mut tail.borrow_mut(),
                cache,
                0,
                &place,
            )
            .expect("a session")
        };
        let facts = read(&mut cache);
        assert_eq!(facts.model.as_deref(), Some("Opus 5.5"));
        assert_eq!(
            facts.usage.expect("tokens").models,
            vec![ModelTokens {
                label: "opus".into(),
                input: 2_000,
                output: 40_002
            }],
            "this session's whole conversation, not the neighbour's"
        );
        let usage = cache.usage.as_ref().expect("folding");
        assert!(!usage.moved(), "just read");
        let bytes = usage.bytes_read();
        let more = row("m2", "claude-haiku-4-5", 5, during);
        append(&mine, &more);
        let facts = read(&mut cache);
        assert_eq!(
            cache.usage.as_ref().expect("folding").bytes_read() - bytes,
            more.len() as u64 + 1,
            "the appended row and the boundary byte"
        );
        assert_eq!(facts.model.as_deref(), Some("Haiku 4.5"));
        assert_eq!(facts.usage.expect("usage").models.len(), 2);

        // A SUBAGENT still behind names no tokens — a prefix of a session is
        // not its usage — and the next reads carry on until caught up.
        let subs = project.join("abc-1").join("subagents");
        std::fs::create_dir_all(&subs).unwrap();
        let many: String = (0..40)
            .map(|i| row(&format!("s{i}"), "claude-haiku-4-5", 1, during))
            .collect();
        append(&subs.join("agent-a.jsonl"), &many);
        let mut short = FooterCache {
            budget: std::fs::metadata(&mine).unwrap().len() + 64,
            ..FooterCache::default()
        };
        assert_eq!(read(&mut short).usage, None, "a prefix is not shown");
        let mut reads = 1;
        let caught = loop {
            reads += 1;
            if let Some(usage) = read(&mut short).usage {
                break usage;
            }
            assert!(reads < 100, "it catches up");
        };
        assert_eq!(
            caught
                .models
                .iter()
                .find(|m| m.label == "haiku")
                .map(|m| m.output),
            Some(5 + 40),
            "the whole session once caught up"
        );

        // `/clear` under the same process: a new session's usage starts
        // from nothing.
        name_session("def-2");
        std::fs::write(
            project.join("def-2.jsonl"),
            row("n1", "claude-opus-5-5", 7, during),
        )
        .unwrap();
        assert_eq!(
            read(&mut cache).usage.expect("tokens").models,
            vec![ModelTokens {
                label: "opus".into(),
                input: 1_000,
                output: 7
            }],
            "the new session's transcript alone"
        );
        // A sessions file caught mid-write: no facts for that read, and the
        // fold is KEPT — the next good read folds nothing again.
        let sessions = claude.join("sessions/4242.json");
        let whole = std::fs::read_to_string(&sessions).unwrap();
        std::fs::write(&sessions, &whole[..whole.len() / 2]).unwrap();
        assert!(
            folded(
                &claude,
                4242,
                Some(started),
                &mut tail.borrow_mut(),
                &mut cache,
                0,
                &place,
            )
            .is_none()
        );
        let held = cache.usage.as_ref().expect("the fold is kept").bytes_read();
        std::fs::write(&sessions, &whole).unwrap();
        assert!(read(&mut cache).usage.is_some());
        assert_eq!(
            cache.usage.as_ref().expect("folding").bytes_read(),
            held,
            "nothing refolded after a torn read"
        );
        // The file rewritten again right after the read's one good parse:
        // the usage is that parse's session's, rather than Σ blinking off.
        let shown = read(&mut cache).usage;
        assert!(shown.is_some(), "control");
        let (facts, _) = read_torn_after_first(
            &claude,
            4242,
            Some(started),
            &mut tail.borrow_mut(),
            &mut cache,
            0,
            &place,
        );
        assert_eq!(
            facts.expect("the read's one parse was whole").usage,
            shown,
            "a rewrite after the read keeps the session's usage on the glass"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The fold is ONE conversation of ONE process. `/clear` names a new
    /// session whose transcript is not written until its first prompt, and
    /// a new Claude Code process in the same tab has its own sessions file:
    /// neither shows the last conversation's Σ or wall meanwhile — only a
    /// torn read of the SAME process's file keeps what it showed.
    #[cfg(unix)]
    #[test]
    fn a_new_conversation_or_process_never_shows_the_last_ones_usage() {
        let root = cache_test_dir("carry");
        let (claude, transcript, two_pm) = walled_session(&root);
        append_to(&transcript, &served_row("m0", two_pm - 600));
        let utc = |_: Option<&str>, _: i64| Some(0);
        let now = two_pm + 60;
        // A kernel start the caller knows (the files carry no `procStart`).
        let started = Some(1_700_000_000);
        // Each process's one transcript tail, as the window keeps them; the
        // tab's one fold.
        let tails = std::cell::RefCell::new(std::collections::HashMap::<u32, TailCache>::new());
        let usage_of = |pid: u32, cache: &mut FooterCache| {
            folded(
                &claude,
                pid,
                started,
                tails.borrow_mut().entry(pid).or_default(),
                cache,
                now,
                &utc,
            )
            .expect("a session")
            .usage
        };
        let name = |pid: u32, id: &str| {
            std::fs::write(
                claude.join(format!("sessions/{pid}.json")),
                format!(r#"{{"pid":{pid},"sessionId":"{id}","cwd":"/w/repo"}}"#),
            )
            .unwrap();
        };
        let mut cache = FooterCache::default();
        let shown = usage_of(4242, &mut cache).expect("control");
        assert!(shown.wall.is_some() && !shown.models.is_empty(), "control");

        // `/clear`: the same process names a session with no transcript yet.
        name(4242, "def-2");
        assert_eq!(usage_of(4242, &mut cache), None, "after /clear");
        name(4242, "abc-1");
        assert_eq!(usage_of(4242, &mut cache), Some(shown.clone()), "back");

        // A new process in the same tab, its session not written yet.
        name(5555, "ghi-3");
        assert_eq!(usage_of(5555, &mut cache), None, "a new process");
        // A torn read shows the fold for the process it was read for, and
        // for no other.
        let tear = |pid: u32| {
            let file = claude.join(format!("sessions/{pid}.json"));
            let whole = std::fs::read_to_string(&file).unwrap();
            std::fs::write(&file, &whole[..whole.len() / 2]).unwrap();
            move || std::fs::write(&file, &whole).unwrap()
        };
        assert!(usage_of(4242, &mut cache).is_some(), "control");
        let (facts, _) = read_torn_after_first(
            &claude,
            4242,
            started,
            tails.borrow_mut().entry(4242).or_default(),
            &mut cache,
            now,
            &utc,
        );
        assert_eq!(
            facts.expect("the read's one parse was whole").usage,
            Some(shown),
            "a torn read of the same process's file"
        );
        assert!(usage_of(4242, &mut cache).is_some(), "control");
        let _mend = tear(5555);
        for read in ["a torn read of another process's file", "nor the next"] {
            assert_eq!(
                folded(
                    &claude,
                    5555,
                    started,
                    tails.borrow_mut().entry(5555).or_default(),
                    &mut cache,
                    now,
                    &utc
                )
                .and_then(|f| f.usage),
                None,
                "{read}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `/clear` names a new session while Claude Code keeps rewriting its
    /// sessions file. One footer read parses that file ONCE, so a rewrite
    /// landing inside the read cannot hand the fold the file's torn bytes
    /// and bring back the previous conversation's Σ and limit wall beside
    /// the new session's facts. The MODEL is the process's, and stays: what
    /// it ran in the session it left carries across the `/clear`
    /// ([`a_clear_keeps_the_model_the_process_chose`]) — over the process's
    /// one tail, as the window reads it. Until the review of 2026-09-28 this
    /// test read each time over a fresh tail and asserted no model, which
    /// the window never shows.
    #[cfg(unix)]
    #[test]
    fn after_clear_a_torn_read_never_shows_the_last_conversations_usage() {
        let root = cache_test_dir("clear-torn");
        let (claude, transcript, two_pm) = walled_session(&root);
        append_to(&transcript, &served_row("m0", two_pm - 600));
        let utc = |_: Option<&str>, _: i64| Some(0);
        let now = two_pm + 60;
        let started = Some(1_700_000_000);
        let mut tail = TailCache::default();
        let mut cache = FooterCache::default();
        let before =
            folded(&claude, 4242, started, &mut tail, &mut cache, now, &utc).expect("control");
        assert_eq!(before.model.as_deref(), Some("Opus 5.5"), "control");
        let shown = before.usage.expect("control");
        assert!(shown.wall.is_some() && !shown.models.is_empty(), "control");

        std::fs::write(
            claude.join("sessions/4242.json"),
            r#"{"pid":4242,"sessionId":"def-2","cwd":"/w/repo"}"#,
        )
        .unwrap();
        let (facts, reads) =
            read_torn_after_first(&claude, 4242, started, &mut tail, &mut cache, now, &utc);
        let facts = facts.expect("the read's one parse was whole");
        assert_eq!(
            facts.usage, None,
            "no Σ and no wall from the conversation /clear left"
        );
        assert_eq!(
            facts.model.as_deref(),
            Some("Opus 5.5"),
            "the process's model carries across /clear"
        );
        assert_eq!(reads, 1, "one footer read parses the sessions file once");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `/resume` switches the process to ANOTHER conversation that has a
    /// transcript of its own: the model the footer shows is read from it,
    /// and so is the Σ beside it — never the new model with the old Σ.
    #[cfg(unix)]
    #[test]
    fn a_resume_to_another_session_never_pairs_its_model_with_the_old_usage() {
        use crate::harness::session_usage::ModelTokens;
        let root = cache_test_dir("resume-torn");
        let claude = root.join("claude");
        let project = claude.join("projects").join("-w-repo");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let started = crate::harness::upgrade_models::parse_utc("2026-09-27T12:00:00Z").unwrap();
        let name_session = |id: &str| {
            std::fs::write(
                claude.join("sessions/4242.json"),
                format!(
                    r#"{{"pid":4242,"sessionId":"{id}","cwd":"/w/repo","procStart":"{}"}}"#,
                    lstart_utc(started)
                ),
            )
            .unwrap();
        };
        let row = |id: &str, model: &str, output: u64| {
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"2026-09-27T13:00:00Z\",\"message\":{{\"id\":\"{id}\",\"model\":\"{model}\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":{output}}}}}}}\n"
            )
        };
        std::fs::write(
            project.join("abc-1.jsonl"),
            row("a1", "claude-opus-5-5", 40_000),
        )
        .unwrap();
        std::fs::write(
            project.join("def-2.jsonl"),
            row("d1", "claude-haiku-4-5", 7),
        )
        .unwrap();
        let place = |_: Option<&str>, _: i64| None;
        let mut tail = TailCache::default();
        let mut cache = FooterCache::default();
        name_session("abc-1");
        let facts = folded(
            &claude,
            4242,
            Some(started),
            &mut tail,
            &mut cache,
            0,
            &place,
        )
        .expect("control");
        assert_eq!(facts.model.as_deref(), Some("Opus 5.5"), "control");
        assert!(facts.usage.is_some(), "control");

        name_session("def-2");
        let (facts, reads) = read_torn_after_first(
            &claude,
            4242,
            Some(started),
            &mut tail,
            &mut cache,
            0,
            &place,
        );
        let facts = facts.expect("the read's one parse was whole");
        assert_eq!(facts.model.as_deref(), Some("Haiku 4.5"), "the resumed one");
        assert_eq!(
            facts.usage.map(|u| u.models),
            Some(vec![ModelTokens {
                label: "haiku".into(),
                input: 10,
                output: 7
            }]),
            "the Σ of the conversation the model is from"
        );
        assert_eq!(reads, 1, "one footer read parses the sessions file once");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A read that meets the sessions file mid-write keeps the fold of the
    /// session it last named — with or WITHOUT a kernel start time (Linux
    /// reads none): a rewrite inside the read does not blank Σ and the wall,
    /// and one that tears the read's only parse yields no facts, keeps the
    /// fold, and the next good read folds nothing again.
    #[cfg(unix)]
    #[test]
    fn a_torn_read_of_the_same_session_keeps_its_fold() {
        for started in [None, Some(1_700_000_000)] {
            let root = cache_test_dir("same-torn");
            let (claude, transcript, two_pm) = walled_session(&root);
            append_to(&transcript, &served_row("m0", two_pm - 600));
            let utc = |_: Option<&str>, _: i64| Some(0);
            let now = two_pm + 60;
            let mut cache = FooterCache::default();
            let tail = std::cell::RefCell::new(TailCache::default());
            let read = |cache: &mut FooterCache| {
                folded(
                    &claude,
                    4242,
                    started,
                    &mut tail.borrow_mut(),
                    cache,
                    now,
                    &utc,
                )
            };
            let shown = read(&mut cache).and_then(|f| f.usage).expect("control");
            assert!(shown.wall.is_some(), "control");

            let (facts, reads) = read_torn_after_first(
                &claude,
                4242,
                started,
                &mut tail.borrow_mut(),
                &mut cache,
                now,
                &utc,
            );
            assert_eq!(
                facts.expect("the read's one parse was whole").usage,
                Some(shown.clone()),
                "started {started:?}: Σ and the wall stay on the glass"
            );
            assert_eq!(reads, 1, "one footer read parses the sessions file once");

            let sessions = claude.join("sessions/4242.json");
            let whole = std::fs::read_to_string(&sessions).unwrap();
            let held = cache.usage.as_ref().expect("folding").bytes_read();
            std::fs::write(&sessions, &whole[..whole.len() / 2]).unwrap();
            assert_eq!(read(&mut cache), None, "no facts from a torn parse");
            std::fs::write(&sessions, &whole).unwrap();
            assert_eq!(read(&mut cache).and_then(|f| f.usage), Some(shown));
            assert_eq!(
                cache.usage.as_ref().expect("the fold is kept").bytes_read(),
                held,
                "nothing refolded after a torn read"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// The fold in a tab's cache is one process's conversation: a torn read
    /// of ANOTHER process's sessions file (a new Claude Code in the same
    /// tab) shows nothing of it — neither when the read's only parse is
    /// torn nor when the file is rewritten inside the read — and neither
    /// does that process's next good read before its first prompt.
    #[cfg(unix)]
    #[test]
    fn a_torn_read_of_another_processs_file_shows_nothing() {
        let root = cache_test_dir("other-torn");
        let (claude, transcript, two_pm) = walled_session(&root);
        append_to(&transcript, &served_row("m0", two_pm - 600));
        let utc = |_: Option<&str>, _: i64| Some(0);
        let now = two_pm + 60;
        let started = Some(1_700_000_000);
        let mut cache = FooterCache::default();
        let tails = std::cell::RefCell::new(std::collections::HashMap::<u32, TailCache>::new());
        let usage_of = |pid: u32, cache: &mut FooterCache| {
            folded(
                &claude,
                pid,
                started,
                tails.borrow_mut().entry(pid).or_default(),
                cache,
                now,
                &utc,
            )
            .map(|f| f.usage)
        };
        assert!(
            usage_of(4242, &mut cache).flatten().is_some(),
            "control: the tab's cache holds 4242's fold"
        );
        let other = claude.join("sessions/5555.json");
        let whole = r#"{"pid":5555,"sessionId":"ghi-3","cwd":"/w/repo"}"#;
        std::fs::write(&other, &whole[..whole.len() / 2]).unwrap();
        assert_eq!(usage_of(5555, &mut cache), None, "its only parse torn");
        std::fs::write(&other, whole).unwrap();
        let (facts, reads) = read_torn_after_first(
            &claude,
            5555,
            started,
            tails.borrow_mut().entry(5555).or_default(),
            &mut cache,
            now,
            &utc,
        );
        assert_eq!(
            facts.expect("the read's one parse was whole").usage,
            None,
            "rewritten inside the read"
        );
        assert_eq!(reads, 1, "one footer read parses the sessions file once");
        assert_eq!(usage_of(5555, &mut cache), Some(None), "its next good read");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// ONE footer read ([`read_pid`]) LOOKS ITS TRANSCRIPT UP ONCE
    /// ([`transcript_path`]): the tail the model is read from and the fold
    /// the usage is read from are the same file, named by one lookup. Until
    /// 2026-09-29 the read looked it up twice — once for the fold, once again
    /// inside [`facts_for_entry_at`] — so every refresh stat'ed the slug's
    /// path twice and, where the slug missed (a vendor spelling change),
    /// scanned every project directory twice. Both shapes are read here: the
    /// slug's own directory, and one the slug no longer names. The count
    /// excludes nothing: a session switch's lookup of the session it LEFT
    /// is another conversation's, and no switch happens in these reads.
    #[cfg(unix)]
    #[test]
    fn one_footer_read_looks_its_transcript_up_once() {
        let root = cache_test_dir("one-lookup");
        let (claude, transcript, two_pm) = walled_session(&root);
        append_to(&transcript, &served_row("m0", two_pm - 600));
        let utc = |_: Option<&str>, _: i64| Some(0);
        // A kernel start the caller knows: the transcript's rows speak for
        // the process, so the tail is read as well as the fold.
        let started = Some(1_700_000_000);
        let read = |claude: &Path| {
            TRANSCRIPT_LOOKUPS.with(|n| n.set(0));
            let facts = folded(
                claude,
                4242,
                started,
                &mut TailCache::default(),
                &mut FooterCache::default(),
                two_pm + 60,
                &utc,
            )
            .expect("a session");
            (facts, TRANSCRIPT_LOOKUPS.with(std::cell::Cell::get))
        };
        let (facts, lookups) = read(&claude);
        assert_eq!(facts.model.as_deref(), Some("Opus 5.5"), "the tail read it");
        assert!(facts.usage.is_some(), "the fold read it");
        assert_eq!(lookups, 1, "one lookup, the slug's own directory");

        // The slug misses: the scan of `projects/` finds the file, once.
        let projects = claude.join("projects");
        std::fs::rename(projects.join("-w-repo"), projects.join("-w-repo-moved")).unwrap();
        let (facts, lookups) = read(&claude);
        assert_eq!(
            facts.model.as_deref(),
            Some("Opus 5.5"),
            "found by the scan"
        );
        assert!(
            facts.usage.is_some(),
            "the fold read the file the scan found"
        );
        assert_eq!(lookups, 1, "one lookup, one scan of every project");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A Claude directory under `root` whose process 4242 runs session
    /// `abc-1`, its transcript holding one session-limit notice written at
    /// 14:00 UTC (`resets 3pm (UTC)`): the directory, the transcript, 14:00.
    #[cfg(unix)]
    fn walled_session(root: &Path) -> (PathBuf, PathBuf, i64) {
        let claude = root.join("claude");
        let project = claude.join("projects").join("-w-repo");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            claude.join("sessions/4242.json"),
            r#"{"pid":4242,"sessionId":"abc-1","cwd":"/w/repo"}"#,
        )
        .unwrap();
        let two_pm = 1_790_000_000 - 1_790_000_000 % 86_400 + 14 * 3600;
        let transcript = project.join("abc-1.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{{\"id\":\"n1\",\"model\":\"<synthetic>\",\"content\":[{{\"type\":\"text\",\"text\":\"You've hit your session limit \u{00b7} resets 3pm (UTC)\"}}],\"usage\":{{\"input_tokens\":0,\"output_tokens\":0}}}}}}\n",
                crate::harness::usage::rfc3339_utc(two_pm)
            ),
        )
        .unwrap();
        (claude, transcript, two_pm)
    }

    /// A response the API served, written at `at`, with its newline.
    #[cfg(unix)]
    fn served_row(id: &str, at: i64) -> String {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-opus-5-5\",\"usage\":{{\"input_tokens\":100,\"output_tokens\":50}}}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        )
    }

    /// The wall segment's text `claude`'s process 4242 shows at `now`, the
    /// clock UTC. No start is known (neither the kernel's nor a `procStart`),
    /// so no transcript row speaks for the process ([`floor_of`]) and every
    /// read clears its tail: a fresh one is the window's.
    #[cfg(unix)]
    fn wall_at(claude: &Path, cache: &mut FooterCache, now: i64) -> Option<String> {
        let utc = |_: Option<&str>, _: i64| Some(0);
        folded(
            claude,
            4242,
            None,
            &mut TailCache::default(),
            cache,
            now,
            &utc,
        )
        .and_then(|f| segments(&f).into_iter().find(|s| s.mark == WALL_MARK))
        .map(|s| s.text)
    }

    #[cfg(unix)]
    fn append_to(path: &Path, text: &str) {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .unwrap();
    }

    /// The limit is the ACCOUNT's: a background workflow's agent served
    /// after the main transcript's notice (the main thread idle) takes the
    /// wall down as the main thread's own response would. One served BEFORE
    /// the notice does not.
    #[cfg(unix)]
    #[test]
    fn a_response_a_subagent_was_served_since_takes_the_wall_down() {
        let root = cache_test_dir("wall-sub");
        let (claude, transcript, two_pm) = walled_session(&root);
        let agent = transcript
            .with_extension("")
            .join("subagents/workflows/wf1/agent-a1.jsonl");
        std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
        append_to(&agent, &served_row("w0", two_pm - 60));
        let mut cache = FooterCache::default();
        assert!(
            wall_at(&claude, &mut cache, two_pm + 300).is_some(),
            "served before the notice: the wall stands"
        );
        append_to(&agent, &served_row("w1", two_pm + 600));
        assert_eq!(
            wall_at(&claude, &mut cache, two_pm + 700),
            None,
            "a workflow's agent served since"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A fold still BEHIND shows no wall it has not caught up to: a first
    /// read cut by its budget between a notice and the response served
    /// after it holds the notice alone, and must not put a lifted wall on
    /// the glass. A wall a caught-up read found is kept while a later read
    /// is behind.
    #[cfg(unix)]
    #[test]
    fn a_fold_still_behind_shows_no_wall_it_has_not_caught_up_to() {
        let root = cache_test_dir("wall-behind");
        let (claude, transcript, two_pm) = walled_session(&root);
        let notice_len = std::fs::metadata(&transcript).unwrap().len();
        append_to(&transcript, &served_row("m1", two_pm + 600));
        let mut cache = FooterCache {
            budget: notice_len + 8,
            ..FooterCache::default()
        };
        assert_eq!(
            wall_at(&claude, &mut cache, two_pm + 700),
            None,
            "a prefix holding the notice but not the response that lifted it"
        );
        for _ in 0..8 {
            assert_eq!(wall_at(&claude, &mut cache, two_pm + 700), None);
        }
        assert!(cache.refreshed.is_some_and(|r| r.caught_up), "caught up");

        let root2 = cache_test_dir("wall-kept");
        let (claude, transcript, two_pm) = walled_session(&root2);
        let mut cache = FooterCache::default();
        assert!(
            wall_at(&claude, &mut cache, two_pm + 60).is_some(),
            "control"
        );
        cache.budget = 64;
        let filler: String = (0..20)
            .map(|i| served_row(&format!("x{i}"), two_pm - 3600))
            .collect();
        append_to(&transcript, &filler);
        assert!(
            wall_at(&claude, &mut cache, two_pm + 120).is_some(),
            "behind: the caught-up read's wall stands"
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(root2).unwrap();
    }

    /// END TO END: the limit wall is the notice Claude Code wrote into THIS
    /// session's transcript, its reset placed from the row's own time and
    /// shown only while it is still ahead — read the next day it stays gone
    /// — and gone once a response is served after it. A row whose reset
    /// cannot be placed shows nothing.
    #[cfg(unix)]
    #[test]
    fn the_limit_wall_is_read_from_the_transcripts_row() {
        let root = cache_test_dir("wall");
        let claude = root.join("claude");
        let project = claude.join("projects").join("-w-repo");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            claude.join("sessions/4242.json"),
            r#"{"pid":4242,"sessionId":"abc-1","cwd":"/w/repo"}"#,
        )
        .unwrap();
        let day0 = 1_790_000_000 - 1_790_000_000 % 86_400;
        let two_pm = day0 + 14 * 3600;
        let notice = |reset: &str| {
            let stamp = crate::harness::usage::rfc3339_utc(two_pm);
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{stamp}\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{{\"id\":\"n1\",\"model\":\"<synthetic>\",\"content\":[{{\"type\":\"text\",\"text\":\"You've hit your session limit \u{00b7} resets {reset}\"}}],\"usage\":{{\"input_tokens\":0,\"output_tokens\":0}}}}}}\n"
            )
        };
        let transcript = project.join("abc-1.jsonl");
        std::fs::write(&transcript, notice("3pm (UTC)")).unwrap();
        let utc = |_: Option<&str>, _: i64| Some(0);
        // No start known: the tail holds nothing ([`wall_at`]).
        let wall = |cache: &mut FooterCache, now: i64| {
            folded(
                &claude,
                4242,
                None,
                &mut TailCache::default(),
                cache,
                now,
                &utc,
            )
            .and_then(|f| segments(&f).into_iter().find(|s| s.mark == WALL_MARK))
            .map(|s| s.text)
        };
        let mut cache = FooterCache::default();
        assert_eq!(
            wall(&mut cache, two_pm + 60).as_deref(),
            Some("5h limit \u{00B7} resets 3pm"),
            "the vendor's reset, its zone left off"
        );
        assert!(wall(&mut cache, day0 + 15 * 3600 - 1).is_some());
        assert_eq!(wall(&mut cache, day0 + 15 * 3600), None, "reset: gone");
        assert_eq!(
            wall(&mut FooterCache::default(), day0 + 86_400 + 11 * 3600),
            None,
            "read the next morning: the row is day 0's, its 3pm has passed"
        );
        // `/limit-reset`: a response served after the notice takes it down.
        let mut cache = FooterCache::default();
        assert!(wall(&mut cache, two_pm + 300).is_some(), "control");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .and_then(|mut f| {
                f.write_all(
                    format!(
                        "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"id\":\"m1\",\"model\":\"claude-opus-5-5\",\"usage\":{{\"input_tokens\":100,\"output_tokens\":50}}}}}}\n",
                        crate::harness::usage::rfc3339_utc(two_pm + 600)
                    )
                    .as_bytes(),
                )
            })
            .unwrap();
        assert_eq!(wall(&mut cache, two_pm + 700), None, "served since");
        std::fs::write(&transcript, notice("when it does")).unwrap();
        assert_eq!(
            wall(&mut FooterCache::default(), two_pm + 60),
            None,
            "a reset that cannot be placed: nothing, never a guess"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The rows are verbatim from the phase fixtures and the 2.1.282 probe:
    /// every known pill opens a mode row, whatever live status follows it;
    /// a `(… to cycle)` hint cut MID-WORD before its `)` does not (a narrow
    /// pane cuts it only between words: the next tests).
    #[test]
    fn a_known_pill_opens_a_mode_row() {
        for row in [
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents",
            "  \u{23F5}\u{23F5} bypass permissions on \u{00B7} 1 monitor \u{00B7} \u{2190} for agents \u{00B7} \u{2193} to manage",
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interrupt \u{00B7} \u{2190} for agents        /rc active",
            "  \u{23F8} plan mode on (shift+tab to cycle)",
            "  \u{23F8} manual mode on (shift+tab to cycle)",
            "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)",
            "  \u{23F5}\u{23F5} don't ask on",
        ] {
            assert!(is_mode_row(row), "{row:?}");
        }
        assert!(!is_mode_row(
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cy"
        ));
    }

    /// What 2.1.284 draws on the mode row of a narrow pane (NARROW PANES in
    /// the module doc): the pill's text wrapped by word at `columns - 4`,
    /// first line only. Each row is that renderer's output at the width
    /// named, transcribed from its code — wrap-ansi's word loop, Ink's
    /// measure and `…` truncation — not a capture (main's 0885b3465, ported
    /// from its painter's planner to this reader: aterm writes nothing on the
    /// row, and the row must still be FOUND as the mode row).
    #[test]
    fn a_narrow_pane_cuts_the_hint_between_words_and_the_row_is_still_the_mode_row() {
        let cases: [(&str, &str); 5] = [
            // 39 and 42 columns.
            (
                "  \u{23F5}\u{23F5} bypass permissions on (shift+tab",
                "bypass permissions",
            ),
            (
                "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to",
                "bypass permissions",
            ),
            // 30 columns, as the grid holds it: padded to the pane's width.
            ("  \u{23F5}\u{23F5} auto mode on (shift+tab  ", "auto mode"),
            // 32 columns: an unexpected mode's pill, its hint cut.
            ("  \u{23F8} plan mode on (shift+tab to", "plan mode"),
            // 30 columns: the hint wrapped away whole, a blank left behind.
            (
                "  \u{23F5}\u{23F5} bypass permissions on ",
                "bypass permissions",
            ),
        ];
        for (row, indicator) in cases {
            assert!(is_mode_row(row), "{row:?}");
            assert_eq!(pill_indicator(row), Some(indicator), "{row:?}");
        }
        let rule = "\u{2500}".repeat(38);
        let screen: Vec<String> = vec![
            rule.clone(),
            "\u{276F} ".into(),
            rule,
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab".into(),
        ];
        assert_eq!(
            mode_row(&screen),
            Some(3),
            "the cut row is still the mode row: the lights read its mode"
        );
        assert_eq!(live_repl_row(&screen), Some(3), "and the REPL is live");
        // CONTROL: the rule as it stood before the port (a hint after the
        // pill must be closed) finds no mode row in that pane.
        let closed_only = |row: &str| {
            row.strip_prefix("  ").is_some_and(|rest| {
                PILLS.iter().any(|(glyph, indicator)| {
                    rest.strip_prefix(&format!("{glyph} {indicator} on"))
                        .is_some_and(|after| {
                            after
                                .strip_prefix(" (")
                                .is_none_or(|hint| hint.contains(')'))
                        })
                })
            })
        };
        assert!(!closed_only(&screen[3]), "the old rule missed it");
    }

    /// A narrow row's gaps and cut items, as 2.1.284 lays them out: each is
    /// still the mode row. The items stay exactly as Claude drew them — aterm
    /// writes into the rule above, never onto this row — so a live item cut
    /// short (`esc to interru…`, `←…` of `← 1 agent`) is never dropped as a
    /// static hint here (the cut-item half of main's f2616a967 holds by
    /// construction). Exactly those shapes: a parenthesis that is neither the
    /// hint nor its cut between words keeps the row no mode row, as a hint
    /// cut mid-word does.
    #[test]
    fn a_narrow_panes_gaps_and_cut_items_leave_the_row_the_mode_row() {
        for row in [
            // 45 and 48 columns, `← for agents` behind the cut hint.
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to  \u{00B7}",
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to  \u{00B7} \u{2190}\u{2026}",
            // 37: the hint wrapped away; the separator past the wide line's gap.
            "  \u{23F5}\u{23F5} bypass permissions on  \u{00B7} \u{2190} fo\u{2026}",
            // 26: auto's second wrapped line is the wider one.
            "  \u{23F5}\u{23F5} auto mode on      \u{00B7}",
            // 51: the whole hint, and a separator the row's edge cut.
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7}",
            // 50, 58 and 66, busy: live status cut short.
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to\u{2026}",
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interru\u{2026}",
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interrupt \u{00B7} \u{2190} f\u{2026}",
            // 31, busy, the separator in the truncating text itself.
            "  \u{23F5}\u{23F5} bypass permissions on \u{00B7}\u{2026}",
            // Cut items after live status (`← 1 agent` cut to its arrow).
            "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} \u{2190}\u{2026}",
            "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} \u{2190} \u{2026}",
            "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} ?\u{2026}",
            // A closed parenthesis that is no hint.
            "  \u{23F5}\u{23F5} bypass permissions on (beta)",
        ] {
            assert!(is_mode_row(row), "{row:?}");
        }
        for row in [
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to go",
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cy",
            "  \u{23F5}\u{23F5} bypass permissions on ((shift+tab",
        ] {
            assert!(!is_mode_row(row), "{row:?}");
        }
    }

    /// The pill-less row says no mode: default mode's hint turns into `esc
    /// to clear` as soon as the composer holds text. It is no mode row — the
    /// lights read no mode from it — but the footer's rule above it is
    /// written all the same (the rule is found by structure).
    #[test]
    fn a_row_without_a_pill_is_no_mode_row() {
        assert!(!is_mode_row("  ? for shortcuts"));
        assert!(!is_mode_row("  esc to clear"));
        assert!(!is_mode_row("  ! for shell mode"));
    }

    /// `procStart` is `ps -o lstart=` in UTC; the guard compares it to the
    /// kernel's start time rendered the same way.
    #[test]
    fn a_reused_pid_is_refused_by_its_start_time() {
        // 1790194435 = Wed Sep 23 2026 20:13:55 UTC (the SESSION_9162 fixture).
        assert_eq!(lstart_utc(1_790_194_435), "Wed Sep 23 20:13:55 2026");
        assert_eq!(lstart_utc(0), "Thu Jan 1 00:00:00 1970");
        let text = r#"{"pid":9162,"sessionId":"03396a15-856e","cwd":"/w","procStart":"Thu Sep  3 20:13:55 2026"}"#;
        let sep3 = 1_790_194_435 - 20 * 86_400;
        assert!(
            parse_session_entry(text, 9162, Some(sep3)).is_some(),
            "`ps` pads a one-digit day; the compare squashes it"
        );
        assert_eq!(
            parse_session_entry(text, 9162, Some(sep3 + 1)),
            None,
            "a different process that holds the same pid"
        );
        assert!(
            parse_session_entry(text, 9162, None).is_some(),
            "no kernel answer: the pid match stands alone"
        );
        assert_eq!(
            parse_session_entry(text, 9162, None).and_then(|e| e.proc_start),
            Some(sep3),
            "the registry's start is kept as the transcript floor"
        );
    }

    #[test]
    fn the_claude_dir_is_the_processs_own() {
        assert_eq!(
            claude_dir_of(Some("/cfg/claude"), Some("/Users//ana")),
            Some(PathBuf::from("/cfg/claude"))
        );
        assert_eq!(
            claude_dir_of(Some(""), Some("/Users//ana")),
            Some(PathBuf::from("/Users//ana/.claude")),
            "an empty override is no override"
        );
        assert_eq!(claude_dir_of(Some("rel/dir"), Some("/Users//ana")), None);
        assert_eq!(claude_dir_of(None, None), None);
    }

    /// A FIFO where `HEAD` should be must not park the reader thread.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_not_read() {
        let dir = std::env::temp_dir().join(format!("aterm-footer-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("HEAD");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path we own; mkfifo only creates a node.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(read_small(&fifo, MAX_GIT_FILE_BYTES), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pill glyph with an indicator this build does not know is the
    /// vendor's drift, and is told apart from rows that are no pill at all.
    #[test]
    fn a_renamed_pill_still_opens_with_a_pill_glyph() {
        let renamed = "  \u{23F5}\u{23F5} turbo mode on (shift+tab to cycle)";
        assert!(opens_with_pill_glyph(renamed));
        assert_eq!(pill_indicator(renamed), None);
        assert!(opens_with_pill_glyph("  \u{23F8} manual mode on"));
        assert!(!opens_with_pill_glyph("  ? for shortcuts"));
        assert!(!opens_with_pill_glyph(
            "    \u{23F5}\u{23F5} indented tool output"
        ));
        // A narrow pane's cut of a known pill is geometry, not drift.
        assert!(is_cut_pill("  \u{23F5}\u{23F5} bypass permissi"));
        assert!(is_cut_pill("  \u{23F5}\u{23F5} bypass permissions o"));
        assert!(!is_cut_pill(renamed));
    }

    #[test]
    fn nothing_else_is_a_mode_row() {
        assert!(!is_mode_row("\u{276F} Try \"refactor gpu_matches_cpu.rs\""));
        assert!(!is_mode_row("  bypass permissions on"), "no pill glyph");
        assert!(
            !is_mode_row("    \u{23F5}\u{23F5} bypass permissions on"),
            "tool output is indented deeper"
        );
        assert!(
            !is_mode_row("  \u{23F8} auto mode on"),
            "the glyph pairs with its indicator"
        );
    }

    /// The rule's cells as a screen's TEXT shows them: a `─` is a plain rule
    /// glyph, anything else is ink. (The host reads colours too: a `─` in
    /// another colour is ink there.)
    fn coverable(row: &str, width: usize) -> Vec<bool> {
        let mut out: Vec<bool> = row.chars().map(|c| c == '\u{2500}').collect();
        out.resize(width, false);
        out
    }

    /// EVERY RECORDED CLAUDE CODE SCREEN in `aterm-phase`'s fixtures: a
    /// screen with a composer has a bottom rule the footer can write, whose
    /// whole run is plain glyphs, and room for the owner's footer at the
    /// fixture's own width (80 columns included); the mode row, where there
    /// is one, is under it. A screen with a box up has none.
    #[test]
    fn every_recorded_screen_has_a_rule_the_footer_can_write() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
        let facts = FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        };
        let mut ruled = 0;
        for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            let Some(bottom) = aterm_phase::phase::composer_bottom(&rows) else {
                continue;
            };
            let rule = &rows[bottom];
            let width = rule.chars().count();
            let run = rule_run(&coverable(rule, width))
                .unwrap_or_else(|| panic!("{}: no plain glyph in {rule:?}", path.display()));
            let room = rule_room(&run);
            if width >= 80 {
                assert_eq!(
                    fit_rule(&facts, room, None).segments,
                    segments(&facts),
                    "{}: the whole footer fits a {width}-column rule",
                    path.display()
                );
            }
            if let Some(mode) = mode_row(&rows) {
                assert!(mode > bottom && is_mode_row(&rows[mode]));
            }
            ruled += 1;
        }
        assert!(ruled >= 10, "the corpus must exercise the rule: {ruled}");
    }

    /// The run is the widest of plain glyphs, the rightmost of equals; any
    /// other ink bounds it. What aterm writes leaves [`RULE_LEAD`] glyphs at
    /// its left and one at its right.
    #[test]
    fn the_rule_run_is_the_widest_plain_run() {
        let row = |s: &str| s.chars().map(|c| c == '-').collect::<Vec<_>>();
        assert_eq!(rule_run(&row("--------")), Some(0..8));
        assert_eq!(rule_run(&row("---x------")), Some(4..10));
        assert_eq!(
            rule_run(&row("----x----")),
            Some(5..9),
            "the rightmost of equals"
        );
        assert_eq!(rule_run(&row("xxxx")), None);
        assert_eq!(rule_run(&[]), None);
        assert_eq!(rule_room(&(0..80)), 76);
        assert_eq!(rule_room(&(0..3)), 0);
    }

    /// A LIGHT'S REASON NEVER TAKES A LIMIT WALL'S ROOM (the merge review of
    /// 2026-09-28: the rule [`fit_rule`] states — a reason may take the
    /// session's tokens' room, the branch's and the path's, never a limit
    /// wall's, the model's or the effort's — had no test, every lights test
    /// using facts with no usage). Over the owner's facts WITH a limit wall
    /// and the session's tokens, at every width: each rule a reason is drawn
    /// in (its title shown) keeps the wall and the model-and-effort segment
    /// whole; the tokens, the branch and the path are each given up for it
    /// somewhere; and where the reason does not fit even beside the wall and
    /// the model, it goes and the facts come back. NEGATIVE CONTROL: a
    /// hovered title (no reason) takes no fact's room at all.
    #[test]
    fn a_lights_reason_never_takes_a_limit_walls_room() {
        use crate::harness::session_usage::{ModelTokens, UsageFacts, Wall};
        use std::collections::BTreeSet;
        let facts = FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/src/github.com/aterm".into()),
            branch: Some("main".into()),
            usage: UsageFacts::of(
                (
                    vec![ModelTokens {
                        label: "opus".into(),
                        input: 48_000_000,
                        output: 310_000,
                    }],
                    0,
                ),
                Some(Wall {
                    label: "5h",
                    resets: "3pm".into(),
                }),
                None,
            ),
            ..FooterFacts::default()
        };
        let whole = segments(&facts);
        let wall = whole
            .iter()
            .find(|s| s.mark == WALL_MARK)
            .cloned()
            .expect("a wall");
        let model = whole
            .iter()
            .find(|s| s.mark == MODEL_MARK)
            .cloned()
            .expect("the model");
        assert_eq!(model.text, "Opus 5.5 xhigh");
        let reason = Some(LightsWidth {
            chips: 13,
            title: 50,
            short: 35,
            reason: true,
        });
        let mut given_up: BTreeSet<char> = BTreeSet::new();
        let mut narrowest_titled: Option<usize> = None;
        let mut came_back = false;
        for room in (0..=240).rev() {
            let fit = fit_rule(&facts, room, reason);
            assert!(fit.width(reason) <= room, "{room}: {fit:?}");
            if fit.title == TitleFit::None {
                if narrowest_titled.is_some_and(|n| fit.segments.len() > n) {
                    came_back = true;
                }
                continue;
            }
            assert!(
                fit.segments.contains(&wall) && fit.segments.contains(&model),
                "{room}: a reason never takes the wall's or the model's room: {fit:?}"
            );
            for mark in [USAGE_MARK, BRANCH_MARK, PATH_MARK] {
                if !fit.segments.iter().any(|s| s.mark == mark) {
                    given_up.insert(mark);
                }
            }
            narrowest_titled = Some(fit.segments.len());
        }
        assert_eq!(
            given_up,
            BTreeSet::from([USAGE_MARK, BRANCH_MARK, PATH_MARK]),
            "the reason takes the tokens', the branch's and the path's room"
        );
        assert_eq!(narrowest_titled, Some(2), "beside the wall and the model");
        assert!(came_back, "a reason too wide goes, and the facts come back");
        // NEGATIVE CONTROL: a hovered title takes nothing from the facts.
        let hover = reason.map(|l| LightsWidth { reason: false, ..l });
        for room in 0..=240 {
            let fit = fit_rule(&facts, room, hover);
            assert!(
                fit.title == TitleFit::None || fit.segments == whole,
                "{room}: {fit:?}"
            );
        }
    }

    /// THE ORDER OF GIVING WAY ([`fit_rule`]; the owner decided only that
    /// the facts go into the rule and fit at any width): a light's transient
    /// title first (whole, then short, then none), then the path is cut to
    /// `…/<dir>`, the branch goes, the path, the effort, then the chips; the
    /// model last. A light's REASON outranks the branch and the path — after
    /// the whole title and the path's head go — but never the model and
    /// effort.
    #[test]
    fn a_narrow_rule_gives_way_title_first_and_the_model_last() {
        let facts = FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/src/github.com/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        };
        let texts = |fit: &RuleFit| -> Vec<String> {
            fit.segments
                .iter()
                .map(|s| format!("{} {}", s.mark, s.text))
                .collect()
        };
        let full = segments_width(&segments(&facts)) + 2;
        assert_eq!(full, 54);
        let mut seen: Vec<Vec<String>> = Vec::new();
        for room in (0..=full).rev() {
            let fit = fit_rule(&facts, room, None);
            assert!(fit.width(None) <= room, "{room}: {fit:?}");
            let t = texts(&fit);
            if seen.last() != Some(&t) {
                seen.push(t);
            }
        }
        assert_eq!(
            seen,
            [
                vec![
                    "\u{25C6} Opus 5.5 xhigh",
                    "\u{2302} ~/src/github.com/aterm",
                    "\u{2387} main"
                ],
                vec![
                    "\u{25C6} Opus 5.5 xhigh",
                    "\u{2302} \u{2026}/aterm",
                    "\u{2387} main"
                ],
                vec!["\u{25C6} Opus 5.5 xhigh", "\u{2302} \u{2026}/aterm"],
                vec!["\u{25C6} Opus 5.5 xhigh"],
                vec!["\u{25C6} Opus 5.5"],
                Vec::<&str>::new(),
            ]
            .map(|v| v.into_iter().map(str::to_owned).collect::<Vec<_>>())
        );
        // With a light drawn and titled: the title gives way first, then the
        // facts, then the chips; the model outlasts them all.
        let lights = Some(LightsWidth {
            chips: 13,
            title: 35,
            short: 20,
            reason: false,
        });
        let steps: Vec<(usize, TitleFit, bool)> = (0..=120)
            .rev()
            .map(|room| {
                let fit = fit_rule(&facts, room, lights);
                assert!(fit.width(lights) <= room, "{room}: {fit:?}");
                (fit.segments.len(), fit.title, fit.chips)
            })
            .fold(Vec::new(), |mut acc, step| {
                if acc.last() != Some(&step) {
                    acc.push(step);
                }
                acc
            });
        assert_eq!(
            steps,
            [
                (3, TitleFit::Full, true),
                (3, TitleFit::Short, true),
                (3, TitleFit::None, true),
                (2, TitleFit::None, true),
                (1, TitleFit::None, true),
                (1, TitleFit::None, false),
                (0, TitleFit::None, false),
            ]
        );
        // At 80 columns (room 76) a hovered light takes nothing from the
        // facts of the owner's own footer: the title gives way instead.
        let owners = FooterFacts {
            path: Some("~/aterm".into()),
            ..facts.clone()
        };
        let fit = fit_rule(&owners, rule_room(&(0..80)), lights);
        assert_eq!(fit.segments, segments(&owners), "{fit:?}");
        assert!(fit.chips && fit.title != TitleFit::Full, "{fit:?}");
        // A REASON: the whole title goes, then the path's head; then the
        // reason takes the branch's and the path's room, never the model's
        // or the effort's — too long even beside those, it goes, and the
        // facts come back.
        let reason = lights.map(|l| LightsWidth {
            reason: true,
            title: 50,
            short: 35,
            ..l
        });
        let steps: Vec<(Vec<String>, TitleFit, bool)> = (0..=130)
            .rev()
            .map(|room| {
                let fit = fit_rule(&facts, room, reason);
                assert!(fit.width(reason) <= room, "{room}: {fit:?}");
                (texts(&fit), fit.title, fit.chips)
            })
            .fold(Vec::new(), |mut acc, step| {
                if acc.last() != Some(&step) {
                    acc.push(step);
                }
                acc
            });
        let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let (model_effort, cut, branch, path) = (
            "\u{25C6} Opus 5.5 xhigh",
            "\u{2302} \u{2026}/aterm",
            "\u{2387} main",
            "\u{2302} ~/src/github.com/aterm",
        );
        assert_eq!(
            steps,
            [
                (owned(&[model_effort, path, branch]), TitleFit::Full, true),
                (owned(&[model_effort, path, branch]), TitleFit::Short, true),
                (owned(&[model_effort, cut, branch]), TitleFit::Short, true),
                (owned(&[model_effort, cut]), TitleFit::Short, true),
                (owned(&[model_effort]), TitleFit::Short, true),
                (owned(&[model_effort, cut, branch]), TitleFit::None, true),
                (owned(&[model_effort, cut]), TitleFit::None, true),
                (owned(&[model_effort]), TitleFit::None, true),
                (owned(&["\u{25C6} Opus 5.5"]), TitleFit::None, true),
                (owned(&["\u{25C6} Opus 5.5"]), TitleFit::None, false),
                (Vec::new(), TitleFit::None, false),
            ]
        );
        // At 80 columns the owner's facts give the reason the branch's and
        // the path's room; a hovered title of the same width does not.
        let stop = Some(LightsWidth {
            chips: 22,
            title: 47,
            short: 30,
            reason: true,
        });
        let fit = fit_rule(&owners, rule_room(&(0..80)), stop);
        assert_eq!(
            (texts(&fit), fit.title),
            (owned(&[model_effort]), TitleFit::Short),
            "{fit:?}"
        );
        let hover = stop.map(|l| LightsWidth { reason: false, ..l });
        let fit = fit_rule(&owners, rule_room(&(0..80)), hover);
        assert_eq!(
            (fit.segments, fit.title),
            (segments(&owners), TitleFit::None)
        );
        // Chips narrower than the model still draw where the model cannot.
        let dot = Some(LightsWidth {
            chips: 6,
            title: 0,
            short: 0,
            reason: false,
        });
        let fit = fit_rule(&facts, 9, dot);
        assert!(fit.chips && fit.segments.is_empty(), "{fit:?}");
        // `chips_room` is exactly the room the chips have beside the model.
        for room in 0..120 {
            let chips = chips_room(&facts, room);
            for w in [chips, chips + 1] {
                let lights = Some(LightsWidth {
                    chips: w,
                    title: 0,
                    short: 0,
                    reason: false,
                });
                let fit = fit_rule(&facts, room, lights);
                let model_fits = !fit_rule(&facts, room, None).segments.is_empty();
                if w == chips && w > 0 {
                    assert!(fit.chips, "{room}: {w} chips fit");
                }
                if w == chips + 1 && model_fits {
                    assert!(
                        !fit.chips || fit.segments.is_empty(),
                        "{room}: {w} is past the room"
                    );
                }
            }
        }
    }

    /// THE LAUNCH CARD, read for the footer only under a LIVE REPL: the
    /// owner's fresh bypass screen names `Opus 5.5` at `xhigh`. NEGATIVE
    /// CONTROLS: the inline renderer's relaunch before the new REPL is up
    /// (its predecessor's box and card on the screen, `Press Ctrl-C again to
    /// exit` under that box) and half drawn; a box over the composer.
    #[test]
    fn the_launch_card_is_read_only_under_a_live_repl() {
        let fixture = |name: &str| -> Vec<String> {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../aterm-phase/src/fixtures")
                .join(name);
            std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .skip(1)
                .map(str::to_owned)
                .collect()
        };
        let card = launch_card(&fixture("claude-2.1.283-footer-bypass-idle.txt")).unwrap();
        assert_eq!(
            (
                card.version.as_str(),
                card.model.as_str(),
                card.effort.as_deref()
            ),
            ("2.1.283", "Opus 5.5", Some("xhigh"))
        );
        let relaunched = launch_card(&fixture("claude-2.1.283-inline-relaunch-repl-ready.txt"));
        assert_eq!(relaunched.map(|c| c.model).as_deref(), Some("Haiku 4.5"));
        for dead in [
            "claude-2.1.283-inline-relaunch-before-repl.txt",
            "claude-2.1.283-inline-relaunch-repl-half-drawn.txt",
            "claude-2.1.283-launch-repl-half-drawn.txt",
            "claude-2.1.283-effort-switch.txt",
        ] {
            assert_eq!(launch_card(&fixture(dead)), None, "{dead}");
        }
        // The phase reader alone would take the predecessor's card there.
        assert!(
            aterm_phase::launch_card(&fixture("claude-2.1.283-inline-relaunch-before-repl.txt"))
                .is_some()
        );
        // DEFAULT MODE: 2.1.283 draws a pill there too (`⏸ manual mode on ·
        // ? for shortcuts`, measured), so the card is read under it.
        let manual = fixture("claude-2.1.283-footer-manual.txt");
        assert_eq!(
            live_repl_row(&manual),
            mode_row(&manual),
            "default mode's row is a mode row"
        );
        assert_eq!(
            launch_card(&manual).map(|c| c.model).as_deref(),
            Some("Opus 5.5")
        );
        // THE PILL-LESS ROW (no pill drawn: a REPL whose mode pill is
        // suppressed, a build that drew default mode bare) proves a live
        // REPL too: its left hint slot at rest, with a draft, mid-turn.
        let bypass = fixture("claude-2.1.283-footer-bypass-idle.txt");
        let at = mode_row(&bypass).expect("the measured mode row");
        for (bare, live) in [
            ("  ? for shortcuts", true),
            ("  esc to clear", true),
            ("  esc to interrupt \u{00B7} \u{2190} for agents", true),
            // NEGATIVE CONTROLS: the inline renderer's dead box, a
            // completion list, shell mode (not in the closed list).
            ("  Press Ctrl-C again to exit", false),
            ("  /clear          Clear conversation history", false),
            ("  ! for shell mode", false),
        ] {
            let mut rows = bypass.clone();
            rows[at] = bare.to_owned();
            assert_eq!(live_repl_row(&rows).is_some(), live, "{bare:?}");
            assert_eq!(
                launch_card(&rows).map(|c| c.model),
                live.then(|| "Opus 5.5".to_owned()),
                "{bare:?}"
            );
            assert_eq!(mode_row(&rows), None, "{bare:?} is no mode row");
        }
    }

    /// A kept card fills ONLY what nothing newer decided, and only from the
    /// running build's card.
    #[test]
    fn a_launch_card_fills_only_open_facts() {
        let card = aterm_phase::LaunchCard {
            version: "2.1.283".into(),
            model: "Opus 5.5".into(),
            effort: Some("xhigh".into()),
        };
        let open = FooterFacts {
            path: Some("~/aterm".into()),
            version: Some("2.1.283".into()),
            model_open: true,
            effort_open: true,
            ..FooterFacts::default()
        };
        let filled = open.filled_from(Some(&card));
        assert_eq!(
            (filled.model.as_deref(), filled.effort.as_deref()),
            (Some("Opus 5.5"), Some("xhigh"))
        );
        let chosen = FooterFacts {
            model: Some("Fable 5.1".into()),
            model_open: false,
            effort_open: false,
            ..open.clone()
        };
        assert_eq!(chosen.filled_from(Some(&card)), chosen, "a choice stands");
        let voided = FooterFacts {
            model_open: false,
            effort_open: false,
            ..open.clone()
        };
        assert_eq!(
            voided.filled_from(Some(&card)),
            voided,
            "unknown stays unknown"
        );
        let other_build = FooterFacts {
            version: Some("2.1.284".into()),
            ..open.clone()
        };
        assert_eq!(other_build.filled_from(Some(&card)), other_build);
        assert_eq!(open.filled_from(None), open);
        // A switch that named no effort, with nothing since the floor naming
        // the model before it: the card's effort is taken only when the card
        // names the model switched to — the fullscreen renderer redraws it
        // with the new model — never another model's.
        let switched = |to: &str| FooterFacts {
            model: Some(to.into()),
            model_open: false,
            effort_for: Some(to.into()),
            ..open.clone()
        };
        assert_eq!(
            switched("Opus 5.5")
                .filled_from(Some(&card))
                .effort
                .as_deref(),
            Some("xhigh"),
            "the card is the model switched to"
        );
        let away = switched("Sonnet 5");
        assert_eq!(
            away.filled_from(Some(&card)),
            away,
            "the card's xhigh was Opus 5.5's, not Sonnet 5's"
        );
    }

    /// Found under the composer's bottom rule only — a quoted copy of the row
    /// in the conversation above the frame is not the footer.
    #[test]
    fn the_mode_row_is_found_under_the_composer_and_nowhere_else() {
        let rule = "\u{2500}".repeat(40);
        let quoted = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
        let screen: Vec<String> = vec![
            quoted.into(),
            String::new(),
            rule.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents".into(),
        ];
        assert_eq!(mode_row(&screen), Some(5));
        let no_frame: Vec<String> = vec![quoted.into(), "$ ".into()];
        assert_eq!(mode_row(&no_frame), None, "no composer, no footer");
        let with_statusline: Vec<String> = vec![
            rule.clone(),
            "\u{276F} ".into(),
            rule,
            "  my statusline".into(),
            "  \u{23F5}\u{23F5} auto mode on".into(),
        ];
        assert_eq!(
            mode_row(&with_statusline),
            Some(4),
            "a statusLine row may sit between"
        );
    }

    /// The Bash tool's directory is the newest main-thread row's `cwd`: a
    /// subagent's row and a row with no directory are passed over, and a
    /// relative value is no directory.
    #[test]
    fn the_last_cwd_is_the_newest_main_thread_rows() {
        let body = [
            r#"{"type":"user","cwd":"/w"}"#,
            r#"{"type":"assistant","cwd":"/w/nested"}"#,
            r#"{"type":"assistant","cwd":"/w/agent","isSidechain":true}"#,
            r#"{"type":"summary","summary":"no directory here"}"#,
            r#"{"type":"user","cwd":"relative"}"#,
        ]
        .join("\n");
        assert_eq!(last_cwd(body.as_bytes()), Some(PathBuf::from("/w/nested")));
        assert_eq!(last_cwd(br#"{"type":"user"}"#), None);
        assert_eq!(last_cwd(b""), None);
    }

    /// [`shell_cwds`] reads each LIVE session launched in the directory the
    /// box's session reports, and only those: a session launched elsewhere,
    /// a dead pid's leftover file and a transcript still in the launch
    /// directory add nothing.
    #[test]
    fn shell_cwds_reads_the_live_sessions_launched_in_the_directory() {
        let root = std::env::temp_dir().join(format!("aterm-footer-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let claude = root.join("claude");
        let launch = root.join("w");
        std::fs::create_dir_all(claude.join("sessions")).expect("sessions");
        let session = |pid: u32, id: &str, cwd: &Path, last: &Path| {
            std::fs::write(
                claude.join("sessions").join(format!("{pid}.json")),
                format!(
                    r#"{{"pid":{pid},"sessionId":"{id}","cwd":"{}"}}"#,
                    cwd.display()
                ),
            )
            .expect("session file");
            let project = claude.join("projects").join(project_slug(cwd));
            std::fs::create_dir_all(&project).expect("project dir");
            std::fs::write(
                project.join(format!("{id}.jsonl")),
                format!(
                    "{}\n{}\n",
                    format_args!(r#"{{"type":"user","cwd":"{}"}}"#, cwd.display()),
                    format_args!(r#"{{"type":"assistant","cwd":"{}"}}"#, last.display()),
                ),
            )
            .expect("transcript");
        };
        let me = std::process::id();
        // Nothing registered yet: nothing learned.
        assert!(shell_cwds(&claude, &launch).is_empty());
        // This process stands in for a live Claude Code whose Bash tool moved.
        session(me, "a-live", &launch, &launch.join("nested"));
        assert_eq!(shell_cwds(&claude, &launch), [launch.join("nested")]);
        // Another launch directory is another session's business.
        assert!(shell_cwds(&claude, &root.join("elsewhere")).is_empty());
        // NEGATIVE CONTROL: a pid no process holds is a leftover file.
        session(999_999_990, "b-dead", &launch, &root.join("dead"));
        assert_eq!(shell_cwds(&claude, &launch), [launch.join("nested")]);
        // A Bash tool still in the launch directory adds nothing new.
        session(me, "a-live", &launch, &launch);
        assert!(shell_cwds(&claude, &launch).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
