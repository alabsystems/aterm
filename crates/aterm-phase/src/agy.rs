// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! agy's screen grammar — Google's Antigravity CLI — MEASURED on agy 1.4.0
//! (2026-10-16, a private headless aterm at 120×40 and 100×30, the vendor's
//! build under a signed-in account and under a scratch HOME; fixtures in
//! [`fixtures`], each naming its provenance, colours and cursor on line 1).
//! [`crate::reader::AgyReader`] is this module behind the reader trait;
//! nothing here reads another program's screen.
//!
//! **THE LAYOUT.** Past its trust box agy draws inline (no alternate
//! screen), so the screen is the transcript's tail with the live zone under
//! it:
//!
//! ```text
//! ──────────────────────────               a turn's separator (60 columns)
//! > Write about 300 words …               the person's message: `>` in column 0
//!   Ocean tides—the rhythmic …            the reply, two columns in
//! ● Ran (ls -la) (ctrl+o to expand)       a tool row
//! ⣾  Generating...                        the SPINNER row, while a turn runs
//! └ Tip: To disable tips, …                 and sometimes a tip under it
//! ──────────────────────────────────────   the COMPOSER: a full-width rule,
//! >                                         the caret and, empty, nothing
//! ──────────────────────────────────────    (the cursor parks at column 2), a
//! ? for shortcuts  Gemini 4 Argon · medium  rule and the FOOTER
//! ```
//!
//! **A MESSAGE'S BLANK LINE** is drawn as a blank row, the message's rows
//! under it two columns in, as a reply's (measured in the lab 2026-10-21,
//! [`fixtures::lab::WORKER_FIRST_PROMPT`]): nothing in the TEXT tells it
//! from the blank row that ends a message, so the rows past it read as the
//! reply ([`said_tail`]). A single line break draws the next row of the
//! same message, no blank row between (measured, `l15a2`). So nothing aterm
//! types into agy holds a blank line: every act the harness types goes as
//! one line (the control wire frames a request per line;
//! `aterm_agent::supervise`'s act executor makes an owner's text of two
//! paragraphs one), and a worker's first prompt, written to a file agy
//! reads, drops its blank lines ([`crate::ScreenReader::bounds_messages`]
//! is `false` here alone): aterm's own words are read back as nobody's
//! reply, and no `NEEDS YOU:` or `Nothing left:` in them as agy's need or
//! defense. A message aterm did not type — a person's own, typed or pasted,
//! or raw bytes a driver sends — is BOUNDED BY ITS CELLS (MEASURED in the
//! lab 2026-10-21, [`fixtures::lab::PERSON_PARAGRAPHS`] and
//! [`fixtures::lab::PERSON_PARAGRAPHS_LONG_REPLY`], the cells in each
//! file's line 1): agy draws every row of a sent message from column 0 in
//! its caret row's style — `5c5cff` bold on its default theme, the person's
//! message and aterm's act alike — its blank rows among them, while the
//! blank row that ends it and every row of the reply, blank ones too, are
//! plain (`d0d0d0`, no attribute; the composer's caret is `5c5cff` with
//! none, a draft's text plain). The text names the rows to read
//! ([`message_blanks`]); a reader of the cells takes the blank rows drawn
//! in the caret row's style out of the message ([`fold_message`]) before
//! any reading, and the message's paragraphs are never agy's reply.
//!
//! **THE EMPTY COMPOSER** draws no placeholder (measured) — but while a
//! permission mode is on (shift+tab cycles them) it draws the mode's hint
//! after its caret: `Plan mode: research & plan only (shift+tab to cycle)`,
//! and accept-edits', auto's and best-of-N's (its binary's four; UNMEASURED
//! on the screen), cut with `...` in a narrow pane. A hint is no draft
//! ([`composer_draft`]). A draft's blank line is a blank row of the frame.
//! A notice card (` ┃ Meet Gemini 4 Argon` … ` ┃ esc to dismiss`, its bar in
//! column 1) may sit over the top rule: no words of the agent.
//!
//! **THE FOOTER'S LEFT SIDE** says what the composer is doing:
//! `? for shortcuts` when it is empty and idle, nothing while a draft is
//! held (the right side alone: the model, with its effort or without, or
//! `(initializing...)`), `press ctrl+c again to exit` after one ctrl+c (at
//! the empty composer or over a held draft, which stays) or `press ctrl+d
//! again to exit` after one ctrl+d at the empty composer, and `esc to
//! cancel` while a turn runs — AND while a menu or a box is open, so
//! `esc to cancel` alone is no running turn — or `Press ↑ to edit queued
//! messages` while one runs with messages queued behind it (UNMEASURED).
//! The composer STAYS OPEN while a turn runs (the cursor at its column 2):
//! what says a turn runs is the spinner row in the turn's rows above it. No
//! row marks a turn's end; the spinner gone and the footer back to `? for
//! shortcuts` is the end ([`phase`]). `/` opens the command menu UNDER the
//! bottom rule, its key row over the footer's `esc to cancel`.
//!
//! **BACKGROUND WORK** (measured in the lab on 1.4.0, 2026-10-17,
//! [`fixtures::lab::BG_TASK`] and its siblings): a `run_command` past its
//! `WaitMsBeforeAsync` (a daemon one too), a `schedule` timer and an
//! `invoke_subagent` subagent go on after the turn that started them ended,
//! and while they do agy draws a PANEL between the composer's bottom rule
//! and the footer — one row an item, `● ` two columns in: `  ● [17:50:42]
//! sleep 150; echo LAB-BG-DONE running` for a task or a timer, `  ● Agent(
//! research: Lab Waiter)  Working · 1s` for a subagent — then a second full
//! rule, then the footer, whose right side counts them (`· 1 task(s) ·
//! /tasks`, `· 1 subagent(s)`). The panel and the count go when the work
//! ends, and agy then takes a turn of its own on its result (each task's
//! end, timer's firing and subagent's going idle started a model turn with
//! nobody typing). The screen does not move while the work runs (one
//! distinct screen over 130 samples). The composer, a draft in it, the
//! spinner over it and the footer read over the panel as without it: busy
//! on the spinner (a turn sent while a task runs, [`fixtures::lab::BG_BUSY`]),
//! a draft the draft ([`fixtures::lab::BG_DRAFT`]). An ended turn under the
//! panel is the agent's own background work ([`background_wait`]): busy as
//! evidence, so nothing is typed into a session agy itself will wake. Every
//! item agy draws there ends and wakes agy, so each is read as Claude Code's
//! reader reads a shell, a workflow or a background agent it left running
//! (`phase::worker_phase`'s HARD rule): busy over a question the turn ended
//! on too, which agy's own turn on the work's result may answer. A limit and
//! a wall the turn ended on stand over it (limited, idle on its wall), so
//! the wait for a reset, the retry ladder and the sign-in's escalation run
//! as without the panel. Before the
//! panel was read, every screen with it read as no composer: idle NOT as
//! evidence, a draft unseen, a running turn idle. A screen agy LEFT keeps
//! its last frame's panel rows above its resume line
//! ([`fixtures::lab::BG_EXITED`]) and reads nothing. agy's setting `Show
//! active subagents and background tasks below the input box` turned off
//! (`"hideActiveItems": true` in its `settings.json`, measured 2026-10-18)
//! draws no panel, and the footer still counts the work (`· 1 task(s) ·
//! /tasks`, `· 1 subagent(s)`; [`fixtures::lab::BG_PANEL_OFF_TASK`] and its
//! siblings): the count alone is read. A subagent's row reads `Processing...`
//! for a moment as it starts, under the spinner of the turn that spawned it,
//! before `Working` (measured): [`background_wait`] names only the states it
//! knows, while [`background_shown`] — what the live upgrade's quiet restart
//! waits on — reads every row of the panel as work, whatever its words.
//! UNMEASURED: more than two items.
//!
//! **AN EXIT PENDING** (`press ctrl+c again to exit`, `press ctrl+d again to
//! exit`) leaves agy one key from gone: that key again ends it, its resume
//! line printed under its last frame. Measured in the lab on 1.4.0
//! (2026-10-17, [`fixtures::lab::EXIT_CONFIRM_CTRL_D`],
//! [`fixtures::lab::CTRL_C_DRAFT`]): neither hint times out (each still
//! stood 45 s on), a typed character takes it away (the character a draft),
//! one ctrl+c over a held draft arms it too and keeps the draft (the second
//! exits and drops it), and a ctrl+d on a draft deletes forward and asks
//! nothing. So it is no idle point, draft or none: [`phase`] reads it idle
//! NOT as evidence, as it reads the survey, and nothing is typed into an agy
//! about to exit — someone is leaving it, and their second press landing
//! before the typed text would hand that text to the shell.
//!
//! **THE BOXES** replace the composer, the cursor hidden: a bold title over a
//! full rule, the body, a question row, numbered options (`> 1. …` focused,
//! `  2. …`), a blank row and a key row (`  ↑/↓ Navigate · …`) over the
//! footer's `esc to cancel`. A digit picks its option at once (measured on 1
//! to 4). A long option WRAPS with its tail at column 0 of the next row
//! ([`fixtures::BOX_EXEC_WRAPPED`]); a narrow pane wraps the key row onto
//! column-2 rows of its own (`  ↑/↓ Navigate · tab Amend` over `  f full
//! diff`; UNMEASURED):
//!
//! | title | options |
//! |---|---|
//! | `Command` | `Yes, run command` · `Yes, and always allow in this conversation for commands that start with '<p>'` · `Yes, and always allow for commands that start with '<p>' (Persist to settings.json)` · `No, cancel` |
//! | `Create file` | `Yes, allow creation` · outside the workspace `Yes, and always allow non-workspace access` (it persists) · `No, deny creation` |
//! | `Pending edit` | `Yes, accept this change` · `No, reject this change` |
//! | `Question` | the model's answers — the one it recommends opening `(Recommended)` — then `Write-in...` |
//!
//! The pending edit's Esc does nothing (agy 1.4.0's binary): after one, agy
//! says `Esc disabled during file edits — press 1 to accept or 2 to reject.`
//! under the key row (UNMEASURED). The question's mark is the model's own,
//! as agy's tool tells it to write it (its binary; UNMEASURED on the
//! screen); a question whose options are a multi-select's checkboxes (`[ ]
//! Red`, `[x] Red`: a digit only toggles one), or hold the person's own kept
//! answer (`✓ …`), or do not end on `Write-in...`, is answered by no digit
//! and carries no role ([`prompt`]).
//!
//! The TRUST box agy draws in a folder it has not seen is unnumbered (`> Yes,
//! I trust this folder` / `  No, exit`), chosen by the cursor and Enter. Its
//! Esc, drawn nowhere, quits agy ([`esc`], measured in the lab).
//!
//! **SIGNED OUT** agy draws `You are currently not signed in.` over `Select
//! login method:` and `> 1. Google OAuth`, whose Enter opens a browser on the
//! person's desktop. It is a WALL ([`wall`], `WallKind::Auth`) and never a
//! box: no option of it is read, so no policy can press one. A person signs
//! in by quitting and relaunching `agy`.
//!
//! **A TURN ENDS** on the reply, on `  ⎿  Interrupted · What should
//! Antigravity CLI do instead?` (Esc in the turn, or a box refused:
//! [`interrupted`]), or on `⚠ <message>` over `Error ID: <id>` when it failed
//! (the id's `-<n>` grows with the conversation's model calls, retries among
//! them: `-1` on a first turn, `-3` after a 503's two retries). A failure the
//! Gemini API answered
//! prints the SDK's error whole — `⚠ agent executor error: generating and
//! executing: Error 401, Message: …, Status: UNAUTHENTICATED, Details: […]`,
//! wrapped at column 0 — and is a WALL by its status ([`wall`], measured in
//! the lab against a local Gemini API stub, [`fixtures::lab`]): 401, and a
//! 400 whose ErrorInfo reason is `API_KEY_INVALID`, the login (`Auth`); a 429
//! saying `You exceeded your current quota` a quota no retry lifts soon
//! (`Spend`, as Codex's `quota exceeded` is; read [`Phase::Limited`]) —
//! but a per-minute one (its QuotaFailure's `quotaId` names `PerMinute`, and
//! no other names `PerDay` or `Daily`, which Google's own client reads as
//! the day's) a rate that lifts by itself, an API error 429; a 503 saying
//! `overloaded` an overload; any other status an API error by its code.
//! agy retries a 503, a
//! 504, a 429 with no RetryInfo and a 429 whose RetryInfo is short, eight
//! times under its spinner (no retry row draws), and a 500, a 400, a 403 and
//! a 404 not at all (measured in the lab: a 403 `PERMISSION_DENIED` with and
//! without its `SERVICE_DISABLED` reason, a 404 `NOT_FOUND` model, a 400
//! `FAILED_PRECONDITION`, a 504 `DEADLINE_EXCEEDED`, a bare 429
//! `RESOURCE_EXHAUSTED`, a per-minute 429 whose RetryInfo was 47 s — not
//! retried — and a per-day one of 41397 s: [`fixtures::lab`]). A 429's
//! RetryInfo prints whole (`map[@type:type.googleapis.com/google.rpc.RetryInfo
//! retryDelay:41397.204181s]`), the delay the API asks before a retry,
//! counted from its answer: the wall's reset is that delay as a span (`in
//! 41398s`), placed from the read — never earlier than the API's own time,
//! since the answer came before the screen showed it. A failure with no
//! status — the one measured signed in, `failed to construct executor: plan
//! model not specified`, came of a dead proxy — fits no wall kind and is an
//! ended turn with no wall.
//!
//! Three failures agy words itself are walls too (its binary's words,
//! UNMEASURED on the screen): `Our servers are experiencing high traffic …`
//! (`WallKind::Overloaded`), `You have exhausted your quota on this model.`
//! (`WallKind::UsageSession`, no reset named) and `Your AI credits balance is
//! too low to continue.` (`WallKind::Spend`), the last two read as
//! `Phase::Limited`.
//!
//! **A SUBAGENT'S ALERT** in the turn's rows (`<agent> needs approval for
//! <tool>`, `<agent> has a question`; its binary's words, UNMEASURED) says a
//! subagent waits on a person, who answers it in the subagent's own view: a
//! prompt with no option ([`prompt`]), which no policy can press.
//!
//! **THE SURVEY.** Unasked, and only sometimes (under an error turn of a
//! session some turns old, never under a first), agy replaces its composer
//! with ` How's the CLI experience so far? Help us improve:` over ` [1] Good
//! [2] Fine  [3] Bad  [0] Skip`, both at column 1, the footer under them. A
//! digit is a RATING, the person's; `0` skips it. It is no box and no composer
//! ([`survey`]): nothing is typed while it is up (a digit typed for a turn
//! would rate), and the supervisor dismisses it with `0` as it dismisses
//! Claude Code's rating cards, guarded on its options row.
//!
//! **FIRST RUN** with the Gemini API provider seeded (no sign-in): `Choose
//! your color scheme:` (eight schemes beside a preview, a `[Next]` button),
//! then `Terms of Service & Data Use` (a box ticked by default that lets
//! Google collect the person's interactions, `[Previous]` and `[Done]`), then
//! the trust box. The first two are BOXES OF NO KIND ([`PromptKind::Other`],
//! every option [`Role::Other`]): no policy presses one — the colour is the
//! person's taste, and the terms their consent.
//!
//! **UNDER AN API KEY** (the lab) the screens are the signed-in ones row for
//! row — the trust box, the composer, the boxes, the interrupt, the menu, the
//! resume line — but for the logo's second row (`Gemini API key` where the
//! account is), the model's name, and the spinner's word, which is the
//! model's thought (`⣽  tick 2...`) while it thinks.

use crate::anchors::{anchor_text, guard_regex};
use crate::phase::{Phase, leading_spaces, plain_rule_width};
use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};
use crate::survey::{SurveyBlock, SurveyKind};
use crate::text::blank;
use crate::turn::{CardPlace, LaunchCard};
use crate::wall::{ApiCause, Placement, Wall, WallKind};

/// The spinner's frames: braille patterns (`⣾`, `⣷`, `⡿`, … measured), the
/// blank one aside.
const SPINNER: std::ops::RangeInclusive<char> = '\u{2801}'..='\u{28FF}';

/// The spinner row (`is_spinner_row`) as the wire's regex: a frame in
/// column 0, two blanks, a word — what [`crate::reader::AgyReader`]'s busy
/// guard waits on to leave.
pub(crate) fn busy_guard() -> String {
    format!("^[{}-{}]\\s\\s\\S", SPINNER.start(), SPINNER.end())
}

/// The spinner row while a turn runs: `⣾  Generating...`, `⣟  Running
/// command...`, `⣷  Working...` — a braille frame in column 0, two blanks,
/// then the word.
fn is_spinner_row(row: &str) -> bool {
    let mut c = row.chars();
    c.next().is_some_and(|g| SPINNER.contains(&g))
        && c.next() == Some(' ')
        && c.next() == Some(' ')
        && c.next().is_some_and(|w| !w.is_whitespace())
}

/// A caret row: `>` in column 0, alone or before a blank.
fn is_caret_row(row: &str) -> bool {
    let b = row.as_bytes();
    b.first() == Some(&b'>') && b.get(1).is_none_or(|&c| c == b' ')
}

/// A person's message in the transcript: a caret row right under a rule (a
/// turn's separator). The `> Blue` an answered question leaves sits under
/// its `? …` row, and is none.
fn is_user_row(rows: &[String], i: usize) -> bool {
    i > 0 && is_caret_row(&rows[i]) && plain_rule_width(&rows[i - 1]).is_some()
}

/// The key row under a box's options: `  ↑/↓ Navigate · …` at column 2.
fn is_key_row(row: &str) -> bool {
    leading_spaces(row) == 2
        && crate::text::trim_start(row).starts_with(const { anchor_text("agy.keys.navigate") })
}

/// The key row whose rows end at `at` (module header: a narrow pane wraps
/// it, `  ↑/↓ Navigate · tab Amend` over `  f full diff`): in the unbroken
/// run of column-2 rows up from `at`, none of them an option, the one that
/// opens `↑/↓ Navigate`.
fn key_row_ending(rows: &[String], at: usize) -> Option<usize> {
    (0..=at)
        .rev()
        .take_while(|&i| leading_spaces(&rows[i]) == 2 && numbered(&rows[i]).is_none())
        .find(|&i| is_key_row(&rows[i]))
}

/// agy's word under the pending edit's key row after an Esc (module header):
/// `Esc disabled during file edits — press 1 to accept or 2 to reject.`
fn is_esc_disabled_row(row: &str) -> bool {
    crate::text::trim_start(row).starts_with(const { anchor_text("agy.edit.esc_disabled") })
}

/// A row of the `/` command menu (`> /add-dir   Add a directory …`, `  /agents
/// …`, `   ↓ 41 more`, and `↑ N more` over the rows once it scrolled).
fn is_menu_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    let t = t.strip_prefix("> ").unwrap_or(t);
    t.starts_with('/') || t.starts_with("↓ ") || t.starts_with("↑ ")
}

/// What the footer's left side says (module header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Footer {
    /// `? for shortcuts`: the composer empty and idle.
    Shortcuts,
    /// Nothing on the left, the model (or `(initializing...)`) on the right:
    /// a draft is held.
    Blank,
    /// `esc to cancel`: a turn runs, or a menu or a box is open. `Press ↑ to
    /// edit queued messages`: a turn runs with messages queued behind it.
    Cancel,
    /// `press ctrl+c again to exit`, `press ctrl+d again to exit`: one
    /// ctrl+c pressed at the idle composer (empty, or over a held draft,
    /// which stays) or one ctrl+d at the empty idle composer, the same key
    /// again ending agy (module header).
    ExitPending,
    /// Words this grammar has not measured.
    Other,
}

const FOOTERS: [(&str, Footer); 2] = [
    (anchor_text("agy.footer.idle"), Footer::Shortcuts),
    (anchor_text("agy.footer.cancel"), Footer::Cancel),
];

fn footer_of(row: &str) -> Footer {
    if leading_spaces(row) > 2 {
        // The right side alone: `Gemini 4 Argon · medium`, a model with no
        // effort badge, `(initializing...)`.
        return Footer::Blank;
    }
    let t = crate::text::trim(row);
    if is_queued_hint(t) {
        return Footer::Cancel;
    }
    if is_exit_hint(t) {
        return Footer::ExitPending;
    }
    FOOTERS
        .iter()
        .find(|(text, _)| t.starts_with(text))
        .map_or(Footer::Other, |&(_, f)| f)
}

/// `Press ↑ to edit queued messages`: the footer's left side while a turn
/// runs with messages queued behind it, its key filled in at run time.
fn is_queued_hint(t: &str) -> bool {
    t.strip_prefix("Press ")
        .and_then(|rest| rest.split_once(' '))
        .is_some_and(|(_, hint)| hint.starts_with(const { anchor_text("agy.footer.queued") }))
}

/// `press ctrl+c again to exit`, `press ctrl+d again to exit`: the footer's
/// left side while an exit is pending (the binary's `press %s again to
/// exit`), its key one word filled in at run time and the model's name on
/// the right after a gap.
fn is_exit_hint(t: &str) -> bool {
    t.strip_prefix("press ")
        .and_then(|rest| rest.split_once(' '))
        .and_then(|(key, hint)| {
            hint.strip_prefix(const { anchor_text("agy.footer.exit_again") })
                .map(|tail| (key, tail))
        })
        .is_some_and(|(key, tail)| !key.is_empty() && (tail.is_empty() || tail.starts_with(' ')))
}

/// The composer, read whole.
#[derive(Debug, Clone, Copy)]
struct Frame {
    top: usize,
    caret: usize,
    bottom: usize,
    /// The footer's row; `None` with nothing drawn under the frame (a draft
    /// held while no model is named leaves the footer blank).
    footer: Option<usize>,
    /// The `/` command menu is open under the bottom rule.
    menu: bool,
    /// The background-work panel's first and last rows, between the bottom
    /// rule and the rule that closes it ([`panel_under`]).
    panel: Option<(usize, usize)>,
}

/// A row of the background-work panel (module header): one item, `● ` two
/// columns in — `  ● [17:50:42] sleep 150 running`, `  ● Agent(research:
/// Lab Waiter)  Working · 1s` (measured: one row an item, never wrapped).
fn is_panel_row(row: &str) -> bool {
    leading_spaces(row) == 2 && crate::text::trim_start(row).starts_with("● ")
}

/// The background-work panel under the bottom rule `bottom` (module header):
/// one row of it or more ([`is_panel_row`]) right under the rule, closed by
/// a plain rule of the frame's `width`, and under that the footer as the
/// last row drawn (`last`) — or nothing, where nothing is drawn under the
/// closing rule. `(its first row, its last row, the footer)`; `None` for any
/// other shape.
fn panel_under(
    rows: &[String],
    bottom: usize,
    width: usize,
    last: usize,
) -> Option<(usize, usize, Option<usize>)> {
    let first = bottom + 1;
    let close = (first..=last).find(|&i| !is_panel_row(&rows[i]))?;
    if close == first || plain_rule_width(&rows[close]) != Some(width) {
        return None;
    }
    let footer = match last - close {
        0 => None,
        1 => Some(last),
        _ => return None,
    };
    Some((first, close - 1, footer))
}

/// The composer ([`Frame`]): the last caret row in column 0 between two
/// plain rules of one width (the transcript's separators are 60 columns, the
/// composer's the pane's), the rows between them its draft — a blank line of
/// it a blank row — and under it the footer as the last row drawn — or the
/// background-work panel, the rule closing it and then the footer
/// ([`panel_under`]), or the command menu, its key row and then the footer.
/// A screen agy left keeps its last frame with its resume line and the
/// shell's prompt under it, and has none.
fn frame(rows: &[String]) -> Option<Frame> {
    (1..rows.len())
        .rev()
        .filter(|&c| is_caret_row(&rows[c]))
        .find_map(|c| frame_at(rows, c))
}

fn frame_at(rows: &[String], caret: usize) -> Option<Frame> {
    let top = caret.checked_sub(1)?;
    let width = plain_rule_width(&rows[top])?;
    // A wrapped draft's rows are indented under the caret's text, and its
    // blank lines are blank rows: the bottom rule is the first row drawn
    // under the caret left of that indent.
    let bottom =
        (caret + 1..rows.len()).find(|&i| !blank(&rows[i]) && leading_spaces(&rows[i]) < 2)?;
    if plain_rule_width(&rows[bottom]) != Some(width) {
        return None;
    }
    let last = rows.iter().rposition(|r| !blank(r))?;
    let (footer, menu, panel) = if last == bottom {
        (None, false, None)
    } else if last == bottom + 1 {
        (Some(last), false, None)
    } else if let Some((first, end, footer)) = panel_under(rows, bottom, width, last) {
        (footer, false, Some((first, end)))
    } else {
        let between = &rows[bottom + 1..last];
        let menu = between.iter().any(|r| is_menu_row(r))
            && between
                .iter()
                .all(|r| blank(r) || is_menu_row(r) || is_key_row(r));
        if !menu {
            return None;
        }
        (Some(last), true, None)
    };
    Some(Frame {
        top,
        caret,
        bottom,
        footer,
        menu,
        panel,
    })
}

/// What [`background_wait`] names: a command task or a timer
/// (`● [17:50:42] <what> running`, `· 1 task(s)`), and a subagent
/// (`● Agent(research: …)  Working · 1s`, `· 1 subagent(s)`).
const TASK_RUNNING: &str = "a background task running";
const SUBAGENT_WORKING: &str = "a subagent working";

/// The work one panel row says runs (module header): a task's or a
/// timer's `● [hh:mm:ss] <what> running`, a subagent's `● Agent(<kind>:
/// <name>)  Working · <time>`. `None` for any other row.
fn panel_work(row: &str) -> Option<&'static str> {
    let item = crate::text::trim(row).strip_prefix("● ")?;
    if let Some((stamp, what)) = item
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
    {
        let clock = stamp.len() == 8
            && stamp.bytes().enumerate().all(|(i, b)| {
                if i % 3 == 2 {
                    b == b':'
                } else {
                    b.is_ascii_digit()
                }
            });
        return (clock
            && what.ends_with(const { anchor_text("agy.panel.running") })
            && what.len() > const { anchor_text("agy.panel.running") }.len())
        .then_some(TASK_RUNNING);
    }
    let (_, state) = item
        .strip_prefix(const { anchor_text("agy.panel.agent") })?
        .rsplit_once(')')?;
    crate::text::trim_start(state)
        .starts_with(const { anchor_text("agy.panel.working") })
        .then_some(SUBAGENT_WORKING)
}

/// The work the footer counts (module header): a segment `<n> task(s)` or
/// `<n> subagent(s)` of its right side, `<n>` a count above zero.
fn footer_work(row: &str) -> Option<&'static str> {
    crate::text::trim(row).split(" · ").find_map(|segment| {
        let segment = crate::text::trim(segment);
        let counted = |noun: &str| {
            segment.strip_suffix(noun).is_some_and(|n| {
                !n.is_empty()
                    && n.bytes().all(|b| b.is_ascii_digit())
                    && n.bytes().any(|b| b != b'0')
            })
        };
        if counted(const { anchor_text("agy.footer.tasks") }) {
            Some(TASK_RUNNING)
        } else if counted(const { anchor_text("agy.footer.subagents") }) {
            Some(SUBAGENT_WORKING)
        } else {
            None
        }
    })
}

/// The work frame `f` says runs in the background: a panel row's
/// ([`panel_work`]), else the footer's count ([`footer_work`]).
fn work(rows: &[String], f: Frame) -> Option<&'static str> {
    f.panel
        .and_then(|(first, last)| rows[first..=last].iter().find_map(|r| panel_work(r)))
        .or_else(|| f.footer.and_then(|r| footer_work(&rows[r])))
}

/// THE AGENT'S OWN BACKGROUND WORK RUNS AND ITS TURN IS OVER (module header):
/// the composer drawn with no menu open, the turn over it ended — idle (on
/// a wall too), a question or a limit, as evidence ([`framed_phase`]) — and
/// the panel under it (or the footer's count) says a task, a
/// timer or a subagent runs. `Some(what runs)`; `None` for a live turn (a
/// spinner over the panel), a box, an exit pending, a screen agy left (its
/// last frame's panel rows under its resume line), and where nothing runs.
#[must_use]
pub fn background_wait(rows: &[String]) -> Option<&'static str> {
    let f = frame(rows)?;
    if f.menu {
        return None;
    }
    let what = work(rows, f)?;
    matches!(
        framed_phase(rows, f),
        (Phase::Idle | Phase::Question | Phase::Limited { .. }, true)
    )
    .then_some(what)
}

/// What [`background_shown`] names for a panel row whose words this grammar
/// has not measured (a subagent's `Processing...` among them).
const BACKGROUND_ITEM: &str = "a background item";

/// THE AGENT'S OWN BACKGROUND WORK THE SCREEN SHOWS, WHATEVER ITS TURN DOES:
/// the composer's frame found, and under its bottom rule a panel row of ANY
/// item — a task's or a timer's `● [hh:mm:ss] … running`, a subagent's `●
/// Agent(…)` in any state (`Working`, and `Processing...` as it starts:
/// measured), a row this grammar never measured — else the footer's `<n>
/// task(s)` or `<n> subagent(s)` (all that shows with the panel turned off,
/// `hideActiveItems`: measured). The live upgrade's quiet restart reads it
/// beside the reader's own reading ([`phase`] reads work over an ended turn
/// busy), so that this witness never rests on the reader's phase: a reader fix
/// that read the frame idle over the panel would not remove it. `Some(what
/// shows)`; `None` where no frame is found (a box, a screen agy left: no idle
/// point either) and where nothing shows.
#[must_use]
pub fn background_shown(rows: &[String]) -> Option<&'static str> {
    let f = frame(rows)?;
    if let Some((first, last)) = f.panel {
        return Some(
            rows[first..=last]
                .iter()
                .find_map(|r| panel_work(r))
                .unwrap_or(BACKGROUND_ITEM),
        );
    }
    f.footer.and_then(|r| footer_work(&rows[r]))
}

/// The composer's caret row.
#[must_use]
pub fn composer(rows: &[String]) -> Option<usize> {
    frame(rows).map(|f| f.caret)
}

fn draft(rows: &[String], f: Frame) -> Vec<String> {
    let caret = rows[f.caret].strip_prefix('>').unwrap_or(&rows[f.caret]);
    let lines: Vec<String> = std::iter::once(crate::text::trim(caret))
        .chain(
            rows[f.caret + 1..f.bottom]
                .iter()
                .map(|r| crate::text::trim(r)),
        )
        .map(str::to_string)
        .collect();
    match lines.as_slice() {
        [one] if is_mode_hint(one) => vec![String::new()],
        _ => lines,
    }
}

/// agy's permission modes' hints (module header), drawn in the EMPTY
/// composer while a mode is on.
const MODE_HINTS: [&str; 4] = [
    anchor_text("agy.mode.hint.accept_edits"),
    anchor_text("agy.mode.hint.auto"),
    anchor_text("agy.mode.hint.plan"),
    anchor_text("agy.mode.hint.best_of_n"),
];

/// Whether the caret row's `text` is a mode's hint: whole, or cut with `...`
/// at the pane's edge while the cut still holds its ` mode: `.
fn is_mode_hint(text: &str) -> bool {
    MODE_HINTS.iter().any(|hint| {
        text == *hint
            || text
                .strip_suffix("...")
                .is_some_and(|cut| hint.starts_with(cut) && cut.contains(" mode: "))
    })
}

/// The composer's text as `(caret row, lines)`: the caret row's text first
/// (its `>` stripped — empty when nothing is typed, and when all it shows is
/// a permission mode's hint: agy draws no other placeholder), then each row
/// its draft wrapped onto, a blank line of it empty.
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let f = frame(rows)?;
    Some((f.caret, draft(rows, f)))
}

/// The last person's message above the composer's top rule.
fn last_user(rows: &[String], top: usize) -> Option<usize> {
    (1..top).rev().find(|&i| is_user_row(rows, i))
}

/// THE ROWS WHOSE CELLS BOUND THE LAST MESSAGE (module header's A
/// MESSAGE'S BLANK LINE): `(its caret row, the blank rows under it that have
/// words under them above the composer's top rule, top down)`. Each blank
/// row is the message's own where its column-0 cell is drawn in the caret
/// row's style; the first that is not ends the message, and the rows under
/// it are the reply. `None` with no composer, while a turn runs, with no
/// message on `rows` (its caret row under its rule, [`is_user_row`]), and
/// with no blank row under it that words follow.
#[must_use]
pub fn message_blanks(rows: &[String]) -> Option<(usize, Vec<usize>)> {
    let f = frame(rows)?;
    if spinning(rows, f.top) {
        return None;
    }
    let user = last_user(rows, f.top)?;
    let last = (user + 1..f.top).rev().find(|&i| !blank(&rows[i]))?;
    let blanks: Vec<usize> = (user + 1..last).filter(|&i| blank(&rows[i])).collect();
    (!blanks.is_empty()).then_some((user, blanks))
}

/// THE LAST MESSAGE BOUNDED BY ITS CELLS (module header): `rows` with the
/// blank rows `inner` of the message whose caret row is `caret` — those of
/// [`message_blanks`] a read of the cells found drawn in the caret row's
/// style — taken out, and as many empty rows put above the rule over the
/// message. Its rows then meet, the blank row that ends it is the first
/// under them, and that row and every row under it (the reply, the
/// composer, the footer) keep their index; the rows above the rule keep
/// theirs too. `rows` as they are where `inner` names no blank row under
/// `caret`, or `caret` is no message's.
#[must_use]
pub fn fold_message(rows: &[String], caret: usize, inner: &[usize]) -> Vec<String> {
    let folds = |i: usize| i > caret && inner.contains(&i) && blank(&rows[i]);
    if !is_user_row(rows, caret) || !(caret + 1..rows.len()).any(folds) {
        return rows.to_vec();
    }
    let n = (caret + 1..rows.len()).filter(|&i| folds(i)).count();
    let mut out = Vec::with_capacity(rows.len());
    out.extend(rows[..caret - 1].iter().cloned());
    out.extend(std::iter::repeat_n(String::new(), n));
    out.extend(
        rows.iter()
            .enumerate()
            .skip(caret - 1)
            .filter(|&(i, _)| !folds(i))
            .map(|(_, r)| r.clone()),
    );
    out
}

/// Whether a turn runs: a spinner row among the turn's rows above the
/// composer's top rule (under the last message, or from the top with it
/// scrolled away) — right above the rule, above the `└ Tip:` rows under it,
/// or above any other row drawn there.
fn spinning(rows: &[String], top: usize) -> bool {
    let from = last_user(rows, top).map_or(0, |u| u + 1);
    rows[from..top].iter().any(|r| is_spinner_row(r))
}

/// A subagent's alert (module header) among the turn's rows above the
/// composer's top rule: `<agent> needs approval for <tool>`, `<agent> has a
/// question`.
fn alert(rows: &[String], top: usize) -> Option<usize> {
    const ALERTS: [&str; 2] = [
        anchor_text("agy.subagent.approval"),
        anchor_text("agy.subagent.question"),
    ];
    let from = last_user(rows, top).map_or(0, |u| u + 1);
    // agy's own rows only: its reply's rows sit two columns in, and a reply
    // that says someone `has a question` is no alert.
    (from..top).rev().find(|&i| {
        let t = crate::text::trim(&rows[i]);
        leading_spaces(&rows[i]) < 2 && ALERTS.iter().any(|a| crate::text::contains(t, a))
    })
}

/// agy's logo row naming its build: `Antigravity CLI 1.4.0`.
fn is_banner_row(row: &str) -> bool {
    let name = const { anchor_text("agy.banner") };
    crate::text::find(row, name).is_some_and(|at| {
        row[at + name.len()..]
            .strip_prefix(' ')
            .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// The phase, and whether it is evidence (module header): a subagent's
/// alert in the turn is a prompt, its spinner running or not; the spinner in
/// the turn over the composer is busy; the composer with `? for shortcuts`
/// (or with a draft and nothing on the footer's left) is idle — limited when
/// the turn ended on a quota, rate, usage or spend wall
/// ([`WallKind::reads_limited`]), a question when the reply's last words end
/// in `?`; a box, and a first run's onboarding screen, is a prompt; the
/// sign-in wall is idle on its wall. An exit pending (`press ctrl+c again to
/// exit`, `press ctrl+d again to exit`) is idle NOT authoritatively: agy is
/// one key from gone, and nothing is typed into it (module header). `esc to
/// cancel` (or the queued messages' hint) with no spinner and no menu is
/// busy NOT authoritatively (a turn that has not drawn its spinner yet, or
/// an overlay this grammar has not measured), and with the command menu open
/// nothing is evidence: the menu takes the footer's place. A screen with no
/// composer, box or wall — the survey's among them — is `idle` NOT
/// authoritatively. An ended turn that would read idle or a question, with
/// no wall, while the agent's own background work runs ([`background_wait`]:
/// a task, a timer, a subagent, each of which ends and wakes agy) is BUSY as
/// evidence, as Claude Code's reader reads a shell, a workflow or a
/// background agent it left running (`phase::worker_phase`'s hard rule); a
/// limit and a wall the turn ended on stand over it.
#[must_use]
pub fn phase(rows: &[String]) -> (Phase, bool) {
    let Some(f) = frame(rows) else {
        if agy_box(rows).is_some() || onboarding(rows).is_some() {
            return (Phase::Prompt, true);
        }
        return (Phase::Idle, signed_out(rows).is_some());
    };
    let read = framed_phase(rows, f);
    if matches!(read, (Phase::Idle | Phase::Question, true))
        && work(rows, f).is_some()
        && failed_turn(rows, f).is_none()
    {
        return (Phase::Busy, true);
    }
    read
}

/// [`phase`] of a screen with the composer's frame `f`, its background work
/// aside.
fn framed_phase(rows: &[String], f: Frame) -> (Phase, bool) {
    // A subagent waiting on a person holds the turn, however long its
    // spinner runs.
    if alert(rows, f.top).is_some() {
        return (Phase::Prompt, true);
    }
    if spinning(rows, f.top) {
        return (Phase::Busy, true);
    }
    let footer = f.footer.map_or(Footer::Blank, |r| footer_of(&rows[r]));
    let idle = match footer {
        Footer::Shortcuts => true,
        Footer::Blank => draft(rows, f).iter().any(|l| !l.is_empty()),
        Footer::Cancel if !f.menu => return (Phase::Busy, false),
        Footer::Cancel | Footer::ExitPending | Footer::Other => false,
    };
    if !idle || f.menu {
        return (Phase::Idle, false);
    }
    if let Some(w) = failed_turn(rows, f).filter(|w| w.kind.reads_limited()) {
        return (
            Phase::Limited {
                message: w.message,
                reset: w.reset,
            },
            true,
        );
    }
    let asks = said_tail(rows).is_some_and(|t| crate::turn::asks(&t));
    (if asks { Phase::Question } else { Phase::Idle }, true)
}

/// No turn yet: the logo naming the build above the composer and no message
/// under it.
#[must_use]
pub fn fresh(rows: &[String]) -> bool {
    frame(rows).is_some_and(|f| {
        last_user(rows, f.top).is_none() && rows[..f.top].iter().any(|r| is_banner_row(r))
    })
}

/// A MESSAGE STANDS ON `rows` ([`crate::ScreenReader::message_drawn`]): a
/// person's message (its caret row under its rule, [`is_user_row`]) above
/// the composer's top rule (anywhere where no composer is drawn), answered
/// or not.
#[must_use]
pub fn message_drawn(rows: &[String]) -> bool {
    let top = frame(rows).map_or(rows.len(), |f| f.top);
    last_user(rows, top).is_some()
}

/// A reply's row: two columns in or more, and none of agy's own marks (`⎿`
/// under a notice or a refusal, `└` under the spinner).
fn is_reply_row(row: &str) -> bool {
    leading_spaces(row) >= 2 && !crate::text::trim_start(row).starts_with(['⎿', '└']) && !blank(row)
}

/// A row of the notice card agy draws over the composer (module header:
/// ` ┃ Meet Gemini 4 Argon` … ` ┃ esc to dismiss`): its bar in column 1.
fn is_card_row(row: &str) -> bool {
    leading_spaces(row) == 1 && crate::text::trim_start(row).starts_with('┃')
}

/// The agent's last words: the reply under the last message, one line per
/// row, its indent stripped — the visible rows from the top when the
/// message scrolled away — under any notice card over the composer. `None`
/// while a turn runs, before any message, and when the turn ended on agy's
/// own rows (an interrupt, an error, a tool row and its output).
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    said_reply(rows).map(|(_, said)| said)
}

/// WHETHER THE REPLY [`said_tail`] READS IS WHOLE ON `rows`: the row above
/// its first is one of them and no part of it — the person's message it
/// answers, or agy's own row. `false` for a reply whose rows reach the top
/// of `rows` (taller than them: its tail is then the rows in sight, and what
/// stood above them — a need, an open item — is not in it), and for one
/// under a caret row on the top row, whose rule above (what makes it a
/// person's message, [`is_user_row`]) is not on `rows`: the rows the
/// message wrapped onto, two columns in as a reply's, read as the reply's.
/// `false` too where there are no last words.
#[must_use]
pub fn said_whole(rows: &[String]) -> bool {
    said_reply(rows).is_some_and(|(head, _)| head > 1 || (head == 1 && !is_caret_row(&rows[0])))
}

/// A row a read of the history above the screen stops at, for a reply
/// [`said_whole`] finds cut: one that opens in column 0 with words, but no
/// caret row — the rule above a person's message is, which makes it theirs
/// ([`is_user_row`]).
#[must_use]
pub fn said_history_stop(row: &str) -> bool {
    !row.is_empty() && !row.starts_with(char::is_whitespace) && !is_caret_row(row)
}

/// THE AGENT'S OTHER WORDS IN THE TURN ITS LAST WORDS END ([`said_tail`]):
/// every reply of the same turn above its last — the words it said before a
/// tool call, say — oldest first, each as [`said_tail`] reads one (its
/// indent stripped, one line per row), joined by a blank line. A reply is a
/// run of rows two columns in, blank rows among them, between agy's own
/// rows in column 0 (a tool's `●` row, a thought's `○` row, a box's
/// remains); a run drawn right under a `●` or `○` row is that row's output.
/// The turn is the rows under the person's last message, or from the rows'
/// top where it is above them. `None` where the turn holds no other reply
/// on `rows`, or there are no last words.
#[must_use]
pub fn turn_said_before(rows: &[String]) -> Option<String> {
    let (_, said) = turn_before(rows)?;
    (!said.is_empty()).then(|| said.join("\n\n"))
}

/// WHETHER THE TURN [`said_tail`]'s REPLY ENDS IS WHOLE ON `rows`: the reply
/// is ([`said_whole`]), and so is the person's message that opened the turn
/// (its caret row under its rule, [`is_user_row`]). `false` where the turn
/// reaches the rows' top: an earlier reply of the turn, and a need said in
/// it, may stand above them.
#[must_use]
pub fn turn_whole(rows: &[String]) -> bool {
    said_whole(rows) && turn_before(rows).is_some_and(|(opened, _)| opened)
}

/// A row a read of the history above the screen stops at, for a turn
/// [`turn_whole`] finds cut: a plain rule, the turn's separator over the
/// person's message that opened it, read walking up — the message is under
/// it.
#[must_use]
pub fn turn_history_stop(row: &str) -> bool {
    plain_rule_width(row).is_some()
}

/// THE MESSAGE THAT OPENED THE TURN [`said_tail`]'s REPLY ENDS: the person's
/// last message (its caret row under its rule, [`is_user_row`], the `>`
/// stripped) and the rows it wrapped onto, up to the blank row under it,
/// joined by a space. `None` where there are no last words, and where that
/// message is not on `rows`.
#[must_use]
pub fn turn_opened_by(rows: &[String]) -> Option<String> {
    said_reply(rows)?;
    let f = frame(rows)?;
    let user = last_user(rows, f.top)?;
    let end = (user + 1..f.top)
        .find(|&i| blank(&rows[i]))
        .unwrap_or(f.top);
    let text: Vec<&str> = std::iter::once(rows[user].get(1..).unwrap_or(""))
        .chain(rows[user + 1..end].iter().map(String::as_str))
        .map(crate::text::trim)
        .filter(|t| !t.is_empty())
        .collect();
    Some(text.join(" "))
}

/// The turn [`turn_said_before`] reads: whether the person's message that
/// opened it is on `rows` ([`turn_whole`]), and the words of its other
/// replies, oldest first. `None` where there are no last words.
fn turn_before(rows: &[String]) -> Option<(bool, Vec<String>)> {
    let f = frame(rows)?;
    let (head, _) = said_reply(rows)?;
    let user = last_user(rows, f.top);
    let from = user.map_or(0, |u| {
        (u + 1..f.top).find(|&i| blank(&rows[i])).unwrap_or(f.top)
    });
    let mut said = Vec::new();
    let mut at = from;
    while at < head {
        if !is_reply_row(&rows[at]) {
            at += 1;
            continue;
        }
        // A run of reply rows and blank rows, up to agy's next own row.
        let start = at;
        let mut last = at;
        while at < head && (is_reply_row(&rows[at]) || blank(&rows[at])) {
            if !blank(&rows[at]) {
                last = at;
            }
            at += 1;
        }
        // A tool row's own output is drawn right under it.
        let output = start > 0 && crate::text::trim_start(&rows[start - 1]).starts_with(['●', '○']);
        if !output {
            let words: Vec<&str> = rows[start..=last]
                .iter()
                .map(|r| crate::text::trim(r))
                .filter(|t| !t.is_empty())
                .collect();
            said.push(words.join("\n"));
        }
    }
    Some((user.is_some(), said))
}

/// The reply [`said_tail`] reads, as `(the row its walk up stopped at — the
/// first under the row that bounds it, 0 where nothing on the rows does —,
/// its words)`.
fn said_reply(rows: &[String]) -> Option<(usize, String)> {
    let f = frame(rows)?;
    if spinning(rows, f.top) {
        return None;
    }
    let user = last_user(rows, f.top);
    if user.is_none() && rows[..f.top].iter().any(|r| is_banner_row(r)) {
        return None;
    }
    // Past the message and the rows it wrapped onto.
    let from = user.map_or(0, |u| {
        (u + 1..f.top).find(|&i| blank(&rows[i])).unwrap_or(f.top)
    });
    let last = (from..f.top)
        .rev()
        .find(|&i| !blank(&rows[i]) && !is_card_row(&rows[i]))?;
    if !is_reply_row(&rows[last]) {
        return None;
    }
    let mut head = last;
    while head > from && (is_reply_row(&rows[head - 1]) || blank(&rows[head - 1])) {
        head -= 1;
    }
    let first = (head..=last).find(|&i| !blank(&rows[i]))?;
    // A tool row's own output is drawn right under it: no words of the agent.
    if first > 0
        && first == head
        && crate::text::trim_start(&rows[first - 1]).starts_with(['●', '○'])
    {
        return None;
    }
    let out: Vec<&str> = rows[first..=last]
        .iter()
        .map(|r| crate::text::trim(r))
        .filter(|t| !t.is_empty())
        .collect();
    Some((head, out.join("\n")))
}

/// Whether a PERSON stopped the last turn: under the last message, agy's `⎿
/// Interrupted · What should Antigravity CLI do instead?` — Esc in the turn,
/// or a box refused (`⎿  User declined the tool call` above it). The next
/// message is theirs.
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    let Some(f) = frame(rows) else {
        return false;
    };
    let from = last_user(rows, f.top).map_or(0, |u| u + 1);
    rows[from..f.top].iter().any(|r| {
        crate::text::trim_start(r)
            .strip_prefix('⎿')
            .is_some_and(|rest| {
                crate::text::trim_start(rest)
                    .starts_with(const { anchor_text("agy.turn.interrupted") })
            })
    })
}

/// The sign-in wall's row (`Welcome to the Antigravity CLI. You are currently
/// not signed in.`): drawn instead of a composer, `Select login method:`
/// under it and its key row the last row drawn.
fn signed_out(rows: &[String]) -> Option<usize> {
    let end = rows.iter().rposition(|r| !blank(r))?;
    if !crate::text::trim_start(&rows[end]).starts_with(const { anchor_text("agy.keys.navigate") })
    {
        return None;
    }
    let at = rows[..end]
        .iter()
        .rposition(|r| crate::text::contains(r, const { anchor_text("agy.signin.wall") }))?;
    let method = rows[at + 1..end]
        .iter()
        .any(|r| crate::text::trim(r) == const { anchor_text("agy.signin.method") });
    (method && frame(rows).is_none()).then_some(at)
}

/// The wall on the screen (module header): agy signed out (`WallKind::Auth`;
/// a person's browser lifts it, and its login menu is never read), or the
/// Gemini API's failure the last turn ended on (`failed_turn`).
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    signed_out(rows)
        .map(|row| Wall::sign_in(rows, row))
        .or_else(|| frame(rows).and_then(|f| failed_turn(rows, f)))
}

/// A failed turn's `⚠` block as `(its head row, its Error ID row)`: the last
/// thing above the composer, under the last message — its head `⚠ ` in column
/// 0, the rows it wrapped onto in column 0 under it, then `Error ID: <id>`.
/// `None` when anything else ended the turn (a reply under the error, a newer
/// message).
fn error_block(rows: &[String], f: Frame) -> Option<(usize, usize)> {
    let from = last_user(rows, f.top).map_or(0, |u| u + 1);
    let id = (from..f.top).rev().find(|&i| !blank(&rows[i]))?;
    if !rows[id].starts_with(const { anchor_text("agy.error.id") }) {
        return None;
    }
    (from..id)
        .rev()
        .take_while(|&i| !blank(&rows[i]) && leading_spaces(&rows[i]) == 0)
        .find(|&i| rows[i].starts_with("⚠ "))
        .map(|head| (head, id))
}

/// The status the Gemini API answered, where agy printed the genai SDK's
/// error (`Error %d, Message: %s, Status: %s, Details: %v`, a format string
/// of the 1.4.0 binary): `… Error 401, Message: …` → 401.
fn api_status(message: &str) -> Option<u16> {
    message.match_indices("Error ").find_map(|(at, w)| {
        let rest = &message[at + w.len()..];
        let code = rest.get(..3)?;
        if !code.bytes().all(|b| b.is_ascii_digit()) || !rest[3..].starts_with(", Message: ") {
            return None;
        }
        code.parse().ok()
    })
}

/// The Gemini API's own words in a failed turn's block that name its kind
/// beyond the status (module header), read lowercased. They are the
/// SERVER's — Google's documented error body, which agy prints whole, and
/// which the lab's stub answers in — never agy's: no canary can find them in
/// its binary.
const API_KEY_INVALID: &str = "api_key_invalid";
const QUOTA_EXCEEDED: &str = "exceeded your current quota";
const OVERLOADED: &str = "overloaded";

/// The failures agy words itself that are walls (module header), by the
/// words their message opens with.
const OWN_WALLS: [(&str, WallKind); 3] = [
    (anchor_text("agy.wall.overloaded"), WallKind::Overloaded),
    (anchor_text("agy.wall.quota"), WallKind::UsageSession),
    (anchor_text("agy.wall.credits"), WallKind::Spend),
];

/// `message` with every blank taken out: the SDK's error wraps at column 0
/// mid-token (`gemini-3.1-pro- preview` once its rows are joined), so a
/// token is read off this.
fn squeezed(message: &str) -> String {
    message.chars().filter(|c| !c.is_whitespace()).collect()
}

/// A 429's QuotaFailure names a per-minute quota (`quotaId:Generate
/// RequestsPerMinutePerProjectPerModel-FreeTier`) and no per-day one: a rate
/// window that lifts within a minute, not a spent quota. A violation of a
/// per-day quota (`PerDay`, `Daily`) beside it makes the answer the day's,
/// as Google's own client reads it (Gemini CLI 0.46.0's
/// `classifyGoogleError` returns its terminal quota error for such a
/// violation before any other test). Each id is read off the squeezed
/// message up to the next key (the SDK prints a map's keys in order:
/// `quotaId` before `quotaMetric`) or the map's end.
fn per_minute_quota(message: &str) -> bool {
    let s = squeezed(message);
    let ids: Vec<&str> = s
        .match_indices("quotaId:")
        .map(|(at, w)| {
            let rest = &s[at + w.len()..];
            let end = rest
                .find("quota")
                .into_iter()
                .chain(rest.find(']'))
                .min()
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .collect();
    ids.iter().any(|id| id.contains("PerMinute"))
        && !ids
            .iter()
            .any(|id| id.contains("PerDay") || id.contains("Daily"))
}

/// The wall a failed turn's message is (module header): one agy words
/// itself ([`OWN_WALLS`]), else by the status the API answered; `None` with
/// neither in it.
fn failure_kind(message: &str) -> Option<WallKind> {
    if let Some(&(_, kind)) = OWN_WALLS
        .iter()
        .find(|(words, _)| message.starts_with(words))
    {
        return Some(kind);
    }
    let code = api_status(message)?;
    let lower = message.to_lowercase();
    Some(match code {
        401 => WallKind::Auth,
        400 if lower.contains(API_KEY_INVALID) => WallKind::Auth,
        429 if lower.contains(QUOTA_EXCEEDED) && !per_minute_quota(message) => WallKind::Spend,
        503 if lower.contains(OVERLOADED) => WallKind::Overloaded,
        code => WallKind::ApiError {
            code: Some(code),
            // agy's own rule, measured: a 503, a 504 and a 429 are retried
            // under its spinner; a 500, a 400, a 401, a 403 and a 404 are not.
            retryable: matches!(code, 429 | 503 | 504),
            cause: ApiCause::Server,
        },
    })
}

/// The delay a 429's RetryInfo asks for (`retryDelay:41397.204181s`, read
/// off the squeezed message) as a span from now, whole seconds rounded up:
/// `in 41398s`. `None` with no RetryInfo, or one this does not read.
fn retry_delay(message: &str) -> Option<String> {
    let s = squeezed(message);
    let (at, w) = s.match_indices("retryDelay:").last()?;
    let rest = &s[at + w.len()..];
    let whole: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[whole.len()..];
    let frac: String = after
        .strip_prefix('.')
        .map(|f| f.chars().take_while(char::is_ascii_digit).collect())
        .unwrap_or_default();
    let unit = &after[if frac.is_empty() { 0 } else { frac.len() + 1 }..];
    if whole.is_empty() || !unit.starts_with('s') {
        return None;
    }
    let secs: u64 = whole.parse().ok()?;
    let up = u64::from(frac.bytes().any(|b| b != b'0'));
    Some(format!("in {}s", secs.checked_add(up)?))
}

/// The wall the last turn ended on when the Gemini API failed it
/// ([`error_block`], [`failure_kind`]): its message the block's rows joined,
/// the `⚠` and the `Error ID:` row left out — the vendor's row under the last
/// thing said, so [`Placement::Banner`]. Its reset is a 429's RetryInfo
/// delay as a span ([`retry_delay`], module header).
fn failed_turn(rows: &[String], f: Frame) -> Option<Wall> {
    let (head, id) = error_block(rows, f)?;
    let message = rows[head..id]
        .iter()
        .map(|r| crate::text::trim(r.strip_prefix("⚠ ").unwrap_or(r)))
        .collect::<Vec<_>>()
        .join(" ");
    Some(Wall {
        kind: failure_kind(&message)?,
        reset: retry_delay(&message),
        message,
        row: head,
        placement: Placement::Banner,
        under_call: false,
    })
}

/// The feedback survey (module header), as a rating card
/// ([`SurveyBlock`]): no composer drawn, and as the last rows drawn — the
/// footer under them or not — the question row and the options row, both at
/// column 1, the options opening `[1] ` and ending on `[0] Skip`. The `0`
/// that skips it is guarded on the options row as the screen shows it (its
/// question carries an ASCII `'`, which the manager's single-quoted command
/// cannot): a copy quoted in a reply or a message sits at another column and
/// matches no guard.
#[must_use]
pub fn survey(rows: &[String]) -> Option<SurveyBlock> {
    if frame(rows).is_some() {
        return None;
    }
    let mut options = rows.iter().rposition(|r| !blank(r))?;
    if footer_of(&rows[options]) != Footer::Other {
        options = (0..options).rev().find(|&i| !blank(&rows[i]))?;
    }
    let head = options.checked_sub(1)?;
    let row = crate::text::trim_end(&rows[options]);
    let said = crate::text::trim(row);
    let card = leading_spaces(row) == 1
        && said.starts_with("[1] ")
        && said.ends_with(const { anchor_text("agy.survey.skip") })
        && leading_spaces(&rows[head]) == 1
        && crate::text::trim(&rows[head]) == const { anchor_text("agy.survey.question") };
    card.then(|| SurveyBlock {
        kind: SurveyKind::Session,
        head,
        options,
        guard: format!("^{}", guard_regex(row)),
        dismissable: true,
    })
}

/// Whether `rows` are agy's by their layout alone, for a session whose
/// program name says nothing: its composer over a footer it draws, one of
/// its boxes, a first run's onboarding screen, its survey, or its sign-in
/// wall.
pub(crate) fn is_agy_screen(rows: &[String]) -> bool {
    frame(rows).is_some_and(|f| {
        f.footer.is_some_and(|r| match footer_of(&rows[r]) {
            Footer::Other => false,
            Footer::Blank => crate::text::contains(&rows[r], " · "),
            Footer::Shortcuts | Footer::Cancel | Footer::ExitPending => true,
        })
    }) || agy_box(rows).is_some()
        || onboarding(rows).is_some()
        || survey(rows).is_some()
        || signed_out(rows).is_some()
}

// The boxes.

/// A box's rows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AgyBox {
    /// Its title row; `None` with its head off the rows read.
    title: Option<usize>,
    /// The trust box's question row (`Do you trust the contents of this
    /// project?`), which names it.
    trust: Option<usize>,
    /// Its first option row and its last row of options (a wrapped tail).
    first: usize,
    last: usize,
    /// Its key row, and the last row the key row wrapped onto.
    keys: usize,
    keys_end: usize,
    /// The footer's row under it; `None` with the key row the last drawn.
    footer: Option<usize>,
}

/// `> 1. Yes, run command` / `  2. …` → `(1, "Yes, run command", focused)`.
fn numbered(row: &str) -> Option<(u8, &str, bool)> {
    crate::prompt::numbered_option(row, "> ", ". ")
}

/// The trust box's `> Yes, I trust this folder` / `  No, exit`.
fn unnumbered(row: &str) -> Option<(&str, bool)> {
    let (focused, rest) = match row.strip_prefix("> ") {
        Some(rest) => (true, rest),
        None => (false, row.strip_prefix("  ")?),
    };
    (!rest.starts_with(char::is_whitespace) && !rest.is_empty())
        .then(|| (crate::text::trim(rest), focused))
}

/// The key row a box or a screen of choices ends on: the last row drawn, or
/// the row over the footer's `esc to cancel` (or the model alone).
fn key_row(rows: &[String]) -> Option<usize> {
    let end = rows.iter().rposition(|r| !blank(r))?;
    if is_key_row(&rows[end]) {
        return Some(end);
    }
    if !matches!(footer_of(&rows[end]), Footer::Cancel | Footer::Blank) {
        return None;
    }
    let k = (0..end).rev().find(|&i| !blank(&rows[i]))?;
    is_key_row(&rows[k]).then_some(k)
}

/// The box on `rows` (module header): no composer, its key row — and the
/// rows it wrapped onto — the last drawn or over the footer's `esc to
/// cancel` (or the right side alone), the pending edit's word after an Esc
/// under it or not, its options the block of rows above it with one of them
/// focused in column 0.
pub(crate) fn agy_box(rows: &[String]) -> Option<AgyBox> {
    let end = rows.iter().rposition(|r| !blank(r))?;
    let footer = matches!(footer_of(&rows[end]), Footer::Cancel | Footer::Blank).then_some(end);
    let keys_end = (0..=end)
        .rev()
        .find(|&i| !blank(&rows[i]) && Some(i) != footer && !is_esc_disabled_row(&rows[i]))?;
    let keys = key_row_ending(rows, keys_end)?;
    let last = (0..keys).rev().find(|&i| !blank(&rows[i]))?;
    let mut top = last;
    while top > 0 && !blank(&rows[top - 1]) {
        top -= 1;
    }
    let block = top..=last;
    // A label that holds a multi-line command wraps onto rows of its own,
    // blank ones among them (measured: `2. Yes, and always allow … for
    // commands that start with 'python3 -c '` over the script's lines): the
    // options run from their `1.` row down, found above the blanks, never past
    // the rule under the box's title.
    let one = (0..=last)
        .rev()
        .take_while(|&i| plain_rule_width(&rows[i]).is_none())
        .find(|&i| numbered(&rows[i]).is_some_and(|(n, _, _)| n == 1));
    let first = match one.or_else(|| block.clone().find(|&i| numbered(&rows[i]).is_some())) {
        Some(first) => first,
        // Unnumbered (the trust box): every row of the block an option.
        None if block.clone().all(|i| unnumbered(&rows[i]).is_some()) => top,
        None => return None,
    };
    if !rows[first..=last].iter().any(|r| r.starts_with("> "))
        || frame(rows).is_some()
        || signed_out(rows).is_some()
    {
        return None;
    }
    let trust = (0..first)
        .rev()
        .find(|&i| crate::text::trim(&rows[i]) == const { anchor_text("agy.trust.title") });
    let title = match trust {
        Some(q) => (0..q).find(|&i| !blank(&rows[i])),
        None => (1..first)
            .rev()
            .find(|&i| plain_rule_width(&rows[i]).is_some())
            .filter(|&r| !blank(&rows[r - 1]) && leading_spaces(&rows[r - 1]) == 0)
            .map(|r| r - 1),
    };
    Some(AgyBox {
        title,
        trust,
        first,
        last,
        keys,
        keys_end,
        footer,
    })
}

/// The box's kind, by its title (module header).
fn kind_of(title: &str) -> PromptKind {
    match crate::text::trim(title) {
        t if t == const { anchor_text("agy.title.command") } => PromptKind::Bash,
        t if t == const { anchor_text("agy.title.create") } => PromptKind::Write,
        t if t == const { anchor_text("agy.title.edit") } => PromptKind::Edit,
        t if t == const { anchor_text("agy.title.question") } => PromptKind::Question,
        _ => PromptKind::Other,
    }
}

/// The one-shot allows of the approval boxes, each its own label.
const ONCE: [&str; 3] = [
    anchor_text("agy.box.once"),
    anchor_text("agy.create.once"),
    anchor_text("agy.edit.once"),
];

/// What choosing an approval box's `label` does (module header's table):
/// the labels are each box's own, so one table reads all three.
fn approval_role(label: &str) -> Role {
    if ONCE.contains(&label) {
        Role::Once
    } else if label.contains(const { anchor_text("agy.box.persisted") }) {
        // agy's own suffix on every answer it saves to settings.json, grant or
        // refusal (its binary's labels), whatever the words before it say.
        Role::Persist
    } else if label.starts_with(const { anchor_text("agy.box.session") })
        || label.contains(const { anchor_text("agy.box.conversation") })
    {
        Role::Session
    } else if label.starts_with(const { anchor_text("agy.box.persist") })
        || label == const { anchor_text("agy.create.persist") }
    {
        Role::Persist
    } else if [
        anchor_text("agy.box.deny"),
        anchor_text("agy.create.deny"),
        anchor_text("agy.edit.deny"),
    ]
    .contains(&label)
    {
        Role::Deny
    } else {
        Role::Other
    }
}

fn role_of(kind: PromptKind, label: &str) -> Role {
    match kind {
        PromptKind::Trust if label == const { anchor_text("agy.trust.yes") } => Role::Trust,
        PromptKind::Trust if label == const { anchor_text("agy.trust.no") } => Role::Exit,
        PromptKind::Bash | PromptKind::Write | PromptKind::Edit | PromptKind::Other => {
            approval_role(label)
        }
        // The question's answers are the model's own; its last row asks for
        // the person's own words instead.
        PromptKind::Question if label == const { anchor_text("agy.question.write_in") } => {
            Role::Other
        }
        PromptKind::Question => Role::Answer,
        _ => Role::Other,
    }
}

/// The Esc a box offers: its key row's own (`esc Skip` on the question,
/// which leaves it unanswered), else the footer's `esc to cancel` — the
/// running turn's own hint, the same as while it streams, so it stops the
/// TURN and refuses nothing.
fn cancel_of(keys: &str, footer: Option<&str>) -> Option<Cancel> {
    let lower = crate::text::trim(keys).to_lowercase();
    if let Some(at) = lower.find("esc ") {
        let verb = lower[at + 4..]
            .split('·')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        return Some(Cancel {
            key: "esc".to_string(),
            verb,
            effect: CancelEffect::Back,
        });
    }
    footer
        .is_some_and(|f| footer_of(f) == Footer::Cancel)
        .then(|| Cancel {
            key: "esc".to_string(),
            verb: "cancel".to_string(),
            effect: CancelEffect::Interrupt,
        })
}

/// Whether a question's `options` are answered by one digit (module header):
/// none of them a multi-select's checkbox (`[ ] Red`, `[x] Red`: a digit
/// only toggles it) or the person's own kept answer (`✓ …`), and the last of
/// them agy's `Write-in...`.
fn one_digit_answers(options: &[Opt]) -> bool {
    const MARKS: [&str; 3] = [
        anchor_text("agy.question.unchecked"),
        anchor_text("agy.question.checked"),
        anchor_text("agy.question.written"),
    ];
    options
        .iter()
        .all(|o| !MARKS.iter().any(|m| o.label.starts_with(m)))
        && options
            .last()
            .is_some_and(|o| o.label == const { anchor_text("agy.question.write_in") })
}

/// The box on `rows`, read whole ([`PromptV2`]): its title and kind, the
/// command box's command (the rows under `Requesting permission for:`), a
/// file box's path (its first body row, less its `+1 -1` counts) and reason,
/// the trust box's folder, the question's text, the options with their
/// roles — wrapped tails joined back, every role [`Role::Other`] unless the
/// options run `1.`, `2.`, … (or are the trust box's two) with the cursor on
/// exactly one, and for a question unless one digit answers it — chosen by
/// digit (the trust box by the cursor and Enter), the key row (its wrapped
/// rows joined), and the Esc offered: none on the pending edit, whose Esc
/// agy disabled. A box whose title is off the rows read is named by nothing
/// ([`PromptKind::Other`], [`PromptV2::head_off_screen`]), its options' roles
/// the approval boxes' only where one of their one-shot allows is among
/// them. With no box up, a first run's onboarding screen is one of no kind
/// (`onboarding`), and a subagent's alert over the composer is a prompt
/// named by nothing, its row the title and NO option: its answer is the
/// person's, given in the subagent's own view.
#[must_use]
pub fn prompt(rows: &[String]) -> Option<PromptV2> {
    match agy_box(rows) {
        Some(b) => Some(box_prompt(rows, b)),
        None => onboarding(rows).or_else(|| alert_prompt(rows)),
    }
}

/// WHAT ESC DOES on `p`, a box [`prompt`] read: the Esc it names
/// ([`PromptV2::cancel`]), else what agy's Esc does on the boxes that name
/// none, MEASURED in the lab on 1.4.0 (2026-10-16): on the trust box it
/// quits agy (back at the shell within two seconds; left alone, the box
/// stood); on the terms screen it goes back to the colour scheme, as its
/// `[Previous]` button does. `None` on any other: Esc on the colour scheme
/// changes nothing.
#[must_use]
pub fn esc(p: &PromptV2) -> Option<CancelEffect> {
    if let Some(c) = &p.cancel {
        return Some(c.effect);
    }
    let previous = const { anchor_text("agy.onboarding.previous") };
    match p.kind {
        PromptKind::Trust => Some(CancelEffect::Exit),
        PromptKind::Other => p
            .options
            .iter()
            .any(|o| o.label == previous)
            .then_some(CancelEffect::Back),
        _ => None,
    }
}

fn box_prompt(rows: &[String], b: AgyBox) -> PromptV2 {
    let kind = match (b.trust, b.title) {
        (Some(_), _) => PromptKind::Trust,
        (None, Some(t)) => kind_of(&rows[t]),
        (None, None) => PromptKind::Other,
    };
    let head_off_screen = b.title.is_none();
    let title_row = b.title.unwrap_or(b.first);
    let mut options: Vec<Opt> = Vec::new();
    for (i, row) in rows.iter().enumerate().take(b.last + 1).skip(b.first) {
        if kind == PromptKind::Trust {
            if let Some((label, focused)) = unnumbered(row) {
                options.push(Opt {
                    n: None,
                    label: label.to_string(),
                    role: Role::Other,
                    focused,
                    row: i,
                    kept: false,
                });
            }
        } else if let Some((n, label, focused)) = numbered(row) {
            options.push(Opt {
                n: Some(n),
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            });
        } else if let Some(o) = options.last_mut().filter(|_| !blank(row)) {
            // A wrapped label's tail, at column 0 under it.
            o.label = format!("{} {}", o.label, crate::text::trim(row));
        }
    }
    let ordered = options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.is_none_or(|n| usize::from(n) == k + 1));
    let focus = options.iter().filter(|o| o.focused).count();
    let named =
        kind != PromptKind::Other || options.iter().any(|o| ONCE.contains(&o.label.as_str()));
    let answered_by_digit = kind != PromptKind::Question || one_digit_answers(&options);
    if ordered && focus == 1 && named && answered_by_digit {
        for o in &mut options {
            o.role = role_of(kind, &o.label);
        }
    }

    let body = b.title.map_or(0, |t| t + 1)..b.first;
    let body_rows = || {
        body.clone()
            .map(|i| crate::text::trim(&rows[i]))
            .filter(|t| !t.is_empty() && plain_rule_width(t).is_none())
    };
    let question = match kind {
        PromptKind::Trust => b.trust,
        PromptKind::Question => body.clone().find(|&i| question_text(&rows[i]).is_some()),
        _ => (b.first > 0 && body.contains(&(b.first - 1)) && !blank(&rows[b.first - 1]))
            .then(|| b.first - 1),
    };
    let mut command_rows = Vec::new();
    let (mut description, mut path) = (String::new(), None);
    match kind {
        PromptKind::Bash => {
            let at = body.clone().find(|&i| {
                crate::text::trim(&rows[i]) == const { anchor_text("agy.box.command.for") }
            });
            if let Some(at) = at {
                command_rows = rows[at + 1..b.first]
                    .iter()
                    .map(|r| crate::text::trim(r))
                    .take_while(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect();
            }
        }
        PromptKind::Write | PromptKind::Edit => {
            path = body_rows()
                .next()
                .map(|t| t.split("  ").next().unwrap_or(t).to_string());
            description = body_rows()
                .find_map(|t| t.strip_prefix(const { anchor_text("agy.create.reason") }))
                .map(|r| crate::text::trim(r).to_string())
                .unwrap_or_default();
        }
        PromptKind::Trust => {
            path = body_rows()
                .find(|t| t.starts_with('/') || t.starts_with('~'))
                .map(str::to_string);
            description = b
                .trust
                .map(|q| crate::text::trim(&rows[q]).to_string())
                .unwrap_or_default();
        }
        PromptKind::Question => {
            description = question
                .and_then(|q| question_text(&rows[q]))
                .unwrap_or_default()
                .to_string();
        }
        _ => {}
    }
    let command = match kind {
        PromptKind::Bash => command_rows.join(" "),
        _ => path.clone().unwrap_or_default(),
    };
    let keys = rows[b.keys..=b.keys_end]
        .iter()
        .map(|r| crate::text::trim(r))
        .collect::<Vec<_>>()
        .join(" ");
    let footer = b.footer.map(|f| rows[f].as_str());
    PromptV2 {
        kind,
        title: crate::text::trim(&rows[title_row]).to_string(),
        command_rows,
        description_rows: 0,
        gutter: false,
        command,
        description,
        notes: Vec::new(),
        auto_deny: None,
        path,
        options,
        select: if kind == PromptKind::Trust {
            Select::ArrowsEnter
        } else {
            Select::Digits
        },
        // The pending edit's Esc does nothing (module header).
        cancel: if kind == PromptKind::Edit {
            None
        } else {
            cancel_of(&keys, footer)
        },
        question,
        span: (title_row, b.keys_end),
        footer: Some(keys),
        head_off_screen,
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

/// A subagent's alert over the composer as a prompt ([`prompt`]): named by
/// nothing, its row the title, NO option — never a box with its head off the
/// screen: the whole of it is on the screen, and it offers nothing here to
/// press.
fn alert_prompt(rows: &[String]) -> Option<PromptV2> {
    let f = frame(rows)?;
    let row = alert(rows, f.top)?;
    Some(PromptV2 {
        kind: PromptKind::Other,
        title: crate::text::trim(&rows[row]).to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: String::new(),
        description: String::new(),
        notes: Vec::new(),
        auto_deny: None,
        path: None,
        options: Vec::new(),
        select: Select::Digits,
        cancel: None,
        question: None,
        span: (row, row),
        footer: None,
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
}

/// `Question 1/1: Which color …` → `Which color …`.
fn question_text(row: &str) -> Option<&str> {
    let rest = crate::text::trim(row).strip_prefix("Question ")?;
    let (count, text) = rest.split_once(": ")?;
    let (a, b) = count.split_once('/')?;
    (a.parse::<u8>().is_ok() && b.parse::<u8>().is_ok()).then_some(text)
}

/// The first run's onboarding headings (module header), each at column 0:
/// the colour scheme's (its preview box drawn on the same row) and the
/// terms'.
const ONBOARDING: [&str; 2] = [
    anchor_text("agy.onboarding.scheme"),
    anchor_text("agy.onboarding.terms"),
];

/// A first run's onboarding screen (module header), read whole as a box of
/// NO KIND: no composer, its key row ([`key_row`]) and above it one of the
/// [`ONBOARDING`] headings. Its options are every row left of the preview
/// box (`│`) that is a choice — `> ` at column 2 the focused one, a label at
/// column 4 another, the rows a long one wrapped onto at column 6 or more
/// joined to it, and each `[Button]` of a row of them — every one
/// [`Role::Other`], unnumbered, chosen by the cursor and Enter. Nothing on
/// it names a role, so no policy presses any of it: the person's taste and
/// the person's consent.
fn onboarding(rows: &[String]) -> Option<PromptV2> {
    if frame(rows).is_some() {
        return None;
    }
    let keys = key_row(rows)?;
    let (head, title) = (0..keys).rev().find_map(|i| {
        let row = crate::text::trim_end(&rows[i]);
        ONBOARDING
            .iter()
            .find(|h| {
                row.starts_with(**h) && (row.len() == h.len() || row[h.len()..].starts_with(' '))
            })
            .map(|h| (i, *h))
    })?;
    let mut options: Vec<Opt> = Vec::new();
    let mut wraps = false;
    for (i, row) in rows.iter().enumerate().take(keys).skip(head + 1) {
        let left = crate::text::trim_end(row.split(['│', '╭', '╰']).next().unwrap_or(row));
        let text = crate::text::trim(left);
        let mut push = |label: &str, focused: bool| {
            options.push(Opt {
                n: None,
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            });
        };
        match leading_spaces(left) {
            _ if text.is_empty() => wraps = false,
            4 if text.starts_with('[') => {
                for button in text
                    .split("  ")
                    .map(crate::text::trim)
                    .filter(|b| !b.is_empty())
                {
                    push(button, false);
                }
                wraps = false;
            }
            2 if text.starts_with("> ") => {
                push(crate::text::trim(&text[2..]), true);
                wraps = true;
            }
            4 => {
                push(text.split("  ").next().unwrap_or(text), false);
                wraps = true;
            }
            n if n >= 6 && wraps => {
                if let Some(o) = options.last_mut() {
                    o.label = format!("{} {text}", o.label);
                }
            }
            _ => wraps = false,
        }
    }
    Some(PromptV2 {
        kind: PromptKind::Other,
        title: title.to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: String::new(),
        description: String::new(),
        notes: Vec::new(),
        auto_deny: None,
        path: None,
        options,
        select: Select::ArrowsEnter,
        cancel: None,
        question: Some(head),
        span: (head, keys),
        footer: Some(crate::text::trim(&rows[keys]).to_string()),
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
}

/// Both composer rules, `(top, bottom)`, when agy's composer frame is on the
/// screen.
#[must_use]
pub fn composer_rules(rows: &[String]) -> Option<(usize, usize)> {
    frame(rows).map(|f| (f.top, f.bottom))
}

/// Where a running turn's spinner row can be: under this session's last user
/// message above the composer's top rule, so a spinner glyph quoted in an
/// earlier turn never holds a wait.
#[must_use]
pub fn busy_zone_start(rows: &[String]) -> usize {
    frame(rows).map_or(0, |f| last_user(rows, f.top).map_or(0, |u| u + 1))
}

/// The banner text on the first row of agy's launch card.
const AGY_BANNER: &str = "Antigravity CLI ";

fn is_mascot_block(c: char) -> bool {
    ('\u{2580}'..='\u{259F}').contains(&c)
}

fn mascot_prefix_end(prefix: &str) -> Option<usize> {
    if !prefix.chars().all(|c| c == ' ' || is_mascot_block(c)) {
        return None;
    }
    let trimmed = crate::text::trim_end(prefix);
    trimmed
        .chars()
        .any(is_mascot_block)
        .then(|| trimmed.chars().count())
}

fn split_at_col(row: &str, col: usize) -> Option<(&str, &str)> {
    let mut chars = row.char_indices();
    for _ in 0..col {
        chars.next()?;
    }
    let byte = chars.next().map_or(row.len(), |(i, _)| i);
    Some((&row[..byte], &row[byte..]))
}

fn agy_banner_version(after: &str) -> Option<&str> {
    let len = after
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count();
    let version = &after[..len];
    let mut parts = 0;
    for part in version.split('.') {
        if part.is_empty() || part.len() > 6 {
            return None;
        }
        parts += 1;
    }
    (parts == 3 && crate::text::trim(&after[len..]).is_empty()).then_some(version)
}

fn agy_card_row(row: &str, text_col: usize) -> Option<(usize, usize)> {
    let (before, after) = split_at_col(row, text_col)?;
    let mascot_end = mascot_prefix_end(before)?;
    let trimmed = crate::text::trim_end(after);
    if trimmed.is_empty()
        || trimmed.starts_with(char::is_whitespace)
        || trimmed.contains('\u{2026}')
    {
        return None;
    }
    Some((mascot_end, text_col + trimmed.chars().count()))
}

fn agy_feet_row(row: &str, text_col: usize) -> Option<usize> {
    match split_at_col(row, text_col) {
        Some((before, after)) => {
            if !crate::text::trim(after).is_empty() {
                return None;
            }
            mascot_prefix_end(before)
        }
        None => mascot_prefix_end(row),
    }
}

/// agy's banner is not known to print its running build: every recorded
/// card reads 1.4.0, captured while the store held 1.4.1 and 1.4.2
/// (`crate::BannerBuild::ProvenanceOnly`).
pub const LAUNCH_CARD_BUILD: crate::BannerBuild = crate::BannerBuild::ProvenanceOnly;

/// The rows [`launch_card_scan`] reads of a card from its banner down: the
/// banner, three mascot rows and the feet row under them.
pub const LAUNCH_CARD_ROWS: usize = 5;

fn agy_card_at(rows: &[String], top: usize) -> Option<CardPlace> {
    if top + 4 >= rows.len() {
        return None;
    }
    let row0 = &rows[top];
    let byte_at = row0.find(AGY_BANNER)?;
    let before = &row0[..byte_at];
    let mut mascot_end = mascot_prefix_end(before)?;
    let text_col = before.chars().count();
    let after = &row0[byte_at + AGY_BANNER.len()..];
    agy_banner_version(after)?;
    let end0 = crate::text::trim_end(row0).chars().count();
    let (m1, end1) = agy_card_row(&rows[top + 1], text_col)?;
    let (m2, end2) = agy_card_row(&rows[top + 2], text_col)?;
    let (m3, end3) = agy_card_row(&rows[top + 3], text_col)?;
    let m4 = agy_feet_row(&rows[top + 4], text_col)?;
    mascot_end = mascot_end.max(m1).max(m2).max(m3).max(m4);
    Some(CardPlace {
        top,
        rows: 4,
        mascot_row: 0,
        text_col,
        mascot_end,
        ends: [end0, end1, end2.max(end3)],
    })
}

/// Scan `rows` for agy's 5-row launch card: `(banner_row, place)`.
#[must_use]
pub fn launch_card_scan(rows: &[String]) -> (Option<usize>, Option<CardPlace>) {
    for i in (0..rows.len()).rev() {
        let Some(byte_at) = rows[i].find(AGY_BANNER) else {
            continue;
        };
        if mascot_prefix_end(&rows[i][..byte_at]).is_none() {
            continue;
        }
        return (Some(i), agy_card_at(rows, i));
    }
    (None, None)
}

/// Whether [`launch_card_scan`] on `rows` is unchanged when only `changed`
/// rows moved since it returned `banner`.
#[must_use]
pub fn launch_card_unmoved(rows: &[String], banner: Option<usize>, changed: &[usize]) -> bool {
    let touches = |j: usize| rows.get(j).is_some_and(|r| r.contains(AGY_BANNER));
    match banner {
        Some(b) => {
            b + 4 < rows.len() && changed.iter().all(|&j| j < b || (j > b + 4 && !touches(j)))
        }
        None => changed.iter().all(|&j| !touches(j)),
    }
}

/// The version agy's launch card at `place` names (`1.4.0`).
#[must_use]
pub fn launch_card_version<'a>(rows: &'a [String], place: &CardPlace) -> Option<&'a str> {
    let (_, after) = split_at_col(rows.get(place.top)?, place.text_col)?;
    agy_banner_version(after.strip_prefix(AGY_BANNER)?)
}

/// The text of rows `0`, `2`, and `3` of agy's launch card at `place`.
#[must_use]
pub fn launch_card_words<'a>(rows: &'a [String], place: &CardPlace) -> Option<[&'a str; 3]> {
    let word = |r: usize| {
        let (_, after) = split_at_col(rows.get(place.top + r)?, place.text_col)?;
        let w = crate::text::trim_end(after);
        (!w.is_empty()).then_some(w)
    };
    Some([word(0)?, word(2)?, word(3)?])
}

fn parse_agy_model_row(text: &str) -> Option<(String, Option<String>)> {
    let t = crate::text::trim(text);
    if t.is_empty() || t.contains('\u{2026}') {
        return None;
    }
    if let Some((model, effort)) = t.strip_suffix(')').and_then(|s| s.rsplit_once(" (")) {
        let lower = effort.to_ascii_lowercase();
        if matches!(lower.as_str(), "low" | "medium" | "high" | "xhigh" | "max") {
            let model = crate::text::trim(model);
            if !model.is_empty() {
                return Some((model.to_owned(), Some(lower)));
            }
        }
    }
    Some((t.to_owned(), None))
}

fn parse_agy_footer_model(row: &str) -> Option<(String, Option<String>)> {
    let t = crate::text::trim_end(row);
    let right = t.rsplit("  ").next().map(crate::text::trim)?;
    let mut parts = right.split(" \u{00B7} ").map(crate::text::trim);
    let model = parts
        .next()
        .filter(|m| !m.is_empty() && *m != "(initializing...)" && !m.starts_with('?'))?;
    let effort = parts
        .next()
        .map(str::to_ascii_lowercase)
        .filter(|e| matches!(e.as_str(), "low" | "medium" | "high" | "xhigh" | "max"))?;
    Some((model.to_owned(), Some(effort)))
}

/// The signed-in email and optional subscription plan on agy's launch card
/// (`user@example.com (Google AI Ultra)` on row `place.top + 1`), borrowed
/// from `rows`: a footer that reads it each frame compares it with what it
/// shows and copies it only when it moved.
#[must_use]
pub fn launch_card_account(rows: &[String]) -> Option<(&str, Option<&str>)> {
    let f = frame(rows)?;
    let (_, place) = launch_card_scan(&rows[..f.top]);
    let p = place?;
    let (_, after) = split_at_col(rows.get(p.top + 1)?, p.text_col)?;
    let t = crate::text::trim(after);
    if t.is_empty() || t.contains('\u{2026}') {
        return None;
    }
    if let Some((email, plan)) = t.strip_suffix(')').and_then(|s| s.rsplit_once(" (")) {
        let email = crate::text::trim(email);
        let plan = crate::text::trim(plan);
        if email.contains('@') && !email.chars().any(char::is_whitespace) {
            return Some((email, (!plan.is_empty()).then_some(plan)));
        }
    }
    if t.contains('@') && !t.chars().any(char::is_whitespace) {
        return Some((t, None));
    }
    None
}

/// agy's launch card and live footer model/effort for the harness footer.
#[must_use]
pub fn launch_card(rows: &[String]) -> Option<LaunchCard> {
    launch_card_read(rows).map(|read| read.card)
}

/// [`launch_card`] with the rows it was read from
/// ([`crate::LaunchCardRead`]): the card's banner row where its version (and
/// its model, with no footer to name one) came from the card, and the live
/// footer's row where the model came from there. Never panics, on any rows.
#[must_use]
pub fn launch_card_read(rows: &[String]) -> Option<crate::LaunchCardRead> {
    let f = frame(rows)?;
    let footer_model = f
        .footer
        .and_then(|r| Some((r, parse_agy_footer_model(rows.get(r)?)?)));
    let (_, place) = launch_card_scan(&rows[..f.top]);
    let card_info = place.and_then(|p| {
        let ver = launch_card_version(rows, &p)?.to_owned();
        let (_, after) = split_at_col(rows.get(p.top + 2)?, p.text_col)?;
        let (model, effort) = parse_agy_model_row(after)?;
        Some((p.top, ver, model, effort))
    });
    let (card, banner, status) = match (card_info, footer_model) {
        (Some((top, version, _card_model, card_effort)), Some((row, (f_model, f_effort)))) => (
            LaunchCard {
                version,
                model: f_model,
                effort: f_effort.or(card_effort),
            },
            Some(top),
            Some(row),
        ),
        (Some((top, version, model, effort)), None) => (
            LaunchCard {
                version,
                model,
                effort,
            },
            Some(top),
            None,
        ),
        (None, Some((row, (model, effort)))) => (
            LaunchCard {
                version: String::new(),
                model,
                effort,
            },
            None,
            Some(row),
        ),
        (None, None) => return None,
    };
    Some(crate::LaunchCardRead {
        card,
        banner,
        status,
    })
}

/// agy's screens, measured, for this crate's tests and a dependent crate's
/// `#[cfg(test)]` code; not part of the documented API. Read them with
/// [`crate::prompt::fixtures::screen`].
#[doc(hidden)]
pub mod fixtures {
    /// The workspace-trust box, drawn from row 0.
    pub const BOX_TRUST: &str = include_str!("fixtures/agy-1.4.0-box-trust.txt");
    /// A fresh session right after the trust box: the shell's echo of agy's
    /// terminal queries, the logo, a notice and a dismissable card.
    pub const IDLE_FRESH_NOTICE: &str = include_str!("fixtures/agy-1.4.0-idle-fresh-notice.txt");
    /// Idle at its composer, the notices dismissed.
    pub const IDLE: &str = include_str!("fixtures/agy-1.4.0-idle.txt");
    /// The same at 100 columns.
    pub const IDLE_100X30: &str = include_str!("fixtures/agy-1.4.0-idle-100x30.txt");
    /// A typed, unsent draft: the footer's left side blank.
    pub const DRAFT: &str = include_str!("fixtures/agy-1.4.0-draft.txt");
    /// 1.5 s into a turn: the spinner row over the open composer.
    pub const BUSY: &str = include_str!("fixtures/agy-1.4.0-busy-generating.txt");
    /// Mid-turn, the reply streaming, a `└ Tip:` row under the spinner.
    pub const BUSY_TIP: &str = include_str!("fixtures/agy-1.4.0-busy-streaming-tip.txt");
    /// The end of a turn: no spinner, `? for shortcuts`.
    pub const END_OF_TURN: &str = include_str!("fixtures/agy-1.4.0-end-of-turn.txt");
    /// A turn stopped with Esc.
    pub const INTERRUPTED: &str = include_str!("fixtures/agy-1.4.0-interrupted.txt");
    /// A turn that failed: `⚠ …` and `Error ID:`.
    pub const ERROR_TURN: &str = include_str!("fixtures/agy-1.4.0-error-turn.txt");
    /// The shell-command approval.
    pub const BOX_EXEC: &str = include_str!("fixtures/agy-1.4.0-box-exec.txt");
    /// The same at 100 columns.
    pub const BOX_EXEC_100X30: &str = include_str!("fixtures/agy-1.4.0-box-exec-100x30.txt");
    /// The same, option 3 wrapped: its tail at column 0 of the next row.
    pub const BOX_EXEC_WRAPPED: &str =
        include_str!("fixtures/agy-1.4.0-box-exec-wrapped-option.txt");
    /// A file creation outside the workspace (three options).
    pub const BOX_CREATE: &str = include_str!("fixtures/agy-1.4.0-box-create-file.txt");
    /// A file creation inside the workspace (two options).
    pub const BOX_CREATE_WORKSPACE: &str =
        include_str!("fixtures/agy-1.4.0-box-create-file-workspace.txt");
    /// An edit to an existing file.
    pub const BOX_EDIT: &str = include_str!("fixtures/agy-1.4.0-box-pending-edit.txt");
    /// agy's ask-the-user tool.
    pub const QUESTION: &str = include_str!("fixtures/agy-1.4.0-question.txt");
    /// The shell-command box for a multi-line script: options 2 and 3 quote
    /// the command's prefix over rows of their own, a blank one among them.
    pub const BOX_EXEC_MULTILINE: &str =
        include_str!("fixtures/agy-1.4.0-box-exec-multiline-options.txt");
    /// After `No, cancel` on the shell box, at 100 columns.
    pub const CANCELLED: &str = include_str!("fixtures/agy-1.4.0-cancelled-100x30.txt");
    /// After `No, deny creation` on the file box.
    pub const DECLINED: &str = include_str!("fixtures/agy-1.4.0-declined-interrupted.txt");
    /// After `No, reject this change` on the edit box.
    pub const REJECTED: &str = include_str!("fixtures/agy-1.4.0-rejected-interrupted.txt");
    /// `/` typed: the command menu under the composer.
    pub const SLASH_MENU: &str = include_str!("fixtures/agy-1.4.0-slash-menu.txt");
    /// One ctrl+c at the empty idle composer: an exit pending (one over a
    /// held draft arms it too: [`lab::CTRL_C_DRAFT`]).
    pub const EXIT_CONFIRM: &str = include_str!("fixtures/agy-1.4.0-exit-confirm.txt");
    /// After the second ctrl+c: the resume line, then the shell.
    pub const EXITED: &str = include_str!("fixtures/agy-1.4.0-exited-resume.txt");
    /// Signed out: the login-method menu drawn instead of a composer.
    pub const SIGNED_OUT: &str = include_str!("fixtures/agy-1.4.0-wall-signed-out.txt");

    /// The same agy driven by `aterm-lab` (`lab agy-captures`) at 144×50,
    /// under a scratch HOME whose settings name the Gemini API provider, a
    /// fake key and a local stub of the Gemini API, its Google hosts
    /// black-holed — each file's line 1 says what the stub answered.
    pub mod lab {
        /// First run, no onboarding seed: the colour-scheme picker.
        pub const FIRST_RUN_SCHEME: &str =
            include_str!("fixtures/agy-1.4.0-lab-first-run-color-scheme.txt");
        /// After one Enter on it: the terms and data use, ticked by default.
        pub const FIRST_RUN_TERMS: &str =
            include_str!("fixtures/agy-1.4.0-lab-first-run-terms.txt");
        /// No trust seed: the folder-trust box.
        pub const FIRST_RUN_TRUST: &str =
            include_str!("fixtures/agy-1.4.0-lab-first-run-trust.txt");
        /// A fresh session, every first-run screen seeded away.
        pub const IDLE: &str = include_str!("fixtures/agy-1.4.0-lab-idle.txt");
        /// A typed, unsent draft.
        pub const DRAFT: &str = include_str!("fixtures/agy-1.4.0-lab-draft.txt");
        /// Thinking: the spinner's word is the thought.
        pub const BUSY_THINKING: &str = include_str!("fixtures/agy-1.4.0-lab-busy-thinking.txt");
        /// Streaming the reply.
        pub const BUSY_STREAMING: &str = include_str!("fixtures/agy-1.4.0-lab-busy-streaming.txt");
        /// The end of a turn.
        pub const END_OF_TURN: &str = include_str!("fixtures/agy-1.4.0-lab-end-of-turn.txt");
        /// A spawned worker's first prompt holding a blank line (the task,
        /// a blank line, the spawn's fact line), its reply, then the
        /// harness's Land act and the landing (`lab forever`'s L15, before
        /// a worker's task file dropped agy's blank lines): the message's
        /// blank line is a blank row, and the rows under it sit two columns
        /// in, as the reply's (the module header's A MESSAGE'S BLANK
        /// LINE).
        pub const WORKER_FIRST_PROMPT: &str =
            include_str!("fixtures/agy-1.4.0-lab-worker-first-prompt.txt");
        /// A person's message of three paragraphs (a line break, then a
        /// blank line, each later one opening `NEEDS YOU:`) sent through
        /// `send`/`key as=person`, the stub's reply under it, then the
        /// harness's Cruise act and its reply: the message's blank line a
        /// blank row like the one that ends it. Its cells (line 1) tell
        /// them apart: the message's rows, that blank row among them,
        /// `5c5cff` bold from column 0; the blank row under it plain.
        pub const PERSON_PARAGRAPHS: &str =
            include_str!("fixtures/agy-1.4.0-lab-person-paragraphs.txt");
        /// A later screen of the same session: a second person's message of
        /// two paragraphs (a blank line between, the second `NEEDS YOU:`)
        /// under the stub's long reply of three paragraphs, blank rows
        /// between them; its cells in `aterm-agent`'s
        /// `supervise/fixtures/agy-1.4.0-lab-person-paragraphs-long-reply.cells`.
        pub const PERSON_PARAGRAPHS_LONG_REPLY: &str =
            include_str!("fixtures/agy-1.4.0-lab-person-paragraphs-long-reply.txt");
        /// The pane at 50 columns: a reply of two paragraphs, the second
        /// wrapped over three rows at column two, two cells short of the
        /// pane's edge, its last `or 2)?`.
        pub const REPLY_WRAPPED_50: &str =
            include_str!("fixtures/agy-1.4.0-lab-reply-wrapped-50col.txt");
        /// A turn stopped with Esc.
        pub const INTERRUPTED: &str = include_str!("fixtures/agy-1.4.0-lab-interrupted.txt");
        /// `/` typed: the command menu.
        pub const SLASH_MENU: &str = include_str!("fixtures/agy-1.4.0-lab-slash-menu.txt");
        /// The shell tool's box.
        pub const BOX_SHELL: &str = include_str!("fixtures/agy-1.4.0-lab-box-shell.txt");
        /// The write tool's box (a new file in the workspace).
        pub const BOX_WRITE: &str = include_str!("fixtures/agy-1.4.0-lab-box-write.txt");
        /// The edit tool's box.
        pub const BOX_EDIT: &str = include_str!("fixtures/agy-1.4.0-lab-box-edit.txt");
        /// The question tool.
        pub const QUESTION: &str = include_str!("fixtures/agy-1.4.0-lab-question.txt");
        /// The question answered with Enter, the turn ended.
        pub const QUESTION_ANSWERED: &str =
            include_str!("fixtures/agy-1.4.0-lab-question-answered.txt");
        /// The feedback survey, drawn unasked in the composer's place.
        pub const SURVEY: &str = include_str!("fixtures/agy-1.4.0-lab-survey.txt");
        /// `/quit` sent: the resume line, then the lab's shell.
        pub const EXIT: &str = include_str!("fixtures/agy-1.4.0-lab-exit.txt");
        /// One ctrl+d at the empty composer: an exit pending, its hint
        /// still standing a minute on.
        pub const EXIT_CONFIRM_CTRL_D: &str =
            include_str!("fixtures/agy-1.4.0-lab-exit-confirm-ctrl-d.txt");
        /// One ctrl+c over a held draft (`x`): an exit pending, the draft
        /// kept in the composer.
        pub const CTRL_C_DRAFT: &str = include_str!("fixtures/agy-1.4.0-lab-ctrl-c-draft.txt");
        /// A 429 RESOURCE_EXHAUSTED per-day quota, not retried.
        pub const ERROR_429: &str = include_str!("fixtures/agy-1.4.0-lab-error-429-per-day.txt");
        /// A 401 UNAUTHENTICATED.
        pub const ERROR_401: &str = include_str!("fixtures/agy-1.4.0-lab-error-401.txt");
        /// A 400 INVALID_ARGUMENT, reason API_KEY_INVALID.
        pub const ERROR_400_KEY: &str = include_str!("fixtures/agy-1.4.0-lab-error-400-key.txt");
        /// A 503 UNAVAILABLE mid-retry: the spinner, no retry row.
        pub const RETRY_503: &str = include_str!("fixtures/agy-1.4.0-lab-retry-503.txt");
        /// The 503 once its retries ran out.
        pub const ERROR_503: &str = include_str!("fixtures/agy-1.4.0-lab-error-503.txt");
        /// A 500 INTERNAL, shown at once (not retried).
        pub const ERROR_500: &str = include_str!("fixtures/agy-1.4.0-lab-error-500.txt");
        /// A 403 PERMISSION_DENIED, reason SERVICE_DISABLED: not retried.
        pub const ERROR_403_DISABLED: &str =
            include_str!("fixtures/agy-1.4.0-lab-error-perm403.txt");
        /// A 403 PERMISSION_DENIED with no details: not retried.
        pub const ERROR_403: &str = include_str!("fixtures/agy-1.4.0-lab-error-denied403.txt");
        /// A 404 NOT_FOUND for the model: not retried.
        pub const ERROR_404: &str = include_str!("fixtures/agy-1.4.0-lab-error-notfound404.txt");
        /// A 400 FAILED_PRECONDITION (the location): not retried.
        pub const ERROR_400_LOCATION: &str =
            include_str!("fixtures/agy-1.4.0-lab-error-location400.txt");
        /// A 504 DEADLINE_EXCEEDED once its eight retries ran out.
        pub const ERROR_504: &str = include_str!("fixtures/agy-1.4.0-lab-error-timeout504.txt");
        /// A bare 429 RESOURCE_EXHAUSTED (no RetryInfo) once its eight
        /// retries ran out.
        pub const ERROR_429_BARE: &str =
            include_str!("fixtures/agy-1.4.0-lab-error-exhausted429.txt");
        /// A per-minute 429, RetryInfo 47.218363629s: not retried.
        pub const ERROR_429_PER_MINUTE: &str =
            include_str!("fixtures/agy-1.4.0-lab-error-minute429.txt");
        /// A per-day 429, RetryInfo 41397.204181s: not retried.
        pub const ERROR_429_LONG: &str =
            include_str!("fixtures/agy-1.4.0-lab-error-day429long.txt");
        /// The turn ended with a command task running on: the panel
        /// (`  ● [17:50:42] … running`) under the empty composer, the
        /// footer counting `1 task(s)`.
        pub const BG_TASK: &str = include_str!("fixtures/agy-1.4.0-lab-bg-task.txt");
        /// A timer pending after the turn: `  ● [17:54:12] Lab timer running`.
        pub const BG_TIMER: &str = include_str!("fixtures/agy-1.4.0-lab-bg-timer.txt");
        /// A subagent working after the turn: `  ● Agent(research: Lab
        /// Waiter)  Working · 1s`, the footer counting `1 subagent(s)`.
        pub const BG_SUBAGENT: &str = include_str!("fixtures/agy-1.4.0-lab-bg-subagent.txt");
        /// A subagent and a command task: one panel row each.
        pub const BG_TASK_AND_SUBAGENT: &str =
            include_str!("fixtures/agy-1.4.0-lab-bg-task-and-subagent.txt");
        /// A daemon command task: `  ● [18:03:48] sleep 300 running`.
        pub const BG_DAEMON: &str = include_str!("fixtures/agy-1.4.0-lab-bg-daemon.txt");
        /// A draft held over the panel, the footer's left side blank.
        pub const BG_DRAFT: &str = include_str!("fixtures/agy-1.4.0-lab-bg-draft.txt");
        /// A turn running while a task runs: the spinner over the composer,
        /// the panel under it.
        pub const BG_BUSY: &str = include_str!("fixtures/agy-1.4.0-lab-bg-busy.txt");
        /// [`BG_TASK`]'s task ended: no panel, no count.
        pub const BG_TASK_ENDED: &str = include_str!("fixtures/agy-1.4.0-lab-bg-task-ended.txt");
        /// agy ended by SIGTERM with work running: its last frame, the panel
        /// rows in it, under them its resume line and the shell.
        pub const BG_EXITED: &str = include_str!("fixtures/agy-1.4.0-lab-bg-exited.txt");
        /// The panel turned off (`"hideActiveItems": true`, measured
        /// 2026-10-18): a command task running after its turn ended, no
        /// panel, the footer still counting `1 task(s)`.
        pub const BG_PANEL_OFF_TASK: &str =
            include_str!("fixtures/agy-1.4.0-lab-bg-panel-off-task.txt");
        /// The panel turned off: a subagent working, the footer counting `1
        /// subagent(s)`.
        pub const BG_PANEL_OFF_SUBAGENT: &str =
            include_str!("fixtures/agy-1.4.0-lab-bg-panel-off-subagent.txt");
        /// The panel turned off: a daemon command task (`IsDaemon`), the
        /// footer counting `1 task(s)`.
        pub const BG_PANEL_OFF_DAEMON: &str =
            include_str!("fixtures/agy-1.4.0-lab-bg-panel-off-daemon.txt");
        /// A redraw split by one 1024-byte PTY read, three tabs at once (measured
        /// 2026-10-22, `docs/measured/half-drawn-frame-2026-10-22.md`), read at the split:
        /// the frame's erase-below took the spinner row and the composer, its list cut
        /// mid-word. No screen agy drew whole, so no layout names agy on it; the
        /// terminal holds its DEC 2026 frame back from every reader of whole frames.
        pub const SPLIT_HALF: &str = include_str!("fixtures/agy-1.4.0-lab-split-half.txt");
        /// [`SPLIT_HALF`]'s frame whole, 1.2 ms on: the spinner over the composer.
        pub const SPLIT_WHOLE: &str = include_str!("fixtures/agy-1.4.0-lab-split-whole.txt");
    }
}

#[cfg(test)]
#[path = "agy_tests.rs"]
mod tests;
