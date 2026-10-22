// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// WHAT AGY READS BACK OF A MESSAGE aterm types into it. agy draws a
// message's blank line as a blank row with the rows under it two columns
// in, as its reply's (MEASURED in the lab, 2026-10-21:
// `aterm_phase::agy::fixtures::lab::WORKER_FIRST_PROMPT`), and its reader
// takes the reply from the gap under the message's first row
// (`aterm_phase::agy::said_tail`): whatever of a message follows a blank
// line of it reads as agy's reply — its `Nothing left:` a defense, its
// `NEEDS YOU:` a need. So does every row of a message whose first row is
// above the rows read: a reply cut at the rows' top. These tests DRAW each
// text the harness types the way agy draws a message ([`agy_turn`], a model:
// held row for row to the measured screens it is built from, by
// [`agys_drawing_is_held_to_its_measured_screens`]) and read it back
// through agy's reader and the harness's own judges ([`needs::needs_you`],
// [`needs::defense_line`], [`clean_defense`]). What the model draws beyond
// those captures (other widths, a message's single line break) is the
// model's, and is called drawn here, never measured.
use aterm_phase::agy::fixtures::lab::{REPLY_WRAPPED_50, WORKER_FIRST_PROMPT};
use aterm_phase::prompt::fixtures::screen;

/// A plain report of progress: no defense, no need.
const PROGRESS: &str = "Implemented the parser module and its unit tests in this turn.";

/// A clean defense under Land: what lands the work.
const DEFENSE: &str = "Nothing left: the parser is committed and pushed to origin/main.";

/// `line`'s words greedily on rows of at most `width` columns (a word wider
/// than the row on rows of its own, cut at the edge).
fn wrapped(line: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in line.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        if !row.is_empty() && row.chars().count() + 1 + word.len() > width {
            rows.push(std::mem::take(&mut row));
        }
        while word.len() > width {
            let rest = word.split_off(width);
            rows.push(word.into_iter().collect());
            word = rest;
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.extend(word);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// One turn under its rule as agy 1.4.0 draws one at `cols` columns (held to
/// the measured screens by [`agys_drawing_is_held_to_its_measured_screens`]):
/// the message's rows — `> ` before its first, two columns in after, wrapped
/// at `cols - 1`; a line of it starts a row of its own and a blank line of it
/// is a blank row — then a blank row, the reply's paragraphs two columns in,
/// wrapped two cells short of the edge, a blank row between them, and a blank
/// row.
fn agy_turn(cols: usize, message: &str, reply: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for line in message.lines() {
        if line.trim().is_empty() {
            rows.push(String::new());
            continue;
        }
        for row in wrapped(line, cols - 3) {
            let lead = if rows.is_empty() { "> " } else { "  " };
            rows.push(format!("{lead}{row}"));
        }
    }
    rows.push(String::new());
    for (i, paragraph) in reply.split("\n\n").enumerate() {
        if i > 0 {
            rows.push(String::new());
        }
        rows.extend(
            wrapped(paragraph, cols - 4)
                .into_iter()
                .map(|r| format!("  {r}")),
        );
    }
    rows.push(String::new());
    rows
}

/// A measured screen's frame: its rows down to its first turn's rule, the
/// turn rule, and its composer from the rule over the caret down.
fn frame(text: &str) -> (Vec<String>, String, Vec<String>) {
    let rows = screen(text);
    let rule = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("a turn's rule");
    let caret = rows.iter().rposition(|r| r == ">").expect("the composer");
    (
        rows[..rule].to_vec(),
        rows[rule].clone(),
        rows[caret - 1..].to_vec(),
    )
}

/// agy's screen at `cols` after one turn: `message`, then `reply`, idle
/// again — at 144 (the L15 worker's pane) and 50 (the narrow lab pane) in
/// the measured screens' own frames, at any other width in the 144 one's
/// with its rules drawn to that width (the turn's separator 60 columns or
/// three short of the edge, as at 50; the composer's full width) and its
/// footer's left side alone.
fn agy_screen(cols: usize, message: &str, reply: &str) -> Vec<String> {
    let (head, rule, tail) = match cols {
        50 => frame(REPLY_WRAPPED_50),
        144 => frame(WORKER_FIRST_PROMPT),
        _ => {
            let (head, _, tail) = frame(WORKER_FIRST_PROMPT);
            let tail = tail
                .iter()
                .map(|r| {
                    if r.starts_with('─') {
                        "─".repeat(cols)
                    } else if r.starts_with("? for shortcuts") {
                        "? for shortcuts".to_string()
                    } else {
                        r.clone()
                    }
                })
                .collect();
            (head, "─".repeat(60.min(cols - 3)), tail)
        }
    };
    let mut rows = head;
    rows.push(rule);
    rows.extend(agy_turn(cols, message, reply));
    rows.extend(tail);
    rows
}

/// `text`'s words, one space between: a reply as wrapped rows join it, or
/// read with no width.
fn words(text: Option<&str>) -> Option<String> {
    text.map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The harness's reading of `rows` under agy's reader.
fn agy_reading(rows: &[String]) -> TurnEndReading {
    let reading = aterm_phase::read(Some("agy"), rows, None);
    assert_eq!(reading.program, Program::Agy);
    TurnEndReading::of(&reading, rows, false, Some(3 * MIN), None, None)
}

/// THE DRAWING IS HELD TO AGY'S OWN: [`agy_turn`] redraws, row for row, both
/// measured screens it is built from — the L15 worker's two turns at 144
/// columns (a message whose blank line agy drew as a blank row, its later
/// rows two columns in; then the Land act and its ack over five rows, and the
/// landing) and the lab's 50-column reply of two paragraphs. Those pin the
/// message's wrap at 144 columns, a blank line's row, and the reply's wrap at
/// 50; the message's wrap at other widths and a single line break of it
/// (`l15a2` saw one drawn as one message, no blank row, its rows unrecorded)
/// are the model's. CONTROL, on the capture rather than the model: the
/// measured screen's own rows hold the blank row and the fact two columns in
/// under it.
#[test]
fn agys_drawing_is_held_to_its_measured_screens() {
    let rows = screen(WORKER_FIRST_PROMPT);
    let (head, rule, tail) = frame(WORKER_FIRST_PROMPT);
    let user = rows
        .iter()
        .position(|r| r.starts_with("> FOREVERLAB-WORKER"))
        .expect("the first prompt");
    let task = rows[user].trim_start_matches("> ").to_string();
    // The spawn's fact line, as its rows hold it.
    let fact = rows[user + 2..user + 5]
        .iter()
        .map(|r| r.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let land = act_words(LAND, &[reversible_clause(false), ack_clause(true)], None);
    let first = format!("{task}\n\n{fact}");
    let drawn = |first: &str| {
        let mut drawn = head.clone();
        drawn.push(rule.clone());
        drawn.extend(agy_turn(144, first, PROGRESS));
        drawn.push(rule.clone());
        drawn.extend(agy_turn(144, &land, DEFENSE));
        drawn.extend(tail.iter().cloned());
        drawn
    };
    assert_eq!(drawn(&first), rows);
    // CONTROL, ON THE CAPTURE ITSELF: the message's blank line is a blank row
    // there, the fact's rows under it two columns in as the reply's are, and
    // agy's reader reads them as its reply — what the model's blank-line
    // branch draws is agy's own screen, not the model's choice.
    assert!(rows[user + 1].is_empty(), "{:?}", rows[user + 1]);
    assert!(
        rows[user + 2..user + 5]
            .iter()
            .all(|r| r.starts_with("  ") && !r.starts_with("   ")),
        "{:?}",
        &rows[user + 2..user + 5]
    );

    let narrow = screen(REPLY_WRAPPED_50);
    let reply = "I finished the parser and the suite is green.\n\nTwo ways on from here: keep the \
                 old API, or move every caller now. Which do you prefer (1 or 2)?";
    assert_eq!(agy_screen(50, "what next", reply), narrow);
}

/// A rules file of two paragraphs, the second naming the NEEDS YOU line.
const RULES: &str = "Run the full suite before you push.\n\nNever force-push; a NEEDS YOU: line \
                     is for credentials only.\n";

/// A rules file of two paragraphs that never says the token: an owner's
/// rules that do are the owner's words, read as a reply's would be where a
/// cut opens on them ([`an_acts_rows_cut_at_the_rows_top_name_no_need_and_land_nothing`]).
const PLAIN_RULES: &str = "Run the full suite before you push.\n\nNever force-push.\n";

/// The standing rules `text` as the loop types them: read by the loop's own
/// reader of the rules file (`turn_end_loop::standing_rules_in`, which makes
/// it one line).
fn rules_line(text: &str) -> String {
    crate::supervise::run::standing_rules_in(text).expect("rules")
}

/// EVERY TEXT THE HARNESS TYPES INTO AN AGENT AS ITS MESSAGE, by name: Land's
/// three acts and the nudge with each suffix it takes, Cruise's continuation,
/// done check and answers (the owner's default words), the wall acts agy's
/// screens raise, the login back, and the relaunch lanes' resumed prompts —
/// each without and with the standing rules.
fn every_act() -> Vec<(String, String)> {
    every_act_with(RULES)
}

/// [`every_act`] with the standing rules of the rules file `rules_text`.
fn every_act_with(rules_text: &str) -> Vec<(String, String)> {
    let repo = crate::harness::repo_work::RepoWork {
        changed: 2,
        untracked: 1,
        unpushed: 3,
        paths: vec!["src/parser.rs".to_string(), "notes.txt".to_string()],
        ..Default::default()
    };
    let open = open_work_strict(Some("Two items remain open and the branch is not pushed."));
    assert!(!open.is_empty(), "words naming open work");
    let rules = rules_line(rules_text);
    let mut acts: Vec<(String, String)> = Vec::new();
    for (with, rules) in [("", None), (" +rules", Some(rules.as_str()))] {
        let mut add = |name: &str, text: String| acts.push((format!("{name}{with}"), text));
        add("land", land_text(rules));
        add("land-nudge", land_nudge_text(rules));
        add("land-answer", land_answer_text(rules));
        add(
            "land-ack",
            act_words(LAND, &[reversible_clause(true), ack_clause(true)], rules),
        );
        add(
            "land-nudge-fitted",
            fitted_act(
                LAND_NUDGE,
                &[
                    Suffix::Open(&open),
                    Suffix::Repo(Some(&repo)),
                    Suffix::Fixed(DONE_CHECK_REVERSIBLE),
                    Suffix::Fixed(NEEDS_ACK),
                ],
                rules,
            ),
        );
        add("done-check", done_check_text(rules, false));
        add("done-check-irreversible", done_check_text(rules, true));
        add("continue", continuation_text(&cfg(), rules));
        add("answer", answer_text(&cfg(), rules));
        add("reversible-answer", reversible_answer(rules));
        for priority in [
            QuestionPriority::Recommended,
            QuestionPriority::LongTerm,
            QuestionPriority::ShortTerm,
        ] {
            let cfg = SupervisorConfig {
                question_priority: priority,
                ..cfg()
            };
            add(
                &format!("prose-answer-{priority:?}"),
                prose_answer(&cfg, rules),
            );
        }
    }
    let causes = [
        ApiCause::Server,
        ApiCause::Unreachable,
        ApiCause::CutOff,
        ApiCause::Config,
        ApiCause::Request,
        ApiCause::Overlong,
    ];
    let kinds =
        std::iter::once(WallKind::Overloaded).chain(causes.map(|cause| WallKind::ApiError {
            code: Some(503),
            retryable: true,
            cause,
        }));
    for kind in kinds {
        for rule in [RULE_API_RETRY, RULE_API_BACK, RULE_API_CUTOFF] {
            acts.push((
                format!("{rule} {kind:?}"),
                wall_retry_text(rule, kind, Program::Agy),
            ));
        }
    }
    acts.push(("login-back".to_string(), login_back_text(Program::Agy)));
    acts.push((
        "upgrade-carry-on".to_string(),
        crate::harness::upgrade::CARRY_ON.to_string(),
    ));
    for upgraded in [false, true] {
        acts.push((
            format!("restore-prompt upgraded={upgraded}"),
            crate::harness::upgrade::restore_prompt(None, upgraded),
        ));
    }
    for cause in ["memory", "host", "stall", "asked", ""] {
        acts.push((
            format!("resumed-prompt {cause}"),
            crate::harness::relaunch::resumed_prompt_as(Program::Agy.product(), "1.4.0", cause),
        ));
    }
    acts
}

/// The text the act executor types for `text` (`turn_end_loop::one_turn`:
/// one line, whatever line breaks the owner's words hold).
fn typed(text: &str) -> String {
    crate::supervise::run::one_turn(text)
}

/// NO ACT THE HARNESS COMPOSES READS BACK AS AGY'S REPLY UNDER ITS WHOLE
/// MESSAGE. Every text of [`every_act`] is one line as composed (its parts
/// joined by spaces, the standing rules made one by the loop's own reader of
/// them), so agy draws it as one message: under it, drawn at every width of
/// [`WIDTHS`], agy's reader reads the reply alone, the harness raises no need
/// from it and takes no landing from a plain report — though the Land act
/// itself says `NEEDS YOU:` and `Nothing left:` mid-line — and the turn reads
/// as opened by the act's own words (what Land's landing is held to). This
/// guards the composers (a rules reader that kept the file's blank line
/// breaks the `+rules` acts here); the executor's fold of an owner's text is
/// `a_message_with_a_blank_line_…` and the loop's
/// `an_owners_act_in_two_paragraphs_is_typed_as_one_line`. NEGATIVE CONTROL:
/// the same acts over a clean defense are a landing, so the act's words
/// spoil no real one.
#[test]
fn no_act_the_harness_types_reads_back_as_agys_reply() {
    let acts = every_act();
    assert!(acts.len() > 40, "{}", acts.len());
    for (name, act) in &acts {
        assert!(
            !act.chars().any(crate::supervise::limit::breaks_a_line),
            "{name} holds a line break: {act:?}"
        );
        let act = typed(act);
        for cols in WIDTHS {
            let r = agy_reading(&agy_screen(cols, &act, PROGRESS));
            assert_eq!(
                words(r.said_tail.as_deref()).as_deref(),
                Some(PROGRESS),
                "{name} at {cols}"
            );
            assert!(!r.said_cut, "{name} at {cols}");
            assert!(
                needs::needs_you(r.said_tail.as_deref().unwrap_or("")).is_empty(),
                "{name} at {cols}"
            );
            assert!(!clean_defense(r.said_tail.as_deref()), "{name} at {cols}");
            assert_eq!(
                r.opened_by,
                Some(crate::supervise::approvals::words_lead(&act)),
                "{name} at {cols}"
            );
            // CONTROL: a real landing under the act lands.
            let landed = agy_reading(&agy_screen(cols, &act, DEFENSE));
            assert_eq!(
                words(landed.said_tail.as_deref()).as_deref(),
                Some(DEFENSE),
                "{name} at {cols}"
            );
            assert!(
                clean_defense(landed.said_tail.as_deref()),
                "{name} at {cols}"
            );
        }
    }
}

/// NO ACT'S OWN WORDS NAME A NEED, A DEFENSE OR A DONE, read whole and at
/// every row agy may wrap them at — what any reader that cannot bound the
/// message (agy's, a reply cut above the rows read) reads back of them. The
/// review of b9/spawn-residuals (F1) found Land's act and its nudge read as
/// the need `and land the rest. Once it is all on remote main, …`: the
/// instruction `on a line starting NEEDS YOU:` was read as a head said
/// (`needs::names_a_head` now names it, on its row or wrapped onto the next).
/// NEGATIVE CONTROL: the same act with a real need after it names that need.
#[test]
fn no_acts_own_words_name_a_need_a_defense_or_a_done() {
    for (name, act) in every_act() {
        let act = typed(&act);
        assert!(needs::needs_you(&act).is_empty(), "{name}: {act:?}");
        assert_eq!(needs::defense_line(&act), None, "{name}");
        assert!(!says_done(Some(&act)), "{name}");
        for width in 20..=150 {
            let rows = wrapped(&act, width).join("\n");
            assert!(
                needs::needs_you(&rows).is_empty(),
                "{name} at {width}: {:?}",
                needs::needs_you(&rows)
            );
            assert!(!clean_defense(Some(&rows)), "{name} at {width}: {rows}");
        }
        // CONTROL: a need said after the act's words is read.
        assert_eq!(
            needs::needs_you(&format!("{act}\nNEEDS YOU: the signing key")),
            ["the signing key"],
            "{name}"
        );
    }
}

/// THE ROWS OF AN ACT CUT AT THE ROWS' TOP name no need, and the reply they
/// read with lands nothing. agy's reader takes every row two columns in under
/// the last rule it sees as the reply, and an act whose `> [aterm harness]`
/// row is above the rows read (a loop's first point after an update, a
/// relaunch, Off to Land; a tall reply) reads its later rows as agy's: so
/// drawn at every width of [`WIDTHS`], under a plain report and a long one,
/// with 1 up to all but one of its rows above the rows read, each reading is
/// CUT ([`TurnEndReading::said_cut`]: nothing lands on it), or reads no
/// words at all, and neither its
/// last words nor its turn's other words name a need (the standing rules
/// ones that never say the token, [`PLAIN_RULES`]). At the policy, the
/// first point over such a reading puts up no need, owes no ack and lands
/// nothing, the review's very screen (Land's act at 50 columns, its first 3
/// rows gone, worked unknown). NEGATIVE CONTROL: the same screens uncut read
/// the reply alone, whole.
#[test]
fn an_acts_rows_cut_at_the_rows_top_name_no_need_and_land_nothing() {
    let long = (0..24)
        .map(|i| format!("Step {i} of the parser is written, tested and committed on its branch."))
        .collect::<Vec<_>>()
        .join(" ");
    for (name, act) in every_act_with(PLAIN_RULES) {
        let act = typed(&act);
        for cols in WIDTHS {
            for reply in [PROGRESS, long.as_str()] {
                let whole = agy_screen(cols, &act, reply);
                let at = whole
                    .iter()
                    .position(|r| r.starts_with("> "))
                    .expect("the act's row");
                let end = (at + 1..whole.len())
                    .find(|&i| whole[i].is_empty())
                    .expect("the act's end");
                let uncut = agy_reading(&whole);
                assert!(!uncut.said_cut, "CONTROL: {name} at {cols}");
                assert_eq!(words(uncut.said_tail.as_deref()).as_deref(), Some(reply));
                for drop in 1..end - at {
                    let cut = whole[at + drop..].to_vec();
                    let r = agy_reading(&cut);
                    let at_cut = format!("{name} at {cols}, {drop} rows above");
                    // Cut, or no words at all (a row naming `Antigravity
                    // CLI` reads as agy's banner, before any message).
                    assert!(r.said_cut || r.said_tail.is_none(), "{at_cut}: {cut:#?}");
                    let tail = r.said_tail.as_deref().unwrap_or("");
                    assert!(needs::needs_you(tail).is_empty(), "{at_cut}: {tail}");
                    assert!(
                        r.turn_said
                            .as_deref()
                            .is_none_or(|t| needs::needs_you(t).is_empty()),
                        "{at_cut}: {:?}",
                        r.turn_said
                    );
                }
            }
        }
    }
    // The policy at a loop's first point, the review's screen.
    let land = typed(&land_text(None));
    let whole = agy_screen(50, &land, PROGRESS);
    let at = whole
        .iter()
        .position(|r| r.starts_with("> "))
        .expect("the act's row");
    let mut r = agy_reading(&whole[at + 3..]);
    assert!(r.said_cut);
    r.worked = None;
    r.autopilot = Autopilot::Land;
    let mut state = TurnEndState::default();
    state.observe(&r, Instant::now());
    assert!(
        state.standing_needs().is_empty(),
        "{:?}",
        state.standing_needs()
    );
    assert!(!state.needs_ack_owed());
    assert!(!state.landed());
}

/// The widths every act is drawn at: the narrow lab pane (50), the L15
/// worker's (144), and a spread between and around them.
const WIDTHS: [usize; 12] = [40, 50, 61, 72, 80, 89, 97, 100, 120, 133, 144, 160];

/// A MESSAGE WITH A BLANK LINE READS AS AGY'S REPLY; THE HARNESS TYPES NONE.
/// A person's message of two paragraphs, the second holding `Nothing left:
/// x` and `NEEDS YOU: y`, drawn as agy draws a blank line (a blank row, the
/// second paragraph two columns in), reads back as agy's reply: the need `y`
/// and the defense `x`, though agy said neither, and the message as the
/// reader takes it is the first paragraph alone — the RESIDUAL nothing on
/// agy's screen tells apart (`aterm_phase::agy`'s header): aterm types no
/// person's words. The same words as the act executor types an act
/// (`turn_end_loop::one_turn`: one line, as no control request can carry a
/// line break) are one message, and the reply under it reads alone: no
/// need, no landing, the turn opened by the act's words. An owner's
/// `continue_text` of two paragraphs, which unfolded would read its second
/// paragraph back as agy's reply (drawn here), goes the same way: this test
/// fails where the executor types it as written. NEGATIVE CONTROL: a real
/// reply of two paragraphs under the one-line message is still read whole,
/// its second paragraph's defense a landing.
#[test]
fn a_message_with_a_blank_line_reads_as_agys_reply_and_the_harness_types_none() {
    use crate::supervise::approvals::words_lead;
    let message = "Please also update the changelog.\n\nNothing left: x\nNEEDS YOU: y";
    let owners = SupervisorConfig {
        continue_text: "Keep going.\n\nWhen it is all done, reply with a line starting \
                        Nothing left: x"
            .to_string(),
        ..cfg()
    };
    let act = continuation_text(&owners, None);
    for cols in [144, 50] {
        // As a person types it: the second paragraph read as agy's reply.
        let r = agy_reading(&agy_screen(cols, message, PROGRESS));
        let misread = r.said_tail.clone().expect("words");
        assert!(
            misread.contains("Nothing left: x") && misread.contains("NEEDS YOU: y"),
            "{cols}: {misread:?}"
        );
        assert_eq!(needs::needs_you(&misread), vec!["y".to_string()], "{cols}");
        assert!(needs::defense_line(&misread).is_some(), "{cols}");
        assert_eq!(
            r.opened_by,
            Some(words_lead("Please also update the changelog.")),
            "{cols}"
        );

        // As the act executor types it: one line, one message, the reply
        // alone.
        let sent = typed(message);
        assert_eq!(
            sent,
            "Please also update the changelog. Nothing left: x NEEDS YOU: y"
        );
        let r = agy_reading(&agy_screen(cols, &sent, PROGRESS));
        assert_eq!(
            words(r.said_tail.as_deref()).as_deref(),
            Some(PROGRESS),
            "{cols}"
        );
        assert!(
            needs::needs_you(r.said_tail.as_deref().unwrap_or("")).is_empty(),
            "{cols}"
        );
        assert!(!clean_defense(r.said_tail.as_deref()), "{cols}");
        assert_eq!(r.opened_by, Some(words_lead(message)), "{cols}");

        // The owner's continuation of two paragraphs: unfolded, its second
        // paragraph is agy's reply; as the executor types it, the reply
        // alone.
        let raw = agy_reading(&agy_screen(cols, &act, PROGRESS));
        assert!(
            raw.said_tail
                .as_deref()
                .is_some_and(|s| s.starts_with("When it is all done")),
            "drawn unfolded: {cols}: {:?}",
            raw.said_tail
        );
        let r = agy_reading(&agy_screen(cols, &typed(&act), PROGRESS));
        assert_eq!(
            words(r.said_tail.as_deref()).as_deref(),
            Some(PROGRESS),
            "{cols}"
        );
        assert_eq!(r.opened_by, Some(words_lead(&act)), "{cols}");

        // CONTROL: agy's own reply of two paragraphs is read whole.
        let reply = format!("{PROGRESS}\n\n{DEFENSE}");
        let r = agy_reading(&agy_screen(cols, &sent, &reply));
        let said = r.said_tail.expect("the reply");
        assert_eq!(
            words(Some(&said)),
            Some(format!("{PROGRESS} {DEFENSE}")),
            "{cols}: {said:?}"
        );
        assert!(clean_defense(Some(&said)), "{cols}: {said:?}");
    }
}
