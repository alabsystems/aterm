// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn log_watch(
    timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    cursor: Option<aterm_slog::Cursor>,
    filter: LogFilter,
) -> (Watch, TargetStreams) {
    let streams = TargetStreams {
        log: true,
        ..TargetStreams::default()
    };
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let target = ResolvedTarget {
        local_id: 1,
        term,
        src: WatchSource {
            byte_fanout: Arc::new(ByteFanout::new()),
            turns: Arc::new(Mutex::new(TurnLedger::default())),
            meta: Arc::default(),
            timeline: timeline.clone(),
            fabric: Arc::default(),
            frame: Arc::default(),
            watchers: Arc::default(),
        },
    };
    let opts = PushOptions {
        log: filter,
        log_since: cursor,
        ..PushOptions::default()
    };
    (new_watch(&target, streams, &opts), streams)
}

fn text(frames: &[Frame]) -> String {
    frames
        .iter()
        .flat_map(|f| &f.segs)
        .map(|seg| String::from_utf8_lossy(seg.bytes()).into_owned())
        .collect()
}

fn timeline() -> Arc<Mutex<crate::session_timeline::SessionTimeline>> {
    let tl = Arc::new(Mutex::new(
        crate::session_timeline::SessionTimeline::for_test(),
    ));
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
    }
    tl
}

fn mark(
    tl: &Mutex<crate::session_timeline::SessionTimeline>,
    kind: aterm_slog::Kind,
    src: aterm_slog::Src,
) {
    let mut g = tl.lock().unwrap();
    let now = g.slog().now();
    g.slog_mut()
        .log_mut_for_journal()
        .mark(
            aterm_slog::Mark::new(kind, src, aterm_slog::Conf::Strong),
            now,
        )
        .expect("a mark");
}

#[test]
fn a_log_streams_sub_line_names_where_it_starts() {
    let tl = timeline();
    mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
    let (live, _) = log_watch(&tl, None, LogFilter::default());
    let ack = "OK subscribe 1\nsub 1 s-aaaa\n";
    assert_eq!(
        with_ack_tails(ack, std::slice::from_ref(&live)),
        "OK subscribe 1\nsub 1 s-aaaa log_seq=2 log_ep=1 log_base=1\n"
    );
    let (resumed, _) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(1, 1)),
        LogFilter::default(),
    );
    assert_eq!(
        with_ack_tails(ack, std::slice::from_ref(&resumed)),
        "OK subscribe 1\nsub 1 s-aaaa log_seq=1 log_ep=1 log_base=1\n"
    );
    let mut quiet = live;
    quiet.log = None;
    assert_eq!(with_ack_tails(ack, std::slice::from_ref(&quiet)), ack);
}

/// C13 (ROBUST-3): a resumed watch whose cursor names another log (`log-base=`) reads this
/// log as a reset, at its head too, and the `GAP` names this log's base for the next resume;
/// a cursor naming this log reads on.
#[test]
fn a_cursor_naming_another_log_resets_the_watch_and_the_gap_names_this_one() {
    let tl = timeline();
    mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
    let (ep, base, head) = {
        let g = tl.lock().unwrap();
        let log = g.slog().log();
        (log.ep(), log.base_ep(), log.last_seq())
    };
    let ack = "OK subscribe 1\nsub 1 s-aaaa\n";
    let (mut own, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep, 1).of_log(base)),
        LogFilter::default(),
    );
    let got = text(&frames_for_watch(&mut own, streams, true));
    assert!(
        !got.contains("GAP") && got.starts_with("LOG 1 2 mark - bell "),
        "its own log's cursor reads on: {got}"
    );
    let other = base + 1;
    let (mut foreign, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep, 1).of_log(other)),
        LogFilter::default(),
    );
    assert_eq!(
        with_ack_tails(ack, std::slice::from_ref(&foreign)),
        format!("OK subscribe 1\nsub 1 s-aaaa log_seq=1 log_ep={ep} log_base={other}\n"),
        "the ack names the log the watch was asked to start in"
    );
    let got = text(&frames_for_watch(&mut foreign, streams, true));
    let gap = got.lines().next().unwrap_or_default();
    assert!(
        gap.starts_with("GAP 1 log-lost=")
            && gap.contains(" why=reset ")
            && gap.ends_with(&format!(" ep={ep} base={base}")),
        "another log's cursor reads as a reset: {got}"
    );
    assert!(got.contains("\nLOG 1 1 open - spawn "), "{got}");
    let (mut at_head, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep, head).of_log(other)),
        LogFilter::default(),
    );
    assert!(
        text(&frames_for_watch(&mut at_head, streams, true)).contains(" why=reset "),
        "another log's cursor at this log's head is no head of it"
    );
}

#[test]
fn the_log_stream_parses_and_fails_closed() {
    for (spec, lvl, class) in [
        ("log", None, false),
        ("log:lvl=turn", Some(aterm_slog::Lvl::Turn), false),
        ("log:class=turn", None, true),
        ("log:lvl=run:class=turn", Some(aterm_slog::Lvl::Run), true),
        ("log:class=turn:lvl=step", Some(aterm_slog::Lvl::Step), true),
    ] {
        let req = Requested::parse(spec).unwrap_or_else(|| panic!("{spec}"));
        assert!(req.targets.log, "{spec}");
        assert_eq!(
            req.log,
            LogFilter {
                lvl,
                class_turn: class
            },
            "{spec}"
        );
    }
    for bad in [
        "log:lvl=page",
        "log:class=step",
        "log:lvl=turn:lvl=run",
        "log:foo=1",
        "log,log",
        "logs",
        "log:",
    ] {
        assert!(Requested::parse(bad).is_none(), "{bad} must fail closed");
    }
    let both = Requested::parse("mail:kinds=task,log:class=turn").expect("two streams");
    assert!(both.targets.mail && both.targets.log);
    assert_eq!(both.mail.kinds, vec!["task".to_string()]);
    assert!(both.log.class_turn);
    assert!(aterm_types::control_verbs::SUBSCRIBE_STREAMS.contains(&"log"));
}

#[test]
fn a_log_watch_is_live_from_the_head_and_replays_from_since_log() {
    let tl = timeline();
    mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
    let (mut live, streams) = log_watch(&tl, None, LogFilter::default());
    assert!(
        frames_for_watch(&mut live, streams, true).is_empty(),
        "no backlog"
    );
    mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
    mark(&tl, aterm_slog::Kind::Cwd, aterm_slog::Src::Engine);
    let got = text(&frames_for_watch(&mut live, streams, true));
    let lines: Vec<&str> = got.lines().collect();
    assert_eq!(lines.len(), 2, "{got}");
    assert!(
        lines[0].starts_with("LOG 1 3 mark - bell ") && lines[1].starts_with("LOG 1 4 mark - cwd "),
        "{got}"
    );
    assert!(
        frames_for_watch(&mut live, streams, false).is_empty(),
        "idle"
    );
    let ep = tl.lock().unwrap().slog().log().ep();
    let (mut resumed, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep, 1)),
        LogFilter::default(),
    );
    let got = text(&frames_for_watch(&mut resumed, streams, true));
    assert_eq!(got.lines().count(), 3, "seqs 2..=4: {got}");
    assert!(got.starts_with("LOG 1 2 mark - bell "), "{got}");
}

#[test]
fn a_program_mark_streams_as_a_log_frame() {
    use aterm_core::terminal::program_state::{ProgramState, ProgramStateWord};
    let tl = timeline();
    let (mut all, streams) = log_watch(&tl, None, LogFilter::default());
    let (mut turns, turn_streams) = log_watch(
        &tl,
        None,
        LogFilter {
            lvl: None,
            class_turn: true,
        },
    );
    tl.lock().unwrap().note_program_state(
        Some(ProgramState {
            state: ProgramStateWord::Working,
            kind: None,
            app: aterm_core::terminal::program_state::ProgramApp::Codex,
        }),
        1,
    );
    let got = text(&frames_for_watch(&mut all, streams, true));
    assert!(
        got.lines().any(|l| l.starts_with("LOG 1 ")
            && l.contains(" mark - program state=working src=engine conf=claim ")),
        "{got}"
    );
    let quiet = text(&frames_for_watch(&mut turns, turn_streams, true));
    assert!(!quiet.contains(" program "), "{quiet}");
}

#[test]
fn the_log_gap_is_the_reads_exact_count() {
    let tl = timeline();
    let ep = tl.lock().unwrap().slog().log().ep();
    for _ in 0..200 {
        mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
    }
    let (mut clean, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep, 1)),
        LogFilter::default(),
    );
    assert!(!text(&frames_for_watch(&mut clean, streams, true)).contains("GAP"));
    tl.lock().unwrap().slog_mut().set_log_byte_budget(2048);
    let lost = tl.lock().unwrap().slog().log().lost();
    assert!(lost > 0);
    let (mut behind, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep, 1)),
        LogFilter::default(),
    );
    let got = text(&frames_for_watch(&mut behind, streams, true));
    assert!(
        got.starts_with(&format!("GAP 1 log-lost={lost} why=evicted resync_to=")),
        "{got}"
    );
    let (mut reset, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep + 7, 3)),
        LogFilter::default(),
    );
    assert!(text(&frames_for_watch(&mut reset, streams, true)).starts_with("GAP 1 log-lost="));
    let (mut reset2, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep + 7, 3)),
        LogFilter::default(),
    );
    assert!(text(&frames_for_watch(&mut reset2, streams, true)).contains(" why=reset "));
    let head = tl.lock().unwrap().slog().log().last_seq();
    let (mut across, streams) = log_watch(
        &tl,
        Some(aterm_slog::Cursor::new(ep, head)),
        LogFilter::default(),
    );
    {
        let mut g = tl.lock().unwrap();
        g.slog_mut().log_mut_for_journal().skip_to(head + 50);
    }
    mark(&tl, aterm_slog::Kind::Bell, aterm_slog::Src::Engine);
    let got = text(&frames_for_watch(&mut across, streams, true));
    assert!(got.starts_with("GAP 1 log-lost=0 why=handoff "), "{got}");
    assert!(
        got.lines().nth(1).is_some_and(|l| l.starts_with("LOG 1 ")),
        "{got}"
    );
}

#[test]
fn the_log_filter_is_the_log_verbs_and_the_tier_is_hosts() {
    let tl = timeline();
    let (mut runs, streams) = log_watch(
        &tl,
        None,
        LogFilter {
            lvl: Some(aterm_slog::Lvl::Run),
            class_turn: false,
        },
    );
    let (mut class, _) = log_watch(
        &tl,
        None,
        LogFilter {
            lvl: None,
            class_turn: true,
        },
    );
    let (mut all, _) = log_watch(&tl, None, LogFilter::default());
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog_mut().log_mut_for_journal();
        let run = log
            .open(
                aterm_slog::Open::new(
                    aterm_slog::Kind::Agent,
                    aterm_slog::Src::Status,
                    aterm_slog::Conf::Strong,
                ),
                now,
            )
            .expect("run");
        let turn = log
            .open(
                aterm_slog::Open::new(
                    aterm_slog::Kind::Turn,
                    aterm_slog::Src::Screen,
                    aterm_slog::Conf::Strong,
                )
                .under(run),
                now,
            )
            .expect("turn");
        log.mark(
            aterm_slog::Mark::new(
                aterm_slog::Kind::Title,
                aterm_slog::Src::Engine,
                aterm_slog::Conf::Exact,
            )
            .at(turn)
            .fields(
                aterm_slog::Fields::new()
                    .text(aterm_slog::Key::Title, "SECRET TITLE")
                    .expect("title"),
            ),
            now,
        )
        .expect("title");
    }
    let runs_got = text(&frames_for_watch(&mut runs, streams, true));
    assert!(
        runs_got.contains(" open r1 agent ") && !runs_got.contains(" open t1 turn "),
        "{runs_got}"
    );
    let class_got = text(&frames_for_watch(&mut class, streams, true));
    assert!(
        class_got.contains(" open t1 turn ") && !class_got.contains(" title"),
        "{class_got}"
    );
    let all_got = text(&frames_for_watch(&mut all, streams, true));
    assert!(all_got.contains(" mark t1 title "), "{all_got}");
    assert!(
        !all_got.contains("SECRET"),
        "never the content tier: {all_got}"
    );
    let log = tl.lock().unwrap().slog().log().clone();
    let q = crate::session_log::LogQuery::parse("lvl=run").expect("q");
    let verb = crate::session_log::render_log(&log, &q);
    assert_eq!(
        verb.lines()
            .skip(1)
            .filter(|l| l.contains(" open "))
            .count(),
        runs_got.lines().filter(|l| l.contains(" open ")).count() + 1,
        "the verb also shows the session's open the live stream started after"
    );
}

#[test]
fn a_closed_sessions_close_record_precedes_exited() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle(9);
    let target = super::tests::target_of(&h);
    store.write().unwrap_or_else(|p| p.into_inner()).register(h);
    let streams = TargetStreams {
        events: true,
        log: true,
        ..Default::default()
    };
    let mut sink: Vec<u8> = Vec::new();
    let mut closed = false;
    let close_once = || {
        if !closed {
            closed = true;
            store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .deregister_local_as(
                    9,
                    crate::session_store::ExitReason::CtlClose,
                    crate::session_store::ExitActor::Ctl,
                );
        }
        false
    };
    push_loop_with_peer_probe(
        &store,
        &[target],
        PushScopes {
            streams,
            instance: InstanceStreams::default(),
            adopt: AdoptScope::none(),
        },
        PushOptions::default(),
        "",
        &mut sink,
        close_once,
    );
    let out = String::from_utf8_lossy(&sink).into_owned();
    let close = out
        .find("LOG 9 2 close - spawn out=ctl-close")
        .unwrap_or_else(|| panic!("{out}"));
    let exited = out
        .find("EVENT 9 exited")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(close < exited, "{out}");
}
