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
    use crate::question::cells;
    let mut rest = text;
    let mut lead = 0;
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
/// read whole, so it is not read.
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
    let mut blocks: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, row) in rows.iter().enumerate().take(footer).skip(title) {
        match blocks.last_mut() {
            _ if blank(row) => {}
            Some(b) if b.end == i => b.end = i + 1,
            _ => blocks.push(i..i + 1),
        }
    }
    let (heading, body) = blocks.split_first()?;
    let apart = blocks
        .windows(2)
        .all(|pair| pair[1].start == pair[0].end + 1);
    if heading.start != title
        || body.is_empty()
        || !apart
        || blocks.last().is_none_or(|b| b.end + 1 != footer)
    {
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
    Some(PromptV2 {
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
        span: (title, end),
        footer: Some(crate::text::trim(&crate::wrap::rejoined(&rows[footer..=end])).to_string()),
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
}

impl PromptV2 {
    /// The `Claude in Chrome` intro's one input, its footer's `Enter to confirm`, when this is
    /// that intro read whole ([`intro_box`]).
    #[must_use]
    pub fn chrome_intro_confirm(&self) -> Option<&Opt> {
        let [confirm] = self.options.as_slice() else {
            return None;
        };
        (self.kind == PromptKind::ChromeIntro
            && confirm.focused
            && confirm.n.is_none()
            && confirm.label == crate::anchor!("prompt.confirm"))
        .then_some(confirm)
    }
}

#[cfg(test)]
#[path = "chrome_intro_tests.rs"]
mod tests;
