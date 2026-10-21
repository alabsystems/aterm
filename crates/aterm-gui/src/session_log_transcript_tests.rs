// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Transcript steps through the footer's own read into a real timeline
//! (session log Phase 3B, plan §2.10; rulings S1, S6, S12, S16, S27):
//! the records, their time order, the floor that never replays history, the
//! chunked hold, the wake straight to the watchers, the disarm when no run
//! takes the steps, and the leak rule (no transcript text anywhere).

use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_agent::harness::footer;
use aterm_slog::Tier;

use super::super::tests::{WALL0, hand_clock, lines, stamp, time_order_holds, with_claude};
use super::*;
use crate::claude_footer::{LogWake, rig};

/// The Claude Code process in front ([`with_claude`]'s group).
const PID: i32 = 300;

/// A row's `timestamp` `ms` into the test (the hand clock's scale).
fn at(ms: u64) -> String {
    iso(WALL0 + ms)
}

/// The RFC 3339 spelling of wall ms `wall`, as Claude Code writes it.
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

/// A session with Claude Code in front (group [`PID`]), its agent run open,
/// on a hand clock; its Claude home with the registry row; its transcript.
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
            g.slog_mut().open_session(None, now);
        }
        Rig::on(name, tl, hand)
    }

    /// The rig over `tl`, its session already open on `hand`.
    fn on(name: &str, tl: Arc<Mutex<SessionTimeline>>, hand: super::super::tests::Hand) -> Rig {
        tl.lock().unwrap().note_root_group(100);
        with_claude(&tl);
        // The process started a minute before the test's epoch.
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

    /// Claude Code wrote its transcript (empty so far).
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

/// THE STEPS OF A TURN (plan §2.4, §2.5) through the footer's own read: a
/// transcript that held rows before the tap bound is history, said once
/// (`source field=transcript state=partial skipped=<its length>`, S1); the
/// rows after it confirm the screen's turn (`amend vref=`), open and close
/// its Bash step under it, and amend the turn's own numbers — never on the
/// close (A3); the screen's settle closes the turn (S9: an unmeasured build
/// overrules nothing). Every record keeps the runs' time order (S4).
#[test]
fn a_turns_transcript_steps_reach_the_log_through_the_footer_read() {
    let mut r = Rig::new("slog-tx-steps");
    // History: a turn before the tap bound.
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
    // A screen turn, and the rows it writes.
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

/// A FILE THAT DID NOT EXIST HELD NO HISTORY (S1 as built): Claude Code
/// writes its transcript at its first prompt, after the run opened and the
/// tap armed. A tap that found no file waits for one, armed, and binds it
/// from its first byte (`skipped=0`): the first turn's prompt and its step
/// reach the log, the floor still dropping rows older than the run.
/// CONTROL: the same rows in a file that existed at the first read are
/// history (the first test's `skipped=<length>`).
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
        // Older than the run (it opened at the epoch): never queued,
        // whatever the seed.
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

/// A FIRST READ'S LOSS IS SAID (S23, S27): a transcript written after the
/// tap armed, holding a row the fold cannot read, binds from its first byte
/// and its lost row is said on a `source … dropped=1` mark at the batch's
/// end: the batch's bind applies before its counts, so the count has a
/// bound join to land on. NEGATIVE CONTROL: the same rows all readable say
/// no loss.
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

/// A BURST PAST THE QUEUE KEEPS ITS BIND (S1, S23): a transcript written
/// after the tap armed whose first read holds more fresh steps than the
/// queue keeps is still bound from its first byte (`skipped=0`, never its
/// length), the steps dropped are said (`dropped=`), and the turn's next
/// prompt and call apply.
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

/// S16: a batch longer than one chunk is applied over several holds, the
/// lock released between them, none longer than [`CHUNK`] events. CONTROL:
/// a batch within one chunk takes one hold.
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
    // The prompt, the model, 40 calls and 40 results.
    assert_eq!(applied.events, 82, "{applied:?}");
    assert_eq!(applied.holds, 82_usize.div_ceil(CHUNK), "{applied:?}");
    let got = r.lines();
    assert_eq!(having(&got, " tool out=ok ").len(), 40, "{got:?}");
    // CONTROL: one chunk, one hold.
    r.append(&[
        calls("m99", 2_000, &[("toolu_z", "Grep")]),
        results(2_010, &[("toolu_z", true)]),
    ]);
    let small = r.read(&LogWake::default());
    assert_eq!((small.events, small.holds), (2, 1), "{small:?}");
    assert!(having(&r.lines(), " tool out=error ").len() == 1);
}

/// `batch` applied to `tl` on a hand clock that moves 7 ns at each read, so
/// a hold (the lock taken, then released) books 7: what it applied and
/// every hold it booked, in order.
fn timed(tl: &Mutex<SessionTimeline>, batch: &StepBatch) -> (Applied, Vec<(Hold, u64)>) {
    let mut t = 0u64;
    let mut booked = Vec::new();
    let applied = apply_batch_timed(
        tl,
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

/// A bind, then `n` compactions: a batch of `n + 1` events.
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

/// T31: each hold that applies a chunk is timed, from the lock taken to its
/// release, and booked by its kind — a batch longer than one chunk books
/// its full chunks, then its last hold; a read that found no new rows still
/// takes the lock once, booked as a last hold. CONTROLS: a batch within one
/// chunk books its last hold alone, and a batch no run takes applies no
/// chunk and books nothing.
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
    // CONTROL: one chunk, one hold, the last.
    let small = Rig::new("slog-tx-hold-small");
    let (applied, booked) = timed(&small.tl, &compactions(CHUNK as u64 - 1));
    assert_eq!((applied.events, applied.holds), (CHUNK, 1), "{applied:?}");
    assert_eq!(booked, [(Hold::Last, 7)]);
    // CONTROL: no run takes the batch.
    let tl = Mutex::new(SessionTimeline::default());
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, now);
    }
    let (applied, booked) = timed(&tl, &compactions(81));
    assert!(applied.disarm && applied.events == 0, "{applied:?}");
    assert!(booked.is_empty(), "{booked:?}");
}

/// S12: the footer read rings the session's watchers itself, under the
/// timeline lock that took the owed wake — a subscription registered on the
/// session holds a ring the moment the read returns, and a parked in-process
/// `await log kind=tool` returns (a one-minute hang detector). NEGATIVE
/// CONTROL: a read that moved nothing owes nothing and rings nobody.
#[test]
fn the_footer_read_wakes_the_watchers_directly() {
    // A registered session, so an in-process `await log` finds it.
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
    // NEGATIVE CONTROL: nothing moved, nothing owed.
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
    // A parked `await log kind=tool` returns on the read.
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

/// THE DISARM: a batch that finds no run to take it (the agent run closed)
/// drops the tap, and the next read with a run in front arms a fresh one,
/// bound at once at the file's length — what the file held is history,
/// said by its own `source … skipped=` (S1). CONTROL: no run, no arm.
#[test]
fn a_tap_with_no_run_to_take_it_disarms_and_rebinds_past_the_history() {
    let mut r = Rig::new("slog-tx-disarm");
    r.touch();
    let _ = r.read(&LogWake::default());
    assert!(r.fold.steps_armed());
    // The run goes: its group leaves the foreground.
    r.hand.set(1_000);
    r.tl.lock().unwrap().note_foreground_group(100);
    r.append(&[prompt("v1", 1_100, "go")]);
    let gone = r.read(&LogWake::default());
    assert!(gone.disarm && !r.fold.steps_armed(), "{gone:?}");
    // CONTROL: with no run in front the next read arms nothing.
    let none = r.read(&LogWake::default());
    assert!(!none.disarm && none.events == 0 && !r.fold.steps_armed());
    // Claude Code comes back: a fresh run, a fresh tap, bound past v1.
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

/// THE LEAK RULE (D5 decision B; ruling S6; plan §2.7): rows carrying
/// sentinels in a prompt, a reply's words, a tool's input, a tool's result,
/// a limit notice and a local command's output, read through the footer's
/// own path into a real timeline, leave none of them in any record at the
/// content tier, the brief, the carry's lines or a record on the bus.
/// NEGATIVE CONTROL: a sentinel in a tool's NAME is a host-tier fact — in
/// the log, never on the bus.
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
    // NEGATIVE CONTROL: the tool's name is a host fact, kept, never wired.
    assert!(
        content.iter().any(|l| l.contains(" tool=SENTINELTOOL ")),
        "{content:?}"
    );
    assert!(!bus.join("\n").contains("SENTINELTOOL"), "{bus:?}");
}
