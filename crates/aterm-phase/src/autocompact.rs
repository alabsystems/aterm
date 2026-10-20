// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE AUTOCOMPACT OFFER** — Claude Code's own dialog ` We recommend
//! compacting earlier` (2.1.302: component `dbe`, title `G8e`, footer `Q8e`,
//! scheduled as `autocompact-offer`), read whole.
//!
//! Claude Code raises it by itself at an idle turn end (nothing loading,
//! nothing queued, the last turn `completed`) once the context passes the
//! server's trigger (200k by default), for a claude.ai subscriber on a 1M
//! model. It is drawn in the permission frame (`ka`: a full-width `─` rule,
//! the title one column in) in place of the composer, its body two columns
//! further in, then an unnumbered Select (`hideIndexes`, the vendor `❯` on
//! the focused row) of two options in a server-chosen order —
//!
//! * the ACCEPT row, `Autocompact at ‹ <size> › tokens` (`‹`/`›` are U+2039 and
//!   U+203A; ←/→ step `<size>` along 300k…900k), optionally over a
//!   description `This conversation is at <n>, so it will be summarized on
//!   your next message.`;
//! * the KEEP row, `Keep auto (<w> tokens)` or `Keep <w> tokens` —
//!
//! and the footer `↑/↓ choose · ←/→ change size · enter confirm · esc not now`
//! (a `More text above: …` warning may sit under it). Its key-hint items
//! carry no ` to `, so the generic finder ([`crate::prompt`]'s `find_box`)
//! never saw a box there: the screen read as non-authoritative idle with no
//! composer, and the session waited for a person.
//!
//! [`offer_box`] reads it only when ALL of these hold, and otherwise leaves
//! the screen to the other readers exactly as before:
//!
//! 1. the last non-blank row (or the one over the `More text above` warning)
//!    is exactly the footer, and no composer frame is on the screen (the
//!    dialog replaces it, so a transcript quoting one is never one);
//! 2. the nearest full-width rule over the footer (`─`, or the fullscreen
//!    panel's `▔`) is followed by exactly the title, at column one to three
//!    (a position label `n of m` beside it allowed);
//! 3. every non-blank row from the title to the footer is drawn at the
//!    title's column or further in;
//! 4. the last block over the footer is exactly two option rows — one accept,
//!    one keep, at one pointer column, exactly one of them under `❯` — with
//!    only the accept row's description rows between or under them;
//! 5. the body over them asks `Want to autocompact at <size> tokens instead?`
//!    with the accept row's own size.
//!
//! The `Press <key> again to exit` footer (an exit pending), a screen-reader
//! render (no footer at all), a size off the ladder or a label in any other
//! words is no such dialog: a person's.

use crate::prompt::{Opt, PromptKind, PromptV2, Role, Select};

/// The sizes Claude Code's stepper offers, in kilo-tokens (2.1.302: the
/// payload's `sizes`, 300k to 900k, only those under the model's window).
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
    /// The size the accept row offers now, in kilo-tokens (`400` of `‹ 400k ›`).
    pub size: u32,
    pub focused: OfferFocus,
    /// The accept row says the conversation is past that size already: the
    /// next message compacts it.
    pub compacts_next: bool,
    /// What the keep row keeps, as drawn (`auto (1m tokens)`, `500k tokens`).
    pub current: String,
}

/// `Autocompact at ‹ 400k › tokens` → `Some(400)`: the accept row's label
/// exactly, its size one of [`LADDER`]'s.
#[must_use]
pub fn accept_size(label: &str) -> Option<u32> {
    let accept = const { crate::anchors::anchor_text("autocompact.accept") };
    let size = label
        .strip_prefix(accept)?
        .strip_prefix(" \u{2039} ")?
        .strip_suffix(" \u{203A} tokens")?;
    kilo(size)
}

/// `400k` → `Some(400)`, a size on [`LADDER`] only.
fn kilo(text: &str) -> Option<u32> {
    let n: u32 = text.strip_suffix('k')?.parse().ok()?;
    (LADDER.contains(&n) && text.len() == 4).then_some(n)
}

/// `Keep auto (1m tokens)` → `auto (1m tokens)`; `Keep 500k tokens` →
/// `500k tokens`: the keep row's label exactly.
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

/// How the description the vendor draws under the accept row opens, and
/// how it ends.
const DESCRIPTION: &str = "This conversation is at ";
const DESCRIPTION_END: &str = "so it will be summarized on your next message.";

/// An option's label less a description drawn on its own row after two
/// spaces or more: `(label, description)`.
fn strip_inline(drawn: &str) -> (&str, Option<&str>) {
    match drawn.find(&format!("  {DESCRIPTION}")) {
        Some(at) => (
            crate::text::trim(&drawn[..at]),
            Some(crate::text::trim(&drawn[at..])),
        ),
        None => (drawn, None),
    }
}

/// One option row: `(pointer column, focused, label)`. The focused row is
/// `❯ <label>` with the `❯` at the pointer column; an unfocused one is the
/// label two columns further in.
fn option_row(row: &str) -> Option<(usize, bool, &str)> {
    let col = crate::phase::leading_spaces(row);
    let t = crate::text::trim(row);
    if let Some(rest) = t.strip_prefix('❯') {
        let label = rest.strip_prefix(' ')?;
        return Some((col, true, crate::text::trim(label)));
    }
    Some((col.checked_sub(2)?, false, t))
}

/// The title row less a position label `n of m` the frame may draw beside it.
fn is_title_row(row: &str) -> bool {
    let title = const { crate::anchors::anchor_text("autocompact.title") };
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

/// THE AUTOCOMPACT OFFER on `rows` (module header), as the box
/// [`crate::prompt::parse_prompt_v2`] answers; `None` unless every condition
/// holds.
pub(crate) fn offer_box(rows: &[String]) -> Option<PromptV2> {
    use crate::phase::{has_composer_frame, is_rule, leading_spaces};
    let footer_text = const { crate::anchors::anchor_text("autocompact.footer") };
    let more = const { crate::anchors::anchor_text("autocompact.more_above") };
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
    // The option block: the non-blank rows over the footer, walked up to the
    // blank row that ends the body.
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
    // The accept row's description, once begun: its rows up to the next
    // option.
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
            // Not an option: only the accept row's description, further in
            // than the pointer.
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
        // One accept row and one keep row: a second of either is no offer.
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
    // The body asks for the accept row's own size.
    let body: Vec<&str> = rows[title + 1..body_end]
        .iter()
        .map(|r| crate::text::trim(r))
        .filter(|t| !t.is_empty())
        .collect();
    let prose = body.join(" ");
    let question = const { crate::anchors::anchor_text("autocompact.question") };
    if !prose.contains(&format!("{question} {size}k tokens instead?")) {
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
        // The title less a position label, as every box's title is read.
        title: const { crate::anchors::anchor_text("autocompact.title") }.to_string(),
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
        // Esc is `not now`, which counts toward the vendor's ask limit: no
        // cancel is offered to any caller.
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
    /// THE AUTOCOMPACT OFFER read whole ([`Offer`]): `None` for any other box,
    /// or an offer whose two options are not one accept and one keep row.
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
            size: accept_size(&self.options[accept].label)?,
            focused,
            compacts_next: self
                .description
                .split_once('\n')
                .is_some_and(|(_, d)| d.starts_with(DESCRIPTION) && d.ends_with(DESCRIPTION_END)),
            current: keep_current(&self.options[keep].label)?.to_string(),
        })
    }
}

#[cfg(test)]
#[path = "autocompact_tests.rs"]
mod tests;
