// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's `Claude in Chrome` intro reader: the first-use dialog a session started with
//! Claude in Chrome on (as `--chrome` turns it on) draws before its REPL, until its config records
//! the intro shown. It offers no answers to pick: its footer's `Enter to confirm · Esc to cancel`
//! is the whole of its input, and in 2.1.317's bundle both keys do the same thing (each calls the
//! dialog's `onDone`; the intro records itself shown when it is drawn), so Chrome stays on for the
//! session either way.

use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};

/// The intro's paragraphs in the order it draws them, in each of its two forms (2.1.317's bundle):
/// while its extension check has not found the extension, the body, the `Requires…` paragraph
/// with the store link, the permissions paragraph closed by `.`, and the `For more info`
/// paragraph; once the check finds it, no `Requires…` paragraph, and the permissions paragraph
/// closed by the permissions link in parentheses and `.`.
fn paragraphs() -> [Vec<String>; 2] {
    let body = crate::anchor!("chrome.intro_body");
    let permissions = crate::anchor!("chrome.intro_permissions");
    let more = format!(
        "{} {} {} {}",
        crate::anchor!("chrome.intro_more"),
        crate::anchor!("chrome.intro_command"),
        crate::anchor!("chrome.intro_visit"),
        crate::anchor!("chrome.intro_docs_url"),
    );
    [
        vec![
            body.to_string(),
            format!(
                "{} {}",
                crate::anchor!("chrome.intro_requires"),
                crate::anchor!("chrome.intro_store_url"),
            ),
            format!("{permissions}."),
            more.clone(),
        ],
        vec![
            body.to_string(),
            format!(
                "{permissions} ({}).",
                crate::anchor!("chrome.intro_permissions_url"),
            ),
            more,
        ],
    ]
}

/// Whether rows `run` say `text` as Ink wraps it into a box `room` cells wide: each row's text
/// (trimmed) is the next stretch of `text`, none empty, and between two rows `text` holds one
/// space (a word wrap: Ink draws no space at either end of a row) or none. None is a word wider
/// than the box broken at the box's edge: the word is wider than `room`, and the upper row fills
/// the room, counting the cell an exactly full row before it leaves unused at its head
/// ([`crate::wrap::lead`]). A URL wider than a narrow pane's box is drawn so.
fn drawn_as(run: &[String], text: &str, room: usize) -> bool {
    drawn_from(run, text, room, 0)
}

/// [`drawn_as`] from a row `lead` cells into the room ([`crate::wrap::lead`]): the first row of a
/// wrap read from part-way through it.
fn drawn_from(run: &[String], text: &str, room: usize, lead: usize) -> bool {
    use crate::question::cells;
    let mut rest = text;
    let mut lead = lead;
    for (k, row) in run.iter().enumerate() {
        let drawn = crate::text::trim(row);
        let Some(after) = rest.strip_prefix(drawn).filter(|_| !drawn.is_empty()) else {
            return false;
        };
        rest = after;
        if k + 1 == run.len() {
            break;
        }
        let used = lead + cells(drawn);
        if let Some(after) = rest.strip_prefix(' ') {
            rest = after;
            lead = crate::wrap::lead(used, room);
        } else {
            let head = drawn.rsplit(' ').next().unwrap_or(drawn);
            let tail = rest.split(' ').next().unwrap_or(rest);
            if used != room || cells(head) + cells(tail) <= room {
                return false;
            }
            lead = 0;
        }
    }
    rest.is_empty()
}

/// Parse the `Claude in Chrome` intro from `rows` ([`crate::prompt::parse_prompt_v2`]).
///
/// The intro is read only whole and only where Claude Code draws it, before its REPL and with
/// nothing under it, in either renderer (measured in the real-Claude lab, 2026-11-03: the
/// fullscreen renderer draws it as the inline one does, and a running session's `/chrome` opens
/// the setup menu instead): its footer, whole on one row or wrapped onto two to four in a narrow
/// pane ([`crate::auto_mode::footer_from`]), ends on the last drawn row; a full-width rule that is
/// no rule of a composer's frame sits right over its title; every drawn row from the title to the
/// footer's first row is at the title's column, 1 to 3; the title and each paragraph are blocks of
/// rows one blank row apart, and one blank row stands over the footer; and the title and the
/// paragraphs, each read across the rows the box wraps it to ([`drawn_as`], in a box as wide as
/// the rule less the title's column on each side), say the intro's words in one of its two forms
/// ([`paragraphs`]) and nothing else. An intro taller than the pane, its head scrolled off, is not
/// read whole, so it is not read here: [`intro_cut`] reads it cut.
pub(crate) fn intro_box(rows: &[String]) -> Option<PromptV2> {
    use crate::phase::{composer_frame, is_rule, leading_spaces, shell_mode_frame};
    let blank = |r: &String| crate::text::trim(r).is_empty();
    let end = rows.iter().rposition(|r| !blank(r))?;
    let footer = crate::auto_mode::footer_from(rows, end)?;
    let frame = composer_frame(rows).or_else(|| shell_mode_frame(rows));
    let rule = (0..footer).rev().find(|&i| is_rule(&rows[i]))?;
    if frame.is_some_and(|f| f.top <= rule && f.bottom >= rule) {
        return None;
    }
    let title = rule + 1;
    let col = leading_spaces(&rows[title]);
    if !(1..=3).contains(&col)
        || leading_spaces(&rows[footer]) != col
        || rows[title..footer]
            .iter()
            .any(|r| !blank(r) && leading_spaces(r) != col)
    {
        return None;
    }
    let blocks = blocks_of(rows, title, footer);
    let (heading, body) = blocks.split_first()?;
    if heading.start != title || body.is_empty() || !apart(&blocks, footer) {
        return None;
    }
    let width = crate::text::trim_end(&rows[rule]).chars().count();
    let room = crate::wrap::room(col, width);
    let heading_text = crate::anchor!("chrome.intro_heading");
    if !drawn_as(&rows[heading.clone()], heading_text, room) {
        return None;
    }
    let said = paragraphs().into_iter().find(|texts| {
        texts.len() == body.len()
            && texts
                .iter()
                .zip(body)
                .all(|(text, b)| drawn_as(&rows[b.clone()], text, room))
    })?;
    Some(intro(rows, (title, end), footer, &said, false))
}

/// The drawn rows from `first` to `footer`, as blocks of rows ([`intro_box`]).
fn blocks_of(rows: &[String], first: usize, footer: usize) -> Vec<std::ops::Range<usize>> {
    let mut blocks: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, row) in rows.iter().enumerate().take(footer).skip(first) {
        match blocks.last_mut() {
            _ if crate::text::trim(row).is_empty() => {}
            Some(b) if b.end == i => b.end = i + 1,
            _ => blocks.push(i..i + 1),
        }
    }
    blocks
}

/// Whether `blocks` stand one blank row apart, the last one blank row over `footer`.
fn apart(blocks: &[std::ops::Range<usize>], footer: usize) -> bool {
    blocks
        .windows(2)
        .all(|pair| pair[1].start == pair[0].end + 1)
        && blocks.last().is_some_and(|b| b.end + 1 == footer)
}

/// Parse the `Claude in Chrome` intro with its head cut off `rows` (a pane, or the rows a reader
/// was given, that starts below its rule): the footer ends on the last drawn row as in
/// [`intro_box`], no rule stands above it, the first drawn row is the first row or the one under
/// it, every drawn row down to the footer's first is at the footer's column, 1 to 3; the blocks
/// stand one blank row apart, one blank row over the footer; and they say the last of the title
/// and the paragraphs of one of the intro's forms, in order, in a box whose room is the widest
/// drawn row or one more (a hard break fills it; the rule that gives it is cut). A first block
/// on the first row is read from a row of its text's wrap ([`drawn_tail`]); one under a blank
/// first row opens a paragraph, never the title, whose rule is no blank row. The read is `head_off_screen`: a reader
/// reads the rows above it again, a supervisor joins its head from the history, and its confirm
/// is never pressed ([`PromptV2::chrome_intro_confirm`]).
pub(crate) fn intro_cut(rows: &[String]) -> Option<PromptV2> {
    use crate::phase::{is_rule, leading_spaces};
    use crate::question::cells;
    let blank = |r: &String| crate::text::trim(r).is_empty();
    let end = rows.iter().rposition(|r| !blank(r))?;
    let footer = crate::auto_mode::footer_from(rows, end)?;
    let first = rows.iter().position(|r| !blank(r))?;
    let col = leading_spaces(&rows[footer]);
    if first > 1
        || first >= footer
        || rows[..footer].iter().any(|r| is_rule(r))
        || !(1..=3).contains(&col)
        || rows[first..footer]
            .iter()
            .any(|r| !blank(r) && leading_spaces(r) != col)
    {
        return None;
    }
    let blocks = blocks_of(rows, first, footer);
    if blocks.first().is_none_or(|b| b.start != first) || !apart(&blocks, footer) {
        return None;
    }
    let widest = rows[first..=end]
        .iter()
        .map(|r| cells(crate::text::trim(r)))
        .max()?;
    let (cut, whole) = blocks.split_first()?;
    let heading = crate::anchor!("chrome.intro_heading");
    let said = paragraphs().into_iter().find(|texts| {
        let all: Vec<&str> = std::iter::once(heading)
            .chain(texts.iter().map(String::as_str))
            .collect();
        let Some(from) = all.len().checked_sub(blocks.len()) else {
            return false;
        };
        // A blank first row is the blank row between two blocks, so the block under it opens
        // its paragraph; the title's rule, never a blank row, stands over the title.
        if first == 1 && from == 0 {
            return false;
        }
        [widest, widest + 1].into_iter().any(|room| {
            let opens = if first == 0 {
                drawn_tail(&rows[cut.clone()], all[from], room)
            } else {
                drawn_as(&rows[cut.clone()], all[from], room)
            };
            opens
                && all[from + 1..]
                    .iter()
                    .zip(whole)
                    .all(|(text, b)| drawn_as(&rows[b.clone()], text, room))
        })
    })?;
    Some(intro(rows, (first, end), footer, &said, true))
}

/// Whether rows `run` say the end of `text` as Ink wraps it into a box `room` cells wide: from a
/// row its wrap starts on, after a space (a word wrap) or inside a word wider than the room (a
/// hard break), that row's text `0` or `1` cell into the room ([`drawn_from`]).
fn drawn_tail(run: &[String], text: &str, room: usize) -> bool {
    use crate::question::cells;
    let Some(head) = run.first().map(|r| crate::text::trim(r)) else {
        return false;
    };
    if head.is_empty() {
        return false;
    }
    text.char_indices()
        .map(|(at, _)| at)
        .filter(|&at| text[at..].starts_with(head))
        .any(|at| {
            let before = &text[..at];
            let starts = at == 0 || before.ends_with(' ') || {
                let word = before.rfind(' ').map_or(0, |i| i + 1);
                let word_end = text[at..].find(' ').map_or(text.len(), |i| at + i);
                cells(&text[word..word_end]) > room
            };
            starts && (0..=1).any(|lead| drawn_from(run, &text[at..], room, lead))
        })
}

/// The intro as read: its title, `said` paragraphs, the footer's Enter as its one input on the
/// footer's first row `footer`, and `span`, its rows from the title (or the first drawn row of a
/// cut) to the footer's last.
fn intro(
    rows: &[String],
    span: (usize, usize),
    footer: usize,
    said: &[String],
    head_off_screen: bool,
) -> PromptV2 {
    let heading_text = crate::anchor!("chrome.intro_heading");
    PromptV2 {
        kind: PromptKind::ChromeIntro,
        title: heading_text.to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: heading_text.to_string(),
        description: said.join(" "),
        notes: Vec::new(),
        auto_deny: None,
        path: None,
        // The intro draws no answers: the footer's Enter is its one input, read as the one
        // option, focused, on the footer's first row.
        options: vec![Opt {
            n: None,
            label: crate::anchor!("prompt.confirm").to_string(),
            role: Role::Other,
            focused: true,
            row: footer,
            kept: false,
        }],
        select: Select::ArrowsEnter,
        cancel: Some(Cancel {
            key: "Esc".to_string(),
            verb: "cancel".to_string(),
            effect: CancelEffect::Back,
        }),
        question: None,
        span,
        footer: Some(crate::text::trim(&crate::wrap::rejoined(&rows[footer..=span.1])).to_string()),
        head_off_screen,
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

/// The intro on `rows`, read whole ([`intro_box`]) or cut ([`intro_cut`]).
pub(crate) fn intro_on(rows: &[String]) -> Option<PromptV2> {
    intro_box(rows).or_else(|| intro_cut(rows))
}

impl PromptV2 {
    /// The `Claude in Chrome` intro's one input, its footer's `Enter to confirm`, when this is
    /// that intro read whole ([`intro_box`]): never on one read cut ([`intro_cut`]).
    #[must_use]
    pub fn chrome_intro_confirm(&self) -> Option<&Opt> {
        let [confirm] = self.options.as_slice() else {
            return None;
        };
        (self.kind == PromptKind::ChromeIntro
            && !self.head_off_screen
            && confirm.focused
            && confirm.n.is_none()
            && confirm.label == crate::anchor!("prompt.confirm"))
        .then_some(confirm)
    }
}

#[cfg(test)]
#[path = "chrome_intro_tests.rs"]
mod tests;
