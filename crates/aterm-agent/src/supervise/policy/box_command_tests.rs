// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`barred_rows`], [`drawn_as`] and [`pending_bash`] over what Claude Code
//! 2.1.307 really drew and wrote (MEASURED 2026-10-22 in the real-Claude
//! lab, `docs/measured/breaker-transcript-and-zsh-2026-10-22.md`): each box
//! capture `claude-2.1.307-box-bash-*` beside the call row the lab's stub
//! made it for, and the transcript as it stood while a box was up.

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

/// Each capture's name and screen: `Probe <name>`'s box.
const SCREENS: &[(&str, &str)] = &[
    (
        "too-many-newline-twin",
        include_str!("fixtures/claude-2.1.307-box-bash-too-many-newline-twin.txt"),
    ),
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
/// The tabs capture: drawn, but by a rule this check does not model.
const TABS: &str = include_str!("fixtures/claude-2.1.307-box-bash-wrap-tabs.txt");

/// The command of the call whose description is `description`.
pub(crate) fn command_of(description: &str) -> String {
    CALLS
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
/// with a blank), each matched by the command the stub made it for. And the
/// measured too-many box (`echo` and 65 `$(true)` as one line, captured
/// 2026-10-21) and its newline twin (the same text with a newline at each
/// wrap, captured 2026-10-22) draw the SAME rows: both commands match both
/// screens, which is why the screen alone proves neither.
///
/// NEGATIVE CONTROLS: every command against every other capture's box fails;
/// the measured box against its command drawn a cell wider or narrower; the
/// tabs capture, whose command holds a tab, is refused by name; a command with
/// a wide character, a `\r` or an escape is refused.
#[test]
fn every_measured_capture_draws_as_its_own_calls_command() {
    for (name, text) in SCREENS {
        let (p, rows) = boxed(text);
        let command = command_of(&format!("Probe {name}"));
        assert_eq!(drawn_as(&p, &rows, &command), Ok(()), "{name}");
        for (other, _) in SCREENS.iter().filter(|(o, _)| o != name) {
            let theirs = command_of(&format!("Probe {other}"));
            let twins = [*name, *other]
                .iter()
                .all(|n| *n == "too-many-newline-twin" || *n == "wrap-100-columns");
            if !twins {
                assert!(drawn_as(&p, &rows, &theirs).is_err(), "{name} by {other}");
            }
        }
    }
    let space = command_of("Echo 65 empty substitutions");
    let twin = command_of("Probe too-many-newline-twin");
    assert_ne!(space, twin);
    assert_eq!(space.replace('\n', " "), twin.replace('\n', " "));
    for text in [phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307, SCREENS[0].1] {
        let (p, rows) = boxed(text);
        assert_eq!(drawn_as(&p, &rows, &space), Ok(()));
        assert_eq!(drawn_as(&p, &rows, &twin), Ok(()));
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
    let (p, rows) = boxed(phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307);
    let (p_tabs, rows_tabs) = boxed(TABS);
    let tabs = command_of("Probe wrap-tabs");
    assert!(
        drawn_as(&p_tabs, &rows_tabs, &tabs)
            .is_err_and(|e| e.contains("`\\t`, which this check does not draw again")),
        "{:?}",
        drawn_as(&p_tabs, &rows_tabs, &tabs)
    );
    for odd in ["echo \u{65e5}", "echo a\r\nls", "echo \u{1b}[31m"] {
        assert!(barred_rows(odd, 140).is_err(), "{odd:?}");
        assert!(drawn_as(&p, &rows, odd).is_err(), "{odd:?}");
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
/// though every row it draws matches one of the box's. The measured box's
/// newline twin draws its four rows (control); the twin less its last line
/// draws the first three exactly, and the twin with one more line draws the
/// four and a fifth.
///
/// NEGATIVE CONTROLS: both are refused, naming the counts.
#[test]
fn a_command_drawing_fewer_or_more_rows_than_the_box_is_not_the_box() {
    let (p, rows) = boxed(phase_fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307);
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
