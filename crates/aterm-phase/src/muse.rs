// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Muse Code's screen grammar (Meta's `muse`), MEASURED on muse
//! 1.4.9-R6297.1 (2026-10-16, a private headless aterm at 120×40, the
//! vendor's build under a scratch HOME: its offline `--provider echo`, and
//! the default provider signed out; fixtures in [`fixtures`], each naming its
//! provenance, colours and cursor on line 1). [`crate::reader::MuseReader`]
//! is this module behind the reader trait.
//!
//! **ITS APPROVAL AND QUESTION BOXES ARE UNMEASURED** (they need a login):
//! nothing here reads one, and where one is drawn the composer may stay on
//! the screen under it. So [`phase`] reads `idle` as evidence only at the
//! measured shapes — a session with no turn yet, or a turn whose reply is
//! the last thing drawn above the composer — and the supervisor does not
//! host Muse Code ([`crate::reader::Program::supervisable`]).
//!
//! **THE LAYOUT.** Past its trust box Muse Code draws inline, its composer a
//! Claude-Code lookalike — the caret is Claude Code's `❯` — whose top rule
//! carries a label:
//!
//! ```text
//!   Muse Code 1.4.9                              the banner
//! ❯ hello there                                  the person's message
//! ◇ Thinking (2s · esc to interrupt)             the STATUS row, while a turn
//!                                                  runs (◇, ◈ and ◆ cycle)
//! ◆ echo: hello there                            the reply, once it ended
//! ── Voice input (⌥ + v to start) ────────────   the COMPOSER: the labelled rule,
//! ❯                                                the caret and NO placeholder
//! ────────────────────────────────────────────     (the cursor at column 2), a
//!   echo · /path/to/work · Auto-review             rule and the footer
//! ```
//!
//! `/` opens the command menu BETWEEN the caret row and the bottom rule
//! (`  /compact   Summarize …`, `  ↓ 43 more`). One ctrl+c at an empty
//! composer turns the footer into `  Press Ctrl-C again to quit`.
//!
//! **TYPING.** An Enter in the same burst as the text is taken as a NEWLINE
//! (the draft grows a row); an Enter after a pause submits (measured).
//!
//! **AN INTERRUPT** (Esc in a turn) RETRACTS the turn: the message leaves the
//! transcript and comes back as the composer's draft, and no marker is drawn
//! — the screen is then that of a person's draft, and is read as one
//! ([`crate::reader::ScreenReader::interrupted`] stays `false`).
//!
//! **THE TRUST BOX** in a folder it has not seen: `⟩ 1  Trust and continue`
//! / `  2  Quit` over `Use Up/Down or 1/2, then Enter. Esc quits.`
//! **SIGNED OUT** it draws `Log in with browser · Enter to choose` over `Set
//! an API key` — whose Enter opens a browser on the person's desktop. It is
//! a WALL ([`wall`], `WallKind::Auth`) and never a box: no option of it is
//! read, so no policy can press one. A person signs in with `muse login` in
//! a shell.

use crate::anchors::anchor_text;
use crate::phase::{Phase, is_rule, leading_spaces, plain_rule_width};
use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};
use crate::text::blank;
use crate::wall::Wall;

/// The composer, read whole.
#[derive(Debug, Clone, Copy)]
struct Frame {
    top: usize,
    caret: usize,
    bottom: usize,
}

/// The composer ([`Frame`]): the last `❯` row in column 0, a rule right
/// above it (labelled or not), the first plain rule under it, and the footer
/// right under that at column 2 as the last row drawn. Muse Code erases its
/// composer when it exits.
fn frame(rows: &[String]) -> Option<Frame> {
    let caret = rows.iter().rposition(|r| r.starts_with('❯'))?;
    let top = caret.checked_sub(1)?;
    if !is_rule(&rows[top]) {
        return None;
    }
    let bottom = (caret + 1..rows.len()).find(|&i| plain_rule_width(&rows[i]).is_some())?;
    let footer = bottom + 1;
    let last = rows.iter().rposition(|r| !blank(r))?;
    (last == footer && leading_spaces(&rows[footer]) == 2).then_some(Frame { top, caret, bottom })
}

/// The composer's caret row.
#[must_use]
pub fn composer(rows: &[String]) -> Option<usize> {
    frame(rows).map(|f| f.caret)
}

/// A row of the `/` command menu, drawn under the caret row while the draft
/// opens with `/` (`  /compact   Summarize …`, `  ↓ 43 more`, and `↑ N more`
/// over the rows once it scrolled).
fn is_menu_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    leading_spaces(row) == 2 && (t.starts_with('/') || t.starts_with("↓ ") || t.starts_with("↑ "))
}

/// The composer's text as `(caret row, lines)`: the caret row's text first
/// (its `❯` stripped — empty when nothing is typed: Muse Code draws no
/// placeholder), then each row down to the bottom rule, the command menu's
/// rows aside.
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let f = frame(rows)?;
    let caret = rows[f.caret].strip_prefix('❯').unwrap_or(&rows[f.caret]);
    let caret = crate::text::trim(caret);
    let menu = caret.starts_with('/');
    let mut lines = vec![caret.to_string()];
    lines.extend(
        rows[f.caret + 1..f.bottom]
            .iter()
            .filter(|r| !(menu && is_menu_row(r)))
            .map(|r| crate::text::trim(r).to_string()),
    );
    Some((f.caret, lines))
}

/// The diamonds the status row cycles through in column 0 (measured live: ◇,
/// ◈ and ◆ over 381 status frames of three turns).
const STATUS_GLYPHS: [char; 3] = ['◇', '◈', '◆'];

/// The status row while a turn runs: `◆ Thinking (2s · esc to interrupt)` —
/// one of [`STATUS_GLYPHS`] in column 0 (the verb `Working`, then
/// `Thinking`) and the Esc hint's parenthesis ending the row.
fn is_status_row(row: &str) -> bool {
    let hint = const { anchor_text("muse.busy.interrupt") };
    row.starts_with(STATUS_GLYPHS)
        && crate::text::trim_end(row)
            .strip_suffix(')')
            .is_some_and(|r| r.ends_with(hint))
}

/// A reply's head: `◆ <text>` in column 0, no status row.
fn is_reply_head(row: &str) -> bool {
    row.starts_with("◆ ") && !is_status_row(row)
}

/// Muse Code's banner naming its build: `  Muse Code 1.4.9`.
fn is_banner_row(row: &str) -> bool {
    crate::text::trim(row)
        .strip_prefix(const { anchor_text("muse.banner") })
        .and_then(|v| v.strip_prefix(' '))
        .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

/// Where the last turn stands, read above the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    /// The status row is up.
    Running,
    /// A message with nothing under it yet: sent, its status row not drawn.
    Sent,
    /// The reply whose head is on this row is the last thing drawn.
    Ended(usize),
    /// No turn yet: the banner and no message.
    Fresh,
    /// Nothing measured says: rows this grammar does not know under the
    /// last message, or its head scrolled away with no reply in sight.
    Unknown,
}

fn turn(rows: &[String], top: usize) -> Turn {
    let user = (0..top).rev().find(|&i| rows[i].starts_with('❯'));
    let from = user.map_or(0, |u| u + 1);
    if rows[from..top].iter().any(|r| is_status_row(r)) {
        return Turn::Running;
    }
    if user.is_none() && rows[..top].iter().any(|r| is_banner_row(r)) {
        return Turn::Fresh;
    }
    let Some(last) = (from..top).rev().find(|&i| !blank(&rows[i])) else {
        return if user.is_some() {
            Turn::Sent
        } else {
            Turn::Unknown
        };
    };
    // The reply is the last block: its head the last row in column 0, every
    // row under it indented.
    match (from..=last)
        .rev()
        .find(|&i| leading_spaces(&rows[i]) == 0 && !blank(&rows[i]))
    {
        Some(head) if is_reply_head(&rows[head]) => Turn::Ended(head),
        _ => Turn::Unknown,
    }
}

/// The phase, and whether it is evidence (module header): the trust box is
/// a prompt; the status row is busy; a reply as the last thing drawn above
/// the composer is idle — a question when its last words end in `?` — and
/// so is a session with no turn yet; the sign-in wall is idle on its wall. A
/// message with nothing under it is busy NOT authoritatively (its status row
/// not drawn yet), and any other screen is `idle` NOT authoritatively.
#[must_use]
pub fn phase(rows: &[String]) -> (Phase, bool) {
    if trust_box(rows).is_some() {
        return (Phase::Prompt, true);
    }
    let Some(f) = frame(rows) else {
        return (Phase::Idle, signed_out(rows).is_some());
    };
    match turn(rows, f.top) {
        Turn::Running => (Phase::Busy, true),
        Turn::Sent => (Phase::Busy, false),
        Turn::Fresh => (Phase::Idle, true),
        Turn::Unknown => (Phase::Idle, false),
        Turn::Ended(_) => {
            let asks = said_tail(rows).is_some_and(|t| crate::turn::asks(&t));
            (if asks { Phase::Question } else { Phase::Idle }, true)
        }
    }
}

/// No turn yet: the banner above the composer and no message.
#[must_use]
pub fn fresh(rows: &[String]) -> bool {
    frame(rows).is_some_and(|f| turn(rows, f.top) == Turn::Fresh)
}

/// The agent's last words: the reply the last turn ended on, its `◆` and
/// indent stripped, one line per row. `None` while a turn runs, before any
/// turn, and where [`phase`] cannot say the turn ended.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    let f = frame(rows)?;
    let Turn::Ended(head) = turn(rows, f.top) else {
        return None;
    };
    let first = rows[head].strip_prefix('◆').unwrap_or(&rows[head]);
    let out: Vec<&str> = std::iter::once(crate::text::trim(first))
        .chain(rows[head + 1..f.top].iter().map(|r| crate::text::trim(r)))
        .filter(|t| !t.is_empty())
        .collect();
    Some(out.join("\n"))
}

/// The sign-in wall's first option row (`Log in with browser · Enter to
/// choose`): drawn instead of a composer, its key row `↓↑ to select · Esc to
/// quit` the last row drawn.
fn signed_out(rows: &[String]) -> Option<usize> {
    let end = rows.iter().rposition(|r| !blank(r))?;
    if crate::text::trim(&rows[end]) != const { anchor_text("muse.signin.keys") } {
        return None;
    }
    rows[..end].iter().rposition(|r| {
        crate::text::trim(r).starts_with(const { anchor_text("muse.signin.browser") })
    })
}

/// The wall on the screen: Muse Code signed out (`WallKind::Auth`; module
/// header). A person's browser lifts it; its login menu is never read.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    signed_out(rows).map(|row| Wall::sign_in(rows, row))
}

/// `⟩ 1  Trust and continue` / `  2  Quit` → `(1, "Trust and continue",
/// focused)`: the number, two blanks, the label.
fn option(row: &str) -> Option<(u8, &str, bool)> {
    crate::prompt::numbered_option(row, "⟩ ", "  ")
}

/// The trust box as `(title row, its option rows, key row)`: `Do you trust
/// this workspace?`, the numbered options under it, and the key row ending
/// `Esc quits.` as the last row drawn.
fn trust_box(rows: &[String]) -> Option<(usize, std::ops::Range<usize>, usize)> {
    let keys = rows.iter().rposition(|r| !blank(r))?;
    if !crate::text::trim(&rows[keys]).ends_with(const { anchor_text("muse.trust.keys") }) {
        return None;
    }
    let title = rows[..keys]
        .iter()
        .position(|r| crate::text::trim(r) == const { anchor_text("muse.trust.title") })?;
    let first = (title + 1..keys).find(|&i| option(&rows[i]).is_some())?;
    let end = (first..keys)
        .find(|&i| option(&rows[i]).is_none())
        .unwrap_or(keys);
    Some((title, first..end, keys))
}

/// The box on `rows`, read whole ([`PromptV2`]): the trust box — the one box
/// measured — its folder, `Trust and continue` the trust and `Quit` the
/// exit (roles only where the options run `1`, `2` with the cursor on
/// exactly one), chosen by the cursor (or a digit) and then Enter, its Esc
/// quitting Muse Code.
#[must_use]
pub fn prompt(rows: &[String]) -> Option<PromptV2> {
    let (title, opts, keys) = trust_box(rows)?;
    let mut options: Vec<Opt> = opts
        .clone()
        .filter_map(|i| {
            option(&rows[i]).map(|(n, label, focused)| Opt {
                n: Some(n),
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
            })
        })
        .collect();
    let ordered = options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.is_some_and(|n| usize::from(n) == k + 1));
    if ordered && options.iter().filter(|o| o.focused).count() == 1 {
        for o in &mut options {
            o.role = if o.label == const { anchor_text("muse.trust.yes") } {
                Role::Trust
            } else if o.label == const { anchor_text("muse.trust.quit") } {
                Role::Exit
            } else {
                Role::Other
            };
        }
    }
    let path = rows[title + 1..opts.start].iter().find_map(|r| {
        crate::text::trim(r)
            .strip_prefix(const { anchor_text("muse.trust.path") })
            .map(|p| crate::text::trim(p).to_string())
    });
    let footer = crate::text::trim(&rows[keys]).to_string();
    let verb = footer
        .to_lowercase()
        .split_once("esc ")
        .map(|(_, v)| v.trim().trim_end_matches('.').to_string())
        .unwrap_or_default();
    Some(PromptV2 {
        kind: PromptKind::Trust,
        title: crate::text::trim(&rows[title]).to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: path.clone().unwrap_or_default(),
        description: String::new(),
        notes: Vec::new(),
        auto_deny: None,
        path,
        options,
        select: Select::ArrowsEnter,
        cancel: Some(Cancel {
            key: "esc".to_string(),
            verb,
            effect: CancelEffect::Exit,
        }),
        question: Some(title),
        span: (title, keys),
        footer: Some(footer),
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
}

/// Whether `rows` are Muse Code's by their layout alone, for a session whose
/// program name says nothing: its composer under the rule carrying its own
/// label (`── Voice input (⌥ + v to start) ──`) — which Claude Code's frame
/// test takes for Claude Code's, so [`crate::reader::identify`] asks this
/// first — its trust box, or its sign-in wall.
pub(crate) fn is_muse_screen(rows: &[String]) -> bool {
    frame(rows).is_some_and(|f| {
        crate::text::contains(&rows[f.top], const { anchor_text("muse.composer.voice") })
    }) || trust_box(rows).is_some()
        || signed_out(rows).is_some()
}

/// Muse Code's screens, measured, for this crate's tests and a dependent
/// crate's `#[cfg(test)]` code; not part of the documented API. Read them
/// with [`crate::prompt::fixtures::screen`].
#[doc(hidden)]
pub mod fixtures {
    /// The workspace-trust box, drawn from row 0.
    pub const BOX_TRUST: &str = include_str!("fixtures/muse-1.4.9-R6297.1-box-trust.txt");
    /// Idle at its composer, no turn yet.
    pub const IDLE: &str = include_str!("fixtures/muse-1.4.9-R6297.1-idle.txt");
    /// A typed, unsent draft.
    pub const DRAFT: &str = include_str!("fixtures/muse-1.4.9-R6297.1-draft.txt");
    /// 2 s into a turn: the status row over the open composer.
    pub const BUSY: &str = include_str!("fixtures/muse-1.4.9-R6297.1-busy-thinking.txt");
    /// A turn running with the status row's third diamond, `◈`, under seven
    /// earlier turns.
    pub const BUSY_ALT_GLYPH: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-busy-thinking-alt-glyph.txt");
    /// The end of a turn: the reply, the composer idle.
    pub const END_OF_TURN: &str = include_str!("fixtures/muse-1.4.9-R6297.1-end-of-turn.txt");
    /// Esc in a turn: the message back in the composer as its draft.
    pub const INTERRUPTED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-interrupted-draft-restored.txt");
    /// `/` typed: the command menu between the caret row and the bottom rule.
    pub const SLASH_MENU: &str = include_str!("fixtures/muse-1.4.9-R6297.1-slash-menu.txt");
    /// One ctrl+c at an empty composer.
    pub const EXIT_CONFIRM: &str = include_str!("fixtures/muse-1.4.9-R6297.1-exit-confirm.txt");
    /// After the second ctrl+c: the resume line, then the shell.
    pub const EXITED: &str = include_str!("fixtures/muse-1.4.9-R6297.1-exited-resume.txt");
    /// Signed out: the login menu drawn instead of a composer.
    pub const SIGNED_OUT: &str = include_str!("fixtures/muse-1.4.9-R6297.1-wall-signed-out.txt");
}

#[cfg(test)]
#[path = "muse_tests.rs"]
mod tests;
