// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `forever --harness <claude|gemini|agy|muse|all> [--mode land|cruise|both]
//! [tag] [--keep]`: FOREVER LAND AND FOREVER CRUISE, LIVE, on every harness
//! aterm supervises — the real Claude Code, Gemini CLI, agy and Muse Code TUIs
//! under a private headless aterm with the harness ON (its defaults), against
//! the stub, each conversation scripted turn by turn
//! ([`crate::stub::scripted_reply`]: the custom mode `forever`, whose rules pick
//! a turn's reply by the scenario's word in the conversation and the act of the
//! harness it answers).
//!
//! A lab of its own per harness (`forever-<pid>` unless a tag is given; `all`
//! runs the four one after another, `<tag>-<harness>`). Every scenario CHAIN
//! ([`Chain`]) is a tab of its own, spawned in a scratch git repository of its
//! own under the lab's project (`<proj>/<chain>`, its `main` pushed to a local
//! bare `origin`, `origin/HEAD` naming it, then left as the chain's [`Repo`]
//! says), its Autopilot set on the tab (`meta set autopilot`) — and for
//! `land-e` a worker's task record (`task mark working from=ctl`) — before
//! the agent starts there, and its first prompt carrying the chain's word. The
//! chains run side by side, so a harness takes its longest chain's time (about
//! 15 min: `land-a` lands at about +6 min and is watched past the ladder's
//! next step, 8 min, after it; 30 where an agent's first screen counted as a
//! turn, P3). Each scenario is judged from the instance's own records — the
//! tab's harness journal (`<state>/drive/<sid>.journal.jsonl`), its ledger
//! (`<sid>.jsonl`), `status` (`task=`, `needs=`, `autopilot=`,
//! `supervisor=`), `meta` (`attention_owners=`, `attention_kinds=`,
//! `attention=`), the task record (`task`) and the stub's request dumps —
//! never the screen alone ([`judge`]):
//!
//! * LAND (`autopilot=land`):
//!   * `land-a`: L1 a work reply with no defense: `land@v1` typed and
//!     ledgered; L3 the land act answered with `NEEDS YOU: rotate the deploy
//!     key` and a defense: the needs entry raised (`NEEDS` journaled, `meta`
//!     `attention_kinds=` naming `ask:needs`, `<program> needs you: … —
//!     carrying on`, `status needs=ask`), a nudge typed that tells the worker
//!     its need is on the person's screen, not landed; L5 the nudge answered
//!     with a clean defense over the clean, pushed repository: `DONE …
//!     rule=land@v1`, `status task=landed`, the needs entry cleared, no task
//!     record invented (`OK task -` throughout: none was given), and the
//!     ladder over — no `WAITING` after the DONE, its last ledger row
//!     `task-done@v1` — with nothing typed (no journal act, no ledger act, no
//!     new harness turn in the stub's dumps) through the ladder's next step
//!     and a minute past it.
//!   * `land-b`: L2 the land act answered with a `Nothing left:` defense beside
//!     a stated leftover (`The docs PR is still open.`): a nudge whose text
//!     names the open word and not the repository, its yield read and
//!     waited on, not landed.
//!   * `land-c`, `land-f`, `land-g`, `land-h`, `land-i`: L4, L8, L9, L10, L11
//!     a clean defense while the repository holds an untracked file, a
//!     changed tracked file, a stash entry on `main`, a commit no remote
//!     holds, a commit pushed to `origin/wip` but not on `origin/main`: a
//!     nudge naming that leg of the repository's work in aterm's words (and
//!     no other leg, no open word), its yield read and waited on, not landed
//!     — the live negative control of L5's judge, which fails each.
//!   * `land-d`: L6 a reply taller than the window, a `NEEDS YOU:` near its
//!     top and `Nothing left:` at its bottom: not landed, the need raised,
//!     the carry-on nudge typed.
//!   * `land-e`: L7 a worker's task (the record `working`): the land act
//!     answered with a clean defense lands it, and the record reads `done
//!     why=landed` (journaled `TASK … done why=landed`), quiet after as L5;
//!     the record reads nothing but `working` before it (no stall while the
//!     worker works) and `status` no need of any kind.
//! * CRUISE (`autopilot=cruise`), `cruise`: C1 a done claim:
//!   `continue-done-check@v1` typed and ledgered, naming no open work and no
//!   repository; C2 the check answered with a `Nothing left:` defense, read
//!   (`EVENT idle … Nothing left:`) and waited on (`WAITING` of that point):
//!   no `DONE`, the task never `landed`, and the next point continues (an
//!   act typed after its back-off).
//! * OFF (`autopilot=off`, every mode), `off`: a done reply, then the
//!   supervisor never on the tab (`supervisor=-`, no journal), nothing typed
//!   (no ledger act, no harness-marked turn in the stub's dumps) for at
//!   least 270 s — past a 4 min first step.
//!
//! Every chain whose replies carry no need is also its own negative control
//! of the needs entry: a `NEEDS` line, an `ask:needs` entry or `needs=ask`
//! there fails its scenario.
//!
//! The report names each observation with its time. A scenario a failed
//! earlier step left unreachable is UNOBSERVED, never a pass. Exit 0 every
//! scenario passed, 1 one failed or went unobserved, 2 usage, 3 could not run.
//! NOT OBSERVED by this lane: a person's own exchange, walls, a question in
//! prose (`land-answer@v1`), the back-off past its fourth step.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_agent::supervise::approvals::{RULE_POINT, RULE_TASK_DONE, TASK_DONE};
use aterm_agent::supervise::policy::needs::NEEDS_ACK;
use aterm_agent::supervise::policy::turn_end;

use crate::door::Door;
use crate::pyjson::J;
use crate::run::{Run, is_run};
use crate::stop;
use crate::util::{now_ms, shell_quote};
use crate::{Fail, LAB, Res};

/// The custom stub mode the lab's conversations are scripted in.
pub const MODE: &str = "forever";

/// How long a reply waits before its first byte: the agent's busy row stands
/// that long, so its loop reads a turn run (an answer the stub sends at once
/// is over before any read sees the worker busy, and no point is judged:
/// measured on Gemini CLI 0.46.0, 2026-10-20).
const REPLY_DELAY_MS: i64 = 3000;
// a reply sent at once is never seen busy
const _: () = assert!(REPLY_DELAY_MS >= 2000);

/// The pause between a reply's deltas, in seconds.
const REPLY_PACE_S: f64 = 0.02;

/// A plain report of progress: no done claim, no defense, no question. It
/// opens with none of Muse Code's tool-row heads (`Ran `, `Wrote `,
/// `Edited `): Muse Code's reader reads a reply that does as the binary's
/// own row, with no last words (measured 2026-10-20, 1.4.10-R6427.1: `Wrote
/// the parser module …` journaled `EVENT idle … -`).
pub const WORK: &str = "Implemented the parser module and its unit tests in this turn.";

/// A done claim (`says_done`), with no defense.
pub const DONE_CLAIM: &str = "Merged the parser and pushed it to origin/main. All done.";

/// A defense's head (`turn_end`'s `Nothing left:` line).
const DEFENSE_HEAD: &str = "Nothing left:";

/// A clean defense: a `Nothing left:` line, no need, no open work.
pub const CLEAN: &str = "Nothing left: the parser is committed and pushed to origin/main.";

/// The need L3 and L6 name.
pub const NEED: &str = "rotate the deploy key";

/// The words a land act opens with (`turn_end::LAND`).
const LAND_KEY: &str = "Autopilot is Land:";
/// …a land nudge (`turn_end::LAND_NUDGE`).
const NUDGE_KEY: &str = "Still landing:";
/// …Cruise's done check (`turn_end::DONE_CHECK`).
const CHECK_KEY: &str = "done is never the end";
/// …Cruise's continuation (`config::CONTINUE_TEXT`).
const CONTINUE_KEY: &str = "Autopilot is Cruise: keep going";

/// The open-work suffix's words (`turn_end::open_work_suffix`).
const OPEN_SUFFIX: &str = "Your report still names open work";
/// The repository suffix's words (`turn_end::repo_work_suffix`).
const REPO_SUFFIX: &str = "Your repository still holds work";

/// How the needs entry's text ends (`escalate::needs_attention_text`).
const CARRYING_ON: &str = "— carrying on";

/// The least a landing is watched for after it (L5, L7).
const QUIET_AFTER_LANDED_MIN_MS: i64 = 90_000;
/// How far past the ladder's next step a landing is watched ([`quiet_after_landed_ms`]).
const PAST_NEXT_STEP_MS: i64 = 60_000;
/// How long after its done reply nothing may be typed under Off: past the
/// 4 min step a first point takes where the agent's fresh screen counted as
/// a turn before it (P3), and half a minute more.
const QUIET_UNDER_OFF_MS: i64 = 270_000;
/// The hang detector: how long a chain may take to reach what it waits for.
const CHAIN_LIMIT_MS: i64 = 16 * 60 * 1000;
/// How near the DONE a ledger row is the DONE's own point (its `point@v1` and
/// `task-done@v1` rows; measured 0–22 ms apart).
const AT_DONE_MS: i64 = 500;

/// The harnesses aterm supervises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Gemini,
    Agy,
    Muse,
}

impl Harness {
    /// Every harness, in the order `all` runs them.
    pub const ALL: [Harness; 4] = [
        Harness::Claude,
        Harness::Gemini,
        Harness::Agy,
        Harness::Muse,
    ];

    /// The verb's word, the lab's `agent`, the reader's program word (the
    /// needs entry's `<program>`: `Program::name`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Gemini => "gemini",
            Harness::Agy => "agy",
            Harness::Muse => "muse",
        }
    }

    fn of(word: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|h| h.name() == word)
    }

    /// The binary and its version, as the lab's `up` recorded them.
    fn bin_ver(self, run: &Run) -> (&str, &str) {
        match self {
            Harness::Claude => (&run.claude_bin, &run.claude_ver),
            Harness::Gemini => (&run.gemini_bin, &run.gemini_ver),
            Harness::Agy => (&run.agy_bin, &run.agy_ver),
            Harness::Muse => (&run.muse_bin, &run.muse_ver),
        }
    }
}

/// Which Autopilot's chains run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Land,
    Cruise,
    Both,
}

/// What a chain's repository holds once laid ([`lay_repo`]), beside its
/// `main`, committed and pushed to `origin`: the five legs of the repository
/// probe (`aterm_agent::harness::repo_work::probe`) one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repo {
    /// Nothing the remote's `main` lacks.
    Clean,
    /// An untracked file, `wip-notes.txt`.
    Untracked,
    /// `README.md` changed and not committed.
    Changed,
    /// A stash entry made on `main` (`WIP on main: …`).
    Stashed,
    /// A commit on `main` no remote holds.
    Unpushed,
    /// A commit on `main` pushed to `origin/wip`, which `origin/main` lacks.
    Unlanded,
}

impl Repo {
    /// What the repository suffix must name of it (`RepoWork::summary`'s
    /// words), and the words of the other legs it must not.
    #[must_use]
    pub fn named(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Repo::Clean => (&[], &[]),
            Repo::Untracked => (
                &["1 untracked (wip-notes.txt)"],
                &["changed", "stash", "unpushed", "not on"],
            ),
            Repo::Changed => (
                &["1 changed (README.md)"],
                &["untracked", "stash", "unpushed", "not on"],
            ),
            Repo::Stashed => (
                &["1 stash"],
                &["changed", "untracked", "unpushed", "not on"],
            ),
            Repo::Unpushed => (
                &["1 unpushed commit", "1 commit not on origin/main"],
                &["changed", "untracked", "stash"],
            ),
            Repo::Unlanded => (
                &["1 commit not on origin/main"],
                &["changed", "untracked", "stash", "unpushed"],
            ),
        }
    }

    /// The repository line of the report.
    fn says(self) -> &'static str {
        match self {
            Repo::Clean => "clean and pushed",
            Repo::Untracked => "wip-notes.txt untracked",
            Repo::Changed => "README.md changed, not committed",
            Repo::Stashed => "a stash entry on main",
            Repo::Unpushed => "a commit no remote holds",
            Repo::Unlanded => "a commit pushed to origin/wip, not on origin/main",
        }
    }
}

/// One tab's scripted conversation: the scenarios it carries, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// L1, L3, L5.
    LandA,
    /// L2.
    LandB,
    /// L4 (untracked).
    LandC,
    /// L6.
    LandD,
    /// L7 (a worker's task).
    LandE,
    /// L8 (changed).
    LandF,
    /// L9 (stash).
    LandG,
    /// L10 (unpushed).
    LandH,
    /// L11 (not on the remote's main).
    LandI,
    /// C1, C2.
    Cruise,
    /// The Off control.
    Off,
}

/// The scenarios, by the names the report and the record use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    L1,
    L2,
    L3,
    L4,
    L5,
    L6,
    L7,
    L8,
    L9,
    L10,
    L11,
    C1,
    C2,
    Off,
}

impl Scenario {
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Scenario::L1 => "L1",
            Scenario::L2 => "L2",
            Scenario::L3 => "L3",
            Scenario::L4 => "L4",
            Scenario::L5 => "L5",
            Scenario::L6 => "L6",
            Scenario::L7 => "L7",
            Scenario::L8 => "L8",
            Scenario::L9 => "L9",
            Scenario::L10 => "L10",
            Scenario::L11 => "L11",
            Scenario::C1 => "C1",
            Scenario::C2 => "C2",
            Scenario::Off => "OFF",
        }
    }

    /// What it holds, for the summary.
    #[must_use]
    pub fn what(self) -> &'static str {
        match self {
            Scenario::L1 => "a work reply: land@v1 typed",
            Scenario::L2 => "a defense beside a leftover: a nudge naming it",
            Scenario::L3 => "NEEDS YOU and a defense: the need raised, carried on",
            Scenario::L4 => "a defense over an untracked file: a nudge naming it",
            Scenario::L5 => "a clean defense, clean repository: landed, then quiet",
            Scenario::L6 => "a tall reply, NEEDS YOU above the window: not landed",
            Scenario::L7 => "a worker's task landed: record done why=landed",
            Scenario::L8 => "a defense over a changed file: a nudge naming it",
            Scenario::L9 => "a defense over a stash entry: a nudge naming it",
            Scenario::L10 => "a defense over an unpushed commit: a nudge naming it",
            Scenario::L11 => "a defense over a commit off origin/main: a nudge naming it",
            Scenario::C1 => "a done claim: continue-done-check@v1",
            Scenario::C2 => "its defense read: never DONE, the next point goes on",
            Scenario::Off => "a done reply under Off: nothing typed, no supervisor",
        }
    }
}

impl Chain {
    pub const ALL: [Chain; 11] = [
        Chain::LandA,
        Chain::LandB,
        Chain::LandC,
        Chain::LandD,
        Chain::LandE,
        Chain::LandF,
        Chain::LandG,
        Chain::LandH,
        Chain::LandI,
        Chain::Cruise,
        Chain::Off,
    ];

    /// The chains a mode runs: the Off control in every mode.
    #[must_use]
    pub fn of_mode(mode: Mode) -> Vec<Chain> {
        Chain::ALL
            .into_iter()
            .filter(|c| match c.autopilot() {
                "land" => mode != Mode::Cruise,
                "cruise" => mode != Mode::Land,
                _ => true,
            })
            .collect()
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Chain::LandA => "land-a",
            Chain::LandB => "land-b",
            Chain::LandC => "land-c",
            Chain::LandD => "land-d",
            Chain::LandE => "land-e",
            Chain::LandF => "land-f",
            Chain::LandG => "land-g",
            Chain::LandH => "land-h",
            Chain::LandI => "land-i",
            Chain::Cruise => "cruise",
            Chain::Off => "off",
        }
    }

    /// The word its conversation carries (its first prompt), by which the
    /// stub's rules know it: no other chain's word holds it.
    #[must_use]
    pub fn token(self) -> String {
        format!("FOREVERLAB-{}", self.name().to_ascii_uppercase())
    }

    /// The Autopilot word its tab is set to.
    #[must_use]
    pub fn autopilot(self) -> &'static str {
        match self {
            Chain::Cruise => "cruise",
            Chain::Off => "off",
            _ => "land",
        }
    }

    #[must_use]
    pub fn scenarios(self) -> &'static [Scenario] {
        match self {
            Chain::LandA => &[Scenario::L1, Scenario::L3, Scenario::L5],
            Chain::LandB => &[Scenario::L2],
            Chain::LandC => &[Scenario::L4],
            Chain::LandD => &[Scenario::L6],
            Chain::LandE => &[Scenario::L7],
            Chain::LandF => &[Scenario::L8],
            Chain::LandG => &[Scenario::L9],
            Chain::LandH => &[Scenario::L10],
            Chain::LandI => &[Scenario::L11],
            Chain::Cruise => &[Scenario::C1, Scenario::C2],
            Chain::Off => &[Scenario::Off],
        }
    }

    /// What its repository holds beside its pushed `main`.
    #[must_use]
    pub fn repo(self) -> Repo {
        match self {
            Chain::LandC => Repo::Untracked,
            Chain::LandF => Repo::Changed,
            Chain::LandG => Repo::Stashed,
            Chain::LandH => Repo::Unpushed,
            Chain::LandI => Repo::Unlanded,
            _ => Repo::Clean,
        }
    }

    /// Its tab carries a worker's task record (`task mark working
    /// from=ctl` before the agent starts): a manager waits on it.
    #[must_use]
    pub fn tasked(self) -> bool {
        self == Chain::LandE
    }

    /// Its landing is the scenario: watched past the ladder's next step
    /// after it, and the stub's dumps read for a new turn after it.
    #[must_use]
    pub fn lands(self) -> bool {
        matches!(self, Chain::LandA | Chain::LandE)
    }

    /// The first prompt, typed as a person types it.
    #[must_use]
    pub fn prompt(self) -> String {
        format!(
            "{} write the parser for this repository and land it",
            self.token()
        )
    }

    /// The stub's rules for its conversation, most specific first
    /// ([`crate::stub::scripted_reply`]).
    #[must_use]
    pub fn rules(self) -> Vec<J> {
        let token = self.token();
        let rule = |last: &str, text: &str| {
            J::obj([
                ("when", J::s(token.as_str())),
                ("last", J::s(last)),
                ("text", J::s(text)),
            ])
        };
        match self {
            Chain::LandA => vec![
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, &needs_defense()),
                rule("", WORK),
            ],
            Chain::LandB => {
                let open = open_defense();
                vec![
                    rule(NUDGE_KEY, &open),
                    rule(LAND_KEY, &open),
                    rule("", WORK),
                ]
            }
            Chain::LandC
            | Chain::LandE
            | Chain::LandF
            | Chain::LandG
            | Chain::LandH
            | Chain::LandI => vec![
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", WORK),
            ],
            Chain::LandD => {
                let tall = tall_reply(80);
                vec![
                    rule(NUDGE_KEY, &tall),
                    rule(LAND_KEY, &tall),
                    rule("", WORK),
                ]
            }
            Chain::Cruise => vec![
                rule(CHECK_KEY, CLEAN),
                rule(CONTINUE_KEY, DONE_CLAIM),
                rule("", DONE_CLAIM),
            ],
            Chain::Off => vec![rule("", DONE_CLAIM)],
        }
    }
}

/// L3's reply: the need, then the defense.
#[must_use]
pub fn needs_defense() -> String {
    format!("NEEDS YOU: {NEED}\n{CLEAN}")
}

/// L2's reply: the defense beside a stated leftover.
#[must_use]
pub fn open_defense() -> String {
    format!("{CLEAN}\nThe docs PR is still open.")
}

/// L6's reply, `lines` tall: a need near its top, the defense at its bottom.
#[must_use]
pub fn tall_reply(lines: usize) -> String {
    let mut s = format!("Here is the landing report.\nNEEDS YOU: {NEED}\n");
    for i in 1..=lines {
        s.push_str(&format!("- check {i} passed\n"));
    }
    s.push_str(CLEAN);
    s
}

/// The custom mode's spec: every chain's rules, each reply after
/// [`REPLY_DELAY_MS`], and the plain work reply for a turn no rule picks.
#[must_use]
pub fn spec(chains: &[Chain]) -> J {
    J::obj([
        ("stream", J::Bool(true)),
        ("delay_ms", J::Int(REPLY_DELAY_MS)),
        ("pace_s", J::Float(REPLY_PACE_S)),
        ("text", J::s(WORK)),
        (
            "replies",
            J::Arr(chains.iter().flat_map(|c| c.rules()).collect()),
        ),
    ])
}

/// What a scenario came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass(String),
    Fail(String),
    Unobserved(String),
}

impl Verdict {
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Verdict::Pass(_) => "PASS",
            Verdict::Fail(_) => "FAIL",
            Verdict::Unobserved(_) => "UNOBSERVED",
        }
    }

    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Verdict::Pass(w) | Verdict::Fail(w) | Verdict::Unobserved(w) => w,
        }
    }
}

/// One ledger row as the harness wrote it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub ts: i64,
    pub rule: String,
    pub decision: String,
    pub command: String,
    pub reason: String,
}

impl Row {
    /// The row records something done to the agent: a key pressed or a text
    /// typed (`approved`, `declined`, `typed`), or one tried (`escalated`,
    /// `deferred`) — every decision but a record (`waiting`) or a skip.
    #[must_use]
    pub fn acted(&self) -> bool {
        !matches!(self.decision.as_str(), "waiting" | "skipped")
    }
}

/// What the stub's request dumps said of a conversation ([`StubSeen::of`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StubSeen {
    /// The harness-marked user texts the conversation held at the landing
    /// (Off: none).
    pub baseline: usize,
    /// Its requests read (after the landing; Off: all of them).
    pub read: usize,
    /// Of those, the ones holding more harness-marked user texts than
    /// [`Self::baseline`]: a new turn of the harness's.
    pub new_turns: usize,
}

/// What one tab's records said, each with its time (ms), in order: the
/// journal's lines, the ledger's rows, and the `status`, `meta` and `task`
/// words at each change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub journal: Vec<(i64, String)>,
    pub ledger: Vec<Row>,
    /// `task=… needs=… autopilot=… supervisor=…` of `status`.
    pub status: Vec<(i64, String)>,
    /// `attention_owners=… attention_kinds=… attention=…` of `meta`, the
    /// attention decoded.
    pub meta: Vec<(i64, String)>,
    /// The `task` record's line.
    pub task: Vec<(i64, String)>,
    /// When the reply the chain watches from showed (Off: its done reply).
    pub reply_ms: Option<i64>,
    /// When watching stopped.
    pub end_ms: i64,
    /// The stub's witness (L5, L7: after the landing; Off: the whole
    /// conversation); `None` where it was never read.
    pub stub: Option<StubSeen>,
}

/// A journal line's act: `CONTINUED`/`TYPED seq=… rule=<rule> …`.
fn act_rule(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("CONTINUED ")
        .or_else(|| line.strip_prefix("TYPED "))?;
    rest.split_whitespace()
        .find_map(|w| w.strip_prefix("rule="))
}

/// An `EVENT idle seq=<n> <words>` line's words (`-` for none read).
fn idle_words(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("EVENT idle ")?;
    let (seq, words) = rest.split_once(' ').unwrap_or((rest, ""));
    seq.starts_with("seq=").then_some(words.trim())
}

/// A point's yield as the journal read it: the first `EVENT idle` line
/// with words after the act, and the `WAITING` the loop decided at that
/// point (the same `seq=`).
#[derive(Clone, Copy, Debug, Default)]
struct Yield<'a> {
    event: Option<&'a (i64, String)>,
    waiting: Option<&'a (i64, String)>,
}

impl Seen {
    /// The journal's acts of `rule`, `(t, line)`.
    fn acts(&self, rule: &str) -> Vec<&(i64, String)> {
        self.journal
            .iter()
            .filter(|(_, l)| act_rule(l) == Some(rule))
            .collect()
    }

    /// Every act the journal says was typed, any rule.
    fn all_acts(&self) -> Vec<&(i64, String)> {
        self.journal
            .iter()
            .filter(|(_, l)| act_rule(l).is_some())
            .collect()
    }

    /// The first journal line opening with `head`, after `t`.
    fn first_after(&self, head: &str, t: i64) -> Option<&(i64, String)> {
        self.journal
            .iter()
            .find(|(at, l)| *at > t && l.starts_with(head))
    }

    /// The ledger's typed row of `rule` nearest after `t` (its full text).
    fn typed_row(&self, rule: &str, t: i64) -> Option<&Row> {
        self.ledger
            .iter()
            .filter(|r| r.rule == rule && r.decision == "typed" && r.ts >= t - 2000)
            .min_by_key(|r| r.ts)
    }

    /// The text an act at `t` typed: its ledger row's, else the journal's line.
    fn act_text<'a>(&'a self, rule: &str, at: &'a (i64, String)) -> &'a str {
        self.typed_row(rule, at.0)
            .map_or(at.1.as_str(), |r| r.command.as_str())
    }

    /// A `status` word as it stood last.
    fn last_status(&self, key: &str) -> Option<String> {
        self.status.last().and_then(|(_, s)| word(s, key))
    }

    /// Whether `status` ever said `key=value`.
    fn status_said(&self, key: &str, value: &str, after: i64) -> bool {
        self.status
            .iter()
            .any(|(t, s)| *t >= after && word(s, key).as_deref() == Some(value))
    }

    /// The yield of the act at `t`, read before `until` (the next act): its
    /// first `EVENT idle` with words — words holding `holding`, where given.
    fn yield_after(&self, t: i64, until: Option<i64>, holding: Option<&str>) -> Yield<'_> {
        let before = |at: i64| until.is_none_or(|u| at < u);
        let event = self.journal.iter().find(|(at, l)| {
            *at > t
                && before(*at)
                && idle_words(l).is_some_and(|w| {
                    !w.is_empty() && w != "-" && holding.is_none_or(|h| w.contains(h))
                })
        });
        let waiting = event.and_then(|(et, el)| {
            let seq = word(el, "seq");
            self.journal.iter().find(|(at, l)| {
                *at >= *et && before(*at) && l.starts_with("WAITING ") && word(l, "seq") == seq
            })
        });
        Yield { event, waiting }
    }

    /// What says a need was raised on this tab, if anything: a `NEEDS`
    /// line, an `ask:needs` entry in `meta`, `needs=ask` in `status`.
    fn needs_seen(&self) -> Option<String> {
        if let Some((_, l)) = self.journal.iter().find(|(_, l)| l.starts_with("NEEDS ")) {
            return Some(format!("journal {}", crate::util::one_line(l, 100)));
        }
        if let Some((_, m)) = self.meta.iter().find(|(_, m)| needs_up(m)) {
            return Some(format!("meta {}", crate::util::one_line(m, 120)));
        }
        self.status
            .iter()
            .find(|(_, s)| word(s, "needs").as_deref() == Some("ask"))
            .map(|(_, s)| format!("status {s}"))
    }
}

/// The value of `key=` among `text`'s words.
#[must_use]
pub fn word(text: &str, key: &str) -> Option<String> {
    text.split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
        .map(str::to_string)
}

/// `%XX` decoded (the control protocol's pct-encoding); a bad escape kept.
#[must_use]
pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(h) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// When a time is said, relative to the watch's start `t0`.
fn at(t0: i64, t: i64) -> String {
    crate::restore::since(t0, t)
}

/// The short turns in a row the point at `t` closed: its `point@v1` row's
/// mark (`… (short turns in a row: N)`), else the last `WAITING` before it
/// (`N short turn(s) in a row`) and one more.
fn streak_at(seen: &Seen, t: i64) -> Option<u32> {
    let marked = seen
        .ledger
        .iter()
        .rfind(|r| r.rule == RULE_POINT && (r.ts - t).abs() <= AT_DONE_MS)
        .and_then(|r| {
            let (_, n) = r.reason.split_once("short turns in a row: ")?;
            n.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
        });
    marked.or_else(|| {
        seen.journal
            .iter()
            .rfind(|(at, l)| *at < t && l.starts_with("WAITING "))
            .and_then(|(_, l)| {
                let (_, rest) = l.split_once(" until=")?;
                let n: u32 = rest.split_whitespace().nth(1)?.parse().ok()?;
                n.checked_add(1)
            })
    })
}

/// The ladder's next step after a point that closed `streak` short turns:
/// the wait before the next act, were the ladder still going (none for a
/// worker its manager waits on, through its first
/// [`turn_end::MANAGED_SHORT_TURNS`]).
#[must_use]
pub fn next_step_ms(streak: u32, managed: bool) -> i64 {
    if managed && streak <= turn_end::MANAGED_SHORT_TURNS {
        return 0;
    }
    let t = turn_end::TurnEndTiming::default();
    let d = aterm_agent::supervise::ladder::doubling(
        t.short_backoff,
        streak.saturating_sub(1),
        t.short_backoff_max,
    );
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// How long the landing at `t` is watched for after it (L5, L7): through
/// the ladder's next step and [`PAST_NEXT_STEP_MS`] more, at least
/// [`QUIET_AFTER_LANDED_MIN_MS`].
#[must_use]
pub fn quiet_after_landed_ms(seen: &Seen, t: i64, managed: bool) -> i64 {
    let step = next_step_ms(streak_at(seen, t).unwrap_or(1), managed);
    step.saturating_add(PAST_NEXT_STEP_MS)
        .max(QUIET_AFTER_LANDED_MIN_MS)
}

/// THE JUDGE of one chain's records `seen` (its `program`: the reader's word
/// the needs entry names), `t0` the first prompt's time: each of its
/// scenarios' verdict, in order.
#[must_use]
pub fn judge(chain: Chain, seen: &Seen, program: &str, t0: i64) -> Vec<(Scenario, Verdict)> {
    let done = seen.first_after("DONE ", t0);
    let land = seen.acts(turn_end::RULE_LAND).first().copied();
    let nudges = seen.acts(turn_end::RULE_LAND_NUDGE);
    let land_nudge_after = |t: i64| nudges.iter().copied().find(|(at, _)| *at > t);
    let mut out = Vec::new();
    for &s in chain.scenarios() {
        let v = match s {
            Scenario::L1 => judge_land_act(seen, land, done, t0),
            Scenario::L2 => judge_nudge(seen, land, &land_nudge_after, done, &Want::open(), t0),
            Scenario::L4 | Scenario::L8 | Scenario::L9 | Scenario::L10 | Scenario::L11 => {
                let v = judge_nudge(
                    seen,
                    land,
                    &land_nudge_after,
                    done,
                    &Want::repo(chain.repo()),
                    t0,
                );
                // the live negative control of L5's judge: it fails these records
                match (v, judge_landed(seen, land, &land_nudge_after, false, t0)) {
                    (Verdict::Pass(w), Verdict::Fail(_)) => Verdict::Pass(format!(
                        "{w}; L5's judge over these records: FAIL, as it must"
                    )),
                    (Verdict::Pass(w), l5) => Verdict::Fail(format!(
                        "{w}; but L5's judge over these records says {}: {}",
                        l5.word(),
                        l5.why()
                    )),
                    (v, _) => v,
                }
            }
            Scenario::L3 => judge_needs(seen, land, &land_nudge_after, program, t0),
            Scenario::L5 => judge_landed(seen, land, &land_nudge_after, false, t0),
            Scenario::L6 => judge_tall(seen, land, done, t0),
            Scenario::L7 => judge_tasked(seen, land, t0),
            Scenario::C1 => judge_check(seen, t0),
            Scenario::C2 => judge_cruise_goes_on(seen, done, t0),
            Scenario::Off => judge_off(seen, t0),
        };
        out.push((s, v));
    }
    out
}

/// L1: the work reply's point typed the land act, and the ledger kept its text.
fn judge_land_act(
    seen: &Seen,
    land: Option<&(i64, String)>,
    done: Option<&(i64, String)>,
    t0: i64,
) -> Verdict {
    match land {
        Some((t, _)) if done.is_some_and(|(d, _)| d < t) => {
            Verdict::Fail("a DONE came before the land instruction".into())
        }
        Some((t, _)) => match seen.typed_row(turn_end::RULE_LAND, *t) {
            Some(_) => Verdict::Pass(format!(
                "land@v1 typed at +{}, ledger row typed",
                at(t0, *t)
            )),
            None => Verdict::Fail(format!(
                "land@v1 journaled at +{}, but NO ledger row typed",
                at(t0, *t)
            )),
        },
        None => Verdict::Fail(format!("no land@v1 typed within {}", at(t0, seen.end_ms))),
    }
}

/// What a nudge must say (L2, L4, L8–L11), and what it must not.
struct Want {
    /// The suffix the report quotes from, which the nudge must carry.
    head: &'static str,
    /// The words it must carry after `head`.
    named: &'static [&'static str],
    /// Words it must not carry after `head` (another leg's).
    not_named: &'static [&'static str],
    /// Words it must not carry anywhere (another suffix).
    not_anywhere: [&'static str; 2],
}

impl Want {
    /// L2: the open-work suffix, no repository suffix, no needs carry-on.
    fn open() -> Want {
        Want {
            head: OPEN_SUFFIX,
            named: &[],
            not_named: &[],
            not_anywhere: [REPO_SUFFIX, NEEDS_ACK],
        }
    }

    /// L4, L8–L11: the repository suffix naming `repo`'s leg and no other
    /// leg, no open-work suffix, no needs carry-on.
    fn repo(repo: Repo) -> Want {
        let (named, not_named) = repo.named();
        Want {
            head: REPO_SUFFIX,
            named,
            not_named,
            not_anywhere: [OPEN_SUFFIX, NEEDS_ACK],
        }
    }
}

/// L2, L4, L8–L11: the land act's defense names open work (L2) or meets a
/// repository holding work (the others): a nudge whose text says `want`, its
/// yield read and waited on, no landing, no need raised.
fn judge_nudge<'a>(
    seen: &'a Seen,
    land: Option<&(i64, String)>,
    nudge_after: &dyn Fn(i64) -> Option<&'a (i64, String)>,
    done: Option<&(i64, String)>,
    want: &Want,
    t0: i64,
) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved(
            "no land instruction was typed, so no defense answered one".into(),
        );
    };
    if let Some((d, _)) = done {
        return Verdict::Fail(format!("LANDED at +{}: {}", at(t0, *d), done_line(done)));
    }
    if seen.status_said("task", "landed", *t1) {
        return Verdict::Fail("status said task=landed".into());
    }
    if let Some(n) = seen.needs_seen() {
        return Verdict::Fail(format!("a need was raised, though no reply named one: {n}"));
    }
    let Some(nudge) = nudge_after(*t1) else {
        return Verdict::Unobserved(format!(
            "not landed, but no land-nudge@v1 typed within {} (the back-off not waited out?)",
            at(t0, seen.end_ms)
        ));
    };
    let t2 = nudge.0;
    let text = seen.act_text(turn_end::RULE_LAND_NUDGE, nudge);
    let suffix = text.find(want.head).map_or("", |i| &text[i..]);
    let missing: Vec<&str> = std::iter::once(want.head)
        .chain(want.named.iter().copied())
        .filter(|w| !suffix.contains(w))
        .collect();
    let wrong: Vec<&str> = want
        .not_named
        .iter()
        .copied()
        .filter(|w| suffix.contains(w))
        .chain(want.not_anywhere.into_iter().filter(|w| text.contains(w)))
        .collect();
    if !missing.is_empty() || !wrong.is_empty() {
        return Verdict::Fail(format!(
            "the nudge at +{} does not say {missing:?}{}: {}",
            at(t0, t2),
            if wrong.is_empty() {
                String::new()
            } else {
                format!(" or says {wrong:?}")
            },
            crate::util::one_line(text, 400)
        ));
    }
    let said = format!(
        "land-nudge@v1 typed at +{} naming \"{}\", no DONE, task never landed, no need raised",
        at(t0, t2),
        crate::util::one_line(suffix, 200)
    );
    let y = seen.yield_after(t2, None, None);
    match y {
        Yield {
            event: Some((te, el)),
            waiting: Some((tw, wl)),
        } => Verdict::Pass(format!(
            "{said}; its yield read at +{} ({}), +{} {}",
            at(t0, *te),
            crate::util::one_line(el, 80),
            at(t0, *tw),
            crate::util::one_line(wl, 100)
        )),
        Yield {
            event: Some((te, el)),
            waiting: None,
        } => Verdict::Fail(format!(
            "{said}; its yield read at +{} ({}) but no WAITING decided for that point within {}",
            at(t0, *te),
            crate::util::one_line(el, 80),
            at(t0, seen.end_ms)
        )),
        Yield { event: None, .. } => Verdict::Unobserved(format!(
            "{said}; the nudge's yield was not read within {}",
            at(t0, seen.end_ms)
        )),
    }
}

fn done_line(done: Option<&(i64, String)>) -> String {
    done.map_or_else(String::new, |(_, l)| crate::util::one_line(l, 120))
}

/// Whether a `meta` list (`a,b,…`, decoded) holds `item`.
fn listed(list: &str, item: &str) -> bool {
    list.split([',', ' ']).any(|x| x == item)
}

/// Whether a `meta` snapshot holds owner `needs`'s entry as an ask
/// (`attention_kinds=` names `ask:needs`; `attention_owners=` is a count).
fn needs_up(m: &str) -> bool {
    word(m, "attention_kinds").is_some_and(|k| listed(&k, "ask:needs"))
}

/// The needs entry at or after `t`: the first `meta` snapshot that holds it.
fn needs_entry(seen: &Seen, t: i64) -> Option<&(i64, String)> {
    seen.meta.iter().find(|(at, m)| *at >= t && needs_up(m))
}

/// The attention text a `meta` snapshot carries (after `attention=`, to the end).
fn attention_text(m: &str) -> &str {
    m.split_once("attention=").map_or("", |(_, t)| t)
}

/// L3: the land act answered with a need and a defense: `NEEDS` journaled,
/// the needs entry up with the program's word and the carry-on, `status
/// needs=ask` while it stands, a nudge typed that tells the worker its need
/// reached the person (and names no open work or repository), no landing
/// before it.
fn judge_needs<'a>(
    seen: &'a Seen,
    land: Option<&(i64, String)>,
    nudge_after: &dyn Fn(i64) -> Option<&'a (i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed (L1 failed)".into());
    };
    let needs = seen.first_after("NEEDS ", *t1);
    let entry = needs_entry(seen, *t1);
    let nudge = nudge_after(*t1);
    let landed_first = seen
        .first_after("DONE ", *t1)
        .is_some_and(|(d, _)| nudge.is_none_or(|(n, _)| d < n));
    let want_text = format!("{program} needs you: {NEED}");
    let text_ok = entry.is_some_and(|(_, m)| {
        let t = attention_text(m);
        t.starts_with(&want_text) && t.trim_end().ends_with(CARRYING_ON)
    });
    // `needs=ask` while the entry stands: from its raise to the landing
    let until = seen.first_after("DONE ", *t1).map_or(i64::MAX, |(d, _)| *d);
    let ask = entry.and_then(|(te, _)| {
        seen.status.iter().find(|(t, s)| {
            *t >= *te - 5000 && *t <= until && word(s, "needs").as_deref() == Some("ask")
        })
    });
    let nudge_text = nudge.map(|n| seen.act_text(turn_end::RULE_LAND_NUDGE, n));
    let carry_on = nudge_text.is_some_and(|t| t.contains(NEEDS_ACK));
    let stray: Vec<&str> = [OPEN_SUFFIX, REPO_SUFFIX]
        .into_iter()
        .filter(|w| nudge_text.is_some_and(|t| t.contains(w)))
        .collect();
    let mut facts = vec![
        needs.map_or_else(
            || "NO NEEDS line".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 100)),
        ),
        entry.map_or_else(
            || "NO needs entry (meta attention_kinds ask:needs)".to_string(),
            |(t, m)| format!("+{} meta {}", at(t0, *t), crate::util::one_line(m, 200)),
        ),
        ask.map_or_else(
            || "NO status needs=ask while it stood".to_string(),
            |(t, _)| format!("+{} status needs=ask", at(t0, *t)),
        ),
        nudge.map_or_else(
            || "NO nudge typed".to_string(),
            |(t, _)| {
                format!(
                    "land-nudge@v1 typed at +{}{}",
                    at(t0, *t),
                    if carry_on {
                        " telling the worker its need is on the person's screen"
                    } else {
                        " WITHOUT the needs carry-on"
                    }
                )
            },
        ),
    ];
    if landed_first {
        facts.push("LANDED before the nudge".into());
    }
    if !stray.is_empty() {
        facts.push(format!("the nudge says {stray:?}"));
    }
    let said = facts.join("; ");
    let raised = needs.is_some() && entry.is_some();
    if raised && text_ok && ask.is_some() && carry_on && stray.is_empty() && !landed_first {
        Verdict::Pass(said)
    } else if raised && nudge.is_none() && !landed_first {
        Verdict::Unobserved(format!("{said} (the carry-on not waited out)"))
    } else if entry.is_some() && !text_ok {
        Verdict::Fail(format!(
            "{said}; the entry does not read \"{want_text} … {CARRYING_ON}\""
        ))
    } else {
        Verdict::Fail(said)
    }
}

/// THE LANDING AT `t3` AND ITS QUIET (L5, L7): what is wrong with it, and
/// what it said: `DONE … rule=land@v1`, `task=landed`, the ladder over (no
/// `WAITING` or `until=` journaled after the DONE, no ledger row after its
/// own point's but `point@v1` and `task-done@v1` records, the last row
/// `task-done@v1` — [`TASK_DONE`]), nothing typed after it (no journal act,
/// no ledger act, no new harness turn in the stub's dumps), and how long it
/// was watched against [`quiet_after_landed_ms`].
fn landing_quiet(
    seen: &Seen,
    (t3, line): &(i64, String),
    managed: bool,
    t0: i64,
) -> (Vec<String>, String, i64) {
    let t3 = *t3;
    let mut bad = Vec::new();
    if !line.contains(&format!("rule={}", turn_end::RULE_LAND)) {
        bad.push(format!("the DONE line names another rule: {line}"));
    }
    let landed = seen.status_said("task", "landed", t3 - 5000);
    if !landed {
        bad.push("status never said task=landed".into());
    }
    let after: Vec<&(i64, String)> = seen
        .journal
        .iter()
        .filter(|(t, l)| *t >= t3 && !std::ptr::eq(l, line))
        .collect();
    let ladder: Vec<&(i64, String)> = after
        .iter()
        .copied()
        .filter(|(_, l)| l.starts_with("WAITING ") || l.contains(" until="))
        .collect();
    if !ladder.is_empty() {
        bad.push(format!(
            "the ladder went on after the landing: {}",
            crate::util::one_line(&ladder[0].1, 140)
        ));
    }
    let typed_after: Vec<&&(i64, String)> = after
        .iter()
        .filter(|(_, l)| act_rule(l).is_some())
        .collect();
    let rows_at: Vec<&Row> = seen
        .ledger
        .iter()
        .filter(|r| r.ts >= t3 - AT_DONE_MS)
        .collect();
    let acted_after = rows_at.iter().filter(|r| r.acted()).count();
    let stray_rows: Vec<String> = rows_at
        .iter()
        .filter(|r| {
            !(r.decision == "waiting" && (r.rule == RULE_POINT || r.rule == RULE_TASK_DONE))
        })
        .map(|r| format!("{} {}", r.rule, r.decision))
        .collect();
    if !typed_after.is_empty() || acted_after > 0 {
        bad.push(format!(
            "TYPED after the landing: {} journal act(s), {acted_after} ledger act(s)",
            typed_after.len()
        ));
    }
    if !stray_rows.is_empty() {
        bad.push(format!(
            "ledger rows after the landing's point: {stray_rows:?}"
        ));
    }
    let last = seen.ledger.last();
    let ends_done = last.is_some_and(|r| r.rule == RULE_TASK_DONE && r.reason == TASK_DONE);
    if !ends_done {
        bad.push(format!(
            "the ledger's last row is not {RULE_TASK_DONE} \"{TASK_DONE}\": {}",
            last.map_or_else(
                || "none".to_string(),
                |r| format!("{} {} {}", r.rule, r.decision, r.reason)
            )
        ));
    }
    match &seen.stub {
        Some(s) if s.new_turns > 0 => bad.push(format!(
            "the stub saw {} new harness turn(s) of the conversation after the landing",
            s.new_turns
        )),
        _ => {}
    }
    let window = quiet_after_landed_ms(seen, t3, managed);
    let quiet = seen.end_ms - t3;
    let said = format!(
        "DONE at +{} ({}); status task=landed {}; the ladder {} (last ledger row {}); nothing typed for {} after, the window {} (the ladder's next step after streak {}: {}, and {} s more, at least {} s); the stub {}",
        at(t0, t3),
        crate::util::one_line(line, 90),
        if landed { "said" } else { "NOT said" },
        if ladder.is_empty() { "over" } else { "NOT over" },
        last.map_or("none", |r| r.rule.as_str()),
        at(0, quiet),
        at(0, window),
        streak_at(seen, t3).map_or_else(|| "unread".to_string(), |n| n.to_string()),
        match next_step_ms(streak_at(seen, t3).unwrap_or(1), managed) {
            0 => "at once (a managed worker's)".to_string(),
            ms => at(0, ms),
        },
        PAST_NEXT_STEP_MS / 1000,
        QUIET_AFTER_LANDED_MIN_MS / 1000,
        seen.stub.as_ref().map_or_else(
            || "UNREAD".to_string(),
            |s| format!(
                "read {} request(s) of the conversation after it, {} with a new harness turn (it held {} harness turn(s) at the landing)",
                s.read, s.new_turns, s.baseline
            )
        )
    );
    (bad, said, window)
}

/// L5: the nudge answered with a clean defense over a clean repository:
/// the landing and its quiet ([`landing_quiet`]), the needs entry cleared,
/// the nudge naming no open work or repository, and no task record where
/// none was given (`OK task -` throughout: the record's leg is L7's).
fn judge_landed<'a>(
    seen: &'a Seen,
    land: Option<&(i64, String)>,
    nudge_after: &dyn Fn(i64) -> Option<&'a (i64, String)>,
    managed: bool,
    t0: i64,
) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed (L1 failed)".into());
    };
    let Some(nudge) = nudge_after(*t1) else {
        return Verdict::Unobserved(
            "no nudge was typed for the clean defense to answer (L3)".into(),
        );
    };
    let t2 = nudge.0;
    let Some(done) = seen.first_after("DONE ", t2) else {
        return Verdict::Fail(format!(
            "no DONE within {} after the nudge's clean defense",
            at(t0, seen.end_ms)
        ));
    };
    let (mut bad, said, window) = landing_quiet(seen, done, managed, t0);
    let text = seen.act_text(turn_end::RULE_LAND_NUDGE, nudge);
    for w in [OPEN_SUFFIX, REPO_SUFFIX] {
        if text.contains(w) {
            bad.push(format!("the nudge that landed it says \"{w}\""));
        }
    }
    let records: Vec<&str> = seen
        .task
        .iter()
        .map(|(_, r)| r.trim())
        .filter(|r| *r != "OK task -")
        .collect();
    if let Some(r) = records.first() {
        bad.push(format!("a task record where none was given: {r}"));
    }
    let cleared = seen
        .journal
        .iter()
        .any(|(t, l)| *t >= t2 && l.starts_with("CLEARED ") && l.contains(" needs "));
    let entry_gone = seen.meta.last().is_some_and(|(_, m)| !needs_up(m));
    if !cleared {
        bad.push("no CLEARED … needs line".into());
    }
    if !entry_gone {
        bad.push("the needs entry still stands in meta".into());
    }
    let said = format!(
        "{said}; needs {}; no task given, the record {}",
        if cleared && entry_gone {
            "cleared"
        } else {
            "NOT cleared"
        },
        if seen.task.is_empty() {
            "not read"
        } else if records.is_empty() {
            "OK task - throughout"
        } else {
            "INVENTED"
        }
    );
    landed_verdict(bad, said, seen, done.0, window)
}

/// A landing's verdict from what is wrong with it: FAIL on anything, then
/// UNOBSERVED where the stub's witness went unread or the watch ended inside
/// `window`, else PASS.
fn landed_verdict(bad: Vec<String>, said: String, seen: &Seen, t3: i64, window: i64) -> Verdict {
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if seen.stub.is_none() {
        return Verdict::Unobserved(format!("{said}: the stub's dumps were not read"));
    }
    if seen.end_ms - t3 < window {
        return Verdict::Unobserved(format!(
            "{said}: watched under the window ({}) after the landing",
            at(0, window)
        ));
    }
    Verdict::Pass(said)
}

/// L7: a worker's task (its record `working`, given before the agent
/// started) landed by a clean defense at the land act's yield: the landing
/// and its quiet ([`landing_quiet`]), `TASK … done why=landed` journaled,
/// the record `state=done why=landed` after it, no need raised.
fn judge_tasked(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Verdict {
    let given = seen
        .task
        .iter()
        .find(|(_, r)| r.contains("state=working"))
        .map(|(t, _)| *t);
    let Some(given) = given else {
        return Verdict::Unobserved(format!(
            "the task record never read working (the mark did not take): {}",
            seen.task.first().map_or("not read", |(_, r)| r.as_str())
        ));
    };
    let Some((t1, _)) = land else {
        return Verdict::Fail(format!(
            "the record working at +{}, but no land@v1 typed within {}",
            at(t0, given),
            at(t0, seen.end_ms)
        ));
    };
    let Some(done) = seen.first_after("DONE ", *t1) else {
        return Verdict::Fail(format!(
            "no DONE within {} after the land act's clean defense",
            at(t0, seen.end_ms)
        ));
    };
    let (mut bad, said, window) = landing_quiet(seen, done, true, t0);
    let marked = seen.journal.iter().find(|(t, l)| {
        *t >= done.0 - 1000 && l.starts_with("TASK ") && l.contains("done why=landed")
    });
    if marked.is_none() {
        bad.push("no TASK … done why=landed journaled".into());
    }
    let record = seen
        .task
        .iter()
        .find(|(t, r)| *t >= done.0 - 5000 && r.contains("state=done"));
    match record {
        Some((_, r)) if word(r, "why").as_deref() == Some("landed") => {}
        Some((_, r)) => bad.push(format!("the record is done, but not why=landed: {r}")),
        None => bad.push(format!(
            "the record never read state=done after the landing: {}",
            seen.task.last().map_or("not read", |(_, r)| r.as_str())
        )),
    }
    if let Some(r) = seen.task.last().map(|(_, r)| r.as_str())
        && !(r.contains("state=done") && word(r, "why").as_deref() == Some("landed"))
    {
        bad.push(format!("the record did not stay done why=landed: {r}"));
    }
    if let Some(n) = seen.needs_seen() {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    // The worker worked from its first reply to the landing: its record
    // reads working, then done, and nothing stands for a person between.
    if let Some((t, r)) = seen
        .task
        .iter()
        .find(|(_, r)| !r.contains("state=working") && !r.contains("state=done"))
    {
        bad.push(format!(
            "the record read state={} at +{} while its worker worked: {}",
            word(r, "state").unwrap_or_default(),
            at(t0, *t),
            crate::util::one_line(r, 200)
        ));
    }
    if let Some((t, s)) = seen
        .status
        .iter()
        .find(|(t, s)| *t < done.0 && word(s, "needs").is_some_and(|n| n != "-"))
    {
        bad.push(format!(
            "status read needs={} at +{}, though no reply named a need",
            word(s, "needs").unwrap_or_default(),
            at(t0, *t)
        ));
    }
    let said = format!(
        "the record working at +{}, land@v1 typed at +{}; {said}; {}; record {}",
        at(t0, given),
        at(t0, *t1),
        marked.map_or_else(
            || "NO TASK line".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 120))
        ),
        record.map_or("NOT done", |(_, r)| r.as_str())
    );
    landed_verdict(bad, said, seen, done.0, window)
}

/// L6: a reply taller than the window, a need at its top and the defense at
/// its bottom: no landing, the need raised, and the worker carried on — the
/// nudge after the need typed, telling it the need reached the person.
fn judge_tall(
    seen: &Seen,
    land: Option<&(i64, String)>,
    done: Option<&(i64, String)>,
    t0: i64,
) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed".into());
    };
    let needs = seen.first_after("NEEDS ", *t1);
    let entry = needs_entry(seen, *t1);
    let nudge = seen
        .acts(turn_end::RULE_LAND_NUDGE)
        .into_iter()
        .find(|(t, _)| t > t1);
    let carry_on = nudge.is_some_and(|n| {
        seen.act_text(turn_end::RULE_LAND_NUDGE, n)
            .contains(NEEDS_ACK)
    });
    let said = format!(
        "{}; {}; {}",
        needs.map_or_else(
            || "NO NEEDS line".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 100))
        ),
        entry.map_or("no needs entry", |_| "the needs entry up"),
        nudge.map_or_else(
            || "no nudge typed".to_string(),
            |(t, _)| format!(
                "land-nudge@v1 typed at +{}{}",
                at(t0, *t),
                if carry_on {
                    " with the needs carry-on"
                } else {
                    " WITHOUT the needs carry-on"
                }
            )
        )
    );
    if let Some((d, l)) = done {
        return Verdict::Fail(format!(
            "LANDED at +{} over the need above the window ({}): {said}",
            at(t0, *d),
            crate::util::one_line(l, 90)
        ));
    }
    if seen.status_said("task", "landed", t0) {
        return Verdict::Fail(format!("status said task=landed: {said}"));
    }
    if needs.is_none() || entry.is_none() {
        return Verdict::Fail(format!("not landed, but the need was not raised: {said}"));
    }
    match nudge {
        Some(_) if carry_on => Verdict::Pass(format!("not landed; {said}")),
        Some(_) => Verdict::Fail(format!("not landed, need raised, but {said}")),
        None => Verdict::Unobserved(format!(
            "not landed, need raised, but the carry-on not observed within {}: {said}",
            at(t0, seen.end_ms)
        )),
    }
}

/// C1: the done claim's point typed Cruise's done check, ledgered, naming
/// no open work (the claim names none) and no repository (it is clean), no
/// need raised.
fn judge_check(seen: &Seen, t0: i64) -> Verdict {
    let Some(check) = seen.acts(turn_end::RULE_DONE_CHECK).first().copied() else {
        return Verdict::Fail(format!(
            "no continue-done-check@v1 typed within {}",
            at(t0, seen.end_ms)
        ));
    };
    let Some(row) = seen.typed_row(turn_end::RULE_DONE_CHECK, check.0) else {
        return Verdict::Fail(format!(
            "continue-done-check@v1 journaled at +{}, but NO ledger row typed",
            at(t0, check.0)
        ));
    };
    let stray: Vec<&str> = [OPEN_SUFFIX, REPO_SUFFIX, NEEDS_ACK]
        .into_iter()
        .filter(|w| row.command.contains(w))
        .collect();
    if !stray.is_empty() {
        return Verdict::Fail(format!(
            "the check at +{} says {stray:?}: {}",
            at(t0, check.0),
            crate::util::one_line(&row.command, 400)
        ));
    }
    if let Some(n) = seen.needs_seen() {
        return Verdict::Fail(format!("a need was raised, though no reply named one: {n}"));
    }
    Verdict::Pass(format!(
        "continue-done-check@v1 typed at +{}, ledger row typed, no suffix",
        at(t0, check.0)
    ))
}

/// C2: the done check answered with a defense, READ (an `EVENT idle` line
/// after the check whose words carry `Nothing left:`, before the next act)
/// and WAITED ON (that point's `WAITING`): never `DONE`, never
/// `task=landed`, no need raised, and the next point continues (an act
/// typed after the back-off).
fn judge_cruise_goes_on(seen: &Seen, done: Option<&(i64, String)>, t0: i64) -> Verdict {
    let checks = seen.acts(turn_end::RULE_DONE_CHECK);
    let Some((t1, _)) = checks.first() else {
        return Verdict::Unobserved("no done check was typed (C1 failed)".into());
    };
    if let Some((d, l)) = done {
        return Verdict::Fail(format!(
            "ENDED at +{}: {}",
            at(t0, *d),
            crate::util::one_line(l, 90)
        ));
    }
    if seen.status_said("task", "landed", t0) {
        return Verdict::Fail("status said task=landed under Cruise".into());
    }
    if let Some(n) = seen.needs_seen() {
        return Verdict::Fail(format!("a need was raised, though no reply named one: {n}"));
    }
    let again = seen.all_acts().into_iter().find(|(t, _)| t > t1);
    let y = seen.yield_after(*t1, again.map(|(t, _)| *t), Some(DEFENSE_HEAD));
    let task = seen.last_status("task").unwrap_or_else(|| "?".into());
    let Some((te, el)) = y.event else {
        return Verdict::Unobserved(format!(
            "no DONE, task={task}, but the check's defense was never read (no EVENT idle … {DEFENSE_HEAD} after it{})",
            again.map_or_else(String::new, |(t, _)| format!(
                " before the next act at +{}",
                at(t0, *t)
            ))
        ));
    };
    let read = format!(
        "the defense read at +{} ({})",
        at(t0, *te),
        crate::util::one_line(el, 90)
    );
    let Some((tw, wl)) = y.waiting else {
        return Verdict::Fail(format!(
            "no DONE, task={task}; {read}, but no WAITING decided for that point{}",
            again.map_or_else(
                || format!(" within {}", at(t0, seen.end_ms)),
                |(t, _)| format!(" before the act at +{}", at(t0, *t))
            )
        ));
    };
    let waited = format!("+{} {}", at(t0, *tw), crate::util::one_line(wl, 110));
    match again {
        Some((t2, l2)) => Verdict::Pass(format!(
            "no DONE, task={task}; {read}; {waited}; {} typed at +{}",
            act_rule(l2).unwrap_or("?"),
            at(t0, *t2)
        )),
        None => Verdict::Unobserved(format!(
            "no DONE, task={task}; {read}; {waited} — the next act not observed within {}",
            at(t0, seen.end_ms)
        )),
    }
}

/// OFF: a done reply under Off, then for [`QUIET_UNDER_OFF_MS`] the
/// supervisor never on the tab (`supervisor=-` at every look, no journal),
/// nothing typed (no ledger act, no harness-marked turn in the stub's
/// dumps of the conversation), no need raised.
fn judge_off(seen: &Seen, t0: i64) -> Verdict {
    let acted = seen.ledger.iter().filter(|r| r.acted()).count();
    let autopilot = seen.last_status("autopilot").unwrap_or_else(|| "?".into());
    let supervised: Vec<String> = seen
        .status
        .iter()
        .filter_map(|(_, s)| word(s, "supervisor").filter(|w| w != "-"))
        .collect();
    let Some(reply) = seen.reply_ms else {
        return Verdict::Unobserved("the done reply never showed".into());
    };
    let quiet = seen.end_ms - reply;
    let said = format!(
        "autopilot={autopilot}; supervisor={}; the done reply at +{}; {} journal line(s), {acted} ledger act(s) over {} after it; the stub {}",
        supervised.first().map_or("- throughout", String::as_str),
        at(t0, reply),
        seen.journal.len(),
        at(0, quiet),
        seen.stub.as_ref().map_or_else(
            || "UNREAD".to_string(),
            |s| format!(
                "read {} request(s) of the conversation, {} with a harness turn",
                s.read, s.new_turns
            )
        )
    );
    let stub_typed = seen.stub.as_ref().is_some_and(|s| s.new_turns > 0);
    if !seen.journal.is_empty()
        || acted > 0
        || autopilot != "off"
        || !supervised.is_empty()
        || stub_typed
    {
        return Verdict::Fail(said);
    }
    if let Some(n) = seen.needs_seen() {
        return Verdict::Fail(format!("{said}; a need was raised: {n}"));
    }
    if seen.stub.is_none() {
        return Verdict::Unobserved(format!("{said}: the stub's dumps were not read"));
    }
    if quiet < QUIET_UNDER_OFF_MS {
        return Verdict::Unobserved(format!(
            "{said}: watched under {} s",
            QUIET_UNDER_OFF_MS / 1000
        ));
    }
    Verdict::Pass(said)
}

// ---------------------------------------------------------------------------
// The live run.

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("forever: COULD NOT RUN: {m}"))
}

/// `git` in the scratch, by path, its global and system configuration
/// left out (`GIT_CONFIG_GLOBAL=/dev/null`): the owner's is never read.
fn git(home: &Path, dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = aterm_uds::spawnfd::command("/usr/bin/git")
        .args([
            "-c",
            "user.name=aterm lab",
            "-c",
            "user.email=lab@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The chain's scratch repository, `<proj>/<chain>`, its `main` pushed to
/// the local bare `<sockdir>/origin-<chain>.git` and `origin/HEAD` naming
/// it, then left as its [`Repo`] says. Its physical path.
fn lay_repo(run: &Run, chain: Chain) -> Result<PathBuf, String> {
    let home = run.w.join("home");
    let dir = Path::new(&run.proj).join(chain.name());
    let bare = Path::new(&run.sockdir).join(format!("origin-{}.git", chain.name()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::create_dir_all(&bare).map_err(|e| format!("{}: {e}", bare.display()))?;
    let put = |name: &str, text: &str| {
        std::fs::write(dir.join(name), text).map_err(|e| format!("{name}: {e}"))
    };
    let g = |args: &[&str]| git(&home, &dir, args);
    git(&home, &bare, &["init", "-q", "--bare", "-b", "main"])?;
    g(&["init", "-q", "-b", "main"])?;
    put(
        "README.md",
        &format!(
            "# {}\n\nThe forever lab's scratch project.\n",
            chain.token()
        ),
    )?;
    g(&["add", "README.md"])?;
    g(&["commit", "-q", "-m", "lab: the scratch project"])?;
    g(&["remote", "add", "origin", &bare.to_string_lossy()])?;
    g(&["push", "-q", "-u", "origin", "main"])?;
    g(&["remote", "set-head", "origin", "main"])?;
    let readme_changed = "# The forever lab\n\nA change nobody committed.\n";
    match chain.repo() {
        Repo::Clean => {}
        Repo::Untracked => put("wip-notes.txt", "work nobody committed\n")?,
        Repo::Changed => put("README.md", readme_changed)?,
        Repo::Stashed => {
            put("README.md", readme_changed)?;
            g(&["stash", "push", "-q"])?;
        }
        Repo::Unpushed | Repo::Unlanded => {
            put("parser.txt", "the parser, committed here only\n")?;
            g(&["add", "parser.txt"])?;
            g(&["commit", "-q", "-m", "lab: the parser"])?;
            if chain.repo() == Repo::Unlanded {
                g(&["push", "-q", "origin", "main:wip"])?;
            }
        }
    }
    std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Claude Code's folder trust for each repository (the scratch
/// `.claude.json`'s `projects`), written before any Claude Code runs.
fn trust_for_claude(run: &Run, dirs: &[PathBuf]) -> Result<(), Fail> {
    let p = run.w.join("home/.claude/.claude.json");
    let text = crate::util::scratch_text(&p);
    let mut cj = J::parse(&text).unwrap_or_else(|_| J::obj([]));
    let mut projects = match cj.get("projects") {
        Some(J::Obj(m)) => m.clone(),
        _ => Vec::new(),
    };
    // each repository's physical path and its `/tmp` spelling: the key is the cwd as the agent reads it
    let keys = dirs.iter().flat_map(|d| {
        let real = d.to_string_lossy().into_owned();
        let short = real.strip_prefix("/private").map(str::to_string);
        std::iter::once(real).chain(short)
    });
    for key in keys {
        let entry = J::obj([
            ("allowedTools", J::Arr(Vec::new())),
            ("mcpContextUris", J::Arr(Vec::new())),
            ("mcpServers", J::obj([])),
            ("enabledMcpjsonServers", J::Arr(Vec::new())),
            ("disabledMcpjsonServers", J::Arr(Vec::new())),
            ("hasTrustDialogAccepted", J::Bool(true)),
            ("hasClaudeMdExternalIncludesApproved", J::Bool(false)),
            ("hasClaudeMdExternalIncludesWarningShown", J::Bool(false)),
        ]);
        projects.retain(|(k, _)| *k != key);
        projects.push((key, entry));
    }
    cj.set("projects", J::Obj(projects));
    let pretty = crate::pyjson::Style {
        indent: Some(2),
        compact: false,
        ascii: true,
    };
    crate::sys::put(
        &p,
        0o600,
        &crate::sys::PutSrc::Bytes(cj.dump(pretty).as_bytes()),
    )
    .map_err(|e| could_not(&format!("{}: {e}", p.display())))
}

/// The harness's launcher, written once into the run directory (each tab
/// types `sh <it>` in its own repository), and its seeds; its path.
fn launcher(run: &Run, h: Harness, dirs: &[PathBuf]) -> Result<PathBuf, Fail> {
    use crate::gemini::Agent;
    let put = |p: &Path, mode: u32, text: &str| {
        crate::sys::put(p, mode, &crate::sys::PutSrc::Bytes(text.as_bytes()))
            .map_err(|e| could_not(&format!("{}: {e}", p.display())))
    };
    let script = run.run.join(format!("forever-{}.sh", h.name()));
    let text = match h {
        Harness::Claude => {
            trust_for_claude(run, dirs)?;
            crate::lab::launcher(run, &crate::lab::Launch::default())
        }
        Harness::Gemini => {
            crate::gemini::seed(run, Agent::Gemini, &crate::gemini::Launch::default())?;
            crate::gemini::launcher(run, Agent::Gemini, &[], &[])
        }
        Harness::Agy => {
            // the provider seeded (never a Google sign-in) and every repository trusted
            crate::gemini::seed(run, Agent::Agy, &crate::gemini::Launch::default())?;
            let mut trusted: Vec<String> = dirs
                .iter()
                .map(|d| d.to_string_lossy().into_owned())
                .collect();
            trusted.extend(dirs.iter().filter_map(|d| {
                d.to_string_lossy()
                    .strip_prefix("/private")
                    .map(str::to_string)
            }));
            let dir = run.w.join("agy-home/.gemini/antigravity-cli");
            put(
                &dir.join("settings.json"),
                0o600,
                &crate::gemini::agy_settings_with(&trusted, &[]),
            )?;
            crate::gemini::launcher(run, Agent::Agy, &[], &[])
        }
        Harness::Muse => crate::muse::launcher(run, &[], &["--trust-workspace".to_string()]),
    };
    put(&script, 0o755, &text)?;
    Ok(script)
}

/// One tab of the run: its chain, its session, its repository.
struct Tab {
    chain: Chain,
    sid: String,
    dir: PathBuf,
    seen: Seen,
    t0: i64,
    /// Watching ended.
    over: bool,
}

/// `@<sid> <args…>` on the lab, its standard output.
fn on(door: &Door<'_>, sid: &str, args: &[&str]) -> String {
    let at = format!("@{sid}");
    let mut v = vec![at.as_str()];
    v.extend_from_slice(args);
    door.out(&v)
}

/// Whether the agent in tab `sid` stands at its composer: its reader finds one.
fn at_composer(door: &Door<'_>, sid: &str, agent: &str) -> Option<String> {
    crate::screen::composer_row(agent, &on(door, sid, &["text"]))
}

/// A turn typed into tab `sid` as `lab turn` types one (the text, then Enter
/// once it stands in the composer; Enter again, once, while it still stands
/// there): whether the composer took it.
fn tab_turn(door: &Door<'_>, sid: &str, agent: &str, text: &str) -> Result<bool, Fail> {
    let at = format!("@{sid}");
    if door.ctl(&[&at, "send", text])?.code != 0 {
        return Ok(false);
    }
    stop::sleep(Duration::from_millis(300))?;
    let standing = |door: &Door<'_>| at_composer(door, sid, agent).as_deref() == Some(text);
    let mut stood = false;
    for _ in 0..40 {
        if standing(door) {
            stood = true;
            break;
        }
        stop::sleep(Duration::from_millis(250))?;
    }
    if !stood {
        return Ok(false);
    }
    for _ in 0..2 {
        if door.ctl(&[&at, "key", "enter"])?.code != 0 {
            return Ok(false);
        }
        for _ in 0..20 {
            stop::sleep(Duration::from_millis(250))?;
            if !standing(door) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The tab's journal lines `(t, line)` and ledger rows, read through no link.
fn records(run: &Run, sid: &str) -> (Vec<(i64, String)>, Vec<Row>) {
    let drive = run.state().join("drive");
    let journal = crate::util::scratch_text(&drive.join(format!("{sid}.journal.jsonl")))
        .lines()
        .filter_map(|l| {
            let j = J::parse(l).ok()?;
            let t = j.get("t").and_then(J::as_i64)?;
            Some((t, j.get("line").and_then(J::as_str)?.to_string()))
        })
        .collect();
    let ledger = crate::util::scratch_text(&drive.join(format!("{sid}.jsonl")))
        .lines()
        .filter_map(|l| {
            let j = J::parse(l).ok()?;
            let s = |k: &str| j.get(k).and_then(J::as_str).unwrap_or("").to_string();
            Some(Row {
                ts: j.get("ts").and_then(J::as_i64)?,
                rule: s("rule_id"),
                decision: s("decision"),
                command: s("command"),
                reason: s("reason"),
            })
        })
        .collect();
    (journal, ledger)
}

/// The `status` words the judge reads, one line.
fn status_words(door: &Door<'_>, sid: &str) -> String {
    let s = on(door, sid, &["status"]);
    ["task", "needs", "autopilot", "supervisor", "agent"]
        .iter()
        .map(|k| format!("{k}={}", word(&s, k).unwrap_or_else(|| "?".into())))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `meta` attention words, the text decoded and last.
fn meta_words(door: &Door<'_>, sid: &str) -> String {
    let m = on(door, sid, &["meta"]);
    let w = |k: &str| word(&m, k).unwrap_or_else(|| "?".into());
    format!(
        "attention_owners={} attention_kinds={} attention={}",
        pct_decode(&w("attention_owners")),
        pct_decode(&w("attention_kinds")),
        pct_decode(&w("attention"))
    )
}

/// The harness's mark on what it types (`harness_mark!`).
const MARK: &str = "[aterm harness]";

/// The harness-marked user texts a request `body` of conversation `token`
/// holds (by the reader of whichever of the three APIs it speaks); `None`
/// for a request of another conversation. A request holding more than the
/// conversation held at the landing carries a NEW turn of the harness's
/// ([`StubSeen::of`]): an agent's own side calls after a turn (Muse Code's
/// reminder observers, its prompt hint, its away recap; Claude Code's title)
/// carry the conversation as it stood, or a question of their own with no
/// mark, and hold no more; an act typed again, the same words as one before
/// included, is one more.
#[must_use]
pub fn marked_turns(body: &[u8], token: &str) -> Option<usize> {
    if !String::from_utf8_lossy(body).contains(token) {
        return None;
    }
    let marked = |users: Vec<String>| users.iter().filter(|u| u.contains(MARK)).count();
    [
        marked(crate::stub::message_users(body)),
        marked(crate::stub::responses_users(body)),
        marked(crate::stub::gemini::gemini_users(body)),
    ]
    .into_iter()
    .max()
}

/// The stub's request dumps of conversation `token`, `(modified ms, marked
/// turns)` each ([`marked_turns`]).
fn dumps_of(run: &Run, token: &str) -> Vec<(i64, usize)> {
    std::fs::read_dir(run.w.join("dump"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let t = e
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            let t = i64::try_from(t.as_millis()).ok()?;
            let body = std::fs::read(e.path()).ok()?;
            Some((t, marked_turns(&body, token)?))
        })
        .collect()
}

impl StubSeen {
    /// The witness of conversation `token` (whose acts the ledger typed
    /// `typed_before` of before `since`): the requests after `since` holding
    /// more harness-marked user texts than any request before it, or than
    /// the acts typed, held. Off passes `since` 0 and no acts: any marked
    /// text is a turn of the harness's.
    fn of(dumps: &[(i64, usize)], since: i64, typed_before: usize) -> StubSeen {
        let baseline = dumps
            .iter()
            .filter(|(t, _)| *t <= since)
            .map(|(_, n)| *n)
            .chain(std::iter::once(typed_before))
            .max()
            .unwrap_or(0);
        let after: Vec<usize> = dumps
            .iter()
            .filter(|(t, _)| *t > since)
            .map(|(_, n)| *n)
            .collect();
        StubSeen {
            baseline,
            read: after.len(),
            new_turns: after.iter().filter(|n| **n > baseline).count(),
        }
    }
}

/// The stub's witness of a tab, read once its watch is over: after its
/// landing (L5, L7), or over its whole conversation (Off).
fn stub_witness(run: &Run, tab: &Tab) -> Option<StubSeen> {
    let token = tab.chain.token();
    if tab.chain == Chain::Off {
        return Some(StubSeen::of(&dumps_of(run, &token), 0, 0));
    }
    if !tab.chain.lands() {
        return None;
    }
    let (d, _) = tab.seen.first_after("DONE ", tab.t0)?;
    let typed = tab
        .seen
        .ledger
        .iter()
        .filter(|r| r.acted() && r.ts <= *d && r.command.contains(MARK))
        .count();
    Some(StubSeen::of(&dumps_of(run, &token), *d, typed))
}

/// One look at a tab: what changed since the last is added to its records.
fn look(run: &Run, door: &Door<'_>, tab: &mut Tab) {
    let now = now_ms();
    let (journal, ledger) = records(run, &tab.sid);
    for (t, l) in journal.iter().skip(tab.seen.journal.len()) {
        println!(
            "  [{}] +{} journal {}",
            tab.chain.name(),
            at(tab.t0, *t),
            crate::util::one_line(l, 220)
        );
    }
    for r in ledger.iter().skip(tab.seen.ledger.len()) {
        println!(
            "  [{}] +{} ledger  {} {} {}",
            tab.chain.name(),
            at(tab.t0, r.ts),
            r.decision,
            r.rule,
            crate::util::one_line(&r.command, 200)
        );
    }
    tab.seen.journal = journal;
    tab.seen.ledger = ledger;
    let put = |list: &mut Vec<(i64, String)>, what: &str, v: String| {
        if list.last().is_none_or(|(_, last)| *last != v) {
            println!(
                "  [{}] +{} {what:<7} {}",
                tab.chain.name(),
                at(tab.t0, now),
                crate::util::one_line(&v, 240)
            );
            list.push((now, v));
        }
    };
    put(&mut tab.seen.status, "status", status_words(door, &tab.sid));
    put(&mut tab.seen.meta, "meta", meta_words(door, &tab.sid));
    put(
        &mut tab.seen.task,
        "task",
        on(door, &tab.sid, &["task"]).trim().to_string(),
    );
    if tab.chain == Chain::Off && tab.seen.reply_ms.is_none() {
        let text = on(door, &tab.sid, &["text"]);
        if text.contains("All done") {
            tab.seen.reply_ms = Some(now);
            println!(
                "  [{}] +{} screen  the done reply shows",
                tab.chain.name(),
                at(tab.t0, now)
            );
        }
    }
    tab.seen.end_ms = now;
}

/// Whether a tab's chain has seen what it waits for (or its hang detector
/// fired): then it is watched no more. A landing is watched through its
/// window ([`quiet_after_landed_ms`]) whatever the hang detector says.
fn finished(tab: &Tab, now: i64) -> bool {
    let s = &tab.seen;
    let done = s.first_after("DONE ", tab.t0).map(|(t, _)| *t);
    if tab.chain.lands()
        && let Some(d) = done
    {
        return now - d >= quiet_after_landed_ms(s, d, tab.chain.tasked()) + 5000;
    }
    if now - tab.t0 > CHAIN_LIMIT_MS {
        return true;
    }
    let first = |rule: &str| {
        s.journal
            .iter()
            .find(|(_, l)| act_rule(l) == Some(rule))
            .map(|(t, _)| *t)
    };
    let nudge = first(turn_end::RULE_LAND_NUDGE);
    let done_since = |ms: i64| done.is_some_and(|d| now - d >= ms);
    match tab.chain {
        Chain::LandA | Chain::LandE => false,
        Chain::LandB | Chain::LandC | Chain::LandF | Chain::LandG | Chain::LandH | Chain::LandI => {
            nudge.is_some_and(|n| {
                s.yield_after(n, None, None)
                    .waiting
                    .is_some_and(|(w, _)| now - w >= 3000)
            }) || done_since(20_000)
        }
        Chain::LandD => nudge.is_some_and(|n| now - n >= 10_000) || done_since(20_000),
        Chain::Cruise => {
            let check = first(turn_end::RULE_DONE_CHECK);
            check.is_some_and(|c| s.all_acts().iter().any(|(t, _)| *t > c)) || done_since(20_000)
        }
        Chain::Off => s
            .reply_ms
            .is_some_and(|r| now - r >= QUIET_UNDER_OFF_MS + 5000),
    }
}

/// The screen's last rows of a tab, for the report.
fn tail_rows(door: &Door<'_>, sid: &str, n: usize) -> Vec<String> {
    let text = on(door, sid, &["text"]);
    let rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    rows[rows.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.chars().take(150).collect())
        .collect()
}

/// One harness's run on the lab `tag`, which [`run_lab`] brought up: every
/// scenario's verdict.
#[allow(clippy::too_many_lines)]
fn run_check(tag: &str, h: Harness, chains: &[Chain]) -> Result<Vec<(Scenario, Verdict)>, Fail> {
    let mut run = Run::load(tag)?;
    let (bin, ver) = h.bin_ver(&run);
    let (bin, ver) = (bin.to_string(), ver.to_string());
    if bin.is_empty() || !crate::sys::executable(&bin) {
        return Err(could_not(&format!(
            "no {} in lab '{tag}' (LAB_{} at up)",
            h.name(),
            h.name().to_ascii_uppercase()
        )));
    }
    crate::procs::need_witness(&run).map_err(|e| could_not(&e))?;
    println!(
        "forever: lab '{tag}', {} {ver} ({bin}), aterm {}, {}x{}",
        h.name(),
        run.aterm,
        run.cols,
        run.rows
    );
    run.agent = h.name().into();
    run.save();
    // ---- the repositories, the seeds, the launcher, the stub's script
    let mut dirs = Vec::new();
    for &c in chains {
        let d = lay_repo(&run, c).map_err(|e| could_not(&format!("{}: {e}", c.name())))?;
        println!(
            "  repo      {}: {} (origin {}/origin-{}.git), {}",
            c.name(),
            d.display(),
            run.sockdir,
            c.name(),
            c.repo().says()
        );
        dirs.push(d);
    }
    let script = launcher(&run, h, &dirs)?;
    crate::verbs::set_custom_mode(&run, MODE, &spec(chains))?;
    println!(
        "  stub      mode {MODE}: {} rule(s)",
        chains.iter().map(|c| c.rules().len()).sum::<usize>()
    );
    let door = Door::new(&run)?;
    // ---- a tab per chain, its Autopilot set (and a worker's task), the agent launched there
    let mut tabs: Vec<Tab> = Vec::new();
    for (&chain, dir) in chains.iter().zip(&dirs) {
        let r = door.ctl(&["spawn", &format!("cwd={}", dir.display())])?;
        let sid = r
            .stdout
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("OK "))
            .and_then(|l| l.split_whitespace().next())
            .map(str::to_string)
            .ok_or_else(|| could_not(&format!("spawn: {}{}", r.stdout, r.stderr)))?;
        let _ = on(&door, &sid, &["await", "idle", "400", "timeout=30000"]);
        let set = on(
            &door,
            &sid,
            &["meta", "set", "autopilot", chain.autopilot()],
        );
        let task = if chain.tasked() {
            let r = on(&door, &sid, &["task", "mark", "working", "from=ctl"]);
            if !r.contains("state=working") {
                return Err(could_not(&format!(
                    "{}: task mark working: {}",
                    chain.name(),
                    r.trim()
                )));
            }
            format!(", task {}", crate::util::one_line(r.trim(), 120))
        } else {
            String::new()
        };
        let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
        let _ = on(&door, &sid, &["send", &line]);
        let _ = on(&door, &sid, &["key", "enter"]);
        println!(
            "  tab       {} @{sid} in {}: autopilot {} ({}){task}, launched",
            chain.name(),
            dir.display(),
            chain.autopilot(),
            set.trim()
        );
        tabs.push(Tab {
            chain,
            sid,
            dir: dir.clone(),
            seen: Seen::default(),
            t0: 0,
            over: false,
        });
    }
    // ---- each agent at its composer, then its first prompt
    for tab in &mut tabs {
        let ready = crate::wait_until(Duration::from_secs(90), || {
            at_composer(&door, &tab.sid, h.name()).is_some_and(|c| c.is_empty())
        });
        if !ready {
            for l in tail_rows(&door, &tab.sid, 12) {
                println!("    | {l}");
            }
            return Err(could_not(&format!(
                "{} never stood at an empty composer in {} within 90 s",
                h.name(),
                tab.chain.name()
            )));
        }
        stop::sleep(Duration::from_secs(1))?;
        tab.t0 = now_ms();
        if !tab_turn(&door, &tab.sid, h.name(), &tab.chain.prompt())? {
            for l in tail_rows(&door, &tab.sid, 12) {
                println!("    | {l}");
            }
            return Err(could_not(&format!(
                "the composer of {} did not take the first prompt",
                tab.chain.name()
            )));
        }
        println!(
            "  prompt    {} @{}: \"{}\"",
            tab.chain.name(),
            tab.sid,
            tab.chain.prompt()
        );
    }
    // ---- watched until every chain saw what it waits for
    loop {
        let now = now_ms();
        for tab in tabs.iter_mut().filter(|t| !t.over) {
            look(&run, &door, tab);
            if finished(tab, now) {
                tab.over = true;
                tab.seen.stub = stub_witness(&run, tab);
                println!(
                    "  [{}] watched to +{}{}",
                    tab.chain.name(),
                    at(tab.t0, now),
                    tab.seen.stub.as_ref().map_or_else(String::new, |s| format!(
                        "; the stub: {} request(s) read, {} new harness turn(s) (baseline {})",
                        s.read, s.new_turns, s.baseline
                    ))
                );
            }
        }
        if tabs.iter().all(|t| t.over) {
            break;
        }
        stop::sleep(Duration::from_secs(2))?;
    }
    // ---- the verdicts
    let mut all = Vec::new();
    for tab in &tabs {
        println!(
            "  screen    {} @{} ({}):",
            tab.chain.name(),
            tab.sid,
            tab.dir.display()
        );
        for l in tail_rows(&door, &tab.sid, 8) {
            println!("    | {l}");
        }
        for (s, v) in judge(tab.chain, &tab.seen, h.name(), tab.t0) {
            println!("forever {} {} {}: {}", h.name(), s.id(), v.word(), v.why());
            all.push((s, v));
        }
    }
    Ok(all)
}

/// One harness on a lab of its own, brought up and taken down (its request
/// dumps kept with its logs: the stub's witness can be read again).
fn run_lab(
    tag: &str,
    h: Harness,
    mode: Mode,
    keep: bool,
) -> Result<Vec<(Scenario, Verdict)>, Fail> {
    if is_run(tag) {
        return Err(Fail::usage(format!(
            "forever: lab '{tag}' is up: forever brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    let mut u = crate::lab::UpOpts::from_env();
    u.trust = true;
    u.lifeline = !keep;
    if let Err(f) = crate::lab::up(tag, &u) {
        if f.stopped.is_some() {
            return Err(f);
        }
        return Err(could_not(&format!(
            "the lab did not come up: {}",
            f.msg.unwrap_or_default()
        )));
    }
    let verdicts = run_check(tag, h, &Chain::of_mode(mode));
    if keep {
        println!("forever: lab '{tag}' left up ({LAB} down {tag})");
    } else {
        let _ = crate::lab::down(tag, true);
    }
    verdicts
}

/// What `forever`'s words ask for.
#[derive(Debug, PartialEq, Eq)]
struct Asked {
    harnesses: Vec<Harness>,
    mode: Mode,
    tag: Option<String>,
    keep: bool,
}

/// `forever`'s words read; `Ok(None)` for `-h`/`--help`.
fn parse_args(args: &[String]) -> Result<Option<Asked>, Fail> {
    let usage = |m: &str| Fail::usage(format!("forever: {m}"));
    let mut harnesses = None;
    let mut mode = Mode::Both;
    let mut tag = None;
    let mut keep = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--harness" => {
                let w = it.next().ok_or_else(|| usage("--harness needs a word"))?;
                harnesses = Some(if w == "all" {
                    Harness::ALL.to_vec()
                } else {
                    vec![Harness::of(w).ok_or_else(|| {
                        usage(&format!(
                            "unknown harness '{w}' (claude, gemini, agy, muse, all)"
                        ))
                    })?]
                });
            }
            "--mode" => {
                mode = match it.next().map(String::as_str) {
                    Some("land") => Mode::Land,
                    Some("cruise") => Mode::Cruise,
                    Some("both") => Mode::Both,
                    other => {
                        return Err(usage(&format!(
                            "--mode takes land, cruise or both, not {other:?}"
                        )));
                    }
                };
            }
            "--keep" => keep = true,
            "-h" | "--help" => return Ok(None),
            s if s.starts_with('-') => return Err(usage(&format!("unknown option '{s}'"))),
            s => tag = Some(s.to_string()),
        }
    }
    let harnesses = harnesses.ok_or_else(|| usage("say --harness claude|gemini|agy|muse|all"))?;
    Ok(Some(Asked {
        harnesses,
        mode,
        tag,
        keep,
    }))
}

/// `forever --harness <claude|gemini|agy|muse|all> [--mode land|cruise|both] [tag] [--keep]`.
pub fn cmd_forever(args: &[String]) -> Res {
    let Some(a) = parse_args(args)? else {
        print!("{}", crate::USAGE);
        return Ok(0);
    };
    let base = a
        .tag
        .clone()
        .unwrap_or_else(|| format!("forever-{}", std::process::id()));
    stop::install();
    let mut lines = Vec::new();
    let mut could_not_run = false;
    for &h in &a.harnesses {
        let tag = if a.harnesses.len() > 1 {
            format!("{base}-{}", h.name())
        } else {
            base.clone()
        };
        if !crate::paths::valid_tag(&tag) {
            return Err(Fail::usage(format!(
                "forever: bad tag '{tag}' (letters, digits, _ and -, at most 24)"
            )));
        }
        match run_lab(&tag, h, a.mode, a.keep) {
            Ok(v) => lines.extend(v.into_iter().map(|(s, v)| (h, s, v))),
            Err(f) if f.code == 3 => {
                eprintln!("aterm-lab: {}", f.msg.unwrap_or_default());
                could_not_run = true;
            }
            Err(f) => return Err(f),
        }
    }
    println!("forever: the verdicts");
    for (h, s, v) in &lines {
        println!(
            "  {:<7} {:<4} {:<10} {}",
            h.name(),
            s.id(),
            v.word(),
            s.what()
        );
    }
    let all_pass = lines.iter().all(|(_, _, v)| matches!(v, Verdict::Pass(_)));
    Ok(if could_not_run {
        3
    } else if all_pass && !lines.is_empty() {
        0
    } else {
        1
    })
}

#[cfg(test)]
#[path = "forever_tests.rs"]
mod tests;
