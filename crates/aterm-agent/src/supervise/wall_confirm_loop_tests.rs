// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The wall confirmed before it is acted on (`wall_confirm.rs`) in the loop, over the scripted
// server: the screens Claude Code 2.1.294 drew (`aterm_phase::prompt::fixtures`) under a host
// that hands the loop the transcript it wrote (`supervise/fixtures/claude-2.1.294-transcript-*`).
// Included from `run_tests.rs` (`mod wall_confirm_loop`), so the `Mock` and helpers are in scope.

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
        "session-limit-cancelled-then-person-turn" => include_str!(
            "fixtures/claude-2.1.299-transcript-session-limit-cancelled-then-person-turn.jsonl"
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

    /// From now on, every ask reads `tail` (`None`: none to read).
    fn reads(&self, tail: Option<&str>) {
        let mut tails = self.tails.lock().unwrap();
        tails.clear();
        tails.push_back(tail.map(str::to_owned));
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

/// One run of `wall` (after a busy read) under the owner's policy, the continuation off unless
/// `continuing`, the wall's act on short clocks, under `host`: the lines, the server's record,
/// the journal's records.
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

/// The acts a wall gets, as the loop said them (`TYPED`, `CONTINUED`, `RESTARTED`), and the
/// journal's waits and limit episodes, each without its `seq=`.
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

/// The spoofs: a wall's words in what a tool printed, where the screen's half reads a wall (MCP
/// results of several blocks, a long first block that scrolled its call off, a Bash output under
/// a PreToolUse hook's row), each under the transcript of such a turn (a tool's result, then the
/// model's empty reply). Nothing the wall asks for is done, and one journal line says it was
/// unconfirmed; screens the screen's half tells from a wall get no line. With the continuation
/// on, the turn is an ordinary turn end, continued under `continue@v1`.
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

/// The real walls, each a screen measured on 2.1.294 under the transcript Claude Code wrote for
/// it, its notice row last, get their act: `/compact` for every context wall (under a prompt's
/// second paragraph, after an MCP or Bash output, a collapsed group, a Stop hook, a wrapped group
/// row), `/login` and the alarm, a 529's retry ladder, a weekly limit's wait and episode; nothing
/// is said unconfirmed.
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

/// A wall the transcript names otherwise is refused: a lost login read where the transcript ends
/// on a 529 notice gets no `/login` and no alarm, and a full context read over a lost login's
/// notice no `/compact`.
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

/// With no transcript (a host with none to read, or no host), a wall read under a tool's call is
/// not acted on and one line says why, once: the MCP spoofs (a blank row in or ending the first
/// block among them), the PreToolUse-hook spoof, and the real context walls after a Bash or MCP
/// output (the accepted loss of recall, `wall_confirm.rs`). Any other wall is acted on as the
/// screen reads it; the heredoc command's output is no wall: nothing done, nothing said.
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

/// A transcript read before Claude Code wrote the notice the screen shows (its newest entry the
/// prompt, or the model's call) is read again, and the notice that lands meanwhile confirms the
/// wall: `/compact`. Negative control: one that never catches up is read again `LAG_TRIES` times,
/// then refused; one that catches up at the fourth read again is in time.
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
    // Caught up only at the fourth read again, a second after the first: still confirmed.
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

/// What a step of the loop said, and what it only noted (the journal alone).
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

/// A refused limit decided again is an idle point: its escalation is the point's (`meta set
/// attention`), and the journal holds one `unconfirmed wall` line for both reads.
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

/// The seam at a decision's read ([`Session::confirm_wall`]), the transcript ending on the model's
/// reply: the memory banner and a Codex's wall are never asked and stand; a refused limit reads
/// `idle`, authoritative only on a drawn composer frame. A books' read keeps the refusal without
/// asking; a screen no decision read is checked as one.
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

/// A wall whose call row is above the rows read (an MCP result's weekly-limit words on a 50-row
/// pane, the call at row 9, the tail rows 10-49) is judged on the whole grid, read once after the
/// tail: under the call, refused with no transcript, nothing waited for, one line. Control: the
/// worker's message where the call's row was, and the limit is waited out.
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

/// The whole read fails closed ([`Session::under_call_on_screen`]): a gutter wall read on a tail
/// with no call over it stands only where the whole grid reads the same wall under no call (the
/// control). A whole read that fails, or one that reads no wall, another kind or other words,
/// counts as under a call.
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

/// One line per refused episode: the same refused wall decided again is one line; an idle point
/// decided (and taken by the host's own step) ends it, and the same spoof met again is said again.
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

/// …and the agent's live turn ends the episode too: a spoof refused, a busy read, the same spoof
/// at the next turn's end: two lines.
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

/// A decision's read always asks again, the screen unmoved: a full context refused while its
/// transcript lagged is decided again where its wait ran out on the same screen, the notice there
/// now: `/compact`.
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

/// The books' read after an act asks the transcript nothing: a confirmed full context gets
/// `/compact`, and the read that records the act keeps the decision's verdict, though the
/// transcript has moved on to a reply that would refuse the wall.
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

/// A real session limit, then a person's `/usage`, `/login` dismissed and `/model` (screen and
/// transcript measured on 2.1.294): local commands are no turns, so the limit is confirmed, its
/// episode opened and waited out, and nothing is said unconfirmed.
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

/// `rows` as a point the loop reads: the whole screen at `seq`, the cursor at
/// the composer's column.
fn point_at(s: &mut Session<'_, Mock>, rows: Vec<String>, seq: u64) -> Turn {
    s.turn_of(Screen {
        rows,
        seq,
        cursor_col: 2,
        ..Screen::default()
    })
}

/// A person's turn the transcript holds ends a left limit's wait: after a confirmed five-hour
/// limit's countdown is cancelled, a short turn no read saw busy, on a screen that no longer shows
/// the notice, is ordered by the transcript (its newest entry the model's reply after the notice),
/// so the point is the continue policy's. Negative controls, each the wait for the reset: the
/// transcript still ending on the notice, and no transcript.
#[test]
fn a_persons_turn_the_transcript_holds_ends_a_left_limits_wait() {
    let measured = transcript("session-limit-cancelled-then-person-turn");
    let rows: Vec<&str> = measured.lines().skip(1).collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    let at_cancel = rows[..=cancel].join("\n");
    let armed: Vec<String> = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED)
        .into_iter()
        .map(|r| {
            r.replace("resets 3:15pm (America/Los_Angeles)", "resets in 1h")
                .replace("limit resets 3:15pm", "limit resets in 1h")
        })
        .collect();
    let cancelled = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED);
    // The person's turn, the rows from the limit's prompt to the cancel off
    // the screen.
    let notice_rows = [
        "carry on with the task",
        "You've hit your session limit",
        "/upgrade to increase",
        "Usage limit reached",
        "Brewed for",
        "Automatic continue cancelled",
    ];
    let turned: Vec<String> = fx::screen(fx::WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED)
        .into_iter()
        .filter(|r| !notice_rows.iter().any(|n| r.contains(n)))
        .collect();
    assert_eq!(
        aterm_phase::since_limit(&turned),
        None,
        "the screen cannot order it"
    );
    let decided = |after: Option<&str>| {
        let host = TranscriptHost::of(&[Some(at_cancel.as_str())]);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: SupervisorConfig {
                answer_questions: false,
                ..SupervisorConfig::default()
            },
            ..auto(30, None)
        };
        let mut m = Mock::new(true, vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        let wall = point_at(&mut s, armed.clone(), 101);
        assert!(
            matches!(wall.phase, Phase::Limited { .. }),
            "{:?}",
            wall.phase
        );
        let a = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { .. }),
            "the confirmed wall's wait: {a:?}"
        );
        let left = point_at(&mut s, cancelled.clone(), 102);
        let a = s
            .turn_end_at_point(&left, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-session resets")),
            "the left limit's wait: {a:?}"
        );
        host.reads(after);
        let point = point_at(&mut s, turned.clone(), 103);
        s.turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"))
    };
    let a = decided(Some(measured));
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("short turn")),
        "the continue policy's back-off: {a:?}"
    );
    // Negative controls.
    for (name, after) in [
        ("nothing since the cancel", Some(at_cancel.as_str())),
        ("no transcript", None),
    ] {
        let a = decided(after);
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-session resets")),
            "{name}: {a:?}"
        );
    }
}

/// A limit a cancel left answers the act only by a notice written since it: the transcript's
/// notice row written since the act makes the point `limited` and the limit waited on; the same
/// row written an hour before (a `/resume`d conversation's older limit) is refused, nothing waited
/// on. With no transcript the screen alone decides, and it is waited on.
#[test]
fn a_cancelled_limit_answers_the_act_only_by_a_notice_written_since_it() {
    const NOTICE_AT: i64 = 1_791_998_913;
    let measured = transcript("session-limit-cancelled-then-person-turn");
    let rows: Vec<&str> = measured.lines().skip(1).collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    let at_cancel = rows[..=cancel].join("\n");
    assert_eq!(
        super::super::wall_confirm::transcript_ends_on(&at_cancel).at,
        Some(NOTICE_AT),
        "the notice row's own stamp"
    );
    // The screen at the cancel: the person's later turn not drawn yet.
    let mut cancelled = fx::screen(fx::WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED);
    let asked = cancelled
        .iter()
        .position(|r| r == "❯ what is left to do?")
        .expect("the person's turn");
    let done = cancelled
        .iter()
        .position(|r| r.starts_with("✻ Churned for"))
        .expect("its done row");
    cancelled.drain(asked..=done + 1);
    assert!(
        aterm_phase::left_wall(&cancelled).is_some(),
        "{cancelled:#?}"
    );
    let decided = |clock: i64, tail: Option<&str>| {
        let host = tail.map(|t| TranscriptHost::of(&[Some(t)]));
        let opts = SuperviseOpts {
            idle_host: host.map(|h| h as Arc<dyn IdleHost>),
            policy: SupervisorConfig {
                answer_questions: false,
                ..SupervisorConfig::default()
            },
            ..auto(30, None)
        };
        let mut m = Mock::new(true, vec![cancelled.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(clock, PDT);
        // The act the loop typed at a turn's end.
        let busy = busy_screen();
        let at_act = TurnEndReading::of(
            &aterm_phase::read(Some("claude"), &busy, Some(2)),
            &busy,
            false,
            None,
            None,
            None,
        );
        s.turn_end.acted(
            &TurnEndAction::Type {
                text: "keep going".to_string(),
                rule_id: crate::supervise::policy::turn_end::RULE_CONTINUE,
            },
            &at_act,
            Instant::now(),
        );
        let mut point = point_at(&mut s, cancelled.clone(), 102);
        let a = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.point_as_decided(&mut point);
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { why, .. }
                if why == "the worker to take the continue@v1 act"),
            "the act's own window: {a:?}"
        );
        (point.phase, s.turn_end.waits_on_named_reset())
    };
    let (phase, waits) = decided(NOTICE_AT + 1, Some(at_cancel.as_str()));
    assert!(
        matches!(phase, Phase::Limited { .. }) && waits,
        "the act's answer: {phase:?}"
    );
    // Negative control: an hour older than the act.
    let (phase, waits) = decided(NOTICE_AT + 3600, Some(at_cancel.as_str()));
    assert!(
        phase == Phase::Idle && !waits,
        "an older limit's notice: {phase:?}"
    );
    // No transcript: the screen alone.
    let (phase, waits) = decided(NOTICE_AT + 3600, None);
    assert!(
        matches!(phase, Phase::Limited { .. }) && waits,
        "hostless: {phase:?}"
    );
}

/// `jsonl` with every row's `timestamp` moved `by` seconds (its fraction
/// dropped): the measured rows, as if written at another time.
fn shifted(jsonl: &str, by: i64) -> String {
    const KEY: &str = "\"timestamp\":\"";
    let mut out = String::new();
    let mut rest = jsonl;
    while let Some(i) = rest.find(KEY) {
        let (head, tail) = rest.split_at(i + KEY.len());
        out.push_str(head);
        let end = tail.find('"').expect("a closed stamp");
        let at = crate::harness::upgrade_models::parse_utc(&tail[..end]).expect("a stamp");
        let moved = i64::try_from(at).expect("a small stamp") + by;
        out.push_str(&aterm_types::rfc3339::format_rfc3339(
            u64::try_from(moved).expect("after the epoch"),
        ));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// When the measured 2.1.299 transcript's five-hour notice says it was
/// written (2026-10-14T17:28:33Z).
const MEASURED_NOTICE_AT: i64 = 1_791_998_913;

/// The measured 2.1.299 conversation up to the vendor's cancel (a person's
/// `hi` and its reply, then their `carry on with the task` met by the
/// five-hour limit, its countdown cancelled), its notice written at `at`.
fn limit_cancelled_at(at: i64) -> String {
    let measured = transcript("session-limit-cancelled-then-person-turn");
    let rows: Vec<&str> = measured.lines().skip(1).collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    shifted(&rows[..=cancel].join("\n"), at - MEASURED_NOTICE_AT)
}

/// The measured 2.1.299 screen (`resets 10:38am`) with the rows from the
/// first one that opens with `from` up to the composer's top rule taken
/// out: the screen as it stood before them.
fn measured_screen_before(from: &str) -> Vec<String> {
    let mut rows = fx::screen(fx::WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED);
    let at = rows
        .iter()
        .position(|r| r.starts_with(from))
        .unwrap_or_else(|| panic!("no row {from}"));
    let rule = rows
        .iter()
        .position(|r| r.starts_with("────"))
        .expect("the composer's rule");
    rows.drain(at..rule);
    rows
}

/// The lines, the journal's waits, the `CONTINUED` lines, the journal's refusals and the server's
/// record of one `watch` over `screens`, the host's tail each of `tails` in turn (the last
/// repeats), the continue policy on with short clocks, the first `held` screens sitting until a
/// turn is sent.
fn transcript_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    held: Option<usize>,
    tails: &[&str],
) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>, Mock) {
    let (dir, path) = journal_file(tag);
    let tails: Vec<Option<&str>> = tails.iter().map(|t| Some(*t)).collect();
    let host = TranscriptHost::of(&tails);
    let mut m = Mock::new(true, screens);
    m.turn_releases = held;
    m.vanish_after = Some(40);
    m.stall_sleep = Some(Duration::from_millis(100));
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        idle_host: Some(host as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        },
        ..auto(30, None)
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
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
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
    let refused = refusals(&records);
    (lines, waits, typed, refused, m)
}

/// A person's turn the limit answers at once, cancelled before any read (the next read shows only
/// `⏺ Automatic continue cancelled` under the notice), is read from the transcript: its notice
/// row was written since the loop last looked, so the limit is that turn's answer, its episode
/// opened and its reset waited on, nothing typed. Negative control: the same rows written an
/// hour before (a `/resume`d conversation's older limit) are refused, said once, and the point is
/// the continue policy's.
#[test]
fn a_persons_turn_the_limit_answers_cancelled_before_any_read_is_read_from_the_transcript() {
    let before = measured_screen_before("❯ carry on with the task");
    let cancelled = measured_screen_before("❯ what is left to do?");
    assert!(aterm_phase::left_wall(&before).is_none(), "{before:#?}");
    assert!(
        aterm_phase::left_wall(&cancelled).is_some(),
        "{cancelled:#?}"
    );
    let (lines, waits, typed, refused, m) = transcript_run(
        "wc-personcancel",
        vec![before.clone(), cancelled.clone()],
        None,
        &[&limit_cancelled_at(TEST_NOW + 1)],
    );
    assert!(
        typed.is_empty(),
        "nothing typed into the limit: {lines:#?}\n{waits:#?}\n{refused:#?}"
    );
    assert_eq!(m.turns, 0, "{:#?}", m.requests);
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
    assert!(refused.is_empty(), "{refused:#?}");
    // Negative control: an hour older than the loop's look.
    let (lines, waits, typed, refused, _) = transcript_run(
        "wc-personcancel-old",
        vec![before, cancelled],
        None,
        &[&limit_cancelled_at(TEST_NOW - 3600)],
    );
    assert!(
        typed
            .first()
            .is_some_and(|t| t.ends_with("rule=continue@v1 keep going")),
        "the point is the continue policy's: {lines:#?}\n{waits:#?}"
    );
    assert_eq!(refused.len(), 1, "said once: {refused:#?}");
    assert!(
        refused[0].contains("written before the turn it would answer"),
        "{refused:#?}"
    );
}

/// An act typed over a cancelled limit, answered by the same reset and cancelled before any read,
/// is read from the transcript: the screen cannot tell the new notice from the one the act was
/// typed over, but the transcript's newest notice row was written since the act. Read as the
/// act's answer: the limit waited on to 10:38 and its grace, nothing typed again. (Claude Code
/// 2.1.300 draws no new countdown when the same reset answers a process that cancelled one, but
/// a `claude --resume`d process does.) Negative control: the transcript's newest notice is the
/// older one: no answer, refused without a word, and the continue policy goes on.
#[test]
fn an_act_over_a_cancelled_limit_answered_by_the_same_reset_is_read_from_the_transcript() {
    let first = measured_screen_before("❯ what is left to do?");
    let mut answered = first.clone();
    let rule = answered
        .iter()
        .position(|r| r.starts_with("────"))
        .expect("the composer's rule");
    let act: Vec<String> = [
        "❯ keep going",
        "  ⎿  You've hit your session limit · resets 10:38am (America/Los_Angeles)",
        "     /upgrade to increase your usage limit.",
        "",
        "⏺ Usage limit reached · continuing automatically at 10:38am · esc to cancel",
        "",
        "✻ Brewed for 0s · done 8:55 AM",
        "",
        "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
        "",
    ]
    .iter()
    .map(|r| (*r).to_string())
    .collect();
    answered.splice(rule..rule, act);
    assert!(aterm_phase::left_wall(&answered).is_some(), "{answered:#?}");
    let older = limit_cancelled_at(TEST_NOW - 600);
    // The act's own rows: its prompt, the notice, the cancel — written since.
    let acts = {
        let ours = another_conversation(&limit_cancelled_at(TEST_NOW + 1));
        let rows: Vec<&str> = ours.lines().collect();
        let prompt = rows
            .iter()
            .position(|r| r.contains("carry on with the task"))
            .expect("the prompt");
        rows[prompt..]
            .join("\n")
            .replace("carry on with the task", "keep going")
    };
    let (lines, waits, typed, refused, m) = transcript_run(
        "wc-samereset",
        vec![first.clone(), answered.clone()],
        Some(0),
        &[&older, &format!("{older}\n{acts}")],
    );
    assert!(
        typed.len() == 1 && typed[0].ends_with(" rule=continue@v1 keep going"),
        "the act the limit answered, and nothing typed into it: {lines:#?}\n{waits:#?}"
    );
    assert_eq!(m.turns, 1, "{:#?}", m.requests);
    assert!(
        waits
            .last()
            .is_some_and(|w| w.starts_with("until=2026-09-17T17:38:0")
                && w.ends_with("usage-session resets")),
        "the reset the notice named and its grace: {waits:#?}\n{lines:#?}"
    );
    assert!(refused.is_empty(), "{refused:#?}");
    // Negative control: the newest notice is the one the act was typed over.
    let (lines, waits, typed, refused, _) = transcript_run(
        "wc-samereset-old",
        vec![first, answered],
        Some(0),
        &[&older],
    );
    assert!(
        typed.len() >= 2,
        "the continue policy goes on: {lines:#?}\n{waits:#?}"
    );
    assert!(refused.is_empty(), "no wall the screen read: {refused:#?}");
}

/// `jsonl` as another conversation wrote it: every id in it replaced consistently, so its rows
/// chain as before.
fn another_conversation(jsonl: &str) -> String {
    let is_id = |s: &str| {
        s.len() == 36
            && s.char_indices().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            })
    };
    let ids: std::collections::BTreeSet<&str> = jsonl
        .match_indices('"')
        .filter_map(|(i, _)| jsonl.get(i + 1..i + 37))
        .filter(|s| is_id(s))
        .collect();
    let mut out = jsonl.to_string();
    for id in ids {
        let first = id.chars().next().expect("an id");
        let other = if first == 'e' { 'f' } else { 'e' };
        out = out.replace(id, &format!("{other}{}", &id[1..]));
    }
    out
}

/// A `<task-notification>` row Claude Code writes itself when a background task ends (its shape
/// as `harness/upgrade_login_wall_tests.rs` keeps it), chained under `parent`.
fn task_notification(parent: &str) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","isSidechain":false,"type":"user","message":{{"role":"user","content":"<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n</task-notification>"}},"uuid":"00000000-0000-4000-8000-00000000000a","origin":{{"kind":"task-notification"}},"promptSource":"system","timestamp":"2026-10-14T17:29:00.000Z"}}"#
    )
}

/// A left limit's wait ends only on its own notice row followed by a reply the API served. With a
/// five-hour limit confirmed and its countdown cancelled, the wait stands through:
/// * a `/resume`d other conversation (its own older limit and a reply under it) whose rows the
///   screen shows with no limit's notice;
/// * this conversation's tail ending on a `<task-notification>` row, no turn served;
/// * the screen still drawing this limit's notice with nothing after it (the transcript is not
///   asked there).
#[test]
fn a_left_limits_wait_ends_on_its_own_rows_alone() {
    let measured = transcript("session-limit-cancelled-then-person-turn");
    // This session's conversation: the same limit and turn, its own ids.
    let ours = another_conversation(measured);
    let rows: Vec<&str> = ours.lines().skip(1).collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    let at_cancel = rows[..=cancel].join("\n");
    let cancel_id = {
        let row = rows[cancel];
        let from = row.find("\"uuid\":\"").expect("its uuid") + "\"uuid\":\"".len();
        row[from..from + 36].to_string()
    };
    let armed: Vec<String> = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED)
        .into_iter()
        .map(|r| {
            r.replace("resets 3:15pm (America/Los_Angeles)", "resets in 1h")
                .replace("limit resets 3:15pm", "limit resets in 1h")
        })
        .collect();
    let cancelled = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_CANCELLED_MEASURED);
    let notice_rows = [
        "carry on with the task",
        "You've hit your session limit",
        "/upgrade to increase",
        "Usage limit reached",
        "Brewed for",
        "Automatic continue cancelled",
    ];
    let unordered: Vec<String> = fx::screen(fx::WALL_SESSION_CANCELLED_THEN_PERSON_TURN_MEASURED)
        .into_iter()
        .filter(|r| !notice_rows.iter().any(|n| r.contains(n)))
        .collect();
    let decided = |point: &[String], after: &str| {
        let host = TranscriptHost::of(&[Some(at_cancel.as_str())]);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: SupervisorConfig {
                answer_questions: false,
                ..SupervisorConfig::default()
            },
            ..auto(30, None)
        };
        let mut m = Mock::new(true, vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        let wall = point_at(&mut s, armed.clone(), 101);
        let a = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { .. }),
            "the confirmed wall's wait: {a:?}"
        );
        let left = point_at(&mut s, cancelled.clone(), 102);
        let a = s
            .turn_end_at_point(&left, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-session resets")),
            "the left limit's wait: {a:?}"
        );
        host.reads(Some(after));
        let p = point_at(&mut s, point.to_vec(), 103);
        s.turn_end_at_point(&p, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"))
    };
    let notified = format!("{at_cancel}\n{}", task_notification(&cancel_id));
    let ended: Vec<String> = [
        ("another conversation resumed", &unordered, measured),
        ("a task notification", &unordered, notified.as_str()),
        ("the notice still the screen's last", &cancelled, ours.as_str()),
    ]
    .into_iter()
    .filter_map(|(name, point, after)| {
        let a = decided(point, after);
        (!matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-session resets")))
            .then(|| format!("{name}: {a:?}"))
    })
    .collect();
    assert!(ended.is_empty(), "the wait ended: {ended:#?}");
}
