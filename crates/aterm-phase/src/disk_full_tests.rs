// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk-full notice reader.

use super::*;
use crate::anchors::anchor_text;
use crate::prompt::fixtures::{TRANSCRIPT_DEGRADED_EACCES_2_1_304, composer, rows, screen};

/// The measured words the vendor maps `EACCES` to, in the measured row.
const MEASURED_CODE: &str = "permission denied — EACCES";

/// The measured screen with the notice's mapped words `mapped` in place of
/// [`MEASURED_CODE`]: the row as Claude Code draws it for that code (2.1.304
/// maps each code to its words in the one template), and the row it is on.
fn measured_with(mapped: &str) -> (Vec<String>, usize) {
    let r: Vec<String> = screen(TRANSCRIPT_DEGRADED_EACCES_2_1_304)
        .into_iter()
        .map(|r| r.replace(MEASURED_CODE, mapped))
        .collect();
    let at = r
        .iter()
        .position(|row| row.contains(anchor_text("wall.disk_full")))
        .expect("the measured notice row");
    (r, at)
}

/// THE NOTICE AS CLAUDE CODE DRAWS IT (2.1.304, MEASURED): under the
/// composer's bottom rule, over the mode row, two columns in, after the
/// vendor's `⚠` and one space — read with its code, its message from the
/// anchor on; the glyph's emoji form (U+FE0F after it) the same. The
/// measured screen itself names `EACCES`, no full disk: not read.
#[test]
fn the_measured_notice_reads_after_the_vendors_glyph() {
    let measured = screen(TRANSCRIPT_DEGRADED_EACCES_2_1_304);
    let (full, at) = measured_with("disk full — ENOSPC");
    assert!(full[at].starts_with("  ⚠ "), "{:?}", full[at]);
    assert!(
        crate::phase::composer_frame(&full).is_some_and(|f| f.bottom < at),
        "under the composer"
    );
    assert_eq!(disk_full(&measured), None, "EACCES is no full disk");
    let d = disk_full(&full).expect("the vendor's row");
    assert_eq!(
        (d.code, d.source, d.row),
        (Code::Enospc, Source::Notification, at)
    );
    assert!(
        d.message.starts_with(anchor_text("wall.disk_full")),
        "{:?}",
        d.message
    );
    let (quota, _) = measured_with("disk quota exceeded — EDQUOT");
    assert_eq!(disk_full(&quota).map(|d| d.code), Some(Code::Edquot));
    let emoji: Vec<String> = full
        .iter()
        .map(|r| r.replacen("⚠ ", "⚠\u{FE0F} ", 1))
        .collect();
    assert_ne!(emoji, full);
    assert_eq!(disk_full(&emoji).map(|d| d.row), Some(at));
}

/// NEGATIVE CONTROLS, each the vendor's row, glyph and all, from the
/// measured screen: typed as a draft into the composer, queued by a person
/// under a running turn, behind any other text than the glyph alone (a
/// label, a quote, a second glyph, a glyph with no space) — none is the
/// notice. CONTROL: the row as drawn is.
#[test]
fn the_glyphs_row_anywhere_else_is_not_the_notice() {
    let (full, at) = measured_with("disk full — ENOSPC");
    assert!(disk_full(&full).is_some(), "the screen the controls edit");
    let drawn = full[at].trim_start().to_string();
    let mut draft = full.clone();
    draft.remove(at);
    let caret = draft
        .iter()
        .rposition(|r| r == "❯")
        .expect("the measured composer's caret row");
    draft[caret] = format!("❯ {drawn}");
    assert_eq!(disk_full(&draft), None, "a draft");
    let queued = format!("❯ {drawn}");
    assert_eq!(
        disk_full(&busy(&["", &queued], "", "  ? for shortcuts")),
        None,
        "a queued message"
    );
    let words = drawn.strip_prefix("⚠ ").expect("the glyph and one space");
    for row in [
        format!("  note: {drawn}"),
        format!("  \"{drawn}\""),
        format!("  ⚠ ⚠ {words}"),
        format!("  ⚠{words}"),
        format!("  ! {words}"),
    ] {
        let mut edited = full.clone();
        edited[at] = row.clone();
        assert_eq!(disk_full(&edited), None, "{row:?}");
    }
}

/// The notification as 2.1.302 composes it for `code`'s mapping.
fn notice(mapped: &str) -> String {
    format!(
        "{}{mapped}) · recent messages may not be saved for resume",
        anchor_text("wall.disk_full")
    )
}

/// A busy screen: a turn under way, its status row, then `live` rows in the
/// live zone, then the composer with `draft` and the footer `footer`.
fn busy(live: &[&str], draft: &str, footer: &str) -> Vec<String> {
    let mut r = rows(&[
        "⏺ Building the workspace.",
        "",
        "⏺ Bash(targo --unverified build)",
        "  ⎿  Running…",
        "",
        "✻ Compiling… (12s · ↓ 1.2k tokens · esc to interrupt)",
    ]);
    r.extend(live.iter().map(|s| (*s).to_string()));
    let mut c = composer(footer);
    c[1] = format!("❯ {draft}");
    r.extend(c);
    r
}

/// THE NOTIFICATION IS READ WHERE CLAUDE CODE DRAWS ONE: right-aligned in
/// the footer under the composer, alone on a footer row, and in the live zone
/// between the status row and the composer; ENOSPC and EDQUOT each read with
/// their code.
#[test]
fn the_notification_reads_under_the_composer_and_in_the_live_zone() {
    let full = notice("disk full — ENOSPC");
    let footer = format!("  ⏵⏵ accept edits on (shift+tab to cycle)          {full}");
    let r = busy(&[], "", &footer);
    let d = disk_full(&r).expect("in the footer");
    assert_eq!((d.code, d.source), (Code::Enospc, Source::Notification));
    assert_eq!(d.message, full);
    assert_eq!(d.row, r.len() - 1);

    let alone = busy(&[], "", &format!("  {full}"));
    assert_eq!(disk_full(&alone).map(|d| d.code), Some(Code::Enospc));

    let live = format!("{full:>118}");
    let r = busy(&["", &live], "", "  ? for shortcuts");
    let d = disk_full(&r).expect("in the live zone");
    assert_eq!(d.row, 7);

    let quota = busy(
        &[],
        "",
        &format!("  {}", notice("disk quota exceeded — EDQUOT")),
    );
    assert_eq!(disk_full(&quota).map(|d| d.code), Some(Code::Edquot));
    let bare = busy(&[], "", &format!("  {}", notice("ENOSPC")));
    assert_eq!(disk_full(&bare).map(|d| d.code), Some(Code::Enospc));
}

/// NEGATIVE CONTROLS, each the same words: a draft in the composer, a
/// message a person queued in the live zone, a transcript row above the
/// status row, a tool's `⎿` row quoting the notification — and the same
/// notification for an error that is no full disk (EROFS, EACCES).
#[test]
fn the_same_words_anywhere_else_are_not_the_notice() {
    let full = notice("disk full — ENOSPC");
    assert_eq!(disk_full(&busy(&[], &full, "  ? for shortcuts")), None);
    let queued = format!("❯ {full}");
    assert_eq!(
        disk_full(&busy(&["", &queued], "", "  ? for shortcuts")),
        None
    );
    let mut transcript = busy(&[], "", "  ? for shortcuts");
    transcript.insert(1, format!("  {full}"));
    transcript.insert(1, format!("  ⎿  {full}"));
    assert_eq!(disk_full(&transcript), None);
    for other in ["read-only filesystem — EROFS", "permission denied — EACCES"] {
        let r = busy(&[], "", &format!("  {}", notice(other)));
        assert_eq!(disk_full(&r), None, "{other}");
    }
    assert_eq!(disk_full(&rows(&[&full])), None);
}

/// A TOOL'S OUTPUT ROW is read as the vendor's `⎿` row at column two, its
/// rest wrapped onto the rows under it; the same words opening a row of the
/// worker's message are not.
#[test]
fn a_tools_lost_output_row_reads_only_as_the_vendors_row() {
    let tool = anchor_text("wall.disk_full.tool");
    let mut r = busy(&[], "", "  ? for shortcuts");
    r.insert(
        4,
        format!("  ⎿  {tool} /private/tmp is full (12MB free). The child"),
    );
    r.insert(
        5,
        "     process's stdout/stderr writes failed with ENOSPC.".to_string(),
    );
    let d = disk_full(&r).expect("the vendor's row");
    assert_eq!((d.source, d.code), (Source::ToolOutput, Code::Enospc));
    assert_eq!(d.row, 4);

    let mut quoted = busy(&[], "", "  ? for shortcuts");
    quoted.insert(4, format!("⏺ {tool} /private/tmp is full, it said."));
    assert_eq!(disk_full(&quoted), None);

    let mut earlier = r.clone();
    earlier.insert(6, "❯ keep going".to_string());
    assert_eq!(disk_full(&earlier), None);
    let mut this_turn = r.clone();
    this_turn.insert(3, "❯ keep going".to_string());
    assert_eq!(disk_full(&this_turn).map(|d| d.row), Some(5));

    let mut path = busy(&[], "", "  ? for shortcuts");
    path.insert(
        4,
        format!("  ⎿  {tool} /Volumes/quota-scratch is full (1MB free)."),
    );
    assert_eq!(disk_full(&path).map(|d| d.code), Some(Code::Enospc));
    let mut quota = busy(&[], "", "  ? for shortcuts");
    quota.insert(4, format!("  ⎿  {tool} /tmp: disk quota exceeded."));
    assert_eq!(disk_full(&quota).map(|d| d.code), Some(Code::Edquot));
}

/// A GOAL THE VENDOR CLEARED ON A FULL DISK reads from its vendor row; the
/// same words in the composer do not.
#[test]
fn a_goal_cleared_on_a_full_disk_reads_from_its_row() {
    let goal = format!(
        "{}disk full): \"ship the release\". Run /goal again to continue.",
        anchor_text("wall.disk_full.goal")
    );
    let mut r = busy(&[], "", "  ? for shortcuts");
    r.insert(6, format!("  ⎿  {goal}"));
    let d = disk_full(&r).expect("the goal's row");
    assert_eq!(d.source, Source::GoalCleared);
    assert_eq!(disk_full(&busy(&[], &goal, "  ? for shortcuts")), None);
}
