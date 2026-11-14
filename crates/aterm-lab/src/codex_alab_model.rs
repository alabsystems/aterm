// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fake Responses model for `codex-alab` proof legs, with state owned by each model world.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aterm_agent::supervise::config::CONTINUE_TEXT;
use aterm_agent::supervise::policy::land_check::LANDED as LANDED_ANSWER;
use aterm_agent::supervise::policy::needs::NEEDS_ACK;
use aterm_agent::supervise::policy::turn_end::{
    self, DONE_CHECK, FINISHED, LAND, LAND_ANSWER, LAND_CHECK_HEAD, LAND_NUDGE,
};

use crate::codex_rig::{Answered, ResponseGate, call_item, message_item, request_view};
use crate::pyjson::J;

#[path = "codex_alab_policy_model.rs"]
pub mod policy;
#[path = "codex_alab_terminals_model.rs"]
pub mod terminals;

/// Legacy continuation substring shared with [`CONTINUE_TEXT`].
pub const KEEP_GOING: &str = "keep going";

/// The two forms of the one line codex-alab types at a usage limit's resume, after its
/// `[codex-alab]` mark (codex-alab `alab/forever-cruise`, aterm `77161d6432`'s `cleared_text`):
/// the reset was named, or the try is blind. Cruise's continuation waits for the agent's own
/// next turn end.
pub const LIMIT_CLEARED: [&str; 2] = [
    "The usage limit has cleared; continue where you stopped.",
    "The usage limit may have cleared; continue where you stopped.",
];

/// Legacy done-check prefix recognized for negative-control classification.
pub const RETIRED_CHECK: &str = "If the task is finished and verified";

/// Prefix of the dirty-repository suffix appended to Land nudges.
pub const REPO_SUFFIX_HEAD: &str = "Your repository still holds work no remote main has";

/// The tag a request carrying Land's contradiction gets (aterm's slice 4c), its words as aterm
/// types them ([`contradiction_fault`]).
pub const CONTRADICTED_TAG: &str = "+contradicted";

/// The tag a request carrying Land's contradiction's head gets where its words are not aterm's.
pub const CONTRADICTED_WRONG_TAG: &str = "+contradicted-wrong";

/// What is wrong with `user`, a prompt that opens with Land's contradiction's head
/// (`LAND_CONTRADICTED_HEAD`), against aterm's words: the measured fact or the agent's list after
/// the head, Land's sentence on files no repository holds, "never delete work", and no way out of
/// landing (the forever lab's escapes, in any case); `None` where it says them all.
#[must_use]
pub fn contradiction_fault(user: &str) -> Option<String> {
    let after = user
        .split_once(turn_end::LAND_CONTRADICTED_HEAD)
        .map(|(_, r)| r.trim_start())?;
    let mut bad = Vec::new();
    if !after.starts_with(turn_end::CONTRADICTED_GIT)
        && !after.starts_with(turn_end::CONTRADICTED_LISTED)
    {
        bad.push("no measured fact or list after the head".to_string());
    }
    for whole in [turn_end::NO_REPOSITORY, "never delete work"] {
        if !user.contains(whole) {
            bad.push(format!("does not say {whole:?}"));
        }
    }
    let lower = user.to_lowercase();
    bad.extend(
        crate::forever::ESCAPES
            .iter()
            .filter(|w| lower.contains(*w))
            .map(|w| format!("offers a way out ({w:?})")),
    );
    (!bad.is_empty()).then(|| bad.join("; "))
}

/// The tag `user` adds for Land's contradiction: [`CONTRADICTED_TAG`] where it carries aterm's
/// words, [`CONTRADICTED_WRONG_TAG`] where it opens with the head in other words, nothing where
/// it carries no contradiction.
#[must_use]
pub fn contradiction_tag(user: &str) -> &'static str {
    if !user.contains(turn_end::LAND_CONTRADICTED_HEAD) {
        return "";
    }
    if contradiction_fault(user).is_some() {
        CONTRADICTED_WRONG_TAG
    } else {
        CONTRADICTED_TAG
    }
}

/// Prompt text typed by a person to reopen a landed task.
pub const REOPEN: &str = "REOPEN";

/// Turn duration exceeding `codex-alab`'s two-minute `min_work` threshold.
pub const SLOW_TURN: Duration = Duration::from_secs(130);

/// Duration for which the hold leg's first response is held.
pub const HOLD_FIRST_TURN: Duration = SLOW_TURN;

/// Duration for which the carry leg's first response is held before SIGKILL.
pub const CARRY_FIRST_TURN: Duration = Duration::from_secs(3600);

/// Duration for which the repository leg holds its response while committing.
pub const REPO_COMMIT_HOLD: Duration = Duration::from_secs(60);

/// Duration for which the repository leg holds its response while pushing.
pub const REPO_PUSH_HOLD: Duration = Duration::from_secs(20);

/// Duration for which the `NEEDS YOU` leg holds its response while the need stands.
pub const NEEDS_HOLD: Duration = Duration::from_secs(10);

/// Usage-limit window duration in seconds reported by the 429 fixture.
pub const RESETS_IN_S: i64 = 5;

/// Number of Land nudges issued before the model returns a clean defense.
pub const NUDGES_BEFORE_DEFENSE: usize = 4;

/// The task `spawn agent=codex` hands the worker leg.
pub const WORKER_TASK: &str = "Write the worker's report.";

/// The worker leg's report, the words its `result` carries.
pub const WORKER_REPORT: &str = "Wrote the worker's report: the lab's task is done.";

/// Whether the worker's land act is answered only once [`WORKER_LAND`] is released.
static WORKER_LAND_HELD: AtomicBool = AtomicBool::new(false);

/// What holds the worker's land act's answer while [`WORKER_LAND_HELD`]: so its Land hold (the
/// land act typed, the yield not yet judged) stands until `codex-alab-proof --window-only` read
/// its status tell the hold, which a 2 s read of a hold of a second or two catches only by
/// luck. The gate's own wait is a one-minute hang detector.
static WORKER_LAND: OnceLock<Arc<ResponseGate>> = OnceLock::new();

/// From now on, the worker's land act's answer waits for [`release_worker_land`].
pub fn hold_worker_land() {
    WORKER_LAND_HELD.store(true, Ordering::SeqCst);
}

/// The worker's land act's answer goes (and any later one at once).
pub fn release_worker_land() {
    WORKER_LAND.get_or_init(Default::default).release();
}

/// First-leg reply claiming completion with the retired done token.
pub const FIRST_DONE: &str = "Hello from the lab model. Nothing else to do.\n\nATERM-TASK-DONE";

/// Cruise leg reply to the defend-and-find-more prompt.
pub const CRUISE_DEFENSE: &str = "Looked again.\n\nNothing left: the greeting was the whole task.";

/// Plain intermediate reply for the continuation leg's first turn.
pub const STEP_ONE: &str = "Here is step one of the plan. I will continue when asked.";

/// Continuation leg reply claiming completion with the retired done token.
pub const CONTINUED_DONE: &str = "Continued as asked. All done.\n\nATERM-TASK-DONE";

/// The facts leg's first turn's reply once both its calls run (gap 23, `codex-alab-proof
/// --facts-only`): no done claim, so Cruise continues it.
pub const FACTS_STARTED: &str = "Started the watcher; step one is next.";

/// The ordinal words [`facts_step`] counts with: words, never digits, since codex-alab's reply
/// mark keeps only a word's letters and folds every word holding a digit into one `#`
/// (`replies::mark_of`). A reply that repeats holds the point: codex-alab paces it 120 s and then
/// types its forward recovery, which is no continuation.
const FACTS_ORDINALS: [&str; 10] = [
    "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth",
];

/// The facts leg's reply to its `n`th continuation (from 1): no done claim, so Cruise continues it
/// again, and in words no earlier continuation's reply had (to the tenth), so codex-alab never reads
/// a repeated report and paces it.
#[must_use]
pub fn facts_step(n: usize) -> String {
    let at = n.saturating_sub(1).min(FACTS_ORDINALS.len() - 1);
    format!(
        "Took the {} step of the plan; the step after it is next.",
        FACTS_ORDINALS[at]
    )
}

/// The tag a request whose user text carries aterm's fact rider gets (`turn_end::FACT_OPEN`).
pub const FACT_TAG: &str = "+fact";

/// The file the facts leg's holder call locks and its waiter call blocks on, in the leg's folder.
pub const FACTS_LOCK: &str = "facts.lock";

/// Where the facts leg's holder call writes its pid before it stops itself holding the lock.
pub const FACTS_HOLDER_PID: &str = "facts-holder.pid";

/// Where the facts leg's waiter call writes its pid before its rhythm.
pub const FACTS_WAITER_PID: &str = "facts-waiter.pid";

/// The regular file the facts leg's waiter call prints its rhythm into: codex's own pipe carries
/// no progress the jobs engine reads, a file does (`aterm-vitals`' facts scenario).
pub const FACTS_TICKS: &str = "facts-ticks.log";

/// How many ticks the facts leg's waiter prints, one every [`FACTS_TICK_MS`]: 15 s of rhythm, so
/// the engine's 2 s readings see well over the three intervals its own μ needs.
pub const FACTS_TICK_COUNT: usize = 60;

/// The gap between two of the facts leg's waiter's ticks, ms.
pub const FACTS_TICK_MS: u64 = 250;

/// Carry leg reply after crash relaunch.
pub const CARRIED_DONE: &str = "Carried on after the relaunch. All done.\n\nATERM-TASK-DONE";

/// `NEEDS YOU` requirement text used by the needs leg.
pub const NEED: &str = "the signing credential to publish the release";

/// Initial report for the `NEEDS YOU` leg naming [`NEED`].
pub const NEEDS_REPORT: &str = "Built the release candidate and ran its checks.\n\nNEEDS YOU: the \
                                signing credential to publish the release";

/// Follow-up report for the `NEEDS YOU` leg with no `NEEDS YOU:` line.
pub const NEEDS_PLAIN: &str = "Carried on: the changelog is updated and the docs are next.";

/// Final report for the `NEEDS YOU` leg withdrawing the need (`NEEDS YOU: nothing`).
pub const NEEDS_CLEARED: &str = "Carried on without it: the changelog and the docs are \
                                 updated.\n\nNEEDS YOU: nothing";

/// In-progress replies for the Land leg before its defense.
pub const LAND_PROGRESS: [&str; 2] = [
    "Committed the change; the push to origin/main is still running.",
    "The push finished; checking that origin/main has every commit.",
];

/// Unclean Land defense that still carries a standing `NEEDS YOU:` line.
pub const NEEDS_DEFENSE: &str = "Checked origin/main.\n\nNothing left: all of the work is on \
                                 origin/main.\nNEEDS YOU: the release credential to tag it";

/// Unclean Land defense that mentions work left for later.
pub const OPEN_DEFENSE: &str = "Checked origin/main again.\n\nNothing left: all of the work is \
                                on origin/main; the docs fix is for later.\nNEEDS YOU: nothing";

/// Clean Land defense for a non-repository workspace.
pub const LAND_DEFENSE: &str = "Checked origin/main once more.\n\nNothing left: all of the work is \
                                on origin/main.\nNEEDS YOU: nothing";

/// Initial repository-leg report after writing an uncommitted file.
pub const REPO_WROTE: &str = "Wrote notes.txt with the release notes.";

/// Repository-leg defense while uncommitted changes remain.
pub const REPO_DEFENSE: &str = "The notes are written.\n\nNothing left: everything is on \
                                origin/main.";

/// Repository-leg defense while an unpushed commit remains.
pub const REPO_COMMITTED: &str = "Committed notes.txt.\n\nNothing left: everything is on \
                                  origin/main.";

/// Clean repository-leg defense after pushing to `origin/main`.
pub const REPO_PUSHED: &str = "Committed notes.txt and pushed it to origin/main.\n\nNothing left: \
                               everything is on origin/main.";

/// Prose question returned by the question leg after its overlay is answered.
pub const QUESTION_PROSE: &str = "Postgres is set up locally. Should I push the schema to \
                                  origin/main now?";

/// Question-leg reply after receiving Land's answer.
pub const QUESTION_LANDED_WORDS: &str = "Pushed the schema to origin/main.";

/// User question text shared by the `exchange` and `control` legs.
pub const PERSON_ASKS: &str = "tell me what this repository holds so far";

/// Plain reply returned for the `exchange` and `control` legs.
pub const EXCHANGE_REPLY: &str = "The repository holds a README and one heading in it.";

/// Conversation step classified from a request's last user text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Initial or subsequent person prompt.
    Person,
    /// Person turn reopening a landed task ([`REOPEN`]).
    Reopen,
    /// Cruise continuation prompt (`continue@v1`).
    Continue,
    /// A usage limit's resume, its one cleared line ([`LIMIT_CLEARED`], `usage-resume@v1`).
    Cleared,
    /// Cruise defend-and-find-more prompt (`continue-done-check@v1`).
    Defend,
    /// Legacy `codex-alab` done-check prompt.
    RetiredCheck,
    /// Land instruction (`land@v1`).
    Land,
    /// `n`-th Land nudge (`land-nudge@v1`) in the conversation.
    Nudge(usize),
    /// Land answer to a prose question (`land-answer@v1`).
    LandAnswer,
    /// The land check (`land-check@v1`, its head [`LAND_CHECK_HEAD`]).
    LandCheck,
    /// Any other harness-typed prompt.
    Other,
}

impl Step {
    /// Returns the step tag used in model log entries.
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Step::Person => "person".to_string(),
            Step::Reopen => "reopen".to_string(),
            Step::Continue => "continue".to_string(),
            Step::Cleared => "cleared".to_string(),
            Step::Defend => "defend".to_string(),
            Step::RetiredCheck => "retired-check".to_string(),
            Step::Land => "land".to_string(),
            Step::Nudge(n) => format!("nudge{n}"),
            Step::LandAnswer => "land-answer".to_string(),
            Step::LandCheck => "land-check".to_string(),
            Step::Other => "other".to_string(),
        }
    }
}

/// The user texts of a request, in order.
fn user_texts(body: &J) -> Vec<String> {
    body.get("input")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|it| it.get("role").and_then(J::as_str) == Some("user"))
        .map(crate::stub::item_text)
        .collect()
}

/// Extracts the `<name>` from the first `LEG-<name>` user text in `body`.
#[must_use]
pub fn leg_of(body: &J) -> Option<String> {
    user_texts(body).iter().find_map(|text| {
        let (_, rest) = text.split_once("LEG-")?;
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .collect();
        (!name.is_empty()).then_some(name)
    })
}

/// Classifies the [`Step`] of `body` from its user texts.
#[must_use]
pub fn step_of(body: &J) -> Step {
    let texts = user_texts(body);
    let (last, _) = request_view(body);
    if last.contains(LAND_CHECK_HEAD) {
        return Step::LandCheck;
    }
    if last.contains(LAND_NUDGE) {
        return Step::Nudge(texts.iter().filter(|t| t.contains(LAND_NUDGE)).count());
    }
    if last.contains(LAND) {
        return Step::Land;
    }
    if last.contains(LAND_ANSWER) {
        return Step::LandAnswer;
    }
    if last.contains(DONE_CHECK) {
        return Step::Defend;
    }
    if last.contains(RETIRED_CHECK) {
        return Step::RetiredCheck;
    }
    if last.contains(CONTINUE_TEXT) || last.contains(KEEP_GOING) {
        return Step::Continue;
    }
    if LIMIT_CLEARED.iter().any(|line| last.contains(line)) {
        return Step::Cleared;
    }
    if last.contains(REOPEN) {
        return Step::Reopen;
    }
    if last.contains("LEG-") {
        return Step::Person;
    }
    Step::Other
}

/// Returns the number of user texts following the initial `LEG-` prompt.
#[must_use]
pub fn texts_after_prompt(body: &J) -> usize {
    let texts = user_texts(body);
    texts
        .iter()
        .position(|t| t.contains("LEG-"))
        .map_or(0, |at| texts.len() - at - 1)
}

/// Returns the Land leg's `(reply_text, hold_duration)` for land act `n`.
#[must_use]
pub const fn land_reply(n: usize) -> (&'static str, Duration) {
    match n {
        0 => (LAND_PROGRESS[0], SLOW_TURN),
        1 => (NEEDS_DEFENSE, Duration::ZERO),
        2 => (LAND_PROGRESS[1], SLOW_TURN),
        3 => (OPEN_DEFENSE, Duration::ZERO),
        _ => (LAND_DEFENSE, Duration::ZERO),
    }
}

/// One fake provider's model: each leg's usage reset is fixed at its first request, and a
/// different provider starts with no windows from earlier proofs.
#[must_use]
pub fn answerer() -> crate::codex_rig::Answer {
    answerer_with_clock(crate::util::now_secs)
}

fn answerer_with_clock(now: impl Fn() -> i64 + Send + Sync + 'static) -> crate::codex_rig::Answer {
    let resets = Mutex::new(Vec::new());
    Arc::new(move |body| answer(body, &resets, &now))
}

fn reset_of(resets: &Mutex<Vec<(String, i64)>>, leg: &str, now: &dyn Fn() -> i64) -> i64 {
    let mut resets = resets
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, at)) = resets.iter().find(|(l, _)| l == leg) {
        return *at;
    }
    let at = now() + RESETS_IN_S;
    resets.push((leg.to_string(), at));
    at
}

/// The question leg's overlay: one question, its second option marked
/// `(Recommended)`.
fn question_call() -> J {
    let option = |label: &str, description: &str| {
        J::obj([("label", J::s(label)), ("description", J::s(description))])
    };
    call_item(
        "request_user_input",
        &J::obj([(
            "questions",
            J::Arr(vec![J::obj([
                ("id", J::s("db_choice")),
                ("header", J::s("Database")),
                ("question", J::s("Which database should I set up?")),
                (
                    "options",
                    J::Arr(vec![
                        option("SQLite", "A single local file."),
                        option("Postgres (Recommended)", "A server, closer to production."),
                    ]),
                ),
            ])]),
        )]),
    )
}

/// The continue leg's continuation's call (gap 20): a command that fails and prints nothing,
/// with no login shell (`login`, codex-alab's `ExecCommandArgs`,
/// codex-rs/core/src/tools/handlers/unified_exec.rs). The leg's opening runs it under the jail
/// alone (`CodexSandbox::Jail`, the `continue` row of the proof's `OPENINGS`), whether the proof
/// or `autopilot-captures`' Cruise scene opens it, so codex's sandbox-denial check
/// (codex-rs/sandboxing/src/denial.rs) reads no denial and asks for no escalation.
fn failing_call() -> J {
    call_item(
        "exec_command",
        &J::obj([("cmd", J::s("exit 3")), ("login", J::Bool(false))]),
    )
}

/// One of the facts leg's calls: `cmd` in codex's unified exec with no terminal (its default
/// `tty=false`: a session of its own, stdin `/dev/null`) and no login shell, yielding after 1 s.
fn facts_call(cmd: String) -> J {
    call_item(
        "exec_command",
        &J::obj([
            ("cmd", J::s(cmd)),
            ("yield_time_ms", J::Int(1000)),
            ("login", J::Bool(false)),
        ]),
    )
}

/// The facts leg's first call: a process that takes [`FACTS_LOCK`], writes its pid and stops
/// itself (`SIGSTOP`) holding the lock, so nothing but the proof's kill ever lets it go.
fn facts_holder_call() -> J {
    facts_call(format!(
        "exec perl -MFcntl=:flock -e '$| = 1; open(my $l, \">>\", \"{FACTS_LOCK}\") or die \"open: $!\"; \
         flock($l, LOCK_EX) or die \"flock: $!\"; open(my $p, \">\", \"{FACTS_HOLDER_PID}\") or die \"pid: $!\"; \
         print $p \"$$\\n\"; close $p; print \"holding {FACTS_LOCK}\\n\"; kill \"STOP\", $$; sleep 3600'"
    ))
}

/// The facts leg's second call (the one aterm's fact names): a process that writes its pid,
/// prints [`FACTS_TICK_COUNT`] ticks [`FACTS_TICK_MS`] apart into [`FACTS_TICKS`], then blocks in
/// `flock` on [`FACTS_LOCK`] behind the stopped holder: unnamed and quiet, the shape the jobs
/// engine reads `no_progress` once φ reaches 8 over its own μ (`aterm-vitals`'
/// `a_codex_pipe_call_blocked_on_a_stopped_holders_lock_reads_no_progress_after_its_rhythm`).
fn facts_waiter_call() -> J {
    let tick = format!("{}.{:03}", FACTS_TICK_MS / 1000, FACTS_TICK_MS % 1000);
    facts_call(format!(
        "exec perl -MFcntl=:flock -e '$| = 1; open(my $p, \">\", \"{FACTS_WAITER_PID}\") or die \"pid: $!\"; \
         print $p \"$$\\n\"; close $p; for my $i (1..{FACTS_TICK_COUNT}) {{ print \"tick $i\\n\"; \
         select(undef, undef, undef, {tick}) }} open(my $l, \">>\", \"{FACTS_LOCK}\") or die \"open: $!\"; \
         flock($l, LOCK_EX) or die \"flock: $!\"; print \"locked\\n\"' >{FACTS_TICKS} 2>&1"
    ))
}

/// How many of a request's user texts are a continuation's ([`step_of`]'s Continue words).
fn continuations(body: &J) -> usize {
    user_texts(body)
        .iter()
        .filter(|t| t.contains(CONTINUE_TEXT) || t.contains(KEEP_GOING))
        .count()
}

/// The approval and box legs' call: an escalated `exec_command` under `on-request`.
fn approval_call() -> J {
    call_item(
        "exec_command",
        &J::obj([
            ("cmd", J::s("echo approved-run >> ran.txt; echo done")),
            ("sandbox_permissions", J::s("require_escalated")),
            ("justification", J::s("write the marker file")),
        ]),
    )
}

/// How many escalated commands a `lab codex-boxes` run's model asks for, one a response.
pub const BOXES: usize = 20;

/// What a `codex-boxes` run's model says once its last command ran.
pub const BOXES_DONE: &str = "Ran all twenty commands, one at a time.\n\nATERM-TASK-DONE";

/// Each `codex-boxes` run's held responses, by leg and box: the lab sends one only once it holds
/// the tab ([`release_box`]), so codex-alab's checks wait and the box goes to codex's TUI.
static BOX_GATES: OnceLock<Mutex<BoxGates>> = OnceLock::new();

/// Each held response's leg, its box (from 0) and its gate.
type BoxGates = Vec<(String, usize, Arc<ResponseGate>)>;

fn box_gate(leg: &str, k: usize) -> Arc<ResponseGate> {
    let mut gates = BOX_GATES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, _, gate)) = gates.iter().find(|(l, n, _)| l == leg && *n == k) {
        return Arc::clone(gate);
    }
    let gate = Arc::new(ResponseGate::default());
    gates.push((leg.to_string(), k, Arc::clone(&gate)));
    gate
}

/// Send the response that asks for `leg`'s box `k` (from 0), which the model holds until then.
pub fn release_box(leg: &str, k: usize) {
    box_gate(leg, k).release();
}

/// The tag of the request box `k` (from 0) answers: `box01` to `box20`.
#[must_use]
pub fn box_tag(k: usize) -> String {
    format!("box{:02}", k + 1)
}

/// Box `k`'s call: an escalated `exec_command` under `on-request`, as the approval leg's.
fn box_call(k: usize) -> J {
    let n = k + 1;
    call_item(
        "exec_command",
        &J::obj([
            ("cmd", J::s(format!("echo box-{n} >> ran.txt; echo done"))),
            ("sandbox_permissions", J::s("require_escalated")),
            ("justification", J::s(format!("write box {n}'s marker"))),
        ]),
    )
}

/// How many tool outputs `body` carries: the boxes a `codex-boxes` run's model has had answered.
fn tool_outputs(body: &J) -> usize {
    body.get("input")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|it| it.get("type").and_then(J::as_str) == Some("function_call_output"))
        .count()
}

/// A `codex-boxes` leg's answer (`boxes<n>`): box `k` while `k` of [`BOXES`] ran, each held until
/// the lab releases it, then [`BOXES_DONE`] to every later request.
fn boxes_answer(body: &J, leg: &str) -> Answered {
    let k = tool_outputs(body);
    let (mut a, name) = if k < BOXES {
        let mut a = Answered::items(vec![box_call(k)]);
        a.gate = Some(box_gate(leg, k));
        (a, box_tag(k))
    } else {
        (
            Answered::items(vec![message_item(BOXES_DONE)]),
            "done".to_string(),
        )
    };
    a.tag = format!("{leg}:{name}");
    a
}

/// Computes the fake model's [`Answered`] response for `body`.
#[must_use]
fn answer(body: &J, resets: &Mutex<Vec<(String, i64)>>, now: &dyn Fn() -> i64) -> Answered {
    let leg = leg_of(body).unwrap_or_else(|| "-".to_string());
    if let Some(answer) = policy::answer(body, &leg) {
        return answer;
    }
    if let Some(answer) = terminals::answer(body, &leg) {
        return answer;
    }
    let say = |text: &str| Answered::items(vec![message_item(text)]);
    let slow = |text: &str, delay: Duration| {
        let mut a = say(text);
        a.delay = delay;
        a
    };
    let format = body.get("text").and_then(|t| t.get("format"));
    if format.and_then(|f| f.get("type")).and_then(J::as_str) == Some("json_schema") {
        let schema = format.and_then(|f| f.get("schema"));
        let mut a = say(&crate::stub::schema_answer(schema));
        a.tag = format!("{leg}:side");
        return a;
    }
    if leg.starts_with("boxes") {
        return boxes_answer(body, &leg);
    }
    let (user, tool) = request_view(body);
    let step = step_of(body);
    let mut name = step.name();
    if user.contains(NEEDS_ACK) {
        name.push_str("+ack");
    }
    if user.contains(REPO_SUFFIX_HEAD) {
        name.push_str("+repo");
    }
    if user.contains(turn_end::FACT_OPEN) {
        name.push_str(FACT_TAG);
    }
    name.push_str(contradiction_tag(&user));
    let mut a = match (leg.as_str(), step) {
        ("first", Step::Person) => say(FIRST_DONE),
        ("worker", Step::Person) => say(WORKER_REPORT),
        ("worker", Step::Land) => {
            let mut a = say(LAND_DEFENSE);
            if WORKER_LAND_HELD.load(Ordering::SeqCst) {
                a.gate = Some(Arc::clone(WORKER_LAND.get_or_init(Default::default)));
            }
            a
        }
        ("needs", Step::Person) => say(NEEDS_REPORT),
        ("needs", Step::Continue | Step::Other) if texts_after_prompt(body) == 1 => {
            slow(NEEDS_PLAIN, NEEDS_HOLD)
        }
        ("needs", Step::Continue | Step::Other) => say(NEEDS_CLEARED),
        ("continue", Step::Person) => say(STEP_ONE),
        ("continue", Step::Continue) if tool.is_none() && continuations(body) == 1 => {
            name = "fail".to_string();
            Answered::items(vec![failing_call()])
        }
        ("facts", Step::Person) if tool_outputs(body) == 0 => {
            name = "holder".to_string();
            Answered::items(vec![facts_holder_call()])
        }
        ("facts", Step::Person) if tool_outputs(body) == 1 => {
            name = "waiter".to_string();
            Answered::items(vec![facts_waiter_call()])
        }
        ("facts", Step::Person) => slow(FACTS_STARTED, SLOW_TURN),
        ("facts", Step::Continue) => slow(&facts_step(continuations(body)), SLOW_TURN),
        ("hold", Step::Person) => slow(STEP_ONE, HOLD_FIRST_TURN),
        ("off", Step::Person) => slow(STEP_ONE, SLOW_TURN),
        ("exchange" | "control", Step::Person) => slow(EXCHANGE_REPLY, SLOW_TURN),
        ("continue" | "hold" | "off" | "control", Step::Continue) => say(CONTINUED_DONE),
        ("carry", Step::Person) => slow("The long task's first step.", CARRY_FIRST_TURN),
        ("carry", Step::Continue) => say(CARRIED_DONE),
        ("approval" | "box", _) if tool.is_some() => {
            name = "after-output".to_string();
            say("The command ran.")
        }
        ("approval" | "box", Step::Person) => {
            name = "call".to_string();
            Answered::items(vec![approval_call()])
        }
        ("question", _) if tool.is_some() => {
            name = "after-answer".to_string();
            say(QUESTION_PROSE)
        }
        ("question", Step::Person) => {
            name = "ask".to_string();
            Answered::items(vec![question_call()])
        }
        ("question", Step::LandAnswer) => say(QUESTION_LANDED_WORDS),
        ("usage", Step::Person) => {
            let reset = reset_of(resets, &leg, now);
            name = format!("limit resets_at={reset}");
            Answered {
                status: 429,
                kind: "application/json",
                body: crate::stub::usage_limit_body("five_hour", reset, Some("plus")).py(),
                headers: crate::stub::usage_limit_headers("five_hour", reset),
                delay: Duration::ZERO,
                tag: String::new(),
                gate: None,
            }
        }
        ("usage", Step::Cleared) => say("Resumed after the reset."),
        ("land", Step::Person) => say("Started on the landing."),
        ("land", Step::Land) => {
            let (text, hold) = land_reply(0);
            slow(text, hold)
        }
        ("land", Step::Nudge(n)) => {
            let (text, hold) = land_reply(n);
            slow(text, hold)
        }
        ("reland", Step::Person) => slow("Started on the landing.", SLOW_TURN),
        ("reland", Step::Reopen) => slow("Looked again at the landed work.", SLOW_TURN),
        ("repo", Step::Person) => slow(REPO_WROTE, SLOW_TURN),
        ("repo", Step::Land) => say(REPO_DEFENSE),
        ("repo", Step::Nudge(1)) => slow(REPO_COMMITTED, REPO_COMMIT_HOLD),
        ("repo", Step::Nudge(_)) => slow(REPO_PUSHED, REPO_PUSH_HOLD),
        (_, Step::Defend | Step::RetiredCheck) => say(CRUISE_DEFENSE),
        (_, Step::Land | Step::Nudge(_)) => say(LAND_DEFENSE),
        (_, Step::LandAnswer) => say("Took the option that lands it soonest."),
        (_, Step::LandCheck) => say(LANDED_ANSWER),
        (_, Step::Continue) => say("Kept going: the next step is in hand."),
        _ => say(&format!(
            "Hello from the lab model. Nothing else to do.\n\n{FINISHED}"
        )),
    };
    a.tag = format!("{leg}:{name}");
    a
}

#[cfg(test)]
#[path = "codex_alab_model_tests.rs"]
mod tests;
