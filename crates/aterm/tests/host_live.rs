// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the IN-GUI SUPERVISOR HOST (`aterm-gui`'s
//! `harness_host`): a headless instance supervises every Claude Code session
//! it owns by default, with nothing started by hand — no `aterm supervise`,
//! no command typed for it.
//!
//! The worker is a FAKE `claude`: a `#!/bin/sh` script named `claude` (so the
//! server publishes `program=claude`, the shim rule of `session_program`)
//! that draws aterm-phase's measured Claude Code 2.1.280/2.1.282/2.1.283
//! screens one after another, waits for ONE key after each — one byte, or an
//! escape and the two bytes after it, so an arrow is one key — and appends it
//! to a key log (`1`, `enter`, `up`, `down`: [`keys`]); a screen named
//! `*.hold` is shown for a few seconds and reads nothing, and one named
//! `*.late` is not drawn at all: the fake waits [`LATE_S`] first, an agent
//! still starting; a screen named `*.type` is shown and then ONE bounded read
//! (`dd bs=4096 count=1`) takes whatever was written to it — a policy text,
//! one `send`, arrives whole in that one read — logged as `text:<bytes>`. The
//! key log is the ground truth of what the host typed.
//!
//! * THE HOST ATTACHES: the server publishes the fake as an agent, and the
//!   host's claim (`meta-change field=supervisor`) is on the session's
//!   timeline.
//! * BY DEFAULT EVERY BOX IS ANSWERED (the owner's direction of
//!   2026-09-24): the rm circuit breaker over `S=/usr` gets `1`, and so does
//!   a `touch x` box; a 49-row screen's tall box is read whole.
//! * AND EVERY QUESTION WITH ITS RECOMMENDED OPTION (`[harness]
//!   answer_questions`, the owner's directive of 2026-09-25): the four-tab
//!   dialog of that day's incident gets Enter on each tab's `(Recommended)`
//!   option and on its review's `Submit answers` — never a digit; a focus on
//!   the free-text row is moved up, one row a key, before the Enter; a
//!   question a person typed into is handed to them, and so is every
//!   question under `answer_questions = false`, with nothing typed. A
//!   question in a shape 2.1.282 does not draw (the hand-built column-1
//!   `Yes`/`No`) is handed over as one aterm-phase did not read whole.
//! * UNDER `[harness] question_priority = "long-term"` (or a session's own
//!   `meta set questions short-term`) THE WORKER DECIDES, on the 2.1.283
//!   screens measured with the policy text typed (c1, c3, c8 — committed as
//!   aterm-phase's `claude-2.1.283-question-*` fixtures and redrawn with the
//!   text the harness types): the host moves the focus down onto the
//!   free-text row, writes the word's fixed policy text with ONE `send` (the
//!   fake's `*.type` read: `text:<the text>` exactly), reads it back, and
//!   presses Enter on it — `[down, down, text, enter]` — or, on a
//!   multi-select, `down` to its `Submit` button (never an Enter in the
//!   field, never a Tab), Enter there and on the review's `Submit answers`;
//!   its ledger holds the word's own rule (`answer-long-term@v1`,
//!   `answer-short-term@v1`) and its journal `CHOSE … policy=<word>`, the
//!   question `decided by the worker (<word>)`. A person's text in the field
//!   is theirs: badged, nothing written. NEGATIVE CONTROL: a dialog that
//!   never takes the text (the field still `Type something.` after it) gets
//!   that ONE text, no Enter and no second text, and is handed over — so the
//!   Enter above is the read-back's, not the script's.
//! * WHAT THE SAFE RULES PROVE KEEPS ITS RULE: a read-only Bash box gets `1`;
//!   the session survey `0`; the rm circuit breaker in a bypass session over
//!   a scratch path `1`.
//! * UNDER `approve = "safe"` WITH `rm_breaker_ask = true`, WHAT THEY CANNOT
//!   PROVE IS HANDED TO THE HUMAN (by default an unproven rm breaker is
//!   approved with a WARN line instead, 4be9d668d): the same breaker over
//!   `S=/usr` and a `touch x` box leave the attention set, naming the
//!   command, and nothing typed; switching `[harness] enabled` off in the file
//!   stops the supervisor (its claim and its badge go) and still types nothing.
//! * ONE SUPERVISOR PER SESSION: another holder's live claim keeps the host
//!   off the session; it attaches once that claim lapses.
//! * NEGATIVE CONTROL: the same headless instance under `headless = false`
//!   (the one key that takes a headless instance's supervision away; the
//!   default supervises it) runs no host — the box waits, `supervisor=-`,
//!   nothing typed.
//!
//! * A BOX A PERSON ANSWERED LOSES ITS BADGE AT ONCE: under `approve =
//!   "none"` the owner's own box (2026-09-24) is escalated; answered, it is
//!   cleared while the worker is still busy — never at the next idle point
//!   (the 0.92.0 host's silent loop).
//!
//! * AN AGENT THAT EXITS UNASKED IS RELAUNCHED: a fake that dies by SIGKILL
//!   is started again in its tab on its own conversation (`--resume <id>`,
//!   its launch flags kept), handed to a supervisor at once, and given its
//!   continuation at that supervisor's loop's first idle point.
//!
//! * THE LIVE UPGRADE IS A STEP OF THE WORKER: atpkg's activation notice
//!   (a push, no sweep) asks the loop for its next idle point, where the worker
//!   announces, reads READY, ends the agent and relaunches it on the new
//!   build on its conversation, and carries it on — and the tab's `upgrade=`
//!   says so on this headless instance. A session nobody has asked anything
//!   is restarted on the new build afresh instead, nothing typed into it.
//!
//! * AN AGENT WHOSE EXIT WAS ITS OWN END IS LEFT ALONE: every fake above
//!   exits when its screens are done, under the full-power default
//!   (`relaunch` and `upgrade` on) — never having registered a conversation,
//!   its exit is journaled and neither relaunched nor said.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off but the supervisor under test (its `[harness]`
//! at the full-power default, the host's own acts included), `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance opens no window, and
//! the fake is typed by its absolute path so no real agent
//! on the machine's PATH can start.
//!
//! RUNNING: `targo --unverified test -p aterm --test aterm_suite -- host_live::`. Every test
//! boots its OWN headless instance from the binary just built, in its own
//! scratch world under `$TMPDIR` (or `/tmp` where `$TMPDIR` would make the
//! control socket's path too long), and removes it at the end, panics
//! included; it never dials a running aterm or touches a real session. On a
//! machine whose `$TMPDIR` is swept (macOS's nightly cleaner), point it at a
//! short durable directory for the run: `TMPDIR="$HOME/scratch.noindex/hl" …`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::supervise::QuestionPriority;
use aterm_agent::supervise::prompt::fixtures::{
    QUESTION_283_FREE_TEXT_FOCUSED, QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED,
    QUESTION_283_MULTISELECT_REVIEW, QUESTION_283_MULTISELECT_TYPED_BUTTON,
    QUESTION_283_MULTISELECT_TYPED_LONG_TERM, QUESTION_283_TYPED_LONG_TERM,
    QUESTION_283_TYPED_PERSON, QUESTION_283_TYPED_SHORT_TERM, rewrap_free_text, screen,
};

use crate::{headless_boot, launch_isolation};

use headless_boot::{ctl, ctl_ok, field, log_tail};

/// How long a `*.hold` screen stays up: long enough for the supervisor to
/// read it settled (its footer is what says bypass).
const HOLD_S: u32 = 5;
/// How long a `*.late` scene keeps the screen blank: longer than the
/// supervisor's idle window (`aterm_agent::supervise`'s `IDLE_MS`, 2 s), so
/// its first look settles on the blank screen as an idle point — as it does
/// on a real Claude Code whose first frame comes seconds after its exec (its
/// own start-up, a loaded machine).
const LATE_S: u32 = 3;
/// How long "nothing typed" is watched for after the escalation showed.
const QUIET: Duration = Duration::from_secs(3);
/// How long a wait for something that MUST happen — a key the host types, the
/// fake's exit, an attention, a journal or log line, a launch — may take
/// before it is read as a hang (AGENTS.md: a hang detector is a minute, never
/// a latency budget). Each wait returns the moment its event lands, so it
/// costs nothing when the product is right; the windows that watch for
/// something NOT happening ([`QUIET`] and the like) are the property and stay
/// their own.
const HANG: Duration = Duration::from_secs(60);
/// How long a claim's RELEASE may take to show. Not a [`HANG`]: the claim is
/// also a lease that lapses by itself (`aterm_agent::supervise::claim`:
/// `CLAIM_TTL_MS` 90 s, renewed at most every `CLAIM_RENEW` 20 s by a loop whose
/// waits run up to 20 s), so a lapse can show about 50 s after the stop, and a
/// wait that long would pass a host that never released it.
const RELEASED: Duration = Duration::from_secs(30);

/// One booted headless instance ([`headless_boot::Instance`], torn down on every
/// exit path) and the sid of its one session.
struct Instance {
    live: headless_boot::Instance,
    sid: String,
}

impl std::ops::Deref for Instance {
    type Target = headless_boot::Instance;
    fn deref(&self) -> &Self::Target {
        &self.live
    }
}

impl std::ops::DerefMut for Instance {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.live
    }
}

/// Boot one headless instance under `harness` config, 40 rows: its `[harness]`
/// table the keys `harness` in place of the isolation config's (which switches
/// the supervisor off).
fn boot(tag: &str, harness: &str) -> Option<Instance> {
    boot_sized(tag, harness, 40, None)
}

/// The live upgrade's settle (`aterm_agent::harness::upgrade::QUIET_S`, 20 s:
/// Claude idle and its screen quiet that long before the upgrade types or ends
/// anything) as the upgrade tests that are not about its length run it, set
/// through its development seam (`upgrade::QUIET_SEAM`, which this debug build
/// reads and a shipped binary never compiles). They pin WHAT happens at the
/// idle point (the notice, READY, the restart, the fresh start, the hand on
/// the tab), and the product's 20 s was 20 of each one's 26-32 s run alone
/// (2026-10-06). Twice the loop's own idle window (`supervise`'s `IDLE_MS`,
/// 2 s), so a look at an idle point the session has just reached still waits
/// `settling` and the look one first rung later takes the step, as in the
/// product (the journal of
/// [`a_session_nobody_asked_anything_is_restarted_afresh_with_nothing_typed`]:
/// `wait:settling`, then `done:fresh` 5.4 s later, one 4 s rung where the
/// product's is 20 s). The test whose property IS the settle's length, a
/// first prompt sent at the first idle verdict landing before the fresh
/// restart, runs the product's
/// ([`a_first_prompt_sent_at_the_first_idle_is_the_agents_never_the_shells`]).
#[cfg(target_os = "macos")] // only the macOS-only upgrade tests use it
const SHORT_SETTLE_S: u64 = 4;

/// [`boot`] for a test of the live upgrade: its settle `settle_s` seconds
/// ([`SHORT_SETTLE_S`]), or the product's for `None`.
#[cfg(target_os = "macos")] // only the macOS-only upgrade tests use it
fn boot_upgrade(tag: &str, harness: &str, settle_s: Option<u64>) -> Option<Instance> {
    boot_sized(tag, harness, 40, settle_s)
}

/// Boot one headless instance with `harness` appended to its config and the
/// fixture world written ([`headless_boot::boot_with`]: `None` is an
/// environment refusal; a product that cannot start fails the test), and read
/// its one session's sid. `settle_s`: the live upgrade's settle, through its
/// seam ([`boot_upgrade`]); `None` leaves the product's.
///
/// `tag` names the scratch root (`athh<tag>-<pid>`, its socket included), so
/// it is this binary's alone: two tests on one tag ran in one root at once,
/// each killing the other's instance (2026-09-29: `t` and `u` were each
/// taken twice, and two tests failed in every whole-file run and passed
/// alone). A reused tag fails here, before it can boot.
fn boot_sized(tag: &str, harness: &str, lines: u16, settle_s: Option<u64>) -> Option<Instance> {
    static TAKEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    {
        let mut taken = TAKEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !taken.iter().any(|t| t == tag),
            "boot tag `{tag}` is taken: two boots would share one scratch root"
        );
        taken.push(tag.to_owned());
    }
    let live = headless_boot::boot_with(
        &format!("athh{tag}"),
        |tmp, cmd| {
            let cfg = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&cfg).expect("the isolation config");
            assert!(base.contains(launch_isolation::HARNESS_OFF), "{base}");
            let base = base.replace(launch_isolation::HARNESS_OFF, "");
            std::fs::write(&cfg, format!("{base}[harness]\n{harness}")).expect("write the config");
            write_world(tmp);
            // The supervisor's lines (`harness @<sid>: …`) are the diagnosis a
            // failed assertion prints ([`harness_lines`]).
            // (A development seam: this is a debug build.)
            cmd.env("ATERM_LOG", "info")
                // The fixtures are 120-140 columns wide; Claude Code lays out its
                // own rows, so a terminal wrap is not a shape it draws.
                .args(["--lines", &lines.to_string(), "--columns", "200"]);
            if let Some(settle) = settle_s {
                // A development seam too ([`SHORT_SETTLE_S`]).
                cmd.env(
                    aterm_agent::harness::upgrade::QUIET_SEAM,
                    settle.to_string(),
                );
            }
        },
        |_| true,
    )?;
    let mut inst = Instance {
        live,
        sid: String::new(),
    };
    inst.sid = boot_session(&inst);
    Some(inst)
}

/// Undo the wire's pct-encoding (`%20` and friends).
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
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

/// The boot session's sid, from `sessions`.
fn boot_session(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["sessions"]);
    let row = body
        .lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("a session row: {body}"));
    row.split_whitespace().nth(1).expect("a sid").to_string()
}

fn status(inst: &Instance) -> String {
    ctl_ok(inst, &[&format!("@{}", inst.sid), "status"])
}

fn meta(inst: &Instance) -> String {
    ctl_ok(inst, &[&format!("@{}", inst.sid), "meta"])
}

/// The session's attention text, decoded (`None` for `-`).
fn attention(inst: &Instance) -> Option<String> {
    let m = meta(inst);
    field(&m, "attention").filter(|v| *v != "-").map(pct_decode)
}

/// The supervisor host's lines from the instance's log (under its scratch
/// home), for a failure's message.
fn harness_lines(inst: &Instance) -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().is_some_and(|n| n == "aterm.log") {
                out.push(p);
            }
        }
    }
    let mut logs = Vec::new();
    walk(&inst.tmp.join("home"), &mut logs);
    walk(&inst.tmp.join("state"), &mut logs);
    logs.iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains("harness"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first file named `name` under `dir`, at any depth.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        if p.is_dir() {
            find_file(&p, name)
        } else {
            (p.file_name().is_some_and(|n| n == name)).then_some(p)
        }
    })
}

/// Poll `probe` until `pred` holds, returning the matching record, or panic
/// with the last record after `within`.
fn until(
    within: Duration,
    what: &str,
    probe: impl Fn() -> String,
    pred: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + within;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = probe();
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what}: not within {within:?}; last: {last}");
}

/// The boot session's timeline records after id `after`, as `(id, kind,
/// payload)`, in the server's record order.
fn timeline_since(inst: &Instance, after: u64) -> Vec<(u64, String, String)> {
    let since = format!("since={after}");
    ctl_ok(inst, &[&format!("@{}", inst.sid), "timeline", &since])
        .lines()
        .filter_map(|l| {
            let (id, rest) = l.strip_prefix("event ")?.split_once(" t=")?;
            let (_, rest) = rest.split_once(" kind=")?;
            let (kind, payload) = rest.split_once(' ').unwrap_or((rest, ""));
            Some((id.parse().ok()?, kind.to_string(), payload.to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The fake worker and its screens.
// ---------------------------------------------------------------------------

/// A fixture without its provenance line.
fn body(fixture: &str) -> String {
    fixture
        .split_once('\n')
        .map_or(fixture, |(_, rest)| rest)
        .to_string()
}

const BOX_TOUCH: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.280-box-bash-touch.txt");
const BOX_RM: &str = include_str!("../../aterm-phase/src/fixtures/claude-2.1.280-box-rm.txt");
const SURVEY: &str = include_str!("../../aterm-phase/src/fixtures/survey-open.txt");
const CARD_LONG: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-survey-long-context.txt");
const CARD_PLUGIN: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-survey-plugin.txt");
const CARD_MEMORY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-survey-memory-cited.txt");
const CARD_BUSY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-survey-session-busy-workflow.txt");
const CARD_SPINNER: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.284-survey-long-context-busy-spinner.txt"
);
const DRAFTS_BUSY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-drafts-card-busy-workflow.txt");
/// The prompts that are not ratings and that nothing presses: the transcript-share ask
/// that follows a rating, and the web-setup offer, each over a worker that waits on a
/// workflow.
const ASK_BUSY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-share-ask-busy-workflow.txt");
const WEB_SETUP_BUSY: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.284-web-setup-busy-workflow.txt");
const SPINNER: &str = include_str!("../../aterm-phase/src/fixtures/spinner-tempering.txt");
const OFFER: &str = include_str!("../../aterm-phase/src/fixtures/hand-built-offer-end-of-turn.txt");
/// The owner's box (2026-09-24, live): a workflow subagent's rm-breaker box.
const OWNERS_BOX: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.28x-box-rm-workflow.txt");
/// The owner's measured weekly wall behind a background shell (tab 1, 12:19,
/// 2026-09-28): its reset an hour off here, so nothing acts on it meanwhile.
const WALL_SHELL: &str = include_str!(
    "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
);
const QUESTION: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-yes-no.txt");
const Q_TAB1: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-tabs-incident.txt");
const Q_TAB2: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-tabs-second.txt");
const Q_TAB3: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-tabs-third.txt");
const Q_TAB4: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-tabs-fourth.txt");
const Q_REVIEW: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-review.txt");
const Q_ONE: &str = include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-one.txt");
const Q_FREE_FOCUSED: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-free-text-focused.txt");
const Q_TYPED: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.282-question-free-text-typed.txt");
/// 2.1.283 mid-turn (the spinner over the composer): the worker taking an
/// answer.
const BUSY_283: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.283-footer-busy.txt");
/// 2.1.283 idle after a turn (`✻ Worked for 2s · done …`), its last words
/// plain — no question, no choice — after a turn far shorter than the
/// continue policy's `min_work`: a point the turn-end policy leaves alone,
/// and the one at which a dialog the loop answered is told (`CHOSE`).
const AFTER_TURN_283: &str =
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.283-footer-after-turn.txt");

/// The width 2.1.283's question captures were measured at (`120x44`).
const CAPTURE_COLS: usize = 120;
/// The most rows of a screen the fake draws: a 2.1.283 capture is 44 rows,
/// and `cat` would scroll the 40-row pane drawing it whole. Its foot — where
/// the dialog is — is kept.
const DRAWN_ROWS: usize = 38;
/// What 2.1.283's footer adds while the focus is on a free-text row.
const VIM_HINT: &str = " · ctrl+g to edit in Vim";
/// The widest row of the review's quoted answer (c3-07, MEASURED: its rows
/// are 116 and 117 cells, and the next word would have made 121 and 127; any
/// limit from 117 to 120 redraws it exactly — pinned in
/// [`the_2_1_283_scenes_redraw_what_was_measured`]).
const REVIEW_ROW: usize = 119;

fn is_rule(row: &str) -> bool {
    !row.is_empty() && row.chars().all(|c| c == '─')
}

/// `rows` as the fake draws them: its trailing blank rows dropped (the fake
/// cleared the pane: nothing changes) and at most [`DRAWN_ROWS`] from its
/// foot.
fn drawn(rows: &[String]) -> String {
    let end = rows
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .map_or(0, |i| i + 1);
    rows[end.saturating_sub(DRAWN_ROWS)..end].join("\n") + "\n"
}

/// The dialog's top rule in a 2.1.283 select tab: the last rule but one
/// (the last is its divider over `Chat about this`).
fn dialog_top(rows: &[String]) -> usize {
    let rules: Vec<usize> = (0..rows.len()).filter(|&i| is_rule(&rows[i])).collect();
    let [.., top, _divider] = rules[..] else {
        panic!("a select tab has its top rule and its divider")
    };
    top
}

/// The 2.1.283 capture `capture` with `text` typed into its free-text row,
/// under the transcript it shows above the dialog: the dialog redrawn by
/// aterm-phase's `rewrap_free_text`, which wraps a text as 2.1.283 was
/// measured to (it pins itself to every measured wrap).
fn typed_283(capture: &str, text: &str) -> Vec<String> {
    let rows = screen(capture);
    let mut out = rows[..dialog_top(&rows)].to_vec();
    out.extend(rewrap_free_text(capture, text, CAPTURE_COLS));
    out
}

/// `rows` — a 2.1.283 tab with its focus on its free-text row — with the
/// focus on the option `to` instead, as 2.1.283 draws that: the `❯` moved,
/// and the footer without [`VIM_HINT`], which it draws only while the
/// free-text row has the focus (MEASURED: c1-01 and c1-02 differ from c1-03
/// in those two rows alone, as c3-01 from c3-04 does, and c4-01, c4-02,
/// c4-05, c4-06 and c4-07 from their tab's free-text capture — the last
/// five pinned in [`the_2_1_283_scenes_redraw_what_was_measured`]).
fn focus_on_option(rows: &[String], to: &str) -> Vec<String> {
    focus_moved(&(rows.join("\n") + "\n"), to)
        .replace(VIM_HINT, "")
        .lines()
        .map(str::to_string)
        .collect()
}

/// `rows` — a single-select tab with text in its focused free-text row —
/// with that row pristine again, one row: `❯ N. Type something.` (c1-03's
/// row; pinned against c4-03 from c4-04).
fn pristine_283(rows: &[String]) -> Vec<String> {
    let divider = rows.iter().rposition(|r| is_rule(r)).expect("the divider");
    let field = rows[..divider]
        .iter()
        .rposition(|r| r.starts_with("❯ "))
        .expect("the focused free-text row");
    let n: String = rows[field]["❯ ".len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    assert!(!n.is_empty(), "not a numbered row: {:?}", rows[field]);
    let mut out = rows[..field].to_vec();
    out.push(format!("❯ {n}. Type something."));
    out.extend_from_slice(&rows[divider..]);
    out
}

/// The review tab `capture` (one answer) with its answer's quoted text
/// `text`, wrapped as 2.1.283 wrapped it (c3-07): `   → "` then rows at
/// column 3, greedily at single spaces, no row past [`REVIEW_ROW`] cells.
fn review_283(capture: &str, text: &str) -> Vec<String> {
    let rows = screen(capture);
    let first = rows
        .iter()
        .position(|r| r.starts_with("   → \""))
        .expect("the review's answer row");
    let last = (first..rows.len())
        .find(|&i| rows[i].ends_with('"'))
        .expect("the answer's closing quote");
    let mut out = rows[..first].to_vec();
    let mut line = "   → ".to_string();
    let mut empty = true;
    for word in format!("\"{text}\"").split(' ') {
        if !empty && line.chars().count() + 1 + word.chars().count() > REVIEW_ROW {
            out.push(std::mem::replace(&mut line, "   ".to_string()));
            empty = true;
        }
        if !empty {
            line.push(' ');
        }
        line.push_str(word);
        empty = false;
    }
    out.push(line);
    out.extend_from_slice(&rows[last + 1..]);
    out
}

/// The key log's line for priority `p`'s policy text read whole.
fn text_read(p: QuestionPriority) -> String {
    format!("text:{}", p.text().expect("a policy text"))
}

/// A session just started in bypass mode: the measured 2.1.280 banner over
/// an empty composer and the `⏵⏵ bypass permissions on` footer (the offer
/// fixture's last four rows). Idle with no turn behind it, so nothing is
/// owed to it — and the footer is read at the supervisor's first look.
/// (A busy screen is not enough: the loop waits a busy turn out without
/// re-reading it, so the footer it carried is never seen.)
fn bypass_start() -> String {
    let banner: Vec<String> = body(BOX_TOUCH)
        .lines()
        .take(4)
        .map(str::to_string)
        .collect();
    let offer = body(OFFER);
    let composer: Vec<&str> = offer.lines().rev().take(4).collect();
    let mut rows = banner;
    rows.push(String::new());
    rows.extend(composer.into_iter().rev().map(str::to_string));
    rows.join("\n") + "\n"
}

/// The measured rm-breaker box over `cmd` instead of the loop it was
/// measured with (which escalates by construction: `set` in a loop).
fn rm_box(cmd: &str) -> String {
    body(BOX_RM)
        .replace(
            "S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done",
            cmd,
        )
        .replace(
            "Removing directories tmp/a and tmp/b",
            "Removing the directory",
        )
        .replace("Remove directories tmp/a and tmp/b", "Remove the directory")
        .replace("$S/$1 in `rm -rf $S/$1`", "$S/a in `rm -rf $S/a`")
}

/// A workflow's Bash box whose heredoc has `lines` rows (aterm-phase's
/// hand-built `tall_bash_box` shape, from the measured incident box): with
/// 33, the box is 43 rows — whole on a 49-row screen, its title more than 40
/// rows above its footer.
fn tall_box(lines: usize) -> String {
    let mut r = vec![
        "⏺ Writing the fixture file.".to_string(),
        String::new(),
        "─".repeat(120),
        " Bash command · from the \"fixtures\" workflow".to_string(),
        String::new(),
        "   │ cat > notes.txt <<'EOF'".to_string(),
    ];
    r.extend((1..lines).map(|i| format!("   │ line {i} of the heredoc")));
    for row in [
        "   Write the notes file",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ] {
        r.push(row.to_string());
    }
    r.join("\n")
}

/// `screen` with the `❯` focus moved from the row it is on onto the row
/// that reads `to` (both drawn at column 2, as 2.1.282 draws its options).
fn focus_moved(screen: &str, to: &str) -> String {
    let from = screen
        .lines()
        .find(|r| {
            r.strip_prefix("❯ ")
                .is_some_and(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        })
        .expect("a focused option row")
        .to_string();
    let target = format!("  {to}");
    assert!(screen.lines().any(|r| r == target), "no row {target:?}");
    screen
        .replace(&from, &format!("  {}", &from["❯ ".len()..]))
        .replace(&target, &format!("❯ {to}"))
}

/// `screen` from its dialog's top rule down: the dialog drawn at the top of
/// the pane.
fn from_the_rule(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().collect();
    let rule = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the dialog's rule");
    rows[rule..].join("\n") + "\n"
}

/// Where the fake `claude` lives: the scratch HOME's managed atpkg prefix
/// (`atpkg::store::default_prefix`), at the twin's own path `agents/claude` —
/// the only agent-named script the server reads as the agent before it
/// execs (a `/bin/sh` script named `claude` anywhere else is a shell).
fn fake_path(tmp: &Path) -> PathBuf {
    atpkg::store::default_prefix(&tmp.join("home")).join("agents/claude")
}

/// Put the fake `text` at `fake`, the system's first-exec check of it already
/// paid ([`headless_boot::standin`]: one file per text, run once, linked into
/// place). The host's version probe of the twin is bounded at 5 s
/// (`aterm_agent::harness::upgrade_drive`'s `VERSION_WAIT`): a probe that
/// paid the check read as a build that did not answer, and the attach it
/// runs in and the upgrade it decides waited on it. That time is the file's,
/// not the product's. Every fake here answers `--version` at once, or ends
/// at once with nothing to draw, and holds no path: it finds its scratch
/// world from `$HOME` (`<root>/home`).
///
/// A fake that REPLACES one at a path is put here under its own name, before
/// the rename ([`stage_upgrade_fake`]): the check stays with the file through
/// the rename, so the path never names a file the system has not checked.
/// Renamed in unchecked, it held up more than the probe: a `/bin/sh` script
/// still running from that path stopped at its next exec until the check of
/// its replacement ended (measured 2026-10-04: 5.2 s to 8.5 s at a load of
/// 15 to 50, with nothing else running the new file) — the upgrade fake's
/// own start-up, before its first frame.
fn place_fake(fake: &Path, text: &str) {
    headless_boot::standin(fake, text);
}

/// Write the fake `claude` and every screen under `tmp`.
fn write_world(tmp: &Path) {
    let claude = fake_path(tmp);
    let bin = claude.parent().expect("agents/").to_path_buf();
    let sc = tmp.join("sc");
    for d in [&bin, &sc, &tmp.join("work")] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    let fake = format!(
        "#!/bin/sh\n\
         # A fake Claude Code: draws each screen file given, reads ONE key after\n\
         # each into the key log ($1) -- one byte, or ESC and the two after it --\n\
         # as `od -c` spells it; a *.hold screen is shown and nothing read, a\n\
         # *.late one is a wait with nothing drawn, and after a *.type screen ONE\n\
         # bounded read takes what was written, logged text:.\n\
         log=$1; shift\n\
         old=$(stty -g)\n\
         stty -icanon -echo min 1\n\
         for scene in \"$@\"; do\n\
         \x20 case \"$scene\" in *.late) sleep {LATE_S}; continue ;; esac\n\
         \x20 printf '\\033[2J\\033[H'\n\
         \x20 cat \"$scene\"\n\
         \x20 r=$(grep -n '^❯' \"$scene\" | tail -n 1 | cut -d: -f1); [ -n \"$r\" ] && printf '\\033[%s;3H' \"$r\"\n\
         \x20 case \"$scene\" in\n\
         \x20 *.hold) sleep {HOLD_S}; continue ;;\n\
         \x20 *.type) t=$(dd bs=4096 count=1 2>/dev/null); printf 'text:%s\\n' \"$t\" >> \"$log\"; continue ;;\n\
         \x20 esac\n\
         \x20 k=$(dd bs=1 count=1 2>/dev/null | od -An -c | tr -d ' ')\n\
         \x20 case \"$k\" in 033) k=\"$k$(dd bs=1 count=2 2>/dev/null | od -An -c | tr -d ' ')\" ;; esac\n\
         \x20 printf '%s\\n' \"$k\" >> \"$log\"\n\
         done\n\
         stty \"$old\"\n\
         printf '\\033[2J\\033[H'\n"
    );
    place_fake(&claude, &fake);
    // The measured touch box draws its third option garbled (`3. Nooject`,
    // aterm-phase's `the_touch_box_is_bash_whatever_its_description_says`),
    // and decision 1's rules press a one-shot `Yes` only on a box that also
    // offers a readable `No` (`once_choice`): the read box repairs that row,
    // so what it tests is the read-only rule, not the garble. (`touch.txt`
    // keeps it: full power presses it as measured, and the safe rules
    // escalate it either way.)
    let read = body(BOX_TOUCH)
        .replace("touch x", "ls -la")
        .replace("Create file x", "List the files")
        .replace("3. Nooject", "3. No");
    // The measured survey held a draft in the composer, and a supervisor
    // never presses over a draft: the survey with the composer empty.
    let survey = body(SURVEY).replace(
        "❯ Fix the parser path break yourself, same rules as the last one",
        "❯ ",
    );
    let screens = [
        ("read.txt", read),
        ("touch.txt", body(BOX_TOUCH)),
        ("question.txt", body(QUESTION)),
        ("survey.txt", survey),
        ("card-long.txt", body(CARD_LONG)),
        ("card-plugin.txt", body(CARD_PLUGIN)),
        ("card-memory.txt", body(CARD_MEMORY)),
        ("card-busy.txt", body(CARD_BUSY)),
        ("card-spinner.txt", body(CARD_SPINNER)),
        ("drafts-busy.txt", body(DRAFTS_BUSY)),
        ("ask-busy.txt", body(ASK_BUSY)),
        ("web-setup-busy.txt", body(WEB_SETUP_BUSY)),
        ("busy.hold", body(SPINNER)),
        (
            "wall.hold",
            body(WALL_SHELL).replace("resets 4pm (America/Los_Angeles)", "resets in 1h"),
        ),
        ("bypass.hold", bypass_start()),
        ("rm-scratch.txt", rm_box("S=/tmp/athh-scratch; rm -rf $S/a")),
        ("rm-usr.txt", rm_box("S=/usr; rm -rf $S/a")),
        ("owners-box.txt", body(OWNERS_BOX)),
        ("tall.txt", tall_box(33)),
        ("q-tab1.txt", body(Q_TAB1)),
        ("q-tab2.txt", body(Q_TAB2)),
        ("q-tab3.txt", body(Q_TAB3)),
        ("q-tab4.txt", body(Q_TAB4)),
        ("q-review.txt", body(Q_REVIEW)),
        ("q-one.txt", body(Q_ONE)),
        ("q-one-top.txt", from_the_rule(&body(Q_ONE))),
        ("q-free-focused.txt", body(Q_FREE_FOCUSED)),
        (
            "q-free-focus3.txt",
            focus_moved(&body(Q_FREE_FOCUSED), "3. High contrast"),
        ),
        (
            "q-free-focus2.txt",
            focus_moved(&body(Q_FREE_FOCUSED), "2. Light"),
        ),
        ("q-typed.txt", body(Q_TYPED)),
    ];
    for (name, text) in screens {
        std::fs::write(sc.join(name), text).expect("write a screen");
    }
    // 2.1.283 under the question policy: the committed captures (c1-03 and
    // c1-04 of the single-select, c3-04 … c3-07 of the multi-select, c8-02 of
    // the short-term case, c2-04 of a person's text), each dialog redrawn
    // with the text the harness types where one was typed, and the moves
    // before the free-text row drawn from its capture as measured.
    let long = QuestionPriority::LongTerm
        .text()
        .expect("the long-term text");
    let short = QuestionPriority::ShortTerm
        .text()
        .expect("the short-term text");
    let c1_field = screen(QUESTION_283_FREE_TEXT_FOCUSED);
    let c3_field = screen(QUESTION_283_MULTISELECT_FREE_TEXT_FOCUSED);
    let c8_field = pristine_283(&screen(QUESTION_283_TYPED_SHORT_TERM));
    let screens_283 = [
        (
            "c1-opt1.txt",
            focus_on_option(&c1_field, "1. Red (Recommended)"),
        ),
        ("c1-opt2.txt", focus_on_option(&c1_field, "2. Blue")),
        ("c1-field.type", c1_field.clone()),
        ("c1-long.txt", typed_283(QUESTION_283_TYPED_LONG_TERM, long)),
        (
            "c3-opt1.txt",
            focus_on_option(&c3_field, "1. [ ] Cheese (Recommended)"),
        ),
        ("c3-opt2.txt", focus_on_option(&c3_field, "2. [ ] Olives")),
        ("c3-opt3.txt", focus_on_option(&c3_field, "3. [ ] Basil")),
        ("c3-field.type", c3_field.clone()),
        (
            "c3-long.txt",
            typed_283(QUESTION_283_MULTISELECT_TYPED_LONG_TERM, long),
        ),
        (
            "c3-long-button.txt",
            typed_283(QUESTION_283_MULTISELECT_TYPED_BUTTON, long),
        ),
        (
            "c3-review.txt",
            review_283(QUESTION_283_MULTISELECT_REVIEW, long),
        ),
        (
            "c8-opt1.txt",
            focus_on_option(&c8_field, "1. Red (Recommended)"),
        ),
        ("c8-opt2.txt", focus_on_option(&c8_field, "2. Blue")),
        ("c8-field.type", c8_field.clone()),
        (
            "c8-short.txt",
            typed_283(QUESTION_283_TYPED_SHORT_TERM, short),
        ),
        ("c2-person.type", screen(QUESTION_283_TYPED_PERSON)),
        ("worked.hold", screen(BUSY_283)),
        ("done.hold", screen(AFTER_TURN_283)),
    ];
    for (name, rows) in screens_283 {
        std::fs::write(sc.join(name), drawn(&rows)).expect("write a screen");
    }
}

/// Start the fake on `screens` in the boot session, from `work/` with the
/// cwd reported as shell integration reports it (OSC 7), and wait until the
/// server names its program.
fn run_fake(inst: &Instance, keys: &str, screens: &[&str]) {
    let tmp = inst.tmp.display();
    let scenes: Vec<String> = screens.iter().map(|s| format!("{tmp}/sc/{s}")).collect();
    let line = format!(
        "clear; cd {tmp}/work; printf '\\033]7;file://localhost%s\\007' \"$PWD\"; \
         '{}' {tmp}/{keys} {}",
        fake_path(&inst.tmp).display(),
        scenes.join(" ")
    );
    let sel = format!("@{}", inst.sid);
    ctl_ok(inst, &[&sel, "send", &line]);
    ctl_ok(inst, &[&sel, "key", "enter"]);
}

/// The keys the fake read, one per line, named: `od -c`'s `\r` (or `\n`,
/// the tty's CR-to-NL) is `enter`, `033[A`/`033OA` (normal or application
/// cursor keys) `up`, `033[B`/`033OB` `down`; anything else — a `*.type`
/// read's `text:<bytes>` among it — as read. (An Enter written where a
/// `*.type` read takes it reads `text:`: the tty's CR-to-NL, and the shell
/// drops the newline.)
fn keys(inst: &Instance, log: &str) -> Vec<String> {
    std::fs::read_to_string(inst.tmp.join(log))
        .unwrap_or_default()
        .lines()
        .map(|k| {
            match k {
                "\\r" | "\\n" => "enter",
                "033[A" | "033OA" => "up",
                "033[B" | "033OB" => "down",
                other => other,
            }
            .to_string()
        })
        .collect()
}

/// The approvals ledger the host wrote for the boot session
/// (`<state>/drive/<sid>.jsonl`, under the scratch world).
fn ledger(inst: &Instance) -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|d| d == "drive")
                && p.extension().is_some_and(|x| x == "jsonl")
                && !p.to_string_lossy().ends_with(".journal.jsonl")
            {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&inst.tmp, &mut files);
    files
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The journal's `CHOSE` line for the dialog the host answered — told once
/// the worker has worked after it and reached its next point — or a panic
/// with the journal and the host's lines after `within`.
fn chose(inst: &Instance, within: Duration) -> String {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(line) = journal(inst).lines().find(|l| l.contains("CHOSE seq=")) {
            return line.to_string();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "no CHOSE within {within:?}; journal:\n{}\nharness log:\n{}",
        journal(inst),
        harness_lines(inst)
    );
}

/// The ledger's rows under `rule` with `"decision":"<decision>"`.
fn ledger_rows(inst: &Instance, rule: &str, decision: &str) -> Vec<String> {
    let rule = format!("\"rule_id\":\"{rule}\"");
    let decision = format!("\"decision\":\"{decision}\"");
    ledger(inst)
        .lines()
        .filter(|r| r.contains(&rule) && r.contains(&decision))
        .map(str::to_string)
        .collect()
}

fn keys_until(inst: &Instance, log: &str, want: &[&str], within: Duration) {
    // The attention rides along in the record, so a timeout says why the
    // host handed the box over instead of answering it. It is read once, at
    // the timeout: read at every 20 ms look it was an `aterm ctl meta` spawn
    // each (6 ms of CPU, measured 2026-10-05) for a value only a failure
    // prints, 50 a second for every test waiting on a key.
    let deadline = Instant::now() + within;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        last = keys(inst, log);
        if last == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let last = format!("{last:?} attention={:?}", attention(inst));
    panic!(
        "the fake read {want:?}: not within {within:?}; last: {last}\nharness log:\n{}",
        harness_lines(inst)
    );
}

/// Wait for the session's program to be `want` (the fake started or ended).
fn program_is(inst: &Instance, want: &str, within: Duration) -> String {
    until(
        within,
        &format!("program={want}"),
        || status(inst),
        |s| field(s, "program") == Some(want),
    )
}

/// The attention appears naming `needle`; then for [`QUIET`] nothing is
/// typed; the attention stands throughout.
fn escalated_and_quiet(inst: &Instance, log: &str, needle: &str) -> String {
    escalated_after(inst, log, needle, &[], HANG)
}

/// The attention appears naming `needle` within `within`, the fake having
/// read no more than `typed` meanwhile (a key past it fails at once, naming
/// what was read); then for [`QUIET`] the fake has read exactly `typed` and
/// nothing more; the attention stands throughout.
fn escalated_after(
    inst: &Instance,
    log: &str,
    needle: &str,
    typed: &[&str],
    within: Duration,
) -> String {
    let deadline = Instant::now() + within;
    let text = loop {
        let got = keys(inst, log);
        let within_typed = got.len() <= typed.len()
            && got
                .iter()
                .map(String::as_str)
                .eq(typed[..got.len()].iter().copied());
        assert!(
            within_typed,
            "the fake read {got:?}, past {typed:?}, before an attention naming {needle:?}\n\
             harness log:\n{}",
            harness_lines(inst)
        );
        let now = attention(inst).unwrap_or_default();
        if now.contains(needle) {
            break now;
        }
        assert!(
            Instant::now() < deadline,
            "an attention naming {needle:?}: not within {within:?}; last: {now:?}; the fake read \
             {got:?}\nharness log:\n{}",
            harness_lines(inst)
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let quiet_until = Instant::now() + QUIET;
    while Instant::now() < quiet_until {
        assert_eq!(keys(inst, log), typed, "typed after the hand-over");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        attention(inst).is_some_and(|a| a.contains(needle)),
        "the attention stands"
    );
    text
}

/// THE OWNER'S DEFAULT (2026-09-24, and 2026-09-25 for questions): with no
/// `[harness]` key written, the host answers every box — the rm circuit
/// breaker over `S=/usr` in a bypass session, which the safe rules hand over
/// ([`the_host_hands_over_what_it_cannot_prove_and_types_nothing`]), and the
/// measured `touch x` box as captured, its garbled `3. Nooject` and all, get
/// their one-shot `Yes` — answers a live-geometry question (S1-01) with Enter
/// on its recommended option, never a digit, and hands the column-1 question
/// (a shape 2.1.282 does not draw) to the human with nothing typed: a
/// question aterm-phase did not read whole is never answered, and its
/// `Yes`/`No` is the question's, never a confirmation.
#[test]
fn the_host_answers_every_box_by_default() {
    let Some(inst) = boot("d", "") else {
        return;
    };
    run_fake(&inst, "k1", &["bypass.hold", "rm-usr.txt"]);
    keys_until(&inst, "k1", &["1"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k2", &["touch.txt"]);
    keys_until(&inst, "k2", &["1"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k3", &["q-one.txt"]);
    keys_until(&inst, "k3", &["enter"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k4", &["question.txt"]);
    let text = escalated_and_quiet(&inst, "k4", "claude question");
    assert!(text.contains("could not read whole"), "{text}");
}

/// THE OWNER RULING OF 2026-09-25, end to end in a real headless aterm:
/// only an explicit setting stops the harness answering. A `[harness]` table
/// carrying a misspelled key, a key a newer aterm might add, and `approve` and
/// `enabled` values no reading makes sense of — the last two of which were
/// read as their limits before the ruling, `approve = none` and the harness
/// off — still gets `1` on the rm circuit breaker over `S=/usr` in a bypass
/// session. NEGATIVE CONTROL: the same box under an explicit off spelled
/// `approve = "no"` is handed over with nothing typed.
///
/// The two instances share nothing (each its own scratch world, socket and
/// fake), so they run side by side: one after the other they were 18-21 s of
/// two 9 s halves (measured 2026-10-05).
#[test]
fn a_typo_in_the_harness_table_never_stops_the_answers() {
    std::thread::scope(|scope| {
        let typo = scope.spawn(|| {
            let noisy = "approve = \"maybe\"\nenabled = \"sometimes\"\nrm_breakr = false\n\
                         some_future_key = true\n";
            let Some(inst) = boot("t", noisy) else {
                return;
            };
            run_fake(&inst, "k1", &["bypass.hold", "rm-usr.txt"]);
            keys_until(&inst, "k1", &["1"], HANG);
            program_is(&inst, "sh", HANG);
        });
        let off = scope.spawn(|| {
            let Some(inst) = boot("u", "approve = \"no\"\n") else {
                return;
            };
            run_fake(&inst, "k1", &["bypass.hold", "rm-usr.txt"]);
            escalated_and_quiet(&inst, "k1", "S=/usr; rm -rf $S/a");
        });
        typo.join().expect("the typo's run");
        off.join().expect("the explicit off's run");
    });
}

/// ON A SCREEN TALLER THAN THE 40 ROWS THE WINDOW READS (the validate
/// drive of 2026-09-24, a private headless aterm at 130x49): the measured
/// Yes/No question drawn at the top of a 49-row screen reached the reader
/// cut at `3. Type something.`, and the parser's backwards body slice
/// panicked the window's main thread — the whole instance died — and a
/// 43-row Bash box whose title sat on row 3 was read from the supervisor's
/// last 40 rows as "taller than the pane" and escalated. Now a live-geometry
/// question drawn at the top of the 49 rows (S1-01 from its rule down: the
/// last 40 rows start inside its options) is read again whole and answered
/// with Enter, the column-1 one is handed over as a question with nothing
/// typed and the instance lives, and the tall box, read again whole, gets
/// `1`.
#[test]
fn a_49_row_screen_is_read_whole_and_the_instance_lives() {
    let Some(mut inst) = boot_sized("w", "", 49, None) else {
        return;
    };
    run_fake(&inst, "k0", &["q-one-top.txt"]);
    keys_until(&inst, "k0", &["enter"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k1", &["question.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "claude question");
    assert!(text.contains("could not read whole"), "{text}");
    assert!(
        matches!(inst.child.try_wait(), Ok(None)),
        "the instance died; log tail:\n{}",
        log_tail(&inst.log)
    );
    // The handed-over question is the person's: their key ends the fake.
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "z"]);
    keys_until(&inst, "k1", &["z"], HANG);
    program_is(&inst, "sh", HANG);

    run_fake(&inst, "k2", &["tall.txt"]);
    keys_until(&inst, "k2", &["1"], HANG);
}

/// THE INCIDENT OF 2026-09-25, ANSWERED LIVE: the four-tab dialog (S6-01 …
/// S6-05) gets Enter on each tab's `(Recommended)` option and on the
/// review's `Submit answers` — five Enters, never a digit — with no badge,
/// and the host's ledger holds five `answer-recommended@v1` approvals.
#[test]
fn the_host_answers_a_tabbed_question_with_its_recommended_options() {
    let Some(inst) = boot("q", "") else {
        return;
    };
    run_fake(
        &inst,
        "k1",
        &[
            "q-tab1.txt",
            "q-tab2.txt",
            "q-tab3.txt",
            "q-tab4.txt",
            "q-review.txt",
        ],
    );
    keys_until(
        &inst,
        "k1",
        &["enter", "enter", "enter", "enter", "enter"],
        HANG,
    );
    program_is(&inst, "sh", HANG);
    assert_eq!(attention(&inst), None, "{}", harness_lines(&inst));
    let rows = ledger(&inst);
    let answered = rows
        .lines()
        .filter(|r| {
            r.contains("\"rule_id\":\"answer-recommended@v1\"")
                && r.contains("\"decision\":\"approved\"")
        })
        .count();
    assert_eq!(
        answered,
        5,
        "{rows}\nharness log:\n{}",
        harness_lines(&inst)
    );
}

/// The free-text row focused (S1-03): the host moves the focus UP one row a
/// key — each move seen landing on the next screen — and presses Enter only
/// once it is on `1. Dark (Recommended)`. Never a digit, never Enter on the
/// free-text row.
#[test]
fn the_host_moves_off_the_free_text_row_before_answering() {
    let Some(inst) = boot("f", "") else {
        return;
    };
    run_fake(
        &inst,
        "k1",
        &[
            "q-free-focused.txt",
            "q-free-focus3.txt",
            "q-free-focus2.txt",
            "q-one.txt",
        ],
    );
    keys_until(&inst, "k1", &["up", "up", "up", "enter"], HANG);
}

/// A person's text in the free-text row (S1-04): the question is theirs —
/// badged with the reason, nothing typed.
#[test]
fn the_host_hands_over_a_question_a_person_typed_into() {
    let Some(inst) = boot("p", "") else {
        return;
    };
    run_fake(&inst, "k1", &["q-typed.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "claude question");
    assert!(text.contains("a person has begun answering"), "{text}");
}

/// The owner's switch: under `answer_questions = false` every question is
/// the human's — the incident's first tab is badged naming the switch, and
/// nothing is typed.
#[test]
fn answer_questions_false_hands_every_question_over() {
    let Some(inst) = boot("x", "answer_questions = false\n") else {
        return;
    };
    run_fake(&inst, "k1", &["q-tab1.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "claude question");
    assert!(text.contains("answer_questions is off"), "{text}");
}

/// The owner's long-term word, in the one global key.
const LONG_TERM_ON: &str = "question_priority = \"long-term\"\n";

/// The dialog is answered, told and gone: `CHOSE` names `rule` and `word`
/// and the question `decided by the worker (<word>)`; the fake exits (every
/// scene read); the ledger holds `approvals` approvals under `rule`, one of
/// them the text typed; nothing is badged.
fn told_by_the_worker(inst: &Instance, rule: &str, word: &str, question: &str, approvals: usize) {
    let line = chose(inst, HANG);
    assert!(
        line.contains(&format!("rule={rule} policy={word} ")),
        "{line}"
    );
    assert!(
        line.contains(&format!("{question} → decided by the worker ({word})")),
        "{line}"
    );
    program_is(inst, "sh", HANG);
    assert_eq!(attention(inst), None, "{}", harness_lines(inst));
    let approved = ledger_rows(inst, rule, "approved");
    assert_eq!(
        approved.len(),
        approvals,
        "{approved:#?}\nledger:\n{}\nharness log:\n{}",
        ledger(inst),
        harness_lines(inst)
    );
    assert_eq!(
        approved
            .iter()
            .filter(|r| r.contains(&format!("typed the {word} policy text")))
            .count(),
        1,
        "{approved:#?}"
    );
}

/// UNDER `question_priority = "long-term"`, A SINGLE-SELECT QUESTION (c1):
/// the host moves the focus from option 1 down to the pristine free-text row
/// (each move seen landing), writes the long-term text there in ONE `send` —
/// the fake reads it whole — reads it back and presses Enter on it: `[down,
/// down, text, enter]`, never a digit; two `answer-long-term@v1` approvals
/// (the text, the Enter) and `CHOSE … policy=long-term`.
#[test]
fn under_long_term_the_worker_decides_a_single_select_question() {
    let Some(inst) = boot("l", LONG_TERM_ON) else {
        return;
    };
    run_fake(
        &inst,
        "k1",
        &[
            "c1-opt1.txt",
            "c1-opt2.txt",
            "c1-field.type",
            "c1-long.txt",
            "worked.hold",
            "done.hold",
        ],
    );
    let text = text_read(QuestionPriority::LongTerm);
    keys_until(&inst, "k1", &["down", "down", &text, "enter"], HANG);
    told_by_the_worker(
        &inst,
        "answer-long-term@v1",
        "long-term",
        "Which colour do you prefer?",
        2,
    );
}

/// UNDER `question_priority = "long-term"`, A MULTI-SELECT QUESTION (c3): the
/// text written into its free-text row (which ticks itself), then `down` to
/// `Submit` — NEVER an Enter while the focus is in the field, which unticks
/// it (c3c), and never a Tab — Enter there, and Enter on the review's
/// `Submit answers`: `[down×3, text, down, enter, enter]`; three
/// `answer-long-term@v1` approvals (the text, the button, the review).
#[test]
fn under_long_term_the_worker_decides_a_multi_select_question() {
    let Some(inst) = boot("m", LONG_TERM_ON) else {
        return;
    };
    run_fake(
        &inst,
        "k1",
        &[
            "c3-opt1.txt",
            "c3-opt2.txt",
            "c3-opt3.txt",
            "c3-field.type",
            "c3-long.txt",
            "c3-long-button.txt",
            "c3-review.txt",
            "worked.hold",
            "done.hold",
        ],
    );
    let text = text_read(QuestionPriority::LongTerm);
    keys_until(
        &inst,
        "k1",
        &["down", "down", "down", &text, "down", "enter", "enter"],
        HANG,
    );
    told_by_the_worker(
        &inst,
        "answer-long-term@v1",
        "long-term",
        "Which toppings do you want?",
        3,
    );
}

/// THE SESSION'S WORD OVER THE GLOBAL ONE (c8): the config answers with the
/// recommended option, and this session says `short-term`
/// (`meta set questions short-term`): the short-term text is written and
/// Enter pressed on it — `[down, down, text, enter]` — under
/// `answer-short-term@v1`, told `policy=short-term`. (Unset, the same dialog
/// gets Enter on `Red (Recommended)`:
/// [`the_host_answers_every_box_by_default`] is that path.)
#[test]
fn a_session_s_short_term_word_decides_over_the_global_recommended() {
    let Some(inst) = boot("s", "") else {
        return;
    };
    ctl_ok(
        &inst,
        &[
            &format!("@{}", inst.sid),
            "meta",
            "set",
            "questions",
            "short-term",
        ],
    );
    run_fake(
        &inst,
        "k1",
        &[
            "c8-opt1.txt",
            "c8-opt2.txt",
            "c8-field.type",
            "c8-short.txt",
            "worked.hold",
            "done.hold",
        ],
    );
    let text = text_read(QuestionPriority::ShortTerm);
    keys_until(&inst, "k1", &["down", "down", &text, "enter"], HANG);
    told_by_the_worker(
        &inst,
        "answer-short-term@v1",
        "short-term",
        "Which colour should I use to paint the fence before tomorrow's garden party?",
        2,
    );
}

/// A PERSON'S TEXT IN THE FIELD (c2-04, `go with blue`) under long-term: it
/// is not the loop's own, so the question is theirs — badged with the reason,
/// and nothing written (the scene is a `*.type` one: a text or a key written
/// into it would be logged whole).
#[test]
fn under_long_term_a_person_s_text_is_handed_to_them_and_nothing_is_typed() {
    let Some(inst) = boot("h", LONG_TERM_ON) else {
        return;
    };
    run_fake(&inst, "k1", &["c2-person.type"]);
    let text = escalated_and_quiet(&inst, "k1", "claude question");
    assert!(
        text.contains("a person has begun answering")
            && text.contains("the supervisor did not write"),
        "{text}"
    );
}

/// NEGATIVE CONTROL: the same single-select dialog under long-term, but the
/// dialog never takes the text — after the `send` the field still reads
/// `Type something.`. The host writes that ONE text and nothing more (the
/// scene after it is a `*.type` one too: a second text or an Enter would be
/// logged), and hands the question over: `[down, down, text]`, and no
/// approval. So the Enter of
/// [`under_long_term_the_worker_decides_a_single_select_question`] is its
/// read-back's, not the fake's script's.
#[test]
fn a_policy_text_the_dialog_does_not_take_is_written_once_and_handed_over() {
    let Some(inst) = boot("c", LONG_TERM_ON) else {
        return;
    };
    run_fake(
        &inst,
        "k1",
        &[
            "c1-opt1.txt",
            "c1-opt2.txt",
            "c1-field.type",
            "c1-field.type",
        ],
    );
    let text = text_read(QuestionPriority::LongTerm);
    let said = escalated_after(
        &inst,
        "k1",
        "never written twice",
        &["down", "down", &text],
        HANG,
    );
    assert!(said.contains("did not take the policy text"), "{said}");
    assert_eq!(
        ledger_rows(&inst, "answer-long-term@v1", "approved"),
        Vec::<String>::new(),
        "no approval for a text never read back"
    );
    assert_eq!(
        ledger_rows(&inst, "answer-long-term@v1", "escalated").len(),
        1,
        "{}",
        ledger(&inst)
    );
}

/// The 2.1.283 scenes redraw what was MEASURED: the helpers that draw them,
/// applied to the committed captures with the text that was typed there,
/// give the captures back row for row — the focus moved off a free-text row
/// (c4-03 → c4-01, c4-02; c4-08 → c4-05, c4-06, c4-07), a typed field made
/// pristine (c4-04 → c4-03, from the dialog's rule down: the pane is
/// anchored at its foot, so the transcript above moves with the dialog's
/// height), a typed field (c1-04, c3-05, c3-06, c8-02) and the review's
/// answer (c3-07). What the live scenes add is the text the harness types
/// in place of the measured one.
#[test]
fn the_2_1_283_scenes_redraw_what_was_measured() {
    use aterm_agent::supervise::prompt::fixtures::{
        MEASURED_LONG_TERM_2_1_283, MEASURED_SHORT_TERM_2_1_283, QUESTION_283_TABS_FIRST,
        QUESTION_283_TABS_FIRST_DOWN, QUESTION_283_TABS_FIRST_FREE_TEXT_FOCUSED,
        QUESTION_283_TABS_FIRST_TYPED_LONG_TERM, QUESTION_283_TABS_SECOND,
        QUESTION_283_TABS_SECOND_DOWN, QUESTION_283_TABS_SECOND_DOWN_TWICE,
        QUESTION_283_TABS_SECOND_FREE_TEXT_FOCUSED,
    };
    let first_field = screen(QUESTION_283_TABS_FIRST_FREE_TEXT_FOCUSED);
    let second_field = screen(QUESTION_283_TABS_SECOND_FREE_TEXT_FOCUSED);
    for (field, to, want) in [
        (
            &first_field,
            "1. Red (Recommended)",
            QUESTION_283_TABS_FIRST,
        ),
        (&first_field, "2. Blue", QUESTION_283_TABS_FIRST_DOWN),
        (
            &second_field,
            "1. [ ] Cheese (Recommended)",
            QUESTION_283_TABS_SECOND,
        ),
        (
            &second_field,
            "2. [ ] Olives",
            QUESTION_283_TABS_SECOND_DOWN,
        ),
        (
            &second_field,
            "3. [ ] Basil",
            QUESTION_283_TABS_SECOND_DOWN_TWICE,
        ),
    ] {
        assert_eq!(focus_on_option(field, to), screen(want), "{to}");
    }
    let from_rule = |rows: &[String]| rows[dialog_top(rows)..].to_vec();
    assert_eq!(
        from_rule(&pristine_283(&screen(
            QUESTION_283_TABS_FIRST_TYPED_LONG_TERM
        ))),
        from_rule(&first_field)
    );
    for (capture, text) in [
        (QUESTION_283_TYPED_LONG_TERM, MEASURED_LONG_TERM_2_1_283),
        (
            QUESTION_283_MULTISELECT_TYPED_LONG_TERM,
            MEASURED_LONG_TERM_2_1_283,
        ),
        (
            QUESTION_283_MULTISELECT_TYPED_BUTTON,
            MEASURED_LONG_TERM_2_1_283,
        ),
        (QUESTION_283_TYPED_SHORT_TERM, MEASURED_SHORT_TERM_2_1_283),
    ] {
        assert_eq!(typed_283(capture, text), screen(capture));
    }
    assert_eq!(
        review_283(QUESTION_283_MULTISELECT_REVIEW, MEASURED_LONG_TERM_2_1_283),
        screen(QUESTION_283_MULTISELECT_REVIEW)
    );
    // And the scenes carry the harness's own texts, not the measured ones.
    let long = QuestionPriority::LongTerm.text().expect("long");
    assert_ne!(long, MEASURED_LONG_TERM_2_1_283);
    let redrawn = typed_283(QUESTION_283_TYPED_LONG_TERM, long).join(" ");
    assert!(
        redrawn.contains("best serves long term correctness"),
        "{redrawn}"
    );
    assert!(!redrawn.contains("long-term correctness"), "{redrawn}");
}

#[test]
fn the_host_answers_what_the_policy_proves_safe() {
    let Some(inst) = boot("a", "") else {
        return;
    };
    // A read-only Bash box: the session is published as Claude Code, the host
    // attaches and presses `1`. Both are the server's own timeline records,
    // read once the press has landed: `program=claude` and the host's claim
    // live only until the host answers and the fake exits, and sampling each
    // through an `aterm ctl status` spawn could miss both (76dff586e's shape).
    let mark = timeline_since(&inst, 0).last().map_or(0, |e| e.0);
    run_fake(&inst, "k1", &["read.txt"]);
    keys_until(&inst, "k1", &["1"], HANG);
    program_is(&inst, "sh", HANG);
    let events = timeline_since(&inst, mark);
    assert!(
        events.iter().any(|e| e.1 == "agent-change"),
        "never published as an agent: {events:?}"
    );
    assert!(
        events.iter().any(
            |e| e.1 == "meta-change" && e.2.starts_with("field=supervisor value=aterm-harness")
        ),
        "the host never claimed the session: {events:?}\n{}",
        harness_lines(&inst)
    );

    // The session survey, after a screen without it: dismissed with `0`.
    run_fake(&inst, "k2", &["busy.hold", "survey.txt"]);
    keys_until(&inst, "k2", &["0"], HANG);
    program_is(&inst, "sh", HANG);

    // The rm circuit breaker in a bypass session, every operand under a
    // scratch root: `1`.
    run_fake(&inst, "k3", &["bypass.hold", "rm-scratch.txt"]);
    keys_until(&inst, "k3", &["1"], HANG);
    program_is(&inst, "sh", HANG);
    // The host detached with the program: its claim went with it.
    until(
        RELEASED,
        "supervisor=- after the agent left",
        || status(&inst),
        |s| field(s, "supervisor") == Some("-"),
    );
    // The fake's exit was its own end (it registered no conversation):
    // journaled, never relaunched — the tab's shell holds it — and never
    // said on the attention, with the relaunch at its full-power default.
    // (An exit whose last frame the server still reads as the agent's is
    // handed over only once it stops reading so; the last one here is.) A
    // step that found another sweep on the upgrade state — the host's own
    // upgrade lane at the same session — is the designed retry
    // (`relaunch::BUSY`, ten seconds on), never an answer: so the wait is a
    // hang detector, and every attempt is that retry or the launch's end.
    let log = until(
        HANG,
        "the exit journaled as the launch's own end",
        || harness_lines(&inst),
        |log| log.contains("exited; not relaunched"),
    );
    for step in log.lines().filter_map(|l| l.split_once("relaunch pid=")) {
        assert!(
            step.1.contains("step=ended:") || step.1.contains("step=busy:another-sweep"),
            "{}",
            step.1
        );
    }
    assert_eq!(field(&status(&inst), "program"), Some("sh"));
    assert_eq!(attention(&inst), None, "nothing said");
}

/// EVERY KIND OF CLAUDE CODE RATING CARD IS DISMISSED with a `0`, and only that
/// (owner, 2026-09-28: "auto dismiss" the surveys): the long-context, plugin and
/// memory cards, and the session survey over a worker that READS BUSY (a dynamic
/// workflow it waits on) — which used to be seen and never pressed. Each is
/// drawn by the fake over the fixtures' hand-built 2.1.284 rows; the key log
/// shows exactly one `0` for each. The Claude-drafted-feedback card is no
/// survey: over a busy worker the server's verdict stays `busy` (the card used
/// to hide the status row, so the worker read idle and the host typed into it)
/// and nothing is pressed.
#[test]
fn the_host_dismisses_every_kind_of_rating_card_and_reads_a_busy_worker_busy_under_any_card() {
    // A tag of its own: two tests booting under one tag share a scratch world.
    let Some(inst) = boot("z", "") else {
        return;
    };
    for (log, screen) in [
        ("k1", "card-long.txt"),
        ("k2", "card-plugin.txt"),
        ("k3", "card-memory.txt"),
        ("k4", "card-busy.txt"),
    ] {
        run_fake(&inst, log, &[screen]);
        keys_until(&inst, log, &["0"], HANG);
        program_is(&inst, "sh", HANG);
    }
    // The drafts card, the transcript-share ask and the web-setup offer over a busy worker
    // (a workflow wait: no busy footer): the verdict stays busy, nothing is typed.
    for (log, screen, what) in [
        ("k5", "drafts-busy.txt", "the drafts card"),
        ("k6", "ask-busy.txt", "the transcript-share ask"),
        ("k7", "web-setup-busy.txt", "the web-setup offer"),
    ] {
        run_fake(&inst, log, &[screen]);
        until(
            HANG,
            &format!("agent=busy under {what}"),
            || status(&inst),
            |s| field(s, "agent") == Some("busy"),
        );
        let quiet_until = Instant::now() + QUIET;
        while Instant::now() < quiet_until {
            assert!(
                keys(&inst, log).is_empty(),
                "typed under {what}: {:?}",
                keys(&inst, log)
            );
            assert_eq!(field(&status(&inst), "agent"), Some("busy"), "{what}");
            std::thread::sleep(Duration::from_millis(100));
        }
        ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "z"]);
        keys_until(&inst, log, &["z"], HANG);
        program_is(&inst, "sh", HANG);
    }
}

/// Under the owner's `approve = "safe"` (full power, the default, answers
/// these boxes: aterm's `supervise_turn_end_live_headless.rs`).
///
/// THE FAKE STARTS LATE ([`LATE_S`]), so the supervisor's first idle point is
/// the blank screen before its first frame, and that frame — the footer that
/// says bypass — moves no verdict (idle to idle). The bypass proof below
/// holds only because a point read before any footer is waited on by its
/// content (`aterm_agent::supervise`'s `agent_wake`): a loop that waited on
/// the verdict slept through the footer and handed the box over as `the rm
/// circuit breaker outside a bypass session` — this test's red under a
/// loaded gate (2026-09-26/27), where the late start came from the load.
#[test]
fn the_host_hands_over_what_it_cannot_prove_and_types_nothing() {
    // `rm_breaker_ask = true`: since 4be9d668d (the owner's "informative and
    // concise warnings for the AI, not extra blockers", 2026-09-29) the
    // default approves an unproven rm breaker with a WARN line; the knob
    // restores the hand-over this test is about.
    let Some(inst) = boot("e", "approve = \"safe\"\nrm_breaker_ask = true\n") else {
        return;
    };
    // The same breaker over /usr: escalated, naming the command.
    run_fake(&inst, "k1", &["start.late", "bypass.hold", "rm-usr.txt"]);
    let text = escalated_and_quiet(&inst, "k1", "S=/usr; rm -rf $S/a");
    // Refused on WHERE it points — the scratch box of the first test was
    // answered under the same bypass start — not on the session's mode.
    assert!(
        text.contains("rm-breaker") && text.contains("scratch root"),
        "{text}"
    );
    // The human answers (this test's own key); the badge goes with the box.
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "z"]);
    keys_until(&inst, "k1", &["z"], HANG);
    program_is(&inst, "sh", HANG);
    until(
        HANG,
        "the badge cleared",
        || attention(&inst).unwrap_or_default(),
        str::is_empty,
    );

    // A `touch x` box (not read-only): escalated, nothing typed…
    run_fake(&inst, "k2", &["touch.txt"]);
    escalated_and_quiet(&inst, "k2", "touch x");
    // …and switching the harness off in the file stops the supervisor at the
    // reload: its claim and its badge go, and still nothing is typed.
    let cfg = inst.tmp.join("cfg/aterm/aterm.toml");
    let text = std::fs::read_to_string(&cfg).expect("config");
    std::fs::write(&cfg, format!("{text}[harness]\nenabled = false\n"))
        .expect("rewrite the config");
    until(
        RELEASED,
        "supervisor=- and no badge after enabled=false",
        || format!("{} attention={:?}", status(&inst), attention(&inst)),
        |s| field(s, "supervisor") == Some("-") && s.ends_with("attention=None"),
    );
    std::thread::sleep(QUIET);
    assert!(
        keys(&inst, "k2").is_empty(),
        "typed: {:?}",
        keys(&inst, "k2")
    );
}

/// THE SILENT LOOP, live (the 0.92.0 host's journal of 2026-09-24:
/// `ESCALATED seq=9246`, then nothing — its badge stood through the whole
/// workflow the owner's answer started, because the loop cleared it only at
/// the next idle point). Under `approve = "none"` (a limit the owner wrote:
/// every box is his) the owner's own box is escalated; a PERSON answers it
/// and the worker goes busy for ten seconds. The badge goes while the
/// worker is still busy. NEGATIVE CONTROL: before the answer the badge
/// stands and nothing is typed.
#[test]
fn a_box_a_person_answered_loses_its_badge_while_the_worker_works() {
    let Some(inst) = boot("b", "approve = \"none\"\n") else {
        return;
    };
    run_fake(&inst, "k1", &["owners-box.txt", "busy.hold", "busy.hold"]);
    escalated_and_quiet(&inst, "k1", "aterm-auto-target-x.noindex");
    // The clear and the worker's verdicts are the SERVER's own timeline
    // records, read once the clear has landed — as
    // `the_host_answers_what_the_policy_proves_safe` reads its attach; the
    // polls below only wait for effects.
    let mark = timeline_since(&inst, 0).last().map_or(0, |e| e.0);
    // The person's answer (this test's own key: the fake reads it as one).
    ctl_ok(&inst, &[&format!("@{}", inst.sid), "key", "1"]);
    keys_until(&inst, "k1", &["1"], HANG);
    until(
        HANG,
        "the badge cleared",
        || attention(&inst).unwrap_or_default(),
        str::is_empty,
    );
    let events = timeline_since(&inst, mark);
    let busy = events
        .iter()
        .find(|e| e.1 == "agent-change" && e.2.starts_with("busy "))
        .unwrap_or_else(|| panic!("the answer never set the worker working: {events:?}"));
    let cleared = events
        .iter()
        .find(|e| e.1 == "meta-change" && e.2 == "field=attention value=-")
        .unwrap_or_else(|| panic!("the badge's clear is not on the timeline: {events:?}"));
    // THE REGRESSION: the badge stood until the worker's next idle point. So
    // the clear precedes (in the timeline's own record order) the first verdict
    // after `busy` that is not `busy`, whenever that has come.
    if let Some(rested) = events
        .iter()
        .find(|e| e.1 == "agent-change" && e.0 > busy.0 && !e.2.starts_with("busy "))
    {
        assert!(
            cleared.0 < rested.0,
            "the badge cleared only when the worker stopped ({rested:?}): {events:?}"
        );
    }
    let journal = ["state", "home"]
        .iter()
        .find_map(|root| find_file(&inst.tmp.join(root), &format!("{}.journal.jsonl", inst.sid)))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    assert!(
        journal.contains("CLEARED") && journal.contains("box attention=OK"),
        "journaled: {journal}"
    );
}

#[test]
fn one_supervisor_per_session() {
    let Some(inst) = boot("o", "") else {
        return;
    };
    let sel = format!("@{}", inst.sid);
    // Hold the claim through launch and program detection — past the
    // [`HANG`] `program_is` may take. Those two control calls can be slow under
    // a full workspace gate; an 8-second claim started here could lapse before
    // the fake even begins, allowing the host to answer and exit before
    // `program_is` samples it.
    ctl_ok(
        &inst,
        &[
            &sel,
            "meta",
            "set",
            "supervisor",
            "someone-else",
            "ttl=120000",
        ],
    );
    run_fake(&inst, "k1", &["read.txt"]);
    program_is(&inst, "claude", HANG);
    // Start the lease being tested only after Claude is observably running.
    ctl_ok(
        &inst,
        &[
            &sel,
            "meta",
            "set",
            "supervisor",
            "someone-else",
            "ttl=10000",
        ],
    );
    let claimed = Instant::now();
    // While it holds: the host stays off, nothing is typed.
    while claimed.elapsed() < Duration::from_secs(4) {
        let s = status(&inst);
        assert_eq!(field(&s, "supervisor"), Some("someone-else"), "{s}");
        assert!(keys(&inst, "k1").is_empty(), "typed under another's claim");
        std::thread::sleep(Duration::from_millis(100));
    }
    // It lapses: the host takes the session over and answers the box.
    keys_until(&inst, "k1", &["1"], HANG);
    let s = status(&inst);
    assert!(
        field(&s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
        "{s}"
    );
}

/// NEGATIVE CONTROL: a headless instance whose `[harness]` says
/// `headless = false` runs no host — the box waits and nothing is typed.
#[test]
fn a_headless_instance_limited_by_its_config_supervises_nothing() {
    let Some(inst) = boot("n", "headless = false\n") else {
        return;
    };
    run_fake(&inst, "k1", &["read.txt"]);
    program_is(&inst, "claude", HANG);
    let quiet_until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < quiet_until {
        let s = status(&inst);
        assert_eq!(field(&s, "supervisor"), Some("-"), "{s}");
        assert!(
            keys(&inst, "k1").is_empty(),
            "typed: {:?}",
            keys(&inst, "k1")
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Relaunch on exit.
// ---------------------------------------------------------------------------

/// The conversation the relaunch fake registers when nothing resumes it.
const CONVERSATION: &str = "0badf00d-1111-2222-3333-444455556666";

/// Replace the world's fake with one that registers itself as Claude Code
/// does (`~/.claude/sessions/<pid>.json`: its conversation — the one it was
/// resumed on, else [`CONVERSATION`] — its kernel start as Claude renders it,
/// idle), logs its argv to `launches.log`, draws an idle composer — on the
/// alternate screen with focus reports on and the cursor on its caret row, as
/// Claude Code's fullscreen renderer does, so a crash is handed back to a clean
/// shell prompt — and dies
/// by SIGKILL once `die` exists (a crash: its record stays behind). It
/// answers `--version` as the managed twin is asked.
fn write_relaunch_fake(inst: &Instance) {
    std::fs::write(inst.tmp.join("sc/idle.txt"), bypass_start()).expect("idle screen");
    let fake = format!(
        "#!/bin/sh\n\
         case \"$1\" in --version) echo '2.1.281 (Claude Code)'; exit 0 ;; esac\n\
         tmp=${{HOME%/home}}\n\
         echo \"$$ $*\" >> \"$tmp/launches.log\"\n\
         stty -icanon -echo min 1 2>/dev/null\n\
         sid={CONVERSATION}; prev=\n\
         for a in \"$@\"; do [ \"$prev\" = --resume ] && sid=$a; prev=$a; done\n\
         start=$(LC_ALL=C TZ=UTC ps -o lstart= -p $$ | awk '{{$1=$1; print}}')\n\
         mkdir -p \"$HOME/.claude/sessions\"\n\
         printf '{{\"pid\":%s,\"sessionId\":\"%s\",\"cwd\":\"%s\",\"version\":\"2.1.281\",\
         \"status\":\"idle\",\"statusUpdatedAt\":%s000,\"procStart\":\"%s\",\
         \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}' \
         $$ \"$sid\" \"$PWD\" \"$(date +%s)\" \"$start\" > \"$HOME/.claude/sessions/$$.json\"\n\
         printf '\\033[?1049h\\033[?1004h\\033[2J\\033[H'\n\
         cat \"$tmp/sc/idle.txt\"\n\
         r=$(grep -n '^❯' \"$tmp/sc/idle.txt\" | tail -n 1 | cut -d: -f1); [ -n \"$r\" ] && printf '\\033[%s;3H' \"$r\"\n\
         while :; do\n\
         \x20 if [ -e \"$tmp/die\" ]; then rm -f \"$tmp/die\"; kill -KILL $$; fi\n\
         \x20 sleep 0.2\n\
         done\n"
    );
    place_fake(&fake_path(&inst.tmp), &fake);
}

/// Each launch of the relaunch fake: `(pid, args)`.
fn launches(inst: &Instance) -> Vec<(u32, String)> {
    std::fs::read_to_string(inst.tmp.join("launches.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, args) = l.split_once(' ').unwrap_or((l, ""));
            Some((pid.parse().ok()?, args.to_string()))
        })
        .collect()
}

/// The fake's launches once the FIRST one is logged. `program=claude` is the
/// exec, not the fake's first line: under load the interpreter can reach that
/// line seconds later (measured 2026-09-26 at a load average of ~80 on 18
/// cores: nothing logged 2 s after `program=claude`, and an upgrade test's
/// first launch read the twin that replaced it 3 s later), so a test that
/// times its next step from `program=claude` races the fake it launched.
fn first_launch(inst: &Instance) -> Vec<(u32, String)> {
    let deadline = Instant::now() + HANG;
    loop {
        let got = launches(inst);
        if !got.is_empty() || Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// RELAUNCH ON EXIT, live: the tab's shell is bash (a job-control shell the
/// relaunch line is written for), the fake runs from it with a launch flag,
/// and dies by SIGKILL with no person at the tab. The host relaunches it in
/// the same tab — a NEW process, a child of the same shell, `--model opus`
/// kept and `--resume <its conversation>` appended — and the supervisor is
/// back on the session.
#[test]
fn an_agent_that_exits_unasked_is_relaunched_on_its_conversation() {
    let Some(inst) = boot("r", "") else {
        return;
    };
    write_relaunch_fake(&inst);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    // A job-control shell in the tab the relaunch line is written for (the
    // isolation's /bin/sh is not one), as the tab's foreground job.
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "bash", HANG);
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", HANG);
    until(
        HANG,
        "the host attached",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    // Let the worker read the agent it would relaunch — the fake launched
    // and registered ([`first_launch`]), and the worker's loop at the agent's
    // first idle point, where it reads the record a relaunch needs — then
    // crash it. (Waited for as events: a fixed two seconds read an empty
    // launch log under a loaded machine, the fake's first line not run yet.)
    let first = first_launch(&inst);
    assert_eq!(
        first.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["--model opus"],
        "{first:?}"
    );
    until(
        HANG,
        "the worker at the agent's first idle point",
        || harness_lines(&inst),
        |l| l.contains("EVENT idle"),
    );
    std::fs::write(inst.tmp.join("die"), "").expect("crash");
    let deadline = Instant::now() + HANG;
    let got = loop {
        let got = launches(&inst);
        if got.len() >= 2 || Instant::now() >= deadline {
            break got;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        got.len(),
        2,
        "relaunched once: {got:?}\nharness log:\n{}",
        harness_lines(&inst)
    );
    let (old, _) = &got[0];
    let (new, args) = &got[1];
    assert_ne!(old, new, "a new process");
    assert_eq!(
        args,
        &format!("--model opus --resume {CONVERSATION}"),
        "its flags kept, its conversation resumed"
    );
    program_is(&inst, "claude", HANG);
    // The relaunch ends at adoption and the new agent is its supervisor's at
    // once; the continuation is OWED to the tab (its record in flight) until
    // that supervisor's loop first reaches an idle point, and TYPED there (the
    // record done, the resumed process named) — the engine hands its host the
    // idle point (`IdleHost::at_idle`) and the worker takes the carry-on in
    // the loop.
    until(
        HANG,
        "adopted, and supervised again",
        || format!("{}\n{}", status(&inst), harness_lines(&inst)),
        |s| {
            s.lines()
                .any(|l| l.contains("relaunch pid=") && l.contains("step=adopted"))
                && field(s.lines().next().unwrap_or(""), "supervisor")
                    .is_some_and(|v| v.starts_with("aterm-harness"))
        },
    );
    let name = format!("{CONVERSATION}.json");
    let record = until(
        HANG,
        "the continuation typed where the loop parked",
        || {
            ["state", "home"]
                .iter()
                .filter_map(|root| find_file(&inst.tmp.join(root), &name))
                .find(|p| p.parent().is_some_and(|d| d.ends_with("upgrade")))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default()
        },
        |r| r.contains(r#""phase":"done""#) && !r.contains(r#""resumed_pid":0"#),
    );
    assert!(record.contains(r#""cause":"exit""#), "{record}");
}

// ---------------------------------------------------------------------------
// The live upgrade, a step of the worker.
// ---------------------------------------------------------------------------

/// Stage the upgrade fake reporting `version` beside the managed twin, as
/// `agents/claude.<version>`, its first exec paid ([`place_fake`]), and return
/// its path; [`install`] and [`land`] rename a staged build onto the twin, as
/// atpkg re-renders the twin, so a running copy keeps the file it started
/// from. A test stages EVERY build it uses before the session launches any
/// (asserted), so that once a copy runs nothing it reads is written and no
/// file is met unchecked (both measured 2026-10-04):
/// - Its screen files are read as a copy starts — `sc/idle.txt` three times:
///   the composer's row, its rule, its footer — and `sc/idle.frame` at every
///   frame it draws. Rewritten while a copy started, a read between
///   `fs::write`'s truncate and its write found no composer row; the copy's
///   one frame had no caret on a composer, the verdict stayed `unknown` while
///   it waited for input, and `await agent idle` timed out (forced with the
///   writer held between its truncate and its write: `OK timeout`,
///   `agent=unknown`).
/// - The system's check of a new file's first exec ([`place_fake`], seconds
///   under load) is paid before the session starts: not in the running
///   copy's start-up, not in the host's probe of the twin, and not between
///   the agent's first idle verdict and the prompt a test sends at it, which
///   is inside the fresh session's settle
///   ([`aterm_agent::harness::upgrade::QUIET_S`], counted from that verdict).
///
/// The fake registers itself as Claude does
/// (its status settled a minute ago), draws an idle composer — the clear, the
/// rows and the caret in ONE write (`sc/idle.frame`), as Claude Code writes a
/// frame: drawn as a clear, then a `cat` of the rows, then the caret, a look
/// between the clear and the rows read the blank screen as `agent=unknown`,
/// and the idle verdict the upgrade's settle counts from
/// ([`aterm_agent::harness::upgrade::QUIET_S`]) began again, so the redraw
/// at the announcement's own Enter cost a second 20 s settle before the
/// restart (`step=wait:settling`: 2 of 4 runs of the activation test, 43 s
/// from the notice where 23 s is its floor, 2026-10-05) — and reads each
/// line typed into it — drawn as it is typed, the way Claude Code's composer
/// wraps it (between words, a continuation row indented two) and grows (its
/// bottom rule and footer redrawn under each new row, in the same write),
/// because the host holds a pressed text to its input frame between keys
/// (the row under the cursor is the bottom rule) and checks the row the text
/// ends on before its Enter (`composer_guard`). It never prints into the
/// last column: a glyph there defers its wrap and leaves the cursor where it
/// was, and a digit's key waits for the caret to move off the cursor the key
/// before it went out from (`TypingHold::held`), so a digit after it would
/// wait out the turn's deadline. Each line is logged at the Enter to
/// `typed.log` as `<its pid> <line>`; an announcement's READY marker is
/// answered in its transcript, the carry-on with a turn of the running
/// build's own, and any other line recorded there as the prompt it is
/// (Claude Code writes a prompt's row as it is submitted).
/// One started on a terminal while `hold-idle-<its version>` stands draws its
/// composer only once the test removes it: an agent still booting, as long
/// as the test needs (a probe of the build — no terminal — is never held,
/// and neither is a build the test does not name).
/// `asked`: a new conversation's transcript opens with a person's prompt and
/// its answer (a session someone asked something); without it the record
/// holds nothing of anyone's. SIGTERM ends it as Claude's graceful shutdown
/// does, its record removed. macOS only, as every caller is (each writes
/// atpkg's activation notice, which is macOS-only).
#[cfg(target_os = "macos")]
fn stage_upgrade_fake(inst: &Instance, version: &str, asked: bool) -> PathBuf {
    assert!(
        launches(inst).is_empty(),
        "{version} staged after a launch: {:?}",
        launches(inst)
    );
    let idle = bypass_start();
    let caret = idle
        .lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with('❯'))
        .last()
        .map(|(i, _)| i + 1)
        .expect("the composer's caret row");
    std::fs::write(inst.tmp.join("sc/idle.txt"), &idle).expect("idle screen");
    std::fs::write(
        inst.tmp.join("sc/idle.frame"),
        format!("\x1b[2J\x1b[H{idle}\x1b[{caret};3H"),
    )
    .expect("idle frame");
    let row = |text: &str| {
        format!(
            "printf '{{\"type\":\"assistant\",\"message\":{{\"model\":\"claude-test\",\
             \"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"%s\"}}]}},\
             \"version\":\"%s\"}}\\n' {text} \"$VERSION\" >> \"$T\""
        )
    };
    let fake = format!(
        "#!/bin/sh\n\
         VERSION={version}\n\
         case \"$1\" in --version) echo \"$VERSION (Claude Code)\"; exit 0 ;; esac\n\
         tmp=${{HOME%/home}}\n\
         echo \"$$ $VERSION $*\" >> \"$tmp/launches.log\"\n\
         sid={CONVERSATION}; prev=\n\
         for a in \"$@\"; do [ \"$prev\" = --resume ] && sid=$a; prev=$a; done\n\
         start=$(LC_ALL=C TZ=UTC ps -o lstart= -p $$ | awk '{{$1=$1; print}}')\n\
         mkdir -p \"$HOME/.claude/sessions\" \"$HOME/.claude/projects/-w\"\n\
         T=\"$HOME/.claude/projects/-w/$sid.jsonl\"\n\
         {ask}\n\
         printf '{{\"pid\":%s,\"sessionId\":\"%s\",\"cwd\":\"%s\",\"version\":\"%s\",\
         \"status\":\"idle\",\"statusUpdatedAt\":%s000,\"procStart\":\"%s\",\
         \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}' \
         $$ \"$sid\" \"$PWD\" \"$VERSION\" \"$(( $(date +%s) - 60 ))\" \"$start\" \
         > \"$HOME/.claude/sessions/$$.json\"\n\
         trap 'rm -f \"$HOME/.claude/sessions/$$.json\"; exit 0' TERM\n\
         stty -echo -icanon min 1 time 0 2>/dev/null\n\
         [ -t 0 ] && while [ -e \"$tmp/hold-idle-$VERSION\" ]; do sleep 0.05; done\n\
         W=$(stty size 2>/dev/null | cut -d' ' -f2); [ -n \"$W\" ] || W=200\n\
         W=$((W - 1))\n\
         r=$(grep -n '^❯' \"$tmp/sc/idle.txt\" | tail -n 1 | cut -d: -f1)\n\
         rule=$(sed -n \"$((r + 1))p\" \"$tmp/sc/idle.txt\"); foot=$(sed -n \"$((r + 2))p\" \"$tmp/sc/idle.txt\")\n\
         draw() {{\n\
         \x20 cat \"$tmp/sc/idle.frame\"\n\
         \x20 buf=; word=; col=2; row=$r\n\
         }}\n\
         wrap() {{\n\
         \x20 row=$((row + 1)); col=2\n\
         \x20 printf '\\033[%s;1H\\033[K  \\033[%s;1H\\033[K%s\\033[%s;1H\\033[K%s\\033[%s;3H' \
         \"$row\" $((row + 1)) \"$rule\" $((row + 2)) \"$foot\" \"$row\"\n\
         }}\n\
         take() {{\n\
         \x20 line=$1\n\
         \x20 printf '%s %s\\n' $$ \"$line\" >> \"$tmp/typed.log\"\n\
         \x20 m=$(printf '%s' \"$line\" | grep -o 'ATERM-UPGRADE-READY-[0-9a-f]*' | head -1)\n\
         \x20 [ -n \"$m\" ] && {ready}\n\
         \x20 case \"$line\" in *Upgraded*) {carry} ;; \"[aterm harness]\"*) ;; *) {prompt} ;; esac\n\
         }}\n\
         cr=$(printf '\\r'); nl=$(printf '\\n.'); nl=${{nl%.}}\n\
         draw\n\
         while IFS= read -r -n 1 -d '' c; do\n\
         \x20 case \"$c\" in\n\
         \x20 \"$cr\"|\"$nl\") take \"$buf\"; draw ;;\n\
         \x20 ' ') buf=\"$buf \"; word=\n\
         \x20   if [ \"$col\" -ge \"$W\" ]; then wrap; else printf ' '; col=$((col + 1)); fi ;;\n\
         \x20 *) buf=\"$buf$c\"\n\
         \x20   if [ $((col + 1)) -le \"$W\" ]; then printf '%s' \"$c\"; col=$((col + 1)); word=\"$word$c\"\n\
         \x20   elif [ -n \"$word\" ] && [ ${{#word}} -lt $((col - 2)) ]; then\n\
         \x20     printf '\\033[%sD\\033[K' ${{#word}}; wrap; printf '%s%s' \"$word\" \"$c\"; word=\"$word$c\"; col=$((2 + ${{#word}}))\n\
         \x20   else wrap; printf '%s' \"$c\"; word=\"$c\"; col=3; fi ;;\n\
         \x20 esac\n\
         done\n",
        ready = row("\"$m\""),
        carry = row("'carrying on'"),
        prompt = "printf '{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"%s\"}}\\n' \"$line\" >> \"$T\"",
        ask = if asked {
            format!(
                "[ -e \"$T\" ] || {{ printf '{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\
                 \"content\":\"Fix the parser.\"}}}}\\n' > \"$T\"; {}; }}",
                row("'On it.'")
            )
        } else {
            ":".to_string()
        },
    );
    let staged = fake_path(&inst.tmp).with_file_name(format!("claude.{version}"));
    place_fake(&staged, &fake);
    staged
}

/// Put a staged build ([`stage_upgrade_fake`]) at the twin's path, by a
/// rename: the path only ever names a file the system has checked.
#[cfg(target_os = "macos")] // only the macOS-only upgrade tests use it
fn install(inst: &Instance, staged: &Path) {
    std::fs::rename(staged, fake_path(&inst.tmp)).expect("replace the twin");
}

/// A newer build lands and atpkg says so: the staged build [`install`]ed and
/// atpkg's activation notice written (a push — no sweep runs).
#[cfg(target_os = "macos")] // atpkg::activation_notice is macOS-only
fn land(inst: &Instance, staged: &Path) {
    install(inst, staged);
    let prefix = atpkg::store::default_prefix(&inst.tmp.join("home"));
    let notices = prefix.join(atpkg::activation_notice::NOTICE_DIR);
    std::fs::create_dir_all(&notices).expect("notice dir");
    std::fs::write(notices.join(".m.tmp"), "1\n").expect("marker");
    std::fs::rename(
        notices.join(".m.tmp"),
        notices.join(atpkg::activation_notice::CLAUDE_MARKER),
    )
    .expect("the activation notice");
}

/// How long the live upgrade has to take a step, at most relaunching the
/// agent on the new build — its announcement, the agent's READY, its end and
/// its relaunch — from the notice (or the prompt the step waits on), or from
/// the host's latest look that could not read that build ([`version_waits`],
/// [`upgrade_until`]).
const UPGRADE_WITHIN: Duration = Duration::from_secs(150);

/// How far those looks may carry the deadline. The version cache asks a
/// build that did not answer again two minutes later, and the host looks
/// at least once a minute meanwhile, so a build that answers its second
/// probe is upgraded to within [`UPGRADE_WITHIN`] of a look five minutes
/// after the notice at the latest; this bounds a product that looks for ever.
const UPGRADE_CEILING: Duration = Duration::from_secs(10 * 60);

/// How many looks the loop journaled (`<sid>.journal.jsonl`; a host's step
/// is a journal line, never a log line) as `upgrade step=wait:no-version`:
/// a build the session could move to did not answer its `--version` within
/// the product's bound (`aterm_agent::harness::upgrade_drive`'s
/// `VERSION_WAIT`), so the look decided nothing and the host looks again on
/// its own short ladder until the version cache asks again
/// (`UNREAD_VERSION`) — the product's own retry, which a fixed deadline
/// cannot know. [`place_fake`] keeps the fakes' probes inside the bound; this
/// is what a probe that still missed it is waited by.
fn version_waits(inst: &Instance) -> usize {
    journal(inst).matches("step=wait:no-version").count()
}

/// The session's loop journal (`<sid>.journal.jsonl`): a host's step is a
/// journal line, and a step's wait word is written nowhere else — the gate
/// failure of 32a51a716 could not say which wait left its restart in flight.
fn journal(inst: &Instance) -> String {
    ["state", "home"]
        .iter()
        .find_map(|root| find_file(&inst.tmp.join(root), &format!("{}.journal.jsonl", inst.sid)))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// Read `probe` until `done` holds of a reading, for a step of the live
/// upgrade owed from `from` (the notice, or the prompt the step waits on),
/// and return the last reading, done or not, for the caller's assertion to
/// name. The step's own settle ([`aterm_agent::harness::upgrade::QUIET_S`])
/// and the look after it are inside [`UPGRADE_WITHIN`]; a look whose build
/// did not answer its `--version` within the product's bound decides nothing
/// and is looked at again ([`version_waits`]), and a step taken later is the
/// product working, not a red. The deadline moves to [`UPGRADE_WITHIN`] past
/// each such look the host journals, never on a guess, and never past
/// [`UPGRADE_CEILING`].
#[cfg(target_os = "macos")] // only the macOS-only upgrade tests use it
fn upgrade_until<T>(
    inst: &Instance,
    from: Instant,
    probe: impl Fn() -> T,
    done: impl Fn(&T) -> bool,
) -> T {
    let mut waits = version_waits(inst);
    let mut deadline = from + UPGRADE_WITHIN;
    loop {
        let got = probe();
        if done(&got) {
            return got;
        }
        let now = version_waits(inst);
        if now > waits {
            waits = now;
            deadline = deadline
                .max(Instant::now() + UPGRADE_WITHIN)
                .min(from + UPGRADE_CEILING);
        }
        if Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// THE LIVE UPGRADE AS A STEP OF THE WORKER, live: a fake Claude Code at
/// 2.1.281 runs as bash's foreground job; the managed twin is replaced by
/// 2.1.282 and atpkg's activation notice is written (a push — no sweep runs).
/// The worker takes its loop's idle point, announces, reads the READY
/// answer at the next one, ends the agent with SIGTERM and relaunches it in
/// the same tab ON THE NEW BUILD, on its conversation (`--resume <id>`, its
/// flags kept), and tells it to carry on. NEGATIVE CONTROL: before the
/// notice, a session on the current build is never announced to.
#[cfg(target_os = "macos")] // atpkg::activation_notice is macOS-only
#[test]
fn an_activation_notice_upgrades_the_session_at_its_idle_point() {
    // `upgrade_ask = true`: since 0b689dfe3 (the owner's "never interrupt an
    // agent that is working", 2026-09-29) the default moves an idle session
    // with nothing typed; the knob restores the notice, READY and carry-on
    // flow this test pins.
    let Some(inst) = boot_upgrade(
        "v",
        "relaunch = false\nupgrade_ask = true\n",
        Some(SHORT_SETTLE_S),
    ) else {
        return;
    };
    let current = stage_upgrade_fake(&inst, "2.1.281", true);
    let newer = stage_upgrade_fake(&inst, "2.1.282", true);
    install(&inst, &current);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "bash", HANG);
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", HANG);
    until(
        HANG,
        "the host attached",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    // The CURRENT build is what ran — the twin is replaced below, and a fake
    // whose interpreter reached its file after that would start on 2.1.282.
    let first = first_launch(&inst);
    assert_eq!(
        first.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["2.1.281 --model opus"],
        "{first:?}"
    );
    // NEGATIVE CONTROL: current, so nothing is announced.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !harness_lines(&inst).contains("announced"),
        "{}",
        harness_lines(&inst)
    );
    // A newer build lands, and atpkg says so.
    land(&inst, &newer);
    let got = upgrade_until(
        &inst,
        Instant::now(),
        || launches(&inst),
        |got| got.len() >= 2,
    );
    assert_eq!(
        got.len(),
        2,
        "relaunched once: {got:?}\nharness log:\n{}\njournal:\n{}",
        harness_lines(&inst),
        journal(&inst)
    );
    assert_ne!(got[0].0, got[1].0, "a new process");
    assert_eq!(
        got[1].1,
        format!("2.1.282 --model opus --resume {CONVERSATION}"),
        "the new build, its flags kept, its conversation resumed"
    );
    let log = until(
        HANG,
        "announced, then told to carry on",
        || harness_lines(&inst),
        |l| {
            l.contains("step=announced:1")
                && (l.contains("step=done") || l.contains("step=continued"))
        },
    );
    assert!(!log.contains("minute"), "{log}");
    program_is(&inst, "claude", HANG);
    // D6 of the live E2E of 2026-09-26: the owner's view is published on a
    // headless instance too — the tab's `upgrade=` names the finished move
    // (it read `-` headless, the view handed to no loop).
    until(
        HANG,
        "the tab's upgrade= column, headless",
        || status(&inst),
        |s| field(s, "upgrade").is_some_and(|v| v.starts_with("done/2.1.282/")),
    );
}

/// D1 OF THE LIVE E2E OF 2026-09-26, live: A SESSION NOBODY HAS ASKED
/// ANYTHING is restarted onto the newer build AFRESH, with nothing typed into
/// it. The fake's conversation holds no prompt of anyone's; the newer build
/// lands and atpkg says so; at the loop's idle point the worker ends the
/// agent and starts the new build in the same tab — its flags kept, NO
/// `--resume` — and types nothing into either process: no notice, no
/// carry-on, through the restart and the five seconds after it. (The E2E's
/// fresh sessions each got the notice, which started a conversation nobody
/// asked for.) What this run cannot observe — the turn ends after: the
/// continue policy's first back-off is two minutes, and the fresh screen
/// already holds it — is pinned by aterm-agent's unit tests of the policy
/// and the loop (`a_session_only_the_harness_has_typed_into_gets_no_act`,
/// and the loop's test that a session its host says nobody asked anything
/// gets nothing typed). NEGATIVE CONTROL: the test above — the same fake
/// with a person's prompt in its record — is announced to and resumed.
#[cfg(target_os = "macos")] // atpkg::activation_notice is macOS-only
#[test]
fn a_session_nobody_asked_anything_is_restarted_afresh_with_nothing_typed() {
    let Some(inst) = boot_upgrade("k", "relaunch = false\n", Some(SHORT_SETTLE_S)) else {
        return;
    };
    let current = stage_upgrade_fake(&inst, "2.1.281", false);
    let newer = stage_upgrade_fake(&inst, "2.1.282", false);
    install(&inst, &current);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "bash", HANG);
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", HANG);
    until(
        HANG,
        "the host attached",
        || status(&inst),
        |s| field(s, "supervisor").is_some_and(|v| v.starts_with("aterm-harness")),
    );
    // The running copy has read its script (a rename before `sh` opened it
    // would start the newer build instead).
    let first = first_launch(&inst);
    assert_eq!(
        first.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["2.1.281 --model opus"],
        "{first:?}"
    );
    land(&inst, &newer);
    let got = upgrade_until(
        &inst,
        Instant::now(),
        || launches(&inst),
        |got| got.len() >= 2,
    );
    assert_eq!(
        got.len(),
        2,
        "restarted once: {got:?}\nharness log:\n{}\njournal:\n{}",
        harness_lines(&inst),
        journal(&inst)
    );
    assert_ne!(got[0].0, got[1].0, "a new process");
    assert_eq!(
        got[1].1, "2.1.282 --model opus",
        "the new build, its flags kept, nothing resumed"
    );
    let log = until(
        HANG,
        "restarted afresh",
        || harness_lines(&inst),
        |l| l.contains("step=done:fresh"),
    );
    assert!(!log.contains("announced"), "no notice: {log}");
    program_is(&inst, "claude", HANG);
    // Nothing typed into either process through the restart and the 5 s
    // after it (the turn ends after are the unit tests': see above).
    std::thread::sleep(Duration::from_secs(5));
    let typed = std::fs::read_to_string(inst.tmp.join("typed.log")).unwrap_or_default();
    assert!(typed.is_empty(), "typed into the agent: {typed:?}");
    until(
        HANG,
        "the tab's upgrade= column, headless",
        || status(&inst),
        |s| field(s, "upgrade").is_some_and(|v| v.starts_with("done/2.1.282/")),
    );
}

/// A fake Claude Code at 2.1.281 in `shell` (`bash` or `zsh`), its
/// supervisor attached, nobody having asked it anything — and 2.1.282
/// installed, atpkg having said so: the upgrade is due before anyone's first
/// prompt (the D1 E2E's order). `harness` is the instance's `[harness]`
/// table: `upgrade_ask = true` for a test of the notice-and-READY flow, which
/// is no longer the default (0b689dfe3). `settle_s` as [`boot_upgrade`].
#[cfg(target_os = "macos")] // atpkg::activation_notice is macOS-only
fn a_fresh_session_behind(
    tag: &str,
    shell: &str,
    harness: &str,
    settle_s: Option<u64>,
) -> Option<Instance> {
    let inst = boot_upgrade(tag, harness, settle_s)?;
    let current = stage_upgrade_fake(&inst, "2.1.281", false);
    let newer = stage_upgrade_fake(&inst, "2.1.282", false);
    install(&inst, &current);
    let sel = format!("@{}", inst.sid);
    let tmp = inst.tmp.display();
    let interactive = match shell {
        "zsh" => "/bin/zsh -f -i",
        _ => "/bin/bash --norc --noprofile -i",
    };
    ctl_ok(&inst, &[&sel, "send", interactive]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, shell, HANG);
    let line = format!(
        "cd {tmp}/work && '{}' --model opus",
        fake_path(&inst.tmp).display()
    );
    ctl_ok(&inst, &[&sel, "send", &line]);
    ctl_ok(&inst, &[&sel, "key", "enter"]);
    program_is(&inst, "claude", HANG);
    let first = first_launch(&inst);
    assert_eq!(
        first.iter().map(|(_, a)| a.as_str()).collect::<Vec<_>>(),
        ["2.1.281 --model opus"],
        "{first:?}"
    );
    land(&inst, &newer);
    Some(inst)
}

/// The lines each process of the upgrade fake read, by pid.
#[cfg(target_os = "macos")] // only the macOS-only upgrade tests use it
fn typed_by(inst: &Instance) -> Vec<(u32, String)> {
    std::fs::read_to_string(inst.tmp.join("typed.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (pid, line) = l.split_once(' ')?;
            Some((pid.parse().ok()?, line.to_string()))
        })
        .collect()
}

/// ND1 OF THE LIVE RE-TEST OF 2026-09-26, live, five times over: A FIRST
/// PROMPT SENT AT THE FIRST IDLE VERDICT IS THE AGENT'S. An orchestrator
/// `await`s `agent idle` on a session nobody has asked anything, whose
/// upgrade is due, and sends its first prompt at once as orchestrators do
/// (`send`, then `key enter`) — the moment the fresh restart used to fire
/// (1.3 s after that verdict, the tab then a bare shell). Its words carry a
/// shell command (`touch`): run by the shell, they would leave a file. Each
/// time the prompt reaches the agent it was sent to and is recorded as its
/// conversation's first task, the session is never restarted afresh, and
/// the upgrade announces to it instead, the one launch still running.
#[cfg(target_os = "macos")] // atpkg::activation_notice is macOS-only
#[test]
fn a_first_prompt_sent_at_the_first_idle_is_the_agents_never_the_shells() {
    std::thread::scope(|scope| {
        let runs: Vec<_> = (0..5)
            .map(|i| {
                scope.spawn(move || {
                    let Some(inst) = a_fresh_session_behind(
                        &format!("q{i}"),
                        "bash",
                        // The announce flow this test is about (0b689dfe3
                        // made the quiet move the default).
                        "relaunch = false\nupgrade_ask = true\n",
                        // The product's settle: it is what this prompt
                        // races, its whole margin ([`SHORT_SETTLE_S`]).
                        None,
                    ) else {
                        return;
                    };
                    let sel = format!("@{}", inst.sid);
                    let pwned = inst.tmp.join(format!("work/pwned-{i}"));
                    let prompt = format!("Fix the parser; touch {}", pwned.display());
                    let idle = ctl(
                        &inst,
                        &[
                            &sel,
                            "await",
                            "agent",
                            "idle",
                            &format!("timeout={}", HANG.as_millis()),
                        ],
                    );
                    let said = String::from_utf8_lossy(&idle.stdout);
                    if !(idle.status.success() && said.starts_with("OK agent idle ")) {
                        // What the verdict was read from: the frame and the
                        // caret on it, the copy that drew it, and the host.
                        let look = |args: &[&str]| {
                            String::from_utf8_lossy(&ctl(&inst, args).stdout).into_owned()
                        };
                        panic!(
                            "run {i}: await agent idle answered {said:?} (stderr {:?})\n\
                             status: {}cursor: {}launches: {:?}\nscreen:\n{}\n\
                             harness log:\n{}\njournal:\n{}",
                            String::from_utf8_lossy(&idle.stderr),
                            look(&[&sel, "status"]),
                            look(&[&sel, "cursor"]),
                            launches(&inst),
                            look(&[&sel, "text"]),
                            harness_lines(&inst),
                            journal(&inst)
                        );
                    }
                    ctl_ok(&inst, &[&sel, "send", &prompt]);
                    ctl_ok(&inst, &[&sel, "key", "enter"]);
                    let log = upgrade_until(
                        &inst,
                        Instant::now(),
                        || harness_lines(&inst),
                        |l| l.contains("step=announced:1") || l.contains("step=done:fresh"),
                    );
                    assert!(
                        !log.contains("done:fresh"),
                        "run {i}: restarted afresh: {log}"
                    );
                    assert!(
                        log.contains("step=announced:1"),
                        "run {i}: announced to (the prompt made a task): {log}\njournal:\n{}",
                        journal(&inst)
                    );
                    assert!(!pwned.exists(), "run {i}: the shell ran the prompt");
                    let first = launches(&inst)[0].0;
                    assert!(
                        typed_by(&inst)
                            .iter()
                            .any(|(pid, line)| *pid == first && *line == prompt),
                        "run {i}: the prompt never reached the agent: {:?}",
                        typed_by(&inst)
                    );
                    assert_eq!(launches(&inst).len(), 1, "run {i}: {:?}", launches(&inst));
                })
            })
            .collect();
        for run in runs {
            run.join().expect("a run");
        }
    });
}

/// ND1, live: THE RESTART HOLDS THE TAB. A session nobody has asked anything
/// is restarted afresh; from its last look to the relaunched agent's first
/// idle — which the test holds back until the orchestrator has met the hold
/// once (`hold-idle-2.1.282`, the relaunched build's), so the window is the
/// test's, not a race — the
/// harness's HARD hand is on the tab
/// (`hand=lease:aterm-harness@<pid>`) — between the signal and the relaunch
/// the tab is a bare shell. An orchestrator that sends its first prompt
/// meanwhile is refused (`ERR busy lease=…`) and retries — as a `turn`, and
/// as the RAW `send` and `key enter` the first cut's cooperative lease let
/// through into the bare shell (the review of it) — and the prompt lands in
/// the RELAUNCHED agent: never run by the shell (its `touch` leaves no
/// file), never read by the old process. Once at a bash prompt (the line a
/// raw `send`), once at zsh (the line the harness's own fenced `turn`,
/// typed under its own hold).
#[cfg(target_os = "macos")] // atpkg::activation_notice is macOS-only
#[test]
fn a_prompt_sent_while_the_restart_holds_the_tab_lands_in_the_new_agent() {
    std::thread::scope(|scope| {
        let runs: Vec<_> = [("hb", "bash", true), ("hz", "zsh", false)]
            .into_iter()
            .map(|(tag, shell, turn)| scope.spawn(move || prompt_meets_the_hand(tag, shell, turn)))
            .collect();
        for run in runs {
            run.join().expect("a run");
        }
    });
}

/// One run of [`a_prompt_sent_while_the_restart_holds_the_tab_lands_in_the_new_agent`]:
/// the relaunch line typed at `shell`, the orchestrator's prompt sent as a
/// `turn` or as a raw `send` and `key enter`.
#[cfg(target_os = "macos")] // only the macOS-only upgrade tests use it
fn prompt_meets_the_hand(tag: &str, shell: &str, turn: bool) {
    let Some(inst) = a_fresh_session_behind(tag, shell, "relaunch = false\n", Some(SHORT_SETTLE_S))
    else {
        return;
    };
    let sel = format!("@{}", inst.sid);
    let pwned = inst.tmp.join("work/pwned");
    let prompt = format!("Fix the parser; touch {}", pwned.display());
    // The relaunched agent comes up idle only once the orchestrator has met
    // the hold.
    let gate = inst.tmp.join("hold-idle-2.1.282");
    std::fs::write(&gate, "").expect("the gate");
    let hand_on =
        |s: &String| field(s, "hand").is_some_and(|h| h.starts_with("lease:aterm-harness@"));
    let st = upgrade_until(&inst, Instant::now(), || status(&inst), hand_on);
    assert!(
        hand_on(&st),
        "{shell}: the restart's hand on the tab: {st}\njournal:\n{}",
        journal(&inst)
    );

    // The orchestrator's retry loop: refused while the hand is on the tab —
    // `ERR busy lease=aterm-harness@…`, or `ERR busy turn=…` while the
    // harness's own relaunch line types under it — and typed once it is not,
    // a turn's submit verified like any other (the fake redraws its composer
    // at the Enter), so every answer that is not busy is a success.
    let deadline = Instant::now() + HANG;
    let mut held = 0;
    let mut until_taken = |args: &[&str]| loop {
        let out = ctl(&inst, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.contains("ERR busy") {
            assert!(out.status.success(), "{shell}: {args:?}: {out:?}");
            break;
        }
        if stderr.contains("ERR busy lease=aterm-harness@") {
            held += 1;
            let _ = std::fs::remove_file(&gate);
        }
        assert!(Instant::now() < deadline, "{shell}: never let in");
        std::thread::sleep(Duration::from_millis(100));
    };
    if turn {
        // The turn's own bound is the [`HANG`] minute: a settle that must come,
        // and `status=timeout` exits 124.
        let hang_ms = format!("timeout={}", HANG.as_millis());
        until_taken(&[&sel, "turn", "idle=1", &hang_ms, &prompt]);
    } else {
        until_taken(&[&sel, "send", &prompt]);
        until_taken(&[&sel, "key", "enter"]);
    }
    assert!(held >= 1, "{shell}: the hand held the tab");
    let log = until(
        HANG,
        "restarted afresh",
        || harness_lines(&inst),
        |l| l.contains("step=done:fresh"),
    );
    let got = launches(&inst);
    assert_eq!(got.len(), 2, "{shell}: {got:?}\n{log}");
    until(
        HANG,
        "the prompt in the relaunched agent",
        || format!("{:?}", typed_by(&inst)),
        |_| {
            typed_by(&inst)
                .iter()
                .any(|(pid, line)| *pid == got[1].0 && *line == prompt)
        },
    );
    assert!(!pwned.exists(), "{shell}: the shell ran the prompt");
    assert!(
        !typed_by(&inst).iter().any(|(pid, _)| *pid == got[0].0),
        "{shell}: the old process read nothing: {:?}",
        typed_by(&inst)
    );
}

/// A WALL THE HOST RESUMES IS NO STOP, LIVE. The owner's weekly
/// wall behind a shell is published as `agent=wall:usage-weekly` (it read `busy`
/// before), and while aterm's own harness holds the session and will continue it
/// at the reset, the presence level is no `limited` — no red rim, no filled
/// diamond saying "Agent can't continue" for the hours the harness spends
/// resuming it (`note`, or `story` until the human looks). NEGATIVE CONTROL: the
/// same wall under `resume_limits = false` — nobody will continue it — is
/// `level=limited`, the stop, as ever. (The two instances share nothing, so
/// they run side by side.)
#[test]
fn a_wall_the_host_resumes_is_no_stop_and_one_nobody_answers_is() {
    std::thread::scope(|scope| {
        let runs: Vec<_> = [("wn", "", false), ("wo", "resume_limits = false\n", true)]
            .into_iter()
            .map(|(tag, harness, stop)| scope.spawn(move || wall_level(tag, harness, stop)))
            .collect();
        for run in runs {
            run.join().expect("a run");
        }
    });
}

/// One run of [`a_wall_the_host_resumes_is_no_stop_and_one_nobody_answers_is`]:
/// the wall under `harness`, `level=limited` exactly when `stop`.
fn wall_level(tag: &str, harness: &str, stop: bool) {
    let Some(inst) = boot(tag, harness) else {
        return;
    };
    run_fake(&inst, "k1", &["wall.hold"; 12]);
    // Published as the wall, and held by the in-window harness ...
    let st = until(
        HANG,
        "the wall is published under the harness's claim",
        || status(&inst),
        |s| s.contains("agent=wall:usage-weekly") && s.contains("supervisor=aterm-harness@"),
    );
    // ... and, once the claim is on the timeline and presence has read it,
    // the level follows who answers the wall.
    let st = if stop {
        st
    } else {
        until(
            HANG,
            "the level leaves the stop",
            || status(&inst),
            |s| s.contains("agent=wall:usage-weekly") && !s.contains("level=limited"),
        )
    };
    assert_eq!(
        st.contains("level=limited"),
        stop,
        "{tag}: {st}\n{}",
        harness_lines(&inst)
    );
}
