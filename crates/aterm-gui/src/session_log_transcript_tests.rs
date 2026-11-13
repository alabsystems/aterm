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
    let parts: Vec<String> = std::iter::once(r#"{"type":"text","text":"SENTINEL-REPLY"}"#.to_string())
        .chain(tools.iter().map(|(id, name)| {
            format!(
                r#"{{"type":"tool_use","id":"{id}","name":"{name}","input":{{"command":"SENTINEL-INPUT"}}}}"#
            )
        }))
        .collect();
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"id":"{msg}","model":"claude-opus-5-5","role":"assistant","content":[{}],"usage":{{"input_tokens":3,"output_tokens":20,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}},"uuid":"a-{msg}","timestamp":"{}","effort":"high","version":"2.1.300"}}"#,
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

/// Fix E1 (E2E-1): a reply too fast for the screen's sampling is never seen busy, so its
/// prompt row, read at `state=partial` (the fixture's 2.1.300 is no measured build), opens
/// the turn and its end row closes it.
#[test]
fn a_reply_the_screen_never_saw_busy_is_a_turn_through_the_footer_read() {
    let mut r = Rig::new("slog-tx-fast");
    r.touch();
    let first = r.read(&LogWake::default());
    assert!(!first.disarm, "a Claude run takes the steps: {first:?}");
    r.publish(1_000, "idle", 5);
    r.append(&[
        prompt("v1", 1_100, "a fast prompt"),
        calls("m1", 1_200, &[]),
        turn_end(1_300, 200),
    ]);
    r.hand.set(1_400);
    let read = r.read(&LogWake::default());
    assert!(read.owed, "the log moved: {read:?}");
    let got = r.lines();
    let source = having(&got, " source field=transcript ");
    assert!(source[0].contains(" state=partial "), "{source:?}");
    let open = having(&got, " open t1 turn ");
    assert_eq!(open.len(), 1, "{got:?}");
    assert!(
        open[0].contains(" vref=v1 ") && open[0].contains(" src=transcript conf=strong "),
        "{open:?}"
    );
    let close = having(&got, " close t1 turn ");
    assert_eq!(close.len(), 1, "{got:?}");
    assert!(
        close[0].contains(" out=done ") && close[0].contains(" by=vendor src=transcript "),
        "{close:?}"
    );
    assert_eq!(having(&got, " open t").len(), 1, "one turn: {got:?}");
    assert!(having(&got, "SENTINEL").is_empty(), "{got:?}");
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
        (false, None),
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

/// A Claude Code 2.1.310 turn's rows, its end written while the dynamic workflow it started runs
/// on (`pendingWorkflowCount`), as the live transcript of the cruise stall wrote them.
fn workflow_turn(ms: u64) -> Vec<String> {
    let v = r#""version":"2.1.310""#;
    vec![
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"promptId":"v1","type":"user","message":{{"role":"user","content":"fix it"}},"uuid":"u-v1","timestamp":"{}",{v}}}"#,
            at(ms)
        ),
        calls("m1", ms + 100, &[("toolu_wf", "Workflow")]).replace(r#""version":"2.1.300""#, v),
        results(ms + 200, &[("toolu_wf", false)]),
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":400,"messageCount":9,"pendingWorkflowCount":3,"timestamp":"{}","uuid":"d-{ms}","isMeta":false,{v}}}"#,
            at(ms + 400)
        ),
    ]
}

/// The turn Claude Code opens on its own when background work it started ends (a
/// `<task-notification>` row, `origin.kind=task-notification`), and that turn's end.
fn notification_turn(ms: u64) -> Vec<String> {
    let v = r#""version":"2.1.310""#;
    vec![
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"promptId":"v2","type":"user","message":{{"role":"user","content":"<task-notification>\n<task-id>bdqlme1ij</task-id>\n<status>completed</status>\n</task-notification>"}},"uuid":"u-v2","timestamp":"{}","origin":{{"kind":"task-notification","producer":"session-task"}},"promptSource":"system","turnOrigin":"task_notification",{v}}}"#,
            at(ms)
        ),
        calls("m2", ms + 100, &[("toolu_b", "Bash")]).replace(r#""version":"2.1.300""#, v),
        results(ms + 200, &[("toolu_b", false)]),
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":300,"messageCount":12,"timestamp":"{}","uuid":"d-{ms}","isMeta":false,{v}}}"#,
            at(ms + 300)
        ),
    ]
}

/// THE CRUISE STALL OF 2026-10-24..27, replayed: a Claude Code turn ends while its own background
/// work runs on, its screen (captured live: `Waiting for 3 dynamic workflows to finish` under an
/// open composer) published as `rest`; the work later wakes it with a notification turn. Returns
/// the log's lines.
fn turns_over_background_work(name: &str, rest: &'static str) -> Vec<String> {
    let mut r = Rig::new(name);
    r.touch();
    let _ = r.read(&LogWake::default());
    r.publish(1_000, "busy", 5);
    r.append(&workflow_turn(1_100));
    let _ = r.read(&LogWake::default());
    r.publish(2_000, rest, 6);
    r.hand.set(3_100);
    let _ = r.tl.lock().unwrap().slog_tick();
    // The workflow ends: Claude Code's own wake runs a turn, and it ends over the work left.
    r.publish(10_050, "busy", 7);
    r.append(&notification_turn(10_000));
    let _ = r.read(&LogWake::default());
    r.publish(11_000, rest, 8);
    r.hand.set(12_100);
    let _ = r.tl.lock().unwrap().slog_tick();
    r.lines()
}

#[test]
fn a_turn_that_ends_under_its_own_background_work_closes_and_so_does_its_wake() {
    use crate::presence::{Cursor, agent_verdict};
    use aterm_phase::prompt::fixtures::{WAITING_WORKFLOWS_2_1_310, screen};
    let rows = screen(WAITING_WORKFLOWS_2_1_310);
    let caret = rows.iter().position(|r| r.starts_with('❯'));
    let verdict = agent_verdict(
        Some("claude"),
        false,
        None,
        &rows,
        Cursor::At(caret),
        "",
        std::time::Instant::now(),
    );
    let rest = verdict.word();
    assert_eq!(rest, "idle", "the captured screen's turn end is published");
    let got = turns_over_background_work("slog-tx-bg-rest", rest);
    for t in ["t1", "t2"] {
        let closed = having(&got, &format!(" close {t} turn "));
        assert!(
            closed.len() == 1
                && closed[0].contains(" out=done ")
                && closed[0].contains(" src=screen "),
            "{t} ends at its turn end: {got:#?}"
        );
    }
    assert_eq!(having(&got, " open t").len(), 2, "two turns: {got:#?}");
    assert_eq!(
        having(&got, " amend t1 turn vdur_ms=400 ").len(),
        1,
        "{got:#?}"
    );
    assert_eq!(
        having(&got, " amend t2 turn vdur_ms=300 ").len(),
        1,
        "the wake's end is its own turn's: {got:#?}"
    );
    assert_eq!(having(&got, " amend t2 turn vref=v2 ").len(), 1, "{got:#?}");
    time_order_holds(&got).unwrap();

    // CONTROL: the same rows with the screen read as the reader's phase alone (`busy`, what the
    // stalled build published) reproduce the stall: one turn, never closed. The
    // second vendor's end remains ambiguous evidence without replacing its metrics.
    let reader_phase = aterm_phase::read(Some("claude"), &rows, None).phase;
    assert_eq!(reader_phase, aterm_phase::Phase::Busy);
    let stalled = turns_over_background_work("slog-tx-bg-busy", reader_phase.name());
    assert!(having(&stalled, " close t").is_empty(), "{stalled:#?}");
    assert_eq!(having(&stalled, " open t").len(), 1, "{stalled:#?}");
    assert_eq!(
        having(&stalled, " amend t1 turn vdur_ms=").len(),
        1,
        "{stalled:#?}"
    );
    let second = having(&stalled, " meta field=vendor-turn vref=v2 state=done ");
    assert_eq!(second.len(), 1, "{stalled:#?}");
    assert!(
        second[0].contains(" parent=t1 why=ambiguous vdur_ms=300 "),
        "{stalled:#?}"
    );
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
    let (tl, _hand, home, target, claim) = codex_rig_hand(name, agent, build, opened, &[]);
    (tl, home, target, claim)
}

/// [`codex_rig_opened`] with its clock's hand, and the acts `early` (`(rule, holder, ms)`)
/// recorded before its run opened.
fn codex_rig_hand(
    name: &str,
    agent: &str,
    build: &str,
    opened: u64,
    early: &[(&str, &str, u64)],
) -> (
    Arc<Mutex<SessionTimeline>>,
    super::super::tests::Hand,
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
        for (rule, holder, ms) in early {
            hand.set(*ms);
            let now = g.slog().now();
            g.slog_mut()
                .self_claim_act(rule, holder, aterm_slog::Actor::Ctl, now)
                .expect("the act mark");
        }
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
    (tl, hand, home, target, RolloutClaim(ctx))
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
            lifetime: None,
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
                goal: None,
                background_terminals: None,
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
    version: alab_12521,
    group: own_group,
    task: no_task,
    record: no_record,
};

/// A claim's target reads no record at its asks: any read here is a read-only target's.
fn no_record(
    _: &std::path::Path,
    _: u32,
    _: &str,
) -> Option<aterm_agent::harness::self_claimed::Seen> {
    None
}

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
    let ask = |session, claim: &RolloutClaim, target: crate::self_claim::RolloutTarget| {
        crate::codex_rollout_tap::request_as(session, &claim.0, target.into(), &wake, MEASURED);
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
    let lines = lines_of(&vendor);
    let source = having(&lines, " source field=transcript ");
    assert!(
        !source.is_empty() && source.iter().all(|l| l.contains(" state=partial ")),
        "a rollout a build nobody measured wrote is read partial: {lines:#?}"
    );
    partial_turns_are_its_own_and_strong(&lines);
    let lines = lines_of(&claude);
    assert!(
        having(&lines, " source field=transcript ").is_empty()
            && having(&lines, "turn-1").is_empty(),
        "NEGATIVE CONTROL: a claude run takes no rollout: {lines:#?}"
    );
}

/// A ROLLOUT READ PARTIAL WITH NO CLAIM'S TURN RUNNING opens the turns it reads (fix E1), as an
/// unmeasured Claude Code's transcript opens the turns its screen missed: at `conf=strong`, never
/// `exact`.
fn partial_turns_are_its_own_and_strong(got: &[String]) {
    assert_eq!(turns_opened(got), ["turn-1", "turn-2"], "{got:#?}");
    let moved = got
        .iter()
        .filter(|l| l.contains(" turn ") && (l.contains(" open t") || l.contains(" close t")));
    for l in moved {
        assert!(l.contains(" src=transcript conf=strong "), "{l}: {got:#?}");
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
        version: alab_12521,
        group: own_group,
        task: no_task,
        record: no_record,
    };
    let _ = host.ask(0xC0DE_0021, &claim.0, target, born);
    let got = lines_of(&tl);
    assert_eq!(turns_opened(&got), ["turn-2"], "{got:#?}");
    let (tl, _home, target, claim) = codex_rig("running", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    let unmeasured = Image {
        start: captured_start,
        born_ms: unborn,
        build: alab_unmeasured,
        version: alab_unmeasured,
        group: own_group,
        task: no_task,
        record: no_record,
    };
    let _ = host.ask(0xC0DE_0022, &claim.0, target, unmeasured);
    let got = lines_of(&tl);
    let source = having(&got, " source field=transcript ");
    assert!(
        !source.is_empty() && source.iter().all(|l| l.contains(" state=partial ")),
        "a writer nobody measured is read partial: {got:#?}"
    );
    partial_turns_are_its_own_and_strong(&got);
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
        version: alab_unmeasured,
        group: own_group,
        task: no_task,
        record: no_record,
    };
    for ((name, agent, build, image, thread, word, asks), session) in [
        (
            "running",
            "codex",
            "0.164.0-dev.12521+g3d3eb12d8",
            unmeasured,
            None,
            "partial",
            1,
        ),
        ("vendor", "codex", "0.163.0", MEASURED, None, "partial", 1),
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
        if word == "partial" {
            partial_turns_are_its_own_and_strong(&got);
        } else {
            assert!(turns_opened(&got).is_empty(), "{name}: {got:#?}");
        }
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
        Ask::Read(target.clone().into()),
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
    follow_ask_as(
        Ask::Read(target.into()),
        0xC0DE_0062,
        &claim.0,
        &wake,
        MEASURED,
    );
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the tap asked again answers afresh",
        || (fields(&tl) == "rollout=bound rollout_asks=2").then_some(()),
    );
}

fn claimed(
    tl: &Arc<Mutex<SessionTimeline>>,
    state: aterm_core::terminal::program_state::ProgramStateWord,
) {
    tl.lock().unwrap().note_program_state(
        Some(aterm_core::terminal::program_state::ProgramState {
            state,
            kind: None,
            app: aterm_core::terminal::program_state::ProgramApp::Codex,
        }),
        1,
    );
}

const UNMEASURED: Image = Image {
    start: captured_start,
    born_ms: unborn,
    build: alab_unmeasured,
    version: alab_unmeasured,
    group: own_group,
    task: no_task,
    record: no_record,
};

/// The rig's rollout, rewritten to `text`.
fn rewrite_rollout(home: &std::path::Path, text: &str) {
    let path = home
        .join("sessions/2025/10/20")
        .join(format!("rollout-2025-10-19T22-03-11-{THREAD}.jsonl"));
    std::fs::write(path, text).unwrap();
}

/// The rig's run through two turns, each claimed working then idle, its rollout (written by
/// `build`, its claimant on `image`) read as each turn's rows land; the log after it.
fn two_claimed_turns(name: &str, build: &str, image: Image) -> (Vec<String>, String) {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    let (tl, home, target, claim) = codex_rig(name, "codex", build);
    let hand = hand_clock(&tl);
    hand.set(10_000);
    let rows: Vec<String> = rollout_rows_as(THREAD, build, 9_000, "turn")
        .lines()
        .map(|l| format!("{l}\n"))
        .collect();
    let mut host = Host::default();
    claimed(&tl, W::Working);
    rewrite_rollout(home.path(), &rows[..6].concat());
    hand.set(12_400);
    let _ = host.ask(0xC0DE_0071, &claim.0, target.clone(), image);
    hand.set(12_500);
    claimed(&tl, W::Idle);
    hand.set(12_900);
    claimed(&tl, W::Working);
    rewrite_rollout(home.path(), &rows.concat());
    hand.set(13_200);
    let _ = host.ask(0xC0DE_0071, &claim.0, target, image);
    hand.set(13_300);
    claimed(&tl, W::Done);
    let fields = tl.lock().unwrap().slog().rollout_fields();
    (lines_of(&tl), fields)
}

fn turn_closes(got: &[String]) -> Vec<&String> {
    got.iter()
        .filter(|l| l.contains(" turn ") && l.contains(" close t"))
        .collect()
}

fn turn_opens(got: &[String]) -> Vec<&String> {
    got.iter()
        .filter(|l| l.contains(" turn ") && l.contains(" open t"))
        .collect()
}

/// AN UNMEASURED CODEX-ALAB'S TURNS REACH THE LOG: a claimant on a build nobody measured has a
/// turn for each OSC 7501 working-to-rest claim (`src=engine conf=claim`), and its rollout, read
/// `state=partial` (`status rollout=partial`), names each turn, hangs its call from it and gives
/// it its duration and tokens (`src=transcript conf=strong`), never opening or closing one.
/// NEGATIVE CONTROL: on a measured build the rollout opens and closes its own turns
/// (`conf=exact`, `rollout=bound`) and the claims open none past the bind: the claim's turn open
/// before it is the one the first prompt names, never a second turn for that span.
#[test]
fn an_unmeasured_codex_alabs_turns_are_its_claims_filled_by_its_rollout() {
    let (got, fields) = two_claimed_turns("partial", "0.164.0-dev.99999+g0123abcd", UNMEASURED);
    assert_eq!(fields, "rollout=partial rollout_asks=2");
    let source = having(&got, " source field=transcript ");
    assert!(
        source.len() == 1 && source[0].contains(" state=partial "),
        "{got:#?}"
    );
    let opens = turn_opens(&got);
    assert_eq!(opens.len(), 2, "{got:#?}");
    assert!(
        opens.iter().all(|l| l.contains(" src=engine conf=claim ")),
        "{got:#?}"
    );
    for vref in ["turn-1", "turn-2"] {
        let named = having(&got, &format!(" turn vref={vref} "));
        let named: Vec<_> = named
            .into_iter()
            .filter(|l| l.contains(" amend t"))
            .collect();
        assert!(
            named.len() == 1
                && named[0].contains(" amend t")
                && named[0].contains(" src=transcript conf=strong "),
            "{vref}: {got:#?}"
        );
    }
    let call = having(&got, " tool=exec_command ");
    assert!(
        call.len() == 1 && call[0].contains(" src=transcript conf=strong "),
        "{got:#?}"
    );
    assert!(
        got.iter().any(|l| l.contains(" amend t1 turn ")
            && l.contains(" vdur_ms=1800 ")
            && l.contains(" tok_in=1200 ")
            && l.contains(" tok_out=40 ")),
        "{got:#?}"
    );
    let closes = turn_closes(&got);
    assert!(
        closes.len() == 2 && closes.iter().all(|l| l.contains(" src=engine conf=claim ")),
        "the claim closes each turn: {got:#?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" model=gpt-5.5 ") && l.contains(" ver=0.164.0-dev.99999 ")),
        "{got:#?}"
    );
    for l in &got {
        assert!(!l.contains("SENTINEL"), "rollout text in the log: {l}");
    }
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));

    let (got, fields) = two_claimed_turns("measured", "0.164.0-dev.12521+g3d3eb12d8", MEASURED);
    assert_eq!(fields, "rollout=bound rollout_asks=2");
    let opens = turn_opens(&got);
    assert_eq!(opens.len(), 2, "NEGATIVE CONTROL: one turn each: {got:#?}");
    assert!(
        opens[0].contains(" src=engine conf=claim ")
            && opens[1].contains(" vref=turn-2 ")
            && opens[1].contains(" src=transcript conf=exact "),
        "the first claimed before the bind, the second the rollout's own: {got:#?}"
    );
    assert!(
        having(&got, " vref=turn-1 ")[0].contains(" amend t1 turn "),
        "{got:#?}"
    );
    let closes = turn_closes(&got);
    assert!(
        closes.len() == 2
            && closes
                .iter()
                .all(|l| l.contains(" src=transcript conf=exact ")),
        "the rollout closes its own: {got:#?}"
    );
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
}

/// THE CLAIM'S TURNS WHERE NO ROLLOUT STEP IS ADMITTED: a codex-alab whose rollout's
/// `session_meta` is of a shape the fold does not know (`status rollout=unknown-meta`) still has
/// a turn for each working-to-rest claim, `src=engine conf=claim`, with its time.
/// NEGATIVE CONTROL: the same claims in front of a Claude Code, whose screen opens its turns,
/// open none.
#[test]
fn a_refused_rollouts_codex_alab_still_has_its_claimed_turns() {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    let (tl, home, target, claim) = codex_rig("schema", "codex", "0.164.0-dev.12521+g3d3eb12d8");
    rewrite_rollout(
        home.path(),
        &rollout_rows("0.164.0-dev.12521+g3d3eb12d8").replacen(
            "\"history_mode\":\"paginated\"",
            "\"history_mode\":\"segmented\"",
            1,
        ),
    );
    let hand = hand_clock(&tl);
    hand.set(10_000);
    claimed(&tl, W::Working);
    let mut host = Host::default();
    hand.set(11_000);
    let _ = host.ask(0xC0DE_0081, &claim.0, target, MEASURED);
    hand.set(14_000);
    claimed(&tl, W::Idle);
    assert_eq!(
        tl.lock().unwrap().slog().rollout_fields(),
        "rollout=unknown-meta rollout_asks=1"
    );
    let got = lines_of(&tl);
    assert!(
        having(&got, " source field=transcript ").is_empty(),
        "{got:#?}"
    );
    let opens = turn_opens(&got);
    assert!(
        opens.len() == 1 && opens[0].contains(" src=engine conf=claim "),
        "{got:#?}"
    );
    let closes = having(&got, " turn out=done ");
    assert!(
        closes.len() == 1 && closes[0].contains(" src=engine conf=claim "),
        "{got:#?}"
    );
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));

    let (claude, _home, _, _claim) =
        codex_rig("schema-claude", "claude", "0.164.0-dev.12521+g3d3eb12d8");
    let hand = hand_clock(&claude);
    hand.set(10_000);
    claimed(&claude, W::Working);
    hand.set(14_000);
    claimed(&claude, W::Idle);
    assert!(
        turn_opens(&lines_of(&claude)).is_empty(),
        "NEGATIVE CONTROL: {:#?}",
        lines_of(&claude)
    );
}

/// A CLAIM'S REST ENDS ITS TURN WHATEVER THE ROLLOUT HAS READ: an unmeasured codex-alab rests and
/// claims its next turn before the tap has read the first turn's `task_complete` (the tap reads
/// at most once a second). The second turn continues nothing (no `cont=`), the second prompt
/// names it, and the first turn's end, read late, gives the first turn its duration and tokens.
/// NEGATIVE CONTROL: the second turn takes none of the first turn's numbers.
#[test]
fn a_claimed_rest_read_before_the_rollouts_end_continues_no_turn() {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    let build = "0.164.0-dev.99999+g0123abcd";
    let (tl, home, target, claim) = codex_rig("rest-first", "codex", build);
    let hand = hand_clock(&tl);
    hand.set(10_000);
    let rows: Vec<String> = rollout_rows_as(THREAD, build, 9_000, "turn")
        .lines()
        .map(|l| format!("{l}\n"))
        .collect();
    let mut host = Host::default();
    claimed(&tl, W::Working);
    rewrite_rollout(home.path(), &rows[..5].concat());
    hand.set(12_400);
    let _ = host.ask(0xC0DE_0091, &claim.0, target.clone(), UNMEASURED);
    hand.set(12_500);
    claimed(&tl, W::Idle);
    hand.set(12_900);
    claimed(&tl, W::Working);
    rewrite_rollout(home.path(), &rows.concat());
    hand.set(13_200);
    let _ = host.ask(0xC0DE_0091, &claim.0, target, UNMEASURED);
    hand.set(13_300);
    claimed(&tl, W::Done);
    let got = lines_of(&tl);
    let opens = turn_opens(&got);
    assert!(
        opens.len() == 2 && opens.iter().all(|l| l.contains(" src=engine conf=claim ")),
        "{got:#?}"
    );
    assert!(having(&got, " cont=").is_empty(), "{got:#?}");
    let named = having(&got, " amend t2 turn vref=turn-2 ");
    assert!(
        named.len() == 1 && named[0].contains(" amend t2 turn "),
        "{got:#?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" amend t1 turn ") && l.contains(" vdur_ms=1800 ")),
        "{got:#?}"
    );
    assert!(
        !got.iter()
            .any(|l| l.contains(" amend t2 turn ") && l.contains(" vdur_ms=1800 ")),
        "NEGATIVE CONTROL: the first turn's numbers are its own: {got:#?}"
    );
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
}

/// A STOPPED TAP HANDS A MEASURED CODEX-ALAB'S TURNS BACK TO ITS CLAIMS: while the rollout is
/// read exactly (`rollout=bound`) it opens and closes the turns and the claims stand aside; once
/// the sweep stops the tap (`rollout=stopped`: the claim released, as when Autopilot goes off)
/// the claims open and close the turns again, and the tap asked again catches up on the gap's
/// rows, which name the claim's turn rather than opening a second one, after which the claims
/// stand aside again. NEGATIVE CONTROL: before the stop, the same claims open nothing.
#[test]
fn a_stopped_rollout_tap_hands_a_measured_runs_turns_back_to_its_claims() {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    let build = "0.164.0-dev.12521+g3d3eb12d8";
    let (tl, home, target, claim) = codex_rig("stopped-claims", "codex", build);
    let hand = hand_clock(&tl);
    let mut host = Host::default();
    hand.set(10_000);
    let _ = host.ask(0xC0DE_00A1, &claim.0, target.clone(), MEASURED);
    let read = turn_opens(&lines_of(&tl)).len();
    assert_eq!(read, 2, "{:#?}", lines_of(&tl));
    hand.set(15_000);
    claimed(&tl, W::Working);
    hand.set(16_000);
    claimed(&tl, W::Idle);
    assert_eq!(
        turn_opens(&lines_of(&tl)).len(),
        read,
        "NEGATIVE CONTROL: the rollout read exactly holds the turns: {:#?}",
        lines_of(&tl)
    );
    host.stop(0xC0DE_00A1, &claim.0);
    assert_eq!(
        tl.lock().unwrap().slog().rollout_fields(),
        "rollout=stopped rollout_asks=1"
    );
    hand.set(17_000);
    claimed(&tl, W::Working);
    hand.set(19_500);
    claimed(&tl, W::Idle);
    let got = lines_of(&tl);
    let opens = turn_opens(&got);
    assert!(
        opens.len() == read + 1 && opens[read].contains(" src=engine conf=claim "),
        "{got:#?}"
    );
    let gap: String = rollout_rows_as(THREAD, build, 16_000, "next")
        .lines()
        .skip(1)
        .take(5)
        .map(|l| format!("{l}\n"))
        .collect();
    rewrite_rollout(home.path(), &(rollout_rows(build) + &gap));
    hand.set(20_000);
    let _ = host.ask(0xC0DE_00A1, &claim.0, target, MEASURED);
    hand.set(21_000);
    claimed(&tl, W::Working);
    let got = lines_of(&tl);
    assert_eq!(
        tl.lock().unwrap().slog().rollout_fields(),
        "rollout=bound rollout_asks=2"
    );
    assert_eq!(
        turn_opens(&got).len(),
        read + 1,
        "the gap's rows name the claim's turn, and the claims stand aside again: {got:#?}"
    );
    let named = having(&got, " amend t3 turn vref=next-1 ");
    assert!(
        named.len() == 1
            && named[0].contains(" amend t3 turn ")
            && named[0].contains(" conf=exact "),
        "{got:#?}"
    );
    assert!(
        got.iter()
            .any(|l| l.contains(" amend t3 turn ") && l.contains(" vdur_ms=1800 ")),
        "{got:#?}"
    );
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
}

/// `help status` names exactly the words `rollout=` can print: every [`RolloutWord`] (its own
/// and a tap refusal's, each enum and its word table one generated list, so a new word cannot
/// reach `status` unenumerated) and [`ROLLOUT_NEVER_ASKED`].
#[test]
fn help_status_names_exactly_the_rollout_words_aterm_writes() {
    use std::collections::BTreeSet;
    let written: BTreeSet<&str> = RolloutWord::words().chain([ROLLOUT_NEVER_ASKED]).collect();
    let help = aterm_types::control_verbs::spec("status")
        .expect("the status verb")
        .help_line();
    let lists: Vec<&str> = help.split("rollout=<").skip(1).collect();
    assert_eq!(lists.len(), 1, "help status names one rollout= vocabulary");
    let named: BTreeSet<&str> = lists[0]
        .split_once('>')
        .expect("the vocabulary closes")
        .0
        .split('|')
        .collect();
    let unwritten: Vec<&&str> = named.difference(&written).collect();
    let unnamed: Vec<&&str> = written.difference(&named).collect();
    assert!(
        unwritten.is_empty() && unnamed.is_empty(),
        "help status names rollout words nothing writes {unwritten:?} and leaves out words \
         aterm writes {unnamed:?}"
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
/// (an unmeasured build's steps read partial, these facts whole); a group not in front takes
/// nothing; the tap's stop takes them away.
#[test]
fn the_rollout_tap_publishes_the_survey_facts_for_the_claimants_group_whatever_its_build() {
    let survey = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().codex_survey().cloned();
    let fields = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().slog().rollout_fields();
    let mut host = Host::default();
    let unmeasured = Image {
        build: alab_unmeasured,
        version: alab_unmeasured,
        ..MEASURED
    };
    for (name, image, word) in [
        ("measured", MEASURED, "bound"),
        ("unmeasured", unmeasured, "partial"),
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
    let sessions = target.home.join("alab").join("sessions");
    std::fs::create_dir_all(&sessions).expect("alab sessions dir");
    std::fs::write(
        sessions.join(format!("{}.json", target.pid)),
        format!(
            r#"{{"v":1,"pid":{},"sessionId":"{}","status":"busy","goal":"Ship native protocol"}}"#,
            target.pid, target.thread
        ),
    )
    .expect("session record with goal");
    let _ = host.ask(0xC0DE_0072, &claim.0, target, tasked);
    assert_eq!(
        survey(&tl).and_then(|s| s.task),
        Some("port the parser".into()),
        "a worker's task rides with the facts"
    );
    assert_eq!(
        survey(&tl).and_then(|s| s.goal),
        Some("Ship native protocol".into()),
        "the session record's native goal rides with the facts"
    );
}

/// THE RECORD'S COUNT: the tap publishes the background terminals the claimant's own session
/// record names (`backgroundTerminals`, read with the goal at every ask) on a slot of its own,
/// whether or not a rollout is found; a count alone publishes no survey facts, so a tab with no
/// rollout, task or goal keeps its screen's read. Each ask reads it afresh, a record that shows an
/// exit names none, and the stop forgets it.
#[test]
fn the_rollout_tap_publishes_the_records_count_apart_from_the_survey_facts() {
    let survey = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().codex_survey().cloned();
    let terminals = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().codex_terminals();
    let mut host = Host::default();
    for rollout in [true, false] {
        let (tl, home, target, claim) = codex_rig(
            &format!("terminals-{rollout}"),
            "codex",
            "0.164.0-dev.12521+g3d3eb12d8",
        );
        if !rollout {
            std::fs::remove_dir_all(home.path().join("sessions")).expect("no rollout");
        }
        let path = aterm_agent::harness::self_claimed::record_path(&target.home, target.pid);
        std::fs::create_dir_all(path.parent().expect("dir")).expect("alab sessions dir");
        let record = |tail: &str| {
            std::fs::write(
                &path,
                format!(
                    r#"{{"v":1,"pid":{},"sessionId":"{}","status":"idle"{tail}}}"#,
                    target.pid, target.thread
                ),
            )
            .expect("the session record");
        };
        let mut ask = || {
            assert!(
                host.ask(0xC0DE_0074, &claim.0, target.clone(), MEASURED)
                    .is_some(),
                "rollout {rollout}: the claimant is current"
            );
        };

        record(r#","backgroundTerminals":2"#);
        ask();
        assert_eq!(terminals(&tl), Some(2), "rollout {rollout}");
        assert_eq!(
            survey(&tl).is_some(),
            rollout,
            "rollout {rollout}: a count alone publishes no survey facts"
        );
        record(r#","backgroundTerminals":0"#);
        ask();
        assert_eq!(terminals(&tl), Some(0), "rollout {rollout}: read afresh");
        record(r#","exit":"signal:9","backgroundTerminals":2"#);
        ask();
        assert_eq!(
            terminals(&tl),
            None,
            "rollout {rollout}: a record that shows an exit names none"
        );
        record(r#","backgroundTerminals":2"#);
        ask();
        assert_eq!(terminals(&tl), Some(2), "rollout {rollout}");
        host.stop(0xC0DE_0074, &claim.0);
        assert_eq!(
            terminals(&tl),
            None,
            "rollout {rollout}: the stop forgets it"
        );
    }
}

/// The record a test planted under `home` for `pid`, read with no look at the process (the rig's
/// pid names none): a read-only target's record at its ask.
fn planted(
    home: &std::path::Path,
    pid: u32,
    _: &str,
) -> Option<aterm_agent::harness::self_claimed::Seen> {
    use aterm_agent::harness::self_claimed::{Seen, parse, record_path};
    let text = std::fs::read_to_string(record_path(home, pid)).ok()?;
    Some(parse(&text).map_or_else(Seen::Unreadable, Seen::Present))
}

/// Plants `pid`'s session record under `home`, naming `thread`, as codex-alab writes it with or
/// without a claim.
fn plant_record(home: &std::path::Path, pid: u32, thread: &str) {
    let path = aterm_agent::harness::self_claimed::record_path(home, pid);
    std::fs::create_dir_all(path.parent().expect("dir")).expect("alab sessions dir");
    std::fs::write(
        &path,
        format!(r#"{{"v":1,"pid":{pid},"sessionId":"{thread}","status":"busy","exit":null}}"#),
    )
    .expect("the session record");
}

/// A RELEASED CLAIM'S PROCESS IS READ ON, READ ONLY: a codex-alab tab whose proven claim was
/// given back while its process runs and its record shows no end (Autopilot Off, the harness
/// off) keeps its tap. Its read-only target ([`crate::self_claim::TapTarget::Watched`]) reads
/// the rollout on, so the turns written after the release reach the log, the survey facts stay
/// published and `status rollout=` stays `bound`; a `/new` written into the record taps the new
/// thread's rollout, and rows the old thread gains after it are never read. FAIL CLOSED: an ask
/// whose record names no thread, or does not parse, taps nothing (`no-thread`) and takes the
/// published facts away, and the next ask whose record names the thread again binds it again.
/// NEGATIVE CONTROL: an ask whose record read finds its process not live is `stale` and takes
/// the published facts away.
#[test]
fn a_released_claims_process_is_read_on_read_only() {
    use crate::self_claim::{Proven, TapTarget, Watched};
    const NEXT: &str = "019a0000-0000-7000-8000-00000000000b";
    const BUILD: &str = "0.164.0-dev.12521+g3d3eb12d8";
    let survey = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().codex_survey().cloned();
    let fields = |tl: &Arc<Mutex<SessionTimeline>>| tl.lock().unwrap().slog().rollout_fields();
    let (tl, home, target, claim) = codex_rig("released", "codex", BUILD);
    let ctx = &claim.0;
    let day = home.path().join("sessions/2025/10/20");
    let rollout = |thread: &str| day.join(format!("rollout-2025-10-19T22-03-11-{thread}.jsonl"));
    let append = |thread: &str, late: u64, turn: &str| {
        let rows: String = rollout_rows_as(thread, BUILD, late, turn)
            .lines()
            .skip(1)
            .map(|row| format!("{row}\n"))
            .collect();
        std::fs::OpenOptions::new()
            .append(true)
            .open(rollout(thread))
            .expect("the rollout")
            .write_all(rows.as_bytes())
            .expect("rows appended");
    };
    let mut host = Host::default();
    let _ = host.ask(0xC0DE_0080, ctx, target.clone(), MEASURED);
    assert_eq!(turns_opened(&lines_of(&tl)), ["turn-1", "turn-2"]);
    assert!(survey(&tl).is_some(), "the claim's tap publishes");

    plant_record(&target.home, target.pid, THREAD);
    let revision = {
        let mut meta = ctx.meta.lock().unwrap();
        meta.supervisor = None;
        meta.supervisor_revision += 1;
        meta.supervisor_revision
    };
    let proven = Proven {
        pid: target.pid,
        start: target.start.clone(),
    };
    assert!(crate::self_claim::released_for_test(
        ctx, &proven, revision, true, None
    ));
    assert_eq!(crate::self_claim::copy_of_tab(ctx.self_id.as_str()), None);
    let watched = TapTarget::Watched(Watched {
        revision,
        pid: target.pid,
        start: target.start.clone(),
        home: target.home.clone(),
    });
    let read_planted = Image {
        record: planted,
        ..MEASURED
    };
    append(THREAD, 5_000, "after");
    let _ = host.ask(0xC0DE_0080, ctx, watched.clone(), read_planted);
    assert_eq!(fields(&tl), "rollout=bound rollout_asks=2");
    assert_eq!(
        turns_opened(&lines_of(&tl)),
        ["turn-1", "turn-2", "after-1", "after-2"],
        "the turns after the release reach the log"
    );
    assert_eq!(
        survey(&tl).and_then(|s| s.facts.model),
        Some("gpt-5.5".into()),
        "the facts stay published"
    );

    std::fs::write(rollout(NEXT), rollout_rows_as(NEXT, BUILD, 10_000, "next")).unwrap();
    plant_record(&target.home, target.pid, NEXT);
    let _ = host.ask(0xC0DE_0080, ctx, watched.clone(), read_planted);
    append(THREAD, 15_000, "old");
    let _ = host.ask(0xC0DE_0080, ctx, watched.clone(), read_planted);
    let got = lines_of(&tl);
    assert_eq!(
        turns_opened(&got),
        ["turn-1", "turn-2", "after-1", "after-2", "next-1", "next-2"],
        "the new thread is tapped and the old one never again: {got:#?}"
    );
    assert_eq!(fields(&tl), "rollout=bound rollout_asks=4");
    assert!(
        survey(&tl).is_some(),
        "the new thread's facts are published"
    );

    let record = aterm_agent::harness::self_claimed::record_path(&target.home, target.pid);
    for (asks, text) in [
        (
            5,
            format!(
                r#"{{"v":1,"pid":{},"sessionId":null,"status":"busy","exit":null}}"#,
                target.pid
            ),
        ),
        (6, "not a record".to_string()),
    ] {
        std::fs::write(&record, text).expect("the session record");
        let _ = host.ask(0xC0DE_0080, ctx, watched.clone(), read_planted);
        assert_eq!(
            fields(&tl),
            format!("rollout=no-thread rollout_asks={asks}")
        );
        assert_eq!(
            survey(&tl),
            None,
            "a record naming no thread keeps no facts"
        );
    }
    plant_record(&target.home, target.pid, NEXT);
    let _ = host.ask(0xC0DE_0080, ctx, watched.clone(), read_planted);
    assert_eq!(fields(&tl), "rollout=bound rollout_asks=7");
    assert!(
        survey(&tl).is_some(),
        "the thread named again is read again"
    );
    assert_eq!(turns_opened(&lines_of(&tl)), turns_opened(&got));

    let gone = Image {
        record: |_, _, _| None,
        ..MEASURED
    };
    let _ = host.ask(0xC0DE_0080, ctx, watched, gone);
    assert_eq!(fields(&tl), "rollout=stale rollout_asks=8");
    assert_eq!(survey(&tl), None, "a process not live keeps no facts");
}

/// [`rollout_rows`] with its one command's completion (`item_completed`, `exit_code` = `code`)
/// between the call and its output, as codex-alab's paginated rollout persists it.
fn rollout_rows_exit(build: &str, code: i64) -> String {
    let mut out = String::new();
    for row in rollout_rows(build).lines() {
        if row.contains("\"function_call_output\"") {
            out.push_str(&format!(
                r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"item_completed","thread_id":"{THREAD}","turn_id":"turn-1","item":{{"type":"commandExecution","id":"call_r0010","command":["SENTINEL-INPUT"],"aggregated_output":"SENTINEL-RESULT","status":"failed","exit_code":{code}}}}}}}"#,
                at(1_900)
            ));
            out.push('\n');
        }
        out.push_str(row);
        out.push('\n');
    }
    out
}

/// The rig's rollout rewritten to `rows`.
fn rollout_is(home: &aterm_tempfile::TempDir, rows: &str) {
    std::fs::write(
        home.path().join(format!(
            "sessions/2025/10/20/rollout-2025-10-19T22-03-11-{THREAD}.jsonl"
        )),
        rows,
    )
    .expect("the rollout");
}

/// GAP 20, END TO END: on a measured codex-alab build, a command whose completion says it exited
/// non-zero closes its step `out=error`, and its turn's close counts it. NEGATIVE CONTROL: the
/// same rows of an upstream build (`0.163.0`, its gate shut) read `out=ok` and count none.
#[test]
fn a_codex_alab_failed_command_closes_out_error_and_counts_errs() {
    for (name, build, failed) in [
        ("exit-alab", "0.164.0-dev.12521+g3d3eb12d8", true),
        ("exit-vendor", "0.163.0", false),
    ] {
        let (tl, home, target, claim) = codex_rig(name, "codex", build);
        rollout_is(&home, &rollout_rows_exit(build, 3));
        let mut host = Host::default();
        let _ = host.ask(0xC0DE_2001, &claim.0, target, MEASURED);
        let got = lines_of(&tl);
        let close = having(&got, " close t1 turn ");
        assert_eq!(close.len(), 1, "{name}: {got:#?}");
        if failed {
            assert_eq!(having(&got, " tool out=error ").len(), 1, "{got:#?}");
            assert!(having(&got, " tool out=ok ").is_empty(), "{got:#?}");
            assert!(close[0].contains(" errs=1 "), "{}", close[0]);
        } else {
            assert_eq!(having(&got, " tool out=ok ").len(), 1, "{got:#?}");
            assert!(having(&got, " errs=").is_empty(), "{got:#?}");
        }
        for l in &got {
            assert!(!l.contains("SENTINEL"), "rollout text in the log: {l}");
        }
        time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
    }
}

fn vref(s: &str) -> aterm_agent::harness::steps::StepId {
    aterm_agent::harness::steps::StepId::new(s).expect("a step id")
}

/// The codex-alab claim's holder, as the rig's claim names it.
const HOLDER: &str = "codex-alab@300";

/// One rollout row of a harness-turn script ([`Ev::Read`]).
#[derive(Clone, Copy, Debug)]
enum Row<'a> {
    /// A turn's `turn_context`: its prompt.
    Prompt(&'a str),
    /// Its `alab_autopilot` `task_started`.
    Harness(&'a str),
    /// The running turn's `task_complete`.
    End,
}

/// How a harness-turn script's run ends ([`Ev::End`]).
#[derive(Clone, Copy, Debug)]
enum RunEnd {
    /// The front goes back to the shell.
    Shell,
    /// The run's group names another program.
    Program,
    /// The session closes.
    Session,
}

/// One thing a harness-turn script does ([`scripted`]), in order; the rig's clock moves to its
/// test ms first.
#[derive(Clone, Copy, Debug)]
enum Ev<'a> {
    /// `holder` records the act `rule` (`meta set harness_act`).
    Act(&'a str, &'a str, u64),
    /// The tap reads the rows `(ms, row)`, short of the rollout's end.
    Read(u64, &'a [(u64, Row<'a>)]),
    /// The tap reads to the rollout's end and finds no new row.
    Idle(u64),
    /// The loop ticks the session log ([`SessionLog::tick`]).
    Tick(u64),
    /// The run ends.
    End(u64, RunEnd),
    /// The log's wake ([`SessionLog::wake`]) is then this, in test ms.
    Wakes(Option<u64>),
    /// The log's harness read wait ([`SessionLog::harness_wait`]) is then from this, in test ms.
    Waits(Option<u64>),
    /// The last batch moved the read wait, or did not ([`Applied::waits`]).
    Moved(bool),
}

/// A codex rig whose run opened at `opened` (test ms) after the acts `early` (`(rule, holder,
/// ms)`), run through `script`: its log's lines and every act mark's seq, in log order.
fn scripted(
    name: &str,
    opened: u64,
    early: &[(&str, &str, u64)],
    script: &[Ev<'_>],
) -> (Vec<String>, Vec<u64>) {
    let build = "0.164.0-dev.12521+g3d3eb12d8";
    let (tl, hand, _home, _target, _claim) = codex_rig_hand(name, "codex", build, opened, early);
    let step = |off: u64, ms: u64, ev: StepEvent| Step {
        pos: aterm_agent::harness::steps::StepPos {
            fp: 1,
            id: None,
            off,
            at_ms: WALL0 + ms,
        },
        state: StepState::On,
        ev,
    };
    let mut off = 0;
    let mut last = None::<Applied>;
    for ev in script {
        match *ev {
            Ev::Act(rule, holder, ms) => {
                hand.set(ms);
                let mut g = tl.lock().unwrap();
                let now = g.slog().now();
                g.slog_mut()
                    .self_claim_act(rule, holder, aterm_slog::Actor::Ctl, now)
                    .expect("the act mark");
            }
            Ev::Idle(ms) => {
                hand.set(ms);
                let applied = apply_batch_timed(
                    &tl,
                    Source::Rollout,
                    &StepBatch::default(),
                    (true, Some(WALL0 + ms)),
                    &mut || 0,
                    &mut |_, _| {},
                );
                assert!(!applied.disarm, "{ev:?}: {applied:?}");
                last = Some(applied);
            }
            Ev::Tick(ms) => {
                hand.set(ms);
                let _ = tl.lock().unwrap().slog_tick();
            }
            Ev::End(ms, how) => {
                hand.set(ms);
                let mut g = tl.lock().unwrap();
                match how {
                    RunEnd::Shell => assert!(g.note_foreground_group(100)),
                    RunEnd::Program => {
                        assert!(g.set_program(300, Some("bash".into())));
                        let pgid = g.agent().program_pgid;
                        let _ = g.sweep_slog(
                            &crate::session_log::BlockRead::default(),
                            false,
                            100,
                            pgid,
                        );
                    }
                    RunEnd::Session => {
                        let now = g.slog().now();
                        g.slog_mut().close_session(
                            aterm_slog::SessionOut::ShellExit,
                            aterm_slog::Actor::Nobody,
                            "-",
                            Some(0),
                            now,
                        );
                    }
                }
            }
            Ev::Wakes(ms) => assert_eq!(tl.lock().unwrap().slog_wake(), ms, "{ev:?}"),
            Ev::Waits(ms) => {
                let since = tl
                    .lock()
                    .unwrap()
                    .slog()
                    .harness_wait()
                    .map(|(w, _)| w.since - WALL0);
                assert_eq!(since, ms, "{ev:?}");
            }
            Ev::Moved(moved) => assert_eq!(last.map(|a| a.waits), Some(moved), "{ev:?}"),
            Ev::Read(ms, rows) => {
                hand.set(ms);
                let mut steps = Vec::new();
                if off == 0 {
                    steps.push(step(0, 0, StepEvent::Bind { len: 0 }));
                }
                for (at, row) in rows {
                    off += 1;
                    let ev = match row {
                        Row::Prompt(v) => StepEvent::Prompt { vref: vref(v) },
                        Row::Harness(v) => StepEvent::HarnessTurn { vref: vref(v) },
                        Row::End => StepEvent::TurnEnd {
                            vdur_ms: 1,
                            tok_in: 0,
                            tok_out: 0,
                        },
                    };
                    steps.push(step(off, *at, ev));
                }
                let batch = StepBatch {
                    steps,
                    ..StepBatch::default()
                };
                let applied = apply_batch_timed(
                    &tl,
                    Source::Rollout,
                    &batch,
                    (true, None),
                    &mut || 0,
                    &mut |_, _| {},
                );
                assert!(!applied.disarm, "{ev:?}: {applied:?}");
                last = Some(applied);
            }
        }
    }
    let seqs = tl
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .filter(|r| r.op == aterm_slog::Op::Mark && r.k == aterm_slog::Kind::Act)
        .map(|r| r.seq)
        .collect();
    (lines_of(&tl), seqs)
}

/// The `turn_by` amend lines of `got`.
fn harnessed(got: &[String]) -> Vec<String> {
    having(got, " turn_by=harness ")
        .into_iter()
        .cloned()
        .collect()
}

/// The `cause=` every amend of `turn` (`t<n>`) in `got` names.
fn causes(got: &[String], turn: &str) -> Vec<u64> {
    having(got, &format!(" amend {turn} turn "))
        .into_iter()
        .filter_map(|l| {
            l.split(' ')
                .find_map(|w| w.strip_prefix("cause="))
                .and_then(|c| c.parse().ok())
        })
        .collect()
}

/// The rows of one autopilot turn `v` opening at `ms`.
const fn harness_rows(v: &str, ms: u64) -> [(u64, Row<'_>); 2] {
    [(ms, Row::Prompt(v)), (ms, Row::Harness(v))]
}

/// GAP 25 AT THE HOST: a harness turn's step amends its turn `turn_by=harness`, and the act that
/// typed it (the nearest act within five seconds of the turn's open whose rule types an
/// Autopilot turn, that no turn names already and that no other turn opened nearer) reaches the
/// turn on its own amend once a later read settles it. NEGATIVE CONTROLS: an act six seconds
/// off, an act nearer another turn and an act a turn named already pair with nothing; and the
/// real tap on a build that showed no `alab_autopilot` (12521) writes no `turn_by=` from such a
/// row.
#[test]
fn a_harness_turn_reaches_the_log_with_the_act_that_began_it() {
    let t1 = harness_rows("turn-1", 5_000);
    let end = [(8_000, Row::End)];
    let (got, seqs) = scripted(
        "harness-act",
        0,
        &[],
        &[
            Ev::Act("continue@v1", HOLDER, 5_020),
            Ev::Read(5_050, &t1),
            Ev::Read(8_100, &end),
        ],
    );
    let amend = harnessed(&got);
    assert_eq!(amend.len(), 1, "{got:#?}");
    assert!(
        amend[0].contains(" amend t1 turn turn_by=harness by=vendor src=transcript conf=exact"),
        "{amend:?}"
    );
    let caused = having(&got, &format!(" cause={} ", seqs[0]));
    assert_eq!(caused.len(), 1, "{got:#?}");
    assert!(
        caused[0].contains(&format!(
            " amend t1 turn cause={} cause_k=rec by=vendor src=transcript conf=exact",
            seqs[0]
        )),
        "{caused:?}"
    );
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
    let far = harness_rows("turn-1", 7_000);
    let nearer = [
        (5_000, Row::Prompt("turn-1")),
        (9_000, Row::Prompt("turn-2")),
        (9_000, Row::Harness("turn-2")),
    ];
    let late = [(12_000, Row::End)];
    for (name, script, why) in [
        (
            "harness-far",
            &[
                Ev::Act("continue@v1", HOLDER, 1_000),
                Ev::Read(7_050, &far),
                Ev::Read(12_100, &late),
            ][..],
            "an act six seconds off",
        ),
        (
            "harness-nearer",
            &[
                Ev::Act("continue@v1", HOLDER, 5_500),
                Ev::Read(9_050, &nearer),
                Ev::Read(12_100, &late),
            ][..],
            "an act another turn opened nearer",
        ),
    ] {
        let (got, _) = scripted(name, 0, &[], script);
        assert_eq!(harnessed(&got).len(), 1, "{why}: {got:#?}");
        assert!(having(&got, " cause=").is_empty(), "{why}: {got:#?}");
    }
    let tie = [
        (5_000, Row::Prompt("turn-1")),
        (5_000, Row::Harness("turn-1")),
        (5_600, Row::Prompt("turn-2")),
        (5_600, Row::Harness("turn-2")),
    ];
    let (got, seqs) = scripted(
        "harness-named",
        0,
        &[],
        &[
            Ev::Act("continue@v1", HOLDER, 5_300),
            Ev::Read(5_700, &tie),
            Ev::Read(8_100, &end),
        ],
    );
    assert_eq!(harnessed(&got).len(), 2, "{got:#?}");
    assert_eq!(causes(&got, "t1"), [seqs[0]], "{got:#?}");
    assert!(
        causes(&got, "t2").is_empty(),
        "an act a turn named already pairs no other: {got:#?}"
    );
    let build = "0.164.0-dev.12521+g3d3eb12d8";
    let (tl, home, target, claim) = codex_rig("harness-gated", "codex", build);
    let started = format!(
        r#"{{"timestamp":"{}","type":"event_msg","payload":{{"type":"task_started","turn_id":"turn-1","turn_attribution":{{"turn_id":"turn-1","turn_trigger":"alab_autopilot"}}}}}}"#,
        at(1_400)
    );
    let rows: String = rollout_rows(build)
        .lines()
        .flat_map(|l| {
            let lead = (l.contains("\"turn_context\"") && l.contains("\"turn-1\""))
                .then(|| started.clone());
            lead.into_iter().chain(std::iter::once(l.to_string()))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(rows.contains("alab_autopilot"), "the plant took");
    rollout_is(&home, &rows);
    let mut host = Host::default();
    let _ = host.ask(0xC0DE_2502, &claim.0, target, MEASURED);
    let got = lines_of(&tl);
    assert_eq!(having(&got, " open t1 turn ").len(), 1, "{got:#?}");
    assert!(
        having(&got, " turn_by=").is_empty(),
        "12521 showed no alab_autopilot: {got:#?}"
    );
}

/// A HARNESS TURN'S ACT, WHICHEVER LANDS FIRST (gap 25): codex-alab records its act once its turn
/// started, and aterm logs it after the act barrier, so the tap's read of the turn's rows can
/// come before or after the act mark. Either way the turn names that act, once: logged before
/// the read, logged after it, or held two seconds and paired only once a read passes the time a
/// nearer turn would have opened. An act logged just before the open waits until the clock
/// passes as far after it, so a nearer act logged after the read is the one the turn takes.
#[test]
fn a_harness_turns_act_is_its_cause_whichever_lands_first() {
    let t1 = harness_rows("turn-1", 5_000);
    let end = [(8_000, Row::End)];
    let person = [(30_000, Row::Prompt("turn-2"))];
    let (got, seqs) = scripted(
        "harness-nearer-later",
        0,
        &[],
        &[
            Ev::Act("continue@v1", HOLDER, 4_700),
            Ev::Read(5_050, &t1),
            Ev::Act("continue@v1", HOLDER, 5_150),
            Ev::Read(8_100, &end),
        ],
    );
    assert_eq!(causes(&got, "t1"), [seqs[1]], "{got:#?}");
    for (name, script) in [
        (
            "harness-act-first",
            &[
                Ev::Act("continue@v1", HOLDER, 5_020),
                Ev::Read(5_050, &t1),
                Ev::Read(8_100, &end),
            ][..],
        ),
        (
            "harness-read-first",
            &[
                Ev::Read(5_050, &t1),
                Ev::Act("continue@v1", HOLDER, 5_150),
                Ev::Read(8_100, &end),
            ][..],
        ),
        (
            "harness-held",
            &[
                Ev::Read(5_050, &t1),
                Ev::Act("continue@v1", HOLDER, 7_000),
                Ev::Read(8_100, &end),
                Ev::Read(30_100, &person),
            ][..],
        ),
    ] {
        let (got, seqs) = scripted(name, 0, &[], script);
        assert_eq!(harnessed(&got).len(), 1, "{name}: {got:#?}");
        assert_eq!(causes(&got, "t1"), [seqs[0]], "{name}: {got:#?}");
        assert!(causes(&got, "t2").is_empty(), "{name}: {got:#?}");
        time_order_holds(&got).unwrap_or_else(|e| panic!("{name}: {e}: {got:#?}"));
    }
}

/// A LATER TURN KEEPS ITS OWN ACT (gap 25): one read holds two Autopilot turns, and only the
/// later one's act was logged; that act is the later turn's, which opened nearer it, though the
/// earlier turn's step is read first. The same holds where the earlier turn was read before the
/// act landed and the later one after, and where a read between them had not yet reached the
/// later turn's rows: the earlier turn waits for a read past the time a nearer turn would open.
#[test]
fn a_later_turn_in_the_read_keeps_its_own_act() {
    let both = [
        (5_000, Row::Prompt("turn-1")),
        (5_000, Row::Harness("turn-1")),
        (8_000, Row::End),
        (9_000, Row::Prompt("turn-2")),
        (9_000, Row::Harness("turn-2")),
        (9_600, Row::End),
    ];
    let first = harness_rows("turn-1", 5_000);
    let rest = [
        (8_000, Row::End),
        (9_000, Row::Prompt("turn-2")),
        (9_000, Row::Harness("turn-2")),
        (9_600, Row::End),
    ];
    for (name, script) in [
        (
            "harness-batch",
            &[
                Ev::Act("continue@v1", HOLDER, 9_150),
                Ev::Read(9_700, &both),
            ][..],
        ),
        (
            "harness-across",
            &[
                Ev::Read(5_050, &first),
                Ev::Act("continue@v1", HOLDER, 9_150),
                Ev::Read(9_700, &rest),
            ][..],
        ),
        (
            "harness-unread",
            &[
                Ev::Read(5_050, &first),
                Ev::Act("continue@v1", HOLDER, 9_150),
                Ev::Read(9_200, &rest[..1]),
                Ev::Read(9_700, &rest[1..]),
            ][..],
        ),
    ] {
        let (got, seqs) = scripted(name, 0, &[], script);
        assert_eq!(harnessed(&got).len(), 2, "{name}: {got:#?}");
        assert_eq!(causes(&got, "t2"), [seqs[0]], "{name}: {got:#?}");
        assert!(causes(&got, "t1").is_empty(), "{name}: {got:#?}");
    }
}

/// ONLY THIS RUN'S HOLDER BEGINS ITS TURNS (gap 25): an act another codex-alab process recorded
/// (`codex-alab@301`, where the run's group is 300) and one this process recorded before this
/// run opened pair with none of this run's turns. NEGATIVE CONTROL: the same process's act after
/// the run opened, though before the turn's open, pairs.
#[test]
fn only_the_runs_own_holder_begins_its_turns() {
    let t1 = harness_rows("turn-1", 5_000);
    let end = [(8_000, Row::End)];
    let (got, _) = scripted(
        "harness-other-pid",
        0,
        &[],
        &[
            Ev::Act("continue@v1", "codex-alab@301", 5_020),
            Ev::Read(5_050, &t1),
            Ev::Read(8_100, &end),
        ],
    );
    assert_eq!(harnessed(&got).len(), 1, "{got:#?}");
    assert!(having(&got, " cause=").is_empty(), "{got:#?}");
    let t1 = harness_rows("turn-1", 4_000);
    let (got, _) = scripted(
        "harness-before-run",
        3_000,
        &[("continue@v1", HOLDER, 2_000)],
        &[Ev::Read(4_050, &t1), Ev::Read(8_100, &end)],
    );
    assert_eq!(harnessed(&got).len(), 1, "{got:#?}");
    assert!(having(&got, " cause=").is_empty(), "{got:#?}");
    let (got, seqs) = scripted(
        "harness-in-run",
        3_000,
        &[],
        &[
            Ev::Act("continue@v1", HOLDER, 3_500),
            Ev::Read(4_050, &t1),
            Ev::Read(8_100, &end),
        ],
    );
    assert_eq!(causes(&got, "t1"), [seqs[0]], "{got:#?}");
}

/// A BOX'S ANSWER BEGINS NO AUTOPILOT TURN (gap 25): `approve-all@v1` types nothing, and the plan
/// box's yes (`plan-approve@v2`) starts its implementing turn through codex's TUI as a person's
/// (`turn_trigger=user`), so neither is the act of an `alab_autopilot` turn.
#[test]
fn a_boxs_answer_begins_no_autopilot_turn() {
    let t1 = harness_rows("turn-1", 5_000);
    let end = [(8_000, Row::End)];
    for rule in ["approve-all@v1", "plan-approve@v2"] {
        let (got, _) = scripted(
            "harness-box",
            0,
            &[],
            &[
                Ev::Act(rule, HOLDER, 5_020),
                Ev::Read(5_050, &t1),
                Ev::Read(8_100, &end),
            ],
        );
        assert_eq!(harnessed(&got).len(), 1, "{rule}: {got:#?}");
        assert!(having(&got, " cause=").is_empty(), "{rule}: {got:#?}");
    }
}

/// A HARNESS TURN NO LATER ROW FOLLOWS STILL TAKES ITS ACT (gap 25): the act lands two seconds
/// after the open, the turn ends a second later, and the rollout writes nothing more. The read
/// that ended the turn leaves it waiting on the read side, from the act's time plus its distance
/// from the open plus the writer's lag (11 s), and arms no wake, since the clock alone settles
/// nothing: the read wait goes to the rollout tap's ask. A read to the rollout's end begun then
/// pairs it; one begun a tenth of a second sooner does not, since a turn stamped up to the act's
/// time plus that distance could still have been on its way to the file, and it moves the wait
/// (the ask takes it as new and asks once more). A read that stops short of the end, though begun
/// after the wait's time, leaves the wait as it was and arms no wake. An act logged after the
/// turn's last read is waited for on the clock until its window shuts (the open plus five
/// seconds), and the tick there leaves the turn waiting on the read side. An act just before the
/// open waits on the clock alone: the loop's tick at its wake pairs it with no read.
#[test]
fn a_harness_turn_no_later_row_follows_takes_its_act_at_a_read_to_the_end() {
    let t1 = harness_rows("turn-1", 5_000);
    let end = [(8_000, Row::End)];
    let lead = [
        Ev::Read(5_050, &t1),
        Ev::Wakes(Some(10_000)),
        Ev::Waits(None),
        Ev::Act("continue@v1", HOLDER, 7_000),
        Ev::Read(8_100, &end),
        Ev::Moved(true),
        Ev::Wakes(None),
        Ev::Waits(Some(11_000)),
    ];
    let (got, _) = scripted(
        "harness-idle-early",
        0,
        &[],
        &[
            &lead[..],
            &[
                Ev::Idle(10_900),
                Ev::Moved(true),
                Ev::Wakes(None),
                Ev::Waits(Some(11_000)),
                Ev::Read(11_500, &[]),
                Ev::Moved(false),
                Ev::Wakes(None),
                Ev::Waits(Some(11_000)),
            ],
        ]
        .concat(),
    );
    assert_eq!(harnessed(&got).len(), 1, "{got:#?}");
    assert!(causes(&got, "t1").is_empty(), "{got:#?}");
    let (got, seqs) = scripted(
        "harness-idle",
        0,
        &[],
        &[
            &lead[..],
            &[
                Ev::Idle(11_000),
                Ev::Moved(true),
                Ev::Wakes(None),
                Ev::Waits(None),
            ],
        ]
        .concat(),
    );
    assert_eq!(causes(&got, "t1"), [seqs[0]], "{got:#?}");
    let quick = [
        (5_000, Row::Prompt("turn-1")),
        (5_000, Row::Harness("turn-1")),
        (5_100, Row::End),
    ];
    let (got, seqs) = scripted(
        "harness-act-late",
        0,
        &[],
        &[
            Ev::Read(5_150, &quick),
            Ev::Act("continue@v1", HOLDER, 6_900),
            Ev::Wakes(Some(10_000)),
            Ev::Tick(10_000),
            Ev::Wakes(None),
            Ev::Waits(Some(10_800)),
            Ev::Idle(10_800),
            Ev::Wakes(None),
            Ev::Waits(None),
        ],
    );
    assert_eq!(causes(&got, "t1"), [seqs[0]], "{got:#?}");
    let (got, seqs) = scripted(
        "harness-clock",
        0,
        &[],
        &[
            Ev::Act("continue@v1", HOLDER, 4_000),
            Ev::Read(5_050, &t1),
            Ev::Wakes(Some(6_000)),
            Ev::Waits(None),
            Ev::Tick(6_000),
            Ev::Wakes(None),
        ],
    );
    assert_eq!(causes(&got, "t1"), [seqs[0]], "{got:#?}");
    time_order_holds(&got).unwrap_or_else(|e| panic!("{e}: {got:#?}"));
}

/// A TURN WAITING ON A READ HIDES NO OTHER TURN'S CLOCK (gap 25): two harness turns, the first
/// (open at 5 s) with its act four and a half seconds before it, which the turn's own read
/// passed, so it waits on the clock alone until 9.5 s; the second (open at 6 s) with its act half
/// a second after it, logged after the read, so it waits on a read begun from 9 s. A read short
/// of the end leaves both: the pair wake at 9.5 s, the read wait from 9 s. A tick before the wake
/// pairs nothing and keeps it, though the read wait's time passed; the tick at the wake pairs the
/// first turn, and the read to the end that follows pairs the second.
#[test]
fn a_turn_waiting_on_a_read_hides_no_other_turns_clock() {
    let rows = [
        &harness_rows("turn-1", 5_000)[..],
        &[(5_800, Row::End)],
        &harness_rows("turn-2", 6_000),
    ]
    .concat();
    let (got, seqs) = scripted(
        "harness-two-waits",
        0,
        &[],
        &[
            Ev::Act("continue@v1", HOLDER, 500),
            Ev::Read(6_050, &rows),
            Ev::Act("continue@v1", HOLDER, 6_500),
            Ev::Read(7_000, &[]),
            Ev::Moved(true),
            Ev::Wakes(Some(9_500)),
            Ev::Waits(Some(9_000)),
            Ev::Tick(9_200),
            Ev::Wakes(Some(9_500)),
            Ev::Tick(9_500),
            Ev::Wakes(None),
            Ev::Waits(Some(9_000)),
            Ev::Idle(9_600),
            Ev::Waits(None),
        ],
    );
    assert_eq!(causes(&got, "t1"), [seqs[0]], "{got:#?}");
    assert_eq!(causes(&got, "t2"), [seqs[1]], "{got:#?}");
    let at = |what: &str| got.iter().position(|l| l.contains(what));
    assert!(
        at(" amend t1 turn cause=") < at(" amend t2 turn cause="),
        "the clock settled the first before the read settled the second: {got:#?}"
    );
}

/// A RUN'S LAST HARNESS TURN TAKES ITS ACT BEFORE THE RUN ENDS (gap 25): an act a second before
/// the turn's open, whose read side the turn's own rows (stamped at the open, the act's time plus
/// that second) meet exactly, and the run ends once the log's clock passed the open by as much,
/// with no tick between, its front back at the shell, its group naming another program, or its
/// session closing: the cause is written before the run's close. NEGATIVE CONTROL: the run ending
/// before the clock passed as far after the open leaves the turn with no cause, since a nearer act
/// could still be logged.
#[test]
fn a_runs_last_harness_turn_takes_its_act_before_the_run_ends() {
    let t1 = harness_rows("turn-1", 5_000);
    for (ends, caused) in [(6_000, true), (5_500, false)] {
        for how in [RunEnd::Shell, RunEnd::Program, RunEnd::Session] {
            let (got, seqs) = scripted(
                "harness-run-end",
                0,
                &[],
                &[
                    Ev::Act("continue@v1", HOLDER, 4_000),
                    Ev::Read(5_050, &t1),
                    Ev::End(ends, how),
                    Ev::Wakes(None),
                ],
            );
            let want: &[u64] = if caused { &seqs[..1] } else { &[] };
            assert_eq!(causes(&got, "t1"), want, "{ends} {how:?}: {got:#?}");
            if caused {
                let at = |what: &str| got.iter().position(|l| l.contains(what));
                let paired = at(&format!(" amend t1 turn cause={} ", seqs[0]));
                let closed = at(" close r1 ");
                assert!(
                    paired.is_some() && closed.is_some() && paired < closed,
                    "{how:?}: {got:#?}"
                );
            }
        }
    }
}

/// A TURN THE RUN'S END CANNOT SETTLE KEEPS NO CAUSE (gap 25): an act three seconds before the
/// harness turn's open, the turn read at once and again a second later, then a second act two
/// seconds after the open, which types a turn whose prompt (stamped a tenth of a second on) the
/// tap has not read when the run ends. The second act is the nearer, and no turn in the log opened
/// nearer it, but no read passed its time plus its distance from the open, so the unread turn
/// could be its own: the run's last pairing gives the turn neither act, whichever way the run
/// ends. NEGATIVE CONTROL: the last pairing with the read side waived pairs the turn with the
/// second act.
#[test]
fn a_turn_the_runs_end_cannot_settle_keeps_no_cause() {
    let t1 = harness_rows("turn-1", 5_000);
    for how in [RunEnd::Shell, RunEnd::Program, RunEnd::Session] {
        let (got, seqs) = scripted(
            "harness-end-unread",
            0,
            &[],
            &[
                Ev::Act("continue@v1", HOLDER, 2_000),
                Ev::Read(5_050, &t1),
                Ev::Read(6_100, &[]),
                Ev::Act("continue@v1", HOLDER, 7_000),
                Ev::End(7_400, how),
                Ev::Wakes(None),
            ],
        );
        assert_eq!(seqs.len(), 2, "{how:?}: {got:#?}");
        assert_eq!(harnessed(&got).len(), 1, "{how:?}: {got:#?}");
        assert!(causes(&got, "t1").is_empty(), "{how:?}: {got:#?}");
    }
}

/// THE TAP'S READ TO THE END (gap 25): the real tap's read of the rig's rollout reaches the file's
/// end, so it raises the run's pairing watermark to the time it began (the log's clock, at 10 s)
/// less the writer's lag.
#[test]
fn a_tap_read_to_the_rollouts_end_raises_the_pairing_watermark() {
    let build = "0.164.0-dev.12521+g3d3eb12d8";
    let (tl, _home, target, claim) = codex_rig("harness-eof", "codex", build);
    let mut host = Host::default();
    let _ = host.ask(0xC0DE_2503, &claim.0, target, MEASURED);
    let read = tl
        .lock()
        .unwrap()
        .slog()
        .agent
        .as_ref()
        .map(|a| a.pairs.read_ms);
    assert_eq!(
        read,
        Some(WALL0 + 10_000 - WRITE_LAG_MS),
        "{:#?}",
        lines_of(&tl)
    );
}
