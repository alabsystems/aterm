// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ONE idea of what a Claude Code screen shows, for every lab verb that judges one.

const NBSP: char = '\u{a0}';

/// The survey card's question (`survey.question`).
const CARD: &str = aterm_phase::anchor!("survey.question");

/// The survey card's head row: `●`, a space, the question.
#[must_use]
pub fn is_card_row(r: &str) -> bool {
    r.strip_prefix('●')
        .and_then(|rest| {
            let mut it = rest.chars();
            it.next().filter(|c| c.is_whitespace()).map(|_| it.as_str())
        })
        .is_some_and(|rest| rest.starts_with(CARD))
}

/// `  1: Bad    2: Fine   3: Good   0: Dismiss`: the card's options row.
#[must_use]
pub fn is_options_row(r: &str) -> bool {
    let mut rest = r;
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    for word in ["1: Bad", "2: Fine", "3: Good", "0: Dismiss"] {
        let t = rest.trim_start();
        if t.len() == rest.len() {
            return false;
        }
        let Some(after) = t.strip_prefix(word) else {
            return false;
        };
        rest = after;
    }
    true
}

/// `✻ Baked for 0s · done 5:39 AM`: the done row of a turn.
#[must_use]
pub fn is_done_row(r: &str) -> bool {
    r.strip_prefix("✻ ").is_some_and(|rest| {
        rest.match_indices(" for ").any(|(i, _)| {
            rest[i + 5..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
    })
}

/// A full-width rule of the composer's frame.
#[must_use]
pub fn is_rule(r: &str) -> bool {
    let s = r.trim();
    s.chars().count() >= 40
        && (s.chars().all(|c| c == '─' || c == '━') || aterm_phase::phase::is_rule(s))
}

/// Whether the survey card is on the screen.
#[must_use]
pub fn has_card(rows: &[String]) -> bool {
    rows.iter().any(|r| is_card_row(r))
}

/// The composer's first row, or `None` when the frame has no composer.
#[must_use]
pub fn composer_of(rows: &[String]) -> Option<String> {
    let idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| is_rule(r))
        .map(|(i, _)| i)
        .collect();
    let [.., top, bot] = idx.as_slice() else {
        return None;
    };
    let Some(first) = rows.get(top + 1..*bot).and_then(<[String]>::first) else {
        return Some(String::new());
    };
    let t = first.replace(NBSP, " ");
    let t = t.strip_prefix('❯').unwrap_or(&t);
    let t = t.strip_prefix(' ').unwrap_or(t);
    Some(t.trim_end().to_string())
}

/// The status row of a vi-family editor holding the screen `ctl text` gave: the last row that
/// is neither blank nor vi's `~` (the filler under a short buffer), on a screen with at least
/// three `~` rows. `None`: no such editor's screen.
#[must_use]
pub fn editor_row(screen: &str) -> Option<String> {
    let rows: Vec<&str> = screen.split('\n').map(str::trim_end).collect();
    if rows.iter().filter(|r| **r == "~").count() < 3 {
        return None;
    }
    rows.iter()
        .rev()
        .find(|r| !r.trim().is_empty() && **r != "~")
        .map(|r| (*r).to_string())
}

/// The first row of the composer on a screen `ctl text` gave.
#[must_use]
pub fn composer_row(agent: &str, screen: &str) -> Option<String> {
    composer_at(agent, screen).map(|(_, row)| row)
}

/// [`composer_row`] with the screen row it stands on (the line of the screen `ctl text` gave, so
/// the row a `ctl cell <row> <col>` read names).
#[must_use]
pub fn composer_at(agent: &str, screen: &str) -> Option<(usize, String)> {
    let rows: Vec<String> = screen
        .replace(NBSP, " ")
        .split('\n')
        .map(str::to_string)
        .collect();
    let first = |(at, lines): (usize, Vec<String>)| Some((at, lines.into_iter().next()?));
    if agent == "muse" {
        return first(aterm_phase::muse::composer_draft(&rows)?);
    }
    if agent == "codex" {
        let at = rows.iter().rposition(|r| r.starts_with('›'))?;
        if rows[at + 1..].iter().any(|r| r.starts_with("  › ")) {
            return None;
        }
        return Some((at, rows[at].trim_start_matches('›').trim().to_string()));
    }
    if agent == "agy" {
        return first(aterm_phase::agy::composer_draft(&rows)?);
    }
    if agent == "kimi" {
        return first(aterm_phase::kimi::composer(&rows)?);
    }
    let idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| is_rule(r))
        .map(|(i, _)| i)
        .collect();
    let [.., top, bot] = idx.as_slice() else {
        return None;
    };
    let first = rows
        .get(top + 1..*bot)
        .and_then(<[String]>::first)
        .cloned()
        .unwrap_or_default();
    let t = first.strip_prefix('❯').unwrap_or(&first);
    Some((top + 1, t.trim().to_string()))
}

/// The spinner's busy row (`✻ Verb… (Ns · …)`), if any.
#[must_use]
pub fn status_row(rows: &[String]) -> Option<String> {
    rows.iter()
        .map(|r| r.trim_start())
        .find(|s| s.starts_with(['✻', '✶', '✳', '✢', '·', '*', '✽']) && s.contains('…'))
        .map(str::to_string)
}

/// An idle turn: its done row, and no spinner.
#[must_use]
pub fn is_idle(rows: &[String]) -> bool {
    rows.iter().any(|r| is_done_row(r)) && status_row(rows).is_none()
}

/// The `key=value` words of a line.
#[must_use]
pub fn kv(text: &str) -> Vec<(String, String)> {
    text.split_whitespace()
        .filter_map(|t| {
            t.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// The value of `key` in a line of `key=value` words.
#[must_use]
pub fn field(text: &str, key: &str) -> Option<String> {
    kv(text).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
