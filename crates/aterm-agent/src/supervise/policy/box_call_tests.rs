// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`drew`] over what Claude Code 2.1.300 really wrote while its box stood
//! (MEASURED 2026-10-15 against the real-Claude lab's stub, scratch paths
//! replaced by `/private/tmp/at.F1`): the Edit tool's call waiting on its
//! `Edit file` box, and a Bash `sed -i` drawn as that same box. And
//! [`pending_calls`] over 2.1.308's first turns (MEASURED 2026-10-22), whose
//! call row the file holds before its prompt row.

use std::path::Path;

use aterm_phase::prompt::fixtures as phase_fixtures;

use super::*;

/// The Edit tool's box, captured (2.1.295, row for row 2.1.300's).
const ASK_EDIT: &str = include_str!("fixtures/claude-2.1.295-box-edit-ask-rule.txt");
/// The box a Bash `sed -i` is drawn as under `Bash(sed:*)` (2.1.300).
const ASK_BASH_SED: &str = include_str!("fixtures/claude-2.1.300-box-bash-sed-specifier-ask.txt");
/// The Write tool's box, captured (2.1.295): only the Write tool draws it.
const ASK_WRITE: &str = include_str!("fixtures/claude-2.1.295-box-write-ask-rule.txt");
/// A Read, answered, then the Edit of `notes.txt` waiting on its box.
const EDIT_PENDING: &str =
    include_str!("../fixtures/claude-2.1.300-transcript-box-edit-pending.jsonl");
/// A Bash `sed -i … notes.txt` waiting on its box.
const SED_PENDING: &str =
    include_str!("../fixtures/claude-2.1.300-transcript-box-bash-sed-pending.jsonl");
/// The same two calls under a PreToolUse hook on every tool (`*`, `exit 0`)
/// in manual mode, each transcript as it stood 350 ms after its box was
/// drawn — past the loop's 300 ms settle (MEASURED 2026-10-15 on 2.1.300,
/// the lab's stub; `api-request` rows cut, attachment rows cut to their
/// type). The stub answers at once, so the prompt row lands after the Read's
/// call: the prompt clears the Read, its result clears nothing more.
const EDIT_STAR_HOOK: &str =
    include_str!("../fixtures/claude-2.1.300-transcript-box-edit-star-hook-pending.jsonl");
const SED_STAR_HOOK: &str =
    include_str!("../fixtures/claude-2.1.300-transcript-box-bash-sed-star-hook-pending.jsonl");
/// The Edit's call id in [`EDIT_PENDING`].
const EDIT_ID: &str = "toolu_labe2ce2e002e831eb0";
/// The sed's call id in [`SED_PENDING`].
const SED_ID: &str = "toolu_lab596693729f4c1589";

/// The box in `text`, its rows changed by `edit`.
fn box_of(text: &str, edit: impl Fn(&mut Vec<String>)) -> PromptV2 {
    let mut rows = phase_fixtures::screen(text);
    edit(&mut rows);
    aterm_phase::prompt::parse_prompt_v2(&rows).expect("the box")
}

/// The row of `rows` that is exactly `from`, replaced by `to`.
fn replace_row(rows: &mut [String], from: &str, to: &str) {
    let row = rows.iter_mut().find(|r| *r == from).expect(from);
    *row = to.to_string();
}

fn proved(p: &PromptV2, jsonl: &str) -> Option<BoxCall> {
    drew(p, &pending_calls(jsonl), &[], None)
}

/// An assistant row calling `tool` as `id`, with `input`.
fn call_row(id: &str, tool: &str, input: &str) -> String {
    format!(
        r#"{{"type":"assistant","isSidechain":false,"message":{{"role":"assistant","content":[{{"type":"tool_use","id":"{id}","name":"{tool}","input":{input}}}]}}}}"#
    )
}

/// A user row answering the call `id`.
fn result_row(id: &str) -> String {
    format!(
        r#"{{"type":"user","isSidechain":false,"message":{{"role":"user","content":[{{"tool_use_id":"{id}","type":"tool_result","content":"ok"}}]}}}}"#
    )
}

/// `jsonl` with `rows` appended.
fn with(jsonl: &str, rows: &[String]) -> String {
    let mut out = jsonl.to_string();
    for row in rows {
        out.push_str(row);
        out.push('\n');
    }
    out
}

/// The transcripts read as the calls waiting: the Edit alone (its Read was
/// answered), and the sed, which names no file but its command.
#[test]
fn the_captured_transcripts_name_the_call_each_box_waits_on() {
    assert_eq!(
        pending_calls(EDIT_PENDING),
        [PendingCall {
            id: EDIT_ID.to_string(),
            tool: "Edit".to_string(),
            file: Some("/private/tmp/at.F1/proj/notes.txt".to_string()),
            command: None,
        }]
    );
    let sed = pending_calls(SED_PENDING);
    assert_eq!(
        sed.iter()
            .map(|c| (c.id.as_str(), c.tool.as_str(), c.file.as_deref()))
            .collect::<Vec<_>>(),
        [(SED_ID, "Bash", None)]
    );
    assert!(
        sed[0]
            .command
            .as_deref()
            .is_some_and(|c| c.starts_with("sed -i") && c.contains("notes.txt")),
        "{sed:?}"
    );
}

/// THE OVER-HOLD'S OWN CASE, live: under a `*` hook in manual mode the
/// transcript at the loop's settle proves the Edit tool's box the Edit's —
/// the hook's rows (attachments) neither add nor clear a call — and never
/// proves the sed's box (negative control).
#[test]
fn under_a_star_hook_the_settled_transcript_proves_the_edit_and_not_the_sed() {
    let edit = box_of(ASK_EDIT, |_| {});
    let sed = box_of(ASK_BASH_SED, |_| {});
    let proved_edit = proved(&edit, EDIT_STAR_HOOK).expect("the Edit proved");
    assert_eq!(proved_edit.tool, "Edit");
    assert!(!lagging(&edit, &pending_calls(EDIT_STAR_HOOK), &[]));
    assert_eq!(proved(&sed, SED_STAR_HOOK), None);
    assert_eq!(proved(&edit, SED_STAR_HOOK), None);
    assert!(
        !lagging(&sed, &pending_calls(SED_STAR_HOOK), &[]),
        "a waiting Bash call ends the re-reads"
    );
}

/// The Edit tool's own box, its call waiting in the main chain: proved the
/// Edit's, by that call — also with the tail's first line torn, a hook's
/// attachment row after it, or an earlier Edit of the same file whose box
/// has left still waiting on its result row (spent, passed over), and for a
/// box path of two components the call's path ends in.
#[test]
fn an_edit_box_whose_call_the_transcript_names_is_proved_the_edits() {
    let edit = box_of(ASK_EDIT, |_| {});
    let want = Some(BoxCall {
        tool: "Edit".to_string(),
        id: EDIT_ID.to_string(),
    });
    assert_eq!(proved(&edit, EDIT_PENDING), want);
    let torn = &EDIT_PENDING[EDIT_PENDING.find("ssionId").expect("a row")..];
    assert_eq!(proved(&edit, torn), want, "a torn first line");
    let hook = r#"{"type":"attachment","isSidechain":false,"attachment":{"type":"hook_success","hookEvent":"PreToolUse"}}"#;
    assert_eq!(
        proved(&edit, &with(EDIT_PENDING, &[hook.to_string()])),
        want,
        "a hook's attachment row neither adds nor clears a call"
    );
    // A second Edit of the file after the captured one, whose result row
    // has not landed: both wait, and the captured one's box has left.
    let next = call_row(
        "toolu_next",
        "Edit",
        r#"{"file_path":"/private/tmp/at.F1/proj/notes.txt"}"#,
    );
    let pending = pending_calls(&with(EDIT_PENDING, &[next]));
    assert_eq!(pending.len(), 2, "{pending:?}");
    assert_eq!(
        drew(&edit, &pending, &[EDIT_ID.to_string()], None),
        Some(BoxCall {
            tool: "Edit".to_string(),
            id: "toolu_next".to_string(),
        }),
        "the spent call is passed over"
    );
    let deeper = box_of(ASK_EDIT, |rows| {
        replace_row(rows, " notes.txt", " proj/notes.txt");
    });
    assert_eq!(proved(&deeper, EDIT_PENDING), want, "proj/notes.txt");
}

/// NEGATIVE CONTROLS: nothing is proved for the sed's box (a Bash call names
/// no file); for the Edit's box with its call not written yet (the measured
/// lag), answered, beside a pending Bash or Read call, naming another file,
/// a subagent's, or after a prompt row; for a box whose path the call's does
/// not end in (`sub/notes.txt` for `…/proj/notes.txt`); for a box that may be a subagent's (an origin in its title, the
/// background agents' note in its footer, a footer cut short); for a box
/// only one tool draws; and for the one spent call alone.
#[test]
fn a_box_the_transcript_does_not_prove_is_not_proved() {
    let edit = box_of(ASK_EDIT, |_| {});
    let sed = box_of(ASK_BASH_SED, |_| {});
    assert_eq!(proved(&sed, SED_PENDING), None, "the sed's own box");
    assert_eq!(proved(&edit, SED_PENDING), None, "the Edit box over a sed");
    let cut = &EDIT_PENDING[..EDIT_PENDING.find(EDIT_ID).expect("the Edit")];
    let cut = &cut[..cut.rfind('\n').expect("a row") + 1];
    assert!(pending_calls(cut).is_empty(), "{:?}", pending_calls(cut));
    assert_eq!(proved(&edit, cut), None, "the call not written yet");
    let edit_file = r#"{"file_path":"/private/tmp/at.F1/proj/notes.txt"}"#;
    for (name, rows) in [
        ("answered", vec![result_row(EDIT_ID)]),
        (
            "a Bash call waiting too",
            vec![call_row(
                "toolu_b",
                "Bash",
                r#"{"command":"sed -i '' 's/a/b/' notes.txt"}"#,
            )],
        ),
        (
            "a Read waiting too",
            vec![call_row("toolu_r", "Read", edit_file)],
        ),
        (
            "a prompt after it",
            vec![
                r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":"go on"}}"#
                    .to_string(),
            ],
        ),
    ] {
        assert_eq!(proved(&edit, &with(EDIT_PENDING, &rows)), None, "{name}");
    }
    for (name, from, to) in [
        ("another file", "proj/notes.txt", "proj/other.txt"),
        (
            "a subagent's call",
            r#""isSidechain":false,"message":{"id":"msg_labfc941cb1337b61ef""#,
            r#""isSidechain":true,"message":{"id":"msg_labfc941cb1337b61ef""#,
        ),
    ] {
        assert!(EDIT_PENDING.contains(from), "{name}");
        // Only the Edit's own row changes.
        let at = EDIT_PENDING.find(EDIT_ID).expect("the Edit");
        let start = EDIT_PENDING[..at].rfind('\n').expect("a row") + 1;
        let (head, tail) = EDIT_PENDING.split_at(start);
        assert!(tail.contains(from), "{name}");
        let changed = format!("{head}{}", tail.replacen(from, to, 1));
        assert_eq!(proved(&edit, &changed), None, "{name}");
    }
    let sub_path = box_of(ASK_EDIT, |rows| {
        replace_row(rows, " notes.txt", " sub/notes.txt");
    });
    assert_eq!(proved(&sub_path, EDIT_PENDING), None, "sub/notes.txt");
    for (name, from, to) in [
        (
            "an origin",
            " Edit file",
            " Edit file · from a subagent".to_string(),
        ),
        (
            "the background agents' note",
            " Esc to cancel · Tab to amend",
            format!(
                " Esc to cancel · Tab to amend · ctrl+x ctrl+k {}",
                aterm_phase::anchor_text("hint.agents_twice")
            ),
        ),
        (
            "a footer cut short",
            " Esc to cancel · Tab to amend",
            " Esc to cancel · Tab to amend · ctrl+x ct…".to_string(),
        ),
    ] {
        let p = box_of(ASK_EDIT, |rows| replace_row(rows, from, &to));
        assert!(!box_may_prove(&p), "{name}: {p:?}");
        assert_eq!(proved(&p, EDIT_PENDING), None, "{name}");
    }
    assert!(box_may_prove(&edit));
    let write = box_of(ASK_WRITE, |_| {});
    assert!(!box_may_prove(&write), "a box one tool draws: {write:?}");
    assert_eq!(
        drew(
            &edit,
            &pending_calls(EDIT_PENDING),
            &[EDIT_ID.to_string()],
            None
        ),
        None,
        "its one call spent"
    );
}

/// The re-read rule: the transcript lags while no call waiting (less the
/// spent) is of a tool the box may be — nothing, a Read whose result row
/// has not landed, a spent Edit — and never once a Bash call waits (a sed is
/// never provable) or the box's own call does.
#[test]
fn the_transcript_lags_until_a_call_the_box_may_be_waits() {
    let edit = box_of(ASK_EDIT, |_| {});
    let read = call_row(
        "toolu_r",
        "Read",
        r#"{"file_path":"/private/tmp/at.F1/proj/notes.txt"}"#,
    );
    let bash = call_row("toolu_b", "Bash", r#"{"command":"sed -i '' 's/a/b/' x"}"#);
    let lags = |jsonl: &str, spent: &[String]| lagging(&edit, &pending_calls(jsonl), spent);
    assert!(lags("", &[]), "nothing");
    assert!(lags(&read, &[]), "a Read only");
    assert!(lags(EDIT_PENDING, &[EDIT_ID.to_string()]), "the Edit spent");
    assert!(!lags(EDIT_PENDING, &[]), "the Edit");
    assert!(!lags(&bash, &[]), "a Bash call");
    assert!(!lags(SED_PENDING, &[]), "the sed");
}

/// A box's path names a call's file where its components end the call's:
/// `./` dropped, `~/` the home (none: nothing), an absolute path the whole
/// file, `..` nothing.
#[test]
fn a_box_path_names_the_file_whose_components_it_ends() {
    let home = Some(Path::new("/Users/_owner"));
    for (shown, file, home, named) in [
        ("notes.txt", "/p/notes.txt", None, true),
        ("./notes.txt", "/p/notes.txt", None, true),
        ("p/notes.txt", "/p/notes.txt", None, true),
        ("q/notes.txt", "/p/notes.txt", None, false),
        ("otes.txt", "/p/notes.txt", None, false),
        ("~/p/notes.txt", "/Users/_owner/p/notes.txt", home, true),
        ("~/p/notes.txt", "/Users/_owner/p/notes.txt", None, false),
        ("~/p/notes.txt", "/Users//other/p/notes.txt", home, false),
        ("/p/notes.txt", "/p/notes.txt", None, true),
        ("/notes.txt", "/p/notes.txt", None, false),
        ("../p/notes.txt", "/p/notes.txt", None, false),
        ("", "/p/notes.txt", None, false),
    ] {
        assert_eq!(names(shown, file, home), named, "{shown} vs {file}");
    }
}

/// A fresh session's first turn as 2.1.308 wrote it (the real-Claude lab
/// `b3p1`, 2026-10-22): the Edit call of notes.txt waiting on its box, its
/// assistant row written BEFORE the prompt row `toolstart @notes.txt` and
/// the attachment rows its `parentUuid` chain runs up through.
const FIRST_EDIT: &str =
    include_str!("../fixtures/claude-2.1.308-transcript-first-turn-edit-pending.jsonl");
/// The same for a Write call of a new file, under the prompt `toolstart`.
const FIRST_WRITE: &str =
    include_str!("../fixtures/claude-2.1.308-transcript-first-turn-write-pending.jsonl");
/// [`FIRST_EDIT`]'s box, read while it stood.
const FIRST_EDIT_BOX: &str = include_str!("fixtures/claude-2.1.308-box-edit-first-turn.txt");

/// The value of `key` on the row of `jsonl` holding `marker`.
fn field_of(jsonl: &str, marker: &str, key: &str) -> String {
    let row = jsonl
        .lines()
        .find(|l| l.contains(marker))
        .unwrap_or_else(|| panic!("the row with {marker}"));
    aterm_json::from_str::<Value>(row)
        .expect("a row")
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{key}"))
        .to_string()
}

/// A FIRST TURN'S CALL WRITTEN BEFORE ITS OWN PROMPT ROW STILL WAITS
/// (MEASURED 2026-10-22, 2.1.308): in [`FIRST_EDIT`] and [`FIRST_WRITE`]
/// the prompt row follows the call's row in the file, but it is the call's
/// own ancestor, so the call is the one waiting, naming its file. Read in
/// the file's order (the reader before breaker-3, the measured note's
/// mutant M8) both files name no call. The Edit's box, read while it
/// stood, takes the call as its candidate ([`drew`]) and the transcript
/// does not lag ([`lagging`]). No decision reads that candidate today (the
/// module docs): this pins the reader both lanes share.
///
/// NEGATIVE CONTROLS: a later prompt (a constructed row whose parent is the
/// call's row) ends the call; the call row's chain cut (its `parentUuid` a
/// uuid the tail does not hold) cannot show the prompt is its own, and the
/// prompt written after it ends it; the call's result, written anywhere,
/// answers it.
#[test]
fn a_first_turns_call_written_before_its_prompt_row_still_waits() {
    for (jsonl, tool, marker, file) in [
        (
            FIRST_EDIT,
            "Edit",
            "\"name\":\"Edit\"",
            "/private/tmp/at.gRGO/proj/notes.txt",
        ),
        (
            FIRST_WRITE,
            "Write",
            "\"name\":\"Write\"",
            "/private/tmp/at.gRGO/proj/new-4.txt",
        ),
    ] {
        let rows: Vec<&str> = jsonl.lines().collect();
        let call_at = rows
            .iter()
            .position(|l| l.contains(marker))
            .expect("the call row");
        let prompt_at = rows
            .iter()
            .position(|l| l.contains("\"type\":\"user\""))
            .expect("the prompt row");
        assert!(
            prompt_at > call_at,
            "{tool}: the prompt row follows the call"
        );
        let id = field_of(jsonl, marker, "uuid");
        let pending = pending_calls(jsonl);
        assert_eq!(
            pending
                .iter()
                .map(|c| (c.tool.as_str(), c.file.as_deref(), c.command.as_deref()))
                .collect::<Vec<_>>(),
            [(tool, Some(file), None)],
            "{tool}"
        );
        let call_id = pending[0].id.clone();
        // NEGATIVE CONTROLS
        let later = format!(
            r#"{{"parentUuid":"{id}","isSidechain":false,"type":"user","message":{{"role":"user","content":"and then"}},"uuid":"00000000-0000-4000-8000-00000000p003"}}"#
        );
        assert!(pending_calls(&with(jsonl, &[later])).is_empty(), "{tool}");
        let parent = field_of(jsonl, marker, "parentUuid");
        let cut = jsonl.replacen(
            &format!("\"parentUuid\":\"{parent}\""),
            "\"parentUuid\":\"00000000-0000-4000-8000-0000000c0002\"",
            1,
        );
        assert_ne!(cut, jsonl);
        assert!(
            pending_calls(&cut).is_empty(),
            "{tool}: {:?}",
            pending_calls(&cut)
        );
        let answered = format!("{}\n{jsonl}", result_row(&call_id));
        assert!(pending_calls(&answered).is_empty(), "{tool}");
    }
    let edit = box_of(FIRST_EDIT_BOX, |_| {});
    let pending = pending_calls(FIRST_EDIT);
    assert!(!lagging(&edit, &pending, &[]));
    assert_eq!(
        drew(&edit, &pending, &[], None).map(|c| c.tool),
        Some("Edit".to_string())
    );
}

/// A CALL ROW WITH NO `uuid` IS ENDED BY ANY LATER PROMPT ([`pending_calls`]):
/// no `parentUuid` chain can show a later prompt is its own, so a user row
/// with no result written after it ends it, the prompt with a `uuid` of its
/// own or without one; and it ends no call written after it.
///
/// NEGATIVE CONTROLS: with no prompt after it, or the prompt written before
/// it, the call waits.
#[test]
fn a_call_row_with_no_uuid_is_ended_by_a_later_prompt() {
    let call = r#"{"isSidechain":false,"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_lab00000000nouuid","name":"Edit","input":{"file_path":"/p/notes.txt","old_string":"a","new_string":"b"}}]}}"#.to_string();
    let prompts = [
        r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"and then"},"uuid":"00000000-0000-4000-8000-00000000p009"}"#.to_string(),
        r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"and then"}}"#.to_string(),
    ];
    for prompt in &prompts {
        assert!(
            pending_calls(&with("", &[call.clone(), prompt.clone()])).is_empty(),
            "{prompt}"
        );
    }
    // NEGATIVE CONTROLS
    let waiting = |jsonl: String| {
        pending_calls(&jsonl)
            .into_iter()
            .map(|c| (c.id, c.tool, c.file))
            .collect::<Vec<_>>()
    };
    let edit = (
        "toolu_lab00000000nouuid".to_string(),
        "Edit".to_string(),
        Some("/p/notes.txt".to_string()),
    );
    assert_eq!(
        waiting(with("", std::slice::from_ref(&call))),
        std::slice::from_ref(&edit)
    );
    for prompt in &prompts {
        assert_eq!(
            waiting(with("", &[prompt.clone(), call.clone()])),
            std::slice::from_ref(&edit),
            "{prompt}"
        );
    }
}
