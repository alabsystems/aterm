// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session log transcript step tests through the footer reader.

use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_agent::harness::footer;
use aterm_slog::Tier;

use super::super::tests::{
    WALL0, hand_clock, lines, lines_of, stamp, time_order_holds, with_claude,
};
use super::*;
use crate::claude_footer::{LogWake, rig};
use crate::codex_rollout_tap::{Host, Image};

const PID: i32 = 300;

fn at(ms: u64) -> String {
    iso(WALL0 + ms)
}

fn iso(wall: u64) -> String {
    let mut s = aterm_types::rfc3339::format_rfc3339(wall / 1000);
    s.pop();
    format!("{s}.{:03}Z", wall % 1000)
}

fn prompt(id: &str, ms: u64, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"promptId":"{id}","type":"user","message":{{"role":"user","content":"{text}"}},"uuid":"u-{id}","timestamp":"{}","version":"2.1.300"}}"#,
        at(ms)
    )
}

fn calls(msg: &str, ms: u64, tools: &[(&str, &str)]) -> String {
    let parts: Vec<String> = tools
        .iter()
        .map(|(id, name)| {
            format!(
                r#"{{"type":"tool_use","id":"{id}","name":"{name}","input":{{"command":"SENTINEL-INPUT"}}}}"#
            )
        })
        .collect();
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"id":"{msg}","model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"SENTINEL-REPLY"}},{}],"usage":{{"input_tokens":3,"output_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}},"uuid":"a-{msg}","timestamp":"{}","effort":"high","version":"2.1.300"}}"#,
        parts.join(","),
        at(ms)
    )
}

fn results(ms: u64, ids: &[(&str, bool)]) -> String {
    let parts: Vec<String> = ids
        .iter()
        .map(|(id, error)| {
            format!(
                r#"{{"type":"tool_result","tool_use_id":"{id}","is_error":{error},"content":[{{"type":"text","text":"SENTINEL-RESULT"}}]}}"#
            )
        })
        .collect();
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":[{}]}},"uuid":"r-{ms}","timestamp":"{}","toolUseResult":{{"stdout":"SENTINEL-RESULT"}}}}"#,
        parts.join(","),
        at(ms)
    )
}

fn turn_end(ms: u64, dur: u64) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":{dur},"timestamp":"{}","uuid":"d-{ms}","isMeta":false}}"#,
        at(ms)
    )
}

fn notice(ms: u64, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"id":"n-{ms}","model":"<synthetic>","role":"assistant","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":0,"output_tokens":0}}}},"isApiErrorMessage":true,"error":"rate_limit","uuid":"n-{ms}","timestamp":"{}"}}"#,
        at(ms)
    )
}

fn local_command(ms: u64, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout>{text}</local-command-stdout>","level":"info","timestamp":"{}","uuid":"l-{ms}","isMeta":false}}"#,
        at(ms)
    )
}

struct Rig {
    tl: Arc<Mutex<SessionTimeline>>,
    hand: super::super::tests::Hand,
    root: std::path::PathBuf,
    claude: std::path::PathBuf,
    transcript: std::path::PathBuf,
    fold: footer::FooterCache,
    tail: footer::TailCache,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let tl = Arc::new(Mutex::new(SessionTimeline::default()));
        let hand = hand_clock(&tl);
        {
            let mut g = tl.lock().unwrap();
            let now = g.slog().now();
            g.slog_mut().open_session(None, None, now);
        }
        Rig::on(name, tl, hand)
    }

    fn on(name: &str, tl: Arc<Mutex<SessionTimeline>>, hand: super::super::tests::Hand) -> Rig {
        tl.lock().unwrap().note_root_group(100);
        with_claude(&tl);
        let (root, claude) = rig::claude_home(name, PID, WALL0 / 1000 - 60);
        let transcript = rig::transcript_of(&root, &claude);
        Rig {
            tl,
            hand,
            root,
            claude,
            transcript,
            fold: footer::FooterCache::default(),
            tail: footer::TailCache::default(),
        }
    }

    fn touch(&self) {
        self.append(&[]);
    }

    fn append(&self, rows: &[String]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript)
            .unwrap();
        for row in rows {
            writeln!(f, "{row}").unwrap();
        }
    }

    fn read(&mut self, wake: &LogWake) -> Applied {
        rig::read(
            &self.tl,
            PID,
            &self.claude,
            &mut self.fold,
            &mut self.tail,
            7,
            wake,
        )
    }

    fn publish(&self, ms: u64, word: &'static str, seq: u64) {
        self.hand.set(ms);
        self.tl.lock().unwrap().publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(seq),
        );
    }

    fn lines(&self) -> Vec<String> {
        lines(self.tl.lock().unwrap().slog().log())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn having<'a>(got: &'a [String], needle: &str) -> Vec<&'a String> {
    got.iter().filter(|l| l.contains(needle)).collect()
}

#[test]
fn a_turns_transcript_steps_reach_the_log_through_the_footer_read() {
    let mut r = Rig::new("slog-tx-steps");
    r.append(&[
        prompt("old", 10, "an old prompt"),
        calls("m0", 20, &[("toolu_old", "Bash")]),
    ]);
    let history = std::fs::metadata(&r.transcript).unwrap().len();
    let first = r.read(&LogWake::default());
    assert!(!first.disarm, "a Claude run takes the steps: {first:?}");
    let got = r.lines();
    let source = having(&got, " source field=transcript ");
    assert_eq!(source.len(), 1, "{got:?}");
    assert!(
        source[0].contains(" state=partial ")
            && source[0].contains(&format!(" skipped={history} ")),
        "{source:?}"
    );
    assert!(
        having(&got, "toolu_old").is_empty() && having(&got, "vref=old").is_empty(),
        "history is never replayed (S1): {got:?}"
    );
    r.publish(1_000, "busy", 5);
    r.append(&[
        prompt("v1", 1_500, "list the files"),
        calls("m1", 2_000, &[("toolu_a", "Bash")]),
        results(3_000, &[("toolu_a", false)]),
        turn_end(4_000, 2_500),
    ]);
    let second = r.read(&LogWake::default());
    assert!(second.owed, "the log moved: {second:?}");
    r.publish(5_000, "idle", 6);
    r.hand.set(6_000);
    let _ = r.tl.lock().unwrap().slog_tick();
    let got = r.lines();
    let one = |needle: &str| {
        let found = having(&got, needle);
        assert_eq!(found.len(), 1, "{needle}: {got:?}");
        found[0].clone()
    };
    assert!(one(" amend t1 turn vref=v1 ").contains(" src=transcript conf=strong "));
    let open = one(" open p1 tool par=t1 ");
    assert!(
        open.contains(" cls=shell ")
            && open.contains(" tool=Bash ")
            && open.contains(" vref=toolu_a ")
            && open.contains(" by=vendor "),
        "{open}"
    );
    assert!(one(" close p1 tool out=ok ").contains(" dur=wall "));
    let numbers = one(" amend t1 turn vdur_ms=2500 ");
    assert!(
        numbers.contains(" tok_in=1103 ") && numbers.contains(" tok_out=20 "),
        "{numbers}"
    );
    let close = one(" close t1 turn ");
    assert!(
        close.contains(" out=done ")
            && close.contains(" src=screen ")
            && !close.contains("vdur_ms"),
        "the screen's settle closes it, with no transcript number (A3): {close}"
    );
    assert_eq!(having(&got, " open t").len(), 1, "one turn (S30): {got:?}");
    time_order_holds(&got).unwrap();
}

#[test]
fn a_transcript_written_after_the_tap_armed_is_read_from_its_first_byte() {
    let mut r = Rig::new("slog-tx-vacant");
    let none = r.read(&LogWake::default());
    assert!(
        !none.disarm && r.fold.steps_armed() && !r.fold.steps_bound(),
        "armed, waiting for its file: {none:?}"
    );
    assert!(having(&r.lines(), " source ").is_empty());
    r.publish(1_000, "busy", 5);
    r.append(&[
        calls("m0", 0, &[("toolu_stale", "Bash")]).replace(&at(0), &iso(WALL0 - 30_000)),
        prompt("v1", 1_100, "go"),
        calls("m1", 1_200, &[("toolu_a", "Bash")]),
    ]);
    let _ = r.read(&LogWake::default());
    let got = r.lines();
    let source = having(&got, " source field=transcript ");
    assert!(
        source.len() == 1 && source[0].contains(" skipped=0 "),
        "{got:?}"
    );
    assert_eq!(having(&got, " amend t1 turn vref=v1 ").len(), 1, "{got:?}");
    assert_eq!(having(&got, " tool=Bash vref=toolu_a ").len(), 1, "{got:?}");
    assert!(having(&got, "toolu_stale").is_empty(), "{got:?}");
    time_order_holds(&got).unwrap();
}

#[test]
fn a_first_binding_read_says_the_rows_it_lost() {
    for (name, broken) in [("slog-tx-lost", true), ("slog-tx-lost-ctl", false)] {
        let mut r = Rig::new(name);
        let _ = r.read(&LogWake::default());
        r.publish(1_000, "busy", 5);
        let mut rows = vec![prompt("v1", 1_100, "go")];
        if broken {
            rows.push(r#"{"type":"user", not json"#.to_string());
        }
        rows.push(calls("m1", 1_200, &[("toolu_a", "Bash")]));
        r.append(&rows);
        let _ = r.read(&LogWake::default());
        let got = r.lines();
        let sources = having(&got, " source field=transcript ");
        assert!(sources[0].contains(" skipped=0 "), "{got:?}");
        let dropped = having(&got, " dropped=1 ");
        assert_eq!(dropped.len(), usize::from(broken), "{got:?}");
        assert_eq!(having(&got, " tool=Bash vref=toolu_a ").len(), 1, "{got:?}");
        time_order_holds(&got).unwrap();
    }
}

#[test]
fn a_burst_past_the_queue_keeps_its_bind_and_says_its_count() {
    use aterm_agent::harness::steps::STEP_QUEUE;
    let mut r = Rig::new("slog-tx-burst");
    let _ = r.read(&LogWake::default());
    r.publish(1_000, "busy", 5);
    let mut rows = vec![prompt("v1", 1_100, "go")];
    let pairs = STEP_QUEUE / 2 + 10;
    for i in 0..pairs {
        let id = format!("toolu_{i:04}");
        rows.push(calls(&format!("m{i}"), 1_150, &[(&id, "Read")]));
        rows.push(results(1_160, &[(&id, false)]));
    }
    r.append(&rows);
    let _ = r.read(&LogWake::default());
    let got = r.lines();
    let sources = having(&got, " source field=transcript ");
    assert!(
        sources
            .iter()
            .all(|l| l.contains(" skipped=0 ") || !l.contains(" skipped=")),
        "bound from its first byte: {sources:?}"
    );
    assert!(
        sources.iter().any(|l| l.contains(" dropped=")),
        "the count is said: {sources:?}"
    );
    r.append(&[
        prompt("v2", 1_300, "after"),
        calls("m-after", 1_400, &[("toolu_after", "Bash")]),
    ]);
    let _ = r.read(&LogWake::default());
    let got = r.lines();
    assert_eq!(having(&got, " vref=toolu_after ").len(), 1, "{got:?}");
    time_order_holds(&got).unwrap();
}

#[test]
fn a_long_batch_is_applied_a_chunk_a_hold() {
    let mut r = Rig::new("slog-tx-chunks");
    r.touch();
    let _ = r.read(&LogWake::default());
    r.publish(1_000, "busy", 5);
    let mut rows = vec![prompt("v1", 1_100, "go")];
    for i in 0..40u64 {
        let id = format!("toolu_{i:02}");
        rows.push(calls(&format!("m{i}"), 1_200 + i * 10, &[(&id, "Read")]));
        rows.push(results(1_205 + i * 10, &[(&id, false)]));
    }
    r.append(&rows);
    let applied = r.read(&LogWake::default());
    assert_eq!(applied.events, 82, "{applied:?}");
    assert_eq!(applied.holds, 82_usize.div_ceil(CHUNK), "{applied:?}");
    let got = r.lines();
    assert_eq!(having(&got, " tool out=ok ").len(), 40, "{got:?}");
    r.append(&[
        calls("m99", 2_000, &[("toolu_z", "Grep")]),
        results(2_010, &[("toolu_z", true)]),
    ]);
    let small = r.read(&LogWake::default());
    assert_eq!((small.events, small.holds), (2, 1), "{small:?}");
    assert!(having(&r.lines(), " tool out=error ").len() == 1);
}

fn timed(tl: &Mutex<SessionTimeline>, batch: &StepBatch) -> (Applied, Vec<(Hold, u64)>) {
    let mut t = 0u64;
    let mut booked = Vec::new();
    let applied = apply_batch_timed(
        tl,
        Source::Transcript,
        batch,
        false,
        &mut || {
            t += 7;
            t
        },
        &mut |kind, ns| booked.push((kind, ns)),
    );
    (applied, booked)
}

fn compactions(n: u64) -> StepBatch {
    use aterm_agent::harness::steps::StepPos;
    let step = |off: u64, ev: StepEvent| Step {
        pos: StepPos {
            fp: 1,
            id: None,
            off,
            at_ms: WALL0,
        },
        state: StepState::Partial,
        ev,
    };
    StepBatch {
        steps: std::iter::once(step(0, StepEvent::Bind { len: 0 }))
            .chain((1..=n).map(|i| step(i, StepEvent::Compact)))
            .collect(),
        ..StepBatch::default()
    }
}

#[test]
fn each_chunk_hold_is_timed_and_booked_by_its_kind() {
    let r = Rig::new("slog-tx-hold");
    let (applied, booked) = timed(&r.tl, &compactions(81));
    assert_eq!((applied.events, applied.holds), (82, 3), "{applied:?}");
    assert_eq!(
        booked,
        [(Hold::Full, 7), (Hold::Full, 7), (Hold::Last, 7)],
        "two full chunks of {CHUNK}, then the last"
    );
    let (applied, booked) = timed(&r.tl, &StepBatch::default());
    assert_eq!((applied.events, applied.holds), (0, 1), "{applied:?}");
    assert_eq!(booked, [(Hold::Last, 7)], "an empty read's one hold");
    let small = Rig::new("slog-tx-hold-small");
    let (applied, booked) = timed(&small.tl, &compactions(CHUNK as u64 - 1));
    assert_eq!((applied.events, applied.holds), (CHUNK, 1), "{applied:?}");
    assert_eq!(booked, [(Hold::Last, 7)]);
    let tl = Mutex::new(SessionTimeline::default());
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
    }
    let (applied, booked) = timed(&tl, &compactions(81));
    assert!(applied.disarm && applied.events == 0, "{applied:?}");
    assert!(booked.is_empty(), "{booked:?}");
}

#[test]
fn the_footer_read_wakes_the_watchers_directly() {
    let h = crate::session_store::test_handle(7);
    let tl = Arc::clone(&h.ctx.timeline);
    let hand = hand_clock(&tl);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    let mut r = Rig::on("slog-tx-wake", tl, hand);
    r.touch();
    let wake = LogWake::default();
    let sub = crate::session_watch::Subscription::single(&h.ctx.watchers);
    let _ = r.read(&wake);
    assert!(sub.wait(Duration::ZERO), "the bind's source mark woke it");
    let idle = r.read(&wake);
    assert!(!idle.owed && !sub.wait(Duration::ZERO), "{idle:?}");
    r.publish(1_000, "busy", 5);
    let _ = sub.wait(Duration::ZERO);
    r.append(&[
        prompt("v1", 1_100, "go"),
        calls("m1", 1_200, &[("toolu_a", "Bash")]),
    ]);
    let moved = r.read(&wake);
    assert!(moved.owed && sub.wait(Duration::ZERO), "{moved:?}");
    let (since, ep) = {
        let g = r.tl.lock().unwrap();
        (g.slog().log().last_seq(), g.slog().log().ep())
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let (h2, store2) = (h.clone(), store.clone());
    std::thread::spawn(move || {
        let reply = crate::control::cmd_await(
            &h2.term,
            &store2,
            h2.local_id,
            &h2.ctx,
            &format!("log kind=tool since={since} ep={ep} timeout=600000"),
        );
        let _ = tx.send(reply);
    });
    r.append(&[calls("m2", 1_300, &[("toolu_b", "Bash")])]);
    let woke = r.read(&wake);
    assert!(woke.owed, "{woke:?}");
    let reply = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the parked await returns on the footer's wake");
    assert!(reply.contains(" tool par=t1 "), "{reply}");
}

#[test]
fn a_tap_with_no_run_to_take_it_disarms_and_rebinds_past_the_history() {
    let mut r = Rig::new("slog-tx-disarm");
    r.touch();
    let _ = r.read(&LogWake::default());
    assert!(r.fold.steps_armed());
    r.hand.set(1_000);
    r.tl.lock().unwrap().note_foreground_group(100);
    r.append(&[prompt("v1", 1_100, "go")]);
    let gone = r.read(&LogWake::default());
    assert!(gone.disarm && !r.fold.steps_armed(), "{gone:?}");
    let none = r.read(&LogWake::default());
    assert!(!none.disarm && none.events == 0 && !r.fold.steps_armed());
    r.hand.set(2_000);
    {
        let mut g = r.tl.lock().unwrap();
        assert!(g.note_foreground_group(PID));
        assert!(g.set_program(PID, Some("claude".into())));
        let pgid = g.agent().program_pgid;
        let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
    }
    let len = std::fs::metadata(&r.transcript).unwrap().len();
    let back = r.read(&LogWake::default());
    assert!(!back.disarm && r.fold.steps_armed(), "{back:?}");
    r.publish(3_000, "busy", 7);
    r.append(&[prompt("v2", 3_100, "again")]);
    let _ = r.read(&LogWake::default());
    let got = r.lines();
    let sources = having(&got, " source field=transcript ");
    assert_eq!(sources.len(), 2, "one per run: {got:?}");
    assert!(
        sources[1].contains(&format!(" skipped={len} ")),
        "{sources:?}"
    );
    assert!(having(&got, "vref=v1").is_empty(), "history: {got:?}");
    assert_eq!(having(&got, " turn vref=v2 ").len(), 1, "{got:?}");
}

#[test]
fn a_transcripts_text_never_reaches_the_log() {
    let mut r = Rig::new("slog-tx-leak");
    r.touch();
    let _ = r.read(&LogWake::default());
    r.publish(1_000, "busy", 5);
    r.append(&[
        prompt("v1", 1_100, "SENTINEL-PROMPT please"),
        calls(
            "m1",
            1_200,
            &[("toolu_a", "Bash"), ("toolu_b", "SENTINELTOOL")],
        ),
        results(1_300, &[("toolu_a", false), ("toolu_b", false)]),
        notice(1_400, "SENTINEL-NOTICE You have hit your limit"),
        local_command(1_500, "SENTINEL-LOCAL"),
        turn_end(1_600, 500),
    ]);
    let _ = r.read(&LogWake::default());
    let g = r.tl.lock().unwrap();
    let log = g.slog().log();
    let content: Vec<String> = log
        .window()
        .map(|rec| aterm_slog::encode(rec, Tier::Content).expect("fits"))
        .collect();
    let bus: Vec<String> = log
        .window()
        .filter_map(|rec| aterm_slog::encode_bus(rec, Tier::Content, log.ep(), None).ok())
        .collect();
    let brief = super::super::render_brief(
        log,
        "s-1",
        aterm_slog::Node::SESSION,
        None,
        aterm_slog::DEFAULT_BUDGET,
        WALL0 + 2_000,
    );
    let carry = log
        .carry(aterm_slog::CarryBudget::SESSION, &[])
        .expect("carries")
        .lines
        .join("\n");
    for sentinel in [
        "SENTINEL-PROMPT",
        "SENTINEL-REPLY",
        "SENTINEL-INPUT",
        "SENTINEL-RESULT",
        "SENTINEL-NOTICE",
        "SENTINEL-LOCAL",
    ] {
        for (what, text) in [
            ("content", content.join("\n")),
            ("bus", bus.join("\n")),
            ("brief", brief.clone()),
            ("carry", carry.clone()),
        ] {
            assert!(!text.contains(sentinel), "{sentinel} in the {what}: {text}");
        }
    }
    assert!(
        content.iter().any(|l| l.contains(" tool=SENTINELTOOL ")),
        "{content:?}"
    );
    assert!(!bus.join("\n").contains("SENTINELTOOL"), "{bus:?}");
}

const THREAD: &str = "019a0000-0000-7000-8000-00000000000a";

fn rollout_rows(build: &str) -> String {
    rollout_rows_as(THREAD, build, 0, "turn")
}

fn rollout_rows_as(thread: &str, build: &str, late: u64, turn: &str) -> String {
    let at = |ms: u64| at(late + ms);
    let rows = [
        format!(
            r#"{{"timestamp":"{t}","type":"session_meta","payload":{{"session_id":"{thread}","id":"{thread}","timestamp":"{t}","cwd":"/private/tmp/x","originator":"codex-tui","cli_version":"{build}","source":"cli","history_mode":"paginated","base_instructions":{{"text":"SENTINEL-INSTRUCTIONS"}}}}}}"#,
            t = at(100)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"turn_context","payload":{{"turn_id":"{turn}-1","root_turn_id":"{turn}-1","cwd":"/private/tmp/x","approval_policy":"on-request","model":"gpt-5.5","effort":"high","user_instructions":"SENTINEL-PROMPT"}}}}"#,
            at(1_500)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call","name":"exec_command","arguments":"{{\"cmd\":\"SENTINEL-INPUT\"}}","call_id":"call_r0010"}}}}"#,
            at(1_800)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call_output","call_id":"call_r0010","output":"SENTINEL-RESULT"}}}}"#,
            at(2_000)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":1200,"cached_input_tokens":1000,"output_tokens":40,"reasoning_output_tokens":0,"total_tokens":1240}},"last_token_usage":{{"input_tokens":1200,"cached_input_tokens":1000,"output_tokens":40,"reasoning_output_tokens":0,"total_tokens":1240}}}}}}}}"#,
            at(2_500)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"task_complete","turn_id":"{turn}-1","last_agent_message":"SENTINEL-REPLY","duration_ms":1800}}}}"#,
            at(3_300)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"turn_context","payload":{{"turn_id":"{turn}-2","root_turn_id":"{turn}-2","cwd":"/private/tmp/x","approval_policy":"on-request","model":"gpt-5.5","effort":"high"}}}}"#,
            at(4_000)
        ),
        format!(
            r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"task_complete","turn_id":"{turn}-2","last_agent_message":null,"error":{{"message":"SENTINEL-NOTICE","codex_error_info":"usage_limit_exceeded"}},"duration_ms":32}}}}"#,
            at(4_100)
        ),
    ];
    rows.iter().map(|r| format!("{r}\n")).collect()
}

fn codex_rig(
    name: &str,
    agent: &str,
    build: &str,
) -> (
    Arc<Mutex<SessionTimeline>>,
    aterm_tempfile::TempDir,
    crate::self_claim::RolloutTarget,
    RolloutClaim,
) {
    codex_rig_opened(name, agent, build, 0)
}

fn codex_rig_opened(
    name: &str,
    agent: &str,
    build: &str,
    opened: u64,
) -> (
    Arc<Mutex<SessionTimeline>>,
    aterm_tempfile::TempDir,
    crate::self_claim::RolloutTarget,
    RolloutClaim,
) {
    let ctx = crate::session_store::test_handle(0xC0DE).ctx;
    let tl = Arc::clone(&ctx.timeline);
    let hand = hand_clock(&tl);
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        g.note_root_group(100);
    }
    hand.set(opened);
    super::super::tests::with_agent(&tl, agent);
    hand.set(10_000);
    let home = aterm_tempfile::Builder::new()
        .prefix(&format!("slog-rollout-{name}-"))
        .tempdir()
        .expect("temp dir");
    let day = home.path().join("sessions/2025/10/20");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(
        day.join(format!("rollout-2025-10-19T22-03-11-{THREAD}.jsonl")),
        rollout_rows(build),
    )
    .unwrap();
    let target = crate::self_claim::RolloutTarget {
        revision: 1,
        pid: 300,
        start: "Mon Oct 20 16:37:58 2026".to_string(),
        thread: THREAD.to_string(),
        home: home.path().to_path_buf(),
    };
    set_rollout_claim(&ctx, &target);
    (tl, home, target, RolloutClaim(ctx))
}

struct RolloutClaim(Arc<crate::SessionCtx>);

impl Drop for RolloutClaim {
    fn drop(&mut self) {
        crate::self_claim::forget(self.0.self_id.as_str());
    }
}

fn set_rollout_claim(ctx: &crate::SessionCtx, target: &crate::self_claim::RolloutTarget) {
    use aterm_agent::harness::self_claimed::{Copy, Record, Seen};
    let mut meta = ctx.meta.lock().unwrap();
    meta.supervisor_revision = target.revision;
    meta.supervisor = Some(crate::session_timeline::SupervisorClaim {
        holder: format!("codex-alab@{}", target.pid),
        conn: Some(1),
        expires_us: None,
        proven: Some(crate::self_claim::Proven {
            pid: target.pid,
            start: target.start.clone(),
        }),
        scoped: None,
    });
    crate::self_claim::set_copy_for_test(
        ctx.self_id.as_str(),
        Some(Copy {
            pid: target.pid,
            start: target.start.clone(),
            home: target.home.clone(),
            seen: Seen::Present(Record {
                pid: target.pid,
                session_id: Some(target.thread.clone()),
                resume_argv: None,
                env: Vec::new(),
                status: Some("busy".into()),
                exit: None,
                cwd: None,
                draft_empty: None,
                frame: None,
                initializing: false,
            }),
            resume: Err("captured-no-resume"),
        }),
    );
}

fn captured_start(_: u32) -> Option<String> {
    Some("Mon Oct 20 16:37:58 2026".to_string())
}

const MEASURED: Image = Image {
    start: captured_start,
    born_ms: unborn,
    build: alab_12521,
    group: own_group,
    task: no_task,
};

fn unborn(_: u32) -> Option<u64> {
    None
}

/// The rig's claimant leads its own group (300, [`super::super::tests::with_agent`]).
fn own_group(pid: u32) -> Option<i32> {
    i32::try_from(pid).ok()
}

fn no_task(_: u32) -> Option<String> {
    None
}

fn alab_12521(_: u32) -> Option<String> {
    Some("0.164.0-dev.12521+g3d3eb12d8".to_string())
}

#[test]
fn a_codex_alab_rollout_row_reaches_the_log_through_the_rollout_tap() {
    let (vendor, _vendor_home, vendor_target, vendor_claim) =
        codex_rig("vendor", "codex", "0.163.0");
    let (claude, _claude_home, claude_target, claude_claim) =
        codex_rig("claude", "claude", "0.164.0-dev.12521+g3d3eb12d8");
    let (tl, _home, target, claim) = codex_rig("alab", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let wake = LogWake::default();
    let ask = |session, claim: &RolloutClaim, target| {
        crate::codex_rollout_tap::request_as(session, &claim.0, target, &wake, MEASURED);
    };
    ask(0xC0DE_0001, &vendor_claim, vendor_target);
    ask(0xC0DE_0002, &claude_claim, claude_target);
    ask(0xC0DE_0003, &claim, target);
    let got = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the rollout's usage wall in the log",
        || {
            let got = lines_of(&tl);
            got.iter()
                .any(|l| l.contains(" out=wall:usage-session "))
                .then_some(got)
        },
    );
    let one = |needle: &str| {
        let found = having(&got, needle);
        assert_eq!(found.len(), 1, "{needle}: {got:#?}");
        found[0].clone()
    };
    assert!(
        one(" source field=transcript ").contains(" state=on "),
        "{got:#?}"
    );
    let open = one(" open t1 turn ");
    assert!(
        open.contains(" vref=turn-1 ") && open.contains(" src=transcript conf=exact "),
        "{open}"
    );
    let call = one(" open p");
    assert!(
        call.contains(" tool=exec_command ") && call.contains(" vref=call_r0010 "),
        "{call}"
    );
    assert_eq!(having(&got, " tool out=ok ").len(), 1, "{got:#?}");
    let close = one(" close t1 turn ");
    assert!(
        close.contains(" out=done ") && close.contains(" src=transcript conf=exact "),
        "{close}"
    );
    assert!(one(" open t2 turn ").contains(" vref=turn-2 "), "{got:#?}");
    assert!(
        one(" out=wall:usage-session ").contains(" amend t2 turn ")
            && one(" out=wall:usage-session ").contains(" src=transcript conf=exact "),
        "{got:#?}"
    );
    assert!(
        got.iter().any(|l| l.contains(" vdur_ms=1800 ")
            && l.contains(" tok_in=1200 ")
            && l.contains(" tok_out=40 ")),
        "{got:#?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" model=gpt-5.5 ") && l.contains(" ver=0.164.0-dev.12521 ")),
        "{got:#?}"
    );
    for l in &got {
        assert!(!l.contains("SENTINEL"), "rollout text in the log: {l}");
    }
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
    for (what, other) in [("vendor", &vendor), ("claude", &claude)] {
        let lines = lines_of(other);
        assert!(
            having(&lines, " source field=transcript ").is_empty()
                && having(&lines, "turn-1").is_empty(),
            "{what}: {lines:#?}"
        );
    }
}

fn turns_opened(got: &[String]) -> Vec<String> {
    got.iter()
        .filter(|l| l.contains(" turn ") && l.contains(" open t"))
        .filter_map(|l| l.split(" vref=").nth(1))
        .filter_map(|v| v.split(' ').next())
        .map(str::to_string)
        .collect()
}

#[test]
fn a_turn_before_the_runs_open_is_history_and_the_next_says_the_model() {
    let (tl, _home, target, claim) =
        codex_rig_opened("opened", "codex", "0.164.0-dev.12521+g3d3eb12d8", 3_500);
    let mut host = Host::default();
    let _ = host.ask(0xC0DE_0011, &claim.0, target, MEASURED);
    let got = lines_of(&tl);
    assert_eq!(turns_opened(&got), ["turn-2"], "{got:#?}");
    assert!(
        got.iter()
            .any(|l| l.contains(" model=gpt-5.5 ") && l.contains(" ver=0.164.0-dev.12521 ")),
        "{got:#?}"
    );
}

fn born_late(_: u32) -> Option<u64> {
    Some(WALL0 + 3_500)
}

fn alab_unmeasured(_: u32) -> Option<String> {
    Some("0.164.0-dev.99999+g0123abcd".to_string())
}

#[test]
fn the_host_floors_at_the_claimants_start_and_holds_it_to_its_build() {
    let (tl, _home, target, claim) = codex_rig("born", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let mut host = Host::default();
    let born = Image {
        start: captured_start,
        born_ms: born_late,
        build: alab_12521,
        group: own_group,
        task: no_task,
    };
    let _ = host.ask(0xC0DE_0021, &claim.0, target, born);
    let got = lines_of(&tl);
    assert_eq!(turns_opened(&got), ["turn-2"], "{got:#?}");
    let (tl, _home, target, claim) = codex_rig("running", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let unmeasured = Image {
        start: captured_start,
        born_ms: unborn,
        build: alab_unmeasured,
        group: own_group,
        task: no_task,
    };
    let _ = host.ask(0xC0DE_0022, &claim.0, target, unmeasured);
    let got = lines_of(&tl);
    assert!(
        having(&got, " source field=transcript ").is_empty() && turns_opened(&got).is_empty(),
        "{got:#?}"
    );
}

#[test]
fn each_ask_the_rollout_tap_answers_is_counted_after_its_records() {
    let fields = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().slog().rollout_fields();
    let last_seq = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().slog().log().last_seq();
    let mut host = Host::default();
    let (tl, _home, target, claim) =
        codex_rig("asks-order", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let before = last_seq(&tl);
    let _ = host.ask(0xC0DE_0040, &claim.0, target, MEASURED);
    let after = last_seq(&tl);
    assert_eq!(turns_opened(&lines_of(&tl)), ["turn-1", "turn-2"]);
    assert!(
        after > before,
        "the ask wrote its records: {before} -> {after}"
    );
    assert_eq!(
        tl.lock().unwrap().slog().rollout_counted_at(),
        after,
        "counted after every record the ask wrote (the log stood at {before} before it)"
    );
    assert_eq!(fields(&tl), "rollout=bound rollout_asks=1");
    let (tl, _home, target, claim) = codex_rig("asks", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    assert_eq!(fields(&tl), "rollout=- rollout_asks=0");
    assert_eq!(
        host.read_on(0xC0DE_0041, &claim.0, target.clone(), MEASURED),
        "bound"
    );
    assert_eq!(turns_opened(&lines_of(&tl)), ["turn-1", "turn-2"]);
    assert_eq!(
        fields(&tl),
        "rollout=- rollout_asks=0",
        "the read and apply alone count nothing"
    );
    let _ = host.ask(0xC0DE_0041, &claim.0, target.clone(), MEASURED);
    assert_eq!(fields(&tl), "rollout=bound rollout_asks=1");
    let _ = host.ask(0xC0DE_0041, &claim.0, target, MEASURED);
    assert_eq!(fields(&tl), "rollout=bound rollout_asks=2");
    assert_eq!(turns_opened(&lines_of(&tl)), ["turn-1", "turn-2"]);
    let unmeasured = Image {
        start: captured_start,
        born_ms: unborn,
        build: alab_unmeasured,
        group: own_group,
        task: no_task,
    };
    for ((name, agent, build, image, thread, word, asks), session) in [
        (
            "running",
            "codex",
            "0.164.0-dev.12521+g3d3eb12d8",
            unmeasured,
            None,
            "unmeasured",
            1,
        ),
        (
            "vendor",
            "codex",
            "0.163.0",
            MEASURED,
            None,
            "unmeasured",
            1,
        ),
        (
            "claude",
            "claude",
            "0.164.0-dev.12521+g3d3eb12d8",
            MEASURED,
            None,
            "-",
            0,
        ),
        (
            "thread",
            "codex",
            "0.164.0-dev.12521+g3d3eb12d8",
            MEASURED,
            Some("not-a-thread"),
            "-",
            0,
        ),
    ]
    .into_iter()
    .zip(0xC0DE_0050_u64..)
    {
        let (tl, _home, target, claim) = codex_rig(&format!("asks-{name}"), agent, build);
        let target = crate::self_claim::RolloutTarget {
            thread: thread.map_or(target.thread.clone(), str::to_string),
            ..target
        };
        set_rollout_claim(&claim.0, &target);
        let _ = host.ask(session, &claim.0, target, image);
        assert_eq!(
            fields(&tl),
            format!("rollout={word} rollout_asks={asks}"),
            "{name}"
        );
        let got = lines_of(&tl);
        assert!(turns_opened(&got).is_empty(), "{name}: {got:#?}");
    }
}

#[test]
fn a_stopped_rollout_tap_says_stopped_and_keeps_its_count() {
    use crate::codex_rollout_tap::{Ask, follow_ask_as};
    let fields = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().slog().rollout_fields();
    let (never, _never_home, _, never_claim) =
        codex_rig("stop-never", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let (tl, _home, target, claim) = codex_rig("stop", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let wake = LogWake::default();
    follow_ask_as(
        Ask::Read(target.clone()),
        0xC0DE_0062,
        &claim.0,
        &wake,
        MEASURED,
    );
    follow_ask_as(Ask::Stop, 0xC0DE_0061, &never_claim.0, &wake, MEASURED);
    follow_ask_as(Ask::Stop, 0xC0DE_0062, &claim.0, &wake, MEASURED);
    let said = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the sweep's stop said in status",
        || {
            let said = fields(&tl);
            said.starts_with("rollout=stopped ").then_some(said)
        },
    );
    assert_eq!(said, "rollout=stopped rollout_asks=1");
    assert_eq!(turns_opened(&lines_of(&tl)), ["turn-1", "turn-2"]);
    assert_eq!(fields(&never), "rollout=- rollout_asks=0", "never asked");
    follow_ask_as(Ask::Read(target), 0xC0DE_0062, &claim.0, &wake, MEASURED);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the tap asked again answers afresh",
        || (fields(&tl) == "rollout=bound rollout_asks=2").then_some(()),
    );
}

#[test]
fn a_new_target_is_read_by_a_new_tap() {
    const NEXT: &str = "019a0000-0000-7000-8000-00000000000b";
    let (tl, home, target, claim) = codex_rig("target", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let mut host = Host::default();
    let _ = host.ask(0xC0DE_0031, &claim.0, target.clone(), MEASURED);
    assert_eq!(turns_opened(&lines_of(&tl)), ["turn-1", "turn-2"]);
    let _ = host.ask(0xC0DE_0031, &claim.0, target.clone(), MEASURED);
    assert_eq!(turns_opened(&lines_of(&tl)), ["turn-1", "turn-2"]);
    std::fs::write(
        home.path().join(format!(
            "sessions/2025/10/20/rollout-2025-10-19T22-03-11-{NEXT}.jsonl"
        )),
        rollout_rows_as(NEXT, "0.164.0-dev.12521+g3d3eb12d8", 5_000, "next"),
    )
    .unwrap();
    let next = crate::self_claim::RolloutTarget {
        revision: target.revision + 1,
        thread: NEXT.to_string(),
        ..target
    };
    set_rollout_claim(&claim.0, &next);
    let _ = host.ask(0xC0DE_0031, &claim.0, next, MEASURED);
    let got = lines_of(&tl);
    assert_eq!(
        turns_opened(&got),
        ["turn-1", "turn-2", "next-1", "next-2"],
        "{got:#?}"
    );

    for after_io in [false, true] {
        for change in ["run", "released", "renewed", "thread", "home"] {
            let (tl, home, target, claim) =
                codex_rig("stale", "codex", "0.164.0-dev.12521+g3d3eb12d8");
            std::fs::write(
                home.path().join(format!(
                    "sessions/2025/10/20/rollout-2025-10-19T22-03-11-{THREAD}.jsonl"
                )),
                rollout_rows_as(THREAD, "0.164.0-dev.12521+g3d3eb12d8", 20_000, "stale"),
            )
            .unwrap();
            let before = tl
                .lock()
                .unwrap()
                .slog()
                .transcript_arm(Source::Rollout)
                .unwrap()
                .run;
            let change_owner = || match change {
                "run" => {
                    let mut g = tl.lock().unwrap();
                    assert!(g.note_foreground_group(301));
                    assert!(g.set_program(301, Some("codex".into())));
                    g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, 301);
                    assert_ne!(
                        g.slog().transcript_arm(Source::Rollout).unwrap().run,
                        before
                    );
                }
                "released" => {
                    let mut meta = claim.0.meta.lock().unwrap();
                    meta.supervisor = None;
                    meta.supervisor_revision += 1;
                }
                "renewed" | "thread" | "home" => {
                    let mut changed = target.clone();
                    match change {
                        "renewed" => changed.revision += 1,
                        "thread" => changed.thread = NEXT.into(),
                        "home" => changed.home = home.path().join("other-home"),
                        _ => unreachable!(),
                    }
                    set_rollout_claim(&claim.0, &changed);
                }
                _ => unreachable!(),
            };
            let mut host = Host::default();
            let result = host.queued(
                0xC0DE_0032,
                &claim.0,
                target.clone(),
                MEASURED,
                (
                    || {
                        if !after_io {
                            change_owner();
                        }
                    },
                    || {
                        if after_io {
                            change_owner();
                        }
                    },
                ),
            );
            assert!(
                result.is_none_or(|r| r.disarm && r.events == 0),
                "{change}, after_io={after_io}: {result:?}"
            );
            assert_eq!(
                tl.lock().unwrap().slog().rollout_fields(),
                "rollout=stale rollout_asks=1",
                "the refused queued ask is counted after its guarded read/apply"
            );
            let got = lines_of(&tl);
            assert!(
                turns_opened(&got).is_empty()
                    && having(&got, " source field=transcript ").is_empty(),
                "{change}, after_io={after_io}: {got:#?}"
            );
            if change == "run" {
                assert!(
                    host.ask(0xC0DE_0032, &claim.0, target.clone(), MEASURED)
                        .is_none()
                );
                let current = crate::self_claim::RolloutTarget {
                    pid: 301,
                    revision: target.revision + 1,
                    ..target
                };
                set_rollout_claim(&claim.0, &current);
                let applied = host.ask(0xC0DE_0032, &claim.0, current, MEASURED).unwrap();
                assert!(!applied.disarm);
                assert_eq!(turns_opened(&lines_of(&tl)), ["stale-1", "stale-2"]);
            }
        }
    }
    let (tl, _home, target, claim) =
        codex_rig("unread-start", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let mut host = Host::default();
    assert!(
        host.ask(
            0xC0DE_0033,
            &claim.0,
            target.clone(),
            Image {
                start: |_| None,
                ..MEASURED
            }
        )
        .is_none()
    );
    assert!(
        host.ask(
            0xC0DE_0033,
            &claim.0,
            target,
            Image {
                start: |_| Some("another process".into()),
                ..MEASURED
            }
        )
        .is_none()
    );
    assert!(turns_opened(&lines_of(&tl)).is_empty());
}

/// THE SURVEY'S FACTS: the tap publishes what the rollout says (model, effort, cwd) to the
/// timeline's codex slot for the claimant's group in front, whatever codex-alab build wrote it
/// (the steps' fold is gated to measured builds, these facts are not); a group not in front takes
/// nothing; the tap's stop takes them away.
#[test]
fn the_rollout_tap_publishes_the_survey_facts_for_the_claimants_group_whatever_its_build() {
    let survey = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().codex_survey().cloned();
    let fields = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().slog().rollout_fields();
    let mut host = Host::default();
    let unmeasured = Image {
        build: alab_unmeasured,
        ..MEASURED
    };
    for (name, image, word) in [
        ("measured", MEASURED, "bound"),
        ("unmeasured", unmeasured, "unmeasured"),
    ] {
        let (tl, _home, target, claim) = codex_rig(
            &format!("facts-{name}"),
            "codex",
            "0.164.0-dev.12521+g3d3eb12d8",
        );
        assert_eq!(survey(&tl), None, "{name}");
        let _ = host.ask(0xC0DE_0070, &claim.0, target.clone(), image);
        assert_eq!(
            fields(&tl),
            format!("rollout={word} rollout_asks=1"),
            "{name}"
        );
        let got = survey(&tl).unwrap_or_else(|| panic!("{name}: no facts published"));
        assert_eq!(got.facts.model.as_deref(), Some("gpt-5.5"), "{name}");
        assert_eq!(got.facts.effort.as_deref(), Some("high"), "{name}");
        assert_eq!(
            got.facts.cwd.as_deref(),
            Some(std::path::Path::new("/private/tmp/x")),
            "{name}"
        );
        assert_eq!(got.task, None, "{name}");
        host.stop(0xC0DE_0070, &claim.0);
        assert_eq!(survey(&tl), None, "{name}: the stop forgets them");
        assert_eq!(fields(&tl), "rollout=stopped rollout_asks=1", "{name}");
    }

    let (tl, _home, target, claim) =
        codex_rig("facts-behind", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let behind = Image {
        group: |_| Some(301),
        ..MEASURED
    };
    let _ = host.ask(0xC0DE_0071, &claim.0, target.clone(), behind);
    assert_eq!(survey(&tl), None, "a group not in front takes nothing");
    let _ = host.ask(0xC0DE_0071, &claim.0, target, MEASURED);
    assert_eq!(
        survey(&tl).and_then(|s| s.facts.model),
        Some("gpt-5.5".into()),
        "facts no group took are offered again, though the rollout did not move"
    );
    let tasked = Image {
        task: |_| Some("port the parser".into()),
        ..MEASURED
    };
    let (tl, _home, target, claim) =
        codex_rig("facts-tasked", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let _ = host.ask(0xC0DE_0072, &claim.0, target, tasked);
    assert_eq!(
        survey(&tl).and_then(|s| s.task),
        Some("port the parser".into()),
        "a worker's task rides with the facts"
    );
}
