// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worker session task records (`task`, `task mark`, `await task`, `result`).

use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use aterm_agent::supervise::occupancy::{FactWord, NoteOutcome};

use crate::SessionCtx;
use crate::control::{Principal, Scope};
use crate::turn_ledger::TurnBy;

/// State of a worker session's task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskState {
    /// Task is active; nothing to read yet.
    Working,
    /// Worker landed its work.
    Done,
    /// Agent exited with no relaunch coming.
    Exited,
    /// Worker stalled before showing its composer or is held without progress.
    Stalled,
    /// The work ended unfinished while its agent may still stand in the tab: it said it could not
    /// finish, a person or a manager took Autopilot Off, its budget ran out or its replies repeated
    /// with no work ([`WHY_COULD_NOT_FINISH`], [`WHY_OFF`], [`WHY_OFF_CTL`], [`WHY_EXPIRED`],
    /// [`WHY_REPEATED`]).
    Failed,
}

impl TaskState {
    /// All states in usage-line order.
    pub(crate) const ALL: [Self; 5] = [
        Self::Working,
        Self::Done,
        Self::Exited,
        Self::Stalled,
        Self::Failed,
    ];

    /// Wire word for this state.
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Done => "done",
            Self::Exited => "exited",
            Self::Stalled => "stalled",
            Self::Failed => "failed",
        }
    }

    /// Parses a wire word into a [`TaskState`].
    #[must_use]
    pub(crate) fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == word)
    }

    /// Whether a waiter has a terminal state to read (every state except `working`).
    #[must_use]
    pub(crate) const fn is_terminal(self) -> bool {
        !matches!(self, Self::Working)
    }

    /// HOW A TASK IN THIS STATE ENDED: `done` finished, `failed` failed, `exited` finished only if
    /// its task had (`finished`, [`TaskRecord::finished`]: that end outlives the exit,
    /// [`TaskMark::apply`]) and failed otherwise, `stalled` failed for as long as it stands (a stall
    /// that moves on goes back to `working` and takes this back), and `working` has none. It is
    /// `Some` exactly where [`Self::is_terminal`] holds, so what wakes a waiter is what can be read.
    #[must_use]
    pub(crate) const fn conclusion(self, finished: bool) -> Option<Conclusion> {
        match self {
            Self::Working => None,
            Self::Done => Some(Conclusion::Finished),
            Self::Exited if finished => Some(Conclusion::Finished),
            Self::Exited | Self::Stalled | Self::Failed => Some(Conclusion::Failed),
        }
    }

    /// Whether a task in this state has an end for a person to read once: every conclusion but a
    /// stall's, which keeps its need. What a look marks seen, where [`Self::marks_its_tab`] says
    /// which of them also leave the tab its `•`.
    #[must_use]
    pub(crate) const fn has_an_end_to_read(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Exited)
    }

    /// Whether a task in this state leaves its tab a `•` until someone looks: it finished, or it
    /// failed with nobody to say so but the record. A stall has a need's own mark, and an exit
    /// has its tab's.
    #[must_use]
    pub(crate) const fn marks_its_tab(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }
}

/// How a worker's task ended, whichever way it got there ([`TaskRecord::conclusion`]): the one
/// question a manager, a person or a surface asks of a record that stopped moving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Conclusion {
    /// The work finished: landed, or marked done.
    Finished,
    /// The work did not finish, and `why=` says how it ended.
    Failed,
}

impl Conclusion {
    /// The `conclusion=` word.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Failed => "failed",
        }
    }
}

/// `why=` value written when Autopilot Land lands the work.
pub(crate) const WHY_LANDED: &str = "landed";

/// `why=` of a worker whose reply said `Could not finish: <why>`
/// (`aterm_agent::supervise::COULD_NOT_FINISH_WHY`).
pub(crate) const WHY_COULD_NOT_FINISH: &str = aterm_agent::supervise::COULD_NOT_FINISH_WHY;

/// `why=` of a worker a person took off Autopilot before it landed: nothing will carry it on.
pub(crate) const WHY_OFF: &str = "off";

/// `why=` of a worker a manager or a script, no person, took off Autopilot before it landed.
pub(crate) const WHY_OFF_CTL: &str = "off-ctl";

/// `why=` of a worker its time budget ran out on (the disk worker's alone today).
pub(crate) const WHY_EXPIRED: &str = "expired";

/// `why=` of a worker Land stopped typing to because the same reply came back with no work
/// (`aterm_agent::supervise::REPEATED_WHY`).
pub(crate) const WHY_REPEATED: &str = aterm_agent::supervise::REPEATED_WHY;

/// `why=` of a usage-limit failure recorded by an earlier harness. The successor reopens it
/// and waits for quota.
pub(crate) const WHY_USAGE_LIMIT: &str = aterm_agent::supervise::USAGE_LIMIT_WHY;

/// `why=` of an agent that left its tab with nothing to bring it back.
pub(crate) const WHY_EXIT: &str = "exit";

/// `why=` of a program worker's conclusion (`spawn program=`, `crate::program_worker`): its
/// program exited, `done` on status 0 and `failed` on any other, the record's `exit=` naming it.
pub(crate) const WHY_EXIT_STATUS: &str = "exit-status";

/// `why=` of a program worker's failed conclusion where no exit status of its program was read: the
/// next prompt sealed its block with none, job control hid how it ended, or a restore brought it
/// back with its shell gone. Its record carries no `exit=`.
pub(crate) const WHY_NO_EXIT_STATUS: &str = "no-exit-status";

/// `why=` of a worker its launcher refused because its CLI is signed out, which only a person
/// can fix (`task_launch::refused_why`).
pub(crate) const WHY_SIGNIN: &str = "signin";

/// `why=` of a worker its launcher refused because atpkg's install of its absent program failed
/// (`task_launch::refused_why`).
pub(crate) const WHY_INSTALL: &str = "install";

/// `why=` of a running worker's record after a restore other than a seamless update
/// (`harness_host::credit_claim::note_restored_worker`, [`seed_restored`]): its worker started
/// before the restore, so no startup or held deadline reads it as one just spawned.
pub(crate) const WHY_RESTORED: &str = "restored";

/// Closed `why=` vocabulary for task state transitions.
pub(crate) const TASK_WHY: &[&str] = &[
    WHY_LANDED,
    "marker",
    "done-check",
    WHY_EXIT,
    WHY_EXIT_STATUS,
    WHY_NO_EXIT_STATUS,
    "shell",
    "startup",
    "held",
    "turn",
    "moved",
    WHY_SIGNIN,
    WHY_INSTALL,
    WHY_RESTORED,
    WHY_COULD_NOT_FINISH,
    WHY_OFF,
    WHY_OFF_CTL,
    WHY_EXPIRED,
    WHY_REPEATED,
    WHY_USAGE_LIMIT,
];

/// A failed conclusion's words for a person, by its `why=` (the Workers row, Done for you and the
/// fact the menu bar keeps), a stalled record's by [`stall_words`].
#[must_use]
pub(crate) fn failed_words(state: TaskState, why: Option<&str>) -> String {
    match (state, why) {
        (TaskState::Stalled, why) => stall_words(why),
        // A task that ended where it stalled ends for the stall's reason.
        (_, Some(why)) if is_stall_why(why) => stall_words(Some(why)),
        (_, Some(WHY_COULD_NOT_FINISH)) => "it said it could not finish".to_string(),
        (_, Some(WHY_OFF)) => "a person turned Autopilot off".to_string(),
        (_, Some(WHY_OFF_CTL)) => "a manager or a script turned Autopilot off".to_string(),
        (_, Some(WHY_EXPIRED)) => "its time ran out".to_string(),
        (_, Some(WHY_REPEATED)) => "it kept repeating the same reply".to_string(),
        (_, Some(WHY_USAGE_LIMIT)) => {
            "its account hit a usage limit that resets too far off to wait for".to_string()
        }
        (_, Some(WHY_EXIT)) | (TaskState::Exited, None) => {
            "its agent exited before landing".to_string()
        }
        (_, Some(WHY_EXIT_STATUS)) => "its program exited with a failure status".to_string(),
        (_, Some(WHY_NO_EXIT_STATUS)) => "no exit status of its program was read".to_string(),
        (_, Some(why)) => format!("ended unfinished ({why})"),
        (_, None) => "ended unfinished".to_string(),
    }
}

/// Whether `why` is a stall's own reason ([`stall_words`] words each).
fn is_stall_why(why: &str) -> bool {
    matches!(why, "held" | "startup" | "shell") || why == WHY_SIGNIN || why == WHY_INSTALL
}

/// A stalled task's words for a person, by its `why=` (the need's `what` and the menu bar's
/// row). A worker refused as signed out says that a person must sign in, the one step nobody
/// else can take.
#[must_use]
pub(crate) fn stall_words(why: Option<&str>) -> String {
    match why {
        Some("held") => "held at a screen aterm does not know".to_string(),
        Some("startup" | "shell") => "never started".to_string(),
        Some(WHY_SIGNIN) => "never started: signed out, a person must sign in".to_string(),
        Some(WHY_INSTALL) => "never started: its program could not be installed".to_string(),
        Some(why) => format!("stalled ({why})"),
        None => "stalled".to_string(),
    }
}

/// Whether a worker trusted to folder `trusted_cwd` has its own report of how it ended, so no
/// generic one goes beside it (no landing record, no failed record or fact, no Done for you row):
/// a credit claim's worker (its claim-worker record in Messages,
/// `App::note_worker_conclusion`) and aterm's disk worker (its disk-worker record, from the disk
/// watch), each an automatic report and never an item to look at.
#[must_use]
pub(crate) fn reports_its_own_end(trusted_cwd: Option<&str>) -> bool {
    trusted_cwd.is_some_and(|folder| {
        let folder = Path::new(folder);
        aterm_agent::harness::credit_offers::is_claim_folder(folder)
            || aterm_agent::harness::disk::fence::is_worker_folder(folder)
    })
}

/// Default `from=` value when a task is started by `ctl` without a caller session ID.
pub(crate) const FROM_CTL: &str = "ctl";

/// Whether `from` is `ctl` or a valid global session ID (`s-<hex>`).
#[must_use]
pub(crate) fn valid_from(from: &str) -> bool {
    from == FROM_CTL
        || (from.len() <= 64
            && from
                .strip_prefix("s-")
                .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())))
}

/// Directory where task result files live (`<state>/tasks/`).
#[must_use]
pub(crate) fn tasks_dir() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|state| tasks_dir_under(&state))
}

/// [`tasks_dir`] under `state`.
#[must_use]
pub(crate) fn tasks_dir_under(state: &Path) -> PathBuf {
    state.join("tasks")
}

/// The wall clock in unix milliseconds (`0` before the epoch).
fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Per-session task record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskRecord {
    /// Monotonic revision incremented on record changes and when [`Self::attention`] moves.
    pub(crate) rev: u64,
    /// Current task state.
    pub(crate) state: TaskState,
    /// Origin of the task (a global session ID or [`FROM_CTL`]).
    pub(crate) from: String,
    /// THE MAC [`Self::from`] RUNS ON, where that is another Mac: the node of the fleet drive the
    /// worker was spawned over (`aterm ctl --host <h> spawn`, the driver its node key proved), so
    /// its mail reaches that manager there (`crate::worker_mail`); `None` for a spawn on this Mac,
    /// whose manager is a session here, and once a mark names another `from` ([`TaskMark::apply`]).
    /// A carry row holds it (`manager_node=`).
    pub(crate) manager_node: Option<String>,
    /// What the worker's mail has told its manager of this task ([`crate::worker_mail::Told`]):
    /// each need and the conclusion a send finished with. A carry row holds what was told
    /// (`told=`, `told_end=1`), never what a send still has in hand, so a successor sends that
    /// again.
    pub(crate) told: crate::worker_mail::Told,
    /// Reason the task entered its current state ([`TASK_WHY`]).
    pub(crate) why: Option<&'static str>,
    /// What aterm checked at the landing that marked it done (`checked=`,
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9, slice 9c: `3f9a1c2e4b7d@origin/main`,
    /// `no-remote`, `no-repo`, …; [`checked_word`]); none where the mark said none (a landing with
    /// no receipt, codex-alab's own, a hand's mark).
    pub(crate) checked: Option<String>,
    /// What the landing that marked it done read of the processes its agent started that still
    /// ran (`left=`, `docs/DESIGN-leftovers-by-default-2026-10-30.md` §2: `0`, `kept:<n>`, `<n>`
    /// or `unchecked`; [`left_word`]); none where the mark said none (a landing whose host read
    /// none, codex-alab's own, a hand's mark).
    pub(crate) left: Option<String>,
    /// Agent exit code for an `exited` task, if known.
    pub(crate) exit: Option<i32>,
    /// An `exited` task that had finished before its agent left, whatever word finished it: an exit
    /// never rewrites that ([`TaskMark::apply`], [`TaskState::conclusion`]). Worked out from nothing
    /// else, so a carry row holds it (`finished=1`).
    pub(crate) finished: bool,
    /// Size in bytes of the worker's result file (`0` when absent).
    pub(crate) result_bytes: u64,
    /// Path to the result file under [`tasks_dir`], if any.
    pub(crate) result_path: Option<PathBuf>,
    /// Whether a supervisor's, a `NEEDS YOU:` or codex-alab's own ask stands on the session
    /// ([`supervisor_attention`]); a `kind=wait` entry is none.
    pub(crate) attention: bool,
    /// Wall-clock timestamp (unix ms) when the current state was entered.
    pub(crate) since_unix_ms: u64,
    /// Monotonic instant when the current state was entered.
    pub(crate) since: Instant,
    /// Wall-clock timestamp (unix ms) when the task began, which no later state moves.
    pub(crate) started_unix_ms: u64,
    /// Monotonic instant when the task began.
    pub(crate) started: Instant,
    /// Revision at which the current state was entered.
    pub(crate) entered: u64,
    /// The `entered` this record arrived with from an earlier process ([`seed_carried`]), never
    /// carried on: a conclusion entered before it was told there.
    pub(crate) carried_entered: Option<u64>,
    /// Agent program started by `spawn agent=<word>`, if spawned as a worker.
    pub(crate) agent: Option<aterm_phase::Program>,
    /// Canonical `cwd` trusted by the starter when spawning the worker.
    pub(crate) trusted_cwd: Option<String>,
    /// A PROGRAM WORKER'S BLOCK ([`Self::runs_a_program`], `crate::program_worker`): the id of the
    /// shell block its launcher typed its program into ([`note_program_block`]), the first of the
    /// tab's blocks its program's end is read from; none before its line was typed, and none once
    /// its task concluded ([`TaskMark::apply`]), so nothing that reopens it later is concluded
    /// again by that end. A carry row holds it (`block=`).
    pub(crate) program_block: Option<u64>,
    /// The fact word the harness loop noted for a managed worker under the owner's `prompt`
    /// (`fact=`, [`mark_session_task_fact_if`]): the line it would have typed but for the
    /// manager. A mark never touches it.
    pub(crate) fact: Option<FactWord>,
    /// The fact word's own revision (`fact_rev=`): moved by every edit of [`Self::fact`], never by
    /// a mark, and never moving [`Self::rev`], so no `await task` past a rev wakes on it.
    pub(crate) fact_rev: u64,
}

impl TaskRecord {
    /// Milliseconds elapsed since the current state was entered (`since_ms=`).
    #[must_use]
    pub(crate) fn since_ms(&self) -> u64 {
        u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// HOW THE TASK ENDED, the one reading of a record's outcome ([`TaskState::conclusion`]).
    #[must_use]
    pub(crate) fn conclusion(&self) -> Option<Conclusion> {
        self.state.conclusion(self.finished)
    }

    /// Whether the record is a program worker's (`spawn program=`): no harness reads its tab, and
    /// its program's exit code concludes it (`crate::program_worker`).
    #[must_use]
    pub(crate) fn runs_a_program(&self) -> bool {
        self.agent == Some(aterm_phase::Program::Generic)
    }

    /// Whether a waiter has news (a terminal state or a standing ask, [`Self::attention`]).
    #[must_use]
    pub(crate) const fn has_news(&self) -> bool {
        self.state.is_terminal() || self.attention
    }

    /// Formats the record's wire reply fields.
    #[must_use]
    pub(crate) fn wire_fields(&self) -> String {
        let mut out = format!(
            "rev={} state={} from={} since_ms={} result_bytes={}",
            self.rev,
            self.state.as_str(),
            self.from,
            self.since_ms(),
            self.result_bytes
        );
        self.push_conclusion(&mut out);
        self.push_optional(&mut out);
        self.push_fact(&mut out);
        out
    }

    /// ` fact=<word|-> fact_rev=<n>` once the fact word was ever edited (`fact_rev` past 0); a
    /// record the harness never noted carries neither field.
    pub(crate) fn push_fact(&self, out: &mut String) {
        if self.fact_rev == 0 {
            return;
        }
        let word = self.fact.map_or_else(|| "-".to_string(), FactWord::word);
        out.push_str(&format!(" fact={word} fact_rev={}", self.fact_rev));
    }

    /// ` conclusion=<word>` once the task has ended ([`Self::conclusion`]). It is worked out from
    /// the state and [`Self::finished`], so a carry row does not hold it.
    fn push_conclusion(&self, out: &mut String) {
        if let Some(conclusion) = self.conclusion() {
            out.push_str(" conclusion=");
            out.push_str(conclusion.word());
        }
    }

    /// ` why= checked= left= exit= result_path=`, each only when set.
    fn push_optional(&self, out: &mut String) {
        if let Some(why) = self.why {
            out.push_str(" why=");
            out.push_str(why);
        }
        if let Some(checked) = &self.checked {
            out.push_str(" checked=");
            out.push_str(checked);
        }
        if let Some(left) = &self.left {
            out.push_str(" left=");
            out.push_str(left);
        }
        if let Some(exit) = self.exit {
            out.push_str(&format!(" exit={exit}"));
        }
        if let Some(path) = &self.result_path {
            out.push_str(" result_path=");
            out.push_str(&crate::control::pct_encode(&path.to_string_lossy()));
        }
    }

    /// Formats the record for seamless-update handoff
    /// ([`crate::session_store::SessionRecord::task`]) at `now`: the wall-clock `since=` and
    /// `started=` with how long each has stood on this process's monotonic clock (`age_ms=`,
    /// `ran_ms=`), which a Mac asleep does not advance, so the successor's clocks go on from the
    /// time this one measured ([`Self::from_carry_row`]).
    #[must_use]
    pub(crate) fn carry_row(&self, now: Instant) -> String {
        let mut out = format!(
            "rev={} state={} from={} since={} result_bytes={} attention={}",
            self.rev,
            self.state.as_str(),
            self.from,
            self.since_unix_ms,
            self.result_bytes,
            u8::from(self.attention)
        );
        self.push_optional(&mut out);
        if self.finished {
            out.push_str(" finished=1");
        }
        let ms = |stood: std::time::Duration| u64::try_from(stood.as_millis()).unwrap_or(u64::MAX);
        if self.started_unix_ms != self.since_unix_ms {
            out.push_str(&format!(" started={}", self.started_unix_ms));
        }
        out.push_str(&format!(
            " age_ms={}",
            ms(now.saturating_duration_since(self.since))
        ));
        if self.started_unix_ms != self.since_unix_ms || self.started != self.since {
            out.push_str(&format!(
                " ran_ms={}",
                ms(now.saturating_duration_since(self.started))
            ));
        }
        if let Some(node) = &self.manager_node {
            out.push_str(" manager_node=");
            out.push_str(node);
        }
        self.told.push_carry(self.entered, &mut out);
        if let Some(agent) = self.agent {
            out.push_str(" agent=");
            out.push_str(agent.name());
        }
        if let Some(folder) = &self.trusted_cwd {
            out.push_str(" trusted_cwd=");
            out.push_str(&crate::control::pct_encode(folder));
        }
        if let Some(block) = self.program_block {
            out.push_str(&format!(" block={block}"));
        }
        if self.fact_rev > 0 {
            if let Some(word) = self.fact {
                out.push_str(" fact=");
                out.push_str(&word.word());
            }
            out.push_str(&format!(" fact_rev={}", self.fact_rev));
        }
        out
    }

    /// The words of carry row `row` ([`Self::carry_row`]) that a seamless update's claims fence
    /// compares between the park and the Commit ([`crate::session_store::CarriedClaims`]): all
    /// but its ages (`age_ms=`, `ran_ms=`), which grow while the record stands still. A mark
    /// still moves the face, by its `rev=` at least.
    #[must_use]
    pub(crate) fn carry_face(row: &str) -> String {
        row.split_whitespace()
            .filter(|word| {
                !word
                    .split_once('=')
                    .is_some_and(|(key, _)| key == "age_ms" || key == "ran_ms")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Restores and validates a [`TaskRecord`] from a seamless-update carry row.
    ///
    /// Its `since` and `started` stand as long before `now` as the predecessor measured
    /// (`age_ms=`, `ran_ms=`), never longer than the wall clock's gap since `since=` and
    /// `started=` (which a row without them, from an older build, goes by), so time the Mac slept
    /// before the update counts no more here than it did there.
    ///
    /// A `fact_rev=` absent or not a number reads 0; a `fact=` that is no [`FactWord`], or has no
    /// rev past 0, reads `None` and keeps the record. The successor's loop finds the carried word
    /// standing, as after a plain restart, so noting the same episode again wakes no `facts=1`
    /// waiter twice; the loop clears a carried word no open hold stands for (aterm-agent's
    /// `Session::door_task_seed`).
    #[must_use]
    pub(crate) fn from_carry_row(
        row: &str,
        tasks: Option<&Path>,
        now: Instant,
        now_unix_ms: u64,
    ) -> Option<Self> {
        let mut fields: Vec<(&str, &str)> = Vec::new();
        for (key, value) in row.split_whitespace().filter_map(|t| t.split_once('=')) {
            if fields.iter().any(|(k, _)| *k == key) {
                return None;
            }
            fields.push((key, value));
        }
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let parse_u64 = |s: &str| digits(s).then(|| s.parse::<u64>().ok()).flatten();
        let field = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let since_unix_ms = parse_u64(field("since")?)?;
        let wall_age = std::time::Duration::from_millis(now_unix_ms.saturating_sub(since_unix_ms));
        let age = match field("age_ms") {
            Some(word) => std::time::Duration::from_millis(parse_u64(word)?).min(wall_age),
            None => wall_age,
        };
        let started_unix_ms = match field("started") {
            Some(word) => parse_u64(word)?,
            None => since_unix_ms,
        };
        let wall_ran =
            std::time::Duration::from_millis(now_unix_ms.saturating_sub(started_unix_ms));
        let ran = match (field("ran_ms"), field("started")) {
            (Some(word), _) => std::time::Duration::from_millis(parse_u64(word)?).min(wall_ran),
            (None, None) => age,
            (None, Some(_)) => wall_ran,
        };
        let rev = parse_u64(field("rev")?).filter(|rev| *rev > 0)?;
        let fact_rev = field("fact_rev").and_then(parse_u64).unwrap_or(0);
        Some(Self {
            rev,
            state: TaskState::parse(field("state")?)?,
            from: Some(field("from")?).filter(|f| valid_from(f))?.to_string(),
            manager_node: field("manager_node")
                .filter(|node| crate::fabric::fleet_mail::valid_node(node))
                .map(str::to_owned),
            told: crate::worker_mail::Told::of_carry(field("told"), field("told_end"), rev),
            why: match field("why") {
                Some(word) => Some(why_word(word)?),
                None => None,
            },
            checked: match field("checked") {
                Some(word) => Some(checked_word(word)?),
                None => None,
            },
            left: match field("left") {
                Some(word) => Some(left_word(word)?),
                None => None,
            },
            exit: match field("exit") {
                Some(code) => Some(parse_exit_i32(code)?),
                None => None,
            },
            finished: field("finished") == Some("1"),
            result_bytes: parse_u64(field("result_bytes")?)?,
            result_path: match field("result_path") {
                Some(path) => Some(result_path(path, tasks)?),
                None => None,
            },
            attention: match field("attention")? {
                "0" => false,
                "1" => true,
                _ => return None,
            },
            since_unix_ms,
            since: now.checked_sub(age).unwrap_or(now),
            started_unix_ms,
            started: now.checked_sub(ran).unwrap_or(now),
            entered: rev,
            carried_entered: None,
            agent: field("agent").and_then(worker_agent),
            trusted_cwd: field("trusted_cwd").and_then(trusted_folder),
            program_block: match field("block") {
                Some(block) => Some(parse_u64(block)?),
                None => None,
            },
            fact: field("fact")
                .filter(|_| fact_rev > 0)
                .and_then(FactWord::parse),
            fact_rev,
        })
    }
}

/// The folder a carried `trusted_cwd=` names: pct-decoded, absolute, spelled
/// as its own parts ([`spelled_as_its_parts`]: no `.`, `..`, doubled or
/// trailing separator), no control character, at most [`RESULT_PATH_MAX`]
/// bytes — the canonical form the spawn stored.
fn trusted_folder(value: &str) -> Option<String> {
    let decoded = aterm_codec::pct::decode(value);
    let path = Path::new(&decoded);
    (decoded.len() <= RESULT_PATH_MAX
        && !decoded.chars().any(char::is_control)
        && path.has_root()
        && path
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
        && spelled_as_its_parts(path))
    .then(|| decoded.clone())
}

/// The agent a worker's `agent=` word names: one a spawn can start
/// (`aterm_agent::harness::worker::worker_cli`), or [`aterm_phase::Program::Generic`], a program
/// worker's ([`TaskRecord::runs_a_program`]); else none.
fn worker_agent(word: &str) -> Option<aterm_phase::Program> {
    aterm_phase::Program::ALL
        .into_iter()
        .find(|p| p.name() == word)
        .filter(|p| {
            *p == aterm_phase::Program::Generic
                || aterm_agent::harness::worker::worker_cli(*p).is_some()
        })
}

/// THE `checked=` WORD `value` IS (§14.9, slice 9c), as a landing's receipt writes it
/// (`aterm_agent::harness::acts::LandReceipt::checked_word`): `<12 lowercase hex>@<ref>` where
/// `HEAD` was found on a remote main (`3f9a1c2e4b7d@origin/main`, the ref one printable ASCII
/// token), else a leg's own word (`pushed`, `no-remote`, `no-repo`, `gone`); `None` for any other.
pub(crate) fn checked_word(value: &str) -> Option<String> {
    aterm_agent::harness::land_receipt::valid_checked_word(value).then(|| value.to_owned())
}

/// THE `left=` WORD `value` IS, as a landing's done line writes it
/// (`aterm_agent::harness::leftover::LeftWord::word`): `0`, `kept:<n>`, `<n>` or `unchecked`;
/// `None` for any other.
pub(crate) fn left_word(value: &str) -> Option<String> {
    aterm_agent::harness::leftover::LeftWord::of_word(value).map(|_| value.to_owned())
}

/// THE REFUSAL OF A HAND MARK'S `left=`: as for `checked=` ([`CHECKED_BY_HAND`]), the word says
/// what aterm's own loop read at a landing, so only that loop writes it.
pub(crate) const LEFT_BY_HAND: &str = "ERR task mark: left= is what aterm's own landing read of the \
     processes its agent left running; a hand mark cannot set it\n";

/// THE REFUSAL OF A HAND MARK'S `checked=` (the post-landing review, 2026-11-01): the word says
/// what aterm itself checked at a landing, so only aterm's own loop writes it (its connection's
/// [`Principal::Harness`]); a mark any other owner-socket caller makes names none.
pub(crate) const CHECKED_BY_HAND: &str =
    concat!("ERR task mark: ", aterm_types::checked_by_hand!(), "\n");

/// THE REFUSAL OF A HAND'S `task mark failed` WHILE A LOOP CARRIES THE SESSION: `failed` means
/// nothing more is typed, and the mark would not reach the loop that types. Autopilot Off stops it,
/// and that fails the task itself.
pub(crate) const FAILED_UNDER_A_LOOP: &str = "ERR task mark: Autopilot carries this session, so a \
     mark of failed would not stop its typing: set its Autopilot off, which fails the task\n";

/// THE REFUSAL OF A HARNESS'S MARK ON A PROGRAM WORKER'S TASK (`spawn program=`,
/// `crate::program_worker`): its program's exit status concludes it, so an agent's harness reading
/// a CLI the program started (`program=env arg=claude`, a script that runs one) marks nothing of
/// it, and its conclusion is never two concluders' race.
pub(crate) const PROGRAM_MARK_BY_HARNESS: &str = "ERR task mark: a program worker's task is \
     concluded by its program's exit status, not by a harness\n";

/// Whether a loop carries the session now: a live claim on it, under an Autopilot word that types.
fn loop_carries(ctx: &SessionCtx) -> bool {
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.live_supervisor(crate::metrics::now_us()).is_some()
        && meta.autopilot_word() != Some(aterm_agent::supervise::Autopilot::Off)
}

/// The [`TASK_WHY`] word `value` is, as its `'static` spelling.
fn why_word(value: &str) -> Option<&'static str> {
    TASK_WHY.iter().copied().find(|w| *w == value)
}

fn parse_exit_i32(value: &str) -> Option<i32> {
    let valid = match value.strip_prefix('-') {
        Some(rest) => {
            !rest.is_empty() && !rest.starts_with('0') && rest.bytes().all(|b| b.is_ascii_digit())
        }
        None => !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
    };
    valid.then(|| value.parse::<i32>().ok()).flatten()
}

/// Parsed `task mark` transition and its associated metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskMark {
    /// Target state.
    pub(crate) state: TaskState,
    /// Task origin (`None` preserves the existing record's `from` or defaults to [`FROM_CTL`]).
    pub(crate) from: Option<String>,
    /// Transition reason ([`TASK_WHY`]).
    pub(crate) why: Option<&'static str>,
    /// What aterm checked at the landing ([`TaskRecord::checked`], [`checked_word`]).
    pub(crate) checked: Option<String>,
    /// What the landing read of the agent's left processes ([`TaskRecord::left`], [`left_word`]).
    pub(crate) left: Option<String>,
    /// Agent exit code.
    pub(crate) exit: Option<i32>,
    /// Result size in bytes.
    pub(crate) result_bytes: u64,
    /// Result file path under [`tasks_dir`].
    pub(crate) result_path: Option<PathBuf>,
}

/// Usage error line for `task`.
pub(crate) const TASK_USAGE: &str = "ERR usage: task [mark <working|done|exited|stalled|failed> \
     [from=<s-…|ctl>] [why=<word>] [checked=<word>] [left=<word>] [exit=<n>] [result_bytes=<n>] \
     [result_path=<pct>]]\n";

impl TaskMark {
    /// Creates a mark to `state` with default fields.
    #[must_use]
    pub(crate) const fn new(state: TaskState) -> Self {
        Self {
            state,
            from: None,
            why: None,
            checked: None,
            left: None,
            exit: None,
            result_bytes: 0,
            result_path: None,
        }
    }

    /// Parses `task mark` arguments against `tasks` directory.
    pub(crate) fn parse(args: &str, tasks: Option<&Path>) -> Result<Self, String> {
        let usage = || TASK_USAGE.to_string();
        let mut words = args.split_whitespace();
        let state = words.next().and_then(TaskState::parse).ok_or_else(usage)?;
        let mut mark = Self::new(state);
        let mut seen: Vec<&str> = Vec::new();
        for token in words {
            let (key, value) = token.split_once('=').ok_or_else(usage)?;
            if seen.contains(&key) {
                return Err(usage());
            }
            seen.push(key);
            match key {
                "from" if valid_from(value) => mark.from = Some(value.to_string()),
                "from" => {
                    return Err("ERR task mark: from= is a session id (s-<hex>) or ctl\n".into());
                }
                "why" => {
                    mark.why = Some(why_word(value).ok_or_else(|| {
                        format!("ERR task mark: why= is one of {}\n", TASK_WHY.join("|"))
                    })?);
                }
                "checked" if state != TaskState::Done => {
                    return Err("ERR task mark: checked= marks a done task\n".into());
                }
                "checked" => {
                    mark.checked = Some(checked_word(value).ok_or_else(|| {
                        "ERR task mark: checked= is <head>@<ref>, pushed, no-remote, no-repo or \
                         gone\n"
                            .to_string()
                    })?);
                }
                "left" if state != TaskState::Done => {
                    return Err("ERR task mark: left= marks a done task\n".into());
                }
                "left" => {
                    mark.left = Some(left_word(value).ok_or_else(|| {
                        "ERR task mark: left= is 0, kept:<n>, <n> or unchecked\n".to_string()
                    })?);
                }
                "exit" => mark.exit = Some(parse_exit_i32(value).ok_or_else(usage)?),
                "result_bytes" => {
                    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(usage());
                    }
                    mark.result_bytes = value.parse::<u64>().map_err(|_| usage())?;
                }
                "result_path" => {
                    mark.result_path = Some(result_path(value, tasks).ok_or_else(|| {
                        "ERR task mark: result_path= names a file under <state>/tasks/\n"
                            .to_string()
                    })?);
                }
                _ => return Err(usage()),
            }
        }
        Ok(mark)
    }

    /// Applies this mark to `current` at `now` and `now_unix_ms`, returning `(record, moved)`.
    #[must_use]
    pub(crate) fn apply(
        &self,
        current: Option<&TaskRecord>,
        attention: bool,
        now: Instant,
        now_unix_ms: u64,
    ) -> (TaskRecord, bool) {
        let from = self
            .from
            .clone()
            .or_else(|| current.map(|r| r.from.clone()))
            .unwrap_or_else(|| FROM_CTL.to_string());
        let Some(current) = current else {
            let record = TaskRecord {
                rev: 1,
                state: self.state,
                from,
                manager_node: None,
                told: crate::worker_mail::Told::default(),
                why: self.why,
                checked: self.checked.clone(),
                left: self.left.clone(),
                exit: self.exit,
                finished: false,
                result_bytes: self.result_bytes,
                result_path: self.result_path.clone(),
                attention,
                since_unix_ms: now_unix_ms,
                since: now,
                started_unix_ms: now_unix_ms,
                started: now,
                entered: 1,
                carried_entered: None,
                agent: None,
                trusted_cwd: None,
                program_block: None,
                fact: None,
                fact_rev: 0,
            };
            return (record, true);
        };
        // AN EXIT DOES NOT REWRITE HOW THE TASK ENDED: an agent that leaves after its task finished,
        // whatever word finished it, or after a failed conclusion, keeps that end's `why=` and
        // `checked=` and `left=`, and the fact that it finished stays on the record
        // ([`TaskRecord::finished`]).
        let finished =
            self.state == TaskState::Exited && current.conclusion() == Some(Conclusion::Finished);
        let ended_before =
            finished || (self.state == TaskState::Exited && current.state == TaskState::Failed);
        let why = if ended_before { current.why } else { self.why };
        let checked = if ended_before {
            current.checked.clone()
        } else {
            self.checked.clone()
        };
        let left = if ended_before {
            current.left.clone()
        } else {
            self.left.clone()
        };
        let same = current.state == self.state
            && current.from == from
            && current.why == why
            && current.checked == checked
            && current.left == left
            && current.exit == self.exit
            && current.finished == finished
            && current.result_bytes == self.result_bytes
            && current.result_path == self.result_path
            && current.attention == attention;
        let mut next = current.clone();
        if same {
            return (next, false);
        }
        next.rev = next.rev.saturating_add(1);
        if next.state != self.state {
            next.since = now;
            next.since_unix_ms = now_unix_ms;
            // A look at how the task ended stays a look when its agent leaves after.
            if !ended_before {
                next.entered = next.rev;
            }
        }
        next.state = self.state;
        // A manager named anew is a session here until its spawn says which Mac it runs on.
        if next.from != from {
            next.manager_node = None;
        }
        next.from = from;
        next.why = why;
        next.checked = checked;
        next.left = left;
        next.exit = self.exit;
        next.finished = finished;
        next.result_bytes = self.result_bytes;
        next.result_path.clone_from(&self.result_path);
        next.attention = attention;
        // A PROGRAM'S END CONCLUDES ITS TASK ONCE: a concluded record keeps no block to read it from.
        if matches!(
            self.state,
            TaskState::Done | TaskState::Failed | TaskState::Exited
        ) {
            next.program_block = None;
        }
        (next, true)
    }
}

/// Maximum decoded byte length of `result_path=` (`PATH_MAX`).
pub(crate) const RESULT_PATH_MAX: usize = 1024;

/// A `result_path=` value as a path: pct-decoded, at most
/// [`RESULT_PATH_MAX`] bytes, absolute, spelled the one way its components
/// spell it (no `.`, no `//`, no trailing `/`; [`spelled_as_its_parts`]), and
/// naming a file strictly inside `tasks` with no `..` or control character on
/// the way.
fn result_path(value: &str, tasks: Option<&Path>) -> Option<PathBuf> {
    let tasks = tasks?;
    let decoded = aterm_codec::pct::decode(value);
    if decoded.len() > RESULT_PATH_MAX || decoded.chars().any(char::is_control) {
        return None;
    }
    let path = PathBuf::from(decoded);
    if !spelled_as_its_parts(&path) {
        return None;
    }
    let inside = path.strip_prefix(tasks).ok()?;
    let mut parts = inside.components().peekable();
    parts.peek()?;
    parts
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(path)
}

/// Whether `path` is spelled as the path its own components rebuild: byte for
/// byte off Windows, so a `.`, a `//` or a trailing `/` is a second spelling
/// of one file.
fn spelled_as_its_parts(path: &Path) -> bool {
    let rebuilt: PathBuf = path.components().collect();
    #[cfg(windows)]
    {
        let one_separator = |p: &Path| p.to_string_lossy().replace('/', "\\");
        one_separator(&rebuilt) == one_separator(path)
    }
    #[cfg(not(windows))]
    {
        rebuilt.as_os_str() == path.as_os_str()
    }
}

/// The attention owner codex-alab raises its own points for a person under (a box it left
/// unanswered, a wall, a login): a worker that carries itself, waiting on someone.
pub(crate) const SELF_CLAIM_ATTENTION_OWNER: &str = "codex-alab";

/// The attention owners whose asks move the task record's attention flag.
const TASK_ATTENTION_OWNERS: [&str; 3] = [
    crate::harness_host::ATTENTION_OWNER,
    crate::harness_host::NEEDS_ATTENTION_OWNER,
    SELF_CLAIM_ATTENTION_OWNER,
];

/// Whether an attention entry written by `owner` updates the task record's attention flag.
pub(crate) fn owner_moves_task(owner: &str) -> bool {
    TASK_ATTENTION_OWNERS.contains(&owner)
}

/// Whether a supervisor's, a worker's `NEEDS YOU:` or codex-alab's own ask stands in `meta`. A
/// `kind=wait` entry under those owners (a limit the harness waits out) is no news: `await task`
/// wakes on a worker done, exited, stalled or needing a person, and a wait it sits out is none.
pub(crate) fn supervisor_attention(meta: &crate::session_timeline::SessionMeta) -> bool {
    TASK_ATTENTION_OWNERS
        .iter()
        .any(|owner| meta.attention_owners.ask(owner).is_some())
}

/// Error returned when a mark times out waiting on a committing update's session fence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MarkFenced;

/// [`mark_session_task_within`] with the session's own fence wait: the tests' unconditional mark.
#[cfg(test)]
pub(crate) fn mark_session_task(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
) -> Result<(TaskRecord, bool), MarkFenced> {
    mark_session_task_within(ctx, local_id, mark, ctx.fabric.fence_wait())
}

/// Applies `mark` to `ctx`'s task record, waking waiters and recording timeline events, waiting
/// at most `fence_wait` on a committing update's fence.
pub(crate) fn mark_session_task_within(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
    fence_wait: std::time::Duration,
) -> Result<(TaskRecord, bool), MarkFenced> {
    let marked = mark_session_task_if(ctx, local_id, mark, fence_wait, |_| true)?;
    Ok(marked.expect("an unconditional mark is applied"))
}

/// [`mark_session_task_within`] applied only when `when` holds on the current record.
pub(crate) fn mark_session_task_if(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
    fence_wait: std::time::Duration,
    when: impl FnOnce(Option<&TaskRecord>) -> bool,
) -> Result<Option<(TaskRecord, bool)>, MarkFenced> {
    let marked = mark_session_task_with(
        ctx,
        local_id,
        Some(mark),
        AttentionEdit::Keep,
        fence_wait,
        |record, _| when(record),
    )?;
    Ok(marked.and_then(|m| m.record.map(|record| (record, m.moved))))
}

/// Edit to apply to the supervisor's attention entry alongside a task mark.
#[derive(Clone, Copy)]
pub(crate) enum AttentionEdit<'a> {
    /// Leave the attention entry unchanged.
    Keep,
    /// Raise the attention entry with the given text.
    Raise(&'a str),
    /// Clear the attention entry if the predicate holds on the standing text.
    TakeDown(fn(Option<&str>) -> bool),
}

/// Outcome of [`mark_session_task_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MarkedWith {
    /// Resulting task record, if any.
    pub(crate) record: Option<TaskRecord>,
    /// Whether the task record revision advanced.
    pub(crate) moved: bool,
    /// Whether the supervisor's attention entry changed.
    pub(crate) attention_moved: bool,
}

/// [`mark_session_task_within`] applied only when `when` holds on the session's whole timeline
/// (its record and task face, read together under the lock the mark takes).
pub(crate) fn mark_session_task_seen(
    ctx: &SessionCtx,
    local_id: u64,
    mark: &TaskMark,
    fence_wait: std::time::Duration,
    when: impl FnOnce(&crate::session_timeline::SessionTimeline) -> bool,
) -> Result<Option<(TaskRecord, bool)>, MarkFenced> {
    let marked = mark_on_timeline(
        ctx,
        local_id,
        Some(mark),
        AttentionEdit::Keep,
        fence_wait,
        |timeline, _| when(timeline),
    )?;
    Ok(marked.and_then(|m| m.record.map(|record| (record, m.moved))))
}

/// Atomically applies `mark` and `attention` under the session timeline lock when `when` holds.
pub(crate) fn mark_session_task_with(
    ctx: &SessionCtx,
    local_id: u64,
    mark: Option<&TaskMark>,
    edit: AttentionEdit<'_>,
    fence_wait: std::time::Duration,
    when: impl FnOnce(Option<&TaskRecord>, Option<&str>) -> bool,
) -> Result<Option<MarkedWith>, MarkFenced> {
    mark_on_timeline(
        ctx,
        local_id,
        mark,
        edit,
        fence_wait,
        |timeline, standing| when(timeline.task(), standing),
    )
}

/// [`mark_session_task_with`], its `when` reading the whole timeline.
fn mark_on_timeline(
    ctx: &SessionCtx,
    local_id: u64,
    mark: Option<&TaskMark>,
    edit: AttentionEdit<'_>,
    fence_wait: std::time::Duration,
    when: impl FnOnce(&crate::session_timeline::SessionTimeline, Option<&str>) -> bool,
) -> Result<Option<MarkedWith>, MarkFenced> {
    let outcome = ctx
        .fabric
        .run_unfenced(fence_wait, || {
            let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let owner = crate::harness_host::ATTENTION_OWNER;
            if !when(&timeline, meta.attention_owners.get(owner)) {
                return None;
            }
            let attention_moved = match edit {
                AttentionEdit::Keep => false,
                AttentionEdit::Raise(text) => crate::session_timeline::put_supervisor_attention(
                    &mut meta,
                    &mut timeline,
                    Some(text),
                ),
                AttentionEdit::TakeDown(stands) => {
                    stands(meta.attention_owners.get(owner))
                        && crate::session_timeline::put_supervisor_attention(
                            &mut meta,
                            &mut timeline,
                            None,
                        )
                }
            };
            let attention = supervisor_attention(&meta);
            let (record, moved) = match mark {
                Some(mark) => {
                    let (record, moved) =
                        timeline.mark_task(mark, attention, Instant::now(), unix_ms_now());
                    (Some(record), moved)
                }
                None => {
                    let moved = timeline.note_task_attention(attention);
                    (timeline.task().cloned(), moved)
                }
            };
            timeline.slog_take_wake();
            drop(timeline);
            drop(meta);
            Some(MarkedWith {
                record,
                moved,
                attention_moved,
            })
        })
        .ok_or(MarkFenced)?;
    let record_moved = outcome
        .as_ref()
        .is_some_and(|m| m.moved || m.attention_moved);
    if record_moved {
        crate::presence::post_task_changed(local_id);
    }
    if outcome
        .as_ref()
        .is_some_and(|m| m.moved && m.record.as_ref().is_some_and(|r| r.entered == r.rev))
    {
        crate::harness_host::ring();
    }
    Ok(outcome)
}

/// Unconditional fact edit for the task-record tests.
#[cfg(test)]
pub(crate) fn mark_session_task_fact(
    ctx: &SessionCtx,
    local_id: u64,
    word: Option<FactWord>,
    fence_wait: std::time::Duration,
) -> NoteOutcome {
    mark_session_task_fact_if(ctx, local_id, word, fence_wait, || true)
}

/// Sets the fact word (`fact=`, the occupancy design's manager door) on `ctx`'s task record, or
/// clears it with `None`, under the lock a mark takes and the fence a committing update holds
/// (waiting at most `fence_wait`). It moves only [`TaskRecord::fact_rev`], so `await task` past a
/// rev and `result`'s repair CAS see no change; a `facts=1` waiter and the session's
/// `EVENT … task` see it. Noting the standing word again, or clearing with none standing, moves
/// nothing.
///
/// [`NoteOutcome::NoTask`] with no record, [`NoteOutcome::Closed`] for a word on an `exited`
/// record (a clear is always taken), [`NoteOutcome::Fenced`] when the fence held past
/// `fence_wait` or `when` (the caller's live write authority) is false. `when` is read after both
/// the fence and timeline waits, so a mode change during either wait leaves the loop's note or
/// clear pending and moves no record and posts no wake.
pub(crate) fn mark_session_task_fact_if(
    ctx: &SessionCtx,
    local_id: u64,
    word: Option<FactWord>,
    fence_wait: std::time::Duration,
    when: impl FnOnce() -> bool,
) -> NoteOutcome {
    let edited = ctx.fabric.run_unfenced(fence_wait, || {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        if !when() {
            return (NoteOutcome::Fenced, false);
        }
        let edited = timeline.note_task_fact(word);
        timeline.slog_take_wake();
        edited
    });
    let Some((outcome, moved)) = edited else {
        return NoteOutcome::Fenced;
    };
    if moved {
        crate::presence::post_task_changed(local_id);
    }
    outcome
}

/// What [`TaskRecord`] the fact edit `word` makes of `current`, and the outcome: `None` for no
/// move ([`mark_session_task_fact_if`]'s rules).
pub(crate) fn fact_edit(
    current: &TaskRecord,
    word: Option<FactWord>,
) -> (NoteOutcome, Option<TaskRecord>) {
    let outcome = if word.is_some() {
        NoteOutcome::Noted
    } else {
        NoteOutcome::Cleared
    };
    if word.is_some() && current.state == TaskState::Exited {
        return (NoteOutcome::Closed, None);
    }
    if current.fact == word {
        return (outcome, None);
    }
    let mut next = current.clone();
    next.fact = word;
    next.fact_rev = next.fact_rev.saturating_add(1);
    if crate::occupancy_host::fault::armed(crate::occupancy_host::fault::Fault::NoteBumpsNews) {
        next.rev = next.rev.saturating_add(1);
    }
    (outcome, Some(next))
}

/// Reopens a finished or failed worker's task to `working` when a new turn is sent to it. A
/// program worker's never reopens ([`TaskRecord::runs_a_program`]): its task was its one program,
/// whose end concluded it, and what a turn types runs at its shell as no task of its.
pub(crate) fn reopen_at_turn(ctx: &SessionCtx, local_id: u64, by: &TurnBy, found: Option<u64>) {
    let is_done = |r: Option<&TaskRecord>| {
        r.is_some_and(|r| {
            matches!(r.state, TaskState::Done | TaskState::Failed)
                && Some(r.entered) == found
                && !r.runs_a_program()
        })
    };
    if *by == TurnBy::Harness
        || !is_done(
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task(),
        )
    {
        return;
    }
    let mut mark = TaskMark::new(TaskState::Working);
    mark.why = Some("turn");
    let fence = ctx.fabric.fence_wait();
    if mark_session_task_if(ctx, local_id, &mark, fence, is_done).is_err() {
        aterm_log::warn!(
            "task: a turn into a finished worker was not marked working: an update is committing"
        );
    }
}

/// What the Autopilot word now standing makes of `record`, when it moves the task
/// ([`note_autopilot_word`]): a working worker whose word someone other than aterm turned Off after
/// it began to work has nothing carrying it on, so it fails, `why=off` where a person did it and
/// `why=off-ctl` where anything else did; one that failed by the word (those two, or `why=expired`,
/// whose end is Off too) goes back to working when someone turns it on again. Cruise, set after a
/// worker failed, reopens it however it failed: Cruise never rests, so it types into the worker
/// whatever ended it, and a record that said failed while it types would tell a waiter nothing
/// true. aterm's own Off is the expiry's, which marks its own `why=`.
///
/// AN OFF FAILS THE TASK ONCE IT HAS STOOD [`OFF_SETTLES_MS`] (`now_unix_ms`): the chip's click
/// order passes through Off on its way from Land to Cruise, and a click that only passes through
/// tells a waiter nothing ([`off_settles_in`] says when to look again).
///
/// A PROGRAM WORKER'S TASK NEVER MOVES BY THE WORD ([`TaskRecord::runs_a_program`]): no Autopilot
/// carries it, so an Off stops nothing of its and Cruise types no task into it; its program's exit
/// status alone concludes it.
#[must_use]
pub(crate) fn autopilot_mark(
    record: &TaskRecord,
    word: Option<aterm_agent::supervise::Autopilot>,
    set_by: Option<&crate::session_timeline::AutopilotSetBy>,
    now_unix_ms: u64,
) -> Option<TaskMark> {
    use aterm_agent::supervise::Autopilot;
    if record.runs_a_program() {
        return None;
    }
    let set_by = set_by.filter(|by| by.actor != aterm_slog::Actor::Aterm)?;
    let set_ms = u64::try_from(set_by.since_ms).ok();
    let set_since = set_ms.is_some_and(|ms| ms >= record.since_unix_ms);
    let off_stood = set_ms.is_some_and(|ms| now_unix_ms >= ms.saturating_add(OFF_SETTLES_MS));
    match (word?, record.state, record.why) {
        (Autopilot::Cruise, TaskState::Failed, _) if set_since => {
            let mut mark = TaskMark::new(TaskState::Working);
            mark.why = Some("turn");
            Some(mark)
        }
        (Autopilot::Off, TaskState::Working, _) if set_since && off_stood => {
            let mut mark = TaskMark::new(TaskState::Failed);
            mark.why = Some(if set_by.actor == aterm_slog::Actor::Human {
                WHY_OFF
            } else {
                WHY_OFF_CTL
            });
            Some(mark)
        }
        (
            Autopilot::Land | Autopilot::Cruise,
            TaskState::Failed,
            Some(WHY_OFF | WHY_OFF_CTL | WHY_EXPIRED),
        ) => {
            let mut mark = TaskMark::new(TaskState::Working);
            mark.why = Some("turn");
            Some(mark)
        }
        _ => None,
    }
}

/// How long an Off of someone other than aterm stands before it fails a working task
/// ([`autopilot_mark`]): longer than a person's two clicks from Land through Off to Cruise.
pub(crate) const OFF_SETTLES_MS: u64 = 3_000;

/// How long until the Off standing on `record`'s session has stood [`OFF_SETTLES_MS`], where it is
/// one that will fail the task then ([`autopilot_mark`]); `None` where no Off waits to fail it, a
/// program worker's included.
#[must_use]
pub(crate) fn off_settles_in(
    record: &TaskRecord,
    word: Option<aterm_agent::supervise::Autopilot>,
    set_by: Option<&crate::session_timeline::AutopilotSetBy>,
    now_unix_ms: u64,
) -> Option<std::time::Duration> {
    if record.runs_a_program() {
        return None;
    }
    let set_by = set_by.filter(|by| by.actor != aterm_slog::Actor::Aterm)?;
    let set_ms = u64::try_from(set_by.since_ms).ok()?;
    let settles = set_ms.saturating_add(OFF_SETTLES_MS);
    (word? == aterm_agent::supervise::Autopilot::Off
        && record.state == TaskState::Working
        && set_ms >= record.since_unix_ms
        && now_unix_ms < settles)
        .then(|| std::time::Duration::from_millis(settles - now_unix_ms))
}

/// Holds session `local_id`'s task record to its Autopilot word ([`autopilot_mark`]), read together
/// under the mark's lock. On the window's thread, so it waits on no update's fence: every later
/// move of the session's meta looks again. Returns how long until an Off not yet settled is due to
/// be looked at again ([`off_settles_in`]), which the caller owes.
#[must_use]
pub(crate) fn note_autopilot_word(ctx: &SessionCtx, local_id: u64) -> Option<std::time::Duration> {
    let (word, set_by) = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        (meta.autopilot_word(), meta.autopilot_set_by)
    };
    let current = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .cloned();
    let current = current?;
    let now_ms = unix_ms_now();
    let Some(mark) = autopilot_mark(&current, word, set_by.as_ref(), now_ms) else {
        return off_settles_in(&current, word, set_by.as_ref(), now_ms);
    };
    let same = |now: Option<&TaskRecord>| now.is_some_and(|now| now.rev == current.rev);
    if mark_session_task_if(ctx, local_id, &mark, std::time::Duration::ZERO, same).is_err() {
        aterm_log::warn!(
            "task: the record does not yet say Autopilot's move: an update is committing"
        );
    }
    None
}

/// Usage error line for `result`.
pub(crate) const RESULT_USAGE: &str = "ERR usage: result [full]\n";

/// `result [full]`'s `full`, or its usage line.
fn result_full(rest: &str) -> Result<bool, String> {
    match rest.trim() {
        "" => Ok(false),
        "full" => Ok(true),
        _ => Err(RESULT_USAGE.to_string()),
    }
}

/// `result [full]` for a worker whose tab aterm closed, its task's end `record`
/// ([`crate::retired_workers`]): its result file, kept while its row stands, read as a live
/// worker's is, with no session left to read a missed result again from.
pub(crate) fn retired_result(record: &TaskRecord, rest: &str) -> String {
    match result_full(rest) {
        Ok(full) => answer_result(record, |path| {
            aterm_agent::harness::result::answer(path, full)
        }),
        Err(usage) => usage,
    }
}

/// `record`'s `result` reply: `read` of its result file where an end left one, else why there is
/// none.
fn answer_result(record: &TaskRecord, read: impl FnOnce(&Path) -> String) -> String {
    match (&record.result_path, record.state) {
        (Some(path), TaskState::Done | TaskState::Exited | TaskState::Failed) => read(path),
        (_, TaskState::Done) => "ERR result unavailable why=no-snapshot\n".to_string(),
        (_, state) => format!("ERR result unavailable why={}\n", state.as_str()),
    }
}

/// Handles `result [full]` on `ctx` (session `local_id`): a result the done edge missed is read
/// again (`result::reread_with`), a codex landing's that knew no conversation by the one the
/// worker's claim names now (`self_claim::rollout_target`, the landing's own resolver).
pub(crate) fn cmd_result(ctx: &SessionCtx, local_id: u64, rest: &str) -> String {
    let full = match result_full(rest) {
        Ok(full) => full,
        Err(usage) => return usage,
    };
    let record = ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .cloned();
    let Some(record) = record else {
        return "ERR no task\n".to_string();
    };
    answer_result(&record, |path| {
        let claim = || crate::self_claim::rollout_target(ctx).map(|t| (t.home, t.thread));
        match aterm_agent::harness::result::reread_with(path, claim) {
            Some(found) => {
                repair_result(ctx, local_id, &record, path, &found);
                found.answer(path, full)
            }
            None => aterm_agent::harness::result::answer(path, full),
        }
    })
}

/// Marks `record` (as `result` read it) again with `found`'s size as its `result_bytes=`, when
/// the record has not moved since: every other field as it stands, a new rev, its waiters woken.
/// Then writes the snapshot at `path` and its sidecar over with it, so `task`, `await task` and
/// every later `result` agree.
fn repair_result(
    ctx: &SessionCtx,
    local_id: u64,
    record: &TaskRecord,
    path: &Path,
    found: &aterm_agent::harness::result::Reread,
) {
    let mark = TaskMark {
        state: record.state,
        from: Some(record.from.clone()),
        why: record.why,
        checked: record.checked.clone(),
        left: record.left.clone(),
        exit: record.exit,
        result_bytes: found.bytes(),
        result_path: record.result_path.clone(),
    };
    let same = |now: Option<&TaskRecord>| now.is_some_and(|now| now.rev == record.rev);
    match mark_session_task_if(ctx, local_id, &mark, std::time::Duration::ZERO, same) {
        Ok(Some(_)) => {
            if let Err(e) = found.write(path) {
                aterm_log::warn!(
                    "task: the result read again was not written over {}: {e}",
                    path.display()
                );
            }
        }
        Ok(None) => {}
        Err(MarkFenced) => aterm_log::warn!(
            "task: the result read again is not kept while an update commits; the next `result` \
             reads it again"
        ),
    }
}

/// `ctx`'s task record for a restore manifest ([`crate::restore::TerminalLeafRestore::task`]): the
/// carry row without its ages, which grow while the record stands still and would have every image
/// written anew. A row without them goes by the wall clock ([`TaskRecord::from_carry_row`]), which
/// is what a restore has: aterm was not running to measure the time between.
pub(crate) fn restore_row(ctx: &SessionCtx) -> Option<String> {
    let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let row = timeline.task()?.carry_row(Instant::now());
    Some(TaskRecord::carry_face(&row))
}

/// Restores a parked worker's record ([`restore_row`]) onto `ctx`, in a restore other than a
/// seamless update. A worker whose agent was `followed` when the image was taken, or whose record
/// already says it ran (a `why=`), reads `working why=restored`, which no startup or held deadline
/// reads as one just spawned. One neither says of, in a shell a keeper handed back (`kept`), may
/// never have had its line typed, and the launcher that would have typed it ended with the process:
/// it reads `working` from now, so the startup deadline judges what its screen shows. Otherwise, where its agent is not coming
/// back, it reads `exited why=exit`: gone (a plain quit forgets it, and no keeper handed its shell
/// back), or a disposable worker whose shell did not come back running. aterm's disk worker is
/// disposable: a later need starts a new one, so a cold restart never relaunches it
/// ([`disposable_worker`]). Returns whether this is such a worker, whose agent the restore leaves
/// unlaunched. One whose relaunch is over without its agent is marked exited then
/// ([`mark_restored_over`]).
pub(crate) fn seed_restored(ctx: &SessionCtx, row: &str, followed: bool, kept: bool) -> bool {
    let now = Instant::now();
    let now_unix_ms = unix_ms_now();
    let Some(mut record) =
        TaskRecord::from_carry_row(row, tasks_dir().as_deref(), now, now_unix_ms)
    else {
        aterm_log::warn!(
            "task: the restored task record does not read ({row:?}); its tab returns as a person's"
        );
        return false;
    };
    let disposable = !kept && disposable_worker(record.trusted_cwd.as_deref());
    let gone = (!followed && !kept) || disposable;
    match record.state {
        TaskState::Working | TaskState::Stalled if gone => {
            record.rev += 1;
            record.entered = record.rev;
            record.state = TaskState::Exited;
            record.why = Some("exit");
            record.exit = None;
            record.attention = false;
            record.since = now;
            record.since_unix_ms = now_unix_ms;
        }
        TaskState::Working if !followed && record.why.is_none() => {
            record.why = None;
            record.since = now;
            record.since_unix_ms = now_unix_ms;
            record.carried_entered = Some(record.entered);
        }
        TaskState::Working => {
            record.why = Some(WHY_RESTORED);
            record.carried_entered = Some(record.entered);
        }
        // A failure told before the quit is not news after it.
        TaskState::Stalled | TaskState::Done | TaskState::Failed | TaskState::Exited => {
            record.carried_entered = Some(record.entered);
        }
    }
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .seed_task(record);
    crate::session_timeline::keep_worker_autopilot(ctx);
    disposable
}

/// Whether a worker trusted to folder `trusted_cwd` is disposable: aterm's disk worker, which a
/// cold restart does not resume ([`seed_restored`]).
#[must_use]
pub(crate) fn disposable_worker(trusted_cwd: Option<&str>) -> bool {
    trusted_cwd.is_some_and(|folder| {
        aterm_agent::harness::disk::fence::is_worker_folder(Path::new(folder))
    })
}

/// A RESTORED WORKER WHOSE RELAUNCH IS OVER WITHOUT ITS AGENT (every attempt failed, the restore did
/// not hand it on, the harness or its Autopilot is off, or its tab was held): a record still
/// `working why=restored`, or still the stall it carried across the restart, reads `exited
/// why=exit`, a failed conclusion, as one whose agent a quit forgot does ([`seed_restored`]). A
/// record that moved since, by its agent, a look or a person, stands.
/// On the window's thread, so it waits on no update's fence.
pub(crate) fn mark_restored_over(ctx: &SessionCtx, local_id: u64) {
    let mut mark = TaskMark::new(TaskState::Exited);
    mark.why = Some(WHY_EXIT);
    let restored = |now: Option<&TaskRecord>| {
        now.is_some_and(|r| {
            (r.state == TaskState::Working && r.why == Some(WHY_RESTORED))
                || (r.state == TaskState::Stalled && r.carried_entered == Some(r.entered))
        })
    };
    if mark_session_task_if(ctx, local_id, &mark, std::time::Duration::ZERO, restored).is_err() {
        aterm_log::warn!(
            "task: a restored worker whose agent did not come back still reads working: an update \
             is committing"
        );
    }
}

/// Notes on `ctx`'s program worker record ([`TaskRecord::runs_a_program`]) the shell block `block`
/// its launcher just typed its program into ([`TaskRecord::program_block`]), where none is noted
/// and the record is still open (`working`, or `stalled` by a deadline that read it first), under
/// the fence a committing update holds, waiting at most `fence_wait`, so a seamless update carries
/// it. It moves no rev: nothing a waiter reads changed. Returns whether it was noted.
pub(crate) fn note_program_block(
    ctx: &SessionCtx,
    block: u64,
    fence_wait: std::time::Duration,
) -> bool {
    ctx.fabric
        .run_unfenced(fence_wait, || {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .note_program_block(block)
        })
        .unwrap_or(false)
}

/// Restores a carried task record onto `ctx` during seamless-update adoption.
pub(crate) fn seed_carried(ctx: &SessionCtx, row: Option<&str>) {
    let Some(row) = row else {
        return;
    };
    let Some(mut record) =
        TaskRecord::from_carry_row(row, tasks_dir().as_deref(), Instant::now(), unix_ms_now())
    else {
        aterm_log::warn!(
            "task: the carried task record does not read ({row:?}); the session crosses without one"
        );
        return;
    };
    record.carried_entered = Some(record.entered);
    record.attention = supervisor_attention(&ctx.meta.lock().unwrap_or_else(|p| p.into_inner()));
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .seed_task(record);
    crate::session_timeline::keep_worker_autopilot(ctx);
}

/// Returns the session's published agent word, the program's own report as the timeline
/// shows it (`program_state=`, [`crate::program_state::column`]: a claim beside `agent=`, read
/// under the same lock) and the percent-encoded attention text, an ask before any wait
/// ([`crate::session_timeline::SessionMeta::row_attention`]).
pub(crate) fn agent_program_and_attention(ctx: &SessionCtx) -> (&'static str, String, String) {
    let attention = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .row_attention()
        .map_or_else(|| "-".to_string(), crate::control::pct_encode);
    let (agent, program) = {
        let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        (
            timeline.agent().word,
            crate::program_state::column(timeline.program_shown()),
        )
    };
    (agent, program, attention)
}

/// The `task` reply line for `record`, unterminated: `OK task -` with none,
/// else `OK task <fields> agent= program_state= attention=`.
fn task_reply(ctx: &SessionCtx, record: Option<&TaskRecord>) -> String {
    let Some(record) = record else {
        return "OK task -".to_string();
    };
    let (agent, program, attention) = agent_program_and_attention(ctx);
    format!(
        "OK task {} agent={agent} program_state={program} attention={attention}",
        record.wire_fields()
    )
}

/// The `task` reply for a worker whose tab aterm closed once its task concluded
/// ([`crate::retired_workers`]): its final record, with no agent, program or attention, its session
/// gone.
pub(crate) fn retired_task_reply(record: &TaskRecord) -> String {
    format!(
        "OK task {} agent=- program_state=- attention=-\n",
        record.wire_fields()
    )
}

/// Handles `task` and `task mark …` on `ctx` (session `local_id`).
pub(crate) fn cmd_task(ctx: &SessionCtx, local_id: u64, scope: Scope, rest: &str) -> String {
    let rest = rest.trim();
    if rest.is_empty() {
        let record = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .task()
            .cloned();
        return format!("{}\n", task_reply(ctx, record.as_ref()));
    }
    let Some(args) = rest
        .strip_prefix("mark")
        .filter(|a| a.is_empty() || a.starts_with(' '))
    else {
        return TASK_USAGE.to_string();
    };
    if !scope.is_owner_class() {
        return "ERR denied\n".to_string();
    }
    let parsed = match TaskMark::parse(args, tasks_dir().as_deref()) {
        Ok(parsed) => parsed,
        Err(reply) => return reply,
    };
    if parsed.checked.is_some() && crate::control::serving_principal() != Principal::Harness {
        return CHECKED_BY_HAND.to_string();
    }
    if parsed.left.is_some() && crate::control::serving_principal() != Principal::Harness {
        return LEFT_BY_HAND.to_string();
    }
    if parsed.state == TaskState::Failed
        && crate::control::serving_principal() != Principal::Harness
        && loop_carries(ctx)
    {
        return FAILED_UNDER_A_LOOP.to_string();
    }
    let deadline_stall =
        parsed.state == TaskState::Stalled && matches!(parsed.why, Some("startup" | "held"));
    let edit = if deadline_stall {
        AttentionEdit::Keep
    } else {
        AttentionEdit::TakeDown(crate::harness_host::deadline_attention_stands)
    };
    let by_harness = crate::control::serving_principal() == Principal::Harness;
    let Ok(marked) = mark_session_task_with(
        ctx,
        local_id,
        Some(&parsed),
        edit,
        ctx.fabric.fence_wait(),
        |record, _| !(by_harness && record.is_some_and(TaskRecord::runs_a_program)),
    ) else {
        return crate::fabric::HOLD_BUSY.to_string();
    };
    let Some(marked) = marked else {
        return PROGRAM_MARK_BY_HARNESS.to_string();
    };
    if marked.attention_moved {
        crate::presence::post_meta_changed(local_id);
    }
    format!(
        "{} changed={}\n",
        task_reply(ctx, marked.record.as_ref()),
        u8::from(marked.moved)
    )
}

/// How an `await task` over multiple targets completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Combine {
    /// Completes when the first target has news.
    Any,
    /// Completes when every target has news.
    All,
}

/// Maximum number of targets allowed in a single `await task` request.
pub(crate) const MAX_AWAIT_TARGETS: usize = 64;

/// Usage error line for `await task`.
pub(crate) const AWAIT_TASK_USAGE: &str = "ERR usage: await task \
     [@<sel>[:<rev>[:<fact_rev>]][,@<sel>…]] [any|all] [since=<rev>] [facts=1 \
     [fact_since=<fact_rev>]] [timeout=<ms>]\n";

/// Single target selector and optional per-target `since` revision for `await task`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetArg {
    /// Selector body after `@`.
    pub(crate) selector: String,
    /// Per-target `since` revision override.
    pub(crate) since: Option<u64>,
    /// Per-target `fact_since` override (`@<sel>:<rev>:<fact_rev>`, with `facts=1` only).
    pub(crate) fact_since: Option<u64>,
}

/// Parsed `await task` arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AwaitTaskArgs {
    /// Target list (empty for the single-session form).
    pub(crate) targets: Vec<TargetArg>,
    /// Completion mode (`any` or `all`).
    pub(crate) combine: Combine,
    /// Default `since` revision for targets without an explicit `:rev`.
    pub(crate) since: Option<u64>,
    /// `facts=1`: a fact word noted past a target's fact edge is news too
    /// ([`TaskRecord::fact_rev`]).
    pub(crate) facts: bool,
    /// Default fact edge (`fact_since=`) for targets without an explicit `:<fact_rev>`: else the
    /// target's `fact_rev` when the wait parks.
    pub(crate) fact_since: Option<u64>,
    /// Optional timeout in milliseconds.
    pub(crate) timeout_ms: Option<u64>,
}

/// Parses `await task` arguments after the `task` word.
pub(crate) fn parse_await_task(words: &[&str], list: bool) -> Result<AwaitTaskArgs, String> {
    let usage = || AWAIT_TASK_USAGE.to_string();
    let parse_rev = |s: &str| -> Result<u64, String> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(usage());
        }
        s.parse::<u64>().map_err(|_| usage())
    };
    let mut args = AwaitTaskArgs {
        targets: Vec::new(),
        combine: Combine::Any,
        since: None,
        facts: false,
        fact_since: None,
        timeout_ms: None,
    };
    let (mut combine_seen, mut list_seen) = (false, false);
    let mut words = words.iter().copied();
    while let Some(word) = words.next() {
        if word.starts_with('@') {
            if !list || list_seen {
                return Err(usage());
            }
            list_seen = true;
            for element in word.split(',') {
                let body = element.strip_prefix('@').ok_or_else(usage)?;
                let (selector, since, fact_since) = match body.split_once(':') {
                    Some((selector, revs)) => {
                        let (rev, fact) = match revs.split_once(':') {
                            Some((rev, fact)) => (rev, Some(parse_rev(fact)?)),
                            None => (revs, None),
                        };
                        (selector, Some(parse_rev(rev)?), fact)
                    }
                    None => (body, None, None),
                };
                if !aterm_types::control_verbs::selector_body_names_something(selector) {
                    return Err(format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR));
                }
                if args.targets.len() == MAX_AWAIT_TARGETS {
                    return Err(format!(
                        "ERR too many await task targets (max {MAX_AWAIT_TARGETS})\n"
                    ));
                }
                args.targets.push(TargetArg {
                    selector: selector.to_string(),
                    since,
                    fact_since,
                });
            }
        } else if matches!(word, "any" | "all") {
            if combine_seen {
                return Err(usage());
            }
            combine_seen = true;
            args.combine = if word == "all" {
                Combine::All
            } else {
                Combine::Any
            };
        } else if let Some(rev) = word.strip_prefix("since=") {
            if args.since.is_some() {
                return Err(usage());
            }
            args.since = Some(parse_rev(rev)?);
        } else if word == "facts=1" {
            if args.facts {
                return Err(usage());
            }
            args.facts = true;
        } else if let Some(rev) = word.strip_prefix("fact_since=") {
            if args.fact_since.is_some() {
                return Err(usage());
            }
            args.fact_since = Some(parse_rev(rev)?);
        } else {
            let value = match word.strip_prefix("timeout=") {
                Some(value) => value,
                None if word == "timeout" => words.next().ok_or_else(usage)?,
                None => return Err(usage()),
            };
            if args.timeout_ms.is_some() {
                return Err(usage());
            }
            args.timeout_ms = Some(aterm_control::wire::wait_ms(value).ok_or_else(usage)?);
        }
    }
    let fact_edge =
        args.fact_since.is_some() || args.targets.iter().any(|t| t.fact_since.is_some());
    if fact_edge && !args.facts {
        return Err(usage());
    }
    Ok(args)
}

/// Formats a single `await task` reply row (` conclusion=` once the task ended,
/// [`TaskRecord::conclusion`], ` why=` where something said, ` checked=` where a landing named what
/// aterm checked, §14.9 slice 9c, ` left=` where it read the agent's left processes, ` exit=` where
/// an exit code is known, as `task` orders them, and ` fact= fact_rev=` at its end once the
/// record's fact word was ever edited, [`TaskRecord::push_fact`]); `program` is the
/// `program_state=` column, a claim shown
/// beside `agent=` that no wait reads. A tab closed on a task with no conclusion yet reads
/// `conclusion=failed`: nothing is left to finish it.
#[must_use]
pub(crate) fn await_row(
    sid: &str,
    record: Option<&TaskRecord>,
    agent: &str,
    program: &str,
    attention: &str,
    closed: bool,
) -> String {
    let (rev, state, result_bytes) =
        record.map_or((0, "-", 0), |r| (r.rev, r.state.as_str(), r.result_bytes));
    let state = if closed { "closed" } else { state };
    let mut row = format!(
        "@{sid} rev={rev} state={state} agent={agent} program_state={program} \
         attention={attention} result_bytes={result_bytes}"
    );
    if let Some(record) = record {
        let conclusion = record.conclusion().or(closed.then_some(Conclusion::Failed));
        if let Some(conclusion) = conclusion {
            row.push_str(" conclusion=");
            row.push_str(conclusion.word());
        }
        if let Some(why) = record.why {
            row.push_str(" why=");
            row.push_str(why);
        }
        if let Some(checked) = &record.checked {
            row.push_str(" checked=");
            row.push_str(checked);
        }
        if let Some(left) = &record.left {
            row.push_str(" left=");
            row.push_str(left);
        }
        if let Some(exit) = record.exit {
            row.push_str(&format!(" exit={exit}"));
        }
        record.push_fact(&mut row);
    }
    row
}

/// Formats `task_state=` and `task_rev=` tokens for `status` and `sessions`.
#[must_use]
pub(crate) fn status_tokens(record: Option<&TaskRecord>) -> String {
    match record {
        Some(r) => format!("task_state={} task_rev={}", r.state.as_str(), r.rev),
        None => "task_state=- task_rev=0".to_string(),
    }
}

#[cfg(test)]
#[path = "task_record_tests.rs"]
mod tests;
