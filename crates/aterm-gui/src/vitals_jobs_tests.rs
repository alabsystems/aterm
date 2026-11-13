// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Start facts [`probe::judge_start`] judges `at`, read the held age after the fork.
fn judged(at: AtStart) -> StartFacts {
    let unstarted = StartFacts {
        lone_zero: true,
        user_ns: Some(0),
        program_check_wait: true,
        local: Some(true),
        age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
        ..StartFacts::default()
    };
    let facts = match at {
        AtStart::Started => StartFacts::default(),
        AtStart::Held => unstarted,
        AtStart::Translating => StartFacts {
            translated: true,
            ..unstarted
        },
        AtStart::Unstarted => StartFacts {
            local: Some(false),
            ..unstarted
        },
        AtStart::StoppedUnstarted { stopped, traced } => StartFacts {
            stopped,
            traced,
            ..unstarted
        },
    };
    assert_eq!(probe::judge_start(&facts), at);
    facts
}

/// The engine's cadence, and with no window the suspect rule's: a live agent call that stands
/// suspect asks for the engine's demand; an idle engine, an exited, running or listening call,
/// and a job that is no agent's ask nothing.
#[test]
fn the_engine_is_asked_again_at_its_own_cadence_never_faster_than_two_seconds() {
    assert_eq!(next_pass_ms(Demand::Fast), 2_000);
    assert_eq!(next_pass_ms(Demand::Slow), 30_000);
    assert_eq!(next_pass_ms(Demand::Idle), 2_000);

    let view = |rows: Vec<JobRow>, demand| JobsView {
        rows,
        demand,
        ..JobsView::default()
    };
    let call = || JobRow {
        standing: Some(Standing::Capped),
        ..call_row(4, Consumer::AgentBg, true, 31, 27098, "python3")
    };
    let mut running = call();
    running.standing = None;
    let mut serving = call();
    serving.standing = Some(Standing::Legit);
    let mut shell = call();
    shell.kind = JobKind::Shell;
    let mut ended = call();
    ended.class = Class::Exited;
    let secs = |s| Some(Duration::from_secs(s));
    assert_eq!(windowless_after(&view(vec![call()], Demand::Fast)), secs(2));
    assert_eq!(
        windowless_after(&view(vec![call()], Demand::Slow)),
        secs(30)
    );
    assert_eq!(windowless_after(&view(vec![call()], Demand::Idle)), None);
    assert_eq!(windowless_after(&view(vec![shell], Demand::Fast)), None);
    assert_eq!(windowless_after(&view(vec![ended], Demand::Fast)), None);
    assert_eq!(windowless_after(&view(Vec::new(), Demand::Idle)), None);
    assert_eq!(windowless_after(&view(vec![running], Demand::Fast)), None);
    assert_eq!(windowless_after(&view(vec![serving], Demand::Slow)), None);
    let beat = |ms| Some(Duration::from_millis(ms));
    assert_eq!(told_after(&view(vec![call()], Demand::Fast)), beat(2_250));
    let mut moving = call();
    moving.no_progress = false;
    assert_eq!(told_after(&view(vec![moving], Demand::Fast)), None);
}

/// What a call at no progress waits on is said from the engine's evidence (rule 5): a Tier B
/// naming's word over the engine's own wait name, and the process holding the wait (a lock's
/// holder, else the wait's peer); never the program's name. CONTROL: a wait that names no peer
/// says no holder.
#[test]
fn the_facts_call_names_its_wait_and_its_holder_never_the_program() {
    use aterm_vitals::jobs::{Holder, NamedRow, Standing, TierB, Wait, WaitName};
    let mut piped = call_row(4, Consumer::AgentBg, true, 31, 27090, "ignore.previous");
    let mut wait = Wait::bare(WaitName::Pipe, Standing::Capable, 27098, "reader");
    wait.peer = Some(4100);
    piped.wait = Some(wait);
    let call = background_call(&piped);
    assert_eq!(
        (call.wait.as_deref(), call.held_by, call.pid),
        (Some("pipe"), Some(4100), 27098)
    );
    assert!(!format!("{call:?}").contains("ignore.previous"), "{call:?}");
    let mut locked = piped.clone();
    locked.named = Some(NamedRow {
        word: TierB::Lock,
        holder: Some(Holder {
            pid: 5200,
            exe: "holder".into(),
            tab: None,
            class: None,
        }),
    });
    let call = background_call(&locked);
    assert_eq!(
        (call.wait.as_deref(), call.held_by),
        (Some("lock"), Some(5200))
    );
    let mut bare = piped;
    bare.wait = Some(Wait::bare(WaitName::Kernel, Standing::Capable, 27098, "x"));
    let call = background_call(&bare);
    assert_eq!((call.wait.as_deref(), call.held_by), (Some("kernel"), None));
}

/// An agent's call in tab `tab` read `no_progress` (or not), by `consumer`,
/// quiet `min` minutes, naming process `pid` (`exe`).
pub(crate) fn call_row(
    tab: u64,
    consumer: Consumer,
    no_progress: bool,
    min: u64,
    pid: u32,
    exe: &str,
) -> JobRow {
    use aterm_vitals::jobs::Verdict;
    JobRow {
        tab,
        kind: JobKind::AgentCall,
        orphan_of: None,
        consumer,
        no_progress,
        leader: (pid, None),
        pgid: pid,
        exe: exe.into(),
        ..crate::stats_paint::tests::job_row(
            &format!("a{pid}"),
            Verdict::Quiet,
            Some(9_000),
            Some(min * 60_000),
        )
    }
}

/// The canary's evidence is this session's background call quiet longest, named by its waiting
/// process; never a foreground call, a person's job, an exited call or another session's.
#[test]
fn the_board_names_this_sessions_background_call_quiet_longest() {
    use aterm_vitals::jobs::{NamedRow, Standing, TierB, Wait, WaitName};
    let board = JobsBoard::new();
    let t0 = Instant::now();
    assert_eq!(board.evidence("s-hung", t0), BackgroundEvidence::Unknown);
    let mut waiting = call_row(4, Consumer::AgentBg, true, 31, 27090, "sh");
    waiting.wait = Some(Wait::bare(
        WaitName::Unnamed,
        Standing::Capped,
        27098,
        "aterm_gui-e0964a",
    ));
    waiting.exe = "aterm_gui-e0964a".into();
    waiting.task = Some("bplle5dxj".into());
    waiting.named = Some(NamedRow {
        word: TierB::Timer,
        holder: None,
    });
    let mut person = call_row(4, Consumer::PersonFg, true, 600, 700, "sleep");
    person.kind = JobKind::Shell;
    let mut ended = call_row(4, Consumer::AgentBg, false, 120, 503, "cargo");
    ended.class = Class::Exited;
    let rows = vec![
        call_row(4, Consumer::Agent, true, 20, 501, "Python"),
        waiting.clone(),
        call_row(4, Consumer::AgentFg, true, 99, 502, "node"),
        ended,
        person.clone(),
        call_row(9, Consumer::AgentBg, true, 500, 504, "make"),
        call_row(9, Consumer::AgentBg, false, 5, 505, "cargo"),
    ];
    let view = || {
        Arc::new(JobsView {
            rows: rows.clone(),
            ..JobsView::default()
        })
    };
    let sids = BTreeMap::from([(4, "s-hung".to_string()), (9, "s-other".to_string())]);
    let first = view();
    board.post(Arc::clone(&first), sids.clone(), t0);
    let hung = BackgroundEvidence::NoProgress {
        worst: BackgroundCall {
            pid: 27098,
            pgid: 27090,
            birth: None,
            episode: 0,
            task: Some("bplle5dxj".into()),
            quiet_ms: 31 * 60_000,
            wait: Some("timer".into()),
            held_by: None,
        },
    };
    assert_eq!(board.evidence("s-hung", t0), hung);
    assert_eq!(board.evidence("s-calm", t0), BackgroundEvidence::Unknown);
    let late = t0 + JOBS_FRESH + Duration::from_secs(1);
    assert_eq!(board.evidence("s-hung", late), BackgroundEvidence::Unknown);
    board.post(Arc::clone(&first), sids.clone(), late);
    assert_eq!(
        board.evidence("s-hung", late),
        BackgroundEvidence::Unknown,
        "the same view posted again keeps its age"
    );
    board.post(view(), sids.clone(), late);
    assert_eq!(board.evidence("s-hung", late), hung, "a new view is fresh");
    let mut building = rows.clone();
    building.push(call_row(4, Consumer::AgentBg, false, 2, 506, "cargo"));
    let view = Arc::new(JobsView {
        rows: building,
        ..JobsView::default()
    });
    board.post(view, sids, late);
    assert_eq!(
        board.evidence("s-hung", late),
        BackgroundEvidence::Waiting,
        "a live call that is not at no progress wakes the agent"
    );
}

/// Each tab keeps its key, number and shell, takes the terminal word
/// and the reader's feed handed for its key, and reads adopted only when
/// its key is among the handed-over sessions.
#[test]
fn a_tab_keeps_its_session_key_shell_and_number_and_its_own_terminal_word_and_feed() {
    let root = |key, label: &str| TabRoot {
        key,
        tab: key,
        shell_pid: 900 + u32::try_from(key).expect("small"),
        label: label.into(),
        program: String::new(),
        tab_id: None,
        agent_leader: None,
    };
    let raw = TtyMode {
        canonical: false,
        echo: false,
        stamped_ms: 7,
    };
    let fed = TabFeed {
        output_ms: Some(9),
        line_output_ms: Some(8),
        drawn_ms: Some(7),
        alt_screen: true,
        output_bytes: 120,
        block: Some(Block {
            id: 3,
            started_ms: 5,
            open: true,
            command: "make".into(),
            status: None,
            stopped: false,
        }),
        ended: Some(Block {
            id: 2,
            started_ms: 1,
            open: false,
            command: "ls".into(),
            status: Some(146),
            stopped: true,
        }),
        shell_ms: Some(4),
        typed_ms: Some(6),
    };
    let tabs = job_tabs(
        &[root(41, "Tab 3"), root(42, "Tab 4")],
        &[(41, raw)],
        &[(42, fed.clone())],
        &[42],
    );
    assert_eq!(
        (tabs[0].key, tabs[0].number, tabs[0].shell_pid, tabs[0].tty),
        (41, 3, 941, Some(raw))
    );
    assert_eq!(
        (tabs[0].adopted, tabs[1].adopted),
        (false, true),
        "only the handed-over session reads adopted"
    );
    assert_eq!(
        (tabs[0].output_ms, tabs[0].output_bytes, &tabs[0].block),
        (None, 0, &None)
    );
    assert_eq!((tabs[1].key, tabs[1].tty), (42, None));
    assert_eq!(
        TabFeed {
            output_ms: tabs[1].output_ms,
            line_output_ms: tabs[1].line_output_ms,
            drawn_ms: tabs[1].drawn_ms,
            alt_screen: tabs[1].alt_screen,
            output_bytes: tabs[1].output_bytes,
            block: tabs[1].block.clone(),
            ended: tabs[1].ended.clone(),
            shell_ms: tabs[1].shell_ms,
            typed_ms: tabs[1].typed_ms,
        },
        fed
    );
}

/// Every read counts its bytes; only a line feed with text of its read stamps a line (not a
/// `\r` redraw or a typed Enter's echo, but a record led by its line feed), only a feed that
/// moved the screen stamps output, and only one that puts the cursor on a row of its own stamps
/// a drawing (not a spinner's erase). The alternate screen reads as the last feed left it.
#[test]
fn a_line_feed_stamps_a_line_and_only_a_moving_feed_stamps_output() {
    const RESET: &[u8] = b"\x1b[39m\x1b[0m";
    let epoch = Instant::now();
    let feed = JobFeed::default();
    let term = aterm_core::terminal::Terminal::new(24, 80);
    assert_eq!(feed.at(epoch), TabFeed::default(), "nothing read");
    for (bytes, moved, ms) in [
        (&b"64 bytes from 127.0.0.1\r\n"[..], true, 1_000),
        (b"\r 50%", true, 2_500),
        (RESET, false, 4_000),
        (b"\r\n", false, 5_000),
    ] {
        feed.read(bytes);
        feed.fed(&term, bytes, moved, || epoch + Duration::from_millis(ms));
    }
    let read = feed.at(epoch);
    assert_eq!(
        (read.output_ms, read.line_output_ms, read.output_bytes),
        (Some(2_500), Some(1_000), 41)
    );
    let long = [b'x'; 16];
    feed.read(&[&long[..], b"\r\n"].concat());
    feed.fed(&term, &long, true, || epoch + Duration::from_millis(6_000));
    feed.fed(&term, b"\r\n", true, || {
        epoch + Duration::from_millis(6_001)
    });
    assert_eq!(
        feed.at(epoch).line_output_ms,
        Some(6_001),
        "a line whose line feed the parse cut from its text"
    );
    let record = b"\r\nThu Oct 23 12:00:05";
    feed.read(record);
    feed.fed(&term, record, true, || epoch + Duration::from_millis(7_000));
    assert_eq!(
        feed.at(epoch).line_output_ms,
        Some(7_000),
        "a writer that leads with its line feed"
    );
    feed.fed(&term, b"", true, || epoch);
    assert_eq!(
        feed.at(epoch).output_ms,
        Some(1),
        "a stamp is at least 1 ms"
    );
    let spinner = "\r\x1b[2K\x1b[?25l\u{280b} fetching".as_bytes();
    feed.fed(&term, spinner, true, || {
        epoch + Duration::from_millis(8_000)
    });
    assert_eq!(feed.at(epoch).drawn_ms, None, "a spinner draws nothing");
    let redraw = b"\x1b[12;1Htests/test_api.py::test_x PASSED\x1b[K\r\n";
    feed.fed(&term, redraw, true, || epoch + Duration::from_millis(9_000));
    assert_eq!(
        feed.at(epoch).drawn_ms,
        Some(9_000),
        "nvim's row of its terminal"
    );
    let mut full = aterm_core::terminal::Terminal::new(24, 80);
    for (bytes, alt) in [(&b"\x1b[?1049h"[..], true), (b"\x1b[?1049l", false)] {
        full.process(bytes);
        feed.fed(&full, bytes, false, || {
            epoch + Duration::from_millis(10_000)
        });
        assert_eq!(feed.at(epoch).alt_screen, alt, "{bytes:?}");
    }
}

/// A writer that never stops (`kubectl logs -f`) has a line fed past each pass's clock, which a
/// newer one overwrites before the next pass; that pass still reads it.
#[test]
fn a_line_fed_past_a_passs_clock_is_the_next_passs() {
    const LINE: &[u8] = b"GET /healthz 200\r\n";
    let epoch = Instant::now();
    let term = aterm_core::terminal::Terminal::new(24, 80);
    let at = |us: u64| epoch + Duration::from_micros(us);
    for offset in [100, 900] {
        let feed = JobFeed::default();
        let line = |us| {
            feed.read(LINE);
            feed.fed(&term, LINE, true, || at(us));
        };
        let pass = |ms| {
            let f = feed.at_pass(epoch, Some(ms));
            (f.line_output_ms, f.output_ms)
        };
        // The newest line and output round past the pass's millisecond.
        line(1_000_000 + offset);
        assert_eq!(pass(1_000), (None, None), "offset {offset}");
        line(2_000_000 + offset);
        assert_eq!(pass(2_000), (Some(1_001), Some(1_001)), "offset {offset}");
        assert_eq!(pass(4_000), (Some(2_001), Some(2_001)), "offset {offset}");
        line(5_000_000 + offset);
        assert_eq!(pass(5_000), (None, None), "consumed stamp, offset {offset}");
    }
    for before in [0, 1] {
        let feed = JobFeed::default();
        let line = |us| {
            feed.read(LINE);
            feed.fed(&term, LINE, true, || at(us));
        };
        line(1_000_000 - before);
        let f = feed.at_pass(epoch, Some(1_000));
        assert_eq!((f.line_output_ms, f.output_ms), (Some(1_000), Some(1_000)));
        line(2_000_100);
        let f = feed.at_pass(epoch, Some(2_000));
        assert_eq!((f.line_output_ms, f.output_ms), (None, None));
    }
}

/// A PASS THAT FINDS KEYS DRIVEN SINCE THE LAST STAMPS ITS CLOCK: the first
/// pass only notes the sink's count, and the stamp stays until keys move it.
#[cfg(unix)]
#[test]
fn a_pass_that_finds_keys_driven_since_the_last_stamps_its_clock() {
    use std::os::fd::AsRawFd as _;
    let (r, w) = aterm_uds::pipe().expect("pipe");
    let sink = aterm_session::sink::SinkWriter::new(w.as_raw_fd());
    let feed = JobFeed::default();
    let first = feed.typed_at(1_000, sink.driven_input());
    let still = feed.typed_at(3_000, sink.driven_input());
    assert_eq!(sink.write_frame(b"x").expect("a key"), 1);
    let typed = feed.typed_at(5_000, sink.driven_input());
    let kept = feed.typed_at(7_000, sink.driven_input());
    drop((r, w));
    assert_eq!(
        (first, still, typed, kept),
        (None, None, Some(5_000), Some(5_000))
    );
}

/// The feed holds the newest block that ran: open from OSC 133 C with its command, closed by D
/// with its status (D and the next A in one feed included), held through the next prompt, and
/// kept as the ended one once a newer block runs. The shell's marks are stamped, and a line
/// parsed with them shares their stamp; a command's output and a reset are not marks, and a
/// reset leaves the block open until the next prompt. A block Ctrl-Z ended is stopped.
#[test]
fn the_feed_holds_the_newest_block_that_ran_and_its_status() {
    let epoch = Instant::now();
    let feed = JobFeed::default();
    let put = |term: &mut aterm_core::terminal::Terminal, bytes: &[u8], moved, ms| {
        term.process(bytes);
        feed.fed(term, bytes, moved, || epoch + Duration::from_millis(ms));
    };
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    put(&mut term, b"$ ls\n", false, 100);
    assert_eq!(feed.at(epoch).block, None, "no shell integration");
    put(
        &mut term,
        b"\x1b]133;A\x07$ \x1b]133;B\x07while true; do date; sleep 5; done\n",
        false,
        200,
    );
    let typed = feed.at(epoch).block.expect("the prompt's block");
    assert!(!typed.open, "typed, not run: {typed:?}");
    put(&mut term, b"\x1b]133;C\x07", false, 300);
    let running = feed.at(epoch).block.expect("the running block");
    assert_eq!(
        (
            running.id,
            running.open,
            running.started_ms,
            running.command.as_str(),
            running.status
        ),
        (
            typed.id,
            true,
            300,
            "while true; do date; sleep 5; done",
            None
        )
    );
    put(&mut term, b"Thu Oct 23\n", false, 5_300);
    assert_eq!(
        feed.at(epoch).block.as_ref(),
        Some(&running),
        "output keeps it"
    );
    put(&mut term, b"^C\x1b]133;D;130\x07", false, 9_000);
    let ended = feed.at(epoch).block.expect("the ended block");
    assert_eq!(
        (
            ended.id,
            ended.open,
            ended.status,
            ended.started_ms,
            &ended.command
        ),
        (running.id, false, Some(130), 300, &running.command)
    );
    put(&mut term, b"\x1b]133;A\x07$ ", false, 9_100);
    assert_eq!(
        feed.at(epoch).block,
        Some(ended.clone()),
        "the next prompt holds it"
    );
    put(
        &mut term,
        b"\x1b]133;B\x07make\n\x1b]133;C\x07",
        false,
        9_200,
    );
    let make = feed.at(epoch).block.expect("the next running block");
    assert!(make.id != running.id && make.open, "{make:?}");
    assert_eq!(feed.at(epoch).ended, Some(ended), "the block it replaced");
    put(&mut term, b"\x1b]133;D;2\x07\x1b]133;A\x07$ ", false, 9_900);
    let failed = feed.at(epoch).block.expect("the failed block");
    assert_eq!(
        (failed.id, failed.open, failed.status, failed.started_ms),
        (make.id, false, Some(2), 9_200),
        "D and A in one feed"
    );
    put(
        &mut term,
        b"\x1b]133;B\x07top\n\x1b]133;C\x07",
        false,
        10_000,
    );
    let top = feed.at(epoch).block.expect("top runs");
    put(
        &mut term,
        b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07less\n\x1b]133;C\x07",
        false,
        11_000,
    );
    let less = feed.at(epoch).block.expect("less runs");
    let now = feed.at(epoch);
    assert_eq!(
        now.block.map(|b| (b.open, b.command, b.started_ms)),
        Some((true, "less".to_owned(), 11_000))
    );
    assert_eq!(
        now.ended
            .map(|b| (b.id, b.open, b.status, b.command, b.started_ms)),
        Some((top.id, false, Some(0), top.command, 10_000)),
        "a block that ended and the next that ran in one feed"
    );
    assert_eq!(feed.at(epoch).output_ms, None, "no feed moved the screen");
    assert_eq!(
        (feed.at(epoch).shell_ms, feed.at(epoch).line_output_ms),
        (Some(11_000), Some(11_000)),
        "the shell's marks, and a line parsed with them shares their stamp"
    );
    put(&mut term, b"README\n", true, 12_000);
    assert_eq!(
        feed.at(epoch).shell_ms,
        Some(11_000),
        "a command's output is none of the shell's marks"
    );
    put(&mut term, b"\x1bc", true, 13_000);
    assert_eq!(
        (
            feed.at(epoch).block.map(|b| (b.id, b.open)),
            feed.at(epoch).shell_ms
        ),
        (Some((less.id, true)), Some(11_000)),
        "a reset leaves the command running, and is none of the shell's marks"
    );
    put(&mut term, b"\x1b]133;A\x07$ ", false, 14_000);
    assert_eq!(
        feed.at(epoch).block.map(|b| (b.id, b.open, b.status)),
        Some((less.id, false, None)),
        "the next prompt closes it, its D lost to the reset"
    );
    assert!(!failed.stopped, "an exit is no stop");
    #[cfg(unix)]
    {
        put(
            &mut term,
            b"\x1b]133;B\x07vim\n\x1b]133;C\x07",
            false,
            15_000,
        );
        let stop = format!("\x1b]133;D;{}\x07\x1b]133;A\x07$ ", 128 + libc::SIGTSTP);
        put(&mut term, stop.as_bytes(), false, 16_000);
        let vim = feed.at(epoch).block.expect("vim stopped");
        assert!(vim.stopped && !vim.open, "Ctrl-Z's status: {vim:?}");
    }
}

/// The pass on this host's real process table, this test process standing in for a tab's
/// shell: inside the cadence it keeps its last view, an edge's reading runs the engine anyway,
/// and a reading without a sweep keeps the last view.
#[test]
fn an_edges_reading_inside_the_cadence_runs_the_engine() {
    let Some(sweep) = aterm_sysprobe::processes() else {
        // A host without a process sweep publishes no jobs.
        return;
    };
    let me = std::process::id();
    let tabs = [TabRoot {
        key: 1,
        tab: 1,
        shell_pid: me,
        label: "Tab 1".into(),
        program: String::new(),
        tab_id: None,
        agent_leader: None,
    }];
    let mut pass = JobsPass::new();
    let first = pass
        .tick(
            10_000,
            || 10_000,
            Some(&sweep.procs),
            &tabs,
            &[],
            &[],
            &GrantFacts::default(),
            me,
            false,
            false,
        )
        .expect("a view");
    let paced = pass
        .tick(
            11_900,
            || 11_900,
            Some(&sweep.procs),
            &tabs,
            &[],
            &[],
            &GrantFacts::default(),
            me,
            false,
            false,
        )
        .expect("the last view");
    assert!(Arc::ptr_eq(&first, &paced), "paced: the engine waits");
    let edge = pass
        .tick(
            11_900,
            || 11_900,
            Some(&sweep.procs),
            &tabs,
            &[],
            &[],
            &GrantFacts::default(),
            me,
            false,
            true,
        )
        .expect("a new view");
    assert!(!Arc::ptr_eq(&first, &edge), "an edge's reading looks");
    assert!(
        pass.tick(
            11_950,
            || 11_950,
            None,
            &tabs,
            &[],
            &[],
            &GrantFacts::default(),
            me,
            false,
            true
        )
        .is_some(),
        "a reading without a sweep keeps the last view"
    );
}

/// AN ENDED CALL'S ROW IS ON THE PASS'S CLOCK (TM-1 §5), through `tick`'s own wiring: the
/// clock it hands the call watch's read and the jobs pass's row. A real call (a session leader
/// with no terminal under the tab's shell, this test process) is first tracked by a pass 300 ms
/// after it started, exits, and is found gone by a reading that read its clock 300 ms before
/// its jobs pass, as a window's reading reads its sweep, machine sample and the rest between.
/// Its row is written at the exit's own wall time, never past it by that span, and its
/// `wall_ms` is the leader's life from its start to its exit, never from the first pass's late
/// look.
#[cfg(target_vendor = "apple")]
#[test]
fn an_ended_calls_row_runs_from_its_leaders_start_to_its_exit_on_the_passs_clock() {
    use crate::telemetry_host::{CallCounts, CallRows, CallWatch};
    static COUNTS: CallCounts = CallCounts::new();
    static ROWS: Mutex<Vec<aterm_telemetry::Row>> = Mutex::new(Vec::new());
    // The two clocks' ms rounding, which a mapping between them may add up.
    const ROUNDING_MS: u64 = 5;
    let me = std::process::id();
    let tabs = [TabRoot {
        key: 1,
        tab: 1,
        shell_pid: me,
        label: "Tab 1".into(),
        program: String::new(),
        tab_id: None,
        agent_leader: None,
    }];
    // The pass's clock, its zero 100 s before the test's: a clock read as 0 is far off.
    let epoch = Instant::now();
    let clock = move || 100_000 + u64::try_from(epoch.elapsed().as_millis()).expect("ms");
    let mut pass = JobsPass::new();
    pass.calls = CallWatch::writing(
        true,
        &COUNTS,
        CallRows {
            host: || Some(("m14", aterm_telemetry::Digest::of("test", &[b"facts"]))),
            emit: |row| {
                ROWS.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(row)
            },
        },
    );
    let pass_at = |pass: &mut JobsPass, at_ms: u64| {
        let sweep = aterm_sysprobe::processes().expect("a process sweep");
        pass.tick(
            at_ms,
            clock,
            Some(&sweep.procs),
            &tabs,
            &[],
            &[],
            &GrantFacts::default(),
            me,
            false,
            true,
        )
        .expect("a view")
    };
    let spawned_before = Instant::now();
    let mut leader = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _; exit 117"])
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn");
    let spawned_after = Instant::now();
    let pid = leader.id();
    std::thread::sleep(Duration::from_millis(300));
    let view = pass_at(&mut pass, clock());
    assert!(
        view.rows
            .iter()
            .any(|r| r.kind == JobKind::AgentCall && r.leader.0 == pid && r.leader.1.is_some()),
        "the pass tracks the call: {:?}",
        view.rows
    );
    assert_eq!(
        pass.calls.stamped(pid),
        Some(false),
        "its leader is watched"
    );
    let released = (Instant::now(), aterm_telemetry::now_ms());
    drop(leader.stdin.take());
    let _ = leader.wait();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the leader's exit stamped",
        || (pass.calls.stamped(pid) == Some(true)).then_some(()),
    );
    let stamped = (Instant::now(), aterm_telemetry::now_ms());
    let at_ms = clock();
    std::thread::sleep(Duration::from_millis(300));
    let _ = pass_at(&mut pass, at_ms);
    assert_eq!(pass.calls.stamped(pid), None, "its watch is let go");
    let rows = ROWS.lock().unwrap_or_else(PoisonError::into_inner);
    let row = rows
        .iter()
        .find(|r| r.code == Some(117))
        .unwrap_or_else(|| panic!("the call's row: {rows:?}"));
    assert_eq!(row.kind, aterm_telemetry::Kind::AgentCall);
    assert!(
        released.1.saturating_sub(ROUNDING_MS) <= row.at_ms && row.at_ms <= stamped.1 + ROUNDING_MS,
        "at the exit's wall time: released {}, row {}, stamped {}",
        released.1,
        row.at_ms,
        stamped.1
    );
    let ms = |d: Duration| u64::try_from(d.as_millis()).expect("ms");
    let lived = (
        ms(released.0 - spawned_after).saturating_sub(ROUNDING_MS),
        ms(stamped.0 - spawned_before) + ROUNDING_MS,
    );
    let aterm_telemetry::Fig::Known(wall_ms) = row.wall_ms else {
        panic!("a known wall time: {:?}", row.wall_ms);
    };
    assert!(
        (lived.0..=lived.1).contains(&wall_ms),
        "the leader's life, {lived:?}: {wall_ms}"
    );
}

/// Members count by their own reading; of the rest only this user's live rows with at most
/// 20 ms of CPU are asked. An actual version is proved around a fresh start read for each
/// named row; the stand-in reads held unasked whatever its CPU. A stopped or traced row that
/// never ran is handed on with what it read, a member (24) or outside every job (25).
#[test]
fn the_jobs_pass_counts_this_users_held_programs_members_or_not() {
    use std::cell::RefCell;
    const ME: u32 = 501;
    let row = |pid, uid, cpu_ms: u64, state| ProcessReading {
        pid,
        uid: Some(uid),
        cpu_ns: Some(cpu_ms * 1_000_000),
        name: format!("p{pid}"),
        state,
        ..ProcessReading::default()
    };
    let sweep = [
        row(10, ME, 400, probe::RunState::Sleeping),
        row(20, ME, 2, probe::RunState::Sleeping),
        row(21, ME, 2, probe::RunState::Sleeping),
        row(22, ME, 2, probe::RunState::Sleeping),
        row(30, ME, 30, probe::RunState::Sleeping),
        row(40, 0, 2, probe::RunState::Sleeping),
        row(50, ME, 0, probe::RunState::Zombie),
    ];
    let unstarted = |translated| probe::StartFacts {
        lone_zero: true,
        user_ns: Some(0),
        program_check_wait: true,
        translated,
        local: Some(true),
        age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
        ..probe::StartFacts::default()
    };
    let member = |pid, name: &str, exe: Option<&str>, facts| ProcFacts {
        pid,
        uid: Some(ME),
        name: name.into(),
        exe: exe.map(str::to_owned),
        start_facts: Some(facts),
        ..ProcFacts::default()
    };
    let members = [
        member(10, "zsh", Some("gh"), unstarted(false)),
        member(22, "sleep", None, probe::StartFacts::default()),
        member(23, "x86tool", None, unstarted(true)),
        member(
            24,
            "debuggee",
            None,
            probe::StartFacts {
                traced: true,
                ..unstarted(false)
            },
        ),
    ];
    let traced = AtStart::StoppedUnstarted {
        stopped: false,
        traced: true,
    };
    let spawned = AtStart::StoppedUnstarted {
        stopped: true,
        traced: false,
    };
    let sweep = [
        &sweep[..],
        &[
            row(23, ME, 2, probe::RunState::Sleeping),
            row(25, ME, 2, probe::RunState::Stopped),
        ][..],
    ]
    .concat();
    let asked = RefCell::new(Vec::new());
    let versions = RefCell::new(Vec::new());
    let (held, young) = held_list(
        &sweep,
        &members,
        Some(ME),
        Some(30),
        0,
        |pid| {
            asked.borrow_mut().push(pid);
            Some(judged(if pid == 21 {
                AtStart::Unstarted
            } else if pid == 23 {
                AtStart::Translating
            } else if pid == 24 {
                traced
            } else if pid == 25 {
                spawned
            } else {
                AtStart::Held
            }))
        },
        |pid| {
            versions.borrow_mut().push(pid);
            Some(u64::from(pid) * 100 + 7)
        },
        |pid| pid == 20,
        |pid| match pid {
            20 => Some("codex-command-runner".to_owned()),
            25 => Some("start-suspended-helper".to_owned()),
            _ => None,
        },
    );
    assert_eq!(
        asked.into_inner(),
        [10, 23, 24, 20, 20, 21, 21, 25, 25],
        "held members are re-probed to bind their version; nonmembers first selected by \
         CPU get the same proof; never another user's, a zombie or the stand-in"
    );
    assert_eq!(
        versions.into_inner(),
        [10, 10, 23, 23, 24, 24, 20, 20, 21, 21, 30, 30, 25, 25],
        "the actual version brackets each fresh wait proof; never member 22, which started"
    );
    let got: Vec<(u32, u64, &str, bool, StartWait)> = held
        .iter()
        .map(|h| &h.row)
        .map(|h| (h.pid, h.exec, h.name.as_str(), h.aterm, h.wait))
        .collect();
    assert_eq!(
        got,
        [
            (10, 1007, "gh", true, StartWait::ProgramCheck),
            (23, 2307, "x86tool", true, StartWait::Translating),
            (
                24,
                2407,
                "debuggee",
                true,
                StartWait::StoppedUnstarted {
                    stopped: false,
                    traced: true
                }
            ),
            (
                20,
                2007,
                "codex-command-runner",
                true,
                StartWait::ProgramCheck
            ),
            (21, 2107, "p21", false, StartWait::NotStarted),
            (30, 3007, "p30", false, StartWait::ProgramCheck),
            (
                25,
                2507,
                "start-suspended-helper",
                false,
                StartWait::StoppedUnstarted {
                    stopped: true,
                    traced: false
                }
            ),
        ],
        "each named by its executable where one reads, never by p_comm's 16 bytes, with what \
         keeps it; the stand-in's 30 ms row counted"
    );
    assert!(held.iter().all(|h| h.row.exec_version == Some(h.row.exec)));
    assert_eq!(young, [], "none read unrun but unlisted");
    assert!(
        held_list(
            &sweep,
            &members,
            None,
            Some(30),
            0,
            |_| Some(judged(AtStart::Held)),
            |_| None,
            |_| true,
            |_| None,
        )
        .0
        .is_empty(),
        "no user read, nothing counted"
    );
}

/// Cached held facts select a member, but only a fresh held probe between equal actual
/// versions certifies its exec. NEGATIVE CONTROLS: the member has started, execs during the
/// fresh probe, or has no readable version; its descriptive row remains unproved.
#[test]
fn a_held_version_is_bound_to_a_fresh_start_read() {
    use std::cell::Cell;
    let member = ProcFacts {
        pid: 601,
        uid: Some(501),
        start: Some(7),
        start_facts: Some(probe::StartFacts {
            lone_zero: true,
            user_ns: Some(0),
            program_check_wait: true,
            local: Some(true),
            age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
            ..probe::StartFacts::default()
        }),
        ..ProcFacts::default()
    };
    let read = |before, fresh, after| {
        let version = Cell::new(before);
        let (rows, _) = held_list(
            &[],
            std::slice::from_ref(&member),
            Some(501),
            None,
            0,
            |_| {
                version.set(after);
                Some(judged(fresh))
            },
            |_| version.get(),
            |_| true,
            |_| None,
        );
        assert_eq!(rows.len(), 1, "the cached descriptive held row remains");
        rows[0].row.exec_version
    };
    assert_eq!(read(Some(17), AtStart::Held, Some(17)), Some(17));
    assert_eq!(read(Some(17), AtStart::Held, Some(18)), None);
    assert_eq!(read(Some(17), AtStart::Started, Some(17)), None);
    assert_eq!(read(None, AtStart::Held, None), None);
}

/// Each held row forks its start facts' age before the clock those ages were taken at: a
/// member by its own reading's facts, a row outside every job by the start reader's, never by
/// the sweep's kernel start (Mach ticks on macOS). The stand-in outside every job is read
/// held unasked, so its fork time is unread.
#[test]
fn a_held_row_forks_its_start_facts_age_before_the_pass_clock() {
    const ME: u32 = 501;
    const NOW_US: u64 = 1_790_000_000_000_000;
    let row = |pid| ProcessReading {
        pid,
        uid: Some(ME),
        cpu_ns: Some(2_000_000),
        start: Some(987_654_321),
        name: format!("p{pid}"),
        ..ProcessReading::default()
    };
    let held_for = |age_ms| StartFacts {
        age_ms: Some(age_ms),
        ..judged(AtStart::Held)
    };
    let member = ProcFacts {
        pid: 10,
        uid: Some(ME),
        name: "gh".into(),
        start: Some(987_654_321),
        start_facts: Some(held_for(90_000)),
        ..ProcFacts::default()
    };
    let (held, _) = held_list(
        &[row(10), row(20), row(30)],
        std::slice::from_ref(&member),
        Some(ME),
        Some(30),
        NOW_US,
        |pid| (pid == 20).then(|| held_for(25_000)),
        |_| None,
        |_| true,
        |_| None,
    );
    let forks: Vec<(u32, Option<u64>)> =
        held.iter().map(|h| (h.row.pid, h.row.started_ms)).collect();
    assert_eq!(
        forks,
        [
            (10, Some(NOW_US / 1000 - 90_000)),
            (20, Some(NOW_US / 1000 - 25_000)),
            (30, None),
        ]
    );
}

/// The pass hands the engine its held list: held rows and the stand-in reach the program-check
/// findings, a translating row `translating`, and only those in this process's coalition count
/// as aterm's.
#[test]
fn the_pass_hands_the_engine_this_users_held_programs_and_the_stand_in() {
    const ME: u32 = 501;
    let row = |pid, cpu_ms: u64| ProcessReading {
        pid,
        uid: Some(ME),
        cpu_ns: Some(cpu_ms * 1_000_000),
        name: format!("p{pid}"),
        ..ProcessReading::default()
    };
    let sweep = [row(10, 400), row(20, 2), row(21, 2), row(22, 2), row(23, 2)];
    let mut pass = JobsPass::new();
    pass.held = HeldReaders {
        start: |pid, _, _| {
            Some(judged(match pid {
                20 | 23 => AtStart::Held,
                22 => AtStart::Translating,
                _ => AtStart::Started,
            }))
        },
        exec: |pid| Some(u64::from(pid)),
        coalition: |pid| Some(if pid == 23 { 8 } else { 7 }),
        exe_name: |pid| (pid == 20).then(|| "codex-command-runner".to_owned()),
        stand_in: || Some(10),
        ..HeldReaders::INERT
    };
    let view = pass
        .tick(
            10_000,
            || 10_000,
            Some(&sweep),
            &[],
            &[],
            &[],
            &GrantFacts::default(),
            10,
            false,
            true,
        )
        .expect("a view");
    let held: Vec<(u32, &str, Option<u64>)> = view
        .findings
        .program_check
        .iter()
        .map(|n| (n.pid, n.name.as_str(), n.tab))
        .collect();
    assert_eq!(
        held,
        [
            (10, "p10", None),
            (20, "codex-command-runner", None),
            (23, "p23", None)
        ],
        "the reader's held rows and the stand-in's, outside every tab"
    );
    assert_eq!(
        view.findings
            .translating
            .iter()
            .map(|n| n.pid)
            .collect::<Vec<_>>(),
        [22],
        "the translating row under its own kind"
    );
    assert_eq!(
        view.findings.program_check_aterm,
        (2, 0),
        "aterm's: the two in this process's coalition, not 23 in another"
    );
}

/// THE PASS TELLS THE CHECK WHETHER A PROGRAM WAITS AT IT ([`HeldReaders::check`]'s argument,
/// which rings the stuck check's watch from its rest): `true` for a pass holding a row the
/// program check holds, `false` for one whose only held row translates under Rosetta and one with
/// none, so the watch looks while a start waits and rests otherwise.
#[test]
fn the_pass_tells_the_check_whether_a_program_waits_at_it() {
    use std::sync::atomic::{AtomicU8, Ordering};
    const ME: u32 = 501;
    /// 0: not asked; 1: asked with none waiting; 2: asked with one waiting.
    static ASKED: AtomicU8 = AtomicU8::new(0);
    static HELD: AtomicU8 = AtomicU8::new(0);
    let row = |pid| ProcessReading {
        pid,
        uid: Some(ME),
        cpu_ns: Some(2_000_000),
        name: format!("p{pid}"),
        ..ProcessReading::default()
    };
    let asked = |held: AtStart| {
        HELD.store(
            match held {
                AtStart::Held => 1,
                AtStart::Translating => 2,
                _ => 0,
            },
            Ordering::SeqCst,
        );
        ASKED.store(0, Ordering::SeqCst);
        let mut pass = JobsPass::new();
        pass.held = HeldReaders {
            start: |pid, _, _| {
                Some(judged(match (pid, HELD.load(Ordering::SeqCst)) {
                    (20, 1) => AtStart::Held,
                    (20, 2) => AtStart::Translating,
                    _ => AtStart::Started,
                }))
            },
            exec: |pid| Some(u64::from(pid)),
            check: |waiting| {
                ASKED.store(if waiting { 2 } else { 1 }, Ordering::SeqCst);
                None
            },
            ..HeldReaders::INERT
        };
        let _ = pass.tick(
            10_000,
            || 10_000,
            Some(&[row(10), row(20)]),
            &[],
            &[],
            &[],
            &GrantFacts::default(),
            10,
            false,
            true,
        );
        ASKED.load(Ordering::SeqCst)
    };
    assert_eq!(asked(AtStart::Held), 2, "a held row waits at the check");
    assert_eq!(
        asked(AtStart::Translating),
        1,
        "CONTROL: a translating row waits elsewhere"
    );
    assert_eq!(asked(AtStart::Started), 1, "CONTROL: none waits");
}

/// THE PASS NAMES STRANDED ONLY A HELD PROGRAM OLDER THAN A DAEMON THAT ANSWERS ALONE (leftover
/// §5.1): with the syspolicyd running now started at T, up the bound awake, its pool not full,
/// a member and a sweep row seen unrun since T − 8 s read stranded by the facts `held_list`
/// kept, once every held program of this user reads no backlog. None does beside a held row
/// forked at T or after (a backlog), nor with the daemon a µs short of the bound by the wall
/// clock or the awake one; a full pool or no fresh look judges nothing; a row seen unrun only
/// from T, a translating row, an undated one and the stand-in are never stranded. Every row is
/// judged only while each is stranded or forked since T, and a row a pass judged stranded
/// stays so whatever this one reads.
#[test]
fn the_pass_names_stranded_only_a_held_program_older_than_a_daemon_that_answers_alone() {
    use aterm_vitals::program_check::Daemon;
    const ME: u32 = 501;
    const SEC: u64 = 1_000_000;
    const T: u64 = 1_791_000_000 * SEC;
    const AWAKE_MS: u64 = probe::STRANDED_AFTER_US / 1_000;
    let daemon = Daemon {
        pid: 11291,
        start_s: T / SEC,
        start_us: 0,
    };
    let up = T + probe::STRANDED_AFTER_US;
    let awake = |pool_full, awake_ms| {
        Some(RunningCheck {
            daemon,
            pool_full,
            awake_ms,
        })
    };
    let check = |pool_full| awake(pool_full, AWAKE_MS);
    let forked = |at, fork_us| StartFacts {
        start_us: Some(fork_us),
        ..judged(at)
    };
    let row = |pid| ProcessReading {
        pid,
        uid: Some(ME),
        cpu_ns: Some(2_000_000),
        name: format!("p{pid}"),
        ..ProcessReading::default()
    };
    let member = ProcFacts {
        pid: 10,
        uid: Some(ME),
        name: "band".into(),
        start_facts: Some(forked(AtStart::Held, T - 8 * SEC)),
        ..ProcFacts::default()
    };
    let (rows, young) = held_list(
        &[row(20), row(30)],
        std::slice::from_ref(&member),
        Some(ME),
        Some(30),
        T,
        |_| Some(forked(AtStart::Held, T - 8 * SEC)),
        |pid| Some(u64::from(pid)),
        |_| false,
        |_| None,
    );
    assert_eq!(young, [], "none read unrun but unlisted");
    assert_eq!(
        rows.iter()
            .map(|h| (h.row.pid, h.facts))
            .collect::<Vec<_>>(),
        [
            (10, member.start_facts),
            (20, member.start_facts),
            (30, None)
        ],
        "the member's own reading, the sweep row's read, none for the stand-in"
    );
    let alone: Vec<Held> = rows
        .into_iter()
        .map(|h| Held {
            sent_by_us: Some(T - 8 * SEC),
            ..h
        })
        .collect();
    let verdict_by = |mut rows: Vec<Held>, check, now_us, backlog: Option<usize>| {
        let (by, every) = strand(&mut rows, check, now_us, |since| {
            assert_eq!(since, T, "the backlog since the daemon's start");
            backlog
        });
        let stranded: Vec<u32> = rows
            .iter()
            .filter(|h| h.row.stranded)
            .map(|h| h.row.pid)
            .collect();
        (by, every, stranded)
    };
    let verdict = |rows, check, now_us| verdict_by(rows, check, now_us, Some(0));
    assert_eq!(
        verdict(alone.clone(), check(false), up),
        (Some(11291), false, vec![10, 20]),
        "held since before the daemon running now, which answers alone; never the stand-in, \
         which no read judges"
    );
    let held_row = |pid, at, fork_us, sent_by_us| {
        let facts = forked(at, fork_us);
        Held {
            row: HeldProc {
                pid,
                exec: u64::from(pid),
                exec_version: Some(u64::from(pid)),
                name: format!("p{pid}"),
                wait: start_wait(probe::judge_start(&facts)).expect("not started"),
                ..HeldProc::default()
            },
            facts: Some(facts),
            cpu_ns: Some(2_000_000),
            sent_by_us: Some(sent_by_us),
            stranded_by: None,
        }
    };
    let beside = |pid, at, fork_us, sent_by_us| {
        let mut rows = alone.clone();
        rows.push(held_row(pid, at, fork_us, sent_by_us));
        rows
    };
    let traced = AtStart::StoppedUnstarted {
        stopped: false,
        traced: true,
    };
    for (case, rows, want) in [
        (
            "a held row forked at T + 1 µs, a slow first exec the daemon running now has yet \
             to answer, is its backlog, and no verdict stands for the rows before T",
            beside(40, AtStart::Held, T + 1, T + 1),
            (Some(11291), false, vec![]),
        ),
        (
            "a held row forked at T is its backlog",
            beside(41, AtStart::Held, T, T),
            (Some(11291), false, vec![]),
        ),
        (
            "CONTROL: a row forked before T but seen unrun only from T on may have exec'd \
             since, so it is not stranded, and the others still are",
            beside(42, AtStart::Held, T - 8 * SEC, T),
            (Some(11291), false, vec![10, 20]),
        ),
        (
            "CONTROL: a translating row forked since T is never stranded nor a backlog",
            beside(43, AtStart::Translating, T + SEC, T + SEC),
            (Some(11291), false, vec![10, 20]),
        ),
        (
            "CONTROL: a traced unstarted row forked since T is never stranded nor a backlog",
            beside(46, traced, T + SEC, T + SEC),
            (Some(11291), false, vec![10, 20]),
        ),
        (
            "CONTROL: rows all forked since T, one translating and one traced, are all judged, \
             none stranded",
            vec![
                held_row(44, AtStart::Held, T + SEC, T + SEC),
                held_row(45, AtStart::Translating, T - 8 * SEC, T - 8 * SEC),
                held_row(47, traced, T - 8 * SEC, T - 8 * SEC),
            ],
            (Some(11291), true, vec![]),
        ),
    ] {
        assert_eq!(verdict(rows, check(false), up), want, "{case}");
    }
    assert_eq!(
        verdict(alone.clone(), check(false), up - 1),
        (Some(11291), false, vec![]),
        "the daemon a µs short of the bound on a first exec gives no verdict"
    );
    assert_eq!(
        verdict(alone.clone(), awake(false, AWAKE_MS - 1), up + 3_600 * SEC),
        (Some(11291), false, vec![]),
        "an hour up by the wall clock but a ms short of the bound awake: a sleep counts for \
         nothing"
    );
    for (backlog, want) in [(Some(1), vec![]), (None, vec![]), (Some(0), vec![10, 20])] {
        assert_eq!(
            verdict_by(alone.clone(), check(false), up, backlog),
            (Some(11291), false, want),
            "a held program of this user the pass does not list ({backlog:?}) is a backlog too"
        );
    }
    assert_eq!(
        verdict(alone.clone(), check(true), up),
        (Some(11291), false, vec![]),
        "CONTROL: a full pool judges nothing"
    );
    assert_eq!(
        verdict(alone.clone(), None, up),
        (None, false, vec![]),
        "CONTROL: no fresh look judges nothing"
    );
    let undated: Vec<Held> = alone
        .iter()
        .cloned()
        .map(|h| Held {
            sent_by_us: None,
            ..h
        })
        .collect();
    assert_eq!(
        verdict(undated, check(false), up),
        (Some(11291), false, vec![]),
        "an undated row is not judged"
    );
    let kept: Vec<Held> = alone
        .iter()
        .cloned()
        .map(|h| Held {
            stranded_by: h.facts.map(|_| 690),
            ..h
        })
        .collect();
    let mut later = kept.clone();
    later.push(held_row(40, AtStart::Held, T + 1, T + 1));
    for (case, rows, check, want) in [
        ("no fresh look", kept.clone(), None, Some(690)),
        ("a full pool", kept.clone(), check(true), Some(11291)),
        ("a backlog", later, check(false), Some(11291)),
    ] {
        assert_eq!(
            verdict(rows, check, up),
            (want, false, vec![10, 20]),
            "a row a pass judged stranded stays so beside {case}, against the daemon read now \
             or else the one it was judged under"
        );
    }
}

/// THE PASS HANDS THE ENGINE THE DAEMON IT JUDGED AGAINST: a held row its passes read unrun at
/// one CPU since before the syspolicyd the stuck check reads now started, once that daemon is
/// up the bound, is the engine's stranded row and privacy's count, with the daemon's pid. A row
/// first read after that start, and one whose CPU moved between the passes, start over and are
/// only held; with no check read (the inert arm's) the same rows are held and none is judged.
#[test]
fn the_pass_hands_the_engine_the_daemon_it_judged_against() {
    use aterm_vitals::jobs::Named;
    use aterm_vitals::program_check::Daemon;
    use std::sync::atomic::{AtomicU64, Ordering};
    const ME: u32 = 501;
    const SEC: u64 = 1_000_000;
    const T: u64 = 1_791_000_000 * SEC;
    static NOW: AtomicU64 = AtomicU64::new(0);
    let row = |pid, cpu_ns| ProcessReading {
        pid,
        uid: Some(ME),
        cpu_ns: Some(cpu_ns),
        name: format!("p{pid}"),
        ..ProcessReading::default()
    };
    let readers = HeldReaders {
        start: |_, _, _| {
            Some(StartFacts {
                start_us: Some(T - 30 * SEC),
                ..judged(AtStart::Held)
            })
        },
        exec: |pid| Some(u64::from(pid)),
        check: |_| {
            let (pid, started) = if NOW.load(Ordering::SeqCst) < T {
                (690, T - 3_600 * SEC)
            } else {
                (11291, T)
            };
            Some(RunningCheck {
                daemon: Daemon {
                    pid,
                    start_s: started / SEC,
                    start_us: 0,
                },
                pool_full: false,
                awake_ms: u64::MAX,
            })
        },
        backlog: |_| Some(0),
        now_us: || NOW.load(Ordering::SeqCst),
        ..HeldReaders::INERT
    };
    let run = |readers: HeldReaders| {
        let mut pass = JobsPass::new();
        pass.held = readers;
        NOW.store(T - 8 * SEC, Ordering::SeqCst);
        let first = [row(10, 400_000_000), row(20, 2_000_000), row(22, 2_000_000)];
        let _ = pass.tick(
            10_000,
            || 10_000,
            Some(&first),
            &[],
            &[],
            &[],
            &GrantFacts::default(),
            10,
            false,
            true,
        );
        NOW.store(T + probe::STRANDED_AFTER_US, Ordering::SeqCst);
        let then = [
            row(10, 400_000_000),
            row(20, 2_000_000),
            row(21, 2_000_000),
            row(22, 3_000_000),
        ];
        pass.tick(
            12_000,
            || 12_000,
            Some(&then),
            &[],
            &[],
            &[],
            &GrantFacts::default(),
            10,
            false,
            true,
        )
        .expect("a view")
    };
    let pids = |list: &[Named]| list.iter().map(|n| n.pid).collect::<Vec<_>>();
    let view = run(readers);
    let f = &view.findings;
    assert_eq!(pids(&f.program_check), [20, 21, 22], "every one held");
    assert_eq!(
        pids(&f.program_check_stranded),
        [20],
        "only the row read unrun at one CPU since before the daemon started"
    );
    assert_eq!(
        f.program_check_daemon,
        Some(11291),
        "the daemon judged against"
    );
    assert_eq!(
        program_check_of(&view, None, Duration::ZERO, false).map(|c| c.stranded),
        Some(Some(1))
    );
    let inert = run(HeldReaders {
        check: HeldReaders::INERT.check,
        ..readers
    });
    let f = &inert.findings;
    assert_eq!(pids(&f.program_check), [20, 21, 22], "CONTROL: still held");
    assert_eq!(
        (pids(&f.program_check_stranded), f.program_check_daemon),
        (vec![], None),
        "CONTROL: no check read judges nothing"
    );
    assert_eq!(
        program_check_of(&inert, None, Duration::ZERO, false).map(|c| c.stranded),
        Some(None)
    );
}

/// A PASS DATES A ROW FROM ITS FIRST UNRUN READ AND KEEPS ITS VERDICT: a row read unrun at
/// T − 1 s, before the syspolicyd running now started at T, still younger than the held bar, is
/// dated then, so it reads stranded once the daemon is up the bound; the verdict stands beside
/// a backlog and through a pass with no fresh look. A row whose CPU moved, a young row whose
/// thread ran, and a young row that read stopped start over.
#[test]
fn a_pass_dates_a_row_from_its_first_unrun_read_and_keeps_its_verdict() {
    use aterm_vitals::program_check::Daemon;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    const ME: u32 = 501;
    const SEC: u64 = 1_000_000;
    const T: u64 = 1_791_000_000 * SEC;
    static NOW: AtomicU64 = AtomicU64::new(0);
    static LOOK: AtomicBool = AtomicBool::new(true);
    let row = |pid, cpu_ns| ProcessReading {
        pid,
        uid: Some(ME),
        cpu_ns: Some(cpu_ns),
        name: format!("p{pid}"),
        ..ProcessReading::default()
    };
    let mut pass = JobsPass::new();
    pass.held = HeldReaders {
        start: |pid, now_us, _| {
            let fork = match pid {
                50..=52 => T - 6 * SEC,
                60 => T + 300 * SEC,
                _ => T - 30 * SEC,
            };
            let before = NOW.load(Ordering::SeqCst) < T;
            Some(StartFacts {
                lone_zero: pid != 51 || !before,
                user_ns: Some(0),
                program_check_wait: true,
                stopped: pid == 52 && before,
                local: Some(true),
                age_ms: Some(now_us.saturating_sub(fork) / 1_000),
                start_us: Some(fork),
                ..StartFacts::default()
            })
        },
        exec: |pid| Some(u64::from(pid)),
        check: |_| {
            let now = NOW.load(Ordering::SeqCst);
            let (pid, started) = if now < T {
                (690, T - 3_600 * SEC)
            } else {
                (11291, T)
            };
            LOOK.load(Ordering::SeqCst).then_some(RunningCheck {
                daemon: Daemon {
                    pid,
                    start_s: started / SEC,
                    start_us: 0,
                },
                pool_full: false,
                awake_ms: now.saturating_sub(started) / 1_000,
            })
        },
        backlog: |_| Some(0),
        now_us: || NOW.load(Ordering::SeqCst),
        ..HeldReaders::INERT
    };
    let mut tick = |at_ms, now_us, rows: &[ProcessReading]| {
        NOW.store(now_us, Ordering::SeqCst);
        let view = pass
            .tick(
                at_ms,
                || at_ms,
                Some(rows),
                &[],
                &[],
                &[],
                &GrantFacts::default(),
                10,
                false,
                true,
            )
            .expect("a view");
        let f = &view.findings;
        (
            f.program_check.iter().map(|n| n.pid).collect::<Vec<_>>(),
            f.program_check_stranded
                .iter()
                .map(|n| n.pid)
                .collect::<Vec<_>>(),
            f.program_check_daemon,
            f.program_check_judged,
        )
    };
    let cpu = 2_000_000;
    let me = row(10, 400_000_000);
    let first = [
        me.clone(),
        row(20, cpu),
        row(22, cpu),
        row(50, cpu),
        row(51, cpu),
        row(52, cpu),
    ];
    assert_eq!(
        tick(10_000, T - SEC, &first),
        (vec![20, 22], vec![], Some(690), true),
        "50, 51 and 52 are five seconds old, under the held bar; 20 and 22 forked since 690 \
         started are judged, none stranded"
    );
    let up = T + probe::STRANDED_AFTER_US;
    assert_eq!(
        tick(12_000, up, &first),
        (
            vec![20, 22, 50, 51, 52],
            vec![20, 22, 50],
            Some(11291),
            false
        ),
        "50 read unrun before it was held, from T − 1 s; CONTROL: 51's thread ran at its \
         first read and 52 read stopped, so each dates from its second, after T"
    );
    let backlog = [
        me,
        row(20, cpu),
        row(22, cpu + 1),
        row(50, cpu),
        row(51, cpu),
        row(52, cpu),
        row(60, cpu),
    ];
    assert_eq!(
        tick(14_000, up + 2 * SEC, &backlog),
        (
            vec![20, 22, 50, 51, 52, 60],
            vec![20, 50],
            Some(11291),
            false
        ),
        "kept beside 60, a backlog forked at T + 5 m; CONTROL: 22's CPU moved, so its run \
         starts over and its verdict goes"
    );
    LOOK.store(false, Ordering::SeqCst);
    assert_eq!(
        tick(16_000, up + 4 * SEC, &backlog),
        (
            vec![20, 22, 50, 51, 52, 60],
            vec![20, 50],
            Some(11291),
            false
        ),
        "kept with no fresh look, against the daemon it was judged under"
    );
}

/// The pass hands the engine the grant's facts, also from the vitals request through
/// `vitals_host::jobs_step`: aterm's held program forked after the grant counts against it;
/// with no grant time nothing counts, and nothing outside every tab while a tab the facts name
/// adopted is open.
#[test]
fn the_pass_hands_the_engine_the_grant_time_and_the_adopted_tabs() {
    const ME: u32 = 501;
    let row = |pid, cpu_ms: u64| ProcessReading {
        pid,
        uid: Some(ME),
        cpu_ns: Some(cpu_ms * 1_000_000),
        name: format!("p{pid}"),
        ..ProcessReading::default()
    };
    let pass = || {
        let mut pass = JobsPass::new();
        pass.held = HeldReaders {
            start: |_, _, _| Some(judged(AtStart::Held)),
            coalition: |_| Some(7),
            ..HeldReaders::INERT
        };
        pass
    };
    let tab = TabRoot {
        key: 41,
        tab: 41,
        shell_pid: 941,
        label: "Tab 1".into(),
        program: String::new(),
        tab_id: None,
        agent_leader: None,
    };
    let after_grant = |grant: &GrantFacts| {
        pass()
            .tick(
                10_000,
                || 10_000,
                Some(&[row(10, 400), row(20, 2)]),
                std::slice::from_ref(&tab),
                &[],
                &[],
                grant,
                10,
                false,
                true,
            )
            .expect("a view")
            .findings
            .program_check_after_grant
    };
    let granted = |adopted: Vec<u64>| GrantFacts {
        grant_ms: Some(1),
        adopted,
    };
    assert_eq!(
        after_grant(&granted(Vec::new())),
        1,
        "aterm's held program forked after the grant"
    );
    assert_eq!(after_grant(&GrantFacts::default()), 0, "no grant time");
    assert_eq!(
        after_grant(&granted(vec![41])),
        0,
        "outside every tab while a tab is adopted"
    );

    let req = crate::vitals_host::Request {
        self_pid: 10,
        jobs_now: true,
        grant: granted(Vec::new()),
        ..crate::vitals_host::Request::default()
    };
    let epoch = Instant::now();
    let view = crate::vitals_host::jobs_step(
        &mut pass(),
        &req.tab_reads(epoch, || epoch),
        Some(&[row(10, 400), row(20, 2)]),
        &req,
        false,
    )
    .expect("a view");
    assert_eq!(
        view.findings.program_check_after_grant, 1,
        "the request's grant reaches the engine"
    );
}

/// With this process's coalition unread, no held program outside every job counts as aterm's:
/// one whose own coalition is unread too is no match.
#[test]
fn with_this_process_coalition_unread_no_held_program_is_aterms() {
    const ME: u32 = 501;
    let row = |pid, cpu_ms: u64| ProcessReading {
        pid,
        uid: Some(ME),
        cpu_ns: Some(cpu_ms * 1_000_000),
        name: format!("p{pid}"),
        ..ProcessReading::default()
    };
    let mut pass = JobsPass::new();
    pass.held = HeldReaders {
        start: |_, _, _| Some(judged(AtStart::Held)),
        coalition: |_| None,
        ..HeldReaders::INERT
    };
    let view = pass
        .tick(
            10_000,
            || 10_000,
            Some(&[row(10, 400), row(20, 2)]),
            &[],
            &[],
            &[],
            &GrantFacts::default(),
            10,
            false,
            true,
        )
        .expect("a view");
    assert_eq!(
        view.findings
            .program_check
            .iter()
            .map(|n| n.pid)
            .collect::<Vec<_>>(),
        [20],
        "held"
    );
    assert_eq!(view.findings.program_check_aterm.0, 0, "not aterm's");
}

/// A root's program-change version stays separate from a running member's naming identity.
/// Unread, held and stand-in members are never asked their exec version.
#[test]
fn root_program_versions_and_running_member_identities_are_read_separately() {
    use std::cell::RefCell;
    let read = [
        ProcFacts {
            pid: 10,
            ..ProcFacts::default()
        },
        ProcFacts {
            pid: 11,
            ..ProcFacts::default()
        },
        ProcFacts {
            pid: 12,
            start_facts: Some(judged(AtStart::Started)),
            ..ProcFacts::default()
        },
        ProcFacts {
            pid: 13,
            start_facts: Some(judged(AtStart::Held)),
            ..ProcFacts::default()
        },
        ProcFacts {
            pid: 14,
            start_facts: Some(judged(AtStart::Started)),
            ..ProcFacts::default()
        },
    ];
    let asked = RefCell::new(Vec::new());
    let procs = job_procs(
        &read,
        &BTreeSet::from([10]),
        Some(14),
        |pid| {
            asked.borrow_mut().push(pid);
            Some(u64::from(pid) * 100 + 7)
        },
        |_| None,
    );
    assert_eq!(
        procs
            .iter()
            .map(|p| (p.pid, p.version, p.exec_version))
            .collect::<Vec<_>>(),
        [
            (10, Some(1007), None),
            (11, None, None),
            (12, None, Some(1207)),
            (13, None, None),
            (14, None, None)
        ]
    );
    assert_eq!(asked.into_inner(), [10, 12]);
}

/// A forced reading without a sweep cannot authorize naming from its cached jobs view.
#[test]
fn naming_needs_an_actual_jobs_pass_instead_of_a_carried_view() {
    let mut pass = JobsPass::new();
    let target = aterm_vitals::sys::query::NameTarget::Job("missing".into());
    for (at_ms, sweep, now, expected) in [
        (1, Some(&[][..]), true, NamingError::Gone),
        (2, Some(&[][..]), false, NamingError::Unread),
        (3, None, true, NamingError::Unread),
    ] {
        assert!(
            pass.tick(
                at_ms,
                || at_ms,
                sweep,
                &[],
                &[],
                &[],
                &GrantFacts::default(),
                0,
                false,
                now
            )
            .is_some()
        );
        assert!(matches!(pass.capture_name(7, &target), Err(error) if error == expected));
    }
}

/// How many times a stand-in's arguments, and a stand-in ledger, were read.
static FRONT_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static LEDGER_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Whether the stand-in ledger holds pid 40's claim (admitted) rather than keeping it waiting.
static ADMITTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The hop's argv for `tool` at `target`.
fn hop(tool: &str, target: &str) -> Vec<OsString> {
    [
        "/Applications/aterm.app/Contents/MacOS/atpkg",
        "__build_admit",
        "--shim",
        tool,
        "-",
        target,
        "--unverified",
        "build",
    ]
    .iter()
    .map(Into::into)
    .collect()
}

/// Pids 40 and 46 run a shim's front (the hop's argv; 46's target a name `status` cleans,
/// `targo@dev`), 41 another verb of the same image; each read counts.
fn stand_in_argv(pid: u32) -> Option<Vec<OsString>> {
    FRONT_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Some(match pid {
        40 => hop("targo", "/store/trust/bin/targo"),
        46 => hop("targo-tippy", "/store/trust/bin/targo@dev"),
        41 => [
            "/Applications/aterm.app/Contents/MacOS/atpkg",
            "__build_cache_register",
            "targo",
        ]
        .iter()
        .map(Into::into)
        .collect(),
        _ => return None,
    })
}

/// The stand-in ledger: pid 77 holds 8 GiB; pid 40's claim waits behind it, or is held once
/// [`ADMITTED`]; pid 46's waits behind both.
fn stand_in_claims(_: &aterm_capacity::Ledger) -> Option<Vec<aterm_capacity::Record>> {
    LEDGER_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let record = |pid: u32, start_ms: u64, held: bool| aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Holder,
        timing: aterm_capacity::TimingEvidence::default(),
        pid,
        start_ms,
        seq: 0,
        checkout: "/ws".to_owned(),
        need: aterm_capacity::Need::new("a build").mem(8 << 30),
        held: held.then_some(aterm_capacity::Held {
            admitted_ms: start_ms,
            cores: 1,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    };
    let admitted = ADMITTED.load(std::sync::atomic::Ordering::SeqCst);
    Some(vec![
        record(77, 1_000, true),
        record(40, 2_000, admitted),
        record(46, 3_000, false),
    ])
}

/// A MEMBER IS NAMED BY THE BUILD ITS FRONT BECOMES, WAITING BEHIND THE CLAIMS ITS LEDGER
/// NAMES: only a member running the front's image is asked, its arguments read once for the
/// program it runs and its ledger once a pass for every front that claims there; it is named
/// by the one rule `status` names a front by (`targo@dev` reads `targodev` in both); once
/// its ledger holds its claim it is still named by its build but waits for none. A member whose
/// thread list shows it never ran (held mid-start) and the development stand-in are never
/// read. NEGATIVE CONTROL: another verb of the same image becomes nothing, a build typed
/// plainly and a front that exec'd its build are not read, being no front's image.
#[test]
fn a_member_is_named_by_the_build_its_front_becomes() {
    use std::sync::atomic::Ordering;
    let ran = |pid: u32, name: &str, lone_zero: bool| ProcFacts {
        pid,
        name: name.to_owned(),
        exe: Some(name.to_owned()),
        start: Some(u64::from(pid)),
        start_facts: Some(probe::StartFacts {
            lone_zero,
            ..probe::StartFacts::default()
        }),
        ..ProcFacts::default()
    };
    let read = [
        ran(40, "atpkg", false),
        ran(41, "atpkg", false),
        ran(42, "targo", false),
        ran(43, "atpkg", true),
        ProcFacts {
            pid: 44,
            ..ProcFacts::default()
        },
        ran(46, "atpkg", false),
    ];
    let readers = FrontReaders {
        argv: stand_in_argv,
        ledger: |_| {
            Some(aterm_capacity::Ledger::at(std::path::PathBuf::from(
                "/scratch",
            )))
        },
        claims: stand_in_claims,
    };
    let mut fronts = Fronts::new(readers);
    let pass = |fronts: &mut Fronts, read: &[ProcFacts], stand_in: Option<u32>| {
        let procs = job_procs(
            read,
            &BTreeSet::new(),
            stand_in,
            |_| None,
            |p| fronts.front(p),
        )
        .into_iter()
        .map(|p| (p.pid, p.becomes, p.claims_ahead))
        .collect::<Vec<_>>();
        fronts.keep(read);
        procs
    };
    let reads = || {
        (
            FRONT_READS.load(Ordering::SeqCst),
            LEDGER_READS.load(Ordering::SeqCst),
        )
    };
    FRONT_READS.store(0, Ordering::SeqCst);
    LEDGER_READS.store(0, Ordering::SeqCst);
    ADMITTED.store(false, Ordering::SeqCst);
    let named = pass(&mut fronts, &read, None);
    assert_eq!(
        named,
        [
            (40, Some("targo".to_owned()), Some(vec![77])),
            (41, None, None),
            (42, None, None),
            (43, None, None),
            (44, None, None),
            (46, Some("targodev".to_owned()), Some(vec![77, 40])),
        ]
    );
    assert_eq!(
        crate::session_status::program::front_program(&hop("targo-tippy", "/b/targo@dev")),
        Some("targodev".to_owned()),
        "as `status` names it"
    );
    assert_eq!(
        reads(),
        (3, 1),
        "the members that ran its image; one ledger read a pass"
    );
    assert_eq!(pass(&mut fronts, &read, None), named);
    assert_eq!(
        reads(),
        (3, 2),
        "arguments once a program, the ledger each pass"
    );
    ADMITTED.store(true, Ordering::SeqCst);
    let admitted = pass(&mut fronts, &read[..1], None);
    assert_eq!(
        admitted,
        [(40, Some("targo".to_owned()), None)],
        "its ledger holds its claim"
    );
    let execd = [ran(40, "targo", false)];
    assert_eq!(pass(&mut fronts, &execd, None), [(40, None, None)]);
    assert_eq!(reads(), (3, 3), "the build it exec'd is not read");
    let mut again = Fronts::new(readers);
    assert_eq!(pass(&mut again, &read[..1], Some(40)), [(40, None, None)]);
    assert_eq!(reads(), (3, 3), "the stand-in is never read");
}

/// The stand-in reads held only where the reading judged a start (never a zombie's or another
/// user's), and a member the kernel reads held stays held.
#[test]
fn a_stand_in_reads_held_only_where_the_reading_judged_a_start() {
    let read = ProcFacts {
        pid: 30,
        start_facts: Some(probe::StartFacts::default()),
        ..ProcFacts::default()
    };
    let unread = ProcFacts {
        pid: 31,
        ..ProcFacts::default()
    };
    let held = ProcFacts {
        pid: 32,
        start_facts: Some(probe::StartFacts {
            lone_zero: true,
            user_ns: Some(0),
            program_check_wait: true,
            local: Some(true),
            age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
            ..probe::StartFacts::default()
        }),
        ..ProcFacts::default()
    };
    let held_at = |p: &ProcFacts, stand_in| job_proc(p, stand_in).start_wait;
    assert_eq!(held_at(&read, Some(30)), Some(StartWait::ProgramCheck));
    assert_eq!(held_at(&read, None), None);
    assert_eq!(
        held_at(&unread, Some(31)),
        None,
        "no start read: a zombie, or another user's"
    );
    assert_eq!(
        held_at(&held, Some(30)),
        Some(StartWait::ProgramCheck),
        "the kernel's own"
    );
}

/// A member that has not started hands the engine what keeps it from its first instruction,
/// each as its reading judges it: Rosetta translating it, a volume not proven local, a stop or
/// a tracer; one that started hands none. NEGATIVE CONTROL: hand the engine only a held start,
/// and a member Rosetta translates reads `wait=unnamed` beside its own `translating` finding.
#[test]
fn a_member_that_has_not_started_hands_the_engine_its_start_wait() {
    let at = |translated, local, stopped| ProcFacts {
        pid: 40,
        start_facts: Some(probe::StartFacts {
            lone_zero: true,
            user_ns: Some(0),
            program_check_wait: true,
            stopped,
            translated,
            local,
            age_ms: Some(probe::PROGRAM_CHECK_AGE_MS),
            ..probe::StartFacts::default()
        }),
        ..ProcFacts::default()
    };
    let wait = |p: &ProcFacts| job_proc(p, None).start_wait;
    assert_eq!(
        wait(&at(true, Some(true), false)),
        Some(StartWait::Translating)
    );
    assert_eq!(wait(&at(false, None, false)), Some(StartWait::NotStarted));
    assert_eq!(
        wait(&at(false, Some(true), true)),
        Some(StartWait::StoppedUnstarted {
            stopped: true,
            traced: false
        })
    );
    assert_eq!(
        wait(&at(false, Some(true), false)),
        Some(StartWait::ProgramCheck)
    );
    let started = ProcFacts {
        pid: 41,
        start_facts: Some(probe::StartFacts::default()),
        ..ProcFacts::default()
    };
    assert_eq!(wait(&started), None, "it ran past its start");
}

/// A leftover view offering a closed tab's row (an owner gone), which a reading with no window
/// reads again within 30 s.
pub(crate) fn offered_left() -> LeftView {
    LeftView {
        rows: vec![aterm_vitals::left::LeftRow {
            pid: 2606,
            state: aterm_vitals::left::State::Left,
            peer: None,
            check_ms: None,
            started_ms: Some(1_500),
            cpu_ms: None,
            exe_judged: None,
            tier: aterm_vitals::left::Tier::Offer,
            why: aterm_vitals::left::Why::OwnerGone,
            proof: Vec::new(),
            listen: false,
            from: aterm_vitals::left::Source::Closed(7),
            teller: None,
            token: None,
            foreground: false,
            after_agent: false,
        }],
        ..LeftView::default()
    }
}

/// A view speaks for now for a minute, and one read again only on edges (a parked one, or a
/// running job's: its own demand arms nothing with no window) for as long as its reader runs;
/// one read on a timer (a suspect agent call, a held wait, a leftover offer read with it) is
/// not read on edges, so past the minute it does not. A program that never ran and reads
/// stopped or traced is such a wait: a debugger's resume raises no edge. NEGATIVE CONTROL:
/// leave its finding out of `holds_a_wait`, and that view speaks past the minute.
#[test]
fn a_view_read_only_on_edges_speaks_for_now_while_its_reader_runs() {
    use aterm_vitals::jobs::{Findings, HeldEpisode, Named, StoppedUnstarted, Verdict};
    let parked = JobsView::default();
    assert_eq!(parked.demand, Demand::Idle);
    let running = JobsView {
        demand: Demand::Fast,
        rows: vec![crate::stats_paint::tests::job_row(
            "j41",
            Verdict::Fine,
            None,
            Some(1_000),
        )],
        ..JobsView::default()
    };
    let watched = JobsView {
        demand: Demand::Fast,
        rows: vec![JobRow {
            standing: Some(Standing::Capped),
            ..call_row(0, Consumer::AgentBg, true, 31, 27098, "python3")
        }],
        ..JobsView::default()
    };
    assert!(windowless_after(&watched).is_some(), "{watched:?}");
    let late = JOBS_FRESH + Duration::from_secs(1);
    assert!(speaks_for_now(&watched, None, JOBS_FRESH, false));
    assert!(
        speaks_for_now(&parked, None, late, true),
        "parked, read on edges"
    );
    assert!(
        speaks_for_now(&running, None, late, true),
        "running, read on edges"
    );
    assert!(!speaks_for_now(&watched, None, late, true), "due long ago");
    assert!(!speaks_for_now(&parked, None, late, false), "no reader");
    assert!(!speaks_for_now(&running, None, late, false), "no reader");
    let held = JobsView {
        findings: Findings {
            program_check: vec![Named {
                pid: 25311,
                exec_version: None,
                name: "gh".into(),
                tab: None,
                job: None,
                for_ms: 35_940_000,
            }],
            ..Findings::default()
        },
        ..JobsView::default()
    };
    let episode = JobsView {
        findings: Findings {
            program_check_episode: Some(HeldEpisode::default()),
            ..Findings::default()
        },
        ..JobsView::default()
    };
    let translating = JobsView {
        findings: Findings {
            translating: held.findings.program_check.clone(),
            ..Findings::default()
        },
        ..JobsView::default()
    };
    let unstarted = JobsView {
        findings: Findings {
            not_started: held.findings.program_check.clone(),
            ..Findings::default()
        },
        ..JobsView::default()
    };
    let stopped = JobsView {
        findings: Findings {
            stopped_unstarted: vec![StoppedUnstarted {
                named: held.findings.program_check[0].clone(),
                stopped: true,
                traced: true,
            }],
            ..Findings::default()
        },
        ..JobsView::default()
    };
    for view in [&held, &episode, &translating, &unstarted, &stopped] {
        assert_eq!(view.demand, Demand::Idle);
        assert!(speaks_for_now(view, None, JOBS_FRESH, true), "{view:?}");
        assert!(
            !speaks_for_now(view, None, late, true),
            "a held wait is read on a timer, not on edges: {view:?}"
        );
    }
}

/// A reading whose leftover view offers a closed tab's row is read again within 30 s with no
/// window (`vitals_host::held_wait_after`), so its jobs view, parked or running, speaks for now
/// for a minute and then, its reader having stopped delivering, no more; read without the
/// offer, the same views speak on. NEGATIVE CONTROL: `read_on_a_timer` that looks at the jobs
/// view alone lets both old views speak past the minute.
#[test]
fn a_view_read_with_a_leftover_offer_is_read_on_a_timer() {
    use aterm_vitals::jobs::Verdict;
    let offered = offered_left();
    assert!(aterm_vitals::left::holds_an_offer(&offered));
    let parked = JobsView::default();
    let running = JobsView {
        demand: Demand::Fast,
        rows: vec![crate::stats_paint::tests::job_row(
            "j41",
            Verdict::Fine,
            None,
            Some(1_000),
        )],
        ..JobsView::default()
    };
    let late = JOBS_FRESH + Duration::from_secs(1);
    for view in [&parked, &running] {
        assert!(read_on_a_timer(view, Some(&offered)), "{view:?}");
        assert!(speaks_for_now(view, Some(&offered), JOBS_FRESH, true));
        assert!(
            !speaks_for_now(view, Some(&offered), late, true),
            "the offer's 30 s re-read never came: {view:?}"
        );
        assert!(
            speaks_for_now(view, Some(&LeftView::default()), late, true),
            "no offer, read on edges: {view:?}"
        );
    }
    assert_eq!(
        program_check_of(&parked, Some(&offered), late, true),
        None,
        "privacy's count pulls past the minute"
    );
    assert_eq!(
        program_check_of(&parked, None, late, true).map(|c| (c.held, c.oldest_ms)),
        Some((0, 61_000)),
        "none held, read on edges"
    );
}

/// Privacy's count: every held program of this user, aterm's and others', the longest held to
/// now, and how many are stranded: a count whenever one is named, `0` only when the view judged
/// every held row, none otherwise; nothing from a view that does not speak for now.
#[test]
fn privacys_count_is_every_held_program_and_the_longest_wait_to_now() {
    use aterm_vitals::jobs::{Findings, Named};
    let named = |pid, for_ms| Named {
        pid,
        exec_version: None,
        name: "gh".into(),
        tab: None,
        job: None,
        for_ms,
    };
    let view = JobsView {
        findings: Findings {
            program_check: vec![named(25311, 35_940_000), named(25312, 20_000)],
            program_check_aterm: (1, 20_000),
            ..Findings::default()
        },
        ..JobsView::default()
    };
    assert_eq!(
        program_check_of(&view, None, Duration::from_millis(1_500), false),
        Some(HeldCount {
            held: 2,
            oldest_ms: 35_941_500,
            stranded: None
        }),
        "both held, the oldest plus the view's age, none judged stranded or not"
    );
    assert_eq!(
        program_check_of(&JobsView::default(), None, Duration::ZERO, false),
        Some(HeldCount {
            held: 0,
            oldest_ms: 0,
            stranded: None
        }),
        "none held"
    );
    assert_eq!(
        program_check_of(&view, None, JOBS_FRESH + Duration::from_secs(1), true),
        None,
        "a view that does not speak for now"
    );
    let judged = |stranded: Vec<Named>, every| JobsView {
        findings: Findings {
            program_check_stranded: stranded,
            program_check_daemon: Some(11291),
            program_check_judged: every,
            ..view.findings.clone()
        },
        ..JobsView::default()
    };
    for (stranded, every, want) in [
        (vec![named(25311, 35_940_000)], false, Some(1)),
        (Vec::new(), true, Some(0)),
        (Vec::new(), false, None),
    ] {
        assert_eq!(
            program_check_of(&judged(stranded, every), None, Duration::ZERO, false)
                .map(|c| c.stranded),
            Some(want),
            "a view counts its stranded rows, and none only when it judged every row \
             ({every})"
        );
    }
}

/// THE BOARD'S HELD PROGRAMS ARE THIS SESSION'S CALLS' AND THEIR ORPHANS': of the view's held
/// rows, session `s-a` (tab 4) is given the members named its agent's calls (a live call's, and
/// the orphan an ended call left, which the engine names by the call), oldest first, each held
/// for its `for_ms` as of the view, with the view's age; not the member of its shell's job or
/// of what one left (the person's), of a nested terminal session, tab 5's call (session
/// `s-b`'s) or a row no tab holds. A session with no held row is given none; a view past
/// [`JOBS_FRESH`] and a board no view was posted to give no reading.
#[test]
fn the_boards_held_programs_are_this_sessions_calls_and_their_orphans() {
    use aterm_vitals::jobs::{Findings, Named};
    let held = |pid, tab, job, for_ms| Named {
        pid,
        exec_version: Some(u64::from(pid)),
        name: "git-remote-https".into(),
        tab,
        job,
        for_ms,
    };
    let view = Arc::new(JobsView {
        findings: Findings {
            program_check: vec![
                held(601, Some(4), Some(JobKind::AgentCall), 30_000),
                held(602, Some(4), Some(JobKind::AgentCall), 1_140_000),
                held(603, Some(4), Some(JobKind::Shell), 9_000_000),
                held(604, Some(5), Some(JobKind::AgentCall), 5_000),
                held(605, None, None, 9_999_000),
                held(606, Some(4), Some(JobKind::Nested), 9_000_000),
            ],
            ..Findings::default()
        },
        ..JobsView::default()
    });
    let sids = BTreeMap::from([
        (4, "s-a".to_string()),
        (5, "s-b".to_string()),
        (6, "s-c".to_string()),
    ]);
    let board = JobsBoard::new();
    let posted = Instant::now();
    board.post(Arc::clone(&view), sids.clone(), posted);
    let later = posted + Duration::from_secs(2);
    let pids = |sid: &str| {
        board.held(sid, later).map(|(age, rows)| {
            (
                age,
                rows.iter()
                    .map(|h| (h.pid, h.exec_version, h.held_ms))
                    .collect(),
            )
        })
    };
    assert_eq!(
        pids("s-a"),
        Some((
            Duration::from_secs(2),
            vec![(602, Some(602), 1_140_000), (601, Some(601), 30_000)]
        ))
    );
    assert_eq!(
        pids("s-b"),
        Some((Duration::from_secs(2), vec![(604, Some(604), 5_000)]))
    );
    assert_eq!(pids("s-c"), Some((Duration::from_secs(2), Vec::new())));
    assert_eq!(
        board.held("s-a", posted + JOBS_FRESH + Duration::from_secs(1)),
        None,
        "a view past the bound"
    );
    assert_eq!(JobsBoard::new().held("s-a", later), None, "no view");
}

/// The board, posted `posted`, over the engine's view of held programs outside every job
/// that W-session ties to tab 4 (session `s-a`), one born after its agent started (702) and
/// one before (704), and to tab 5 (`s-b`, 703, after its agent), beside one call's member of
/// tab 4's (701).
fn tied_board(posted: Instant) -> JobsBoard {
    use aterm_vitals::jobs::{HeldProc, JobTick, Jobs, StartWait};
    use aterm_vitals::left::Tie;
    let held = |pid| HeldProc {
        pid,
        exec: u64::from(pid),
        exec_version: Some(u64::from(pid)),
        name: "sh".into(),
        aterm: true,
        wait: StartWait::ProgramCheck,
        ..HeldProc::default()
    };
    let tie = |key, after_agent| Tie {
        key,
        closed: false,
        after_agent,
    };
    let mut rows = vec![held(702), held(703), held(704)];
    name_tied(
        &mut rows,
        &BTreeMap::from([
            (702, tie(4, true)),
            (703, tie(5, true)),
            (704, tie(4, false)),
        ]),
    );
    let mut engine = Jobs::new();
    let tick = |at_ms| JobTick {
        at_ms,
        read_ms: at_ms,
        self_uid: Some(501),
        held: rows.clone(),
        ..JobTick::default()
    };
    engine.absorb(&tick(1_000));
    let mut view = engine.absorb(&tick(601_000));
    view.findings.program_check.push(aterm_vitals::jobs::Named {
        pid: 701,
        exec_version: Some(701),
        name: "sh".into(),
        tab: Some(4),
        job: Some(JobKind::AgentCall),
        for_ms: 30_000,
    });
    let view = Arc::new(view);
    let board = JobsBoard::new();
    board.post(
        view,
        BTreeMap::from([(4, "s-a".to_string()), (5, "s-b".to_string())]),
        posted,
    );
    board
}

/// A SESSION-MATE TIED HELD ROW COUNTS ONCE ITS AGENT HAD STARTED: a held program in no job
/// (born and orphaned between two readings) that a session-mate's environment ties to this
/// tab, born after the tab's agent started, is this tab's agent's to be told of, beside its
/// calls' members; one born before the agent started is the person's, never told.
/// NEGATIVE CONTROL: name every tied program an agent's call, and the person's is told too.
#[test]
fn a_session_mate_tied_held_row_counts() {
    let posted = Instant::now();
    let board = tied_board(posted);
    let (_, rows) = board
        .held("s-a", posted + Duration::from_secs(1))
        .expect("a fresh view");
    let pids: Vec<u32> = rows.iter().map(|h| h.pid).collect();
    assert_eq!(
        pids,
        vec![702, 701],
        "held longest first, the person's never"
    );
}

/// ANOTHER SESSION'S HELD ROW DOES NOT COUNT: a program W-session ties to tab 5 is session
/// `s-b`'s, never `s-a`'s, and a session no held row is tied to is told of none.
#[test]
fn another_sessions_held_row_does_not() {
    let posted = Instant::now();
    let board = tied_board(posted);
    let later = posted + Duration::from_secs(1);
    let pids = |sid: &str| -> Vec<u32> {
        board
            .held(sid, later)
            .expect("a fresh view")
            .1
            .iter()
            .map(|h| h.pid)
            .collect()
    };
    assert_eq!(pids("s-b"), vec![703]);
    assert!(!pids("s-a").contains(&703));
    assert_eq!(pids("s-c"), Vec::<u32>::new());
}

/// A HELD PID IS PROVED ONLY IN A PROCESS AS OLD AS ITS TOLD WAIT ([`held_as_long`]): this
/// process has run a moment, and a pid no process holds has run none. NEGATIVE CONTROL: a wait
/// longer than this process has lived is a later process's, one that took the pid since; and
/// this process, past its start, is never held.
#[test]
fn a_held_pid_is_proved_only_in_a_process_as_old_as_its_wait() {
    let me = std::process::id();
    assert!(has_run(me, 0), "this process has run");
    assert!(
        !has_run(me, 10 * 365 * 24 * 3_600_000),
        "a wait longer than this process has lived"
    );
    assert!(!has_run(u32::MAX - 1, 0), "a pid no process holds");
    assert!(
        !held_as_long(&HeldCall {
            pid: me,
            exec_version: (HeldReaders::KERNEL.exec)(me),
            held_ms: 0,
        }),
        "this process is past its start"
    );
}

/// A fork's age survives exec, but its old held duration does not. The unchanged exec is
/// proved with the same pid and sufficient age; a later exec, an exec during the probe and
/// an unavailable version are unproved even while the program is held and its fork is old.
#[test]
fn a_held_wait_is_reproved_only_for_its_original_exec() {
    let held = HeldCall {
        pid: 601,
        exec_version: Some(17),
        held_ms: 1_140_000,
    };
    let prove = |held: &HeldCall, versions: [Option<u64>; 2]| {
        let mut versions = versions.into_iter();
        held_as_long_with(
            held,
            |pid| {
                assert_eq!(pid, 601, "the same pid across exec");
                versions.next().flatten()
            },
            |_| true,
            |_, ms| 10_000_000 >= ms,
        )
    };
    assert!(prove(&held, [Some(17), Some(17)]), "the unchanged exec");
    assert!(
        !prove(&held, [Some(18), Some(18)]),
        "a newer exec in the same old process cannot inherit the wait"
    );
    assert!(!prove(&held, [Some(17), Some(18)]), "exec during proof");
    assert!(
        !prove(&held, [Some(17), None]),
        "the final identity was unread"
    );
    assert!(
        !prove(&held, [None, None]),
        "the current identity was unread"
    );
    assert!(
        !prove(
            &HeldCall {
                exec_version: None,
                ..held
            },
            [Some(17), Some(17)]
        ),
        "a birth fallback is no actual exec version"
    );
}

/// This process lives at its own start, a start one unit off is another process, and a pid no
/// process holds lives at no start (the negative control).
#[test]
fn a_leader_lives_only_at_its_own_birth() {
    let me = std::process::id();
    let birth = JobsReader::new()
        .read(ReadReq {
            pid: me,
            fds: false,
        })
        .and_then(|f| f.start)
        .expect("this process's start");
    assert!(leader_lives(me, birth));
    assert!(!leader_lives(me, birth + 1));
    assert!(!leader_lives(u32::MAX - 1, birth));
}
