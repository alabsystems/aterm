// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`barred_rows`], [`header_rows`], [`drawn_as`] and [`pending_bash`] over
//! what Claude Code 2.1.307 and 2.1.308 really drew and wrote (MEASURED
//! 2026-10-22 in the real-Claude lab,
//! `docs/measured/breaker-transcript-and-zsh-2026-10-22.md`): each box
//! capture `claude-2.1.30[78]-box-bash-*` beside the call row the lab's stub
//! made it for, and the transcript as it stood while a box was up.

use aterm_json::Value;
use aterm_phase::prompt::fixtures as phase_fixtures;

use super::*;

/// The call row of every box capture below (`Probe <name>`), and the first
/// `Echo 65 empty substitutions` call: `echo` and 65 `$(true)` as one line.
const CALLS: &str = include_str!("fixtures/claude-2.1.307-transcript-box-probe-calls.jsonl");
/// The transcript from the prompt to the moment the newline twin's box stood:
/// its call is the last row, with no result.
const TWIN_PENDING: &str =
    include_str!("fixtures/claude-2.1.307-transcript-too-many-twin-pending.jsonl");
/// The rows written after that box was refused (Esc): its result first.
const TWIN_REJECTED: &str =
    include_str!("fixtures/claude-2.1.307-transcript-too-many-twin-rejected.jsonl");
/// The twin's call id in [`TWIN_PENDING`].
const TWIN_ID: &str = "toolu_lab16914d0fee39f3eb";

/// The newline twin's own box: the measured too-many box's rows, under the
/// twin's call's row.
pub(crate) const TWIN_SCREEN: &str =
    include_str!("fixtures/claude-2.1.307-box-bash-too-many-newline-twin.txt");

/// Each capture's name and screen: `Probe <name>`'s box.
const SCREENS: &[(&str, &str)] = &[
    ("too-many-newline-twin", TWIN_SCREEN),
    (
        "wrap-long-word",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-long-word.txt"),
    ),
    (
        "wrap-spaces-at-wrap",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-spaces-at-wrap.txt"),
    ),
    (
        "wrap-newlines",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-newlines.txt"),
    ),
    (
        "wrap-word-of-width",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-word-of-width.txt"),
    ),
    (
        "wrap-word-past-width",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-word-past-width.txt"),
    ),
    (
        "wrap-row-139",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-row-139.txt"),
    ),
    (
        "wrap-edge-spaces",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-edge-spaces.txt"),
    ),
    (
        "wrap-100-columns",
        include_str!("fixtures/claude-2.1.307-box-bash-wrap-100-columns.txt"),
    ),
];
/// The tabs capture: a tab drawn to the next stop of eight.
const TABS: &str = include_str!("fixtures/claude-2.1.307-box-bash-wrap-tabs.txt");

/// The command of the call whose description is `description`.
pub(crate) fn command_of(description: &str) -> String {
    command_in(CALLS, description)
}

/// The command of the call in `calls` whose description is `description`.
pub(crate) fn command_in(calls: &str, description: &str) -> String {
    calls
        .lines()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .find_map(|row| {
            let part = row
                .get("message")?
                .get("content")?
                .as_array()?
                .first()?
                .clone();
            let input = part.get("input")?;
            (input.get("description")?.as_str()? == description)
                .then(|| input.get("command")?.as_str().map(str::to_string))
                .flatten()
        })
        .unwrap_or_else(|| panic!("the call `{description}`"))
}

/// The box on a capture's screen, and its rows.
fn boxed(text: &str) -> (PromptV2, Vec<String>) {
    let rows = phase_fixtures::screen(text);
    let p = aterm_phase::prompt::parse_prompt_v2(&rows).expect("the box");
    (p, rows)
}

/// EVERY MEASURED CAPTURE DRAWS AS ITS OWN CALL'S COMMAND: nine boxes at 144
/// and 100 columns (a word broken across rows, spaces at a wrap, newlines with
/// leading and trailing blanks and an empty line, a word exactly as wide as
/// the row and one wider, a row ending one short of the edge, a line opening
/// with a blank, a tab), each matched by the command the stub made it for,
/// in its box and in its call's row above it. The measured too-many box
/// (`echo` and 65 `$(true)` as one line, captured 2026-10-21) and its
/// newline twin (the same text with a newline at each wrap, captured
/// 2026-10-22) draw the SAME box rows, which is why the box alone proves
/// neither; their calls' rows differ (the twin's breaks at its newline, the
/// one-line call's two cells sooner), so each now draws as its own capture
/// alone.
///
/// NEGATIVE CONTROLS: every command against every other capture fails (the
/// 100-column capture's command is the one-line call's, so that pair is the
/// same text); the measured box against its command drawn a cell wider or
/// narrower; a command with an ideographic space, an ambiguous-width
/// letter, an emoji, a zero-width space, a `\r` or an escape is refused by
/// name.
#[test]
fn every_measured_capture_draws_as_its_own_calls_command() {
    let space = command_of("Echo 65 empty substitutions");
    let twin = command_of("Probe too-many-newline-twin");
    assert_eq!(command_of("Probe wrap-100-columns"), space);
    for (name, text) in SCREENS {
        let (p, rows) = boxed(text);
        let command = command_of(&format!("Probe {name}"));
        assert_eq!(drawn_as(&p, &rows, &command), Ok(()), "{name}");
        for (other, _) in SCREENS.iter().filter(|(o, _)| o != name) {
            let theirs = command_of(&format!("Probe {other}"));
            if theirs != command {
                assert!(drawn_as(&p, &rows, &theirs).is_err(), "{name} by {other}");
            }
        }
    }
    let (tabs_p, tabs_rows) = boxed(TABS);
    assert_eq!(
        drawn_as(&tabs_p, &tabs_rows, &command_of("Probe wrap-tabs")),
        Ok(())
    );
    assert_ne!(space, twin);
    assert_eq!(space.replace('\n', " "), twin.replace('\n', " "));
    let (one_line, one_line_rows) = boxed(phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307);
    let (twin_p, twin_rows) = boxed(SCREENS[0].1);
    assert_eq!(drawn_as(&one_line, &one_line_rows, &space), Ok(()));
    assert_eq!(drawn_as(&twin_p, &twin_rows, &twin), Ok(()));
    let trimmed = |command: &str| -> Vec<String> {
        barred_rows(command, 140)
            .expect("drawn")
            .iter()
            .map(|r| r.trim_end().to_string())
            .collect()
    };
    assert_eq!(trimmed(&space), trimmed(&twin), "the same box rows");
    for (p, rows, command) in [
        (&one_line, &one_line_rows, &twin),
        (&twin_p, &twin_rows, &space),
    ] {
        assert!(
            drawn_as(p, rows, command).is_err_and(|e| e.contains("the call's row")),
            "{:?}",
            drawn_as(p, rows, command)
        );
    }
    // NEGATIVE CONTROLS
    let row_139 = command_of("Probe wrap-row-139");
    for (command, text, width) in [
        (&space, phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307, 139),
        (&row_139, SCREENS[6].1, 141),
    ] {
        let (_, rows) = boxed(text);
        let at = |w: usize| -> Vec<String> {
            barred_rows(command, w)
                .expect("drawn")
                .into_iter()
                .map(|r| format!(" \u{2502} {r}").trim_end().to_string())
                .collect()
        };
        let shown: Vec<String> = rows
            .iter()
            .skip_while(|r| !r.starts_with('\u{254c}'))
            .skip(1)
            .take_while(|r| !r.starts_with('\u{254c}'))
            .cloned()
            .collect();
        assert_eq!(at(140), shown, "{width}");
        assert_ne!(at(width), shown, "{width}");
    }
    for (odd, named) in [
        ("echo \u{3000}x", "\\u{3000}"),
        ("echo \u{e9}", "\\u{e9}"),
        ("echo \u{1f600}", "\\u{1f600}"),
        ("echo a\u{200b}b", "\\u{200b}"),
        ("echo a\r\nls", "\\r"),
        ("echo \u{1b}[31m", "\\u{1b}"),
    ] {
        for refused in [barred_rows(odd, 140), header_rows(odd, 138)] {
            assert!(
                refused.as_ref().is_err_and(|e| e.contains(named)),
                "{odd:?}: {refused:?}"
            );
        }
        assert!(drawn_as(&one_line, &one_line_rows, odd).is_err(), "{odd:?}");
    }
}

/// THE MODEL'S OWN STEPS, each from a capture above: a row a wrap starts
/// loses one leading space and keeps the rest (`   $(true)` after a full row
/// draws two blanks); a word exactly as wide as the row after a full row
/// leaves an empty row; a word one wider is broken one short of the edge on
/// the row after the full one; a line's own leading blank is kept.
///
/// NEGATIVE CONTROLS: each step undone draws the capture otherwise.
#[test]
fn the_wrap_drops_one_blank_at_a_wrap_and_breaks_a_wide_word() {
    let s = " $(true)";
    let full = format!("echo{}", s.repeat(17));
    assert_eq!(full.len(), 140);
    let spaces = barred_rows(&format!("{full}   $(true)"), 140).expect("drawn");
    assert_eq!(spaces, vec![full.clone(), "  $(true)".to_string()]);
    let word = barred_rows(&format!("{full} {}", "b".repeat(140)), 140).expect("drawn");
    assert_eq!(word, vec![full.clone(), String::new(), "b".repeat(140)]);
    let wider = barred_rows(&format!("{full} {} x", "b".repeat(141)), 140).expect("drawn");
    assert_eq!(
        wider,
        vec![full.clone(), "b".repeat(139), "bb x".to_string()]
    );
    let edge = barred_rows(" echo a", 140).expect("drawn");
    assert_eq!(edge, vec![" echo a".to_string()]);
    let lines = barred_rows("echo a\n\n   echo b  ", 140).expect("drawn");
    assert_eq!(lines, vec!["echo a", "", "   echo b  "]);
    // NEGATIVE CONTROLS
    assert_ne!(spaces[1], "   $(true)");
    assert_ne!(word.len(), 2);
    assert_ne!(wider[1].len(), 140);
}

/// THE CALL WAITING ON THE BOX IS THE TRANSCRIPT'S ONE PENDING BASH CALL: the
/// transcript as it stood while the twin's box was up names the twin's call
/// and its exact command (passing over the `api-request-blob` row, which
/// echoes the previous call's result, and the `api-request` row).
///
/// NEGATIVE CONTROLS: the same transcript with the rows written after the box
/// was refused (its result) names none; a call already spent names none; a
/// second call waiting beside it names none (`2 calls`); a subagent's call
/// beside it is passed over; a call of another tool, alone, is no Bash call;
/// a result in an `api-request-blob` row answers nothing; a person's prompt
/// after it (a row whose parent is the call's) ends it; a file with nothing
/// waiting names none.
#[test]
fn the_call_waiting_is_the_transcripts_one_pending_bash_call() {
    let twin = command_of("Probe too-many-newline-twin");
    assert_eq!(
        pending_bash(TWIN_PENDING, &[]),
        Ok(BoxCommand {
            id: TWIN_ID.to_string(),
            command: twin.clone()
        })
    );
    // NEGATIVE CONTROLS
    let answered = format!("{TWIN_PENDING}{TWIN_REJECTED}");
    assert!(pending_bash(&answered, &[]).is_err_and(|e| e.contains("no call waiting")));
    assert!(
        pending_bash(TWIN_PENDING, &[TWIN_ID.to_string()])
            .is_err_and(|e| e.contains("no call waiting"))
    );
    let call = TWIN_PENDING
        .lines()
        .last()
        .expect("the twin's call row")
        .to_string();
    let second = call.replace(TWIN_ID, "toolu_labsecondcall00000");
    let two = format!("{TWIN_PENDING}{second}\n");
    assert!(pending_bash(&two, &[]).is_err_and(|e| e.contains("2 calls waiting")));
    let sidechain = second.replace("\"isSidechain\":false", "\"isSidechain\":true");
    assert_ne!(sidechain, second);
    assert_eq!(
        pending_bash(&format!("{TWIN_PENDING}{sidechain}\n"), &[]).map(|c| c.id),
        Ok(TWIN_ID.to_string())
    );
    let read = second.replace("\"name\":\"Bash\"", "\"name\":\"Read\"");
    assert!(
        pending_bash(&format!("{read}\n"), &[]).is_err_and(|e| e.contains("Read's, not a Bash"))
    );
    let blob = TWIN_PENDING
        .lines()
        .find(|l| l.starts_with("{\"type\":\"api-request-blob\""))
        .expect("the blob row");
    let key = "\"tool_use_id\":\"";
    let at = blob.find(key).expect("its echoed result") + key.len();
    let old = &blob[at..at + blob[at..].find('"').expect("the id's end")];
    let echoed = blob.replace(old, TWIN_ID);
    assert!(echoed.contains(TWIN_ID));
    assert_eq!(
        pending_bash(&format!("{TWIN_PENDING}{echoed}\n"), &[]).map(|c| c.id),
        Ok(TWIN_ID.to_string())
    );
    // The measured prompt row asked again after the call: its `uuid` fresh,
    // its parent the call's row (a row that is the call's own ancestor is
    // its own turn's prompt, never a later one).
    let prompt = TWIN_PENDING
        .lines()
        .find(|l| l.contains("\"type\":\"user\""))
        .expect("the prompt row");
    let field = |row: &str, key: &str| -> String {
        let value = aterm_json::from_str::<Value>(row).expect("a row");
        value
            .get(key)
            .and_then(Value::as_str)
            .expect("the field")
            .to_string()
    };
    let later = prompt
        .replace(
            &field(prompt, "uuid"),
            "00000000-0000-4000-8000-00000000p002",
        )
        .replace(&field(prompt, "parentUuid"), &field(&call, "uuid"));
    assert!(
        pending_bash(&format!("{TWIN_PENDING}{later}\n"), &[])
            .is_err_and(|e| e.contains("no call waiting"))
    );
    assert!(pending_bash("", &[]).is_err());
}

/// The measured too-many box (manual mode) with the row reading `from` (its
/// trailing blanks aside) replaced by `to`, and its rows.
fn measured_with(from: &str, to: &str) -> (PromptV2, Vec<String>) {
    let mut rows = phase_fixtures::screen(phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307);
    let row = rows
        .iter_mut()
        .find(|r| r.trim_end() == from)
        .unwrap_or_else(|| panic!("the row `{from}`"));
    *row = to.to_string();
    let p = aterm_phase::prompt::parse_prompt_v2(&rows).expect("the box");
    (p, rows)
}

/// THE ROW COUNT IS PART OF THE JOIN: a command that draws only the box's
/// first rows, or the box's rows and one more, is not what the box draws,
/// though every row it draws matches one of the box's. The newline twin
/// draws its own capture's four rows (control); the twin less its last line
/// draws the first three exactly, and the twin with one more line draws the
/// four and a fifth.
///
/// NEGATIVE CONTROLS: both are refused, naming the counts.
#[test]
fn a_command_drawing_fewer_or_more_rows_than_the_box_is_not_the_box() {
    let (p, rows) = boxed(SCREENS[0].1);
    let twin = command_of("Probe too-many-newline-twin");
    assert_eq!(drawn_as(&p, &rows, &twin), Ok(()));
    let first_three = &twin[..twin.rfind('\n').expect("four lines")];
    assert_eq!(barred_rows(first_three, 140).expect("drawn").len(), 3);
    let shown: Vec<&str> = rows
        .iter()
        .skip_while(|r| !r.starts_with('\u{254c}'))
        .skip(1)
        .take(3)
        .map(|r| r.trim_end())
        .collect();
    let drawn: Vec<String> = barred_rows(first_three, 140)
        .expect("drawn")
        .into_iter()
        .map(|r| format!(" \u{2502} {r}").trim_end().to_string())
        .collect();
    assert_eq!(drawn, shown, "a strict prefix of the box's rows");
    // NEGATIVE CONTROLS
    assert_eq!(
        drawn_as(&p, &rows, first_three),
        Err("the command draws 3 rows 140 cells wide, and the box shows 4".to_string())
    );
    let one_more = format!("{twin}\n$(true)");
    assert_eq!(
        drawn_as(&p, &rows, &one_more),
        Err("the command draws 5 rows 140 cells wide, and the box shows 4".to_string())
    );
}

/// A CALL ANSWERED BY ITS RESULT WAITS NO MORE, with no prompt row after it:
/// Claude Code 2.1.300's transcript while its Edit box stood (the real-Claude
/// lab, 2026-10-15) holds a Read call, the Read's result in a user row of
/// results alone, then the Edit call, so the one call waiting is the Edit
/// (no Bash call). And 2.1.307's newline twin answered by its result row
/// alone (the measured rejection's first row), then a second Bash call: the
/// second is the one waiting.
///
/// NEGATIVE CONTROLS: the same files with the result rows taken out hold two
/// calls waiting.
#[test]
fn a_result_row_takes_its_call_away_and_leaves_the_next() {
    const EDIT_PENDING: &str =
        include_str!("../fixtures/claude-2.1.300-transcript-box-edit-pending.jsonl");
    assert_eq!(
        pending_bash(EDIT_PENDING, &[]),
        Err("the call waiting is Edit's, not a Bash call".to_string())
    );
    let result = TWIN_REJECTED
        .lines()
        .find(|l| l.contains("\"tool_result\""))
        .expect("the result row");
    assert!(result.contains(TWIN_ID));
    let call = TWIN_PENDING.lines().last().expect("the twin's call row");
    let second = call.replace(TWIN_ID, "toolu_labsecondcall00000");
    let answered = format!("{TWIN_PENDING}{result}\n{second}\n");
    assert_eq!(
        pending_bash(&answered, &[]).map(|c| c.id),
        Ok("toolu_labsecondcall00000".to_string())
    );
    // NEGATIVE CONTROLS
    let unanswered: String = EDIT_PENDING
        .lines()
        .filter(|l| !l.contains("\"tool_result\"") || l.contains("api-request"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(
        pending_bash(&unanswered, &[]).is_err_and(|e| e.contains("2 calls waiting")),
        "{:?}",
        pending_bash(&unanswered, &[])
    );
    assert!(
        pending_bash(&format!("{TWIN_PENDING}{second}\n"), &[])
            .is_err_and(|e| e.contains("2 calls waiting"))
    );
}

/// THE BOX A TRANSCRIPT'S COMMAND IS MATCHED TO ([`may_prove`]): the measured
/// too-many box, read whole, from the main agent, its footer its own keys.
///
/// NEGATIVE CONTROLS, each refused by [`may_prove`] and so by [`drawn_as`]
/// though its rows still draw as the command: the footer ending in the
/// background agents' note (a subagent's box), the footer cut short, the
/// breaker's note another kind's (`some nested commands …`), the box's head
/// off the screen (its title row and the rows above it gone).
#[test]
fn only_the_main_agents_whole_too_many_box_is_matched_to_a_command() {
    let space = command_of("Echo 65 empty substitutions");
    let (p, rows) = boxed(phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307);
    assert!(may_prove(&p));
    assert_eq!(drawn_as(&p, &rows, &space), Ok(()));
    // NEGATIVE CONTROLS
    let footer = " Esc to cancel · Tab to amend";
    let note = " \u{2502} Dangerous rm operation not ruled out — too many command substitutions to \
                analyze (65)";
    for (name, from, to) in [
        (
            "a subagent's footer",
            footer,
            format!(
                "{footer} · ctrl+x ctrl+k {}",
                aterm_phase::anchor_text("hint.agents_twice")
            ),
        ),
        (
            "a footer cut short",
            footer,
            format!("{footer} · ctrl+x ct\u{2026}"),
        ),
        (
            "another breaker",
            note,
            " \u{2502} Dangerous rm operation not ruled out — some nested commands or function \
             substitutions could not be analyzed"
                .to_string(),
        ),
    ] {
        let (p, rows) = measured_with(from, &to);
        assert!(!may_prove(&p), "{name}: {p:?}");
        assert!(drawn_as(&p, &rows, &space).is_err(), "{name}");
    }
    let mut cut = phase_fixtures::screen(phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307);
    let title = cut
        .iter()
        .position(|r| r.trim_end() == " Bash command")
        .expect("the title row");
    cut.drain(..=title);
    let p = aterm_phase::prompt::parse_prompt_v2(&cut).expect("the box");
    assert!(p.head_off_screen, "{p:?}");
    assert!(!may_prove(&p), "{p:?}");
}

/// THE TRANSCRIPT IS READ ONLY FOR CLAUDE CODE'S TOO-MANY BOX THAT THE SCREEN
/// LEAVES TO A PERSON ([`wanted`]): the measured box, drawn by Claude Code,
/// escalated on its screen readings.
///
/// NEGATIVE CONTROLS: the same box read for any other program; the box the
/// screen's readings decide (the escalation asked, and not); a box
/// [`may_prove`] refuses (another breaker's note), with the escalation never
/// asked.
#[test]
fn the_transcript_is_read_only_for_claudes_too_many_box_the_screen_escalates() {
    let (p, _) = boxed(phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307);
    assert!(wanted(Program::Claude, &p, || true));
    // NEGATIVE CONTROLS
    for program in [
        Program::Codex,
        Program::Agy,
        Program::Muse,
        Program::Gemini,
        Program::Generic,
    ] {
        assert!(!wanted(program, &p, || true), "{program:?}");
    }
    let mut asked = false;
    assert!(!wanted(Program::Claude, &p, || {
        asked = true;
        false
    }));
    assert!(asked);
    let (other, _) = measured_with(
        " \u{2502} Dangerous rm operation not ruled out — too many command substitutions to analyze \
         (65)",
        " \u{2502} Dangerous rm operation not ruled out — some nested commands or function \
         substitutions could not be analyzed",
    );
    assert!(!wanted(Program::Claude, &other, || panic!("asked")));
}

/// A conversation's first turn as 2.1.307 wrote it in a bypass session (the
/// real-Claude lab `b2l1`, 2026-10-22): the assistant row of the too-many
/// box's call (`echo` and 65 `$(true)`) BEFORE the prompt row `toolstart`
/// and the attachment rows its `parentUuid` chain runs up through.
const FIRST_TURN: &str =
    include_str!("fixtures/claude-2.1.307-transcript-too-many-first-turn-pending.jsonl");
/// The call's id in [`FIRST_TURN`].
const FIRST_TURN_ID: &str = "toolu_labca5ab29a1491b609";

/// THE CONVERSATION'S ORDER, NOT THE FILE'S (MEASURED 2026-10-22): in
/// [`FIRST_TURN`] the prompt row follows the call's row in the file, but it
/// is the call's own ancestor, so the call is still the one waiting, with
/// the measured box's command. And a result answers its call wherever it is
/// written: a result row (constructed) for the call written BEFORE the call's
/// row leaves nothing waiting.
///
/// NEGATIVE CONTROLS: a later prompt (a constructed row whose parent is the
/// call's row) ends the call; the same file with the call row's chain cut
/// (its `parentUuid` a uuid the tail does not hold) cannot show the prompt
/// is its own, and the prompt written after it ends it.
#[test]
fn a_call_written_before_its_own_prompt_row_is_still_waiting() {
    let space = command_of("Echo 65 empty substitutions");
    assert_eq!(
        pending_bash(FIRST_TURN, &[]),
        Ok(BoxCommand {
            id: FIRST_TURN_ID.to_string(),
            command: space
        })
    );
    let rows: Vec<&str> = FIRST_TURN.lines().collect();
    let call_at = rows
        .iter()
        .position(|l| l.contains(FIRST_TURN_ID))
        .expect("the call row");
    let prompt_at = rows
        .iter()
        .position(|l| l.contains("\"type\":\"user\""))
        .expect("the prompt row");
    assert!(
        prompt_at > call_at,
        "the prompt row is written after the call"
    );
    let call_row = aterm_json::from_str::<Value>(rows[call_at]).expect("the call row");
    let call_uuid = call_row
        .get("uuid")
        .and_then(Value::as_str)
        .expect("its uuid");
    let result = format!(
        r#"{{"parentUuid":"{call_uuid}","isSidechain":false,"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","content":"ok","tool_use_id":"{FIRST_TURN_ID}"}}]}},"uuid":"00000000-0000-4000-8000-00000000r001"}}"#
    );
    let result_first = format!("{result}\n{FIRST_TURN}");
    assert!(
        pending_bash(&result_first, &[]).is_err_and(|e| e.contains("no call waiting")),
        "{:?}",
        pending_bash(&result_first, &[])
    );
    // NEGATIVE CONTROLS
    let later = format!(
        r#"{{"parentUuid":"{call_uuid}","isSidechain":false,"type":"user","message":{{"role":"user","content":"and then"}},"uuid":"00000000-0000-4000-8000-00000000p001"}}"#
    );
    assert!(
        pending_bash(&format!("{FIRST_TURN}{later}\n"), &[])
            .is_err_and(|e| e.contains("no call waiting"))
    );
    let parent = call_row
        .get("parentUuid")
        .and_then(Value::as_str)
        .expect("its parent");
    let cut = FIRST_TURN.replacen(
        &format!("\"parentUuid\":\"{parent}\""),
        "\"parentUuid\":\"00000000-0000-4000-8000-0000000c0001\"",
        1,
    );
    assert_ne!(cut, FIRST_TURN);
    assert!(
        pending_bash(&cut, &[]).is_err_and(|e| e.contains("no call waiting")),
        "{:?}",
        pending_bash(&cut, &[])
    );
}

/// The call row of every 2.1.308 box capture below (`Probe <name>`).
pub(crate) const CALLS_308: &str =
    include_str!("fixtures/claude-2.1.308-transcript-box-probe-calls.jsonl");

/// Each 2.1.308 capture's name and screen: `Probe <name>`'s box, under its
/// call's row (MEASURED 2026-10-22, the measured note's §H and §W): the
/// call's row's width, cut and trim (`h-…`), tabs, CJK, kana, Hangul and
/// fullwidth forms, and both ends of every block [`WIDE`] holds, in the box
/// (`w-ranges`) and in the call's row (`w-ranges` for the two CJK blocks,
/// `hr-…` for the other five).
pub(crate) const SCREENS_308: &[(&str, &str)] = &[
    (
        "h-one-line",
        include_str!("fixtures/claude-2.1.308-box-bash-h-one-line.txt"),
    ),
    (
        "h-twin",
        include_str!("fixtures/claude-2.1.308-box-bash-h-twin.txt"),
    ),
    (
        "h-width-a",
        include_str!("fixtures/claude-2.1.308-box-bash-h-width-a.txt"),
    ),
    (
        "h-width-b",
        include_str!("fixtures/claude-2.1.308-box-bash-h-width-b.txt"),
    ),
    (
        "h-long-word",
        include_str!("fixtures/claude-2.1.308-box-bash-h-long-word.txt"),
    ),
    (
        "h-newlines",
        include_str!("fixtures/claude-2.1.308-box-bash-h-newlines.txt"),
    ),
    (
        "h-edge-spaces",
        include_str!("fixtures/claude-2.1.308-box-bash-h-edge-spaces.txt"),
    ),
    (
        "h-full-split",
        include_str!("fixtures/claude-2.1.308-box-bash-h-full-split.txt"),
    ),
    (
        "h-full-join",
        include_str!("fixtures/claude-2.1.308-box-bash-h-full-join.txt"),
    ),
    (
        "h-short-lines",
        include_str!("fixtures/claude-2.1.308-box-bash-h-short-lines.txt"),
    ),
    (
        "h-wrap-same",
        include_str!("fixtures/claude-2.1.308-box-bash-h-wrap-same.txt"),
    ),
    (
        "h-wrap-same-twin",
        include_str!("fixtures/claude-2.1.308-box-bash-h-wrap-same-twin.txt"),
    ),
    (
        "w-tabs",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tabs.txt"),
    ),
    (
        "w-tab-at-wrap",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-at-wrap.txt"),
    ),
    (
        "w-tab-late",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-late.txt"),
    ),
    (
        "w-tab-lead",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-lead.txt"),
    ),
    (
        "w-tab-runs",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-runs.txt"),
    ),
    (
        "w-tab-second-line",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-second-line.txt"),
    ),
    (
        "w-tab-header-line2",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-header-line2.txt"),
    ),
    (
        "w-tab-after-wide",
        include_str!("fixtures/claude-2.1.308-box-bash-w-tab-after-wide.txt"),
    ),
    (
        "w-cjk-long",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-long.txt"),
    ),
    (
        "w-cjk-odd",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-odd.txt"),
    ),
    (
        "w-cjk-words",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-words.txt"),
    ),
    (
        "w-cjk-edge",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-edge.txt"),
    ),
    (
        "w-cjk-header",
        include_str!("fixtures/claude-2.1.308-box-bash-w-cjk-header.txt"),
    ),
    (
        "w-hangul",
        include_str!("fixtures/claude-2.1.308-box-bash-w-hangul.txt"),
    ),
    (
        "w-ranges",
        include_str!("fixtures/claude-2.1.308-box-bash-w-ranges.txt"),
    ),
    (
        "hr-kana",
        include_str!("fixtures/claude-2.1.308-box-bash-hr-kana.txt"),
    ),
    (
        "hr-hangul-fullwidth",
        include_str!("fixtures/claude-2.1.308-box-bash-hr-hangul-fullwidth.txt"),
    ),
    (
        "hr-signs",
        include_str!("fixtures/claude-2.1.308-box-bash-hr-signs.txt"),
    ),
];

/// The pairs of 2.1.308 captures whose commands draw both rows alike (the
/// residual the measured note keeps): a newline where the other has a space
/// at a break both rows share (`h-wrap-same`), and a word one splits with a
/// blank where it fills a row and the other does not (`h-full-split`).
const ALIKE_308: &[(&str, &str)] = &[
    ("h-wrap-same", "h-wrap-same-twin"),
    ("h-full-split", "h-full-join"),
];

/// The 2.1.308 capture `name`'s screen text.
fn capture_308(name: &str) -> &'static str {
    SCREENS_308
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| *t)
        .unwrap_or_else(|| panic!("the capture {name}"))
}

/// A SCREEN FOR A PAIR NO CAPTURE HOLDS: the `h-one-line` capture (144
/// columns) with its call's row and its box's rows redrawn for `command` by
/// this model ([`header_rows`] at 138 cells, led by `⏺ Bash` and six blanks;
/// [`barred_rows`] at 140, led by ` │ `), and its box.
pub(crate) fn drawn_screen(command: &str) -> (PromptV2, Vec<String>) {
    let rows = phase_fixtures::screen(capture_308("h-one-line"));
    let lead = rows
        .iter()
        .position(|r| r.starts_with("\u{23fa} Bash("))
        .expect("the call's row");
    let waiting = rows
        .iter()
        .position(|r| r.trim_end() == WAITING)
        .expect("the Waiting row");
    let open = rows
        .iter()
        .position(|r| r.starts_with('\u{254c}'))
        .expect("the dashed rule");
    let close = open
        + 1
        + rows[open + 1..]
            .iter()
            .position(|r| r.starts_with('\u{254c}'))
            .expect("the closing dashed rule");
    let mut out = rows[..lead].to_vec();
    for (k, row) in header_rows(command, 138)
        .expect("drawn")
        .into_iter()
        .enumerate()
    {
        let lead = if k == 0 { "\u{23fa} Bash" } else { "      " };
        out.push(format!("{lead}{row}"));
    }
    out.extend_from_slice(&rows[waiting..=open]);
    for row in barred_rows(command, 140).expect("drawn") {
        out.push(format!(" \u{2502} {row}"));
    }
    out.extend_from_slice(&rows[close..]);
    let p = aterm_phase::prompt::parse_prompt_v2(&out).expect("the box");
    (p, out)
}

/// The 2.1.308 captures whose commands hold a character this model refuses:
/// an ideographic space inside (`ＡＢＣ　ｘｙｚ`) and at both ends, and a
/// zero-width space and a soft hyphen (the box draws each as `U+FFFD`, the
/// call's row draws neither).
pub(crate) const REFUSED_308: &[(&str, &str, &str)] = &[
    (
        "w-fullwidth",
        include_str!("fixtures/claude-2.1.308-box-bash-w-fullwidth.txt"),
        "\\u{3000}",
    ),
    (
        "w-ideo-space-edges",
        include_str!("fixtures/claude-2.1.308-box-bash-w-ideo-space-edges.txt"),
        "\\u{3000}",
    ),
    (
        "w-zero-width",
        include_str!("fixtures/claude-2.1.308-box-bash-w-zero-width.txt"),
        "\\u{200b}",
    ),
];

/// The Bash row of a command of two lines and under 160 units (`  ` and 65
/// empty backtick pairs, then `echo x`), shown as it is: its blanks kept,
/// no `…`. Its box is the parser's (`Parse error`), not the too-many box.
const SHORT_BACKTICKS: &str =
    include_str!("fixtures/claude-2.1.308-box-bash-w-short-backticks.txt");

/// The rows between a capture's dashed rules, trailing blanks dropped.
fn box_rows(rows: &[String]) -> Vec<String> {
    rows.iter()
        .skip_while(|r| !r.starts_with('\u{254c}'))
        .skip(1)
        .take_while(|r| !r.starts_with('\u{254c}'))
        .map(|r| r.trim_end().to_string())
        .collect()
}

/// EVERY 2.1.308 CAPTURE DRAWS AS ITS OWN CALL'S COMMAND, IN THE BOX AND IN
/// THE CALL'S ROW (MEASURED 2026-10-22, 144 columns): twelve probes of the
/// call's row (its cut to two lines and 160 units, its trim, its two cells
/// less than the box, a newline twin, a word split or not where it fills a
/// row), eight of tabs (after a word, at a wrap, late in a long line,
/// leading, in runs, on a second line, on the call's row's second line,
/// after a wide character) and eleven of East Asian wide characters (a run
/// broken one cell short of a row's end, at an odd column, among words, at
/// a row's edge, cut on the call's row, kana and Hangul, every block's both
/// ends in the box and in the call's row).
///
/// NEGATIVE CONTROLS: every command against every other capture fails but
/// for the two measured pairs whose rows draw alike ([`ALIKE_308`], the
/// residual); each refused capture is refused naming its character, and its
/// box draws the zero-width space and soft hyphen as `U+FFFD`.
#[test]
fn every_2_1_308_capture_draws_as_its_own_calls_command_in_the_box_and_the_calls_row() {
    for (name, text) in SCREENS_308 {
        let (p, rows) = boxed(text);
        let command = command_in(CALLS_308, &format!("Probe {name}"));
        assert_eq!(drawn_as(&p, &rows, &command), Ok(()), "{name}");
        for (other, _) in SCREENS_308.iter().filter(|(o, _)| o != name) {
            let theirs = command_in(CALLS_308, &format!("Probe {other}"));
            let alike = ALIKE_308
                .iter()
                .any(|(a, b)| (a, b) == (name, other) || (b, a) == (name, other));
            assert_eq!(
                drawn_as(&p, &rows, &theirs).is_ok(),
                alike,
                "{name} by {other}"
            );
        }
    }
    // NEGATIVE CONTROLS
    for (name, text, named) in REFUSED_308 {
        let (p, rows) = boxed(text);
        let command = command_in(CALLS_308, &format!("Probe {name}"));
        assert!(
            drawn_as(&p, &rows, &command).is_err_and(|e| e.contains(named)),
            "{name}: {:?}",
            drawn_as(&p, &rows, &command)
        );
    }
    let (_, rows) = boxed(REFUSED_308[2].1);
    assert!(
        box_rows(&rows)[0].contains("a\u{fffd}b c\u{fffd}x"),
        "{rows:?}"
    );
}

/// THE CALL'S ROW TELLS A NEWLINE FROM A WRAP THE BOX DRAWS ALIKE, where
/// its own wrap falls elsewhere (MEASURED 2026-10-22, 2.1.308): `echo` and
/// 65 `$(true)` as one line, and the same text with a newline at the box's
/// first wrap, draw the same box rows; the call's row, two cells narrower,
/// breaks the one-line text before its 17th `$(true)` and the twin at its
/// newline, so each command draws as its own capture alone. A row whose dot
/// is blinked off (`  Bash(`) is the same row.
///
/// NEGATIVE CONTROLS: where the newline falls on the call's row's own break
/// (`h-wrap-same` and its twin) both rows draw alike and both commands draw
/// as both captures (the residual the measured note keeps); with the
/// `Waiting…` row gone, the blank row under it gone, the call's row drawn
/// under another tool's name, or its first row cut off the screen, no
/// command draws as the box.
#[test]
fn the_calls_row_tells_a_newline_from_a_wrap_the_box_draws_alike() {
    let screen = |name: &str| {
        let text = SCREENS_308
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| *t)
            .expect(name);
        boxed(text)
    };
    let probe = |name: &str| command_in(CALLS_308, &format!("Probe {name}"));
    let (one_p, one_rows) = screen("h-one-line");
    let (twin_p, twin_rows) = screen("h-twin");
    let (one, twin) = (probe("h-one-line"), probe("h-twin"));
    assert_eq!(one.replace('\n', " "), twin.replace('\n', " "));
    assert_eq!(box_rows(&one_rows), box_rows(&twin_rows), "the same box");
    assert_eq!(drawn_as(&one_p, &one_rows, &one), Ok(()));
    assert_eq!(drawn_as(&twin_p, &twin_rows, &twin), Ok(()));
    for (p, rows, other) in [(&one_p, &one_rows, &twin), (&twin_p, &twin_rows, &one)] {
        assert!(
            drawn_as(p, rows, other).is_err_and(|e| e.starts_with("the call's row ")
                && e.ends_with(" above the box draws otherwise than the command's")),
            "{:?}",
            drawn_as(p, rows, other)
        );
    }
    let mut blinked = one_rows.clone();
    let lead = blinked
        .iter()
        .position(|r| r.starts_with("\u{23fa} Bash("))
        .expect("the call's row");
    blinked[lead] = blinked[lead].replacen("\u{23fa} Bash(", "  Bash(", 1);
    let p = aterm_phase::prompt::parse_prompt_v2(&blinked).expect("the box");
    assert_eq!(drawn_as(&p, &blinked, &one), Ok(()));
    // NEGATIVE CONTROLS
    let (same_p, same_rows) = screen("h-wrap-same");
    let (same_twin_p, same_twin_rows) = screen("h-wrap-same-twin");
    let (same, same_twin) = (probe("h-wrap-same"), probe("h-wrap-same-twin"));
    assert_ne!(same, same_twin);
    for command in [&same, &same_twin] {
        assert_eq!(drawn_as(&same_p, &same_rows, command), Ok(()));
        assert_eq!(drawn_as(&same_twin_p, &same_twin_rows, command), Ok(()));
    }
    let waiting = one_rows
        .iter()
        .rposition(|r| r.trim_end() == WAITING)
        .expect("the Waiting row");
    let changed = |edit: &dyn Fn(&mut Vec<String>)| {
        let mut rows = one_rows.clone();
        edit(&mut rows);
        let p = aterm_phase::prompt::parse_prompt_v2(&rows).expect("the box");
        drawn_as(&p, &rows, &one)
    };
    for (what, edit) in [
        (
            "no Waiting row",
            &(|r: &mut Vec<String>| r[waiting] = "  \u{23bf}  Running\u{2026}".to_string())
                as &dyn Fn(&mut Vec<String>),
        ),
        ("no blank row", &|r: &mut Vec<String>| {
            r.remove(waiting + 1);
        }),
        ("another tool's row", &|r: &mut Vec<String>| {
            r[lead] = r[lead].replacen("Bash(", "SandboxedBash(", 1);
        }),
        ("its first row cut off", &|r: &mut Vec<String>| {
            r.drain(..=lead);
        }),
    ] {
        assert!(changed(edit).is_err(), "{what}: {:?}", changed(edit));
    }
}

/// THE CALL'S ROW'S TEXT (MEASURED 2026-10-22, 2.1.308, and Claude Code's
/// own `Jhi`): a command of more than two lines shows its first two and `…`
/// (`h-newlines`, and three short lines); one over 160 units its first 160, trimmed, and `…`
/// (`h-long-word`, `h-edge-spaces`); a command of two lines and under 160
/// units shows as it is, its leading blanks kept and no `…` (the
/// short-backticks capture). A command whose lines are all comments may
/// show its comment in its place: refused.
///
/// NEGATIVE CONTROLS: the short command drawn as if cut (trimmed, with `…`)
/// is not the captured row; a comment line followed by a command is drawn
/// (the comment branch needs every other line a comment or blank).
#[test]
fn the_calls_row_shows_two_lines_and_160_units_trimmed() {
    let probe = |name: &str| command_in(CALLS_308, &format!("Probe {name}"));
    assert_eq!(
        header_rows(&probe("h-newlines"), 138).expect("drawn"),
        ["(echo a", "echo b\u{2026})"]
    );
    // Three short lines, under 160 units (`Jhi`'s `c.length > E`, read in
    // the bytes; no too-many box is that short): two shown.
    assert_eq!(
        header_rows("echo a\necho b\necho c", 138).expect("drawn"),
        ["(echo a", "echo b\u{2026})"]
    );
    let long = header_rows(&probe("h-long-word"), 138).expect("drawn");
    assert_eq!(long[0], format!("(echo {}", "b".repeat(132)));
    assert_eq!(long[1], format!("{}\u{2026})", "b".repeat(155 - 132)));
    let edges = probe("h-edge-spaces");
    assert!(edges.starts_with("  echo"));
    assert!(header_rows(&edges, 138).expect("drawn")[0].starts_with("(echo $(true)"));
    let short = "  ".to_string() + &"``".repeat(65) + "\necho x";
    assert_eq!(short, probe("w-short-backticks"));
    let rows = phase_fixtures::screen(SHORT_BACKTICKS);
    let rule = rows
        .iter()
        .rposition(|r| r.starts_with('\u{2500}'))
        .expect("the rule");
    assert_eq!(called_as(&rows[..rule], &short, 138), Ok(()));
    assert_eq!(
        header_rows(&short, 138).expect("drawn"),
        [format!("(  {}", "``".repeat(65)), "echo x)".to_string()]
    );
    assert!(header_rows("# just a note\n\n# and more", 138).is_err_and(|e| e.contains("comment")));
    // NEGATIVE CONTROLS
    let cut = format!("({}\u{2026})", short.trim_start());
    let cut_rows = [cut.split('\n').next().unwrap_or_default().to_string()];
    assert_ne!(header_rows(&short, 138).expect("drawn")[..1], cut_rows[..]);
    assert_eq!(
        header_rows("# a note\necho a", 138).expect("drawn"),
        ["(# a note", "echo a)"]
    );
}

/// TABS GO TO STOPS OF EIGHT CELLS AND THE BLOCKS [`WIDE`] HOLDS TAKE TWO
/// (MEASURED 2026-10-22, 2.1.308, the captures above): a tab after `echo`
/// draws four blanks, after `echo 日` one (日 is two cells), after `日日`
/// four, two tabs from a stop eight and eight; a tab on a later line counts
/// from that line; a wide character with one cell left goes to the next
/// row, so `echo ` and 100 `日` fill 139 cells of 140.
///
/// NEGATIVE CONTROLS: counted in characters, `echo 日` would put the tab's
/// stop two cells on (two blanks); the 68th `日` does not fit on the first
/// row.
#[test]
fn tabs_go_to_stops_of_eight_cells_and_wide_characters_take_two() {
    assert_eq!(expand_tabs("echo\tx"), "echo    x");
    assert_eq!(expand_tabs("echo \u{65e5}\tx"), "echo \u{65e5} x");
    assert_eq!(expand_tabs("\u{65e5}\u{65e5}\tx"), "\u{65e5}\u{65e5}    x");
    assert_eq!(expand_tabs("echo\t\ta"), format!("echo{}a", " ".repeat(12)));
    assert_eq!(
        barred_rows("echo a\n\tb", 140).expect("drawn"),
        ["echo a", "        b"]
    );
    let rows = barred_rows(&format!("echo {}", "\u{65e5}".repeat(100)), 140).expect("drawn");
    assert_eq!(rows[0], format!("echo {}", "\u{65e5}".repeat(67)));
    assert_eq!(width(&rows[0]), 139);
    assert_eq!(rows[1], "\u{65e5}".repeat(33));
    // NEGATIVE CONTROLS
    assert_ne!(expand_tabs("echo \u{65e5}\tx"), "echo \u{65e5}  x");
    assert_eq!(width(&format!("echo {}", "\u{65e5}".repeat(68))), 141);
}

/// THE CALL'S ROW IS COMPARED ROW FOR ROW, EACH WITH ITS LEAD, UNDER ITS
/// `Waiting…` AND A BLANK ROW ([`called_as`]): `echo a` over `echo` and 65
/// `$(true)`, and the same with a newline after the second line's 17th
/// `$(true)` (the box's own wrap there), draw the same box rows and a call's
/// row of three rows each whose first two are alike; only the third tells
/// them apart. On the first's screen (redrawn by this model from
/// `h-one-line`) the first is taken and the second refused at row 3.
///
/// NEGATIVE CONTROLS, on the measured `h-one-line` screen: the blank row
/// under `Waiting…` drawn on (`Waiting…` kept in its place), and the call's
/// second row led by eight blanks, not six, are each refused.
#[test]
fn the_calls_row_is_compared_row_for_row_and_lead_for_lead() {
    let second = format!("echo{}", " $(true)".repeat(65));
    let a = format!("echo a\n{second}");
    let cut = "echo".len() + 17 * " $(true)".len();
    let b = format!("echo a\n{}\n{}", &second[..cut], &second[cut + 1..]);
    assert_eq!(
        barred_rows(&a, 140),
        barred_rows(&b, 140),
        "the same box rows"
    );
    let (head_a, head_b) = (
        header_rows(&a, 138).expect("drawn"),
        header_rows(&b, 138).expect("drawn"),
    );
    assert_eq!((head_a.len(), head_b.len()), (3, 3));
    assert_eq!(head_a[..2], head_b[..2]);
    assert_ne!(head_a[2], head_b[2]);
    let (p, rows) = drawn_screen(&a);
    assert_eq!(drawn_as(&p, &rows, &a), Ok(()));
    assert_eq!(
        drawn_as(&p, &rows, &b),
        Err("the call's row 3 above the box draws otherwise than the command's".to_string())
    );
    // NEGATIVE CONTROLS
    let one = command_in(CALLS_308, "Probe h-one-line");
    let measured = phase_fixtures::screen(capture_308("h-one-line"));
    let waiting = measured
        .iter()
        .position(|r| r.trim_end() == WAITING)
        .expect("the Waiting row");
    let lead = measured
        .iter()
        .position(|r| r.starts_with("\u{23fa} Bash("))
        .expect("the call's row");
    assert_eq!(waiting, lead + 2, "a call's row of two rows");
    let changed = |edit: &dyn Fn(&mut Vec<String>)| {
        let mut rows = measured.clone();
        edit(&mut rows);
        let p = aterm_phase::prompt::parse_prompt_v2(&rows).expect("the box");
        drawn_as(&p, &rows, &one)
    };
    assert_eq!(changed(&|_| {}), Ok(()));
    for (what, edit) in [
        (
            "the blank row drawn on",
            &(|r: &mut Vec<String>| r[waiting + 1] = "  x".to_string())
                as &dyn Fn(&mut Vec<String>),
        ),
        ("the second row led by eight blanks", &|r: &mut Vec<
            String,
        >| {
            r[lead + 1] = format!("  {}", r[lead + 1]);
        }),
    ] {
        assert!(changed(edit).is_err(), "{what}: {:?}", changed(edit));
    }
}

/// THE BLOCKS [`WIDE`] HOLDS END WHERE THEY WERE MEASURED: each block's first
/// and last character take two cells, and the code points just before its
/// first and just after its last are refused (none is printable ASCII or in
/// another block); so are the characters [`WIDE`]'s docs leave out by name:
/// the combining sound marks U+3099 and U+309A, the CJK compatibility
/// ideographs U+F900 and U+FAFF (which `normalize()` rewrites), U+D7B0 of
/// Hangul Jamo Extended-B (narrow) and the ideographic space U+3000.
///
/// NEGATIVE CONTROLS: the blocks' own ends are drawn, in the box and in the
/// call's row.
#[test]
fn the_wide_blocks_end_where_they_were_measured() {
    let refused = |c: char| {
        let command = format!("echo {c}x");
        let named = c.escape_default().to_string();
        for got in [barred_rows(&command, 140), header_rows(&command, 138)] {
            assert!(
                got.as_ref().is_err_and(|e| e.contains(&named)),
                "{named}: {got:?}"
            );
        }
    };
    for (first, last) in WIDE {
        assert_eq!((cells(*first), cells(*last)), (2, 2));
        for outside in [u32::from(*first) - 1, u32::from(*last) + 1] {
            let c = char::from_u32(outside).expect("a character");
            assert!(!c.is_ascii(), "{c:?}");
            refused(c);
        }
        // NEGATIVE CONTROLS
        for end in [*first, *last] {
            let command = format!("echo {end}x");
            assert!(barred_rows(&command, 140).is_ok(), "{end:?}");
            assert!(header_rows(&command, 138).is_ok(), "{end:?}");
        }
    }
    for c in [
        '\u{3099}', '\u{309a}', '\u{f900}', '\u{faff}', '\u{d7b0}', '\u{3000}',
    ] {
        refused(c);
    }
}

/// A READING OF THE ROWS PROVES NOTHING WHERE ITS WORDS TURN ON A BLANK THE
/// SCREEN CANNOT SHOW ([`blanks_read_as_drawn`]; MEASURED 2026-10-22 under
/// `/bin/bash` 3.2, the measured note's §B). Each command below draws as its
/// box ([`drawn_as`], redrawn from `h-one-line`), and so does its twin, the
/// text the screen shows typed as a command (the two differ past the call's
/// row's two lines); the classifier reads the twin a read and the command
/// not, since bash runs the command's `tee x` where the twin holds it as
/// here-document text or as a word: `<<-` over a tab-led `EOF`, and over
/// the blanks the tab draws as; a backslash before a blank that ends a line,
/// and one ending it; a backslash before a tab and `#`, and one before the
/// blanks the tab draws as. The screen's newline reading of each box is
/// refused, the form named. Where the screen's reading is the twin (both
/// backslashes) it would otherwise be proven a read; the rows of the
/// here-document lose their leading blanks, so there the screen's own
/// reading ends the body and runs `tee x`.
///
/// NEGATIVE CONTROLS: blanks and a tab that only separate words, a backslash
/// before one blank and a word (`My\ File`, read alike whether the blank is
/// a tab drawn as one), a here-string and the measured lines of `echo` are
/// read as drawn; a backslash at the text's end, one before a tab, one
/// before a blank that ends a line, and an arithmetic `<<` are refused by
/// name (stricter than the shell there).
#[test]
fn a_reading_whose_words_turn_on_a_blank_the_screen_cannot_show_proves_nothing() {
    use crate::supervise::classify::classify_command;
    let pre = ["echo $(true) $(true) $(true) $(true) $(true)"; 13].join("\n");
    let newline_reading = |p: &PromptV2| {
        p.readings()
            .into_iter()
            .find(|r| r.contains('\n'))
            .expect("the newline reading")
    };
    for (what, run, twin, fooled, named) in [
        (
            "<<- and a tab",
            format!("{pre}\ncat <<-'EOF'\n\tEOF\ntee x"),
            format!("{pre}\ncat <<-'EOF'\n        EOF\ntee x"),
            false,
            "it holds a here-document",
        ),
        (
            "a backslash before a blank at a line's end",
            format!("{pre} a\\ \ntee x"),
            format!("{pre} a\\\ntee x"),
            true,
            "a backslash ends a line",
        ),
        (
            "a backslash before a tab and #",
            format!("{pre}\necho ab\\\t#; tee x"),
            format!("{pre}\necho ab\\        #; tee x"),
            true,
            "a backslash stands before blanks that may be a tab",
        ),
    ] {
        assert!(!classify_command(&run).read_only, "{what}");
        assert!(classify_command(&twin).read_only, "{what}");
        let (p, rows) = drawn_screen(&run);
        assert_eq!(drawn_as(&p, &rows, &run), Ok(()), "{what}");
        assert_eq!(drawn_as(&p, &rows, &twin), Ok(()), "{what}: drawn alike");
        let shown = newline_reading(&p);
        assert_eq!(shown == twin, fooled, "{what}: {shown:?}");
        assert_eq!(classify_command(&shown).read_only, fooled, "{what}");
        let got = blanks_read_as_drawn(&shown);
        assert!(
            got.as_ref().is_err_and(|e| e.contains(named)),
            "{what}: {got:?}"
        );
    }
    // NEGATIVE CONTROLS
    for plain in [
        format!("{pre}\necho ab        x"),
        format!("{pre}\necho\tab x"),
        format!("{pre}\ncat My\\ File"),
        format!("{pre}\ncat <<< x"),
        pre.clone(),
    ] {
        let (p, rows) = drawn_screen(&plain);
        assert_eq!(drawn_as(&p, &rows, &plain), Ok(()), "{plain:?}");
        for reading in p.readings() {
            assert_eq!(blanks_read_as_drawn(&reading), Ok(()), "{reading:?}");
        }
    }
    for (odd, named) in [
        ("echo a\\", "a backslash ends a line"),
        ("echo a\\\tb", "may be a tab"),
        ("echo a\\ \necho b", "may be a tab"),
        ("echo $((1<<2))", "here-document"),
    ] {
        let got = blanks_read_as_drawn(odd);
        assert!(
            got.as_ref().is_err_and(|e| e.contains(named)),
            "{odd:?}: {got:?}"
        );
    }
}

/// THE STALE CALL (MEASURED 2026-10-22, 2.1.308, the measured note's
/// breaker-4 section): the transcript as it stood while the box of
/// [`STALE_SCREEN`] (call N, `Probe stale`, h-wrap-same's newline twin) was
/// up. The file refused writes while S's box stood (S: `Probe stale` too,
/// h-wrap-same's one-line command), S was answered Yes and ran, and the
/// writes of S's result and N's call were lost: S is the last row, waiting.
pub(crate) const STALE_PENDING: &str =
    include_str!("fixtures/claude-2.1.308-transcript-stale-pending.jsonl");
/// The rows written after N's box was refused (Esc): N's result (for a call
/// the file never held), the interruption, the turn's end.
const STALE_REJECTED: &str =
    include_str!("fixtures/claude-2.1.308-transcript-stale-rejected.jsonl");
/// N's box, under S's answered row and N's own `Waiting…` row.
pub(crate) const STALE_SCREEN: &str = include_str!("fixtures/claude-2.1.308-box-bash-stale-n.txt");
/// S's call id in [`STALE_PENDING`].
pub(crate) const STALE_S_ID: &str = "toolu_labe02bf4f6eb4c1b97";
/// N's call id, named only by its result in [`STALE_REJECTED`].
const STALE_N_ID: &str = "toolu_lab1f7a88c34ad9d8bf";

/// AN OLDER CALL LEFT WAITING DRAWS AS A NEWER BOX WHOSE OWN CALL THE FILE
/// NEVER GOT (MEASURED 2026-10-22, 2.1.308, the real-Claude lab `b4o1`):
/// the reader names S, the one call waiting, for N's box; S's command and
/// N's both draw as N's box and as the call's row above it (`⏺ Bash(…)`
/// over `Waiting…`: S's answered row and N's draw alike); S is a read and N
/// is not (its second line runs `cccccccccc`), and the screen's newline
/// reading is not a read either. No text on the screen ties the box to one
/// call: the description is the model's (both say `Probe stale`), the row
/// above the box is the newest call's, no call id is drawn. So the
/// transcript's command is one more reading, never the box's alone
/// (`breaker-read-only@v4`).
///
/// NEGATIVE CONTROLS: after Esc the file holds N's result and the
/// interruption, a user row with no result, which ends S: nothing waits;
/// S spent on its own box leaves nothing waiting.
#[test]
fn an_older_call_left_waiting_draws_as_a_newer_box_whose_call_the_file_never_got() {
    use crate::supervise::classify::classify_command;
    let s = command_in(CALLS_308, "Probe h-wrap-same");
    let n = command_in(CALLS_308, "Probe h-wrap-same-twin");
    assert_ne!(s, n);
    assert_eq!(
        pending_bash(STALE_PENDING, &[]),
        Ok(BoxCommand {
            id: STALE_S_ID.to_string(),
            command: s.clone()
        })
    );
    assert!(!STALE_PENDING.contains(STALE_N_ID));
    let (p, rows) = boxed(STALE_SCREEN);
    assert_eq!(p.description, "Probe stale", "{p:?}");
    assert_eq!(drawn_as(&p, &rows, &s), Ok(()));
    assert_eq!(drawn_as(&p, &rows, &n), Ok(()));
    assert!(classify_command(&s).read_only);
    assert!(!classify_command(&n).read_only);
    let readings = p.readings();
    assert!(readings.contains(&s), "{readings:?}");
    let newline = readings
        .iter()
        .find(|r| r.contains('\n'))
        .expect("the newline reading");
    assert!(!classify_command(newline).read_only, "{newline:?}");
    // NEGATIVE CONTROLS
    let after = format!("{STALE_PENDING}{STALE_REJECTED}");
    assert!(after.contains(STALE_N_ID));
    assert!(
        pending_bash(&after, &[]).is_err_and(|e| e.contains("no call waiting")),
        "{:?}",
        pending_bash(&after, &[])
    );
    assert!(
        pending_bash(STALE_PENDING, &[STALE_S_ID.to_string()])
            .is_err_and(|e| e.contains("no call waiting"))
    );
}

/// The transcript at the first sighting of the next box after a kill
/// (MEASURED 2026-10-22, 2.1.308, lab `b4o1`): the box over `Probe orph213`
/// stood when its Claude Code was killed, the harness resumed the session,
/// and the box over `Probe after286` was up; the killed call is the last
/// call, and nothing the resumed session wrote had reached the file.
const ORPHAN_AT_NEXT_BOX: &str =
    include_str!("fixtures/claude-2.1.308-transcript-orphan-at-next-box.jsonl");
/// The resumed session's first row: the killed call's synthetic result,
/// written 38 ms after [`ORPHAN_AT_NEXT_BOX`] was read.
const ORPHAN_INTERRUPTED: &str =
    include_str!("fixtures/claude-2.1.308-transcript-orphan-interrupted.jsonl");
/// The killed call's id.
const ORPHAN_ID: &str = "toolu_lab4132fb62a965895a";

/// A KILLED SESSION'S CALL IS THE ONE WAITING AT THE RESUMED SESSION'S NEXT
/// BOX (MEASURED 2026-10-22, 2.1.308): the resumed Claude Code holds the
/// killed call's synthetic result, the new prompt's rows and the new call in
/// memory and writes them in one batch after the new box is drawn, so at
/// that box's first sighting the reader names the killed call, alone. The
/// stale call needs no fault: a kill and a resume make one. (Its command
/// differs from the new call's, so it would not draw as the new box; and a
/// settle's re-read, after the batch lands, finds another call.)
///
/// NEGATIVE CONTROL: the synthetic result answers the killed call wherever
/// it lands: with it, nothing in these rows waits.
#[test]
fn a_killed_sessions_call_is_the_one_waiting_at_the_resumed_sessions_next_box() {
    let call = pending_bash(ORPHAN_AT_NEXT_BOX, &[]).expect("the killed call");
    assert_eq!(call.id, ORPHAN_ID);
    assert!(call.command.starts_with("echo $(true) "), "{call:?}");
    assert!(!ORPHAN_AT_NEXT_BOX.contains("toolu_lab754e5f6714a7e7fc"));
    // NEGATIVE CONTROL
    assert!(ORPHAN_INTERRUPTED.contains(ORPHAN_ID));
    let answered = format!("{ORPHAN_AT_NEXT_BOX}{ORPHAN_INTERRUPTED}");
    assert!(
        pending_bash(&answered, &[]).is_err_and(|e| e.contains("no call waiting")),
        "{:?}",
        pending_bash(&answered, &[])
    );
}
