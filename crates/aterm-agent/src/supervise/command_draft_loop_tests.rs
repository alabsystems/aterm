// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PERSON'S COMMAND STANDS (v6 §14.17.5.8): aterm's Enter sends a person's standing draft only
//! where its program would take it as a prompt. A slash command, a shell line or an exit word the
//! person typed and left is their hold: past the shipped grace (120 s) nothing is pressed, nothing
//! typed over it and nothing cleared, the hold journalled once and nothing badged; it lifts when
//! they clear it, and the loop goes on. A prompt draft is still sent past the grace, as before.

use super::super::super::turn_end_loop::{OwnBy, OwnLine};
use super::*;
use crate::harness::usage::USAGE_COMMAND;
use crate::supervise::policy::turn_end::{COMMAND_STANDS, CommandEnter, Composer};
use aterm_phase::DraftCommand::{self, Exit, Shell, Slash};
use aterm_phase::prompt::fixtures::screen;

/// The shipped grace a person's key holds every act for ([`SupervisorConfig::human_grace_s`]).
const GRACE: u64 = 120;

/// What a loop did over `screens` under `program`'s mock, a person's key at the read of
/// `screens[1]` (the draft's), run on the shipped grace for `max_s` of its clock (or to its first
/// typed prompt, `to_typed`), `stand` long on the draft's screen before any next one draws: the
/// Enters it pressed, the prompts it typed (`send` and `turn`), its badges, its journal's `HOLD`
/// lines, the run's lines and every request.
struct Ran {
    enters: usize,
    typed: usize,
    badged: usize,
    holds: Vec<String>,
    journal: Vec<String>,
    lines: Vec<String>,
    requests: Vec<String>,
}

fn run_over(
    tag: &str,
    program: &str,
    screens: Vec<Vec<String>>,
    stand: Duration,
    max_s: u64,
    to_typed: bool,
) -> Ran {
    let mut m = match program {
        "agy" => agy_mock(screens),
        "muse" => {
            let mut m = muse_mock(screens);
            m.gen_fence = true;
            m.sends_gen = true;
            m.human_seq = Some(7);
            m.human = vec!["null"];
            m.help = FENCED_HELP.to_string();
            m
        }
        _ => {
            let mut m = fenced(screens);
            m.help = FENCED_HELP.to_string();
            m
        }
    };
    // The draft is the person's typed text, never a grey suggestion.
    m.cell_attrs_on.insert(1, "none");
    m.keyed_at_read = Some(1);
    m.stands.insert(1, stand);
    if to_typed {
        m.exit_when = Some(|m| m.requests.iter().any(|r| types(r)));
        // A wait past the last screen naps on the run's clock, so the person's grace from a
        // clearing (which their leaving the draft marks) runs out on it: the nap caps how far the
        // clock moves, overshooting no wake the loop asked for.
        m.stall_sleep = Some(Duration::from_secs(5));
    }
    let (dir, path) = journal_file(tag);
    let mut opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(max_s)
    };
    opts.policy.human_grace_s = u32::try_from(GRACE).expect("a grace");
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
    Ran {
        enters: m.presses().iter().filter(|p| p.ends_with(" enter")).count(),
        typed: count(&m, "send") + count(&m, "turn"),
        badged: count(&m, "meta set attention"),
        holds: journal
            .iter()
            .filter(|l| l.starts_with("HOLD "))
            .cloned()
            .collect(),
        journal,
        lines,
        requests: m.requests.clone(),
    }
}

/// Whether the request `r` types a prompt (`send`, or `turn`, fenced or not).
fn types(r: &str) -> bool {
    matches!(verb_of(r), "send" | "turn")
}

/// Claude Code's turn end with `draft` (its lines) standing in the composer, the caret after it.
fn claude_drafted(draft: &[&str]) -> Vec<String> {
    let mut rows = ended("Stage 1 is in.");
    let caret = rows.iter().position(|r| r == "❯").expect("the caret row");
    rows[caret] = format!("❯ {}", draft[0]);
    for (k, line) in draft.iter().enumerate().skip(1) {
        rows.insert(caret + k, format!("  {line}"));
    }
    rows
}

/// Muse Code's turn end with `draft` standing at its caret.
fn muse_drafted(draft: &str) -> (Vec<String>, Vec<String>) {
    use aterm_phase::muse::fixtures as u;
    let end = screen(u::END_OF_TURN_1_4_10);
    let caret = aterm_phase::muse::composer(&end).expect("Muse Code's caret");
    let mut drafted = end;
    drafted[caret] = format!("❯ {draft}");
    (screen(u::BUSY_1_4_10), drafted)
}

/// agy's turn end with `draft` standing at its caret.
fn agy_drafted_end(draft: &str) -> (Vec<String>, Vec<String>) {
    use aterm_phase::agy::fixtures as a;
    let end = screen(a::END_OF_TURN);
    (
        screen(a::BUSY),
        crate::supervise::run::tests::point_key::agy_drafted(&end, &[draft]),
    )
}

/// Each program's turn end, busy first, with `draft` standing past the grace.
fn drafted_under(program: &str, draft: &[&str]) -> Vec<Vec<String>> {
    match program {
        "agy" => {
            let (busy, end) = agy_drafted_end(&draft.join("\n"));
            vec![busy, end]
        }
        "muse" => {
            let (busy, end) = muse_drafted(&draft.join("\n"));
            vec![busy, end]
        }
        _ => vec![busy_screen(), claude_drafted(draft)],
    }
}

/// A PERSON'S COMMAND LEFT STANDING PAST THE SHIPPED GRACE IS NEVER SENT, under Claude Code, agy
/// and Muse Code: a slash command (`/clear`, `/exit`, `/logout`, `/compact` with its words, a
/// harmless `/help`), a shell line (agy's pasted `!` line, which its `>` composer keeps as text:
/// the measured `agy::fixtures::lab::PASTED_BANG_DRAFT`; a `!` typed takes each program into a
/// shell mode that draws no composer, [`each_programs_shell_mode_line_is_never_sent`]), Claude
/// Code's exit word, one after leading spaces, and a draft of several lines whose first is a
/// command. Past the grace nothing is
/// pressed, nothing typed and nothing badged, and the hold is journalled once, its line the form's
/// words and [`COMMAND_STANDS`] alone (no word of the draft). On origin/main (9c586c9ad0) every
/// one of them was sent by the draft step's Enter at the grace's end (`CONTINUED …
/// rule=continue@v1 /clear`). Negative controls by mutation: the draft step's command arm dropped
/// (`policy/turn_end.rs`, the `Composer::Command` arm of `decide_unridden`): the Enter at the
/// grace's end; the hold's words not taken from the command (`HoldSaid::Command` in
/// `turn_end_write`): `HOLD … why=policy`.
#[test]
fn a_persons_command_left_standing_past_the_grace_is_never_sent() {
    let cases: &[(&str, &[&str], DraftCommand)] = &[
        ("claude", &["/clear"], Slash),
        ("claude", &["/exit"], Slash),
        ("claude", &["/logout"], Slash),
        ("claude", &["/compact keep the parser notes"], Slash),
        ("claude", &["/help"], Slash),
        ("claude", &["exit"], Exit),
        ("claude", &["   /quit"], Slash),
        ("claude", &["/clear", "then start on the lexer"], Slash),
        ("agy", &["/clear"], Slash),
        ("agy", &["!echo pasted-probe"], Shell),
        ("agy", &["/help"], Slash),
        ("muse", &["/clear"], Slash),
        ("muse", &["/help"], Slash),
    ];
    // agy's pasted `!` line is the measured one: its `>` composer keeps the paste as text.
    assert!(
        screen(aterm_phase::agy::fixtures::lab::PASTED_BANG_DRAFT)
            .iter()
            .any(|r| r == "> !echo pasted-probe"),
        "agy's measured paste"
    );
    for (program, draft, form) in cases {
        let tag = format!("command-stands-{program}-{}", draft.join("-"));
        let ran = run_over(
            &tag,
            program,
            drafted_under(program, draft),
            Duration::from_secs(400),
            300,
            false,
        );
        let what = format!("{program}: {draft:?}");
        assert_eq!(
            (ran.enters, ran.typed, ran.badged),
            (0, 0, 0),
            "{what}: nothing pressed, typed or badged: {:#?}\n{:#?}",
            ran.lines,
            ran.journal
        );
        assert_eq!(
            ran.holds.len(),
            1,
            "{what}: the hold journalled once: {:#?}",
            ran.journal
        );
        let said = ran.holds[0]
            .split_once(" point ")
            .map_or("", |(_, said)| said);
        assert_eq!(
            said,
            format!(
                "nothing why=command a person's {} {COMMAND_STANDS}",
                form.words()
            ),
            "{what}: the form's words alone, no word of the draft: {:#?}",
            ran.holds
        );
    }
}

/// A PERSON'S PROMPT DRAFT IS STILL SENT PAST THE GRACE, under Claude Code, agy and Muse Code, as
/// on origin/main: one Enter at the grace's end and no hold of a command. Claude Code's paste chip
/// is a prompt to it (its own display text opens no command), and a draft naming a command mid-line
/// is a prompt. Negative control:
/// [`a_persons_command_left_standing_past_the_grace_is_never_sent`]'s.
#[test]
fn a_persons_prompt_draft_is_still_sent_past_the_grace() {
    let cases: &[(&str, &[&str])] = &[
        ("claude", &["also update the changelog"]),
        ("claude", &["run /clear after the suite is green"]),
        ("claude", &["[Pasted text #1 +12 lines]"]),
        ("agy", &["also update the changelog"]),
        ("muse", &["also update the changelog"]),
    ];
    for (program, draft) in cases {
        let tag = format!("prompt-goes-{program}-{}", draft.join("-"));
        let ran = run_over(
            &tag,
            program,
            drafted_under(program, draft),
            Duration::from_secs(400),
            300,
            false,
        );
        assert_eq!(
            ran.enters, 1,
            "{program}: {draft:?}: its Enter: {:#?}\n{:#?}",
            ran.lines, ran.journal
        );
        assert!(
            !ran.holds.iter().any(|l| l.contains("why=command")),
            "{program}: {draft:?}: {:#?}",
            ran.holds
        );
    }
}

/// A PERSON'S COMMAND THEY CLEAR LETS THE LOOP GO ON: `/clear` stands past the grace (no Enter, the
/// hold journalled), then the person clears it, and the loop types its continuation: the hold is
/// no park for good. Negative control by mutation: the hold not let go when the composer's reading
/// changes (`draft_look` returning on any held draft): no continuation, the run held until the
/// one-minute hang detector ends it. A second: a command not held as a draft (`draft_holds` false
/// for it): its hold journalled as the policy's, not the person's command.
#[test]
fn a_persons_command_cleared_lets_the_loop_go_on() {
    for program in ["claude", "agy", "muse"] {
        let mut screens = drafted_under(program, &["/clear"]);
        let cleared = match program {
            "agy" => screen(aterm_phase::agy::fixtures::END_OF_TURN),
            "muse" => screen(aterm_phase::muse::fixtures::END_OF_TURN_1_4_10),
            _ => ended("Stage 1 is in."),
        };
        // The draft drawn once more past its stand, so the clearing comes as a move of the screen
        // the loop's wait wakes on (the mock draws a stand's next screen with no wake of its own).
        let drafted = screens.last().cloned().expect("the draft's screen");
        screens.push(drafted);
        screens.push(cleared);
        let ran = run_over(
            &format!("command-cleared-{program}"),
            program,
            screens,
            Duration::from_secs(200),
            600,
            true,
        );
        let typed: Vec<&String> = ran.requests.iter().filter(|r| types(r)).collect();
        let first = ran.requests.iter().position(|r| types(r));
        let enters_before = ran.requests[..first.unwrap_or(ran.requests.len())]
            .iter()
            .filter(|r| verb_of(r) == "key" && r.ends_with(" enter"))
            .count();
        assert_eq!(
            ran.holds
                .iter()
                .filter(|l| l.contains("why=command"))
                .count(),
            1,
            "{program}: {:#?}",
            ran.journal
        );
        assert!(
            enters_before == 0 && typed.len() == 1 && typed[0].ends_with(" keep going"),
            "{program}: the continuation once it is cleared, never the command: {:#?}\n{:#?}\n{:#?}",
            ran.lines,
            ran.journal,
            typed
        );
    }
}

/// EACH PROGRAM'S OWN SHELL MODE DRAWS NO PROMPT BOX A DRAFT STANDS IN: a `!` line typed into
/// Claude Code's (the measured `claude-2.1.283-shell-mode-draft.txt`, its caret `!`), agy 1.4.12's
/// bash mode and Muse Code 1.4.14's shell mode (`! ls -la`), and a `!` line pasted into Muse Code,
/// which takes it there too (`! echo pasted-probe`), read as no composer, so no point is decided
/// on them and nothing is pressed or typed past the shipped grace, on origin/main as on this
/// branch (v6 §14.17.5.8, departure 2). Only agy keeps a pasted `!` as text, held as a shell line
/// ([`a_persons_command_left_standing_past_the_grace_is_never_sent`]). Negative control by
/// mutation: agy's caret row read with `!` as its caret (`agy::is_caret_row`): its bash mode's
/// `ls -la` reads a prompt draft, and the grace's end presses its Enter.
#[test]
fn each_programs_shell_mode_line_is_never_sent() {
    use aterm_phase::{agy, muse};
    let cases = [
        (
            "claude",
            busy_screen(),
            screen(aterm_phase::prompt::fixtures::SHELL_MODE_DRAFT),
            "!\u{a0}ls",
        ),
        (
            "agy",
            screen(agy::fixtures::BUSY),
            screen(agy::fixtures::lab::BASH_MODE_DRAFT),
            "! ls -la",
        ),
        (
            "muse",
            screen(muse::fixtures::BUSY),
            screen(muse::fixtures::SHELL_MODE_DRAFT),
            "! ls -la",
        ),
        (
            "muse",
            screen(muse::fixtures::BUSY),
            screen(muse::fixtures::PASTED_BANG_DRAFT),
            "! echo pasted-probe",
        ),
    ];
    for (program, busy, shell, caret) in cases {
        assert!(
            shell.iter().any(|r| r == caret),
            "{program}: the measured caret row: {shell:#?}"
        );
        let ran = run_over(
            &format!("command-{program}-shell-mode-{}", caret.len()),
            program,
            vec![busy, shell],
            Duration::from_secs(400),
            300,
            false,
        );
        assert_eq!(
            (ran.enters, ran.typed, ran.badged),
            (0, 0, 0),
            "{program} {caret:?}: {:#?}\n{:#?}",
            ran.lines,
            ran.journal
        );
    }
}

/// THE LOOP'S OWN COMMAND LINE IS ITS OWN TO SEND ONLY WITH NO PERSON'S TYPING SINCE IT WAS LEFT
/// ([`OwnLine::unkeyed`], the command arm of `left_composer`): the probe's `/usage`, left whole and
/// found left 30 s before the read, reads as the loop's line to send (`Composer::Typed`) where the
/// window's typing stamp (`human_typed_ms`) puts the person's last typing before then, or says
/// they never typed, whatever their scrolls and clicks moved the gesture count to; where it puts
/// typing after then, the same `/usage` is a person's command. A window that stamps no typing
/// falls back to the gesture count: the line is the loop's only while the count is the one its
/// write was fenced on. Under a window that counts nothing, or with no line of the loop's left,
/// it is a person's. Negative controls by mutation: `unkeyed` reading the count alone (`typed`
/// ignored): the scroll's case is a person's command, held; `typed_since` saying no typing
/// whatever the stamp: the typing's case is the loop's, sent.
#[test]
fn the_loops_own_command_line_is_its_own_only_with_no_persons_typing_since() {
    let person = Composer::Command(Slash, CommandEnter::Runs);
    for (what, left, human_now, typed_ms, want) in [
        (
            "its write's count, no stamp",
            true,
            Some(0),
            None,
            Composer::Typed,
        ),
        (
            "a scroll since, typing before it was left",
            true,
            Some(3),
            Some("60000"),
            Composer::Typed,
        ),
        (
            "a scroll since, no typing ever",
            true,
            Some(3),
            Some("null"),
            Composer::Typed,
        ),
        (
            "a person's typing since",
            true,
            Some(1),
            Some("10000"),
            person.clone(),
        ),
        (
            "a person's typing since, the count unmoved",
            true,
            Some(0),
            Some("10000"),
            person.clone(),
        ),
        (
            "a gesture since, no stamp",
            true,
            Some(1),
            None,
            person.clone(),
        ),
        ("no count, no stamp", true, None, None, person.clone()),
        (
            "no line of the loop's",
            false,
            Some(0),
            Some("60000"),
            person.clone(),
        ),
    ] {
        let mut m = Mock::new(vec![with_draft(&ended(PROBE_SAID), USAGE_COMMAND)]);
        m.gen_fence = true;
        m.human_seq = human_now;
        m.typed_ms = typed_ms.and_then(|ms| ms.parse().ok());
        if typed_ms == Some("null") {
            m.human = vec!["null"];
        }
        m.cursor_col = Some(2 + USAGE_COMMAND.len());
        let opts = hosted(60);
        let mut s = virtual_session(&mut m, None);
        let screen = s.screen().expect("the read");
        if typed_ms == Some("null") {
            assert_eq!(
                screen.typed,
                HumanInput::Never,
                "{what}: the window's stamp"
            );
        }
        if left {
            let found =
                s.fx.mono()
                    .checked_sub(Duration::from_secs(30))
                    .expect("a clock past 30 s");
            s.own_line = Some(OwnLine::of(USAGE_COMMAND, OwnBy::Probe, Some(0)).at(found));
        }
        let mut r = s.turn_end_reading(&screen, None, &opts);
        assert_eq!(r.composer, person, "{what}: read as a command");
        s.left_composer(&mut r, &opts.policy, &screen);
        assert_eq!(r.composer, want, "{what}");
    }
}

/// THE LOOP'S OWN COMMAND LINE UNDER A MENU THAT RUNS ANOTHER ENTRY IS NOT SENT
/// ([`CommandEnter::own`]): the loop's own `/config`, left whole with no person's gesture since,
/// under Claude Code 2.1.268's slash menu (measured cells) with `/config` itself selected reads as
/// the loop's line to send (`Composer::Typed`: Enter runs it as written), and with
/// `/update-config` selected as the menu's diversion (`Composer::Diverted(Diversion::Menu)`, as a
/// draft of no command's reads there on origin/main), never sent onto an entry the loop never
/// wrote; a selection unread is unknown. A person's command stands whatever the menu says
/// ([`a_persons_command_left_standing_past_the_grace_is_never_sent`]). Negative control by
/// mutation: `CommandEnter::own` giving `Composer::Typed` for every menu: the `/update-config`
/// case reads `Typed`, the line the loop's to send onto the menu's other entry.
#[test]
fn the_loops_own_command_line_under_a_menu_running_another_entry_is_not_sent() {
    use crate::supervise::policy::turn_end::Diversion;
    use aterm_phase::prompt::fixtures::{
        SLASH_MENU_CONFIG_2_1_268, SLASH_MENU_CONFIG_CELLS_2_1_268,
        SLASH_MENU_CONFIG_DOWN_CELLS_2_1_268, cells,
    };
    let mut at_end = screen(SLASH_MENU_CONFIG_2_1_268);
    at_end[3] = "⏺ Stage 1 is in.".to_string();
    at_end[4] = "✻ Worked for 12m 2s · done 4:24 PM".to_string();
    let caret = at_end
        .iter()
        .position(|r| r.starts_with('❯') && r.trim_end().ends_with("/config"))
        .expect("the caret row");
    for (what, measured, enter, want) in [
        (
            "`/config` selected",
            SLASH_MENU_CONFIG_CELLS_2_1_268,
            CommandEnter::Runs,
            Composer::Typed,
        ),
        (
            "`/update-config` selected",
            SLASH_MENU_CONFIG_DOWN_CELLS_2_1_268,
            CommandEnter::Diverted,
            Composer::Diverted(Diversion::Menu),
        ),
    ] {
        let mut m = fenced(vec![at_end.clone()]);
        m.cursor_row = Some(caret);
        m.cursor_col = Some(2 + "/config".chars().count());
        m.cells = cells(measured)
            .into_iter()
            .map(|(r, c, reply)| ((r, c), reply))
            .collect();
        m.human_seq = Some(0);
        let opts = hosted(60);
        let mut s = virtual_session(&mut m, None);
        let read = s.screen().expect("the read");
        s.own_line = Some(OwnLine::of("/config", OwnBy::Line, Some(0)));
        let mut r = s.turn_end_reading(&read, None, &opts);
        assert_eq!(
            r.composer,
            Composer::Command(Slash, enter),
            "{what}: read as a command under the menu"
        );
        s.left_composer(&mut r, &opts.policy, &read);
        assert_eq!(r.composer, want, "{what}");
    }
    assert_eq!(CommandEnter::Unread.own(), Composer::Unknown);
}

/// A HOLD NAMES THE PERSON'S COMMAND ONLY WHERE THE DRAFT STEP'S COMMAND ARM HELD IT (v6
/// §14.17.5.8, `Session::command_why`): `/` typed into agy 1.4.0 at a turn's end draws its slash
/// menu (the measured `agy::fixtures::lab::SLASH_MENU`), a read no point is decided on, so the
/// point's act is `Nothing` for that and not for the command: past the shipped grace nothing is
/// pressed, typed or badged, and the hold journalled says its own why, never `why=command`.
/// Negative control by mutation: `command_why` true wherever a command stands under a `Nothing`
/// (the review's finding): `HOLD … why=command` over the menu.
#[test]
fn a_hold_names_the_command_only_where_the_command_held_it() {
    use aterm_phase::agy::fixtures as a;
    let menu = screen(a::lab::SLASH_MENU);
    assert!(
        menu.iter().any(|r| r.trim_end() == "> /"),
        "the measured `/` under its menu"
    );
    let ran = run_over(
        "command-why-agy-menu",
        "agy",
        vec![screen(a::BUSY), menu],
        Duration::from_secs(400),
        300,
        false,
    );
    assert_eq!(
        (ran.enters, ran.typed, ran.badged),
        (0, 0, 0),
        "{:#?}\n{:#?}",
        ran.lines,
        ran.journal
    );
    assert!(
        !ran.holds.is_empty() && !ran.holds.iter().any(|l| l.contains("why=command")),
        "a hold of its own why: {:#?}",
        ran.journal
    );
}
