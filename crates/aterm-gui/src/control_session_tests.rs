// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn the_handoff_turn_count_leaves_room_for_turns_in_the_overlap() {
    let _own = super::own_turn_ids();
    let carried = super::turn_ids_for_handoff();
    let overlap = super::mint_turn_id();
    assert!(
        overlap <= carried,
        "an id minted after the manifest ({overlap}) is one the successor, counting \
         on from {carried}, never mints"
    );
    assert!(!super::past_ceiling(carried, carried));
    assert!(super::past_ceiling(carried + 1, carried));
    assert!(!super::past_ceiling(u64::MAX, 0), "no manifest, no ceiling");
}

#[test]
fn a_count_at_the_manifest_limit_never_wedges_the_commit() {
    let max = crate::handoff_carry::MAX_TURN_ID;
    let minted = max + 1;
    let ceiling = super::handoff_ceiling_for(minted);
    assert_eq!(ceiling, max, "the ceiling is held to the top");
    assert!(!super::past_ceiling(minted, ceiling));
    assert!(!super::past_ceiling(minted + TURN_ID_RESERVE, ceiling));
    let below = max - TURN_ID_RESERVE - 1;
    let below_ceiling = super::handoff_ceiling_for(below);
    assert_eq!(below_ceiling, below + TURN_ID_RESERVE);
    assert!(!super::past_ceiling(below, below_ceiling));
    assert!(
        super::past_ceiling(below_ceiling + 1, below_ceiling),
        "negative control: past a ceiling below the top still stands down"
    );
}
use super::*;
use aterm_session::{EdgeTable, LaunchNonce};

#[test]
fn history_and_timeline_limits_match_retained_wire_rows() {
    use crate::turn_ledger::{ArchMark, LEDGER_CAP, TurnRecord};

    let h = crate::session_store::test_handle(73);
    assert_eq!(cmd_history(&h.ctx, "1 since=0"), "OK 0\n");
    assert_eq!(cmd_timeline(&h.ctx, "1 since=0"), "OK 0\n");

    for i in 1..=(LEDGER_CAP + 7) as u64 {
        h.ctx.turns.lock().unwrap().push(TurnRecord {
            id: i * 2,
            started_ms: i,
            dur_ms: 3,
            submitted: true,
            status: "settled",
            text: format!("turn {i} 🦀").into(),
            screen_hash: i,
            seq: i,
            arch: ArchMark { origin: 7, last: i },
            by: TurnBy::Ctl,
            carried: i.is_multiple_of(2),
        });
    }
    for i in 1..=(aterm_slog::MAX_RECORDS + 7) as u64 {
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .record_hand_row("title-change", format!("title=tab%20{i}"));
    }

    type HistoryQuery = fn(&SessionCtx, &str) -> String;
    let verbs: [(HistoryQuery, Option<usize>, bool); 2] = [
        (cmd_history, Some(LEDGER_CAP), false),
        (cmd_timeline, None, true),
    ];
    for (verb, cap, above_reads_all) in verbs {
        let all = verb(&h.ctx, "");
        let rows: Vec<(u64, &str)> = all
            .lines()
            .skip(1)
            .map(|line| {
                (
                    line.split_whitespace().nth(1).unwrap().parse().unwrap(),
                    line,
                )
            })
            .collect();
        if cap.is_none() {
            let floor = aterm_slog::row_floor(crate::session_log::ring_limits()).records;
            assert!(rows.len() > floor, "the view holds more than its row floor");
        }
        let cap = cap.unwrap_or(rows.len());
        assert_eq!(rows.len(), cap, "fixture must reach ring eviction");
        let low = rows[0].0;
        let high = rows.last().unwrap().0;
        assert!(low > 1, "fixture must evict a prefix");

        for n in [0, 1, 2, cap - 1, cap, cap + 1, usize::MAX] {
            for since in [
                None,
                Some(0),
                Some(low - 1),
                Some(low),
                Some(high - 1),
                Some(high),
                Some(high + 1),
                Some(u64::MAX),
            ] {
                let mut expected: Vec<&str> = rows
                    .iter()
                    .filter(|(id, _)| {
                        since.is_none_or(|after| (above_reads_all && after > high) || *id > after)
                    })
                    .map(|(_, line)| *line)
                    .collect();
                if n > 0 && expected.len() > n {
                    expected.drain(..expected.len() - n);
                }
                let shown = expected.len();
                let mut want = format!("OK {shown}\n");
                for line in expected {
                    want.push_str(line);
                    want.push('\n');
                }
                let args = match since {
                    Some(id) => format!("{n} since={id}"),
                    None => n.to_string(),
                };
                crate::work_counts::take_ring_rows_formatted();
                assert_eq!(verb(&h.ctx, &args), want, "arguments: {args}");
                assert_eq!(
                    crate::work_counts::take_ring_rows_formatted(),
                    shown,
                    "only the rows shown are formatted: {args}"
                );
            }
        }
    }
}

#[test]
fn ring_reads_format_after_their_lock_is_released() {
    use crate::session_log::ENCODE_PROBE;
    use crate::session_store::{ExitActor, ExitReason, new_store, test_handle};
    use crate::turn_ledger::{ArchMark, TurnRecord};

    let store = new_store();
    let h = test_handle(5);
    {
        let mut g = store.write().unwrap();
        g.register(h.clone());
        g.register(test_handle(6));
        g.deregister_local_as(6, ExitReason::UiClose, ExitActor::Human);
    }
    h.ctx.turns.lock().unwrap().push(TurnRecord {
        id: 1,
        started_ms: 1,
        dur_ms: 1,
        submitted: true,
        status: "settled",
        text: "make".into(),
        screen_hash: 1,
        seq: 1,
        arch: ArchMark::default(),
        by: crate::turn_ledger::TurnBy::Ctl,
        carried: false,
    });
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .record_hand_row("title-change", "title=t".to_string());

    let held: Arc<Mutex<Vec<[bool; 3]>>> = Arc::new(Mutex::new(Vec::new()));
    let (ctx, probe_store, probe_held) =
        (Arc::clone(&h.ctx), Arc::clone(&store), Arc::clone(&held));
    ENCODE_PROBE.with(|p| {
        *p.borrow_mut() = Some(Box::new(move || {
            probe_held.lock().unwrap().push([
                ctx.turns.try_lock().is_err(),
                ctx.timeline.try_lock().is_err(),
                probe_store.try_write().is_err(),
            ]);
        }));
    });
    let replies = [
        cmd_history(&h.ctx, ""),
        cmd_timeline(&h.ctx, ""),
        cmd_exits(&store, "", &[]),
    ];
    let probes = std::mem::take(&mut *held.lock().unwrap());
    for reply in &replies {
        assert!(
            reply.starts_with("OK ") && !reply.starts_with("OK 0"),
            "{reply}"
        );
    }
    assert_eq!(
        probes,
        vec![[false; 3]; 3],
        "a ring was formatted under its lock"
    );

    let all = RingPage { n: 0, since: None };
    let under = [
        {
            let ledger = h.ctx.turns.lock().unwrap();
            history_reply(&all.copy(ledger.since(None)), None)
        },
        {
            let tl = h.ctx.timeline.lock().unwrap();
            timeline_reply(&tl.view_copy(all.since, all.n).rows())
        },
        {
            let g = store.read().unwrap();
            exits_reply(&all.copy(g.closed_logs().exits_since(0)), &[])
        },
    ];
    let probes = std::mem::take(&mut *held.lock().unwrap());
    ENCODE_PROBE.with(|p| *p.borrow_mut() = None);
    assert_eq!(
        probes,
        vec![
            [true, false, false],
            [false, true, false],
            [false, false, true]
        ]
    );
    assert_eq!(under, replies);
}

#[test]
fn momentum_wait_read_guards_conform_and_catch_expired_reparking() {
    use std::time::{Duration, Instant};

    let model = aterm_models_session::momentum_wait_read_model();
    let now = Instant::now();
    for (value, floor) in [(0.0, 0.0), (0.000_75, 0.0), (0.000_75, 0.000_7)] {
        for expired in [false, true] {
            let until = if expired {
                now
            } else {
                now + Duration::from_secs(1)
            };
            let mut state = model.init_state();
            let met = value <= floor;
            let observe = match (met, expired) {
                (false, false) => "ReadLive",
                (false, true) => "ReadExpired",
                (true, false) => "ReadBelow",
                (true, true) => "ReadBelowExpired",
            };
            assert!(model.fire(observe, &mut state));
            let chosen = match momentum_read_outcome(value, floor, now, until) {
                Some(MomentumWait::Below(v)) => {
                    assert_eq!(v, value);
                    "Below"
                }
                Some(MomentumWait::Deadline(v)) => {
                    assert_eq!(v, value);
                    "Deadline"
                }
                None => "Park",
                _ => panic!("a pure read cannot exit or fail"),
            };
            for action in ["Below", "Deadline", "Park"] {
                assert_eq!(model.action_enabled(action, &state), chosen == action);
            }
            let mut post = state.clone();
            assert!(model.fire(chosen, &mut post));
            let (valid, why) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                &state,
                &post,
                Some(chosen),
                "real momentum read guard",
            );
            assert!(valid, "{why}");
            if expired && !met {
                let buggy = aterm_spec::interp::with_buggy(&model, 1);
                assert!(buggy.fire("Park", &mut state));
                assert!(!model.check_invariant("ExpiredReadingCannotRepark", &state));
            }
        }
    }
}

#[test]
fn momentum_wait_zero_and_low_floors_park_once_until_the_real_reading_matches() {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use aterm_effects::typing_momentum::TypingMomentum;

    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let sub = crate::session_watch::Subscription::new();
    for (value, floor) in [(0.000_51, 0.0), (0.000_51, 0.000_1), (0.000_75, 0.000_7)] {
        let at = Instant::now();
        let metric = Cell::new(TypingMomentum::default());
        let reads = Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            if reads.get() > 4 {
                return Err("immediate-latch read loop".to_string());
            }
            let now = Instant::now();
            if reads.get() == 1 {
                let mut warm = TypingMomentum::default();
                warm.set_value(now, value);
                metric.set(warm);
            }
            Ok((metric.get().value(now), now))
        };
        match park_until_momentum_below(
            &term,
            &|| false,
            &sub,
            &read,
            floor,
            at + Duration::from_secs(2),
        ) {
            MomentumWait::Below(v) => assert!(v <= floor),
            MomentumWait::Unreadable(why) => panic!("{why}"),
            _ => panic!("the real metric must reach {floor}"),
        }
        assert_eq!(reads.get(), 2, "one solving read and one confirmation");
        assert!(term_lock(&term).watch_next_deadline().is_none());
    }
}

#[test]
fn momentum_wait_low_floor_honors_timeout_and_expired_read_backstop() {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let sub = crate::session_watch::Subscription::new();
    for floor in [0.0, 0.000_1, 0.000_7] {
        let reads = Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            if reads.get() > 4 {
                return Err("immediate-latch read loop".to_string());
            }
            Ok((0.000_75, Instant::now()))
        };
        let until = Instant::now() + Duration::from_millis(20);
        assert!(matches!(
            park_until_momentum_below(&term, &|| false, &sub, &read, floor, until),
            MomentumWait::Deadline(v) if v > floor
        ));
        assert!(
            (1..=2).contains(&reads.get()),
            "at most the solving read and deadline read, without polling"
        );
        assert!(term_lock(&term).watch_next_deadline().is_none());
    }

    let now = Instant::now();
    let reads = Cell::new(0);
    let stale = || {
        reads.set(reads.get() + 1);
        if reads.get() > 4 {
            return Err("expired reading re-parked".to_string());
        }
        Ok((0.000_75, now - Duration::from_secs(10)))
    };
    assert!(matches!(
        park_until_momentum_below(&term, &|| false, &sub, &stale, 0.0, now),
        MomentumWait::Deadline(v) if v > 0.0
    ));
    assert_eq!(reads.get(), 1);
    assert!(term_lock(&term).watch_next_deadline().is_none());
    assert!(matches!(
        park_until_momentum_below(&term, &|| true, &sub, &stale, 0.0, now),
        MomentumWait::Exited
    ));
}

fn handle(local_id: u64, term: &Arc<Mutex<Terminal>>) -> SessionHandle {
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let observed = crate::session_watch::Observed::default();
    let ctx = Arc::new(crate::SessionCtx {
        sink: Arc::new(aterm_session::sink::SinkWriter::new(-1)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(term),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(std::sync::Mutex::new(EdgeTable::new())),
        turn_lease: std::sync::Mutex::new(None),
        self_id: sid.clone(),
        nonce,
        cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
            80, 24,
        ))),
        temporal: Arc::new(std::sync::Mutex::new(
            crate::temporal::TemporalRecorder::new(),
        )),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: observed.turns,
        meta: Arc::default(),
        timeline: observed.timeline,
        fabric: std::sync::Arc::default(),
        idem: Default::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        licence: Default::default(),
        launch: Default::default(),
        reset_lane: Default::default(),
        crash_journal: Default::default(),
        typed: Default::default(),
        bells: Default::default(),
        screen_moves: Default::default(),
        job_feed: Default::default(),
        ran: Default::default(),
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        witness: Default::default(),
        fenced_writes: Default::default(),
        native_policy: Default::default(),
        spawn_mark: Default::default(),
    });
    SessionHandle {
        sid,
        nonce,
        local_id,
        parent: None,
        state: crate::session_store::SessionState::Alive,
        title: format!("tab-{local_id}"),
        term: term.clone(),
        master: -1,
        shell_identity: aterm_pty::ShellIdentity::unproven(0),
        ctx,
    }
}

fn row(local: u64, window: u64, active_tab: bool, window_focused: bool) -> SessionWindowRow {
    SessionWindowRow {
        local,
        window,
        active_tab,
        window_focused,
    }
}

fn assert_roster_fields(line: &str, expected: &[(&str, &str)]) {
    for &(key, value) in expected {
        let mut found = line
            .split_whitespace()
            .filter_map(|word| word.split_once('='))
            .filter_map(|(name, value)| (name == key).then_some(value));
        assert_eq!(found.next(), Some(value), "{key}: {line}");
        assert_eq!(found.next(), None, "duplicate {key}: {line}");
    }
}

const AGENT_COLUMNS: [&str; 31] = [
    "program=",
    "agent=",
    "agent_detail=",
    "agent_rev=",
    "agent_since_ms=",
    "agent_gen=",
    "agent_fp=",
    "wait=",
    "wait_gib=",
    "wait_ahead=",
    "program_state=",
    "human_ms=",
    "role=",
    "attention=",
    "user_title=",
    "supervisor_contract=",
    "supervisor_scope=",
    "supervisor_revision=",
    "supervisor=",
    "supervisor_proven=",
    "supervisor_version=",
    "supervisor_vendor=",
    "autopilot=",
    "path_evidence=",
    "copy=",
    "upgrade=",
    "land_at=",
    "task=",
    "pause=",
    "task_state=",
    "task_rev=",
];

fn tail(line: &str) -> String {
    let mut f = line.splitn(3, ' ');
    let local = f.next().unwrap();
    let _sid = f.next().unwrap();
    let rest = f.next().unwrap();
    let rest = rest
        .split_whitespace()
        .filter(|t| !t.starts_with("nonce="))
        .filter(|t| !AGENT_COLUMNS.iter().any(|k| t.starts_with(k)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{local} {rest}")
}

#[test]
fn await_agent_words_take_walls_and_refuse_typos() {
    let words = parse_agent_words("prompt,wall,wall:usage-session,unknown").expect("valid");
    assert_eq!(
        words,
        vec![
            AgentWord::Verdict("prompt"),
            AgentWord::Wall(None),
            AgentWord::Wall(Some("usage-session".to_string())),
            AgentWord::Verdict("unknown"),
        ]
    );
    let overloaded = aterm_phase::WallKind::Overloaded.wire();
    let session = aterm_phase::WallKind::UsageSession.wire();
    assert!(words[1].matches(overloaded));
    assert!(words[2].matches(session));
    assert!(!words[2].matches(overloaded));
    assert!(!words[1].matches("prompt") && !words[1].matches("wallpaper"));
    for bad in [
        "",
        "promt",
        "limited",
        "limitd",
        "wall:",
        "wall:Rate",
        "wall:a b",
        "prompt,,idle",
    ] {
        assert_eq!(parse_agent_words(bad), None, "{bad:?}");
    }
}

#[test]
fn the_roster_carries_the_typed_role_attention_and_title() {
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let h = handle(0, &t0);
    {
        let mut m = h.ctx.meta.lock().unwrap();
        m.set("role", Some("reviewer".into()));
        m.set("attention", Some("stuck on CI".into()));
        m.set("title", Some("my build".into()));
    }
    let out = sessions_lines(
        std::slice::from_ref(&h),
        Ok(&[]),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let line = out.lines().nth(1).unwrap();
    assert_roster_fields(
        line,
        &[
            ("role", "reviewer"),
            ("attention", "stuck%20on%20CI"),
            ("user_title", "my%20build"),
            ("supervisor", "-"),
            ("supervisor_proven", "0"),
            ("meta", "1"),
        ],
    );
}

#[test]
fn the_roster_carries_each_sessions_task_record() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let (worker, shell) = (handle(0, &t0), handle(1, &t0));
    mark_session_task(&worker.ctx, 0, &TaskMark::new(TaskState::Working)).expect("unfenced");
    mark_session_task(&worker.ctx, 0, &TaskMark::new(TaskState::Done)).expect("unfenced");
    let out = sessions_lines(
        &[worker, shell],
        Ok(&[]),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let rows: Vec<&str> = out.lines().skip(1).collect();
    assert!(
        rows[0].ends_with(" task=- pause=- task_state=done task_rev=2"),
        "{out}"
    );
    assert!(
        rows[1].ends_with(" task=- pause=- task_state=- task_rev=0"),
        "{out}"
    );
}

fn three_workers(store: &Store) -> Vec<SessionHandle> {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let workers: Vec<SessionHandle> = (1..=3).map(crate::session_store::test_handle).collect();
    for worker in &workers {
        store.write().unwrap().register(worker.clone());
        mark_session_task(
            &worker.ctx,
            worker.local_id,
            &TaskMark::new(TaskState::Working),
        )
        .expect("unfenced");
    }
    workers
}

fn task_reply(reply: &str) -> (&str, Vec<&str>) {
    let mut lines = reply.lines();
    let header = lines.next().expect("a header");
    let states = lines
        .map(|row| {
            row.split_whitespace()
                .find_map(|t| t.strip_prefix("state="))
                .expect("state=")
        })
        .collect();
    (header, states)
}

/// A row names qualifying request news, independently of the current display
/// string. Already-terminal/noted baselines stay quiet; captured edges hold.
#[test]
fn await_task_rows_name_recorded_news_without_guessing_display_attention() {
    use crate::session_timeline::{MetaEdit, write_attention_owned};
    use crate::task_record::{
        Combine, TaskMark, TaskState, mark_session_task, mark_session_task_fact,
    };
    use aterm_agent::supervise::occupancy::{FactWord, NoteOutcome, RuleId};
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let bit = |line: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix("task_news="))
            .map(str::to_string)
    };
    let w = &workers[0];
    write_attention_owned(
        &w.ctx,
        "unrelated-owner",
        MetaEdit::Set("an unrelated display message"),
        aterm_slog::Actor::Harness,
    )
    .expect("the display entry");
    mark_session_task(
        &workers[2].ctx,
        workers[2].local_id,
        &TaskMark::new(TaskState::Done),
    )
    .expect("terminal baseline");
    let f = &workers[1];
    let note = |word| mark_session_task_fact(&f.ctx, f.local_id, word, std::time::Duration::ZERO);
    assert_eq!(note(FactWord::new(RuleId::HoldRoom, 1)), NoteOutcome::Noted);
    let targets: Vec<_> = workers
        .iter()
        .map(|w| TaskTarget {
            local_id: w.local_id,
            ctx: &w.ctx,
            since: None,
            facts: true,
            fact_since: None,
        })
        .collect();
    let quiet = await_task(&store, &targets, Combine::All, 0);
    assert!(quiet.contains("attention=an%20unrelated"));
    assert!(
        quiet
            .lines()
            .skip(1)
            .all(|row| bit(row).as_deref() == Some("0")),
        "{quiet}"
    );
    let edges: Vec<_> = targets
        .iter()
        .map(|t| {
            let record = task_record_of(t.ctx).expect("a task");
            (record.rev, Some(record.fact_rev))
        })
        .collect();
    let exits: Vec<_> = targets
        .iter()
        .map(|t| exit_probe(&store, t.local_id))
        .collect();
    let mut latched = vec![None; targets.len()];
    write_attention_owned(
        &w.ctx,
        crate::harness_host::ATTENTION_OWNER,
        MetaEdit::Set("a login"),
        aterm_slog::Actor::Harness,
    )
    .expect("the task entry");
    assert_eq!(
        note(FactWord::new(RuleId::WaitNudge, 1)),
        NoteOutcome::Noted
    );
    latch_task_news(&store, &targets, &edges, &exits, &mut latched);
    assert!(latched[..2].iter().all(|row| {
        row.as_deref()
            .is_some_and(|row| bit(row).as_deref() == Some("1"))
    }));
    assert!(latched[2].is_none(), "the terminal baseline is no new edge");
    write_attention_owned(
        &w.ctx,
        crate::harness_host::ATTENTION_OWNER,
        MetaEdit::Clear,
        aterm_slog::Actor::Harness,
    )
    .expect("the task entry clears");
    assert_eq!(note(None), NoteOutcome::Cleared);
    latch_task_news(&store, &targets, &edges, &exits, &mut latched);
    assert!(latched[..2].iter().all(|row| {
        row.as_deref()
            .is_some_and(|row| bit(row).as_deref() == Some("1"))
    }));
    let quiet = await_task(&store, &targets, Combine::All, 0);
    assert!(
        quiet
            .lines()
            .skip(1)
            .all(|row| bit(row).as_deref() == Some("0")),
        "{quiet}"
    );
}

#[test]
fn await_task_answers_news_past_each_targets_rev_once() {
    use crate::task_record::{Combine, TaskMark, TaskState, mark_session_task};
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let wait = |since: [Option<u64>; 3], combine: Combine, timeout_ms: u64| {
        let targets: Vec<TaskTarget<'_>> = workers
            .iter()
            .zip(since)
            .map(|(w, since)| TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since,
                facts: false,
                fact_since: None,
            })
            .collect();
        await_task(&store, &targets, combine, timeout_ms)
    };
    let quiet = wait([None; 3], Combine::Any, 20);
    assert_eq!(
        task_reply(&quiet),
        ("OK 3 status=timeout", vec!["working"; 3]),
        "{quiet}"
    );
    assert!(
        quiet.contains(&format!(
            "@{} rev=1 state=working agent=- program_state=- attention=- result_bytes=0 \
             task_news=0\n",
            workers[0].sid.as_str()
        )),
        "{quiet}"
    );

    let mut done = TaskMark::new(TaskState::Done);
    done.result_bytes = 64;
    mark_session_task(&workers[1].ctx, 2, &done).expect("unfenced");
    let any = wait([Some(1); 3], Combine::Any, 60_000);
    assert_eq!(
        task_reply(&any),
        ("OK 3", vec!["working", "done", "working"])
    );
    assert!(
        any.contains(
            " rev=2 state=done agent=- program_state=- attention=- result_bytes=64 \
             conclusion=finished task_news=1\n"
        ),
        "{any}"
    );
    for (since, combine) in [
        ([Some(1); 3], Combine::All),
        ([Some(1), Some(2), Some(1)], Combine::Any),
        ([None; 3], Combine::Any),
    ] {
        let again = wait(since, combine, 20);
        assert_eq!(
            task_reply(&again).0,
            "OK 3 status=timeout",
            "{since:?}: {again}"
        );
    }
    assert_eq!(
        task_reply(&wait([Some(0); 3], Combine::Any, 60_000)).0,
        "OK 3"
    );

    let mut stalled = TaskMark::new(TaskState::Stalled);
    stalled.why = Some("startup");
    mark_session_task(&workers[0].ctx, 1, &stalled).expect("unfenced");
    let mut exited = TaskMark::new(TaskState::Exited);
    exited.exit = Some(1);
    mark_session_task(&workers[2].ctx, 3, &exited).expect("unfenced");
    let all = wait([Some(1); 3], Combine::All, 60_000);
    assert_eq!(
        task_reply(&all),
        ("OK 3", vec!["stalled", "done", "exited"])
    );
}

#[test]
fn a_returned_agent_is_working_for_the_task_waiter() {
    use crate::session_timeline::AgentStamp;
    use crate::task_record::{Combine, TaskMark, TaskState, mark_session_task};

    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let w = &workers[0];
    let mut exited = TaskMark::new(TaskState::Exited);
    exited.why = Some(crate::task_record::WHY_EXIT);
    mark_session_task(&w.ctx, w.local_id, &exited).expect("unfenced");
    {
        let mut timeline = w.ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(42);
        timeline.set_program(42, Some("claude".to_string()));
        timeline.publish_agent(
            "busy",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    let reply = await_task(
        &store,
        &[TaskTarget {
            local_id: w.local_id,
            ctx: &w.ctx,
            since: Some(0),
            facts: false,
            fact_since: None,
        }],
        Combine::Any,
        0,
    );
    assert!(reply.contains("status=timeout"), "{reply}");
    assert!(reply.contains("state=working agent=busy"), "{reply}");
    assert!(reply.contains("task_news=0"), "{reply}");
    assert!(!reply.contains("conclusion=failed"), "{reply}");
}

/// A CLAIM IS NO TASK NEWS: a codex's own report (`program_state=`) rides an `await task`
/// row but never wakes one. A wait parked past a working worker's rev sees the program
/// report `done`, then `blocked:permission`, and still answers only at its deadline, the
/// claim in its row and `task_news=0`, and the `task` reply carries it too; the harness's own
/// `task mark done` then is news.
#[test]
fn a_programs_own_report_rides_the_await_task_row_and_never_wakes_it() {
    use crate::task_record::{Combine, TaskMark, TaskState, mark_session_task};
    use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramState, ProgramStateWord};
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let w = &workers[0];
    let one = |timeout_ms: u64| {
        await_task(
            &store,
            &[TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: Some(1),
                facts: false,
                fact_since: None,
            }],
            Combine::Any,
            timeout_ms,
        )
    };
    let report = |state, kind| {
        w.ctx.timeline.lock().unwrap().note_program_state(
            Some(ProgramState {
                state,
                kind,
                app: aterm_core::terminal::program_state::ProgramApp::Codex,
            }),
            1,
        );
    };
    let sid = w.sid.as_str();
    let parked = std::thread::scope(|s| {
        let waiter = s.spawn(|| one(2_000));
        let armed = std::time::Instant::now();
        while w.ctx.watchers.count() == 0 {
            assert!(
                armed.elapsed() < std::time::Duration::from_secs(60),
                "the waiter never parked"
            );
            aterm_uds::deadline::poll_yield();
        }
        report(ProgramStateWord::Done, None);
        report(
            ProgramStateWord::Blocked,
            Some(ProgramBlockedKind::Permission),
        );
        waiter.join().expect("the waiter")
    });
    assert_eq!(
        parked,
        format!(
            "OK 1 status=timeout\n@{sid} rev=1 state=working agent=- \
             program_state=blocked:permission attention=- result_bytes=0 task_news=0\n"
        ),
    );
    let task = crate::task_record::cmd_task(&w.ctx, w.local_id, Scope::Owner, "");
    assert!(
        task.starts_with("OK task rev=1 state=working ")
            && task.ends_with(" agent=- program_state=blocked:permission attention=-\n"),
        "the task reply carries the claim beside agent=: {task}"
    );
    mark_session_task(&w.ctx, w.local_id, &TaskMark::new(TaskState::Done)).expect("unfenced");
    assert_eq!(
        one(60_000),
        format!(
            "OK 1\n@{sid} rev=2 state=done agent=- program_state=blocked:permission \
             attention=- result_bytes=0 conclusion=finished task_news=1\n"
        ),
        "the record's own news"
    );
}

/// `supervisor_proven=` on `ls` is `status supervisor_proven=`: `1` only while the live claim
/// is a proven self-claim, `0` for an unproven one and for a tab nobody claims. A proven
/// contract v3 claim carries the versions it announced (`supervisor_version=`,
/// `supervisor_vendor=`, `docs/DESIGN-harness-repos-2026-10-24.md` §5); every other row, the
/// proven codex-alab self-claim among them (the control), carries `-` for both.
#[test]
fn the_roster_says_whether_the_live_claim_is_proven() {
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let (proven, unproven, plain) = (handle(0, &t0), handle(1, &t0), handle(2, &t0));
    let (scoped, bare) = (handle(3, &t0), handle(4, &t0));
    for (h, words) in [
        (
            &scoped,
            &[
                "contract=3",
                "scope=relaunch,upgrade",
                "version=0.3.0+gabc",
                "vendor=1.0.70",
            ][..],
        ),
        (&bare, &["contract=3", "scope=relaunch"][..]),
    ] {
        let aterm_agent::harness::contract::Words::Scoped(claim) =
            aterm_agent::harness::contract::read("grok-alab@4243", words)
        else {
            panic!("grok-alab's claim reads as a scoped claim: {words:?}");
        };
        h.ctx.meta.lock().unwrap().supervisor = Some(crate::session_timeline::SupervisorClaim {
            holder: "grok-alab@4243".to_string(),
            conn: Some(2),
            expires_us: None,
            proven: None,
            scoped: Some(crate::harness_claim::Scoped {
                proven: crate::self_claim::Proven {
                    pid: 4243,
                    start: "stand-in".to_string(),
                },
                claim,
            }),
        });
    }
    for (h, proof) in [(&proven, true), (&unproven, false)] {
        h.ctx.meta.lock().unwrap().supervisor = Some(crate::session_timeline::SupervisorClaim {
            holder: "codex-alab@4242".to_string(),
            conn: Some(1),
            expires_us: None,
            proven: proof.then(|| crate::self_claim::Proven {
                pid: 4242,
                start: "Mon Oct 20 16:37:58 2026".to_string(),
            }),
            scoped: None,
        });
    }
    let out = sessions_lines(
        &[proven, unproven, plain, scoped, bare],
        Ok(&[]),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let rows: Vec<&str> = out.lines().skip(1).collect();
    assert_eq!(rows.len(), 5, "{out}");
    for (row, contract, scopes) in [
        (
            rows[0],
            "2",
            "relaunch,upgrade,turn-end,approvals,questions,walls",
        ),
        (
            rows[1],
            "-",
            "relaunch,upgrade,turn-end,approvals,questions,walls",
        ),
        (rows[2], "-", "-"),
        (rows[3], "3", "relaunch,upgrade"),
        (rows[4], "3", "relaunch"),
    ] {
        assert!(
            row.contains(&format!(
                " supervisor_contract={contract} supervisor_scope={scopes} supervisor_revision=0 "
            )),
            "{row}"
        );
    }
    for (row, want) in rows.iter().zip([
        " supervisor=codex-alab@4242 supervisor_proven=1 supervisor_version=- \
         supervisor_vendor=- autopilot=",
        " supervisor=codex-alab@4242 supervisor_proven=0 supervisor_version=- \
         supervisor_vendor=- autopilot=",
        " supervisor=- supervisor_proven=0 supervisor_version=- supervisor_vendor=- autopilot=",
        " supervisor=grok-alab@4243 supervisor_proven=1 supervisor_version=0.3.0+gabc \
         supervisor_vendor=1.0.70 autopilot=",
        " supervisor=grok-alab@4243 supervisor_proven=1 supervisor_version=- \
         supervisor_vendor=- autopilot=",
    ]) {
        assert!(row.contains(want), "{want}: {row}");
    }
}

/// THE ROSTER'S FIXED COLUMN ORDER: a `sessions` row is five positional fields (`local sid
/// parent state title`) and then exactly these keys, in this order, as the SPECS row
/// roster-line-denotation names them.
#[test]
fn a_roster_row_keeps_its_fixed_column_order() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let h = handle(0, &term);
    let out = sessions_lines(
        std::slice::from_ref(&h),
        Ok(&[]),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let row = out.lines().nth(1).expect("one row");
    let mut tokens = row.split(' ');
    let positional: Vec<&str> = tokens.by_ref().take(5).collect();
    assert_eq!(
        positional,
        ["0", h.sid.as_str(), "-", h.state.as_str(), "tab-0"],
        "{row}"
    );
    let keys: Vec<&str> = tokens
        .map(|t| t.split_once('=').map_or(t, |(k, _)| k))
        .collect();
    assert_eq!(
        keys,
        [
            "meta",
            "nonce",
            "window",
            "active",
            "wfocus",
            "detail",
            "path",
            "program",
            "agent",
            "agent_detail",
            "agent_rev",
            "agent_since_ms",
            "agent_gen",
            "agent_fp",
            "wait",
            "wait_gib",
            "wait_ahead",
            "program_state",
            "human_ms",
            "role",
            "attention",
            "user_title",
            "supervisor_contract",
            "supervisor_scope",
            "supervisor_revision",
            "supervisor",
            "supervisor_proven",
            "supervisor_version",
            "supervisor_vendor",
            "autopilot",
            "path_evidence",
            "copy",
            "upgrade",
            "land_at",
            "task",
            "pause",
            "task_state",
            "task_rev",
        ],
        "{row}"
    );
}

#[test]
fn await_task_wakes_on_a_mark_attention_and_a_close() {
    use crate::session_store::SessionState;
    use crate::session_timeline::{MetaEdit, write_attention_owned};
    use crate::task_record::{Combine, TaskMark, TaskState, mark_session_task};
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let one = |worker: &SessionHandle, since: u64| {
        await_task(
            &store,
            &[TaskTarget {
                local_id: worker.local_id,
                ctx: &worker.ctx,
                since: Some(since),
                facts: false,
                fact_since: None,
            }],
            Combine::Any,
            60_000,
        )
    };
    std::thread::scope(|s| {
        let waiter = s.spawn(|| one(&workers[0], 1));
        let parked = std::time::Instant::now();
        while workers[0].ctx.watchers.count() == 0 {
            assert!(
                parked.elapsed() < std::time::Duration::from_secs(60),
                "the waiter never parked"
            );
            aterm_uds::deadline::poll_yield();
        }
        mark_session_task(&workers[0].ctx, 1, &TaskMark::new(TaskState::Done)).expect("unfenced");
        let reply = waiter.join().expect("the waiter");
        assert_eq!(task_reply(&reply), ("OK 1", vec!["done"]), "{reply}");
    });
    write_attention_owned(
        &workers[1].ctx,
        crate::harness_host::ATTENTION_OWNER,
        MetaEdit::Set("a login"),
        aterm_slog::Actor::Harness,
    )
    .unwrap();
    let reply = one(&workers[1], 1);
    assert!(
        reply.ends_with(
            " rev=2 state=working agent=- program_state=- attention=a%20login \
             result_bytes=0 task_news=1\n"
        ),
        "{reply}"
    );
    store
        .write()
        .unwrap()
        .set_state(workers[2].local_id, SessionState::Exited);
    assert_eq!(task_reply(&one(&workers[2], 1)), ("OK 1", vec!["closed"]));
}

/// A WAIT WAKES NO `await task`: codex-alab's automatic wait (`meta set attention
/// owner=codex-alab-wait kind=wait`) and aterm's harness waiting out a limit under its own
/// owner (`owner=supervisor kind=wait`) leave the worker's record where it stood, so a wait
/// past its rev times out with no news, its row naming the newest wait. NEGATIVE CONTROL:
/// codex-alab's ask (`owner=codex-alab`, no kind) wakes it with `task_news=1`; its row, and the
/// `sessions` row, then name the ask, not the wait written after it.
#[test]
fn a_wait_entry_never_wakes_await_task_and_an_ask_does() {
    use crate::task_record::Combine;
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let w = &workers[0];
    let meta = |rest: &str| cmd_meta(&w.term, &store, w.local_id, &w.ctx, rest).0;
    let rev = task_record_of(&w.ctx).expect("a record").rev;
    let wait = |timeout_ms| {
        await_task(
            &store,
            &[TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: Some(rev),
                facts: false,
                fact_since: None,
            }],
            Combine::Any,
            timeout_ms,
        )
    };
    assert_eq!(
        meta("set attention owner=codex-alab-wait kind=wait waiting out the usage limit"),
        "OK\n"
    );
    assert_eq!(
        meta("set attention owner=supervisor kind=wait limited: resets 15:00"),
        "OK\n"
    );
    let quiet = wait(20);
    assert_eq!(task_reply(&quiet).0, "OK 1 status=timeout", "{quiet}");
    assert!(
        quiet.ends_with(" attention=limited:%20resets%2015:00 result_bytes=0 task_news=0\n"),
        "with no ask, the row names the newest wait: {quiet}"
    );
    assert_eq!(task_record_of(&w.ctx).expect("a record").rev, rev);

    assert_eq!(meta("unset attention owner=supervisor"), "OK\n");
    assert_eq!(
        meta("set attention owner=codex-alab box: Allow this command?"),
        "OK\n"
    );
    assert_eq!(
        meta("set attention owner=codex-alab-wait kind=wait waiting out the outage"),
        "OK\n"
    );
    let woke = wait(60_000);
    assert_eq!(task_reply(&woke), ("OK 1", vec!["working"]), "{woke}");
    assert!(
        woke.ends_with(" attention=box:%20Allow%20this%20command? result_bytes=0 task_news=1\n"),
        "{woke}"
    );
    let roster = sessions_lines(
        std::slice::from_ref(w),
        Ok(&[]),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    assert!(
        roster.contains(" attention=box:%20Allow%20this%20command? "),
        "{roster}"
    );
    assert_eq!(
        w.ctx.meta.lock().unwrap().attention.as_deref(),
        Some("waiting out the outage"),
        "the newest entry is still the wait"
    );
}

/// A SECOND ASK IS NEWS: while a worker's `NEEDS YOU:` stands, a lead parked in `await task`
/// past the record's rev wakes when another ask joins it (a box the harness escalates under
/// `owner=supervisor`, a new needs set written over the old), though the attention flag never
/// fell, and again when an ask taken down is raised once more. NEGATIVE CONTROL: the same
/// needs set written again, a `kind=wait` entry under a task owner and one of two standing
/// asks cleared leave the rev where it stood, so a wait past it times out with no news.
#[test]
fn a_second_ask_under_a_standing_one_wakes_await_task() {
    use crate::task_record::Combine;
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let w = &workers[0];
    let meta = |rest: &str| cmd_meta(&w.term, &store, w.local_id, &w.ctx, rest).0;
    let rev = || task_record_of(&w.ctx).expect("a record").rev;
    let wait = |since: u64, timeout_ms| {
        await_task(
            &store,
            &[TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: Some(since),
                facts: false,
                fact_since: None,
            }],
            Combine::Any,
            timeout_ms,
        )
    };
    let news = |reply: &str| {
        reply
            .lines()
            .nth(1)
            .and_then(|row| row.rsplit_once(" task_news="))
            .map(|(_, bit)| bit.to_string())
    };
    let first = "set attention owner=needs kind=ask claude needs you: a release credential";
    let again = "set attention owner=needs kind=ask claude needs you: the org admin role";
    let the_box = "set attention owner=supervisor box: Allow this command?";
    assert_eq!(meta(first), "OK\n");
    let parked_wakes_on = |write: &str| {
        let since = rev();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| wait(since, 60_000));
            let parked = std::time::Instant::now();
            while w.ctx.watchers.count() == 0 {
                assert!(
                    parked.elapsed() < std::time::Duration::from_secs(60),
                    "the waiter never parked"
                );
                aterm_uds::deadline::poll_yield();
            }
            assert_eq!(meta(write), "OK\n");
            let reply = waiter.join().expect("the waiter");
            assert_eq!(
                task_reply(&reply),
                ("OK 1", vec!["working"]),
                "{write}: {reply}"
            );
            assert_eq!(news(&reply).as_deref(), Some("1"), "{write}: {reply}");
        });
        assert_eq!(rev(), since + 1, "{write}");
    };
    parked_wakes_on(the_box);
    parked_wakes_on(again);

    let since = rev();
    for quiet in [
        again,
        "set attention owner=codex-alab kind=wait waiting out the outage",
        "unset attention owner=supervisor",
    ] {
        assert_eq!(meta(quiet), "OK\n", "{quiet}");
        assert_eq!(rev(), since, "{quiet} is no news");
    }
    let quiet = wait(since, 20);
    assert_eq!(task_reply(&quiet).0, "OK 1 status=timeout", "{quiet}");
    assert_eq!(news(&quiet).as_deref(), Some("0"), "{quiet}");
    parked_wakes_on(the_box);
}

/// A NEED IS AN ASK, NOT THE ENTRY'S WORDS: the needs entry names only a set's first need and a
/// count, so a lead parked past the rev sleeps when one need of the set its loop told the
/// window is taken down though the words change, and wakes when a need joins the set though
/// they hold (its first need and count unchanged). NEGATIVE CONTROL: the same set told and
/// written again moves nothing, and the need taken down raised once more wakes it.
#[test]
fn a_need_joining_a_told_set_wakes_await_task_and_one_taken_down_does_not() {
    use crate::task_record::Combine;
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let w = &workers[0];
    let sid = w.sid.as_str().to_string();
    let rev = || task_record_of(&w.ctx).expect("a record").rev;
    let wait = |since: u64, timeout_ms| {
        await_task(
            &store,
            &[TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: Some(since),
                facts: false,
                fact_since: None,
            }],
            Combine::Any,
            timeout_ms,
        )
    };
    // The loop tells the window the set, then writes its entry (`Session::request_needs`).
    let publish = |needs: &[&str], entry: &str| {
        let needs: Vec<String> = needs.iter().map(|need| (*need).to_string()).collect();
        crate::harness_host::store_needs(&store, &sid, &needs, entry);
        let write = format!("set attention owner=needs kind=ask {entry}");
        assert_eq!(
            cmd_meta(&w.term, &store, w.local_id, &w.ctx, &write).0,
            "OK\n",
            "{entry}"
        );
    };
    let two = "claude needs you: sign 0.48.0 (+1 more) — carrying on";
    let one = "claude needs you: sign 0.48.0 — carrying on";
    let parked_wakes_on = |what: &str, needs: &[&str], entry: &str| {
        let since = rev();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| wait(since, 60_000));
            let parked = std::time::Instant::now();
            while w.ctx.watchers.count() == 0 {
                assert!(
                    parked.elapsed() < std::time::Duration::from_secs(60),
                    "the waiter never parked"
                );
                aterm_uds::deadline::poll_yield();
            }
            publish(needs, entry);
            let reply = waiter.join().expect("the waiter");
            assert!(
                reply.starts_with("OK 1\n") && reply.ends_with(" task_news=1\n"),
                "{what}: {reply}"
            );
        });
        assert_eq!(rev(), since + 1, "{what}");
    };
    publish(&["sign 0.48.0", "grant Full Disk Access"], two);
    let since = rev();
    publish(&["sign 0.48.0"], one);
    assert_eq!(rev(), since, "a need taken down is no news");
    publish(&["sign 0.48.0"], one);
    assert_eq!(rev(), since, "CONTROL: the same set again is no news");
    let quiet = wait(since, 20);
    assert_eq!(task_reply(&quiet).0, "OK 1 status=timeout", "{quiet}");
    parked_wakes_on(
        "CONTROL: a need taken down raised again",
        &["sign 0.48.0", "grant Full Disk Access"],
        two,
    );
    parked_wakes_on(
        "a need joins under the same words",
        &["sign 0.48.0", "restart the Mac"],
        two,
    );
}

/// A SEAMLESS UPDATE CARRIES THE NEEDS ENTRY, NOT THE SET ITS LOOP TOLD: the successor's loop
/// telling the same set and writing the same entry again at its first point (its `reconcile`
/// reads the entry a loop before it left) is no news to a lead parked past the carried
/// record's rev, though the window knew only the entry's words before and knows its needs
/// now. NEGATIVE CONTROL: a need joining the set after that wakes the lead.
#[test]
fn the_same_needs_told_again_after_a_seamless_update_are_no_news() {
    use crate::task_record::Combine;
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let one = "claude needs you: sign 0.48.0 — carrying on";
    let two = "claude needs you: sign 0.48.0 (+1 more) — carrying on";
    // The loop tells the window the set, then writes its entry (`Session::request_needs`).
    let publish = |w: &SessionHandle, needs: &[&str], entry: &str| {
        let needs: Vec<String> = needs.iter().map(|need| (*need).to_string()).collect();
        crate::harness_host::store_needs(&store, w.sid.as_str(), &needs, entry);
        let write = format!("set attention owner=needs kind=ask {entry}");
        assert_eq!(
            cmd_meta(&w.term, &store, w.local_id, &w.ctx, &write).0,
            "OK\n",
            "{entry}"
        );
    };
    publish(&workers[0], &["sign 0.48.0"], one);
    let row = task_record_of(&workers[0].ctx)
        .expect("a record")
        .carry_row(std::time::Instant::now());
    let escalations = workers[0]
        .ctx
        .meta
        .lock()
        .unwrap()
        .handoff_attention_owners();
    // The successor adopts the session as `spawn` does: the escalations, then the record.
    let w = crate::session_store::test_handle(4);
    crate::session_timeline::seed_carried_claims(
        &w.ctx.meta,
        &w.ctx.timeline,
        None,
        &escalations,
        crate::metrics::now_us(),
    );
    crate::task_record::seed_carried(&w.ctx, Some(&row));
    store.write().unwrap().register(w.clone());
    let rev = || task_record_of(&w.ctx).expect("a record").rev;
    let carried = task_record_of(&w.ctx).expect("a carried record");
    assert!(carried.attention, "the carried need stands");
    let wait = |since: u64, timeout_ms| {
        await_task(
            &store,
            &[TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: Some(since),
                facts: false,
                fact_since: None,
            }],
            Combine::Any,
            timeout_ms,
        )
    };
    let since = carried.rev;
    publish(&w, &["sign 0.48.0"], one);
    assert_eq!(rev(), since, "the same needs told again are no news");
    let quiet = wait(since, 20);
    assert_eq!(task_reply(&quiet).0, "OK 1 status=timeout", "{quiet}");

    std::thread::scope(|s| {
        let waiter = s.spawn(|| wait(since, 60_000));
        let parked = std::time::Instant::now();
        while w.ctx.watchers.count() == 0 {
            assert!(
                parked.elapsed() < std::time::Duration::from_secs(60),
                "the waiter never parked"
            );
            aterm_uds::deadline::poll_yield();
        }
        publish(&w, &["sign 0.48.0", "grant Full Disk Access"], two);
        let reply = waiter.join().expect("the waiter");
        assert!(
            reply.starts_with("OK 1\n") && reply.ends_with(" task_news=1\n"),
            "CONTROL: a need joins: {reply}"
        );
    });
    assert_eq!(rev(), since + 1, "CONTROL: a need joins");
}

/// THE FACT WORD IS NO NEWS TO A DEFAULT WAIT (the occupancy design's manager door): a
/// worker whose supervisor attention stands, waited on past its rev, is not woken by a fact
/// edit, which never moves `rev`. NEGATIVE CONTROL: `NoteBumpsNews` (the edit moves `rev`
/// too) wakes it at once.
#[test]
fn a_fact_edit_never_wakes_a_default_await_task() {
    use crate::occupancy_host::fault::{Fault, with};
    use crate::session_timeline::{MetaEdit, write_attention_owned};
    use crate::task_record::{Combine, mark_session_task_fact};
    use aterm_agent::supervise::occupancy::{FactWord, NoteOutcome, RuleId};
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let w = &workers[0];
    write_attention_owned(
        &w.ctx,
        crate::harness_host::ATTENTION_OWNER,
        MetaEdit::Set("a login"),
        aterm_slog::Actor::Harness,
    )
    .unwrap();
    let rev = task_record_of(&w.ctx).expect("a record").rev;
    let wait = |timeout_ms| {
        await_task(
            &store,
            &[TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: Some(rev),
                facts: false,
                fact_since: None,
            }],
            Combine::Any,
            timeout_ms,
        )
    };
    assert_eq!(task_reply(&wait(20)).0, "OK 1 status=timeout");
    let word = |n| FactWord::new(RuleId::HoldRoom, n);
    let fence = std::time::Duration::ZERO;
    assert_eq!(
        mark_session_task_fact(&w.ctx, w.local_id, word(1), fence),
        NoteOutcome::Noted
    );
    let quiet = wait(20);
    assert_eq!(task_reply(&quiet).0, "OK 1 status=timeout", "{quiet}");
    assert!(
        quiet.contains(" fact=hold-room@v1#1 fact_rev=1 task_news=0\n"),
        "the row shows the word: {quiet}"
    );
    assert_eq!(task_record_of(&w.ctx).map(|t| t.rev), Some(rev));
    with(Fault::NoteBumpsNews, || {
        mark_session_task_fact(&w.ctx, w.local_id, word(2), fence)
    });
    let woken = wait(60_000);
    assert_eq!(
        task_reply(&woken).0,
        "OK 1",
        "CONTROL: a fact edit that moves the rev wakes it: {woken}"
    );
}

/// `facts=1`: a waiter parked on a worker's fact word is woken once by its note, a wait
/// past the noted `fact_rev` is quiet, and a clear wakes none. The parked waiter names its
/// fact edge (`fact_since=0`): a default edge is read after the waiter subscribes, so a note
/// landing between the two would be the edge itself, and the answer would hang on the race.
#[test]
fn facts_1_is_woken_once_by_a_note_and_never_by_a_clear() {
    use crate::task_record::{Combine, mark_session_task_fact};
    use aterm_agent::supervise::occupancy::{FactWord, NoteOutcome, RuleId};
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let w = &workers[0];
    let wait = |fact_since: Option<u64>, timeout_ms| {
        await_task(
            &store,
            &[TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: None,
                facts: true,
                fact_since,
            }],
            Combine::Any,
            timeout_ms,
        )
    };
    let note = |word: Option<FactWord>| {
        mark_session_task_fact(&w.ctx, w.local_id, word, std::time::Duration::ZERO)
    };
    assert_eq!(task_reply(&wait(None, 20)).0, "OK 1 status=timeout");
    std::thread::scope(|s| {
        let waiter = s.spawn(|| wait(Some(0), 60_000));
        let parked = std::time::Instant::now();
        while w.ctx.watchers.count() == 0 {
            assert!(
                parked.elapsed() < std::time::Duration::from_secs(60),
                "the waiter never parked"
            );
            aterm_uds::deadline::poll_yield();
        }
        assert_eq!(note(FactWord::new(RuleId::HoldRoom, 1)), NoteOutcome::Noted);
        let reply = waiter.join().expect("the waiter");
        assert_eq!(task_reply(&reply).0, "OK 1", "{reply}");
        assert!(
            reply.ends_with(
                " rev=1 state=working agent=- program_state=- attention=- result_bytes=0 \
                 fact=hold-room@v1#1 fact_rev=1 task_news=1\n"
            ),
            "{reply}"
        );
    });
    assert_eq!(
        task_reply(&wait(Some(1), 20)).0,
        "OK 1 status=timeout",
        "the note was read"
    );
    assert_eq!(
        task_reply(&wait(None, 20)).0,
        "OK 1 status=timeout",
        "armed past the note"
    );
    assert_eq!(note(FactWord::new(RuleId::HoldRoom, 1)), NoteOutcome::Noted);
    assert_eq!(
        task_reply(&wait(Some(1), 20)).0,
        "OK 1 status=timeout",
        "the same word again moves nothing"
    );
    assert_eq!(note(None), NoteOutcome::Cleared);
    let cleared = wait(Some(1), 20);
    assert_eq!(task_reply(&cleared).0, "OK 1 status=timeout", "{cleared}");
    assert!(
        cleared.ends_with(" fact=- fact_rev=2 task_news=0\n"),
        "{cleared}"
    );
    assert_eq!(
        task_reply(&wait(Some(0), 20)).0,
        "OK 1 status=timeout",
        "past an old note, the cleared word is no news"
    );
    assert_eq!(
        note(FactWord::new(RuleId::WaitNudge, 1)),
        NoteOutcome::Noted
    );
    assert_eq!(
        task_reply(&wait(Some(2), 60_000)).0,
        "OK 1",
        "CONTROL: the next note past the clear wakes it"
    );
    let verb = |rest: &str| cmd_await(&w.term, &store, w.local_id, &w.ctx, rest);
    assert!(
        verb("task facts=1 fact_since=2 timeout=60000")
            .ends_with(" fact=wait-nudge@v1#1 fact_rev=3 task_news=1\n"),
        "the single form reads facts=1"
    );
    assert_eq!(
        task_reply(&verb("task facts=1 timeout=20")).0,
        "OK 1 status=timeout"
    );
    assert_eq!(
        verb("task fact_since=2"),
        crate::task_record::AWAIT_TASK_USAGE,
        "a fact edge names facts=1"
    );
}

#[test]
fn the_single_await_task_form_waits_on_its_session() {
    use crate::task_record::{AWAIT_TASK_USAGE, TaskMark, TaskState, mark_session_task};
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let w = &workers[0];
    mark_session_task(&w.ctx, w.local_id, &TaskMark::new(TaskState::Done)).expect("unfenced");
    let await_ = |rest: &str| cmd_await(&w.term, &store, w.local_id, &w.ctx, rest);
    assert!(
        await_("task since=1 timeout=60000").starts_with("OK 1\n@"),
        "the done news"
    );
    assert_eq!(
        task_reply(&await_("task timeout=20")).0,
        "OK 1 status=timeout",
        "armed at rev 2, the news is already read"
    );
    assert_eq!(await_("task @s-other since=1"), AWAIT_TASK_USAGE);
    assert_eq!(await_("task since=x"), AWAIT_TASK_USAGE);
    assert_eq!(await_("timeout=20 task"), AWAIT_TASK_USAGE);
    assert_eq!(await_("timeout 20 task since=1"), AWAIT_TASK_USAGE);
}

#[test]
fn await_task_latches_news_that_comes_and_goes() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let store = crate::session_store::new_store();
    let workers = three_workers(&store);
    let mark = |worker: &SessionHandle, state: TaskState| {
        mark_session_task(&worker.ctx, worker.local_id, &TaskMark::new(state)).expect("unfenced");
    };
    let targets: Vec<TaskTarget<'_>> = workers
        .iter()
        .map(|w| TaskTarget {
            local_id: w.local_id,
            ctx: &w.ctx,
            since: Some(1),
            facts: false,
            fact_since: None,
        })
        .collect();
    let edges = [(1, None); 3];
    let exits: Vec<_> = workers
        .iter()
        .map(|w| exit_probe(&store, w.local_id))
        .collect();
    let mut latched: Vec<Option<String>> = vec![None; 3];
    mark(&workers[0], TaskState::Done);
    latch_task_news(&store, &targets, &edges, &exits, &mut latched);
    let done = latched[0].clone().expect("the done news latches");
    assert!(done.contains(" rev=2 state=done "), "{done}");
    assert!(latched[1].is_none() && latched[2].is_none());
    mark(&workers[0], TaskState::Working);
    mark(&workers[1], TaskState::Done);
    latch_task_news(&store, &targets, &edges, &exits, &mut latched);
    assert_eq!(latched[0].as_deref(), Some(done.as_str()), "the news holds");
    assert!(
        latched[1]
            .as_deref()
            .is_some_and(|row| row.contains(" rev=2 state=done ")),
        "{latched:?}"
    );
    assert!(latched[2].is_none());
    let mut again: Vec<Option<String>> = vec![None; 3];
    latch_task_news(&store, &targets, &edges, &exits, &mut again);
    assert!(again[0].is_none() && again[1].is_some(), "{again:?}");
}

/// A PROVEN SELF-CLAIM ADMITS ITS CODEX BEFORE THE SWEEP NAMES THE PROGRAM (CONTRACT §1 of
/// codex-alab; aterm's `spawn agent=codex` lab leg, 2026-10-23): the worker claimed while
/// `status` printed `autopilot=-`, so it decided under Cruise in a Land session until the
/// sweep admitted it. The proof read the claimant leading the foreground group and running an
/// ALab build, so the session's own word is printed at the claim. NEGATIVE CONTROL: the same
/// holder's claim unproven prints `-`, as nothing admits it yet.
#[test]
fn a_proven_self_claim_prints_its_sessions_word_before_the_sweep_names_it() {
    use aterm_agent::supervise::Autopilot;
    let me = std::process::id();
    let line = |h: &SessionHandle| {
        sessions_lines(
            std::slice::from_ref(h),
            Err("no event loop"),
            crate::autopilot::HarnessPosture::On(Autopilot::Cruise),
            |_| true,
        )
        .lines()
        .nth(1)
        .unwrap()
        .to_string()
    };
    for (local, proven, word) in [(1, true, "land"), (2, false, "-")] {
        let t = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let h = handle(local, &t);
        let _ = crate::session_timeline::apply_autopilot(
            &h.ctx,
            Some("land".to_string()),
            None,
            aterm_slog::Actor::Ctl,
            None,
        );
        crate::session_timeline::claim_supervisor_as(
            &h.ctx,
            &format!("codex-alab@{me}"),
            Some(local),
            None,
            proven.then(|| crate::self_claim::Proven {
                pid: me,
                start: "a test identity".to_string(),
            }),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("the claim");
        assert_eq!(
            crate::harness_host::agent_of(h.ctx.timeline.lock().unwrap().agent()),
            None,
            "the sweep has named no program"
        );
        let status = line(&h);
        assert!(
            status.contains(&format!(" autopilot={word} ")),
            "proven={proven}: {status}"
        );
    }
}

#[test]
fn the_roster_path_is_measured_where_a_child_of_the_shell_was_read() {
    use crate::session_status::program::PathVerdict;
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let h = handle(0, &t0);
    let line = |h: &SessionHandle| {
        sessions_lines(
            std::slice::from_ref(h),
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |_| true,
        )
        .lines()
        .nth(1)
        .unwrap()
        .to_string()
    };
    let carried = line(&h);
    assert!(carried.contains(" path=frozen program=- "), "{carried}");
    assert!(
        carried.ends_with(
            " path_evidence=carried copy=- upgrade=- land_at=- task=- pause=- task_state=- task_rev=0"
        ),
        "{carried}"
    );
    {
        let mut tl = h.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_leader(8, Some("managed"), Some(PathVerdict::Live));
    }
    let late = line(&h);
    assert!(
        late.contains(" path=frozen program=- ")
            && late.ends_with(
                " path_evidence=carried copy=- upgrade=- land_at=- task=- pause=- task_state=- \
                 task_rev=0"
            ),
        "a late reading for another group changes nothing: {late}"
    );
    {
        let mut tl = h.ctx.timeline.lock().unwrap();
        tl.set_program(7, Some("claude".into()));
        tl.set_leader(7, Some("foreign"), Some(PathVerdict::Live));
        tl.set_upgrade(Some(aterm_agent::harness::upgrade_drive::Row {
            tab: h.sid.as_str().to_string(),
            to: "2.1.282".into(),
            phase: aterm_agent::harness::upgrade::Phase::Pending,
            behind_since: crate::upgrade_host::now_s() - 120,
            wait: "not-idle:busy".into(),
            ..Default::default()
        }));
    }
    let measured = line(&h);
    assert!(
        measured.contains(" path=live program=claude "),
        "{measured}"
    );
    assert!(measured.contains(" path_evidence=measured:"), "{measured}");
    assert!(
        measured.ends_with(
            " copy=foreign upgrade=pending/2.1.282/not-idle:busy/2m land_at=- task=- \
             pause=- task_state=- task_rev=0"
        ),
        "{measured}"
    );
    let mut tl = h.ctx.timeline.lock().unwrap();
    assert!(tl.take_path_lowered(), "live: lower the mark");
    assert!(!tl.take_path_lowered(), "owed once");
    tl.set_leader(7, Some("foreign"), Some(PathVerdict::Frozen));
    assert!(!tl.take_path_lowered(), "frozen: nothing is raised");
    assert!(!tl.path_settled_frozen(), "one job is not the shell");
    tl.note_foreground_group(9);
    assert_eq!(tl.copy(), None, "the copy was the old leader's");
    assert!(
        tl.path_reading().is_some(),
        "the PATH is the shell's: it outlives the job"
    );
}

#[test]
fn the_roster_carries_the_published_program_and_agent_verdict() {
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let h = handle(0, &t0);
    let out = sessions_lines(
        std::slice::from_ref(&h),
        Ok(&[]),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let line = out.lines().nth(1).unwrap();
    assert!(
        line.contains(
            " supervisor=- supervisor_proven=0 supervisor_version=- supervisor_vendor=- \
             autopilot=- "
        ),
        "no agent: {line}"
    );
    let (_, cols) = line.split_once(" path=live ").expect("after path=");
    assert!(
        cols.starts_with("program=- agent=- agent_detail=- agent_rev=0 agent_since_ms="),
        "{line}"
    );
    assert!(
        cols.contains(" agent_gen=- agent_fp=- "),
        "no read yet, no stamp: {line}"
    );
    {
        let mut tl = h.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
        tl.publish_agent(
            "prompt",
            Some("bash:not-read-only".into()),
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 2, seq: 40 },
                fp: 0xab,
            },
        );
    }
    let out = sessions_lines(
        std::slice::from_ref(&h),
        Err("no event loop"),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let line = out.lines().nth(1).unwrap();
    assert!(
        line.contains(
            " path=live program=claude agent=prompt agent_detail=bash:not-read-only \
             agent_rev=1 agent_since_ms="
        ),
        "the failed-hop arm carries them too: {line}"
    );
    assert!(
        line.contains(" agent_gen=2.40 agent_fp=00000000000000ab "),
        "the verdict's stamp rides the row: {line}"
    );
    let unclaimed_fields = [
        ("supervisor", "-"),
        ("supervisor_proven", "0"),
        ("supervisor_version", "-"),
        ("supervisor_vendor", "-"),
        ("autopilot", "off"),
        ("path_evidence", "-"),
        ("copy", "-"),
        ("upgrade", "-"),
        ("land_at", "-"),
        ("task", "-"),
        ("pause", "-"),
        ("task_state", "-"),
        ("task_rev", "0"),
    ];
    assert_roster_fields(
        line,
        &[
            ("agent_fp", "00000000000000ab"),
            ("wait", "-"),
            ("wait_gib", "-"),
            ("wait_ahead", "-"),
            ("program_state", "-"),
            ("human_ms", "-"),
            ("role", "-"),
            ("attention", "-"),
            ("user_title", "-"),
        ],
    );
    assert_roster_fields(line, &unclaimed_fields);
    crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
    let out = sessions_lines(
        std::slice::from_ref(&h),
        Err("no event loop"),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let line = out.lines().nth(1).unwrap();
    let human = line
        .split_whitespace()
        .find_map(|t| t.strip_prefix("human_ms="))
        .unwrap();
    assert!(human.parse::<u64>().is_ok(), "{line}");
    assert_roster_fields(line, &unclaimed_fields);
    let cruise = crate::autopilot::HarnessPosture::On(aterm_agent::supervise::Autopilot::Cruise);
    let autopilot = |posture| {
        let out = sessions_lines(std::slice::from_ref(&h), Ok(&[]), posture, |_| false);
        out.lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .find_map(|t| t.strip_prefix("autopilot="))
            .unwrap()
            .to_string()
    };
    assert_eq!(autopilot(cruise), "cruise");
    h.ctx
        .meta
        .lock()
        .unwrap()
        .set("autopilot", Some("land".into()));
    assert_eq!(autopilot(cruise), "land");
    assert_eq!(
        autopilot(crate::autopilot::HarnessPosture::MasterOff),
        "off",
        "the master switch wins over the word"
    );
}

#[test]
fn sessions_lines_carry_the_placement_columns_from_the_rows() {
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let t2 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let snapshot = vec![handle(0, &t0), handle(1, &t1), handle(2, &t2)];
    let rows = vec![row(0, 0, true, false), row(1, 3, false, true)];
    let out = sessions_lines(
        &snapshot,
        Ok(&rows),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "OK 3");
    assert_eq!(
        tail(lines[1]),
        "0 - alive tab-0 meta=0 window=0 active=1 wfocus=0 detail=- path=live"
    );
    assert_eq!(
        tail(lines[2]),
        "1 - alive tab-1 meta=0 window=3 active=0 wfocus=1 detail=- path=live"
    );
    assert_eq!(
        tail(lines[3]),
        "2 - alive tab-2 meta=0 window=none active=0 wfocus=0 detail=- path=live"
    );
}

/// THE ROSTER NAMES A WAITING FRONT'S WAIT, as `status` does: a tab whose leader in front is a
/// build's front waiting for memory reads `program=targo … wait=memory wait_gib=12.0
/// wait_ahead=1` between the agent's columns and `program_state=`, so `ls` never reads it as
/// running. NEGATIVE CONTROL: once admitted (its wait read away) the same row says `wait=-`.
#[test]
fn sessions_lines_name_a_waiting_fronts_wait() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let snapshot = vec![handle(0, &term)];
    let rows = vec![row(0, 0, true, true)];
    let wait = crate::session_status::program::BuildWait {
        mem: Some(12 << 30),
        ahead: Some(1),
    };
    let line = |wait: Option<crate::session_status::program::BuildWait>| {
        {
            let mut timeline = snapshot[0].ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(5100);
            timeline.set_program(5100, Some("targo".to_owned()));
            timeline.set_build_wait(5100, wait);
        }
        sessions_lines(
            &snapshot,
            Ok(&rows),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        )
    };
    let waiting = line(Some(wait));
    assert!(waiting.contains(" program=targo "), "{waiting}");
    assert!(
        waiting.contains(" wait=memory wait_gib=12.0 wait_ahead=1 program_state="),
        "{waiting}"
    );
    let admitted = line(None);
    assert!(
        admitted.contains(" wait=- wait_gib=- wait_ahead=- program_state="),
        "{admitted}"
    );
}

#[test]
fn sessions_lines_print_the_title_the_strip_settles_on() {
    let bare = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let fresh = Arc::new(Mutex::new(Terminal::new(24, 80)));
    {
        let mut t = fresh.lock().unwrap();
        t.set_title("C:\\Program Files\\PowerShell\\7\\pwsh.exe");
        t.process(b"\x1b]7;file://localhost/srv/demo-cwd\x07");
    }
    let settled = Arc::new(Mutex::new(Terminal::new(24, 80)));
    settled
        .lock()
        .unwrap()
        .set_title("user@m17-tower: ~\\aterm");
    let running = Arc::new(Mutex::new(Terminal::new(24, 80)));
    running
        .lock()
        .unwrap()
        .set_title("C:\\Windows\\system32\\cmd.exe - ping build-01");
    let named = Arc::new(Mutex::new(Terminal::new(24, 80)));
    named.lock().unwrap().set_title("~\\aterm");
    let snapshot = vec![
        handle(0, &bare),
        handle(1, &fresh),
        handle(2, &settled),
        handle(3, &running),
        handle(4, &named),
    ];
    snapshot[4].ctx.meta.lock().unwrap().user_title = Some("ops shell".to_string());
    let out = sessions_lines(
        &snapshot,
        Err("no event loop"),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let titles: Vec<&str> = out
        .lines()
        .skip(1)
        .map(|line| line.split_whitespace().nth(4).unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "tab-0",
            "/srv/demo-cwd",
            "user@m17-tower:%20~\\aterm",
            "ping%20build-01",
            "ops%20shell"
        ]
    );

    let mut stale = handle(5, &fresh);
    stale.title = "C:\\Windows\\system32\\cmd.exe".to_string();
    let mut empty = handle(6, &fresh);
    empty.title.clear();
    let held = fresh.lock().unwrap();
    let out = sessions_lines(
        &[stale, empty],
        Err("no event loop"),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    drop(held);
    let titles: Vec<&str> = out
        .lines()
        .skip(1)
        .map(|line| line.split_whitespace().nth(4).unwrap())
        .collect();
    assert_eq!(titles, ["aterm", "aterm"]);
    for line in out.lines().skip(1) {
        assert_eq!(
            line.split_whitespace().nth(5),
            Some("meta=0"),
            "every column keeps its place: {line:?}"
        );
    }
}

#[test]
fn family_prints_the_roster_title() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    {
        let mut t = term.lock().unwrap();
        t.set_title("C:\\Program Files\\PowerShell\\7\\pwsh.exe");
        t.process(b"\x1b]7;file://localhost/srv/demo-cwd\x07");
    }
    let h = handle(0, &term);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    let family = cmd_family(&h.ctx, &store, Scope::Owner, "");
    assert_eq!(
        family.lines().nth(1),
        Some(format!("self {} alive /srv/demo-cwd", h.sid.as_str()).as_str()),
        "{family}"
    );
    let sessions = sessions_lines(
        &[h],
        Err("no event loop"),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    assert_eq!(
        sessions
            .lines()
            .nth(1)
            .and_then(|line| line.split_whitespace().nth(4)),
        Some("/srv/demo-cwd"),
        "{sessions}"
    );
}

#[test]
fn sessions_lines_keep_every_line_when_the_hop_fails() {
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    t1.lock()
        .unwrap()
        .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 30\n\x1b]133;C\x07");
    let snapshot = vec![handle(0, &t0), handle(1, &t1)];
    snapshot[1].ctx.ran.hear_running("sleep");
    let out = sessions_lines(
        &snapshot,
        Err("main-thread reply did not arrive within 30s"),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "OK 2");
    assert_eq!(
        tail(lines[1]),
        "0 - alive tab-0 meta=0 window=- active=- wfocus=- detail=- path=live"
    );
    assert_eq!(
        tail(lines[2]),
        "1 - alive tab-1 meta=0 window=- active=- wfocus=- detail=sleep path=live"
    );
}

#[test]
fn the_roster_carries_each_sessions_public_launch_nonce() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let h = handle(0, &term);
    let expect = h.nonce.to_hex();
    let out = sessions_lines(
        &[h],
        Err("no event loop"),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    let row = out.lines().nth(1).expect("one session row");
    assert!(
        row.contains(&format!(" nonce={expect} ")),
        "the roster must carry the launch nonce: {row}"
    );
    assert_eq!(expect.len(), 32, "{expect}");
    let fields: Vec<&str> = row.split_whitespace().collect();
    let meta_at = fields.iter().position(|f| f.starts_with("meta=")).unwrap();
    let nonce_at = fields.iter().position(|f| f.starts_with("nonce=")).unwrap();
    assert_eq!(nonce_at, meta_at + 1, "{row}");
}

#[test]
fn bridge_roster_matches_public_identities_without_per_session_locks() {
    let store = crate::session_store::new_store();
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let h0 = handle(9, &t0);
    let h1 = handle(2, &t1);
    {
        let mut g = store.write().unwrap();
        g.register(h0.clone());
        g.register(h1.clone());
    }
    let full = cmd_sessions_store(&store, None);
    let expected: Vec<String> = full
        .lines()
        .skip(1)
        .map(|row| {
            let mut cols = row.split_whitespace();
            let local = cols.next().unwrap();
            let sid = cols.next().unwrap();
            let nonce = cols.find(|tok| tok.starts_with("nonce=")).unwrap();
            format!("{local} {sid} {nonce}")
        })
        .collect();
    assert_eq!(expected.len(), 2);

    let _term = t0.lock().unwrap();
    let _meta = h0.ctx.meta.lock().unwrap();
    let _timeline = h0.ctx.timeline.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let _ = tx.send(cmd_sessions_bridge(&store));
    });
    let bridge = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("identity-only roster must not wait on terminal, meta or timeline");
    let mut lines = bridge.lines();
    assert_eq!(lines.next(), Some("OK 2 roster=2"));
    assert_eq!(lines.map(str::to_string).collect::<Vec<_>>(), expected);
    assert!(!bridge.contains("window=") && !bridge.contains("detail="));
    worker.join().unwrap();
}

#[test]
fn sessions_lines_detail_is_the_running_command_or_a_dash() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    term.lock().unwrap().process(
        b"\x1b]133;A\x07$ \x1b]133;B\x07targo --unverified test -p aterm-gui\n\x1b]133;C\x07",
    );
    let snapshot = vec![handle(0, &term)];
    let rows = vec![row(0, 0, true, true)];
    let out = sessions_lines(
        &snapshot,
        Ok(&rows),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    assert!(
        out.contains(" detail=- path=live "),
        "nothing named targo heard running yet: {out}"
    );
    snapshot[0].ctx.ran.hear_running("targo");
    let out = sessions_lines(
        &snapshot,
        Ok(&rows),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    assert!(
        out.contains(" window=0 active=1 wfocus=1 detail=targo%20test path=live "),
        "{out}"
    );
    assert!(
        !out.contains("aterm-gui"),
        "arguments never reach the wire: {out}"
    );

    let held = term.lock().unwrap();
    let out = sessions_lines(
        &snapshot,
        Ok(&rows),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    assert!(
        out.contains(" detail=- path=live "),
        "contended reads `-` (and the published columns need no terminal lock): {out}"
    );
    drop(held);

    term.lock().unwrap().process(b"done\n\x1b]133;D;0\x07");
    let out = sessions_lines(
        &snapshot,
        Ok(&rows),
        crate::autopilot::HarnessPosture::default(),
        |_| false,
    );
    assert!(
        out.contains(" detail=- path=live "),
        "complete reads `-`: {out}"
    );
}

#[test]
fn sessions_lines_name_the_frozen_path_tab_as_the_last_column() {
    let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let snapshot = vec![handle(0, &t0), handle(1, &t1)];
    let rows = vec![row(0, 0, true, true), row(1, 0, false, true)];
    let out = sessions_lines(
        &snapshot,
        Ok(&rows),
        crate::autopilot::HarnessPosture::default(),
        |id| id == 0,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        tail(lines[1]),
        "0 - alive tab-0 meta=0 window=0 active=1 wfocus=1 detail=- path=frozen"
    );
    assert_eq!(
        tail(lines[2]),
        "1 - alive tab-1 meta=0 window=0 active=0 wfocus=1 detail=- path=live"
    );
    let out = sessions_lines(
        &snapshot,
        Err("no event loop"),
        crate::autopilot::HarnessPosture::default(),
        |id| id == 0,
    );
    assert!(
        out.lines()
            .nth(1)
            .unwrap()
            .contains(" detail=- path=frozen program=-"),
        "the failed-hop arm carries the mark too: {out}"
    );
    let store = crate::session_store::new_store();
    {
        let mut g = store.write().unwrap();
        g.register(handle(7, &t0));
        g.register(handle(8, &t1));
        g.mark_frozen_path(7);
    }
    let out = cmd_sessions_store(&store, None);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "OK 2");
    assert!(
        tail(lines[1]).ends_with(" detail=- path=frozen"),
        "the marked session reads frozen: {out}"
    );
    assert!(
        tail(lines[2]).ends_with(" detail=- path=live"),
        "an unmarked session reads live: {out}"
    );
}

fn temporal_ctx() -> crate::SessionCtx {
    let observed = crate::session_watch::Observed::default();
    crate::SessionCtx {
        sink: Arc::new(aterm_session::sink::SinkWriter::new(-1)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: Arc::default(),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(std::sync::Mutex::new(EdgeTable::new())),
        turn_lease: std::sync::Mutex::new(None),
        self_id: SessionId::generate(),
        nonce: LaunchNonce::generate(),
        cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
            80, 24,
        ))),
        temporal: Arc::new(std::sync::Mutex::new(
            crate::temporal::TemporalRecorder::new(),
        )),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: observed.turns,
        meta: Arc::default(),
        timeline: observed.timeline,
        fabric: std::sync::Arc::default(),
        idem: Default::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        licence: Default::default(),
        launch: Default::default(),
        reset_lane: Default::default(),
        crash_journal: Default::default(),
        typed: Default::default(),
        bells: Default::default(),
        screen_moves: Default::default(),
        job_feed: Default::default(),
        ran: Default::default(),
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        witness: Default::default(),
        fenced_writes: Default::default(),
        native_policy: Default::default(),
        spawn_mark: Default::default(),
    }
}

#[test]
fn temporal_trim_byte_frames_the_trimmed_body_and_status_trim_is_usage() {
    let ctx = temporal_ctx();
    let rows: usize = 8;
    let live = Mutex::new(aterm_core::terminal::Terminal::new(rows as u16, 20));
    let input = b"first line\r\nsecond line\r\n";
    let (base, at) = {
        let mut t = term_lock(&live);
        let base = (t.checkpoint(), crate::temporal::SpinePre::take(&t));
        t.process(input);
        base
    };
    {
        let mut rec = ctx.temporal.lock().unwrap();
        rec.record_keyframe(base);
        rec.seed_spine(at);
        rec.record_raw_in(input);
    }

    let bare = cmd_temporal(&ctx, &live, "");
    assert!(bare.starts_with("OK "), "bare temporal: {bare:?}");

    let trimmed = cmd_temporal(&ctx, &live, "trim");
    let (header, body) = trimmed
        .split_once('\n')
        .expect("a header line then the body");
    let mut tok = header.split_whitespace();
    assert_eq!(tok.next(), Some("OK"));
    let nbytes: usize = tok
        .next()
        .expect("byte count")
        .parse()
        .expect("nbytes is a number");
    assert_eq!(tok.next(), Some("trimmed=6"), "six blank rows dropped");
    assert_eq!(nbytes, body.len(), "header nbytes == body bytes");
    assert_eq!(body.lines().count(), 2, "only the two written rows survive");
    assert!(
        body.trim_end_matches('\n').ends_with("second line"),
        "body ends on the last non-blank row: {body:?}"
    );

    let status_trim = cmd_temporal(&ctx, &live, "status trim");
    assert!(
        status_trim.starts_with("ERR usage: temporal"),
        "status trim is a usage error: {status_trim:?}"
    );
}

#[test]
fn temporal_status_says_where_replay_starts() {
    let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 20));
    let reachable_from = |status: &str| {
        status
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix("reachable_from="))
            .map(str::to_owned)
    };
    let ctx = temporal_ctx();
    let base = term_lock(&live).checkpoint();
    let tick = {
        let mut rec = ctx.temporal.lock().unwrap();
        rec.record_keyframe(base);
        rec.record_raw_in(b"a");
        rec.reachable_from().expect("the keyframe replays")
    };
    let status = cmd_temporal(&ctx, &live, "status");
    assert!(status.starts_with("OK enabled=true "), "{status}");
    assert_eq!(reachable_from(&status), Some(tick.0.to_string()));

    let dead = temporal_ctx();
    dead.temporal.lock().unwrap().record_raw_in(b"a");
    let status = cmd_temporal(&dead, &live, "status");
    assert!(status.starts_with("OK enabled=true "), "{status}");
    assert_eq!(reachable_from(&status).as_deref(), Some("-"));
    assert_eq!(cmd_temporal(&dead, &live, ""), "ERR temporal unreachable\n");
}

#[test]
fn temporal_replays_with_the_recorder_unlocked() {
    use std::cell::Cell;
    use std::rc::Rc;
    let ctx = temporal_ctx();
    let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 20));
    {
        let mut rec = ctx.temporal.lock().unwrap();
        rec.record_keyframe(term_lock(&live).checkpoint());
        rec.record_raw_in(b"a");
    }
    let held = Rc::new(Cell::new(None));
    let _hook = crate::temporal::replay_hook::install({
        let (temporal, held) = (ctx.temporal.clone(), held.clone());
        move || held.set(Some(temporal.try_lock().is_err()))
    });
    assert!(cmd_temporal(&ctx, &live, "").starts_with("OK "));
    assert_eq!(held.take(), Some(false), "replayed with the recorder held");
    let rec = ctx.temporal.lock().unwrap();
    let _ = rec.replay_plan(None).map(crate::temporal::ReplayPlan::run);
    drop(rec);
    assert_eq!(held.take(), Some(true), "the probe sees a held recorder");
}

/// `temporal status` counts the live witnesses a `temporal` fold or `temporal judge` judged and those still pending: a recorded session's all match; replayed on the live clock one diverges and the next slice records a live keyframe.
#[test]
fn temporal_status_counts_the_witnesses_a_replay_judged() {
    use crate::temporal::{InputSlice, SpineMark, SpinePre};
    use aterm_core::terminal::ClockReading;
    let key = |status: &str, key: &str| {
        status
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix(key))
            .map(str::to_owned)
    };
    for (live_clock, judge) in [(false, false), (true, false), (false, true), (true, true)] {
        let ctx = temporal_ctx();
        let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 30));
        let watermark = {
            let t = term_lock(&live);
            let mut rec = ctx.temporal.lock().unwrap();
            rec.record_keyframe(t.checkpoint());
            rec.seed_spine(SpinePre::take(&t));
            rec.spine_watermark()
        };
        let (tx, rx) = crate::temporal::queue(64);
        let mut mark = SpineMark::attach(watermark);
        let t0 = ClockReading::now();
        let mut hold = |bytes: &[u8], ms: u64| {
            let at = ClockReading {
                monotonic: t0.monotonic + std::time::Duration::from_millis(ms),
                wall_ms: t0.wall_ms.map(|w| w + ms),
            };
            {
                let mut t = term_lock(&live);
                let pre = SpinePre::take(&t);
                t.process_at(bytes, at);
                mark.record(&tx, pre, &t, InputSlice::at(bytes, at));
            }
            let mut rec = ctx.temporal.lock().unwrap();
            while let Some(msg) = rx.try_recv() {
                rec.apply(msg);
            }
        };
        hold(b"\x1b[?2026hframe", 0);
        hold(b"\r\none", 1500);
        let status = cmd_temporal(&ctx, &live, "status");
        assert_eq!(key(&status, "witnessed=").as_deref(), Some("0"), "{status}");
        assert_eq!(key(&status, "pending=").as_deref(), Some("1"), "{status}");
        let _clock = live_clock.then(crate::temporal::replay_clock::on_live_clock);
        if judge {
            assert_eq!(
                cmd_temporal(&ctx, &live, "judge"),
                format!(
                    "OK judged=1 diverged={} pending=0 unjudged_evicted=0\n",
                    u8::from(live_clock)
                )
            );
        } else {
            assert!(cmd_temporal(&ctx, &live, "").starts_with("OK "));
        }
        let status = cmd_temporal(&ctx, &live, "status");
        assert_eq!(key(&status, "witnessed=").as_deref(), Some("1"), "{status}");
        assert_eq!(
            key(&status, "diverged=").as_deref(),
            Some(if live_clock { "1" } else { "0" }),
            "{status}"
        );
        assert_eq!(key(&status, "pending=").as_deref(), Some("0"), "{status}");
        assert_eq!(
            key(&status, "unjudged_evicted=").as_deref(),
            Some("0"),
            "{status}"
        );
        hold(b"\r\ntwo", 1600);
        let status = cmd_temporal(&ctx, &live, "status");
        assert_eq!(
            key(&status, "keyframes=").as_deref(),
            Some(if live_clock { "2" } else { "1" }),
            "a mismatch takes a live keyframe: {status}"
        );
    }
}

/// `temporal judge` judges the pending witnesses of every kept chain, the closed ones and the newest alike, and `temporal status` agrees; with nothing left pending it judges none, and `trim` is no form of it.
#[test]
fn temporal_judge_verb_judges_every_kept_chain() {
    use crate::temporal::{InputSlice, TemporalMsg, keyframe_of};
    use aterm_core::terminal::ClockReading;
    let key = |status: &str, key: &str| {
        status
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix(key))
            .map(str::to_owned)
    };
    let ctx = temporal_ctx();
    let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 30));
    {
        let mut t = term_lock(&live);
        let mut rec = ctx.temporal.lock().unwrap();
        rec.record_keyframe(t.checkpoint());
        for chain in 0..3 {
            if chain > 0 {
                rec.record_keyframe(keyframe_of(&t, usize::MAX).expect("keyable"));
            }
            for n in 0..2 {
                let bytes = format!("chain {chain} line {n}\r\n");
                let clock = ClockReading::now();
                t.process_at(bytes.as_bytes(), clock);
                rec.apply(TemporalMsg::RawIn(InputSlice::at(bytes.as_bytes(), clock)));
                let seen = t.checkpoint_visible().expect("visible");
                rec.apply(TemporalMsg::Witness(Arc::new(seen)));
            }
        }
    }
    let status = cmd_temporal(&ctx, &live, "status");
    for (field, want) in [("keyframes=", "3"), ("witnessed=", "0"), ("pending=", "6")] {
        assert_eq!(
            key(&status, field).as_deref(),
            Some(want),
            "fixture: {status}"
        );
    }
    assert_eq!(
        cmd_temporal(&ctx, &live, "judge"),
        "OK judged=6 diverged=0 pending=0 unjudged_evicted=0\n"
    );
    let status = cmd_temporal(&ctx, &live, "status");
    for (field, want) in [
        ("witnessed=", "6"),
        ("diverged=", "0"),
        ("pending=", "0"),
        ("unjudged_evicted=", "0"),
    ] {
        assert_eq!(key(&status, field).as_deref(), Some(want), "{status}");
    }
    assert_eq!(
        cmd_temporal(&ctx, &live, "judge"),
        "OK judged=0 diverged=0 pending=0 unjudged_evicted=0\n",
        "nothing is left to judge"
    );
    assert!(
        cmd_temporal(&ctx, &live, "judge trim").starts_with("ERR usage: temporal"),
        "judge takes no trim"
    );
}

#[test]
fn temporal_status_discloses_dropped_input_and_an_open_break() {
    use crate::temporal::{SpineMark, SpinePre, TemporalTx};
    fn hold(live: &Mutex<Terminal>, mark: &mut SpineMark, tx: &TemporalTx, bytes: &[u8]) {
        static CLOCK: std::sync::OnceLock<aterm_core::terminal::ClockReading> =
            std::sync::OnceLock::new();
        let clock = *CLOCK.get_or_init(aterm_core::terminal::ClockReading::now);
        let mut t = term_lock(live);
        let pre = SpinePre::take(&t);
        t.process_at(bytes, clock);
        mark.record(tx, pre, &t, crate::temporal::InputSlice::at(bytes, clock));
    }
    let key = |status: &str, key: &str| {
        status
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix(key))
            .map(str::to_owned)
    };
    let ctx = temporal_ctx();
    let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 20));
    let (base, at) = {
        let t = term_lock(&live);
        (t.checkpoint(), SpinePre::take(&t))
    };
    let watermark = {
        let mut rec = ctx.temporal.lock().unwrap();
        rec.record_keyframe(base);
        rec.seed_spine(at);
        rec.spine_watermark()
    };
    let (tx, rx) = crate::temporal::queue(1);
    let drain = || {
        let mut rec = ctx.temporal.lock().unwrap();
        while let Some(msg) = rx.try_recv() {
            rec.apply(msg);
        }
    };
    let mut mark = SpineMark::attach(watermark);
    hold(&live, &mut mark, &tx, b"one\r\n");
    hold(&live, &mut mark, &tx, b"two\r\n");
    mark.reply(&tx);
    drain();
    let status = cmd_temporal(&ctx, &live, "status");
    assert_eq!(
        key(&status, "dropped_input=").as_deref(),
        Some("1"),
        "{status}"
    );
    assert_eq!(
        key(&status, "dropped_replies=").as_deref(),
        Some("1"),
        "{status}"
    );
    assert_eq!(
        key(&status, "reachable_from=").as_deref(),
        Some("-"),
        "{status}"
    );
    assert_eq!(cmd_temporal(&ctx, &live, ""), "ERR temporal unreachable\n");

    hold(&live, &mut mark, &tx, b"three\r\n");
    drain();
    let status = cmd_temporal(&ctx, &live, "status");
    assert_ne!(
        key(&status, "reachable_from=").as_deref(),
        Some("-"),
        "{status}"
    );
    let screen: String = {
        let t = term_lock(&live);
        (0..t.rows() as usize)
            .map(|r| format!("{}\n", crate::control::visible_row(&t, r)))
            .collect()
    };
    let reply = cmd_temporal(&ctx, &live, "");
    assert_eq!(
        reply.split_once('\n').map(|(_, body)| body),
        Some(&screen[..])
    );
}

#[test]
fn temporal_discloses_resizes_still_pending_on_the_spine() {
    use crate::temporal::{SpineMark, SpinePre};
    const ROWS: u16 = 6;
    const COLS: u16 = 20;
    let ctx = temporal_ctx();
    let live = Mutex::new(aterm_core::terminal::Terminal::new(ROWS, COLS));
    let (base, at) = {
        let t = term_lock(&live);
        (t.checkpoint(), SpinePre::take(&t))
    };
    let watermark = {
        let mut rec = ctx.temporal.lock().unwrap();
        rec.record_keyframe(base);
        rec.seed_spine(at);
        rec.spine_watermark()
    };
    let (tx, rx) = crate::temporal::queue(64);
    let mut mark = SpineMark::attach(watermark);
    let clock = aterm_core::terminal::ClockReading::now();
    let mut slice = |bytes: &[u8]| {
        {
            let mut t = term_lock(&live);
            let pre = SpinePre::take(&t);
            t.process_at(bytes, clock);
            mark.record(&tx, pre, &t, crate::temporal::InputSlice::at(bytes, clock));
        }
        let mut rec = ctx.temporal.lock().unwrap();
        while let Some(msg) = rx.try_recv() {
            rec.apply(msg);
        }
    };
    let mut frame = String::from("\x1b[?1049h\x1b[2J");
    for r in 0..ROWS {
        frame.push_str(&format!(
            "\x1b[{row};1HL{r}\x1b[{row};{COLS}HE",
            row = r + 1
        ));
    }
    frame.push_str("\x1b[4;3H");
    slice(frame.as_bytes());
    let screen = || -> String {
        let t = term_lock(&live);
        (0..t.rows() as usize)
            .map(|r| format!("{}\n", crate::control::visible_row(&t, r)))
            .collect()
    };
    let replayed = |reply: &str| -> (String, String) {
        let (head, body) = reply.split_once('\n').expect("header + body");
        (head.to_string(), body.to_string())
    };
    assert!(
        cmd_temporal(&ctx, &live, "status").contains(" pending_resizes=0 "),
        "nothing pending before the flap"
    );

    {
        let mut t = term_lock(&live);
        t.resize(ROWS, COLS - 1);
        t.resize(ROWS, COLS);
    }
    let status = cmd_temporal(&ctx, &live, "status");
    assert!(status.contains(" pending_resizes=2 "), "{status}");
    let (head, body) = replayed(&cmd_temporal(&ctx, &live, ""));
    assert!(head.ends_with(" pending_resizes=2"), "{head}");
    assert_ne!(
        body,
        screen(),
        "the spine lacks the flap until the next output"
    );

    slice(b"\x1b[1;1HT1\x1b[K\x1b[4;3H");
    let status = cmd_temporal(&ctx, &live, "status");
    assert!(status.contains(" pending_resizes=0 "), "{status}");
    let (head, body) = replayed(&cmd_temporal(&ctx, &live, ""));
    assert!(!head.contains("pending_resizes"), "{head}");
    assert_eq!(body, screen(), "both halves handed over: the live screen");
}

fn flapped_session() -> (Arc<Mutex<Terminal>>, Mutex<crate::cast::CastRecorder>) {
    flapped_session_split(true)
}

fn quiet_flapped_session() -> (Arc<Mutex<Terminal>>, Mutex<crate::cast::CastRecorder>) {
    flapped_session_split(false)
}

fn flapped_session_split(split: bool) -> (Arc<Mutex<Terminal>>, Mutex<crate::cast::CastRecorder>) {
    use std::time::Duration;
    let term = Arc::new(Mutex::new(Terminal::new(8, 20)));
    let cast = Mutex::new(crate::cast::CastRecorder::new(20, 8));
    let mut frame = String::from("\x1b[?1049h\x1b[?2026h\x1b[2J\x1b[H");
    for r in 0..8 {
        frame.push_str(&format!("\x1b[{};1HA{r}", r + 1));
    }
    frame.push_str("\x1b[6;3H\x1b[?2026l");
    use crate::cast::rig;
    rig::burst(&term, &cast, Duration::from_millis(100), frame.as_bytes());
    for (us, rows) in [(1_000_000u64, 7u16), (1_000_700, 8)] {
        if split && rows == 8 {
            rig::burst(
                &term,
                &cast,
                Duration::from_micros(1_000_300),
                b"\x1b[?1000h",
            );
        }
        rig::resize(&term, &cast, Duration::from_micros(us), rows, 20);
    }
    rig::burst(
        &term,
        &cast,
        Duration::from_millis(1_006),
        b"\x1b[?2026h\x1b[3;1H\x1b[KB2\x1b[6;3H\x1b[?2026l",
    );
    (term, cast)
}

#[test]
fn cmd_cast_drift_frames_lines_and_names_the_flap() {
    let (term, cast) = flapped_session();
    let reply = cmd_cast_drift(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        &cast,
        aterm_core::grid::ResizePolicy::Native,
        "",
    );
    let mut lines = reply.lines();
    let head = lines.next().expect("a header");
    let n: usize = head
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .expect("OK <n>");
    assert_eq!(lines.clone().count(), n, "{reply}");
    assert!(reply.ends_with('\n'));
    for key in [
        "verdict=desync",
        "fidelity=pass",
        "culprit=1.000000",
        "cut=quiescent",
        "dropped=0",
        "folds=2",
    ] {
        assert!(head.split_whitespace().any(|t| t == key), "{key} in {head}");
    }
    assert_eq!(
        lines.next(),
        Some(
            "run t=1.000000 n=2 gap_ms=0.700 geom=20x8>20x7>20x8 net=zero first_out_ms=5.300 \
             clear_ms=- culprit=1"
        )
    );
    assert_eq!(lines.next(), Some("row 0 live=A1 expected=A0"));
    assert_eq!(
        cmd_cast_drift(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            aterm_core::grid::ResizePolicy::Native,
            "frames"
        ),
        format!("{}\n", aterm_control::cast_drift::USAGE)
    );
}

#[test]
fn cmd_cast_drift_answers_clean_for_a_quiet_flap() {
    let (term, cast) = quiet_flapped_session();
    let reply = cmd_cast_drift(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        &cast,
        aterm_core::grid::ResizePolicy::Native,
        "",
    );
    let head = reply.lines().next().expect("a header");
    for key in ["verdict=clean", "fidelity=pass", "culprit=-"] {
        assert!(head.split_whitespace().any(|t| t == key), "{key} in {head}");
    }
    assert!(!reply.contains(" live="), "no drifted row: {reply}");
}

#[test]
fn cmd_cast_drift_answers_a_racy_cut_from_the_replays() {
    let (term, cast) = flapped_session();
    let moves = std::cell::Cell::new(0u64);
    let reply = cast_drift_with(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        &cast,
        aterm_core::grid::ResizePolicy::Native,
        "",
        CAST_DRIFT_CATCH_UP,
        &|| {
            moves.set(moves.get() + 1);
            let t = std::time::Duration::from_secs(2 + moves.get());
            crate::cast::rig::burst(&term, &cast, t, b"\x1b[1;1Hmoved");
        },
    );
    assert_eq!(
        moves.get(),
        CAST_DRIFT_CUT_ATTEMPTS as u64,
        "bounded retries"
    );
    let head = reply.lines().next().unwrap();
    assert!(
        head.contains(" verdict=desync fidelity=- against=replay "),
        "{head}"
    );
    assert!(head.contains(" culprit=1.000000 "), "{head}");
    assert!(head.contains(" cut=racy "), "{head}");
    assert!(
        !reply.contains(" live=moved"),
        "the moving screen is not read"
    );
}

#[test]
fn cast_drift_calls_no_cut_quiescent_on_a_frame_still_unfinished() {
    use std::time::Duration;
    let (term, cast) = quiet_flapped_session();
    let drift = || {
        let reply = cmd_cast_drift(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            aterm_core::grid::ResizePolicy::Native,
            "",
        );
        reply.lines().next().expect("a header").to_string()
    };
    crate::cast::rig::burst(
        &term,
        &cast,
        Duration::from_secs(2),
        b"\x1b[?2026h\x1b[1;1Hhalf",
    );
    let head = drift();
    assert!(head.contains(" cut=racy "), "{head}");
    crate::cast::rig::burst(&term, &cast, Duration::from_secs(3), b"\x1b[?2026l");
    let head = drift();
    assert!(head.contains(" cut=quiescent "), "{head}");
}

#[test]
fn cast_cut_is_consistent_without_sleeping() {
    use crate::cast::cut_probe;
    let (term, cast) = quiet_flapped_session();
    let waits = cut_probe::waits();
    let reply = cmd_cast_drift(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        &cast,
        aterm_core::grid::ResizePolicy::Native,
        "",
    );
    let head = reply.lines().next().expect("a header");
    assert!(head.contains(" cut=quiescent "), "{head}");
    assert_eq!(
        cut_probe::waits(),
        waits,
        "the cut waited on a recording that held it"
    );

    let _unrecorded = crate::cast::rig::hold(&term, &cast, b"\x1b[2;1Hpending");
    let reply = cast_drift_with(
        &term,
        &crate::frame_memo::FrameSlot::default(),
        &cast,
        aterm_core::grid::ResizePolicy::Native,
        "",
        std::time::Duration::ZERO,
        &|| {},
    );
    let head = reply.lines().next().expect("a header");
    assert!(head.contains(" cut=racy "), "{head}");
    assert_eq!(cut_probe::waits(), waits + CAST_DRIFT_CUT_ATTEMPTS);
}

#[test]
fn cast_cut_waits_for_the_resize_and_burst_the_screen_shows() {
    use std::time::Duration;

    use crate::cast::rig;
    let policy = aterm_core::grid::ResizePolicy::Native;
    let pending = |term: &Mutex<Terminal>, cast: &Mutex<crate::cast::CastRecorder>| {
        let stamp = rig::resize_hold(term, cast, Duration::from_secs(2), 7, 20);
        let diff = b"\x1b[4;1HC3";
        (stamp, rig::hold(term, cast, diff), diff)
    };
    for which in ["resize", "burst"] {
        let (term, cast) = flapped_session();
        let (stamp, cuts, diff) = pending(&term, &cast);
        match which {
            "resize" => cast.lock().unwrap().record_burst(
                Arc::from(&diff[..]),
                cuts,
                Duration::from_secs(3),
            ),
            _ => cast.lock().unwrap().record_resize_stamped(stamp, 20, 7),
        }
        let reply = cast_drift_with(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            policy,
            "",
            Duration::ZERO,
            &|| {},
        );
        let head = reply.lines().next().expect("a header");
        assert!(
            head.contains(" cut=racy "),
            "the {which} is not recorded: {head}"
        );
    }

    let (term, cast) = flapped_session();
    let (stamp, cuts, diff) = pending(&term, &cast);
    let reply = std::thread::scope(|scope| {
        scope.spawn(|| {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the cut to wait",
                || (cast.lock().unwrap().waiters() > 0).then_some(()),
            );
            let mut rec = cast.lock().unwrap();
            rec.record_resize_stamped(stamp, 20, 7);
            rec.record_burst(Arc::from(&diff[..]), cuts, Duration::from_secs(3));
        });
        cast_drift_with(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            policy,
            "",
            aterm_spec::wait::MUST_HAPPEN,
            &|| {},
        )
    });
    let head = reply.lines().next().expect("a header");
    assert!(head.contains(" cut=quiescent "), "{head}");
    assert!(head.contains(" fidelity=pass "), "{head}");
}

#[test]
fn cast_drift_slots_cap_the_concurrent_folds() {
    static SLOTS: DriftSlots = DriftSlots {
        busy: Mutex::new(0),
        freed: std::sync::Condvar::new(),
    };
    let short = std::time::Duration::from_millis(20);
    let held: Vec<DriftSlot> = (0..CAST_DRIFT_CONCURRENCY)
        .map(|_| SLOTS.acquire(short).expect("a free slot"))
        .collect();
    assert!(SLOTS.acquire(short).is_none(), "every slot is taken");
    drop(held);
    assert!(SLOTS.acquire(short).is_some(), "released slots are free");
}
