// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Live upgrade of a running Claude Code session.
//!
//! Quiet by default: unless `[harness] upgrade_ask = true`, nothing is typed
//! into the agent. A session is restarted in place only at a genuinely idle
//! point ([`Facts::quiet`], [`quiet_step`]: the restart's own gate as a READY
//! agent would pass it) and resumed as it was; until then the owner's waiting
//! record says a newer build is installed, and it stands a look before any
//! restart that types nothing first ([`Facts::unnoticed`]). One line follows
//! only where the resumed build itself warns that something the session ran
//! did not come back ([`resumed_build_warned`], [`restore_prompt`]) and its
//! task is still open. A conversation whose record says its task LANDED
//! ([`FinishedScan`]) moves the quiet way whatever the key says. The
//! steps below are the `upgrade_ask = true` flow.
//!
//! A running Claude Code cannot have its binary replaced (the kernel mapped
//! the old image) and Claude's own `/update` relaunch is compiled in but
//! switched off, so this module plans a cooperative, in-place resume relaunch:
//!
//! 1. **Announce.** At an idle point with an empty composer, type one turn
//!    saying an upgrade restart is coming: let live background work finish,
//!    stop any wait that can never end ([`STOPPING_POINT`]), with the shells
//!    aterm sees named ([`running_clause`]), save work, and answer with a
//!    readiness answer in ordinary language ([`prepare_prompt`]). Never at a usage or rate
//!    limit ([`Facts::limited`]) or the login wall ([`Facts::login`]), where
//!    it would be read by no model; a notice the login wall answered is typed
//!    again once the login is back ([`Facts::undelivered`]).
//! 2. **Drain.** Wait until the agent answered the current notice that it is ready
//!    ([`transcript_has_ready`]), Claude's session file says `idle`, nothing
//!    runs under the agent (Claude Code's own `caffeinate` aside), and the
//!    composer is empty ([`gate_restart`]). Nothing is ever killed. An
//!    unanswered notice, or a READY that work outlives, is asked again every
//!    [`REASK_S`] naming what runs; after [`MAX_ASKS`] the round gives up and
//!    says what held it ([`next_step`]), and [`RETRY_S`] later a new round
//!    starts ([`Step::Rearm`]) with new notice references. A person's box or unsent
//!    draft ([`person_hold`]) standing for [`HOLD_S`] voids a READY older than
//!    [`DRAIN_S`]; the upgrade asks again at the next idle point. The answer
//!    is read at the turn end it ends: a READY on screen but not yet in the
//!    transcript holds that point ([`Facts::ready_unrecorded`]), and a READY
//!    superseded by later direction is asked again at the next point
//!    ([`Facts::ready_superseded`]).
//! 3. **Restart.** SIGTERM the agent (graceful shutdown, nothing typed into
//!    the TUI), wait for the tab's shell to hold the terminal, and type one
//!    line ([`relaunch_line`]) running the newer build with the original flags
//!    ([`rewrite_argv`]) and `--resume <sessionId>`, healing a stale shell's
//!    PATH, a `degraded` shell integration ([`with_rekey`]) or a pre-loader
//!    script in the same line when needed.
//! 4. **Continue.** Once the new process re-registers the same session, type
//!    one turn saying it was upgraded, the model before and (if the relaunch
//!    asked for one) the model now, and to carry on ([`continue_prompt`],
//!    [`continue_prompt_with_model`]).
//! 5. **Confirm.** Read the model the resumed session's first answer names
//!    ([`transcript_first_model`]) and record it beside the one before
//!    ([`restart_outcome`]). The rewrite keeps an explicit `--model`
//!    ([`launch_model`]) and adds none, unless the model rule (`upgrade_models`:
//!    the newest model of its own family first, then up the priority list for
//!    a model nobody chose, never down) moves it; then the notices name the
//!    model and why ([`prepare_prompt_with_model`]), the relaunch carries it as
//!    `--model`, and the outcome says whether it was taken
//!    ([`restart_outcome_listed`]). A session that has not answered in time is
//!    unconfirmed. Without an explicit flag Claude Code's own resume picks the
//!    model, which may not be the newest.
//!
//! A conversation with no task ([`TaskScan`]) skips all of this: once idle it
//! is ended and the new build started afresh in its tab ([`Step::Fresh`]).
//! One whose task LANDED ([`FinishedScan`]: a clean `Nothing left:` defense
//! answering one of Autopilot Land's acts, with no prompt of a person's or an
//! orchestrator's since — never a token or a done reply) is asked nothing
//! and told nothing: moved the quiet way, and any carry-on owed after a
//! restart is left untyped (`done:finished`).
//!
//! An agent the upgrade asked is never left stopped: it is restarted and
//! told to carry on, or released. Whenever the upgrade abandons an
//! announcement without restarting (gives up, voids a READY held by a person,
//! is held by `--skip`/`--defer`, or a restart after READY stops), one line
//! ([`release_prompt`]) is owed and typed at the next point the notice could
//! be ([`gate_release`]). A given-up upgrade still honours a READY that comes
//! before that line ([`GAVE_UP`]). The line is dropped only when it is no
//! longer the upgrade's to type: another process holds the conversation, or
//! the agent took up direction given after its last answer
//! ([`directed_since_ready`]). One exception, outside the derived model
//! (`harness_upgrade_never_strands_model`): an agent that is no job of a
//! job-control shell is refused and owed no line (`upgrade_drive::not_a_job`),
//! since it can never be proven to read it. An aterm tab's own program
//! (`aterm -e claude`), refused the same way, keeps the line an older build's
//! notice owes it: it leads its terminal, so the line reaches it.
//!
//! Everything here is pure (parsers, the target rule, the argv rewrite, the
//! prompts, model reads, the outcome line and the gates) over facts a driver
//! measured; the driver (`cli::run_upgrade`) owns the I/O. Claude Code's
//! session file `~/.claude/sessions/<pid>.json` (`pid`, `sessionId`, `cwd`,
//! `version`, `status` busy|shell|idle|waiting, `statusUpdatedAt` ms,
//! `procStart`, `kind`, `entrypoint`) and transcript
//! (`~/.claude/projects/<dir>/<sessionId>.jsonl`) are a third party's files,
//! so every read refuses an unexpected shape rather than guessing.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::steps::{BASH_INPUT, BASH_STDERR, BASH_STDOUT, INTERRUPTED, TASK_NOTIFICATION};
use super::upgrade_models as models;
use crate::supervise::approvals::Typed;
use crate::supervise::policy::turn_end;

/// A dotted-digits version (`2.1.281`), compared numerically part by part
/// with missing parts as 0, so `2.1` equals `2.1.0` and equality agrees with
/// the ordering.
#[derive(Clone, Debug)]
pub struct Version(Vec<u64>);

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl Version {
    /// Parses `2.1.281` (a leading `v` tolerated). `None` for anything but
    /// dot-separated ASCII digit runs: a pre-release suffix is refused rather
    /// than ordered, so "never downgrade" never rests on a guess.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let t = t.strip_prefix('v').unwrap_or(t);
        if t.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for p in t.split('.') {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 12 {
                return None;
            }
            parts.push(p.parse().ok()?);
        }
        Some(Self(parts))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.0.len().max(other.0.len());
        for i in 0..n {
            let a = self.0.get(i).copied().unwrap_or(0);
            let b = other.0.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for p in &self.0 {
            if !first {
                f.write_char('.')?;
            }
            write!(f, "{p}")?;
            first = false;
        }
        Ok(())
    }
}

/// Claude Code's own record of one running interactive process,
/// `<config>/sessions/<pid>.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFile {
    /// The process id the record describes.
    pub pid: u32,
    /// The conversation this process holds now (Claude rewrites it when the
    /// session changes, so it is re-read at every decision).
    pub session_id: String,
    /// The process's working directory as Claude last recorded it.
    pub cwd: String,
    /// The running build's version, as the running build reports itself.
    pub version: String,
    /// `busy`, `shell` (a background shell runs), `idle` or `waiting` (a
    /// dialog or permission prompt is up).
    pub status: String,
    /// When `status` last changed, unix milliseconds.
    pub status_updated_at_ms: u64,
    /// The process start time as Claude renders it (`ps -o lstart=` in UTC,
    /// C locale): the pid-reuse guard.
    pub proc_start: String,
    /// `interactive` for a TUI session.
    pub kind: String,
    /// `cli` for a session started from a shell.
    pub entrypoint: String,
    /// THE CLAUDE CODE DIRECTORY THIS RECORD WAS READ FROM (`<dir>/sessions/
    /// <pid>.json`): where the process keeps every record of its own, its
    /// conversation's transcript too — the directory its
    /// `CLAUDE_CONFIG_DIR` named, else `~/.claude`. Set by the harness's
    /// readers (`upgrade_drive::session_files_in`), which read every Claude
    /// home an agent in a tab runs under, so whatever a step reads about the
    /// agent it reads THERE (`upgrade_drive::Records::of_file`). `None` for a
    /// record parsed from text alone ([`parse_session_file`]), read as the
    /// user's own `~/.claude`.
    pub claude_dir: Option<PathBuf>,
    /// THE PROFILE CLAUDE CODE KEEPS FOR THE HOME THIS RECORD WAS READ UNDER
    /// (its global config, where the login's model caches are), where the
    /// directory alone does not name it (`upgrade_drive::profile_apart`): a
    /// `CLAUDE_CONFIG_DIR` set and EMPTY files its records in the directory
    /// the process runs in and keeps its profile in `$HOME/.claude.json`, and
    /// one naming the user's own `~/.claude` keeps it inside, not beside, it.
    /// Set by the readers that know the home's raw value
    /// (`upgrade_drive::Records`); `None` for every other home, whose profile
    /// is read by `claude_dir` (beside the user's `~/.claude`, inside any
    /// other).
    pub profile: Option<PathBuf>,
}

/// Parses a session file, refusing any shape this module was not written for.
///
/// # Errors
/// The text is not a JSON object, or a required field is missing or of the
/// wrong type (named, so a schema change reads as one, not as an idle
/// session).
pub fn parse_session_file(text: &str) -> Result<SessionFile, String> {
    let v: Value = aterm_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let obj = v.as_object().ok_or("not a JSON object")?;
    let s = |k: &str| -> Result<String, String> {
        obj.get(k)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("no string `{k}`"))
    };
    let n = |k: &str| -> Result<u64, String> {
        obj.get(k)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("no number `{k}`"))
    };
    let pid = u32::try_from(n("pid")?).map_err(|_| "pid out of range".to_string())?;
    Ok(SessionFile {
        pid,
        session_id: s("sessionId")?,
        cwd: s("cwd")?,
        version: s("version")?,
        status: s("status")?,
        status_updated_at_ms: n("statusUpdatedAt")?,
        proc_start: s("procStart")?,
        kind: s("kind")?,
        entrypoint: s("entrypoint")?,
        claude_dir: None,
        profile: None,
    })
}

/// Whether a session id is safe to put on a command line: Claude's ids are
/// UUIDs, so nothing but hex and dashes, 8 to 64 bytes.
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Where a relaunch target comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// aterm's managed build (the agents twin's store build).
    Managed,
    /// The vendor's own installer's copy (Claude's native `~/.local` install,
    /// which its updater moves — the source of `Update installed · Restart to
    /// update`).
    Native,
}

impl Source {
    /// The word the ledger and the notice print.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Managed => "managed",
            Source::Native => "native",
        }
    }
}

/// Which agent an upgrade or a relaunch record is of: the Claude lane's (this
/// module), a Codex's (a codex-alab's relaunch, `upgrade_drive`'s
/// `codex_alab_drive`; no Codex is moved live), a Muse Code's
/// ([`super::upgrade_muse`]), an agy's ([`super::upgrade_agy`]) or a Gemini
/// CLI's ([`super::upgrade_gemini`]): each of the last three relaunch on exit
/// and restore only. One state directory, one ledger and one owner's view
/// carry them all; this word tells their rows apart. Every variant is in
/// [`Agent::ALL`].
///
/// Muse Code's word (`muse`), agy's (`agy`) and Gemini CLI's (`gemini`) are
/// written by their relaunches on exit (`<word>-<tab>` records, their restore
/// leaves' `[muse]`, `[agy]` and `[gemini]` tables), and their upgrade arms
/// still answer that nothing moves them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code (a state written before the word existed is one).
    #[default]
    Claude,
    /// Codex.
    Codex,
    /// Google's Antigravity CLI: relaunched on exit and restored, and moved
    /// onto a newer build live by the quiet flow alone (`upgrade_agy_live`:
    /// ended at an idle point and resumed, nothing typed; never the ask flow).
    Agy,
    /// Meta's Muse Code: relaunched on exit and restored, and moved onto a
    /// newer build live by the quiet flow alone (`upgrade_muse_live`: ended at
    /// an idle point and resumed, nothing typed; never the ask flow).
    Muse,
    /// Google's Gemini CLI: relaunched on exit and restored, never upgraded
    /// live.
    Gemini,
}

/// Each agent's successor in declaration order, `None` after the last: an
/// exhaustive match, so a variant added to [`Agent`] does not compile until it
/// is placed here, and the walk below holds [`Agent::ALL`] to this chain.
const fn next_agent(agent: Agent) -> Option<Agent> {
    match agent {
        Agent::Claude => Some(Agent::Codex),
        Agent::Codex => Some(Agent::Agy),
        Agent::Agy => Some(Agent::Muse),
        Agent::Muse => Some(Agent::Gemini),
        Agent::Gemini => None,
    }
}

// [`Agent::ALL`] is exactly the chain [`next_agent`] walks from Claude Code,
// each at its own discriminant: checked at compile time. Claude Code is first,
// so [`Agent::others`] is every other lane.
const _: () = {
    let mut at = 0;
    let mut next = Some(Agent::Claude);
    while let Some(agent) = next {
        assert!(at < Agent::ALL.len() && agent as usize == at && Agent::ALL[at] as usize == at);
        at += 1;
        next = next_agent(agent);
    }
    assert!(at == Agent::ALL.len());
};

impl Agent {
    /// Every agent, in declaration order, Claude Code first: what must cover
    /// each one (a count per agent, the passes of one sweep) iterates this,
    /// never a list of its own. Every variant is here BY CONSTRUCTION
    /// ([`next_agent`], walked at compile time), and `agent as usize` is its
    /// index here.
    pub const ALL: [Agent; 5] = [
        Agent::Claude,
        Agent::Codex,
        Agent::Agy,
        Agent::Muse,
        Agent::Gemini,
    ];

    /// Every agent but Claude Code, in the ONE FIXED ORDER the live upgrade's
    /// passes visit their lanes, each before Claude Code's
    /// (`upgrade_drive::lanes`, `due`, `note_behind`): a tab whose foreground
    /// job is one of them belongs to that lane alone, and Claude Code's
    /// machine-wide reads are never that tab's.
    #[must_use]
    pub fn others() -> &'static [Agent] {
        &Self::ALL[1..]
    }

    /// THE PREFIX OF THIS AGENT'S PER-TAB RECORD KEYS: a lane past Claude
    /// Code's keys the record of its upgrade or relaunch in a tab
    /// `<word>-<tab>` ([`Self::record_key`]); Claude Code's records are keyed
    /// by its conversation, so it has none (`None`). No conversation id starts
    /// with one ([`is_session_id`]: hex and dashes).
    #[must_use]
    pub const fn record_prefix(self) -> Option<&'static str> {
        match self {
            Agent::Claude => None,
            Agent::Codex => Some("codex-"),
            // Their relaunch on exit's records, one per tab, as Codex's.
            Agent::Muse => Some("muse-"),
            Agent::Agy => Some("agy-"),
            Agent::Gemini => Some("gemini-"),
        }
    }

    /// The key of this agent's record of its upgrade in `tab`
    /// (`<word>-<tab>`); `None` for Claude Code ([`Self::record_prefix`]).
    #[must_use]
    pub fn record_key(self, tab: &str) -> Option<String> {
        self.record_prefix().map(|prefix| format!("{prefix}{tab}"))
    }

    /// The tab a record key of this agent names ([`Self::record_key`] read
    /// back); `None` for a key that is not this agent's, and for Claude Code.
    #[must_use]
    pub fn record_tab(self, key: &str) -> Option<&str> {
        key.strip_prefix(self.record_prefix()?)
    }

    /// The lane past Claude Code's whose per-tab record `key` is
    /// ([`Self::record_key`]), first in [`Self::others`]' order; `None` for a
    /// key of none (a Claude Code conversation's).
    #[must_use]
    pub fn keyed(key: &str) -> Option<Agent> {
        Self::others()
            .iter()
            .copied()
            .find(|agent| agent.record_tab(key).is_some())
    }

    /// The word the state file carries: empty for Claude (every state an
    /// older build wrote), `codex`, `muse`, `agy`, `gemini`
    /// ([`aterm_phase::Program::name`]).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Agent::Claude => "",
            Agent::Codex => "codex",
            Agent::Agy => "agy",
            Agent::Muse => "muse",
            Agent::Gemini => "gemini",
        }
    }

    /// [`Self::word`] read back, and Claude's own name for its lane. Muse
    /// Code's, agy's and Gemini CLI's are their relaunch lanes'. `None` for
    /// any other
    /// word: a record a newer build wrote for an agent this one does not know,
    /// which is never driven as another agent's.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "" | "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "agy" => Some(Agent::Agy),
            "muse" => Some(Agent::Muse),
            "gemini" => Some(Agent::Gemini),
            _ => None,
        }
    }

    /// The product's name, as the owner reads it ([`aterm_phase::Program::product`]).
    #[must_use]
    pub fn product(self) -> &'static str {
        self.program().product()
    }

    /// The program whose screen reader reads this agent's tab.
    #[must_use]
    pub fn program(self) -> aterm_phase::Program {
        match self {
            Agent::Claude => aterm_phase::Program::Claude,
            Agent::Codex => aterm_phase::Program::Codex,
            Agent::Agy => aterm_phase::Program::Agy,
            Agent::Muse => aterm_phase::Program::Muse,
            Agent::Gemini => aterm_phase::Program::Gemini,
        }
    }

    /// The agent `program` is; `None` for a program that is no agent
    /// (`Generic`). Muse Code, agy and Gemini CLI are relaunched on exit,
    /// never moved by an upgrade ([`super::lanes::lanes`]).
    #[must_use]
    pub fn of(program: aterm_phase::Program) -> Option<Self> {
        match program {
            aterm_phase::Program::Claude => Some(Agent::Claude),
            aterm_phase::Program::Codex => Some(Agent::Codex),
            aterm_phase::Program::Agy => Some(Agent::Agy),
            aterm_phase::Program::Muse => Some(Agent::Muse),
            aterm_phase::Program::Gemini => Some(Agent::Gemini),
            aterm_phase::Program::Generic => None,
        }
    }
}

/// A build a session could be relaunched on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What to execute: an absolute path.
    pub exe: PathBuf,
    /// Its version, as it reports itself.
    pub version: Version,
    /// Where it comes from.
    pub source: Source,
}

/// The target rule: the newest candidate strictly newer than what runs, and
/// on a version tie the managed one. `None` when nothing is newer: a session
/// is never moved sideways or down.
#[must_use]
pub fn choose_target(running: &Version, candidates: &[Candidate]) -> Option<Candidate> {
    let mut best: Option<&Candidate> = None;
    for c in candidates.iter().filter(|c| c.version > *running) {
        best = match best {
            None => Some(c),
            Some(b) => match c.version.cmp(&b.version) {
                Ordering::Greater => Some(c),
                Ordering::Equal if c.source == Source::Managed => Some(c),
                _ => Some(b),
            },
        };
    }
    best.cloned()
}

/// Why a flag list cannot be carried into a resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgvRefusal {
    /// A flag this table does not know: fail closed rather than guess whether it
    /// takes a value.
    UnknownFlag(String),
    /// A flag whose session cannot be resumed as a TUI in place (`--print`,
    /// `--bg`, `--tmux`, `--worktree`, `--no-session-persistence`, …).
    NotResumable(String),
}

impl std::fmt::Display for ArgvRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgvRefusal::UnknownFlag(fl) => write!(f, "unknown-flag {fl}"),
            ArgvRefusal::NotResumable(fl) => write!(f, "not-resumable {fl}"),
        }
    }
}

/// How a Claude flag takes its value (from `claude --help`, 2.1.281), consumed
/// exactly as Claude's own parser consumes it (see [`maybe_option`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    /// A switch.
    None,
    /// Exactly one value (`<value>`): the next token, whatever it looks like.
    One,
    /// A required list (`<values...>`): the next token whatever it looks like,
    /// then every token until an option. Optional lists (`[values...]`) would
    /// need their own arm.
    Many,
    /// An optional value (`[value]`): the next token if it is not an option (a
    /// lone `-` is a value).
    Optional,
}

/// What the rewrite does with a flag.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Carried verbatim.
    Keep,
    /// Dropped with its value — the resume family, replaced by `--resume <id>`.
    Drop,
    /// The whole relaunch is refused.
    Refuse,
}

/// Claude Code's options. A flag missing here refuses the rewrite (fail
/// closed); new flags are added deliberately once known safe to carry.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--add-dir", Arity::Many, Fate::Keep),
    ("--agent", Arity::One, Fate::Keep),
    ("--agents", Arity::One, Fate::Keep),
    (
        "--allow-dangerously-skip-permissions",
        Arity::None,
        Fate::Keep,
    ),
    ("--allowedTools", Arity::Many, Fate::Keep),
    ("--allowed-tools", Arity::Many, Fate::Keep),
    ("--append-system-prompt", Arity::One, Fate::Keep),
    ("--append-system-prompt-file", Arity::One, Fate::Keep),
    ("--autocompact", Arity::One, Fate::Keep),
    ("--ax-screen-reader", Arity::None, Fate::Keep),
    ("--bg", Arity::None, Fate::Refuse),
    ("--background", Arity::None, Fate::Refuse),
    ("--bare", Arity::None, Fate::Keep),
    ("--betas", Arity::Many, Fate::Keep),
    ("--brief", Arity::None, Fate::Keep),
    ("--chrome", Arity::None, Fate::Keep),
    ("--cloud", Arity::Optional, Fate::Refuse),
    ("-c", Arity::None, Fate::Drop),
    ("--continue", Arity::None, Fate::Drop),
    ("--dangerously-skip-permissions", Arity::None, Fate::Keep),
    ("-d", Arity::Optional, Fate::Keep),
    ("--debug", Arity::Optional, Fate::Keep),
    ("--debug-file", Arity::One, Fate::Keep),
    ("--disable-slash-commands", Arity::None, Fate::Keep),
    ("--disallowedTools", Arity::Many, Fate::Keep),
    ("--disallowed-tools", Arity::Many, Fate::Keep),
    // Dropped: the resumed session comes back at the user's default effort,
    // never pinned to the launch's. A launch `--settings` is still carried.
    ("--effort", Arity::One, Fate::Drop),
    ("--environment", Arity::One, Fate::Refuse),
    (
        "--exclude-dynamic-system-prompt-sections",
        Arity::None,
        Fate::Keep,
    ),
    ("--fallback-model", Arity::One, Fate::Keep),
    ("--file", Arity::Many, Fate::Refuse),
    ("--fork-session", Arity::None, Fate::Drop),
    ("--forward-subagent-text", Arity::None, Fate::Keep),
    ("--from-pr", Arity::Optional, Fate::Drop),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    ("--ide", Arity::None, Fate::Keep),
    ("--include-hook-events", Arity::None, Fate::Keep),
    ("--include-partial-messages", Arity::None, Fate::Keep),
    ("--input-format", Arity::One, Fate::Refuse),
    ("--json-schema", Arity::One, Fate::Refuse),
    ("--max-budget-usd", Arity::One, Fate::Keep),
    ("--mcp-config", Arity::Many, Fate::Keep),
    ("--model", Arity::One, Fate::Keep),
    ("-n", Arity::One, Fate::Keep),
    ("--name", Arity::One, Fate::Keep),
    ("--no-chrome", Arity::None, Fate::Keep),
    ("--no-session-persistence", Arity::None, Fate::Refuse),
    ("--output-format", Arity::One, Fate::Refuse),
    ("--permission-mode", Arity::One, Fate::Keep),
    ("--permission-prompts", Arity::One, Fate::Keep),
    ("--plugin-dir", Arity::One, Fate::Keep),
    ("--plugin-url", Arity::One, Fate::Keep),
    ("-p", Arity::None, Fate::Refuse),
    ("--print", Arity::None, Fate::Refuse),
    ("--prompt-suggestions", Arity::Optional, Fate::Keep),
    ("--remote-control", Arity::Optional, Fate::Refuse),
    (
        "--remote-control-session-name-prefix",
        Arity::One,
        Fate::Refuse,
    ),
    ("--replay-user-messages", Arity::None, Fate::Refuse),
    ("--restricted", Arity::None, Fate::Keep),
    ("-r", Arity::Optional, Fate::Drop),
    ("--resume", Arity::Optional, Fate::Drop),
    ("--safe-mode", Arity::None, Fate::Keep),
    ("--session-id", Arity::One, Fate::Drop),
    ("--setting-sources", Arity::One, Fate::Keep),
    // Kept verbatim — all but aterm's own usage tap, exactly
    // (`super::tap::is_ours`), which [`carried`] sheds: a relaunch adds it
    // again where its gate still holds (`super::tap::gate`), so a stale path
    // or a tap switched off leaves at the next relaunch.
    ("--settings", Arity::One, Fate::Keep),
    ("--strict-mcp-config", Arity::None, Fate::Keep),
    ("--system-prompt", Arity::One, Fate::Keep),
    ("--system-prompt-file", Arity::One, Fate::Keep),
    ("--system-prompt-snapshot", Arity::One, Fate::Keep),
    ("--teleport", Arity::Optional, Fate::Refuse),
    ("--tmux", Arity::Optional, Fate::Refuse),
    ("--tools", Arity::Many, Fate::Keep),
    ("--verbose", Arity::None, Fate::Keep),
    ("-v", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    ("-w", Arity::Optional, Fate::Refuse),
    ("--worktree", Arity::Optional, Fate::Refuse),
];

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Whether Claude's own parser reads `tok` as an option: its bundled
/// Commander tests `o.length>1&&o[0]==="-"`, so a lone `-` is a value or
/// positional. [`rewrite_argv`] asks only this, so it consumes exactly the
/// tokens Claude did; otherwise a bare flag could swallow the appended
/// `--resume <id>` as its value.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// The argv rewrite: the original argv (argv[0] dropped) becomes the flags a
/// resume carries. Every known flag and its values are kept verbatim, the
/// resume family (`--resume`/`-r [id]`, `--continue`/`-c`, `--session-id
/// <v>`, `--fork-session`, `--from-pr [v]`) is dropped, and `--resume
/// <session_id>` is appended.
///
/// A positional (the launch prompt) is dropped, not refused: the conversation
/// already holds it. Everything after `--` is positional too, unless it is a
/// flag's value (`--add-dir --`), which Claude's parser takes as is.
///
/// # Errors
/// An unknown flag ([`ArgvRefusal::UnknownFlag`]) or one whose session is not
/// a TUI resumable in place ([`ArgvRefusal::NotResumable`]).
pub fn rewrite_argv(argv: &[String], session_id: &str) -> Result<Vec<String>, ArgvRefusal> {
    let mut out: Vec<String> = carried(argv)?
        .into_iter()
        .flat_map(|(_, tokens)| tokens.iter().cloned())
        .collect();
    out.push("--resume".to_string());
    out.push(session_id.to_string());
    Ok(out)
}

/// The flags that set a launch's PERMISSION MODE ([`permission_argv`]).
const PERMISSION_FLAGS: &[&str] = &[
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--permission-mode",
];

/// `argv` (`argv[0]` first) with only the flags that set its permission
/// mode ([`PERMISSION_FLAGS`]), as [`rewrite_argv`] carries them: what a
/// resume line a person is told steps down to before the bare one, where
/// every carried flag does not fit its budget (`resume::shorter`). A bare
/// `claude --resume <id>` brings an orchestrator started with
/// `--dangerously-skip-permissions` back in another permission mode, and one
/// started in `--permission-mode plan` out of it. An argv the rewrite refuses
/// keeps `argv[0]` alone.
#[must_use]
pub(crate) fn permission_argv(argv: &[String]) -> Vec<String> {
    let mut out: Vec<String> = argv.first().cloned().into_iter().collect();
    if let Ok(flags) = carried(argv) {
        out.extend(
            flags
                .into_iter()
                .filter(|(name, _)| PERMISSION_FLAGS.contains(name))
                .flat_map(|(_, tokens)| tokens.iter().cloned()),
        );
    }
    out
}

/// Every flag [`rewrite_argv`] carries, in argv order: its name and its tokens
/// as the CLI parsed them (`--model=<v>` is one token, `--model <v>` two).
/// A `--settings` whose value is EXACTLY aterm's usage tap
/// (`super::tap::is_ours`, either spelling) is not carried: it is aterm's
/// to add again, never a person's.
fn carried(argv: &[String]) -> Result<Vec<(&str, &[String])>, ArgvRefusal> {
    let mut out = Vec::new();
    for (name, fate, tokens) in parsed(argv) {
        match fate {
            None => return Err(ArgvRefusal::UnknownFlag(name.to_string())),
            Some(Fate::Refuse) => return Err(ArgvRefusal::NotResumable(name.to_string())),
            Some(Fate::Keep) if name == "--settings" && tap_tokens(tokens) => {}
            Some(Fate::Keep) => out.push((name, tokens)),
            Some(Fate::Drop) => {}
        }
    }
    Ok(out)
}

/// Whether a `--settings` flag's tokens (`--settings v` or `--settings=v`)
/// carry aterm's usage tap ([`super::tap::is_ours`]).
fn tap_tokens(tokens: &[String]) -> bool {
    let value = match tokens {
        [flag, value] if flag == "--settings" => Some(value.as_str()),
        [one] => one.strip_prefix("--settings="),
        _ => None,
    };
    value.is_some_and(super::tap::is_ours)
}

/// The NAMES of the flags [`rewrite_argv`] carries from `argv`, in argv
/// order (aterm's usage tap shed): what the tap's gate asks of a launch
/// (`super::tap::gate`: no `--settings` of a person's, no `--bare`).
///
/// # Errors
/// As [`rewrite_argv`].
pub(super) fn carried_names(argv: &[String]) -> Result<Vec<&str>, ArgvRefusal> {
    Ok(carried(argv)?.into_iter().map(|(name, _)| name).collect())
}

/// Every flag in `argv` as Claude's parser reads it, in argv order: its name,
/// its fate in [`FLAGS`] (`None`: a flag the table does not know, read as a
/// switch — [`carried`] stops at it), and its tokens. Positionals and
/// everything after `--` are not flags.
fn parsed(argv: &[String]) -> Vec<(&str, Option<Fate>, &[String])> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let tok = &argv[i];
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            // A positional: the first prompt. Dropped (see the doc).
            i += 1;
            continue;
        }
        let (name, inline) = match tok.split_once('=') {
            Some((n, _)) if n.starts_with("--") => (n, true),
            _ => (tok.as_str(), false),
        };
        let known = flag(name);
        // The token and its values, as the CLI parsed them.
        let start = i;
        i += 1;
        if !inline {
            match known.map_or(Arity::None, |(arity, _)| arity) {
                Arity::None => {}
                Arity::One => {
                    if i < argv.len() {
                        i += 1;
                    }
                }
                Arity::Optional => {
                    if i < argv.len() && !maybe_option(&argv[i]) {
                        i += 1;
                    }
                }
                Arity::Many => {
                    // The first value is required, so Claude's parser takes it
                    // whatever it looks like (`--add-dir -c`).
                    if i < argv.len() {
                        i += 1;
                    }
                    while i < argv.len() && !maybe_option(&argv[i]) {
                        i += 1;
                    }
                }
            }
        }
        out.push((name, known.map(|(_, fate)| fate), &argv[start..i]));
    }
    out
}

/// The flags that make a launch a one-shot run rather than a session: it
/// prints one answer and exits (`-p`), or answers about itself and exits.
const ONE_SHOT: &[&str] = &["-p", "--print", "-h", "--help", "-v", "--version"];

/// Whether `argv` (argv[0] included) is a one-shot run ([`ONE_SHOT`]): its
/// exit is the intended end, never a crash to relaunch, wherever the flag
/// stands.
#[must_use]
pub fn one_shot(argv: &[String]) -> bool {
    parsed(argv)
        .iter()
        .any(|(name, _, _)| ONE_SHOT.contains(name))
}

/// The model the relaunch asks for: the last `--model` value
/// [`rewrite_argv`] keeps, or `None` when the launch named none or its argv
/// is refused.
#[must_use]
pub fn launch_model(argv: &[String]) -> Option<String> {
    let flags = carried(argv).ok()?;
    let (_, tokens) = flags
        .into_iter()
        .rev()
        .find(|(name, _)| *name == "--model")?;
    match tokens {
        [one] => one.split_once('=').map(|(_, v)| v.to_string()),
        [_, value] => Some(value.clone()),
        _ => None,
    }
}

/// The conversation a launch resumed, as Claude's parser reads `argv`
/// (argv[0] included): `Some(Some(id))` for `--resume <id>`/`-r <id>` (last
/// wins); `Some(None)` for a resume naming no kept conversation (`--continue`,
/// a bare `--resume`, or any resume with `--fork-session`); `None` for no
/// resume. Unlike [`launch_model`], reads refused argvs too, treating unknown
/// flags as switches.
#[must_use]
pub fn launch_resume(argv: &[String]) -> Option<Option<String>> {
    let mut resumed: Option<Option<String>> = None;
    let mut fork = false;
    for (name, _, tokens) in parsed(argv) {
        match name {
            "--resume" | "-r" => {
                let id = match tokens {
                    [one] => one.split_once('=').map(|(_, v)| v.to_string()),
                    [_, value] => Some(value.clone()),
                    _ => None,
                };
                resumed = Some(id.filter(|id| !id.is_empty()));
            }
            "--continue" | "-c" => resumed = Some(None),
            "--fork-session" => fork = true,
            _ => {}
        }
    }
    resumed.map(|id| id.filter(|_| !fork))
}

/// The tab's shell, as its executable names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// zsh.
    Zsh,
    /// bash.
    Bash,
    /// fish.
    Fish,
}

impl Dialect {
    /// From the shell's executable basename (a login shell's `-zsh` included).
    /// `None` for any other shell: the relaunch line is written for these three
    /// and no other.
    #[must_use]
    pub fn from_exe_name(name: &str) -> Option<Self> {
        let base = name.rsplit('/').next().unwrap_or(name);
        match base.trim_start_matches('-') {
            "zsh" => Some(Dialect::Zsh),
            "bash" => Some(Dialect::Bash),
            "fish" => Some(Dialect::Fish),
            _ => None,
        }
    }

    /// The atpkg shell hook for this dialect's extension.
    #[must_use]
    pub fn hook_ext(self) -> &'static str {
        match self {
            Dialect::Zsh => "zsh",
            Dialect::Bash => "bash",
            Dialect::Fish => "fish",
        }
    }
}

/// Quote one word for `dialect`. POSIX shells: single quotes with `'\''`. fish:
/// single quotes, where `\\` and `\'` are the only escapes.
#[must_use]
pub fn quote(dialect: Dialect, word: &str) -> String {
    match dialect {
        Dialect::Zsh | Dialect::Bash => format!("'{}'", word.replace('\'', r"'\''")),
        Dialect::Fish => format!("'{}'", word.replace('\\', r"\\").replace('\'', r"\'")),
    }
}

/// The longest relaunch line this module writes on the host kernel
/// (`LineDiscipline::HOST`). A longer one is refused, never truncated: a cut
/// line would run something no one planned.
///
/// The bound is the terminal's. The line is typed once the shell holds the
/// terminal again, which can be before its line editor puts the tty in raw
/// mode (prompt work the shell does itself, like `$(git …)` substitutions).
/// Typing in that window goes to the kernel's canonical line buffer, which
/// keeps a bounded number of bytes (`LineDiscipline::line_keeps`) and loses
/// the rest; `--resume <id>` is at the end, so a cut can leave the shell at
/// `quote>` or start a new conversation without `--resume`.
///
/// So the line, its bracketed-paste frame (`PASTE_FRAME`) and the Enter fit
/// one canonical line of each kernel with room to spare, checked at compile
/// time below. The bound is per kernel because they differ fourfold. The
/// driver builds the line before the notice and before its signal, so a line
/// that would not fit is refused before the agent is asked to wind down.
#[cfg(test)]
pub(crate) const MAX_LINE: usize = LineDiscipline::HOST.max_line();

/// The kernel line discipline a relaunch line is typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineDiscipline {
    /// macOS — and any kernel not named here, which gets the smaller bound.
    Darwin,
    /// Linux `n_tty`.
    Linux,
}

impl LineDiscipline {
    /// The kernel this build runs on.
    const HOST: Self = if cfg!(target_os = "linux") {
        Self::Linux
    } else {
        Self::Darwin
    };

    /// The most bytes of one canonical line the kernel keeps, Enter included.
    /// macOS: `MAX_INPUT` (1024; the rest is dropped, Enter included, so a cut
    /// line never runs). Linux: `N_TTY_BUF_SIZE`, 4095 bytes plus the newline
    /// (the rest is discarded but the newline arrives, so a cut line runs).
    const fn line_keeps(self) -> usize {
        match self {
            Self::Darwin => 1024,
            Self::Linux => 4096,
        }
    }

    /// The longest relaunch line written for this kernel.
    const fn max_line(self) -> usize {
        match self {
            Self::Darwin => 1000,
            Self::Linux => 4000,
        }
    }
}

/// `ESC [ 200 ~` and `ESC [ 201 ~`, the frame a paste may be wrapped in.
const PASTE_FRAME: usize = 12;

// The line and its frame, strictly under each kernel's limit: the Enter is the
// last byte kept.
const _: () =
    assert!(LineDiscipline::Darwin.max_line() + PASTE_FRAME < LineDiscipline::Darwin.line_keeps());
const _: () =
    assert!(LineDiscipline::Linux.max_line() + PASTE_FRAME < LineDiscipline::Linux.line_keeps());

/// The relaunch line the tab's shell runs, prefixed with a space (kept out of
/// history where the shell honours that). `heal` is the atpkg hook to source
/// first when the shell predates aterm's managed `agents/` directory (fixing
/// its PATH for later commands too); `cd` is the agent's launch directory
/// when it differs from the shell's (Claude files a conversation under its
/// launch cwd, so `--resume` must run there), entered in a subshell so the
/// person's shell keeps its directory.
///
/// # Errors
/// A line past `MAX_LINE`, a control character in any word, or a `cd` for
/// fish (no subshell form is written for it).
pub fn relaunch_line(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_env(dialect, heal, cd, &[], exe, args)
}

/// [`relaunch_line`] with `env` exported to the relaunched program: `NAME=VALUE` words in
/// front of it (`env NAME=VALUE` under fish, which has no such prefix). A same-program
/// relaunch runs the process image the agent had (the store binary, for a twin-launched
/// Claude Code), which the twin's own exports never reach.
///
/// # Errors
/// As [`relaunch_line`]; and a name that is not `[A-Z0-9_]+` or a value with a control
/// character.
pub fn relaunch_line_env(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_on(LineDiscipline::HOST, dialect, heal, cd, env, exe, args)
}

/// [`relaunch_line_env`] with explicit removals as well as values. The
/// target runs through the absolute env utility, so an unset variable stays
/// absent even when the interactive shell has since changed its exports.
/// The parent shell's environment is unchanged. This is used only where
/// exact target-profile context is required; ordinary relaunch bytes stay
/// unchanged through [`relaunch_line_env`].
///
/// # Errors
/// As [`relaunch_line_env`], an invalid removal name, or a name both set and
/// removed. The env utility must support `-u` (the supported Unix shells).
#[allow(clippy::too_many_arguments)]
pub(super) fn relaunch_line_env_clear(
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    unset: &[&str],
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_with_clear(
        LineDiscipline::HOST,
        dialect,
        heal,
        cd,
        env,
        Some(unset),
        exe,
        args,
    )
}

/// THE ONE OPERAND A WORKER'S FIRST LINE ENDS WITH (`spawn agent=claude`,
/// [`task_line_env`]): the end of options, then the task file's whole text as
/// ONE argument. The same text in zsh, bash and fish 3.4 or later (`$(…)`
/// in double quotes, which none of them splits), and FIXED: the task is never
/// a word of the line, so no byte of it is typed, quoted or parsed by the
/// shell — `$(…)` leaves its output unexpanded — and a task that opens with
/// `-` is still an operand. Trailing newlines are trimmed by every shell's
/// substitution, so the task file ends its last line as it likes. `command
/// cat --`: the line is typed at an interactive prompt, where zsh and bash
/// expand the person's aliases inside `$(…)` too, and an `alias cat=bat` (or
/// a `cat` function) would hand the agent its decorations instead of the task.
pub const TASK_OPERAND: &str = r#"-- "$(command cat -- "$ATERM_TASK_FILE")""#;

/// [`relaunch_line_env`] for a WORKER's first line (`spawn agent=claude`):
/// `env` and the quoted `exe` and `args` as a fresh start writes them, then
/// [`TASK_OPERAND`] unquoted — the one text on any line this module writes
/// that [`quote`] does not wrap, because it is a constant. Held to the same
/// bound as every relaunch line, the operand included.
///
/// # Errors
/// As [`relaunch_line_env`], and a line the operand takes past the bound.
pub fn task_line_env(
    dialect: Dialect,
    env: &[(&str, &str)],
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    let tty = LineDiscipline::HOST;
    let line = relaunch_line_on(tty, dialect, None, None, env, exe, args)?;
    let line = format!("{line} {TASK_OPERAND}");
    let max = tty.max_line();
    if line.len() > max {
        return Err(format!(
            "the line would be {} bytes (max {max})",
            line.len()
        ));
    }
    Ok(line)
}

/// [`relaunch_line`] for the kernel `tty`, whose bound it keeps.
fn relaunch_line_on(
    tty: LineDiscipline,
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    relaunch_line_with_clear(tty, dialect, heal, cd, env, None, exe, args)
}

#[allow(clippy::too_many_arguments)]
fn relaunch_line_with_clear(
    tty: LineDiscipline,
    dialect: Dialect,
    heal: Option<&Path>,
    cd: Option<&str>,
    env: &[(&str, &str)],
    unset: Option<&[&str]>,
    exe: &Path,
    args: &[String],
) -> Result<String, String> {
    if let Some((name, _)) = env.iter().find(|(n, _)| {
        n.is_empty()
            || !n
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    }) {
        return Err(format!("{name:?} is not an environment variable name"));
    }
    if let Some(names) = unset {
        for name in names {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(format!("{name:?} is not an environment variable name"));
            }
            if env.iter().any(|(key, _)| key == name) {
                return Err(format!("{name:?} is both set and removed"));
            }
        }
    }
    let exe_s = exe.to_string_lossy();
    for w in std::iter::once(exe_s.as_ref())
        .chain(args.iter().map(String::as_str))
        .chain(env.iter().map(|(_, v)| *v))
        .chain(cd)
    {
        if w.chars().any(char::is_control) {
            return Err("a control character in the line".to_string());
        }
    }
    let mut line = String::from(" ");
    if let Some(hook) = heal {
        let h = quote(dialect, &hook.to_string_lossy());
        match dialect {
            Dialect::Zsh => {
                let _ = write!(line, ". {h}; rehash; ");
            }
            Dialect::Bash => {
                let _ = write!(line, ". {h}; hash -r; ");
            }
            Dialect::Fish => {
                let _ = write!(line, "source {h}; ");
            }
        }
    }
    let mut cmd = quote(dialect, &exe_s);
    for a in args {
        cmd.push(' ');
        cmd.push_str(&quote(dialect, a));
    }
    if let Some(names) = unset {
        let mut exact = quote(dialect, "/usr/bin/env");
        for name in names {
            let _ = write!(exact, " -u {}", quote(dialect, name));
        }
        for (name, value) in env {
            let _ = write!(exact, " {}", quote(dialect, &format!("{name}={value}")));
        }
        cmd = format!("{exact} {cmd}");
    }
    let mut vars = String::new();
    for (name, value) in env.iter().filter(|_| unset.is_none()) {
        // A plain word needs no quotes (`NAME=1`); anything else is quoted as every word is.
        let plain = !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-'));
        if plain {
            let _ = write!(vars, "{name}={value} ");
        } else {
            let _ = write!(vars, "{name}={} ", quote(dialect, value));
        }
    }
    match cd {
        None if dialect == Dialect::Fish && !vars.is_empty() => {
            let _ = write!(line, "env {vars}{cmd}");
        }
        None => {
            line.push_str(&vars);
            line.push_str(&cmd);
        }
        Some(dir) => match dialect {
            Dialect::Zsh | Dialect::Bash => {
                let _ = write!(
                    line,
                    "( cd -- {} && {vars}exec {cmd} )",
                    quote(dialect, dir)
                );
            }
            Dialect::Fish => {
                return Err("fish: the agent's directory differs from the shell's".to_string());
            }
        },
    }
    let max = tty.max_line();
    if line.len() > max {
        return Err(format!(
            "the line would be {} bytes (max {max})",
            line.len()
        ));
    }
    Ok(line)
}

/// `line` ([`relaunch_line`]) prefixed with the shell taking a fresh
/// shell-integration key from the one-use file `path`, which the window
/// wrote and authorized when the sweep asked (`rekey`, for a tab whose
/// integration reads `degraded`). An old shell has no hook that would read
/// such a file and its foreground is usually an agent, so the relaunch, the
/// one line typed while the shell holds the terminal, carries the heal. Only
/// the path is typed, never the key ([`aterm_shell_integration::typed_rekey`]).
///
/// The same line UPGRADES a shell whose integration predates loaders, or
/// whose loader no body pointer reaches: the window names this build's script
/// folder and the tab's body pointer in the file for such a shell — healthy
/// too, with the key it already signs with — and the text
/// ([`aterm_shell_integration::typed_rekey_with_loader`]) sources this build's
/// loader when they are there. Where only the key-only text fits the bound,
/// that one is typed.
///
/// # Errors
/// A control character in the path, or both forms past the kernel's bound.
/// The caller then types `line` as is: the heal is best effort.
pub fn with_rekey(dialect: Dialect, line: &str, path: &Path) -> Result<String, String> {
    with_rekey_on(LineDiscipline::HOST, dialect, line, path)
}

/// [`with_rekey`] for the kernel `tty`, whose bound it keeps.
fn with_rekey_on(
    tty: LineDiscipline,
    dialect: Dialect,
    line: &str,
    path: &Path,
) -> Result<String, String> {
    let p = path.to_string_lossy();
    if p.chars().any(char::is_control) {
        return Err("a control character in the re-key path".to_string());
    }
    let shell = match dialect {
        Dialect::Zsh => aterm_shell_integration::ShellType::Zsh,
        Dialect::Bash => aterm_shell_integration::ShellType::Bash,
        Dialect::Fish => aterm_shell_integration::ShellType::Fish,
    };
    // The form that also upgrades a pre-loader shell in place, else, where
    // only it fits, the key-only form: a relaunch never loses its heal.
    let q = quote(dialect, &p);
    let takes = [
        aterm_shell_integration::typed_rekey_with_loader(shell, &q),
        aterm_shell_integration::typed_rekey(shell, &q),
    ];
    // After the one leading space that keeps the line out of history.
    let rest = line.strip_prefix(' ').unwrap_or(line);
    let max = tty.max_line();
    let mut shortest = None;
    for take in takes.into_iter().flatten() {
        let healed = format!(" {take} {rest}");
        if healed.len() <= max {
            return Ok(healed);
        }
        shortest = Some(healed.len());
    }
    Err(shortest.map_or_else(
        || "no re-key for this shell".to_string(),
        |len| format!("the healed line would be {len} bytes (max {max})"),
    ))
}

/// The one-time reference for an upgrade notice. It correlates the recorded
/// announcement with its round; new prompts never ask the agent to echo it.
/// Older transcripts that answered with this reference remain readable.
#[must_use]
pub fn ready_marker(session_id: &str, to: &Version, salt: u64) -> String {
    format!(
        "{READY_PREFIX}{:08x}",
        marker_id(session_id, &to.to_string(), salt)
    )
}

/// A marker's short id: FNV-1a over the inputs, low 32 bits — a label, not a
/// secret.
fn marker_id(session_id: &str, to: &str, salt: u64) -> u64 {
    let mut h = aterm_hash::Fnv1a64::new();
    h.write(session_id.as_bytes());
    h.write(to.as_bytes());
    h.write(&salt.to_le_bytes());
    h.finish() & 0xffff_ffff
}

/// The fixed words every READY marker opens with ([`ready_marker`]).
pub const READY_PREFIX: &str = "ATERM-UPGRADE-READY-";

/// Whether one line of an agent's words is the READY answer `marker`: the
/// line, trimmed of spaces and backticks, is the whole marker. Words that
/// merely quote the marker are no answer.
fn is_ready_line(line: &str, marker: &str) -> bool {
    line.trim().trim_matches('`') == marker
}

/// Whether an agent's `words` affirm readiness in ordinary language, or
/// contain a legacy marker reply. The transcript also requires an ordinary
/// answer to follow the matching notice ([`transcript_has_ready`]); the
/// screen uses this only to wait for that answer to be recorded.
#[must_use]
pub fn says_ready(words: &str, marker: &str) -> bool {
    !marker.is_empty()
        && (words.lines().any(|l| is_ready_line(l, marker)) || readiness::says_ready(words))
}

/// How [`prepare_prompt`]'s announcement opens, distinct from
/// [`continue_prompt`]'s (`[aterm harness] Upgraded:`). The turn-end policy
/// types nothing at a point answering an announcement: the worker owns it
/// until the restart (`IdleHost::owns_turn_end`).
pub const ANNOUNCE_HEAD: &str = concat!(harness_mark!(), " Claude Code ");

/// The announcement, typed as one ordinary user turn: asks for a good
/// stopping point ([`STOPPING_POINT`]) and asks for an ordinary-language answer,
/// never asking to cancel work still making progress.
#[must_use]
pub fn prepare_prompt(from: &Version, to: &Version, source: Source, marker: &str) -> String {
    format!(
        "{ANNOUNCE_HEAD}{to} ({}) is installed; this session runs {from}. \
         To move you onto it, aterm will restart this Claude Code in place and resume this \
         same conversation (claude --resume, same tab, same flags). {STOPPING_POINT} \
         {READINESS_ANSWER} Notice reference: {marker}. \
         {NOTICE_CLOSE}",
        source.as_str()
    )
}

/// Readiness is reported in the agent's own words; the notice reference is
/// input metadata, never a required output token.
const READINESS_ANSWER: &str = "Say in ordinary language whether this session is ready to \
    restart now, or explain what must finish first.";

/// What the announcement asks of the agent's own work. Live work is waited
/// for, never cancelled, but a wait that can never end (a poll loop counting
/// agents that have died) is not such work, and only the agent can tell. Told
/// what runs, the agent can stop such loops and answer READY. aterm itself
/// ends nothing ([`gate_restart`]).
///
/// A watch is not work either: a monitor loop (`while true; …; sleep 120;
/// done`) ends only on an alert, and the restart gate waits on anything under
/// the agent. A monitor loses nothing by stopping and restarting after the
/// resume, so the notice asks the agent to stop and note it, and the carry-on
/// reminds it to start it again ([`RESTART_WATCHES`]).
pub const STOPPING_POINT: &str = "Please get to a good stopping point first: let \
     background tasks, subagents or workflows you started finish while they are still making \
     progress (do not cancel them), but stop any background shell or task of yours that only \
     waits for something that has already ended or can never happen (a poll loop on a workflow, \
     agent, job or file that is gone): that wait is not work. A watch or monitor loop you can \
     start again after the resume (a disk or swap watch, a status poll) is not work either: \
     stop it, and note it. Save or commit work in progress, and do not start new long-running \
     work.";

/// What an upgrade's carry-on adds ([`continue_prompt`],
/// [`continue_prompt_with_model`]): the other half of [`STOPPING_POINT`]'s
/// watch clause. A relaunch's (`super::relaunch::resumed_prompt`), whose
/// notice asked nothing to be stopped, says [`RELAUNCH_RESTORES`] instead.
pub const RESTART_WATCHES: &str =
    "If you stopped a watch or loop for this restart, start it again now.";

/// What a RELAUNCH'S carry-on adds (`super::relaunch::resumed_prompt`): the
/// process that ended took with it what it ran in the background — a watch,
/// a monitor (an Artifact comment monitor did not resume after a restart,
/// MEASURED 2026-10-15, and the owner's comments went unanswered), a loop, a
/// shell — whichever way it ended (a crash, aterm's own end, a memory or a
/// stall restart), and no gate of the relaunch sees it. Fixed words, never
/// the resumed build's (H6-HARNESS-1); the agent restarts only what still
/// matters (the cross-lane review's XREV-LC-2).
pub const RELAUNCH_RESTORES: &str = "Anything this session ran in the background before (a \
     watch, a monitor, a loop, a shell) may not have come back: start again only what is no \
     longer running and you still rely on.";

/// One process under the agent that the restart waits on, as the notice
/// names it ([`running_clause`]). `age_s` is its elapsed time. `command` is
/// what it runs, cut to its point by `upgrade_drive::command_head`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// Its pid.
    pub pid: u32,
    /// Its executable's basename (`zsh`, `caffeinate`).
    pub name: String,
    /// Seconds since it started.
    pub age_s: u64,
    /// What it runs, one line; empty unless it is the agent's own Bash-tool
    /// shell (a direct child). The notice quotes only what the agent itself
    /// ran, never deeper processes (`upgrade_drive::held_of`); long paths are
    /// elided in the middle ([`HELD_PATH_CHARS`]).
    pub command: String,
}

/// The most processes [`running_clause`] names; the rest are counted.
pub const HELD_NAMED: usize = 5;

/// The longest command [`running_clause`] quotes, in characters. A longer one
/// has the middle of each long path elided first ([`HELD_PATH_CHARS`]), and
/// what is still over is cut here, the cut marked with `…`.
pub const HELD_COMMAND_CHARS: usize = 160;

/// The longest path a command over [`HELD_COMMAND_CHARS`] keeps whole, in
/// characters. A longer one keeps its root through its first name and its last
/// name, the middle elided (`D=/Users/…/wf_examplerun-1;`), so the rest of the
/// command (often what makes it a wait that never ends) stays in view; the
/// named pid gives the full text (`ps -o args= -p <pid>`). URLs are never
/// paths here (`path_middle`).
pub const HELD_PATH_CHARS: usize = 64;

/// The notice's closing sentence, the last words of every notice
/// ([`prepare_prompt`], [`prepare_prompt_with_model`]); what runs is named
/// before it
/// ([`with_running`]).
pub const NOTICE_CLOSE: &str =
    "If you cannot stop now, say why; aterm will wait and ask again later.";

/// `notice` with `running` ([`running_clause`]) inserted before its closing
/// sentence ([`NOTICE_CLOSE`]), so every notice ends with the same short
/// words: a long trailing path could wrap the caret row past the guarded
/// Enter's check. A notice without that sentence gets `running`
/// at its end.
#[must_use]
pub fn with_running(notice: &str, running: &str) -> String {
    match notice.strip_suffix(NOTICE_CLOSE) {
        Some(head) if !running.is_empty() => {
            format!("{}{running} {NOTICE_CLOSE}", head.trim_end())
        }
        _ => format!("{notice}{running}"),
    }
}

/// The notice's list of what runs under the agent, appended whenever anything
/// runs (re-asks included): each process by pid, name and age, plus the
/// command when it is the agent's own Bash-tool shell ([`Held::command`]), so
/// the agent judges from what aterm sees. Empty when nothing runs. One line
/// with no control characters, since the notice is one paste ([`held_list`]).
#[must_use]
pub fn running_clause(held: &[Held]) -> String {
    running_clause_shaped(held, HELD_NAMED, HELD_COMMAND_CHARS)
}

/// [`running_clause`] naming at most `named` processes, each command quoted
/// in `chars` ([`held_list_shaped`]).
fn running_clause_shaped(held: &[Held], named: usize, chars: usize) -> String {
    let list = held_list_shaped(held, named, chars);
    if list.is_empty() {
        return String::new();
    }
    format!(" Running under you now, as aterm sees it: {list}.")
}

/// What runs under the agent as a bare list, with no lead-in and no final
/// period: `pid 63492 (zsh, 5d4h): <command>; pid …; and 2 more`. The notice
/// wraps it for the agent ([`running_clause`]) and the give-up's ledger row
/// for the owner. At most [`HELD_NAMED`] processes are named and the rest
/// counted, and each command is quoted in [`HELD_COMMAND_CHARS`], its long
/// paths elided before the cut (`command_words`). Empty when nothing runs.
#[must_use]
pub fn held_list(held: &[Held]) -> String {
    held_list_shaped(held, HELD_NAMED, HELD_COMMAND_CHARS)
}

/// [`held_list`] naming at most `named` processes, each command quoted in
/// `chars`.
fn held_list_shaped(held: &[Held], named: usize, chars: usize) -> String {
    // OLDEST FIRST (design record 2026-09-28, §3.2 C7c): only the first
    // [`HELD_NAMED`] are named, so the long-lived process likeliest to hold
    // the restart is never among the merely counted. A stable sort, so
    // processes of the same age keep their given order.
    let mut held: Vec<&Held> = held.iter().collect();
    held.sort_by_key(|h| std::cmp::Reverse(h.age_s));
    let mut out = String::new();
    for (i, h) in held.iter().take(named).enumerate() {
        if i > 0 {
            out.push_str("; ");
        }
        let command = command_words(&h.command, chars);
        let _ = write!(
            out,
            "pid {} ({}, {})",
            h.pid,
            one_line(&h.name, 32),
            span(h.age_s)
        );
        if !command.is_empty() {
            let _ = write!(out, ": {command}");
        }
    }
    if held.len() > named {
        let _ = write!(out, "; and {} more", held.len() - named);
    }
    out
}

/// `text` as one line of at most `max` characters: every control character
/// a space, runs of spaces one, and a cut marked with `…`.
fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// A held command as [`held_list`] quotes it: [`one_line`], and if over `max`,
/// each long path's middle elided first ([`path_middle`]) before the cut.
/// Every character stays the agent's own (or the `…`), in order. A word
/// inside a quote is never elided: there a path can be the very pattern a
/// wait turns on (`grep -m1 -F "wrote <path>"`).
fn command_words(text: &str, max: usize) -> String {
    let flat = one_line(text, usize::MAX);
    if flat.chars().count() <= max {
        return flat;
    }
    let mut quote = None;
    let words: Vec<String> = flat
        .split(' ')
        .map(|word| {
            let elided = quote.is_none().then(|| path_middle(word)).flatten();
            quote = quote_after(quote, word);
            elided.unwrap_or_else(|| word.to_string())
        })
        .collect();
    one_line(&words.join(" "), max)
}

/// The quote a shell word leaves open, given the one open before it: `'`
/// opens and closes a single quote, `"` a double one, and outside a single
/// quote a `\` escapes the character after it.
fn quote_after(mut quote: Option<char>, word: &str) -> Option<char> {
    let mut escaped = false;
    for c in word.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if quote == Some(c) {
            quote = None;
        }
    }
    quote
}

/// `word` with its path's middle elided (`D=/Users/…/wf_examplerun-1;`) when it
/// is a path over [`HELD_PATH_CHARS`]: at most an assignment (`D=`, `--out=`)
/// or redirection (`>`, `2>>`), a `/` or `~/` root, then only letters, digits
/// and `/._-+@,`, then at most a shell separator (`;`, `&`, `|`, `)`). URLs,
/// globs, variables, quotes and escapes never match. `None` otherwise, or when
/// the result would be no shorter.
fn path_middle(word: &str) -> Option<String> {
    if word.chars().count() <= HELD_PATH_CHARS {
        return None;
    }
    let lead = match word.split_once('=') {
        Some((name, _))
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) =>
        {
            name.len() + 1
        }
        _ => {
            let redirect = word
                .trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '&' | '<' | '>'));
            let redirect = &word[..word.len() - redirect.len()];
            if redirect.contains(['<', '>']) {
                redirect.len()
            } else {
                0
            }
        }
    };
    let rest = &word[lead..];
    let path = rest.trim_end_matches([';', '&', '|', ')']);
    let root = usize::from(path.starts_with("~/"));
    let rooted = &path[root..];
    if !rooted.starts_with('/')
        || !rooted.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | ',')
        })
    {
        return None;
    }
    // Every byte is ASCII from here, so each index below is a character's.
    let named = path.len() - rooted.trim_start_matches('/').len();
    let first = named + path[named..].find('/')?;
    let last = path.trim_end_matches('/').rfind('/')?;
    if last <= first {
        return None;
    }
    let elided = format!(
        "{}{}/…{}{}",
        &word[..lead],
        &path[..first],
        &path[last..],
        &rest[path.len()..]
    );
    (elided.chars().count() < word.chars().count()).then_some(elided)
}

/// [`prepare_prompt`] for a restart that also (or only, when `to == from`)
/// moves the conversation to `model` from `running`, saying why
/// ([`models::move_why`]). Still starts with [`ANNOUNCE_HEAD`] so the turn-end
/// policy recognises it.
#[must_use]
pub fn prepare_prompt_with_model(
    from: &Version,
    to: &Version,
    source: Source,
    marker: &str,
    model: Option<&str>,
    running: Option<&str>,
) -> String {
    let Some(model) = model else {
        return prepare_prompt(from, to, source, marker);
    };
    let why = models::move_why(running, model).words();
    let what = if to == from {
        format!(
            "{ANNOUNCE_HEAD}{to} can run {model}, {why}, which this session does not run. To \
             move you onto it, aterm will restart this Claude Code in place and resume this same \
             conversation on {model} (claude --resume --model {model}, same tab, same flags)."
        )
    } else {
        format!(
            "{ANNOUNCE_HEAD}{to} ({}) is installed; this session runs {from}. To move you onto \
             it, and onto {model} ({why}), aterm will restart this Claude Code in place and \
             resume this same conversation (claude --resume --model {model}, same tab, same \
             flags).",
            source.as_str()
        )
    };
    format!("{what} {STOPPING_POINT} {READINESS_ANSWER} Notice reference: {marker}. {NOTICE_CLOSE}")
}

/// [`continue_prompt`] for a relaunch that asked for `model`: names the model
/// before the restart (`ran`), the one it runs now, and why
/// ([`models::move_why`]). A command-line `--model` is what a resumed Claude
/// Code runs, over the transcript's model and the saved default; the outcome
/// row confirms it from the first answer ([`restart_outcome_listed`]).
#[must_use]
pub fn continue_prompt_with_model(
    from: &Version,
    to: &Version,
    ran: Option<&str>,
    model: Option<&str>,
) -> String {
    let Some(model) = model else {
        return continue_prompt(from, to, ran);
    };
    let build = if to == from {
        format!("on Claude Code {to}")
    } else {
        format!("on Claude Code {to} (from {from})")
    };
    let runs = ran.map_or_else(
        || "it now runs".to_string(),
        |m| format!("it ran {m} before the restart and now runs"),
    );
    let why = models::move_why(ran, model).words();
    format!(
        "{HARNESS_MARK} Upgraded: this session was restarted {build} and resumed; {runs} \
         {model}, {why} (for this session only: your default model is unchanged). \
         {RESTART_WATCHES} {CARRY_ON}"
    )
}

/// The continuation, typed once the relaunched process holds the same session.
/// `ran` is the model the session's last turn before the restart named
/// ([`transcript_model`]), said in one neutral clause so the agent — and the
/// person reading the tab — knows what it ran before; `None` (no turn named
/// one) leaves the clause out.
#[must_use]
pub fn continue_prompt(from: &Version, to: &Version, ran: Option<&str>) -> String {
    let resumed = match ran {
        Some(model) => format!("; it ran {model} before the restart and was resumed"),
        None => " and resumed".to_string(),
    };
    format!(
        "{HARNESS_MARK} Upgraded: this session was restarted on Claude Code {to} (from \
         {from}){resumed}. {RESTART_WATCHES} {CARRY_ON}"
    )
}

/// A restart's continuation `text` (the upgrade's [`continue_prompt`], a
/// relaunch's `super::relaunch::resumed_prompt`) for a conversation whose
/// last words claim the work is done ([`FinishedScan::said_done`]) under
/// Cruise: its facts, then the Cruise prompt in place of [`CARRY_ON`] — where
/// the turn-end policy types only that prompt at a done claim, never `keep
/// going` (the live lab's LIVE-RL-1) — worded as the policy's
/// ([`turn_end::done_check_text`]): where the report names an irreversible
/// act (`irreversible`, [`FinishedScan::said_irreversible`]) the prompt's
/// carry-on alone would read as consent to it, so
/// [`turn_end::DONE_CHECK_REVERSIBLE`] follows (the review's RR4-RL-3). A
/// text not ending so is kept as it is. Under Land a relaunch types nothing
/// of its own at a done claim: the loop's next act is Land's
/// (`super::relaunch`'s `done:landing`).
#[must_use]
pub fn checking(text: &str, irreversible: bool) -> String {
    text.strip_suffix(CARRY_ON).map_or_else(
        || text.to_string(),
        |facts| format!("{facts}{}", turn_end::done_check_text(None, irreversible)),
    )
}

/// How every continuation after a restart ends (the upgrade's
/// [`continue_prompt`] and a relaunch's `super::relaunch::resumed_prompt`,
/// every lane's — Claude Code's, Gemini CLI's, agy's, Muse Code's): nobody
/// is at the keyboard, so an agent waiting on the user decides for itself
/// and keeps going (as `[harness] answer_text`), and what only a person can
/// give goes on a NEEDS YOU line, which the loop puts on the person's screen
/// ([`crate::supervise::policy::needs::NEEDS_HINT`]).
pub const CARRY_ON: &str = concat!(
    "Continue where you left off. If you were waiting on the user, nobody is here to answer: \
     decide for yourself, prefer reversible steps, and keep going. ",
    crate::supervise::policy::needs::needs_hint!()
);

/// THE ONE LINE A QUIET RESTART TYPES, and only where the resumed build
/// itself warned that something did not come back ([`resumed_build_warned`])
/// into a conversation with a task its worker has not finished
/// (`super::relaunch::restore_look`): that the session was restarted on
/// `to`, that something it ran did not come back, and to restore what it
/// relies on (`to` `None`: a build whose version does not parse, left
/// unnamed). `upgraded`: the restart was the upgrade's (`Upgraded:`); any
/// other — an account move, a relaunch whose carry-on typed nothing — says
/// `Restarted:`. aterm's own fixed words: the vendor's row is never quoted
/// (H6-HARNESS-1), and nothing else is asked.
#[must_use]
pub fn restore_prompt(to: Option<&Version>, upgraded: bool) -> String {
    let on = to.map_or_else(String::new, |to| format!(" {to}"));
    let head = if upgraded { "Upgraded" } else { "Restarted" };
    format!(
        "{HARNESS_MARK} {head}: this session was restarted on Claude Code{on} and resumed \
         as it was. Claude Code reported that something this session ran did not come back \
         after the restart: restore what you rely on (a watch, a monitor, a loop). Nothing \
         else is asked."
    )
}

/// WHETHER THE RESUMED BUILD WARNED, AS IT RESUMED, THAT SOMETHING DID NOT
/// COME BACK (MEASURED 2026-10-15, transcript `71159b44`: a quiet restart onto
/// 2.1.300 said `done:resumed`, and a second later the resumed build wrote
/// that the session's Artifact comment monitor did not resume): a main-chain
/// row `{"type":"system","subtype":"informational","level":"warning"}`
/// written by build `version`, its time within `from..=to` (unix seconds;
/// `from` `0`: a record that names no relaunch, any time up to `to`).
/// Decided by the vendor's level field alone, never its words, which are
/// neither read nor typed (H6-HARNESS-1). `jsonl`: the rows past the
/// restart's mark. Non-JSON lines are skipped.
#[must_use]
pub fn resumed_build_warned(jsonl: &str, version: &str, from: u64, to: u64) -> bool {
    !version.is_empty()
        && jsonl.lines().any(|line| {
            if !line.contains("\"warning\"") {
                return false;
            }
            let Ok(v) = super::transcript::conversation(line) else {
                return false;
            };
            let field = |key: &str| v.get(key).and_then(Value::as_str);
            let at = field("timestamp").and_then(models::parse_utc);
            field("type") == Some("system")
                && field("subtype") == Some("informational")
                && field("level") == Some("warning")
                && v.get("isSidechain").and_then(Value::as_bool) != Some(true)
                && field("version") == Some(version)
                && at.is_some_and(|at| at >= from && at <= to)
        })
}

/// The model the last assistant turn in `jsonl` (a transcript tail) names as
/// `message.model`. Non-JSON lines (a cut first line, a half-written last one)
/// are skipped.
///
/// Only a main-chain `assistant` row (not `isSidechain`) whose model looks
/// like a model id ([`is_model_id`]) counts; Claude Code's own `<synthetic>`
/// rows (limit notices, API errors, `No response requested.`) are skipped.
#[must_use]
pub fn transcript_model(jsonl: &str) -> Option<String> {
    assistant_models(jsonl, None).last()
}

/// The model the first assistant turn written by build `version` names in
/// `jsonl`, by [`transcript_model`]'s rules: for the bytes past a restart's
/// mark, whose first new-build answer is the resumed session's. Every row
/// carries the `version` that wrote it, so a late row from the old process
/// (alive past SIGTERM for one more turn), or one with no version, is skipped.
#[must_use]
pub fn transcript_first_model(jsonl: &str, version: &str) -> Option<String> {
    assistant_models(jsonl, Some(version)).next()
}

/// Every model the session's assistant turns in `jsonl` name, in order — only
/// the rows the build `version` wrote, when one is given.
fn assistant_models<'a>(
    jsonl: &'a str,
    version: Option<&'a str>,
) -> impl Iterator<Item = String> + 'a {
    jsonl.lines().filter_map(move |line| {
        if !line.contains("assistant") {
            return None;
        }
        let v = super::transcript::metadata(line).ok()?;
        if v.get("type").and_then(Value::as_str) != Some("assistant")
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || version.is_some_and(|want| v.get("version").and_then(Value::as_str) != Some(want))
        {
            return None;
        }
        let model = v.get("message")?.get("model")?.as_str()?;
        is_model_id(model).then(|| model.to_string())
    })
}

/// Whether `m` looks like a model id: 1 to 256 bytes of ASCII letters, digits
/// and `. _ - : @ / [ ]` (`claude-opus-5-5[1m]`, `us.anthropic.…-v1:0`, an
/// inference profile ARN). Not `<synthetic>`, nor anything with a space or
/// control character: the id is typed into the tab, so a third party's file
/// must never put a keystroke or an unbounded run of bytes there.
pub(crate) fn is_model_id(m: &str) -> bool {
    (1..=256).contains(&m.len())
        && m.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@/[]".contains(&b))
}

/// The outcome line for the owner once the resumed session answered (or did
/// not): the build it restarted on and the model its first answer named. A
/// different model is said with what decided it, not as a fault: `kept`, the
/// `--model` carried from the launch ([`launch_model`]), which undoes a
/// `/model` choice made since and resolves an alias against the new build;
/// with none kept, Claude Code's default at the relaunch. `after` of `None`
/// is unconfirmed, with the model before when known.
#[must_use]
pub fn restart_outcome(
    to: &str,
    before: Option<&str>,
    after: Option<&str>,
    kept: Option<&str>,
) -> String {
    let head = format!("claude restarted on {to} · model");
    match (before, after) {
        (Some(b), Some(a)) if b != a => {
            let why = kept.map_or_else(
                || "a session launched without --model takes the current default".to_string(),
                |m| format!("the relaunch kept the launch's --model {m}"),
            );
            format!("{head} {b} -> {a} ({why}; /model changes it)")
        }
        (_, Some(a)) => format!("{head} {a}"),
        (Some(b), None) => format!("{head} unconfirmed (it ran {b} before the restart)"),
        (None, None) => format!("{head} unconfirmed"),
    }
}

/// [`restart_outcome`] for a relaunch that asked for `chosen` by the model
/// rule (`upgrade_models`): a change is said with its reason
/// ([`models::move_why`] from `before`), and a model other than `chosen` as
/// not taken.
#[must_use]
pub fn restart_outcome_listed(
    to: &str,
    before: Option<&str>,
    after: Option<&str>,
    chosen: &str,
) -> String {
    let head = format!("claude restarted on {to} · model");
    let base = |m: &str| m.strip_suffix("[1m]").unwrap_or(m).to_string();
    let (asked, chose) = match models::move_why(before, chosen) {
        models::MoveWhy::Family => (
            format!("the relaunch asked for {chosen}, the newest of its family"),
            "the newest of its family",
        ),
        models::MoveWhy::List => (
            format!("the priority list asked for {chosen}"),
            "the priority list chose it",
        ),
        models::MoveWhy::Unknown => (
            format!("the relaunch asked for {chosen}"),
            "the model rule chose it",
        ),
    };
    match (before, after) {
        (_, Some(a)) if base(a) != base(chosen) => {
            format!("{head} {a} ({asked}; it was not taken)")
        }
        (Some(b), Some(a)) if base(b) != base(a) => {
            format!("{head} {b} -> {a} ({chose}; /model changes it)")
        }
        (_, Some(a)) => format!("{head} {a}"),
        (Some(b), None) => {
            format!("{head} unconfirmed (it ran {b} before the restart; asked for {chosen})")
        }
        (None, None) => format!("{head} unconfirmed (asked for {chosen})"),
    }
}

/// Whether the agent's last answer affirms readiness after the announcement
/// carrying `marker`. Historical replies with the marker on its own line
/// remain readable even when the notice is outside this transcript tail.
/// Non-JSON lines are skipped. Only the main-chain assistant's own words count.
///
/// Readiness is consent only while it is still the agent's last word, so the
/// answer counts only in the newest main-chain assistant row of the session's
/// own model (not `isSidechain`, not `<synthetic>`, [`is_model_id`]), and
/// only when no main-chain user row after it is an announcement
/// ([`ANNOUNCE_HEAD`]) or a direction ([`is_direction`]: a person's words, an
/// Esc, a peer's message, the supervisor's continuation), whether or not the
/// agent has written since.
#[must_use]
pub fn transcript_has_ready(jsonl: &str, marker: &str) -> bool {
    let (mut ready, mut matching_notice) = (false, false);
    for line in jsonl.lines() {
        match turn_of(line) {
            Some(Turn::Notice(message)) => {
                ready = false;
                matching_notice = notice_matches(&message, marker);
            }
            Some(Turn::Direction) => (ready, matching_notice) = (false, false),
            Some(Turn::Agent(message)) => ready = answers(&message, marker, matching_notice),
            None => {}
        }
    }
    ready
}

/// Whether someone has directed the conversation since the agent last
/// answered the upgrade and the agent took that direction up, so an owed
/// release ([`release_prompt`]) would be typed over newer direction. `jsonl`
/// is a transcript tail, `markers` every READY marker the round typed
/// (`upgrade_drive::St::asked`).
///
/// A direction counts only after the agent's latest READY answering one of
/// `markers` (with none, after the latest notice, [`ANNOUNCE_HEAD`]); one
/// before the READY was answered by holding for the restart. And only once
/// the agent has answered it with a non-READY row of its own model
/// ([`Turn::Agent`]): an unanswered direction may still be answered with
/// READY, and one met only by an Esc or a `<synthetic>` row leaves the agent
/// idle, so the release is still typed.
///
/// A direction is a main-chain user turn of typed words: a person's (a
/// prompt, a `!` command, an Esc, `[Request interrupted by user]`), a peer's
/// (`[from s-…]`), or the supervisor's continuation or answer. Not the
/// harness's own lines (`[aterm harness] …`), nor rows Claude Code writes
/// itself: tool results, `isMeta` rows, compaction summaries,
/// `<task-notification>`, commands and their output (`<command-name>`,
/// `<local-command-stdout>`, `<bash-stdout>`). With no notice in the tail the
/// whole tail is read.
#[must_use]
pub fn directed_since_ready(jsonl: &str, markers: &[String]) -> bool {
    let (mut told, mut directed) = (false, false);
    let mut matching_notice = false;
    for line in jsonl.lines() {
        match turn_of(line) {
            Some(Turn::Notice(message)) => {
                (told, directed) = (false, false);
                matching_notice = markers.iter().any(|m| notice_matches(&message, m));
            }
            Some(Turn::Direction) => {
                told = true;
                matching_notice = false;
            }
            Some(Turn::Agent(message)) => {
                if markers
                    .iter()
                    .any(|m| answers(&message, m, matching_notice))
                {
                    (told, directed) = (false, false);
                } else if told {
                    directed = true;
                }
            }
            None => {}
        }
    }
    directed
}

/// WHETHER A CONVERSATION'S TASK IS FINISHED, BY ITS OWN RECORD — LANDED
/// (FOREVER CRUISE / FOREVER LAND): the worker answered one of Autopilot
/// Land's acts ([`Asked::Land`]: its instruction, a nudge, its answer to a
/// question once the instruction was read — today's words or a build
/// before's) with a CLEAN DEFENSE ([`turn_end::clean_defense`]: a `Nothing
/// left:` line, no NEEDS YOU beside it, no open work in Land's strict
/// reading) — and NOBODY HAS ASKED ANYTHING SINCE: no prompt of a person's
/// or an orchestrator's, no command of theirs that started a turn. Nothing
/// else finishes a task: not the retired `ATERM-TASK-DONE`, not a done
/// reply to a check (today's Cruise prompt or an older build's done check),
/// not an idle report. The record cannot see the session's repository, the
/// policy's fourth condition of a landing: the loop holds a landing to it,
/// and the record speaks only of the words.
/// Folded over the transcript one line at a time ([`Self::line`]),
/// oldest first, so a caller reads on from where it stopped
/// (`upgrade_drive::Finished`). Unlike [`TaskScan`] it never settles: the
/// next prompt of someone else's opens the task again.
///
/// A TURN NOBODY ASKED REOPENS NOTHING, whatever its answer says: Claude
/// Code's own wakes — a background task's `<task-notification>` and its
/// usage-limit continuation, by the row's `origin.kind`
/// (`task-notification`, `auto-continuation`; an older build's notification
/// by its `<task-notification>` words) — the harness's own turns
/// ([`HARNESS_MARK`], and what the loop typed, by its ledger, as
/// [`TaskScan`] reads them), a local command's output, and a relaunched
/// agent's start-up, which writes no prompt at all. Neither do rows that ask
/// nothing: compaction summaries (`isCompactSummary`), tool results,
/// Claude Code's expansions (`isMeta`), Esc's `[Request interrupted…`.
/// A check of the harness's — the Cruise prompt, an older build's done
/// check or open-items challenge — opens the finish again: typed into a
/// landed task, the task is open (a Land changed to Cruise reopens it, as the
/// policy reads it).
/// A PEER'S MESSAGE is someone's (`origin.kind` `peer`, which Claude Code
/// records `isMeta`), and so is a row of any kind this fold does not know,
/// `isMeta` or not: in doubt the task is opened, as the screen would open
/// it. Decided by the origin's kind, never by `promptSource`: a peer's
/// message is `system`-sourced too. A command of
/// someone else's (`/usage`, `/model`) opens the task only once it starts a
/// turn, as [`TaskScan`]'s pending command does: a person's look at the
/// limits of a finished tab asks the worker nothing. So too a person's `!`
/// shell command (`<bash-input>…`), which runs in the terminal and asks the
/// model nothing; its output (`<bash-stdout>…`) is no prompt. A PROMPT TYPED
/// WHILE THE WORKER WORKS is recorded only as an attachment absorbed into
/// the running turn (`queued_command`, MEASURED on 2.1.267 to 2.1.300: no
/// user row is written for it): read by its origin and words as a user row
/// of them would be, someone's opens the task, and the turn it joined goes on.
///
/// THE FINISH IS ITS TURN'S LAST WORD: a later text row of the session's own
/// model in the SAME turn (no prompt row between) that is no clean defense
/// takes it back, as the screen's rule reads only the last words. A turn
/// begins at a row that asks something — a prompt, a command, a check, a
/// land act, one of the turns nobody asked above — and at a row that asks
/// nothing which Claude Code numbered a turn of its own (a `promptId` other
/// than the last row's: a `/loop`'s wake, known on a build that numbers no
/// turn by its `scheduledTaskId`, its `turnOrigin` or, for a missed one-shot
/// task's notice, its fixed words: [`begins_unnumbered`]). A row that asks
/// nothing under the running turn's `promptId`, or under none, goes on that
/// turn: a Stop hook's feedback (`Stop hook feedback: … not met`) or
/// `/goal`'s check-in, after which the worker works on in the same turn
/// because its own goal says the task is not done — its later words take
/// the finish back (MEASURED 2026-10-15: 39 hook-feedback and 16 check-in
/// rows on this machine, each under its turn's id, a second after the
/// model's text). Rows `<synthetic>` writes (API errors, `No response
/// requested.`), `isApiErrorMessage` rows and subagents' rows say nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FinishedScan {
    /// The record says the task is finished: landed.
    finished: bool,
    /// The finish was said in the turn running now: a later text row of the
    /// same turn says the turn's last word again.
    said: bool,
    /// The turn running now was asked by one of Land's acts
    /// ([`Asked::Land`]): a clean defense answering it lands the task.
    land: bool,
    /// The model's last text row in the turn running now claims its worker
    /// is done ([`turn_end::says_done`]: a done report, a defense, an idle
    /// report, the retired token), whatever asked the turn
    /// ([`Self::said_done`]).
    done_said: bool,
    /// The turn running now was asked by the live upgrade's notice
    /// ([`is_announcement`]): its answer — the stopping point it asked for,
    /// `READY-…` — is never a done report and never takes one back, as the
    /// turn-end policy reads a notice's answer (`HostTurn::Notice`; the
    /// review's RR4-RL-2).
    notice: bool,
    /// That done report also names an irreversible act
    /// ([`turn_end::Said::Irreversible`]): the Cruise prompt asked after it
    /// says to approve nothing irreversible, as the turn-end policy's does
    /// ([`turn_end::done_check_text`]; the review's RR4-RL-3).
    done_irreversible: bool,
    /// A WALL CUT A LAND ACT'S TURN: Claude Code's own error row
    /// (`isApiErrorMessage`: an API error, a limit) came while it ran, and
    /// the worker has said nothing since. The turn-end policy's retry of the
    /// wall, typed next, delivers the act again, so its answer is the act's,
    /// as the policy reads it (`TurnEndState::cut_land`; the merge review's
    /// M5-R1): the next row that asks something keeps the act where it is the
    /// supervisor's plain text ([`supervisors_plain`]: the wall's retry, the
    /// usage resume) or Claude Code's own continuation of the limit
    /// ([`vendor_continues`]), and clears this either way.
    cut: bool,
    /// A command of someone else's waits: the turn it starts opens the task.
    command: bool,
    /// The `promptId` of the last main-chain user row read: the turn running
    /// now, as Claude Code numbers it.
    turn: Option<String>,
    /// What the session's supervisor typed, and when ([`TaskScan`]'s).
    ours: Vec<Typed>,
    /// The turn the record read so far ends on, by its keys ([`TurnFold`]:
    /// [`Self::last_turn_key`], [`Self::last_turn`]).
    turn_fold: TurnFold,
    /// LAND'S INSTRUCTION WAS READ (or a nudge, typed only after it) since
    /// the last prompt of someone's: Land's answer to a question
    /// ([`lands_answer`]) is then a land act too ([`Asked::Land`]), as the
    /// turn-end policy lands a defense answering it once the instruction was
    /// typed (`TurnEndState::acted`).
    land_typed: bool,
    /// A row read SETTLES THE FINISH, whatever came before it
    /// ([`Self::settled`]).
    settled: bool,
}

/// What one user row asks, to [`FinishedScan`].
enum Asked {
    /// A person's or an orchestrator's prompt.
    Theirs,
    /// A command of theirs: a turn only if it starts one.
    Command,
    /// A check of the supervisor's on a done claim: today's Cruise prompt
    /// (`turn_end::DONE_CHECK`, `continue-done-check@v1`) or an older
    /// build's done check ([`turn_end::RETIRED_DONE_CHECKS`]). Its answer
    /// lands nothing (Cruise never ends); typed into a landed task, the task
    /// is open again.
    Check,
    /// One of Autopilot Land's acts: its instruction or nudge
    /// (`turn_end::LAND`, `turn_end::LAND_NUDGE`, or a build before's), or
    /// Land's answer to a question (`turn_end::LAND_ANSWER`, by its ledger
    /// rule: [`lands_answer`]) once the instruction was read since the last
    /// prompt of someone's ([`FinishedScan::land_typed`]) — before it, the
    /// harness's own line. A clean defense answering it lands the task.
    Land,
    /// The challenges older builds' supervisors typed (retired:
    /// [`turn_end::RULE_OPEN_ITEMS`], [`turn_end::RULE_UNCOMMITTED`], known by
    /// [`turn_end::is_challenge_text`]): a check's kin, as older records hold
    /// them, read as a check is.
    Challenge,
    /// A turn nobody asked: the harness's own, Claude Code's own wake, a
    /// local command's output.
    Nobody,
    /// No prompt at all: an expansion, Esc, a caveat.
    Nothing,
}

impl FinishedScan {
    /// Read on knowing `ours`: the texts the session's supervisor typed, and
    /// when ([`TaskScan::supervisor_typed`]).
    pub fn supervisor_typed(&mut self, ours: &[Typed]) {
        ours.clone_into(&mut self.ours);
    }

    /// Folds one transcript line in and returns whether the task is
    /// finished now ([`Self::finished`]). Non-JSON lines are skipped.
    pub fn line(&mut self, line: &str) -> bool {
        if !(line.contains("\"user\"")
            || line.contains("\"assistant\"")
            || line.contains("\"queued_command\"")
            || line.contains("\"turn_duration\""))
        {
            return self.finished;
        }
        let Ok(v) = super::transcript::conversation(line) else {
            return self.finished;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return self.finished;
        }
        self.turn_fold.row(&v);
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => self.answered(&v),
            Some("user") => self.asked(&v),
            Some("attachment") => self.queued(&v),
            _ => {}
        }
        self.finished
    }

    /// Whether the record read so far says the task is finished: landed.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Whether a row read so far SETTLES THE FINISH whatever came before it:
    /// a prompt of someone's (a command of theirs once it starts a turn, one
    /// typed while the worker worked), a check or a land act of the
    /// harness's (each opens the finish, its answer the only one that can
    /// land it), or an answer that lands the task. Every other row — Claude
    /// Code's own wakes and their answers, the harness's own lines — leaves
    /// the finish as it stood before it, so a read that begins after the
    /// record's head and settles nothing cannot say it
    /// (`upgrade_drive::Finished`, which reads such a window from further
    /// back; ATERM-KI-1016-1).
    #[must_use]
    pub fn settled(&self) -> bool {
        self.settled
    }

    /// Whether the conversation's LAST WORDS claim the work is done: the
    /// model's last text row says so ([`turn_end::says_done`]: a done
    /// phrase, a `Nothing left:` defense, an idle report, the retired token),
    /// whatever asked its turn — a check's answer included, as Cruise reads
    /// every done claim — and nobody has asked the worker anything since: a
    /// prompt of someone's opens it, a command of theirs once it starts a
    /// turn, while a look at the tab (`/usage`, `/model`, a `!` shell
    /// command, their output), the harness's own line and Claude Code's own
    /// wake take nothing back until an answer with words does (the review's
    /// RR4-RL-1). What a relaunch's carry-on answers under Cruise with the
    /// Cruise prompt ([`checking`]), never `keep going` (the live lab's
    /// LIVE-RL-1), and under Land leaves to the loop.
    #[must_use]
    pub fn said_done(&self) -> bool {
        self.done_said
    }

    /// THE TURN THE RECORD READ SO FAR ENDS ON, by its key, as
    /// [`last_turn_key`] reads it from a tail — kept as the fold goes, so a
    /// read that reaches back as far as the finish does
    /// (`upgrade_drive::FINISHED_ONCE_BYTES`, read on from there) names it
    /// however much Claude Code wrote after the turn that names no turn: an
    /// idle compaction's boundary, its summary, the files and skills it
    /// restores (measured up to 216 KB idle, 322 KB mid-turn; the merge
    /// review's XR-1). Read from a 256 KiB tail, a left exchange's
    /// `answer:` key was lost under such a block, and a relaunch typed its
    /// carry-on into the person's exchange.
    #[must_use]
    pub fn last_turn_key(&self) -> Option<String> {
        self.turn_fold.turn().last
    }

    /// THE TURN THE RECORD READ SO FAR ENDS ON, by every key that names it
    /// ([`LastTurn`]): its last row's ([`Self::last_turn_key`]) and the
    /// prompt that opened it — what a left exchange's row is matched by.
    #[must_use]
    pub fn last_turn(&self) -> LastTurn {
        self.turn_fold.turn()
    }

    /// Whether that done report ([`Self::said_done`]) names an irreversible
    /// act ([`turn_end::classify_said`]): the Cruise prompt asked after it
    /// adds [`turn_end::DONE_CHECK_REVERSIBLE`] ([`checking`]).
    #[must_use]
    pub fn said_irreversible(&self) -> bool {
        self.done_said && self.done_irreversible
    }

    /// An assistant row: the turn a pending command started, and the
    /// model's words.
    fn answered(&mut self, row: &Value) {
        // A command of someone else's that started a turn was a prompt of
        // theirs (any answer counts, `<synthetic>` included: [`TaskScan`]).
        if std::mem::take(&mut self.command) {
            (self.finished, self.said, self.land, self.done_said) = (false, false, false, false);
            (self.notice, self.cut) = (false, false);
            self.settled = true;
        }
        if row.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
            // A wall in a land act's turn, whatever was answered before it:
            // the retry's answer is the act's ([`Self::cut`]).
            self.cut = self.land;
            return;
        }
        let Some(message) = row.get("message") else {
            return;
        };
        if message
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|m| !is_model_id(m))
        {
            return;
        }
        // The worker went on past the wall's row: no wall ends the turn.
        self.cut = false;
        let Some(words) = answer_words(message) else {
            return;
        };
        // A done claim at every yield, the Cruise prompt's too, as the
        // turn-end policy reads one ([`turn_end::says_done`]). The notice's
        // answer says nothing of the task either way.
        if !self.notice {
            self.done_said = turn_end::says_done(Some(&words));
            self.done_irreversible = self.done_said
                && matches!(
                    turn_end::classify_said(Some(&words)),
                    turn_end::Said::Irreversible(_)
                );
        }
        // Only a clean defense at a land act's yield lands the task: never
        // the token, never a check's answer, never a defense that names a
        // need or open work.
        if self.land && turn_end::clean_defense(Some(&words)) {
            (self.finished, self.said, self.settled) = (true, true, true);
        } else if self.said {
            (self.finished, self.said) = (false, false);
        }
    }

    /// A user row: a turn begins, unless it is a tool's result, a
    /// compaction's summary, or a row that asks nothing under the running
    /// turn's `promptId` or none (a Stop hook's feedback).
    fn asked(&mut self, row: &Value) {
        let id = row.get("promptId").and_then(Value::as_str);
        // A `/loop`'s wake is a turn of its own: by its new `promptId`, and
        // on a build that numbers no turn as [`begins_unnumbered`] reads it
        // (ATERM-KI-1016-1: read as part of the finish's turn, its answer
        // took the finish back).
        let anew = numbered_anew(id, self.turn.as_deref(), || begins_unnumbered(row));
        if anew {
            self.turn = id.map(str::to_owned);
        }
        if row.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            || only_tool_results(row)
        {
            return;
        }
        let asked = match asked_by(row, &self.ours) {
            Asked::Nobody if self.land_typed && lands_answer(row, &self.ours) => Asked::Land,
            asked => asked,
        };
        if matches!(asked, Asked::Nothing) && !anew {
            return;
        }
        // A done report stands until an answer with words says otherwise,
        // or someone asks the worker something: a person's command or `!`
        // shell command, a local command's output, the harness's own line
        // and Claude Code's wake ask it nothing (the review's RR4-RL-1: a
        // `/usage` at a done tab, then a crash, and the relaunch typed `keep
        // going` into the done report). Nor does a check take it back before
        // its answer does (RR4-RL-4): a check the worker never answered (a
        // crash, a stall, aterm's end) leaves the done report the last
        // words, and the relaunch asks the Cruise prompt again.
        let notice = row.get("message").is_some_and(is_announcement);
        // A WALL'S RETRY DELIVERS AGAIN THE LAND ACT THE WALL CUT
        // ([`Self::cut`]): the act stands, its answer still to come. So does
        // the vendor's own continuation of the limit that cut it
        // ([`vendor_continues`]). Any other row that asks something ends the
        // cut.
        let retry = std::mem::take(&mut self.cut)
            && matches!(asked, Asked::Nobody)
            && (supervisors_plain(row, &self.ours) || vendor_continues(row));
        (self.said, self.notice) = (false, notice);
        if retry {
            self.command = false;
            return;
        }
        self.land = false;
        match asked {
            Asked::Theirs => {
                (self.finished, self.command, self.done_said) = (false, false, false);
                (self.land_typed, self.settled) = (false, true);
            }
            Asked::Command => self.command = true,
            // A check or a land act opens the finish: only the answer to a
            // land act can land the task again.
            Asked::Check | Asked::Challenge => {
                (self.finished, self.command, self.settled) = (false, false, true);
            }
            Asked::Land => {
                (self.finished, self.land, self.command) = (false, true, false);
                (self.land_typed, self.settled) = (true, true);
            }
            Asked::Nobody => self.command = false,
            Asked::Nothing => {}
        }
    }

    /// An attachment row: a prompt typed while the worker worked, absorbed
    /// into its running turn (`queued_command`), asked as a user row of its
    /// words would be. Someone's opens the task; it begins no turn of its own.
    fn queued(&mut self, row: &Value) {
        let Some(queued) = row
            .get("attachment")
            .filter(|a| a.get("type").and_then(Value::as_str) == Some("queued_command"))
        else {
            return;
        };
        let Some(prompt) = queued.get("prompt").filter(|p| !p.is_null()) else {
            return;
        };
        let mut message = aterm_json::Map::new();
        message.insert("content".to_owned(), prompt.clone());
        let mut asked = aterm_json::Map::new();
        asked.insert("message".to_owned(), Value::Object(message));
        for (key, value) in [
            ("origin", queued.get("origin")),
            ("timestamp", row.get("timestamp")),
        ] {
            if let Some(value) = value {
                asked.insert(key.to_owned(), value.clone());
            }
        }
        if matches!(asked_by(&Value::Object(asked), &self.ours), Asked::Theirs) {
            (self.finished, self.said, self.land, self.command) = (false, false, false, false);
            (self.done_said, self.notice, self.cut) = (false, false, false);
            (self.land_typed, self.settled) = (false, true);
        }
    }
}

/// [`FinishedScan`] over a whole transcript, its session's supervisor having
/// typed `ours`.
#[must_use]
pub fn transcript_finished(jsonl: &str, ours: &[Typed]) -> bool {
    let mut scan = FinishedScan::default();
    scan.supervisor_typed(ours);
    for line in jsonl.lines() {
        scan.line(line);
    }
    scan.finished()
}

/// THE TURN A CONVERSATION ENDS ON, by a key that names it: `answer:<uuid>`
/// for the session's model's last answer row of its main chain, or
/// `prompt:<uuid>` where a prompt row (a user row with words, no tool's
/// result, no expansion of Claude Code's) comes after it, read from the end
/// of transcript text `tail`. What a person's own exchange left to them is
/// ledgered under, and read back by a later loop and a relaunch
/// (`approvals::open_left`, the review's RR3-TE-2): the same key, the same
/// last turn. Read past a command run in Claude Code itself ([`local_row`]),
/// whose turn its answer names, as [`TurnFold`] reads it. `None`: no such
/// row, or one that names no `uuid`.
#[must_use]
pub fn last_turn_key(tail: &str) -> Option<String> {
    tail.lines()
        .rev()
        .find_map(|line| {
            if !(line.contains("\"user\"") || line.contains("\"assistant\"")) {
                return None;
            }
            let v = super::transcript::conversation(line).ok()?;
            if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
                || (v.get("type").and_then(Value::as_str) == Some("user")
                    && first_text(&v)
                        .map(typed_words)
                        .and_then(local_row)
                        .is_some())
            {
                return None;
            }
            turn_key_of(&v)
        })
        .flatten()
}

/// THE TURN A CONVERSATION ENDS ON, BY EVERY KEY THAT NAMES IT: its last
/// turn row's ([`last_turn_key`]: `answer:<uuid>`, or a prompt after it) and
/// the PROMPT THAT OPENED IT (`prompt:<uuid>`). What a person's own exchange
/// left to them is ledgered under ([`Self::key`]) and matched by
/// ([`Self::names`]; `approvals::open_left`, the review's RR3-TE-2).
///
/// BY ITS PROMPT, because the prompt's row is the exchange's that is on the
/// record when the turn ends (ATERM-KI-1016-1): Claude Code writes its rows
/// on a timer, so a loop that leaves the exchange the moment the busy row
/// leaves can read a tail that still ends on the person's prompt, or on a
/// tool's call before the reply's last words — and keyed by that row, the
/// exchange matched no later reading (each names the reply's last row), and
/// a new loop or a relaunch typed the done check, `answer@v1` or its
/// carry-on into the person's exchange. The prompt is the same row whether
/// the reply's rows are written yet or not. A row an earlier build keyed by
/// the reply's last row is still matched by that row.
///
/// A PERSON'S EXCHANGE AT A WALL, ITS RETRY TYPED, is named by the turn the
/// retry opened ([`crate::supervise::approvals::LEFT_RETRIED`]): its prompt
/// says the retry's words, and the turn ended — Claude Code's `turn_duration`
/// row closed it — within [`turn_end::EXCHANGE_WORK`] together with the
/// work the exchange did before the wall, as the loop that typed the retry
/// leaves its short answer to the person.
///
/// A COMMAND RUN IN CLAUDE CODE ITSELF SINCE, THAT NO ANSWER FOLLOWED, leaves
/// that turn the conversation's last ([`LastTurn::asked`], the review's
/// KR-P5-CMD): Claude Code answers `/config`, `/login`, `/usage` or a `!`
/// shell command itself, and [`FinishedScan`] reads such a command as a turn
/// only once an answer follows it. Read as a turn of its own, a person's
/// `/config` after their exchange was left to them named another turn, and a
/// relaunch typed its carry-on into the exchange.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LastTurn {
    /// The last turn row's key ([`last_turn_key`]).
    pub last: Option<String>,
    /// The key of the prompt that opened that turn: `None` where the tail
    /// holds none, or another turn began after it with no prompt of its own
    /// that was answered ([`TurnFold`]: a `/loop`'s wake, a peer's message,
    /// a task's notification Claude Code records as its own).
    pub opened: Option<String>,
    /// That prompt's words, hashed
    /// ([`crate::supervise::approvals::words_hash`]).
    pub opened_said: Option<u64>,
    /// How long that prompt's turn worked, by the `turn_duration` row that
    /// closed it: `None` while no such row has followed it.
    pub ended: Option<std::time::Duration>,
    /// When that prompt was recorded (its row's `timestamp`, unix ms in
    /// whole seconds): `None` where its row names no time. What A PERSON'S
    /// EXCHANGE AN OLDER BUILD NOTED LEFT, with no key, is matched by
    /// (`crate::supervise::approvals::LEFT_BEFORE`, [`Self::names`]).
    pub opened_ms: Option<u64>,
    /// The keys of the COMMANDS ASKED SINCE THAT TURN's answer that no
    /// answer has followed on the record ([`TurnFold`]), oldest first: a
    /// local command Claude Code answered itself, or a prompt command
    /// (`/init`, a custom `/review`) whose answer is still to be written —
    /// the turn a loop that leaves it the moment its busy row leaves has
    /// just seen run ([`Self::key`]).
    pub asked: Vec<String>,
}

impl LastTurn {
    /// The turn the transcript text `tail` ends on, read from its start
    /// ([`TurnFold`]).
    #[must_use]
    pub fn of(tail: &str) -> Self {
        let mut fold = TurnFold::default();
        for line in tail.lines() {
            if !(line.contains("\"user\"")
                || line.contains("\"assistant\"")
                || line.contains("\"turn_duration\""))
            {
                continue;
            }
            let Ok(v) = super::transcript::conversation(line) else {
                continue;
            };
            if v.get("isSidechain").and_then(Value::as_bool) != Some(true) {
                fold.row(&v);
            }
        }
        fold.turn()
    }

    /// The key a left exchange is ledgered under: the last command asked
    /// since the turn's answer ([`Self::asked`]) — the record may not hold
    /// its answer yet when the loop decides, and once it does that command
    /// opened the turn — else the prompt that opened the turn, else its last
    /// row's. `None` where none is named.
    #[must_use]
    pub fn key(&self) -> Option<String> {
        self.asked
            .last()
            .cloned()
            .or_else(|| self.opened.clone())
            .or_else(|| self.last.clone())
    }

    /// Whether `key` (a left exchange's row) names this turn: its last
    /// row's key, the prompt that opened it, or a command asked since that
    /// no answer followed ([`Self::asked`]) — or, for an exchange at a
    /// wall whose retry was typed ([`crate::supervise::approvals::parse_left_retried`]),
    /// the retry's turn, ended within the exchange's bound with the work
    /// before the wall ([`Self`]) — or, for an exchange an older build
    /// noted left at a time and named no turn of
    /// ([`crate::supervise::approvals::parse_left_before`]), a turn whose
    /// prompt was recorded at or before it: no turn opened since, so the
    /// conversation's last turn is the one that build left. A turn no prompt
    /// with a time opened is not that turn.
    #[must_use]
    pub fn names(&self, key: &str) -> bool {
        if let Some(at_ms) = crate::supervise::approvals::parse_left_before(key) {
            return self.opened_ms.is_some_and(|o| o <= at_ms);
        }
        if let Some((said, wall)) = crate::supervise::approvals::parse_left_retried(key) {
            let before = crate::supervise::approvals::parse_left_at_wall(wall)
                .and_then(|(_, work)| work)
                .unwrap_or_default();
            return self.opened_said == Some(said)
                && self
                    .ended
                    .is_some_and(|d| before.saturating_add(d) < turn_end::EXCHANGE_WORK);
        }
        self.last.as_deref() == Some(key)
            || self.opened.as_deref() == Some(key)
            || self.asked.iter().any(|k| k == key)
    }
}

/// The fold [`LastTurn`] is read by, one main-chain row at a time, oldest
/// first. A prompt row ([`turn_key_of`]'s `prompt:`) opens a turn. The turn
/// goes on through its model's rows and its tools' results, and through a
/// row that asks nothing under the turn's own `promptId` or under none (a
/// Stop hook's feedback, `/goal`'s check-in), as [`FinishedScan`] reads a
/// turn. A row that names no prompt key but is a turn of its own — numbered
/// anew by Claude Code (a `/loop`'s wake) or asked by someone who is no
/// person at the terminal (`origin.kind`: a peer's message, a task's
/// notification, a usage limit's continuation) — ends it once an answer
/// follows: the answer is that turn's, which no prompt opened. A row after
/// the turn's answer that nothing answers (an idle compaction's summary, the
/// files it restores) leaves it the last turn. So does A COMMAND RUN IN
/// CLAUDE CODE ITSELF ([`local_row`]), as [`FinishedScan`] reads it
/// ([`Asked::Command`]): held ([`LastTurn::asked`]) until an answer follows,
/// which opens the turn at the last command held (a prompt command, a `!`
/// command the model answers); its output and its caveat ask nothing, and
/// a turn of its own after it drops it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TurnFold {
    /// The last turn row's key: `Some(None)` for one that names no `uuid`.
    last: Option<Option<String>>,
    /// The key of the prompt that opened the turn running now.
    opened: Option<String>,
    /// That prompt's words, hashed ([`LastTurn::opened_said`]).
    said: Option<u64>,
    /// That prompt's turn's work, once a `turn_duration` row closed it.
    ended: Option<std::time::Duration>,
    /// When that prompt was recorded ([`LastTurn::opened_ms`]).
    opened_ms: Option<u64>,
    /// That prompt's `promptId`.
    prompt_id: Option<String>,
    /// An answer was read since that prompt.
    answered: bool,
    /// A turn of its own began after that answer: the next answer is its.
    aside: bool,
    /// The commands asked since, no answer yet ([`LastTurn::asked`]), oldest
    /// first, at most [`HELD_COMMANDS`].
    held: Vec<HeldCommand>,
}

/// A command a [`TurnFold`] holds: its row's key, its words hashed and its
/// `promptId`, what the turn it opens once answered is named by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HeldCommand {
    key: Option<String>,
    said: Option<u64>,
    prompt_id: Option<String>,
    at_ms: Option<u64>,
}

/// The commands a [`TurnFold`] holds at most: the newest kept.
const HELD_COMMANDS: usize = 16;

impl TurnFold {
    /// Folds main-chain row `v` in.
    fn row(&mut self, v: &Value) {
        let key = turn_key_of(v);
        let words = || first_text(v).map(typed_words);
        match v.get("type").and_then(Value::as_str) {
            Some("user")
                if key.is_some() && words().and_then(local_row) == Some(LocalRow::Command) =>
            {
                if self.held.len() >= HELD_COMMANDS {
                    self.held.remove(0);
                }
                self.held.push(HeldCommand {
                    key: key.flatten(),
                    said: words().map(crate::supervise::approvals::words_hash),
                    prompt_id: v.get("promptId").and_then(Value::as_str).map(str::to_owned),
                    at_ms: recorded_ms(v),
                });
                return;
            }
            Some("user") if key.is_some() => {
                self.held.clear();
                self.opened = key.clone().flatten();
                self.said = first_text(v)
                    .map(typed_words)
                    .map(crate::supervise::approvals::words_hash);
                self.ended = None;
                self.opened_ms = recorded_ms(v);
                self.prompt_id = v.get("promptId").and_then(Value::as_str).map(str::to_owned);
                (self.answered, self.aside) = (false, false);
            }
            Some("system")
                if v.get("subtype").and_then(Value::as_str) == Some("turn_duration")
                    && self.opened.is_some()
                    && !self.aside =>
            {
                self.ended = v
                    .get("durationMs")
                    .and_then(Value::as_u64)
                    .map(std::time::Duration::from_millis);
            }
            Some("user")
                if (self.answered || !self.held.is_empty())
                    && !only_tool_results(v)
                    && self.turn_of_its_own(v) =>
            {
                self.aside = true;
                self.held.clear();
            }
            Some("assistant") if key.is_some() => {
                if let Some(command) = self.held.pop() {
                    self.held.clear();
                    (self.opened, self.said) = (command.key, command.said);
                    (self.prompt_id, self.ended, self.aside) = (command.prompt_id, None, false);
                    self.opened_ms = command.at_ms;
                } else if std::mem::take(&mut self.aside) {
                    (self.opened, self.opened_ms) = (None, None);
                }
                self.answered = true;
            }
            _ => {}
        }
        if let Some(key) = key {
            self.last = Some(key);
        }
    }

    /// Whether user row `v`, which names no prompt key, is a turn of its
    /// own: numbered anew — against the last command held, whose own rows
    /// (a prompt command's expansion, its output) share its `promptId` —
    /// or asked by no person at the terminal.
    fn turn_of_its_own(&self, v: &Value) -> bool {
        let origin = v
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str);
        let id = v.get("promptId").and_then(Value::as_str);
        let running = self
            .held
            .last()
            .map_or(self.prompt_id.as_deref(), |c| c.prompt_id.as_deref());
        begins_unnumbered(v)
            || origin.is_some_and(|k| k != "human")
            || id.is_some_and(|id| running != Some(id))
    }

    /// The turn as folded so far.
    fn turn(&self) -> LastTurn {
        let opened = self.opened.is_some();
        LastTurn {
            last: self.last.clone().flatten(),
            opened: self.opened.clone(),
            opened_said: self.said.filter(|_| opened),
            ended: self.ended.filter(|_| opened),
            opened_ms: self.opened_ms.filter(|_| opened),
            asked: self.held.iter().filter_map(|c| c.key.clone()).collect(),
        }
    }
}

/// A row of a COMMAND RUN IN CLAUDE CODE ITSELF, by its words
/// ([`typed_words`]), as [`asked_by`] reads it: the command — a slash
/// command's `<command-name>`/`<command-message>`, a `!` shell command's
/// `<bash-input>` — a turn only if an answer follows it ([`Asked::Command`]),
/// or its output (`<local-command-…>`, `<bash-stdout>`, `<bash-stderr>`),
/// which asks nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalRow {
    Command,
    Output,
}

/// [`LocalRow`] of a user row's words: `None` for any other row.
fn local_row(words: &str) -> Option<LocalRow> {
    if ["<command-name>", "<command-message>", BASH_INPUT]
        .iter()
        .any(|tag| words.starts_with(tag))
    {
        Some(LocalRow::Command)
    } else if ["<local-command-", BASH_STDOUT, BASH_STDERR]
        .iter()
        .any(|tag| words.starts_with(tag))
    {
        Some(LocalRow::Output)
    } else {
        None
    }
}

/// The key main-chain row `v` names as a turn ([`last_turn_key`]): `None`
/// for a row that is no turn's — an API error's, a non-model's, Claude
/// Code's own expansion, a tool's result, a compaction's summary, a row of
/// no words, a local command's output ([`LocalRow::Output`]) — and
/// `Some(None)` for a turn's row that names no `uuid`. A command's own row
/// is keyed: it names the turn its answer opens ([`TurnFold`]).
fn turn_key_of(v: &Value) -> Option<Option<String>> {
    let uuid = || v.get("uuid").and_then(Value::as_str).map(str::to_owned);
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let model = v
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str);
            if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
                || model.is_some_and(|m| !is_model_id(m))
            {
                return None;
            }
            Some(uuid().map(|u| format!("answer:{u}")))
        }
        Some("user") => {
            if v.get("isMeta").and_then(Value::as_bool) == Some(true)
                || v.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
                || only_tool_results(v)
                || first_text(v).is_none_or(|t| t.trim().is_empty())
                || first_text(v).map(typed_words).and_then(local_row) == Some(LocalRow::Output)
            {
                return None;
            }
            Some(uuid().map(|u| format!("prompt:{u}")))
        }
        _ => None,
    }
}

/// What one main-chain user row asks ([`FinishedScan`]): by its origin's
/// kind first — Claude Code's own wakes ask nothing, a peer's message and any
/// kind not known here (whatever its `isMeta`) are someone's — then, for a
/// person's terminal input (`human`) and a row that names no origin, by its
/// words as [`prompt_of`] reads them, save a shell command's (`!`): a turn
/// only if one follows it, its output none.
fn asked_by(row: &Value, ours: &[Typed]) -> Asked {
    if origin_is_someone(row) {
        return Asked::Theirs;
    }
    if matches!(
        row.get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str),
        Some("task-notification" | "auto-continuation")
    ) {
        return Asked::Nobody;
    }
    let words = first_text(row).map(typed_words);
    if words.is_some_and(|t| t.starts_with(TASK_NOTIFICATION)) {
        return Asked::Nobody;
    }
    if words.is_some_and(|t| t.starts_with(BASH_INPUT)) {
        return Asked::Command;
    }
    if words.is_some_and(|t| t.starts_with(BASH_STDOUT) || t.starts_with(BASH_STDERR)) {
        return Asked::Nobody;
    }
    if words.is_some_and(is_done_check) {
        return Asked::Check;
    }
    if words.is_some_and(is_land_check) {
        return Asked::Land;
    }
    if words.is_some_and(turn_end::is_challenge_text) {
        return Asked::Challenge;
    }
    match prompt_of(row, ours) {
        Prompt::Theirs => Asked::Theirs,
        Prompt::Command => Asked::Command,
        Prompt::Harness | Prompt::LocalOutput => Asked::Nobody,
        Prompt::None => Asked::Nothing,
    }
}

/// Whether `text` is the supervisor's done check as typed: its words, with
/// whatever rides after them (`turn_end::done_check_text`: the open-work and
/// repository suffixes, the reversible clause,
/// [`turn_end::DONE_CHECK_REVERSIBLE`], and the standing rules), or an
/// earlier build's check, which records written then still hold
/// ([`turn_end::RETIRED_DONE_CHECKS`]). So too a restart's carry-on that asks
/// the check ([`checking`], `continued:check`): the harness's mark, the
/// restart's facts, then the check's words — its answer read as the check's
/// yield. BY ITS WORDS ALONE, whoever typed them (ATERM-KI-1016-1, done-2):
/// the ledger has no row of a real check in a hand `/resume` tab, or past
/// [`TYPED_WITHIN_MS`] or the ledger's cut, and a person who types the check
/// word for word asks what it asks.
fn is_done_check(text: &str) -> bool {
    let checks = || std::iter::once(turn_end::DONE_CHECK).chain(turn_end::RETIRED_DONE_CHECKS);
    checks().any(|check| opens_with(check, text))
        || (text.starts_with(HARNESS_MARK) && {
            let words: Vec<&str> = text.split_whitespace().collect();
            checks().any(|check| {
                let check: Vec<&str> = check.split_whitespace().collect();
                words.windows(check.len()).any(|w| w == check.as_slice())
            })
        })
}

/// Whether `text` is Land's instruction or nudge as the policy typed it
/// ([`Asked::Land`]: `turn_end::land_text`, `turn_end::land_nudge_text`, the
/// standing rules after it or not), or as builds before typed it
/// ([`turn_end::RETIRED_LANDS`], [`turn_end::RETIRED_LAND_NUDGES`]). By its
/// opening words alone: a person's prompt that quotes them is theirs.
fn is_land_check(text: &str) -> bool {
    [turn_end::LAND, turn_end::LAND_NUDGE]
        .into_iter()
        .chain(turn_end::RETIRED_LANDS)
        .chain(turn_end::RETIRED_LAND_NUDGES)
        .any(|head| opens_with(head, text))
}

/// Whether `text` opens a LAND ACT, by its opening words as typed — Land's
/// instruction or a nudge ([`is_land_check`]) or Land's answer to a question
/// (`turn_end::LAND_ANSWER`), today's words or a build before's: the turns
/// a worker's result reads as a landing (`super::result`). By the words
/// alone, as the result reads the transcript with no ledger: a landing's
/// reply is a defense whoever typed the act, and a person who types Land's
/// words asks what they ask.
pub(crate) fn is_land_act(text: &str) -> bool {
    is_land_check(text)
        || std::iter::once(turn_end::LAND_ANSWER)
            .chain(turn_end::RETIRED_LAND_ANSWERS)
            .any(|answer| opens_with(answer, text))
}

/// Whether user row `row` is CLAUDE CODE'S OWN CONTINUATION of a usage limit
/// at its reset (`origin.kind` `auto-continuation`): after a limit's row in
/// the done check's turn or a land act's ([`FinishedScan::cut`]) it delivers
/// the cut act again, as the wall's retry does, and as the turn-end policy
/// reads the turn under the vendor's `continuing automatically`
/// (`TurnEndState::observe`; ATERM-KI-1016-1: read as a turn nobody asked,
/// the reply to it was a first done report, and the record asked the check
/// once more).
fn vendor_continues(row: &Value) -> bool {
    row.get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str)
        == Some("auto-continuation")
}

/// Whether user row `row` is LAND'S ANSWER TO A QUESTION the session's
/// supervisor typed (`turn_end::LAND_ANSWER`, or a build before's,
/// [`turn_end::RETIRED_LAND_ANSWERS`]): its words, by the ledger row
/// of its rule (`land-answer@v1`), never the words alone — a person may
/// type them. Land's check to the record only once the instruction was read
/// ([`FinishedScan::land_typed`]).
fn lands_answer(row: &Value, ours: &[Typed]) -> bool {
    row.get("isMeta").and_then(Value::as_bool) != Some(true)
        && first_text(row).map(typed_words).is_some_and(|t| {
            std::iter::once(turn_end::LAND_ANSWER)
                .chain(turn_end::RETIRED_LAND_ANSWERS)
                .any(|answer| opens_with(answer, t))
                && typed_by(ours, t, recorded_ms(row), |o| {
                    o.rule == turn_end::RULE_LAND_ANSWER
                })
        })
}

/// Whether user row `row` is a WALL'S RETRY the session's supervisor typed:
/// a text (no command's row) its ledger `ours` records under a wall's retry
/// rule (`turn_end::is_wall_retry`) — the retry
/// ([`turn_end::wall_retry_text`]) or the usage resume
/// ([`turn_end::continuation_text`], which is the harness-marked Cruise
/// continuation, [`HARNESS_MARK`]) — the acts that deliver again the land act
/// a wall cut ([`FinishedScan::cut`]), as the turn-end policy reads them
/// (`TurnEndState::acted`). By the ledger's rule, never the words alone:
/// the usage resume's words are the continuation's too, and the same words
/// the continue policy typed after a wall (`continue@v1`) deliver no land
/// act. Not `/compact` or `/login` (a command's row), nor a restart's
/// marked carry-on, which no wall's retry rule records. Where a full context
/// cut the act, the turn-end policy types the act itself as the
/// `/compact`'s continuation (`TurnEndState::compact_owes`), which
/// [`is_land_check`] reads by its words. A ledger row that names no rule is
/// none either.
fn supervisors_plain(row: &Value, ours: &[Typed]) -> bool {
    row.get("isMeta").and_then(Value::as_bool) != Some(true)
        && first_text(row).map(typed_words).is_some_and(|t| {
            !t.starts_with('<')
                && typed_by(ours, t, recorded_ms(row), |o| {
                    turn_end::is_wall_retry(&o.rule)
                })
        })
}

/// Whether `text` opens with `check`'s words, however spaced or wrapped.
fn opens_with(check: &str, text: &str) -> bool {
    let mut words = text.split_whitespace();
    check.split_whitespace().all(|w| words.next() == Some(w))
}

/// A user row's first text, as [`prompt_of`] reads it.
/// The fixed opening words of Claude Code's notice of a one-shot scheduled
/// task missed while it was not running (`Pcr` in the 2.1.252 bundle, the
/// same in 2.1.302: `The following one-shot scheduled task was missed …` or
/// `… tasks were missed …`).
pub(crate) const MISSED_TASK_NOTICE: &str = "The following one-shot scheduled task";

/// Whether a user row with NO `promptId` is a turn Claude Code begins of its
/// own ([`FinishedScan`]'s `asked`), never a row of the running turn: one
/// whose `turnOrigin` names it (Claude Code's own rule, 2.1.302: `promptId ||
/// turnOrigin || !isMeta`; `scheduled`, `auto_continuation`, `peer`, while a
/// Stop hook's feedback and `/goal`'s check-in carry none), a `/loop`'s wake
/// by the scheduled task it fired for (`scheduledTaskId`, the scheduler's
/// `onFireTask`), and the notice of a missed one-shot task, which the
/// scheduler's `onFire` gives neither at a start (the review of
/// ATERM-KI-1016-1, KR-DONE-4: `if(l)l(e);else t(Pcr(e))`, the REPL passing
/// no `onMissed`), by its fixed opening words ([`MISSED_TASK_NOTICE`]).
fn begins_unnumbered(row: &Value) -> bool {
    begins_unnumbered_by(
        |key| row.get(key).is_some_and(|v| !v.is_null()),
        row.get("isMeta").and_then(Value::as_bool) == Some(true),
        first_text(row),
    )
}

/// [`begins_unnumbered`] of a row read in parts, as the step fold reads one
/// (`steps::StepFold`, which keeps no text: `lead` is at most the first
/// decoded bytes of its first text): `named(key)` says whether the row
/// names a non-null `key` ([`TURN_ORIGIN`], [`SCHEDULED_TASK_ID`]), `meta`
/// whether it is `isMeta`.
pub(crate) fn begins_unnumbered_by(
    named: impl Fn(&str) -> bool,
    meta: bool,
    lead: Option<&str>,
) -> bool {
    named(TURN_ORIGIN)
        || named(SCHEDULED_TASK_ID)
        || (meta && lead.is_some_and(|t| t.trim_start().starts_with(MISSED_TASK_NOTICE)))
}

/// The key Claude Code names a turn's origin by (`scheduled`,
/// `auto_continuation`, `peer`, `human`, `task_notification`).
pub(crate) const TURN_ORIGIN: &str = "turnOrigin";
/// The key of the scheduled task a `/loop`'s wake fired for.
pub(crate) const SCHEDULED_TASK_ID: &str = "scheduledTaskId";

/// Whether a user row whose `promptId` is `id` begins a turn Claude Code
/// numbered of its own while the turn running is `running` (the last
/// `promptId` read; [`FinishedScan`]'s rule, which the step fold shares):
/// a `promptId` other than the running one, or none on a row that
/// `unnumbered` says begins one ([`begins_unnumbered`]). A row under the
/// running turn's `promptId` (a Stop hook's feedback, `/goal`'s check-in)
/// goes on that turn. `FinishedScan` reads from the file's first byte; the
/// step fold, which can bind mid-turn, passes no `id` while its running
/// turn is unread (`steps.rs`'s module docs).
pub(crate) fn numbered_anew<T: PartialEq>(
    id: Option<T>,
    running: Option<T>,
    unnumbered: impl FnOnce() -> bool,
) -> bool {
    match id {
        Some(id) => running != Some(id),
        None => unnumbered(),
    }
}

fn first_text(row: &Value) -> Option<&str> {
    match row.get("message")?.get("content")? {
        Value::String(t) => Some(t.as_str()),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .find_map(|p| p.get("text").and_then(Value::as_str)),
        _ => None,
    }
}

/// Whether a user row is only a tool's result (the agent's own loop, its
/// turn going on): every part of its content is one.
fn only_tool_results(row: &Value) -> bool {
    row.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            !parts.is_empty()
                && parts
                    .iter()
                    .all(|p| p.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

/// An assistant `message`'s words: its text parts that say anything, in
/// order; `None` for a row of a tool's call or a thought alone.
fn answer_words(message: &Value) -> Option<String> {
    let texts: Vec<&str> = match message.get("content")? {
        Value::String(t) => vec![t.as_str()],
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect(),
        _ => return None,
    };
    let texts: Vec<&str> = texts.into_iter().filter(|t| !t.trim().is_empty()).collect();
    (!texts.is_empty()).then(|| texts.join("\n\n"))
}

/// One transcript row as the upgrade reads it ([`turn_of`]).
enum Turn {
    /// A main-chain user row that IS the live upgrade's announcement
    /// ([`is_announcement`]).
    Notice(Value),
    /// A main-chain user row that directs the conversation ([`is_direction`]).
    Direction,
    /// A main-chain assistant row the agent wrote itself — never a
    /// subagent's (`isSidechain`), never one of the rows Claude Code writes
    /// on its own (`<synthetic>`, [`is_model_id`]) — and its `message`.
    Agent(Value),
}

/// What one transcript `line` is to the upgrade ([`Turn`]); `None` for any
/// other row (a subagent's, a user row that directs nothing, a `<synthetic>`
/// assistant row) and for non-JSON lines.
fn turn_of(line: &str) -> Option<Turn> {
    if !line.contains("\"user\"") && !line.contains("assistant") {
        return None;
    }
    let mut v = super::transcript::conversation(line).ok()?;
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let message = v.as_object_mut()?.remove("message").unwrap_or_default();
            let theirs = message
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|m| !is_model_id(m));
            (!theirs).then_some(Turn::Agent(message))
        }
        Some("user") => {
            let message = v.get("message")?;
            if is_announcement(message) {
                return Some(Turn::Notice(message.clone()));
            }
            if v.get("isMeta").and_then(Value::as_bool) == Some(true)
                || v.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            {
                return None;
            }
            let directs = match message.get("content") {
                Some(Value::String(t)) => is_direction(t),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .any(is_direction),
                _ => false,
            };
            directs.then_some(Turn::Direction)
        }
        _ => None,
    }
}

/// A notice's input reference must match the round before an ordinary
/// answer can consent to a restart. A substring elsewhere in a conversation
/// is never enough: callers first identify the announcement itself.
fn notice_matches(message: &Value, marker: &str) -> bool {
    !marker.is_empty() && answer_words(message).is_some_and(|words| words.contains(marker))
}

/// Legacy marker answers identify their own request. An ordinary readiness
/// answer instead needs the matching notice, the session's model, and its
/// final text row. Text parts are read together so a later caveat cannot be
/// hidden in a second part.
fn answers(message: &Value, marker: &str, matching_notice: bool) -> bool {
    let Some(words) = answer_words(message) else {
        return false;
    };
    if !marker.is_empty() && words.lines().any(|line| is_ready_line(line, marker)) {
        return true;
    }
    matching_notice
        && message
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(is_model_id)
        && message.get("stop_reason").and_then(Value::as_str) == Some("end_turn")
        && readiness::says_ready(&words)
}

/// Whether the agent's READY was superseded ([`Facts::ready_superseded`]):
/// after the latest notice ([`ANNOUNCE_HEAD`]) it answered one of `markers`
/// ([`answers`]), then a direction ([`is_direction`]) came with no READY
/// since. Exactly the answers [`transcript_has_ready`] stops counting for a
/// direction, as distinct from a notice not yet answered. `jsonl` is the
/// transcript tail.
#[must_use]
pub fn ready_superseded(jsonl: &str, markers: &[String]) -> bool {
    let (mut answered, mut superseded) = (false, false);
    let mut matching_notice = false;
    for line in jsonl.lines() {
        match turn_of(line) {
            Some(Turn::Notice(message)) => {
                (answered, superseded) = (false, false);
                matching_notice = markers.iter().any(|m| notice_matches(&message, m));
            }
            Some(Turn::Direction) => {
                superseded |= answered;
                matching_notice = false;
            }
            Some(Turn::Agent(message))
                if markers
                    .iter()
                    .any(|m| answers(&message, m, matching_notice)) =>
            {
                (answered, superseded) = (true, false);
            }
            Some(Turn::Agent(_)) | None => {}
        }
    }
    superseded
}

/// A user row's words as typed, seen through Claude Code's paste wrapper: a
/// long paste is recorded as `\n\n<pasted_content id="…">\n<text>\n
/// </pasted_content id="…">\n` (the upgrade's own notice, typed as one paste
/// by the control socket's `turn`, included). Every reader of how a row's
/// words begin ([`is_announcement`], [`TaskScan`], [`is_direction`], the
/// survey's prompts in [`super::excerpt`]) reads the pasted text of a wrapped
/// row, tags dropped; any other row as is, leading blanks trimmed.
pub(crate) fn typed_words(text: &str) -> &str {
    let t = text.trim_start();
    let Some((tag, body)) = t
        .strip_prefix("<pasted_content")
        .and_then(|rest| rest.split_once('>'))
    else {
        return t;
    };
    // The open tag is one line (` id="…"`): anything else is not the wrapper.
    if tag.contains('\n') {
        return t;
    }
    let body = body.split_once("</pasted_content").map_or(body, |(b, _)| b);
    body.trim()
}

/// Whether one user row's words are a direction ([`directed_since_ready`]):
/// anything typed except the harness's own lines and Claude Code's tagged
/// rows, of which only a person's shell command (`<bash-input>`) counts. A
/// paste is read as what was pasted ([`typed_words`]).
fn is_direction(text: &str) -> bool {
    let t = typed_words(text);
    if t.is_empty() || t.starts_with("[aterm harness]") {
        return false;
    }
    !t.starts_with('<') || t.starts_with(BASH_INPUT)
}

/// Whether a transcript user `message` is the live upgrade's announcement:
/// its text — the whole content, or a text part of it — begins with
/// [`ANNOUNCE_HEAD`], typed or pasted ([`typed_words`]).
fn is_announcement(message: &Value) -> bool {
    let head = |t: &str| typed_words(t).starts_with(ANNOUNCE_HEAD);
    match message.get("content") {
        Some(Value::String(t)) => head(t),
        Some(Value::Array(parts)) => parts.iter().any(|part| {
            part.get("type").and_then(Value::as_str) == Some("text")
                && part.get("text").and_then(Value::as_str).is_some_and(head)
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------- the login wall

/// The login wall's signature in a transcript row: a main-chain `<synthetic>`
/// assistant row flagged `isApiErrorMessage` whose `error` is
/// `authentication_failed` (text `Login expired · Please run /login`, drawn
/// `⏺ Login expired · …` on screen, [`login_wall`]).
fn is_auth_wall_row(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("assistant")
        && v.get("isSidechain").and_then(Value::as_bool) != Some(true)
        && v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        && v.get("error").and_then(Value::as_str) == Some("authentication_failed")
}

/// What one transcript row says of the login ([`login_row`], [`LoginScan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoginRow {
    /// The wall ([`is_auth_wall_row`]).
    Wall,
    /// What lifts it: the vendor's own `Login successful` (a person's
    /// `/login` finished: `<local-command-stdout>Login successful…`), or a
    /// row of the session's own model — a turn the API answered.
    Lifted,
}

/// Whether `text` is the output of a `/login` that finished, as Claude Code
/// writes it to the transcript: `<local-command-stdout>Login successful…`.
fn login_succeeded(text: &str) -> bool {
    text.trim_start()
        .starts_with("<local-command-stdout>Login successful")
}

/// What one transcript row says of the login, with its time (unix seconds,
/// `None` when it names none); `None` for a row that says nothing of it, a
/// subagent's and one that is no JSON ([`LoginScan`]).
fn login_row(line: &str) -> Option<(LoginRow, Option<u64>)> {
    if !line.contains("authentication_failed")
        && !line.contains("Login successful")
        && !line.contains("\"assistant\"")
    {
        return None;
    }
    let v = super::transcript::conversation(line).ok()?;
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let at = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(super::upgrade_models::parse_utc);
    if is_auth_wall_row(&v) {
        return Some((LoginRow::Wall, at));
    }
    let lifted = match v.get("type").and_then(Value::as_str) {
        Some("assistant") => v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .is_some_and(is_model_id),
        _ => is_login_success_row(&v),
    };
    lifted.then_some((LoginRow::Lifted, at))
}

/// THE LOGIN WALL'S ONE SCAN, a transcript row at a time: what
/// [`transcript_login_wall`] and [`login_lifted_at`] read of the rows folded
/// in so far, each row's say ([`LoginRow`]) in order. A reader that reads a
/// transcript row by row reads the login through it (`aterm-lab reduce`'s
/// readings, session log ruling U7: the closed words a reduction keeps).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoginScan {
    /// The last row that said something of the login.
    last: Option<LoginRow>,
    /// A wall stands that nothing lifted yet.
    walled: bool,
    /// When the last wall was lifted.
    lifted: Option<u64>,
}

impl LoginScan {
    /// The scan of every line of `jsonl`.
    #[must_use]
    pub fn of(jsonl: &str) -> Self {
        let mut scan = Self::default();
        for line in jsonl.lines() {
            scan.line(line);
        }
        scan
    }

    /// Folds one transcript line in. Non-JSON lines are skipped.
    pub fn line(&mut self, line: &str) {
        let Some((row, at)) = login_row(line) else {
            return;
        };
        self.last = Some(row);
        match row {
            LoginRow::Wall => (self.walled, self.lifted) = (true, None),
            LoginRow::Lifted if self.walled => (self.walled, self.lifted) = (false, at),
            LoginRow::Lifted => {}
        }
    }

    /// [`transcript_login_wall`] of the rows folded in.
    #[must_use]
    pub fn wall(&self) -> bool {
        self.last == Some(LoginRow::Wall)
    }

    /// [`login_lifted_at`] of the rows folded in.
    #[must_use]
    pub fn lifted_at(&self) -> Option<u64> {
        self.lifted
    }
}

/// Whether one transcript row is a person's finished `/login`
/// ([`login_succeeded`]). Two shapes: a `user` row's `message.content`, or a
/// `system` row with `subtype` `local_command` and a top-level `content`,
/// depending on the Claude Code version.
fn is_login_success_row(v: &Value) -> bool {
    match v.get("type").and_then(Value::as_str) {
        Some("user") => v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .is_some_and(login_succeeded),
        Some("system") => {
            v.get("subtype").and_then(Value::as_str) == Some("local_command")
                && v.get("content")
                    .and_then(Value::as_str)
                    .is_some_and(login_succeeded)
        }
        _ => false,
    }
}

/// Whether the session stands at the login wall by its transcript tail: the
/// last login-related row is the wall ([`is_auth_wall_row`]), with no
/// `Login successful` or answer of the session's model after it. Read beside
/// the screen ([`login_wall`]), whose wall row leaves when a `/login` dialog is
/// dismissed. A login finished in another tab writes nothing here, so this
/// reads the wall until something typed into this session is answered.
#[must_use]
pub fn transcript_login_wall(jsonl: &str) -> bool {
    LoginScan::of(jsonl).wall()
}

/// When the last login wall in `jsonl` was lifted (unix seconds): the time of
/// the first row after it that lifts it ([`LoginRow::Lifted`]). `None` with no
/// wall, a standing wall, or an untimed lifting row. Holds the upgrade's
/// clocks through the wall ([`clock_held_until`]): a notice read before the
/// login went gets its whole window once the agent can answer again.
#[must_use]
pub fn login_lifted_at(jsonl: &str) -> Option<u64> {
    LoginScan::of(jsonl).lifted_at()
}

/// Whether the latest notice carrying `marker` never reached the model: the
/// first main-chain assistant row in `jsonl` after the last announcement
/// ([`ANNOUNCE_HEAD`]) carrying `marker` is the login wall
/// ([`is_auth_wall_row`]). `Some(false)` when the model answered it or nothing
/// has yet; `None` when the tail holds no such announcement. Claude Code's
/// other own rows (`No response requested.`) are passed over.
///
/// A notice the model read later as history (in someone else's turn, or when
/// Claude Code re-queries after a `/login`) still counts as never reached as
/// asked; unless that later answer is READY ([`transcript_has_ready`]), the
/// notice is typed once more as the same ask, spending no ask.
#[must_use]
pub fn notice_fate(jsonl: &str, marker: &str) -> Option<bool> {
    notice_fate_of(jsonl, marker).map(|fate| fate == NoticeFate::Wall)
}

/// What became of ONE NOTICE, by the transcript ([`notice_fate_of`]): whether
/// its own turn reached the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeFate {
    /// Nothing has answered its own turn yet (the turn is in flight).
    Open,
    /// The session's own model answered its own turn: it was read as asked.
    Answered,
    /// The login wall answered its turn ([`is_auth_wall_row`]): it never
    /// reached the model ([`notice_fate`]).
    Wall,
    /// A usage limit answered its turn ([`is_limit_row`]) and the model has
    /// written nothing since: the notice waits unread and reaches the model
    /// with the next turn past the limit (the reset's `Continuing
    /// automatically at …`, or anyone's message). Not an ask the agent could
    /// answer, and a second notice would only queue behind it.
    Queued,
    /// A usage limit answered it and the model took it later at `.0` (unix
    /// seconds; `None` if untimed): the first row of the session's model after
    /// it. Its window opens then (the driver holds the clock until that row).
    Taken(Option<u64>),
}

/// The fate of the latest notice carrying `marker` in `jsonl`: the first
/// decisive main-chain assistant row after it (the login wall's, a usage
/// limit's, after which the model's first row means taken, or the model's
/// own). Claude Code's other own rows and subagents' are passed over. `None`
/// when the tail holds no such notice.
#[must_use]
pub fn notice_fate_of(jsonl: &str, marker: &str) -> Option<NoticeFate> {
    notice_scan(jsonl, marker).map(|scan| scan.fate)
}

/// Until when a notice queued behind a usage limit waits on it: while the
/// latest notice carrying `marker` is [`NoticeFate::Queued`] in `jsonl`, the
/// reset the limit row that last answered it names
/// (`quotaLimits.resetsAt`, unix seconds), or, naming none (an API rate
/// limit, a model bucket), [`REASK_S`] after the row was written. `Some(0)`:
/// a `/login` finished after that row, or the row names neither; the screen
/// alone says whether a limit stands. `None`: not queued.
///
/// Past it, a notice still queued never reached the model and is typed again
/// as the same ask ([`Facts::undelivered`]) where the screen shows no limit:
/// at once while at most [`REQUEUE_MAX`] upgrade notices wait untaken
/// ([`queued_copies`]), otherwise after a rest ([`Scan::rests_until`]).
#[must_use]
pub fn queued_until(jsonl: &str, marker: &str) -> Option<u64> {
    notice_scan(jsonl, marker)?.queued_until()
}

/// How many upgrade notices the conversation holds untaken while the latest
/// notice carrying `marker` is [`NoticeFate::Queued`]: every notice typed
/// since the session's model last wrote a row (any marker, round or target,
/// whatever answered it), all of which reach the agent together once a turn
/// passes the limit. `0`: not queued. Bounds re-typing ([`REQUEUE_MAX`]); only
/// a row of the session's own model resets the count (a `/login` does not).
#[must_use]
pub fn queued_copies(jsonl: &str, marker: &str) -> u32 {
    notice_scan(jsonl, marker).map_or(0, |scan| scan.queued_copies())
}

/// What [`notice_scan`] found of the latest notice carrying a marker, and of
/// the conversation it waits in — one read of the tail for all of it
/// ([`notice_fate_of`], [`queued_until`], [`queued_copies`],
/// [`Scan::rests_until`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scan {
    /// Its fate ([`notice_fate_of`]).
    pub fate: NoticeFate,
    /// While queued, until when the limit that answered it holds by the
    /// transcript ([`queued_until`]; `None`: the row named nothing, or a
    /// `/login` finished since).
    pub until: Option<u64>,
    /// Upgrade notices the session's model has not taken, typed since its
    /// last own row, this one included ([`queued_copies`]).
    pub untaken: u32,
    /// When the limit row that last answered it was written (unix seconds;
    /// `None`: not queued, or the row names no time).
    pub answered_at: Option<u64>,
    /// When it was typed (unix seconds; `None`: its row names no time).
    pub typed_at: Option<u64>,
}

impl Scan {
    /// [`queued_until`] of this scan.
    #[must_use]
    pub fn queued_until(&self) -> Option<u64> {
        (self.fate == NoticeFate::Queued).then(|| self.until.unwrap_or(0))
    }

    /// [`queued_copies`] of this scan.
    #[must_use]
    pub fn queued_copies(&self) -> u32 {
        if self.fate == NoticeFate::Queued {
            self.untaken
        } else {
            0
        }
    }

    /// Until when a full queue rests: once more than [`REQUEUE_MAX`] notices
    /// wait untaken ([`queued_copies`]) and the latest one's limit is over by
    /// the transcript, that notice is typed once more only [`queue_rest`]
    /// after the limit's word ran out (its named reset, or [`REASK_S`] after a
    /// row naming none or ended by `/login`). The rest grows with each unread
    /// copy, so a limit lasting days adds a handful of copies and then one a
    /// day. `None`: not queued, or the limit row names no time; the queue then
    /// waits for the session to go on or the owner's word.
    #[must_use]
    pub fn rests_until(&self) -> Option<u64> {
        if self.fate != NoticeFate::Queued {
            return None;
        }
        let at = self.answered_at?;
        Some(
            self.until
                .unwrap_or(0)
                .max(at.saturating_add(REASK_S))
                .saturating_add(queue_rest(self.untaken)),
        )
    }
}

/// The longest a full queue rests before one more copy: a day. What bounds
/// the copies a usage limit that lasts for days adds, once the rest has
/// grown to it ([`queue_rest`]): one a day.
pub const QUEUE_REST_MAX_S: u64 = 24 * 3_600;

/// How many times a full queue's rest doubles before reaching
/// [`QUEUE_REST_MAX_S`] ([`queue_rest`]): two, four, eight and sixteen hours.
/// The derived model's `Daily`
/// (`aterm_models_harness::harness_upgrade_limit_queue_model`).
pub const QUEUE_REST_DOUBLINGS: u32 = {
    let mut k = 0;
    while RETRY_S << k < QUEUE_REST_MAX_S {
        k += 1;
    }
    k
};

/// How long a full queue of `untaken` notices rests before the upgrade types
/// one more copy ([`Scan::rests_until`]). Beyond the `1 + REQUEUE_MAX` a full
/// queue holds, the rest doubles with each unread copy: [`RETRY_S`], then
/// twice, four and eight times that, then [`QUEUE_REST_MAX_S`]. Each copy the
/// limit answers again says it still stands, so each extra is worth less; a
/// row of the model's own resets the count ([`queued_copies`]). The owner's
/// `Upgrade now` still types one at once.
#[must_use]
pub fn queue_rest(untaken: u32) -> u64 {
    let over = untaken.saturating_sub(REQUEUE_MAX + 1);
    if over >= QUEUE_REST_DOUBLINGS {
        QUEUE_REST_MAX_S
    } else {
        (RETRY_S << over).min(QUEUE_REST_MAX_S)
    }
}

/// [`notice_fate_of`] plus, while queued, until when the limit holds, when
/// its row was written, how many notices wait untaken, and when the notice
/// was typed: one read of `jsonl` for the driver (`queue_facts`).
#[must_use]
pub fn notice_scan(jsonl: &str, marker: &str) -> Option<Scan> {
    let (mut armed, mut fate, mut until) = (false, None, None);
    let (mut answered_at, mut typed_at, mut untaken) = (None, None, 0_u32);
    for line in jsonl.lines() {
        if !line.contains("\"user\"")
            && !line.contains("assistant")
            && !line.contains("Login successful")
        {
            continue;
        }
        let Ok(v) = super::transcript::conversation(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if armed && fate == Some(NoticeFate::Queued) && is_login_success_row(&v) {
            until = None;
            continue;
        }
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                if v.get("message").is_some_and(is_announcement) {
                    // One more notice in the conversation the model has not
                    // taken, whatever it carries.
                    untaken = untaken.saturating_add(1);
                    if line.contains(marker) {
                        (armed, fate, until, answered_at) =
                            (true, Some(NoticeFate::Open), None, None);
                        typed_at = row_time(&v);
                    }
                }
            }
            Some("assistant") => {
                let own = v
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .is_some_and(is_model_id);
                if armed {
                    if is_auth_wall_row(&v) {
                        (armed, fate, until) = (false, Some(NoticeFate::Wall), None);
                    } else if is_limit_row(&v) {
                        fate = Some(NoticeFate::Queued);
                        until = limit_holds_until(&v);
                        answered_at = row_time(&v);
                    } else if own {
                        armed = false;
                        until = None;
                        fate = Some(if fate == Some(NoticeFate::Queued) {
                            NoticeFate::Taken(row_time(&v))
                        } else {
                            NoticeFate::Answered
                        });
                    }
                }
                // The model's own row: every notice before it was taken.
                if own {
                    untaken = 0;
                }
            }
            _ => {}
        }
    }
    fate.map(|fate| Scan {
        fate,
        until,
        untaken,
        answered_at,
        typed_at,
    })
}

/// A transcript row's `timestamp`, in unix seconds.
fn row_time(v: &Value) -> Option<u64> {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(super::upgrade_models::parse_utc)
}

/// Until when one limit row ([`is_limit_row`]) holds the session: the reset
/// it names (`quotaLimits.resetsAt`), else [`REASK_S`] after it was written,
/// else `None`.
fn limit_holds_until(v: &Value) -> Option<u64> {
    v.get("quotaLimits")
        .and_then(|q| q.get("resetsAt"))
        .and_then(Value::as_u64)
        .or_else(|| row_time(v).map(|at| at.saturating_add(REASK_S)))
}

/// The usage limit's signature in a transcript row: a main-chain
/// `<synthetic>` assistant row flagged `isApiErrorMessage` whose `error` is
/// `rate_limit` (text like `You've hit your weekly limit · resets Oct 3 at
/// 10am (America/Los_Angeles)`), or whose text the supervisor's wall table
/// reads as a limit ([`aterm_phase::classify_wall`],
/// [`aterm_phase::WallKind::reads_limited`]). No second pattern of the
/// upgrade's own.
fn is_limit_row(v: &Value) -> bool {
    if v.get("type").and_then(Value::as_str) != Some("assistant")
        || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || v.get("isApiErrorMessage").and_then(Value::as_bool) != Some(true)
    {
        return false;
    }
    if v.get("error").and_then(Value::as_str) == Some("rate_limit") {
        return true;
    }
    row_texts(v)
        .any(|text| aterm_phase::classify_wall(text).is_some_and(|kind| kind.reads_limited()))
}

/// Every text part of one transcript row's `message.content`, in order.
fn row_texts(v: &Value) -> impl Iterator<Item = &str> {
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
}

/// Whether one limit row ([`is_limit_row`]) is a SPEND OR CREDIT wall: a
/// text the wall table reads as money ([`aterm_phase::WallKind::Spend`]).
fn is_spend_row(v: &Value) -> bool {
    row_texts(v).any(|text| aterm_phase::classify_wall(text) == Some(aterm_phase::WallKind::Spend))
}

/// Until when the session stands at a usage limit by its transcript tail: if
/// the last main-chain row saying whether the model can answer (a row of the
/// session's model, or a limit row, [`is_limit_row`]) is a limit row, the
/// reset it names (`quotaLimits.resetsAt`, unix seconds) or, naming none (an
/// API rate limit), [`REASK_S`] after it was written (as [`queued_until`]).
/// `None`: the model answered last, no limit row, or the row names no time.
/// Keeps even the first notice out of a session whose limit banner has left
/// the screen ([`Facts::limited`]).
///
/// A `/login` that finished after the limit row ([`is_login_success_row`],
/// e.g. an account switch) ends it too; the screen then judges, as
/// [`login_lifted_at`] leaves it for the login wall.
#[must_use]
pub fn transcript_limit_until(jsonl: &str) -> Option<u64> {
    StandingScan::of(jsonl).until()
}

/// The reset the session's standing usage limit names, by its transcript
/// tail: [`transcript_limit_until`]'s limit row read for the instant it names
/// alone (`quotaLimits.resetsAt`, past or future), never the [`REASK_S`] wait
/// of a row naming none. `None` where that is `None` and for a row naming no
/// reset. Says when a notice left on screen is past its reset ([`LIMIT_GRACE_S`]
/// past it the tab counts as idle); a reset is never guessed.
///
/// [`LIMIT_GRACE_S`]: super::upgrade_drive::LIMIT_GRACE_S
#[must_use]
pub fn transcript_limit_reset(jsonl: &str) -> Option<u64> {
    StandingScan::of(jsonl).reset()
}

/// The reset one limit row names alone ([`transcript_limit_reset`]).
fn reset_named(v: &Value) -> Option<u64> {
    v.get("quotaLimits")
        .and_then(|q| q.get("resetsAt"))
        .and_then(Value::as_u64)
        .filter(|&at| at > 0)
}

/// Whether the session's standing usage limit is a spend or credit wall, by
/// its transcript tail: [`transcript_limit_until`]'s limit row read as money
/// ([`aterm_phase::WallKind::Spend`]: a spend limit, shared budget, no
/// credits). Not a usage window: naming no reset, only the account holder
/// ends it, and the owner's view says so.
#[must_use]
pub fn transcript_limit_spend(jsonl: &str) -> bool {
    StandingScan::of(jsonl).spend()
}

/// The longest limit type ([`StandingWall::window`]) that is kept, in bytes.
pub const LIMIT_TYPE_MAX: usize = 64;

/// The limit type of the vendor's usage-credit limit: money, not a window.
/// READ in the 2.1.286 bundle, its own table of names: `five_hour` "session
/// limit", `seven_day` "weekly limit", `seven_day_opus` "Opus limit",
/// `seven_day_sonnet` "Sonnet limit", `seven_day_overage_included` (a model
/// bucket's), `overage` "usage credit limit".
const CREDIT_LIMIT_TYPE: &str = "overage";

/// Whether `word` can be a limit type: 1 to [`LIMIT_TYPE_MAX`] bytes of
/// ASCII lowercase letters, digits and `_`, the shape of every one the vendor
/// names. The word becomes a key of the accounts state and a ledger row's
/// reason, so nothing else in a transcript is let through as one.
fn is_limit_type(word: &str) -> bool {
    (1..=LIMIT_TYPE_MAX).contains(&word.len())
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// THE QUOTA WALL A SESSION STANDS AT, as its transcript's limit row wrote
/// it ([`transcript_standing_wall`]): the facts the accounts recorder is
/// handed (`accounts::record_wall`), and nothing of the row's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandingWall {
    /// When the row was written: its own `timestamp`, unix seconds. What
    /// says WHICH PROCESS hit the wall: a row older than the session's
    /// process is the previous one's, still standing after a relaunch.
    pub at: u64,
    /// The reset the wall names (`quotaLimits.resetsAt`, unix seconds, past
    /// or to come); `None` where it names none.
    pub until: Option<u64>,
    /// Which of the account's limits it is (`quotaLimits.rateLimitType`:
    /// `five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`,
    /// `seven_day_overage_included`); where the row carries none, what its
    /// text names (`five_hour` or `seven_day`, `usage::notice_window`); else
    /// `None`.
    pub window: Option<String>,
    /// What the row says of the account's usage credits
    /// (`quotaLimits.overageStatus`): `None` where it says nothing.
    pub credits: Option<CreditsWord>,
}

/// What a limit row says of the account's usage credits
/// (`quotaLimits.overageStatus`, `overageDisabledReason`, `overageResetsAt`;
/// the keys MEASURED on this machine's rows, `"overageStatus":"rejected",
/// "overageDisabledReason":"org_level_disabled"`, the others' values READ in
/// the 2.1.300 bundle).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreditsWord {
    /// `allowed` or `allowed_warning`: credits are on.
    On,
    /// `rejected`: off — the vendor's reason (`out_of_credits`,
    /// `org_level_disabled`, …) and when they come back, where it names them.
    Off {
        reason: Option<String>,
        until: Option<u64>,
    },
}

impl CreditsWord {
    /// The word of a row's `quotaLimits`, if it says one.
    fn of(quota: &Value) -> Option<CreditsWord> {
        match quota.get("overageStatus").and_then(Value::as_str)? {
            "allowed" | "allowed_warning" => Some(CreditsWord::On),
            "rejected" => Some(CreditsWord::Off {
                reason: quota
                    .get("overageDisabledReason")
                    .and_then(Value::as_str)
                    .filter(|word| is_limit_type(word))
                    .map(str::to_owned),
                until: quota
                    .get("overageResetsAt")
                    .and_then(Value::as_u64)
                    .filter(|&at| at > 0),
            }),
            _ => None,
        }
    }
}

/// THE QUOTA WALL THE SESSION STANDS AT, by its transcript (`jsonl`, a tail):
/// [`transcript_limit_until`]'s limit row — the last main-chain row saying
/// whether the model can answer; a row of the session's own model after it,
/// or a `/login` that finished, ends it — read as an OBSERVATION OF THE
/// ACCOUNT (accounts design §5 and §6, "Which walls rotate"), and so only
/// when it is a QUOTA REJECTION: `quotaLimits.status` is `rejected`.
///
/// `None` wherever [`transcript_limit_until`] is, and for a standing limit
/// row that says nothing of the account's budget:
///
/// * AN API RATE LIMIT (`API Error: Rate limit reached`): it carries no
///   `quotaLimits`, and is the route's, not the account's.
/// * A SPEND OR CREDIT WALL: its text is one the wall table reads as money
///   ([`transcript_limit_spend`]), or its limit type is the vendor's
///   usage-credit limit (`overage`). No other account's window answers it.
/// * A ROW THAT NAMES NO TIME: nobody could say which process wrote it.
///
/// What a row of the wrong shape costs is its own key and no more: a reset
/// that is not a positive integer is no reset (the wall still stands, on the
/// recorder's ladder); a limit type that is not a string, or not a word
/// ([`LIMIT_TYPE_MAX`]), is read as absent — the schema makes it optional
/// (READ in the 2.1.286 bundle), and every row MEASURED here carries one
/// (2026-10-01, ten rows of 2.1.273 and 2.1.283).
#[must_use]
pub fn transcript_standing_wall(jsonl: &str) -> Option<StandingWall> {
    StandingScan::of(jsonl).wall()
}

/// One limit row read as the quota wall it stands for
/// ([`transcript_standing_wall`]).
fn standing_wall_of(v: &Value) -> Option<StandingWall> {
    let quota = v.get("quotaLimits")?;
    if quota.get("status").and_then(Value::as_str) != Some("rejected") || is_spend_row(v) {
        return None;
    }
    let named = quota
        .get("rateLimitType")
        .and_then(Value::as_str)
        .filter(|word| is_limit_type(word));
    if named == Some(CREDIT_LIMIT_TYPE) {
        return None;
    }
    Some(StandingWall {
        at: row_time(v)?,
        until: quota
            .get("resetsAt")
            .and_then(Value::as_u64)
            .filter(|&at| at > 0),
        window: named
            .or_else(|| row_texts(v).find_map(super::usage::notice_window))
            .map(str::to_owned),
        credits: CreditsWord::of(quota),
    })
}

/// THE LIMIT ROW A SESSION STANDS AT, as its transcript wrote it
/// ([`transcript_standing_limit`]): what the supervisor's loop reads as the
/// wall where the screen no longer draws it (a relaunch's or a restore's
/// redrawn conversation; `supervise/wall_confirm.rs`). Its words are SCREEN
/// TEXT to the loop: never typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandingLimit {
    /// The row's words (`You've hit your weekly limit · resets …`).
    pub text: String,
    /// The reset it names (`quotaLimits.resetsAt`, unix seconds, past or to
    /// come); `None` where it names none.
    pub until: Option<u64>,
    /// Which of the account's limits it is (`quotaLimits.rateLimitType`, a
    /// word of the vendor's shape, [`LIMIT_TYPE_MAX`]); `None` where it names
    /// none.
    pub window: Option<String>,
    /// When the row was written (its own `timestamp`, unix seconds).
    pub at: Option<u64>,
    /// The row's own id (`uuid`): a reply written after it is a turn served
    /// since the limit.
    pub row: Option<String>,
}

/// THE LIMIT ROW THE SESSION STANDS AT, by its transcript (`jsonl`, a tail):
/// [`transcript_limit_until`]'s row — the last main-chain row saying whether
/// the model can answer is a limit row, and no `/login` finished after it —
/// read whole ([`StandingLimit`]). `None` wherever that is.
#[must_use]
pub fn transcript_standing_limit(jsonl: &str) -> Option<StandingLimit> {
    StandingScan::of(jsonl).limit()
}

/// One limit row read whole ([`transcript_standing_limit`]).
fn standing_limit_of(v: &Value) -> Option<StandingLimit> {
    let quota = v.get("quotaLimits");
    Some(StandingLimit {
        text: row_texts(v).collect::<Vec<_>>().join(" "),
        until: quota
            .and_then(|q| q.get("resetsAt"))
            .and_then(Value::as_u64)
            .filter(|&at| at > 0),
        window: quota
            .and_then(|q| q.get("rateLimitType"))
            .and_then(Value::as_str)
            .filter(|word| is_limit_type(word))
            .map(str::to_owned),
        at: row_time(v),
        row: v.get("uuid").and_then(Value::as_str).map(str::to_owned),
    })
}

/// THE STANDING LIMIT'S ONE SCAN, a transcript row at a time: the limit row
/// the rows folded in so far stand at — the last main-chain row saying
/// whether the model can answer is a limit row ([`is_limit_row`]), and no
/// `/login` finished after it — which [`transcript_limit_until`],
/// [`transcript_limit_reset`], [`transcript_limit_spend`],
/// [`transcript_standing_wall`] and [`transcript_standing_limit`] each read.
/// A reader that reads a transcript row by row reads the limit through it
/// (`aterm-lab reduce`'s readings, session log ruling U7: the closed words a
/// reduction keeps).
#[derive(Clone, Debug, Default)]
pub struct StandingScan {
    /// That row, as the conversation reader reads it.
    row: Option<Value>,
}

impl StandingScan {
    /// The scan of every line of `jsonl`.
    #[must_use]
    pub fn of(jsonl: &str) -> Self {
        let mut scan = Self::default();
        for line in jsonl.lines() {
            scan.line(line);
        }
        scan
    }

    /// Folds one transcript line in. Non-JSON lines are skipped.
    pub fn line(&mut self, line: &str) {
        if !line.contains("assistant") && !line.contains("Login successful") {
            return;
        }
        let Ok(v) = super::transcript::conversation(line) else {
            return;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        if is_login_success_row(&v) {
            self.row = None;
            return;
        }
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        if is_limit_row(&v) {
            self.row = Some(v);
        } else if v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .is_some_and(is_model_id)
        {
            self.row = None;
        }
    }

    /// What `read` makes of the limit row that stands.
    fn read<T>(&self, read: impl Fn(&Value) -> Option<T>) -> Option<T> {
        self.row.as_ref().and_then(read)
    }

    /// [`transcript_limit_until`] of the rows folded in.
    #[must_use]
    pub fn until(&self) -> Option<u64> {
        self.read(limit_holds_until)
    }

    /// [`transcript_limit_reset`] of the rows folded in.
    #[must_use]
    pub fn reset(&self) -> Option<u64> {
        self.read(reset_named)
    }

    /// [`transcript_limit_spend`] of the rows folded in.
    #[must_use]
    pub fn spend(&self) -> bool {
        self.read(|v| is_spend_row(v).then_some(())).is_some()
    }

    /// [`transcript_standing_wall`] of the rows folded in.
    #[must_use]
    pub fn wall(&self) -> Option<StandingWall> {
        self.read(standing_wall_of)
    }

    /// [`transcript_standing_limit`] of the rows folded in.
    #[must_use]
    pub fn limit(&self) -> Option<StandingLimit> {
        self.read(standing_limit_of)
    }
}

/// WHEN THE SESSION'S NEWEST ANSWER CAME, by its transcript (`jsonl`, a
/// tail), of those that came by the second `by`: the latest `timestamp`, unix
/// seconds, no later than `by`, of an assistant row that is an API response —
/// the model's own (`message.model` a model id, a sidechain's included) or a
/// limit's rejection ([`is_limit_row`]). What Claude Code's notices of usage
/// beside the composer were true at: it draws them from the newest answer's
/// rate-limit headers, and leaves them standing at rest however long the tab
/// idles; `by` is where a screen read can have been drawn after an answer
/// (`accounts::UsageSaid::at`). `None`: no such row in the tail. It reads; it
/// records nothing.
#[must_use]
pub fn transcript_last_answer(jsonl: &str, by: u64) -> Option<u64> {
    jsonl
        .lines()
        .filter(|line| line.contains("assistant"))
        .filter_map(|line| {
            let v = super::transcript::conversation(line).ok()?;
            let answer = v.get("type").and_then(Value::as_str) == Some("assistant")
                && (is_limit_row(&v)
                    || v.get("message")
                        .and_then(|m| m.get("model"))
                        .and_then(Value::as_str)
                        .is_some_and(is_model_id));
            answer.then(|| row_time(&v)).flatten()
        })
        .filter(|at| *at <= by)
        .max()
}

/// The command Claude Code records for its usage-limit dialog: it submits
/// `/rate-limit-options` itself at a wall, and a person can type it.
const LIMIT_DIALOG_COMMAND: &str = "/rate-limit-options";

/// The tag a local command's output row opens with, and the one it closes
/// with.
const COMMAND_STDOUT: (&str, &str) = ("<local-command-stdout>", "</local-command-stdout>");

/// The most of a dialog's result line that is handed on, in bytes: the
/// vendor's own lines are under two hundred.
pub const RESET_LINE_MAX: usize = 400;

/// What a local command wrote in one transcript row: the command's own row
/// or its output. Two shapes, by the Claude Code version and the result's
/// kind (as [`is_login_success_row`] reads them): a `user` row whose
/// `message.content` is a string, or a `system` row with `subtype`
/// `local_command` and a top-level `content`. `None` for every other row —
/// a tool result and a prompt with an attachment are block arrays.
fn local_command_text(v: &Value) -> Option<&str> {
    match v.get("type").and_then(Value::as_str) {
        Some("user") => v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str),
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("local_command") => {
            v.get("content").and_then(Value::as_str)
        }
        _ => None,
    }
}

/// THE LINE THE USAGE-LIMIT DIALOG ENDED WITH, by the session's transcript
/// (`jsonl`, a tail): the text of the LAST `<local-command-stdout>` row
/// written at or after `since` (unix seconds) that is the output of a
/// `/rate-limit-options` command — the last local command row before it is
/// that command's own. What a limit-reset attempt's outcome is read from
/// (`aterm_phase::reset_line`, with `since` the second the attempt was
/// claimed): the dialog writes ONE result line when it ends, as its own two
/// rows, and nothing on the screen says more than that the dialog is gone.
///
/// THE LAST, because a claim can outlive a dialog: one a person answered
/// with the wait row after the claim was made, and before the dialog the
/// attempt went through, left its own line since the claim. The attempt's
/// answer is the newest; read as the first, a reset that was spent was
/// recorded as that earlier line's failure.
///
/// MEASURED for this dialog's wait row (the owner's transcripts: the command
/// row, then `<local-command-stdout>Claude Code will continue automatically
/// at 6am`); READ in the 2.1.287 and 2.1.304 bundles for every other line —
/// 2.1.304 still answers under `/rate-limit-options` — a result
/// drawn as `skip` writes no row at all. NO RESET LINE HAS BEEN SEEN in any
/// transcript.
///
/// `None`, each a row that is not this dialog's answer since the claim:
///
/// * A ROW OLDER THAN `since`, or one that names no time: an earlier
///   dialog's line (the wait an earlier wall armed) is not this attempt's.
/// * THE OUTPUT OF ANOTHER COMMAND (`/model`, `/effort`), whenever written:
///   only the output under the dialog's own command row is read, and an
///   output row ends what its command armed.
/// * An error output (`<local-command-stderr>`), a sidechain's row, and a
///   row that is no local command's ([`local_command_text`]).
///
/// The text is the row's, between its tags, as one line of at most
/// [`RESET_LINE_MAX`] bytes; a row that says nothing answers the empty line.
/// It classifies nothing.
#[must_use]
pub fn transcript_reset_line(jsonl: &str, since: u64) -> Option<String> {
    // Whether the last local command row was the dialog's own.
    let mut dialog = false;
    let mut last = None;
    for line in jsonl.lines() {
        if !line.contains("<command-") && !line.contains("<local-command-") {
            continue;
        }
        let Ok(v) = super::transcript::conversation(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(text) = local_command_text(&v).map(str::trim_start) else {
            continue;
        };
        if text.starts_with("<command-name>") || text.starts_with("<command-message>") {
            dialog = command_line(text) == LIMIT_DIALOG_COMMAND;
        } else if let Some(said) = text.strip_prefix(COMMAND_STDOUT.0) {
            if std::mem::take(&mut dialog) && row_time(&v).is_some_and(|at| at >= since) {
                let said = said.trim_end();
                let said = said.strip_suffix(COMMAND_STDOUT.1).unwrap_or(said);
                last = Some(super::one_line(said, RESET_LINE_MAX));
            }
        } else if text.starts_with("<local-command-stderr>") {
            dialog = false;
        }
    }
    last
}

/// What a transcript's tail says of the login to an upgrade ([`transcript_login`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranscriptLogin {
    /// The wall stands ([`transcript_login_wall`]): [`Facts::login`].
    pub stands: bool,
    /// When the last wall was lifted ([`login_lifted_at`]): the clocks start
    /// again from it ([`clock_held_until`]).
    pub lifted_at: Option<u64>,
    /// Whether the latest notice never reached the model ([`notice_fate`]),
    /// for an announced or given-up phase; `None` when the tail holds no such
    /// notice or the phase asks nothing ([`Facts::undelivered`] reads it
    /// `false`; the driver looks further back for a give-up's).
    pub undelivered: Option<bool>,
}

/// The transcript's word on the login for an upgrade in `phase` whose latest
/// notice carries `marker`, from `tail`: the driver's one fold of it
/// (`upgrade_drive::login_facts`), bound by the conformance walk.
#[must_use]
pub fn transcript_login(phase: &Phase, marker: &str, tail: &str) -> TranscriptLogin {
    let asks = match phase {
        Phase::Announced { .. } => true,
        Phase::Failed(why) => why == GAVE_UP,
        _ => false,
    };
    TranscriptLogin {
        stands: transcript_login_wall(tail),
        lifted_at: login_lifted_at(tail),
        undelivered: (asks && !marker.is_empty())
            .then(|| notice_fate(tail, marker))
            .flatten(),
    }
}

/// The login fact ([`Facts::login`]) of a screen of `agent`'s, by the
/// agent's reader the supervisor reads walls with
/// ([`aterm_phase::ScreenReader::wall`]): Claude Code's `⏺ Login expired ·
/// Please run /login` or `Not logged in · …`, Codex's login notice.
#[must_use]
pub fn login_wall(agent: Agent, rows: &[String]) -> bool {
    agent
        .program()
        .reader()
        .wall(rows)
        .is_some_and(|w| w.kind == aterm_phase::WallKind::Auth)
}

/// The ask number of the next notice ([`Step::Announce`]) of an upgrade in
/// `phase`: one more than its asks, except when its last notice never reached
/// the model (`undelivered`: the login wall's, [`notice_fate`], or one queued
/// behind a limit now over, [`queued_until`]), which spent no ask and is typed
/// again with the same READY marker.
#[must_use]
pub fn announce_asks(phase: &Phase, undelivered: bool) -> u32 {
    match phase {
        Phase::Announced { asks, .. } if undelivered => (*asks).max(1),
        Phase::Announced { asks, .. } => asks.saturating_add(1),
        _ => 1,
    }
}

/// Every READY marker of a round an older build recorded only the latest of:
/// each ask's, minted as its notice did ([`ready_marker`] over the round's
/// `salt` and the ask).
#[must_use]
pub fn round_markers(session_id: &str, to: &Version, salt: u64) -> Vec<String> {
    (1..=MAX_ASKS)
        .map(|asks| ready_marker(session_id, to, salt.wrapping_add(u64::from(asks))))
        .collect()
}

/// How many of the round's notices (`markers`) reached the model by `jsonl`:
/// every one whose turn was not the login wall's ([`notice_fate`]), plus any
/// the tail no longer holds (which cannot be shown unread).
#[must_use]
pub fn notices_received(jsonl: &str, markers: &[String]) -> u32 {
    let received = markers
        .iter()
        .filter(|m| notice_fate(jsonl, m) != Some(true))
        .count();
    u32::try_from(received).unwrap_or(u32::MAX)
}

/// A give-up spent on notices that never reached the model is taken back: a
/// given-up upgrade ([`GAVE_UP`]) whose last notice was the wall's
/// (`undelivered`) is treated as if unread notices spent no ask: pending when
/// none reached the model (`received`, [`notices_received`]), else announced
/// with the asks that did and its window run out. It gives up again only after
/// [`MAX_ASKS`] received notices. `None`: nothing to take back.
#[must_use]
pub fn rearmed(phase: &Phase, undelivered: bool, received: u32) -> Option<Phase> {
    match phase {
        Phase::Failed(why) if why == GAVE_UP && undelivered && received < MAX_ASKS => {
            Some(if received == 0 {
                Phase::Pending
            } else {
                Phase::Announced {
                    at_s: 0,
                    asks: received,
                }
            })
        }
        _ => None,
    }
}

/// The harness's mark: how every turn the harness types into an agent's
/// conversation begins (the upgrade's notice [`ANNOUNCE_HEAD`], its
/// continuation [`continue_prompt`], a relaunch's
/// `relaunch::resumed_prompt`, the Codex branch's). Tells the harness's own
/// turns from anyone else's ([`TaskScan`]).
pub const HARNESS_MARK: &str = harness_mark!();

// NO KEY OF A ONE-KEY CARD OPENS A PRESSED TEXT (measured 2026-10-01): Claude
// Code's session survey and its other one-key cards answer from the
// composer's VALUE, only while it is exactly one of their keys — a digit, or
// `y`/`n`/`d` — so a text answers one only if it OPENS with such a key. Every
// pressed text (the notice, the release) opens with the harness's mark.
const _: () = {
    const fn opens_with_a_card_key(text: &str) -> bool {
        matches!(
            text.as_bytes()[0],
            b'0'..=b'9' | b'y' | b'n' | b'd' | b'Y' | b'N' | b'D'
        )
    }
    assert!(!opens_with_a_card_key(HARNESS_MARK));
    assert!(!opens_with_a_card_key(ANNOUNCE_HEAD));
    assert!(!opens_with_a_card_key(RELEASE_HEAD));
};

/// WHETHER A CONVERSATION HAS A TASK: a
/// conversation nobody has asked anything — no turn from a PERSON or an
/// ORCHESTRATOR (a control-socket client other than the harness) — is
/// TASKLESS, however many turns the harness itself typed into it. Folded over
/// the conversation's transcript, one line at a time ([`Self::line`]), oldest
/// first: the record is appended to, so a caller can read on from where it
/// stopped (`upgrade_drive::Tasked`).
///
/// A task starts at the first prompt of someone else's: a main-chain user row
/// whose words are not the harness's ([`HARNESS_MARK`]), counted as soon as it
/// is recorded (before any answer), so a restart never ends a first prompt in
/// flight. Someone else's command (`<command-name>…`) is a task once it starts
/// a turn (an assistant row after it, `<synthetic>` included), none once its
/// local output shows it started none (`/model`), and a task while neither is
/// read. A peer's message (`origin.kind` `peer`, recorded `isMeta`) and a row
/// of an origin kind not known here are someone's prompts too. Not prompts:
/// subagent rows (`isSidechain`), Claude Code's command or skill expansions
/// (`isMeta`), tool results, local command output
/// (`<local-command-…>`, in a `user` row or, since 2.1.302's `/clear`, a
/// `system` row of subtype `local_command`), Esc's `[Request interrupted by
/// user…`. Any other shape counts as a task.
///
/// THE HARNESS'S OWN TURNS are of two kinds, and neither is a task: the ones
/// the upgrade and the relaunch type, which begin with [`HARNESS_MARK`], and
/// the ones the supervisor's turn-end policy types — `keep going`,
/// `answer_text`, an accepted suggestion, a wall's retry — whose words are
/// the owner's and carry no mark, but which its loop's own ledger records as
/// it types them ([`Self::supervisor_typed`]:
/// `crate::supervise::approvals::typed_texts`). A row is the loop's only
/// where the loop typed THE SAME WORDS AS THE ROW WAS RECORDED (within
/// [`TYPED_WITHIN_MS`] of its `timestamp`): the ledger is the TAB's, kept
/// across every conversation the tab has held, and read by its words alone
/// a person who began a new conversation there with the loop's `keep going`
/// made it taskless — the upgrade restarted it afresh, out of its tab. An
/// unmarked row with no time, or with none of the
/// loop's texts typed near it, is someone's: in doubt, the conversation is
/// kept. The policy types those only where a task already stands, except
/// where nobody could say (a record that could not be read yet, a Codex, a
/// conversation not registered yet): there one `keep going` read as
/// someone's would make the conversation tasked for good.
///
/// The text and ledger are used rather than the vendor's fields because every
/// terminal input reads `"promptSource":"typed"`, `"origin":{"kind":"human"}`;
/// the mark survives in the conversation's record across restarts and
/// resumes, where the screen does not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskScan {
    /// A command of someone else's waits: the turn it starts, or the local
    /// output that says it started none. Read as a task meanwhile.
    command: bool,
    /// Someone else's prompt, or a command that started a turn, was read:
    /// the conversation has a task, for good.
    tasked: bool,
    /// What the session's supervisor typed, and when, from its ledger: a
    /// row with the same words, recorded as they were typed, is its, not
    /// someone else's.
    ours: Vec<Typed>,
}

/// How far apart the loop's ledger row of a text it typed and Claude Code's
/// record of it may be ([`TaskScan`]): the loop writes its row as its Enter
/// lands, Claude Code records the prompt as it is submitted — the same
/// moment, but for the few calls between and a prompt hook's run.
pub const TYPED_WITHIN_MS: u64 = 60_000;

impl TaskScan {
    /// Read on knowing `ours`: the texts the session's supervisor typed,
    /// and when ([`TaskScan`]'s second kind of the harness's own turns).
    pub fn supervisor_typed(&mut self, ours: &[Typed]) {
        ours.clone_into(&mut self.ours);
    }

    /// Folds one transcript line in and returns whether the conversation has a
    /// task now ([`Self::tasked`]). Non-JSON lines are skipped.
    pub fn line(&mut self, line: &str) -> bool {
        if self.tasked
            || !(line.contains("\"user\"")
                || line.contains("\"assistant\"")
                || line.contains("\"local_command\""))
        {
            return self.tasked();
        }
        let Ok(v) = super::transcript::conversation(line) else {
            return self.tasked();
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return self.tasked();
        }
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") => self.tasked = self.command,
            // A PEER'S MESSAGE is someone's ask (`origin.kind` `peer`, which
            // Claude Code records `isMeta`, so its words read as no prompt),
            // and so is a row of any kind not known here, as
            // [`FinishedScan`] reads them: a conversation only a peer
            // directed read taskless, and the upgrade restarted it afresh,
            // its conversation left behind. Claude Code's own wakes and a
            // person's terminal input are read by their words below (a
            // background task's notification there is a task: in doubt the
            // conversation is kept).
            Some("user") if origin_is_someone(&v) => self.tasked = true,
            Some("user") => match prompt_of(&v, &self.ours) {
                Prompt::Theirs => self.tasked = true,
                Prompt::Command => self.command = true,
                Prompt::Harness | Prompt::LocalOutput => self.command = false,
                Prompt::None => {}
            },
            // A local command's output in its newer shape (2.1.302's
            // `/clear`): a `system` row of subtype `local_command`, its
            // `<local-command-…>` text the top-level `content`
            // ([`local_command_text`]) — the command before it started no
            // turn, as the `user` row of that text says. Any other `system`
            // row says nothing of a command.
            Some("system")
                if local_command_text(&v)
                    .is_some_and(|t| t.trim_start().starts_with("<local-command-")) =>
            {
                self.command = false;
            }
            _ => {}
        }
        self.tasked()
    }

    /// Whether the conversation has a task: someone else's prompt was read,
    /// or a command of theirs that has not said it started no turn.
    #[must_use]
    pub fn tasked(&self) -> bool {
        self.tasked || self.command
    }

    /// Whether it has one for good: nothing read later can take it away, so
    /// nothing more need be read.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.tasked
    }
}

/// Whether user row `row` is someone's by its origin's kind alone: a peer's
/// message, or a kind this build does not know — never Claude Code's own
/// wakes (`task-notification`, `auto-continuation`), a person's terminal
/// input (`human`) or a row that names no origin, which are read by their
/// words ([`asked_by`], [`TaskScan`]).
fn origin_is_someone(row: &Value) -> bool {
    row.get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| !matches!(kind, "human" | "task-notification" | "auto-continuation"))
}

/// [`TaskScan`] over a whole transcript, its session's supervisor having
/// typed `ours`.
#[must_use]
pub fn transcript_tasked(jsonl: &str, ours: &[Typed]) -> bool {
    let mut scan = TaskScan::default();
    scan.supervisor_typed(ours);
    for line in jsonl.lines() {
        if scan.line(line) && scan.settled() {
            break;
        }
    }
    scan.tasked()
}

/// Whose prompt a main-chain user row is ([`TaskScan`]).
enum Prompt {
    /// The harness's own turn ([`HARNESS_MARK`]).
    Harness,
    /// A person's or an orchestrator's.
    Theirs,
    /// A command of theirs (`<command-name>…`): a turn only if it starts one.
    Command,
    /// A local command's output: the command before it started no turn.
    LocalOutput,
    /// No prompt at all: an expansion, a tool's result, Claude Code's own note.
    None,
}

fn prompt_of(row: &Value, ours: &[Typed]) -> Prompt {
    if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return Prompt::None;
    }
    let content = row.get("message").and_then(|m| m.get("content"));
    let text = match content {
        Some(Value::String(t)) => Some(t.as_str()),
        Some(Value::Array(parts)) => {
            let mut texts = parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str));
            match texts.next() {
                Some(t) => Some(t),
                // Every part is a tool's result (or an image): the agent's
                // own loop, no prompt.
                None if !parts.is_empty()
                    && parts
                        .iter()
                        .all(|p| p.get("type").and_then(Value::as_str) == Some("tool_result")) =>
                {
                    return Prompt::None;
                }
                None => None,
            }
        }
        _ => None,
    };
    // A paste is read as what was pasted: the harness's own notice arrives
    // wrapped ([`typed_words`]).
    let Some(text) = text.map(typed_words) else {
        return Prompt::Theirs;
    };
    let supervisors = |said: &str| typed_by(ours, said, recorded_ms(row), |_| true);
    if text.starts_with(HARNESS_MARK) {
        Prompt::Harness
    } else if text.starts_with("<local-command-") {
        Prompt::LocalOutput
    } else if text.starts_with("<command-name>") || text.starts_with("<command-message>") {
        if supervisors(&command_line(text)) {
            Prompt::Harness
        } else {
            Prompt::Command
        }
    } else if text.starts_with(INTERRUPTED) {
        Prompt::None
    } else if supervisors(text) {
        Prompt::Harness
    } else {
        Prompt::Theirs
    }
}

/// When user row `row` was recorded, unix ms (whole seconds): a row with no
/// time is no row of the loop's ([`typed_by`]), and opens no turn an older
/// build's note of a left exchange names ([`LastTurn::opened_ms`]).
fn recorded_ms(row: &Value) -> Option<u64> {
    row.get("timestamp")
        .and_then(Value::as_str)
        .and_then(models::parse_utc)
        .map(|s| s.saturating_mul(1000))
}

/// Whether the session's supervisor typed words `said`, recorded at `at_ms`
/// ([`recorded_ms`]), by its ledger `ours`: a text of the same words typed
/// within [`TYPED_WITHIN_MS`] of it, whose row `of` accepts (its rule:
/// [`supervisors_plain`]). A row with no time is none of the loop's.
fn typed_by(ours: &[Typed], said: &str, at_ms: Option<u64>, of: impl Fn(&Typed) -> bool) -> bool {
    at_ms.is_some_and(|at| {
        ours.iter().any(|o| {
            same_words(&o.text, said)
                && u64::try_from(o.at_ms).is_ok_and(|t| t.abs_diff(at) <= TYPED_WITHIN_MS)
                && of(o)
        })
    })
}

/// Whether two texts are the same words, however spaced or wrapped.
fn same_words(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

/// The command a command row records, as it was typed: `/<name> <args>`
/// from Claude Code's `<command-name>/<name></command-name>` and
/// `<command-args><args></command-args>`.
fn command_line(row: &str) -> String {
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        row.split_once(&open)
            .and_then(|(_, rest)| rest.split_once(&close))
            .map_or("", |(inner, _)| inner.trim())
    };
    format!("{} {}", tag("command-name"), tag("command-args"))
        .trim()
        .to_string()
}

/// What the driver measured about one session, for the gates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Claude's own `status` (the session file).
    pub status: String,
    /// Seconds since Claude last changed `status`.
    pub status_age_s: u64,
    /// The composer is present and holds no typed text (a dim placeholder
    /// suggestion is not typed text — [`composer_is_empty`]).
    pub composer_empty: bool,
    /// An approval or question box is up.
    pub approval_box: bool,
    /// The busy footer (`esc to interrupt`) is on screen.
    pub busy_footer: bool,
    /// Executable basenames of the agent's descendants that are background
    /// work: shells and `caffeinate`. Not MCP servers (resume restarts them)
    /// nor the agent's own per-turn keep-awake `caffeinate`, its direct child
    /// (`upgrade_drive::background`).
    pub background: Vec<String>,
    /// aterm's hold on the session (a halt, or a driver's custody or lease).
    pub held: bool,
    /// Seconds the screen has been unchanged (no output, no keystroke echo).
    pub quiet_s: u64,
    /// Seconds a person's hold ([`person_hold`]) has stood, every sample of it
    /// the driver took showing one, none more than [`HOLD_GAP_S`] apart
    /// ([`hold_since`]; 0: none now).
    pub hold_s: u64,
    /// The owner's `aterm harness upgrade <sid> --now` is in force
    /// ([`Request::Now`], [`requested_step`]): the upgrade stands at the last
    /// rung at once ([`Rung::Land`]), with no settling window and a person at
    /// the tab counting only if they typed within [`KEYS_GAP_S`]. Everything
    /// else is still asked of the current screen (Claude idle, empty composer,
    /// no box, busy footer or hold, no keystroke within [`KEYS_GAP_S`]), plus
    /// READY and an empty process tree for the restart ([`gate_restart`]).
    pub owner_now: bool,
    /// A person is at the tab: someone gave this session input within
    /// `[harness] human_grace_s`, by the server's per-session person stamp
    /// ([`attended_by`] over `text --json`'s `"human_ms"`, `status
    /// human_ms=`), the same grace the supervisor's question policy and
    /// relaunch use. Not tab focus or the OS-wide input clock, which do not
    /// say who is at this session. Nothing the upgrade decides itself is typed
    /// or signalled here ([`gate_announce`], [`gate_restart`]) unless `--now`
    /// is in force ([`Self::owner_now`]). It is the gate's first wait after a
    /// hold, ahead of Claude's status. The upgrade owns the tab's turn ends
    /// meanwhile, through the grace to the look that finds it lapsed
    /// (`upgrade_drive::owns_turn_ends`, bounded by a backstop), so the loop
    /// does not type over a READY; a look past the grace that finds the
    /// agent's work running waits on that and gives the turn ends back.
    pub attended: bool,
    /// The step is taken at a break of the agent's own background work: its
    /// turn is over and only work it started runs (a workflow, a background
    /// agent, a shell, a Codex background terminal), as the session's loop saw
    /// it stand [`QUIET_S`] (`supervise`'s `background_settle`). There, and
    /// where the status only lags an idle screen ([`status_lags`]), Claude's
    /// `busy`/`shell` status does not hold a notice (the first, or a re-ask
    /// after [`REASK_S`]); the loop's settle stands for the screen's. The
    /// restart still needs an idle point ([`next_step`]: `background`); a
    /// held READY is voided and an owed release typed here as there
    /// ([`gate_release`]).
    pub background_point: bool,
    /// The conversation has no task ([`TaskScan`]): nothing to preserve, so no
    /// notice is typed and nothing is resumed; the agent is restarted afresh
    /// on the new build ([`Step::Fresh`]) once idle the full [`QUIET_S`]. A
    /// person at the tab, a hold, a box, a draft and running work still wait.
    pub taskless: bool,
    /// The session stands at a usage or rate limit ([`limited`], from the
    /// same screen as every other fact): anything typed is queued behind the
    /// limit or answered by an error, never read, even while the status reads
    /// `idle` with an empty composer. Nothing is typed and no upgrade clock
    /// runs while this stands ([`gate_announce`], [`next_step`],
    /// [`clock_held`]); `--now` does not waive it.
    ///
    /// The transcript can say it too: a latest notice the limit answered and
    /// the model has not taken ([`NoticeFate::Queued`]) means limited whatever
    /// the screen shows, until the limit is over by the transcript
    /// ([`queued_until`]); past that the notice is [`Facts::undelivered`], or
    /// with a full queue [`Facts::queued`].
    pub limited: bool,
    /// The session stands at the login wall: the screen shows it
    /// ([`login_wall`]) or the transcript's last login word is the
    /// `authentication_failed` row ([`transcript_login_wall`]). Every turn is
    /// then answered by `Login expired · Please run /login` without reaching
    /// the model. A wait like the limit's ([`gate_announce`], [`next_step`],
    /// [`clock_held`]), lifted by a person's `/login`, never waived by `--now`.
    pub login: bool,
    /// The latest notice never reached the model: its turn ended on the login
    /// wall ([`notice_fate`]). It spent no ask: [`next_step`] types it again as
    /// the same ask ([`announce_asks`]) at the first point a notice could go,
    /// never waiting out its window or giving up on it (a give-up spent on it
    /// is taken back first, [`rearmed`]).
    ///
    /// Likewise a notice a usage limit answered whose limit is over by the
    /// transcript ([`queued_until`]) while the screen shows none: typed again
    /// as the same ask, at once while at most [`REQUEUE_MAX`] notices wait
    /// untaken ([`queued_copies`]), else only after the queue's rest or on the
    /// owner's word ([`Facts::queued`] until then).
    pub undelivered: bool,
    /// The conversation's queue is full: the latest notice was answered by a
    /// usage limit now over (by transcript and screen), more than
    /// [`REQUEUE_MAX`] notices wait untaken ([`queued_copies`]), and neither
    /// the rest before one more ([`Scan::rests_until`], [`queue_rest`]) nor a
    /// `--now` since the latest has come. Nothing is typed behind them
    /// ([`gate_announce`]: `queued`) and no clock runs ([`clock_held`]) until a
    /// turn passes the limit and the model takes them all, the rest runs out
    /// (one more copy), or the owner presses `Upgrade now` (one more copy).
    pub queued: bool,
    /// Seconds the READY the upgrade acts on has stood unacted on
    /// ([`ready_since`]; 0: none, or first heard at this look), held like
    /// every upgrade clock while the session is limited and restarted by every
    /// new notice. Bounds how long background work may hold a READY restart
    /// ([`next_step`]): the answer gets a whole [`REASK_S`] of its own before
    /// that work supersedes it, however late in its notice's window it came.
    pub ready_s: u64,
    /// How many consecutive looks, this one included, read the screen
    /// authoritatively idle by the session's reader ([`aterm_phase::read_at`]:
    /// prompt box drawn and holding the cursor, no spinner, busy footer, box
    /// or wall); 0 for any other look. Claude's `busy`/`shell` status is read
    /// against it ([`IDLE_LOOKS`], [`gate_announce`]).
    pub idle_looks: u32,
    /// Seconds since the round stopped ([`Phase::Failed`];
    /// `upgrade_drive::St::failed_at`; 0: not stopped, or stopped at this
    /// look). An older build's unstamped stop reads as long ago. Past
    /// [`RETRY_S`] the round is re-armed ([`Step::Rearm`]).
    pub failed_s: u64,
    /// The agent's own work has ended: a look since the latest notice saw its
    /// work run (a break, or processes under it; `upgrade_drive::St::work_seen`)
    /// and this look is an idle point with nothing under the agent. What the
    /// agent's "not yet" waited on is over, so [`next_step`] asks again at once
    /// rather than after [`REASK_S`] (or re-arms a given-up round and asks in
    /// the same visit) once [`gate_restart`] would let a READY agent go. At
    /// most once per end of work (the notice clears the stamp). What that gate
    /// waits on holds an announced round only within the re-ask's window.
    pub work_ended: bool,
    /// The screen shows a READY the transcript has not recorded yet: the
    /// agent's last words on an idle screen ([`aterm_phase::said_tail`]) affirm
    /// readiness or carry a legacy marker ([`says_ready`]), from the
    /// process the notice reached, but the transcript tail does not. Claude
    /// Code draws the answer before its batched transcript append lands. The
    /// screen is no consent (only the transcript row lets the restart go), but
    /// it holds the point: [`next_step`] waits `ready-unrecorded`, owned like
    /// the settle (`upgrade_drive::owns_turn_ends`), so the loop's continuation
    /// cannot void it, and the next look reads the row.
    pub ready_unrecorded: bool,
    /// The agent's READY was superseded ([`ready_superseded`]): it answered
    /// this round's marker after the latest notice and someone directed the
    /// conversation afterwards (a person, a peer, the loop's continuation), so
    /// the answer is no consent ([`transcript_has_ready`]). [`next_step`] asks
    /// again at the first point a notice may go rather than waiting out
    /// [`REASK_S`]; while waiting for the agent's turn to settle it says
    /// `superseded`.
    pub ready_superseded: bool,
    /// The upgrade asks nothing first (the default, from `[harness]
    /// upgrade_ask = false`, and whatever the key for a pending round of a
    /// conversation whose worker finished its task, [`FinishedScan`]): no
    /// notice and no READY. A session is restarted
    /// onto the newer build ([`Step::Terminate`]) only where [`gate_restart`]
    /// would let a READY agent go (Claude `idle`, nothing under it, no
    /// background-work break, no box, draft or person at the tab, the
    /// ladder's settle) and never at a usage limit or the login wall. Until
    /// then the waiting record tells the owner a newer build is installed
    /// ([`quiet_step`]). `false` is the ask-and-drain flow in the module
    /// header.
    pub quiet: bool,
    /// The owner has not been told yet: the session was first seen behind less
    /// than [`QUIET_S`] ago, so no waiting record has stood. A restart that
    /// types nothing first ([`Step::Fresh`], and every restart under
    /// [`Self::quiet`]) waits `notice-shown` until it has, so the owner hears
    /// before the move. `false` in hand-built facts: told.
    pub unnoticed: bool,
    /// Where the upgrade stands on its ladder ([`rung`] of how long the session
    /// has been behind): what the gate may pass that a natural idle point
    /// would not. [`Rung::Prefer`] by default.
    pub rung: Rung,
    /// A person typed into this session within [`KEYS_GAP_S`] (the server's
    /// person stamp): the one presence fact kept from [`Rung::KeysOnly`] on,
    /// never waived, `--now` included. A missing stamp (older host, hand-run
    /// sweep) is no keystroke here; [`Self::attended`] fails closed on it
    /// below [`Rung::KeysOnly`], and typing shows as a draft
    /// ([`Self::composer_empty`]) at every rung.
    pub typing: bool,
    /// Seconds the session's reader has read the screen authoritatively idle
    /// with the same last words, look after look of one process, none more
    /// than `upgrade_drive::IDLE_RUN_GAP_S` apart (`St::still`; 0: this look
    /// begins the run or is not idle). The repaint-proof settle the ladder
    /// counts from [`Rung::Settled`]: a redrawn footer clock or spinner resets
    /// [`Self::quiet_s`] but not this. Positive only across at least two looks.
    pub still_s: u64,
}

/// The ladder's rungs: prefer a natural pause, and the longer the session has
/// been behind the less it waits. One ladder keyed on time behind ([`rung`]),
/// following aterm's own update ladder
/// (`docs/DESIGN-auto-apply-ladder-2026-09-21.md`): activity delays, never
/// disables. Every rung keeps every floor: aterm's hold, the usage limit and
/// login wall, a full queue, the agent's own turn and work (status, busy
/// footer, shells under it), a box, a draft, a keystroke within
/// [`KEYS_GAP_S`], READY and an empty process tree for the restart. The rungs relax only comfort waits: the
/// settle ([`QUIET_S`]) and a person merely near the tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    /// The natural idle point: Claude idle, the screen still [`QUIET_S`], no
    /// person within `[harness] human_grace_s`.
    #[default]
    Prefer,
    /// From [`RUNG_SETTLED_S`]: a pause that only looks quiet counts: the
    /// reader read the screen idle with the same last words across two looks
    /// [`QUIET_S`] apart ([`Facts::still_s`]), however it repainted between.
    Settled,
    /// From [`RUNG_KEYS_S`]: a person at the tab holds it only while they
    /// typed within [`KEYS_GAP_S`] ([`Facts::typing`]).
    KeysOnly,
    /// From [`RUNG_LAND_S`], and at once on `--now`: the first such pause,
    /// settle waived; still no keystroke within [`KEYS_GAP_S`], and every
    /// floor.
    Land,
}

impl Rung {
    /// The word `--status`, the ledger and the rows carry.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Rung::Prefer => "prefer",
            Rung::Settled => "settled",
            Rung::KeysOnly => "keys-only",
            Rung::Land => "land",
        }
    }
}

/// When a pause that only looks quiet starts to count ([`Rung::Settled`]).
pub const RUNG_SETTLED_S: u64 = 20 * 60;

/// When a person merely at the tab stops holding it ([`Rung::KeysOnly`]).
pub const RUNG_KEYS_S: u64 = 3_600;

/// When the upgrade moves at the first pause ([`Rung::Land`]); what the owner
/// is told the move comes by (`upgrade_status::Row::lands_by`).
pub const RUNG_LAND_S: u64 = 2 * 3_600;

/// How recent a keystroke still holds the upgrade from [`Rung::KeysOnly`] on,
/// and under `--now` ([`Facts::typing`]). Never relaxed.
pub const KEYS_GAP_S: u32 = 20;

const _: () = assert!(RUNG_SETTLED_S < RUNG_KEYS_S && RUNG_KEYS_S < RUNG_LAND_S);
const _: () = assert!(KEYS_GAP_S as u64 <= QUIET_S);

/// The last words' print for the repaint-proof settle ([`Facts::still_s`]):
/// `pid`'s screen `rows` above the composer's caret row (`composer_row`),
/// each end-trimmed, trailing blank rows dropped, hashed with the process
/// (FNV-1a, 64 bits, 16 hex digits). New words or a new process change it;
/// the composer row and everything under it (footer clocks, Codex's goal
/// timer, a context meter) do not.
#[must_use]
pub fn still_print(pid: u32, rows: &[String], composer_row: usize) -> String {
    let above = rows.get(..composer_row).unwrap_or(rows);
    let kept = above
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .map_or(0, |i| i + 1);
    let mut h = aterm_hash::Fnv1a64::new();
    h.write(&pid.to_le_bytes());
    for row in &above[..kept] {
        h.write(row.trim_end().as_bytes());
        h.write(b"\n");
    }
    format!("{:016x}", h.finish())
}

/// The rung of an upgrade `behind_s` seconds behind ([`Rung`]).
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeLadder",
        action = "Advance",
        project = "aterm_agent::harness::upgrade_drive::ladder_projection"
    )
)]
pub fn rung(behind_s: u64) -> Rung {
    if behind_s >= RUNG_LAND_S {
        Rung::Land
    } else if behind_s >= RUNG_KEYS_S {
        Rung::KeysOnly
    } else if behind_s >= RUNG_SETTLED_S {
        Rung::Settled
    } else {
        Rung::Prefer
    }
}

/// The rung the gate reads: [`Rung::Land`] under `--now`
/// ([`Facts::owner_now`], keeping every floor), else the ladder's own.
#[must_use]
pub fn rung_in_force(f: &Facts) -> Rung {
    if f.owner_now { Rung::Land } else { f.rung }
}

/// The grace a person's hand keeps the upgrade's acts off for at `rung`:
/// `[harness] human_grace_s` until [`Rung::KeysOnly`], then [`KEYS_GAP_S`]
/// (or `grace_s` if shorter). The drivers ask the server's person stamp with
/// it, for [`Facts::typing`], the restart's signal, the relaunch line's
/// `quiet=` (`upgrade_drive`), the Codex `/exit` and its daemon's move.
#[must_use]
pub fn person_grace(rung: Rung, grace_s: u32) -> u32 {
    if rung >= Rung::KeysOnly {
        grace_s.min(KEYS_GAP_S)
    } else {
        grace_s
    }
}

/// How many consecutive authoritatively idle looks ([`Facts::idle_looks`]),
/// with Claude's status standing [`QUIET_S`], before that status is read
/// against the screen and the work under the agent ([`gate_announce`]): one
/// look is a moment, and a status not yet written at a turn's end is no
/// disagreement.
pub const IDLE_LOOKS: u32 = 2;

/// The limit fact ([`Facts::limited`]) of a Claude Code screen, by the one
/// recogniser the supervisor reads limits with: [`aterm_phase::limit_notice`],
/// the notice the last turn ended on wherever drawn, of a kind that
/// [`aterm_phase::WallKind::reads_limited`]; asked of the notice itself rather
/// than [`aterm_phase::worker_phase`], where a busy signal (a background-work
/// break) wins over the standing wall.
#[must_use]
pub fn limited(rows: &[String]) -> bool {
    aterm_phase::limit_notice(rows).is_some()
}

/// A gate's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Go.
    Go,
    /// Not yet, and why (one word).
    Wait(&'static str),
}

/// Seconds Claude must have been idle and the screen quiet before anything is
/// typed or ended: long enough not to type over a person reading the answer,
/// short enough that an idle session moves within the hour. Measured on the
/// agent's own verdict ([`Facts::quiet_s`]).
///
/// A conversation with no task settles the same ([`Facts::taskless`],
/// [`Step::Fresh`]): right after launch its first prompt may be on its way
/// (an orchestrator's `await agent idle` returns at the first idle verdict).
/// After the settle the prompt has come (a task, recorded at once,
/// [`TaskScan`]) or the session has sat still this long.
///
/// Read at run time through [`quiet_s`], never directly.
pub const QUIET_S: u64 = 20;

/// The development seam that sets the settle ([`quiet_s`]), in seconds.
pub const QUIET_SEAM: &str = "ATERM_UPGRADE_QUIET_S";

/// The settle ([`QUIET_S`]) as this process runs it, read once: the constant,
/// unless a development build ([`aterm_types::dev_seam!`]: a debug build, or an
/// optimized one with this crate's `dev-seams` feature) starts with
/// [`QUIET_SEAM`] set to a positive whole number of seconds (anything else is
/// ignored). A shipped binary compiles no seam, so there it is [`QUIET_S`].
///
/// Every run-time read of the settle comes here — the gates ([`gate_announce`],
/// [`status_lags`]), the drivers' first re-look (`upgrade_drive::first_look`)
/// and their [`Facts::unnoticed`], and the
/// loop's background break and the step its host's request is read at
/// (`supervise`'s `background_settle`, `next_wait_step`) — so a seamed build is
/// the product with a shorter settle, never half of one.
///
/// For the live suite (`crates/aterm/tests/host_live.rs`), whose upgrade tests
/// are about what happens at the idle point, not how long the product waits
/// for it: the 20 s settle was 20 s of each one's 26-32 s alone (2026-10-06).
#[must_use]
pub fn quiet_s() -> u64 {
    static SETTLE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SETTLE.get_or_init(|| {
        aterm_types::dev_seam!(QUIET_SEAM)
            .and_then(|v| v.to_str()?.trim().parse::<u64>().ok())
            .filter(|&s| s > 0)
            .unwrap_or(QUIET_S)
    })
}

/// Whether a session whose person stamp reads `human` is attended
/// ([`Facts::attended`]): a person gave it input within `grace_s`
/// (`[harness] human_grace_s`). No person since the host began serving it
/// (`null`, `-`) is no one. A missing stamp (an older host, a hand-run
/// sweep) proves no absence and fails closed: attended, moved only by
/// `--now`.
#[must_use]
pub fn attended_by(human: crate::supervise::screen::HumanInput, grace_s: u32) -> bool {
    human.within(grace_s).unwrap_or(true)
}

/// May the announcement be typed now? No hold, no person at the tab
/// ([`Facts::attended`]), Claude idle and settled, an empty composer, no box,
/// no busy footer, a quiet screen (person and quiet waived only by `--now`,
/// [`Facts::owner_now`]). The same gate opens [`Step::Fresh`] for a taskless
/// conversation ([`Facts::taskless`]).
///
/// The person is asked first, before Claude's status, so a busy attended tab
/// waits `attended`. The upgrade owns the tab's turn ends only for the
/// person's grace (`upgrade_drive::owns_turn_ends`).
///
/// `not-idle` also covers a turn that ended with work still running: Claude
/// Code keeps its status `busy` (workflows) or `shell` (background shells),
/// and the restart would end that work, so [`gate_restart`] waits for an
/// idle point with nothing under the agent. The notice itself, though, is
/// typed at a break of the agent's background work
/// ([`Facts::background_point`]), once and again only after a whole
/// [`REASK_S`] with the work still running ([`next_step`]); there `busy` and
/// `shell` are no wait and the settle is the loop's, while a person, a hold,
/// a box, a draft and a live turn's busy footer still wait.
///
/// A session at its limit ([`Facts::limited`]) waits `limited`, at the login
/// wall ([`Facts::login`]) `login`: a notice there would be queued unread or
/// answered by the wall. `--now` waives neither. A conversation already
/// holding a full queue of untaken notices ([`Facts::queued`]) waits
/// `queued`; the driver lifts that for a `--now` since the latest notice and
/// once the queue has rested.
///
/// Claude's status is read against its screen: once it lags a turn that is
/// over ([`status_lags`]), it is no wait for one line (the notice, or an owed
/// release, [`gate_release`]), since each types one turn and ends nothing. It
/// is never read past for the restart ([`gate_restart`]), which needs Claude's
/// own `idle` because work inside the agent's process shows only in the
/// status: the restart waits `background` while work runs under the agent and
/// `status-stale` while only the status holds, both bounded by the re-ask and
/// drain ([`next_step`]). Short of a lag, a status the idle screen does not
/// bear out also waits `status-stale`. A running turn never reads
/// authoritatively idle, and a person's `waiting` is never read past.
#[must_use]
pub fn gate_announce(f: &Facts) -> Gate {
    gate(f, status_lags(f))
}

/// [`gate_announce`] with Claude's `busy`/`shell` read past where `lags`
/// ([`status_lags`]): for a line, never for the restart ([`gate_restart`]).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeLadder",
        action = "Move",
        project = "aterm_agent::harness::upgrade_drive::ladder_projection"
    )
)]
fn gate(f: &Facts, lags: bool) -> Gate {
    let rung = rung_in_force(f);
    if f.held {
        return Gate::Wait("held");
    }
    // A person at the tab: before the KeysOnly rung anyone within
    // `human_grace_s`; from it, and under `--now`, only a keystroke within
    // KEYS_GAP_S, which nothing waives.
    if f.typing || (f.attended && rung < Rung::KeysOnly) {
        return Gate::Wait("attended");
    }
    if f.limited {
        return Gate::Wait("limited");
    }
    if f.login {
        return Gate::Wait("login");
    }
    if f.queued {
        return Gate::Wait("queued");
    }
    let own_work = matches!(f.status.as_str(), "busy" | "shell");
    let at_break = (f.background_point || lags) && (f.status == "idle" || own_work);
    if f.status != "idle" && !at_break {
        return Gate::Wait(if own_work && f.idle_looks > 0 {
            "status-stale"
        } else {
            "not-idle"
        });
    }
    if f.busy_footer {
        return Gate::Wait("busy");
    }
    if f.approval_box {
        return Gate::Wait("box");
    }
    if !f.composer_empty {
        return Gate::Wait("draft");
    }
    if rung == Rung::Land || f.background_point {
        return Gate::Go;
    }
    // The settle: screen and status still QUIET_S, or from the Settled rung
    // the reader's own run of idle looks at the same last words (a repainted
    // footer clock resets the first, never the second).
    let quiet = quiet_s();
    let settled = (f.status_age_s >= quiet && f.quiet_s >= quiet)
        || (rung >= Rung::Settled && f.still_s >= quiet);
    if !settled {
        return Gate::Wait("settling");
    }
    Gate::Go
}

/// Whether Claude's status lags a turn that is over ([`gate_announce`]): it
/// reads `busy` or `shell` (never a person's `waiting`), has stood
/// [`QUIET_S`], and the reader read the screen authoritatively idle
/// [`IDLE_LOOKS`] looks in a row ([`Facts::idle_looks`]). With nothing under
/// the agent the status is stale, for a line only; never for the restart
/// ([`gate_restart`]), since work inside the agent's process shows only in
/// the status.
#[must_use]
pub fn status_lags(f: &Facts) -> bool {
    matches!(f.status.as_str(), "busy" | "shell")
        && f.status_age_s >= quiet_s()
        && f.idle_looks >= IDLE_LOOKS
}

/// May the agent be ended now? Everything [`gate_announce`] asks, plus the
/// agent's READY and nothing running under it. Nothing is killed to pass:
/// background work is waited for, bounded by the re-ask in [`next_step`].
///
/// An attended tab is never signalled on the upgrade's own judgment, READY
/// or not; only `--now` waives it ([`Facts::owner_now`]). A SIGTERM in front
/// of a person is the one act the upgrade cannot take back.
///
/// Claude's own `idle` is required, never a status that only lags
/// ([`status_lags`]): such a status lets one line go but holds the signal
/// here (`status-stale`), since work inside the agent's process shows only in
/// the status. The READY it holds is bounded like one the agent's work holds
/// ([`next_step`]), so the agent is released, never left stopped. A taskless
/// conversation is not restarted afresh on it either ([`Step::Fresh`]).
///
/// Never at a break of the agent's own work ([`Facts::background_point`]):
/// the restart waits `background` there (work inside the agent's process is
/// not in [`Facts::background`]). [`next_step`]'s break arm answers first and
/// [`break_step`] turns an end into a wait; the gate agrees, `--now` included.
///
/// This is `HarnessUpgradeProgress`'s `RestartAtIdle` (READY heard, nothing
/// under the agent), reached through [`next_step`] and bound to a real
/// restart by the progress tests.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeProgress",
        action = "RestartAtIdle",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
pub fn gate_restart(f: &Facts, ready: bool) -> Gate {
    if !ready {
        return Gate::Wait("not-ready");
    }
    restart_gate(f, false)
}

/// [`gate_restart`] for an agent whose READY is given or not asked for,
/// with Claude's own `busy`/`shell` read past where `lags` — never on the
/// restart's own judgment: only the quiet flow's restart under the owner's
/// `--now` passes it ([`quiet_step`]).
fn restart_gate(f: &Facts, lags: bool) -> Gate {
    if !f.background.is_empty() {
        return Gate::Wait("background");
    }
    match gate(f, lags) {
        Gate::Go if f.background_point => Gate::Wait("background"),
        gate => gate,
    }
}

/// Whether the restart's gate waits on the AGENT'S OWN WORK (`w`, the
/// gate's wait): what runs under it, or what a break of its own work stands
/// for (`background`: [`gate_restart`] at a break, and [`next_step`]'s break
/// arm before it), or what Claude's own status says still runs over an idle
/// screen (`status-stale`). Neither is ever ended; both are bounded — a
/// READY they hold is asked again once [`REASK_S`] has passed, and a gave-up
/// upgrade's is void past [`DRAIN_S`] ([`next_step`], [`void_of`]).
pub(super) fn own_work_holds(w: &str) -> bool {
    matches!(w, "background" | "status-stale")
}

/// Which gate fact only a person can clear, if one holds now: `box` (an
/// approval or question box, or Claude's status `waiting`) or `draft` (unsent
/// composer text). The supervisor answers readable boxes within seconds, so a
/// box counts as a person's only once it has stood [`HOLD_S`]
/// ([`Facts::hold_s`]). The agent's own work, aterm's hold and the settle are
/// not a person's and are waited for as long as they last.
#[must_use]
pub fn person_hold(f: &Facts) -> Option<&'static str> {
    if f.approval_box || f.status == "waiting" {
        return Some("box");
    }
    if !f.composer_empty {
        return Some("draft");
    }
    None
}

/// How long after the announcement a READY still authorizes the restart while
/// a person holds the gate ([`person_hold`]). Past it the answer is void: the
/// stopping point it named is long past, and without the bound a person
/// returning hours later to answer the box or clear the draft would have the
/// agent ended under them on a stale answer. On expiry the driver forgets the
/// markers, records why, releases the agent ([`release_prompt`]) and restarts
/// the re-ask clock. At least [`REASK_S`]. Also bounds how long the agent's
/// own background work may hold a READY restart ([`Facts::ready_s`]).
pub const DRAIN_S: u64 = 30 * 60;

const _: () = assert!(DRAIN_S >= REASK_S);

/// How long a person's hold ([`person_hold`]) must have stood, samples no more
/// than [`HOLD_GAP_S`] apart, before it voids a READY past [`DRAIN_S`]: the
/// supervisor answers readable boxes within seconds and the driver samples a
/// held session once a minute, so one sample is not yet an unanswered box.
pub const HOLD_S: u64 = 5 * 60;

/// The longest gap between two hold-showing samples for the hold to count as
/// having stood through it ([`hold_since`]): three of the host's looks at a
/// held session (`upgrade_drive::HOLD_LOOK`). A longer gap (sleep, driver
/// restart, busy worker) is unsampled time: the boxes at either end may be
/// two different boxes, so the dwell begins again.
pub const HOLD_GAP_S: u64 = 3 * 60;

/// [`HOLD_S`] is never met across one gap: a hold voids only once at least three
/// samples, none more than [`HOLD_GAP_S`] apart, have shown it.
const _: () = assert!(HOLD_GAP_S < HOLD_S);

/// The person's hold standing now, for [`Facts::hold_s`]: `(since, seen)`,
/// the sample it began at and the last that showed it, from the previous
/// answer `prior` (`(0, 0)`: none). A sample without a hold returns `(0, 0)`;
/// one with a hold continues it while `seen` is within [`HOLD_GAP_S`], else
/// restarts it at `now_s`.
#[must_use]
pub fn hold_since(prior: (u64, u64), f: &Facts, now_s: u64) -> (u64, u64) {
    let (since, seen) = prior;
    match person_hold(f) {
        None => (0, 0),
        Some(_) if since == 0 || now_s.saturating_sub(seen) > HOLD_GAP_S => (now_s, now_s),
        Some(_) => (since, now_s),
    }
}

/// Whether the composer holds no TYPED text: present, and either blank or a dim
/// placeholder suggestion — text after the caret drawn DIM. `dim_at_caret` is
/// the CARET ROW's column 2 read with the `cell` verb (measured 2026-09-23
/// under Claude Code 2.1.280 and 2.1.281: the suggestion's first letter reads
/// `dim`, a typed draft's `none`); [`composer_dim_row`] names the row to read.
/// The cursor alone cannot tell them apart: a typed draft whose caret was
/// moved home (←, Home, ctrl-a) leaves it at column 2 too, and a notice typed
/// there lands IN FRONT of the person's draft and is submitted with it
/// (measured the same day). A suggestion drawn without SGR dim (no colour at
/// all) therefore reads as a draft: nothing is typed, which is the safe way
/// to be wrong. A draft on a second row is typed text whatever the cursor
/// says.
///
/// THE CURSOR CAN ONLY PROVE TYPED TEXT, NEVER PLACEHOLDER (after a net-zero
/// re-grid the engine's grid can sit a row off Claude's drawing, its cursor
/// below the composer, so requiring the cursor at column 2 read Claude's own
/// dim suggestion as a person's draft): a cursor on the caret row anywhere but
/// column 2 is inside typed text (a placeholder keeps it at column 2).
/// Anywhere else — another row, none — it says nothing about the composer, and
/// the dim cell decides. `cursor` is `(row, col)` in the same coordinates as
/// `rows`.
#[must_use]
pub fn composer_is_empty(
    rows: &[String],
    cursor: Option<(usize, usize)>,
    dim_at_caret: bool,
) -> bool {
    let Some((caret, lines)) = aterm_phase::phase::composer_draft(rows) else {
        return false;
    };
    if lines.iter().skip(1).any(|l| !l.is_empty()) {
        return false;
    }
    if lines.first().is_none_or(|l| l.is_empty()) {
        return true;
    }
    dim_at_caret && !aterm_phase::phase::cursor_inside_text(cursor, caret)
}

/// THE ROW A `cell` READ WOULD DECIDE ([`composer_is_empty`]'s
/// `dim_at_caret`): the caret row, when the composer is one line and the
/// cursor does not already prove its text typed. `None` where no read can
/// change the answer — no composer, text on a second row, a cursor inside
/// the text. (A blank composer is read too, as it always was: the read is
/// cheap, and asking it there keeps the request order the fence window's
/// tests pin.)
#[must_use]
pub fn composer_dim_row(rows: &[String], cursor: Option<(usize, usize)>) -> Option<usize> {
    let (caret, lines) = aterm_phase::phase::composer_draft(rows)?;
    if lines.iter().skip(1).any(|l| !l.is_empty())
        || aterm_phase::phase::cursor_inside_text(cursor, caret)
    {
        return None;
    }
    Some(caret)
}

/// Where the upgrade of one session stands, persisted per session id so the
/// next step (the window's host or a hand-run sweep) picks up where the last
/// one stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Nothing typed yet.
    #[default]
    Pending,
    /// The announcement was typed at `at_s` (unix seconds), `asks` times so far.
    Announced {
        /// When the latest announcement was typed.
        at_s: u64,
        /// How many announcements were typed for this upgrade.
        asks: u32,
    },
    /// SIGTERM was sent at `at_s`.
    Exiting {
        /// When.
        at_s: u64,
    },
    /// The relaunch line was typed at `at_s`.
    Relaunched {
        /// When.
        at_s: u64,
    },
    /// The new process holds the session and the continuation was typed.
    Done,
    /// This round stopped, and why. Never for good: [`RETRY_S`] after the stop
    /// ([`Facts::failed_s`]) a new round starts ([`Step::Rearm`],
    /// [`retry_due`]) unless the owner's `--skip` or an unexpired `--defer`
    /// holds it ([`requested_step`]). Meanwhile it waits `failed`, and a
    /// round that stopped asking ([`GAVE_UP`]) still honours a READY to one of
    /// its notices while the asked session lives ([`next_step`]).
    Failed(String),
}

/// The [`Phase::Failed`] reason of a round that stopped asking after
/// [`MAX_ASKS`] unanswered notices ([`Step::GiveUp`]); the owner's view says
/// `gave-up` and when the next round starts.
pub const GAVE_UP: &str = "unanswered";

/// Seconds between announcements when the agent has not answered READY, or
/// when work under it outlives a READY ([`next_step`]), at an idle point or a
/// break alike. Spent only while the session can read a notice: at a usage
/// limit the clock is held ([`clock_held`]).
pub const REASK_S: u64 = 30 * 60;

/// The most announcements one round makes: a bound on nagging, never a
/// reason to strand the agent. Past it the round stops asking
/// ([`Step::GiveUp`]), logs what still runs under the agent, releases it
/// ([`release_prompt`]), and still honours a late READY ([`GAVE_UP`]) until
/// the next round, [`RETRY_S`] later. Never a forced restart.
///
/// Only notices the model could read count: one that never reached it (the
/// login wall's, [`Facts::undelivered`], or one queued behind a limit now
/// over, [`queued_until`]) is typed again as the same ask, and none is typed
/// behind a limit that still holds ([`Facts::limited`]).
pub const MAX_ASKS: u32 = 4;

/// The most times a notice a usage limit answered is retyped straight away
/// before the model takes one ([`queued_copies`], [`Facts::undelivered`]): at
/// most `1 + REQUEUE_MAX` upgrade notices wait untaken at once, and past that
/// one more only after each rest ([`Scan::rests_until`], [`Facts::queued`]).
/// A limit row naming no reset reads as over every [`REASK_S`] however long
/// the limit really stands, so without the bound copies would pile up every
/// half hour. Past it the upgrade waits `queued` for a row of the model's own
/// (which takes every copy and opens the ask's window), for the queue's rest
/// ([`queue_rest`]), or for the owner's `--now` (one copy at once). Derived
/// model: `aterm_models_harness::harness_upgrade_limit_queue_model`
/// (`RetypeMax`, and `Daily` for [`QUEUE_REST_DOUBLINGS`]).
pub const REQUEUE_MAX: u32 = 2;

/// Seconds a STOPPED round of an upgrade ([`Phase::Failed`], whatever stopped
/// it) rests before the upgrade starts a new one ([`Step::Rearm`]): one
/// round's worth of asking, `MAX_ASKS × REASK_S` (two hours today).
///
/// WHY THAT LONG AND NO LONGER (an upgrade is never left stalled). A round
/// asks for at most `MAX_ASKS × REASK_S` before it gives up; resting exactly
/// as long again keeps the nagging to at most half of any stretch of time, one
/// round on, one round off, however long the agent's work lasts. And it BOUNDS
/// THE SILENCE, though not to one rest. At an agent that can read, the stretch
/// with no notice after a round's last one is the give-up's window, `REASK_S`,
/// plus the rest: this, stretched up to `RETRY_S << RETRY_BACKOFF_MAX_SHIFT`
/// for a stop that repeats ([`rest_extension`]). A late READY the gave-up
/// round hears holds the new round back while the answer stands
/// ([`retry_due`]: the round acts on it instead), and one the agent's own work
/// outlives is voided [`DRAIN_S`] after it — the rest then begins again at the
/// void, as long as the stop's streak makes it (not a bare `RETRY_S`, which
/// would undo the back-off for exactly the stop that repeats most). So the
/// longest silence is `REASK_S + rest + DRAIN_S + rest`, reached by a READY
/// heard just before the rest runs out: five hours today after a first stop,
/// seventeen after one repeated to the cap (two and a half and eight and a
/// half with no late READY) — a whole round the agent DECLINED rests by its
/// own ladder ([`unanswered_rest_extension`]: 2 h, 4 h, 8 h, a day), the void
/// beginning that rest again as it does any. Any other stop (a refused signal,
/// a relaunch that never came up, a conversation resumed elsewhere) hears no
/// late READY: its silence is its rest. Whatever else a late READY's restart
/// waits on ends the stretch when it passes, with the restart or the void,
/// never with a notice — the settle, a turn in progress, aterm's hold, a
/// person's box or draft (voided [`HOLD_S`] into its hold): the agent's or a
/// person's to end, not the upgrade's silence. The rest is not held at a usage
/// limit: the round it starts types nothing there (a limited session is never
/// asked), so no clock needs holding for it.
pub const RETRY_S: u64 = MAX_ASKS as u64 * REASK_S;

const _: () = assert!(RETRY_S >= REASK_S);

/// The longest a repeated stop stretches the rest, as a power of two of
/// [`RETRY_S`]: a stop whose reason repeats every round (a relaunch that never
/// comes up) rests `RETRY_S`, then twice, then four times as long, no longer.
/// Never terminal.
pub const RETRY_BACKOFF_MAX_SHIFT: u32 = 2;

/// How much LATER than [`RETRY_S`] after a stop the next round starts, for
/// the `streak`-th stop in a row with the same reason (`St::fail`): nothing
/// for the first, then `RETRY_S`, then `3 × RETRY_S` — the rest doubling up
/// to `RETRY_S << RETRY_BACKOFF_MAX_SHIFT`.
#[must_use]
pub fn rest_extension(streak: u32) -> u64 {
    let shift = streak.saturating_sub(1).min(RETRY_BACKOFF_MAX_SHIFT);
    RETRY_S.saturating_mul((1_u64 << shift) - 1)
}

/// THE REST AFTER A WHOLE ROUND THE AGENT DECLINED ([`GAVE_UP`]), in
/// [`RETRY_S`] multiples LATER than the first rest: the `streak`-th
/// unanswered round in a row rests 2 h, 4 h, 8 h, then a day
/// (`0, 1, 3, 11` extra multiples). A session that answered four notices with
/// "not yet" is one whose own work the upgrade never ends; asking it again
/// every two to eight hours is a full model turn on its whole context each
/// time, into the conversation the owner works in, for an answer that has not
/// changed (2026-09-28: seven typed turns in five hours and forty minutes, all
/// declined). The end of that work asks at once, whatever the rest
/// ([`Facts::work_ended`]), so the rest costs an agent whose work ends no
/// latency; what it delays is the chance a session with nothing to observe
/// changes its mind. Every other stop keeps [`rest_extension`]: a relaunch that
/// never comes up must not rest a day. (The owner's call, D10: this is the
/// default; the ladder is one table.)
#[must_use]
pub fn unanswered_rest_extension(streak: u32) -> u64 {
    let multiples = match streak {
        0 | 1 => 0,
        2 => 1,
        3 => 3,
        _ => 11,
    };
    RETRY_S.saturating_mul(multiples)
}

/// The least time between a notice and the next one THE END OF THE AGENT'S
/// OWN WORK may ask ([`Facts::work_ended`]): ten minutes. The ask at the end of
/// the work was exempt from [`MAX_ASKS`] and from every clock, so an agent that
/// answered "not yet" by starting a short job and idling was asked again at
/// the end of each one. Inside the floor the
/// ordinary clock decides, and the point goes to the loop as any does.
pub const WORK_END_FLOOR_S: u64 = 10 * 60;

/// The most notices the end of the agent's work may ask in any [`WORK_END_WINDOW_S`]:
/// six. Past it the ordinary clock and the rest decide, so the notices a
/// session that keeps starting and ending work is asked in a day are bounded
/// by the clock's own and this, not by how often its work ends.
pub const WORK_END_DAILY: u32 = 6;

/// The window [`WORK_END_DAILY`] counts in: a day.
pub const WORK_END_WINDOW_S: u64 = 24 * 60 * 60;

impl Phase {
    /// The word the ledger and `status` print.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Phase::Pending => "pending".to_string(),
            Phase::Announced { asks, .. } => format!("announced:{asks}"),
            Phase::Exiting { .. } => "exiting".to_string(),
            Phase::Relaunched { .. } => "relaunched".to_string(),
            Phase::Done => "done".to_string(),
            Phase::Failed(why) => format!("failed:{why}"),
        }
    }
}

/// One step the driver takes next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Type the announcement.
    Announce,
    /// Nothing to do this tick, and why.
    Wait(&'static str),
    /// End the agent (SIGTERM to its pid only).
    Terminate,
    /// The READY is void, and why (the [`person_hold`] word, or
    /// `background`): past [`DRAIN_S`] a person's state has held the gate for
    /// [`HOLD_S`] ([`person_void`]), or for a given-up upgrade (no re-ask left
    /// to supersede it) the agent's own background work has run [`DRAIN_S`]
    /// past the answer ([`void_of`]). Forget the markers, release the agent
    /// and record it; an announced upgrade asks again [`REASK_S`] later.
    Void(&'static str),
    /// Stop asking: [`MAX_ASKS`] announcements went unanswered, or none left a
    /// READY the restart could act on. The upgrade is then [`GAVE_UP`]: the
    /// agent is owed its release line, and a READY before that line is still
    /// honoured.
    GiveUp,
    /// The conversation has no task ([`Facts::taskless`]): SIGTERM the agent's
    /// pid and start the new build afresh in its tab, with nothing typed
    /// before or after and no `--resume`.
    Fresh,
    /// A stopped round has rested ([`retry_due`]): start a new round (pending,
    /// fresh salt and READY markers, asks reset, spent `--now` gone) and log
    /// `rearmed:<why>`. Types and ends nothing, so it is taken anywhere,
    /// including at a break or a limit; the new round asks under every gate.
    Rearm,
}

/// Whether a stopped round starts a new one now ([`Step::Rearm`]): it stopped
/// ([`Phase::Failed`], any reason) at least [`RETRY_S`] ago
/// ([`Facts::failed_s`]), and it is not a given-up upgrade holding a READY to
/// one of its notices (`ready`), which is still acted on ([`next_step`]).
#[must_use]
pub fn retry_due(phase: &Phase, failed_s: u64, ready: bool) -> bool {
    match phase {
        Phase::Failed(why) => !(why == GAVE_UP && ready) && failed_s >= RETRY_S,
        _ => false,
    }
}

/// The reducer for the cooperative half: from where the upgrade stands, the
/// facts and whether the agent has answered, the one next step. The exit and
/// relaunch halves are the driver's (each waits on the kernel).
///
/// * At a break of the agent's background work
///   ([`Facts::background_point`]) only a notice is taken: the first, a
///   re-ask after a whole [`REASK_S`] ([`reask`]), or the give-up. A READY a
///   person held past the drain is voided there too. Otherwise: an announced
///   upgrade in its window waits `background`, a stopped round `failed`
///   ([`gate_release`] types its release at a break too), a given-up round's
///   late READY `background` until the drain voids it ([`void_of`]), and any
///   other phase `background`. [`break_step`] is the rule both drivers hold a
///   break to.
/// * The agent's own work bounds no wait but bounds the silence: a notice
///   whose [`REASK_S`] has run out is asked again, also when work under the
///   agent outlives a READY by a whole [`REASK_S`] of the answer's own
///   ([`Facts::ready_s`]); the new notice supersedes that answer. Each re-ask
///   names what runs ([`running_clause`]); past [`MAX_ASKS`] the upgrade gives
///   up. A status that only lags the idle screen is held the same way
///   (`status-stale`).
/// * A taskless conversation ([`Facts::taskless`]) is never announced to: once
///   [`gate_restart`] would let a READY agent go (settle waived) it is
///   restarted afresh ([`Step::Fresh`]).
/// * At a limit ([`Facts::limited`]) or the login wall ([`Facts::login`])
///   every step waits `limited`/`login`: nothing is asked, re-asked, given up,
///   voided or ended, and neither the asks budget nor the re-ask clock
///   ([`clock_held`]) is spent.
/// * A given-up upgrade ([`GAVE_UP`]) still hears a late READY from the
///   process its notice reached: [`gate_restart`] with every gate, then the
///   restart and carry-on, or past the drain's bound the void ([`void_of`]).
///   At a break its answer waits `background` until that bound voids it.
/// * No stop is for good: any other stopped round waits `failed` for
///   [`RETRY_S`] ([`Facts::failed_s`]), then re-arms ([`Step::Rearm`],
///   [`retry_due`]), anywhere, since that types nothing.
/// * A notice that never reached the model ([`Facts::undelivered`]) spent no
///   ask: it is typed again at the first point a notice could be, never given
///   up on ([`announce_asks`]); a give-up spent on such notices is taken back
///   ([`rearmed`]). A notice queued behind a limit reads as
///   [`Facts::limited`] until the limit is over by the transcript
///   ([`queued_until`]), then as undelivered, retyped at once while the queue
///   has room ([`REQUEUE_MAX`]) or else ([`Facts::queued`]) after a rest or on
///   `--now`.
///
/// This is `HarnessUpgradeProgress`'s step at an idle point
/// (`aterm_models_harness::harness_upgrade_progress_model`): announce, restart,
/// give up or re-arm, and what the watch's dry read records; bound over every
/// reachable state in `upgrade_progress_tests.rs`.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeProgress",
        action = "StepAtIdle",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
pub fn next_step(phase: &Phase, f: &Facts, ready: bool, now_s: u64) -> Step {
    if unrecorded_holds(phase, f, ready) {
        return Step::Wait(READY_UNRECORDED);
    }
    if retry_due(phase, f.failed_s, ready) {
        return Step::Rearm;
    }
    // The end of the agent's own work is the upgrade's point
    // ([`Facts::work_ended`]): a round that gave up on a "not yet" is re-armed
    // (and the driver asks in the same visit) once the restart's gate is open
    // but for the READY. Never at a break, a limit or the login wall.
    if f.work_ended
        && !ready
        && !f.background_point
        && matches!(phase, Phase::Failed(why) if why == GAVE_UP)
    {
        return match gate_restart(f, true) {
            Gate::Go => Step::Rearm,
            Gate::Wait(w) => Step::Wait(w),
        };
    }
    if f.taskless && matches!(phase, Phase::Pending | Phase::Announced { .. }) {
        if f.limited {
            return Step::Wait("limited");
        }
        if f.background_point {
            return Step::Wait("background");
        }
        return match gate_restart(f, true) {
            Gate::Go if f.unnoticed => Step::Wait(NOTICE_SHOWN),
            Gate::Go => Step::Fresh,
            Gate::Wait(w) => Step::Wait(w),
        };
    }
    if f.quiet
        && let Some(step) = quiet_step(phase, f, ready, now_s)
    {
        return step;
    }
    let reannounce = || match gate_announce(f) {
        Gate::Go => Step::Announce,
        Gate::Wait(w) => Step::Wait(w),
    };
    if f.background_point && *phase != Phase::Pending {
        let Phase::Announced { at_s, asks } = phase else {
            // A given-up upgrade's late READY at a break: the agent's work
            // outliving the answer voids it here as at an idle point, so an
            // always-at-a-break session is not held on it for good.
            if matches!(phase, Phase::Failed(why) if why == GAVE_UP) && ready {
                if f.limited {
                    return Step::Wait("limited");
                }
                if f.login {
                    return Step::Wait("login");
                }
                let drained = REASK_S.saturating_add(f.ready_s) >= DRAIN_S;
                return void_of(f, "background", drained)
                    .map_or(Step::Wait("background"), Step::Void);
            }
            // A stopped round resting says `failed` at a break too, naming the
            // rest rather than the agent's work.
            if matches!(phase, Phase::Failed(_)) {
                return Step::Wait("failed");
            }
            return Step::Wait("background");
        };
        if f.limited {
            return Step::Wait("limited");
        }
        if f.login {
            return Step::Wait("login");
        }
        let since = now_s.saturating_sub(*at_s);
        // A person's hold voids a READY answer here as at an idle point: the
        // restart a stale answer would authorize comes at the next idle point.
        if ready && let Some(who) = person_void(f, since >= DRAIN_S) {
            return Step::Void(who);
        }
        // A notice the wall answered is no ask the clock may count toward a
        // give-up: typed again here, where a notice may go.
        if !ready && f.undelivered {
            return reannounce();
        }
        // A READY the agent was moved on from is asked again here, where a
        // notice may go, as at an idle point ([`Facts::ready_superseded`]).
        if !ready && f.ready_superseded {
            return reask(f, *asks);
        }
        return if clock(since, f, ready) >= REASK_S {
            reask(f, *asks)
        } else {
            Step::Wait("background")
        };
    }
    match phase {
        Phase::Pending => reannounce(),
        Phase::Announced { at_s, asks } => {
            if f.limited {
                return Step::Wait("limited");
            }
            if f.login {
                return Step::Wait("login");
            }
            let since = now_s.saturating_sub(*at_s);
            if ready {
                return match gate_restart(f, true) {
                    Gate::Go => Step::Terminate,
                    Gate::Wait(w) => match person_void(f, since >= DRAIN_S) {
                        Some(who) => Step::Void(who),
                        // The agent's own work outlives the answer — what
                        // runs under it, or what its status says runs: asked
                        // again, naming what runs, or given up on.
                        None if own_work_holds(w) && clock(since, f, ready) >= REASK_S => {
                            reask(f, *asks)
                        }
                        None => Step::Wait(w),
                    },
                };
            }
            if f.undelivered {
                return reannounce();
            }
            // The agent agreed and was moved on ([`Facts::ready_superseded`]):
            // asked again at once, not after REASK_S.
            if f.ready_superseded {
                return reask_superseded(f, *asks);
            }
            // The agent's "not yet" waited on its own work, which has ended
            // ([`Facts::work_ended`]): asked again at once, exempt from
            // MAX_ASKS, once the restart's gate is open but for the READY; its
            // settle owns the point (`upgrade_drive::owns_turn_ends`).
            //
            // That gate's wait holds the point only inside the re-ask's
            // window; past it the re-ask decides as for any announced round,
            // or a status stuck at `shell`/`busy` over an idle screen would
            // hold the round announced for ever.
            if f.work_ended {
                match gate_restart(f, true) {
                    Gate::Go => return Step::Announce,
                    Gate::Wait(w) if since < REASK_S => return Step::Wait(w),
                    Gate::Wait(_) => {}
                }
            }
            if since < REASK_S {
                return Step::Wait("awaiting-ready");
            }
            reask(f, *asks)
        }
        Phase::Exiting { .. } | Phase::Relaunched { .. } => Step::Wait("in-flight"),
        Phase::Done => Step::Wait("done"),
        Phase::Failed(why) if why == GAVE_UP && ready => {
            if f.limited {
                return Step::Wait("limited");
            }
            if f.login {
                return Step::Wait("login");
            }
            match gate_restart(f, true) {
                Gate::Go => Step::Terminate,
                // The drain clock counts from the latest notice, which a
                // given-up upgrade no longer keeps; it gave up at least
                // REASK_S after that notice and has heard this answer for
                // `ready_s`, so the sum bounds the notice's age from below.
                Gate::Wait(w) => void_of(f, w, REASK_S.saturating_add(f.ready_s) >= DRAIN_S)
                    .map_or(Step::Wait(w), Step::Void),
            }
        }
        Phase::Failed(_) => Step::Wait("failed"),
    }
}

/// The wait of a restart that types nothing first while the owner has not
/// been told yet ([`Facts::unnoticed`]): the waiting record is shown at this
/// look, and the move comes at a later one.
pub const NOTICE_SHOWN: &str = "notice-shown";

/// The neutral word the host's watch records where its dry read, off any
/// point, finds the round's own next step due at a point nothing holds (a
/// notice or restart that passed its gate, a give-up or re-arm with no
/// [`held_point`]): neither the agent's work, a person, a limit nor an aterm
/// failure holds the move; the step goes at the session's next point. Keeps
/// the tab from showing a stale word. Never a visit's word: a visit takes the
/// step, which clears the recorded wait.
pub const NEXT_POINT: &str = "next-point";

/// THE QUIET UPGRADE'S STEP ([`Facts::quiet`], the default),
/// or `None` where the phase is the ask-and-drain flow's to answer: a
/// restart in flight, a finished move, a stopped round (its rest and re-arm,
/// and a late READY a round that gave up still hears, are [`next_step`]'s as
/// ever). For a round that has not moved — pending, or announced by an older
/// aterm (or by this one under `upgrade_ask`), whose agent may still be
/// winding down — the ONE step is the restart, at a point where it is
/// genuinely idle: [`gate_restart`] as a READY agent would pass it — Claude's
/// own `idle`, nothing under the agent, no break of its own background work,
/// the ladder's settle, nobody at the tab, no box, no draft — and never at a
/// usage limit or the login wall (the restart would drop the vendor's own
/// wait, and resume into the wall). Nothing is typed first and nothing is
/// asked; the owner is told before the move ([`Facts::unnoticed`],
/// [`NOTICE_SHOWN`]).
///
/// AN ANNOUNCED ROUND IS STILL BOUNDED: its agent was told
/// to wind down — it may have answered READY and stopped, or "not yet" and
/// stopped taking up long work — and the quiet flow asks it nothing again.
/// Answered here with the restart's wait alone, a round an older aterm
/// announced over work that never ended (a widowed shell, a break that
/// never closed) stood announced for good: no re-ask, no give-up, no void,
/// so no release, and the agent waited on a restart that could not come.
/// So the notice's own bounds still hold, minus the re-ask: a READY a
/// PERSON held past [`DRAIN_S`] is void ([`person_void`]), and once the
/// agent's OWN WORK ([`own_work_holds`]: what runs under it, a break of its
/// work, a status its idle screen does not bear out) has held the restart a
/// whole [`REASK_S`] past the notice — or past the READY, as [`clock`]
/// counts it — the round GIVES UP ([`Step::GiveUp`]) at any ask: the driver
/// owes the agent its release, and types it where a notice may go, a break
/// included. Its rest and the round after it are [`next_step`]'s; that round
/// is pending, and quiet. Inside the window the restart's wait stands, and a
/// pending round — nothing asked of its agent — is never given up on.
///
/// THE OWNER'S `--now` READS A STATUS THAT ONLY LAGS PAST:
/// Claude's own `busy`/`shell` over a screen its reader reads idle look
/// after look ([`status_lags`]) holds the restart (`status-stale`), and
/// under the ask flow the notice goes through the lag — its turn rewrites
/// the status, and the restart follows. The quiet flow types nothing, so
/// nothing rewrites it: the status of an agent that finished, or asked its
/// person something, could hold the move for ever. The restart never kills
/// on that status unasked (work inside the agent's own process is no
/// process under it, and only the status says it runs), but the owner's
/// own word to move it now ([`Facts::owner_now`]) is the consent the ask
/// flow's notice would have sought: under it the lag is read past, at an
/// idle point with nothing under the agent and every other gate kept.
#[must_use]
pub fn quiet_step(phase: &Phase, f: &Facts, ready: bool, now_s: u64) -> Option<Step> {
    let announced = match phase {
        Phase::Pending => None,
        Phase::Announced { at_s, .. } => Some(now_s.saturating_sub(*at_s)),
        _ => return None,
    };
    if f.limited {
        return Some(Step::Wait("limited"));
    }
    if f.login {
        return Some(Step::Wait("login"));
    }
    let wait = if f.background_point {
        "background"
    } else {
        match restart_gate(f, f.owner_now && status_lags(f)) {
            Gate::Go if f.unnoticed => return Some(Step::Wait(NOTICE_SHOWN)),
            Gate::Go => return Some(Step::Terminate),
            Gate::Wait(w) => w,
        }
    };
    if let Some(since) = announced {
        if ready && let Some(who) = person_void(f, since >= DRAIN_S) {
            return Some(Step::Void(who));
        }
        if own_work_holds(wait) && clock(since, f, ready) >= REASK_S {
            return Some(Step::GiveUp);
        }
    }
    Some(Step::Wait(wait))
}

/// The person's void: past the drain's bound (`drained`, [`DRAIN_S`] after
/// the notice), a person's hold ([`person_hold`]) that has stood [`HOLD_S`]
/// over the READY. `None` for no hold, a shorter one, or inside the bound.
/// Both arms of [`next_step`] void on it, so the restart comes at the next
/// idle point after the person lets go.
fn person_void(f: &Facts, drained: bool) -> Option<&'static str> {
    if !drained || f.hold_s < HOLD_S {
        return None;
    }
    person_hold(f)
}

/// Why the READY a given-up upgrade's restart gate still holds (`w`, the
/// gate's wait) is void now, past the drain's bound (`drained`), if it is:
/// the person's hold ([`person_void`]); background work ([`Facts::background`])
/// still running [`DRAIN_S`] after the answer ([`Facts::ready_s`]), since the
/// agent said nothing of its own runs and such work will not end on its own;
/// a break ([`Facts::background_point`]) that long, whatever runs inside the
/// agent's process; or a status still claiming work over an idle screen that
/// long (`status-stale`). Anything else the gate waits on (the settle, the
/// agent's turn, a person's hand, aterm's hold, the limit) passes, and the
/// restart follows.
///
/// Voided, the answer is forgotten and the agent released
/// (`upgrade_drive::drain_expired`). An announced upgrade voids only on the
/// person: its agent's work outliving the answer is met by a superseding
/// re-ask ([`next_step`]), not a release line the next notice would
/// contradict.
fn void_of(f: &Facts, w: &'static str, drained: bool) -> Option<&'static str> {
    person_void(f, drained)
        .or_else(|| (drained && own_work_holds(w) && f.ready_s >= DRAIN_S).then_some(w))
}

/// Since when the READY the upgrade acts on has stood, for
/// [`Facts::ready_s`]: the driver's `prior` stamp (0: none), kept while the
/// answer stands, started at `now_s` when first heard and again at every look
/// that finds the session limited ([`Facts::limited`]). 0 without an answer
/// (`ready` false).
#[must_use]
pub fn ready_since(prior: u64, ready: bool, f: &Facts, now_s: u64) -> u64 {
    if !ready {
        0
    } else if prior == 0 || f.limited || f.login {
        now_s
    } else {
        prior
    }
}

/// Holds the re-ask clock at a limit: the phase a visit records after reading
/// `f` at `now_s`, with an announced upgrade's clock restarted at `now_s`
/// while the session is limited ([`Facts::limited`]). A notice typed before
/// the limit is answered only after the reset, so [`REASK_S`], [`DRAIN_S`]
/// and the path to [`MAX_ASKS`] count from the last limited look. Likewise at
/// the login wall ([`Facts::login`]), where the driver restarts the clock
/// from the lift the transcript records ([`login_lifted_at`]), and behind a
/// full queue ([`Facts::queued`]).
#[must_use]
pub fn clock_held(phase: &Phase, f: &Facts, now_s: u64) -> Phase {
    if f.limited || f.login || f.queued {
        clock_held_until(phase, now_s)
    } else {
        phase.clone()
    }
}

/// [`clock_held`] for a limit known to have stood until `until` (unix
/// seconds): an announced upgrade's clock starts no sooner. Applied by the
/// window's host when its loop's limit episode opens and closes
/// (`upgrade_drive::hold_clock`), since the host takes no step during one.
#[must_use]
pub fn clock_held_until(phase: &Phase, until: u64) -> Phase {
    match phase {
        Phase::Announced { at_s, asks } => Phase::Announced {
            at_s: (*at_s).max(until),
            asks: *asks,
        },
        other => other.clone(),
    }
}

/// Whether a step waiting on `why` leaves the release ([`release_prompt`]) as
/// the upgrade's next act, if owed: only when the upgrade waits on nothing it
/// would type or signal (its re-ask window `awaiting-ready`, a stop `failed`,
/// the owner's hold `skipped`/`deferred`, a build that fails to start
/// [`BUILD_FAILS`]). Any other wait is the notice's or restart's own gate,
/// and what it waits for goes first: a re-ask supersedes the release, and a
/// restart's carry-on tells the agent to go on. Neither comes while the
/// build the round moves to fails to start ([`build_hold`]): left to them,
/// the release a pending round owes (a retarget's, a dropped ride's) was
/// never typed (the review of round eighteen).
#[must_use]
pub fn release_is_next(why: &str) -> bool {
    matches!(
        why,
        "awaiting-ready" | "failed" | "skipped" | "deferred" | BUILD_FAILS
    )
}

/// The wait of a move onto a build that failed to start — for any
/// conversation — and is not proved to start since ([`build_hold`];
/// `upgrade_drive`'s `build_start` keeps what is known of each build).
pub const BUILD_FAILS: &str = "build-fails";

/// Whether the round has asked its agent, which holds for the move: announced
/// (its notice stands, the agent winding down or READY), or given up with a
/// late READY in hand (`ready`, [`GAVE_UP`]: the agent wound down and
/// answered, and is restarted on it). Any other round has asked nothing that
/// still holds the agent: pending, a stopped round's release owed or typed.
#[must_use]
pub fn asked_for_a_move(phase: &Phase, ready: bool) -> bool {
    match phase {
        Phase::Announced { .. } => true,
        Phase::Failed(why) => why == GAVE_UP && ready,
        Phase::Pending | Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
    }
}

/// THE STEP A BUILD THAT FAILS TO START LEAVES (round eighteen), from the
/// reducer's `step` at a round in `phase` with the READY the step was taken
/// on (`ready`). The driver asks it wherever the round's target failed to
/// start, for any conversation, and is not proved to start since: no agent
/// is moved onto that build — a notice asks for a move that cannot come, a
/// restart ends a working agent onto a build that exits at once.
///
/// * A round that ASKED ITS AGENT ([`asked_for_a_move`]) is abandoned,
///   whatever its step: [`Step::Void`] of [`BUILD_FAILS`], which the driver
///   takes as the round's stop — its markers forgotten, so no READY given to
///   them restarts the agent later, and the agent owed its release, typed at
///   once where the notice's gate lets it. Held as a plain wait, the READY of
///   a round that gave up held its release behind a restart that never came
///   (`retry_due` holds the re-arm over it), and an announced round's re-ask
///   never went, so no give-up released its agent either: wound down, or
///   stopped for the restart, for as long as the build failed (the review of
///   round eighteen).
/// * Any other round waits ([`BUILD_FAILS`]) where it would announce or
///   restart; a release it still owes is the next act there
///   ([`release_is_next`]). Every other step is its own.
///
/// `HarnessUpgradeNeverStrands`' `BuildHold` and its release at a pending
/// round (`aterm_models_harness::harness_upgrade_never_strands_model`).
#[must_use]
pub fn build_hold(step: Step, phase: &Phase, ready: bool) -> Step {
    if asked_for_a_move(phase, ready) {
        return Step::Void(BUILD_FAILS);
    }
    match step {
        Step::Announce | Step::Terminate | Step::Fresh => Step::Wait(BUILD_FAILS),
        Step::Wait(_) | Step::Void(_) | Step::GiveUp | Step::Rearm => step,
    }
}

/// May the release line ([`release_prompt`]) be typed now? Never while a READY
/// the phase acts on stands (`ready`: an announced upgrade's, or a late one a
/// given-up upgrade hears): the restart goes first and its carry-on tells the
/// agent to continue. Otherwise under exactly the notice's gate
/// ([`gate_announce`]), a settled background-work break included
/// ([`Facts::background_point`]): the line types one turn and ends nothing.
/// A READY no phase acts on (answering a stopped upgrade) holds nothing.
#[must_use]
pub fn gate_release(f: &Facts, ready: bool) -> Gate {
    if ready {
        return Gate::Wait("ready");
    }
    gate_announce(f)
}

/// How [`release_prompt`] opens: the harness's tag, and words no notice
/// ([`ANNOUNCE_HEAD`]) or continuation (`[aterm harness] Upgraded:`) opens
/// with.
pub const RELEASE_HEAD: &str = "[aterm harness] Upgrade off: ";

/// Whether a prompt's draft is aterm's own notice or release, whole or a
/// start of it: what a pressed notice or release leaves when stopped
/// part-way or when its guarded Enter misses. Nothing takes it back (`upgrade_drive::left_standing` only
/// reads; a person's key may be in flight), and the supervisor never sends it
/// (`supervise::policy::turn_end::Composer::Own`). Compared across wrapping
/// with whitespace dropped: a whole head, or any start of one at least two
/// characters long (a cut can land inside the mark, so `[aterm har` counts);
/// a lone `[` stays a person's. A continuation (`[aterm harness] Upgraded:`)
/// is pasted whole, matches no head, and stays the draft rule's.
#[must_use]
pub fn own_draft(draft: &str) -> bool {
    let compact = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    let draft = compact(draft);
    let draft = draft.trim_start_matches(['\u{276F}', '\u{203A}', '\u{00BB}']);
    draft.chars().nth(1).is_some()
        && [ANNOUNCE_HEAD, RELEASE_HEAD]
            .iter()
            .map(|head| compact(head))
            .any(|head| draft.starts_with(&head) || head.starts_with(draft))
}

/// The release line, typed once into a session the upgrade asked to wind down
/// and then abandoned without restarting (it gave up, voided the READY, was
/// held by `--skip`/`--defer`, or a restart after READY stopped). It tells the
/// agent the restart is not coming, to go on as if the notice had never come,
/// and that nothing restarts it without asking first (a later round,
/// [`Step::Rearm`], asks afresh). It points at no particular work, so it does
/// not pull the agent back from direction given since, and it is typed only to
/// the process the notice reached, never over direction taken up after its
/// last answer (`upgrade_drive::release`, [`directed_since_ready`]). `agent`
/// names the product (`Claude Code`).
///
/// It is said under the flow the next round runs in: `quiet`
/// ([`Facts::quiet`], the default) makes no promise to ask first — the round
/// after it moves the session at a genuinely idle point and asks nothing
/// ([`quiet_step`]), so "nothing will restart this session without asking you
/// again first" would be false there. Both say the notice no longer applies, and to carry on.
#[must_use]
pub fn release_prompt(agent: Agent, quiet: bool) -> String {
    if quiet {
        return format!(
            "{RELEASE_HEAD}The {} upgrade is off for now, and nothing the notice asked of you \
             still applies. Carry on as you would have without it.",
            agent.product()
        );
    }
    format!(
        "{RELEASE_HEAD}The {} upgrade is off for now: nothing will restart this session without \
         asking you again first, and nothing the notice asked of you still applies. Carry on as \
         you would have without it.",
        agent.product()
    )
}

/// What a break of the agent's background work may do
/// ([`Facts::background_point`]): type a notice, void a READY, give up, re-arm
/// a stopped round, wait, and after a give-up, void or stop, type the one
/// release line (`upgrade_drive::release`). Each types at most one line and
/// ends nothing; any other step becomes a wait on that work (`background`).
/// The one rule both drivers (`upgrade_drive`, the Codex lane) apply.
///
/// `HarnessUpgradeProgress`'s step at a settled break
/// (`aterm_models_harness::harness_upgrade_progress_model`): never the restart.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessUpgradeProgress",
        action = "StepAtBreak",
        project = "aterm_agent::harness::upgrade_drive::progress_projection"
    )
)]
pub fn break_step(step: Step) -> Step {
    match step {
        Step::Announce | Step::GiveUp | Step::Void(_) | Step::Wait(_) | Step::Rearm => step,
        Step::Terminate | Step::Fresh => Step::Wait("background"),
    }
}

/// THE RE-ASK CLOCK: seconds since the latest notice. With a READY answer in
/// hand, the clock runs from the later of the notice and the answer
/// ([`Facts::ready_s`], begun again by every notice and held at a limit), so
/// an answer given late still gets a whole [`REASK_S`] before work it
/// outlives supersedes it.
fn clock(since: u64, f: &Facts, ready: bool) -> u64 {
    if ready { since.min(f.ready_s) } else { since }
}

/// THE RE-ASK ([`next_step`]): past [`MAX_ASKS`] notices the upgrade gives
/// up. Before that the notice is typed again wherever [`gate_announce`] lets
/// a notice go. A new notice carries a new READY marker and follows any
/// earlier answer, so an answer the agent's work outlived is superseded,
/// never acted on.
fn reask(f: &Facts, asks: u32) -> Step {
    if asks >= MAX_ASKS {
        return Step::GiveUp;
    }
    match gate_announce(f) {
        Gate::Go => Step::Announce,
        Gate::Wait(w) => Step::Wait(w),
    }
}

/// The wait of a READY the screen shows and the transcript has not recorded
/// yet ([`Facts::ready_unrecorded`]): the point is held (owned like the settle,
/// `upgrade_drive::owns_turn_ends`) and the next look reads the row.
pub const READY_UNRECORDED: &str = "ready-unrecorded";

/// The wait of a re-ask owed to a superseded READY
/// ([`Facts::ready_superseded`]) while the agent's turn settles (`settling`,
/// `busy`, `not-idle`/`status-stale` over `busy`/`shell`): the agent agreed,
/// was moved on, and is asked again once its turn is over. Owned like the
/// settle, so the loop types nothing over the point. Other waits are said as
/// themselves.
pub const SUPERSEDED: &str = "superseded";

/// How long the same last words (the READY on an idle screen) may hold the
/// point for their transcript row ([`unrecorded_holds`]), measured by the
/// repaint-proof settle ([`Facts::still_s`]). Claude Code appends the row
/// within a flush; a transcript not caught up within a minute is not lagging.
pub const UNRECORDED_S: u64 = 60;

/// Whether a READY on screen holds the point ([`READY_UNRECORDED`]): the
/// driver saw it ([`Facts::ready_unrecorded`]) for a phase that acts on a
/// READY (announced under the ask-and-drain flow, or given up and hearing a
/// late one), at an idle point with no limit or wall, and only while those
/// last words have stood less than [`UNRECORDED_S`]; it never waits on the
/// screen for good.
fn unrecorded_holds(phase: &Phase, f: &Facts, ready: bool) -> bool {
    let hears = match phase {
        // The quiet upgrade acts on no READY: its restart waits for the idle
        // point itself ([`quiet_step`]).
        Phase::Announced { .. } => !f.quiet,
        Phase::Failed(why) => why == GAVE_UP,
        Phase::Pending | Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
    };
    hears
        && f.ready_unrecorded
        && !ready
        && !f.background_point
        && !f.limited
        && !f.login
        && f.still_s < UNRECORDED_S
}

/// [`reask`] for a READY the agent was moved on from
/// ([`Facts::ready_superseded`]): a wait on the agent's own turn settling is
/// said as [`SUPERSEDED`]; the act, a give-up and every other wait as they are.
fn reask_superseded(f: &Facts, asks: u32) -> Step {
    let own_turn = matches!(f.status.as_str(), "busy" | "shell");
    match reask(f, asks) {
        Step::Wait("settling" | "busy") => Step::Wait(SUPERSEDED),
        Step::Wait("not-idle" | "status-stale") if own_turn => Step::Wait(SUPERSEDED),
        step => step,
    }
}
// ---------------------------------------------------------------- the owner's word

/// The owner's word on one session's upgrade, written by `aterm harness
/// upgrade <sid> --now|--defer <dur>|--skip` into the session's state under
/// the sweep lock and read by every sweep through [`requested_step`]. Each
/// word only waives the waits that keep a person from being typed over
/// (`Now`) or adds one (`DeferUntil`, `Skip`), with one exception made where
/// the word is written (`upgrade_drive::ask`): `Now` on a given-up upgrade
/// re-arms it at once. A refused or failed upgrade is not re-armed by the
/// word; it rests [`RETRY_S`] and then re-arms itself ([`Step::Rearm`]). The
/// word applies to the named tab, not the conversation wherever it goes.
///
/// A word that holds the upgrade ([`Request::holds`], [`rearm_held`]) makes
/// the step wait `skipped`/`deferred`, so the upgrade owns none of the
/// session's turn ends and the supervisor continues the worker. Every word
/// also arms a new round with a fresh salt (`upgrade_drive::ask`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Request {
    /// Nothing asked: the gates alone decide.
    #[default]
    None,
    /// Move this session at its first pause: the upgrade stands at the last
    /// rung at once ([`Rung::Land`], [`Facts::owner_now`]) for the notice and
    /// the signal, with no settling window and a person holding it only by a
    /// keystroke within [`KEYS_GAP_S`]. Still required: idle status, empty
    /// composer, no box, busy footer, hold or hidden running turn, READY, an
    /// empty process tree and the typed turn's screen fence. On a supervised
    /// session the pause is the next idle point its worker steps at.
    Now,
    /// Not before this unix second; the request lapses on its own after it.
    DeferUntil(u64),
    /// Not onto this version: the session stays where it is until a newer
    /// build is the target.
    Skip(String),
}

impl Request {
    /// The word the state file and `--json` carry: `-`, `now`,
    /// `defer-until:<unix s>`, `skip:<version>`; `--status` and the
    /// `upgrade=` column show [`Self::shown`].
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Self::None => "-".to_string(),
            Self::Now => "now".to_string(),
            Self::DeferUntil(t) => format!("defer-until:{t}"),
            Self::Skip(v) => format!("skip:{v}"),
        }
    }

    /// The word as `--status` and the `upgrade=` column show it at `now_s`:
    /// [`Self::word`], save that a deferral reads as the time it has left
    /// (`defer:5h59m`, `defer:due` once it ran out) rather than its unix
    /// second.
    #[must_use]
    pub fn shown(&self, now_s: u64) -> String {
        match self {
            Self::DeferUntil(t) if *t > now_s => format!("defer:{}", span(t - now_s)),
            Self::DeferUntil(_) => "defer:due".to_string(),
            _ => self.word(),
        }
    }

    /// Whether this word HOLDS the upgrade of `phase` to `target` at `now_s`:
    /// a skip of THAT target, or a deferral not yet run out, while nothing
    /// has begun restarting (a restart in flight is never held). What
    /// [`requested_step`] waits `skipped`/`deferred` on, and what releases
    /// the session's supervisor from the upgrade.
    #[must_use]
    pub fn holds(&self, phase: &Phase, target: &str, now_s: u64) -> bool {
        matches!(phase, Phase::Pending | Phase::Announced { .. }) && self.holds_build(target, now_s)
    }

    /// Whether this word keeps a session off the build `target` at `now_s`,
    /// whatever its round: a skip of THAT build, or a deferral not yet run
    /// out — what a relaunch of the conversation onto a newer build, or a
    /// record that outlived the round the word was given in, keeps to.
    #[must_use]
    pub fn holds_build(&self, target: &str, now_s: u64) -> bool {
        match self {
            Self::Skip(v) => Version::parse(v).is_some_and(|v| Some(v) == Version::parse(target)),
            Self::DeferUntil(t) => now_s < *t,
            Self::None | Self::Now => false,
        }
    }

    /// [`Self::word`] read back; an unknown word is `None` (the caller keeps
    /// [`Request::None`]: an unreadable request asks for nothing).
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "" | "-" => Some(Self::None),
            "now" => Some(Self::Now),
            _ => {
                if let Some(t) = word.strip_prefix("defer-until:") {
                    return t.parse().ok().map(Self::DeferUntil);
                }
                let v = word.strip_prefix("skip:")?;
                Version::parse(v).map(|v| Self::Skip(v.to_string()))
            }
        }
    }
}

/// [`next_step`] under the owner's [`Request`] for the upgrade to `target`.
/// A skip of this target or an unexpired deferral holds a session not yet
/// restarting (`skipped`, `deferred`, [`Request::holds`]), and a stopped
/// round's new one ([`rearm_held`]); `Now` stands the upgrade at the last
/// rung ([`Rung::Land`]) and nothing else. A restart in flight is never held:
/// stopping half way would strand the conversation.
///
/// `Now` is applied as [`Facts::owner_now`], never by aging the facts (which
/// would read a just-ended turn as settled); a keystroke within
/// [`KEYS_GAP_S`] still holds it, as at every rung.
#[must_use]
pub fn requested_step(
    request: &Request,
    phase: &Phase,
    f: &Facts,
    ready: bool,
    now_s: u64,
    target: &str,
) -> Step {
    if request.holds(phase, target, now_s) {
        return Step::Wait(held_word(request));
    }
    let step = if *request == Request::Now {
        let now = Facts {
            owner_now: true,
            ..f.clone()
        };
        next_step(phase, &now, ready, now_s)
    } else {
        next_step(phase, f, ready, now_s)
    };
    rearm_held(request, step, target, now_s)
}

/// The wait the owner's holding word says: `skipped` or `deferred`.
fn held_word(request: &Request) -> &'static str {
    if matches!(request, Request::Skip(_)) {
        "skipped"
    } else {
        "deferred"
    }
}

/// The owner's word holds a new round as it holds any ([`Step::Rearm`],
/// [`Request::holds`]): a stopped round skipped for this target or under an
/// unexpired `--defer` is not re-armed and waits `skipped`/`deferred`.
/// [`requested_step`] ends here.
#[must_use]
pub fn rearm_held(request: &Request, step: Step, target: &str, now_s: u64) -> Step {
    if step == Step::Rearm && request.holds(&Phase::Pending, target, now_s) {
        Step::Wait(held_word(request))
    } else {
        step
    }
}

/// What holds the point of a read that would act. The reducer takes two
/// steps whatever the session shows, the give-up ([`Step::GiveUp`]) and the
/// re-arm ([`Step::Rearm`]), so a read off any point (the host's watch, a dry
/// run) may say it would act at a running turn, a box or a limit wall. This
/// returns what holds the point the step waits for, as a gate wait word
/// (`not-idle`, `box`, `limited`, …) for facts `f`, or `None` when nothing
/// holds it and for every other step:
///
/// * the give-up: the notice's gate at that read ([`gate_announce`]) — the
///   point the release it owes is typed at ([`gate_release`]), in the
///   ask-and-drain flow and in the quiet one alike (which gives up only on a
///   round an older aterm, or `upgrade_ask`, announced: [`quiet_step`]);
/// * the re-arm: the NEW ROUND'S OWN STEP at that read — [`requested_step`]
///   of a pending round under `request` (the owner's spent `--now` gone, as
///   [`Step::Rearm`] leaves it), quiet or asking as `f` says — which is what
///   the round the host's watch re-arms waits on the moment it exists.
///
/// Read by `upgrade_drive`'s dry run (`gave-up@box`) and by the watch, which
/// records it ([`NEXT_POINT`] when `None`). Bound over every reachable state
/// of `HarnessUpgradeProgress` in the progress tests.
#[must_use]
pub fn held_point(
    request: &Request,
    step: &Step,
    f: &Facts,
    now_s: u64,
    target: &str,
) -> Option<&'static str> {
    match step {
        Step::GiveUp => match gate_announce(f) {
            Gate::Go => None,
            Gate::Wait(w) => Some(w),
        },
        Step::Rearm => {
            let request = if *request == Request::Now {
                Request::None
            } else {
                request.clone()
            };
            match requested_step(&request, &Phase::Pending, f, false, now_s, target) {
                Step::Wait(w) | Step::Void(w) => Some(w),
                Step::Announce | Step::Terminate | Step::Fresh | Step::GiveUp | Step::Rearm => None,
            }
        }
        Step::Announce | Step::Wait(_) | Step::Terminate | Step::Void(_) | Step::Fresh => None,
    }
}

/// The wait a sweep records for the owner: the gate's word, with Claude's
/// status beside `not-idle` (`not-idle:busy`, `:shell`, `:waiting`) to tell a
/// running turn, a background shell and a question for a person apart. The
/// status is a third party's word, so it is cut to `[a-z-]`, 16 bytes. The
/// same for `status-stale` (`status-stale:busy`), a status the idle screen
/// does not bear out ([`gate_announce`]).
#[must_use]
pub fn wait_word(why: &str, status: &str) -> String {
    if why != "not-idle" && why != "status-stale" {
        return why.to_string();
    }
    let status: String = status
        .chars()
        .filter(|c| c.is_ascii_lowercase() || *c == '-')
        .take(16)
        .collect();
    if status.is_empty() {
        why.to_string()
    } else {
        format!("{why}:{status}")
    }
}

/// A span of seconds as the owner reads it: `45s`, `12m`, `8h22m`, `3d4h` —
/// the two largest units, the smaller dropped when it is zero. One spelling
/// for `--status`, the dry run's `pending_for=`/`wait_for=` and the window's
/// `upgrade=` column.
#[must_use]
pub fn span(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h{m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d{h}h"),
    }
}

#[cfg(test)]
#[path = "upgrade_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "steps_prompt_agreement_tests.rs"]
mod steps_prompt_agreement_tests;

#[path = "upgrade_readiness.rs"]
mod readiness;
