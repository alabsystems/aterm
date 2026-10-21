// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK IS FULL, BY CLAUDE CODE'S OWN WORD — read by PLACE, never by
//! the words alone.
//!
//! On 2026-10-16 a supervised Claude Code drew `Transcript writes are failing
//! (disk full — ENOSPC) · recent messages may not be saved for resume` and
//! every tool call it made failed with `ENOSPC`, while the harness went on
//! typing `keep going` into it. Claude Code 2.1.302 (read from the store
//! binary) draws three such texts:
//!
//! * the pinned notification `transcript-writer-degraded`: `<anchor>` +
//!   `disk full — ENOSPC` (or `disk quota exceeded — EDQUOT`, and others that
//!   are no full disk: `read-only filesystem — EROFS`, `permission denied —
//!   EACCES`) + `)` + ` · recent messages may not be saved for resume`,
//!   where `<anchor>` is [`crate::anchors`]' `wall.disk_full`. It clears
//!   itself at the next successful write to the same transcript;
//! * a tool's output row: `Command output was lost: the temp filesystem at
//!   <dir> is full (<n>MB free). The child process's stdout/stderr writes
//!   failed with ENOSPC. …` (`wall.disk_full.tool`), or `is out of inodes`;
//! * a goal ended by it: `Goal cleared after an unrecoverable error (disk
//!   full): "<condition>"` (`wall.disk_full.goal` is the template's head,
//!   its label `disk full` this cause's).
//!
//! Where the notification is drawn was MEASURED on 2.1.304 (the lab,
//! 2026-10-20, `prompt::fixtures::TRANSCRIPT_DEGRADED_EACCES_2_1_304`): under
//! the composer's bottom rule, over the footer's mode row, two columns in,
//! and opened by the vendor's warning glyph — `  ⚠ Transcript writes are
//! failing (…` — as every pinned notice is (the bundle's notice row draws
//! `⚠`, one space, then the notice). It is read there and in the live zone
//! between the status row and the composer's top rule, where Claude Code
//! draws its other banners: the anchor opening a segment of the row, or
//! following the glyph and ONE space at a segment's head (`after_glyph`),
//! never after any other text — never inside the composer (a draft can
//! quote it) and never in a message a person queued. A tool's row is read
//! only as the vendor's `⎿` row at column two, and the goal's notice only
//! where a vendor row or the footer stands.
//!
//! WHAT A READING IS FOR: a request to MEASURE. aterm's disk watch reads the
//! volume's free space when this says so; nothing is removed on a screen's
//! word (`aterm_agent::harness::disk::poke`), so a misread costs one
//! `statvfs`.

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
    pub fn as_str(self) -> &'static str {
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
    /// The notice as drawn on that row, from its anchor on (the vendor's
    /// glyph before it left off).
    pub message: String,
}

/// The disk-full notice on a Claude Code screen, if one stands where Claude
/// Code draws it (the module's rules); the newest (lowest) first.
#[must_use]
pub fn disk_full(rows: &[String]) -> Option<DiskFull> {
    let frame = crate::phase::composer_frame(rows)?;
    // Under the composer: the footer and its notifications.
    let under = (frame.bottom + 1..rows.len()).rev().find_map(|i| {
        notification(rows, i)
            .or_else(|| goal_cleared(rows, i, false))
            .or_else(|| tool_output(rows, i, false))
    });
    if under.is_some() {
        return under;
    }
    // Above it: the live zone under the status row, or with none the rows
    // the last block ends on — a notice there is a vendor row (`⎿`) or a
    // banner standing alone at the edge.
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
            // (the rows under it were read above): up to the last message a
            // person sent (a `❯` row at the edge), never past it — a row of
            // an earlier turn is an incident since answered.
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
    // The rest of the notice may be wrapped onto the rows under it.
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
        // Above the composer only the vendor's own rows: a `⎿` row, or a
        // `⏺` system row.
        let t = row.trim_start();
        t.strip_prefix('⎿')
            .or_else(|| t.strip_prefix('⏺'))?
            .trim_start()
    } else {
        segments(row).find(|s| s.starts_with(anchor))?
    };
    // The anchor is the template's head; its label (`disk full`) is the
    // vendor's for this cause alone.
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
