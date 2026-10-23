// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `forever --harness <claude|gemini|agy|muse|all> [--mode land|cruise|both|cover|cap|wall|wall-hid|wall-day|wall-climb|wall-shift|wall-redraw|wall-switch|wall-unread|wall-day-stop|wall-cruise|wall-relaunch|wall-bucket] [--chains a,b] [--no-surfaces] [--window] [tag] [--keep]`: Autopilot Land and Cruise, LIVE, on every harness aterm supervises — the real Claude Code, Gemini CLI, agy and Muse Code TUIs under a private headless aterm with the harness ON (its defaults), against the stub, each conversation scripted turn by turn ([`crate::stub::scripted_reply`]: the custom mode `forever`, whose rules pick a turn's reply by the scenario's word in the conversation and the act of the harness it answers).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_agent::supervise::approvals::{RULE_POINT, RULE_TASK_DONE, TASK_DONE};
use aterm_agent::supervise::policy::needs::NEEDS_ACK;
use aterm_agent::supervise::policy::turn_end;

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

/// A done claim (`says_done`), with no defense.
pub const DONE_CLAIM: &str = "Merged the parser and pushed it to origin/main. All done.";

const DEFENSE_HEAD: &str = "Nothing left:";

/// A clean defense: a `Nothing left:` line, no need, no open work.
pub const CLEAN: &str = "Nothing left: the parser is committed and pushed to origin/main.";

const ASK_KEY: &str = "a person asks: what stands now";

/// The reply to it: no done claim, no defense, no question, opening with none of Muse Code's tool-row heads.
pub const ASK_REPLY: &str = "Here is where it stands: the parser module is written and tested.";

/// The need L3 and L6 name.
pub const NEED: &str = "rotate the deploy key";

/// N1–N4's needs (`cruise-n`), each under the 64 cells the needs entry's writer keeps of a need, so every surface can read it whole.
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
#[expect(
    clippy::cast_precision_loss,
    reason = "a few seconds in ms, exact in an f64"
)]
const LONG_PACE_S: f64 = LONG_PACE_MS as f64 / 1000.0;

/// How long a long reply of `text` keeps its agent busy at the least: its hold, and its pace before each of its deltas ([`crate::stub::split_deltas`]).
#[must_use]
pub fn long_reply_ms(text: &str) -> i64 {
    let words = i64::try_from(crate::stub::split_deltas(text).len()).unwrap_or(0);
    LONG_DELAY_MS + words * LONG_PACE_MS
}

const LAND_KEY: &str = "Autopilot is Land:";
const NUDGE_KEY: &str = "Still landing:";
const CHECK_KEY: &str = "done is never the end";
const CONTINUE_KEY: &str = "Autopilot is Cruise: keep going";

const OPEN_SUFFIX: &str = "Your report still names open work";
const REPO_SUFFIX: &str = "Your repository still holds work";

const CARRYING_ON: &str = "— carrying on";

const QUIET_AFTER_LANDED_MIN_MS: i64 = 90_000;
const PAST_NEXT_STEP_MS: i64 = 60_000;
const QUIET_UNDER_OFF_MS: i64 = 270_000;
const CHAIN_LIMIT_MS: i64 = 16 * 60 * 1000;
const CRUISE_N_LIMIT_MS: i64 = 20 * 60 * 1000;
const WALL_DAY_LIMIT_MS: i64 = 45 * 60 * 1000;
const WALL_CLIMB_LIMIT_MS: i64 = 3 * 60 * 60 * 1000;
const WALL_SHIFT_LIMIT_MS: i64 = 35 * 60 * 1000;
const WALL_REDRAW_LIMIT_MS: i64 = 30 * 60 * 1000;
const WALL_SWITCH_LIMIT_MS: i64 = 30 * 60 * 1000;
const WALL_UNREAD_LIMIT_MS: i64 = 100 * 60 * 1000;
const WALL_CRUISE_LIMIT_MS: i64 = 25 * 60 * 1000;
const WALL_RELAUNCH_LIMIT_MS: i64 = 30 * 60 * 1000;
const AT_DONE_MS: i64 = 500;

const DUE_SLACK_MS: i64 = 2_000;

const PERSISTENCE_GRACE_MS: i64 = 60_000;

const UNJUDGED_MS: i64 = 300_000;

const WORK_KEY: &str = "Implemented the parser";

const OPEN_KEY: &str = "The docs PR is still open";

/// L12's question in prose (`classify_said`: a question, no choice, no destructive word; no done claim).
pub const QUESTION: &str = "Should the parser also accept tab-indented input?";
const QUESTION_KEY: &str = "tab-indented input";
const ANSWER_KEY: &str = "Nobody is here to choose:";
const ANSWER_HEAD: &str = "Nobody is here to ";

const PERSON_ASKS: &str = "tell me what this repository holds so far";
/// Its reply: a plain answer.
pub const PERSON_REPLY: &str = "The repository holds a README and nothing else yet.";
const ORCHESTRATED: &str = "now write the parser for this repository and land it";
const ORCHESTRATED_KEY: &str = "now write the parser";
const PERSON_STAND_MS: u64 = 8_000;
const DRAFT_HANG: Duration = Duration::from_secs(60);
const EXCHANGE_WATCH_MS: i64 = QUIET_UNDER_OFF_MS + 30_000;

/// P2's replies, each opening `Wrote ` (Muse Code's tool-row head) with a plain word after it: L14's work report, its need and defense, its clean defense; C3's done claim and its defense.
pub const W_WORK: &str = "Wrote the parser module and its unit tests in this turn.";
pub const W_NEEDS: &str = "Wrote the docs for the parser too.\nNEEDS YOU: rotate the deploy key\nNothing left: the parser is committed and pushed to origin/main.";
pub const W_DEFENSE: &str = "Wrote the last commit to origin/main.\nNothing left: the parser is committed and pushed to origin/main.";
pub const W_DONE: &str = "Wrote the parser and pushed it to origin/main. All done.";
pub const W_CHECKED: &str = "Wrote the last checks for the parser.\nNothing left: the parser is committed and pushed to origin/main.";
const W_WORK_KEY: &str = "Wrote the parser module";
const W_DONE_KEY: &str = "Wrote the parser and pushed";

/// C4: THE OWNER'S OWN CONTINUATION, IN TWO PARAGRAPHS (`[harness] continue_text`, set on the lab of a run holding `cruise-o`: [`owner_keys`]): a blank line, then a `Nothing left:` line and a `NEEDS YOU:` line of its own, every line a head agy's reader would take where a blank line in a message cut it (agy draws the rows under it two columns in, as a reply's: `aterm_phase::agy`'s header).
pub const OWNER_CONTINUE: &str = "Keep going on the owner's terms: finish the parser, verify it and land it.\n\nNothing left: is a line you write only once everything is on remote main.\nNEEDS YOU: lines are for credentials only.";
const OWNER_KEY: &str = "on the owner's terms";
const OWNER_TAIL: &str = "lines are for credentials only";
const DONE_CLAIM_KEY: &str = "Merged the parser";

/// The `[harness]` line that sets [`OWNER_CONTINUE`] as the lab's `continue_text`: a TOML basic string, its line breaks escaped.
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

/// The startup deadline of a worker's record (`harness_host::STARTUP_DEADLINE`, the crate's own): a record `working` as its starter left it whose agent shows no composer by then is marked `stalled why=startup`.
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

/// The grace past a reset the harness waits on `program` (the harness's word, [`Harness::name`]) before it goes on (`TurnEndTiming::grace_for`): 60 s for a program with no auto-continue (Muse Code, agy, Gemini CLI), 150 s for Claude Code, whose own continue comes 30 to 90 s past the reset.
#[must_use]
pub fn wall_grace_ms(program: &str) -> i64 {
    use aterm_phase::Program;
    let p = match Harness::of(program) {
        Some(Harness::Claude) => Program::Claude,
        Some(Harness::Gemini) => Program::Gemini,
        Some(Harness::Agy) => Program::Agy,
        Some(Harness::Muse) => Program::Muse,
        None => Program::Generic,
    };
    i64::try_from(turn_end::TurnEndTiming::default().grace_for(p).as_millis()).unwrap_or(i64::MAX)
}

/// The claim the stub's wall is raised with on harness `h` ([`wall_step`]): a five-hour window, which Claude Code reads off the unified-limit headers and Muse Code off the `usage_limit_reached` body; on agy's Gemini API key the claim names nothing and the stub answers the key's spent per-day quota; Gemini CLI gets a Google account's spent usage limit ([`crate::stub::gemini::ACCOUNT_CLAIM`]), the 429 on which its usage-limit dialog names the reset.
#[must_use]
pub const fn wall_claim(h: Harness) -> &'static str {
    match h {
        Harness::Gemini => crate::stub::gemini::ACCOUNT_CLAIM,
        Harness::Claude | Harness::Agy | Harness::Muse => WALL_CLAIM,
    }
}

/// The unix second a wall raised at `now` (ms) resets at: the first whole minute at least `ahead` ms on (W1's [`WALL_AHEAD_MS`], W2's further, [`Chain::wall_ahead_ms`]).
#[must_use]
pub const fn reset_s_ahead(now: i64, ahead: i64) -> i64 {
    (now + ahead + 59_999) / 60_000 * 60
}

const WALL_HID_AHEAD_MS: i64 = 420_000;
const WALL_DAY_AHEAD_MS: i64 = 25 * 60 * 1000;
const WALL_CLIMB_AHEAD_MS: i64 = 45 * 60 * 1000;
/// G3's shift ([`Chain::shifts`]): each raise again moves the reset this far on — past the loop's back-off after a try the dialog came back on (2 min, then 4: `approval_loop.rs`'s `LIMIT_RETRY_BACKOFF_S`), so each try waits the reset the dialog names, never the back-off alone.
pub const SHIFT_STEP_MS: i64 = 5 * 60 * 1000;
/// How many times G3's wall is raised again at its reset: the dialog drawn again with a new reset after each of two tries, the third getting through.
pub const SHIFT_TIMES: usize = 2;
/// W4's redraw: the resume typed past the first reset meets the wall raised again this far on.
pub const REDRAW_AHEAD_MS: i64 = 5 * 60 * 1000;
const WALL_SWITCH_AHEAD_MS: i64 = 15 * 60 * 1000;
const WALL_RELAUNCH_AHEAD_MS: i64 = 10 * 60 * 1000;
/// How long after the told W6's lab reads the conversation again ([`unread_restore_due`]).
pub const UNREAD_RESTORE_AFTER_TOLD_MS: i64 = 60_000;
/// How long past the told's due (the reset and [`futile_after_ms`]) W6's lab reads the conversation again with no told seen: the told is then late.
pub const UNREAD_TOLD_LATE_MS: i64 = 10 * 60 * 1000;
/// The words of the loop's hold on a limit whose conversation no read names (`turn_end.rs`'s `limit-conversation-unread`).
pub const UNREAD_WORDS: &str = "the remembered limit's conversation is unread";

/// The loop's bound on a limit held unread past its reset before the owner is told (`exception.rs`'s `FUTILE_AFTER`), in ms.
#[must_use]
pub fn futile_after_ms() -> i64 {
    i64::try_from(aterm_agent::supervise::policy::exception::FUTILE_AFTER.as_millis())
        .unwrap_or(i64::MAX)
}

/// The words of the owner's told for a limit held unread (`Exception::unread_limit`'s why): its `EXCEPTION` line carries them.
#[must_use]
pub fn unread_told_words() -> String {
    aterm_agent::supervise::policy::exception::Exception::unread_limit().why
}
/// The latest the notice may leave the screen: this long before the reset, so the loop reads and decides the screen without it while the wait stands.
pub const HIDE_BEFORE_RESET_MS: i64 = 120_000;
const HIDE_TRIES: usize = 32;
/// The local command whose output scrolls the notice away: Muse Code prints `  Rules loaded this session (in precedence order):`, `  user: none`, `  project: none` under the turn's end and sends no request (measured on 1.4.10, 2026-10-21).
pub const HIDE_COMMAND: &str = "/rules";
/// The words of Muse Code's usage wall (`◆ Usage limit reached · /upgrade (…) …`): a screen holding none of them has lost the notice.
pub const WALL_WORDS: &str = "Usage limit reached";
/// agy's local command whose output scrolls the notice away: with no `title` block in its settings (the lab's), `/title` prints `  ⎿  Error: No title command configured in settings.json.`
pub const AGY_HIDE_COMMAND: &str = "/title";
/// The words of agy's quota wall: the Gemini API's message in its `⚠ agent executor error: …` block, and the block's last row (`Error ID: <id>`), which leaves the screen last as the rows above it scroll away.
pub const AGY_WALL_WORDS: [&str; 2] = ["exceeded your current quota", "Error ID: "];
/// The option of Gemini CLI's usage-limit dialog that takes it away: `2.`
pub const GEMINI_HIDE_OPTION: &str = "2. Stop";
/// The words of Gemini CLI's usage-limit dialog: its title, and the reset row a Google account's spent quota draws under it.
pub const GEMINI_WALL_WORDS: [&str; 2] = ["Usage limit reached", "Access resets at"];
/// Claude Code's local command whose output scrolls the notice away once its countdown is cancelled ([`CLAUDE_CANCEL`]): `/rename <name>` prints `  ⎿ Session renamed to: <name>` under its own `❯ /rename <name>` row and sends no request; the composer's top rule carries the name from then on (`──── <name> ─`), and with an argument typed no command menu is drawn (measured on 2.1.307, 2026-10-21, the stub's five-hour limit standing: twelve of them took the notice off a 50-row tab; the stub read no request).
pub const CLAUDE_HIDE_COMMAND: &str = "/rename forever-w2";
/// The words of Claude Code's usage limit: the turn's `⎿  You've hit your session limit · resets …`, its `⏺ Usage limit reached · continuing automatically at …` row, and the footer's `⚠ Usage limit reached · limit resets …` banner.
pub const CLAUDE_WALL_WORDS: [&str; 4] = [
    "session limit",
    "Usage limit",
    "usage limit",
    "limit resets",
];
/// Claude Code's countdown, cancelled before the notice can be scrolled away ([`Hide::cancel`]): while it is armed its footer draws `⚠ Usage limit reached · limit resets …` and `Continuing automatically at … · esc to cancel` under every local command's rows; Esc cancels it — `⏺ Automatic continue cancelled · /rate-limit-options to re-arm` — and the banner leaves the footer (measured on 2.1.307, 2026-10-21).
pub const CLAUDE_CANCEL: Cancel = Cancel {
    key: "escape",
    armed: "esc to cancel",
    cancelled: "Automatic continue cancelled",
};

/// THE RESIDUALS, LIVE (`--mode resid`, `--mode ladder`): what the review's fix pass and `b9/resid3` left to their unit tests.
pub const LONG_TOKEN: &str = "lab/parser-module-parser-module-parser.rs";
/// The size `same-words`' tab is resized to once its first point is decided (`resize <rows> <cols>`): narrower, so the opener re-wraps, and taller, so more of the conversation above the reply comes into sight.
pub const RESIZED: (u16, u16) = (104, 62);
const _: () = assert!(RESIZED.0 < 144);
const RESIZE_AFTER_MS: i64 = 5_000;
const TURN_AFTER_RESIZE_MS: i64 = 30_000;
const SAME_TURN: &str = "now carry on with the parser module and report";
const SAME_TURN_KEY: &str = "now carry on with the parser module";
/// `tall-scroll`'s report rows ([`scroll_reply`]): with its head, the work report's words and each agent's own rows around them, a few rows taller than the tab's 50, so a few notches bring its opener back.
pub const SCROLL_LINES: usize = 46;
const SCROLL_AFTER_MS: i64 = 5_000;
const SCROLL_NOTCHES: u32 = 40;
const SCROLL_STILL: u32 = 3;
const SCROLL_HELD_MS: i64 = 10_000;
/// How long the chain is watched after the last notch: the loop has read the scrolled screen (its recheck is 5 s at the most) and decided there, long before the first point's due.
pub const SCROLL_WATCH_MS: i64 = 40_000;

/// F3's reply to its tool's result: no words, so the turn ends on the tool's row ([`read_call`]).
pub const TOOL_AFTER: &str = "";

/// The factor `--mode ladder` scales the short back-off ladder by (the product's development seam, `turn_end::LADDER_SCALE_SEAM`): an eighth, so the steps of 2, 4, 8, 16, 32 and 60 min are 15 s to 7.5 min, and the ladder to its cap and the cap decided again fit in about 16 min.
pub const LADDER_SCALE: f64 = 0.125;
/// How many back-offs `ladder` must see decided: the doubling steps to the cap (2, 4, 8, 16, 32, 60 min), and the cap once more.
pub const LADDER_STEPS: u32 = 7;
const LADDER_LIMIT_MS: i64 = 25 * 60 * 1000;
const STEP_SLACK_MS: i64 = 2_500;

/// The instance environment `--mode ladder` runs its aterm under: the ladder's seam at [`LADDER_SCALE`] (a development build reads it, a shipped one never).
#[must_use]
pub fn ladder_env() -> String {
    format!("{}={LADDER_SCALE}", turn_end::LADDER_SCALE_SEAM)
}

/// The shipped short back-off ladder, in minutes, one step per short turn in a row from the first: 2 min doubled to the 60 min cap, and the cap again — the judge's own word of it, which the product's timing is held to in a test (`forever_tests.rs`).
pub const SHIPPED_LADDER_MIN: [i64; LADDER_STEPS as usize] = [2, 4, 8, 16, 32, 60, 60];

/// The step the ladder names for a point that closed `streak` short turns ([`SHIPPED_LADDER_MIN`], the cap past its end), scaled by `scale`, in ms.
#[must_use]
pub fn ladder_step_ms(streak: u32, scale: f64) -> i64 {
    let i = usize::try_from(streak.max(1) - 1)
        .unwrap_or(usize::MAX)
        .min(SHIPPED_LADDER_MIN.len() - 1);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "a step of minutes scaled by a factor at most 1: far inside both"
    )]
    let ms = (SHIPPED_LADDER_MIN[i] as f64 * 60_000.0 * scale).round() as i64;
    ms
}

/// F3's tool call on harness `h`: a read of the chain's own `README.md` (`file`, its physical path), read-only, which each CLI runs with no confirm in the lab's jail: Claude Code's `Read`, Gemini CLI's `read_file`, agy's `view_file`, Muse Code's `read_file` (its path read from its workspace root: an absolute one only where its file policy allows).
#[must_use]
pub fn read_call(h: Harness, file: &Path) -> J {
    let abs = file.to_string_lossy().into_owned();
    let (name, input) = match h {
        Harness::Claude => ("Read", J::obj([("file_path", J::s(abs))])),
        Harness::Gemini => ("read_file", J::obj([("file_path", J::s(abs))])),
        Harness::Agy => (
            "view_file",
            J::obj([
                ("AbsolutePath", J::s(abs)),
                ("toolSummary", J::s("README read")),
                ("toolAction", J::s("Reading the README")),
            ]),
        ),
        Harness::Muse => ("read_file", J::obj([("path", J::s("README.md"))])),
    };
    J::obj([("name", J::s(name)), ("input", input)])
}

/// Why F3 is out of this lab's reach on harness `h`: no turn of its ends on a tool's row with no words after it (`None` where one does).
#[must_use]
pub fn tool_end_out_of_reach(h: Harness) -> Option<&'static str> {
    (h == Harness::Agy).then_some(
        "agy refuses a reply holding neither text nor a tool call: after the tool's result it sends \"Error: model output error: model output must contain either output text or tool calls, these cannot both be empty, please try again\" as a turn of its own and asks again, so no turn of its ends on a tool's row with no words after it",
    )
}

/// Why W1 or W2 (`mode`'s scenario) is out of this lab's reach on harness `h`, where it is: `None` where the stub draws the harness's own usage wall and, for W2, the lab takes its notice off the screen ([`hide_of`]).
#[must_use]
pub fn wall_out_of_reach(h: Harness, mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::Wall => None,
        Mode::WallHid => hide_of(h).err(),
        Mode::WallDay | Mode::WallClimb => match h {
            Harness::Gemini => None,
            Harness::Agy => Some(
                "agy draws a key's spent per-day quota as an inline error with its RetryInfo's delay, W1's own wall (`--mode wall`): no dialog, no ladder",
            ),
            Harness::Claude | Harness::Muse => Some(
                "the stub's per-day quota is the Gemini API's: only Gemini CLI draws it as a usage-limit dialog that names no reset",
            ),
        },
        Mode::WallShift => match h {
            Harness::Gemini => None,
            Harness::Claude | Harness::Agy | Harness::Muse => Some(
                "only Gemini CLI's usage-limit dialog takes a try (its Keep trying) that the quota answers by drawing it again: the others' resume meeting a new reset is `--mode wall-redraw`",
            ),
        },
        Mode::WallRedraw => hide_of(h).err(),
        Mode::WallSwitch => None,
        Mode::WallUnread => match h {
            Harness::Claude => None,
            Harness::Gemini | Harness::Agy | Harness::Muse => Some(
                "only Claude Code's conversation is read (its transcript): another agent's limit is the screen's, never held unread",
            ),
        },
        Mode::WallDayStop => wall_out_of_reach(h, Mode::WallDay),
        Mode::WallCruise => None,
        Mode::WallRelaunch => match h {
            Harness::Gemini | Harness::Agy | Harness::Muse => None,
            Harness::Claude => Some(
                "Claude Code's limit is its conversation's (its transcript), read again after any relaunch: the relaunch rule is the screen-alone agents' (Muse Code, agy, Gemini CLI)",
            ),
        },
        Mode::WallBucket => match h {
            Harness::Gemini => None,
            Harness::Claude | Harness::Agy | Harness::Muse => Some(
                "only Gemini CLI draws a per-model bucket's dialog (a Pro model's spent quota with a model to fall back to)",
            ),
        },
        Mode::Land
        | Mode::Cruise
        | Mode::Both
        | Mode::Cover
        | Mode::Cap
        | Mode::Resid
        | Mode::Ladder => None,
    }
}

/// How W2 takes the wall's notice off harness `h`'s screen while the wait stands ([`hide_step`]): a local command entered again and again (it sends no request), or the dialog's option chosen once; or why the harness has neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hide {
    /// What takes the notice away.
    pub by: HideBy,
    /// The words of the harness's usage wall: a screen holding none of them has lost the notice ([`notice_gone`]).
    pub words: &'static [&'static str],
    /// A key pressed once first, where the program draws the notice for as long as its own countdown is armed (Claude Code's [`CLAUDE_CANCEL`]).
    pub cancel: Option<Cancel>,
}

/// The key that cancels a program's own countdown to its reset, and the words that show it armed and cancelled ([`Hide::cancel`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancel {
    /// The key (`aterm ctl key`).
    pub key: &'static str,
    /// Words on the screen while the countdown is armed: the key is pressed only where they stand.
    pub armed: &'static str,
    /// Words the program draws once it is cancelled: the hide goes on only where they stand.
    pub cancelled: &'static str,
}

/// What takes a harness's notice away ([`Hide::by`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HideBy {
    /// A local command, typed into the empty composer and entered once the menu offers it first ([`menu_offers`]), again and again until the notice has scrolled away.
    Command(&'static str),
    /// The dialog's option, chosen once: the focus moved onto it (Down) and Enter pressed only once the dialog draws it focused ([`dialog_focus`]).
    Option(&'static str),
}

impl Hide {
    /// The command or the option, as the report names it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self.by {
            HideBy::Command(c) | HideBy::Option(c) => c,
        }
    }
}

/// [`Hide`] of harness `h`, or why none takes its notice away.
pub const fn hide_of(h: Harness) -> Result<Hide, &'static str> {
    match h {
        Harness::Muse => Ok(Hide {
            by: HideBy::Command(HIDE_COMMAND),
            words: &[WALL_WORDS],
            cancel: None,
        }),
        Harness::Agy => Ok(Hide {
            by: HideBy::Command(AGY_HIDE_COMMAND),
            words: &AGY_WALL_WORDS,
            cancel: None,
        }),
        Harness::Gemini => Ok(Hide {
            by: HideBy::Option(GEMINI_HIDE_OPTION),
            words: &GEMINI_WALL_WORDS,
            cancel: None,
        }),
        Harness::Claude => Ok(Hide {
            by: HideBy::Command(CLAUDE_HIDE_COMMAND),
            words: &CLAUDE_WALL_WORDS,
            cancel: Some(CLAUDE_CANCEL),
        }),
    }
}

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

    /// The verb's word, the lab's `agent`, the reader's program word (the needs entry's `<program>`: `Program::name`).
    #[must_use]
    pub const fn name(self) -> &'static str {
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

    fn bin_ver(self, run: &Run) -> (&str, &str) {
        match self {
            Harness::Claude => (&run.claude_bin, &run.claude_ver),
            Harness::Gemini => (&run.gemini_bin, &run.gemini_ver),
            Harness::Agy => (&run.agy_bin, &run.agy_ver),
            Harness::Muse => (&run.muse_bin, &run.muse_ver),
        }
    }
}

/// Which chains run: the measured pass's by Autopilot (`land`, `cruise`, `both`), the coverage chains (`cover`), or the hour's cap on a lab of its own (`cap`).
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
    /// W1 on the usage-limit dialog that names no reset (`wall-day`): Gemini CLI's on a key's spent per-day quota.
    WallDay,
    /// W1 on that dialog, its quota standing past the ladder's first rung (`wall-climb`): the ladder climbed while NEEDS YOU stands.
    WallClimb,
    /// W3, G3's climb across a changed reset (`wall-shift`): Gemini CLI's usage-limit dialog drawn again with a new reset after each try.
    WallShift,
    /// W4, the redraw after a typed resume (`wall-redraw`): the resume typed past the reset meets the wall again, under a new reset.
    WallRedraw,
    /// W5, the cross-program end (`wall-switch`): another agent started in the tab under the first one's remembered limit.
    WallSwitch,
    /// W6, the unread limit told (`wall-unread`): Claude Code's limit held unread past its reset by `FUTILE_AFTER`.
    WallUnread,
    /// W7, the per-day dialog answered Stop (`wall-day-stop`): Gemini CLI's usage-limit dialog that names no reset, answered `2. Stop` as a person would, kept trying on its ladder while NEEDS YOU stands.
    WallDayStop,
    /// W8, a usage wall under Cruise (`wall-cruise`).
    WallCruise,
    /// W9, the agent ended and launched again by a person under its standing wall (`wall-relaunch`).
    WallRelaunch,
    /// W10, Gemini CLI's per-model bucket (`wall-bucket`): its dialog's `Stop`, never its switch.
    WallBucket,
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
            "wall-day" => Mode::WallDay,
            "wall-climb" => Mode::WallClimb,
            "wall-shift" => Mode::WallShift,
            "wall-redraw" => Mode::WallRedraw,
            "wall-switch" => Mode::WallSwitch,
            "wall-unread" => Mode::WallUnread,
            "wall-day-stop" => Mode::WallDayStop,
            "wall-cruise" => Mode::WallCruise,
            "wall-relaunch" => Mode::WallRelaunch,
            "wall-bucket" => Mode::WallBucket,
            _ => return None,
        })
    }

    /// A mode of the stub's usage wall, on a lab of its own on each harness it reaches ([`wall_out_of_reach`]), and its one scenario.
    #[must_use]
    pub const fn walled(self) -> Option<Scenario> {
        match self {
            Mode::Wall | Mode::WallDay | Mode::WallClimb => Some(Scenario::W1),
            Mode::WallHid => Some(Scenario::W2),
            Mode::WallShift => Some(Scenario::W3),
            Mode::WallRedraw => Some(Scenario::W4),
            Mode::WallSwitch => Some(Scenario::W5),
            Mode::WallUnread => Some(Scenario::W6),
            Mode::WallDayStop => Some(Scenario::W7),
            Mode::WallCruise => Some(Scenario::W8),
            Mode::WallRelaunch => Some(Scenario::W9),
            Mode::WallBucket => Some(Scenario::W10),
            Mode::Land
            | Mode::Cruise
            | Mode::Both
            | Mode::Cover
            | Mode::Cap
            | Mode::Resid
            | Mode::Ladder => None,
        }
    }
}

/// What a chain's repository holds once laid ([`lay_repo`]), beside its `main`, committed and pushed to `origin`: the five legs of the repository probe (`aterm_agent::harness::repo_work::probe`) one at a time.
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
    /// What the repository suffix must name of it (`RepoWork::summary`'s words), and the words of the other legs it must not.
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
    /// F18 (a person's keystroke into an instant turn: P4C's chain, its person's prompt typed `as=person`).
    InstantPerson,
    /// F1 and F2 (a judged turn's opener re-wrapped and revealed by a resize) and SAME (a new turn answered with the very words of the one before).
    SameWords,
    /// F1 BY A SCROLL (a judged turn's opener scrolled into sight from the history above the screen by a wheel, nothing submitted).
    TallScroll,
    /// F3 (a first turn ended on a tool's row after a launch).
    ToolEnd,
    /// LADDER (the back-off ladder doubled to its cap, scaled).
    Ladder,
    /// W1 on Gemini CLI's usage-limit dialog that names no reset (a key's spent per-day quota), kept trying on the entitlement ladder while a person is told.
    WallDay,
    /// W1 on that dialog, its quota standing past the ladder's first rung: the ladder climbed while a person is told.
    WallClimb,
    /// W3 (G3): Gemini CLI's usage-limit dialog drawn again with a new reset after each try ([`Chain::shifts`]).
    ShiftWall,
    /// W4: the resume typed past the reset meets the wall again, under a new reset.
    RedrawWall,
    /// W5: another agent started in the tab under the first one's remembered limit ([`switch_step`]).
    SwitchWall,
    /// W6: Claude Code's limit held unread past its reset by `FUTILE_AFTER`, told, then read again ([`unread_step`]).
    UnreadWall,
    /// W7: Gemini CLI's per-day dialog answered `Stop`, kept trying while a person is told.
    DayStopWall,
    /// W8: a usage wall met by Cruise's act.
    CruiseWall,
    /// W9: the agent ended and launched again in its tab under its standing wall ([`relaunch_step`]).
    RelaunchWall,
    /// W10: Gemini CLI's per-model bucket's dialog ([`crate::stub::gemini::BUCKET_CLAIM`]).
    BucketWall,
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
    /// G3's climb across a changed reset (`--mode wall-shift`).
    W3,
    /// The redraw after a typed resume (`--mode wall-redraw`).
    W4,
    /// The cross-program end (`--mode wall-switch`).
    W5,
    /// The unread limit told past `FUTILE_AFTER` (`--mode wall-unread`).
    W6,
    /// The per-day dialog answered Stop (`--mode wall-day-stop`).
    W7,
    /// A usage wall under Cruise (`--mode wall-cruise`).
    W8,
    /// The relaunch under a standing wall (`--mode wall-relaunch`).
    W9,
    /// Gemini CLI's per-model bucket (`--mode wall-bucket`).
    W10,
    L16,
    L17,
    N1,
    N2,
    N3,
    N4,
    /// The harness's END (§12.5.4): the window at rest with ends and needs standing, then each chain's chip in front ([`surfaces::judge_end`]).
    End,
    /// A person's keystroke into an instant turn no read saw run: their own exchange, left to them (the review's F18).
    F18,
    /// A judged turn's opener re-wrapped and revealed by a resize, nothing submitted: not judged again (the review's F1 and F2).
    Rewrap,
    /// A new turn answered with the very words of the reply before it, under its own opener: judged as its own point (`b9/resid3`'s submission).
    SameWords,
    /// A judged turn's opener scrolled into sight from the history above the screen by a wheel, nothing submitted: not judged again (F1's other path).
    Scrolled,
    /// A first turn ended on a tool's row after a launch: judged (F3).
    F3,
    /// The back-off ladder doubled to its cap, each act at its due (scaled).
    Ladder,
}

impl Scenario {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Scenario::W2 => "W2",
            Scenario::W3 => "W3",
            Scenario::W4 => "W4",
            Scenario::W5 => "W5",
            Scenario::W6 => "W6",
            Scenario::W7 => "W7",
            Scenario::W8 => "W8",
            Scenario::W9 => "W9",
            Scenario::W10 => "W10",
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
        }
    }

    /// What it holds, for the summary.
    #[must_use]
    pub const fn what(self) -> &'static str {
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
            Scenario::N1 => "Cruise names two needs: needs_n=2, carried on",
            Scenario::N2 => "a defense names none: the needs cleared, needs_n=-",
            Scenario::N3 => "the first need again: needs_n=1",
            Scenario::N4 => "it and another: needs_n=2",
            Scenario::End => "the window at rest, then each chain's chip in front",
            Scenario::W2 => {
                "its notice scrolled away: the remembered wall waited out, resumed, landed"
            }
            Scenario::W3 => {
                "the dialog drawn again with a new reset after each try: each waited out, landed"
            }
            Scenario::W4 => {
                "the typed resume meets the wall again: the new reset waited out, landed"
            }
            Scenario::W5 => {
                "another agent in the tab under the first's limit: acted on at once, landed"
            }
            Scenario::W6 => "a limit held unread an hour past its reset: told, still held",
            Scenario::W7 => {
                "the per-day dialog answered Stop: kept trying, NEEDS YOU up, then down, landed"
            }
            Scenario::W8 => {
                "a usage wall under Cruise: nothing typed into it, resumed, Cruise goes on"
            }
            Scenario::W9 => {
                "the agent launched again under its wall: nothing typed into it, resumed, landed"
            }
            Scenario::W10 => {
                "the per-model bucket's dialog: its Stop at its due, never the switch, landed"
            }
            Scenario::F18 => "a person's key into an instant turn: LEFT, theirs, nothing typed",
            Scenario::Rewrap => "a judged opener re-wrapped by a resize: not judged again",
            Scenario::SameWords => "a new turn answered with the same words: its own point",
            Scenario::Scrolled => {
                "a judged opener scrolled into sight from the history: not judged again"
            }
            Scenario::F3 => "a first turn ended on a tool's row after a launch: judged",
            Scenario::Ladder => "the back-off ladder doubled to its cap, each act at its due",
        }
    }
}

/// How a chain's conversation opens once its agent stands at its composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    /// Its first prompt, typed as `lab turn` types one.
    Prompt,
    /// Its first prompt typed as a person types it: the draft stands in the composer, then Enter ([`person_turn`]).
    Person,
    /// No prompt: the manager tab stands at its composer (L15's starter).
    Idle,
    /// The tab is the worker the manager's spawn opens, its task the prompt.
    Spawned,
}

impl Chain {
    /// The measured pass's chains (`--mode both`), with P4's instant ones and the window's three (L16, L17, N1–N4).
    pub const ALL: [Chain; 16] = [
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
        Chain::InstantLand,
        Chain::InstantCruise,
        Chain::LandJ,
        Chain::LandK,
        Chain::CruiseN,
    ];

    /// The coverage chains (`--mode cover`): what the measured pass left unobserved, and the Off control.
    pub const COVER: [Chain; 9] = [
        Chain::LandQ,
        Chain::LandP,
        Chain::LandW,
        Chain::WroteC,
        Chain::LandT,
        Chain::Mgr,
        Chain::Worker,
        Chain::CruiseO,
        Chain::Off,
    ];

    /// The cap's chains (`--mode cap`, on a lab of their own: the cap is the instance's).
    pub const CAPPED: [Chain; 2] = [Chain::Cap, Chain::Off];

    /// The wall's chain (`--mode wall`, alone on a lab of its own: the stub's usage wall meets every conversation it serves, so not even the Off control shares it).
    pub const WALLED: [Chain; 1] = [Chain::Wall];

    /// The remembered wall's chain (`--mode wall-hid`, alone on a lab of its own for W1's reason).
    pub const WALL_HID: [Chain; 1] = [Chain::WallHid];

    /// The residuals' chains (`--mode resid`): F18's beside its negative control, P4C's chain whose person's prompt goes through the control socket (judged as a turn of the conversation); F1, F2 and SAME's; F1's by a scroll; F3's; and the Off control.
    pub const RESID: [Chain; 6] = [
        Chain::InstantCruise,
        Chain::InstantPerson,
        Chain::SameWords,
        Chain::TallScroll,
        Chain::ToolEnd,
        Chain::Off,
    ];

    /// The ladder's chain (`--mode ladder`, on a lab of its own: the scale is the instance's, [`ladder_env`]) and the Off control.
    pub const LADDER: [Chain; 2] = [Chain::Ladder, Chain::Off];
    /// The per-day quota's chain (`--mode wall-day`, alone on a lab of its own for W1's reason).
    pub const WALL_DAY: [Chain; 1] = [Chain::WallDay];

    /// The climbing quota's chain (`--mode wall-climb`, alone on a lab of its own for W1's reason).
    pub const WALL_CLIMB: [Chain; 1] = [Chain::WallClimb];

    /// G3's chain (`--mode wall-shift`, alone on a lab of its own for W1's reason).
    pub const WALL_SHIFT: [Chain; 1] = [Chain::ShiftWall];

    /// The redraw's chain (`--mode wall-redraw`, alone for W1's reason).
    pub const WALL_REDRAW: [Chain; 1] = [Chain::RedrawWall];

    /// The cross-program end's chain (`--mode wall-switch`, alone for W1's reason).
    pub const WALL_SWITCH: [Chain; 1] = [Chain::SwitchWall];

    /// The unread limit's chain (`--mode wall-unread`, alone for W1's reason).
    pub const WALL_UNREAD: [Chain; 1] = [Chain::UnreadWall];

    /// The per-day dialog's Stop's chain (`--mode wall-day-stop`, alone for W1's reason).
    pub const WALL_DAY_STOP: [Chain; 1] = [Chain::DayStopWall];

    /// The Cruise wall's chain (`--mode wall-cruise`, alone for W1's reason).
    pub const WALL_CRUISE: [Chain; 1] = [Chain::CruiseWall];

    /// The relaunch's chain (`--mode wall-relaunch`, alone for W1's reason).
    pub const WALL_RELAUNCH: [Chain; 1] = [Chain::RelaunchWall];

    /// The bucket's chain (`--mode wall-bucket`, alone for W1's reason).
    pub const WALL_BUCKET: [Chain; 1] = [Chain::BucketWall];

    /// Every chain of every mode, each once (`--chains` names one of them; the tests' census of them).
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
            .chain(Chain::WALL_DAY)
            .chain(Chain::WALL_CLIMB)
            .chain(Chain::WALL_SHIFT)
            .chain(Chain::WALL_REDRAW)
            .chain(Chain::WALL_SWITCH)
            .chain(Chain::WALL_UNREAD)
            .chain(Chain::WALL_DAY_STOP)
            .chain(Chain::WALL_CRUISE)
            .chain(Chain::WALL_RELAUNCH)
            .chain(Chain::WALL_BUCKET)
        {
            if !v.contains(&c) {
                v.push(c);
            }
        }
        v
    }

    /// The chains a mode runs: the Off control in every mode but the wall's.
    #[must_use]
    pub fn of_mode(mode: Mode) -> Vec<Chain> {
        match mode {
            Mode::Cover => Chain::COVER.to_vec(),
            Mode::Cap => Chain::CAPPED.to_vec(),
            Mode::Wall => Chain::WALLED.to_vec(),
            Mode::WallHid => Chain::WALL_HID.to_vec(),
            Mode::Resid => Chain::RESID.to_vec(),
            Mode::Ladder => Chain::LADDER.to_vec(),
            Mode::WallDay => Chain::WALL_DAY.to_vec(),
            Mode::WallClimb => Chain::WALL_CLIMB.to_vec(),
            Mode::WallShift => Chain::WALL_SHIFT.to_vec(),
            Mode::WallRedraw => Chain::WALL_REDRAW.to_vec(),
            Mode::WallSwitch => Chain::WALL_SWITCH.to_vec(),
            Mode::WallUnread => Chain::WALL_UNREAD.to_vec(),
            Mode::WallDayStop => Chain::WALL_DAY_STOP.to_vec(),
            Mode::WallCruise => Chain::WALL_CRUISE.to_vec(),
            Mode::WallRelaunch => Chain::WALL_RELAUNCH.to_vec(),
            Mode::WallBucket => Chain::WALL_BUCKET.to_vec(),
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

    /// The chains of `mode` harness `h` runs: W2's only where a local command takes the notice away ([`wall_out_of_reach`]).
    #[must_use]
    pub fn of_mode_on(mode: Mode, h: Harness) -> Vec<Chain> {
        Chain::of_mode(mode)
            .into_iter()
            .filter(|c| match c {
                Chain::WallHid => wall_out_of_reach(h, Mode::WallHid).is_none(),
                Chain::ToolEnd => tool_end_out_of_reach(h).is_none(),
                Chain::WallDay => wall_out_of_reach(h, Mode::WallDay).is_none(),
                Chain::WallClimb => wall_out_of_reach(h, Mode::WallClimb).is_none(),
                Chain::ShiftWall => wall_out_of_reach(h, Mode::WallShift).is_none(),
                Chain::RedrawWall => wall_out_of_reach(h, Mode::WallRedraw).is_none(),
                Chain::UnreadWall => wall_out_of_reach(h, Mode::WallUnread).is_none(),
                Chain::DayStopWall => wall_out_of_reach(h, Mode::WallDayStop).is_none(),
                Chain::RelaunchWall => wall_out_of_reach(h, Mode::WallRelaunch).is_none(),
                Chain::BucketWall => wall_out_of_reach(h, Mode::WallBucket).is_none(),
                _ => true,
            })
            .collect()
    }

    /// It meets the stub's usage wall ([`wall_step`]): W1's chains, W2's, and W3's to W10's.
    #[must_use]
    pub const fn walled(self) -> bool {
        matches!(
            self,
            Chain::Wall
                | Chain::WallHid
                | Chain::WallDay
                | Chain::WallClimb
                | Chain::ShiftWall
                | Chain::RedrawWall
                | Chain::SwitchWall
                | Chain::UnreadWall
                | Chain::DayStopWall
                | Chain::CruiseWall
                | Chain::RelaunchWall
                | Chain::BucketWall
        )
    }

    /// How many times the lab raises its wall again at the reset instead of lifting it ([`wall_move`]): G3's dialog drawn again with a new reset after each of [`SHIFT_TIMES`] tries, W4's once, under the resume typed past the first reset. None for the other chains.
    #[must_use]
    pub fn shifts(self) -> usize {
        match self {
            Chain::ShiftWall => SHIFT_TIMES,
            Chain::RedrawWall => 1,
            _ => 0,
        }
    }

    /// How far each raise again moves the reset on ([`wall_move`]): a whole number of minutes, so each reset stays on the minute the dialog and the notices name.
    #[must_use]
    pub fn shift_step_ms(self) -> i64 {
        match self {
            Chain::RedrawWall => REDRAW_AHEAD_MS,
            _ => SHIFT_STEP_MS,
        }
    }

    /// W2's hide runs on its tab on harness `h` ([`hide_step`]): W2's own, W6's (its notice gone before the conversation is made unread), W7's (the per-day dialog answered Stop), and W4's where the resume is typed only past it — Claude Code's countdown cancelled, Gemini CLI's dialog answered Stop.
    #[must_use]
    pub fn hides_on(self, h: Harness) -> bool {
        match self {
            Chain::WallHid | Chain::UnreadWall | Chain::DayStopWall => true,
            Chain::RedrawWall => matches!(h, Harness::Claude | Harness::Gemini),
            _ => false,
        }
    }

    /// How far ahead of its raising its wall resets, at least ([`wall_step`]).
    #[must_use]
    pub const fn wall_ahead_ms(self) -> i64 {
        match self {
            Chain::WallHid => WALL_HID_AHEAD_MS,
            Chain::WallDay | Chain::DayStopWall => WALL_DAY_AHEAD_MS,
            Chain::WallClimb => WALL_CLIMB_AHEAD_MS,
            Chain::RelaunchWall => WALL_RELAUNCH_AHEAD_MS,
            Chain::RedrawWall | Chain::UnreadWall => WALL_HID_AHEAD_MS,
            Chain::SwitchWall => WALL_SWITCH_AHEAD_MS,
            _ => WALL_AHEAD_MS,
        }
    }

    /// The claim its wall is raised with on harness `h` ([`wall_step`]): [`wall_claim`]'s, but on the per-day quota's chain the one on which the stub answers the Gemini API's key a spent per-day quota ([`WALL_CLAIM`]: it names no Code Assist limit), whose dialog names no reset; on the bucket's the account's limit on the Pro model ([`crate::stub::gemini::BUCKET_CLAIM`]).
    #[must_use]
    pub const fn wall_claim_on(self, h: Harness) -> &'static str {
        match self {
            Chain::WallDay | Chain::WallClimb | Chain::DayStopWall => WALL_CLAIM,
            Chain::BucketWall => crate::stub::gemini::BUCKET_CLAIM,
            _ => wall_claim(h),
        }
    }

    /// How its conversation opens.
    #[must_use]
    pub const fn opening(self) -> Opening {
        match self {
            Chain::LandP => Opening::Person,
            Chain::Mgr => Opening::Idle,
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
            Chain::Ladder => LADDER_LIMIT_MS,
            Chain::WallDay | Chain::DayStopWall => WALL_DAY_LIMIT_MS,
            Chain::CruiseWall => WALL_CRUISE_LIMIT_MS,
            Chain::RelaunchWall => WALL_RELAUNCH_LIMIT_MS,
            Chain::WallClimb => WALL_CLIMB_LIMIT_MS,
            Chain::ShiftWall => WALL_SHIFT_LIMIT_MS,
            Chain::RedrawWall => WALL_REDRAW_LIMIT_MS,
            Chain::SwitchWall => WALL_SWITCH_LIMIT_MS,
            Chain::UnreadWall => WALL_UNREAD_LIMIT_MS,
            _ => CHAIN_LIMIT_MS,
        }
    }

    /// Its worker is one a manager waits on (a task record), so the ladder after its landing is a managed worker's ([`next_step_ms`]).
    #[must_use]
    pub fn managed(self) -> bool {
        self.tasked() || self == Chain::Worker
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
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
            Chain::WallDay => "day-wall",
            Chain::WallClimb => "climb-wall",
            Chain::ShiftWall => "shift-wall",
            Chain::RedrawWall => "redraw-wall",
            Chain::SwitchWall => "switch-wall",
            Chain::UnreadWall => "unread-wall",
            Chain::DayStopWall => "day-stop-wall",
            Chain::CruiseWall => "cruise-wall",
            Chain::RelaunchWall => "relaunch-wall",
            Chain::BucketWall => "bucket-wall",
        }
    }

    /// The word its conversation carries (its first prompt), by which the stub's rules know it: no other chain's word holds it.
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
            | Chain::CruiseWall => "cruise",
            Chain::Off | Chain::Mgr => "off",
            _ => "land",
        }
    }

    #[must_use]
    pub const fn scenarios(self) -> &'static [Scenario] {
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
            Chain::WallDay | Chain::WallClimb => &[Scenario::W1],
            Chain::ShiftWall => &[Scenario::W3],
            Chain::RedrawWall => &[Scenario::W4],
            Chain::SwitchWall => &[Scenario::W5],
            Chain::UnreadWall => &[Scenario::W6],
            Chain::DayStopWall => &[Scenario::W7],
            Chain::CruiseWall => &[Scenario::W8],
            Chain::RelaunchWall => &[Scenario::W9],
            Chain::BucketWall => &[Scenario::W10],
        }
    }

    /// What its repository holds beside its pushed `main`.
    #[must_use]
    pub const fn repo(self) -> Repo {
        match self {
            Chain::LandC => Repo::Untracked,
            Chain::LandF => Repo::Changed,
            Chain::LandG => Repo::Stashed,
            Chain::LandH => Repo::Unpushed,
            Chain::LandI => Repo::Unlanded,
            _ => Repo::Clean,
        }
    }

    /// Its tab carries a worker's task record (`task mark working from=ctl` before the agent starts): a manager waits on it.
    #[must_use]
    pub const fn tasked(self) -> bool {
        matches!(self, Chain::LandE | Chain::LandT)
    }

    /// Its landing is the scenario: watched past the ladder's next step after it, and the stub's dumps read for a new turn after it.
    #[must_use]
    pub const fn lands(self) -> bool {
        matches!(
            self,
            Chain::LandA
                | Chain::LandE
                | Chain::InstantLand
                | Chain::LandQ
                | Chain::LandW
                | Chain::Worker
                | Chain::Wall
                | Chain::WallHid
                | Chain::WallDay
                | Chain::WallClimb
                | Chain::ShiftWall
                | Chain::RedrawWall
                | Chain::SwitchWall
                | Chain::UnreadWall
                | Chain::DayStopWall
                | Chain::RelaunchWall
                | Chain::BucketWall
        )
    }

    /// Its replies are sent at once: no delay before the first byte, no pause between deltas (P4: a turn over before any read sees the worker busy).
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

    /// A PERSON'S PROMPT MID-CHAIN (P4P): typed as the first prompt is, once the done check's defense was decided, while the loop waits out its back-off.
    #[must_use]
    pub fn asks(self) -> Option<String> {
        matches!(self, Chain::InstantCruise | Chain::InstantPerson)
            .then(|| format!("{} {ASK_KEY}", self.token()))
    }

    /// Its person's prompt mid-chain ([`Self::asks`]) is typed AS A PERSON TYPES IT ([`person_turn`]: `send|key as=person`, a person's hand on the session), not through the control socket (F18).
    #[must_use]
    pub fn asks_as_person(self) -> bool {
        self == Chain::InstantPerson
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

    /// The stub's rules for its conversation, most specific first ([`crate::stub::scripted_reply`]).
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
        let after = |last: &str, before: &str, text: &str| {
            let mut r = rule(last, text);
            r.set("after", J::s(before));
            r
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
            Chain::LandQ => vec![
                rule(NUDGE_KEY, CLEAN),
                rule(ANSWER_KEY, CLEAN),
                rule(LAND_KEY, QUESTION),
                rule("", WORK),
            ],
            Chain::LandP => vec![
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule(ORCHESTRATED_KEY, WORK),
                rule("", PERSON_REPLY),
            ],
            Chain::LandW => vec![
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
            Chain::LandT => vec![rule(NUDGE_KEY, CLEAN), rule("", WORK)],
            Chain::Cap => {
                let open = open_defense();
                vec![
                    rule(NUDGE_KEY, CLEAN),
                    rule(LAND_KEY, &open),
                    rule(RESUMED_KEY, &open),
                    rule("", WORK),
                ]
            }
            Chain::Mgr => Vec::new(),
            Chain::InstantLand | Chain::Worker => vec![
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", WORK),
            ],
            Chain::InstantCruise | Chain::InstantPerson => vec![
                rule(ASK_KEY, ASK_REPLY),
                rule(CHECK_KEY, CLEAN),
                rule(CONTINUE_KEY, DONE_CLAIM),
                rule("", DONE_CLAIM),
            ],
            Chain::LandJ | Chain::LandK => vec![
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
            Chain::CruiseWall => vec![
                rule(CHECK_KEY, CLEAN),
                rule(CONTINUE_KEY, DONE_CLAIM),
                rule(MARK, DONE_CLAIM),
                rule("", DONE_CLAIM),
            ],
            Chain::Wall
            | Chain::WallHid
            | Chain::WallDay
            | Chain::WallClimb
            | Chain::ShiftWall
            | Chain::RedrawWall
            | Chain::SwitchWall
            | Chain::UnreadWall
            | Chain::DayStopWall
            | Chain::RelaunchWall
            | Chain::BucketWall => vec![
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule(MARK, CLEAN),
                rule("", WORK),
            ],
            Chain::SameWords => vec![rule(SAME_TURN_KEY, WORK), rule("", WORK)],
            Chain::TallScroll => vec![rule("", &scroll_reply())],
            Chain::ToolEnd => vec![
                rule(NUDGE_KEY, CLEAN),
                rule(LAND_KEY, CLEAN),
                rule("", TOOL_AFTER),
            ],
            Chain::Ladder => vec![rule("", WORK)],
        }
    }

    /// The orchestrator's turn the lab types into its tab mid-watch, and when ([`orchestrator_due`]).
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

/// L6's reply, `lines` tall: ONE need near its top, the defense at its bottom.
#[must_use]
pub fn tall_reply(lines: usize) -> String {
    let mut s = format!("Here is the landing report.\nNEEDS YOU: {NEED}\n");
    for i in 1..=lines {
        s.push_str(&format!("- check {i} passed\n"));
    }
    s.push_str(CLEAN);
    s
}

/// `tall-scroll`'s reply: a head, [`SCROLL_LINES`] rows of a report, then the work report's words ([`WORK`]) — taller than the 50 rows of the window's tab, so once the turn is over its opener stands in the history above the screen (the agent's own, where it draws its transcript in a view of its own).
#[must_use]
pub fn scroll_reply() -> String {
    let mut s = String::from("Here is where the parser stands.\n");
    for i in 1..=SCROLL_LINES {
        s.push_str(&format!("- part {i} of the parser is written\n"));
    }
    s.push_str(WORK);
    s
}

/// The custom mode's spec: every chain's rules, each reply after [`REPLY_DELAY_MS`], and the plain work reply for a turn no rule picks.
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

/// [`spec`] for a run on harness `h`, `dirs` its chains' repositories (in `chains`' order): F3's first turn answered with that harness's read of its own `README.md` ([`read_call`]) on the rule that answers its prompt.
#[must_use]
pub fn spec_on(chains: &[Chain], h: Harness, dirs: &[PathBuf]) -> J {
    let mut sp = spec(chains);
    let Some(dir) = chains
        .iter()
        .zip(dirs)
        .find(|(c, _)| **c == Chain::ToolEnd)
        .map(|(_, d)| d)
    else {
        return sp;
    };
    let call = read_call(h, &dir.join("README.md"));
    let token = Chain::ToolEnd.token();
    if let Some(J::Arr(rules)) = sp.get("replies").cloned() {
        let rules = rules
            .into_iter()
            .map(|mut r| {
                let ours = r.get("when").and_then(J::as_str) == Some(token.as_str())
                    && r.get("last").and_then(J::as_str) == Some("");
                if ours {
                    r.set("tool", call.clone());
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
    pub rule: String,
    pub decision: String,
    pub command: String,
    pub reason: String,
}

impl Row {
    /// The row records something done to the agent: a key pressed or a text typed (`approved`, `declined`, `typed`), or one tried (`escalated`, `deferred`) — every decision but a record (`waiting`) or a skip.
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
    /// Of those, the ones holding more harness-marked user texts than [`Self::baseline`]: a new turn of the harness's.
    pub new_turns: usize,
}

/// What one tab's records said, each with its time (ms), in order: the journal's lines, the ledger's rows, and the `status`, `meta` and `task` words at each change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub journal: Vec<(i64, String)>,
    pub ledger: Vec<Row>,
    /// `task=… needs=… autopilot=… supervisor=…` of `status`.
    pub status: Vec<(i64, String)>,
    /// Actual status reads, even where the recorded words did not change.
    pub status_reads: Vec<i64>,
    /// `attention_owners=… attention_kinds=… attention=…` of `meta`, the attention decoded.
    pub meta: Vec<(i64, String)>,
    /// The `task` record's line.
    pub task: Vec<(i64, String)>,
    /// When the reply the chain watches from showed (Off: its done reply).
    pub reply_ms: Option<i64>,
    /// When a person's prompt mid-chain was typed (P4P, [`Chain::asks`]).
    pub asked_ms: Option<i64>,
    /// When one was typed that the composer did not take: it never stood there as typed, or stood there still after Enter twice.
    pub ask_refused_ms: Option<i64>,
    /// When watching stopped.
    pub end_ms: i64,
    /// The stub's witness (L5, L7: after the landing; Off: the whole conversation); `None` where it was never read.
    pub stub: Option<StubSeen>,
    /// What the coverage chains' run did and read beside the records.
    pub cover: Cover,
    /// What the window showed of the tab ([`surfaces`]).
    pub window: surfaces::Window,
}

/// What a coverage chain's run did to its tab, and read of it, beside the records ([`Seen`]), each with its time (ms).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cover {
    /// L13: when the person's Enter went in under their standing draft.
    pub person_ms: Option<i64>,
    /// L13: `status human_ms=` read once their Enter took the draft: how long ago a person's gesture reached the session (`-`: none ever).
    pub person_stamp: Option<String>,
    /// L13 typed through `hwkey` in a windowed lab ([`person_keys`]): what the real key path posted and counted.
    pub keyed: Option<Keyed>,
    /// L13, CAP: when the lab's orchestrator turn went in.
    pub turn_ms: Option<i64>,
    /// The stub's witness of a window no harness turn may fall in: L13's exchange (its first prompt to the orchestrator's turn), CAP's hour (after the act to the turn after it).
    pub window_stub: Option<StubSeen>,
    /// L15: the manager's sid.
    pub starter: Option<String>,
    /// L15: the worker's spawn reply.
    pub spawn: Option<String>,
    /// L15: when the manager's `await task @<w> since=0` returned, its exit status and what it printed.
    pub awaited: Option<(i64, i32, String)>,
    /// L15: the worker's `result`, as printed (its standard output).
    pub result: Option<String>,
    /// L15: the agent the lab's spawn named (`agent=<word>`): the harness under test's.
    pub agent: Option<String>,
    /// L15: the `result` reply's header (`OK <n> source=… bytes=<n>`, on its standard error).
    pub result_head: Option<String>,
    /// L15: the agent the worker's spawn started, as `meta worker=` names it once the watch is over.
    pub worker: Option<String>,
    /// W1: when the lab raised the stub's usage wall, and the unix second it resets at ([`wall_step`]).
    pub wall: Option<(i64, i64)>,
    /// W1: when the lab gave the stub its scripted mode back.
    pub lifted_ms: Option<i64>,
    /// W1: when the PROGRAM WENT ON BY ITSELF past the wall: the first of the conversation's requests from the wall's lift on carrying its own continuation ([`carries_continuation`]: Claude Code's, which goes on at the reset with no act of the harness's), read from the stub's dumps once the watch is over ([`went_on_witness`]).
    pub went_on_ms: Option<i64>,
    /// C4: the user texts of the conversation's requests that carry the owner's continuation ([`OWNER_KEY`]), each as the agent sent it, read from the stub's dumps once the watch is over ([`owner_witness`]).
    pub owner_texts: Option<Vec<String>>,
    /// W2: the notice scrolled out of sight ([`hide_step`]).
    pub hidden: Option<Hidden>,
    /// W2: why the notice was not taken off the screen, where it was not.
    pub hide_gave_up: Option<String>,
    /// W3, W4: each time the lab raised the wall again at its reset instead of lifting it ([`wall_move`]), and the unix second the new wall resets at, in order.
    pub shifted: Vec<(i64, i64)>,
    /// W5: the switch to the second agent ([`switch_step`]).
    pub switch: Option<Switch>,
    /// W5: why the switch was not made, where it was not.
    pub switch_gave_up: Option<String>,
    /// W6: the conversation made unread, and read again ([`unread_step`]).
    pub unread: Option<Unread>,
    /// W6: why the conversation was not made unread or read again.
    pub unread_gave_up: Option<String>,
    /// W9: the agent ended and launched again in its tab ([`relaunch_step`]): a [`Switch`] to the same agent, its wall never lifted.
    pub relaunch: Option<Switch>,
    /// W9: why the relaunch was not made, where it was not.
    pub relaunch_gave_up: Option<String>,
    /// F18: `status human_ms=` read once the person's mid-chain Enter took their draft ([`Chain::asks_as_person`]): a stamp where a person's hand reached the session.
    pub ask_stamp: Option<String>,
    /// F1, F2: when the lab resized the tab ([`RESIZED`]), and what the resize and `dims` answered.
    pub resized: Option<(i64, String)>,
    /// F1 BY A SCROLL: the wheel the lab turned over the tab, and what the screen showed ([`scroll_step`]).
    pub scrolled: Option<Scrolled>,
    /// F3: how many of the conversation's requests carried a tool's result back (the stub's dumps), read once its watch is over.
    pub tool_results: Option<usize>,
    /// F3: the screen's last rows read once the first point after the prompt was journaled, and when.
    pub first_screen: Option<(i64, Vec<String>)>,
}

impl Cover {
    /// The unix second the stub's wall resets at now: the last raise again's ([`Cover::shifted`]), else the first wall's.
    #[must_use]
    pub fn reset_now(&self) -> Option<i64> {
        let (_, first) = self.wall?;
        Some(self.shifted.last().map_or(first, |(_, r)| *r))
    }

    /// The unix second the wall answering a request at `t` (ms) named: the last raise at or before it, `None` before the wall was raised and from its lift on ([`Cover::lifted_ms`]).
    #[must_use]
    pub fn reset_at(&self, t: i64) -> Option<i64> {
        let (raised, first) = self.wall?;
        if t < raised || self.lifted_ms.is_some_and(|l| t >= l) {
            return None;
        }
        Some(
            self.shifted
                .iter()
                .rev()
                .find(|(at, _)| *at <= t)
                .map_or(first, |(_, r)| *r),
        )
    }
}

/// W5's switch ([`switch_step`]): the first agent ended in the tab, the stub's wall lifted (the limit is that agent's account's), the second agent launched there and its first prompt typed.
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

/// W6's unread conversation ([`unread_step`]): Claude Code's transcript of the chain's conversation moved aside, so no read names the conversation the limit stopped, and moved back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unread {
    /// The transcript.
    pub file: PathBuf,
    /// When it was moved aside ([`aside_of`]).
    pub moved_ms: i64,
    /// When it was moved back.
    pub restored_ms: Option<i64>,
}

/// F1 BY A SCROLL, AS DRIVEN ([`scroll_step`]): the wheel turned up over `tall-scroll`'s tab, a row a notch, until its opener was in sight.
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
    /// The tab's `modes` before the first notch: `alt_screen=` and `mouse_mode=`, whether the agent takes the wheel (it scrolls its own view) or the tab's viewport moves.
    pub modes: String,
    /// The tab's history above its screen before the first notch, as `lines` answered (`OK <rows> …`): the rows a viewport can scroll back.
    pub history: String,
    /// Whether the opener (the chain's token) was on the screen before the first notch.
    pub before: bool,
    /// Whether it was in sight after the last notch: on the live screen where the agent took the wheel, in the tab's viewport where it did not.
    pub after: bool,
    /// Where the agent takes no wheel (`mouse_mode=none`): the tab's viewport after the last notch, as `scroll` answered it (`OK <offset> <max>`), with where the opener and the live screen were (`search`).
    pub viewport: Option<String>,
    /// Whether the live screen (the one the harness reads) changed under the wheel at all.
    pub moved: bool,
    /// The screen's rows after the last notch, blank rows left out.
    pub rows: Vec<String>,
    /// Whether the opener was still on the screen [`SCROLL_HELD_MS`] after the last notch; `None` before that read.
    pub held: Option<bool>,
}

/// L13's prompt typed through `hwkey` (`forever --window`, [`person_keys`]): each key a real `NSEvent` posted into the windowed instance's own event queue, and the session's count of a person's gestures around them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyed {
    /// The presses `hwkey` handed to the window's event queue (`OK posted=<n>`): the text's characters and each Enter.
    pub posted: u64,
    /// How many of them it posted while the window was the key window of the active app (`key=1`): none in the jail, whose instance is no Launch Services application, so nothing can activate it.
    pub key_window: u64,
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
    /// When the Enter of the command that scrolled the notice away went in (the screen before it still drew the notice).
    pub enter_ms: i64,
    /// When the lab read the screen without it.
    pub seen_ms: i64,
    /// That screen's content seq (`status seq=`, the journal's `seq=`): a point read at it or after is a screen without the notice.
    pub seq: Option<u64>,
    /// The hiding commands entered, or the dialog's option chosen (once) ([`Hide::by`]).
    pub commands: usize,
    /// That command or option ([`Hide::label`]): Muse Code's [`HIDE_COMMAND`], agy's [`AGY_HIDE_COMMAND`], Gemini CLI's [`GEMINI_HIDE_OPTION`].
    pub command: &'static str,
    /// The notice was taken by choosing the dialog's option ([`HideBy::Option`]): the screen after it draws the turn's error that option leaves (Gemini CLI's `✕ [API Error: An unknown error occurred.]`), a wall its reader reads that no longer names the limit.
    pub stopped: bool,
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
        let waiting = event.and_then(|(et, el)| {
            let seq = word(el, "seq");
            self.journal.iter().find(|(at, l)| {
                *at >= *et && before(*at) && l.starts_with("WAITING ") && word(l, "seq") == seq
            })
        });
        Yield { event, waiting }
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

/// P3 OF `docs/measured/forever-lab-2026-10-20.md`: the point records (`point@v1`) written before the first prompt at `t0` — a point judged before anyone asked the agent anything (a frame drawn before its banner, a blank first screen), which counts a short turn the agent never ran and starts every back-off a step late.
#[must_use]
pub fn points_before_prompt(seen: &Seen, t0: i64) -> Vec<&Row> {
    seen.ledger
        .iter()
        .filter(|r| r.rule == RULE_POINT && r.ts < t0)
        .collect()
}

/// The tab's first point and first back-off, as `lab forever` reports them (P3): `first point +5.6 s (short turns in a row: 1), first back-off +5.6 s "1 short turn(s) in a row: 2 min back-off", 0 point record(s) before the first prompt` — every point record before the first prompt named with its time (before `t0`: `-…`).
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

/// The ladder's next step after a point that closed `streak` short turns: the wait before the next act, were the ladder still going (none for a worker its manager waits on, through its first [`turn_end::MANAGED_SHORT_TURNS`]).
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

/// How long the landing at `t` is watched for after it (L5, L7): through the ladder's next step and [`PAST_NEXT_STEP_MS`] more, at least [`QUIET_AFTER_LANDED_MIN_MS`].
#[must_use]
pub fn quiet_after_landed_ms(seen: &Seen, t: i64, managed: bool) -> i64 {
    let step = next_step_ms(streak_at(seen, t).unwrap_or(1), managed);
    step.saturating_add(PAST_NEXT_STEP_MS)
        .max(QUIET_AFTER_LANDED_MIN_MS)
}

/// THE JUDGE of one chain's records `seen` (its `program`: the reader's word the needs entry names), `t0` the first prompt's time: each of its scenarios' verdict, in order — every one a FAIL where the chain's records hold a fault of their own ([`chain_faults`]: an act before its due, an act typed twice, a point before the first prompt), its own judge's words kept.
#[must_use]
pub fn judge(chain: Chain, seen: &Seen, program: &str, t0: i64) -> Vec<(Scenario, Verdict)> {
    let faults = chain_faults(seen, t0);
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
    let mut out = Vec::new();
    for &s in chain.scenarios() {
        let v = match s {
            Scenario::L1 => judge_land_act(seen, land, done, t0),
            Scenario::L2 => judge_nudge(seen, land, &land_nudge_after, done, &Want::open(), t0),
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
            Scenario::L3 => judge_needs(seen, land, &land_nudge_after, program, t0),
            Scenario::L5 => judge_landed(seen, land, &land_nudge_after, false, t0),
            Scenario::L6 => judge_tall(seen, land, done, t0),
            Scenario::L7 => judge_tasked(seen, land, t0),
            Scenario::C1 => judge_check(seen, t0),
            Scenario::C2 => judge_cruise_goes_on(seen, done, t0),
            Scenario::Off => judge_off(seen, t0),
            Scenario::P4L => judge_instant_land(seen, land, done, t0),
            Scenario::P4C => judge_instant_cruise(seen, done, t0),
            Scenario::P4P => judge_instant_ask(seen, t0),
            Scenario::L12 => cover::judge_answer(seen, t0),
            Scenario::L13 => cover::judge_theirs(seen, t0),
            Scenario::L14 => cover::judge_wrote_land(seen, land, &land_nudge_after, program, t0),
            Scenario::C3 => cover::judge_wrote_cruise(seen, done, t0),
            Scenario::C4 => cover::judge_owner_continuation(seen, done, t0),
            Scenario::P5 => cover::judge_startup(seen, t0),
            Scenario::L15 => cover::judge_spawned(seen, land, program, t0),
            Scenario::Cap => cover::judge_cap(seen, t0),
            Scenario::W1 if chain == Chain::WallClimb => {
                cover::judge_wall_climb(seen, land, program, t0)
            }
            Scenario::W1 => cover::judge_wall(seen, land, program, t0),
            Scenario::L16 | Scenario::L17 => judge_long_landing(seen, land, t0),
            Scenario::N1 => surfaces::judge_needs_step(seen, t0, 0),
            Scenario::N2 => surfaces::judge_needs_step(seen, t0, 1),
            Scenario::N3 => surfaces::judge_needs_step(seen, t0, 2),
            Scenario::N4 => surfaces::judge_needs_step(seen, t0, 3),
            Scenario::End => continue,
            Scenario::W2 => cover::judge_wall_hidden(seen, land, program, t0),
            Scenario::W3 => walls::judge_wall_shift(seen, land, program, t0),
            Scenario::W4 => walls::judge_wall_redraw(seen, land, program, t0),
            Scenario::W5 => walls::judge_wall_switch(seen, land, program, t0),
            Scenario::W6 => walls::judge_wall_unread(seen, land, program, t0),
            Scenario::W7 => walls::judge_wall_day_stopped(seen, land, program, t0),
            Scenario::W8 => walls::judge_wall_cruise(seen, program, t0),
            Scenario::W9 => walls::judge_wall_relaunch(seen, land, program, t0),
            Scenario::W10 => walls::judge_wall_bucket(seen, land, program, t0),
            Scenario::F18 => cover::judge_person_instant(seen, t0),
            Scenario::Rewrap => cover::judge_rewrap(seen, t0),
            Scenario::Scrolled => cover::judge_scrolled(seen, t0),
            Scenario::SameWords => cover::judge_same_words(seen, t0),
            Scenario::F3 => cover::judge_tool_end(seen, land, t0),
            Scenario::Ladder => cover::judge_ladder(seen, LADDER_SCALE, t0),
        };
        let cx = surfaces::Ctx { chain, program, t0 };
        out.push((s, surfaces::with_window(s, seen, &cx, v)));
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
    head: &'static str,
    named: &'static [&'static str],
    not_named: &'static [&'static str],
    not_anywhere: [&'static str; 2],
}

impl Want {
    const fn open() -> Want {
        Want {
            head: OPEN_SUFFIX,
            named: &[],
            not_named: &[],
            not_anywhere: [REPO_SUFFIX, NEEDS_ACK],
        }
    }

    const fn repo(repo: Repo) -> Want {
        let (named, not_named) = repo.named();
        Want {
            head: REPO_SUFFIX,
            named,
            not_named,
            not_anywhere: [OPEN_SUFFIX, NEEDS_ACK],
        }
    }
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
        if let Some(lost) = lost_after(seen, *t1, t0) {
            return Verdict::Fail(format!(
                "no nudge was typed for the clean defense to answer: {lost}"
            ));
        }
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
        None => match lost_after(seen, *t1, t0) {
            Some(lost) => Verdict::Fail(format!(
                "not landed, need raised, but the carry-on never typed: {lost}: {said}"
            )),
            None => Verdict::Unobserved(format!(
                "not landed, need raised, but the carry-on not observed within {}: {said}",
                at(t0, seen.end_ms)
            )),
        },
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

/// How long the loop waits after a continuation at a usage wall that met the wall again, at the least (`TurnEndTiming::limit_backoff`'s first rung): its next is due at that or the reset and its grace, whichever is later.
#[must_use]
pub fn limit_retry_ms() -> i64 {
    turn_end::TurnEndTiming::default()
        .limit_backoff
        .first()
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub(crate) fn take_window_ms() -> i64 {
    i64::try_from(turn_end::TurnEndTiming::default().take_within.as_millis()).unwrap_or(i64::MAX)
}

fn doubled_acts(seen: &Seen, t0: i64) -> Vec<String> {
    let acts = seen.all_acts();
    acts.windows(2)
        .filter(|w| {
            let (a, b) = (w[0].0, w[1].0);
            !seen.journal.iter().any(|(t, l)| {
                *t > a
                    && *t < b
                    && ((l.starts_with("WAITING ") && !l.contains(" the worker to take the "))
                        || l.starts_with("DONE ")
                        || l.starts_with("LEFT "))
            })
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

/// The short back-off ladder's steps from `t0`, as the journal decided them: the first turn-end `WAITING … N short turn(s) in a row: …` of each point, in order — a wall's back-off and the take window's left out, and a step told again left out whether under its point's `seq=` or a re-read's own ([`same_backoff`]: the same streak and due).
#[must_use]
pub fn ladder_steps(seen: &Seen, t0: i64) -> Vec<Step> {
    let mut seqs: Vec<Option<String>> = Vec::new();
    let mut out: Vec<Step> = Vec::new();
    for (t, l) in &seen.journal {
        let Some(streak) = backoff_streak(l).filter(|_| *t >= t0 && backoff_named(l)) else {
            continue;
        };
        let seq = word(l, "seq");
        if seqs.contains(&seq) || out.iter().any(|st| same_backoff(&st.line, l)) {
            continue;
        }
        seqs.push(seq);
        if let Some(due) = backoff_due(l) {
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
        .rfind(|(at, l)| *at > t && backoff_named(l))
    {
        Some((tw, wl)) => {
            let due = backoff_due(wl)?;
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

fn p3_faults(seen: &Seen, t0: i64) -> Vec<String> {
    let from = seen.cover.person_ms.unwrap_or(t0);
    let streak = |r: &Row| -> Option<u32> {
        let (_, n) = r.reason.split_once("short turns in a row: ")?;
        n.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
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
        let want = u32::from(!left);
        if streak(first) != Some(want) {
            out.push(format!(
                "the first point at +{} closed {} short turn(s), not {want} (P3: the ladder starts a step late): {}",
                at(t0, first.ts),
                streak(first).map_or_else(|| "?".to_string(), |n| n.to_string()),
                crate::util::one_line(&first.reason, 90)
            ));
        }
    }
    out
}

/// What fails every scenario of a chain, whatever each scenario's own judge says (the review of `b9/forever-gaps`, F10–F12): an act typed before the due its back-off named ([`early_acts`]), an act typed twice with no point judged between ([`doubled_acts`]), a point before the first prompt or a first point that closed other than one short turn ([`p3_faults`]).
#[must_use]
pub fn chain_faults(seen: &Seen, t0: i64) -> Vec<String> {
    let mut out = early_acts(seen, t0);
    let doubled = doubled_acts(seen, t0);
    if !doubled.is_empty() {
        out.push(format!(
            "acts typed with no point judged between: {doubled:?}"
        ));
    }
    out.extend(p3_faults(seen, t0));
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
    let Some((tw, wl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t >= *te && t < t1 && backoff_named(l) && word(l, "seq") == seq)
    else {
        return Verdict::Fail(format!(
            "the first reply read at +{} ({}), but no back-off decided at that point before land@v1 at +{}",
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
    if before.len() != 1 {
        bad.push(format!(
            "{} acts before the landing (one land@v1 is its due): {}",
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

fn trust_for_claude(run: &Run, dirs: &[PathBuf]) -> Result<(), Fail> {
    let p = run.w.join("home/.claude/.claude.json");
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

/// Gemini CLI's `trustedFolders.json` trusting the lab's project (`proj`, as named and as resolved) and every folder under it: the manager's and the other chains' folders, under L15's folder trust ([`launcher`]).
#[must_use]
pub fn gemini_trusted_project(proj: &str) -> String {
    let real = std::fs::canonicalize(proj)
        .map_or_else(|_| proj.to_string(), |p| p.to_string_lossy().into_owned());
    let mut rows = vec![(proj.to_string(), J::s("TRUST_FOLDER"))];
    if real != proj {
        rows.push((real, J::s("TRUST_FOLDER")));
    }
    J::Obj(rows).dump(crate::pyjson::PY)
}

fn launcher(
    run: &Run,
    h: Harness,
    dirs: &[PathBuf],
    (walled, worker): (bool, bool),
) -> Result<PathBuf, Fail> {
    use crate::gemini::Agent;
    let put = |p: &Path, mode: u32, text: &str| {
        crate::sys::put(p, mode, &crate::sys::PutSrc::Bytes(text.as_bytes()))
            .map_err(|e| could_not(&format!("{}: {e}", p.display())))
    };
    let script = run.run.join(format!("forever-{}.sh", h.name()));
    let text = match h {
        Harness::Claude => {
            trust_for_claude(run, dirs)?;
            if walled {
                crate::verbs::settings_merge(run, &J::obj([("apiKeyHelper", J::Null)]))?;
            }
            let l = crate::lab::Launch {
                subscriber: walled,
                ..crate::lab::Launch::default()
            };
            crate::lab::launcher(run, &l)
        }
        Harness::Gemini => {
            crate::gemini::seed(run, Agent::Gemini, &crate::gemini::Launch::default())?;
            if worker {
                let home = run.w.join(Agent::Gemini.home()).join(".gemini");
                put(
                    &home.join("settings.json"),
                    0o600,
                    &crate::gemini::gemini_settings(true),
                )?;
                put(
                    &home.join("trustedFolders.json"),
                    0o600,
                    &gemini_trusted_project(&run.proj),
                )?;
            }
            let tz = walled
                .then(|| format!("TZ={}", foreign_tz(crate::util::now_ms() / 1000)))
                .into_iter()
                .collect::<Vec<_>>();
            crate::gemini::launcher(run, Agent::Gemini, &tz, &[])
        }
        Harness::Agy => {
            crate::gemini::seed(run, Agent::Agy, &crate::gemini::Launch::default())?;
            let dir = run.w.join("agy-home/.gemini/antigravity-cli");
            put(
                &dir.join("settings.json"),
                0o600,
                &crate::gemini::agy_settings_with(&agy_trusted(dirs), &[]),
            )?;
            crate::gemini::launcher(run, Agent::Agy, &[], &[])
        }
        Harness::Muse => crate::muse::launcher(run, &[], &["--trust-workspace".to_string()]),
    };
    put(&script, 0o755, &text)?;
    Ok(script)
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

/// THE RENDERER THAT TAKES THE WHEEL, for `tall-scroll`'s tab ([`scroll_launcher`]): the harness reads the tab's LIVE screen (`text --json`), which a scroll of the tab's own viewport never moves, so only an agent that scrolls its own view brings a judged opener back into the harness's sight.
pub const SCROLL_CLAUDE_ENV: &str = "CLAUDE_CODE_NO_FLICKER=1";
/// agy's `tall-scroll` home under the run's scratch ([`SCROLL_CLAUDE_ENV`]'s reason): its settings the run's with `altScreenMode` `always`, and its onboarding done.
pub const SCROLL_AGY_HOME: &str = "agy-scroll-home";
/// The system settings Gemini CLI's `tall-scroll` tab reads ([`SCROLL_CLAUDE_ENV`]'s reason).
pub const SCROLL_GEMINI_SETTINGS: &str = "{\"ui\":{\"useAlternateBuffer\":true}}";
/// Where [`SCROLL_GEMINI_SETTINGS`] is written, under the run's scratch.
pub const SCROLL_GEMINI_FILE: &str = "gemini-scroll-settings.json";

fn scroll_launcher(
    run: &Run,
    h: Harness,
    dirs: &[PathBuf],
    walled: bool,
) -> Result<Option<PathBuf>, Fail> {
    use crate::gemini::Agent;
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
                ..crate::lab::Launch::default()
            },
        ),
        Harness::Gemini => {
            let settings = run.w.join(SCROLL_GEMINI_FILE);
            put(&settings, 0o644, SCROLL_GEMINI_SETTINGS)?;
            let mut envs = vec![format!(
                "GEMINI_CLI_SYSTEM_SETTINGS_PATH={}",
                settings.to_string_lossy()
            )];
            if walled {
                envs.push(format!("TZ={}", foreign_tz(crate::util::now_ms() / 1000)));
            }
            crate::gemini::launcher(run, Agent::Gemini, &envs, &[])
        }
        Harness::Agy => {
            let home = run.w.join(SCROLL_AGY_HOME);
            let dir = home.join(".gemini/antigravity-cli");
            put(
                &dir.join("settings.json"),
                0o600,
                &crate::gemini::agy_settings_with(
                    &agy_trusted(dirs),
                    &[("altScreenMode".to_string(), J::s("always"))],
                ),
            )?;
            put(
                &dir.join("cache/onboarding.json"),
                0o600,
                &crate::gemini::agy_onboarded(),
            )?;
            crate::gemini::launcher(
                run,
                Agent::Agy,
                &[format!("HOME={}", home.to_string_lossy())],
                &[],
            )
        }
        Harness::Muse => return Ok(None),
    };
    let script = run.run.join(format!("forever-{}-scroll.sh", h.name()));
    put(&script, 0o755, &text)?;
    Ok(Some(script))
}

/// The zone a walled run's Gemini CLI runs in ([`launcher`]): one whose offset at the unix second `now` is not the machine's, and a half-hour one (a guess of the machine's zone, or any whole hour off it, never lands on it): `Asia/Kolkata` (GMT+5:30, no daylight saving), or `America/St_Johns` (GMT-2:30 or -3:30) on a machine at +5:30.
#[must_use]
pub fn foreign_tz(now: i64) -> &'static str {
    if crate::sys::local_offset_s(now) == 19_800 {
        "America/St_Johns"
    } else {
        "Asia/Kolkata"
    }
}

struct Tab {
    chain: Chain,
    sid: String,
    dir: PathBuf,
    seen: Seen,
    t0: i64,
    ask_tried: bool,
    over: bool,
}

fn on(door: &Door<'_>, sid: &str, args: &[&str]) -> String {
    let at = format!("@{sid}");
    let mut v = vec![at.as_str()];
    v.extend_from_slice(args);
    door.out(&v)
}

fn at_composer(door: &Door<'_>, sid: &str, agent: &str) -> Option<String> {
    crate::screen::composer_row(agent, &on(door, sid, &["text"]))
}

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
        match word(said, "posted").and_then(|n| n.parse::<u64>().ok()) {
            Some(n) if r.code == 0 && said.starts_with("OK ") => {
                keyed.posted += n;
                if word(said, "key").as_deref() == Some("1") {
                    keyed.key_window += n;
                }
                Ok(())
            }
            _ => Err(could_not(&format!(
                "hwkey {key}: {said}{}: the person's keys need a windowed lab (forever --window)",
                r.stderr.trim()
            ))),
        }
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
    println!(
        "  keys      @{sid}: {} character(s) pressed through hwkey in {}; the draft stood {} after the last",
        keyed.posted,
        at(first, last),
        at(last, now_ms())
    );
    stop::sleep(Duration::from_millis(PERSON_STAND_MS))?;
    for _ in 0..2 {
        let pressed = now_ms();
        press("enter", &mut keyed)?;
        for _ in 0..20 {
            stop::sleep(Duration::from_millis(250))?;
            if !standing(door) {
                let status = on(door, sid, &["status"]);
                keyed.seq_after = seq(&status);
                let stamp = word(&status, "human_ms").unwrap_or_else(|| "?".to_string());
                return Ok(Some((pressed, stamp, keyed)));
            }
        }
    }
    Ok(None)
}

/// The `hwkey` name of the key that types `c`: `space` for a space (the verb splits its words on it), the character itself otherwise.
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
        Harness::Gemini => {
            let wrapper = run.run.join("gbin/gemini");
            put(&wrapper, 0o755, &gemini_wrapper(&run.gemini_bin))?;
            (
                crate::relaunch_gemini::shell_exports(run),
                format!("{} -> {}", wrapper.display(), run.gemini_bin),
            )
        }
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

/// The lab's `gemini` on the worker shell's `PATH` (`<run>/gbin`, out of the jail's reach): `exec` the lab's build, so the process is `node` running its script, as the manager's is.
#[must_use]
pub fn gemini_wrapper(bin: &str) -> String {
    format!(
        "#!/bin/sh\n# aterm-lab forever: the worker shell's gemini\nexec {} \"$@\"\n",
        shell_quote(bin)
    )
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
        t0,
        over: false,
        ask_tried: false,
    };
    Ok((tab, slot))
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

fn shown_rows(door: &Door<'_>, sid: &str) -> Vec<String> {
    on(door, sid, &["text"])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().take(150).collect())
        .collect()
}

fn scroll_step(door: &Door<'_>, tab: &mut Tab, now: i64) -> Result<(), Fail> {
    if tab.chain != Chain::TallScroll {
        return Ok(());
    }
    let token = tab.chain.token();
    let in_sight = |rows: &[String]| rows.iter().any(|r| r.contains(token.as_str()));
    if let Some(sc) = tab.seen.cover.scrolled.as_mut() {
        if sc.viewport.is_none() && sc.held.is_none() && now - sc.at >= SCROLL_HELD_MS {
            let rows = shown_rows(door, &tab.sid);
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
    let modes: Vec<String> = on(door, &tab.sid, &["modes"])
        .lines()
        .filter(|l| l.starts_with("alt_screen=") || l.starts_with("mouse_mode="))
        .map(str::to_string)
        .collect();
    let tracks = !modes.iter().any(|m| m == "mouse_mode=none");
    let history = crate::util::one_line(on(door, &tab.sid, &["lines"]).trim(), 80);
    let first = shown_rows(door, &tab.sid);
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
            let next = shown_rows(door, &tab.sid);
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
                rows = shown_rows(door, &tab.sid);
                let said = format!(
                    "scroll {}; the opener {}, the viewport rows {top}..{}, the live screen's {screen_first}..{}",
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
    let rows = tail_rows(door, &tab.sid, 12);
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

fn tool_results_of(run: &Run, token: &str) -> usize {
    crate::stub::turn_dumps(&run.w.join("dump"))
        .into_iter()
        .filter_map(|(_, p)| std::fs::read(p).ok())
        .filter(|body| String::from_utf8_lossy(body).contains(token))
        .filter(|body| {
            crate::stub::messages_answered(body)
                || crate::stub::gemini::results_answered(body)
                || crate::stub::muse::results_answered(body)
        })
        .count()
}

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

fn status_words(door: &Door<'_>, sid: &str) -> String {
    let s = on(door, sid, &["status"]);
    [
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
    ]
    .iter()
    .map(|k| format!("{k}={}", word(&s, k).unwrap_or_else(|| "?".into())))
    .collect::<Vec<_>>()
    .join(" ")
}

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

const MARK: &str = "[aterm harness]";

/// Whether a user text is the harness's: its mark, or one of its answers (`Nobody is here to …`: Land's answer, the reversible answer and the answer text carry no mark).
#[must_use]
pub fn harness_said(text: &str) -> bool {
    text.contains(MARK) || text.contains(ANSWER_HEAD)
}

/// The harness's user texts a request `body` of conversation `token` holds ([`harness_said`], by the reader of whichever of the three APIs it speaks); `None` for a request of another conversation.
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

/// CLAUDE CODE'S OWN CONTINUATION of a usage limit, as its request carries it: the opening words of the user text it sends when its countdown fires at the reset (`Your claude.ai usage limit has reset. Continue the task you were working on when the limit was reached; do not repeat work that is already complete.`, the transcript's `origin.kind` `auto-continuation` prompt), beside the act the limit cut.
pub const CLAUDE_CONTINUES: &str = "Your claude.ai usage limit has reset. Continue the task";

/// Whether request `body` of conversation `token` carries the program's own continuation of a usage limit: one of its user texts holds [`CLAUDE_CONTINUES`] (a Messages API body: Claude Code's).
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

/// The stub's dumps of the conversations' TURNS in `dir`, `(modified ms, body)` each: `req-<n>.json` alone ([`crate::stub::turn_dumps`]), never a side call's `side-<n>.json`, whose copy of a conversation is no request of its turns' — Muse Code's reminder observer renders the whole conversation into one text with line breaks, which C4 read as the owner's words reaching the agent so (the lab's `r10-cv`, 2026-10-22).
#[must_use]
pub fn dumps_in(dir: &Path) -> Vec<(i64, Vec<u8>)> {
    crate::stub::turn_dumps(dir)
        .into_iter()
        .filter_map(|(_, p)| {
            let t = std::fs::metadata(&p)
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?;
            let t = i64::try_from(t.as_millis()).ok()?;
            Some((t, std::fs::read(&p).ok()?))
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

/// C4's witness of what reached the agent ([`Cover::owner_texts`]): every user text of conversation `token`'s requests holding [`OWNER_KEY`], by the reader of whichever of the three APIs it speaks, each once.
#[must_use]
pub fn owner_texts(dumps: &[Vec<u8>], token: &str) -> Vec<String> {
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
            if u.contains(OWNER_KEY) && !out.contains(&u) {
                out.push(u);
            }
        }
    }
    out
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
        Chain::InstantPerson => {
            let asked = tab.seen.asked_ms?;
            Some(StubSeen::within(
                &dumps_of(run, &token),
                asked,
                tab.seen.end_ms,
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

/// The end (ms) of the walled chain's window no harness text may reach the stub in ([`Cover::window_stub`]), from the land act that met its wall: the wall's reset (W1, W2, W4's first wall: the resume past it meets the redraw), the last raise again's (W3: a try resends its turn, no new text), the first agent's exit (W5: the second's conversation is its own; W9: the agent launched again opens a new one, judged by its acts), the conversation read again (W6: held unread past the reset).
#[must_use]
pub fn wall_window_end(chain: Chain, cover: &Cover) -> Option<i64> {
    let (_, reset_s) = cover.wall?;
    Some(match chain {
        Chain::ShiftWall => cover.reset_now()? * 1000,
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

/// What the lab does to chain `chain`'s raised wall at `now`: nothing until [`WALL_LIFT_EARLY_MS`] before the reset now standing ([`Cover::reset_now`]); then raised again [`Chain::shift_step_ms`] on while the chain has shifts left ([`Chain::shifts`]: the try or the resume past that reset meets the new one), else lifted. W5's is never lifted by time: its switch lifts it ([`switch_step`]). Nothing once lifted, or before the wall was raised.
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

fn wall_step(run: &Run, h: Harness, tab: &mut Tab, now: i64) -> Result<(), Fail> {
    if !tab.chain.walled() {
        return Ok(());
    }
    match tab.seen.cover.wall {
        None => {
            let pointed = tab.seen.journal.iter().any(|(t, l)| {
                *t > tab.t0 && idle_words(l).is_some_and(|w| !w.is_empty() && w != "-")
            });
            if !pointed || !tab.seen.all_acts().is_empty() {
                return Ok(());
            }
            let reset = reset_s_ahead(now, tab.chain.wall_ahead_ms());
            let claim = tab.chain.wall_claim_on(h);
            let answer = crate::verbs::set_mode(run, &format!("limit:{claim}:{reset}"))?;
            tab.seen.cover.wall = Some((now, reset));
            println!(
                "  [{}] +{} stub    the usage wall raised: {claim}, resets at +{} ({})",
                tab.chain.name(),
                at(tab.t0, now),
                at(tab.t0, reset * 1000),
                crate::util::one_line(&answer, 120)
            );
        }
        Some(_) => match wall_move(tab.chain, &tab.seen.cover, now) {
            WallMove::Hold => {}
            WallMove::Shift(reset) => {
                let claim = tab.chain.wall_claim_on(h);
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

/// Whether harness `h`'s screen `text` no longer draws the wall's notice: its composer drawn, no row holding any of the wall's words ([`Hide::words`]), and its reader reading no wall — at Gemini CLI, no usage-limit dialog (its Stop leaves `✕ [API Error: An unknown error occurred.]`, a wall that no longer names the limit).
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
        Harness::Gemini => (
            aterm_phase::gemini::composer_draft(&rows).is_some(),
            aterm_phase::gemini::prompt(&rows)
                .is_some_and(|p| p.kind == aterm_phase::PromptKind::UsageLimit),
        ),
        Harness::Claude => (
            claude_composer(&rows).is_some(),
            aterm_phase::read(None, &rows, None).wall.is_some(),
        ),
    };
    composer && !wall && !hide.words.iter().any(|w| text.contains(w))
}

/// Claude Code's composer's first row (its `❯` taken off), between the composer's rules as its reader finds them (`aterm_phase::phase:: composer_rules`, which takes a rule carrying a label: `──── forever-w2 ─` once `/rename` named the session), or `None` where no frame is drawn. Not `screen::composer_row`'s Claude reading, which also takes agy's `>` frame for a composer.
#[must_use]
pub fn claude_composer(rows: &[String]) -> Option<String> {
    let (top, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let first = rows.get(top + 1..bottom)?.first()?;
    let first = first.replace('\u{a0}', " ");
    Some(first.strip_prefix('❯').unwrap_or(&first).trim().to_string())
}

/// The option a dialog on `text` draws focused (`● 2. Stop` inside its box's `│` borders reads `2. Stop`), or `None` where no row draws one.
#[must_use]
pub fn dialog_focus(text: &str) -> Option<String> {
    text.lines().find_map(|r| {
        let inner = r
            .trim()
            .trim_start_matches('│')
            .trim_end_matches('│')
            .trim();
        inner
            .strip_prefix("● ")
            .map(|o| o.trim().to_string())
            .filter(|o| !o.is_empty())
    })
}

/// Whether harness `h`'s composer holds `cmd` as typed and its command menu's first row is that very command, so Enter runs it.
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
        Harness::Gemini => false,
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
    let cmd = hide.label();
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
            stopped: matches!(hide.by, HideBy::Option(_)),
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
    if let HideBy::Option(option) = hide.by {
        let why = match choose_option(door, &at_tab, &tab.sid, option, deadline)? {
            Ok(enter) => {
                say(tab, &format!("{option} chosen"));
                let mut left = false;
                for _ in 0..10 {
                    stop::sleep(Duration::from_millis(1000))?;
                    if notice_gone(h, &on(door, &tab.sid, &["text"])) {
                        left = true;
                        break;
                    }
                }
                if left {
                    gone(tab, enter, enter, 1);
                    return Ok(());
                }
                format!("the notice still in sight 10 s after {option}")
            }
            Err(why) => why,
        };
        say(tab, &format!("the notice NOT taken off the screen: {why}"));
        tab.seen.cover.hide_gave_up = Some(why);
        return Ok(());
    }
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
        let composer = match h {
            Harness::Claude => {
                claude_composer(&text.lines().map(str::to_string).collect::<Vec<_>>())
            }
            _ => crate::screen::composer_row(h.name(), &text),
        };
        if composer.as_deref() != Some("") {
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

/// Whether the program's own countdown stands armed on `text`: its armed words drawn ([`Cancel::armed`]) and not yet its cancel ([`Cancel::cancelled`]; the armed row stays in the conversation above it once cancelled).
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

fn choose_option(
    door: &Door<'_>,
    at_tab: &str,
    sid: &str,
    option: &str,
    deadline: i64,
) -> Result<Result<i64, String>, Fail> {
    if now_ms() >= deadline {
        return Ok(Err(format!(
            "the reset under {} away before {option} was chosen",
            at(0, HIDE_BEFORE_RESET_MS)
        )));
    }
    let focus = dialog_focus(&on(door, sid, &["text"]));
    if focus.as_deref() != Some("1. Keep trying") {
        return Ok(Err(format!(
            "the dialog's focus read {focus:?}, not its first option: nothing pressed"
        )));
    }
    if door.ctl(&[at_tab, "key", "down"])?.code != 0 {
        return Ok(Err("`key down` refused".to_string()));
    }
    stop::sleep(Duration::from_millis(1000))?;
    let focus = dialog_focus(&on(door, sid, &["text"]));
    if focus.as_deref() != Some(option) {
        let _ = door.ctl(&[at_tab, "key", "up"]);
        return Ok(Err(format!(
            "Down moved the focus to {focus:?}, not {option}: Enter not pressed"
        )));
    }
    let enter = now_ms();
    if door.ctl(&[at_tab, "key", "enter"])?.code != 0 {
        return Ok(Err("`key enter` refused".to_string()));
    }
    Ok(Ok(enter))
}

/// The agent W5 starts in harness `h`'s tab under `h`'s remembered limit ([`switch_step`]): one whose limits the loop reads and decides on too (`turn_end.rs`'s `met_under_another`), on another account: Muse Code after Claude Code, Gemini CLI or agy, Claude Code after Muse Code — the pairs measured on 2026-10-21, where each held the other.
#[must_use]
pub fn switch_to(h: Harness) -> Harness {
    match h {
        Harness::Muse => Harness::Claude,
        Harness::Claude | Harness::Gemini | Harness::Agy => Harness::Muse,
    }
}

/// How W5 ends harness `h`'s agent at its wall ([`exit_agent`]): Muse Code takes ctrl+c twice (measured: its `exit-confirm` capture); Claude Code too (`footers.rs`'s quit), but its usage limit's countdown takes the first ctrl+c for its cancel, so the countdown is cancelled first where it stands armed ([`CLAUDE_CANCEL`]; measured on 2.1.308: ctrl+c twice at the armed countdown drew `Automatic continue cancelled` and left Claude Code in the tab); agy `/quit` (its relaunch lane's); Gemini CLI's usage-limit dialog takes the keys first, so it is answered `2. Stop` ([`GEMINI_HIDE_OPTION`]) and then `/quit` (its relaunch lane's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// ctrl+c, half a second, ctrl+c.
    CtrlCTwice,
    /// The program's own countdown cancelled where it stands armed ([`countdown_armed`], [`cancel_countdown`]), then ctrl+c twice.
    CancelThenCtrlCTwice(Cancel),
    /// The command, then Enter.
    Command(&'static str),
    /// The dialog's option chosen, then the command.
    OptionThenCommand(&'static str, &'static str),
}

/// [`Exit`] of harness `h`.
#[must_use]
pub fn exit_of(h: Harness) -> Exit {
    match h {
        Harness::Claude => Exit::CancelThenCtrlCTwice(CLAUDE_CANCEL),
        Harness::Muse => Exit::CtrlCTwice,
        Harness::Agy => Exit::Command("/quit"),
        Harness::Gemini => Exit::OptionThenCommand(GEMINI_HIDE_OPTION, "/quit"),
    }
}

/// The program `ls` names in session `sid`'s row (`program=<word>`), a login shell's leading `-` cut.
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
        Exit::OptionThenCommand(option, cmd) => {
            if let Err(why) = choose_option(door, &at_tab, sid, option, i64::MAX)? {
                return Ok(Err(why));
            }
            stop::sleep(Duration::from_millis(2000))?;
            if !command(door, cmd)? {
                return Ok(Err(format!("{option} chosen, but `{cmd}` not entered")));
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

/// How long past the first agent's reset W5 watches its tab: the longest grace a program's resume waits ([`wall_grace_ms`]) and [`PERSISTENCE_GRACE_MS`] more, so a resume of the ended limit typed into the second agent is seen.
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
    let script = launcher(run, to, std::slice::from_ref(&tab.dir), (!lift, false))?;
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    let _ = on(door, &tab.sid, &["send", &line]);
    let _ = on(door, &tab.sid, &["key", "enter"]);
    let launched_ms = now_ms();
    let ready = crate::wait_until(Duration::from_secs(90), || {
        at_composer(door, &tab.sid, to.name()).is_some_and(|c| c.is_empty())
    });
    if !ready {
        return give_up(format!(
            "{} never stood at an empty composer within 90 s",
            to.name()
        ));
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

/// Where W6 moves a transcript aside ([`unread_step`]): `<id>.jsonl.unread` beside it, which no reader of `*.jsonl` takes.
#[must_use]
pub fn aside_of(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".unread");
    file.with_file_name(name)
}

/// When the loop told the owner of a limit held unread ([`unread_told_words`]): the first `EXCEPTION` line after `since` that carries the told's words.
#[must_use]
pub fn told_unread(seen: &Seen, since: i64) -> Option<i64> {
    let words = unread_told_words();
    seen.journal
        .iter()
        .find(|(t, l)| *t > since && l.starts_with("EXCEPTION ") && l.contains(words.as_str()))
        .map(|(t, _)| *t)
}

/// Whether W6's lab reads the conversation again at `now` ([`unread_step`]): [`UNREAD_RESTORE_AFTER_TOLD_MS`] after the told, or, with none seen, [`UNREAD_TOLD_LATE_MS`] past its due (the reset and [`futile_after_ms`]), so the run ends either way.
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
                    && std::fs::read_to_string(p).is_ok_and(|t| t.contains(&token))
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
    let status = status_words(door, &tab.sid);
    tab.seen.status_reads.push(now);
    put(&mut tab.seen.status, "status", status);
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
        | Chain::LandE
        | Chain::LandQ
        | Chain::LandW
        | Chain::Worker
        | Chain::Wall
        | Chain::WallHid
        | Chain::WallDay
        | Chain::WallClimb
        | Chain::ShiftWall
        | Chain::RedrawWall
        | Chain::SwitchWall
        | Chain::UnreadWall
        | Chain::DayStopWall
        | Chain::RelaunchWall
        | Chain::BucketWall => false,
        Chain::CruiseWall => walls::cruise_went_on(s).is_some_and(|t| now - t >= 10_000),
        Chain::Mgr => true,
        Chain::LandT => done_since(20_000),
        Chain::LandP => done_since(20_000),
        Chain::Cap => cover::cap_watched(s, now) || done_since(20_000),
        Chain::InstantLand => s.all_acts().is_empty() && now - tab.t0 > 300_000,
        Chain::LandB | Chain::LandC | Chain::LandF | Chain::LandG | Chain::LandH | Chain::LandI => {
            nudge.is_some_and(|n| {
                s.yield_after(n, None, None)
                    .waiting
                    .is_some_and(|(w, _)| now - w >= 3000)
            }) || done_since(20_000)
        }
        Chain::LandD => nudge.is_some_and(|n| now - n >= 10_000) || done_since(20_000),
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
            check.is_some_and(|c| s.all_acts().iter().any(|(t, _)| *t > c)) || done_since(20_000)
        }
        Chain::CruiseO => {
            first(turn_end::RULE_CONTINUE).is_some_and(|c| {
                s.all_acts().iter().any(|(t, _)| *t > c)
                    || s.yield_after(c, None, Some(DONE_CLAIM_KEY))
                        .event
                        .is_some_and(|(t, _)| now - t >= 20_000)
            }) || done_since(20_000)
        }
        Chain::Off => s
            .reply_ms
            .is_some_and(|r| now - r >= QUIET_UNDER_OFF_MS + 5000),
        Chain::LandJ | Chain::LandK => done_since(20_000),
        Chain::InstantPerson => {
            let asked = s.asked_ms;
            let left = asked.and_then(|a| {
                s.journal
                    .iter()
                    .find(|(t, l)| *t > a && l.starts_with("LEFT "))
                    .map(|(t, _)| *t)
            });
            let decided = asked.and_then(|a| own_backoff_after(s, a)).map(|(t, _)| *t);
            let typed_into =
                asked.is_some_and(|a| s.all_acts().iter().any(|(t, _)| *t > a && now - *t >= 3000));
            let unjudged =
                asked.is_some_and(|a| left.is_none() && decided.is_none() && now - a > 150_000);
            let refused = s.ask_refused_ms.is_some_and(|a| now - a > 5000);
            left.is_some_and(|l| now - l >= QUIET_UNDER_OFF_MS + 5000)
                || decided.is_some_and(|d| now - d >= 10_000)
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
        }
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
    (surfaces_on, window): (bool, bool),
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
    let walled = chains.iter().any(|c| c.walled());
    let script = launcher(&run, h, &dirs, (walled, chains.contains(&Chain::Worker)))?;
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
    let mut order: Vec<usize> = (0..chains.len()).collect();
    if desk.is_some()
        && let Some(i) = chains.iter().position(|c| *c == Chain::CruiseN)
    {
        order.remove(i);
        order.insert(0, i);
    }
    for (chain, dir) in order.into_iter().map(|i| (chains[i], &dirs[i])) {
        if chain.opening() == Opening::Spawned {
            continue;
        }
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
        let script = match &scroll_script {
            Some(own) if chain == Chain::TallScroll => own,
            _ => &script,
        };
        let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
        let _ = on(&door, &sid, &["send", &line]);
        let _ = on(&door, &sid, &["key", "enter"]);
        println!(
            "  tab       {} @{sid} in {}: autopilot {} ({}){task}{titled}, launched",
            chain.name(),
            dir.display(),
            chain.autopilot(),
            set.trim()
        );
        tabs.push(Tab {
            chain,
            sid,
            dir: dir.clone(),
            seen: Seen {
                window: surfaces::Window {
                    label: chain.name().to_string(),
                    ..surfaces::Window::default()
                },
                ..Seen::default()
            },
            t0: 0,
            ask_tried: false,
            over: false,
        });
    }
    if let Some(d) = desk.as_mut()
        && let Some(i) = surfaces::front_of(&tabs.iter().map(|t| t.chain).collect::<Vec<_>>())
    {
        let label = tabs[i].seen.window.label.clone();
        let strip = surfaces::strip_tabs(&door.out(&["chrome"]));
        let Some(at) = strip.iter().find(|t| t.label == label).map(|t| t.index) else {
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
        tab.t0 = now_ms();
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
            tab.seen.cover.person_ms.map_or_else(String::new, |p| format!(
                " as a person types it (the draft stood {} s, Enter at +{}; status human_ms={})",
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
        let (tab, slot) = spawn_worker((tag, &run, h), &door, &tabs, wdir)?;
        tabs.push(tab);
        awaiting = Some(slot);
    }
    let watch0 = now_ms();
    loop {
        let now = now_ms();
        let glance = desk.as_mut().map(|d| {
            let g = surfaces::glance(&door, d);
            for (what, v) in d.note(&g) {
                println!(
                    "  [desk] +{} {what:<8} {}",
                    at(watch0, g.t),
                    crate::util::one_line(&v, 240)
                );
            }
            g
        });
        for tab in tabs.iter_mut().filter(|t| !t.over) {
            look(&run, &door, tab);
            if let Some(g) = &glance {
                for (what, v) in tab.seen.window.note(g, &tab.sid) {
                    println!(
                        "  [{}] +{} {what:<7} {}",
                        tab.chain.name(),
                        at(tab.t0, g.t),
                        crate::util::one_line(&v, 240)
                    );
                }
            }
            if let Some(ask) = tab.chain.asks()
                && !tab.ask_tried
                && ask_due(&tab.seen, now)
                && at_composer(&door, &tab.sid, h.name()).is_some_and(|c| c.is_empty())
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
            wall_step(&run, h, tab, now)?;
            hide_step(&door, h, tab, now)?;
            switch_step(&run, &door, h, tab)?;
            relaunch_step(&run, &door, h, tab)?;
            unread_step(&run, tab, now);
            resize_step(&door, tab, now)?;
            scroll_step(&door, tab, now)?;
            first_screen_step(&door, tab);
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
                tab.seen.stub = stub_witness(&run, tab);
                tab.seen.cover.window_stub = window_witness(&run, tab);
                tab.seen.cover.went_on_ms = went_on_witness(&run, tab);
                tab.seen.cover.owner_texts = owner_witness(&run, tab);
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
            let v = surfaces::judge_end(d, &windows, h.name());
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
    (surfaces_on, window): (bool, bool),
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
            "forever: lab '{tag}' OPENED A WINDOW on this desk (instance pid {}): L13's person types through hwkey",
            r.map_or(0, |r| r.instance_pid)
        );
    }
    let verdicts = run_check(tag, h, chains, (surfaces_on, window));
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
    if mode == Mode::Ladder {
        u.instance_env = format!("{} {}", u.instance_env, ladder_env())
            .trim()
            .to_string();
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
                let w = it.next().map(String::as_str);
                mode = w.and_then(Mode::of).ok_or_else(|| {
                    usage(&format!(
                        "--mode takes land, cruise, both, cover, cap, wall, wall-hid, wall-day, wall-climb, wall-shift, wall-redraw, wall-switch, wall-unread, wall-day-stop, wall-cruise, wall-relaunch, wall-bucket, resid or ladder, not {w:?}"
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
            "-h" | "--help" => return Ok(None),
            s if s.starts_with('-') => return Err(usage(&format!("unknown option '{s}'"))),
            s => tag = Some(s.to_string()),
        }
    }
    let harnesses = harnesses.ok_or_else(|| usage("say --harness claude|gemini|agy|muse|all"))?;
    let asked = Asked {
        harnesses,
        mode,
        chains,
        tag,
        keep,
        surfaces,
        window,
    };
    if asked.window
        && !asked.harnesses.iter().any(|&h| {
            asked
                .chains(h)
                .iter()
                .any(|c| c.opening() == Opening::Person)
        })
    {
        return Err(usage(
            "--window opens a window for a person's keys (hwkey): the run needs land-p (--mode cover, \
             --chains land-p)",
        ));
    }
    Ok(Some(asked))
}

/// `forever --harness <claude|gemini|agy|muse|all> [--mode land|cruise|both|cover|cap|wall|wall-hid|wall-day|wall-climb|wall-shift|wall-redraw|wall-switch|wall-unread|wall-day-stop|wall-cruise|wall-relaunch|wall-bucket|resid|ladder] [--chains a,b] [--no-surfaces] [--window] [tag] [--keep]`.
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
        let tool_end_asked = Chain::of_mode(a.mode).contains(&Chain::ToolEnd)
            && a.chains
                .as_ref()
                .is_none_or(|only| only.contains(&Chain::ToolEnd));
        if tool_end_asked && let Some(why) = tool_end_out_of_reach(h) {
            println!("forever: {} F3 is out of this lab's reach: {why}", h.name());
            lines.push((h, Scenario::F3, Verdict::Unobserved(why.to_string())));
        }
        if chains.is_empty() && a.mode.walled().is_none() {
            println!(
                "forever: {} runs none of the chains --chains asks of its mode",
                h.name()
            );
            continue;
        }
        match run_lab(&tag, h, a.mode, &chains, a.keep, (a.surfaces, a.window)) {
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

#[path = "forever_cover.rs"]
pub mod cover;

#[path = "forever_walls.rs"]
pub mod walls;

#[path = "forever_surfaces.rs"]
pub mod surfaces;

#[cfg(test)]
#[path = "forever_tests.rs"]
mod tests;
