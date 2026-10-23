// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whole-box reconstruction from main-screen history
//! ([`Session::box_from_history`], [`joined_box`]).

use super::*;
use crate::supervise::report::decimal;
use aterm_ctl::SEARCH_DEPTH_NOTE;

/// The most rows above the screen a box's head is read from.
pub(super) const HEAD_ROWS: u64 = 400;

/// What the `search` for a box's top rule matches.
pub(super) const TOP_RULE: &str = "^──────────";

impl<C: Ctl> Session<'_, C> {
    /// Reconstruct a box off `screen` ([`aterm_phase::prompt::PromptV2::head_off_screen`]).
    pub(super) fn box_from_history(
        &mut self,
        screen: &Screen,
        reading: &aterm_phase::Reading,
    ) -> Result<Option<(Vec<String>, aterm_phase::Reading)>, Fail> {
        let Some(cut) = reading
            .prompt
            .as_ref()
            .filter(|p| p.head_off_screen && p.kind != PromptKind::Question)
        else {
            return Ok(None);
        };
        let claude = match reading.program {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        if !claude || screen.first != 0 || screen.generation.is_none() || self.alt_screen_now()? {
            return Ok(None);
        }
        let r = self.call(&SEARCH)?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("search failed: {}", r.stderr.trim())));
        }
        let Some(rows) = head_rows(&r) else {
            return Ok(None);
        };
        let mut head = Vec::new();
        for row in rows {
            let r = self.call(&["line", &row.to_string()])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("line failed: {}", r.stderr.trim())));
            }
            let Some(text) = head_row(&r) else {
                return Ok(None);
            };
            head.push(text);
        }
        let (now, _) = self.read_once()?;
        if !same_box(&now.rows, &screen.rows) {
            return Ok(None);
        }
        Ok(joined_box(&head, &screen.rows, cut, screen.cursor_col))
    }

    /// Whether the session draws on the ALTERNATE screen (`modes`' `alt_screen=true` row).
    pub(super) fn alt_screen_now(&mut self) -> Result<bool, Fail> {
        let r = self.call(&["modes"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("modes failed: {}", r.stderr.trim())));
        }
        Ok(!(r.ok() && r.stdout.lines().any(|l| l.trim() == "alt_screen=false")))
    }
}

/// Whether `now` matches `screen` ([`super::approval_loop::box_identity`], [`head_rows`]).
pub(crate) fn same_box(now: &[String], screen: &[String]) -> bool {
    super::approval_loop::box_identity(now) == super::approval_loop::box_identity(screen)
}

/// The request [`Session::box_from_history`] finds a box's top rule with.
pub(crate) const SEARCH: [&str; 3] = ["search", TOP_RULE, "regex"];

/// History rows for a box's head from the [`SEARCH`] reply ([`HEAD_ROWS`]).
pub(crate) fn head_rows(search: &CtlReply) -> Option<std::ops::Range<u64>> {
    if !search.ok() || unsearched(search) == Unsearched::Anywhere {
        return None;
    }
    let first = search_screen_first(search)?;
    let top = hit_rows(&search.stdout).filter(|&h| h < first).max()?;
    (first - top <= HEAD_ROWS).then_some(top..first)
}

/// Text of a `line` reply, or `None` when unreadable ([`head_rows`]).
pub(crate) fn head_row(line: &CtlReply) -> Option<String> {
    if !line.ok() {
        return None;
    }
    let first = line.stdout.lines().next().unwrap_or("");
    let text = first.strip_prefix("OK").unwrap_or(first);
    Some(text.strip_prefix(' ').unwrap_or(text).to_string())
}

/// The absolute rows of a `search` reply's hits (`<row> <col> <len>` each).
fn hit_rows(stdout: &str) -> impl Iterator<Item = u64> + '_ {
    stdout.lines().filter_map(|l| {
        let mut words = l.split(' ');
        let row = words.next()?.parse().ok()?;
        words.next()?.parse::<u64>().ok()?;
        words.next()?.parse::<u64>().ok()?;
        words.next().is_none().then_some(row)
    })
}

/// The box `cut`, read off `screen` with its head off it, read WHOLE from `head`.
#[must_use]
pub(crate) fn joined_box(
    head: &[String],
    screen: &[String],
    cut: &aterm_phase::PromptV2,
    cursor_col: usize,
) -> Option<(Vec<String>, aterm_phase::Reading)> {
    let mut rows = head.to_vec();
    rows.extend(screen.iter().cloned());
    let reading = aterm_phase::read(Some("claude"), &rows, Some(cursor_col));
    let whole = reading.prompt.as_ref()?;
    let h = head.len();
    let options = |p: &aterm_phase::PromptV2, shift: usize| -> Vec<_> {
        p.options
            .iter()
            .filter_map(|o| Some((o.n, o.label.clone(), o.focused, o.row.checked_sub(shift)?)))
            .collect()
    };
    let one = !whole.head_off_screen
        && whole.kind != PromptKind::Question
        && whole.span == (1, cut.span.1 + h)
        && options(whole, h) == options(cut, 0);
    one.then_some((rows, reading))
}

/// What a `search` reply did not search ([`aterm_ctl::search_incomplete_note`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unsearched {
    /// Nothing: the hits are every row that matches.
    Nothing,
    /// History older than the rows searched (the depth cap).
    Older,
    /// Rows anywhere: the match cap.
    Anywhere,
}

/// [`Unsearched`] of one `search` reply.
fn unsearched(r: &CtlReply) -> Unsearched {
    let mut worst = Unsearched::Nothing;
    let header = |tail: &str| {
        let mut words = tail.split_whitespace();
        let n = words.next().and_then(decimal);
        let words: Vec<&str> = words
            .take_while(|w| !w.starts_with('('))
            .filter(|w| !w.starts_with("screen_first=") && !w.starts_with("frame="))
            .collect();
        match words.as_slice() {
            [] => Unsearched::Nothing,
            ["incomplete"] if n.is_some_and(|n| n >= 100_000) => Unsearched::Anywhere,
            ["incomplete"] => Unsearched::Older,
            ["incomplete", causes @ ..] if causes.iter().all(|c| *c == "older") => {
                Unsearched::Older
            }
            _ => Unsearched::Anywhere,
        }
    };
    if let Some(tail) = r
        .stdout
        .lines()
        .next()
        .and_then(|l| l.trim().strip_prefix("OK "))
    {
        worst = worst.max(header(tail));
    }
    for line in r.stderr.lines() {
        let line = line.trim();
        let line = line.strip_prefix("aterm-ctl:").map_or(line, str::trim);
        if line.is_empty() {
            continue;
        }
        let said = match line.strip_prefix("OK ") {
            Some(tail) => header(tail),
            None if line.split("; ").all(|part| part == SEARCH_DEPTH_NOTE) => Unsearched::Older,
            None => Unsearched::Anywhere,
        };
        worst = worst.max(said);
    }
    worst
}

/// First absolute row of the live screen in a `search` reply ([`unsearched`]).
pub(super) fn search_screen_first(reply: &CtlReply) -> Option<u64> {
    let raw = reply
        .stderr
        .lines()
        .map(|line| {
            line.trim()
                .strip_prefix("aterm-ctl:")
                .unwrap_or(line)
                .trim()
        })
        .chain(reply.stdout.lines().take(1))
        .find_map(|line| line.strip_prefix("OK "));
    let raw = raw?;
    let mut words = raw.split_whitespace();
    words.next().and_then(decimal)?;
    let mut screen_first = None;
    for word in words {
        if let Some(value) = word.strip_prefix("screen_first=") {
            let first = decimal(value)?;
            if screen_first.replace(first).is_some() {
                return None;
            }
        }
    }
    screen_first
}
