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
//! >                                         the caret and NO placeholder (the
//! ──────────────────────────────────────    cursor parks at column 2), a rule
//! ? for shortcuts  Gemini 4 Argon · medium  and the FOOTER
//! ```
//!
//! **THE FOOTER'S LEFT SIDE** says what the composer is doing:
//! `? for shortcuts` when it is empty and idle, nothing while a draft is
//! held, `press ctrl+c again to exit` after one ctrl+c, and `esc to cancel`
//! while a turn runs — AND while a menu or a box is open, so `esc to cancel`
//! alone is no running turn. The composer STAYS OPEN while a turn runs (the
//! cursor at its column 2): what says a turn runs is the spinner row right
//! above it. No row marks a turn's end; the spinner gone and the footer back
//! to `? for shortcuts` is the end ([`phase`]). `/` opens the command menu
//! UNDER the bottom rule, its key row over the footer's `esc to cancel`.
//!
//! **THE BOXES** replace the composer, the cursor hidden: a bold title over a
//! full rule, the body, a question row, numbered options (`> 1. …` focused,
//! `  2. …`), a blank row and a key row (`  ↑/↓ Navigate · …`) over the
//! footer's `esc to cancel`. A digit picks its option at once (measured on 1
//! to 4). A long option WRAPS with its tail at column 0 of the next row
//! ([`fixtures::BOX_EXEC_WRAPPED`]):
//!
//! | title | options |
//! |---|---|
//! | `Command` | `Yes, run command` · `Yes, and always allow in this conversation for commands that start with '<p>'` · `Yes, and always allow for commands that start with '<p>' (Persist to settings.json)` · `No, cancel` |
//! | `Create file` | `Yes, allow creation` · outside the workspace `Yes, and always allow non-workspace access` (it persists) · `No, deny creation` |
//! | `Pending edit` | `Yes, accept this change` · `No, reject this change` |
//! | `Question` | the model's answers, none marked recommended, then `Write-in...` |
//!
//! The TRUST box agy draws in a folder it has not seen is unnumbered (`> Yes,
//! I trust this folder` / `  No, exit`), chosen by the cursor and Enter.
//!
//! **SIGNED OUT** agy draws `You are currently not signed in.` over `Select
//! login method:` and `> 1. Google OAuth`, whose Enter opens a browser on the
//! person's desktop. It is a WALL ([`wall`], `WallKind::Auth`) and never a
//! box: no option of it is read, so no policy can press one. A person signs
//! in by quitting and relaunching `agy`.
//!
//! **A TURN ENDS** on the reply, on `  ⎿  Interrupted · What should
//! Antigravity CLI do instead?` (Esc in the turn, or a box refused:
//! [`interrupted`]), or on `⚠ <message>` over `Error ID: <id>` when it failed.
//! No wall kind fits that failure — the one measured, `failed to construct
//! executor: plan model not specified`, came of a dead proxy and names none —
//! so it is an ended turn with no wall. agy's usage and quota walls are
//! UNMEASURED and not read.

use crate::anchors::anchor_text;
use crate::phase::{Phase, leading_spaces, plain_rule_width};
use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};
use crate::text::blank;
use crate::wall::Wall;

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
    /// Nothing on the left, the model on the right: a draft is held.
    Blank,
    /// `esc to cancel`: a turn runs, or a menu or a box is open.
    Cancel,
    /// `press ctrl+c again to exit`: one ctrl+c pressed at an idle composer.
    ExitPending,
    /// Words this grammar has not measured.
    Other,
}

const FOOTERS: [(&str, Footer); 3] = [
    (anchor_text("agy.footer.idle"), Footer::Shortcuts),
    (anchor_text("agy.footer.cancel"), Footer::Cancel),
    (anchor_text("agy.footer.exit"), Footer::ExitPending),
];

fn footer_of(row: &str) -> Footer {
    if leading_spaces(row) > 2 {
        // The right side alone: `Gemini 4 Argon · medium`.
        return if crate::text::contains(row, " · ") {
            Footer::Blank
        } else {
            Footer::Other
        };
    }
    let t = crate::text::trim(row);
    FOOTERS
        .iter()
        .find(|(text, _)| t.starts_with(text))
        .map_or(Footer::Other, |&(_, f)| f)
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
}

/// The composer ([`Frame`]): the last caret row in column 0 between two
/// plain rules of one width (the transcript's separators are 60 columns, the
/// composer's the pane's), the rows between them its draft, and under it the
/// footer as the last row drawn — or the command menu, its key row and then
/// the footer. A screen agy left keeps its last frame with its resume line
/// and the shell's prompt under it, and has none.
fn frame(rows: &[String]) -> Option<Frame> {
    (1..rows.len())
        .rev()
        .filter(|&c| is_caret_row(&rows[c]))
        .find_map(|c| frame_at(rows, c))
}

fn frame_at(rows: &[String], caret: usize) -> Option<Frame> {
    let top = caret.checked_sub(1)?;
    let width = plain_rule_width(&rows[top])?;
    // A wrapped draft's rows are indented under the caret's text.
    let bottom =
        (caret + 1..rows.len()).find(|&i| blank(&rows[i]) || leading_spaces(&rows[i]) < 2)?;
    if plain_rule_width(&rows[bottom]) != Some(width) {
        return None;
    }
    let last = rows.iter().rposition(|r| !blank(r))?;
    let (footer, menu) = if last == bottom {
        (None, false)
    } else if last == bottom + 1 {
        (Some(last), false)
    } else {
        let between = &rows[bottom + 1..last];
        let menu = between.iter().any(|r| is_menu_row(r))
            && between
                .iter()
                .all(|r| blank(r) || is_menu_row(r) || is_key_row(r));
        if !menu {
            return None;
        }
        (Some(last), true)
    };
    Some(Frame {
        top,
        caret,
        bottom,
        footer,
        menu,
    })
}

/// The composer's caret row.
#[must_use]
pub fn composer(rows: &[String]) -> Option<usize> {
    frame(rows).map(|f| f.caret)
}

fn draft(rows: &[String], f: Frame) -> Vec<String> {
    let caret = rows[f.caret].strip_prefix('>').unwrap_or(&rows[f.caret]);
    std::iter::once(crate::text::trim(caret))
        .chain(
            rows[f.caret + 1..f.bottom]
                .iter()
                .map(|r| crate::text::trim(r)),
        )
        .map(str::to_string)
        .collect()
}

/// The composer's text as `(caret row, lines)`: the caret row's text first
/// (its `>` stripped — empty when nothing is typed: agy draws no
/// placeholder), then each row its draft wrapped onto.
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let f = frame(rows)?;
    Some((f.caret, draft(rows, f)))
}

/// The last person's message above the composer's top rule.
fn last_user(rows: &[String], top: usize) -> Option<usize> {
    (1..top).rev().find(|&i| is_user_row(rows, i))
}

/// Whether a turn runs: the spinner row right above the composer's top rule,
/// or above the `└ Tip:` rows under it.
fn spinning(rows: &[String], top: usize) -> bool {
    (0..top)
        .rev()
        .find(|&i| !crate::text::trim_start(&rows[i]).starts_with('└'))
        .is_some_and(|i| is_spinner_row(&rows[i]))
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

/// The phase, and whether it is evidence (module header): the spinner over
/// the composer is busy; the composer with `? for shortcuts` (or with a
/// draft and nothing on the footer's left, or `press ctrl+c again to exit`)
/// is idle — a question when the reply's last words end in `?`; a box is a
/// prompt; the sign-in wall is idle on its wall. `esc to cancel` with no
/// spinner and no menu is busy NOT authoritatively (a turn that has not
/// drawn its spinner yet, or an overlay this grammar has not measured), and
/// with the command menu open nothing is evidence: the menu takes the
/// footer's place. A screen with no composer, box or wall is `idle` NOT
/// authoritatively.
#[must_use]
pub fn phase(rows: &[String]) -> (Phase, bool) {
    let Some(f) = frame(rows) else {
        if agy_box(rows).is_some() {
            return (Phase::Prompt, true);
        }
        return (Phase::Idle, signed_out(rows).is_some());
    };
    if spinning(rows, f.top) {
        return (Phase::Busy, true);
    }
    let footer = f.footer.map_or(Footer::Blank, |r| footer_of(&rows[r]));
    let idle = match footer {
        Footer::Shortcuts | Footer::ExitPending => true,
        Footer::Blank => draft(rows, f).iter().any(|l| !l.is_empty()),
        Footer::Cancel if !f.menu => return (Phase::Busy, false),
        Footer::Cancel | Footer::Other => false,
    };
    if !idle || f.menu {
        return (Phase::Idle, false);
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

/// A reply's row: two columns in or more, and none of agy's own marks (`⎿`
/// under a notice or a refusal, `└` under the spinner).
fn is_reply_row(row: &str) -> bool {
    leading_spaces(row) >= 2 && !crate::text::trim_start(row).starts_with(['⎿', '└']) && !blank(row)
}

/// The agent's last words: the reply under the last message, one line per
/// row, its indent stripped — the visible rows from the top when the
/// message scrolled away. `None` while a turn runs, before any message, and
/// when the turn ended on agy's own rows (an interrupt, an error, a tool
/// row and its output).
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
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
    let last = (from..f.top).rev().find(|&i| !blank(&rows[i]))?;
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
    Some(out.join("\n"))
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

/// The wall on the screen: agy signed out (`WallKind::Auth`; module header).
/// A person's browser lifts it; its login menu is never read.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    signed_out(rows).map(|row| Wall::sign_in(rows, row))
}

/// Whether `rows` are agy's by their layout alone, for a session whose
/// program name says nothing: its composer over a footer it draws, one of
/// its boxes, or its sign-in wall.
pub(crate) fn is_agy_screen(rows: &[String]) -> bool {
    frame(rows).is_some_and(|f| {
        f.footer
            .is_some_and(|r| footer_of(&rows[r]) != Footer::Other)
    }) || agy_box(rows).is_some()
        || signed_out(rows).is_some()
}

// ---------------------------------------------------------------------------
// The boxes.
// ---------------------------------------------------------------------------

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
    keys: usize,
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

/// The box on `rows` (module header): no composer, its key row the last row
/// drawn or over the footer's `esc to cancel` (or the model alone), its
/// options the block of rows above it with one of them focused in column 0.
pub(crate) fn agy_box(rows: &[String]) -> Option<AgyBox> {
    let end = rows.iter().rposition(|r| !blank(r))?;
    let keys = if is_key_row(&rows[end]) {
        end
    } else {
        if !matches!(footer_of(&rows[end]), Footer::Cancel | Footer::Blank) {
            return None;
        }
        let k = (0..end).rev().find(|&i| !blank(&rows[i]))?;
        if !is_key_row(&rows[k]) {
            return None;
        }
        k
    };
    let last = (0..keys).rev().find(|&i| !blank(&rows[i]))?;
    let mut top = last;
    while top > 0 && !blank(&rows[top - 1]) {
        top -= 1;
    }
    let block = top..=last;
    let first = match block.clone().find(|&i| numbered(&rows[i]).is_some()) {
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
    } else if label.starts_with(const { anchor_text("agy.box.session") }) {
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

/// The box on `rows`, read whole ([`PromptV2`]): its title and kind, the
/// command box's command (the rows under `Requesting permission for:`), a
/// file box's path (its first body row, less its `+1 -1` counts) and reason,
/// the trust box's folder, the question's text, the options with their
/// roles — wrapped tails joined back, every role [`Role::Other`] unless the
/// options run `1.`, `2.`, … (or are the trust box's two) with the cursor on
/// exactly one — chosen by digit (the trust box by the cursor and Enter),
/// and the Esc offered. A box whose title is off the rows read is named by
/// nothing ([`PromptKind::Other`], [`PromptV2::head_off_screen`]), its
/// options' roles the approval boxes' only where one of their one-shot
/// allows is among them.
#[must_use]
pub fn prompt(rows: &[String]) -> Option<PromptV2> {
    let b = agy_box(rows)?;
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
                });
            }
        } else if let Some((n, label, focused)) = numbered(row) {
            options.push(Opt {
                n: Some(n),
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
            });
        } else if let Some(o) = options.last_mut() {
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
    if ordered && focus == 1 && named {
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
    let footer = rows
        .iter()
        .rposition(|r| !blank(r))
        .filter(|&f| f > b.keys)
        .map(|f| rows[f].as_str());
    Some(PromptV2 {
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
        cancel: cancel_of(&rows[b.keys], footer),
        question,
        span: (title_row, b.keys),
        footer: Some(crate::text::trim(&rows[b.keys]).to_string()),
        head_off_screen,
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
    /// After `No, cancel` on the shell box, at 100 columns.
    pub const CANCELLED: &str = include_str!("fixtures/agy-1.4.0-cancelled-100x30.txt");
    /// After `No, deny creation` on the file box.
    pub const DECLINED: &str = include_str!("fixtures/agy-1.4.0-declined-interrupted.txt");
    /// After `No, reject this change` on the edit box.
    pub const REJECTED: &str = include_str!("fixtures/agy-1.4.0-rejected-interrupted.txt");
    /// `/` typed: the command menu under the composer.
    pub const SLASH_MENU: &str = include_str!("fixtures/agy-1.4.0-slash-menu.txt");
    /// One ctrl+c at an idle composer.
    pub const EXIT_CONFIRM: &str = include_str!("fixtures/agy-1.4.0-exit-confirm.txt");
    /// After the second ctrl+c: the resume line, then the shell.
    pub const EXITED: &str = include_str!("fixtures/agy-1.4.0-exited-resume.txt");
    /// Signed out: the login-method menu drawn instead of a composer.
    pub const SIGNED_OUT: &str = include_str!("fixtures/agy-1.4.0-wall-signed-out.txt");
}

#[cfg(test)]
#[path = "agy_tests.rs"]
mod tests;
