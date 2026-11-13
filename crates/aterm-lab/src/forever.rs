// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `forever` ([`cmd_forever`]): Autopilot Land and Cruise live on every harness aterm supervises.
//! The real Claude Code, agy and Muse Code TUIs run under a private aterm with the
//! harness on (its defaults), against the stub, each conversation scripted turn by turn
//! ([`crate::stub::scripted_reply`]: the custom mode `forever`, whose rules pick a turn's reply by
//! the scenario's word in the conversation and the act of the harness it answers).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::supervise::approvals::{self, RULE_POINT, RULE_TASK_DONE, TASK_DONE};
use aterm_agent::supervise::policy::needs::NEEDS_ACK;
use aterm_agent::supervise::policy::turn_end;
use aterm_agent::supervise::tape::show;

use crate::composer_turn::DRAFT_HANG;
use crate::door::{Door, Reply};
use crate::pyjson::J;
use crate::run::{Run, is_run};
use crate::stop;
use crate::util::{now_ms, shell_quote};
use crate::{Fail, LAB, Res};

/// The custom stub mode the lab's conversations are scripted in.
pub const MODE: &str = "forever";

const REPLY_DELAY_MS: i64 = 3000;
const _: () = assert!(REPLY_DELAY_MS >= 2000);

const REPLY_PACE_S: f64 = 0.02;

/// A plain report of progress: no done claim, no defense, no question.
pub const WORK: &str = "Implemented the parser module and its unit tests in this turn.";

/// A done claim (`says_done`), with no defense: a legacy `ATERM-TASK-DONE` line, the one such
/// claim the reader still takes since no words are read for one (2026-11-05). The token stands
/// last, as an older primer taught it, so it is the reply's last line, which the journal's
/// `EVENT idle` row keeps ([`DONE_CLAIM_READ`]).
pub const DONE_CLAIM: &str = "Merged the parser and pushed it to origin/main.\n\nATERM-TASK-DONE";

const DEFENSE_HEAD: &str = "Nothing left:";

/// A clean defense: a `Nothing left:` line, no need, no open work.
pub const CLEAN: &str = "Nothing left: the parser is committed and pushed to origin/main.";

const ASK_KEY: &str = "a person asks: what stands now";

/// The reply to it: no done claim, no defense, no question, opening with none of Muse Code's
/// tool-row heads.
pub const ASK_REPLY: &str = "Here is where it stands: the parser module is written and tested.";

/// The need L3 and L6 name.
pub const NEED: &str = "rotate the deploy key";

/// N1–N4's needs (`cruise-n`), each under the 64 cells the needs entry's writer keeps of a need,
/// so every surface can read it whole.
pub const NEED_A: &str = "grant Screen Recording to com.aterm.aterm in System Settings";
/// …the second of N1's.
pub const NEED_B: &str = "the release credential to sign 0.48.0";
/// …N4's other.
pub const NEED_C: &str = "a second reviewer for the parser's error paths";
const N1_HEAD: &str = "Implemented the parser module and its unit tests in this turn.";
const N3_HEAD: &str = "Reviewed the parser module again in this turn.";
const N4_HEAD: &str = "Hardened the parser's error paths in this turn.";

const LONG_DELAY_MS: i64 = 10_000;
const LONG_PACE_MS: i64 = 7_000;
/// How long a reply of real work holds its agent before it streams: past the turn-end policy's
/// floor ([`turn_end::MIN_WORK`], 2 min), so its turn is work and the short streak starts over.
#[expect(
    clippy::cast_possible_truncation,
    reason = "two minutes and ten seconds in ms"
)]
const WORK_TURN_MS: i64 = turn_end::MIN_WORK.as_millis() as i64 + 10_000;
#[expect(
    clippy::cast_precision_loss,
    reason = "a few seconds in ms, exact in an f64"
)]
const LONG_PACE_S: f64 = LONG_PACE_MS as f64 / 1000.0;

/// How long a long reply of `text` keeps its agent busy at the least: its hold, and its pace
/// before each of its deltas ([`crate::stub::split_deltas`]).
#[must_use]
pub fn long_reply_ms(text: &str) -> i64 {
    let words = i64::try_from(crate::stub::split_deltas(text).len()).unwrap_or(0);
    LONG_DELAY_MS + words * LONG_PACE_MS
}

const LAND_KEY: &str = "Autopilot is Land:";

/// What only Land's one ask at the person's switch over a held tab says (v6 §14.17 item 3): the
/// close of its why, which `switch-over-held`'s stand-in answers slowly.
const SWITCH_KEY: &str = turn_end::SWITCH_WHY_TAIL;

/// How long `switch-over-held`'s stand-in takes over its answer to Land's one ask: long enough
/// that a look reads the ask standing, short of real work (`turn_end::MIN_WORK`).
pub const SWITCH_ASK_DELAY_MS: i64 = 30_000;
const NUDGE_KEY: &str = "Still landing:";
/// The land check's words the stub's rules answer (`land-check@v1`): its fixed head, ahead of a
/// chain's `MARK` and `NUDGE_KEY` rules, so no other rule answers the check.
const LAND_CHECK_KEY: &str = turn_end::LAND_CHECK_HEAD;
/// The one answer to the land check that lands.
const LANDED: &str = aterm_agent::supervise::policy::land_check::LANDED;
/// LAND'S CONTRADICTION'S HEAD (slice 4c): the stub's rule for land-contradicted's answer to it,
/// ahead of `NUDGE_KEY`'s, whose words it opens with.
const CONTRADICTED_KEY: &str = turn_end::LAND_CONTRADICTED_HEAD;
/// land-contradicted's answer to the contradiction, given once the lab has landed
/// `wip-notes.txt` for it ([`land_contradicted_work`]): what it did, then a defense of other words
/// than [`CLEAN`]'s, so it is no same reply.
pub const LANDED_NOTES: &str = "Committed wip-notes.txt and pushed it to origin/main.\n\
                                Nothing left: the parser and its notes are on origin/main.";
/// The head of Land's nudge quoting the agent's own list
/// ([`land_check::LISTED_OPEN`](aterm_agent::supervise::policy::land_check::LISTED_OPEN)).
const LISTED_KEY: &str = aterm_agent::supervise::policy::land_check::LISTED_OPEN;
/// The hinted check's quote opening
/// ([`land_check::LAND_CHECK_HINT_OPEN`](aterm_agent::supervise::policy::land_check::LAND_CHECK_HINT_OPEN)).
const HINT_OPEN: &str = aterm_agent::supervise::policy::land_check::LAND_CHECK_HINT_OPEN;
/// L2's list, its answer to the check: its report's leftover, said alone.
pub const CHECK_LIST: &str = "The docs PR is still open.";
/// land-same-list's list.
pub const SAME_LIST: &str = "The changelog entry is not written yet.";
/// land-reworded's second list: [`CHECK_LIST`]'s leftover in other words, its word family the
/// same (`still open`), so only the hold's quote tells the two apart (slice 4b).
pub const REWORDED_LIST: &str = "The pull request for the docs is still open.";
/// What the nudge quoting [`REWORDED_LIST`] names.
const REWORDED_KEY: &str = "The pull request for the docs is still open";
/// land-reworded's work reply to the nudge quoting its first list (no defense).
pub const REWORD_WORK: &str = "Rebased the docs branch onto main in this turn.";
const REWORD_WORK_KEY: &str = "Rebased the docs branch";
/// What the nudge quoting [`SAME_LIST`] names.
const SAME_KEY: &str = "The changelog entry is not written yet";
/// land-listed's list, and the work its nudge's reply does.
pub const RELEASE_LIST: &str = "The release notes are not written yet.";
/// What the nudge quoting [`RELEASE_LIST`] names.
const RELEASE_KEY: &str = "The release notes are not written yet";
/// land-listed's work reply to the nudge quoting its list (no defense).
pub const RELEASE_WORK: &str = "Implemented the release notes for the parser in this turn.";
const RELEASE_WORK_KEY: &str = "Implemented the release notes";
/// land-hinted's defense: clean, a narrative clause in it that the reader reads open.
pub const HINTED: &str = "Nothing left: the parser is on origin/main, and the old bug where a stale lock still blocked the build is fixed.";
/// What land-hinted's check quotes of [`HINTED`].
const HINT_KEY: &str = "stale lock still blocked the build";
/// L6's answer to the check: its need alone.
fn need_alone() -> String {
    format!("NEEDS YOU: {NEED}")
}
/// The words of the done check the stub's rules answer (`continue-done-check@v1`).
pub const CHECK_KEY: &str = "done is never the end";
/// The words of Cruise's continuation the stub's rules answer (`continue@v1`).
pub const CONTINUE_KEY: &str = "Autopilot is Cruise: keep going";

const OPEN_SUFFIX: &str = "Your report still names open work";
const REPO_SUFFIX: &str = "Your repository still holds work";

const CARRYING_ON: &str = "— carrying on";

const QUIET_AFTER_LANDED_MIN_MS: i64 = 90_000;
const PAST_NEXT_STEP_MS: i64 = 60_000;
const QUIET_UNDER_OFF_MS: i64 = 270_000;
const CHAIN_LIMIT_MS: i64 = 16 * 60 * 1000;
const CRUISE_N_LIMIT_MS: i64 = 20 * 60 * 1000;
/// How long land-contradicted-list may take to watch past its hold: its contradiction waits out
/// the short streak's fourth back-off (16 min, a defense being a done claim that lengthens the
/// streak) after the 8 min one before the nudge quoting its list, about 28 min on every harness.
const LIST_CONTRADICTED_LIMIT_MS: i64 = 35 * 60 * 1000;
/// How long land-reworded may take to watch past its hold: the nudge quoting its new words waits
/// out a second back-off after the second check (4 min, its hold at +875.1 s under Claude Code
/// 2.1.318, 8 min on a longer short streak) after the 8 min one before the nudge quoting its list.
const LIST_REWORDED_LIMIT_MS: i64 = 24 * 60 * 1000;
const WALL_REDRAW_LIMIT_MS: i64 = 30 * 60 * 1000;
const WALL_SWITCH_LIMIT_MS: i64 = 30 * 60 * 1000;
const WALL_UNREAD_LIMIT_MS: i64 = 100 * 60 * 1000;
const WALL_CRUISE_LIMIT_MS: i64 = 25 * 60 * 1000;
const WALL_RELAUNCH_LIMIT_MS: i64 = 30 * 60 * 1000;
const WALL_TAP_LIMIT_MS: i64 = 30 * 60 * 1000;
const AT_DONE_MS: i64 = 500;

const DUE_SLACK_MS: i64 = 2_000;

const PERSISTENCE_GRACE_MS: i64 = 60_000;

const UNJUDGED_MS: i64 = 300_000;

const WORK_KEY: &str = "Implemented the parser";

const OPEN_KEY: &str = "The docs PR is still open";

/// L12's question in prose (`classify_said`: a question, no choice, no destructive word; no done
/// claim).
pub const QUESTION: &str = "Should the parser also accept tab-indented input?";
const QUESTION_KEY: &str = "tab-indented input";
const ANSWER_KEY: &str = "Nobody is here to choose:";
const ANSWER_HEAD: &str = "Nobody is here to ";

/// L20 and C5's question dialog, asked by the stub's `AskUserQuestion` in the first turn
/// ([`dialog_call`]): its Recommended option is new work, the other lands the work.
pub const DIALOG_QUESTION: &str = "Which should come next for the parser?";
/// The dialog's Recommended option: new work, which Land starts none of.
pub const DIALOG_NEW_WORK: &str = "Start the docs site (Recommended)";
/// The dialog's landing option.
pub const DIALOG_LANDS: &str = "Push the parser to origin/main";
const PERSON_ASKS: &str = "tell me what this repository holds so far";
/// Its reply: a plain answer.
pub const PERSON_REPLY: &str = "The repository holds a README and nothing else yet.";
const ORCHESTRATED: &str = "now write the parser for this repository and land it";
const ORCHESTRATED_KEY: &str = "now write the parser";
const PERSON_STAND_MS: u64 = 8_000;
const EXCHANGE_WATCH_MS: i64 = QUIET_UNDER_OFF_MS + 30_000;

/// P2's replies, each opening `Wrote ` (Muse Code's tool-row head) with a plain word after it:
/// L14's work report, its need and defense, its clean defense; C3's done claim and its defense.
pub const W_WORK: &str = "Wrote the parser module and its unit tests in this turn.";
pub const W_NEEDS: &str = "Wrote the docs for the parser too.\nNEEDS YOU: rotate the deploy key\nNothing left: the parser is committed and pushed to origin/main.";
pub const W_DEFENSE: &str = "Wrote the last commit to origin/main.\nNothing left: the parser is committed and pushed to origin/main.";
pub const W_DONE: &str = "Wrote the parser and pushed it to origin/main.\n\nATERM-TASK-DONE";
pub const W_CHECKED: &str = "Wrote the last checks for the parser.\nNothing left: the parser is committed and pushed to origin/main.";
const W_WORK_KEY: &str = "Wrote the parser module";
const W_DONE_KEY: &str = "Wrote the parser and pushed";

/// C4: the owner's own continuation in two paragraphs (`[harness] continue_text`, set on the lab
/// of a run holding `cruise-o`: [`owner_keys`]): a blank line, then a `Nothing left:` line and a
/// `NEEDS YOU:` line of its own, every line a head agy's reader would take where a blank line in
/// a message cut it (agy draws the rows under it two columns in, as a reply's: `aterm_phase::agy`'s
/// header).
pub const OWNER_CONTINUE: &str = "Keep going on the owner's terms: finish the parser, verify it and land it.\n\nNothing left: is a line you write only once everything is on remote main.\nNEEDS YOU: lines are for a person's own acts only.";
const OWNER_KEY: &str = "on the owner's terms";
const OWNER_TAIL: &str = "lines are for a person's own acts only";
/// The words [`DONE_CLAIM`] opens with, by which a look at the screen knows it stands.
pub const DONE_CLAIM_KEY: &str = "Merged the parser";

/// What the journal's `EVENT idle` row keeps of [`DONE_CLAIM`] and [`W_DONE`]: their last line,
/// the token, which no other reply of a chain that says either one ends with.
pub const DONE_CLAIM_READ: &str = turn_end::FINISHED;

/// The `[harness]` line that sets [`OWNER_CONTINUE`] as the lab's `continue_text`: a TOML basic
/// string, its line breaks escaped.
#[must_use]
pub fn owner_keys() -> String {
    let mut q = String::from("continue_text = \"");
    for c in OWNER_CONTINUE.chars() {
        match c {
            '\\' => q.push_str("\\\\"),
            '"' => q.push_str("\\\""),
            '\n' => q.push_str("\\n"),
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

/// The startup deadline of a worker's record (aterm-gui's `harness_host::STARTUP_DEADLINE`): a
/// record `working` as its starter left it whose agent shows no composer by then is marked
/// `stalled why=startup`.
pub const STARTUP_DEADLINE_MS: i64 = 120_000;
const PAST_DEADLINE_MS: i64 = 30_000;

/// The cap `--mode cap` sets (`[harness] continue_per_hour`).
pub const CAP_PER_HOUR: u32 = 1;
/// The words the capped point's escalation carries (`act_at_point`).
#[must_use]
pub fn cap_reason() -> String {
    format!("continue_per_hour = {CAP_PER_HOUR} reached")
}
/// The cap's window (`TurnEndTiming::window`), in ms.
#[must_use]
pub fn cap_window_ms() -> i64 {
    i64::try_from(turn_end::TurnEndTiming::default().window.as_millis()).unwrap_or(i64::MAX)
}
const PAST_WINDOW_MS: i64 = 30_000;
const RESUMED: &str = "carry on with the parser and land it";
const RESUMED_KEY: &str = "carry on with the parser";
const CAP_LIMIT_MS: i64 = 95 * 60 * 1000;

const WALL_CLAIM: &str = "five_hour";
const WALL_AHEAD_MS: i64 = 240_000;
const WALL_LIFT_EARLY_MS: i64 = 3_000;
const WALL_LOST_MS: i64 = 5 * 60_000;

/// The grace past a reset the harness waits on `program` ([`Harness::name`]) before it goes on
/// (`TurnEndTiming::grace_for`): 60 s for a program with no auto-continue (Muse Code, agy),
/// 150 s for Claude Code, whose own continue comes 30 to 90 s past the reset.
#[must_use]
pub fn wall_grace_ms(program: &str) -> i64 {
    let p = Harness::of(program).map_or(aterm_phase::Program::Generic, Harness::program);
    i64::try_from(turn_end::TurnEndTiming::default().grace_for(p).as_millis()).unwrap_or(i64::MAX)
}

const WALL_HID_AHEAD_MS: i64 = 420_000;
/// W4's redraw: the resume typed past the first reset meets the wall raised again this far on.
pub const REDRAW_AHEAD_MS: i64 = 5 * 60 * 1000;
const WALL_SWITCH_AHEAD_MS: i64 = 15 * 60 * 1000;
const WALL_RELAUNCH_AHEAD_MS: i64 = 10 * 60 * 1000;
/// How long after the told W6's lab reads the conversation again ([`unread_restore_due`]).
pub const UNREAD_RESTORE_AFTER_TOLD_MS: i64 = 60_000;
/// How long past the told's due (the reset and [`futile_after_ms`]) W6's lab reads the
/// conversation again with no told seen: the told is then late.
pub const UNREAD_TOLD_LATE_MS: i64 = 10 * 60 * 1000;
/// The words of the loop's hold on a limit whose conversation no read names (`turn_end.rs`'s
/// `limit-conversation-unread`).
pub const UNREAD_WORDS: &str = "the remembered limit's conversation is unread";

/// The loop's bound on a limit held unread past its reset before the owner is told
/// (`exception.rs`'s `FUTILE_AFTER`), in ms.
#[must_use]
pub fn futile_after_ms() -> i64 {
    i64::try_from(aterm_agent::supervise::policy::exception::FUTILE_AFTER.as_millis())
        .unwrap_or(i64::MAX)
}

/// The words of the owner's told for a limit held unread (`Exception::unread_limit`'s why), which
/// its `EXCEPTION` line carries.
#[must_use]
pub fn unread_told_words() -> String {
    aterm_agent::supervise::policy::exception::Exception::unread_limit().why
}
/// The latest the notice may leave the screen: this long before the reset, so the loop reads and
/// decides the screen without it while the wait stands.
pub const HIDE_BEFORE_RESET_MS: i64 = 120_000;
const HIDE_TRIES: usize = 32;
/// Muse Code's local command whose output scrolls the notice away: it prints `  Rules loaded this
/// session (in precedence order):`, `  user: none`, `  project: none` under the turn's end and
/// sends no request (measured on 1.4.10).
pub const HIDE_COMMAND: &str = "/rules";
/// The words of Muse Code's usage wall (`◆ Usage limit reached · /upgrade (…) …`): a screen
/// holding none of them has lost the notice.
pub const WALL_WORDS: &str = "Usage limit reached";
/// agy's local command whose output scrolls the notice away: with no `title` block in its settings
/// (the lab's), `/title` prints `  ⎿  Error: No title command configured in settings.json.`
pub const AGY_HIDE_COMMAND: &str = "/title";
/// The words of agy's quota wall: the Gemini API's message in its `⚠ agent executor error: …`
/// block, and the block's last row (`Error ID: <id>`), the last to scroll off the screen.
pub const AGY_WALL_WORDS: [&str; 2] = ["exceeded your current quota", "Error ID: "];
/// Claude Code's local command whose output scrolls the notice away once its countdown is
/// cancelled ([`CLAUDE_CANCEL`]): `/rename <name>` prints `  ⎿ Session renamed to: <name>` under
/// its own `❯ /rename <name>` row and sends no request; the composer's top rule carries the name
/// from then on (`──── <name> ─`), and with an argument typed no command menu is drawn (measured
/// on 2.1.307).
pub const CLAUDE_HIDE_COMMAND: &str = "/rename forever-w2";
/// The words of Claude Code's usage limit: the turn's `⎿  You've hit your session limit · resets
/// …` (`5-hour limit` from 2.1.312, which renamed the five-hour window), its `⏺ Usage limit
/// reached · continuing automatically at …` row, and the footer's `⚠ Usage limit reached · limit
/// resets …` banner.
pub const CLAUDE_WALL_WORDS: [&str; 5] = [
    "session limit",
    "5-hour limit",
    "Usage limit",
    "usage limit",
    "limit resets",
];
/// Claude Code's countdown, cancelled before the notice can be scrolled away ([`Hide::cancel`]):
/// while it is armed its footer draws `⚠ Usage limit reached · limit resets …` and `Continuing
/// automatically at … · esc to cancel` under every local command's rows; Esc cancels it (`⏺
/// Automatic continue cancelled · /rate-limit-options to re-arm`) and the banner leaves the footer
/// (measured on 2.1.307).
pub const CLAUDE_CANCEL: Cancel = Cancel {
    key: "escape",
    armed: "esc to cancel",
    cancelled: "Automatic continue cancelled",
};

/// A path, one word with no space, that `same-words`' first prompt carries (`--mode resid`).
pub const LONG_TOKEN: &str = "lab/parser-module-parser-module-parser.rs";
/// The size `same-words`' tab is resized to once its first point is decided (`resize <rows>
/// <cols>`): narrower, so the opener re-wraps, and taller, so more of the conversation above the
/// reply comes into sight.
pub const RESIZED: (u16, u16) = (104, 62);
const _: () = assert!(RESIZED.0 < 144);
const RESIZE_AFTER_MS: i64 = 5_000;
const TURN_AFTER_RESIZE_MS: i64 = 30_000;
const SAME_TURN: &str = "now carry on with the parser module and report";
const SAME_TURN_KEY: &str = "now carry on with the parser module";
/// `tall-scroll`'s report rows ([`scroll_reply`]): with its head, the work report and each
/// agent's own rows, a few rows taller than the tab's 50, so a few notches bring its opener back.
pub const SCROLL_LINES: usize = 46;
const SCROLL_AFTER_MS: i64 = 5_000;
const SCROLL_NOTCHES: u32 = 40;
const SCROLL_STILL: u32 = 3;
const SCROLL_HELD_MS: i64 = 10_000;
/// How long the chain is watched after the last notch: the loop has read the scrolled screen (its
/// recheck is 5 s at the most) and decided there, long before the first point's due.
pub const SCROLL_WATCH_MS: i64 = 40_000;

/// F3's reply to its tool's result: no words, so the turn ends on the tool's row ([`read_call`]);
/// agy's is a space ([`tool_after`]).
pub const TOOL_AFTER: &str = "";

/// F3's reply to its tool's result on harness `h`: [`TOOL_AFTER`], but a single space on agy. agy
/// refuses a reply holding neither text nor a tool call (it sends "Error: model output error:
/// model output must contain either output text or tool calls, these cannot both be empty, please
/// try again" as a turn of its own and asks again), but takes one whose text is a space, draws
/// nothing for it and ends its turn on the tool's row (`● Explored 1 file (README.md)`, then `?
/// for shortcuts`), as agy 1.5.1's cast read on 2026-11-05.
#[must_use]
pub fn tool_after(h: Harness) -> &'static str {
    match h {
        Harness::Agy => " ",
        Harness::Claude | Harness::Muse => TOOL_AFTER,
    }
}

/// The factor `--mode ladder` scales the short back-off ladder by (`turn_end::LADDER_SCALE_SEAM`):
/// an eighth, so the steps of 2 to 60 min are 15 s to 7.5 min, and the ladder to its cap and the
/// cap decided again fit in about 16 min.
pub const LADDER_SCALE: f64 = 0.125;
/// How many back-offs `ladder` must see decided: the doubling steps to the cap (2, 4, 8, 16, 32,
/// 60 min), and the cap once more.
pub const LADDER_STEPS: u32 = 7;
const LADDER_LIMIT_MS: i64 = 25 * 60 * 1000;
const STEP_SLACK_MS: i64 = 2_500;

/// The instance environment `--mode ladder` runs its aterm under: the ladder's seam at
/// [`LADDER_SCALE`], which only a development build reads.
#[must_use]
pub fn ladder_env() -> String {
    format!("{}={LADDER_SCALE}", turn_end::LADDER_SCALE_SEAM)
}

/// The shipped short back-off ladder in minutes, one step per short turn in a row: 2 min doubled
/// to the 60 min cap, then the cap again. A test in `forever_tests.rs` holds it to the product's
/// own timing.
pub const SHIPPED_LADDER_MIN: [i64; LADDER_STEPS as usize] = [2, 4, 8, 16, 32, 60, 60];

/// The step the ladder names for a point that closed `streak` short turns, in ms, scaled by
/// `scale` ([`SHIPPED_LADDER_MIN`], the cap past its end).
#[must_use]
pub fn ladder_step_ms(streak: u32, scale: f64) -> i64 {
    minutes_scaled(&SHIPPED_LADDER_MIN, streak, scale)
}

/// Step `n` (from 1) of a ladder of `minutes`, in ms, scaled by `scale`: its last step past its
/// end.
fn minutes_scaled(minutes: &[i64], n: u32, scale: f64) -> i64 {
    let i = usize::try_from(n.max(1) - 1)
        .unwrap_or(usize::MAX)
        .min(minutes.len() - 1);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "a step of minutes scaled by a factor at most 1: far inside both"
    )]
    let ms = (minutes[i] as f64 * 60_000.0 * scale).round() as i64;
    ms
}

/// F3's tool call on harness `h`: a read of the chain's own `README.md` (`file`, its physical
/// path), which Claude Code (`Read`), agy (`read_file`, its `Path`) and Muse Code (`read_file`,
/// with a path relative to its workspace root: its file policy allows an absolute one only in
/// some places) run with no confirm in the lab's jail.
#[must_use]
pub fn read_call(h: Harness, file: &Path) -> J {
    let abs = file.to_string_lossy().into_owned();
    let (name, input) = match h {
        Harness::Claude => ("Read", J::obj([("file_path", J::s(abs))])),
        Harness::Agy => ("read_file", J::obj([("Path", J::s(abs))])),
        Harness::Muse => ("read_file", J::obj([("path", J::s("README.md"))])),
    };
    J::obj([("name", J::s(name)), ("input", input)])
}

/// L20 and C5's first-turn tool call: Claude Code's `AskUserQuestion` with [`DIALOG_QUESTION`],
/// its Recommended option new work ([`DIALOG_NEW_WORK`]) and the other the landing
/// ([`DIALOG_LANDS`]). The stub calls it once: a conversation holding a tool's result gets the
/// rule's text ([`crate::stub::scripted_call`]).
#[must_use]
pub fn dialog_call() -> J {
    let option = |label: &str, description: &str| {
        J::obj([("label", J::s(label)), ("description", J::s(description))])
    };
    let question = J::obj([
        ("question", J::s(DIALOG_QUESTION)),
        ("header", J::s("Next")),
        ("multiSelect", J::Bool(false)),
        (
            "options",
            J::Arr(vec![
                option(DIALOG_NEW_WORK, "Begin a new docs site for the parser"),
                option(DIALOG_LANDS, "Land the parser as it stands"),
            ]),
        ),
    ]);
    J::obj([
        ("name", J::s("AskUserQuestion")),
        ("input", J::obj([("questions", J::Arr(vec![question]))])),
    ])
}

/// Why L20 and C5 are out of this lab's reach on harness `h`: `None` on Claude Code, whose
/// `AskUserQuestion` the stub scripts.
#[must_use]
pub fn dialog_out_of_reach(h: Harness) -> Option<&'static str> {
    (h != Harness::Claude).then_some(
        "the stub scripts no question tool of agy's or Muse Code's: L20 and C5 call Claude Code's AskUserQuestion",
    )
}

/// Why `mode`'s wall scenario is out of this lab's reach on harness `h`: `None` where the stub
/// draws the usage wall or the outage the scenario needs and, for W2 and W4, the lab can take its
/// notice off the screen ([`hide_of`]).
#[must_use]
pub fn wall_out_of_reach(h: Harness, mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::Wall => None,
        Mode::WallHid => hide_of(h).err(),
        Mode::WallRedraw => hide_of(h).err(),
        Mode::WallSwitch => None,
        Mode::WallUnread => match h {
            Harness::Claude => None,
            Harness::Agy | Harness::Muse => Some(
                "only Claude Code's conversation is read (its transcript): another agent's limit is the screen's, never held unread",
            ),
        },
        Mode::WallCruise => None,
        Mode::WallRelaunch => match h {
            Harness::Agy | Harness::Muse => None,
            Harness::Claude => Some(
                "Claude Code's limit is its conversation's (its transcript), read again after any relaunch: the relaunch rule is the screen-alone agents' (Muse Code, agy)",
            ),
        },
        Mode::WallTap | Mode::WallTapNarrow => match h {
            Harness::Claude => None,
            Harness::Agy | Harness::Muse => Some(
                "the usage tap is Claude Code's status line alone: no other agent's window is read off a tap",
            ),
        },
        Mode::Sysmsg => None,
        Mode::OutageLand | Mode::OutageCruise => match h {
            Harness::Claude => None,
            Harness::Agy | Harness::Muse => Some(
                "the stub's outage is Claude Code's overload (the Messages API's 529 overloaded_error with x-should-retry: false): agy's and Muse Code's own outage forms are not staged",
            ),
        },
        Mode::Land
        | Mode::Cruise
        | Mode::Both
        | Mode::Cover
        | Mode::Cap
        | Mode::Resid
        | Mode::Ladder
        | Mode::Angles
        | Mode::Switch
        | Mode::Dialog => None,
    }
}

/// How W2 takes the wall's notice off harness `h`'s screen while the wait stands ([`hide_step`]):
/// a local command entered again and again (it sends no request).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hide {
    /// The local command that takes the notice away, typed into the empty composer and entered
    /// once the menu offers it first ([`menu_offers`]), again and again until the notice has
    /// scrolled away.
    pub command: &'static str,
    /// The words of the harness's usage wall: a screen holding none of them has lost the notice
    /// ([`notice_gone`]).
    pub words: &'static [&'static str],
    /// A key pressed once first, where the program draws the notice for as long as its own
    /// countdown is armed (Claude Code's [`CLAUDE_CANCEL`]).
    pub cancel: Option<Cancel>,
}

/// The key that cancels a program's own countdown to its reset, and the words that show it armed
/// and cancelled ([`Hide::cancel`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancel {
    /// The key (`aterm ctl key`).
    pub key: &'static str,
    /// Words on the screen while the countdown is armed: the key is pressed only where they stand.
    pub armed: &'static str,
    /// Words the program draws once it is cancelled: the hide goes on only where they stand.
    pub cancelled: &'static str,
}

/// [`Hide`] of harness `h`, or why none takes its notice away.
pub const fn hide_of(h: Harness) -> Result<Hide, &'static str> {
    match h {
        Harness::Muse => Ok(Hide {
            command: HIDE_COMMAND,
            words: &[WALL_WORDS],
            cancel: None,
        }),
        Harness::Agy => Ok(Hide {
            command: AGY_HIDE_COMMAND,
            words: &AGY_WALL_WORDS,
            cancel: None,
        }),
        Harness::Claude => Ok(Hide {
            command: CLAUDE_HIDE_COMMAND,
            words: &CLAUDE_WALL_WORDS,
            cancel: Some(CLAUDE_CANCEL),
        }),
    }
}

/// The harnesses aterm supervises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Agy,
    Muse,
}

impl Harness {
    /// Every harness, in the order `all` runs them.
    pub const ALL: [Harness; 3] = [Harness::Claude, Harness::Agy, Harness::Muse];

    /// The verb's word, the lab's `agent`, the reader's program word (the needs entry's
    /// `<program>`: `Program::name`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Agy => "agy",
            Harness::Muse => "muse",
        }
    }

    fn of(word: &str) -> Option<Harness> {
        Harness::ALL.into_iter().find(|h| h.name() == word)
    }

    /// The reader's program it runs.
    #[must_use]
    pub const fn program(self) -> aterm_phase::Program {
        match self {
            Harness::Claude => aterm_phase::Program::Claude,
            Harness::Agy => aterm_phase::Program::Agy,
            Harness::Muse => aterm_phase::Program::Muse,
        }
    }

    pub(crate) fn bin_ver(self, run: &Run) -> (&str, &str) {
        match self {
            Harness::Claude => (&run.claude_bin, &run.claude_ver),
            Harness::Agy => (&run.agy_bin, &run.agy_ver),
            Harness::Muse => (&run.muse_bin, &run.muse_ver),
        }
    }
}

/// Which chains run: the measured pass's by Autopilot (`land`, `cruise`, `both`), the coverage
/// chains (`cover`), the hour's cap (`cap`), the residuals (`resid`), the scaled ladder
/// (`ladder`), one of the stub's usage walls (`wall`, `wall-…`), or its outage (`outage-…`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Land,
    Cruise,
    Both,
    Cover,
    Cap,
    Wall,
    /// W2: the wall whose notice leaves the screen (`wall-hid`).
    WallHid,
    /// The residuals live (`resid`): F18, F1 and F2, the same words, F3.
    Resid,
    /// The back-off ladder past its fourth step, scaled (`ladder`).
    Ladder,
    /// W4, the redraw after a typed resume (`wall-redraw`): the resume typed past the reset meets
    /// the wall again, under a new reset.
    WallRedraw,
    /// W5, the cross-program end (`wall-switch`): another agent started in the tab under the first
    /// one's remembered limit.
    WallSwitch,
    /// W6, the unread limit told (`wall-unread`): Claude Code's limit held unread past its reset by
    /// `FUTILE_AFTER`.
    WallUnread,
    /// W8, a usage wall under Cruise (`wall-cruise`).
    WallCruise,
    /// W9, the agent ended and launched again by a person under its standing wall
    /// (`wall-relaunch`).
    WallRelaunch,
    /// W10, a five-hour wall the usage tap times (`wall-tap`, [`tap`]).
    WallTap,
    /// W11, the same wall in a narrow pane (`wall-tap-narrow`, [`tap`]).
    WallTapNarrow,
    /// ANGLES, a new kind of work at each done claim (`angles`, v6 §14.5's 5a), under the short
    /// back-off ladder at an eighth ([`ladder_env`]), as `ladder` runs.
    Angles,
    /// SWITCH, a running Cruise tab's word switched mid-run (`switch`, v6 §14.10's 10a,
    /// [`switch`]).
    Switch,
    /// O1, an API overload standing from the first prompt under Land and lifted (`outage-land`,
    /// v6 §14.17.5.2's 12b, [`outage`]), under the ladders at three eighths ([`outage::env`]).
    OutageLand,
    /// O2, an API overload met by Cruise's own act and lifted (`outage-cruise`, [`outage`]), under
    /// the ladders at three eighths.
    OutageCruise,
    /// A SYSTEM MESSAGE IS A WAIT (v6 §14.17.5's 12a, `sysmsg`): one chain per system-message
    /// form the stub plays, on every harness, each on a lab of its own ([`sysmsg::Form`]).
    Sysmsg,
    /// L20 and C5, a question dialog in the first turn under Land and under Cruise (`dialog`).
    Dialog,
}

impl Mode {
    fn of(word: &str) -> Option<Mode> {
        Some(match word {
            "land" => Mode::Land,
            "cruise" => Mode::Cruise,
            "both" => Mode::Both,
            "cover" => Mode::Cover,
            "cap" => Mode::Cap,
            "wall" => Mode::Wall,
            "wall-hid" => Mode::WallHid,
            "resid" => Mode::Resid,
            "ladder" => Mode::Ladder,
            "wall-redraw" => Mode::WallRedraw,
            "wall-switch" => Mode::WallSwitch,
            "wall-unread" => Mode::WallUnread,
            "wall-cruise" => Mode::WallCruise,
            "wall-relaunch" => Mode::WallRelaunch,
            "wall-tap" => Mode::WallTap,
            "wall-tap-narrow" => Mode::WallTapNarrow,
            "angles" => Mode::Angles,
            "switch" => Mode::Switch,
            "outage-land" => Mode::OutageLand,
            "outage-cruise" => Mode::OutageCruise,
            "sysmsg" => Mode::Sysmsg,
            "dialog" => Mode::Dialog,
            _ => return None,
        })
    }

    /// A mode of the stub's usage wall or its outage, on a lab of its own on each harness it
    /// reaches ([`wall_out_of_reach`]), and its one scenario.
    #[must_use]
    pub const fn walled(self) -> Option<Scenario> {
        match self {
            Mode::Wall => Some(Scenario::W1),
            Mode::WallHid => Some(Scenario::W2),
            Mode::WallRedraw => Some(Scenario::W4),
            Mode::WallSwitch => Some(Scenario::W5),
            Mode::WallUnread => Some(Scenario::W6),
            Mode::WallCruise => Some(Scenario::W8),
            Mode::WallRelaunch => Some(Scenario::W9),
            Mode::WallTap => Some(Scenario::W10),
            Mode::WallTapNarrow => Some(Scenario::W11),
            Mode::OutageLand => Some(Scenario::O1),
            Mode::OutageCruise => Some(Scenario::O2),
            Mode::Sysmsg
            | Mode::Land
            | Mode::Cruise
            | Mode::Both
            | Mode::Cover
            | Mode::Cap
            | Mode::Resid
            | Mode::Ladder
            | Mode::Angles
            | Mode::Switch
            | Mode::Dialog => None,
        }
    }
}

/// What a chain's repository holds once laid ([`lay_repo`]) beside its `main`, committed and
/// pushed to `origin`: the five legs of the repository probe
/// (`aterm_agent::harness::repo_work::probe`), one at a time.
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
    /// Another session's work beside a clean `main` ([`angles::lay_others`]): a worktree on a
    /// branch of its own, a branch holding a commit no remote has, and a stash entry. Angle 6
    /// leaves every one as it is ([`angles::others_left`]).
    Others,
    /// `main` committed and its `origin` removed: a repository with no remote (slice 9c's
    /// land-n, whose receipt is `no-remote`).
    NoRemote,
    /// No repository at all: the folder holds its README alone (slice 9c's land-o, whose receipt
    /// is `no-repo`).
    Plain,
}

impl Repo {
    /// Whether the repository check reads it clean (`RepoWork::is_clean`): nothing left to land
    /// where it can read, a committed repository with no remote (land-n) and a folder with no
    /// repository (land-o) among them, so a defense there owes the land check (slice 13a).
    #[must_use]
    pub const fn reads_clean(self) -> bool {
        matches!(self, Repo::Clean | Repo::NoRemote | Repo::Plain)
    }

    /// What the repository suffix must name of it (`RepoWork::summary`'s words), and the words of
    /// the other legs it must not.
    #[must_use]
    pub const fn named(self) -> (&'static [&'static str], &'static [&'static str]) {
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
            Repo::Others => (
                &["1 stash"],
                &["changed", "untracked", "unpushed", "not on"],
            ),
            Repo::NoRemote | Repo::Plain => (&[], &[]),
        }
    }

    const fn says(self) -> &'static str {
        match self {
            Repo::Clean => "clean and pushed",
            Repo::Untracked => "wip-notes.txt untracked",
            Repo::Changed => "README.md changed, not committed",
            Repo::Stashed => "a stash entry on main",
            Repo::Unpushed => "a commit no remote holds",
            Repo::Unlanded => "a commit pushed to origin/wip, not on origin/main",
            Repo::Others => "another session's worktree, branch and stash",
            Repo::NoRemote => "committed, no remote",
            Repo::Plain => "a plain folder, no repository",
        }
    }
}

/// One tab's scripted conversation: the scenarios it carries, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// L1, L3, L5.
    LandA,
    /// L18 (slice 9c: a landing in a repository with no remote, its receipt `no-remote`).
    LandN,
    /// L19 (slice 9c: a landing in a plain folder, its receipt `no-repo`).
    LandO,
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
    /// P4L: Land, every reply sent at once.
    InstantLand,
    /// P4C: Cruise, every reply sent at once.
    InstantCruise,
    /// L12 (a question in prose: `land-answer@v1`).
    LandQ,
    /// L13 (a person's own exchange).
    LandP,
    /// L14 (P2 under Land: replies opening `Wrote `).
    LandW,
    /// C3 (P2 under Cruise).
    WroteC,
    /// P5 (a worker's record past its startup deadline).
    LandT,
    /// The manager tab, running the harness under test, whose spawn starts L15's worker.
    Mgr,
    /// L16 (an end past the work floor, seen by a look at its tab).
    LandJ,
    /// L17 (an end past the work floor, seen by a press on its menu-bar row).
    LandK,
    /// N1–N4 (Cruise, its needs moving: two, none, one, two).
    CruiseN,
    /// L15 (a worker given by `spawn agent=<harness>`).
    Worker,
    /// C4 (Cruise, the owner's continuation in two paragraphs).
    CruiseO,
    /// CAP (`continue_per_hour`).
    Cap,
    /// W1 (a usage wall under Land).
    Wall,
    /// W2 (the usage wall whose notice leaves the screen: the remembered wall).
    WallHid,
    /// F18 (a person's keystroke into an instant turn: P4C's chain, its person's prompt typed
    /// `as=person`).
    InstantPerson,
    /// F1 and F2 (a judged turn's opener re-wrapped and revealed by a resize) and SAME (a new turn
    /// answered with the very words of the one before).
    SameWords,
    /// F1 by a scroll (a judged turn's opener scrolled into sight from the history above the
    /// screen by a wheel, nothing submitted).
    TallScroll,
    /// F3 (a first turn ended on a tool's row after a launch).
    ToolEnd,
    /// LADDER (the back-off ladder doubled to its cap, scaled).
    Ladder,
    /// W4 (`--mode wall-redraw`).
    RedrawWall,
    /// W5 (`--mode wall-switch`, [`switch_step`]).
    SwitchWall,
    /// W6 (`--mode wall-unread`, [`unread_step`]).
    UnreadWall,
    /// W8 (`--mode wall-cruise`).
    CruiseWall,
    /// W9 (`--mode wall-relaunch`, [`relaunch_step`]).
    RelaunchWall,
    /// W10 (`--mode wall-tap`, [`tap::step`]).
    TapWall,
    /// W11 (`--mode wall-tap-narrow`, [`tap::step`]).
    NarrowTapWall,
    /// HL (Land: one defense to every act over an untracked file, held at its second answer).
    SameLand,
    /// HC (Cruise: one reply to every act, paced on the ladder and then asked for a different
    /// approach, never stopped: [`paced`]).
    SameCruise,
    /// LH (a clean defense with a clause the reader reads open: the hinted land check, landed).
    LandHinted,
    /// LL (the check answered with a list, the work done, a second check after it, landed).
    LandListed,
    /// LS (the check answered with a list, the same defense again with no work: held, quoted).
    LandSameList,
    /// LR (the check answered with a list, the work done, the second check answered with the
    /// list reworded: held, the new words quoted).
    LandReworded,
    /// LX (slice 4c: a defense over an untracked file contradicted, the file landed, then the
    /// check and `LANDED`).
    LandContradicted,
    /// LY (slice 4c's Open leg: the defense said after real work, the check's list, the nudge
    /// quoting it answered by the audited defense again, contradicted with the list quoted, then
    /// held at the same reply).
    LandContradictedList,
    /// ANGLES (Cruise: every reply its own real work and a done claim, each check another angle).
    CruiseAngles,
    /// SWA (Cruise at rest in a short-turn back-off, switched to Land: [`switch`]).
    SwitchAgent,
    /// SWS (the same, under a draft that stands over the switch).
    SwitchSeq,
    /// SWC (Cruise at rest under a person's command left past their grace: [`switch`]).
    CommandDraft,
    /// SWF (Cruise in a back-off, switched to Land then Off within 300 ms).
    SwitchFlip,
    /// SWO (Cruise's replies repeated, switched to Land, the same reply to Land's one ask, which
    /// Land holds: [`switch::judge_over_held`]).
    SwitchOverHeld,
    /// SWB (Cruise switched to Land 5 s into a long busy turn).
    SwitchBusy,
    /// O1 (`--mode outage-land`, [`outage`]).
    LandOutage,
    /// O2 (`--mode outage-cruise`, [`outage`]).
    CruiseOutage,
    /// OVL (`--mode sysmsg`): the API overloaded ([`sysmsg::Form::Overload`]).
    SysOverload,
    /// 5XX (`--mode sysmsg`): the API's server error ([`sysmsg::Form::Outage`]).
    SysOutage,
    /// 429 (`--mode sysmsg`): a request rate limit ([`sysmsg::Form::Rate`]).
    SysRate,
    /// NET (`--mode sysmsg`): the API host unreachable ([`sysmsg::Form::Drop`]).
    SysDrop,
    /// WIT (`--mode sysmsg`): the API overloaded under two tabs of one route, this one the first
    /// ([`sysmsg::Form::Witness`]); its lab runs [`Chain::SysWitnessMate`] beside it.
    SysWitness,
    /// WTN (`--mode sysmsg`): the second tab on the witness lab's route, prompted into the
    /// standing wall past the first's ([`sysmsg::mate_step`]), resumed on the first's answered
    /// turn ([`sysmsg::judge_witnessed`]).
    SysWitnessMate,
    /// LOG (`--mode sysmsg`): an expired login under Cruise ([`sysmsg::Form::Login`]).
    SysLogin,
    /// LOVL (`--mode sysmsg`): the API overloaded under Land (v6 §14.17.5.6).
    SysLandOverload,
    /// L5XX (`--mode sysmsg`): the API's server error under Land.
    SysLandOutage,
    /// L429 (`--mode sysmsg`): a request rate limit under Land.
    SysLandRate,
    /// LNET (`--mode sysmsg`): the API host unreachable under Land.
    SysLandDrop,
    /// LLOG (`--mode sysmsg`): an expired login under Land.
    SysLandLogin,
    /// L20 (`--mode dialog`: a question dialog under Land, answered by the land text).
    LandDialog,
    /// C5 (`--mode dialog`: the same dialog under Cruise, answered by its Recommended option).
    CruiseDialog,
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
    P4L,
    P4C,
    P4P,
    L12,
    L13,
    L14,
    L15,
    C3,
    C4,
    P5,
    Cap,
    W1,
    W2,
    /// The redraw after a typed resume (`--mode wall-redraw`).
    W4,
    /// The cross-program end (`--mode wall-switch`).
    W5,
    /// The unread limit told past `FUTILE_AFTER` (`--mode wall-unread`).
    W6,
    /// A usage wall under Cruise (`--mode wall-cruise`).
    W8,
    /// The relaunch under a standing wall (`--mode wall-relaunch`).
    W9,
    /// A five-hour wall timed by the usage tap: Esc on the countdown, a new loop in the wait
    /// (`--mode wall-tap`).
    W10,
    /// The same wall timed by the tap in a pane too narrow for its countdown's words
    /// (`--mode wall-tap-narrow`).
    W11,
    L16,
    L17,
    /// A landing in a repository with no remote: its receipt `no-remote` (slice 9c, land-n).
    L18,
    /// A landing in a plain folder: its receipt `no-repo` (slice 9c, land-o).
    L19,
    N1,
    N2,
    N3,
    N4,
    /// The harness's end (§12.5.4): the window at rest with ends and needs standing, then each
    /// chain's chip in front ([`surfaces::judge_end`]).
    End,
    /// A person's keystroke into an instant turn no read saw run: their own exchange, left to them.
    F18,
    /// A judged turn's opener re-wrapped and revealed by a resize, nothing submitted: not judged
    /// again (F1 and F2).
    Rewrap,
    /// A new turn answered with the very words of the reply before it, under its own opener:
    /// judged as its own point.
    SameWords,
    /// A judged turn's opener scrolled into sight from the history above the screen by a wheel,
    /// nothing submitted: not judged again (F1's other path).
    Scrolled,
    /// A first turn ended on a tool's row after a launch: judged (F3).
    F3,
    /// The back-off ladder doubled to its cap, each act at its due (scaled).
    Ladder,
    /// One defense to every act under Land, over an untracked file: held at its second answer
    /// (ruling F), and the hold said ([`held`](surfaces::held)).
    HL,
    /// One reply to every done check under Cruise: held at its second answer, and said.
    HC,
    /// A clean defense the reader reads open: the hinted land check quotes it, and `LANDED` lands.
    LH,
    /// The check answered with a list: quoted, the work done, a second check only after it.
    LL,
    /// The check answered with a list, then the same defense with no work: held, the list quoted.
    LS,
    /// The check answered with a list, worked, the second check answered with the list reworded:
    /// the hold quotes the new words (slice 4b).
    LR,
    /// A false defense over an untracked file: Land's nudge leads with the contradiction, the
    /// file is landed, then the check and `LANDED` land it (slice 4c, [`judge_contradicted`]).
    LX,
    /// The audited defense said again while the agent's list stands: Land's nudge leads with the
    /// contradiction quoting the list, then the same reply is held (slice 4c's Open leg,
    /// [`judge_contradicted_list`]).
    LY,
    /// A new kind of work at each done claim under Cruise ([`angles`]): six angles in turn, then
    /// the first again, never held.
    Angles,
    /// A Cruise tab at rest in a short-turn back-off switched to Land: land@v1 within 5 s of the
    /// switch, before the back-off's due ([`switch::judge_at_rest`]).
    SwitchAgent,
    /// The same under a draft: the switch's wake journaled, nothing typed while the draft stands,
    /// land@v1 within 5 s of its clearing ([`switch::judge_draft`]).
    SwitchSeq,
    /// A person's command (`/help`) left at rest past their grace: nothing sent and the hold
    /// journalled once; cleared, the loop goes on past the grace from the clearing
    /// ([`switch::judge_command`], v6 §14.17.5.8).
    CommandDraft,
    /// Land then Off within 300 ms: nothing typed ([`switch::judge_flip`]).
    SwitchFlip,
    /// A tab held under Cruise switched to Land: land@v1 once, opened by the switch's why, and
    /// held again at the same answer, nothing typed past it ([`switch::judge_over_held`]).
    SwitchOverHeld,
    /// A switch 5 s into a long busy turn: nothing typed mid-turn, land@v1 within 3 s after the
    /// turn's point ([`switch::judge_busy`]).
    SwitchBusy,
    /// An API overload standing from the first prompt under Land: only tries on the outage's
    /// rungs, the first Land's instruction, one resume after the lift, landed on its answer
    /// ([`outage::judge`]).
    O1,
    /// An API overload met by Cruise's own act: only tries on the outage's rungs, one resume
    /// after the lift, and Cruise goes on ([`outage::judge`]).
    O2,
    /// A system message's wait (v6 §14.17.5), one per form ([`sysmsg::Form`], [`sysmsg::judge`]).
    Sys(sysmsg::Form),
    /// An outage cleared on a witness (v6 §14.17.5.4): the witness lab's second tab resumes on
    /// the first tab's answered turn, *has cleared*, with no blind try past the clearing
    /// ([`sysmsg::judge_witnessed`]).
    Witnessed,
    /// A system message's wait under Land (v6 §14.17.5.6): its form's wants as under Cruise, no
    /// act of Land's inside it, and Land carrying on past the resume to its landing or hold
    /// ([`sysmsg::judge`]); the expired login's are [`sysmsg::login::judge`]'s.
    SysLand(sysmsg::Form),
    /// The person's sign-in after an expired login (v6 §14.17.5.6), `true` under Land: the lab
    /// plays it last ([`sysmsg::login`]'s `step`), and the tab goes on once
    /// ([`sysmsg::login::judge_signed_in`]).
    SignedIn(bool),
    /// A question dialog under Land: the land text in its free-text row (`answer-land@v1`), no
    /// hand-over, then landed ([`cover::judge_dialog`]).
    L20,
    /// The same dialog under Cruise (L20's control): its Recommended option pressed.
    C5,
}

impl Scenario {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Scenario::O1 => "O1",
            Scenario::O2 => "O2",
            Scenario::Sys(f) => f.id(),
            Scenario::SysLand(f) => f.land_id(),
            Scenario::SignedIn(false) => "SGN",
            Scenario::SignedIn(true) => "LSGN",
            Scenario::Witnessed => "WTN",
            Scenario::L20 => "L20",
            Scenario::C5 => "C5",
            Scenario::W2 => "W2",
            Scenario::W4 => "W4",
            Scenario::W5 => "W5",
            Scenario::W6 => "W6",
            Scenario::W8 => "W8",
            Scenario::W9 => "W9",
            Scenario::W10 => "W10",
            Scenario::W11 => "W11",
            Scenario::L12 => "L12",
            Scenario::L13 => "L13",
            Scenario::L14 => "L14",
            Scenario::L15 => "L15",
            Scenario::C3 => "C3",
            Scenario::C4 => "C4",
            Scenario::P5 => "P5",
            Scenario::Cap => "CAP",
            Scenario::W1 => "W1",
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
            Scenario::P4L => "P4L",
            Scenario::P4C => "P4C",
            Scenario::P4P => "P4P",
            Scenario::L16 => "L16",
            Scenario::L17 => "L17",
            Scenario::L18 => "L18",
            Scenario::L19 => "L19",
            Scenario::N1 => "N1",
            Scenario::N2 => "N2",
            Scenario::N3 => "N3",
            Scenario::N4 => "N4",
            Scenario::End => "END",
            Scenario::F18 => "F18",
            Scenario::Rewrap => "F1F2",
            Scenario::SameWords => "SAME",
            Scenario::Scrolled => "F1S",
            Scenario::F3 => "F3",
            Scenario::Ladder => "LADDER",
            Scenario::HL => "HL",
            Scenario::HC => "HC",
            Scenario::LH => "LH",
            Scenario::LL => "LL",
            Scenario::LS => "LS",
            Scenario::LR => "LR",
            Scenario::LX => "LX",
            Scenario::LY => "LY",
            Scenario::Angles => "ANGLES",
            Scenario::SwitchAgent => "SWA",
            Scenario::SwitchSeq => "SWS",
            Scenario::CommandDraft => "SWC",
            Scenario::SwitchFlip => "SWF",
            Scenario::SwitchOverHeld => "SWO",
            Scenario::SwitchBusy => "SWB",
        }
    }

    /// [`Self::what`] on harness `h`: the expired login's scenarios in its own words
    /// ([`sysmsg::login::what_on`]), every other scenario's alike on all.
    #[must_use]
    pub const fn what_on(self, h: Harness) -> &'static str {
        match self {
            Scenario::Sys(sysmsg::Form::Login) => sysmsg::login::what_on(h, false, false),
            Scenario::SysLand(sysmsg::Form::Login) => sysmsg::login::what_on(h, true, false),
            Scenario::SignedIn(land) => sysmsg::login::what_on(h, land, true),
            _ => self.what(),
        }
    }

    /// [`Self::what_on`] under the lab's login `arm`: Claude Code's expired login under settings
    /// that pin its method in its own words ([`sysmsg::login::what_forced`]).
    #[must_use]
    pub const fn what_under(self, h: Harness, arm: sysmsg::login::LoginArm) -> &'static str {
        match (self, h, arm) {
            (
                Scenario::Sys(sysmsg::Form::Login),
                Harness::Claude,
                sysmsg::login::LoginArm::Forced,
            ) => sysmsg::login::what_forced(false, false),
            (
                Scenario::SysLand(sysmsg::Form::Login),
                Harness::Claude,
                sysmsg::login::LoginArm::Forced,
            ) => sysmsg::login::what_forced(true, false),
            (Scenario::SignedIn(land), Harness::Claude, sysmsg::login::LoginArm::Forced) => {
                sysmsg::login::what_forced(land, true)
            }
            _ => self.what_on(h),
        }
    }

    /// What it holds, for the summary.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Scenario::L1 => "a work reply: land@v1 typed",
            Scenario::L2 => "a defense beside a leftover: a nudge naming it",
            Scenario::L3 => "NEEDS YOU and a defense: the need raised, carried on",
            Scenario::L4 => "a defense over an untracked file: the contradiction naming it",
            Scenario::L5 => "a clean defense, clean repository: landed, then quiet",
            Scenario::L6 => "a tall reply, NEEDS YOU above the window: not landed",
            Scenario::L7 => "a worker's task landed: record done why=landed",
            Scenario::L8 => "a defense over a changed file: the contradiction naming it",
            Scenario::L9 => "a defense over a stash entry: the contradiction naming it",
            Scenario::L10 => "a defense over an unpushed commit: the contradiction naming it",
            Scenario::L11 => "a defense over a commit off origin/main: the contradiction naming it",
            Scenario::C1 => "a done claim: continue-done-check@v1",
            Scenario::C2 => "its defense read: never DONE, the next point goes on",
            Scenario::Off => "a done reply under Off: nothing typed, no supervisor",
            Scenario::P4L => "instant replies under Land: each judged, landed, no act twice",
            Scenario::P4C => "instant replies under Cruise: each judged at once, no act twice",
            Scenario::P4P => "a person's prompt mid-chain, answered at once: judged at once",
            Scenario::L12 => "a question in prose: land-answer@v1, then its defense lands",
            Scenario::L13 => "a person's own exchange: LEFT, theirs, nothing typed",
            Scenario::L14 => "P2 live under Land: replies opening Wrote read",
            Scenario::L15 => "a spawned worker landed: await task, result",
            Scenario::C3 => "P2 live under Cruise: a done claim opening Wrote read",
            Scenario::C4 => {
                "the owner's two-paragraph continuation: one line, never the agent's reply"
            }
            Scenario::P5 => "a worker's record past its startup deadline: working",
            Scenario::Cap => "continue_per_hour = 1: acts stop, then resume",
            Scenario::W1 => "a usage wall under Land: nothing typed into it, resumed, landed",
            Scenario::L16 => "an end past the work floor, seen by a look at its tab",
            Scenario::L17 => "an end past the work floor, seen by a press on its row",
            Scenario::L18 => "a landing with no remote: what aterm checked, no-remote",
            Scenario::L19 => "a landing in a plain folder: what aterm checked, no-repo",
            Scenario::N1 => "Cruise names two needs: needs_n=2, carried on",
            Scenario::N2 => "a defense names none: the needs cleared, needs_n=-",
            Scenario::N3 => "the first need again: needs_n=1",
            Scenario::N4 => "it and another: needs_n=2",
            Scenario::End => "the window at rest, then each chain's chip in front",
            Scenario::W2 => {
                "its notice scrolled away: the remembered wall waited out, resumed, landed"
            }
            Scenario::W4 => {
                "the typed resume meets the wall again: the new reset waited out, landed"
            }
            Scenario::W5 => {
                "another agent in the tab under the first's limit: acted on at once, landed"
            }
            Scenario::W6 => "a limit held unread an hour past its reset: told, still held",
            Scenario::W8 => {
                "a usage wall under Cruise: nothing typed into it, resumed, Cruise goes on"
            }
            Scenario::W9 => {
                "the agent launched again under its wall: nothing typed into it, resumed, landed"
            }
            Scenario::W10 => {
                "the tap times the wall: Esc on the countdown, a new loop in the wait, resumed at the tap's reset, landed"
            }
            Scenario::W11 => {
                "the tap times the wall in a narrow pane: the wait to the tap's reset, nothing typed into it, landed"
            }
            Scenario::O1 => {
                "an overload from the first prompt under Land: tries on its rungs alone, resumed once, landed"
            }
            Scenario::O2 => {
                "an overload met by Cruise: tries on its rungs alone, resumed once, Cruise goes on"
            }
            Scenario::Sys(f) => f.what(),
            Scenario::SysLand(f) => f.what_land(),
            Scenario::SignedIn(land) => sysmsg::login::what_on(Harness::Claude, land, true),
            Scenario::Witnessed => {
                "the second tab of one route resumes on the first's answered turn: has cleared, no blind try past the clearing"
            }
            Scenario::F18 => {
                "a person's key into an instant Cruise turn: judged on the ladder, no short \
                 answer added, the work on at its due"
            }
            Scenario::Rewrap => "a judged opener re-wrapped by a resize: not judged again",
            Scenario::SameWords => "a new turn answered with the same words: its own point",
            Scenario::Scrolled => {
                "a judged opener scrolled into sight from the history: not judged again"
            }
            Scenario::F3 => "a first turn ended on a tool's row after a launch: judged",
            Scenario::Ladder => "the back-off ladder doubled to its cap, each act at its due",
            Scenario::HL => "one defense to every Land act: held at its second answer, and said so",
            Scenario::HC => {
                "one reply to every act under Cruise: paced on the ladder, then a different \
                 approach asked, never stopped"
            }
            Scenario::LH => "a clean defense read open: the hinted land check, LANDED, landed",
            Scenario::LL => "the check answered with a list: quoted, worked, checked again, landed",
            Scenario::LS => "the check's list, then the same defense: held, the list quoted",
            Scenario::LR => {
                "the check's list, worked, the list reworded: held, the new words quoted"
            }
            Scenario::LX => {
                "a false defense over an untracked file: the contradiction leads, landed after it"
            }
            Scenario::LY => {
                "the check's list, then the audited defense again: the contradiction quotes it, held"
            }
            Scenario::Angles => {
                "six done claims after real work: six kinds of work asked in turn, never held"
            }
            Scenario::SwitchAgent => {
                "Cruise at rest in a back-off switched to Land: land@v1 within 5 s, before its due"
            }
            Scenario::SwitchSeq => {
                "switched under a draft: woken, nothing typed over it, land@v1 once it clears"
            }
            Scenario::CommandDraft => {
                "a person's command left past their grace: never sent, held once; cleared, goes on"
            }
            Scenario::SwitchFlip => "Land then Off within 300 ms: nothing typed",
            Scenario::SwitchOverHeld => {
                "a Cruise tab's repeated reply switched to Land: Land asks once, held at the same \
                 answer"
            }
            Scenario::SwitchBusy => {
                "switched 5 s into a busy turn: nothing typed mid-turn, land@v1 at its point"
            }
            Scenario::L20 => {
                "a question dialog under Land: the land text written, never handed over, landed"
            }
            Scenario::C5 => "the same dialog under Cruise: its Recommended option pressed",
        }
    }
}

/// How a chain's conversation opens once its agent stands at its composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    /// Its first prompt, typed as `lab turn` types one.
    Prompt,
    /// Its first prompt typed as a person types it: the draft stands in the composer, then Enter
    /// ([`person_turn`]).
    Person,
    /// No prompt: the manager tab stands at its composer (L15's starter).
    Idle,
    /// The tab is the worker the manager's spawn opens, its task the prompt.
    Spawned,
}

impl Chain {
    /// The measured pass's chains (`--mode both`), with P4's instant ones and the window's three
    /// (L16, L17, N1–N4).
    pub const ALL: [Chain; 26] = [
        Chain::LandA,
        Chain::LandN,
        Chain::LandO,
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
        Chain::InstantLand,
        Chain::InstantCruise,
        Chain::LandJ,
        Chain::LandK,
        Chain::CruiseN,
        Chain::SameLand,
        Chain::SameCruise,
        Chain::LandHinted,
        Chain::LandListed,
        Chain::LandSameList,
        Chain::LandReworded,
        Chain::LandContradicted,
        Chain::LandContradictedList,
    ];

    /// The coverage chains (`--mode cover`): what the measured pass left unobserved, and the Off
    /// control. land-q and land-t open first, so P5 and L12 read their rows on the page's one screen.
    pub const COVER: [Chain; 9] = [
        Chain::LandQ,
        Chain::LandT,
        Chain::LandP,
        Chain::LandW,
        Chain::WroteC,
        Chain::Mgr,
        Chain::Worker,
        Chain::CruiseO,
        Chain::Off,
    ];

    /// The cap's chains (`--mode cap`, on a lab of their own: the cap is the instance's).
    pub const CAPPED: [Chain; 2] = [Chain::Cap, Chain::Off];

    /// The wall's chain (`--mode wall`, alone on a lab of its own: the stub's usage wall meets
    /// every conversation it serves, so not even the Off control shares it).
    pub const WALLED: [Chain; 1] = [Chain::Wall];

    /// The remembered wall's chain (`--mode wall-hid`, alone on a lab of its own for W1's reason).
    pub const WALL_HID: [Chain; 1] = [Chain::WallHid];

    /// The residuals' chains (`--mode resid`): F18's beside its negative control (P4C's chain,
    /// whose person's prompt goes through the control socket and is judged as a turn of the
    /// conversation); F1, F2 and SAME's; F1's by a scroll; F3's; and the Off control.
    pub const RESID: [Chain; 6] = [
        Chain::InstantCruise,
        Chain::InstantPerson,
        Chain::SameWords,
        Chain::TallScroll,
        Chain::ToolEnd,
        Chain::Off,
    ];

    /// The ladder's chain (`--mode ladder`, on a lab of its own: the scale is the instance's,
    /// [`ladder_env`]) and the Off control.
    pub const LADDER: [Chain; 2] = [Chain::Ladder, Chain::Off];

    /// The redraw's chain (`--mode wall-redraw`, alone for W1's reason).
    pub const WALL_REDRAW: [Chain; 1] = [Chain::RedrawWall];

    /// The cross-program end's chain (`--mode wall-switch`, alone for W1's reason).
    pub const WALL_SWITCH: [Chain; 1] = [Chain::SwitchWall];

    /// The unread limit's chain (`--mode wall-unread`, alone for W1's reason).
    pub const WALL_UNREAD: [Chain; 1] = [Chain::UnreadWall];

    /// The Cruise wall's chain (`--mode wall-cruise`, alone for W1's reason).
    pub const WALL_CRUISE: [Chain; 1] = [Chain::CruiseWall];

    /// The relaunch's chain (`--mode wall-relaunch`, alone for W1's reason).
    pub const WALL_RELAUNCH: [Chain; 1] = [Chain::RelaunchWall];

    /// The tapped wall's chain (`--mode wall-tap`, alone for W1's reason).
    pub const WALL_TAP: [Chain; 1] = [Chain::TapWall];

    /// The narrow tapped wall's chain (`--mode wall-tap-narrow`, alone for W1's reason).
    pub const WALL_TAP_NARROW: [Chain; 1] = [Chain::NarrowTapWall];

    /// The angles' chain (`--mode angles`, on a lab of its own: its replies hold the stub for
    /// minutes each) and the Off control.
    pub const ANGLES: [Chain; 2] = [Chain::CruiseAngles, Chain::Off];

    /// The switch's chains (`--mode switch`, [`switch`]): a running Cruise tab's word switched
    /// mid-run, and the Off control. The held chain's tab comes first, so its row stands on the
    /// Harness page's first screen, which is all a read of the page sees.
    pub const SWITCH: [Chain; 7] = [
        Chain::SwitchOverHeld,
        Chain::SwitchAgent,
        Chain::SwitchSeq,
        Chain::SwitchFlip,
        Chain::SwitchBusy,
        Chain::CommandDraft,
        Chain::Off,
    ];

    /// The Land outage's chain (`--mode outage-land`, alone for W1's reason: the stub's outage
    /// meets every conversation it serves; the scale is the instance's, [`ladder_env`]).
    pub const OUTAGE_LAND: [Chain; 1] = [Chain::LandOutage];

    /// The Cruise outage's chain (`--mode outage-cruise`, alone for the same reasons).
    pub const OUTAGE_CRUISE: [Chain; 1] = [Chain::CruiseOutage];

    /// The system messages' chains (`--mode sysmsg`), each alone on a lab of its own for W1's
    /// reason ([`sysmsg::Form`]): each form's under Cruise ([`sysmsg::Form::ALL`]'s order), then
    /// each form's under Land that has one ([`sysmsg::Form::LAND`]'s, v6 §14.17.5.6).
    pub const SYSMSG: [Chain; 11] = [
        Chain::SysOverload,
        Chain::SysOutage,
        Chain::SysRate,
        Chain::SysDrop,
        Chain::SysWitness,
        Chain::SysLogin,
        Chain::SysLandOverload,
        Chain::SysLandOutage,
        Chain::SysLandRate,
        Chain::SysLandDrop,
        Chain::SysLandLogin,
    ];
    /// The question dialog's chains (`--mode dialog`): L20 under Land, C5 its Cruise control, and
    /// the Off control.
    pub const DIALOG: [Chain; 3] = [Chain::LandDialog, Chain::CruiseDialog, Chain::Off];

    /// Every chain of every mode, each once: what `--chains` may name.
    #[must_use]
    pub fn every() -> Vec<Chain> {
        let mut v: Vec<Chain> = Chain::ALL.to_vec();
        for c in Chain::COVER
            .into_iter()
            .chain(Chain::CAPPED)
            .chain(Chain::WALLED)
            .chain(Chain::WALL_HID)
            .chain(Chain::RESID)
            .chain(Chain::LADDER)
            .chain(Chain::WALL_REDRAW)
            .chain(Chain::WALL_SWITCH)
            .chain(Chain::WALL_UNREAD)
            .chain(Chain::WALL_CRUISE)
            .chain(Chain::WALL_RELAUNCH)
            .chain(Chain::WALL_TAP)
            .chain(Chain::WALL_TAP_NARROW)
            .chain(Chain::ANGLES)
            .chain(Chain::SWITCH)
            .chain(Chain::OUTAGE_LAND)
            .chain(Chain::OUTAGE_CRUISE)
            .chain(Chain::SYSMSG)
            .chain([Chain::SysWitnessMate])
            .chain(Chain::DIALOG)
        {
            if !v.contains(&c) {
                v.push(c);
            }
        }
        v
    }

    /// The chains a mode runs: the Off control in every mode but the walled ones.
    #[must_use]
    pub fn of_mode(mode: Mode) -> Vec<Chain> {
        match mode {
            Mode::Cover => Chain::COVER.to_vec(),
            Mode::Cap => Chain::CAPPED.to_vec(),
            Mode::Wall => Chain::WALLED.to_vec(),
            Mode::WallHid => Chain::WALL_HID.to_vec(),
            Mode::Resid => Chain::RESID.to_vec(),
            Mode::Ladder => Chain::LADDER.to_vec(),
            Mode::WallRedraw => Chain::WALL_REDRAW.to_vec(),
            Mode::WallSwitch => Chain::WALL_SWITCH.to_vec(),
            Mode::WallUnread => Chain::WALL_UNREAD.to_vec(),
            Mode::WallCruise => Chain::WALL_CRUISE.to_vec(),
            Mode::WallRelaunch => Chain::WALL_RELAUNCH.to_vec(),
            Mode::WallTap => Chain::WALL_TAP.to_vec(),
            Mode::WallTapNarrow => Chain::WALL_TAP_NARROW.to_vec(),
            Mode::Angles => Chain::ANGLES.to_vec(),
            Mode::Switch => Chain::SWITCH.to_vec(),
            Mode::OutageLand => Chain::OUTAGE_LAND.to_vec(),
            Mode::OutageCruise => Chain::OUTAGE_CRUISE.to_vec(),
            Mode::Sysmsg => Chain::SYSMSG.to_vec(),
            Mode::Dialog => Chain::DIALOG.to_vec(),
            Mode::Land | Mode::Cruise | Mode::Both => Chain::ALL
                .into_iter()
                .filter(|c| match c.autopilot() {
                    "land" => mode != Mode::Cruise,
                    "cruise" => mode != Mode::Land,
                    _ => true,
                })
                .collect(),
        }
    }

    /// The chains of `mode` harness `h` runs: those out of its reach left out
    /// ([`wall_out_of_reach`]).
    #[must_use]
    pub fn of_mode_on(mode: Mode, h: Harness) -> Vec<Chain> {
        Chain::of_mode(mode)
            .into_iter()
            .filter(|c| match c {
                Chain::WallHid => wall_out_of_reach(h, Mode::WallHid).is_none(),
                Chain::RedrawWall => wall_out_of_reach(h, Mode::WallRedraw).is_none(),
                Chain::UnreadWall => wall_out_of_reach(h, Mode::WallUnread).is_none(),
                Chain::RelaunchWall => wall_out_of_reach(h, Mode::WallRelaunch).is_none(),
                Chain::TapWall => wall_out_of_reach(h, Mode::WallTap).is_none(),
                Chain::NarrowTapWall => wall_out_of_reach(h, Mode::WallTapNarrow).is_none(),
                Chain::LandOutage => wall_out_of_reach(h, Mode::OutageLand).is_none(),
                Chain::CruiseOutage => wall_out_of_reach(h, Mode::OutageCruise).is_none(),
                Chain::LandDialog | Chain::CruiseDialog => dialog_out_of_reach(h).is_none(),
                c => sysmsg::Form::of(*c).is_none_or(|f| f.out_of_reach(h).is_none()),
            })
            .collect()
    }

    /// It meets a system message's stand-in ([`sysmsg::step`]): `--mode sysmsg`'s chains.
    #[must_use]
    pub const fn sysmsg(self) -> bool {
        sysmsg::Form::of(self).is_some()
    }

    /// THE GATE ON A LAND CHAIN'S FIRST ANSWER (v6 §14.17.5.6): a system message's chain under
    /// Land, whose stand-in the lab raises once the stub read the first prompt and then opens
    /// this gate ([`sysmsg::step`]), so Land's act on that answer meets the stand-in (Land acts on
    /// its first point within seconds, sooner than the lab's look could raise it after the point).
    #[must_use]
    pub fn raise_gate(self) -> Option<String> {
        (self.sysmsg() && self.autopilot() == "land").then(|| format!("{}-raise", self.name()))
    }

    /// Its stand-in is an expired login ([`sysmsg::Form::Login`]): Claude Code launches with a
    /// claude.ai login of the lab's, so the stub's 401 is that login's ([`launcher_with`]).
    #[must_use]
    pub fn login(self) -> bool {
        sysmsg::Form::of(self) == Some(sysmsg::Form::Login)
    }

    /// It meets the stub's outage ([`outage::step`]): O1's chain and O2's.
    #[must_use]
    pub const fn outage(self) -> bool {
        matches!(self, Chain::LandOutage | Chain::CruiseOutage)
    }

    /// Its outage stands from before its first prompt, so the prompt meets it and Land's
    /// instruction was never given (O1's); O2's is raised at its first point, so Cruise's own act
    /// meets it ([`outage::outage_move`]).
    #[must_use]
    pub const fn outage_at_prompt(self) -> bool {
        matches!(self, Chain::LandOutage)
    }

    /// It meets the stub's usage wall ([`wall_step`]): W1's chain, W2's, and W4's to W9's.
    #[must_use]
    pub const fn walled(self) -> bool {
        matches!(
            self,
            Chain::Wall
                | Chain::WallHid
                | Chain::RedrawWall
                | Chain::SwitchWall
                | Chain::UnreadWall
                | Chain::CruiseWall
                | Chain::RelaunchWall
                | Chain::TapWall
                | Chain::NarrowTapWall
        )
    }

    /// Its Claude Code runs with aterm's usage tap on its line and its wall's 429 carries the
    /// window's figures ([`tap`]): W10's chain and W11's.
    #[must_use]
    pub const fn tapped(self) -> bool {
        matches!(self, Chain::TapWall | Chain::NarrowTapWall)
    }

    /// How many times the lab raises its wall again at the reset instead of lifting it
    /// ([`wall_move`]): W4's once (under the resume typed past the first reset), none for the
    /// other chains.
    #[must_use]
    pub fn shifts(self) -> usize {
        match self {
            Chain::RedrawWall => 1,
            _ => 0,
        }
    }

    /// How far each raise again moves the reset on ([`wall_move`]): whole minutes, so each reset
    /// stays on the minute the notices name; nothing for a chain that never raises its wall again
    /// ([`Chain::shifts`] 0).
    #[must_use]
    pub fn shift_step_ms(self) -> i64 {
        match self {
            Chain::RedrawWall => REDRAW_AHEAD_MS,
            _ => 0,
        }
    }

    /// W2's hide runs on its tab on harness `h` ([`hide_step`]): W2's own, W6's (its notice gone
    /// before the conversation is made unread), and W4's where the resume is typed only past it
    /// (Claude Code's countdown cancelled).
    #[must_use]
    pub fn hides_on(self, h: Harness) -> bool {
        match self {
            Chain::WallHid | Chain::UnreadWall => true,
            Chain::RedrawWall => matches!(h, Harness::Claude),
            _ => false,
        }
    }

    /// How far ahead of its raising its wall resets, at least ([`wall_step`]): a chain the lab
    /// drives at the wall before `reset - `[`HIDE_BEFORE_RESET_MS`] (the hide, the tapped walls'
    /// Esc and narrow pane) has the hide's room.
    #[must_use]
    pub const fn wall_ahead_ms(self) -> i64 {
        match self {
            Chain::WallHid => WALL_HID_AHEAD_MS,
            Chain::RelaunchWall => WALL_RELAUNCH_AHEAD_MS,
            Chain::TapWall | Chain::NarrowTapWall => WALL_HID_AHEAD_MS,
            Chain::RedrawWall | Chain::UnreadWall => WALL_HID_AHEAD_MS,
            Chain::SwitchWall => WALL_SWITCH_AHEAD_MS,
            _ => WALL_AHEAD_MS,
        }
    }

    /// How its conversation opens.
    #[must_use]
    pub const fn opening(self) -> Opening {
        match self {
            Chain::LandP => Opening::Person,
            Chain::Mgr | Chain::SysWitnessMate => Opening::Idle,
            Chain::Worker => Opening::Spawned,
            _ => Opening::Prompt,
        }
    }

    /// Its hang detector: how long it may take to reach what it waits for.
    #[must_use]
    pub const fn limit_ms(self) -> i64 {
        match self {
            Chain::Cap => CAP_LIMIT_MS,
            Chain::CruiseN => CRUISE_N_LIMIT_MS,
            Chain::LandContradictedList => LIST_CONTRADICTED_LIMIT_MS,
            Chain::LandReworded => LIST_REWORDED_LIMIT_MS,
            Chain::Ladder => LADDER_LIMIT_MS,
            Chain::CruiseWall => WALL_CRUISE_LIMIT_MS,
            Chain::RelaunchWall => WALL_RELAUNCH_LIMIT_MS,
            Chain::TapWall | Chain::NarrowTapWall => WALL_TAP_LIMIT_MS,
            Chain::RedrawWall => WALL_REDRAW_LIMIT_MS,
            Chain::SwitchWall => WALL_SWITCH_LIMIT_MS,
            Chain::UnreadWall => WALL_UNREAD_LIMIT_MS,
            Chain::CruiseAngles => angles::LIMIT_MS,
            Chain::SameCruise => paced::LIMIT_MS,
            Chain::LandOutage | Chain::CruiseOutage => outage::LIMIT_MS,
            Chain::SysOverload
            | Chain::SysOutage
            | Chain::SysRate
            | Chain::SysDrop
            | Chain::SysWitness
            | Chain::SysWitnessMate
            | Chain::SysLogin
            | Chain::SysLandOverload
            | Chain::SysLandOutage
            | Chain::SysLandRate
            | Chain::SysLandDrop
            | Chain::SysLandLogin => sysmsg::LIMIT_MS,
            _ => CHAIN_LIMIT_MS,
        }
    }

    /// Its worker is one a manager waits on, so the ladder after its landing is a managed worker's
    /// ([`next_step_ms`]).
    #[must_use]
    pub fn managed(self) -> bool {
        self.tasked() || self == Chain::Worker
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Chain::LandA => "land-a",
            Chain::LandN => "land-n",
            Chain::LandO => "land-o",
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
            Chain::InstantLand => "instant-land",
            Chain::InstantCruise => "instant-cruise",
            Chain::LandQ => "land-q",
            Chain::LandP => "land-p",
            Chain::LandW => "land-w",
            Chain::WroteC => "wrote-c",
            Chain::LandT => "land-t",
            Chain::Mgr => "mgr",
            Chain::Worker => "worker",
            Chain::Cap => "cap",
            Chain::Wall => "wall",
            Chain::LandJ => "land-j",
            Chain::LandK => "land-k",
            Chain::CruiseN => "cruise-n",
            Chain::CruiseO => "cruise-o",
            Chain::WallHid => "hid-wall",
            Chain::InstantPerson => "instant-person",
            Chain::SameWords => "same-words",
            Chain::TallScroll => "tall-scroll",
            Chain::ToolEnd => "tool-end",
            Chain::Ladder => "ladder",
            Chain::RedrawWall => "redraw-wall",
            Chain::SwitchWall => "switch-wall",
            Chain::UnreadWall => "unread-wall",
            Chain::CruiseWall => "cruise-wall",
            Chain::RelaunchWall => "relaunch-wall",
            Chain::TapWall => "tap-wall",
            Chain::NarrowTapWall => "narrow-tap-wall",
            Chain::SameLand => "same-land",
            Chain::SameCruise => "same-cruise",
            Chain::LandHinted => "land-hinted",
            Chain::LandListed => "land-listed",
            Chain::LandSameList => "land-same-list",
            Chain::LandReworded => "land-reworded",
            Chain::LandContradicted => "land-contradicted",
            Chain::LandContradictedList => "land-contradicted-list",
            Chain::CruiseAngles => "cruise-angles",
            Chain::SwitchAgent => "switch-agent",
            Chain::SwitchSeq => "switch-seq",
            Chain::CommandDraft => "command-draft",
            Chain::SwitchFlip => "switch-flip",
            Chain::SwitchOverHeld => "switch-over-held",
            Chain::SwitchBusy => "switch-busy",
            Chain::LandOutage => "land-outage",
            Chain::CruiseOutage => "cruise-outage",
            Chain::SysOverload => "sys-overload",
            Chain::SysOutage => "sys-outage",
            Chain::SysRate => "sys-rate",
            Chain::SysDrop => "sys-drop",
            Chain::SysWitness => "sys-witness",
            Chain::SysWitnessMate => "sys-witness-mate",
            Chain::SysLogin => "sys-login",
            Chain::SysLandOverload => "sys-land-overload",
            Chain::SysLandOutage => "sys-land-outage",
            Chain::SysLandRate => "sys-land-rate",
            Chain::SysLandDrop => "sys-land-drop",
            Chain::SysLandLogin => "sys-land-login",
            Chain::LandDialog => "land-dialog",
            Chain::CruiseDialog => "cruise-dialog",
        }
    }

    /// The word its conversation carries (its first prompt), by which the stub's rules know it: no
    /// other chain's word holds it.
    #[must_use]
    pub fn token(self) -> String {
        format!("FOREVERLAB-{}.", self.name().to_ascii_uppercase())
    }

    /// The Autopilot word its tab is set to.
    #[must_use]
    pub const fn autopilot(self) -> &'static str {
        match self {
            Chain::Cruise
            | Chain::InstantCruise
            | Chain::WroteC
            | Chain::CruiseN
            | Chain::InstantPerson
            | Chain::SameWords
            | Chain::TallScroll
            | Chain::Ladder
            | Chain::CruiseO
            | Chain::CruiseWall
            | Chain::SameCruise
            | Chain::CruiseAngles
            | Chain::SwitchAgent
            | Chain::SwitchSeq
            | Chain::CommandDraft
            | Chain::SwitchFlip
            | Chain::SwitchOverHeld
            | Chain::SwitchBusy
            | Chain::CruiseOutage
            | Chain::SysOverload
            | Chain::SysOutage
            | Chain::SysRate
            | Chain::SysDrop
            | Chain::SysWitness
            | Chain::SysWitnessMate
            | Chain::SysLogin
            | Chain::CruiseDialog => "cruise",
            Chain::Off | Chain::Mgr => "off",
            _ => "land",
        }
    }

    #[must_use]
    pub const fn scenarios(self) -> &'static [Scenario] {
        match self {
            Chain::LandA => &[Scenario::L1, Scenario::L3, Scenario::L5],
            Chain::LandN => &[Scenario::L18],
            Chain::LandO => &[Scenario::L19],
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
            Chain::InstantLand => &[Scenario::P4L],
            Chain::InstantCruise => &[Scenario::P4C, Scenario::P4P],
            Chain::LandQ => &[Scenario::L12],
            Chain::LandP => &[Scenario::L13],
            Chain::LandW => &[Scenario::L14],
            Chain::WroteC => &[Scenario::C3],
            Chain::LandT => &[Scenario::P5],
            Chain::Mgr => &[],
            Chain::Worker => &[Scenario::L15],
            Chain::CruiseO => &[Scenario::C4],
            Chain::Cap => &[Scenario::Cap],
            Chain::Wall => &[Scenario::W1],
            Chain::LandJ => &[Scenario::L16],
            Chain::LandK => &[Scenario::L17],
            Chain::CruiseN => &[Scenario::N1, Scenario::N2, Scenario::N3, Scenario::N4],
            Chain::WallHid => &[Scenario::W2],
            Chain::InstantPerson => &[Scenario::F18],
            Chain::SameWords => &[Scenario::Rewrap, Scenario::SameWords],
            Chain::TallScroll => &[Scenario::Scrolled],
            Chain::ToolEnd => &[Scenario::F3],
            Chain::Ladder => &[Scenario::Ladder],
            Chain::RedrawWall => &[Scenario::W4],
            Chain::SwitchWall => &[Scenario::W5],
            Chain::UnreadWall => &[Scenario::W6],
            Chain::CruiseWall => &[Scenario::W8],
            Chain::RelaunchWall => &[Scenario::W9],
            Chain::TapWall => &[Scenario::W10],
            Chain::NarrowTapWall => &[Scenario::W11],
            Chain::SameLand => &[Scenario::HL],
            Chain::SameCruise => &[Scenario::HC],
            Chain::LandHinted => &[Scenario::LH],
            Chain::LandListed => &[Scenario::LL],
            Chain::LandSameList => &[Scenario::LS],
            Chain::LandReworded => &[Scenario::LR],
            Chain::LandContradicted => &[Scenario::LX],
            Chain::LandContradictedList => &[Scenario::LY],
            Chain::CruiseAngles => &[Scenario::Angles],
            Chain::SwitchAgent => &[Scenario::SwitchAgent],
            Chain::SwitchSeq => &[Scenario::SwitchSeq],
            Chain::CommandDraft => &[Scenario::CommandDraft],
            Chain::SwitchFlip => &[Scenario::SwitchFlip],
            Chain::SwitchOverHeld => &[Scenario::SwitchOverHeld],
            Chain::SwitchBusy => &[Scenario::SwitchBusy],
            Chain::LandOutage => &[Scenario::O1],
            Chain::CruiseOutage => &[Scenario::O2],
            Chain::SysOverload => &[Scenario::Sys(sysmsg::Form::Overload)],
            Chain::SysOutage => &[Scenario::Sys(sysmsg::Form::Outage)],
            Chain::SysRate => &[Scenario::Sys(sysmsg::Form::Rate)],
            Chain::SysDrop => &[Scenario::Sys(sysmsg::Form::Drop)],
            Chain::SysWitness => &[Scenario::Sys(sysmsg::Form::Witness)],
            Chain::SysWitnessMate => &[Scenario::Witnessed],
            Chain::SysLogin => &[
                Scenario::Sys(sysmsg::Form::Login),
                Scenario::SignedIn(false),
            ],
            Chain::SysLandOverload => &[Scenario::SysLand(sysmsg::Form::Overload)],
            Chain::SysLandOutage => &[Scenario::SysLand(sysmsg::Form::Outage)],
            Chain::SysLandRate => &[Scenario::SysLand(sysmsg::Form::Rate)],
            Chain::SysLandDrop => &[Scenario::SysLand(sysmsg::Form::Drop)],
            Chain::SysLandLogin => &[
                Scenario::SysLand(sysmsg::Form::Login),
                Scenario::SignedIn(true),
            ],
            Chain::LandDialog => &[Scenario::L20],
            Chain::CruiseDialog => &[Scenario::C5],
        }
    }

    /// What its repository holds beside its pushed `main`.
    #[must_use]
    pub const fn repo(self) -> Repo {
        match self {
            Chain::LandC | Chain::LandContradicted => Repo::Untracked,
            Chain::LandN => Repo::NoRemote,
            Chain::LandO => Repo::Plain,
            Chain::LandF => Repo::Changed,
            Chain::LandG => Repo::Stashed,
            Chain::LandH => Repo::Unpushed,
            Chain::LandI => Repo::Unlanded,
            Chain::SameLand => Repo::Untracked,
            Chain::CruiseAngles => Repo::Others,
            _ => Repo::Clean,
        }
    }

    /// Its tab carries a worker's task record (`task mark working from=ctl` before the agent
    /// starts): a manager waits on it.
    #[must_use]
    pub const fn tasked(self) -> bool {
        matches!(self, Chain::LandE | Chain::LandT)
    }

    /// Its landing is the scenario: watched past the ladder's next step after it, and the stub's
    /// dumps read for a new turn after it.
    #[must_use]
    pub const fn lands(self) -> bool {
        matches!(
            self,
            Chain::LandA
                | Chain::LandN
                | Chain::LandO
                | Chain::LandE
                | Chain::InstantLand
                | Chain::LandQ
                | Chain::LandW
                | Chain::LandHinted
                | Chain::LandListed
                | Chain::LandContradicted
                | Chain::Worker
                | Chain::Wall
                | Chain::WallHid
                | Chain::RedrawWall
                | Chain::SwitchWall
                | Chain::UnreadWall
                | Chain::RelaunchWall
                | Chain::TapWall
                | Chain::NarrowTapWall
                | Chain::LandOutage
                | Chain::LandDialog
        )
    }

    /// Its replies are sent at once: no delay before the first byte, no pause between deltas (P4:
    /// a turn over before any read sees the worker busy).
    #[must_use]
    pub const fn instant(self) -> bool {
        matches!(
            self,
            Chain::InstantLand
                | Chain::InstantCruise
                | Chain::InstantPerson
                | Chain::SameWords
                | Chain::TallScroll
                | Chain::ToolEnd
        )
    }

    /// Its script answers the acts it meets before its watch ends with one canned reply, so under
    /// Land ruling F (`4cefe18775`) stops aterm's typing into it at that reply's second answer
    /// (the same-reply hold, `EXCEPTION … kind=stalled`): land@v1 and the nudge answered alike
    /// (land-b, land-c and land-f to land-i, and land-same-list, the land check's list between;
    /// land-e lands at the first; land-d rests on its check's need, nudged never), each done check
    /// answered alike (`cruise`, and `wrote-c`'s `W_CHECKED`), and the two chains made for the
    /// hold. Under Cruise the same reply holds nothing since the continuous-forward design of
    /// 2026-11-01: Cruise waits the ladder's step and asks for a different approach
    /// ([`paced`], HC). A chain held anyway is watched past its hold; a Land chain wants the hold
    /// said ([`surfaces::held`]), and a Cruise chain or any other chain of the measured pass or
    /// the coverage chains fails at a `kind=stalled` ([`surfaces::held::stall_fault`]).
    /// `switch-over-held`'s holds are its own judge's ([`switch::judge_over_held`]).
    #[must_use]
    pub const fn same_reply(self) -> bool {
        matches!(
            self,
            Chain::LandB
                | Chain::LandC
                | Chain::LandE
                | Chain::LandF
                | Chain::LandG
                | Chain::LandH
                | Chain::LandI
                | Chain::Cruise
                | Chain::WroteC
                | Chain::SameLand
                | Chain::SameCruise
                | Chain::LandSameList
                | Chain::LandReworded
                | Chain::LandContradictedList
        )
    }

    /// A person's prompt mid-chain (P4P): typed as the first prompt is, once the done check's
    /// defense was decided, while the loop waits out its back-off.
    #[must_use]
    pub fn asks(self) -> Option<String> {
        matches!(self, Chain::InstantCruise | Chain::InstantPerson)
            .then(|| format!("{} {ASK_KEY}", self.token()))
    }

    /// Its person's prompt mid-chain ([`Self::asks`]) is typed as a person types it
    /// ([`person_turn`]: `send|key as=person`, a person's hand on the session), not through the
    /// control socket (F18).
    #[must_use]
    pub fn asks_as_person(self) -> bool {
        self == Chain::InstantPerson
    }

    /// How long the stub takes over its reply to the first prompt ([`Self::prompt`]), in ms: the
    /// rule that answers it, or the spec's plain reply, its delay and its pace over each delta.
    #[must_use]
    pub fn first_reply_ms(self) -> i64 {
        let spec = spec(&[self]);
        let rule = crate::stub::scripted_reply(&spec, &[self.prompt()]);
        let field = |k: &str| rule.and_then(|r| r.get(k)).or_else(|| spec.get(k));
        let delay = field("delay_ms")
            .and_then(J::as_i64)
            .unwrap_or(REPLY_DELAY_MS);
        let pace = field("pace_s").and_then(J::as_f64).unwrap_or(REPLY_PACE_S);
        let text = rule.map_or(Some(WORK), |r| r.get("text").and_then(J::as_str));
        let deltas = crate::stub::split_deltas(text.unwrap_or_default()).len();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "a pace of seconds over a reply's few hundred deltas"
        )]
        let paced = (pace * 1000.0 * deltas as f64) as i64;
        delay.saturating_add(paced)
    }

    /// Whether the first prompt's reply is real work past the short-turn floor
    /// (`turn_end::MIN_WORK`), so the first point closes no short turn (`cruise-angles`'): every
    /// other chain's comes within seconds and closes one ([`p3_faults`]).
    #[must_use]
    pub fn first_reply_works(self) -> bool {
        let floor = i64::try_from(turn_end::MIN_WORK.as_millis()).unwrap_or(i64::MAX);
        self.first_reply_ms() >= floor
    }

    /// THE NEED GATE (L3's land-a, L14's land-w): the name of the gate the stub holds the land
    /// check's `LANDED` behind ([`crate::stub::GATE_SUFFIX`]), opened once the lab has read the
    /// defense's need on every surface it judges ([`need_read`]); `None` for a chain whose
    /// answer goes at once.
    #[must_use]
    pub fn need_gate(self) -> Option<String> {
        matches!(self, Chain::LandA | Chain::LandW).then(|| format!("{}-need", self.name()))
    }

    /// LAND-CONTRADICTED'S GATE (slice 4c): the name of the gate the stub holds its answer to
    /// Land's contradiction behind, opened once the lab has landed `wip-notes.txt` for it
    /// ([`land_contradicted_work`]), as an agent's own commit and push would: the stub runs no
    /// tool. `None` for every other chain.
    #[must_use]
    pub fn landed_gate(self) -> Option<String> {
        (self == Chain::LandContradicted).then(|| format!("{}-landed", self.name()))
    }

    /// Whether the stub holds one of its answers behind a gate the lab's watch opens
    /// ([`Self::need_gate`], [`Self::landed_gate`]).
    #[must_use]
    pub fn gated(self) -> bool {
        self.need_gate().is_some() || self.landed_gate().is_some()
    }

    /// Whether the chain's first judged span closes on its program's own clock, sooner than a
    /// loaded lab prompts a full run: cruise-n's N1 at its first need's 2 min back-off, land-j's
    /// and land-k's L16 and L17 at the landing, about 45 s after `land@v1`. The watch's reads begin
    /// once every chain is prompted, which took Muse Code's 26 tabs about 2 min on 2026-11-05, so
    /// these go in late, before the gated chains ([`prompt_order`]).
    #[must_use]
    pub fn timed_from_its_start(self) -> bool {
        matches!(self, Chain::CruiseN | Chain::LandJ | Chain::LandK)
    }

    /// The first prompt (L13's a person's question; L15's the worker's task).
    #[must_use]
    pub fn prompt(self) -> String {
        if self == Chain::LandP {
            return format!("{} {PERSON_ASKS}", self.token());
        }
        if self == Chain::SameWords {
            return format!(
                "{} write the parser for this repository under {LONG_TOKEN} and land it",
                self.token()
            );
        }
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
        let instant = self.instant();
        let rule = |last: &str, text: &str| {
            let mut r = J::obj([
                ("when", J::s(token.as_str())),
                ("last", J::s(last)),
                ("text", J::s(text)),
            ]);
            if instant {
                r.set("delay_ms", J::Int(0));
                r.set("pace_s", J::Float(0.0));
            }
            r
        };
        let long = |last: &str, text: &str| {
            let mut r = rule(last, text);
            r.set("delay_ms", J::Int(LONG_DELAY_MS));
            r.set("pace_s", J::Float(LONG_PACE_S));
            r
        };
        let worked = |last: &str, text: &str| {
            let mut r = rule(last, text);
            r.set("delay_ms", J::Int(WORK_TURN_MS));
            r
        };
        let after = |last: &str, before: &str, text: &str| {
            let mut r = rule(last, text);
            r.set("after", J::s(before));
            r
        };
        // Every chain under Land answers the land check first (a `LANDED` where nothing is
        // scripted left), so no `MARK` or `NUDGE_KEY` rule answers it with a defense.
        let landed = || rule(LAND_CHECK_KEY, LANDED);
        // L3's and L14's check answer: the need their defense raised stands until the lab has
        // read it on every surface it judges ([`Chain::need_gate`]): the stub holds `LANDED`
        // until the lab opens the gate, an event, never a fixed delay that the lab's reads may
        // outlast under load.
        let landed_late = || {
            let mut r = landed();
            if let Some(gate) = self.need_gate() {
                r.set("gate", J::s(gate.as_str()));
            }
            r
        };
        let asking = |text: &str| {
            let mut r = rule("", text);
            r.set("tool", dialog_call());
            r
        };
        match self {
            Chain::LandA => vec![
                landed_late(),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, &needs_defense()),
                rule("", WORK),
            ],
            Chain::LandB => {
                let open = open_defense();
                vec![
                    rule(LAND_CHECK_KEY, CHECK_LIST),
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
                landed(),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", WORK),
            ],
            Chain::LandD => {
                let tall = tall_reply(80);
                vec![
                    rule(LAND_CHECK_KEY, &need_alone()),
                    rule(NUDGE_KEY, &tall),
                    rule(LAND_KEY, &tall),
                    rule("", WORK),
                ]
            }
            Chain::LandHinted => vec![
                landed(),
                rule(NUDGE_KEY, HINTED),
                rule(LAND_KEY, HINTED),
                rule("", WORK),
            ],
            // Its first check is answered with a list; the nudge quoting it gets work done (a
            // reply past the policy's work floor, no defense, so the short streak starts over and
            // Land's next nudge is not a back-off away); the nudge after the work gets the
            // defense, and the check that defense owes (allowed by the work) gets `LANDED`.
            Chain::LandListed => vec![
                after(LAND_CHECK_KEY, LISTED_KEY, LANDED),
                rule(LAND_CHECK_KEY, RELEASE_LIST),
                worked(LISTED_KEY, RELEASE_WORK),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", WORK),
            ],
            // Its defense over the untracked file gets Land's contradiction, whose answer waits on
            // the lab's landing of the file ([`Chain::landed_gate`]) and defends it in other words;
            // the check that defense owes over the clean repository gets `LANDED`.
            Chain::LandContradicted => {
                let mut answered = rule(CONTRADICTED_KEY, LANDED_NOTES);
                if let Some(gate) = self.landed_gate() {
                    answered.set("gate", J::s(gate.as_str()));
                }
                vec![
                    landed(),
                    answered,
                    rule(NUDGE_KEY, CLEAN),
                    rule(LAND_KEY, CLEAN),
                    rule("", WORK),
                ]
            }
            // Its defense is first said after real work (a tool's read, [`spec_on`], and past the
            // work floor), so the check audits it and no same reply keeps it; the check is
            // answered with the list; the nudge quoting the list, and the contradiction after it,
            // each get that defense again, short: the first saying with no work is contradicted,
            // the second is the same reply, held.
            Chain::LandContradictedList => {
                let open = open_defense();
                vec![
                    rule(LAND_CHECK_KEY, CHECK_LIST),
                    rule(NUDGE_KEY, &open),
                    worked(LAND_KEY, &open),
                    rule("", WORK),
                ]
            }
            Chain::LandSameList => vec![
                rule(LAND_CHECK_KEY, SAME_LIST),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", WORK),
            ],
            // Its first check is answered with a list; the nudge quoting it gets work done (so the
            // list no longer stands and the defense after it owes a second check); the second
            // check, after a nudge that quoted a list, gets the list in other words of the same
            // family; every other nudge the defense, so Land holds on the reworded list, quoted.
            Chain::LandReworded => vec![
                after(LAND_CHECK_KEY, LISTED_KEY, REWORDED_LIST),
                rule(LAND_CHECK_KEY, CHECK_LIST),
                worked(OPEN_KEY, REWORD_WORK),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", WORK),
            ],
            Chain::Cruise => vec![
                rule(CHECK_KEY, CLEAN),
                rule(CONTINUE_KEY, DONE_CLAIM),
                rule("", DONE_CLAIM),
            ],
            Chain::Off => vec![rule("", DONE_CLAIM)],
            Chain::LandQ => vec![
                landed(),
                rule(NUDGE_KEY, CLEAN),
                rule(ANSWER_KEY, CLEAN),
                rule(LAND_KEY, QUESTION),
                rule("", WORK),
            ],
            Chain::LandP => vec![
                landed(),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule(ORCHESTRATED_KEY, WORK),
                rule("", PERSON_REPLY),
            ],
            Chain::LandW => vec![
                landed_late(),
                rule(NUDGE_KEY, W_DEFENSE),
                rule(LAND_KEY, W_NEEDS),
                rule("", W_WORK),
            ],
            Chain::WroteC => vec![
                rule(CHECK_KEY, W_CHECKED),
                rule(CONTINUE_KEY, W_DONE),
                rule("", W_DONE),
            ],
            Chain::CruiseO => vec![
                rule(CHECK_KEY, CLEAN),
                rule(OWNER_KEY, DONE_CLAIM),
                rule("", WORK),
            ],
            Chain::LandT => vec![landed(), rule(NUDGE_KEY, CLEAN), rule("", WORK)],
            Chain::Cap => {
                let open = open_defense();
                vec![
                    landed(),
                    rule(NUDGE_KEY, CLEAN),
                    rule(LAND_KEY, &open),
                    rule(RESUMED_KEY, &open),
                    rule("", WORK),
                ]
            }
            Chain::Mgr => Vec::new(),
            Chain::InstantLand | Chain::LandN | Chain::LandO => vec![
                landed(),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", WORK),
            ],
            // Its first land reply holds (a work reply, no defense), so its row under its
            // manager reads `landing` with the hold's tip until the nudge's defense lands it
            // (§12.3.11's 5d); its result is still its task's reply and the defense.
            Chain::Worker => vec![landed(), rule(NUDGE_KEY, CLEAN), rule("", WORK)],
            Chain::InstantCruise | Chain::InstantPerson => vec![
                rule(ASK_KEY, ASK_REPLY),
                rule(CHECK_KEY, CLEAN),
                rule(CONTINUE_KEY, DONE_CLAIM),
                rule("", DONE_CLAIM),
            ],
            Chain::LandJ | Chain::LandK => vec![
                landed(),
                long(NUDGE_KEY, CLEAN),
                long(LAND_KEY, CLEAN),
                rule("", WORK),
            ],
            Chain::CruiseN => vec![
                rule(CHECK_KEY, &needs_reply(N3_HEAD, &[NEED_A])),
                after(
                    CONTINUE_KEY,
                    CHECK_KEY,
                    &needs_reply(N4_HEAD, &[NEED_A, NEED_C]),
                ),
                rule(CONTINUE_KEY, CLEAN),
                rule("", &needs_reply(N1_HEAD, &[NEED_A, NEED_B])),
            ],
            Chain::CruiseWall
            | Chain::CruiseOutage
            | Chain::SysOverload
            | Chain::SysOutage
            | Chain::SysRate
            | Chain::SysDrop
            | Chain::SysWitness
            | Chain::SysWitnessMate
            | Chain::SysLogin => vec![
                rule(CHECK_KEY, CLEAN),
                rule(CONTINUE_KEY, DONE_CLAIM),
                rule(MARK, DONE_CLAIM),
                rule("", DONE_CLAIM),
            ],
            // A system message's chain under Land (v6 §14.17.5.6): Land's own act meets the
            // stand-in, which the lab raises while the stub holds the first prompt's answer
            // ([`Chain::raise_gate`], [`sysmsg::step`]), since Land acts at once on that answer.
            Chain::SysLandOverload
            | Chain::SysLandOutage
            | Chain::SysLandRate
            | Chain::SysLandDrop
            | Chain::SysLandLogin => {
                let mut first = rule("", WORK);
                if let Some(gate) = self.raise_gate() {
                    first.set("gate", J::s(gate.as_str()));
                }
                vec![
                    landed(),
                    rule(NUDGE_KEY, CLEAN),
                    rule(LAND_KEY, CLEAN),
                    rule(MARK, CLEAN),
                    first,
                ]
            }
            Chain::Wall
            | Chain::WallHid
            | Chain::RedrawWall
            | Chain::SwitchWall
            | Chain::UnreadWall
            | Chain::RelaunchWall
            | Chain::TapWall
            | Chain::NarrowTapWall
            | Chain::LandOutage => vec![
                landed(),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule(MARK, CLEAN),
                rule("", WORK),
            ],
            Chain::SameWords => vec![rule(SAME_TURN_KEY, WORK), rule("", WORK)],
            Chain::TallScroll => vec![rule("", &scroll_reply())],
            Chain::ToolEnd => vec![
                landed(),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", TOOL_AFTER),
            ],
            Chain::Ladder => vec![rule("", WORK)],
            Chain::SameLand => vec![rule(MARK, CLEAN), rule("", WORK)],
            Chain::SameCruise => vec![rule(MARK, CLEAN), rule("", DONE_CLAIM)],
            Chain::CruiseAngles => angles::KEYS
                .iter()
                .zip(angles::REPLIES)
                .map(|(key, reply)| long(key, reply))
                .chain([long("", angles::FIRST)])
                .collect(),
            Chain::SwitchAgent | Chain::SwitchSeq | Chain::SwitchFlip | Chain::CommandDraft => {
                vec![
                    rule(NUDGE_KEY, CLEAN),
                    rule(LAND_KEY, CLEAN),
                    rule(CHECK_KEY, CLEAN),
                    rule("", DONE_CLAIM),
                ]
            }
            // The same reply to every act, Land's one ask at the switch included, which the
            // stand-in takes its time over so its ask is read standing.
            Chain::SwitchOverHeld => {
                let mut asked = rule(SWITCH_KEY, DONE_CLAIM);
                asked.set("delay_ms", J::Int(SWITCH_ASK_DELAY_MS));
                vec![asked, rule(MARK, DONE_CLAIM), rule("", DONE_CLAIM)]
            }
            Chain::SwitchBusy => vec![
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule(MARK, CLEAN),
                long("", WORK),
            ],
            Chain::LandDialog => vec![
                landed(),
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                asking(WORK),
            ],
            Chain::CruiseDialog => vec![
                rule(CHECK_KEY, CLEAN),
                rule(CONTINUE_KEY, DONE_CLAIM),
                asking(WORK),
            ],
        }
    }

    /// The orchestrator's turn the lab types into its tab mid-watch, and when
    /// ([`orchestrator_due`]).
    #[must_use]
    pub const fn orchestrated(self) -> Option<&'static str> {
        match self {
            Chain::LandP => Some(ORCHESTRATED),
            Chain::Cap => Some(RESUMED),
            Chain::SameWords => Some(SAME_TURN),
            _ => None,
        }
    }
}

/// A report `head` naming `needs`, each on a `NEEDS YOU:` line of its own (N1, N3, N4).
#[must_use]
pub fn needs_reply(head: &str, needs: &[&str]) -> String {
    let mut s = head.to_string();
    for n in needs {
        s.push_str(&format!("\nNEEDS YOU: {n}"));
    }
    s
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

/// L6's reply, `lines` tall: one need near its top, the defense at its bottom.
#[must_use]
pub fn tall_reply(lines: usize) -> String {
    let mut s = format!("Here is the landing report.\nNEEDS YOU: {NEED}\n");
    for i in 1..=lines {
        s.push_str(&format!("- check {i} passed\n"));
    }
    s.push_str(CLEAN);
    s
}

/// `tall-scroll`'s reply: a head, [`SCROLL_LINES`] rows of a report, then [`WORK`]: taller than
/// the window's 50-row tab, so once the turn is over its opener stands in the history above the
/// screen (the agent's own, where it draws its transcript in a view of its own).
#[must_use]
pub fn scroll_reply() -> String {
    let mut s = String::from("Here is where the parser stands.\n");
    for i in 1..=SCROLL_LINES {
        s.push_str(&format!("- part {i} of the parser is written\n"));
    }
    s.push_str(WORK);
    s
}

/// The custom mode's spec: every chain's rules, each reply after [`REPLY_DELAY_MS`], and the plain
/// work reply for a turn no rule picks.
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

/// [`spec`] for a run on harness `h`, `dirs` its chains' repositories (in `chains`' order), each
/// tool call that harness's read of the chain's own `README.md` ([`read_call`]): F3's first turn
/// is answered with one, its words after it that harness's ([`tool_after`]), and land-listed's and land-reworded's work on their quoted lists runs
/// one before it says what it did, as an agent's work does (a long answer with no tool row, where
/// the reader reads them, is no work to the policy, so the list would stand); so does
/// land-contradicted-list's first defense, so it is no no-work answer the same reply could hold.
#[must_use]
pub fn spec_on(chains: &[Chain], h: Harness, dirs: &[PathBuf]) -> J {
    let mut sp = spec(chains);
    let calls: Vec<(String, &str, J)> = chains
        .iter()
        .zip(dirs)
        .filter_map(|(c, d)| {
            let last = match c {
                Chain::ToolEnd => "",
                Chain::LandListed => LISTED_KEY,
                Chain::LandReworded => OPEN_KEY,
                Chain::LandContradictedList => LAND_KEY,
                _ => return None,
            };
            Some((c.token(), last, read_call(h, &d.join("README.md"))))
        })
        .collect();
    if calls.is_empty() {
        return sp;
    }
    if let Some(J::Arr(rules)) = sp.get("replies").cloned() {
        let rules = rules
            .into_iter()
            .map(|mut r| {
                let when = r.get("when").and_then(J::as_str).map(str::to_string);
                let last = r.get("last").and_then(J::as_str).map(str::to_string);
                let ours = calls.iter().find(|(token, l, _)| {
                    when.as_deref() == Some(token.as_str()) && last.as_deref() == Some(*l)
                });
                if let Some((token, _, call)) = ours {
                    r.set("tool", call.clone());
                    if *token == Chain::ToolEnd.token() {
                        r.set("text", J::s(tool_after(h)));
                    }
                }
                r
            })
            .collect();
        sp.set("replies", J::Arr(rules));
    }
    sp
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
    pub const fn word(&self) -> &'static str {
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
    pub box_seq: Option<u64>,
    pub rule: String,
    pub decision: String,
    pub command: String,
    pub reason: String,
}

impl Row {
    /// The row records something done to the agent: a key pressed or a text typed (`approved`,
    /// `declined`, `typed`), or one tried (`escalated`, `deferred`); any decision but `waiting` or
    /// `skipped`.
    #[must_use]
    pub fn acted(&self) -> bool {
        !matches!(self.decision.as_str(), "waiting" | "skipped")
    }
}

/// What the stub's request dumps said of a conversation ([`StubSeen::of`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StubSeen {
    /// The harness-marked user texts the conversation held at the landing (Off: none).
    pub baseline: usize,
    /// Its requests read (after the landing; Off: all of them).
    pub read: usize,
    /// Of those, the ones holding more harness-marked user texts than [`Self::baseline`]: a new
    /// turn of the harness's.
    pub new_turns: usize,
}

/// What one tab's records said, each with its time (ms), in order: the journal's lines, the
/// ledger's rows, and the `status`, `meta` and `task` words at each change, of reads that
/// answered (a failed reply is no reading: `look` records nothing of it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub journal: Vec<(i64, String)>,
    pub ledger: Vec<Row>,
    /// `task=… needs=… autopilot=… supervisor=…` of `status`.
    pub status: Vec<(i64, String)>,
    /// Actual status reads that answered, even where the recorded words did not change.
    pub status_reads: Vec<i64>,
    /// `attention_owners=… attention_kinds=… attention=…` of `meta`, the attention decoded.
    pub meta: Vec<(i64, String)>,
    /// The `task` record's line.
    pub task: Vec<(i64, String)>,
    /// When the reply the chain watches from showed (Off: its done reply).
    pub reply_ms: Option<i64>,
    /// When a person's prompt mid-chain was typed (P4P, [`Chain::asks`]).
    pub asked_ms: Option<i64>,
    /// When one was typed that the composer did not take: it never stood there as typed, or stood
    /// there still after Enter twice.
    pub ask_refused_ms: Option<i64>,
    /// When watching stopped.
    pub end_ms: i64,
    /// The stub's witness (L5, L7: after the landing; Off: the whole conversation); `None` where it
    /// was never read.
    pub stub: Option<StubSeen>,
    /// A same-reply chain's hold lifted before its next act ([`surfaces::held::lift_of`]): how
    /// many requests of its conversation the stub read between the hold and the lift, read once
    /// the watch is over ([`lift_witness`]); `None` where it was not lifted.
    pub lift_stub: Option<usize>,
    /// The harness rows `ctl messages tag=harness` printed at each change: a same-reply chain's
    /// resume ask ([`surfaces::held::resume_ask_rows`]) or a login chain's warnings
    /// ([`sysmsg::login::rows`]), and every read of them that answered.
    pub harness: Vec<(i64, String)>,
    pub harness_reads: Vec<i64>,
    /// When same-land's person began typing their turn into its held tab
    /// ([`surfaces::held::person_due`], [`surfaces::held::person_typed`]): their first key is
    /// already their turn (aterm journals `a person is typing`).
    pub stop_person_ms: Option<i64>,
    /// When that turn was sent (its Enter read back); `None` where the composer did not take it.
    pub stop_person_sent_ms: Option<i64>,
    /// What the coverage chains' run did and read beside the records.
    pub cover: Cover,
    /// What the window showed of the tab ([`surfaces`]).
    pub window: surfaces::Window,
    /// The chain's repository's `HEAD`, read once the watch is over, for a chain whose receipt
    /// the lab judges ([`receipt::reads_head`]).
    pub head: Option<String>,
    /// When the lab landed land-contradicted's `wip-notes.txt` after Land's contradiction
    /// ([`land_contradicted_work`]); `None` where it did not.
    pub landed_ms: Option<i64>,
}

/// What a coverage chain's run did to its tab, and read of it, beside the records ([`Seen`]), each
/// with its time (ms).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cover {
    /// L13: when the person's Enter went in under their standing draft.
    pub person_ms: Option<i64>,
    /// L13: `status human_ms=` read once their Enter took the draft: how long ago a person's
    /// gesture reached the session (`-`: none ever).
    pub person_stamp: Option<String>,
    /// L13's controller keys in a windowed lab ([`person_keys`]): native posting, independent
    /// composer effects, and the person counter they must leave unchanged.
    pub keyed: Option<Keyed>,
    /// L13, CAP: when the lab's orchestrator turn went in.
    pub turn_ms: Option<i64>,
    /// The stub's witness of a window no harness turn may fall in: L13's exchange (its first
    /// prompt to the orchestrator's turn), CAP's hour (after the act to the turn after it).
    pub window_stub: Option<StubSeen>,
    /// L15: the manager's sid.
    pub starter: Option<String>,
    /// L15: the worker's spawn reply.
    pub spawn: Option<String>,
    /// L15: when the manager's `await task @<w> since=0` returned, its exit status and what it
    /// printed.
    pub awaited: Option<(i64, i32, String)>,
    /// L15: the worker's `result`, as printed (its standard output).
    pub result: Option<String>,
    /// L15: the agent the lab's spawn named (`agent=<word>`): the harness under test's.
    pub agent: Option<String>,
    /// L15: the `result` reply's header (`OK <n> source=… bytes=<n>`, on its standard error).
    pub result_head: Option<String>,
    /// L15: the agent the worker's spawn started, as `meta worker=` names it once the watch is
    /// over.
    pub worker: Option<String>,
    /// W1: the stub's actual admission that raised its prearmed usage wall, and its reset second
    /// ([`wall_step`]).
    pub wall: Option<(i64, i64)>,
    /// W1: when the lab gave the stub its scripted mode back.
    pub lifted_ms: Option<i64>,
    /// W1: when the program went on by itself past the wall: the first of the conversation's
    /// requests from the wall's lift on carrying its own continuation ([`carries_continuation`]:
    /// Claude Code's, which goes on at the reset with no act of the harness's), read from the
    /// stub's dumps once the watch is over ([`went_on_witness`]).
    pub went_on_ms: Option<i64>,
    /// C4: the user texts of the conversation's requests that carry the owner's continuation
    /// ([`OWNER_KEY`]), each as the agent sent it, read from the stub's dumps once the watch is
    /// over ([`owner_witness`]).
    pub owner_texts: Option<Vec<String>>,
    /// LH: the user texts of the conversation's requests that hold the land check
    /// ([`LAND_CHECK_KEY`]), each as the agent sent it, read from the stub's dumps once the watch is
    /// over ([`check_witness`]).
    pub check_texts: Option<Vec<String>>,
    /// W2: the notice scrolled out of sight ([`hide_step`]).
    pub hidden: Option<Hidden>,
    /// W2: why the notice was not taken off the screen, where it was not.
    pub hide_gave_up: Option<String>,
    /// W4: each time the lab raised the wall again at its reset instead of lifting it
    /// ([`wall_move`]), and the unix second the new wall resets at, in order.
    pub shifted: Vec<(i64, i64)>,
    /// W5: the switch to the second agent ([`switch_step`]).
    pub switch: Option<Switch>,
    /// W5: why the switch was not made, where it was not.
    pub switch_gave_up: Option<String>,
    /// W6: the conversation made unread, and read again ([`unread_step`]).
    pub unread: Option<Unread>,
    /// W6: why the conversation was not made unread or read again.
    pub unread_gave_up: Option<String>,
    /// W9: the agent ended and launched again in its tab ([`relaunch_step`]): a [`Switch`] to the
    /// same agent, its wall never lifted.
    pub relaunch: Option<Switch>,
    /// W9: why the relaunch was not made, where it was not.
    pub relaunch_gave_up: Option<String>,
    /// W10, W11: what the lab did to the tapped wall ([`tap::step`]).
    pub tap: Option<tap::Driven>,
    /// F18: `status human_ms=` read once the person's mid-chain Enter took their draft
    /// ([`Chain::asks_as_person`]): a stamp where a person's hand reached the session.
    pub ask_stamp: Option<String>,
    /// F1, F2: when the lab resized the tab ([`RESIZED`]), and what the resize and `dims` answered.
    pub resized: Option<(i64, String)>,
    /// F1 by a scroll: the wheel the lab turned over the tab, and what the screen showed
    /// ([`scroll_step`]).
    pub scrolled: Option<Scrolled>,
    /// F3: how many of the conversation's requests carried a tool's result back (the stub's
    /// dumps), read once its watch is over.
    pub tool_results: Option<usize>,
    /// L20, C5: the tool results the conversation carried back (the dialog's answer among them),
    /// read from the stub's dumps once its watch is over ([`dialog_answers_of`]).
    pub dialog_answers: Option<Vec<String>>,
    /// F3: the screen's rows (blank ones dropped) read once the first point after the prompt was
    /// journaled, and when ([`cover::first_turn`] finds the first turn's rows in them).
    pub first_screen: Option<(i64, Vec<String>)>,
    /// ANGLES: what of the other session's work ([`Repo::Others`]) no longer stands as laid, read
    /// once the watch is over ([`angles::others_left`]; empty: all of it stands).
    pub others_left: Option<Vec<String>>,
    /// SWITCH: the lab's writes of the tab's Autopilot word mid-run, in order
    /// ([`switch::word_step`]).
    pub words: Vec<switch::Switched>,
    /// SWS: the draft that stood over the switch ([`switch::word_step`]).
    pub draft: Option<switch::Draft>,
    /// SWITCH: the waits at rest the session's tapes show carrying `autopilot=` and woken by
    /// another word, read once the watch is over ([`switch::woke_witness`]; `None`: no tape read).
    pub woke: Option<Vec<switch::Woke>>,
    /// AT ONCE: the texts the session's tapes show the loop writing, each timed from its decision
    /// to its program's first draw, read once the watch is over ([`switch::typed_witness`];
    /// `None`: no tape read): [`switch::land_at_once`]'s clock.
    pub typed: Option<Vec<show::Typed>>,
    /// O1, O2: when the lab raised the stub's outage ([`outage::step`]).
    pub outage_ms: Option<i64>,
    /// O1, O2: when the lab gave the stub its scripted mode back.
    pub outage_lifted_ms: Option<i64>,
    /// O1, O2: the requests of the conversation the stub read after the lift whose last user text
    /// holds the outage's line, read once the watch is over ([`outage::resumed_witness`]; `None`:
    /// not read, or never lifted).
    pub outage_resumed: Option<usize>,
    /// `--mode sysmsg`: the instance's wakes while the stand-in stood, `metrics` reset at its
    /// raise and read at its clearing ([`sysmsg::WAKE_FIELDS`]).
    pub wall_wakes: Option<String>,
    /// `--mode sysmsg`: the `metrics reset` at the raise failed, so [`Self::wall_wakes`] would
    /// count wakes from before the wall: the figure is unobserved.
    pub wall_wakes_unreset: bool,
    /// `--mode sysmsg`'s witness lab: when the lab prompted its second tab into the standing wall
    /// ([`sysmsg::mate_step`]).
    pub mate_ms: Option<i64>,
    /// `--mode sysmsg`'s expired login: the lab's play of the person ([`sysmsg::login`]'s `step`).
    pub login: Option<sysmsg::login::Played>,
}

impl Cover {
    /// The unix second the stub's wall resets at now: the last raise again's ([`Cover::shifted`]),
    /// else the first wall's.
    #[must_use]
    pub fn reset_now(&self) -> Option<i64> {
        let (_, first) = self.wall?;
        Some(self.shifted.last().map_or(first, |(_, r)| *r))
    }
}

/// W5's switch ([`switch_step`]): the first agent ended in the tab, the stub's wall lifted (the
/// limit is that agent's account's), the second agent launched there and its first prompt typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Switch {
    /// The second agent's harness word ([`Harness::name`]).
    pub to: &'static str,
    /// When the first agent's exit went in (its last key).
    pub exited_ms: i64,
    /// When the shell stood in the tab again.
    pub shell_ms: i64,
    /// When the lab lifted the stub's wall (W5's; `None` at W9's relaunch, whose wall stands).
    pub lifted_ms: Option<i64>,
    /// When the second agent's launcher was entered.
    pub launched_ms: i64,
    /// When its first prompt went in.
    pub prompt_ms: i64,
}

/// W6's unread conversation ([`unread_step`]): Claude Code's transcript of the chain's
/// conversation moved aside, so no read names the conversation the limit stopped, then moved back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unread {
    /// The transcript.
    pub file: PathBuf,
    /// When it was moved aside ([`aside_of`]).
    pub moved_ms: i64,
    /// When it was moved back.
    pub restored_ms: Option<i64>,
}

/// F1 by a scroll, as driven ([`scroll_step`]): the wheel turned up over `tall-scroll`'s tab, a
/// row a notch, until its opener was in sight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scrolled {
    /// When the first notch was turned: F1's law holds from there ([`crate::forever::cover`]'s
    /// `judge_scrolled`), the harness's reads during the notches (the screen the opener first
    /// came into sight on among them) included.
    pub from: i64,
    /// When the last notch was turned.
    pub at: i64,
    /// The notches turned (`mouse wheelup … lines=1`).
    pub notches: u32,
    /// What `mouse` answered where it did not say `OK`.
    pub refused: Option<String>,
    /// The tab's `modes` before the first notch (`alt_screen=`, `mouse_mode=`): whether the agent
    /// takes the wheel (it scrolls its own view) or the tab's viewport moves.
    pub modes: String,
    /// The tab's history above its screen before the first notch, as `lines` answered (`OK <rows>
    /// …`; `unread (…)` where that read failed): the rows a viewport can scroll back.
    pub history: String,
    /// Whether the opener (the chain's token) was on the screen before the first notch.
    pub before: bool,
    /// Whether it was in sight after the last notch: on the live screen where the agent took the
    /// wheel, in the tab's viewport where it did not.
    pub after: bool,
    /// Where the agent takes no wheel (`mouse_mode=none`): the tab's viewport after the last
    /// notch, as `scroll` answered it (`OK <offset> <max>`), with where the opener and the live
    /// screen were (`search`), and the live screen's read after it where that failed.
    pub viewport: Option<String>,
    /// Whether the live screen (the one the harness reads) changed under the wheel at all, by
    /// the last read of it that answered: `false` where none after the wheel did (where the
    /// agent takes no wheel, [`Self::viewport`] names that read's failure).
    pub moved: bool,
    /// The screen's rows after the last notch, blank rows left out: the last read that answered.
    pub rows: Vec<String>,
    /// Whether the opener was still on the screen [`SCROLL_HELD_MS`] after the last notch; `None`
    /// before that read.
    pub held: Option<bool>,
}

/// L13's controller prompt typed through `hwkey` (`forever --window`, [`person_keys`]): native
/// `NSEvent` posting and independent composer observations, without claiming a person's input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyed {
    /// The presses `hwkey` handed to the window's event queue (`OK posted=<n>`): the text's
    /// characters and each Enter.
    pub posted: u64,
    /// How many were posted to the key window of the active app (`key=1`).
    pub key_window: u64,
    /// The exact prompt was independently read in the composer's draft after the text keys.
    pub draft_stood: bool,
    /// A readable composer no longer held that draft after the single Enter.
    pub draft_taken: bool,
    /// `status human_seq=` before the first key.
    pub seq_before: u64,
    /// `status human_seq=` once the Enter took the draft (`None`: unread).
    pub seq_after: Option<u64>,
}

/// W2's hide ([`hide_step`]): the notice out of sight while the wait stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hidden {
    /// When the first hiding command's Enter went in: the screen before it was the notice's own.
    pub first_ms: i64,
    /// When the Enter of the command that scrolled the notice away went in (the screen before it
    /// still drew the notice).
    pub enter_ms: i64,
    /// When the lab read the screen without it.
    pub seen_ms: i64,
    /// That screen's content seq (`status seq=`, the journal's `seq=`): a point read at it or after
    /// is a screen without the notice.
    pub seq: Option<u64>,
    /// The hiding commands entered ([`Hide::command`]).
    pub commands: usize,
    /// That command ([`Hide::command`]): Muse Code's [`HIDE_COMMAND`], agy's
    /// [`AGY_HIDE_COMMAND`], Claude Code's [`CLAUDE_HIDE_COMMAND`].
    pub command: &'static str,
    /// The notice's words ([`Hide::words`]).
    pub words: &'static [&'static str],
}

fn act_rule(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("CONTINUED ")
        .or_else(|| line.strip_prefix("TYPED "))?;
    rest.split_whitespace()
        .find_map(|w| w.strip_prefix("rule="))
}

fn idle_words(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("EVENT idle ")?;
    let (seq, words) = rest.split_once(' ').unwrap_or((rest, ""));
    seq.starts_with("seq=").then_some(words.trim())
}

#[derive(Clone, Copy, Debug, Default)]
struct Yield<'a> {
    event: Option<&'a (i64, String)>,
    waiting: Option<&'a (i64, String)>,
    /// The same-reply hold decided for that point in place of a `WAITING` (its `EXCEPTION …
    /// kind=stalled`, under the event's `seq=`): aterm types nothing more at it.
    held: Option<&'a (i64, String)>,
}

impl Seen {
    fn acts(&self, rule: &str) -> Vec<&(i64, String)> {
        self.journal
            .iter()
            .filter(|(_, l)| act_rule(l) == Some(rule))
            .collect()
    }

    fn all_acts(&self) -> Vec<&(i64, String)> {
        self.journal
            .iter()
            .filter(|(_, l)| act_rule(l).is_some())
            .collect()
    }

    fn first_after(&self, head: &str, t: i64) -> Option<&(i64, String)> {
        self.journal
            .iter()
            .find(|(at, l)| *at > t && l.starts_with(head))
    }

    fn typed_row(&self, rule: &str, t: i64) -> Option<&Row> {
        self.ledger
            .iter()
            .filter(|r| r.rule == rule && r.decision == "typed" && r.ts >= t - 2000)
            .min_by_key(|r| r.ts)
    }

    fn act_text<'a>(&'a self, rule: &str, at: &'a (i64, String)) -> &'a str {
        self.typed_row(rule, at.0)
            .map_or(at.1.as_str(), |r| r.command.as_str())
    }

    fn last_status(&self, key: &str) -> Option<String> {
        self.status.last().and_then(|(_, s)| word(s, key))
    }

    fn status_said(&self, key: &str, value: &str, after: i64) -> bool {
        self.status
            .iter()
            .any(|(t, s)| *t >= after && word(s, key).as_deref() == Some(value))
    }

    fn yield_after(&self, t: i64, until: Option<i64>, holding: Option<&str>) -> Yield<'_> {
        let before = |at: i64| until.is_none_or(|u| at < u);
        let event = self.journal.iter().find(|(at, l)| {
            *at > t
                && before(*at)
                && idle_words(l).is_some_and(|w| {
                    !w.is_empty() && w != "-" && holding.is_none_or(|h| w.contains(h))
                })
        });
        let decided = |what: fn(&str) -> bool| {
            event.and_then(|(et, el)| {
                let seq = word(el, "seq");
                self.journal
                    .iter()
                    .find(|(at, l)| *at >= *et && before(*at) && what(l) && word(l, "seq") == seq)
            })
        };
        Yield {
            event,
            waiting: decided(|l| l.starts_with("WAITING ")),
            held: decided(surfaces::held::is_stall),
        }
    }

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
            && h.bytes().all(|b| b.is_ascii_hexdigit())
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

fn at(t0: i64, t: i64) -> String {
    crate::restore::since(t0, t)
}

/// The short streak a point record (`point@v1`) names, read as the loop's own seed reads it
/// ([`approvals::short_streak_of`]).
fn point_streak(r: &Row) -> Option<u32> {
    approvals::short_streak_of(&r.reason)
}

/// The short streak at the point at `t`: its ledgered point's, else the last back-off's before it
/// and one more for each point read since (at least one: the point at `t`), since an act typed
/// at once (the land check at its defense's point) journals no back-off of its own.
fn streak_at(seen: &Seen, t: i64) -> Option<u32> {
    let marked = seen
        .ledger
        .iter()
        .rfind(|r| r.rule == RULE_POINT && (r.ts - t).abs() <= AT_DONE_MS)
        .and_then(point_streak);
    marked.or_else(|| {
        let (w, l) = seen
            .journal
            .iter()
            .rfind(|(at, l)| *at < t && l.starts_with("WAITING "))?;
        let (_, rest) = l.split_once(" until=")?;
        let n: u32 = rest.split_whitespace().nth(1)?.parse().ok()?;
        let mut since: Vec<String> = seen
            .journal
            .iter()
            .filter(|(at, l)| {
                at > w
                    && *at <= t
                    && idle_words(l).is_some_and(|said| !said.is_empty() && said != "-")
            })
            .filter_map(|(_, l)| word(l, "seq"))
            .collect();
        since.dedup();
        n.checked_add(u32::try_from(since.len().max(1)).unwrap_or(u32::MAX))
    })
}

/// P3 of `docs/measured/forever-lab-2026-10-20.md`: the point records (`point@v1`) written before
/// the first prompt at `t0`. A point judged before anyone asked the agent anything (a frame drawn
/// before its banner, a blank first screen) counts a short turn the agent never ran and starts
/// every back-off a step late.
#[must_use]
pub fn points_before_prompt(seen: &Seen, t0: i64) -> Vec<&Row> {
    seen.ledger
        .iter()
        .filter(|r| r.rule == RULE_POINT && r.ts < t0)
        .collect()
}

/// The tab's first point and first back-off, as `lab forever` reports them (P3), such as
/// `first point +5.6 s (short turns in a row: 1), first back-off +5.6 s "1 short turn(s) in a row:
/// 2 min back-off", 0 point record(s) before the first prompt`, every point record before the
/// first prompt named with its time (before `t0`: `-…`).
#[must_use]
pub fn first_point_line(seen: &Seen, t0: i64) -> String {
    let streak = |r: &Row| {
        r.reason
            .split_once("(short turns in a row: ")
            .map_or("?", |(_, n)| n.trim_end_matches(')'))
            .to_string()
    };
    let since = |t: i64| {
        if t < t0 {
            format!("-{}", at(t, t0))
        } else {
            format!("+{}", at(t0, t))
        }
    };
    let point = seen
        .ledger
        .iter()
        .find(|r| r.rule == RULE_POINT)
        .map_or_else(
            || "no point".to_string(),
            |r| {
                format!(
                    "first point {} (short turns in a row: {})",
                    since(r.ts),
                    streak(r)
                )
            },
        );
    let back_off = seen
        .journal
        .iter()
        .find(|(_, l)| l.starts_with("WAITING ") && l.contains("short turn(s) in a row"))
        .map_or_else(
            || "no back-off".to_string(),
            |(t, l)| {
                let words = l.split_once("Z ").map_or(l.as_str(), |(_, w)| w);
                format!("first back-off {} \"{words}\"", since(*t))
            },
        );
    let before = points_before_prompt(seen, t0);
    let named: Vec<String> = before
        .iter()
        .map(|r| format!("{} ({})", since(r.ts), streak(r)))
        .collect();
    format!(
        "{point}, {back_off}, {} point record(s) before the first prompt{}",
        before.len(),
        if named.is_empty() {
            String::new()
        } else {
            format!(": {}", named.join(", "))
        }
    )
}

/// The ladder's next step after a point that closed `streak` short turns: the wait before the next
/// act, were the ladder still going (none for a worker its manager waits on, through its first
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

/// How long the landing at `t` is watched for after it (L5, L7): through the ladder's next step and
/// [`PAST_NEXT_STEP_MS`] more, at least [`QUIET_AFTER_LANDED_MIN_MS`].
#[must_use]
pub fn quiet_after_landed_ms(seen: &Seen, t: i64, managed: bool) -> i64 {
    let step = next_step_ms(streak_at(seen, t).unwrap_or(1), managed);
    step.saturating_add(PAST_NEXT_STEP_MS)
        .max(QUIET_AFTER_LANDED_MIN_MS)
}

/// The judge of one chain's records `seen` (`program`: the reader's word the needs entry names),
/// `t0` the first prompt's time: each of its scenarios' verdict, in order. Every one is a `FAIL`
/// where the chain's records hold a fault of their own ([`chain_faults`]: an act before its due,
/// an act typed twice, a point before the first prompt; [`surfaces::held::stall_fault`]: a hold in
/// a chain whose script makes none), its own judge's words kept. A same-reply chain's last
/// scenario also carries the hold's wants ([`surfaces::held::with_held`]).
#[must_use]
pub fn judge(chain: Chain, seen: &Seen, program: &str, t0: i64) -> Vec<(Scenario, Verdict)> {
    let mut faults = chain_faults(seen, t0, chain.first_reply_works());
    faults.extend(surfaces::held::stall_fault(chain, seen, t0));
    let verdicts = judge_scenarios(chain, seen, program, t0);
    if faults.is_empty() {
        return verdicts;
    }
    let faults = faults.join("; ");
    verdicts
        .into_iter()
        .map(|(s, v)| {
            (
                s,
                Verdict::Fail(format!(
                    "THE CHAIN'S RECORDS: {faults} (its own judge: {} {})",
                    v.word(),
                    v.why()
                )),
            )
        })
        .collect()
}

fn judge_scenarios(chain: Chain, seen: &Seen, program: &str, t0: i64) -> Vec<(Scenario, Verdict)> {
    let done = seen.first_after("DONE ", t0);
    let land = seen.acts(turn_end::RULE_LAND).first().copied();
    let nudges = seen.acts(turn_end::RULE_LAND_NUDGE);
    let land_nudge_after = |t: i64| nudges.iter().copied().find(|(at, _)| *at > t);
    let checks = seen.acts(turn_end::RULE_LAND_CHECK);
    let land_check_after = |t: i64| checks.iter().copied().find(|(at, _)| *at > t);
    let mut out = Vec::new();
    for &s in chain.scenarios() {
        let v = match s {
            Scenario::L1 => judge_land_act(seen, land, done, t0),
            Scenario::L2 => judge_listed(
                seen,
                land,
                &land_nudge_after,
                done,
                (Some(OPEN_KEY), &Want::listed(&[OPEN_KEY])),
                t0,
            ),
            Scenario::LS => judge_listed(
                seen,
                land,
                &land_nudge_after,
                done,
                (None, &Want::listed(&[SAME_KEY])),
                t0,
            ),
            Scenario::LH => judge_hinted(seen, land, t0),
            Scenario::LL => judge_relisted(seen, land, t0),
            Scenario::LR => judge_reworded(seen, land, t0),
            Scenario::LX => judge_contradicted(seen, land, &land_nudge_after, t0),
            Scenario::LY => judge_contradicted_list(seen, land, &land_nudge_after, done, t0),
            Scenario::L4 | Scenario::L8 | Scenario::L9 | Scenario::L10 | Scenario::L11 => {
                judge_nudge(
                    seen,
                    land,
                    &land_nudge_after,
                    done,
                    &Want::repo(chain.repo()),
                    t0,
                )
            }
            Scenario::L3 => judge_needs(seen, land, &land_check_after, program, t0),
            Scenario::L5 => judge_landed(seen, land, false, t0),
            Scenario::L6 => judge_rest(seen, land, done, t0),
            Scenario::L7 => judge_tasked(seen, land, t0),
            Scenario::C1 => judge_check(seen, t0),
            Scenario::C2 => judge_cruise_goes_on(seen, done, t0),
            Scenario::Off => judge_off(seen, t0),
            Scenario::P4L => judge_instant_land(seen, land, done, t0),
            Scenario::P4C => judge_instant_cruise(seen, done, t0),
            Scenario::P4P => judge_instant_ask(seen, t0),
            Scenario::L12 => cover::judge_answer(seen, t0),
            Scenario::L13 => cover::judge_theirs(seen, t0),
            Scenario::L14 => cover::judge_wrote_land(seen, land, &land_check_after, program, t0),
            Scenario::C3 => cover::judge_wrote_cruise(seen, done, t0),
            Scenario::C4 => cover::judge_owner_continuation(seen, done, t0),
            Scenario::P5 => cover::judge_startup(seen, t0),
            Scenario::L15 => cover::judge_spawned(seen, land, program, t0),
            Scenario::Cap => cover::judge_cap(seen, t0),
            Scenario::W1 => cover::judge_wall(seen, land, program, t0),
            Scenario::L16 | Scenario::L17 => judge_long_landing(seen, land, t0),
            Scenario::L18 | Scenario::L19 => checked::judge_landing(seen, land, t0),
            Scenario::N1 => surfaces::judge_needs_step(seen, t0, 0),
            Scenario::N2 => surfaces::judge_needs_step(seen, t0, 1),
            Scenario::N3 => surfaces::judge_needs_step(seen, t0, 2),
            Scenario::N4 => surfaces::judge_needs_step(seen, t0, 3),
            Scenario::End => continue,
            Scenario::W2 => cover::judge_wall_hidden(seen, land, program, t0),
            Scenario::W4 => walls::judge_wall_redraw(seen, land, program, t0),
            Scenario::W5 => walls::judge_wall_switch(seen, land, program, t0),
            Scenario::W6 => walls::judge_wall_unread(seen, land, program, t0),
            Scenario::W8 => walls::judge_wall_cruise(seen, program, t0),
            Scenario::W9 => walls::judge_wall_relaunch(seen, land, program, t0),
            Scenario::W10 => tap::judge(seen, land, program, t0),
            Scenario::W11 => tap::judge_narrow(seen, land, program, t0),
            Scenario::O1 | Scenario::O2 => outage::judge(chain, seen, outage::SCALE, t0),
            Scenario::Sys(sysmsg::Form::Login) => sysmsg::login::judge(seen, false, program, t0),
            Scenario::SysLand(sysmsg::Form::Login) => sysmsg::login::judge(seen, true, program, t0),
            Scenario::Sys(f) => sysmsg::judge(seen, f, false, program, t0),
            Scenario::SysLand(f) => sysmsg::judge(seen, f, true, program, t0),
            Scenario::SignedIn(land) => sysmsg::login::judge_signed_in(seen, land, program, t0),
            Scenario::Witnessed => sysmsg::judge_witnessed(seen, program, t0),
            Scenario::L20 => cover::judge_dialog(seen, true, t0),
            Scenario::C5 => cover::judge_dialog(seen, false, t0),
            Scenario::F18 => cover::judge_person_instant(seen, t0),
            Scenario::Rewrap => cover::judge_rewrap(seen, t0),
            Scenario::Scrolled => cover::judge_scrolled(seen, t0),
            Scenario::SameWords => cover::judge_same_words(seen, t0),
            Scenario::F3 => cover::judge_tool_end(seen, land, t0),
            Scenario::Ladder => cover::judge_ladder(seen, LADDER_SCALE, t0),
            Scenario::HL => surfaces::held::judge_hold(seen, t0),
            Scenario::HC => paced::judge(seen, paced::SHIPPED, t0),
            Scenario::Angles => angles::judge(seen, t0),
            Scenario::SwitchAgent => switch::judge_at_rest(seen, t0),
            Scenario::SwitchSeq => switch::judge_draft(seen, t0),
            Scenario::CommandDraft => switch::judge_command(seen, t0),
            Scenario::SwitchFlip => switch::judge_flip(seen, t0),
            Scenario::SwitchOverHeld => switch::judge_over_held(seen, t0),
            Scenario::SwitchBusy => switch::judge_busy(seen, t0),
        };
        let v = through_check(chain, seen, t0, v);
        let cx = surfaces::Ctx { chain, program, t0 };
        let v = switch::with_at_once(s, seen, &cx, v);
        let v = surfaces::with_window(s, seen, &cx, v);
        let v = surfaces::held::with_held(s, seen, &cx, v);
        let v = receipt::with_receipt(chain, s, seen, t0, v);
        out.push((s, checked::with_checked(chain, s, seen, t0, v)));
    }
    out
}

fn judge_long_landing(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land act was typed".into());
    };
    let Some((d, line)) = seen.first_after("DONE ", *t1) else {
        return Verdict::Fail(format!(
            "no DONE within {} after the land act at +{}",
            at(t0, seen.end_ms),
            at(t0, *t1)
        ));
    };
    let busy = d - t1;
    let mut bad = Vec::new();
    if !line.contains(&format!("rule={}", turn_end::RULE_LAND)) {
        bad.push(format!("the DONE names another rule: {line}"));
    }
    if !seen.status_said("task", "landed", d - 5000) {
        bad.push("status never said task=landed".into());
    }
    if busy < surfaces::END_WORK_FLOOR_MS {
        bad.push(format!(
            "landed {} after the act, under the {} s floor",
            at(0, busy),
            surfaces::END_WORK_FLOOR_MS / 1000
        ));
    }
    let said = format!(
        "land@v1 at +{}, DONE at +{} ({}), {} after it (its defense held and paced {})",
        at(t0, *t1),
        at(t0, *d),
        crate::util::one_line(line, 90),
        at(0, busy),
        at(0, long_reply_ms(CLEAN))
    );
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

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

struct Want {
    /// What the nudge opens with, `head` right after it; `None` where the head may stand anywhere.
    leads: Option<&'static str>,
    head: &'static str,
    named: &'static [&'static str],
    /// What the nudge says after its head beside `named`.
    says: &'static [&'static str],
    not_named: &'static [&'static str],
    not_anywhere: &'static [&'static str],
}

/// What Land's contradiction never says (slice 4c): any way out of landing the work that would
/// clear git's probe or the reader without landing anything.
pub(crate) const ESCAPES: [&str; 3] = ["ignore", "move it", "not open"];

impl Want {
    /// The nudge quoting the agent's own list, `named` in it: never the reader's words, the
    /// repository's or the needs ack (no need is scripted).
    const fn listed(named: &'static [&'static str]) -> Want {
        Want {
            leads: None,
            head: LISTED_KEY,
            named,
            says: &[],
            not_named: &[],
            not_anywhere: &[OPEN_SUFFIX, REPO_SUFFIX, NEEDS_ACK],
        }
    }

    /// LAND'S CONTRADICTION OF A DEFENSE WHILE THE AGENT'S LIST STANDS (slice 4c's Open leg): its
    /// head, then the list quoted (`named` in it), its one ask and Land's own close; never the
    /// reader's words, the repository's, the needs ack or a way out ([`ESCAPES`]).
    const fn listed_contradicted(named: &'static [&'static str]) -> Want {
        Want {
            leads: Some(turn_end::LAND_CONTRADICTED_HEAD),
            head: turn_end::CONTRADICTED_LISTED,
            named,
            says: &[turn_end::CONTRADICTED_LISTED_ASK, turn_end::NO_REPOSITORY],
            not_named: &[],
            not_anywhere: &[
                OPEN_SUFFIX,
                REPO_SUFFIX,
                NEEDS_ACK,
                ESCAPES[0],
                ESCAPES[1],
                ESCAPES[2],
            ],
        }
    }

    /// LAND'S CONTRADICTION OF A FALSE DEFENSE over the repository's work (slice 4c): its head,
    /// then git's fact naming `repo`'s work and no other leg's, its one ask and Land's own close;
    /// never the repository's suffix of the nudge it replaces, the reader's words, the needs ack
    /// or a way out ([`ESCAPES`]).
    const fn repo(repo: Repo) -> Want {
        let (named, not_named) = repo.named();
        Want {
            leads: Some(turn_end::LAND_CONTRADICTED_HEAD),
            head: turn_end::CONTRADICTED_GIT,
            named,
            says: &[turn_end::CONTRADICTED_GIT_ASK, turn_end::NO_REPOSITORY],
            not_named,
            not_anywhere: &[
                OPEN_SUFFIX,
                REPO_SUFFIX,
                NEEDS_ACK,
                ESCAPES[0],
                ESCAPES[1],
                ESCAPES[2],
            ],
        }
    }
}

/// What of `text`, a nudge, `want` judges: the words from its head on, or why it is not the nudge
/// `want` wants (what it does not say, what it says that it must not, and the text).
fn nudge_words<'t>(text: &'t str, want: &Want) -> Result<&'t str, String> {
    let suffix = text.find(want.head).map_or("", |i| &text[i..]);
    let led = want.leads.is_none_or(|lead| {
        text.strip_prefix(lead)
            .is_some_and(|r| r.trim_start().starts_with(want.head))
    });
    let missing: Vec<&str> = want
        .leads
        .filter(|_| !led)
        .into_iter()
        .chain(
            std::iter::once(want.head)
                .chain(want.named.iter().copied())
                .chain(want.says.iter().copied())
                .filter(|w| !suffix.contains(w)),
        )
        .collect();
    let wrong: Vec<&str> = want
        .not_named
        .iter()
        .copied()
        .filter(|w| suffix.contains(w))
        .chain({
            // A word that must stand nowhere, an escape among them, is matched in any case: "Or
            // ignore it." and "Ignore it." are the same way out.
            let lower = text.to_lowercase();
            want.not_anywhere
                .iter()
                .copied()
                .filter(move |w| lower.contains(&w.to_lowercase()))
        })
        .collect();
    if missing.is_empty() && wrong.is_empty() {
        return Ok(suffix);
    }
    Err(format!(
        "does not say {missing:?}{}: {}",
        if wrong.is_empty() {
            String::new()
        } else {
            format!(" or says {wrong:?}")
        },
        crate::util::one_line(text, 400)
    ))
}

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
        if let Some(lost) = lost_after(seen, *t1, t0) {
            return Verdict::Fail(format!("not landed, but no land-nudge@v1 typed: {lost}"));
        }
        return Verdict::Unobserved(format!(
            "not landed, but no land-nudge@v1 typed within {} (the back-off not waited out?)",
            at(t0, seen.end_ms)
        ));
    };
    let t2 = nudge.0;
    let text = seen.act_text(turn_end::RULE_LAND_NUDGE, nudge);
    let suffix = match nudge_words(text, want) {
        Ok(suffix) => suffix,
        Err(why) => return Verdict::Fail(format!("the nudge at +{} {why}", at(t0, t2))),
    };
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
            ..
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
            held: Some((th, hl)),
        } => Verdict::Pass(format!(
            "{said}; its yield read at +{} ({}), +{} HELD {}",
            at(t0, *te),
            crate::util::one_line(el, 80),
            at(t0, *th),
            crate::util::one_line(hl, 100)
        )),
        Yield {
            event: Some((te, el)),
            waiting: None,
            held: None,
        } => Verdict::Fail(format!(
            "{said}; its yield read at +{} ({}) but no WAITING decided for that point within {}",
            at(t0, *te),
            crate::util::one_line(el, 80),
            at(t0, seen.end_ms)
        )),
        Yield { event: None, .. } => match lost_after(seen, t2, t0) {
            Some(lost) => Verdict::Fail(format!("{said}; the nudge's yield never judged: {lost}")),
            None => Verdict::Unobserved(format!(
                "{said}; the nudge's yield was not read within {}",
                at(t0, seen.end_ms)
            )),
        },
    }
}

fn done_line(done: Option<&(i64, String)>) -> String {
    done.map_or_else(String::new, |(_, l)| crate::util::one_line(l, 120))
}

fn listed(list: &str, item: &str) -> bool {
    list.split([',', ' ']).any(|x| x == item)
}

fn needs_up(m: &str) -> bool {
    word(m, "attention_kinds").is_some_and(|k| listed(&k, "ask:needs"))
}

fn needs_entry(seen: &Seen, t: i64) -> Option<&(i64, String)> {
    seen.meta.iter().find(|(at, m)| *at >= t && needs_up(m))
}

fn attention_text(m: &str) -> &str {
    m.split_once("attention=").map_or("", |(_, t)| t)
}

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
    let until = seen.first_after("DONE ", *t1).map_or(i64::MAX, |(d, _)| *d);
    let ask = entry.and_then(|(te, _)| {
        seen.status.iter().find(|(t, s)| {
            *t >= *te - 5000 && *t <= until && word(s, "needs").as_deref() == Some("ask")
        })
    });
    // The act that carries the needs ack: the land check where the defense owes one (L3), its
    // text read off its own ledger row.
    let act = nudge
        .and_then(|(_, l)| act_rule(l))
        .unwrap_or(turn_end::RULE_LAND_NUDGE);
    let nudge_text = nudge.map(|n| seen.act_text(act, n));
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
            || "NO land check typed".to_string(),
            |(t, _)| {
                format!(
                    "{act} typed at +{}{}",
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
        facts.push(format!("LANDED before the {act}"));
    }
    if !stray.is_empty() {
        facts.push(format!("the {act} says {stray:?}"));
    }
    let said = facts.join("; ");
    let raised = needs.is_some() && entry.is_some();
    if raised && text_ok && ask.is_some() && carry_on && stray.is_empty() && !landed_first {
        Verdict::Pass(said)
    } else if raised && nudge.is_none() && !landed_first {
        match lost_after(seen, *t1, t0) {
            Some(lost) => Verdict::Fail(format!("{said}: the need raised, then {lost}")),
            None => Verdict::Unobserved(format!("{said} (the carry-on not waited out)")),
        }
    } else if entry.is_some() && !text_ok {
        Verdict::Fail(format!(
            "{said}; the entry does not read \"{want_text} … {CARRYING_ON}\""
        ))
    } else {
        Verdict::Fail(said)
    }
}

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
    let own_point = rows_at
        .iter()
        .position(|r| r.decision == "waiting" && r.rule == RULE_POINT)
        .map_or(0, |i| i + 1);
    let stray_rows: Vec<String> = rows_at[own_point..]
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
    let gaps = rev_gaps(seen, t0);
    if !gaps.is_empty() {
        bad.push(format!(
            "the record changed between looks unread: {}",
            gaps.join(", ")
        ));
    }
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

/// The land checks typed after `t` (`land-check@v1`), in order.
fn checks_after(seen: &Seen, t: i64) -> Vec<&(i64, String)> {
    seen.acts(turn_end::RULE_LAND_CHECK)
        .into_iter()
        .filter(|(at, _)| *at > t)
        .collect()
}

/// L6's rest: the loop's one `WAITING … rule=land-check@v1 … waits only on you` line, said once
/// the check's answer named only a need.
fn rest_of(seen: &Seen) -> Option<&(i64, String)> {
    seen.journal.iter().find(|(_, l)| {
        l.starts_with("WAITING ")
            && word(l, "rule").as_deref() == Some(turn_end::RULE_LAND_CHECK)
            && l.contains(aterm_agent::supervise::policy::land_check::WAITS_ON_YOU)
    })
}

/// The first land check after `t1`, or the verdict for none: a landing without it fails, a lost
/// loop fails, a watch too short is unobserved.
fn the_check(seen: &Seen, t1: i64, t0: i64) -> Result<&(i64, String), Verdict> {
    if let Some(c) = checks_after(seen, t1).first().copied() {
        return Ok(c);
    }
    if let Some((d, l)) = seen.first_after("DONE ", t1) {
        return Err(Verdict::Fail(format!(
            "LANDED at +{} with no land check typed: {}",
            at(t0, *d),
            crate::util::one_line(l, 120)
        )));
    }
    if let Some((t, l)) = seen
        .acts(turn_end::RULE_LAND_NUDGE)
        .into_iter()
        .find(|(t, _)| *t > t1)
    {
        return Err(Verdict::Fail(format!(
            "Land's nudge typed at +{} with no land check before it: {}",
            at(t0, *t),
            crate::util::one_line(l, 100)
        )));
    }
    if let Some(lost) = lost_after(seen, t1, t0) {
        return Err(Verdict::Fail(format!("no land check typed: {lost}")));
    }
    Err(Verdict::Unobserved(format!(
        "no land-check@v1 typed within {}",
        at(t0, seen.end_ms)
    )))
}

/// What a check's text must and must not say: its head, hinted quoting `hint` or plain.
fn check_text_faults(text: &str, hint: Option<&str>) -> Vec<String> {
    let mut bad = Vec::new();
    if !text.starts_with(turn_end::LAND_CHECK_HEAD) {
        bad.push(format!(
            "the check does not open with \"{}\": {}",
            turn_end::LAND_CHECK_HEAD,
            crate::util::one_line(text, 120)
        ));
    }
    for w in [OPEN_SUFFIX, REPO_SUFFIX] {
        if text.contains(w) {
            bad.push(format!("the check says \"{w}\""));
        }
    }
    match hint {
        Some(h) if !(text.contains(HINT_OPEN) && text.contains(h)) => bad.push(format!(
            "the check is not hinted with \"{h}\": {}",
            crate::util::one_line(text, 300)
        )),
        None if text.contains(HINT_OPEN) => bad.push(format!(
            "the check is hinted, not plain: {}",
            crate::util::one_line(text, 300)
        )),
        _ => {}
    }
    bad
}

/// A clean defense's landing (L20's, whose script raises no need): Land's instruction typed, one
/// land check after its yield, `LANDED` landing the task (`DONE … rule=land@v1`, `status
/// task=landed`), and nothing typed after it ([`landing_quiet`]).
fn judge_clean_landing(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed".into());
    };
    let check = match the_check(seen, *t1, t0) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let t2 = check.0;
    let Some(done) = seen.first_after("DONE ", t2) else {
        return Verdict::Fail(format!(
            "no DONE within {} after the land check's LANDED",
            at(t0, seen.end_ms)
        ));
    };
    let (mut bad, said, _) = landing_quiet(seen, done, false, t0);
    let checks = checks_after(seen, *t1).len();
    if checks != 1 {
        bad.push(format!("{checks} land checks typed, not one"));
    }
    let said = format!("land-check@v1 typed at +{}; {said}", at(t0, t2));
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// L5 (land-a): the defense at the land act's yield gets one land check at that point (no
/// back-off), its `LANDED` lands, and nothing more is typed. The defense names a need, so the
/// check carries the needs ack (L3); `LANDED` retracts no need (the post-landing review,
/// 2026-11-01), so the need stands quietly beside the landing: no `CLEARED … needs` line after
/// the check, and the needs entry still up at the last read.
fn judge_landed(seen: &Seen, land: Option<&(i64, String)>, managed: bool, t0: i64) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed (L1 failed)".into());
    };
    let check = match the_check(seen, *t1, t0) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let t2 = check.0;
    let Some(done) = seen.first_after("DONE ", t2) else {
        return Verdict::Fail(format!(
            "no DONE within {} after the land check's LANDED",
            at(t0, seen.end_ms)
        ));
    };
    let (mut bad, said, window) = landing_quiet(seen, done, managed, t0);
    bad.extend(check_text_faults(
        seen.act_text(turn_end::RULE_LAND_CHECK, check),
        None,
    ));
    let checks = checks_after(seen, *t1).len();
    if checks != 1 {
        bad.push(format!("{checks} land checks typed, not one"));
    }
    if let Some((tn, _)) = seen
        .acts(turn_end::RULE_LAND_NUDGE)
        .into_iter()
        .find(|(t, _)| *t > *t1 && *t < done.0)
    {
        bad.push(format!(
            "a land-nudge@v1 typed at +{} before the landing",
            at(t0, *tn)
        ));
    }
    let defense = seen.yield_after(*t1, Some(t2), None).event;
    let backed_off = seen
        .journal
        .iter()
        .find(|(t, l)| *t > *t1 && *t < t2 && l.starts_with("WAITING ") && l.contains(" until="));
    match (defense, backed_off) {
        (_, Some((tw, l))) => bad.push(format!(
            "a back-off decided before the check, at +{}: {}",
            at(t0, *tw),
            crate::util::one_line(l, 120)
        )),
        (None, None) => bad.push("the defense's yield was not read before the check".into()),
        (Some((te, _)), None) if t2 - te >= next_step_ms(1, false) => bad.push(format!(
            "the check typed {} after the defense's yield, a back-off's wait",
            at(0, t2 - te)
        )),
        _ => {}
    }
    let invented = bad.len();
    let unread = cover::invented_record(seen, &mut bad);
    let invented = bad.len() > invented;
    let cleared = seen
        .journal
        .iter()
        .find(|(t, l)| *t >= t2 && l.starts_with("CLEARED ") && l.contains(" needs "));
    let entry_up = seen.meta.last().is_some_and(|(_, m)| needs_up(m));
    if let Some((tc, l)) = cleared {
        bad.push(format!(
            "the need was taken down at +{} though nothing retracted it: {}",
            at(t0, *tc),
            crate::util::one_line(l, 120)
        ));
    }
    if !entry_up {
        bad.push("the needs entry is gone from meta at the last read".into());
    }
    let said = format!(
        "land-check@v1 typed at +{}{}; {said}; needs {}; no task given, the record {}",
        at(t0, t2),
        defense.map_or_else(String::new, |(te, _)| format!(
            ", {} after the defense's yield",
            at(0, t2 - te)
        )),
        if cleared.is_none() && entry_up {
            "standing beside the landing"
        } else {
            "TAKEN DOWN"
        },
        if unread.is_some() {
            "not read"
        } else if invented {
            "INVENTED"
        } else {
            "OK task - throughout"
        }
    );
    match unread {
        Some(why) if bad.is_empty() => Verdict::Unobserved(format!("{said}: {why}")),
        _ => landed_verdict(bad, said, seen, done.0, window),
    }
}

/// L2 (land-b) and LS (land-same-list): the defense's one check (hinted with `hint` where the
/// reader reads a clause of it open, else plain), answered with the agent's own list; Land's
/// nudge quotes that list (`named` in it), never the reader's words or the repository's; the same
/// defense again is held ([`surfaces::held`]), never checked again; never landed.
fn judge_listed<'a>(
    seen: &'a Seen,
    land: Option<&(i64, String)>,
    nudge_after: &dyn Fn(i64) -> Option<&'a (i64, String)>,
    done: Option<&(i64, String)>,
    (hint, want): (Option<&str>, &Want),
    t0: i64,
) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed".into());
    };
    let check = match the_check(seen, *t1, t0) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let mut bad = check_text_faults(seen.act_text(turn_end::RULE_LAND_CHECK, check), hint);
    let checks = checks_after(seen, *t1).len();
    if checks != 1 {
        bad.push(format!("{checks} land checks typed, not one"));
    }
    let v = judge_nudge(seen, Some(check), nudge_after, done, want, t0);
    let said = format!(
        "land-check@v1 typed at +{} ({}), {checks} check(s) in all",
        at(t0, check.0),
        if hint.is_some() { "hinted" } else { "plain" }
    );
    match v {
        Verdict::Pass(why) if bad.is_empty() => Verdict::Pass(format!("{said}; {why}")),
        Verdict::Pass(why) => Verdict::Fail(format!("{said}; {why}: {}", bad.join("; "))),
        Verdict::Fail(why) => Verdict::Fail(format!("{said}; {why}{}", tail_of(&bad))),
        Verdict::Unobserved(why) if bad.is_empty() => Verdict::Unobserved(format!("{said}; {why}")),
        Verdict::Unobserved(why) => Verdict::Fail(format!("{said}; {why}: {}", bad.join("; "))),
    }
}

fn tail_of(bad: &[String]) -> String {
    if bad.is_empty() {
        String::new()
    } else {
        format!("; {}", bad.join("; "))
    }
}

/// L6 (land-d): the tall reply, its need above the window, gets the plain check (a need or a cut
/// does not bar it); the check's answer names only the need, so Land rests: the rest said once,
/// nothing typed past the ladder's next step, `land_held=needs`, no hold's `EXCEPTION`, never
/// landed.
fn judge_rest(
    seen: &Seen,
    land: Option<&(i64, String)>,
    done: Option<&(i64, String)>,
    t0: i64,
) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed".into());
    };
    if let Some((d, l)) = done {
        return Verdict::Fail(format!(
            "LANDED at +{} over the need ({})",
            at(t0, *d),
            crate::util::one_line(l, 90)
        ));
    }
    if seen.status_said("task", "landed", t0) {
        return Verdict::Fail("status said task=landed".into());
    }
    let check = match the_check(seen, *t1, t0) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let t2 = check.0;
    let mut bad = check_text_faults(seen.act_text(turn_end::RULE_LAND_CHECK, check), None);
    let needs = seen.first_after("NEEDS ", *t1);
    let entry = needs_entry(seen, *t1);
    if needs.is_none() || entry.is_none() {
        bad.push("the need was not raised".into());
    }
    let typed: Vec<&(i64, String)> = seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| *t > t2)
        .collect();
    if let Some((t, l)) = typed.first() {
        bad.push(format!(
            "TYPED into the rest at +{}: {}",
            at(t0, *t),
            crate::util::one_line(l, 120)
        ));
    }
    if let Some((t, l)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > *t1 && l.starts_with("EXCEPTION "))
    {
        bad.push(format!(
            "an EXCEPTION at +{}: {}",
            at(t0, *t),
            crate::util::one_line(l, 120)
        ));
    }
    let Some((tr, rl)) = rest_of(seen).filter(|(t, _)| *t > t2) else {
        let why = format!(
            "land-check@v1 typed at +{}, but no rest said (WAITING … rule={} … {})",
            at(t0, t2),
            turn_end::RULE_LAND_CHECK,
            aterm_agent::supervise::policy::land_check::WAITS_ON_YOU
        );
        return match lost_after(seen, t2, t0) {
            Some(lost) => Verdict::Fail(format!("{why}: {lost}{}", tail_of(&bad))),
            None if bad.is_empty() => Verdict::Unobserved(why),
            None => Verdict::Fail(format!("{why}{}", tail_of(&bad))),
        };
    };
    // The status words are kept at each change, and the need can reach them before the rest is
    // journaled: the words standing at the rest count, as does any later change.
    let standing = seen
        .status
        .iter()
        .rposition(|(t, _)| *t <= *tr)
        .unwrap_or(0);
    let held_needs = seen
        .status
        .iter()
        .skip(standing)
        .any(|(_, s)| word(s, "land_held").is_some_and(|h| h.split(',').any(|l| l == "needs")));
    if !held_needs {
        bad.push("status never read land_held=needs after the rest".into());
    }
    let window = quiet_after_landed_ms(seen, *tr, false);
    let said = format!(
        "land-check@v1 (plain) typed at +{}; +{} {}; {}; nothing typed for {} after it (the window {})",
        at(t0, t2),
        at(t0, *tr),
        crate::util::one_line(rl, 120),
        needs.map_or_else(
            || "NO NEEDS line".to_string(),
            |(t, l)| format!("+{} {}", at(t0, *t), crate::util::one_line(l, 80))
        ),
        at(0, seen.end_ms - tr),
        at(0, window)
    );
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if seen.end_ms - tr < window {
        return Verdict::Unobserved(format!("{said}: watched under the window after the rest"));
    }
    Verdict::Pass(format!("not landed; {said}"))
}

/// LH (land-hinted): the clean defense whose clause the reader reads open gets one hinted check
/// quoting it, its `LANDED` lands with no nudge typed, and the composer held the hinted text as
/// typed (the stub's own copy of the check holds the quote's glyphs).
fn judge_hinted(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed".into());
    };
    let check = match the_check(seen, *t1, t0) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let t2 = check.0;
    let Some(done) = seen.first_after("DONE ", t2) else {
        return Verdict::Fail(format!(
            "no DONE within {} after the hinted check's LANDED",
            at(t0, seen.end_ms)
        ));
    };
    let (mut bad, said, window) = landing_quiet(seen, done, false, t0);
    let text = seen.act_text(turn_end::RULE_LAND_CHECK, check);
    bad.extend(check_text_faults(text, Some(HINT_KEY)));
    let checks = checks_after(seen, *t1).len();
    if checks != 1 {
        bad.push(format!("{checks} land checks typed, not one"));
    }
    if let Some((tn, _)) = seen.acts(turn_end::RULE_LAND_NUDGE).first() {
        bad.push(format!("a land-nudge@v1 typed at +{}", at(t0, *tn)));
    }
    let held = match &seen.cover.check_texts {
        None => "the stub's copy UNREAD".to_string(),
        Some(texts) => {
            let close = aterm_agent::supervise::policy::land_check::LAND_CHECK_HINT_CLOSE;
            if texts
                .iter()
                .any(|t| t.contains(HINT_OPEN) && t.contains(HINT_KEY) && t.contains(close))
            {
                format!(
                    "the stub's copy holds the quote as typed ({} text(s))",
                    texts.len()
                )
            } else {
                bad.push(format!(
                    "no copy of the check the stub read holds the quote as typed: {:?}",
                    texts
                        .iter()
                        .map(|t| crate::util::one_line(t, 200))
                        .collect::<Vec<_>>()
                ));
                "the stub's copy NOT as typed".to_string()
            }
        }
    };
    let said = format!(
        "land-check@v1 typed at +{} quoting \"{HINT_KEY}\"; {held}; {said}",
        at(t0, t2)
    );
    if seen.cover.check_texts.is_none() && bad.is_empty() {
        return Verdict::Unobserved(format!("{said}: the stub's dumps were not read"));
    }
    landed_verdict(bad, said, seen, done.0, window)
}

/// LL (land-listed): the check's list is quoted by the nudge, the nudge's reply does work, and a
/// second check comes only after that work (and the defense after it), its `LANDED` landing.
fn judge_relisted(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed".into());
    };
    let first = match the_check(seen, *t1, t0) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let nudges = seen.acts(turn_end::RULE_LAND_NUDGE);
    let Some(quoting) = nudges.iter().copied().find(|(t, _)| *t > first.0) else {
        return match lost_after(seen, first.0, t0) {
            Some(lost) => Verdict::Fail(format!("no nudge after the check's list: {lost}")),
            None => Verdict::Unobserved(format!(
                "no nudge after the check's list within {}",
                at(t0, seen.end_ms)
            )),
        };
    };
    let mut bad = check_text_faults(seen.act_text(turn_end::RULE_LAND_CHECK, first), None);
    let text = seen.act_text(turn_end::RULE_LAND_NUDGE, quoting);
    let suffix = text.find(LISTED_KEY).map_or("", |i| &text[i..]);
    if !suffix.contains(RELEASE_KEY) {
        bad.push(format!(
            "the nudge at +{} does not quote the list: {}",
            at(t0, quoting.0),
            crate::util::one_line(text, 300)
        ));
    }
    for w in [OPEN_SUFFIX, REPO_SUFFIX] {
        if text.contains(w) {
            bad.push(format!("the nudge says \"{w}\""));
        }
    }
    let worked = seen.journal.iter().find(|(t, l)| {
        *t > quoting.0 && idle_words(l).is_some_and(|w| w.contains(RELEASE_WORK_KEY))
    });
    let checks = checks_after(seen, *t1);
    let second = checks.get(1).copied();
    match (worked, second) {
        (_, None) => {
            let why = format!(
                "the list quoted at +{}, but no second check typed",
                at(t0, quoting.0)
            );
            return match lost_after(seen, quoting.0, t0) {
                Some(lost) => Verdict::Fail(format!("{why}: {lost}{}", tail_of(&bad))),
                None if bad.is_empty() => Verdict::Unobserved(why),
                None => Verdict::Fail(format!("{why}{}", tail_of(&bad))),
            };
        }
        (None, Some((t, _))) => bad.push(format!(
            "a second check at +{} with no work read before it",
            at(t0, *t)
        )),
        (Some((tw, _)), Some((t, _))) if *t < *tw => bad.push(format!(
            "a second check at +{} before the work read at +{}",
            at(t0, *t),
            at(t0, *tw)
        )),
        _ => {}
    }
    if checks.len() > 2 {
        bad.push(format!("{} land checks typed, not two", checks.len()));
    }
    let Some(second) = second else {
        return Verdict::Fail(bad.join("; "));
    };
    let Some(done) = seen.first_after("DONE ", second.0) else {
        return Verdict::Fail(format!(
            "no DONE within {} after the second check's LANDED{}",
            at(t0, seen.end_ms),
            tail_of(&bad)
        ));
    };
    if let Some((d, _)) = seen
        .first_after("DONE ", *t1)
        .filter(|(d, _)| *d < second.0)
    {
        bad.push(format!("LANDED at +{} on the list", at(t0, *d)));
    }
    let (more, said, window) = landing_quiet(seen, done, false, t0);
    bad.extend(more);
    let said = format!(
        "land-check@v1 at +{}, the list quoted by the nudge at +{}, the work read{}, the second check at +{}; {said}",
        at(t0, first.0),
        at(t0, quoting.0),
        worked.map_or_else(String::new, |(t, _)| format!(" at +{}", at(t0, *t))),
        at(t0, second.0)
    );
    landed_verdict(bad, said, seen, done.0, window)
}

/// LR (land-reworded): the first check answered with [`CHECK_LIST`], the nudge after it quoting
/// it; the work its reply did read; a second check only after that work, answered with
/// [`REWORDED_LIST`], and a nudge after it quoting the new words, never the old; never landed
/// (the hold, its quote on the window's surfaces, is [`surfaces::hold::reworded`]'s).
fn judge_reworded(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved("no land instruction was typed".into());
    };
    if let Some((d, l)) = seen.first_after("DONE ", *t1) {
        return Verdict::Fail(format!(
            "LANDED at +{} over the list ({})",
            at(t0, *d),
            crate::util::one_line(l, 90)
        ));
    }
    let first = match the_check(seen, *t1, t0) {
        Ok(c) => c,
        Err(v) => return v,
    };
    let mut bad = check_text_faults(seen.act_text(turn_end::RULE_LAND_CHECK, first), None);
    let nudges = seen.acts(turn_end::RULE_LAND_NUDGE);
    let quoting = |after: i64, key: &str| {
        nudges.iter().copied().find(|n| {
            n.0 > after && {
                let text = seen.act_text(turn_end::RULE_LAND_NUDGE, n);
                text.find(LISTED_KEY)
                    .is_some_and(|i| text[i..].contains(key))
            }
        })
    };
    let pending = |why: String, from: i64, bad: &[String]| match lost_after(seen, from, t0) {
        Some(lost) => Verdict::Fail(format!("{why}: {lost}{}", tail_of(bad))),
        None if bad.is_empty() => Verdict::Unobserved(why),
        None => Verdict::Fail(format!("{why}{}", tail_of(bad))),
    };
    let Some(old) = quoting(first.0, OPEN_KEY) else {
        return pending("no nudge quoted the check's list".into(), first.0, &bad);
    };
    let worked = seen
        .journal
        .iter()
        .find(|(t, l)| *t > old.0 && idle_words(l).is_some_and(|w| w.contains(REWORD_WORK_KEY)));
    let checks = checks_after(seen, *t1);
    let Some(second) = checks.get(1).copied() else {
        return pending(
            format!(
                "the list quoted at +{}, but no second check typed",
                at(t0, old.0)
            ),
            old.0,
            &bad,
        );
    };
    match worked {
        None => bad.push(format!(
            "a second check at +{} with no work read before it",
            at(t0, second.0)
        )),
        Some((tw, _)) if second.0 < *tw => bad.push(format!(
            "a second check at +{} before the work read at +{}",
            at(t0, second.0),
            at(t0, *tw)
        )),
        Some(_) => {}
    }
    if checks.len() > 2 {
        bad.push(format!("{} land checks typed, not two", checks.len()));
    }
    if let Some(stale) = quoting(second.0, OPEN_KEY) {
        bad.push(format!(
            "the nudge at +{} quotes the old words after the second check",
            at(t0, stale.0)
        ));
    }
    let Some((answer, _)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > second.0 && idle_words(l).is_some_and(|w| w.contains(REWORDED_KEY)))
    else {
        return pending(
            format!(
                "the second check at +{}, but no answer rewording the list read",
                at(t0, second.0)
            ),
            second.0,
            &bad,
        );
    };
    // The next nudge waits out its back-off, often past the watch: where one was typed it quotes
    // the new words.
    let next = nudges.iter().copied().find(|n| n.0 > second.0);
    let new = match (next, quoting(second.0, REWORDED_KEY)) {
        (Some(n), Some(q)) if q.0 == n.0 => format!(", the new words quoted at +{}", at(t0, n.0)),
        (Some(n), _) => {
            bad.push(format!(
                "the nudge at +{} after the second check quotes no new words",
                at(t0, n.0)
            ));
            String::new()
        }
        (None, _) => ", no nudge due yet".to_string(),
    };
    let said = format!(
        "land-check@v1 at +{}, the list quoted at +{}, the work read{}, the second check at +{}, \
         answered with the list reworded at +{}{new}",
        at(t0, first.0),
        at(t0, old.0),
        worked.map_or_else(String::new, |(t, _)| format!(" at +{}", at(t0, *t))),
        at(t0, second.0),
        at(t0, *answer)
    );
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// EVERY LANDING UNDER LAND COMES THROUGH THE CHECK, and no check meets a dirty repository: a
/// chain over a repository holding work types none; a chain of any other repository that landed
/// (one the check reads clean, [`Repo::reads_clean`]) typed one before its landing. `v` stands
/// where both hold.
fn through_check(chain: Chain, seen: &Seen, t0: i64, v: Verdict) -> Verdict {
    if chain.autopilot() != "land" {
        return v;
    }
    let checks = seen.acts(turn_end::RULE_LAND_CHECK);
    let fault = if chain == Chain::LandContradicted {
        contradicted_check_fault(seen, &checks, t0)
    } else if chain.repo().reads_clean() {
        seen.journal
            .iter()
            .find(|(_, l)| {
                l.starts_with("DONE ") && l.contains(&format!("rule={}", turn_end::RULE_LAND))
            })
            .filter(|(d, _)| !checks.iter().any(|(t, _)| t < d))
            .map(|(d, _)| {
                format!(
                    "landed at +{} with no land check typed before it",
                    at(t0, *d)
                )
            })
    } else {
        checks.first().map(|(t, l)| {
            format!(
                "a land check typed over a repository holding work at +{}: {}",
                at(t0, *t),
                crate::util::one_line(l, 100)
            )
        })
    };
    match (fault, v) {
        (None, v) => v,
        (Some(f), Verdict::Fail(why)) => Verdict::Fail(format!("{why}; {f}")),
        (Some(f), Verdict::Pass(why) | Verdict::Unobserved(why)) => {
            Verdict::Fail(format!("{why}; {f}"))
        }
    }
}

/// LAND-CONTRADICTED'S CHECKS (slice 4c): its repository holds work until the lab lands it after
/// the contradiction ([`Seen::landed_ms`]), so a land check before then is one typed over work,
/// and its landing comes through a check typed after it.
fn contradicted_check_fault(seen: &Seen, checks: &[&(i64, String)], t0: i64) -> Option<String> {
    let landed = seen.landed_ms.unwrap_or(i64::MAX);
    if let Some((t, l)) = checks.iter().find(|(t, _)| *t < landed) {
        return Some(format!(
            "a land check typed over a repository holding work at +{}: {}",
            at(t0, *t),
            crate::util::one_line(l, 100)
        ));
    }
    seen.journal
        .iter()
        .find(|(_, l)| {
            l.starts_with("DONE ") && l.contains(&format!("rule={}", turn_end::RULE_LAND))
        })
        .filter(|(d, _)| !checks.iter().any(|(t, _)| *t > landed && t < d))
        .map(|(d, _)| {
            format!(
                "landed at +{} with no land check typed after the work was landed",
                at(t0, *d)
            )
        })
}

/// LX (land-contradicted, slice 4c): Land's first nudge after the false defense is its
/// contradiction, typed under `land-nudge@v1`, leading with git's fact about `wip-notes.txt`
/// ([`Want::repo`]); it is the one act at that point, and once the lab has landed the file
/// ([`land_contradicted_work`]) the defense of the landed work gets the check and `LANDED`
/// lands it (status `task=landed`), no need raised and no second contradiction.
fn judge_contradicted<'a>(
    seen: &'a Seen,
    land: Option<&(i64, String)>,
    nudge_after: &dyn Fn(i64) -> Option<&'a (i64, String)>,
    t0: i64,
) -> Verdict {
    let Some((t1, _)) = land else {
        return Verdict::Unobserved(
            "no land instruction was typed, so no defense answered one".into(),
        );
    };
    let Some(nudge) = nudge_after(*t1) else {
        return match lost_after(seen, *t1, t0) {
            Some(lost) => Verdict::Fail(format!("no land-nudge@v1 after the defense: {lost}")),
            None => Verdict::Unobserved(format!(
                "no land-nudge@v1 after the defense within {}",
                at(t0, seen.end_ms)
            )),
        };
    };
    let text = seen.act_text(turn_end::RULE_LAND_NUDGE, nudge);
    let said = match nudge_words(text, &Want::repo(Repo::Untracked)) {
        Ok(suffix) => format!(
            "land-nudge@v1 typed at +{} leading with the contradiction: \"{}\"",
            at(t0, nudge.0),
            crate::util::one_line(suffix, 200)
        ),
        Err(why) => {
            return Verdict::Fail(format!("the nudge at +{} {why}", at(t0, nudge.0)));
        }
    };
    let mut bad = Vec::new();
    if let Some((t, l)) = seen
        .all_acts()
        .into_iter()
        .find(|(t, _)| *t > *t1 && *t < nudge.0)
    {
        bad.push(format!(
            "an act at +{} before the contradiction: {}",
            at(t0, *t),
            crate::util::one_line(l, 100)
        ));
    }
    let Some(landed) = seen.landed_ms.filter(|l| *l >= nudge.0) else {
        return Verdict::Fail(format!(
            "{said}; the lab did not land wip-notes.txt after it{}",
            tail_of(&bad)
        ));
    };
    let Some((d, dl)) = seen
        .first_after("DONE ", landed)
        .filter(|(_, l)| l.contains(&format!("rule={}", turn_end::RULE_LAND)))
    else {
        let why = format!(
            "{said}; wip-notes.txt landed at +{}, but no landing within {}",
            at(t0, landed),
            at(t0, seen.end_ms)
        );
        return match lost_after(seen, landed, t0) {
            Some(lost) => Verdict::Fail(format!("{why}: {lost}{}", tail_of(&bad))),
            None if bad.is_empty() => Verdict::Unobserved(why),
            None => Verdict::Fail(format!("{why}{}", tail_of(&bad))),
        };
    };
    let nudges = seen
        .acts(turn_end::RULE_LAND_NUDGE)
        .into_iter()
        .filter(|(t, _)| *t > *t1 && t < d)
        .count();
    if nudges != 1 {
        bad.push(format!("{nudges} nudges typed before the landing, not one"));
    }
    if let Some(n) = seen.needs_seen() {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    if !seen.status_said("task", "landed", *d - 5000) {
        bad.push("status never said task=landed".into());
    }
    let why = format!(
        "{said}; wip-notes.txt landed by the lab at +{}; {}",
        at(t0, landed),
        crate::util::one_line(dl, 120)
    );
    if bad.is_empty() {
        Verdict::Pass(why)
    } else {
        Verdict::Fail(format!("{why}: {}", bad.join("; ")))
    }
}

/// LY (land-contradicted-list, slice 4c's Open leg): the defense, said after real work, gets the
/// one hinted check, answered with the agent's list, and Land's nudge quotes that list
/// ([`judge_listed`]); the audited defense said again with no work gets Land's contradiction,
/// leading with the list quoted ([`Want::listed_contradicted`]), the next act after that nudge;
/// the same defense after it is held, nothing typed after the contradiction (the hold's words are
/// [`surfaces::held`]'s), never landed and no need raised.
fn judge_contradicted_list<'a>(
    seen: &'a Seen,
    land: Option<&(i64, String)>,
    nudge_after: &dyn Fn(i64) -> Option<&'a (i64, String)>,
    done: Option<&(i64, String)>,
    t0: i64,
) -> Verdict {
    let first = judge_listed(
        seen,
        land,
        nudge_after,
        done,
        (Some(OPEN_KEY), &Want::listed(&[OPEN_KEY])),
        t0,
    );
    let Verdict::Pass(listed) = first else {
        return first;
    };
    let quoted = land
        .and_then(|(t1, _)| checks_after(seen, *t1).first().copied())
        .and_then(|(tc, _)| nudge_after(*tc));
    let Some(quoted) = quoted else {
        return Verdict::Fail(format!("{listed}; but no nudge after the check read"));
    };
    let Some(contra) = nudge_after(quoted.0) else {
        if let Yield {
            held: Some((th, hl)),
            ..
        } = seen.yield_after(quoted.0, None, None)
        {
            return Verdict::Fail(format!(
                "{listed}; but the defense said again was held at +{}, not contradicted: {}",
                at(t0, *th),
                crate::util::one_line(hl, 100)
            ));
        }
        return match lost_after(seen, quoted.0, t0) {
            Some(lost) => Verdict::Fail(format!(
                "{listed}; no land-nudge@v1 after the nudge quoting the list: {lost}"
            )),
            None => Verdict::Unobserved(format!(
                "{listed}; no land-nudge@v1 after the nudge quoting the list within {}",
                at(t0, seen.end_ms)
            )),
        };
    };
    let text = seen.act_text(turn_end::RULE_LAND_NUDGE, contra);
    let said = match nudge_words(text, &Want::listed_contradicted(&[OPEN_KEY])) {
        Ok(suffix) => format!(
            "{listed}; then land-nudge@v1 at +{} leading with the contradiction: \"{}\"",
            at(t0, contra.0),
            crate::util::one_line(suffix, 200)
        ),
        Err(why) => {
            return Verdict::Fail(format!(
                "{listed}; but the nudge at +{} {why}",
                at(t0, contra.0)
            ));
        }
    };
    let mut bad = Vec::new();
    for (t, l) in seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| *t > quoted.0 && *t != contra.0)
    {
        bad.push(format!(
            "an act at +{} {} the contradiction: {}",
            at(t0, *t),
            if *t < contra.0 { "before" } else { "after" },
            crate::util::one_line(l, 100)
        ));
    }
    if let Some(n) = seen.needs_seen() {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    match seen.yield_after(contra.0, None, None) {
        Yield {
            event: Some(_),
            held: Some((th, hl)),
            ..
        } if bad.is_empty() => Verdict::Pass(format!(
            "{said}; +{} HELD {}",
            at(t0, *th),
            crate::util::one_line(hl, 100)
        )),
        Yield {
            held: None,
            waiting: Some((tw, wl)),
            ..
        } if bad.is_empty() => Verdict::Fail(format!(
            "{said}; not held after it: +{} {}",
            at(t0, *tw),
            crate::util::one_line(wl, 100)
        )),
        Yield { held: None, .. } if bad.is_empty() => match lost_after(seen, contra.0, t0) {
            Some(lost) => Verdict::Fail(format!("{said}; not held after it: {lost}")),
            None => Verdict::Unobserved(format!(
                "{said}; no hold read after it within {}",
                at(t0, seen.end_ms)
            )),
        },
        _ => Verdict::Fail(format!("{said}: {}", bad.join("; "))),
    }
}

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
        if let Some(lost) = lost_after(seen, *t1, t0) {
            return Verdict::Fail(format!(
                "no DONE, task={task}, but the check's defense was never read: {lost}"
            ));
        }
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
        None => match lost_after(seen, *t1, t0) {
            Some(lost) => Verdict::Fail(format!("no DONE, task={task}; {read}; {waited}: {lost}")),
            None => Verdict::Unobserved(format!(
                "no DONE, task={task}; {read}; {waited} — the next act not observed within {}",
                at(t0, seen.end_ms)
            )),
        },
    }
}

pub(crate) fn take_window_ms() -> i64 {
    i64::try_from(turn_end::TurnEndTiming::default().take_within.as_millis()).unwrap_or(i64::MAX)
}

/// Acts typed with no point judged between them: no `WAITING` (but a worker's take), `DONE` or
/// `LEFT` line, and no new point read (`EVENT idle` with words, its seq not the act before's).
/// An act typed at once has only its point to show: THE LAND CHECK, one step of the landing
/// typed at its defense's point with no back-off (`land-check@v1`, §14.17 item 5), and an act
/// after a turn of real work, which no back-off paces.
fn doubled_acts(seen: &Seen, t0: i64) -> Vec<String> {
    let acts = seen.all_acts();
    acts.windows(2)
        .filter(|w| {
            let (a, b) = (w[0].0, w[1].0);
            let judged = seen.journal.iter().any(|(t, l)| {
                *t > a
                    && *t < b
                    && ((l.starts_with("WAITING ") && !l.contains(" the worker to take the "))
                        || l.starts_with("DONE ")
                        || l.starts_with("LEFT "))
            });
            let new_point = seen.journal.iter().any(|(t, l)| {
                *t > a
                    && *t <= b
                    && idle_words(l).is_some_and(|said| !said.is_empty() && said != "-")
                    && word(l, "seq") != word(&w[0].1, "seq")
            });
            !(judged || new_point)
        })
        .map(|w| {
            format!(
                "+{} {} then +{} {}",
                at(t0, w[0].0),
                act_rule(&w[0].1).unwrap_or("?"),
                at(t0, w[1].0),
                act_rule(&w[1].1).unwrap_or("?")
            )
        })
        .collect()
}

type Line = (i64, String);

fn point_after(seen: &Seen, t: i64, until: Option<i64>) -> Option<(&Line, Option<&Line>)> {
    let before = |at: i64| until.is_none_or(|u| at < u);
    let event = seen.journal.iter().find(|(at, l)| {
        *at > t && before(*at) && idle_words(l).is_some_and(|w| !w.is_empty() && w != "-")
    })?;
    let decided = seen.journal.iter().find(|(at, l)| {
        *at >= event.0 && before(*at) && (l.starts_with("WAITING ") || l.starts_with("DONE "))
    });
    Some((event, decided))
}

fn backoff_named(line: &str) -> bool {
    line.starts_with("WAITING ")
        && line.split_once(" until=").is_some_and(|(_, rest)| {
            let words = rest.split_once(' ').map_or("", |(_, w)| w);
            words.contains("short turn(s) in a row") || words.ends_with(" resets")
        })
}

fn backoff_due(line: &str) -> Option<i64> {
    if !backoff_named(line) {
        return None;
    }
    let (_, rest) = line.split_once(" until=")?;
    crate::workers::iso_ms(rest.split_whitespace().next()?)
}

/// CRUISE'S WAIT BEFORE ITS REQUEST FOR A DIFFERENT APPROACH (the continuous-forward design of
/// 2026-11-01, `docs/DESIGN-continuous-forward-2026-11-01.md`): `WAITING … until=<t>
/// <turn_end::REPEAT_RECOVERY_WAIT>`, the loop's answer to a repeated report under Cruise. It
/// waits the short back-off ladder's step for the streak its point closed, and names none.
#[must_use]
pub fn recovery_wait(line: &str) -> bool {
    line.starts_with("WAITING ")
        && line.split_once(" until=").is_some_and(|(_, rest)| {
            rest.split_once(' ')
                .is_some_and(|(_, words)| words.trim() == turn_end::REPEAT_RECOVERY_WAIT)
        })
}

/// A [`recovery_wait`]'s `until=`, unix ms.
fn recovery_due(line: &str) -> Option<i64> {
    if !recovery_wait(line) {
        return None;
    }
    let (_, rest) = line.split_once(" until=")?;
    crate::workers::iso_ms(rest.split_whitespace().next()?)
}

const SAME_DUE_MS: i64 = 1000;

fn same_backoff(a: &str, b: &str) -> bool {
    backoff_named(a)
        && backoff_named(b)
        && backoff_streak(a) == backoff_streak(b)
        && matches!((backoff_due(a), backoff_due(b)), (Some(x), Some(y)) if (x - y).abs() <= SAME_DUE_MS)
}

fn own_backoff_after(seen: &Seen, t: i64) -> Option<&Line> {
    let standing: Vec<&str> = seen
        .journal
        .iter()
        .filter(|(at, l)| *at <= t && backoff_named(l))
        .map(|(_, l)| l.as_str())
        .collect();
    seen.journal.iter().find(|(at, l)| {
        *at > t
            && backoff_named(l)
            && !standing
                .iter()
                .any(|s| word(s, "seq") == word(l, "seq") || same_backoff(s, l))
    })
}

fn backoff_streak(line: &str) -> Option<u32> {
    let (_, rest) = line.split_once(" until=")?;
    let mut words = rest.split_whitespace().skip(1);
    let n = words.next()?.parse().ok()?;
    (words.next() == Some("short")).then_some(n)
}

/// A step of the short back-off ladder as the journal decided it ([`ladder_steps`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// When its `WAITING` was journaled first.
    pub at: i64,
    /// The short turns in a row it names.
    pub streak: u32,
    /// Its `until=`, unix ms.
    pub due: i64,
    /// Its journal line.
    pub line: String,
}

/// The short back-off ladder's steps from `t0`, as the journal decided them: the first turn-end
/// `WAITING … N short turn(s) in a row: …` of each point, in order — a wall's back-off and the take
/// window's left out, and a step told again left out whether under its point's `seq=` or a
/// re-read's own ([`same_backoff`]: the same streak and due). Cruise's wait before its request for
/// a different approach ([`recovery_wait`]) is a step too, the one after the step before it (its
/// streak one more, or one where it is the first): the loop waits the ladder's step for the
/// streak its point closed there, in words that name none.
#[must_use]
pub fn ladder_steps(seen: &Seen, t0: i64) -> Vec<Step> {
    let mut seqs: Vec<Option<String>> = Vec::new();
    let mut out: Vec<Step> = Vec::new();
    for (t, l) in &seen.journal {
        if *t < t0 {
            continue;
        }
        let streak = if backoff_named(l) {
            backoff_streak(l)
        } else if recovery_wait(l) {
            Some(out.last().map_or(1, |st| st.streak.saturating_add(1)))
        } else {
            None
        };
        let Some(streak) = streak else {
            continue;
        };
        let seq = word(l, "seq");
        let told = |st: &Step| {
            same_backoff(&st.line, l)
                || (recovery_wait(&st.line)
                    && recovery_due(l).is_some_and(|d| (d - st.due).abs() <= SAME_DUE_MS))
        };
        if seqs.contains(&seq) || out.iter().any(told) {
            continue;
        }
        seqs.push(seq);
        if let Some(due) = backoff_due(l).or_else(|| recovery_due(l)) {
            out.push(Step {
                at: *t,
                streak,
                due,
                line: l.clone(),
            });
        }
    }
    out
}

fn worded_event(line: &str) -> bool {
    line.strip_prefix("EVENT ")
        .and_then(|rest| rest.split_once(' '))
        .and_then(|(_, rest)| {
            let (seq, words) = rest.split_once(' ').unwrap_or((rest, ""));
            seq.starts_with("seq=").then_some(words.trim())
        })
        .is_some_and(|w| !w.is_empty() && w != "-")
}

fn early_acts(seen: &Seen, t0: i64) -> Vec<String> {
    let mut out = Vec::new();
    let mut prev = i64::MIN;
    for (ta, la) in seen.all_acts() {
        let since: Vec<&Line> = seen
            .journal
            .iter()
            .filter(|(t, _)| *t > prev && *t < *ta)
            .collect();
        prev = *ta;
        let Some(i) = since.iter().rposition(|(_, l)| backoff_named(l)) else {
            continue;
        };
        let (tw, wl) = since[i];
        let seq = word(wl, "seq");
        if since[i + 1..]
            .iter()
            .any(|(_, l)| worded_event(l) && word(l, "seq") != seq)
        {
            continue;
        }
        if [seen.cover.switch.as_ref(), seen.cover.relaunch.as_ref()]
            .into_iter()
            .flatten()
            .any(|sw| sw.exited_ms > *tw && sw.exited_ms < *ta)
        {
            continue;
        }
        // A switch of the tab's word to Land between the back-off and the act decides the act
        // anew: Land's instruction waits no back-off (v6 §14.10).
        if act_rule(la) == Some(turn_end::RULE_LAND)
            && seen
                .cover
                .words
                .iter()
                .any(|w| w.word == "land" && w.at > *tw && w.at < *ta)
        {
            continue;
        }
        match backoff_due(wl) {
            Some(due) if *ta < due - DUE_SLACK_MS => out.push(format!(
                "+{} {} typed {} BEFORE the due its back-off named (+{} {})",
                at(t0, *ta),
                act_rule(la).unwrap_or("?"),
                at(0, due - ta),
                at(t0, *tw),
                crate::util::one_line(wl, 110)
            )),
            Some(_) => {}
            None => out.push(format!(
                "the back-off at +{} names no due that reads: {}",
                at(t0, *tw),
                crate::util::one_line(wl, 110)
            )),
        }
    }
    out
}

fn lost_after(seen: &Seen, t: i64, t0: i64) -> Option<String> {
    if seen.all_acts().iter().any(|(at, _)| *at > t) {
        return None;
    }
    match seen
        .journal
        .iter()
        .rfind(|(at, l)| *at > t && (backoff_named(l) || recovery_wait(l)))
    {
        Some((tw, wl)) => {
            let due = backoff_due(wl).or_else(|| recovery_due(wl))?;
            (seen.end_ms >= due + PERSISTENCE_GRACE_MS).then(|| {
                format!(
                    "PERSISTENCE LOST: the back-off at +{} ({}) named its act due at +{}, and nothing was typed by +{}, {} past it",
                    at(t0, *tw),
                    crate::util::one_line(wl, 110),
                    at(t0, due),
                    at(t0, seen.end_ms),
                    at(0, seen.end_ms - due)
                )
            })
        }
        None => {
            let decided = seen
                .journal
                .iter()
                .any(|(at, l)| *at > t && (l.starts_with("DONE ") || l.starts_with("LEFT ")));
            (!decided && seen.end_ms - t >= UNJUDGED_MS).then(|| {
                format!(
                    "PERSISTENCE LOST: no point judged after +{} (no back-off, no DONE, no LEFT journaled) in the {} watched after it",
                    at(t0, t),
                    at(0, seen.end_ms - t)
                )
            })
        }
    }
}

fn p3_faults(seen: &Seen, t0: i64, first_works: bool) -> Vec<String> {
    let from = seen.cover.person_ms.unwrap_or(t0);
    let mut out: Vec<String> = seen
        .ledger
        .iter()
        .filter(|r| r.rule == RULE_POINT && r.ts < from)
        .map(|r| {
            format!(
                "a point judged {} before the first prompt (P3): {}",
                at(r.ts, from),
                crate::util::one_line(&r.reason, 90)
            )
        })
        .collect();
    if let Some(first) = seen
        .ledger
        .iter()
        .find(|r| r.rule == RULE_POINT && r.ts >= from)
    {
        let left = seen.ledger.iter().any(|r| {
            r.rule == turn_end::RULE_LEFT
                && r.decision == "skipped"
                && (r.ts - first.ts).abs() <= AT_DONE_MS
        });
        // A first point that met an outage is decided as the outage's wait, never a short-turn
        // back-off, so P3's ladder does not start there.
        let outage = seen
            .journal
            .iter()
            .any(|(t, l)| (t - first.ts).abs() <= AT_DONE_MS && outage::is_wait(l));
        let want = u32::from(!left && !first_works);
        if !outage && point_streak(first) != Some(want) {
            out.push(format!(
                "the first point at +{} closed {} short turn(s), not {want} (P3: the ladder starts a step late): {}",
                at(t0, first.ts),
                point_streak(first).map_or_else(|| "?".to_string(), |n| n.to_string()),
                crate::util::one_line(&first.reason, 90)
            ));
        }
    }
    out
}

/// What fails every scenario of a chain, whatever each scenario's own judge says: an act typed
/// before the due its back-off named ([`early_acts`]), an act typed twice with no point judged
/// between ([`doubled_acts`]), a point before the first prompt or a first point that closed other
/// than one short turn, none where the chain's first reply is real work (`first_works`,
/// [`Chain::first_reply_works`]) and no count where it met an outage ([`p3_faults`]).
#[must_use]
pub fn chain_faults(seen: &Seen, t0: i64, first_works: bool) -> Vec<String> {
    let mut out = early_acts(seen, t0);
    let doubled = doubled_acts(seen, t0);
    if !doubled.is_empty() {
        out.push(format!(
            "acts typed with no point judged between: {doubled:?}"
        ));
    }
    out.extend(p3_faults(seen, t0, first_works));
    out
}

fn rev_gaps(seen: &Seen, t0: i64) -> Vec<String> {
    let revs: Vec<(i64, u64)> = seen
        .task
        .iter()
        .filter_map(|(t, r)| Some((*t, word(r, "rev")?.parse().ok()?)))
        .collect();
    revs.windows(2)
        .filter(|w| w[1].1 != w[0].1 && w[1].1 != w[0].1 + 1)
        .map(|w| format!("rev {} then {} at +{}", w[0].1, w[1].1, at(t0, w[1].0)))
        .collect()
}

fn judge_instant_land(
    seen: &Seen,
    land: Option<&(i64, String)>,
    done: Option<&(i64, String)>,
    t0: i64,
) -> Verdict {
    let first = point_after(seen, t0, land.map(|(t, _)| *t));
    let Some((t1, _)) = land else {
        return Verdict::Fail(format!(
            "the first prompt's reply, sent at once, was never judged: no land@v1 within {}{}",
            at(t0, seen.end_ms),
            first.map_or_else(
                || " and no point read (no EVENT idle with words)".to_string(),
                |((t, l), _)| format!(
                    " (a point read at +{}: {})",
                    at(t0, *t),
                    crate::util::one_line(l, 90)
                )
            )
        ));
    };
    let Some((te, el)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t0 && t < t1 && idle_words(l).is_some_and(|w| w.contains(WORK_KEY)))
    else {
        return Verdict::Fail(format!(
            "land@v1 typed at +{}, but the first reply, sent at once, was never read: no EVENT idle holding \"{WORK_KEY}\" before it{}",
            at(t0, *t1),
            first.map_or_else(String::new, |((t, l), _)| format!(
                " (the point read at +{}: {})",
                at(t0, *t),
                crate::util::one_line(l, 90)
            ))
        ));
    };
    let seq = word(el, "seq");
    // The decision at that point: its back-off, else Land's instruction typed at it at once (v6
    // §14.10).
    let Some((tw, wl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t >= *te && t < t1 && backoff_named(l) && word(l, "seq") == seq)
        .or_else(|| land.filter(|(_, l)| word(l, "seq") == seq))
    else {
        return Verdict::Fail(format!(
            "the first reply read at +{} ({}), but no back-off decided at that point, nor land@v1 typed at it, before land@v1 at +{}",
            at(t0, *te),
            crate::util::one_line(el, 80),
            at(t0, *t1)
        ));
    };
    let read = format!(
        "its point read at +{} ({}), +{} {}",
        at(t0, *te),
        crate::util::one_line(el, 80),
        at(t0, *tw),
        crate::util::one_line(wl, 100)
    );
    let Some(done) = done.filter(|(d, _)| d > t1) else {
        return Verdict::Fail(format!(
            "land@v1 typed at +{} ({read}), but its clean defense, sent at once, never landed the task within {}; acts typed: {}",
            at(t0, *t1),
            at(t0, seen.end_ms),
            seen.all_acts()
                .iter()
                .map(|(t, l)| format!("+{} {}", at(t0, *t), act_rule(l).unwrap_or("?")))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let (mut bad, said, window) = landing_quiet(seen, done, false, t0);
    let before: Vec<&(i64, String)> = seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| *t < done.0)
        .collect();
    // land@v1 at its due, and the land check its clean defense gets at once (§14.17 item 5).
    let checked = before.len() == 2
        && act_rule(&before[1].1) == Some(turn_end::RULE_LAND_CHECK)
        && before[1].0 > *t1;
    if before.len() != 1 && !checked {
        bad.push(format!(
            "{} acts before the landing (one land@v1 is its due, then its defense's land check): {}",
            before.len(),
            before
                .iter()
                .map(|(t, l)| format!("+{} {}", at(t0, *t), act_rule(l).unwrap_or("?")))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let taken_in = done.0 - t1;
    if taken_in >= take_window_ms() {
        bad.push(format!(
            "the landing came {} after the land act: at the take window's end, not as its answer showed",
            at(0, taken_in)
        ));
    }
    let doubled = doubled_acts(seen, t0);
    if !doubled.is_empty() {
        bad.push(format!(
            "acts typed with no point judged between: {doubled:?}"
        ));
    }
    if let Some(n) = seen.needs_seen() {
        bad.push(format!("a need was raised, though no reply named one: {n}"));
    }
    let said = format!(
        "{read}; land@v1 typed at +{}; landed {} after it; {said}",
        at(t0, *t1),
        at(0, taken_in)
    );
    landed_verdict(bad, said, seen, done.0, window)
}

fn judge_instant_cruise(seen: &Seen, done: Option<&(i64, String)>, t0: i64) -> Verdict {
    let acts = seen.all_acts();
    let Some((t1, l1)) = acts.first().copied() else {
        return Verdict::Fail(format!(
            "the first prompt's reply, sent at once, was never judged: nothing typed within {}{}",
            at(t0, seen.end_ms),
            point_after(seen, t0, None).map_or_else(
                || " and no point read (no EVENT idle with words)".to_string(),
                |((t, l), _)| format!(
                    " (a point read at +{}: {})",
                    at(t0, *t),
                    crate::util::one_line(l, 90)
                )
            )
        ));
    };
    if act_rule(l1) != Some(turn_end::RULE_DONE_CHECK) {
        return Verdict::Fail(format!(
            "the first act at +{} is {}, not continue-done-check@v1",
            at(t0, *t1),
            act_rule(l1).unwrap_or("?")
        ));
    }
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
    let doubled = doubled_acts(seen, t0);
    if !doubled.is_empty() {
        return Verdict::Fail(format!(
            "acts typed with no point judged between: {doubled:?}"
        ));
    }
    let again = acts.get(1).copied();
    let until = again.map(|(t, _)| *t);
    let y = seen.yield_after(*t1, until, Some(DEFENSE_HEAD));
    let Some((te, el)) = y.event else {
        return Verdict::Fail(format!(
            "continue-done-check@v1 typed at +{}, but its defense, sent at once, was never read (no EVENT idle … {DEFENSE_HEAD} after it{})",
            at(t0, *t1),
            again.map_or_else(
                || format!(" within {}", at(t0, seen.end_ms)),
                |(t, _)| format!(" before the next act at +{}", at(t0, *t))
            )
        ));
    };
    let read = format!(
        "continue-done-check@v1 typed at +{}; its defense read at +{} ({})",
        at(t0, *t1),
        at(t0, *te),
        crate::util::one_line(el, 90)
    );
    let Some((tw, wl)) = y.waiting else {
        return Verdict::Fail(format!(
            "{read}, but no WAITING decided for that point{}",
            again.map_or_else(
                || format!(" within {}", at(t0, seen.end_ms)),
                |(t, _)| format!(" before the act at +{}", at(t0, *t))
            )
        ));
    };
    if te - t1 >= take_window_ms() || wl.contains(" the worker to take the ") {
        return Verdict::Fail(format!(
            "{read}: judged at the take window's end, not as its answer showed: +{} {}",
            at(t0, *tw),
            crate::util::one_line(wl, 110)
        ));
    }
    let waited = format!("+{} {}", at(t0, *tw), crate::util::one_line(wl, 110));
    match again {
        Some((t2, l2)) => Verdict::Pass(format!(
            "{read}; {waited}; {} typed at +{}",
            act_rule(l2).unwrap_or("?"),
            at(t0, *t2)
        )),
        None => match lost_after(seen, *te, t0) {
            Some(lost) => Verdict::Fail(format!("{read}; {waited}: {lost}")),
            None => Verdict::Unobserved(format!(
                "{read}; {waited} — the next act not observed within {}",
                at(t0, seen.end_ms)
            )),
        },
    }
}

fn judge_instant_ask(seen: &Seen, t0: i64) -> Verdict {
    let Some(asked) = seen.asked_ms else {
        return Verdict::Unobserved(match seen.ask_refused_ms {
            Some(t) => format!(
                "the person's prompt typed at +{} was not taken by the composer (it never stood \
                 there as typed, or stood after Enter twice)",
                at(t0, t)
            ),
            None => {
                "the person's prompt was never typed (the check's yield never decided: P4C)".into()
            }
        });
    };
    let head: String = ASK_REPLY.chars().take(24).collect();
    let event = seen
        .journal
        .iter()
        .find(|(t, l)| *t > asked && idle_words(l).is_some_and(|w| w.contains(head.as_str())));
    let typed_before = seen
        .all_acts()
        .into_iter()
        .find(|(t, _)| *t > asked && event.is_none_or(|(e, _)| t < e));
    if let Some((t, l)) = typed_before {
        return Verdict::Fail(format!(
            "{} typed at +{} after the person's prompt at +{}, before its reply was judged{}",
            act_rule(l).unwrap_or("?"),
            at(t0, *t),
            at(t0, asked),
            event.map_or_else(String::new, |(e, _)| format!(" (at +{})", at(t0, *e)))
        ));
    }
    let Some((te, el)) = event else {
        return Verdict::Fail(format!(
            "the person's prompt at +{}, answered at once, was never judged: no EVENT idle with its reply within {}",
            at(t0, asked),
            at(t0, seen.end_ms)
        ));
    };
    let read = format!(
        "the person's prompt at +{}; its reply read at +{} ({})",
        at(t0, asked),
        at(t0, *te),
        crate::util::one_line(el, 90)
    );
    if te - asked >= take_window_ms() {
        return Verdict::Fail(format!(
            "{read}: {} after it, not as it showed",
            at(0, te - asked)
        ));
    }
    let next_act = seen.all_acts().into_iter().find(|(t, _)| t > te);
    let before_next = |t: i64| next_act.is_none_or(|(n, _)| t < *n);
    let doubled = doubled_acts(seen, t0);
    if !doubled.is_empty() {
        return Verdict::Fail(format!(
            "{read}; acts typed with no point judged between: {doubled:?}"
        ));
    }
    if let Some((tl, ll)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t >= *te && before_next(*t) && l.starts_with("LEFT "))
    {
        return Verdict::Fail(format!(
            "{read}; LEFT at +{} ({}), though no person's hand typed it",
            at(t0, *tl),
            crate::util::one_line(ll, 110)
        ));
    }
    let seq = word(el, "seq");
    let Some((tw, wl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t >= *te && before_next(*t) && backoff_named(l) && word(l, "seq") == seq)
    else {
        return Verdict::Fail(format!(
            "{read}, but no back-off decided at that point (seq={}) within {}{}",
            seq.as_deref().unwrap_or("?"),
            at(t0, seen.end_ms),
            seen.journal
                .iter()
                .find(|(t, l)| *t >= *te
                    && before_next(*t)
                    && (l.starts_with("WAITING ") || l.starts_with("DONE ")))
                .map_or_else(String::new, |(t, l)| format!(
                    " (+{} {})",
                    at(t0, *t),
                    crate::util::one_line(l, 110)
                ))
        ));
    };
    let decided = format!(
        "decided at +{} ({})",
        at(t0, *tw),
        crate::util::one_line(wl, 110)
    );
    match next_act {
        Some((ta, la)) => Verdict::Pass(format!(
            "{read}; {decided}; {} typed at +{}",
            act_rule(la).unwrap_or("?"),
            at(t0, *ta)
        )),
        None => match lost_after(seen, *te, t0) {
            Some(lost) => Verdict::Fail(format!("{read}; {decided}: {lost}")),
            None => Verdict::Unobserved(format!(
                "{read}; {decided} — the act after its back-off not observed within {}",
                at(t0, seen.end_ms)
            )),
        },
    }
}

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

fn could_not(m: &str) -> Fail {
    Fail::exit(3, format!("forever: COULD NOT RUN: {m}"))
}

/// A chain's tab spawned in `dir` and let settle idle, as the forever lab opens each one (and
/// `autopilot-captures` and its typing take): its session id.
/// # Errors
/// The control socket failed, or `spawn` named no session (a COULD NOT RUN).
pub(crate) fn spawn_tab(door: &Door<'_>, dir: &Path) -> Result<String, Fail> {
    let r = door.ctl(&["spawn", &format!("cwd={}", dir.display())])?;
    let sid = r
        .stdout
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("OK "))
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| could_not(&format!("spawn: {}{}", r.stdout, r.stderr)))?;
    let _ = on(door, &sid, &["await", "idle", "400", "timeout=30000"]);
    Ok(sid)
}

/// What [`launch_tab`] said, and a tasked chain's mark: the record as its `task mark` replied, at
/// the time it was read.
pub(crate) struct Launched {
    /// The Autopilot it set and `meta set`'s reply, and the task's.
    pub(crate) said: String,
    /// The record's first read, the only `working` one when the task lands before the first look.
    pub(crate) task: Option<(i64, String)>,
}

/// `chain`'s program launched in tab `sid` as the forever lab launches it: its Autopilot set, a
/// tasked chain's task marked working, then `script` typed and entered.
/// # Errors
/// A tasked chain's task was not marked working (a COULD NOT RUN).
pub(crate) fn launch_tab(
    door: &Door<'_>,
    sid: &str,
    chain: Chain,
    script: &Path,
) -> Result<Launched, Fail> {
    let set = on(door, sid, &["meta", "set", "autopilot", chain.autopilot()]);
    let task = if chain.tasked() {
        let r = on(door, sid, &["task", "mark", "working", "from=ctl"]);
        if !r.contains("state=working") {
            return Err(could_not(&format!(
                "{}: task mark working: {}",
                chain.name(),
                r.trim()
            )));
        }
        Some((now_ms(), r.trim().to_string()))
    } else {
        None
    };
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    let _ = on(door, sid, &["send", &line]);
    let _ = on(door, sid, &["key", "enter"]);
    let marked = task.as_ref().map_or_else(String::new, |(_, r)| {
        format!(", task {}", crate::util::one_line(r, 120))
    });
    Ok(Launched {
        said: format!("autopilot {} ({}){marked}", chain.autopilot(), set.trim()),
        task,
    })
}

fn git(home: &Path, dir: &Path, args: &[&str]) -> Result<(), String> {
    git_out(home, dir, args).map(|_| ())
}

/// [`git`]'s run, its standard output on success.
pub(crate) fn git_out(home: &Path, dir: &Path, args: &[&str]) -> Result<String, String> {
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
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub(crate) fn lay_repo(run: &Run, chain: Chain) -> Result<PathBuf, String> {
    let dir = Path::new(&run.proj).join(chain.name());
    let bare = Path::new(&run.sockdir).join(format!("origin-{}.git", chain.name()));
    lay_repo_at(run, &dir, &bare, &chain.token(), chain.repo())
}

/// A chain's repository laid in `dir` ([`lay_repo`]'s, `token` its README's word): `main`
/// committed and pushed to `bare`, its `origin`, with `repo` beside it. The corpus's Land scenes
/// lay theirs in the lab's own project ([`crate::autopilot_scenes`]).
/// # Errors
/// A folder that cannot be made, or a git step that fails.
pub fn lay_repo_at(
    run: &Run,
    dir: &Path,
    bare: &Path,
    token: &str,
    repo: Repo,
) -> Result<PathBuf, String> {
    let home = run.w.join("home");
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::create_dir_all(bare).map_err(|e| format!("{}: {e}", bare.display()))?;
    let put = |name: &str, text: &str| {
        crate::util::write_fresh(&dir.join(name), text).map_err(|e| format!("{name}: {e}"))
    };
    let g = |args: &[&str]| git(&home, dir, args);
    if repo == Repo::Plain {
        put(
            "README.md",
            &format!("# {token}\n\nThe forever lab's scratch folder, no repository.\n"),
        )?;
        return std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()));
    }
    git(&home, bare, &["init", "-q", "--bare", "-b", "main"])?;
    g(&["init", "-q", "-b", "main"])?;
    put(
        "README.md",
        &format!("# {token}\n\nThe forever lab's scratch project.\n"),
    )?;
    g(&["add", "README.md"])?;
    g(&["commit", "-q", "-m", "lab: the scratch project"])?;
    g(&["remote", "add", "origin", &bare.to_string_lossy()])?;
    g(&["push", "-q", "-u", "origin", "main"])?;
    g(&["remote", "set-head", "origin", "main"])?;
    let readme_changed = "# The forever lab\n\nA change nobody committed.\n";
    match repo {
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
            if repo == Repo::Unlanded {
                g(&["push", "-q", "origin", "main:wip"])?;
            }
        }
        Repo::Others => angles::lay_others(&home, dir)?,
        Repo::NoRemote => g(&["remote", "remove", "origin"])?,
        Repo::Plain => {}
    }
    std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Seed each of `dirs` (and its `/private`-less spelling) as a folder Claude Code trusts, in the
/// `.claude.json` the run's Claude Code reads ([`Run::claude_json`]: the scratch `.claude`'s,
/// or after an own login the scratch HOME's own).
/// # Errors
/// The scratch `.claude.json` could not be written (a COULD NOT RUN).
pub(crate) fn trust_for_claude(run: &Run, dirs: &[PathBuf]) -> Result<(), Fail> {
    let p = run.claude_json();
    let text = crate::util::scratch_text(&p);
    let mut cj = J::parse(&text).unwrap_or_else(|_| J::obj([]));
    let mut projects = match cj.get("projects") {
        Some(J::Obj(m)) => m.clone(),
        _ => Vec::new(),
    };
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

/// One script every tab of `h` runs ([`Launcher::shared`]), its preparation done
/// ([`launcher_with`]): Claude Code's with no debug log of its own.
pub(crate) fn launcher(
    run: &Run,
    h: Harness,
    dirs: &[PathBuf],
    walled: bool,
) -> Result<PathBuf, Fail> {
    launcher_with(run, h, dirs, (walled, false), (Vec::new(), Vec::new()))?.shared(run)
}

/// How a forever lab launches one harness's tabs, once [`launcher_with`] did what every tab
/// shares (the folders trusted, the settings and the login laid): one script, or Claude Code's
/// launch, written per chain so each tab keeps a debug log of its own ([`Launcher::of_chain`]).
pub(crate) enum Launcher {
    /// agy's or Muse Code's one script.
    Script(PathBuf),
    /// Claude Code's launch, the same for every chain but its debug log.
    Claude(crate::lab::Launch),
}

impl Launcher {
    /// The one script every tab runs alike, `forever-<harness>.sh` (Claude Code's with no debug
    /// log), for a launch that is no chain's own start (a switch's, autopilot-captures').
    /// # Errors
    /// Claude Code's script could not be written (a COULD NOT RUN).
    pub(crate) fn shared(&self, run: &Run) -> Result<PathBuf, Fail> {
        match self {
            Self::Script(p) => Ok(p.clone()),
            Self::Claude(l) => put_script(run, "forever-claude.sh", &crate::lab::launcher(run, l)),
        }
    }

    /// The script `chain`'s tab runs: Claude Code's its own, `forever-claude-<chain>.sh`, its
    /// `--debug-file` the chain's ([`crate::stats::chain_debug_log`]), else the one script.
    /// # Errors
    /// Claude Code's script could not be written (a COULD NOT RUN).
    pub(crate) fn of_chain(&self, run: &Run, chain: Chain) -> Result<PathBuf, Fail> {
        match self {
            Self::Script(p) => Ok(p.clone()),
            Self::Claude(l) => {
                let mut l = l.clone();
                l.args.splice(0..0, chain_debug_args(run, chain));
                put_script(
                    run,
                    &format!("forever-claude-{}.sh", chain.name()),
                    &crate::lab::launcher(run, &l),
                )
            }
        }
    }
}

/// `--debug-file <chain's log>` ([`crate::stats::chain_debug_log`]) for Claude Code's line.
fn chain_debug_args(run: &Run, chain: Chain) -> [String; 2] {
    [
        "--debug-file".to_string(),
        crate::stats::chain_debug_log(run, chain.name())
            .to_string_lossy()
            .into_owned(),
    ]
}

/// `text` as the run's launch script `name`: its path.
fn put_script(run: &Run, name: &str, text: &str) -> Result<PathBuf, Fail> {
    let script = run.run.join(name);
    crate::sys::put(&script, 0o755, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| could_not(&format!("{}: {e}", script.display())))?;
    Ok(script)
}

/// [`launcher`]'s [`Launcher`], with `envs` in the agent's environment and `args` on its line
/// after the lab's own (W10's and W11's usage tap, [`tap::tap_args`]; a system message's chains'
/// own retries cut short, [`sysmsg::vendor_retries`]); Claude Code on a stored login of the lab's
/// where `stored` ([`sysmsg::login::lay_stored`]).
fn launcher_with(
    run: &Run,
    h: Harness,
    dirs: &[PathBuf],
    (walled, stored): (bool, bool),
    (envs, args): (Vec<String>, Vec<String>),
) -> Result<Launcher, Fail> {
    let put = |p: &Path, mode: u32, text: &str| {
        crate::sys::put(p, mode, &crate::sys::PutSrc::Bytes(text.as_bytes()))
            .map_err(|e| could_not(&format!("{}: {e}", p.display())))
    };
    let text = match h {
        Harness::Claude => {
            trust_for_claude(run, dirs)?;
            if walled {
                crate::verbs::settings_merge(run, &J::obj([("apiKeyHelper", J::Null)]))?;
            }
            if stored {
                sysmsg::login::lay_stored(run)?;
            }
            return Ok(Launcher::Claude(crate::lab::Launch {
                subscriber: walled,
                stub_route_alone: stored,
                envs,
                args,
                ..crate::lab::Launch::default()
            }));
        }
        Harness::Agy => {
            crate::agy::seed(run, &crate::agy::Launch::default())?;
            let dir = run.w.join("agy-home/.gemini/antigravity-cli");
            put(
                &dir.join("settings.json"),
                0o600,
                &crate::agy::agy_settings_with(&agy_trusted(dirs), &[]),
            )?;
            crate::agy::launcher(run, &envs, &args)
        }
        Harness::Muse => {
            let mut args = args;
            args.push("--trust-workspace".to_string());
            crate::muse::launcher(run, &envs, &args)
        }
    };
    Ok(Launcher::Script(put_script(
        run,
        &format!("forever-{}.sh", h.name()),
        &text,
    )?))
}

fn agy_trusted(dirs: &[PathBuf]) -> Vec<String> {
    let mut trusted: Vec<String> = dirs
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    trusted.extend(dirs.iter().filter_map(|d| {
        d.to_string_lossy()
            .strip_prefix("/private")
            .map(str::to_string)
    }));
    trusted
}

/// The renderer that takes the wheel, for `tall-scroll`'s tab ([`scroll_launcher`]): the harness
/// reads the tab's live screen (`text --json`), which a scroll of the tab's own viewport never
/// moves, so only an agent that scrolls its own view brings a judged opener back into the
/// harness's sight.
pub const SCROLL_CLAUDE_ENV: &str = "CLAUDE_CODE_NO_FLICKER=1";
/// agy's `tall-scroll` home under the run's scratch ([`SCROLL_CLAUDE_ENV`]'s reason): its settings
/// the run's with `altScreenMode` `always`, and its onboarding done.
pub const SCROLL_AGY_HOME: &str = "agy-scroll-home";

fn scroll_launcher(
    run: &Run,
    h: Harness,
    dirs: &[PathBuf],
    walled: bool,
) -> Result<Option<PathBuf>, Fail> {
    let put = |p: &Path, mode: u32, text: &str| {
        crate::sys::put(p, mode, &crate::sys::PutSrc::Bytes(text.as_bytes()))
            .map_err(|e| could_not(&format!("{}: {e}", p.display())))
    };
    let text = match h {
        Harness::Claude => crate::lab::launcher(
            run,
            &crate::lab::Launch {
                envs: vec![SCROLL_CLAUDE_ENV.to_string()],
                subscriber: walled,
                args: chain_debug_args(run, Chain::TallScroll).to_vec(),
                ..crate::lab::Launch::default()
            },
        ),
        Harness::Agy => {
            let home = run.w.join(SCROLL_AGY_HOME);
            let dir = home.join(".gemini/antigravity-cli");
            put(
                &dir.join("settings.json"),
                0o600,
                &crate::agy::agy_settings_with(
                    &agy_trusted(dirs),
                    &[("altScreenMode".to_string(), J::s("always"))],
                ),
            )?;
            put(
                &dir.join("cache/onboarding.json"),
                0o600,
                &crate::agy::agy_onboarded(),
            )?;
            crate::agy::launcher(run, &[format!("HOME={}", home.to_string_lossy())], &[])
        }
        Harness::Muse => return Ok(None),
    };
    let script = run.run.join(format!("forever-{}-scroll.sh", h.name()));
    put(&script, 0o755, &text)?;
    Ok(Some(script))
}

struct Tab {
    chain: Chain,
    sid: String,
    dir: PathBuf,
    seen: Seen,
    journal_cursor: crate::util::LineTailCursor,
    ledger_cursor: crate::util::LineTailCursor,
    t0: i64,
    ask_tried: bool,
    over: bool,
    /// The chain's need gate was opened ([`Chain::need_gate`], [`open_need_gate`]).
    gate_opened: bool,
}

pub(crate) fn on(door: &Door<'_>, sid: &str, args: &[&str]) -> String {
    let at = format!("@{sid}");
    let mut v = vec![at.as_str()];
    v.extend_from_slice(args);
    door.out(&v)
}

/// `aterm ctl <args>`'s standard output where it answered, for a read a judge counts from: a
/// failed reply is no reading, and [`Door::out`]'s empty string for one cannot be told from a line
/// or a screen that names nothing.
/// # Errors
/// What it said instead where it did not answer: the client could not start, it exited non-zero,
/// or it said `ERR`.
pub(crate) fn reading(door: &Door<'_>, args: &[&str]) -> Result<String, String> {
    let asked = args.join(" ");
    let r = door.ctl(args).map_err(|e| format!("`{asked}`: {e}"))?;
    let refused = r
        .stdout
        .lines()
        .next()
        .is_some_and(|l| l == "ERR" || l.starts_with("ERR "));
    if r.code == 0 && !refused {
        return Ok(r.stdout);
    }
    Err(format!(
        "`{asked}` answered {}: {}",
        r.code,
        crate::util::one_line(&format!("{} {}", r.stdout.trim(), r.stderr.trim()), 200)
    ))
}

/// [`reading`] of tab `sid` (`@<sid> <args>`).
/// # Errors
/// As [`reading`].
pub(crate) fn reading_on(door: &Door<'_>, sid: &str, args: &[&str]) -> Result<String, String> {
    let at = format!("@{sid}");
    let mut v = vec![at.as_str()];
    v.extend_from_slice(args);
    reading(door, &v)
}

fn at_composer(door: &Door<'_>, sid: &str, agent: &str) -> Option<String> {
    crate::screen::composer_row(agent, &on(door, sid, &["text"]))
}

/// What stands for `agent` on `text`, tab `sid`'s screen: [`crate::composer_turn::stand_of`], over
/// the composer row's `ctl cell` replies and, where a grey glyph asks it, the tab's `ctl cursor`.
/// Every gate here that asks whether an agent's composer stands empty before the lab types reads
/// it so, a placeholder no one typed read as empty.
fn stand_on(door: &Door<'_>, sid: &str, agent: &str, text: &str) -> Stand {
    crate::composer_turn::stand_of(agent, text, cell_on(door, sid), cursor_on(door, sid))
}

/// Tab `sid`'s caret, from its `ctl cursor` reply, where it answered with a settled frame's
/// ([`crate::composer_turn::caret_of`]).
fn cursor_on<'a>(door: &'a Door<'_>, sid: &'a str) -> impl FnOnce() -> Option<(usize, usize)> + 'a {
    move || {
        reading_on(door, sid, &["cursor"])
            .ok()
            .as_deref()
            .and_then(crate::composer_turn::caret_of)
    }
}

/// Tab `sid`'s `ctl cell <row> <col>` reply, where it answered.
fn cell_on<'a>(
    door: &'a Door<'_>,
    sid: &'a str,
) -> impl FnMut(usize, usize) -> Option<String> + 'a {
    move |row, col| reading_on(door, sid, &["cell", &row.to_string(), &col.to_string()]).ok()
}

/// A hang detector for a tab coming to its agent's empty composer ([`empty_composer`]): a
/// minute in which the tab's screen did not change, never a budget, since a starved agent may
/// take minutes to draw.
pub(crate) const COMPOSER_HANG: Duration = Duration::from_secs(60);

/// How a wait for an agent's empty composer ended ([`composer_wait`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposerWait {
    /// The composer stood empty.
    Ready,
    /// The composer held text before the lab typed anything: input no one gave reached the
    /// agent (a terminal's reply read as keys, say), a real failure, reported at once.
    Draft(String),
    /// An editor held the tab before the lab typed anything (its status row): input no one
    /// gave opened it, a real failure, reported at once.
    Editor(String),
    /// The screen stood unchanged [`COMPOSER_HANG`] with no empty composer: a hang.
    Hung,
}

impl ComposerWait {
    /// Why the wait failed, for `agent` (`None`: it did not).
    pub(crate) fn why(&self, agent: &str) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::Draft(text) => Some(format!(
                "{agent}'s composer held a draft no one typed before its prompt: {text:?}"
            )),
            Self::Editor(row) => Some(format!(
                "an editor held {agent}'s tab before its prompt, opened by no one's keys: {row:?}"
            )),
            Self::Hung => Some(format!(
                "{agent} never stood at an empty composer: its screen stood unchanged {} s",
                COMPOSER_HANG.as_secs()
            )),
        }
    }
}

/// What one look at a tab found ([`composer_wait`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stand {
    /// The agent's composer, its first row holding this text (empty: nothing in it).
    Composer(String),
    /// An editor holding the tab, its status row ([`crate::screen::editor_row`]).
    Editor(String),
    /// Neither: the agent is still starting.
    Starting,
    /// A composer holding text whose first glyph's cell said neither typed nor placeholder (no
    /// `ctl cell` reply, or an unfinished frame's): no evidence yet of what it holds
    /// ([`crate::composer_turn::stand_of`]).
    Unknown,
}

/// Looks at a tab until its composer stands empty: each `look` gives the screen (any value that
/// changes when the screen does) and what stands on it. A composer holding text or an editor
/// holding the tab ends it at once ([`ComposerWait::Draft`], [`ComposerWait::Editor`]); a screen
/// that stood unchanged `hang` with neither ends it as a hang; `pause` waits between looks and
/// says the time.
///
/// # Errors
/// The run was interrupted.
pub(crate) fn composer_wait<S: PartialEq>(
    mut look: impl FnMut() -> (S, Stand),
    hang: Duration,
    start: Instant,
    mut pause: impl FnMut() -> Result<Instant, stop::Stopped>,
) -> Result<ComposerWait, stop::Stopped> {
    let mut still: Option<(S, Instant)> = None;
    let mut now = start;
    loop {
        let (screen, stand) = look();
        match stand {
            Stand::Composer(text) if text.is_empty() => return Ok(ComposerWait::Ready),
            Stand::Composer(text) => return Ok(ComposerWait::Draft(text)),
            Stand::Editor(row) => return Ok(ComposerWait::Editor(row)),
            Stand::Starting | Stand::Unknown => {}
        }
        let since = match still.take() {
            Some((seen, since)) if seen == screen => {
                still = Some((seen, since));
                since
            }
            _ => {
                still = Some((screen, now));
                now
            }
        };
        if now.saturating_duration_since(since) >= hang {
            return Ok(ComposerWait::Hung);
        }
        now = pause()?;
    }
}

/// Waits for `agent`'s empty composer in tab `sid` ([`composer_wait`] over `ctl text`, read by
/// [`crate::composer_turn::stand_of`], looking every 100 ms).
///
/// # Errors
/// The run was interrupted.
pub(crate) fn empty_composer(
    door: &Door<'_>,
    sid: &str,
    agent: &str,
) -> Result<ComposerWait, Fail> {
    Ok(composer_wait(
        || {
            let text = on(door, sid, &["text"]);
            let stand = stand_on(door, sid, agent, &text);
            (text, stand)
        },
        COMPOSER_HANG,
        Instant::now(),
        || {
            stop::sleep(Duration::from_millis(100))?;
            Ok(Instant::now())
        },
    )?)
}

/// What `status` says of a tab's input: `input=` and `input_bytes=` (each `-` where the host does
/// not say).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Input {
    /// `input=`: `clear`, `pending`, `typeahead`, `stalled`, `stopped`, or `-`.
    pub(crate) word: String,
    /// `input_bytes=`.
    pub(crate) bytes: String,
    /// `input_wait_ms=`.
    pub(crate) wait_ms: String,
}

impl Input {
    /// The input `status` reply `stdout` names.
    pub(crate) fn of(stdout: &str) -> Self {
        let f = |key: &str| word(stdout, key).unwrap_or_else(|| "-".to_string());
        Self {
            word: f("input"),
            bytes: f("input_bytes"),
            wait_ms: f("input_wait_ms"),
        }
    }

    /// Whether the program has input it has not read: anything but `clear`, or `-` where the host
    /// does not measure it.
    pub(crate) fn unread(&self) -> bool {
        !matches!(self.word.as_str(), "clear" | "-")
    }

    /// `input=<w> input_bytes=<n> input_wait_ms=<ms>`.
    pub(crate) fn said(&self) -> String {
        format!(
            "input={} input_bytes={} input_wait_ms={}",
            self.word, self.bytes, self.wait_ms
        )
    }
}

/// How a tab's program came to have finished starting ([`started_composer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Started {
    /// How the wait ended: [`ComposerWait::Ready`] once it had.
    pub(crate) wait: ComposerWait,
    /// How long it took (ms).
    pub(crate) ms: i64,
    /// The last `status` read that found its input unread, where one did.
    pub(crate) held: Option<Input>,
    /// The last `status` read's input.
    pub(crate) last: Input,
    /// The program's capture, where a look took the tab's one ([`input_hold::Holds::due`]).
    pub(crate) capture: Option<input_hold::Capture>,
}

/// What one look at a starting tab's program stands at: what stands on its screen (`stand`) and
/// its input. A composer drawn empty over input the program has not read is still starting: the
/// lab's first prompt would wait behind that input, and the chain's clock with it.
fn start_stand(stand: Stand, input: &Input) -> Stand {
    match stand {
        Stand::Composer(row) if row.is_empty() && input.unread() => Stand::Starting,
        stand => stand,
    }
}

/// [`start_stand`] for `agent` on `text`, its screen, read as every empty-composer gate reads one
/// ([`crate::composer_turn::stand_of`] over `cell`, the composer row's `ctl cell` replies, and
/// `cursor`, the tab's caret).
fn start_look(
    agent: &str,
    text: &str,
    input: &Input,
    cell: impl FnMut(usize, usize) -> Option<String>,
    cursor: impl FnOnce() -> Option<(usize, usize)>,
) -> Stand {
    start_stand(
        crate::composer_turn::stand_of(agent, text, cell, cursor),
        input,
    )
}

/// THE LAB PROMPTS A PROGRAM ONLY ONCE IT HAS FINISHED STARTING (v6 §14.10.1, slice at1): its empty
/// composer drawn and `status input=clear`, every byte written to it read. Claude Code 2.1.316,
/// started beside more of itself, left even its terminal's replies unread 12 to 102 s while its
/// composer stood drawn (rv1-l2), and so the lab's first prompt and Land's first act behind it. A
/// hang detector over the screen and the input together ([`composer_wait`], [`COMPOSER_HANG`]): a
/// minute in which the program neither drew nor read anything ends it. A look that finds the
/// input unread past the run's threshold captures the program, once per tab, `label` naming its
/// files ([`input_hold`]); the wait goes on while the capture runs, and ends with it.
///
/// # Errors
/// The run was interrupted.
pub(crate) fn started_composer(
    door: &Door<'_>,
    sid: &str,
    agent: &str,
    (holds, label): (&mut input_hold::Holds, &str),
) -> Result<Started, Fail> {
    let from = now_ms();
    let mut held: Option<Input> = None;
    let mut last = Input::default();
    let mut taking: Option<input_hold::Taking> = None;
    let wait = composer_wait(
        || {
            let text = on(door, sid, &["text"]);
            let status = on(door, sid, &["status"]);
            let input = Input::of(&status);
            if input.unread() {
                held = Some(input.clone());
            }
            last.clone_from(&input);
            let stand = start_look(
                agent,
                &text,
                &input,
                cell_on(door, sid),
                cursor_on(door, sid),
            );
            let ready = matches!(&stand, Stand::Composer(row) if row.is_empty());
            if holds.due(sid, &input, ready) {
                taking = Some(holds.take(door, sid, label, &input, &status));
            }
            ((text, input.word, input.bytes), stand)
        },
        COMPOSER_HANG,
        Instant::now(),
        || {
            stop::sleep(Duration::from_millis(100))?;
            Ok(Instant::now())
        },
    );
    let ms = now_ms() - from;
    let capture = taking.map(input_hold::Taking::finish);
    Ok(Started {
        wait: wait?,
        ms,
        held,
        last,
        capture,
    })
}

/// Why a tab's start failed ([`started_composer`]; `None`: it did not), for `agent`: a hang over
/// input the program left unread says so, and the capture of the program names its files.
pub(crate) fn start_failed(s: &Started, agent: &str) -> Option<String> {
    let why = match &s.wait {
        ComposerWait::Hung if s.last.unread() => Some(format!(
            "{agent} drew and read nothing for {} s while it started, its input unread ({})",
            COMPOSER_HANG.as_secs(),
            s.last.said()
        )),
        w => w.why(agent),
    }?;
    Some(format!("{why}{}", captured(s)))
}

/// [`Started`] for the run's report: how long the start took, the input it left unread, and the
/// capture of the program, where a look took one.
pub(crate) fn started_line(s: &Started) -> String {
    format!(
        "{}{}{}",
        crate::restore::since(0, s.ms),
        s.held.as_ref().map_or_else(
            || ", its input read at every look".to_string(),
            |i| format!(", its input unread meanwhile (last: {})", i.said())
        ),
        captured(s)
    )
}

/// `; <the capture's words>` ([`input_hold::Capture::said`]), or nothing where none was taken.
fn captured(s: &Started) -> String {
    s.capture
        .as_ref()
        .map_or_else(String::new, |c| format!("; {}", c.said()))
}

/// Types `text` as `agent`'s turn in tab `sid` ([`crate::composer_turn::turn`]: only into its
/// empty composer, then Enter once the text stands there); whether the composer took it, the why
/// on stderr where it did not.
///
/// # Errors
/// The client could not run, or the run was interrupted.
pub(crate) fn tab_turn(door: &Door<'_>, sid: &str, agent: &str, text: &str) -> Result<bool, Fail> {
    let turned =
        crate::composer_turn::turn(&crate::composer_turn::Target::tab(door, sid, agent), text)?;
    if let Some(why) = turned.why(agent, text) {
        eprintln!("aterm-lab: turn @{sid}: {why}");
    }
    Ok(turned == crate::composer_turn::Turned::Taken)
}

fn person_turn(
    door: &Door<'_>,
    sid: &str,
    agent: &str,
    text: &str,
) -> Result<Option<(i64, String)>, Fail> {
    let at = format!("@{sid}");
    let refused = |verb: &str, r: Reply| {
        could_not(&format!(
            "`{verb} as=person` refused ({}{}): the person's turn needs the seam, a debug or \
             dev-seams aterm of this tree",
            r.stdout.trim(),
            r.stderr.trim()
        ))
    };
    let r = door.ctl(&[&at, "send", "as=person", text])?;
    if r.code != 0 {
        return Err(refused("send", r));
    }
    let standing = |door: &Door<'_>| at_composer(door, sid, agent).as_deref() == Some(text);
    let stood = crate::wait_until(Duration::from_secs(10), || standing(door));
    if !stood {
        if at_composer(door, sid, agent).is_some_and(|d| d.starts_with("as=person")) {
            return Err(could_not(
                "the aterm under test is older than `send|key as=person` (it typed the word into \
                 the composer): build this tree's (targo --unverified build -p aterm)",
            ));
        }
        return Ok(None);
    }
    stop::sleep(Duration::from_millis(PERSON_STAND_MS))?;
    for _ in 0..2 {
        let pressed = now_ms();
        let r = door.ctl(&[&at, "key", "as=person", "enter"])?;
        if r.code != 0 {
            return Err(refused("key", r));
        }
        for _ in 0..20 {
            stop::sleep(Duration::from_millis(250))?;
            if !standing(door) {
                let stamp = word(&on(door, sid, &["status"]), "human_ms")
                    .unwrap_or_else(|| "?".to_string());
                return Ok(Some((pressed, stamp)));
            }
        }
    }
    Ok(None)
}

fn person_keys(
    door: &Door<'_>,
    sid: &str,
    agent: &str,
    text: &str,
) -> Result<Option<(i64, String, Keyed)>, Fail> {
    if let Err(why) = crate::meta_keys::await_idle()? {
        return Err(could_not(&format!(
            "hwkey presses keys at an idle desk: {why}"
        )));
    }
    let raised = door.ctl(&["raise", sid])?;
    if raised.code != 0 {
        return Err(could_not(&format!(
            "raise {sid}: {}{}",
            raised.stdout.trim(),
            raised.stderr.trim()
        )));
    }
    let seq = |status: &str| word(status, "human_seq").and_then(|n| n.parse::<u64>().ok());
    let Some(seq_before) = seq(&on(door, sid, &["status"])) else {
        return Err(could_not(&format!(
            "status @{sid} says no human_seq= before the keys"
        )));
    };
    let mut keyed = Keyed {
        seq_before,
        ..Keyed::default()
    };
    let press = |key: &str, keyed: &mut Keyed| -> Result<(), Fail> {
        let r = door.ctl(&["hwkey", key])?;
        let said = r.stdout.trim();
        if !active_key_posted(&r) {
            return Err(could_not(&format!(
                "hwkey {key}: {said}{}: one key must be posted to the active key window; delivery was not measured",
                r.stderr.trim()
            )));
        }
        keyed.posted += 1;
        keyed.key_window += 1;
        Ok(())
    };
    let first = now_ms();
    for c in text.chars() {
        press(&hwkey_name(c), &mut keyed)?;
    }
    let last = now_ms();
    let standing = |door: &Door<'_>| at_composer(door, sid, agent).as_deref() == Some(text);
    if !crate::wait_until(DRAFT_HANG, || standing(door)) {
        let after = seq(&on(door, sid, &["status"]));
        println!(
            "  keys      @{sid}: no draft stood within {} s of {} key(s) posted by hwkey ({} to the key window); status human_seq= {} -> {}; the composer held {}",
            DRAFT_HANG.as_secs(),
            keyed.posted,
            keyed.key_window,
            keyed.seq_before,
            after.map_or_else(|| "-".to_string(), |a| a.to_string()),
            at_composer(door, sid, agent)
                .map_or_else(|| "no frame".to_string(), |c| format!("{c:?}"))
        );
        return Ok(None);
    }
    keyed.draft_stood = true;
    println!(
        "  keys      @{sid}: {} character(s) pressed through hwkey in {}; the draft stood {} after the last",
        keyed.posted,
        at(first, last),
        at(last, now_ms())
    );
    stop::sleep(Duration::from_millis(PERSON_STAND_MS))?;
    let pressed = now_ms();
    press("enter", &mut keyed)?;
    if !crate::wait_until(DRAFT_HANG, || {
        at_composer(door, sid, agent).is_some_and(|draft| draft != text)
    }) {
        return Ok(None);
    }
    keyed.draft_taken = true;
    let status = on(door, sid, &["status"]);
    keyed.seq_after = seq(&status);
    if let Some(why) = cover::keys_unaccounted(&keyed) {
        return Err(could_not(&format!("native controller delivery: {why}")));
    }
    let stamp = word(&status, "human_ms").unwrap_or_else(|| "?".to_string());
    Ok(Some((pressed, stamp, keyed)))
}

/// A single native key was posted to the active app's key window, not merely queued elsewhere.
fn active_key_posted(reply: &Reply) -> bool {
    reply.code == 0
        && reply
            .stdout
            .split_whitespace()
            .eq(["OK", "posted=1", "key=1"])
}

/// The `hwkey` name of the key that types `c`: `space` for a space (the verb splits its words on
/// it), the character itself otherwise.
#[must_use]
pub fn hwkey_name(c: char) -> String {
    if c == ' ' {
        "space".to_string()
    } else {
        c.to_string()
    }
}

type Awaited = Arc<Mutex<Option<(i64, i32, String)>>>;

fn worker_shell(run: &Run, h: Harness) -> Result<Option<String>, Fail> {
    let put = |p: &Path, mode: u32, text: &str| {
        crate::sys::put(p, mode, &crate::sys::PutSrc::Bytes(text.as_bytes()))
            .map_err(|e| could_not(&format!("{}: {e}", p.display())))
    };
    let pkg = run
        .w
        .join("home/Library/Application Support/aterm/pkg/agents");
    let (rc, laid) = match h {
        Harness::Claude => return Ok(None),
        Harness::Agy => {
            let twin = pkg.join("agy");
            put(
                &twin,
                0o755,
                &crate::relaunch_agy::twin_script(&run.agy_bin),
            )?;
            (
                crate::relaunch_agy::shell_exports(run),
                format!("{} -> {}", twin.display(), run.agy_bin),
            )
        }
        Harness::Muse => {
            let twin = pkg.join("muse");
            put(
                &twin,
                0o755,
                &crate::relaunch_muse::twin_script(&run.muse_bin),
            )?;
            (
                crate::relaunch_muse::shell_exports(run),
                format!("{} -> {}", twin.display(), run.muse_bin),
            )
        }
    };
    put(&run.w.join("home/.zshrc"), 0o644, &rc)?;
    Ok(Some(laid))
}

fn spawn_worker(
    (tag, run, h): (&str, &Run, Harness),
    door: &Door<'_>,
    tabs: &[Tab],
    wdir: &Path,
) -> Result<(Tab, Awaited), Fail> {
    let mgr = tabs
        .iter()
        .find(|t| t.chain == Chain::Mgr)
        .map(|t| t.sid.clone())
        .ok_or_else(|| could_not("no manager tab for the worker's spawn"))?;
    if let Some(laid) = worker_shell(run, h)? {
        println!(
            "  shell     the worker's: {}/home/.zshrc exports {}'s provider at the stub; {laid}",
            run.w.display(),
            h.name()
        );
    }
    let task = Chain::Worker.prompt();
    let (at, agent, from, cwd) = (
        format!("@{mgr}"),
        format!("agent={}", h.name()),
        format!("from={mgr}"),
        format!("cwd={}", wdir.display()),
    );
    let r = door.ctl_input(&[&at, "spawn", &agent, &from, &cwd, "-"], task.as_bytes())?;
    let reply = r.stdout.lines().next().unwrap_or("").trim().to_string();
    let sid = reply
        .strip_prefix("OK ")
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| could_not(&format!("spawn {agent}: {}{}", r.stdout, r.stderr)))?;
    let t0 = now_ms();
    println!(
        "  worker    {} @{sid} in {}: spawned {agent} from @{mgr} ({reply}), its task \"{task}\"",
        Chain::Worker.name(),
        wdir.display()
    );
    let slot: Awaited = Arc::default();
    let held = Arc::clone(&slot);
    let (tag, w) = (tag.to_string(), format!("@{sid}"));
    std::thread::spawn(move || {
        let answer = match Run::load(&tag) {
            Ok(run) => match Door::new(&run) {
                Ok(door) => match door.ctl(&["await", "task", &w, "since=0"]) {
                    Ok(r) => (r.code, format!("{}{}", r.stdout, r.stderr)),
                    Err(e) => (-1, e),
                },
                Err(e) => (-1, e),
            },
            Err(e) => (-1, e),
        };
        *held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((now_ms(), answer.0, answer.1));
    });
    let tab = Tab {
        chain: Chain::Worker,
        sid,
        dir: wdir.to_path_buf(),
        seen: Seen {
            cover: Cover {
                starter: Some(mgr),
                spawn: Some(reply),
                agent: Some(h.name().to_string()),
                ..Cover::default()
            },
            ..Seen::default()
        },
        journal_cursor: crate::util::LineTailCursor::default(),
        ledger_cursor: crate::util::LineTailCursor::default(),
        t0,
        over: false,
        ask_tried: false,
        gate_opened: false,
    };
    Ok((tab, slot))
}

/// The order a run opens its tabs in, as indexes into `chains`: as listed, but cruise-n first
/// where the window is read (`read`), so its rows (N1–N4's page state) stand at the page's top.
/// A spawned chain is listed in its place and opened by its manager.
fn opening_order(chains: &[Chain], read: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..chains.len()).collect();
    if read && let Some(i) = chains.iter().position(|c| *c == Chain::CruiseN) {
        order.remove(i);
        order.insert(0, i);
    }
    order
}

fn orchestrator_due(chain: Chain, seen: &Seen, now: i64) -> bool {
    if seen.cover.turn_ms.is_some() {
        return false;
    }
    match chain {
        Chain::LandP => seen
            .cover
            .person_ms
            .is_some_and(|p| now - p >= EXCHANGE_WATCH_MS),
        Chain::Cap => seen
            .all_acts()
            .first()
            .is_some_and(|(t1, _)| now - t1 >= cap_window_ms() + PAST_WINDOW_MS),
        Chain::SameWords => seen
            .cover
            .resized
            .as_ref()
            .is_some_and(|(r, _)| now - r >= TURN_AFTER_RESIZE_MS),
        _ => false,
    }
}

fn resize_step(door: &Door<'_>, tab: &mut Tab, now: i64) -> Result<(), Fail> {
    if tab.chain != Chain::SameWords || tab.seen.cover.resized.is_some() {
        return Ok(());
    }
    let Some((tw, _)) = tab
        .seen
        .journal
        .iter()
        .find(|(t, l)| *t > tab.t0 && backoff_named(l))
    else {
        return Ok(());
    };
    if now - tw < RESIZE_AFTER_MS || !tab.seen.all_acts().is_empty() {
        return Ok(());
    }
    let (cols, rows) = RESIZED;
    let who = format!("@{}", tab.sid);
    let r = door.ctl(&[&who, "resize", &rows.to_string(), &cols.to_string()])?;
    let dims = on(door, &tab.sid, &["dims"]);
    let t = now_ms();
    let said = format!(
        "resize {rows} {cols}: {}{} (exit {}); dims {}",
        r.stdout.trim(),
        r.stderr.trim(),
        r.code,
        crate::util::one_line(dims.trim(), 160)
    );
    println!(
        "  [{}] +{} resize  {}",
        tab.chain.name(),
        at(tab.t0, t),
        crate::util::one_line(&said, 220)
    );
    tab.seen.cover.resized = Some((t, said));
    Ok(())
}

/// `sid`'s screen rows, blank rows left out, each cut to 150 characters; `Err` where the `text`
/// read failed ([`reading`]), which no empty screen stands in for.
fn shown_rows(door: &Door<'_>, sid: &str) -> Result<Vec<String>, String> {
    Ok(reading_on(door, sid, &["text"])?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take(150).collect())
        .collect())
}

fn scroll_step(door: &Door<'_>, tab: &mut Tab, now: i64) -> Result<(), Fail> {
    if tab.chain != Chain::TallScroll {
        return Ok(());
    }
    let token = tab.chain.token();
    let in_sight = |rows: &[String]| rows.iter().any(|r| r.contains(token.as_str()));
    if let Some(sc) = tab.seen.cover.scrolled.as_mut() {
        if sc.viewport.is_none()
            && sc.held.is_none()
            && now - sc.at >= SCROLL_HELD_MS
            && let Ok(rows) = shown_rows(door, &tab.sid)
        {
            sc.held = Some(in_sight(&rows));
            println!(
                "  [{}] +{} scroll  {} s after the last notch: the opener {}",
                tab.chain.name(),
                at(tab.t0, now_ms()),
                SCROLL_HELD_MS / 1000,
                if in_sight(&rows) {
                    "still in sight"
                } else {
                    "out of sight"
                }
            );
        }
        return Ok(());
    }
    let Some((tw, _)) = tab
        .seen
        .journal
        .iter()
        .find(|(t, l)| *t > tab.t0 && backoff_named(l))
    else {
        return Ok(());
    };
    if now - tw < SCROLL_AFTER_MS || !tab.seen.all_acts().is_empty() {
        return Ok(());
    }
    let who = format!("@{}", tab.sid);
    // What the verdict counts from (whether the agent takes the wheel, whether the opener was in
    // sight before it) is read before any notch: where a read failed, no notch is turned and the
    // next look reads again.
    let base = reading_on(door, &tab.sid, &["modes"]).and_then(|m| {
        let history =
            reading_on(door, &tab.sid, &["lines"]).unwrap_or_else(|e| format!("unread ({e})"));
        Ok((m, history, shown_rows(door, &tab.sid)?))
    });
    let (modes, history, first) = match base {
        Ok(b) => b,
        Err(e) => {
            println!(
                "  [{}] +{} scroll  no notch turned, what it counts from unread: {}",
                tab.chain.name(),
                at(tab.t0, now_ms()),
                crate::util::one_line(&e, 220)
            );
            return Ok(());
        }
    };
    let modes: Vec<String> = modes
        .lines()
        .filter(|l| l.starts_with("alt_screen=") || l.starts_with("mouse_mode="))
        .map(str::to_string)
        .collect();
    let tracks = !modes.iter().any(|m| m == "mouse_mode=none");
    let history = crate::util::one_line(history.trim(), 80);
    let before = in_sight(&first);
    let mut rows = first.clone();
    let (mut notches, mut still, mut refused) = (0, 0, None);
    let from = now_ms();
    let mut wheel = |notches: &mut u32| -> Result<bool, Fail> {
        let r = door.ctl(&[&who, "mouse", "wheelup", "left", "12", "20", "lines=1"])?;
        *notches += 1;
        if r.stdout.starts_with("OK") {
            std::thread::sleep(std::time::Duration::from_millis(150));
            return Ok(true);
        }
        refused = Some(crate::util::one_line(
            &format!("{}{}", r.stdout.trim(), r.stderr.trim()),
            160,
        ));
        Ok(false)
    };
    let (after, viewport) = if tracks || before {
        while !in_sight(&rows) && notches < SCROLL_NOTCHES && still < SCROLL_STILL {
            if !wheel(&mut notches)? {
                break;
            }
            // A failed read leaves the last screen read standing: the next notch reads again.
            let Ok(next) = shown_rows(door, &tab.sid) else {
                continue;
            };
            still = if next == rows { still + 1 } else { 0 };
            rows = next;
        }
        (in_sight(&rows), None)
    } else {
        let (opener, screen_first) = opener_row(door, &tab.sid, &token);
        let height = on(door, &tab.sid, &["dims"])
            .split_whitespace()
            .nth(1)
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        let mut place = on(door, &tab.sid, &["scroll"]);
        loop {
            let (off, max) = scroll_place(&place);
            let top = screen_first.saturating_sub(off);
            let shown = opener.is_some_and(|r| r >= top && r < top + height);
            if shown || off >= max || notches >= SCROLL_NOTCHES || !wheel(&mut notches)? {
                let unread = match shown_rows(door, &tab.sid) {
                    Ok(r) => {
                        rows = r;
                        String::new()
                    }
                    Err(e) => format!("; the live screen unread after it ({e})"),
                };
                let said = format!(
                    "scroll {}; the opener {}, the viewport rows {top}..{}, the live screen's {screen_first}..{}{unread}",
                    crate::util::one_line(place.trim(), 40),
                    opener.map_or_else(|| "found nowhere".to_string(), |r| format!("at row {r}")),
                    top + height,
                    screen_first + height
                );
                break (shown, Some(said));
            }
            place = on(door, &tab.sid, &["scroll"]);
        }
    };
    let t = now_ms();
    let sc = Scrolled {
        from,
        at: t,
        notches,
        refused,
        modes: modes.join(" "),
        history,
        before,
        after,
        moved: rows != first,
        viewport,
        rows,
        held: None,
    };
    println!(
        "  [{}] +{} scroll  {} notch(es) up from +{} ({}; lines {}){}: the opener in sight before {}, after {}{}; the live screen {}:",
        tab.chain.name(),
        at(tab.t0, t),
        sc.notches,
        at(tab.t0, sc.from),
        sc.modes,
        sc.history,
        sc.refused
            .as_deref()
            .map_or_else(String::new, |r| format!(", refused: {r}")),
        sc.before,
        sc.after,
        sc.viewport
            .as_deref()
            .map_or_else(String::new, |v| format!(" (the tab's viewport: {v})")),
        if sc.moved { "moved" } else { "unmoved" }
    );
    for l in &sc.rows {
        println!("    | {l}");
    }
    tab.seen.cover.scrolled = Some(sc);
    Ok(())
}

fn opener_row(door: &Door<'_>, sid: &str, token: &str) -> (Option<u64>, u64) {
    door.ctl(&[&format!("@{sid}"), "search", token])
        .map_or((None, 0), |r| search_rows(&r))
}

fn search_rows(r: &Reply) -> (Option<u64>, u64) {
    let rows = r.stdout.as_str();
    let Some(head) = r
        .stderr
        .lines()
        .map(|l| l.strip_prefix("aterm-ctl: ").unwrap_or(l))
        .find(|l| l.starts_with("OK "))
    else {
        return (None, 0);
    };
    let first = head
        .split_whitespace()
        .find_map(|w| w.strip_prefix("screen_first="))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let row = rows
        .lines()
        .filter_map(|l| l.split_whitespace().next()?.parse::<u64>().ok())
        .max();
    (row, first)
}

fn scroll_place(reply: &str) -> (u64, u64) {
    let mut w = reply.split_whitespace();
    if w.next() != Some("OK") {
        return (0, 0);
    }
    let mut n = || w.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let off = n();
    (off, n())
}

fn first_screen_step(door: &Door<'_>, tab: &mut Tab) {
    if tab.chain != Chain::ToolEnd || tab.seen.cover.first_screen.is_some() {
        return;
    }
    let pointed = tab
        .seen
        .ledger
        .iter()
        .any(|r| r.rule == RULE_POINT && r.ts >= tab.t0);
    if !pointed {
        return;
    }
    // The whole screen, so the first turn's rows from the opener's echo down are in it
    // ([`cover::first_turn`]), and in the run's log.
    let rows = tail_rows(door, &tab.sid, usize::MAX);
    let t = now_ms();
    println!(
        "  [{}] +{} screen  at the first point:",
        tab.chain.name(),
        at(tab.t0, t)
    );
    for l in &rows {
        println!("    | {l}");
    }
    tab.seen.cover.first_screen = Some((t, rows));
}

/// L20 and C5: the tool results the conversation's requests carried back (the stub's dumps), each
/// once, in order.
fn dialog_answers_of(run: &Run, token: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for body in crate::stub::turn_dumps(&run.w.join("dump"))
        .into_iter()
        .filter_map(|(_, p)| crate::util::read_regular_file(&p).ok())
        .filter(|body| body.windows(token.len()).any(|w| w == token.as_bytes()))
    {
        for r in crate::stub::message_results(&body) {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

fn tool_results_of(run: &Run, token: &str) -> usize {
    crate::stub::turn_dumps(&run.w.join("dump"))
        .into_iter()
        .filter_map(|(_, p)| crate::util::read_regular_file(&p).ok())
        .filter(|body| String::from_utf8_lossy(body).contains(token))
        .filter(|body| {
            crate::stub::messages_answered(body)
                || crate::stub::gemini::results_answered(body)
                || crate::stub::muse::results_answered(body)
        })
        .count()
}

fn parse_journal_lines(text: &str) -> impl Iterator<Item = (i64, String)> + '_ {
    text.lines().filter_map(|l| {
        let j = J::parse(l).ok()?;
        let t = j.get("t").and_then(J::as_i64)?;
        Some((t, j.get("line").and_then(J::as_str)?.to_string()))
    })
}

fn parse_ledger_lines(text: &str) -> impl Iterator<Item = Row> + '_ {
    text.lines().filter_map(|l| {
        let j = J::parse(l).ok()?;
        let s = |k: &str| j.get(k).and_then(J::as_str).unwrap_or("").to_string();
        Some(Row {
            ts: j.get("ts").and_then(J::as_i64)?,
            box_seq: j
                .get("box_seq")
                .and_then(J::as_i64)
                .and_then(|seq| u64::try_from(seq).ok()),
            rule: s("rule_id"),
            decision: s("decision"),
            command: s("command"),
            reason: s("reason"),
        })
    })
}

pub(crate) fn records(run: &Run, sid: &str) -> (Vec<(i64, String)>, Vec<Row>) {
    let drive = run.state().join("drive");
    let journal = parse_journal_lines(&crate::util::scratch_text(
        &drive.join(format!("{sid}.journal.jsonl")),
    ))
    .collect();
    let ledger = parse_ledger_lines(&crate::util::scratch_text(
        &drive.join(format!("{sid}.jsonl")),
    ))
    .collect();
    (journal, ledger)
}

/// The words of `sid`'s `status` the judges read (`?` for one it does not name); `None` where the
/// read failed ([`reading`]).
fn status_words(door: &Door<'_>, sid: &str) -> Option<String> {
    let s = reading_on(door, sid, &["status"]).ok()?;
    let words = [
        "task",
        "needs",
        "needs_n",
        "autopilot",
        "supervisor",
        "agent",
        "end",
        "end_seen",
        "end_cheered",
        "land_held",
        "land_nudges",
        "land_since",
        "held",
        "waiting",
        "land_asked",
        "landed_head",
        "landed_on",
        "landed_leg",
        "landed_repo",
    ]
    .iter()
    .map(|k| format!("{k}={}", word(&s, k).unwrap_or_else(|| "?".into())))
    .collect::<Vec<_>>()
    .join(" ");
    Some(words)
}

/// `sid`'s `meta` attention words, decoded (`?` for one it does not name); `None` where the read
/// failed ([`reading`]).
fn meta_words(door: &Door<'_>, sid: &str) -> Option<String> {
    let m = reading_on(door, sid, &["meta"]).ok()?;
    let w = |k: &str| word(&m, k).unwrap_or_else(|| "?".into());
    Some(format!(
        "attention_owners={} attention_kinds={} attention={}",
        pct_decode(&w("attention_owners")),
        pct_decode(&w("attention_kinds")),
        pct_decode(&w("attention"))
    ))
}

const MARK: &str = "[aterm harness]";

/// Whether a user text is the harness's: its mark, or one of its answers (`Nobody is here to …`:
/// Land's answer, the reversible answer and the answer text carry no mark).
#[must_use]
pub fn harness_said(text: &str) -> bool {
    text.contains(MARK) || text.contains(ANSWER_HEAD)
}

/// The harness's user texts a request `body` of conversation `token` holds ([`harness_said`], by
/// the reader of whichever of the three APIs it speaks); `None` for a request of another
/// conversation.
#[must_use]
pub fn marked_turns(body: &[u8], token: &str) -> Option<usize> {
    if !String::from_utf8_lossy(body).contains(token) {
        return None;
    }
    let marked = |users: Vec<String>| users.iter().filter(|u| harness_said(u)).count();
    [
        marked(crate::stub::message_users(body)),
        marked(crate::stub::responses_users(body)),
        marked(crate::stub::gemini::gemini_users(body)),
    ]
    .into_iter()
    .max()
}

/// Claude Code's own continuation of a usage limit, as its request carries it: the opening words
/// of the user text it sends when its countdown fires at the reset (the transcript's
/// `origin.kind` `auto-continuation` prompt: `Your claude.ai usage limit has reset. Continue the
/// task you were working on when the limit was reached; do not repeat work that is already
/// complete.`).
pub const CLAUDE_CONTINUES: &str = "Your claude.ai usage limit has reset. Continue the task";

/// Whether request `body` of conversation `token` carries the program's own continuation of a usage
/// limit: one of its user texts holds [`CLAUDE_CONTINUES`] (a Messages API body: Claude Code's).
#[must_use]
pub fn carries_continuation(body: &[u8], token: &str) -> bool {
    String::from_utf8_lossy(body).contains(token)
        && crate::stub::message_users(body)
            .iter()
            .any(|u| u.contains(CLAUDE_CONTINUES))
}

fn dumps(run: &Run) -> Vec<(i64, Vec<u8>)> {
    dumps_in(&run.w.join("dump"))
}

/// The stub's dumps of the conversations' turns in `dir`, `(modified ms, body)` each:
/// `req-<n>.json` alone ([`crate::stub::turn_dumps`]), never a side call's `side-<n>.json`. Muse
/// Code's reminder observer renders the whole conversation, line breaks and all, into one side-call
/// text, which C4 would read as the owner's words reaching the agent.
#[must_use]
pub fn dumps_in(dir: &Path) -> Vec<(i64, Vec<u8>)> {
    crate::stub::turn_dumps(dir)
        .into_iter()
        .filter_map(|(_, p)| {
            let (body, meta) = crate::util::read_regular_file_with_meta(&p).ok()?;
            let t = meta
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            let t = i64::try_from(t.as_millis()).ok()?;
            Some((t, body))
        })
        .collect()
}

fn dumps_of(run: &Run, token: &str) -> Vec<(i64, usize)> {
    dumps(run)
        .into_iter()
        .filter_map(|(t, body)| Some((t, marked_turns(&body, token)?)))
        .collect()
}

#[must_use]
fn first_continuation(dumps: &[(i64, Vec<u8>)], token: &str, since: i64) -> Option<i64> {
    dumps
        .iter()
        .filter(|(t, body)| *t >= since && carries_continuation(body, token))
        .map(|(t, _)| *t)
        .min()
}

fn went_on_witness(run: &Run, tab: &Tab) -> Option<i64> {
    if !tab.chain.walled() {
        return None;
    }
    let (_, reset_s) = tab.seen.cover.wall?;
    let since = tab.seen.cover.lifted_ms.unwrap_or(reset_s * 1000);
    first_continuation(&dumps(run), &tab.chain.token(), since)
}

/// C4's witness of what reached the agent ([`Cover::owner_texts`]): every user text of conversation
/// `token`'s requests holding [`OWNER_KEY`], by the reader of whichever of the three APIs it
/// speaks, each once.
#[must_use]
pub fn owner_texts(dumps: &[Vec<u8>], token: &str) -> Vec<String> {
    texts_holding(dumps, token, OWNER_KEY)
}

/// Every user text of conversation `token`'s requests holding `key`, by the reader of whichever of
/// the three APIs it speaks, each once.
#[must_use]
pub fn texts_holding(dumps: &[Vec<u8>], token: &str, key: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for body in dumps {
        if !String::from_utf8_lossy(body).contains(token) {
            continue;
        }
        for u in crate::stub::message_users(body)
            .into_iter()
            .chain(crate::stub::responses_users(body))
            .chain(crate::stub::gemini::gemini_users(body))
        {
            if u.contains(key) && !out.contains(&u) {
                out.push(u);
            }
        }
    }
    out
}

/// LH's witness of the hinted check as the agent sent it ([`Cover::check_texts`]).
fn check_witness(run: &Run, tab: &Tab) -> Option<Vec<String>> {
    (tab.chain == Chain::LandHinted).then(|| {
        let bodies: Vec<Vec<u8>> = dumps(run).into_iter().map(|(_, b)| b).collect();
        texts_holding(&bodies, &tab.chain.token(), LAND_CHECK_KEY)
    })
}

fn owner_witness(run: &Run, tab: &Tab) -> Option<Vec<String>> {
    (tab.chain == Chain::CruiseO).then(|| {
        let bodies: Vec<Vec<u8>> = dumps(run).into_iter().map(|(_, b)| b).collect();
        owner_texts(&bodies, &tab.chain.token())
    })
}

impl StubSeen {
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
        .filter(|r| r.acted() && r.ts <= *d && harness_said(&r.command))
        .count();
    Some(StubSeen::of(&dumps_of(run, &token), *d, typed))
}

impl StubSeen {
    fn within(dumps: &[(i64, usize)], since: i64, until: i64, typed_before: usize) -> StubSeen {
        let inside: Vec<(i64, usize)> =
            dumps.iter().copied().filter(|(t, _)| *t <= until).collect();
        StubSeen::of(&inside, since, typed_before)
    }
}

/// THE LIFT'S WITNESS ([`Seen::lift_stub`]): the requests of a same-reply chain's conversation the
/// stub read after its hold and by the hold's lift ([`surfaces::held::lift_of`]); `None` where it
/// was not lifted.
fn lift_witness(run: &Run, tab: &Tab) -> Option<usize> {
    if !tab.chain.same_reply() {
        return None;
    }
    let (ts, _) = surfaces::held::stall_of(&tab.seen, tab.t0)?;
    let (tl, _) = surfaces::held::lift_of(&tab.seen, *ts)?;
    Some(surfaces::held::requests_between(
        &dumps_of(run, &tab.chain.token()),
        *ts,
        *tl,
    ))
}

fn window_witness(run: &Run, tab: &Tab) -> Option<StubSeen> {
    let token = tab.chain.token();
    let typed_by = |t: i64| {
        tab.seen
            .ledger
            .iter()
            .filter(|r| r.acted() && r.ts <= t && harness_said(&r.command))
            .count()
    };
    match tab.chain {
        Chain::LandP => {
            let until = tab.seen.cover.turn_ms?;
            Some(StubSeen::within(&dumps_of(run, &token), 0, until, 0))
        }
        Chain::Cap => {
            let until = tab.seen.cover.turn_ms?;
            let (t1, _) = tab.seen.all_acts().first().copied()?;
            let typed = usize::from(tab.seen.ledger.iter().any(|r| r.decision == "typed"));
            Some(StubSeen::within(&dumps_of(run, &token), *t1, until, typed))
        }
        // To the due of the back-off at the person's reply: the act there is Cruise's own.
        Chain::InstantPerson => {
            let asked = tab.seen.asked_ms?;
            let until = own_backoff_after(&tab.seen, asked)
                .and_then(|(_, l)| backoff_due(l))
                .map_or(tab.seen.end_ms, |due| due.min(tab.seen.end_ms));
            Some(StubSeen::within(
                &dumps_of(run, &token),
                asked,
                until,
                typed_by(asked),
            ))
        }
        c if c.walled() => {
            let until = wall_window_end(c, &tab.seen.cover)?;
            let t1 = if c == Chain::CruiseWall {
                walls::cruise_met(&tab.seen)?
            } else {
                tab.seen.acts(turn_end::RULE_LAND).first()?.0
            };
            Some(StubSeen::within(
                &dumps_of(run, &token),
                t1,
                until,
                typed_by(t1).max(1),
            ))
        }
        _ => None,
    }
}

/// The end (ms) of the walled chain's window no harness text may reach the stub in
/// ([`Cover::window_stub`]), from the land act that met its wall: the wall's reset (W1, W2, W4's
/// first wall: the resume past it meets the redraw), the first agent's exit (W5: the second's conversation is its own; W9: the
/// agent launched again opens a new one, judged by its acts), the conversation read again (W6: held
/// unread past the reset).
#[must_use]
pub fn wall_window_end(chain: Chain, cover: &Cover) -> Option<i64> {
    let (_, reset_s) = cover.wall?;
    Some(match chain {
        Chain::SwitchWall => cover.switch.as_ref()?.exited_ms,
        Chain::RelaunchWall => cover
            .relaunch
            .as_ref()
            .map_or(reset_s * 1000, |r| r.exited_ms),
        Chain::UnreadWall => cover
            .unread
            .as_ref()
            .and_then(|u| u.restored_ms)
            .unwrap_or(reset_s * 1000),
        _ => reset_s * 1000,
    })
}

/// What the lab does to the stub's raised wall at `now` ([`wall_step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallMove {
    /// Nothing yet.
    Hold,
    /// Raise it again, resetting at this unix second ([`Chain::shifts`]).
    Shift(i64),
    /// Give the stub its scripted mode back.
    Lift,
}

/// What the lab does to chain `chain`'s raised wall at `now`: nothing until [`WALL_LIFT_EARLY_MS`]
/// before the reset now standing ([`Cover::reset_now`]); then raised again [`Chain::shift_step_ms`]
/// on while the chain has shifts left ([`Chain::shifts`]: the try or the resume past that reset
/// meets the new one), else lifted. W5's is never lifted by time: its switch lifts it
/// ([`switch_step`]). Nothing once lifted, or before the wall was raised.
#[must_use]
pub fn wall_move(chain: Chain, cover: &Cover, now: i64) -> WallMove {
    let Some(reset) = cover.reset_now() else {
        return WallMove::Hold;
    };
    if cover.lifted_ms.is_some()
        || chain == Chain::SwitchWall
        || now < reset * 1000 - WALL_LIFT_EARLY_MS
    {
        return WallMove::Hold;
    }
    if cover.shifted.len() < chain.shifts() {
        WallMove::Shift(reset + chain.shift_step_ms() / 1000)
    } else {
        WallMove::Lift
    }
}

/// Whether a point was read from `t0` on: an idle event with words.
#[must_use]
pub fn point_read(seen: &Seen, t0: i64) -> bool {
    seen.journal
        .iter()
        .any(|(t, l)| *t > t0 && idle_words(l).is_some_and(|w| !w.is_empty() && w != "-"))
}

/// Whether the chain's first point from `t0` was read and nothing was typed yet: the outage's
/// eligibility ([`outage::outage_move`]); a prearmed wall does not wait for this observation.
#[must_use]
pub fn first_point_unanswered(seen: &Seen, t0: i64) -> bool {
    point_read(seen, t0) && seen.all_acts().is_empty()
}

/// Whether the observer still needs the prearmed wall's admission evidence. A POINT and
/// immediate ACT already in the same read cannot suppress that observation.
fn awaits_first_raise(chain: Chain, seen: &Seen, _t0: i64) -> bool {
    chain.walled() && seen.cover.wall.is_none()
}

fn armed_wall(status: &J) -> Option<(i64, i64)> {
    let arm = status.get("limit_arm")?;
    let raised = arm.get("raised_ms")?.as_i64()?;
    let reset = arm.get("reset_s")?.as_i64()?;
    (raised > 0 && reset.saturating_mul(1000) > raised).then_some((raised, reset))
}

fn wall_step(run: &Run, tab: &mut Tab, now: i64) -> Result<(), Fail> {
    if !tab.chain.walled() {
        return Ok(());
    }
    match tab.seen.cover.wall {
        None => {
            if !awaits_first_raise(tab.chain, &tab.seen, tab.t0) {
                return Ok(());
            }
            let answer = crate::stub::control(&run.port, "/__status")
                .map_err(|e| could_not(&format!("the prearmed wall status: {e}")))?;
            let status = J::parse(&answer)
                .map_err(|e| could_not(&format!("the prearmed wall status: {e}")))?;
            let Some((raised, reset)) = armed_wall(&status) else {
                return Ok(());
            };
            let claim = WALL_CLAIM;
            tab.seen.cover.wall = Some((raised, reset));
            println!(
                "  [{}] +{} stub    the usage wall raised: {claim}, resets at +{} ({})",
                tab.chain.name(),
                at(tab.t0, raised),
                at(tab.t0, reset * 1000),
                crate::util::one_line(&answer, 120)
            );
        }
        Some(_) => match wall_move(tab.chain, &tab.seen.cover, now) {
            WallMove::Hold => {}
            WallMove::Shift(reset) => {
                let claim = WALL_CLAIM;
                let answer = crate::verbs::set_mode(run, &format!("limit:{claim}:{reset}"))?;
                tab.seen.cover.shifted.push((now, reset));
                println!(
                    "  [{}] +{} stub    the wall raised again at its reset ({} of {}): {claim}, resets at +{} ({})",
                    tab.chain.name(),
                    at(tab.t0, now),
                    tab.seen.cover.shifted.len(),
                    tab.chain.shifts(),
                    at(tab.t0, reset * 1000),
                    crate::util::one_line(&answer, 120)
                );
            }
            WallMove::Lift => {
                let answer = crate::verbs::set_mode(run, MODE)?;
                tab.seen.cover.lifted_ms = Some(now);
                println!(
                    "  [{}] +{} stub    the wall lifted: mode {MODE} again ({})",
                    tab.chain.name(),
                    at(tab.t0, now),
                    crate::util::one_line(&answer, 120)
                );
            }
        },
    }
    Ok(())
}

fn waits_on_the_wall(s: &Seen) -> bool {
    let Some((t1, _)) = s.acts(turn_end::RULE_LAND).first().copied() else {
        return false;
    };
    let points: Vec<Option<String>> = s
        .journal
        .iter()
        .filter(|(t, l)| *t > *t1 && cover::wall_point(l))
        .map(|(_, l)| word(l, "seq"))
        .collect();
    s.journal.iter().any(|(t, l)| {
        *t > *t1
            && l.starts_with("WAITING ")
            && !l.contains(" the worker to take the ")
            && points.contains(&word(l, "seq"))
    })
}

/// Whether harness `h`'s screen `text` no longer draws the wall's notice: its composer drawn, no
/// row holding any of the wall's words ([`Hide::words`]), and its reader reading no wall.
#[must_use]
pub fn notice_gone(h: Harness, text: &str) -> bool {
    let Ok(hide) = hide_of(h) else {
        return false;
    };
    let rows: Vec<String> = text.lines().map(str::to_string).collect();
    let (composer, wall) = match h {
        Harness::Muse => (
            aterm_phase::muse::composer_draft(&rows).is_some(),
            aterm_phase::muse::wall(&rows).is_some(),
        ),
        Harness::Agy => (
            aterm_phase::agy::composer_draft(&rows).is_some(),
            aterm_phase::agy::wall(&rows).is_some(),
        ),
        Harness::Claude => (
            claude_composer(&rows).is_some(),
            aterm_phase::read(None, &rows, None).wall.is_some(),
        ),
    };
    composer && !wall && !hide.words.iter().any(|w| text.contains(w))
}

/// Claude Code's composer's first row (its `❯` taken off), between the composer's rules as its
/// reader finds them (`aterm_phase::phase::composer_rules`, which takes a rule carrying a label:
/// `──── forever-w2 ─` once `/rename` named the session), or `None` where no frame is drawn. Not
/// `screen::composer_row`'s Claude reading, which also takes agy's `>` frame for a composer.
#[must_use]
pub fn claude_composer(rows: &[String]) -> Option<String> {
    let (top, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let first = rows.get(top + 1..bottom)?.first()?;
    let first = first.replace('\u{a0}', " ");
    Some(first.strip_prefix('❯').unwrap_or(&first).trim().to_string())
}

/// Whether harness `h`'s composer holds `cmd` as typed and its command menu's first row is that
/// very command, so Enter runs it.
#[must_use]
pub fn menu_offers(h: Harness, text: &str, cmd: &str) -> bool {
    let rows: Vec<String> = text.lines().map(str::to_string).collect();
    let names = |r: &str| {
        r.strip_prefix(cmd)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    };
    match h {
        Harness::Muse => {
            let Some((caret, lines)) = aterm_phase::muse::composer_draft(&rows) else {
                return false;
            };
            let typed = lines.first().is_some_and(|l| {
                l == cmd
                    || l.strip_prefix(cmd)
                        .is_some_and(|hint| hint.starts_with(" [") && hint.ends_with(']'))
            });
            typed && rows.get(caret + 1).is_some_and(|r| names(r.trim_start()))
        }
        Harness::Agy => {
            let (Some((_, lines)), Some((_, bottom))) = (
                aterm_phase::agy::composer_draft(&rows),
                aterm_phase::agy::composer_rules(&rows),
            ) else {
                return false;
            };
            lines.len() == 1
                && lines[0] == cmd
                && rows
                    .get(bottom + 1)
                    .and_then(|r| r.strip_prefix("> "))
                    .is_some_and(names)
        }
        Harness::Claude => {
            claude_composer(&rows).as_deref() == Some(cmd)
                && !rows
                    .iter()
                    .any(|r| r.starts_with(' ') && r.trim_start().starts_with("❯ /"))
        }
    }
}

fn hide_step(door: &Door<'_>, h: Harness, tab: &mut Tab, now: i64) -> Result<(), Fail> {
    if !tab.chain.hides_on(h)
        || tab.seen.cover.hidden.is_some()
        || tab.seen.cover.hide_gave_up.is_some()
    {
        return Ok(());
    }
    let hide = match hide_of(h) {
        Ok(hide) => hide,
        Err(why) => {
            tab.seen.cover.hide_gave_up = Some(why.to_string());
            return Ok(());
        }
    };
    let cmd = hide.command;
    let Some((_, reset_s)) = tab.seen.cover.wall else {
        return Ok(());
    };
    if !waits_on_the_wall(&tab.seen) {
        return Ok(());
    }
    let deadline = reset_s * 1000 - HIDE_BEFORE_RESET_MS;
    let at_tab = format!("@{}", tab.sid);
    let say = |tab: &Tab, what: &str| {
        println!(
            "  [{}] +{} screen  {what}",
            tab.chain.name(),
            at(tab.t0, now_ms())
        );
    };
    let gone = |tab: &mut Tab, first: i64, enter: i64, commands: usize| {
        let seq = word(&on(door, &tab.sid, &["status"]), "seq").and_then(|s| s.parse().ok());
        let hidden = Hidden {
            first_ms: first,
            enter_ms: enter,
            seen_ms: now_ms(),
            seq,
            commands,
            command: cmd,
            words: hide.words,
        };
        say(
            tab,
            &format!(
                "the notice is out of sight after {commands} {cmd} (screen seq={}), {} before the reset",
                seq.map_or_else(|| "?".to_string(), |s| s.to_string()),
                at(0, reset_s * 1000 - hidden.seen_ms)
            ),
        );
        tab.seen.cover.hidden = Some(hidden);
    };
    let mut commands = 0;
    let mut first = None;
    let mut enter = now;
    let mut why = None;
    if let Some(c) = hide.cancel {
        match cancel_countdown(door, &at_tab, &tab.sid, c, deadline)? {
            Ok(at) => {
                say(tab, &format!("its countdown cancelled ({})", c.key));
                first = Some(at);
                enter = at;
            }
            Err(why) => {
                say(tab, &format!("the notice NOT taken off the screen: {why}"));
                tab.seen.cover.hide_gave_up = Some(why);
                return Ok(());
            }
        }
    }
    for _ in 0..=HIDE_TRIES {
        let text = on(door, &tab.sid, &["text"]);
        if notice_gone(h, &text) {
            gone(tab, first.unwrap_or(enter), enter, commands);
            return Ok(());
        }
        if commands == HIDE_TRIES {
            why = Some(format!("the notice still in sight after {commands} {cmd}"));
            break;
        }
        if now_ms() >= deadline {
            why = Some(format!(
                "the reset under {} away before the notice left (after {commands} {cmd})",
                at(0, HIDE_BEFORE_RESET_MS)
            ));
            break;
        }
        let empty = match h {
            Harness::Claude => {
                claude_composer(&text.lines().map(str::to_string).collect::<Vec<_>>()).as_deref()
                    == Some("")
            }
            _ => stand_on(door, &tab.sid, h.name(), &text) == Stand::Composer(String::new()),
        };
        if !empty {
            why = Some("the composer was not empty: nothing entered".to_string());
            break;
        }
        if door.ctl(&[&at_tab, "send", cmd])?.code != 0 {
            why = Some(format!("`send {cmd}` refused"));
            break;
        }
        stop::sleep(Duration::from_millis(1500))?;
        let text = on(door, &tab.sid, &["text"]);
        if !menu_offers(h, &text, cmd) {
            let _ = door.ctl(&[&at_tab, "key", "escape"]);
            let _ = door.ctl(&[&at_tab, "key", "ctrl+u"]);
            why = Some(format!(
                "the menu did not offer {cmd} first: Enter not pressed"
            ));
            break;
        }
        enter = now_ms();
        first.get_or_insert(enter);
        if door.ctl(&[&at_tab, "key", "enter"])?.code != 0 {
            why = Some("`key enter` refused".to_string());
            break;
        }
        commands += 1;
        say(tab, &format!("{cmd} {commands} entered"));
        stop::sleep(Duration::from_millis(2500))?;
    }
    let why = why.unwrap_or_else(|| "no try left".to_string());
    say(tab, &format!("the notice NOT taken off the screen: {why}"));
    tab.seen.cover.hide_gave_up = Some(why);
    Ok(())
}

/// Whether the program's own countdown stands armed on `text`: its armed words drawn
/// ([`Cancel::armed`]) and not yet its cancel ([`Cancel::cancelled`]; the armed row stays in the
/// conversation above it once cancelled).
#[must_use]
pub fn countdown_armed(text: &str, c: Cancel) -> bool {
    text.contains(c.armed) && !text.contains(c.cancelled)
}

fn cancel_countdown(
    door: &Door<'_>,
    at_tab: &str,
    sid: &str,
    c: Cancel,
    deadline: i64,
) -> Result<Result<i64, String>, Fail> {
    if now_ms() >= deadline {
        return Ok(Err(format!(
            "the reset under {} away before its countdown was cancelled",
            at(0, HIDE_BEFORE_RESET_MS)
        )));
    }
    let text = on(door, sid, &["text"]);
    if !countdown_armed(&text, c) {
        return Ok(Err(format!(
            "no countdown armed ({:?} not drawn, or {:?} already): {} not pressed",
            c.armed, c.cancelled, c.key
        )));
    }
    let at_key = now_ms();
    if door.ctl(&[at_tab, "key", c.key])?.code != 0 {
        return Ok(Err(format!("`key {}` refused", c.key)));
    }
    stop::sleep(Duration::from_millis(1500))?;
    if !on(door, sid, &["text"]).contains(c.cancelled) {
        return Ok(Err(format!(
            "{} pressed, but {:?} never drawn",
            c.key, c.cancelled
        )));
    }
    Ok(Ok(at_key))
}

/// The agent W5 starts in harness `h`'s tab under `h`'s remembered limit ([`switch_step`]): one
/// whose limits the loop reads and decides on too (`turn_end.rs`'s `met_under_another`), on another
/// account: Muse Code after Claude Code or agy, Claude Code after Muse Code — the
/// measured pairs, in which each holds the other.
#[must_use]
pub fn switch_to(h: Harness) -> Harness {
    match h {
        Harness::Muse => Harness::Claude,
        Harness::Claude | Harness::Agy => Harness::Muse,
    }
}

/// How W5 ends harness `h`'s agent at its wall ([`exit_agent`]): Muse Code takes ctrl+c twice (its
/// `exit-confirm` capture); Claude Code too (`footers.rs`'s quit), but its usage limit's countdown
/// takes the first ctrl+c for its cancel, so the countdown is cancelled first where it stands
/// armed ([`CLAUDE_CANCEL`], measured on 2.1.308); agy takes `/quit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// ctrl+c, half a second, ctrl+c.
    CtrlCTwice,
    /// The program's own countdown cancelled where it stands armed ([`countdown_armed`],
    /// [`cancel_countdown`]), then ctrl+c twice.
    CancelThenCtrlCTwice(Cancel),
    /// The command, then Enter.
    Command(&'static str),
}

/// [`Exit`] of harness `h`.
#[must_use]
pub fn exit_of(h: Harness) -> Exit {
    match h {
        Harness::Claude => Exit::CancelThenCtrlCTwice(CLAUDE_CANCEL),
        Harness::Muse => Exit::CtrlCTwice,
        Harness::Agy => Exit::Command("/quit"),
    }
}

/// The program `ls` names in session `sid`'s row (`program=<word>`), a login shell's leading `-`
/// cut.
#[must_use]
pub fn program_of(ls: &str, sid: &str) -> Option<String> {
    ls.lines()
        .find(|l| l.split_whitespace().nth(2) == Some(sid))
        .and_then(|l| word(l, "program"))
        .map(|p| p.trim_start_matches('-').to_string())
}

/// Whether `program` (as [`program_of`] reads it) is a shell: the tab's own, its agent gone.
#[must_use]
pub fn is_shell(program: &str) -> bool {
    ["sh", "zsh", "bash", "dash", "fish"].contains(&program)
}

fn exit_agent(door: &Door<'_>, sid: &str, h: Harness) -> Result<Result<(i64, i64), String>, Fail> {
    let at_tab = format!("@{sid}");
    let command = |door: &Door<'_>, cmd: &str| -> Result<bool, Fail> {
        if door.ctl(&[&at_tab, "send", cmd])?.code != 0 {
            return Ok(false);
        }
        stop::sleep(Duration::from_millis(700))?;
        Ok(door.ctl(&[&at_tab, "key", "enter"])?.code == 0)
    };
    let ctrl_c_twice = |door: &Door<'_>| -> Result<Result<i64, String>, Fail> {
        let mut last = now_ms();
        for _ in 0..2 {
            if door.ctl(&[&at_tab, "key", "ctrl+c"])?.code != 0 {
                return Ok(Err("`key ctrl+c` refused".to_string()));
            }
            last = now_ms();
            stop::sleep(Duration::from_millis(500))?;
        }
        Ok(Ok(last))
    };
    let last;
    match exit_of(h) {
        Exit::CtrlCTwice => match ctrl_c_twice(door)? {
            Ok(t) => last = t,
            Err(why) => return Ok(Err(why)),
        },
        Exit::CancelThenCtrlCTwice(c) => {
            if countdown_armed(&on(door, sid, &["text"]), c) {
                if let Err(why) = cancel_countdown(door, &at_tab, sid, c, i64::MAX)? {
                    return Ok(Err(why));
                }
                stop::sleep(Duration::from_millis(1000))?;
            }
            match ctrl_c_twice(door)? {
                Ok(t) => last = t,
                Err(why) => return Ok(Err(why)),
            }
        }
        Exit::Command(cmd) => {
            if !command(door, cmd)? {
                return Ok(Err(format!("`{cmd}` not entered")));
            }
            last = now_ms();
        }
    }
    let mut shell = None;
    let back = crate::wait_until(Duration::from_secs(20), || {
        let p = program_of(&door.out(&["ls"]), sid);
        let at_shell = p.as_deref().is_some_and(is_shell);
        if at_shell {
            shell = Some(now_ms());
        }
        at_shell
    });
    match shell.filter(|_| back) {
        Some(t) => Ok(Ok((last, t))),
        None => Ok(Err(format!(
            "{} still in the tab 20 s after its exit (ls program={})",
            h.name(),
            program_of(&door.out(&["ls"]), sid).unwrap_or_else(|| "?".to_string())
        ))),
    }
}

/// How long past the first agent's reset W5 watches its tab: the longest grace a program's resume
/// waits ([`wall_grace_ms`]) and [`PERSISTENCE_GRACE_MS`] more, so a resume of the ended limit
/// typed into the second agent is seen.
#[must_use]
pub fn switch_watch_until(reset_s: i64) -> i64 {
    let grace = Harness::ALL
        .iter()
        .map(|h| wall_grace_ms(h.name()))
        .max()
        .unwrap_or(0);
    reset_s * 1000 + grace + PERSISTENCE_GRACE_MS
}

fn switch_step(run: &Run, door: &Door<'_>, h: Harness, tab: &mut Tab) -> Result<(), Fail> {
    if tab.chain != Chain::SwitchWall
        || tab.seen.cover.switch.is_some()
        || tab.seen.cover.switch_gave_up.is_some()
        || tab.seen.cover.wall.is_none()
        || !waits_on_the_wall(&tab.seen)
    {
        return Ok(());
    }
    match agent_again(run, door, h, switch_to(h), tab, true)? {
        Ok(sw) => {
            tab.seen.cover.lifted_ms = sw.lifted_ms;
            tab.seen.cover.switch = Some(sw);
        }
        Err(why) => tab.seen.cover.switch_gave_up = Some(why),
    }
    Ok(())
}

fn relaunch_step(run: &Run, door: &Door<'_>, h: Harness, tab: &mut Tab) -> Result<(), Fail> {
    if tab.chain != Chain::RelaunchWall
        || tab.seen.cover.relaunch.is_some()
        || tab.seen.cover.relaunch_gave_up.is_some()
        || tab.seen.cover.wall.is_none()
        || !waits_on_the_wall(&tab.seen)
    {
        return Ok(());
    }
    match agent_again(run, door, h, h, tab, false)? {
        Ok(r) => tab.seen.cover.relaunch = Some(r),
        Err(why) => tab.seen.cover.relaunch_gave_up = Some(why),
    }
    Ok(())
}

fn agent_again(
    run: &Run,
    door: &Door<'_>,
    h: Harness,
    to: Harness,
    tab: &Tab,
    lift: bool,
) -> Result<Result<Switch, String>, Fail> {
    let verb = if lift { "switch " } else { "relaunch" };
    let say = |what: &str| {
        println!(
            "  [{}] +{} {verb} {what}",
            tab.chain.name(),
            at(tab.t0, now_ms())
        );
    };
    let give_up = |why: String| {
        say(&format!("NOT done: {why}"));
        Ok(Err(why))
    };
    let (bin, _) = to.bin_ver(run);
    if bin.is_empty() || !crate::sys::executable(bin) {
        return give_up(format!(
            "no {} in the lab (LAB_{} at up) to start under {}'s limit",
            to.name(),
            to.name().to_ascii_uppercase(),
            h.name()
        ));
    }
    let (exited_ms, shell_ms) = match exit_agent(door, &tab.sid, h)? {
        Ok(t) => t,
        Err(why) => return give_up(why),
    };
    say(&format!("{} ended, the shell back", h.name()));
    let lifted_ms = if lift {
        let answer = crate::verbs::set_mode(run, MODE)?;
        say(&format!(
            "the stub's wall lifted for {}'s account: mode {MODE} again ({})",
            to.name(),
            crate::util::one_line(&answer, 120)
        ));
        Some(now_ms())
    } else {
        None
    };
    let script = launcher(run, to, std::slice::from_ref(&tab.dir), !lift)?;
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    let _ = on(door, &tab.sid, &["send", &line]);
    let _ = on(door, &tab.sid, &["key", "enter"]);
    let launched_ms = now_ms();
    if let Some(why) = empty_composer(door, &tab.sid, to.name())?.why(to.name()) {
        return give_up(why);
    }
    stop::sleep(Duration::from_secs(1))?;
    let prompt_ms = now_ms();
    if !tab_turn(door, &tab.sid, to.name(), &tab.chain.prompt())? {
        return give_up(format!(
            "the composer of {} did not take the prompt",
            to.name()
        ));
    }
    say(&format!(
        "{} launched in the same tab, its prompt typed: \"{}\"",
        to.name(),
        tab.chain.prompt()
    ));
    Ok(Ok(Switch {
        to: to.name(),
        exited_ms,
        shell_ms,
        lifted_ms,
        launched_ms,
        prompt_ms,
    }))
}

/// Where W6 moves a transcript aside ([`unread_step`]): `<id>.jsonl.unread` beside it, which no
/// reader of `*.jsonl` takes.
#[must_use]
pub fn aside_of(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".unread");
    file.with_file_name(name)
}

/// When the loop told the owner of a limit held unread ([`unread_told_words`]): the first
/// `EXCEPTION` line after `since` that carries the told's words.
#[must_use]
pub fn told_unread(seen: &Seen, since: i64) -> Option<i64> {
    let words = unread_told_words();
    seen.journal
        .iter()
        .find(|(t, l)| *t > since && l.starts_with("EXCEPTION ") && l.contains(words.as_str()))
        .map(|(t, _)| *t)
}

/// Whether W6's lab reads the conversation again at `now` ([`unread_step`]):
/// [`UNREAD_RESTORE_AFTER_TOLD_MS`] after the told, or, with none seen, [`UNREAD_TOLD_LATE_MS`]
/// past its due (the reset and [`futile_after_ms`]), so the run ends either way.
#[must_use]
pub fn unread_restore_due(seen: &Seen, u: &Unread, reset_s: i64, now: i64) -> bool {
    match told_unread(seen, u.moved_ms) {
        Some(t) => now >= t + UNREAD_RESTORE_AFTER_TOLD_MS,
        None => now >= reset_s * 1000 + futile_after_ms() + UNREAD_TOLD_LATE_MS,
    }
}

fn unread_step(run: &Run, tab: &mut Tab, now: i64) {
    if tab.chain != Chain::UnreadWall || tab.seen.cover.unread_gave_up.is_some() {
        return;
    }
    let Some((_, reset_s)) = tab.seen.cover.wall else {
        return;
    };
    let say = |tab: &Tab, what: &str| {
        println!(
            "  [{}] +{} unread  {what}",
            tab.chain.name(),
            at(tab.t0, now_ms())
        );
    };
    match tab.seen.cover.unread.clone() {
        None if tab.seen.cover.hidden.is_some() => {
            let projects = run.w.join("home/.claude/projects");
            let mut files = Vec::new();
            crate::util::walk_files(&projects, &mut files);
            let token = tab.chain.token();
            let file = files.into_iter().find(|p| {
                p.extension().is_some_and(|e| e == "jsonl")
                    && crate::util::read_regular_text(p).is_ok_and(|t| t.contains(&token))
            });
            let Some(file) = file else {
                let why = format!("no transcript holds {token} under {}", projects.display());
                say(tab, &why);
                tab.seen.cover.unread_gave_up = Some(why);
                return;
            };
            if let Err(e) = std::fs::rename(&file, aside_of(&file)) {
                let why = format!("{} not moved aside: {e}", file.display());
                say(tab, &why);
                tab.seen.cover.unread_gave_up = Some(why);
                return;
            }
            say(
                tab,
                &format!(
                    "the transcript moved aside ({}): no read names the conversation",
                    file.display()
                ),
            );
            tab.seen.cover.unread = Some(Unread {
                file,
                moved_ms: now_ms(),
                restored_ms: None,
            });
        }
        Some(u) if u.restored_ms.is_none() && unread_restore_due(&tab.seen, &u, reset_s, now) => {
            if u.file.exists() {
                let why = format!(
                    "Claude Code wrote {} again while it was aside: not moved back",
                    u.file.display()
                );
                say(tab, &why);
                tab.seen.cover.unread_gave_up = Some(why);
                return;
            }
            if let Err(e) = std::fs::rename(aside_of(&u.file), &u.file) {
                let why = format!("{} not moved back: {e}", u.file.display());
                say(tab, &why);
                tab.seen.cover.unread_gave_up = Some(why);
                return;
            }
            say(
                tab,
                "the transcript moved back: the conversation read again",
            );
            if let Some(u) = tab.seen.cover.unread.as_mut() {
                u.restored_ms = Some(now_ms());
            }
        }
        _ => {}
    }
}

fn look(run: &Run, door: &Door<'_>, tab: &mut Tab) {
    let now = now_ms();
    let drive = run.state().join("drive");
    let (j_reset, j_delta) = tab
        .journal_cursor
        .poll_scratch_text(&drive.join(format!("{}.journal.jsonl", tab.sid)))
        .unwrap_or_default();
    if j_reset {
        tab.seen.journal.clear();
    }
    for (t, l) in parse_journal_lines(&j_delta) {
        println!(
            "  [{}] +{} journal {}",
            tab.chain.name(),
            at(tab.t0, t),
            crate::util::one_line(&l, 220)
        );
        tab.seen.journal.push((t, l));
    }
    let (l_reset, l_delta) = tab
        .ledger_cursor
        .poll_scratch_text(&drive.join(format!("{}.jsonl", tab.sid)))
        .unwrap_or_default();
    if l_reset {
        tab.seen.ledger.clear();
    }
    for r in parse_ledger_lines(&l_delta) {
        println!(
            "  [{}] +{} ledger  {} {} {}",
            tab.chain.name(),
            at(tab.t0, r.ts),
            r.decision,
            r.rule,
            crate::util::one_line(&r.command, 200)
        );
        tab.seen.ledger.push(r);
    }
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
    // A read whose reply failed records nothing, its time included: the next look reads again.
    if let Some(status) = status_words(door, &tab.sid) {
        tab.seen.status_reads.push(now);
        put(&mut tab.seen.status, "status", status);
    }
    if let Some(meta) = meta_words(door, &tab.sid) {
        put(&mut tab.seen.meta, "meta", meta);
    }
    if let Ok(task) = reading_on(door, &tab.sid, &["task"]) {
        put(&mut tab.seen.task, "task", task.trim().to_string());
    }
    if tab.chain.same_reply()
        && let Ok(messages) = reading(door, &["messages", "tag=harness"])
    {
        let asks = surfaces::held::resume_ask_rows(&messages, &tab.sid);
        tab.seen.harness_reads.push(now);
        put(&mut tab.seen.harness, "harness", asks);
    }
    if tab.chain.login()
        && let Ok(messages) = reading(door, &["messages", "tag=harness"])
    {
        tab.seen.harness_reads.push(now);
        put(
            &mut tab.seen.harness,
            "harness",
            sysmsg::login::rows(&messages),
        );
    }
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

/// THE ORDER THE CHAINS ARE PROMPTED IN, as indices into `chains`: theirs, but the chains whose
/// first span closes on their program's clock ([`Chain::timed_from_its_start`]) late and the gated
/// chains ([`Chain::gated`]: the need chains, land-contradicted) last. The lab's watch, which reads
/// each need, or lands land-contradicted's file, and opens its gate, begins once every chain is
/// prompted, and prompting a full run takes longer than the gate's hang detector
/// (`stub::GATE_HANG`): a need raised first stood unread until its gate gave way, and a span that
/// closed before the watch began was read by nobody.
#[must_use]
pub fn prompt_order(chains: &[Chain]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..chains.len()).collect();
    order.sort_by_key(|&i| (chains[i].gated(), chains[i].timed_from_its_start()));
    order
}

/// WHETHER THE LAB HAS READ A NEED STANDING on every surface its need wants judge ([`Chain::need_gate`]):
/// `status needs=ask needs_n=1` (L3), the `meta` entry (`attention_kinds` `ask:needs`, L1), and,
/// where the windows are read (`window`), a `Needs you` row, the tab's `attention` state and a
/// Harness page read once `status` showed Land's hold ([`hold_paged`]).
#[must_use]
pub fn need_read(seen: &Seen, window: bool) -> bool {
    let status = seen.status.iter().any(|(_, s)| {
        word(s, "needs").as_deref() == Some("ask") && word(s, "needs_n").as_deref() == Some("1")
    });
    let meta = seen.meta.iter().any(|(_, m)| needs_up(m));
    let shown = !window
        || (seen
            .window
            .menu
            .iter()
            .any(|(_, m)| !surfaces::rows_in(m, "needs").is_empty())
            && seen
                .window
                .strip
                .iter()
                .any(|(_, s)| surfaces::states(s).contains(&"attention"))
            && hold_paged(seen));
    status && meta && shown
}

/// Whether a Harness page read fell at or after the first `status` read that showed Land's hold
/// (`land_held=` other than `-`): the read L3's hold judge takes the state line from
/// (`hold::legs`, `no Harness page read after Land's instruction`). The watch reads the page at
/// the head of each pass and `status` after it, so the page read that shows the hold's state line
/// beside the pass whose `status` first shows the hold falls before that read: the next pass's
/// is the first to count, and a gate opened without it let the landing fall before it under a
/// loaded lab (26 tabs a harness, three harnesses at once).
#[must_use]
pub fn hold_paged(seen: &Seen) -> bool {
    let held = seen
        .status
        .iter()
        .find(|(_, s)| word(s, "land_held").is_some_and(|v| v != "-" && v != "?"))
        .map(|(t, _)| *t);
    held.is_some_and(|from| seen.window.page_reads.iter().any(|t| *t >= from))
}

/// OPENS `tab`'S NEED GATE once its need was read ([`need_read`]): the stub then answers the land
/// check with `LANDED`, which lands and leaves the need standing.
fn open_need_gate(run: &Run, tab: &mut Tab, window: bool) {
    let Some(gate) = tab.chain.need_gate().filter(|_| !tab.gate_opened) else {
        return;
    };
    if !need_read(&tab.seen, window) {
        return;
    }
    let path = crate::stub::gate_path(&run.w.join("modes"), &gate);
    match crate::util::write_fresh(&path, b"") {
        Ok(()) => {
            tab.gate_opened = true;
            println!(
                "  [{}] +{} gate    the need was read on every surface: the check's answer goes",
                tab.chain.name(),
                at(tab.t0, now_ms())
            );
        }
        Err(e) => println!(
            "  [{}] gate    {} not written: {e}",
            tab.chain.name(),
            path.display()
        ),
    }
}

/// THE CONTRADICTION'S TYPED ROW (slice 4c): the first `land-nudge@v1` the ledger says was typed
/// with [`turn_end::LAND_CONTRADICTED_HEAD`] leading.
fn contradiction_row(seen: &Seen) -> Option<&Row> {
    seen.ledger.iter().find(|r| {
        r.rule == turn_end::RULE_LAND_NUDGE
            && r.decision == "typed"
            && r.command.starts_with(turn_end::LAND_CONTRADICTED_HEAD)
    })
}

/// LAND-CONTRADICTED'S HANDS (slice 4c): once the ledger shows Land's contradiction typed, the lab
/// commits `wip-notes.txt` in the chain's repository and pushes it to `origin/main`, as the agent
/// the contradiction asks would, then opens [`Chain::landed_gate`], so the stub's answer that says
/// so goes ([`LANDED_NOTES`]). When it landed is [`Seen::landed_ms`]. A git step that fails leaves
/// the gate shut: the stub's answer then goes at its hang detector over a repository still
/// holding the file, and the judge fails the run on the second contradiction.
fn land_contradicted_work(run: &Run, tab: &mut Tab) {
    let Some(gate) = tab.chain.landed_gate().filter(|_| !tab.gate_opened) else {
        return;
    };
    let Some(row) = contradiction_row(&tab.seen) else {
        return;
    };
    let typed = row.ts;
    tab.gate_opened = true;
    let home = run.w.join("home");
    let landed = [
        &["add", "wip-notes.txt"][..],
        &[
            "commit",
            "-q",
            "-m",
            "lab: the notes Land's contradiction named",
        ],
        &["push", "-q", "origin", "main"],
    ]
    .iter()
    .try_for_each(|args| git(&home, &tab.dir, args));
    let path = crate::stub::gate_path(&run.w.join("modes"), &gate);
    let said = match landed.and_then(|()| {
        crate::util::write_fresh(&path, b"").map_err(|e| format!("{}: {e}", path.display()))
    }) {
        Ok(()) => {
            let now = now_ms();
            tab.seen.landed_ms = Some(now);
            format!(
                "the contradiction typed at +{}: wip-notes.txt committed and pushed to origin/main, \
                 the answer goes",
                at(tab.t0, typed)
            )
        }
        Err(e) => format!("the contradiction's work NOT landed: {e}"),
    };
    println!(
        "  [{}] +{} gate    {said}",
        tab.chain.name(),
        at(tab.t0, now_ms())
    );
}

fn ask_due(s: &Seen, now: i64) -> bool {
    let Some((t1, _)) = s
        .all_acts()
        .into_iter()
        .find(|(_, l)| act_rule(l) == Some(turn_end::RULE_DONE_CHECK))
    else {
        return false;
    };
    let t1 = *t1;
    let y = s.yield_after(t1, None, Some(DEFENSE_HEAD));
    let Some((te, _)) = y.event else {
        return false;
    };
    let decided = s.journal.iter().find(|(t, l)| {
        *t >= *te && l.starts_with("WAITING ") && !l.contains(" the worker to take the ")
    });
    decided.is_some_and(|(tw, _)| now - *tw >= 5000) && s.all_acts().iter().all(|(t, _)| *t <= t1)
}

/// Whether `tab`'s watch is over: a landing chain quiet past the ladder's next step, a same-reply
/// chain [`PAST_NEXT_STEP_MS`] past its last act's point (past its hold, so what it says of it is
/// read: [`surfaces::held::watched_past`]), each other chain at what it waits for, every one at its
/// hang detector ([`Chain::limit_ms`]).
fn finished(tab: &Tab, now: i64) -> bool {
    let s = &tab.seen;
    let done = s.first_after("DONE ", tab.t0).map(|(t, _)| *t);
    if tab.chain.lands()
        && let Some(d) = done
    {
        let quiet = now - d >= quiet_after_landed_ms(s, d, tab.chain.managed()) + 5000;
        return quiet
            && (tab.chain != Chain::SwitchWall
                || s.cover
                    .wall
                    .is_none_or(|(_, reset_s)| now >= switch_watch_until(reset_s)));
    }
    if now - tab.t0 > tab.chain.limit_ms() {
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
        Chain::LandA
        | Chain::LandN
        | Chain::LandO
        | Chain::LandE
        | Chain::LandQ
        | Chain::LandW
        | Chain::LandHinted
        | Chain::LandListed
        | Chain::Worker
        | Chain::Wall
        | Chain::WallHid
        | Chain::RedrawWall
        | Chain::SwitchWall
        | Chain::UnreadWall
        | Chain::RelaunchWall
        | Chain::TapWall
        | Chain::NarrowTapWall
        | Chain::LandOutage
        | Chain::LandDialog => false,
        Chain::CruiseDialog => {
            s.all_acts().first().is_some_and(|(t, _)| now - t >= 30_000)
                || (s.all_acts().is_empty() && now - tab.t0 > 330_000)
        }
        // land-contradicted lands (above); a build that holds it instead (main before 4c, whose
        // nudge its script answers with the defense again) is watched past its hold.
        Chain::LandContradicted => {
            surfaces::held::stall_of(s, tab.t0).is_some_and(|(t, _)| now - *t >= PAST_NEXT_STEP_MS)
        }
        Chain::CruiseWall => walls::cruise_went_on(s).is_some_and(|t| now - t >= 10_000),
        Chain::CruiseOutage => {
            outage::went_on(s).is_some_and(|t| now - t >= outage::WENT_ON_WATCH_MS)
        }
        Chain::SysOverload
        | Chain::SysOutage
        | Chain::SysRate
        | Chain::SysDrop
        | Chain::SysWitness
        | Chain::SysWitnessMate
        | Chain::SysLandOverload
        | Chain::SysLandOutage
        | Chain::SysLandRate
        | Chain::SysLandDrop => sysmsg::watched(s, tab.chain.autopilot() == "land", now),
        Chain::SysLogin | Chain::SysLandLogin => {
            sysmsg::login::watched(s, tab.chain.autopilot() == "land", now)
        }
        Chain::Mgr => true,
        Chain::LandT => done_since(20_000),
        Chain::LandP => done_since(20_000),
        Chain::Cap => cover::cap_watched(s, now) || done_since(20_000),
        Chain::InstantLand => s.all_acts().is_empty() && now - tab.t0 > 300_000,
        Chain::LandB
        | Chain::LandC
        | Chain::LandF
        | Chain::LandG
        | Chain::LandH
        | Chain::LandI
        | Chain::LandSameList => {
            (nudge.is_some() && surfaces::held::watched_past(s, now)) || done_since(20_000)
        }
        // land-contradicted-list's hold comes after its second nudge, the contradiction: the
        // first's back-off is no end of its watch.
        Chain::LandContradictedList => {
            (s.acts(turn_end::RULE_LAND_NUDGE).len() >= 2 && surfaces::held::watched_past(s, now))
                || done_since(20_000)
        }
        // land-reworded's hold comes after the nudge quoting its new words, the first past its
        // second check: the back-off before that nudge is no end of its watch.
        Chain::LandReworded => {
            let second_check = s.acts(turn_end::RULE_LAND_CHECK).get(1).map(|(t, _)| *t);
            let quoted = second_check.is_some_and(|c| {
                s.acts(turn_end::RULE_LAND_NUDGE)
                    .iter()
                    .any(|(t, _)| *t > c)
            });
            (quoted && surfaces::held::watched_past(s, now)) || done_since(20_000)
        }
        // L6 rests on its check's need: watched past the ladder's next step after the rest, as a
        // landing is.
        Chain::LandD => {
            rest_of(s).is_some_and(|(t, _)| now - t >= quiet_after_landed_ms(s, *t, false) + 5000)
                || (nudge.is_some() && surfaces::held::watched_past(s, now))
                || done_since(20_000)
        }
        // same-land is watched past its person's turn, until its resume ask is gone or a minute
        // passed.
        Chain::SameLand => {
            s.stop_person_ms.is_some_and(|p| {
                now - p >= 60_000
                    || s.harness
                        .last()
                        .is_some_and(|(t, b)| *t > p && surfaces::held::stop_rows(b).is_empty())
            }) || done_since(20_000)
        }
        Chain::SameCruise => paced::watched(s, now, tab.t0) || done_since(20_000),
        Chain::InstantCruise => {
            let head: String = ASK_REPLY.chars().take(24).collect();
            let judged = s.asked_ms.and_then(|a| {
                s.journal
                    .iter()
                    .find(|(t, l)| {
                        *t > a && idle_words(l).is_some_and(|w| w.contains(head.as_str()))
                    })
                    .map(|(t, _)| *t)
            });
            let decision = judged.and_then(|e| {
                s.journal.iter().find(|(t, l)| {
                    *t >= e
                        && ((l.starts_with("WAITING ") && !l.contains(" the worker to take the "))
                            || l.starts_with("LEFT ")
                            || l.starts_with("DONE "))
                })
            });
            let gone_on = judged
                .is_some_and(|e| s.all_acts().iter().any(|(t, _)| *t > e && now - *t >= 3000));
            let watched_out = decision.is_some_and(|(td, dl)| match backoff_due(dl) {
                Some(due) => now >= due + PERSISTENCE_GRACE_MS + 5000,
                None => now - td >= 3000,
            });
            let unjudged = s
                .asked_ms
                .is_some_and(|a| judged.is_none() && now - a > 150_000);
            let refused = s.ask_refused_ms.is_some_and(|a| now - a > 5000);
            (decision.is_some() && (gone_on || watched_out))
                || unjudged
                || refused
                || done_since(20_000)
                || (s.all_acts().is_empty() && now - tab.t0 > 300_000)
        }
        Chain::Cruise | Chain::WroteC => {
            let check = first(turn_end::RULE_DONE_CHECK);
            (check.is_some_and(|c| s.all_acts().iter().any(|(t, _)| *t > c))
                && surfaces::held::watched_past(s, now))
                || done_since(20_000)
        }
        Chain::CruiseO => {
            first(turn_end::RULE_CONTINUE).is_some_and(|c| {
                s.all_acts().iter().any(|(t, _)| *t > c)
                    || s.yield_after(c, None, Some(DONE_CLAIM_READ))
                        .event
                        .is_some_and(|(t, _)| now - t >= 20_000)
            }) || done_since(20_000)
        }
        Chain::Off => s
            .reply_ms
            .is_some_and(|r| now - r >= QUIET_UNDER_OFF_MS + 5000),
        Chain::LandJ | Chain::LandK => done_since(20_000),
        // F18's turn is judged on the ladder (Cruise never parks): watched past its back-off's
        // due to the act there, or past the grace where none came.
        Chain::InstantPerson => {
            let asked = s.asked_ms;
            let left = asked.and_then(|a| {
                s.journal
                    .iter()
                    .find(|(t, l)| *t > a && l.starts_with("LEFT "))
                    .map(|(t, _)| *t)
            });
            let decided = asked.and_then(|a| own_backoff_after(s, a));
            let lost = decided
                .and_then(|(_, l)| backoff_due(l))
                .is_some_and(|due| now >= due + PERSISTENCE_GRACE_MS + 5000);
            let typed_into =
                asked.is_some_and(|a| s.all_acts().iter().any(|(t, _)| *t > a && now - *t >= 3000));
            let unjudged =
                asked.is_some_and(|a| left.is_none() && decided.is_none() && now - a > 150_000);
            let refused = s.ask_refused_ms.is_some_and(|a| now - a > 5000);
            left.is_some_and(|l| now - l >= QUIET_UNDER_OFF_MS + 5000)
                || lost
                || typed_into
                || unjudged
                || refused
                || done_since(20_000)
                || (s.all_acts().is_empty() && now - tab.t0 > 300_000)
        }
        Chain::SameWords => {
            s.cover.turn_ms.is_some_and(|tt| {
                s.all_acts()
                    .iter()
                    .any(|(t, _)| *t > tt && now - *t >= 20_000)
            }) || done_since(20_000)
                || (s.all_acts().is_empty() && now - tab.t0 > 330_000)
        }
        Chain::TallScroll => {
            s.cover
                .scrolled
                .as_ref()
                .is_some_and(|sc| now - sc.at >= SCROLL_WATCH_MS)
                || s.all_acts().first().is_some_and(|(t, _)| now - t >= 10_000)
                || done_since(20_000)
                || (s.all_acts().is_empty() && now - tab.t0 > 330_000)
        }
        Chain::ToolEnd => {
            s.all_acts().first().is_some_and(|(t, _)| now - t >= 30_000)
                || done_since(20_000)
                || (s.all_acts().is_empty() && now - tab.t0 > 330_000)
        }
        Chain::Ladder => ladder_steps(s, tab.t0)
            .get(LADDER_STEPS as usize - 1)
            .is_some_and(|st| now - st.at >= 5_000),
        Chain::CruiseN => {
            let acts = s.all_acts();
            acts.len() >= 3
                && s.yield_after(acts[2].0, None, Some(NEED_C))
                    .event
                    .is_some_and(|(t, _)| now - t >= 10_000)
                // N4's stand-in ask has been read where the window is (§14.7).
                && (!s.window.read || s.window.stopped.is_some())
        }
        Chain::CruiseAngles => angles::watched(s, now, tab.t0),
        Chain::SwitchAgent
        | Chain::SwitchSeq
        | Chain::CommandDraft
        | Chain::SwitchFlip
        | Chain::SwitchOverHeld
        | Chain::SwitchBusy => switch::watched(tab.chain, s, now),
    }
}

fn tail_rows(door: &Door<'_>, sid: &str, n: usize) -> Vec<String> {
    let text = on(door, sid, &["text"]);
    let rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    rows[rows.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.chars().take(150).collect())
        .collect()
}

#[allow(clippy::too_many_lines)]
fn run_check(
    tag: &str,
    h: Harness,
    chains: &[Chain],
    (surfaces_on, window, tapped, login, ready_gate): (
        bool,
        bool,
        bool,
        sysmsg::login::LoginArm,
        bool,
    ),
) -> Result<Vec<(Scenario, Verdict)>, Fail> {
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
    // An expired login's chain launches Claude Code on a claude.ai login of the lab's, so the
    // stub's 401 is that login's ([`sysmsg::login`]): a stored one, the stub its only route
    // ([`sysmsg::login::stored_arm`]), or with `--token-login` the environment's token.
    let stored = sysmsg::login::stored_arm(h, chains, login);
    let walled = chains.iter().any(|c| c.walled() || (c.login() && !stored));
    let tap_wall = chains.iter().any(|c| c.tapped());
    let extra = if tap_wall && tapped {
        (Vec::new(), tap::tap_args(&run)?)
    } else if chains.iter().any(|c| c.sysmsg()) {
        sysmsg::vendor_retries(h)
    } else {
        (Vec::new(), Vec::new())
    };
    let launch = launcher_with(&run, h, &dirs, (walled, stored), extra)?;
    if stored && login == sysmsg::login::LoginArm::Forced {
        sysmsg::login::lay_pin(&run)?;
        println!(
            "  login     the lab's settings pin the sign-in method (forceLoginMethod {}): /login draws no picker and opens the browser on its Enter",
            sysmsg::login::PINNED_METHOD
        );
    }
    if tap_wall {
        crate::verbs::write_headers(
            &run.w,
            crate::verbs::rate_blocks(Some(&tap::on_429())).map_err(|e| could_not(&e))?,
            true,
        )?;
        println!(
            "  tap       {}; the wall's 429 carries the window's figures: {}",
            if tapped {
                "aterm's usage tap on Claude Code's line"
            } else {
                "NO tap on Claude Code's line (--untapped, the negative control)"
            },
            tap::on_429()
        );
    }
    let scroll_script = if chains.contains(&Chain::TallScroll) {
        scroll_launcher(&run, h, &dirs, walled)?
    } else {
        None
    };
    crate::verbs::set_custom_mode(&run, MODE, &spec_on(chains, h, &dirs))?;
    println!(
        "  stub      mode {MODE}: {} rule(s)",
        chains.iter().map(|c| c.rules().len()).sum::<usize>()
    );
    let door = Door::new(&run)?;
    let mut holds = input_hold::Holds::of(&run)?;
    if let Some(said) = holds.said(ready_gate) {
        println!("  holds     {said}");
    }
    let unread = surfaces::unread_why(surfaces_on, chains);
    let mut desk = if let Some(why) = unread {
        println!("  windows   not read ({why}): the window's wants are named skips");
        None
    } else {
        let d =
            surfaces::lay_windows(&door).map_err(|e| could_not(&format!("the windows: {e}")))?;
        println!(
            "  windows   Settings ▸ Harness in window {} (view {}); the chains' window {}, in front",
            d.page_window, d.page_view, d.chains_window
        );
        Some(d)
    };
    let mut tabs: Vec<Tab> = Vec::new();
    for (chain, dir) in opening_order(chains, desk.is_some())
        .into_iter()
        .map(|i| (chains[i], &dirs[i]))
    {
        if chain.opening() == Opening::Spawned {
            continue;
        }
        let sid = spawn_tab(&door, dir)?;
        let titled = if let Some(d) = &desk {
            let r = on(&door, &sid, &["meta", "set", "title", chain.name()]);
            let w = surfaces::window_of(&door.out(&["ls"]), &sid).unwrap_or_default();
            if w != d.chains_window {
                return Err(could_not(&format!(
                    "{}: spawned in window {w:?}, not the chains' window {}",
                    chain.name(),
                    d.chains_window
                )));
            }
            format!(", titled in window {w} ({})", r.trim())
        } else {
            String::new()
        };
        let script = match &scroll_script {
            Some(own) if chain == Chain::TallScroll => own.clone(),
            _ => launch.of_chain(&run, chain)?,
        };
        let launched = launch_tab(&door, &sid, chain, &script)?;
        println!(
            "  tab       {} @{sid} in {}: {}{titled}, launched",
            chain.name(),
            dir.display(),
            launched.said,
        );
        // THE LAUNCHES ARE STAGGERED (slice at1): the next tab's program starts only once this
        // one's has finished starting, so no program starts beside another still starting.
        if ready_gate {
            let started = started_composer(&door, &sid, h.name(), (&mut holds, chain.name()))?;
            if let Some(why) = start_failed(&started, h.name()) {
                for l in tail_rows(&door, &sid, 12) {
                    println!("    | {l}");
                }
                return Err(could_not(&format!("{why}, in {}", chain.name())));
            }
            println!(
                "  started   {} @{sid}: {}",
                chain.name(),
                started_line(&started)
            );
        }
        tabs.push(Tab {
            chain,
            sid,
            dir: dir.clone(),
            seen: Seen {
                window: surfaces::Window {
                    label: chain.name().to_string(),
                    ..surfaces::Window::default()
                },
                task: launched.task.into_iter().collect(),
                ..Seen::default()
            },
            journal_cursor: crate::util::LineTailCursor::default(),
            ledger_cursor: crate::util::LineTailCursor::default(),
            t0: 0,
            ask_tried: false,
            gate_opened: false,
            over: false,
        });
    }
    let ask_sid = match (&desk, tabs.iter().find(|t| t.chain == Chain::CruiseN)) {
        (Some(d), Some(cruise_n)) => {
            let sid = spawn_tab(&door, &cruise_n.dir)?;
            let r = on(
                &door,
                &sid,
                &["meta", "set", "title", surfaces::stopped::ASK_TAB],
            );
            let w = surfaces::window_of(&door.out(&["ls"]), &sid).unwrap_or_default();
            if w != d.chains_window {
                return Err(could_not(&format!(
                    "{}: spawned in window {w:?}, not the chains' window {}",
                    surfaces::stopped::ASK_TAB,
                    d.chains_window
                )));
            }
            println!(
                "  tab       {} @{sid}: a plain shell beside cruise-n, titled in window {w} ({}), \
                 for N4's stand-in ask",
                surfaces::stopped::ASK_TAB,
                r.trim()
            );
            Some(sid)
        }
        _ => None,
    };
    if let Some(d) = desk.as_mut()
        && let Some(i) = surfaces::front_of(&tabs.iter().map(|t| t.chain).collect::<Vec<_>>())
    {
        let label = tabs[i].seen.window.label.clone();
        let strip = surfaces::strip_tabs(&door.out(&["chrome"]));
        let Some(at) = strip.iter().find(|t| t.label == label).map(|t| t.index + 1) else {
            return Err(could_not(&format!(
                "{label} is not in the chains' window's strip"
            )));
        };
        let brought = door.out(&["tab", &at.to_string()]);
        let strip = surfaces::strip_tabs(&door.out(&["chrome"]));
        if !strip.iter().any(|t| t.label == label && t.selected) {
            return Err(could_not(&format!(
                "tab {at} ({label}): {}, yet not the selected tab",
                brought.trim()
            )));
        }
        let front = &mut tabs[i];
        d.front_sid.clone_from(&front.sid);
        front.seen.window.front_tab = true;
        println!(
            "  front     {} @{} (tab {at}: {}): the chains' window's front tab, captured at each look",
            front.chain.name(),
            front.sid,
            brought.trim()
        );
    }
    if desk.as_ref().is_some_and(|d| d.front_sid.is_empty()) {
        println!(
            "  front     none: the run holds no chain to put in front ({}), so no tab is captured and END's look is not run",
            surfaces::FRONT.map(Chain::name).join(", ")
        );
    }
    for tab in &mut tabs {
        if let Some(why) = empty_composer(&door, &tab.sid, h.name())?.why(h.name()) {
            for l in tail_rows(&door, &tab.sid, 12) {
                println!("    | {l}");
            }
            return Err(could_not(&format!("{why}, in {}", tab.chain.name())));
        }
    }
    let front_label = desk
        .as_ref()
        .and_then(|d| tabs.iter().find(|t| t.sid == d.front_sid))
        .map(|t| t.seen.window.label.clone());
    if desk.is_some() {
        surfaces::opening_sweep(&door, &mut tabs, front_label.as_deref(), false);
    }
    let chains_now: Vec<Chain> = tabs.iter().map(|t| t.chain).collect();
    for i in prompt_order(&chains_now) {
        let tab = &mut tabs[i];
        // The prompts go in a second apart, and the look below that the program is ready takes
        // its time out of that second, not after it.
        let spaced = Instant::now() + Duration::from_secs(1);
        let opening = tab.chain.opening();
        if opening == Opening::Person {
            let on_tab = crate::wait_until(Duration::from_secs(60), || {
                word(&on(&door, &tab.sid, &["status"]), "supervisor").is_some_and(|w| w != "-")
            });
            if !on_tab {
                return Err(could_not(&format!(
                    "no supervisor on {} within 60 s of its composer",
                    tab.chain.name()
                )));
            }
        }
        // No clock starts and no tab is prompted over input its program has not read.
        if ready_gate {
            let ready =
                started_composer(&door, &tab.sid, h.name(), (&mut holds, tab.chain.name()))?;
            if let Some(why) = start_failed(&ready, h.name()) {
                for l in tail_rows(&door, &tab.sid, 12) {
                    println!("    | {l}");
                }
                return Err(could_not(&format!("{why}, in {}", tab.chain.name())));
            }
            if ready.held.is_some() || ready.capture.is_some() {
                println!(
                    "  ready     {} @{}: before its prompt, {}",
                    tab.chain.name(),
                    tab.sid,
                    started_line(&ready)
                );
            }
        }
        stop::sleep(spaced.saturating_duration_since(Instant::now()))?;
        tab.t0 = now_ms();
        if tab.chain.walled() {
            crate::verbs::set_mode(
                &run,
                &format!(
                    "limit-after:1:{WALL_CLAIM}:{}:{MODE}",
                    tab.chain.wall_ahead_ms(),
                ),
            )?;
        }
        outage::step(&run, tab, tab.t0)?;
        let took = match opening {
            Opening::Prompt => tab_turn(&door, &tab.sid, h.name(), &tab.chain.prompt())?,
            Opening::Person if window => {
                let enter = person_keys(&door, &tab.sid, h.name(), &tab.chain.prompt())?;
                tab.seen.cover.person_ms = enter.as_ref().map(|(t, _, _)| *t);
                if let Some((_, stamp, keyed)) = enter {
                    tab.seen.cover.person_stamp = Some(stamp);
                    tab.seen.cover.keyed = Some(keyed);
                }
                tab.seen.cover.person_ms.is_some()
            }
            Opening::Person => {
                let enter = person_turn(&door, &tab.sid, h.name(), &tab.chain.prompt())?;
                tab.seen.cover.person_ms = enter.as_ref().map(|(t, _)| *t);
                tab.seen.cover.person_stamp = enter.map(|(_, h)| h);
                tab.seen.cover.person_ms.is_some()
            }
            Opening::Idle | Opening::Spawned => {
                println!(
                    "  idle      {} @{}: at its composer, no prompt",
                    tab.chain.name(),
                    tab.sid
                );
                continue;
            }
        };
        if !took {
            for l in tail_rows(&door, &tab.sid, 12) {
                println!("    | {l}");
            }
            return Err(could_not(&format!(
                "the composer of {} did not take the first prompt",
                tab.chain.name()
            )));
        }
        println!(
            "  prompt    {} @{}: \"{}\"{}{}",
            tab.chain.name(),
            tab.sid,
            tab.chain.prompt(),
            tab.seen
                .cover
                .person_ms
                .map_or_else(String::new, |p| format!(
                    " with a standing draft ({} s, Enter at +{}; status human_ms={})",
                    PERSON_STAND_MS / 1000,
                    at(tab.t0, p),
                    tab.seen.cover.person_stamp.as_deref().unwrap_or("?")
                )),
            tab.seen
                .cover
                .keyed
                .as_ref()
                .map_or_else(String::new, |k| format!("; {}", cover::keyed_line(k)))
        );
    }
    let mut awaiting = None;
    if let Some((_, wdir)) = chains
        .iter()
        .zip(&dirs)
        .find(|(c, _)| c.opening() == Opening::Spawned)
    {
        let (mut tab, slot) = spawn_worker((tag, &run, h), &door, &tabs, wdir)?;
        if let Some(d) = &desk {
            surfaces::open_worker(&door, &mut tab, front_label.as_deref(), &d.front_sid);
        }
        tabs.push(tab);
        awaiting = Some(slot);
    }
    let watch0 = now_ms();
    loop {
        let now = now_ms();
        // Under the surfaces, this pass's glance: none inside where its `chrome` read failed,
        // which notes nothing of any tab (no empty strip or menu stands in for it); the surfaces'
        // steps run all the same.
        let glance = desk.as_mut().map(|d| match surfaces::glance(&door, d) {
            Ok(g) => {
                for (what, v) in d.note(&g) {
                    println!(
                        "  [desk] +{} {what:<8} {}",
                        at(watch0, g.t),
                        crate::util::one_line(&v, 240)
                    );
                }
                Some(g)
            }
            Err(e) => {
                println!(
                    "  [desk] +{} chrome   unread, nothing noted: {}",
                    at(watch0, now_ms()),
                    crate::util::one_line(&e, 240)
                );
                None
            }
        });
        for tab in tabs.iter_mut().filter(|t| !t.over) {
            look(&run, &door, tab);
            if let Some(g) = &glance {
                if let Some(g) = g {
                    for (what, v) in tab.seen.window.note(g, &tab.sid) {
                        println!(
                            "  [{}] +{} {what:<7} {}",
                            tab.chain.name(),
                            at(tab.t0, g.t),
                            crate::util::one_line(&v, 240)
                        );
                    }
                }
                surfaces::held::look_step(&door, tab, front_label.as_deref());
                surfaces::stopped::ask_step(&door, tab, ask_sid.as_deref());
            }
            open_need_gate(&run, tab, glance.is_some());
            land_contradicted_work(&run, tab);
            if surfaces::held::person_due(tab.chain, &tab.seen, tab.t0, now) {
                let began = now_ms();
                let typed = person_turn(&door, &tab.sid, h.name(), surfaces::held::PERSON_TURN)?;
                let sent = typed.as_ref().map(|(t, _)| *t);
                surfaces::held::person_typed(&mut tab.seen, began, sent);
                println!(
                    "  [{}] +{} person  their turn into the held tab: \"{}\"{}",
                    tab.chain.name(),
                    at(tab.t0, began),
                    surfaces::held::PERSON_TURN,
                    sent.map_or_else(
                        || " — the composer did not take it".to_string(),
                        |t| format!(", sent at +{}", at(tab.t0, t))
                    )
                );
            }
            if let Some(ask) = tab.chain.asks()
                && !tab.ask_tried
                && ask_due(&tab.seen, now)
                && stand_on(&door, &tab.sid, h.name(), &on(&door, &tab.sid, &["text"]))
                    == Stand::Composer(String::new())
            {
                tab.ask_tried = true;
                let asked = now_ms();
                let (took, entered) = if tab.chain.asks_as_person() {
                    match person_turn(&door, &tab.sid, h.name(), &ask)? {
                        Some((pressed, stamp)) => {
                            tab.seen.cover.ask_stamp = Some(stamp);
                            (true, pressed)
                        }
                        None => (false, asked),
                    }
                } else {
                    (tab_turn(&door, &tab.sid, h.name(), &ask)?, asked)
                };
                println!(
                    "  [{}] +{} prompt  a person's, mid-chain: \"{ask}\"{}",
                    tab.chain.name(),
                    at(tab.t0, asked),
                    match (took, tab.seen.cover.ask_stamp.as_deref()) {
                        (false, _) => " — the composer did not take it".to_string(),
                        (true, Some(stamp)) => format!(
                            " as a person types it (the draft stood {} s, Enter at +{}; status human_ms={stamp})",
                            PERSON_STAND_MS / 1000,
                            at(tab.t0, entered)
                        ),
                        (true, None) => String::new(),
                    }
                );
                if took {
                    tab.seen.asked_ms = Some(entered);
                } else {
                    tab.seen.ask_refused_ms = Some(asked);
                }
            }
            wall_step(&run, tab, now)?;
            outage::step(&run, tab, now)?;
            sysmsg::step(&run, &door, h, tab, now)?;
            hide_step(&door, h, tab, now)?;
            switch_step(&run, &door, h, tab)?;
            relaunch_step(&run, &door, h, tab)?;
            tap::step(&run, &door, h, tab)?;
            unread_step(&run, tab, now);
            resize_step(&door, tab, now)?;
            scroll_step(&door, tab, now)?;
            first_screen_step(&door, tab);
            switch::word_step(
                &door,
                h,
                tab,
                now,
                desk.as_ref().map(|d| d.page_view.as_str()),
            )?;
            if let Some(text) = tab.chain.orchestrated()
                && orchestrator_due(tab.chain, &tab.seen, now)
            {
                let sent = now_ms();
                if tab_turn(&door, &tab.sid, h.name(), text)? {
                    tab.seen.cover.turn_ms = Some(sent);
                    println!(
                        "  [{}] +{} turn    the orchestrator's \"{text}\"",
                        tab.chain.name(),
                        at(tab.t0, sent)
                    );
                } else {
                    println!(
                        "  [{}] +{} turn    the composer did NOT take the orchestrator's turn: watched no more",
                        tab.chain.name(),
                        at(tab.t0, sent)
                    );
                    tab.over = true;
                    continue;
                }
            }
            if finished(tab, now) {
                tab.over = true;
                if receipt::reads_head(tab.chain) {
                    tab.seen.head = receipt::head_of(&run.w.join("home"), &tab.dir);
                    println!(
                        "  [{}] repo    HEAD {}",
                        tab.chain.name(),
                        tab.seen.head.as_deref().unwrap_or("not read")
                    );
                }
                tab.seen.stub = stub_witness(&run, tab);
                tab.seen.lift_stub = lift_witness(&run, tab);
                if let Some(n) = tab.seen.lift_stub {
                    println!(
                        "  [{}] stub    its hold was lifted; the stub read {n} request(s) of its conversation between",
                        tab.chain.name()
                    );
                }
                tab.seen.cover.window_stub = window_witness(&run, tab);
                tab.seen.cover.went_on_ms = went_on_witness(&run, tab);
                tab.seen.cover.outage_resumed = outage::resumed_witness(&run, tab);
                if let Some(n) = tab.seen.cover.outage_resumed {
                    println!(
                        "  [{}] stub    after the outage's lift it read {n} request(s) of the conversation carrying the resume",
                        tab.chain.name()
                    );
                }
                if tab.chain.repo() == Repo::Others {
                    tab.seen.cover.others_left =
                        Some(angles::others_left(&run.w.join("home"), &tab.dir));
                }
                tab.seen.cover.owner_texts = owner_witness(&run, tab);
                tab.seen.cover.check_texts = check_witness(&run, tab);
                if let Some(texts) = &tab.seen.cover.owner_texts {
                    println!(
                        "  [{}] stub    the owner's words reached the agent in {} text(s), {} of them holding a line break",
                        tab.chain.name(),
                        texts.len(),
                        texts.iter().filter(|t| t.contains(['\n', '\r'])).count()
                    );
                }
                if tab.chain == Chain::ToolEnd {
                    tab.seen.cover.tool_results = Some(tool_results_of(&run, &tab.chain.token()));
                }
                if switch::at_once(tab.chain) {
                    let typed = switch::typed_witness(&run, &tab.sid);
                    println!(
                        "  [{}] typed   {}",
                        tab.chain.name(),
                        switch::typed_line(typed.as_deref(), tab.t0)
                    );
                    tab.seen.cover.typed = typed;
                }
                if matches!(tab.chain, Chain::LandDialog | Chain::CruiseDialog) {
                    let answers = dialog_answers_of(&run, &tab.chain.token());
                    println!(
                        "  [{}] stub    the dialog's answer came back in {} tool result(s)",
                        tab.chain.name(),
                        answers.len()
                    );
                    tab.seen.cover.dialog_answers = Some(answers);
                }
                if switch::switches(tab.chain) {
                    let woke = switch::woke_witness(&run, &tab.sid);
                    println!(
                        "  [{}] tape    {}",
                        tab.chain.name(),
                        switch::woke_line(woke.as_deref(), tab.t0)
                    );
                    tab.seen.cover.woke = woke;
                }
                if tab.chain == Chain::Worker {
                    let aim = format!("@{}", tab.sid);
                    let r = door.ctl(&[&aim, "result"]);
                    tab.seen.cover.result =
                        Some(r.as_ref().map_or_else(Clone::clone, |r| r.stdout.clone()));
                    tab.seen.cover.result_head = r.ok().map(|r| r.stderr.trim().to_string());
                    tab.seen.cover.worker = word(&on(&door, &tab.sid, &["meta"]), "worker");
                    println!(
                        "  [{}] result  {} (meta worker={})",
                        tab.chain.name(),
                        tab.seen.cover.result_head.as_deref().unwrap_or("not read"),
                        tab.seen.cover.worker.as_deref().unwrap_or("?")
                    );
                    tab.seen.cover.awaited = awaiting.as_ref().and_then(|slot| {
                        slot.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                    });
                    println!(
                        "  [{}] await   {}",
                        tab.chain.name(),
                        tab.seen.cover.awaited.as_ref().map_or_else(
                            || "the manager's await task had NOT returned".to_string(),
                            |(t, code, out)| format!(
                                "returned at +{} (exit {code}): {}",
                                at(tab.t0, *t),
                                crate::util::one_line(out, 200)
                            )
                        )
                    );
                }
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
        sysmsg::mate_step(&door, h, &mut tabs, now)?;
        if tabs.iter().all(|t| t.over) {
            break;
        }
        stop::sleep(Duration::from_secs(2))?;
    }
    let ending = surfaces::end_runs(&tabs.iter().map(|t| t.chain).collect::<Vec<_>>());
    if let Some(d) = desk.as_mut()
        && ending
    {
        surfaces::end_steps(&run, &door, &mut tabs, d)?;
    }
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
        println!(
            "  first     {} @{}: {}",
            tab.chain.name(),
            tab.sid,
            first_point_line(&tab.seen, tab.t0)
        );
        for (s, v) in judge(tab.chain, &tab.seen, h.name(), tab.t0) {
            println!("forever {} {} {}: {}", h.name(), s.id(), v.word(), v.why());
            all.push((s, v));
        }
    }
    match (&desk, unread) {
        (Some(d), _) if ending => {
            let windows: Vec<(Chain, &surfaces::Window)> =
                tabs.iter().map(|t| (t.chain, &t.seen.window)).collect();
            let v = surfaces::judge_end(d, &windows);
            println!("forever {} END {}: {}", h.name(), v.word(), v.why());
            all.push((Scenario::End, v));
        }
        (Some(_), _) => println!(
            "forever {} END SKIPPED: the run holds no land-j and land-k, whose ends END wants standing",
            h.name()
        ),
        (None, why) => println!(
            "forever {} END SKIPPED: no window read ({})",
            h.name(),
            why.unwrap_or("-")
        ),
    }
    Ok(all)
}

fn run_lab(
    tag: &str,
    h: Harness,
    mode: Mode,
    chains: &[Chain],
    keep: bool,
    (surfaces_on, window, tapped, login, ready_gate): (
        bool,
        bool,
        bool,
        sysmsg::login::LoginArm,
        bool,
    ),
) -> Result<Vec<(Scenario, Verdict)>, Fail> {
    if is_run(tag) {
        return Err(Fail::usage(format!(
            "forever: lab '{tag}' is up: forever brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    let mut u = lab_opts(
        crate::lab::UpOpts::from_env(),
        mode,
        keep,
        (surfaces_on, window),
    )?;
    if chains.contains(&Chain::CruiseO) {
        u.harness_keys = format!("{}\n{}", u.harness_keys, owner_keys());
    }
    // A stored login's process is the default route's only where the instance admits the stub's
    // base URL ([`sysmsg::login::stored_arm`]).
    u.route_seam |= sysmsg::login::stored_arm(h, chains, login);
    if let Some(w) = mode.walled()
        && let Some(why) = wall_out_of_reach(h, mode)
    {
        println!(
            "forever: {} {} is out of this lab's reach: {why}",
            h.name(),
            w.id()
        );
        return Ok(vec![(w, Verdict::Unobserved(why.to_string()))]);
    }
    if let Err(f) = crate::lab::up(tag, &u) {
        if f.stopped.is_some() {
            return Err(f);
        }
        return Err(could_not(&format!(
            "the lab did not come up: {}",
            f.msg.unwrap_or_default()
        )));
    }
    if window {
        let r = Run::load(tag).ok();
        println!(
            "forever: lab '{tag}' OPENED A WINDOW on this desk (instance pid {}): L13 checks native controller delivery through hwkey, not person takeover; same-land's hold is read off it",
            r.map_or(0, |r| r.instance_pid)
        );
    }
    let verdicts = run_check(
        tag,
        h,
        chains,
        (surfaces_on, window, tapped, login, ready_gate),
    );
    if keep {
        println!("forever: lab '{tag}' left up ({LAB} down {tag})");
    } else {
        let _ = crate::lab::down(tag, true);
    }
    verdicts
}

fn lab_opts(
    base: crate::lab::UpOpts,
    mode: Mode,
    keep: bool,
    (surfaces_on, window): (bool, bool),
) -> Result<crate::lab::UpOpts, Fail> {
    let mut u = base;
    u.trust = true;
    u.lifeline = !keep;
    u.window = window;
    if surfaces_on && !u.instance_env.contains(PERSON_SEAM) {
        u.instance_env = format!("{} {PERSON_SEAM}", u.instance_env)
            .trim()
            .to_string();
    }
    if mode == Mode::Cap {
        u.harness_keys = format!("{}\ncontinue_per_hour = {CAP_PER_HOUR}", u.harness_keys);
    }
    let scaled = match mode {
        Mode::Ladder | Mode::Angles => Some(ladder_env()),
        Mode::OutageLand | Mode::OutageCruise => Some(outage::env()),
        _ => None,
    };
    if let Some(env) = scaled {
        u.instance_env = format!("{} {env}", u.instance_env).trim().to_string();
    }
    match crate::lab::opts_refused(&u) {
        Some(why) => Err(Fail::usage(format!("forever: {why}"))),
        None => Ok(u),
    }
}

const PERSON_SEAM: &str = "ATERM_DEBUG_PERSON_INVOKE=1";

#[derive(Debug, PartialEq, Eq)]
struct Asked {
    harnesses: Vec<Harness>,
    mode: Mode,
    chains: Option<Vec<Chain>>,
    tag: Option<String>,
    keep: bool,
    surfaces: bool,
    window: bool,
    /// `--untapped`: W10's and W11's live negative control, no usage tap on Claude Code's line.
    untapped: bool,
    /// `--token-login`: Claude Code's expired-login chains on the environment's OAuth token
    /// (`claude --subscriber`), where nothing names the login's method, not on a stored login
    /// ([`sysmsg::login::stored_arm`]); `--forced-login`: on the stored login under settings that
    /// pin its method ([`sysmsg::login::lay_pin`]).
    login: sysmsg::login::LoginArm,
    /// `--no-ready-gate`: every tab launched at once and prompted with no look that its program
    /// has finished starting, as the lab ran before slice at1, so Land writes behind input the
    /// program has not read and the held-text path (its `NOTE`, the late draw) is exercised.
    ungated: bool,
}

impl Asked {
    fn chains(&self, h: Harness) -> Vec<Chain> {
        let mode = Chain::of_mode_on(self.mode, h);
        let picked = |off: bool| -> Vec<Chain> {
            mode.iter()
                .copied()
                .filter(|c| {
                    self.chains.as_ref().is_none_or(|only| {
                        only.contains(c)
                            || (*c == Chain::Mgr && only.contains(&Chain::Worker))
                            || (*c == Chain::Off && off)
                    })
                })
                .collect()
        };
        let chains = picked(false);
        if self.chains.is_some()
            && surfaces::unread_why(self.surfaces, &chains).is_none()
            && surfaces::front_of(&chains).is_none()
        {
            picked(true)
        } else {
            chains
        }
    }
}

fn parse_args(args: &[String]) -> Result<Option<Asked>, Fail> {
    let usage = |m: &str| Fail::usage(format!("forever: {m}"));
    let mut harnesses = None;
    let mut mode = Mode::Both;
    let mut chains = None;
    let mut tag = None;
    let mut keep = false;
    let mut surfaces = true;
    let mut window = false;
    let mut untapped = false;
    let mut token_login = false;
    let mut forced_login = false;
    let mut ungated = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--harness" => {
                let w = it.next().ok_or_else(|| usage("--harness needs a word"))?;
                harnesses = Some(if w == "all" {
                    Harness::ALL.to_vec()
                } else {
                    vec![Harness::of(w).ok_or_else(|| {
                        usage(&format!("unknown harness '{w}' (claude, agy, muse, all)"))
                    })?]
                });
            }
            "--mode" => {
                let w = it.next().map(String::as_str);
                mode = w.and_then(Mode::of).ok_or_else(|| {
                    usage(&format!(
                        "--mode takes land, cruise, both, cover, cap, wall, wall-hid, wall-redraw, wall-switch, wall-unread, wall-cruise, wall-relaunch, wall-tap, wall-tap-narrow, resid, ladder, angles, switch, outage-land, outage-cruise, sysmsg or dialog, not {w:?}"
                    ))
                })?;
            }
            "--chains" => {
                let w = it.next().ok_or_else(|| usage("--chains needs a list"))?;
                let mut only = Vec::new();
                for name in w.split(',').filter(|n| !n.is_empty()) {
                    let every = Chain::every();
                    only.push(
                        every
                            .iter()
                            .copied()
                            .find(|c| c.name() == name)
                            .ok_or_else(|| {
                                usage(&format!(
                                    "unknown chain '{name}' ({})",
                                    every
                                        .iter()
                                        .map(|c| c.name())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ))
                            })?,
                    );
                }
                if only.is_empty() {
                    return Err(usage("--chains needs a list"));
                }
                chains = Some(only);
            }
            "--keep" => keep = true,
            "--no-surfaces" => surfaces = false,
            "--window" => window = true,
            "--untapped" => untapped = true,
            "--token-login" => token_login = true,
            "--forced-login" => forced_login = true,
            "--no-ready-gate" => ungated = true,
            "-h" | "--help" => return Ok(None),
            s if s.starts_with('-') => return Err(usage(&format!("unknown option '{s}'"))),
            s => tag = Some(s.to_string()),
        }
    }
    let harnesses = harnesses.ok_or_else(|| usage("say --harness claude|agy|muse|all"))?;
    let asked = Asked {
        harnesses,
        mode,
        chains,
        tag,
        keep,
        surfaces,
        window,
        untapped,
        login: match (token_login, forced_login) {
            (true, true) => {
                return Err(usage(
                    "--token-login and --forced-login are two ways in: name one",
                ));
            }
            (true, false) => sysmsg::login::LoginArm::Token,
            (false, true) => sysmsg::login::LoginArm::Forced,
            (false, false) => sysmsg::login::LoginArm::Stored,
        },
        ungated,
    };
    if asked.login != sysmsg::login::LoginArm::Stored && asked.mode != Mode::Sysmsg {
        return Err(usage(
            "--token-login and --forced-login run the expired login's chains: they take --mode sysmsg",
        ));
    }
    if asked.untapped && !matches!(asked.mode, Mode::WallTap | Mode::WallTapNarrow) {
        return Err(usage(
            "--untapped is the tapped walls' negative control: it takes --mode wall-tap or wall-tap-narrow",
        ));
    }
    if asked.window
        && !asked.harnesses.iter().any(|&h| {
            asked
                .chains(h)
                .iter()
                .any(|c| c.opening() == Opening::Person || *c == Chain::SameLand)
        })
    {
        return Err(usage(
            "--window opens a window for native controller keys (hwkey) or for same-land's hold read off \
             a real window (its chip's screen-reader node): the run needs land-p \
             (--mode cover, --chains land-p) or same-land (--chains same-land)",
        ));
    }
    Ok(Some(asked))
}

/// `forever --harness <claude|agy|muse|all> [--mode MODE] [--chains a,b] [--no-surfaces]
/// [--window] [--untapped] [--token-login] [--no-ready-gate] [tag] [--keep]`, `MODE` one of `land`, `cruise`, `both`, `cover`,
/// `cap`, `wall`, `wall-hid`, `wall-redraw`, `wall-switch`, `wall-unread`, `wall-cruise`,
/// `wall-relaunch`, `wall-tap`, `wall-tap-narrow`, `resid`, `ladder`, `angles`, `switch`, `outage-land`,
/// `outage-cruise`, `sysmsg` (each form's chain on a lab of its own, tagged `<tag>-<form>`) or
/// `dialog`.
pub fn cmd_forever(args: &[String]) -> Res {
    let Some(a) = parse_args(args)? else {
        print!("{}", crate::USAGE);
        return Ok(0);
    };
    let base = a
        .tag
        .clone()
        .unwrap_or_else(|| format!("forever-{}", std::process::id()));
    if a.mode.walled().is_none() && a.harnesses.iter().all(|&h| a.chains(h).is_empty()) {
        return Err(Fail::usage(
            "forever: --chains names no chain of the mode's".to_string(),
        ));
    }
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
        let chains = a.chains(h);
        if a.mode == Mode::Sysmsg {
            for form in sysmsg::Form::ALL {
                if let Some(why) = form.out_of_reach(h) {
                    println!(
                        "forever: {} {} is out of this lab's reach: {why}",
                        h.name(),
                        form.id()
                    );
                    lines.push((h, Scenario::Sys(form), Verdict::Unobserved(why.to_string())));
                }
            }
            for form in sysmsg::Form::LAND {
                if let Some(why) = form.out_of_reach(h) {
                    println!(
                        "forever: {} {} is out of this lab's reach: {why}",
                        h.name(),
                        form.land_id()
                    );
                    lines.push((
                        h,
                        Scenario::SysLand(form),
                        Verdict::Unobserved(why.to_string()),
                    ));
                }
            }
            // A note, not a verdict: no run could ever observe it, so a row of it would fail
            // every run's exit.
            for u in sysmsg::Unplayed::ALL {
                println!(
                    "forever: note: {} {} ({}) is out of this lab's reach: {}",
                    h.name(),
                    u.id(),
                    u.what(),
                    u.why()
                );
            }
        }
        if chains.is_empty() && a.mode.walled().is_none() {
            println!(
                "forever: {} runs none of the chains --chains asks of its mode",
                h.name()
            );
            continue;
        }
        // A SYSTEM MESSAGE'S CHAINS, EACH ON A LAB OF ITS OWN (`<tag>-<form>`): the stub's stand-in
        // meets every conversation it serves.
        if a.mode == Mode::Sysmsg {
            for c in chains {
                let Some(id) = c
                    .scenarios()
                    .first()
                    .map(|s| s.id())
                    .filter(|_| c != Chain::SysWitnessMate)
                else {
                    continue;
                };
                let tag = format!("{tag}-{}", id.to_ascii_lowercase());
                if !crate::paths::valid_tag(&tag) {
                    return Err(Fail::usage(format!(
                        "forever: bad tag '{tag}' (letters, digits, _ and -, at most 24): name a shorter one"
                    )));
                }
                // The witness lab runs its two tabs of one route together (v6 §14.17.5.4).
                let pair = [c, Chain::SysWitnessMate];
                let on_lab: &[Chain] = if c == Chain::SysWitness { &pair } else { &[c] };
                match run_lab(
                    &tag,
                    h,
                    a.mode,
                    on_lab,
                    a.keep,
                    (a.surfaces, a.window, !a.untapped, a.login, !a.ungated),
                ) {
                    Ok(v) => lines.extend(v.into_iter().map(|(s, v)| (h, s, v))),
                    Err(f) if f.code == 3 => {
                        eprintln!("aterm-lab: {}", f.msg.unwrap_or_default());
                        could_not_run = true;
                    }
                    Err(f) => return Err(f),
                }
            }
            continue;
        }
        match run_lab(
            &tag,
            h,
            a.mode,
            &chains,
            a.keep,
            (a.surfaces, a.window, !a.untapped, a.login, !a.ungated),
        ) {
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
            s.what_under(*h, a.login)
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

#[path = "forever_cover.rs"]
pub mod cover;

#[path = "forever_walls.rs"]
pub mod walls;

#[path = "forever_surfaces.rs"]
pub mod surfaces;

/// `seen`, a Land chain's records from before the land check, with the check its landing now
/// takes (`land-check@v1`, §14.17 item 5): typed a second before the first `DONE …
/// rule=land@v1`, at the defense's point (the last `EVENT idle` with words before it, moved
/// earlier where it is later than that), and ledgered typed. The landing then stands on the
/// check's answer. Records that have a check, or no landing, are left as they are.
#[cfg(test)]
pub(crate) fn with_land_check(mut seen: Seen) -> Seen {
    let landing = format!("rule={}", turn_end::RULE_LAND);
    let Some(i) = seen
        .journal
        .iter()
        .position(|(_, l)| l.starts_with("DONE ") && l.contains(&landing))
    else {
        return seen;
    };
    let d = seen.journal[i].0;
    if seen.journal[..i]
        .iter()
        .any(|(_, l)| act_rule(l) == Some(turn_end::RULE_LAND_CHECK))
    {
        return seen;
    }
    // The check comes a second before the landing its `LANDED` answers, past the landing's own
    // rows ([`AT_DONE_MS`]), and after the defense's point and the act before it.
    let after_act = seen.journal[..i]
        .iter()
        .rev()
        .find(|(_, l)| act_rule(l).is_some())
        .map_or(i64::MIN, |(t, _)| t + 1);
    let defense = seen.journal[..i]
        .iter()
        .rposition(|(_, l)| idle_words(l).is_some_and(|w| !w.is_empty() && w != "-"));
    let mut t = d - AT_DONE_MS - 500;
    if let Some(e) = defense {
        if seen.journal[e].0 >= t {
            seen.journal[e].0 = (t - 1).max(after_act);
        }
        t = t.max(seen.journal[e].0 + 1);
    }
    let seq = defense
        .and_then(|e| word(&seen.journal[e].1, "seq"))
        .or_else(|| word(&seen.journal[i].1, "seq"))
        .unwrap_or_else(|| "0".into());
    seen.journal.insert(
        i,
        (
            t,
            format!(
                "CONTINUED seq={seq} rule={} {}",
                turn_end::RULE_LAND_CHECK,
                turn_end::LAND_CHECK_HEAD
            ),
        ),
    );
    seen.journal.sort_by_key(|(at, _)| *at);
    let at = seen
        .ledger
        .iter()
        .position(|r| r.ts > t)
        .unwrap_or(seen.ledger.len());
    seen.ledger.insert(
        at,
        Row {
            ts: t,
            rule: turn_end::RULE_LAND_CHECK.into(),
            decision: "typed".into(),
            command: turn_end::LAND_CHECK.into(),
            reason: "the turn-end policy (short turns in a row: 1)".into(),
            box_seq: None,
        },
    );
    seen
}

#[path = "forever_angles.rs"]
pub mod angles;

#[path = "forever_switch.rs"]
pub mod switch;

#[path = "forever_paced.rs"]
pub mod paced;

#[path = "forever_outage.rs"]
pub mod outage;
#[path = "forever_sysmsg.rs"]
pub mod sysmsg;

#[path = "forever_tap.rs"]
pub mod tap;

#[path = "forever_receipt.rs"]
pub mod receipt;

#[path = "forever_checked.rs"]
pub mod checked;

#[path = "forever_input_hold.rs"]
pub mod input_hold;

#[cfg(test)]
#[path = "forever_tests.rs"]
mod tests;
