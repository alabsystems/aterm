// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The turn-end policy in the loop, over the scripted server: what the loop
// types, through which verb, under which guard, and what it raises, under
// the owner's `[harness]` policy (every switch on) as the in-GUI host runs
// it. Each behaviour has its negative control. Included from `run.rs`'s
// tests (`mod turn_end`), so the `Mock` and the helpers there are in scope.

use crate::supervise::policy::turn_end::{
    DONE_CHECK, FINISHED, RULE_CONSENT, RULE_CONTINUE, RULE_DONE_CHECK, RULE_FINISHED, RULE_LEFT,
    RULE_OPEN_ITEMS, RULE_SUGGESTION, open_items_text,
};

#[path = "remembered_limit_tests.rs"]
mod remembered_limit;

/// The host's policy, every switch on (`SupervisorConfig::default()`) but
/// the answers: these scripts end on a request for a decision ([`STOP`]),
/// which `answer_questions = false` hands to a person, so each shows what the
/// loop typed before it, and the escalation after. The answer itself is
/// [`a_stop_phrase_is_answered_with_the_answer_text`]'s.
fn hosted(max_s: u64) -> SuperviseOpts {
    SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        },
        ..auto(max_s, None)
    }
}

/// A turn that ended after 12 minutes of work (the done row's measure: a job,
/// past `EXCHANGE_WORK`, even where a person opened it), the worker's last
/// words `said`, at an empty composer in bypass mode.
fn ended(said: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("⏺ {said}"),
        "",
        "✻ Worked for 12m 2s · done 4:24 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// [`ended`] with Claude Code's dim suggestion in the composer (`❯ keep
/// going`, the cursor left at column 2), or, `filled`, the suggestion
/// accepted into it (the cursor after it).
fn suggesting(said: &str) -> Vec<String> {
    let mut r = ended(said);
    let caret = r.iter().position(|row| row == "❯").expect("caret row");
    r[caret] = "❯ keep going".to_string();
    r
}

/// The worker asks for a decision: the point is escalated, never typed.
const STOP: &str = "I need your decision on the schema before I go on.";

fn ledger_at(tag: &str) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("te-ledger", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("s-1.jsonl");
    (dir, path)
}

/// The continue policy: a turn that ended after real work, with nothing
/// asked, is continued once (`keep going` typed through the guarded submit
/// `turn submit=guarded:^❯\skeep\x20going\s*$ …`, the session's program read
/// first, `CONTINUED` printed, a `typed` ledger row under `continue@v1`), and
/// the next turn, ending on a request for a decision, is escalated with the
/// worker's words, nothing typed. Negative control: the policy switched off
/// types nothing.
#[test]
fn a_turn_end_after_work_is_continued_once_and_a_stop_phrase_escalates() {
    let (dir, ledger) = ledger_at("continue");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            busy_screen(),
            ended(STOP),
        ],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert_eq!(
        lines,
        [
            "EVENT idle seq=102 ⏺ Fixed the parser; the suite is green.".to_string(),
            "CONTINUED seq=102 rule=continue@v1 keep going".to_string(),
            format!("EVENT idle seq=104 ⏺ {STOP}"),
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    let turn = continue_request("keep going");
    assert_eq!(
        turn,
        "turn submit=guarded:^❯\\skeep\\x20going\\s*$ idle=600 timeout=2500 yield=0.2 keep going"
    );
    let at = m
        .requests
        .iter()
        .position(|r| *r == turn)
        .expect("the turn");
    assert_eq!(m.requests[at - 1], "status");
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert_eq!(
        m.attention.as_deref(),
        Some(
            "claude idle: I need your decision on the schema before I go on. (the worker said \
             \"need your decision\"; answer_questions is off)"
        )
    );
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert!(
        rows.lines()
            .any(|l| l.contains("\"rule_id\":\"continue@v1\"")
                && l.contains("\"decision\":\"typed\"")
                && l.contains("\"command\":\"keep going\"")),
        "{rows}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Negative control: the continue switch off.
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.vanish_after = Some(2);
    let mut off = hosted(30);
    off.policy.continue_policy = false;
    let _ = watch_lines(&mut m, &off);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

/// A worker that says it is done is asked once: the done check is typed
/// where `keep going` would be, and when its answer is `DONE` the task is
/// done: nothing more is typed, and the journal says so once (`DONE …`).
/// Negative control: the same turn reported as progress is continued with
/// `keep going`.
#[test]
fn a_done_report_is_checked_once_and_a_done_reply_ends_the_task() {
    let (dir, journal) = journal_file("done-check");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; all done."),
            busy_screen(),
            ended("DONE"),
        ],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(4);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        journal: Some(journal.clone()),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_DONE_CHECK} {}",
            clip(DONE_CHECK)
        )),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "the check alone: {:#?}", m.requests);
    let done: Vec<String> = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with("DONE "))
        .collect();
    assert_eq!(done.len(), 1, "{done:#?}");
    assert!(
        done[0].starts_with(&format!("DONE seq=104 rule={RULE_DONE_CHECK} ")),
        "{done:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Negative control: progress, not a done report.
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
        )),
        "{lines:#?}"
    );
}

/// The request after the write that starts `write` waits for the worker's
/// reaction with [`turn_end_loop::REACTION_WAIT`] (the screen moving) before
/// anything about the write is judged; never the approval press's 2 s
/// settle, which misjudges a worker merely slow to react.
fn assert_reaction_awaited(m: &Mock, write: &str) {
    let next = m
        .requests
        .iter()
        .skip_while(|r| !r.starts_with(write))
        .nth(1);
    let bound = format!(" timeout {}", turn_end_loop::REACTION_WAIT.as_millis());
    assert!(
        next.is_some_and(|r| r.starts_with("await seq ") && r.ends_with(&bound)),
        "after `{write}` the reaction is awaited{bound}: {:#?}",
        m.requests
    );
}

/// The worker's own suggestion, accepted by the vendor's accept key: `right`
/// fenced on the judged read's generation and guarded on the suggestion's
/// row, the screen read again, then Enter fenced on that read and guarded on
/// the filled row (`continue-suggestion@v1`, no `turn`). Negative control: a
/// host without the generation fence gets the same words typed through the
/// guarded submit instead, under the same rule.
#[test]
fn the_workers_suggestion_is_accepted_with_the_accept_key_and_a_fenced_enter() {
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            suggesting("Stage 1 is in."),
            suggesting("Stage 1 is in."),
            busy_screen(),
            ended(STOP),
        ],
    );
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
    m.screen_cols.insert(2, 12);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_SUGGESTION} keep going"
        )),
        "{lines:#?}\n{:#?}",
        m.requests
    );
    let keys: Vec<&String> = m.presses();
    assert_eq!(
        keys,
        [
            "key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right",
            "key if-gen=1.103 if=^❯\\x20keep\\x20going\\s*$ enter",
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert_reaction_awaited(&m, "key if-gen=1.102");

    // Negative control: no generation fence — the words, typed.
    let mut m = Mock::new(
        true,
        vec![busy_screen(), suggesting("Stage 1 is in."), ended(STOP)],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_SUGGESTION} keep going"
        )),
        "{lines:#?}"
    );
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert!(m.requests.contains(&continue_request("keep going")));
}

/// The read after the accept key landed between the worker's clear and its redraw
/// (no caret row) and the loop returned with the suggestion filled: nothing pressed,
/// nothing journaled, nothing escalated. That read is taken again after one more
/// settle, and Enter goes fenced on the read that shows the fill. Negative control: a
/// composer still torn on the second read is not judged either, nothing more is
/// pressed, and the journal says why.
#[test]
fn a_torn_read_after_the_accept_key_is_read_again() {
    let torn = {
        let mut rows = suggesting("Stage 1 is in.");
        let caret = rows
            .iter()
            .position(|row| row == "❯ keep going")
            .expect("caret row");
        rows[caret] = String::new();
        rows
    };
    let accepting = |screens: Vec<Vec<String>>, filled_at: Option<usize>| {
        let mut m = Mock::new(true, screens);
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        m.idle_never = true;
        if let Some(at) = filled_at {
            m.screen_cols.insert(at, 12);
        }
        m.vanish_after = Some(2);
        m
    };
    let mut m = accepting(
        vec![
            busy_screen(),
            suggesting("Stage 1 is in."),
            torn.clone(),
            suggesting("Stage 1 is in."),
            busy_screen(),
            ended(STOP),
        ],
        Some(3),
    );
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_SUGGESTION} keep going"
        )),
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert_eq!(
        m.presses(),
        [
            "key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right",
            "key if-gen=1.104 if=^❯\\x20keep\\x20going\\s*$ enter",
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    // Between the two presses: the settle, the torn read, ONE MORE settle, then
    // the read Enter is fenced on. A read taken again with no settle before it
    // lands inside the same redraw. A read is one or two `text` requests (the
    // tail, then the whole screen when the tail decides nothing), so runs of
    // reads count once.
    let mut between: Vec<&str> = m
        .requests
        .iter()
        .skip_while(|r| !(verb_of(r) == "key" && r.ends_with(" right")))
        .skip(1)
        .take_while(|r| !(verb_of(r) == "key" && r.ends_with(" enter")))
        .filter_map(|r| {
            if r.starts_with("await idle ") {
                Some("settle")
            } else if verb_of(r) == "text" {
                Some("read")
            } else {
                None
            }
        })
        .collect();
    between.dedup();
    assert_eq!(
        between,
        ["settle", "read", "settle", "read"],
        "{:#?}",
        m.requests
    );

    // NEGATIVE CONTROL: still torn when read again — judged on neither read,
    // and journaled.
    let (dir, path) = journal_file("te-accept-torn");
    let mut m = accepting(
        vec![
            busy_screen(),
            suggesting("Stage 1 is in."),
            torn.clone(),
            torn,
        ],
        None,
    );
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        m.presses(),
        ["key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right"],
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        records.iter().any(|r| r.kind == "skipped"
            && r.seq == Some(102)
            && r.summary.contains("no composer after the accept key")),
        "{records:#?}"
    );
}

/// A person's draft whose caret was moved home (←, Home, ctrl-a) sits at
/// column 2 of the caret row, where the placeholder does too. The `cell` at
/// column 2 says which: dim is the placeholder (continued, the suggestion
/// accepted), anything else a typed draft, never typed over. Negative
/// control: the dim cell is the placeholder.
#[test]
fn a_draft_homed_to_column_2_is_never_typed_over() {
    for (attrs, typed) in [("dim", true), ("none", false)] {
        let mut m = Mock::new(
            true,
            vec![busy_screen(), suggesting("Stage 1 is in."), ended(STOP)],
        );
        m.cursor_on_caret = true;
        m.cell_attrs = attrs;
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines(&mut m, &hosted(3));
        assert!(
            m.requests.iter().any(|r| r.starts_with("cell ")),
            "{attrs}: the cell was read: {:#?}",
            m.requests
        );
        assert_eq!(
            m.requests.contains(&continue_request("keep going")),
            typed,
            "{attrs}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

/// A `cell` THE SERVER DOES NOT ANSWER (an older server, a refused verb)
/// cannot say what stands at column 2, and the loop falls back to where the
/// cursor stands: on the caret row the text is the placeholder, as before (the
/// suggestion is accepted and `keep going` typed); anywhere else — a grid
/// drawn one row high, whose cursor sits below the prompt — it may be a
/// person's, and nothing is typed over it. The person's-typing grace is off
/// (`human_grace_s: 0`), so it is the reading of the composer that decides,
/// not the clock a changed draft starts: the three cases differ in one thing
/// each. NEGATIVE CONTROL: the same grid with the cell answered dim is the
/// placeholder (the fallback is what holds the unanswered one back).
#[test]
fn a_cell_the_server_does_not_answer_is_the_placeholder_only_on_the_caret_row() {
    let screen = suggesting("Stage 1 is in.");
    let caret = screen
        .iter()
        .position(|r| r.starts_with('❯'))
        .expect("caret row");
    // (the cursor's row from the caret row, the cell readable, `keep going` typed)
    for (off, readable, typed) in [(0, false, true), (1, false, false), (1, true, true)] {
        let mut m = Mock::new(true, vec![busy_screen(), screen.clone(), ended(STOP)]);
        m.cursor_row = Some(caret + off);
        m.cell_unreadable = !readable;
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines(&mut m, &opts);
        assert!(
            m.requests.iter().any(|r| r.starts_with("cell ")),
            "cursor {off} rows off, readable {readable}: the cell was asked: {:#?}",
            m.requests
        );
        assert_eq!(
            m.requests.contains(&continue_request("keep going")),
            typed,
            "cursor {off} rows off, readable {readable}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

/// A 529 END OF TURN (`API Error: 529 Overloaded` under the last message:
/// idle to the eye) is waited out — the backoff (50 ms here) journaled as
/// `WAITING` and waited as the deadline of the loop's own wait, no sleep —
/// then continued EXACTLY ONCE under `api-retry@v1`; it raises no badge.
/// Negative control: the retry switch off escalates it at once.
#[test]
fn a_529_waits_its_backoff_then_continues_exactly_once() {
    let (dir, path) = journal_file("te-529");
    let end_529 = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    let mut m = Mock::new(
        true,
        vec![busy_screen(), end_529.clone(), busy_screen(), ended(STOP)],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            retry_backoff: vec![Duration::from_millis(50); 3],
            ..TurnEndTiming::default()
        });
    });
    assert_eq!(count(&m, "turn"), 1, "{lines:#?}\n{:#?}", m.requests);
    // The act names the wall (`wall_retry_text`), never the rote `keep
    // going`, and never the screen's words.
    assert!(
        lines.iter().any(|l| l.starts_with(
            "CONTINUED seq=102 rule=api-retry@v1 Claude Code reported an API error (overloaded)."
        )),
        "{lines:#?}"
    );
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let point = records
        .iter()
        .find(|r| r.kind == "event" && r.seq == Some(102))
        .expect("the 529 point");
    let waiting = records
        .iter()
        .find(|r| r.kind == "waiting")
        .expect("a WAITING row");
    assert!(
        waiting.summary.ends_with("overloaded retry 1"),
        "{}",
        waiting.summary
    );
    let continued = records
        .iter()
        .find(|r| r.kind == "continued")
        .expect("the continuation");
    assert!(continued.t - point.t >= 50, "{} {}", continued.t, point.t);
    // The badge is the stop phrase's, after it: the 529 raised none.
    assert_eq!(count(&m, "meta set attention"), 1, "{:#?}", m.requests);

    // Negative control: retries off — escalated as a wall, nothing typed.
    let mut m = Mock::new(true, vec![busy_screen(), end_529]);
    m.vanish_after = Some(2);
    let mut off = hosted(30);
    off.policy.retry_api_errors = false;
    let _ = watch_lines(&mut m, &off);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.starts_with("claude wall: API Error: 529 Overloaded")),
        "{:?}",
        m.attention
    );
}

/// Owner decision 3, in a loop with no host to relaunch the agent (`drive
/// watch`, D7): a Fable limit is waited out to its reset, never Claude's own
/// `/model` (which also saves the person's default), and raises no badge:
/// the wall is handled, journaled `LIMITED … handled`. (Where a host
/// relaunches it, the fallback is a relaunch with `--model`:
/// `run_engine_tests`' `a_model_bucket_is_relaunched_…`.) A bucket that asks
/// consent to go on on usage credits is accepted instead: continued under
/// `consent-accept@v1`, and no badge.
#[test]
fn a_fable_limit_with_no_host_to_relaunch_is_waited_out_never_by_model() {
    let (dir, path) = journal_file("te-fable");
    let mut fable = rows(&[
        "⏺ Running the suite.",
        "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch \
         models with /model.",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    fable.extend(composer("  ? for shortcuts"));
    let mut m = Mock::new(true, vec![busy_screen(), fable.clone()]);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(3)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !m.requests.iter().any(|r| r.contains("/model")),
        "never /model: {:#?}",
        m.requests
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        records
            .iter()
            .any(|r| r.kind == "limited" && r.summary.starts_with("handled: You've reached")),
        "{records:#?}"
    );
    assert!(
        records
            .iter()
            .any(|r| r.kind == "waiting" && r.summary.contains("until=")),
        "the reset waited out: {records:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);

    // The consent notice: accepted, continued, no switch, and no `limited:`
    // badge (owner, 2026-09-24).
    let mut consent = rows(&[
        "⏺ Running the suite.",
        "  ⎿  Fable limit reached · continuing on Sonnet uses usage credits, and the prompt \
         to confirm",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    consent.extend(composer("  ? for shortcuts"));
    let mut m = Mock::new(true, vec![busy_screen(), consent]);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        m.requests.contains(&continue_request("keep going")),
        "{:#?}",
        m.requests
    );
    assert!(
        lines.contains(&format!("CONTINUED seq=102 rule={RULE_CONSENT} keep going")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        !m.attention
            .as_deref()
            .is_some_and(|a| a.starts_with("limited: ")),
        "{:?}",
        m.attention
    );
}

/// A full context is compacted (`/compact` under `context-compact@v1`) and,
/// once the compaction has run, continued under the same rule.
#[test]
fn a_full_context_is_compacted_then_continued() {
    let mut full = rows(&[
        "⏺ Reading the rest of the logs.",
        "  ⎿  Context limit reached · /compact or /clear to continue",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    full.extend(composer("  ? for shortcuts"));
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            full,
            busy_screen(),
            ended("Compacted; picking the logs up again."),
            busy_screen(),
            ended(STOP),
        ],
    );
    m.turn_gates = vec![1, 3];
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    let decided: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        decided,
        [
            "TYPED seq=102 rule=context-compact@v1 /compact",
            "CONTINUED seq=104 rule=context-compact@v1 keep going",
        ],
        "{lines:#?}\n{:#?}",
        m.requests
    );
}

/// A CONTEXT NO `/compact` LOWERS IS ONE EXCEPTION across the ladder's rungs, and
/// ends when it is lowered. The third failed `/compact` raises `EXCEPTION
/// kind=context`; each `/compact` the loop types after it reads BUSY (the
/// compaction's own spinner, which ends any exception over a worker that works)
/// and fails again, three rungs running, so the line must neither end nor come
/// back around them: ONE `EXCEPTION`, the host told once, and a `CLEARED` only
/// when a compaction worked and the wall left. Over the screens of Claude Code
/// 2.1.286 (MEASURED 2026-10-01): the failed `/compact`s, the spinner, and the
/// compaction that worked. NEGATIVE CONTROL: before the third failure nothing is
/// raised.
#[test]
fn a_context_no_compact_lowers_is_one_exception_across_the_rungs_until_it_is_lowered() {
    use aterm_phase::prompt::fixtures as fx;
    let (dir, path) = journal_file("te-context-hopeless");
    let host = Arc::new(LadderHost::default());
    let frame = |f: &str| fx::screen(f);
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            frame(fx::CONTEXT_WALL_MEASURED),
            frame(fx::COMPACT_RUNNING_MEASURED),
            frame(fx::COMPACT_FAILED_1_MEASURED),
            frame(fx::COMPACT_RUNNING_MEASURED),
            frame(fx::COMPACT_FAILED_2_MEASURED),
            frame(fx::COMPACT_RUNNING_MEASURED),
            frame(fx::COMPACT_FAILED_3_MEASURED),
            frame(fx::COMPACT_RUNNING_MEASURED),
            frame(fx::COMPACT_FAILED_3_MEASURED),
            frame(fx::COMPACT_RUNNING_MEASURED),
            frame(fx::COMPACT_FAILED_3_MEASURED),
            frame(fx::COMPACT_RUNNING_MEASURED),
            frame(fx::COMPACT_FAILED_3_MEASURED),
            frame(fx::COMPACT_RUNNING_MEASURED),
            frame(fx::COMPACT_SUCCEEDED_MEASURED),
            busy_screen(),
            ended(STOP),
        ],
    );
    m.turn_gates = vec![1, 3, 5, 7, 9, 11, 13, 15];
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            retry_backoff: vec![Duration::from_millis(50); 7],
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let story: Vec<&str> = records
        .iter()
        .filter_map(|r| match r.kind.as_str() {
            "exception" => Some("EXCEPTION"),
            "cleared" => Some("CLEARED"),
            "typed" | "continued" if r.line.contains("/compact") => Some("/compact"),
            "typed" | "continued" => Some("keep going"),
            _ => None,
        })
        .collect();
    let compact = "/compact";
    assert_eq!(
        story,
        [
            compact,
            compact,
            compact,
            "EXCEPTION",
            compact,
            compact,
            compact,
            compact,
            "CLEARED",
            "keep going"
        ],
        "{lines:#?}"
    );
    let said = |kind: &str| records.iter().find(|r| r.kind == kind).map(|r| &r.line);
    let (raised, ended) = (said("exception").unwrap(), said("cleared").unwrap());
    assert!(raised.contains("kind=context when=later"), "{raised}");
    assert!(ended.contains(" exception=context after="), "{ended}");
    assert_eq!(
        *host.told.lock().unwrap(),
        ["exception:context", "exception:none"],
        "the host hears it once, and its end"
    );
}

/// A LOST LOGIN: `/login` typed under `auth-login@v1`, then a badge — "finish
/// sign-in in the browser" — that no point carries; the dialog (no composer)
/// gets nothing typed; the worker working again clears the badge.
#[test]
fn a_lost_login_types_login_and_its_badge_goes_when_the_worker_works() {
    let (dir, path) = journal_file("te-login");
    let mut lost = rows(&[
        "⏺ Pushing the branch.",
        "  ⎿  Not logged in · Please run /login",
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    lost.extend(composer("  ? for shortcuts"));
    let dialog = rows(&[
        " Browser didn't open? Use the url below to sign in (⌘/ctrl + click):",
        " https://claude.ai/oauth/authorize?code=true",
        "",
        " Paste code here if prompted >",
    ]);
    let mut m = Mock::new(
        true,
        vec![busy_screen(), lost, dialog, busy_screen(), ended("Pushed.")],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let mut opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    // The point after the login is continued by the policy; keep it quiet.
    opts.policy.continue_policy = false;
    let (lines, _) = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        lines.contains(&"TYPED seq=102 rule=auth-login@v1 /login".to_string()),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        m.requests.iter().any(|r| r.starts_with(
            "meta set attention owner=supervisor claude wall: Not logged in · Please run /login \
             (finish sign-in in the browser)"
        )),
        "{:#?}",
        m.requests
    );
    assert!(
        records
            .iter()
            .any(|r| r.kind == "cleared" && r.summary.starts_with("turn-end attention=OK")),
        "{records:#?}"
    );
    assert_eq!(m.attention, None, "cleared once the worker worked");
}

/// Claude Code 2.1.281's `/login` method picker (`Select login method:` and
/// its three options, from the binary's strings), in the frame it draws
/// under the typed command, no composer. Hand-built.
fn login_picker() -> Vec<String> {
    let mut r = login_expired();
    r.truncate(r.len() - 4);
    r.extend(rows(&[
        "❯ /login",
        "",
        &"─".repeat(100),
        " Claude Code can be used with your Claude subscription or billed based on API usage \
         through your Console account.",
        "",
        " Select login method:",
        "",
        " ❯ 1. Claude account with subscription · Pro, Max, Team, or Enterprise",
        "   2. Anthropic Console account · API usage billing",
        "   3. 3rd-party platform · Amazon Bedrock, Microsoft Foundry, or Vertex AI",
        "",
    ]));
    r
}

/// The supervisor's `continue` answered by `⏺ Login expired · Please run
/// /login` ([`aterm_phase::prompt::fixtures::LOGIN_EXPIRED`]).
fn login_expired() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED)
}

/// [`login_expired`] once the person's `/login` is done: its command and
/// Claude Code's `Login successful` under the wall's row.
fn login_back() -> Vec<String> {
    let mut r = login_expired();
    let at = r
        .iter()
        .position(|row| row.starts_with("⏺ Login expired"))
        .expect("the wall row");
    r.splice(
        at + 1..at + 1,
        rows(&["", "❯ /login", "  ⎿  Login successful"]),
    );
    r
}

/// The login wall in the loop: the supervisor's own `continue` answered by
/// `⏺ Login expired · Please run /login`. `/login` is typed once under
/// `auth-login@v1` and the owner is told as it is (the badge `claude wall:
/// Login expired · Please run /login (finish sign-in in the browser)`), and
/// the method picker it opens (no composer) is handed to a person with no
/// key pressed: which account to sign in with is theirs. Nothing is
/// continued until the login is back: at the person's `Login successful` the
/// worker is continued once, and its next turn end is the ordinary policy's.
/// Negative control: no `keep going` is typed while the wall stands.
#[test]
fn the_login_expired_row_is_told_once_and_the_login_back_is_continued() {
    let (dir, path) = journal_file("te-login-expired");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            login_expired(),
            login_picker(),
            login_back(),
            busy_screen(),
            ended(STOP),
        ],
    );
    m.turn_gates = vec![1, 3];
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let decided: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        decided,
        [
            "TYPED seq=102 rule=auth-login@v1 /login",
            "CONTINUED seq=104 rule=continue@v1 keep going",
        ],
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert!(
        m.requests.iter().any(|r| r.starts_with(
            "meta set attention owner=supervisor claude wall: Login expired · Please run /login \
             (finish sign-in in the browser)"
        )),
        "{:#?}",
        m.requests
    );
    assert!(
        !m.requests.iter().any(|r| r.starts_with("key")),
        "no key pressed on the login picker: {:#?}",
        m.requests
    );
    assert!(
        m.requests.iter().any(|r| r.starts_with(
            "meta set attention owner=supervisor claude other: Claude Code can be used with your \
             Claude subscription"
        )),
        "the picker is a person's: {:#?}",
        m.requests
    );
    assert!(
        records.iter().any(|r| r.kind == "escalated"),
        "{records:#?}"
    );
    assert!(
        lines.contains(&"EVENT idle seq=104 ⎿  Login successful".to_string()),
        "{lines:#?}"
    );
}

/// Never over a person: a draft in the composer (the cursor after it) gets
/// nothing typed and raises nothing within the grace (a draft first seen is
/// a keystroke just made; [`a_draft_left_standing_is_submitted_once_the_
/// grace_has_passed`] is what comes after it); a session whose program is a
/// shell is never typed into (the program read fresh before the act,
/// `SKIPPED` journaled); on a host without the send fence every `turn`
/// carries `yield=0.2`, and a person typing past the yield (`ERR yield
/// timeout`) is no act and raises nothing; a guarded submit whose guard
/// missed (`skipped`) is no act (no `CONTINUED`, no ledger row) and, the text
/// left typed, is escalated, never left for the next look to read as a
/// person's draft. The control is the first test.
#[test]
fn a_draft_a_shell_and_a_missed_guard_type_nothing() {
    let mut drafted = ended("Fixed the parser; the suite is green.");
    let caret = drafted.iter().position(|r| r == "❯").expect("caret");
    drafted[caret] = "❯ wait, check the lexer".to_string();
    let mut m = Mock::new(true, vec![busy_screen(), drafted]);
    m.cursor_col = Some(25);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}");
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);

    let (dir, path) = journal_file("te-shell");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.program = "zsh";
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let _ = watch_lines(&mut m, &opts);
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        records
            .iter()
            .any(|r| r.kind == "skipped" && r.summary.contains("runs zsh, no agent")),
        "{records:#?}"
    );

    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.verb_replies
        .insert("turn", VecDeque::from([err("yield timeout momentum=0.61")]));
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        m.requests.contains(&continue_request("keep going")),
        "{:#?}",
        m.requests
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);

    let (dir, ledger) = ledger_at("missed");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.verb_replies.insert(
        "turn",
        VecDeque::from([ok(
            "OK 0 turn skipped reason=guard submitted=0 seq=102 id=1\n",
        )]),
    );
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        !std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .contains("\"decision\":\"typed\""),
        "no act recorded"
    );
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.contains("(typed, not submitted (")),
        "{:?}\n{:#?}",
        m.attention,
        m.requests
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A continuation whose Enter was written and not seen to land
/// (`submitted=0 … pressed=<n>`, any n ≥ 1: a screen that never holds still
/// keeps the `turn`'s echo settle open to its deadline — Claude Code 2.1.301,
/// measured 2026-10-16) is an act typed, journaled UNVERIFIED: typing it
/// again would double it. Control: the same timeout with no Enter written
/// (`pressed=0`) is no act.
#[test]
fn an_enter_written_and_not_seen_is_a_continuation_typed() {
    let timed_out = |pressed: u32| CtlReply {
        code: 124,
        stdout: format!(
            "OK 50 turn submitted=0 status=timeout seq=102 id=1 dur_ms=2501 \
             hash=0000000000000000 pressed={pressed}\n"
        ),
        stderr: String::new(),
    };
    for pressed in [1, 2, 0] {
        let (dir, ledger) = ledger_at(&format!("unverified-{pressed}"));
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
            ],
        );
        m.verb_replies
            .insert("turn", VecDeque::from([timed_out(pressed)]));
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let typed = std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .contains("\"decision\":\"typed\"");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(count(&m, "turn"), 1, "pressed={pressed}: {:#?}", m.requests);
        assert_eq!(typed, pressed > 0, "pressed={pressed}: {lines:#?}");
        assert_eq!(
            lines.iter().any(|l| l.starts_with("CONTINUED")),
            pressed > 0,
            "pressed={pressed}: {lines:#?}"
        );
    }
}

/// An offer is a continuation: `… want me to take that on?` reads as a
/// question and is continued, not escalated. Negative control: a plain
/// question for a person is escalated and nothing is typed.
#[test]
fn an_offer_is_continued_and_a_question_is_escalated() {
    let offer = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_OFFER);
    assert_eq!(worker_phase(&offer), Phase::Question);
    let mut m = Mock::new(true, vec![busy_screen(), offer, busy_screen(), ended(STOP)]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
        )),
        "{lines:#?}"
    );
    let asks: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("meta set attention"))
        .collect();
    assert_eq!(asks.len(), 1, "only the stop phrase's: {asks:#?}");

    let mut m = Mock::new(
        true,
        vec![busy_screen(), ended("Did the suite pass on your machine?")],
    );
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &hosted(30));
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert_eq!(
        m.attention.as_deref(),
        Some(
            "claude question: Did the suite pass on your machine? (the worker asked a question; \
             answer_questions is off)"
        )
    );
}

/// Scenario 19: the question in prose follows the session's priority word,
/// in the loop, under the owner's table that answers (full power) and over
/// its `answer_questions = false` ([`hosted`]) alike ("global, and then
/// session"; contract decision 7): the prose path and the dialog path
/// resolve the same word, so they cannot disagree. A session whose
/// `questions` word is `long-term` has a question asked in prose answered
/// with the long-term policy text (the one its dialogs' free-text row gets)
/// in place of `answer_text`, under `prose-long-term@v1`, printed
/// `CONTINUED` and ledgered `typed`, no badge; the word read with one `meta`
/// for the point. `short-term` the same with its text; `recommended` types
/// `answer_text` under `answer@v1`. Negative controls: a point that asks
/// nothing reads no `meta` for it; under the switch that is off, no session
/// word and the session's `ask` leave the table's escalation (its own reason,
/// `answer_questions is off`, nothing typed); a loop that gives no session a
/// say (`--no-answer`: `session_questions` off) reads no word and escalates.
/// (The session's `ask` under a table that answers is
/// [`a_sessions_ask_holds_its_dialogs_never_a_question_in_prose`]'s.)
#[test]
fn a_question_in_prose_is_answered_by_the_sessions_priority_word() {
    use crate::supervise::policy::question::{LONG_TERM_TEXT, SHORT_TERM_TEXT};
    use crate::supervise::policy::turn_end::RULE_ANSWER;
    use crate::supervise::policy::{RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM};
    let answer_text = SupervisorConfig::default().answer_text;
    let full = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let off = hosted(30);
    for opts in [&full, &off] {
        let on = opts.policy.answer_questions;
        for (word, text, rule) in [
            (
                "long-term",
                LONG_TERM_TEXT.to_string(),
                RULE_PROSE_LONG_TERM,
            ),
            (
                "short-term",
                SHORT_TERM_TEXT.to_string(),
                RULE_PROSE_SHORT_TERM,
            ),
            ("recommended", answer_text.clone(), RULE_ANSWER),
        ] {
            let (dir, ledger) = ledger_at(&format!("prose-{word}-{on}"));
            let mut m = Mock::new(true, vec![busy_screen(), ended(PROSE_ASK)]);
            m.turn_releases = Some(1);
            m.vanish_after = Some(2);
            m.human = vec!["null"];
            m.questions = Some(word);
            let (lines, _) = watch_lines_with(&mut m, opts, |s| {
                s.set_approval_ledger(Some(ledger.clone()));
            });
            assert!(
                lines.contains(&format!("CONTINUED seq=102 rule={rule} {}", clip(&text))),
                "{word} {on}: {lines:#?}\n{:#?}",
                m.requests
            );
            assert!(
                m.requests.contains(&continue_request(&text)),
                "{word} {on}: {:#?}",
                m.requests
            );
            assert_eq!(
                m.requests.iter().filter(|r| *r == "meta").count(),
                2,
                "{word} {on}: the start-up reconcile and ONE read of the word: {:#?}",
                m.requests
            );
            assert_eq!(m.attention, None, "{word} {on}: {:#?}", m.requests);
            let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
            assert!(
                rows.lines()
                    .any(|l| l.contains(&format!("\"rule_id\":\"{rule}\""))
                        && l.contains("\"decision\":\"typed\"")),
                "{word} {on}: {rows}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    // A point that asks nothing spends no `meta` on the word (the watch's
    // start-up reconcile is its one bare `meta`).
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    m.questions = Some("long-term");
    let _ = watch_lines(&mut m, &full);
    assert_eq!(
        m.requests.iter().filter(|r| *r == "meta").count(),
        1,
        "{:#?}",
        m.requests
    );
    // Under the switch that is off, no word and `ask` leave the escalation.
    for word in [None, Some("ask")] {
        let mut m = Mock::new(true, vec![busy_screen(), ended(PROSE_ASK)]);
        m.vanish_after = Some(2);
        m.human = vec!["null"];
        m.questions = word;
        let _ = watch_lines(&mut m, &off);
        assert_eq!(count(&m, "turn"), 0, "{word:?}: {:#?}", m.requests);
        assert_eq!(
            m.attention.as_deref(),
            Some(
                "claude question: Should I rewrite the parser or patch the lexer? (the worker \
                 asks to choose between options; answer_questions is off)"
            ),
            "{word:?}"
        );
        // The word IS read where it could have answered: the one request
        // this point spends beyond main's escalation.
        assert_eq!(
            m.requests.iter().filter(|r| *r == "meta").count(),
            2,
            "{word:?}: {:#?}",
            m.requests
        );
    }
    let no_say = SuperviseOpts {
        policy: SupervisorConfig {
            session_questions: false,
            ..hosted(30).policy
        },
        ..hosted(30)
    };
    let mut m = Mock::new(true, vec![busy_screen(), ended(PROSE_ASK)]);
    m.vanish_after = Some(2);
    m.human = vec!["null"];
    m.questions = Some("long-term");
    let _ = watch_lines(&mut m, &no_say);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert_eq!(
        m.requests.iter().filter(|r| *r == "meta").count(),
        1,
        "no word read where the loop gives no session a say: {:#?}",
        m.requests
    );
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.ends_with("answer_questions is off)")),
        "{:?}",
        m.attention
    );
}

/// The question scenarios 19 and after ask in prose.
const PROSE_ASK: &str = "Should I rewrite the parser or patch the lexer?";

/// The session's `ask` hands the session's question dialogs to a person (how
/// a controller takes a worker's questions, answered with `aterm drive
/// answer`, handed back by `meta unset questions`), never a question the
/// worker asks in prose, which no `drive answer` can answer and no hand-back
/// poll watches. Under the owner's table, which answers, such a question is
/// answered: `answer_text` under `answer@v1`, no badge, and under a global
/// `long-term` by the table's text. A word this build does not know is
/// `ask`'s twin here. (The same session's dialog is still held, nothing
/// pressed, escalated naming the session's `ask`, answered within 2 s of the
/// hand-back: `the_sessions_word_decides_its_question_dialog_over_the_switch`
/// and `a_dialog_held_by_the_policy_is_answered_when_handed_back`.)
#[test]
fn a_sessions_ask_holds_its_dialogs_never_a_question_in_prose() {
    use crate::supervise::policy::RULE_PROSE_LONG_TERM;
    use crate::supervise::policy::question::LONG_TERM_TEXT;
    let answer_text = SupervisorConfig::default().answer_text;
    let full = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let long = SuperviseOpts {
        policy: SupervisorConfig {
            question_priority: crate::supervise::QuestionPriority::LongTerm,
            ..SupervisorConfig::default()
        },
        ..auto(30, None)
    };
    for (word, opts, text, rule) in [
        ("ask", &full, answer_text.clone(), "answer@v1"),
        ("sometimes", &full, answer_text.clone(), "answer@v1"),
        (
            "ask",
            &long,
            LONG_TERM_TEXT.to_string(),
            RULE_PROSE_LONG_TERM,
        ),
    ] {
        let mut m = Mock::new(true, vec![busy_screen(), ended(PROSE_ASK)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        m.human = vec!["null"];
        m.questions = Some(word);
        let (lines, _) = watch_lines(&mut m, opts);
        assert!(
            lines.contains(&format!("CONTINUED seq=102 rule={rule} {}", clip(&text))),
            "{word}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert!(
            m.requests.contains(&continue_request(&text)),
            "{word}: {:#?}",
            m.requests
        );
        assert_eq!(m.attention, None, "{word}: {:#?}", m.requests);
    }
}

/// The default prose answer, request for request: with the owner's default
/// table and no session word, a question asked in prose makes the same
/// decisions, keys and text as with no session say, and its requests add
/// exactly one bare `meta`, the session's `questions` word, read right before
/// the act's `status` (contract decision 7: a word set at any time is the one
/// the question is answered by). Pinned two ways, up to the answer: against
/// a loop that gives no session a say (`session_questions` off, request for
/// request), and as a literal list for this screen (its two busy waits
/// scoped to the live zone: the first capped at 3 s, the second over `rows 2
/// 7`) with that one `meta` inserted, so a second read, a read moved, or a
/// read dropped all show here.
#[test]
fn a_default_prose_answer_is_mains_requests_and_one_meta() {
    let answer_text = SupervisorConfig::default().answer_text;
    let answered = |policy: SupervisorConfig| {
        let opts = SuperviseOpts {
            policy,
            ..auto(30, None)
        };
        let mut m = Mock::new(true, vec![busy_screen(), ended(PROSE_ASK)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        m.human = vec!["null"];
        let (lines, _) = watch_lines(&mut m, &opts);
        assert!(
            lines.contains(&format!("CONTINUED seq=102 rule=answer@v1 {answer_text}")),
            "{lines:#?}"
        );
        assert_eq!(m.attention, None, "{:#?}", m.requests);
        let turn = continue_request(&answer_text);
        let at = m
            .requests
            .iter()
            .position(|r| *r == turn)
            .expect("the answer");
        m.requests[..=at].to_vec()
    };
    let default = answered(SupervisorConfig::default());
    let mut mains = answered(SupervisorConfig {
        session_questions: false,
        ..SupervisorConfig::default()
    });
    let status = mains.len() - 2;
    assert_eq!(mains[status], "status", "{mains:#?}");
    mains.insert(status, "meta".to_string());
    assert_eq!(default, mains, "main's, and the word read before the act");
    let turn = continue_request(&answer_text);
    assert_eq!(
        default,
        [
            "meta",
            // The busy waits are the live zone's: the first, armed before any
            // read, over every row and capped; the next over the rows the
            // read placed the live zone in.
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            "await gone esc.to.interrupt rows 2 7 timeout 20000",
            "text --json tail=40",
            "meta",
            "status",
            turn.as_str(),
        ],
        "main 6b1da64d9's list for this screen (its busy waits scoped to the live zone), \
         and the one `meta` before `status`"
    );
}

/// A continuation whose reply ends inside the `turn` verb's own settle is
/// never read busy. Its point is waited for [`TurnEndTiming::take_within`]
/// (the deadline of the loop's own wait, no sleep), then judged as the short
/// yield it is: backed off ([`TurnEndTiming::short_backoff`]) and continued
/// once more at the same point ([`Session::turn_end_now`]), never escalated
/// as "done" (the back-off grows instead).
#[test]
fn a_reply_inside_the_settle_is_judged_at_its_deadline_not_latched() {
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Stage 1 done."),
            ended("All finished, nothing left."),
            ended("Still nothing left to do."),
        ],
    );
    m.turn_gates = vec![1, 2];
    m.vanish_after = Some(40);
    m.stall_sleep = Some(Duration::from_millis(5));
    let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_millis(60),
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    });
    let continued: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        continued[..2],
        [
            "CONTINUED seq=102 rule=continue@v1 keep going",
            "CONTINUED seq=103 rule=continue@v1 keep going",
        ],
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert!(count(&m, "turn") >= 2, "{:#?}", m.requests);
    assert_eq!(m.attention, None, "never escalated as done: {lines:#?}");
}

/// The `help` a host with both fences answers (`key` and `send` each name
/// `if-gen=`), as the Mock serves it for any `help <verb>`.
const FENCED_HELP: &str = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n\
                           send [id=<key>] [if=<re>] [if-gen=<e.s>] <text>: write text\n";

/// [`ended`] with `text` typed into its composer (the cursor after it).
fn typed_in(said: &str, text: &str) -> Vec<String> {
    let mut r = ended(said);
    let caret = r.iter().position(|row| row == "❯").expect("caret row");
    r[caret] = format!("❯ {text}");
    r
}

/// The continuation is never spliced into a draft a person started between
/// the read and the paste: where the host fences `send`, the text is written
/// only while the screen is the judged read's (`send if-gen=<g> if=<the
/// composer row> -- <text>`), and Enter goes, fenced on the read that shows
/// the composer holding exactly that text, under `continue@v1`, no `turn`.
/// The continuation carries no person fence (`if-human=` is the question
/// answer's and the decline's, never the turn-end policy's): on a host whose
/// reads carry the person count (`human_seq`) and on one whose reads do not,
/// the same two requests. Negative controls: a screen that moved before the
/// write gets nothing written and no Enter; a composer that does not hold
/// the text as written gets no Enter and is escalated.
#[test]
fn a_fenced_host_writes_the_continuation_only_on_the_judged_screen() {
    for human_seq in [None, Some(3)] {
        let (dir, ledger) = ledger_at(&format!("fenced-{}", human_seq.is_some()));
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
                typed_in("Fixed the parser; the suite is green.", "keep going"),
                busy_screen(),
                ended(STOP),
            ],
        );
        m.gen_fence = true;
        m.sends_gen = true;
        m.human_seq = human_seq;
        m.help = FENCED_HELP.to_string();
        m.screen_cols.insert(2, 12);
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        assert!(
            lines.contains(&format!(
                "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
            )),
            "{human_seq:?}: {lines:#?}\n{:#?}",
            m.requests
        );
        let writes: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("send ") || r.starts_with("key "))
            .collect();
        assert_eq!(
            writes,
            [
                "send if-gen=1.102 if=^❯\\s*$ -- keep going",
                "key if-gen=1.103 if=^❯\\x20keep\\x20going\\s*$ enter",
            ],
            "{human_seq:?}: {:#?}",
            m.requests
        );
        assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
        assert_reaction_awaited(&m, "send if-gen=1.102");
        assert!(
            std::fs::read_to_string(&ledger)
                .unwrap_or_default()
                .contains("\"decision\":\"typed\""),
            "{human_seq:?}: the act recorded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Negative control: the screen moved before the write (a person's first
    // keystroke) — nothing written, no Enter, no act.
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = FENCED_HELP.to_string();
    m.verb_replies.insert(
        "send",
        VecDeque::from([ok("OK skipped reason=changed seq=103\n")]),
    );
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        !m.requests.iter().any(|r| r.starts_with("key ")),
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "never an unfenced paste after a skip");

    // Negative control: the composer holds something else after the write
    // (a person typed into it too) — no Enter; escalated.
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            typed_in("Fixed the parser; the suite is green.", "wkeep going"),
        ],
    );
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = FENCED_HELP.to_string();
    m.screen_cols.insert(2, 13);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        !m.requests.iter().any(|r| r.starts_with("key ")),
        "{:#?}",
        m.requests
    );
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.contains("(typed, not submitted (")),
        "{:?}",
        m.attention
    );
}

/// A CONTINUATION THE SERVER REFUSED FOR NOW IS TYPED ONCE IT CAN BE (the
/// sweep of 2026-10-09, Claude Code 2.1.295, `continue`): an idle point's
/// act meets `ERR halted` (an owner's `hold`) or `ERR busy …` (`ERR rate`
/// takes the same branch);
/// the loop parks on the hold (`HALTED`, then `UNHALTED` once `status` reads
/// `hold=0`) or backs off, and decides the point again and types the act,
/// though the screen never moves — on the guarded `turn` and on the fenced
/// `send`. Measured before: after `UNHALTED` nothing was typed for 16 minutes
/// and nobody was told, since an idle point that stands moves nothing the
/// loop's wait latches on. The session ends on the fact (the second write),
/// its stalls a hang detector.
#[test]
fn a_continuation_a_refusal_turned_away_is_typed_once_it_can_be() {
    for refusal in ["halted reason=owner origin=local", "busy lease=s-9"] {
        let (jdir, journal) = journal_file("refused-continue");
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
            ],
        );
        m.verb_replies
            .insert("turn", VecDeque::from([err(refusal)]));
        if refusal.starts_with("halted") {
            m.hold = "1";
            m.hold_lifts_after = Some(2);
        }
        m.stall_sleep = Some(Duration::from_millis(20));
        m.vanish_after = Some(1500);
        m.exit_when = Some(|m| m.turns >= 2);
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..hosted(60)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        assert_eq!(count(&m, "turn"), 2, "{refusal}: {:#?}", m.requests);
        // The same point, read again as the wait reads it (the mock's seq
        // moves with each read of the screen that never changed).
        assert!(
            lines.iter().any(|l| l.starts_with("CONTINUED seq=")
                && l.ends_with(&format!("rule={RULE_CONTINUE} keep going"))),
            "{refusal}: {lines:#?}"
        );
        let (records, _) = journal_records(&journal);
        let notes: Vec<&str> = records.iter().map(|r| r.line.as_str()).collect();
        let lifted = if refusal.starts_with("halted") {
            "UNHALTED seq=102"
        } else {
            "REFUSED seq=102"
        };
        assert!(
            notes.iter().any(|n| n.starts_with(lifted)),
            "{refusal}: {notes:#?}"
        );
        let _ = std::fs::remove_dir_all(&jdir);
    }

    // The fenced host's `send`, refused by the hold: written again once it
    // lifts, on the screen that never moved.
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = FENCED_HELP.to_string();
    m.send_replies
        .push_back(err("halted reason=owner origin=local"));
    m.hold = "1";
    m.hold_lifts_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(20));
    m.vanish_after = Some(1500);
    m.exit_when = Some(|m| m.requests.iter().filter(|r| r.starts_with("send ")).count() >= 2);
    let _ = watch_lines(&mut m, &hosted(60));
    let sends: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("send "))
        .collect();
    assert_eq!(sends.len(), 2, "{:#?}", m.requests);
    assert_eq!(sends[0], "send if-gen=1.102 if=^❯\\s*$ -- keep going");
    assert!(
        sends[1].starts_with("send if-gen=1.") && sends[1].ends_with(" if=^❯\\s*$ -- keep going"),
        "{sends:#?}"
    );
}

/// A REFUSAL THAT PERSISTS IS JOURNALED ONCE, AND AGAIN WHEN IT CHANGES (the
/// review of 2026-10-14). A continuation the server turns away is decided
/// again a moment later and written again — nothing wrong is typed — and a
/// refusal that stands (another driver's lease, `ERR busy lease=…`; a hold
/// on a host whose `status` names none, so the park cannot wait it out)
/// meets every retry, one every ~10 s for as long as it stands. The journal
/// says it when it first appears and when it changes (another holder), not
/// at every retry: before, each retry wrote its own `REFUSED`/`HALTED` line.
/// The run ends on the fact (the write taken), its stalls a hang detector;
/// the turns written show every retry was still made.
#[test]
fn a_refusal_that_persists_is_journaled_once_and_again_when_it_changes() {
    let run = |tag: &str, refusals: &[&str], hold: &'static str| {
        let (jdir, journal) = journal_file(tag);
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
            ],
        );
        m.verb_replies
            .insert("turn", refusals.iter().map(|r| err(r)).collect());
        m.hold = hold;
        m.stall_sleep = Some(Duration::from_millis(20));
        m.vanish_after = Some(3000);
        let taken = refusals.len() + 1;
        m.exit_when = Some(match taken {
            3 => |m: &Mock| m.turns >= 3,
            _ => |m: &Mock| m.turns >= 4,
        });
        let opts = SuperviseOpts {
            journal: Some(journal.clone()),
            ..hosted(90)
        };
        let _ = watch_lines(&mut m, &opts);
        assert_eq!(count(&m, "turn"), taken, "{tag}: {:#?}", m.requests);
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&jdir);
        records
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("REFUSED ") || l.starts_with("HALTED "))
            .map(|l| {
                l.split(' ')
                    .filter(|w| !w.starts_with("seq=") && !w.starts_with("backoff="))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
    };
    // Another driver's lease, twice; then another holder's. A hold on a
    // host whose `status` names none, twice.
    let lease = run(
        "refused-persists-lease",
        &["busy lease=s-9", "busy lease=s-9", "busy lease=s-7"],
        "0",
    );
    let hold = run(
        "refused-persists-hold",
        &["halted reason=owner origin=local"; 2],
        "-",
    );
    assert_eq!(
        (lease, hold),
        (
            vec![
                "REFUSED ERR busy lease=s-9".to_string(),
                "REFUSED ERR busy lease=s-7".to_string()
            ],
            vec!["HALTED ERR halted reason=owner origin=local".to_string()]
        )
    );
}

/// A REFUSAL THAT PERSISTS IS ONE REFUSAL, HOWEVER ITS COUNTERS MOVE (the
/// review of 2026-10-14). The server's refusal of a write a person is typing
/// over says how long ago they typed (`ERR busy person human_ms=<n>`,
/// `control_input.rs`): a new number at every retry of the same act. Keyed
/// on the whole text, each retry read as a new refusal and was journaled — the
/// spam the episode exists to stop. The same refusal is said once.
#[test]
fn a_refusal_whose_counters_move_is_journaled_once() {
    let (jdir, journal) = journal_file("refused-counters-move");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
        ],
    );
    m.verb_replies.insert(
        "turn",
        ["busy person human_ms=1200", "busy person human_ms=4810"]
            .iter()
            .map(|r| err(r))
            .collect(),
    );
    m.stall_sleep = Some(Duration::from_millis(20));
    m.vanish_after = Some(3000);
    m.exit_when = Some(|m: &Mock| m.turns >= 3);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(90)
    };
    let _ = watch_lines(&mut m, &opts);
    assert_eq!(count(&m, "turn"), 3, "{:#?}", m.requests);
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&jdir);
    let refused: Vec<String> = records
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with("REFUSED "))
        .collect();
    assert_eq!(refused.len(), 1, "{refused:#?}");
}

/// A REFUSAL'S EPISODE, by its key and its last meeting
/// ([`Session::refusal_news`]): the same refusal met again within
/// [`REFUSAL_QUIET`] of its last meeting is no news, whatever its measures
/// say (`human_ms=`, `bytes=`, a `…_ms=` wait); met again after that long
/// unmet — across a wall's wait — it is a new appearance, and said. A new
/// holder of the session is news: another lease, another turn, another
/// supervisor, another hold's reason.
#[test]
fn a_refusals_episode_is_its_holder_and_ends_after_a_quiet_spell() {
    assert_eq!(
        super::super::lifecycle::refusal_key(
            "REFUSED ERR busy input-unread bytes=4096 wait_ms=1200"
        ),
        "REFUSED ERR busy input-unread"
    );
    assert_eq!(
        super::super::lifecycle::refusal_key("REFUSED ERR busy turn=17"),
        "REFUSED ERR busy turn=17"
    );
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let t = Instant::now();
    let person = |ms: u32| format!("REFUSED ERR busy person human_ms={ms}");
    assert!(s.refusal_news(&person(1200), t), "first met");
    let mut at = t;
    // Met every few seconds for an hour: one episode.
    for i in 0..400u32 {
        at += Duration::from_secs(9);
        assert!(!s.refusal_news(&person(1200 + i), at), "retry {i}");
    }
    // Unmet for the quiet spell, then met again: a new appearance.
    at += REFUSAL_QUIET;
    assert!(s.refusal_news(&person(5), at), "after the quiet spell");
    // A new holder each time is news.
    for said in [
        "REFUSED ERR busy lease=s-9",
        "REFUSED ERR busy lease=s-7",
        "REFUSED ERR busy turn=17",
        "REFUSED ERR busy turn=18",
        "HALTED ERR halted reason=owner origin=local",
        "HALTED ERR halted reason=fabric-lost origin=fleet",
    ] {
        at += Duration::from_secs(1);
        assert!(s.refusal_news(said, at), "{said}");
    }
}

/// A REFUSAL'S EPISODE ENDS AT A WRITE THE SERVER TAKES AND AT A HOLD SEEN
/// LIFTED (the review of 2026-10-14: neither was tested). Another driver's
/// lease refuses the act twice — said once — then a write is taken, and the
/// same lease refusing again is a new appearance, said again; a write the
/// server refuses in between ends nothing. A hold parks the loop, lifts
/// (`UNHALTED`), and the same hold met again is said again. Without either
/// end, the refusal that came back was never journaled.
#[test]
fn a_refusals_episode_ends_at_a_taken_write_and_at_a_lifted_hold() {
    let refused_lines = |heard: &Heard| -> Vec<String> {
        heard
            .noted
            .iter()
            .filter(|l| l.starts_with("REFUSED ") || l.starts_with("HALTED "))
            .map(|l| {
                l.split(' ')
                    .filter(|w| !w.starts_with("seq=") && !w.starts_with("backoff="))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    let mut m = Mock::new(true, vec![ended("Fixed the parser; the suite is green.")]);
    m.verb_replies.insert(
        "turn",
        VecDeque::from([
            err("busy lease=s-9"),
            ok("OK id=1 submitted=1 status=settled seq=100 dur_ms=600 hash=0\n"),
        ]),
    );
    m.stall_sleep = Some(Duration::from_millis(5));
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let mut heard = Heard::default();
    let lease = "ERR busy lease=s-9";
    s.back_off(lease, 101, None, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.back_off(lease, 102, None, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    // Refused: the episode stands.
    let r = s
        .call(&["turn", "keep going"])
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(r.is_err("busy"), "{r:?}");
    s.back_off(lease, 103, None, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(refused_lines(&heard), vec![format!("REFUSED {lease}")]);
    // Taken: the episode is over.
    let r = s
        .call(&["turn", "keep going"])
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(r.ok(), "{r:?}");
    s.back_off(lease, 104, None, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(
        refused_lines(&heard),
        vec![format!("REFUSED {lease}"), format!("REFUSED {lease}")]
    );

    // A hold, lifted, met again.
    let mut m = Mock::new(true, vec![ended("Fixed the parser; the suite is green.")]);
    m.hold = "1";
    m.hold_lifts_after = Some(1);
    m.stall_sleep = Some(Duration::from_millis(5));
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let mut heard = Heard::default();
    let hold = "ERR halted reason=owner origin=local";
    s.park_on_hold(hold, 101, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(
        heard.noted.iter().any(|l| l.starts_with("UNHALTED ")),
        "{:#?}",
        heard.noted
    );
    s.park_on_hold(hold, 102, &mut heard)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(
        refused_lines(&heard),
        vec![format!("HALTED {hold}"), format!("HALTED {hold}")]
    );
}

/// THE FENCED PRESS A HOLD TURNED AWAY IS PRESSED AGAIN ONCE IT LIFTS (the
/// review of 2026-10-14: [`a_continuation_a_refusal_turned_away_is_typed_once_it_can_be`]'s
/// other door, `press_fenced`). The worker's own suggestion's accept key and
/// a standing draft's Enter meet `ERR halted` (an owner's `hold`); the loop
/// parks on the hold and, once `status` reads `hold=0`, decides the point
/// again and presses the key again, though the screen never moved. Before,
/// nothing was pressed after `UNHALTED`. The session ends on the fact (the
/// second press), its stalls a hang detector.
#[test]
fn a_fenced_press_a_hold_turned_away_is_pressed_once_it_lifts() {
    let held = |screen: Vec<String>, key: &'static str| {
        let mut m = Mock::new(true, vec![busy_screen(), screen]);
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = FENCED_HELP.to_string();
        m.key_replies
            .push_back(err("halted reason=owner origin=local"));
        m.hold = "1";
        m.hold_lifts_after = Some(2);
        m.stall_sleep = Some(Duration::from_millis(20));
        m.vanish_after = Some(1500);
        m.exit_when = Some(match key {
            "right" => |m: &Mock| m.presses().iter().filter(|p| p.ends_with(" right")).count() >= 2,
            _ => |m: &Mock| m.presses().iter().filter(|p| p.ends_with(" enter")).count() >= 2,
        });
        m
    };
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            human_grace_s: 1,
            ..SupervisorConfig::default()
        },
        ..auto(60, None)
    };
    // The suggestion's accept key.
    let mut m = held(suggesting("Stage 1 is in."), "right");
    let _ = watch_lines(&mut m, &opts);
    let rights: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|p| p.ends_with(" right"))
        .collect();
    assert_eq!(rights.len(), 2, "{:#?}", m.requests);
    assert_eq!(
        rights[0],
        "key if-gen=1.102 if=^❯\\x20keep\\x20going\\s*$ right"
    );
    // A draft left standing: its Enter.
    let mut m = held(typed_in("Stage 1 is in.", "also check the lexer"), "enter");
    m.cursor_col = Some(24);
    let _ = watch_lines(&mut m, &opts);
    let enters: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|p| p.ends_with(" enter"))
        .collect();
    assert_eq!(enters.len(), 2, "{:#?}", m.requests);
    assert!(
        enters
            .iter()
            .all(|p| p.starts_with("key if-gen=1.") && p.contains("lexer")),
        "{enters:#?}"
    );
}

/// Claude Code word-wraps its composer, so the guard is the end of the
/// text's last word, which the cursor's row holds at every width. Negative
/// control: a 40-character tail misses at some of the same widths.
#[test]
fn a_long_continuations_guard_matches_its_last_row_at_every_width() {
    let rules = "stay on the branch, never push to main, run the lane's tests before \
                 every commit, and keep the ledger rows exact";
    let text = format!("keep going (standing rules: {rules})");
    let guard = aterm_observe::row_matcher(&crate::supervise::run::turn_end_loop::composer_guard(
        '❯', &text,
    ))
    .expect("a pattern");
    let old_tail: String = {
        let chars: Vec<char> = text.chars().collect();
        chars[chars.len() - 40..].iter().collect()
    };
    let old = aterm_observe::row_matcher(
        crate::supervise::policy::row_guard(&old_tail).trim_start_matches('^'),
    )
    .expect("a pattern");
    let mut old_missed = 0;
    for width in 24..=200 {
        let last = word_wrapped_last_row(&format!("❯ {text}"), width);
        assert!(guard.matches(&last), "width {width}: {last:?}");
        if !old.matches(&last) {
            old_missed += 1;
        }
    }
    assert!(old_missed > 0, "the old guard missed at some width");
    // The rules ride capped.
    let long = "word ".repeat(200);
    let capped = crate::supervise::run::turn_end_loop::capped_rules(long.trim_end());
    assert!(capped.chars().count() <= crate::supervise::run::turn_end_loop::RULES_CAP + 1);
    assert!(capped.ends_with("word…"), "{capped}");
    assert_eq!(
        crate::supervise::run::turn_end_loop::capped_rules("short rules"),
        "short rules"
    );
}

/// A long text's guard sits on a row the composer draws (its caret row, or a
/// continuation indented two), never an unanchored tail that could match a
/// shell's cursor row, so an agent that exited between the read and the
/// write never has its answer run as a command. Negative control: the
/// composer's own last rows (Claude Code's `❯` and Codex's `›`, NBSP caret
/// included) still match.
#[test]
fn a_long_texts_guard_never_matches_a_shells_row() {
    let text = crate::supervise::SupervisorConfig::default().answer_text;
    for caret in ['❯', '›'] {
        let guard = aterm_observe::row_matcher(
            &crate::supervise::run::turn_end_loop::composer_guard(caret, &text),
        )
        .expect("a pattern");
        for shell in [
            "user@host ~ % Nobody is here to answer. Decide for yourself with your best judgment: \
             take the option you would recommend, prefer reversible steps, and keep going.",
            "ould recommend, prefer reversible steps, and keep going.",
            "$ keep going.",
        ] {
            assert!(!guard.matches(shell), "{caret}: {shell}");
        }
        for width in 30..=160 {
            let last = word_wrapped_last_row(&format!("{caret} {text}"), width);
            assert!(guard.matches(&last), "{caret} width {width}: {last:?}");
        }
        let nbsp = format!("{caret}\u{a0}{text}");
        assert!(guard.matches(&nbsp), "{nbsp}");
    }
}

/// The row a composer's text ends on, word-wrapped at `width` columns as
/// Claude Code wraps it: between words, a word longer than a row broken
/// inside it, continuation rows indented two.
fn word_wrapped_last_row(text: &str, width: usize) -> String {
    const INDENT: &str = "  ";
    let len = |s: &str| s.chars().count();
    let mut row = String::new();
    for word in text.split(' ') {
        let mut word = word.to_string();
        loop {
            let fresh = row.is_empty() || row == INDENT;
            let need = len(&word) + usize::from(!fresh);
            if len(&row) + need <= width {
                if !fresh {
                    row.push(' ');
                }
                row.push_str(&word);
                break;
            }
            if fresh {
                // Too long for any row: broken inside the word.
                let room = width - len(&row);
                row.extend(word.chars().take(room));
                word = word.chars().skip(room).collect();
            }
            row = INDENT.to_string();
        }
    }
    row
}

/// Six `typed` rows of this session's in the ledger, each `age_min` minutes
/// old plus its index (the acts of an earlier loop on it), under `sid`.
fn seed_ledger(path: &std::path::Path, age_min: i64, sid: Option<&str>) {
    let now = crate::supervise::journal::unix_ms();
    let rows: Vec<String> = (0..6)
        .map(|k| {
            crate::supervise::approvals::Row {
                rule_id: RULE_CONTINUE,
                outcome: crate::supervise::approvals::Outcome::Typed,
                command: "keep going",
                reason: "the turn-end policy",
                box_seq: 100 + u64::try_from(k).unwrap(),
            }
            .to_json(now - (age_min + k) * 60_000, sid)
        })
        .collect();
    std::fs::write(path, rows.join("\n") + "\n").expect("the ledger");
}

/// The typing budget is seeded from the ledger's `typed` rows of the session
/// within the window, so a re-spawned loop does not start with a fresh
/// budget: under a written `continue_per_hour = 6`, six from an earlier loop
/// in the last hour spend it (escalated, nothing typed). Negative controls:
/// six from two hours ago, six of another session's, and the default (`0`,
/// no cap) leave it whole: continued.
#[test]
fn the_typing_budget_counts_an_earlier_loops_acts_from_the_ledger() {
    for (tag, age, sid, cap, spent) in [
        ("recent", 1, None, 6, true),
        ("old", 120, None, 6, false),
        ("other", 1, Some("@s-9"), 6, false),
        ("uncapped", 1, None, 0, false),
    ] {
        let (dir, ledger) = ledger_at(&format!("budget-{tag}"));
        seed_ledger(&ledger, age, sid);
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
            ],
        );
        m.vanish_after = Some(2);
        let mut opts = hosted(30);
        opts.policy.continue_per_hour = cap;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let _ = std::fs::remove_dir_all(&dir);
        if spent {
            assert_eq!(count(&m, "turn"), 0, "{tag}: {lines:#?}");
            assert!(
                m.attention
                    .as_deref()
                    .is_some_and(|a| a.contains("continue_per_hour = 6 reached")),
                "{tag}: {:?}\n{lines:#?}\n{:#?}",
                m.attention,
                m.requests
            );
        } else {
            assert_eq!(count(&m, "turn"), 1, "{tag}: {lines:#?}");
        }
    }
}

/// `rows` as Claude Code 2.1.281 draws its composer (the caret, then a
/// no-break space: `❯\u{a0}`, `❯\u{a0}keep going`), the row the server's
/// `if=` tests, where `text` trims the empty one to `❯`.
fn drawn_nbsp(mut r: Vec<String>) -> Vec<String> {
    for row in &mut r {
        if row == "❯" {
            *row = "❯\u{a0}".to_string();
        } else if let Some(rest) = row.strip_prefix("❯ ") {
            *row = format!("❯\u{a0}{rest}");
        }
    }
    r
}

/// The composer guard's trailing run is any whitespace and the caret's space
/// is `\s`, so the write and its Enter land on Claude Code 2.1.281's empty
/// composer (`❯` and a no-break space). Negative control: a bare skip on the
/// unchanged screen (the guard matched no row) is escalated with what was
/// not typed, never retried as if the screen had moved and never left
/// silent.
#[test]
fn the_no_break_space_composer_takes_the_fenced_continuation() {
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            drawn_nbsp(ended("chunk 1 done")),
            drawn_nbsp(typed_in("chunk 1 done", "keep going")),
            busy_screen(),
            ended(STOP),
        ],
    );
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = FENCED_HELP.to_string();
    m.screen_cols.insert(2, 12);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_CONTINUE} keep going"
        )),
        "{lines:#?}\n{:#?}",
        m.requests
    );
    let writes: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("send ") || r.starts_with("key "))
        .collect();
    assert_eq!(
        writes,
        [
            "send if-gen=1.102 if=^❯\\s*$ -- keep going",
            "key if-gen=1.103 if=^❯\\x{A0}keep\\x20going\\s*$ enter",
        ],
        "{:#?}",
        m.requests
    );
    // The guarded submit's guard, for a host without the send fence, takes
    // the same caret.
    let g = aterm_observe::row_matcher(&crate::supervise::run::turn_end_loop::composer_guard(
        '❯',
        "keep going",
    ))
    .expect("compiles");
    assert!(g.matches("❯\u{a0}keep going"));
    assert!(g.matches("❯ keep going"));
    assert!(!g.matches("❯ keep going on"));

    // Negative control: the judged screen, and the guard matched no row.
    let mut m = Mock::new(true, vec![busy_screen(), drawn_nbsp(ended("chunk 1 done"))]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = FENCED_HELP.to_string();
    m.verb_replies
        .insert("send", VecDeque::from([ok("OK skipped seq=102\n")]));
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines(&mut m, &hosted(30));
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "send"), 1, "never retried: {:#?}", m.requests);
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.contains("not typed: aterm could not find the prompt line")),
        "{:?}",
        m.attention
    );
}

/// Run `run_hosted` over `m` until its host stops it, once the loop has read
/// the last screen and is watching past it ([`host_stops`]; a fixed delay is
/// outrun on a loaded machine), with the hand-over flag raised first when
/// `hand_over`, and return its printed lines. The approval ledger is the
/// caller's (a temp one, never the real state root's): the loops of one
/// restart share it, as a restarted worker's do.
fn hosted_until_stopped(m: &mut Mock, ledger: &Path, hand_over: bool) -> Vec<String> {
    let (r, lines) = host_stops(
        m,
        Some("@s-1"),
        ledger.to_path_buf(),
        &hosted(30),
        hand_over,
    );
    assert_eq!(r, Ok(()));
    lines
}

/// A worker restarted by its host (a policy edit, a seamless update) keeps
/// the idle point's escalation: a stop that hands the session over leaves
/// the badge, and the next loop adopts it (an idle point's too), one `meta
/// set` in all, never cleared. Negative control (a stop for good): the badge
/// is cleared, and the next loop raises none.
#[test]
fn a_handed_over_idle_escalation_survives_the_restart() {
    let (dir, ledger) = ledger_at("hand-over");
    let mut m = Mock::new(true, vec![busy_screen(), ended(STOP)]);
    let first = hosted_until_stopped(&mut m, &ledger, true);
    assert_eq!(
        first.last().map(String::as_str),
        Some("EXIT stopped"),
        "{first:?}"
    );
    let raised = m
        .attention
        .clone()
        .unwrap_or_else(|| panic!("the idle point escalated: {first:?}\n{:#?}", m.requests));
    assert!(
        raised.starts_with("claude idle: I need your decision"),
        "{raised}"
    );
    assert_eq!(count(&m, "@s-1 meta unset attention owner=supervisor"), 0);
    let _ = hosted_until_stopped(&mut m, &ledger, true);
    assert_eq!(
        m.attention.as_deref(),
        Some(raised.as_str()),
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "@s-1 meta set attention owner=supervisor"), 1);
    assert_eq!(count(&m, "@s-1 meta unset attention owner=supervisor"), 0);
    // The last loop stops for good: its adopted badge is released.
    let _ = hosted_until_stopped(&mut m, &ledger, false);
    assert_eq!(m.attention, None, "{:#?}", m.requests);
    let _ = std::fs::remove_dir_all(&dir);

    // Negative control: the stop is not a hand-over — cleared; the question
    // still stands, so a loop started over it later raises it again.
    let (dir, ledger) = ledger_at("hand-over-ctl");
    let mut m = Mock::new(true, vec![busy_screen(), ended(STOP)]);
    let _ = hosted_until_stopped(&mut m, &ledger, false);
    assert_eq!(m.attention, None);
    let _ = hosted_until_stopped(&mut m, &ledger, false);
    assert_eq!(m.attention, None);
    assert_eq!(count(&m, "@s-1 meta set attention owner=supervisor"), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stop in a continuation's settle ends the hosted run before its next
/// wait: after the accept key that fills the worker's suggestion, and after
/// the fenced write of the continuation, the loop settles with two waits
/// back to back (the key's change, then the short idle), and the stop set as
/// the first goes out ([`stop_in_every_wait`], every other wait of the path
/// too) ends the run before the second, and before the Enter:
/// `Session::call` refuses a wait once the stop is set, as it refuses a
/// write. Negative control: with that refusal removed, each path's stop set
/// in `await seq 102 timeout <REACTION_WAIT>` is followed by `await idle 500
/// timeout 1500`. The settle's bound is read from [`REACTION_WAIT`] itself,
/// never spelled here.
#[test]
fn a_stop_in_a_continuations_settle_ends_the_run_before_its_next_wait() {
    let bound = format!(
        " timeout {}",
        crate::supervise::run::turn_end_loop::REACTION_WAIT.as_millis()
    );
    let settle = |armed: &[String]| {
        assert!(
            armed
                .iter()
                .any(|w| w.starts_with("@s-1 await seq ") && w.ends_with(&bound)),
            "the stop was never set in the settle: {armed:#?}"
        );
    };
    let armed = stop_in_every_wait("suggestion", &hosted(30), Some("@s-1"), || {
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                suggesting("Stage 1 is in."),
                suggesting("Stage 1 is in."),
                busy_screen(),
                ended(STOP),
            ],
        );
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = FENCED_HELP.to_string();
        m.screen_cols.insert(2, 12);
        m.vanish_after = Some(2);
        m
    });
    settle(&armed);

    let armed = stop_in_every_wait("fenced", &hosted(30), Some("@s-1"), || {
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
                typed_in("Fixed the parser; the suite is green.", "keep going"),
                busy_screen(),
                ended(STOP),
            ],
        );
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = FENCED_HELP.to_string();
        m.screen_cols.insert(2, 12);
        m.vanish_after = Some(2);
        m
    });
    settle(&armed);
}

/// A ledger holding one model switch row (as the loop that switched wrote
/// it, [`crate::supervise::approvals::model_switch_reason`]: `relaunch
/// --model opus`, or in an older ledger `/model opus`), then, when
/// `restored`, the switch back's row.
pub(super) fn seed_switch(
    path: &std::path::Path,
    sid: Option<&str>,
    back_at_unix: i64,
    restored: bool,
    legacy: bool,
) {
    use crate::supervise::approvals::{Outcome, Row, model_switch_reason};
    use crate::supervise::policy::turn_end::{RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
    let now = crate::supervise::journal::unix_ms();
    let reason = model_switch_reason(Some("fable"), Some(back_at_unix));
    let mut rows = vec![
        Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: if legacy {
                "/model opus"
            } else {
                "relaunch --model opus"
            },
            reason: &reason,
            box_seq: 90,
        }
        .to_json(now - 30 * 60_000, sid),
        Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: "keep going",
            reason: "the turn-end policy",
            box_seq: 91,
        }
        .to_json(now - 29 * 60_000, sid),
    ];
    if restored {
        rows.push(
            Row {
                rule_id: RULE_MODEL_RESTORE,
                outcome: Outcome::Typed,
                command: if legacy {
                    "/model fable"
                } else {
                    "relaunch --model fable"
                },
                reason: "the turn-end policy",
                box_seq: 95,
            }
            .to_json(now - 10 * 60_000, sid),
        );
    }
    std::fs::write(path, rows.join("\n") + "\n").expect("the ledger");
}

/// Owner decision 3's switch back at the bucket's reset survives a host
/// restart: the switch's ledger row says how to undo it (`relaunch --model
/// opus`, or `/model opus` in an older ledger), and a loop that starts after
/// it reads it back as the open switch; the switch back is a relaunch its
/// host makes (`run_engine_tests`' `a_model_bucket_is_relaunched_…`). Here,
/// with no host to relaunch, the point gets its plain continuation, never
/// `/model`. Negative controls: a ledger whose switch was already undone,
/// and one whose reset is still ahead, read back no open switch or one not
/// due.
#[test]
fn a_model_switch_is_read_back_by_a_loop_that_starts_after_it() {
    use crate::supervise::approvals::{OpenSwitch, open_model_switch};
    // 2026-09-17T12:55:00-07:00 is the watch tests' clock (TEST_NOW).
    let test_now: i64 = 1_789_660_500;
    for legacy in [false, true] {
        for (tag, back_at, restored) in [
            ("due", test_now - 60, false),
            ("undone", test_now - 60, true),
            ("ahead", test_now + 3600, false),
        ] {
            let (dir, ledger) = ledger_at(&format!("switch-{tag}-{legacy}"));
            seed_switch(&ledger, None, back_at, restored, legacy);
            let open = open_model_switch(&ledger, None);
            assert_eq!(
                open,
                (!restored).then(|| OpenSwitch {
                    from: Some("fable".to_string()),
                    to: "opus".to_string(),
                    back_at_unix: Some(back_at),
                }),
                "{tag} {legacy}"
            );
            let mut m = Mock::new(
                true,
                vec![busy_screen(), ended("Stage 3 done; the suite is green.")],
            );
            m.vanish_after = Some(2);
            let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
                s.set_approval_ledger(Some(ledger.clone()));
            });
            let _ = std::fs::remove_dir_all(&dir);
            assert!(
                !m.requests.iter().any(|r| r.contains("/model")),
                "{tag}: {lines:#?}\n{:#?}",
                m.requests
            );
            assert!(
                m.requests.contains(&continue_request("keep going")),
                "{tag}: {:#?}",
                m.requests
            );
        }
    }
}

/// Full power: a request for a decision is answered (`answer_text` typed
/// through the same guarded write as a continuation, `CONTINUED …
/// rule=answer@v1`, a `typed` ledger row) and no badge is raised. Negative
/// control: under `answer_questions = false` ([`hosted`]) the same point is
/// escalated and nothing is typed.
#[test]
fn a_stop_phrase_is_answered_with_the_answer_text() {
    let answer = SupervisorConfig::default().answer_text;
    let mut m = Mock::new(true, vec![busy_screen(), ended(STOP)]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let full = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines(&mut m, &full);
    assert!(
        lines.contains(&format!("CONTINUED seq=102 rule=answer@v1 {answer}")),
        "{lines:#?}"
    );
    assert!(
        m.requests.contains(&continue_request(&answer)),
        "{:#?}",
        m.requests
    );
    assert_eq!(m.attention, None, "{:#?}", m.requests);

    let mut m = Mock::new(true, vec![busy_screen(), ended(STOP)]);
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &hosted(30));
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(m.attention.is_some());
}

/// A person at the keyboard wins (the server's `status human_ms=`): a
/// continuation the policy decided is not typed within `human_grace_s` of
/// their last keystroke (`WAITING … a person is typing`, the point decided
/// again when the grace ends), and a box the policy would answer is held,
/// neither pressed nor escalated. Negative controls: a keystroke older than
/// the grace, and a server that says nothing (`human_ms` absent), hold
/// nothing.
#[test]
fn a_persons_keystroke_holds_the_loop_for_the_grace() {
    // A short budget: a held point waits out the rest of it.
    let full = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(1, None)
    };
    for (human_ms, holds) in [(Some(1_000), true), (Some(200_000), false), (None, false)] {
        let mut m = Mock::new(true, vec![busy_screen(), ended("Fixed the parser.")]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        m.human_ms = human_ms;
        let (lines, _) = watch_lines(&mut m, &full);
        assert_eq!(
            count(&m, "turn"),
            usize::from(!holds),
            "{human_ms:?}: {lines:#?}"
        );
        assert_eq!(m.attention, None, "{human_ms:?}");

        let mut m = Mock::new(true, vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
        m.vanish_after = Some(0);
        m.human_ms = human_ms;
        let (lines, _) = watch_lines(&mut m, &full);
        assert_eq!(
            m.presses().len(),
            usize::from(!holds),
            "{human_ms:?}: {lines:#?}"
        );
        assert_eq!(
            m.attention, None,
            "{human_ms:?}: a held box is nobody's to be told of"
        );
    }
}

/// Another driver's hand on the session (a drive lease, `hand=lease:<holder>`,
/// or a turn a named driver typed, `hand=turn:<id>:<holder>`) holds the loop
/// as a person's keystroke does: no box pressed, nothing typed, nothing
/// raised, so the host never fights a manager driving its worker. Negative
/// control: an owner-class turn (`hand=turn:<id>`, as this loop's own
/// continuations are) holds nothing.
#[test]
fn another_drivers_hand_holds_the_loop() {
    let opts = || SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 60,
            ..SupervisorConfig::default()
        },
        ..auto(1, None)
    };
    for hand in ["lease:mgr", "turn:7:mgr"] {
        let mut m = Mock::new(true, vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
        m.hand = hand;
        m.stall_sleep = Some(Duration::from_millis(100));
        let (lines, _) = watch_lines(&mut m, &opts());
        assert!(
            m.presses().is_empty(),
            "{hand}: {lines:#?}\n{:#?}",
            m.requests
        );
        assert_eq!(
            count(&m, "meta set attention"),
            0,
            "{hand}: {:#?}",
            m.requests
        );
    }
    let mut m = Mock::new(true, vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
    m.hand = "turn:7";
    m.stall_sleep = Some(Duration::from_millis(100));
    let _ = watch_lines(&mut m, &opts());
    assert!(!m.presses().is_empty(), "{:#?}", m.requests);
}

/// A box held for a person is answered once their grace has passed: the
/// first status says they typed half a second ago (`HELD`, nothing pressed
/// or raised); when the grace runs out the box is judged again, on a fresh
/// status that says they have stopped, and pressed, never before the grace.
/// (The scripted box never leaves, so full power reads it as a press that
/// did not land and tries it again after its back-off; not this test's.)
/// Negative control: a person who keeps typing keeps it held to the end of
/// the budget.
#[test]
fn a_held_box_is_answered_once_the_persons_grace_has_passed() {
    let opts = |budget: u64| SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 1,
            ..SupervisorConfig::default()
        },
        ..auto(budget, None)
    };
    let mut m = Mock::new(true, vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
    m.human_ms_reads = VecDeque::from([Some(500)]);
    m.stall_sleep = Some(Duration::from_millis(300));
    m.vanish_after = Some(4);
    let started = Instant::now();
    let (lines, _) = watch_lines(&mut m, &opts(10));
    assert!(!m.presses().is_empty(), "{lines:#?}\n{:#?}", m.requests);
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "pressed inside the grace: {:?}",
        started.elapsed()
    );
    let held = m
        .requests
        .iter()
        .position(|r| r == "status")
        .expect("the status the box was held on");
    let press = m
        .requests
        .iter()
        .position(|r| r.starts_with("key "))
        .unwrap();
    assert!(
        m.requests[held + 1..press].iter().any(|r| r == "status"),
        "judged again on a fresh status: {:#?}",
        m.requests
    );
    // Nothing raised while it was held. (The scripted box never leaves, so
    // what follows the press is "the box did not change", not this test's.)
    assert!(
        !m.requests[..press]
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "{:#?}",
        m.requests
    );

    let mut m = Mock::new(true, vec![bash_box(&["⏺ Cleaning."], "rm -rf build")]);
    m.human_ms = Some(300);
    m.stall_sleep = Some(Duration::from_millis(200));
    let _ = watch_lines(&mut m, &opts(2));
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);
}

/// A draft left standing is the next message: once nothing has changed it
/// for the grace (the loop's own clock, since this server says nothing of a
/// person), the policy's act goes as the draft's Enter, fenced on the judged
/// read and guarded on its caret row, and is said as the act's `CONTINUED`
/// line with the draft's words; nothing is typed into it and nothing raised.
/// Negative control: a draft still changing is a person typing (every read
/// shows new text), and nothing is pressed.
#[test]
fn a_draft_left_standing_is_submitted_once_the_grace_has_passed() {
    let drafted = |text: &str| {
        let mut r = ended("Fixed the parser; the suite is green.");
        let caret = r.iter().position(|row| row == "❯").expect("caret");
        r[caret] = format!("❯ {text}");
        r
    };
    let fenced = |screens: Vec<Vec<String>>| {
        let mut m = Mock::new(true, screens);
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        m.cursor_col = Some(24);
        m.stall_sleep = Some(Duration::from_millis(400));
        m
    };
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 1,
            ..SupervisorConfig::default()
        },
        ..auto(4, None)
    };
    let mut m = fenced(vec![busy_screen(), drafted("also check the lexer")]);
    m.vanish_after = Some(12);
    let started = Instant::now();
    let (lines, _) = watch_lines(&mut m, &opts);
    let enters: Vec<&String> = m
        .presses()
        .into_iter()
        .filter(|p| p.ends_with(" enter"))
        .collect();
    assert_eq!(enters.len(), 1, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        enters[0].contains("if-gen=") && enters[0].contains("lexer"),
        "{enters:?}"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        count(&m, "turn"),
        0,
        "nothing typed into it: {:#?}",
        m.requests
    );
    assert_eq!(count(&m, "send"), 0, "{:#?}", m.requests);
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED seq=")
            && l.ends_with(&format!("rule={RULE_CONTINUE} also check the lexer"))),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);

    // Still being written, for longer than the grace and to the end of the
    // budget: every read, 100 ms apart, shows a different draft.
    let mut screens = vec![busy_screen()];
    let words = [
        "also",
        "also check",
        "also check the",
        "also check the lexer",
    ];
    for w in words.iter().cycle().take(200) {
        screens.push(drafted(w));
    }
    let mut m = fenced(screens);
    m.delay = (0..2000).map(|i| (i, Duration::from_millis(100))).collect();
    m.vanish_after = Some(0);
    let started = Instant::now();
    let _ = watch_lines(&mut m, &opts);
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(
        !m.presses().iter().any(|p| p.ends_with(" enter")),
        "{:#?}",
        m.requests
    );
}

/// Codex 0.158.0 draws its input line `»` (read bold by `cell 60 0`): the
/// loop reads the same screens with that mark as it reads 0.156.1's, and the
/// continuation is guarded on the mark drawn (`submit=guarded:^»\skeep…`).
/// Negative control: the 0.156.1 screens keep `^›`
/// (`a_codex_session_is_supervised_end_to_end`, below).
#[test]
fn a_codex_0_158_input_line_is_typed_into_under_its_own_mark() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let marked = |text: &str| {
        let mut rows = screen(text);
        let at = aterm_phase::codex::composer(&rows).expect("the input line");
        rows[at] = rows[at].replacen('›', "»", 1);
        rows
    };
    let mut m = Mock::new(true, vec![marked(cx::BUSY), marked(cx::END_OF_TURN)]);
    m.program = "codex";
    m.cursor_on_caret = true;
    m.gen_fence = true;
    m.sends_gen = true;
    m.turn_releases = Some(1);
    m.vanish_after = Some(1);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let turn = m
        .requests
        .iter()
        .find(|r| r.starts_with("turn "))
        .unwrap_or_else(|| panic!("the continuation: {lines:#?} {:#?}", m.requests));
    assert!(
        turn.starts_with("turn submit=guarded:^»\\skeep\\x20going\\s*$ "),
        "{turn}"
    );
}

/// Codex, end to end over the scripted server (codex 0.156.1's screens,
/// `program=codex`): the loop reads Codex's own grammar (busy while its
/// status row runs, an ended turn at its end row) and at full power
/// continues the turn in Codex's composer: on a host that fences only `key`,
/// through the guarded `turn` (`submit=guarded:^›\skeep\x20going…`); on one
/// that fences `send` too, through the fenced write and a fenced Enter held
/// until the echo is still 600 ms (Codex takes an Enter right behind typed
/// text as a newline, its paste guard). A box on the way is answered by its
/// role (`1`, `Yes, proceed`, guarded on its `$` row). Negative control: the
/// same screens under `program=zsh` type and press nothing.
#[test]
fn a_codex_session_is_supervised_end_to_end() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let screens = || {
        vec![
            screen(cx::BUSY),
            screen(cx::BOX_EXEC),
            screen(cx::BUSY),
            screen(cx::END_OF_TURN),
        ]
    };
    let mut m = Mock::new(true, screens());
    m.program = "codex";
    m.cursor_on_caret = true;
    m.gen_fence = true;
    m.sends_gen = true;
    m.turn_releases = Some(3);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let exec_row = screen(cx::BOX_EXEC)
        .into_iter()
        .find(|r| r.trim_start().starts_with("$ touch"))
        .expect("the command row");
    let press = m
        .presses()
        .into_iter()
        .find(|r| r.ends_with(" 1"))
        .cloned()
        .unwrap_or_else(|| panic!("the box answered: {lines:#?} {:#?}", m.requests));
    assert!(
        press.contains(&crate::supervise::policy::row_guard(&exec_row)),
        "guarded on the $ row: {press}"
    );
    let turn = m
        .requests
        .iter()
        .find(|r| r.starts_with("turn "))
        .unwrap_or_else(|| panic!("the continuation: {lines:#?} {:#?}", m.requests));
    assert!(
        turn.starts_with("turn submit=guarded:^›\\skeep\\x20going\\s*$ "),
        "{turn}"
    );
    assert!(
        !m.requests.iter().any(|r| r.starts_with("send ")),
        "no fenced send on a host whose send takes no fence: {:#?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "{lines:#?}"
    );
    // The point's line names Codex's last words, never its footer.
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("EVENT idle ") && l.ends_with("as it evolves continuously.")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "nothing handed over");

    // A host that fences `send`: the continuation is written only on the
    // judged read, and its Enter goes fenced once the echo has held still for
    // Codex's paste guard (600 ms), never a bare `turn` whose paste checks
    // nothing.
    let mut typed = screen(cx::END_OF_TURN);
    let caret = typed
        .iter()
        .rposition(|r| r.starts_with("› Ask Codex"))
        .expect("the composer");
    typed[caret] = "› keep going".to_string();
    let mut m = Mock::new(
        true,
        vec![
            screen(cx::BUSY),
            screen(cx::END_OF_TURN),
            typed,
            screen(cx::BUSY),
            screen(cx::END_OF_TURN),
        ],
    );
    m.program = "codex";
    m.cursor_on_caret = true;
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = FENCED_HELP.to_string();
    m.screen_cols.insert(2, 14);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let writes: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("send ") || r.starts_with("key "))
        .collect();
    assert!(
        writes.len() >= 2,
        "fenced write and fenced Enter: {lines:#?}\n{:#?}",
        m.requests
    );
    assert!(writes[0].starts_with("send if-gen="), "{writes:?}");
    assert!(
        writes[1].starts_with("key if-gen=") && writes[1].ends_with(" enter"),
        "{writes:?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        m.requests.iter().any(|r| r.starts_with("await idle 600 ")),
        "the paste guard's settle: {:#?}",
        m.requests
    );

    // Negative control: a shell showing the same screens.
    let mut m = Mock::new(true, screens());
    m.program = "zsh";
    m.cursor_on_caret = true;
    m.gen_fence = true;
    m.sends_gen = true;
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

/// agy's screens served `program=agy`, the cursor where agy parks it: on the
/// caret row of each composer agy draws (a box draws none, and hides it).
fn agy_mock(screens: Vec<Vec<String>>) -> Mock {
    let carets: Vec<Option<usize>> = screens
        .iter()
        .map(|s| aterm_phase::agy::composer(s))
        .collect();
    let mut m = Mock::new(true, screens);
    m.program = "agy";
    for (i, caret) in carets.into_iter().enumerate() {
        if let Some(row) = caret {
            m.screen_cursors.insert(i, row);
        }
    }
    m
}

/// agy, end to end over the scripted server (agy 1.4.0's screens,
/// `program=agy`): the loop reads agy's own grammar — busy while its spinner
/// row runs over the open composer, an ended turn at its reply with the
/// footer back to `? for shortcuts` — answers its command box with the digit
/// agy draws for the one-shot allow (`1`, `Yes, run command`), guarded on the
/// command's row, and at full power continues the turn in agy's composer
/// under its own caret (`submit=guarded:^>\skeep\x20going…`). NEGATIVE
/// CONTROL: the same screens under `program=zsh` type and press nothing.
#[test]
fn an_agy_session_is_supervised_end_to_end() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    let screens = || {
        vec![
            screen(a::BUSY),
            screen(a::BOX_EXEC),
            screen(a::BUSY),
            screen(a::END_OF_TURN),
        ]
    };
    let mut m = agy_mock(screens());
    m.gen_fence = true;
    m.sends_gen = true;
    m.turn_releases = Some(3);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let command_row = screen(a::BOX_EXEC)
        .into_iter()
        .find(|r| r.trim() == "touch x.txt")
        .expect("the command row");
    let press = m
        .presses()
        .into_iter()
        .find(|r| r.ends_with(" 1"))
        .cloned()
        .unwrap_or_else(|| panic!("the box answered: {lines:#?} {:#?}", m.requests));
    assert!(
        press.contains(&crate::supervise::policy::row_guard(&command_row)),
        "guarded on the command's row: {press}"
    );
    let turn = m
        .requests
        .iter()
        .find(|r| r.starts_with("turn "))
        .unwrap_or_else(|| panic!("the continuation: {lines:#?} {:#?}", m.requests));
    assert!(
        turn.starts_with("turn submit=guarded:^>\\skeep\\x20going\\s*$ "),
        "{turn}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 0, "nothing handed over");

    let mut m = agy_mock(screens());
    m.program = "zsh";
    m.gen_fence = true;
    m.sends_gen = true;
    m.vanish_after = Some(2);
    let _ = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

/// agy draws NO PLACEHOLDER: every word on its caret row was typed, so a
/// person's draft whose cursor was moved home (column 2, where an empty
/// composer parks it too) is never typed over, and no `cell` is asked to
/// tell it from one — even where the server answers none, the case Claude
/// Code's reading falls back to the cursor's row for. The person's-typing
/// grace is off, so the composer's reading decides. NEGATIVE CONTROL: the
/// same turn's end with the composer empty is continued.
#[test]
fn an_agy_draft_homed_to_its_caret_is_never_typed_over() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    let ended = screen(a::END_OF_TURN);
    let caret = aterm_phase::agy::composer(&ended).expect("the composer");
    let mut drafted = ended.clone();
    drafted[caret] = "> hello there".to_string();
    for (end, typed) in [(drafted, false), (ended, true)] {
        let mut m = agy_mock(vec![screen(a::BUSY), end]);
        m.cell_unreadable = true;
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let mut opts = hosted(3);
        opts.policy.human_grace_s = 0;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        assert!(
            !m.requests.iter().any(|r| r.starts_with("cell ")),
            "typed {typed}: {:#?}",
            m.requests
        );
        assert_eq!(
            m.requests.iter().any(|r| r.starts_with("turn ")),
            typed,
            "typed {typed}: {lines:#?}\n{:#?}",
            m.requests
        );
    }
}

/// agy's `/` command menu is NO IDLE COMPOSER: its footer reads `esc to
/// cancel` with no turn running and its rows sit under the bottom rule, so
/// the turn's end it follows is not acted on while it is open — nothing is
/// typed into the menu's filter. NEGATIVE CONTROL: the composer at rest is
/// continued.
#[test]
fn agys_command_menu_is_never_typed_into() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    for (end, typed) in [(a::SLASH_MENU, false), (a::END_OF_TURN, true)] {
        let mut m = agy_mock(vec![screen(a::BUSY), screen(end)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines_with(&mut m, &hosted(3), |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        assert_eq!(
            m.requests
                .iter()
                .any(|r| r.starts_with("turn ") || r.starts_with("send ")),
            typed,
            "{}: {lines:#?}\n{:#?}",
            end.lines().next().unwrap_or_default(),
            m.requests
        );
    }
}

/// agy SIGNED OUT draws its sign-in menu INSTEAD of a composer (measured on
/// 1.4.0): the loop hands the session to its owner with how agy signs in —
/// quit it and run it again — and types and presses nothing, so no option of
/// the menu (`Google OAuth` opens a browser) is ever chosen. NEGATIVE
/// CONTROL: the menu with its `Select login method:` row gone is no wall,
/// and nothing is handed over.
#[test]
fn agys_sign_in_menu_is_handed_to_its_owner() {
    use aterm_phase::agy::fixtures as a;
    use aterm_phase::prompt::fixtures::screen;
    let signed_out = screen(a::SIGNED_OUT);
    let unnamed: Vec<String> = signed_out
        .iter()
        .map(|r| {
            if r.trim() == "Select login method:" {
                String::new()
            } else {
                r.clone()
            }
        })
        .collect();
    assert_ne!(unnamed, signed_out);
    for (end, told) in [(signed_out, true), (unnamed, false)] {
        let mut m = agy_mock(vec![screen(a::BUSY), end]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(2);
        let (lines, _) = watch_lines(&mut m, &hosted(3));
        assert!(m.presses().is_empty(), "told {told}: {:#?}", m.requests);
        assert_eq!(count(&m, "turn"), 0, "told {told}: {:#?}", m.requests);
        let raised = m
            .requests
            .iter()
            .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "));
        assert_eq!(
            raised.is_some_and(|a| a.contains("quit agy and run it again to sign in")),
            told,
            "{raised:?}\n{lines:#?}"
        );
    }
}

/// A host whose measure of the API's reach is scripted ([`IdleHost::reach`]):
/// each ask answers the next entry, the last for ever after; every ask is
/// counted. It takes no step and owns no turn end.
#[derive(Debug, Default)]
struct ReachHost {
    script: std::sync::Mutex<VecDeque<Reach>>,
    last: std::sync::Mutex<Reach>,
    asks: std::sync::atomic::AtomicUsize,
    /// Every plan the loop told the host, in order ([`IdleHost::waiting`]).
    told: std::sync::Mutex<Vec<Option<crate::supervise::WaitPlan>>>,
}

impl ReachHost {
    fn scripted(script: &[Reach]) -> Arc<Self> {
        Arc::new(Self {
            script: std::sync::Mutex::new(script.iter().copied().collect()),
            ..Self::default()
        })
    }

    fn asks(&self) -> usize {
        self.asks.load(Ordering::SeqCst)
    }

    fn told(&self) -> Vec<Option<crate::supervise::WaitPlan>> {
        self.told.lock().unwrap().clone()
    }
}

impl IdleHost for ReachHost {
    fn waiting(&self, plan: Option<crate::supervise::WaitPlan>) {
        self.told.lock().unwrap().push(plan);
    }
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn reach(&self) -> Reach {
        self.asks.fetch_add(1, Ordering::SeqCst);
        let mut last = self.last.lock().unwrap();
        if let Some(next) = self.script.lock().unwrap().pop_front() {
            *last = next;
        }
        *last
    }
}

/// The outage's screen: Claude Code 2.1.283's `⏺ API Error: Can't reach
/// the API server … (ENOTFOUND)` after the vendor's 3 minutes of retries.
fn enotfound() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::API_ERROR_ENOTFOUND)
}

/// Clocks that keep every rung and the hold an hour off: at the network
/// wall only the host's measure can move the loop.
fn far_rungs() -> TurnEndTiming {
    let hour = Duration::from_secs(3600);
    TurnEndTiming {
        net_backoff: vec![hour],
        down_hold: hour,
        ..TurnEndTiming::default()
    }
}

/// One run of the outage's point under a host scripted `script`: the lines,
/// the host, the server's record of the run, and the journal's `WAITING`
/// rows' summaries.
fn outage_under(script: &[Reach], max_s: u64) -> (Vec<String>, Arc<ReachHost>, Mock, Vec<String>) {
    outage_timed(script, max_s, far_rungs())
}

/// [`outage_under`] with the policy's clocks `timing`.
fn outage_timed(
    script: &[Reach],
    max_s: u64,
    timing: TurnEndTiming,
) -> (Vec<String>, Arc<ReachHost>, Mock, Vec<String>) {
    let host = ReachHost::scripted(script);
    let tag = timing.net_backoff.first().map_or(0, Duration::as_secs);
    let (dir, path) = journal_file(&format!("te-outage-{}-{max_s}-{tag}", script.len()));
    let mut m = Mock::new(
        true,
        vec![busy_screen(), enotfound(), busy_screen(), ended(STOP)],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(5));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..hosted(max_s)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_turn_end_timing(timing));
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let waiting = records
        .into_iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary)
        .collect();
    (lines, host, m, waiting)
}

/// One run of an outage over the scripted server with its clocks short (a
/// rung of 50 ms, the act check's window 300 ms) under a [`ReachHost`]: the
/// lines, the host, the server's record and the journal's `WAITING`
/// summaries. `screens` are served in order and the last repeats; the wall's
/// screen (index 1) sits until the loop types a `turn`.
fn short_outage(
    tag: &str,
    screens: Vec<Vec<String>>,
    vanish_after: u32,
    max_s: u64,
) -> (Vec<String>, Arc<ReachHost>, Mock, Vec<String>) {
    let host = ReachHost::scripted(&[]);
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(true, screens);
    m.turn_releases = Some(1);
    m.vanish_after = Some(vanish_after);
    m.cursor_on_caret = true;
    m.stall_sleep = Some(Duration::from_millis(5));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..hosted(max_s)
    };
    let timing = TurnEndTiming {
        net_backoff: vec![Duration::from_millis(50)],
        down_hold: Duration::from_millis(50),
        take_within: Duration::from_millis(300),
        ..TurnEndTiming::default()
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_turn_end_timing(timing));
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let waiting = records
        .into_iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary)
        .collect();
    (lines, host, m, waiting)
}

/// The loop tells the host its next try, and clears the plan when the try is
/// typed, not at the next point: after a try the loop sits in the worker's
/// whole busy turn (`await_turn_from`), so a plan the try ended must not
/// stand on the tab through it. The worker here goes busy after the try and
/// the run ends inside that busy turn, so the `None` can only come from the
/// try itself. Control: a run whose wall is never tried (its API's reach
/// unmeasured, a rung an hour off) is told the plan first and once, as unix
/// seconds on the loop's clock, the rung from now, however many steps the
/// wait takes, and nothing after it. (An ordinary point, no wall, is never
/// told a plan: `the_loop_reads_the_wall_of_a_tall_panes_short_transcript`'s
/// control.)
#[test]
fn the_plan_is_cleared_when_the_try_is_typed_not_at_the_next_point() {
    let (lines, host, m, _) = short_outage(
        "te-plan-cleared-at-try",
        vec![busy_screen(), enotfound(), busy_screen()],
        3,
        3,
    );
    let told = host.told();
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED")),
        "the try was made: {lines:#?}\n{:#?}",
        m.requests
    );
    assert!(
        matches!(told.as_slice(), [Some(_), None]),
        "a plan, then none once the try was typed — the run ends in the busy turn after it: \
         {told:#?}"
    );
    // Control: no try, no clearing: the rung is an hour off and the run ends
    // waiting, so the host holds the one plan it was told.
    let (lines, host, _, _) = outage_under(&[], 2);
    let told = host.told();
    let plans: Vec<&crate::supervise::WaitPlan> = told.iter().flatten().collect();
    assert_eq!(
        plans.len(),
        1,
        "told once, however many steps: {told:#?}\n{lines:#?}"
    );
    assert!(
        told.first().is_some_and(Option::is_some),
        "the plan comes first: {told:#?}"
    );
    let rung = plans[0].at_unix - TEST_NOW;
    assert!(
        (3590..=3600).contains(&rung),
        "the rung from the loop's clock: {rung}"
    );
    assert!(!plans[0].why.is_empty(), "{:?}", plans[0]);
}

/// The check that the try was taken is told as no plan: after the try the
/// worker stays at the wall, so the policy stands in `the worker to take the
/// api-retry@v1 act`, a wait the journal shows, and the host must never be
/// told it as a plan. The filter in `Session::tell_wait` (`Due::retry`, the
/// policy's `WaitUntil` `retry`) is the whole of it; without it this test
/// fails (a `WaitPlan` whose `why` is the check's). Each read is a new
/// point (the done row's seconds tick) so the point is decided again.
#[test]
fn the_act_check_is_told_to_the_host_as_no_plan() {
    let tick = |n: u32| -> Vec<String> {
        enotfound()
            .into_iter()
            .map(|r| r.replace("Worked for 3m 0s", &format!("Worked for 3m {n}s")))
            .collect()
    };
    let screens = vec![busy_screen(), tick(0), tick(1), tick(2), tick(3), tick(4)];
    let (lines, host, _, waiting) = short_outage("te-plan-act-check", screens, 150, 30);
    let told = host.told();
    assert!(
        waiting.iter().any(|w| w.contains("the worker to take")),
        "control: the act check's wait stood: {waiting:#?}\n{lines:#?}"
    );
    assert!(
        told.iter().flatten().count() >= 1,
        "control: the rungs were told: {told:#?}"
    );
    assert!(
        told.iter()
            .flatten()
            .all(|p| !p.why.contains("the worker to take")),
        "the act check was told as a next try: {told:#?}"
    );
}

/// A wait is told at the wall second it falls at ([`Due::unix_ms`]), once
/// however many steps, and a sleep does not move it: the deadline falls at
/// that second however long the Mac slept ([`Session::due_at`]). The same
/// `Instant` set again after a sleep (the policy's own pauses are `Instant`s,
/// which a sleep stops) names a later second, and is told again. Controls:
/// the sleep alone, and the same `Instant` set again a second or two off (the
/// two clocks read a moment apart), tell nothing.
#[test]
fn a_wait_is_told_at_its_wall_second_and_again_only_when_that_second_moves() {
    let host = ReachHost::scripted(&[]);
    let mut m = Mock::new(true, vec![busy_screen()]);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(2)
    };
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    let at = Instant::now() + Duration::from_secs(3600);
    s.set_wait(at, "api-error retry 1", true);
    s.tell_wait(&opts);
    s.tell_wait(&opts);
    let told = host.told();
    assert_eq!(told.len(), 1, "told once, however many steps: {told:#?}");
    let first = told[0].as_ref().expect("a plan").at_unix;
    assert!(
        (TEST_NOW + 3599..=TEST_NOW + 3600).contains(&first),
        "the deadline's wall second: {first}"
    );
    // Ten minutes of sleep: the wall clock jumps, the deadline does not.
    s.set_clock(TEST_NOW + 600, PDT);
    s.tell_wait(&opts);
    assert_eq!(host.told().len(), 1, "{:#?}", host.told());
    // The same `Instant` set again after the sleep: ten minutes later.
    s.set_wait(at, "api-error retry 1", true);
    s.tell_wait(&opts);
    let told = host.told();
    assert_eq!(told.len(), 2, "told again: {told:#?}");
    let again = told[1].as_ref().expect("a plan again").at_unix;
    assert!(
        (first + 599..=first + 601).contains(&again),
        "{first} then {again}"
    );
    // Set again two seconds on: no news.
    s.set_clock(TEST_NOW + 602, PDT);
    s.set_wait(at, "api-error retry 1", true);
    s.tell_wait(&opts);
    assert_eq!(host.told().len(), 2, "{:#?}", host.told());
}

/// The loop reads the wall of a tall pane's short transcript: Claude Code
/// 2.1.284's fullscreen renderer in a 144x50 pane, its first turn ended on
/// `⏺ API Error: Can't reach the API server` (`API_ERROR_TALL_PANE_MEASURED`),
/// the error on row 8, 35 blank rows, the composer and its footer on rows
/// 46-49. The loop's `tail=40` read holds rows 10-49, its last row the
/// footer, so neither [`tail_misses_the_live_rows`] nor [`drawn_above_tail`]
/// takes the whole screen; the tail holds a gap ([`gap_above_tail`]), so it
/// is read again whole and cut to the zone, and the outage is the outage:
/// `WAITING … api-error retry 1`, a plan told, the try typed. Controls: the
/// same pane with the gap filled (the error more than 40 rows up and outside
/// the zone) reads its tail alone and sees no wall; the outage on a pane
/// short enough to hold it in the tail is the same wall, read without the
/// second read.
#[test]
fn the_loop_reads_the_wall_of_a_tall_panes_short_transcript() {
    use aterm_phase::prompt::fixtures::{API_ERROR_TALL_PANE_MEASURED, screen};
    let whole_reads = |m: &Mock| {
        m.requests
            .iter()
            .filter(|r| r.trim_start_matches("@s-1 ") == "text --json")
            .count()
    };
    let run = |tag: &str, rows: Vec<String>| {
        let (lines, host, m, waiting) = short_outage(tag, vec![busy_screen(), rows], 4, 3);
        (lines, host.told(), whole_reads(&m), waiting, m.requests)
    };
    let tall = screen(API_ERROR_TALL_PANE_MEASURED);
    assert_eq!(tall.len(), 50, "the measured pane");
    let (lines, told, whole, waiting, requests) = run("te-tall-pane", tall.clone());
    assert!(
        waiting.iter().any(|w| w.contains("api-error retry 1")),
        "the wall is read: {waiting:#?}\n{lines:#?}\n{requests:#?}"
    );
    assert!(
        told.iter().flatten().any(|p| p.why.contains("api-error")),
        "a plan told: {told:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("CONTINUED")),
        "the try typed: {lines:#?}"
    );
    assert!(whole > 0, "the tail was read again whole: {requests:#?}");

    // Control: a full transcript — the gap filled — has the error outside
    // its zone and its tail stands: no wall, and no second read.
    let mut full = tall.clone();
    for row in &mut full[11..46] {
        *row = "· earlier".to_string();
    }
    let (lines, told, whole, waiting, requests) = run("te-tall-pane-full", full);
    assert!(
        !waiting.iter().any(|w| w.contains("api-error")),
        "the error is outside the zone: {waiting:#?}\n{lines:#?}"
    );
    assert!(told.iter().flatten().count() == 0, "{told:#?}");
    assert_eq!(whole, 0, "the tail alone: {requests:#?}");

    // Control: the same outage on a pane that holds it in the tail (the gap
    // cut to nothing, 30 rows) is the same wall, from the tail alone.
    let mut short = tall.clone();
    short.drain(11..31);
    let (lines, told, whole, waiting, requests) = run("te-tall-pane-short", short);
    assert!(
        waiting.iter().any(|w| w.contains("api-error retry 1")),
        "{waiting:#?}\n{lines:#?}"
    );
    assert!(told.iter().flatten().count() >= 1, "{told:#?}");
    assert_eq!(whole, 0, "the whole pane fits the tail: {requests:#?}");
}

/// The `await`s the loop made before its first `turn` (none: every one).
fn awaits_before_turn(m: &Mock) -> usize {
    let end = m
        .requests
        .iter()
        .position(|r| r.starts_with("turn "))
        .unwrap_or(m.requests.len());
    m.requests[..end]
        .iter()
        .filter(|r| r.starts_with("await "))
        .count()
}

/// The API back, within one step: at the outage's point the host measures
/// the API down (the hold journaled, nothing typed), and its measure turns up
/// while the loop waits: the point is decided again at the top of the very
/// next step, and continued exactly once under `api-back@v1`, in words that
/// name the wall and say the API is back, with no rung and no hold waited
/// out. Each further Down answer costs exactly one step more (one more
/// `await` before the `turn`). Negative control: a host that stays down
/// types nothing before the hold; the point is held, and journaled so.
#[test]
fn an_up_measure_mid_wait_continues_the_outage_within_one_step() {
    let t = Instant::now();
    let down = Reach::Down { since: t };
    let up = Reach::Up {
        since: t + Duration::from_secs(1),
    };
    let (lines, host, m, waiting) = outage_under(&[down, down, down, up], 30);
    let continued: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(continued.len(), 1, "{lines:#?}");
    assert!(
        continued[0].starts_with(
            "CONTINUED seq=102 rule=api-back@v1 Claude Code reported an API error (the API could \
             not be reached), and"
        ) && continued[0].contains("the API is reachable again now"),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert_eq!(waiting.len(), 1, "the hold, said once: {waiting:#?}");
    assert!(waiting[0].contains("the API is unreachable"), "{waiting:?}");
    // Asked at the point, at the top of each of the three steps, and by the
    // decision the Up made and the act's record of it; never after the wall
    // left (the next point, a request for a decision, asks nothing).
    assert_eq!(host.asks(), 6, "{lines:#?}");

    // One Down more is exactly one step more: the Up acts in its own step.
    let (_, _, longer, _) = outage_under(&[down, down, down, down, up], 30);
    assert_eq!(
        awaits_before_turn(&longer),
        awaits_before_turn(&m) + 1,
        "{:#?}\n{:#?}",
        m.requests,
        longer.requests
    );

    // Negative control: the host stays Down, nothing typed before the hold.
    let (lines, host, m, waiting) = outage_under(&[down], 1);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert!(
        waiting.iter().any(|w| w.contains("the API is unreachable")),
        "{waiting:#?}"
    );
    assert!(host.asks() > 2, "asked at each step while it waited");
}

/// A Down lost is back on the ladder: at the outage's point the host
/// measures the API down, and the hold (an hour here) is journaled; while the
/// loop waits, the measure is lost (stale, or a probe past its budget:
/// `Unknown`). The point is decided again at the top of that step on the
/// measure as it stands (here the unmeasured wall's ladder, whose first rung
/// is already past) and continued once under `api-retry@v1`, never held out
/// to the Down's hour. Negative control: the same clocks with the host
/// staying Down type nothing (the hold stands).
#[test]
fn a_down_measure_lost_mid_wait_puts_the_outage_back_on_the_ladder() {
    let t = Instant::now();
    let down = Reach::Down { since: t };
    let clocks = || TurnEndTiming {
        net_backoff: vec![Duration::ZERO],
        down_hold: Duration::from_secs(3600),
        ..TurnEndTiming::default()
    };
    let (lines, _, m, waiting) = outage_timed(&[down, down, Reach::Unknown], 30, clocks());
    let continued: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(continued.len(), 1, "{lines:#?}");
    assert!(
        continued[0].starts_with(
            "CONTINUED seq=102 rule=api-retry@v1 Claude Code reported an API error (the API \
             could not be reached);"
        ) && continued[0].contains("; trying again."),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert_eq!(waiting.len(), 1, "the hold, said once: {waiting:#?}");
    assert!(waiting[0].contains("the API is unreachable"), "{waiting:?}");

    // Negative control: the host stays Down, so the hold stands.
    let (lines, _, m, _) = outage_timed(&[down], 1, clocks());
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
}

/// Asking is waiting: an ordinary point (a turn that ended after work, a
/// request for a decision) never asks the host for the API's reach, so a
/// host's probe runs only while some loop waits at a network wall. At a
/// network wall the owner's policy escalates (`retry_api_errors = false`),
/// the host is asked once, at the point, and never again: no wait of the
/// policy's stands, so a measure turning up cannot decide the point again
/// and post its ask again (`escalate_point` has no dedupe but the point).
#[test]
fn an_ordinary_or_escalated_point_never_asks_for_the_reach_again() {
    let host = ReachHost::scripted(&[Reach::Up {
        since: Instant::now(),
    }]);
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            busy_screen(),
            ended(STOP),
        ],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert!(
        lines.contains(&"CONTINUED seq=102 rule=continue@v1 keep going".to_string()),
        "{lines:#?}"
    );
    assert_eq!(host.asks(), 0, "an ordinary point asks nothing: {lines:#?}");

    let host = ReachHost::scripted(&[
        Reach::Down {
            since: Instant::now(),
        },
        Reach::Up {
            since: Instant::now(),
        },
    ]);
    let mut m = Mock::new(true, vec![busy_screen(), enotfound()]);
    m.vanish_after = Some(4);
    let mut off = hosted(30);
    off.policy.retry_api_errors = false;
    off.idle_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
    let (lines, _) = watch_lines(&mut m, &off);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}");
    assert_eq!(count(&m, "meta set attention"), 1, "{:#?}", m.requests);
    assert_eq!(host.asks(), 1, "only at the point: {lines:#?}");
}

// --- Codex's rate-limit nudge and its save-then-wait switch -------------------

/// The measured rate-limit nudge (0.163.0, a 50-row pane) as a `tail=40`
/// read serves it: its last 40 rows. Whole, the blank gap above the box sends
/// the loop to a second, whole read ([`super::super::run`]'s `gap_above_tail`),
/// which a script answers with its NEXT screen, as if the box had gone.
fn nudge() -> Vec<String> {
    let rows = aterm_phase::prompt::fixtures::screen(aterm_phase::codex::fixtures::RATE_NUDGE);
    rows[rows.len().saturating_sub(40)..].to_vec()
}

/// A Codex screen whose footer shows `model` and, right-aligned, `goal`.
fn codex_footer(mut rows: Vec<String>, model: &str, goal: Option<&str>) -> Vec<String> {
    let at = rows
        .iter()
        .rposition(|r| r.starts_with("  GPT-"))
        .expect("the footer");
    let left = format!("  {model} · ~/pj · Fix the parser bugs · Main [default]");
    rows[at] = match goal {
        Some(g) => format!(
            "{left}{}{g}",
            " ".repeat(120 - left.chars().count() - g.len())
        ),
        None => left,
    };
    rows
}

/// The switch's rows as a loop that opened it wrote them, up to `phase`
/// ([`crate::supervise::approvals::wind_switch_words`]), on the test's clock
/// ([`TEST_NOW`]): the loop reads a person's keystrokes against the rows'
/// times.
fn seed_wind(path: &std::path::Path, sid: Option<&str>, back_at_unix: i64, phase: &str) {
    use crate::supervise::approvals::{Outcome, Row};
    let now = TEST_NOW * 1000;
    let words = |p: &str| {
        format!(
            "(model switch: kind=wind-down from=GPT-6-Astra effort=ultra to=gpt-6-luna \
             back_at={back_at_unix} marker=ATERM-SAVED-3f9a1c2e goal=paused phase={p})"
        )
    };
    let owed = format!("unproven: near {}", words("owed"));
    let winding = format!("the turn-end policy {}", words("winding"));
    let later = format!("the turn-end policy {}", words(phase));
    let rows = [
        Row {
            rule_id: crate::supervise::policy::RULE_RATE_NUDGE_SWITCH,
            outcome: Outcome::Approved,
            command: "Approaching rate limits => Switch to gpt-6-luna",
            reason: &owed,
            box_seq: 90,
        }
        .to_json(now - 30 * 60_000, sid),
        Row {
            rule_id: crate::supervise::policy::turn_end::RULE_WIND_DOWN,
            outcome: Outcome::Typed,
            command: "[aterm harness] GPT-6-Astra is close to its usage limit …",
            reason: &winding,
            box_seq: 91,
        }
        .to_json(now - 29 * 60_000, sid),
        Row {
            rule_id: crate::supervise::policy::turn_end::RULE_MODEL_RESTORE,
            outcome: Outcome::Skipped,
            command: "back on GPT-6-Astra ultra",
            reason: &later,
            box_seq: 92,
        }
        .to_json(now - 20 * 60_000, sid),
    ];
    std::fs::write(path, rows.join("\n") + "\n").expect("the ledger");
}

/// A hold survives the loop's restart: a switch whose last row says the
/// session is held on GPT-6-Astra ultra until its window resets, read back by
/// a loop that starts at the idle point the hold left, types nothing into the
/// idle Codex (no continuation, `/model` or answer) and raises nothing. The
/// switch's rows round-trip ([`crate::supervise::approvals::open_wind_down`]).
/// Negative controls: a switch whose last row says `done` is no switch, and
/// the point is continued; `released` too.
#[test]
fn a_codex_hold_is_read_back_and_types_nothing() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let test_now: i64 = 1_789_660_500;
    let back = test_now + 3 * 86_400;
    for (phase, holds) in [("holding", true), ("done", false), ("released", false)] {
        let (dir, ledger) = ledger_at(&format!("wind-{phase}"));
        seed_wind(&ledger, None, back, phase);
        let open = crate::supervise::approvals::open_wind_down(&ledger, None);
        assert_eq!(open.is_some(), holds, "{phase}: {open:?}");
        if let Some(o) = &open {
            assert_eq!(o.from.words(), "GPT-6-Astra ultra");
            assert_eq!((o.to.as_str(), o.back_at_unix), ("gpt-6-luna", Some(back)));
            assert!(o.goal_paused);
        }
        // The session sits idle where the hold left it; with no back-off
        // the first point is continued at once unless the switch holds it.
        let idle = codex_footer(screen(cx::END_OF_TURN), "GPT-6-Astra ultra", None);
        let mut m = Mock::new(true, vec![idle]);
        m.program = "codex";
        m.cursor_on_caret = true;
        m.gen_fence = true;
        m.sends_gen = true;
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                short_backoff: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
        let typed: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("turn ") || r.starts_with("send "))
            .collect();
        if holds {
            assert!(typed.is_empty(), "{phase}: {typed:#?}\n{lines:#?}");
            assert_eq!(count(&m, "meta set attention"), 0, "a hold raises nothing");
            assert!(
                !m.presses().iter().any(|p| p.ends_with(" esc")),
                "{:#?}",
                m.requests
            );
        } else {
            assert!(
                typed.iter().any(|r| r.contains("keep")),
                "{phase}: the control is continued: {typed:#?}\n{lines:#?}"
            );
        }
    }
}

/// Codex's pursued goal in the loop: an ended turn whose footer says the goal
/// is pursued gets nothing typed and nothing raised; Codex starts the next
/// turn itself. Negative control: the same point with the goal paused is
/// continued.
#[test]
fn a_codex_whose_goal_is_pursued_is_never_continued() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    for (goal, continued) in [
        (Some("Pursuing goal (10d 3h 2m)"), false),
        (Some("Goal paused (/goal resume)"), true),
    ] {
        let mut m = Mock::new(
            true,
            vec![
                codex_footer(screen(cx::BUSY), "GPT-6-Astra ultra", goal),
                codex_footer(screen(cx::END_OF_TURN), "GPT-6-Astra ultra", goal),
            ],
        );
        m.program = "codex";
        m.cursor_on_caret = true;
        m.gen_fence = true;
        m.sends_gen = true;
        m.vanish_after = Some(2);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let typed = m
            .requests
            .iter()
            .any(|r| r.starts_with("turn ") || r.starts_with("send "));
        assert_eq!(typed, continued, "{goal:?}: {lines:#?}\n{:#?}", m.requests);
        assert_eq!(
            count(&m, "meta set attention"),
            0,
            "{goal:?}: nothing raised"
        );
    }
}

/// Codex's rate-limit nudge near its limit, end to end over the scripted
/// server (a goal pursued, 99% of the weekly window): the nudge's `1` pressed
/// under `rate-nudge-switch@v1`, its row carrying the switch; the goal turn
/// under the box stopped by one Esc guarded on its status row; the save
/// instruction typed (never `keep going`); its marker line read as the save;
/// `/model` typed and the picker driven to the original model (Enter on the
/// model box) and effort (`s`, this conversation, on the effort box, never
/// Enter or a digit); the thread seen back on GPT-6-Astra medium, held:
/// nothing typed after. The ledger carries every edge, and reads back as a
/// hold.
#[test]
fn a_codex_nudge_near_its_limit_saves_the_work_restores_and_holds() {
    use crate::supervise::codex_usage::{CodexSeen, LimitRead};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let test_now: i64 = 1_789_660_500;
    let astra = "GPT-6-Astra medium";
    let luna = "GPT-6-Luna medium";
    let pursuing = Some("Pursuing goal (1h 2m)");
    let paused = Some("Goal paused (/goal resume)");
    let marker = crate::harness::upgrade::saved_marker(
        "-",
        "gpt-6-luna",
        u64::try_from(test_now).expect("after 1970"),
    );
    let mut saved = screen(cx::END_OF_TURN);
    let last = saved
        .iter()
        .rposition(|r| r.contains("as it evolves continuously."))
        .expect("the answer's last row");
    saved[last] = format!("  {marker}");
    let screens = vec![
        codex_footer(screen(cx::BUSY), astra, pursuing),
        nudge(),
        // The goal turn under the box: read as the box leaves, and again
        // for the Esc's guard.
        codex_footer(screen(cx::BUSY), luna, pursuing),
        codex_footer(screen(cx::BUSY), luna, pursuing),
        codex_footer(screen(cx::INTERRUPTED), luna, paused),
        codex_footer(screen(cx::BUSY), luna, paused),
        codex_footer(saved.clone(), luna, paused),
        screen(cx::MODEL_PICK),
        // The effort box: read as the model box leaves, and again to judge.
        screen(cx::EFFORT_PICK),
        screen(cx::EFFORT_PICK),
        codex_footer(saved, astra, paused),
    ];
    let mut m = Mock::new(true, screens);
    m.program = "codex";
    m.cursor_on_caret = true;
    m.gen_fence = true;
    m.sends_gen = true;
    m.turn_gates = vec![4, 6];
    m.vanish_after = Some(2);
    // `key` takes the generation fence (the picker's focus moves need it);
    // `send` does not, so text goes through the guarded `turn`.
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
    let (dir, ledger) = ledger_at("nudge-flow");
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(60, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_codex_records(CodexSeen {
            limits: LimitRead::Near {
                used: 99,
                back_at: Some(test_now + 3 * 86_400),
            },
            ..CodexSeen::default()
        });
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let open = crate::supervise::approvals::open_wind_down(&ledger, None);
    let _ = std::fs::remove_dir_all(&dir);
    let why = format!("{lines:#?}\n{:#?}\n{rows}", m.requests);
    let keys: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("key "))
        .collect();
    let turns: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("turn "))
        .collect();
    assert!(keys.iter().any(|k| k.ends_with(" 1")), "the switch: {why}");
    assert!(
        keys.iter()
            .any(|k| k.contains("esc.to.interrupt") && k.ends_with(" esc")),
        "the goal turn stopped: {why}"
    );
    assert!(
        turns.iter().any(|t| t.contains(&marker)),
        "the save instruction: {why}"
    );
    assert!(
        !turns.iter().any(|t| t.contains("keep going")),
        "never continued: {why}"
    );
    assert_eq!(
        turns.iter().filter(|t| t.ends_with(" /model")).count(),
        1,
        "the restore, once: {why}"
    );
    assert!(
        keys.iter().any(|k| k.ends_with(" enter")),
        "the model box: {why}"
    );
    assert!(
        keys.iter().any(|k| k.ends_with(" s")),
        "the effort, this conversation: {why}"
    );
    assert!(
        !keys.iter().any(|k| k.ends_with(" 2") || k.ends_with(" 3")),
        "no keep, no never-show-again, no digit on the picker: {why}"
    );
    for rule in [
        "rate-nudge-switch@v1",
        "model-wind-down@v1",
        "model-restore@v1",
        "model-restore-pick@v1",
    ] {
        assert!(rows.contains(rule), "{rule}: {why}");
    }
    assert!(rows.contains("phase=holding"), "{why}");
    // The press's intent is ledgered before its key, and the switch's rows
    // carry its stops.
    let intent = rows
        .lines()
        .position(|l| l.contains("pressing; the switch opens once the box leaves"))
        .unwrap_or_else(|| panic!("the intent row: {why}"));
    let approved = rows
        .lines()
        .position(|l| l.contains("\"approved\"") && l.contains("rate-nudge-switch@v1"))
        .unwrap_or_else(|| panic!("the approved row: {why}"));
    assert!(intent < approved, "{why}");
    // Both only intended until the box is seen leaving; the Esc's row, the
    // switch's first act, carries it as owed, with its Esc (`esc=`) and its
    // point still to come.
    for at in [intent, approved] {
        assert!(
            rows.lines()
                .nth(at)
                .is_some_and(|l| l.contains("phase=intent")),
            "{why}"
        );
    }
    assert!(
        rows.lines().any(|l| l.contains("model-wind-down@v1")
            && l.contains("\"typed\"")
            && l.contains("stops=1")
            && l.contains("phase=owed")
            && !l.contains("esc=-")),
        "{why}"
    );
    assert!(rows.contains("stops=1"), "{why}");
    // One Esc: the goal turn under the box.
    assert_eq!(
        keys.iter().filter(|k| k.ends_with(" esc")).count(),
        1,
        "{why}"
    );
    // The hold is said, in plain words, with the model it waits on.
    assert!(
        lines.iter().any(|l| l.starts_with("HOLDING ")
            && l.contains(
                "Codex saved its work and waits for GPT-6-Astra medium's limit to reset at"
            )),
        "{why}"
    );
    let open = open.unwrap_or_else(|| panic!("the hold reads back: {why}"));
    assert_eq!(open.phase, "holding");
    assert_eq!(open.from.words(), astra);
    assert!(open.since_unix.is_some(), "the hold's start is carried");
}

/// The goal stop, bound to the loop (the Tier-1 walk binds the decision,
/// `TurnEndState::goal_stop`; this binds the key): a switch owed its
/// wind-down (carried on from the ledger, as after a restart) and Codex
/// running a turn on the cheaper model: the busy read sends one Esc guarded
/// on the turn's status row (`key if=<busy guard> esc`), ledgered under
/// `model-wind-down@v1` with the switch's stops. Negative controls: a
/// person's keystroke within the grace (`status human_ms=`) sends nothing;
/// with no switch open, the same busy screen sends nothing.
#[test]
fn a_turn_running_while_the_switch_is_owed_is_stopped_on_its_busy_read() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let test_now: i64 = 1_789_660_500;
    let luna = "GPT-6-Luna medium";
    for (switch, human_ms, stops) in [
        (true, None, true),
        (true, Some(1_000), false),
        (false, None, false),
    ] {
        let (dir, ledger) = ledger_at(&format!("goal-stop-{switch}-{human_ms:?}"));
        if switch {
            seed_wind(&ledger, None, test_now + 3 * 86_400, "owed");
        }
        let screens = vec![
            codex_footer(screen(cx::BUSY), luna, Some("Pursuing goal (1h 2m)")),
            codex_footer(screen(cx::BUSY), luna, Some("Pursuing goal (1h 2m)")),
            codex_footer(
                screen(cx::INTERRUPTED),
                luna,
                Some("Goal paused (/goal resume)"),
            ),
        ];
        let mut m = Mock::new(true, screens);
        m.program = "codex";
        m.cursor_on_caret = true;
        m.gen_fence = true;
        m.sends_gen = true;
        m.vanish_after = Some(2);
        m.human_ms = human_ms;
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{lines:#?}\n{:#?}\n{rows}", m.requests);
        let escs: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" esc"))
            .collect();
        if stops {
            assert_eq!(escs.len(), 1, "{why}");
            assert!(escs[0].contains("esc.to.interrupt"), "guarded: {why}");
            assert!(
                rows.lines().any(|l| l.contains("model-wind-down@v1")
                    && l.contains("\"typed\"")
                    && l.contains("stops=1")),
                "{why}"
            );
        } else {
            assert!(escs.is_empty(), "{switch} {human_ms:?}: {why}");
        }
    }
}

/// A press of the nudge's switch that did not land (the box did not change):
/// its intent row is closed as `released`, no switch opens (nothing of the
/// wind-down goes), and a restarted loop reads none back.
#[test]
fn a_nudge_switch_that_did_not_land_opens_nothing() {
    use crate::supervise::codex_usage::{CodexSeen, LimitRead};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let test_now: i64 = 1_789_660_500;
    let astra = "GPT-6-Astra medium";
    let screens = vec![codex_footer(screen(cx::END_OF_TURN), astra, None), nudge()];
    let mut m = Mock::new(true, screens);
    m.program = "codex";
    m.cursor_on_caret = true;
    m.gen_fence = true;
    m.sends_gen = true;
    m.vanish_after = Some(3);
    let (dir, ledger) = ledger_at("nudge-missed");
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            approve: crate::supervise::config::Approve::All,
            ..SupervisorConfig::default()
        },
        ..auto(20, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_codex_records(CodexSeen {
            limits: LimitRead::Near {
                used: 99,
                back_at: Some(test_now + 3 * 86_400),
            },
            ..CodexSeen::default()
        });
        s.set_turn_end_timing(TurnEndTiming {
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let open = crate::supervise::approvals::open_wind_down(&ledger, None);
    let _ = std::fs::remove_dir_all(&dir);
    let why = format!("{lines:#?}\n{:#?}\n{rows}", m.requests);
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("key ") && r.ends_with(" 1")),
        "the switch pressed: {why}"
    );
    assert!(
        rows.contains("the box did not change after the press: the switch is not open"),
        "{why}"
    );
    assert!(open.is_none(), "{open:?}: {why}");
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("turn ") && r.contains("saving your work")),
        "{why}"
    );
}

/// A switch's three rows as a loop that opened it wrote them (the press, its
/// wind-down, then `phase` with `extra` words, `stops=3 told=goal`) on the
/// test's clock ([`TEST_NOW`]), the last `last_age_s` before it.
fn seed_wind_at(path: &std::path::Path, phase: &str, extra: &str, last_age_s: i64) {
    use crate::supervise::approvals::{Outcome, Row};
    let now_ms = TEST_NOW * 1000;
    let back = TEST_NOW + 3 * 86_400;
    let words = |p: &str, x: &str| {
        format!(
            "(model switch: kind=wind-down from=GPT-6-Astra effort=ultra to=gpt-6-luna \
             back_at={back} marker=ATERM-SAVED-3f9a1c2e goal=paused {x} phase={p})"
        )
    };
    let owed = format!("unproven: near {}", words("owed", ""));
    let winding = format!("the turn-end policy {}", words("winding", ""));
    let later = format!("the turn-end policy {}", words(phase, extra));
    let rows = [
        Row {
            rule_id: crate::supervise::policy::RULE_RATE_NUDGE_SWITCH,
            outcome: Outcome::Approved,
            command: "Approaching rate limits => Switch to gpt-6-luna",
            reason: &owed,
            box_seq: 90,
        }
        .to_json(now_ms - 40 * 60_000, None),
        Row {
            rule_id: crate::supervise::policy::turn_end::RULE_WIND_DOWN,
            outcome: Outcome::Typed,
            command: "[aterm harness] GPT-6-Astra is close to its usage limit …",
            reason: &winding,
            box_seq: 91,
        }
        .to_json(now_ms - 39 * 60_000, None),
        Row {
            rule_id: crate::supervise::policy::turn_end::RULE_MODEL_RESTORE,
            outcome: Outcome::Skipped,
            command: "the switch's last edge",
            reason: &later,
            box_seq: 92,
        }
        .to_json(now_ms - last_age_s * 1000, None),
    ];
    std::fs::write(path, rows.join("\n") + "\n").expect("the ledger");
}

/// A Codex loop over `screens` with the switch's rows in `ledger`.
fn codex_mock(screens: Vec<Vec<String>>, vanish: u32) -> Mock {
    let mut m = Mock::new(true, screens);
    m.program = "codex";
    m.cursor_on_caret = true;
    m.gen_fence = true;
    m.sends_gen = true;
    m.vanish_after = Some(vanish);
    m
}

/// A person's own turn is never stopped by the loop: a switch carried on from
/// the ledger (held, owed its save, owed its model back) and a turn running
/// whose person's keystroke (`status human_ms=121000`) is past the grace but
/// after the switch's last row (their `/goal resume`, their message). No Esc
/// goes. Negative control: a keystroke from before that row (25 minutes) is
/// no hand in the turn; the goal's turn is stopped, one Esc guarded on its
/// status row.
#[test]
fn a_persons_turn_past_the_grace_is_never_stopped() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let astra = "GPT-6-Astra ultra";
    let luna = "GPT-6-Luna medium";
    let pursuing = "Pursuing goal (1h 2m)";
    let paused = "Goal paused (/goal resume)";
    for (phase, model, goal, human_ms, stopped) in [
        ("holding", astra, pursuing, 121_000, false),
        ("owed", luna, paused, 121_000, false),
        ("restore", luna, paused, 121_000, false),
        ("holding", astra, pursuing, 1_500_000, true),
        ("owed", luna, paused, 1_500_000, true),
    ] {
        let (dir, ledger) = ledger_at(&format!("persons-turn-{phase}-{human_ms}"));
        seed_wind_at(&ledger, phase, "", 20 * 60);
        let mut m = codex_mock(
            vec![
                codex_footer(screen(cx::BUSY), model, Some(goal)),
                codex_footer(screen(cx::BUSY), model, Some(goal)),
                codex_footer(screen(cx::INTERRUPTED), model, Some(paused)),
            ],
            2,
        );
        m.human_ms = Some(human_ms);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{phase} {human_ms}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        let escs = m
            .requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" esc"))
            .count();
        assert_eq!(escs, usize::from(stopped), "{why}");
        assert_eq!(
            rows.lines()
                .any(|l| l.contains("\"typed\"") && l.contains("\"command\":\"esc\"")),
            stopped,
            "{why}"
        );
    }
}

/// AN OFF LOOP LEAVES THE SWITCH STANDING ON ITS BUSY READS (the review of
/// a295792e4): a save-then-wait switch carried on from the ledger, and the
/// session's word `off` read at the loop's start (a CLI `watch` over it).
/// The busy reads of the wait act on nothing: the goal's turn running on
/// Codex gets no Escape and no `INTERRUPTED` row, and a save's turn ended
/// under a background terminal is no point for the switch's steps (no
/// `/model`, no `BACKGROUND` line) — the switch stands until the word turns
/// (the design's §7), and the loop parks at the turn's point. NEGATIVE
/// CONTROL: the same screens under the word `cruise` stop the goal's turn
/// with one Escape and restore the model at the break.
#[test]
fn an_off_loop_leaves_the_switch_standing_on_its_busy_reads() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let astra = "GPT-6-Astra ultra";
    let paused = Some("Goal paused (/goal resume)");
    // The loop over `screens` under the session's `word`, the switch's rows
    // at `phase` in its ledger (the last `age_s` old), a person's last
    // keystroke `human_ms` ago: the requests, the journal and the ledger.
    let run = |word: &'static str,
               (phase, age_s, human_ms): (&str, i64, Option<u64>),
               screens: Vec<Vec<String>>,
               vanish: u32| {
        let tag = format!("off-switch-{phase}-{word}");
        let (dir, ledger) = ledger_at(&tag);
        let (jdir, journal) = journal_file(&tag);
        seed_wind_at(&ledger, phase, "", age_s);
        let mut m = codex_mock(screens, vanish);
        m.autopilot = Some(word);
        m.human_ms = human_ms;
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
            s.set_background_settle(Duration::ZERO);
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let said: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&jdir);
        let why = format!(
            "{word} {phase}: {lines:#?}\n{said:#?}\n{:#?}\n{rows}",
            m.requests
        );
        (m, said, rows, why)
    };
    let escs = |m: &Mock| {
        m.requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" esc"))
            .count()
    };
    let restores = |m: &Mock| {
        m.requests
            .iter()
            .filter(|r| r.starts_with("turn ") && r.ends_with(" /model"))
            .count()
    };

    // The hold, the goal pursued on the session's own model: the goal's
    // turn, nobody's hand in it.
    let goal_turn = || {
        vec![
            codex_footer(screen(cx::BUSY), astra, Some("Pursuing goal (1h 2m)")),
            codex_footer(screen(cx::BUSY), astra, Some("Pursuing goal (1h 2m)")),
            codex_footer(screen(cx::INTERRUPTED), astra, paused),
        ]
    };
    let held = ("holding", 20 * 60, Some(1_500_000));
    let (m, said, rows, why) = run("off", held, goal_turn(), 2);
    assert_eq!(escs(&m), 0, "{why}");
    assert!(!said.iter().any(|l| l.starts_with("INTERRUPTED")), "{why}");
    assert!(!rows.contains("\"command\":\"esc\""), "{why}");
    assert!(said.iter().any(|l| l.starts_with("PARKED seq=")), "{why}");
    // NEGATIVE CONTROL: Cruise stops it.
    let (m, said, _, why) = run("cruise", held, goal_turn(), 2);
    assert_eq!(escs(&m), 1, "{why}");
    assert!(said.iter().any(|l| l.starts_with("INTERRUPTED")), "{why}");

    // The save's turn ended under a background terminal.
    let saved = ["• Committed and pushed.", "", "  ATERM-SAVED-3f9a1c2e"];
    let end = under_background(&saved, "GPT-6-Luna medium", paused, true);
    let winding = ("winding", 60, None);
    let (m, said, _, why) = run(
        "off",
        winding,
        vec![end.clone(), end.clone(), end.clone()],
        3,
    );
    assert_eq!(count(&m, "turn"), 0, "nothing typed, no `/model`: {why}");
    assert!(!said.iter().any(|l| l.starts_with("BACKGROUND")), "{why}");
    // NEGATIVE CONTROL: Cruise takes the break as the switch's point.
    let (m, said, _, why) = run("cruise", winding, vec![end.clone(), end.clone(), end], 3);
    assert_eq!(restores(&m), 1, "{why}");
    assert!(said.iter().any(|l| l.starts_with("BACKGROUND")), "{why}");
}

/// The latch: a busy read that saw a person's hand in the running turn keeps
/// it theirs until the turn's point, however the keystroke ages; the turn's
/// own work keeps it theirs past the grace. A keystroke from before the open
/// switch opened is none of any turn it reads. Negative controls: unlatched,
/// a keystroke older than the turn so far is none; one after the opening,
/// within the turn, is theirs.
#[test]
fn a_persons_hand_seen_in_a_turn_is_latched_until_its_point() {
    use crate::supervise::policy::turn_end::{CodexSetting, RunningTurn, WindDown};
    let mut m = Mock::new(true, vec![rows(&["x"])]);
    let mut s = session(&mut m, None);
    let now = Instant::now() + Duration::from_secs(3600);
    let ago = |d: u64| now.checked_sub(Duration::from_secs(d));
    s.running.busy(now);
    s.person = ago(1);
    assert!(s.person_in_this_turn(now), "within the turn");
    s.person = ago(400);
    assert!(s.person_in_this_turn(now), "latched");
    s.running = RunningTurn::default();
    s.running.busy(now);
    assert!(!s.person_in_this_turn(now), "unlatched: before the turn");
    s.running = RunningTurn::default();
    s.running.busy(ago(400).expect("the clock"));
    assert!(s.person_in_this_turn(now), "the turn's own work");
    // The floor: the switch opened 10 s ago; a keystroke a minute ago, in
    // the span the loop kept, is none of the goal turn running now.
    s.turn_end.open_switch(WindDown {
        opened_at: ago(10),
        ..WindDown::opened(
            CodexSetting {
                model: "GPT-6-Astra".to_string(),
                effort: Some("ultra".to_string()),
            },
            "gpt-6-luna".to_string(),
            None,
            "ATERM-SAVED-3f9a1c2e".to_string(),
        )
    });
    s.running = RunningTurn::default();
    s.running.busy(ago(600).expect("the clock"));
    s.person = ago(60);
    assert!(!s.person_in_this_turn(now), "floored at the opening");
    s.person = ago(5);
    assert!(s.person_in_this_turn(now), "after the opening: theirs");
}

/// A Codex screen whose turn ended under a background terminal (`1
/// background terminal running`, which reads busy for as long as it runs),
/// the agent's last words `said`, its footer `model` and `goal`.
fn under_background(said: &[&str], model: &str, goal: Option<&str>, bg: bool) -> Vec<String> {
    let mut r = vec![
        "› [aterm harness] GPT-6-Astra is close to its usage limit ...".to_string(),
        String::new(),
    ];
    for l in said {
        r.push((*l).to_string());
    }
    r.extend([String::new(), "  1:41 AM".to_string(), String::new()]);
    if bg {
        r.push("  1 background terminal running · /ps to view · /stop to close".to_string());
        r.push(String::new());
    }
    r.push("› Ask Codex to do anything".to_string());
    r.push(String::new());
    r.push("  GPT-6-Luna medium · ~/pj · Main [default]".to_string());
    codex_footer(r, model, goal)
}

/// A host that wants every point, counts the background breaks offered, and
/// keeps the guards the loop said withheld one ([`IdleHost::withheld`]).
#[derive(Debug, Default)]
struct BackgroundHost {
    backgrounds: std::sync::atomic::AtomicUsize,
    informed: std::sync::Mutex<Vec<String>>,
    withheld: std::sync::Mutex<Vec<crate::supervise::Guard>>,
}

impl IdleHost for BackgroundHost {
    fn wants(&self) -> bool {
        true
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn at_background(&self) -> Option<String> {
        self.backgrounds.fetch_add(1, Ordering::SeqCst);
        Some("upgrade step=announced:1".to_string())
    }
    fn inform(&self, text: &str) {
        self.informed.lock().unwrap().push(text.to_string());
    }
    fn withheld(&self, guard: crate::supervise::Guard) {
        self.withheld.lock().unwrap().push(guard);
    }
}

/// No upgrade notice into an open switch at a background break: the host's
/// background step is offered no break while a save-then-wait switch is
/// open, as at an idle point, both over the loop (where the switch takes the
/// break as its own point first) and at the step's own gate (a loop that does
/// not take it: another supervisor's claim, an attended look), where the loop
/// says so to the host (`guard=switch-open`, [`IdleHost::withheld`]).
/// Negative control: with no switch, the same break is offered and nothing is
/// said withheld.
#[test]
fn the_hosts_background_step_waits_for_an_open_switch() {
    // The step's own gate.
    for open in [true, false] {
        let bg = under_background(
            &["• The server runs in the background."],
            "GPT-6-Astra ultra",
            None,
            true,
        );
        let host = Arc::new(BackgroundHost::default());
        let mut m = codex_mock(vec![bg.clone()], 3);
        m.program = "codex";
        let mut s = session(&mut m, None);
        s.program = Some("codex".to_string());
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.background_settle = Duration::ZERO;
        if open {
            s.turn_end
                .open_switch(crate::supervise::policy::turn_end::WindDown::opened(
                    crate::supervise::policy::turn_end::CodexSetting {
                        model: "GPT-6-Astra".to_string(),
                        effort: Some("ultra".to_string()),
                    },
                    "gpt-6-luna".to_string(),
                    None,
                    "ATERM-SAVED-3f9a1c2e".to_string(),
                ));
        }
        let screen = Screen {
            rows: bg,
            cursor_row: 0,
            cursor_col: 0,
            seq: 101,
            first: 0,
            generation: None,
            human: crate::supervise::screen::HumanInput::Unknown,
            human_seq: None,
            cols: None,
            pane_rows: None,
        };
        let mut since = Some(Instant::now());
        s.host_steps_in_background(&screen, &mut since, &mut Alone);
        let offered = host.backgrounds.load(Ordering::SeqCst);
        assert_eq!(offered > 0, !open, "switch open: {open}");
        let said = host.withheld.lock().unwrap().clone();
        let expected = if open {
            vec![crate::supervise::Guard::SwitchOpen]
        } else {
            vec![]
        };
        assert_eq!(said, expected, "switch open: {open}");
    }
    for phase in [Some("holding"), Some("owed"), None] {
        let (dir, ledger) = ledger_at(&format!("bg-host-{phase:?}"));
        if let Some(p) = phase {
            seed_wind_at(&ledger, p, "", 60);
        }
        let model = if phase == Some("owed") {
            "GPT-6-Luna medium"
        } else {
            "GPT-6-Astra ultra"
        };
        let bg = under_background(&["• The server runs in the background."], model, None, true);
        let host = Arc::new(BackgroundHost::default());
        let mut m = codex_mock(vec![bg], 3);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
            s.set_background_settle(Duration::ZERO);
        });
        let _ = std::fs::remove_dir_all(&dir);
        let offered = host.backgrounds.load(Ordering::SeqCst);
        assert_eq!(
            offered > 0,
            phase.is_none(),
            "{phase:?}: {offered} {lines:#?}"
        );
    }
}

/// The goal's note stays up: the stops spent and Codex's goal running on
/// while the switch stands, a person is told (the badge raised, the note
/// recorded for them by the host, and the switch's row carrying it as said,
/// `told=goal`), and the goal's next turn does not take the badge down. A
/// loop restarted from those rows says it no more. Negative control: the
/// footer shows the goal paused, and the badge goes.
#[test]
fn the_goal_note_stays_up_while_the_goal_runs() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let luna = "GPT-6-Luna medium";
    let pursuing = Some("Pursuing goal (1h 2m)");
    let paused = Some("Goal paused (/goal resume)");
    let (dir, ledger) = ledger_at("goal-note");
    seed_wind_at(&ledger, "owed", "stops=3", 60);
    let host = Arc::new(BackgroundHost::default());
    let mut m = codex_mock(
        vec![
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::END_OF_TURN), luna, pursuing),
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::END_OF_TURN), luna, pursuing),
        ],
        4,
    );
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
    });
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let why = format!("{lines:#?}\n{:#?}\n{rows}", m.requests);
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.contains("Codex's goal keeps running")),
        "the badge stands through the goal's next turn: {why}"
    );
    assert_eq!(count(&m, "meta set attention"), 1, "said once: {why}");
    assert!(
        host.informed
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.contains("Codex's goal keeps running")),
        "{why}"
    );
    assert!(rows.contains("told=goal"), "{why}");
    // A loop restarted from these rows says it no more.
    let mut again = codex_mock(
        vec![
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::END_OF_TURN), luna, pursuing),
        ],
        2,
    );
    let (lines, _) = watch_lines_with(&mut again, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
    });
    assert_eq!(
        count(&again, "meta set attention"),
        0,
        "{lines:#?}\n{:#?}",
        again.requests
    );
    let _ = std::fs::remove_dir_all(&dir);
    // The control: the goal seen paused, the badge goes.
    let (dir, ledger) = ledger_at("goal-note-paused");
    seed_wind_at(&ledger, "owed", "stops=3", 60);
    let mut m = codex_mock(
        vec![
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::END_OF_TURN), luna, pursuing),
            codex_footer(screen(cx::END_OF_TURN), luna, paused),
        ],
        3,
    );
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        count(&m, "meta set attention"),
        1,
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "meta unset attention"), 1, "{:#?}", m.requests);
    assert_eq!(m.attention, None, "{lines:#?}\n{:#?}", m.requests);
}

/// A press only intended, read back: a loop that died between the intent row
/// and the key finds the nudge still up, its row only intended
/// (`phase=intent`); the box is decided again and its switch pressed, near
/// the limit. Negative control: a switch whose row says it landed (`owed`)
/// keeps the model at the same nudge.
#[test]
fn an_intended_press_leaves_the_nudge_to_be_decided_again() {
    use crate::supervise::codex_usage::{CodexSeen, LimitRead};
    for (phase, switched) in [("intent", true), ("owed", false)] {
        let (dir, ledger) = ledger_at(&format!("intent-{phase}"));
        intent_ledger(&ledger, phase);
        // The loop starts at the box, which covers the footer: the intended
        // switch's own `from` is the thread's model.
        let mut m = codex_mock(vec![nudge()], 2);
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                approve: crate::supervise::config::Approve::All,
                ..SupervisorConfig::default()
            },
            ..auto(20, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(CodexSeen {
                limits: LimitRead::Near {
                    used: 99,
                    back_at: Some(TEST_NOW + 3 * 86_400),
                },
                ..CodexSeen::default()
            });
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{phase}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        assert_eq!(
            rows.contains("rate-nudge-switch@v1\",\"decision\":\"approved\""),
            switched,
            "{why}"
        );
        assert_eq!(
            rows.contains("rate-nudge-keep@v1\",\"decision\":\"approved\""),
            !switched,
            "{why}"
        );
    }
}

/// A switch that cannot go on under a background terminal is said, naming
/// what holds it: the save owed at a break a person is typing through types
/// nothing, and past the bound (shortened here) a person is told once that
/// their typing holds it; with nobody's hand the save is typed, and when the
/// break does not move after it, the terminal is named, with `/ps`, `/stop`.
/// Neither advises putting the model back by hand before the save. The note
/// rides its own row (`told=background`), so a loop restarted from it says
/// it no more. Negative control: within the bound, nothing is raised.
#[test]
fn a_switch_standing_still_under_a_background_terminal_is_said() {
    for (bound, human_ms, said) in [
        (
            Duration::ZERO,
            Some(1_000),
            Some("a person is typing into the session"),
        ),
        (
            Duration::ZERO,
            None,
            Some("a Codex background terminal keeps the session busy"),
        ),
        (Duration::from_secs(3600), Some(1_000), None),
    ] {
        let (dir, ledger) = ledger_at(&format!("bg-still-{human_ms:?}-{}", said.is_some()));
        seed_wind_at(&ledger, "owed", "", 60);
        let stopped = under_background(
            &["• The parser's second step is done."],
            "GPT-6-Luna medium",
            Some("Goal paused (/goal resume)"),
            true,
        );
        let mut m = codex_mock(vec![stopped.clone(), stopped.clone(), stopped.clone()], 3);
        m.human_ms = human_ms;
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
            s.set_switch_break_note(bound);
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let why = format!(
            "{bound:?} {human_ms:?}: {lines:#?}\n{:#?}\n{rows}",
            m.requests
        );
        let typed = m
            .requests
            .iter()
            .filter(|r| r.starts_with("turn ") && r.contains("saving your work"))
            .count();
        assert_eq!(typed, usize::from(human_ms.is_none()), "{why}");
        let noted: Vec<&str> = rows
            .lines()
            .filter(|l| l.contains("the session stays on gpt-6-luna meanwhile"))
            .collect();
        match said {
            Some(held) => {
                assert_eq!(noted.len(), 1, "said once: {why}");
                assert!(noted[0].contains(held), "{why}");
                assert!(noted[0].contains("told=background"), "its row: {why}");
                assert_eq!(
                    noted[0].contains("/ps"),
                    human_ms.is_none(),
                    "the terminal named with `/ps`: {why}"
                );
                assert!(
                    !noted[0].contains("/model"),
                    "no hand `/model` before the save: {why}"
                );
                assert_eq!(count(&m, "meta set attention"), 1, "{why}");
                // A loop restarted from these rows says it no more.
                let mut again = codex_mock(vec![stopped.clone(), stopped.clone()], 2);
                again.human_ms = human_ms;
                let (lines, _) = watch_lines_with(&mut again, &opts, |s| {
                    s.set_approval_ledger(Some(ledger.clone()));
                    s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
                    s.set_switch_break_note(bound);
                });
                let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
                assert_eq!(
                    rows.lines()
                        .filter(|l| l.contains("the session stays on gpt-6-luna meanwhile"))
                        .count(),
                    1,
                    "{lines:#?}\n{:#?}\n{rows}",
                    again.requests
                );
            }
            None => {
                assert!(noted.is_empty(), "{why}");
                assert_eq!(count(&m, "meta set attention"), 0, "{why}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// --- the goal turn under the nudge's box -----------------------------------

/// The goal turn under the nudge's box is stopped, whoever began the turn the
/// box covered: the nudge flow near the limit, the loop's span of the turn
/// before the box preset (a person's message began it ten minutes ago,
/// `human_ms=610000`; typed into it five minutes ago, `300000`; or their
/// `/goal` three hours ago with the goal's turns chained since), and Codex's
/// goal runs its own turn under the box on GPT-6-Luna: one Esc, guarded on
/// its status row, as with nobody's hand (the control). (A person's
/// keystroke after the switch opened is their hand in the goal turn:
/// `a_persons_hand_seen_in_a_turn_is_latched_until_its_point`.)
#[test]
fn the_goal_turn_under_the_box_is_stopped_whoever_began_the_covered_turn() {
    use crate::supervise::codex_usage::{CodexSeen, LimitRead};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let astra = "GPT-6-Astra medium";
    let luna = "GPT-6-Luna medium";
    let pursuing = Some("Pursuing goal (1h 2m)");
    let paused = Some("Goal paused (/goal resume)");
    for (label, human_ms, covered_s, escs) in [
        (
            "a person began the covered turn",
            Some(610_000u64),
            Some(600u64),
            1,
        ),
        (
            "a person typed into the covered turn",
            Some(300_000),
            Some(600),
            1,
        ),
        (
            "a person's /goal three hours ago, the goal chained since",
            Some(3 * 3_600_000 + 5_000),
            Some(3 * 3600),
            1,
        ),
        ("nobody's hand", None, Some(600), 1),
    ] {
        let screens = vec![
            codex_footer(screen(cx::BUSY), astra, pursuing),
            nudge(),
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::INTERRUPTED), luna, paused),
        ];
        let mut m = Mock::new(true, screens);
        m.program = "codex";
        m.cursor_on_caret = true;
        m.gen_fence = true;
        m.sends_gen = true;
        m.vanish_after = Some(2);
        m.human_ms = human_ms;
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        let (dir, ledger) = ledger_at(&format!("covered-{}", label.len()));
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(CodexSeen {
                limits: LimitRead::Near {
                    used: 99,
                    back_at: Some(TEST_NOW + 3 * 86_400),
                },
                ..CodexSeen::default()
            });
            if let Some(c) = covered_s {
                s.running.busy(
                    Instant::now()
                        .checked_sub(Duration::from_secs(c))
                        .expect("the clock"),
                );
            }
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{label}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        assert!(
            m.requests
                .iter()
                .any(|r| r.starts_with("key ") && r.ends_with(" 1")),
            "the switch pressed: {why}"
        );
        let esc: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" esc"))
            .collect();
        assert_eq!(esc.len(), escs, "{why}");
        assert!(esc[0].contains("esc.to.interrupt"), "guarded: {why}");
        assert!(
            rows.lines().any(|l| l.contains("model-wind-down@v1")
                && l.contains("\"typed\"")
                && l.contains("\"command\":\"esc\"")),
            "ledgered: {why}"
        );
    }
}

/// A carried-on hold's escaped goal is stopped though a person typed a day
/// ago: the hold's last row three days old, a person's keystroke a day ago
/// (after the row, long before the turn in flight), and Codex's goal running
/// on the thread's own model at the restart: one Esc, the hold stands (never
/// "a person resumed Codex's goal during the hold"), and the goal is paused
/// at its point. Negative control: a keystroke two minutes ago (their `/goal
/// resume`) is theirs: no Esc, and its end releases the hold. (The switch
/// claims the goal it stops on the tab's goal record, `harness::goal_hold`; a
/// goal the upgrade holds there is never the switch's:
/// `a_goal_the_upgrade_holds_is_never_stopped_or_paused_by_the_switch`.)
#[test]
fn a_carried_on_holds_escaped_goal_is_stopped_past_an_old_keystroke() {
    use crate::harness::goal_hold::{self, Owner, Stage};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let astra = "GPT-6-Astra ultra";
    let pursuing = Some("Pursuing goal (1h 2m)");
    for (human_ms, theirs) in [(86_400_000u64, false), (120_000, true)] {
        let (dir, ledger) = ledger_at(&format!("seeded-old-{human_ms}"));
        seed_wind_at(&ledger, "holding", "", 3 * 86_400);
        let mut m = codex_mock(
            vec![
                codex_footer(screen(cx::BUSY), astra, pursuing),
                codex_footer(screen(cx::BUSY), astra, pursuing),
                codex_footer(screen(cx::END_OF_TURN), astra, pursuing),
            ],
            3,
        );
        m.human_ms = Some(human_ms);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let held = goal_hold::read(&goal_hold::path_beside(&ledger));
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{human_ms}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        let escs = m
            .requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" esc"))
            .count();
        assert_eq!(escs, usize::from(!theirs), "{why}");
        assert_eq!(
            rows.contains("a person resumed Codex's goal during the hold"),
            theirs,
            "{why}"
        );
        if !theirs {
            assert!(
                m.requests
                    .iter()
                    .any(|r| r.starts_with("turn ") && r.ends_with(" /goal pause")),
                "the goal paused at its point: {why}"
            );
            assert_eq!(
                held.map(|h| (h.owner, h.stage)),
                Some((Owner::Switch, Stage::Paused)),
                "claimed: {why}"
            );
        }
    }
}

/// A goal the live upgrade holds is never the switch's (the tab's goal
/// record, `harness::goal_hold`): the carried-on hold above, its goal turn
/// running, but the record beside the loop's ledger says the upgrade paused
/// Codex's goal for its move. The switch presses no Esc into the turn that
/// pause lets finish, types no `/goal pause`, and writes no claim of its own
/// over the upgrade's. Negative control: no record: the Esc, the pause and
/// the switch's claim on the record
/// (`a_carried_on_holds_escaped_goal_is_stopped_past_an_old_keystroke`, the
/// same run without the record).
#[test]
fn a_goal_the_upgrade_holds_is_never_stopped_or_paused_by_the_switch() {
    use crate::harness::goal_hold::{self, Hold, How, Owner, Stage};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let astra = "GPT-6-Astra ultra";
    let pursuing = Some("Pursuing goal (1h 2m)");
    let (dir, ledger) = ledger_at("upgrade-holds");
    seed_wind_at(&ledger, "holding", "", 3 * 86_400);
    let record = goal_hold::path_beside(&ledger);
    goal_hold::write(
        &record,
        &Hold {
            stage: Stage::Paused,
            ..Hold::pausing(Owner::Upgrade, How::Typed, 42, "0.158.0", 1)
        },
    )
    .expect("record");
    let mut m = codex_mock(
        vec![
            codex_footer(screen(cx::BUSY), astra, pursuing),
            codex_footer(screen(cx::BUSY), astra, pursuing),
            codex_footer(screen(cx::END_OF_TURN), astra, pursuing),
        ],
        3,
    );
    m.human_ms = Some(86_400_000);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
    });
    let held = goal_hold::read(&record);
    let _ = std::fs::remove_dir_all(&dir);
    let why = format!("{lines:#?}\n{:#?}", m.requests);
    let paused = m
        .requests
        .iter()
        .any(|r| r.starts_with("turn ") && r.ends_with(" /goal pause"));
    assert_eq!((escs(&m), paused), (0, false), "{why}");
    assert_eq!(
        held.map(|h| (h.owner, h.stage)),
        Some((Owner::Upgrade, Stage::Paused)),
        "{why}"
    );
}

/// A switch under a Codex background terminal goes on, however the turn
/// ended: the save's own turn ends with a background terminal still running
/// (Codex draws its line under the ended turn, and the screen reads busy for
/// as long as it runs), and the break is the switch's point as an idle end
/// is: the marker judged (`saved`) and `/model` typed once, the break read
/// again deciding nothing new; an owed save under the same line is typed.
/// Nothing else goes at that break: no continuation. A save that ends asking
/// (a rejected push, "Should I merge them and push again?") is judged not
/// saved (a person told, quoting its words); a saved turn that ends on an
/// offer is judged saved; a save that ended on luna's own usage wall under
/// the line is judged there too. Controls: the same screens without the line
/// do the same at an ordinary point.
#[test]
fn a_switch_under_a_background_terminal_goes_on_however_the_turn_ended() {
    let paused = Some("Goal paused (/goal resume)");
    let saved_plainly = ["• Committed and pushed.", "", "  ATERM-SAVED-3f9a1c2e"];
    let done = ["• The parser's second step is done."];
    let asks = [
        "• The push was rejected: the remote has two new commits.",
        "",
        "  Should I merge them and push again?",
    ];
    let offers = [
        "• Committed and pushed.",
        "",
        "  ATERM-SAVED-3f9a1c2e",
        "",
        "  Want me to open a pull request for the branch as well?",
    ];
    let wall = [
        "■ You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at",
        "Sep 25th, 2026 3:05 PM.",
    ];
    for bg in [true, false] {
        for (row, (phase, said, saved)) in [
            ("winding", &saved_plainly[..], Some(true)),
            ("winding", &asks[..], Some(false)),
            ("winding", &offers[..], Some(true)),
            ("winding", &wall[..], Some(false)),
            ("owed", &done[..], None),
            ("owed", &asks[..], None),
        ]
        .into_iter()
        .enumerate()
        {
            let (dir, ledger) = ledger_at(&format!("bg-ended-{phase}-{bg}-{row}"));
            seed_wind_at(&ledger, phase, "", 60);
            let end = under_background(said, "GPT-6-Luna medium", paused, bg);
            let mut m = codex_mock(vec![end.clone(), end.clone(), end], 3);
            let opts = SuperviseOpts {
                policy: SupervisorConfig::default(),
                ..auto(30, None)
            };
            let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
                s.set_approval_ledger(Some(ledger.clone()));
                s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
                s.set_background_settle(Duration::ZERO);
            });
            let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
            let _ = std::fs::remove_dir_all(&dir);
            let why = format!(
                "{phase} bg={bg} {:?}: {lines:#?}\n{:#?}\n{rows}",
                said.last(),
                m.requests
            );
            let turns: Vec<&String> = m
                .requests
                .iter()
                .filter(|r| r.starts_with("turn "))
                .collect();
            assert!(
                !turns.iter().any(|t| t.contains("keep going")),
                "never continued: {why}"
            );
            match saved {
                Some(saved) => {
                    assert_eq!(
                        turns.iter().filter(|t| t.ends_with(" /model")).count(),
                        1,
                        "the restore, once: {why}"
                    );
                    let judged = if saved {
                        "\"command\":\"saved\""
                    } else {
                        "\"command\":\"not saved\""
                    };
                    assert!(rows.contains(judged), "{why}");
                    assert_eq!(
                        rows.contains("did not confirm that the work is committed and pushed"),
                        !saved,
                        "{why}"
                    );
                }
                None => assert!(
                    turns.iter().any(|t| t.contains("saving your work")),
                    "the save: {why}"
                ),
            }
        }
    }
}

/// Frames between the picker's boxes: the nudge flow with idle frames
/// between the model box and the effort box (frames the loop's own settle
/// outlasts and reads as an idle point) types `/model` once: the restore's
/// claim on the picker stands until it has been gone [`PICKER_SETTLE`], the
/// effort box is answered as the restore's (`s`, this conversation), and the
/// session is held on its own model. Negative control: no frame between, the
/// same (`a_codex_nudge_near_its_limit_saves_the_work_restores_and_holds`,
/// these screens without the frames).
///
/// [`PICKER_SETTLE`]: crate::supervise::policy::turn_end::PICKER_SETTLE
#[test]
fn frames_between_the_pickers_boxes_type_one_model() {
    use crate::supervise::codex_usage::{CodexSeen, LimitRead};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let astra = "GPT-6-Astra medium";
    let luna = "GPT-6-Luna medium";
    let pursuing = Some("Pursuing goal (1h 2m)");
    let paused = Some("Goal paused (/goal resume)");
    let marker = crate::harness::upgrade::saved_marker(
        "-",
        "gpt-6-luna",
        u64::try_from(TEST_NOW).expect("after 1970"),
    );
    let mut saved = screen(cx::END_OF_TURN);
    let last = saved
        .iter()
        .rposition(|r| r.contains("as it evolves continuously."))
        .expect("the answer's last row");
    saved[last] = format!("  {marker}");
    for between in [2usize, 3] {
        let mut screens = vec![
            codex_footer(screen(cx::BUSY), astra, pursuing),
            nudge(),
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::BUSY), luna, pursuing),
            codex_footer(screen(cx::INTERRUPTED), luna, paused),
            codex_footer(screen(cx::BUSY), luna, paused),
            codex_footer(saved.clone(), luna, paused),
            screen(cx::MODEL_PICK),
        ];
        for _ in 0..between {
            screens.push(codex_footer(saved.clone(), luna, paused));
        }
        screens.extend([
            screen(cx::EFFORT_PICK),
            screen(cx::EFFORT_PICK),
            codex_footer(saved.clone(), astra, paused),
        ]);
        let mut m = Mock::new(true, screens);
        m.program = "codex";
        m.cursor_on_caret = true;
        m.gen_fence = true;
        m.sends_gen = true;
        m.turn_gates = vec![4, 6];
        m.vanish_after = Some(2);
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        let (dir, ledger) = ledger_at(&format!("between-{between}"));
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(60, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(CodexSeen {
                limits: LimitRead::Near {
                    used: 99,
                    back_at: Some(TEST_NOW + 3 * 86_400),
                },
                ..CodexSeen::default()
            });
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
        });
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{between}: {lines:#?}\n{:#?}", m.requests);
        assert_eq!(
            m.requests
                .iter()
                .filter(|r| r.starts_with("turn ") && r.ends_with(" /model"))
                .count(),
            1,
            "one `/model`: {why}"
        );
        assert!(
            m.requests
                .iter()
                .any(|r| r.starts_with("key ") && r.ends_with(" s")),
            "the effort, this conversation: {why}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("HOLDING ")),
            "held: {why}"
        );
    }
}

// --- the save's bound, intended presses and the policy's own holds ---------

/// Esc keys the loop sent.
fn escs(m: &Mock) -> usize {
    m.requests
        .iter()
        .filter(|r| r.starts_with("key ") && r.ends_with(" esc"))
        .count()
}

/// The save's own turn past its bound is stopped once, judged, and the model
/// put back (the cheaper model only commits and pushes): a switch carried on
/// winding, the save's turn running on gpt-6-luna with 16 minutes of busy
/// work behind it (the loop's span preset), gets one Esc guarded on its
/// status row, journaled as the save's; its interrupted point judges the
/// marker (none: a person is told, naming the stop) and `/model` is typed.
/// Negative control: five minutes into the save, no Esc goes; its end,
/// carrying the marker, is the save, and `/model` follows all the same.
#[test]
fn the_save_turn_past_its_bound_is_stopped_judged_and_restored() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let luna = "GPT-6-Luna medium";
    let paused = Some("Goal paused (/goal resume)");
    for (worked_min, stopped) in [(16u64, true), (5, false)] {
        let (dir, ledger) = ledger_at(&format!("wind-bound-{worked_min}"));
        seed_wind_at(&ledger, "winding", "", 60);
        let busy = codex_footer(screen(cx::BUSY), luna, paused);
        let end = if stopped {
            codex_footer(screen(cx::INTERRUPTED), luna, paused)
        } else {
            under_background(
                &["• Pushed.", "", "  ATERM-SAVED-3f9a1c2e"],
                luna,
                paused,
                false,
            )
        };
        let mut m = codex_mock(vec![busy.clone(), busy, end], 2);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
            s.running.busy(
                Instant::now()
                    .checked_sub(Duration::from_secs(worked_min * 60))
                    .expect("the clock"),
            );
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{worked_min}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        assert_eq!(escs(&m), usize::from(stopped), "{why}");
        assert!(
            m.requests
                .iter()
                .any(|r| r.starts_with("turn ") && r.ends_with(" /model")),
            "the model put back: {why}"
        );
        if stopped {
            assert!(
                m.requests
                    .iter()
                    .any(|r| r.starts_with("key ") && r.contains("esc.to.interrupt")),
                "guarded: {why}"
            );
            assert!(
                rows.lines()
                    .any(|l| l.contains("\"typed\"") && l.contains("\"command\":\"esc\"")),
                "the Esc ledgered: {why}"
            );
            assert!(
                rows.lines().any(|l| l.contains("did not confirm")
                    && l.contains("aterm stopped its turn after 15 min")
                    && l.contains("told=unsaved")),
                "said, on its row: {why}"
            );
            assert!(
                rows.contains("not saved: its turn stopped past 15 min"),
                "{why}"
            );
        } else {
            assert!(!rows.contains("did not confirm"), "{why}");
            assert!(rows.contains("\"command\":\"saved\""), "{why}");
        }
    }
}

/// An owed switch's press only intended, as the loop ledgers it before the
/// key ([`Session::nudge_intended`]) — or, `phase`, one said to have landed.
fn intent_ledger(ledger: &std::path::Path, phase: &str) {
    use crate::supervise::approvals::{Outcome, Row};
    let row = Row {
        rule_id: crate::supervise::policy::RULE_RATE_NUDGE_SWITCH,
        outcome: Outcome::Skipped,
        command: "Approaching rate limits => Switch to gpt-6-luna",
        reason: &format!(
            "pressing; the switch opens once the box leaves (model switch: kind=wind-down \
             from=GPT-6-Astra effort=medium to=gpt-6-luna back_at={} \
             marker=ATERM-SAVED-3f9a1c2e goal=- phase={phase})",
            TEST_NOW + 3 * 86_400
        ),
        box_seq: 90,
    }
    .to_json(TEST_NOW * 1000 - 5_000, None);
    std::fs::write(ledger, row + "\n").expect("the ledger");
}

/// A note said while the press is only intended keeps it intended: a press
/// carried on as only intended (`phase=intent`: the key may never have
/// gone), the session at a break under a background terminal whose footer
/// shows no model; the stood-still note (its bound shortened) rides its own
/// row, which says `phase=intent` and names the footer as what holds the
/// switch, never that the session is on gpt-6-luna. A loop restarted from
/// those rows, Codex's goal running on the thread's own model, sends no Esc
/// (the press may never have landed). Negative controls: the intent row
/// alone, no Esc either; the same rows said as landed (`phase=owed`), the
/// Esc goes.
#[test]
fn a_note_said_while_the_press_is_only_intended_keeps_it_intended() {
    use crate::supervise::approvals::open_wind_down;
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let (dir, ledger) = ledger_at("intent-note");
    intent_ledger(&ledger, "intent");
    assert_eq!(open_wind_down(&ledger, None).expect("open").phase, "intent");
    let mut scr = under_background(
        &["• The parser's second step is done."],
        "GPT-6-Astra medium",
        None,
        true,
    );
    let n = scr.len();
    scr[n - 1] = "  ? for shortcuts".to_string();
    let mut m = codex_mock(vec![scr.clone(), scr.clone(), scr], 3);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
        s.set_switch_break_note(Duration::ZERO);
    });
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let why = format!("{lines:#?}\n{:#?}\n{rows}", m.requests);
    let noted: Vec<&str> = rows
        .lines()
        .filter(|l| l.contains("\"skipped\"") && l.contains("told=background"))
        .collect();
    assert_eq!(noted.len(), 1, "said once, on its row: {why}");
    assert!(noted[0].contains("phase=intent"), "{why}");
    assert!(
        noted[0].contains("footer has not shown gpt-6-luna") && !noted[0].contains("stays on"),
        "{why}"
    );
    assert_eq!(
        open_wind_down(&ledger, None).expect("open").phase,
        "intent",
        "{why}"
    );
    // A restart from those rows, Codex's goal on the thread's own model.
    let busy = codex_footer(
        screen(cx::BUSY),
        "GPT-6-Astra medium",
        Some("Pursuing goal (1h 2m)"),
    );
    let escs_of = |ledger: &std::path::Path| {
        let mut again = codex_mock(vec![busy.clone(), busy.clone(), busy.clone()], 3);
        let (lines, _) = watch_lines_with(&mut again, &opts, |s| {
            s.set_approval_ledger(Some(ledger.to_path_buf()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
        });
        (escs(&again), format!("{lines:#?}\n{:#?}", again.requests))
    };
    let (after_note, why2) = escs_of(&ledger);
    assert_eq!(after_note, 0, "{why2}");
    // Controls: the intent row alone; the rows said as landed.
    let (dir2, alone) = ledger_at("intent-note-alone");
    intent_ledger(&alone, "intent");
    assert_eq!(escs_of(&alone).0, 0);
    let (dir3, landed) = ledger_at("intent-note-landed");
    intent_ledger(&landed, "owed");
    let (landed_escs, why3) = escs_of(&landed);
    assert_eq!(landed_escs, 1, "{why3}");
    for d in [dir, dir2, dir3] {
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// A press only intended, a draft at its point: the footer showing no model,
/// or gpt-6-luna, and a person's draft standing: nothing is typed over it, no
/// row claims the session is on gpt-6-luna to save the work, and a restarted
/// loop still reads the press as intended (a footer seen landing it writes no
/// row of its own: the next loop reads it off the footer again).
#[test]
fn an_intended_press_with_a_draft_stays_intended() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    for footer in ["none", "luna"] {
        let (dir, ledger) = ledger_at(&format!("intent-draft-{footer}"));
        intent_ledger(&ledger, "intent");
        let mut end = codex_footer(
            screen(cx::END_OF_TURN),
            "GPT-6-Luna medium",
            Some("Goal paused (/goal resume)"),
        );
        if footer == "none" {
            let at = end
                .iter()
                .rposition(|r| r.starts_with("  GPT-"))
                .expect("footer");
            end[at] = "  ~/pj · Fix the parser bugs · Main [default]".to_string();
        }
        let draft = "› wait, one more thing";
        let at = end
            .iter()
            .rposition(|r| r.starts_with("› Ask Codex"))
            .expect("composer");
        end[at] = draft.to_string();
        let mut m = codex_mock(vec![end.clone(), end], 2);
        m.screen_cols.insert(0, draft.chars().count());
        m.screen_cols.insert(1, draft.chars().count());
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(20, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let open = crate::supervise::approvals::open_wind_down(&ledger, None);
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{footer}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        assert_eq!(open.expect("still open").phase, "intent", "{why}");
        assert!(!rows.contains("gpt-6-luna to save the work"), "{why}");
        assert!(
            !m.requests.iter().any(|r| r.starts_with("turn ")),
            "nothing typed over the draft: {why}"
        );
    }
}

/// The owed footer gate reads the thread since the press: a switch carried on
/// as owed, pressed at `pressed=` and seen opening twenty seconds later at
/// `opened=`, its footer back on GPT-6-Astra at a point, and the thread's
/// rollout saying its last turn ran gpt-6-luna, begun between the press and
/// the opening (the goal turn under the box: ten seconds, or one, before the
/// opening) or in the press's own second (the rollout's time cut to the
/// second): a footer lagging it, so the save waits, the switch open. With no
/// `pressed=` (an older row), the opening floors it, a second's slack
/// included, and a turn begun after the opening is a footer lagging it too.
/// Negative control: a turn a minute before the press, or before an opening
/// with no press, is none of the switch's (the goal turn under the box, say):
/// released as the switch that did not land, nothing typed.
#[test]
fn the_owed_footer_gate_reads_the_thread_since_the_press() {
    use crate::supervise::codex_usage::CodexSeen;
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let opened = TEST_NOW - 10 * 60;
    let pressed = opened - 20;
    for (words, turn_at, released) in [
        (
            format!("opened={opened} pressed={pressed}"),
            opened - 10,
            false,
        ),
        (
            format!("opened={opened} pressed={pressed}"),
            opened - 1,
            false,
        ),
        (
            format!("opened={opened} pressed={pressed}"),
            pressed - 1,
            false,
        ),
        (
            format!("opened={opened} pressed={pressed}"),
            pressed - 60,
            true,
        ),
        (format!("opened={opened}"), opened + 60, false),
        (format!("opened={opened}"), opened - 1, false),
        (format!("opened={opened}"), opened - 60, true),
    ] {
        let (dir, ledger) = ledger_at(&format!("footer-press-{turn_at}-{}", words.len()));
        seed_wind_at(&ledger, "owed", &words, 5 * 60);
        let end = codex_footer(
            screen(cx::END_OF_TURN),
            "GPT-6-Astra ultra",
            Some("Goal paused (/goal resume)"),
        );
        let mut m = codex_mock(vec![end.clone(), end], 2);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(CodexSeen {
                thread_model: Some("gpt-6-luna".to_string()),
                thread_model_at: Some(turn_at),
                ..CodexSeen::default()
            });
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let open = crate::supervise::approvals::open_wind_down(&ledger, None);
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{words} {turn_at}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        assert_eq!(open.is_none(), released, "{why}");
        assert_eq!(rows.contains("did not land"), released, "{why}");
        if let Some(open) = open
            && words.contains("pressed=")
        {
            assert_eq!(open.pressed_unix, Some(pressed), "{why}");
        }
        assert!(
            !m.requests.iter().any(|r| r.starts_with("turn ")),
            "nothing typed: {why}"
        );
    }
}

/// The stood-still note at a break is only said: the save's turn ended under
/// a background terminal (on its marker and an offer, on a rejected push's
/// question, or plainly) and the note's bound at zero: the break is decided
/// once (the marker judged, `/model` typed once), and read again before the
/// wait that decision named, the switch standing still is said without
/// deciding the break again: no second `/model`. Control: the same screens
/// without the line type one `/model` at an ordinary point.
#[test]
fn a_stood_still_note_at_a_break_decides_nothing_again() {
    let paused = Some("Goal paused (/goal resume)");
    let marker = "  ATERM-SAVED-3f9a1c2e";
    let rejected = [
        "• The push was rejected: the remote has two new commits.",
        "",
        "  Should I merge them and push again?",
    ];
    let offer = [
        "• Committed and pushed.",
        marker,
        "",
        "  Want me to open a pull request for the branch as well?",
    ];
    let plain = ["• Committed and pushed.", "", marker];
    for (label, said) in [
        ("offer", &offer[..]),
        ("rejected", &rejected[..]),
        ("plain", &plain[..]),
    ] {
        for bg in [true, false] {
            let (dir, ledger) = ledger_at(&format!("still-once-{label}-{bg}"));
            seed_wind_at(&ledger, "winding", "", 60);
            let end = under_background(said, "GPT-6-Luna medium", paused, bg);
            let mut m = codex_mock(vec![end.clone(), end.clone(), end], 3);
            let opts = SuperviseOpts {
                policy: SupervisorConfig::default(),
                ..auto(30, None)
            };
            let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
                s.set_approval_ledger(Some(ledger.clone()));
                s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
                s.set_background_settle(Duration::ZERO);
                s.set_switch_break_note(Duration::ZERO);
            });
            let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
            let _ = std::fs::remove_dir_all(&dir);
            let why = format!("{label} {bg}: {lines:#?}\n{:#?}\n{rows}", m.requests);
            assert_eq!(
                m.requests
                    .iter()
                    .filter(|r| r.starts_with("turn ") && r.ends_with(" /model"))
                    .count(),
                1,
                "one `/model`: {why}"
            );
            assert!(
                !m.requests
                    .iter()
                    .any(|r| r.starts_with("turn ") && r.contains("keep going")),
                "{why}"
            );
        }
    }
}

/// The policy's own hold is named at a break, never blamed on the terminal
/// with `/ps` and `/stop`, which move nothing: an owed save at a break under
/// a background terminal whose footer shows no model (its bound shortened)
/// is said to wait on the footer; one whose thread fell into a sandbox, to be
/// held by the sandbox, with the resume that fixes it; neither with `/ps`,
/// nor with the save typed. Negative control: the footer on gpt-6-luna types
/// the save, and the break that then stands still names the terminal, with
/// `/ps` (`a_switch_standing_still_under_a_background_terminal_is_said`).
#[test]
fn the_policys_own_hold_is_named_at_a_break() {
    use crate::supervise::codex_usage::CodexSeen;
    for (label, footer, sandbox, held) in [
        ("footer", false, false, "footer has not shown gpt-6-luna"),
        ("sandbox", true, true, "fell into a sandbox"),
    ] {
        let (dir, ledger) = ledger_at(&format!("own-hold-{label}"));
        seed_wind_at(&ledger, "owed", "", 60);
        let mut scr = under_background(
            &["• The parser's second step is done."],
            "GPT-6-Luna medium",
            Some("Goal paused (/goal resume)"),
            true,
        );
        if !footer {
            let n = scr.len();
            scr[n - 1] = "  ? for shortcuts".to_string();
        }
        let mut m = codex_mock(vec![scr.clone(), scr.clone(), scr], 3);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(CodexSeen {
                sandbox_fell: sandbox.then(|| "workspace-write".to_string()),
                ..CodexSeen::default()
            });
            s.set_switch_break_note(Duration::ZERO);
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{label}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        let noted: Vec<&str> = rows
            .lines()
            .filter(|l| l.contains("\"skipped\"") && l.contains("told=background"))
            .collect();
        assert_eq!(noted.len(), 1, "said once: {why}");
        assert!(noted[0].contains(held), "{why}");
        assert!(!noted[0].contains("/ps"), "{why}");
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("turn ") && r.contains("saving your work")),
            "{why}"
        );
    }
}

/// The switch's opening closes the box's span itself: a person's hand
/// latched in the turn the nudge's box covered, then the switch opened by the
/// loop's own `open_wind`: the latch is spent and the span begun afresh, so
/// the goal turn Codex runs under the box is no person's, and a busy read of
/// it measures its own work. The keystroke itself (before the opening) is
/// floored. Negative control: the switch opened without the box close (the
/// policy's `open_switch` alone) keeps the latch, and the goal turn would be
/// spared.
#[test]
fn the_switchs_opening_closes_the_boxs_span() {
    use crate::supervise::policy::turn_end::{CodexSetting, WindDown};
    let wind = || {
        WindDown::opened(
            CodexSetting {
                model: "GPT-6-Astra".to_string(),
                effort: Some("ultra".to_string()),
            },
            "gpt-6-luna".to_string(),
            None,
            "ATERM-SAVED-3f9a1c2e".to_string(),
        )
    };
    for through_the_loop in [true, false] {
        let mut m = Mock::new(true, vec![rows(&["x"])]);
        let mut s = session(&mut m, None);
        let t = Instant::now()
            .checked_sub(Duration::from_secs(600))
            .expect("the clock");
        // The covered turn: a person's message began it, seen by a busy read.
        s.running.busy(t);
        s.person = Some(t + Duration::from_secs(1));
        assert!(
            s.person_in_this_turn(t + Duration::from_secs(60)),
            "their turn"
        );
        assert!(s.running.latched());
        if through_the_loop {
            s.open_wind(wind());
        } else {
            s.turn_end.open_switch(WindDown {
                opened_at: Some(Instant::now()),
                ..wind()
            });
        }
        assert_eq!(
            s.running.latched(),
            !through_the_loop,
            "the latch at the opening"
        );
        assert_eq!(
            s.running.since().is_none(),
            through_the_loop,
            "the span at the opening"
        );
        // The goal turn under the box, read busy after the opening.
        let now = Instant::now() + Duration::from_secs(5);
        s.running.busy(now);
        assert_eq!(
            s.person_in_this_turn(now),
            !through_the_loop,
            "the goal turn under the box"
        );
    }
}

// --- the switch beside the upgrade's goal hold ------------------------------

/// The save's bound stands under the upgrade's goal hold: the bound test
/// above (a switch carried on winding, 16 minutes of save work behind it) run
/// under the live upgrade's Paused hold on the tab's goal record. The
/// upgrade's hold keeps the loop's Esc off a goal's turn (the one its pause
/// lets finish), never off the switch's own save turn: one Esc, as without
/// the hold (`the_save_turn_past_its_bound_is_stopped_judged_and_restored`).
#[test]
fn the_saves_bound_stands_under_the_upgrades_goal_hold() {
    use crate::harness::goal_hold::{self, Hold, How, Owner, Stage};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let luna = "GPT-6-Luna medium";
    let paused = Some("Goal paused (/goal resume)");
    let (dir, ledger) = ledger_at("wind-bound-upgrade");
    seed_wind_at(&ledger, "winding", "", 60);
    goal_hold::write(
        &goal_hold::path_beside(&ledger),
        &Hold {
            stage: Stage::Paused,
            ..Hold::pausing(Owner::Upgrade, How::Typed, 42, "0.158.0", 1)
        },
    )
    .expect("record");
    let busy = codex_footer(screen(cx::BUSY), luna, paused);
    let end = codex_footer(screen(cx::INTERRUPTED), luna, paused);
    let mut m = codex_mock(vec![busy.clone(), busy, end], 2);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
        s.running.busy(
            Instant::now()
                .checked_sub(Duration::from_secs(16 * 60))
                .expect("the clock"),
        );
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(escs(&m), 1, "{lines:#?}\n{:#?}", m.requests);
}

/// The paused goal's box is answered only on the upgrade's own relaunch: the
/// box is pressed where the hold names the Codex it relaunched and that Codex
/// still leads its terminal, never a person's own `codex resume` of another
/// goal; its press is on the upgrade's record (`Resuming`, `moved`).
/// Negative controls, each alone: the relaunch no longer leading (the box is
/// another Codex's), and the box left to a person at the keys
/// (`BOX_THEIRS`): nothing pressed, the record as it was.
#[test]
fn only_the_upgrades_relaunch_has_its_goal_box_answered() {
    use crate::harness::goal_hold::{self, Hold, How, Owner, Stage};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    for (leads, theirs, pressed) in [
        (true, false, true),
        (false, false, false),
        (true, true, false),
    ] {
        let (dir, ledger) = ledger_at(&format!("goal-box-{leads}-{theirs}"));
        let record = goal_hold::path_beside(&ledger);
        let mut hold = Hold {
            stage: Stage::Paused,
            took_at: 10,
            relaunched: 5151,
            ..Hold::pausing(Owner::Upgrade, How::Typed, 42, "0.158.0", 1)
        };
        if theirs {
            hold.say(crate::harness::upgrade_codex::BOX_THEIRS);
        }
        goal_hold::write(&record, &hold).expect("record");
        // The box sits until a key reaches it; then the goal runs again.
        let resumed = codex_footer(
            screen(cx::END_OF_TURN),
            "GPT-6-Astra ultra",
            Some("Pursuing goal (1h 2m)"),
        );
        let mut m = codex_mock(vec![screen(cx::GOAL_RESUME), resumed], 2);
        m.key_releases = Some(0);
        // A host that fences a focus move on the screen generation.
        m.help = FENCED_HELP.to_string();
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                approve: crate::supervise::config::Approve::All,
                ..SupervisorConfig::default()
            },
            ..auto(20, None)
        };
        let lead: fn(u32) -> bool = if leads { |p| p == 5151 } else { |_| false };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
            s.codex_leads = lead;
        });
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let after = goal_hold::read(&record);
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("{leads}/{theirs}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        assert_eq!(
            rows.contains("goal-resume@v1\",\"decision\":\"approved\""),
            pressed,
            "{why}"
        );
        assert_eq!(
            after.map(|h| (h.stage, h.why)),
            Some(if pressed {
                (Stage::Resuming, "moved".to_string())
            } else {
                (Stage::Paused, String::new())
            }),
            "{why}"
        );
    }
}

/// A wall's bare clock
/// time is placed for its KIND and on the PRINT. `9am` read at 8 pm: a weekly
/// wall's is TOMORROW's (13 h off), a session window's cannot be (it resets
/// within five hours), so it is today's, passed. The print is what the screen
/// says of it — the done row's clock (`done 7:58 PM`), else, for a wall the
/// loop saw arrive, a minute ago — less six minutes of slack, because a wrong
/// "passed" is one rejected try and a wrong "tomorrow" a day's stall. A wall
/// FOUND sitting with no done row has no evidence: the session rule places it,
/// as before this change (it is right whenever the next occurrence is under
/// five hours off, and otherwise one early try).
#[test]
fn a_bare_clock_reset_is_placed_for_the_wall_it_belongs_to() {
    // TEST_NOW is 08:55 PDT on 2026-09-17.
    let at = |h: i64, m: i64| TEST_NOW + (h - 8) * 3600 + (m - 55) * 60;
    let none: Vec<String> = Vec::new();
    let done = |clock: &str| {
        let mut r = rows(&[
            "⏺ Done.",
            "",
            &format!("✻ Worked for 3m 2s · done {clock}"),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on"));
        r
    };
    let mut m = Mock::new(true, vec![busy_screen()]);
    // `window`: the notice names a session window (five hours at most).
    let minutes =
        |m: &mut Mock, now: (i64, i64), reset: &str, window: bool, live, rows: &[String]| {
            let mut s = session(m, None);
            s.set_clock(at(now.0, now.1), PDT);
            s.set_zone_lookup(test_zones);
            let n = s.now_unix();
            s.reset_unix_for(Some(reset), window, live, false, rows)
                .map(|u| (u - n) / 60)
        };
    let nine = "9am (America/Los_Angeles)";
    let four = "4pm (America/Los_Angeles)";
    // Read at 20:00. Seen ARRIVING: a weekly wall's is tomorrow's.
    assert_eq!(
        minutes(&mut m, (20, 0), nine, false, true, &none),
        Some(13 * 60)
    );
    // A spend or model wall is placed like a weekly one; a session window's
    // is today's, passed (the five-hour rule).
    assert_eq!(
        minutes(&mut m, (20, 0), nine, false, true, &none),
        Some(13 * 60)
    );
    assert_eq!(
        minutes(&mut m, (20, 0), nine, true, true, &none),
        Some(-11 * 60)
    );
    // FOUND SITTING, the done row saying `done 7:58 PM`: tomorrow's again.
    assert_eq!(
        minutes(&mut m, (20, 0), nine, false, false, &done("7:58 PM")),
        Some(13 * 60)
    );
    // FOUND SITTING with no done row: no evidence of the print — the session
    // rule, which says passed here (an early try; the reprint is placed on
    // what the loop sees), and says +2 h where the next `4pm` is near, never
    // the "passed" a last-occurrence rule would give.
    assert_eq!(
        minutes(&mut m, (20, 0), nine, false, false, &none),
        Some(-11 * 60)
    );
    assert_eq!(
        minutes(&mut m, (14, 0), four, false, false, &none),
        Some(2 * 60)
    );
    // A `4pm` that passed five minutes before the print is passed, never
    // tomorrow's — arriving, or sitting with or without a done row.
    for live in [false, true] {
        assert_eq!(
            minutes(&mut m, (16, 5), four, false, live, &none),
            Some(-5),
            "live={live}"
        );
    }
    assert_eq!(
        minutes(&mut m, (16, 5), four, false, false, &done("4:05 PM")),
        Some(-5)
    );
    // ... and one printed after the slack is tomorrow's, at once (a
    // ten-minute band would read a wall printed at 16:09 read as -9 min).
    assert_eq!(
        minutes(&mut m, (16, 9), four, false, true, &none),
        Some(24 * 60 - 9)
    );
    assert_eq!(
        minutes(&mut m, (16, 11), four, false, true, &none),
        Some(24 * 60 - 11)
    );
    // A span and a dated time are placed as before.
    assert_eq!(
        minutes(&mut m, (20, 0), "in 3h", false, true, &none),
        Some(180)
    );
    // A SESSION window found sitting after a night: `done 9:22 PM` yesterday, `resets 9:50pm` — the print
    // says it lifted ten hours ago and the continuation goes at once, where
    // the five-hour rule waited 13.8 hours for tonight's. No done row, no
    // evidence: the five-hour rule, as before.
    let session_nine_fifty = "9:50pm (America/Los_Angeles)";
    assert_eq!(
        minutes(
            &mut m,
            (8, 0),
            session_nine_fifty,
            true,
            false,
            &done("9:22 PM")
        ),
        Some(-(10 * 60 + 10))
    );
    assert_eq!(
        minutes(&mut m, (8, 0), session_nine_fifty, true, false, &none),
        Some(13 * 60 + 50)
    );
    // A session window whose clock is further off than its five hours from
    // the print contradicts the notice: the five-hour rule, as before.
    assert_eq!(
        minutes(&mut m, (20, 0), nine, true, true, &none),
        Some(-11 * 60)
    );
    // NEGATIVE CONTROL, the session rule for a weekly wall at 20:00: 11 h in
    // the past, the 26 messages in 13 hours of a replay of the policy.
    let mut s = session(&mut m, None);
    s.set_clock(at(20, 0), PDT);
    s.set_zone_lookup(test_zones);
    let now = s.now_unix();
    assert_eq!(
        s.reset_unix(Some(nine)).map(|u| (u - now) / 60),
        Some(-11 * 60)
    );
}

/// The owner's tab 1 through the LOOP, 2026-09-28 12:19:43 PDT: the MEASURED
/// screen (`…weekly-goal-paused-shell-measured.txt`) read by the real reader,
/// its `4pm` placed by the loop's clock after the print the done row says
/// (12:19 PM). The loop waits until 16:02:30 PDT (the reset and its 150 s of
/// grace) and types nothing before it.
#[test]
fn the_measured_weekly_wall_of_tab_1_is_waited_out_until_its_reset_and_grace_have_passed() {
    const WALL: &str = include_str!(
        "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    );
    let noon = TEST_NOW + 3 * 3600 + 24 * 60 + 43; // 12:19:43 PDT
    let (dir, path) = journal_file("te-tab1");
    let mut m = Mock::new(
        true,
        vec![busy_screen(), aterm_phase::prompt::fixtures::screen(WALL)],
    );
    m.vanish_after = Some(6);
    // The composer holds Claude's own dim suggestion, the cursor at its column 2.
    m.cell_attrs = "dim";
    m.cursor_on_caret = true;
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_clock(noon, PDT));
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    let waits: Vec<String> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.line.clone())
        .collect();
    // 16:02:30 PDT on 2026-09-17 is 23:02:30Z.
    assert!(
        waits
            .iter()
            .any(|l| l.contains("until=2026-09-17T23:02:30Z") && l.contains("usage-weekly resets")),
        "{waits:#?}\n{lines:#?}"
    );
    // A watcher with a budget (this one has 30 s; `drive supervise` 1800 s) is
    // stretched to ten minutes past the reset the wall names, once — it can
    // now SEE the wall, so it no longer times out unseen a few minutes in.
    let extends: Vec<&String> = lines.iter().filter(|l| l.starts_with("EXTEND ")).collect();
    assert_eq!(
        extends,
        ["EXTEND until=2026-09-17T23:10:00Z reset=4pm (America/Los_Angeles)"],
        "{lines:#?}"
    );
}

/// A wall's bare clock, placed IN THE LOOP: the gate of the
/// first fix. A WEEKLY wall hit at 8 pm that says `resets 9am` resets tomorrow
/// morning, 13 hours off; placed by the session window's five-hour rule it was
/// 11 hours in the PAST, and the loop continued at once and again every half
/// hour for 13 hours — 26 blind messages, each a turn and a synthetic error
/// row, and with Claude Code's own auto-continue armed the first one cancelled
/// it. Nothing is typed before 09:02:30 (the reset and its grace); a vendor-armed
/// wall is deferred to until 09:10; and a wall FOUND sitting with nothing on
/// the screen to say when it was printed is tried once (the session rule says
/// passed) — the wall answers, and the reprint is placed on what the loop
/// watched, tomorrow morning: one message, not 26.
#[test]
fn a_weekly_wall_hit_in_the_evening_that_resets_in_the_morning_is_waited_for() {
    let evening = TEST_NOW + 11 * 3600 + 5 * 60; // 20:00 PDT
    let wall = |done: bool, banner: Option<&str>| {
        let mut r = rows(&[
            "⏺ Running the suite.",
            "  ⎿  You've hit your weekly limit · resets 9am (America/Los_Angeles)",
            "",
            if done {
                "✻ Worked for 3m 2s · done 8:00 PM"
            } else {
                ""
            },
        ]);
        if let Some(b) = banner {
            r.push(format!("{}{b}", " ".repeat(90)));
        }
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let run = |tag: &str, screens: Vec<Vec<String>>, sits: bool| {
        let (dir, path) = journal_file(tag);
        let mut m = Mock::new(true, screens);
        if sits {
            m.turn_releases = Some(0);
        }
        m.vanish_after = Some(6);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..hosted(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| s.set_clock(evening, PDT));
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, records)
    };
    let waits = |records: &[crate::supervise::journal::JournalRecord]| {
        records
            .iter()
            .filter(|r| r.kind == "waiting")
            .map(|r| r.line.clone())
            .collect::<Vec<_>>()
    };
    // Seen arriving: waited for, until 09:02:30 PDT, the reset and its grace.
    let (m, lines, records) = run("te-evening", vec![busy_screen(), wall(true, None)], false);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    let w = waits(&records);
    assert!(
        w.iter()
            .any(|l| l.contains("until=2026-09-18T16:02:30Z") && l.contains("usage-weekly resets")),
        "{w:#?}"
    );
    // Armed by the vendor (`continuing automatically at 9am`): deferred to it
    // and its ten minutes, typed into by no one before.
    let armed = "⚠ Usage limit reached · continuing automatically at 9am · esc to cancel";
    let (m, lines, records) = run(
        "te-evening-armed",
        vec![busy_screen(), wall(true, Some(armed))],
        false,
    );
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}");
    let w = waits(&records);
    assert!(
        w.iter()
            .any(|l| l.contains("until=2026-09-18T16:10:00Z") && l.contains("goes on by itself")),
        "{w:#?}"
    );
    // Found sitting with no done row: tried once, then placed on the reprint.
    let (m, lines, records) = run(
        "te-evening-sitting",
        vec![wall(false, None), busy_screen(), wall(false, None)],
        true,
    );
    assert_eq!(
        count(&m, "turn"),
        1,
        "one try, not 26: {lines:#?}\n{:#?}",
        m.requests
    );
    let w = waits(&records);
    assert!(
        w.iter().any(|l| l.contains("until=2026-09-18T16:02:30Z")),
        "the reprint is placed for tomorrow morning: {w:#?}"
    );
    // NEGATIVE CONTROL: a SESSION window's `9am` (the notice says so) is the
    // five-hour rule's — today's, passed: continued at once.
    let mut r = wall(true, None);
    r[1] = "  ⎿  You've hit your session limit · resets 9am (America/Los_Angeles)".to_string();
    let (m, lines, _) = run("te-evening-session", vec![busy_screen(), r], false);
    assert_eq!(count(&m, "turn"), 1, "{lines:#?}");
}

/// 17:26 PDT, the minute the owner's tab met its weekly limit on 2026-10-01
/// (here on the watch tests' day, 2026-09-17).
const TAB_EVENING: i64 = TEST_NOW + 8 * 3600 + 31 * 60;

/// The owner's tab of 2026-10-01 17:26 PDT (Claude Code 2.1.287 at a weekly
/// limit whose reset is the next morning at 5am): the vendor's footer banner
/// under the composer, a bare clock and no window named (`⚠ Usage limit
/// reached · limit resets 5am`, the journal's `EVENT limited` word for word),
/// under the worker's last short answer and no done row. `typed`: the line
/// the loop typed into it, echoed and answered by the banner alone — Claude
/// Code ran no turn for it (no busy read); `queued`: a second line held under
/// it (`ctrl+enter to send now`, as the measured queued fixture draws it).
fn limit_resets_5am(typed: bool, queued: bool) -> Vec<String> {
    let mut r = rows(&["⏺ Still done, and I'm not starting new work.", ""]);
    if typed {
        r.extend(rows(&["❯ keep going", ""]));
    }
    if queued {
        r.extend(rows(&["❯ keep going", "  ctrl+enter to send now", ""]));
    }
    r.extend(composer("  ⚠ Usage limit reached · limit resets 5am"));
    r.push("  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string());
    r
}

/// A run of the loop over `screens` at `clock` (`sits`: the screen that sits
/// until a turn is typed), the act's take window cut to 50 ms so the point
/// after an act is judged inside the run: the requests, the lines and the
/// journal's `WAITING` summaries.
fn tab_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    sits: Option<usize>,
    clock: i64,
) -> (Mock, Vec<String>, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(true, screens);
    m.turn_releases = sits;
    m.vanish_after = Some(40);
    m.stall_sleep = Some(Duration::from_millis(5));
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_clock(clock, PDT);
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_millis(50),
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.clone())
        .collect();
    (m, lines, waits)
}

/// THE LIVE DEFECT OF 2026-10-01: the loop's hourly `keep going` met the
/// weekly limit, and Claude Code answered it with the banner alone — no turn
/// ran, so no read saw the worker busy. Read as a wall found SITTING (no
/// evidence of its print), `5am` at 17:26 was placed by the session window's
/// rule, 12 h 26 m in the PAST: `usage-resume@v1` was typed at once and then
/// every 30 minutes until the reset, 24 lines queued into one prompt. The wall
/// standing at the point after the loop's own act, with its take window run
/// out and no live turn read, ANSWERED that act: the limit is in force now,
/// so the bare clock is placed on that print — tomorrow 05:00 — and nothing
/// more is typed before it and its grace (12:02:30Z).
#[test]
fn a_weekly_wall_that_answers_the_loops_own_continuation_is_waited_out_to_its_reset() {
    let screens = vec![
        busy_screen(),
        ended("Still done, and I'm not starting new work."),
        limit_resets_5am(true, false),
    ];
    let (m, lines, waits) = tab_run("te-tab-answered", screens, Some(1), TAB_EVENING);
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        ["CONTINUED seq=102 rule=continue@v1 keep going"],
        "the continuation that met the wall, and nothing typed into it: {lines:#?}\n{waits:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        waits
            .iter()
            .any(|w| w.starts_with("until=2026-09-18T12:02:30Z ") && w.contains("resets")),
        "waited for tomorrow's 05:00 and its grace: {waits:#?}\n{lines:#?}"
    );
}

/// A wall that answered the loop's act is placed on that read even under a
/// done row hours old: the act drew no row of its own (Claude Code held the
/// line, no turn ran), and a long turn's `done 3:21 AM` still stands above
/// the composer. Read alone, that row dates the print 03:15 and puts `5am`
/// in the past at 17:26; answered, the print is the read's, and `5am` is
/// tomorrow's. NEGATIVE CONTROL: not answered, the done row stands as the
/// print, as it always has.
#[test]
fn a_wall_that_answered_an_act_is_not_dated_by_an_older_turns_done_row() {
    let mut r = rows(&[
        "⏺ Still done, and I'm not starting new work.",
        "",
        "✻ Worked for 2h 8m · done 3:21 AM",
        "",
    ]);
    r.extend(composer("  ⚠ Usage limit reached · limit resets 5am"));
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_clock(TAB_EVENING, PDT);
    s.set_zone_lookup(test_zones);
    let now = s.now_unix();
    let hours = |u: Option<i64>| u.map(|u| (u - now) / 3600);
    assert_eq!(
        hours(s.reset_unix_for(Some("5am"), false, false, true, &r)),
        Some(11),
        "tomorrow 05:00, 11 h 34 m off"
    );
    assert_eq!(
        hours(s.reset_unix_for(Some("5am"), false, false, false, &r)),
        Some(-12),
        "the old row's day: 12 h 26 m passed"
    );
}

/// The same wall FOUND sitting (a loop that starts at it, no done row: nothing
/// dates its print) is tried ONCE — the session rule says `5am` passed at
/// 17:26, and a wrong "passed" is meant to cost one try. Claude Code holds the
/// line under the banner (no turn): the wall standing after the try answered
/// it, so the reset is placed on that print and the next act waits for
/// tomorrow's 05:00 — never the 10- and 30-minute ladder of 24 blind lines.
#[test]
fn a_weekly_wall_still_standing_after_a_typed_line_is_never_typed_into_again_before_its_reset() {
    let screens = vec![
        limit_resets_5am(false, false),
        limit_resets_5am(true, false),
    ];
    let (m, lines, waits) = tab_run("te-tab-sitting", screens, Some(0), TAB_EVENING);
    assert_eq!(
        count(&m, "turn"),
        1,
        "one try, never another before the reset: {lines:#?}\n{waits:#?}\n{:#?}",
        m.requests
    );
    assert!(
        waits
            .iter()
            .any(|w| w.starts_with("until=2026-09-18T12:02:30Z ") && w.contains("resets")),
        "the try answered, the wait is tomorrow's 05:00 and its grace: {waits:#?}\n{lines:#?}"
    );
    assert!(
        !waits
            .iter()
            .any(|w| w.ends_with(" resets") && !w.starts_with("until=2026-09-18T12:02:30Z ")),
        "no rung of the ladder tonight (00:36Z, 17:36 PDT, before the fix): {waits:#?}"
    );
}

/// A weekly wall that names its DATE (`resets Sep 18 at 5am`) is waited out to
/// that reset and its grace, nothing typed before; read past it, the
/// continuation goes, once, and the same wall answering it (no turn ran) is
/// not typed into again in the run.
#[test]
fn a_dated_weekly_wall_is_continued_once_and_only_after_its_reset() {
    let dated = |typed: bool| {
        let mut r = rows(&[
            "⏺ Still done, and I'm not starting new work.",
            "  ⎿  You've hit your weekly limit · resets Sep 18 at 5am (America/Los_Angeles)",
            "",
        ]);
        if typed {
            r.extend(rows(&[
                "❯ keep going",
                "  ⎿  You've hit your weekly limit · resets Sep 18 at 5am (America/Los_Angeles)",
                "",
            ]));
        }
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let (m, lines, waits) = tab_run("te-tab-dated", vec![dated(false)], None, TAB_EVENING);
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{waits:#?}");
    assert!(
        waits
            .iter()
            .any(|w| w.starts_with("until=2026-09-18T12:02:30Z ") && w.contains("usage-weekly")),
        "{waits:#?}\n{lines:#?}"
    );
    // 05:03 PDT the next morning: past the reset and its grace.
    let morning = TEST_NOW + 20 * 3600 + 8 * 60;
    let (m, lines, waits) = tab_run(
        "te-tab-dated-after",
        vec![dated(false), dated(true)],
        Some(0),
        morning,
    );
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        ["CONTINUED seq=101 rule=usage-resume@v1 keep going"],
        "{lines:#?}\n{waits:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
}

/// NEGATIVE CONTROL: a SESSION limit whose reset has passed (`resets 3pm`, the
/// notice naming its five-hour window, read at 17:26) is continued at once.
/// What a wall answering an act says is never "wait for tomorrow" for a
/// session window's clock, and nothing is held back that has lifted.
#[test]
fn a_session_limit_whose_reset_has_passed_is_continued() {
    let session_wall = |typed: bool| {
        let mut r = rows(&[
            "⏺ Suites are running; I'll report the number when it lands.",
            "  ⎿  You've hit your session limit · resets 3pm (America/Los_Angeles)",
            "",
        ]);
        if typed {
            r.extend(rows(&[
                "❯ keep going",
                "  ⎿  You've hit your session limit · resets 3pm (America/Los_Angeles)",
                "",
            ]));
        }
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let (m, lines, waits) = tab_run(
        "te-tab-session-passed",
        vec![session_wall(false), session_wall(true)],
        Some(0),
        TAB_EVENING,
    );
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        ["CONTINUED seq=101 rule=usage-resume@v1 keep going"],
        "{lines:#?}\n{waits:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    // Answered by the wall again, the session window keeps its five-hour
    // rule: the ladder's first rung (17:36 PDT), never tomorrow's 3pm (22:00Z).
    assert!(
        waits
            .iter()
            .any(|w| w == "until=2026-09-18T00:36:00Z usage-session resets"),
        "{waits:#?}"
    );
    assert!(
        !waits.iter().any(|w| w.starts_with("until=2026-09-18T22:")),
        "a session window's passed clock is never moved to tomorrow: {waits:#?}"
    );
}

/// The composer is read where it
/// is DRAWN. A window that re-gridded twice in a millisecond leaves an
/// alternate-screen program's rows one higher than it drew them and its cursor
/// on the row BELOW its prompt (tab 1 sat so at 17:30, until Claude's next full
/// frame): read by the cursor's row, Claude's own dim suggestion was a person's
/// draft and the wall's act was a bare Enter on ghost text, not `keep going`.
/// The cursor's column says where it can (2, after the caret: the suggestion
/// or a draft whose caret went home; anywhere else: inside a draft), and at
/// column 2 the caret row's `cell` decides. NEGATIVE CONTROLS: a draft
/// (`cell` not dim) on the same displaced grid is submitted as it stands, never
/// typed over, and a cursor inside the text is a draft without asking.
#[test]
fn a_suggestion_on_a_grid_drawn_one_row_high_is_not_a_draft_at_a_wall() {
    let suggesting = || {
        let mut r = weekly_limited();
        let caret = r.iter().position(|row| row == "❯").expect("caret row");
        r[caret] = "❯ keep going, then merge when ready".to_string();
        (r, caret)
    };
    let run = |cursor_row_from_caret: Option<usize>,
               cursor_col: Option<usize>,
               attrs: &'static str| {
        let (screen, caret) = suggesting();
        let mut m = Mock::new(true, vec![busy_screen(), screen]);
        m.vanish_after = Some(3);
        m.cell_attrs = attrs;
        m.cursor_col = cursor_col;
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        match cursor_row_from_caret {
            Some(k) => m.cursor_row = Some(caret + k),
            None => m.cursor_on_caret = true,
        }
        // The screen has stood for longer than any grace: nobody is typing.
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..SupervisorConfig::default()
            },
            ..resuming(&auto(30, None), None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(AFTER_RESET, PDT);
            s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
        });
        let enters = m.presses().iter().filter(|p| p.ends_with(" enter")).count();
        (count(&m, "turn"), enters, lines, m.requests)
    };
    // Aligned, and one row high: the suggestion, so `keep going` is typed and
    // no Enter is pressed on ghost text.
    for k in [None, Some(1)] {
        let (turns, enters, lines, requests) = run(k, None, "dim");
        assert_eq!(
            (turns, enters),
            (1, 0),
            "cursor row {k:?}: {lines:#?}\n{requests:#?}"
        );
    }
    // NEGATIVE CONTROLS. A draft on the displaced grid (the cell is not dim):
    // nothing is typed over it; it is submitted as it stands.
    let (turns, enters, lines, requests) = run(Some(1), None, "none");
    assert_eq!((turns, enters), (0, 1), "{lines:#?}\n{requests:#?}");
    // The cursor inside the text (column 30, whatever the cell says): a draft.
    for k in [None, Some(1)] {
        let (turns, enters, lines, requests) = run(k, Some(30), "dim");
        assert_eq!(
            (turns, enters),
            (0, 1),
            "cursor row {k:?}: {lines:#?}\n{requests:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The wall the loop waits
// on is REMEMBERED while its rows are out of view.
// ---------------------------------------------------------------------------

/// The owner's measured wall (tab 1, 12:19) with a reset a span away, so that a
/// test can cross it: `resets in 2s`. The done row still counts `1 shell still
/// running` and the footer `· 1 shell ·`.
fn memory_wall() -> Vec<String> {
    memory_wall_in("2s")
}

/// [`memory_wall`] with the reset `span` away.
fn memory_wall_in(span: &str) -> Vec<String> {
    const WALL: &str = include_str!(
        "../../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    );
    aterm_phase::prompt::fixtures::screen(WALL)
        .into_iter()
        .map(|r| {
            r.replace(
                "resets 4pm (America/Los_Angeles)",
                &format!("resets in {span}"),
            )
        })
        .collect()
}

/// A TURN SERVED UNDER THE REMEMBERED WALL'S OWN NOTICE IS NO BACKGROUND
/// WORK (the review of 2026-10-14: untested). The loop remembers the weekly
/// wall of tab 1 (its shell still running); a screen that shows only the
/// agent's background work stands for that wall — but not one where a
/// person's turn, its prompt and the model's reply, was served under the
/// wall's notice: the limit lifted, and the turn is work since it. NEGATIVE
/// CONTROLS, each still the remembered wall: the screen with no such turn,
/// and the same turn under ANOTHER limit's notice (its reset not the
/// remembered wall's).
#[test]
fn a_turn_served_under_the_remembered_walls_notice_is_no_background_work() {
    let walled = memory_wall_in("2s")
        .into_iter()
        .map(|r| r.replace("resets in 2s", "resets 4pm (America/Los_Angeles)"))
        .collect::<Vec<_>>();
    let with_turn = |rows: &[String]| {
        let mut rows = rows.to_vec();
        let at = rows
            .iter()
            .position(|r| r.starts_with("✻ Sautéed"))
            .expect("the done row");
        for (i, row) in [
            "❯ what is left to do?",
            "",
            "⏺ Nothing is left: the suite is green.",
            "",
        ]
        .iter()
        .enumerate()
        {
            rows.insert(at + i, (*row).to_string());
        }
        rows
    };
    let turned = with_turn(&walled);
    let elsewhere: Vec<String> = turned.iter().map(|r| r.replace("4pm", "9pm")).collect();
    for rows in [&walled, &turned, &elsewhere] {
        assert!(aterm_phase::background_only(rows), "{rows:#?}");
    }
    let mut m = Mock::new(true, vec![walled.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let reader = s.reader(&walled);
    let mut reading = reader.read(&walled, Some(2));
    assert!(reading.wall.is_some(), "{reading:#?}");
    s.wall_memory_step(reader, &walled, &mut reading, 101);
    assert!(s.remembered_wall(reader, &turned).is_none());
    // NEGATIVE CONTROLS.
    assert!(s.remembered_wall(reader, &walled).is_some());
    assert!(s.remembered_wall(reader, &elsewhere).is_some());
}

/// The same screen with the wall's rows gone: the owner scrolled back up the
/// transcript (Claude Code's fullscreen view keeps the status row, the
/// composer and the footer where they are).
fn memory_scrolled() -> Vec<String> {
    memory_scrolled_in("2s")
}

/// [`memory_scrolled`] for a wall whose reset was `span` away.
fn memory_scrolled_in(span: &str) -> Vec<String> {
    let mut rows = memory_wall_in(span);
    let from = rows
        .iter()
        .position(|r| r.starts_with("⏺ Dynamic workflow"))
        .unwrap();
    let to = rows
        .iter()
        .position(|r| r.starts_with("⏺ Goal paused"))
        .unwrap();
    rows.drain(from..=to);
    rows
}

/// The wall with the agent's spinner drawn under it: a live turn.
fn memory_with_a_live_turn() -> Vec<String> {
    memory_with_a_live_turn_in("2s")
}

/// [`memory_with_a_live_turn`] for a wall whose reset is `span` away.
fn memory_with_a_live_turn_in(span: &str) -> Vec<String> {
    let mut rows = memory_wall_in(span);
    let foot = rows
        .iter()
        .position(|r| r.contains("bypass permissions"))
        .unwrap();
    rows[foot] = "  ⏵⏵ bypass permissions on · esc to interrupt · 1 shell".to_string();
    rows
}

/// The stop of a real-clock run that must type: a hang detector, never a
/// latency budget.
const HANG_S: u64 = 60;

/// Runs the loop over `screens` (the sat one last, released by the first
/// `turn`), the reset's grace one second, stopped `stop_s` seconds in, and
/// hands back the mock, the printed lines and the journal's waiting lines. A
/// run that types ends at its turn (the scripted server's end), so for one
/// the stop is a hang detector ([`HANG_S`]).
fn memory_run_for(
    tag: &str,
    screens: Vec<Vec<String>>,
    stop_s: u64,
    sid: Option<&str>,
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (Mock, Vec<String>, Vec<String>, PathBuf) {
    memory_run_on(tag, screens, stop_s, sid, |_| {}, setup)
}

/// [`memory_run_for`] on a server `server` makes more of (a pushed verdict).
fn memory_run_on(
    tag: &str,
    screens: Vec<Vec<String>>,
    stop_s: u64,
    sid: Option<&str>,
    server: impl FnOnce(&mut Mock),
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (Mock, Vec<String>, Vec<String>, PathBuf) {
    let sits = screens.len() - 1;
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(true, screens);
    m.turn_gates = vec![sits];
    m.cell_attrs = "dim";
    m.cursor_on_caret = true;
    m.dead_after_turns = Some(0);
    // The sat screen's waits sleep (a quarter second each).
    m.stall_sleep = Some(Duration::from_millis(250));
    server(&mut m);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(HANG_S)
    };
    let mut out: Vec<u8> = Vec::new();
    // A run that types nothing (the failure, or a control) ends by this stop,
    // not at the budget the wall's reset stretches it to.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(stop_s));
            stop.store(true, Ordering::SeqCst);
        });
    }
    {
        // The machine's own clock: a span counts from its print in real time.
        let mut s = session(&mut m, sid.map(str::to_string));
        s.stop = Some(stop);
        s.set_zone_lookup(test_zones);
        s.set_turn_end_timing(TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        setup(&mut s);
        let _ = s.watch(&opts, &mut out);
    }
    let lines: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    let (records, _) = journal_records(&path);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.line.clone())
        .collect();
    (m, lines, waits, dir)
}

/// THE UPDATE THAT APPLIES WHILE THE WALL IS SCROLLED OUT OF VIEW. The loop
/// that waited on the wall wrote it down (a `wall-wait@v1` ledger row: the
/// notice and the reset as it PLACED it); a loop that starts after it — a GUI
/// replacement, a fault — finds a screen with none of its rows and goes on
/// waiting for the same reset: it types `keep going` once, at the reset and
/// its grace, and not before. NEGATIVE CONTROLS (the `a_hidden_wall_…` tests
/// below): a ledger whose wall was ended (the worker worked since, the screen
/// left it) and one whose wall was acted on since seed nothing — the loop that
/// starts on the hidden screen types nothing; and a wall long past its horizon
/// is history. A control runs to its 6 s stop (a wrongly seeded act
/// comes at the reset and its grace, ~4 s in); the positive one ends at its
/// turn, its stop a hang detector.
mod hidden_wall {
    use super::*;
    use crate::supervise::approvals::{self, Outcome, Row};

    /// Append one approval-ledger row, `ts_back_ms` before now.
    pub(super) fn row_at(
        ledger: &Path,
        rule: &str,
        outcome: Outcome,
        reason: &str,
        ts_back_ms: i64,
    ) {
        let row = Row {
            rule_id: rule,
            outcome,
            command: "",
            reason,
            box_seq: 102,
        };
        let line = row.to_json(
            crate::supervise::journal::unix_ms() - ts_back_ms,
            Some("@s-1"),
        );
        let mut body = std::fs::read_to_string(ledger).unwrap_or_default();
        body.push_str(&line);
        body.push('\n');
        std::fs::write(ledger, body).expect("ledger");
    }

    /// The wall's notice, as the predecessor wrote it down.
    pub(super) fn notice() -> String {
        memory_wall_in("3s")
            .iter()
            .find(|r| r.contains("You've hit your weekly limit"))
            .expect("the notice row")
            .trim()
            .trim_start_matches('⎿')
            .trim()
            .replace('\u{a0}', " ")
    }

    /// The predecessor's row: it waits on a reset at `reset_unix`.
    pub(super) fn waits_row(ledger: &Path, reset_unix: i64) {
        row_at(
            ledger,
            approvals::RULE_WALL_WAIT,
            Outcome::Waiting,
            &approvals::wall_wait_reason(Some(reset_unix), Some("in 3s"), &notice()),
            500,
        );
    }

    /// The successor's run on the SCROLLED screen over a ledger `prepare`
    /// wrote, stopped `stop_s` seconds in: what it asked, its lines, how long
    /// it ran, the ledger after.
    pub(super) fn start(
        tag: &str,
        stop_s: u64,
        prepare: &dyn Fn(&Path),
    ) -> (Mock, Vec<String>, Duration, String) {
        let dir = crate::supervise::test_scratch_path("wm-ledger", tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let ledger = dir.join("s-1.jsonl");
        prepare(&ledger);
        let started = Instant::now();
        // The loop starts on the SCROLLED screen: no wall in view at all.
        let (m, lines, _, jdir) = memory_run_for(
            tag,
            vec![memory_scrolled_in("3s")],
            stop_s,
            Some("@s-1"),
            |s| s.set_approval_ledger(Some(ledger.clone())),
        );
        let took = started.elapsed();
        let _ = std::fs::remove_dir_all(&jdir);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, took, rows)
    }

    /// `reset_unix` is `unix_now() + 3`, a whole second; the grace is one more.
    pub(super) fn reset_soon() -> i64 {
        crate::supervise::limit::unix_now() + 3
    }
}

#[test]
fn a_loop_that_starts_on_a_hidden_wall_its_predecessor_waited_on_waits_on() {
    // The predecessor's row: the successor waits, then types once.
    let reset_unix = hidden_wall::reset_soon();
    let (m, lines, took, rows) = hidden_wall::start("wm-seed", HANG_S, &|l| {
        hidden_wall::waits_row(l, reset_unix)
    });
    assert_eq!(m.turns, 1, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        took >= Duration::from_secs(2),
        "not before the reset and its grace: {took:?}"
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains(" turn submit=guarded:") && r.ends_with(" keep going")),
        "the act is the wall's continuation: {:#?}\n{rows}",
        m.requests
    );
}

/// CONTROL: the wall was ended after it was written.
#[test]
fn a_hidden_wall_ended_since_it_was_written_seeds_nothing() {
    let reset_unix = hidden_wall::reset_soon();
    let (m, lines, _, _) = hidden_wall::start("wm-ended", 6, &|l| {
        hidden_wall::waits_row(l, reset_unix);
        hidden_wall::row_at(
            l,
            crate::supervise::approvals::RULE_WALL_WAIT_END,
            crate::supervise::approvals::Outcome::Waiting,
            "the worker worked",
            400,
        );
    });
    assert_eq!(m.turns, 0, "{lines:#?}\n{:#?}", m.requests);
}

/// CONTROL: an act was typed after the wall was written.
#[test]
fn a_hidden_wall_acted_on_since_it_was_written_seeds_nothing() {
    let reset_unix = hidden_wall::reset_soon();
    let (m, lines, _, _) = hidden_wall::start("wm-acted", 6, &|l| {
        hidden_wall::waits_row(l, reset_unix);
        hidden_wall::row_at(
            l,
            "continue@v1",
            crate::supervise::approvals::Outcome::Typed,
            "keep going",
            300,
        );
    });
    assert_eq!(m.turns, 0, "{lines:#?}\n{:#?}", m.requests);
}

/// CONTROL: a hidden wall whose reset was two days ago is history.
#[test]
fn a_hidden_wall_past_its_horizon_is_history() {
    let reset_unix = hidden_wall::reset_soon();
    let (m, lines, _, _) = hidden_wall::start("wm-stale", 6, &|l| {
        hidden_wall::row_at(
            l,
            crate::supervise::approvals::RULE_WALL_WAIT,
            crate::supervise::approvals::Outcome::Waiting,
            &crate::supervise::approvals::wall_wait_reason(
                Some(reset_unix - 2 * 86_400),
                Some("in 3s"),
                &hidden_wall::notice(),
            ),
            2 * 86_400_000,
        );
    });
    assert_eq!(m.turns, 0, "{lines:#?}\n{:#?}", m.requests);
}

/// Claude Code 2.1.296's five-hour limit with its own auto-continue armed
/// (the lab's capture), its reset `span` away: the loop places it on the
/// print, so a test can cross it.
fn auto_continue_wall_in(span: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
    )
    .into_iter()
    .map(|r| {
        r.replace(
            "resets 3:15pm (America/Los_Angeles)",
            &format!("resets in {span}"),
        )
        .replace("limit resets 3:15pm", &format!("limit resets in {span}"))
    })
    .collect()
}

/// The same kind of limit after Esc on the countdown (the lab's capture):
/// `⏺ Automatic continue cancelled · /rate-limit-options to re-arm`, an idle
/// screen with no wall.
fn auto_continue_cancelled() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED,
    )
}

/// A LIMIT WHOSE NOTICE LEFT IS WAITED ON ACROSS A LOOP'S RESTART (the review
/// of 2026-10-14). A loop reads the limit, then Esc on the vendor's countdown
/// leaves an idle screen with no wall, and the policy waits for the reset the
/// notice named. A wait can last days, and the harness restarts its loops (each
/// seamless update, each fault): the ledger keeps the wall open — no
/// `wall-wait-end@v1` while the policy waits on it — and the loop that follows
/// takes the named reset up from it. Measured before: the first loop wrote the
/// wall's end at the idle screen and its successor seeded nothing, so it typed
/// `continue@v1 keep going` into the standing limit after its first back-off
/// (with `continue_policy = false`, nothing ever, and nobody told). Here the
/// successor's back-off is a second, the reset seven seconds out: it waits by
/// the wall's own rule (`usage-session resets`) for the reset and its grace,
/// then types `keep going` once.
#[test]
fn a_limit_whose_notice_left_is_waited_on_across_a_loop_restart() {
    let dir = crate::supervise::test_scratch_path("wm-ledger", "left-restart");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let ledger = dir.join("s-1.jsonl");
    let reset = Instant::now() + Duration::from_secs(7);
    // The loop that reads the limit and then the cancel, stopped there.
    let (m, lines, _, jdir) = memory_run_for(
        "left-restart-1",
        vec![
            busy_screen(),
            auto_continue_wall_in("7s"),
            auto_continue_cancelled(),
        ],
        2,
        Some("@s-1"),
        |s| s.set_approval_ledger(Some(ledger.clone())),
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert_eq!(m.turns, 0, "{lines:#?}\n{rows}");
    assert!(
        crate::supervise::approvals::open_wall(&ledger, Some("@s-1")).is_some(),
        "the wall is open for a loop that follows: {rows}"
    );
    // The loop that follows, on the cancelled screen.
    let (m, lines, waits, jdir) = memory_run_for(
        "left-restart-2",
        vec![auto_continue_cancelled()],
        HANG_S,
        Some("@s-1"),
        |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_turn_end_timing(TurnEndTiming {
                reset_grace: Duration::from_secs(1),
                reset_jitter: Duration::ZERO,
                short_backoff: Duration::from_secs(1),
                ..TurnEndTiming::default()
            });
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&dir);
    // One turn, the session gone at it (the scripted server's end), and no
    // sooner than the reset: the wait was the wall's, never the continue
    // policy's back-off.
    assert_eq!(m.turns, 1, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(Instant::now() >= reset, "not before the reset: {waits:#?}");
    assert!(
        !waits.is_empty() && waits.iter().all(|w| w.ends_with("usage-session resets")),
        "{waits:#?}"
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains(" turn submit=guarded:") && r.ends_with(" keep going")),
        "{:#?}",
        m.requests
    );
}

/// The measured armed wall ([`auto_continue_wall_in`]'s) naming the bare
/// clock `clock` for its reset and its countdown.
fn auto_continue_wall_at(clock: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED,
    )
    .into_iter()
    .map(|r| r.replace("3:15pm", clock))
    .collect()
}

/// The same kind of limit after Esc on the countdown, then — the limit
/// lifted before its reset — a person's short turn under the cancel (Claude
/// Code 2.1.299, the lab's capture): the limit's notice names `10:38am`.
fn cancelled_then_a_persons_turn() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED,
    )
}

/// THE WAIT ON A LIMIT WHOSE NOTICE LEFT WAKES ON THE SCREEN, NOT ON THE
/// SERVER'S VERDICT (the review of 2026-10-14; the lab, Claude Code 2.1.299).
/// At an idle point the loop waits for the server's pushed agent verdict to
/// move off `idle` (`await agent busy,prompt,…`): one wake per real turn,
/// none per footer tick. The verdict is a sweep's, and a turn shorter than
/// the sweep is never published busy — a person's turn the stub answers at
/// once is not. At an ordinary point the policy's own deadline (a back-off
/// of minutes) reads the screen again soon; at a limit whose notice left the
/// screen it is the reset — hours, days at a weekly limit. Measured live
/// before: the person's turn at 10:28:46, no point decided, then
/// `usage-resume@v1 keep going` at the reset, 10:40:32. That wait is woken by
/// the content, as a wall that shows is: the person's turn is read as the
/// point it is, and decided (a turn since the limit: the continue policy's
/// back-off, then its `keep going`, the session ending there). The
/// script's verdict passes the person's idle screen over unread, as the
/// server's sweep did.
#[test]
fn the_wait_on_a_left_limit_wakes_on_a_persons_short_turn() {
    // The person's turn stands under THIS limit's notice (`10:38am`, an
    // hour and 43 minutes past the watch tests' 08:55 PDT).
    let (m, lines, waits, jdir) = memory_run_on(
        "left-person-turn",
        vec![
            busy_screen(),
            auto_continue_wall_at("10:38am"),
            auto_continue_cancelled(),
            cancelled_then_a_persons_turn(),
        ],
        HANG_S,
        Some("@s-1"),
        |m| {
            m.sends_gen = true;
            m.agent_pushes = true;
        },
        |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_turn_end_timing(TurnEndTiming {
                reset_grace: Duration::from_secs(1),
                reset_jitter: Duration::ZERO,
                short_backoff: Duration::from_secs(1),
                ..TurnEndTiming::default()
            });
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains(" await agent busy,prompt")),
        "the idle points are waited on by the verdict: {:#?}",
        m.requests
    );
    assert!(
        waits.iter().any(|w| w.contains("usage-session resets")),
        "the left limit's wait: {waits:#?}"
    );
    // The person's turn read as its point, and decided by the continue
    // policy (a turn since the limit: a short turn's back-off), whose
    // `keep going` is then written — at the back-off, not the reset an hour
    // off.
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("EVENT idle seq=") && l.contains("(reply 3)")),
        "the person's turn is a point: {lines:#?}"
    );
    assert!(
        waits.last().is_some_and(|w| w.contains("short turn")),
        "the continue policy's back-off: {waits:#?}"
    );
    assert_eq!(m.turns, 1, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains(" turn ") && r.ends_with(" keep going")),
        "{:#?}",
        m.requests
    );
}

/// Claude Code 2.1.300 after a `/clear`: conversation B at its own five-hour
/// limit (`resets 9:19am`, 24 minutes past the watch tests' 08:55 PDT), its
/// countdown armed (the lab's capture).
fn armed_conversation_b() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SESSION_ARMED_CONVERSATION_B_2_1_300,
    )
}

/// A `/resume` of conversation A at [`armed_conversation_b`]'s limit (the
/// lab's capture, 2.1.300): A's history redrawn — its own older limit
/// (`resets 8:48am`) and the vendor's continuation of it — B's countdown
/// dropped.
fn resumed_older_limit_continued() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::RESUMED_OLDER_LIMIT_CONTINUED_AT_ARMED_LIMIT_2_1_300,
    )
}

/// A `/clear` at an armed five-hour limit (the lab's capture, 2.1.300): the
/// launch card and `❯ /clear`, nothing else.
fn cleared_at_an_armed_limit() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::CLEARED_AT_ARMED_LIMIT_2_1_300,
    )
}

/// A `/RESUME` AT AN ARMED LIMIT TYPES NOTHING INTO IT (W1 of
/// ATERM-KI-1015-1, the lab on Claude Code 2.1.300, 2026-10-15: `lp20w1b`).
/// Conversation B's five-hour limit stands, its countdown armed; a person
/// `/resume`s conversation A, whose history is redrawn: A's own OLDER limit
/// and the vendor's continuation of it (`⏺ Usage limit reset · continuing
/// automatically`), no `❯` row under it. Measured on the base: that word read
/// as B's continuation ended the wait (`wall-wait-end@v1`, `3 short turn(s) in
/// a row: 8 min back-off`) and `continue@v1 keep going` met B's standing
/// limit (the stub's 429, 51 minutes before its reset). Here the back-off is a
/// second: nothing is typed, and the wait is B's limit's.
#[test]
fn a_resume_at_an_armed_limit_types_nothing_into_it() {
    let (m, lines, waits, jdir) = memory_run_for(
        "w1-resume-armed",
        vec![
            busy_screen(),
            armed_conversation_b(),
            resumed_older_limit_continued(),
        ],
        8,
        Some("@s-1"),
        |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_turn_end_timing(TurnEndTiming {
                reset_grace: Duration::from_secs(1),
                reset_jitter: Duration::ZERO,
                short_backoff: Duration::from_secs(1),
                ..TurnEndTiming::default()
            });
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert_eq!(
        m.turns, 0,
        "nothing typed into the standing limit: {lines:#?}\n{waits:#?}\n{:#?}",
        m.requests
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.contains("until=2026-09-17T16:") && w.contains("usage-session")),
        "B's limit's own wait: {waits:#?}"
    );
}

/// A `/RESUME` AT AN ARMED LIMIT DROPS THE VENDOR'S COUNTDOWN, AND THE LIMIT
/// IS WAITED ON AS A CANCELLED ONE (W4 of ATERM-KI-1015-1, the lab on Claude
/// Code 2.1.300, 2026-10-15: `lp20w4rb`). Conversation B's limit armed (`goes
/// on by itself`); a person `/resume`s conversation A, whose history holds no
/// limit: Claude Code drops B's countdown without a word — the composer, no
/// notice, no countdown. Measured on the base: `WAITING … usage-session goes
/// on by itself` to the reset plus 10 minutes, where nobody goes on by
/// itself. Placed as a cancelled one: the reset (9:19am) plus the grace (a
/// second here), `usage-session resets`.
#[test]
fn a_resume_at_an_armed_limit_waits_on_it_as_a_cancelled_one() {
    let resumed = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
    );
    let (m, lines, waits, jdir) = memory_run_for(
        "w4-resume-armed",
        vec![busy_screen(), armed_conversation_b(), resumed],
        5,
        Some("@s-1"),
        |s| s.set_clock(TEST_NOW, PDT),
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        waits.last().is_some_and(
            |w| w.contains("until=2026-09-17T16:19:01Z") && w.ends_with("usage-session resets")
        ),
        "the reset and its grace, as a cancelled countdown: {waits:#?}"
    );
}

/// A `/RESUME` AT AN ARMED MODEL BUCKET TYPES NOTHING INTO IT (the review of
/// ATERM-KI-1015-1's W3 fix, 2026-10-15). The lab's Sonnet bucket on Claude
/// Code 2.1.300 (`resets Oct 17 at 4:20pm`, a month past the watch tests'
/// clock), its countdown armed under the generic banner, read as the bucket
/// it is; no host here relaunches it on the fallback. Then the lab's
/// `/resume` screen: the countdown dropped, no notice. Measured before: the
/// bucket's track went at once where its notice left, and `usage-resume@v1
/// keep going` was typed into the bucket a month before its reset. The wait
/// is the bucket's own: its reset, placed as a cancelled countdown (the
/// grace, a second here), `model-bucket resets`.
#[test]
fn a_resume_at_an_armed_bucket_types_nothing_into_it() {
    let armed = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
    );
    let resumed = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
    );
    let (m, lines, waits, jdir) = memory_run_for(
        "bucket-resume-armed",
        vec![busy_screen(), armed, resumed],
        5,
        Some("@s-1"),
        |s| s.set_clock(TEST_NOW, PDT),
    );
    let _ = std::fs::remove_dir_all(&jdir);
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        waits.last().is_some_and(
            |w| w.contains("until=2026-10-17T23:20:01Z") && w.ends_with("model-bucket resets")
        ),
        "the bucket's reset and its grace, as a cancelled countdown: {waits:#?}"
    );
}

/// A BUCKET WHOSE COUNTDOWN ENDED IS WAITED ON ACROSS A LOOP'S RESTART (the
/// review of ATERM-KI-1015-1's W3 fix). The same `/resume` at the lab's armed
/// Sonnet bucket: the first loop waits on the bucket's reset and keeps its
/// wall open in the ledger (no `wall-wait-end@v1`), and the loop that
/// follows on the `/resume`d screen takes it up — the bucket's reset and its
/// grace, `model-bucket resets` — and types nothing. Read only as a usage
/// limit's, a bucket's wall was closed where its notice left, and the
/// successor's continue policy typed into it.
#[test]
fn a_bucket_whose_countdown_ended_is_waited_on_across_a_loop_restart() {
    let (ldir, ledger) = ledger_at("bucket-restart");
    let armed = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
    );
    let resumed = || {
        aterm_phase::prompt::fixtures::screen(
            aterm_phase::prompt::fixtures::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
        )
    };
    let (m, lines, waits, jdir) = memory_run_for(
        "bucket-restart-1",
        vec![busy_screen(), armed, resumed()],
        4,
        Some("@s-1"),
        |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{rows}");
    assert!(
        crate::supervise::approvals::open_wall(&ledger, Some("@s-1")).is_some(),
        "the bucket's wall is open for a loop that follows: {rows}"
    );
    let (m, lines, waits, jdir) =
        memory_run_for("bucket-restart-2", vec![resumed()], 4, Some("@s-1"), |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_turn_end_timing(TurnEndTiming {
                reset_grace: Duration::from_secs(1),
                reset_jitter: Duration::ZERO,
                short_backoff: Duration::from_secs(1),
                ..TurnEndTiming::default()
            });
        });
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&ldir);
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        waits.last().is_some_and(
            |w| w.contains("until=2026-10-17T23:20:01Z") && w.ends_with("model-bucket resets")
        ),
        "the bucket's own wait, taken up: {waits:#?}"
    );
}

/// A `/CLEAR` AT AN ARMED LIMIT CLOSES THE LEDGER'S WALL, AND SAYS WHY (W4 of
/// ATERM-KI-1015-1, the lab on Claude Code 2.1.300, 2026-10-15: `lp20w4cb`).
/// The countdown armed, the wall open in the ledger (`wall-wait@v1`); a person
/// `/clear`s: the screen is the launch card and `❯ /clear`, a conversation
/// with no task, and Claude Code's countdown is gone with the old one.
/// Measured on the base: no point was ever decided there (`❯ /clear` read as a
/// prompt not answered yet), the policy went on waiting on the limit of a
/// conversation that no longer exists, and the ledger's `wall-wait@v1` stood
/// open for good — a loop that follows takes it up again. A conversation with
/// no task owes no continuation at any reset: the wait ends there, and the
/// ledger's `wall-wait-end@v1` says so. Nothing is typed.
#[test]
fn a_clear_at_an_armed_limit_closes_the_ledgers_wall_and_says_why() {
    let (ldir, ledger) = ledger_at("w4-clear");
    let (m, lines, waits, jdir) = memory_run_for(
        "w4-clear-armed",
        vec![
            busy_screen(),
            armed_conversation_b(),
            cleared_at_an_armed_limit(),
        ],
        6,
        Some("@s-1"),
        |s| {
            s.set_clock(TEST_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        },
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&ldir);
    assert_eq!(m.turns, 0, "{lines:#?}\n{waits:#?}\n{:#?}", m.requests);
    assert!(
        rows.contains("\"rule_id\":\"wall-wait@v1\""),
        "the wall was waited on: {rows}"
    );
    let end = rows
        .lines()
        .filter(|r| r.contains("\"rule_id\":\"wall-wait-end@v1\""))
        .collect::<Vec<_>>();
    assert!(
        end.len() == 1 && end[0].contains("no task"),
        "the wall closed once, saying why: {rows}"
    );
    assert!(
        crate::supervise::approvals::open_wall(&ledger, Some("@s-1")).is_none(),
        "no wall open for a loop that follows: {rows}"
    );
}

/// The measured armed limit ([`auto_continue_wall_at`]'s, `clock` its reset)
/// drawn under the loop's own `keep going`: the stub answered the line with
/// its 429 at once, so no read saw the worker busy since it was typed.
fn own_act_met_a_limit(clock: &str) -> Vec<String> {
    auto_continue_wall_at(clock)
        .into_iter()
        .map(|r| r.replace("❯ carry on with the task", "❯ keep going"))
        .collect()
}

/// [`own_act_met_a_limit`] after Esc on the vendor's countdown: the measured
/// cancel (`⏺ Automatic continue cancelled`), an idle screen with no wall, its
/// newest notice naming `clock`.
fn own_act_limit_cancelled(clock: &str) -> Vec<String> {
    auto_continue_cancelled()
        .into_iter()
        .map(|r| {
            r.replace("3:50pm", clock)
                .replace("❯ carry on with the task", "❯ keep going")
        })
        .collect()
}

/// A LIMIT MET WHILE THE LOOP'S OWN ACT IS STILL INSIDE ITS TAKE WINDOW IS
/// TRACKED (round 15's left-wall run, lab tag `amlw15`, Claude Code 2.1.300,
/// 2026-10-14): the loop typed `continue@v1 keep going`; the stub answered it
/// with a five-hour limit at once (`resets 2:26pm`, no busy read), read as
/// `WAITING … the worker to take the continue@v1 act`; a person's Esc on the
/// vendor's countdown three seconds later took the notice off the screen,
/// and at the take window's end the point was decided as the act's short
/// turn — `3 short turn(s) in a row: 8 min back-off`, a continuation due into
/// the limit, and `wall-wait-end@v1` written at the Esc. The point is not the
/// act's until its window ends (its own bookkeeping, as before), but the wall
/// it shows is the policy's to track now: when the notice leaves inside the
/// window, the point is waited on to the reset the notice named
/// (`usage-session resets`), the ledger keeps the wall open, and nothing is
/// typed before the reset (10:38, an hour and 43 minutes past the test's
/// 08:55). Measured before the fix: a second `keep going` at the continue
/// policy's back-off, and the wall's end in the ledger.
#[test]
fn a_limit_met_inside_the_own_acts_take_window_is_waited_on_when_its_notice_leaves() {
    let (dir, journal) = journal_file("te-takewin");
    let (ldir, ledger) = ledger_at("takewin");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            own_act_met_a_limit("10:38am"),
            own_act_limit_cancelled("10:38am"),
        ],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(60);
    m.stall_sleep = Some(Duration::from_millis(100));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ldir);
    let waits: Vec<&str> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.as_str())
        .collect();
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        ["CONTINUED seq=102 rule=continue@v1 keep going"],
        "the act that met the limit, and nothing typed into the limit: {lines:#?}\n{waits:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert!(
        waits
            .iter()
            .any(|w| w.contains("the worker to take the continue@v1 act")),
        "the act's own window still stands: {waits:#?}"
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.starts_with("until=2026-09-17T17:38:0")
                && w.ends_with("usage-session resets")),
        "the reset the notice named and its grace: {waits:#?}\n{lines:#?}"
    );
    assert!(
        rows.contains("\"rule_id\":\"wall-wait@v1\"")
            && !rows.contains("\"rule_id\":\"wall-wait-end@v1\""),
        "the wall is open in the ledger: {rows}"
    );
}

/// A LIMIT CANCELLED BEFORE ANY READ AFTER THE OWN ACT IS WAITED ON (round
/// 16's residual (a), lab tag `tw16after`, Claude Code 2.1.300, 2026-10-14):
/// the loop typed `continue@v1 keep going`; the stub answered it with a
/// five-hour limit at once, and a person's Esc on the vendor's countdown came
/// before the loop's first read after its act — that read was already the
/// cancel under the notice (`⏺ Automatic continue cancelled`), no wall. The
/// newest limit notice no turn has been served since, under the vendor's word
/// that it will not go on by itself, is the act's answer: the limit is
/// tracked from that reading, its reset placed (10:38, an hour and 43 minutes
/// past the test's 08:55), the ledger keeps the wall open, and nothing is
/// typed before the reset. Measured before the fix: the window's end read the
/// act's short turn (`2 short turn(s) in a row`), and the continue policy
/// typed `keep going` into the standing limit at its back-off.
#[test]
fn a_limit_cancelled_before_any_read_after_the_own_act_is_waited_on_to_its_reset() {
    let (dir, journal) = journal_file("te-leftesc");
    let (ldir, ledger) = ledger_at("leftesc");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            own_act_limit_cancelled("10:38am"),
        ],
    );
    m.turn_releases = Some(1);
    m.vanish_after = Some(60);
    m.stall_sleep = Some(Duration::from_millis(100));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ldir);
    let waits: Vec<&str> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.as_str())
        .collect();
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        ["CONTINUED seq=102 rule=continue@v1 keep going"],
        "the act the limit answered, and nothing typed into the limit: {lines:#?}\n{waits:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    // The first read after the act: the cancel's notice read as the limit
    // point it was (its episode opened), as the same wall read before the
    // cancel is.
    assert!(
        lines.iter().any(|l| l.starts_with("EVENT limited seq=103 ")
            && l.contains("Usage limit reached")
            && l.ends_with(" reset=10:38am")),
        "{lines:#?}"
    );
    assert!(
        waits
            .iter()
            .any(|w| w.contains("the worker to take the continue@v1 act")),
        "the act's own window still stands: {waits:#?}"
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.starts_with("until=2026-09-17T17:38:0")
                && w.ends_with("usage-session resets")),
        "the reset the notice named and its grace: {waits:#?}\n{lines:#?}"
    );
    assert!(
        rows.contains("\"rule_id\":\"wall-wait@v1\"")
            && !rows.contains("\"rule_id\":\"wall-wait-end@v1\""),
        "the wall is open in the ledger: {rows}"
    );
}

/// A RETRY THE LIMIT ANSWERS INSIDE ITS OWN TAKE WINDOW IS MET, NOT TAKEN
/// (round 16's review, 2026-10-14): the loop's `keep going` met a limit two
/// seconds from its reset; Esc on the vendor's countdown left its notice; at
/// the reset the loop typed `usage-resume@v1 keep going`, and the limit
/// answered that at once (no busy read) with a new notice an hour out — and
/// Esc on the new countdown, inside the resume's window, left that notice
/// too. The point that judges the resume finds no wall, but the wall
/// answered it: the new limit is waited on to the reset it named
/// (`usage-session resets`, 16:55 and its grace), the ledger keeps the wall
/// open, and nothing more is typed. Measured before: the resume read as
/// taken, `wall-wait-end@v1` written, and `continue@v1 keep going` typed into
/// the standing limit at the continue policy's back-off.
#[test]
fn a_resume_the_limit_answers_inside_its_window_is_waited_on_to_the_new_reset() {
    let (dir, journal) = journal_file("te-takewin-retry");
    let (ldir, ledger) = ledger_at("takewin-retry");
    let kept = |r: String| r.replace("❯ carry on with the task", "❯ keep going");
    let armed =
        |span: &str| -> Vec<String> { auto_continue_wall_in(span).into_iter().map(kept).collect() };
    let cancelled = |span: &str| -> Vec<String> {
        auto_continue_cancelled()
            .into_iter()
            .map(|r| {
                kept(r)
                    .replace(
                        "resets 3:50pm (America/Los_Angeles)",
                        &format!("resets in {span}"),
                    )
                    .replace(
                        "resets 3:35pm (America/Los_Angeles)",
                        &format!("resets in {span}"),
                    )
            })
            .collect()
    };
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            armed("2s"),
            cancelled("2s"),
            armed("1h"),
            cancelled("1h"),
        ],
    );
    m.turn_gates = vec![1, 3];
    m.vanish_after = Some(100);
    m.stall_sleep = Some(Duration::from_millis(100));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ldir);
    let waits: Vec<&str> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.as_str())
        .collect();
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        [
            "CONTINUED seq=102 rule=continue@v1 keep going",
            "CONTINUED seq=104 rule=usage-resume@v1 keep going",
        ],
        "nothing typed into the limit that answered the resume: {lines:#?}\n{waits:#?}"
    );
    assert!(
        waits
            .iter()
            .any(|w| w.contains("the worker to take the usage-resume@v1 act")),
        "the resume's own window: {waits:#?}"
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.starts_with("until=2026-09-17T16:55:0")
                && w.ends_with("usage-session resets")),
        "the reset the answer named and its grace: {waits:#?}\n{lines:#?}"
    );
    assert!(
        !rows.contains("\"rule_id\":\"wall-wait-end@v1\""),
        "the wall is open in the ledger: {rows}"
    );
}

/// The measured cancel ([`auto_continue_cancelled`]) as a PERSON'S turn
/// left it: their `carry on with the task` met a five-hour limit at once and
/// their Esc on the vendor's countdown came before the loop's next read — the
/// cancel under the notice (naming `clock`) under the person's prompt, no
/// wall, and no act of the loop's under it.
fn person_limit_cancelled(clock: &str) -> Vec<String> {
    auto_continue_cancelled()
        .into_iter()
        .map(|r| r.replace("3:50pm", clock))
        .collect()
}

/// The journal's waits, the `CONTINUED` lines and the ledger's rows of one
/// hostless `watch` over `screens` (the policy's clocks short: a second's
/// take window, a tenth of a second's back-off, a second's grace), the first
/// `held` screens sitting until a turn is sent.
fn left_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    held: Option<usize>,
) -> (Vec<String>, Vec<String>, Vec<String>, String, Mock) {
    let (dir, journal) = journal_file(&format!("te-{tag}"));
    let (ldir, ledger) = ledger_at(tag);
    let mut m = Mock::new(true, screens);
    m.turn_releases = held;
    m.vanish_after = Some(60);
    m.stall_sleep = Some(Duration::from_millis(100));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ldir);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.clone())
        .collect();
    let typed = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .cloned()
        .collect();
    (lines, waits, typed, rows, m)
}

/// A PERSON'S TURN THE LIMIT ANSWERS AT ONCE, CANCELLED BEFORE ANY READ, IS
/// WAITED ON TO ITS RESET (round 17's residual (a), what was left of it): the
/// loop backs off at the first point it sees (an idle worker it attached to);
/// a person's `carry on with the task` meets a five-hour limit at once, and
/// their Esc on the vendor's countdown comes before the loop's next read — so
/// that read shows only the cancel under the notice under the person's
/// prompt: no wall, and no act of the loop's for it to answer. With no
/// transcript to date the notice, the screen alone says it is NEW since the
/// loop last looked (that look drew no cancelled limit), and the loop fails
/// closed: the limit is tracked from that reading as its episode, its reset
/// placed (10:38, an hour and 43 minutes past the test's 08:55), the ledger
/// keeps the wall open, and nothing is typed before the reset. Measured
/// before: no wall was read, and the continue policy's back-off typed `keep
/// going` into the standing limit.
#[test]
fn a_persons_turn_the_limit_answers_cancelled_before_any_read_is_waited_on_to_its_reset() {
    let (lines, waits, typed, rows, m) = left_run(
        "personcancel",
        vec![
            ended("Fixed the parser; the suite is green."),
            person_limit_cancelled("10:38am"),
        ],
        None,
    );
    assert!(
        typed.is_empty(),
        "nothing typed into the limit: {lines:#?}\n{waits:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        lines.iter().any(|l| l.starts_with("EVENT limited ")
            && l.contains("Usage limit reached")
            && l.ends_with(" reset=10:38am")),
        "{lines:#?}"
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.starts_with("until=2026-09-17T17:38:0")
                && w.ends_with("usage-session resets")),
        "the reset the notice named and its grace: {waits:#?}\n{lines:#?}"
    );
    assert!(
        rows.contains("\"rule_id\":\"wall-wait@v1\"")
            && !rows.contains("\"rule_id\":\"wall-wait-end@v1\""),
        "the wall is open in the ledger: {rows}"
    );
}

/// ANOTHER CONVERSATION'S OLDER CANCELLED LIMIT, `/resume`d INSIDE THE OWN
/// ACT'S WINDOW, IS NO DAY'S WAIT (round 17's residual (a), what was left of
/// it). The loop's `keep going` is awaited, and before its first read after
/// it a person `/resume`s a conversation whose last thing is an older
/// five-hour limit cancelled on its countdown (`continuing automatically at
/// 7:30am`): the screen shows only that cancel under that notice, and with no
/// transcript nothing tells it from the act's own answer. Read as the
/// answer, nothing dates its print, so it is placed as a wall found sitting
/// (its done row, `done 3:46 PM`, the session rule) and never more than a
/// session window out: its clock, an hour and 25 minutes past at the test's
/// 08:55, is PASSED, and the continuation goes at its grace — the limit, had
/// it stood, would answer that with a notice the loop reads as it arrives.
/// Measured before: placed on the print of the act it was taken to answer,
/// `7:30am` was TOMORROW's — `WAITING until=2026-09-18T14:30:01Z`, 22 hours
/// and 35 minutes with the session free.
#[test]
fn an_older_cancelled_limit_resumed_inside_the_acts_window_is_no_days_wait() {
    let (lines, waits, typed, _, _) = left_run(
        "resumedold",
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            person_limit_cancelled("7:30am"),
        ],
        Some(1),
    );
    assert!(
        !waits.iter().any(|w| w.starts_with("until=2026-09-18")),
        "no wait for tomorrow's 7:30: {waits:#?}\n{lines:#?}"
    );
    assert!(
        typed.len() >= 2
            && typed[0].ends_with(" rule=continue@v1 keep going")
            && typed[1].ends_with(" rule=usage-resume@v1 keep going"),
        "the act, then the continuation at the passed reset's grace: {lines:#?}\n{waits:#?}"
    );
}

/// The measured armed limit (2.1.296's capture) as a WEEKLY limit whose bare
/// clock reset is `clock` (no session window named: the vendor prints a bare
/// clock for a reset within a day ahead), its countdown armed, drawn under
/// the loop's own `keep going` — the stub's 429 answered the line at once,
/// so the done row above it (`done 3:11 PM`) is an older turn's.
fn weekly_met_by_own_act(clock: &str) -> Vec<String> {
    auto_continue_wall_at(clock)
        .into_iter()
        .map(|r| {
            r.replace("❯ carry on with the task", "❯ keep going")
                .replace("session limit", "weekly limit")
        })
        .collect()
}

/// [`weekly_met_by_own_act`] after Esc on the vendor's countdown: the
/// measured cancel, an idle screen with no wall, the newest notice naming
/// `clock`.
fn weekly_met_by_own_act_cancelled(clock: &str) -> Vec<String> {
    auto_continue_cancelled()
        .into_iter()
        .map(|r| {
            r.replace("3:50pm", clock)
                .replace("3:35pm", clock)
                .replace("❯ carry on with the task", "❯ keep going")
                .replace("session limit", "weekly limit")
        })
        .collect()
}

/// A WALL READ INSIDE THE LOOP'S OWN ACT'S TAKE WINDOW ANSWERED THAT ACT, AND
/// IS PLACED ON ITS PRINT (round 16's review, residual (b)): the loop's `keep
/// going` met a weekly limit, `resets 6am` read at 08:55 under an older
/// turn's `done 3:11 PM` — no turn ran for the line, so nothing on the screen
/// dates the notice — and Esc on the countdown took it off the screen inside
/// the act's window. The limit stands NOW (it answered the act), so `6am` is
/// tomorrow's: waited on to 06:00 and its grace (13:00Z), nothing typed.
/// Measured before: the window's reading placed the clock on the old done
/// row (today's 06:00, three hours PASSED, the rule a wall found sitting
/// gets), and the window's end typed `usage-resume@v1 keep going` into the
/// standing limit at once. The same read past the window was always placed
/// so ([`a_weekly_wall_that_answers_the_loops_own_continuation_is_waited_out_to_its_reset`]).
#[test]
fn a_wall_read_inside_the_own_acts_window_is_placed_on_its_print() {
    let (dir, journal) = journal_file("te-takewin-placed");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            weekly_met_by_own_act("6am"),
            weekly_met_by_own_act_cancelled("6am"),
        ],
    );
    m.turn_gates = vec![1];
    m.vanish_after = Some(60);
    m.stall_sleep = Some(Duration::from_millis(100));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    let waits: Vec<&str> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| r.summary.as_str())
        .collect();
    let typed: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        typed,
        ["CONTINUED seq=102 rule=continue@v1 keep going"],
        "the act the limit answered, and nothing typed into it: {lines:#?}\n{waits:#?}"
    );
    assert!(
        waits
            .last()
            .is_some_and(|w| w.starts_with("until=2026-09-18T13:00:0") && w.ends_with(" resets")),
        "tomorrow's 06:00 and its grace: {waits:#?}\n{lines:#?}"
    );
}

/// [`login_expired`] with a person's local command said under its row
/// (`/model`, kept): the row is no longer the last thing said, and the
/// screen reads idle with no wall — the login not back.
fn login_left() -> Vec<String> {
    let mut r = login_expired();
    let at = r
        .iter()
        .position(|row| row.starts_with("⏺ Login expired"))
        .expect("the wall row");
    r.splice(
        at + 1..at + 1,
        rows(&["", "❯ /model", "  ⎿  Kept model as Opus 4.7"]),
    );
    r
}

/// A LOST LOGIN THAT ANSWERED THE LOOP'S OWN ACT AND LEFT THE SCREEN INSIDE
/// THE ACT'S TAKE WINDOW IS TOLD (round 16's review, 2026-10-14): the loop's
/// `keep going` was answered at once by `⏺ Login expired · Please run
/// /login`, and a person's `/model` under it took the row off the screen
/// before the window's end. No point decided the wall, so the point after
/// the window does, as the wall it left: `/login` typed once under
/// `auth-login@v1`, the owner told as it is typed — with the wall's words,
/// the screen no longer showing them — and nothing more. Measured before:
/// the track the window's reading opened held the point as a login already
/// told of, and the loop sat silent with the login lost.
#[test]
fn a_lost_login_left_inside_the_own_acts_window_types_login_and_tells_the_owner() {
    let (dir, journal) = journal_file("te-takewin-login");
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            ended("Fixed the parser; the suite is green."),
            login_expired(),
            login_left(),
        ],
    );
    m.turn_gates = vec![1];
    m.vanish_after = Some(40);
    m.stall_sleep = Some(Duration::from_millis(100));
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..hosted(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_secs(1),
            short_backoff: Duration::from_millis(100),
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&journal);
    let _ = std::fs::remove_dir_all(&dir);
    let decided: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        decided.len(),
        2,
        "the continuation, then `/login` once: {lines:#?}\n{:#?}",
        m.requests
    );
    assert_eq!(decided[0], "CONTINUED seq=102 rule=continue@v1 keep going");
    assert!(
        decided[1].starts_with("TYPED seq=") && decided[1].ends_with("rule=auth-login@v1 /login"),
        "{lines:#?}"
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention owner=supervisor")
                && r.contains("Login expired · Please run /login")
                && r.contains("finish sign-in in the browser")),
        "the owner told, with the wall's words: {:#?}",
        m.requests
    );
    assert!(
        records.iter().any(|r| r.kind == "escalated"),
        "{records:#?}"
    );
}

/// A weekly wall that says `resets 4pm`
/// and is met again by a continuation is placed afresh on each reprint's own
/// print. Tried at 16:02 and 16:12, the reprint at 16:13 is placed on 16:07 —
/// tomorrow's 16:00, a 23 hour wait for a lift that lagged by twelve minutes
/// (measured: -2 min, then +1427). A jump past the ladder's last
/// rung on the SAME words is held back four reprints — the ladder goes on, about
/// two hours — and then believed: the vendor prints a bare clock only for a reset
/// within a day ahead. NEGATIVE CONTROL: other words are a new wall, placed at
/// once.
#[test]
fn a_reprinted_wall_is_not_sent_to_tomorrow_at_once() {
    let at = |h: i64, m: i64| TEST_NOW + (h - 8) * 3600 + (m - 55) * 60;
    let four = "4pm (America/Los_Angeles)";
    let frame = |done: &str| {
        let mut r = rows(&[
            "⏺ Working.",
            "",
            &format!("✻ Worked for 3m 2s · done {done}"),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on"));
        r
    };
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    let reset_after = |s: &mut Session<'_, Mock>, h: i64, mi: i64, clock: &str, text: &str| {
        s.set_clock(at(h, mi), PDT);
        let _ = s.wall_reset_at(
            Some(text),
            aterm_phase::WallKind::UsageWeekly,
            false,
            true,
            false,
            false,
            &frame(clock),
        );
        let now = s.now_unix();
        s.wall_reset
            .as_ref()
            .and_then(|(_, u)| *u)
            .map(|u| (u - now) / 60)
    };
    // The first print (seen arriving, 16:02): the clock just passed.
    assert_eq!(reset_after(&mut s, 16, 3, "4:02 PM", four), Some(-3));
    // The reprints: the far placement is held back for the first four ...
    for (h, mi, done) in [
        (16, 13, "4:13 PM"),
        (16, 43, "4:43 PM"),
        (17, 13, "5:13 PM"),
        (17, 43, "5:43 PM"),
    ] {
        let got = reset_after(&mut s, h, mi, done, four).expect("a placement");
        assert!(
            got < 0,
            "{h}:{mi:02}: held at the first placement, {got} min from now"
        );
    }
    assert_eq!(s.reprint_jumps, super::turn_end_loop::REPRINT_JUMPS_MAX);
    // ... and believed at the fifth: tomorrow's 4pm.
    let got = reset_after(&mut s, 18, 13, "6:13 PM", four).expect("a placement");
    assert!(got > 21 * 60, "believed: {got} min");
    // CONTROL: other words are a new wall, placed at once (the counter starts over).
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    assert_eq!(reset_after(&mut s, 16, 3, "4:02 PM", four), Some(-3));
    let got =
        reset_after(&mut s, 16, 13, "4:13 PM", "5pm (America/Los_Angeles)").expect("a placement");
    assert!(
        (45..=47).contains(&got),
        "a different clock is placed on its own print: {got}"
    );
}

/// A CODEX WALL'S WORDS KEEP THEIR FIRST PLACEMENT, however often and however
/// late they are read again: a bare clock (`3:05 PM`) is placed by Codex's own
/// rule and cached under its words, and the reprint rule — a wall met again is
/// placed afresh on its own print, a jump past the ladder held four reprints
/// and then believed — is Claude Code's. Read as a reprint, a Codex wall
/// still standing a day after its reset (the next look, 21 hours on) was sent
/// to the NEXT day's clock. NEGATIVE CONTROL: the same words under Claude
/// Code's done row, which carries each look's own clock, are a reprint,
/// believed at the fifth.
#[test]
fn a_codex_walls_words_keep_their_first_placement() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let at = |h: i64, m: i64| TEST_NOW + (h - 8) * 3600 + (m - 55) * 60;
    let day_one = [
        (15, 3),
        (15, 20),
        (15, 50),
        (16, 20),
        (16, 50),
        (17, 20),
        (17, 50),
    ];
    // The minutes from each look to the wall's reset, as placed at that look.
    let ahead = |looks: &[(i64, i64)], rows_at: &dyn Fn(i64, i64) -> Vec<String>| {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.set_zone_lookup(test_zones);
        looks
            .iter()
            .map(|&(h, mi)| {
                s.set_clock(at(h, mi), PDT);
                let _ = s.wall_reset_at(
                    Some("3:05 PM"),
                    aterm_phase::WallKind::UsageWeekly,
                    false,
                    true,
                    false,
                    false,
                    &rows_at(h, mi),
                );
                let unix = s.wall_reset.as_ref().and_then(|(_, u)| *u);
                (unix.expect("a placement") - s.now_unix()) / 60
            })
            .collect::<Vec<_>>()
    };
    let codex_rows = screen(cx::HIT_LIMIT);
    let mut m = Mock::new(true, vec![busy_screen()]);
    assert_eq!(
        session(&mut m, None).reader(&codex_rows).program(),
        aterm_phase::Program::Codex,
        "the fixture is read as Codex"
    );
    let mut looks = day_one.to_vec();
    looks.push((36, 30));
    let codex = ahead(&looks, &|_, _| codex_rows.clone());
    assert_eq!(codex[0], 2, "the first look's placement: {codex:?}");
    assert!(
        codex.iter().all(|&min| min <= 2),
        "never sent to the next day's clock: {codex:?}"
    );
    // CONTROL: Claude Code's reprint of the same words is placed afresh on
    // its own print, and believed once the ladder's holds are spent.
    let claude = ahead(&day_one, &|h, mi| {
        let mut r = rows(&[
            "⏺ Working.",
            "",
            &format!("✻ Worked for 3m 2s · done {}:{mi:02} PM", h - 12),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on"));
        r
    });
    assert!(
        claude.last().is_some_and(|&min| min > 20 * 60),
        "a reprint is believed in the end: {claude:?}"
    );
}

/// THE CENTRAL SAFETY OBLIGATION of the wall act, through the real loop:
/// nothing is typed while a person types. The weekly wall at its reset, a
/// person's keystroke 1 s ago (the
/// server's `status human_ms=`): held; 200 s ago: the continuation goes; the
/// server saying nothing: it goes. A DRAFT standing in the composer is never
/// typed over ([`a_draft_left_at_a_usage_wall_is_submitted_after_the_reset`], the
/// policy's).
#[test]
fn a_person_typing_at_the_reset_holds_the_wall_act() {
    let run = |human_ms: Option<u64>| {
        let screen = weekly_limited();
        let mut m = Mock::new(true, vec![busy_screen(), screen]);
        m.vanish_after = Some(3);
        m.human_ms = human_ms;
        m.cell_attrs = "none";
        m.cursor_on_caret = true;
        m.gen_fence = true;
        m.sends_gen = true;
        m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] <name>: send a named key\n".to_string();
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..resuming(&auto(30, None), None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(AFTER_RESET, PDT);
            s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
        });
        (count(&m, "turn") + count(&m, "send"), lines)
    };
    for (human_ms, want_typed) in [(Some(1_000), false), (Some(200_000), true), (None, true)] {
        let (typed, lines) = run(human_ms);
        assert_eq!(typed > 0, want_typed, "human_ms={human_ms:?}: {lines:#?}");
    }
}

/// A wall's placed reset is cached under its words, and the cache is dropped
/// once the worker has WORKED with no wall on the screen: the next wall of the
/// same words is a new reset, placed afresh. NEGATIVE CONTROL: a look at a
/// screen with no wall before the worker has worked keeps the cache.
#[test]
fn the_wall_cache_is_dropped_when_the_worker_has_worked() {
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    let opts = hosted(30);
    let of = |rows: Vec<String>| Screen {
        rows,
        cursor_row: 0,
        cursor_col: 2,
        seq: 1,
        first: 0,
        generation: None,
        human: crate::supervise::screen::HumanInput::Unknown,
        human_seq: None,
        cols: None,
        pane_rows: None,
    };
    let wall = of(memory_wall_in("3h"));
    let r = s.turn_end_reading(&wall, None, &opts);
    assert!(r.wall.is_some() && r.reset_at.is_some(), "the wall is read");
    assert!(s.wall_reset.is_some(), "and its reset is cached");
    let mut idle = rows(&["⏺ All done.", "", "✻ Worked for 3m 2s · done 8:00 PM", ""]);
    idle.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    let gone = of(idle);
    let _ = s.turn_end_reading(&gone, None, &opts);
    assert!(
        s.wall_reset.is_some(),
        "no wall on the screen but no work either: the cache stays"
    );
    let _ = s.turn_end_reading(&gone, Some(Duration::from_secs(600)), &opts);
    assert!(s.wall_reset.is_none(), "the worker worked: dropped");
    assert_eq!(s.reprint_jumps, 0);
}

/// A Mac that slept through a wait is noticed (the wall clock ran ahead of the
/// monotonic one by more than a minute; a clock stepped BACK is no sleep), and
/// a wait the sleep carried past its wall time is decided again a settle
/// later — the network is coming back — spread by the session's own share so
/// the sessions of one account do not all type in the same second. Only a
/// wait that stands and that the sleep carried past: a point with nothing
/// waited on is decided again by nothing here (its ask would be posted twice),
/// and a wait still ahead falls at its own wall time ([`Session::due_at`]).
#[test]
fn a_mac_that_slept_through_the_wait_decides_the_point_again_after_a_settle() {
    use super::super::slept_through;
    use super::super::turn_end_loop::{WAKE_SETTLE, WAKE_SPREAD};
    assert!(!slept_through(20, 20), "an ordinary step");
    assert!(!slept_through(70, 20 + 60), "a minute of slack");
    assert!(slept_through(20 + 61, 20), "a sleep of a minute");
    assert!(slept_through(3 * 3600, 20), "a night");
    assert!(!slept_through(-30, 20), "a clock stepped back");
    // A wait three hours ahead (or none), and the Mac waking at `wall_now`.
    let sched = |sid: Option<&str>, standing: bool, wall_now: i64| {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = session(&mut m, sid.map(str::to_string));
        s.set_clock(TEST_NOW, PDT);
        if standing {
            s.set_due(
                Instant::now() + Duration::from_secs(3 * 3600),
                "usage-weekly resets",
            );
        }
        // The due is `now + settle + share` for a `now` taken inside the
        // call, so it lies between `before` and `before + took` plus that
        // offset: the call's own measured time is the slack, never a guess at
        // the machine's load.
        let before = Instant::now();
        s.settle_wake(wall_now, Instant::now());
        s.woke(102, wall_now, &mut super::super::Alone);
        let took = before.elapsed();
        s.turn_end_due
            .map(|d| (d.at.saturating_duration_since(before), took, d.why))
    };
    let past = TEST_NOW + 8 * 3600;
    // A wait the sleep carried past: decided again in 30-90 s.
    let (after, took, why) = sched(Some("s-1"), true, past).expect("a wait");
    assert!(
        after >= WAKE_SETTLE && after < WAKE_SETTLE + WAKE_SPREAD + took,
        "{after:?} (the call took {took:?})"
    );
    assert_eq!(why, "the Mac woke");
    // The share is the session's: 20 sessions land on several distinct seconds.
    let mut seconds = std::collections::BTreeSet::new();
    for k in 0..20 {
        let (after, _, _) = sched(Some(&format!("s-{k}")), true, past).expect("a wait");
        seconds.insert(after.as_secs());
    }
    assert!(seconds.len() >= 8, "{seconds:?}");
    // No session id: the settle alone.
    let (after, took, _) = sched(None, true, past).expect("a wait");
    assert!(
        after >= WAKE_SETTLE && after <= WAKE_SETTLE + took,
        "{after:?} (the call took {took:?})"
    );
    // NEGATIVE CONTROLS: no wait, nothing set; a wait the Mac woke short of
    // stands as it was.
    assert_eq!(sched(Some("s-1"), false, past), None);
    let (after, _, why) = sched(Some("s-1"), true, TEST_NOW + 3600).expect("still standing");
    assert_eq!(why, "usage-weekly resets");
    assert!(after > Duration::from_secs(3 * 3600 - 5), "{after:?}");
}

/// Every line a step of the loop said or noted ([`Heard`]).
fn heard_lines(heard: &Heard) -> Vec<String> {
    heard.said.iter().chain(&heard.noted).cloned().collect()
}

/// THE SLEEP, WHERE IT IS WIRED. [`Session::wait_for_next`] marks the two
/// clocks and looks at them at every step; a wall clock that ran ahead of the
/// monotonic one by more than a minute between two steps is a Mac that slept,
/// and the wait it carried past is decided again a settle later. The test
/// stages the sleep in the session's own clock (a fixed clock that jumps 4
/// hours after the wait's first read, the monotonic one standing still) and
/// runs the real wait. NEGATIVE CONTROLS: a clock that only ticks an ordinary
/// step, and one stepped BACK, wake nothing; a wait with nothing standing is
/// left alone.
#[test]
fn a_sleep_between_two_steps_of_the_wait_is_seen_by_the_wait_itself() {
    use super::super::turn_end_loop::{WAKE_SETTLE, WAKE_SPREAD};
    let run = |step: i64, standing: bool| {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        if standing {
            s.set_due(
                Instant::now() + Duration::from_secs(3 * 3600),
                "usage-weekly resets",
            );
        }
        // The wait marks the clocks at its entry, the jump comes after.
        s.set_clock_jump(step);
        let seen = Turn {
            phase: Phase::Idle,
            screen: Screen {
                rows: ended("Stage done."),
                seq: 102,
                ..Screen::default()
            },
            timed_out: false,
        };
        let began = Instant::now();
        let mut deadline = began + Duration::from_millis(600);
        let mut review = Heard::default();
        let end = s.wait_for_next(seen, &hosted(30), &[], &mut deadline, &mut review);
        let took = began.elapsed();
        let lines = heard_lines(&review);
        // The scripted server answers the first wait with a moved screen: the
        // step ends, the loop looks again. What the step did to the wait is
        // what is asserted.
        assert!(end.is_ok(), "step {step}: {lines:#?}");
        let due = s
            .turn_end_due
            .clone()
            .map(|d| (d.at.saturating_duration_since(began), d.why));
        (lines, due, took)
    };
    // Four hours in one read: the Mac slept.
    let (lines, due, took) = run(4 * 3600, true);
    assert!(
        lines.iter().any(|l| l.starts_with("WAITING seq=102 until=")
            && l.ends_with("the Mac woke: deciding again")),
        "{lines:#?}"
    );
    let (after, why) = due.expect("a wait stands");
    assert_eq!(why, "the Mac woke");
    // The settle is placed from a read of the monotonic clock inside the
    // wait, so it lies between `began` and `began + took` past the settle and
    // the session's share (under WAKE_SPREAD): the wait's own measured time is
    // the slack, never a guess at the machine's load.
    assert!(
        after >= WAKE_SETTLE && after < WAKE_SETTLE + WAKE_SPREAD + took,
        "the far deadline is replaced by a settle: {after:?} (the wait took {took:?})"
    );
    // NEGATIVE CONTROLS.
    for step in [0, 20, -30] {
        let (lines, due, _) = run(step, true);
        assert!(
            !lines.iter().any(|l| l.contains("the Mac woke")),
            "step {step}: {lines:#?}"
        );
        assert!(due.expect("still standing").0 > Duration::from_secs(3 * 3600 - 5));
    }
    let (lines, due, _) = run(4 * 3600, false);
    assert!(lines.is_empty() && due.is_none(), "{lines:#?} {due:?}");
}

/// THE SLEEP BETWEEN TWO CALLS OF THE WAIT, WHATEVER POINT SHOWS: a wait that
/// ends on an owed look, a host wake or a repaint is entered again — on the
/// same point, or on the same point at a NEW seq (Claude Code repaints at the
/// wake) — and the Mac may have slept in between. The marks of the watch are
/// the SESSION's ([`Session::watch_sleep`]), not one call's or one seq's, so
/// the second call sees the sleep either way, and the standing due the sleep
/// carried past is decided again a settle later. NEGATIVE CONTROL: the same
/// two calls with no jump between them wake nothing.
#[test]
fn a_sleep_between_two_calls_of_the_wait_is_seen_by_the_second_whatever_point_shows() {
    let seen = |seq: u64| Turn {
        phase: Phase::Idle,
        screen: Screen {
            rows: ended("Stage done."),
            seq,
            ..Screen::default()
        },
        timed_out: false,
    };
    // The jump between the calls (seconds of wall clock the Mac slept), and the
    // seq the second call waits on.
    let run = |jump: i64, second_seq: u64| {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_due(
            Instant::now() + Duration::from_secs(3 * 3600),
            "usage-weekly resets",
        );
        let mut review = Heard::default();
        let mut deadline = Instant::now() + Duration::from_millis(600);
        let first = s.wait_for_next(seen(102), &hosted(30), &[], &mut deadline, &mut review);
        assert!(first.is_ok(), "{:#?}", heard_lines(&review));
        assert!(
            !heard_lines(&review)
                .iter()
                .any(|l| l.contains("the Mac woke")),
            "the first call saw no sleep: {:#?}",
            heard_lines(&review)
        );
        s.set_clock(TEST_NOW + jump, PDT);
        let mut deadline = Instant::now() + Duration::from_millis(600);
        let second = s.wait_for_next(
            seen(second_seq),
            &hosted(30),
            &[],
            &mut deadline,
            &mut review,
        );
        assert!(second.is_ok(), "{:#?}", heard_lines(&review));
        (heard_lines(&review), s.turn_end_due.clone().map(|d| d.why))
    };
    for seq in [102, 103] {
        let (lines, why) = run(4 * 3600, seq);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("WAITING seq={seq} until="))
                    && l.ends_with("the Mac woke: deciding again")),
            "seq {seq}: {lines:#?}"
        );
        assert_eq!(why.as_deref(), Some("the Mac woke"), "seq {seq}");
    }
    // NEGATIVE CONTROL.
    let (lines, why) = run(0, 102);
    assert!(
        !lines.iter().any(|l| l.contains("the Mac woke")),
        "no sleep, no wake: {lines:#?}"
    );
    assert_eq!(why.as_deref(), Some("usage-weekly resets"));
}

/// A DUE THE SLEEP FELL SHORT OF FALLS AT ITS WALL TIME (the audit of
/// 2026-10-09: three tabs left 11.6-14.1 h past a weekly reset their
/// `Instant` waits had slept through). A wait set three hours ahead; the Mac
/// sleeps two of them — the wall clock jumps, the monotonic one stands still
/// — and wakes on a repaint (a new seq). The watch sees the sleep, but the
/// due is not one it carried past ([`Session::woke`] leaves it): it stands,
/// at its WALL time ([`Session::due_at`]) — an hour from the wake, not the
/// three its `Instant` still counts. (A due the sleep carried past waits for
/// the wake's settle instead:
/// [`a_reset_the_sleep_carried_past_waits_for_the_settle_though_a_repaint_wakes_the_loop`].)
/// NEGATIVE CONTROL: no sleep, three hours.
#[test]
fn a_due_the_sleep_fell_short_of_falls_at_its_wall_time() {
    let seen = |seq: u64| Turn {
        phase: Phase::Idle,
        screen: Screen {
            rows: ended("Stage done."),
            seq,
            ..Screen::default()
        },
        timed_out: false,
    };
    let run = |slept: i64| {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_due(
            Instant::now() + Duration::from_secs(3 * 3600),
            "usage-weekly resets",
        );
        let mut review = Heard::default();
        let mut deadline = Instant::now() + Duration::from_millis(600);
        let before = s.wait_for_next(seen(102), &hosted(30), &[], &mut deadline, &mut review);
        assert!(before.is_ok(), "{:#?}", heard_lines(&review));
        s.set_clock(TEST_NOW + slept, PDT);
        let mut deadline = Instant::now() + Duration::from_millis(600);
        let after = s.wait_for_next(seen(103), &hosted(30), &[], &mut deadline, &mut review);
        assert!(after.is_ok(), "{:#?}", heard_lines(&review));
        let left = s
            .due_at()
            .map(|t| t.saturating_duration_since(Instant::now()));
        (s.turn_end_due.clone().map(|d| d.why), left)
    };
    let (why, left) = run(2 * 3600);
    assert_eq!(why.as_deref(), Some("usage-weekly resets"));
    let left = left.expect("a due");
    assert!(
        left > Duration::from_secs(3600 - 60) && left <= Duration::from_secs(3600),
        "an hour from the wake: {left:?}"
    );
    // NEGATIVE CONTROL.
    let (why, left) = run(0);
    assert_eq!(why.as_deref(), Some("usage-weekly resets"));
    let left = left.expect("a due");
    assert!(left > Duration::from_secs(3 * 3600 - 60), "{left:?}");
}

/// What one wait of the loop's ([`Session::wait_for_next`]) said and left: the
/// lines heard, and the due standing after it (its reason).
struct Waited {
    lines: Vec<String>,
    due: Option<String>,
}

/// One wait on `point` (a 600 ms budget), into `heard`: what it said NEW, and
/// the due it left.
fn wait_once(
    s: &mut Session<'_, Mock>,
    point: &Turn,
    opts: &SuperviseOpts,
    heard: &mut Heard,
) -> Waited {
    let (said, noted) = (heard.said.len(), heard.noted.len());
    let mut deadline = Instant::now() + Duration::from_millis(600);
    let end = s.wait_for_next(point.clone(), opts, &[], &mut deadline, heard);
    assert!(end.is_ok(), "{:#?}", heard_lines(heard));
    Waited {
        lines: heard.said[said..]
            .iter()
            .chain(&heard.noted[noted..])
            .cloned()
            .collect(),
        due: s.turn_end_due.as_ref().map(|d| d.why.clone()),
    }
}

/// `rows` at `seq` as the session's own reader takes the point.
fn point_at(s: &Session<'_, Mock>, rows: Vec<String>, seq: u64) -> Turn {
    s.turn_of(Screen {
        rows,
        seq,
        ..Screen::default()
    })
}

/// Whether `lines` hold a continuation under `rule`.
fn continued(lines: &[String], rule: &str) -> bool {
    lines
        .iter()
        .any(|l| l.starts_with("CONTINUED seq=") && l.contains(&format!("rule={rule}")))
}

/// THE WAKE'S FIRST EVENT IS A REPAINT (the audit of 2026-10-09: `WAITING …
/// usage-weekly resets`, the Mac asleep 4.26 days across the reset, and no
/// `CONTINUED … rule=usage-resume@v1` until a person resumed three tabs
/// 11.6-14.1 h after it; and the handoff's residual (b) of the fix that
/// followed). A weekly limit is decided before its reset and waited for; the
/// Mac sleeps eleven hours past the reset, and what wakes the loop is Claude
/// Code repainting — the same point at a NEW seq. The reset the sleep carried
/// the wait past is decided after the wake's settle (the network is coming
/// back: a continuation typed into an API that cannot be reached is a wall of
/// its own), not at that first wait: nothing is typed there, the due stands
/// at the settle (`WAITING … the Mac woke`), and once the settle has passed
/// the point is continued, once — never held by the sleep. NEGATIVE CONTROL:
/// the same waits with no sleep type nothing and leave the reset's wait
/// standing.
#[test]
fn a_reset_the_sleep_carried_past_waits_for_the_settle_though_a_repaint_wakes_the_loop() {
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let run = |slept: bool| {
        let mut m = Mock::new(true, vec![weekly_limited()]);
        let opts = hosted(30);
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let at = |s: &Session<'_, Mock>, seq| point_at(s, weekly_limited(), seq);
        let first = at(&s, 102);
        let action = s
            .turn_end_at_point(&first, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        s.turn_end_execute(&first, action, &opts, &[], &mut heard)
            .expect("the wait is set");
        let before = wait_once(&mut s, &first, &opts, &mut heard);
        assert!(
            !continued(&before.lines, RULE_LIMIT_RESUME),
            "{:#?}",
            before.lines
        );
        if slept {
            s.set_clock(AFTER_RESET + 11 * 3600, PDT);
        }
        let repainted = at(&s, 103);
        let woke = wait_once(&mut s, &repainted, &opts, &mut heard);
        s.pass_wake_settle();
        let again = at(&s, 104);
        let settled = wait_once(&mut s, &again, &opts, &mut heard);
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (woke, settled, typed)
    };
    let (woke, settled, typed) = run(true);
    assert!(
        !continued(&woke.lines, RULE_LIMIT_RESUME),
        "nothing is typed at the wake's first wait: {:#?}",
        woke.lines
    );
    assert!(
        woke.lines
            .iter()
            .any(|l| l.starts_with("WAITING seq=103 until=")
                && l.ends_with("the Mac woke: deciding again")),
        "{:#?}",
        woke.lines
    );
    assert_eq!(woke.due.as_deref(), Some("the Mac woke"));
    assert!(
        continued(&settled.lines, RULE_LIMIT_RESUME),
        "continued once the settle has passed: {:#?}",
        settled.lines
    );
    assert_eq!(typed, 1);
    // NEGATIVE CONTROL.
    let (woke, settled, typed) = run(false);
    let lines: Vec<&String> = woke.lines.iter().chain(&settled.lines).collect();
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("the Mac woke") || l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
    assert_eq!(settled.due.as_deref(), Some("usage-weekly resets"));
    assert_eq!(typed, 0);
}

/// THE POLICY'S OWN BACK-OFF ACROSS A SLEEP (the handoff's residual (a) of
/// 2026-10-09). A 529 is waited out on a rung of an hour; the Mac sleeps
/// eight hours ten minutes into it and wakes on the same point (its seq
/// unchanged, so the wake is seen and its settle placed). Once the settle has
/// passed, the rung — counted on the wall clock, as every pause of the
/// policy's is — is long over: the retry is typed (`api-retry@v1`), not
/// another fifty minutes waited (the rung counted on `Instant`, which a sleep
/// stops). NEGATIVE CONTROL: no sleep, the rung still stands and nothing is
/// typed.
#[test]
fn a_back_off_the_mac_slept_through_is_over_once_the_wake_settles() {
    use crate::supervise::policy::turn_end::RULE_API_RETRY;
    let end_529 = || aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    let run = |slept: bool| {
        let mut m = Mock::new(true, vec![end_529()]);
        let opts = hosted(30);
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_turn_end_timing(TurnEndTiming {
            retry_backoff: vec![Duration::from_secs(3600)],
            ..TurnEndTiming::default()
        });
        let point = point_at(&s, end_529(), 102);
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        assert!(
            matches!(&action, TurnEndAction::WaitUntil { why, .. } if why == "overloaded retry 1"),
            "{action:?}"
        );
        s.turn_end_execute(&point, action, &opts, &[], &mut heard)
            .expect("the wait is set");
        let _ = wait_once(&mut s, &point, &opts, &mut heard);
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let woke = wait_once(&mut s, &point, &opts, &mut heard);
        s.pass_wake_settle();
        let settled = wait_once(&mut s, &point, &opts, &mut heard);
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (woke, settled, typed)
    };
    let (woke, settled, typed) = run(true);
    assert_eq!(
        woke.due.as_deref(),
        Some("the Mac woke"),
        "{:#?}",
        woke.lines
    );
    assert!(
        continued(&settled.lines, RULE_API_RETRY),
        "the rung slept through is over: {:#?}",
        settled.lines
    );
    assert_eq!(typed, 1, "{:#?}", settled.lines);
    // NEGATIVE CONTROL.
    let (_, settled, typed) = run(false);
    assert_eq!(settled.due.as_deref(), Some("overloaded retry 1"));
    assert_eq!(typed, 0, "{:#?}", settled.lines);
}

/// A RESET THE POLICY HOLDS AS AN `Instant` ACROSS A SLEEP (residual (a)'s
/// other half). A five-hour limit's notice names its reset three hours out,
/// and Esc on the vendor's countdown leaves `Automatic continue cancelled`,
/// no wall: the policy waits for the reset the notice NAMED, kept from the
/// last reading that showed it. The Mac sleeps eight hours, past it, and
/// wakes on the same point. Once the settle has passed, the reset is behind
/// the wall clock and the limit is continued (`usage-resume@v1`) — not
/// waited for the three hours the kept `Instant` still counts. NEGATIVE
/// CONTROL: no sleep, the named reset still stands.
#[test]
fn a_reset_whose_notice_left_is_not_held_late_by_a_sleep() {
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let run = |slept: bool| {
        let mut m = Mock::new(true, vec![auto_continue_cancelled()]);
        let opts = hosted(30);
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(600)]));
        let armed = point_at(&s, auto_continue_wall_in("3h"), 101);
        let action = s
            .turn_end_at_point(&armed, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        s.turn_end_execute(&armed, action, &opts, &[], &mut heard)
            .expect("the wall's wait");
        let left = point_at(&s, auto_continue_cancelled(), 102);
        let action = s.turn_end_at_point(&left, None, &opts).expect("a decision");
        assert!(
            matches!(&action, TurnEndAction::WaitUntil { why, .. } if why == "usage-session resets"),
            "{action:?}"
        );
        s.turn_end_execute(&left, action, &opts, &[], &mut heard)
            .expect("the reset's wait");
        let _ = wait_once(&mut s, &left, &opts, &mut heard);
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let woke = wait_once(&mut s, &left, &opts, &mut heard);
        s.pass_wake_settle();
        let settled = wait_once(&mut s, &left, &opts, &mut heard);
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (woke, settled, typed)
    };
    let (woke, settled, typed) = run(true);
    assert_eq!(
        woke.due.as_deref(),
        Some("the Mac woke"),
        "{:#?}",
        woke.lines
    );
    assert!(
        continued(&settled.lines, RULE_LIMIT_RESUME),
        "the named reset is behind the wall clock: {:#?}",
        settled.lines
    );
    assert_eq!(typed, 1, "{:#?}", settled.lines);
    // NEGATIVE CONTROL.
    let (_, settled, typed) = run(false);
    assert_eq!(settled.due.as_deref(), Some("usage-session resets"));
    assert_eq!(typed, 0, "{:#?}", settled.lines);
}

/// ANOTHER EVENT REACHES THE LOOP FIRST (residual (b): "whatever event
/// reaches the loop first"). At a network wall waited on a rung of an hour,
/// the Mac sleeps eight hours; at the wake the host's measure of the API
/// turns `Up` — an edge that decides the point at once, ahead of the due.
/// The wake is seen (the same seq) and the due placed at the settle, and the
/// edge still decides inside it: the try is typed before the network has
/// settled. It must wait for the settle like the due it overtook, and go
/// once the settle has passed (`api-back@v1`). NEGATIVE CONTROL: the same
/// edge with no sleep is acted on at once, as ever.
#[test]
fn an_edge_at_the_wake_types_nothing_before_the_settle() {
    use crate::supervise::policy::turn_end::RULE_API_BACK;
    let run = |slept: bool| {
        let host = ReachHost::scripted(&[Reach::Unknown]);
        let mut m = Mock::new(true, vec![enotfound()]);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.set_turn_end_timing(far_rungs());
        let point = point_at(&s, enotfound(), 102);
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        assert!(
            matches!(&action, TurnEndAction::WaitUntil { .. }),
            "{action:?}"
        );
        s.turn_end_execute(&point, action, &opts, &[], &mut heard)
            .expect("the rung's wait");
        let _ = wait_once(&mut s, &point, &opts, &mut heard);
        host.script.lock().unwrap().push_back(Reach::Up {
            since: Instant::now(),
        });
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let woke = wait_once(&mut s, &point, &opts, &mut heard);
        s.pass_wake_settle();
        let settled = wait_once(&mut s, &point, &opts, &mut heard);
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (woke, settled, typed)
    };
    let (woke, settled, typed) = run(true);
    assert!(
        !continued(&woke.lines, RULE_API_BACK),
        "the edge types nothing inside the settle: {:#?}",
        woke.lines
    );
    assert_eq!(
        woke.due.as_deref(),
        Some("the Mac woke"),
        "{:#?}",
        woke.lines
    );
    assert!(
        continued(&settled.lines, RULE_API_BACK),
        "the try goes once the settle has passed: {:#?}",
        settled.lines
    );
    assert_eq!(typed, 1);
    // NEGATIVE CONTROL.
    let (woke, _, typed) = run(false);
    assert!(continued(&woke.lines, RULE_API_BACK), "{:#?}", woke.lines);
    assert_eq!(typed, 1);
}

/// A host that asks for a point once told to (`wanting`) and takes it with a
/// step that TYPES into the agent — the live upgrade's carry-on — counted; it
/// keeps the words of the guards the loop said withheld a point
/// ([`IdleHost::withheld`]).
#[derive(Debug, Default)]
struct WakeHost {
    wanting: AtomicBool,
    steps: std::sync::atomic::AtomicUsize,
    withheld: std::sync::Mutex<Vec<&'static str>>,
}

impl IdleHost for WakeHost {
    fn wants(&self) -> bool {
        self.wanting.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.steps.fetch_add(1, Ordering::SeqCst);
        self.wanting.store(false, Ordering::SeqCst);
        Some(HostStep {
            line: "upgrade step=carry-on".to_string(),
            moved: true,
            typed: true,
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn withheld(&self, guard: crate::supervise::Guard) {
        self.withheld.lock().unwrap().push(guard.word());
    }
}

/// THE HOST'S STEP AT THE WAKE (the review of 2026-10-14, its probe D). The
/// wake's settle holds more than the turn-end policy's acts: the host's step
/// at an idle point — the live upgrade's notice or carry-on, a relaunched
/// agent's continuation — TYPES a turn into the agent too, into an API the
/// network may not reach yet. An idle point is waited on; the Mac sleeps
/// eight hours; at the wake the host asks for the point. Inside the settle
/// the point is withheld from it, and it is told why (`guard=wake-settle`);
/// once the settle has passed, its step goes. NEGATIVE CONTROL: the same ask
/// with no sleep is stepped at once.
#[test]
fn the_hosts_step_at_the_wake_waits_for_the_settle() {
    let run = |slept: bool| {
        let host = Arc::new(WakeHost::default());
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        let _ = wait_once(&mut s, &point, &opts, &mut heard);
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        host.wanting.store(true, Ordering::SeqCst);
        let woke = wait_once(&mut s, &point, &opts, &mut heard);
        let at_wake = host.steps.load(Ordering::SeqCst);
        s.pass_wake_settle();
        let _ = wait_once(&mut s, &point, &opts, &mut heard);
        let after = host.steps.load(Ordering::SeqCst);
        let withheld = host.withheld.lock().unwrap().clone();
        (woke.lines, at_wake, after, withheld)
    };
    let (lines, at_wake, after, withheld) = run(true);
    assert_eq!(at_wake, 0, "no step inside the settle: {lines:#?}");
    assert!(
        !lines.iter().any(|l| l.starts_with("HOST seq=")),
        "{lines:#?}"
    );
    assert_eq!(withheld, ["wake-settle"], "{lines:#?}");
    assert_eq!(after, 1, "the step goes once the settle has passed");
    // NEGATIVE CONTROL.
    let (lines, at_wake, _, withheld) = run(false);
    assert_eq!(at_wake, 1, "{lines:#?}");
    assert!(withheld.is_empty(), "{withheld:?}");
}

/// A WAIT INSIDE THE WAKE'S SETTLE STEPS TO ITS FALL: what the settle
/// withholds (the host's point) is offered at the top of the step that
/// follows, not a whole step later ([`Session::wait_for_next`]'s step cap, as
/// for [`IdleHost::wants_at`]). The settle falls 300 ms ahead under a
/// five-second budget: the wait's first request asks no longer than that.
/// NEGATIVE CONTROL: with no settle the step is the budget's.
#[test]
fn a_wait_inside_the_wakes_settle_steps_to_its_fall() {
    use super::super::turn_end_loop::WAKE_SETTLE;
    let run = |settling: bool| {
        let host = Arc::new(WakeHost::default());
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.cursor_on_caret = true;
        // The wait's first request is its last: what it asked is the test.
        m.vanish_after = Some(0);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        if settling {
            // No session id, no share: the settle alone, placed so it falls
            // 300 ms from now.
            let woke = Instant::now() + Duration::from_millis(300) - WAKE_SETTLE;
            s.settle_wake(TEST_NOW, woke);
        }
        let mut deadline = Instant::now() + Duration::from_secs(5);
        let _ = s.wait_for_next(point, &opts, &[], &mut deadline, &mut Heard::default());
        drop(s);
        m.requests
            .iter()
            .filter(|r| r.starts_with("await "))
            .find_map(|r| {
                let mut words = r.split(' ');
                words.find(|w| *w == "timeout")?;
                words.next()?.parse::<u64>().ok()
            })
            .expect("the wait's request")
    };
    let asked = run(true);
    assert!(asked <= 300, "stepped to the settle's fall: {asked} ms");
    // NEGATIVE CONTROL.
    let asked = run(false);
    assert!(asked > 300, "the budget's step: {asked} ms");
}

/// THE HOST'S BACKGROUND STEP AT THE WAKE, and the sleep seen by a REQUEST
/// (the review of 2026-10-14). A break in the agent's own background work is
/// offered to the host on a busy read ([`Session::host_steps_in_background`]):
/// the upgrade's notice typed there is a turn into the API like any other.
/// The Mac sleeps between two busy reads; only the read itself — a request
/// ([`Session::call`]) — can see the sleep, since no wait or decision runs
/// on a busy turn. Inside the settle the break is withheld
/// (`guard=wake-settle`); once it has passed, offered. NEGATIVE CONTROL: the
/// same reads with no sleep offer the break at once.
#[test]
fn the_hosts_background_step_at_the_wake_waits_for_the_settle() {
    let bg = || {
        let mut r = rows(&[
            "⏺ Waiting for the build.",
            "",
            "✻ Crunched for 9m 27s · done 11:07 AM · 1 shell still running",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on · 1 shell · ← for agents"));
        r
    };
    let run = |slept: bool| {
        let host = Arc::new(BackgroundHost::default());
        let mut m = Mock::new(true, vec![bg()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.background_settle = Duration::ZERO;
        let _ = s.screen().expect("the busy read before the night");
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let screen = s.screen().expect("the busy read at the wake");
        let mut since = Some(Instant::now());
        s.host_steps_in_background(&screen, &mut since, &mut Alone);
        let at_wake = host.backgrounds.load(Ordering::SeqCst);
        s.pass_wake_settle();
        s.host_steps_in_background(&screen, &mut since, &mut Alone);
        let after = host.backgrounds.load(Ordering::SeqCst);
        let withheld: Vec<&str> = host
            .withheld
            .lock()
            .unwrap()
            .iter()
            .map(|g| g.word())
            .collect();
        (at_wake, after, withheld)
    };
    let (at_wake, after, withheld) = run(true);
    assert_eq!(at_wake, 0, "no notice inside the settle");
    assert_eq!(withheld, ["wake-settle"]);
    assert!(after >= 1, "offered once the settle has passed");
    // NEGATIVE CONTROL.
    let (at_wake, _, withheld) = run(false);
    assert_eq!(at_wake, 1);
    assert!(withheld.is_empty(), "{withheld:?}");
}

/// THE SETTLE COUNTS FROM THE WAKE, NOT FROM THE FIRST ACT AFTER IT (the
/// review of 2026-10-14, its probes A and B). The Mac sleeps through a
/// request of a busy turn — the request sees the sleep — and the turn ends
/// at a new point that the policy continues. Decided inside the settle, the
/// act is held at it (`WAITING … the Mac woke`) and nothing is typed: the
/// path of a sleep a request measured, through the act's own gate. Decided
/// after a busy turn that OUTLASTED the settle, it is typed at once and the
/// wake says nothing: the settle was placed when the sleep was measured, and
/// it has passed. NEGATIVE CONTROL: no sleep, typed at once.
#[test]
fn a_point_after_a_sleep_a_request_saw_is_held_only_until_the_wakes_settle() {
    use crate::supervise::policy::turn_end::RULE_CONTINUE;
    // `slept`: whether the Mac slept; `outlasted`: whether the busy turn ran
    // on past the settle before its point.
    let run = |slept: bool, outlasted: bool| {
        let mut m = Mock::new(true, vec![ended("Stage done.")]);
        let opts = hosted(30);
        let mut heard = Heard::default();
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(TEST_NOW, PDT);
        let _ = s.call(&["status"]).expect("a read before the night");
        if slept {
            s.set_clock(TEST_NOW + 8 * 3600, PDT);
        }
        let _ = s.call(&["status"]).expect("a read of the busy turn");
        if outlasted {
            s.pass_wake_settle();
        }
        let point = point_at(&s, ended("Stage done."), 102);
        let action = s
            .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        s.turn_end_execute(&point, action, &opts, &[], &mut heard)
            .expect("carried out");
        let due = s.turn_end_due.as_ref().map(|d| d.why.clone());
        drop(s);
        let typed = count(&m, "@s-1 turn") + count(&m, "@s-1 send");
        (heard_lines(&heard), due, typed)
    };
    let (lines, due, typed) = run(true, false);
    assert_eq!(typed, 0, "held inside the settle: {lines:#?}");
    assert_eq!(due.as_deref(), Some("the Mac woke"), "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("WAITING seq=102 until=")
            && l.ends_with("the Mac woke: deciding again")),
        "{lines:#?}"
    );
    let (lines, _, typed) = run(true, true);
    assert!(continued(&lines, RULE_CONTINUE), "{lines:#?}");
    assert_eq!(typed, 1, "{lines:#?}");
    assert!(
        !lines.iter().any(|l| l.contains("the Mac woke")),
        "a settle long passed says nothing: {lines:#?}"
    );
    // NEGATIVE CONTROL.
    let (lines, _, typed) = run(false, false);
    assert!(continued(&lines, RULE_CONTINUE), "{lines:#?}");
    assert_eq!(typed, 1, "{lines:#?}");
}

/// A screen as the loop reads one.
fn read_screen(rows: Vec<String>) -> Screen {
    Screen {
        rows,
        cursor_row: 0,
        cursor_col: 2,
        seq: 1,
        first: 0,
        generation: None,
        human: crate::supervise::screen::HumanInput::Unknown,
        human_seq: None,
        cols: None,
        pane_rows: None,
    }
}

/// THE HERD SPREAD (the accounts' sessions all reach the reset in the same
/// second and all type in it). A session's share of
/// [`TurnEndTiming::reset_jitter`] is FNV-1a of its id and the reset: the same
/// session waits the same on every reading (the policy stays a pure function of
/// what it is handed), less than the span, none at a zero span, and 20 sessions
/// land on many distinct seconds. The loop hands the policy that share for the
/// walls a reset lifts, with a session id. NEGATIVE CONTROLS: no session id, and
/// a wall no reset lifts (a 529), get none.
#[test]
fn a_sessions_jitter_is_stable_bounded_and_spread() {
    use super::super::turn_end_loop::resume_jitter;
    let span = Duration::from_secs(60);
    let mut seen = std::collections::BTreeSet::new();
    for k in 0..20 {
        let sid = format!("@s-{k}");
        let j = resume_jitter(&sid, TEST_NOW + 3 * 3600, span);
        assert_eq!(j, resume_jitter(&sid, TEST_NOW + 3 * 3600, span), "stable");
        assert!(j < span, "{sid}: {j:?}");
        assert_eq!(resume_jitter(&sid, 1, Duration::ZERO), Duration::ZERO);
        seen.insert(j.as_secs());
    }
    assert!(seen.len() >= 8, "spread over the minute: {seen:?}");
    // What the loop hands the policy.
    let read = |sid: Option<&str>, rows: Vec<String>| {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = session(&mut m, sid.map(str::to_string));
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let opts = hosted(30);
        let first = s.turn_end_reading(&read_screen(rows.clone()), None, &opts);
        let again = s.turn_end_reading(&read_screen(rows), None, &opts);
        assert_eq!(
            first.jitter, again.jitter,
            "the same session, the same share"
        );
        let unix = s.wall_reset.as_ref().and_then(|(_, u)| *u);
        (first.jitter, unix)
    };
    let mut shares = std::collections::BTreeSet::new();
    for k in 0..20 {
        let sid = format!("@s-{k}");
        let (jitter, unix) = read(Some(&sid), memory_wall_in("3h"));
        let unix = unix.expect("the placed reset");
        assert_eq!(jitter, resume_jitter(&sid, unix, span), "{sid}");
        assert!(jitter < span);
        shares.insert(jitter.as_secs());
    }
    assert!(
        shares.len() >= 8,
        "twenty sessions, several seconds: {shares:?}"
    );
    assert_eq!(
        read(None, memory_wall_in("3h")).0,
        Duration::ZERO,
        "no session"
    );
    let end_529 = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_529);
    assert_eq!(
        read(Some("@s-3"), end_529).0,
        Duration::ZERO,
        "a 529 is no reset"
    );
}

/// THE RESET IS CACHED AS UNIX TIME. An `Instant` does not count the time a Mac
/// sleeps, so a reset cached as an `Instant` lands late by every sleep since; the
/// loop keeps the wall's reset as unix seconds and turns it into an `Instant` at
/// every reading. A wall clock that jumps 4 hours (the sleep) moves the reading
/// by the same 4 hours, the reset itself unmoved. NEGATIVE CONTROL: the cached
/// reset is the same one, not placed afresh.
#[test]
fn a_reset_cached_as_unix_follows_the_wall_clock() {
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let kind = aterm_phase::WallKind::UsageWeekly;
    let rows = memory_wall_in("2h");
    let began = Instant::now();
    let first = s
        .wall_reset_at(Some("in 2h"), kind, false, false, false, false, &rows)
        .expect("a reset");
    let placed = s.wall_reset.clone().expect("cached");
    assert_eq!(placed.1, Some(TEST_NOW + 2 * 3600), "{placed:?}");
    // The Mac slept 4 hours: the wall clock is ahead, the monotonic clock not.
    s.set_clock(TEST_NOW + 4 * 3600, PDT);
    let second = s
        .wall_reset_at(Some("in 2h"), kind, false, false, false, false, &rows)
        .expect("a reset");
    let took = began.elapsed();
    assert_eq!(
        s.wall_reset,
        Some(placed),
        "the same reset, not placed afresh"
    );
    // Each `Instant` is placed from its own read of the monotonic clock, so the
    // gap is the 4 hours less the real time between those two reads, which lie
    // inside `began..began + took`: the calls' own measured time is the slack,
    // never a guess at the machine's load.
    let gap = first.duration_since(second);
    assert!(
        gap <= Duration::from_secs(4 * 3600) && gap >= Duration::from_secs(4 * 3600) - took,
        "the reset now lies 2 hours BEHIND (it was 2 ahead): {gap:?} (the calls took {took:?})"
    );
}

/// A LIMIT A CANCEL LEFT THAT ONLY THE SCREEN READ IS NEVER A DAY'S WAIT
/// (round 17's residual (a)): with no transcript row to date its notice — a
/// conversation `/resume`d inside the act's window ends on its own older
/// cancelled limit, which draws the same — its bare clock is placed as an
/// answer's is (on the act's print, here: an act untaken) and then kept
/// within a session window: at the test's 08:55, `7:30am` (TOMORROW's on the
/// print) and `11:50pm` (fourteen hours ahead) are the day before's, PASSED;
/// `10:38am` (an hour and 43 minutes ahead) stands; a dated reset (`Oct 18 at
/// 7:30am`) and a span are what they name. NEGATIVE CONTROL: the same clocks
/// dated by a transcript row (`undated` false) — `7:30am` is tomorrow's, as
/// the print says.
#[test]
fn an_undated_cancelled_limit_is_placed_within_a_session_window() {
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    let kind = aterm_phase::WallKind::UsageSession;
    let rows = auto_continue_cancelled();
    let mut placed = |text: &str, undated: bool| -> i64 {
        s.set_clock(TEST_NOW, PDT);
        s.wall_reset = None;
        let _ = s.wall_reset_at(Some(text), kind, false, false, true, undated, &rows);
        s.wall_reset
            .as_ref()
            .and_then(|(_, u)| *u)
            .expect("a placement")
            - TEST_NOW
    };
    let min = 60;
    let hour = 3600;
    // 08:55 PDT is 15:55Z.
    assert_eq!(placed("7:30am", true), -(hour + 25 * min), "passed");
    assert_eq!(
        placed("11:50pm", true),
        -(9 * hour + 5 * min),
        "yesterday's, passed"
    );
    assert_eq!(
        placed("10:38am", true),
        hour + 43 * min,
        "within the window"
    );
    assert_eq!(
        placed("Oct 18 at 7:30am", true),
        30 * 86_400 + 22 * hour + 35 * min,
        "a dated reset"
    );
    assert_eq!(placed("in 7h", true), 7 * hour, "a span");
    // NEGATIVE CONTROL: dated by a transcript row.
    assert_eq!(
        placed("7:30am", false),
        22 * hour + 35 * min,
        "tomorrow's on the print"
    );
    assert_eq!(placed("11:50pm", false), 14 * hour + 55 * min);
}

/// EVERY UNIX SECOND IS ITSELF, AT EVERY PHASE OF THE CLOCK. A FIXED clock
/// stood at nine phases of its second (no sleep, no running clock), and at
/// each a unix second made an `Instant` by `instant_of` reads back as itself
/// through `unix_of`, and one still ahead stamps as itself through
/// `stamp_in`. A read-back that drops its own read's phase can still round
/// right just past a second's turn; from 500 ms on it cannot. Each conversion
/// put on whole seconds — `instant_of`, `unix_of`, or `stamp_in` as
/// `now_unix() + ahead` — fails here.
#[test]
fn a_unix_second_reads_back_and_stamps_as_itself_at_any_phase_of_the_clock() {
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let mut bad = Vec::new();
    for phase in [0_i64, 1, 250, 499, 500, 501, 600, 750, 999] {
        s.set_clock_at_ms(TEST_NOW * 1000 + phase, PDT);
        for ahead in [-60_i64, -1, 0, 1, 2, 59, 600, 7200] {
            let u = TEST_NOW + ahead;
            let t = s.instant_of(u);
            let back = s.unix_of(t);
            if back != u {
                bad.push(format!(
                    "phase={phase} ahead={ahead}: unix_of={back}, want {u}"
                ));
            }
            if ahead >= 1 {
                let stamp = s.stamp_in(t);
                let want = utc_stamp(u64::try_from(u).expect("after 1970"));
                if stamp != want {
                    bad.push(format!(
                        "phase={phase} ahead={ahead}: stamp_in={stamp}, want {want}"
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// A SWITCH CARRIED ON KEEPS ITS SECONDS, at every phase of the clock. A
/// loop that starts after another left a model switch or a Codex wind-down
/// open puts the times the ledger's rows name on its own clock
/// ([`Session::carried_model_switch`], [`Session::carried_wind`]), and each
/// reads back — the switch's reset, and the wind-down's reset, hold, last
/// Esc, opening and press in its own row words — as the second its row
/// named. Through the real rows ([`seed_switch`], [`seed_wind_at`]) and their
/// readers. Placed by whole seconds, every time comes back a second late
/// whenever the clock stands 500 ms or more into its second.
#[test]
fn a_switch_carried_on_keeps_its_seconds_at_any_phase_of_the_clock() {
    use crate::supervise::approvals::{open_model_switch, open_wind_down};
    let back = TEST_NOW + 3600;
    let (switch_dir, switch_ledger) = ledger_at("carried-switch-phase");
    seed_switch(&switch_ledger, None, back, false, false);
    let switch = open_model_switch(&switch_ledger, None).expect("the open switch");
    let (since, esc, opened, pressed) = (
        TEST_NOW - 600,
        TEST_NOW - 120,
        TEST_NOW - 2400,
        TEST_NOW - 2340,
    );
    let (wind_dir, wind_ledger) = ledger_at("carried-wind-phase");
    seed_wind_at(
        &wind_ledger,
        "holding",
        &format!("since={since} esc={esc} opened={opened} pressed={pressed}"),
        60,
    );
    let wind = open_wind_down(&wind_ledger, None).expect("the open wind-down");
    let _ = std::fs::remove_dir_all(&switch_dir);
    let _ = std::fs::remove_dir_all(&wind_dir);
    let wind_back = wind.back_at_unix.expect("the wind-down's reset");
    let want_words = [
        format!(" back_at={wind_back} "),
        format!(" since={since} "),
        format!(" esc={esc} "),
        format!(" opened={opened} "),
        format!(" pressed={pressed} "),
    ];
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let mut bad = Vec::new();
    for phase in [0_i64, 1, 250, 499, 500, 501, 600, 750, 999] {
        s.set_clock_at_ms(TEST_NOW * 1000 + phase, PDT);
        let carried = s.carried_model_switch(switch.clone());
        let got = carried.back_at.map(|at| s.unix_of(at));
        if got != Some(back) {
            bad.push(format!(
                "phase={phase}: the switch's reset {got:?}, want {back}"
            ));
        }
        let w = s.carried_wind(wind.clone(), Instant::now());
        let words = s.wind_words(&w, "holding");
        for want in &want_words {
            if !words.contains(want.as_str()) {
                bad.push(format!("phase={phase}: no {want:?} in {words}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// THE WALL IS WRITTEN DOWN, and its end. The loop that waits on a wall writes a
/// `wall-wait@v1` row with the reset as it placed it, for a loop that follows it
/// (a GUI replacement, a fault); a live turn drawn under the wall ends it with a
/// `wall-wait-end@v1` row, and the ledger then holds no open wall. Without
/// the row an update at that moment loses the wait for good.
/// NEGATIVE CONTROL: a run that never saw a wall writes neither row.
#[test]
fn the_wall_waited_on_is_written_to_the_ledger_and_its_end_too() {
    use crate::supervise::approvals;
    let run = |tag: &str, screens: Vec<Vec<String>>| {
        let dir = crate::supervise::test_scratch_path("wm-writes", tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let ledger = dir.join("s-1.jsonl");
        let before = crate::supervise::limit::unix_now();
        let (m, lines, _, jdir) = memory_run_for(tag, screens, 3, Some("@s-1"), |s| {
            s.set_approval_ledger(Some(ledger.clone()))
        });
        let after = crate::supervise::limit::unix_now();
        let _ = std::fs::remove_dir_all(&jdir);
        let rows = std::fs::read_to_string(&ledger).unwrap_or_default();
        let open = approvals::open_wall(&ledger, Some("@s-1"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(m.turns, 0, "{lines:#?}\n{:#?}", m.requests);
        (rows, open, before, after)
    };
    let count_rule = |rows: &str, rule: &str| {
        rows.lines()
            .filter(|l| l.contains(&format!("\"rule_id\":\"{rule}\"")))
            .count()
    };
    // The wall, then the scrolled screen: the wall is written, once, with the
    // reset placed a minute from the print, and stands open.
    let (rows, open, before, after) = run(
        "wm-write",
        vec![
            busy_screen(),
            memory_wall_in("60s"),
            memory_scrolled_in("60s"),
        ],
    );
    assert_eq!(count_rule(&rows, approvals::RULE_WALL_WAIT), 1, "{rows}");
    assert_eq!(
        count_rule(&rows, approvals::RULE_WALL_WAIT_END),
        0,
        "{rows}"
    );
    let open = open.expect("an open wall");
    let reset = open.reset_unix.expect("the placed reset");
    assert!(
        reset >= before + 58 && reset <= after + 62,
        "reset {reset} not within [{before}, {after}] + a minute"
    );
    assert!(open.message.contains("weekly limit"), "{}", open.message);
    // A live turn under the wall ends it, and says so.
    let (rows, open, _, _) = run(
        "wm-end",
        vec![
            busy_screen(),
            memory_wall_in("60s"),
            memory_with_a_live_turn_in("60s"),
        ],
    );
    assert_eq!(count_rule(&rows, approvals::RULE_WALL_WAIT), 1, "{rows}");
    assert_eq!(
        count_rule(&rows, approvals::RULE_WALL_WAIT_END),
        1,
        "{rows}"
    );
    assert!(open.is_none(), "{open:?}");
    // NEGATIVE CONTROL: no wall, no rows.
    let (rows, open, _, _) = run("wm-none", vec![busy_screen(), memory_scrolled_in("60s")]);
    assert!(rows.is_empty() && open.is_none(), "{rows}");
}

/// AN IDLE SCREEN OR A QUESTION ENDS THE MEMORY once the policy has folded it
/// in ([`Session::observe_point`]) and waits on the wall no more: the worker
/// worked since, so the limit is over, and the point is the policy's own (it
/// continues, or asks for a person). The memory must not outlive it, or a
/// LATER screen that shows only background work would be read as the old wall
/// and answered with a spurious `keep going`. NEGATIVE CONTROLS: without the
/// idle screen between, the same scrolled screen IS read as the wall; and an
/// idle screen with no work since (Esc on the vendor's countdown, a person's
/// `/usage`) leaves the policy waiting on the reset the notice named, and the
/// memory with it — the wall it would end is standing still.
#[test]
fn an_idle_screen_or_a_question_ends_the_wall_memory() {
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let opts = hosted(30);
    let wall = read_screen(memory_wall_in("3h"));
    let scrolled = read_screen(memory_scrolled_in("3h"));
    let quiet = |said: &str| {
        let mut idle = rows(&[
            &format!("⏺ {said}"),
            "",
            "✻ Worked for 3m 2s · done 8:00 PM",
            "",
        ]);
        idle.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        read_screen(idle)
    };
    // A point as the loop folds it: read, then into the policy and the memory.
    let fold = |s: &mut Session<'_, Mock>, screen: &Screen, worked: Option<Duration>| {
        let r = s.turn_end_reading(screen, worked, &opts);
        s.observe_point(&r, Instant::now(), screen.seq);
        r
    };
    // CONTROL: the wall, then only its background work: read as the wall.
    let _ = fold(&mut s, &wall, None);
    assert!(s.wall_memory.is_some());
    let r = s.turn_end_reading(&scrolled, None, &opts);
    assert!(
        r.wall.is_some(),
        "the scrolled screen is the remembered wall"
    );
    for said in ["All done.", "Should I rename the module?"] {
        let _ = fold(&mut s, &wall, None);
        assert!(s.wall_memory.is_some(), "{said}");
        let _ = fold(&mut s, &quiet(said), Some(Duration::from_secs(182)));
        assert!(s.wall_memory.is_none(), "{said:?} ends the memory");
        let r = s.turn_end_reading(&scrolled, None, &opts);
        assert!(
            r.wall.is_none(),
            "{said:?}: a stale memory would be read here"
        );
    }
    // CONTROL: no work since the wall — the policy waits on its reset still.
    let _ = fold(&mut s, &wall, None);
    let _ = fold(&mut s, &quiet("All done."), None);
    assert!(s.turn_end.waits_on_named_reset());
    assert!(s.wall_memory.is_some(), "the wall stands: its memory too");
    let r = s.turn_end_reading(&scrolled, None, &opts);
    assert!(r.wall.is_some(), "the scrolled screen is the wall still");
}

/// TIER-1 FOR THE HIDING HALF OF `SupervisorUsageWall`
/// (`aterm_spec::derive::supervisor_usage_wall_model`, dial `Hidden = 1`): the
/// wall's rows leave the screen while the agent's shell keeps the tab reading
/// busy (`WallHides`), and come back (`WallShows`); the loop's memory
/// ([`wall_memory`]) is what keeps the wall a wall meanwhile (`mem`). The
/// integration bind (`tests/supervise_conformance_usage_wall.rs`) drives the
/// reader and the decider with the dial off, since a `Session` is not theirs to
/// reach; this one drives the REAL `Session::turn_end_reading` along every
/// reachable state of the model, replaying the actions that reach it (the
/// wall's appearance and its being met again read the wall's screen, hiding
/// reads the scrolled one, an act taken reads an idle screen after work), and
/// at each state where the wall stands (`wall` 1, or 5 with the wall's rows out
/// of view) the real reading holds a wall exactly where the model enables the
/// act (`wall == 1 || wall == 5 && mem == 1`). NEGATIVE CONTROL: the `Buggy = 1`
/// model's `MemoryLost` (the loop forgets a wall when its rows leave) is not
/// the real code's move — with the real side left as it is, the bind disagrees
/// at the hidden wall the model says is forgotten.
#[test]
fn tier1_the_real_loop_remembers_a_hidden_wall_exactly_where_the_model_does() {
    use aterm_spec::derive::supervisor_usage_wall_model;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    type State = BTreeMap<&'static str, i64>;
    // The clock's range is shrunk (the memory does not read it); the dial is on.
    // A notice that leaves an idle screen is the decider's to wait out, and
    // bound in `tests/supervise_conformance_usage_wall.rs`: its dial is off;
    // so is the own act's take window (`OwnAct`), the decider's too.
    let hidden_model = aterm_spec::interp::with_consts(
        &supervisor_usage_wall_model(),
        &[
            ("Hidden", 1),
            ("Leaves", 0),
            ("OwnAct", 0),
            ("Cap", 5),
            ("Skew", 3),
        ],
    );
    let idle = || {
        let mut idle = rows(&["⏺ All done.", "", "✻ Worked for 3m 2s · done 8:00 PM", ""]);
        idle.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        idle
    };
    // Replays `path` on a fresh loop, then says whether its reading of the
    // screen `st` stands for holds a wall.
    let real_wall = |path: &[&'static str], st: &State| -> bool {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let opts = hosted(30);
        // A point as the loop folds it: read, then into the policy and the
        // memory ([`Session::observe_point`]).
        let mut fold = |rows: Vec<String>, worked: Option<Duration>| {
            let screen = read_screen(rows);
            let r = s.turn_end_reading(&screen, worked, &opts);
            s.observe_point(&r, Instant::now(), screen.seq);
        };
        for action in path {
            match *action {
                "WallAppears" | "WallAppearsLate" | "WallShows" | "Met" => {
                    fold(memory_wall_in("3h"), Some(Duration::from_secs(600)));
                }
                // Found standing: read with no turn seen.
                "WallFoundSitting" => fold(memory_wall_in("3h"), None),
                "WallHides" => fold(memory_scrolled_in("3h"), None),
                "Taken" => fold(idle(), Some(Duration::from_secs(600))),
                // The world, the clock and the act: nothing the memory reads.
                // `MemoryLost` is the defect: the real loop makes no such move.
                _ => {}
            }
        }
        let screen = if st["wall"] == 5 {
            memory_scrolled_in("3h")
        } else {
            memory_wall_in("3h")
        };
        s.turn_end_reading(&read_screen(screen), None, &opts)
            .wall
            .is_some()
    };
    let walk = |m: &aterm_spec::derive::Model| {
        let mut seen: BTreeSet<State> = BTreeSet::new();
        let mut queue = VecDeque::from([(m.init_state(), Vec::<&'static str>::new())]);
        let (mut checked, mut hidden_checked, mut disagree) = (0, 0, Vec::new());
        while let Some((st, path)) = queue.pop_front() {
            if !seen.insert(st.clone()) {
                continue;
            }
            if st["wall"] == 1 || st["wall"] == 5 {
                let want = st["wall"] == 1 || st["mem"] == 1;
                let got = real_wall(&path, &st);
                checked += 1;
                hidden_checked += usize::from(st["wall"] == 5);
                if got != want {
                    disagree.push((st.clone(), got, want));
                }
            }
            for action in &m.actions {
                let mut next = st.clone();
                if m.fire(action.name, &mut next) {
                    let mut p = path.clone();
                    p.push(action.name);
                    queue.push_back((next, p));
                }
            }
        }
        (checked, hidden_checked, disagree)
    };
    let (checked, hidden_checked, disagree) = walk(&hidden_model);
    assert!(disagree.is_empty(), "{disagree:#?}");
    assert!(
        checked > 40 && hidden_checked > 10,
        "{checked} {hidden_checked}"
    );
    // NEGATIVE CONTROL.
    let buggy = aterm_spec::interp::with_buggy(&hidden_model, 1);
    let (_, _, off) = walk(&buggy);
    assert!(
        off.iter()
            .any(|(st, got, want)| st["wall"] == 5 && st["mem"] == 0 && *got && !*want),
        "the model that forgets the wall agrees with the real loop: {off:?}"
    );
}

/// ONE PLACEMENT OF A WALL'S RESET. The wait for a weekly wall's reset and the
/// budget stretched past it (`EXTEND`, which keeps a `supervise --max-s` from
/// timing out under the wall) read the same reset: the one the loop placed
/// (`place_reset`, by the wall's kind and its print). An evening weekly wall
/// that says `resets 9am` is tomorrow's 9am, 13 hours off; the session
/// window's rule (`limit::reset_at`: today's, tomorrow's only when it is under
/// five hours off) put it in the PAST for the stretch while the wait held for
/// tomorrow. NEGATIVE CONTROL: a wall no placement was made for falls back on
/// the session rule.
#[test]
fn the_budget_is_stretched_to_the_reset_the_wait_holds_for() {
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    // 2026-09-28 20:00 PDT, the wall printed at 12:19 PM (the done row).
    let now = 1_790_650_800;
    s.set_clock(now, PDT);
    s.set_zone_lookup(test_zones);
    let rows: Vec<String> = memory_wall()
        .into_iter()
        .map(|r| r.replace("resets in 2s", "resets 9am (America/Los_Angeles)"))
        .collect();
    let text = "9am (America/Los_Angeles)";
    // No placement yet: the session rule, in the past.
    assert!(s.reset_unix(Some(text)).is_some_and(|at| at < now));
    let r = s.turn_end_reading(&read_screen(rows), None, &hosted(30));
    let waited = r.reset_at.expect("the wait");
    let unix = s.wall_reset.as_ref().and_then(|(_, u)| *u).expect("placed");
    assert!(
        unix > now + 12 * 3600 && unix < now + 14 * 3600,
        "{}",
        unix - now
    );
    assert!(waited > Instant::now() + Duration::from_secs(12 * 3600));
    assert_eq!(
        s.reset_unix(Some(text)),
        Some(unix),
        "the stretch reads it too"
    );
    // Other words are not the placed wall's.
    assert!(
        s.reset_unix(Some("10am (America/Los_Angeles)"))
            .is_some_and(|at| at < now)
    );
}

/// THE WAIT PLACES A RESET ACROSS A DAYLIGHT-SAVING CHANGE with the offset its
/// zone has where it falls ([`limit::place_reset_with`]), not the one it has
/// when the notice is read: the placement the continuation's wait is built on
/// (`reset_unix_for`), for a bare clock with and without a print to go by, a
/// dated one, and a Codex wall. PDT until 2026-11-01 09:00Z, PST after.
#[test]
fn the_wait_places_a_reset_across_a_dst_change_with_the_offset_there() {
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    fn la(zone: &str, unix: i64) -> Option<i64> {
        let pst = unix >= 1_793_523_600;
        (zone == "America/Los_Angeles").then_some(if pst { -8 * 3600 } else { PDT })
    }
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.set_zone_lookup(la);
    // Read at 23:30 PDT on Oct 31, `3am` is 3am PST, 11:00Z: an hour later than
    // the PDT reading, 10:00Z, that the offset at the read says.
    s.set_clock(1_793_514_600, PDT);
    let three = Some("3am (America/Los_Angeles)");
    assert_eq!(
        s.reset_unix_for(three, false, true, false, &[]),
        Some(1_793_530_800)
    );
    // With a print to go by (`done 11:25 PM`) it is the first 3am after the print.
    let mut printed = rows(&["⏺ Working.", "", "✻ Worked for 3m 2s · done 11:25 PM", ""]);
    printed.extend(composer("  ⏵⏵ bypass permissions on"));
    assert_eq!(
        s.reset_unix_for(three, false, true, false, &printed),
        Some(1_793_530_800)
    );
    // A dated reset, read on Oct 30 at 12:00 PDT, on Claude Code's screen and on Codex's.
    s.set_clock(1_793_386_800, PDT);
    let nov_2 = Some("Nov 2 at 9am (America/Los_Angeles)");
    assert_eq!(
        s.reset_unix_for(nov_2, false, true, false, &[]),
        Some(1_793_638_800)
    );
    assert_eq!(
        s.reset_unix_for(nov_2, false, true, false, &screen(cx::HIT_LIMIT)),
        Some(1_793_638_800)
    );
}

/// A host that only listens for the canary's ladder.
#[derive(Debug, Default)]
struct LadderHost {
    told: std::sync::Mutex<Vec<String>>,
}

impl IdleHost for LadderHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn exception(&self, standing: Option<&crate::supervise::policy::exception::Exception>) {
        self.told.lock().unwrap().push(format!(
            "exception:{}",
            standing.map_or("none", |e| e.kind.word())
        ));
    }
    fn stall_unread(&self, minutes: Option<u64>) {
        self.told
            .lock()
            .unwrap()
            .push(format!("unread:{}", minutes.map_or("none", |_| "some")));
    }
}

/// THE STALL'S END KEEPS THE POINT UNDER IT: the host has
/// one exception slot per tab, so when the raised stall ends while a point's
/// exception still stands, the host is told that point again, not `None`.
/// NEGATIVE CONTROL: with no point standing, the end is `exception:none`.
#[test]
fn a_stall_that_ends_tells_the_host_the_point_that_stood_under_it() {
    use crate::supervise::policy::outlook::ExceptionKind;
    let end = |standing: bool| {
        let host = Arc::new(LadderHost::default());
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.stall_host = Some(Arc::clone(&host) as Arc<dyn IdleHost>);
        s.stuck_rung = super::super::wall_memory::StuckRung::Raised;
        s.stuck_raised = Some((Instant::now(), ExceptionKind::Stalled));
        if standing {
            let mut point = crate::supervise::policy::exception::Exception::stalled();
            point.kind = ExceptionKind::Futile;
            s.exception_open = Some((point, Instant::now()));
        }
        s.stuck_over(1, "the screen moved on", &mut Alone);
        host.told.lock().unwrap().clone()
    };
    assert_eq!(
        end(true),
        ["unread:none", "exception:futile"],
        "the point that stood under the stall is told again"
    );
    assert_eq!(end(false), ["unread:none", "exception:none"]);
}

/// THE CANARY FOR A SCREEN NO READER NAMES AND THE
/// LADDER ABOVE IT (design-exceptions 9.3). A screen that reads busy on the
/// agent's own background work alone, while the agent's OWN terminal title
/// rests on the idle glyph (`✳`; a working agent animates it), is one this
/// build does not read. Kept up:
/// `STUCK` (rung 1, the journal), `STALLED` and the host's record (rung 2,
/// three times as long), `EXCEPTION kind=stalled` and the host's exception (rung
/// 3, six times), each ONCE, and the loop's end withdraws both (`CLEARED
/// exception=stalled`, `unread:none`, `exception:none`). Nothing is typed. The
/// title is asked again at each rung: NEGATIVE CONTROLS, a title that animates,
/// a live spinner on the screen and a screen that shows no title climb no rung.
#[test]
fn a_stall_kept_up_climbs_to_a_record_and_then_an_exception_and_its_end_withdraws_both() {
    let scrolled = memory_scrolled();
    let run = |title: Option<&str>, screen: Vec<String>| {
        let (dir, path) = journal_file("stuck-ladder");
        let host = Arc::new(LadderHost::default());
        let mut m = Mock::new(true, vec![busy_screen(), screen]);
        m.title = title.map(str::to_string);
        m.vanish_after = Some(14);
        m.stall_sleep = Some(Duration::from_millis(250));
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(60)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.stuck_after = Duration::from_millis(300);
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let kind = |k: &str| -> Vec<String> {
            records
                .iter()
                .filter(|r| r.line.starts_with(k))
                .map(|r| r.line.clone())
                .collect()
        };
        (
            kind("STUCK"),
            kind("STALLED"),
            kind("EXCEPTION"),
            kind("CLEARED"),
            host.told.lock().unwrap().clone(),
            count(&m, "turn"),
            lines,
        )
    };
    let (stuck, stalled, exception, cleared, told, turns, lines) =
        run(Some("✳ Claude Code"), scrolled.clone());
    assert_eq!(stuck.len(), 1, "rung 1 once: {stuck:#?}\n{lines:#?}");
    assert_eq!(stalled.len(), 1, "rung 2 once: {stalled:#?}\n{lines:#?}");
    assert_eq!(
        exception.len(),
        1,
        "rung 3 once: {exception:#?}\n{lines:#?}"
    );
    assert!(
        exception[0].contains("kind=stalled when=later")
            && exception[0].ends_with("=> Open the tab and type keep going"),
        "{}",
        exception[0]
    );
    assert!(
        cleared
            .iter()
            .any(|l| l.contains("exception=stalled after=") && l.ends_with("the loop ended")),
        "{cleared:#?}"
    );
    assert_eq!(
        told,
        [
            "unread:some",
            "exception:stalled",
            "unread:none",
            "exception:none"
        ],
        "the host hears each rung once, and its end"
    );
    assert_eq!(turns, 0, "the ladder types nothing");
    // CONTROLS.
    for (name, title, screen) in [
        ("an animated title", Some("◐ Claude Code"), scrolled.clone()),
        (
            "a live spinner",
            Some("✳ Claude Code"),
            memory_with_a_live_turn(),
        ),
        ("no title", None, scrolled),
    ] {
        let (stuck, stalled, exception, _, told, _, lines) = run(title, screen);
        assert!(
            stuck.is_empty() && stalled.is_empty() && exception.is_empty() && told.is_empty(),
            "{name} climbs nothing: {stuck:#?} {stalled:#?} {told:#?}\n{lines:#?}"
        );
    }
}

/// THE CANARY'S SECOND FLAVOUR: A DIALOG NO READER NAMES (measured 2026-10-06
/// on Claude Code 2.1.292: the classifier-billing notice, `Enter to continue ·
/// Esc to cancel`, no options, no composer — silent for as long as it stood).
/// The screen reads `idle` with no evidence, its words stand still, and the
/// agent's own title rests: `STUCK` (rung 1) with the rows that teach a reader,
/// `STALLED` and the host's record (rung 2), `EXCEPTION kind=dialog` and the
/// host's exception (rung 3), each ONCE, from the clock the first sight starts
/// (a third of the busy flavour's), and the loop's end withdraws both. The
/// rungs' times are lower bounds and the run ends on the mock's count of
/// waits, never on a clock. Nothing is typed. Run twice: a screen that holds still
/// (the wait's step reaches the clock) and one that never does (a footer that
/// ticks: the read of the wait for the turn reaches it, the wait never does).
/// NEGATIVE CONTROLS: a title that animates, the same words over an idle
/// composer, and a tab whose program is a shell under a stale title say
/// nothing. The words stand still through a ticking countdown and a blinked
/// glyph, and move for a sentence added: `seq` alone is no change.
#[test]
fn a_dialog_no_reader_names_climbs_to_a_record_and_a_line_and_its_end_withdraws_both() {
    // The notice as Claude Code 2.1.292 drew it, measured: no composer frame
    // (one rule over four paragraphs and the footer), the pending tool row above.
    let notice = crate::supervise::prompt::fixtures::screen(
        crate::supervise::prompt::fixtures::AUTO_CLASSIFIER_BILLING_NOTICE_MEASURED,
    );
    let run = |title: &str, program: &'static str, screen: Vec<String>, never_still: bool| {
        let (dir, path) = journal_file("dialog-ladder");
        let host = Arc::new(LadderHost::default());
        let mut m = Mock::new(true, vec![busy_screen(), screen]);
        m.title = Some(title.to_string());
        m.program = program;
        m.vanish_after = Some(14);
        m.stall_sleep = Some(Duration::from_millis(25));
        m.idle_never = never_still;
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(60)
        };
        // The unit is 30 ms: the rungs fall at 30, 90 and 180 ms at the earliest.
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.stuck_after = Duration::from_millis(90);
        });
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        let kind = |k: &str| -> Vec<String> {
            records
                .iter()
                .filter(|r| r.line.starts_with(k))
                .map(|r| r.line.clone())
                .collect()
        };
        (
            kind("STUCK"),
            kind("STALLED"),
            kind("EXCEPTION"),
            kind("CLEARED"),
            host.told.lock().unwrap().clone(),
            count(&m, "turn"),
            lines,
        )
    };
    for never_still in [false, true] {
        let (stuck, stalled, exception, cleared, told, turns, lines) =
            run("✳ Claude Code", "claude", notice.clone(), never_still);
        let said = format!("never_still={never_still}\n{lines:#?}");
        assert_eq!(stuck.len(), 1, "rung 1 once: {stuck:#?}\n{said}");
        assert!(
            stuck[0].contains("no reader here names it")
                && stuck[0].ends_with("Enter to continue · Esc to cancel"),
            "{}",
            stuck[0]
        );
        assert_eq!(stalled.len(), 1, "rung 2 once: {stalled:#?}\n{said}");
        assert_eq!(exception.len(), 1, "rung 3 once: {exception:#?}\n{said}");
        assert!(
            exception[0].contains("kind=dialog when=later")
                && exception[0].ends_with("=> Open the tab and answer the dialog."),
            "{}",
            exception[0]
        );
        assert!(
            cleared
                .iter()
                .any(|l| l.contains("exception=dialog after=") && l.ends_with("the loop ended")),
            "{cleared:#?}"
        );
        assert_eq!(
            told,
            [
                "unread:some",
                "exception:dialog",
                "unread:none",
                "exception:none"
            ],
            "the host hears each rung once, and its end\n{said}"
        );
        assert_eq!(turns, 0, "the canary types nothing");
    }
    // CONTROLS.
    for (name, title, program, over_a_composer) in [
        ("an animated title", "◐ Claude Code", "claude", false),
        ("an idle composer", "✳ Claude Code", "claude", true),
        ("a shell under a stale title", "✳ Claude Code", "zsh", false),
    ] {
        let mut screen = notice.clone();
        if over_a_composer {
            screen.extend(composer("  ? for shortcuts"));
        }
        let (stuck, stalled, exception, _, told, _, lines) = run(title, program, screen, false);
        assert!(
            stuck.is_empty() && stalled.is_empty() && exception.is_empty() && told.is_empty(),
            "{name}: {stuck:#?} {stalled:#?} {exception:#?} {told:#?}\n{lines:#?}"
        );
    }
    // THE WORDS: what stands still is the words, never the seq, a countdown or a
    // glyph, and the clock they keep follows them.
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let screen = |rows: Vec<String>| Screen {
        rows,
        ..Screen::default()
    };
    let at = |rows: &[String], what: &str| {
        rows.iter()
            .position(|r| r.contains(what))
            .unwrap_or_else(|| panic!("{what}"))
    };
    let shown = |glyph: &str, secs: &str| {
        let mut r = notice.clone();
        let (tool, line) = (at(&r, "⏺ Bash"), at(&r, "Nothing breaks"));
        r[tool] = r[tool].replacen('⏺', glyph, 1);
        r[line].push_str(&format!(" ({secs}s)"));
        r
    };
    let mut longer = notice.clone();
    longer.insert(
        at(&notice, "Enter to continue"),
        "One more thing: it needs a login.".to_string(),
    );
    let still = s.dialog_words(&screen(shown("⏺", "12")));
    assert!(still.is_some(), "the notice has words");
    assert_eq!(still, s.dialog_words(&screen(shown("·", "13"))));
    assert_ne!(still, s.dialog_words(&screen(longer.clone())));
    assert_eq!(s.dialog_words(&screen(idle_screen())), None);
    assert_eq!(s.dialog_words(&screen(busy_screen())), None);
    let mut heard = Heard::default();
    s.stuck_rung = super::super::wall_memory::StuckRung::Said;
    s.dialog_note(&screen(shown("⏺", "12")), &mut heard);
    let began = s.stuck_dialog.clone().expect("the notice starts a clock");
    assert_eq!(
        s.stuck_rung,
        super::super::wall_memory::StuckRung::Quiet,
        "what the busy flavour's ladder held ends at a dialog's first sight"
    );
    std::thread::sleep(Duration::from_millis(2));
    s.dialog_note(&screen(shown("·", "13")), &mut heard);
    assert_eq!(
        s.stuck_dialog,
        Some(began.clone()),
        "a tick keeps the clock"
    );
    s.dialog_note(&screen(longer), &mut heard);
    assert!(
        s.stuck_dialog.as_ref().is_some_and(|(_, at)| *at > began.1),
        "other words start it afresh"
    );
    s.stuck_busy_note(&screen(memory_scrolled()), Some(Instant::now()), &mut heard);
    assert_eq!(
        s.stuck_dialog, None,
        "a busy read ends it, background work alone too"
    );
    s.dialog_note(&screen(shown("⏺", "12")), &mut heard);
    s.dialog_note(&screen(idle_screen()), &mut heard);
    assert_eq!(s.stuck_dialog, None, "a screen with no words ends it");
    assert!(
        heard.said.is_empty() && heard.noted.is_empty(),
        "{:?}",
        heard.noted
    );
}

/// THE WORKER'S OWN FINISHED: a turn that ends with `ATERM-TASK-DONE` on its
/// last line gets nothing typed — no continuation, no done check — and the
/// journal says once why (`DONE … rule=finished@v1`).
#[test]
fn a_worker_that_reports_finished_is_left_alone() {
    let (dir, journal) = journal_file("finished");
    let mut m = Mock::new(true, vec![busy_screen(), ended("ATERM-TASK-DONE")]);
    m.turn_releases = Some(1);
    let opts = SuperviseOpts {
        policy: SupervisorConfig::default(),
        journal: Some(journal.clone()),
        ..auto(1, None)
    };
    let (_lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(count(&m, "turn"), 0, "nothing typed: {:#?}", m.requests);
    let done: Vec<String> = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .filter(|l| l.starts_with("DONE "))
        .collect();
    assert_eq!(done.len(), 1, "{done:#?}");
    assert!(
        done[0].contains(&format!("rule={RULE_FINISHED} ")),
        "{done:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A FINISHED REPORT THAT STILL NAMES OPEN WORK through the real supervisor
/// (the owner, 2026-10-16: "stop telling me done if you're not done"): a
/// report drawn as Claude Code draws it — `Still open: …` on its own row,
/// then `ATERM-TASK-DONE` — gets the challenge typed once
/// (`CONTINUED … rule=continue-open-items@v1`), and the reply that ends with
/// the token again ends the task (`DONE … rule=finished@v1`) though it still
/// lists an item: one act in all. A reply that says it is done in words ends
/// it as the check's does, the journal naming the challenge
/// (`DONE … rule=continue-open-items@v1`).
/// NEGATIVE CONTROL: `Nothing left.` over the token ends the task at once,
/// nothing typed.
#[test]
fn a_finished_report_naming_open_work_is_challenged_once_then_ends() {
    let report = |lines: &[&str]| {
        let mut r = vec![format!("⏺ {}", lines[0])];
        r.extend(lines[1..].iter().map(|l| format!("  {l}")));
        r.extend(rows(&["", "✻ Worked for 12m 2s · done 4:24 PM", ""]));
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let open = report(&[
        "Built and tested the parser; committed on main.",
        "Still open: the lexer's e2e suite (not run).",
        FINISHED,
    ]);
    // The reply still lists an item: the job had its challenge, so it ends.
    let again = report(&[
        "Ran the lexer's e2e suite: green, committed.",
        "Left: the docs.",
        FINISHED,
    ]);
    let clean = report(&[
        "Built, tested and landed the parser. Nothing left.",
        FINISHED,
    ]);
    // `releases`: the report's screen sits until a `turn` is sent.
    let run = |tag: &str, screens: Vec<Vec<String>>, releases: Option<usize>| {
        let (dir, journal) = journal_file(tag);
        let mut m = Mock::new(true, screens);
        m.turn_releases = releases;
        m.vanish_after = Some(4);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            journal: Some(journal.clone()),
            ..auto(30, None)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let done: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("DONE "))
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        (count(&m, "turn"), lines, done)
    };
    let (turns, lines, done) = run(
        "open-items",
        vec![busy_screen(), open, busy_screen(), again],
        Some(1),
    );
    assert_eq!(turns, 1, "the challenge alone: {lines:#?}");
    let challenge = open_items_text(&["still open"], None);
    assert!(
        lines.contains(&format!(
            "CONTINUED seq=102 rule={RULE_OPEN_ITEMS} {}",
            clip(&challenge)
        )),
        "{lines:#?}"
    );
    assert_eq!(done.len(), 1, "{done:#?}");
    assert!(
        done[0].starts_with(&format!("DONE seq=104 rule={RULE_FINISHED} ")),
        "{done:#?}"
    );
    // A done reply in words.
    let words = report(&["Ran the lexer's e2e suite: green. All done; nothing left to do."]);
    let open = report(&[
        "Built and tested the parser; committed on main.",
        "Still open: the lexer's e2e suite (not run).",
        FINISHED,
    ]);
    let (turns, lines, done) = run(
        "open-items-words",
        vec![busy_screen(), open, busy_screen(), words],
        Some(1),
    );
    assert_eq!(turns, 1, "the challenge alone: {lines:#?}");
    assert_eq!(done.len(), 1, "{done:#?}");
    assert!(
        done[0].starts_with(&format!("DONE seq=104 rule={RULE_OPEN_ITEMS} ")),
        "{done:#?}"
    );
    // NEGATIVE CONTROL: nothing left.
    let (turns, lines, done) = run("open-items-none", vec![busy_screen(), clean], None);
    assert_eq!(turns, 0, "{lines:#?}");
    assert_eq!(done.len(), 1, "{done:#?}");
    assert!(
        done[0].starts_with(&format!("DONE seq=102 rule={RULE_FINISHED} ")),
        "{done:#?}"
    );
}

/// A PERSON'S OWN EXCHANGE through the real supervisor (the owner,
/// 2026-10-15): a greeting the person's keystroke opened, answered in
/// seconds, gets nothing typed — not the done check its words would get, and
/// no token needed — and the journal says once why (`LEFT …
/// rule=left-to-person@v1`). The person's grace is off, so nothing but the
/// rule holds the act. Negative control: the same turn with no person's
/// stamp (a host that sends none) gets the done check after the back-off.
#[test]
fn a_persons_greeting_is_left_to_them() {
    let greeting = "Hi! What would you like me to work on?";
    let quick = |said: &str| {
        let mut r = rows(&[&format!("⏺ {said}"), ""]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let timing = |s: &mut Session<'_, Mock>| {
        s.set_turn_end_timing(TurnEndTiming {
            short_backoff: Duration::from_millis(30),
            short_backoff_max: Duration::from_millis(60),
            ..TurnEndTiming::default()
        });
    };
    let policy = SupervisorConfig {
        human_grace_s: 0,
        ..SupervisorConfig::default()
    };
    let with_token = format!("{greeting}\n{FINISHED}");
    for said in [greeting, with_token.as_str()] {
        let (dir, journal) = journal_file("persons-exchange");
        let mut m = Mock::new(true, vec![busy_screen(), quick(said)]);
        m.turn_releases = Some(1);
        m.vanish_after = Some(8);
        m.human = vec!["2000"];
        let opts = SuperviseOpts {
            policy: policy.clone(),
            journal: Some(journal.clone()),
            ..auto(1, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, timing);
        assert_eq!(count(&m, "turn"), 0, "{said:?}: {lines:#?}");
        let left: Vec<String> = journal_records(&journal)
            .0
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("LEFT "))
            .collect();
        assert_eq!(left.len(), 1, "{said:?}: {left:#?}");
        assert!(left[0].contains(&format!("rule={RULE_LEFT} ")), "{left:#?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    // Negative control: nobody's stamp.
    let mut m = Mock::new(true, vec![busy_screen(), quick(greeting)]);
    m.turn_releases = Some(1);
    m.vanish_after = Some(8);
    let opts = SuperviseOpts {
        policy,
        ..auto(1, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, timing);
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("rule={RULE_DONE_CHECK} "))),
        "{lines:#?}"
    );
}

/// A PERSON'S QUESTION IS THEIRS THOUGH ITS ANSWER TAKES MINUTES, through the
/// real supervisor (the owner, 2026-10-15: "these auto prompts are for
/// continuously running jobs"): their question, then a three-minute answer
/// that looked things up, gets nothing typed, and the journal says once why.
/// Negative control: a turn they opened that works past `EXCHANGE_WORK` is a
/// job, and is continued.
#[test]
fn a_persons_question_answered_at_length_is_left_to_them() {
    let answered = |worked: &str| {
        let mut r = rows(&[
            "❯ what do you think?",
            "",
            "  Ran 2 shell commands",
            "",
            "⏺ Keep the token: it costs nothing and survives a restart.",
            "",
            &format!("✻ Worked for {worked} · done 4:24 PM"),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    // Their keystroke sent the question, just before the turn's work.
    for (screen, human, typed) in [
        (answered("3m 2s"), "185000", false),
        (answered("12m 2s"), "725000", true),
    ] {
        let (dir, journal) = journal_file("persons-question");
        let mut m = Mock::new(true, vec![busy_screen(), screen]);
        m.turn_releases = Some(1);
        m.human = vec![human];
        // An act that must come ends the watch, a minute its hang detector;
        // one that must not is looked for for a second.
        let max = if typed {
            m.exit_when = Some(|m| m.turns > 0);
            60
        } else {
            m.vanish_after = Some(8);
            1
        };
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                ..SupervisorConfig::default()
            },
            journal: Some(journal.clone()),
            ..auto(max, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        assert_eq!(count(&m, "turn") > 0, typed, "{typed}: {lines:#?}");
        let left = journal_records(&journal)
            .0
            .into_iter()
            .filter(|r| r.line.starts_with("LEFT "))
            .count();
        assert_eq!(left, usize::from(!typed), "{typed}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// CLAUDE CODE'S OWN HOUSEKEEPING IS NO TURN (2026-10-15, two of the owner's
/// tabs typed `keep going` an hour after their finished reports): a spinner,
/// then a screen whose last thing said is `⏺ Compacted while idle, before the
/// prompt cache expired`. Nothing is typed where the last turn left nothing
/// to type: after a FINISHED report (the MEASURED screen) whether the loop
/// watched the compaction or attaches after it (a seamless update, a fault),
/// after a person's own exchange (their greeting, answered in seconds), and at
/// a question held for a person with answers off. The person's grace
/// is off and the back-off short, so nothing but the reading holds an act.
/// Negative controls, each typed: a message of the worker's in the notice's
/// place (a turn that ended), and an attach to a report that did not finish.
#[test]
fn claude_codes_idle_compaction_is_no_turn() {
    use aterm_phase::prompt::fixtures::{IDLE_COMPACTED_MEASURED, screen};
    let measured = screen(IDLE_COMPACTED_MEASURED);
    let swap = |rows: &[String], from: &str, to: &str| -> Vec<String> {
        let at = rows.iter().position(|r| r.starts_with(from)).expect(from);
        let mut out = rows.to_vec();
        out[at] = to.to_string();
        out
    };
    let notice = "⏺ Compacted while idle";
    let worker = "⏺ Compacted the build logs into one file.";
    let ask = "Shall I drop the old table?";
    let greeting = "Hi! What would you like me to work on?";
    let framed = |body: &[&str]| {
        let mut r = rows(body);
        r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
        r
    };
    let greeted = framed(&[&format!("⏺ {greeting}"), ""]);
    // The worker's words, then what the compaction appended under them.
    let compacted = |said: &str| {
        framed(&[
            &format!("⏺ {said}"),
            "",
            "  ⎿  Skills restored (workflow-authoring)",
            "",
            "⏺ Compacted while idle, before the prompt cache expired",
            "",
        ])
    };
    let (left, held) = (compacted(greeting), compacted(ask));
    let run = |screens: Vec<Vec<String>>| {
        vec![
            busy_screen(),
            screens[0].clone(),
            busy_screen(),
            screens[1].clone(),
        ]
    };
    let arms = [
        (
            "watched",
            run(vec![ended(FINISHED), measured.clone()]),
            true,
            false,
        ),
        (
            "watched, a worker's turn",
            run(vec![ended(FINISHED), swap(&measured, notice, worker)]),
            true,
            true,
        ),
        ("attached", vec![measured.clone()], true, false),
        (
            "attached, unfinished",
            vec![swap(
                &measured,
                "  ATERM-TASK-DONE",
                "  Next I port the reader.",
            )],
            true,
            true,
        ),
        (
            "left",
            run(vec![greeted.clone(), left.clone()]),
            true,
            false,
        ),
        (
            "left, a worker's turn",
            run(vec![greeted.clone(), swap(&left, notice, worker)]),
            true,
            true,
        ),
        ("held", run(vec![ended(ask), held.clone()]), false, false),
        (
            "held, a worker's turn",
            run(vec![ended(ask), swap(&held, notice, worker)]),
            false,
            true,
        ),
    ];
    let mut wrong = Vec::new();
    for (arm, screens, answers, typed) in arms {
        let last = screens.len() - 1;
        let mut m = Mock::new(true, screens);
        m.turn_releases = Some(last);
        // The greeting's turn was opened by a person's keystroke.
        if arm.starts_with("left") {
            m.human = vec!["2000"];
        }
        // An act that must come ends the watch, a minute its hang detector;
        // one that must not is looked for until the session goes.
        let max = if typed {
            m.exit_when = Some(|m| m.turns > 0);
            60
        } else {
            m.vanish_after = Some(8);
            1
        };
        let opts = SuperviseOpts {
            policy: SupervisorConfig {
                human_grace_s: 0,
                answer_questions: answers,
                ..SupervisorConfig::default()
            },
            ..auto(max, None)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                short_backoff: Duration::from_millis(30),
                short_backoff_max: Duration::from_millis(60),
                ..TurnEndTiming::default()
            });
        });
        if (count(&m, "turn") > 0) != typed {
            wrong.push(format!("{arm}: typed={}: {lines:#?}", !typed));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

// ------------------------------------------------------------------------
// Another account in the loop (`docs/DESIGN-claude-accounts-2026-10-01.md`
// §6): the host's advice, its relaunch, the tells. Labels are the design's.
// ------------------------------------------------------------------------

use crate::supervise::policy::exception::RENEW_STEP;
use crate::supervise::policy::turn_end::{AccountAt, RULE_ACCOUNT, Restart};

/// A host that keeps a roster, as a test drives it: what it advises
/// ([`IdleHost::account`]) and what its relaunch answers
/// ([`IdleHost::restart`]) are scripted — each ask takes the next entry, the
/// last for ever after — and everything the loop asked or told it is kept,
/// IN ORDER. It owns no turn end, and takes no step of its own but the
/// relaunch's last one, where a test owes it (`carry`).
#[derive(Debug, Default)]
struct AccountHost {
    advice: std::sync::Mutex<VecDeque<Option<AccountAdvice>>>,
    advised: std::sync::Mutex<Option<AccountAdvice>>,
    answers: std::sync::Mutex<VecDeque<&'static str>>,
    answered: std::sync::Mutex<Option<&'static str>>,
    said: std::sync::Mutex<Vec<String>>,
    /// [`IdleHost::account_stamp`], as a test moves it.
    stamp: std::sync::atomic::AtomicU64,
    /// The stamp moves by itself right after the FIRST ask for advice: news
    /// that lands while the session waits on that advice.
    news_after_first_ask: std::sync::atomic::AtomicBool,
    /// A relaunched agent's last step is owed: the host asks for the next
    /// idle point and takes it there ([`IdleHost::at_idle`]), once.
    carry: std::sync::atomic::AtomicBool,
    /// The word that step answers; `None`: `done:resumed`, what the relaunch
    /// of an account move answers (`relaunch::carried_quietly`: nothing
    /// typed). `continued` is a carry-on typed, what it answered before.
    step: std::sync::Mutex<Option<&'static str>>,
    /// The step is owed once a relaunch answers `adopted`, as a window's
    /// host parks it (`account_move`). Off, a test owes it by hand.
    parks: std::sync::atomic::AtomicBool,
    /// What the loop told of its exceptions ([`IdleHost::exception`]), in
    /// order, each with how many entries [`Self::said`] held then:
    /// `exception:<kind>:<when>`, `:escalated` where the loop's own
    /// escalation carries it, or `exception:none`.
    told: std::sync::Mutex<Vec<(String, usize)>>,
    /// [`IdleHost::login_label`]: the session's renewable login, as a test
    /// sets it.
    login_label: std::sync::Mutex<Option<String>>,
    /// [`IdleHost::login_superseded`], as a test sets it; and how many
    /// times the loop asked it.
    login_superseded: std::sync::atomic::AtomicBool,
    superseded_asked: std::sync::atomic::AtomicUsize,
    /// The owner acts on the first exception told that asks for the
    /// renewal ([`RENEW_STEP`]), from another terminal: the stamp moves,
    /// once, and with `Some(true)` the process is then on the login the
    /// renewal replaced ([`Self::login_superseded`]); `Some(false)` is news
    /// of something else, the process still on its row's own login.
    renews_when_asked: std::sync::Mutex<Option<bool>>,
    /// The renewal lands right after the host's next answer that the
    /// process holds its row's own login, before the loop reads anything
    /// else: the stamp moves, and the process is on the login it replaced.
    renews_after_answer: std::sync::atomic::AtomicBool,
}

impl AccountHost {
    fn scripted(advice: &[Option<AccountAdvice>], answers: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            advice: std::sync::Mutex::new(advice.iter().cloned().collect()),
            answers: std::sync::Mutex::new(answers.iter().copied().collect()),
            ..Self::default()
        })
    }

    /// The host advises a move to `to` at every point, and its relaunch
    /// answers `answer`.
    fn moving(to: &str, answer: &'static str) -> Arc<Self> {
        Self::scripted(
            &[Some(AccountAdvice::Move { to: to.to_string() })],
            &[answer],
        )
    }

    /// Everything said to it that starts with `prefix`, in order.
    fn said(&self, prefix: &str) -> Vec<String> {
        self.said
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<String> {
        self.said.lock().unwrap().clone()
    }

    /// What the loop told of its exceptions, in order, without the counts.
    fn told(&self) -> Vec<String> {
        self.told
            .lock()
            .unwrap()
            .iter()
            .map(|(word, _)| word.clone())
            .collect()
    }
}

impl IdleHost for AccountHost {
    fn wants(&self) -> bool {
        self.carry.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        if !self.carry.swap(false, Ordering::SeqCst) {
            return None;
        }
        self.said.lock().unwrap().push("carry-on".to_string());
        let step = self.step.lock().unwrap().unwrap_or("done:resumed");
        Some(HostStep {
            line: format!("carry-on step={step}"),
            moved: crate::harness::upgrade_drive::moved(step),
            typed: crate::harness::upgrade_drive::typed(step),
            repeat: false,
        })
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn can_restart(&self) -> bool {
        true
    }
    fn restart(&self, why: &Restart) -> Option<String> {
        let what = match why {
            Restart::Account { to, .. } => format!("restart:{}:{to}", why.word()),
            other => format!("restart:{}", other.word()),
        };
        self.said.lock().unwrap().push(what);
        let mut last = self.answered.lock().unwrap();
        if let Some(next) = self.answers.lock().unwrap().pop_front() {
            *last = Some(next);
        }
        if *last == Some("adopted") && self.parks.load(Ordering::SeqCst) {
            self.carry.store(true, Ordering::SeqCst);
        }
        last.map(str::to_string)
    }
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let at = match point {
            AccountPoint::Wall { kind } => format!("account:wall:{}", kind.name()),
            AccountPoint::LoginEnded => "account:login-ended".to_string(),
            AccountPoint::TurnEnd => "account:turn-end".to_string(),
        };
        self.said.lock().unwrap().push(at);
        let mut last = self.advised.lock().unwrap();
        if let Some(next) = self.advice.lock().unwrap().pop_front() {
            *last = next;
        }
        if self.news_after_first_ask.swap(false, Ordering::SeqCst) {
            self.stamp.fetch_add(1, Ordering::SeqCst);
        }
        last.clone()
    }
    fn account_stamp(&self) -> u64 {
        self.stamp.load(Ordering::SeqCst)
    }
    fn login_label(&self) -> Option<String> {
        self.login_label.lock().unwrap().clone()
    }
    fn login_superseded(&self) -> bool {
        self.superseded_asked.fetch_add(1, Ordering::SeqCst);
        let superseded = self.login_superseded.load(Ordering::SeqCst);
        if !superseded && self.renews_after_answer.swap(false, Ordering::SeqCst) {
            self.login_superseded.store(true, Ordering::SeqCst);
            self.stamp.fetch_add(1, Ordering::SeqCst);
        }
        superseded
    }
    fn login_notice(&self, days: u32) {
        self.said.lock().unwrap().push(format!("notice:{days}"));
    }
    fn exception(&self, standing: Option<&crate::supervise::policy::exception::Exception>) {
        let word = standing.map_or_else(
            || "exception:none".to_string(),
            |e| {
                format!(
                    "exception:{}:{}{}",
                    e.kind.word(),
                    e.when.word(),
                    if e.escalated { ":escalated" } else { "" }
                )
            },
        );
        let at = self.said.lock().unwrap().len();
        self.told.lock().unwrap().push((word, at));
        if standing.is_some_and(|e| e.action.starts_with(RENEW_STEP))
            && let Some(supersedes) = self.renews_when_asked.lock().unwrap().take()
        {
            if supersedes {
                self.login_superseded.store(true, Ordering::SeqCst);
            }
            self.stamp.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn account_served(&self) {
        self.said.lock().unwrap().push("served".to_string());
    }
    fn account_answered(&self, bad: AccountBad) {
        self.said.lock().unwrap().push(
            match bad {
                AccountBad::Wall => "answered:wall",
                AccountBad::SignedOut => "answered:signed-out",
            }
            .to_string(),
        );
    }
}

/// 2026-09-17T17:05:00-07:00: three minutes after [`limited_screen`]'s done
/// row, its `7:30pm` reset still ahead.
const ACCOUNT_NOW: i64 = TEST_NOW + 8 * 3600 + 10 * 60;

/// One watch over `screens` under `host`, its approval ledger at `ledger`:
/// the lines, the server's record and the journal's lines.
fn account_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    host: &Arc<AccountHost>,
    ledger: &Path,
) -> (Vec<String>, Mock, Vec<String>) {
    account_run_with(tag, screens, host, ledger, |_| {})
}

/// [`account_run`] over a server `setup` changes further.
fn account_run_with(
    tag: &str,
    screens: Vec<Vec<String>>,
    host: &Arc<AccountHost>,
    ledger: &Path,
    setup: impl FnOnce(&mut Mock),
) -> (Vec<String>, Mock, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(true, screens);
    m.vanish_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(5));
    setup(&mut m);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        ..hosted(3)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_approval_ledger(Some(ledger.to_path_buf()));
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, m, records.into_iter().map(|r| r.line).collect())
}

/// AT A WALL the host's advice to move is ITS RELAUNCH: asked once, the label
/// its only name — `RESTARTED seq=<n> rule=account-rotate@v1 relaunch under
/// account work-2 (…)`, a `typed` ledger row a later loop reads back as a
/// move made and still to be answered — and nothing is typed into the agent
/// at the wall. A relaunch that must wait (`wait:changed`: the agent's own
/// jobs) HOLDS the point, `WAITING … restart:account waits: …`, nothing
/// typed. One that can never be made (`refused:line`) is written down
/// (`skipped`), not asked again, and the wall is waited out as it would be.
/// NEGATIVE CONTROL: a host that advises nothing is asked for no relaunch,
/// and the wall is waited out the same way.
#[test]
fn a_wall_with_an_account_to_move_to_is_the_hosts_relaunch() {
    use crate::supervise::approvals::{account_moves, open_account_move, open_wall};
    let wall = || vec![busy_screen(), limited_screen()];

    // Made.
    let (dir, ledger) = ledger_at("account-made");
    let host = AccountHost::moving("work-2", "adopted");
    let (lines, m, _) = account_run("te-account-made", wall(), &host, &ledger);
    assert_eq!(
        host.said("restart"),
        ["restart:account:work-2"],
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.ends_with(
                "rule=account-rotate@v1 relaunch under account work-2 (from the limit its turn \
                 ended on; the same conversation)"
            )),
        "{lines:#?}"
    );
    assert_eq!(
        count(&m, "turn"),
        0,
        "nothing typed at the wall: {:#?}",
        m.requests
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:#?}", m.requests);
    let moves = account_moves(&ledger, None);
    assert_eq!(moves.made_ms.len(), 1, "{moves:?}");
    assert!(!moves.unmade);
    let open = open_account_move(&ledger, None).expect("the move, still to be answered");
    assert_eq!((open.to.as_str(), open.at), ("work-2", AccountAt::Wall));
    assert_eq!(
        open_wall(&ledger, None),
        None,
        "the session left that wall's account: its wait is over"
    );
    let text = std::fs::read_to_string(&ledger).expect("the ledger");
    assert!(
        text.contains("\"rule_id\":\"account-rotate@v1\",\"decision\":\"typed\"")
            && text.contains("(account move: to=work-2 at=wall)"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Not yet: the point is held.
    let (dir, ledger) = ledger_at("account-waits");
    let host = AccountHost::moving("work-2", "wait:changed");
    let (lines, m, journal) = account_run("te-account-waits", wall(), &host, &ledger);
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("restart:account waits: wait:changed")),
        "{journal:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("RESTARTED")),
        "{lines:#?}"
    );
    assert_eq!(
        account_moves(&ledger, None),
        Default::default(),
        "nothing made, nothing refused"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // Never: written down, the wall waited out.
    let (dir, ledger) = ledger_at("account-refused");
    let host = AccountHost::moving("work-2", "refused:line");
    let (lines, m, journal) = account_run("te-account-refused", wall(), &host, &ledger);
    assert_eq!(
        host.said("restart"),
        ["restart:account:work-2"],
        "asked once: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("RESTARTED")),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    let moves = account_moves(&ledger, None);
    assert!(
        moves.unmade && moves.made_ms.is_empty() && moves.open.is_none(),
        "{moves:?}"
    );
    assert!(
        open_wall(&ledger, None).is_some(),
        "the wall still stands, and is still waited on"
    );
    let waited: Vec<&String> = journal
        .iter()
        .filter(|l| l.starts_with("WAITING ") && l.contains("usage-session resets"))
        .collect();
    assert_eq!(waited.len(), 1, "{journal:#?}");
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: no advice — no relaunch asked, the same wait.
    let (dir, ledger) = ledger_at("account-none");
    let host = AccountHost::scripted(&[None], &["adopted"]);
    let (lines, m, journal) = account_run("te-account-none", wall(), &host, &ledger);
    assert!(host.said("restart").is_empty(), "{lines:#?}");
    assert_eq!(host.said("account"), ["account:wall:usage-session"]);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert_eq!(account_moves(&ledger, None), Default::default());
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.starts_with("WAITING ") && l.contains("usage-session resets"))
            .count(),
        1,
        "{journal:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A MOVE AHEAD OF A WALL THE HOST CANNOT MAKE YET (`wait:changed`: the
/// agent's own background work runs, and the relaunch would end it) NEVER
/// HOLDS THE POINT: it is deferred as the model's return is — the turn's own
/// continuation is typed, `DEFERRED … restart:account-ahead` journaled, and
/// the move is asked again at the next idle point. Held, as a move AT a wall
/// is ([`a_wall_with_an_account_to_move_to_is_the_hosts_relaunch`]), a tab
/// that always has a job running would never be continued again. NEGATIVE
/// CONTROL: a move the host makes types nothing at the point it was made —
/// the relaunched agent's first point is the next decision
/// ([`a_move_ahead_of_a_wall_types_nothing_and_its_point_is_decided_as_with_no_move`]).
#[test]
fn a_move_ahead_the_host_defers_never_holds_the_continuation() {
    let end = || vec![busy_screen(), ended("Stage 3 done; the suite is green.")];
    let (dir, ledger) = ledger_at("account-ahead-waits");
    let host = AccountHost::moving("work-2", "wait:changed");
    let (lines, m, journal) = account_run("te-account-ahead-waits", end(), &host, &ledger);
    assert_eq!(
        host.said("restart"),
        ["restart:account-ahead:work-2"],
        "{lines:#?}"
    );
    assert!(
        m.requests.contains(&continue_request("keep going")),
        "the continuation goes ahead: {:#?}",
        m.requests
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("CONTINUED")).count(),
        1,
        "{lines:#?}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("DEFERRED ")
            && l.contains("restart:account-ahead waits for a quiet moment (wait:changed)")),
        "{journal:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let (dir, ledger) = ledger_at("account-ahead-made");
    let host = AccountHost::moving("work-2", "adopted");
    let (lines, m, _) = account_run("te-account-ahead-made", end(), &host, &ledger);
    assert_eq!(host.said("restart"), ["restart:account-ahead:work-2"]);
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.ends_with(
                "rule=account-rotate@v1 relaunch under account work-2 (at a turn end; the same \
                 conversation)"
            )),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert_eq!(
        crate::supervise::approvals::open_account_move(&ledger, None).map(|o| o.at),
        Some(AccountAt::Ahead)
    );
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: no advice, the plain continuation and no relaunch.
    let (dir, ledger) = ledger_at("account-ahead-none");
    let host = AccountHost::scripted(&[None], &["adopted"]);
    let (_, m, _) = account_run("te-account-ahead-none", end(), &host, &ledger);
    assert!(host.said("restart").is_empty());
    assert!(m.requests.contains(&continue_request("keep going")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A MOVE AHEAD OF A WALL TYPES NOTHING, AND ITS POINT IS DECIDED AS WITH NO
/// MOVE (the accounts design v2, §3.5): the relaunch of an account move
/// answers `done:resumed` at the relaunched agent's first idle point
/// (`relaunch::carried_quietly`), a step that moves nothing, so the loop
/// decides that point itself — and what it types is exactly what the same
/// session was typed with no roster at all: the same decision, typed once.
/// NEGATIVE CONTROL: the same moved run whose host step TYPES (`continued`,
/// the relaunch's carry-on before 2026-10-15) takes the point, and what the
/// session is typed is no longer the decision it had with no move.
#[test]
fn a_move_ahead_of_a_wall_types_nothing_and_its_point_is_decided_as_with_no_move() {
    let done = "Stage 3 done; the suite is green.";
    // The third screen is the relaunched agent at its prompt, its
    // conversation repainted.
    let screens = || vec![busy_screen(), ended(done), ended(done)];
    let turns = |m: &Mock| -> Vec<String> {
        m.requests
            .iter()
            .filter(|r| r.starts_with("turn"))
            .cloned()
            .collect()
    };
    let run = |tag: &str, host: &Arc<AccountHost>| {
        let (dir, ledger) = ledger_at(tag);
        // The cursor at the caret, as a real agent parks it: the reader
        // vouches for the idle prompt the host's step is taken at.
        let (lines, m, journal) =
            account_run_with(&format!("te-{tag}"), screens(), host, &ledger, |m| {
                m.vanish_after = Some(4);
                m.cursor_on_caret = true;
            });
        let _ = std::fs::remove_dir_all(&dir);
        (turns(&m), format!("{lines:#?}\n{journal:#?}"), journal)
    };
    let moving = |step: Option<&'static str>| {
        let host = AccountHost::moving("work-2", "adopted");
        host.parks.store(true, Ordering::SeqCst);
        *host.step.lock().unwrap() = step;
        host
    };

    // No roster's advice: the turn's own continuation.
    let baseline = AccountHost::scripted(&[None], &["adopted"]);
    let (unmoved, log, _) = run("account-ahead-baseline", &baseline);
    assert!(baseline.said("restart").is_empty(), "{log}");
    assert_eq!(unmoved, [continue_request("keep going")], "{log}");

    // The move: made, its last step taken at the relaunched prompt with
    // nothing typed, and the point decided as with no move.
    let host = moving(None);
    let (moved, log, journal) = run("account-ahead-quiet", &host);
    assert_eq!(
        host.said("restart"),
        ["restart:account-ahead:work-2"],
        "{log}"
    );
    assert_eq!(host.said("carry-on"), ["carry-on"], "{log}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("HOST seq=") && l.ends_with(" carry-on step=done:resumed")),
        "{log}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("RESTARTED seq=")
            && l.ends_with("(at a turn end; the same conversation)")),
        "{log}"
    );
    assert_eq!(moved, unmoved, "the same decision, typed once: {log}");

    // NEGATIVE CONTROL: a host step that types takes the point.
    let host = moving(Some("continued"));
    let (carried, log, _) = run("account-ahead-carried", &host);
    assert_eq!(host.said("carry-on"), ["carry-on"], "{log}");
    assert!(carried.is_empty(), "the host's turn took the point: {log}");
    assert_ne!(carried, unmoved, "{log}");
}

/// A session at `rows`, its point, and the options a hosted loop has under
/// `host`.
fn account_point_of(rows: Vec<String>, phase: Phase) -> Turn {
    Turn {
        phase,
        screen: Screen {
            rows,
            seq: 102,
            cursor_col: 2,
            ..Screen::default()
        },
        timed_out: false,
    }
}

fn account_opts(host: &Arc<AccountHost>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..SuperviseOpts::hosted()
    }
}

/// THE HOST IS ASKED FOR ITS ADVICE ONCE A DECISION, however many times the
/// reading of the point is built: the decision reads it, a relaunch that
/// could not be made reads it again, and so does the act typed in its place
/// — one ask (a host that reads its roster and state file is not asked six
/// times). A point decided AGAIN asks again: that is how a session waiting
/// on every account learns one is usable. NEGATIVE CONTROL: the relaunch is
/// asked of the host as often as the decision is made, so the single ask is
/// not a decision that was skipped.
#[test]
fn the_host_is_asked_for_its_advice_once_a_decision() {
    let host = AccountHost::scripted(
        &[Some(AccountAdvice::Move {
            to: "work-2".to_string(),
        })],
        &["wait:changed"],
    );
    let opts = account_opts(&host);
    let rows = limited_screen();
    let mut m = Mock::new(true, vec![rows.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let point = account_point_of(
        rows,
        Phase::Limited {
            message: String::new(),
            reset: None,
        },
    );
    for round in 1..=3 {
        let action = s
            .turn_end_at_point(
                &point,
                (round == 1).then_some(Duration::from_secs(180)),
                &opts,
            )
            .expect("a decision");
        assert!(
            matches!(
                &action,
                TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Wall }, rule_id, .. }
                    if to == "work-2" && *rule_id == RULE_ACCOUNT
            ),
            "{action:?}"
        );
        s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
            .expect("the relaunch waits, it does not fail");
        assert_eq!(host.said("account").len(), round, "{:?}", host.all());
        assert_eq!(host.said("restart").len(), round, "{:?}", host.all());
    }
    assert!(
        s.turn_end_due.is_some(),
        "the point is held on the back-off"
    );
}

/// A WAIT THAT STANDS ON THE HOST'S ADVICE IS DECIDED AGAIN WHEN THE HOST'S
/// STAMP MOVES ([`IdleHost::account_stamp`], [`Session::account_edge`]; the
/// accounts design §6: the owner's `accounts use`, an account proven or
/// struck by another session — none of which moves this session's screen). A
/// session whose host advised nothing waits its wall out; the stamp moving is
/// an edge, ONCE, and the point decided again asks the host again — whose
/// advice is now the move. NEGATIVE CONTROLS: the stamp as it stood when the
/// advice was asked is no edge however often it is read; nor is a moved stamp
/// with no wait standing (an escalated point has nothing to decide again), nor
/// one at a point that never asked the host for advice.
#[test]
fn a_wait_on_the_hosts_advice_is_decided_again_when_its_stamp_moves() {
    let host = AccountHost::scripted(
        &[
            None,
            Some(AccountAdvice::Move {
                to: "work-2".to_string(),
            }),
        ],
        &["adopted"],
    );
    let opts = account_opts(&host);
    let rows = limited_screen();
    let mut m = Mock::new(true, vec![rows.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let point = account_point_of(
        rows,
        Phase::Limited {
            message: String::new(),
            reset: None,
        },
    );
    // NEGATIVE CONTROL: before any decision the host was asked nothing.
    host.stamp.store(7, Ordering::SeqCst);
    s.set_due(
        Instant::now() + Duration::from_secs(3600),
        "usage-session resets",
    );
    assert!(!s.account_edge(&opts), "no advice asked: nothing to re-ask");
    s.turn_end_due = None;

    // Decided once: no advice, so the wall's own wait.
    let action = s
        .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
        .expect("a decision");
    assert!(
        matches!(action, TurnEndAction::WaitUntil { .. }),
        "{action:?}"
    );
    s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
        .expect("the wait is set");
    assert!(s.turn_end_due.is_some(), "the wall is waited out");
    assert_eq!(host.said("account").len(), 1, "{:?}", host.all());
    // NEGATIVE CONTROL: the stamp as the advice was asked is no edge.
    assert!(!s.account_edge(&opts));
    assert!(!s.account_edge(&opts));

    // The accounts moved: an edge, once.
    host.stamp.store(8, Ordering::SeqCst);
    assert!(s.account_edge(&opts), "the stamp moved under a wait");
    assert!(!s.account_edge(&opts), "the same stamp is no second edge");
    // …and the point decided again asks again: the move.
    let again = s
        .turn_end_at_point(&point, None, &opts)
        .expect("a decision");
    assert!(
        matches!(
            &again,
            TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Wall }, .. }
                if to == "work-2"
        ),
        "{again:?}"
    );
    assert_eq!(host.said("account").len(), 2, "{:?}", host.all());

    // NEGATIVE CONTROL: a moved stamp with no wait standing decides nothing.
    host.stamp.store(9, Ordering::SeqCst);
    s.turn_end_due = None;
    assert!(!s.account_edge(&opts), "nothing is waited on");
}

/// THE SEAT OF THAT EDGE IS THE LOOP'S WAIT ([`Session::wait_for_next`]): a
/// session at a usage limit whose host advised nothing, and whose stamp moves
/// while it waits, is decided again INSIDE the wait — the host is asked a
/// second time and its relaunch is made, long before the limit's reset.
/// NEGATIVE CONTROL: the same host with no news is asked once and makes no
/// relaunch (the wall is waited out, as with no roster).
#[test]
fn news_of_the_accounts_ends_a_sessions_wait_at_its_limit() {
    let wall = || vec![busy_screen(), limited_screen()];
    let script = || {
        AccountHost::scripted(
            &[
                None,
                Some(AccountAdvice::Move {
                    to: "work-2".to_string(),
                }),
            ],
            &["adopted"],
        )
    };

    let (dir, ledger) = ledger_at("account-news");
    let host = script();
    host.news_after_first_ask.store(true, Ordering::SeqCst);
    let (lines, _, journal) = account_run("te-account-news", wall(), &host, &ledger);
    assert_eq!(
        host.said("account").len(),
        2,
        "asked again inside the wait: {:?}\n{journal:#?}",
        host.all()
    );
    assert_eq!(
        host.said("restart"),
        ["restart:account:work-2"],
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("RESTARTED seq=")),
        "{lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: no news, no second ask, no relaunch.
    let (dir, ledger) = ledger_at("account-no-news");
    let host = script();
    let (lines, _, _) = account_run("te-account-no-news", wall(), &host, &ledger);
    assert_eq!(host.said("account").len(), 1, "{:?}", host.all());
    assert!(host.said("restart").is_empty(), "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A LOST LOGIN WITH NO ACCOUNT LEFT, SIGNED IN AGAIN BY THE OWNER: the
/// session's own move to `pool-a` met a lost login, the host struck it and
/// advises nothing at all, so the loop waits with no end anyone knows and
/// tells its host the owner is needed NOW — `Run aterm harness accounts
/// login pool-a.`, carried by no escalation of the loop's (no badge, no
/// `/login`), so the window asks for it on the glass. The owner's sign-in is
/// the accounts' news inside that wait: the point is decided again, the host
/// advises the move back onto `pool-a`, the ask ends there, and the
/// relaunched agent's first point gets the wall's resume. NEGATIVE CONTROLS:
/// a host that advises a wait with an end raises nothing Now-class; and a
/// session with no move behind it keeps the old path — `/login` typed, the
/// loop's own badge, and no Now the window would ask for.
#[test]
fn a_lost_login_with_no_account_left_asks_its_sign_in_until_the_move_back() {
    use crate::supervise::approvals::{ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason};
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    // The relaunched agent's resumed conversation: its last row is the old
    // process's notice, and it sits there until the resume is typed.
    let mut resumed = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    resumed.extend(composer("  ? for shortcuts"));
    // The turn asked under `pool-a` meets the lost login (read again where
    // the news decides it again), the relaunch, the resume taken.
    let screens = || {
        vec![
            busy_screen(),
            login.clone(),
            login.clone(),
            resumed.clone(),
            busy_screen(),
            ended("Carried on: stage 3 is done."),
        ]
    };
    let moved = || {
        let now_ms = crate::supervise::journal::unix_ms();
        Row {
            rule_id: RULE_ACCOUNT,
            outcome: Outcome::Typed,
            command: &format!("{ACCOUNT_MOVE_COMMAND}pool-a"),
            reason: &account_switch_reason("pool-a", AccountAt::Wall),
            box_seq: 90,
        }
        .to_json(now_ms - 60_000, None)
    };
    // The turn asked under `pool-a`, before this loop: the wall's resume.
    let resumed = || {
        let now_ms = crate::supervise::journal::unix_ms();
        Row {
            rule_id: crate::supervise::policy::turn_end::RULE_LIMIT_RESUME,
            outcome: Outcome::Typed,
            command: "keep going",
            reason: "the turn-end policy",
            box_seq: 91,
        }
        .to_json(now_ms - 50_000, None)
    };
    let run = |tag: &str, seeded: bool, host: &Arc<AccountHost>| {
        let (dir, ledger) = ledger_at(tag);
        if seeded {
            std::fs::write(&ledger, moved() + "\n" + &resumed() + "\n").expect("the ledger");
        }
        let (lines, m, journal) =
            account_run_with(&format!("te-{tag}"), screens(), host, &ledger, |m| {
                m.vanish_after = Some(4);
                m.cursor_on_caret = true;
                m.turn_releases = Some(3);
            });
        let _ = std::fs::remove_dir_all(&dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        (lines, m, journal, log)
    };

    // Stuck, then signed in again: the ask stands until the move back. (The
    // host advises nothing after it: the relaunched session's own turn end.)
    let host = AccountHost::scripted(
        &[
            None,
            Some(AccountAdvice::Move {
                to: "pool-a".to_string(),
            }),
            None,
        ],
        &["adopted"],
    );
    host.news_after_first_ask.store(true, Ordering::SeqCst);
    host.parks.store(true, Ordering::SeqCst);
    let (lines, m, journal, log) = run("account-stuck-login", true, &host);
    assert_eq!(
        host.told(),
        ["exception:login:now", "exception:none"],
        "{log}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("EXCEPTION ")
            && l.contains("kind=login when=now")
            && l.ends_with("=> Run aterm harness accounts login pool-a.")),
        "{log}"
    );
    assert_eq!(
        host.said("restart"),
        ["restart:account:pool-a"],
        "the move back onto the login signed in again: {log}"
    );
    let told = host.told.lock().unwrap().clone();
    let said = host.all();
    let restart_at = said
        .iter()
        .position(|s| s == "restart:account:pool-a")
        .expect("the move");
    assert!(
        told[0].1 <= restart_at && told[1].1 <= restart_at,
        "the ask stood until the move back was asked: {log}"
    );
    assert!(
        said[..told[1].1]
            .iter()
            .filter(|s| *s == "account:wall:auth")
            .count()
            == 2,
        "it ended at the point the news decided again: {log}"
    );
    let restarted = lines
        .iter()
        .position(|l| l.starts_with("RESTARTED seq="))
        .expect("relaunched");
    assert!(
        lines[restarted..]
            .iter()
            .any(|l| l.starts_with("CONTINUED seq=") && l.contains("rule=usage-resume@v1")),
        "the wall's resume, after the move: {log}"
    );
    assert_eq!(host.said("served"), ["served"], "served under it: {log}");
    assert!(
        !m.requests.iter().any(|r| r.contains("/login")),
        "no /login: {log}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "no badge of the loop's: {log}"
    );

    // NEGATIVE CONTROL: a wait with an end raises nothing Now-class.
    let host = AccountHost::scripted(
        &[Some(AccountAdvice::Wait {
            until: Instant::now() + Duration::from_secs(30 * 60),
        })],
        &["adopted"],
    );
    let (_, _, journal, log) = run("account-stuck-login-wait", true, &host);
    assert!(host.told().is_empty(), "{log}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("another account can be used then")),
        "{log}"
    );

    // NEGATIVE CONTROL: no move behind the session — the old path.
    let host = AccountHost::scripted(&[None], &["adopted"]);
    let (lines, m, _, log) = run("account-stuck-login-none", false, &host);
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with(" rule=auth-login@v1 /login")),
        "{log}"
    );
    assert!(
        m.requests.iter().any(|r| r.starts_with(
            "meta set attention owner=supervisor claude wall: Not logged in · Please run /login"
        )),
        "the loop's own escalation: {log}"
    );
    assert_eq!(
        host.told(),
        ["exception:login:now:escalated", "exception:none"],
        "the old exception, carried by that escalation: nothing for the window to ask: {log}"
    );
    assert!(
        !host
            .said("account")
            .iter()
            .any(|s| s == "account:wall:auth"),
        "no host is asked about the login the person lost: {log}"
    );
}

/// A ONE-LOGIN MACHINE'S LOGIN ENDS (accounts v2 S7): the session's login is
/// a roster row with an email, and there is no move behind it. The loop
/// types no `/login`: it waits, asks the host about that wall, and with
/// nothing advised tells it the owner is needed NOW — `Run aterm harness
/// accounts renew m7-a.`, carried by no escalation of the loop's. The
/// renewal is the accounts' news inside that wait: decided again, the host
/// advises the move onto `m7-a` (renewed), the ask ends, the relaunch is
/// made and its first point gets the wall's resume — under `accounts =
/// "off"`, where the rotation is off and the login lifecycle is not. Claude
/// Code's expiry notice on a screen is told to the host once. NEGATIVE
/// CONTROL: `logins = "off"` is the old path, `/login` typed and the loop's
/// own badge.
#[test]
fn a_one_login_machine_waits_asks_renew_and_relaunches_after_it() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    // Claude Code's notice where it parks a notification, at the end.
    let mut noticed = rows(&[
        "⏺ Carried on: stage 3 is done.",
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        &format!(
            "{:>118}",
            "\u{26a0} Your login expires in 2 days \u{b7} run /login to renew"
        ),
    ]);
    noticed.extend(composer("  ? for shortcuts"));
    let mut resumed = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    resumed.extend(composer("  ? for shortcuts"));
    let screens = || {
        vec![
            busy_screen(),
            login.clone(),
            login.clone(),
            resumed.clone(),
            busy_screen(),
            noticed.clone(),
        ]
    };
    let run = |tag: &str, host: &Arc<AccountHost>, logins: bool| {
        let (dir, ledger) = ledger_at(tag);
        let (journal_dir, path) = journal_file(&format!("te-{tag}"));
        let mut m = Mock::new(true, screens());
        m.vanish_after = Some(4);
        m.stall_sleep = Some(Duration::from_millis(5));
        m.cursor_on_caret = true;
        m.turn_releases = Some(3);
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..hosted(3)
        };
        opts.policy.accounts = crate::harness::accounts::Mode::Off;
        opts.policy.logins = logins;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(ACCOUNT_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let (records, _) = journal_records(&path);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&journal_dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        (lines, m, journal, log)
    };
    let host = AccountHost::scripted(
        &[
            None,
            Some(AccountAdvice::Move {
                to: "m7-a".to_string(),
            }),
            None,
        ],
        &["adopted"],
    );
    *host.login_label.lock().unwrap() = Some("m7-a".to_string());
    host.news_after_first_ask.store(true, Ordering::SeqCst);
    host.parks.store(true, Ordering::SeqCst);
    let (lines, m, journal, log) = run("one-login-renew", &host, true);
    assert_eq!(host.said("notice"), ["notice:2"], "told once: {log}");
    assert_eq!(
        host.told(),
        ["exception:login:now", "exception:none"],
        "{log}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("EXCEPTION ")
            && l.contains("kind=login when=now")
            && l.ends_with("=> Run aterm harness accounts renew m7-a.")),
        "{log}"
    );
    assert_eq!(
        host.said("restart"),
        ["restart:account:m7-a"],
        "the move onto the renewed login: {log}"
    );
    let restarted = lines
        .iter()
        .position(|l| l.starts_with("RESTARTED seq="))
        .expect("relaunched");
    assert!(
        lines[restarted..]
            .iter()
            .any(|l| l.starts_with("CONTINUED seq=") && l.contains("rule=usage-resume@v1")),
        "the wall's resume, after the move: {log}"
    );
    assert!(
        !m.requests.iter().any(|r| r.contains("/login")),
        "no /login: {log}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "no badge of the loop's: {log}"
    );

    // NEGATIVE CONTROL: the logins' lifecycle off — the old path.
    let host = AccountHost::scripted(&[None], &["adopted"]);
    *host.login_label.lock().unwrap() = Some("m7-a".to_string());
    let (lines, m, _, log) = run("one-login-off", &host, false);
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with(" rule=auth-login@v1 /login")),
        "{log}"
    );
    assert!(
        m.requests.iter().any(|r| r.starts_with(
            "meta set attention owner=supervisor claude wall: Not logged in · Please run /login"
        )),
        "the loop's own escalation: {log}"
    );
    assert!(host.said("account").is_empty(), "nothing asked: {log}");
    assert!(host.said("notice").is_empty(), "nothing told: {log}");
}

/// A RENEWED LOGIN THAT NOTHING RELAUNCHES THE TAB ONTO (accounts v2 S7,
/// `relaunch = false`): the session's process is still on the login the
/// renewal replaced, as its host says at the lost login
/// ([`IdleHost::login_superseded`]), so no renewal reaches it. The loop
/// types no `/login` and asks no renewal again: its escalation and the
/// exception it tells name the start under the renewed login by hand.
/// NEGATIVE CONTROL: the same wall with the process on its row's own login
/// asks the renewal, as before it.
#[test]
fn a_renewed_login_nothing_relaunches_the_tab_onto_is_never_renewed_again() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let run = |tag: &str, superseded: bool| {
        let (dir, ledger) = ledger_at(tag);
        let (journal_dir, path) = journal_file(&format!("te-{tag}"));
        let mut m = Mock::new(true, vec![busy_screen(), login.clone(), login.clone()]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        m.turn_releases = Some(1);
        let host = AccountHost::scripted(&[None], &["adopted"]);
        *host.login_label.lock().unwrap() = Some("m7-a".to_string());
        host.login_superseded.store(superseded, Ordering::SeqCst);
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..hosted(3)
        };
        opts.policy.accounts = crate::harness::accounts::Mode::Off;
        opts.policy.relaunch = false;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(ACCOUNT_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let (records, _) = journal_records(&path);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&journal_dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        (m, journal, host, log)
    };
    let (m, journal, host, log) = run("renewed-by-hand", true);
    assert!(
        host.superseded_asked.load(Ordering::SeqCst) > 0,
        "asked at the lost login: {log}"
    );
    assert!(
        m.requests.iter().any(|r| r.starts_with(
            "meta set attention owner=supervisor claude wall: Not logged in · Please run /login \
             (the login was renewed, but relaunch is off: quit Claude Code and start it with the \
             line accounts renew printed)"
        )),
        "the loop's escalation names what holds the tab: {log}"
    );
    assert!(
        journal.iter().any(|l| l.starts_with("EXCEPTION ")
            && l.contains("kind=login when=now")
            && l.ends_with(
                "=> Quit Claude Code here and start it with the line accounts renew printed."
            )),
        "{log}"
    );
    assert!(
        !journal.iter().any(|l| l.contains("accounts renew m7-a")),
        "no renewal asked again: {log}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| !r.starts_with("meta ") && r.contains("/login")),
        "no /login: {log}"
    );
    assert!(host.said("restart").is_empty(), "nothing relaunched: {log}");

    // NEGATIVE CONTROL: on its row's own login, the renewal is the step.
    let (m, journal, _, log) = run("renew-by-hand", false);
    assert!(
        journal.iter().any(|l| l.starts_with("EXCEPTION ")
            && l.ends_with("=> Run aterm harness accounts renew m7-a.")),
        "{log}"
    );
    assert!(
        m.requests
            .iter()
            .any(|r| r.contains("renew it: aterm harness accounts renew m7-a")),
        "{log}"
    );
}

/// A RENEWAL MADE WHILE THE TAB'S ESCALATION ASKS FOR IT (accounts v2 S7,
/// `relaunch = false`): the escalated point has no wait of the policy's, and
/// the idle lost-login screen does not move, so only the host's stamp says
/// the renewal was made. At that news the loop asks the host again whether
/// the process holds a login a renewal replaced, and, now that it does,
/// decides the point again: the badge and the exception name what still
/// holds the tab, never the renewal again. NEGATIVE CONTROL: news of
/// something else (the stamp moved, the process still on its row's own
/// login) asks the host once more and keeps the point as it stands — one
/// badge, the renewal's.
#[test]
fn a_renewal_made_while_its_escalation_stands_decides_the_point_again() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let run = |tag: &str, supersedes: bool| {
        let (dir, ledger) = ledger_at(tag);
        let (journal_dir, path) = journal_file(&format!("te-{tag}"));
        let mut m = Mock::new(true, vec![busy_screen(), login.clone(), login.clone()]);
        m.vanish_after = Some(3);
        m.cursor_on_caret = true;
        m.turn_releases = Some(1);
        let host = AccountHost::scripted(&[None], &["adopted"]);
        *host.login_label.lock().unwrap() = Some("m7-a".to_string());
        *host.renews_when_asked.lock().unwrap() = Some(supersedes);
        let mut opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            journal: Some(path.clone()),
            ..hosted(3)
        };
        opts.policy.accounts = crate::harness::accounts::Mode::Off;
        opts.policy.relaunch = false;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(ACCOUNT_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let (records, _) = journal_records(&path);
        let journal: Vec<String> = records.into_iter().map(|r| r.line).collect();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&journal_dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        let badges: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set attention owner=supervisor claude wall: "))
            .cloned()
            .collect();
        let steps: Vec<String> = journal
            .iter()
            .filter(|l| l.starts_with("EXCEPTION ") && l.contains("kind=login when=now"))
            .filter_map(|l| l.rsplit_once("=> ").map(|(_, step)| step.to_string()))
            .collect();
        (m, steps, badges, host, log)
    };
    let (m, steps, badges, host, log) = run("renewed-under", true);
    assert_eq!(
        host.superseded_asked.load(Ordering::SeqCst),
        3,
        "asked at the lost login, at the news, and at the point decided again: {log}"
    );
    assert_eq!(badges.len(), 2, "{log}");
    assert!(
        badges[0].contains("(renew it: aterm harness accounts renew m7-a)"),
        "{log}"
    );
    assert!(
        badges[1].ends_with(
            "(the login was renewed, but relaunch is off: quit Claude Code and start it with \
             the line accounts renew printed)"
        ),
        "the badge now names what holds the tab: {log}"
    );
    assert_eq!(
        steps,
        [
            "Run aterm harness accounts renew m7-a.",
            "Quit Claude Code here and start it with the line accounts renew printed.",
        ],
        "{log}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| !r.starts_with("meta ") && r.contains("/login")),
        "no /login: {log}"
    );
    assert!(host.said("restart").is_empty(), "nothing relaunched: {log}");

    // NEGATIVE CONTROL: other news keeps the renewal's escalation as it is.
    let (_, steps, badges, host, log) = run("news-under", false);
    assert_eq!(host.superseded_asked.load(Ordering::SeqCst), 2, "{log}");
    assert_eq!(badges.len(), 1, "{log}");
    assert!(
        badges[0].contains("(renew it: aterm harness accounts renew m7-a)"),
        "{log}"
    );
    assert_eq!(steps, ["Run aterm harness accounts renew m7-a."], "{log}");
}

/// THE RENEWAL'S EDGE KEEPS WHAT THE DECISION READ (accounts v2 S7): the
/// host's stamp is read before it answers whether the process holds a login
/// a renewal replaced, so a renewal that lands right after that answer, before
/// the loop reads anything else, is news to the point decided on it; and a
/// read for the loop's books after the decision (its own answer the
/// renewal's) leaves the decision's mark alone, so the wait's edge still
/// decides the point again, once. NEGATIVE CONTROLS: while the stamp stands
/// still the host is not asked again, however many steps the wait takes;
/// news of something else asks it once and keeps the point.
#[test]
fn the_renewal_edge_keeps_what_the_decision_read() {
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let screen = Screen {
        rows: login.clone(),
        seq: 102,
        cursor_col: 2,
        ..Screen::default()
    };
    let host = AccountHost::scripted(&[None], &["adopted"]);
    *host.login_label.lock().unwrap() = Some("m7-a".to_string());
    let mut opts = account_opts(&host);
    opts.policy.accounts = crate::harness::accounts::Mode::Off;
    opts.policy.relaunch = false;
    let mut m = Mock::new(true, vec![login]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    let asked = || host.superseded_asked.load(Ordering::SeqCst);
    let r = s.turn_end_reading(&screen, None, &opts);
    assert_eq!(r.login_label.as_deref(), Some("m7-a"));
    assert!(!r.login_superseded);
    assert_eq!(asked(), 1);
    // NEGATIVE CONTROL: no news, no ask.
    for _ in 0..3 {
        assert!(!s.account_edge(&opts));
    }
    assert_eq!(asked(), 1);
    // NEGATIVE CONTROL: news of something else, asked once; the point kept.
    host.stamp.fetch_add(1, Ordering::SeqCst);
    assert!(!s.account_edge(&opts));
    assert!(!s.account_edge(&opts));
    assert_eq!(asked(), 2);
    // The renewal lands right after the decision's answer.
    host.renews_after_answer.store(true, Ordering::SeqCst);
    let r = s.turn_end_reading(&screen, None, &opts);
    assert!(!r.login_superseded);
    let books = s.turn_end_reread(&screen, &opts);
    assert!(books.login_superseded);
    assert!(s.account_edge(&opts), "the renewal decides the point again");
    assert!(!s.account_edge(&opts), "once");
}

/// THE HOST IS TOLD ONCE WHAT ANSWERED THE MOVE, and of a bad answer BEFORE
/// the same point asks its advice — so a host that strikes the account there
/// advises the next one in its very next answer. Served: the first turn
/// asked after the relaunch ran and ended with no wall. A usage limit: only
/// once a turn was asked under the new login. A lost login: likewise, only
/// once a turn was asked under the new login. NEGATIVE CONTROLS: a session
/// that made no move is told nothing; a limit on the resumed screen before
/// any turn was asked is told as nothing and asked about as nothing (it is
/// the old account's notice: the wall's resume is owed there); and a lost
/// login before any turn was asked is told nothing and asks the host nothing
/// (the old process's notice).
#[test]
fn the_host_is_told_once_what_answered_the_move() {
    let wall = limited_screen();
    let idle = ended("Carried on: stage 3 is done.");
    let mut login = rows(&[
        "⏺ Carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let moved_to = |s: &mut Session<'_, Mock>, asked: bool| {
        let r = crate::supervise::policy::turn_end::TurnEndReading::of(
            &aterm_phase::ClaudeReader.read(&idle, Some(2)),
            &idle,
            false,
            None,
            None,
            None,
        );
        s.turn_end.restarted(
            &Restart::Account {
                to: "work-2".to_string(),
                at: AccountAt::Wall,
            },
            true,
            &r,
            Instant::now(),
        );
        if asked {
            s.turn_end.host_typed(Instant::now());
        }
    };
    let told = |rows: &[String], moved: Option<bool>, worked: Option<Duration>| {
        let host = AccountHost::moving("pool-a", "adopted");
        let opts = account_opts(&host);
        let mut m = Mock::new(true, vec![rows.to_vec()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        if let Some(asked) = moved {
            moved_to(&mut s, asked);
        }
        let screen = Screen {
            rows: rows.to_vec(),
            seq: 102,
            cursor_col: 2,
            ..Screen::default()
        };
        // The loop's order: the tells off the reading, the point folded
        // in, then the host's advice.
        let mut first = s.turn_end_reading(&screen, worked, &opts);
        s.fold_point(&mut first, Instant::now(), screen.seq, &opts);
        // The reading of the same point, built again: nothing is told twice
        // and the host is not asked twice.
        let again = s.turn_end_reading(&screen, None, &opts);
        assert_eq!(first.account, again.account);
        (host.all(), first.account)
    };
    let work = Some(Duration::from_secs(180));
    let next = Some(AccountAdvice::Move {
        to: "pool-a".to_string(),
    });
    // A usage limit after a turn asked under the new login: struck, THEN
    // advised.
    assert_eq!(
        told(&wall, Some(true), work),
        (
            vec![
                "answered:wall".to_string(),
                "account:wall:usage-session".to_string()
            ],
            next.clone()
        )
    );
    // A lost login after a turn asked under the new login: struck, THEN
    // advised. (Rewritten for F11/F12: it was told at once, no turn asked.)
    assert_eq!(
        told(&login, Some(true), work),
        (
            vec![
                "answered:signed-out".to_string(),
                "account:wall:auth".to_string()
            ],
            next.clone()
        )
    );
    // Served: told, and the point is then a plain turn end like any other.
    assert_eq!(
        told(&idle, Some(true), work),
        (
            vec!["served".to_string(), "account:turn-end".to_string()],
            next.clone()
        )
    );
    // NEGATIVE CONTROLS.
    assert_eq!(
        told(&wall, None, work).0,
        ["account:wall:usage-session"],
        "no move was made: nothing to tell"
    );
    assert_eq!(
        told(&wall, Some(false), work).0,
        Vec::<String>::new(),
        "no turn was asked under the new login: the limit is the old account's notice, and \
         the wall's resume is what the point gets"
    );
    assert_eq!(
        told(&idle, Some(false), work).0,
        Vec::<String>::new(),
        "nobody asked that turn — and with the move unanswered, no move ahead is asked for"
    );
    assert_eq!(
        told(&login, Some(false), None),
        (Vec::<String>::new(), None),
        "no turn was asked under the new login: the lost login is the old process's notice, \
         nothing is struck and no host is asked"
    );
    assert_eq!(
        told(&login, None, None).0,
        Vec::<String>::new(),
        "a login the person lost: the login rule's, and no host is asked"
    );
}

/// R10/R5: A LOST LOGIN AFTER A TURN ASKED UNDER THE LOGIN THE MOVE CHOSE IS
/// TOLD — STRUCK ON THAT LOGIN — BEFORE ANY ADVICE FOR THAT POINT IS ASKED,
/// though no read saw that turn busy. The session was moved to `work-2` at
/// main's limit and the wall's resume typed there; `work-2` answers it with
/// `Not logged in` inside one read. Inside the act's take window the point
/// waits for it and the host is asked nothing (no login is struck yet); the
/// fold that judges the act tells the lost login, THEN the host is asked, so
/// the move it advises is made from a struck `work-2` — and the point read
/// again once that relaunch is made tells nothing more (the host judges the
/// process under `pool-a` by then). Asked first, the host advised with
/// `work-2` unstruck (the next account, or the move back onto `work-2` on
/// the `ok` it had before), and the strike landed after the relaunch, on
/// `pool-a`. So too where the row has LEFT the screen by the point that
/// judges the act (T8: a person's `/status` drawn over it inside the
/// window): the host is asked about the lost login the track holds, and it
/// is told there first. NEGATIVE CONTROLS: the same lost login read with
/// the turn's work is told before the fold, as ever, in the same order; and
/// `Login successful` drawn over it is no lost login, told or asked.
#[test]
fn a_lost_login_no_read_saw_busy_is_struck_before_the_host_is_asked() {
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let mut login = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let wall = limited_screen();
    // Each point: its screen, the act's take window there and the work read.
    let run = |points: &[(&[String], Duration, Option<Duration>)]| {
        let host = AccountHost::moving("pool-a", "adopted");
        let opts = account_opts(&host);
        let mut m = Mock::new(true, points.iter().map(|p| p.0.to_vec()).collect());
        let action = {
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(ACCOUNT_NOW, PDT);
            s.set_zone_lookup(test_zones);
            let r = crate::supervise::policy::turn_end::TurnEndReading::of(
                &aterm_phase::ClaudeReader.read(&wall, Some(2)),
                &wall,
                false,
                None,
                None,
                None,
            );
            let now = Instant::now();
            s.turn_end.restarted(
                &Restart::Account {
                    to: "work-2".to_string(),
                    at: AccountAt::Wall,
                },
                true,
                &r,
                now,
            );
            s.turn_end.acted(
                &TurnEndAction::Type {
                    text: "keep going".to_string(),
                    rule_id: RULE_LIMIT_RESUME,
                },
                &r,
                now,
            );
            let mut action = None;
            for (seq, (rows, take_within, worked)) in (102..).zip(points) {
                s.turn_end.timing.take_within = *take_within;
                let point = point_at(&s, rows.to_vec(), seq);
                let decided = s
                    .turn_end_at_point(&point, *worked, &opts)
                    .expect("a decision");
                if matches!(decided, TurnEndAction::Restart { .. }) {
                    s.turn_end_execute(&point, decided.clone(), &opts, &[], &mut Alone)
                        .expect("carried out");
                }
                action = Some(decided);
            }
            action.expect("a point")
        };
        (action, host.all())
    };
    let window = TurnEndTiming::default().take_within;
    let moved = [
        "answered:signed-out",
        "account:wall:auth",
        "restart:account:pool-a",
    ];
    let to_pool_a = Restart::Account {
        to: "pool-a".to_string(),
        at: AccountAt::Wall,
    };
    // Inside the take window: nothing told, nothing asked.
    let (action, said) = run(&[(&login, window, None)]);
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.starts_with("the worker to take")),
        "{action:?}"
    );
    assert!(
        said.is_empty(),
        "no advice for a login nobody struck: {said:?}"
    );
    // Judged at its deadline (a window of nothing): struck, THEN asked.
    let (action, said) = run(&[(&login, Duration::ZERO, None)]);
    assert!(
        matches!(&action, TurnEndAction::Restart { why, .. } if *why == to_pool_a),
        "{action:?}"
    );
    assert_eq!(said, moved, "struck on work-2 before the advice");
    // NEGATIVE CONTROL: the turn's work read — told before the fold.
    let (action, said) = run(&[(&login, window, Some(Duration::from_secs(1)))]);
    assert!(
        matches!(&action, TurnEndAction::Restart { why, .. } if *why == to_pool_a),
        "{action:?}"
    );
    assert_eq!(said, moved);
    // T8: THE ROW GONE BY THE POINT THAT JUDGES THE ACT — a person's
    // `/status` drawn over it inside the window: the lost login the track
    // holds is the point's, told there and struck on work-2, THEN asked
    // about. Asked first, the host advised with work-2 unstruck, and nothing
    // ever told it.
    let mut status = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
        "❯ /status",
        "  ⎿  Status dialog dismissed",
        "",
    ]);
    status.extend(composer("  ? for shortcuts"));
    let (action, said) = run(&[(&login, window, None), (&status, Duration::ZERO, None)]);
    assert!(
        matches!(&action, TurnEndAction::Restart { why, .. } if *why == to_pool_a),
        "{action:?}"
    );
    assert_eq!(said, moved, "struck on work-2 before the advice");
    // NEGATIVE CONTROL: the login back (`Login successful`) instead —
    // nothing told, and no lost login asked about.
    let mut back = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
        "❯ /login",
        "  ⎿  Login successful",
        "",
    ]);
    back.extend(composer("  ? for shortcuts"));
    let (_, said) = run(&[(&login, window, None), (&back, Duration::ZERO, None)]);
    assert!(
        !said
            .iter()
            .any(|s| s == "answered:signed-out" || s == "account:wall:auth"),
        "{said:?}"
    );
}

/// T1/T6: A LOOP STARTED ON A TOLD MOVE AN HOUR AND MORE OLD HOLDS IT
/// ([`TurnEndState::seed_account`]): the session was moved to `pool-a`,
/// whose lost login was told, and ninety minutes on its agent is relaunched
/// under that login (it crashed while it waited there, every login signed
/// out). The loop that takes it reads the move back from the ledger and
/// waits at the lost login, asking the host about it — and, nothing
/// advised, the owner for the sign-in: `/login` is never typed — nor where
/// the relaunched agent's start-up, read busy, showed a first point with no
/// wall before the lost login its continuation met (that start-up is no
/// service). NEGATIVE CONTROLS: the same move never told is history at that age, and the lost
/// login is the login rule's — `/login` typed, as with no move; so is the
/// same move told by an older build, whose row says no `until=served` (it
/// closed told moves silently, so its ledger holds every one open).
#[test]
fn a_loop_started_on_an_old_told_move_waits_at_its_lost_login() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_answer_reason, account_switch_reason,
    };
    use crate::supervise::policy::turn_end::{AccountAnswer, AccountBad};
    let mut login = rows(&[
        "⏺ Resumed: carrying on.",
        "  ⎿  Not logged in · Please run /login",
        "",
    ]);
    login.extend(composer("  ? for shortcuts"));
    let run = |tag: &str, told: Option<String>, screens: Vec<Vec<String>>| {
        let (dir, ledger) = ledger_at(tag);
        let now_ms = crate::supervise::journal::unix_ms();
        let command = format!("{ACCOUNT_MOVE_COMMAND}pool-a");
        let row = |outcome, reason: &str, box_seq, ago_min: i64| {
            Row {
                rule_id: RULE_ACCOUNT,
                outcome,
                command: &command,
                reason,
                box_seq,
            }
            .to_json(now_ms - ago_min * 60_000, None)
        };
        let mut written = vec![row(
            Outcome::Typed,
            &account_switch_reason("pool-a", AccountAt::Wall),
            90,
            90,
        )];
        if let Some(told) = told {
            written.push(row(Outcome::Waiting, &told, 91, 89));
        }
        std::fs::write(&ledger, written.join("\n") + "\n").expect("the ledger");
        let host = AccountHost::scripted(&[None], &[]);
        let (lines, m, journal) =
            account_run_with(&format!("te-{tag}"), screens, &host, &ledger, |_| {});
        let _ = std::fs::remove_dir_all(&dir);
        let log = format!(
            "{lines:#?}\n{journal:#?}\n{:?}\n{:?}\n{:#?}",
            host.all(),
            host.told(),
            m.requests
        );
        (lines, m, host, log)
    };
    let by = AccountAnswer::Bad(AccountBad::SignedOut);
    let marked = account_answer_reason("pool-a", by);
    let at_login = || vec![login.clone(), login.clone()];
    let (_, m, host, log) = run("account-old-told", Some(marked.clone()), at_login());
    assert!(
        !m.requests.iter().any(|r| r.contains("/login")),
        "no /login: {log}"
    );
    assert!(
        host.said("account")
            .iter()
            .any(|s| s == "account:wall:auth"),
        "the host is asked about the lost login: {log}"
    );
    assert!(
        host.told().iter().any(|t| t == "exception:login:now"),
        "the owner is asked for the sign-in: {log}"
    );
    // The relaunched agent read busy drawing its conversation, its first
    // point with no wall, then the lost login its continuation meets: that
    // start-up is no service — nothing told `served`, the move stands, and
    // the lost login is waited at.
    let started = vec![
        busy_screen(),
        ended("Resumed: carrying on."),
        login.clone(),
        login.clone(),
    ];
    let (_, m, host, log) = run("account-old-told-startup", Some(marked), started);
    assert!(host.said("served").is_empty(), "nothing served: {log}");
    assert!(
        !m.requests.iter().any(|r| r.contains("/login")),
        "no /login: {log}"
    );
    assert!(
        host.told().iter().any(|t| t == "exception:login:now"),
        "the owner is asked for the sign-in: {log}"
    );
    // NEGATIVE CONTROLS: never told, or told by an older build, the move
    // is history.
    let older = "the turn-end policy (account move answered: to=pool-a by=signed-out)";
    for (tag, told) in [
        ("account-old-untold", None),
        ("account-old-told-older", Some(older.to_string())),
    ] {
        let (lines, _, _, log) = run(tag, told, at_login());
        assert!(
            lines
                .iter()
                .any(|l| l.ends_with(" rule=auth-login@v1 /login")),
            "{tag}: {log}"
        );
    }
}

/// T2: A BAD ANSWER TOLD, THE POINT THE WORKER WORKS PAST IT TELLS THE HOST
/// `served` ([`IdleHost::account_served`]: it forgets the move it kept, so
/// an exit of that process after the limit has lifted keeps the login it
/// had) — then asks it as the plain turn end it is. NEGATIVE CONTROL: the
/// limit still on the screen tells nothing more, and the move stands.
#[test]
fn a_told_move_the_worker_works_past_is_told_served() {
    let idle = ended("Carried on: stage 3 is done.");
    let wall = limited_screen();
    let told = |rows: &[String]| {
        let host = AccountHost::moving("pool-a", "adopted");
        let opts = account_opts(&host);
        let mut m = Mock::new(true, vec![rows.to_vec()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        let r = crate::supervise::policy::turn_end::TurnEndReading::of(
            &aterm_phase::ClaudeReader.read(&wall, Some(2)),
            &wall,
            false,
            Some(Duration::from_secs(120)),
            None,
            None,
        );
        s.turn_end.restarted(
            &Restart::Account {
                to: "work-2".to_string(),
                at: AccountAt::Wall,
            },
            true,
            &r,
            Instant::now(),
        );
        s.turn_end.host_typed(Instant::now());
        s.turn_end
            .account_told(crate::supervise::policy::turn_end::AccountAnswer::Bad(
                crate::supervise::policy::turn_end::AccountBad::Wall,
            ));
        // The host's turn ran into `work-2`'s limit: told, and folded where
        // it was told, as the loop does (the relaunch's first point).
        s.turn_end.observe(&r, Instant::now());
        let screen = Screen {
            rows: rows.to_vec(),
            seq: 102,
            cursor_col: 2,
            ..Screen::default()
        };
        let mut first = s.turn_end_reading(&screen, Some(Duration::from_secs(180)), &opts);
        s.fold_point(&mut first, Instant::now(), screen.seq, &opts);
        (host.all(), s.turn_end.account_move().is_some())
    };
    assert_eq!(
        told(&idle),
        (
            vec!["served".to_string(), "account:turn-end".to_string()],
            false
        )
    );
    // NEGATIVE CONTROL: still at the limit.
    let (said, stands) = told(&wall);
    assert!(!said.iter().any(|s| s == "served"), "{said:?}");
    assert!(stands);
}

/// R10's hardening: THE POINT READ AGAIN ONCE AN ACCOUNT MOVE'S RELAUNCH IS
/// MADE TELLS NOTHING. Its screen is the old process's, but the host judges
/// the one its tab runs now, under the login moved to. A person's turn under
/// `work-2` runs into `work-2`'s usage limit — a turn the fold, not the tell
/// before it, marks asked — and the host advises the move to `pool-a`: once
/// that relaunch is made nothing is told (told, the limit was charged to
/// `pool-a`'s new process). NEGATIVE CONTROL: the same point where the
/// relaunch is refused (`refused:line`) tells the limit at the reread — the
/// process there is still `work-2`'s.
#[test]
fn the_point_read_again_after_an_account_relaunch_tells_nothing() {
    let wall = limited_screen();
    let started = ended("Resumed: stage 2.");
    let run = |answer: &'static str| {
        let host = AccountHost::moving("pool-a", answer);
        let opts = account_opts(&host);
        let mut m = Mock::new(true, vec![wall.clone()]);
        {
            let (mut s, point) = account_session(&mut m);
            let r = crate::supervise::policy::turn_end::TurnEndReading::of(
                &aterm_phase::ClaudeReader.read(&started, Some(2)),
                &started,
                false,
                Some(Duration::from_secs(1)),
                None,
                None,
            );
            let now = Instant::now();
            s.turn_end.restarted(
                &Restart::Account {
                    to: "work-2".to_string(),
                    at: AccountAt::Ahead,
                },
                true,
                &r,
                now,
            );
            // The relaunch's own start-up, its first point.
            s.turn_end.observe(&r, now);
            let action = s
                .turn_end_at_point(&point, Some(Duration::from_secs(180)), &opts)
                .expect("a decision");
            assert!(
                matches!(&action, TurnEndAction::Restart { why: Restart::Account { to, .. }, .. } if to == "pool-a"),
                "{action:?}"
            );
            s.turn_end_execute(&point, action, &opts, &[], &mut Alone)
                .expect("carried out");
        }
        host.all()
    };
    assert_eq!(
        run("adopted"),
        ["account:wall:usage-session", "restart:account:pool-a"],
        "nothing told of the old screen once the relaunch is made"
    );
    // NEGATIVE CONTROL.
    assert_eq!(
        run("refused:line"),
        [
            "account:wall:usage-session",
            "restart:account:pool-a",
            "answered:wall"
        ],
        "no relaunch: the process is still work-2's"
    );
}

/// A LOOP THAT STARTS AT A WALL AFTER ANOTHER KEEPS WHAT THAT ONE LEFT IN THE
/// LEDGER (each seamless update and each fault starts a new loop): four
/// moves made within the hour are the bound spent — the host's relaunch is
/// not asked, and the wall is waited out, said to be a breaker — and a move
/// that could not be made at the wall is not asked again there. NEGATIVE
/// CONTROL: the same wall and the same advice over an empty ledger ask the
/// relaunch at once.
#[test]
fn a_loop_that_starts_after_another_keeps_its_bound_and_its_latch() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason, account_unmade_reason,
    };
    let wall = || vec![busy_screen(), limited_screen()];
    let now_ms = crate::supervise::journal::unix_ms();
    let row = |outcome: Outcome, reason: &str, ago_min: i64| {
        Row {
            rule_id: RULE_ACCOUNT,
            outcome,
            command: &format!("{ACCOUNT_MOVE_COMMAND}pool-a"),
            reason,
            box_seq: 90,
        }
        .to_json(now_ms - ago_min * 60_000, None)
    };
    let run = |tag: &str, rows: &[String]| {
        let (dir, ledger) = ledger_at(tag);
        if !rows.is_empty() {
            std::fs::write(&ledger, rows.join("\n") + "\n").expect("the ledger");
        }
        let host = AccountHost::moving("work-2", "adopted");
        let (lines, _, journal) = account_run(&format!("te-{tag}"), wall(), &host, &ledger);
        let _ = std::fs::remove_dir_all(&dir);
        (host.said("restart"), lines, journal)
    };
    // The bound, spent by the loops before this one — the last move's wall's
    // resume typed under it, so the limit this loop finds is that account's
    // own, no notice its relaunch repainted.
    let made = account_switch_reason("pool-a", AccountAt::Wall);
    let resumed = Row {
        rule_id: crate::supervise::policy::turn_end::RULE_LIMIT_RESUME,
        outcome: Outcome::Typed,
        command: "keep going",
        reason: "the turn-end policy",
        box_seq: 91,
    }
    .to_json(now_ms - 5 * 60_000, None);
    let four: Vec<String> = (1..=4)
        .rev()
        .map(|ago| row(Outcome::Typed, &made, 10 * ago))
        .chain(std::iter::once(resumed.clone()))
        .collect();
    let (asked, lines, journal) = run("account-seeded-bound", &four);
    assert!(asked.is_empty(), "{lines:#?}");
    assert!(
        journal.iter().any(|l| l.starts_with("WAITING ")
            && l.contains("a breaker: 4 account moves in the last 60 min")),
        "{journal:#?}"
    );
    // The latch: refused at this wall by the loop before.
    let unmade = account_unmade_reason("pool-a", AccountAt::Wall, Some("refused:line"));
    let (asked, lines, journal) = run("account-seeded-latch", &[row(Outcome::Skipped, &unmade, 5)]);
    assert!(asked.is_empty(), "{lines:#?}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("usage-session resets")),
        "{journal:#?}"
    );
    // NEGATIVE CONTROLS: nothing on the ledger, and moves made long ago.
    let (asked, lines, _) = run("account-seeded-none", &[]);
    assert_eq!(asked, ["restart:account:work-2"], "{lines:#?}");
    let old: Vec<String> = (1..=4)
        .map(|ago| row(Outcome::Typed, &made, 120 * ago))
        .collect();
    let (asked, lines, _) = run("account-seeded-old", &old);
    assert_eq!(asked, ["restart:account:work-2"], "{lines:#?}");

    // A MOVE THE LOOP BEFORE MADE AND NOTHING ANSWERED is this loop's to
    // answer: once a turn was asked under it (a `typed` row after it), the
    // lost login it finds is the move's — told to the host, and answered by
    // the next account, no `/login` typed. NEGATIVE CONTROLS: with no turn
    // asked under the move, the first point this loop sees is the resumed
    // process's start-up (R6: it may have begun while that drew its
    // conversation, read busy) and the row it repaints the old process's —
    // nothing told, and the wall's resume typed, once; and with no such
    // row the same screen is a login the person lost.
    let lost = || {
        let mut login = rows(&[
            "⏺ Carrying on.",
            "  ⎿  Not logged in · Please run /login",
            "",
        ]);
        login.extend(composer("  ? for shortcuts"));
        vec![busy_screen(), login]
    };
    let answered = |tag: &str, rows: &[String]| {
        let (dir, ledger) = ledger_at(tag);
        if !rows.is_empty() {
            std::fs::write(&ledger, rows.join("\n") + "\n").expect("the ledger");
        }
        let host = AccountHost::moving("work-2", "adopted");
        let (lines, m, _) = account_run(&format!("te-{tag}"), lost(), &host, &ledger);
        let _ = std::fs::remove_dir_all(&dir);
        (host.all(), count(&m, "turn"), lines)
    };
    let (said, typed, lines) = answered(
        "account-seeded-open",
        &[row(Outcome::Typed, &made, 10), resumed],
    );
    assert_eq!(
        said,
        [
            "answered:signed-out",
            "account:wall:auth",
            "restart:account:work-2"
        ],
        "{lines:#?}"
    );
    assert_eq!(typed, 0, "no /login: {lines:#?}");
    let (said, typed, lines) = answered(
        "account-seeded-open-unasked",
        &[row(Outcome::Typed, &made, 1)],
    );
    assert!(
        !said.iter().any(|s| s.starts_with("answered")),
        "nothing struck: {said:?}"
    );
    assert_eq!(typed, 1, "{lines:#?}");
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("CONTINUED ") && l.contains("rule=usage-resume@v1"))
            .count(),
        1,
        "the wall's resume, once: {lines:#?}"
    );
    let (said, typed, lines) = answered("account-seeded-open-none", &[]);
    assert!(said.is_empty(), "{said:?}");
    assert_eq!(typed, 1, "the login rule's /login: {lines:#?}");
}

/// A LOOP THAT STARTS AFTER ANOTHER reads the session's account moves back
/// from the approval ledger: the moves made count toward the bound, a move
/// that could not be made at a wall is not asked again there, and a move
/// made is still to be answered until a row says it was SERVED — a typed
/// row after it is a turn asked under the new login, a bad answer told is
/// told (and closed by a `served` row, where its row says `until=served`;
/// an older build's says nothing of it). NEGATIVE CONTROLS: another
/// session's rows, a draft submitted under the rule, and a refusal ahead
/// of a wall read as nothing.
#[test]
fn the_ledger_reads_a_sessions_account_moves_back() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, AccountMoves, OpenAccountMove, Outcome, RULE_WALL_WAIT_END, Row,
        account_answer_reason, account_moves, account_switch_reason, account_unmade_reason,
    };
    use crate::supervise::policy::turn_end::AccountAnswer;
    let (dir, ledger) = ledger_at("account-ledger");
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ts: i64, sid| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, sid)
    };
    let to = |label: &str| format!("{ACCOUNT_MOVE_COMMAND}{label}");
    let write = |rows: &[String]| std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    let made = |label: &str, at, ts, sid| {
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            &to(label),
            &account_switch_reason(label, at),
            ts,
            sid,
        )
    };
    let me = Some("s-1");
    write(&[
        made("work-2", AccountAt::Wall, 1_000, me),
        made("pool-a", AccountAt::Ahead, 2_000, me),
    ]);
    let open = |asked: bool, told: bool| {
        Some(OpenAccountMove {
            to: "work-2".to_string(),
            at: AccountAt::Wall,
            at_ms: 1_000,
            asked,
            told,
            told_closes: told,
        })
    };
    assert_eq!(
        account_moves(&ledger, Some("@s-1")),
        AccountMoves {
            made_ms: vec![1_000, 2_000],
            open: Some(OpenAccountMove {
                to: "pool-a".to_string(),
                at: AccountAt::Ahead,
                at_ms: 2_000,
                asked: false,
                told: false,
                told_closes: false,
            }),
            unmade: false,
        }
    );
    // An act of the policy's after it: a turn was asked under the new login,
    // and the move is STILL to be answered — closed there, a loop that
    // started after the move's first typed act never told its host `served`.
    let act = || {
        row(
            "continue@v1",
            Outcome::Typed,
            "keep going",
            "the turn-end policy",
            3_000,
            me,
        )
    };
    let answer = |by: AccountAnswer, ts: i64| {
        row(
            RULE_ACCOUNT,
            Outcome::Waiting,
            &to("work-2"),
            &account_answer_reason("work-2", by),
            ts,
            me,
        )
    };
    write(&[made("work-2", AccountAt::Wall, 1_000, me), act()]);
    let read = account_moves(&ledger, Some("@s-1"));
    assert_eq!(
        (read.made_ms.as_slice(), read.open),
        ([1_000].as_slice(), open(true, false))
    );
    // What answered it, as the loop told its host: served closes it; a wall
    // or a lost login is told, and stands.
    write(&[
        made("work-2", AccountAt::Wall, 1_000, me),
        act(),
        answer(AccountAnswer::Served, 4_000),
    ]);
    assert_eq!(account_moves(&ledger, Some("@s-1")).open, None);
    for bad in [AccountBad::Wall, AccountBad::SignedOut] {
        write(&[
            made("work-2", AccountAt::Wall, 1_000, me),
            answer(AccountAnswer::Bad(bad), 4_000),
        ]);
        assert_eq!(
            account_moves(&ledger, Some("@s-1")).open,
            open(false, true),
            "{bad:?}"
        );
    }
    // A told row an older build wrote, without `until=served`: told, and
    // not one this build's loops close.
    write(&[
        made("work-2", AccountAt::Wall, 1_000, me),
        row(
            RULE_ACCOUNT,
            Outcome::Waiting,
            &to("work-2"),
            "the turn-end policy (account move answered: to=work-2 by=signed-out)",
            4_000,
            me,
        ),
    ]);
    assert_eq!(
        account_moves(&ledger, Some("@s-1"))
            .open
            .map(|o| (o.told, o.told_closes)),
        Some((true, false))
    );
    // NEGATIVE CONTROL: another session's answer closes nothing.
    write(&[
        made("work-2", AccountAt::Wall, 1_000, me),
        row(
            RULE_ACCOUNT,
            Outcome::Waiting,
            &to("work-2"),
            &account_answer_reason("work-2", AccountAnswer::Served),
            4_000,
            Some("s-2"),
        ),
    ]);
    assert_eq!(
        account_moves(&ledger, Some("@s-1")).open,
        open(false, false)
    );
    // A move that could not be made at a wall, and what ends that latch.
    let unmade = |at, ts| {
        row(
            RULE_ACCOUNT,
            Outcome::Skipped,
            &to("work-2"),
            &account_unmade_reason("work-2", at, Some("refused:line")),
            ts,
            me,
        )
    };
    write(&[unmade(AccountAt::Wall, 1_000)]);
    assert!(account_moves(&ledger, Some("@s-1")).unmade);
    write(&[
        unmade(AccountAt::Wall, 1_000),
        row(
            RULE_WALL_WAIT_END,
            Outcome::Waiting,
            "",
            "the wall left the screen",
            2_000,
            me,
        ),
    ]);
    assert!(
        !account_moves(&ledger, Some("@s-1")).unmade,
        "the wall is over"
    );
    // NEGATIVE CONTROLS.
    write(&[
        made("work-2", AccountAt::Wall, 1_000, Some("s-2")),
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            "my own words",
            "the turn-end policy",
            2_000,
            me,
        ),
        unmade(AccountAt::Ahead, 3_000),
    ]);
    assert_eq!(
        account_moves(&ledger, Some("@s-1")),
        AccountMoves::default()
    );
    assert_eq!(
        account_moves(&dir.join("absent.jsonl"), me),
        AccountMoves::default()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A MODEL SWITCH A MADE ACCOUNT MOVE LET GO IS NOT CARRIED ON BY THE LOOP
/// AFTER IT. The move relaunches the session on the model the fallback
/// replaced and the policy forgets the switch (`TurnEndState::restarted`):
/// its return was owed at the OLD account's bucket's reset. The ledger must
/// say the same, or every loop that starts afterwards (each seamless update,
/// each fault) reads the switch back as open and relaunches the session once
/// more at that reset — onto the model it already runs. NEGATIVE CONTROLS:
/// the switch alone is open; a move that could NOT be made, another
/// session's move, and a draft submitted under the rule leave it open; a
/// switch made AFTER the move is open.
#[test]
fn an_account_move_made_closes_the_model_switch_for_the_loop_after_it() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason, account_unmade_reason,
        model_switch_reason, open_model_switch,
    };
    use crate::supervise::policy::turn_end::RULE_MODEL_FALLBACK;
    let (dir, ledger) = ledger_at("account-closes-switch");
    let me = Some("s-1");
    let back_at = 1_789_700_000;
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ts: i64, sid| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, sid)
    };
    let switch = |ts: i64| {
        row(
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model opus",
            &model_switch_reason(Some("fable"), Some(back_at)),
            ts,
            me,
        )
    };
    let to = format!("{ACCOUNT_MOVE_COMMAND}work-2");
    let moved = |ts: i64, sid| {
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            &to,
            &account_switch_reason("work-2", AccountAt::Wall),
            ts,
            sid,
        )
    };
    let write = |rows: &[String]| std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    let open = || open_model_switch(&ledger, Some("@s-1")).map(|o| (o.to, o.back_at_unix));
    let standing = Some(("opus".to_string(), Some(back_at)));
    // NEGATIVE CONTROL: the switch alone is open.
    write(&[switch(1_000)]);
    assert_eq!(open(), standing);
    // The move made after it: no return is owed.
    write(&[switch(1_000), moved(2_000, me)]);
    assert_eq!(open(), None, "the move let the switch go");
    // NEGATIVE CONTROLS: none of these is this session's move made.
    let unmade = row(
        RULE_ACCOUNT,
        Outcome::Skipped,
        &to,
        &account_unmade_reason("work-2", AccountAt::Wall, Some("refused:line")),
        2_000,
        me,
    );
    let draft = row(
        RULE_ACCOUNT,
        Outcome::Typed,
        "my own words",
        "the turn-end policy",
        2_000,
        me,
    );
    for (what, after) in [
        ("a move not made", unmade),
        ("another session's move", moved(2_000, Some("s-2"))),
        ("a draft under the rule", draft),
    ] {
        write(&[switch(1_000), after]);
        assert_eq!(open(), standing, "{what}");
    }
    // A switch made AFTER the move is the new account's own, and open.
    write(&[switch(1_000), moved(2_000, me), switch(3_000)]);
    assert_eq!(open(), standing);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A TASK THE WORKER FINISHED, OPENED AGAIN BY SOMEONE'S TURN THAT ENDS ON A
/// USAGE LIMIT, IS ASKED ABOUT AT THAT LIMIT. The host is asked for its
/// advice BEFORE the point is folded into the policy's memory, and it is the
/// fold that opens the task again (a turn nobody of the harness asked, with
/// work, is the task theirs again): judged on the memory as it stood, the
/// session read as done, the host was asked nothing, and the wall was waited
/// out to its reset beside an account that could be run on — with nothing to
/// decide it again (a wait stands on the host's stamp only where the host
/// was asked). NEGATIVE CONTROL: the same limit read with NO turn run since
/// the task was finished is a done session's — nothing asked, nothing
/// decided.
#[test]
fn a_finished_task_opened_again_is_asked_about_at_the_limit_its_turn_ends_on() {
    let limited = || Phase::Limited {
        message: String::new(),
        reset: None,
    };
    let run = |worked: Option<Duration>| {
        let host = AccountHost::moving("work-2", "wait:changed");
        let opts = account_opts(&host);
        let mut m = Mock::new(true, vec![limited_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(ACCOUNT_NOW, PDT);
        s.set_zone_lookup(test_zones);
        // The worker reports its task finished: nothing is typed from here.
        let done = account_point_of(ended("All landed.\nATERM-TASK-DONE"), Phase::Idle);
        let action = s
            .turn_end_at_point(&done, Some(Duration::from_secs(300)), &opts)
            .expect("a decision");
        assert_eq!(action, TurnEndAction::Nothing);
        assert!(s.turn_end.task_done());
        // Then the session's screen shows a session limit.
        let wall = account_point_of(limited_screen(), limited());
        let action = s
            .turn_end_at_point(&wall, worked, &opts)
            .expect("a decision");
        (action, host.said("account:wall"))
    };
    // Someone's turn ran and ended on the limit: the task is theirs again,
    // the host is asked, and its move is asked for.
    let (action, asked) = run(Some(Duration::from_secs(180)));
    assert_eq!(asked, ["account:wall:usage-session"]);
    assert!(
        matches!(
            &action,
            TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Wall }, rule_id, .. }
                if to == "work-2" && *rule_id == RULE_ACCOUNT
        ),
        "{action:?}"
    );
    // NEGATIVE CONTROL: no turn ran — the task is still done.
    let (action, asked) = run(None);
    assert_eq!(action, TurnEndAction::Nothing);
    assert!(asked.is_empty(), "{asked:?}");
}

/// A session at its session limit under `host`, its clock the account
/// tests', and the point its screen shows.
fn account_session<'m>(m: &'m mut Mock) -> (Session<'m, Mock>, Turn) {
    let mut s = session(m, Some("@s-1".to_string()));
    s.set_clock(ACCOUNT_NOW, PDT);
    s.set_zone_lookup(test_zones);
    let wall = account_point_of(
        limited_screen(),
        Phase::Limited {
            message: String::new(),
            reset: None,
        },
    );
    (s, wall)
}

/// THE HOST IS ASKED AFTER THE POINT IS FOLDED IN, and a latch is its
/// wall's. A move the host refused for good is not asked again while that
/// wall stands; once the worker has worked and its turn ended with no wall,
/// THAT point asks again — the fold is what ends the latch, and asked before
/// it the host was passed over for one more turn end (and for a whole second
/// wall where the work ended on the limit again).
/// NEGATIVE CONTROL: the wall read again with no work asks nothing.
#[test]
fn the_host_is_asked_on_the_folded_point_where_a_latch_has_ended() {
    let host = AccountHost::moving("work-2", "refused:line");
    let opts = account_opts(&host);
    let mut m = Mock::new(true, vec![limited_screen()]);
    let (mut s, wall) = account_session(&mut m);
    let action = s
        .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
        .expect("a decision");
    s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
        .expect("the refusal is taken");
    assert!(s.turn_end.account_refused());
    assert_eq!(host.said("account"), ["account:wall:usage-session"]);
    // NEGATIVE CONTROL: the same wall, no work — latched, nothing asked.
    let again = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
    assert!(
        matches!(again, TurnEndAction::WaitUntil { .. }),
        "{again:?}"
    );
    assert_eq!(host.said("account").len(), 1, "{:?}", host.all());
    // The worker worked, and its turn ended with no wall.
    let end = account_point_of(ended("Stage 3 done; the suite is green."), Phase::Idle);
    let action = s
        .turn_end_at_point(&end, Some(Duration::from_secs(300)), &opts)
        .expect("a decision");
    assert_eq!(
        host.said("account"),
        ["account:wall:usage-session", "account:turn-end"],
        "asked at the very point that ended the latch"
    );
    assert!(
        matches!(
            &action,
            TurnEndAction::Restart { why: Restart::Account { to, at: AccountAt::Ahead }, .. }
                if to == "work-2"
        ),
        "{action:?}"
    );
}

/// NEWS OF THE ACCOUNTS REACHES A LATCHED TAB. A move refused for good is
/// not asked again while its wall stands — until the host's stamp moves: the
/// owner signed the account in and checked it, or said `accounts use`. Then
/// the latch is cleared, the point decided again, and the host asked.
/// NEGATIVE CONTROLS: with no news the refusal is not re-asked, however
/// often the point is decided; and news that follows a clearing within the
/// minute waits for the minute (a host whose refusals are themselves news
/// costs one ask a minute, never a hot loop).
#[test]
fn news_of_the_accounts_clears_a_latch_and_no_news_asks_nothing() {
    let host = AccountHost::moving("work-2", "refused:line");
    let opts = account_opts(&host);
    let mut m = Mock::new(true, vec![limited_screen()]);
    let (mut s, wall) = account_session(&mut m);
    let decide = |s: &mut Session<'_, Mock>| {
        let action = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
        let asked = matches!(action, TurnEndAction::Restart { .. });
        s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
            .expect("carried out");
        asked
    };
    assert!(decide(&mut s), "the move is asked");
    assert!(s.turn_end.account_refused() && s.turn_end_due.is_some());
    // NEGATIVE CONTROL: no news.
    assert!(!s.account_edge(&opts));
    assert!(
        !decide(&mut s),
        "a refusal repeated with no news is not re-asked"
    );
    assert_eq!(host.said("restart").len(), 1, "{:?}", host.all());
    // News.
    host.stamp.store(1, Ordering::SeqCst);
    assert!(
        s.account_edge(&opts),
        "the stamp moved under a latched wait"
    );
    assert!(
        !s.turn_end.account_refused(),
        "the latch is the news's to end"
    );
    assert!(decide(&mut s), "asked again");
    assert_eq!(host.said("restart").len(), 2, "{:?}", host.all());
    // Refused again, and more news at once: not before the minute is out.
    assert!(s.turn_end.account_refused());
    host.stamp.store(2, Ordering::SeqCst);
    assert!(!s.account_edge(&opts), "inside the minute");
    s.account_news_at = Instant::now().checked_sub(Duration::from_secs(61));
    assert!(s.account_edge(&opts), "the minute is out");
}

/// `[harness] accounts = "off"` WITH `logins = "off"` STOPS THE TELLS TOO: a
/// session whose policy remembers a move (its ledger's, from before the
/// switch) tells its host nothing of what answered it, asks it nothing, and
/// its wait hears no news. (Under `accounts = "off"` alone the login
/// lifecycle's moves are still told: the host, which keeps the roster, is
/// the one that knows whether a row it renews is there.) NEGATIVE CONTROL:
/// the same session under the default is told at once.
#[test]
fn under_accounts_off_nothing_is_told_or_asked() {
    let run = |mode: crate::harness::accounts::Mode| {
        let host = AccountHost::moving("pool-a", "adopted");
        let mut opts = account_opts(&host);
        opts.policy.accounts = mode;
        opts.policy.logins = mode != crate::harness::accounts::Mode::Off;
        let mut m = Mock::new(true, vec![limited_screen()]);
        let (mut s, wall) = account_session(&mut m);
        // The session's own move, and a turn asked under it.
        let idle = ended("Carried on: stage 3 is done.");
        let r = crate::supervise::policy::turn_end::TurnEndReading::of(
            &aterm_phase::ClaudeReader.read(&idle, Some(2)),
            &idle,
            false,
            None,
            None,
            None,
        );
        let made = Restart::Account {
            to: "work-2".to_string(),
            at: AccountAt::Wall,
        };
        s.turn_end.restarted(&made, true, &r, Instant::now());
        s.turn_end.host_typed(Instant::now());
        let _ = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        host.stamp.store(5, Ordering::SeqCst);
        s.set_due(
            Instant::now() + Duration::from_secs(3600),
            "usage-session resets",
        );
        (host.all(), s.account_edge(&opts))
    };
    assert_eq!(
        run(crate::harness::accounts::Mode::Off),
        (Vec::<String>::new(), false)
    );
    let (said, edge) = run(crate::harness::accounts::Mode::Early);
    assert_eq!(said.first().map(String::as_str), Some("answered:wall"));
    assert!(edge);
}

/// A MOVE MADE AT A WALL LEAVES THE LIMIT EPISODE BEHIND WITH THE ACCOUNT:
/// the episode the loop opened was the OLD account's, and kept open it
/// withheld every step of the host's until that account's reset plus an
/// hour — the move resumed LATER than no roster would have. The episode
/// closes with the move; the notice the resumed conversation repaints is no
/// wall to the host's last step of the relaunch, which is taken there and
/// types nothing (`done:resumed`); and what follows it is the wall's own
/// resume, at once, with the host asked nothing — as where the host owes no
/// step at all. NEGATIVE CONTROL: at a limit with no move made, the host's
/// step is withheld.
#[test]
fn a_move_made_at_a_wall_is_carried_on_past_the_limit_it_left() {
    let moved = |carry: bool| {
        let host = AccountHost::moving("work-2", "adopted");
        let opts = account_opts(&host);
        let mut m = Mock::new(true, vec![limited_screen()]);
        let (mut s, wall) = account_session(&mut m);
        let action = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        let mut deadline = Instant::now() + Duration::from_secs(3600);
        s.limit_after(&wall, &mut deadline, true, &mut Alone)
            .expect("the episode opens");
        assert!(s.limit.is_some());
        s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
            .expect("the relaunch is made");
        assert_eq!(host.said("restart"), ["restart:account:work-2"]);
        assert!(
            s.limit.is_none(),
            "the episode was the limit of the account it left"
        );
        host.carry.store(carry, Ordering::SeqCst);
        let stepped = s
            .host_steps_here(&wall, &opts, &mut Alone)
            .expect("the host's step");
        let action = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
        (stepped, action, host.all())
    };
    let resume = TurnEndAction::Type {
        text: "keep going".to_string(),
        rule_id: crate::supervise::policy::turn_end::RULE_LIMIT_RESUME,
    };
    // The host's last step of the relaunch, taken at the notice the old
    // account left: nothing typed, nothing answered, and the wall's resume
    // follows.
    let (stepped, action, said) = moved(true);
    assert!(stepped, "{said:?}");
    assert_eq!(
        said,
        [
            "account:wall:usage-session",
            "restart:account:work-2",
            "carry-on"
        ]
    );
    assert_eq!(action, resume);
    // No step of the host's owed: the same resume, the host not asked.
    let (stepped, action, said) = moved(false);
    assert!(!stepped);
    assert_eq!(action, resume);
    assert_eq!(
        said,
        ["account:wall:usage-session", "restart:account:work-2"]
    );
    // NEGATIVE CONTROL: no move made — the limit withholds the host's step.
    let host = AccountHost::scripted(&[None], &["adopted"]);
    host.carry.store(true, Ordering::SeqCst);
    let opts = account_opts(&host);
    let mut m = Mock::new(true, vec![limited_screen()]);
    let (mut s, wall) = account_session(&mut m);
    let _ = s
        .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
        .expect("a decision");
    assert!(
        !s.host_steps_here(&wall, &opts, &mut Alone)
            .expect("the host's step")
    );
    assert!(host.said("carry-on").is_empty());
}

/// THE TYPING BUDGET COUNTS TYPED ACTS, NOT RELAUNCHES: a made account
/// move's row is `typed` in the ledger (so that a later loop reads the move
/// back) and was counted toward `continue_per_hour` by a loop that started
/// after it — where the loop that made it counted no restart. NEGATIVE
/// CONTROL: a draft submitted under the rule is a typed act, and counts.
#[test]
fn an_account_move_made_is_no_typed_act_of_the_budgets() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason, typed_at,
    };
    let (dir, ledger) = ledger_at("account-budget");
    let row = |rule: &str, command: &str, reason: &str, ts: i64| {
        Row {
            rule_id: rule,
            outcome: Outcome::Typed,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    let rows = [
        row("continue@v1", "keep going", "the turn-end policy", 1_000),
        row(
            RULE_ACCOUNT,
            &format!("{ACCOUNT_MOVE_COMMAND}work-2"),
            &account_switch_reason("work-2", AccountAt::Wall),
            2_000,
        ),
        row(RULE_ACCOUNT, "my own words", "the turn-end policy", 3_000),
    ];
    std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    assert_eq!(typed_at(&ledger, Some("@s-1")), [1_000, 3_000]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A LOOP RESTARTED AT A WALL TAKES UP THE USAGE-RESUME THE LOOP BEFORE IT
/// TYPED — unless the session has moved to another account since: that
/// continuation was typed into the account it LEFT, and seeded, the new
/// account's first limit was worked as the retry of a wall it never met.
/// NEGATIVE CONTROLS: with no move after it the act is taken up as before;
/// one typed AFTER the move is the new account's own; a move that could not
/// be made changes nothing.
#[test]
fn a_usage_resume_typed_into_the_account_a_session_left_is_not_taken_up() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason, account_unmade_reason,
        usage_attempt_at,
    };
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let (dir, ledger) = ledger_at("account-usage-seed");
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ts: i64| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    let resume = |ts: i64| {
        row(
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            "keep going",
            "the turn-end policy",
            ts,
        )
    };
    let to = format!("{ACCOUNT_MOVE_COMMAND}work-2");
    let moved = |ts: i64| {
        row(
            RULE_ACCOUNT,
            Outcome::Typed,
            &to,
            &account_switch_reason("work-2", AccountAt::Wall),
            ts,
        )
    };
    let unmade = row(
        RULE_ACCOUNT,
        Outcome::Skipped,
        &to,
        &account_unmade_reason("work-2", AccountAt::Wall, Some("refused:line")),
        2_000,
    );
    let write = |rows: &[String]| std::fs::write(&ledger, rows.join("\n") + "\n").expect("ledger");
    let read = || usage_attempt_at(&ledger, Some("@s-1"));
    write(&[resume(1_000), moved(2_000)]);
    assert_eq!(read(), None, "typed into the account it left");
    // NEGATIVE CONTROLS.
    write(&[resume(1_000)]);
    assert_eq!(read(), Some(1_000));
    write(&[resume(1_000), moved(2_000), resume(3_000)]);
    assert_eq!(read(), Some(3_000), "the new account's own");
    write(&[resume(1_000), unmade]);
    assert_eq!(read(), Some(1_000), "no move was made");
    let _ = std::fs::remove_dir_all(&dir);
}

/// WHAT ANSWERED A MOVE IS WRITTEN DOWN, so the loop after this one neither
/// forgets it nor tells it twice. A loop that starts on a move the ledger
/// shows unanswered, with a turn already asked under it (a `typed` row after
/// the move), tells its host `served` at the first turn that ends with no
/// wall — and writes the answer; a loop that starts after THAT tells nothing
/// and holds no move. A lost login told by the loop before is not told
/// again. NEGATIVE CONTROL: the same lost login, not told yet, is told.
#[test]
fn a_move_seeded_from_the_ledger_is_answered_once_across_loops() {
    use crate::supervise::approvals::{
        ACCOUNT_MOVE_COMMAND, Outcome, Row, account_answer_reason, account_moves,
        account_switch_reason,
    };
    use crate::supervise::policy::turn_end::{AccountAnswer, RULE_LIMIT_RESUME};
    let now_ms = crate::supervise::journal::unix_ms();
    let row = |rule: &str, outcome: Outcome, command: &str, reason: &str, ago_s: i64| {
        Row {
            rule_id: rule,
            outcome,
            command,
            reason,
            box_seq: 90,
        }
        .to_json(now_ms - ago_s * 1_000, None)
    };
    let to = format!("{ACCOUNT_MOVE_COMMAND}work-2");
    let made = row(
        RULE_ACCOUNT,
        Outcome::Typed,
        &to,
        &account_switch_reason("work-2", AccountAt::Wall),
        120,
    );
    let resumed = row(
        RULE_LIMIT_RESUME,
        Outcome::Typed,
        "keep going",
        "the turn-end policy",
        110,
    );
    let served = || vec![busy_screen(), ended("Carried on: stage 3 is done.")];
    let run = |tag: &str, rows: &[String], screens: Vec<Vec<String>>| {
        let (dir, ledger) = ledger_at(tag);
        std::fs::write(&ledger, rows.join("\n") + "\n").expect("the ledger");
        let host = AccountHost::scripted(&[None], &["adopted"]);
        let _ = account_run(&format!("te-{tag}"), screens, &host, &ledger);
        let after = account_moves(&ledger, None);
        let text = std::fs::read_to_string(&ledger).expect("the ledger");
        let _ = std::fs::remove_dir_all(&dir);
        (host.all(), after, text)
    };
    // The move, a turn asked under it, and this loop sees that turn served.
    let (said, after, text) = run(
        "account-seeded-served",
        &[made.clone(), resumed.clone()],
        served(),
    );
    assert_eq!(said.first().map(String::as_str), Some("served"), "{said:?}");
    assert_eq!(said.iter().filter(|s| *s == "served").count(), 1);
    assert_eq!(after.open, None, "served: the move is behind the session");
    assert!(
        text.contains("\"rule_id\":\"account-rotate@v1\",\"decision\":\"waiting\"")
            && text.contains("(account move answered: to=work-2 by=served)"),
        "{text}"
    );
    // The loop after THAT: nothing to tell.
    let answered = row(
        RULE_ACCOUNT,
        Outcome::Waiting,
        &to,
        &account_answer_reason("work-2", AccountAnswer::Served),
        100,
    );
    let (said, ..) = run(
        "account-seeded-served-again",
        &[made.clone(), resumed.clone(), answered],
        served(),
    );
    assert!(!said.iter().any(|s| s == "served"), "{said:?}");
    // A lost login the loop before told: not told again by this one.
    let lost = || {
        let mut login = rows(&[
            "⏺ Carrying on.",
            "  ⎿  Not logged in · Please run /login",
            "",
        ]);
        login.extend(composer("  ? for shortcuts"));
        vec![busy_screen(), login]
    };
    let told = row(
        RULE_ACCOUNT,
        Outcome::Waiting,
        &to,
        &account_answer_reason(
            "work-2",
            AccountAnswer::Bad(crate::supervise::policy::turn_end::AccountBad::SignedOut),
        ),
        100,
    );
    let (said, after, _) = run("account-seeded-told", &[made.clone(), told], lost());
    assert!(!said.iter().any(|s| s.starts_with("answered")), "{said:?}");
    assert!(after.open.is_some_and(|open| open.told));
    // NEGATIVE CONTROL: the same lost login after a turn asked under the
    // move, not told yet, is told.
    let (said, after, _) = run(
        "account-seeded-untold",
        &[made.clone(), resumed.clone()],
        lost(),
    );
    assert_eq!(
        said.first().map(String::as_str),
        Some("answered:signed-out"),
        "{said:?}"
    );
    assert!(after.open.is_some_and(|open| open.told), "and written down");
    // …and with NO turn asked under the move, the first point this loop
    // sees is the resumed process's start-up (R6): the row it repaints is
    // the old process's, nothing is told, and the wall's resume is typed.
    let (said, after, text) = run("account-seeded-unasked", &[made], lost());
    assert!(!said.iter().any(|s| s.starts_with("answered")), "{said:?}");
    assert!(after.open.is_some_and(|open| !open.told));
    assert_eq!(
        text.matches("\"rule_id\":\"usage-resume@v1\",\"decision\":\"typed\"")
            .count(),
        1,
        "the wall's resume, once: {text}"
    );
}

/// THE OWNER LIFTED THE WALL A SESSION STANDS AT (`accounts use` on its own
/// account; the host's advice is `Resume`): the loop types that wall's own
/// continuation at the wall, at once — one guarded turn, under
/// `usage-resume@v1`, counted as the wall's attempt — and the limit met
/// again under the SAME clearing is waited out by the wall's own ladder:
/// asked, and typed nothing more. No relaunch is asked for. NEGATIVE
/// CONTROL: a host with no advice types nothing at that wall.
#[test]
fn the_owners_clearing_continues_a_session_at_its_wall_once() {
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let run = |advice: Option<AccountAdvice>| {
        let host = AccountHost::scripted(&[advice], &["adopted"]);
        let opts = account_opts(&host);
        let mut m = Mock::new(true, vec![limited_screen()]);
        let attempts = {
            let (mut s, wall) = account_session(&mut m);
            let first = s
                .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
                .expect("a decision");
            s.turn_end_execute(&wall, first.clone(), &opts, &[], &mut Alone)
                .expect("carried out");
            // The account answers with its limit again, three seconds on.
            let again = s
                .turn_end_at_point(&wall, Some(Duration::from_secs(3)), &opts)
                .expect("a decision");
            assert!(
                matches!(again, TurnEndAction::WaitUntil { .. }),
                "{again:?}"
            );
            s.turn_end_execute(&wall, again, &opts, &[], &mut Alone)
                .expect("carried out");
            (
                first,
                s.turn_end
                    .wall_attempts(aterm_phase::WallKind::UsageSession),
            )
        };
        let typed = m.requests.iter().filter(|r| r.contains("keep going"));
        (attempts, typed.count(), host.all())
    };
    let ((first, attempts), turns, said) = run(Some(AccountAdvice::Resume {
        cleared_at: ACCOUNT_NOW - 30,
    }));
    assert_eq!(
        first,
        TurnEndAction::Type {
            text: "keep going".to_string(),
            rule_id: RULE_LIMIT_RESUME,
        }
    );
    assert_eq!(
        (attempts, turns),
        (Some(1), 1),
        "one continuation: {said:?}"
    );
    assert_eq!(
        said,
        ["account:wall:usage-session", "account:wall:usage-session"],
        "asked at each decision, and no relaunch"
    );
    // NEGATIVE CONTROL.
    let ((first, attempts), turns, _) = run(None);
    assert!(
        matches!(first, TurnEndAction::WaitUntil { .. }),
        "{first:?}"
    );
    assert_eq!((attempts, turns), (Some(0), 0));
}

/// THE STAMP A LOOP FIRST READS IS NO NEWS. A host's stamp counts the
/// accounts' news since the HOST started; a loop that starts later (each
/// seamless update, each fault) has read none of it, and the latch the loop
/// before it left in the ledger is still this wall's: read as news, the
/// first decision of every new loop ended that latch and asked the host for
/// the relaunch it had refused for good. NEGATIVE CONTROL: the stamp moving
/// AFTER that first read is news — the latch ends and the host is asked.
#[test]
fn the_stamp_a_loop_first_reads_is_no_news_to_its_latch() {
    let host = AccountHost::moving("work-2", "refused:line");
    host.stamp.store(3, Ordering::SeqCst);
    let opts = account_opts(&host);
    let mut m = Mock::new(true, vec![limited_screen()]);
    let (mut s, wall) = account_session(&mut m);
    s.turn_end.seed_account_unmade();
    let action = s
        .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
        .expect("a decision");
    assert!(
        matches!(action, TurnEndAction::WaitUntil { .. }),
        "{action:?}"
    );
    assert!(
        s.turn_end.account_refused(),
        "the latch the loop before left stands"
    );
    assert!(host.all().is_empty(), "{:?}", host.all());
    s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
        .expect("the wait is set");
    assert!(!s.account_edge(&opts), "nothing new since the first read");
    // NEGATIVE CONTROL: news.
    host.stamp.store(4, Ordering::SeqCst);
    assert!(s.account_edge(&opts));
    let again = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
    assert!(matches!(again, TurnEndAction::Restart { .. }), "{again:?}");
    assert_eq!(host.said("account"), ["account:wall:usage-session"]);
}

/// A MOVE MADE AT A WALL THAT A LATER LOOP TAKES UP — a loop that started
/// right after the relaunch, or the one that takes the agent a host's carry
/// landed (the host writes the move's row then) — IS CARRIED ON PAST THE
/// NOTICE IT LEFT, as by the loop that made it: the limit the resumed
/// conversation repaints is the old account's, the host's last step of the
/// relaunch is taken there (it types nothing) and not withheld until that
/// account's reset, and the wall's own resume follows at once — as where the
/// host owes no step — the host asked nothing.
/// NEGATIVE CONTROLS: with no such row the limit withholds the host's step
/// and is waited out; a move made AHEAD of a wall left no notice, so a limit
/// on the screen is a wall like any other.
#[test]
fn a_loop_that_takes_up_a_move_made_at_a_wall_carries_it_on_past_the_notice() {
    use crate::supervise::approvals::{ACCOUNT_MOVE_COMMAND, Outcome, Row, account_switch_reason};
    let now_ms = crate::supervise::journal::unix_ms();
    let made = |at: AccountAt| {
        Row {
            rule_id: RULE_ACCOUNT,
            outcome: Outcome::Typed,
            command: &format!("{ACCOUNT_MOVE_COMMAND}work-2"),
            reason: &account_switch_reason("work-2", at),
            box_seq: 90,
        }
        .to_json(now_ms - 120_000, None)
    };
    let run = |tag: &str, rows: &[String], carry: bool| {
        let (dir, ledger) = ledger_at(tag);
        if !rows.is_empty() {
            std::fs::write(&ledger, rows.join("\n") + "\n").expect("the ledger");
        }
        let host = AccountHost::scripted(&[None], &["adopted"]);
        host.carry.store(carry, Ordering::SeqCst);
        let (lines, m, journal) =
            account_run(&format!("te-{tag}"), vec![limited_screen()], &host, &ledger);
        let _ = std::fs::remove_dir_all(&dir);
        let typed = m.requests.iter().filter(|r| r.contains("keep going"));
        (
            host.all(),
            typed.count(),
            format!("{lines:#?}\n{journal:#?}"),
        )
    };
    // The host's last step of the relaunch, at the notice the old account
    // left, types nothing: the wall's own resume follows.
    let (said, typed, log) = run("account-taken-up-carry", &[made(AccountAt::Wall)], true);
    assert_eq!(said, ["carry-on"], "{log}");
    assert_eq!(typed, 1, "the wall's resume, once: {log}");
    assert!(log.contains("carry-on step=done:resumed"), "{log}");
    assert!(
        log.contains(&format!(
            "rule={}",
            crate::supervise::policy::turn_end::RULE_LIMIT_RESUME
        )),
        "{log}"
    );
    // No step of the host's owed: the same resume, the host not asked.
    let (said, typed, log) = run("account-taken-up-resume", &[made(AccountAt::Wall)], false);
    assert!(said.is_empty(), "{said:?}\n{log}");
    assert_eq!(typed, 1, "{log}");
    // NEGATIVE CONTROLS.
    let (said, typed, log) = run("account-taken-up-none", &[], true);
    assert_eq!(said, ["account:wall:usage-session"], "{log}");
    assert_eq!(typed, 0, "{log}");
    let (said, typed, log) = run("account-taken-up-ahead", &[made(AccountAt::Ahead)], true);
    assert_eq!(said, ["account:wall:usage-session"], "{log}");
    assert_eq!(typed, 0, "{log}");
}

/// A HOST WHOSE EVIDENCE LAGS THE SCREEN LEARNS TO LOOK AGAIN BY BEING ASKED
/// AGAIN ([`IdleHost::account`]: the limit's row is not in the transcript
/// yet). Its answer is a wait for a moment away; the wall's wait is cut to
/// it, never nearer than a minute; nothing is latched and no relaunch is
/// asked; and the point decided when that wait runs out asks the host once
/// more — which then has its row, and advises the move. NEGATIVE CONTROL: a
/// host that advises nothing leaves the wall's own wait, to its reset.
#[test]
fn a_host_that_answers_a_short_wait_is_asked_again_when_it_runs_out() {
    let decided = |first: Option<AccountAdvice>| {
        let host = AccountHost::scripted(
            &[
                first,
                Some(AccountAdvice::Move {
                    to: "work-2".to_string(),
                }),
            ],
            &["adopted"],
        );
        let opts = account_opts(&host);
        let mut m = Mock::new(true, vec![limited_screen()]);
        let (mut s, wall) = account_session(&mut m);
        let asked_at = Instant::now();
        let action = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(180)), &opts)
            .expect("a decision");
        let TurnEndAction::WaitUntil { until, .. } = &action else {
            panic!("{action:?}")
        };
        let waits = until.saturating_duration_since(asked_at);
        s.turn_end_execute(&wall, action, &opts, &[], &mut Alone)
            .expect("the wait is set");
        assert!(!s.turn_end.account_refused(), "a wait latches nothing");
        assert!(host.said("restart").is_empty(), "{:?}", host.all());
        // The wait runs out: the point is decided again.
        let again = s.turn_end_at_point(&wall, None, &opts).expect("a decision");
        (waits, again, host.said("account").len())
    };
    let soon = Instant::now() + Duration::from_secs(5);
    let (waits, again, asked) = decided(Some(AccountAdvice::Wait { until: soon }));
    assert!(
        (Duration::from_secs(60)..Duration::from_secs(90)).contains(&waits),
        "cut to the host's moment, never nearer than a minute: {waits:?}"
    );
    assert_eq!(asked, 2, "asked again at the next decision");
    assert!(
        matches!(&again, TurnEndAction::Restart { why: Restart::Account { to, .. }, .. } if to == "work-2"),
        "{again:?}"
    );
    // NEGATIVE CONTROL.
    let (waits, ..) = decided(None);
    assert!(waits > Duration::from_secs(3600), "{waits:?}");
}

/// ONE CONTINUATION A CLEARING, ACROSS LOOPS. Which clearing was answered is
/// the policy's memory, and a loop's restart loses it: a loop that started at
/// the same notice — the continuation not taken, or the account's answer not
/// in the transcript yet — typed that clearing's continuation a second time.
/// The ledger's last `usage-resume@v1` row answers every clearing made before
/// it. NEGATIVE CONTROL: a clearing made AFTER that row is a new one, and is
/// continued.
#[test]
fn a_clearing_the_loop_before_answered_is_not_answered_again() {
    use crate::supervise::approvals::{Outcome, Row};
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    let now_ms = crate::supervise::journal::unix_ms();
    let resumed = Row {
        rule_id: RULE_LIMIT_RESUME,
        outcome: Outcome::Typed,
        command: "keep going",
        reason: "the turn-end policy",
        box_seq: 90,
    }
    .to_json(now_ms - 120_000, None);
    let typed_again = |tag: &str, cleared_ago_s: i64| {
        let (dir, ledger) = ledger_at(tag);
        std::fs::write(&ledger, format!("{resumed}\n")).expect("the ledger");
        let host = AccountHost::scripted(
            &[Some(AccountAdvice::Resume {
                cleared_at: now_ms / 1000 - cleared_ago_s,
            })],
            &["adopted"],
        );
        let (lines, m, journal) =
            account_run(&format!("te-{tag}"), vec![limited_screen()], &host, &ledger);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            host.said("account"),
            ["account:wall:usage-session"],
            "{lines:#?}\n{journal:#?}"
        );
        m.requests
            .iter()
            .filter(|r| r.contains("keep going"))
            .count()
    };
    assert_eq!(
        typed_again("account-cleared-before", 300),
        0,
        "the loop before answered that clearing"
    );
    assert_eq!(typed_again("account-cleared-after", 60), 1);
}

// ------------------------------------------------------------------------
// The accounts probe in the loop
// (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §3): `/usage` typed where
// the host claims one, the Settings panel read settled, the sample handed
// over, the panel closed — every exit of the step, over the panel's world
// (`PanelWorld`: the measured 2.1.287 screens, reacting to what is written).
// Each run is also projected onto `SupervisorUsageProbe`
// (`usage_probe_violations`): nothing it wrote is anything the model forbids.
// ------------------------------------------------------------------------

use aterm_phase::prompt::fixtures as panel_fx;

/// 2026-10-01T21:20:00-07:00, minutes after the measured screens were
/// taken: their `Resets 2am` is 4 h 40 min ahead and their `Resets Oct 8 at
/// 9am` six and a half days, both inside what a sample believes.
const PROBE_NOW: i64 = 1_790_914_800;

/// The turn whose end is probed: real work behind it, nothing asked.
const PROBE_SAID: &str = "Stage 3 done; the suite is green.";

/// The world a probe test starts from: a live turn, its end at an empty
/// composer, and — once the loop has taken its own act there — a turn that
/// ends on a request for a decision, which is escalated and ends the run.
fn probe_world(frames: &[&str], host: &ProbeHost) -> Mock {
    let mut m = probe_mock(vec![busy_screen(), ended(PROBE_SAID)], frames, host);
    world(&mut m)
        .then
        .push_back(vec![busy_screen(), ended(STOP)]);
    m
}

/// The probe's waits at a scripted server's scale: 60 ms for a panel that
/// never comes, 100 ms for one that does not leave, a read every 10 ms at
/// the latest — and HALF A MINUTE to read settled, a hang detector and no
/// latency budget (a loaded machine may take its time between two reads;
/// the one test of a panel that never settles shortens it).
fn probe_waits(s: &mut Session<'_, Mock>) {
    let ms = Duration::from_millis;
    s.set_probe_waits(ms(60), Duration::from_secs(30), ms(10), ms(100));
}

/// One watch over `m` under `host`, the session's ledger at `ledger`, with
/// [`probe_waits`]: the lines printed and the journal's. The run ends when
/// the script does (the session gone); its budget of a minute is a hang
/// detector ([`probe_run_for`] gives another).
fn probe_run(
    tag: &str,
    m: &mut Mock,
    host: &Arc<ProbeHost>,
    ledger: &Path,
    policy: SupervisorConfig,
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (Vec<String>, Vec<String>) {
    probe_run_for(60, tag, m, host, ledger, policy, setup)
}

/// [`probe_run`] with the watch's budget as given: a second, for a world
/// that never moves on by itself — the run then ends on its budget, having
/// shown what the loop does for as long as it runs.
fn probe_run_for(
    max_s: u64,
    tag: &str,
    m: &mut Mock,
    host: &Arc<ProbeHost>,
    ledger: &Path,
    policy: SupervisorConfig,
    setup: impl FnOnce(&mut Session<'_, Mock>),
) -> (Vec<String>, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        policy,
        ..hosted(max_s)
    };
    let (lines, _) = watch_lines_with(m, &opts, |s| {
        s.set_clock(PROBE_NOW, PDT);
        s.set_approval_ledger(Some(ledger.to_path_buf()));
        probe_waits(s);
        setup(s);
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, records.into_iter().map(|r| r.line).collect())
}

/// The owner's policy as these tests run it ([`hosted`]'s), with the
/// person's grace as given.
fn probe_policy(human_grace_s: u32) -> SupervisorConfig {
    SupervisorConfig {
        human_grace_s,
        ..hosted(60).policy
    }
}

/// The probe's rows of the session ledger at `path`, as `decision reason`.
fn probe_rows(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("\"rule_id\":\"usage-probe@v1\""))
        .map(|l| {
            let field = |name: &str| {
                l.split(&format!("\"{name}\":\""))
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .unwrap_or("")
                    .to_string()
            };
            format!(
                "{} {} {}",
                field("decision"),
                field("command"),
                field("reason")
            )
        })
        .collect()
}

/// The loop's lines about a DUE probe it did not run
/// (`SKIPPED seq=<n> rule=usage-probe@v1 gate=<word>[ host=<word>]`:
/// [`Session::probe_shut`]), as `<seq> <word>[ host=<word>]`, in order.
fn gates_said(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("SKIPPED seq="))
        .filter_map(|rest| {
            let (seq, rest) = rest.split_once(' ')?;
            let gate = rest.strip_prefix("rule=usage-probe@v1 gate=")?;
            Some(format!("{seq} {gate}"))
        })
        .collect()
}

/// [`gates_said`] without the seq — which counts the reads of a run, and so
/// moves with its timing at every point but the first.
fn gate_words(lines: &[String]) -> Vec<String> {
    gates_said(lines)
        .iter()
        .filter_map(|g| g.split_once(' '))
        .map(|(_, word)| word.to_string())
        .collect()
}

/// Every OTHER `SKIPPED` line: a step that was begun and not carried out.
fn other_skips(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.starts_with("SKIPPED ") && !l.contains(" gate="))
        .collect()
}

/// THE PROBE, SETTLED. At a turn end read live, where the host's advice is
/// the probe and it grants the claim, the loop writes `/usage` and its Enter
/// — each fenced on the person count, the judged read's generation and the
/// composer's caret row — reads the panel until two reads in a row say
/// SETTLED with the same windows (the first read here is the measured
/// `Refreshing…` frame: no sample from it), hands the host the sample with
/// its resets placed on the loop's clock, and only THEN presses one Escape,
/// fenced on the person count and the panel's own header row. Then the point
/// is decided again and its own act typed. In the ledger: the `typed` row
/// and the `waiting` row; in the hourly budget's reader: only the act.
///
/// A PROBE THAT RUNS SAYS NO SKIP: the point it ran at has no `gate=` line,
/// though the host — asked again where the point is decided after the probe
/// — still answers that the read is due. (The one such line of the run is
/// the LATER point's, which the policy escalates: no probe there.)
///
/// NEGATIVE CONTROLS. A host that declines the claim: not one byte of the
/// probe's is written — the writes are the act's alone. A host that advises
/// nothing (no roster) is never asked to claim, asks the server nothing the
/// loop did not ask before the probe existed (`modes` is never sent), and
/// its request stream is byte for byte that of a loop with no host at all.
#[test]
fn a_claimed_probe_types_usage_samples_the_settled_panel_and_closes_it() {
    let (dir, ledger) = ledger_at("probe-settled");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let (lines, journal) = probe_run(
        "te-probe-settled",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "esc", "text:keep going", "enter"],
        "{:#?}",
        m.requests
    );
    let wrote = |what: &str| {
        m.requests
            .iter()
            .find(|r| matches!(verb_of(r), "send" | "key") && r.ends_with(what))
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(
        wrote("-- /usage"),
        "send if-human=0 if-gen=1.102 if=^❯\\s*$ -- /usage"
    );
    assert_eq!(
        wrote("usage\\s*$ enter"),
        "key if-human=0 if-gen=1.104 if=^❯\\x20/usage\\s*$ enter"
    );
    assert_eq!(
        wrote(" esc"),
        "key if-human=0 if=^...Settings..Status...Config...Usage esc",
        "the Escape: the person fence and the panel's header row, no generation"
    );
    assert_eq!(count(&m, "turn"), 0, "never the unfenced fallback");
    assert_eq!(
        host.said(""),
        [
            "turn-ran",
            "account:turn-end",
            "probe:claimed",
            "probed:ok",
            "account:turn-end",
            "turn-ran",
            "account:turn-end",
        ],
        "no turn ran between the probe and the point decided again: {lines:#?}"
    );
    let samples = host.samples.lock().unwrap().clone();
    assert_eq!(samples.len(), 1);
    let (sample, pan, hand) = &samples[0];
    assert_eq!(
        (sample.session_pct, sample.week_pct, sample.at),
        (Some(1), Some(10), PROBE_NOW),
        "the settled frame's figures, never the refreshing one's 9 %"
    );
    assert_eq!(
        (sample.session_reset, sample.week_reset),
        (Some(PROBE_NOW + 16_800), Some(PROBE_NOW + 560_400)),
        "2am and Oct 8 at 9am, Pacific"
    );
    assert_eq!((*pan, *hand), (2, false), "handed over at a settled panel");
    let kinds: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert_eq!(
        kinds,
        ["EVENT", "PROBED", "CONTINUED", "EVENT", "SKIPPED", "EXIT"],
        "{lines:#?}"
    );
    assert_eq!(
        lines[1],
        "PROBED seq=102 rule=usage-probe@v1 /usage session=1% week=10%"
    );
    // The probe ran at seq 102: no skip is said of that point — not at the
    // probe, and not where it was decided again with the read still advised.
    assert_eq!(
        gate_words(&journal),
        ["escalated"],
        "the later point's, which is a person's: {journal:#?}"
    );
    assert!(
        !gates_said(&journal).iter().any(|g| g.starts_with("102 ")),
        "{journal:#?}"
    );
    assert_eq!(gates_said(&lines), gates_said(&journal), "said, so printed");
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(
        crate::supervise::approvals::typed_at(&ledger, None).len(),
        1,
        "the budget's reader counts the continuation alone"
    );
    assert!(!crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("WAITING ") || l.starts_with("HELD ")),
        "{journal:#?}"
    );
    assert!(other_skips(&journal).is_empty(), "{journal:#?}");
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: the claim declined.
    let (dir, ledger) = ledger_at("probe-declined");
    let host = ProbeHost::probing(false);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let (lines, journal) = probe_run(
        "te-probe-declined",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:keep going", "enter"], "{lines:#?}");
    assert_eq!(host.said("probe"), ["probe:declined"]);
    // …and THIS point, where the due probe was not run, has its line: the
    // claim, with the host's word for the refusal.
    assert_eq!(
        gates_said(&journal)[0],
        "102 claim host=not-due",
        "{journal:#?}"
    );
    assert_eq!(
        gate_words(&journal),
        ["claim host=not-due", "escalated"],
        "{journal:#?}"
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new());
    assert!(probe_rows(&ledger).is_empty());
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: no roster. A host that advises nothing — and would
    // grant any claim — is never asked for one, and the server sees exactly
    // what it sees from a loop with no host.
    let run = |host: Option<&Arc<ProbeHost>>, tag: &str| {
        let (dir, ledger) = ledger_at(tag);
        let none = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_SETTLED], host.unwrap_or(&none));
        let opts = SuperviseOpts {
            idle_host: host.map(|h| Arc::clone(h) as Arc<dyn IdleHost>),
            ..hosted(60)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(PROBE_NOW, PDT);
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let _ = std::fs::remove_dir_all(&dir);
        // A wait's `timeout <ms>` is what is left of the run's budget on the
        // wall clock: the one thing two runs do not share.
        m.requests
            .iter()
            .map(|r| match r.rsplit_once(" timeout ") {
                Some((head, ms)) if ms.bytes().all(|b| b.is_ascii_digit()) => head.to_string(),
                _ => r.clone(),
            })
            .collect::<Vec<String>>()
    };
    let rosterless = ProbeHost::scripted(&[None], &[true]);
    let with_host = run(Some(&rosterless), "probe-no-roster");
    assert!(rosterless.said("probe").is_empty(), "never asked to claim");
    assert_eq!(
        rosterless.said("account"),
        ["account:turn-end", "account:turn-end"]
    );
    assert!(
        !with_host
            .iter()
            .any(|r| r == "modes" || r.contains("/usage") || r.ends_with(" esc")),
        "{with_host:#?}"
    );
    assert_eq!(with_host, run(None, "probe-no-host"));
}

/// What a step of the loop SAID (printed, journaled, and in a host's log)
/// and what it only NOTED (the journal alone).
#[derive(Default)]
struct Heard {
    said: Vec<String>,
    noted: Vec<String>,
}

impl Review for Heard {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(
        &mut self,
        _turn: &Turn,
        _report: Option<ReportBrief>,
        _fold: Option<Fold>,
    ) -> Result<bool, String> {
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        self.said.push(line.to_string());
        Ok(())
    }
    fn note(&mut self, line: &str) {
        self.noted.push(line.to_string());
    }
    fn unattended(&self) -> bool {
        true
    }
}

/// One thing that shuts a gate, done to the session at its point (after the
/// decision that got the roster's advice), to its host, or to the decision.
type Shut = fn(&mut Session<'_, Mock>, &Turn, &ProbeHost, &mut TurnEndAction);

/// A session at a turn end where the roster says the probe is DUE, every
/// gate open — the turn read live, a ledger, the fences, the alternate
/// screen, a host that grants the claim — but for what `shut` does; then the
/// probe's step TWICE at that point and the point decided again. Answers
/// what the first step did, everything SAID, the probe's writes and what
/// the host was asked about a claim.
fn due_probe_at_a_gate(
    tag: &str,
    shut: Option<Shut>,
) -> (
    Result<Probed, Fail>,
    Vec<String>,
    Vec<&'static str>,
    Vec<String>,
) {
    let (dir, ledger) = ledger_at(tag);
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(
        vec![ended(PROBE_SAID)],
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            continue_policy: false,
            ..hosted(30).policy
        },
        ..hosted(30)
    };
    let mut s = session(&mut m, None);
    s.set_clock(PROBE_NOW, PDT);
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.clone()));
    s.ledger = crate::supervise::approvals::Ledger::open(Some(&ledger), None, &mut Vec::new());
    probe_waits(&mut s);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let mut decided = s
        .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(host.said("account"), ["account:turn-end"], "advised");
    s.turn_live = true;
    if let Some(shut) = shut {
        shut(&mut s, &point, &host, &mut decided);
    }
    let mut heard = Heard::default();
    let first = s.usage_probe(&point, &decided, &opts, &mut heard);
    // ONCE A POINT: the step again, and the point decided again — the host
    // advising the probe each time — say nothing more of it.
    if first == Ok(Probed::No) {
        assert_eq!(
            s.usage_probe(&point, &decided, &opts, &mut heard),
            Ok(Probed::No),
            "{tag}"
        );
    }
    s.turn_end_now(&point, &opts, &[], &mut heard)
        .unwrap_or_else(|e| panic!("{tag}: {e:?}"));
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
    (first, heard.said, probe_writes(&m), host.said("probe"))
}

/// The Mac slept eight hours after the point was decided, and a request
/// saw it ([`Session::call`]): the wake's settle stands.
fn a_night_slept(s: &mut Session<'_, Mock>, _: &Turn, _: &ProbeHost, _: &mut TurnEndAction) {
    s.set_clock(PROBE_NOW + 8 * 3600, PDT);
    let _ = s.call(&["status"]).expect("a read at the wake");
}

/// THE ACCOUNTS PROBE AT THE WAKE (the review of 2026-10-14): `/usage` is a
/// command typed into the agent, and the Settings panel it opens reads the
/// account's windows from the API — which the network may not reach in the
/// wake's settle. A due probe at a point decided just before the Mac slept,
/// asked once a request has seen the sleep, is not run: nothing written, and
/// its one line says why (`gate=wake-settle`). NEGATIVE CONTROL: the same
/// point with no sleep runs it.
#[test]
fn a_due_probe_at_the_wake_waits_for_the_settle() {
    let (did, said, wrote, claims) =
        due_probe_at_a_gate("probe-at-the-wake", Some(a_night_slept as Shut));
    assert_eq!(did, Ok(Probed::No), "{said:#?}");
    assert_eq!(
        said,
        ["SKIPPED seq=101 rule=usage-probe@v1 gate=wake-settle"],
        "one line, once"
    );
    assert_eq!(wrote, Vec::<&str>::new(), "nothing written");
    assert_eq!(claims, Vec::<&str>::new(), "no claim asked");
    // NEGATIVE CONTROL.
    let (did, _, wrote, _) = due_probe_at_a_gate("probe-at-the-wake-none", None);
    assert_eq!(did, Ok(Probed::Done));
    assert_eq!(wrote, ["/usage", "esc"]);
}

/// A DUE PROBE THAT IS NOT RUN SAYS WHICH GATE SHUT. One row a gate of
/// the loop's — each the same world with ONE thing changed — and for each:
/// nothing is written, and ONE line is said,
/// `SKIPPED seq=<n> rule=usage-probe@v1 gate=<word>`, the claim's with the
/// host's own word after it. Said once a point: the step asked again and
/// the point decided again, the read still advised, add nothing
/// ([`due_probe_at_a_gate`]). The policy's own gates have their words in
/// its table (`the_probes_gate_is_true_at_a_plain_idle_point_…`); the one
/// of them only a fresh `status` can shut, a person's grace, is here too.
///
/// NEGATIVE CONTROL: the same world with nothing changed RUNS the probe —
/// `/usage`, the sample, the Escape — and says no skip at all, though the
/// point is then decided again with the read still advised.
#[test]
fn a_due_probe_that_is_not_run_says_which_gate_shut_once_a_point() {
    use crate::supervise::policy::turn_end::ProbeGate;
    let gates: [(&str, Shut); 15] = [
        ("watching", |s, _, _, _| {
            s.claim = claim::Claim::Behind {
                holder: "another".to_string(),
                tried: Instant::now(),
            };
        }),
        ("no-ledger", |s, _, _, _| s.set_approval_ledger(None)),
        ("probes-off", |s, _, _, _| s.probe_off = true),
        ("panel-owed", |s, _, _, _| {
            s.probe = UsageProbe::Owed {
                own: true,
                since: Instant::now(),
            };
        }),
        ("not-live", |s, _, _, _| s.turn_live = false),
        ("escalated", |_, _, _, decided| {
            *decided = TurnEndAction::Escalate {
                reason: "a person's".to_string(),
            };
        }),
        ("fold-latch", |s, point, _, _| {
            s.probe_fold = Some((point.screen.cols, point.screen.pane_rows));
        }),
        ("host-wants", |_, _, host, _| {
            host.wanting.store(true, Ordering::SeqCst);
        }),
        ("stalled", |s, _, _, _| s.stall_suspect = true),
        ("wake-settle", a_night_slept),
        ("no-fence", |s, _, _, _| s.caps.gone = Some(false)),
        ("not-claude", |s, _, _, _| s.ctl.program = "zsh"),
        ("person", |s, _, _, _| s.ctl.human_ms = Some(5_000)),
        ("not-fullscreen", |s, _, _, _| {
            s.ctl.alt_screen = Some(false);
        }),
        ("claim host=not-due", |_, _, host, _| {
            *host.claims.lock().unwrap() = VecDeque::from([false]);
        }),
    ];
    for (word, shut) in gates {
        let tag = format!("probe-gate-{}", word.replace([' ', '='], "-"));
        let (did, said, wrote, claims) = due_probe_at_a_gate(&tag, Some(shut));
        assert_eq!(did, Ok(Probed::No), "{word}");
        assert_eq!(
            said,
            [format!("SKIPPED seq=101 rule=usage-probe@v1 gate={word}")],
            "{word}: one line, once"
        );
        assert_eq!(wrote, Vec::<&str>::new(), "{word}: nothing written");
        // The host is asked for its claim only past every gate of the
        // loop's: at each of the two steps there, and both refused — which
        // is said once.
        let asked = if word.starts_with("claim") { 2 } else { 0 };
        assert_eq!(claims, vec!["probe:declined"; asked], "{word}");
    }
    // Every gate of the loop's has its row (a point decided again is
    // [`a_point_decided_again_says_not_new`]'s).
    let rows: Vec<&str> = gates
        .iter()
        .filter_map(|(word, _)| word.split(' ').next())
        .collect();
    for gate in &ProbeGate::ALL[12..] {
        assert!(
            *gate == ProbeGate::NotNew || rows.contains(&gate.word()),
            "{}",
            gate.word()
        );
    }

    // NEGATIVE CONTROL: nothing shut. The probe runs, and no skip is said —
    // not at the step, not where the point is decided again after it.
    let (did, said, wrote, claims) = due_probe_at_a_gate("probe-gate-none", None);
    assert_eq!(did, Ok(Probed::Done));
    assert_eq!(
        said,
        ["PROBED seq=101 rule=usage-probe@v1 /usage session=1% week=10%"]
    );
    assert_eq!(wrote, ["/usage", "esc"]);
    assert_eq!(claims, ["probe:claimed", "probed:ok"]);
}

/// A DUE PROBE NOT RUN, IN THE WHOLE LOOP: the roster advises
/// the probe at a turn end whose turn the loop never read LIVE — a reply
/// that came and went inside the settle after the Enter — and nothing is
/// typed. The loop SAYS so, printed (a hosted loop's printed lines are
/// its host's log) and journaled, right after the point's own line: `SKIPPED
/// … gate=not-live`. And a host that wants the point for a step of its own
/// is `gate=host-wants` — where the point is OFFERED to it and it takes it
/// (the look returns before the probe's step is ever reached), and where a
/// guard of the loop's withholds the point from it (the probe's own gate).
/// Each ONCE, for as long as the session then sits at that point.
///
/// NEGATIVE CONTROL: the same turn end read live, under the same host, is
/// probed and says no skip ([`a_claimed_probe_types_usage_samples_the_
/// settled_panel_and_closes_it`]); and with no roster nothing is said
/// (`gates_said` is empty for a host that advises nothing).
#[test]
fn the_loop_says_once_why_a_due_probe_was_not_run() {
    let idle_policy = || SupervisorConfig {
        continue_policy: false,
        ..probe_policy(120)
    };
    let run = |tag: &str, screens: Vec<Vec<String>>, host: &Arc<ProbeHost>| {
        let (dir, ledger) = ledger_at(tag);
        let mut m = probe_mock(screens, &[panel_fx::USAGE_SETTLED], host);
        // The cursor on the composer's caret row: the idle point a host is
        // offered (off it, the point is withheld from the host).
        m.cursor_on_caret = tag.ends_with("-offered");
        let (lines, journal) = probe_run_for(
            1,
            &format!("te-{tag}"),
            &mut m,
            host,
            &ledger,
            idle_policy(),
            |_| {},
        );
        let _ = std::fs::remove_dir_all(&dir);
        (lines, journal, probe_writes(&m))
    };

    // Never read live: the point is the first thing this loop sees.
    let host = ProbeHost::probing(true);
    let (lines, journal, wrote) = run("probe-loop-not-live", vec![ended(PROBE_SAID)], &host);
    assert_eq!(gates_said(&lines), ["101 not-live"], "{lines:#?}");
    assert_eq!(gates_said(&journal), gates_said(&lines), "{journal:#?}");
    let at = lines
        .iter()
        .position(|l| l.starts_with("SKIPPED "))
        .expect("said");
    assert!(
        lines[at - 1].starts_with("EVENT idle seq=101 "),
        "right after the point's own line: {lines:#?}"
    );
    assert_eq!(wrote, Vec::<&str>::new());
    assert!(host.said("probe").is_empty(), "no claim asked");

    // The host wants the point: offered it and taking it, and withheld it.
    let live = vec![busy_screen(), ended(PROBE_SAID)];
    for (tag, offered) in [
        ("probe-loop-host-wants-offered", true),
        ("probe-loop-host-wants-withheld", false),
    ] {
        let host = ProbeHost::probing(true);
        host.wanting.store(true, Ordering::SeqCst);
        let (lines, journal, wrote) = run(tag, live.clone(), &host);
        assert_eq!(gates_said(&lines), ["102 host-wants"], "{tag}: {lines:#?}");
        assert_eq!(gates_said(&journal), gates_said(&lines), "{tag}");
        assert_eq!(wrote, Vec::<&str>::new(), "{tag}");
        assert!(host.said("probe").is_empty(), "{tag}");
        assert_eq!(!host.said("at-idle").is_empty(), offered, "{tag}");
    }

    // NEGATIVE CONTROL: no roster — nothing advised, nothing said.
    let host = ProbeHost::scripted(&[None], &[true]);
    let (lines, journal, wrote) = run("probe-loop-no-roster", live, &host);
    assert_eq!(gates_said(&journal), Vec::<String>::new(), "{lines:#?}");
    assert_eq!(wrote, Vec::<&str>::new());

    // ONCE A POINT, AND A POINT AFTER A TURN IS ANOTHER — though it reads the
    // very same: two turn ends on the same words, the host refusing its
    // claim at each, have a line each (and the third, a person's, its own).
    let (dir, ledger) = ledger_at("probe-loop-same-words");
    let host = ProbeHost::probing(false);
    let mut m = probe_mock(
        vec![busy_screen(), ended(PROBE_SAID)],
        &[panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).then.extend([
        vec![busy_screen(), ended(PROBE_SAID)],
        vec![busy_screen(), ended(STOP)],
    ]);
    let (lines, _) = probe_run(
        "te-probe-loop-same-words",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        gate_words(&lines),
        ["claim host=not-due", "claim host=not-due", "escalated"],
        "{lines:#?}"
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE POINT IS NOT NEW: a probe goes only at the look that first takes a
/// point, so one the roster first calls due where the point is decided
/// AGAIN — a wait that ran out, the accounts' news — is not run, and says
/// `gate=not-new`, once: decided a third time, nothing more. NEGATIVE
/// CONTROL: with no probe advised (the host's first answer here) the
/// decision says nothing at all.
#[test]
fn a_point_decided_again_says_not_new() {
    let (dir, ledger) = ledger_at("probe-gate-not-new");
    // No advice at the point's first decision; the probe from then on.
    let host = ProbeHost::scripted(
        &[None, Some(AccountAdvice::Probe { otherwise: None })],
        &[true],
    );
    let mut m = probe_mock(vec![ended(PROBE_SAID)], &[panel_fx::USAGE_SETTLED], &host);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            continue_policy: false,
            ..hosted(30).policy
        },
        ..hosted(30)
    };
    let mut s = session(&mut m, None);
    s.set_clock(PROBE_NOW, PDT);
    s.set_approval_ledger(Some(ledger));
    probe_waits(&mut s);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let mut heard = Heard::default();
    let decided = s
        .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.turn_live = true;
    assert_eq!(
        s.usage_probe(&point, &decided, &opts, &mut heard),
        Ok(Probed::No)
    );
    assert_eq!(heard.said, Vec::<String>::new(), "control: nothing advised");
    for _ in 0..3 {
        s.turn_end_now(&point, &opts, &[], &mut heard)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
    assert_eq!(
        host.said("account").len(),
        4,
        "asked at every decision: {:?}",
        host.said("")
    );
    assert_eq!(heard.said.len(), 1, "{:?}", heard.said);
    assert!(
        heard.said[0].starts_with("SKIPPED seq=")
            && heard.said[0].ends_with(" rule=usage-probe@v1 gate=not-new"),
        "{:?}",
        heard.said
    );
    assert!(host.said("probe").is_empty(), "no claim asked");
    drop(s);
    assert_eq!(probe_writes(&m), Vec::<&str>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE PROBE MOVES NONE OF THE POLICY'S MEMORY: the turn-end policy's state
/// after the step — the hourly budget, the short-turn streak, the point it
/// counts from, the done reports, the act awaited, everything it holds — is
/// EQUAL to its state before, though `/usage` and an Escape were written
/// and a sample handed over. NEGATIVE CONTROL: the same command booked as an
/// act of the policy's ([`TurnEndState::acted`]) moves the budget and the
/// awaited act; and a point the policy escalates is never probed.
#[test]
fn a_probe_books_no_act_and_moves_none_of_the_policys_memory() {
    use crate::supervise::policy::turn_end::{RULE_COMPACT, Then};
    let (dir, ledger) = ledger_at("probe-books");
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(
        vec![ended(PROBE_SAID)],
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut s = session(&mut m, None);
    s.set_clock(PROBE_NOW, PDT);
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.clone()));
    s.ledger = crate::supervise::approvals::Ledger::open(Some(&ledger), None, &mut Vec::new());
    probe_waits(&mut s);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let decided = s
        .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(
        matches!(decided, TurnEndAction::Type { .. }),
        "the point's own act is a continuation: {decided:?}"
    );

    // A point decided again (no turn read live at this look) is not probed.
    assert_eq!(
        s.usage_probe(&point, &decided, &opts, &mut Alone),
        Ok(Probed::No)
    );
    // Nor one the policy hands to a person.
    s.turn_live = true;
    let escalated = TurnEndAction::Escalate {
        reason: "a person's".to_string(),
    };
    assert_eq!(
        s.usage_probe(&point, &escalated, &opts, &mut Alone),
        Ok(Probed::No)
    );
    assert!(host.said("probe").is_empty(), "no claim asked for either");

    let before = s.turn_end.clone();
    let now = Instant::now();
    let booked = before.acts_in_window(now);
    assert_eq!(
        s.usage_probe(&point, &decided, &opts, &mut Alone),
        Ok(Probed::Done)
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    assert_eq!(
        s.turn_end, before,
        "the policy's memory, whole: nothing of it moved"
    );
    assert_eq!(s.turn_end.acts_in_window(Instant::now()), booked);
    assert!(!s.turn_end.act_in_flight());
    assert_eq!(s.probe, UsageProbe::Idle);
    assert!(!s.turn_live || s.probe_fold.is_none() && !s.probe_off);

    // NEGATIVE CONTROL: booked as an act, the same command moves the books.
    let reading = s.turn_end_reading(&point.screen, None, &opts);
    let mut acted = before.clone();
    acted.acted(
        &TurnEndAction::TypeCommand {
            command: "/usage".to_string(),
            rule_id: RULE_COMPACT,
            then: Then::Nothing,
        },
        &reading,
        now,
    );
    assert_ne!(acted, before);
    assert_eq!(acted.acts_in_window(now), booked + 1);
    assert!(acted.act_in_flight());
    drop(s);
    assert_eq!(probe_writes(&m), ["/usage", "esc"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// NOT OPENED: `/usage` and its Enter were written and no panel came within
/// the bound. NO ESCAPE is pressed — nothing says a panel is there to take
/// one — the host is told `not-opened`, the composer's return settles what
/// was owed, and the point's own act goes on. A TURN THAT STARTS INSTEAD (a
/// spinner after the Enter) is the same, at once, with no Escape either —
/// and the point having changed across the probe, this loop probes no more.
#[test]
fn a_panel_that_never_opens_gets_no_escape() {
    let (dir, ledger) = ledger_at("probe-not-opened");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).opens = false;
    let (lines, journal) = probe_run(
        "te-probe-not-opened",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert_eq!(probe_writes(&m), ["/usage"], "no Escape");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:not-opened"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert!(
        journal
            .iter()
            .any(|l| l == "PROBED seq=102 rule=usage-probe@v1 /usage no sample (not-opened)"),
        "{journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    // A turn starts instead.
    let (dir, ledger) = ledger_at("probe-turn-instead");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).turn_instead = Some(vec![busy_screen(), busy_screen(), ended(STOP)]);
    let (lines, journal) = probe_run(
        "te-probe-turn-instead",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage", "enter"], "{lines:#?}");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:not-opened"]);
    assert!(
        journal
            .iter()
            .any(|l| l.contains("the point changed across the probe")),
        "{journal:#?}"
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// UNSETTLED: a panel that says `Refreshing…` for the whole bound is never
/// sampled — the host is told `unsettled` — and gets its one Escape.
#[test]
fn a_panel_that_never_settles_is_closed_with_no_sample() {
    let (dir, ledger) = ledger_at("probe-unsettled");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_REFRESHING], &host);
    let (lines, journal) = probe_run(
        "te-probe-unsettled",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |s| {
            let ms = Duration::from_millis;
            s.set_probe_waits(ms(60), ms(200), ms(10), ms(100));
        },
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "esc", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:unsettled"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert!(
        journal
            .iter()
            .any(|l| l.ends_with("/usage no sample (unsettled)")),
        "{journal:#?}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("PROBED")), "{lines:#?}");
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// One watch over two turn ends — the second on a screen `taller` rows
/// taller than the first — under a host that grants every claim, the panel
/// showing `frames`: what the probe wrote, and what the host was told.
fn two_turn_ends(tag: &str, frames: &[&str], taller: usize) -> (Vec<&'static str>, Vec<String>) {
    let (dir, ledger) = ledger_at(tag);
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(vec![busy_screen(), ended(PROBE_SAID)], frames, &host);
    let mut second = vec![String::new(); taller];
    second.extend(ended("Stage 4 done; the suite is green."));
    world(&mut m).then.extend([
        vec![busy_screen(), second],
        vec![busy_screen(), ended(STOP)],
    ]);
    let (lines, _) = probe_run(
        &format!("te-{tag}"),
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        usage_probe_violations(&m, &host),
        Vec::<String>::new(),
        "{lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    (probe_writes(&m), host.said("probe"))
}

/// THE FOLD. A grid on which the panel's tail is under the fold from the
/// first read (measured: 110x32, the last row the vendor's `↓` alone) is no
/// sample — the host is told `fold` — the panel gets its one Escape, and
/// THAT GRID SIZE IS NOT PROBED AGAIN by this loop: at the next turn end no
/// claim is asked and nothing is typed. NEGATIVE CONTROL: after a resize (a
/// taller pane) the gate admits one.
///
/// A RACE LOST IS NOT A SIZE: where a read of the probe DID show the panel's
/// tail region before it folded (measured at 150x50: the refresh line for a
/// quarter second, the whole settled tab for about a second, then the
/// contributing block fills in and the tail is gone), the host is told
/// `fold` and the size is tried again at the next turn end.
#[test]
fn a_folded_panel_is_no_sample_and_its_grid_size_is_not_probed_again() {
    let (wrote, told) = two_turn_ends("probe-fold", &[panel_fx::USAGE_SHORT], 0);
    assert_eq!(wrote, ["/usage", "esc"], "one probe, one Escape");
    assert_eq!(
        told,
        ["probe:claimed", "probed:fold"],
        "the second turn end asks for no claim"
    );

    // NEGATIVE CONTROL: a resize.
    let (wrote, told) = two_turn_ends("probe-fold-resized", &[panel_fx::USAGE_SHORT], 3);
    assert_eq!(wrote, ["/usage", "esc", "/usage", "esc"]);
    assert_eq!(
        told,
        [
            "probe:claimed",
            "probed:fold",
            "probe:claimed",
            "probed:fold"
        ]
    );

    // The race lost: unsettled, settled ONCE, then folded for good.
    let lost = [
        panel_fx::USAGE_REFRESHING,
        panel_fx::USAGE_SETTLED,
        panel_fx::USAGE_FOLD,
    ];
    let (wrote, told) = two_turn_ends("probe-fold-race", &lost, 0);
    assert_eq!(wrote, ["/usage", "esc", "/usage", "esc"]);
    assert_eq!(
        told,
        [
            "probe:claimed",
            "probed:fold",
            "probe:claimed",
            "probed:fold"
        ],
        "no sample from one settled read, and no latch on the size"
    );
    // The race lost outright: the refresh line read once (the tail region
    // was on the grid), then the fold — the settled second never read.
    let lost = [panel_fx::USAGE_REFRESHING, panel_fx::USAGE_FOLD];
    let (wrote, _) = two_turn_ends("probe-fold-race-lost", &lost, 0);
    assert_eq!(wrote, ["/usage", "esc", "/usage", "esc"]);
}

/// ONLY UNDER THE FULLSCREEN RENDERER: with the agent on the main screen
/// (`modes` says `alt_screen=false`: Claude Code's inline renderer), or a
/// host whose `modes` names no such row, nothing is typed and no claim is
/// asked. The control is every other test here: `alt_screen=true` types.
#[test]
fn the_inline_renderer_is_never_probed() {
    for alt in [Some(false), None] {
        let (dir, ledger) = ledger_at("probe-inline");
        let host = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_INLINE], &host);
        m.alt_screen = alt;
        let (lines, _) = probe_run(
            "te-probe-inline",
            &mut m,
            &host,
            &ledger,
            probe_policy(120),
            |_| {},
        );
        assert_eq!(
            m.writes(),
            ["text:keep going", "enter"],
            "{alt:?}: {lines:#?}"
        );
        assert_eq!(count(&m, "modes"), 1, "{alt:?}: asked once, at the gate");
        assert!(host.said("probe").is_empty(), "{alt:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A PERSON. One who typed within the grace of the point (`status
/// human_ms=`) shuts the gate: no claim, nothing written. One whose key
/// lands between the judged read and the write skips it under the person
/// fence (`OK skipped reason=person`): the host is told `person`, nothing is
/// in the composer and no Enter follows. One who keys WHILE THE PANEL IS
/// READ ends the sample — `person` — and the panel's Escape waits out their
/// grace: `HELD`, said once, and one Escape no sooner than the grace after
/// their key.
#[test]
fn a_person_at_the_session_is_never_typed_under_or_sampled_over() {
    // Typed just before the point.
    let (dir, ledger) = ledger_at("probe-person-before");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.human_ms = Some(5_000);
    let (lines, _) = probe_run(
        "te-probe-person-before",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), Vec::<String>::new(), "{lines:#?}");
    assert!(host.said("probe").is_empty());
    let _ = std::fs::remove_dir_all(&dir);

    // A key between the read and the write.
    let (dir, ledger) = ledger_at("probe-person-at-send");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.person_at_send = 1;
    let (lines, journal) = probe_run(
        "te-probe-person-at-send",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage"], "asked once: {lines:#?}");
    assert_eq!(m.person_skipped.len(), 1, "and skipped for the person");
    assert_eq!(world(&mut m).at, PanelAt::Rest, "nothing in the composer");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:person"]);
    assert!(
        journal.iter().any(|l| l
            == "SKIPPED seq=102 rule=usage-probe@v1 a person keyed before the write; nothing \
                written"),
        "{journal:#?}"
    );
    assert!(
        probe_rows(&ledger).is_empty(),
        "nothing written, nothing owed"
    );
    let _ = std::fs::remove_dir_all(&dir);

    // A key while the panel is read: the grace is a second here.
    let (dir, ledger) = ledger_at("probe-person-reading");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).person_after = Some(2);
    world(&mut m).grace = Duration::from_secs(1);
    let (lines, journal) = probe_run(
        "te-probe-person-reading",
        &mut m,
        &host,
        &ledger,
        probe_policy(1),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage", "enter", "esc"], "{lines:#?}");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:person"]);
    assert!(host.samples.lock().unwrap().is_empty());
    let w = world(&mut m);
    let keyed = w.keyed_at.expect("the person keyed");
    let esc = w.probes.last().expect("the Escape").at;
    assert!(
        esc.duration_since(keyed) >= Duration::from_secs(1),
        "the Escape came {:?} after the person's key",
        esc.duration_since(keyed)
    );
    let held: Vec<&String> = journal.iter().filter(|l| l.starts_with("HELD ")).collect();
    assert_eq!(held.len(), 1, "{journal:#?}");
    assert!(
        held[0].contains("a person is typing") && held[0].ends_with("(the usage panel's Escape)"),
        "{held:?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// REFUSALS. An Escape the server turns away — `ERR halted`, then `ERR
/// busy` — is parked on or backed off from, spends no try, and is SENT
/// AGAIN: three Escapes asked, the third written. Through all of it no
/// second `/usage` is typed while the panel is owed, and the sample, handed
/// over before the first Escape, is not lost to the close.
#[test]
fn a_refused_escape_is_sent_again_and_never_a_second_usage() {
    let (dir, ledger) = ledger_at("probe-refused");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).esc_replies = [err("halted by the owner"), err("busy turn=1")].into();
    let (lines, journal) = probe_run(
        "te-probe-refused",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        probe_writes(&m),
        ["/usage", "esc", "esc", "esc"],
        "{lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    assert_eq!(world(&mut m).at, PanelAt::Rest, "closed by the third");
    assert!(
        journal.iter().any(|l| l.starts_with("HALTED "))
            && journal.iter().any(|l| l.starts_with("REFUSED ")),
        "{journal:#?}"
    );
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("panel is open")),
        "a refusal spends no try: {journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE LADDER. An Escape the server writes and the panel does not take is
/// tried again after the survey's pauses (shortened here), `WAITING`
/// journaled at each failure; at the SECOND failure the host is told, once
/// (`The /usage panel did not close` / `Press Esc in this tab.` — a record,
/// no badge), and the tries go on until one takes: `CLEARED`. Then the
/// point is decided again and its act typed — the panel never cost the
/// session its continuation.
#[test]
fn a_panel_that_does_not_take_its_escape_is_tried_on_the_ladder_and_told_once() {
    let (dir, ledger) = ledger_at("probe-ladder");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).esc_fails = 3;
    // The re-decision comes two seconds after the close, in waits a scripted
    // server answers 5 ms apart: enough of them are let pass.
    m.vanish_after = Some(700);
    let (lines, journal) = probe_run(
        "te-probe-ladder",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |s| s.set_survey_retry([Duration::from_millis(20); SURVEY_TRIES]),
    );
    assert_eq!(
        probe_writes(&m),
        ["/usage", "esc", "esc", "esc", "esc"],
        "{lines:#?}"
    );
    assert_eq!(
        host.said("inform"),
        ["inform:The /usage panel did not close\nPress Esc in this tab."],
        "told once, at the second failure"
    );
    let waiting: Vec<&String> = journal
        .iter()
        .filter(|l| l.starts_with("WAITING ") && l.contains("the /usage panel is open"))
        .collect();
    assert_eq!(waiting.len(), 3, "{journal:#?}");
    for (i, line) in waiting.iter().enumerate() {
        assert!(
            line.contains(&format!("({} tries): Escape again in 0 s", i + 1)),
            "{line}"
        );
    }
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.ends_with(" usage panel") && l.starts_with("CLEARED "))
            .count(),
        1,
        "{journal:#?}"
    );
    assert_eq!(count(&m, "meta set attention"), 1, "the STOP point's alone");
    assert_eq!(
        m.writes()
            .iter()
            .filter(|w| *w == "text:keep going")
            .count(),
        1,
        "the point's own act, after the panel closed: {lines:#?}"
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A world that STARTS with the panel on the screen — `frame` — over the
/// composer screen it returns to: what a loop meets that starts after the
/// one that typed `/usage` died.
fn panel_standing(frame: &str, host: &ProbeHost) -> Mock {
    let mut m = probe_mock(vec![panel_fx::screen(frame)], &[frame], host);
    let w = world(&mut m);
    w.at = PanelAt::Open;
    w.rest = Some(ended(PROBE_SAID));
    m
}

/// The session ledger of a loop that wrote `/usage` and never saw the panel
/// gone.
fn owed_ledger(path: &Path) {
    let row = crate::supervise::approvals::Row {
        rule_id: "usage-probe@v1",
        outcome: crate::supervise::approvals::Outcome::Typed,
        command: "/usage",
        reason: crate::supervise::approvals::PROBE_TYPED_REASON,
        box_seq: 7,
    };
    std::fs::write(path, format!("{}\n", row.to_json(1_000, None))).expect("the ledger");
}

/// THE LOOP DIED MID-PROBE. Its successor finds the ledger's `typed` row
/// with no `waiting` row after it and a Settings panel on the screen: it
/// lets the panel stand ONE PERSON'S GRACE in its own sight (a second here),
/// closes it with one guarded Escape, writes the `waiting` row, and SAMPLES
/// NOTHING — no claim is asked, the host is told nothing.
///
/// NEGATIVE CONTROLS. With no such row — a person's own `/usage`, on every
/// machine that never ran a probe — the same panel gets NO KEY AT ALL, for
/// as long as the loop runs. And with the row but NO HEADER ROW ON THE GRID
/// (the measured inline panel once it scrolled: the cancel hint in sight,
/// its rule and header gone) nothing is pressed either: an Escape is only
/// ever written under the panel's own header.
#[test]
fn a_successor_closes_only_a_panel_the_ledger_says_the_harness_opened() {
    let (dir, ledger) = ledger_at("probe-successor");
    owed_ledger(&ledger);
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
    let started = Instant::now();
    let (lines, _) = probe_run(
        "te-probe-successor",
        &mut m,
        &host,
        &ledger,
        probe_policy(1),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["esc"], "{lines:#?}");
    let esc = &world(&mut m).probes[0];
    assert!(
        esc.at.duration_since(started) >= Duration::from_secs(1),
        "one grace in the successor's own sight: {:?}",
        esc.at.duration_since(started)
    );
    assert_eq!(
        esc.request,
        "key if-human=0 if=^...Settings..Status...Config...Usage esc"
    );
    assert!(
        host.said("probe").is_empty(),
        "nothing sampled: {:?}",
        host.said("")
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: no row. The panel is a person's.
    let (dir, ledger) = ledger_at("probe-not-ours");
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
    let (lines, _) = probe_run_for(
        1,
        "te-probe-not-ours",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(m.writes(), Vec::<String>::new(), "{lines:#?}");
    assert_eq!(world(&mut m).at, PanelAt::Open, "it stands as it stood");
    assert!(probe_rows(&ledger).is_empty());
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: the row, and no header row on the grid.
    let (dir, ledger) = ledger_at("probe-no-header");
    owed_ledger(&ledger);
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_INLINE_SCROLLED, &host);
    let (lines, _) = probe_run_for(
        1,
        "te-probe-no-header",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(m.writes(), Vec::<String>::new(), "{lines:#?}");
    assert_eq!(world(&mut m).at, PanelAt::Open);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A LOOP THAT ONLY WATCHES WAITS ON THE PANEL. The ledger says a panel is
/// owed and one stands, but another supervisor holds the session's claim
/// (`ERR busy supervisor=gui%3A1`): the panel is the holder's to close, and
/// this loop asks for no Escape — none is sent, and no `REFUSED` is
/// journaled for one its own claim would refuse. It waits on the panel's
/// header row, and once the panel is gone and the composer back it writes
/// the `waiting` row: the panel is owed no more.
///
/// NEGATIVE CONTROL: the same world with the claim free gets its one
/// guarded Escape.
#[test]
fn a_loop_watching_behind_another_claim_asks_no_escape_of_an_owed_panel() {
    let run = |tag: &str, behind: bool| {
        let (dir, ledger) = ledger_at(tag);
        owed_ledger(&ledger);
        let host = ProbeHost::probing(true);
        let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
        if behind {
            m.claim_replies = VecDeque::from([err("busy supervisor=gui%3A1")]);
            // The holder closes it: it leaves by itself after a few reads.
            world(&mut m).leaves_after = Some(3);
        }
        let (lines, journal) =
            probe_run_for(1, tag, &mut m, &host, &ledger, probe_policy(0), |s| {
                s.set_claim_timing(Duration::from_secs(3600), 90_000)
            });
        let rows = probe_rows(&ledger);
        let _ = std::fs::remove_dir_all(&dir);
        (m, lines, journal, rows)
    };
    let refused = |journal: &[String]| {
        journal
            .iter()
            .filter(|l| l.starts_with("REFUSED "))
            .cloned()
            .collect::<Vec<_>>()
    };

    let (mut m, lines, journal, rows) = run("probe-watching", true);
    assert!(
        lines[0].starts_with("WATCHING another supervisor holds this session: gui:1"),
        "{lines:#?}"
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new(), "{lines:#?}");
    assert!(
        !m.requests.iter().any(|r| r.ends_with(" esc")),
        "{:#?}",
        m.requests
    );
    assert_eq!(refused(&journal), Vec::<String>::new());
    assert_eq!(world(&mut m).at, PanelAt::Rest, "the holder closed it");
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );

    // NEGATIVE CONTROL: the claim free.
    let (m, lines, journal, rows) = run("probe-watching-free", false);
    assert_eq!(probe_writes(&m), ["esc"], "{lines:#?}");
    assert_eq!(refused(&journal), Vec::<String>::new());
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
}

/// AN OUTAGE MID-PROBE: the answer to a read of the open panel never comes.
/// The step ends there — the host is told `lost`, nothing sampled — the
/// outage is ridden out, and THE SAME LOOP, which remembers the panel it
/// owes, closes it at its next look with one guarded Escape.
#[test]
fn an_outage_in_the_read_leaves_the_panel_owed_and_the_same_loop_closes_it() {
    let (dir, ledger) = ledger_at("probe-outage");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).lose_read = Some(2);
    let (lines, _) = probe_run(
        "te-probe-outage",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |s| s.pause = Duration::from_millis(5),
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:lost"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.starts_with("RECONNECT ")),
        "{lines:#?}"
    );
    assert_eq!(world(&mut m).at, PanelAt::Over, "closed, and the act typed");
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE PANEL'S REPAINT IS NOT WORK. A panel the harness owes its close
/// repaints for ever (no read ever finds the screen still): the loop does
/// not read it as output being written — the worker working — so the host
/// hears of no turn that ran, nothing is printed for it, and the panel is
/// closed. NEGATIVE CONTROL: the same repainting screen with NO panel owed
/// (a person's own) reads as it always did — busy, until it leaves — and
/// the host is told a turn ran.
#[test]
fn an_owed_panels_repaint_is_never_the_worker_working() {
    let grace = || probe_policy(0);
    let (dir, ledger) = ledger_at("probe-repaint");
    owed_ledger(&ledger);
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
    world(&mut m).repaints = true;
    let (lines, _) = probe_run("te-probe-repaint", &mut m, &host, &ledger, grace(), |_| {});
    assert_eq!(probe_writes(&m), ["esc"], "{lines:#?}");
    assert_eq!(host.said("turn-ran"), Vec::<String>::new(), "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: not owed. It leaves by itself after eight reads.
    let (dir, ledger) = ledger_at("probe-repaint-not-ours");
    let host = ProbeHost::probing(true);
    let mut m = panel_standing(panel_fx::USAGE_SETTLED, &host);
    world(&mut m).repaints = true;
    world(&mut m).leaves_after = Some(8);
    let (lines, _) = probe_run(
        "te-probe-repaint-not-ours",
        &mut m,
        &host,
        &ledger,
        grace(),
        |_| {},
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new(), "{lines:#?}");
    assert_eq!(host.said("turn-ran"), ["turn-ran"], "{lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// PROBE, THEN MOVE. The roster says the read is due with nothing behind it
/// (`Probe { otherwise: None }`); the probe is made; the point is decided
/// again, the host asked afresh — and now, the sample in, it advises the
/// move: the relaunch under the other account is asked AT THE SAME POINT,
/// ahead of any wall, and the loop types no continuation of its own.
#[test]
fn a_sample_that_shows_the_account_low_moves_the_session_at_the_same_point() {
    let (dir, ledger) = ledger_at("probe-then-move");
    let host = ProbeHost::scripted(
        &[
            Some(AccountAdvice::Probe { otherwise: None }),
            Some(AccountAdvice::Move {
                to: "work-2".to_string(),
            }),
        ],
        &[true],
    );
    *host.answer.lock().unwrap() = Some("adopted");
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    let (lines, _) = probe_run(
        "te-probe-then-move",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        host.said(""),
        [
            "turn-ran",
            "account:turn-end",
            "probe:claimed",
            "probed:ok",
            "account:turn-end",
            "restart:account-ahead:work-2",
        ],
        "{lines:#?}"
    );
    assert_eq!(m.writes(), ["text:/usage", "enter", "esc"], "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("RESTARTED seq=") && l.contains("relaunch under account work-2")),
        "{lines:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE POINT CHANGED ACROSS THE PROBE: the screen the panel leaves is not
/// the point it was typed at (rows left in the transcript, as the inline
/// renderer leaves them). NOTHING of the point's is carried out on it —
/// `SKIPPED`, journaled once — and this loop probes no more.
#[test]
fn a_point_that_changed_across_the_probe_is_not_acted_on() {
    let (dir, ledger) = ledger_at("probe-point-changed");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    world(&mut m).after = Some(ended("Something else stands here now."));
    let (lines, journal) = probe_run(
        "te-probe-point-changed",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage", "enter", "esc"], "{lines:#?}");
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.starts_with("SKIPPED ")
                && l.contains("rule=usage-probe@v1 the point changed across the probe"))
            .count(),
        1,
        "{journal:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE PANEL HALF ERASED, as one read caught it right after the Escape.
/// MEASURED on 2.1.287, 2026-10-01 (150x80; the read taken as soon as the
/// header row had left, a few tens of milliseconds after the key, in one
/// close of the live check's eight): the transcript redrawn at the top, the
/// panel's rule, header and upper rows GONE, its lower rows down to the
/// cancel hint still on the grid, and the composer NOT DRAWN YET. COMPOSED
/// here on that shape — the capture's own banner names the login — from the
/// measured settled screen: the turn's rows, then that screen's rows from
/// its last window's reset down.
fn panel_torn() -> Vec<String> {
    let settled = panel_fx::screen(panel_fx::USAGE_SETTLED);
    let from = settled
        .iter()
        .rposition(|row| row.trim_start().starts_with("Resets "))
        .expect("a window's reset row");
    let mut torn = ended(PROBE_SAID);
    let frame = torn
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the composer's frame");
    torn.truncate(frame);
    torn.extend(settled[from..].iter().cloned());
    torn
}

/// A HALF-ERASED PANEL IS NO PANEL AND NO NEW POINT. The read that follows
/// the Escape can catch the panel mid-erase ([`panel_torn`], measured): no
/// header row, no composer. The loop takes it for neither a panel that did
/// not take its Escape — NO SECOND ESCAPE, no failed try — nor a point that
/// changed across the probe: the panel stays owed for one more look, the
/// composer's return closes it (the ledger's `waiting` row), and the point's
/// own act is decided again and typed. The sample, handed over before the
/// Escape, is the settled panel's.
///
/// NEGATIVE CONTROL: the same world where the Escape does NOT take — the
/// panel, header and all, still on the grid at the read after it — is a
/// failed try: `WAITING … (1 tries)`, and a second Escape.
#[test]
fn a_half_erased_panel_after_the_escape_gets_no_second_escape() {
    let torn = panel_torn();
    assert_eq!(aterm_phase::settings_panel(&torn), None);
    assert!(!has_composer_frame(&torn));
    assert!(torn.iter().any(|row| aterm_phase::panel_tail(row)));
    let run = |tag: &str, edit: &dyn Fn(&mut PanelWorld)| {
        let (dir, ledger) = ledger_at(tag);
        let host = ProbeHost::probing(true);
        let mut m = probe_world(
            &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
            &host,
        );
        edit(world(&mut m));
        // The re-decision comes two seconds after the close, in waits a
        // scripted server answers 5 ms apart: enough of them are let pass.
        m.vanish_after = Some(700);
        let (lines, journal) = probe_run(tag, &mut m, &host, &ledger, probe_policy(120), |s| {
            s.set_survey_retry([Duration::from_millis(20); SURVEY_TRIES]);
        });
        let rows = probe_rows(&ledger);
        let bad = usage_probe_violations(&m, &host);
        let _ = std::fs::remove_dir_all(&dir);
        (m, host, lines, journal, rows, bad)
    };
    let stuck = |journal: &[String]| {
        journal
            .iter()
            .filter(|l| l.starts_with("WAITING ") && l.contains("the /usage panel is open"))
            .count()
    };

    let (mut m, host, lines, journal, rows, bad) =
        run("te-probe-torn", &|w| w.torn = Some(panel_torn()));
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "esc", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert!(
        world(&mut m).torn.is_none() && world(&mut m).mends.is_none(),
        "the half-erased frame was served to a read, and the composer after it"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    assert_eq!(
        host.samples
            .lock()
            .unwrap()
            .iter()
            .map(|(sample, pan, _)| (sample.session_pct, sample.week_pct, *pan))
            .collect::<Vec<_>>(),
        [(Some(1), Some(10), 2)]
    );
    assert_eq!(stuck(&journal), 0, "{journal:#?}");
    assert!(other_skips(&journal).is_empty(), "{journal:#?}");
    assert_eq!(
        gate_words(&journal),
        ["escalated"],
        "the later point's alone: {journal:#?}"
    );
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(bad, Vec::<String>::new());

    // NEGATIVE CONTROL: the Escape does not take, and the panel stands.
    let (m, _, lines, journal, _, bad) = run("te-probe-torn-control", &|w| w.esc_fails = 1);
    assert_eq!(probe_writes(&m), ["/usage", "esc", "esc"], "{lines:#?}");
    assert_eq!(stuck(&journal), 1, "{journal:#?}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.contains("(1 tries)")),
        "{journal:#?}"
    );
    assert_eq!(bad, Vec::<String>::new());
}

/// A PANEL THAT CLOSES UNDER THE READ IS NO NEW POINT. The panel reads
/// settled once; then it closes with no Escape of the loop's (a key the loop
/// did not write, the vendor's own repaint), and the next two reads catch it
/// half erased ([`panel_torn`]) — the step's own, which ends it with nothing
/// sampled (`closed`), and the one that decides the point again after it —
/// before the composer is drawn. A screen with no composer while the panel
/// is owed is a close not yet painted: NO `point changed across the probe`,
/// the composer's return closes what was owed (the `waiting` row), the
/// point's own act is typed once, and THE LOOP'S PROBES GO ON — the next
/// point's line names the gate that shut there (`escalated`), not
/// `probes-off`. Where the key that closed it was a PERSON'S, the act waits
/// out their grace like any act.
///
/// NEGATIVE CONTROL: the same close onto ANOTHER point, its composer drawn
/// (rows left in the transcript), still ends the loop's probes: the `point
/// changed` line, and `probes-off` at every point after it.
#[test]
fn a_panel_closed_under_the_read_is_no_new_point_and_the_probes_go_on() {
    let run = |tag: &str, edit: &dyn Fn(&mut Mock)| {
        let (dir, ledger) = ledger_at(tag);
        let host = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
        edit(&mut m);
        let (lines, journal) = probe_run(tag, &mut m, &host, &ledger, probe_policy(120), |_| {});
        let rows = probe_rows(&ledger);
        let bad = usage_probe_violations(&m, &host);
        let _ = std::fs::remove_dir_all(&dir);
        (m, host, lines, journal, rows, bad)
    };
    let changed = |journal: &[String]| {
        journal
            .iter()
            .filter(|l| l.contains("the point changed across the probe"))
            .count()
    };

    let (m, host, lines, journal, rows, bad) = run("te-probe-closed-mid-read", &|m| {
        let w = world(m);
        w.frames = vec![panel_fx::screen(panel_fx::USAGE_SETTLED), panel_torn()];
        w.leaves_after = Some(3);
    });
    assert_eq!(changed(&journal), 0, "{journal:#?}");
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "text:keep going", "enter"],
        "no Escape of the loop's, and the act once: {lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:closed"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert_eq!(
        gate_words(&journal),
        ["escalated"],
        "the later point's, and never probes-off: {journal:#?}"
    );
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(bad, Vec::<String>::new());

    // A PERSON'S Esc closed it (their key served with the half-erased
    // read): no new point either, and the act is HELD for their grace (two
    // minutes here, past the script's end) — never typed into their hands.
    let (m, _, lines, journal, rows, bad) = run("te-probe-closed-by-a-person", &|m| {
        let w = world(m);
        w.frames = vec![panel_fx::screen(panel_fx::USAGE_SETTLED), panel_torn()];
        w.leaves_after = Some(3);
        w.person_after = Some(2);
    });
    assert_eq!(changed(&journal), 0, "{journal:#?}");
    assert_eq!(m.writes(), ["text:/usage", "enter"], "{lines:#?}");
    assert!(
        journal
            .iter()
            .any(|l| l.starts_with("WAITING ") && l.ends_with("a person is typing")),
        "{journal:#?}"
    );
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(bad, Vec::<String>::new());

    // NEGATIVE CONTROL: the composer back, on another point.
    let (_, _, lines, journal, _, bad) = run("te-probe-closed-mid-read-moved", &|m| {
        // The run goes on past the point it was moved to, to the next.
        m.vanish_after = Some(700);
        let w = world(m);
        w.leaves_after = Some(1);
        w.after = Some(ended("Something else stands here now."));
    });
    assert_eq!(changed(&journal), 1, "{lines:#?}");
    assert!(
        !gate_words(&journal).is_empty() && gate_words(&journal).iter().all(|g| g == "probes-off"),
        "{journal:#?}"
    );
    assert_eq!(bad, Vec::<String>::new());
}

/// NO ESCAPE WHILE A BUSY SIGNAL SHOWS (the spec's P14, P20): Escape stops a
/// running turn, and what it does with the panel over one is not measured.
/// A panel that comes up with a turn running under it
/// ([`panel_under_a_turn`], composed: never observed) is NOT sampled — the
/// host is told `unsettled` — and gets NO Escape for as long as the turn
/// runs: it stands, owed, and no second `/usage` is typed over it.
///
/// NEGATIVE CONTROL: the same world whose turn ends — the next read shows
/// the panel with no busy row — gets its one Escape, and still no sample:
/// nothing is recorded from a panel a turn ran under.
#[test]
fn a_panel_with_a_turn_running_under_it_gets_no_escape_and_no_sample() {
    let busy = panel_under_a_turn();
    assert!(aterm_phase::settings_panel(&busy).is_some());
    assert_eq!(worker_phase(&busy), Phase::Busy);
    let still = panel_fx::screen(panel_fx::USAGE_SETTLED);
    assert_ne!(worker_phase(&still), Phase::Busy);

    let (dir, ledger) = ledger_at("probe-turn-under");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).frames = vec![busy.clone()];
    // The turn runs for as long as the run does: it ends on its budget.
    let (lines, _) = probe_run_for(
        1,
        "te-probe-turn-under",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage", "enter"], "{lines:#?}");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:unsettled"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert_eq!(world(&mut m).at, PanelAt::Open, "it stands, owed");
    assert_eq!(probe_rows(&ledger), ["typed /usage the accounts probe"]);
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: the turn ends.
    let (dir, ledger) = ledger_at("probe-turn-ended");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    world(&mut m).frames = vec![busy, still];
    let (lines, _) = probe_run(
        "te-probe-turn-ended",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:unsettled"]);
    assert!(host.samples.lock().unwrap().is_empty());
    assert_eq!(
        world(&mut m).at,
        PanelAt::Rest,
        "closed once the turn ended"
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `/usage` THE SERVER DOES NOT TAKE IS NEVER FORCED. Turned away (`ERR
/// busy`, `ERR halted`): nothing is in the composer, the host is told
/// `not-written`, the loop backs off or parks — and the point's own act,
/// not carried out at that look, is decided again and typed. Skipped on the
/// judged screen because the guard matched no row (`OK skipped`): the act
/// goes on at once, as decided, with no probe.
#[test]
fn a_usage_the_server_turns_away_or_skips_is_not_written_and_the_act_goes_on() {
    for (refusal, word) in [
        ("busy turn=1", "REFUSED "),
        ("halted by the owner", "HALTED "),
    ] {
        let (dir, ledger) = ledger_at("probe-send-refused");
        let host = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
        m.send_replies.push_back(err(refusal));
        // The re-decision is two seconds off, in 5 ms waits.
        m.vanish_after = Some(700);
        let (lines, journal) = probe_run(
            "te-probe-send-refused",
            &mut m,
            &host,
            &ledger,
            probe_policy(120),
            |_| {},
        );
        assert_eq!(
            m.writes(),
            ["text:/usage", "text:keep going", "enter"],
            "{refusal}: {lines:#?}"
        );
        assert_eq!(probe_writes(&m), ["/usage"], "asked once, no Escape");
        assert_eq!(
            host.said("probe"),
            ["probe:claimed", "probed:not-written"],
            "{refusal}"
        );
        assert!(
            journal.iter().any(|l| l.starts_with(word)),
            "{refusal}: {journal:#?}"
        );
        assert!(
            probe_rows(&ledger).is_empty(),
            "nothing written: nothing owed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The guard matched no row of the judged screen.
    let (dir, ledger) = ledger_at("probe-send-skipped");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.send_replies.push_back(ok("OK skipped seq=102\n"));
    let (lines, journal) = probe_run(
        "te-probe-send-skipped",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:not-written"]);
    assert!(
        journal.iter().any(|l| l
            == "SKIPPED seq=102 rule=usage-probe@v1 the guard matched no row of the judged \
                screen; nothing written"),
        "{journal:#?}"
    );
    assert!(probe_rows(&ledger).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// What the host is told of a `/usage` left typed (the loop's `USAGE_LEFT`).
const USAGE_LEFT_TOLD: &str = "inform:/usage was typed and not sent\nThe supervisor's own check \
                               of the account: it is sent with the session's next act, or stands \
                               until you send or clear it; its panel is closed either way.";

/// `/usage` WRITTEN AND NOT SUBMITTED (the screen moved before its Enter:
/// `OK skipped reason=changed`) IS A RECORD AND RAISES NOTHING (alerts have
/// a class, and this is none): the host is told
/// `not-written` and informed ONCE
/// ([`IdleHost::inform`], no badge), the journal holds its one `SKIPPED`
/// line — and no attention is set, no ask posted, nothing escalated. THE
/// PANEL STAYS OWED, in the ledger too, because text of the harness's that
/// stands in the composer may still be sent. When it is (a person's Enter,
/// here), the panel that opens is the harness's: the loop that follows
/// closes it, and samples nothing.
///
/// NEGATIVE CONTROL: ANOTHER RULE'S LEFTOVER STILL ESCALATES. The same world
/// and the same skipped Enter with the claim declined — so the text left typed is the continuation's — sets the
/// worker's `attention` to `typed, not submitted (…): `keep going``,
/// journals `ESCALATED`, and informs nobody. And the look that finds the
/// composer still holding `/usage` closes nothing and writes no `waiting`
/// row.
#[test]
fn usage_left_typed_is_a_record_that_raises_nothing_and_the_panel_stays_owed() {
    let raised = |m: &Mock| -> Vec<String> {
        m.requests
            .iter()
            .filter(|r| {
                r.starts_with("meta set attention") || matches!(verb_of(r), "post" | "notify")
            })
            .cloned()
            .collect()
    };
    let (dir, ledger) = ledger_at("probe-left-typed");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.tick_at_key = 1;
    // The text stands in the composer and nothing moves on: the run ends on
    // its budget.
    let (lines, journal) = probe_run_for(
        1,
        "te-probe-left-typed",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:/usage", "enter"], "{lines:#?}");
    assert_eq!(
        world(&mut m).at,
        PanelAt::Typed("/usage".to_string()),
        "it stands in the composer"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:not-written"]);
    assert_eq!(
        host.said("inform"),
        [USAGE_LEFT_TOLD],
        "told once: a record"
    );
    assert_eq!(raised(&m), Vec::<String>::new(), "nothing is raised for it");
    assert!(
        !journal.iter().any(|l| l.starts_with("ESCALATED ")),
        "{journal:#?}"
    );
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.contains("typed, not submitted"))
            .collect::<Vec<_>>(),
        [
            "SKIPPED seq=102 rule=usage-probe@v1 typed, not submitted: the screen moved before \
          Enter"
        ],
        "its one journal line: {journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe"],
        "owed: no `waiting` row while the text stands"
    );
    assert!(crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));

    // A person sends it after all: the panel opens. The loop that follows
    // (no grace here) closes it.
    assert!(m.call(&["key", "enter"]).expect("served").ok());
    assert_eq!(world(&mut m).at, PanelAt::Open);
    m.stalls = 0;
    let next = ProbeHost::probing(true);
    let (lines, _) = probe_run(
        "te-probe-left-typed-next",
        &mut m,
        &next,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert!(next.said("probe").is_empty(), "nothing sampled");
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: the claim declined, the text left typed is the
    // continuation's — escalated as every other typed act's leftover is.
    let (dir, ledger) = ledger_at("probe-left-typed-other-rule");
    let host = ProbeHost::probing(false);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.tick_at_key = 1;
    let (lines, journal) = probe_run_for(
        1,
        "te-probe-left-typed-other-rule",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(m.writes(), ["text:keep going", "enter"], "{lines:#?}");
    assert_eq!(world(&mut m).at, PanelAt::Typed("keep going".to_string()));
    assert_eq!(
        raised(&m),
        [
            "meta set attention owner=supervisor claude idle: Stage 3 done; the suite is green. \
          (typed, not submitted (the screen moved before Enter): `keep going`)"
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.starts_with("ESCALATED "))
            .count(),
        1,
        "{journal:#?}"
    );
    assert!(
        host.said("inform").is_empty(),
        "an escalation, not a record"
    );
    assert!(probe_rows(&ledger).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// WHAT BECOMES OF `/usage` LEFT TYPED, with nobody at the session. The
/// turn-end policy reads it as the draft it is and, past a person's grace,
/// sends it in its act's place — and that Enter is THE PROBE'S, not the
/// act's: fenced three ways as the probe's own, journaled under the probe's
/// rule, and booked nowhere in the policy (no `CONTINUED`, no `typed` row
/// under the act's rule, nothing of the hour's budget). The panel it opens
/// is owed since the text was written: the next look closes it with ONE
/// Escape under its own header row and samples nothing, the ledger gets its
/// `waiting` row, and the point — decided again on the empty composer — gets
/// its own act at once: `keep going` is typed within the two seconds of the
/// re-decision, which an act booked for `/usage` would have held back for
/// the half minute a typed act is given to be taken, and a step of the
/// back-off after it. Nothing is raised on the way, and the host is asked
/// for no second claim.
///
/// NEGATIVE CONTROL: a `/usage` standing in the composer that NO LEDGER ROW
/// OWES — a person's own draft — is sent as the draft rule sends any: as
/// the act (`CONTINUED … rule=continue@v1 /usage`, its `typed` row counted
/// by the budget's reader), and the panel it opens gets NO Escape: a loop
/// closes only a panel the harness opened.
#[test]
fn usage_left_typed_is_sent_as_the_probes_own_and_its_panel_is_closed() {
    let (dir, ledger) = ledger_at("probe-left-sent");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.tick_at_key = 1;
    // The re-decision after the panel's close is two seconds off, in 5 ms
    // waits (the one before it is waited out while the text stands, which
    // counts no wait).
    m.vanish_after = Some(700);
    let (lines, journal) = probe_run(
        "te-probe-left-sent",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        [
            "text:/usage",
            "enter",
            "enter",
            "esc",
            "text:keep going",
            "enter"
        ],
        "{lines:#?}\n{journal:#?}"
    );
    let keys: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| verb_of(r) == "key" && (r.contains("/usage") || r.ends_with(" esc")))
        .collect();
    assert_eq!(keys.len(), 3, "{keys:#?}");
    assert_eq!(
        keys[0], "key if-human=0 if-gen=1.104 if=^❯\\x20/usage\\s*$ enter",
        "the probe's Enter, which the moved screen skipped"
    );
    assert!(
        keys[1].starts_with("key if-human=0 if-gen=1.")
            && keys[1].ends_with(" if=^❯\\x20/usage\\s*$ enter")
            && keys[1] != keys[0],
        "the Enter of the text left typed: fenced three ways on a later read: {keys:#?}"
    );
    assert_eq!(
        keys[2],
        "key if-human=0 if=^...Settings..Status...Config...Usage esc"
    );
    assert_eq!(count(&m, "turn"), 0, "never the unfenced fallback");
    assert_eq!(
        probe_writes(&m),
        ["/usage", "esc"],
        "one `/usage`, one Escape"
    );
    assert_eq!(
        host.said("probe"),
        ["probe:claimed", "probed:not-written"],
        "no second claim, and nothing sampled"
    );
    assert_eq!(host.said("inform"), [USAGE_LEFT_TOLD]);
    let kinds: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert_eq!(
        kinds,
        ["EVENT", "CONTINUED", "EVENT", "SKIPPED", "EXIT"],
        "{lines:#?}"
    );
    assert!(
        lines[1].ends_with(" rule=continue@v1 keep going"),
        "{lines:#?}"
    );
    assert_eq!(gate_words(&lines), ["escalated"], "{lines:#?}");
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("/usage")),
        "`/usage` is no continuation: {journal:#?}"
    );
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.starts_with("TYPED "))
            .collect::<Vec<_>>(),
        [
            "TYPED seq=106 rule=usage-probe@v1 /usage left typed, its Enter written now: no act \
          of the policy's, and the panel it opens is closed unsampled"
        ],
        "{journal:#?}"
    );
    // Nothing was raised for the text or its panel: the run's one escalation
    // is the last turn's own request for a decision.
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention") && r.contains("usage")),
        "{:#?}",
        m.requests
    );
    let escalated = journal.iter().position(|l| l.starts_with("ESCALATED "));
    let continued = journal.iter().position(|l| l.starts_with("CONTINUED "));
    assert!(
        continued.is_some() && continued < escalated,
        "nothing escalated before the act: {journal:#?}"
    );
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    let body = std::fs::read_to_string(&ledger).expect("the ledger");
    assert_eq!(
        body.lines()
            .filter(|l| l.contains("\"command\":\"/usage\""))
            .count(),
        2,
        "no row of `/usage` under the act's rule: {body}"
    );
    assert_eq!(
        crate::supervise::approvals::typed_at(&ledger, None).len(),
        1,
        "the budget's reader counts the continuation alone"
    );
    assert!(!crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);

    // NEGATIVE CONTROL: the same text with nothing owed is a person's draft.
    let (dir, ledger) = ledger_at("probe-left-not-ours");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    let rest = m.screens.last().cloned().expect("the point");
    *m.screens.last_mut().expect("the point") = with_draft(&rest, "/usage");
    m.cursor_col = Some(8);
    world(&mut m).at = PanelAt::Typed("/usage".to_string());
    world(&mut m).rest = Some(rest);
    // The panel stands and nothing moves on: the run ends on its budget.
    let (lines, _) = probe_run_for(
        2,
        "te-probe-left-not-ours",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(m.writes(), ["enter"], "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.ends_with(" rule=continue@v1 /usage")),
        "the draft rule's act, as before: {lines:#?}"
    );
    assert_eq!(
        world(&mut m).at,
        PanelAt::Open,
        "not the harness's to close"
    );
    assert_eq!(probe_writes(&m), Vec::<&str>::new());
    assert!(host.said("probe").is_empty() && host.said("inform").is_empty());
    assert!(probe_rows(&ledger).is_empty());
    assert_eq!(
        crate::supervise::approvals::typed_at(&ledger, None).len(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN ENTER OF THE TEXT LEFT TYPED THAT DOES NOT TAKE (the server wrote it;
/// the composer holds `/usage` still, the screen as it was) IS NOT THE LAST
/// LOOK AT IT: the point is decided again once the wait a typed act is given
/// has passed, the Enter is written again there, and the run goes on as
/// [`usage_left_typed_is_sent_as_the_probes_own_and_its_panel_is_closed`]
/// — the panel closed, the act typed. Both Enters are journaled under the
/// probe's rule, and neither is an act.
#[test]
fn an_enter_of_the_text_left_typed_that_does_not_take_is_written_again() {
    let (dir, ledger) = ledger_at("probe-left-swallowed");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    // The probe's own Enter is skipped (the screen moved); the first Enter
    // of the text left typed is answered as written and the world does
    // nothing with it; the next one is the world's to take.
    m.key_replies
        .push_back(ok("OK skipped reason=changed seq=104\n"));
    m.key_replies.push_back(ok("OK seq=106\n"));
    m.vanish_after = Some(700);
    let (lines, journal) = probe_run(
        "te-probe-left-swallowed",
        &mut m,
        &host,
        &ledger,
        probe_policy(0),
        |s| {
            s.set_turn_end_timing(TurnEndTiming {
                take_within: Duration::from_millis(100),
                ..TurnEndTiming::default()
            });
        },
    );
    assert_eq!(
        m.writes(),
        [
            "text:/usage",
            "enter",
            "enter",
            "enter",
            "esc",
            "text:keep going",
            "enter"
        ],
        "{lines:#?}\n{journal:#?}"
    );
    assert_eq!(
        journal
            .iter()
            .filter(
                |l| l.starts_with("TYPED ") && l.contains(" rule=usage-probe@v1 /usage left typed")
            )
            .count(),
        2,
        "{journal:#?}"
    );
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains("/usage")),
        "{journal:#?}"
    );
    assert_eq!(probe_writes(&m), ["/usage", "esc"]);
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(
        crate::supervise::approvals::typed_at(&ledger, None).len(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN ENTER OF THE TEXT LEFT TYPED THAT THE SERVER TURNS AWAY IS WRITTEN AGAIN
/// once it can be ([`Session::probe_left_enter`]'s refusal arms, the review
/// of 2026-10-14): `ERR halted` (an owner's hold, parked on until `status`
/// reads `hold=0`) or `ERR busy lease=…` (another driver's, backed off from)
/// wrote nothing, and the screen — `/usage` standing in the composer — never
/// moves, so no look would come: the point is decided again a moment later
/// and the Enter written there, the run going on as
/// [`usage_left_typed_is_sent_as_the_probes_own_and_its_panel_is_closed`]
/// (the panel closed, the act typed). Parked or backed off and nothing
/// more, the text stood unsent until something else moved the screen, and
/// the act behind it with it. The run ends on the fact, its stalls a hang
/// detector.
#[test]
fn an_enter_of_the_text_left_typed_that_the_server_turns_away_is_written_again() {
    for refusal in ["halted reason=owner origin=local", "busy lease=s-9"] {
        let tag = if refusal.starts_with("halted") {
            "probe-left-halted"
        } else {
            "probe-left-busy"
        };
        let (dir, ledger) = ledger_at(tag);
        let host = ProbeHost::probing(true);
        let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
        // The probe's own Enter is skipped (the screen moved); the first
        // Enter of the text left typed is turned away; the next one is the
        // world's to take.
        m.key_replies
            .push_back(ok("OK skipped reason=changed seq=104\n"));
        m.key_replies.push_back(err(refusal));
        if refusal.starts_with("halted") {
            m.hold = "1";
            m.hold_lifts_after = Some(1);
        }
        // The re-decisions are two seconds off, in 5 ms waits.
        m.vanish_after = Some(700);
        let (lines, journal) = probe_run(
            &format!("te-{tag}"),
            &mut m,
            &host,
            &ledger,
            probe_policy(0),
            |s| {
                s.set_turn_end_timing(TurnEndTiming {
                    take_within: Duration::from_millis(100),
                    ..TurnEndTiming::default()
                });
            },
        );
        assert_eq!(
            m.writes(),
            [
                "text:/usage",
                "enter",
                "enter",
                "enter",
                "esc",
                "text:keep going",
                "enter"
            ],
            "{refusal}: {lines:#?}\n{journal:#?}"
        );
        assert_eq!(probe_writes(&m), ["/usage", "esc"], "{refusal}");
        assert_eq!(
            journal
                .iter()
                .filter(|l| l.starts_with("TYPED ")
                    && l.contains(" rule=usage-probe@v1 /usage left typed"))
                .count(),
            1,
            "{refusal}: {journal:#?}"
        );
        assert_eq!(
            probe_rows(&ledger),
            ["typed /usage the accounts probe", "waiting /usage closed"],
            "{refusal}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `rows` with `text` typed into its composer (the caret row).
fn with_draft(rows: &[String], text: &str) -> Vec<String> {
    let mut rows = rows.to_vec();
    let caret = rows.iter().rposition(|r| r == "❯").expect("a composer");
    rows[caret] = format!("❯ {text}");
    rows
}

/// THE PROBE IS NEVER TYPED A SECOND TIME OVER ITS OWN TEXT (no
/// `/usage/usage`). `/usage` stands typed and unsent, and another turn runs
/// and ends over it — read live, under a roster that says the read is due
/// and a host that would grant every claim: the loop that takes the session
/// up asks for NO claim and writes nothing. Three things shut it, any one
/// enough: a panel is owed (the first of the probe's gates), the composer is
/// not empty (the policy's own gate — and with a draft standing the host is
/// not even asked about the account), and the write's caret-row guard
/// (`^❯\s*$`) matches no row of a composer that holds text.
///
/// NEGATIVE CONTROL: the same second turn end with the text cleared first is
/// probed — the host asked, the claim granted, `/usage` typed, the panel
/// sampled and closed.
///
/// THE LOOP'S OWN GATE, ALONE: at a point where every other gate is open —
/// an EMPTY composer, the advice the probe's, the turn read live — a panel
/// still owed gets no claim asked and nothing written; with nothing owed
/// the same call claims and probes.
#[test]
fn the_probe_is_not_typed_again_over_its_own_text_left_typed() {
    let second_turn_end = |tag: &str, cleared: bool| {
        let (dir, ledger) = ledger_at(tag);
        let host = ProbeHost::probing(true);
        let mut m = probe_world(
            &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
            &host,
        );
        m.tick_at_key = 1;
        let _ = probe_run_for(
            1,
            &format!("te-{tag}"),
            &mut m,
            &host,
            &ledger,
            probe_policy(120),
            |_| {},
        );
        assert_eq!(probe_writes(&m), ["/usage"], "{tag}");
        assert_eq!(world(&mut m).at, PanelAt::Typed("/usage".to_string()));
        let rest = world(&mut m).rest.clone().expect("the composer");
        if cleared {
            // A person took the text out.
            world(&mut m).at = PanelAt::Rest;
            m.cursor_col = None;
            *m.screens.last_mut().expect("a screen") = rest;
        }
        // A turn runs and ends over the composer as it stands.
        let standing = m.screens.last().cloned().expect("a screen");
        let busy = if cleared {
            busy_screen()
        } else {
            with_draft(&busy_screen(), "/usage")
        };
        m.screens.push(busy);
        m.screens.push(standing);
        m.served = m.screens.len() - 2;
        m.stalls = 0;
        let next = ProbeHost::probing(true);
        // With the text standing nothing moves on: that run ends on its
        // budget.
        let (lines, _) = probe_run_for(
            if cleared { 60 } else { 1 },
            &format!("te-{tag}-next"),
            &mut m,
            &next,
            &ledger,
            probe_policy(120),
            |_| {},
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            next.said("").first().map(String::as_str),
            Some("turn-ran"),
            "{tag}: the turn was read live: {lines:#?}"
        );
        (probe_writes(&m), next.said(""), m.writes())
    };
    let (probes, said, writes) = second_turn_end("probe-left-again", false);
    assert_eq!(probes, ["/usage"], "the first loop's alone");
    assert_eq!(said, ["turn-ran"], "asked for no advice and no claim");
    assert_eq!(writes, ["text:/usage", "enter"], "nothing written over it");

    let (probes, said, _) = second_turn_end("probe-left-cleared", true);
    assert_eq!(probes, ["/usage", "/usage", "esc"]);
    assert_eq!(
        said[..4],
        ["turn-ran", "account:turn-end", "probe:claimed", "probed:ok"],
        "{said:#?}"
    );

    // The loop's own gate, alone.
    let (dir, ledger) = ledger_at("probe-left-gate");
    let host = ProbeHost::probing(true);
    let mut m = probe_mock(vec![ended(PROBE_SAID)], &[panel_fx::USAGE_SETTLED], &host);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut s = session(&mut m, None);
    s.set_clock(PROBE_NOW, PDT);
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.clone()));
    s.ledger = crate::supervise::approvals::Ledger::open(Some(&ledger), None, &mut Vec::new());
    probe_waits(&mut s);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let decided = s
        .turn_end_at_point(&point, Some(Duration::from_secs(182)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.turn_live = true;
    s.probe = UsageProbe::Owed {
        own: true,
        since: Instant::now(),
    };
    assert_eq!(
        s.usage_probe(&point, &decided, &opts, &mut Alone),
        Ok(Probed::No)
    );
    assert!(host.said("probe").is_empty(), "no claim while one is owed");
    s.probe = UsageProbe::Idle;
    assert_eq!(
        s.usage_probe(&point, &decided, &opts, &mut Alone),
        Ok(Probed::Done)
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:ok"]);
    drop(s);
    assert_eq!(probe_writes(&m), ["/usage", "esc"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// WHAT CAME UP IS NOT THE USAGE TAB UNDER THE FULLSCREEN RENDERER'S RULE —
/// the inline renderer's panel (its `─` rule, two columns in), or another
/// tab of the Settings panel: no sample, the host is told `renderer`, the
/// panel gets its one Escape under its own header row, and THIS LOOP PROBES
/// NO MORE — not even on another grid size (so it is not the fold's latch).
/// And a panel that LEAVES BY ITSELF while it is read is `closed`: no
/// sample, no Escape, the point's act as decided.
#[test]
fn another_panel_ends_the_loops_probes_and_one_that_leaves_gets_no_escape() {
    let (wrote, told) = two_turn_ends("probe-inline-panel", &[panel_fx::USAGE_INLINE], 3);
    assert_eq!(wrote, ["/usage", "esc"]);
    assert_eq!(told, ["probe:claimed", "probed:renderer"]);

    // Another tab: the measured panel's header over rows the Usage tab does
    // not draw (COMPOSED: no other tab was ever captured — the Status tab
    // names the login).
    let settled = panel_fx::screen(panel_fx::USAGE_SETTLED);
    let header = aterm_phase::settings_panel(&settled)
        .expect("the panel")
        .header;
    let mut other: Vec<String> = settled[..=header].to_vec();
    other.extend(
        ["", "   Version: 2.1.287", "", "   Esc to cancel"]
            .iter()
            .map(|r| r.to_string()),
    );
    let other = other.join("\n");
    let (wrote, told) = two_turn_ends("probe-other-tab", &[other.as_str()], 3);
    assert_eq!(wrote, ["/usage", "esc"]);
    assert_eq!(told, ["probe:claimed", "probed:renderer"]);

    // It leaves by itself after its first read.
    let (dir, ledger) = ledger_at("probe-left");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_REFRESHING], &host);
    world(&mut m).leaves_after = Some(1);
    let (lines, _) = probe_run(
        "te-probe-left",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        |_| {},
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "text:keep going", "enter"],
        "{lines:#?}"
    );
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:closed"]);
    assert_eq!(
        probe_rows(&ledger),
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
    assert_eq!(usage_probe_violations(&m, &host), Vec::<String>::new());
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE LOOP IS STOPPED MID-PROBE (its host's stop lands while the panel is
/// read): a stopped loop writes nothing — NO ESCAPE goes — the host is told
/// `lost`, and the ledger keeps the `typed` row with no `waiting` row after
/// it. The loop that follows reads that row and closes the panel.
#[test]
fn a_loop_stopped_mid_probe_presses_nothing_and_the_next_loop_closes_the_panel() {
    let (dir, ledger) = ledger_at("probe-stopped");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_REFRESHING], &host);
    let stop = Arc::new(AtomicBool::new(false));
    world(&mut m).stop_at_read = Some((2, Arc::clone(&stop)));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ledger: Some(ledger.clone()),
        policy: probe_policy(120),
        ..SuperviseOpts::hosted()
    };
    let mut out: Vec<u8> = Vec::new();
    {
        let mut s = session(&mut m, None);
        s.set_clock(PROBE_NOW, PDT);
        s.set_zone_lookup(test_zones);
        probe_waits(&mut s);
        assert_eq!(s.run_hosted(&opts, stop, &mut out), Ok(()));
    }
    assert_eq!(probe_writes(&m), ["/usage"], "{:#?}", m.requests);
    assert_eq!(world(&mut m).at, PanelAt::Open, "the panel stands");
    assert_eq!(host.said("probe"), ["probe:claimed", "probed:lost"]);
    assert_eq!(probe_rows(&ledger), ["typed /usage the accounts probe"]);
    assert!(crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));

    // The next loop.
    m.stalls = 0;
    let next = ProbeHost::probing(true);
    let (lines, _) = probe_run(
        "te-probe-stopped-next",
        &mut m,
        &next,
        &ledger,
        probe_policy(0),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert!(next.said("probe").is_empty());
    assert!(!crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Claude Code's own retry of a usage limit under a hand-set
/// `CLAUDE_CODE_RETRY_WATCHDOG` (the lab's capture `fixture`), the prompt it
/// retries the supervisor's own `keep going`, its ticking span `tick`, and no
/// `esc to interrupt` in the footer (as with a statusLine configured: the
/// retry row is then the screen's only word that the turn is not over).
fn watchdog_after_keep_going(fixture: &str, tick: &str) -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(fixture)
        .into_iter()
        .map(|r| {
            let r = r
                .replace("❯ carry on with the task", "❯ keep going")
                .replace("❯ toolstart", "❯ keep going")
                .replace(" · esc to interrupt", "");
            match r.split_once(" · Retrying in ") {
                Some((head, rest)) => match rest.split_once(" (") {
                    Some((_, clock)) => format!("{head} · Retrying in {tick} ({clock}"),
                    None => r,
                },
                None => r,
            }
        })
        .collect()
}

/// `rows` as the reader before accounts v2 S5 took the retry row: a status
/// row it could not count the attempt of, a done row.
fn retry_read_as_done(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|r| r.replace("attempt 1/300", "attempt one of 300"))
        .collect()
}

/// A loop over `screens` (the first continuation's turn, then whatever
/// follows), its acts' window a millisecond and no back-off, `grace` the
/// person's ([`SupervisorConfig::human_grace_s`]): the Mock and the lines.
fn watchdog_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    grace: u32,
) -> (Mock, Vec<String>, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let mut m = Mock::new(true, screens);
    m.turn_releases = Some(1);
    m.vanish_after = Some(8);
    m.stall_sleep = Some(Duration::from_millis(20));
    let mut opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    opts.policy.human_grace_s = grace;
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_millis(1),
            short_backoff: Duration::ZERO,
            min_work: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    // What each wait was for, its `until=` (a clock reading) left out.
    let (records, _) = journal_records(&path);
    let waits = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| {
            r.summary
                .split_once(' ')
                .map_or(r.summary.clone(), |(_, why)| why.to_string())
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (m, lines, waits)
}

/// The acts a run typed (`CONTINUED`, `TYPED`), as `rule text`.
fn typed_acts(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED") || l.starts_with("TYPED"))
        .map(|l| l.split(" rule=").nth(1).unwrap_or_default().to_string())
        .collect()
}

/// CLAUDE CODE'S OWN RETRY OF A USAGE LIMIT IS A TURN IN PROGRESS, AND NOTHING
/// IS TYPED INTO IT (accounts v2 S5). The supervisor's continuation meets
/// the five-hour limit under a hand-set watchdog: no notice and no
/// transcript row, one status row ticking every second (`✻ Session limit
/// reached · Retrying in … (8:48am) · attempt 1/300`) until the vendor sends
/// the request again by itself — measured on 2.1.300 inline, on 2.1.301 on
/// the fullscreen renderer and met by a turn's second request. Every read of
/// it is busy: no point, no limit, no wait, and nothing typed after the one
/// continuation however long it ticks. NEGATIVE CONTROL: the same run read
/// as the reader before read it (the row a done row) is an idle point, and
/// the continue policy types `keep going` into the session that is retrying.
#[test]
fn the_watchdogs_retry_row_is_a_turn_in_progress_and_nothing_is_typed_into_it() {
    use aterm_phase::prompt::fixtures::{
        WALL_WATCHDOG_FULLSCREEN_2_1_301, WALL_WATCHDOG_MIDTURN_2_1_301,
        WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300,
    };
    for (name, fixture) in [
        ("inline", WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300),
        ("fullscreen", WALL_WATCHDOG_FULLSCREEN_2_1_301),
        ("mid-turn", WALL_WATCHDOG_MIDTURN_2_1_301),
    ] {
        // The row ticks every second: each read a new screen.
        let ticking: Vec<Vec<String>> = (40..46)
            .map(|s| watchdog_after_keep_going(fixture, &format!("59m {s}s")))
            .collect();
        let run = |tag: &str, retry: Vec<Vec<String>>| {
            let mut screens = vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
            ];
            screens.extend(retry);
            watchdog_run(tag, screens, 120)
        };
        let (m, lines, waits) = run("te-watchdog-busy", ticking.clone());
        assert_eq!(
            typed_acts(&lines),
            ["continue@v1 keep going"],
            "{name}: the first continuation only: {lines:#?}"
        );
        assert_eq!(m.turns, 1, "{name}: {lines:#?}\n{:#?}", m.requests);
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("EVENT limited") || l.starts_with("LIMITED")),
            "{name}: no limit: {lines:#?}"
        );
        assert!(
            waits
                .iter()
                .all(|w| w.contains("to take the continue@v1 act")),
            "{name}: no wait but the act's own window: {waits:#?}"
        );
        // NEGATIVE CONTROL: the row read as a done row, as before.
        let old: Vec<Vec<String>> = ticking.iter().map(|r| retry_read_as_done(r)).collect();
        let (m, lines, _) = run("te-watchdog-done-row", old);
        assert!(m.turns >= 2, "{name}: typed into the retry: {lines:#?}");
    }
}

/// ESC ON THE VENDOR'S RETRY IS A PERSON ENDING THE TURN, AS ANYWHERE (accounts
/// v2 S5). Measured on 2.1.301: Esc on the row of a limit met mid-turn leaves
/// `⎿  Interrupted · What should Claude do instead?` under the turn's Bash
/// call; Esc on it at a turn's first request (the fullscreen renderer) puts
/// the prompt back in the composer, as Esc before any answer does. Each run
/// is decided exactly as the same Esc on an ordinary turn — the retry row's
/// place a spinner's — is decided: the person's grace first (`a person
/// stopped the turn with Esc`; the prompt put back is their draft, `a person
/// is typing`), then, the grace spent, the same acts; no limit, no wait on a
/// reset. And the retry served at the reset is the turn
/// going on: its done row is an ordinary turn end, decided as the same turn
/// served with no retry before it. NEGATIVE CONTROL: the run read as the
/// reader before read the row (a done row) is not the ordinary turn's: the
/// continue policy typed into the retry before the Esc.
#[test]
fn esc_on_the_vendors_retry_is_an_ordinary_turn_a_person_ended() {
    use aterm_phase::prompt::fixtures::{
        WALL_WATCHDOG_FULLSCREEN_2_1_301, WALL_WATCHDOG_FULLSCREEN_ESC_2_1_301,
        WALL_WATCHDOG_MIDTURN_2_1_301, WALL_WATCHDOG_MIDTURN_ESC_2_1_301,
        WALL_WATCHDOG_MIDTURN_SERVED_2_1_301,
    };
    // The retry row's place a spinner's: the ordinary turn.
    let spinning = |rows: Vec<String>| -> Vec<String> {
        rows.into_iter()
            .map(
                |r| match r.split_once(" Session limit reached · Retrying in ") {
                    Some((glyph, _)) => format!("{glyph} Cooking… (2m 4s · ↓ 1.1k tokens)"),
                    None => r,
                },
            )
            .collect()
    };
    for (name, standing, after, held) in [
        (
            "mid-turn Esc",
            WALL_WATCHDOG_MIDTURN_2_1_301,
            WALL_WATCHDOG_MIDTURN_ESC_2_1_301,
            Some("a person stopped the turn with Esc"),
        ),
        (
            "first-request Esc",
            WALL_WATCHDOG_FULLSCREEN_2_1_301,
            WALL_WATCHDOG_FULLSCREEN_ESC_2_1_301,
            Some("a person is typing"),
        ),
        (
            "served",
            WALL_WATCHDOG_MIDTURN_2_1_301,
            WALL_WATCHDOG_MIDTURN_SERVED_2_1_301,
            None,
        ),
    ] {
        let screens = |turn: &dyn Fn(Vec<String>) -> Vec<String>| -> Vec<Vec<String>> {
            let mut screens = vec![
                busy_screen(),
                ended("Fixed the parser; the suite is green."),
            ];
            screens.extend(
                (40..46).map(|s| turn(watchdog_after_keep_going(standing, &format!("59m {s}s")))),
            );
            screens.push(watchdog_after_keep_going(after, ""));
            screens
        };
        for grace in [120, 0] {
            let (_, retry, retry_waits) = watchdog_run("te-watchdog-esc", screens(&|r| r), grace);
            let (_, plain, plain_waits) =
                watchdog_run("te-watchdog-esc-plain", screens(&spinning), grace);
            assert_eq!(
                (typed_acts(&retry), &retry_waits),
                (typed_acts(&plain), &plain_waits),
                "{name}, grace {grace}: as the ordinary turn: {retry:#?}\n{plain:#?}"
            );
            assert!(
                !retry
                    .iter()
                    .any(|l| l.starts_with("EVENT limited") || l.starts_with("LIMITED")),
                "{name}: no limit: {retry:#?}"
            );
            assert!(
                retry_waits
                    .iter()
                    .all(|w| !w.contains("resets") && !w.contains("by itself")),
                "{name}: no wait on a reset: {retry_waits:#?}"
            );
            if let Some(held) = held.filter(|_| grace > 0) {
                assert!(
                    retry_waits.iter().any(|w| w.ends_with(held)),
                    "{name}: the person's grace: {retry_waits:#?}\n{retry:#?}"
                );
            }
        }
        // NEGATIVE CONTROL: the row read as a done row.
        let old = screens(&|r| retry_read_as_done(&r));
        let (_, old, _) = watchdog_run("te-watchdog-esc-old", old, 0);
        let (_, plain, _) = watchdog_run("te-watchdog-esc-plain", screens(&spinning), 0);
        assert_ne!(typed_acts(&old), typed_acts(&plain), "{name}: {old:#?}");
    }
}

/// A host that keeps what the loop tells it of the account's usage.
#[derive(Debug, Default)]
struct UsageHost {
    told: std::sync::Mutex<Vec<UsageSeen>>,
}

impl IdleHost for UsageHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn usage_seen(&self, seen: &UsageSeen) {
        self.told.lock().unwrap().push(seen.clone());
    }
}

/// WHAT THE SCREEN SAYS OF THE ACCOUNT'S USAGE IS TOLD TO THE HOST, ONCE PER
/// CHANGE (accounts v2 S5): busy reads included — credits start mid-turn —
/// and the turn ends as an ordinary idle point, no wall and no limit episode:
/// credits are work going on. The measured credits screens (2.1.300) are told as credits
/// in use, the started notice with its window, then the lasting one; the same
/// screen read again tells nothing new. NEGATIVE CONTROL: a run with no
/// notice tells the host only that nothing is said, once.
#[test]
fn what_the_screen_says_of_usage_is_told_to_the_host_once_per_change() {
    use aterm_phase::prompt::fixtures::{CREDITS_IN_USE_2_1_300, CREDITS_STARTED_2_1_300};
    let run = |screens: Vec<Vec<String>>| {
        let host = Arc::new(UsageHost::default());
        let mut m = Mock::new(true, screens);
        m.vanish_after = Some(3);
        m.stall_sleep = Some(Duration::from_millis(5));
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            ..hosted(30)
        };
        let (lines, _) = watch_lines(&mut m, &opts);
        let told = host.told.lock().unwrap().clone();
        (lines, told)
    };
    let busy_on_credits = {
        let mut r = busy_screen();
        let at = r.len() - 1;
        // Right-aligned against the 120-column rules, two short.
        r[at] = format!("{:<95}Now using usage credits", r[at]);
        r
    };
    let started = aterm_phase::prompt::fixtures::screen(CREDITS_STARTED_2_1_300);
    let lasting = aterm_phase::prompt::fixtures::screen(CREDITS_IN_USE_2_1_300);
    let (lines, told) = run(vec![
        busy_screen(),
        busy_on_credits,
        started.clone(),
        lasting.clone(),
        lasting,
    ]);
    let credits: Vec<Option<bool>> = told
        .iter()
        .map(|t| t.credits.as_ref().map(|c| c.started.is_some()))
        .collect();
    assert_eq!(
        credits,
        [None, Some(false), Some(true), Some(false)],
        "{told:#?}\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("EVENT idle"))
            && !lines
                .iter()
                .any(|l| l.starts_with("EVENT limited") || l.starts_with("LIMITED")),
        "an ordinary turn end, no wall: {lines:#?}"
    );
    // NEGATIVE CONTROL: nothing said.
    let (_, told) = run(vec![
        busy_screen(),
        ended("Fixed the parser; the suite is green."),
    ]);
    assert_eq!(told, [UsageSeen::default()], "{told:#?}");
}

/// AN OFF SESSION IS LEFT WITH NO `/usage` PANEL OF THE HARNESS'S: the loop
/// that takes up the panel a stopped loop left owed, on a session whose
/// Autopilot word is `off`, lets it stand the person's grace in its own
/// sight (a second here, as the default's 120 s would: the grace starts as
/// the panel is taken up, so the first look is always inside it), closes it
/// — its one Escape, past that grace — BEFORE it parks, and while parked
/// writes nothing more: no continuation, no probe, no second Escape. (What
/// a park does leave standing — a limit reset's step, a Codex switch — is
/// the design's §7.) The run ends a few park steps in, so a park reached
/// with the panel still open ends it too. NEGATIVE CONTROL: before the
/// close, the panel stood.
#[test]
fn an_off_session_has_the_harnesss_usage_panel_closed_before_the_park() {
    let (dir, ledger) = ledger_at("probe-off");
    let mut m = panel_left_owed(&ledger);
    // NEGATIVE CONTROL: the panel the stopped loop left stands.
    assert_eq!(world(&mut m).at, PanelAt::Open);
    let before = m.writes();

    // The next loop, the session's word `off`, a grace of a second.
    m.autopilot = Some("off");
    m.exit_when = Some(|m| m.metas >= 3);
    let next = ProbeHost::probing(true);
    let started = Instant::now();
    let (lines, journal) = probe_run(
        "te-probe-off-next",
        &mut m,
        &next,
        &ledger,
        probe_policy(1),
        |_| {},
    );
    assert_eq!(probe_writes(&m), ["/usage", "esc"], "{lines:#?}");
    assert_eq!(world(&mut m).at, PanelAt::Rest, "{lines:#?}");
    let esc = world(&mut m).probes.last().expect("the Escape").at;
    assert!(
        esc.duration_since(started) >= Duration::from_secs(1),
        "one grace in the loop's own sight: {:?}",
        esc.duration_since(started)
    );
    assert!(
        journal.iter().any(|l| l.starts_with("PARKED seq=")),
        "{journal:#?}"
    );
    // The Escape went before the park's first step (its wait for the
    // screen to move, `OFF_STEP` long).
    let escaped = m
        .requests
        .iter()
        .rposition(|r| verb_of(r) == "key" && r.ends_with(" esc"))
        .unwrap_or_else(|| panic!("the Escape: {:#?}", m.requests));
    let step = format!("timeout {}", OFF_STEP.as_millis());
    let parked = m
        .requests
        .iter()
        .position(|r| r.starts_with("await seq ") && r.ends_with(&step))
        .unwrap_or_else(|| panic!("the park's step: {:#?}", m.requests));
    assert!(escaped < parked, "{:#?}", m.requests);
    assert_eq!(
        m.writes()[before.len()..],
        ["esc"],
        "the Escape alone: {:#?}",
        m.requests
    );
    assert!(next.said("probe").is_empty(), "no probe while parked");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The world an Off test of the panel starts from: a loop stopped mid-probe
/// (its host's stop lands while the panel is read) has left its `/usage`
/// panel standing, owed its close in the ledger at `ledger`, which the next
/// loop takes up. The counts the next loop's tests read start over: the
/// waits that end a run, the `meta` and `status` reads.
fn panel_left_owed(ledger: &Path) -> Mock {
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_REFRESHING], &host);
    let stop = Arc::new(AtomicBool::new(false));
    world(&mut m).stop_at_read = Some((2, Arc::clone(&stop)));
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        ledger: Some(ledger.to_path_buf()),
        policy: probe_policy(120),
        ..SuperviseOpts::hosted()
    };
    let mut out: Vec<u8> = Vec::new();
    {
        let mut s = session(&mut m, None);
        s.set_clock(PROBE_NOW, PDT);
        s.set_zone_lookup(test_zones);
        probe_waits(&mut s);
        assert_eq!(s.run_hosted(&opts, stop, &mut out), Ok(()));
    }
    assert_eq!(world(&mut m).at, PanelAt::Open, "the panel stands");
    m.stalls = 0;
    m.metas = 0;
    m.status_reads = 0;
    m
}

/// What the next loop over [`panel_left_owed`] did ([`off_panel_run`]).
struct OffPanelRun {
    m: Mock,
    /// The next loop's requests.
    requests: Vec<String>,
    /// The next loop's writes of the probe's ([`probe_writes`]).
    writes: Vec<&'static str>,
    journal: Vec<String>,
    /// The ledger still owes the panel's close.
    owed: bool,
}

/// The next loop over [`panel_left_owed`], under the session's word `word`,
/// with a stall suspected or not (`suspect`: as after a press of the loop's
/// refused `ERR busy input-unread`), the worker's input as `input` (`status
/// input=…`), and the panel's Escapes as `esc` makes them ([`esc_takes`],
/// [`esc_refused_unread`], …) — the manager's mail, the host's relaunch and
/// the restart's bound all in place, so a stall held at the panel mails a
/// `kind=ask`, journals `FROZEN` and restarts the worker. The run ends at
/// the first wait after `exit` holds of the mock (else on the script).
fn off_panel_run(
    tag: &str,
    word: &'static str,
    suspect: bool,
    input: &'static str,
    esc: fn(&mut PanelWorld),
    exit: fn(&Mock) -> bool,
) -> OffPanelRun {
    let (dir, ledger) = ledger_at(tag);
    let mut m = panel_left_owed(&ledger);
    let (requests_before, writes_before) = (m.requests.len(), probe_writes(&m).len());
    m.autopilot = Some(word);
    m.input = input;
    m.exit_when = Some(exit);
    esc(world(&mut m));
    let next = ProbeHost::probing(true);
    next.relaunches.store(true, Ordering::SeqCst);
    let policy = SupervisorConfig {
        stall_term: true,
        stall_term_after_s: 60,
        ..probe_policy(0)
    };
    let (_, journal) = probe_run(tag, &mut m, &next, &ledger, policy, |s| {
        s.set_manager(Some("@s-9".to_string()));
        s.stall_suspect = suspect;
    });
    let owed = crate::supervise::approvals::probe_owed(
        &std::fs::read_to_string(&ledger).unwrap_or_default(),
        None,
    );
    let _ = std::fs::remove_dir_all(&dir);
    let requests = m.requests[requests_before..].to_vec();
    let writes = probe_writes(&m)[writes_before..].to_vec();
    OffPanelRun {
        m,
        requests,
        writes,
        journal,
        owed,
    }
}

/// An [`off_panel_run`] whose panel's Escape takes, as the world's does.
fn esc_takes(_: &mut PanelWorld) {}

/// An [`off_panel_run`] whose panel's first Escape is refused `ERR busy
/// input-unread` (which suspects a stall).
fn esc_refused_unread(w: &mut PanelWorld) {
    w.esc_replies = [err(
        "busy input-unread bytes=1 wait_ms=1500 input=pending (the program has not read input \
         queued 1s ago; retry in a moment)",
    )]
    .into();
}

/// An [`off_panel_run`] whose panel's first Escape is refused by a hold
/// (`ERR halted`).
fn esc_halted(w: &mut PanelWorld) {
    w.esc_replies = [err("halted by the owner")].into();
}

/// An [`off_panel_run`] whose panel's first Escape is written and does not
/// take: the panel stands.
fn esc_fails_once(w: &mut PanelWorld) {
    w.esc_fails = 1;
}

/// Whether `requests` hold one whose verb is `verb`.
fn sent(requests: &[String], verb: &str) -> bool {
    requests.iter().any(|r| verb_of(r) == verb)
}

/// AN OFF SESSION'S PANEL GETS ONE PLAIN CLOSE, NEVER A STALL'S HOLD (the
/// review of c1d193998). The panel a stopped loop left owed, on a session
/// whose word is `off`, with a stall suspected and the worker published
/// stalled, is not looked into: the loop parks at once — no `kind=ask`
/// posted, no `FROZEN`, no restart, no Escape — and the panel stays owed for
/// the word's turn. NEGATIVE CONTROL: the same world under Cruise holds the
/// stall at the panel — `FROZEN`, the manager's ask, the restart.
#[test]
fn an_off_session_with_a_stall_suspected_parks_and_leaves_the_panel_owed() {
    let off = off_panel_run(
        "probe-off-stall",
        "off",
        true,
        INPUT_STALLED,
        esc_takes,
        |m| m.metas >= 3,
    );
    assert!(
        off.journal.iter().any(|l| l.starts_with("PARKED seq=")),
        "{:#?}",
        off.journal
    );
    assert!(
        !off.journal.iter().any(|l| l.starts_with("FROZEN")),
        "{:#?}",
        off.journal
    );
    assert!(!sent(&off.requests, "post"), "{:#?}", off.requests);
    assert!(!sent(&off.requests, "restart"), "{:#?}", off.requests);
    assert!(off.writes.is_empty(), "no Escape: {:#?}", off.requests);
    let mut m = off.m;
    assert_eq!(world(&mut m).at, PanelAt::Open, "the panel stands");
    assert!(off.owed, "owed until the word turns");

    // NEGATIVE CONTROL: Cruise.
    let cruise = off_panel_run(
        "probe-cruise-stall",
        "cruise",
        true,
        INPUT_STALLED,
        esc_takes,
        |m| m.requests.iter().any(|r| verb_of(r) == "restart"),
    );
    assert!(
        cruise.journal.iter().any(|l| l.starts_with("FROZEN seq=")),
        "{:#?}",
        cruise.journal
    );
    assert!(
        cruise
            .requests
            .iter()
            .any(|r| r.starts_with("post to=@s-9 kind=ask ")),
        "{:#?}",
        cruise.requests
    );
    assert!(sent(&cruise.requests, "restart"), "{:#?}", cruise.requests);
    assert!(
        !cruise.journal.iter().any(|l| l.starts_with("PARKED")),
        "{:#?}",
        cruise.journal
    );
}

/// UNDER OFF A REFUSED ESCAPE IS NOT TRIED AGAIN: the panel's one guarded
/// Escape, refused `ERR busy input-unread` (which suspects a stall), and the
/// loop parks — no second Escape, no stall's hold, nothing posted, the panel
/// owed. NEGATIVE CONTROL: under Cruise the refusal is backed off and the
/// Escape tried again, which closes the panel.
#[test]
fn an_off_sessions_refused_escape_is_its_one_close() {
    let off = off_panel_run(
        "probe-off-refused",
        "off",
        false,
        INPUT_CLEAR,
        esc_refused_unread,
        |m| m.metas >= 3,
    );
    assert_eq!(off.writes, ["esc"], "one Escape: {:#?}", off.requests);
    assert!(
        off.journal
            .iter()
            .any(|l| l.starts_with("REFUSED") && l.contains("ERR busy input-unread")),
        "{:#?}",
        off.journal
    );
    assert!(
        off.journal.iter().any(|l| l.starts_with("PARKED seq=")),
        "{:#?}",
        off.journal
    );
    assert!(
        !off.journal.iter().any(|l| l.starts_with("FROZEN")),
        "{:#?}",
        off.journal
    );
    assert!(!sent(&off.requests, "post"), "{:#?}", off.requests);
    let mut m = off.m;
    assert_eq!(world(&mut m).at, PanelAt::Open, "the panel stands");
    assert!(off.owed, "owed until the word turns");

    // NEGATIVE CONTROL: Cruise backs off and tries again, and the panel
    // closes.
    let cruise = off_panel_run(
        "probe-cruise-refused",
        "cruise",
        false,
        INPUT_CLEAR,
        esc_refused_unread,
        |_| false,
    );
    assert_eq!(cruise.writes, ["esc", "esc"], "{:#?}", cruise.requests);
    let mut m = cruise.m;
    assert_ne!(world(&mut m).at, PanelAt::Open, "closed");
    assert!(!cruise.owed);
}

/// UNDER OFF THE ONE CLOSE PROMISES NOTHING MORE AND WAITS ON NO HOLD (the
/// review of b512d03d8). An Escape that does not take is journaled
/// `WAITING … owed until the word turns` — never `Escape again in 5 s`, as
/// no Escape follows under Off — and the loop parks. An Escape a hold
/// refuses (`ERR halted`) is journaled `HALTED … owed until the word turns`
/// and the loop parks, never waiting the hold out (`park_on_hold`: an
/// unbounded wait, with the loop's badges and claim up; no `UNHALTED`).
/// Each leaves one Escape and the panel owed. NEGATIVE CONTROLS: under
/// Cruise the failed Escape's line names its next try, and the hold is
/// waited out (`UNHALTED`) and the Escape sent again.
#[test]
fn an_off_sessions_one_close_promises_no_second_escape_and_waits_on_no_hold() {
    let parked = |run: &OffPanelRun| run.journal.iter().any(|l| l.starts_with("PARKED seq="));
    let off = off_panel_run(
        "probe-off-failed",
        "off",
        false,
        INPUT_CLEAR,
        esc_fails_once,
        |m| m.metas >= 3,
    );
    assert_eq!(off.writes, ["esc"], "one Escape: {:#?}", off.requests);
    let waiting: Vec<&String> = off
        .journal
        .iter()
        .filter(|l| l.starts_with("WAITING seq="))
        .collect();
    assert_eq!(waiting.len(), 1, "{:#?}", off.journal);
    assert!(
        waiting[0].ends_with(
            "panel is open (1 tries): owed until the word turns (autopilot off: no Escape again)"
        ),
        "{}",
        waiting[0]
    );
    assert!(parked(&off), "{:#?}", off.journal);
    assert!(off.owed, "owed until the word turns");

    let off = off_panel_run(
        "probe-off-halted",
        "off",
        false,
        INPUT_CLEAR,
        esc_halted,
        |m| m.metas >= 3,
    );
    assert_eq!(off.writes, ["esc"], "one Escape: {:#?}", off.requests);
    assert!(
        off.journal.iter().any(|l| l.starts_with("HALTED seq=")
            && l.contains("halted by the owner")
            && l.contains("owed until the word turns")),
        "{:#?}",
        off.journal
    );
    assert!(
        !off.journal.iter().any(|l| l.starts_with("UNHALTED")),
        "no wait on the hold: {:#?}",
        off.journal
    );
    assert!(parked(&off), "{:#?}", off.journal);
    assert!(off.owed, "owed until the word turns");

    // NEGATIVE CONTROLS: Cruise. The failed Escape names its next try (the
    // run ends at the wait for it: the second wait on the header row after
    // the Escape, the first being the Escape's own).
    let cruise = off_panel_run(
        "probe-cruise-failed",
        "cruise",
        false,
        INPUT_CLEAR,
        esc_fails_once,
        |m| {
            m.requests
                .iter()
                .rposition(|r| verb_of(r) == "key" && r.ends_with(" esc"))
                .is_some_and(|at| {
                    m.requests[at..]
                        .iter()
                        .filter(|r| r.starts_with("await gone "))
                        .count()
                        >= 2
                })
        },
    );
    assert!(
        cruise
            .journal
            .iter()
            .any(|l| l.starts_with("WAITING seq=") && l.ends_with("(1 tries): Escape again in 5 s")),
        "{:#?}",
        cruise.journal
    );
    // The hold is waited out, and the Escape sent again.
    let cruise = off_panel_run(
        "probe-cruise-halted",
        "cruise",
        false,
        INPUT_CLEAR,
        esc_halted,
        |_| false,
    );
    assert!(
        cruise.journal.iter().any(|l| l.starts_with("UNHALTED")),
        "{:#?}",
        cruise.journal
    );
    assert_eq!(cruise.writes, ["esc", "esc"], "{:#?}", cruise.requests);
    assert!(!cruise.owed);
}
