// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The approval policy in the loop: every box a look finds is decided by
//! [`decide`] — the ONE decider, under the owner's `[harness] approve`
//! level (full power by default) — and an approval is pressed under the
//! guard it returns ([`Session::press_one_guarded`], or, for a box answered
//! by its focus — an unnumbered box, the usage-limit dialog's wait row — the
//! confirmed focus move of [`Session::press_focused`]). A DECLINE
//! ([`Decision::Decline`]) is carried out keystroke by keystroke from fresh
//! reads ([`Session::press_decline`]: one keystroke in flight, never written
//! twice, and none while a person keys the session; the derived model
//! `SupervisorDeclineKeys`) and, like an approval, is an answer: noted,
//! ledgered `declined` with the text typed, journaled `DECLINED seq=<n>
//! rule=<id> <subject> => <text>` — no badge, no mail, nobody waited on —
//! and the loop looks again once the box has left. The same box back after
//! [`REASKED_AFTER`] declines since the last review point is the worker not
//! acting on its reason yet: declined again after [`reask_pause`], as a
//! re-asked question is answered again, never handed over for the count.
//!
//! What the loop adds around the pure decision is only what the pure
//! decision cannot know, read FRESH for every box it decides: the session's
//! foreground PROGRAM (`status program=`, lane C: the box is read by that
//! program's reader, so a shell showing a captured box is never judged by
//! Claude Code's grammar), its working directory (`meta cwd=`, for a box
//! that needs it — the rm circuit breaker and the folder-trust dialog; the
//! launch directory, which a trust dialog asks about), the permission mode
//! its footer showed on an earlier read (a 2.1.280 box replaces the footer),
//! the owner's home, uid and `$TMPDIR`, and the level, roots and question
//! switch [`SuperviseOpts::policy`] sets (`approve`, `answer_questions`).
//! Nothing is read under `approve = "none"` but for a question, which no
//! level limits: every other box escalates. Every
//! decision is a row of the approval ledger ([`approvals`]); a press no safe
//! rule proved carries `unproven: <why the safe rules did not prove it>` as
//! that row's reason, in the `--notes` line, and in a journal-only
//! `UNPROVEN` line.
//!
//! **The repeat cap** is the safe rules': the same command approved
//! [`MAX_APPROVALS_OF_ONE_COMMAND`] times since the last review point is
//! handed over on its next return under `approve = "safe"`. Under full power
//! a box that comes back is answered again — the worker asks, and nobody
//! else is there to (a press the box never took is caught below, not by a
//! count). A question is never counted by that cap: every question's parser
//! "command" is empty, so every tab of a dialog would be "the same prompt".
//!
//! **What an approval names.** The ledger row, the `--notes` line, the
//! `APPROVED` line and the escalation reason name the decision's SUBJECT
//! ([`shown_subject`]: the judged command, path or folder — a trust
//! dialog's backstop warnings, an rm breaker's resolved targets, the option
//! full power chose), one line; the parser's command only when a box has no
//! decision to name it.
//!
//! **The box must leave.** After a press the loop waits for the content to
//! move past it; a move that leaves the SAME box on the screen
//! ([`box_identity`]: its auto-deny countdown ticked or wiped, a blank row
//! gone, the transcript above it reflowed) is no move, and the wait goes on
//! — so a `1` the box never took is "the box did not change after the
//! press", not an approval pressed again each second (harness round-1
//! review, 2026-09-24), and a box the vendor redraws before it takes it
//! down is not pressed twice (measured live 2026-09-25, 2.1.283: every
//! approved rm box of the proof run was).
//!
//! Two things hold a box before it is judged or pressed. The vendor's key
//! guard: a box is judged only on a read made once it has shown
//! [`Session::box_settle`] since the loop FIRST read it ([`BOX_SETTLE`]) —
//! a younger one is waited on (`await seq`, bounded by what is left) and
//! read again, so the press is fenced on a fresh read, never on one a sleep
//! has made stale. And a person: an answer the policy has is held for
//! `human_grace_s` after their last keystroke ([`Session::held_for_person`]).
//!
//! **A question dialog** ([`RULE_ANSWER_RECOMMENDED`], `policy/question.rs`:
//! Enter on its recommended option, under `[harness] answer_questions`) is
//! keyed only when the things the pure decision cannot know hold, each read
//! fresh (the critique of 2026-09-25) — else the loop waits and decides
//! again on a fresh read, and never escalates for the wait
//! ([`Session::question_hold`]):
//!
//! 1. **A person has been quiet** (R1): the read's person stamp (`text
//!    --json`'s `"human_ms"`, the server's per-session record of a person's
//!    last gesture, [`HumanInput`]) is at least `human_grace_s` old, or no
//!    person has keyed the session. This is what covers a person's keys
//!    still in flight: the generation fence proves only that the SCREEN did
//!    not change, and a key written to the PTY that Claude Code has not read
//!    yet changes nothing on it. It covers the person evidence the decision
//!    cannot see, too (R2 (a) and (b): a focus a person moved, the review a
//!    person skipped to): answered only after their quiet. A host that sends
//!    no stamp gets the stand-in: the screen holding still
//!    [`QUESTION_STANDIN_QUIET`], once per box. The press asks again before
//!    every later key of the answer — each read inside it carries its own
//!    stamp, and `status` is asked after the first-key wait — and a person
//!    who keyed meanwhile ([`Press::Yielded`]) is waited for here in turn.
//! 2. **The loop's own last key took effect** (R3a): no further key while a
//!    read still shows the box as it was keyed — the same [`box_identity`]
//!    and the same focus. On such a read the loop waits for `await idle
//!    2000` and reads again; still unchanged, and only once the screen HELD
//!    STILL (the wait latched), the key is sent once more (a key the dialog
//!    refused inside its typeahead window is gone, O4) — counted as sent
//!    when it is written, not when it is decided. A key still in flight when
//!    a retry is written would answer the next tab with its vendor focus —
//!    option 1, where the tool puts its recommendation, or the review's
//!    `Submit answers`, never its cancel: the one residual, and why every
//!    retry waits for the screen to hold still first. Unchanged after that
//!    retry, the key is tried again for as long as the dialog stands, each
//!    try on the press back-off ([`Session::box_back_off`]: a pause that
//!    doubles from [`REFUSAL_PAUSE`] to a minute, the session badged — as
//!    information, the tries going on — once it has missed for
//!    [`PRESS_BADGE_AFTER`]); a screen that never holds still is waited on
//!    the same back-off, and never keyed until it does. Nobody is there to
//!    hand it to (the philosophy review of 2026-09-25, P3). `supervise`'s
//!    one look hands the box to the manager it returns to instead: after the
//!    one retry, or after [`MAX_UNSETTLED_WAITS`] waits that ran out.
//! 3. **Every check on a multi-select is the loop's own** (R2 (d)): it
//!    remembers the options it toggled, per question; a checked option it
//!    did not toggle — a recommended one included — or one it toggled that
//!    reads unchecked again is a person's answer begun, and handed over.
//! 4. **A re-asked question is answered again** (O5): the same question
//!    answered [`REASKED_AFTER`] times since the last review point is the
//!    model re-asking it, and nobody else is there to answer it — it is
//!    answered again after a pause that doubles with each further return,
//!    from [`REFUSAL_PAUSE`] to a minute ([`reask_pause`]), journaled
//!    `WAITING … re-asking`. No count hands it over.
//!
//! The press itself ([`Session::press_question`]) waits about a second after
//! a tab first appears before its first key (O4), moves the focus one row at
//! a time — each move seen landing before the next, never re-sent — and
//! presses Enter guarded on the focused row as drawn, every key under the
//! generation fence. A fenced key skipped because the screen moved
//! (`reason=changed`) settles and decides again, never presses again at
//! once. Every question decision, answered or escalated, journals the
//! dialog's rows (`DIALOG seq=<n> rule=<id> <rows>`, O3).
//!
//! The derived model `SupervisorQuestionAnswer` (aterm-spec
//! `supervisor_question_answer_model`) states 1 and 2 as guards over keys
//! split into their write and Claude Code's read of them, and proves that
//! every key meets the dialog as the read it was decided on showed it —
//! never the free-text field, a next tab, the review's cancel or the
//! composer — naming the one round trip between a stamp's read and the
//! write, and the retry's premise (a key is read by Claude Code before the
//! screen holds still 2 s after it), as the assumptions it rests on.
//! `tier1_a_question_key_goes_only_where_the_model_allows`
//! (`run_engine_tests.rs`) binds this loop to it.

use super::super::policy::WorkerEnv;
use super::super::policy::approval::{
    Answer, AnswerTarget, ApprovalCtx, Choice, Decision, DeclineStep, RULE_LIMIT_WAIT,
    RULE_READ_ONLY, decide,
};
use super::super::policy::approval::{
    NudgeCtx, RULE_GOAL_RESUME, RULE_RATE_NUDGE_KEEP, RULE_RATE_NUDGE_SWITCH,
};
use super::super::policy::question::{RULE_ANSWER_RECOMMENDED, begun};
use super::super::policy::turn_end::WindDown;
use super::*;
use crate::harness::upgrade_drive::{LiveTab, roster_rows, unique_tab_for_group};
use aterm_phase::{QuestionDialog, QuestionFocus, QuestionForm};
use aterm_types::domain::ENV_PARENT_SESSION_ID;

/// One answer since the last review point: the parser's command (the safe
/// rules' cap, [`MAX_APPROVALS_OF_ONE_COMMAND`]); for a key that ANSWERS a
/// question — a single-select or preview option, the multi-select button,
/// the review's submit; not a multi-select toggle — the question it answered
/// ([`question_key`], the re-ask back-off [`REASKED_AFTER`]); and for a
/// decline, the box it declined (`press.rs`'s `decline_box`: a box too tall
/// to read has no command), counted apart from approvals.
#[derive(Debug, Clone)]
pub(super) struct Approved {
    command: String,
    question: Option<String>,
    declined: Option<String>,
}

/// What the loop does with a box it answers.
enum Act {
    /// Press the approving option under `guard` ([`Decision::Approve`]).
    Approve { guard: String, choice: Choice },
    /// Refuse it amended with `text` ([`Decision::Decline`]).
    Decline { refusal: usize, text: String },
}

/// How often the same question is answered — or the same box declined —
/// since the last review point before its next return is the worker asking
/// again (O5): answered again, after [`reask_pause`].
const REASKED_AFTER: usize = 2;

/// The pause before a re-asked question answered `answered` times is
/// answered again: [`REFUSAL_PAUSE`] at the first re-ask, twice the last at
/// each next, at most [`PRESS_RETRY_MAX`].
pub(super) fn reask_pause(answered: usize) -> Duration {
    let n = u32::try_from(answered.saturating_sub(REASKED_AFTER)).unwrap_or(u32::MAX);
    crate::supervise::ladder::doubling(REFUSAL_PAUSE, n, PRESS_RETRY_MAX)
}

/// How many waits for the screen to hold still (`await idle 2000`, a
/// [`WAIT_STEP`] each) a key whose effect no read has shown gets before the
/// wait is backed off ([`Session::box_back_off`]) — or, in `supervise`'s one
/// look, the box handed to its manager: the retry's premise is a screen that
/// HELD STILL after the key (a key read by Claude Code by then), so a screen
/// that never does — a ticking row the box identity leaves out — is never
/// keyed again until it does.
const MAX_UNSETTLED_WAITS: u32 = 3;

/// How long the screen must hold still before a question is keyed on a host
/// that sends no person stamp ([`HumanInput::Unknown`]): the stand-in for a
/// person's quiet, waited once per box.
const QUESTION_STANDIN_QUIET: Duration = Duration::from_secs(10);

/// What the loop remembers of the question dialog it is answering
/// (module header, "A question dialog"): cleared when the worker works again
/// ([`Session::look`]).
#[derive(Debug, Default)]
pub(super) struct QuestionMemory {
    /// The question the first-key wait (O4) was spent on ([`question_key`]).
    pub(super) keyed: Option<String>,
    /// The loop's last question key whose effect no read has shown yet.
    pub(super) pending: Option<QuestionPending>,
    /// The box the stand-in quiet was waited out for (a host that sends no
    /// person stamp).
    pub(super) standin: Option<String>,
    /// The box the journal's last `WAITING … rule=answer-recommended@v1`
    /// line was said for: one line per box, not one per wait.
    pub(super) waiting_said: Option<String>,
    /// The options the loop itself toggled, per multi-select question
    /// ([`question_key`]), for the person evidence R2 (d): a check it did
    /// not make, or one it made that reads unchecked, is a person's.
    pub(super) toggled: std::collections::BTreeMap<String, std::collections::BTreeSet<u8>>,
    /// The re-asked question ([`question_key`]) and its answer count whose
    /// pause ([`reask_pause`]) was waited: answered on the next decision.
    pub(super) reasked: Option<(String, usize)>,
    /// The answers the loop's OWN Enters gave this dialog, as the decider
    /// names them (`<question> → <option>`), in order — the review's submit
    /// and a multi-select's toggles excepted: told ONCE as `CHOSE` when the
    /// worker works again ([`Session::tell_answered`]), with a `story chose`
    /// to the window (the chime and the pulse).
    pub(super) answered: Vec<String>,
    /// The dialog was a PERSON'S at some point — their input while it was up,
    /// a key yielded to them, or the dialog handed over: its end is theirs,
    /// never told as the policy's choice.
    pub(super) theirs: bool,
}

/// What the loop remembers of the decline it is carrying out
/// ([`Session::press_decline`]): cleared when the worker works again
/// ([`Session::look`]).
#[derive(Debug, Default)]
pub(super) struct DeclineMemory {
    /// The decline's last keystroke WRITTEN whose effect no read has shown
    /// yet: never written a second time.
    pub(super) pending: Option<DeclinePending>,
    /// The re-raised box (`press.rs`'s `decline_box`) and its decline count
    /// whose pause ([`reask_pause`]) was waited: declined on the next
    /// decision.
    pub(super) again: Option<(String, usize)>,
}

/// A decline keystroke written, as the box stood when it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeclinePending {
    /// The box it was written on (`press.rs`, `decline_box`).
    pub(super) key: String,
    /// The keystroke, as decided on the read it was written on: a read that
    /// decides the very same one shows the box as it was keyed.
    pub(super) step: DeclineStep,
    /// The reads since that showed the box as it was keyed.
    pub(super) unseen: u32,
}

/// A question key written, as the box and its focus stood when it was
/// written (R3a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QuestionPending {
    /// [`box_identity`] of the read the key was decided on.
    pub(super) identity: String,
    /// The focus the key was written on.
    pub(super) focus: QuestionFocus,
    /// The key was an Enter (an answer, or a multi-select toggle), not a
    /// focus move.
    pub(super) enter: bool,
    /// A read showed the box unchanged, and the screen then HELD STILL
    /// (`await idle 2000` latched) before the loop read again: the retry's
    /// premise.
    pub(super) waited: bool,
    /// Waits for the screen to hold still that ran out instead
    /// ([`MAX_UNSETTLED_WAITS`]).
    pub(super) unsettled: u32,
    /// How many times the key was WRITTEN again on this box and focus
    /// ([`Session::question_pending`] counts it when the key goes, never
    /// before).
    pub(super) retries: u32,
    /// The press back-off before the next try was waited
    /// ([`Session::box_back_off`]): the next read that still shows the box
    /// as keyed, the screen having held still, keys it again.
    pub(super) backed_off: bool,
}

/// What a question's answer waits on before it is keyed
/// ([`Session::question_hold`]).
enum Hold {
    /// Decide again after this (a person's quiet, the last key's effect, a
    /// re-ask's pause).
    Again,
    /// The question is the manager's, with the reason (`supervise`'s one
    /// look, and a person's begun answer).
    HandOver(String),
    /// Nobody is there to hand it to: decided again after the press
    /// back-off ([`Session::box_back_off`]), with the reason.
    BackOff(String),
}

/// One field of a bare `meta` reply, found BY KEY: `None` when the reply has
/// no such field (a build that predates it) or it is unset (`-`), else the
/// value, pct-decoded.
fn meta_word(stdout: &str, key: &str) -> Option<String> {
    let line = stdout.lines().find(|l| l.starts_with("OK"))?;
    let prefix = format!("{key}=");
    let raw = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))?;
    (raw != "-" && !raw.is_empty()).then(|| super::super::report::pct_decode(raw))
}

/// A question's identity for the first-key wait and the re-ask cap: its
/// form and its question text; the review tab's, its rows as drawn (its
/// answers).
pub(super) fn question_key(dialog: &QuestionDialog, rows: &[String]) -> String {
    let form = match &dialog.form {
        QuestionForm::Single { .. } => "single",
        QuestionForm::Multi { .. } => "multi",
        QuestionForm::Preview { .. } => "preview",
        QuestionForm::Review { .. } => "review",
        QuestionForm::Unknown { .. } => "unknown",
    };
    match dialog.question() {
        Some(q) => format!("{form}\n{}", q.text),
        None => format!("{form}\n{}", box_identity(rows).unwrap_or_default()),
    }
}

/// Whether an Enter on `target` ANSWERS the question (it moves on or
/// submits) rather than toggling a multi-select option.
fn answers(dialog: &QuestionDialog, target: AnswerTarget) -> bool {
    !matches!(
        (&dialog.form, target),
        (QuestionForm::Multi { .. }, AnswerTarget::Option(_))
    )
}

/// The dialog's rows as drawn, the rule above its tab bar to its footer,
/// joined into one journal line (O3: the incident's journal kept none).
fn dialog_rows(rows: &[String]) -> Option<String> {
    let v2 = aterm_phase::parse_prompt_v2(rows)?;
    let (top, bottom) = v2.span;
    let top = top.saturating_sub(1);
    let bottom = bottom.min(rows.len().checked_sub(1)?);
    (top <= bottom).then(|| {
        rows[top..=bottom]
            .iter()
            .map(|r| r.trim_end())
            .collect::<Vec<_>>()
            .join(" ⏎ ")
    })
}

/// `DIALOG seq=<n> rule=<id> <rows>`: a question decision's dialog, for the
/// journal alone (O3).
fn note_dialog(review: &mut dyn Review, seq: u64, rule_id: &str, rows: &[String]) {
    if let Some(drawn) = dialog_rows(rows) {
        review.note(&format!("DIALOG seq={seq} rule={rule_id} {drawn}"));
    }
}

/// What "the same box" is after a press ([`Session::box_left`]): the box's
/// kind and every row of it as drawn, title to footer, trimmed — its body
/// with it (an Edit's diff, a tool's arguments, a Chrome box's action in
/// its title, a workflow's script) — less the auto-deny countdown, which
/// ticks while the box waits: its `⚠` row and every row it wrapped onto in
/// a narrow grid (the harness round-2 review of 2026-09-24: aterm-phase
/// joins those into [`aterm_phase::PromptV2::auto_deny`], and the ticking
/// first row no longer equalled it, so each tick read as a new box). A row
/// is the countdown's while the rows so far spell the start of it, led by
/// its `⚠` row. The focus marker `❯` is left out as well: a focus move
/// changes it, and an identity that counted it read the moved box as a new
/// one (the harness round-3 review of 2026-09-24). So are its blank rows:
/// within 5 ms of a taken `1` the vendor (2.1.283) wipes the countdown and
/// ONE of the two blank rows under it, and keeps the box drawn some 550 ms
/// more — an identity that counted blank rows read that as the box leaving,
/// and the box still on the screen was decided and pressed a second time
/// (measured live 2026-09-25, every approved rm box of the proof run).
/// `None` with no box on `rows`.
pub(super) fn box_identity(rows: &[String]) -> Option<String> {
    let v2 = aterm_phase::parse_prompt_v2(rows)?;
    let (top, bottom) = v2.span;
    let countdown = v2.auto_deny.as_deref();
    let mut key = format!("{}\n", v2.kind.name());
    // What is left of the countdown to spell, once its `⚠` row is seen.
    let mut rest: Option<&str> = None;
    for r in rows.iter().take(bottom.saturating_add(1)).skip(top) {
        let t = r.trim();
        if t.is_empty() {
            continue;
        }
        let t = t.strip_prefix('❯').map_or(t, str::trim_start);
        let piece = t.trim_start_matches('⚠').trim();
        let left = match (rest, countdown) {
            (Some(left), _) => Some(left),
            (None, Some(c)) if t.starts_with('⚠') => Some(c),
            _ => None,
        };
        if let Some(left) = left.filter(|l| !piece.is_empty() && l.starts_with(piece)) {
            rest = Some(left[piece.len()..].trim_start()).filter(|l| !l.is_empty());
            continue;
        }
        rest = None;
        key.push_str(t);
        key.push('\n');
    }
    Some(key)
}

/// The label of the option the focus is on in the box on `rows`, if any.
pub(super) fn focused_label(program: Option<&str>, rows: &[String]) -> Option<String> {
    aterm_phase::read(program, rows, None)
        .prompt
        .and_then(|q| q.focused().map(|o| o.label.clone()))
}

/// The one line an approval names ([`Decision::Approve`]'s subject, its row
/// breaks as spaces), or `command` when the subject is empty.
fn shown_subject(subject: &str, command: &str) -> String {
    if subject.trim().is_empty() {
        command.to_string()
    } else {
        subject
            .split('\n')
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The box needs the session's working directory: the vendor's rm/rmdir
/// circuit breaker of any kind ([`aterm_phase::PromptV2::rm_breaker`]), the
/// folder-trust dialog, and a Bash box that may run git ([`may_run_git`]) —
/// and, where the worker is read from the session, every Bash box, whose
/// shell startup is read with the project's Claude Code settings
/// ([`ApprovalCtx::shell`]; the caller adds that).
fn needs_cwd(p: &aterm_phase::PromptV2) -> bool {
    p.kind == PromptKind::Trust || p.rm_breaker().is_some() || may_run_git(p)
}

/// A Bash box whose command may run git: its configuration is read where the
/// session's Bash tool stands as well ([`ApprovalCtx::shell_cwds`]).
fn may_run_git(p: &aterm_phase::PromptV2) -> bool {
    p.kind == PromptKind::Bash && p.command_rows.iter().any(|r| r.contains("git"))
}

/// The session's `s-…` id and the foreground process group of its PTY, off a
/// `who` reply's rows ([`roster_rows`], the live upgrade's one reading of the
/// roster): the row of `stable` — the `s-…` id the loop addresses (`@s-…`),
/// which no other session or instance reuses — where the loop has one, else
/// the row whose instance-local id is `local` (what `status sid=` named; a
/// local id is reused, and means this instance's session only). Refused when
/// the roster cannot be read, when the kernel could not read the group, and
/// when another row names the same group ([`unique_tab_for_group`]: a group
/// two PTYs claim is no session's alone).
pub(super) fn session_group(
    rows: &str,
    stable: Option<&str>,
    local: Option<&str>,
) -> Result<(String, u32), String> {
    let rows = roster_rows(rows).ok_or("`who` answered no roster this check can read")?;
    let sid = match (stable, local) {
        (Some(stable), _) => stable.to_string(),
        (None, Some(local)) => rows
            .iter()
            .find(|(id, _)| id.to_string() == local)
            .map(|(_, tab)| tab.sid.clone())
            .ok_or_else(|| format!("session {local} is not on the instance's roster (`who`)"))?,
        (None, None) => return Err("the session's id is unknown (`status` named none)".to_string()),
    };
    let tabs: Vec<LiveTab> = rows.into_iter().map(|(_, tab)| tab).collect();
    let group = tabs
        .iter()
        .find(|tab| tab.sid == sid)
        .ok_or_else(|| format!("session {sid} is not on the instance's roster (`who`)"))?
        .fgpgid
        .ok_or_else(|| format!("{sid}'s foreground process group is unknown"))?;
    if unique_tab_for_group(&tabs, group) != Some(sid.as_str()) {
        return Err(format!(
            "{sid}'s foreground process group {group} is another session's too"
        ));
    }
    let group = u32::try_from(group)
        .map_err(|_| format!("{sid}'s foreground process group {group} is out of range"))?;
    Ok((sid, group))
}

/// How long a box on a screen whose named program is no agent waits for
/// the server's own verdict (`await agent prompt`) before it is judged as
/// that program's.
const PROGRAM_SETTLE: Duration = Duration::from_millis(2000);

impl<C: Ctl> Session<'_, C> {
    /// The session's foreground program as `status program=` names it —
    /// `None` from a host that does not publish it, whose screen then names
    /// the reader ([`aterm_phase::identify`]) — and `None` too when the
    /// server's own verdict (`agent=`, lane C) already identified an agent
    /// on this screen while `program=` still names something else: the name
    /// is re-read off the server's event loop, and an agent `exec`ed in
    /// place keeps its launcher's name (`bash`) until the screen moves
    /// (measured on this lane's live check), while the verdict sticks to
    /// the process group it identified. A shell showing a captured box gets
    /// no verdict (`agent=-`), so it keeps its name and is never judged as
    /// an agent.
    ///
    /// The same read says when a PERSON last typed into the session
    /// (`human_ms=`, [`Session::person`]): every act the loop decides is
    /// decided on a status read made for it, so the person is never older
    /// than the decision.
    pub(super) fn foreground_program(&mut self) -> Result<Option<String>, Fail> {
        let r = self.call(&["status"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            self.person = None;
            self.status_sid = None;
            return Ok(None);
        }
        let field = |f: &str| super::escalate::status_field(&r.stdout, f).map(str::to_string);
        self.status_sid = field("sid").filter(|s| s != "-");
        self.person = field("human_ms")
            .and_then(|ms| ms.parse::<u64>().ok())
            .and_then(|ms| Instant::now().checked_sub(Duration::from_millis(ms)));
        // ANOTHER DRIVER'S HAND on the session — a drive lease, a turn a named
        // driver typed (`hand=lease:<holder>`, `hand=turn:<id>:<holder>`) —
        // holds the loop as a person's keystroke does: the manager driving
        // the worker answers it (the hazards review of 2026-09-25: the host
        // continued every turn a manager dispatched, and typed `answer_text`
        // over questions meant for the manager). An owner-class turn names
        // nobody: this loop's own continuations are typed so.
        // (A halt is the server's to refuse: `ERR halted` parks the loop.)
        self.driver = crate::harness::relaunch::driver_hand(&r.stdout);
        if self.driver {
            self.person = Some(Instant::now());
        }
        let program = field("program");
        let named_agent = program
            .as_deref()
            .and_then(aterm_phase::program_of)
            .is_some();
        Ok(if field("agent").is_some() && !named_agent {
            None
        } else {
            program
        })
    }

    /// The approval policy's context: the session's cwd (`meta` read fresh
    /// when `fresh_cwd`), for a box that may run git (`git`) the worker's
    /// environment ([`ApprovalCtx::worker`]) and the directories its Bash
    /// tool may stand in, from the transcripts under the worker's own Claude
    /// Code directory ([`ApprovalCtx::shell_cwds`]), for any Bash box
    /// (`bash`) the worker's environment and — when it is read from the
    /// session ([`WorkerSource::Session`]; a fixed one is a test's, whose
    /// startup [`ApprovalCtx::new`] already holds) — the startup its Bash
    /// tool's shell runs with ([`ApprovalCtx::shell`]), the footer's mode,
    /// the process's home/uid/`$TMPDIR`, the approve level and trust roots
    /// `opts` sets.
    fn approval_ctx(
        &mut self,
        fresh_cwd: bool,
        git: bool,
        bash: bool,
        opts: &SuperviseOpts,
        allow: &[String],
    ) -> Result<ApprovalCtx, Fail> {
        if fresh_cwd {
            let r = self.call(&["meta"])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
            }
            if r.ok() {
                self.note_cwd(&r.stdout);
            } else {
                self.cwd = None;
            }
        }
        let env = self.approval_env.clone();
        let cwd = self.cwd.clone();
        let mut ctx = ApprovalCtx::new(
            cwd.clone().unwrap_or_else(|| PathBuf::from("/")),
            env.home,
            env.uid,
            env.tmpdir,
        );
        ctx.cwd_known = cwd.is_some();
        ctx.set_trust_roots(&opts.policy.trust_roots, env.uid);
        // A safe rule a retired key took away (`config::Withheld`): its roots.
        if opts.policy.withheld.rm_breaker {
            ctx.scratch_roots.clear();
        }
        if opts.policy.withheld.read_outside_cwd {
            ctx.read_roots.clear();
        }
        ctx.bypass_mode = self.footer == Some(FooterMode::Bypass);
        ctx.approve = opts.policy.approve;
        ctx.answer_questions = opts.policy.answer_questions;
        ctx.model_fallback = opts.policy.model_fallback.is_some();
        ctx.model_restore = self.turn_end.restore_target(Instant::now());
        // A goal the live upgrade paused for its move, owed its resume: the
        // paused goal's box the relaunched Codex opens with is its to answer
        // — only that Codex's box, still leading its terminal and not left
        // to a person (`goal_hold::box_is_upgrades`: a person's own `codex
        // resume` in the tab draws a box of theirs), and never under an open
        // save-then-wait switch, whose session it is (the goal would run on
        // the cheaper model).
        ctx.goal_resume = !self.turn_end.switch_open() && self.goal_box_is_upgrades();
        ctx.limit_wait = opts.policy.limit_wait;
        ctx.python_allow = allow.to_vec();
        if git || bash {
            let session = matches!(env.worker, WorkerSource::Session);
            let worker = match env.worker {
                WorkerSource::Session => self.worker_env(),
                WorkerSource::Fixed(worker) => worker,
            };
            if git
                && let (Ok(w), Some(cwd)) = (&worker, cwd.as_deref())
                && let Some(claude) = w.claude_dir()
            {
                ctx.shell_cwds = crate::harness::footer::shell_cwds(&claude, cwd);
            }
            if bash && session {
                ctx.shell = match (&worker, cwd.as_deref()) {
                    (Ok(w), Some(cwd)) => crate::supervise::policy::shell_startup::read(w, cwd),
                    (Err(why), _) => Err(format!("the worker's environment is unknown: {why}")),
                    (Ok(_), None) => Err("the session's cwd is unknown (meta cwd=-)".to_string()),
                };
            }
            ctx.worker = worker;
        }
        Ok(ctx)
    }

    /// The WORKER's environment ([`WorkerEnv`]): the environment at exec of
    /// the session's foreground process group's leader — `who`'s `fgpgid=` on
    /// the session's row ([`session_group`]: by the `@s-…` the loop addresses,
    /// else by the local id the last `status` named) — read through the
    /// kernel ([`atpkg::caller_shell::process_args`]), and bound to this
    /// session by the `ATERM_PARENT_SESSION_ID` every aterm session hands its
    /// shell: a process whose variable names another session, or none, is not
    /// taken for the worker. `Err` says what could not be read or did not
    /// hold.
    pub(super) fn worker_env(&mut self) -> Result<WorkerEnv, String> {
        let stable = self
            .sid
            .as_deref()
            .and_then(|s| s.strip_prefix('@'))
            .filter(|s| s.starts_with("s-"))
            .map(str::to_string);
        let r = self
            .ctl
            .call(&["who"])
            .map_err(|e| format!("`who` failed: {e}"))?;
        if !r.ok() {
            return Err(format!("`who` refused: {}", r.stderr.trim()));
        }
        let (sid, group) = session_group(&r.stdout, stable.as_deref(), self.status_sid.as_deref())?;
        let args = atpkg::caller_shell::process_args(group).ok_or_else(|| {
            format!("the environment of {sid}'s foreground process {group} could not be read")
        })?;
        let worker = WorkerEnv::new(args).with_pid(group);
        match worker.var(ENV_PARENT_SESSION_ID) {
            Some(tab) if tab == sid => Ok(worker),
            Some(tab) => Err(format!(
                "{sid}'s foreground process {group} belongs to session {tab}"
            )),
            None => Err(format!(
                "{sid}'s foreground process {group} names no session \
                 ({ENV_PARENT_SESSION_ID} unset, or its environment hidden)"
            )),
        }
    }

    /// The decision on the box on `screen`: read by the reader for the
    /// session's foreground program, then [`decide`]d. Under `approve =
    /// "none"` nothing is read and the box escalates — but a question, or
    /// the usage-limit dialog while `limit_wait` is on, each its own rule's.
    pub(super) fn decide_box(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        allow: &[String],
    ) -> Result<Decision, Fail> {
        // A question is no permission, and neither is the usage-limit
        // dialog: no level limits either (only `answer_questions` and
        // `limit_wait`), so under `none` the screen is read (no request) for
        // one before anything is asked.
        let own_rule = || {
            aterm_phase::read(None, &screen.rows, None)
                .prompt
                .is_some_and(|p| {
                    p.kind == PromptKind::Question
                        || p.kind == PromptKind::UsageLimit && opts.policy.limit_wait
                })
        };
        if opts.policy.approve == Approve::None && !own_rule() {
            return Ok(Decision::Escalate {
                reason: "approve = \"none\": every box is the owner's".to_string(),
            });
        }
        if let Some(holder) = self.claim.watching_behind() {
            return Ok(Decision::Escalate {
                reason: format!("another supervisor ({holder}) answers this session"),
            });
        }
        self.program = self.foreground_program()?;
        let mut reading = aterm_phase::read(
            self.program.as_deref(),
            &screen.rows,
            Some(screen.cursor_col),
        );
        if reading.prompt.is_none() && aterm_phase::read(None, &screen.rows, None).prompt.is_some()
        {
            // The screen shows an agent's box and the named program is none:
            // a shell showing a capture — or an agent just started, whose
            // name the server resolves off its event loop (lane C). The
            // server's own verdict waits for the name: `await agent prompt`,
            // bounded, then the name is read once more.
            let settle = PROGRAM_SETTLE.as_millis().to_string();
            let r = self.call(&["await", "agent", "prompt", "timeout", &settle])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!(
                    "await agent failed: {}",
                    r.stderr.trim()
                )));
            }
            if r.ok() {
                self.program = self.foreground_program()?;
                reading = aterm_phase::read(
                    self.program.as_deref(),
                    &screen.rows,
                    Some(screen.cursor_col),
                );
            }
        }
        let git = reading.prompt.as_ref().is_some_and(may_run_git);
        let bash = reading
            .prompt
            .as_ref()
            .is_some_and(|p| p.kind == PromptKind::Bash);
        let fresh_cwd = reading.prompt.as_ref().is_some_and(needs_cwd)
            || bash && matches!(self.approval_env.worker, WorkerSource::Session);
        let mut ctx = self.approval_ctx(fresh_cwd, git, bash, opts, allow)?;
        // Codex's rate-limit nudge is answered by what Codex's own records
        // say of its usage window, and by the model the footer last showed
        // (the box covers the footer).
        if reading
            .prompt
            .as_ref()
            .is_some_and(|p| p.kind == PromptKind::RateNudge)
        {
            ctx.nudge = NudgeCtx {
                enabled: opts.policy.rate_nudge,
                // A press a restarted loop found only INTENDED (its key may
                // never have gone) leaves the nudge still up to be decided
                // again.
                open: self.turn_end.switch_landed(),
                from: self.nudge_from(),
                limits: self.codex_records().limits,
            };
        }
        // THE SESSION'S WORD (2026-09-25): a question dialog is answered by
        // this session's own `questions` word where it has one — `ask` hands
        // it to a person, `recommended` answers it — over the table's
        // `answer_questions`, but never over a loop's own `--no-answer` or a
        // value the reader could not take (`session_questions`). A dialog the
        // POLICY hands over is held: the wait for it re-reads the word
        // ([`Session::wait_for_next`]).
        self.question_held = false;
        let mut asked_by_session = false;
        if opts.policy.session_questions
            && reading
                .prompt
                .as_ref()
                .is_some_and(|p| p.kind == PromptKind::Question)
        {
            let (answers, by_session) = self.question_policy(opts)?;
            ctx.answer_questions = answers;
            asked_by_session = by_session && !answers;
            self.question_held = !answers;
        }
        Ok(match decide(&reading, &screen.rows, &ctx) {
            // The escalation names the word that decided it: the session's
            // own `ask`, not a switch the owner left on.
            Decision::Escalate { .. } if asked_by_session => Decision::Escalate {
                reason: "this session's `questions` word is ask: a question is the person's to \
                         answer"
                    .to_string(),
            },
            decision => decision,
        })
    }

    /// A question dialog the loop's own keys answered, done (the worker works
    /// again) with no person taking part: said ONCE — one `CHOSE seq=<n>
    /// rule=answer-recommended@v1 policy=recommended <question → answer | …>`
    /// line, printed and journaled, told to the window as `story chose
    /// recommended` (its chime and rim pulse: the owner's "a little sound and
    /// animation for choosing"), through a `watch`'s teller or, in the hosted
    /// loop that has none, one `story` request of its own.
    pub(super) fn tell_answered(
        &mut self,
        answered: &[String],
        seq: u64,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let said = answered.join(" | ");
        let line = format!(
            "CHOSE seq={seq} rule={RULE_ANSWER_RECOMMENDED} policy=recommended {}",
            clip(&one_line(&said))
        );
        review.say(&line).map_err(Fail::Hard)?;
        if self.tell_own
            && let Some((verb, text)) = story_of(&line)
        {
            // Best-effort, as the teller's post is: a host without the word
            // answers `ERR usage`, and nothing printed changes.
            let _ = self.call(&["story", verb, &text]);
        }
        append_note(
            opts.notes.as_deref(),
            &format!("chose ({RULE_ANSWER_RECOMMENDED}, recommended): {said}"),
        )?;
        Ok(())
    }

    /// Whether this session's question dialog is ANSWERED: its own
    /// `questions` word (`recommended` yes, `ask` no), read BY KEY off a bare
    /// `meta` reply (its `cwd=` noted on the way), else the table's switch
    /// (`[harness] answer_questions`). A build whose meta has no such field,
    /// an unset one (`-`) and a refused read all fall to the table's switch.
    pub(super) fn question_policy_answers(&mut self, opts: &SuperviseOpts) -> Result<bool, Fail> {
        Ok(self.question_policy(opts)?.0)
    }

    /// [`Self::question_policy_answers`], and whether the SESSION'S word
    /// decided it (`false`: the global switch did).
    fn question_policy(&mut self, opts: &SuperviseOpts) -> Result<(bool, bool), Fail> {
        let global = opts.policy.answer_questions;
        let r = self.call(&["meta"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("meta failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            return Ok((global, false));
        }
        self.note_cwd(&r.stdout);
        Ok(match meta_word(&r.stdout, "questions").as_deref() {
            Some("recommended") => (true, true),
            Some("ask") => (false, true),
            _ => (global, false),
        })
    }

    /// One row of the approval ledger.
    pub(super) fn ledger_row(
        &mut self,
        rule_id: &str,
        outcome: approvals::Outcome,
        command: &str,
        reason: &str,
        box_seq: u64,
    ) {
        self.ledger.write(
            &approvals::Row {
                rule_id,
                outcome,
                command,
                reason,
                box_seq,
            },
            &mut std::io::stderr(),
        );
    }

    /// How much longer the box on `turn` must show before it is judged
    /// ([`BOX_SETTLE`]: the vendor refuses a key sooner, and re-arms its
    /// refusal on it), counted from the read that FIRST showed it — `None`
    /// once it has shown that long. A box seen before (held for a person,
    /// judged again after a moved screen) waits no more.
    fn box_young(&mut self, turn: &Turn, allow: &[String]) -> Option<Duration> {
        let key = review_key(turn, allow);
        let first = match &self.box_first_seen {
            Some((k, at)) if *k == key => *at,
            _ => {
                let now = Instant::now();
                self.box_first_seen = Some((key, now));
                now
            }
        };
        let wait = self.box_settle.saturating_sub(first.elapsed());
        (!wait.is_zero()).then_some(wait)
    }

    /// Whether a person holds the act this look was about to take (`act`,
    /// for the journal): they typed into the session within `human_grace_s`
    /// of the last status read — made now when `fresh` ([`Self::
    /// foreground_program`]). Held, nothing is pressed, typed or escalated —
    /// the person is right there — the loop notes `HELD`, and the point is
    /// looked at again once the grace has passed ([`Self::turn_end_now`]).
    pub(super) fn held_for_person(
        &mut self,
        seq: u64,
        act: &str,
        fresh: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if fresh {
            self.program = self.foreground_program()?;
        }
        let Some(until) = self.hands_off_until(opts) else {
            return Ok(false);
        };
        let who = if self.driver {
            "another driver's hand is on the session"
        } else {
            "a person is typing"
        };
        review.note(&format!(
            "HELD seq={seq} {who}: hands off until {} ({act})",
            utc_stamp(
                u64::try_from(self.now_unix()).unwrap_or(0)
                    + until.saturating_duration_since(Instant::now()).as_secs()
            )
        ));
        self.turn_end_due = Some((until, "a person is typing".to_string()));
        self.held = true;
        Ok(true)
    }

    /// Until when the loop keeps its hands off the session: a person typed
    /// into it within `human_grace_s` of the last status read
    /// ([`Self::person`]). `None`: no person, or the grace has passed.
    pub(super) fn hands_off_until(&self, opts: &SuperviseOpts) -> Option<Instant> {
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        self.person
            .and_then(|typed| typed.checked_add(grace))
            .filter(|until| *until > Instant::now())
    }

    /// One turn under the approval policy. A box [`decide`] approves is
    /// pressed under its guard (noted, ledgered, and reported to `review`)
    /// and the loop looks again once the box has LEFT; a box it escalates —
    /// with the reason kept for the escalation's text ([`Session::box_reason`])
    /// — is a review point, and so is anything that is not a box, the same
    /// approval back after two presses, a press whose box row aterm could not
    /// find on this very screen (its guard matched no row), a box that did
    /// not move after the press, and a fallback press withheld because the
    /// session survey is open ([`Pressing::Withheld`]). A press the server skipped because the box
    /// left or the fenced screen moved, parked on a hold, or backed off
    /// after `ERR busy …`/`ERR rate`, is followed by a fresh read and a fresh
    /// decision — never the same press again. When the connection is lost
    /// before the press is seen through, what the loop knows it did is
    /// noted — a `1` the server confirmed is an approval, a press whose
    /// answer never came is noted as such and is not — and the loss is
    /// ridden out.
    pub(super) fn auto_read(
        &mut self,
        turn: Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        approved: &mut Vec<Approved>,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let box_on = (turn.phase == Phase::Prompt)
            .then(|| self.reader(&turn.screen.rows).prompt(&turn.screen.rows))
            .flatten();
        let Some(p) = box_on else {
            return Ok(Step::Review(turn));
        };
        let seq = turn.screen.seq;
        // Too young to judge: waited on, bounded by what is left of the
        // settle, and read again.
        if let Some(wait) = self.box_young(&turn, allow) {
            self.wait(&["seq", &seq.to_string()], wait)?;
            return Ok(Step::Again { settle: false });
        }
        let notes = opts.notes.as_deref();
        let decision = self.decide_box(&turn.screen, opts, allow)?;
        // The `status` that named the program says the worker is not reading
        // its input: this box is neither pressed nor escalated; the next look
        // holds (`stall.rs`).
        if self.stall_in_hand(review) {
            return Ok(Step::Again { settle: false });
        }
        // A person at the keyboard wins: an answer the policy has is held
        // until their grace has passed — on the status read the decision
        // was made on — and neither pressed nor escalated.
        if matches!(
            decision,
            Decision::Approve { .. } | Decision::Decline { .. }
        ) && self.held_for_person(seq, "the box's answer", false, opts, review)?
        {
            return Ok(Step::Review(turn));
        }
        let (rule_id, act, subject, unproven) = match decision {
            Decision::Escalate { reason } => {
                append_note(
                    notes,
                    &format!("handed to the manager ({reason}): {}", p.command),
                )?;
                self.ledger_row("-", approvals::Outcome::Escalated, &p.command, &reason, seq);
                if p.kind == PromptKind::Question {
                    note_dialog(review, seq, "-", &turn.screen.rows);
                    // Handed to a person for a reason of its own — not the
                    // POLICY holding it (`question_held`), which a hand-back
                    // lifts: answered then, the answer is still the policy's.
                    if !self.question_held {
                        self.question.theirs = true;
                    }
                }
                self.box_reason = Some((review_key(&turn, allow), reason));
                return Ok(Step::Review(turn));
            }
            Decision::Approve {
                rule_id,
                guard,
                choice,
                subject,
                unproven,
            } => (rule_id, Act::Approve { guard, choice }, subject, unproven),
            Decision::Decline {
                rule_id,
                refusal,
                text,
                subject,
            } => (rule_id, Act::Decline { refusal, text }, subject, None),
        };
        // What the lines name (module header): the decision's subject.
        let what = shown_subject(&subject, &p.command);
        // A question's answer: its dialog read again from the same rows —
        // the key's bookkeeping needs it — and the waits the pure decision
        // cannot know (module header, "A question dialog").
        let (question, toggle) = match &act {
            Act::Approve {
                choice: Choice::Answer(Answer::FocusEnter { target, .. }),
                ..
            } => {
                let Some(dialog) =
                    aterm_phase::parse_prompt_v2(&turn.screen.rows).and_then(|v| v.question_dialog)
                else {
                    let why = "the question dialog was not read again from the same rows";
                    self.question.theirs = true;
                    append_note(notes, &format!("handed to the manager ({why}): {what}"))?;
                    self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, why, seq);
                    self.box_reason = Some((review_key(&turn, allow), why.to_string()));
                    return Ok(Step::Review(turn));
                };
                let identity = box_identity(&turn.screen.rows).unwrap_or_else(|| what.clone());
                let answering = answers(&dialog, *target);
                let this_question = question_key(&dialog, &turn.screen.rows);
                let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
                match self.question_hold(
                    &turn,
                    &dialog,
                    &identity,
                    answering.then_some(this_question.as_str()),
                    approved,
                    grace,
                    review,
                )? {
                    None => {}
                    Some(Hold::Again) => return Ok(Step::Again { settle: true }),
                    Some(Hold::BackOff(why)) => {
                        return self.box_back_off(&turn, allow, &what, &why, review);
                    }
                    Some(Hold::HandOver(why)) => {
                        self.question.theirs = true;
                        append_note(notes, &format!("handed to the manager ({why}): {what}"))?;
                        self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, &why, seq);
                        note_dialog(review, seq, rule_id, &turn.screen.rows);
                        self.box_reason = Some((review_key(&turn, allow), why));
                        return Ok(Step::Review(turn));
                    }
                }
                let toggle = match (&dialog.form, *target) {
                    (QuestionForm::Multi { .. }, AnswerTarget::Option(n)) => {
                        Some((this_question.clone(), n))
                    }
                    _ => None,
                };
                (answering.then_some(this_question), toggle)
            }
            _ => (None, None),
        };
        let answer_named = question.is_some();
        // A decline's box as the decline knows it: the same across its own
        // keystrokes, and a box too tall to read has no command.
        let declined = match &act {
            Act::Decline { .. } => {
                let key = super::press::decline_box(&p, &turn.screen.rows);
                if let Some(pause) = self.decline_again(&key, approved) {
                    review.note(&format!(
                        "WAITING seq={seq} rule={rule_id} the same box came back after its \
                         declines since the last review point (the worker has not acted on its \
                         reason yet): declined again in {} ms",
                        pause.as_millis()
                    ));
                    self.wait(&["seq", &seq.to_string()], pause)?;
                    return Ok(Step::Again { settle: true });
                }
                Some(key)
            }
            Act::Approve { .. } => None,
        };
        // Under full power a box that comes back is answered again (module
        // header): the cap is the safe rules', on the parser's command — a
        // question's has its own ([`Self::question_hold`]), and a decline's
        // ([`Self::decline_again`]).
        let repeats = approved
            .iter()
            .filter(|a| a.declined.is_none() && a.command == p.command)
            .count();
        if opts.policy.approve != Approve::All
            && rule_id != RULE_ANSWER_RECOMMENDED
            && declined.is_none()
            && repeats >= MAX_APPROVALS_OF_ONE_COMMAND
        {
            let why = "the same prompt came back after two approvals";
            append_note(notes, &format!("handed to the manager ({why}): {what}"))?;
            self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, why, seq);
            self.box_reason = Some((review_key(&turn, allow), why.to_string()));
            return Ok(Step::Review(turn));
        }
        // A press no safe rule proved says it was waved through, and why; a
        // decline, the text the worker was given.
        let unproven = unproven.map(|u| format!("unproven: {u}"));
        let over = match &act {
            Act::Decline { text, .. } => text.clone(),
            Act::Approve { .. } => unproven.clone().unwrap_or_default(),
        };
        let approved_note = match (&act, &unproven) {
            (Act::Decline { text, .. }, _) => format!("declined ({rule_id}): {what} — {text}"),
            (Act::Approve { .. }, Some(u)) => format!("approved ({rule_id}; {u}): {what}"),
            (Act::Approve { .. }, None) if rule_id == RULE_READ_ONLY => {
                format!("approved read-only: {what}")
            }
            (Act::Approve { .. }, None) => format!("approved ({rule_id}): {what}"),
        };
        let grace = Duration::from_secs(u64::from(opts.policy.human_grace_s));
        // CODEX'S RATE-LIMIT NUDGE'S SWITCH: the save-then-wait switch it
        // opens is made, stamped as pressed, and ledgered as the press's
        // INTENT (`phase=intent`), BEFORE the key goes — a loop that dies
        // between the key and its record still carries the switch on (a
        // restart seeds it, and the footer then says whether the press
        // landed). It opens here only once the box is seen leaving; a press
        // that did not land closes it.
        let intent = if rule_id == RULE_RATE_NUDGE_SWITCH {
            let label = what.rsplit_once(" => ").map_or("", |(_, l)| l).to_string();
            let w = self.nudge_intent(&label);
            if let Some(w) = &w {
                self.nudge_intended(w, &what, seq);
            }
            w
        } else {
            None
        };
        let pressing = match &act {
            Act::Approve {
                guard,
                choice: Choice::Digit(n),
            } => self.press_one_guarded(&p, guard, &n.to_string(), &turn.screen)?,
            Act::Approve {
                guard,
                choice: Choice::Focus { steps, label },
            } => self.press_focused(&p, *steps, label, "enter", guard, &turn.screen)?,
            Act::Approve {
                guard,
                choice: Choice::FocusKey { steps, label, key },
            } => self.press_focused(&p, *steps, label, key, guard, &turn.screen)?,
            Act::Approve {
                choice: Choice::Answer(answer),
                ..
            } => self.press_question(answer, &turn.screen, grace)?,
            Act::Decline { refusal, text } => {
                self.press_decline(*refusal, text, &turn.screen, grace)?
            }
        };
        let this = Approved {
            command: p.command.clone(),
            question,
            declined,
        };
        if let Some(w) = &intent {
            match &pressing {
                // Seen through below: open once the box leaves.
                Pressing::Done(Press::Pressed { .. }) => {}
                // The key may have landed and nothing saw after it: the
                // switch is carried as only INTENDED — the footer says
                // whether it landed (`TurnEndState::footer_seen`, and at the
                // next point `TurnEndState::observe`); a box still up is
                // decided again, and nothing of the switch stops or types
                // anything before the footer shows the cheaper model.
                Pressing::Lost {
                    known: Some(Press::Pressed { .. }) | None,
                    ..
                } => self.open_wind(WindDown {
                    intent: true,
                    ..w.clone()
                }),
                _ => self.nudge_missed(w, &what, "the press was not sent", seq),
            }
        }
        let press = match pressing {
            Pressing::Done(press) => press,
            Pressing::Unconfirmed { why, retry } => {
                if retry && self.retries_presses(opts, review, rule_id) {
                    return self.press_missed(&turn, allow, &what, &why, review);
                }
                let why = if this.declined.is_some() {
                    format!("the decline could not be carried out: {why}")
                } else {
                    why
                };
                append_note(notes, &format!("handed to the manager ({why}): {}", what))?;
                self.ledger_row(rule_id, approvals::Outcome::Escalated, &what, &why, seq);
                if rule_id == RULE_ANSWER_RECOMMENDED {
                    self.question.theirs = true;
                    note_dialog(review, seq, rule_id, &turn.screen.rows);
                }
                self.box_reason = Some((review_key(&turn, allow), why));
                return Ok(Step::Review(turn));
            }
            Pressing::Withheld => {
                append_note(
                    notes,
                    &format!(
                        "handed to the manager (the session survey is open and this host has \
                         no guarded press: an unguarded 1 could rate the session): {what}"
                    ),
                )?;
                return Ok(Step::Review(turn));
            }
            Pressing::Halted { why } => {
                self.ledger_row(rule_id, approvals::Outcome::Deferred, &what, &why, seq);
                self.park_on_hold(&why, seq, review)?;
                return Ok(Step::Again { settle: false });
            }
            Pressing::Refused { why } => {
                self.ledger_row(rule_id, approvals::Outcome::Deferred, &what, &why, seq);
                let anchor = match &act {
                    Act::Approve { guard, .. } => Some(guard.as_str()),
                    Act::Decline { .. } => None,
                };
                self.back_off(&why, seq, anchor, review)?;
                return Ok(Step::Again { settle: false });
            }
            Pressing::Lost { known, why } => {
                match known {
                    Some(Press::Pressed { seq: at }) => {
                        if let Some((q, n)) = toggle {
                            self.question.toggled.entry(q).or_default().insert(n);
                        }
                        approved.push(this);
                        append_note(notes, &approved_note)?;
                        let why = ["pressed; the connection was lost after", &over]
                            .join(if over.is_empty() { "" } else { "; " });
                        self.answered(&act, rule_id, &what, &why, (at, seq), review)?;
                        note_unproven(review, at, rule_id, unproven.as_deref());
                    }
                    Some(
                        Press::Skipped { .. }
                        | Press::Changed { .. }
                        | Press::Unseen { .. }
                        | Press::Yielded { .. },
                    ) => append_note(
                        notes,
                        &format!("nothing pressed, the box had left the screen: {what}"),
                    )?,
                    None => append_note(
                        notes,
                        &format!(
                            "pressed, no answer came; the box is read again after the \
                             reconnect: {what}"
                        ),
                    )?,
                }
                return Err(Fail::Lost(why));
            }
        };
        self.refusal_pause = REFUSAL_PAUSE;
        match press {
            // A question's key: its effect is the next read's to show — the
            // next tab, a checkbox, the focus, the review gone (R3a, module
            // header) — never waited out as "the box must leave", which any
            // redraw satisfies.
            Press::Pressed { seq: at } if rule_id == RULE_ANSWER_RECOMMENDED => {
                self.changed_streak = 0;
                // An answer the loop's own Enter gave (not a toggle, not the
                // review's submit): told once when the dialog is done — a
                // retry of the same Enter (P3) told once with it.
                if answer_named
                    && !matches!(
                        &act,
                        Act::Approve {
                            choice: Choice::Answer(Answer::FocusEnter {
                                target: AnswerTarget::ReviewSubmit,
                                ..
                            }),
                            ..
                        }
                    )
                    && self.question.answered.last() != Some(&what)
                {
                    self.question.answered.push(what.clone());
                }
                // A multi-select toggle the loop made: its check is the
                // loop's, not a person's (R2 (d)).
                if let Some((q, n)) = toggle {
                    self.question.toggled.entry(q).or_default().insert(n);
                }
                approved.push(this);
                append_note(notes, &approved_note)?;
                self.ledger_row(rule_id, approvals::Outcome::Approved, &what, &over, seq);
                review.approved(at, &what)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                note_dialog(review, seq, rule_id, &turn.screen.rows);
                // Let the worker take the key before the next read: the
                // first content change after it, bounded.
                self.wait(&["seq", &at.to_string()], STRAY_SETTLE)?;
                Ok(Step::Again { settle: true })
            }
            Press::Yielded { seq: at } => {
                if rule_id == RULE_ANSWER_RECOMMENDED {
                    self.question.theirs = true;
                }
                // A person keyed the session while the answer was keyed:
                // nothing more was sent; the next read waits for their quiet
                // (`question_hold`) and decides again — never escalated for
                // the wait (R1).
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "a person gave the session input while the answer was keyed: decided again \
                     after their quiet",
                    at,
                );
                Ok(Step::Again { settle: true })
            }
            Press::Unseen { seq: at } => {
                // A focus move written, its effect not seen yet: nothing is
                // re-sent here; the next read decides (R3a).
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the dialog has not shown the key's effect yet",
                    at,
                );
                Ok(Step::Again { settle: true })
            }
            Press::Pressed { seq: at } => {
                self.changed_streak = 0;
                approved.push(this);
                // The nudge's switch is open on the screen already: a notes
                // file that cannot be written is said, never the end of the
                // loop before the switch is recorded.
                if let Err(e) = append_note(notes, &approved_note) {
                    if intent.is_none() {
                        return Err(e.into());
                    }
                    review.note(&format!("NOTES seq={seq} {e}"));
                }
                // Its approved row carries the switch too — still only
                // intended: the box has not been seen leaving yet.
                let over = match &intent {
                    Some(w) => format!("{over} {}", self.wind_words(w, "intent")),
                    None => over,
                };
                self.answered(&act, rule_id, &what, &over, (at, seq), review)?;
                note_unproven(review, at, rule_id, unproven.as_deref());
                // The box must LEAVE before the next look: `await gone` is
                // level-triggered and a box shows no busy footer, so an
                // immediate re-read of an unchanged screen would match the same
                // box and press it again — the stray digit the guard exists to
                // prevent.
                // Any move is not its leaving: the vendor redraws a box it
                // has taken (the countdown wiped, the transcript above
                // reflowed) before it takes it down ([`box_identity`]).
                let identity = box_identity(&turn.screen.rows).unwrap_or_default();
                let screen = match self.box_left(at, &identity, deadline)? {
                    Past::Moved => {
                        self.press_retry = None;
                        // The switch landed: it opens — the turn the box
                        // covered ended under it, and a person's keystroke
                        // before this is no hand in anything after — and the
                        // goal turn Codex runs under the box is stopped at
                        // once. The keep: the covered turn ended there too.
                        if let Some(w) = intent {
                            self.open_wind(w);
                            let screen = self.screen()?;
                            self.stop_codex_turn(&screen, review)?;
                        } else if rule_id == RULE_RATE_NUDGE_KEEP {
                            self.nudge_answered();
                        } else if rule_id == RULE_GOAL_RESUME {
                            self.goal_resume_pressed("moved");
                        }
                        return Ok(Step::Again { settle: true });
                    }
                    Past::Still(Some(screen)) => {
                        if let Some(w) = &intent {
                            self.nudge_missed(
                                w,
                                &what,
                                "the box did not change after the press",
                                seq,
                            );
                        }
                        screen
                    }
                    // The budget ran out waiting: nothing is read or handed
                    // over after it — the box as last read is the TIMEOUT's
                    // ([`Self::look`]). A nudge's switch that may have landed
                    // is carried as open, as its intent row says.
                    Past::Still(None) => {
                        if let Some(w) = intent {
                            self.open_wind(WindDown { intent: true, ..w });
                        }
                        return Ok(Step::Review(turn));
                    }
                };
                let why = "the box did not change after the press";
                let still = self.turn_of(screen);
                if self.retries_presses(opts, review, rule_id) {
                    return self.press_missed(&still, allow, &what, why, review);
                }
                append_note(notes, &format!("handed to the manager ({why}): {what}"))?;
                Ok(Step::Review(still))
            }
            Press::Changed { seq: at } => {
                // The screen moved between the read and the press: read and
                // decide again — and after a streak of these, hand the box
                // over rather than press it unfenced ([`MAX_CHANGED_PRESSES`]).
                self.changed_streak += 1;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the screen moved past the judged read",
                    at,
                );
                if self.changed_streak >= MAX_CHANGED_PRESSES {
                    self.changed_streak = 0;
                    let why = format!(
                        "{MAX_CHANGED_PRESSES} fenced presses in a row did not land (the screen \
                         moved, or the key was not taken)"
                    );
                    if self.retries_presses(opts, review, rule_id) {
                        return self.press_missed(&turn, allow, &what, &why, review);
                    }
                    append_note(notes, &format!("handed to the manager ({why}): {}", what))?;
                    self.box_reason = Some((review_key(&turn, allow), why));
                    return Ok(Step::Review(turn));
                }
                // A question's key skipped this way settles and is decided
                // again — a person's key may be what moved the screen, and
                // the quiet is checked again first — never pressed again at
                // once (R2).
                Ok(Step::Again {
                    settle: rule_id == RULE_ANSWER_RECOMMENDED,
                })
            }
            Press::Skipped { seq: at } => {
                self.changed_streak = 0;
                self.ledger_row(
                    rule_id,
                    approvals::Outcome::Skipped,
                    &what,
                    "the guarded row was not on the screen",
                    at,
                );
                if at == seq {
                    // The very screen we parsed, and the guard found no row:
                    // under the safe rules this box is not one the supervisor
                    // answers; at full power it is read and tried again.
                    let why = "aterm could not find the box's row";
                    if self.retries_presses(opts, review, rule_id) {
                        return self.press_missed(&turn, allow, &what, why, review);
                    }
                    append_note(notes, &format!("handed to the manager ({why}): {}", what))?;
                    self.box_reason = Some((review_key(&turn, allow), why.to_string()));
                    return Ok(Step::Review(turn));
                }
                append_note(
                    notes,
                    &format!("nothing pressed, the box had left the screen: {what}"),
                )?;
                Ok(Step::Again { settle: false })
            }
        }
    }
}

/// The longest pause between tries of a press that did not land at full
/// power ([`Session::press_missed`]); the first is [`REFUSAL_PAUSE`], each
/// next twice the last.
const PRESS_RETRY_MAX: Duration = Duration::from_secs(60);
/// How long a box whose presses keep missing is tried before the session is
/// badged — the tries going on behind the badge.
pub(super) const PRESS_BADGE_AFTER: Duration = Duration::from_secs(120);

/// A box whose press did not land, being tried again at full power
/// ([`Session::press_missed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PressRetry {
    /// The box ([`box_identity`]).
    key: String,
    tries: u32,
    since: Instant,
    badged: bool,
}

impl<C: Ctl> Session<'_, C> {
    /// Whether a press that did not land is tried again
    /// ([`Self::press_missed`]) rather than handed over: with nobody there
    /// to take it (`watch`, the window's host) — `supervise`'s one look
    /// hands it to the manager it returns to — at full power, or for a
    /// question's answer or the usage-limit dialog's wait (`rule_id`), which
    /// no `approve` level limits: only `answer_questions` or `limit_wait`,
    /// which already decided it.
    fn retries_presses(&self, opts: &SuperviseOpts, review: &dyn Review, rule_id: &str) -> bool {
        review.unattended()
            && (opts.policy.approve == Approve::All
                || rule_id == RULE_ANSWER_RECOMMENDED
                || rule_id == RULE_LIMIT_WAIT)
    }

    /// FULL POWER'S ANSWER TO A PRESS THAT DID NOT LAND (the philosophy review
    /// of 2026-09-25: the box did not change after the press, a streak of
    /// fenced presses that did not land, a focus move that did not land, or
    /// a box row aterm could not find on the very screen read (its guard
    /// matched no row) — each handed the box
    /// to a person, and the box was never tried again; headless, nobody could
    /// answer): [`Self::box_back_off`], journaled `WAITING seq=<n> the press
    /// did not land (<why>) …`.
    fn press_missed(
        &mut self,
        turn: &Turn,
        allow: &[String],
        what: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let why = format!("the press did not land ({why})");
        self.box_back_off(turn, allow, what, &why, review)
    }

    /// THE PRESS BACK-OFF, for a box nobody else is there to answer — a press
    /// that did not land ([`Self::press_missed`]), a question key the dialog
    /// did not take, a screen that never held still for the key to be sent
    /// again ([`Self::question_hold`]): the box is read and decided again
    /// after a pause that doubles from [`REFUSAL_PAUSE`] to
    /// [`PRESS_RETRY_MAX`] — Claude Code drops keys 0.3 s after it draws a
    /// box, a transient the next try passes — and once it has missed for
    /// [`PRESS_BADGE_AFTER`] the session is badged ([`Self::escalate_point`],
    /// a keyed badge that goes with the box) while the tries go on.
    /// Journaled `WAITING seq=<n> <why> (try <n>) …` each time.
    fn box_back_off(
        &mut self,
        turn: &Turn,
        allow: &[String],
        what: &str,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<Step, Fail> {
        let key = box_identity(&turn.screen.rows).unwrap_or_default();
        let now = Instant::now();
        if self.press_retry.as_ref().is_none_or(|r| r.key != key) {
            self.press_retry = Some(PressRetry {
                key,
                tries: 0,
                since: now,
                badged: false,
            });
        }
        let Some(r) = self.press_retry.as_mut() else {
            return Ok(Step::Again { settle: false });
        };
        r.tries = r.tries.saturating_add(1);
        let pause = crate::supervise::ladder::doubling(REFUSAL_PAUSE, r.tries - 1, PRESS_RETRY_MAX);
        let badge = !r.badged && now.duration_since(r.since) >= self.press_badge_after;
        r.badged |= badge;
        let tries = r.tries;
        let seq = turn.screen.seq;
        review.note(&format!(
            "WAITING seq={seq} {why} (try {tries}): read and tried again in {} ms: {}",
            pause.as_millis(),
            clip(what)
        ));
        if badge && review.unattended() {
            let reason = format!("{why}; still trying ({tries} tries)");
            self.escalate_point(turn, allow, Some(&reason), review)?;
        }
        self.wait(&["seq", &seq.to_string()], pause)?;
        Ok(Step::Again { settle: false })
    }

    /// What a question's answer waits on before it is keyed (module header,
    /// "A question dialog"): `None` to key it now, [`Hold::Again`] after a
    /// wait — a person not quiet for `grace` yet, the loop's own last key not
    /// seen taking effect yet — or [`Hold::HandOver`]. `identity` is the box
    /// as drawn ([`box_identity`]); `answering` the question an Enter on this
    /// decision answers ([`question_key`]; `None` for a multi-select toggle),
    /// for the re-ask cap.
    #[allow(clippy::too_many_arguments)]
    fn question_hold(
        &mut self,
        turn: &Turn,
        dialog: &QuestionDialog,
        identity: &str,
        answering: Option<&str>,
        approved: &[Approved],
        grace: Duration,
        review: &mut dyn Review,
    ) -> Result<Option<Hold>, Fail> {
        let seq = turn.screen.seq;
        let quiet_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX);
        // 1. A person has been quiet (R1).
        match turn.screen.human {
            HumanInput::Never => {}
            HumanInput::Ago(ms) if ms >= quiet_ms => {}
            HumanInput::Ago(ms) => {
                // A person's input after the loop's own keys reached the
                // dialog: whatever the loop answers after their quiet, the
                // dialog's end is not told as the policy's alone. (Input
                // before any key of the loop's is, as often as not, the
                // prompt that led to the question.)
                if self.question.pending.is_some() || !self.question.answered.is_empty() {
                    self.question.theirs = true;
                }
                self.note_question_wait(
                    review,
                    seq,
                    identity,
                    &format!(
                        "a person gave the session input {ms} ms ago: the question waits for {} \
                         s of their quiet",
                        grace.as_secs()
                    ),
                );
                // Woken early by any change: the next read re-checks.
                let left = Duration::from_millis(quiet_ms - ms);
                self.wait(&["seq", &seq.to_string()], left.min(WAIT_STEP))?;
                return Ok(Some(Hold::Again));
            }
            HumanInput::Unknown if self.question.standin.as_deref() == Some(identity) => {}
            HumanInput::Unknown => {
                self.note_question_wait(
                    review,
                    seq,
                    identity,
                    &format!(
                        "this host does not stamp a person's input: the question waits for the \
                         screen to hold still {} s",
                        QUESTION_STANDIN_QUIET.as_secs()
                    ),
                );
                let ms = QUESTION_STANDIN_QUIET.as_millis().to_string();
                match self.wait(&["idle", &ms], QUESTION_STANDIN_QUIET + SETTLE_CAP)? {
                    Wait::Latched | Wait::Unsupported => {
                        self.question.standin = Some(identity.to_string());
                    }
                    Wait::TimedOut => {}
                }
                return Ok(Some(Hold::Again));
            }
        }
        // 2. The loop's own last key took effect (R3a).
        let focus = dialog.focus();
        let unchanged = self
            .question
            .pending
            .clone()
            .filter(|p| p.identity == identity && p.focus == focus);
        // The multi-select toggle still in flight on this very box, if any:
        // its check may not be drawn yet (R2 (d) below).
        let toggling = match &unchanged {
            Some(QuestionPending {
                enter: true,
                focus: QuestionFocus::Option(n),
                ..
            }) => Some(*n),
            _ => None,
        };
        // Nobody to hand a key the dialog did not take to (module header,
        // 2): it is tried again on the press back-off instead.
        let unattended = review.unattended();
        // A key sent again on the box as it was keyed is no re-ask (3).
        let retrying = unchanged.is_some();
        match unchanged {
            Some(QuestionPending {
                waited: false,
                unsettled,
                ..
            }) => {
                if unsettled == 0 {
                    review.note(&format!(
                        "WAITING seq={seq} rule={RULE_ANSWER_RECOMMENDED} the dialog has not \
                         shown the last key's effect: read again once the screen holds still"
                    ));
                }
                // Only a screen that HELD STILL is the retry's premise (a
                // key read by Claude Code by then); a wait that ran out is
                // waited again, and after a few backed off (or, to a manager,
                // handed over) — never keyed.
                let settled = matches!(self.wait(&["idle", IDLE_MS], WAIT_STEP)?, Wait::Latched);
                if let Some(p) = self.question.pending.as_mut() {
                    if settled {
                        p.waited = true;
                    } else {
                        p.unsettled += 1;
                        if p.unsettled >= MAX_UNSETTLED_WAITS {
                            if unattended {
                                p.unsettled = 0;
                                return Ok(Some(Hold::BackOff(
                                    "the screen has not held still for the key to be sent again"
                                        .to_string(),
                                )));
                            }
                            self.question.pending = None;
                            return Ok(Some(Hold::HandOver(
                                "the dialog did not take the key, and the screen never held \
                                 still after it for the key to be sent once more"
                                    .to_string(),
                            )));
                        }
                    }
                }
                return Ok(Some(Hold::Again));
            }
            Some(QuestionPending {
                retries,
                backed_off,
                ..
            }) if retries > 0 => {
                if !unattended {
                    self.question.pending = None;
                    return Ok(Some(Hold::HandOver(
                        "the dialog did not take the key: nothing changed after it, nor after it \
                         was sent once more"
                            .to_string(),
                    )));
                }
                if !backed_off {
                    if let Some(p) = self.question.pending.as_mut() {
                        p.backed_off = true;
                    }
                    return Ok(Some(Hold::BackOff(format!(
                        "the dialog took none of the {} keys sent",
                        retries.saturating_add(1)
                    ))));
                }
                // The back-off waited, the screen still: the key again.
            }
            // Once more at once: a key the dialog refused (its typeahead
            // window, O4) is gone, and the screen held still after it. A
            // retry is counted when that key is WRITTEN (`question_pending`),
            // so a press that sends nothing does not count as one.
            Some(_) => {}
            None => {
                // The last key took effect: its box's back-off is over.
                if let Some(p) = self.question.pending.take()
                    && self
                        .press_retry
                        .as_ref()
                        .is_some_and(|r| r.key == p.identity)
                {
                    self.press_retry = None;
                }
            }
        }
        // R2 (d): on a multi-select, every check is one the loop made, and
        // every check it made still reads checked — else a person has begun
        // answering. The one toggle still in flight on the box exactly as
        // the loop keyed it is exempt: it may not have landed yet (above).
        if let QuestionForm::Multi { options, .. } = &dialog.form {
            let mine = self
                .question
                .toggled
                .get(&question_key(dialog, &turn.screen.rows));
            let made = |n: u8| mine.is_some_and(|m| m.contains(&n));
            if let Some(o) = options
                .iter()
                .find(|o| o.checked == Some(true) && !made(o.n))
            {
                return Ok(Some(Hold::HandOver(begun(&format!(
                    "option {} is checked, and the supervisor did not check it",
                    o.n
                )))));
            }
            if let Some(o) = options
                .iter()
                .find(|o| made(o.n) && o.checked != Some(true) && toggling != Some(o.n))
            {
                return Ok(Some(Hold::HandOver(begun(&format!(
                    "option {}, which the supervisor checked, reads unchecked",
                    o.n
                )))));
            }
        }
        // 3. A re-asked question is answered again (O5), after its pause.
        if let Some(q) = answering.filter(|_| !retrying) {
            let answered = approved
                .iter()
                .filter(|a| a.question.as_deref() == Some(q))
                .count();
            let waited = self
                .question
                .reasked
                .as_ref()
                .is_some_and(|(k, n)| k == q && *n == answered);
            if answered >= REASKED_AFTER && !waited {
                self.question.reasked = Some((q.to_string(), answered));
                let pause = reask_pause(answered);
                review.note(&format!(
                    "WAITING seq={seq} rule={RULE_ANSWER_RECOMMENDED} the same question came back \
                     after {answered} answers since the last review point (the model is \
                     re-asking it): answered again in {} ms",
                    pause.as_millis()
                ));
                self.wait(&["seq", &seq.to_string()], pause)?;
                return Ok(Some(Hold::Again));
            }
        }
        Ok(None)
    }

    /// An answer that landed at `at` on the box read at `seq`, ledgered and
    /// said: an approval `approved` (its `reason` the `unproven:` of a press
    /// no safe rule proved) and its `APPROVED` line to `review`; a decline
    /// `declined` (its `reason` the text typed) and a journal-only `DECLINED
    /// seq=<at> rule=<id> <subject> => <text>` — an answer, never a line a
    /// manager waits on, a badge or mail.
    fn answered(
        &mut self,
        act: &Act,
        rule_id: &str,
        shown: &str,
        reason: &str,
        (at, seq): (u64, u64),
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        match act {
            Act::Approve { .. } => {
                self.ledger_row(rule_id, approvals::Outcome::Approved, shown, reason, seq);
                review.approved(at, shown)?;
            }
            Act::Decline { text, .. } => {
                self.ledger_row(rule_id, approvals::Outcome::Declined, shown, reason, seq);
                review.note(&format!(
                    "DECLINED seq={at} rule={rule_id} {} => {text}",
                    clip(shown)
                ));
            }
        }
        Ok(())
    }

    /// The pause before the box `key` (`press.rs`'s `decline_box`) is
    /// declined again, when it came back [`REASKED_AFTER`] or more times
    /// since the last review point and that pause ([`reask_pause`]) has not
    /// been waited yet: the worker has not acted on its reason yet, and
    /// nobody else is there to answer it (module header) — never a count
    /// that hands it over.
    fn decline_again(&mut self, key: &str, approved: &[Approved]) -> Option<Duration> {
        let declines = approved
            .iter()
            .filter(|a| a.declined.as_deref() == Some(key))
            .count();
        let waited = self
            .decline
            .again
            .as_ref()
            .is_some_and(|(k, n)| k == key && *n == declines);
        if declines < REASKED_AFTER || waited {
            return None;
        }
        self.decline.again = Some((key.to_string(), declines));
        Some(reask_pause(declines))
    }

    /// The journal's `WAITING seq=<n> rule=answer-recommended@v1 <why>`, once
    /// per box however many waits it takes.
    fn note_question_wait(&mut self, review: &mut dyn Review, seq: u64, identity: &str, why: &str) {
        if self.question.waiting_said.as_deref() != Some(identity) {
            self.question.waiting_said = Some(identity.to_string());
            review.note(&format!(
                "WAITING seq={seq} rule={RULE_ANSWER_RECOMMENDED} {why}"
            ));
        }
    }

    /// Whether a box pressed at `at` LEFT ([`Self::moved_past`]), a move
    /// that leaves the same box ([`box_identity`] equal to `identity`: its
    /// countdown ticked or was wiped, the transcript above it reflowed)
    /// being no move: the wait goes on past it, for at most one
    /// [`WAIT_STEP`] from the press, and then the box as read is
    /// [`Past::Still`] — "the box did not change after the press".
    fn box_left(&mut self, at: u64, identity: &str, deadline: Instant) -> Result<Past, Fail> {
        let until = Instant::now() + WAIT_STEP;
        let mut since = at;
        // The first wait is one whole step, as every wait is; a later one
        // what is left of it.
        let mut by = deadline;
        loop {
            match self.moved_past(since, by, WAIT_STEP)? {
                Past::Moved => {}
                Past::Still(None) if Instant::now() < deadline => {
                    // The step from the press ran out, the budget did not.
                    return Ok(Past::Still(Some(self.screen()?)));
                }
                still => return Ok(still),
            }
            let now = self.screen()?;
            if box_identity(&now.rows).as_deref() != Some(identity) {
                return Ok(Past::Moved);
            }
            if Instant::now() >= until {
                return Ok(Past::Still(Some(now)));
            }
            since = now.seq;
            by = deadline.min(until);
        }
    }
}

/// The journal-only line of a press no safe rule proved, after its
/// `APPROVED`: `UNPROVEN seq=<n> rule=<id> unproven: <why>` (the journal's
/// `unproven` record, the rule as its phase). Printed nowhere: `watch`'s
/// lines are the same with and without it.
fn note_unproven(review: &mut dyn Review, seq: u64, rule_id: &str, unproven: Option<&str>) {
    if let Some(u) = unproven {
        review.note(&format!("UNPROVEN seq={seq} rule={rule_id} {u}"));
    }
}
