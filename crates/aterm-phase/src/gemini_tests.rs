// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::*;
use super::*;
use crate::prompt::fixtures::{provenance, rows, screen};
use crate::reader::{
    AgyReader, ClaudeReader, CodexReader, MuseReader, Program, ScreenReader, identify, read,
};

fn gemini(text: &str) -> crate::reader::Reading {
    read(Some("gemini"), &screen(text), None)
}

fn roles(p: &PromptV2) -> Vec<Role> {
    p.options.iter().map(|o| o.role).collect()
}

fn labels(p: &PromptV2) -> Vec<&str> {
    p.options.iter().map(|o| o.label.as_str()).collect()
}

/// `rows` with the last row that starts with `head` replaced by `with`.
fn edited(rows: &[String], head: &str, with: &str) -> Vec<String> {
    let at = rows
        .iter()
        .rposition(|r| r.starts_with(head))
        .unwrap_or_else(|| panic!("a row starting {head:?}"));
    let mut out = rows.to_vec();
    out[at] = with.to_string();
    out
}

/// The row of the composer's band that holds its caret: the last ` > ` row.
fn caret_at(rows: &[String]) -> usize {
    rows.iter()
        .rposition(|r| r.starts_with(" > "))
        .expect("a caret row")
}

/// The status row: the one right above the live zone's rule.
fn status_at(rows: &[String]) -> usize {
    rows.iter()
        .rposition(|r| plain_rule_width(r).is_some())
        .expect("the rule")
        - 1
}

/// Every Gemini CLI capture under `src/fixtures/`, by file name.
fn captures() -> Vec<(String, String)> {
    crate::text::fixture_files()
        .into_iter()
        .filter(|(name, _)| name.starts_with("gemini-"))
        .collect()
}

/// A fresh session at its composer is idle as evidence: no message yet, the
/// status row's `? for shortcuts`, and the composer EMPTY — its placeholder
/// read as no text at all, since neither its cursor (column 3) nor its colour
/// (grey, not dim) tells it from a draft.
#[test]
fn a_fresh_session_is_idle_at_its_composer_and_its_placeholder_is_no_draft() {
    let r = gemini(IDLE);
    let rows = screen(IDLE);
    assert_eq!(r.program, Program::Gemini);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(r.fresh);
    assert_eq!((r.prompt, r.wall, r.said_tail), (None, None, None));
    assert!(!r.interrupted);
    let caret = caret_at(&rows);
    assert_eq!(r.composer, Some((caret, vec![String::new()])));
    assert!(
        rows[caret].contains(anchor_text("gemini.composer.placeholder")),
        "{}",
        rows[caret]
    );
    let busy = gemini(BUSY_THINKING);
    assert_eq!(busy.phase, Phase::Busy, "the control");
    assert_eq!(busy.composer.map(|(_, l)| l), Some(vec![String::new()]));
    assert!(!busy.fresh, "a message was sent");

    let typed = edited(&rows, " > ", " > Type your message or @path/to/file");
    assert_eq!(
        composer_draft(&typed).map(|(_, l)| l),
        Some(vec!["Type your message or @path/to/file".to_string()])
    );
    assert_eq!(
        phase(&typed),
        (Phase::Idle, false),
        "a draft under the hint"
    );
}

/// A typed draft is the composer's text, and the status row is BLANK while
/// it is held: idle as evidence, the session still fresh.
#[test]
fn an_unsent_draft_is_the_composers() {
    let r = gemini(DRAFT);
    let rows = screen(DRAFT);
    assert_eq!(
        r.composer,
        Some((
            caret_at(&rows),
            vec!["a draft the lab never sends".to_string()]
        ))
    );
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert!(r.fresh);
    let mut tipped = rows.clone();
    tipped[status_at(&rows)] = format!("{:>143}", "Tip: a tip this grammar never read");
    assert_eq!(phase(&tipped), (Phase::Idle, false), "the control");
}

/// A draft that wrapped: its rows under the caret row, at column 3, are
/// lines of the draft.
#[test]
fn a_wrapped_draft_is_every_row_of_the_band() {
    let rows = screen(DRAFT);
    let caret = caret_at(&rows);
    let mut wrapped = rows.clone();
    wrapped.insert(caret + 1, "   and a second row".to_string());
    assert_eq!(
        composer_draft(&wrapped).map(|(_, l)| l),
        Some(vec![
            "a draft the lab never sends".to_string(),
            "and a second row".to_string()
        ])
    );
    assert_eq!(phase(&wrapped), (Phase::Idle, true));
    wrapped[caret + 1] = " not the band".to_string();
    assert_eq!(composer_draft(&wrapped), None, "the control");
}

/// THE LAB'S HAZARD: every message a person sent is drawn in the composer's
/// own band, so with a box up (the live zone gone) the last band on the
/// screen is the message the box answers.
#[test]
fn a_sent_message_is_never_the_composer() {
    let boxed = screen(BOX_SHELL);
    assert!(boxed.iter().any(|r| r == " > toolstart"), "the sent band");
    assert_eq!(composer_draft(&boxed), None);
    assert_eq!(gemini(BOX_SHELL).composer, None);
    let band_end = boxed
        .iter()
        .rposition(|r| r.starts_with('▀'))
        .expect("the sent band's bottom");
    let transcript = boxed[..=band_end].to_vec();
    assert_eq!(composer_draft(&transcript), None);
    assert_eq!(phase(&transcript), (Phase::Idle, false));
    let idle = screen(IDLE);
    assert!(composer_draft(&idle).is_some(), "the control");
    let rule = status_at(&idle) + 1;
    let mut far = idle.clone();
    far.splice(
        rule + 1..rule + 1,
        [" one", " two", " three", " four"].map(String::from),
    );
    assert_eq!(
        composer_draft(&far),
        None,
        "five rows between the rule and the band"
    );
}

/// THE SPINNER ON THE STATUS ROW IS BUSY — thinking, streaming its reply,
/// and through every retry of a 503 and a 500, which draw no row of their
/// own (the CLI retries them for ever): no wall, no last words.
#[test]
fn the_spinner_is_busy_through_thinking_streaming_and_retries() {
    for text in [BUSY_THINKING, BUSY_STREAMING, RETRY_503, RETRY_500] {
        let r = gemini(text);
        let what = provenance(text).unwrap_or_default();
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Busy, true),
            "{what}"
        );
        assert_eq!((r.said_tail, r.wall), (None, None), "{what}");
        let rows = screen(text);
        let status = status_at(&rows);
        assert!(is_spinner_row(&rows[status]), "{what}");
        assert!(
            rows[status].contains(anchor_text("gemini.status.cancel")),
            "{what}"
        );
        let mut gone = rows.clone();
        gone[status] = " Thinking... (esc to cancel, 3s)".to_string();
        assert_eq!(phase(&gone), (Phase::Idle, false), "{what}");
        gone[status] = format!("{:>143}", "? for shortcuts");
        assert_eq!(phase(&gone), (Phase::Idle, true), "{what}: the control");
        gone[status] = String::new();
        assert_eq!(phase(&gone), (Phase::Idle, true), "{what}: no hint");
    }
}

/// A turn ends on its reply, with no row marking the end: the spinner gone
/// and `? for shortcuts` back.
#[test]
fn a_turn_ends_on_its_reply() {
    let r = gemini(END_OF_TURN);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    let said = r.said_tail.expect("the reply");
    assert!(said.starts_with("tick 16 tick 17"), "{said}");
    assert!(said.ends_with("tick 30"), "{said}");
    assert!(!r.fresh && !r.interrupted);
    for text in [RETRY_503_RECOVERED, RETRY_500_RECOVERED] {
        let r = gemini(text);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
        assert_eq!(
            r.said_tail.as_deref(),
            Some("Hi! I am the local lab stub, not a model. (reply 3)")
        );
        assert_eq!(r.wall, None);
    }
    let rows = screen(END_OF_TURN);
    let at = rows
        .iter()
        .position(|r| r.starts_with("✦ tick 16"))
        .expect("the reply");
    let mut asks = rows.clone();
    asks.insert(at + 1, "  Shall I go on?".to_string());
    assert_eq!(phase(&asks), (Phase::Question, true));
    assert_eq!(
        said_tail(&asks)
            .as_deref()
            .map(|s| s.ends_with("Shall I go on?")),
        Some(true)
    );
    let mut tooled = rows.clone();
    tooled.insert(at + 1, "  ✓  Shell  ls → done".to_string());
    assert_eq!(said_tail(&tooled), None);
}

/// Esc in a turn ends it on `ℹ Request cancelled.`: idle as evidence, and
/// the PERSON's — the next message is theirs; no last words, no wall.
#[test]
fn an_interrupted_turn_is_the_persons() {
    let r = gemini(INTERRUPTED);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert!(r.interrupted);
    assert_eq!((r.said_tail, r.wall), (None, None));
    assert!(!gemini(END_OF_TURN).interrupted, "the control");

    let rows = screen(INTERRUPTED);
    let at = rows
        .iter()
        .position(|r| r == "ℹ Request cancelled.")
        .expect("the cancel row");
    let band = |text: &str| {
        [
            "▄".repeat(144),
            format!(" > {text}"),
            "▀".repeat(144),
            "✦ Done again.".to_string(),
        ]
    };
    let mut answered = rows.clone();
    answered.splice(at + 1..at + 1, band("again, shorter"));
    assert!(!interrupted(&answered), "a newer message answered it");
    assert_eq!(said_tail(&answered).as_deref(), Some("Done again."));
    let quoted = edited(&rows, "ℹ Request cancelled.", "✦ Request cancelled.");
    assert!(!interrupted(&quoted), "the words in a reply");
}

/// A NARROW PANE (measured at 72 columns, a `GEMINI.md` loaded): the context
/// summary stands on a row of its own under a blank, four rows between the
/// rule and the band, and the status row never says `? for shortcuts` — so
/// its rest is a blank status row over an empty composer: idle as evidence,
/// and its turn's end too, with its wrapped reply; its draft is idle with
/// its text, its spinner busy, and every screen is Gemini CLI's by its
/// layout.
#[test]
fn a_narrow_pane_is_read_without_its_shortcuts_hint() {
    for (what, text) in [("narrow", NARROW_IDLE), ("wide", IDLE_CONTEXT)] {
        let r = gemini(text);
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        let (caret, lines) = r.composer.expect("the composer");
        assert_eq!(lines, [""], "{what}: the placeholder is no draft");
        assert!(screen(text)[caret].starts_with(" > "), "{what}");
        assert_eq!(
            identify(Some("node"), &screen(text)).program(),
            Program::Gemini,
            "{what}"
        );
        assert!(r.said_tail.is_none() && r.prompt.is_none(), "{what}");
    }
    let draft = gemini(NARROW_DRAFT);
    assert_eq!(
        (draft.phase, draft.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(
        draft.composer.map(|(_, l)| l),
        Some(vec!["count in a narrow pane".to_string()])
    );
    let busy = gemini(NARROW_BUSY);
    assert_eq!((busy.phase, busy.phase_authoritative), (Phase::Busy, true));
    let ended = gemini(NARROW_END_OF_TURN);
    assert_eq!(
        (ended.phase, ended.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(
        ended
            .said_tail
            .as_deref()
            .is_some_and(|t| t.starts_with("tick 16") && t.ends_with("tick 30")),
        "{:?}",
        ended.said_tail
    );
    for text in [NARROW_IDLE, NARROW_DRAFT, NARROW_BUSY, NARROW_END_OF_TURN] {
        assert_eq!(
            identify(Some("node"), &screen(text)).program(),
            Program::Gemini,
            "{:?}",
            provenance(text)
        );
    }

    for text in [IDLE, NARROW_IDLE] {
        let rows = screen(text);
        let at = status_at(&rows);
        let mut tipped = rows.clone();
        tipped[at] = " Tip: a tip this grammar never read".to_string();
        assert_eq!(
            phase(&tipped),
            (Phase::Idle, false),
            "{:?}",
            provenance(text)
        );
    }

    let narrow = screen(NARROW_IDLE);
    let rule = narrow
        .iter()
        .rposition(|r| plain_rule_width(r).is_some())
        .expect("the rule");
    let mut banded = narrow.clone();
    banded[rule + 2] = "▀".repeat(72);
    assert!(
        composer_draft(&banded).is_none(),
        "a band row under the rule"
    );
    let mut far = narrow.clone();
    far.splice(rule + 1..rule + 1, [String::new(), String::new()]);
    assert!(composer_draft(&far).is_none(), "five rows between");
    let mut wrapped = narrow.clone();
    wrapped.insert(rule + 1, String::new());
    assert!(composer_draft(&wrapped).is_some(), "four rows between");
}

/// A box the PERSON refused with Esc (`No, suggest changes (esc)`, measured
/// under a supervised lab) ends the turn as Esc in a turn does: the tool's
/// row marked `-` in its own small box, then `ℹ Request cancelled.` — idle as
/// evidence, the person's, no last words and no wall, and no box up.
#[test]
fn a_box_refused_with_esc_is_the_persons_stop() {
    let r = gemini(BOX_DENIED);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert!(r.interrupted);
    assert_eq!((r.said_tail, r.wall, r.prompt), (None, None, None));
    assert_eq!(
        identify(Some("node"), &screen(BOX_DENIED)).program(),
        Program::Gemini
    );
    let rows = screen(BOX_DENIED);
    let last = rows
        .iter()
        .rposition(|r| r == "ℹ Request cancelled.")
        .expect("the cancel row");
    let mut uncancelled = rows.clone();
    uncancelled[last] = String::new();
    assert!(!interrupted(&uncancelled), "the control");
    assert_eq!(gemini(BOX_SHELL).phase, Phase::Prompt, "the box before it");
}

/// `/` opens the command menu under the composer's band: the composer is
/// still the composer, its draft the `/`, and no box is up, but nothing is
/// evidence — the menu stands where the footer did.
#[test]
fn the_command_menu_is_no_box_and_no_evidence() {
    let r = gemini(SLASH_MENU);
    let rows = screen(SLASH_MENU);
    assert_eq!(r.composer, Some((caret_at(&rows), vec!["/".to_string()])));
    assert_eq!(r.prompt, None);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
    let bottom = caret_at(&rows) + 1;
    let footer = rows
        .iter()
        .position(|r| r.contains(anchor_text("gemini.footer.workspace")))
        .expect("the footer");
    let mut closed = rows[..=bottom].to_vec();
    closed.extend_from_slice(&rows[footer..]);
    assert_eq!(phase(&closed), (Phase::Idle, true), "the control");
}

/// Every box's span runs from its TITLE row to its last row, as every
/// reader's does ([`PromptV2::span`]): the row a press or an answer is
/// fenced on where no row of its own is named — the tool's row, the trust
/// dialog's question, the question dialog's and the usage limit's title —
/// never the round top border every box of that width shares.
#[test]
fn every_box_span_opens_on_its_title_row() {
    for (fixture, title) in [
        (BOX_SHELL, "│ ? Shell  touch lab-probe.txt"),
        (
            BOX_SHELL_REDIRECTION,
            "│ ? Shell  env | cut -d= -f1 | sort > envnames.txt",
        ),
        (BOX_WRITE, "│ ? WriteFile  Writing to lab-write.txt"),
        (BOX_EDIT, "│ ? Edit  lab-write.txt: written by the lab stub"),
        (QUESTION, "│ Answer Questions"),
        (LIMIT_DIALOG, "│ Usage limit reached for gemini-3.5-flash."),
        (TRUST, "│ Do you trust the files in this folder?"),
    ] {
        let rows = screen(fixture);
        let p = prompt(&rows).expect("the box");
        let (top, bottom) = p.span;
        assert!(rows[top].trim_start().starts_with(title), "{:?}", rows[top]);
        assert_eq!(bottom, rows.len() - 1, "{title}");
        let border = rows[..top]
            .iter()
            .rposition(|r| r.trim_start().starts_with('╭'))
            .expect("the top border");
        assert!(
            rows[border + 1..top]
                .iter()
                .all(|r| crate::text::trim(r).trim_matches('│').trim().is_empty()),
            "only blank rows between the border and {title}"
        );
    }
}

/// The shell box for a command with a REDIRECTION draws a warning under its
/// question (`Redirection detected. To auto-accept, press Shift+Tab`,
/// captured under the lab): it is the same Bash prompt, its question the
/// row over the warning, its options' roles the shell box's.
#[test]
fn a_shell_box_with_a_redirection_warning_is_still_the_shell_box() {
    let rows = screen(BOX_SHELL_REDIRECTION);
    let p = prompt(&rows).expect("the box");
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.command, "env | cut -d= -f1 | sort > envnames.txt");
    assert!(
        p.question
            .is_some_and(|q| rows[q].contains("Allow execution of [Shell]?")),
        "{:?}",
        p.question
    );
    assert_eq!(roles(&p), vec![Role::Once, Role::Persist, Role::Deny]);
    let other = edited(
        &rows,
        "│ Redirection detected.",
        &rows
            .iter()
            .find(|r| r.starts_with("│ Redirection detected."))
            .expect("the warning")
            .replacen("Redirection detected.", "Something else noted.", 1),
    );
    let p = prompt(&other).expect("still a box");
    assert_eq!((p.kind, p.question), (PromptKind::Other, None));
}

/// The shell tool's box: a Bash prompt for the command in the box drawn
/// inside it, its three options by their roles — `Allow once` the one-shot,
/// `Allow for this session` a standing grant ([`Role::Persist`]: every later
/// command of the same root), `No, suggest changes (esc)` the refusal and the
/// box's Esc — the first focused, chosen by digit.
#[test]
fn the_shell_box_is_a_bash_prompt_with_its_roles() {
    let r = gemini(BOX_SHELL);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    assert_eq!((r.composer, r.wall), (None, None));
    let p = r.prompt.expect("the box");
    let rows = screen(BOX_SHELL);
    assert_eq!(p.kind, PromptKind::Bash);
    assert_eq!(p.title, "Shell  touch lab-probe.txt");
    assert_eq!(p.command, "touch lab-probe.txt");
    assert_eq!(p.readings(), vec!["touch lab-probe.txt".to_string()]);
    assert_eq!(roles(&p), vec![Role::Once, Role::Persist, Role::Deny]);
    assert_eq!(
        labels(&p),
        vec![
            anchor_text("gemini.box.once"),
            anchor_text("gemini.box.session"),
            anchor_text("gemini.box.deny")
        ]
    );
    assert_eq!(p.focused().map(|o| o.n), Some(Some(1)));
    assert_eq!(p.select, Select::Digits);
    assert!(
        p.question
            .is_some_and(|q| rows[q].contains("Allow execution of [Shell]?"))
    );
    let cancel = p.cancel.expect("esc");
    assert_eq!(
        (cancel.verb.as_str(), cancel.effect),
        ("No, suggest changes", CancelEffect::Reject)
    );
    assert_eq!(p.span.1, rows.len() - 1, "the box is the last thing drawn");

    let two = rows
        .iter()
        .position(|r| r.contains("  2. Allow for this session"))
        .expect("option 2");
    let mut cursors = rows.clone();
    cursors[two] = rows[two].replacen("  2.", "● 2.", 1);
    let p = prompt(&cursors).expect("still a box");
    assert!(
        roles(&p).iter().all(|&r| r == Role::Other),
        "{:?}",
        roles(&p)
    );
    let mut gap = rows.clone();
    gap[two] = rows[two].replacen("2.", "5.", 1);
    let p = prompt(&gap).expect("still a box");
    assert!(
        roles(&p).iter().all(|&r| r == Role::Other),
        "{:?}",
        roles(&p)
    );
}

/// The file boxes: `WriteFile` is a Write prompt and `Edit` an Edit prompt,
/// each for the file its title names, both asking `Apply this change?` over
/// four options — `Modify with external editor` [`Role::Other`], since it
/// opens an editor, and `Allow for this session` [`Role::Persist`], since on
/// a file box it switches the session to accepting every edit.
#[test]
fn the_file_boxes_are_write_and_edit_prompts() {
    for (text, kind) in [(BOX_WRITE, PromptKind::Write), (BOX_EDIT, PromptKind::Edit)] {
        let r = gemini(text);
        let what = provenance(text).unwrap_or_default();
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
        let p = r.prompt.expect("the box");
        assert_eq!(p.kind, kind, "{what}");
        assert_eq!(
            (p.path.as_deref(), p.command.as_str()),
            (Some("lab-write.txt"), "lab-write.txt"),
            "{what}"
        );
        assert_eq!(
            roles(&p),
            vec![Role::Once, Role::Persist, Role::Other, Role::Deny],
            "{what}"
        );
        assert_eq!(labels(&p)[2], anchor_text("gemini.box.editor"));
        let rows = screen(text);
        assert_eq!(ClaudeReader.prompt(&rows), None, "the control");
        assert_eq!(AgyReader.prompt(&rows), None, "the control");
        let asked = edited(
            &rows,
            "│ Apply this change?",
            &rows[rows
                .iter()
                .rposition(|r| r.starts_with("│ Apply this change?"))
                .expect("the question")]
            .replacen("Apply this change?", "Apply these edits?", 1),
        );
        let p = prompt(&asked).expect("still a box");
        assert_eq!(p.kind, PromptKind::Other);
        assert_eq!(roles(&p)[0], Role::Once, "its one-shot still names it");
    }
}

/// The ask-the-user tool's dialog: a Question prompt, its question the row
/// under `Answer Questions`, the model's answers [`Role::Answer`] (their
/// descriptions under them no options) and `Enter a custom value` (the
/// person's own words) [`Role::Other`], chosen by digit, its key row's `Esc
/// to cancel` leaving it unanswered.
#[test]
fn the_question_tool_is_a_question_with_answers() {
    let r = gemini(QUESTION);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the question");
    assert_eq!(p.kind, PromptKind::Question);
    assert_eq!(p.title, anchor_text("gemini.question.title"));
    assert_eq!(p.description, "Which colour should the lab probe use?");
    assert_eq!(labels(&p), vec!["Red", "Blue", "Enter a custom value"]);
    assert_eq!(roles(&p), vec![Role::Answer, Role::Answer, Role::Other]);
    assert_eq!(p.select, Select::Digits);
    let cancel = p.cancel.expect("esc");
    assert_eq!(
        (cancel.verb.as_str(), cancel.effect),
        ("cancel", CancelEffect::Back)
    );
    assert!(
        p.footer
            .as_deref()
            .is_some_and(|f| f.starts_with(anchor_text("gemini.question.keys")))
    );
    let rows = screen(QUESTION);
    assert!(!matches!(
        ClaudeReader.phase(&rows),
        Phase::Prompt | Phase::Question
    ));

    let answered = gemini(QUESTION_ANSWERED);
    assert_eq!(answered.prompt, None);
    assert_eq!(
        (answered.phase, answered.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(answered.said_tail.as_deref(), Some("Tool call done."));
}

/// THE PER-DAY QUOTA'S DIALOG IS A BOX, NOT A WALL (module header): a
/// [`PromptKind::UsageLimit`] box in place of the composer, read as a
/// prompt with no wall, and neither `Keep trying` nor `Stop` has a role any
/// rule presses.
#[test]
fn the_usage_limit_dialog_is_a_box_with_nothing_to_press() {
    let r = gemini(LIMIT_DIALOG);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    assert_eq!((r.wall, r.composer), (None, None));
    let p = r.prompt.expect("the dialog");
    assert_eq!(p.kind, PromptKind::UsageLimit);
    assert!(p.title.starts_with(anchor_text("gemini.usage.title")));
    assert_eq!(labels(&p), vec![anchor_text("gemini.usage.retry"), "Stop"]);
    assert_eq!(roles(&p), vec![Role::Other, Role::Other]);
    assert_eq!(p.cancel, None);

    let stopped = gemini(LIMIT_STOPPED);
    assert_eq!(
        (stopped.phase, stopped.phase_authoritative),
        (Phase::Idle, true)
    );
    let w = stopped.wall.expect("the error row");
    assert_eq!(
        w.kind,
        WallKind::ApiError {
            code: None,
            retryable: false,
            cause: ApiCause::Server
        }
    );
    assert_eq!(w.message, "[API Error: An unknown error occurred.]");

    let rows = screen(LIMIT_DIALOG);
    let renamed = edited(
        &rows,
        "│ Usage limit reached",
        &rows[rows
            .iter()
            .position(|r| r.starts_with("│ Usage limit reached"))
            .expect("the title")]
        .replacen("Usage limit reached", "Quota exhausted xxx", 1),
    );
    let p = prompt(&renamed).expect("still a box");
    assert_eq!(p.kind, PromptKind::Other);
    assert!(roles(&p).iter().all(|&r| r == Role::Other));
}

/// A Google account's spent quota draws the same dialog with its reset named
/// under the title (`Access resets at 5:06 AM PDT.`): the same
/// [`PromptKind::UsageLimit`] box, read as a prompt with no wall, its title
/// the title row alone, and nothing on it to press.
#[test]
fn the_accounts_usage_limit_dialog_names_its_reset_and_is_the_same_box() {
    let r = gemini(LIMIT_DIALOG_ACCOUNT);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    assert_eq!((r.wall, r.composer), (None, None));
    let p = r.prompt.expect("the dialog");
    assert_eq!(p.kind, PromptKind::UsageLimit);
    assert!(p.title.starts_with(anchor_text("gemini.usage.title")));
    assert_eq!(labels(&p), vec![anchor_text("gemini.usage.retry"), "Stop"]);
    assert_eq!(roles(&p), vec![Role::Other, Role::Other]);
    assert_eq!(p.cancel, None);
    let rows = screen(LIMIT_DIALOG_ACCOUNT);
    assert!(
        rows.iter()
            .any(|r| r.starts_with("│ Access resets at 5:06 AM PDT.")),
        "{rows:#?}"
    );
    assert!(
        !screen(LIMIT_DIALOG)
            .iter()
            .any(|r| r.contains("Access resets at"))
    );

    let title = rows
        .iter()
        .position(|r| r.starts_with("│ Usage limit reached"))
        .expect("the title");
    let mut swapped = rows.clone();
    swapped.swap(title, title + 1);
    let p = prompt(&swapped).expect("still a box");
    assert_eq!(p.kind, PromptKind::Other);
    assert!(roles(&p).iter().all(|&r| r == Role::Other));
}

/// THE PER-MODEL BUCKET'S DIALOG (a terminal quota on a Pro model whose
/// fallback is another, HAND-BUILT by the bundle's `ProQuotaDialog`) is the
/// same [`PromptKind::UsageLimit`] box: its title names `all Pro models`,
/// its reset is read as the account's, and its options are `Switch to
/// <fallback>` over `Stop` — under the Google login with `Upgrade for higher
/// limits` between them — none with a role. NEGATIVE CONTROL: the measured
/// account dialog offers `Keep trying` over `Stop`, and no switch.
#[test]
fn the_buckets_usage_limit_dialog_offers_its_switch_and_stop() {
    for (fixture, want) in [
        (
            LIMIT_DIALOG_BUCKET,
            vec!["Switch to gemini-3.5-flash", "Stop"],
        ),
        (
            LIMIT_DIALOG_BUCKET_UPGRADE,
            vec![
                "Switch to gemini-3.5-flash",
                anchor_text("gemini.usage.upgrade"),
                "Stop",
            ],
        ),
    ] {
        let r = gemini(fixture);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
        assert_eq!((r.wall, r.composer), (None, None));
        let p = r.prompt.expect("the dialog");
        assert_eq!(p.kind, PromptKind::UsageLimit);
        assert_eq!(p.title, "Usage limit reached for all Pro models.");
        assert_eq!(labels(&p), want);
        assert!(labels(&p)[0].starts_with(anchor_text("gemini.usage.switch")));
        assert!(roles(&p).iter().all(|&r| r == Role::Other));
        assert_eq!(
            limit_reset(&screen(fixture)),
            Some(reset_of("5:06 AM", Some("PDT")))
        );
    }
    let measured = prompt(&screen(LIMIT_DIALOG_ACCOUNT)).expect("the dialog");
    assert!(
        !labels(&measured)
            .iter()
            .any(|l| l.starts_with(anchor_text("gemini.usage.switch")))
    );
}

/// The reset as [`limit_reset`] reads it: `clock` and `zone`.
fn reset_of(clock: &str, zone: Option<&str>) -> LimitReset {
    LimitReset {
        clock: clock.to_string(),
        zone: zone.map(str::to_string),
    }
}

/// The reset the account's dialog names is its clock AND the zone it is
/// drawn in (`5:06 AM` in `PDT` of `Access resets at 5:06 AM PDT.`), the
/// CLI's no-break space read as a space; the supervisor waits for it before
/// `Keep trying`.
#[test]
fn the_accounts_usage_limit_dialog_names_its_reset_by_its_clock() {
    let rows = screen(LIMIT_DIALOG_ACCOUNT);
    let read = limit_reset(&rows).expect("the reset");
    assert_eq!(read, reset_of("5:06 AM", Some("PDT")));
    assert_eq!(read.to_string(), "5:06 AM PDT");
    assert_eq!(read.offset_s(), Some(-7 * 3600));
    let row = rows
        .iter()
        .find(|r| r.starts_with("│ Access resets at"))
        .expect("the reset row")
        .clone();
    let respelled = |from: &str, to: &str| -> Vec<String> {
        assert!(row.contains(from), "{row:?}");
        let with = row.replacen(from, to, 1);
        let body = with.trim_end_matches('│').trim_end();
        let width = row.chars().count() - 1;
        edited(&rows, "│ Access resets at", &format!("{body:<width$}│"))
    };
    assert_eq!(
        limit_reset(&respelled("5:06 AM PDT", "5:06\u{202f}AM PDT")),
        Some(reset_of("5:06 AM", Some("PDT")))
    );
    for (zone, offset) in [("GMT+2", 2 * 3600), ("GMT+5:30", 19_800), ("UTC", 0)] {
        let read = limit_reset(&respelled("5:06 AM PDT", &format!("5:06 AM {zone}")));
        assert_eq!(read, Some(reset_of("5:06 AM", Some(zone))), "{zone}");
        assert_eq!(read.and_then(|r| r.offset_s()), Some(offset), "{zone}");
    }
    let bare = limit_reset(&respelled("5:06 AM PDT.", "11:30 PM.")).expect("the reset");
    assert_eq!(bare, reset_of("11:30 PM", None));
    assert_eq!(bare.offset_s(), None);
    let foreign = limit_reset(&respelled("5:06 AM PDT", "5:06 AM CEST")).expect("the reset");
    assert_eq!(foreign, reset_of("5:06 AM", Some("CEST")));
    assert_eq!(foreign.offset_s(), None);
    let at = rows.iter().position(|r| *r == row).expect("the row");
    let width = row.chars().count() - 1;
    let boxed = |text: &str| format!("{:<width$}│", format!("│ {text}"));
    let wrapped = |first: &str, rest: &[&str]| -> Vec<String> {
        let mut w = rows.clone();
        w[at] = boxed(first);
        for (k, more) in rest.iter().enumerate() {
            w.insert(at + 1 + k, boxed(more));
        }
        w
    };
    for (first, rest, clock, zone) in [
        (
            "Access resets at 11:05 PM",
            &["PDT."][..],
            "11:05 PM",
            "PDT",
        ),
        (
            "Access resets at 9:35 PM",
            &["GMT+5:30."][..],
            "9:35 PM",
            "GMT+5:30",
        ),
        (
            "Access resets at",
            &["11:05\u{202f}PM", "EDT."][..],
            "11:05 PM",
            "EDT",
        ),
    ] {
        let w = wrapped(first, rest);
        assert_eq!(
            prompt(&w).map(|p| p.kind),
            Some(PromptKind::UsageLimit),
            "PRECONDITION: still the dialog"
        );
        let read = limit_reset(&w);
        assert_eq!(read, Some(reset_of(clock, Some(zone))), "{w:#?}");
        assert!(read.and_then(|r| r.offset_s()).is_some(), "{zone}");
    }
    let cut = limit_reset(&wrapped("Access resets at 11:05 PM", &[])).expect("the reset");
    assert_eq!(
        cut,
        reset_of("11:05 PM", None),
        "the next line is no rest of it"
    );
    assert_eq!(limit_reset(&screen(LIMIT_DIALOG)), None);
    assert_eq!(limit_reset(&screen(IDLE)), None);
    let at = rows.iter().position(|r| *r == row).expect("the row");
    let option = rows
        .iter()
        .position(|r| r.contains("2. Stop"))
        .expect("the last option");
    let mut under = rows.clone();
    let moved = under.remove(at);
    under.insert(option, moved);
    assert_eq!(
        prompt(&under).map(|p| p.kind),
        Some(PromptKind::UsageLimit),
        "still the dialog"
    );
    assert_eq!(limit_reset(&under), None, "{under:#?}");
    let renamed = respelled("Access resets at", "Access ended at");
    assert_eq!(limit_reset(&renamed), None);
}

/// The zone a reset is drawn in reads as its offset, for every spelling the
/// measure drew ([`zone_offset_s`]: `Intl.DateTimeFormat("en-US", …
/// timeZoneName: "short")` over every IANA zone, January and July, as
/// `node` printed them under `TZ=<zone>`: `5:06 AM PDT`, `9:36 AM GMT-2:30`,
/// `1:51 AM GMT+13:45`, `3:06 AM HADT`, `12:06 PM GMT+0` for a `TZ` it does
/// not know).
#[test]
fn the_dialogs_zone_names_read_as_their_offsets() {
    let h = |n: i32| n * 3600;
    for (zone, offset) in [
        ("UTC", 0),
        ("GMT", 0),
        ("GMT+0", 0),
        ("PST", h(-8)),
        ("PDT", h(-7)),
        ("MST", h(-7)),
        ("MDT", h(-6)),
        ("CST", h(-6)),
        ("CDT", h(-5)),
        ("EST", h(-5)),
        ("EDT", h(-4)),
        ("AKST", h(-9)),
        ("AKDT", h(-8)),
        ("HST", h(-10)),
        ("HAST", h(-10)),
        ("HADT", h(-9)),
        ("AST", h(-4)),
        ("ADT", h(-3)),
        ("GMT+1", h(1)),
        ("GMT+2", h(2)),
        ("GMT+9", h(9)),
        ("GMT+14", h(14)),
        ("GMT-12", h(-12)),
        ("GMT+5:30", h(5) + 1800),
        ("GMT+5:45", h(5) + 2700),
        ("GMT+13:45", h(13) + 2700),
        ("GMT-2:30", -(h(2) + 1800)),
        ("GMT-9:30", -(h(9) + 1800)),
    ] {
        assert_eq!(zone_offset_s(zone), Some(offset), "{zone}");
    }
    for zone in [
        "CEST",
        "IST",
        "BST",
        "America/Los_Angeles",
        "pdt",
        "GMT+15",
        "GMT+5:3",
        "GMT+5:60",
        "GMT+",
        "GMT5",
        "GMT+123",
        "GMT+-2",
        "",
    ] {
        assert_eq!(zone_offset_s(zone), None, "{zone}");
    }
}

/// The folder-trust dialog of a first run is a box no typing answers: a
/// Trust prompt, `Trust folder (<dir>)` the trust, `Trust parent folder
/// (<parent>)` a wider standing grant, `Don't trust` the refusal, chosen by
/// digit; its Esc quits the program and is drawn nowhere, so none is
/// offered.
#[test]
fn the_trust_dialog_is_a_trust_prompt() {
    let r = gemini(TRUST);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    assert_eq!(r.composer, None);
    let p = r.prompt.expect("the dialog");
    assert_eq!(p.kind, PromptKind::Trust);
    assert_eq!(p.description, anchor_text("gemini.trust.title"));
    assert_eq!(
        labels(&p),
        vec![
            "Trust folder (proj)",
            "Trust parent folder (at.7EJB)",
            "Don't trust"
        ]
    );
    assert_eq!(roles(&p), vec![Role::Trust, Role::Persist, Role::Deny]);
    assert_eq!(p.select, Select::Digits);
    assert_eq!(p.cancel, None);
    let rows = screen(TRUST);
    assert_eq!(ClaudeReader.prompt(&rows), None, "the control");
    let both = edited(
        &rows,
        " │   3. Don't trust",
        &rows[rows
            .iter()
            .position(|r| r.starts_with(" │   3. Don't trust"))
            .expect("option 3")]
        .replacen("  3.", "● 3.", 1),
    );
    let p = prompt(&both).expect("still a box");
    assert!(roles(&p).iter().all(|&r| r == Role::Other));
}

/// AN API ERROR ROW IS THE TURN'S WALL: the 401 and the 400 Google answers
/// a key that is no key are the login ([`WallKind::Auth`]), their JSON read
/// across the rows it wrapped onto (mid-token); the message is the row and
/// its wrap, joined.
#[test]
fn an_api_error_row_is_the_turns_wall() {
    for text in [ERROR_401, ERROR_400_KEY] {
        let r = gemini(text);
        let what = provenance(text).unwrap_or_default();
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert_eq!(r.said_tail, None, "{what}");
        let w = r.wall.expect("the wall");
        assert_eq!(w.kind, WallKind::Auth, "{what}");
        assert_eq!(w.placement, Placement::ErrorRow);
        assert!(w.message.starts_with("[API Error: {"), "{}", w.message);
        assert!(w.message.ends_with("}}]"), "{}", w.message);
        let rows = screen(text);
        assert!(
            rows[w.row + 1..]
                .iter()
                .any(|r| r.contains(anchor_text("gemini.error.failed"))),
            "{what}"
        );
        let mut replied = rows.clone();
        replied.insert(status_at(&rows), "✦ Retrying now.".to_string());
        assert_eq!(wall(&replied), None, "{what}");
        assert_eq!(said_tail(&replied).as_deref(), Some("Retrying now."));
        let mut running = rows.clone();
        let status = status_at(&rows);
        running[status] = " ⠏ Thinking... (esc to cancel, 1s)".to_string();
        assert_eq!(wall(&running), None, "{what}");
    }
    assert_eq!(
        error_kind(
            r#"[API Error: {"error":{"code":400,"message":"bad","status":"INVALID_ARGUMENT"}}]"#
        ),
        WallKind::ApiError {
            code: Some(400),
            retryable: false,
            cause: ApiCause::Server
        }
    );
    assert_eq!(
        error_kind(r#"[API Error: {"error":{"code":503,"status":"UNAVAILABLE"}}]"#),
        WallKind::ApiError {
            code: Some(503),
            retryable: true,
            cause: ApiCause::Server
        }
    );
    assert_eq!(
        error_kind("[API Error: {\"error\":{\"co\n  de\":401}}]"),
        WallKind::Auth,
        "a code the wrap cut"
    );
}

/// AN ERROR'S NOTICE WRAPS in a pane narrower than its line (measured at 72
/// columns: `ℹ This request failed. …` over `  change "Error Verbosity" …`),
/// and the error is the turn's wall all the same: the `Stop` a usage limit
/// left, a non-retryable API error, idle as evidence.
#[test]
fn an_error_whose_notice_wrapped_is_still_the_wall() {
    let r = gemini(NARROW_LIMIT_STOPPED);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
    assert_eq!(
        r.wall.map(|w| (w.kind, w.message)),
        Some((
            WallKind::ApiError {
                code: None,
                retryable: false,
                cause: ApiCause::Server
            },
            "[API Error: An unknown error occurred.]".to_string()
        ))
    );
    let rows = screen(NARROW_LIMIT_STOPPED);
    let notice = rows
        .iter()
        .position(|r| r.starts_with("ℹ This request failed."))
        .expect("the notice");
    assert!(rows[notice + 1].starts_with("  change "), "its wrap");
    let mut stray = rows.clone();
    stray.insert(notice + 2, String::new());
    stray.insert(notice + 3, "  a row of nobody's".to_string());
    assert_eq!(wall(&stray), None, "an indented row under a blank");
    let mut replied = rows.clone();
    replied.insert(notice + 2, "✦ Went on.".to_string());
    assert_eq!(wall(&replied), None, "a reply under it");
}

/// `/quit`: the farewell box, the resume line inside it, and the shell's
/// prompt under it.
#[test]
fn an_exited_gemini_is_read_as_nothing() {
    let r = gemini(EXIT);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, false));
    assert_eq!((r.composer, r.prompt), (None, None));
    let rows = screen(EXIT);
    assert_eq!(identify(None, &rows).program(), Program::Generic);
    assert!(
        rows.iter()
            .any(|r| r.contains(anchor_text("gemini.exit.resume")))
    );
    assert!(
        rows.iter()
            .any(|r| r.contains(anchor_text("gemini.exit.farewell")))
    );
    let last = rows
        .iter()
        .rposition(|r| r.starts_with('╰'))
        .expect("the farewell's bottom");
    let cut = rows[..=last].to_vec();
    assert!(gemini_box(&cut).is_some(), "drawn last");
    assert_eq!(prompt(&cut), None);
    assert_eq!(phase(&cut), (Phase::Idle, false));
}

/// A box whose head scrolled off the rows read is named by nothing (an Other
/// prompt, its head off the screen), and its options take the approval
/// boxes' roles where `Allow once` is among them.
#[test]
fn a_box_whose_head_scrolled_off_is_named_by_its_one_shot_allow() {
    let rows = screen(BOX_SHELL);
    let at = rows
        .iter()
        .position(|r| r.starts_with("│ Allow execution of"))
        .expect("the question");
    let cut = rows[at..].to_vec();
    assert_eq!(phase(&cut), (Phase::Prompt, true));
    let p = prompt(&cut).expect("still a box");
    assert_eq!((p.kind, p.head_off_screen), (PromptKind::Other, true));
    assert_eq!(roles(&p), vec![Role::Once, Role::Persist, Role::Deny]);
    let unnamed = edited(
        &cut,
        "│ ● 1. Allow once",
        &cut[cut
            .iter()
            .position(|r| r.starts_with("│ ● 1. Allow once"))
            .expect("option 1")]
        .replacen("Allow once", "Allow it  ", 1),
    );
    let p = prompt(&unnamed).expect("still a box");
    assert!(
        roles(&p).iter().all(|&r| r == Role::Other),
        "{:?}",
        roles(&p)
    );
}

/// Claude Code's, Codex's, agy's and Muse Code's readers on Gemini CLI's
/// screens: never a box, never a question.
#[test]
fn the_other_readers_see_no_box_and_no_question_on_gemini_screens() {
    for (name, text) in captures() {
        let r = screen(&text);
        for reader in [
            &ClaudeReader as &dyn ScreenReader,
            &CodexReader,
            &AgyReader,
            &MuseReader,
        ] {
            let phase = reader.phase(&r);
            assert!(
                !matches!(phase, Phase::Prompt | Phase::Question),
                "{:?} on {name}: {phase:?}",
                reader.program()
            );
            assert_eq!(reader.prompt(&r), None, "{:?} on {name}", reader.program());
        }
    }
}

/// A shell, a pager or a viewer showing Gemini CLI's screen is not Gemini
/// CLI: the program's NAME wins.
#[test]
fn a_shell_quoting_gemini_is_not_gemini() {
    let r = screen(BOX_SHELL);
    for shell in ["zsh", "-bash", "less", "cat"] {
        let reading = read(Some(shell), &r, None);
        assert_eq!(reading.program, Program::Generic, "{shell}");
        assert_eq!(reading.prompt, None, "{shell}");
    }
    let mut named = 0;
    for (file, text) in captures() {
        let rows = screen(&text);
        if text == EXIT || text == ERASE_HALF {
            continue;
        }
        for name in [None, Some("node"), Some("/opt/homebrew/bin/gemini")] {
            assert_eq!(
                identify(name, &rows).program(),
                Program::Gemini,
                "{name:?}: {file}"
            );
        }
        assert_eq!(
            identify(Some("zsh"), &rows).program(),
            Program::Generic,
            "the name wins: {file}"
        );
        named += 1;
    }
    assert_eq!(
        named,
        captures().len() - 2,
        "every capture but the exit and the half-drawn redraw"
    );
    assert!(named >= 22, "the 0.46.0 captures: {named}");
    let quoted = rows(&[
        "% grep -h Thinking log.txt",
        " ⠧ Thinking... (esc to cancel, 3s)",
        " > think slowly",
        "%",
    ]);
    assert_eq!(identify(None, &quoted).program(), Program::Generic);
}

/// Gemini CLI is named `gemini`, by its path and arguments too, and its busy
/// row is the spinner on the status row: exactly one on each busy screen —
/// in column 1, or in column 0 with the UI details hidden.
#[test]
fn gemini_is_named_and_busy_on_its_spinner_row() {
    assert_eq!(crate::reader::program_of("gemini"), Some(Program::Gemini));
    assert_eq!(
        crate::reader::program_of("/opt/homebrew/bin/gemini --resume latest"),
        Some(Program::Gemini)
    );
    assert_eq!(Program::Gemini.product(), "Gemini CLI");
    for text in [
        BUSY_THINKING,
        BUSY_STREAMING,
        RETRY_503,
        RETRY_500,
        DETAILS_HIDDEN_BUSY,
    ] {
        let spinners = screen(text).iter().filter(|r| is_spinner_row(r)).count();
        assert_eq!(spinners, 1, "{}", provenance(text).unwrap_or_default());
    }
    assert!(is_spinner_row("⠋ Lab stub thinking (esc to cancel, 1s)"));
    for row in [
        &format!("{:>143}", "? for shortcuts"),
        "✦ tick 16 tick 17 tick 18",
        "  ⠧ Thinking... (esc to cancel, 3s)",
        " ⠀ blank braille is no frame",
        " ⠧Thinking...",
    ] {
        assert!(!is_spinner_row(row), "{row}");
    }
    assert_eq!(busy_guard(), "^\\s?[\u{2801}-\u{28ff}]\\s\\S");
}

/// THE OTHER SNIFFS CLAIM NONE OF GEMINI CLI'S SCREENS, so the sniff's order
/// is no part of naming them: Muse Code's, agy's, Claude Code's (its frame,
/// and its box parser where it names its own kind) say no on every Gemini
/// CLI capture.
#[test]
fn no_other_sniff_claims_a_gemini_screen() {
    use crate::prompt::{PromptKind, parse_prompt_v2};
    let names_claude =
        |r: &[String]| parse_prompt_v2(r).is_some_and(|p| p.kind != PromptKind::Other);
    for (name, text) in captures() {
        let r = screen(&text);
        assert!(!crate::muse::is_muse_screen(&r), "muse: {name}");
        assert!(!crate::agy::is_agy_screen(&r), "agy: {name}");
        assert!(
            !crate::phase::has_composer_frame(&r),
            "claude frame: {name}"
        );
        assert!(!names_claude(&r), "claude box: {name}");
    }
    assert!(crate::muse::is_muse_screen(&screen(
        crate::muse::fixtures::IDLE
    )));
    assert!(crate::agy::is_agy_screen(&screen(
        crate::agy::fixtures::IDLE
    )));
    assert!(crate::phase::has_composer_frame(&screen(
        crate::prompt::fixtures::LAUNCH_REPL_READY
    )));
    assert!(names_claude(&screen(crate::prompt::fixtures::TRUST)));
}

/// Only Gemini CLI draws its caret one column in: every other reader's guard
/// anchors the caret at column 0 ([`ScreenReader::caret_column`]).
#[test]
fn only_gemini_clis_caret_is_one_column_in() {
    for p in Program::ALL {
        let want = usize::from(p == Program::Gemini);
        assert_eq!(p.reader().caret_column(), want, "{p:?}");
    }
    assert_eq!(Program::Gemini.reader().caret(), Some('>'));
    assert!(
        !Program::Gemini.reader().placeholder(),
        "taken out by its reader"
    );
    assert!(Program::Gemini.reader().paste_guard());
}

/// THE SNIFF CLAIMS NO OTHER PROGRAM'S SCREEN: over every capture of Claude
/// Code, agy and Muse Code under `src/fixtures/`, Gemini CLI's layout
/// test says no, the screen is never identified as Gemini CLI's, and Gemini
/// CLI's reader reads no box and no composer on it.
#[test]
fn the_gemini_sniff_claims_no_other_programs_screen() {
    let mut swept = 0;
    for (name, text) in crate::text::fixture_files() {
        let r = screen(&text);
        if name.starts_with("gemini-") {
            assert_eq!(
                is_gemini_screen(&r),
                text != EXIT && text != ERASE_HALF,
                "{name}"
            );
            continue;
        }
        swept += 1;
        assert!(!is_gemini_screen(&r), "{name}");
        assert_ne!(identify(None, &r).program(), Program::Gemini, "{name}");
        assert_eq!(prompt(&r), None, "{name}");
        assert_eq!(composer_draft(&r), None, "{name}");
    }
    assert!(swept > 150, "only {swept} other captures");
}

/// SHELL MODE (`!` typed into the empty composer, measured): Gemini CLI's
/// screen, named by its layout, but no evidence and no composer — what is
/// typed there runs as a shell command — and no caret anything is typed
/// under; its command's run draws no box this reader reads.
#[test]
fn shell_mode_is_geminis_screen_but_no_composer_and_no_evidence() {
    for text in [SHELL_MODE, SHELL_MODE_DRAFT, SHELL_MODE_RAN] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = gemini(text);
        assert_eq!(
            (r.phase, r.phase_authoritative),
            (Phase::Idle, false),
            "{what}"
        );
        assert_eq!((r.composer, r.prompt, r.wall), (None, None, None), "{what}");
        assert_eq!(Program::Gemini.reader().caret_on(&rows), None, "{what}");
        assert_eq!(identify(Some("node"), &rows).program(), Program::Gemini);
    }
    let rows = screen(SHELL_MODE);
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with(" ! "))
        .expect("the shell caret");
    let mut message = rows.clone();
    message[caret] = " >   Type your message or @path/to/file".to_string();
    assert_eq!(phase(&message), (Phase::Idle, true), "the control");
    assert_eq!(
        composer_draft(&message).map(|(_, l)| l),
        Some(vec![String::new()])
    );

    let left = gemini(SHELL_MODE_LEFT);
    assert_eq!((left.phase, left.phase_authoritative), (Phase::Idle, true));
    assert_eq!(left.composer.map(|(_, l)| l), Some(vec![String::new()]));
    assert!(!left.fresh, "the shell command is the person's message");
    assert_eq!((left.said_tail, left.wall), (None, None));
    let unsent = edited(
        &screen(SHELL_MODE_LEFT),
        " $ ",
        " % echo lab-shell-$((6*7))",
    );
    assert!(fresh(&unsent), "the control: no message yet");
}

/// YOLO (ctrl+y, measured) draws the composer's caret `*`: the composer all
/// the same — at rest, with a draft, busy under the spinner and at a turn's
/// end, its tool run with no box — and a guard on typed text anchors to `*`
/// there ([`caret_on`]).
#[test]
fn yolo_is_a_composer_under_its_own_caret() {
    for (text, phase_, draft) in [
        (YOLO_IDLE, Phase::Idle, ""),
        (YOLO_DRAFT, Phase::Idle, "hello in yolo"),
        (YOLO_BUSY, Phase::Busy, ""),
        (YOLO_END_OF_TURN, Phase::Idle, ""),
        (YOLO_TOOL_RAN, Phase::Idle, ""),
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = gemini(text);
        assert_eq!((r.phase, r.phase_authoritative), (phase_, true), "{what}");
        assert_eq!(
            r.composer.map(|(_, l)| l),
            Some(vec![draft.to_string()]),
            "{what}"
        );
        assert_eq!(caret_on(&rows), Some('*'), "{what}");
        assert_eq!(Program::Gemini.reader().caret_on(&rows), Some('*'));
    }
    assert!(
        gemini(YOLO_END_OF_TURN)
            .said_tail
            .is_some_and(|t| t.starts_with("tick 16") && t.ends_with("tick 30"))
    );
    assert_eq!(
        gemini(YOLO_TOOL_RAN).said_tail.as_deref(),
        Some("Tool call done.")
    );
    assert_eq!(caret_on(&screen(IDLE)), Some('>'), "the control");
    assert_eq!(caret_on(&screen(BOX_SHELL)), Some('>'));
    let top = "▄".repeat(144);
    let bottom = "▀".repeat(144);
    let sent = rows(&["✦ Hi.", &top, " * not the composer", &bottom, "╭──╮"]);
    assert_eq!(composer_draft(&sent), None);
}

/// THE UI DETAILS HIDDEN (Tab twice, measured, `focusUiEnabled` in the CLI's
/// state, so it stays across sessions): no rule, mode row or footer — the
/// status row right over the band, the band the last thing drawn — and still
/// the composer: at rest under `press tab twice for more` (or a blank row
/// with the hint off), with a draft under a blank row, busy under the
/// spinner in column 0, at a turn's end with its last words, under YOLO with
/// `● YOLO` on the status row and the rule back, or the mode's row alone
/// over a draft; its box is the shell box.
#[test]
fn the_composer_with_the_ui_details_hidden_is_read() {
    for (text, phase_, draft, caret) in [
        (DETAILS_HIDDEN_IDLE, Phase::Idle, "", '>'),
        (DETAILS_HIDDEN_DRAFT, Phase::Idle, "a hidden draft", '>'),
        (DETAILS_HIDDEN_BUSY, Phase::Busy, "", '>'),
        (DETAILS_HIDDEN_END_OF_TURN, Phase::Idle, "", '>'),
        (DETAILS_HIDDEN_YOLO_IDLE, Phase::Idle, "", '*'),
        (
            DETAILS_HIDDEN_YOLO_DRAFT,
            Phase::Idle,
            "yolo hidden draft",
            '*',
        ),
        (HINT_OFF_DETAILS_HIDDEN_IDLE, Phase::Idle, "", '>'),
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = gemini(text);
        assert_eq!((r.phase, r.phase_authoritative), (phase_, true), "{what}");
        assert_eq!(
            r.composer.map(|(_, l)| l),
            Some(vec![draft.to_string()]),
            "{what}"
        );
        assert_eq!(caret_on(&rows), Some(caret), "{what}");
        assert_eq!(identify(None, &rows).program(), Program::Gemini, "{what}");
        assert_eq!(r.prompt, None, "{what}");
    }
    assert!(
        gemini(DETAILS_HIDDEN_END_OF_TURN)
            .said_tail
            .is_some_and(|t| t.ends_with("tick 30"))
    );
    let boxed = gemini(DETAILS_HIDDEN_BOX_SHELL);
    assert_eq!(boxed.prompt.map(|p| p.kind), Some(PromptKind::Bash));
    assert_eq!(boxed.composer, None);

    let rows = screen(DETAILS_HIDDEN_IDLE);
    let bottom = rows
        .iter()
        .rposition(|r| r.starts_with('▀'))
        .expect("the band");
    let mut listed = rows[..=bottom].to_vec();
    listed.push("  /about   Show version info".to_string());
    assert_eq!(composer_draft(&listed), None, "a row under the band");
    assert_eq!(phase(&listed), (Phase::Idle, false));
    let top = bottom - 2;
    let mut tipped = rows[..=bottom].to_vec();
    tipped[top - 1] = " Tip: a tip this grammar never read".to_string();
    assert_eq!(composer_draft(&tipped), None, "unmeasured words");
    let mut replied = rows[..=bottom].to_vec();
    replied[top - 1] = "✦ a reply's row".to_string();
    assert_eq!(composer_draft(&replied), None, "the transcript's band");
}

/// `ui.showShortcutsHint = false` (measured at 144 columns): the status row
/// is blank at rest in a wide pane too, and that is the composer's rest, as
/// evidence — the session continued as any other; busy is the spinner, and
/// the turn's end is the spinner gone with its last words.
#[test]
fn a_rest_with_the_shortcuts_hint_off_is_evidence() {
    let idle = gemini(HINT_OFF_IDLE);
    assert_eq!((idle.phase, idle.phase_authoritative), (Phase::Idle, true));
    assert_eq!(idle.composer.map(|(_, l)| l), Some(vec![String::new()]));
    let rows = screen(HINT_OFF_IDLE);
    assert!(
        rows[status_at(&rows)].trim().is_empty(),
        "the blank status row"
    );
    let busy = gemini(HINT_OFF_BUSY);
    assert_eq!((busy.phase, busy.phase_authoritative), (Phase::Busy, true));
    let ended = gemini(HINT_OFF_END_OF_TURN);
    assert_eq!(
        (ended.phase.clone(), ended.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(ended.said_tail.is_some_and(|t| t.ends_with("tick 30")));
    let mut worded = rows.clone();
    worded[status_at(&rows)] = " Lab stub thinking (esc to cancel, 1s)".to_string();
    assert_eq!(phase(&worded), (Phase::Idle, false), "the control");
}

/// SEVERAL QUESTIONS (measured, two): each tab is a question box whose
/// question is the row under the tab row (the tab row its note), and its
/// options carry NO role — such a dialog is a person's whole (module
/// header); its review tab, with no numbered option, is a question box with
/// none, its answers its notes, `Enter to submit · …` its key row; submitted,
/// the turn ends on the tool's row.
#[test]
fn several_questions_are_read_tab_by_tab_and_left_to_a_person() {
    for (text, asks) in [
        (QUESTION_MULTI, "Which colour should the lab probe use?"),
        (QUESTION_MULTI_SECOND, "How big should the lab probe be?"),
    ] {
        let what = provenance(text).unwrap_or_default();
        let r = gemini(text);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
        let p = r.prompt.expect("the question");
        assert_eq!(p.kind, PromptKind::Question, "{what}");
        assert_eq!(p.description, asks, "{what}");
        assert!(is_tab_row(&p.notes[0]), "{what}: {:?}", p.notes);
        assert_eq!(labels(&p).len(), 3, "{what}");
        assert!(roles(&p).iter().all(|&r| r == Role::Other), "{what}");
    }
    let r = gemini(QUESTION_MULTI_REVIEW);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    assert_eq!(
        identify(None, &screen(QUESTION_MULTI_REVIEW)).program(),
        Program::Gemini
    );
    let p = r.prompt.expect("the review");
    assert_eq!(p.kind, PromptKind::Question);
    assert!(p.options.is_empty());
    assert_eq!(p.description, anchor_text("gemini.question.review"));
    assert!(p.notes.iter().any(|n| n == "Colour → Red"), "{:?}", p.notes);
    assert!(p.notes.iter().any(|n| n == "Size → Small"), "{:?}", p.notes);
    assert!(
        p.footer
            .as_deref()
            .is_some_and(|f| f.starts_with(anchor_text("gemini.question.submit")))
    );
    let answered = gemini(QUESTION_MULTI_ANSWERED);
    assert_eq!(
        (answered.phase, answered.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(answered.said_tail.as_deref(), Some("Tool call done."));

    let single = gemini(QUESTION).prompt.expect("the question");
    assert!(single.notes.is_empty());
    assert_eq!(roles(&single)[0], Role::Answer, "the control");
    let reworded = edited(
        &screen(QUESTION_MULTI_REVIEW),
        "│ Review your answers:",
        &format!("│ {:<140} │", "Look over your answers:"),
    );
    assert_eq!(prompt(&reworded), None, "no option, no review");
}

/// THE MCP, FETCH AND SANDBOX-EXPANSION BOXES (measured: a local stdio MCP
/// server, a `web_fetch` call, a shell call naming `additional_permissions`
/// with no sandbox on): each a box of kind Other, named Gemini CLI's by its
/// one `Allow once`, read whole — the MCP and fetch boxes' title the tool's
/// row under `Action Required`, each question kept, the expansion's command
/// and its `• …` rows — with `Allow once` the one-shot, every `… for this
/// session` a standing grant and `No, suggest changes (esc)` the refusal.
#[test]
fn the_mcp_fetch_and_expansion_boxes_are_read_whole() {
    let mcp = gemini(BOX_MCP).prompt.expect("the MCP box");
    assert_eq!(mcp.kind, PromptKind::Other);
    assert!(
        mcp.title.starts_with("lab_echo (lab MCP Server)"),
        "{}",
        mcp.title
    );
    assert_eq!(
        mcp.description,
        "Allow execution of MCP tool \"lab_echo\" from server \"lab\"?"
    );
    assert_eq!(
        roles(&mcp),
        vec![Role::Once, Role::Persist, Role::Persist, Role::Deny]
    );
    assert!(mcp.cancel.is_some());
    let fetch = gemini(BOX_WEB_FETCH).prompt.expect("the fetch box");
    assert_eq!(fetch.kind, PromptKind::Other);
    assert!(
        fetch.title.starts_with("WebFetch Processing URLs"),
        "{}",
        fetch.title
    );
    assert_eq!(fetch.description, anchor_text("gemini.box.proceed"));
    assert_eq!(roles(&fetch), vec![Role::Once, Role::Persist, Role::Deny]);
    let wide = gemini(BOX_SANDBOX_EXPANSION).prompt.expect("the expansion");
    assert_eq!(wide.kind, PromptKind::Other);
    assert_eq!(
        wide.description,
        "To run [Shell], allow access to the following?"
    );
    assert_eq!(wide.command, "touch lab-sandbox.txt");
    assert_eq!(
        wide.notes,
        vec![
            "• Network: All Urls",
            "• Write: /tmp/lab-write",
            "• Read: /tmp/lab-read"
        ]
    );
    assert_eq!(roles(&wide), vec![Role::Once, Role::Persist, Role::Deny]);
    for text in [BOX_MCP, BOX_WEB_FETCH, BOX_SANDBOX_EXPANSION] {
        let rows = screen(text);
        assert_eq!(identify(None, &rows).program(), Program::Gemini);
        assert_eq!(phase(&rows), (Phase::Prompt, true));
    }

    let relabelled = edited(
        &screen(BOX_MCP),
        "│   2. Allow tool for this session",
        &format!("│ {:<140} │", "  2. Allow tool again later"),
    );
    let p = prompt(&relabelled).expect("still the box");
    assert_eq!(p.options[1].role, Role::Other, "the control");
    let reworded = edited(
        &screen(BOX_SANDBOX_EXPANSION),
        "│ To run",
        &format!("│ {:<140} │", "To go on, widen the sandbox?"),
    );
    let p = prompt(&reworded).expect("still a box");
    assert_eq!((p.question, p.description.as_str()), (None, ""));
}

/// IN A NARROW PANE, A 401 AND A 403 (measured at 72 columns): each error
/// wraps over rows two columns in, its notice too, and is still the turn's
/// wall — the 401 the login, the 403 `PERMISSION_DENIED` (`SERVICE_DISABLED`)
/// an API error the CLI does not retry.
#[test]
fn narrow_errors_and_the_no_access_dialog() {
    let r401 = gemini(NARROW_ERROR_401);
    assert_eq!(r401.wall.map(|w| w.kind), Some(WallKind::Auth));
    assert_eq!((r401.phase, r401.phase_authoritative), (Phase::Idle, true));
    let w = gemini(NARROW_ERROR_403).wall.expect("the 403");
    assert_eq!(
        w.kind,
        WallKind::ApiError {
            code: Some(403),
            retryable: false,
            cause: ApiCause::Server
        }
    );
    assert!(w.message.contains("SERVICE_DISABLED"));

    let rows = screen(NARROW_NO_ACCESS);
    let r = gemini(NARROW_NO_ACCESS);
    assert_eq!((r.phase, r.phase_authoritative), (Phase::Prompt, true));
    let p = r.prompt.expect("the dialog");
    assert_eq!(p.kind, PromptKind::Other);
    assert!(
        p.description
            .starts_with("It seems like you don't have access to gemini-3.5-flash."),
        "{}",
        p.description
    );
    assert_eq!(labels(&p), vec![anchor_text("gemini.usage.retry"), "Stop"]);
    assert_eq!(roles(&p), vec![Role::Other, Role::Other]);
    assert_eq!(identify(None, &rows).program(), Program::Gemini);
    let stopped = gemini(NARROW_NO_ACCESS_STOPPED).wall.expect("the error");
    assert_eq!(
        stopped.kind,
        WallKind::ApiError {
            code: None,
            retryable: false,
            cause: ApiCause::Server
        }
    );
    let reworded = edited(
        &rows,
        "│ It seems like",
        &format!("│ {:<68} │", "No model for you today."),
    );
    assert_ne!(
        identify(None, &reworded).program(),
        Program::Gemini,
        "the control"
    );
}

/// A BOX KEPT OVER THE COMPOSER (`ui.collapseDrawerDuringApproval = false`,
/// measured): the shell box stays in the transcript over a composer still
/// drawn and focused, its status row blank — with the composer empty, with
/// a draft typed into it, with that draft queued under the box by an Enter,
/// and with the UI details hidden too.
#[test]
fn a_box_kept_over_the_composer_is_a_persons_prompt() {
    for text in [
        DRAWER_KEPT_BOX_SHELL,
        DRAWER_KEPT_BOX_SHELL_DRAFT,
        DRAWER_KEPT_BOX_SHELL_QUEUED,
        DRAWER_KEPT_DETAILS_HIDDEN_BOX_SHELL,
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = gemini(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true),
            "{what}"
        );
        let p = r.prompt.expect("the kept box");
        assert_eq!(
            (p.kind, p.command.as_str()),
            (PromptKind::Bash, "touch lab-drawer.txt"),
            "{what}"
        );
        assert_eq!(
            labels(&p),
            vec![
                anchor_text("gemini.box.once"),
                anchor_text("gemini.box.session"),
                anchor_text("gemini.box.deny")
            ],
            "{what}"
        );
        assert!(p.options[0].focused, "{what}");
        assert!(roles(&p).iter().all(|&r| r == Role::Other), "{what}");
        assert_eq!(identify(Some("node"), &rows).program(), Program::Gemini);

        let top = rows
            .iter()
            .rposition(|r| r.starts_with('╭'))
            .expect("the box's top");
        let bottom = rows
            .iter()
            .rposition(|r| r.starts_with('╰'))
            .expect("the box's bottom");
        let mut gone = rows.clone();
        for row in &mut gone[top..=bottom] {
            row.clear();
        }
        assert_eq!(phase(&gone), (Phase::Idle, true), "{what}: the control");
        assert_eq!(prompt(&gone), None, "{what}");
        let finished: Vec<String> = rows.iter().map(|r| r.replace("│ ● 1.", "│   1.")).collect();
        assert_ne!(finished, rows, "{what}");
        assert_eq!(
            prompt(&finished),
            None,
            "{what}: no option under the cursor"
        );
        assert_ne!(phase(&finished).0, Phase::Prompt, "{what}");
    }
    let plain = gemini(BOX_SHELL).prompt.expect("the shell box");
    assert_eq!(
        roles(&plain),
        vec![Role::Once, Role::Persist, Role::Deny],
        "the control"
    );
}

/// UNDER ITS OWN SANDBOX (measured outside the lab's jail: its seatbelt,
/// `GEMINI_SANDBOX=sandbox-exec`, and its per-tool one,
/// `security.toolSandboxing`, at 144 and 72 columns): the footer's sandbox
/// column says `current process` or `all tools`, and a fresh session is
/// still idle as evidence at its empty composer — `? for shortcuts` in a
/// wide pane, a blank status row in a narrow one; a turn under the seatbelt
/// is busy on its spinner and ends on its reply.
#[test]
fn a_session_under_its_sandbox_reads_as_without_it() {
    for (text, says) in [
        (SANDBOX_IDLE, "current process"),
        (SANDBOX_TOOLS_IDLE, "all tools"),
        (SANDBOX_NARROW_IDLE, "current process"),
        (SANDBOX_TOOLS_NARROW_IDLE, "all tools"),
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = gemini(text);
        assert_eq!(identify(None, &rows).program(), Program::Gemini, "{what}");
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert!(r.fresh, "{what}");
        assert_eq!(
            r.composer,
            Some((caret_at(&rows), vec![String::new()])),
            "{what}"
        );
        let footer = rows.len() - 1;
        assert!(rows[footer].contains(says), "{what}: {:?}", rows[footer]);
        assert!(
            rows[footer - 1].contains(anchor_text("gemini.footer.workspace")),
            "{what}"
        );
        let mut moved = rows.clone();
        moved[status_at(&rows)] = format!(" {says}");
        assert_eq!(phase(&moved), (Phase::Idle, false), "{what}: the control");
    }
    let busy = gemini(SANDBOX_BUSY);
    assert_eq!((busy.phase, busy.phase_authoritative), (Phase::Busy, true));
    let ended = gemini(SANDBOX_END_OF_TURN);
    assert_eq!(
        (ended.phase.clone(), ended.phase_authoritative),
        (Phase::Idle, true)
    );
    assert_eq!(
        ended.said_tail.as_deref(),
        Some(
            "This reply is streaming slowly from the fake backend so the busy state stays on \
             screen for a while before it finishes. Done."
        )
    );
    assert!(!ended.interrupted && ended.wall.is_none());
}

/// A SHELL COMMAND THE SANDBOX DENIED, with nothing offered (measured: a
/// write outside the project and a `curl` to a non-loopback address under
/// its seatbelt; the same write once a sandbox expansion was allowed, which
/// widens nothing the process's own profile denies; a `touch` with both
/// sandboxes on, whose per-tool `sandbox-exec` cannot apply inside the
/// other): the tool's row is `✓` — the command ran and exited non-zero, no
/// tool error — with the denial in its box, and the turn ends on the
/// model's reply: idle as evidence, its last words the reply, no wall, no
/// person's stop, no box.
#[test]
fn a_command_its_sandbox_denied_ends_the_turn_on_the_reply() {
    for (text, denial) in [
        (SANDBOX_DENIED, "Operation not permitted"),
        (SANDBOX_DENIED_NETWORK, "Couldn't connect to server"),
        (SANDBOX_RETRY_ALLOWED, "Operation not permitted"),
        (
            SANDBOX_NESTED_DENIED,
            "sandbox-exec: sandbox_apply: Operation not permitted",
        ),
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = gemini(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert_eq!((r.wall, r.prompt), (None, None), "{what}");
        assert!(!r.interrupted && !r.fresh, "{what}");
        let said = r.said_tail.expect("the reply");
        assert!(
            said.starts_with("Done. run_shell_command returned"),
            "{what}: {said}"
        );
        let tool = rows
            .iter()
            .rposition(|row| row.starts_with("│ ✓  Shell "))
            .expect("the tool's row");
        assert!(
            rows[tool + 1..tool + 4]
                .iter()
                .any(|row| row.contains(denial)),
            "{what}"
        );
        let reply = rows
            .iter()
            .rposition(|row| row.starts_with("✦ "))
            .expect("the reply");
        let mut unsaid = rows.clone();
        for row in &mut unsaid[reply..status_at(&rows)] {
            row.clear();
        }
        assert_eq!(said_tail(&unsaid), None, "{what}: the control");
        assert_eq!(phase(&unsaid), (Phase::Idle, true), "{what}");
    }
}

/// THE SANDBOX-EXPANSION BOX OFFERED WITH THE SANDBOX ON (measured: after
/// the per-tool seatbelt denied a write outside the project, at 144, 100 and
/// 72 columns, `• Write:` and `• Read:` the directory above the denied path;
/// before a `curl` runs, `• Network: All Urls`; after the process's seatbelt
/// denied the write, for the model's next call naming
/// `additional_permissions`, under the denied call's finished row): each a
/// box of kind Other named Gemini CLI's by its `Allow once`, read whole —
/// the command in the box drawn inside it, the question, the `• …` rows as
/// notes — `Allow once` the one-shot, `Allow for this session` a standing
/// grant, `No, suggest changes (esc)` the refusal and its Esc, chosen by
/// digit.
#[test]
fn the_expansion_box_after_a_denial_is_read_whole() {
    let write = "touch /Users//Shared/gemini-sandbox-probe.txt";
    let both = vec!["• Write: /Users//Shared", "• Read: /Users//Shared"];
    for (text, command, notes) in [
        (SANDBOX_TOOLS_EXPANSION, write, both.clone()),
        (SANDBOX_TOOLS_EXPANSION_100, write, both.clone()),
        (SANDBOX_TOOLS_NARROW_EXPANSION, write, both.clone()),
        (
            SANDBOX_TOOLS_NETWORK,
            "curl --noproxy '*' -sS -m 5 http://192.0.2.1/",
            vec!["• Network: All Urls"],
        ),
        (
            SANDBOX_RETRY_EXPANSION,
            write,
            vec!["• Write: /Users//Shared"],
        ),
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = gemini(text);
        assert_eq!(identify(None, &rows).program(), Program::Gemini, "{what}");
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Prompt, true),
            "{what}"
        );
        assert_eq!((r.composer, r.wall), (None, None), "{what}");
        let p = r.prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::Other, "{what}");
        assert_eq!(p.title, format!("Shell  {command}"), "{what}");
        assert_eq!(p.command, command, "{what}");
        assert_eq!(
            p.description, "To run [Shell], allow access to the following?",
            "{what}"
        );
        assert_eq!(p.notes, notes, "{what}");
        assert_eq!(
            labels(&p),
            vec![
                anchor_text("gemini.box.once"),
                anchor_text("gemini.box.session"),
                anchor_text("gemini.box.deny")
            ],
            "{what}"
        );
        assert_eq!(
            roles(&p),
            vec![Role::Once, Role::Persist, Role::Deny],
            "{what}"
        );
        assert_eq!(p.select, Select::Digits, "{what}");
        assert_eq!(esc(&p), Some(CancelEffect::Reject), "{what}");
        assert!(
            rows[p.span.0].contains(&format!("? Shell  {command}")),
            "{what}"
        );

        let top = rows
            .iter()
            .rposition(|row| row.starts_with('╭'))
            .expect("the box's top");
        let mut gone = rows.clone();
        for row in &mut gone[top..] {
            row.clear();
        }
        assert_eq!(prompt(&gone), None, "{what}: the control");
        let ask = p.question.expect("its question");
        let width = rows[ask].chars().count();
        let mut reworded = rows.clone();
        reworded[ask] = format!("│ {:<w$} │", "To go on, widen the sandbox?", w = width - 4);
        let p = prompt(&reworded).expect("still a box");
        assert_eq!((p.question, p.description.as_str()), (None, ""), "{what}");
    }
}

/// A SANDBOX EXPANSION REFUSED WITH ESC (measured).
#[test]
fn a_refused_expansion_is_the_persons_stop_or_the_models_turn() {
    for text in [SANDBOX_RETRY_REFUSED, SANDBOX_TOOLS_NETWORK_REFUSED] {
        let what = provenance(text).unwrap_or_default();
        let r = gemini(text);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Idle, true));
        assert!(r.interrupted, "{what}");
        assert_eq!((r.said_tail, r.wall, r.prompt), (None, None, None));
        let rows = screen(text);
        let cancelled = rows
            .iter()
            .rposition(|row| row == "ℹ Request cancelled.")
            .expect("the cancel row");
        let mut uncancelled = rows.clone();
        uncancelled[cancelled] = String::new();
        assert!(!interrupted(&uncancelled), "{what}: the control");
    }
    for text in [
        SANDBOX_TOOLS_EXPANSION_REFUSED,
        SANDBOX_TOOLS_EXPANSION_REFUSED_100,
        SANDBOX_TOOLS_NARROW_EXPANSION_REFUSED,
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let r = gemini(text);
        assert_eq!(
            (r.phase.clone(), r.phase_authoritative),
            (Phase::Idle, true),
            "{what}"
        );
        assert!(!r.interrupted, "{what}");
        assert_eq!((r.wall, r.prompt), (None, None), "{what}");
        assert!(
            !rows.iter().any(|row| row.contains("Shell touch")),
            "{what}: no row for the call"
        );
        let said = r.said_tail.expect("the reply");
        assert!(
            said.starts_with("Done. run_shell_command returned {\"error\"")
                && said.contains("sandbox_expansion"),
            "{what}: {said}"
        );
        let mut cancelled = rows.clone();
        cancelled.insert(status_at(&rows), "ℹ Request cancelled.".to_string());
        assert!(interrupted(&cancelled), "{what}: the control");
    }
}

/// A REPLY TALLER THAN THE SCREEN IS NO WHOLE REPLY ([`said_whole`]): its
/// `✦` head scrolled above the rows read, its last words are the rows in
/// sight, and the need its head said is not in them — so a reader of a
/// landing knows to read the rest (the recheck of d88a7bf289 found only
/// Claude Code's cut read, and a tall report here land on its last rows).
#[test]
fn a_reply_whose_head_scrolled_away_is_no_whole_reply() {
    let rows = screen(END_OF_TURN);
    let at = rows
        .iter()
        .rposition(|r| r.starts_with("✦ "))
        .expect("the reply");
    let mut drawn = rows[..at].to_vec();
    drawn.push("✦ NEEDS YOU: the signing credential".to_string());
    drawn.extend((0..40).map(|i| format!("  - merged item {i}")));
    drawn.push("  Nothing left: all of it is merged.".to_string());
    drawn.extend(rows[at + 1..].iter().cloned());
    let cut = drawn[drawn.len() - rows.len()..].to_vec();
    let tail = said_tail(&cut).expect("the rows in sight");
    assert!(
        tail.ends_with("Nothing left: all of it is merged.") && !tail.contains("NEEDS YOU"),
        "{tail}"
    );
    assert!(!said_whole(&cut) && !Program::Gemini.reader().said_whole(&cut));
    assert!(said_whole(&drawn) && Program::Gemini.reader().said_whole(&drawn));
    assert!(said_tail(&drawn).is_some_and(|t| t.starts_with("NEEDS YOU: the signing")));
}

/// A NEED SAID BEFORE A TOOL CALL IS THE TURN'S ([`turn_said_before`]): the
/// `✦` reply above a tool's box, under the person's band, is read with the
/// turn its last reply ends — a `NEEDS YOU:` there stands however the last
/// reply says nothing is left (the recheck of ea564676a3 found the turn's
/// earlier words read on Claude Code alone).
#[test]
fn a_need_said_before_a_tool_call_is_read_with_the_turn() {
    let rows = screen(END_OF_TURN);
    let at = rows
        .iter()
        .rposition(|r| r.starts_with("✦ "))
        .expect("the reply");
    let turn = |before: &[&str]| {
        let mut drawn = rows[..at].to_vec();
        drawn.extend(before.iter().map(|r| (*r).to_string()));
        drawn.extend(
            [
                "",
                "╭──────────────────────────────────────╮",
                "│ ✓  Shell git push origin main        │",
                "╰──────────────────────────────────────╯",
                "",
                "✦ Nothing left: all of it is merged.",
            ]
            .map(str::to_string),
        );
        drawn.extend(rows[at + 1..].iter().cloned());
        drawn
    };
    let reader = Program::Gemini.reader();
    let drawn = turn(&[
        "✦ NEEDS YOU: the signing credential, to sign the release.",
        "  I will push the branch now.",
    ]);
    let need =
        "NEEDS YOU: the signing credential, to sign the release.\nI will push the branch now.";
    assert_eq!(turn_said_before(&drawn).as_deref(), Some(need));
    assert_eq!(reader.turn_said_before(&drawn).as_deref(), Some(need));
    assert_eq!(
        said_tail(&drawn).as_deref(),
        Some("Nothing left: all of it is merged.")
    );
    assert!(turn_whole(&drawn) && reader.turn_whole(&drawn));
    let band = drawn
        .iter()
        .position(|r| r == " > think slowly")
        .expect("the band");
    let cut = drawn[band + 2..].to_vec();
    assert!(said_whole(&cut) && !turn_whole(&cut) && !reader.turn_whole(&cut));
    assert!(turn_history_stop(&drawn[band - 1]) && reader.turn_history_stop(&drawn[band - 1]));
    for row in &drawn[band..band + 4] {
        assert!(!turn_history_stop(row), "{row}");
    }
    let plain = turn(&[]);
    assert_eq!(turn_said_before(&plain), None);
    assert!(turn_whole(&plain) && said_tail(&plain).is_some());
}

/// THE MESSAGE THAT OPENED THE TURN IS READ OFF ITS BAND ([`turn_opened_by`]):
/// the sent row's text past its ` > `, and the rows it wrapped onto three
/// columns in, joined — what tells a successor whether its land act opened
/// the turn whose defense stands (the review of b8/residuals found a
/// defense said under a person's earlier message landed on Gemini CLI,
/// nothing reading the band).
#[test]
fn the_message_that_opened_the_turn_is_read_off_its_band() {
    let rows = screen(END_OF_TURN);
    let reader = Program::Gemini.reader();
    assert_eq!(turn_opened_by(&rows).as_deref(), Some("think slowly"));
    assert_eq!(
        reader.turn_opened_by(&rows).as_deref(),
        Some("think slowly")
    );
    let band = rows
        .iter()
        .position(|r| r == " > think slowly")
        .expect("the band");
    let mut wrapped = rows.clone();
    wrapped[band] = " > [aterm harness] Autopilot is Land: land ALL".to_string();
    wrapped.insert(
        band + 1,
        "   of this work into the remote main branch".to_string(),
    );
    assert!(said_tail(&wrapped).is_some());
    assert_eq!(
        turn_opened_by(&wrapped).as_deref(),
        Some(
            "[aterm harness] Autopilot is Land: land ALL of this work into the remote main branch"
        )
    );
    let cut = rows[band + 2..].to_vec();
    assert!(said_tail(&cut).is_some());
    assert_eq!(turn_opened_by(&cut), None);
    assert_eq!(ClaudeReader.turn_opened_by(&rows), None);
}

/// THE FRAME A LAUNCH DRAWS BEFORE ITS BANNER (measured in the forever lab,
/// P3 of `docs/measured/forever-lab-2026-10-20.md`): the composer, its
/// footer and `? for shortcuts` with nothing above them — idle as evidence,
/// no message, no reply, no opener, an empty composer, and NOT `fresh`: no
/// banner row stands above the composer yet, so the screen alone cannot tell
/// it from a conversation scrolled or cleared away (the supervisor reads the
/// host's launch for that).
#[test]
fn the_frame_before_the_banner_is_idle_with_no_turn_and_not_fresh() {
    let r = gemini(PRE_BANNER);
    let rows = screen(PRE_BANNER);
    assert_eq!(r.program, Program::Gemini);
    assert_eq!(
        (r.phase.clone(), r.phase_authoritative),
        (Phase::Idle, true)
    );
    assert!(!r.fresh, "no banner row above the composer");
    assert_eq!((r.prompt, r.wall, r.said_tail), (None, None, None));
    assert!(!r.interrupted);
    assert_eq!(r.composer, Some((caret_at(&rows), vec![String::new()])));
    assert_eq!(turn_opened_by(&rows), None);
    assert!(!rows.iter().any(|row| is_banner_row(row)));
    assert!(gemini(IDLE).fresh, "the banner drawn");
    let ended = gemini(END_OF_TURN);
    assert!(!ended.fresh);
    assert!(ended.said_tail.is_some());
    assert!(turn_opened_by(&screen(END_OF_TURN)).is_some());
}

/// GEMINI CLI'S TOP LINE ([`top_rule`], the row aterm's glass marks the
/// program on): the live zone's `─` rule over the approval mode, as wide as
/// the composer's band, in every capture whose composer has one — at rest,
/// with a draft, busy, at a turn's end, narrow, under YOLO, under its
/// sandbox, with a box kept over the composer, and with the UI details
/// hidden under a non-default mode, which brings the rule back (module
/// header) — and none with the UI details hidden at the default mode, nor
/// where a box replaced the live zone.
#[test]
fn gemini_top_rule_is_the_live_zone_rule() {
    /// The rule `top` names on `rows`, proven: a plain rule, the band of its
    /// width under it within reach, the caret on the band's next row.
    fn proven(rows: &[String], top: usize) -> bool {
        plain_rule_width(&rows[top]).is_some_and(|w| {
            (top + 1..=(top + UNDER_RULE_ROWS + 1).min(rows.len() - 1)).any(|t| {
                run_of(&rows[t], BAND_TOP) == Some(w)
                    && rows.get(t + 1).is_some_and(|r| is_caret_row(r))
            })
        })
    }
    for text in [
        IDLE,
        IDLE_CONTEXT,
        DRAFT,
        END_OF_TURN,
        BUSY_THINKING,
        BUSY_STREAMING,
        NARROW_IDLE,
        NARROW_DRAFT,
        NARROW_BUSY,
        NARROW_END_OF_TURN,
        YOLO_IDLE,
        YOLO_DRAFT,
        YOLO_BUSY,
        YOLO_END_OF_TURN,
        HINT_OFF_IDLE,
        SANDBOX_IDLE,
        SANDBOX_NARROW_IDLE,
        DRAWER_KEPT_BOX_SHELL,
        DETAILS_HIDDEN_YOLO_IDLE,
    ] {
        let what = provenance(text).unwrap_or_default();
        let rows = screen(text);
        let top = top_rule(&rows).unwrap_or_else(|| panic!("a top rule: {what}"));
        assert!(proven(&rows, top), "{what}: row {top} {:?}", rows[top]);
        assert_eq!(
            Some(top),
            rows.iter().rposition(|r| plain_rule_width(r).is_some()),
            "the last plain rule drawn: {what}"
        );
    }
    for text in [
        DETAILS_HIDDEN_IDLE,
        DETAILS_HIDDEN_DRAFT,
        DETAILS_HIDDEN_BUSY,
        DETAILS_HIDDEN_END_OF_TURN,
        DETAILS_HIDDEN_YOLO_DRAFT,
        DETAILS_HIDDEN_BOX_SHELL,
        HINT_OFF_DETAILS_HIDDEN_IDLE,
        BOX_SHELL,
        BOX_WRITE,
        QUESTION,
        TRUST,
        EXIT,
    ] {
        let what = provenance(text).unwrap_or_default();
        assert_eq!(top_rule(&screen(text)), None, "{what}");
    }
    for (name, text) in captures() {
        let rows = screen(&text);
        if let Some(top) = top_rule(&rows) {
            assert!(proven(&rows, top), "{name}: row {top} {:?}", rows[top]);
        }
    }

    let rows = screen(IDLE);
    let band = caret_at(&rows) - 1;
    assert_eq!(
        run_of(&rows[band], BAND_TOP),
        plain_rule_width(&rows[top_rule(&rows).expect("the rule")])
    );
    assert!(!proven(&rows, band), "the band's top is no rule");
    let mut unruled = rows.clone();
    unruled[top_rule(&rows).expect("the rule")] = String::new();
    assert_eq!(top_rule(&unruled), None, "the band alone");
    let rows = screen(BUSY_STREAMING);
    let live = top_rule(&rows).expect("the live zone's rule");
    let sent = rows
        .iter()
        .position(|r| r.starts_with(" > think slowly"))
        .expect("the sent message")
        - 1;
    assert!(run_of(&rows[sent], BAND_TOP).is_some() && sent < live);
    let mut unruled = rows.clone();
    unruled[live] = String::new();
    assert_eq!(top_rule(&unruled), None, "the sent band, no rule over it");
    let mut planted = rows.clone();
    planted[sent - 1] = "─".repeat(run_of(&rows[sent], BAND_TOP).expect("its width"));
    assert_eq!(top_rule(&planted), Some(live), "a rule over the sent band");
}

/// THE ERROR A DIALOG'S `Stop` LEAVES ([`stopped_error`]): the usage-limit
/// dialog's answered `2. Stop`, at 144 columns and wrapped at 72, and the
/// no-access dialog's — `[API Error: An unknown error occurred.]`, no code,
/// not retried.
#[test]
fn the_error_a_dialogs_stop_leaves_is_read_as_such() {
    for f in [
        LIMIT_STOPPED,
        NARROW_LIMIT_STOPPED,
        NARROW_NO_ACCESS_STOPPED,
    ] {
        let w = wall(&screen(f)).expect("the turn's error");
        assert!(stopped_error(&w), "{w:?}");
    }
    for f in [ERROR_401, ERROR_400_KEY, NARROW_ERROR_403] {
        let w = wall(&screen(f)).expect("the turn's error");
        assert!(!stopped_error(&w), "{w:?}");
    }
    let mut w = wall(&screen(LIMIT_STOPPED)).expect("the turn's error");
    w.kind = WallKind::ApiError {
        code: Some(503),
        retryable: true,
        cause: ApiCause::Server,
    };
    assert!(!stopped_error(&w));
}

/// A LONG PASTE IS DRAWN AS ITS CHIP ([`folded_paste`]): on the measured
/// screen ([`FOLDED_PASTE`]) the composer holds `[Pasted Text: 538 chars]`
/// alone, which is what `folded_paste` says for any 538-character text, and
/// Gemini CLI's reader says the same ([`ScreenReader::folded_paste`]).
#[test]
fn a_long_paste_is_drawn_as_its_chip() {
    let rows = screen(FOLDED_PASTE);
    let (caret, lines) = composer_draft(&rows).expect("the composer");
    assert_eq!(rows[caret], " > [Pasted Text: 538 chars]");
    assert_eq!(lines, ["[Pasted Text: 538 chars]"]);
    let text = "x".repeat(538);
    assert_eq!(folded_paste(&text).as_deref(), Some(lines[0].as_str()));
    assert_eq!(
        crate::reader::GeminiReader.folded_paste(&text).as_deref(),
        Some(lines[0].as_str())
    );

    assert_eq!(folded_paste(&"x".repeat(500)), None);
    assert_eq!(
        folded_paste(&"x".repeat(501)).as_deref(),
        Some("[Pasted Text: 501 chars]")
    );
    assert_eq!(folded_paste(&"\u{1F600}".repeat(250)), None);
    assert_eq!(
        folded_paste(&"\u{1F600}".repeat(251)).as_deref(),
        Some("[Pasted Text: 502 chars]")
    );
    assert_eq!(folded_paste(&format!("{}\r\n", "x".repeat(499))), None);
    assert_eq!(
        folded_paste(&format!("{}\r\n", "x".repeat(500))).as_deref(),
        Some("[Pasted Text: 501 chars]")
    );
    assert_eq!(folded_paste("a\nb\nc\nd\ne"), None);
    assert_eq!(
        folded_paste("a\r\nb\rc\nd\ne\nf").as_deref(),
        Some("[Pasted Text: 6 lines]")
    );
    for reader in [
        &ClaudeReader as &dyn ScreenReader,
        &CodexReader,
        &AgyReader,
        &MuseReader,
    ] {
        assert_eq!(reader.folded_paste(&text), None, "{:?}", reader.program());
    }
}

/// THE SEAT GEMINI CLI'S SCREEN NAMES: its footer's model ([`footer_model`],
/// `Auto` as measured, the chosen model's name in its place) and its
/// banner's login ([`banner_login`], `Authenticated with gemini-api-key` as
/// measured, `Signed in with Google: <email>` as the bundle draws it).
/// NEGATIVE CONTROLS: a screen with no footer names no model (the dialog's
/// own rows are no footer), a screen whose banner scrolled off names no
/// login, and the footer's other columns are no model.
#[test]
fn the_footer_names_its_model_and_the_banner_its_login() {
    let idle = screen(IDLE);
    assert_eq!(footer_model(&idle).as_deref(), Some("Auto"));
    assert_eq!(
        footer_model(&screen(LIMIT_STOPPED)).as_deref(),
        Some("Auto")
    );
    let at = idle
        .iter()
        .rposition(|r| r.contains(anchor_text("gemini.footer.workspace")))
        .expect("the footer")
        + 1;
    let mut chosen = idle.clone();
    chosen[at] = idle[at].replacen("          Auto", "gemini-3.5-pro", 1);
    assert_eq!(footer_model(&chosen).as_deref(), Some("gemini-3.5-pro"));
    let no_footer: Vec<String> = idle[..at - 1].to_vec();
    assert_eq!(footer_model(&no_footer), None);
    assert_ne!(footer_model(&idle).as_deref(), Some("no sandbox"));

    assert_eq!(
        banner_login(&idle).as_deref(),
        Some("Authenticated with gemini-api-key")
    );
    let google: Vec<String> = idle
        .iter()
        .map(|r| {
            r.replacen(
                "Authenticated with gemini-api-key",
                "Signed in with Google: user@example.com",
                1,
            )
        })
        .collect();
    assert_eq!(
        banner_login(&google).as_deref(),
        Some("Signed in with Google: user@example.com")
    );
    let scrolled: Vec<String> = idle
        .iter()
        .filter(|r| !r.contains(anchor_text("gemini.banner.auth.tail").trim()))
        .cloned()
        .collect();
    assert_eq!(banner_login(&scrolled), None);
}

/// GEMINI CLI'S TRUST NOTICES ARE CHROME UNDER A REPLY
/// ([`trust_notices_above`]): on the screen of a worker aterm relaunched on
/// its session after its own restart (measured), the defense `✦ Nothing
/// left: …` stands above `⚠  Note: This folder is still trusted because …`
/// and `ℹ This folder is untrusted, …` with its `/permissions` row; the
/// turn's last words are the defense, whole, and the turn is the Land act's.
#[test]
fn trust_notices_under_a_resumed_reply_are_chrome() {
    let rows = screen(LAB_RESUMED_TRUST_NOTICES);
    assert!(provenance(LAB_RESUMED_TRUST_NOTICES).is_some_and(|p| p.contains("MEASURED")));
    let defense = "Nothing left: the parser is committed and pushed to origin/main.";
    assert_eq!(said_tail(&rows).as_deref(), Some(defense));
    assert!(said_whole(&rows) && turn_whole(&rows));
    assert!(
        turn_opened_by(&rows).is_some_and(|m| m.starts_with("[aterm harness] Autopilot is Land")),
        "{:?}",
        turn_opened_by(&rows)
    );
    let r = gemini(LAB_RESUMED_TRUST_NOTICES);
    assert_eq!(r.said_tail.as_deref(), Some(defense));
    assert_eq!(r.phase, Phase::Idle);
    let untrusted = rows
        .iter()
        .position(|r| r.starts_with("ℹ This folder is untrusted"))
        .expect("the untrusted notice");
    let mut narrow = rows.clone();
    narrow.splice(
        untrusted..=untrusted,
        [
            "ℹ This folder is untrusted, project settings, hooks, MCPs,".to_string(),
            "  and GEMINI.md files will not be applied for this folder.".to_string(),
        ],
    );
    assert_eq!(said_tail(&narrow).as_deref(), Some(defense));
    let mut cancelled = rows.clone();
    let reply = rows
        .iter()
        .rposition(|r| r.starts_with("✦ Nothing left"))
        .expect("the defense");
    cancelled.splice(
        reply + 1..reply + 1,
        ["".to_string(), "ℹ Request cancelled.".to_string()],
    );
    cancelled.drain(cancelled.len() - 2..);
    assert!(interrupted(&cancelled));
    let other = edited(
        &rows,
        "⚠  Note:",
        "⚠  Note: the loop detector stopped this turn.",
    );
    assert_eq!(said_tail(&other), None);
    let mut broken = rows.clone();
    broken[untrusted + 1] = "Use the /permissions command to change the trust level.".to_string();
    assert_eq!(said_tail(&broken), None);
}

/// A RAW WRITE IS NO PASTE ([`folded_paste`]): 600 and 1600 characters of
/// one line sent raw (the fenced write's `send`, no paste brackets) are drawn
/// in the composer as the words typed, wrapped over its rows (measured), so the
/// fenced write's comparison of the composer with its words holds past the
/// fold's 500 units.
#[test]
fn a_raw_send_longer_than_the_fold_is_drawn_as_words() {
    assert!(provenance(LAB_RAW_SEND_600).is_some_and(|p| p.contains("MEASURED")));
    let phrase =
        "[lab raw send] this is a long raw write of plain words to measure the paste fold ";
    let sent: String = phrase.repeat(8).chars().take(600).collect();
    let squash = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let rows = screen(LAB_RAW_SEND_600);
    let (_, lines) = composer_draft(&rows).expect("the composer");
    assert!(lines.len() >= 5, "{lines:?}");
    assert_eq!(squash(&lines.concat()), squash(&sent));
    assert!(lines.iter().all(|l| !l.contains("[Pasted Text")));
    assert_eq!(
        folded_paste(&sent).as_deref(),
        Some("[Pasted Text: 600 chars]")
    );
    let (_, chip) = composer_draft(&screen(FOLDED_PASTE)).expect("the composer");
    assert!(chip[0].starts_with("[Pasted Text: 538 chars]"), "{chip:?}");
    assert!(provenance(LAB_RAW_SEND_1600).is_some_and(|p| p.contains("MEASURED")));
    let long: String = phrase.repeat(30).chars().take(1600).collect();
    let (_, lines) = composer_draft(&screen(LAB_RAW_SEND_1600)).expect("the composer");
    assert_eq!(lines.len(), 10, "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("[Pasted Text")));
    let drawn = squash(&lines.concat());
    assert!(squash(&long).ends_with(&drawn), "{drawn}");
    assert!(drawn.len() > squash(&sent).len(), "past the 600's words");
    assert_eq!(
        folded_paste(&long).as_deref(),
        Some("[Pasted Text: 1600 chars]")
    );
}
