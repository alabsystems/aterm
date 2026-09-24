// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The turn-end policy in the loop, over the scripted server: what the loop
// types, through which verb, under which guard, and what it raises — the
// owner's `[harness]` policy (every switch on), as the in-GUI host runs it.
// Each behaviour has its negative control. Included from `run.rs`'s tests
// (`mod turn_end`), so the `Mock` and the helpers there are in scope.

use crate::supervise::policy::turn_end::{RULE_CONTINUE, RULE_SUGGESTION};

/// The host's policy: every switch on (`SupervisorConfig::default()`).
fn hosted(max_s: u64) -> SuperviseOpts {
    SuperviseOpts {
        policy: SupervisorConfig::default(),
        ..auto(max_s, None)
    }
}

/// A turn that ended after 3 minutes of work (the done row's measure), the
/// worker's last words `said`, at an empty composer in bypass mode.
fn ended(said: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("⏺ {said}"),
        "",
        "✻ Worked for 3m 2s · done 4:24 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// [`ended`] with Claude Code's dim suggestion in the composer (`❯ keep
/// going`, the cursor left at column 2), or — `filled` — the suggestion
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
    let dir = std::env::temp_dir().join(format!("aterm-te-ledger-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("s-1.jsonl");
    (dir, path)
}

/// THE CONTINUE POLICY: a turn that ended after real work, with nothing
/// asked, is continued ONCE — `keep going` typed through the guarded submit
/// (`turn submit=guarded:^❯\skeep\x20going\s*$ …`), the session's
/// program read first, `CONTINUED` printed, a `typed` ledger row under
/// `continue@v1` — and the next turn, ending on a request for a decision,
/// is escalated with the worker's words, nothing typed. Negative control:
/// the policy switched off types nothing.
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
             \"need your decision\")"
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

/// THE WORKER'S OWN SUGGESTION, accepted by the vendor's accept key: `right`
/// fenced on the judged read's generation and guarded on the suggestion's
/// row, the screen read again, then Enter fenced on THAT read and guarded
/// on the filled row — `continue-suggestion@v1`, no `turn`. Negative
/// control: a host without the generation fence gets the same words typed
/// through the guarded submit instead, under the same rule.
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
    assert!(
        lines.contains(&"CONTINUED seq=102 rule=api-retry@v1 keep going".to_string()),
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
        waiting.summary.ends_with("overloaded retry 1 of 3"),
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

/// OWNER DECISION 3, in the loop: a Fable limit types `/model opus` under
/// `model-fallback@v1` (`TYPED`), raises no badge — the wall is handled,
/// journaled `LIMITED … handled` — and the `/model` output's point is
/// continued. Negative control: a bucket that asks consent to spend credits
/// is escalated and nothing is typed.
#[test]
fn a_fable_limit_switches_to_the_fallback_then_continues_with_no_badge() {
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
    let mut switched = rows(&[
        "⏺ Running the suite.",
        "",
        "❯ /model opus",
        "  ⎿  Set model to Opus 5 (1M context) (default)",
        "",
    ]);
    switched.extend(composer("  ? for shortcuts"));
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            fable.clone(),
            switched,
            busy_screen(),
            ended(STOP),
        ],
    );
    m.turn_gates = vec![1, 2];
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..hosted(30)
    };
    let (ldir, ledger) = ledger_at("fable-switch");
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    // The switch's ledger row says how to undo it (here: the bucket's
    // model, no reset named), and reads back as the open switch.
    assert_eq!(
        crate::supervise::approvals::open_model_switch(&ledger, None),
        Some(crate::supervise::approvals::OpenSwitch {
            from: Some("fable".to_string()),
            to: "opus".to_string(),
            back_at_unix: None,
        })
    );
    let _ = std::fs::remove_dir_all(&ldir);
    let decided: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("TYPED") || l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        decided,
        [
            "TYPED seq=102 rule=model-fallback@v1 /model opus (from fable; the notice names no \
             reset: not switched back; now the default for new sessions)",
            "CONTINUED seq=103 rule=model-fallback@v1 keep going",
        ],
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert!(m.requests.contains(&continue_request("/model opus")));
    assert!(
        records
            .iter()
            .any(|r| r.kind == "limited" && r.summary.starts_with("handled: You've reached")),
        "{records:#?}"
    );
    // Two badges: the default model moved for good (no reset named, so no
    // switch back — lane B2's review), and the stop phrase's; none for the
    // wall itself, which was handled.
    assert_eq!(count(&m, "meta set attention"), 2, "{:#?}", m.requests);
    assert!(
        m.requests.iter().any(|r| r.contains(
            "/model opus is now the default for new sessions: not switched back (no reset named)"
        )),
        "{:#?}",
        m.requests
    );

    // Negative control: the consent notice.
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
    let _ = watch_lines(&mut m, &hosted(30));
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.starts_with("limited: ")),
        "{:?}",
        m.attention
    );
}

/// A FULL CONTEXT is compacted — `/compact` under `context-compact@v1` —
/// and, once the compaction has run, continued under the same rule.
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

/// NEVER OVER A PERSON: a draft in the composer (the cursor after it) gets
/// nothing typed and raises nothing; a session whose program is a shell is
/// never typed into (the program read fresh before the act, `SKIPPED`
/// journaled); a guarded submit whose guard missed (`skipped`) is no act —
/// no `CONTINUED`, no ledger row. The control is the first test.
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
            .any(|r| r.kind == "skipped" && r.summary.contains("runs zsh, not Claude Code")),
        "{records:#?}"
    );

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
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN OFFER IS A CONTINUATION: `… want me to take that on?` reads as a
/// question and is continued, not escalated. Negative control: a plain
/// question for a person is escalated as ever and nothing is typed.
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
        Some("claude question: Did the suite pass on your machine? (the worker asked a question)")
    );
}

/// Lane B2's review (major 1), in the loop: a continuation whose reply ends
/// inside the `turn` verb's own settle is never read busy. Its point is
/// waited for [`TurnEndTiming::take_within`] (the deadline of the loop's own
/// wait, no sleep), then judged as the short yield it is — continued once
/// more at the SAME point ([`Session::turn_end_now`]) — and the second such
/// yield is escalated as done. Before the fix the first unseen point was
/// awaited forever: one `turn`, and nothing after. The negative control is
/// the third point: escalated, never typed into.
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
            ..TurnEndTiming::default()
        });
    });
    let continued: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("CONTINUED"))
        .collect();
    assert_eq!(
        continued,
        [
            "CONTINUED seq=102 rule=continue@v1 keep going",
            "CONTINUED seq=103 rule=continue@v1 keep going",
        ],
        "{lines:#?}\n{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 2, "{:#?}", m.requests);
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.contains("worker reports done")),
        "{:?}\n{lines:#?}",
        m.attention
    );
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

/// Lane B2's review (major 3): the continuation was PASTED by `turn` with
/// nothing fencing the paste on the judged read, so a person who started
/// typing in between got `keep going` spliced into a draft. Where the host
/// fences `send`, the text is written only while the screen is the judged
/// read's (`send if-gen=<g> if=<the composer row> -- <text>`), and Enter
/// goes, fenced on the read that shows the composer holding exactly that
/// text, under `continue@v1` — no `turn`. Negative controls: a screen that
/// moved before the write gets nothing written and no Enter; a composer
/// that does not hold the text as written gets no Enter and is escalated.
#[test]
fn a_fenced_host_writes_the_continuation_only_on_the_judged_screen() {
    let (dir, ledger) = ledger_at("fenced");
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
    let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
        s.set_approval_ledger(Some(ledger.clone()));
    });
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
            "key if-gen=1.103 if=^❯\\x20keep\\x20going\\s*$ enter",
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    assert!(
        std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .contains("\"decision\":\"typed\""),
        "the act recorded"
    );
    let _ = std::fs::remove_dir_all(&dir);

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

/// On a host without the send fence, every `turn` carries `yield=0.2`; a
/// person typing past the yield (`ERR yield timeout`) is no act and raises
/// nothing; a guard miss (`skipped reason=guard`) — the text left typed —
/// is escalated, never left for the next look to read as a person's draft.
#[test]
fn the_unfenced_turn_yields_to_a_person_and_says_what_it_left_typed() {
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
    let _ = watch_lines(&mut m, &hosted(30));
    assert!(
        m.attention
            .as_deref()
            .is_some_and(|a| a.contains("(typed, not submitted (")),
        "{:?}\n{:#?}",
        m.attention,
        m.requests
    );
}

/// Lane B2's review (major 4): past 40 characters the guard was the text's
/// last 40 characters, and Claude Code word-wraps its composer, so wherever
/// the wrap left fewer on the cursor's row the guard missed and the text
/// was stranded. The guard is now the end of the text's last word, which
/// the cursor's row holds at EVERY width. Negative control: the old 40-
/// character tail misses at some of the same widths.
#[test]
fn a_long_continuations_guard_matches_its_last_row_at_every_width() {
    let rules = "stay on the branch, never push to main, run the lane's tests before \
                 every commit, and keep the ledger rows exact";
    let text = format!("keep going (standing rules: {rules})");
    let guard =
        aterm_observe::row_matcher(&crate::supervise::run::turn_end_loop::composer_guard(&text))
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

/// Lane B2's review (minor): the typing budget lived only in the loop, so a
/// loop its host re-spawned started with a fresh six an hour. It is seeded
/// from the ledger's `typed` rows of the session within the window: six
/// from an earlier loop in the last hour spend it — escalated, nothing
/// typed. Negative controls: six from two hours ago, and six of another
/// session's, leave it whole — continued.
#[test]
fn the_typing_budget_counts_an_earlier_loops_acts_from_the_ledger() {
    for (tag, age, sid, spent) in [
        ("recent", 1, None, true),
        ("old", 120, None, false),
        ("other", 1, Some("@s-9"), false),
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
        let (lines, _) = watch_lines_with(&mut m, &hosted(30), |s| {
            s.set_approval_ledger(Some(ledger.clone()));
        });
        let _ = std::fs::remove_dir_all(&dir);
        if spent {
            assert_eq!(count(&m, "turn"), 0, "{tag}: {lines:#?}");
            assert!(
                m.attention
                    .as_deref()
                    .is_some_and(|a| a.contains("typing budget spent: 6 acts")),
                "{tag}: {:?}\n{lines:#?}\n{:#?}",
                m.attention,
                m.requests
            );
        } else {
            assert_eq!(count(&m, "turn"), 1, "{tag}: {lines:#?}");
        }
    }
}

/// `/model` also saves the switch as the owner's default for new sessions:
/// a switch with no way back is said (lane B2's review). Negative control: a
/// switch whose model and reset are known is switched back — and the
/// default with it — so it says nothing.
#[test]
fn a_switch_with_no_way_back_says_the_default_moved() {
    use crate::supervise::policy::turn_end::ModelSwitch;
    use crate::supervise::run::turn_end_loop::default_moved;
    let known = ModelSwitch {
        from: Some("fable".to_string()),
        to: "opus".to_string(),
        back_at: Some(Instant::now()),
    };
    assert_eq!(default_moved(&known), None);
    let no_reset = ModelSwitch {
        back_at: None,
        ..known.clone()
    };
    assert!(
        default_moved(&no_reset).is_some_and(|r| r.contains("default for new sessions")),
        "{:?}",
        default_moved(&no_reset)
    );
    let no_model = ModelSwitch {
        from: None,
        ..known
    };
    assert!(
        default_moved(&no_model).is_some_and(|r| r.contains("no model /model knows")),
        "{:?}",
        default_moved(&no_model)
    );
}

/// `rows` as Claude Code 2.1.281 draws its composer — the caret, then a
/// NO-BREAK SPACE (`❯\u{a0}`, `❯\u{a0}keep going`) — the row the server's
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

/// THE LIVE E2E's D1 (2026-09-24, Claude Code 2.1.281): the composer guard
/// ended `\x20*$`, the empty composer's caret row is `❯` and a NO-BREAK
/// SPACE, and every fenced write was answered a bare `OK skipped` —
/// journaled as "the screen moved", retried, and left: no continuation, no
/// slash command, nothing raised. Now the guard's trailing run is any
/// whitespace and the caret's space is `\s`, so the write and its Enter
/// land on that composer. Negative control: a bare skip on the unchanged
/// screen (the guard matched no row) is escalated with what was not typed,
/// never retried as if the screen had moved and never left silent.
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
            .is_some_and(|a| a.contains("not typed: the composer guard matched no row")),
        "{:?}",
        m.attention
    );
}

/// Run `run_hosted` over `m` until a stop set 150 ms in — with the hand-over
/// flag raised first when `hand_over` — and return its printed lines.
fn hosted_until_stopped(m: &mut Mock, hand_over: bool) -> Vec<String> {
    m.stall_sleep = Some(Duration::from_millis(5));
    let stop = Arc::new(AtomicBool::new(false));
    let handover = Arc::new(AtomicBool::new(false));
    let (flag, over) = (Arc::clone(&stop), Arc::clone(&handover));
    let setter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        over.store(hand_over, Ordering::SeqCst);
        flag.store(true, Ordering::SeqCst);
    });
    let mut out: Vec<u8> = Vec::new();
    let mut s = Session::new(m, Some("@s-1".to_string()));
    s.set_handover(handover);
    // Never the real state root's ledger.
    s.set_approval_ledger(Some(
        std::env::temp_dir().join(format!("aterm-hand-over-{}.jsonl", std::process::id())),
    ));
    let r = s.run_hosted(&hosted(30), stop, &mut out);
    setter.join().expect("setter");
    assert_eq!(r, Ok(()));
    String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

/// The reliability review of 2026-09-24 (major): a worker restarted by its
/// host (a policy edit, every seamless update) cleared the idle point's
/// escalation as it stopped, and the new loop — no work seen since, so
/// `worked` = 0 — raised nothing: the badge went while the worker still
/// waited on the owner. Now a stop that HANDS the session over leaves the
/// badge, and the next loop ADOPTS it (an idle point's too) — one `meta set`
/// in all, never cleared. NEGATIVE CONTROL (the old behaviour, a stop for
/// good): the badge is cleared, and the next loop raises none.
#[test]
fn a_handed_over_idle_escalation_survives_the_restart() {
    let mut m = Mock::new(true, vec![busy_screen(), ended(STOP)]);
    let first = hosted_until_stopped(&mut m, true);
    assert_eq!(
        first.last().map(String::as_str),
        Some("EXIT stopped"),
        "{first:?}"
    );
    let raised = m.attention.clone().expect("the idle point escalated");
    assert!(
        raised.starts_with("claude idle: I need your decision"),
        "{raised}"
    );
    assert_eq!(count(&m, "@s-1 meta unset attention owner=supervisor"), 0);
    let _ = hosted_until_stopped(&mut m, true);
    assert_eq!(
        m.attention.as_deref(),
        Some(raised.as_str()),
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "@s-1 meta set attention owner=supervisor"), 1);
    assert_eq!(count(&m, "@s-1 meta unset attention owner=supervisor"), 0);
    // The last loop stops for good: its adopted badge is released.
    let _ = hosted_until_stopped(&mut m, false);
    assert_eq!(m.attention, None, "{:#?}", m.requests);

    // Negative control: the stop is not a hand-over — cleared, and the
    // restarted loop raises nothing on the same point.
    let mut m = Mock::new(true, vec![busy_screen(), ended(STOP)]);
    let _ = hosted_until_stopped(&mut m, false);
    assert_eq!(m.attention, None);
    let _ = hosted_until_stopped(&mut m, false);
    assert_eq!(m.attention, None);
    assert_eq!(count(&m, "@s-1 meta set attention owner=supervisor"), 1);
}

/// A ledger holding one `/model` switch row (as the loop that switched wrote
/// it, [`crate::supervise::approvals::model_switch_reason`]), then — when
/// `restored` — the switch back's row.
fn seed_switch(path: &std::path::Path, back_at_unix: i64, restored: bool) {
    use crate::supervise::approvals::{Outcome, Row, model_switch_reason};
    use crate::supervise::policy::turn_end::{RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
    let now = crate::supervise::journal::unix_ms();
    let reason = model_switch_reason(Some("fable"), Some(back_at_unix));
    let mut rows = vec![
        Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: "/model opus",
            reason: &reason,
            box_seq: 90,
        }
        .to_json(now - 30 * 60_000, None),
        Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: "keep going",
            reason: "the turn-end policy",
            box_seq: 91,
        }
        .to_json(now - 29 * 60_000, None),
    ];
    if restored {
        rows.push(
            Row {
                rule_id: RULE_MODEL_RESTORE,
                outcome: Outcome::Typed,
                command: "/model fable",
                reason: "the turn-end policy",
                box_seq: 95,
            }
            .to_json(now - 10 * 60_000, None),
        );
    }
    std::fs::write(path, rows.join("\n") + "\n").expect("the ledger");
}

/// The reliability review of 2026-09-24 (major): owner decision 3's switch
/// back at the bucket's reset lived in one loop's memory, so a host restart
/// between `/model opus` and the reset left the session — and, `/model`
/// saving it, every new session — on opus for good. The switch's ledger row
/// now says how to undo it, and a loop that starts after it switches back
/// at the reset: `/model fable` under `model-restore@v1`. The write side:
/// the switching loop's row names the bucket's model. NEGATIVE CONTROLS: a
/// ledger whose switch was already undone, and one whose reset is still
/// ahead, get the plain continuation.
#[test]
fn a_model_switch_is_switched_back_by_a_loop_that_starts_after_it() {
    use crate::supervise::approvals::{OpenSwitch, open_model_switch};
    // 2026-09-17T12:55:00-07:00 is the watch tests' clock (TEST_NOW).
    let test_now: i64 = 1_789_660_500;
    for (tag, back_at, restored, expect_restore) in [
        ("due", test_now - 60, false, true),
        ("undone", test_now - 60, true, false),
        ("ahead", test_now + 3600, false, false),
    ] {
        let (dir, ledger) = ledger_at(&format!("switch-{tag}"));
        seed_switch(&ledger, back_at, restored);
        let open = open_model_switch(&ledger, None);
        assert_eq!(
            open,
            (!restored).then(|| OpenSwitch {
                from: Some("fable".to_string()),
                to: "opus".to_string(),
                back_at_unix: Some(back_at),
            }),
            "{tag}"
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
        let restore = m.requests.contains(&continue_request("/model fable"));
        assert_eq!(
            restore, expect_restore,
            "{tag}: {lines:#?}\n{:#?}",
            m.requests
        );
        if expect_restore {
            assert!(
                lines
                    .iter()
                    .any(|l| l.starts_with("TYPED seq=102 rule=model-restore@v1 /model fable")),
                "{tag}: {lines:#?}"
            );
        } else {
            assert!(
                m.requests.contains(&continue_request("keep going")),
                "{tag}: {:#?}",
                m.requests
            );
        }
    }
}
