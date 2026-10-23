// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the disk-full notice reader.

use super::*;
use crate::anchors::anchor_text;
use crate::prompt::fixtures::{
    TOOL_DISK_FULL_2_1_309, TOOL_ERROR_AUTO_MODE_2_1_309, TOOL_PRESTART_PRINTED_2_1_309,
    TRANSCRIPT_DEGRADED_EACCES_2_1_304, composer, ink_wrap, rows, screen,
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

/// The measured Bash error result (2.1.309, an auto-mode refusal): its
/// screen, the `⎿` row the result opens on, and how many rows its message
/// takes.
fn measured_error() -> (Vec<String>, usize, usize) {
    let r = screen(TOOL_ERROR_AUTO_MODE_2_1_309);
    let at = r
        .iter()
        .rposition(|row| result_row(row).is_some_and(|t| t.starts_with("Error: Auto mode")))
        .expect("the measured error row");
    let rows = 1 + r[at + 1..]
        .iter()
        .take_while(|row| message_row(row))
        .count();
    (r, at, rows)
}

/// The directory 2.1.309's notices name on the measured build (its
/// temporary directory, `/private/tmp/claude-` and a three-digit uid).
const MEASURED_DIR: &str = "/private/tmp/claude-502";

/// The words of 2.1.309's pre-start notice, from the bundle's template:
/// `said` naming `code`, the measured directory, and the advice.
fn pre_start_words(said: &str, code: &str) -> String {
    format!(
        "{}{said}{code}){}{MEASURED_DIR}{}{} {}",
        anchor_text("wall.disk_full.pre_start"),
        anchor_text("wall.disk_full.pre_start_volume"),
        anchor_text("wall.disk_full.tool_dir"),
        anchor_text("wall.disk_full.pre_start_ran"),
        anchor_text("wall.disk_full.free_space"),
    )
}

/// The pre-start notice of a disk with no space left.
fn full_words() -> String {
    pre_start_words(anchor_text("wall.disk_full.pre_start_full"), "ENOSPC")
}

/// The measured error result's rows replaced by `text` behind `Error: `,
/// wrapped as Claude Code wraps a result's message in a `cols`-column pane:
/// the first row after the measured `⎿` and its white space, the rest five
/// columns in.
fn in_error_place(text: &str, cols: usize) -> Vec<String> {
    let (mut r, at, rows) = measured_error();
    let glyph = r[at][..r[at].find("Error: ").expect("the measured head")].to_string();
    let wrapped = ink_wrap(&format!("Error: {text}"), cols - RESULT_BODY.len());
    let drawn = std::iter::once(format!("{glyph}{}", wrapped[0]))
        .chain(wrapped[1..].iter().map(|w| format!("{RESULT_BODY}{w}")));
    r.splice(at..at + rows, drawn);
    r
}

/// A CALL CLAUDE CODE DID NOT START (2.1.309): its pre-start notice is the
/// call's error result, which Claude Code draws as it draws every Bash error
/// result (MEASURED: an auto-mode refusal): on the vendor's `⎿` row after
/// `Error: `, the rest wrapped exactly five columns in. The bundle's words,
/// rewrapped as the measured message is, read there with their code (ENOSPC
/// for the full form, EDQUOT for the quota form) at every width down to a
/// 40-column pane, where the anchor's own row ends `started:`. CONTROL: the
/// rewrap of the measured message is the
/// vendor's, and the measured screen itself (the classifier's words) reads
/// as no full disk.
#[test]
fn a_command_not_started_reads_in_the_place_of_its_error_result() {
    let (r, at, rows) = measured_error();
    assert!(
        r[at].starts_with("  ⎿ \u{a0}Error: "),
        "the vendor's `⎿`, a space and a no-break space: {:?}",
        r[at]
    );
    assert_eq!(rows, 4, "wrapped onto four rows at 144 columns");
    let measured: Vec<String> = std::iter::once(result_row(&r[at]).expect("a `⎿` row"))
        .chain(r[at + 1..at + rows].iter().map(|row| row.trim()))
        .map(str::to_string)
        .collect();
    assert_eq!(
        ink_wrap(&measured.join(" "), 144 - RESULT_BODY.len()),
        measured,
        "the rewrap is the vendor's wrap"
    );
    assert_eq!(disk_full(&r), None, "the classifier's words");

    let quota = pre_start_words(anchor_text("wall.disk_full.pre_start_quota"), "EDQUOT");
    for cols in [144, 100, 80, 60, 40] {
        for (words, code) in [(full_words(), Code::Enospc), (quota.clone(), Code::Edquot)] {
            let d = disk_full(&in_error_place(&words, cols))
                .unwrap_or_else(|| panic!("{code:?} at {cols}"));
            assert_eq!(
                (d.code, d.source, d.row),
                (code, Source::ToolOutput, at),
                "at {cols}"
            );
            assert!(
                d.message
                    .starts_with(anchor_text("wall.disk_full.pre_start").trim_end()),
                "{:?}",
                d.message
            );
        }
    }
}

/// A PRINTED COPY READS ONLY WHERE ITS ROWS ARE A WORD WRAP'S: the bundle's
/// words printed by a command, drawn in every pane from 40 to 220 columns,
/// read as the notice exactly where each of their row breaks falls just
/// after a space or inside the directory (rows a word wrap of the same
/// words also draws, told apart only by the pane's wrap width) and nowhere
/// else; the notice itself, word-wrapped into its error result's place,
/// reads with its code at every one of those widths. CONTROL: the copy of
/// the full form at 144 (the measured capture), 100, 120, 145, 165 and 200
/// columns breaks a word or opens a row on a space, and does not read.
#[test]
fn a_printed_copy_reads_only_where_its_rows_are_a_word_wraps() {
    let quota = pre_start_words(anchor_text("wall.disk_full.pre_start_quota"), "EDQUOT");
    for (words, code) in [(full_words(), Code::Enospc), (quota, Code::Edquot)] {
        let line = format!("Error: {words}");
        let chars: Vec<char> = line.chars().collect();
        let dir_at = line[..line.find(MEASURED_DIR).expect("the directory")]
            .chars()
            .count();
        let inside_dir = dir_at + 1..dir_at + MEASURED_DIR.chars().count();
        for cols in 40..=220 {
            let real = disk_full(&in_error_place(&words, cols)).map(|d| (d.code, d.source));
            assert_eq!(real, Some((code, Source::ToolOutput)), "{code:?} at {cols}");
            let like_words = (cols - 10..chars.len())
                .step_by(cols - 10)
                .all(|b| chars[b - 1] == ' ' || inside_dir.contains(&b));
            assert_eq!(
                disk_full(&in_printed_place(&line, cols)).is_some(),
                like_words,
                "{code:?} printed at {cols}"
            );
        }
    }
    let line = format!("Error: {}", full_words());
    for cols in [144, 100, 120, 145, 165, 200] {
        assert_eq!(
            disk_full(&in_printed_place(&line, cols)),
            None,
            "printed at {cols}"
        );
    }
}

/// The rows of a command's printed output `line` as Claude Code 2.1.309
/// draws them in a `cols`-column pane, in the place of the measured printed
/// capture's: wrapped by column at `cols - 10` (MEASURED: the capture's
/// 447-character line breaks every 134 characters at 144 columns, `tel|l`
/// included), the first row on the vendor's `⎿` row, the rest five columns
/// in, so a row that opens on a space sits six in.
fn in_printed_place(line: &str, cols: usize) -> Vec<String> {
    let mut r = screen(TOOL_PRESTART_PRINTED_2_1_309);
    let at = r
        .iter()
        .position(|row| result_row(row).is_some_and(|t| t.starts_with("Error: The command")))
        .expect("the printed head");
    let glyph = r[at][..r[at].find("Error: ").expect("the printed head")].to_string();
    let rows = 1 + r[at + 1..]
        .iter()
        .take_while(|row| row.starts_with(RESULT_BODY))
        .count();
    let chars: Vec<char> = line.chars().collect();
    let drawn: Vec<String> = chars
        .chunks(cols - 10)
        .enumerate()
        .map(|(k, chunk)| {
            let text: String = chunk.iter().collect();
            let lead = if k == 0 { glyph.as_str() } else { RESULT_BODY };
            format!("{lead}{}", text.trim_end())
        })
        .collect();
    r.splice(at..at + rows, drawn);
    r
}

/// NEGATIVE CONTROLS, each the pre-start notice moved, cut or changed: a
/// command's printed output of the same words (MEASURED: wrapped by column,
/// a row six columns in and a word broken across rows; the line it prints
/// is the bundle's words, which read in the error result's place); a row
/// of the message one column deeper; the rows under the composer; the
/// words on the `⎿` row with no `Error: `; as a failed command's output
/// under its exit line, with or without `Error: `; without the directory
/// phrase, or with no directory named; with another volume phrase, another account of what failed, or
/// other advice; the full words with EDQUOT, the quota words with ENOSPC,
/// another code, a code with more words in its parenthesis; the `⎿` one
/// column deeper; a `⏺` reply of the worker's; in an earlier turn (a
/// person's message drawn since). CONTROL: the notice in the measured
/// place reads.
#[test]
fn the_pre_start_words_anywhere_else_are_not_read() {
    let words = full_words();
    let placed = in_error_place(&words, 144);
    let (_, at, _) = measured_error();
    assert!(disk_full(&placed).is_some(), "the control");

    let printed = screen(TOOL_PRESTART_PRINTED_2_1_309);
    let head = printed
        .iter()
        .position(|row| result_row(row).is_some_and(|t| t.starts_with("Error: The command")))
        .expect("the printed head");
    let line: String = std::iter::once(result_row(&printed[head]).expect("a `⎿` row"))
        .chain(
            printed[head + 1..]
                .iter()
                .take_while(|row| row.starts_with(RESULT_BODY))
                .map(|row| &row[RESULT_BODY.len()..]),
        )
        .collect();
    assert_eq!(
        line,
        format!("Error: {words}"),
        "the bundle's words, printed"
    );
    assert_eq!(in_printed_place(&line, 144), printed, "the column wrap");
    assert!(
        printed[head + 1].starts_with("      "),
        "{:?}",
        printed[head + 1]
    );
    assert_eq!(disk_full(&printed), None, "a command's printed output");

    let message = placed[at + 1..]
        .iter()
        .take_while(|row| message_row(row))
        .count();
    assert!(message >= 2, "the message wraps at 144 columns");
    let mut deeper = placed.clone();
    deeper[at + 2] = format!(" {}", deeper[at + 2]);
    assert_eq!(disk_full(&deeper), None, "a message row six columns in");

    let mut under = placed.clone();
    let notice: Vec<String> = under.drain(at..=at + message).collect();
    under.extend(notice);
    assert_eq!(disk_full(&under), None, "under the composer");

    let mut bare = placed.clone();
    bare[at] = bare[at].replacen("Error: ", "", 1);
    assert_eq!(disk_full(&bare), None, "no `Error: `");

    let wrapped = ink_wrap(&words, 144 - RESULT_BODY.len());
    let output: Vec<&str> = wrapped.iter().map(String::as_str).collect();
    assert_eq!(disk_full(&with_notice(&output)), None, "a command's output");
    let errored = ink_wrap(&format!("Error: {words}"), 144 - RESULT_BODY.len());
    let output: Vec<&str> = errored.iter().map(String::as_str).collect();
    assert_eq!(
        disk_full(&with_notice(&output)),
        None,
        "a command's output, `Error: ` and all"
    );

    let dir = anchor_text("wall.disk_full.tool_dir");
    let undirected = words.replace(dir, "");
    assert_ne!(undirected, words);
    assert_eq!(
        disk_full(&in_error_place(&undirected, 144)),
        None,
        "no directory phrase"
    );
    let nameless = words.replace(MEASURED_DIR, "");
    assert_ne!(nameless, words);
    assert_eq!(
        disk_full(&in_error_place(&nameless, 144)),
        None,
        "no directory named"
    );
    for (part, other) in [
        ("wall.disk_full.pre_start_volume", " on the disk holding "),
        (
            "wall.disk_full.pre_start_ran",
            ", so its output could not be saved.",
        ),
        (
            "wall.disk_full.free_space",
            "Delete files there that are no longer needed, then retry.",
        ),
    ] {
        let changed = words.replace(anchor_text(part), other);
        assert_ne!(changed, words);
        assert_eq!(
            disk_full(&in_error_place(&changed, 144)),
            None,
            "{part} as {other:?}"
        );
    }
    for (said, code) in [
        ("wall.disk_full.pre_start_full", "EDQUOT"),
        ("wall.disk_full.pre_start_quota", "ENOSPC"),
        ("wall.disk_full.pre_start_full", "EROFS"),
        (
            "wall.disk_full.pre_start_full",
            "ENOSPC: no space left on device",
        ),
    ] {
        let other = pre_start_words(anchor_text(said), code);
        assert_eq!(
            disk_full(&in_error_place(&other, 144)),
            None,
            "{said} with {code}"
        );
    }

    let mut glyph_deeper = placed.clone();
    glyph_deeper[at] = format!(" {}", glyph_deeper[at]);
    assert_eq!(disk_full(&glyph_deeper), None, "the `⎿` one column deeper");
    let mut reply = placed.clone();
    reply[at] = format!("⏺ {words}");
    assert_eq!(disk_full(&reply), None, "the worker's reply");

    let rows = 1 + placed[at + 1..]
        .iter()
        .take_while(|row| message_row(row))
        .count();
    let mut earlier = placed.clone();
    earlier.splice(
        at + rows..at + rows,
        [
            String::new(),
            "❯ carry on".to_string(),
            String::new(),
            "⏺ Done.".to_string(),
        ],
    );
    assert_eq!(disk_full(&earlier), None, "an earlier turn");
}
