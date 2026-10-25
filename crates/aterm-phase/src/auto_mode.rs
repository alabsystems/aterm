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

/// Parse the auto-mode environment offer from `rows` ([`crate::prompt::parse_prompt_v2`]).
///
/// The offer is read only whole and only where Claude Code draws it: its footer is the last drawn
/// row (the inline renderer, under the composer's footer) or sits right on the composer's top
/// rule, a shell-mode composer's included (the fullscreen renderer); a full-width rule sits right
/// over its title; the title and the body, each joined across the rows a narrow pane wraps them
/// to, say the offer's words and nothing else; and its three answers are drawn in their order
/// with the `❯` on one of them.
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
    let footer = if is_footer(&rows[last]) {
        last
    } else {
        frame
            .and_then(|f| f.top.checked_sub(1))
            .filter(|&f| is_footer(&rows[f]))?
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
        || answers.len() != ANSWERS.len()
        || rows[title..heading]
            .iter()
            .chain(&rows[body])
            .any(|r| leading_spaces(r) != col)
        || prose != crate::anchor!("automode.body")
    {
        return None;
    }
    let mut options: Vec<Opt> = Vec::with_capacity(ANSWERS.len());
    for (at, (r, want)) in answers.zip(ANSWERS).enumerate() {
        let row = rows[r].as_str();
        let drawn = row
            .get(col..)
            .filter(|_| row.as_bytes()[..col].iter().all(|&b| b == b' '))?;
        let (n, label, focused) = crate::prompt::numbered_option(drawn, "❯ ", ". ")?;
        if usize::from(n) != at + 1 || label != want {
            return None;
        }
        options.push(Opt {
            n: Some(n),
            label: label.to_string(),
            role: if label == crate::anchor!("automode.not_now") {
                Role::Deny
            } else {
                Role::Other
            },
            focused,
            row: r,
            kept: false,
        });
    }
    if options.iter().filter(|o| o.focused).count() != 1 {
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
        span: (title, footer),
        footer: Some(crate::text::trim(&rows[footer]).to_string()),
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
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
