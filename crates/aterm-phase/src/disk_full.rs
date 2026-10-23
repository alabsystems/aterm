// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK IS FULL, BY CLAUDE CODE'S OWN WORD — read by PLACE, never by the words alone.

/// The error code a disk-full notice names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// `ENOSPC`: no space (or no inodes) left on the device.
    Enospc,
    /// `EDQUOT`: the user's disk quota is exceeded.
    Edquot,
}

impl Code {
    /// The code as the notice spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::Enospc => "ENOSPC",
            Code::Edquot => "EDQUOT",
        }
    }
}

/// Which of Claude Code's texts said it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The pinned notification (`wall.disk_full`).
    Notification,
    /// A tool's output row (`wall.disk_full.tool`).
    ToolOutput,
    /// A goal cleared by it (`wall.disk_full.goal`).
    GoalCleared,
}

/// One disk-full notice read from a screen ([`disk_full`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFull {
    pub code: Code,
    pub source: Source,
    /// The row it opens on.
    pub row: usize,
    /// The notice as drawn on that row, from its anchor on (the vendor's glyph before it left off).
    pub message: String,
}

/// The disk-full notice on a Claude Code screen, if one stands where Claude Code draws it (the
/// module's rules); the newest (lowest) first.
#[must_use]
pub fn disk_full(rows: &[String]) -> Option<DiskFull> {
    let frame = crate::phase::composer_frame(rows)?;
    let under = (frame.bottom + 1..rows.len()).rev().find_map(|i| {
        notification(rows, i)
            .or_else(|| goal_cleared(rows, i, false))
            .or_else(|| tool_output(rows, i, false))
    });
    if under.is_some() {
        return under;
    }
    let block = crate::phase::status_block(rows, frame.top);
    let from = block.status.map_or(block.from, |s| s + 1);
    (from..frame.top)
        .rev()
        .find_map(|i| {
            if block
                .status
                .is_some_and(|s| crate::wall::in_queued_message(rows, s, i))
            {
                return None;
            }
            notification(rows, i)
                .or_else(|| tool_output(rows, i, true))
                .or_else(|| goal_cleared(rows, i, true))
        })
        .or_else(|| {
            // The vendor's `⎿` rows of the LAST TURN, above the status row
            let end = block.status.unwrap_or(frame.top).min(frame.top);
            (0..end)
                .rev()
                .take(TOOL_ROWS)
                .take_while(|&i| !rows[i].starts_with('❯'))
                .find_map(|i| tool_output(rows, i, true).or_else(|| goal_cleared(rows, i, true)))
        })
}

/// How far above the composer a tool's output row is looked for: the last
/// screenful's tail, where the turn that failed drew it.
const TOOL_ROWS: usize = 40;

/// Segments of a row: its text after its indent, and the text after each run
/// of two or more spaces — where a footer item, a right-aligned notice or a
/// banner beside the spinner starts.
fn segments(row: &str) -> impl Iterator<Item = &str> {
    let trimmed = row.trim_start();
    let mut starts = vec![trimmed];
    let mut rest = trimmed;
    while let Some(at) = rest.find("  ") {
        let after = rest[at..].trim_start_matches(' ');
        if after.is_empty() {
            break;
        }
        starts.push(after);
        rest = after;
    }
    starts.into_iter()
}

/// The pinned notification on row `i`: a segment that opens with the anchor,
/// or with the vendor's glyph and the anchor ([`after_glyph`]), and names
/// `— ENOSPC)` or `— EDQUOT)` (a code alone when the vendor maps none:
/// `(ENOSPC)`).
fn notification(rows: &[String], i: usize) -> Option<DiskFull> {
    let anchor = const { crate::anchors::anchor_text("wall.disk_full") };
    let row = crate::text::trim_end(&rows[i]);
    segments(row).find_map(|seg| {
        let words = after_glyph(seg);
        let rest = words.strip_prefix(anchor)?;
        let inside = rest.split(')').next()?;
        let code = code_in(inside)?;
        Some(DiskFull {
            code,
            source: Source::Notification,
            row: i,
            message: words.to_string(),
        })
    })
}

/// The warning glyph Claude Code draws before a pinned notice's words
/// (2.1.304's notice row: `⚠`, then one space; MEASURED, the module's
/// fixture), U+26A0 with or without the emoji selector U+FE0F.
const GLYPH: char = '\u{26A0}';

/// `seg` less the vendor's glyph and the ONE space after it, where it opens
/// with them; `seg` itself otherwise. Nothing else before the words is
/// taken off: a draft, a quote or a label in front of them is no notice.
fn after_glyph(seg: &str) -> &str {
    seg.strip_prefix(GLYPH)
        .map(|r| r.strip_prefix('\u{FE0F}').unwrap_or(r))
        .and_then(|r| r.strip_prefix(' '))
        .unwrap_or(seg)
}

/// The code a notice's words name: the last word of `text`, or `None` when
/// it is neither code.
fn code_in(text: &str) -> Option<Code> {
    match text.rsplit([' ', '(']).next()? {
        "ENOSPC" => Some(Code::Enospc),
        "EDQUOT" => Some(Code::Edquot),
        _ => None,
    }
}

/// A tool's output row: `  ⎿  <anchor> …` at column two (`vendor`: above
/// the composer, where only the vendor's `⎿` row counts), or the anchor
/// opening a segment under it.
fn tool_output(rows: &[String], i: usize, vendor: bool) -> Option<DiskFull> {
    let anchor = const { crate::anchors::anchor_text("wall.disk_full.tool") };
    let row = crate::text::trim_end(&rows[i]);
    let seg = if vendor {
        row.strip_prefix("  ⎿  ")?.trim_start()
    } else {
        segments(row).find(|s| s.starts_with(anchor))?
    };
    if !seg.starts_with(anchor) {
        return None;
    }
    let joined: String = std::iter::once(seg)
        .chain(
            rows.iter()
                .skip(i + 1)
                .take(3)
                .map(|r| r.trim())
                .take_while(|r| !r.is_empty()),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let code = if joined.contains("EDQUOT") || joined.contains("disk quota") {
        Code::Edquot
    } else {
        Code::Enospc
    };
    Some(DiskFull {
        code,
        source: Source::ToolOutput,
        row: i,
        message: seg.to_string(),
    })
}

/// A goal the vendor ended on a full disk: its notice opening a segment of
/// a footer row, or (`vendor`: above the composer) a vendor row, `⎿` or
/// `⏺` at its head.
fn goal_cleared(rows: &[String], i: usize, vendor: bool) -> Option<DiskFull> {
    let anchor = const { crate::anchors::anchor_text("wall.disk_full.goal") };
    let row = crate::text::trim_end(&rows[i]);
    let seg = if vendor {
        let t = row.trim_start();
        t.strip_prefix('⎿')
            .or_else(|| t.strip_prefix('⏺'))?
            .trim_start()
    } else {
        segments(row).find(|s| s.starts_with(anchor))?
    };
    seg.strip_prefix(anchor)
        .is_some_and(|rest| rest.starts_with("disk full)"))
        .then(|| DiskFull {
            code: Code::Enospc,
            source: Source::GoalCleared,
            row: i,
            message: seg.to_string(),
        })
}

#[cfg(test)]
#[path = "disk_full_tests.rs"]
mod tests;
