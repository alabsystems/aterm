// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code autocompact offer reader.

use crate::prompt::{Opt, PromptKind, PromptV2, Role, Select};

/// Default autocompact stepper sizes in kilo-tokens ([`accept_size`]).
pub const LADDER: [u32; 7] = [300, 400, 500, 600, 700, 800, 900];

/// Which of the offer's two options the `❯` is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferFocus {
    /// `Autocompact at ‹ <size> › tokens`.
    Accept,
    /// `Keep …`.
    Keep,
}

/// The autocompact offer, read whole ([`PromptV2::autocompact_offer`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The accept option's index in [`PromptV2::options`].
    pub accept: usize,
    /// The keep option's index in [`PromptV2::options`].
    pub keep: usize,
    /// Token threshold currently shown on the accept row ([`size_words`]).
    pub tokens: u32,
    pub focused: OfferFocus,
}

/// Parse `Autocompact at ‹ 400k › tokens` into its size in tokens ([`size_tokens`]).
#[must_use]
pub fn accept_size(label: &str) -> Option<u32> {
    let accept = const { crate::anchor!("autocompact.accept") };
    let size = label
        .strip_prefix(accept)?
        .strip_prefix(" \u{2039} ")?
        .strip_suffix(" \u{203A} tokens")?;
    size_tokens(size)
}

/// A size in the words Claude Code draws it in, as tokens.
#[must_use]
pub fn size_tokens(text: &str) -> Option<u32> {
    if text == "1m" {
        return Some(1_000_000);
    }
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    let number = text.strip_suffix('k')?;
    let (whole, tenth) = match number.split_once('.') {
        Some((w, t)) => (w, Some(t)),
        None => (number, None),
    };
    if !digits(whole) || whole.len() > 3 || whole.starts_with('0') {
        return None;
    }
    let tenth: u32 = match tenth {
        None => 0,
        Some(t) if t.len() == 1 && digits(t) && t != "0" => t.parse().ok()?,
        Some(_) => return None,
    };
    Some(whole.parse::<u32>().ok()? * 1000 + tenth * 100)
}

/// `tokens` in the words Claude Code draws a size in ([`size_tokens`]'s inverse on its words).
#[must_use]
pub fn size_words(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        return "1m".to_string();
    }
    match (tokens / 1000, tokens % 1000 / 100) {
        (k, 0) => format!("{k}k"),
        (k, tenth) => format!("{k}.{tenth}k"),
    }
}

fn keep_current(label: &str) -> Option<&str> {
    let rest = label.strip_prefix("Keep ")?;
    let amount = |t: &str| {
        let digits = t.trim_end_matches(['k', 'm']);
        !digits.is_empty()
            && digits.len() < t.len()
            && t.len() - digits.len() == 1
            && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
            && digits.starts_with(|c: char| c.is_ascii_digit())
    };
    let ok = match rest.strip_prefix("auto (") {
        Some(inner) => inner
            .strip_suffix(" tokens)")
            .is_some_and(|a| amount(a) && !a.contains(' ')),
        None => rest
            .strip_suffix(" tokens")
            .is_some_and(|a| amount(a) && !a.contains(' ')),
    };
    ok.then_some(rest)
}

const DESCRIPTION: &str = "This conversation is at ";

fn strip_inline(drawn: &str) -> (&str, Option<&str>) {
    match drawn.find(&format!("  {DESCRIPTION}")) {
        Some(at) => (
            crate::text::trim(&drawn[..at]),
            Some(crate::text::trim(&drawn[at..])),
        ),
        None => (drawn, None),
    }
}

fn option_row(row: &str) -> Option<(usize, bool, &str)> {
    let col = crate::phase::leading_spaces(row);
    let t = crate::text::trim(row);
    if let Some(rest) = t.strip_prefix('❯') {
        let label = rest.strip_prefix(' ')?;
        return Some((col, true, crate::text::trim(label)));
    }
    Some((col.checked_sub(2)?, false, t))
}

fn is_title_row(row: &str) -> bool {
    let title = const { crate::anchor!("autocompact.title") };
    let t = crate::text::trim(row);
    t == title
        || t.strip_prefix(title).is_some_and(|rest| {
            rest.starts_with("  ")
                && rest
                    .trim()
                    .split_once(" of ")
                    .is_some_and(|(n, m)| n.parse::<u8>().is_ok() && m.parse::<u8>().is_ok())
        })
}

/// Parse an autocompact offer box from `rows` ([`crate::prompt::parse_prompt_v2`]).
pub(crate) fn offer_box(rows: &[String]) -> Option<PromptV2> {
    use crate::phase::{has_composer_frame, is_rule, leading_spaces};
    let footer_text = const { crate::anchor!("autocompact.footer") };
    let more = const { crate::anchor!("autocompact.more_above") };
    let blank = |r: &String| crate::text::trim(r).is_empty();
    let last = rows.iter().rposition(|r| !blank(r))?;
    let footer = if crate::text::trim(&rows[last]) == more {
        rows[..last].iter().rposition(|r| !blank(r))?
    } else {
        last
    };
    if crate::text::trim(&rows[footer]) != footer_text || has_composer_frame(rows) {
        return None;
    }
    let rule = (0..footer)
        .rev()
        .find(|&i| is_rule(&rows[i]) || crate::prompt::is_panel_rule(&rows[i]))?;
    let title = (rule + 1..footer).find(|&i| !blank(&rows[i]))?;
    let col = leading_spaces(&rows[title]);
    if !(1..=3).contains(&col) || !is_title_row(&rows[title]) {
        return None;
    }
    if rows[title..=footer]
        .iter()
        .any(|r| !blank(r) && leading_spaces(r) < col)
    {
        return None;
    }
    let mut block: Vec<usize> = Vec::new();
    let mut i = footer;
    while i > title + 1 {
        i -= 1;
        if blank(&rows[i]) {
            if block.is_empty() {
                continue;
            }
            break;
        }
        block.push(i);
    }
    block.reverse();
    let body_end = *block.first()?;
    let mut options: Vec<Opt> = Vec::new();
    let mut pointer_col: Option<usize> = None;
    let mut size = None;
    let mut current = None;
    let mut described: Vec<&str> = Vec::new();
    let mut in_description = false;
    for &r in &block {
        let t = crate::text::trim(&rows[r]);
        let after_accept = options
            .last()
            .is_some_and(|o| accept_size(&o.label).is_some());
        let opt = option_row(&rows[r]).filter(|(_, _, label)| {
            accept_size(strip_inline(label).0).is_some() || keep_current(label).is_some()
        });
        let Some((pc, is_focused, drawn)) = opt else {
            let starts = after_accept && t.starts_with(DESCRIPTION) && described.is_empty();
            if !(starts || in_description) || leading_spaces(&rows[r]) < pointer_col? + 2 {
                return None;
            }
            in_description = true;
            described.push(t);
            continue;
        };
        in_description = false;
        if *pointer_col.get_or_insert(pc) != pc {
            return None;
        }
        let (label, inline) = strip_inline(drawn);
        if let Some(d) = inline {
            described.push(d);
        }
        let again = match accept_size(label) {
            Some(s) => size.replace(s).is_some(),
            None => keep_current(label).is_some_and(|c| current.replace(c.to_string()).is_some()),
        };
        if again {
            return None;
        }
        if is_focused && options.iter().any(|o| o.focused) {
            return None;
        }
        options.push(Opt {
            n: None,
            label: label.to_string(),
            role: Role::Other,
            focused: is_focused,
            row: r,
            kept: false,
        });
    }
    let (size, _current) = (size?, current?);
    if options.len() != 2 || !options.iter().any(|o| o.focused) {
        return None;
    }
    let body: Vec<&str> = rows[title + 1..body_end]
        .iter()
        .map(|r| crate::text::trim(r))
        .filter(|t| !t.is_empty())
        .collect();
    let prose = body.join(" ");
    let question = const { crate::anchor!("autocompact.question") };
    if !prose.contains(&format!("{question} {} tokens instead?", size_words(size))) {
        return None;
    }
    let description = if described.is_empty() {
        prose
    } else {
        format!("{prose}\n{}", described.join(" "))
    };
    let accept_label = options
        .iter()
        .find(|o| accept_size(&o.label).is_some())
        .map(|o| o.label.clone())
        .unwrap_or_default();
    Some(PromptV2 {
        kind: PromptKind::AutocompactOffer,
        title: const { crate::anchor!("autocompact.title") }.to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: accept_label,
        description,
        notes: Vec::new(),
        auto_deny: None,
        path: None,
        options,
        select: Select::ArrowsEnter,
        cancel: None,
        question: None,
        span: (title, last),
        footer: Some(footer_text.to_string()),
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
}

impl PromptV2 {
    /// THE AUTOCOMPACT OFFER read whole ([`Offer`]).
    #[must_use]
    pub fn autocompact_offer(&self) -> Option<Offer> {
        if self.kind != PromptKind::AutocompactOffer || self.options.len() != 2 {
            return None;
        }
        let accept = self
            .options
            .iter()
            .position(|o| accept_size(&o.label).is_some())?;
        let keep = self
            .options
            .iter()
            .position(|o| keep_current(&o.label).is_some())?;
        let focused = match self.options.iter().position(|o| o.focused)? {
            k if k == accept => OfferFocus::Accept,
            k if k == keep => OfferFocus::Keep,
            _ => return None,
        };
        Some(Offer {
            accept,
            keep,
            tokens: accept_size(&self.options[accept].label)?,
            focused,
        })
    }
}

#[cfg(test)]
#[path = "autocompact_tests.rs"]
mod tests;
