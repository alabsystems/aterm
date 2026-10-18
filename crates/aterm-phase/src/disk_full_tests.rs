// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk-full notice reader. The notices are built from the
//! anchors (the drift canary holds those to the store binary), never typed
//! here as prose; the screens around them are synthetic — where the
//! notification is drawn was not measured (the module's docs).

use super::*;
use crate::anchors::anchor_text;
use crate::prompt::fixtures::{composer, rows};

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
    // An unmapped code is drawn alone in the parentheses.
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
    // No composer on the screen: nothing is placed, nothing is read.
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

    // A row of an EARLIER turn — a person's message drawn since — is an
    // incident since answered: not read. The same row under that message
    // is.
    let mut earlier = r.clone();
    earlier.insert(6, "❯ keep going".to_string());
    assert_eq!(disk_full(&earlier), None);
    let mut this_turn = r.clone();
    this_turn.insert(3, "❯ keep going".to_string());
    assert_eq!(disk_full(&this_turn).map(|d| d.row), Some(5));

    // A path that merely says `quota` is no EDQUOT; `disk quota` is.
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
