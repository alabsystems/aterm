// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The turn-end policy in the loop, over the scripted server: what the loop
// types, through which verb, under which guard, and what it raises — the
// owner's `[harness]` policy (every switch on), as the in-GUI host runs it.
// Each behaviour has its negative control. Included from `run.rs`'s tests
// (`mod turn_end`), so the `Mock` and the helpers there are in scope.

use crate::supervise::policy::turn_end::{
    DONE_CHECK, RULE_CONSENT, RULE_CONTINUE, RULE_DONE_CHECK, RULE_SUGGESTION,
};

/// The host's policy, every switch on (`SupervisorConfig::default()`) but
/// the answers: these scripts end on a request for a decision ([`STOP`]),
/// which the owner's `answer_questions = false` hands to a person — so each
/// shows what the loop typed before it, and the escalation after. The
/// answer itself is [`a_stop_phrase_is_answered_with_the_answer_text`]'s.
fn hosted(max_s: u64) -> SuperviseOpts {
    SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        },
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
    let dir = crate::supervise::test_scratch_path("te-ledger", tag);
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

/// A WORKER THAT SAYS IT IS DONE is asked once (decided 2026-09-27 under
/// the owner's standing direction): the done check is typed where `keep
/// going` was, and when its answer is `DONE` the task is done — nothing more
/// is typed, and the journal says so once (`DONE …`). NEGATIVE CONTROL: the
/// same turn reported as progress is continued with `keep going`.
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
            "CONTINUED seq=102 rule={RULE_DONE_CHECK} {DONE_CHECK}"
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

    // NEGATIVE CONTROL: progress, not a done report.
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
/// REACTION with [`turn_end_loop::REACTION_WAIT`] — the screen moving —
/// before anything about the write is judged; never the approval press's
/// 2 s settle, which judged a worker merely slow to react (a continuation
/// escalated as not shown, an accept key's words typed after it: doubled).
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

/// THE ELEGANCE REVIEW OF 2026-09-25 (minor): a person's draft whose caret
/// was moved home (←, Home, ctrl-a) sits at column 2 of the caret row, where
/// the placeholder does too — the loop read it as the placeholder with no
/// look and typed its continuation in front of it, the failure the upgrade's
/// gate had measured. The `cell` at column 2 now says which: DIM is the
/// placeholder (continued, the suggestion accepted), anything else a typed
/// draft — never typed over. NEGATIVE CONTROL: the dim cell is the
/// placeholder, as before.
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
    // The act quotes the vendor's line (`wall_retry_text`), never the rote
    // `keep going`.
    assert!(
        lines.iter().any(|l| l.starts_with(
            "CONTINUED seq=102 rule=api-retry@v1 Claude Code reported \"API Error: 529 Overloaded."
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

/// OWNER DECISION 3, in a loop with no host to relaunch the agent (`drive
/// watch`, D7): a Fable limit is WAITED OUT to its reset — never Claude's
/// own `/model`, which also saves the person's default for every new
/// session — and raises no badge: the wall is handled, journaled `LIMITED …
/// handled`. (Where a host relaunches it, the fallback is a relaunch with
/// `--model`: `run_engine_tests`' `a_model_bucket_is_relaunched_…`.) A
/// bucket that asks CONSENT to go on on usage credits is accepted instead:
/// continued under `consent-accept@v1`, and no badge.
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

    // The consent notice: accepted (owner, 2026-09-24) — continued, no
    // switch, and no `limited:` badge.
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

/// Claude Code 2.1.281's `/login` method picker (the binary's strings, read
/// 2026-09-27: `Select login method:` and its three options), in the frame
/// it draws under the typed command — no composer. HAND-BUILT.
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

/// The incident's screen: the supervisor's `continue` answered by `⏺ Login
/// expired · Please run /login` ([`aterm_phase::prompt::fixtures::LOGIN_EXPIRED`]).
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

/// THE LOGIN WALL OF 2026-09-27 IN THE LOOP, over the incident's screen: the
/// supervisor's own `continue` answered by `⏺ Login expired · Please run
/// /login`. Main read it idle and typed `keep going` into it; now `/login` is
/// typed once under `auth-login@v1` and the owner is told as it is — the
/// badge `claude wall: Login expired · Please run /login (finish sign-in in
/// the browser)` — and the method picker it opens (no composer) is handed
/// to a person with no key pressed: which account to sign in with is theirs.
/// Nothing is continued until the login is back: at the person's `Login
/// successful` the worker is continued ONCE, and its next turn end is the
/// ordinary policy's. NEGATIVE CONTROL: no `keep going` is typed while the
/// wall stands (main typed one at each point).
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

/// NEVER OVER A PERSON: a draft in the composer (the cursor after it) gets
/// nothing typed and raises nothing within the grace — a draft first seen
/// is a keystroke just made ([`a_draft_left_standing_is_submitted_once_the_
/// grace_has_passed`] is what comes after it); a session whose program is a
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
            .any(|r| r.kind == "skipped" && r.summary.contains("runs zsh, no agent")),
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
        Some(
            "claude question: Did the suite pass on your machine? (the worker asked a question; \
             answer_questions is off)"
        )
    );
}

/// Lane B2's review (major 1), in the loop: a continuation whose reply ends
/// inside the `turn` verb's own settle is never read busy. Its point is
/// waited for [`TurnEndTiming::take_within`] (the deadline of the loop's own
/// wait, no sleep), then judged as the short yield it is — backed off
/// ([`TurnEndTiming::short_backoff`]) and continued once more at the SAME
/// point ([`Session::turn_end_now`]) — and never escalated as "done" (the
/// back-off grows instead). Before the fix the first unseen point was
/// awaited forever: one `turn`, and nothing after.
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
    assert_reaction_awaited(&m, "send if-gen=1.102");
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

/// THE HAZARDS REVIEW OF 2026-09-25 (major): a long text's guard was an
/// unanchored tail of its last word, and it matched a SHELL's cursor row —
/// the default `answer_text` against `…prefer reversible steps, and keep
/// going.` on zsh — so an agent that exited between the read and the write
/// would have had its answer run as a command. The tail now sits on a row
/// the composer draws: its caret row, or a continuation indented two.
/// NEGATIVE CONTROL: the composer's own last rows (Claude Code's `❯` and
/// Codex's `›`, NBSP caret included) still match.
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

/// Lane B2's review (minor): the typing budget lived only in the loop, so a
/// loop its host re-spawned started with a fresh six an hour. It is seeded
/// from the ledger's `typed` rows of the session within the window: under a
/// written `continue_per_hour = 6`, six from an earlier loop in the last hour
/// spend it — escalated, nothing typed. Negative controls: six from two
/// hours ago, six of another session's, and the default (`0`, no cap) leave
/// it whole — continued.
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

/// Run `run_hosted` over `m` until its host stops it — once the loop has
/// read the last screen and is watching past it ([`host_stops`]), not 150 ms
/// in, which a loaded machine outran before the idle point was reached (2
/// in 40, measured) — with the hand-over flag raised first when
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

/// A STOP IN A CONTINUATION'S SETTLE ends the hosted run before its next
/// wait (the review of 2026-09-24): after the accept key that fills the
/// worker's suggestion, and after the fenced write of the continuation, the
/// loop settles with two waits back to back — the key's change, then the
/// short idle — and the stop set as the first goes out
/// ([`stop_in_every_wait`], every other wait of the path too) ends the run
/// before the second, and before the Enter: `Session::call` refuses a wait
/// once the stop is set, as it refuses a write. NEGATIVE CONTROL, measured:
/// with that refusal of a wait removed, each path's stop set in `await seq
/// 102 timeout <REACTION_WAIT>` is followed by `await idle 500 timeout 1500`.
/// The settle's bound is read from [`REACTION_WAIT`] itself, never spelled
/// here: it moved from 2 s to 10 s once, and a copy of the number would have
/// made this test miss the settle it exists to stop in.
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
/// --model opus`, or before D7 `/model opus`), then — when `restored` — the
/// switch back's row.
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

/// The reliability review of 2026-09-24 (major): owner decision 3's switch
/// back at the bucket's reset lived in one loop's memory, so a host restart
/// between the switch and the reset left the session on the fallback for
/// good. The switch's ledger row says how to undo it — `relaunch --model
/// opus` since D7, `/model opus` in a ledger from before — and a loop that
/// starts after it reads it back as the OPEN switch; the switch back is a
/// relaunch its host makes (`run_engine_tests`'
/// `a_model_bucket_is_relaunched_…`). Here, with no host to relaunch, the
/// point gets its plain continuation — never `/model`. NEGATIVE CONTROLS: a
/// ledger whose switch was already undone, and one whose reset is still
/// ahead, read back no open switch or one not due.
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

/// FULL POWER: a request for a decision is ANSWERED — `answer_text` typed
/// through the same guarded write as a continuation, `CONTINUED …
/// rule=answer@v1`, a `typed` ledger row — and no badge is raised. NEGATIVE
/// CONTROL: under the owner's `answer_questions = false` ([`hosted`]) the
/// same point is escalated and nothing is typed.
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

/// A PERSON AT THE KEYBOARD wins (the server's `status human_ms=`): a
/// continuation the policy decided is not typed within `human_grace_s` of
/// their last keystroke — `WAITING … a person is typing`, the point decided
/// again when the grace ends — and a box the policy would answer is HELD,
/// neither pressed nor escalated. NEGATIVE CONTROLS: a keystroke older than
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

/// THE HAZARDS REVIEW OF 2026-09-25 (major): the host fought a manager
/// driving its worker — it read only `human_ms=`, never `hand=`, so it
/// continued every turn the manager dispatched and answered questions meant
/// for the manager. Another driver's hand on the session — a drive lease
/// (`hand=lease:<holder>`), a turn a named driver typed
/// (`hand=turn:<id>:<holder>`) — now holds the loop as a person's keystroke
/// does: no box pressed, nothing typed, nothing raised. NEGATIVE CONTROL: an
/// owner-class turn (`hand=turn:<id>`, as this loop's own continuations
/// are) holds nothing.
#[test]
fn another_drivers_hand_holds_the_loop() {
    let opts = || SuperviseOpts {
        policy: SupervisorConfig {
            human_grace_s: 60,
            ..SupervisorConfig::default()
        },
        ..auto(2, None)
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

/// A BOX HELD FOR A PERSON IS ANSWERED ONCE THEIR GRACE HAS PASSED: the
/// first status says they typed half a second ago (`HELD`, nothing pressed
/// or raised); when the grace runs out the box on the screen is judged
/// again, on a fresh status that says they have stopped, and pressed —
/// never before the grace, nothing raised while it was held. (The scripted
/// box never leaves, so full power reads it as a press that did not land
/// and tries it again after its back-off — not this test's.)
/// NEGATIVE CONTROL:
/// a person who keeps typing keeps it held to the end of the budget.
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

/// A DRAFT LEFT STANDING is the next message (lane P's review: a draft
/// stopped a fully automatic session for ever, with nobody told): once
/// nothing has changed it for the grace — the loop's own clock, since this
/// server says nothing of a person — the policy's act goes as the draft's
/// Enter, fenced on the judged read and guarded on its caret row, and is
/// said as the act's `CONTINUED` line with the draft's words; nothing is
/// typed into it and nothing raised. NEGATIVE CONTROL: a draft still
/// changing is a person typing — every read shows new text — and nothing is
/// pressed.
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

/// CODEX 0.158.0 DRAWS ITS INPUT LINE `»` (measured 2026-09-28 on the
/// owner's goal-mode tab: `cell 60 0` read `»` bold): the loop reads the
/// same screens with that mark as it reads 0.156.1's, and the continuation
/// is guarded on the mark DRAWN (`submit=guarded:^»\skeep…`) — spelled `›`,
/// the guard never matched the new input line and the text was left typed
/// and unsent. NEGATIVE CONTROL: the 0.156.1 screens keep `^›`
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

/// CODEX, END TO END over the scripted server (codex 0.156.1's measured
/// screens, `program=codex`): the loop reads Codex's own grammar — busy
/// while its status row runs, an ended turn at its end row — and at full
/// power continues the turn in CODEX's composer: on a host that fences only
/// `key`, through the guarded `turn` (`submit=guarded:^›\skeep\x20going…`);
/// on one that fences `send` too, through the fenced write and a fenced
/// Enter held until the echo is still 600 ms (Codex takes an Enter right
/// behind typed text as a newline, its paste guard, measured). A box on the
/// way is answered by its role (`1`, `Yes, proceed`, guarded on its `$`
/// row).
/// NEGATIVE CONTROL: the same screens under `program=zsh` type and press
/// nothing.
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

    // A host that fences `send` (the hazards review of 2026-09-25): the
    // continuation is written only on the judged read, and its Enter goes
    // fenced once the echo has held still for Codex's paste guard (600 ms),
    // never a bare `turn` whose paste checks nothing.
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

    // NEGATIVE CONTROL: a shell showing the same screens.
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

/// A host whose measure of the API's reach is scripted ([`IdleHost::reach`]):
/// each ask answers the next entry, the last for ever after; every ask is
/// counted. It takes no step and owns no turn end.
#[derive(Debug, Default)]
struct ReachHost {
    script: std::sync::Mutex<VecDeque<Reach>>,
    last: std::sync::Mutex<Reach>,
    asks: std::sync::atomic::AtomicUsize,
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
}

impl IdleHost for ReachHost {
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

/// THE API BACK, WITHIN ONE STEP (the outage of 2026-09-27: the network came
/// back at 19:15, and a rung may be minutes off). At the outage's point the
/// host measures the API DOWN — the hold is journaled, nothing typed — and
/// its measure turns UP while the loop waits: the point is decided again
/// at the top of the very next step, and continued EXACTLY ONCE under
/// `api-back@v1`, in words that quote the vendor and say the API is back,
/// with no rung and no hold waited out. Each further Down answer costs
/// exactly one step more (the same run with one more Down makes exactly one
/// more `await` before the `turn`). NEGATIVE CONTROL: a host that stays
/// DOWN types nothing before the hold — the point is held, and journaled so.
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
            "CONTINUED seq=102 rule=api-back@v1 Claude Code reported \"API Error: Can't reach the \
             API server"
        ) && continued[0].contains("the API is reachable again now"),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert_eq!(waiting.len(), 1, "the hold, said once: {waiting:#?}");
    assert!(waiting[0].contains("the API is unreachable"), "{waiting:?}");
    // Asked at the point, at the top of each of the three steps, and by the
    // decision the Up made and the act's record of it — never after the
    // wall left (the next point, a request for a decision, asks nothing).
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

    // NEGATIVE CONTROL: the host stays Down — nothing typed before the hold.
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

/// A DOWN LOST IS BACK ON THE LADDER (the review of 2026-09-27): at the
/// outage's point the host measures the API DOWN, and the hold (an hour
/// here) is journaled; while the loop waits, the measure is LOST (a stale
/// measure, a probe past its budget: `Unknown`). The point is decided again
/// at the top of that step, as the policy and its model decide on the
/// measure as it stands — here the unmeasured wall's ladder, whose first
/// rung is already past — and continued once under `api-retry@v1`, never
/// held out to the Down's hour. NEGATIVE CONTROL: the same clocks with the
/// host staying Down type nothing (the hold stands).
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
            "CONTINUED seq=102 rule=api-retry@v1 Claude Code reported \"API Error: Can't reach \
             the API server"
        ) && continued[0].contains("; trying again."),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    assert_eq!(waiting.len(), 1, "the hold, said once: {waiting:#?}");
    assert!(waiting[0].contains("the API is unreachable"), "{waiting:?}");

    // NEGATIVE CONTROL: the host stays Down — the hold stands.
    let (lines, _, m, _) = outage_timed(&[down], 1, clocks());
    assert_eq!(count(&m, "turn"), 0, "{lines:#?}\n{:#?}", m.requests);
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "{lines:#?}"
    );
}

/// ASKING IS WAITING: an ordinary point — a turn that ended after work, a
/// request for a decision — never asks the host for the API's reach, so a
/// host's probe runs only while some loop waits at a network wall. And at a
/// network wall the owner's policy ESCALATES (`retry_api_errors = false`),
/// the host is asked once, at the point, and never again: no wait of the
/// policy's stands, so a measure turning up cannot decide the point again —
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

// --- Codex's rate-limit nudge and its save-then-wait switch (2026-09-28) ------

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
/// ([`crate::supervise::approvals::wind_switch_words`]), on the TEST'S clock
/// ([`TEST_NOW`]): the loop reads a person's keystrokes against the rows'
/// times, and rows stamped on the wall clock lie days after the test's.
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

/// A HOLD SURVIVES THE LOOP'S RESTART: a switch whose last row says the
/// session is held on GPT-6-Astra ultra until its window resets, read back
/// by a loop that starts after it at the idle point the hold left, types
/// NOTHING into the idle Codex — no
/// continuation, no `/model`, no answer — and raises nothing. The switch's
/// rows round-trip ([`crate::supervise::approvals::open_wind_down`]).
/// NEGATIVE CONTROLS: a switch whose last row says `done` is no switch — the
/// point is continued; `released` too.
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

/// CODEX'S PURSUED GOAL IN THE LOOP: an ended turn whose footer says the goal
/// is pursued gets nothing typed and nothing raised — Codex starts the next
/// turn itself. NEGATIVE CONTROL: the same point with the goal paused is
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

/// Codex's rate-limit nudge NEAR ITS LIMIT, END TO END over the scripted
/// server — the incident's shape (a goal pursued, 99% of the weekly window):
/// the nudge's `1` pressed under `rate-nudge-switch@v1`, its row carrying the
/// switch; the goal turn under the box stopped by ONE Esc guarded on its
/// status row; the save instruction typed (never `keep going`); its marker
/// line read as the save; `/model` typed and the picker driven to the
/// original model (Enter on the model box) and effort (`s` — this
/// conversation — on the effort box, never Enter or a digit); the thread
/// seen back on GPT-6-Astra medium, HELD: nothing typed after. The ledger
/// carries every edge, and reads back as a hold.
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
        screen(cx::RATE_NUDGE),
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
    assert!(
        turns.iter().any(|t| t.ends_with(" /model")),
        "the restore: {why}"
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
    // The press's INTENT is ledgered before its key, and the switch's rows
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
    // Both only INTENDED until the box is seen leaving; the Esc's row, the
    // switch's first act, carries it as owed — and its Esc (`esc=`), its
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
    // ONE Esc: the goal turn under the box.
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

/// THE GOAL STOP, BOUND TO THE LOOP (the Tier-1 walk binds the decision,
/// `TurnEndState::goal_stop`; this binds the key): a switch owed its
/// wind-down — carried on from the ledger, as after a restart — and Codex
/// running a turn on the cheaper model: the busy read sends ONE Esc guarded
/// on the turn's status row (`key if=<busy guard> esc`), ledgered under
/// `model-wind-down@v1` with the switch's stops. NEGATIVE CONTROLS: a
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

/// A PRESS OF THE NUDGE'S SWITCH THAT DID NOT LAND (the box did not change):
/// its intent row is closed as `released`, no switch opens — nothing of the
/// wind-down goes — and a restarted loop reads none back.
#[test]
fn a_nudge_switch_that_did_not_land_opens_nothing() {
    use crate::supervise::codex_usage::{CodexSeen, LimitRead};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let test_now: i64 = 1_789_660_500;
    let astra = "GPT-6-Astra medium";
    let screens = vec![
        codex_footer(screen(cx::END_OF_TURN), astra, None),
        screen(cx::RATE_NUDGE),
    ];
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

/// A switch's three rows as a loop that opened it wrote them — the press,
/// its wind-down, then `phase` with `extra` words (`stops=3 told=goal`) —
/// on the TEST'S clock ([`TEST_NOW`]), the last `last_age_s` before it.
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

/// A PERSON'S OWN TURN IS NEVER STOPPED BY THE LOOP (the re-review of
/// 2026-09-28, its scratch loop test made real): a switch carried on from
/// the ledger — held, owed its save, owed its model back — and a turn
/// running whose person's keystroke (`status human_ms=121000`) is past the
/// grace but after the switch's last row: their `/goal resume`, their
/// message. No Esc goes. NEGATIVE CONTROL: a keystroke from before that row
/// (25 minutes) is no hand in the turn — the goal's turn is stopped, one Esc
/// guarded on its status row.
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

/// THE LATCH: a busy read that saw a person's hand IN the running turn keeps
/// it theirs until the turn's point, however the keystroke ages; the turn's
/// own work keeps it theirs past the grace. A keystroke from before the open
/// switch opened is none of any turn it reads (the round-3 re-review).
/// NEGATIVE CONTROLS: unlatched, a keystroke older than the turn so far is
/// none; one after the opening, within the turn, is theirs.
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

/// A SWITCH UNDER A CODEX BACKGROUND TERMINAL GOES ON (the re-review of
/// 2026-09-28, its scratch loop tests made real): the save's own turn ends
/// with a background terminal still running — Codex draws its line under
/// the ended turn, and the screen reads busy for as long as it runs — and
/// the break is the switch's point: the marker judged (`saved`) and `/model`
/// typed; an owed save under the same line is typed. Nothing else goes at
/// that break: no continuation, no host step. CONTROLS: the same screens
/// without the line do the same at an ordinary point.
#[test]
fn a_switch_under_a_background_terminal_goes_on() {
    let paused = Some("Goal paused (/goal resume)");
    for bg in [true, false] {
        // Winding: the save's end carries its marker.
        let (dir, ledger) = ledger_at(&format!("bg-winding-{bg}"));
        seed_wind_at(&ledger, "winding", "", 60);
        let end = under_background(
            &["• Committed and pushed.", "", "  ATERM-SAVED-3f9a1c2e"],
            "GPT-6-Luna medium",
            paused,
            bg,
        );
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
        let why = format!("bg={bg}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        assert!(
            m.requests
                .iter()
                .any(|r| r.starts_with("turn ") && r.ends_with(" /model")),
            "the restore: {why}"
        );
        assert!(
            rows.contains("\"command\":\"saved\""),
            "judged saved: {why}"
        );
        assert!(
            !m.requests
                .iter()
                .any(|r| r.starts_with("turn ") && r.contains("keep going")),
            "{why}"
        );
        assert_eq!(
            m.requests
                .iter()
                .filter(|r| r.starts_with("turn ") && r.ends_with(" /model"))
                .count(),
            1,
            "once, the break read again deciding nothing new: {why}"
        );
        // Owed: the goal turn in flight when the loop started ended, a
        // terminal it started still running.
        let (dir, ledger) = ledger_at(&format!("bg-owed-{bg}"));
        seed_wind_at(&ledger, "owed", "", 60);
        let stopped = under_background(
            &["• The parser's second step is done."],
            "GPT-6-Luna medium",
            paused,
            bg,
        );
        let mut m = codex_mock(vec![stopped.clone(), stopped.clone(), stopped], 3);
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
        });
        let _ = std::fs::remove_dir_all(&dir);
        let why = format!("bg={bg}: {lines:#?}\n{:#?}", m.requests);
        assert!(
            m.requests
                .iter()
                .any(|r| r.starts_with("turn ") && r.contains("saving your work")),
            "the save: {why}"
        );
    }
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

/// NO UPGRADE NOTICE INTO AN OPEN SWITCH AT A BACKGROUND BREAK (the
/// re-review of 2026-09-28): the host's background step is offered no break
/// while a save-then-wait switch is open, as at an idle point — over the
/// loop (where the switch takes the break as its own point first) and at
/// the step's own gate (a loop that does not take it: another supervisor's
/// claim, an attended look), where the loop SAYS so to the host
/// (`guard=switch-open`, [`IdleHost::withheld`]). NEGATIVE CONTROL: with no
/// switch, the same break is offered and nothing is said withheld.
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

/// THE GOAL'S NOTE STAYS UP (the re-review of 2026-09-28): the stops spent
/// and Codex's goal running on while the switch stands, a person is told —
/// the badge raised, the note recorded for them by the host, and the
/// switch's row carrying it as said (`told=goal`) — and the goal's next
/// turn does NOT take the badge down. A loop restarted from those rows says
/// it no more. NEGATIVE CONTROL: the footer shows the goal paused — the
/// badge goes.
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

/// A PRESS ONLY INTENDED, READ BACK (the re-review's nit): a loop that died
/// between the intent row and the key finds the nudge still up — its row
/// only intended (`phase=intent`), the box is decided again and its switch
/// pressed, near the limit. NEGATIVE CONTROL: a switch whose row says it
/// landed (`owed`) keeps the model at the same nudge.
#[test]
fn an_intended_press_leaves_the_nudge_to_be_decided_again() {
    use crate::supervise::approvals::{Outcome, Row};
    use crate::supervise::codex_usage::{CodexSeen, LimitRead};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    for (phase, switched) in [("intent", true), ("owed", false)] {
        let (dir, ledger) = ledger_at(&format!("intent-{phase}"));
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
        std::fs::write(&ledger, row + "\n").expect("the ledger");
        // The loop starts at the box, which covers the footer: the intended
        // switch's own `from` is the thread's model.
        let mut m = codex_mock(vec![screen(cx::RATE_NUDGE)], 2);
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

/// A SWITCH THAT CANNOT GO ON UNDER A BACKGROUND TERMINAL IS SAID, NAMING
/// WHAT HOLDS IT (the round-3 re-review: it blamed the terminal, and advised
/// a hand `/model` before the save, whatever held it): the save owed at a
/// break a person is typing through types nothing, and past the bound
/// (shortened here) a person is told once that THEIR TYPING holds it; with
/// nobody's hand the save is typed, and when the break does not move after
/// it, the TERMINAL is named, with `/ps`, `/stop`. Neither advises putting
/// the model back by hand before the save. The note rides its own row
/// (`told=background`), so a loop restarted from it says it no more.
/// NEGATIVE CONTROL: within the bound, nothing is raised.
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

// --- the round-3 re-review's scenarios (2026-09-28), made real -------------

/// THE GOAL TURN UNDER THE NUDGE'S BOX IS STOPPED, WHOEVER BEGAN THE TURN THE
/// BOX COVERED (the round-3 re-review's regression, its two scratch loop
/// tests made real): the nudge flow near the limit, the loop's span of the
/// turn before the box preset — a person's message began it ten minutes ago
/// (`human_ms=610000`), typed into it five minutes ago (`300000`), or their
/// `/goal` three hours ago with the goal's turns chained since — and Codex's
/// goal runs its own turn under the box on GPT-6-Luna: ONE Esc, guarded on
/// its status row — as with nobody's hand (the control). (A person's
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
            screen(cx::RATE_NUDGE),
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

/// A CARRIED-ON HOLD'S ESCAPED GOAL IS STOPPED THOUGH A PERSON TYPED A DAY
/// AGO (the round-3 re-review, its scratch loop test made real): the hold's
/// last row three days old, a person's keystroke a day ago (after the row,
/// long before the turn in flight), and Codex's goal running on the
/// thread's own model at the restart: ONE Esc, the hold stands (never "a
/// person resumed Codex's goal during the hold"), and the goal is paused at
/// its point. NEGATIVE CONTROL: a keystroke two minutes ago — their `/goal
/// resume` — is theirs: no Esc, and its end releases the hold.
#[test]
fn a_carried_on_holds_escaped_goal_is_stopped_past_an_old_keystroke() {
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
        }
    }
}

/// A GOAL THE LIVE UPGRADE HOLDS IS NEVER THE SWITCH'S (the owner's decision
/// of 2026-09-28; the tab's goal record, `harness::goal_hold`): the carried-on
/// hold above, its goal turn running — but the record beside the loop's
/// ledger says the upgrade paused Codex's goal for its move. The switch
/// presses no Esc into the turn that pause lets finish, types no `/goal
/// pause`, and writes no claim of its own over the upgrade's. NEGATIVE
/// CONTROL: no record — the Esc and the pause, as above, and the switch's
/// claim on the record.
#[test]
fn a_goal_the_upgrade_holds_is_never_stopped_or_paused_by_the_switch() {
    use crate::harness::goal_hold::{self, Hold, How, Owner, Stage};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let astra = "GPT-6-Astra ultra";
    let pursuing = Some("Pursuing goal (1h 2m)");
    for upgrade_holds in [true, false] {
        let (dir, ledger) = ledger_at(&format!("upgrade-holds-{upgrade_holds}"));
        seed_wind_at(&ledger, "holding", "", 3 * 86_400);
        let record = goal_hold::path_beside(&ledger);
        if upgrade_holds {
            goal_hold::write(
                &record,
                &Hold {
                    stage: Stage::Paused,
                    ..Hold::pausing(Owner::Upgrade, How::Typed, 42, "0.158.0", 1)
                },
            )
            .expect("record");
        }
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
        let why = format!("{upgrade_holds}: {lines:#?}\n{:#?}", m.requests);
        let escs = m
            .requests
            .iter()
            .filter(|r| r.starts_with("key ") && r.ends_with(" esc"))
            .count();
        let paused = m
            .requests
            .iter()
            .any(|r| r.starts_with("turn ") && r.ends_with(" /goal pause"));
        assert_eq!(
            (escs, paused),
            (usize::from(!upgrade_holds), !upgrade_holds),
            "{why}"
        );
        let owner = held.map(|h| (h.owner, h.stage));
        if upgrade_holds {
            assert_eq!(owner, Some((Owner::Upgrade, Stage::Paused)), "{why}");
        } else {
            assert_eq!(
                owner,
                Some((Owner::Switch, Stage::Paused)),
                "claimed: {why}"
            );
        }
    }
}

/// THE SAVE'S END UNDER A BACKGROUND TERMINAL, HOWEVER IT ENDED (the round-3
/// re-review, rr3's and crs7q's scratch loop tests made real): the save's
/// turn ends ASKING — a rejected push, "Should I merge them and push
/// again?" — with a background terminal still running: the break is the
/// switch's point as an idle end is — the marker judged (not saved: a
/// person told, quoting its words) and `/model` typed; an owed save under
/// the same line is typed. A saved turn that ends on an offer is judged
/// saved. A save that ended on luna's own usage WALL under the line is
/// judged there too. CONTROLS: the same screens without the line.
#[test]
fn a_switch_under_a_background_terminal_goes_on_however_the_turn_ended() {
    let paused = Some("Goal paused (/goal resume)");
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
        for (phase, said, saved) in [
            ("winding", &asks[..], Some(false)),
            ("winding", &offers[..], Some(true)),
            ("winding", &wall[..], Some(false)),
            ("owed", &asks[..], None),
        ] {
            let (dir, ledger) = ledger_at(&format!("bg-ended-{phase}-{bg}-{}", said.len()));
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

/// FRAMES BETWEEN THE PICKER'S BOXES (the round-3 re-review, crs7q's
/// scratch test made real): the nudge flow with idle frames between the
/// model box and the effort box — frames the loop's own settle outlasts and
/// reads as an idle POINT — types `/model` ONCE: the restore's claim on the
/// picker stands until it has been gone [`PICKER_SETTLE`], the effort box is
/// answered as the restore's (`s`, this conversation), and the session is
/// held on its own model. NEGATIVE CONTROL: no frame between — the same.
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
    for between in [0usize, 2, 3] {
        let mut screens = vec![
            codex_footer(screen(cx::BUSY), astra, pursuing),
            screen(cx::RATE_NUDGE),
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

/// THE OWED FOOTER GATE READS THE THREAD SINCE THE SWITCH OPENED (the
/// round-3 re-review, bound to the loop's reading): a switch carried on as
/// owed, opened at `opened=`, its footer back on GPT-6-Astra at a point,
/// and the thread's rollout saying its last turn ran gpt-6-luna BEFORE the
/// opening — the goal turn under the box, say — is the switch that did not
/// land: released, nothing typed. NEGATIVE CONTROL: that turn begun after
/// the opening is a footer lagging it — the save waits, the switch open.
#[test]
fn the_owed_footer_gate_reads_the_thread_since_the_switch_opened() {
    use crate::supervise::codex_usage::CodexSeen;
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let opened = TEST_NOW - 10 * 60;
    for (turn_at, released) in [(opened - 60, true), (opened + 60, false)] {
        let (dir, ledger) = ledger_at(&format!("footer-since-{released}"));
        seed_wind_at(&ledger, "owed", &format!("opened={opened}"), 5 * 60);
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
        let why = format!("{turn_at}: {lines:#?}\n{:#?}\n{rows}", m.requests);
        assert_eq!(open.is_none(), released, "{why}");
        assert_eq!(rows.contains("did not land"), released, "{why}");
        assert!(
            !m.requests.iter().any(|r| r.starts_with("turn ")),
            "nothing typed: {why}"
        );
    }
}

// --- the round-4 re-review's scenarios (2026-09-28), made real -------------

/// Esc keys the loop sent.
fn escs(m: &Mock) -> usize {
    m.requests
        .iter()
        .filter(|r| r.starts_with("key ") && r.ends_with(" esc"))
        .count()
}

/// THE SAVE'S OWN TURN PAST ITS BOUND IS STOPPED ONCE, JUDGED, AND THE
/// MODEL PUT BACK (the owner's rule: the cheaper model only commits and
/// pushes; the round-4 re-review: nothing bounded the save's turn): a switch
/// carried on WINDING, the save's turn running on gpt-6-luna with 16 minutes
/// of busy work behind it (the loop's span preset), gets ONE Esc guarded on
/// its status row, journaled as the save's; its interrupted point judges
/// the marker — none: a person is told, naming the stop — and `/model` is
/// typed. NEGATIVE CONTROL: five minutes into the save, no Esc goes; its
/// end, carrying the marker, is the save, and `/model` follows all the same.
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

/// An owed switch's press only INTENDED, as the loop ledgers it before the
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

/// A NOTE SAID WHILE THE PRESS IS ONLY INTENDED KEEPS IT INTENDED (the
/// round-4 re-review, its scratch loop test made real): a press carried on
/// as only intended (`phase=intent`: the key may never have gone), the
/// session at a break under a background terminal whose footer shows no
/// model — the stood-still note (its bound shortened) rides its own row,
/// which says `phase=intent` and names the footer as what holds the switch,
/// never that the session is on gpt-6-luna. A loop restarted from those
/// rows, Codex's goal running on the thread's OWN model, sends NO Esc (the
/// press may never have landed). NEGATIVE CONTROLS: the intent row alone —
/// no Esc either; the same rows said as landed (`phase=owed`, the defect's
/// word) — the Esc goes.
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
    // CONTROLS: the intent row alone; the rows said as landed.
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

/// A PRESS ONLY INTENDED, A DRAFT AT ITS POINT (the round-4 re-review's
/// second loop probe made real): the footer showing no model, or
/// gpt-6-luna, and a person's draft standing — nothing is typed over it, no
/// row claims the session is on gpt-6-luna to save the work, and a
/// restarted loop still reads the press as intended (a footer seen landing
/// it writes no row of its own: the next loop reads it off the footer
/// again).
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

/// THE OWED FOOTER GATE READS THE THREAD SINCE THE PRESS (the round-4
/// re-review, its scratch probe made real): a switch carried on as owed,
/// pressed at `pressed=` and seen opening twenty seconds later at
/// `opened=`, its footer back on GPT-6-Astra at a point, and the thread's
/// rollout saying its last turn ran gpt-6-luna — begun between the press and
/// the opening (the goal turn under the box: ten seconds, or one, before the
/// opening), or in the press's own second (the rollout's time cut to the
/// second): a footer lagging it — the save waits, the switch open. With no
/// `pressed=` (an older row), the opening floors it, a second's slack
/// included. NEGATIVE CONTROL: a turn a minute before
/// the press is none of the switch's — released as the switch that did not
/// land, nothing typed.
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

/// THE STOOD-STILL NOTE AT A BREAK IS ONLY SAID (the round-4 re-review, its
/// ports made real): the save's turn ended under a background terminal —
/// on its marker and an offer, on a rejected push's question, or plainly —
/// and the note's bound at zero: the break is decided ONCE (the marker
/// judged, `/model` typed once), and read again before the wait that
/// decision named, the switch standing still is said without deciding the
/// break again — no second `/model`. CONTROL: the same screens without the
/// line type one `/model` at an ordinary point.
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

/// THE POLICY'S OWN HOLD IS NAMED AT A BREAK (the round-4 re-review: a
/// switch held by the policy's own wait was told as the terminal's, with
/// `/ps` and `/stop`, which move nothing): an owed save at a break under a
/// background terminal whose footer shows no model — its bound shortened —
/// is said to wait on the footer; one whose thread fell into a sandbox, to
/// be held by the sandbox, with the resume that fixes it. NEGATIVE CONTROL:
/// the footer on gpt-6-luna types the save, and the break that then stands
/// still names the terminal.
#[test]
fn the_policys_own_hold_is_named_at_a_break() {
    use crate::supervise::codex_usage::CodexSeen;
    for (label, footer, sandbox, held) in [
        ("footer", false, false, "footer has not shown gpt-6-luna"),
        ("sandbox", true, true, "fell into a sandbox"),
        (
            "terminal",
            true,
            false,
            "background terminal keeps the session busy",
        ),
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
        assert_eq!(noted[0].contains("/ps"), label == "terminal", "{why}");
        assert_eq!(
            m.requests
                .iter()
                .any(|r| r.starts_with("turn ") && r.contains("saving your work")),
            label == "terminal",
            "{why}"
        );
    }
}

/// THE SWITCH'S OPENING CLOSES THE BOX'S SPAN ITSELF (the round-4
/// re-review: no test pinned `open_wind`'s call of the box close, and
/// deleting it left every suite green): a person's hand latched in the turn
/// the nudge's box covered, then the switch opened by the loop's own
/// `open_wind` — the latch is spent and the span begun afresh, so the goal
/// turn Codex runs under the box is no person's, and a busy read of it
/// measures its own work. The keystroke itself (before the opening) is
/// floored as before. NEGATIVE CONTROL: the switch opened without the box
/// close (the policy's `open_switch` alone) keeps the latch — the goal
/// turn would be spared.
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

// --- the goal-pause review's scenarios (2026-09-28), made real -------------

/// THE SAVE'S BOUND STANDS UNDER THE UPGRADE'S GOAL HOLD (the goal-pause
/// review of 2026-09-28, its probe made real): the owner's bound test above
/// — a switch carried on WINDING, 16 minutes of save work behind it — run
/// with and without the live upgrade's Paused hold on the tab's goal record.
/// The upgrade's hold keeps the loop's Esc off a GOAL's turn (the one its
/// pause lets finish), never off the switch's own save turn: ONE Esc either
/// way. Until that day it was none under the hold, and the cheaper model's
/// save ran unbounded.
#[test]
fn the_saves_bound_stands_under_the_upgrades_goal_hold() {
    use crate::harness::goal_hold::{self, Hold, How, Owner, Stage};
    use aterm_phase::codex::fixtures as cx;
    use aterm_phase::prompt::fixtures::screen;
    let luna = "GPT-6-Luna medium";
    let paused = Some("Goal paused (/goal resume)");
    let mut got = Vec::new();
    for upgrade_holds in [false, true] {
        let (dir, ledger) = ledger_at(&format!("wind-bound-upgrade-{upgrade_holds}"));
        seed_wind_at(&ledger, "winding", "", 60);
        if upgrade_holds {
            goal_hold::write(
                &goal_hold::path_beside(&ledger),
                &Hold {
                    stage: Stage::Paused,
                    ..Hold::pausing(Owner::Upgrade, How::Typed, 42, "0.158.0", 1)
                },
            )
            .expect("record");
        }
        let busy = codex_footer(screen(cx::BUSY), luna, paused);
        let end = codex_footer(screen(cx::INTERRUPTED), luna, paused);
        let mut m = codex_mock(vec![busy.clone(), busy, end], 2);
        let opts = SuperviseOpts {
            policy: SupervisorConfig::default(),
            ..auto(30, None)
        };
        let _ = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_ledger(Some(ledger.clone()));
            s.set_codex_records(crate::supervise::codex_usage::CodexSeen::default());
            s.running.busy(
                Instant::now()
                    .checked_sub(Duration::from_secs(16 * 60))
                    .expect("the clock"),
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
        got.push((upgrade_holds, escs(&m)));
    }
    assert_eq!(got, vec![(false, 1), (true, 1)]);
}

/// THE PAUSED GOAL'S BOX IS ANSWERED ONLY ON THE UPGRADE'S OWN RELAUNCH (the
/// goal-pause review of 2026-09-28: `goal-resume@v1` was armed by the
/// upgrade's hold alone, and pressed `Resume goal` on any paused-goal box in
/// the tab — a person's own `codex resume` of another goal included). The
/// box is pressed where the hold names the Codex it relaunched and that
/// Codex still leads its terminal; its press is on the upgrade's record
/// (`Resuming`, `moved`). NEGATIVE CONTROLS, each alone: the relaunch no
/// longer leading (the box is another Codex's), and the box left to a person
/// at the keys (`BOX_THEIRS`) — nothing pressed, the record as it was.
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
