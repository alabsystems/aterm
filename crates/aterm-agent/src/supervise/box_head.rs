// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A BOX TALLER THAN THE PANE, READ WHOLE FROM THE HISTORY. Claude Code's
//! inline renderer draws on the MAIN screen, and writes a box taller than the
//! pane whole: its head scrolls into the main screen's history, right above
//! the screen, and runs straight into it. MEASURED 2026-10-17 on 2.1.302 in an
//! 80x18 lab pane (a 30-row `touch` command): `search '^──────────' regex`
//! found the box's top rule in the history, and `line` read its title, its
//! description and its command rows down to the row above the screen's first,
//! with no gap; the screen's generation did not move while the box sat; a
//! focus move redrew the screen in place, the head left as it was; and each
//! later box (the next call, or the second of two parallel calls) was written
//! whole under the transcript rows its predecessor's answer added, so an
//! older box's head lies above those rows, never against the screen.
//!
//! The screen's tail alone cannot say what a cut box asks (two measured boxes
//! end in the same five rows and need opposite answers:
//! `policy::approval`'s module doc, "A box taller than the pane"), so the
//! head is read from the history and the box decided whole
//! ([`Session::box_from_history`]) — only where the join proves itself: the
//! session's program Claude Code and the box no question, on the main screen
//! (the fullscreen renderer draws on the alternate screen, which has no
//! history, and its archive never held such a box's head: the alternate
//! archive's read was dropped for gluing an older box's head on), the read
//! the whole grid with its generation; the top rule nearest above the screen
//! found by `search` (a reply that may have left out newer rows is no
//! answer), every row from it to the screen read by `line`, the screen's
//! generation and rows the same after the reads, and the rows joined read as
//! ONE box from that rule to the screen's foot, its options on the screen the
//! cut's own ([`joined_box`]). Anything else is the cut, held as before. A
//! press made on the whole box is fenced on the screen's generation and
//! guarded on a row of it the screen shows (`approval_loop`,
//! `whole_box_decision`).

use super::*;
use crate::supervise::report::decimal;
use aterm_ctl::SEARCH_DEPTH_NOTE;

/// The most rows above the screen a box's head is read from: a box further up
/// than this is held as a cut one.
pub(super) const HEAD_ROWS: u64 = 400;

/// What the `search` for a box's top rule matches: a row that opens with the
/// rule Claude Code draws over its boxes (and its composer), a run of `─`
/// from column 0.
pub(super) const TOP_RULE: &str = "^──────────";

impl<C: Ctl> Session<'_, C> {
    /// The box `reading` read off `screen`, its head off the screen
    /// ([`aterm_phase::prompt::PromptV2::head_off_screen`]), read whole with
    /// the history above the screen (module docs): the rows joined, from the
    /// box's top rule to the screen's last row, and their reading. `None`
    /// wherever the join does not prove itself; the cut is then decided as it
    /// is (held).
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
        let r = self.call(&["search", TOP_RULE, "regex"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("search failed: {}", r.stderr.trim())));
        }
        if !r.ok() || unsearched(&r) == Unsearched::Anywhere {
            return Ok(None);
        }
        let Some(first) = search_screen_first(&r) else {
            return Ok(None);
        };
        let Some(top) = hit_rows(&r.stdout).filter(|&h| h < first).max() else {
            return Ok(None);
        };
        if first - top > HEAD_ROWS {
            return Ok(None);
        }
        let mut head = Vec::new();
        for row in top..first {
            let r = self.call(&["line", &row.to_string()])?;
            if self.unserved(&r) {
                return Err(Fail::Lost(format!("line failed: {}", r.stderr.trim())));
            }
            // `ERR evicted`, `ERR unreadable`, `ERR out of range`, a host
            // without the verb: no head.
            if !r.ok() {
                return Ok(None);
            }
            let line = r.stdout.lines().next().unwrap_or("");
            let text = line.strip_prefix("OK").unwrap_or(line);
            head.push(text.strip_prefix(' ').unwrap_or(text).to_string());
        }
        // The screen unchanged under the reads: the history read is the one
        // drawn with it, and the press is fenced on that generation.
        let (now, _) = self.read_once()?;
        if now.generation != screen.generation || now.rows != screen.rows {
            return Ok(None);
        }
        Ok(joined_box(&head, &screen.rows, cut, screen.cursor_col))
    }

    /// Whether the session draws on the ALTERNATE screen (`modes`'
    /// `alt_screen=true` row); a reply that names none is not known to be the
    /// main screen, and is `true`.
    pub(super) fn alt_screen_now(&mut self) -> Result<bool, Fail> {
        let r = self.call(&["modes"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("modes failed: {}", r.stderr.trim())));
        }
        Ok(!(r.ok() && r.stdout.lines().any(|l| l.trim() == "alt_screen=false")))
    }
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

/// The box `cut`, read off `screen` with its head off it, read WHOLE from
/// `head` — the history's rows from the top rule nearest above the screen
/// down to the screen's first row — joined over `screen`: the joined rows and
/// their reading, where they are one box. The join must read as one box, not
/// cut, opening on the row under that rule — so every row joined is the
/// box's own: a row a repaint left between them (a transcript row, a banner,
/// an older frame's rule or footer) moves the box's top off it — no question,
/// ending where the cut ends, the options it draws on the screen the cut's
/// own, on the cut's rows (an option above the screen is the head's).
#[must_use]
pub(super) fn joined_box(
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
    // The options drawn on the screen, by their rows on it: the whole box's
    // under the head, the cut's own.
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

/// What a `search` reply says it did not search, by its ` incomplete` header
/// marker (a raw client's header, the first line of stdout; `aterm ctl`'s, on
/// stderr when nothing matched) or the note `aterm ctl` prints for it
/// ([`aterm_ctl::search_incomplete_note`], on stderr); the worst one wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unsearched {
    /// Nothing: the hits are every row that matches.
    Nothing,
    /// History OLDER than the rows searched (the depth cap: a bare `
    /// incomplete`, or ` older` alone): every newer row was searched.
    Older,
    /// Rows anywhere: the match cap, a rewrap (only the screen searched), a
    /// long line not searched to its end, the terminal changed mid-search, or
    /// a qualifier or note this reader does not know.
    Anywhere,
}

/// [`Unsearched`] of one `search` reply.
fn unsearched(r: &CtlReply) -> Unsearched {
    let mut worst = Unsearched::Nothing;
    let header = |tail: &str| {
        let mut words = tail.split_whitespace();
        let n = words.next().and_then(decimal);
        // The qualifiers end where a client's own note on the header begins
        // (`aterm ctl`'s ` (search: no results)` on a reply with no hits);
        // `screen_first=` places the screen and says nothing of what was left
        // out ([`search_screen_first`]).
        let words: Vec<&str> = words
            .take_while(|w| !w.starts_with('('))
            .filter(|w| !w.starts_with("screen_first="))
            .collect();
        match words.as_slice() {
            [] => Unsearched::Nothing,
            // An older server marks the match limit bare too.
            ["incomplete"] if n.is_some_and(|n| n >= 100_000) => Unsearched::Anywhere,
            ["incomplete"] => Unsearched::Older,
            ["incomplete", causes @ ..] if causes.iter().all(|c| *c == "older") => {
                Unsearched::Older
            }
            // Any other cause, or a qualifier this reader does not know (a
            // future server's), may have left out newer rows.
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

/// The live screen's first absolute row in the frame a `search` reply
/// searched: its header's one `screen_first=<row>` (a raw client's header, the
/// first line of stdout; `aterm ctl`'s, on stderr). `None` for a reply with no
/// header, none or a malformed one, or two: an older host or client cannot
/// tell a logical-line hit from a physical continuation row that starts with
/// the caret. What the reply left out is [`unsearched`]'s.
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
