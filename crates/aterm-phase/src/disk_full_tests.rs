// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk-full notice reader.

use super::*;
use crate::anchors::anchor_text;
use crate::prompt::fixtures::{
    TOOL_DISK_FULL_2_1_309, TRANSCRIPT_DEGRADED_EACCES_2_1_304, composer, ink_wrap, rows, screen,
};

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

/// The measured 2.1.309 screen, the row its notice opens on, and how many
/// rows the notice takes.
fn measured_tool() -> (Vec<String>, usize, usize) {
    let r = screen(TOOL_DISK_FULL_2_1_309);
    let head = format!("{RESULT_BODY}{}", anchor_text("wall.disk_full.tool"));
    let at = r
        .iter()
        .position(|row| row.starts_with(&head))
        .expect("the measured notice row");
    let rows = r[at..]
        .iter()
        .take_while(|row| row.starts_with(RESULT_BODY))
        .count();
    (r, at, rows)
}

/// The measured screen with its notice's rows replaced by `notice`'s.
fn with_notice(notice: &[&str]) -> Vec<String> {
    let (mut r, at, rows) = measured_tool();
    r.splice(
        at..at + rows,
        notice.iter().map(|n| format!("{RESULT_BODY}{n}")),
    );
    r
}

/// A FAILED COMMAND'S NOTICE AS CLAUDE CODE DRAWS IT (2.1.309, MEASURED):
/// the first row of the command's output, five columns in, right under the
/// vendor's `⎿  Error: Exit code 1` — not on the `⎿` row, so a reader of
/// the `⎿` row alone never sees it. Read with ENOSPC; the inodes form the
/// same; the quota form with EDQUOT; and the head builds before 2.1.309
/// drew, in the same place.
#[test]
fn the_measured_tool_notice_reads_under_its_exit_row() {
    let (r, at, rows) = measured_tool();
    assert!(result_row(&r[at - 1]).is_some(), "{:?}", r[at - 1]);
    assert!(
        r[at - 1].starts_with("  ⎿ \u{a0}"),
        "the vendor's `⎿`, a space and a no-break space: {:?}",
        r[at - 1]
    );
    assert!(exit_row(&r[at - 1]), "a failed command's exit line");
    assert!(result_row(&r[at]).is_none(), "the notice is no `⎿` row");
    assert_eq!(rows, 4, "wrapped onto four rows at 144 columns");
    let d = disk_full(&r).expect("the vendor's place");
    assert_eq!(
        (d.source, d.code, d.row),
        (Source::ToolOutput, Code::Enospc, at)
    );
    assert!(d.message.starts_with(anchor_text("wall.disk_full.tool")));
    assert!(
        r[at..at + rows]
            .iter()
            .any(|row| row.contains(anchor_text("wall.disk_full.tool_dir")))
    );

    let inodes: Vec<String> = r
        .iter()
        .map(|row| {
            row.replacen(
                anchor_text("wall.disk_full.tool"),
                anchor_text("wall.disk_full.tool_inodes"),
                1,
            )
        })
        .collect();
    assert_eq!(
        disk_full(&inodes).map(|d| (d.code, d.row)),
        Some((Code::Enospc, at))
    );
    let quota_head = format!(
        "{}a test write beside this command's output file, on the volume holding /private/tmp/claude-502",
        anchor_text("wall.disk_full.tool_quota")
    );
    let quota_rest = format!(
        "{}, failed with EDQUOT, so whatever this command printed was lost.",
        anchor_text("wall.disk_full.tool_dir").trim_start()
    );
    let quota = with_notice(&[&quota_head, &quota_rest]);
    assert_eq!(disk_full(&quota).map(|d| d.code), Some(Code::Edquot));
    let before = format!("{TOOL_HEAD_BEFORE_2_1_309} /private/tmp/claude-502 is full (3MB free).");
    let old = with_notice(&[&before]);
    assert_eq!(
        disk_full(&old).map(|d| (d.code, d.row)),
        Some((Code::Enospc, at))
    );
    // On the measured `⎿` row itself (its no-break space and all), as a
    // result with no exit line would draw it.
    let mut on_glyph = with_notice(&[]);
    let glyph_row = on_glyph[at - 1].replacen("Error: Exit code 1", &before, 1);
    assert_ne!(glyph_row, on_glyph[at - 1]);
    on_glyph[at - 1] = glyph_row;
    assert_eq!(
        disk_full(&on_glyph).map(|d| (d.source, d.row)),
        Some((Source::ToolOutput, at - 1))
    );
}

/// NEGATIVE CONTROLS, each the measured notice moved or cut: without its
/// directory phrase; a later row of the command's output (a row between it
/// and the `⎿` row); under a `⎿` row that is no failed command's exit line
/// (a command that succeeded, whose output's SECOND line it then is; a
/// first line wrapped onto it; an exit line with no code; a `⎿` with no
/// white space after it); deeper than five columns (no head opens with
/// white space); under a `⏺` row instead of a `⎿` row; as a `⏺` reply of
/// the worker's; in an earlier turn (a person's message drawn since); its
/// directory phrase only on a row under it that is no row of its result.
/// CONTROL: the measured screen reads.
#[test]
fn the_tool_notice_anywhere_else_is_not_read() {
    let (r, at, rows) = measured_tool();
    assert!(disk_full(&r).is_some(), "the screen the controls edit");
    let dir = anchor_text("wall.disk_full.tool_dir");
    let undirected: Vec<String> = r.iter().map(|row| row.replace(dir, "")).collect();
    assert_ne!(undirected, r);
    assert_eq!(disk_full(&undirected), None, "no directory phrase");

    let mut later = r.clone();
    later.insert(at, format!("{RESULT_BODY}make: *** [all] Error 1"));
    assert_eq!(disk_full(&later), None, "not the output's first row");

    let exit = "Error: Exit code 1";
    assert!(r[at - 1].ends_with(exit), "{:?}", r[at - 1]);
    for (above, why) in [
        (
            "# Incident 2026-10-20 (resolved): what the worker saw",
            "the second line of a command that succeeded",
        ),
        (
            "Found 3 matches in the logs; the last one reads",
            "a first line wrapped onto it",
        ),
        ("Error: Exit code ", "an exit line with no code"),
        ("Error: Exit code 1 (killed)", "more after the code"),
    ] {
        let mut not_exit = r.clone();
        not_exit[at - 1] = r[at - 1].replacen(exit, above, 1);
        assert_ne!(not_exit[at - 1], r[at - 1]);
        assert_eq!(disk_full(&not_exit), None, "{why}");
    }
    let mut glued = r.clone();
    glued[at - 1] = format!("{RESULT_GLYPH}{exit}");
    assert_eq!(disk_full(&glued), None, "no white space after the glyph");
    let mut spaced = r.clone();
    spaced[at - 1] = format!("{RESULT_GLYPH}  {exit}");
    assert!(
        disk_full(&spaced).is_some(),
        "the control: two ASCII spaces"
    );

    let head = format!(
        "{}this computer's volume holding /private/tmp/claude-502",
        anchor_text("wall.disk_full.tool")
    );
    let mut stray = with_notice(&[&head]);
    stray.insert(
        at + 1,
        format!("{RESULT_GLYPH}  {} is where it writes", dir.trim_start()),
    );
    assert_eq!(
        disk_full(&stray),
        None,
        "the phrase on no row of the result"
    );
    stray[at + 1] = format!("{RESULT_BODY}{} has 3 MB free", dir.trim_start());
    assert!(
        disk_full(&stray).is_some(),
        "the control: the phrase wrapped"
    );

    let deeper: Vec<String> = r
        .iter()
        .enumerate()
        .map(|(i, row)| {
            if i == at {
                format!(" {row}")
            } else {
                row.clone()
            }
        })
        .collect();
    assert_eq!(disk_full(&deeper), None, "six columns in: no head opens so");

    let mut unheaded = r.clone();
    unheaded[at - 1] = "⏺ Bash(cat notes.txt)".to_string();
    assert_eq!(disk_full(&unheaded), None, "under no `⎿` row");

    let mut reply = r.clone();
    reply[at] = format!("⏺ {}", r[at].trim_start());
    assert_eq!(disk_full(&reply), None, "the worker's own words");

    let mut earlier = r.clone();
    earlier.splice(
        at + rows..at + rows,
        ["".to_string(), "❯ keep going".to_string()],
    );
    assert_eq!(disk_full(&earlier), None, "answered since");

    let footer = busy(
        &[],
        "",
        &format!("  {}soon", anchor_text("wall.disk_full.tool")),
    );
    assert_eq!(
        disk_full(&footer),
        None,
        "a footer's words with no directory"
    );
}

/// A NARROW PANE'S NOTICE READS ACROSS ITS WRAP: each of 2.1.309's four
/// forms (the statfs form MEASURED; the inodes, quota and test-write forms
/// the bundle's templates around the measured directory and tail),
/// rewrapped as Ink wraps a result's text five columns in, reads with its
/// code at every width down to a 40-column pane. CONTROL: the rewrap is
/// the measured rows at 144 columns. At 60 columns the statfs form's
/// directory phrase ends on its third row and at 40 the quota form's on
/// its fifth, so a join of fewer rows than [`TOOL_NOTICE_ROWS`] misses
/// them. NEGATIVE CONTROL: the phrase one row past [`TOOL_NOTICE_ROWS`]
/// (a later line of the command's output) is not read; on the bound's last
/// row, it is.
#[test]
fn a_narrow_panes_notice_reads_across_its_wrap() {
    let (r, at, rows) = measured_tool();
    let measured: Vec<&str> = r[at..at + rows].iter().map(|row| row.trim()).collect();
    let words = measured.join(" ");
    let room = |cols: usize| cols - RESULT_BODY.len();
    assert_eq!(
        ink_wrap(&words, room(144)),
        measured,
        "the rewrap is the vendor's wrap"
    );

    let dir = anchor_text("wall.disk_full.tool_dir");
    let tail = &words[words
        .find("Free space on that volume")
        .expect("the measured tail")..];
    let volume = "/private/tmp/claude-502";
    let beside = format!(
        "a test write beside this command's output file, on the volume holding {volume}{dir}"
    );
    let forms = [
        (words.clone(), Code::Enospc),
        (
            format!(
                "{}this computer's volume holding {volume}{dir} has 812 free inodes, so creating \
                 files there may fail and the command may have failed for that reason. {tail}",
                anchor_text("wall.disk_full.tool_inodes")
            ),
            Code::Enospc,
        ),
        (
            format!(
                "{}{beside}, failed with EDQUOT, so whatever this command printed was lost and its \
                 own writes may have failed with \"Disk quota exceeded\". {tail}",
                anchor_text("wall.disk_full.tool_quota")
            ),
            Code::Edquot,
        ),
        (
            format!(
                "{}{beside}, failed with ENOSPC (the volume, or the user's quota on it, has no space \
                 left), so whatever this command printed was lost. {tail}",
                anchor_text("wall.disk_full.tool")
            ),
            Code::Enospc,
        ),
    ];
    // The row (from 0) a wrapped notice's directory phrase ends on.
    let phrase_ends = |wrapped: &[String]| {
        (0..wrapped.len())
            .find(|&k| wrapped[..=k].join(" ").contains(dir))
            .expect("the directory phrase")
    };
    for cols in [144, 100, 80, 60, 40] {
        for (k, (form, code)) in forms.iter().enumerate() {
            let wrapped = ink_wrap(form, room(cols));
            assert!(
                phrase_ends(&wrapped) < TOOL_NOTICE_ROWS,
                "form {k} at {cols}: {wrapped:#?}"
            );
            let notice: Vec<&str> = wrapped.iter().map(String::as_str).collect();
            assert_eq!(
                disk_full(&with_notice(&notice)).map(|d| (d.code, d.row)),
                Some((*code, at)),
                "form {k} at {cols}: {wrapped:#?}"
            );
        }
    }
    assert_eq!(phrase_ends(&ink_wrap(&words, room(60))), 2);
    assert_eq!(phrase_ends(&ink_wrap(&forms[2].0, room(40))), 4);

    let head = format!(
        "{}this computer's volume holding {volume}",
        anchor_text("wall.disk_full.tool")
    );
    let mut far = vec![head];
    far.extend((1..TOOL_NOTICE_ROWS).map(|k| format!("build step {k} of 9")));
    far.push(format!("{} was set by the owner", dir.trim_start()));
    let notice: Vec<&str> = far.iter().map(String::as_str).collect();
    assert_eq!(
        disk_full(&with_notice(&notice)),
        None,
        "past the notice's rows"
    );
    far.remove(1);
    let notice: Vec<&str> = far.iter().map(String::as_str).collect();
    assert!(
        disk_full(&with_notice(&notice)).is_some(),
        "the control: on its last row"
    );
}

/// A TOOL'S ROW of the head builds before 2.1.309 drew is read as the
/// vendor's `⎿` row at column two, its rest wrapped onto the rows under it;
/// the same words opening a row of the worker's message are not.
#[test]
fn a_tools_lost_output_row_reads_only_as_the_vendors_row() {
    let tool = TOOL_HEAD_BEFORE_2_1_309;
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
