// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The wall confirmed before it is acted on (`wall_confirm.rs`, H6-HARNESS-1)
// in the loop, over the scripted server: the screens Claude Code 2.1.294 drew
// (`aterm_phase::prompt::fixtures`) under a host that hands the loop the
// transcript it wrote (`supervise/fixtures/claude-2.1.294-transcript-*`).
// Included from `run.rs`'s tests (`mod wall_confirm_loop`), so the `Mock`
// and the helpers there are in scope.

use aterm_phase::prompt::fixtures as fx;

/// A measured transcript, by name (`supervise/fixtures/`).
fn transcript(name: &str) -> &'static str {
    match name {
        "529" => include_str!("fixtures/claude-2.1.294-transcript-529.jsonl"),
        "context" => include_str!("fixtures/claude-2.1.294-transcript-context.jsonl"),
        "not-logged-in" => include_str!("fixtures/claude-2.1.294-transcript-not-logged-in.jsonl"),
        "weekly-limit" => include_str!("fixtures/claude-2.1.294-transcript-weekly-limit.jsonl"),
        "bash-output-then-context" => {
            include_str!("fixtures/claude-2.1.294-transcript-bash-output-then-context.jsonl")
        }
        "bash-output-wall-phrase" => {
            include_str!("fixtures/claude-2.1.294-transcript-bash-output-wall-phrase.jsonl")
        }
        "shell-mode-wall-phrase" => {
            include_str!("fixtures/claude-2.1.294-transcript-shell-mode-wall-phrase.jsonl")
        }
        "mcp-wall-phrase" => {
            include_str!("fixtures/claude-2.1.294-transcript-mcp-wall-phrase.jsonl")
        }
        "mcp-long-wall-phrase" => {
            include_str!("fixtures/claude-2.1.294-transcript-mcp-long-wall-phrase.jsonl")
        }
        "mcp-output-then-context" => {
            include_str!("fixtures/claude-2.1.294-transcript-mcp-output-then-context.jsonl")
        }
        "session-limit-then-local-commands" => include_str!(
            "fixtures/claude-2.1.294-transcript-session-limit-then-local-commands.jsonl"
        ),
        other => panic!("no transcript {other}"),
    }
}

/// A host whose agent's transcript tail is each of `tails` in turn (the last
/// repeats; `None`: none to read), counting the asks.
#[derive(Debug, Default)]
struct TranscriptHost {
    tails: std::sync::Mutex<VecDeque<Option<String>>>,
    asks: std::sync::atomic::AtomicUsize,
}

impl TranscriptHost {
    fn of(tails: &[Option<&str>]) -> Arc<Self> {
        Arc::new(Self {
            tails: std::sync::Mutex::new(tails.iter().map(|t| t.map(str::to_owned)).collect()),
            ..Self::default()
        })
    }

    fn asks(&self) -> usize {
        self.asks.load(Ordering::SeqCst)
    }
}

impl IdleHost for TranscriptHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn transcript_tail(&self) -> Option<String> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        let mut tails = self.tails.lock().unwrap();
        if tails.len() > 1 {
            tails.pop_front().flatten()
        } else {
            tails.front().cloned().flatten()
        }
    }
}

/// One run of `wall` (after a busy read) under the owner's policy, the
/// continuation switched OFF unless `continuing` (so an ordinary turn end
/// types nothing), the wall's act on short clocks, and the host `host`
/// (none: `None`): the lines, the server's record, the journal's records.
fn run_at(
    tag: &str,
    wall: Vec<String>,
    host: Option<Arc<TranscriptHost>>,
    continuing: bool,
) -> (
    Vec<String>,
    Mock,
    Vec<crate::supervise::journal::JournalRecord>,
) {
    let mut m = Mock::new(true, vec![busy_screen(), wall]);
    m.vanish_after = Some(3);
    m.stall_sleep = Some(Duration::from_millis(5));
    run_mock(tag, m, host, continuing)
}

/// [`run_at`] on the scripted server `m`.
fn run_mock(
    tag: &str,
    mut m: Mock,
    host: Option<Arc<TranscriptHost>>,
    continuing: bool,
) -> (
    Vec<String>,
    Mock,
    Vec<crate::supervise::journal::JournalRecord>,
) {
    let (dir, path) = journal_file(tag);
    // The host's policy, every switch on (`turn_end_loop_tests.rs`'s
    // `hosted`), the continuation as asked.
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        idle_host: host.map(|h| h as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            continue_policy: continuing,
            ..SupervisorConfig::default()
        },
        ..auto(30, None)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_turn_end_timing(TurnEndTiming {
            retry_backoff: vec![Duration::from_millis(50); 3],
            reset_grace: Duration::ZERO,
            reset_jitter: Duration::ZERO,
            short_backoff: Duration::ZERO,
            ..TurnEndTiming::default()
        });
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, m, records)
}

/// The acts a wall gets, as the loop said them (`TYPED`, `CONTINUED`,
/// `RESTARTED`), and the journal's waits and limit episodes — each without
/// its `seq=` (which counts reads).
fn wall_acts(
    lines: &[String],
    records: &[crate::supervise::journal::JournalRecord],
) -> Vec<String> {
    let said = lines
        .iter()
        .filter(|l| {
            l.starts_with("TYPED") || l.starts_with("CONTINUED") || l.starts_with("RESTARTED")
        })
        .cloned();
    let journaled = records
        .iter()
        .filter(|r| r.kind == "limited" || r.kind == "waiting")
        .map(|r| r.line.clone());
    said.chain(journaled)
        .map(|l| {
            l.split(' ')
                .filter(|w| !w.starts_with("seq="))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The journal's `unconfirmed wall` lines.
fn refusals(records: &[crate::supervise::journal::JournalRecord]) -> Vec<String> {
    records
        .iter()
        .filter(|r| r.kind == "skipped" && r.line.contains("rule=wall-confirm@v1"))
        .map(|r| r.line.clone())
        .collect()
}

/// `body` (a transcript's rows) framed as Claude Code 2.1.294 draws a turn's
/// end: a done row, the composer, the footer.
fn ended_on(body: &[&str]) -> Vec<String> {
    let mut r = rows(body);
    r.extend(rows(&["", "✻ Churned for 3s · done 9:59 AM", ""]));
    r.extend(composer(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    ));
    r
}

/// An MCP result of two blocks (verbose output), the second `words` — what
/// the screen's half reads as the wall under the call.
fn mcp_second_block(words: &str) -> Vec<String> {
    ended_on(&[
        "❯ toolstart",
        "",
        "⏺ github - get_issue (MCP)(n: 7)",
        "  ⎿  Issue 7: the parser drops a row",
        &format!("  ⎿  {words}"),
    ])
}

/// THE SPOOFS (H6-HARNESS-1): a wall's words in what a tool printed, where
/// the screen's half reads a wall — an MCP result of several blocks, one
/// whose first block holds or ends on a blank line, one whose long first
/// block scrolled its call off, a Bash output under a PreToolUse hook's row
/// (all measured on 2.1.294), and MCP results whose second block is a 529, a
/// full context, a weekly limit — each under the transcript Claude Code wrote
/// for such a turn: a tool's result, then the model's (empty) reply. NOTHING
/// the wall asks for is done — no `/login`, no `/compact`, no retry, no wait
/// for a reset, no limit episode, no login-gone alarm — and one journal line
/// says the wall was unconfirmed. The screens the screen's half tells from a
/// wall (a shell-mode `!` output, a file's own `⎿` at column 5, a heredoc
/// command's output, the first row under its call) get no line at all. With
/// the continuation on, the turn is an ORDINARY turn end: continued under
/// `continue@v1`. Main typed `/login` on the MCP one; pass 2 of the fix read
/// none of these as walls, and missed the real walls of
/// [`a_wall_the_transcript_confirms_is_acted_on`] with them.
#[test]
fn a_wall_in_a_tools_output_the_transcript_refuses_is_no_wall() {
    let read = |capture: &str| fx::screen(capture);
    let cases: Vec<(&str, Vec<String>, &str, Option<&str>)> = vec![
        (
            "mcp, three blocks",
            read(fx::MCP_MULTI_BLOCK_OUTPUT_MEASURED),
            "mcp-wall-phrase",
            Some("auth"),
        ),
        (
            "mcp, a long block then another",
            read(fx::MCP_LONG_BLOCK_THEN_WALL_PHRASE_MEASURED),
            "mcp-long-wall-phrase",
            Some("auth"),
        ),
        (
            "pre hook",
            read(fx::PRE_HOOK_ABOVE_OUTPUT_MEASURED),
            "bash-output-wall-phrase",
            Some("auth"),
        ),
        (
            "pre hook error",
            read(fx::PRE_HOOK_ERROR_ABOVE_OUTPUT_MEASURED),
            "bash-output-wall-phrase",
            Some("auth"),
        ),
        (
            "mcp, a blank row in the first block",
            read(fx::MCP_BLANK_ROW_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED),
            "mcp-wall-phrase",
            Some("auth"),
        ),
        (
            "mcp, the first block ending on a blank line",
            read(fx::MCP_TRAILING_BLANK_ROWS_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED),
            "mcp-wall-phrase",
            Some("auth"),
        ),
        (
            "mcp, a 529",
            mcp_second_block(
                "API Error: Repeated 529 Overloaded errors. The API is at capacity — this is \
                 usually temporary. Try again in a moment.",
            ),
            "mcp-wall-phrase",
            Some("overloaded"),
        ),
        (
            "mcp, a full context",
            mcp_second_block("Context limit reached · /compact or /clear to continue"),
            "mcp-wall-phrase",
            Some("context"),
        ),
        (
            "mcp, a weekly limit",
            mcp_second_block("You've hit your weekly limit · resets 9am (America/Los_Angeles)"),
            "mcp-wall-phrase",
            Some("usage-weekly"),
        ),
        (
            "shell mode",
            read(fx::SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED),
            "shell-mode-wall-phrase",
            None,
        ),
        (
            "column 5",
            read(fx::GUTTER_PHRASE_IN_OUTPUT_MEASURED),
            "bash-output-wall-phrase",
            None,
        ),
        (
            "heredoc",
            read(fx::MULTILINE_COMMAND_OUTPUT_MEASURED),
            "bash-output-wall-phrase",
            None,
        ),
    ];
    for (name, wall, tail, kind) in cases {
        let host = TranscriptHost::of(&[Some(transcript(tail))]);
        let (lines, m, records) = run_at("wc-spoof", wall.clone(), Some(Arc::clone(&host)), false);
        assert_eq!(
            wall_acts(&lines, &records),
            Vec::<String>::new(),
            "{name}: {lines:#?}"
        );
        assert_eq!(count(&m, "turn"), 0, "{name}: {:#?}", m.requests);
        assert_eq!(count(&m, "send"), 0, "{name}: {:#?}", m.requests);
        assert_eq!(
            count(&m, "meta set attention"),
            0,
            "{name}: {:#?}",
            m.requests
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("EXTEND") || l.starts_with("EVENT limited")),
            "{name}: no limit — {lines:#?}"
        );
        let said = refusals(&records);
        match kind {
            Some(kind) => {
                assert_eq!(said.len(), 1, "{name}: once — {said:#?}");
                assert!(
                    said[0].contains(&format!(
                        "unconfirmed wall: {kind} — the transcript ends on the model's reply"
                    )),
                    "{name}: {said:#?}"
                );
            }
            None => assert!(said.is_empty(), "{name}: {said:#?}"),
        }
        // With the continuation on: the ordinary turn end's.
        if kind.is_some() {
            let host = TranscriptHost::of(&[Some(transcript(tail))]);
            let (lines, _, records) = run_at("wc-spoof-cont", wall, Some(host), true);
            let acts = wall_acts(&lines, &records);
            assert_eq!(acts.len(), 1, "{name}: {acts:#?}");
            assert!(
                acts[0].starts_with("CONTINUED rule=continue@v1 "),
                "{name}: {acts:#?}"
            );
        }
    }
}

/// THE REAL WALLS, each a screen measured on 2.1.294 — the reviews' captures
/// among them (a context wall under a prompt's second paragraph, after an
/// MCP call's output, under a long output whose call scrolled off, under a
/// collapsed group, a Stop hook's summary, a wrapped group row, after a Bash
/// output, one holding a blank row or not) — under the transcript Claude
/// Code wrote for it, its own notice
/// row last: the wall's act is taken (`/compact`), as on main. A lost login
/// gets `/login` and the alarm, a 529 its retry ladder (the wait for its
/// first try), a weekly limit its wait for the reset and its episode;
/// nothing is said unconfirmed.
#[test]
fn a_wall_the_transcript_confirms_is_acted_on() {
    let read = |capture: &str| fx::screen(capture);
    let compact = "TYPED rule=context-compact@v1 /compact";
    let cases: Vec<(&str, Vec<String>, &str, &str)> = vec![
        (
            "a prompt's second paragraph",
            read(fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED),
            "context",
            compact,
        ),
        (
            "after an mcp output",
            read(fx::MCP_OUTPUT_THEN_CONTEXT_WALL_MEASURED),
            "mcp-output-then-context",
            compact,
        ),
        (
            "a long output",
            read(fx::LONG_OUTPUT_CONTEXT_WALL_MEASURED),
            "bash-output-then-context",
            compact,
        ),
        (
            "after a bash output",
            read(fx::BASH_OUTPUT_THEN_CONTEXT_WALL_MEASURED),
            "bash-output-then-context",
            compact,
        ),
        (
            "after a bash output that holds a blank row",
            read(fx::BASH_BLANK_ROW_THEN_CONTEXT_WALL_MEASURED),
            "bash-output-then-context",
            compact,
        ),
        (
            "a collapsed group",
            read(fx::COLLAPSED_GROUP_CONTEXT_WALL_MEASURED),
            "bash-output-then-context",
            compact,
        ),
        (
            "a stop hook",
            read(fx::STOP_HOOK_CONTEXT_WALL_MEASURED),
            "bash-output-then-context",
            compact,
        ),
        (
            "a wrapped group row",
            read(fx::WRAPPED_GROUP_ROW_CONTEXT_WALL),
            "bash-output-then-context",
            compact,
        ),
        (
            "no login",
            ended_on(&["❯ fifth try", "  ⎿  Not logged in · Please run /login"]),
            "not-logged-in",
            "TYPED rule=auth-login@v1 /login",
        ),
        ("a 529", read(fx::END_529), "529", "WAITING until="),
        (
            "a weekly limit",
            ended_on(&[
                "❯ eighth try",
                "  ⎿  You've hit your weekly limit · resets 9am (America/Los_Angeles)",
            ]),
            "weekly-limit",
            "WAITING until=",
        ),
    ];
    for (name, wall, tail, act) in cases {
        let host = TranscriptHost::of(&[Some(transcript(tail))]);
        let (lines, m, records) = run_at("wc-real", wall, Some(Arc::clone(&host)), false);
        let acts = wall_acts(&lines, &records);
        assert!(
            acts.iter().any(|a| a.starts_with(act)),
            "{name}: {acts:#?}\n{lines:#?}"
        );
        assert!(refusals(&records).is_empty(), "{name}: {records:#?}");
        assert!(host.asks() >= 1, "{name}: the transcript was asked");
        if name == "no login" {
            assert!(
                m.requests.iter().any(|r| r
                    .starts_with("meta set attention owner=supervisor claude wall: Not logged in")),
                "{name}: the alarm — {:#?}",
                m.requests
            );
        }
        if name == "a 529" {
            assert!(
                acts.iter().any(|a| a.ends_with(" overloaded retry 1")),
                "{name}: {acts:#?}"
            );
        }
        if name == "a weekly limit" {
            assert!(
                records.iter().any(|r| r.kind == "limited"),
                "{name}: the episode — {records:#?}"
            );
        }
    }
}

/// A WALL THE TRANSCRIPT NAMES OTHERWISE: the screen reads a lost login (an
/// MCP result's second block) while the transcript's newest entry is Claude
/// Code's own 529 notice — the session did end on a wall, but not on the one
/// the screen read: no `/login` and no alarm (the wall said unconfirmed,
/// with the notice it ends on); and a full context read where the
/// transcript ends on a lost login gets no `/compact`.
#[test]
fn a_wall_the_transcript_names_otherwise_is_refused() {
    for (name, wall, tail, kind) in [
        (
            "login over a 529",
            mcp_second_block("Not logged in · Please run /login"),
            "529",
            "auth",
        ),
        (
            "context over a lost login",
            mcp_second_block("Context limit reached · /compact or /clear to continue"),
            "not-logged-in",
            "context",
        ),
    ] {
        let host = TranscriptHost::of(&[Some(transcript(tail))]);
        let (lines, m, records) = run_at("wc-other", wall, Some(host), false);
        assert_eq!(
            wall_acts(&lines, &records),
            Vec::<String>::new(),
            "{name}: {lines:#?}"
        );
        assert_eq!(count(&m, "turn"), 0, "{name}: {:#?}", m.requests);
        let said = refusals(&records);
        assert_eq!(said.len(), 1, "{name}: {said:#?}");
        assert!(
            said[0].contains(&format!(
                "unconfirmed wall: {kind} — the transcript ends on a notice of its own"
            )),
            "{name}: {said:#?}"
        );
    }
}

/// NO TRANSCRIPT (a host with none to read — no record of the process yet,
/// two transcripts of its id, a file it cannot read — and no host at all,
/// `aterm drive watch`): A WALL READ UNDER A TOOL'S CALL IS NOT ACTED ON, and
/// one line says why, once — the MCP spoofs (which main read as walls: its
/// walk did not know the MCP call), the two whose first block holds or ends
/// on a blank line among them (`/login` was typed on each in the lab, the
/// first by the review of pass 5, the second in pass 6 before its walk went
/// on over blank rows above a `⎿` row: the walk stopped at the blank row),
/// and the PreToolUse-hook spoof (which main read as no wall: pass 3 typed
/// `/login` on it under a `CLAUDE_CONFIG_DIR`), and with them THE REAL WALLS
/// UNDER A CALL: the context wall after a Bash output, which main never read
/// either, and the context walls after an MCP output and after a Bash output
/// that holds a blank row, which MAIN COMPACTED — the knowing loss of recall
/// ruled 2026-10-08 (`wall_confirm.rs`, module docs). Any other wall is acted
/// on as the screen reads it, as main did: the context wall under a prompt's
/// second paragraph gets `/compact`, a lost login under the person's prompt
/// `/login`. The heredoc spoof, which main read with no owner behind the
/// command's blank row and typed `/login` on, is no wall now (the first row
/// under its call): nothing done, nothing said.
#[test]
fn with_no_transcript_a_wall_under_a_tools_call_is_not_acted_on() {
    let read = |capture: &str| fx::screen(capture);
    let refused: Vec<(&str, Vec<String>, &str)> = vec![
        ("mcp", read(fx::MCP_MULTI_BLOCK_OUTPUT_MEASURED), "auth"),
        (
            "mcp, a blank row in the first block",
            read(fx::MCP_BLANK_ROW_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED),
            "auth",
        ),
        (
            "mcp, the first block ending on a blank line",
            read(fx::MCP_TRAILING_BLANK_ROWS_IN_FIRST_BLOCK_THEN_LOGIN_PHRASE_MEASURED),
            "auth",
        ),
        ("pre hook", read(fx::PRE_HOOK_ABOVE_OUTPUT_MEASURED), "auth"),
        (
            "a real wall after a bash output",
            read(fx::BASH_OUTPUT_THEN_CONTEXT_WALL_MEASURED),
            "context",
        ),
        (
            "a real wall after an mcp output (main compacted it)",
            read(fx::MCP_OUTPUT_THEN_CONTEXT_WALL_MEASURED),
            "context",
        ),
        (
            "a real wall after a bash output that holds a blank row (main compacted it)",
            read(fx::BASH_BLANK_ROW_THEN_CONTEXT_WALL_MEASURED),
            "context",
        ),
    ];
    let acted: Vec<(&str, Vec<String>, &str)> = vec![
        (
            "a prompt's second paragraph",
            read(fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED),
            "TYPED rule=context-compact@v1 /compact",
        ),
        (
            "no login",
            ended_on(&["❯ fifth try", "  ⎿  Not logged in · Please run /login"]),
            "TYPED rule=auth-login@v1 /login",
        ),
    ];
    // No wall read: the first row under its call.
    let heredoc = read(fx::MULTILINE_COMMAND_OUTPUT_MEASURED);
    for none in [true, false] {
        let host = || (!none).then(|| TranscriptHost::of(&[None]));
        for (name, wall, kind) in &refused {
            let (lines, m, records) = run_at("wc-none", wall.clone(), host(), false);
            assert_eq!(
                wall_acts(&lines, &records),
                Vec::<String>::new(),
                "{name} (no host: {none}): {lines:#?}"
            );
            assert_eq!(count(&m, "turn"), 0, "{name}: {:#?}", m.requests);
            assert_eq!(
                count(&m, "meta set attention"),
                0,
                "{name}: {:#?}",
                m.requests
            );
            let said = refusals(&records);
            assert_eq!(said.len(), 1, "{name}: once — {said:#?}");
            assert!(
                said[0].contains(&format!(
                    "unconfirmed wall: {kind} — read under a tool's call, and no transcript to \
                     confirm it"
                )),
                "{name}: {said:#?}"
            );
        }
        for (name, wall, act) in &acted {
            let (lines, _, records) = run_at("wc-none-acted", wall.clone(), host(), false);
            let acts = wall_acts(&lines, &records);
            assert!(
                acts.first().is_some_and(|a| a.starts_with(act)),
                "{name} (no host: {none}): {acts:#?}"
            );
            assert!(refusals(&records).is_empty(), "{name}: {records:#?}");
        }
        let (lines, m, records) = run_at("wc-none-heredoc", heredoc.clone(), host(), false);
        assert_eq!(
            wall_acts(&lines, &records),
            Vec::<String>::new(),
            "heredoc (no host: {none}): {lines:#?}"
        );
        assert_eq!(count(&m, "turn"), 0, "heredoc: {:#?}", m.requests);
        assert!(refusals(&records).is_empty(), "heredoc: {records:#?}");
    }
}

/// THE LAG: a transcript read before Claude Code wrote the notice the
/// screen already shows — no `turn_duration` closing it, its newest entry
/// still the turn's prompt, or the model's tool call whose result and notice
/// are not written yet (most likely the tail the lab's hosted loop refused a
/// real wall after an MCP call on, before this rule) — is read again before
/// the wall is refused, and the notice that lands meanwhile confirms it:
/// `/compact`. NEGATIVE CONTROL: a transcript that never catches up is read
/// again `LAG_TRIES` times, then the wall is refused; one that catches up
/// only at its fourth read again (a second on) is still in time.
#[test]
fn a_transcript_that_lags_the_screen_is_read_again() {
    use super::super::wall_confirm::LAG_TRIES;
    let cut = |name: &str, upto: &dyn Fn(&str) -> bool| -> String {
        let rows: Vec<&str> = transcript(name).lines().collect();
        let end = rows.iter().position(|r| upto(r)).expect("the cut");
        rows[..end].join("\n")
    };
    let cases = [
        (
            "at the prompt",
            fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED,
            "context",
            cut("context", &|r: &str| {
                r.contains("\"isApiErrorMessage\":true")
            }),
        ),
        (
            "at the model's call",
            fx::MCP_OUTPUT_THEN_CONTEXT_WALL_MEASURED,
            "mcp-output-then-context",
            cut("mcp-output-then-context", &|r: &str| {
                r.contains("\"tool_result\"")
            }),
        ),
    ];
    // Caught up only at the FOURTH read again, a second after the first:
    // still confirmed (the measured lag, ten times over).
    let lagging = &cases[0].3;
    let host = TranscriptHost::of(&[
        Some(lagging),
        Some(lagging),
        Some(lagging),
        Some(lagging),
        Some(transcript("context")),
    ]);
    let (lines, _, records) = run_at(
        "wc-lag-fourth",
        fx::screen(fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED),
        Some(Arc::clone(&host)),
        false,
    );
    let acts = wall_acts(&lines, &records);
    assert!(
        acts.first()
            .is_some_and(|a| a.starts_with("TYPED rule=context-compact@v1 /compact")),
        "the fourth read again: {acts:#?}"
    );
    assert!(refusals(&records).is_empty(), "{records:#?}");
    assert!(host.asks() >= 5, "{} asks", host.asks());
    for (name, capture, full, lagging) in cases {
        let wall = fx::screen(capture);
        let host = TranscriptHost::of(&[Some(&lagging), Some(transcript(full))]);
        let (lines, _, records) = run_at("wc-lag", wall.clone(), Some(Arc::clone(&host)), false);
        let acts = wall_acts(&lines, &records);
        assert!(
            acts.first()
                .is_some_and(|a| a.starts_with("TYPED rule=context-compact@v1 /compact")),
            "{name}: {acts:#?}"
        );
        assert!(refusals(&records).is_empty(), "{name}: {records:#?}");
        assert!(host.asks() >= 2, "{name}: {}", host.asks());
        // Never caught up.
        let host = TranscriptHost::of(&[Some(&lagging)]);
        let (lines, _, records) = run_at("wc-lag-never", wall, Some(Arc::clone(&host)), false);
        assert_eq!(wall_acts(&lines, &records), Vec::<String>::new(), "{name}");
        assert_eq!(refusals(&records).len(), 1, "{name}: {records:#?}");
        assert!(
            host.asks() > LAG_TRIES as usize,
            "{name}: {} asks",
            host.asks()
        );
    }
}

/// What a step of the loop SAID and what it only NOTED (the journal alone).
#[derive(Default)]
struct Noted {
    said: Vec<String>,
    noted: Vec<String>,
}

impl Review for Noted {
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

/// `rows` as the whole screen, read at `seq`.
fn at(rows: &[String], seq: u64) -> Screen {
    Screen {
        rows: rows.to_vec(),
        seq,
        ..Screen::default()
    }
}

/// The owner's policy, the answers and the continuation off, under `host`.
fn hosted_by(host: &Arc<TranscriptHost>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            continue_policy: false,
            ..SupervisorConfig::default()
        },
        ..auto(30, None)
    }
}

/// A REFUSED LIMIT DECIDED AGAIN is an idle point there too, and its
/// refusal is said ONCE: a weekly limit's words under the worker's message,
/// which asks for a decision, and the transcript ending on the model's reply
/// (a tool's output's). Decided again twice where a wait ran out
/// (`turn_end_now`), the point's escalation is the POINT's — raised (`meta
/// set attention`), where a limit's would be its episode's and raise nothing
/// here — and the journal holds one `unconfirmed wall` line for the two
/// reads of the same wall.
#[test]
fn a_refused_limit_decided_again_is_an_idle_point_and_said_once() {
    let wall = ended_on(&[
        "⏺ I need your decision on the schema before I go on.",
        "  ⎿  You've hit your weekly limit · resets 9am (America/Los_Angeles)",
    ]);
    let host = TranscriptHost::of(&[Some(transcript("bash-output-wall-phrase"))]);
    let opts = hosted_by(&host);
    let mut m = Mock::new(true, vec![wall]);
    let mut noted = Noted::default();
    {
        let mut s = session(&mut m, None);
        let screen = s.screen().expect("the point");
        let point = s.turn_of(screen);
        assert!(
            matches!(point.phase, Phase::Limited { .. }),
            "the screen reads a limit: {:?}",
            point.phase
        );
        for _ in 0..2 {
            s.turn_end_now(&point, &opts, &[], &mut noted)
                .unwrap_or_else(|e| panic!("{e:?}"));
        }
    }
    assert!(
        count(&m, "meta set attention") >= 1,
        "the point's escalation: {:#?}",
        m.requests
    );
    let said: Vec<&String> = noted
        .noted
        .iter()
        .chain(&noted.said)
        .filter(|l| l.contains("rule=wall-confirm@v1"))
        .collect();
    assert_eq!(said.len(), 1, "once: {said:#?}");
    assert!(
        said[0]
            .contains("unconfirmed wall: usage-weekly — the transcript ends on the model's reply"),
        "{said:#?}"
    );
    assert!(host.asks() >= 2, "each decision asked: {}", host.asks());
}

/// THE SEAM'S OUTPUT at a decision's read ([`Session::confirm_wall`]), the
/// transcript ending on the model's reply (a tool's output's), which
/// confirms no wall: Claude Code's critical-memory banner is never asked
/// and stands; a Codex's wall is never asked and stands; a limit refused
/// on a screen with no composer frame drawn reads `idle` with no evidence
/// (`phase_authoritative` false, as for any idle screen with no frame),
/// and on one with its frame `idle` on that frame. A read for the books
/// ([`Check::AsDecided`]) of the refused screen keeps the refusal without
/// asking again, and a screen no decision read is checked as one.
#[test]
fn the_seam_spares_the_banner_and_a_codex_and_reads_a_refused_limit_on_its_frame() {
    use super::super::wall_confirm::Check;
    let host = TranscriptHost::of(&[Some(transcript("bash-output-wall-phrase"))]);
    let opts = hosted_by(&host);
    let mut m = Mock::new(true, vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let read = |rows: &[String]| aterm_phase::reader::read(Some("claude"), rows, Some(2));
    // The memory banner.
    let banner = fx::screen(fx::MEMORY_BANNER_IDLE);
    let mut reading = read(&banner);
    assert_eq!(
        reading.wall.as_ref().map(|w| w.kind),
        Some(aterm_phase::WallKind::Memory)
    );
    s.confirm_wall(&mut reading, &at(&banner, 1), &opts, Check::Decide);
    assert_eq!(
        reading.wall.as_ref().map(|w| w.kind),
        Some(aterm_phase::WallKind::Memory),
        "the banner stands"
    );
    assert_eq!(host.asks(), 0, "the banner is never asked");
    // A Codex's wall.
    let limited = ended_on(&[
        "⏺ The runs are queued.",
        "  ⎿  You've hit your weekly limit · resets 9am (America/Los_Angeles)",
    ]);
    let mut reading = read(&limited);
    assert!(reading.wall.is_some());
    reading.program = aterm_phase::Program::Codex;
    s.confirm_wall(&mut reading, &at(&limited, 2), &opts, Check::Decide);
    assert!(reading.wall.is_some(), "a Codex's wall stands");
    assert_eq!(host.asks(), 0, "a Codex is never asked");
    // A limit refused, on its composer's frame and with none drawn.
    let frameless: Vec<String> = limited
        .iter()
        .take_while(|r| !r.starts_with('─'))
        .cloned()
        .collect();
    assert!(!aterm_phase::phase::has_composer_frame(&frameless));
    for (seq, rows, framed) in [(3, &limited, true), (4, &frameless, false)] {
        let mut reading = read(rows);
        assert!(
            matches!(reading.phase, Phase::Limited { .. }) && reading.phase_authoritative,
            "{framed}: {:?}",
            reading.phase
        );
        s.confirm_wall(&mut reading, &at(rows, seq), &opts, Check::Decide);
        assert_eq!(reading.wall, None, "{framed}");
        assert_eq!(reading.phase, Phase::Idle, "{framed}");
        assert_eq!(reading.phase_authoritative, framed, "{framed}");
    }
    // The books' read of the refused screen: refused, nothing asked.
    let asked = host.asks();
    let mut reading = read(&frameless);
    s.confirm_wall(&mut reading, &at(&frameless, 4), &opts, Check::AsDecided);
    assert_eq!(reading.wall, None);
    assert_eq!(host.asks(), asked, "the books' read asks nothing");
    // A screen no decision read: checked as a decision's.
    let mut reading = read(&limited);
    s.confirm_wall(&mut reading, &at(&limited, 5), &opts, Check::AsDecided);
    assert_eq!(reading.wall, None);
    assert!(host.asks() > asked, "asked");
}

/// DECIDED FROM THE WHOLE SCREEN (the review of pass 4, measured on
/// 2.1.294): an MCP result's second block, a weekly limit's words, on a
/// 50-row pane whose inline renderer drew the call's row at row 9 and left
/// the 31 rows under the footer blank. The loop reads the screen's tail
/// (`tail=40`: rows 10-49), which holds the limit and not the call, and the
/// owner walk, reaching the tail's top, read it under no owner: with no
/// transcript the limit was acted on (`LIMITED … handled`, a wait for the
/// reset). Now the whole grid is read for that wall (`text --json`, once,
/// after the tail): under the call — refused with no transcript (no host,
/// and a host with none), nothing waited for, one line saying why. CONTROL:
/// the same screen with the worker's message where the call's row was: no
/// call over the wall on the whole screen either, and the limit is waited
/// out.
#[test]
fn a_wall_whose_call_is_above_the_rows_read_is_judged_on_the_whole_screen() {
    let screen = fx::screen(fx::MCP_LIMIT_PHRASE_CALL_ROW_ABOVE_TAIL_MEASURED);
    let call = screen
        .iter()
        .position(|r| r.starts_with("⏺ github - get_limit (MCP)"))
        .expect("the call's row");
    assert!(call < screen.len() - 40, "the call is above the tail");
    let mut worker = screen.clone();
    worker[call] = "⏺ The tracker's issue 7, as it stands:".to_string();
    let mock = |rows: &[String]| {
        let mut m = Mock::new(true, vec![busy_screen(), rows.to_vec()]);
        m.cursor_on_caret = true;
        m.vanish_after = Some(3);
        m.stall_sleep = Some(Duration::from_millis(5));
        m
    };
    for none in [true, false] {
        let host = || (!none).then(|| TranscriptHost::of(&[None]));
        let (lines, m, records) = run_mock("wc-whole", mock(&screen), host(), false);
        assert!(
            m.requests
                .iter()
                .any(|r| r.ends_with("text --json tail=40")),
            "the tail read: {:#?}",
            m.requests
        );
        assert!(
            m.requests.iter().any(|r| r.ends_with("text --json")),
            "the whole grid read: {:#?}",
            m.requests
        );
        assert_eq!(
            wall_acts(&lines, &records),
            Vec::<String>::new(),
            "(no host: {none}) {lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("EVENT limited")),
            "(no host: {none}) no limit: {lines:#?}"
        );
        let said = refusals(&records);
        assert_eq!(said.len(), 1, "(no host: {none}) once — {said:#?}");
        assert!(
            said[0].contains(
                "unconfirmed wall: usage-weekly — read under a tool's call, and no transcript to \
                 confirm it"
            ),
            "(no host: {none}) {said:#?}"
        );
        // The control: no call over it, the whole screen read too.
        let (lines, m, records) = run_mock("wc-whole-control", mock(&worker), host(), false);
        assert!(
            m.requests.iter().any(|r| r.ends_with("text --json")),
            "the whole grid read: {:#?}",
            m.requests
        );
        let acts = wall_acts(&lines, &records);
        assert!(
            acts.iter().any(|a| a.starts_with("WAITING until=")),
            "(no host: {none}) {acts:#?}\n{lines:#?}"
        );
        assert!(
            refusals(&records).is_empty(),
            "(no host: {none}) {records:#?}"
        );
    }
}

/// THE WHOLE READ FAILS CLOSED (the review of pass 5 found both survive
/// every other test): with no transcript, a gutter wall read on a TAIL
/// with no call over it is read again from the whole grid
/// ([`Session::under_call_on_screen`]), and stands only where that read
/// finds the SAME wall and no call over it. CONTROL: the measured screen of
/// [`a_wall_whose_call_is_above_the_rows_read_is_judged_on_the_whole_screen`]
/// with the worker's message where the call's row was — the whole read
/// finds the same weekly limit under no call, and it stands. A whole read
/// that FAILS (`ERR no such session`) counts as under a call: refused. So
/// does a whole screen that no longer reads the same wall, the screen moved
/// on between the two reads: no wall at all, a wall of another kind (a lost
/// login), the same kind in other words (another reset).
#[test]
fn a_whole_read_that_fails_or_reads_another_wall_counts_as_under_a_call() {
    use super::super::wall_confirm::Check;
    let mut whole = fx::screen(fx::MCP_LIMIT_PHRASE_CALL_ROW_ABOVE_TAIL_MEASURED);
    let call = whole
        .iter()
        .position(|r| r.starts_with("⏺ github - get_limit (MCP)"))
        .expect("the call's row");
    whole[call] = "⏺ The tracker's issue 7, as it stands:".to_string();
    let limit = whole
        .iter()
        .position(|r| r.contains("You've hit your weekly limit"))
        .expect("the limit's row");
    let first = whole.len() - 40;
    assert!(
        call < first && limit >= first,
        "the tail cuts the owner off"
    );
    let tail = Screen {
        rows: whole[first..].to_vec(),
        first,
        seq: 3,
        ..Screen::default()
    };
    // No host: no transcript to read.
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            continue_policy: false,
            ..SupervisorConfig::default()
        },
        ..auto(30, None)
    };
    // Whether the tail's wall stands, the whole grid read as `grid`
    // (`None`: the read fails).
    let stands = |grid: Option<Vec<String>>| -> bool {
        let failed = grid.is_none();
        let mut m = Mock::new(true, vec![grid.unwrap_or_else(busy_screen)]);
        m.cursor_on_caret = true;
        if failed {
            m.down = Some((0, err("no such session")));
        }
        let mut reading = aterm_phase::reader::read(Some("claude"), &tail.rows, Some(2));
        let w = reading.wall.as_ref().expect("the tail reads the limit");
        assert!(!w.under_call, "the tail reads it under no owner");
        {
            let mut s = session(&mut m, None);
            s.confirm_wall(&mut reading, &tail, &opts, Check::Decide);
        }
        assert_eq!(
            m.requests,
            vec!["text --json".to_string()],
            "the whole grid read, once"
        );
        reading.wall.is_some()
    };
    let with = |row: &str| {
        let mut r = whole.clone();
        r[limit] = row.to_string();
        r
    };
    assert!(
        stands(Some(whole.clone())),
        "the control: the same wall under no call"
    );
    assert!(!stands(None), "a whole read that fails");
    for (name, grid) in [
        ("no wall", with("  ⎿  Steps to reproduce: run it")),
        (
            "another kind",
            with("  ⎿  Not logged in · Please run /login"),
        ),
        (
            "other words",
            with("  ⎿  You've hit your weekly limit · resets Oct 31 at 7pm (America/Los_Angeles)"),
        ),
    ] {
        assert!(!stands(Some(grid)), "the screen moved on: {name}");
    }
}

/// An MCP spoof of a lost login (its second block), under `prompt`: the
/// screen's half reads the wall under the call, the transcript refuses it.
fn spoof(prompt: &str) -> Vec<String> {
    ended_on(&[
        prompt,
        "",
        "⏺ github - get_issue (MCP)(n: 7)",
        "  ⎿  Issue 7: the parser drops a row",
        "  ⎿  Not logged in · Please run /login",
    ])
}

/// The journal's `unconfirmed wall` lines of a step's `Noted`.
fn noted_refusals(noted: &Noted) -> Vec<&String> {
    noted
        .noted
        .iter()
        .chain(&noted.said)
        .filter(|l| l.contains("rule=wall-confirm@v1"))
        .collect()
}

/// ONE LINE A REFUSED EPISODE (the review of pass 4 saw the second of two
/// identical spoofs go unjournaled in the lab): the same refused wall,
/// decided and decided again where its wait ran out, is one line; then an
/// idle point with no wall is DECIDED — and taken by the host's own step,
/// which says nothing of walls (no `turn_end_execute`) — and the refused
/// wall is gone: the same spoof met again is said again.
#[test]
fn a_refused_wall_is_said_again_once_it_went_away() {
    let host = TranscriptHost::of(&[Some(transcript("mcp-wall-phrase"))]);
    let opts = hosted_by(&host);
    let idle = ended_on(&["❯ /clear", "  ⎿  (no content)"]);
    let mut m = Mock::new(
        true,
        vec![
            spoof("❯ first try"),
            spoof("❯ first try"),
            idle,
            spoof("❯ second try"),
        ],
    );
    let mut noted = Noted::default();
    {
        let mut s = session(&mut m, None);
        let screen = s.screen().expect("the spoof");
        let point = s.turn_of(screen);
        let action = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.turn_end_execute(&point, action, &opts, &[], &mut noted)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.turn_end_now(&point, &opts, &[], &mut noted)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(noted_refusals(&noted).len(), 1, "decided again: one line");
        // The idle point: decided, then the host's step takes it.
        let screen = s.screen().expect("the idle point");
        let idle = s.turn_of(screen);
        assert_eq!(idle.phase, Phase::Idle);
        s.turn_end_at_point(&idle, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        // The same spoof again.
        let screen = s.screen().expect("the spoof again");
        let again = s.turn_of(screen);
        let action = s
            .turn_end_at_point(&again, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.turn_end_execute(&again, action, &opts, &[], &mut noted)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
    let said = noted_refusals(&noted);
    assert_eq!(said.len(), 2, "said again: {said:#?}");
    assert!(
        said.iter().all(
            |l| l.contains("unconfirmed wall: auth — the transcript ends on the model's reply")
        ),
        "{said:#?}"
    );
}

/// …and THE AGENT'S LIVE TURN ends the episode too: a spoof refused at a
/// turn's end, the worker working again (a busy read — no point, no
/// decision between), and the same spoof at the next turn's end: two lines.
#[test]
fn a_refused_wall_met_again_after_a_live_turn_is_said_again() {
    let host = TranscriptHost::of(&[Some(transcript("mcp-wall-phrase"))]);
    let mut m = Mock::new(
        true,
        vec![
            busy_screen(),
            spoof("❯ first try"),
            busy_screen(),
            spoof("❯ second try"),
        ],
    );
    m.vanish_after = Some(3);
    m.stall_sleep = Some(Duration::from_millis(5));
    let (lines, m, records) = run_mock("wc-live-turn", m, Some(host), false);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    let said = refusals(&records);
    assert_eq!(said.len(), 2, "{said:#?}\n{lines:#?}");
}

/// DECISION READS ALWAYS ASK AGAIN, the screen unmoved (its `seq` the
/// same): a real full context whose transcript had not caught up through
/// the first decision's reads again (refused, said), decided again where
/// its wait ran out on the SAME screen — the transcript asked again, the
/// notice there now: `/compact`. Kept from the first decision, the wall
/// would stay refused for as long as the screen did not move.
#[test]
fn a_point_decided_again_on_an_unmoved_screen_asks_the_transcript_again() {
    use super::super::wall_confirm::LAG_TRIES;
    let full = transcript("context");
    let rows: Vec<&str> = full.lines().collect();
    let notice = rows
        .iter()
        .position(|r| r.contains("\"isApiErrorMessage\":true"))
        .expect("the notice");
    let lagging = rows[..notice].join("\n");
    let mut tails = vec![Some(lagging.as_str()); 1 + LAG_TRIES as usize];
    tails.push(Some(full));
    let host = TranscriptHost::of(&tails);
    let opts = hosted_by(&host);
    let mut m = Mock::new(
        true,
        vec![fx::screen(fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED)],
    );
    // The screen sits, its seq unmoved, until something is typed.
    m.turn_releases = Some(0);
    let mut noted = Noted::default();
    let seq = {
        let mut s = session(&mut m, None);
        let screen = s.screen().expect("the wall");
        let point = s.turn_of(screen);
        let action = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.turn_end_execute(&point, action, &opts, &[], &mut noted)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(noted_refusals(&noted).len(), 1, "refused first");
        s.turn_end_now(&point, &opts, &[], &mut noted)
            .unwrap_or_else(|e| panic!("{e:?}"));
        point.screen.seq
    };
    assert!(
        m.requests.iter().any(|r| r.contains("/compact")),
        "decided again, confirmed: {:#?}\n{:#?}",
        noted.said,
        m.requests
    );
    assert!(
        noted
            .noted
            .iter()
            .chain(&noted.said)
            .any(|l| l.starts_with(&format!("TYPED seq={seq} rule=context-compact@v1 /compact"))),
        "at the same screen: {:#?} {:#?}",
        noted.said,
        noted.noted
    );
    assert_eq!(
        host.asks(),
        tails.len(),
        "asked again at the second decision"
    );
}

/// THE BOOKS' READ AFTER AN ACT ASKS NOTHING: a full context, its notice the
/// transcript's newest entry — confirmed at the decision — gets `/compact`,
/// and the read that records the act (`TurnEndState::acted`) keeps the
/// decision's verdict: the transcript, which has moved on by then (here to a
/// closed reply that would refuse the wall), is not asked again.
#[test]
fn the_books_read_after_an_act_asks_the_transcript_nothing() {
    let host = TranscriptHost::of(&[
        Some(transcript("context")),
        Some(transcript("bash-output-wall-phrase")),
    ]);
    let opts = hosted_by(&host);
    let mut m = Mock::new(
        true,
        vec![fx::screen(fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED)],
    );
    let mut noted = Noted::default();
    {
        let mut s = session(&mut m, None);
        let screen = s.screen().expect("the wall");
        let point = s.turn_of(screen);
        let action = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(host.asks(), 1, "the decision asked");
        s.turn_end_execute(&point, action, &opts, &[], &mut noted)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
    assert!(
        m.requests.iter().any(|r| r.contains("/compact")),
        "{:#?}",
        m.requests
    );
    assert_eq!(host.asks(), 1, "the books' read asked nothing");
    assert!(noted_refusals(&noted).is_empty(), "{:#?}", noted.noted);
}

/// THE REVIEW OF PASS 4'S CASE, MEASURED (Claude Code 2.1.294 in the lab): a
/// real session limit standing, then a person's `/usage`, `/login`
/// dismissed with Esc and `/model` — the screen and the transcript Claude
/// Code wrote for it. The dismissed `/login` writes `user` rows
/// (`<command-name>/login…`, `<local-command-stdout>Login interrupted`),
/// which pass 4 read as a user turn and so refused the limit (no episode, no
/// wait). Local commands are no turns: the limit is confirmed — its episode
/// opened, handled, and waited out as the vendor goes on by itself — and
/// nothing is said unconfirmed.
#[test]
fn a_limit_still_stands_after_a_persons_local_commands() {
    let host = TranscriptHost::of(&[Some(transcript("session-limit-then-local-commands"))]);
    let (lines, _, records) = run_at(
        "wc-local-commands",
        fx::screen(fx::SESSION_LIMIT_THEN_LOCAL_COMMANDS_MEASURED),
        Some(Arc::clone(&host)),
        false,
    );
    assert!(refusals(&records).is_empty(), "{records:#?}");
    assert!(
        records.iter().any(|r| r.kind == "limited"),
        "the episode: {records:#?}\n{lines:#?}"
    );
    assert!(
        wall_acts(&lines, &records)
            .iter()
            .any(|a| a.starts_with("WAITING until=") && a.contains("goes on by itself")),
        "{lines:#?}\n{records:#?}"
    );
    assert!(host.asks() >= 1);
}
