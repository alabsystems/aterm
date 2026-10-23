// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_phase::prompt::fixtures as fx;

fn transcript(name: &str) -> &'static str {
    match name {
        "529" => include_str!("fixtures/claude-2.1.294-transcript-529.jsonl"),
        "context" => include_str!("fixtures/claude-2.1.294-transcript-context.jsonl"),
        "not-logged-in" => include_str!("fixtures/claude-2.1.294-transcript-not-logged-in.jsonl"),
        "weekly-limit" => include_str!("fixtures/claude-2.1.294-transcript-weekly-limit.jsonl"),
        "session-limit" => include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl"),
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
        "post-hook-blank-reason-then-context" => include_str!(
            "fixtures/claude-2.1.304-transcript-post-hook-blank-reason-then-context.jsonl"
        ),
        "post-hook-reason-context-phrase" => include_str!(
            "fixtures/claude-2.1.304-transcript-post-hook-reason-context-phrase-empty-reply.jsonl"
        ),
        "verbose-long-output-then-context" => include_str!(
            "fixtures/claude-2.1.304-transcript-verbose-long-output-then-context.jsonl"
        ),
        "stop-hook-feedback-then-context" => {
            include_str!("fixtures/claude-2.1.304-transcript-stop-hook-feedback-then-context.jsonl")
        }
        "bucket-cancelled-then-model" => {
            include_str!("fixtures/claude-2.1.302-transcript-bucket-cancelled-then-model.jsonl")
        }
        other => panic!("no transcript {other}"),
    }
}

#[derive(Debug, Default)]
struct TranscriptHost {
    tails: std::sync::Mutex<VecDeque<Option<String>>>,
    asks: std::sync::atomic::AtomicUsize,
    restartable: std::sync::atomic::AtomicBool,
    wanting: std::sync::atomic::AtomicBool,
    steps: std::sync::atomic::AtomicUsize,
    withheld: std::sync::Mutex<Vec<crate::supervise::Guard>>,
    again: std::sync::atomic::AtomicUsize,
    own_again: std::sync::atomic::AtomicBool,
    once: std::sync::atomic::AtomicBool,
    started: std::sync::Mutex<Option<i64>>,
}

impl TranscriptHost {
    fn of(tails: &[Option<&str>]) -> Arc<Self> {
        Arc::new(Self {
            tails: std::sync::Mutex::new(tails.iter().map(|t| t.map(str::to_owned)).collect()),
            ..Self::default()
        })
    }

    fn asks(&self) -> usize {
        self.asks.load(Ordering::SeqCst) + self.again.load(Ordering::SeqCst)
    }

    fn fresh(&self) -> usize {
        self.asks.load(Ordering::SeqCst)
    }

    fn next_tail(&self) -> Option<String> {
        let mut tails = self.tails.lock().unwrap();
        if tails.len() > 1 {
            tails.pop_front().flatten()
        } else {
            tails.front().cloned().flatten()
        }
    }

    fn reads(&self, tail: Option<&str>) {
        let mut tails = self.tails.lock().unwrap();
        tails.clear();
        tails.push_back(tail.map(str::to_owned));
    }
}

impl IdleHost for TranscriptHost {
    fn wants(&self) -> bool {
        self.wanting.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.steps.fetch_add(1, Ordering::SeqCst);
        if self.once.load(Ordering::SeqCst) {
            self.wanting.store(false, Ordering::SeqCst);
        }
        Some(HostStep {
            line: "upgrade step=carry-on".to_string(),
            moved: true,
            typed: Some(crate::supervise::policy::turn_end::HostTurn::CarryOn),
            repeat: false,
        })
    }
    fn withheld(&self, guard: crate::supervise::Guard) {
        self.withheld.lock().unwrap().push(guard);
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn can_restart(&self) -> bool {
        self.restartable.load(Ordering::SeqCst)
    }
    fn agent_started(&self) -> Option<i64> {
        *self.started.lock().unwrap()
    }
    fn transcript_tail(&self) -> Option<String> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        self.next_tail()
    }
    fn transcript_tail_again(&self) -> Option<String> {
        if !self.own_again.load(Ordering::SeqCst) {
            return self.transcript_tail();
        }
        self.again.fetch_add(1, Ordering::SeqCst);
        self.next_tail()
    }
}

fn run_at<H: IdleHost + 'static>(
    tag: &str,
    wall: Vec<String>,
    host: Option<Arc<H>>,
    continuing: bool,
) -> (
    Vec<String>,
    Mock,
    Vec<crate::supervise::journal::JournalRecord>,
) {
    let mut m = Mock::new(vec![busy_screen(), wall]);
    m.vanish_after = Some(3);
    m.stall_sleep = Some(Duration::from_millis(5));
    run_mock(tag, m, host, continuing)
}

fn run_mock<H: IdleHost + 'static>(
    tag: &str,
    mut m: Mock,
    host: Option<Arc<H>>,
    continuing: bool,
) -> (
    Vec<String>,
    Mock,
    Vec<crate::supervise::journal::JournalRecord>,
) {
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        idle_host: host.map(|h| h as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            continue_policy: continuing,
            ..owners_policy()
        },
        ..auto(30)
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

fn refusals(records: &[crate::supervise::journal::JournalRecord]) -> Vec<String> {
    records
        .iter()
        .filter(|r| r.kind == "skipped" && r.line.contains("rule=wall-confirm@v1"))
        .map(|r| r.line.clone())
        .collect()
}

fn ended_on(body: &[&str]) -> Vec<String> {
    let mut r = rows(body);
    r.extend(rows(&["", "✻ Churned for 3s · done 9:59 AM", ""]));
    r.extend(composer(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    ));
    r
}

fn mcp_second_block(words: &str) -> Vec<String> {
    ended_on(&[
        "❯ toolstart",
        "",
        "⏺ github - get_issue (MCP)(n: 7)",
        "  ⎿  Issue 7: the parser drops a row",
        &format!("  ⎿  {words}"),
    ])
}

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
                !m.requests
                    .iter()
                    .any(|r| r.starts_with("meta set attention")),
                "{name}: no badge — {:#?}",
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

#[test]
fn with_no_transcript_a_programs_words_behind_a_vendor_label_are_not_acted_on() {
    let spoofs = [
        (
            "monitor event, login",
            fx::MONITOR_EVENT_LOGIN_PHRASE_MEASURED,
        ),
        (
            "monitor event, context",
            fx::MONITOR_EVENT_CONTEXT_PHRASE_MEASURED,
        ),
        (
            "stop hook stderr",
            fx::STOP_HOOK_STDERR_LOGIN_PHRASE_MEASURED,
        ),
        (
            "stop hook message",
            fx::STOP_HOOK_MESSAGE_LOGIN_PHRASE_MEASURED,
        ),
        (
            "prompt hook message",
            fx::PROMPT_HOOK_MESSAGE_CONTEXT_PHRASE_MEASURED,
        ),
        (
            "post-tool hook message",
            fx::POST_HOOK_MESSAGE_LOGIN_PHRASE_MEASURED,
        ),
    ];
    let real = fx::screen(fx::MONITOR_EVENT_THEN_CONTEXT_WALL_MEASURED);
    let mut wrong = Vec::new();
    for none in [true, false] {
        let host = || (!none).then(|| TranscriptHost::of(&[None]));
        for (name, capture) in spoofs {
            let (lines, m, records) = run_at("wc-label", fx::screen(capture), host(), false);
            let acts = wall_acts(&lines, &records);
            let said = refusals(&records);
            if !acts.is_empty() || count(&m, "turn") != 0 || !said.is_empty() {
                wrong.push(format!("{name} (no host: {none}): {acts:?} {said:?}"));
            }
        }
        let (lines, _, records) = run_at("wc-label-real", real.clone(), host(), false);
        let acts = wall_acts(&lines, &records);
        assert!(
            acts.first()
                .is_some_and(|a| a.starts_with("TYPED rule=context-compact@v1 /compact")),
            "the real wall under the event (no host: {none}): {acts:#?}"
        );
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn with_no_transcript_a_hooks_words_under_its_own_rows_are_not_acted_on() {
    let spoofs = [
        (
            "stop reason, login",
            fx::STOP_HOOK_STOP_REASON_LOGIN_PHRASE_MEASURED,
        ),
        (
            "stop reason, context",
            fx::STOP_HOOK_STOP_REASON_CONTEXT_PHRASE_MEASURED,
        ),
        (
            "hook words, double space",
            fx::STOP_HOOK_WORDS_DOUBLE_SPACE_LIMIT_PHRASE_MEASURED,
        ),
        (
            "post-tool hook reason",
            fx::POST_HOOK_BLOCKING_REASON_LOGIN_PHRASE_MEASURED,
        ),
        (
            "verbose stop hooks, a labelled prompt row, the stop reason last",
            fx::STOP_HOOKS_VERBOSE_LABELLED_PROMPT_ROW_LAST_MEASURED,
        ),
        (
            "verbose stop hooks, a labelled prompt row",
            fx::STOP_HOOKS_VERBOSE_LABELLED_PROMPT_ROW_MEASURED,
        ),
    ];
    let reals = [
        fx::STOP_HOOK_ERROR_THEN_CONTEXT_WALL_302_MEASURED,
        fx::STOP_HOOK_DECISION_BLOCK_THEN_CONTEXT_WALL_MEASURED,
        fx::STOP_HOOK_FEEDBACK_THEN_CONTEXT_WALL_MEASURED,
    ];
    let mut wrong = Vec::new();
    for none in [true, false] {
        let host = || (!none).then(|| TranscriptHost::of(&[None]));
        for (name, capture) in spoofs {
            let (lines, m, records) = run_at("wc-hook", fx::screen(capture), host(), false);
            let acts = wall_acts(&lines, &records);
            let said = refusals(&records);
            if !acts.is_empty() || count(&m, "turn") != 0 || !said.is_empty() {
                wrong.push(format!("{name} (no host: {none}): {acts:?} {said:?}"));
            }
        }
        for real in reals {
            let (lines, _, records) = run_at("wc-hook-real", fx::screen(real), host(), false);
            let acts = wall_acts(&lines, &records);
            assert!(
                acts.first()
                    .is_some_and(|a| a.starts_with("TYPED rule=context-compact@v1 /compact")),
                "the real wall after the hook's error row (no host: {none}): {acts:#?}"
            );
        }
    }
    let host = TranscriptHost::of(&[Some(transcript("stop-hook-feedback-then-context"))]);
    let (lines, _, records) = run_at(
        "wc-hook-feedback",
        fx::screen(fx::STOP_HOOK_FEEDBACK_THEN_CONTEXT_WALL_MEASURED),
        Some(host),
        false,
    );
    let acts = wall_acts(&lines, &records);
    if !acts
        .first()
        .is_some_and(|a| a.starts_with("TYPED rule=context-compact@v1 /compact"))
    {
        wrong.push(format!(
            "the wall after the feedback row, confirmed: {acts:?}"
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn with_no_transcript_a_status_lines_words_are_not_acted_on() {
    const WORDS: &str = "Not logged in · Run /login";
    let login = "TYPED rule=auth-login@v1 /login";
    let status_line = fx::screen(fx::STATUS_LINE_LOGIN_WORDS_MEASURED);
    let line = status_line
        .iter()
        .position(|r| r.trim() == WORDS)
        .expect("the status line's row");
    let mode = status_line[line + 1].clone();
    assert!(mode.starts_with("  ⏵⏵"), "{mode}");
    let width = status_line[line - 1].chars().count();
    let pad = width - 2 - mode.chars().count();
    let mut right = status_line.clone();
    right.remove(line);
    right[line] = format!("{mode}{WORDS:>pad$}");
    let mut stacked = status_line.clone();
    stacked.swap(line, line + 1);
    let fields = |text: &str| {
        let mut s = status_line.clone();
        s[line] = text.to_string();
        s
    };
    let mut wrong = Vec::new();
    for none in [true, false] {
        let host = || (!none).then(|| TranscriptHost::of(&[None]));
        for (name, screen) in [
            ("status line", status_line.clone()),
            (
                "fields",
                fields("  opus-5.5  ~/proj/aterm  Not logged in · Run /login"),
            ),
            (
                "fields, a limit",
                fields("  ~/Developer/aterm  main  You've hit your session limit · resets 3pm"),
            ),
        ] {
            let (lines, m, records) = run_at("wc-status-line", screen, host(), false);
            let acts = wall_acts(&lines, &records);
            let said = refusals(&records);
            if !acts.is_empty() || count(&m, "turn") != 0 || !said.is_empty() {
                wrong.push(format!("{name} (no host: {none}): {acts:?} {said:?}"));
            }
        }
        let (lines, _, records) = run_at(
            "wc-status-line-limit",
            fx::screen(fx::WALL_SESSION_ARMED_STATUS_LINE_LOGIN_WORDS_MEASURED),
            host(),
            false,
        );
        let acts = wall_acts(&lines, &records);
        if acts.is_empty() || acts.iter().any(|a| a.starts_with(login)) {
            wrong.push(format!("under the banner (no host: {none}): {acts:?}"));
        }
        for (name, screen) in [("right-aligned", &right), ("stacked", &stacked)] {
            let (lines, _, records) = run_at("wc-status-line-real", screen.clone(), host(), false);
            let acts = wall_acts(&lines, &records);
            if !acts.first().is_some_and(|a| a.starts_with(login)) {
                wrong.push(format!("{name}, the vendor's (no host: {none}): {acts:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_wall_where_a_hooks_words_stand_is_found_by_the_transcript() {
    let real = fx::screen(fx::POST_HOOK_BLANK_REASON_THEN_CONTEXT_WALL_MEASURED);
    let spoof = fx::screen(fx::POST_HOOK_REASON_CONTEXT_PHRASE_MEASURED);
    let compact = "TYPED rule=context-compact@v1 /compact";
    let host = TranscriptHost::of(&[Some(transcript("post-hook-blank-reason-then-context"))]);
    let (lines, _, records) = run_at("wc-hook-place-real", real.clone(), Some(host), false);
    let acts = wall_acts(&lines, &records);
    assert!(
        acts.first().is_some_and(|a| a.starts_with(compact)),
        "the real wall: {acts:#?} {lines:#?}"
    );
    let host = TranscriptHost::of(&[Some(transcript("post-hook-reason-context-phrase"))]);
    let (lines, m, records) = run_at("wc-hook-place-spoof", spoof.clone(), Some(host), false);
    assert_eq!(
        wall_acts(&lines, &records),
        Vec::<String>::new(),
        "{lines:#?}"
    );
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    for none in [true, false] {
        let host = || (!none).then(|| TranscriptHost::of(&[None]));
        for (name, screen) in [("real", &real), ("spoof", &spoof)] {
            let (lines, m, records) = run_at("wc-hook-place-none", screen.clone(), host(), false);
            assert_eq!(
                wall_acts(&lines, &records),
                Vec::<String>::new(),
                "{name} (no host: {none}): {lines:#?}"
            );
            assert_eq!(count(&m, "turn"), 0, "{name} (no host: {none})");
        }
    }
}

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

#[derive(Default)]
struct Noted {
    said: Vec<String>,
    noted: Vec<String>,
}

impl Review for Noted {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
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

fn at(rows: &[String], seq: u64) -> Screen {
    Screen {
        rows: rows.to_vec(),
        seq,
        ..Screen::default()
    }
}

fn hosted_by(host: &Arc<TranscriptHost>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            continue_policy: false,
            ..owners_policy()
        },
        ..auto(30)
    }
}

#[test]
fn the_hosts_step_waits_at_the_wall_the_decision_read() {
    const MODEL: &str = "There's an issue with the selected model (claude-opus-9). It may not exist or you may not have access to it. Run /model to pick a different model.";
    let rows = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ There's an issue with the selected model (claude-opus-9). It may not exist or",
        "  you may not have access to it. Run /model to pick a different model.",
    ]);
    let notice = notice_turn(MODEL, "model_not_found", true);
    let reply = notice
        .replace("\"isApiErrorMessage\":true", "\"isApiErrorMessage\":false")
        .replace("\"<synthetic>\"", "\"claude-opus-5-5\"");
    for (name, tail, wall) in [
        ("the transcript's wall", Some(notice.as_str()), true),
        ("no transcript", None, false),
        ("the model's own words", Some(reply.as_str()), false),
    ] {
        let host = TranscriptHost::of(&[tail]);
        let opts = hosted_by(&host);
        let mut m = Mock::new(vec![rows.clone()]);
        m.cursor_on_caret = true;
        let mut heard = Noted::default();
        let (offered, asked) = {
            let mut s = session(&mut m, None);
            let screen = s.screen().expect("the point");
            let mut point = s.turn_of(screen);
            let _ = s.turn_end_reading(&point.screen, None, &opts);
            s.point_as_decided(&mut point);
            let asked = host.asks();
            host.wanting.store(true, Ordering::SeqCst);
            let offered = s
                .host_steps_here(&point, &opts, &mut false, &mut heard)
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            (offered, asked)
        };
        let withheld = host.withheld.lock().unwrap().clone();
        assert_eq!(offered, !wall, "{name}: {withheld:?}");
        assert_eq!(
            host.steps.load(Ordering::SeqCst),
            usize::from(!wall),
            "{name}"
        );
        if wall {
            assert_eq!(withheld, [crate::supervise::Guard::Wall], "{name}");
        } else {
            assert!(withheld.is_empty(), "{name}: {withheld:?}");
        }
        assert_eq!(host.asks(), asked, "{name}: the gate asks nothing");
    }
}

#[test]
fn the_hosts_step_waits_at_the_decided_wall_through_a_repaint() {
    let rows = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ There's an issue with the selected model (claude-opus-9). It may not exist or",
        "  you may not have access to it. Run /model to pick a different model.",
    ]);
    let mut ticked = rows.clone();
    let footer = ticked
        .iter_mut()
        .rev()
        .find(|r| r.contains("auto mode on"))
        .expect("the footer");
    *footer = footer.replace("for agents", "for agents ·");
    const MODEL: &str = "There's an issue with the selected model (claude-opus-9). It may not exist or you may not have access to it. Run /model to pick a different model.";
    let notice = notice_turn(MODEL, "model_not_found", true);
    let reply = notice
        .replace("\"isApiErrorMessage\":true", "\"isApiErrorMessage\":false")
        .replace("\"<synthetic>\"", "\"claude-opus-5-5\"");
    for (name, tail, wall) in [
        ("the transcript's wall", Some(notice.as_str()), true),
        ("no transcript", None, false),
        ("the model's own words", Some(reply.as_str()), false),
    ] {
        let host = TranscriptHost::of(&[tail]);
        host.wanting.store(true, Ordering::SeqCst);
        host.once.store(true, Ordering::SeqCst);
        let mut m = Mock::new(vec![busy_screen(), rows.clone(), ticked.clone()]);
        m.cursor_on_caret = true;
        m.vanish_after = Some(3);
        m.stall_sleep = Some(Duration::from_millis(5));
        let _ = run_mock("repaint-wall", m, Some(Arc::clone(&host)), false);
        let withheld = host.withheld.lock().unwrap().clone();
        let steps = host.steps.load(Ordering::SeqCst);
        if wall {
            assert_eq!(steps, 0, "{name}: {withheld:?}");
            assert!(
                withheld.contains(&crate::supervise::Guard::Wall),
                "{name}: {withheld:?}"
            );
        } else {
            assert_eq!(steps, 1, "{name}: {withheld:?}");
            assert!(
                !withheld.contains(&crate::supervise::Guard::Wall),
                "{name}: {withheld:?}"
            );
        }
    }
}

#[test]
fn a_refused_limit_decided_again_is_an_idle_point_and_said_once() {
    let wall = ended_on(&[
        "⏺ I need your decision on the schema before I go on.",
        "  ⎿  You've hit your weekly limit · resets 9am (America/Los_Angeles)",
    ]);
    let host = TranscriptHost::of(&[Some(transcript("bash-output-wall-phrase"))]);
    let opts = hosted_by(&host);
    let mut m = Mock::new(vec![wall]);
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

#[test]
fn the_seam_spares_the_banner_and_a_codex_and_reads_a_refused_limit_on_its_frame() {
    use super::super::wall_confirm::Check;
    let host = TranscriptHost::of(&[Some(transcript("bash-output-wall-phrase"))]);
    let opts = hosted_by(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    let read = |rows: &[String]| aterm_phase::reader::read(Some("claude"), rows, Some(2));
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
    let asked = host.asks();
    let mut reading = read(&frameless);
    s.confirm_wall(&mut reading, &at(&frameless, 4), &opts, Check::AsDecided);
    assert_eq!(reading.wall, None);
    assert_eq!(host.asks(), asked, "the books' read asks nothing");
    let mut reading = read(&limited);
    s.confirm_wall(&mut reading, &at(&limited, 5), &opts, Check::AsDecided);
    assert_eq!(reading.wall, None);
    assert!(host.asks() > asked, "asked");
}

fn notice_turn(text: &str, error: &str, closed: bool) -> String {
    let rows: Vec<&str> = transcript("529").lines().collect();
    let at = rows
        .iter()
        .rposition(|r| r.contains("\"isApiErrorMessage\":true"))
        .expect("the notice");
    let prompt = rows[..at]
        .iter()
        .rposition(|r| r.contains("\"type\":\"user\""))
        .expect("its prompt");
    let notice = rows[at];
    let from = notice.find("API Error: Repeated 529").expect("the text");
    let to = from + notice[from..].find('"').expect("its end");
    let notice = format!("{}{text}{}", &notice[..from], &notice[to..])
        .replace(
            "\"error\":\"server_error\"",
            &format!("\"error\":\"{error}\""),
        )
        .replace("\"apiErrorStatus\":529,", "");
    let mut turn = rows[prompt..at].to_vec();
    turn.push(&notice);
    if closed {
        turn.extend(
            rows[at + 1..]
                .iter()
                .find(|r| r.contains("\"turn_duration\"")),
        );
    }
    turn.join("\n")
}

#[test]
fn a_closed_notice_the_screen_cannot_name_is_the_decisions_wall() {
    use super::super::wall_confirm::{Check, LAG_TRIES};
    const MODEL: &str = "There's an issue with the selected model (claude-opus-9). It may not exist or you may not have access to it. Run /model to pick a different model.";
    let shown = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ There's an issue with the selected model (claude-opus-9). It may not exist or",
        "  you may not have access to it. Run /model to pick a different model.",
    ]);
    let read = |rows: &[String]| aterm_phase::reader::read(Some("claude"), rows, Some(2));
    assert_eq!(read(&shown).wall, None, "the screen alone names none");
    assert_eq!(read(&shown).phase, Phase::Idle);
    let refused = aterm_phase::WallKind::ApiError {
        code: None,
        retryable: false,
        cause: aterm_phase::ApiCause::Request,
    };
    let host = TranscriptHost::of(&[Some(&notice_turn(MODEL, "model_not_found", true))]);
    let opts = hosted_by(&host);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = virtual_session(&mut m, None);
    let mut reading = read(&shown);
    s.confirm_wall(&mut reading, &at(&shown, 1), &opts, Check::Decide);
    let w = reading.wall.clone().expect("the transcript's wall");
    assert_eq!(w.kind, refused);
    assert_eq!(w.message, MODEL);
    assert_eq!(w.placement, aterm_phase::Placement::ErrorRow);
    assert!(shown[w.row].starts_with("⏺ There's an issue"));
    assert_eq!(reading.phase, Phase::Idle, "an API error reads idle");
    assert_eq!(host.asks(), 1, "read once: the row is shaped like prose");
    let mut again = read(&shown);
    s.confirm_wall(&mut again, &at(&shown, 1), &opts, Check::AsDecided);
    assert_eq!(again.wall.map(|w| w.kind), Some(refused), "the books' read");
    assert_eq!(host.asks(), 1, "the books' read asks nothing");
    let notice = notice_turn(MODEL, "model_not_found", true);
    let reply = notice
        .replace("\"isApiErrorMessage\":true", "\"isApiErrorMessage\":false")
        .replace("\"<synthetic>\"", "\"claude-opus-5-5\"");
    let hidden = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ Hi! I am the local lab stub, not a model. (reply 3)",
    ]);
    for (seq, (name, rows, tail)) in [
        ("the model's own words", &shown, Some(reply)),
        (
            "a turn still open",
            &shown,
            Some(notice_turn(MODEL, "model_not_found", false)),
        ),
        ("a notice the screen does not show", &hidden, Some(notice)),
        ("no transcript", &shown, None),
    ]
    .into_iter()
    .enumerate()
    {
        let host = TranscriptHost::of(&[tail.as_deref()]);
        let opts = hosted_by(&host);
        let mut reading = read(rows);
        s.confirm_wall(
            &mut reading,
            &at(rows, 10 + seq as u64),
            &opts,
            Check::Decide,
        );
        assert_eq!(reading.wall, None, "{name}");
        assert_eq!(reading.phase, Phase::Idle, "{name}");
    }
    let helper = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ Your apiKeyHelper script is failing · This usually means you need to re-authenticate",
    ]);
    assert_eq!(read(&helper).wall, None, "no table names it");
    let lagging = notice_turn(MODEL, "model_not_found", false)
        .lines()
        .take_while(|r| !r.contains("\"isApiErrorMessage\""))
        .collect::<Vec<_>>()
        .join("\n");
    let host = TranscriptHost::of(&[Some(&lagging)]);
    let opts = hosted_by(&host);
    let mut reading = read(&helper);
    s.confirm_wall(&mut reading, &at(&helper, 20), &opts, Check::Decide);
    assert_eq!(reading.wall, None);
    assert!(
        host.asks() > LAG_TRIES as usize,
        "read again for its lag: {}",
        host.asks()
    );
    let aborted = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ API Error: Request was aborted.",
    ]);
    let host = TranscriptHost::of(&[Some(&lagging)]);
    let opts = hosted_by(&host);
    let mut reading = read(&aborted);
    assert_eq!(reading.wall, None);
    s.confirm_wall(&mut reading, &at(&aborted, 21), &opts, Check::Decide);
    assert_eq!(reading.wall, None);
    assert_eq!(host.asks(), 0, "an aborted request is not asked about");
}

fn typed_turn(text: &str, error: &str, api: Option<&str>, closed: bool) -> String {
    let turn = notice_turn(text, error, closed);
    match api {
        Some(kind) => turn.replace(
            "\"isApiErrorMessage\":true",
            &format!("\"isApiErrorMessage\":true,\"apiError\":\"{kind}\""),
        ),
        None => turn,
    }
}

#[test]
fn a_confirmed_unnamed_api_error_is_named_by_its_notice_at_the_decision() {
    use super::super::wall_confirm::Check;
    const DLP: &str = "API Error: Request blocked by your organization's policy.";
    const FALLBACK: &str =
        "API Error (claude-opus-9): model: claude-opus-9. Try /model to switch to Opus 5.5.";
    let server = aterm_phase::WallKind::ApiError {
        code: None,
        retryable: true,
        cause: aterm_phase::ApiCause::Server,
    };
    let refused = aterm_phase::WallKind::ApiError {
        code: None,
        retryable: false,
        cause: aterm_phase::ApiCause::Request,
    };
    let read = |rows: &[String]| aterm_phase::reader::read(Some("claude"), rows, Some(2));
    let shown = |text: &str| ended_on(&["❯ carry on with the task", "", &format!("⏺ {text}")]);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    for (seq, (text, error, api)) in [
        (DLP, "invalid_request", Some("dlp_request_denied")),
        (FALLBACK, "model_not_found", None),
    ]
    .into_iter()
    .enumerate()
    {
        let rows = shown(text);
        let screen = read(&rows).wall.expect("the screen reads a wall");
        assert_eq!(screen.kind, server, "{text}: the screen alone");
        let host = TranscriptHost::of(&[Some(&typed_turn(text, error, api, true))]);
        let opts = hosted_by(&host);
        let seq = 1 + seq as u64;
        let mut reading = read(&rows);
        s.confirm_wall(&mut reading, &at(&rows, seq), &opts, Check::Decide);
        let w = reading.wall.clone().expect("confirmed");
        assert_eq!(w.kind, refused, "{text}");
        assert_eq!(
            (w.row, w.message.as_str(), w.placement),
            (screen.row, screen.message.as_str(), screen.placement),
            "{text}: the screen's own row and words"
        );
        assert_eq!(
            reading.phase,
            Phase::Idle,
            "{text}: an API error reads idle"
        );
        let asked = host.asks();
        let mut again = read(&rows);
        s.confirm_wall(&mut again, &at(&rows, seq), &opts, Check::AsDecided);
        assert_eq!(
            again.wall.map(|w| w.kind),
            Some(refused),
            "{text}: the books' read"
        );
        assert_eq!(host.asks(), asked, "{text}: the books' read asks nothing");
    }
    let rows = shown(DLP);
    let host = TranscriptHost::of(&[Some(&typed_turn(DLP, "unknown", None, true))]);
    let opts = hosted_by(&host);
    let mut reading = read(&rows);
    s.confirm_wall(&mut reading, &at(&rows, 10), &opts, Check::Decide);
    assert_eq!(
        reading.wall.map(|w| w.kind),
        Some(server),
        "an unknown notice"
    );
    let host = TranscriptHost::of(&[None]);
    let opts = hosted_by(&host);
    let mut reading = read(&rows);
    s.confirm_wall(&mut reading, &at(&rows, 11), &opts, Check::Decide);
    assert_eq!(reading.wall.map(|w| w.kind), Some(server), "no transcript");
    let rows = shown("API Error: terminated");
    let older = format!(
        "{}\n{}\n{}",
        typed_turn(DLP, "invalid_request", Some("dlp_request_denied"), true),
        r#"{"parentUuid":"p","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-message>review</command-message>\n<command-name>/review</command-name>"},"uuid":"cmd-1","timestamp":"2026-10-15T10:00:00.000Z"}"#,
        r#"{"parentUuid":"cmd-1","isSidechain":false,"type":"user","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"Review the diff."}]},"uuid":"cmd-2","timestamp":"2026-10-15T10:00:00.001Z"}"#,
    );
    let host = TranscriptHost::of(&[Some(&older)]);
    let opts = hosted_by(&host);
    let mut reading = read(&rows);
    s.confirm_wall(&mut reading, &at(&rows, 12), &opts, Check::Decide);
    assert_eq!(
        reading.wall.map(|w| w.kind),
        Some(server),
        "an older notice than the row shown"
    );
    const FORBIDDEN: &str = "API Error: 403 Forbidden";
    const BLOCKED: &str =
        "API Error: 403 Request blocked by your organization's data loss prevention policy.";
    let forbidden = aterm_phase::WallKind::ApiError {
        code: Some(403),
        retryable: false,
        cause: aterm_phase::ApiCause::Request,
    };
    for (seq, text, error, want) in [
        (
            13,
            FORBIDDEN,
            "authentication_failed",
            aterm_phase::WallKind::Auth,
        ),
        (14, FORBIDDEN, "unknown", forbidden),
        (15, BLOCKED, "authentication_failed", forbidden),
    ] {
        let rows = shown(text);
        assert_eq!(
            read(&rows).wall.map(|w| w.kind),
            Some(forbidden),
            "{text}: the screen alone"
        );
        let host = TranscriptHost::of(&[Some(&typed_turn(text, error, None, true))]);
        let opts = hosted_by(&host);
        let mut reading = read(&rows);
        s.confirm_wall(&mut reading, &at(&rows, seq), &opts, Check::Decide);
        assert_eq!(reading.wall.map(|w| w.kind), Some(want), "{text} {error}");
    }
}

#[test]
fn a_notice_the_screen_shows_is_read_until_its_turn_closes() {
    use super::super::wall_confirm::{Check, LAG_TRIES};
    const MODEL: &str = "There's an issue with the selected model (claude-opus-9). It may not exist or you may not have access to it. Run /model to pick a different model.";
    let shown = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ There's an issue with the selected model (claude-opus-9). It may not exist or",
        "  you may not have access to it. Run /model to pick a different model.",
    ]);
    let read = |rows: &[String]| aterm_phase::reader::read(Some("claude"), rows, Some(2));
    let closed = notice_turn(MODEL, "model_not_found", true);
    let open = notice_turn(MODEL, "model_not_found", false);
    let prompt = closed
        .lines()
        .take_while(|r| !r.contains("\"isApiErrorMessage\""))
        .collect::<Vec<_>>()
        .join("\n");
    let refused = Some(aterm_phase::WallKind::ApiError {
        code: None,
        retryable: false,
        cause: aterm_phase::ApiCause::Request,
    });
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = virtual_session(&mut m, None);
    for (seq, (name, tails, asks)) in [
        (
            "open, then closed",
            vec![Some(open.as_str()), Some(&closed)],
            2,
        ),
        (
            "the prompt, open, then closed",
            vec![Some(prompt.as_str()), Some(&open), Some(&closed)],
            3,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let host = TranscriptHost::of(&tails);
        let opts = hosted_by(&host);
        let mut reading = read(&shown);
        s.confirm_wall(
            &mut reading,
            &at(&shown, 1 + seq as u64),
            &opts,
            Check::Decide,
        );
        assert_eq!(reading.wall.map(|w| w.kind), refused, "{name}");
        assert_eq!(host.asks(), asks, "{name}");
    }
    for own_again in [true, false] {
        let host = TranscriptHost::of(&[Some(prompt.as_str()), Some(&open), Some(&closed)]);
        host.own_again.store(own_again, Ordering::SeqCst);
        let opts = hosted_by(&host);
        let mut reading = read(&shown);
        s.confirm_wall(
            &mut reading,
            &at(&shown, 5 + u64::from(own_again)),
            &opts,
            Check::Decide,
        );
        assert_eq!(reading.wall.map(|w| w.kind), refused, "{own_again}");
        assert_eq!(host.fresh(), if own_again { 1 } else { 3 }, "{own_again}");
        assert_eq!(host.asks(), 3, "{own_again}");
    }
    let host = TranscriptHost::of(&[Some(&open)]);
    let opts = hosted_by(&host);
    let mut reading = read(&shown);
    s.confirm_wall(&mut reading, &at(&shown, 10), &opts, Check::Decide);
    assert_eq!(reading.wall, None, "a notice that stays open");
    assert_eq!(
        host.asks(),
        1 + LAG_TRIES as usize,
        "a notice that stays open"
    );
    let other = notice_turn(
        "Your account is on hold. Visit claude.ai/restricted for details.",
        "account_on_hold",
        false,
    );
    let host = TranscriptHost::of(&[Some(&other)]);
    let opts = hosted_by(&host);
    let mut reading = read(&shown);
    s.confirm_wall(&mut reading, &at(&shown, 11), &opts, Check::Decide);
    assert_eq!(reading.wall, None, "a notice the screen does not show");
    assert_eq!(host.asks(), 1, "a notice the screen does not show");
}

#[test]
fn a_notice_drawn_like_prose_the_transcript_has_not_written_is_read_again_once() {
    use super::super::wall_confirm::Check;
    const MODEL: &str = "There's an issue with the selected model (claude-opus-9). It may not exist or you may not have access to it. Run /model to pick a different model.";
    let shown = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ There's an issue with the selected model (claude-opus-9). It may not exist or",
        "  you may not have access to it. Run /model to pick a different model.",
    ]);
    let read = |rows: &[String]| aterm_phase::reader::read(Some("claude"), rows, Some(2));
    let closed = notice_turn(MODEL, "model_not_found", true);
    let prompt = closed
        .lines()
        .take_while(|r| !r.contains("\"isApiErrorMessage\""))
        .collect::<Vec<_>>()
        .join("\n");
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = virtual_session(&mut m, None);
    let host = TranscriptHost::of(&[Some(&prompt), Some(&closed)]);
    let opts = hosted_by(&host);
    let mut reading = read(&shown);
    s.confirm_wall(&mut reading, &at(&shown, 1), &opts, Check::Decide);
    assert_eq!(
        reading.wall.map(|w| w.kind),
        Some(aterm_phase::WallKind::ApiError {
            code: None,
            retryable: false,
            cause: aterm_phase::ApiCause::Request,
        }),
        "found on the second read"
    );
    assert_eq!(host.asks(), 2);
    let host = TranscriptHost::of(&[Some(&prompt)]);
    let opts = hosted_by(&host);
    let mut reading = read(&shown);
    s.confirm_wall(&mut reading, &at(&shown, 2), &opts, Check::Decide);
    assert_eq!(reading.wall, None, "still on the prompt");
    assert_eq!(host.asks(), 2, "once more, no more");
    let reply = notice_turn(MODEL, "model_not_found", false)
        .replace("\"isApiErrorMessage\":true", "\"isApiErrorMessage\":false")
        .replace("\"<synthetic>\"", "\"claude-opus-5-5\"");
    let host = TranscriptHost::of(&[Some(&reply)]);
    let opts = hosted_by(&host);
    let mut reading = read(&shown);
    s.confirm_wall(&mut reading, &at(&shown, 3), &opts, Check::Decide);
    assert_eq!(reading.wall, None, "the model's reply");
    assert_eq!(host.asks(), 1, "an ordinary turn end is read once");
}

#[test]
fn a_prose_notice_after_a_prompt_command_behind_an_older_notice_is_read_again_once() {
    use super::super::wall_confirm::Check;
    const MODEL: &str = "There's an issue with the selected model (claude-opus-9). It may not exist or you may not have access to it. Run /model to pick a different model.";
    const DLP: &str = "API Error: Request blocked by your organization's policy.";
    let shown = ended_on(&[
        "❯ /review",
        "",
        "⏺ There's an issue with the selected model (claude-opus-9). It may not exist or",
        "  you may not have access to it. Run /model to pick a different model.",
    ]);
    let read = |rows: &[String]| aterm_phase::reader::read(Some("claude"), rows, Some(2));
    let older = format!(
        "{}\n{}\n{}",
        typed_turn(DLP, "invalid_request", Some("dlp_request_denied"), true),
        r#"{"parentUuid":"p","isSidechain":false,"type":"user","message":{"role":"user","content":"<command-message>review</command-message>\n<command-name>/review</command-name>"},"uuid":"cmd-1","timestamp":"2026-10-15T10:00:00.000Z"}"#,
        r#"{"parentUuid":"cmd-1","isSidechain":false,"type":"user","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"Review the diff."}]},"uuid":"cmd-2","timestamp":"2026-10-15T10:00:00.001Z"}"#,
    );
    let written = format!(
        "{older}\n{}",
        notice_turn(MODEL, "model_not_found", true)
            .lines()
            .skip_while(|r| !r.contains("\"isApiErrorMessage\""))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let refused = Some(aterm_phase::WallKind::ApiError {
        code: None,
        retryable: false,
        cause: aterm_phase::ApiCause::Request,
    });
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = virtual_session(&mut m, None);
    let host = TranscriptHost::of(&[Some(&older), Some(&written)]);
    let opts = hosted_by(&host);
    let mut reading = read(&shown);
    s.confirm_wall(&mut reading, &at(&shown, 1), &opts, Check::Decide);
    assert_eq!(
        reading.wall.map(|w| w.kind),
        refused,
        "found on the second read"
    );
    assert_eq!(host.asks(), 2);
    let host = TranscriptHost::of(&[Some(&older)]);
    let opts = hosted_by(&host);
    let mut reading = read(&shown);
    s.confirm_wall(&mut reading, &at(&shown, 2), &opts, Check::Decide);
    assert_eq!(reading.wall, None, "still on the older notice");
    assert_eq!(host.asks(), 2, "once more, no more");
    let same = ended_on(&[
        "❯ carry on with the task",
        "",
        "⏺ There's an issue with the selected model (claude-opus-9). It may not exist or",
        "  you may not have access to it. Run /model to pick a different model.",
    ]);
    let host = TranscriptHost::of(&[Some(&notice_turn(MODEL, "model_not_found", true))]);
    let opts = hosted_by(&host);
    let mut reading = read(&same);
    s.confirm_wall(&mut reading, &at(&same, 3), &opts, Check::Decide);
    assert_eq!(reading.wall.map(|w| w.kind), refused, "the notice shown");
    assert_eq!(host.asks(), 1, "the notice shown is read once");
}

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
        let mut m = Mock::new(vec![busy_screen(), rows.to_vec()]);
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

#[test]
fn a_wall_whose_call_scrolled_off_is_judged_with_the_history_above() {
    let history = |capture: &str| -> Vec<String> { fx::screen(capture) };
    let bash = fx::screen(fx::VERBOSE_LONG_OUTPUT_CONTEXT_WALL_MEASURED);
    let bash_history = history(fx::VERBOSE_LONG_OUTPUT_CONTEXT_WALL_SCROLLBACK_MEASURED);
    assert_eq!(
        bash_history
            .iter()
            .rev()
            .find(|r| r.starts_with('⏺'))
            .map(String::as_str),
        Some("⏺ Bash(sleep 4; seq 1 80)"),
        "the measured history holds the call"
    );
    let mcp = fx::screen(fx::MCP_LONG_BLOCK_THEN_WALL_PHRASE_MEASURED);
    let mut mcp_history: Vec<String> = ["❯ toolstart", "", "⏺ github - get_long (MCP)(n: 7)"]
        .iter()
        .map(|r| r.to_string())
        .collect();
    mcp_history.push("  ⎿  issue line 1".to_string());
    mcp_history.extend((2..20).map(|n| format!("     issue line {n}")));
    assert_eq!(
        mcp[0], "     issue line 20",
        "the history runs straight into the screen"
    );
    let mut worker_history = mcp_history.clone();
    worker_history[2] = "⏺ The tracker's issue 7, as it stands:".to_string();
    let mock = |rows: &[String], above: &[String], alt: bool, gap: bool| {
        let mut m = Mock::new(vec![busy_screen(), rows.to_vec()]);
        m.cursor_on_caret = true;
        m.vanish_after = Some(3);
        m.stall_sleep = Some(Duration::from_millis(5));
        if alt {
            m.scrollback = Some(Vec::new());
            m.alt_archive = Some(above.to_vec());
            m.archive_gap = gap;
        } else {
            m.scrollback = Some(above.to_vec());
        }
        m.alt_screen = Some(alt);
        m
    };
    let under_call = "read under a tool's call, and no transcript to confirm it";
    let mut wrong = Vec::new();
    for none in [true, false] {
        let host = || (!none).then(|| TranscriptHost::of(&[None]));
        for (name, rows, above, alt, evicted, gap) in [
            ("the real wall", &bash, &bash_history, false, 0, false),
            ("the spoof", &mcp, &mcp_history, false, 0, false),
            (
                "the real wall, the alternate screen",
                &bash,
                &bash_history,
                true,
                0,
                false,
            ),
            (
                "the spoof, the alternate screen",
                &mcp,
                &mcp_history,
                true,
                0,
                false,
            ),
            (
                "the spoof, a row evicted",
                &mcp,
                &mcp_history,
                false,
                10,
                false,
            ),
            (
                "the worker's message, a gap in the archive",
                &mcp,
                &worker_history,
                true,
                0,
                true,
            ),
        ] {
            let mut m = mock(rows, above, alt, gap);
            m.evicted_below = evicted;
            let (lines, m, records) = run_mock("wc-scrolled", m, host(), false);
            let acts = wall_acts(&lines, &records);
            let said = refusals(&records);
            if !acts.is_empty() || said.len() != 1 || !said[0].contains(under_call) {
                wrong.push(format!("{name} (no host: {none}): {acts:?} {said:?}"));
            }
            let owner = above
                .iter()
                .rposition(|r| !r.is_empty() && !r.starts_with(' '))
                .expect("an owner");
            let line = |n: usize| format!("line {n}");
            let read = |n: usize| m.requests.iter().any(|r| r.ends_with(&line(n)));
            let past_owner = m
                .requests
                .iter()
                .position(|r| r.ends_with(&line(owner)))
                .and_then(|at| m.requests.get(at + 1))
                .is_some_and(|next| next.ends_with(&line(owner - 1)));
            if !alt && evicted == 0 && (!read(owner) || past_owner) {
                wrong.push(format!("{name}: the history not read up to its owner"));
            }
            let archived = m
                .requests
                .iter()
                .any(|r| r.contains("offscreen since=") && r.ends_with("screen=1"));
            if alt != archived || alt && read(0) {
                wrong.push(format!("{name}: the archive read {archived}, alt {alt}"));
            }
        }
        for (name, alt) in [("the main screen", false), ("the alternate screen", true)] {
            let (lines, _, records) = run_mock(
                "wc-scrolled-worker",
                mock(&mcp, &worker_history, alt, false),
                host(),
                false,
            );
            let acts = wall_acts(&lines, &records);
            if !acts
                .first()
                .is_some_and(|a| a.starts_with("TYPED rule=auth-login@v1 /login"))
            {
                wrong.push(format!(
                    "the worker's message over it, {name} (no host: {none}): {acts:?}"
                ));
            }
        }
    }
    let mut moved = mcp.clone();
    moved[0] = "     issue line 20 (moved)".to_string();
    let mut m = mock(&mcp, &worker_history, false, false);
    m.screens.push(mcp.clone());
    m.screens.push(moved);
    let (lines, _, records) = run_mock("wc-scrolled-moved", m, None::<Arc<TranscriptHost>>, false);
    let acts = wall_acts(&lines, &records);
    let said = refusals(&records);
    if !acts.is_empty() || said.len() != 1 || !said[0].contains(under_call) {
        wrong.push(format!(
            "the screen moved under the reads: {acts:?} {said:?}"
        ));
    }
    let host = TranscriptHost::of(&[Some(transcript("verbose-long-output-then-context"))]);
    let (lines, _, records) = run_mock(
        "wc-scrolled-transcript",
        mock(&bash, &bash_history, false, false),
        Some(host),
        false,
    );
    let acts = wall_acts(&lines, &records);
    if !acts
        .first()
        .is_some_and(|a| a.starts_with("TYPED rule=context-compact@v1 /compact"))
    {
        wrong.push(format!("the real wall, confirmed: {acts:?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_stop_reason_taller_than_the_tail_is_read_with_the_rows_above() {
    let session_limit = Some((aterm_phase::WallKind::UsageSession, Some("3pm".to_string())));
    let phrase = "  Usage limit reached · resets 3pm";
    let pane = |capture: &str| {
        let mut rows = fx::screen(capture);
        assert_eq!(rows.len(), 49, "the capture's rows");
        rows.push(String::new());
        rows
    };
    let tall35 = pane(fx::STOP_HOOK_STOP_REASON_35_LINES_LIMIT_PHRASE_MEASURED);
    let tall60 = pane(fx::STOP_HOOK_STOP_REASON_60_LINES_LIMIT_PHRASE_MEASURED);
    let history = fx::screen(fx::STOP_HOOK_STOP_REASON_60_LINES_SCROLLBACK_MEASURED);
    let summary = history
        .iter()
        .position(|r| r.starts_with("⏺ Ran 1 stop hook"))
        .expect("the summary in the history");
    let mut worker_history = history.clone();
    worker_history[summary] = "⏺ The nightly notes:".to_string();
    let real = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED);
    let read = |rows: &[String], above: &[String], alt: bool, evicted: u64| {
        let mut m = Mock::new(vec![rows.to_vec()]);
        m.cursor_on_caret = true;
        m.alt_screen = Some(alt);
        if alt {
            m.scrollback = Some(Vec::new());
            m.alt_archive = Some(above.to_vec());
        } else {
            m.scrollback = Some(above.to_vec());
        }
        m.evicted_below = evicted;
        let opts = auto(30);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        let screen = s.screen().unwrap_or_else(|e| panic!("{e:?}"));
        let since = s
            .turn_end_reread(&screen, &opts)
            .since_limit
            .map(|l| (l.kind, l.reset));
        drop(s);
        (since, screen, m.requests)
    };
    let mut wrong = Vec::new();
    let (since, screen, requests) = read(&tall35, &[], false, 0);
    let whole = requests
        .iter()
        .filter(|r| r.ends_with("text --json"))
        .count()
        == 1;
    let history_read = requests.iter().any(|r| r.contains(" line "));
    if since.is_some() || !whole || history_read || screen.rows.iter().any(|r| r == phrase) {
        wrong.push(format!("35 lines: {since:?} {requests:?}"));
    }
    for alt in [false, true] {
        let (since, screen, requests) = read(&tall60, &history, alt, 0);
        let line = |n: usize| requests.iter().any(|r| r.ends_with(&format!("line {n}")));
        let archived = requests
            .iter()
            .any(|r| r.contains("offscreen since=") && r.ends_with("screen=1"));
        let up_to_summary = if alt {
            archived && !line(0)
        } else {
            line(summary) && !line(summary - 1)
        };
        if since.is_some() || !up_to_summary || screen.rows.iter().any(|r| r == phrase) {
            wrong.push(format!("60 lines (alt: {alt}): {since:?} {requests:?}"));
        }
    }
    for (name, above, evicted) in [
        ("the worker's message", &worker_history, 0),
        ("a row evicted", &history, history.len() as u64 - 3),
    ] {
        let (since, screen, _) = read(&tall60, above, false, evicted);
        if since != session_limit || !screen.rows.iter().any(|r| r == phrase) {
            wrong.push(format!("{name}: {since:?}, first {}", screen.first));
        }
    }
    let (since, _, requests) = read(&real, &[], false, 0);
    if since.is_none() || requests.iter().any(|r| r.ends_with(" search")) {
        wrong.push(format!("a real notice: {since:?} {requests:?}"));
    }
    let mut m = Mock::new(vec![tall35.clone()]);
    m.cursor_on_caret = true;
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let tail = Screen {
        rows: tall35[10..].to_vec(),
        first: 10,
        seq: 7,
        ..Screen::default()
    };
    let (once, _) = s
        .limit_words_read_above(tail.clone(), None, None)
        .unwrap_or_else(|e| panic!("{e:?}"));
    let (again, _) = s
        .limit_words_read_above(tail, None, None)
        .unwrap_or_else(|e| panic!("{e:?}"));
    drop(s);
    if once.first != again.first || once.first <= 10 || m.requests.len() != 1 {
        wrong.push(format!(
            "read again: first {} then {}, {:?}",
            once.first, again.first, m.requests
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

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
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            answer_questions: false,
            continue_policy: false,
            ..owners_policy()
        },
        ..auto(30)
    };
    let stands = |grid: Option<Vec<String>>| -> bool {
        let failed = grid.is_none();
        let mut m = Mock::new(vec![grid.unwrap_or_else(busy_screen)]);
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

fn spoof(prompt: &str) -> Vec<String> {
    ended_on(&[
        prompt,
        "",
        "⏺ github - get_issue (MCP)(n: 7)",
        "  ⎿  Issue 7: the parser drops a row",
        "  ⎿  Not logged in · Please run /login",
    ])
}

fn noted_refusals(noted: &Noted) -> Vec<&String> {
    noted
        .noted
        .iter()
        .chain(&noted.said)
        .filter(|l| l.contains("rule=wall-confirm@v1"))
        .collect()
}

#[test]
fn a_refused_wall_is_said_again_once_it_went_away() {
    let host = TranscriptHost::of(&[Some(transcript("mcp-wall-phrase"))]);
    let opts = hosted_by(&host);
    let idle = ended_on(&["❯ /clear", "  ⎿  (no content)"]);
    let mut m = Mock::new(vec![
        spoof("❯ first try"),
        spoof("❯ first try"),
        idle,
        spoof("❯ second try"),
    ]);
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
        let screen = s.screen().expect("the idle point");
        let idle = s.turn_of(screen);
        assert_eq!(idle.phase, Phase::Idle);
        s.turn_end_at_point(&idle, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
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

#[test]
fn a_refused_wall_met_again_after_a_live_turn_is_said_again() {
    let host = TranscriptHost::of(&[Some(transcript("mcp-wall-phrase"))]);
    let mut m = Mock::new(vec![
        busy_screen(),
        spoof("❯ first try"),
        busy_screen(),
        spoof("❯ second try"),
    ]);
    m.vanish_after = Some(3);
    m.stall_sleep = Some(Duration::from_millis(5));
    let (lines, m, records) = run_mock("wc-live-turn", m, Some(host), false);
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
    let said = refusals(&records);
    assert_eq!(said.len(), 2, "{said:#?}\n{lines:#?}");
}

#[test]
fn a_lag_re_read_that_reads_nothing_refuses_the_wall() {
    use super::super::wall_confirm::Check;
    let full = transcript("context");
    let rows: Vec<&str> = full.lines().collect();
    let notice = rows
        .iter()
        .position(|r| r.contains("\"isApiErrorMessage\":true"))
        .expect("the notice");
    let lagging = rows[..notice].join("\n");
    let shown = fx::screen(fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = virtual_session(&mut m, None);
    for (seq, (then, stands)) in [(None, false), (Some(full), true)].into_iter().enumerate() {
        let host = TranscriptHost::of(&[Some(lagging.as_str()), then]);
        host.own_again.store(true, Ordering::SeqCst);
        let opts = hosted_by(&host);
        let mut reading = aterm_phase::reader::read(Some("claude"), &shown, Some(2));
        assert!(reading.wall.is_some(), "the screen's wall");
        s.confirm_wall(
            &mut reading,
            &at(&shown, 1 + seq as u64),
            &opts,
            Check::Decide,
        );
        assert_eq!(reading.wall.is_some(), stands, "{then:?}");
        assert_eq!(host.fresh(), 1, "one fresh read");
    }
}

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
    let mut m = Mock::new(vec![fx::screen(
        fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED,
    )]);
    m.turn_releases = Some(0);
    let mut noted = Noted::default();
    let seq = {
        let mut s = virtual_session(&mut m, None);
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

#[test]
fn the_books_read_after_an_act_asks_the_transcript_nothing() {
    let host = TranscriptHost::of(&[
        Some(transcript("context")),
        Some(transcript("bash-output-wall-phrase")),
    ]);
    let opts = hosted_by(&host);
    let mut m = Mock::new(vec![fx::screen(
        fx::MULTI_PARAGRAPH_PROMPT_CONTEXT_WALL_MEASURED,
    )]);
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

#[test]
fn the_transcript_names_the_limit_a_generic_footer_does_not() {
    let weekly = transcript("weekly-limit");
    let written = |words: &str, kind: &str| {
        weekly
            .replace(
                "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
                words,
            )
            .replace(
                "\"rateLimitType\":\"seven_day\"",
                &format!("\"rateLimitType\":\"{kind}\""),
            )
    };
    let mut rows = ended_on(&["⏺ Picking the parser back up after the tests."]);
    let footer = rows.len() - 1;
    rows.insert(
        footer,
        "  ⚠ Usage limit reached · limit resets 6pm".to_string(),
    );
    let decide = |tail: String| {
        let host = TranscriptHost::of(&[Some(tail.as_str())]);
        host.restartable.store(true, Ordering::SeqCst);
        let opts = hosted_by(&host);
        let mut m = Mock::new(vec![rows.clone()]);
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW + 8 * 3600 + 10 * 60, PDT);
        let mut point = point_at(&mut s, rows.clone(), 7);
        assert!(
            matches!(&point.phase, Phase::Limited { message, .. } if message.contains("Usage limit reached")),
            "the screen reads the generic footer: {:?}",
            point.phase
        );
        let action = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let asks = host.asks();
        let books = s.turn_end_reread(&point.screen, &opts);
        assert_eq!(host.asks(), asks, "the books' read asked nothing");
        s.point_as_decided(&mut point);
        (action, books.wall, books.wall_message, point.phase)
    };
    let (action, wall, message, phase) = decide(written(
        "You've hit your Sonnet limit · resets 6pm (America/Los_Angeles)",
        "seven_day_sonnet",
    ));
    assert!(
        matches!(
            &action,
            TurnEndAction::Restart { why: crate::supervise::policy::turn_end::Restart::Model { to }, .. } if to == "opus"
        ),
        "{action:?}"
    );
    assert_eq!(
        wall,
        Some(aterm_phase::WallKind::ModelBucket { consent: false })
    );
    assert_eq!(
        message,
        "You've hit your Sonnet limit · resets 6pm (America/Los_Angeles)"
    );
    assert_eq!(
        phase,
        Phase::Limited {
            message: message.clone(),
            reset: Some("6pm".to_string()),
        }
    );
    let (action, wall, message, _) = decide(written(
        "You've hit your limit · resets 6pm (America/Los_Angeles)",
        "seven_day_sonnet",
    ));
    assert!(
        matches!(
            &action,
            TurnEndAction::Restart { why: crate::supervise::policy::turn_end::Restart::Model { to }, .. } if to == "opus"
        ),
        "{action:?}"
    );
    assert_eq!(
        wall,
        Some(aterm_phase::WallKind::ModelBucket { consent: false })
    );
    assert!(message.ends_with("seven_day_sonnet"), "{message}");
    let (action, wall, message, _) = decide(written(
        "You've hit your limit · resets 6pm (America/Los_Angeles)",
        "seven_day_opus",
    ));
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { .. }),
        "the fallback is the bucket at its limit: {action:?}"
    );
    assert_eq!(
        wall,
        Some(aterm_phase::WallKind::ModelBucket { consent: false })
    );
    assert!(message.ends_with("seven_day_opus"), "{message}");
    let (action, wall, message, phase) = decide(written(
        "You've hit your session limit · resets 6pm (America/Los_Angeles)",
        "five_hour",
    ));
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { .. }),
        "{action:?}"
    );
    assert_eq!(wall, Some(aterm_phase::WallKind::UsageSession));
    assert!(message.contains("Usage limit reached"), "{message}");
    assert!(
        matches!(&phase, Phase::Limited { message, .. } if message.contains("Usage limit reached")),
        "{phase:?}"
    );
}

fn point_at(s: &mut Session<'_, Mock>, rows: Vec<String>, seq: u64) -> Turn {
    s.turn_of(Screen {
        rows,
        seq,
        cursor_col: 2,
        ..Screen::default()
    })
}

fn waits_on_the_reset(s: &Session<'_, Mock>, a: &TurnEndAction) -> bool {
    s.turn_end
        .waited_limit()
        .is_some_and(|w| matches!(a, TurnEndAction::WaitUntil { until, .. } if *until >= w.at))
}

fn backs_off(s: &Session<'_, Mock>, a: &TurnEndAction, before: Instant, after: Instant) -> bool {
    let t = &s.turn_end.timing;
    !s.turn_end.waits_on_named_reset()
        && matches!(a, TurnEndAction::WaitUntil { until, wait: WaitKind::BackOff { .. }, .. }
            if *until >= before + t.short_backoff && *until <= after + t.short_backoff_max)
}

#[test]
fn a_persons_turn_the_transcript_holds_ends_a_left_limits_wait() {
    use crate::supervise::policy::turn_end::LIMIT_CONVERSATION_LOOK;
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
                ..owners_policy()
            },
            ..auto(30)
        };
        let mut m = Mock::new(vec![armed.clone()]);
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
        assert!(waits_on_the_reset(&s, &a), "the left limit's wait: {a:?}");
        let remembered = s.turn_end.waited_limit().expect("the remembered reset");
        let owner = s
            .turn_end
            .limit_conversation()
            .cloned()
            .expect("the limit's owner");
        assert!(matches!(
            &owner,
            crate::supervise::policy::turn_end::Conversation::Named(_)
        ));
        host.reads(after);
        let point = point_at(&mut s, turned.clone(), 103);
        let before = Instant::now();
        let a = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let decided_at = Instant::now();
        if after.is_none() {
            let look = LIMIT_CONVERSATION_LOOK;
            assert!(
                matches!(&a, TurnEndAction::WaitUntil { until, wait: WaitKind::Other, .. }
                    if *until >= remembered.at.min(before + look)
                        && *until <= remembered.at.min(decided_at + look)),
                "an unread identity is rechecked within its bounded look: {a:?}"
            );
            assert_eq!(a.rule_id(), None, "no act on an unread conversation");
            assert_eq!(s.turn_end.waited_limit(), Some(remembered));
            assert_eq!(s.turn_end.limit_conversation(), Some(&owner));
        }
        let (on_reset, backed) = (
            waits_on_the_reset(&s, &a),
            backs_off(&s, &a, before, decided_at),
        );
        (a, on_reset, backed)
    };
    let (a, _, backed) = decided(Some(measured));
    assert!(backed, "the continue policy's back-off: {a:?}");
    let (a, on_reset, _) = decided(Some(at_cancel.as_str()));
    assert!(on_reset, "nothing since the cancel: {a:?}");
    let (a, on_reset, backed) = decided(None);
    assert!(
        !on_reset && !backed,
        "no continuation back-off for an unread identity: {a:?}"
    );
}

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
                ..owners_policy()
            },
            ..auto(30)
        };
        let mut m = Mock::new(vec![cancelled.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(clock, PDT);
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
        let before = Instant::now();
        let a = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let after = Instant::now();
        s.point_as_decided(&mut point);
        let take = s.turn_end.timing.take_within;
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { until, .. }
                if *until >= before + take && *until <= after + take),
            "the act's own window: {a:?}"
        );
        (point.phase, s.turn_end.waits_on_named_reset())
    };
    let (phase, waits) = decided(NOTICE_AT + 1, Some(at_cancel.as_str()));
    assert!(
        matches!(phase, Phase::Limited { .. }) && waits,
        "the act's answer: {phase:?}"
    );
    let (phase, waits) = decided(NOTICE_AT + 3600, Some(at_cancel.as_str()));
    assert!(
        phase == Phase::Idle && !waits,
        "an older limit's notice: {phase:?}"
    );
    let (phase, waits) = decided(NOTICE_AT + 3600, None);
    assert!(
        matches!(phase, Phase::Limited { .. }) && waits,
        "hostless: {phase:?}"
    );
}

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

const MEASURED_NOTICE_AT: i64 = 1_791_998_913;

fn limit_cancelled_at(at: i64) -> String {
    let measured = transcript("session-limit-cancelled-then-person-turn");
    let rows: Vec<&str> = measured.lines().skip(1).collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    shifted(&rows[..=cancel].join("\n"), at - MEASURED_NOTICE_AT)
}

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

fn transcript_run(
    tag: &str,
    screens: Vec<Vec<String>>,
    held: Option<usize>,
    tails: &[&str],
) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>, Mock) {
    let (dir, path) = journal_file(tag);
    let tails: Vec<Option<&str>> = tails.iter().map(|t| Some(*t)).collect();
    let host = TranscriptHost::of(&tails);
    let mut m = Mock::new(screens);
    m.turn_releases = held;
    m.vanish_after = Some(40);
    m.stall_sleep = Some(Duration::from_millis(100));
    m.end_when_parked = true;
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        idle_host: Some(host as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            ..owners_policy()
        },
        ..auto(30)
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
        &[&older, &older, &format!("{older}\n{acts}")],
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

fn task_notification(parent: &str) -> String {
    format!(
        r#"{{"parentUuid":"{parent}","isSidechain":false,"type":"user","message":{{"role":"user","content":"<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n</task-notification>"}},"uuid":"00000000-0000-4000-8000-00000000000a","origin":{{"kind":"task-notification"}},"promptSource":"system","timestamp":"2026-10-14T17:29:00.000Z"}}"#
    )
}

#[test]
fn a_left_limits_wait_ends_on_its_own_rows_alone() {
    let measured = transcript("session-limit-cancelled-then-person-turn");
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
                ..owners_policy()
            },
            ..auto(30)
        };
        let mut m = Mock::new(vec![armed.clone()]);
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
        assert!(waits_on_the_reset(&s, &a), "the left limit's wait: {a:?}");
        host.reads(Some(after));
        let p = point_at(&mut s, point.to_vec(), 103);
        let a = s
            .turn_end_at_point(&p, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let on_reset = waits_on_the_reset(&s, &a);
        (a, on_reset)
    };
    let notified = format!("{at_cancel}\n{}", task_notification(&cancel_id));
    let ended: Vec<String> = [
        ("another conversation resumed", &unordered, measured),
        ("a task notification", &unordered, notified.as_str()),
        (
            "the notice still the screen's last",
            &cancelled,
            ours.as_str(),
        ),
    ]
    .into_iter()
    .filter_map(|(name, point, after)| {
        let (a, on_reset) = decided(point, after);
        (!on_reset).then(|| format!("{name}: {a:?}"))
    })
    .collect();
    assert!(ended.is_empty(), "the wait ended: {ended:#?}");
}

#[test]
fn a_limit_notice_is_another_conversations_where_the_tail_lacks_its_row() {
    let tail = transcript("weekly-limit");
    let row = tail
        .lines()
        .rev()
        .find(|l| l.contains("\"isApiErrorMessage\":true"))
        .and_then(|l| {
            let at = l.find("\"uuid\":\"")? + 8;
            l.get(at..at + 36).map(str::to_owned)
        })
        .expect("the notice's row id");
    let other = another_conversation(tail);
    for (name, tails, kept, want) in [
        ("its own conversation", Some(tail), true, false),
        ("another conversation", Some(other.as_str()), true, true),
        ("no transcript", None, true, false),
        ("no row kept", Some(other.as_str()), false, false),
    ] {
        let host = TranscriptHost::of(&[tails]);
        let opts = hosted_by(&host);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.limit_notice_row = kept.then(|| row.clone());
        assert_eq!(s.notice_elsewhere(&opts), want, "{name}");
    }
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, None);
    s.limit_notice_row = Some(row);
    assert!(!s.notice_elsewhere(&auto(30)), "no host");
}

#[test]
fn a_resumed_conversations_notice_with_this_limits_clock_ends_no_wait_in_the_loop() {
    let now = TEST_NOW + 6 * 3600 + 20 * 60 + 30;
    let at_cancel = limit_cancelled_at(now - 60).replace("10:38am", "3:15pm");
    let other = another_conversation(&at_cancel);
    let armed = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED);
    let history: Vec<String> = fx::screen(fx::WALL_SESSION_RESUMED_BY_ITSELF_MEASURED)
        .into_iter()
        .map(|r| r.replace("3:35pm", "3:15pm"))
        .collect();
    let since = aterm_phase::since_limit(&history).expect("the history's notice");
    assert_eq!(
        (since.reset.as_deref(), since.fired),
        (Some("3:15pm"), true),
        "{since:?}"
    );
    let decided = |after: &str| {
        let host = TranscriptHost::of(&[Some(at_cancel.as_str())]);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: SupervisorConfig {
                answer_questions: false,
                ..owners_policy()
            },
            ..auto(30)
        };
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        let wall = point_at(&mut s, armed.clone(), 101);
        let a = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { .. }),
            "the confirmed wall's wait: {a:?}"
        );
        assert!(s.limit_notice_row.is_some(), "its notice row kept");
        host.reads(Some(after));
        let point = point_at(&mut s, history.clone(), 102);
        let before = Instant::now();
        let a = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let after = Instant::now();
        let (on_reset, backed) = (waits_on_the_reset(&s, &a), backs_off(&s, &a, before, after));
        (a, on_reset, backed)
    };
    let (a, on_reset, _) = decided(&other);
    assert!(on_reset, "this limit's wait stands: {a:?}");
    let (a, _, backed) = decided(&at_cancel);
    assert!(backed, "the continue policy's back-off: {a:?}");
}

#[test]
fn a_cold_restores_first_read_of_a_countdown_its_agent_never_held_is_nobodys() {
    let now = MEASURED_NOTICE_AT + 120;
    let measured = transcript("session-limit-cancelled-then-person-turn");
    let rows: Vec<&str> = measured.lines().skip(1).collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    let at_limit = rows[..cancel].join("\n");
    let armed: Vec<String> = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED)
        .into_iter()
        .map(|r| r.replace("3:15pm", "10:38am"))
        .collect();
    let decided = |started: Option<i64>| {
        let host = TranscriptHost::of(&[Some(at_limit.as_str())]);
        *host.started.lock().unwrap() = started;
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: SupervisorConfig {
                answer_questions: false,
                ..owners_policy()
            },
            ..auto(30)
        };
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        let wall = point_at(&mut s, armed.clone(), 101);
        s.turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts)
            .unwrap_or_else(|e| panic!("{e:?}"))
    };
    let a = decided(Some(MEASURED_NOTICE_AT + 60));
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { why, .. } if why == "usage-session resets"),
        "{a:?}"
    );
    for started in [Some(MEASURED_NOTICE_AT - 60), None] {
        let a = decided(started);
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("goes on by itself")),
            "{started:?}: {a:?}"
        );
    }
}

#[test]
fn a_seeded_loop_reads_the_limits_end_in_the_transcript_by_its_reset() {
    const RESET: i64 = 1_791_999_480;
    const RESETS_AT: &str = "\"resetsAt\":1791999513";
    let now = MEASURED_NOTICE_AT + 30 * 60;
    let measured = transcript("session-limit-cancelled-then-person-turn");
    let rows: Vec<&str> = measured.lines().skip(1).collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    let at_limit = rows[..cancel].join("\n");
    let done = "All done: the change is committed and pushed, and every suite passes.";
    let stub = "Hi! I am the local lab stub, not a model. (reply 3)";
    let continued = rows
        .iter()
        .filter(|r| {
            !r.contains("Automatic continue cancelled") && !r.contains("what is left to do?")
        })
        .map(|r| r.replace(stub, done))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(continued.contains(RESETS_AT) && continued.contains(done));
    let armed: Vec<String> = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED)
        .into_iter()
        .map(|r| r.replace("3:15pm", "10:38am"))
        .collect();
    let wall = aterm_phase::read(Some("claude"), &armed, Some(2))
        .wall
        .expect("the armed wall");
    let replied: Vec<String> = fx::screen(fx::WALL_SESSION_RESUMED_BY_ITSELF_MEASURED)
        .into_iter()
        .filter(|r| {
            ![
                "carry on with the task",
                "You've hit your",
                "/upgrade to",
                "Usage limit reached",
            ]
            .iter()
            .any(|g| r.contains(g))
        })
        .map(|r| r.replace(stub, done))
        .collect();
    assert_eq!(
        aterm_phase::since_limit(&replied),
        None,
        "nothing on the screen ties it"
    );
    let decided = |tail: Option<&str>| {
        let host = TranscriptHost::of(&[tail]);
        let opts = SuperviseOpts {
            idle_host: Some(host as Arc<dyn IdleHost>),
            policy: SupervisorConfig {
                answer_questions: false,
                ..owners_policy()
            },
            ..auto(30)
        };
        let mut m = Mock::new(vec![replied.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        s.seed_wall_memory(Some(crate::supervise::approvals::OpenWall {
            conversation: super::super::wall_confirm::conversation_of(measured),
            program: None,
            at_ms: MEASURED_NOTICE_AT * 1000,
            message: wall.message.clone(),
            reset: wall.reset.clone(),
            reset_unix: Some(RESET),
            tap_unix: None,
            kind: Some(wall.kind),
            judged_ms: None,
            retry_ms: None,
        }));
        assert!(s.turn_end.waits_on_named_reset(), "the wait taken over");
        assert!(s.limit_notice_row.is_none(), "no notice row remembered");
        let point = point_at(&mut s, replied.clone(), 101);
        let before = Instant::now();
        let a = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let after = Instant::now();
        let backed = backs_off(&s, &a, before, after);
        (a, s.turn_end.waits_on_named_reset(), backed)
    };
    let (a, _, backed) = decided(Some(&continued));
    assert!(backed, "the continue policy's back-off: {a:?}");
    let other_clock = continued.replace("10:38am", "9:10am");
    let a_day_before = shifted(&continued, -24 * 3600).replace(
        RESETS_AT,
        &format!("\"resetsAt\":{}", 1_791_999_513 - 24 * 3600),
    );
    let notice_row = continued
        .lines()
        .find(|l| l.contains("\"isApiErrorMessage\":true"))
        .expect("the notice row");
    let quoted = continued.replace(
        notice_row,
        &notice_row
            .replace("\"isApiErrorMessage\":true", "\"isApiErrorMessage\":false")
            .replace(
                "\"model\":\"<synthetic>\"",
                "\"model\":\"claude-sonnet-5-5\"",
            ),
    );
    let subagents = continued.replace(
        notice_row,
        &notice_row.replace("\"isSidechain\":false", "\"isSidechain\":true"),
    );
    let met_again = format!(
        "{continued}\n{}",
        shifted(notice_row, 60).replace(
            "0cf0d54b-537e-4c7a-8e7f-8577289890c5",
            "0cf0d54b-537e-4c7a-8e7f-000000000002"
        )
    );
    assert!(
        quoted != continued && subagents != continued,
        "the variants differ"
    );
    let ended: Vec<String> = [
        ("nothing since the notice", Some(at_limit.as_str())),
        ("another clock", Some(other_clock.as_str())),
        ("the same clock a day before", Some(a_day_before.as_str())),
        ("a reply quoting the notice", Some(quoted.as_str())),
        ("a subagent's notice", Some(subagents.as_str())),
        ("met again", Some(met_again.as_str())),
        ("no transcript", None),
    ]
    .into_iter()
    .filter_map(|(name, tail)| {
        let (a, waits, _) = decided(tail);
        (!waits).then(|| format!("{name}: {a:?}"))
    })
    .collect();
    assert!(ended.is_empty(), "the wait ended: {ended:#?}");
}

fn replied_under_a_shell(reply: &str, fired_row: bool) -> Vec<String> {
    let mut rows = Vec::new();
    for r in fx::screen(fx::WALL_SESSION_RESUMED_BY_ITSELF_MEASURED) {
        let gone = [
            "carry on with the task",
            "You've hit your",
            "/upgrade to",
            "Usage limit reached",
        ]
        .iter()
        .any(|g| r.contains(g))
            || (!fired_row && r.contains("Usage limit reset"));
        if gone {
            continue;
        }
        if r.contains("(reply 3)") {
            for (i, line) in reply.lines().enumerate() {
                rows.push(if i == 0 {
                    format!("⏺ {line}")
                } else {
                    format!("  {line}")
                });
            }
            continue;
        }
        rows.push(r);
    }
    let done = rows
        .iter()
        .rposition(|r| r.starts_with('✻'))
        .expect("the done row");
    rows[done].push_str(" · 1 shell still running");
    let mode = rows
        .iter()
        .position(|r| r.starts_with("  ⏵⏵ auto mode on"))
        .expect("the footer");
    let notified = if fired_row {
        String::new()
    } else {
        format!(
            "{}Usage limit reset · continuing automatically",
            " ".repeat(16)
        )
    };
    rows[mode] = format!(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · 1 shell · ← for agents · ↓ to manage{notified}"
    );
    assert!(aterm_phase::background_only(&rows), "{rows:#?}");
    assert_eq!(aterm_phase::since_limit(&rows), None, "nothing ties it");
    rows
}

#[test]
fn a_limits_end_under_a_background_shell_types_no_keep_going_into_a_done_report() {
    use crate::supervise::policy::turn_end::{
        DONE_CHECK, DONE_CHECK_REVERSIBLE, RULE_DONE_CHECK, RULE_LIMIT_RESUME,
    };
    const RESET: i64 = 1_791_999_480;
    let done = "All done: the change is committed and pushed, and every suite passes.";
    let stub = "Hi! I am the local lab stub, not a model. (reply 3)";
    let measured = transcript("session-limit-cancelled-then-person-turn");
    let rows: Vec<&str> = measured.lines().skip(1).collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    let at_limit = rows[..cancel].join("\n");
    let continued = rows
        .iter()
        .filter(|r| {
            !r.contains("Automatic continue cancelled") && !r.contains("what is left to do?")
        })
        .map(|r| r.replace(stub, done))
        .collect::<Vec<_>>()
        .join("\n");
    let policy = owners_policy();
    let mut wrong = Vec::new();
    for fired_row in [true, false] {
        let seeded = |reply: &str, tail: Option<&str>| {
            let screen = replied_under_a_shell(reply, fired_row);
            let armed: Vec<String> = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED)
                .into_iter()
                .map(|r| r.replace("3:15pm", "10:38am"))
                .collect();
            let wall = aterm_phase::read(Some("claude"), &armed, Some(2))
                .wall
                .expect("the armed wall");
            let host = TranscriptHost::of(&[tail]);
            let opts = SuperviseOpts {
                idle_host: Some(host as Arc<dyn IdleHost>),
                policy: policy.clone(),
                ..auto(30)
            };
            let mut m = Mock::new(vec![screen.clone()]);
            let mut s = session(&mut m, Some("@s-1".to_string()));
            s.set_clock(MEASURED_NOTICE_AT + 30 * 60, PDT);
            s.set_zone_lookup(test_zones);
            s.seed_wall_memory(Some(crate::supervise::approvals::OpenWall {
                conversation: super::super::wall_confirm::conversation_of(measured),
                program: None,
                at_ms: MEASURED_NOTICE_AT * 1000,
                message: wall.message.clone(),
                reset: wall.reset.clone(),
                reset_unix: Some(RESET),
                tap_unix: None,
                kind: Some(wall.kind),
                judged_ms: None,
                retry_ms: None,
            }));
            assert!(s.limit_notice_row.is_none(), "no notice row remembered");
            let point = point_at(&mut s, screen, 101);
            let before = Instant::now();
            let a = s
                .turn_end_at_point(&point, None, &opts)
                .unwrap_or_else(|e| panic!("{e:?}"));
            let after = Instant::now();
            let backed = backs_off(&s, &a, before, after);
            (
                a,
                s.turn_end.waits_on_named_reset(),
                s.turn_end.task_done(),
                backed,
            )
        };
        let (a, waits, _, backed) = seeded(done, Some(&continued));
        if waits || !backed {
            wrong.push(format!("fired row {fired_row}, seeded, transcript: {a:?}"));
        }
        let (a, waits, _, _) = seeded(done, Some(&at_limit));
        if !waits {
            wrong.push(format!("fired row {fired_row}, nothing since: {a:?}"));
        }
        let now = TEST_NOW + 6 * 3600 + 17 * 60;
        let by = now - 60 - MEASURED_NOTICE_AT;
        let at_notice = shifted(&at_limit, by).replace("10:38am", "3:15pm");
        let replied = shifted(&continued, by).replace("10:38am", "3:15pm");
        let host = TranscriptHost::of(&[Some(at_notice.as_str())]);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
            policy: policy.clone(),
            ..auto(30)
        };
        let armed = fx::screen(fx::WALL_SESSION_AUTO_CONTINUE_ARMED_MEASURED);
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(now, PDT);
        s.set_zone_lookup(test_zones);
        let wall = point_at(&mut s, armed, 101);
        let a = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(matches!(&a, TurnEndAction::WaitUntil { .. }), "{a:?}");
        assert!(s.limit_notice_row.is_some(), "its notice row kept");
        host.reads(Some(&replied));
        s.set_clock(now + 20 * 60, PDT);
        let point = point_at(&mut s, replied_under_a_shell(done, fired_row), 102);
        let before = Instant::now();
        let a = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let after = Instant::now();
        if s.turn_end.waits_on_named_reset() || !backs_off(&s, &a, before, after) {
            wrong.push(format!("fired row {fired_row}, saw the notice: {a:?}"));
        }
        for (reply, want, finished) in [
            (
                done,
                TurnEndAction::Type {
                    text: DONE_CHECK.to_string(),
                    rule_id: RULE_DONE_CHECK,
                },
                false,
            ),
            (
                "All done. Shall I delete the old release branches?",
                TurnEndAction::Type {
                    text: format!("{DONE_CHECK} {DONE_CHECK_REVERSIBLE}"),
                    rule_id: RULE_DONE_CHECK,
                },
                false,
            ),
            (
                "All done.\nATERM-TASK-DONE",
                TurnEndAction::Type {
                    text: DONE_CHECK.to_string(),
                    rule_id: RULE_DONE_CHECK,
                },
                false,
            ),
            (
                "Stage 2 of 5 is in; stage 3 is next.",
                TurnEndAction::Type {
                    text: "keep going".to_string(),
                    rule_id: RULE_LIMIT_RESUME,
                },
                false,
            ),
        ] {
            let (unread, waits, _, _) = seeded(reply, None);
            if !matches!(unread, TurnEndAction::WaitUntil { .. }) || !waits {
                wrong.push(format!(
                    "fired row {fired_row}, unread {reply:?}: {unread:?}"
                ));
            }
            let (a, _, task_done, _) = seeded(reply, Some(&at_limit));
            if a != want || task_done != finished {
                wrong.push(format!(
                    "fired row {fired_row}, same conversation, {reply:?}: {a:?} (finished {task_done})"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_bucket_only_its_rate_limit_type_named_is_that_bucket_for_a_loop_that_follows() {
    use crate::supervise::approvals::{Ledger, open_wall, usage_attempt};
    use crate::supervise::policy::turn_end::RULE_LIMIT_RESUME;
    const RESET: i64 = 1_792_279_200;
    let bucket = aterm_phase::WallKind::ModelBucket { consent: false };
    let reset = "Oct 17 at 4:20pm (America/Los_Angeles)";
    let armed = |words: &str| -> Vec<String> {
        fx::screen(fx::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300)
            .into_iter()
            .map(|r| {
                r.replace(
                    &format!("You've hit your Sonnet limit · resets {reset}"),
                    words,
                )
            })
            .collect()
    };
    let resumed = || fx::screen(fx::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300);
    let weekly = transcript("weekly-limit");
    let tail = |words: &str, kind: &str| {
        weekly
            .replace(
                "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
                words,
            )
            .replace(
                "\"rateLimitType\":\"seven_day\"",
                &format!("\"rateLimitType\":\"{kind}\""),
            )
    };
    let generic = format!("You've hit your limit · resets {reset}");
    let mut wrong = Vec::new();
    for (n, (case, words, kind)) in [
        (
            "the generic banner, seven_day_opus",
            generic.clone(),
            "seven_day_opus",
        ),
        (
            "the generic banner, seven_day_overage_included",
            generic.clone(),
            "seven_day_overage_included",
        ),
        (
            "control: the bucket's own words",
            format!("You've hit your Sonnet limit · resets {reset}"),
            "seven_day_sonnet",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let dir = crate::supervise::test_scratch_path("bucket-type-read-back", &n.to_string());
        std::fs::create_dir_all(&dir).expect("the scratch dir");
        let ledger = dir.join("s-1.jsonl");
        let books = dir.join("books.jsonl");
        let t = tail(&words, kind);
        let wall = armed(&words);
        let decided = |s: &mut Session<'_, Mock>,
                       opts: &SuperviseOpts,
                       rows: &[String],
                       seq: u64,
                       worked: Option<Duration>|
         -> (i64, Option<&'static str>, String) {
            let point = point_at(s, rows.to_vec(), seq);
            match s
                .turn_end_at_point(&point, worked, opts)
                .expect("a decision")
            {
                TurnEndAction::WaitUntil { until, why, .. } => (s.unix_of(until), None, why),
                other => (s.now_unix(), other.rule_id(), format!("{other:?}")),
            }
        };
        let host = TranscriptHost::of(&[Some(t.as_str())]);
        let mut opts = hosted_by(&host);
        opts.policy.model_fallback = None;
        opts.policy.continue_policy = true;
        let mut m = Mock::new(vec![wall.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::from_millis(50),
            ..TurnEndTiming::default()
        });
        s.set_clock(RESET - 3600, PDT);
        let try_at = decided(&mut s, &opts, &wall, 100, Some(Duration::from_secs(180))).0;
        s.set_clock(try_at, PDT);
        let p = point_at(&mut s, wall.clone(), 101);
        let a = s.turn_end_at_point(&p, None, &opts).expect("the try");
        assert_eq!(a.rule_id(), Some(RULE_LIMIT_RESUME), "{case}: {a:?}");
        s.pass_wake_settle();
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("typed");
        assert_eq!(
            s.turn_end.wall_attempts(bucket),
            Some(1),
            "{case}: a bucket's try"
        );
        s.set_clock(try_at + 5, PDT);
        let _ = decided(&mut s, &opts, &wall, 102, None);
        std::thread::sleep(Duration::from_millis(80));
        s.set_clock(try_at + 35, PDT);
        let mut mine = decided(&mut s, &opts, &resumed(), 103, None);
        let mut seq = 104;
        while mine.2.contains("to take") && seq < 110 {
            s.set_clock(mine.0, PDT);
            mine = decided(&mut s, &opts, &resumed(), seq, None);
            seq += 1;
        }
        std::fs::copy(&ledger, &books).expect("books");
        let act_ms = usage_attempt(&ledger, Some("@s-1")).map_or(0, |a| a.at_ms);
        drop(s);
        let decide_at = try_at + 60;
        let host2 = TranscriptHost::of(&[Some(t.as_str())]);
        let mut opts2 = hosted_by(&host2);
        opts2.policy.model_fallback = None;
        opts2.policy.continue_policy = true;
        let mut m2 = Mock::new(vec![resumed()]);
        let mut s2 = session(&mut m2, Some("@s-1".to_string()));
        s2.set_clock(decide_at, PDT);
        s2.ledger = Ledger::open(Some(&books), Some("@s-1"), &mut Vec::new());
        seed_usage_act(
            &mut s2.turn_end,
            &books,
            s2.setup.sid.as_deref(),
            act_ms + (decide_at - try_at) * 1000,
            Instant::now(),
        );
        s2.seed_wall_memory(open_wall(&books, Some("@s-1")));
        let tracked = s2.turn_end.wall_attempts(bucket);
        let theirs = decided(&mut s2, &opts2, &resumed(), 101, None);
        drop(s2);
        let _ = std::fs::remove_dir_all(&dir);
        if tracked != Some(1) || theirs.0 + 40 < mine.0 {
            wrong.push(format!(
                "{case}: the successor tracks the bucket at {tracked:?} and acts at try{:+}s \
                 ({}); the first loop at try{:+}s ({})",
                theirs.0 - try_at,
                theirs.2,
                mine.0 - try_at,
                mine.2,
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

fn virtual_clock<C: Ctl>(s: &mut Session<'_, C>, now: i64, offset: i64) {
    use crate::supervise::fx::{ClockSource, Fx, VirtualClock};
    s.set_clock(now, offset);
    s.set_fx(Fx::over(ClockSource::Virtual(Arc::new(VirtualClock::new(
        Instant::now(),
        now.saturating_mul(1000),
    )))));
}
#[test]
fn a_buckets_cancel_on_a_short_pane_is_the_bucket_the_transcript_names() {
    use aterm_phase::WallKind;
    let short: Vec<String> = {
        let rows = fx::screen(fx::WALL_SONNET_BUCKET_CANCELLED_UNDER_24H_2_1_302);
        rows[rows.len() - 12..].to_vec()
    };
    assert!(
        !short.iter().any(|r| r.contains("Sonnet limit")),
        "the bucket's notice off the pane: {short:#?}"
    );
    assert_eq!(
        aterm_phase::left_wall(&short).map(|w| w.kind),
        Some(WallKind::UsageSession),
        "the screen alone: a usage window"
    );
    let rows: Vec<&str> = transcript("bucket-cancelled-then-model")
        .lines()
        .skip(1)
        .collect();
    let notice = rows
        .iter()
        .position(|r| r.contains("\"isApiErrorMessage\":true"))
        .expect("the bucket's notice row");
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    let under_a_day: Vec<String> = rows[..=notice + 1]
        .iter()
        .chain(std::iter::once(&rows[cancel]))
        .map(|r| {
            r.replace("carry on with the task", "keep going")
                .replace("Oct 19 at 2:42am", "7:09pm")
                .replace("\"resetsAt\":1792402927", "\"resetsAt\":1792202940")
        })
        .collect();
    let notice_at: i64 = 1_792_202_088;
    let tail = shifted(&under_a_day.join("\n"), -14 * 60);
    assert_eq!(
        super::super::wall_confirm::transcript_ends_on(&tail).at,
        Some(notice_at),
        "the notice row's own stamp"
    );
    let decided = |tail: Option<&str>| {
        let host = tail.map(|t| TranscriptHost::of(&[Some(t)]));
        let opts = SuperviseOpts {
            idle_host: host.map(|h| h as Arc<dyn IdleHost>),
            policy: SupervisorConfig {
                answer_questions: false,
                ..SupervisorConfig::default()
            },
            ..auto(30)
        };
        let mut m = Mock::new(vec![short.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_clock(notice_at + 1, PDT);
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
        let mut point = point_at(&mut s, short.clone(), 102);
        s.turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.point_as_decided(&mut point);
        let bucket = s
            .turn_end
            .wall_since(WallKind::ModelBucket { consent: false })
            .is_some();
        let window = s.turn_end.wall_since(WallKind::UsageSession).is_some();
        (point.phase, bucket, window)
    };
    let (phase, bucket, window) = decided(Some(tail.as_str()));
    assert!(
        matches!(phase, Phase::Limited { .. }) && bucket && !window,
        "the bucket the transcript's row names: {phase:?}, bucket {bucket}, window {window}"
    );
    let (phase, bucket, window) = decided(None);
    assert!(
        matches!(phase, Phase::Limited { .. }) && window && !bucket,
        "hostless, the screen alone: {phase:?}, bucket {bucket}, window {window}"
    );
}

#[test]
fn a_buckets_cancel_under_the_limit_dialog_answers_the_act_and_nothing_is_typed_into_it() {
    use aterm_phase::WallKind;
    let screen: Vec<String> = fx::screen(fx::WALL_SONNET_BUCKET_CANCELLED_OVER_24H_2_1_302)
        .into_iter()
        .map(|r| r.replace("❯ carry on with the task", "❯ keep going"))
        .collect();
    let rows: Vec<&str> = transcript("bucket-cancelled-then-model")
        .lines()
        .skip(1)
        .collect();
    let cancel = rows
        .iter()
        .position(|r| r.contains("Automatic continue cancelled"))
        .expect("the cancel");
    let tail: Vec<String> = rows[..=cancel]
        .iter()
        .map(|r| {
            r.replace("carry on with the task", "keep going")
                .replace("Oct 19 at 2:42am", "Oct 19 at 2:25am")
                .replace("\"resetsAt\":1792402927", "\"resetsAt\":1792401934")
        })
        .collect();
    let tail = shifted(&tail.join("\n"), -16 * 60);
    let notice_at = super::super::wall_confirm::transcript_ends_on(&tail)
        .notice_at()
        .expect("the bucket's notice row");
    let host = TranscriptHost::of(&[Some(tail.as_str())]);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            model_fallback: None,
            ..SupervisorConfig::default()
        },
        ..auto(30)
    };
    let mut m = Mock::new(vec![screen.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_zone_lookup(test_zones);
    virtual_clock(&mut s, notice_at + 1, PDT);
    let busy = busy_screen();
    let at_act = TurnEndReading::of(
        &aterm_phase::read(Some("claude"), &busy, Some(2)),
        &busy,
        false,
        None,
        None,
        None,
    );
    let typed_at = s.fx.mono();
    s.turn_end.acted(
        &TurnEndAction::Type {
            text: "keep going".to_string(),
            rule_id: crate::supervise::policy::turn_end::RULE_CONTINUE,
        },
        &at_act,
        typed_at,
    );
    let mut acts = Vec::new();
    let mut phase = None;
    for seq in 102..106 {
        let mut point = point_at(&mut s, screen.clone(), seq);
        let a = s
            .turn_end_at_point(&point, None, &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        s.point_as_decided(&mut point);
        phase.get_or_insert(point.phase);
        s.fx.sleep(Duration::from_secs(10 * 60));
        acts.push(a);
    }
    let typed: Vec<_> = acts.iter().filter(|a| a.rule_id().is_some()).collect();
    assert!(typed.is_empty(), "nothing typed into the bucket: {acts:#?}");
    assert!(
        matches!(phase, Some(Phase::Limited { .. }))
            && s.turn_end
                .wall_since(WallKind::ModelBucket { consent: false })
                .is_some()
            && s.turn_end.waits_on_named_reset(),
        "the bucket is the act's answer: {phase:?}"
    );
}

const BEFORE_B_RESETS: i64 = TEST_NOW + 23 * 60 + 59;

#[derive(Clone, Copy, Debug)]
enum AfterB {
    Live,
    Seeded,
    LegacySeed,
    WithoutHost,
}

fn decided_after_b(
    second: &[String],
    tail: Option<&str>,
    clock: i64,
    past: bool,
    after: AfterB,
) -> TurnEndAction {
    let armed = fx::screen(fx::WALL_SESSION_ARMED_CONVERSATION_B_2_1_300);
    let host = TranscriptHost::of(&[Some(transcript("session-limit"))]);
    let mut opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            continue_policy: false,
            ..SupervisorConfig::default()
        },
        ..auto(30)
    };
    let mut m = Mock::new(vec![armed.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_zone_lookup(test_zones);
    virtual_clock(&mut s, clock, PDT);
    s.set_turn_end_timing(TurnEndTiming {
        reset_grace: Duration::ZERO,
        reset_jitter: Duration::ZERO,
        ..TurnEndTiming::default()
    });
    let wall = point_at(&mut s, armed, 101);
    let a = s
        .turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(
        matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("goes on by itself")),
        "B's armed countdown: {a:?}"
    );
    use crate::supervise::approvals::{self, Outcome, Row};
    use crate::supervise::policy::turn_end::Conversation;
    let memory = s.wall_memory.as_ref().expect("the confirmed wall");
    assert!(
        matches!(&memory.conversation, Conversation::Named(_)),
        "the confirming tail named B"
    );
    if matches!(after, AfterB::Seeded | AfterB::LegacySeed) {
        let reason = approvals::wall_wait_reason(
            s.wall_reset.as_ref().and_then(|(_, at)| *at),
            memory.wall.reset.as_deref(),
            memory.wall.kind,
            &memory.wall.message,
        );
        let row = Row {
            rule_id: approvals::RULE_WALL_WAIT,
            outcome: Outcome::Waiting,
            command: "",
            reason: &reason,
            box_seq: 101,
        };
        let body = if matches!(after, AfterB::Seeded) {
            row.to_json_wall_wait(clock * 1000, Some("@s-1"), &memory.conversation)
        } else {
            row.to_json(clock * 1000, Some("@s-1"))
        };
        let open = approvals::open_wall_in(&body, Some("@s-1")).expect("the same wall read back");
        assert_eq!(
            open.kind,
            Some(memory.wall.kind),
            "identity is outside the legacy kind span"
        );
        drop(s);
        s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        virtual_clock(&mut s, clock, PDT);
        s.set_turn_end_timing(TurnEndTiming {
            reset_grace: Duration::ZERO,
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.seed_wall_memory(Some(open));
    }
    if matches!(after, AfterB::WithoutHost) {
        opts.idle_host = None;
    }
    if past {
        // The reset is a second off: past it once this virtual sleep has run.
        s.fx.sleep(Duration::from_millis(1500));
    }
    host.reads(tail);
    let p = point_at(&mut s, second.to_vec(), 102);
    s.turn_end_at_point(&p, None, &opts)
        .unwrap_or_else(|e| panic!("{e:?}"))
}

#[test]
fn a_limits_continuation_is_typed_into_no_other_conversation() {
    use crate::supervise::approvals::{self, Outcome, Row};
    use crate::supervise::policy::turn_end::Conversation;
    let resumed = fx::screen(fx::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300);
    let ours = transcript("session-limit");
    let theirs = another_conversation(ours);
    let unnamed = ours.replace("\"sessionId\":", "\"oldSessionId\":");
    for after in [AfterB::Live, AfterB::Seeded] {
        let a = decided_after_b(&resumed, Some(&theirs), BEFORE_B_RESETS, true, after);
        assert_eq!(
            a.rule_id(),
            None,
            "nothing typed into A at B's reset ({after:?}): {a:?}"
        );
        for tail in [None, Some(""), Some(unnamed.as_str())] {
            let a = decided_after_b(&resumed, tail, BEFORE_B_RESETS, true, after);
            assert!(
                matches!(a, TurnEndAction::WaitUntil { .. }),
                "unread ({after:?}): {a:?}"
            );
        }
        let a = decided_after_b(&resumed, Some(ours), BEFORE_B_RESETS, true, after);
        assert_eq!(
            a.rule_id(),
            Some(crate::supervise::policy::turn_end::RULE_LIMIT_RESUME),
            "B's own ({after:?}): {a:?}"
        );
    }
    let a = decided_after_b(&resumed, None, BEFORE_B_RESETS, true, AfterB::WithoutHost);
    assert!(
        matches!(a, TurnEndAction::WaitUntil { .. }),
        "a departed host cannot unbind B: {a:?}"
    );
    let a = decided_after_b(
        &resumed,
        Some(ours),
        BEFORE_B_RESETS,
        true,
        AfterB::LegacySeed,
    );
    assert!(
        matches!(a, TurnEndAction::WaitUntil { .. }),
        "a legacy host-scoped seed has no owner: {a:?}"
    );

    let reason = approvals::wall_wait_reason(
        Some(BEFORE_B_RESETS + 1),
        Some("9:19am"),
        aterm_phase::WallKind::UsageSession,
        "session limit",
    );
    let row = Row {
        rule_id: approvals::RULE_WALL_WAIT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 101,
    };
    for conversation in [
        Conversation::Hostless,
        Conversation::Unknown,
        Conversation::Named("B;\"unicode-λ".to_owned()),
    ] {
        let body = row.to_json_wall_wait(BEFORE_B_RESETS * 1000, Some("@s-1"), &conversation);
        let value: aterm_json::Value = aterm_json::from_str(&body).expect("JSON");
        assert_eq!(
            value.get("reason").and_then(aterm_json::Value::as_str),
            Some(reason.as_str())
        );
        let open = approvals::open_wall_in(&body, Some("@s-1")).expect("legacy-compatible row");
        assert_eq!(open.conversation, conversation);
        assert_eq!(open.kind, Some(aterm_phase::WallKind::UsageSession));
    }
    let tried = Row {
        rule_id: crate::supervise::policy::turn_end::RULE_LIMIT_RESUME,
        outcome: Outcome::Typed,
        command: "keep going",
        reason: &approvals::act_reason(Some(crate::supervise::policy::turn_end::LimitTry {
            bucket: false,
            in_place: false,
            attempts: 1,
            entitlement: false,
        })),
        box_seq: 102,
    };
    let owner = Conversation::Named("B".to_owned());
    let body = tried.to_json_scoped(BEFORE_B_RESETS * 1000, Some("@s-1"), None, None, &owner);
    assert!(approvals::open_wall_in(&body, Some("@s-1")).is_none());
    assert_eq!(
        approvals::usage_attempt_in(&body, Some("@s-1"))
            .expect("the try")
            .conversation,
        owner
    );
    let mut state = crate::supervise::policy::turn_end::TurnEndState::default();
    seed_usage_act_in(
        &mut state,
        &body,
        Some("@s-1"),
        &OlderReads::default(),
        (BEFORE_B_RESETS + 60) * 1000,
        Instant::now(),
    );
    assert_eq!(state.limit_conversation(), Some(&owner));
    let point = Row {
        rule_id: approvals::RULE_POINT,
        outcome: Outcome::Waiting,
        command: "",
        reason: "point",
        box_seq: 103,
    };
    let conflicting = point.to_json_scoped(
        (BEFORE_B_RESETS + 60) * 1000,
        Some("@s-1"),
        None,
        None,
        &Conversation::Named("A".to_owned()),
    );
    assert_eq!(
        approvals::usage_attempt_in(&format!("{body}\n{conflicting}\n"), Some("@s-1"))
            .expect("the original try")
            .conversation,
        Conversation::Unknown
    );
    let unknown = point.to_json_scoped(
        (BEFORE_B_RESETS + 60) * 1000,
        Some("@s-1"),
        None,
        None,
        &Conversation::Unknown,
    );
    let legacy = point.to_json((BEFORE_B_RESETS + 61) * 1000, Some("@s-1"));
    let body = format!("{body}\n{unknown}\n{legacy}\n");
    assert_eq!(
        approvals::usage_attempt_in(&body, Some("@s-1"))
            .expect("the scoped try")
            .conversation,
        Conversation::Unknown
    );
}

fn with_a_shell(mut rows: Vec<String>) -> Vec<String> {
    let done = rows
        .iter()
        .rposition(|r| r.starts_with('✻'))
        .expect("done row");
    rows[done].push_str(" · 1 shell still running");
    let footer = rows
        .iter()
        .rposition(|r| r.contains("auto mode on"))
        .expect("mode row");
    rows[footer].push_str(" · 1 shell");
    assert!(aterm_phase::background_only(&rows), "{rows:#?}");
    rows
}

#[test]
fn a_remembered_limits_continuation_is_typed_into_no_other_conversation() {
    let resumed = with_a_shell(fx::screen(fx::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300));
    let ours = transcript("session-limit");
    let theirs = another_conversation(ours);
    let a = decided_after_b(&resumed, Some(&theirs), BEFORE_B_RESETS, true, AfterB::Live);
    assert_eq!(
        a.rule_id(),
        None,
        "nothing typed into conversation A at B's reset: {a:?}"
    );
    let a = decided_after_b(&resumed, Some(ours), BEFORE_B_RESETS, true, AfterB::Live);
    assert_eq!(
        a.rule_id(),
        Some(crate::supervise::policy::turn_end::RULE_LIMIT_RESUME),
        "B's remembered wall at its reset: {a:?}"
    );
}

fn decided_at_scrolled_bucket(second: &[String], tail: &str) -> (TurnEndAction, TurnEndAction) {
    let sep = |s: &str| s.replace("Oct 19 at 2:42am", "Sep 19 at 2:42am");
    let armed: Vec<String> = fx::screen(fx::WALL_SONNET_BUCKET_ARMED_NOTICE_SCROLLED_OFF_2_1_302)
        .iter()
        .map(|r| sep(r))
        .collect();
    let whole = sep(transcript("bucket-cancelled-then-model"));
    let at_wall: String = whole
        .lines()
        .take_while(|l| !(l.starts_with('{') && l.contains("Automatic continue cancelled")))
        .map(|l| format!("{l}\n"))
        .collect();
    let host = TranscriptHost::of(&[Some(&at_wall)]);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        },
        ..auto(30)
    };
    let mut m = Mock::new(vec![armed.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_zone_lookup(test_zones);
    s.set_clock(TEST_NOW, PDT);
    let wall = point_at(&mut s, armed, 101);
    let first = s
        .turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    host.reads(Some(&sep(tail)));
    let p = point_at(&mut s, second.to_vec(), 102);
    let second = s
        .turn_end_at_point(&p, None, &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    (first, second)
}

#[test]
fn a_model_chosen_off_a_bucket_whose_notice_scrolled_off_moves_the_session_off_it() {
    let chosen = fx::screen(fx::SONNET_BUCKET_CANCELLED_THEN_MODEL_NOTICE_SCROLLED_OFF_2_1_302);
    let whole = transcript("bucket-cancelled-then-model");
    let (first, second) = decided_at_scrolled_bucket(&chosen, whole);
    assert!(
        matches!(&first, TurnEndAction::WaitUntil { why, .. } if why.contains("model-bucket")),
        "the bucket the transcript names: {first:?}"
    );
    assert_eq!(
        second.rule_id(),
        Some(crate::supervise::policy::turn_end::RULE_LIMIT_RESUME),
        "off the bucket's model, its resume at once: {second:?}"
    );
    let unchosen: String = whole
        .lines()
        .take_while(|l| !(l.starts_with('{') && l.contains("<command-name>/model</command-name>")))
        .map(|l| format!("{l}\n"))
        .collect();
    let same = whole.replace("Set model to `Opus 5.5`", "Set model to `Sonnet 5.5`");
    let same_rows: Vec<String> = chosen
        .iter()
        .map(|r| r.replace("Opus 5.5", "Sonnet 5.5"))
        .collect();
    for (name, rows, tail) in [
        ("no choice since the notice", &chosen, unchosen.as_str()),
        ("the same family", &same_rows, same.as_str()),
    ] {
        let (_, second) = decided_at_scrolled_bucket(rows, tail);
        assert!(
            matches!(&second, TurnEndAction::WaitUntil { why, .. } if why.contains("model-bucket resets")),
            "{name}: the bucket's wait stands: {second:?}"
        );
    }
}

fn bucket_tail_on_the_tests_clock(tail: &str) -> String {
    tail.replace("Oct 19 at 2:42am", "Sep 19 at 2:42am")
        .replace(
            "\"resetsAt\":1792402927",
            &format!("\"resetsAt\":{}", 1_792_402_927 - 30 * 24 * 3600),
        )
}

fn seeded_at_scrolled_bucket(second: &[String], tail: &str) -> TurnEndAction {
    use crate::supervise::approvals::{Ledger, open_wall};
    let dir = crate::supervise::ScratchDir::new("seeded-bucket", "e");
    let ledger = dir.join("s-1.jsonl");
    let armed: Vec<String> = fx::screen(fx::WALL_SONNET_BUCKET_ARMED_NOTICE_SCROLLED_OFF_2_1_302)
        .iter()
        .map(|r| r.replace("Oct 19 at 2:42am", "Sep 19 at 2:42am"))
        .collect();
    let whole = bucket_tail_on_the_tests_clock(transcript("bucket-cancelled-then-model"));
    let at_wall: String = whole
        .lines()
        .take_while(|l| !(l.starts_with('{') && l.contains("Automatic continue cancelled")))
        .map(|l| format!("{l}\n"))
        .collect();
    let opts_of = |host: &Arc<TranscriptHost>| SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        },
        ..auto(30)
    };
    {
        let host = TranscriptHost::of(&[Some(&at_wall)]);
        let opts = opts_of(&host);
        let mut m = Mock::new(vec![armed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
        s.set_zone_lookup(test_zones);
        s.set_clock(TEST_NOW, PDT);
        let wall = point_at(&mut s, armed.clone(), 101);
        let first = s
            .turn_end_at_point(&wall, Some(Duration::from_secs(60)), &opts)
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert!(
            matches!(&first, TurnEndAction::WaitUntil { why, .. } if why.contains("model-bucket")),
            "the bucket the transcript names: {first:?}"
        );
    }
    let open = open_wall(&ledger, Some("@s-1"));
    assert!(open.is_some(), "the first loop's wait: {open:?}");
    let host = TranscriptHost::of(&[Some(&bucket_tail_on_the_tests_clock(tail))]);
    let opts = opts_of(&host);
    let mut m = Mock::new(vec![second.to_vec()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_zone_lookup(test_zones);
    s.set_clock(TEST_NOW + 120, PDT);
    s.seed_wall_memory(open);
    assert!(s.turn_end.waits_on_named_reset(), "the wait taken over");
    assert!(s.limit_notice_row.is_none(), "no notice row remembered");
    let p = point_at(&mut s, second.to_vec(), 102);
    s.turn_end_at_point(&p, None, &opts)
        .unwrap_or_else(|e| panic!("{e:?}"))
}

#[test]
fn a_model_chosen_off_a_bucket_moves_a_loop_that_took_its_wait_over() {
    let chosen = fx::screen(fx::SONNET_BUCKET_CANCELLED_THEN_MODEL_NOTICE_SCROLLED_OFF_2_1_302);
    let whole = transcript("bucket-cancelled-then-model");
    let a = seeded_at_scrolled_bucket(&chosen, whole);
    assert_eq!(
        a.rule_id(),
        Some(crate::supervise::policy::turn_end::RULE_LIMIT_RESUME),
        "off the bucket's model, its resume at once: {a:?}"
    );
    let unchosen: String = whole
        .lines()
        .take_while(|l| !(l.starts_with('{') && l.contains("<command-name>/model</command-name>")))
        .map(|l| format!("{l}\n"))
        .collect();
    let same = whole.replace("Set model to `Opus 5.5`", "Set model to `Sonnet 5.5`");
    let same_rows: Vec<String> = chosen
        .iter()
        .map(|r| r.replace("Opus 5.5", "Sonnet 5.5"))
        .collect();
    let a_day_off = whole.replace(
        "\"resetsAt\":1792402927",
        &format!("\"resetsAt\":{}", 1_792_402_927 - 31 * 24 * 3600),
    );
    for (name, rows, tail) in [
        ("no choice since the notice", &chosen, unchosen.as_str()),
        ("the same family", &same_rows, same.as_str()),
        (
            "the notice a day off the reset",
            &chosen,
            a_day_off.as_str(),
        ),
    ] {
        let a = seeded_at_scrolled_bucket(rows, tail);
        assert!(
            matches!(&a, TurnEndAction::WaitUntil { why, .. } if why.contains("model-bucket resets")),
            "{name}: the bucket's wait stands: {a:?}"
        );
    }
}

#[test]
fn a_standing_weekly_limit_over_a_refused_generic_banner_and_transcript_only_row_marks_the_point_limited()
 {
    use aterm_phase::WallKind;
    let weekly = transcript("weekly-limit");
    let queue_only = r#"{"parentUuid":"00000000-0000-4000-8000-000000000001","isSidechain":false,"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>wf1</task-id>\n<status>failed</status>\n<summary>Orphaned on session resume</summary>\n</task-notification>"},"uuid":"00000000-0000-4000-8000-000000000099","timestamp":"2026-10-08T16:36:00.000Z","queueTranscriptOnly":true}"#;
    let tail = format!("{weekly}\n{queue_only}");
    let notice_at = super::super::wall_confirm::transcript_ends_on(&tail)
        .notice_at()
        .expect("the weekly limit's notice row stands through queueTranscriptOnly");
    let mut rows = ended_on(&[
        "⏺ Goal paused · usage limit reached · send a message after it resets to continue",
    ]);
    let footer = rows.len() - 1;
    rows.insert(
        footer,
        "  ⚠ Usage limit reached · limit resets Oct 11 at 9:35am".to_string(),
    );
    let host = TranscriptHost::of(&[Some(tail.as_str())]);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            answer_questions: false,
            ..SupervisorConfig::default()
        },
        ..auto(30)
    };
    let mut m = Mock::new(vec![rows.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_string()));
    s.set_zone_lookup(test_zones);
    s.set_clock(notice_at + 10, PDT);
    let mut point = point_at(&mut s, rows, 101);
    let action = s
        .turn_end_at_point(&point, Some(Duration::from_secs(60)), &opts)
        .unwrap_or_else(|e| panic!("{e:?}"));
    s.point_as_decided(&mut point);
    assert!(
        matches!(&action, TurnEndAction::WaitUntil { why, .. } if why.contains("usage-weekly")),
        "waits on usage-weekly: {action:?}"
    );
    assert!(
        matches!(
            &point.phase,
            Phase::Limited { message, reset }
                if message.contains("weekly limit") && reset.as_deref() == Some("Oct 11 at 9:35am")
        ),
        "point_as_decided promotes the standing weekly limit onto point.phase: {:?}",
        point.phase
    );
    assert!(
        s.turn_end.wall_since(WallKind::UsageWeekly).is_some(),
        "weekly wall recorded in turn_end"
    );
}
