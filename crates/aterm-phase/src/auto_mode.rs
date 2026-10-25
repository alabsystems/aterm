// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's auto-mode environment offer reader (`Teach auto mode about your environment?`).

use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};

/// The offer's answers in the order it draws them: `Yes` starts a setup scan of about a minute,
/// `Not now` (Esc too) puts the offer off for a week, `Don't show again` turns it off for good.
const ANSWERS: [&str; 3] = [
    "Yes",
    crate::anchor!("automode.not_now"),
    crate::anchor!("automode.never"),
];

fn is_footer(row: &str) -> bool {
    crate::text::trim(row).split_once(" · ")
        == Some((
            crate::anchor!("prompt.confirm"),
            crate::anchor!("prompt.cancel"),
        ))
}

/// The first row of the offer's footer when the footer ends on row `end`: `end` itself when the
/// footer is whole there, or the first of the rows a narrow pane wraps it onto, found by walking
/// up from `end` while each row continues the one above it ([`crate::prompt::wraps_onto`]) until
/// the rows, rejoined, say the footer's words or more than it holds. Measured on Claude Code
/// 2.1.310, the footer takes two rows in a pane 35 columns wide or narrower down to 20, three at
/// 19 and 17, and four at 16, the narrowest pane a split leaves.
fn footer_from(rows: &[String], end: usize) -> Option<usize> {
    let most = crate::anchor!("prompt.confirm").len()
        + " · ".len()
        + crate::anchor!("prompt.cancel").len();
    let width = crate::question::screen_width(rows);
    let mut first = end;
    loop {
        let run = crate::wrap::rejoined(&rows[first..=end]);
        if is_footer(&run) {
            return Some(first);
        }
        let above = first.checked_sub(1)?;
        if crate::text::trim(&run).len() >= most
            || !crate::prompt::wraps_onto(&rows[above], &rows[first], width)
        {
            return None;
        }
        first = above;
    }
}

/// Parse the auto-mode environment offer from `rows` ([`crate::prompt::parse_prompt_v2`]).
///
/// The offer is read only whole and only where Claude Code draws it: its footer, whole on one row
/// or wrapped onto two to four in a narrow pane, ends on the last drawn row (the inline renderer,
/// under the composer's footer) or right on the composer's top rule, a shell-mode composer's
/// included (the fullscreen renderer); a full-width rule sits right over its title; the title
/// and the body, each joined across the rows a narrow pane wraps them to, say the offer's words
/// and nothing else; and its three answers, each joined across the rows a narrow pane wraps its
/// label to, are drawn in their order with the `❯` on one of them.
pub(crate) fn offer_box(rows: &[String]) -> Option<PromptV2> {
    use crate::phase::{composer_frame, is_rule, leading_spaces, shell_mode_frame};
    let blank = |r: &String| crate::text::trim(r).is_empty();
    let joined = |rows: &[String]| {
        rows.iter()
            .map(|r| crate::text::trim(r))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let last = rows.iter().rposition(|r| !blank(r))?;
    let frame = composer_frame(rows).or_else(|| shell_mode_frame(rows));
    let (footer, end) = match footer_from(rows, last) {
        Some(first) => (first, last),
        None => {
            let end = frame.and_then(|f| f.top.checked_sub(1))?;
            (footer_from(rows, end)?, end)
        }
    };
    let rule = (0..footer).rev().find(|&i| is_rule(&rows[i]))?;
    let title = rule + 1;
    let heading = (title..footer).find(|&i| blank(&rows[i]))?;
    if heading == title
        || frame.is_some_and(|f| f.top <= rule && f.bottom >= rule)
        || joined(&rows[title..heading]) != crate::anchor!("automode.title")
    {
        return None;
    }
    let col = leading_spaces(&rows[title]);
    if !(1..=3).contains(&col)
        || rows[title + 1..footer]
            .iter()
            .any(|r| !blank(r) && leading_spaces(r) < col)
    {
        return None;
    }
    let mut blocks: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, row) in rows.iter().enumerate().take(footer).skip(heading) {
        match blocks.last_mut() {
            _ if blank(row) => {}
            Some(b) if b.end == i => b.end = i + 1,
            _ => blocks.push(i..i + 1),
        }
    }
    let [body, answers] = <[_; 2]>::try_from(blocks).ok()?;
    let prose = joined(&rows[body.clone()]);
    if answers.end == footer
        || rows[title..heading]
            .iter()
            .chain(&rows[body])
            .any(|r| leading_spaces(r) != col)
        || prose != crate::anchor!("automode.body")
    {
        return None;
    }
    let options = answers_in(rows, answers, col)?;
    if options.iter().filter(|o| o.focused).count() != 1
        || !options.iter().map(|o| o.label.as_str()).eq(ANSWERS)
        || !options
            .iter()
            .enumerate()
            .all(|(at, o)| o.n.is_some_and(|n| usize::from(n) == at + 1))
    {
        return None;
    }
    let title_text = crate::anchor!("automode.title");
    Some(PromptV2 {
        kind: PromptKind::AutoModeSetup,
        title: title_text.to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: title_text.to_string(),
        description: prose,
        notes: Vec::new(),
        auto_deny: None,
        path: None,
        options,
        select: Select::Digits,
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

/// The answers drawn on rows `answers` of a box at column `col`. Each row opens an answer (from
/// `col`, the `❯` and a space or two spaces, then its digit, `. ` and its label) or holds the rest
/// of the label above it, which a pane 22 columns wide or narrower wraps to the label's column
/// (onto three rows at 16): it [`crate::wrap::continues`] the label's row above it in the room
/// right of that column, which runs to the pane's edge (measured on Claude Code 2.1.310: at 17
/// columns `3. Don't show` fills its row to the edge, and at 23 `3. Don't show again` does).
fn answers_in(rows: &[String], answers: std::ops::Range<usize>, col: usize) -> Option<Vec<Opt>> {
    let width = crate::question::screen_width(rows);
    let mut options: Vec<Opt> = Vec::with_capacity(ANSWERS.len());
    let (mut label_col, mut above) = (0, "");
    for r in answers {
        let row = rows[r].as_str();
        let drawn = row
            .get(col..)
            .filter(|_| row.as_bytes()[..col].iter().all(|&b| b == b' '))?;
        if let Some((n, label, focused)) = crate::prompt::numbered_option(drawn, "❯ ", ". ") {
            // The cursor's two cells, the digits, and `. `.
            label_col = col + 2 + n.to_string().len() + 2;
            above = label;
            options.push(Opt {
                n: Some(n),
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: r,
                kept: false,
            });
            continue;
        }
        let opt = options.last_mut()?;
        if !crate::wrap::continues(above, label_col, row, width.saturating_sub(label_col)) {
            return None;
        }
        above = crate::text::trim(row);
        opt.label.push(' ');
        opt.label.push_str(above);
    }
    for opt in &mut options {
        if opt.label == crate::anchor!("automode.not_now") {
            opt.role = Role::Deny;
        }
    }
    Some(options)
}

impl PromptV2 {
    /// The auto-mode environment offer's `Not now` answer, when this is that offer read whole
    /// ([`offer_box`]).
    #[must_use]
    pub fn auto_mode_later(&self) -> Option<&Opt> {
        let labels = self.options.iter().map(|o| o.label.as_str());
        if self.kind != PromptKind::AutoModeSetup || !labels.eq(ANSWERS) {
            return None;
        }
        self.options
            .iter()
            .find(|o| o.label == crate::anchor!("automode.not_now"))
    }
}

#[cfg(test)]
#[path = "auto_mode_tests.rs"]
mod tests;
